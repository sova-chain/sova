//! [`SovaNode`]: the `NodeTypes`/`Node` preset that assembles a Sova node —
//! stock Ethereum pool component, reth's Ethereum network with an optional
//! `sova/1` registration ([`crate::p2p::SovaNetworkBuilder`]), Sova's consensus,
//! Sova's executor seam
//! ([`evm::SovaExecutorBuilder`]), Sova's payload builder
//! ([`crate::SovaPayloadBuilderBuilder`]), and Sova's engine types
//! ([`SovaEngineTypes`]) — the same shape as reth's `examples/custom-engine-types`
//! `MyCustomNode`, wired for Sova.

use std::sync::Arc;

use evm::SovaExecutorBuilder;
use reth_ethereum::{
    EthPrimitives,
    node::{
        api::{
            FullNodeComponents, FullNodeTypes, NodeTypes, PayloadAttributesBuilder, PayloadTypes,
        },
        builder::{
            DebugNode, Node, NodeAdapter,
            components::{BasicPayloadServiceBuilder, ComponentsBuilder},
        },
        node::EthereumPoolBuilder,
    },
    provider::EthStorage,
};

use crate::{
    SovaConsensusBuilder, SovaEngineTypes, SovaLocalPayloadAttributesBuilder, SovaNodeAddOns,
    SovaPayloadBuilderBuilder, p2p::SovaNetworkBuilder,
};

/// Sova's node preset: swaps in [`SovaEngineTypes`] (custom payload
/// attributes) and [`SovaExecutorBuilder`] (the settlement/precompile
/// executor seam) and [`SovaConsensusBuilder`] (C5 on every import path)
/// while keeping reth's stock pool component unchanged. The network is
/// [`SovaNetworkBuilder`]: by default exactly reth's Ethereum network;
/// `bin/sova` swaps in one carrying the `sova/1` handler in p2p mode.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SovaNode;

impl NodeTypes for SovaNode {
    type Primitives = EthPrimitives;
    type ChainSpec = reth_ethereum::chainspec::ChainSpec;
    type Storage = EthStorage;
    type Payload = SovaEngineTypes;
}

impl<N> Node<N> for SovaNode
where
    N: FullNodeTypes<Types = Self>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        EthereumPoolBuilder,
        BasicPayloadServiceBuilder<SovaPayloadBuilderBuilder>,
        SovaNetworkBuilder,
        SovaExecutorBuilder,
        SovaConsensusBuilder,
    >;
    type AddOns = SovaNodeAddOns<NodeAdapter<N>>;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(EthereumPoolBuilder::default())
            .executor(SovaExecutorBuilder::default())
            .payload(BasicPayloadServiceBuilder::default())
            .network(SovaNetworkBuilder::default())
            .consensus(SovaConsensusBuilder)
    }

    fn add_ons(&self) -> Self::AddOns {
        SovaNodeAddOns::default()
    }
}

impl<N: FullNodeComponents<Types = Self>> DebugNode<N> for SovaNode {
    type RpcBlock = alloy_rpc_types_eth::Block;

    fn rpc_to_primitive_block(rpc_block: Self::RpcBlock) -> reth_ethereum::Block {
        rpc_block.into_consensus().convert_transactions()
    }

    fn local_payload_attributes_builder(
        chain_spec: &Self::ChainSpec,
    ) -> impl PayloadAttributesBuilder<<Self::Payload as PayloadTypes>::PayloadAttributes> {
        SovaLocalPayloadAttributesBuilder::new(Arc::new(chain_spec.clone()))
    }
}
