//! [`SovaEngineTypes`]: the `PayloadTypes`/`EngineTypes` pair that tells
//! reth's Engine API to use [`SovaPayloadAttributes`](crate::SovaPayloadAttributes)
//! wherever it would otherwise use the stock Ethereum payload attributes.
//!
//! The block/payload wire formats themselves are unchanged from vanilla
//! Ethereum (`ExecutionData`, `EthBuiltPayload`, the standard V1-V6 envelope
//! types) — only the attributes type passed into `engine_forkchoiceUpdated`
//! and threaded through to the payload builder is Sova-specific.

use alloy_primitives::Bytes;
use alloy_rpc_types::engine::{
    ExecutionPayloadEnvelopeV2, ExecutionPayloadEnvelopeV3, ExecutionPayloadEnvelopeV4,
    ExecutionPayloadEnvelopeV5, ExecutionPayloadEnvelopeV6, ExecutionPayloadV1,
};
use reth_ethereum::{
    node::api::{BuiltPayload, EngineTypes, NodePrimitives, PayloadTypes},
    primitives::SealedBlock,
    rpc::types::engine::{ExecutionData, ExecutionPayload},
};
use reth_payload_builder::EthBuiltPayload;
use serde::{Deserialize, Serialize};

use crate::SovaPayloadAttributes;

/// Sova's Engine API type set: a custom payload *attributes* RPC type
/// ([`SovaPayloadAttributes`]), with everything else (execution data, built
/// payload, envelope versions) identical to stock Ethereum.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SovaEngineTypes;

impl PayloadTypes for SovaEngineTypes {
    type ExecutionData = ExecutionData;
    type BuiltPayload = EthBuiltPayload;
    type PayloadAttributes = SovaPayloadAttributes;

    fn block_to_payload(
        block: SealedBlock<
            <<Self::BuiltPayload as BuiltPayload>::Primitives as NodePrimitives>::Block,
        >,
        _bal: Option<Bytes>,
    ) -> ExecutionData {
        let (payload, sidecar) =
            ExecutionPayload::from_block_unchecked(block.hash(), &block.into_block());
        ExecutionData { payload, sidecar }
    }
}

impl EngineTypes for SovaEngineTypes {
    type ExecutionPayloadEnvelopeV1 = ExecutionPayloadV1;
    type ExecutionPayloadEnvelopeV2 = ExecutionPayloadEnvelopeV2;
    type ExecutionPayloadEnvelopeV3 = ExecutionPayloadEnvelopeV3;
    type ExecutionPayloadEnvelopeV4 = ExecutionPayloadEnvelopeV4;
    type ExecutionPayloadEnvelopeV5 = ExecutionPayloadEnvelopeV5;
    type ExecutionPayloadEnvelopeV6 = ExecutionPayloadEnvelopeV6;
}
