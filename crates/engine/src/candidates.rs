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
//!   blocks observed at rank `usize::MAX`). If a producer stacks its next
//!   block before the preferred empty arrives, the branch rule
//!   ([`CandidateTracker`]) still moves it across, up to
//!   [`MAX_REPLACE_DEPTH`] blocks deep; a split deeper than that outlives
//!   the epoch until a cross-history rule ships (audit F2, SIP-8).

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

/// How far below a candidate the attachment walk looks for the point
/// where its branch meets our canonical chain.
const MAX_ATTACH_DEPTH: u64 = 64;

/// How many of this node's canonical blocks a preferred branch may replace
/// (audit 2026-09-23, F1 follow-up). One is the late win at the tip; a
/// little more lets two nodes that built on different siblings (a
/// burn-less epoch sealed by both, a late rank-0 block) converge again.
/// A branch forking deeper than this is not a candidate, whatever its rank.
pub const MAX_REPLACE_DEPTH: u64 = 3;

/// Blocks below the head reported as `safe`: the branch rule never replaces
/// a block with this many blocks built on it.
pub const SAFE_DEPTH: u64 = MAX_REPLACE_DEPTH;

/// Blocks below the head reported as `finalized`. Must exceed how deep
/// Zcash itself can still reorganize — Zebra rolls back at most 99 blocks
/// (`MAX_BLOCK_REORG_HEIGHT`) — because reth refuses any head below the
/// finalized block it was given (`engine::tree` "too deep reorg"), and a
/// Zcash reorg unwinds Sova block for block (SIP-4 §7). At 10 (Rob's
/// 2026-09-23 label) a Zcash reorg deeper than ~9 blocks wedged the node:
/// it could never re-seal on the new branch (zcash-reorg scenario).
pub const FINALIZED_DEPTH: u64 = 100;

/// The canonical-hash lookup, borrowed.
type Reader<'a> = &'a (dyn Fn(u64) -> Option<[u8; 32]> + Send + Sync);

/// Reads the node's canonical block hash at a height (installed by
/// bin/sova over its provider; see [`set_canonical_reader`]).
pub type CanonicalReader = Box<dyn Fn(u64) -> Option<[u8; 32]> + Send + Sync>;

static CANONICAL: OnceLock<CanonicalReader> = OnceLock::new();

/// Our own canonical block at a height, ranked from what the node stores
/// (audit F2, measure A; `docs/design/f2-join-and-restart.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalRecord {
    /// The canonical block's hash.
    pub hash: [u8; 32],
    /// Its parent.
    pub parent: [u8; 32],
    /// Its sealer rank, recomputed from its seal (SIP-6) or withdrawals.
    pub rank: usize,
    /// Its seal identity, when sealed.
    pub seal: Option<SealInfo>,
}

/// Ranks this node's canonical block at a height (`None`: no block there,
/// or its epoch isn't scanned yet so its rank is unknown).
pub type CanonicalRanker = Box<dyn Fn(u64) -> Option<CanonicalRecord> + Send + Sync>;

static RANKER: OnceLock<CanonicalRanker> = OnceLock::new();

/// Install the canonical-block ranker (bin/sova, over its provider). After a
/// restart the tracker is empty; without this, our own blocks competed as
/// unobserved (the lowest rank) and any valid branch within
/// [`MAX_REPLACE_DEPTH`] could move a restarted node off a rank-0 tip.
pub fn set_canonical_ranker(ranker: CanonicalRanker) -> bool {
    RANKER.set(ranker).is_ok()
}

/// Install the canonical-hash reader the **sibling rule** needs (audit
/// 2026-09-23 F1). Without it (unit tests, tools) every candidate counts as
/// attached, the pre-rule behaviour.
pub fn set_canonical_reader(reader: CanonicalReader) -> bool {
    CANONICAL.set(reader).is_ok()
}

/// One observed candidate block for an epoch.
#[derive(Debug, Clone, Copy)]
struct Seen {
    candidate: Candidate,
    parent: [u8; 32],
    /// SIP-6: who sealed it, for equivocation evidence (`None` unsealed).
    seal: Option<SealInfo>,
}

/// A sealed candidate's slot and signature identity (SIP-6 §2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealInfo {
    /// The recovered signer.
    pub signer: [u8; 20],
    /// The block's Zcash anchor (`parent_beacon_block_root`).
    pub anchor: [u8; 32],
    /// The hash the seal signs (the header without its signature).
    pub seal_hash: [u8; 32],
}

/// Rank of a signer with equivocation evidence for a slot: below every
/// honest rank, above the null block (SIP-6 §2.6/§2.7).
pub const EQUIVOCATOR_RANK: usize = usize::MAX - 1;

/// `s` as it competes: demoted to [`EQUIVOCATOR_RANK`] when the same
/// signer sealed another block (different `seal_hash`) for the same slot —
/// same height (all of `others`), parent and anchor. A re-seal on a new
/// parent or anchor is a new slot, not evidence.
fn effective(others: &[Seen], s: &Seen) -> Candidate {
    let Some(me) = s.seal else {
        return s.candidate;
    };
    let equivocated = others.iter().any(|o| {
        o.parent == s.parent
            && o.seal.is_some_and(|x| {
                x.signer == me.signer && x.anchor == me.anchor && x.seal_hash != me.seal_hash
            })
    });
    if equivocated {
        Candidate {
            sealer_rank: EQUIVOCATOR_RANK,
            ..s.candidate
        }
    } else {
        s.candidate
    }
}

/// Tracks every candidate per epoch (keyed by Sova height) with its parent.
///
/// **Branch rule** (audit 2026-09-23, F1): a candidate counts only if it is
/// *attached* to this node's chain: its ancestry, through blocks we hold,
/// meets our canonical chain, and where it first leaves it (the fork point)
/// either our chain ends (it extends our head) or its block beats ours by
/// preference ((rank, hash)) with at most [`MAX_REPLACE_DEPTH`] of our
/// blocks above that point. Candidates are then ordered by their blocks at
/// the height where their branches part, so every node holding the same
/// blocks prefers the same branch. Without this rule a preferred block at
/// the tip carried any ancestry with it and reth followed it however deep
/// (free today; the rank-0 sealer's option even after SIP-6); with only a
/// sibling rule (parent must be our block) two nodes that ever held
/// different blocks at one height never converged again.
#[derive(Default)]
pub struct CandidateTracker {
    seen: Mutex<BTreeMap<u64, Vec<Seen>>>,
    /// Per-instance canonical reader (tests); the global one otherwise.
    canonical: Option<CanonicalReader>,
    /// Per-instance canonical ranker (tests); the global one otherwise.
    ranker: Option<CanonicalRanker>,
    /// Candidates observed before our follower had scanned their epoch —
    /// held at trust rank `usize::MAX` — with the withdrawals needed to
    /// rank them once it has ([`CandidateTracker::rerank`]). Without this
    /// a node's *own* block imported ahead of its scan kept the trust rank
    /// forever, and its sealer then saw a "worse-ranked" block at the tip
    /// and tried to win its own epoch back (seen on slow CI runners).
    unranked: Mutex<BTreeMap<u64, Vec<Unranked>>>,
}

impl std::fmt::Debug for CandidateTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandidateTracker").finish_non_exhaustive()
    }
}

impl CandidateTracker {
    /// A tracker that reads canonical hashes from `reader` instead of the
    /// process-global one.
    #[must_use]
    pub fn with_canonical_reader(reader: CanonicalReader) -> Self {
        Self {
            canonical: Some(reader),
            ..Self::default()
        }
    }

    /// The same tracker ranking its canonical blocks with `ranker` (tests).
    #[must_use]
    pub fn with_canonical_ranker(mut self, ranker: CanonicalRanker) -> Self {
        self.ranker = Some(ranker);
        self
    }

    fn ranker(&self) -> Option<&(dyn Fn(u64) -> Option<CanonicalRecord> + Send + Sync)> {
        self.ranker
            .as_deref()
            .or_else(|| RANKER.get().map(|b| b.as_ref()))
    }

    /// Record our own canonical blocks at `height` and the
    /// [`MAX_REPLACE_DEPTH`] heights below, when the tracker has no record
    /// of them (a restart): a competitor must then beat our real block, as
    /// it would have before the restart.
    fn seed_canonical(&self, seen: &mut BTreeMap<u64, Vec<Seen>>, height: u64) {
        let Some(ranker) = self.ranker() else {
            return;
        };
        for h in height.saturating_sub(MAX_REPLACE_DEPTH)..=height {
            let Some(rec) = ranker(h) else {
                continue;
            };
            let entry = seen.entry(h).or_default();
            if !entry.iter().any(|s| s.candidate.block_hash == rec.hash) {
                entry.push(Seen {
                    candidate: Candidate {
                        sealer_rank: rec.rank,
                        block_hash: rec.hash,
                    },
                    parent: rec.parent,
                    seal: rec.seal,
                });
            }
        }
    }

    fn reader(&self) -> Option<&(dyn Fn(u64) -> Option<[u8; 32]> + Send + Sync)> {
        self.canonical
            .as_deref()
            .or_else(|| CANONICAL.get().map(|b| b.as_ref()))
    }

    /// Observe a candidate block for `sova_height` whose parent is
    /// `parent`; returns whether it is now the epoch's best attached
    /// candidate.
    pub fn observe(&self, sova_height: u64, candidate: Candidate, parent: [u8; 32]) -> Observation {
        self.observe_with(sova_height, candidate, parent, None)
    }

    /// [`Self::observe`] for a sealed block (SIP-6): its seal identity is
    /// kept, so a second seal by the same signer for the same slot demotes
    /// both ([`EQUIVOCATOR_RANK`]).
    pub fn observe_sealed(
        &self,
        sova_height: u64,
        candidate: Candidate,
        parent: [u8; 32],
        seal: SealInfo,
    ) -> Observation {
        self.observe_with(sova_height, candidate, parent, Some(seal))
    }

    fn observe_with(
        &self,
        sova_height: u64,
        candidate: Candidate,
        parent: [u8; 32],
        seal: Option<SealInfo>,
    ) -> Observation {
        {
            let Ok(mut seen) = self.seen.lock() else {
                return Observation::NotBetter;
            };
            self.seed_canonical(&mut seen, sova_height);
            let entry = seen.entry(sova_height).or_default();
            match entry
                .iter_mut()
                .find(|s| s.candidate.block_hash == candidate.block_hash)
            {
                // Same block again: keep its (possibly re-ranked) record.
                Some(existing) => {
                    if prefer(&candidate, &existing.candidate) != std::cmp::Ordering::Less {
                        return Observation::NotBetter;
                    }
                    existing.candidate = candidate;
                }
                None => entry.push(Seen {
                    candidate,
                    parent,
                    seal,
                }),
            }
            while seen.len() > RETAIN {
                let Some((&lowest, _)) = seen.first_key_value() else {
                    break;
                };
                seen.remove(&lowest);
            }
        }
        if self.best(sova_height).map(|b| b.block_hash) == Some(candidate.block_hash) {
            Observation::NewBest
        } else {
            Observation::NotBetter
        }
    }

    /// Observe a candidate whose epoch our follower hasn't scanned yet: it
    /// competes at trust rank `usize::MAX` until [`Self::rerank`] runs for
    /// its height.
    pub fn observe_unranked(
        &self,
        sova_height: u64,
        block_hash: [u8; 32],
        parent: [u8; 32],
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
            parent,
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
        let before = self.best(sova_height);
        {
            let mut seen = self.seen.lock().ok()?;
            let entry = seen.entry(sova_height).or_default();
            for (hash, withdrawals) in &pending {
                let rank = rank_of(withdrawals);
                match rank {
                    Some(sealer_rank) => {
                        if let Some(s) = entry.iter_mut().find(|s| s.candidate.block_hash == *hash)
                        {
                            s.candidate.sealer_rank = sealer_rank;
                        }
                    }
                    None => entry.retain(|s| s.candidate.block_hash != *hash),
                }
            }
        }
        let after = self.best(sova_height)?;
        (before.map(|b| b.block_hash) != Some(after.block_hash)).then_some(after)
    }

    /// The best **attached** candidate for an epoch, if any (see the
    /// branch rule on [`CandidateTracker`]).
    #[must_use]
    pub fn best(&self, sova_height: u64) -> Option<Candidate> {
        let seen = self.seen.lock().ok()?;
        let candidates = seen.get(&sova_height)?;
        let Some(reader) = self.reader() else {
            return candidates
                .iter()
                .map(|s| effective(candidates, s))
                .min_by(prefer);
        };
        candidates
            .iter()
            .filter_map(|s| {
                let b = branch(reader, &seen, sova_height, *s)?;
                attached(reader, &seen, &b).then_some(b)
            })
            .min_by(|a, b| order(reader, &seen, a, b))
            .and_then(|b| b.blocks.first().map(|&(_, c)| c))
    }

    /// Forget a candidate our own engine rejected as permanently invalid (an
    /// `Invalid` that is not a hold, `crate::consensus::HOLD_MARKER`). It is
    /// observed before execution, so a block that then fails execution (a
    /// state-root mismatch, say) would otherwise stay preferred and the
    /// arbiter would retry its forkchoice update forever (public testnet
    /// 2026-09-26, height 6225). Returns the epoch's new best when it
    /// changed, for the arbiter to adopt.
    pub fn forget_invalid(&self, sova_height: u64, block_hash: [u8; 32]) -> Option<Candidate> {
        let before = self.best(sova_height);
        if let Ok(mut unranked) = self.unranked.lock()
            && let Some(entry) = unranked.get_mut(&sova_height)
        {
            entry.retain(|(hash, _)| *hash != block_hash);
        }
        if let Ok(mut seen) = self.seen.lock()
            && let Some(entry) = seen.get_mut(&sova_height)
        {
            entry.retain(|s| s.candidate.block_hash != block_hash);
        }
        let after = self.best(sova_height)?;
        (before.map(|b| b.block_hash) != Some(after.block_hash)).then_some(after)
    }

    /// Drop candidates above `sova_height` (Zcash reorg unwinding).
    pub fn unwind_above(&self, sova_height: u64) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.retain(|&h, _| h <= sova_height);
        }
        if let Ok(mut unranked) = self.unranked.lock() {
            unranked.retain(|&h, _| h <= sova_height);
        }
    }
}

/// A candidate's branch: its blocks from the candidate down to the first
/// one whose parent is our canonical block at `base`, top down.
struct Branch {
    blocks: Vec<(u64, Candidate)>,
    base: u64,
}

impl Branch {
    /// The branch's block hash at `height` (ours at or below `base`).
    fn hash_at(&self, reader: Reader<'_>, height: u64) -> Option<[u8; 32]> {
        if height > self.base {
            self.blocks
                .iter()
                .find(|&&(h, _)| h == height)
                .map(|&(_, c)| c.block_hash)
        } else {
            reader(height)
        }
    }

    /// The branch's block at `height` as a candidate, when we observed it.
    fn candidate_at(
        &self,
        reader: Reader<'_>,
        seen: &BTreeMap<u64, Vec<Seen>>,
        height: u64,
    ) -> Option<Candidate> {
        if height > self.base {
            self.blocks
                .iter()
                .find(|&&(h, _)| h == height)
                .map(|&(_, c)| c)
        } else {
            canonical_candidate(reader, seen, height)
        }
    }
}

/// Walk a candidate's ancestry, through blocks we hold, down to our
/// canonical chain. `None` when an ancestor is missing or too deep.
fn branch(
    reader: Reader<'_>,
    seen: &BTreeMap<u64, Vec<Seen>>,
    height: u64,
    tip: Seen,
) -> Option<Branch> {
    let tip_candidate = seen
        .get(&height)
        .map_or(tip.candidate, |others| effective(others, &tip));
    let mut blocks = vec![(height, tip_candidate)];
    let mut parent = tip.parent;
    let mut h = height;
    loop {
        let below = h.checked_sub(1)?;
        if reader(below) == Some(parent) {
            return Some(Branch {
                blocks,
                base: below,
            });
        }
        if blocks.len() as u64 > MAX_ATTACH_DEPTH {
            return None;
        }
        let others = seen.get(&below)?;
        let s = others.iter().find(|s| s.candidate.block_hash == parent)?;
        blocks.push((below, effective(others, s)));
        parent = s.parent;
        h = below;
    }
}

/// Our canonical block at `height` as a candidate, when we observed it.
fn canonical_candidate(
    reader: Reader<'_>,
    seen: &BTreeMap<u64, Vec<Seen>>,
    height: u64,
) -> Option<Candidate> {
    let hash = reader(height)?;
    seen.get(&height)?
        .iter()
        .find(|s| s.candidate.block_hash == hash)
        .map(|s| effective(&seen[&height], s))
}

/// Whether a branch may become our chain: it extends our head, or it is our
/// own block, or its block at the fork point beats ours there and replaces
/// at most [`MAX_REPLACE_DEPTH`] of our blocks. A block of ours we never
/// observed (imported before a restart) counts as the lowest rank, as in
/// [`order`], so a restarted node still follows the late win; that it can
/// be moved by any branch within the depth is audit F2.
fn attached(reader: Reader<'_>, seen: &BTreeMap<u64, Vec<Seen>>, b: &Branch) -> bool {
    let Some(&(fork, low)) = b.blocks.last() else {
        return false;
    };
    match reader(fork) {
        None => true,
        Some(ours) if ours == low.block_hash => true,
        Some(hash) => {
            let ours = canonical_candidate(reader, seen, fork).unwrap_or(Candidate {
                sealer_rank: usize::MAX,
                block_hash: hash,
            });
            reader(fork.saturating_add(MAX_REPLACE_DEPTH)).is_none()
                && prefer(&low, &ours) == std::cmp::Ordering::Less
        }
    }
}

/// Order two branches ending at the same height by their blocks at the
/// height where they part (below it they are the same chain). Siblings part
/// at their own height, so this is plain preference for them.
fn order(
    reader: Reader<'_>,
    seen: &BTreeMap<u64, Vec<Seen>>,
    a: &Branch,
    b: &Branch,
) -> std::cmp::Ordering {
    let (Some(&(top, ca)), Some(&(_, cb))) = (a.blocks.first(), b.blocks.first()) else {
        return std::cmp::Ordering::Equal;
    };
    let floor = a.base.min(b.base);
    let fork = (floor.saturating_add(1)..=top)
        .find(|&h| a.hash_at(reader, h) != b.hash_at(reader, h))
        .unwrap_or(top);
    let unknown = |hash| Candidate {
        sealer_rank: usize::MAX,
        block_hash: hash,
    };
    let at = |br: &Branch, fallback: Candidate| {
        br.candidate_at(reader, seen, fork)
            .unwrap_or_else(|| br.hash_at(reader, fork).map_or(fallback, unknown))
    };
    prefer(&at(a, ca), &at(b, cb))
}

/// Whether `block_hash` would replace our canonical block at `height` (a
/// preferred branch below our head). `false` without a canonical reader.
fn replaces_canonical(height: u64, block_hash: [u8; 32]) -> bool {
    CANONICAL
        .get()
        .is_some_and(|reader| reader(height).is_some_and(|ours| ours != block_hash))
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
/// canonical block we've built past — and is skipped, unless it is a
/// different block from ours there: then it heads a preferred branch the
/// tracker admitted (at most [`MAX_REPLACE_DEPTH`] blocks deep) and is
/// adopted.
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
    // An adoption whose forkchoice update failed (typically SYNCING: the
    // block was held until our Zcash scan reached it, and imported a moment
    // later). The candidate stays preferred, so no new notification comes;
    // retry it until it lands or stops being the one to adopt.
    let mut pending: Option<BestCandidate> = None;
    let mut retry = tokio::time::interval(ADOPT_RETRY);
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let best = tokio::select! {
            msg = rx.recv() => match msg {
                Some(best) => best,
                None => return,
            },
            _ = retry.tick(), if pending.is_some() => {
                let Some(p) = pending.take() else { continue };
                // Only if it is still the preferred candidate for its height.
                if global().best(p.sova_height).map(|c| c.block_hash) != Some(p.block_hash) {
                    continue;
                }
                p
            }
        };
        let head = head_height();
        // Below our head only a preferred branch (the tracker bounds its
        // depth) moves us; anything else there is an epoch already built past.
        if best.sova_height < head && !replaces_canonical(best.sova_height, best.block_hash) {
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
            Ok(()) => {
                tracing::info!(
                    height = best.sova_height,
                    hash = %alloy_primitives::hex::encode(best.block_hash),
                    "arbiter adopted preferred candidate"
                );
                if pending.is_some_and(|p| p.sova_height <= best.sova_height) {
                    pending = None;
                }
                // A child observed before this block became canonical was
                // not attached then (sibling rule); it may be now.
                if let Some(next) = global().best(best.sova_height.saturating_add(1)) {
                    pending = Some(BestCandidate {
                        sova_height: best.sova_height.saturating_add(1),
                        block_hash: next.block_hash,
                    });
                }
            }
            Err(err) => {
                tracing::warn!(
                    height = best.sova_height,
                    %err,
                    "arbiter forkchoice update failed; will retry"
                );
                pending = Some(best);
            }
        }
    }
}

/// How often a failed adoption is retried (see [`run_arbiter`]).
pub const ADOPT_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

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

/// Catch-up targets offered so far, by height (audit 2026-09-23 F3). The
/// driver acts on the highest one our own Zcash scan already covers, so an
/// unsatisfiable target (a bogus `Announce` of an absurd height) can no
/// longer hide the real ones.
static SYNC_TARGETS: Mutex<BTreeMap<u64, [u8; 32]>> = Mutex::new(BTreeMap::new());

/// Targets kept at most; beyond it the **highest** are evicted first, which
/// is where bogus announcements land.
const MAX_SYNC_TARGETS: usize = 64;

fn remember_target(target: SyncTarget) {
    if let Ok(mut targets) = SYNC_TARGETS.lock() {
        targets.insert(target.sova_height, target.block_hash);
        while targets.len() > MAX_SYNC_TARGETS {
            targets.pop_last();
        }
    }
}

/// The highest remembered target above `head` that our scan covers
/// (`scanned = None` means no gate); drops targets at or below `head`.
fn actionable_target(head: u64, scanned: Option<u64>) -> Option<SyncTarget> {
    let mut targets = SYNC_TARGETS.lock().ok()?;
    // Audit F2 measure B: a target contradicting a checkpoint is never
    // worth backfilling towards.
    targets.retain(|&h, hash| h > head && crate::checkpoints::allows(h, hash));
    targets
        .range(..=scanned.unwrap_or(u64::MAX))
        .next_back()
        .map(|(&sova_height, &block_hash)| SyncTarget {
            sova_height,
            block_hash,
        })
}

/// Offer a catch-up target: remembered for the sync driver, which acts on
/// the highest one our own scan covers.
pub fn request_sync(target: SyncTarget) {
    remember_target(target);
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
    let mut waiting_logged = false;
    // The target the last "catching up" line was logged for: that line is
    // logged once per target, not on every re-assertion.
    let mut announced: Option<SyncTarget> = None;
    loop {
        // Targets sent straight into the channel (tests, older callers)
        // join the remembered set.
        if let Some(target) = *rx.borrow_and_update() {
            remember_target(target);
        }
        let head = head_height();
        let scanned = watermark();
        match actionable_target(head, scanned) {
            Some(target) => {
                waiting_logged = false;
                if announced != Some(target) {
                    // Blocks in (from, height] may now be imported by the
                    // engine's own downloader (devp2p `eth`), not through
                    // sova/1's submit path, so they get no "peer block
                    // accepted" line. Logged so a reader of the log (the
                    // p2p sims' transport check) can attribute them.
                    tracing::info!(
                        from = head,
                        height = target.sova_height,
                        hash = %alloy_primitives::hex::encode(target.block_hash),
                        "catching up to sync target (engine download)"
                    );
                    announced = Some(target);
                }
                match fcu(target).await {
                    Ok(()) => {}
                    Err(status) => tracing::debug!(
                        target = target.sova_height,
                        %status,
                        "catch-up forkchoice pending (engine downloading/backfilling)"
                    ),
                }
                if head_height() >= target.sova_height {
                    tracing::info!(height = target.sova_height, "caught up to sync target");
                }
            }
            None => {
                let pending = SYNC_TARGETS.lock().map(|t| t.len()).unwrap_or(0);
                if pending == 0 {
                    // Nothing to do until a new target arrives.
                    if rx.changed().await.is_err() {
                        return;
                    }
                    continue;
                }
                if !waiting_logged {
                    tracing::info!(
                        pending,
                        ?scanned,
                        "catch-up waiting for our zebrad scan to reach a target"
                    );
                    waiting_logged = true;
                }
            }
        }
        tokio::time::sleep(SYNC_POLL).await;
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
        assert_eq!(t.observe(7, cand(3, 0xAA), [0; 32]), Observation::NewBest);
        assert_eq!(t.best(7), Some(cand(3, 0xAA)));
    }

    #[test]
    fn a_permanently_invalid_candidate_is_forgotten() {
        let t = CandidateTracker::default();
        // The invalid block is the preferred one (lower hash at equal rank:
        // the 6225 shape).
        let _ = t.observe(7, cand(0, 0x10), [0; 32]);
        let _ = t.observe(7, cand(0, 0x20), [0; 32]);
        assert_eq!(t.best(7), Some(cand(0, 0x10)));
        // Forgetting it hands the epoch to the next candidate.
        assert_eq!(t.forget_invalid(7, [0x10; 32]), Some(cand(0, 0x20)));
        assert_eq!(t.best(7), Some(cand(0, 0x20)));
        // Forgetting a non-best or unknown block changes nothing.
        assert_eq!(t.forget_invalid(7, [0x99; 32]), None);
        assert_eq!(t.best(7), Some(cand(0, 0x20)));
        // Forgetting the last one leaves no best, so the arbiter's retry
        // (which checks `best`) stops.
        assert_eq!(t.forget_invalid(7, [0x20; 32]), None);
        assert_eq!(t.best(7), None);
        // A re-sealed block for the epoch is then accepted as best.
        assert_eq!(t.observe(7, cand(0, 0x30), [0; 32]), Observation::NewBest);
    }

    #[test]
    fn better_rank_displaces_regardless_of_order() {
        let t = CandidateTracker::default();
        let _ = t.observe(7, cand(2, 0xFF), [0; 32]);
        // Late rank-0 wins (rank beats timing).
        assert_eq!(t.observe(7, cand(0, 0xEE), [0; 32]), Observation::NewBest);
        assert_eq!(t.best(7), Some(cand(0, 0xEE)));
        // A worse rank after that is ignored.
        assert_eq!(t.observe(7, cand(1, 0x00), [0; 32]), Observation::NotBetter);
    }

    #[test]
    fn equivocation_tie_breaks_by_hash_and_duplicates_are_not_better() {
        let t = CandidateTracker::default();
        let _ = t.observe(7, cand(1, 0x50), [0; 32]);
        assert_eq!(t.observe(7, cand(1, 0x40), [0; 32]), Observation::NewBest);
        assert_eq!(t.observe(7, cand(1, 0x40), [0; 32]), Observation::NotBetter);
        assert_eq!(t.observe(7, cand(1, 0x60), [0; 32]), Observation::NotBetter);
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
            t.observe_unranked(7, [0xAA; 32], [0; 32], w(1)),
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
        let _ = t.observe_unranked(7, [0x10; 32], [0; 32], w(1)); // hash-tiebreak winner at trust rank
        let _ = t.observe_unranked(7, [0x90; 32], [0; 32], w(2));
        assert_eq!(t.best(7), Some(cand(usize::MAX, 0x10)));
        // Scan says 0x90 is rank 0, 0x10 rank 1: the best flips.
        let rank_of = |wd: &[Withdrawal]| Some(if wd == w(2).as_slice() { 0 } else { 1 });
        assert_eq!(t.rerank(7, rank_of), Some(cand(0, 0x90)));
        assert_eq!(t.best(7), Some(cand(0, 0x90)));
    }

    #[test]
    fn contradicted_candidates_lose_their_place_and_known_ones_stay() {
        let t = CandidateTracker::default();
        let _ = t.observe(7, cand(1, 0x50), [0; 32]); // already ranked when seen
        let _ = t.observe_unranked(7, [0x40; 32], [0; 32], w(9));
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
        let _ = t.observe(5, cand(0, 1), [0; 32]);
        let _ = t.observe(6, cand(2, 2), [0; 32]);
        let _ = t.observe(7, cand(1, 3), [0; 32]);
        t.unwind_above(5);
        assert_eq!(t.best(5), Some(cand(0, 1)));
        assert_eq!(t.best(6), None);
        assert_eq!(t.best(7), None);
    }

    /// Canonical chain 0..=10 with hash `[h; 32]` at height h.
    fn chain_to_10() -> CandidateTracker {
        CandidateTracker::with_canonical_reader(Box::new(|h| (h <= 10).then_some([h as u8; 32])))
    }

    /// Audit 2026-09-23 F1: a better-ranked block on a foreign parent is not
    /// a candidate; the honest block stays best. A sibling of the tip still
    /// wins by rank (the late win).
    #[test]
    fn sibling_rule_ignores_foreign_ancestry() {
        let t = chain_to_10();
        let _ = t.observe(10, cand(1, 0x10), [9; 32]);
        assert_eq!(
            t.observe(10, cand(0, 0x01), [0xEE; 32]),
            Observation::NotBetter
        );
        assert_eq!(t.best(10).map(|c| c.block_hash), Some([0x10; 32]));
        assert_eq!(t.observe(10, cand(0, 0x02), [9; 32]), Observation::NewBest);
        assert_eq!(t.best(10).map(|c| c.block_hash), Some([0x02; 32]));
    }

    /// Above the head a candidate must chain down to our head through
    /// attached candidates; an unconnected chain is not a candidate.
    #[test]
    fn sibling_rule_attaches_forward_chains_only_through_our_head() {
        let t = chain_to_10();
        let _ = t.observe(11, cand(0, 0x11), [10; 32]);
        let _ = t.observe(12, cand(0, 0x12), [0x11; 32]);
        assert_eq!(t.best(12).map(|c| c.block_hash), Some([0x12; 32]));
        // A chain whose bottom does not extend our head.
        let _ = t.observe(11, cand(0, 0x21), [0xEE; 32]);
        let _ = t.observe(12, cand(0, 0x01), [0x21; 32]);
        assert_eq!(
            t.best(12).map(|c| c.block_hash),
            Some([0x12; 32]),
            "a lower hash on an unattached chain must not win"
        );
    }

    /// Canonical chain 0..=7 with hash `[h; 32]`; heights 6 and 7 observed.
    fn split_at_6() -> CandidateTracker {
        let t = CandidateTracker::with_canonical_reader(Box::new(|h| {
            (h <= 7).then_some([h as u8; 32])
        }));
        let _ = t.observe(6, cand(usize::MAX, 6), [5; 32]);
        let _ = t.observe(7, cand(1, 7), [6; 32]);
        t
    }

    /// Audit F1 follow-up: two nodes that sealed different burn-less blocks
    /// at 6 converge on the preferred one, and the late rank-0 block built
    /// on it takes 7 with it (the ladder scenario's split).
    #[test]
    fn a_preferred_branch_within_depth_replaces_ours() {
        let t = split_at_6();
        assert_eq!(
            t.observe(6, cand(usize::MAX, 0x01), [5; 32]),
            Observation::NewBest
        );
        assert_eq!(
            t.observe(7, cand(0, 0x71), [0x01; 32]),
            Observation::NewBest
        );
        assert_eq!(t.best(7).map(|c| c.block_hash), Some([0x71; 32]));
    }

    /// The order is by the blocks where branches part, not at the tip: a
    /// rank-0 block on the worse branch loses to rank 1 on the better one.
    #[test]
    fn branches_are_ordered_where_they_part() {
        let t = split_at_6();
        let _ = t.observe(6, cand(usize::MAX, 0xF0), [5; 32]); // worse than ours
        assert_eq!(
            t.observe(7, cand(0, 0x71), [0xF0; 32]),
            Observation::NotBetter
        );
        assert_eq!(t.best(7).map(|c| c.block_hash), Some([7; 32]));
        assert_eq!(t.best(6).map(|c| c.block_hash), Some([6; 32]));
    }

    /// Deeper than MAX_REPLACE_DEPTH a branch is not a candidate, however
    /// well ranked; nor is one with an ancestor we do not hold.
    #[test]
    fn deep_or_unconnected_branches_are_ignored() {
        let t = chain_to_10();
        for h in 7..=10 {
            let _ = t.observe(h, cand(1, h as u8), [h as u8 - 1; 32]);
        }
        // Fork at 7 would replace 7..=10: four blocks.
        let _ = t.observe(7, cand(0, 0xA7), [6; 32]);
        let _ = t.observe(8, cand(0, 0xA8), [0xA7; 32]);
        assert_eq!(t.best(8).map(|c| c.block_hash), Some([8; 32]));
        // Fork at 8 replaces 8..=10: three blocks, allowed.
        let _ = t.observe(8, cand(0, 0xB8), [7; 32]);
        assert_eq!(t.best(8).map(|c| c.block_hash), Some([0xB8; 32]));
        // A missing ancestor.
        let _ = t.observe(10, cand(0, 0xC0), [0xEE; 32]);
        assert_eq!(t.best(10).map(|c| c.block_hash), Some([10; 32]));
    }

    fn seal(signer: u8, anchor: u8, seal_hash: u8) -> SealInfo {
        SealInfo {
            signer: [signer; 20],
            anchor: [anchor; 32],
            seal_hash: [seal_hash; 32],
        }
    }

    /// SIP-6 §2.7: rank 0 sealing two blocks for one slot demotes both
    /// below rank 1's block; the ladder's rank 1 then wins everywhere.
    #[test]
    fn an_equivocating_signer_loses_to_the_next_rank() {
        let t = chain_to_10();
        let _ = t.observe_sealed(11, cand(0, 0x01), [10; 32], seal(0xA0, 5, 1));
        let _ = t.observe_sealed(11, cand(1, 0x30), [10; 32], seal(0xB1, 5, 3));
        assert_eq!(
            t.best(11).map(|c| c.block_hash),
            Some([0x01; 32]),
            "honest so far"
        );
        // The same signer, slot and anchor, another seal: evidence.
        let _ = t.observe_sealed(11, cand(0, 0x02), [10; 32], seal(0xA0, 5, 2));
        assert_eq!(t.best(11).map(|c| c.block_hash), Some([0x30; 32]));
        // With nobody else ranked, the equivocator still beats null.
        let t = chain_to_10();
        let _ = t.observe(11, cand(usize::MAX, 0x00), [10; 32]);
        let _ = t.observe_sealed(11, cand(0, 0x05), [10; 32], seal(0xA0, 5, 1));
        let _ = t.observe_sealed(11, cand(0, 0x06), [10; 32], seal(0xA0, 5, 2));
        assert_eq!(t.best(11).map(|c| c.block_hash), Some([0x05; 32]));
        assert_eq!(t.best(11).map(|c| c.sealer_rank), Some(EQUIVOCATOR_RANK));
    }

    /// A re-seal on a new anchor (Zcash reorg) or a new parent (a late-win
    /// reorg) is a new slot, not equivocation.
    #[test]
    fn a_reseal_on_a_new_slot_is_not_evidence() {
        let t = chain_to_10();
        let _ = t.observe_sealed(11, cand(0, 0x01), [10; 32], seal(0xA0, 5, 1));
        let _ = t.observe_sealed(11, cand(0, 0x02), [10; 32], seal(0xA0, 6, 2));
        assert_eq!(t.best(11).map(|c| c.sealer_rank), Some(0));
        let _ = t.observe_sealed(11, cand(0, 0x03), [0xEE; 32], seal(0xA0, 5, 3));
        assert_eq!(t.best(11).map(|c| c.sealer_rank), Some(0));
    }

    /// A tracker over canonical 0..=10 (`[h; 32]`) that, like a restarted
    /// node, has observed nothing but can rank its canonical blocks.
    fn restarted(rank_of: fn(u64) -> usize) -> CandidateTracker {
        chain_to_10().with_canonical_ranker(Box::new(move |h| {
            (h <= 10).then(|| CanonicalRecord {
                hash: [h as u8; 32],
                parent: [h.saturating_sub(1) as u8; 32],
                rank: rank_of(h),
                seal: None,
            })
        }))
    }

    /// Audit F2 measure A: after a restart, a worse sibling doesn't move a
    /// rank-0 tip (it used to: our block competed as unobserved), a better
    /// one still wins (the late win), and a worse branch forking below the
    /// tip stays out.
    #[test]
    fn after_restart_our_own_block_sets_the_bar() {
        let t = restarted(|_| 0);
        assert_eq!(
            t.observe(10, cand(1, 0x01), [9; 32]),
            Observation::NotBetter
        );
        assert_eq!(t.best(10).map(|c| c.block_hash), Some([10; 32]));
        let _ = t.observe(9, cand(1, 0x02), [8; 32]);
        let _ = t.observe(10, cand(0, 0x03), [0x02; 32]);
        assert_eq!(
            t.best(10).map(|c| c.block_hash),
            Some([10; 32]),
            "worse fork at 9"
        );

        let t = restarted(|h| if h == 10 { 2 } else { 0 });
        assert_eq!(
            t.observe(10, cand(0, 0x01), [9; 32]),
            Observation::NewBest,
            "late win"
        );
        assert_eq!(t.best(10).map(|c| c.block_hash), Some([0x01; 32]));
    }

    /// No reader installed (tools, other tests): every candidate counts.
    #[test]
    fn without_a_reader_every_candidate_counts() {
        let t = CandidateTracker::default();
        assert_eq!(
            t.observe(10, cand(0, 0x01), [0xEE; 32]),
            Observation::NewBest
        );
    }
}
