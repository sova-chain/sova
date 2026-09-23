//! Sova EVM extensions: settlement transactions, Zcash query precompile.
//!
//! This crate will hold Sova's extensions to the EVM execution layer,
//! including settlement transaction types and the precompile used to query
//! Zcash chain state from EVM contracts. It is currently a build-scaffold
//! stub, aside from [`SovaExecutorBuilder`] (a B1-spike proof of the
//! executor seam Sova's extensions will eventually hook into).

use reth_ethereum::{
    EthPrimitives,
    chainspec::ChainSpec,
    node::{
        EthereumExecutorBuilder,
        api::{FullNodeTypes, NodeTypes},
        builder::{BuilderContext, components::ExecutorBuilder},
    },
};

/// Returns the name of this crate, used as a placeholder smoke test until
/// real EVM extension types land.
#[must_use]
pub fn crate_name() -> &'static str {
    "evm"
}

/// A no-op executor builder that delegates straight to reth's stock
/// [`EthereumExecutorBuilder`].
///
/// This proves out the `ComponentsBuilder::executor(...)` seam that Sova's
/// settlement-transaction and Zcash-query-precompile EVM extensions will
/// eventually plug into (see the crate-level docs); for now it changes no
/// execution behavior at all.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct SovaExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for SovaExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec, Primitives = EthPrimitives>>,
{
    type EVM = <EthereumExecutorBuilder as ExecutorBuilder<Node>>::EVM;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        EthereumExecutorBuilder::default().build_evm(ctx).await
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
