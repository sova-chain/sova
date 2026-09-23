//! Block-propagation transport selection (`SOVA_GOSSIP`).
//!
//! - `relay` (default): gossip v1's authrpc relay (`engine::relay`) —
//!   mine-mode nodes push `engine_newPayloadV4` to `SOVA_PEERS` over a
//!   shared JWT. Unchanged; box-scale trust only.
//! - `p2p`: the `sova/1` RLPx sub-protocol (`engine::p2p`,
//!   `docs/design/p2p-m1.md`). Every node — mine-mode or follow-only —
//!   announces the blocks it produces or accepts, pulls announced blocks
//!   it lacks, and submits them to its own engine in-process. No shared
//!   secret: peers connect over devp2p — static peers from
//!   `SOVA_P2P_PEERS` (comma-separated enode URLs) and, on non-dev
//!   profiles, peers found by discovery ([`crate::discovery`]). The engine
//!   API (authrpc) stays on localhost.
//!
//! `sova/1` is registered in the network config before launch
//! ([`engine::p2p::SovaNetworkBuilder`], built from [`protocol`]), so
//! every session — static, discovered, or inbound, including those opened
//! before `launch` returns — negotiates it. [`start`] then only spawns the
//! service and adds static peers.

use std::{net::SocketAddr, time::Duration};

use engine::{
    SovaEngineTypes,
    p2p::{RethGossipBackend, ServiceReceivers, SovaNetworkBuilder},
};
use reth_ethereum::{
    network::{
        Peers, PeersInfo,
        api::{PeerId, PeerKind},
    },
    node::api::ConsensusEngineHandle,
    storage::{BlockHashReader, BlockNumReader, BlockReader},
};
use reth_network_peers::NodeRecord;

/// How often the gossip service checks the canonical head for blocks to
/// announce.
const HEAD_POLL: Duration = Duration::from_millis(250);
/// How often static peers without a live session are redialed.
const REDIAL_EVERY: Duration = Duration::from_secs(5);

/// The block-propagation transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gossip {
    /// Gossip v1's authrpc relay (default).
    Relay,
    /// The `sova/1` RLPx sub-protocol.
    P2p,
}

impl Gossip {
    /// Reads `SOVA_GOSSIP` (`relay` | `p2p`; unset = `relay`).
    pub(crate) fn from_env() -> eyre::Result<Self> {
        Self::parse(std::env::var("SOVA_GOSSIP").ok().as_deref())
    }

    fn parse(raw: Option<&str>) -> eyre::Result<Self> {
        match raw {
            None | Some("relay") => Ok(Self::Relay),
            Some("p2p") => Ok(Self::P2p),
            Some(other) => Err(eyre::eyre!(
                "SOVA_GOSSIP must be \"relay\" or \"p2p\", got {other:?}"
            )),
        }
    }
}

/// Parses `SOVA_P2P_PEERS`: comma-separated `enode://<id>@<ip>:<port>`.
pub(crate) fn parse_p2p_peers(raw: Option<&str>) -> eyre::Result<Vec<NodeRecord>> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<NodeRecord>()
                .map_err(|e| eyre::eyre!("bad SOVA_P2P_PEERS entry {s:?}: {e}"))
        })
        .collect()
}

/// The network component for this transport: with `sova/1` in the
/// network config for `P2p` (plus the receivers [`start`] needs), reth's
/// stock Ethereum network for `Relay`.
pub(crate) fn network_builder(gossip: Gossip) -> (SovaNetworkBuilder, Option<ServiceReceivers>) {
    match gossip {
        Gossip::Relay => (SovaNetworkBuilder::default(), None),
        Gossip::P2p => {
            let (handler, receivers) = engine::p2p::protocol();
            (
                SovaNetworkBuilder::with_sova_protocol(handler),
                Some(receivers),
            )
        }
    }
}

/// Spawns the gossip service on a launched node whose network already
/// carries `sova/1` (see [`network_builder`]), then peers with
/// `static_peers`.
pub(crate) fn start<P, N>(
    provider: P,
    engine: ConsensusEngineHandle<SovaEngineTypes>,
    network: N,
    receivers: ServiceReceivers,
    static_peers: Vec<NodeRecord>,
) where
    P: BlockReader<Block = reth_ethereum::Block>
        + BlockHashReader
        + BlockNumReader
        + Send
        + Sync
        + 'static,
    N: Peers + PeersInfo + Clone + Send + Sync + 'static,
{
    let backend = RethGossipBackend::new(provider, engine, network.clone());
    tokio::spawn(engine::p2p::service(backend, receivers, HEAD_POLL));

    let local = network.local_node_record();
    println!("p2p: sova/1 gossip enabled; local enode {local}");

    if static_peers.is_empty() {
        println!("p2p: no SOVA_P2P_PEERS; peers come from discovery or dial in");
        return;
    }
    let peers: Vec<(PeerId, SocketAddr)> = static_peers
        .iter()
        .map(|r| (r.id, SocketAddr::new(r.address, r.tcp_port)))
        .collect();
    for (id, addr) in &peers {
        network.add_trusted_peer(*id, *addr);
    }
    println!(
        "p2p: static peers ({}): {}",
        peers.len(),
        static_peers
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Keep static peers connected: reth backs off a peer after a dropped
    // session (e.g. a stalled/SIGSTOPped node missing pings); redial any
    // without a live session so propagation resumes promptly.
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(REDIAL_EVERY).await;
            for (id, addr) in &peers {
                if matches!(network.get_peer_by_id(*id).await, Ok(None)) {
                    println!("p2p: static peer {id} ({addr}) not connected; redialing");
                    network.connect_peer_kind(*id, PeerKind::Trusted, *addr, None);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gossip_defaults_to_relay_and_rejects_unknown() {
        assert_eq!(Gossip::parse(None).ok(), Some(Gossip::Relay));
        assert_eq!(Gossip::parse(Some("relay")).ok(), Some(Gossip::Relay));
        assert_eq!(Gossip::parse(Some("p2p")).ok(), Some(Gossip::P2p));
        assert!(Gossip::parse(Some("P2P")).is_err());
        assert!(Gossip::parse(Some("")).is_err());
    }

    #[test]
    fn only_p2p_registers_sova_1() {
        let (relay, rx) = network_builder(Gossip::Relay);
        assert!(!relay.has_sova_protocol() && rx.is_none());
        let (p2p, rx) = network_builder(Gossip::P2p);
        assert!(p2p.has_sova_protocol() && rx.is_some());
    }

    #[test]
    fn parses_static_peer_enodes() {
        let id = "6f8a80d14311c39f35f516fa664deaaaa13e85b2f7493f37f6144d86991ec012937307647bd3b9a82abe2974e1407241d54947bbb39763a4cac9f77166ad92a0";
        let raw = format!(" enode://{id}@127.0.0.1:30411 ,enode://{id}@10.0.0.2:30412,");
        let peers = parse_p2p_peers(Some(&raw)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].tcp_port, 30411);
        assert_eq!(peers[1].address.to_string(), "10.0.0.2");
        assert!(parse_p2p_peers(None).unwrap_or_default().is_empty());
        assert!(parse_p2p_peers(Some("enode://nope@1.2.3.4:1")).is_err());
    }
}
