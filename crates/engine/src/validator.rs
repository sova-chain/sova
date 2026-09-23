//! Engine API validation plumbing for [`SovaEngineTypes`](crate::SovaEngineTypes).
//!
//! Mirrors reth's `examples/custom-engine-types` `CustomEngineValidator`, but
//! Sova's validation rule is the opposite of the example's: `epoch: None` is
//! accepted (vanilla behavior, unchanged from a stock Ethereum node) and any
//! well-formed `epoch: Some(_)` is accepted too. Semantic checks against the
//! Zcash follower's recognized epochs (B3b) are not this crate's job.

use std::sync::Arc;

use reth_ethereum::{
    chainspec::ChainSpec,
    node::{
        EthereumEthApiBuilder,
        api::{
            AddOnsContext, EngineApiValidator, FullNodeComponents, InvalidPayloadAttributesError,
            NewPayloadError, PayloadValidator,
            payload::{EngineApiMessageVersion, EngineObjectValidationError, PayloadOrAttributes},
            validate_version_specific_fields,
        },
        builder::rpc::{PayloadValidatorBuilder, RpcAddOns},
    },
    primitives::Block,
    rpc::types::engine::ExecutionData,
};
use reth_ethereum_payload_builder::EthereumExecutionPayloadValidator;

use crate::{SovaEngineTypes, SovaNode, SovaPayloadAttributes};

/// RPC add-ons for [`SovaNode`]: reth's standard `eth_*` namespace paired
/// with [`SovaEngineValidatorBuilder`] so `engine_*` sees Sova's payload
/// attributes type.
pub type SovaNodeAddOns<N> = RpcAddOns<N, EthereumEthApiBuilder, SovaEngineValidatorBuilder>;

/// Validates Sova payloads and payload attributes for the Engine API.
///
/// Payload (block) validation is delegated unchanged to reth's stock
/// [`EthereumExecutionPayloadValidator`] — B3a introduces no new block
/// format. Only payload *attributes* validation is Sova-specific.
#[derive(Debug, Clone)]
pub struct SovaEngineValidator {
    inner: EthereumExecutionPayloadValidator<ChainSpec>,
}

impl SovaEngineValidator {
    /// Instantiates a new validator for the given chain spec.
    #[must_use]
    pub const fn new(chain_spec: Arc<ChainSpec>) -> Self {
        Self {
            inner: EthereumExecutionPayloadValidator::new(chain_spec),
        }
    }

    /// Returns the chain spec used by the validator.
    #[inline]
    fn chain_spec(&self) -> &ChainSpec {
        self.inner.chain_spec()
    }

    /// reth's payload well-formedness checks, with room for a SIP-6 seal.
    ///
    /// alloy's payload conversion rejects `extra_data` over 32 bytes and is
    /// not configurable (SIP-6 §2.1), but a sealed block carries 97. So a
    /// 97-byte payload is converted with its `extra_data` cut to the
    /// vanity — reth's checks run unchanged on that — and the seal is then
    /// put back and the result checked against the payload's own block
    /// hash. Whether a seal is *allowed* (activation) and valid is
    /// `SovaConsensus`'s header rule, not this function's.
    fn well_formed(
        &self,
        payload: ExecutionData,
    ) -> Result<reth_ethereum::primitives::SealedBlock<reth_ethereum::Block>, NewPayloadError> {
        use crate::seal::{SEALED_EXTRA_LEN, VANITY_LEN};
        let full = payload.payload.as_v1().extra_data.clone();
        if full.len() != SEALED_EXTRA_LEN {
            return self
                .inner
                .ensure_well_formed_payload(payload)
                .map_err(Into::into);
        }
        let expected = payload.payload.block_hash();
        let ExecutionData {
            mut payload,
            sidecar,
        } = payload;
        payload.as_v1_mut().extra_data =
            alloy_primitives::Bytes::copy_from_slice(&full[..VANITY_LEN]);
        // The vanity-only block's hash, so reth's own hash check passes.
        let unsealed = reth_ethereum::primitives::SealedBlock::seal_slow(
            payload
                .clone()
                .try_into_block_with_sidecar::<reth_ethereum::TransactionSigned>(&sidecar)
                .map_err(Into::<NewPayloadError>::into)?,
        );
        payload.as_v1_mut().block_hash = unsealed.hash();
        let checked = self
            .inner
            .ensure_well_formed_payload(ExecutionData { payload, sidecar })
            .map_err(Into::<NewPayloadError>::into)?;
        let mut block = checked.into_block();
        block.header.extra_data = full;
        let sealed = reth_ethereum::primitives::SealedBlock::seal_slow(block);
        if sealed.hash() != expected {
            return Err(reth_ethereum::rpc::types::engine::PayloadError::BlockHash {
                execution: sealed.hash(),
                consensus: expected,
            }
            .into());
        }
        Ok(sealed)
    }
}

impl PayloadValidator<SovaEngineTypes> for SovaEngineValidator {
    type Block = reth_ethereum::Block;

    fn convert_payload_to_block(
        &self,
        payload: ExecutionData,
    ) -> Result<reth_ethereum::primitives::SealedBlock<Self::Block>, NewPayloadError> {
        let block = self.well_formed(payload)?;

        // C5 (v2): re-derive the height's settlements from our own Zcash
        // view; the block is valid when its withdrawals match some rank's
        // derivation, and the recovered rank feeds candidate preference
        // (see crate::expectations and crate::candidates).
        use consensus::sealer::Candidate;

        use crate::candidates::{self, BestCandidate, Observation};
        use crate::expectations::{AnchorVerdict, RankedVerdict, global, schedule};
        let height = block.number;
        // SIP-4: a block anchored to a Zcash block our follower doesn't
        // have at its epoch is not a candidate, and its withdrawals say
        // nothing permanent (they derive from a different Zcash block).
        // It is passed on untouched so SovaConsensus returns the hold.
        if let AnchorVerdict::Mismatch { .. } =
            global().check_anchor(height, block.header().parent_beacon_block_root)
        {
            tracing::debug!(height, hash = %block.hash(), "zcash anchor mismatch; not a candidate");
            return Ok(block);
        }
        let withdrawals = block.body().withdrawals.as_deref().map(|w| w.as_slice());
        // Every accepted block becomes a fork-choice candidate — since
        // v2 the relay never FCUs a peer, so observation → arbiter is
        // the ONLY way an imported block can become a receiver's head.
        //
        // Rank assignment:
        // - A recovered sealer rank competes as itself.
        // - Burn-less epochs have no rank; their empty blocks compete
        //   at `usize::MAX` so the hash tiebreak converges concurrent
        //   producers. (An empty candidate can never outrank a settled
        //   one — burn-bearing epochs reject empty withdrawals above.)
        // - Unknown heights (not yet scanned by our follower) also
        //   compete at `usize::MAX`: trust-shaped liveness — the first
        //   arrival advances the head — and are re-ranked with their
        //   real sealer rank once our scan reaches the height
        //   (`CandidateTracker::rerank`, driven by the expectations feed).
        // SIP-6 §2.5: verify the seal before the block can be observed, so a
        // forged copy never takes the epoch's best slot even for a moment.
        let sealer = crate::seal::sealer(block.header(), crate::seal::active_chain_id())
            .map_err(|e| NewPayloadError::Other(e.to_string().into()))?;
        let observed_rank = match global().check_sealed(height, withdrawals, schedule(), sealer) {
            RankedVerdict::Valid { rank } => Some(rank),
            RankedVerdict::ValidEmpty => Some(usize::MAX),
            RankedVerdict::Unknown => {
                tracing::debug!(height, "no settlement expectation yet; accepting on trust");
                None
            }
            RankedVerdict::Mismatch { height } => {
                return Err(NewPayloadError::Other(
                    format!(
                        "settlement mismatch at height {height}: withdrawals match no rank's local Zcash derivation"
                    )
                    .into(),
                ));
            }
        };
        // Equivocation evidence can demote another signer's block and make
        // a different candidate best, so compare the height's best around
        // the observation rather than only asking about this block.
        let best_before = candidates::global().best(height).map(|c| c.block_hash);
        let observation = match (observed_rank, sealer) {
            (Some(sealer_rank), crate::seal::Sealer::Signed(signer)) => candidates::global()
                .observe_sealed(
                    height,
                    Candidate {
                        sealer_rank,
                        block_hash: block.hash().0,
                    },
                    block.parent_hash.0,
                    candidates::SealInfo {
                        signer: signer.0.0,
                        anchor: block
                            .header()
                            .parent_beacon_block_root
                            .unwrap_or_default()
                            .0,
                        seal_hash: crate::seal::seal_hash(block.header()).0,
                    },
                ),
            (Some(sealer_rank), _) => candidates::global().observe(
                height,
                Candidate {
                    sealer_rank,
                    block_hash: block.hash().0,
                },
                block.parent_hash.0,
            ),
            (None, _) => candidates::global().observe_unranked(
                height,
                block.hash().0,
                block.parent_hash.0,
                withdrawals.map(<[_]>::to_vec).unwrap_or_default(),
            ),
        };
        let best_after = candidates::global().best(height).map(|c| c.block_hash);
        if observation == Observation::NewBest {
            candidates::notify_best(BestCandidate {
                sova_height: height,
                block_hash: block.hash().0,
            });
        } else if let Some(best) = best_after
            && best_after != best_before
        {
            candidates::notify_best(BestCandidate {
                sova_height: height,
                block_hash: best,
            });
        }
        Ok(block)
    }

    fn validate_payload_attributes_against_header(
        &self,
        _attr: &SovaPayloadAttributes,
        _header: &<Self::Block as Block>::Header,
    ) -> Result<(), InvalidPayloadAttributesError> {
        // No Sova-specific header/attribute cross-checks yet (matches reth's
        // own example here); B3b is where epoch attributes get validated
        // against actually-recognized Zcash epochs.
        Ok(())
    }
}

impl EngineApiValidator<SovaEngineTypes> for SovaEngineValidator {
    fn validate_version_specific_fields(
        &self,
        version: EngineApiMessageVersion,
        payload_or_attrs: PayloadOrAttributes<'_, ExecutionData, SovaPayloadAttributes>,
    ) -> Result<(), EngineObjectValidationError> {
        validate_version_specific_fields(self.chain_spec(), version, payload_or_attrs)
    }

    fn ensure_well_formed_attributes(
        &self,
        version: EngineApiMessageVersion,
        attributes: &SovaPayloadAttributes,
    ) -> Result<(), EngineObjectValidationError> {
        validate_version_specific_fields(
            self.chain_spec(),
            version,
            PayloadOrAttributes::<ExecutionData, SovaPayloadAttributes>::PayloadAttributes(
                attributes,
            ),
        )?;

        // `epoch: None` is vanilla behavior; a present `epoch` needs no
        // additional structural checks beyond what serde already enforced
        // deserializing it (SIP-1 burn recognition happens upstream, in the
        // Zcash follower — this validator only guards the wire format).
        Ok(())
    }
}

/// Builds [`SovaEngineValidator`]s for a launched [`SovaNode`].
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct SovaEngineValidatorBuilder;

impl<N> PayloadValidatorBuilder<N> for SovaEngineValidatorBuilder
where
    N: FullNodeComponents<Types = SovaNode>,
{
    type Validator = SovaEngineValidator;

    async fn build(self, ctx: &AddOnsContext<'_, N>) -> eyre::Result<Self::Validator> {
        Ok(SovaEngineValidator::new(ctx.config.chain.clone()))
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, U256};

    use super::*;
    use crate::SovaEpochAttribute;

    fn validator() -> SovaEngineValidator {
        SovaEngineValidator::new(Arc::new(ChainSpec::default()))
    }

    fn base_attrs() -> SovaPayloadAttributes {
        SovaPayloadAttributes {
            inner: alloy_rpc_types::engine::PayloadAttributes {
                timestamp: 1,
                prev_randao: B256::ZERO,
                suggested_fee_recipient: Address::ZERO,
                withdrawals: None,
                parent_beacon_block_root: None,
                slot_number: None,
                ..Default::default()
            },
            epoch: None,
        }
    }

    /// A pre-Shanghai block (V1 payload) with `extra` as its extra data.
    fn block_with_extra(extra: &[u8]) -> reth_ethereum::Block {
        let mut block = reth_ethereum::Block::default();
        block.header.number = 1;
        block.header.timestamp = 1;
        block.header.gas_limit = 30_000_000;
        block.header.base_fee_per_gas = Some(7);
        block.header.extra_data = alloy_primitives::Bytes::copy_from_slice(extra);
        block
    }

    fn payload_of(block: reth_ethereum::Block) -> ExecutionData {
        use reth_ethereum::node::api::PayloadTypes;
        crate::SovaEngineTypes::block_to_payload(
            reth_ethereum::primitives::SealedBlock::seal_slow(block),
            None,
        )
    }

    /// SIP-6 §2.1/§8: a 97-byte sealed block survives block → payload →
    /// block with its hash, seal and signer unchanged; a tampered seal
    /// fails the payload's own hash check; other lengths over 32 still
    /// fail alloy's conversion.
    #[test]
    fn a_sealed_block_round_trips_through_the_payload_conversion() {
        let mut block = block_with_extra(b"");
        crate::seal::sign(&mut block.header, b"sova", B256::repeat_byte(0x42), 82_330)
            .unwrap_or_else(|e| panic!("{e}"));
        let hash = block.header.hash_slow();
        let converted = validator()
            .well_formed(payload_of(block.clone()))
            .unwrap_or_else(|e| panic!("sealed payload rejected: {e}"));
        assert_eq!(converted.hash(), hash);
        assert_eq!(converted.header().extra_data, block.header.extra_data);
        assert_eq!(
            crate::seal::recover(converted.header(), 82_330),
            crate::seal::address_of(B256::repeat_byte(0x42))
        );

        let mut tampered = payload_of(block);
        let mut extra = tampered.payload.as_v1().extra_data.to_vec();
        extra[40] ^= 1;
        tampered.payload.as_v1_mut().extra_data = extra.into();
        assert!(
            validator().well_formed(tampered).is_err(),
            "a changed seal changes the hash"
        );

        assert!(
            validator()
                .well_formed(payload_of(block_with_extra(&[1; 33])))
                .is_err()
        );
        assert!(
            validator()
                .well_formed(payload_of(block_with_extra(&[1; 98])))
                .is_err()
        );
        assert!(
            validator()
                .well_formed(payload_of(block_with_extra(b"sova")))
                .is_ok()
        );
    }

    /// `epoch: None` is vanilla behavior and must validate cleanly.
    #[test]
    fn accepts_no_epoch() {
        let attrs = base_attrs();
        let result = validator().ensure_well_formed_attributes(EngineApiMessageVersion::V1, &attrs);
        assert!(
            result.is_ok(),
            "expected None epoch to validate, got {result:?}"
        );
    }

    /// A well-formed `epoch: Some(_)` must also validate cleanly — Sova does
    /// not reject epoch-bearing attributes at this layer.
    #[test]
    fn accepts_well_formed_epoch() {
        let mut attrs = base_attrs();
        attrs.epoch = Some(SovaEpochAttribute {
            zcash_height: 7,
            zcash_hash: [0x11; 32],
            settlements: vec![(Address::with_last_byte(9), U256::from(1u64))],
            zcash_time: 0,
            null: false,
        });
        let result = validator().ensure_well_formed_attributes(EngineApiMessageVersion::V1, &attrs);
        assert!(
            result.is_ok(),
            "expected well-formed Some(epoch) to validate, got {result:?}"
        );
    }
}
