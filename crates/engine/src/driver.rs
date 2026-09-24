//! Bridges the consensus crate's pure epoch data into engine payload
//! attributes: `EpochData` → ranked miners → reward shares →
//! [`SovaEpochAttribute`] settlements.
//!
//! Reward denominations, once, precisely: the schedule and all share math
//! run in **gwei** (`epoch_rewards` is unit-agnostic u128; we feed it
//! gwei), so every share is gwei-exact by construction; settlements are
//! then expressed in wei (share × 10⁹) for the EVM-natural wire format,
//! and [`crate::settlements_to_withdrawals`] converts back to gwei
//! withdrawal amounts losslessly. Conservation holds end to end: the sum
//! of minted wei equals the epoch reward exactly.
//!
//! The async loop that feeds this from a live follower and drives the
//! Engine API is the next C3 slice; this module keeps the consensus-
//! critical construction pure and testable.

use alloy_primitives::{Address, U256};
use consensus::epoch::{EpochBurn, MinerWeight, epoch_rewards, rank_miners};
use consensus::follower::{EpochData, Follower, FollowerEvent, ViewError, ZcashView};

use crate::PendingEpoch;

use crate::SovaEpochAttribute;

/// The flat per-epoch reward regtest and the box run on: SIP-3's base
/// reward (6,250 SOVA — the numbers are locked, SIP-3 is Accepted).
/// The name keeps its historical `DRAFT_` prefix only to avoid churning
/// every call site; the real schedule (slow start + halving eras) lives
/// in [`consensus::schedule`] and is selected per network (C8).
pub const DRAFT_EPOCH_REWARD_GWEI: u128 = consensus::schedule::BASE_EPOCH_REWARD_GWEI;

/// One gwei in wei, for settlement wire amounts.
const GWEI_IN_WEI: u128 = 1_000_000_000;

/// Why an epoch produced no settlement attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EpochAttributeError {
    /// The epoch has no burns: nothing to mint, no ranked sealer.
    #[error("epoch has no burns")]
    EmptyEpoch,
    /// The sealer is not one of the epoch's ranked miners, or reward
    /// parameters violated `epoch_rewards` invariants.
    #[error("reward computation rejected the inputs")]
    RewardsInvalid,
}

/// Rank an epoch's burns. Thin re-export point so driver callers don't
/// need the consensus crate in scope for the common path.
#[must_use]
pub fn ranked_miners(burns: &[EpochBurn]) -> Vec<MinerWeight> {
    rank_miners(burns)
}

/// Build the [`SovaEpochAttribute`] for a sealed epoch: the claimed Zcash
/// anchor plus the settlement list `epoch_rewards` mandates, in rank
/// order, denominated in wei (gwei-aligned by construction).
pub fn epoch_attribute(
    epoch: &EpochData,
    ranked: &[MinerWeight],
    sealer: [u8; 20],
    reward_gwei: u128,
) -> Result<SovaEpochAttribute, EpochAttributeError> {
    if epoch.burns.is_empty() || ranked.is_empty() {
        return Err(EpochAttributeError::EmptyEpoch);
    }
    let shares =
        epoch_rewards(reward_gwei, ranked, sealer).ok_or(EpochAttributeError::RewardsInvalid)?;
    let settlements = shares
        .into_iter()
        .map(|s| {
            (
                Address::from(s.evm_address),
                U256::from(s.amount_wei) * U256::from(GWEI_IN_WEI),
            )
        })
        .collect();
    Ok(SovaEpochAttribute {
        zcash_height: epoch.height,
        zcash_hash: epoch.hash,
        settlements,
        zcash_time: u64::from(epoch.time),
        null: false,
    })
}

/// Identify which ranked miner sealed a block from its withdrawals
/// alone (v2 preference, docs/design/gossip-v1.md addendum): the sealer
/// tip makes each candidate sealer's derivation distinct, so the unique
/// rank whose derivation matches identifies the sealer. Returns the
/// rank (0-based), or `None` if nothing matches (foreign/invalid block)
/// or the epoch has no miners (rewardless candidates carry no
/// settlements and tie-break by hash alone).
#[must_use]
pub fn identify_sealer(
    epoch: &EpochData,
    ranked: &[MinerWeight],
    reward_gwei: u128,
    withdrawals: &[alloy_rpc_types::Withdrawal],
) -> Option<usize> {
    ranked.iter().enumerate().find_map(|(rank, miner)| {
        let attr = epoch_attribute(epoch, ranked, miner.evm_address, reward_gwei).ok()?;
        let derived = crate::settlements_to_withdrawals(&attr).ok()?;
        (derived.as_slice() == withdrawals).then_some(rank)
    })
}

/// Sealer configuration for a single mining node.
#[derive(Debug, Clone)]
pub struct SealerConfig {
    /// Our miner's EVM address (the one our burns credit).
    pub our_address: [u8; 20],
    /// The network's emission schedule (SIP-3). Must match what the
    /// node's validator enforces ([`crate::expectations`]) or the node
    /// rejects its own blocks.
    pub schedule: consensus::schedule::Schedule,
    /// Ladder step: rank `r` may seal once `r * rank_step` has elapsed
    /// since we saw the epoch ([`consensus::sealer::produce_decision`]).
    pub rank_step: std::time::Duration,
    /// SIP-6 is active: burn-less and abandoned epochs get their null
    /// block instead of an unsigned cadence or rank-0 block.
    pub sip6: bool,
}

/// What one follower poll produced, for logging and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealerOutcome {
    /// A block trigger should fire for this epoch; if it carried our
    /// settlements, they were staged in the [`PendingEpoch`] mailbox.
    Trigger {
        /// The epoch's Zcash height.
        height: u64,
        /// The Sova height to build (the miner builds on `sova_height − 1`).
        sova_height: u64,
        /// A late win: build a sibling of the covered tip.
        late_win: bool,
        /// A re-fire of an earlier trigger whose build hasn't landed.
        retry: bool,
        /// Whether a settlement attribute was staged for this trigger.
        settled: bool,
        /// SIP-6: build the epoch's null block (unsigned, empty).
        null: bool,
    },
    /// The follower reported a reorg; v0 logs and continues (multi-node
    /// rollback handling arrives with gossip).
    Rollback {
        /// Deepest still-canonical Zcash height.
        to_height: u64,
    },
}

/// The sealer's per-poll core: follower events in, block triggers and
/// staged settlements out. Sync and view-generic so tests drive it with a
/// mock chain (including time, passed in as `now`); [`run_sealer`] wraps
/// it in the async loop.
///
/// v2 ladder rules:
/// - Production is **in-order**: epoch E's block sits at Sova height
///   `E − base + 1`, so nothing later may build while an earlier epoch
///   is unsealed — a cadence block jumping the queue would occupy the
///   waiting epoch's height with the wrong (empty) settlements and be
///   rejected by our own validator. The queue therefore only ever
///   produces at the front (`expected == sova_head + 1`).
/// - Burn-less epochs at the front trigger a rewardless cadence block
///   immediately (validates as `ValidEmpty` on import).
/// - Burn-bearing epochs at the front run the ladder: rank `r` seals
///   once `r * rank_step` has elapsed since the epoch reached the front
///   (its first sealable moment), unless a better-or-equal candidate
///   has already been seen for the height (`best_seen`, fed from
///   [`crate::candidates`]). A node not in the epoch's ranking seals it
///   only if the epoch is *abandoned*: every rank's rung has passed plus
///   [`ABANDON_GRACE_RUNGS`] more, and no candidate has been seen. It then
///   seals rank 0's exact derivation, which is valid under C5 from any
///   node and pays every burner (rank 0's tip included) exactly what they
///   are owed. Without this, one burn crediting an address nobody seals
///   for halts the chain, since production is in order.
/// - Timeouts are liveness-only: a late better-ranked block still wins
///   at fork choice (the arbiter's micro-reorg), per SIP-2. The epoch at
///   the tip height therefore stays queued: if the block covering it is
///   strictly worse than our rank, we seal a sibling on its parent (a
///   *late win*) and hold production of the next epoch until that
///   resolves — building on the losing block would make our own late
///   block stale to every arbiter. Below the tip nothing is reconsidered:
///   the arbiter bounds micro-reorgs to the tip epoch.
/// - A trigger whose build didn't land (head never reached its height)
///   re-fires after [`RETRIGGER`]; epochs are never dropped on trigger.
#[derive(Debug)]
pub struct SealerCore {
    follower: Follower,
    config: SealerConfig,
    base_height: u64,
    queue: std::collections::BTreeMap<u64, QueueEntry>,
}

/// An epoch awaiting in-order production.
#[derive(Debug)]
struct QueueEntry {
    epoch: EpochData,
    ranked: Vec<MinerWeight>,
    /// Ladder clock: set when the epoch reaches the queue front.
    front_since: Option<std::time::Instant>,
    /// When we last asked the miner to build this epoch's height.
    triggered_at: Option<std::time::Instant>,
}

/// Extra rungs, beyond the last rank's, before a non-ranked node seals an
/// abandoned burn epoch with rank 0's derivation (liveness fallback; see
/// [`SealerCore`]). SIP-6 replaces this with a keyless null block.
pub const ABANDON_GRACE_RUNGS: u32 = 2;

/// Queued epochs retained at most (Zcash reorg window scale).
const QUEUE_RETAIN: usize = 256;

/// How long a trigger may go unanswered (its height still unbuilt, or our
/// late-win sibling still not observed) before it is re-fired.
pub const RETRIGGER: std::time::Duration = std::time::Duration::from_secs(15);

impl SealerCore {
    /// A sealer following Zcash from `base_height` with the given window.
    #[must_use]
    pub fn new(config: SealerConfig, base_height: u64, window: usize) -> Self {
        Self {
            follower: Follower::new(base_height, window),
            config,
            base_height,
            queue: std::collections::BTreeMap::new(),
        }
    }

    /// Expected Sova chain height once epoch `zcash_height` is sealed
    /// (one Sova block per epoch from the base).
    #[must_use]
    pub fn expected_sova_height(&self, zcash_height: u64) -> u64 {
        zcash_height
            .saturating_sub(self.base_height)
            .saturating_add(1)
    }

    /// Poll the view once; stage settlements into `pending` and report
    /// the triggers the caller should fire.
    ///
    /// `sova_head` is the node's current chain height; epochs whose
    /// expected Sova height is already covered are skipped (a block —
    /// ours or a peer's — already landed): the no-double-production rule
    /// from docs/design/gossip-v1.md. `now` drives the ladder clock and
    /// `best_seen` answers "what's the best candidate already observed
    /// for this expected Sova height?" (fed from [`crate::candidates`]
    /// in production, a stub in tests).
    pub fn process<V: ZcashView>(
        &mut self,
        view: &V,
        sova_head: u64,
        pending: &PendingEpoch,
        now: std::time::Instant,
        best_seen: impl Fn(u64) -> Option<consensus::sealer::Candidate>,
    ) -> Result<Vec<SealerOutcome>, ViewError> {
        let events = self.follower.poll(view)?;
        let mut out = Vec::new();
        for event in events {
            match event {
                FollowerEvent::Rollback { to_height } => {
                    self.queue.retain(|&h, _| h <= to_height);
                    out.push(SealerOutcome::Rollback { to_height });
                }
                FollowerEvent::Epoch(mut epoch) => {
                    // The sealer needs burns, not the full tx list.
                    epoch.txs = Vec::new();
                    let ranked = rank_miners(&epoch.burns);
                    self.queue.insert(
                        epoch.height,
                        QueueEntry {
                            epoch,
                            ranked,
                            front_since: None,
                            triggered_at: None,
                        },
                    );
                    while self.queue.len() > QUEUE_RETAIN {
                        self.queue.pop_first();
                    }
                }
            }
        }

        // Epochs below the tip are settled history for the sealer (the
        // arbiter never reorgs below the tip); the tip epoch stays for a
        // possible late win.
        let base = self.base_height;
        self.queue
            .retain(|&h, _| h.saturating_sub(base).saturating_add(1) >= sova_head);

        let mut produced_head = sova_head;
        let mut keys: Vec<u64> = self.queue.keys().copied().collect();
        keys.sort_unstable();
        for zcash_height in keys {
            let expected = zcash_height.saturating_sub(base).saturating_add(1);
            let reward_gwei = self.config.schedule.reward_gwei(expected.saturating_sub(1));
            let Some(entry) = self.queue.get_mut(&zcash_height) else {
                continue;
            };
            let our_rank = entry
                .ranked
                .iter()
                .position(|m| m.evm_address == self.config.our_address);
            let settles = !entry.ranked.is_empty() && reward_gwei > 0;
            let retrigger_due = entry
                .triggered_at
                .is_none_or(|at| now.duration_since(at) >= RETRIGGER);
            let retry = entry.triggered_at.is_some();

            if expected <= sova_head {
                // Covered tip. Late win only when a block we can beat
                // holds the height: a known candidate strictly worse than
                // our rank. (None — e.g. after a restart — means covered.)
                let beatable = match (settles, our_rank, best_seen(expected)) {
                    (true, Some(rank), Some(best)) => best.sealer_rank > rank,
                    _ => false,
                };
                if !beatable {
                    continue;
                }
                let front_since = *entry.front_since.get_or_insert(now);
                match consensus::sealer::produce_decision(
                    our_rank,
                    now.duration_since(front_since),
                    best_seen(expected).as_ref(),
                    self.config.rank_step,
                ) {
                    consensus::sealer::ProduceDecision::Produce if retrigger_due => {
                        if let Ok(attr) = epoch_attribute(
                            &entry.epoch,
                            &entry.ranked,
                            self.config.our_address,
                            reward_gwei,
                        ) {
                            pending.stage(expected, attr);
                            entry.triggered_at = Some(now);
                            out.push(SealerOutcome::Trigger {
                                height: zcash_height,
                                sova_height: expected,
                                late_win: true,
                                retry,
                                settled: true,
                                null: false,
                            });
                        }
                    }
                    _ => {}
                }
                // Hold the next epoch while a late win is live.
                break;
            }

            if expected != produced_head.saturating_add(1) {
                // A gap: the missing height's epoch hasn't been observed
                // (or was pruned); nothing in-order to produce.
                break;
            }
            if !retrigger_due {
                // Asked recently; wait for the build to land.
                break;
            }
            // A zero scheduled reward (post-emission era) settles like a
            // burn-less epoch: rewardless cadence block, empty
            // withdrawals — there is nothing to derive or rank over.
            if !settles {
                // Nothing to mint, but the block still commits to its
                // epoch's Zcash hash (SIP-4 anchor).
                // SIP-6: that block is the epoch's null block, the same
                // on every node that builds it.
                pending.stage(
                    expected,
                    SovaEpochAttribute {
                        zcash_height,
                        zcash_hash: entry.epoch.hash,
                        settlements: Vec::new(),
                        zcash_time: u64::from(entry.epoch.time),
                        null: self.config.sip6,
                    },
                );
                entry.triggered_at = Some(now);
                out.push(SealerOutcome::Trigger {
                    height: zcash_height,
                    sova_height: expected,
                    late_win: false,
                    retry,
                    settled: false,
                    null: self.config.sip6,
                });
                produced_head = expected;
                continue;
            }
            if our_rank.is_none() {
                // Not ours to seal at any rank; the block arrives by
                // relay from a ranked miner — unless the epoch is
                // abandoned (see the SealerCore doc). Hold the queue.
                let front_since = *entry.front_since.get_or_insert(now);
                let rungs = u32::try_from(entry.ranked.len())
                    .unwrap_or(u32::MAX)
                    .saturating_add(ABANDON_GRACE_RUNGS);
                let abandoned = best_seen(expected).is_none()
                    && now.duration_since(front_since)
                        >= self.config.rank_step.saturating_mul(rungs);
                if abandoned && self.config.sip6 {
                    // SIP-6: nobody ranked sealed, and only they can sign;
                    // the null block (lowest preference, mints nothing)
                    // keeps the chain moving. Any signed block displaces it.
                    tracing::warn!(
                        zcash_height,
                        sova_height = expected,
                        "abandoned burn epoch: building its null block for liveness"
                    );
                    pending.stage(
                        expected,
                        SovaEpochAttribute {
                            zcash_height,
                            zcash_hash: entry.epoch.hash,
                            settlements: Vec::new(),
                            zcash_time: u64::from(entry.epoch.time),
                            null: true,
                        },
                    );
                    entry.triggered_at = Some(now);
                    out.push(SealerOutcome::Trigger {
                        height: zcash_height,
                        sova_height: expected,
                        late_win: false,
                        retry,
                        settled: false,
                        null: true,
                    });
                } else if abandoned
                    && let Some(rank0) = entry.ranked.first()
                    && let Ok(attr) =
                        epoch_attribute(&entry.epoch, &entry.ranked, rank0.evm_address, reward_gwei)
                {
                    tracing::warn!(
                        zcash_height,
                        sova_height = expected,
                        "abandoned burn epoch: sealing rank 0's derivation for liveness"
                    );
                    pending.stage(expected, attr);
                    entry.triggered_at = Some(now);
                    out.push(SealerOutcome::Trigger {
                        height: zcash_height,
                        sova_height: expected,
                        late_win: false,
                        retry,
                        settled: true,
                        null: false,
                    });
                }
                break;
            }
            // Ladder clock starts when the epoch becomes sealable.
            let front_since = *entry.front_since.get_or_insert(now);
            match consensus::sealer::produce_decision(
                our_rank,
                now.duration_since(front_since),
                best_seen(expected).as_ref(),
                self.config.rank_step,
            ) {
                consensus::sealer::ProduceDecision::Produce => {
                    if let Ok(attr) = epoch_attribute(
                        &entry.epoch,
                        &entry.ranked,
                        self.config.our_address,
                        reward_gwei,
                    ) {
                        pending.stage(expected, attr);
                        entry.triggered_at = Some(now);
                        out.push(SealerOutcome::Trigger {
                            height: zcash_height,
                            sova_height: expected,
                            late_win: false,
                            retry,
                            settled: true,
                            null: false,
                        });
                    }
                    // One settled epoch per pass (one-slot mailbox).
                    break;
                }
                // Wait: our rung isn't due. NotEligible: a better-or-
                // equal candidate seals; hold until its block covers the
                // height (a Zcash rollback also clears the entry).
                consensus::sealer::ProduceDecision::Wait(_)
                | consensus::sealer::ProduceDecision::NotEligible => break,
            }
        }
        Ok(out)
    }
}

/// The async sealer loop: poll the Zcash view, stage settlements, and
/// fire one `()` per epoch into `trigger` — wired to reth's
/// `MiningMode::trigger` so each Zcash block yields one Sova block.
pub async fn run_sealer<V: ZcashView>(
    mut core: SealerCore,
    view: V,
    sova_head: impl Fn() -> u64,
    pending: PendingEpoch,
    trigger: tokio::sync::mpsc::Sender<crate::miner::BuildTarget>,
    poll_interval: std::time::Duration,
) {
    loop {
        match core.process(
            &view,
            sova_head(),
            &pending,
            std::time::Instant::now(),
            |h| crate::candidates::global().best(h),
        ) {
            Ok(outcomes) => {
                for outcome in outcomes {
                    match outcome {
                        SealerOutcome::Trigger {
                            height,
                            sova_height,
                            late_win,
                            retry,
                            settled,
                            null,
                        } => {
                            // Retries log apart from first triggers: one
                            // `settled=true` line per settled epoch.
                            if retry {
                                tracing::info!(
                                    height,
                                    sova_height,
                                    late_win,
                                    "sova epoch retrigger"
                                );
                            } else {
                                tracing::info!(
                                    height,
                                    sova_height,
                                    late_win,
                                    settled,
                                    "sova epoch trigger"
                                );
                            }
                            // Our own consensus holds a block whose epoch the
                            // expectations follower hasn't scanned yet (SIP-4
                            // "hold, don't accept"). That follower polls
                            // separately from ours, so give it a moment rather
                            // than build a block we'd hold and re-build after
                            // RETRIGGER.
                            wait_for_scan(sova_height).await;
                            let target = crate::miner::BuildTarget {
                                sova_height,
                                sibling: late_win,
                                null,
                            };
                            if trigger.send(target).await.is_err() {
                                tracing::warn!("trigger channel closed; sealer stopping");
                                return;
                            }
                        }
                        SealerOutcome::Rollback { to_height } => {
                            tracing::warn!(to_height, "zcash reorg observed (v0: log-only)");
                        }
                    }
                }
            }
            Err(err) => tracing::warn!(%err, "zcash view poll failed; retrying"),
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Longest the sealer waits for the expectations follower to scan a height
/// it is about to build.
const SCAN_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Wait (bounded by [`SCAN_WAIT`]) until the expectations follower has
/// scanned `sova_height`. Returns immediately on a node with no follower
/// (nothing scanned), where nothing is held.
async fn wait_for_scan(sova_height: u64) {
    let started = tokio::time::Instant::now();
    while let Some(scanned) = crate::expectations::global().scanned_through() {
        if scanned >= sova_height || started.elapsed() >= SCAN_WAIT {
            if scanned < sova_height {
                tracing::debug!(sova_height, scanned, "building ahead of our own scan");
            }
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use consensus::sealer::DEFAULT_RANK_STEP;
    use consensus::sip1::Burn;

    use super::*;
    use crate::settlements_to_withdrawals;

    fn addr(b: u8) -> [u8; 20] {
        [b; 20]
    }

    fn eb(txid_byte: u8, addr_byte: u8, value_zat: u64) -> EpochBurn {
        EpochBurn {
            txid: [txid_byte; 32],
            burn: Burn {
                evm_address: addr(addr_byte),
                signal_bits: 0,
                value_zat,
            },
        }
    }

    fn epoch(burns: Vec<EpochBurn>) -> EpochData {
        EpochData {
            height: 77,
            hash: [0x77; 32],
            burns,
            time: 0,
            txs: Vec::new(),
            pools: None,
        }
    }

    #[test]
    fn two_burner_epoch_mints_exactly_the_reward() {
        let e = epoch(vec![eb(1, 1, 600_000), eb(2, 2, 400_000)]);
        let ranked = ranked_miners(&e.burns);
        let sealer = ranked[0].evm_address;
        let attr = epoch_attribute(&e, &ranked, sealer, DRAFT_EPOCH_REWARD_GWEI)
            .unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(attr.zcash_height, 77);
        assert_eq!(attr.zcash_hash, [0x77; 32]);
        // Conservation in wei: settlements sum to reward_gwei * 1e9 exactly.
        let total: U256 = attr.settlements.iter().map(|&(_, w)| w).sum();
        assert_eq!(
            total,
            U256::from(DRAFT_EPOCH_REWARD_GWEI) * U256::from(1_000_000_000u64)
        );
        // Rank order: top burner first; sealer (rank 0 here) got tip + share.
        assert_eq!(attr.settlements[0].0, Address::from(addr(1)));
        assert!(attr.settlements[0].1 > attr.settlements[1].1);
    }

    #[test]
    fn settlements_round_trip_to_withdrawals_losslessly() {
        let e = epoch(vec![eb(1, 1, 3), eb(2, 2, 7), eb(3, 3, 11)]);
        let ranked = ranked_miners(&e.burns);
        let attr = epoch_attribute(&e, &ranked, ranked[1].evm_address, DRAFT_EPOCH_REWARD_GWEI)
            .unwrap_or_else(|err| panic!("{err}"));
        // Every settlement is gwei-aligned by construction, so the
        // withdrawal mapping must accept it and conserve the total.
        let withdrawals = settlements_to_withdrawals(&attr).unwrap_or_else(|err| panic!("{err}"));
        let gwei_total: u128 = withdrawals.iter().map(|w| u128::from(w.amount)).sum();
        assert_eq!(gwei_total, DRAFT_EPOCH_REWARD_GWEI);
    }

    #[test]
    fn identify_sealer_recovers_rank_from_withdrawals() {
        let burns = vec![eb(1, 1, 600_000), eb(2, 2, 400_000)];
        let e = epoch(burns);
        let ranked = ranked_miners(&e.burns);
        for expect_rank in 0..ranked.len() {
            let attr = epoch_attribute(
                &e,
                &ranked,
                ranked[expect_rank].evm_address,
                DRAFT_EPOCH_REWARD_GWEI,
            )
            .unwrap_or_else(|err| panic!("{err}"));
            let w = settlements_to_withdrawals(&attr).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(
                identify_sealer(&e, &ranked, DRAFT_EPOCH_REWARD_GWEI, &w),
                Some(expect_rank),
                "rank {expect_rank} must be recoverable"
            );
        }
        // Tampered withdrawals identify no sealer.
        let attr = epoch_attribute(&e, &ranked, ranked[0].evm_address, DRAFT_EPOCH_REWARD_GWEI)
            .unwrap_or_else(|err| panic!("{err}"));
        let mut w = settlements_to_withdrawals(&attr).unwrap_or_else(|err| panic!("{err}"));
        w[0].amount += 1;
        assert_eq!(
            identify_sealer(&e, &ranked, DRAFT_EPOCH_REWARD_GWEI, &w),
            None
        );
    }

    struct OneShotView {
        blocks: Vec<consensus::follower::BlockView>,
    }

    impl ZcashView for OneShotView {
        fn tip_height(&self) -> Result<u64, ViewError> {
            Ok(self.blocks.len() as u64)
        }
        fn block_at(
            &self,
            height: u64,
        ) -> Result<Option<consensus::follower::BlockView>, ViewError> {
            if height == 0 {
                return Ok(None);
            }
            Ok(self.blocks.get(height as usize - 1).cloned())
        }
    }

    fn view_with_burn(our: [u8; 20]) -> OneShotView {
        use consensus::follower::{BlockView, TxOut, TxView};
        use consensus::sip1::{BurnPayload, burn_lock_script};
        let payload = BurnPayload {
            evm_address: our,
            signal_bits: 0,
        };
        let burn_tx = TxView {
            txid: [9; 32],
            outputs: vec![
                TxOut {
                    value_zat: 0,
                    script: payload.to_script().to_vec(),
                },
                TxOut {
                    value_zat: 50_000,
                    script: burn_lock_script().to_vec(),
                },
            ],
            version: 5,
            shielded: Default::default(),
        };
        OneShotView {
            blocks: vec![
                BlockView {
                    height: 1,
                    hash: [1; 32],
                    prev_hash: [0; 32],
                    txs: vec![],
                    time: 0,
                    pools: None,
                },
                BlockView {
                    height: 2,
                    hash: [2; 32],
                    prev_hash: [1; 32],
                    txs: vec![burn_tx],
                    time: 0,
                    pools: None,
                },
            ],
        }
    }

    fn config(our: [u8; 20], rank_step: Duration) -> SealerConfig {
        SealerConfig {
            our_address: our,
            schedule: consensus::schedule::Schedule::Flat {
                reward_gwei: DRAFT_EPOCH_REWARD_GWEI,
            },
            rank_step,
            sip6: false,
        }
    }

    const NO_BEST: fn(u64) -> Option<consensus::sealer::Candidate> = |_| None;

    #[test]
    fn sealer_triggers_every_epoch_and_settles_own_burns() {
        let our = addr(0xAA);
        let view = view_with_burn(our);
        let pending = PendingEpoch::default();
        let mut core = SealerCore::new(config(our, DEFAULT_RANK_STEP), 1, 50);
        let out = core
            .process(&view, 0, &pending, Instant::now(), NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            out,
            vec![
                SealerOutcome::Trigger {
                    height: 1,
                    sova_height: 1,
                    late_win: false,
                    retry: false,
                    settled: false,
                    null: false
                },
                SealerOutcome::Trigger {
                    height: 2,
                    sova_height: 2,
                    late_win: false,
                    retry: false,
                    settled: true,
                    null: false
                },
            ]
        );
        // The burn-less epoch stages its anchor only (SIP-4).
        let anchor = pending
            .take_for(1)
            .unwrap_or_else(|| panic!("expected an anchor for the cadence height"));
        assert_eq!(anchor.zcash_hash, [1; 32]);
        assert!(anchor.settlements.is_empty());
        let staged = pending
            .take_for(2)
            .unwrap_or_else(|| panic!("expected staged epoch"));
        assert_eq!(staged.zcash_height, 2);
        assert_eq!(staged.zcash_hash, [2; 32]);
        assert_eq!(staged.settlements.len(), 1);
        assert!(pending.take().is_none(), "mailbox drains exactly once");
    }

    #[test]
    fn already_sealed_epochs_are_suppressed() {
        let our = addr(0xAA);
        let view = view_with_burn(our);
        let pending = PendingEpoch::default();
        let mut core = SealerCore::new(config(our, DEFAULT_RANK_STEP), 1, 50);
        // Head already at 2: both epochs (expected heights 1 and 2) are
        // covered — no triggers, nothing staged.
        let out = core
            .process(&view, 2, &pending, Instant::now(), NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(out.is_empty());
        assert!(pending.take().is_none());
    }

    #[test]
    fn foreign_epoch_is_left_to_its_sealer() {
        let our = addr(0xAA);
        let view = view_with_burn(addr(0xBB)); // someone else's burn
        let pending = PendingEpoch::default();
        let mut core = SealerCore::new(config(our, DEFAULT_RANK_STEP), 1, 50);
        // Only the burn-less epoch triggers a cadence block; the foreign
        // burn-bearing epoch is not ours at any rank — its block arrives
        // by relay, never from us.
        let out = core
            .process(&view, 0, &pending, Instant::now(), NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            out,
            vec![SealerOutcome::Trigger {
                height: 1,
                sova_height: 1,
                late_win: false,
                retry: false,
                settled: false,
                null: false
            }]
        );
        assert_anchor_only(&pending, 1, [1; 32]);
        assert!(pending.take().is_none());
    }

    fn assert_anchor_only(pending: &PendingEpoch, sova_height: u64, hash: [u8; 32]) {
        let anchor = pending
            .take_for(sova_height)
            .unwrap_or_else(|| panic!("expected an anchor staged for {sova_height}"));
        assert_eq!(anchor.zcash_hash, hash);
        assert!(anchor.settlements.is_empty(), "cadence blocks mint nothing");
    }

    /// Two burns in block 2: `top` outweighs `second`.
    fn view_with_two_burns(top: [u8; 20], second: [u8; 20]) -> OneShotView {
        use consensus::follower::{BlockView, TxOut, TxView};
        use consensus::sip1::{BurnPayload, burn_lock_script};
        let tx = |txid_byte: u8, who: [u8; 20], value_zat: u64| TxView {
            txid: [txid_byte; 32],
            outputs: vec![
                TxOut {
                    value_zat: 0,
                    script: BurnPayload {
                        evm_address: who,
                        signal_bits: 0,
                    }
                    .to_script()
                    .to_vec(),
                },
                TxOut {
                    value_zat,
                    script: burn_lock_script().to_vec(),
                },
            ],
            version: 5,
            shielded: Default::default(),
        };
        OneShotView {
            blocks: vec![
                BlockView {
                    height: 1,
                    hash: [1; 32],
                    prev_hash: [0; 32],
                    txs: vec![],
                    time: 0,
                    pools: None,
                },
                BlockView {
                    height: 2,
                    hash: [2; 32],
                    prev_hash: [1; 32],
                    txs: vec![tx(9, top, 60_000), tx(8, second, 40_000)],
                    time: 0,
                    pools: None,
                },
            ],
        }
    }

    #[test]
    fn lower_rank_waits_its_rung_then_seals_with_its_own_derivation() {
        let our = addr(0xAA);
        let step = Duration::from_secs(5);
        let view = view_with_two_burns(addr(0xBB), our); // we are rank 1
        let pending = PendingEpoch::default();
        let mut core = SealerCore::new(config(our, step), 1, 50);
        let t0 = Instant::now();

        // Rung not due yet: only the cadence trigger for epoch 1.
        let out = core
            .process(&view, 0, &pending, t0, NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            out,
            vec![SealerOutcome::Trigger {
                height: 1,
                sova_height: 1,
                late_win: false,
                retry: false,
                settled: false,
                null: false
            }]
        );
        assert_anchor_only(&pending, 1, [1; 32]);
        assert!(pending.take().is_none());

        // Still short of 1 * step.
        let out = core
            .process(&view, 1, &pending, t0 + Duration::from_secs(4), NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(out.is_empty());
        assert!(pending.take().is_none());

        // At 1 * step: we seal, with OUR derivation (rank 1 recoverable).
        let out = core
            .process(&view, 1, &pending, t0 + step, NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            out,
            vec![SealerOutcome::Trigger {
                height: 2,
                sova_height: 2,
                late_win: false,
                retry: false,
                settled: true,
                null: false
            }]
        );
        let staged = pending
            .take()
            .unwrap_or_else(|| panic!("expected staged epoch"));
        let withdrawals = settlements_to_withdrawals(&staged).unwrap_or_else(|err| panic!("{err}"));
        let e = EpochData {
            height: 2,
            hash: [2; 32],
            burns: vec![eb(9, 0xBB, 60_000), eb(8, 0xAA, 40_000)],
            time: 0,
            txs: Vec::new(),
            pools: None,
        };
        let ranked = ranked_miners(&e.burns);
        assert_eq!(
            identify_sealer(&e, &ranked, DRAFT_EPOCH_REWARD_GWEI, &withdrawals),
            Some(1),
            "our ladder block must derive as the rank-1 sealer"
        );
    }

    #[test]
    fn sip3_schedule_flows_through_sealing_with_exact_conservation() {
        use consensus::schedule::{SLOW_START_STEP_GWEI, Schedule};
        let our = addr(0xAA);
        let view = view_with_burn(our); // burn epoch at zcash height 2
        let pending = PendingEpoch::default();
        let mut core = SealerCore::new(
            SealerConfig {
                our_address: our,
                schedule: Schedule::Sip3,
                rank_step: DEFAULT_RANK_STEP,
                sip6: false,
            },
            1,
            50,
        );
        let out = core
            .process(&view, 0, &pending, Instant::now(), NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            out.last(),
            Some(&SealerOutcome::Trigger {
                height: 2,
                sova_height: 2,
                late_win: false,
                retry: false,
                settled: true,
                null: false
            })
        );
        // Epoch at Sova height 2 = epoch index 1: slow-start reward is
        // exactly 2 ramp steps. Conservation must hold to the wei.
        let staged = pending
            .take_for(2)
            .unwrap_or_else(|| panic!("expected staged epoch"));
        let expected_gwei = SLOW_START_STEP_GWEI * 2;
        let total_wei: U256 = staged.settlements.iter().map(|&(_, w)| w).sum();
        assert_eq!(
            total_wei,
            U256::from(expected_gwei) * U256::from(1_000_000_000u64)
        );
        // And the halving boundary halves: era 1's first epoch.
        assert_eq!(
            Schedule::Sip3.reward_gwei(consensus::schedule::ERA_EPOCHS),
            DRAFT_EPOCH_REWARD_GWEI / 2
        );
    }

    #[test]
    fn seen_better_candidate_suppresses_our_rung_until_covered() {
        let our = addr(0xAA);
        let step = Duration::from_secs(5);
        let view = view_with_two_burns(addr(0xBB), our); // we are rank 1
        let pending = PendingEpoch::default();
        let mut core = SealerCore::new(config(our, step), 1, 50);
        let t0 = Instant::now();
        let seen_rank0 = |_h: u64| {
            Some(consensus::sealer::Candidate {
                sealer_rank: 0,
                block_hash: [0xC0; 32],
            })
        };

        let _ = core
            .process(&view, 1, &pending, t0, seen_rank0)
            .unwrap_or_else(|e| panic!("{e}"));
        // Even well past our rung, a seen rank-0 candidate suppresses us.
        let out = core
            .process(&view, 1, &pending, t0 + step * 10, seen_rank0)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(out.is_empty());
        assert!(pending.take().is_none());
        // Once the height is covered (their block landed), nothing fires.
        let out = core
            .process(&view, 2, &pending, t0 + step * 20, NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(out.is_empty());
        assert!(pending.take().is_none());
    }

    fn seen(rank: usize) -> impl Fn(u64) -> Option<consensus::sealer::Candidate> {
        move |_h| {
            Some(consensus::sealer::Candidate {
                sealer_rank: rank,
                block_hash: [0xC0 + u8::try_from(rank).unwrap_or(0); 32],
            })
        }
    }

    /// The p2p ladder failure: a rank-0 sealer that was offline resumes
    /// to find the rank-1 block already covering the tip. It must seal a
    /// sibling (late win) addressed to that height — not skip the epoch,
    /// and not stack a block on top of the loser.
    #[test]
    fn covered_tip_held_by_a_worse_rank_is_won_late() {
        let our = addr(0xAA);
        let view = view_with_two_burns(our, addr(0xBB)); // we are rank 0
        let pending = PendingEpoch::default();
        let mut core = SealerCore::new(config(our, DEFAULT_RANK_STEP), 1, 50);
        let out = core
            .process(&view, 2, &pending, Instant::now(), seen(1))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            out,
            vec![SealerOutcome::Trigger {
                height: 2,
                sova_height: 2,
                late_win: true,
                retry: false,
                settled: true,
                null: false
            }]
        );
        let staged = pending
            .take_for(2)
            .unwrap_or_else(|| panic!("late-win settlement staged for height 2"));
        assert_eq!(staged.zcash_height, 2);
    }

    #[test]
    fn covered_tip_held_by_an_equal_or_better_rank_is_left_alone() {
        let our = addr(0xAA);
        let view = view_with_two_burns(addr(0xBB), our); // we are rank 1
        let pending = PendingEpoch::default();
        let mut core = SealerCore::new(config(our, DEFAULT_RANK_STEP), 1, 50);
        for best in [seen(0), seen(1)] {
            let out = core
                .process(&view, 2, &pending, Instant::now(), best)
                .unwrap_or_else(|e| panic!("{e}"));
            assert!(out.is_empty());
        }
        assert!(pending.take().is_none());
    }

    /// After a restart the candidate tracker is empty: a covered tip with
    /// no known candidate is treated as settled, never re-sealed.
    #[test]
    fn covered_tip_with_no_known_candidate_is_not_resealed() {
        let our = addr(0xAA);
        let view = view_with_two_burns(our, addr(0xBB));
        let pending = PendingEpoch::default();
        let mut core = SealerCore::new(config(our, DEFAULT_RANK_STEP), 1, 50);
        let out = core
            .process(&view, 2, &pending, Instant::now(), NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(out.is_empty());
        assert!(pending.take().is_none());
    }

    /// A trigger whose build never landed re-fires after RETRIGGER instead
    /// of losing the epoch (and does not spam before then).
    #[test]
    fn unanswered_trigger_refires_after_retrigger() {
        let our = addr(0xAA);
        let view = view_with_burn(our);
        let pending = PendingEpoch::default();
        let mut core = SealerCore::new(config(our, DEFAULT_RANK_STEP), 1, 50);
        let t0 = Instant::now();
        let first = core
            .process(&view, 0, &pending, t0, NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(first.len(), 2);
        let _ = pending.take();
        // Head never moved: nothing re-fires within the window...
        let quiet = core
            .process(&view, 0, &pending, t0 + Duration::from_secs(1), NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(quiet.is_empty());
        // ...and the front epoch re-fires once it has elapsed.
        let again = core
            .process(&view, 0, &pending, t0 + RETRIGGER, NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            again.first(),
            Some(&SealerOutcome::Trigger {
                height: 1,
                sova_height: 1,
                late_win: false,
                retry: true,
                settled: false,
                null: false
            })
        );
    }

    /// One burn crediting an address that never seals must not halt the
    /// chain: after every rung plus the grace, a non-ranked node seals
    /// rank 0's derivation — and not a moment earlier.
    #[test]
    fn abandoned_burn_epoch_is_sealed_with_rank0_derivation() {
        let our = addr(0xAA);
        let step = Duration::from_secs(5);
        let absent = addr(0xBB);
        let view = view_with_burn(absent); // one burner, never seals
        let pending = PendingEpoch::default();
        let mut core = SealerCore::new(config(our, step), 1, 50);
        let t0 = Instant::now();
        let _ = core
            .process(&view, 1, &pending, t0, NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        // 1 rank + 2 grace rungs = 15 s; at 14 s nothing fires.
        let early = core
            .process(&view, 1, &pending, t0 + Duration::from_secs(14), NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(early.is_empty());
        // A candidate already seen for the height also suppresses it.
        let seen_any = core
            .process(&view, 1, &pending, t0 + Duration::from_secs(20), seen(0))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(seen_any.is_empty());
        let out = core
            .process(&view, 1, &pending, t0 + Duration::from_secs(15), NO_BEST)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            out,
            vec![SealerOutcome::Trigger {
                height: 2,
                sova_height: 2,
                late_win: false,
                retry: false,
                settled: true,
                null: false
            }]
        );
        let staged = pending
            .take_for(2)
            .unwrap_or_else(|| panic!("abandoned epoch staged"));
        let w = settlements_to_withdrawals(&staged).unwrap_or_else(|e| panic!("{e}"));
        let e = EpochData {
            height: 2,
            hash: [2; 32],
            burns: vec![eb(9, 0xBB, 50_000)],
            time: 0,
            txs: Vec::new(),
            pools: None,
        };
        assert_eq!(
            identify_sealer(&e, &ranked_miners(&e.burns), DRAFT_EPOCH_REWARD_GWEI, &w),
            Some(0),
            "the fallback block must derive exactly as rank 0's own block"
        );
    }

    #[test]
    fn empty_epoch_and_foreign_sealer_are_rejected() {
        let empty = epoch(vec![]);
        assert_eq!(
            epoch_attribute(&empty, &[], addr(1), DRAFT_EPOCH_REWARD_GWEI),
            Err(EpochAttributeError::EmptyEpoch)
        );
        let e = epoch(vec![eb(1, 1, 1_000)]);
        let ranked = ranked_miners(&e.burns);
        assert_eq!(
            epoch_attribute(&e, &ranked, addr(9), DRAFT_EPOCH_REWARD_GWEI),
            Err(EpochAttributeError::RewardsInvalid)
        );
    }
}
