//! The sealer decision core: who may produce an epoch's block, when, and
//! which candidate block wins.
//!
//! Pure logic, no I/O, no clocks — callers supply elapsed time. The two
//! consensus rules this module owns:
//!
//! 1. **Production eligibility is a timeout ladder, liveness-only.**
//!    Rank r (0-based) may *produce* once `elapsed >= r * step`. Rank 0
//!    (the epoch's top burner) produces immediately; each fallback rank
//!    waits one more step. Clocks affect only when a node bothers to
//!    build — never which block is valid or preferred.
//! 2. **Preference is rank, then hash — never arrival time.** Among
//!    candidate blocks for the same epoch, the lowest sealer rank wins;
//!    a late rank-0 block still beats an on-time rank-1 block (a
//!    micro-reorg bounded to the epoch). Equal rank (an equivocating
//!    sealer) breaks the tie by lowest block hash, so every node picks
//!    the same winner without coordination.
//!
//! Empty epochs (no burns) have no ranked sealers; the design lets recent
//! sealers extend the chain with a rewardless block using the same ladder
//! — [`extension_rank`] maps a member of the recent-sealer set to its
//! ladder position.

use std::cmp::Ordering;
use std::time::Duration;

use crate::epoch::MinerWeight;

/// Default per-rank production timeout step. Draft v0 constant; final
/// value is a SIP parameter (see the workplan's epoch-granularity item).
pub const DEFAULT_RANK_STEP: Duration = Duration::from_secs(15);

/// A candidate block for one epoch, as fork-choice sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// The sealing miner's rank in the epoch (0 = top burner).
    pub sealer_rank: usize,
    /// The candidate Sova block's hash.
    pub block_hash: [u8; 32],
}

/// Total preference order over an epoch's candidates: lower rank first,
/// then lower hash. [`Ordering::Less`] means "preferred over".
#[must_use]
pub fn prefer(a: &Candidate, b: &Candidate) -> Ordering {
    a.sealer_rank
        .cmp(&b.sealer_rank)
        .then_with(|| a.block_hash.cmp(&b.block_hash))
}

/// Trust rank of a block with no ranked sealer: a null block, or a
/// burn-less or rewardless epoch's block.
pub const NULL_RANK: usize = usize::MAX;

/// Trust rank of a sealer demoted for equivocation (SIP-6 §2.7).
pub const EQUIVOCATOR_RANK: usize = usize::MAX - 1;

/// Audit F2 measure C (`docs/design/f2-join-and-restart.md` §C): one
/// block's score for comparing branches. `min(rank, 63)` for a ranked
/// sealer, 64 for a demoted equivocator, 128 for a block with no ranked
/// sealer. Bounded, so sums cannot overflow and null blocks stay last.
#[must_use]
pub const fn rank_score(rank: usize) -> u64 {
    match rank {
        NULL_RANK => 128,
        EQUIVOCATOR_RANK => 64,
        r if r < 63 => r as u64,
        _ => 63,
    }
}

/// Audit F2 measure C: compare two branches from their common ancestor.
///
/// `a` and `b` are the sealer ranks of each branch's blocks from the first
/// height after the fork point, in height order. Over the heights both
/// have (`m = min(len)`), the lower summed [`rank_score`] wins; on a tie the
/// longer branch wins; [`Ordering::Equal`] leaves the decision to the
/// caller (the incumbent, or for a node with none, the lower hash of the
/// first block after the fork). [`Ordering::Less`] means `a` is preferred.
///
/// Not lexicographic: the first block does not decide everything. At one
/// height this is SIP-2's rank order, so the tip behaves as it always has.
#[must_use]
pub fn prefer_branch(a: &[usize], b: &[usize]) -> Ordering {
    let m = a.len().min(b.len());
    let sum = |ranks: &[usize]| ranks[..m].iter().map(|&r| rank_score(r)).sum::<u64>();
    sum(a).cmp(&sum(b)).then_with(|| b.len().cmp(&a.len()))
}

/// The best of two optional candidates under [`prefer`].
#[must_use]
pub fn better(a: Option<Candidate>, b: Option<Candidate>) -> Option<Candidate> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if prefer(&x, &y) == Ordering::Greater {
            y
        } else {
            x
        }),
        (x, None) => x,
        (None, y) => y,
    }
}

/// What a node should do about producing this epoch's block right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProduceDecision {
    /// Build and gossip a block now.
    Produce,
    /// Eligible later: check again once `elapsed` reaches this value.
    Wait(Duration),
    /// Not eligible this epoch (not ranked, or a better candidate seals).
    NotEligible,
}

/// Our rank in the epoch's ranked-miner list, if any.
#[must_use]
pub fn own_rank(ranked: &[MinerWeight], our_address: [u8; 20]) -> Option<usize> {
    ranked.iter().position(|m| m.evm_address == our_address)
}

/// Decide whether to produce, given our rank, the elapsed time since the
/// epoch opened, and the best candidate block already seen (if any).
///
/// A seen candidate at rank <= ours ends our claim ([`ProduceDecision::NotEligible`]):
/// producing would lose fork choice by rule 2. A worse-ranked candidate
/// does not stop us — our block will be preferred once produced.
#[must_use]
pub fn produce_decision(
    our_rank: Option<usize>,
    elapsed: Duration,
    best_seen: Option<&Candidate>,
    step: Duration,
) -> ProduceDecision {
    let Some(rank) = our_rank else {
        return ProduceDecision::NotEligible;
    };
    if let Some(seen) = best_seen
        && seen.sealer_rank <= rank
    {
        return ProduceDecision::NotEligible;
    }
    let due = step.saturating_mul(rank as u32);
    if elapsed >= due {
        ProduceDecision::Produce
    } else {
        ProduceDecision::Wait(due)
    }
}

/// Ladder position for extending an *empty* epoch (no burns): members of
/// the recent-sealer set, ordered most-recent-first, reuse the same
/// timeout ladder to produce a rewardless extension block.
#[must_use]
pub fn extension_rank(recent_sealers: &[[u8; 20]], our_address: [u8; 20]) -> Option<usize> {
    recent_sealers.iter().position(|a| *a == our_address)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> [u8; 20] {
        [b; 20]
    }

    fn miner(b: u8, weight: u64) -> MinerWeight {
        MinerWeight {
            evm_address: addr(b),
            weight_zat: weight,
            min_txid: [b; 32],
        }
    }

    fn cand(rank: usize, hash_byte: u8) -> Candidate {
        Candidate {
            sealer_rank: rank,
            block_hash: [hash_byte; 32],
        }
    }

    const STEP: Duration = Duration::from_secs(10);

    #[test]
    fn rank_zero_produces_immediately() {
        let ranked = [miner(1, 100), miner(2, 50)];
        let r = own_rank(&ranked, addr(1));
        assert_eq!(
            produce_decision(r, Duration::ZERO, None, STEP),
            ProduceDecision::Produce
        );
    }

    #[test]
    fn fallback_waits_its_ladder_step_then_produces() {
        let ranked = [miner(1, 100), miner(2, 50), miner(3, 10)];
        let r = own_rank(&ranked, addr(3)); // rank 2
        assert_eq!(
            produce_decision(r, Duration::from_secs(19), None, STEP),
            ProduceDecision::Wait(Duration::from_secs(20))
        );
        assert_eq!(
            produce_decision(r, Duration::from_secs(20), None, STEP),
            ProduceDecision::Produce
        );
    }

    #[test]
    fn better_seen_candidate_ends_the_claim() {
        let ranked = [miner(1, 100), miner(2, 50)];
        let r = own_rank(&ranked, addr(2)); // rank 1
        let seen = cand(0, 0xAA);
        assert_eq!(
            produce_decision(r, Duration::from_secs(60), Some(&seen), STEP),
            ProduceDecision::NotEligible
        );
        // Equal rank seen (our own equivocation guard) also ends it.
        let seen_equal = cand(1, 0xBB);
        assert_eq!(
            produce_decision(r, Duration::from_secs(60), Some(&seen_equal), STEP),
            ProduceDecision::NotEligible
        );
    }

    #[test]
    fn worse_seen_candidate_does_not_stop_us() {
        let ranked = [miner(1, 100), miner(2, 50)];
        let r = own_rank(&ranked, addr(1)); // rank 0
        let seen = cand(1, 0xAA);
        assert_eq!(
            produce_decision(r, Duration::from_secs(60), Some(&seen), STEP),
            ProduceDecision::Produce
        );
    }

    #[test]
    fn unranked_is_never_eligible() {
        let ranked = [miner(1, 100)];
        assert_eq!(
            produce_decision(
                own_rank(&ranked, addr(9)),
                Duration::from_secs(600),
                None,
                STEP
            ),
            ProduceDecision::NotEligible
        );
        assert_eq!(
            produce_decision(None, Duration::ZERO, None, STEP),
            ProduceDecision::NotEligible
        );
    }

    #[test]
    fn rank_score_is_bounded_and_orders_null_last() {
        assert_eq!(rank_score(0), 0);
        assert_eq!(rank_score(5), 5);
        assert_eq!(rank_score(62), 62);
        assert_eq!(rank_score(63), 63);
        assert_eq!(rank_score(10_000), 63);
        assert_eq!(rank_score(EQUIVOCATOR_RANK), 64);
        assert_eq!(rank_score(NULL_RANK), 128);
    }

    #[test]
    fn prefer_branch_sums_over_the_common_range() {
        use Ordering::{Equal, Greater, Less};
        // One height: SIP-2's rank order.
        assert_eq!(prefer_branch(&[0], &[1]), Less);
        assert_eq!(prefer_branch(&[2], &[1]), Greater);
        // Not lexicographic: a worse first block is outweighed later.
        assert_eq!(prefer_branch(&[1, 0, 0], &[0, 2, 2]), Less);
        // Deeper better-ranked branch beats a mostly-null one.
        assert_eq!(
            prefer_branch(&[0, 0, 0, 0], &[0, NULL_RANK, NULL_RANK, 0]),
            Less
        );
        // Only the common range is summed; then the longer one wins.
        assert_eq!(prefer_branch(&[0, 0], &[0, 0, 5]), Greater);
        assert_eq!(prefer_branch(&[0, NULL_RANK], &[0]), Less);
        // Full tie: the caller decides (incumbent, or hash).
        assert_eq!(prefer_branch(&[0, 1], &[1, 0]), Equal);
        assert_eq!(prefer_branch(&[], &[]), Equal);
        // Equivocators sit between every honest rank and a null block.
        assert_eq!(prefer_branch(&[EQUIVOCATOR_RANK], &[63]), Greater);
        assert_eq!(prefer_branch(&[EQUIVOCATOR_RANK], &[NULL_RANK]), Less);
        // Sums cannot overflow on long all-null branches.
        let long = vec![NULL_RANK; 100_000];
        assert_eq!(prefer_branch(&long, &long), Equal);
    }

    #[test]
    fn preference_is_rank_then_hash_never_time() {
        // Late rank-0 beats on-time rank-1.
        assert_eq!(prefer(&cand(0, 0xFF), &cand(1, 0x00)), Ordering::Less);
        // Equivocation tie-break: lowest hash wins.
        assert_eq!(prefer(&cand(1, 0x01), &cand(1, 0x02)), Ordering::Less);
        // Identical candidates are equal.
        assert_eq!(prefer(&cand(1, 0x01), &cand(1, 0x01)), Ordering::Equal);
    }

    #[test]
    fn better_composes_options_correctly() {
        assert_eq!(better(None, None), None);
        assert_eq!(better(Some(cand(2, 1)), None), Some(cand(2, 1)));
        assert_eq!(better(Some(cand(2, 1)), Some(cand(0, 9))), Some(cand(0, 9)));
        // Deterministic under argument order.
        assert_eq!(better(Some(cand(0, 9)), Some(cand(2, 1))), Some(cand(0, 9)));
    }

    #[test]
    fn extension_ladder_uses_recent_sealer_order() {
        let recent = [addr(5), addr(6)];
        assert_eq!(extension_rank(&recent, addr(5)), Some(0));
        assert_eq!(extension_rank(&recent, addr(6)), Some(1));
        assert_eq!(extension_rank(&recent, addr(7)), None);
    }
}
