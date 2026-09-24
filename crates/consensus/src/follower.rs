//! The Zcash follower: turns a live Zcash chain view into an ordered,
//! reorg-aware stream of Sova epochs.
//!
//! The follower is deliberately split from I/O: it consumes any
//! [`ZcashView`] (the [`crate::zebrad::ZebradClient`] in production, a
//! mock in tests) and emits [`FollowerEvent`]s from [`Follower::poll`]:
//!
//! - [`FollowerEvent::Epoch`] — one per newly-final-enough Zcash block,
//!   carrying every SIP-1 burn recognized in it (possibly none).
//! - [`FollowerEvent::Rollback`] — the previously-emitted chain above
//!   `to_height` is no longer canonical; the consumer must unwind to
//!   `to_height` before applying the epochs that follow.
//!
//! Reorg handling: the follower keeps a bounded window of `(height,
//! hash)` pairs it has emitted. Each poll first re-validates the window
//! top against the view; mismatching entries pop until a match (or the
//! window empties), producing a single `Rollback` to the deepest still-
//! canonical height, then scanning resumes. A reorg deeper than the
//! window unwinds to the follower's base height — correct, just
//! expensive, and bounded by configuration.
//!
//! Determinism: two followers over the same chain view emit identical
//! epoch sequences; a rescan after rollback equals a fresh scan. This is
//! what makes the epoch stream usable as consensus input.
//!
//! Hash/txid convention: all hashes and txids are stored as the 32-byte
//! decoding of the RPC display hex (display order). Comparisons and the
//! [`crate::epoch`] tie-breaks use these bytes; nothing here ever
//! reinterprets wire order.

use std::collections::VecDeque;

use crate::sip1::{self, TxOutRef};

/// One transparent output as raw consensus input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxOut {
    /// Value in zatoshis.
    pub value_zat: u64,
    /// Raw lock script bytes.
    pub script: Vec<u8>,
}

/// One transaction in a block view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxView {
    /// Txid, display-order bytes.
    pub txid: [u8; 32],
    /// Transaction format version (SIP-4 `txInfo`).
    pub version: u32,
    /// Transparent outputs in order.
    pub outputs: Vec<TxOut>,
    /// SIP-7: transparent input count and shielded flows.
    pub shielded: crate::pools::TxShielded,
}

/// One block as the follower consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockView {
    /// Block height.
    pub height: u64,
    /// Block hash, display-order bytes.
    pub hash: [u8; 32],
    /// Parent block hash, display-order bytes.
    pub prev_hash: [u8; 32],
    /// Header time (SIP-4 `blockAt`).
    pub time: u32,
    /// Transactions, block order (coinbase included — the burn rule is
    /// total over every transaction).
    pub txs: Vec<TxView>,
    /// SIP-7: the block's value pools (`None` if zebrad omitted them).
    pub pools: Option<Box<crate::pools::BlockPools>>,
}

/// Errors a chain view can produce. Transport-level only: "no block at
/// that height" is `Ok(None)`, not an error.
#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    /// The view's backend could not be reached or answered garbage.
    #[error("zcash view error: {0}")]
    Backend(String),
}

/// A read-only view of a Zcash chain.
pub trait ZcashView {
    /// Current tip height.
    fn tip_height(&self) -> Result<u64, ViewError>;
    /// The canonical block at `height`, or `None` if the chain has no
    /// block there (beyond tip, or racing a reorg).
    fn block_at(&self, height: u64) -> Result<Option<BlockView>, ViewError>;
}

/// A recognized burn plus its txid — the epoch ingredient.
pub use crate::epoch::EpochBurn;

/// One epoch as emitted by the follower.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochData {
    /// Zcash height = epoch number.
    pub height: u64,
    /// The epoch block's hash (display-order bytes).
    pub hash: [u8; 32],
    /// Every SIP-1 burn recognized in the block, block order.
    pub burns: Vec<EpochBurn>,
    /// Header time.
    pub time: u32,
    /// Every transaction, block order (coinbase first). Feeds the SIP-4
    /// Zcash index; consumers that only need burns may drop it.
    pub txs: Vec<TxView>,
    /// SIP-7: the block's value pools (`None` if zebrad omitted them).
    pub pools: Option<Box<crate::pools::BlockPools>>,
}

/// Events from [`Follower::poll`], in application order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowerEvent {
    /// Unwind all previously-applied epochs above `to_height`.
    Rollback {
        /// Deepest still-canonical height; epochs above it are void.
        to_height: u64,
    },
    /// Apply this epoch.
    Epoch(EpochData),
}

/// Reorg-aware epoch scanner over a [`ZcashView`].
#[derive(Debug)]
pub struct Follower {
    /// Emitted `(height, hash)` pairs, oldest first, bounded.
    window: VecDeque<(u64, [u8; 32])>,
    /// Next height to scan.
    next_height: u64,
    /// First height this follower covers (genesis of interest).
    base_height: u64,
    /// Window capacity; reorgs deeper than this unwind to base.
    window_cap: usize,
    /// SIP-7: refuse to emit a block whose value pools are missing or fail
    /// [`crate::pools::check_pools`] — the follower stops before it and
    /// retries next poll (a hold, never a skip).
    strict_pools: bool,
    /// The last emitted block's pools, for the continuity check (`None`
    /// after a rollback: continuity is skipped once).
    last_pools: Option<(u64, crate::pools::BlockPools)>,
    /// Why the last poll stopped short under `strict_pools`, if it did.
    held: Option<String>,
    /// SIP-8 activation: the first Zcash height at which version-2 burns
    /// are recognized (`None`: never, SIP-1 alone).
    sip8_from: Option<u64>,
}

impl Follower {
    /// A follower that scans from `base_height` (inclusive), retaining a
    /// reorg window of `window_cap` blocks. `window_cap` must be >= 1.
    #[must_use]
    pub fn new(base_height: u64, window_cap: usize) -> Self {
        Self {
            window: VecDeque::new(),
            next_height: base_height,
            base_height,
            window_cap: window_cap.max(1),
            strict_pools: false,
            last_pools: None,
            held: None,
            sip8_from: None,
        }
    }

    /// SIP-8: recognize version-2 burns from Zcash height `from` on. Below
    /// it SIP-1 alone applies, forever, so history replays the same way.
    #[must_use]
    pub const fn with_sip8_from(mut self, from: Option<u64>) -> Self {
        self.sip8_from = from;
        self
    }

    /// SIP-7: hold on missing or inconsistent value pools (see
    /// [`crate::pools::check_pools`]).
    #[must_use]
    pub const fn with_strict_pools(mut self, strict: bool) -> Self {
        self.strict_pools = strict;
        self
    }

    /// Why the last poll stopped before the tip under strict pools.
    #[must_use]
    pub fn hold_reason(&self) -> Option<&str> {
        self.held.as_deref()
    }

    /// Poll the view once: detect reorgs, then scan forward to the tip.
    ///
    /// Returns events in application order (a `Rollback` always precedes
    /// the epochs replacing the unwound range). An unchanged chain
    /// returns an empty vec.
    pub fn poll<V: ZcashView>(&mut self, view: &V) -> Result<Vec<FollowerEvent>, ViewError> {
        let mut events = Vec::new();

        // 1. Re-validate the window against the current chain, popping
        //    entries the chain no longer agrees with.
        let mut rolled_back = false;
        while let Some(&(h, stored_hash)) = self.window.back() {
            let current = view.block_at(h)?;
            match current {
                Some(ref b) if b.hash == stored_hash => break,
                _ => {
                    self.window.pop_back();
                    rolled_back = true;
                }
            }
        }
        if rolled_back {
            let to_height = self
                .window
                .back()
                .map_or(self.base_height.saturating_sub(1), |&(h, _)| h);
            self.next_height = to_height.saturating_add(1).max(self.base_height);
            self.last_pools = None;
            events.push(FollowerEvent::Rollback { to_height });
        }

        // 2. Scan forward to the tip.
        self.held = None;
        let tip = view.tip_height()?;
        while self.next_height <= tip {
            let Some(block) = view.block_at(self.next_height)? else {
                // Chain moved under us (concurrent reorg); next poll
                // resolves it.
                break;
            };
            // Parent linkage check: a mismatch means the chain changed
            // between our rollback pass and now — stop, next poll fixes.
            if let Some(&(_, parent_hash)) = self.window.back()
                && block.prev_hash != parent_hash
            {
                break;
            }
            if self.strict_pools {
                let prev = self
                    .last_pools
                    .as_ref()
                    .filter(|(h, _)| h.saturating_add(1) == block.height)
                    .map(|(_, p)| p);
                let verdict = match block.pools.as_deref() {
                    None => Err("zebrad reported no valuePools".to_string()),
                    Some(pools) => crate::pools::check_pools(prev, pools, &block.txs)
                        .map_err(|e| format!("zebrad pool accounting inconsistent: {e}")),
                };
                if let Err(why) = verdict {
                    self.held = Some(format!("zcash block {}: {why}", block.height));
                    break;
                }
            }
            let sip8 = self.sip8_from.is_some_and(|from| block.height >= from);
            let mut burns = Vec::new();
            for tx in &block.txs {
                let outs = tx.outputs.iter().map(|o| TxOutRef {
                    value_zat: o.value_zat,
                    script: o.script.as_slice(),
                });
                if let Some((burn, reference)) = sip1::extract_burn_at(outs, sip8) {
                    burns.push(EpochBurn {
                        txid: tx.txid,
                        burn,
                        reference,
                    });
                }
            }
            self.window.push_back((block.height, block.hash));
            while self.window.len() > self.window_cap {
                self.window.pop_front();
            }
            self.next_height = block.height.saturating_add(1);
            self.last_pools = block.pools.as_deref().map(|p| (block.height, *p));
            events.push(FollowerEvent::Epoch(EpochData {
                height: block.height,
                hash: block.hash,
                burns,
                time: block.time,
                txs: block.txs,
                pools: block.pools,
            }));
        }

        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sip1::{BurnPayload, BurnPayloadV2, SovaRef, burn_lock_script};
    use std::cell::RefCell;

    /// In-memory chain the tests mutate to simulate growth and reorgs.
    struct MockView {
        blocks: RefCell<Vec<BlockView>>, // index 0 = height 1
    }

    fn h32(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn burn_tx(txid_byte: u8, addr_byte: u8, value_zat: u64) -> TxView {
        let payload = BurnPayload {
            evm_address: [addr_byte; 20],
            signal_bits: 0,
        };
        TxView {
            txid: h32(txid_byte),
            outputs: vec![
                TxOut {
                    value_zat: 0,
                    script: payload.to_script().to_vec(),
                },
                TxOut {
                    value_zat,
                    script: burn_lock_script().to_vec(),
                },
            ],
            version: 5,
            shielded: Default::default(),
        }
    }

    fn plain_tx(txid_byte: u8) -> TxView {
        TxView {
            txid: h32(txid_byte),
            outputs: vec![TxOut {
                value_zat: 42,
                script: vec![0x51],
            }],
            version: 5,
            shielded: Default::default(),
        }
    }

    impl MockView {
        fn new() -> Self {
            Self {
                blocks: RefCell::new(Vec::new()),
            }
        }

        /// Append a block with the given hash byte and txs.
        fn push(&self, hash_byte: u8, txs: Vec<TxView>) {
            let mut blocks = self.blocks.borrow_mut();
            let height = blocks.len() as u64 + 1;
            let prev_hash = blocks.last().map_or(h32(0), |b: &BlockView| b.hash);
            blocks.push(BlockView {
                height,
                hash: h32(hash_byte),
                prev_hash,
                txs,
                time: 0,
                pools: None,
            });
        }

        /// Reorg: truncate to `keep` blocks, then the caller pushes the
        /// replacement chain.
        fn truncate(&self, keep: usize) {
            self.blocks.borrow_mut().truncate(keep);
        }
    }

    impl ZcashView for MockView {
        fn tip_height(&self) -> Result<u64, ViewError> {
            Ok(self.blocks.borrow().len() as u64)
        }
        fn block_at(&self, height: u64) -> Result<Option<BlockView>, ViewError> {
            if height == 0 {
                return Ok(None);
            }
            Ok(self.blocks.borrow().get(height as usize - 1).cloned())
        }
    }

    fn epochs_of(events: &[FollowerEvent]) -> Vec<(u64, [u8; 32], usize)> {
        events
            .iter()
            .filter_map(|e| match e {
                FollowerEvent::Epoch(ep) => Some((ep.height, ep.hash, ep.burns.len())),
                FollowerEvent::Rollback { .. } => None,
            })
            .collect()
    }

    fn v2_burn_tx(txid_byte: u8, addr_byte: u8, value_zat: u64, reference: SovaRef) -> TxView {
        let payload = BurnPayloadV2 {
            evm_address: [addr_byte; 20],
            signal_bits: 0,
            reference,
        };
        TxView {
            txid: h32(txid_byte),
            outputs: vec![
                TxOut {
                    value_zat: 0,
                    script: payload.to_script().to_vec(),
                },
                TxOut {
                    value_zat,
                    script: burn_lock_script().to_vec(),
                },
            ],
            version: 5,
            shielded: Default::default(),
        }
    }

    /// SIP-8 §10 "activation boundary": a v2 burn below the activation
    /// height is not a burn, one at it is, and v1 burns are untouched on
    /// both sides.
    #[test]
    fn sip8_activation_boundary() {
        let r = SovaRef {
            height: 1,
            hash: h32(0xee),
        };
        let view = MockView::new();
        view.push(
            1,
            vec![v2_burn_tx(0x11, 1, 5_000, r), burn_tx(0x12, 2, 5_000)],
        );
        view.push(
            2,
            vec![v2_burn_tx(0x21, 1, 5_000, r), burn_tx(0x22, 2, 5_000)],
        );
        let burns_at = |from: Option<u64>| -> Vec<Vec<(u8, Option<SovaRef>)>> {
            let mut f = Follower::new(1, 100).with_sip8_from(from);
            f.poll(&view)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|e| match e {
                    FollowerEvent::Epoch(ep) => {
                        Some(ep.burns.iter().map(|b| (b.txid[0], b.reference)).collect())
                    }
                    FollowerEvent::Rollback { .. } => None,
                })
                .collect()
        };
        // Activation at height 2.
        assert_eq!(
            burns_at(Some(2)),
            vec![vec![(0x12, None)], vec![(0x21, Some(r)), (0x22, None)]]
        );
        // Never active: SIP-1 alone, the v2 burns do not exist.
        assert_eq!(burns_at(None), vec![vec![(0x12, None)], vec![(0x22, None)]]);
    }

    #[test]
    fn scans_linear_chain_and_recognizes_burns() {
        let view = MockView::new();
        view.push(1, vec![plain_tx(0x11)]);
        view.push(2, vec![burn_tx(0x22, 7, 50_000), plain_tx(0x23)]);
        view.push(3, vec![]);
        let mut f = Follower::new(1, 100);
        let events = f.poll(&view).unwrap_or_default();
        assert_eq!(
            epochs_of(&events),
            vec![(1, h32(1), 0), (2, h32(2), 1), (3, h32(3), 0)]
        );
        let FollowerEvent::Epoch(ep2) = &events[1] else {
            panic!("expected epoch")
        };
        assert_eq!(ep2.burns[0].burn.evm_address, [7u8; 20]);
        assert_eq!(ep2.burns[0].burn.value_zat, 50_000);
        // Idle poll: nothing new.
        assert!(f.poll(&view).unwrap_or_default().is_empty());
    }

    #[test]
    fn reorg_emits_rollback_then_replacement_epochs() {
        let view = MockView::new();
        view.push(1, vec![]);
        view.push(2, vec![burn_tx(0x22, 7, 50_000)]);
        view.push(3, vec![]);
        view.push(4, vec![]);
        let mut f = Follower::new(1, 100);
        let _ = f.poll(&view).unwrap_or_default();

        // Reorg heights 3-4 away; new chain 3'-4'-5' with a burn at 4'.
        view.truncate(2);
        view.push(0x33, vec![]);
        view.push(0x44, vec![burn_tx(0x99, 9, 70_000)]);
        view.push(0x55, vec![]);

        let events = f.poll(&view).unwrap_or_default();
        assert_eq!(events[0], FollowerEvent::Rollback { to_height: 2 });
        assert_eq!(
            epochs_of(&events),
            vec![(3, h32(0x33), 0), (4, h32(0x44), 1), (5, h32(0x55), 0)]
        );
    }

    #[test]
    fn rescan_after_rollback_equals_fresh_scan() {
        let view = MockView::new();
        view.push(1, vec![burn_tx(0x21, 1, 10_000)]);
        view.push(2, vec![]);
        view.push(3, vec![burn_tx(0x23, 3, 30_000)]);
        let mut ongoing = Follower::new(1, 100);
        let _ = ongoing.poll(&view).unwrap_or_default();

        view.truncate(1);
        view.push(0x32, vec![burn_tx(0x77, 5, 20_000)]);
        view.push(0x33, vec![]);

        let continued = ongoing.poll(&view).unwrap_or_default();
        let mut fresh = Follower::new(1, 100);
        let fresh_all = fresh.poll(&view).unwrap_or_default();

        // The continued follower's post-rollback epochs must equal the
        // fresh follower's epochs for the same range (heights 2..).
        let continued_epochs: Vec<_> = epochs_of(&continued);
        let fresh_tail: Vec<_> = epochs_of(&fresh_all)
            .into_iter()
            .filter(|(h, _, _)| *h >= 2)
            .collect();
        assert_eq!(continued_epochs, fresh_tail);
    }

    #[test]
    fn reorg_deeper_than_window_unwinds_to_base() {
        let view = MockView::new();
        for i in 1..=6u8 {
            view.push(i, vec![]);
        }
        // Window of 3: follower only remembers heights 4-6.
        let mut f = Follower::new(1, 3);
        let _ = f.poll(&view).unwrap_or_default();

        // Reorg everything from height 2 up.
        view.truncate(1);
        for i in 0x62..=0x66u8 {
            view.push(i, vec![]);
        }
        let events = f.poll(&view).unwrap_or_default();
        // Window exhausted: unwind to base-1 = 0, rescan all heights.
        assert_eq!(events[0], FollowerEvent::Rollback { to_height: 0 });
        assert_eq!(epochs_of(&events).len(), 6);
        assert_eq!(epochs_of(&events)[0].0, 1);
    }

    #[test]
    fn window_stays_bounded() {
        let view = MockView::new();
        for i in 1..=50u8 {
            view.push(i, vec![]);
        }
        let mut f = Follower::new(1, 10);
        let _ = f.poll(&view).unwrap_or_default();
        assert!(f.window.len() <= 10);
    }

    /// SIP-7: with strict pools the follower stops before a block whose
    /// pools are missing or inconsistent, and emits it once they add up;
    /// without, nothing changes.
    #[test]
    fn strict_pools_hold_rather_than_skip() {
        use crate::pools::BlockPools;
        let supply = |v: u64| BlockPools {
            chain_value_zat: [v, 0, 0, 0, 0, 0],
            delta_zat: [if v > 100 { 10 } else { 100 }, 0, 0, 0, 0, 0],
            chain_supply_zat: v,
            ..BlockPools::default()
        };
        let view = MockView::new();
        view.push(1, vec![plain_tx(1)]);
        view.push(2, vec![plain_tx(2)]);
        // Lenient: both emitted, pools or not.
        let mut lenient = Follower::new(1, 8);
        assert_eq!(lenient.poll(&view).map(|e| e.len()).ok(), Some(2));
        // Strict: block 1 has no pools -> hold at 1.
        let mut strict = Follower::new(1, 8).with_strict_pools(true);
        assert_eq!(strict.poll(&view).map(|e| e.len()).ok(), Some(0));
        assert!(
            strict
                .hold_reason()
                .is_some_and(|r| r.contains("no valuePools"))
        );
        // Pools for both, but block 2 breaks continuity (100 + 10 != 111).
        view.blocks.borrow_mut()[0].pools = Some(Box::new(supply(100)));
        let mut bad = supply(111);
        bad.chain_supply_zat = 111;
        view.blocks.borrow_mut()[1].pools = Some(Box::new(bad));
        assert_eq!(
            strict.poll(&view).map(|e| e.len()).ok(),
            Some(1),
            "block 1 only"
        );
        assert!(
            strict
                .hold_reason()
                .is_some_and(|r| r.contains("continuity"))
        );
        // Fixed: block 2 is emitted on the next poll, not skipped.
        view.blocks.borrow_mut()[1].pools = Some(Box::new(supply(110)));
        let events = strict.poll(&view).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(events.as_slice(), [FollowerEvent::Epoch(e)] if e.height == 2));
        assert!(strict.hold_reason().is_none());
    }
}
