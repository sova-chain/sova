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
    /// Lowest pending rollback floor + 1 (0 = none): after a Zcash reorg
    /// unwinds us to Sova height `N_R`, canonical blocks above `N_R` may
    /// be anchored to orphaned Zcash blocks until they are re-sealed
    /// (SIP-4 §7). See [`Self::effective_head`].
    unwound: AtomicU64,
}

/// How far [`ExpectedSettlements::effective_head`] walks down a stale tip
/// when no rollback is pending: past Zebra's 99-block reorg limit.
pub const STALE_SCAN_MAX: u64 = 100;

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
    /// from the replacement chain) and remember `height` as a rollback
    /// floor for [`Self::effective_head`].
    pub fn unwind_above(&self, height: u64) {
        if let Ok(mut map) = self.map.lock() {
            map.retain(|&h, _| h <= height);
            self.scanned.fetch_min(height, Ordering::SeqCst);
        }
        let floor = height.saturating_add(1);
        let _ = self
            .unwound
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                Some(if cur == 0 { floor } else { cur.min(floor) })
            });
    }

    /// Whether the canonical block at `height`, whose Zcash anchor is
    /// `canonical_anchor`, commits to a Zcash block our follower no longer
    /// has there (it was built on a branch Zcash reorged away).
    #[must_use]
    pub fn is_stale(&self, height: u64, canonical_anchor: Option<[u8; 32]>) -> bool {
        self.record(height)
            .is_some_and(|r| canonical_anchor != Some(r.epoch.hash))
    }

    /// SIP-4 §7: the head the sealer and arbiter should act on. After a
    /// Zcash reorg unwound us to `N_R`, canonical blocks above `N_R` are
    /// stale until the block at `N_R + 1` matches our follower's new branch;
    /// until then the effective head is `N_R`, so the sealer re-seals
    /// `N_R + 1` on its canonical parent and the arbiter adopts a
    /// replacement below the (stale) tip. reth then reorgs the stale
    /// blocks out when the replacement becomes head.
    /// `anchor_at(h)` reads the canonical block's `parent_beacon_block_root`.
    ///
    /// Without a pending rollback (a restart, or an unwind this process
    /// never saw) the stale tail is found directly: walk down from `head`
    /// while the canonical block is anchored to a Zcash block our follower
    /// no longer has. Usually one check. Capped at [`STALE_SCAN_MAX`] blocks,
    /// beyond Zebra's 99-block reorg limit. (Testnet stall 2026-09-24: a
    /// restarted keeper treated a stale tip as its head and never re-sealed.)
    pub fn effective_head(&self, head: u64, anchor_at: impl Fn(u64) -> Option<[u8; 32]>) -> u64 {
        let pending = self.unwound.load(Ordering::SeqCst);
        if pending == 0 {
            let mut h = head;
            while h > 0 && head - h < STALE_SCAN_MAX && self.is_stale(h, anchor_at(h)) {
                h -= 1;
            }
            return h;
        }
        let floor = pending - 1;
        let first = floor.saturating_add(1);
        let resolved = head <= floor
            || self
                .record(first)
                .is_some_and(|r| anchor_at(first) == Some(r.epoch.hash));
        if resolved {
            let _ = self
                .unwound
                .compare_exchange(pending, 0, Ordering::SeqCst, Ordering::SeqCst);
            return head;
        }
        floor
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

    /// SIP-6 §2.3/§2.4: the settlement rule with the block's producer
    /// known. A **signed** block is valid only if its signer is one of the
    /// epoch's ranked burners (so a block for a burn-less epoch can't be
    /// signed) and its withdrawals are exactly that burner's derivation;
    /// its rank is the signer's place, not read from the tip. A **null**
    /// block is valid for any scanned epoch and must mint nothing (Rob's
    /// D5). Before activation ([`crate::seal::Sealer::Unsealed`]) this is
    /// [`Self::check_ranked`].
    #[must_use]
    pub fn check_sealed(
        &self,
        height: u64,
        actual: Option<&[Withdrawal]>,
        schedule: consensus::schedule::Schedule,
        sealer: crate::seal::Sealer,
    ) -> RankedVerdict {
        use crate::seal::Sealer;
        let signer = match sealer {
            Sealer::Unsealed => return self.check_ranked(height, actual, schedule),
            Sealer::Null => {
                if self.record(height).is_none() {
                    return RankedVerdict::Unknown;
                }
                return if actual.unwrap_or(&[]).is_empty() {
                    RankedVerdict::ValidEmpty
                } else {
                    RankedVerdict::Mismatch { height }
                };
            }
            Sealer::Signed(signer) => signer,
        };
        let Some(record) = self.record(height) else {
            return RankedVerdict::Unknown;
        };
        let Some(rank) = record
            .ranked
            .iter()
            .position(|m| m.evm_address == signer.0.0)
        else {
            return RankedVerdict::Mismatch { height };
        };
        let actual = actual.unwrap_or(&[]);
        let reward_gwei = schedule.reward_gwei(height.saturating_sub(1));
        if reward_gwei == 0 {
            // Past emission: signed burn epochs stay ranked (fees pay the
            // sealer) and mint nothing.
            return if actual.is_empty() {
                RankedVerdict::Valid { rank }
            } else {
                RankedVerdict::Mismatch { height }
            };
        }
        let derived =
            crate::driver::epoch_attribute(&record.epoch, &record.ranked, signer.0.0, reward_gwei)
                .ok()
                .and_then(|attr| crate::settlements_to_withdrawals(&attr).ok());
        if derived.as_deref() == Some(actual) {
            RankedVerdict::Valid { rank }
        } else {
            RankedVerdict::Mismatch { height }
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
    // SIP-7: the follower that feeds the index holds on missing or
    // inconsistent pool accounting (a stall and an alert, never an answer).
    let mut follower = Follower::new(base_height, REORG_WINDOW)
        .with_strict_pools(evm::zcash::sip7_active())
        .with_sip8_from(crate::votes::sip8_from());
    loop {
        match follower.poll(&view) {
            Ok(events) => {
                if let Some(why) = follower.hold_reason() {
                    tracing::error!(%why, "sip-7 hold: zcash scan stopped before this block");
                }
                for event in events {
                    match event {
                        FollowerEvent::Rollback { to_height } => {
                            let sova = to_height.saturating_sub(base_height).saturating_add(1);
                            global().unwind_above(sova);
                            crate::candidates::global().unwind_above(sova);
                            crate::zcash_index::global().unwind_above(to_height);
                            crate::votes::global().unwind_above(to_height);
                            tracing::warn!(
                                to_height,
                                "expectations and candidates unwound (zcash reorg)"
                            );
                        }
                        FollowerEvent::Epoch(mut epoch) => {
                            // Index first: once the expectations watermark
                            // lets a block through consensus, the precompile
                            // must already cover its anchor.
                            crate::zcash_index::global().insert(&epoch);
                            crate::votes::global().insert(&epoch);
                            epoch.txs = Vec::new();
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
                time: 0,
                txs: Vec::new(),
                pools: None,
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
                reference: None,
            },
            EpochBurn {
                txid: [2; 32],
                burn: Burn {
                    evm_address: [2; 20],
                    signal_bits: 0,
                    value_zat: 400_000,
                },
                reference: None,
            },
        ];
        let epoch = EpochData {
            height: 100,
            hash: [7; 32],
            burns,
            time: 0,
            txs: Vec::new(),
            pools: None,
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

    /// SIP-6 §2.3/§2.4: with a seal, the rank comes from the signer.
    #[test]
    fn check_sealed_ranks_by_signer_and_nulls_mint_nothing() {
        use alloy_primitives::Address;
        use consensus::epoch::EpochBurn;
        use consensus::sip1::Burn;

        use crate::driver::{DRAFT_EPOCH_REWARD_GWEI, epoch_attribute, ranked_miners};
        use crate::seal::Sealer;
        use crate::settlements_to_withdrawals;

        const FLAT: consensus::schedule::Schedule = consensus::schedule::Schedule::Flat {
            reward_gwei: DRAFT_EPOCH_REWARD_GWEI,
        };
        let burn = |a: u8, zat: u64| EpochBurn {
            txid: [a; 32],
            burn: Burn {
                evm_address: [a; 20],
                signal_bits: 0,
                value_zat: zat,
            },
            reference: None,
        };
        let epoch = EpochData {
            height: 100,
            hash: [7; 32],
            burns: vec![burn(1, 600_000), burn(2, 400_000)],
            time: 0,
            txs: Vec::new(),
            pools: None,
        };
        let ranked = ranked_miners(&epoch.burns);
        let derive = |sealer: [u8; 20]| {
            settlements_to_withdrawals(
                &epoch_attribute(&epoch, &ranked, sealer, DRAFT_EPOCH_REWARD_GWEI)
                    .unwrap_or_else(|err| panic!("{err}")),
            )
            .unwrap_or_else(|err| panic!("{err}"))
        };
        let e = ExpectedSettlements::default();
        let signed = |a: [u8; 20]| Sealer::Signed(Address::from(a));
        assert_eq!(
            e.check_sealed(9, None, FLAT, Sealer::Null),
            RankedVerdict::Unknown,
            "unscanned: unknown, never valid"
        );
        e.insert(
            9,
            HeightRecord {
                withdrawals: derive(ranked[0].evm_address),
                epoch: epoch.clone(),
                ranked: ranked.clone(),
            },
        );
        let (r0, r1) = (ranked[0].evm_address, ranked[1].evm_address);
        // Each ranked signer with its own derivation, at its own rank.
        assert_eq!(
            e.check_sealed(9, Some(&derive(r0)), FLAT, signed(r0)),
            RankedVerdict::Valid { rank: 0 }
        );
        assert_eq!(
            e.check_sealed(9, Some(&derive(r1)), FLAT, signed(r1)),
            RankedVerdict::Valid { rank: 1 }
        );
        // Rank 1 signing rank 0's derivation (claiming the tip) is invalid,
        // and so is an unranked signer with a perfect copy.
        let bad = RankedVerdict::Mismatch { height: 9 };
        assert_eq!(e.check_sealed(9, Some(&derive(r0)), FLAT, signed(r1)), bad);
        assert_eq!(
            e.check_sealed(9, Some(&derive(r0)), FLAT, signed([9; 20])),
            bad
        );
        // A null block is valid on a burn epoch only if it mints nothing.
        assert_eq!(
            e.check_sealed(9, None, FLAT, Sealer::Null),
            RankedVerdict::ValidEmpty
        );
        assert_eq!(
            e.check_sealed(9, Some(&derive(r0)), FLAT, Sealer::Null),
            bad
        );
        // A burn-less epoch: only the null block; nobody is ranked to sign.
        e.insert(10, rec(Vec::new()));
        assert_eq!(
            e.check_sealed(10, None, FLAT, Sealer::Null),
            RankedVerdict::ValidEmpty
        );
        assert_eq!(
            e.check_sealed(10, None, FLAT, signed(r0)),
            RankedVerdict::Mismatch { height: 10 }
        );
        // Unsealed (SIP-6 off) is the old rule.
        assert_eq!(
            e.check_sealed(9, Some(&derive(r1)), FLAT, Sealer::Unsealed),
            RankedVerdict::Valid { rank: 1 }
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

    fn rb_rec(hash: u8) -> HeightRecord {
        HeightRecord {
            withdrawals: Vec::new(),
            epoch: EpochData {
                height: 0,
                hash: [hash; 32],
                burns: Vec::new(),
                time: 0,
                txs: Vec::new(),
                pools: None,
            },
            ranked: Vec::new(),
        }
    }

    /// SIP-4 §7: after a Zcash reorg to N_R, the effective head is N_R
    /// until the block at N_R + 1 matches the new branch; then it clears.
    #[test]
    fn effective_head_holds_at_the_rollback_floor_until_resealed() {
        let e = ExpectedSettlements::default();
        for h in 1..=10 {
            e.insert(h, rb_rec(h as u8));
        }
        // Canonical chain 1..=15 anchored to the old branch (hash = h).
        let old = |h: u64| Some([h as u8; 32]);
        assert_eq!(e.effective_head(15, old), 15, "no rollback yet");
        e.unwind_above(4);
        // New branch rescanned: heights 5.. now carry different hashes.
        for h in 5..=12 {
            e.insert(h, rb_rec(0x80 + h as u8));
        }
        assert!(e.is_stale(5, old(5)));
        assert!(!e.is_stale(4, old(4)));
        assert_eq!(e.effective_head(15, old), 4, "stale above N_R");
        // The replacement at 5 became canonical.
        let new = |h: u64| {
            Some(if h >= 5 {
                [0x80 + h as u8; 32]
            } else {
                [h as u8; 32]
            })
        };
        assert_eq!(e.effective_head(5, new), 5);
        assert_eq!(e.effective_head(15, old), 15, "cleared once resolved");
    }

    /// The testnet stall (2026-09-24): after a restart no rollback is
    /// pending, but the stored tip is anchored to an orphaned Zcash block.
    /// The effective head must still drop below it so the sealer re-seals.
    #[test]
    fn a_stale_tip_is_found_without_a_rollback_event() {
        let e = ExpectedSettlements::default();
        // Follower's (current) branch: heights 1..=10.
        for h in 1..=10 {
            e.insert(h, rb_rec(h as u8));
        }
        // Stored chain: 1..=7 match, 8 and 9 were built on an orphaned branch.
        let stored = |h: u64| Some(if h >= 8 { [0xee; 32] } else { [h as u8; 32] });
        assert_eq!(e.effective_head(9, stored), 7);
        assert_eq!(e.effective_head(7, stored), 7);
        // A head above what the follower has scanned is not stale.
        assert_eq!(e.effective_head(12, |h| Some([h as u8; 32])), 12);
        // Nothing matches: the walk stops after STALE_SCAN_MAX blocks.
        let big = ExpectedSettlements::default();
        for h in 1..=300 {
            big.insert(h, rb_rec(1));
        }
        assert_eq!(
            big.effective_head(300, |_| Some([0xee; 32])),
            300 - STALE_SCAN_MAX
        );
    }

    #[test]
    fn a_rollback_above_the_head_changes_nothing() {
        let e = ExpectedSettlements::default();
        e.insert(1, rb_rec(1));
        e.unwind_above(7);
        assert_eq!(e.effective_head(3, |_| None), 3);
    }
}
