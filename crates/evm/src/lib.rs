//! Sova EVM extensions: settlement transactions, Zcash query precompile.
//!
//! This crate holds Sova's extensions to the EVM execution layer. Today
//! that is [`SovaExecutorBuilder`], which gives the node an
//! [`EthEvmConfig`] built on [`zcash::SovaEvmFactory`]: reth's Ethereum EVM
//! plus the SIP-4 Zcash query precompile at [`zcash::ZCASH_QUERY`]
//! (research spike: only `anchor()` is implemented; see
//! `docs/design/sip4-evm-seam.md`).

pub mod zcash;

use reth_ethereum::{
    EthPrimitives,
    chainspec::ChainSpec,
    evm::EthEvmConfig,
    node::{
        api::{FullNodeTypes, NodeTypes},
        builder::{BuilderContext, components::ExecutorBuilder},
    },
};

use crate::zcash::SovaEvmFactory;

/// Returns the name of this crate, used as a placeholder smoke test until
/// real EVM extension types land.
#[must_use]
pub fn crate_name() -> &'static str {
    "evm"
}

/// Sova's executor builder: reth's Ethereum block executor over
/// [`SovaEvmFactory`].
///
/// The one [`EthEvmConfig`] it returns is what reth hands to every
/// execution path — engine import (`payload_validator.rs`), the payload
/// builder, the backfill pipeline, and the RPC `eth_call` /
/// `eth_estimateGas` / tracing helpers — so the precompile is present
/// everywhere the EVM runs.
///
/// Differences from reth's stock `EthereumExecutorBuilder` (v2.6.0,
/// `ethereum/node/src/node.rs:617-656`): the stock builder uses
/// `RethEvmFactory`, which without the `jit` cargo feature (not enabled
/// in this workspace) is a newtype over the same `EthEvmFactory` this
/// factory wraps; the sender-recovery cache is carried over below.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct SovaExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for SovaExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec, Primitives = EthPrimitives>>,
{
    type EVM = EthEvmConfig<ChainSpec, SovaEvmFactory>;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        if ctx.config().jit.enabled {
            eyre::bail!("sova: --jit is not supported (the Sova EVM factory has no JIT path)");
        }
        let mut evm_config =
            EthEvmConfig::new_with_evm_factory(ctx.chain_spec(), SovaEvmFactory::default());
        if let Some(cache) = ctx.sender_recovery_cache() {
            evm_config = evm_config.with_sender_recovery_cache(cache.clone());
        }
        Ok(evm_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_crate_name() {
        assert_eq!(crate_name(), "evm");
    }
}
