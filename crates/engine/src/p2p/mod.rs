//! `sova/1`: Sova's RLPx sub-protocol for block propagation
//! (`docs/design/p2p-m1.md`, "Propagation"; board m1-b step 3).
//!
//! Replaces gossip v1's JWT-authenticated authrpc relay ([`crate::relay`])
//! with an unprivileged channel that can face strangers:
//!
//! - [`codec`] — three messages, `Announce { height, hash }`,
//!   `GetBlock { hash }`, `Block { rlp }`. None of them can move a head.
//! - [`protocol`] — the reth [`ProtocolHandler`](reth_ethereum::network::protocol::ProtocolHandler)
//!   and per-connection streams with inbound limits (the Stake-mode
//!   network guards stay on; no `.with_pow()`).
//! - [`network`] — [`SovaNetworkBuilder`], which registers the handler in
//!   the network *config* so every session — including ones discovery
//!   dials, or inbound ones, before `launch` returns — offers `sova/1`
//!   (m1-b step 4; step 3 registered after launch).
//! - [`service`] — the per-node task: announce own/accepted heads, pull
//!   announced bodies, submit them **in-process** through
//!   `ConsensusEngineHandle::new_payload`, chase `SYNCING` parents
//!   (bounded), and dedup. It never calls `fork_choice_updated`: relay
//!   delivers, the local arbiter decides.
//!
//! [`RethGossipBackend`] adapts a launched node (provider + engine handle +
//! network handle) to the service's [`GossipBackend`] seam.

pub mod codec;
pub mod network;
pub mod protocol;
pub mod service;

use std::{future::Future, time::Duration};

use alloy_primitives::{B256, Bytes};
use alloy_rlp::Encodable;
use alloy_rpc_types::engine::PayloadStatusEnum;
use reth_ethereum::{
    network::{
        Peers,
        api::{PeerId, ReputationChangeKind},
    },
    node::api::{ConsensusEngineHandle, PayloadTypes},
    primitives::SealedBlock,
    storage::{BlockHashReader, BlockNumReader, BlockReader},
};

pub use network::SovaNetworkBuilder;
pub use protocol::{ServiceReceivers, SovaProtocolHandler};
pub use service::{GossipBackend, GossipService};

use crate::SovaEngineTypes;

/// Builds the `sova/1` protocol handler, before the node launches.
///
/// Hand the handler to [`SovaNetworkBuilder::with_sova_protocol`] (so it is
/// in the network config before the network starts) and keep the
/// receivers for [`service`] once the node is up. Connection events
/// that arrive in between wait in the receivers.
#[must_use]
pub fn protocol() -> (SovaProtocolHandler, ServiceReceivers) {
    let (channels, receivers) = protocol::service_channels();
    (SovaProtocolHandler::new(channels), receivers)
}

/// The gossip service future over a launched node's `backend`, draining
/// the receivers [`protocol`] returned. Runs for as long as the handler
/// is registered.
pub fn service<B: GossipBackend>(
    backend: B,
    receivers: ServiceReceivers,
    head_poll: Duration,
) -> impl Future<Output = ()> + Send {
    GossipService::new(backend).run(receivers, head_poll)
}

/// [`GossipBackend`] over a launched reth node.
#[derive(Debug, Clone)]
pub struct RethGossipBackend<P, N> {
    provider: P,
    engine: ConsensusEngineHandle<SovaEngineTypes>,
    network: N,
}

impl<P, N> RethGossipBackend<P, N> {
    /// Wraps a node's provider, engine handle, and network handle.
    pub const fn new(
        provider: P,
        engine: ConsensusEngineHandle<SovaEngineTypes>,
        network: N,
    ) -> Self {
        Self {
            provider,
            engine,
            network,
        }
    }
}

impl<P, N> GossipBackend for RethGossipBackend<P, N>
where
    P: BlockReader<Block = reth_ethereum::Block>
        + BlockHashReader
        + BlockNumReader
        + Send
        + Sync
        + 'static,
    N: Peers + Send + Sync + 'static,
{
    fn head(&self) -> Option<(u64, B256)> {
        let height = self.provider.best_block_number().ok()?;
        let hash = self.provider.block_hash(height).ok()??;
        Some((height, hash))
    }

    fn canonical_hash(&self, height: u64) -> Option<B256> {
        self.provider.block_hash(height).ok()?
    }

    fn has_block(&self, hash: B256) -> bool {
        matches!(self.provider.block_number(hash), Ok(Some(_)))
    }

    fn block_rlp(&self, hash: B256) -> Option<Bytes> {
        let block = self.provider.block_by_hash(hash).ok()??;
        let mut out = Vec::with_capacity(block.length());
        block.encode(&mut out);
        Some(out.into())
    }

    async fn submit(
        &self,
        block: SealedBlock<reth_ethereum::Block>,
    ) -> Result<PayloadStatusEnum, String> {
        let payload = SovaEngineTypes::block_to_payload(block, None);
        self.engine
            .new_payload(payload)
            .await
            .map(|status| status.status)
            .map_err(|err| err.to_string())
    }

    fn penalize_invalid_block(&self, peer: PeerId) {
        self.network
            .reputation_change(peer, ReputationChangeKind::BadBlock);
    }
}
