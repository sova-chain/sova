//! [`SovaConsensus`]: C5 enforced on every import path.
//!
//! C5 (a block's withdrawals — its SOVA mint — must match what this node
//! derives from its own zebrad) first lived only in
//! [`crate::SovaEngineValidator`]'s `convert_payload_to_block`. That hook
//! runs for Engine API payloads only; reth's engine-tree download path and
//! its backfill pipeline import blocks through `Consensus` + the executor
//! and never call it, so a node syncing history would have accepted any
//! mint in it (`docs/design/p2p-m1.md`). `Consensus::validate_block_pre_execution`
//! runs on all three paths — and withdrawals sit in the body, so no
//! execution is needed — which makes it the enforcement point.
//!
//! Heights above the follower's scanned watermark are **held** (SIP-4 §1,
//! "hold, don't accept"): a transient error, never cached as invalid, and
//! retried once our zebrad catches up (the sova/1 service parks held
//! blocks and resubmits them). A Sova block can legitimately outrun our
//! zebrad by seconds at the tip, but executing it would need Zcash answers
//! (the SIP-4 precompile) this node can't give yet, and accepting it on
//! trust would skip the mint check. The payload-path check stays for candidate
//! observation; the two share [`ExpectedSettlements::check_ranked`], so
//! they cannot disagree about validity.

use std::sync::Arc;

use alloy_primitives::B256;
use reth_ethereum::{
    Block, EthPrimitives, Receipt,
    chainspec::ChainSpec,
    consensus::{
        Consensus, ConsensusError, EthBeaconConsensus, FullConsensus, HeaderValidator,
        ReceiptRootBloom, TransactionRoot,
    },
    evm::primitives::block::BlockExecutionResult,
    node::{
        api::{FullNodeTypes, NodeTypes},
        builder::{BuilderContext, components::ConsensusBuilder},
    },
    primitives::{AlloyBlockHeader, RecoveredBlock, SealedBlock, SealedHeader},
};

use crate::expectations::{AnchorVerdict, ExpectedSettlements, RankedVerdict};

/// Appears in the text of every transient ("hold") rejection. reth reports
/// a transient consensus error to the submitter as `Invalid` (it only
/// skips the invalid-block cache), so the sova/1 service matches on this
/// to tell a hold from a bad block: no reputation hit, and the block may
/// be offered again.
pub(crate) const HOLD_MARKER: &str = "sova-hold";

/// Why C5 rejected a block.
#[derive(Debug, thiserror::Error)]
pub enum SettlementError {
    /// The block's withdrawals match no rank's local derivation.
    #[error(
        "settlement mismatch at height {height}: withdrawals match no rank's local Zcash derivation"
    )]
    Mismatch {
        /// The Sova height checked.
        height: u64,
    },
    /// A height at or below the scanned watermark has no record. Should be
    /// impossible (history is never pruned); treated as transient so a
    /// bookkeeping fault can never permanently mark a block invalid.
    #[error("{HOLD_MARKER}: no settlement record at scanned height {height}")]
    MissingRecord {
        /// The Sova height checked.
        height: u64,
    },
    /// SIP-4: the block's epoch is above what our follower has scanned.
    /// Held until the scan reaches it.
    #[error(
        "{HOLD_MARKER}: zcash epoch not scanned yet at height {height} (scanned through {scanned})"
    )]
    Unscanned {
        /// The Sova height checked.
        height: u64,
        /// Our scan watermark (Sova height).
        scanned: u64,
    },
    /// SIP-4: the block's Zcash anchor (`parent_beacon_block_root`) is
    /// not the Zcash block our follower has at the block's epoch. Held,
    /// not rejected: the sealer may have been on a Zcash fork we will
    /// converge onto, and reth must not cache the block as invalid.
    #[error(
        "{HOLD_MARKER}: zcash anchor mismatch at height {height}: block commits to {got:?}, our zebrad has {expected}"
    )]
    AnchorMismatch {
        /// The Sova height checked.
        height: u64,
        /// Our follower's Zcash hash at the epoch (display order).
        expected: B256,
        /// The block's `parent_beacon_block_root`.
        got: Option<B256>,
    },
}

/// Ethereum beacon consensus plus C5 settlement enforcement.
#[derive(Debug, Clone)]
pub struct SovaConsensus {
    inner: EthBeaconConsensus<ChainSpec>,
    expectations: &'static ExpectedSettlements,
    /// Read per check, not at construction: bin/sova sets the process
    /// schedule after the node (and so this consensus) is built.
    schedule: fn() -> consensus::schedule::Schedule,
    /// SIP-6 activation (the chain ID seals are checked under), read per
    /// check for the same reason.
    sip6: fn() -> Option<u64>,
}

impl SovaConsensus {
    /// Consensus over `chain_spec`, checking settlements against
    /// `expectations` under the schedule `schedule` returns.
    #[must_use]
    pub const fn new(
        chain_spec: Arc<ChainSpec>,
        expectations: &'static ExpectedSettlements,
        schedule: fn() -> consensus::schedule::Schedule,
    ) -> Self {
        Self {
            // The 97-byte sealed extra_data passes the inner check; the
            // exact SIP-6 length rule (and the 32-byte one before it) is
            // `crate::seal::check_header`.
            inner: EthBeaconConsensus::new(chain_spec)
                .with_max_extra_data_size(crate::seal::SEALED_EXTRA_LEN),
            expectations,
            schedule,
            sip6: crate::seal::active_chain_id,
        }
    }

    /// The same consensus with SIP-6 activation read from `sip6` (tests).
    #[must_use]
    pub const fn with_sip6(mut self, sip6: fn() -> Option<u64>) -> Self {
        self.sip6 = sip6;
        self
    }

    /// The C5 rule for one block.
    fn check_settlements(&self, block: &SealedBlock<Block>) -> Result<(), ConsensusError> {
        let height = block.header().number();
        let Some(scanned) = self.expectations.scanned_through() else {
            // No zebrad (plain dev chain) or nothing scanned yet.
            return Ok(());
        };
        if height > scanned {
            return Err(ConsensusError::other(SettlementError::Unscanned {
                height,
                scanned,
            }));
        }
        // The anchor comes first: only once the block provably settles the
        // same Zcash block as our follower does is a withdrawals mismatch
        // a permanent fact rather than a fork difference.
        let got = block.header().parent_beacon_block_root();
        match self.expectations.check_anchor(height, got) {
            AnchorVerdict::Match => {}
            AnchorVerdict::Mismatch { expected } => {
                return Err(ConsensusError::other(SettlementError::AnchorMismatch {
                    height,
                    expected: B256::from(expected),
                    got,
                }));
            }
            AnchorVerdict::Unknown => {
                return Err(ConsensusError::other(SettlementError::MissingRecord {
                    height,
                }));
            }
        }
        let withdrawals = block.body().withdrawals.as_deref().map(|w| w.as_slice());
        // SIP-6: the producer comes from the seal (validate_header already
        // rejected a bad one; this cannot fail differently).
        let sealer =
            crate::seal::sealer(block.header(), (self.sip6)()).map_err(ConsensusError::other)?;
        match self
            .expectations
            .check_sealed(height, withdrawals, (self.schedule)(), sealer)
        {
            RankedVerdict::Valid { rank } => {
                tracing::info!(height, rank, scanned, "c5: settlement enforced");
                Ok(())
            }
            RankedVerdict::ValidEmpty => {
                tracing::debug!(height, scanned, "c5: empty settlement enforced");
                Ok(())
            }
            RankedVerdict::Mismatch { height } => {
                Err(ConsensusError::other(SettlementError::Mismatch { height }))
            }
            RankedVerdict::Unknown => Err(ConsensusError::other(SettlementError::MissingRecord {
                height,
            })),
        }
    }
}

impl HeaderValidator for SovaConsensus {
    fn validate_header(&self, header: &SealedHeader) -> Result<(), ConsensusError> {
        self.inner.validate_header(header)?;
        crate::seal::check_header(header.header(), (self.sip6)())
            .map(|_| ())
            .map_err(ConsensusError::other)
    }

    fn validate_header_against_parent(
        &self,
        header: &SealedHeader,
        parent: &SealedHeader,
    ) -> Result<(), ConsensusError> {
        self.inner.validate_header_against_parent(header, parent)?;
        if (self.sip6)().is_none() || header.number() == 0 {
            return Ok(());
        }
        // SIP-6 pins timestamps to the epoch's Zcash time: without our
        // record of that epoch the block is held, never judged.
        let height = header.number();
        let Some(record) = self.expectations.record(height) else {
            return Err(ConsensusError::other(
                match self.expectations.scanned_through() {
                    Some(scanned) if height > scanned => {
                        SettlementError::Unscanned { height, scanned }
                    }
                    _ => SettlementError::MissingRecord { height },
                },
            ));
        };
        crate::seal::check_against_parent(
            header.header(),
            parent.header(),
            u64::from(record.epoch.time),
        )
        .map_err(ConsensusError::other)
    }
}

impl Consensus<Block> for SovaConsensus {
    fn validate_body_against_header(
        &self,
        body: &<Block as reth_ethereum::primitives::Block>::Body,
        header: &SealedHeader,
    ) -> Result<(), ConsensusError> {
        <EthBeaconConsensus<ChainSpec> as Consensus<Block>>::validate_body_against_header(
            &self.inner,
            body,
            header,
        )
    }

    fn validate_block_pre_execution(
        &self,
        block: &SealedBlock<Block>,
    ) -> Result<(), ConsensusError> {
        self.inner.validate_block_pre_execution(block)?;
        self.check_settlements(block)
    }

    fn validate_block_pre_execution_with_tx_root(
        &self,
        block: &SealedBlock<Block>,
        transaction_root: Option<TransactionRoot>,
    ) -> Result<(), ConsensusError> {
        self.inner
            .validate_block_pre_execution_with_tx_root(block, transaction_root)?;
        self.check_settlements(block)
    }

    fn is_transient_error(&self, error: &ConsensusError) -> bool {
        if let ConsensusError::Other(err) = error
            && let Some(
                SettlementError::MissingRecord { .. }
                | SettlementError::Unscanned { .. }
                | SettlementError::AnchorMismatch { .. },
            ) = err.downcast_ref()
        {
            return true;
        }
        <EthBeaconConsensus<ChainSpec> as Consensus<Block>>::is_transient_error(&self.inner, error)
    }
}

impl FullConsensus<EthPrimitives> for SovaConsensus {
    fn validate_block_post_execution(
        &self,
        block: &RecoveredBlock<Block>,
        result: &BlockExecutionResult<Receipt>,
        receipt_root_bloom: Option<ReceiptRootBloom>,
        block_access_list_hash: Option<B256>,
    ) -> Result<(), ConsensusError> {
        <EthBeaconConsensus<ChainSpec> as FullConsensus<EthPrimitives>>::validate_block_post_execution(
            &self.inner,
            block,
            result,
            receipt_root_bloom,
            block_access_list_hash,
        )
    }
}

/// Builds [`SovaConsensus`] over the process-global expectations and
/// emission schedule — the same instances the payload validator, sealer
/// and expectations feeder use, so validity has exactly one source.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct SovaConsensusBuilder;

impl<Node> ConsensusBuilder<Node> for SovaConsensusBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec, Primitives = EthPrimitives>>,
{
    type Consensus = Arc<SovaConsensus>;

    async fn build_consensus(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Consensus> {
        Ok(Arc::new(SovaConsensus::new(
            ctx.chain_spec(),
            crate::expectations::global(),
            crate::expectations::schedule,
        )))
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;
    use alloy_rpc_types::Withdrawal;
    use consensus::follower::EpochData;
    use reth_ethereum::{BlockBody, chainspec::DEV};

    use super::*;
    use crate::expectations::HeightRecord;

    fn flat() -> consensus::schedule::Schedule {
        consensus::schedule::Schedule::Flat {
            reward_gwei: crate::driver::DRAFT_EPOCH_REWARD_GWEI,
        }
    }

    fn leak() -> &'static ExpectedSettlements {
        Box::leak(Box::default())
    }

    /// Every test record's epoch closed at a Zcash block with this hash.
    const ANCHOR: [u8; 32] = [0x5A; 32];

    fn burnless_record() -> HeightRecord {
        HeightRecord {
            withdrawals: Vec::new(),
            epoch: EpochData {
                height: 0,
                hash: ANCHOR,
                burns: Vec::new(),
                time: 0,
                txs: Vec::new(),
                pools: None,
            },
            ranked: Vec::new(),
        }
    }

    fn block(height: u64, withdrawals: Vec<Withdrawal>) -> SealedBlock<Block> {
        anchored_block(height, withdrawals, Some(B256::from(ANCHOR)))
    }

    fn anchored_block(
        height: u64,
        withdrawals: Vec<Withdrawal>,
        anchor: Option<B256>,
    ) -> SealedBlock<Block> {
        let header = reth_ethereum::primitives::Header {
            number: height,
            parent_beacon_block_root: anchor,
            ..Default::default()
        };
        let body = BlockBody {
            withdrawals: Some(withdrawals.into()),
            ..Default::default()
        };
        SealedBlock::seal_slow(Block::new(header, body))
    }

    fn mint() -> Withdrawal {
        Withdrawal {
            index: 0,
            validator_index: 0,
            address: Address::with_last_byte(7),
            amount: 1,
        }
    }

    fn consensus(e: &'static ExpectedSettlements) -> SovaConsensus {
        SovaConsensus::new(DEV.clone(), e, flat)
    }

    /// A header reth's own checks accept on DEV (all forks active).
    fn dev_header(extra: &[u8]) -> reth_ethereum::primitives::Header {
        reth_ethereum::primitives::Header {
            number: 5,
            gas_limit: 30_000_000,
            base_fee_per_gas: Some(7),
            withdrawals_root: Some(crate::seal::EMPTY_ROOT),
            blob_gas_used: Some(0),
            excess_blob_gas: Some(0),
            parent_beacon_block_root: Some(B256::from(ANCHOR)),
            requests_hash: Some(alloy_primitives::b256!(
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            )),
            transactions_root: crate::seal::EMPTY_ROOT,
            mix_hash: crate::seal::pinned_randao(B256::from(ANCHOR)),
            extra_data: alloy_primitives::Bytes::copy_from_slice(extra),
            ..Default::default()
        }
    }

    /// SIP-6 §2.5: before activation the header rule is Ethereum's 32
    /// bytes; after it, exactly a null block or a valid seal.
    #[test]
    fn the_seal_rule_is_enforced_once_active() {
        let before = consensus(leak()).with_sip6(|| None);
        let after = consensus(leak()).with_sip6(|| Some(82_330));
        let plain = SealedHeader::seal_slow(dev_header(b"reth/v2.6.0"));
        let mut sealed = dev_header(b"");
        crate::seal::sign(&mut sealed, b"sova", B256::repeat_byte(0x42), 82_330)
            .unwrap_or_else(|e| panic!("{e}"));
        let sealed = SealedHeader::seal_slow(sealed);
        let null = SealedHeader::seal_slow(dev_header(b""));

        assert!(
            before.validate_header(&plain).is_ok(),
            "{:?}",
            before.validate_header(&plain)
        );
        assert!(
            before.validate_header(&sealed).is_err(),
            "97 bytes before activation"
        );
        assert!(
            after.validate_header(&sealed).is_ok(),
            "{:?}",
            after.validate_header(&sealed)
        );
        assert!(
            after.validate_header(&null).is_ok(),
            "{:?}",
            after.validate_header(&null)
        );
        assert!(
            after.validate_header(&plain).is_err(),
            "unsealed after activation"
        );
        let wrong_chain = consensus(leak()).with_sip6(|| Some(8_233));
        assert!(
            wrong_chain.validate_header(&sealed).is_ok(),
            "recovers, but another signer"
        );
    }

    #[test]
    fn nothing_scanned_defers() {
        let c = consensus(leak());
        assert!(c.check_settlements(&block(5, vec![mint()])).is_ok());
    }

    #[test]
    fn scanned_mismatch_is_permanent() {
        let e = leak();
        e.insert(5, burnless_record());
        let c = consensus(e);
        let Err(err) = c.check_settlements(&block(5, vec![mint()])) else {
            panic!("a contradicting mint must be rejected");
        };
        assert!(
            !c.is_transient_error(&err),
            "a mint contradicting our zebrad must stay invalid"
        );
        assert!(c.check_settlements(&block(5, Vec::new())).is_ok());
    }

    #[test]
    fn above_watermark_is_held() {
        let e = leak();
        e.insert(5, burnless_record());
        let c = consensus(e);
        let Err(err) = c.check_settlements(&block(6, vec![mint()])) else {
            panic!("an unscanned epoch must not be accepted on trust");
        };
        assert!(c.is_transient_error(&err), "unscanned must be a hold");
        assert!(err.to_string().contains(HOLD_MARKER));
    }

    /// History a joining node syncs: every height below the watermark is
    /// enforced, however old — nothing is pruned away from the check.
    #[test]
    fn old_history_stays_enforced() {
        let e = leak();
        for h in 1..=5_000 {
            e.insert(h, burnless_record());
        }
        let c = consensus(e);
        assert!(c.check_settlements(&block(1, vec![mint()])).is_err());
        assert!(c.check_settlements(&block(1, Vec::new())).is_ok());
    }

    #[test]
    fn missing_record_below_watermark_is_transient() {
        let e = leak();
        e.insert(5, burnless_record());
        let c = consensus(e);
        let Err(err) = c.check_settlements(&block(3, Vec::new())) else {
            panic!("a missing record below the watermark must not pass");
        };
        assert!(c.is_transient_error(&err));
    }

    #[test]
    fn zcash_reorg_lowers_watermark() {
        let e = leak();
        for h in 1..=10 {
            e.insert(h, burnless_record());
        }
        e.unwind_above(7);
        assert_eq!(e.scanned_through(), Some(7));
        let c = consensus(e);
        let Err(err) = c.check_settlements(&block(9, vec![mint()])) else {
            panic!("unwound heights must be held, not accepted");
        };
        assert!(c.is_transient_error(&err), "unwound heights are held");
    }

    /// SIP-4: a block on another Zcash fork is held, never cached invalid,
    /// even when its withdrawals also differ from our derivation (they
    /// would: its epoch saw different burns).
    #[test]
    fn anchor_mismatch_is_a_hold_even_with_foreign_withdrawals() {
        let e = leak();
        e.insert(5, burnless_record());
        let c = consensus(e);
        for anchor in [Some(B256::from([0xEE; 32])), None, Some(B256::ZERO)] {
            let Err(err) = c.check_settlements(&anchored_block(5, vec![mint()], anchor)) else {
                panic!("a block anchored to {anchor:?} must not pass");
            };
            assert!(c.is_transient_error(&err), "anchor mismatch must be a hold");
            assert!(err.to_string().contains(HOLD_MARKER));
        }
    }

    /// Same anchor, contradicting mint: now provably the same Zcash block,
    /// so the mismatch is permanent.
    #[test]
    fn matching_anchor_makes_a_mint_mismatch_permanent() {
        let e = leak();
        e.insert(5, burnless_record());
        let c = consensus(e);
        let Err(err) = c.check_settlements(&block(5, vec![mint()])) else {
            panic!("a contradicting mint must be rejected");
        };
        assert!(!c.is_transient_error(&err));
        assert!(!err.to_string().contains(HOLD_MARKER));
    }
}
