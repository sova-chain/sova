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
