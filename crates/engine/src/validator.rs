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
}

impl PayloadValidator<SovaEngineTypes> for SovaEngineValidator {
    type Block = reth_ethereum::Block;

    fn convert_payload_to_block(
        &self,
        payload: ExecutionData,
    ) -> Result<reth_ethereum::primitives::SealedBlock<Self::Block>, NewPayloadError> {
        let block = self
            .inner
            .ensure_well_formed_payload(payload)
            .map_err(Into::<NewPayloadError>::into)?;

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
        let observed_rank = match global().check_ranked(height, withdrawals, schedule()) {
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
        let observation = match observed_rank {
            Some(sealer_rank) => candidates::global().observe(
                height,
                Candidate {
                    sealer_rank,
                    block_hash: block.hash().0,
                },
            ),
            None => candidates::global().observe_unranked(
                height,
                block.hash().0,
                withdrawals.map(<[_]>::to_vec).unwrap_or_default(),
            ),
        };
        if observation == Observation::NewBest {
            candidates::notify_best(BestCandidate {
                sova_height: height,
                block_hash: block.hash().0,
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
        });
        let result = validator().ensure_well_formed_attributes(EngineApiMessageVersion::V1, &attrs);
        assert!(
            result.is_ok(),
            "expected well-formed Some(epoch) to validate, got {result:?}"
        );
    }
}
