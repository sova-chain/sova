//! SIP-8 node-side vote store: every vote (§2.3) the node's follower has
//! seen, keyed by the Zcash height that carries it.
//!
//! Fed by the **same follower** as the C5 expectations and the SIP-4 index
//! ([`crate::expectations::run_expectations`]) and unwound by the same
//! reorg events, so a Zcash `Rollback` removes exactly the votes of the
//! unwound blocks. Like the index it lives in memory and is rebuilt by the
//! rescan from the epoch base on every start.
//!
//! A dumb store: it records `(E, h, H, w)` and aggregates weight per
//! reference `(h, H)`. Whether a referenced block exists, is valid or wins is
//! fork choice's question (SIP-8 §2.4), not this module's.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use consensus::follower::EpochData;
use consensus::sip1::SovaRef;

static SIP8_FROM: OnceLock<u64> = OnceLock::new();

/// Switch SIP-8 on from Zcash height `from` for this process. Every
/// follower in the node (the expectations' and the sealer's) reads it, so a
/// sealer and its validators always agree on which burns exist. Call once
/// at startup, before the node launches; returns `false` if already set.
pub fn activate_sip8(from: u64) -> bool {
    SIP8_FROM.set(from).is_ok()
}

/// The Zcash height SIP-8 recognition starts at, if it is on.
#[must_use]
pub fn sip8_from() -> Option<u64> {
    SIP8_FROM.get().copied()
}

/// One vote: a v2 burn's reference and its weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vote {
    /// The Sova block voted for.
    pub reference: SovaRef,
    /// The burn's weight in zatoshis.
    pub weight_zat: u64,
}

/// Votes by the Zcash height that carries them.
#[derive(Debug, Default)]
pub struct VoteStore {
    epoch_base: AtomicU64,
    votes: RwLock<BTreeMap<u64, Vec<Vote>>>,
    /// Bumped on every change, so fork choice knows when to recompute.
    generation: AtomicU64,
}

impl VoteStore {
    /// An empty store for a network whose epoch base is `epoch_base`.
    #[must_use]
    pub fn with_base(epoch_base: u64) -> Self {
        Self {
            epoch_base: AtomicU64::new(epoch_base),
            ..Self::default()
        }
    }

    /// Record the votes in one epoch (replacing any at that height). A burn
    /// whose reference is out of range (§2.3) is a burn with no vote.
    pub fn insert(&self, epoch: &EpochData) {
        let base = self.epoch_base.load(Ordering::SeqCst);
        let votes: Vec<Vote> = epoch
            .burns
            .iter()
            .filter_map(|b| {
                let reference = b.reference?;
                reference.votes_at(epoch.height, base).then_some(Vote {
                    reference,
                    weight_zat: b.burn.value_zat,
                })
            })
            .collect();
        let Ok(mut map) = self.votes.write() else {
            return;
        };
        let changed = if votes.is_empty() {
            map.remove(&epoch.height).is_some()
        } else {
            map.insert(epoch.height, votes);
            true
        };
        drop(map);
        if changed {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Drop every vote carried above Zcash height `height` (a follower
    /// `Rollback { to_height }`).
    pub fn unwind_above(&self, height: u64) {
        let Ok(mut map) = self.votes.write() else {
            return;
        };
        let removed = map.split_off(&height.saturating_add(1));
        drop(map);
        if !removed.is_empty() {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Summed weight per reference, over every stored vote.
    ///
    /// Keyed by the whole `(height, hash)`: a block hash commits to its
    /// number, so at most one height per hash can match a real block, and
    /// votes naming a real hash at a wrong height must neither count for it
    /// nor crowd out the votes that name it correctly.
    #[must_use]
    pub fn weights(&self) -> HashMap<SovaRef, u128> {
        let mut out: HashMap<SovaRef, u128> = HashMap::new();
        let Ok(map) = self.votes.read() else {
            return out;
        };
        for vote in map.values().flatten() {
            *out.entry(vote.reference).or_default() += u128::from(vote.weight_zat);
        }
        out
    }

    /// Votes carried by Zcash height `height`, if any.
    #[must_use]
    pub fn at(&self, height: u64) -> Vec<Vote> {
        self.votes
            .read()
            .ok()
            .and_then(|m| m.get(&height).cloned())
            .unwrap_or_default()
    }

    /// Changes so far; see the field.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

/// The process-wide store (see [`crate::expectations::global`] for why
/// globals).
pub fn global() -> &'static VoteStore {
    static GLOBAL: OnceLock<VoteStore> = OnceLock::new();
    GLOBAL.get_or_init(VoteStore::default)
}

/// Set the epoch base of the global store. Call once at startup.
pub fn install(epoch_base: u64) {
    global().epoch_base.store(epoch_base, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use consensus::epoch::EpochBurn;
    use consensus::sip1::Burn;

    use super::*;

    const BASE: u64 = 1_000;

    fn r(height: u32, tag: u8) -> SovaRef {
        SovaRef {
            height,
            hash: [tag; 32],
        }
    }

    fn burn(zat: u64, reference: Option<SovaRef>) -> EpochBurn {
        EpochBurn {
            txid: [zat as u8; 32],
            burn: Burn {
                evm_address: [1; 20],
                signal_bits: 0,
                value_zat: zat,
            },
            reference,
        }
    }

    fn epoch(height: u64, burns: Vec<EpochBurn>) -> EpochData {
        EpochData {
            height,
            hash: [height as u8; 32],
            burns,
            time: 0,
            txs: Vec::new(),
            pools: None,
        }
    }

    #[test]
    fn records_only_votes_in_range() {
        let s = VoteStore::with_base(BASE);
        // E = 1005: heights 1..=5 are votable.
        s.insert(&epoch(
            1_005,
            vec![
                burn(100, Some(r(5, 0xa))),
                burn(200, Some(r(6, 0xb))), // too high: a burn, no vote
                burn(300, Some(r(0, 0xc))), // genesis: no vote
                burn(400, None),            // v1 burn
            ],
        ));
        assert_eq!(
            s.at(1_005),
            vec![Vote {
                reference: r(5, 0xa),
                weight_zat: 100
            }]
        );
    }

    #[test]
    fn weights_aggregate_by_reference() {
        let s = VoteStore::with_base(BASE);
        // An early vote naming hash 0xa at the wrong height must not stop
        // the correct votes for (4, 0xa) from counting.
        s.insert(&epoch(1_004, vec![burn(999, Some(r(2, 0xa)))]));
        s.insert(&epoch(
            1_005,
            vec![burn(100, Some(r(4, 0xa))), burn(50, Some(r(3, 0xb)))],
        ));
        s.insert(&epoch(1_006, vec![burn(25, Some(r(4, 0xa)))]));
        let w = s.weights();
        assert_eq!(w[&r(4, 0xa)], 125);
        assert_eq!(w[&r(2, 0xa)], 999);
        assert_eq!(w[&r(3, 0xb)], 50);
    }

    /// SIP-8 §10: the store after a rollback and rescan equals a fresh one.
    #[test]
    fn rollback_then_rescan_equals_fresh() {
        let a = epoch(1_005, vec![burn(100, Some(r(4, 0xa)))]);
        let b = epoch(1_006, vec![burn(70, Some(r(5, 0xb)))]);
        let b2 = epoch(1_006, vec![burn(90, Some(r(5, 0xc)))]);
        let c2 = epoch(1_007, vec![burn(10, Some(r(6, 0xd)))]);

        let replayed = VoteStore::with_base(BASE);
        for e in [&a, &b] {
            replayed.insert(e);
        }
        let g = replayed.generation();
        replayed.unwind_above(1_005);
        assert!(replayed.generation() > g);
        for e in [&b2, &c2] {
            replayed.insert(e);
        }

        let fresh = VoteStore::with_base(BASE);
        for e in [&a, &b2, &c2] {
            fresh.insert(e);
        }
        assert_eq!(replayed.weights(), fresh.weights());
        for h in 1_005..=1_007 {
            assert_eq!(replayed.at(h), fresh.at(h));
        }
    }

    #[test]
    fn an_epoch_without_votes_changes_nothing() {
        let s = VoteStore::with_base(BASE);
        let g = s.generation();
        s.insert(&epoch(1_005, vec![burn(100, None)]));
        s.unwind_above(2_000);
        assert_eq!(s.generation(), g);
    }
}
