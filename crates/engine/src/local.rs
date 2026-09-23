//! Dev-mode ("`--dev`") local mining support for [`SovaNode`](crate::SovaNode).
//!
//! reth's `LocalMiner` builds each auto-mined block's payload attributes via
//! `DebugNode::local_payload_attributes_builder`. This wraps reth's stock
//! [`LocalPayloadAttributesBuilder`] and lifts its output into
//! [`SovaPayloadAttributes`] with `epoch: None` — dev-chain auto-mining stays
//! exactly as it was under `EthereumNode` (this is the B1 -> B3a continuity
//! check: the dev chain must still launch and mine unchanged).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256};
use reth_ethereum::{
    chainspec::{EthChainSpec, EthereumHardforks},
    engine::local::LocalPayloadAttributesBuilder,
    node::api::PayloadAttributesBuilder,
    primitives::{AlloyBlockHeader, SealedHeader},
};

use crate::{SovaEpochAttribute, SovaPayloadAttributes};

/// Staged epochs kept at most; far above any real backlog (the sealer
/// produces in order from the front), it only bounds a stuck miner.
const PENDING_RETAIN: usize = 256;

/// A **height-addressed** mailbox between the sealer service and the
/// payload attributes builder: the sealer stages the epoch for a
/// specific Sova height; the builder takes it only when it is actually
/// building that height. Without the address, a cadence build for an
/// earlier (burn-less) height would drain the settlement into the wrong
/// block — which the node's own C5 validator then rejects, losing the
/// epoch (observed live in the ladder scenario: a node resuming with a
/// backlog stalled exactly this way).
///
/// Since SIP-4 every height is staged, burn-less ones included (they
/// carry the Zcash anchor), and one sealer pass can trigger several
/// cadence heights, so the mailbox holds one entry per height.
///
/// A poisoned lock degrades to "no epoch" rather than panicking in the
/// build path — a missed settlement is retried next trigger, a panic in
/// payload building is not.
#[derive(Debug, Default, Clone)]
pub struct PendingEpoch(Arc<Mutex<BTreeMap<u64, SovaEpochAttribute>>>);

impl PendingEpoch {
    /// Stage the epoch for the block at `sova_height`, replacing any
    /// unbuilt one for that height.
    pub fn stage(&self, sova_height: u64, epoch: SovaEpochAttribute) {
        if let Ok(mut slots) = self.0.lock() {
            slots.insert(sova_height, epoch);
            while slots.len() > PENDING_RETAIN {
                slots.pop_first();
            }
        }
    }

    /// Take the epoch staged for `sova_height`. Entries for lower
    /// heights are stale (that height was built or covered) and are
    /// dropped; entries for higher heights stay put.
    #[must_use]
    pub fn take_for(&self, sova_height: u64) -> Option<SovaEpochAttribute> {
        let mut slots = self.0.lock().ok()?;
        slots.retain(|&h, _| h >= sova_height);
        slots.remove(&sova_height)
    }

    /// Take the lowest staged epoch regardless of address
    /// (tests/inspection).
    #[must_use]
    pub fn take(&self) -> Option<SovaEpochAttribute> {
        self.0
            .lock()
            .ok()
            .and_then(|mut slots| slots.pop_first())
            .map(|(_, epoch)| epoch)
    }
}

/// Builds [`SovaPayloadAttributes`] (with `epoch: None`) for dev-mode local
/// mining, by delegating to reth's stock [`LocalPayloadAttributesBuilder`]
/// for the inner Ethereum attributes.
#[derive(Debug)]
pub struct SovaLocalPayloadAttributesBuilder<ChainSpec> {
    inner: LocalPayloadAttributesBuilder<ChainSpec>,
    pending: PendingEpoch,
    /// Who collects priority fees in blocks we build. reth's stock
    /// builder picks a random address, which would burn SIP-3's
    /// "priority fees pay the sealer" in practice.
    fee_recipient: Option<Address>,
}

impl<ChainSpec> SovaLocalPayloadAttributesBuilder<ChainSpec> {
    /// Creates a new instance of the builder for the given chain spec.
    pub fn new(chain_spec: Arc<ChainSpec>) -> Self {
        Self::with_pending(chain_spec, PendingEpoch::default())
    }

    /// Creates a builder sharing the given [`PendingEpoch`] mailbox with a
    /// sealer service: a staged epoch rides the next built block.
    pub fn with_pending(chain_spec: Arc<ChainSpec>, pending: PendingEpoch) -> Self {
        Self {
            inner: LocalPayloadAttributesBuilder::new(chain_spec),
            pending,
            fee_recipient: None,
        }
    }

    /// Pay priority fees in blocks we build to `recipient` (our sealer
    /// address).
    #[must_use]
    pub const fn with_fee_recipient(mut self, recipient: Address) -> Self {
        self.fee_recipient = Some(recipient);
        self
    }
}

impl<ChainSpec> PayloadAttributesBuilder<SovaPayloadAttributes, ChainSpec::Header>
    for SovaLocalPayloadAttributesBuilder<ChainSpec>
where
    ChainSpec: EthChainSpec + EthereumHardforks + 'static,
    ChainSpec::Header: AlloyBlockHeader,
{
    fn build(&self, parent: &SealedHeader<ChainSpec::Header>) -> SovaPayloadAttributes {
        // Height-addressed drain: this build produces the block at
        // parent + 1; only a settlement staged for exactly that height
        // may ride it (see `PendingEpoch`'s doc for the failure mode).
        let epoch = self.pending.take_for(parent.number().saturating_add(1));
        let mut inner = self.inner.build(parent);
        if let Some(recipient) = self.fee_recipient {
            inner.suggested_fee_recipient = recipient;
        }
        // SIP-4 anchor: the block commits to its epoch's Zcash block hash.
        // Without a staged epoch (plain dev chain, no zebrad) reth's
        // default stays.
        if let Some(epoch) = &epoch {
            inner.parent_beacon_block_root = Some(B256::from(epoch.zcash_hash));
        }
        SovaPayloadAttributes { inner, epoch }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch(zcash_height: u64) -> SovaEpochAttribute {
        SovaEpochAttribute {
            zcash_height,
            zcash_hash: [zcash_height as u8; 32],
            settlements: Vec::new(),
        }
    }

    /// One sealer pass can stage several cadence heights before the
    /// miner builds any of them; each build must get its own anchor.
    #[test]
    fn several_heights_stage_independently() {
        let p = PendingEpoch::default();
        p.stage(5, epoch(105));
        p.stage(6, epoch(106));
        p.stage(7, epoch(107));
        assert_eq!(p.take_for(6).map(|e| e.zcash_height), Some(106));
        // Height 5 was passed over: stale, dropped.
        assert_eq!(p.take_for(5), None);
        assert_eq!(p.take_for(7).map(|e| e.zcash_height), Some(107));
        assert_eq!(p.take(), None);
    }

    #[test]
    fn restage_replaces_and_other_heights_stay() {
        let p = PendingEpoch::default();
        p.stage(9, epoch(1));
        p.stage(9, epoch(2));
        p.stage(10, epoch(3));
        assert_eq!(p.take_for(8), None);
        assert_eq!(p.take_for(9).map(|e| e.zcash_height), Some(2));
        assert_eq!(p.take_for(10).map(|e| e.zcash_height), Some(3));
    }
}
