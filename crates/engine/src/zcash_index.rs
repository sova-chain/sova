//! SIP-4 node-side Zcash index: the store behind the Zcash query
//! precompile (`evm::zcash`).
//!
//! Fed by the **same follower** as the C5 expectations
//! ([`crate::expectations::run_expectations`]), so the mint check and the
//! precompile can never see different Zcash chains, and unwound by the same
//! reorg events. It is deliberately a dumb store — blocks by height,
//! transactions by txid — because every consensus rule (the anchored
//! horizon, status codes, confirmations) lives in the precompile, in one
//! tested place.
//!
//! In memory, rebuilt by rescanning from the epoch base on every start (as
//! the expectations are): consistent by construction, and the follower
//! already fetches every transaction. A persistent store is a mainnet
//! concern (`docs/design/sip4-evm-seam.md`).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use consensus::follower::EpochData;
use evm::zcash::{IndexedTx, ZcashSource, set_zcash_source};

/// An indexed block: hash, header time, and its txids (to unwind them).
#[derive(Debug, Clone)]
struct Block {
    hash: [u8; 32],
    time: u32,
    txids: Vec<[u8; 32]>,
}

#[derive(Debug, Default)]
struct Inner {
    blocks: BTreeMap<u64, Block>,
    txs: HashMap<[u8; 32], Arc<IndexedTx>>,
    /// Highest `h` with every block in `base ..= h` present.
    through: Option<u64>,
}

/// Zcash blocks and transactions from the epoch base up, as this node's
/// follower scanned them.
#[derive(Debug, Default)]
pub struct ZcashIndex {
    base: AtomicU64,
    inner: RwLock<Inner>,
    /// Bumped before any change to already-indexed data (a reorg unwind or
    /// a replacement at an indexed height); see `ZcashSource::generation`.
    generation: AtomicU64,
}

impl ZcashIndex {
    /// An empty index for a network whose epoch base is `base`.
    #[must_use]
    pub fn with_base(base: u64) -> Self {
        Self {
            base: AtomicU64::new(base),
            inner: RwLock::default(),
            generation: AtomicU64::new(0),
        }
    }

    fn base(&self) -> u64 {
        self.base.load(Ordering::SeqCst)
    }

    /// Index one epoch's block (replacing any block already at that
    /// height). The follower emits in order from the base, so coverage
    /// grows contiguously.
    pub fn insert(&self, epoch: &EpochData) {
        let base = self.base();
        // Build the records before taking the write lock, and free what
        // they replace after releasing it: a large Zcash block must not
        // stall the precompile's readers for the length of a deep copy.
        let records: Vec<([u8; 32], Arc<IndexedTx>)> = epoch
            .txs
            .iter()
            .enumerate()
            .map(|(index, tx)| {
                let record = IndexedTx::new(
                    epoch.height,
                    u32::try_from(index).unwrap_or(u32::MAX),
                    tx.version,
                    tx.outputs
                        .iter()
                        .map(|o| (o.value_zat, o.script.clone()))
                        .collect(),
                );
                (tx.txid, Arc::new(record))
            })
            .collect();
        let txids = records.iter().map(|(txid, _)| *txid).collect();
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        if inner.blocks.contains_key(&epoch.height) {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        let mut freed = remove_block(&mut inner, epoch.height);
        for (txid, record) in records {
            freed.extend(inner.txs.insert(txid, record));
        }
        inner.blocks.insert(
            epoch.height,
            Block {
                hash: epoch.hash,
                time: epoch.time,
                txids,
            },
        );
        let mut next = inner.through.map_or(base, |t| t + 1);
        while inner.blocks.contains_key(&next) {
            inner.through = Some(next);
            next += 1;
        }
        drop(inner);
        drop(freed);
    }

    /// Drop everything above Zcash height `height` (a follower
    /// `Rollback { to_height }`).
    pub fn unwind_above(&self, height: u64) {
        let base = self.base();
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        self.generation.fetch_add(1, Ordering::SeqCst);
        let above: Vec<u64> = inner
            .blocks
            .range(height.saturating_add(1)..)
            .map(|(h, _)| *h)
            .collect();
        let mut freed = Vec::new();
        for h in above {
            freed.extend(remove_block(&mut inner, h));
        }
        inner.through = if height < base {
            None
        } else {
            inner.through.map(|t| t.min(height))
        };
        drop(inner);
        drop(freed);
    }
}

/// Remove the block at `height` and the txs it contributed. A txid now
/// recorded at a different height (re-mined after a reorg) is kept.
/// Returns the removed records so the caller can free them after
/// releasing the lock.
fn remove_block(inner: &mut Inner, height: u64) -> Vec<Arc<IndexedTx>> {
    let mut removed = Vec::new();
    if let Some(block) = inner.blocks.remove(&height) {
        for txid in block.txids {
            if inner.txs.get(&txid).is_some_and(|t| t.height == height) {
                removed.extend(inner.txs.remove(&txid));
            }
        }
    }
    removed
}

impl ZcashSource for ZcashIndex {
    fn epoch_base(&self) -> u64 {
        self.base()
    }

    fn indexed_through(&self) -> Option<u64> {
        self.inner.read().ok()?.through
    }

    fn block(&self, zcash_height: u64) -> Option<([u8; 32], u32)> {
        let inner = self.inner.read().ok()?;
        inner.blocks.get(&zcash_height).map(|b| (b.hash, b.time))
    }

    fn tx(&self, txid: &[u8; 32]) -> Option<Arc<IndexedTx>> {
        self.inner.read().ok()?.txs.get(txid).cloned()
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

/// The process-wide index (see [`crate::expectations::global`] for why
/// globals).
pub fn global() -> &'static ZcashIndex {
    static GLOBAL: OnceLock<ZcashIndex> = OnceLock::new();
    GLOBAL.get_or_init(ZcashIndex::default)
}

/// [`ZcashSource`] handle over [`global`].
#[derive(Debug, Clone, Copy)]
struct GlobalIndex;

impl ZcashSource for GlobalIndex {
    fn epoch_base(&self) -> u64 {
        global().epoch_base()
    }
    fn indexed_through(&self) -> Option<u64> {
        global().indexed_through()
    }
    fn block(&self, zcash_height: u64) -> Option<([u8; 32], u32)> {
        global().block(zcash_height)
    }
    fn tx(&self, txid: &[u8; 32]) -> Option<Arc<IndexedTx>> {
        global().tx(txid)
    }
    fn generation(&self) -> u64 {
        global().generation()
    }
}

/// Set the epoch base and install the global index as the precompile's
/// source. Call once at startup, before the node launches (bin/sova
/// does). Returns `false` if a source was already installed.
pub fn install(base: u64) -> bool {
    global().base.store(base, Ordering::SeqCst);
    set_zcash_source(Arc::new(GlobalIndex))
}

#[cfg(test)]
mod tests {
    use consensus::follower::{TxOut, TxView};

    use super::*;

    const BASE: u64 = 100;

    fn tx(tag: u8) -> TxView {
        TxView {
            txid: [tag; 32],
            version: 5,
            outputs: vec![TxOut {
                value_zat: u64::from(tag),
                script: vec![tag],
            }],
        }
    }

    fn epoch(height: u64, hash: u8, txs: Vec<TxView>) -> EpochData {
        EpochData {
            height,
            hash: [hash; 32],
            burns: Vec::new(),
            time: 1_000 + height as u32,
            txs,
        }
    }

    #[test]
    fn coverage_is_contiguous_from_the_base() {
        let idx = ZcashIndex::with_base(BASE);
        assert_eq!(idx.indexed_through(), None);
        idx.insert(&epoch(BASE, 1, vec![tx(1)]));
        idx.insert(&epoch(BASE + 1, 2, vec![]));
        assert_eq!(idx.indexed_through(), Some(BASE + 1));
        // A gap stops coverage until it is filled.
        idx.insert(&epoch(BASE + 3, 4, vec![]));
        assert_eq!(idx.indexed_through(), Some(BASE + 1));
        idx.insert(&epoch(BASE + 2, 3, vec![]));
        assert_eq!(idx.indexed_through(), Some(BASE + 3));
    }

    #[test]
    fn txs_record_height_position_version_and_outputs() {
        let idx = ZcashIndex::with_base(BASE);
        idx.insert(&epoch(BASE, 1, vec![tx(0xc0), tx(7)]));
        let t = idx.tx(&[7; 32]).unwrap_or_else(|| panic!("indexed"));
        assert_eq!((t.height, t.index, t.version), (BASE, 1, 5));
        assert_eq!(t.outputs, vec![(7, vec![7])]);
        assert_eq!(idx.block(BASE), Some(([1; 32], 1_000 + BASE as u32)));
    }

    /// A Zcash reorg: the unwound block's txs vanish, coverage drops, and
    /// a tx re-mined on the new branch is found at its new height. The
    /// result equals an index built fresh from the new branch.
    #[test]
    fn unwind_then_replay_equals_a_fresh_index() {
        let idx = ZcashIndex::with_base(BASE);
        idx.insert(&epoch(BASE, 1, vec![tx(1)]));
        idx.insert(&epoch(BASE + 1, 2, vec![tx(2), tx(3)]));
        idx.unwind_above(BASE);
        assert_eq!(idx.indexed_through(), Some(BASE));
        assert!(idx.tx(&[2; 32]).is_none());
        idx.insert(&epoch(BASE + 1, 9, vec![tx(3)]));
        idx.insert(&epoch(BASE + 2, 8, vec![tx(2)]));

        let fresh = ZcashIndex::with_base(BASE);
        fresh.insert(&epoch(BASE, 1, vec![tx(1)]));
        fresh.insert(&epoch(BASE + 1, 9, vec![tx(3)]));
        fresh.insert(&epoch(BASE + 2, 8, vec![tx(2)]));
        for h in BASE..=BASE + 2 {
            assert_eq!(idx.block(h), fresh.block(h));
        }
        for t in [1u8, 2, 3] {
            assert_eq!(idx.tx(&[t; 32]), fresh.tx(&[t; 32]));
        }
        assert_eq!(idx.indexed_through(), fresh.indexed_through());
        assert_eq!(idx.tx(&[2; 32]).map(|t| t.height), Some(BASE + 2));
    }

    #[test]
    fn deep_unwind_below_the_base_empties_coverage() {
        let idx = ZcashIndex::with_base(BASE);
        idx.insert(&epoch(BASE, 1, vec![tx(1)]));
        idx.unwind_above(BASE - 1);
        assert_eq!(idx.indexed_through(), None);
        assert!(idx.tx(&[1; 32]).is_none());
    }

    #[test]
    fn generation_moves_only_when_indexed_data_changes() {
        let idx = ZcashIndex::with_base(BASE);
        idx.insert(&epoch(BASE, 1, vec![tx(1)]));
        idx.insert(&epoch(BASE + 1, 2, vec![]));
        let g = idx.generation();
        idx.insert(&epoch(BASE + 2, 3, vec![]));
        assert_eq!(idx.generation(), g, "appending is not a change");
        idx.unwind_above(BASE + 1);
        assert!(idx.generation() > g, "a reorg unwind is");
        let g = idx.generation();
        idx.insert(&epoch(BASE + 1, 9, vec![]));
        assert!(idx.generation() > g, "replacing an indexed height is");
    }
}
