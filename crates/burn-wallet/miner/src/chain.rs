//! Chain-identity guard (E1d): notice when the Zcash chain this miner's
//! state was built on is no longer the chain its node serves, and retire the
//! chain-derived state instead of spending UTXOs that don't exist.
//!
//! The failure this exists for: a regtest node is recreated (`./box/up.sh
//! down` then `up`, or anyone resetting their own regtest `zebrad`) while
//! `state.json` survives. The tracked UTXOs name transactions on the dead
//! chain, so every burn is rejected with `could not find transparent input
//! UTXO` and nothing ever settles. The same thing happens if `--rpc` is
//! pointed at a node on a different network with an existing data dir.
//!
//! Two checks, both needed:
//!
//! 1. **Chain identity** -- the block hash at [`ANCHOR_HEIGHT`], recorded in
//!    [`MinerState::chain_anchor`]. Why height 1 and not genesis: every
//!    regtest `zebrad` shares the same hard-coded genesis block, so the
//!    genesis hash can't tell two regtest chains apart. On testnet/mainnet
//!    block 1 is buried under millions of blocks, so a mismatch never fires
//!    on an ordinary reorg -- only when the node serves a different chain.
//! 2. **Tracked transactions exist** -- the node must know (in its best
//!    chain or mempool) every transaction that funds a tracked UTXO, and
//!    the latest epoch's burn. The identity check alone is not enough on
//!    regtest: zebrad's regtest block timestamps are deterministic, and a
//!    box re-funds the same keystore the same way, so a recreated chain
//!    starts out as a near-replay of the old one -- the funding and burn
//!    *txids* were byte-identical across a `down`/`up` in the E1d run. Two
//!    observed resets did still produce different block-1 hashes, but
//!    nothing guarantees that, and a replayed prefix with an equal block 1
//!    would pass check 1 while the change outputs the state holds (created
//!    by burns above the new chain's tip) don't exist.
//!
//! Either check failing means the state is for a chain the node doesn't
//! serve: its chain-derived part is retired (see
//! [`MinerState::retire_chain`]) and the miner re-discovers its funding on
//! the node's chain.
//!
//! When the node's tip is still below the anchor height (a freshly started
//! regtest node before anything is mined), the check is
//! [`ChainCheck::Undetermined`]: nothing is reset on a guess, the caller
//! just waits for blocks. A node that is behind on a real network (still
//! syncing) fails check 2 and resets: the tracked outpoints stay in
//! `retired_chains`, and mining against an unsynced node can't work anyway.

use burn_wallet::RpcClient;
use burn_wallet::RpcError;

use crate::state::{ChainAnchor, MinerState};

/// The height whose block hash identifies a chain -- see the module docs.
pub(crate) const ANCHOR_HEIGHT: u64 = 1;

/// The few read-only node queries the guard needs; a trait so the
/// detection logic is unit-testable without a node.
pub(crate) trait ChainView {
    /// `getblockcount`.
    fn tip_height(&self) -> Result<u64, RpcError>;
    /// `getblockhash <height>`, RPC-display hex.
    fn block_hash(&self, height: u64) -> Result<String, RpcError>;
    /// Whether the node knows `txid` at all (mempool or best chain).
    fn tx_known(&self, txid: &str) -> Result<bool, RpcError>;
}

impl ChainView for RpcClient {
    fn tip_height(&self) -> Result<u64, RpcError> {
        self.get_block_count()
    }

    fn block_hash(&self, height: u64) -> Result<String, RpcError> {
        self.get_block_hash(height)
    }

    fn tx_known(&self, txid: &str) -> Result<bool, RpcError> {
        match self.get_raw_transaction_verbose(txid) {
            Ok(_) => Ok(true),
            // The node answered and did not return the tx (zebrad: -5 "No
            // such mempool or main chain transaction"). Transport errors
            // still propagate: "couldn't ask" is not "not there".
            Err(RpcError::RpcFailure { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// The outcome of [`check_chain`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChainCheck {
    /// The recorded anchor matches the node and it knows every tracked
    /// transaction: state is for this chain.
    Same,
    /// No anchor was recorded (a new data dir, or state from before E1d)
    /// and nothing tracked contradicts the node; the anchor is now set.
    Anchored,
    /// The node's tip is below the anchor height, so the chains can't be
    /// compared yet. Nothing was changed.
    Undetermined {
        /// The node's current tip height.
        tip: u64,
    },
    /// The recorded chain is gone; its chain-derived state was retired
    /// (see [`MinerState::retire_chain`]) and the state re-anchored to the
    /// node's chain.
    Reset {
        /// Why, human-readable (also stored with the retired chain).
        reason: String,
    },
}

/// Compares `state`'s recorded chain against the node behind `view` (the
/// two checks in the module docs), anchoring or retiring as described on
/// [`ChainCheck`]. Mutates `state` only in memory; the caller saves it.
///
/// # Errors
///
/// Returns the underlying [`RpcError`] if the node can't be queried.
pub(crate) fn check_chain(
    view: &impl ChainView,
    state: &mut MinerState,
) -> Result<ChainCheck, RpcError> {
    let anchor_height = state
        .chain_anchor
        .as_ref()
        .map_or(ANCHOR_HEIGHT, |a| a.height);
    let tip = view.tip_height()?;
    if tip < anchor_height {
        return Ok(ChainCheck::Undetermined { tip });
    }
    let live = ChainAnchor {
        height: anchor_height,
        hash: view.block_hash(anchor_height)?,
    };

    let reason = match &state.chain_anchor {
        Some(recorded) if *recorded != live => Some(format!(
            "block {} on the node is {}, but this state was recorded on a chain where it is {}",
            recorded.height, live.hash, recorded.hash
        )),
        _ => first_unknown_txid(view, state)?.map(|txid| {
            format!(
                "the node does not know tx {txid}, which this state tracks (the chain was recreated or rewound)"
            )
        }),
    };
    match reason {
        Some(reason) => {
            state.retire_chain(reason.clone(), live);
            Ok(ChainCheck::Reset { reason })
        }
        None if state.chain_anchor.is_none() => {
            state.chain_anchor = Some(live);
            Ok(ChainCheck::Anchored)
        }
        None => Ok(ChainCheck::Same),
    }
}

/// The first txid among the tracked UTXOs' funding transactions and the
/// latest epoch's burn that `view` doesn't know, if any.
fn first_unknown_txid(
    view: &impl ChainView,
    state: &MinerState,
) -> Result<Option<String>, RpcError> {
    // Deduplicated: the latest burn usually also funds the change UTXO,
    // and one RPC per distinct tx is all this needs.
    let txids: std::collections::BTreeSet<&str> = state
        .utxos
        .iter()
        .map(|u| u.txid.as_str())
        .chain(state.epochs.last().map(|e| e.txid.as_str()))
        .collect();
    for txid in txids {
        if !view.tx_known(txid)? {
            return Ok(Some(txid.to_string()));
        }
    }
    Ok(None)
}

#[cfg(test)]
// Test code: an unexpected `Err` here is a test failure.
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::state::{EpochRecord, TrackedUtxo};

    /// An in-memory node: a list of block hashes (index = height) and the
    /// set of txids it knows.
    struct FakeNode {
        blocks: Vec<String>,
        txs: BTreeSet<String>,
    }

    impl FakeNode {
        fn new(chain_tag: &str, height: u64, txs: &[&str]) -> Self {
            // Genesis is shared by every regtest chain, exactly like zebrad.
            let mut blocks = vec!["genesis".to_string()];
            blocks.extend((1..=height).map(|h| format!("{chain_tag}-{h}")));
            Self {
                blocks,
                txs: txs.iter().map(ToString::to_string).collect(),
            }
        }
    }

    impl ChainView for FakeNode {
        fn tip_height(&self) -> Result<u64, RpcError> {
            Ok(u64::try_from(self.blocks.len()).unwrap() - 1)
        }
        fn block_hash(&self, height: u64) -> Result<String, RpcError> {
            self.blocks
                .get(usize::try_from(height).unwrap())
                .cloned()
                .ok_or_else(|| RpcError::RpcFailure {
                    method: "getblockhash".to_string(),
                    code: -8,
                    message: "Block height out of range".to_string(),
                })
        }
        fn tx_known(&self, txid: &str) -> Result<bool, RpcError> {
            Ok(self.txs.contains(txid))
        }
    }

    fn txid(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    /// State that has mined two epochs on chain `tag` (anchored to it)
    /// and holds one change UTXO.
    fn mined_state(tag: &str) -> MinerState {
        let mut state = MinerState::new("tmAddr".to_string(), "ab".repeat(20));
        state.chain_anchor = Some(ChainAnchor {
            height: ANCHOR_HEIGHT,
            hash: format!("{tag}-1"),
        });
        for n in 1..=2u8 {
            state.record_epoch(EpochRecord {
                epoch: u64::from(n),
                height: 102 + u64::from(n),
                burn_zat: 100_000,
                fee_zat: 20_000,
                change_zat: 1_000_000,
                txid: txid(n),
            });
        }
        state.utxos.push(TrackedUtxo {
            txid: txid(2),
            vout: 2,
            value_zat: 1_000_000,
        });
        state
    }

    #[test]
    fn same_chain_is_left_alone() {
        let node = FakeNode::new("a", 150, &[&txid(1), &txid(2)]);
        let mut state = mined_state("a");
        assert_eq!(check_chain(&node, &mut state).unwrap(), ChainCheck::Same);
        assert_eq!(state.utxos.len(), 1);
        assert_eq!(state.epochs.len(), 2);
        assert!(state.retired_chains.is_empty());
    }

    /// The E1d bug: the regtest chain was recreated under a surviving
    /// state.json. Same genesis, different block 1 -> reset.
    #[test]
    fn recreated_regtest_chain_retires_chain_state() {
        let node = FakeNode::new("b", 101, &[]);
        let mut state = mined_state("a");
        state.begin_invocation(6_000_000, 100_000, Some(9_000_000));
        let lifetime_before = state.total_spent_zat();

        let check = check_chain(&node, &mut state).unwrap();
        assert!(matches!(check, ChainCheck::Reset { .. }), "{check:?}");

        // Chain-derived state is gone from the live view...
        assert!(state.utxos.is_empty());
        assert!(state.epochs.is_empty());
        assert_eq!(state.chain_burned_zat(), 0);
        assert_eq!(
            state.chain_anchor,
            Some(ChainAnchor {
                height: 1,
                hash: "b-1".to_string()
            })
        );
        // ...kept in the archive...
        assert_eq!(state.retired_chains.len(), 1);
        let retired = &state.retired_chains[0];
        assert_eq!(retired.anchor.as_ref().unwrap().hash, "a-1");
        assert_eq!(retired.epochs.len(), 2);
        assert_eq!(retired.utxos.len(), 1);
        // ...and lifetime/budget accounting is untouched.
        assert_eq!(state.total_spent_zat(), lifetime_before);
        assert_eq!(state.lifetime_budget_zat, Some(9_000_000));
        assert_eq!(state.budget_remaining_zat(), 6_000_000);

        // Now anchored to the new chain: a second check is a no-op.
        assert_eq!(check_chain(&node, &mut state).unwrap(), ChainCheck::Same);
    }

    /// Check 1 on its own: a different block 1 resets even if the node
    /// happens to know the tracked txids (a regtest replay produces
    /// byte-identical funding/burn txids on a new chain).
    #[test]
    fn different_anchor_resets_even_with_identical_txids() {
        let node = FakeNode::new("b", 150, &[&txid(1), &txid(2)]);
        let mut state = mined_state("a");
        let check = check_chain(&node, &mut state).unwrap();
        assert!(matches!(check, ChainCheck::Reset { .. }), "{check:?}");
        assert!(state.utxos.is_empty());
    }

    /// A node that is shorter than the anchor height can't be compared:
    /// nothing is reset on a guess.
    #[test]
    fn node_below_anchor_height_is_undetermined() {
        let node = FakeNode::new("b", 0, &[]);
        let mut state = mined_state("a");
        assert_eq!(
            check_chain(&node, &mut state).unwrap(),
            ChainCheck::Undetermined { tip: 0 }
        );
        assert_eq!(state.utxos.len(), 1);
        assert!(state.retired_chains.is_empty());
    }

    /// A recreated regtest chain can replay the old one's prefix (same
    /// block 1 in the worst case): check 2 still catches that the tracked
    /// change outputs were created above the new chain's tip.
    #[test]
    fn replayed_prefix_with_same_anchor_is_still_reset() {
        // Same anchor ("a-1"), but the node is a fresh replay at height 101
        // that has none of the state's burns yet.
        let node = FakeNode::new("a", 101, &[]);
        let mut state = mined_state("a");
        let check = check_chain(&node, &mut state).unwrap();
        assert!(matches!(check, ChainCheck::Reset { .. }), "{check:?}");
        assert!(state.utxos.is_empty());
        assert!(state.epochs.is_empty());
        assert_eq!(state.retired_chains.len(), 1);
    }

    #[test]
    fn fresh_state_gets_anchored() {
        let node = FakeNode::new("a", 101, &[]);
        let mut state = MinerState::new("tmAddr".to_string(), "ab".repeat(20));
        assert_eq!(
            check_chain(&node, &mut state).unwrap(),
            ChainCheck::Anchored
        );
        assert_eq!(state.chain_anchor.as_ref().unwrap().hash, "a-1");
    }

    /// Pre-E1d state (no anchor) whose UTXOs the node knows is simply
    /// anchored; one whose UTXOs it doesn't know is retired.
    #[test]
    fn unanchored_legacy_state_is_validated_by_its_txids() {
        let mut legacy = mined_state("a");
        legacy.chain_anchor = None;

        let knows_them = FakeNode::new("a", 150, &[&txid(1), &txid(2)]);
        let mut state = legacy.clone();
        assert_eq!(
            check_chain(&knows_them, &mut state).unwrap(),
            ChainCheck::Anchored
        );
        assert_eq!(state.utxos.len(), 1);

        let fresh_chain = FakeNode::new("b", 101, &[]);
        let mut state = legacy;
        let check = check_chain(&fresh_chain, &mut state).unwrap();
        assert!(matches!(check, ChainCheck::Reset { .. }), "{check:?}");
        assert!(state.utxos.is_empty());
        assert!(state.retired_chains[0].anchor.is_none());
        assert_eq!(state.chain_anchor.as_ref().unwrap().hash, "b-1");
    }

    /// After a reset the per-chain epoch counter restarts: the next epoch
    /// is numbered 1 again (`attempt_epoch` numbers `epochs.len() + 1`).
    #[test]
    fn epoch_counter_is_per_chain() {
        let node = FakeNode::new("b", 101, &[]);
        let mut state = mined_state("a");
        check_chain(&node, &mut state).unwrap();
        assert_eq!(state.epochs.len() + 1, 1);
    }
}
