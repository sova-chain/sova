//! The payload builder that turns [`SovaPayloadAttributes`] into a built
//! block.
//!
//! For B3a this is a thin pass-through to reth's stock
//! [`EthereumPayloadBuilder`]: `try_build`/`build_empty_payload` receive the
//! full `SovaPayloadAttributes` (so the `epoch` field is visible here, which
//! is what `crates/engine/tests/payload_roundtrip.rs` proves), then strip
//! `.inner` before delegating. B3b will insert the settlement-mint logic
//! right here, before that delegation.

use reth_basic_payload_builder::{BuildArguments, BuildOutcome, PayloadBuilder, PayloadConfig};
use reth_ethereum::{
    EthPrimitives, TransactionSigned,
    chainspec::{ChainSpec, ChainSpecProvider},
    evm::primitives::{ConfigureEvm, NextBlockEnvAttributes},
    node::{
        api::{FullNodeTypes, NodeTypes},
        builder::{BuilderContext, PayloadBuilderConfig, components::PayloadBuilderBuilder},
    },
    pool::{PoolTransaction, TransactionPool},
    provider::StateProviderFactory,
};
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_payload_builder::{EthBuiltPayload, PayloadBuilderError};

use alloy_primitives::B256;
use alloy_rpc_types::engine::PayloadAttributes as EthPayloadAttributes;

use crate::{SovaEngineTypes, SovaPayloadAttributes, settlements_to_withdrawals};

/// Builds a [`SovaPayloadBuilder`] as the node's `payload` component.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct SovaPayloadBuilderBuilder;

impl<Node, Pool, Evm> PayloadBuilderBuilder<Node, Pool, Evm> for SovaPayloadBuilderBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypes<
            Payload = SovaEngineTypes,
            ChainSpec = ChainSpec,
            Primitives = EthPrimitives,
        >,
    >,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>
        + Unpin
        + 'static,
    Evm: ConfigureEvm<Primitives = EthPrimitives, NextBlockEnvCtx = NextBlockEnvAttributes>
        + 'static,
{
    type PayloadBuilder = SovaPayloadBuilder<Pool, Node::Provider, Evm>;

    async fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm_config: Evm,
    ) -> eyre::Result<Self::PayloadBuilder> {
        Ok(SovaPayloadBuilder {
            inner: reth_ethereum_payload_builder::EthereumPayloadBuilder::new(
                ctx.provider().clone(),
                pool,
                evm_config,
                EthereumBuilderConfig::new()
                    .with_extra_data(ctx.payload_builder_config().extra_data()),
            ),
        })
    }
}

/// Builds payloads for [`SovaEngineTypes`], receiving the full
/// [`SovaPayloadAttributes`] (epoch field included) before delegating block
/// construction to reth's stock [`EthereumPayloadBuilder`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SovaPayloadBuilder<Pool, Client, Evm> {
    inner: reth_ethereum_payload_builder::EthereumPayloadBuilder<Pool, Client, Evm>,
}

impl<Pool, Client, Evm> PayloadBuilder for SovaPayloadBuilder<Pool, Client, Evm>
where
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = ChainSpec> + Clone,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
    Evm: ConfigureEvm<Primitives = EthPrimitives, NextBlockEnvCtx = NextBlockEnvAttributes>,
{
    type Attributes = SovaPayloadAttributes;
    type BuiltPayload = EthBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        let BuildArguments {
            cached_reads,
            execution_cache,
            state_root_handle,
            config,
            cancel,
            best_payload,
        } = args;
        let PayloadConfig {
            parent_header,
            parent_block_info,
            attributes,
            payload_id,
        } = config;

        // B3b: an epoch-bearing build request mints its settlements through
        // the withdrawals channel — Ethereum's consensus-grade balance
        // increment, applied by reth's stock executor with no custom code.
        let inner_attributes = apply_epoch_settlements(attributes)?;
        self.inner.try_build(BuildArguments {
            cached_reads,
            execution_cache,
            state_root_handle,
            config: PayloadConfig {
                parent_header,
                parent_block_info,
                attributes: inner_attributes,
                payload_id,
            },
            cancel,
            best_payload,
        })
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        let PayloadConfig {
            parent_header,
            parent_block_info,
            attributes,
            payload_id,
        } = config;
        let inner_attributes = apply_epoch_settlements(attributes)?;
        self.inner.build_empty_payload(PayloadConfig {
            parent_header,
            parent_block_info,
            attributes: inner_attributes,
            payload_id,
        })
    }
}

/// Fold an epoch's settlements into the inner Ethereum attributes as
/// withdrawals (SIP-2 rule), or pass vanilla attributes through untouched.
///
/// Consistency is strict: with `epoch: Some(_)`, the caller must either
/// leave `withdrawals` unset/empty (the derivation is injected) or carry
/// exactly the derived list already (idempotent re-validation) — anything
/// else is a tampered build request and fails the job. The same holds for
/// the SIP-4 anchor: `parent_beacon_block_root` is set to the epoch's
/// Zcash hash, and a request already carrying a different non-zero root
/// fails.
fn apply_epoch_settlements(
    attributes: SovaPayloadAttributes,
) -> Result<EthPayloadAttributes, PayloadBuilderError> {
    let SovaPayloadAttributes { mut inner, epoch } = attributes;
    let Some(epoch) = epoch else {
        return Ok(inner);
    };
    let tampered = |what| {
        PayloadBuilderError::Other(Box::new(SettlementTamperError {
            what,
            zcash_height: epoch.zcash_height,
        }))
    };
    let anchor = B256::from(epoch.zcash_hash);
    match inner.parent_beacon_block_root {
        None => inner.parent_beacon_block_root = Some(anchor),
        Some(root) if root.is_zero() => inner.parent_beacon_block_root = Some(anchor),
        Some(root) if root == anchor => {}
        Some(_) => return Err(tampered("the zcash anchor")),
    }
    let derived =
        settlements_to_withdrawals(&epoch).map_err(|e| PayloadBuilderError::Other(Box::new(e)))?;
    match inner.withdrawals.as_deref() {
        None | Some([]) => {
            inner.withdrawals = Some(derived);
            Ok(inner)
        }
        Some(existing) if existing == derived.as_slice() => Ok(inner),
        Some(_) => Err(tampered("withdrawals")),
    }
}

/// A build request whose carried withdrawals or anchor contradict its
/// claimed epoch.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{what} do not match zcash epoch {zcash_height}: tampered build request")]
struct SettlementTamperError {
    /// Which part of the request contradicts the epoch.
    what: &'static str,
    /// The claimed epoch's Zcash height.
    zcash_height: u64,
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, U256};
    use alloy_rpc_types::Withdrawal;

    use super::*;
    use crate::{SETTLEMENT_VALIDATOR_INDEX, SovaEpochAttribute};

    fn attrs(epoch: Option<SovaEpochAttribute>) -> SovaPayloadAttributes {
        SovaPayloadAttributes {
            inner: EthPayloadAttributes {
                timestamp: 1,
                prev_randao: B256::ZERO,
                suggested_fee_recipient: Address::ZERO,
                withdrawals: None,
                parent_beacon_block_root: None,
                slot_number: None,
                ..Default::default()
            },
            epoch,
        }
    }

    fn gwei(n: u64) -> U256 {
        U256::from(n) * U256::from(1_000_000_000u64)
    }

    fn epoch(settlements: Vec<(Address, U256)>) -> SovaEpochAttribute {
        SovaEpochAttribute {
            zcash_height: 42,
            zcash_hash: [0x22; 32],
            settlements,
        }
    }

    #[test]
    fn vanilla_attributes_pass_through() {
        let out = apply_epoch_settlements(attrs(None)).unwrap_or_else(|e| panic!("{e}"));
        assert!(out.withdrawals.is_none());
    }

    #[test]
    fn epoch_settlements_become_withdrawals() {
        let a = Address::with_last_byte(1);
        let b = Address::with_last_byte(2);
        let out = apply_epoch_settlements(attrs(Some(epoch(vec![(a, gwei(900)), (b, gwei(100))]))))
            .unwrap_or_else(|e| panic!("{e}"));
        let w = out.withdrawals.unwrap_or_default();
        assert_eq!(
            w,
            vec![
                Withdrawal {
                    index: 0,
                    validator_index: SETTLEMENT_VALIDATOR_INDEX,
                    address: a,
                    amount: 900,
                },
                Withdrawal {
                    index: 1,
                    validator_index: SETTLEMENT_VALIDATOR_INDEX,
                    address: b,
                    amount: 100,
                },
            ]
        );
        // Conservation at the withdrawal layer: gwei sum equals the input
        // sum expressed in gwei.
        let total: u128 = w.iter().map(|w| u128::from(w.amount)).sum();
        assert_eq!(total, 1_000u128);
    }

    #[test]
    fn matching_precarried_withdrawals_are_idempotent() {
        let a = Address::with_last_byte(1);
        let e = epoch(vec![(a, gwei(5))]);
        let derived = settlements_to_withdrawals(&e).unwrap_or_else(|err| panic!("{err}"));
        let mut request = attrs(Some(e));
        request.inner.withdrawals = Some(derived.clone());
        let out = apply_epoch_settlements(request).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(out.withdrawals.unwrap_or_default(), derived);
    }

    #[test]
    fn tampered_withdrawals_fail_the_build() {
        let a = Address::with_last_byte(1);
        let attacker = Address::with_last_byte(0xEE);
        let mut request = attrs(Some(epoch(vec![(a, gwei(5))])));
        request.inner.withdrawals = Some(vec![Withdrawal {
            index: 0,
            validator_index: SETTLEMENT_VALIDATOR_INDEX,
            address: attacker,
            amount: 5,
        }]);
        assert!(apply_epoch_settlements(request).is_err());
    }

    #[test]
    fn epoch_sets_the_zcash_anchor() {
        let out = apply_epoch_settlements(attrs(Some(epoch(Vec::new()))))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out.parent_beacon_block_root, Some(B256::from([0x22; 32])));
        assert_eq!(
            out.withdrawals,
            Some(Vec::new()),
            "cadence block mints nothing"
        );

        let mut zeroed = attrs(Some(epoch(Vec::new())));
        zeroed.inner.parent_beacon_block_root = Some(B256::ZERO);
        let out = apply_epoch_settlements(zeroed).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out.parent_beacon_block_root, Some(B256::from([0x22; 32])));
    }

    #[test]
    fn contradicting_anchor_fails_the_build() {
        let mut request = attrs(Some(epoch(Vec::new())));
        request.inner.parent_beacon_block_root = Some(B256::from([0x99; 32]));
        assert!(apply_epoch_settlements(request).is_err());
    }

    #[test]
    fn misaligned_zero_and_overflow_settlements_fail() {
        let a = Address::with_last_byte(1);
        for bad in [
            U256::from(1u64),      // not gwei-aligned
            U256::ZERO,            // zero mint
            U256::from(u128::MAX), // overflows u64 gwei
        ] {
            assert!(
                apply_epoch_settlements(attrs(Some(epoch(vec![(a, bad)])))).is_err(),
                "expected failure for {bad}"
            );
        }
    }
}
