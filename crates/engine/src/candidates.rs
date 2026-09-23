//! v2 preference: the per-epoch candidate tracker (pure core).
//!
//! Receivers arbitrate instead of trusting the sender's FCU (see the v2
//! addendum in docs/design/gossip-v1.md): every imported candidate block
//! for an epoch is observed here with its recovered sealer rank
//! ([`crate::driver::identify_sealer`] — no metadata channel), and the
//! tracker answers the only question fork choice asks: *is this
//! candidate strictly preferred over the best one seen for its epoch?*
//! Preference is `consensus::sealer::prefer` — (rank asc, hash asc),
//! never arrival time. The engine validator observes candidates at
//! import ([`crate::SovaEngineValidator`]) and, on [`Observation::NewBest`],
//! notifies the FCU arbiter ([`run_arbiter`]) through a process-global
//! channel (same wiring pattern — and the same recorded debt — as
//! [`crate::expectations::global`]).
//!
//! v2 caveats, stated plainly:
//! - Candidates are observed at settlement-validation time, *before*
//!   execution. A well-formed block that later fails execution still
//!   occupies the tracker's best slot for its epoch and can suppress an
//!   honest same-rank candidate (hash tiebreak) until the epoch passes.
//!   Observing on canonical-commit instead is the post-v2 hardening.
//! - Burn-less epochs converge by hash tiebreak (both producers' empty
//!   blocks observed at rank `usize::MAX`), but only while neither has
//!   built *past* the epoch: if a producer stacks its next block before
//!   the preferred empty arrives, the arbiter's stale-height skip keeps
//!   it on its own lineage and the fork outlives the epoch. Real fix is
//!   parent-aware preference or the `extension_rank` ladder for empty
//!   epochs (v2.1); at box scale the 1s relay against the 2s+ cadence
//!   makes the window small, and the nightly sim will measure it.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use alloy_rpc_types::Withdrawal;
use consensus::sealer::{Candidate, prefer};

/// How many trailing epochs of candidates to retain.
const RETAIN: usize = 1024;

/// Result of observing a candidate for an epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// First candidate seen for this epoch, or strictly preferred over
    /// the previous best: fork choice should adopt it.
    NewBest,
    /// Equal to or worse than the current best: ignore.
    NotBetter,
}

/// A candidate awaiting its rank: block hash + the withdrawals to rank it by.
type Unranked = ([u8; 32], Vec<Withdrawal>);

/// Tracks the best candidate per epoch (keyed by Sova height).
#[derive(Debug, Default)]
pub struct CandidateTracker {
    best: Mutex<BTreeMap<u64, Candidate>>,
    /// Candidates observed before our follower had scanned their epoch —
    /// held at trust rank `usize::MAX` — with the withdrawals needed to
    /// rank them once it has ([`CandidateTracker::rerank`]). Without this
    /// a node's *own* block imported ahead of its scan kept the trust rank
    /// forever, and its sealer then saw a "worse-ranked" block at the tip
    /// and tried to win its own epoch back (seen on slow CI runners).
    unranked: Mutex<BTreeMap<u64, Vec<Unranked>>>,
}

impl CandidateTracker {
    /// Observe a candidate block for `sova_height`; returns whether it
    /// becomes the epoch's best.
    pub fn observe(&self, sova_height: u64, candidate: Candidate) -> Observation {
        let Ok(mut best) = self.best.lock() else {
            return Observation::NotBetter;
        };
        let outcome = match best.get(&sova_height) {
            None => Observation::NewBest,
            Some(current) => {
                if prefer(&candidate, current) == std::cmp::Ordering::Less {
                    Observation::NewBest
                } else {
                    Observation::NotBetter
                }
            }
        };
        if outcome == Observation::NewBest {
            best.insert(sova_height, candidate);
            while best.len() > RETAIN {
                let Some((&lowest, _)) = best.first_key_value() else {
                    break;
                };
                best.remove(&lowest);
            }
        }
        outcome
    }

    /// Observe a candidate whose epoch our follower hasn't scanned yet: it
    /// competes at trust rank `usize::MAX` until [`Self::rerank`] runs for
    /// its height.
    pub fn observe_unranked(
        &self,
        sova_height: u64,
        block_hash: [u8; 32],
        withdrawals: Vec<Withdrawal>,
    ) -> Observation {
        if let Ok(mut unranked) = self.unranked.lock() {
            let entry = unranked.entry(sova_height).or_default();
            if !entry.iter().any(|(hash, _)| *hash == block_hash) {
                entry.push((block_hash, withdrawals));
            }
            while unranked.len() > RETAIN {
                let Some((&lowest, _)) = unranked.first_key_value() else {
                    break;
                };
                unranked.remove(&lowest);
            }
        }
        self.observe(
            sova_height,
            Candidate {
                sealer_rank: usize::MAX,
                block_hash,
            },
        )
    }

    /// Rank the candidates held at trust rank for `sova_height` now that its
    /// expectation is known. `rank_of` maps a candidate's withdrawals to its
    /// recovered sealer rank (`usize::MAX` for a valid rewardless block), or
    /// `None` when our zebrad contradicts it — such a candidate keeps no
    /// place in the ranking. Returns the new best when its block changed
    /// (the arbiter should adopt it).
    pub fn rerank(
        &self,
        sova_height: u64,
        rank_of: impl Fn(&[Withdrawal]) -> Option<usize>,
    ) -> Option<Candidate> {
        let pending = self.unranked.lock().ok()?.remove(&sova_height)?;
        let mut best = self.best.lock().ok()?;
        let before = best.get(&sova_height).copied();
        let reranked: Vec<Candidate> = pending
            .iter()
            .filter_map(|(hash, withdrawals)| {
                rank_of(withdrawals).map(|sealer_rank| Candidate {
                    sealer_rank,
                    block_hash: *hash,
                })
            })
            .collect();
        let untouched = before.filter(|b| !pending.iter().any(|(hash, _)| *hash == b.block_hash));
        let new_best = reranked.into_iter().chain(untouched).min_by(prefer)?;
        best.insert(sova_height, new_best);
        (before.map(|b| b.block_hash) != Some(new_best.block_hash)).then_some(new_best)
    }

    /// The current best candidate for an epoch, if any.
    #[must_use]
    pub fn best(&self, sova_height: u64) -> Option<Candidate> {
        self.best.lock().ok()?.get(&sova_height).copied()
    }

    /// Drop candidates above `sova_height` (Zcash reorg unwinding).
    pub fn unwind_above(&self, sova_height: u64) {
        if let Ok(mut best) = self.best.lock() {
            best.retain(|&h, _| h <= sova_height);
        }
        if let Ok(mut unranked) = self.unranked.lock() {
            unranked.retain(|&h, _| h <= sova_height);
        }
    }
}

/// Process-global candidate tracker (see module docs for why).
pub fn global() -> &'static CandidateTracker {
    static GLOBAL: OnceLock<CandidateTracker> = OnceLock::new();
    GLOBAL.get_or_init(CandidateTracker::default)
}

/// A newly-preferred candidate the FCU arbiter should adopt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BestCandidate {
    /// The candidate's Sova height.
    pub sova_height: u64,
    /// The candidate block's hash.
    pub block_hash: [u8; 32],
}

static ARBITER: OnceLock<tokio::sync::mpsc::UnboundedSender<BestCandidate>> = OnceLock::new();

/// Install the arbiter channel and return its receiver. Only the first
/// call installs; later calls return `None` (one arbiter per process).
/// Modes that never arbitrate (dev) simply never call this, and
/// [`notify_best`] stays a no-op.
pub fn install_arbiter() -> Option<tokio::sync::mpsc::UnboundedReceiver<BestCandidate>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    ARBITER.set(tx).ok().map(|()| rx)
}

/// Notify the arbiter of a new best candidate (no-op when no arbiter is
/// installed, or after it stopped).
pub fn notify_best(best: BestCandidate) {
    if let Some(tx) = ARBITER.get()
        && tx.send(best).is_err()
    {
        tracing::debug!(
            height = best.sova_height,
            "arbiter gone; best-candidate dropped"
        );
    }
}

/// The FCU arbiter: adopts each [`BestCandidate`] that is not behind the
/// local head by issuing a forkchoice update for its hash.
///
/// Engine-agnostic on purpose (same pattern as `run_sealer`'s closures):
/// `head_height` reads the local Sova head and `fcu` performs the actual
/// `engine_forkchoiceUpdated` with the caller's handle. A best candidate
/// strictly below the head is stale — its epoch already produced a
/// canonical block we've built past — and is skipped, which is what
/// bounds v2 micro-reorgs to the epoch.
pub async fn run_arbiter<H, W, F, Fut>(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<BestCandidate>,
    head_height: H,
    watermark: W,
    fcu: F,
) where
    H: Fn() -> u64 + Send,
    W: Fn() -> Option<u64> + Send,
    F: Fn(BestCandidate) -> Fut + Send,
    Fut: std::future::Future<Output = Result<(), String>> + Send,
{
    while let Some(best) = rx.recv().await {
        let head = head_height();
        if best.sova_height < head {
            tracing::debug!(
                height = best.sova_height,
                head,
                "stale best-candidate; epoch already built past"
            );
            continue;
        }
        // A candidate well above our own Zcash scan means we are behind,
        // not at the tip: adopting it would make reth download its missing
        // ancestors before our scan covers them (the C5 tip deferral would
        // then apply to history). Hand it to the scan-gated sync driver.
        if let Some(scanned) = watermark()
            && best.sova_height > scanned.saturating_add(TIP_SLACK)
        {
            tracing::info!(
                height = best.sova_height,
                scanned,
                "candidate beyond our scan; catch-up goes through the sync driver"
            );
            request_sync(SyncTarget {
                sova_height: best.sova_height,
                block_hash: best.block_hash,
            });
            continue;
        }
        match fcu(best).await {
            Ok(()) => tracing::info!(
                height = best.sova_height,
                hash = %alloy_primitives::hex::encode(best.block_hash),
                "arbiter adopted preferred candidate"
            ),
            Err(err) => tracing::warn!(
                height = best.sova_height,
                %err,
                "arbiter forkchoice update failed"
            ),
        }
    }
}

/// A chain tip a peer offered that is too far ahead for `sova/1`'s bounded
/// ancestor chase: the catch-up target for [`run_sync_driver`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncTarget {
    /// The target's Sova height.
    pub sova_height: u64,
    /// The target block's hash.
    pub block_hash: [u8; 32],
}

static SYNC: OnceLock<tokio::sync::watch::Sender<Option<SyncTarget>>> = OnceLock::new();

/// Install the sync-driver channel and return its receiver (first call
/// only). Nodes that never catch up (no arbiter) never call this and
/// [`request_sync`] stays a no-op.
pub fn install_sync() -> Option<tokio::sync::watch::Receiver<Option<SyncTarget>>> {
    let (tx, rx) = tokio::sync::watch::channel(None);
    SYNC.set(tx).ok().map(|()| rx)
}

/// Offer a catch-up target; kept only if higher than the current one.
pub fn request_sync(target: SyncTarget) {
    if let Some(tx) = SYNC.get() {
        tx.send_if_modified(|current| keep_higher(current, target));
    }
}

/// Replace `current` with `target` iff it is higher; returns whether it did.
fn keep_higher(current: &mut Option<SyncTarget>, target: SyncTarget) -> bool {
    match current {
        Some(existing) if existing.sova_height >= target.sova_height => false,
        _ => {
            *current = Some(target);
            true
        }
    }
}

/// How far above our scanned watermark the arbiter still adopts directly:
/// tip blocks legitimately outrun our zebrad by an epoch or so.
const TIP_SLACK: u64 = 2;

/// How often a pending catch-up re-checks its gate and re-asserts its
/// forkchoice while the engine downloads or backfills.
const SYNC_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// Late-join catch-up (docs/design/p2p-m1.md, Decision 2). Waits for a
/// [`SyncTarget`], then — **only once our own Zcash scan covers the
/// target's height** (`watermark`, `None` when the node enforces no C5) —
/// issues a forkchoice update toward it so the engine downloads (≤ 32
/// blocks) or backfills (more) the missing history. Gating on the scan is
/// what makes every synced block meet an *enforced* settlement check in
/// `SovaConsensus` instead of the tip-deferral. Re-asserts until our head
/// reaches the target; a newer, higher target replaces the old one.
///
/// Like the arbiter this only moves the head toward a block the network
/// offered; it never builds, and it never finalizes (`fcu` receives just
/// the target).
pub async fn run_sync_driver<H, W, F, Fut>(
    mut rx: tokio::sync::watch::Receiver<Option<SyncTarget>>,
    head_height: H,
    watermark: W,
    fcu: F,
) where
    H: Fn() -> u64 + Send,
    W: Fn() -> Option<u64> + Send,
    F: Fn(SyncTarget) -> Fut + Send,
    Fut: std::future::Future<Output = Result<(), String>> + Send,
{
    loop {
        let Some(target) = *rx.borrow_and_update() else {
            if rx.changed().await.is_err() {
                return;
            }
            continue;
        };
        let mut waiting_logged = false;
        while head_height() < target.sova_height {
            if rx.has_changed().unwrap_or(false) {
                break; // a higher target arrived
            }
            match watermark() {
                Some(scanned) if scanned < target.sova_height => {
                    if !waiting_logged {
                        tracing::info!(
                            target = target.sova_height,
                            scanned,
                            "catch-up waiting for our zebrad scan to reach the target"
                        );
                        waiting_logged = true;
                    }
                }
                _ => match fcu(target).await {
                    Ok(()) => {}
                    Err(status) => tracing::debug!(
                        target = target.sova_height,
                        %status,
                        "catch-up forkchoice pending (engine downloading/backfilling)"
                    ),
                },
            }
            tokio::time::sleep(SYNC_POLL).await;
        }
        if head_height() >= target.sova_height {
            tracing::info!(height = target.sova_height, "caught up to sync target");
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(rank: usize, hash_byte: u8) -> Candidate {
        Candidate {
            sealer_rank: rank,
            block_hash: [hash_byte; 32],
        }
    }

    #[test]
    fn first_candidate_is_best() {
        let t = CandidateTracker::default();
        assert_eq!(t.observe(7, cand(3, 0xAA)), Observation::NewBest);
        assert_eq!(t.best(7), Some(cand(3, 0xAA)));
    }

    #[test]
    fn better_rank_displaces_regardless_of_order() {
        let t = CandidateTracker::default();
        let _ = t.observe(7, cand(2, 0xFF));
        // Late rank-0 wins (rank beats timing).
        assert_eq!(t.observe(7, cand(0, 0xEE)), Observation::NewBest);
        assert_eq!(t.best(7), Some(cand(0, 0xEE)));
        // A worse rank after that is ignored.
        assert_eq!(t.observe(7, cand(1, 0x00)), Observation::NotBetter);
    }

    #[test]
    fn equivocation_tie_breaks_by_hash_and_duplicates_are_not_better() {
        let t = CandidateTracker::default();
        let _ = t.observe(7, cand(1, 0x50));
        assert_eq!(t.observe(7, cand(1, 0x40)), Observation::NewBest);
        assert_eq!(t.observe(7, cand(1, 0x40)), Observation::NotBetter);
        assert_eq!(t.observe(7, cand(1, 0x60)), Observation::NotBetter);
        assert_eq!(t.best(7), Some(cand(1, 0x40)));
    }

    #[tokio::test]
    async fn arbiter_skips_stale_attempts_the_rest_and_survives_fcu_errors() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        for (h, b) in [(3u64, 3u8), (5, 5), (6, 6)] {
            tx.send(BestCandidate {
                sova_height: h,
                block_hash: [b; 32],
            })
            .unwrap_or_else(|err| panic!("{err}"));
        }
        drop(tx);

        let attempted = std::sync::Arc::new(Mutex::new(Vec::new()));
        let sink = attempted.clone();
        // Head is 5: height 3 is stale, 5 and 6 get FCU attempts, and the
        // deliberate error on 5 must not stop 6 from being attempted.
        run_arbiter(
            rx,
            || 5,
            || None,
            move |best: BestCandidate| {
                let sink = sink.clone();
                async move {
                    if let Ok(mut seen) = sink.lock() {
                        seen.push(best.block_hash);
                    }
                    if best.block_hash == [5; 32] {
                        Err("engine says no".to_owned())
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .await;

        let seen = attempted.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(seen.as_slice(), &[[5u8; 32], [6; 32]]);
    }

    fn w(byte: u8) -> Vec<Withdrawal> {
        vec![Withdrawal {
            index: 0,
            validator_index: 0,
            address: alloy_primitives::Address::with_last_byte(byte),
            amount: 1,
        }]
    }

    /// The slow-runner failure: our own block is imported before our scan
    /// reaches its epoch. Once ranked it must read as rank 0 — not as a
    /// trust-rank block our own sealer should try to beat — and the best
    /// block itself does not change (no arbiter churn).
    #[test]
    fn own_block_imported_before_scan_is_reranked_in_place() {
        let t = CandidateTracker::default();
        assert_eq!(
            t.observe_unranked(7, [0xAA; 32], w(1)),
            Observation::NewBest
        );
        assert_eq!(t.best(7), Some(cand(usize::MAX, 0xAA)));
        assert_eq!(t.rerank(7, |_| Some(0)), None, "same block stays best");
        assert_eq!(t.best(7), Some(cand(0, 0xAA)));
        assert_eq!(t.rerank(7, |_| Some(0)), None, "nothing left to rank");
    }

    #[test]
    fn rerank_promotes_the_truly_better_candidate() {
        let t = CandidateTracker::default();
        let _ = t.observe_unranked(7, [0x10; 32], w(1)); // hash-tiebreak winner at trust rank
        let _ = t.observe_unranked(7, [0x90; 32], w(2));
        assert_eq!(t.best(7), Some(cand(usize::MAX, 0x10)));
        // Scan says 0x90 is rank 0, 0x10 rank 1: the best flips.
        let rank_of = |wd: &[Withdrawal]| Some(if wd == w(2).as_slice() { 0 } else { 1 });
        assert_eq!(t.rerank(7, rank_of), Some(cand(0, 0x90)));
        assert_eq!(t.best(7), Some(cand(0, 0x90)));
    }

    #[test]
    fn contradicted_candidates_lose_their_place_and_known_ones_stay() {
        let t = CandidateTracker::default();
        let _ = t.observe(7, cand(1, 0x50)); // already ranked when seen
        let _ = t.observe_unranked(7, [0x40; 32], w(9));
        // Our zebrad contradicts 0x40 (it never led: trust rank loses to
        // rank 1): nothing changes, so no adoption is signalled.
        assert_eq!(t.rerank(7, |_| None), None);
        assert_eq!(t.best(7), Some(cand(1, 0x50)));
    }

    /// Late join: the driver must not FCU toward a target our zebrad scan
    /// hasn't covered (that history would only meet the tip deferral),
    /// then catch up as soon as it is covered.
    #[tokio::test(start_paused = true)]
    async fn sync_driver_waits_for_the_scan_then_catches_up() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};
        let head = Arc::new(AtomicU64::new(3));
        let scanned = Arc::new(AtomicU64::new(5));
        let fcus = Arc::new(Mutex::new(Vec::<u64>::new()));
        let (tx, rx) = tokio::sync::watch::channel(None);
        let (h, s, f) = (head.clone(), scanned.clone(), fcus.clone());
        let driver = tokio::spawn(run_sync_driver(
            rx,
            move || h.load(Ordering::SeqCst),
            move || Some(s.load(Ordering::SeqCst)),
            move |t: SyncTarget| {
                let (f, head) = (f.clone(), head.clone());
                async move {
                    if let Ok(mut calls) = f.lock() {
                        calls.push(t.sova_height);
                    }
                    head.store(t.sova_height, Ordering::SeqCst); // engine synced
                    Ok(())
                }
            },
        ));
        let _ = tx.send(Some(SyncTarget {
            sova_height: 10,
            block_hash: [1; 32],
        }));
        tokio::time::sleep(SYNC_POLL * 4).await;
        assert!(
            fcus.lock().map(|c| c.is_empty()).unwrap_or(false),
            "no FCU while the scan (5) is behind the target (10)"
        );
        scanned.store(10, Ordering::SeqCst);
        tokio::time::sleep(SYNC_POLL * 2).await;
        assert_eq!(fcus.lock().map(|c| c.clone()).unwrap_or_default(), vec![10]);
        drop(tx);
        let _ = tokio::time::timeout(SYNC_POLL * 4, driver).await;
    }

    #[test]
    fn sync_target_keeps_only_the_highest() {
        let t = |h: u64| SyncTarget {
            sova_height: h,
            block_hash: [0; 32],
        };
        let mut cur = None;
        assert!(keep_higher(&mut cur, t(50)));
        assert!(!keep_higher(&mut cur, t(40)));
        assert!(!keep_higher(&mut cur, t(50)));
        assert!(keep_higher(&mut cur, t(60)));
        assert_eq!(cur.map(|x| x.sova_height), Some(60));
    }

    #[test]
    fn epochs_are_independent_and_unwind_drops_above() {
        let t = CandidateTracker::default();
        let _ = t.observe(5, cand(0, 1));
        let _ = t.observe(6, cand(2, 2));
        let _ = t.observe(7, cand(1, 3));
        t.unwind_above(5);
        assert_eq!(t.best(5), Some(cand(0, 1)));
        assert_eq!(t.best(6), None);
        assert_eq!(t.best(7), None);
    }
}
