//! Peer discovery policy (board m1-b step 4; `docs/design/p2p-m1.md`
//! "Discovery and isolation").
//!
//! **Default:** discovery is on exactly when the chain profile is not
//! `dev` *and* the transport is `sova/1` (`SOVA_GOSSIP=p2p`). `dev` — the
//! box, the sims, nightly CI — keeps discovery off, as before. The relay
//! transport never discovers: it propagates over authrpc to `SOVA_PEERS`,
//! so devp2p peers would carry nothing.
//!
//! `SOVA_DISCOVERY=off` turns it off for a non-dev p2p node (static peers
//! only). `SOVA_DISCOVERY=on` is accepted only where the default already
//! is on: discovery on the `dev` profile is refused, because the dev
//! genesis — and so the fork ID peers are filtered by — is shared with
//! every `reth --dev` node there is.
//!
//! When on, the node runs:
//! - **discv4 and discv5 on one shared UDP port** — the RLPx port
//!   (`SOVA_P2P_PORT`). reth runs both over one socket when their bind
//!   addresses match (`reth network/src/discovery.rs:111-160`), which also
//!   means a plain `enode://` bootnode is reachable by both protocols
//!   (discv5 bootstraps unsigned enodes with a `request_enr` to the
//!   record's UDP port).
//! - **no DNS discovery**: Sova publishes no EIP-1459 tree, and there is
//!   nothing to query.
//! - **EIP-868 ENR fork-ID enforcement** (`--enforce-enr-fork-id`): a
//!   discovered peer is added to the peer set only once its ENR's `eth`
//!   fork ID has been fetched and matches ours (`reth network/src/swarm.rs:258-285`).
//!   The `sova-testnet` fork ID is our own (unique genesis, see
//!   [`crate::chain`]), so Ethereum and other reth chains' nodes never
//!   reach the dialer. The eth `Status` handshake's genesis + fork-filter
//!   check stays as the second gate.
//! - **pinned bootnodes**: [`crate::chain::ChainProfile::apply_bootnodes`]
//!   always sets an explicit list for non-dev profiles; [`apply`] refuses to
//!   enable discovery if the list is somehow unset, since reth would then
//!   fall back to Ethereum mainnet's bootnodes.
//!
//! Two knobs affect binding and advertising whether or not discovery is on:
//! `SOVA_P2P_ADDR` (IP the RLPx listener *and* discovery bind to; reth's
//! default is `0.0.0.0`) and `SOVA_NAT` (reth's `--nat`: `any`, `none`,
//! `upnp`, `publicip`, `extip:<IP>`, …; default `any`). Note `any` may
//! query UPnP and a public-IP service to learn the external address; a
//! loopback-only deployment (the sims) sets `SOVA_NAT=extip:127.0.0.1`.

use std::net::IpAddr;

use reth_ethereum::{network::types::NatResolver, node::core::args::NetworkArgs};

use crate::{chain::ChainProfile, gossip::Gossip};

/// Resolve whether discovery runs, from the profile, the transport, and
/// `SOVA_DISCOVERY` (`raw_override`: `on` | `off` | unset).
pub(crate) fn enabled(
    profile: ChainProfile,
    gossip: Gossip,
    raw_override: Option<&str>,
) -> eyre::Result<bool> {
    let default = profile != ChainProfile::Dev && gossip == Gossip::P2p;
    match raw_override {
        None => Ok(default),
        Some("off") => Ok(false),
        Some("on") if default => Ok(true),
        Some("on") if profile == ChainProfile::Dev => Err(eyre::eyre!(
            "SOVA_DISCOVERY=on is refused on the dev profile: its genesis (and fork ID) is every reth --dev node's, so discovery could not isolate the network; use SOVA_CHAIN=sova-testnet"
        )),
        Some("on") => Err(eyre::eyre!(
            "SOVA_DISCOVERY=on needs SOVA_GOSSIP=p2p (the relay transport propagates over authrpc, not devp2p)"
        )),
        Some(other) => Err(eyre::eyre!(
            "SOVA_DISCOVERY must be \"on\" or \"off\", got {other:?}"
        )),
    }
}

/// Bind/advertise overrides that apply with or without discovery.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AddrOverrides {
    /// `SOVA_P2P_ADDR`: RLPx listener and discovery bind IP.
    pub(crate) bind: Option<IpAddr>,
    /// `SOVA_NAT`: how the node learns the IP it advertises.
    pub(crate) nat: Option<NatResolver>,
}

impl AddrOverrides {
    /// Reads `SOVA_P2P_ADDR` and `SOVA_NAT`.
    pub(crate) fn from_env() -> eyre::Result<Self> {
        Self::parse(
            std::env::var("SOVA_P2P_ADDR").ok().as_deref(),
            std::env::var("SOVA_NAT").ok().as_deref(),
        )
    }

    fn parse(bind: Option<&str>, nat: Option<&str>) -> eyre::Result<Self> {
        let bind = bind
            .map(|s| {
                s.parse::<IpAddr>()
                    .map_err(|e| eyre::eyre!("bad SOVA_P2P_ADDR {s:?}: {e}"))
            })
            .transpose()?;
        let nat = nat
            .map(|s| {
                s.parse::<NatResolver>()
                    .map_err(|e| eyre::eyre!("bad SOVA_NAT {s:?}: {e}"))
            })
            .transpose()?;
        Ok(Self { bind, nat })
    }
}

/// Apply the discovery decision and address overrides to `network`. Call
/// after the bootnode list is pinned and `SOVA_P2P_PORT` is applied (the
/// discovery ports follow the RLPx port).
pub(crate) fn apply(
    network: &mut NetworkArgs,
    enabled: bool,
    overrides: &AddrOverrides,
) -> eyre::Result<()> {
    if let Some(ip) = overrides.bind {
        network.addr = ip;
        network.discovery.addr = ip;
    }
    if let Some(nat) = &overrides.nat {
        network.nat = nat.clone();
    }

    let discovery = &mut network.discovery;
    if !enabled {
        // What `NodeConfig::dev()` sets, and what every non-`.dev()` mode
        // set by hand before m1-b step 4.
        discovery.disable_discovery = true;
        return Ok(());
    }
    if network.bootnodes.is_none() {
        return Err(eyre::eyre!(
            "refusing to enable discovery with no pinned bootnode list: reth would fall back to Ethereum mainnet's"
        ));
    }
    discovery.disable_discovery = false;
    discovery.disable_discv4_discovery = false;
    discovery.disable_discv5_discovery = false;
    discovery.disable_dns_discovery = true;
    // One UDP port for discv4 + discv5 (reth's shared-socket mode needs
    // the same address and port; discv5's IPv4 address defaults to the
    // RLPx listener's, which `bind` set to `discovery.addr` too).
    discovery.port = network.port;
    discovery.discv5_port = Some(network.port);
    discovery.discv5_port_ipv6 = Some(network.port);
    network.enforce_enr_fork_id = true;
    Ok(())
}

/// One startup line describing the effective discovery config.
pub(crate) fn describe(network: &NetworkArgs) -> String {
    let d = &network.discovery;
    if d.disable_discovery {
        return "p2p: discovery off (static peers only)".to_owned();
    }
    let bootnodes = network.bootnodes.as_ref().map_or(0, Vec::len);
    format!(
        "p2p: discovery on (discv4{} on udp {}:{}; dns {}; enforce ENR fork id {}; nat {}; {} bootnode(s), no mainnet fallback)",
        if d.disable_discv5_discovery {
            ""
        } else {
            " + discv5"
        },
        d.addr,
        d.port,
        if d.disable_dns_discovery { "off" } else { "on" },
        network.enforce_enr_fork_id,
        network.nat,
        bootnodes,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::net::Ipv4Addr;

    use reth_ethereum::chainspec::{EthChainSpec, Head, MAINNET};

    use super::*;
    use crate::chain::{self, SOVA_TESTNET_BOOTNODES};

    const ENODE: &str = "enode://6f8a80d14311c39f35f516fa664deaaaa13e85b2f7493f37f6144d86991ec012937307647bd3b9a82abe2974e1407241d54947bbb39763a4cac9f77166ad92a0@127.0.0.1:30512";

    /// The path `main` takes: bootnodes pinned, port set, then [`apply`].
    fn configure(
        profile: ChainProfile,
        gossip: Gossip,
        discovery_env: Option<&str>,
        bootnodes_env: Option<&str>,
    ) -> eyre::Result<NetworkArgs> {
        let mut network = NetworkArgs::default();
        profile.apply_bootnodes(&mut network, bootnodes_env)?;
        network.port = 30_512;
        let on = enabled(profile, gossip, discovery_env)?;
        apply(&mut network, on, &AddrOverrides::default())?;
        Ok(network)
    }

    #[test]
    fn default_is_on_only_for_non_dev_p2p() {
        use ChainProfile::{Dev, SovaTestnet};
        use Gossip::{P2p, Relay};
        assert!(enabled(SovaTestnet, P2p, None).unwrap());
        assert!(!enabled(SovaTestnet, Relay, None).unwrap());
        assert!(!enabled(Dev, P2p, None).unwrap());
        assert!(!enabled(Dev, Relay, None).unwrap());
    }

    #[test]
    fn override_can_only_turn_off() {
        use ChainProfile::{Dev, SovaTestnet};
        use Gossip::{P2p, Relay};
        assert!(!enabled(SovaTestnet, P2p, Some("off")).unwrap());
        assert!(enabled(SovaTestnet, P2p, Some("on")).unwrap());
        assert!(!enabled(Dev, P2p, Some("off")).unwrap());
        // Dev can't be made to discover (shared genesis), nor can relay.
        assert!(enabled(Dev, P2p, Some("on")).is_err());
        assert!(enabled(Dev, Relay, Some("on")).is_err());
        assert!(enabled(SovaTestnet, Relay, Some("on")).is_err());
        assert!(enabled(SovaTestnet, P2p, Some("ON")).is_err());
        assert!(enabled(SovaTestnet, P2p, Some("")).is_err());
    }

    #[test]
    fn dev_keeps_discovery_off_and_reth_defaults_otherwise() {
        for gossip in [Gossip::Relay, Gossip::P2p] {
            let network = configure(ChainProfile::Dev, gossip, None, None).unwrap();
            assert!(network.discovery.disable_discovery);
            // Nothing else moved: same as reth's defaults (bar the port
            // this test set), in particular no fork-id / bootnode changes.
            let mut expected = NetworkArgs {
                port: 30_512,
                ..NetworkArgs::default()
            };
            expected.discovery.disable_discovery = true;
            assert_eq!(network, expected);
        }
    }

    #[test]
    fn testnet_p2p_runs_discv4_and_discv5_on_one_port_with_fork_id_enforced() {
        let network = configure(ChainProfile::SovaTestnet, Gossip::P2p, None, None).unwrap();
        let d = &network.discovery;
        assert!(!d.disable_discovery);
        assert!(!d.disable_discv4_discovery);
        assert!(!d.disable_discv5_discovery);
        assert!(d.disable_dns_discovery, "no Sova DNS tree to query");
        assert!(network.enforce_enr_fork_id);
        assert!(
            !NetworkArgs::default().enforce_enr_fork_id,
            "reth's default is off"
        );
        assert_eq!(d.port, 30_512);
        assert_eq!(d.discv5_port, Some(30_512));
        assert_eq!(d.addr, network.addr, "shared socket needs one address");
    }

    #[test]
    fn testnet_off_override_and_relay_stay_off() {
        let off = configure(ChainProfile::SovaTestnet, Gossip::P2p, Some("off"), None).unwrap();
        assert!(off.discovery.disable_discovery);
        assert!(!off.enforce_enr_fork_id);
        let relay = configure(ChainProfile::SovaTestnet, Gossip::Relay, None, None).unwrap();
        assert!(relay.discovery.disable_discovery);
    }

    /// With discovery on, the bootnode list reth resolves is exactly ours
    /// (`SOVA_BOOTNODES` or the profile default) — never mainnet's.
    #[test]
    fn discovery_bootnodes_are_explicit_never_mainnet() {
        let mainnet = MAINNET.bootnodes().expect("reth ships mainnet bootnodes");

        let network = configure(ChainProfile::SovaTestnet, Gossip::P2p, None, None).unwrap();
        let resolved = network.resolved_bootnodes().expect("pinned");
        assert_eq!(resolved.len(), SOVA_TESTNET_BOOTNODES.len());
        assert!(resolved.iter().all(|n| !mainnet.contains(n)));

        let network = configure(ChainProfile::SovaTestnet, Gossip::P2p, None, Some(ENODE)).unwrap();
        let resolved = network.resolved_bootnodes().expect("pinned");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].tcp_port, 30_512);
        assert!(!mainnet.contains(&resolved[0]));

        // The custom chainspec has none of its own: without the pin,
        // reth's resolution would reach `mainnet_nodes()`.
        assert!(chain::sova_testnet_chain_spec().bootnodes().is_none());
    }

    #[test]
    fn refuses_discovery_without_a_pinned_list() {
        let mut network = NetworkArgs::default();
        assert!(apply(&mut network, true, &AddrOverrides::default()).is_err());
        // Off never needs one.
        assert!(apply(&mut network, false, &AddrOverrides::default()).is_ok());
    }

    /// The fork ID a `sova-testnet` node advertises in its ENR (and sends
    /// in `Status`) differs from Ethereum mainnet's and from every
    /// `reth --dev` node's, so enforcement actually separates them.
    #[test]
    fn testnet_fork_id_isolates_from_ethereum_and_reth_dev() {
        let testnet = chain::sova_testnet_chain_spec();
        let ours = testnet.latest_fork_id();
        // What a sova-testnet node's ENR `eth` entry carries (seen on the
        // wire in discv5 traces); pinned like the genesis hash.
        assert_eq!(ours.hash.0, [0xa8, 0x72, 0xbd, 0x73]);
        assert_eq!(ours.next, 0);
        assert_ne!(ours, MAINNET.latest_fork_id());
        assert_ne!(ours, chain::dev_chain_spec().latest_fork_id());
        // A mainnet / dev peer's fork ID is rejected by our fork filter —
        // the same check the swarm applies to a discovered ENR fork ID.
        let filter = testnet.fork_filter(Head {
            hash: testnet.genesis_hash(),
            timestamp: testnet.genesis.timestamp,
            ..Head::default()
        });
        assert!(filter.validate(MAINNET.latest_fork_id()).is_err());
        assert!(
            filter
                .validate(chain::dev_chain_spec().latest_fork_id())
                .is_err()
        );
        assert!(filter.validate(ours).is_ok());
    }

    #[test]
    fn addr_overrides_parse_and_apply() {
        let o = AddrOverrides::parse(Some("127.0.0.1"), Some("extip:127.0.0.1")).unwrap();
        assert_eq!(o.bind, Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert_eq!(
            o.nat,
            Some(NatResolver::ExternalIp(IpAddr::V4(Ipv4Addr::LOCALHOST)))
        );
        assert!(AddrOverrides::parse(Some("localhost"), None).is_err());
        assert!(AddrOverrides::parse(None, Some("bogus")).is_err());
        assert_eq!(
            AddrOverrides::parse(None, None).unwrap(),
            AddrOverrides::default()
        );

        let mut network = NetworkArgs::default();
        ChainProfile::SovaTestnet
            .apply_bootnodes(&mut network, None)
            .unwrap();
        apply(&mut network, true, &o).unwrap();
        assert_eq!(network.addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(network.discovery.addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(
            network.nat,
            NatResolver::ExternalIp(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
        assert!(describe(&network).contains("discv4 + discv5 on udp 127.0.0.1:"));
    }
}
