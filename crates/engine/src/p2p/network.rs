//! [`SovaNetworkBuilder`]: reth's stock Ethereum network component, with
//! `sova/1` registered in the network *config* — before the
//! `NetworkManager` exists, binds its listener, or starts discovery.
//!
//! Why not `NetworkProtocols::add_rlpx_sub_protocol` after launch (what
//! m1-b step 3 did)? That call is a message to an already-running
//! `NetworkManager` (`reth network/src/network.rs:255`). Between the
//! manager's start (inside the component build) and our call (after
//! `launch` returns), the RLPx listener is accepting and — once discovery
//! is on — the manager is dialing discovered peers and bootnodes. A session
//! established in that window negotiates without `sova/1` and keeps it for
//! its lifetime. Static peers dodged this by being added only after
//! registration; discovered and inbound peers can't. Registering through
//! `NetworkConfigBuilder::add_rlpx_sub_protocol` (`reth network/src/config.rs:557`)
//! closes the window by construction: the `SessionManager` is created with
//! `sova/1` in its `extra_protocols` (`manager.rs:317`), so every session,
//! inbound or outbound, from the very first, offers it.
//!
//! The protocol handler only needs the connection→service channels, not
//! the engine handle or provider, so it can be built before launch. The
//! [`super::GossipService`] that owns the receiving ends is spawned after
//! launch; connection events that arrive first wait in its channels
//! (lifecycle events unbounded, messages bounded — excess is dropped and
//! recovered by the next announcement).

use reth_ethereum::{
    chainspec::Hardforks,
    network::{
        NetworkHandle, NetworkManager, PeersInfo, primitives::BasicNetworkPrimitives,
        protocol::IntoRlpxSubProtocol,
    },
    node::{
        api::{FullNodeTypes, NodeTypes, PrimitivesTy, TxTy},
        builder::{BuilderContext, components::NetworkBuilder},
    },
    pool::{PoolPooledTx, PoolTransaction, TransactionPool},
};

use super::SovaProtocolHandler;

/// Sova's network component: reth's Ethereum network (same config, same
/// Stake-mode guards, same `start_network` task wiring as
/// `EthereumNetworkBuilder`), plus `sova/1` in the config's extra
/// protocols when a handler is given. `Default` (no handler) builds exactly
/// what `EthereumNetworkBuilder` builds.
#[derive(Debug, Default, Clone)]
pub struct SovaNetworkBuilder {
    sova_protocol: Option<SovaProtocolHandler>,
}

impl SovaNetworkBuilder {
    /// A network component that offers `sova/1` on every session.
    #[must_use]
    pub const fn with_sova_protocol(handler: SovaProtocolHandler) -> Self {
        Self {
            sova_protocol: Some(handler),
        }
    }

    /// Whether `sova/1` will be registered.
    #[must_use]
    pub const fn has_sova_protocol(&self) -> bool {
        self.sova_protocol.is_some()
    }
}

impl<Node, Pool> NetworkBuilder<Node, Pool> for SovaNetworkBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: Hardforks>>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<Node::Types>>>
        + Unpin
        + 'static,
{
    type Network =
        NetworkHandle<BasicNetworkPrimitives<PrimitivesTy<Node::Types>, PoolPooledTx<Pool>>>;

    async fn build_network(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<Self::Network> {
        // `ctx.network_builder()` is `network_config_builder()` → build →
        // `NetworkManager::builder`; we do the same with one extra step.
        let mut config_builder = ctx.network_config_builder()?;
        if let Some(handler) = self.sova_protocol {
            config_builder = config_builder.add_rlpx_sub_protocol(handler.into_rlpx_sub_protocol());
            tracing::info!(target: "reth::cli", "sova/1 registered in the network config (before listen/discovery)");
        }
        let config = ctx.build_network_config(config_builder);
        let network = NetworkManager::builder(config).await?;
        let handle = ctx.start_network(network, pool);
        tracing::info!(target: "reth::cli", enode=%handle.local_node_record(), "P2P networking initialized");
        Ok(handle)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Real reth networks on loopback (no node, no discovery). The gossip
    //! service is never run: whether a session carries `sova/1` is decided
    //! entirely by how the handler was registered.

    use std::{
        net::{Ipv4Addr, SocketAddr, SocketAddrV4},
        time::Duration,
    };

    use reth_ethereum::{
        chainspec::DEV,
        network::{
            NetworkConfig, NetworkHandle, NetworkInfo, NetworkProtocols, Peers, api::PeerId,
            config::rng_secret_key, eth_wire::EthNetworkPrimitives,
        },
        tasks::Runtime,
    };

    use super::*;
    use crate::p2p::{ServiceReceivers, protocol::PeerEvent};

    /// Starts a network on 127.0.0.1:0, with `sova/1` in its config if
    /// `proto` is given (the [`SovaNetworkBuilder`] path).
    async fn spawn_network(
        proto: Option<SovaProtocolHandler>,
    ) -> NetworkHandle<EthNetworkPrimitives> {
        let mut builder = NetworkConfig::builder(rng_secret_key(), Runtime::test())
            .listener_addr(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
            .disable_discovery();
        if let Some(handler) = proto {
            builder = builder.add_rlpx_sub_protocol(handler.into_rlpx_sub_protocol());
        }
        let manager = NetworkManager::eth(builder.build_with_noop_provider(DEV.clone()))
            .await
            .unwrap();
        let handle = manager.handle().clone();
        tokio::spawn(manager);
        handle
    }

    async fn next_connected(rx: &mut ServiceReceivers, within: Duration) -> Option<PeerId> {
        match tokio::time::timeout(within, rx.lifecycle.recv()).await {
            Ok(Some(PeerEvent::Connected { peer_id, .. })) => Some(peer_id),
            _ => None,
        }
    }

    /// Config-time registration: the very first session, established
    /// before any gossip service exists, negotiates `sova/1` on both ends.
    #[tokio::test(flavor = "multi_thread")]
    async fn config_registration_covers_the_first_session() {
        let (handler_x, mut rx_x) = crate::p2p::protocol();
        let (handler_y, mut rx_y) = crate::p2p::protocol();
        let x = spawn_network(Some(handler_x)).await;
        let y = spawn_network(Some(handler_y)).await;
        y.add_peer(*x.peer_id(), x.local_addr());

        let within = Duration::from_secs(20);
        assert_eq!(next_connected(&mut rx_x, within).await, Some(*y.peer_id()));
        assert_eq!(next_connected(&mut rx_y, within).await, Some(*x.peer_id()));
    }

    /// The m1-b step-3 gap, reproduced: a session opened before a
    /// post-launch `add_rlpx_sub_protocol` never gets `sova/1`, even
    /// though both ends support it afterwards. This is what discovered and
    /// inbound peers could hit before [`SovaNetworkBuilder`].
    #[tokio::test(flavor = "multi_thread")]
    async fn late_registration_misses_an_existing_session() {
        let (handler_x, mut rx_x) = crate::p2p::protocol();
        let (handler_y, mut rx_y) = crate::p2p::protocol();
        let x = spawn_network(Some(handler_x)).await;
        let y = spawn_network(None).await;
        y.add_peer(*x.peer_id(), x.local_addr());

        // Wait for the (eth-only) session.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while y.num_connected_peers() == 0 {
            assert!(tokio::time::Instant::now() < deadline, "no session");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Now register on Y, as step 3 did after launch.
        y.add_rlpx_sub_protocol(handler_y.into_rlpx_sub_protocol());

        let quiet = Duration::from_secs(2);
        assert_eq!(next_connected(&mut rx_x, quiet).await, None);
        assert_eq!(next_connected(&mut rx_y, quiet).await, None);
        assert_eq!(y.num_connected_peers(), 1, "session kept, without sova/1");
    }
}
