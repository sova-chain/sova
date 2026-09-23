//! C5: settlement re-derivation on import.
//!
//! Every node — importer or sealer — derives, from its **own** Zcash
//! view, the withdrawals each Sova height is required to carry, and the
//! engine validator rejects imported payloads that disagree. This turns
//! the two-node determinism *observation* into an enforced rule: a peer
//! cannot mint what your zebrad doesn't justify.
//!
//! Scope, stated plainly:
//! - Expected withdrawals are sealer-dependent (the tip lands on the
//!   sealer). Since v2 the import check is
//!   [`ExpectedSettlements::check_ranked`]: a block is valid when its
//!   withdrawals match **some** rank's derivation (the tip makes each
//!   sealer's derivation distinct, so [`crate::driver::identify_sealer`]
//!   recovers the rank with no metadata channel), and rank-then-hash
//!   *preference* — not validity — arbitrates between candidates
//!   ([`crate::candidates`]). The stored [`HeightRecord::withdrawals`]
//!   remains the rank-0 derivation, the strict v1 rule kept by
//!   [`ExpectedSettlements::check`].
//! - Heights the local follower hasn't scanned yet verify as
//!   [`Verdict::Unknown`] and are accepted with a warning — refusing
//!   them would deadlock a node that imports faster than it scans.
//!   Post-v1 hardening can hold such payloads instead.
//! - Every height at or below the follower's scanned watermark
//!   ([`ExpectedSettlements::scanned_through`]) has a record — history is
//!   never pruned — so [`crate::consensus::SovaConsensus`] can enforce C5
//!   on every import path, including history a joining node syncs through
//!   reth's download and backfill paths (which never call the payload
//!   validator). Memory grows ~linearly with chain age; a persistent store
//!   is the follow-up once that matters.
//! - Wiring uses a process-global ([`global`]) because reth's add-on
//!   builders are constructed statically; proper dependency injection
//!   through `SovaNodeAddOns` is a TODO recorded here deliberately.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use alloy_rpc_types::Withdrawal;
use consensus::follower::{Follower, FollowerEvent, ZcashView};

use consensus::epoch::MinerWeight;
use consensus::follower::EpochData;

use crate::driver::{epoch_attribute, ranked_miners};
use crate::settlements_to_withdrawals;

/// Zcash reorg window the expectations follower tracks; reorgs deeper
/// than this unwind to base.
const REORG_WINDOW: usize = 1024;

/// Verdict of checking a payload's withdrawals against expectations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Withdrawals match the local derivation exactly.
    Match,
    /// Withdrawals contradict the local derivation: invalid payload.
    Mismatch {
        /// The Sova height checked.
        height: u64,
    },
    /// No expectation recorded for this height (not yet scanned, or
    /// before the follower's base).
    Unknown,
}

/// Verdict of the v2 any-rank check ([`ExpectedSettlements::check_ranked`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankedVerdict {
    /// Withdrawals match rank `rank`'s derivation exactly.
    Valid {
        /// The recovered sealer rank (0 = top burner).
        rank: usize,
    },
    /// Burn-less epoch sealed with the required empty withdrawals; no
    /// rank exists to arbitrate on.
    ValidEmpty,
    /// Withdrawals match no rank's derivation: invalid payload.
    Mismatch {
        /// The Sova height checked.
        height: u64,
    },
    /// No expectation recorded for this height.
    Unknown,
}

/// Verdict of checking a block's SIP-4 Zcash anchor
/// (`parent_beacon_block_root`) against the local follower.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorVerdict {
    /// The block commits to the Zcash block our follower has at its epoch.
    Match,
    /// The block commits to a different Zcash block (or none): the sealer
    /// was on another Zcash fork, or the block is forged. Indistinguishable
    /// locally, so callers treat it as a hold, never as permanent.
    Mismatch {
        /// Our follower's hash at the epoch.
        expected: [u8; 32],
    },
    /// No record for this height.
    Unknown,
}

/// Per-height record: the rank-0 withdrawals requirement plus the epoch
/// context v2 preference needs (identify_sealer takes the epoch and its
/// ranking to recover an imported block's sealer rank).
#[derive(Debug, Clone)]
pub struct HeightRecord {
    /// Withdrawals required under the v1 rank-0-seals rule.
    pub withdrawals: Vec<Withdrawal>,
    /// The epoch as the local follower saw it.
    pub epoch: EpochData,
    /// The epoch's ranked miners.
    pub ranked: Vec<MinerWeight>,
}

/// Shared map of Sova height → required withdrawals + epoch context.
#[derive(Debug, Default)]
pub struct ExpectedSettlements {
    map: Mutex<BTreeMap<u64, HeightRecord>>,
    /// Highest Sova height with a record, contiguous from the first
    /// scanned height; 0 = nothing scanned (Sova heights start at 1).
    scanned: AtomicU64,
}

impl ExpectedSettlements {
    /// Record the requirement for a height and advance the scanned
    /// watermark to it. The follower emits epochs in order, so the
    /// watermark only ever moves forward here.
    pub fn insert(&self, height: u64, record: HeightRecord) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(height, record);
            self.scanned.fetch_max(height, Ordering::SeqCst);
        }
    }

    /// Highest Sova height whose expectation is known, or `None` before
    /// the follower has scanned anything (including nodes with no zebrad).
    #[must_use]
    pub fn scanned_through(&self) -> Option<u64> {
        match self.scanned.load(Ordering::SeqCst) {
            0 => None,
            h => Some(h),
        }
    }

    /// The full record for a height (epoch context for v2 preference).
    #[must_use]
    pub fn record(&self, height: u64) -> Option<HeightRecord> {
        self.map.lock().ok()?.get(&height).cloned()
    }

    /// Drop expectations above `height` (Zcash reorg: they regenerate
    /// from the replacement chain).
    pub fn unwind_above(&self, height: u64) {
        if let Ok(mut map) = self.map.lock() {
            map.retain(|&h, _| h <= height);
            self.scanned.fetch_min(height, Ordering::SeqCst);
        }
    }

    /// SIP-4 anchor check: a block at `height` must commit, in
    /// `parent_beacon_block_root`, to the hash of the Zcash block its
    /// epoch closed at, as this node's follower saw it.
    #[must_use]
    pub fn check_anchor(&self, height: u64, root: Option<alloy_primitives::B256>) -> AnchorVerdict {
        let Some(record) = self.record(height) else {
            return AnchorVerdict::Unknown;
        };
        if root.is_some_and(|r| r.0 == record.epoch.hash) {
            AnchorVerdict::Match
        } else {
            AnchorVerdict::Mismatch {
                expected: record.epoch.hash,
            }
        }
    }

    /// v2 check: a payload's withdrawals are valid when they match the
    /// derivation for **some** rank of the height's epoch — the ladder
    /// makes every rank a legitimate sealer; preference (not validity)
    /// arbitrates between them. Burn-less epochs — and epochs whose
    /// scheduled reward is zero (post-emission) — require empty
    /// withdrawals and carry no rank.
    #[must_use]
    pub fn check_ranked(
        &self,
        height: u64,
        actual: Option<&[Withdrawal]>,
        schedule: consensus::schedule::Schedule,
    ) -> RankedVerdict {
        let Some(record) = self.record(height) else {
            return RankedVerdict::Unknown;
        };
        let actual = actual.unwrap_or(&[]);
        let reward_gwei = schedule.reward_gwei(height.saturating_sub(1));
        if record.ranked.is_empty() || reward_gwei == 0 {
            return if actual.is_empty() {
                RankedVerdict::ValidEmpty
            } else {
                RankedVerdict::Mismatch { height }
            };
        }
        match crate::driver::identify_sealer(&record.epoch, &record.ranked, reward_gwei, actual) {
            Some(rank) => RankedVerdict::Valid { rank },
            None => RankedVerdict::Mismatch { height },
        }
    }

    /// Check a payload's withdrawals (`None` treated as empty) for a
    /// height against the recorded requirement.
    ///
    /// This is the v1 rule (rank 0 seals, exactly one valid derivation);
    /// the import path uses [`Self::check_ranked`] since v2. Kept as the
    /// strict primitive — it is what `check_ranked` degenerates to when
    /// only rank 0's block exists.
    #[must_use]
    pub fn check(&self, height: u64, actual: Option<&[Withdrawal]>) -> Verdict {
        let Ok(map) = self.map.lock() else {
            return Verdict::Unknown;
        };
        match map.get(&height) {
            None => Verdict::Unknown,
            Some(record) => {
                if record.withdrawals.as_slice() == actual.unwrap_or(&[]) {
                    Verdict::Match
                } else {
                    Verdict::Mismatch { height }
                }
            }
        }
    }
}

/// Process-global expectations instance (see module docs for why).
pub fn global() -> &'static ExpectedSettlements {
    static GLOBAL: OnceLock<ExpectedSettlements> = OnceLock::new();
    GLOBAL.get_or_init(ExpectedSettlements::default)
}

static SCHEDULE: OnceLock<consensus::schedule::Schedule> = OnceLock::new();

/// Set the process-wide emission schedule (SIP-3), once, at startup —
/// before any block is imported. Returns false if already set. The
/// same value must feed the sealer's [`crate::driver::SealerConfig`];
/// bin/sova constructs both from one place.
pub fn set_schedule(schedule: consensus::schedule::Schedule) -> bool {
    SCHEDULE.set(schedule).is_ok()
}

/// The process-wide emission schedule; defaults to the flat draft
/// reward (regtest/box behavior) when never set.
#[must_use]
pub fn schedule() -> consensus::schedule::Schedule {
    *SCHEDULE
        .get()
        .unwrap_or(&consensus::schedule::Schedule::Flat {
            reward_gwei: crate::driver::DRAFT_EPOCH_REWARD_GWEI,
        })
}

/// Feed [`global`] from a Zcash view: one required-withdrawals entry per
/// epoch (empty for burn-less epochs), keyed by expected Sova height
/// (`E − base + 1`), assuming rank-0 seals (v1 rule; see module docs).
pub async fn run_expectations<V: ZcashView>(
    view: V,
    base_height: u64,
    schedule: consensus::schedule::Schedule,
    poll_interval: std::time::Duration,
) {
    let mut follower = Follower::new(base_height, REORG_WINDOW);
    loop {
        match follower.poll(&view) {
            Ok(events) => {
                for event in events {
                    match event {
                        FollowerEvent::Rollback { to_height } => {
                            let sova = to_height.saturating_sub(base_height).saturating_add(1);
                            global().unwind_above(sova);
                            crate::candidates::global().unwind_above(sova);
                            tracing::warn!(
                                to_height,
                                "expectations and candidates unwound (zcash reorg)"
                            );
                        }
                        FollowerEvent::Epoch(epoch) => {
                            let sova_height =
                                epoch.height.saturating_sub(base_height).saturating_add(1);
                            let reward_gwei = schedule.reward_gwei(sova_height.saturating_sub(1));
                            let ranked = ranked_miners(&epoch.burns);
                            let withdrawals = match ranked.first() {
                                Some(rank0) if reward_gwei > 0 => {
                                    epoch_attribute(&epoch, &ranked, rank0.evm_address, reward_gwei)
                                        .ok()
                                        .and_then(|attr| settlements_to_withdrawals(&attr).ok())
                                        .unwrap_or_default()
                                }
                                _ => Vec::new(),
                            };
                            global().insert(
                                sova_height,
                                HeightRecord {
                                    withdrawals,
                                    epoch,
                                    ranked,
                                },
                            );
                            // Candidates imported ahead of this scan now get
                            // their real sealer rank (see candidates.rs).
                            let rank_of = |w: &[Withdrawal]| match global().check_ranked(
                                sova_height,
                                Some(w),
                                schedule,
                            ) {
                                RankedVerdict::Valid { rank } => Some(rank),
                                RankedVerdict::ValidEmpty => Some(usize::MAX),
                                RankedVerdict::Mismatch { .. } | RankedVerdict::Unknown => None,
                            };
                            if let Some(best) =
                                crate::candidates::global().rerank(sova_height, rank_of)
                            {
                                crate::candidates::notify_best(crate::candidates::BestCandidate {
                                    sova_height,
                                    block_hash: best.block_hash,
                                });
                            }
                        }
                    }
                }
            }
            Err(err) => tracing::warn!(%err, "expectations poll failed; retrying"),
        }
        tokio::time::sleep(poll_interval).await;
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;

    use super::*;

    fn rec(withdrawals: Vec<Withdrawal>) -> HeightRecord {
        HeightRecord {
            withdrawals,
            epoch: EpochData {
                height: 0,
                hash: [0; 32],
                burns: Vec::new(),
            },
            ranked: Vec::new(),
        }
    }

    fn w(i: u64, addr_byte: u8, amount: u64) -> Withdrawal {
        Withdrawal {
            index: i,
            validator_index: 0,
            address: Address::with_last_byte(addr_byte),
            amount,
        }
    }

    #[test]
    fn check_verdicts() {
        let e = ExpectedSettlements::default();
        assert_eq!(e.check(5, None), Verdict::Unknown);

        e.insert(5, rec(vec![w(0, 1, 100)]));
        assert_eq!(e.check(5, Some(&[w(0, 1, 100)])), Verdict::Match);
        assert_eq!(
            e.check(5, Some(&[w(0, 2, 100)])),
            Verdict::Mismatch { height: 5 }
        );
        assert_eq!(e.check(5, None), Verdict::Mismatch { height: 5 });

        // Burn-less epoch: expected empty matches both None and [].
        e.insert(6, rec(Vec::new()));
        assert_eq!(e.check(6, None), Verdict::Match);
        assert_eq!(e.check(6, Some(&[])), Verdict::Match);
        assert_eq!(
            e.check(6, Some(&[w(0, 1, 1)])),
            Verdict::Mismatch { height: 6 }
        );
    }

    #[test]
    fn check_ranked_accepts_any_rank_rejects_no_rank() {
        use consensus::epoch::EpochBurn;
        use consensus::sip1::Burn;

        use crate::driver::{DRAFT_EPOCH_REWARD_GWEI, epoch_attribute, ranked_miners};
        use crate::settlements_to_withdrawals;

        const FLAT: consensus::schedule::Schedule = consensus::schedule::Schedule::Flat {
            reward_gwei: DRAFT_EPOCH_REWARD_GWEI,
        };

        let burns = vec![
            EpochBurn {
                txid: [1; 32],
                burn: Burn {
                    evm_address: [1; 20],
                    signal_bits: 0,
                    value_zat: 600_000,
                },
            },
            EpochBurn {
                txid: [2; 32],
                burn: Burn {
                    evm_address: [2; 20],
                    signal_bits: 0,
                    value_zat: 400_000,
                },
            },
        ];
        let epoch = EpochData {
            height: 100,
            hash: [7; 32],
            burns,
        };
        let ranked = ranked_miners(&epoch.burns);

        let e = ExpectedSettlements::default();
        assert_eq!(e.check_ranked(9, None, FLAT), RankedVerdict::Unknown);

        // Record the epoch with the rank-0 requirement (as run_expectations does).
        let rank0_w = settlements_to_withdrawals(
            &epoch_attribute(
                &epoch,
                &ranked,
                ranked[0].evm_address,
                DRAFT_EPOCH_REWARD_GWEI,
            )
            .unwrap_or_else(|err| panic!("{err}")),
        )
        .unwrap_or_else(|err| panic!("{err}"));
        e.insert(
            9,
            HeightRecord {
                withdrawals: rank0_w.clone(),
                epoch: epoch.clone(),
                ranked: ranked.clone(),
            },
        );

        // Every rank's derivation is valid and recovers its rank.
        for rank in 0..ranked.len() {
            let w = settlements_to_withdrawals(
                &epoch_attribute(
                    &epoch,
                    &ranked,
                    ranked[rank].evm_address,
                    DRAFT_EPOCH_REWARD_GWEI,
                )
                .unwrap_or_else(|err| panic!("{err}")),
            )
            .unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(
                e.check_ranked(9, Some(&w), FLAT),
                RankedVerdict::Valid { rank }
            );
        }

        // Tampered withdrawals match no rank; empty withdrawals on a
        // burn-bearing epoch are reward-withholding, also invalid.
        let mut tampered = rank0_w;
        tampered[0].amount += 1;
        assert_eq!(
            e.check_ranked(9, Some(&tampered), FLAT),
            RankedVerdict::Mismatch { height: 9 }
        );
        assert_eq!(
            e.check_ranked(9, None, FLAT),
            RankedVerdict::Mismatch { height: 9 }
        );

        // Burn-less epoch: empty withdrawals valid (no rank), any payout invalid.
        e.insert(10, rec(Vec::new()));
        assert_eq!(e.check_ranked(10, None, FLAT), RankedVerdict::ValidEmpty);
        assert_eq!(
            e.check_ranked(10, Some(&[w(0, 1, 1)]), FLAT),
            RankedVerdict::Mismatch { height: 10 }
        );
    }

    #[test]
    fn unwind_drops_above() {
        let e = ExpectedSettlements::default();
        for h in 1..=10 {
            e.insert(h, rec(Vec::new()));
        }
        e.unwind_above(4);
        assert_eq!(e.check(4, None), Verdict::Match);
        assert_eq!(e.check(5, None), Verdict::Unknown);
        assert_eq!(e.check(10, None), Verdict::Unknown);
    }
}
