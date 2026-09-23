//! RPC profiles (board m1-c): what `bin/sova`'s HTTP JSON-RPC exposes.
//!
//! Selected with `SOVA_RPC_PROFILE`:
//!
//! - `local` (default, and what an unset `SOVA_RPC_PROFILE` means): exactly
//!   the pre-m1-c behaviour — reth's standard HTTP module set (`eth`, `net`,
//!   `web3`, every method in them) on 127.0.0.1, no WebSocket, no IPC.
//!   The box, sims and wallets-on-localhost use this.
//! - `public`: for an RPC origin that strangers reach (behind the
//!   Cloudflare Worker in `docs/design/infra-m1.md` §2). Two layers, both
//!   through reth's own APIs:
//!   1. **Namespaces** — `http.api` is pinned to an explicit
//!      [`RpcModuleSelection`] of `eth`, `net`, `web3` (not reth's
//!      "standard" default, which could widen on a reth bump), so `admin`,
//!      `debug`, `trace`, `txpool`, `miner`, `reth`, `ots`, `flashbots`,
//!      `mev`, `testing` and `rpc` are never even built for HTTP. The
//!      Engine API is authrpc-only and never on this server. WS is forced
//!      off (the edge denies `eth_subscribe` at M1 anyway).
//!   2. **Methods** — after reth assembles the HTTP module, an
//!      `extend_rpc_modules` hook removes every method not in
//!      [`PUBLIC_RPC_METHODS`] (reth's `TransportRpcModules::
//!      remove_http_method`). That drops the dangerous corners *inside*
//!      `eth`: `eth_sendTransaction`/`eth_sign*` (a `--dev` node holds 20
//!      publicly-keyed dev signers), `eth_accounts`, the stateful filter
//!      methods, `eth_simulateV1`, `eth_getProof`, and so on.
//!
//! The allowlist is the same list as the edge Worker's (infra-m1 §2, "Edge
//! allowlist"): public RPC is read-and-broadcast only, enforced twice.
//!
//! Both profiles answer `eth_call`/`eth_estimateGas`/`eth_createAccessList`
//! at block tag `pending` as at `latest` (an RPC middleware, see
//! `pending_rpc.rs`); the profile only decides which methods exist.
//!
//! CORS is separate from the profile: `SOVA_RPC_CORS` (see [`RpcCors`])
//! sets reth's `--http.corsdomain`, so browser pages can call the node
//! directly. Unset, the node sends no CORS headers (reth's default). The
//! box sets `*`; the testnet hosts leave it unset because their RPC is
//! loopback-only and reached through the edge Worker, which answers CORS.

use std::collections::HashSet;

use reth_ethereum::{
    node::core::args::RpcServerArgs,
    rpc::builder::{RethRpcModule, RpcModuleSelection, TransportRpcModules},
};

/// The only namespaces the `public` profile builds for HTTP.
pub(crate) const PUBLIC_HTTP_MODULES: [RethRpcModule; 3] =
    [RethRpcModule::Eth, RethRpcModule::Net, RethRpcModule::Web3];

/// Every method the `public` profile serves over HTTP. Mirrors the edge
/// allowlist in `docs/design/infra-m1.md` §2 — change both together.
pub(crate) const PUBLIC_RPC_METHODS: &[&str] = &[
    // Chain metadata and fees.
    "eth_chainId",
    "net_version",
    "web3_clientVersion",
    "eth_syncing",
    "eth_blockNumber",
    "eth_gasPrice",
    "eth_maxPriorityFeePerGas",
    "eth_feeHistory",
    // Blocks and transactions.
    "eth_getBlockByNumber",
    "eth_getBlockByHash",
    "eth_getBlockReceipts",
    "eth_getTransactionByHash",
    "eth_getTransactionByBlockHashAndIndex",
    "eth_getTransactionByBlockNumberAndIndex",
    "eth_getTransactionReceipt",
    "eth_getTransactionCount",
    // State and calls.
    "eth_getBalance",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_call",
    "eth_estimateGas",
    "eth_getLogs",
    // Broadcast.
    "eth_sendRawTransaction",
];

/// An RPC profile, parsed from `SOVA_RPC_PROFILE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RpcProfile {
    /// Today's behaviour: reth's standard HTTP modules, unfiltered.
    Local,
    /// Read-and-broadcast only, for strangers.
    Public,
}

impl RpcProfile {
    /// Parse a `SOVA_RPC_PROFILE` value; `None` (unset) means [`Self::Local`].
    pub(crate) fn parse(raw: Option<&str>) -> eyre::Result<Self> {
        match raw {
            None | Some("local") => Ok(Self::Local),
            Some("public") => Ok(Self::Public),
            Some(other) => Err(eyre::eyre!(
                "SOVA_RPC_PROFILE must be \"local\" or \"public\", got {other:?}"
            )),
        }
    }

    /// Read the profile from the `SOVA_RPC_PROFILE` environment variable.
    pub(crate) fn from_env() -> eyre::Result<Self> {
        Self::parse(std::env::var("SOVA_RPC_PROFILE").ok().as_deref())
    }

    /// Apply the profile's server-level settings. `Local` touches nothing.
    pub(crate) fn apply(self, rpc: &mut RpcServerArgs) {
        match self {
            Self::Local => {}
            Self::Public => {
                rpc.http_api = Some(RpcModuleSelection::from_iter(PUBLIC_HTTP_MODULES));
                rpc.ws = false;
                rpc.ws_api = None;
                rpc.ipcdisable = true;
            }
        }
    }

    /// The per-method HTTP allowlist, if this profile has one.
    pub(crate) const fn method_allowlist(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Local => None,
            Self::Public => Some(PUBLIC_RPC_METHODS),
        }
    }
}

/// Allowed browser origins for the HTTP RPC, from `SOVA_RPC_CORS`.
///
/// The value goes to reth's `http_corsdomain` unchanged (after trimming):
/// `*` for any origin, or a comma-separated list of exact origins such as
/// `https://sova.io,http://localhost:5173`. Unset or blank = no CORS layer,
/// which is today's behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RpcCors(Option<String>);

impl RpcCors {
    /// Parse a `SOVA_RPC_CORS` value. Rejects what reth would only reject
    /// later at server start (a `*` inside a list, an empty list entry), so
    /// a typo fails with this variable's name in the message.
    pub(crate) fn parse(raw: Option<&str>) -> eyre::Result<Self> {
        let Some(value) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
            return Ok(Self(None));
        };
        if value != "*" {
            for origin in value.split(',').map(str::trim) {
                if origin.is_empty() || origin == "*" || origin.chars().any(char::is_whitespace) {
                    return Err(eyre::eyre!(
                        "SOVA_RPC_CORS must be \"*\" or a comma-separated list of origins (no \"*\" inside a list), got {value:?}"
                    ));
                }
            }
        }
        Ok(Self(Some(value.to_owned())))
    }

    /// Read `SOVA_RPC_CORS` from the environment.
    pub(crate) fn from_env() -> eyre::Result<Self> {
        Self::parse(std::env::var("SOVA_RPC_CORS").ok().as_deref())
    }

    /// Set reth's HTTP CORS domains. Unset leaves the config untouched.
    pub(crate) fn apply(&self, rpc: &mut RpcServerArgs) {
        if let Some(domains) = &self.0 {
            rpc.http_corsdomain = Some(domains.clone());
        }
    }

    /// For the startup log.
    pub(crate) fn describe(&self) -> String {
        match &self.0 {
            None => "rpc cors: off (SOVA_RPC_CORS unset)".to_owned(),
            Some(d) => format!("rpc cors: HTTP allows origin(s) {d}"),
        }
    }
}

/// Which of `registered` to remove so that only `allowlist` remains, and
/// which allowlisted names aren't registered at all (a typo, or a method
/// a reth bump renamed — worth a loud warning, not a failed launch).
pub(crate) fn partition_methods(
    registered: impl IntoIterator<Item = &'static str>,
    allowlist: &[&str],
) -> (Vec<&'static str>, Vec<String>) {
    let allowed: HashSet<&str> = allowlist.iter().copied().collect();
    let registered: HashSet<&'static str> = registered.into_iter().collect();
    let mut remove: Vec<_> = registered
        .iter()
        .copied()
        .filter(|m| !allowed.contains(m))
        .collect();
    remove.sort_unstable();
    let missing = allowlist
        .iter()
        .filter(|m| !registered.contains(*m))
        .map(|m| (*m).to_owned())
        .collect();
    (remove, missing)
}

/// Strip every HTTP method outside `allowlist` from reth's assembled
/// modules. Returns `(kept, removed, missing)` for the startup log.
pub(crate) fn restrict_http_methods(
    modules: &mut TransportRpcModules,
    allowlist: &[&str],
) -> (usize, usize, Vec<String>) {
    let registered: Vec<&'static str> = modules
        .http_methods(|_| true)
        .map(|m| m.method_names().collect())
        .unwrap_or_default();
    let (remove, missing) = partition_methods(registered.iter().copied(), allowlist);
    for name in &remove {
        modules.remove_http_method(name);
    }
    (registered.len() - remove.len(), remove.len(), missing)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use reth_ethereum::node::core::args::RpcServerArgs;

    /// Every namespace a stranger must never reach over HTTP.
    const DENIED: [RethRpcModule; 11] = [
        RethRpcModule::Admin,
        RethRpcModule::Debug,
        RethRpcModule::Trace,
        RethRpcModule::Txpool,
        RethRpcModule::Miner,
        RethRpcModule::Reth,
        RethRpcModule::Ots,
        RethRpcModule::Flashbots,
        RethRpcModule::Mev,
        RethRpcModule::Testing,
        RethRpcModule::Rpc,
    ];

    fn base() -> RpcServerArgs {
        // Exactly how main.rs starts its RPC config.
        RpcServerArgs::default().with_http()
    }

    #[test]
    fn local_profile_is_todays_config() {
        let mut rpc = base();
        RpcProfile::Local.apply(&mut rpc);
        assert_eq!(rpc, base());
        assert!(
            rpc.http_api.is_none(),
            "local keeps reth's standard default"
        );
        assert!(RpcProfile::Local.method_allowlist().is_none());
    }

    #[test]
    fn public_profile_excludes_every_dangerous_namespace() {
        let mut rpc = base();
        RpcProfile::Public.apply(&mut rpc);
        let api = rpc.http_api.clone().expect("public pins http.api");
        for ns in DENIED {
            assert!(!api.contains(&ns), "public http.api must not contain {ns}");
            assert!(
                !rpc.is_namespace_enabled(ns.clone()),
                "{ns} must be off on every transport"
            );
        }
        assert_eq!(api.to_selection(), HashSet::from(PUBLIC_HTTP_MODULES));
        assert!(!rpc.ws && rpc.ipcdisable);
        // Every reth namespace is either one of ours or denied: nothing
        // new in a reth bump slips through unclassified.
        for ns in RethRpcModule::all_variants() {
            assert!(
                PUBLIC_HTTP_MODULES.contains(ns) || DENIED.contains(ns),
                "{ns}"
            );
        }
    }

    #[test]
    fn allowlist_is_read_and_broadcast_only() {
        let allow = RpcProfile::Public.method_allowlist().unwrap();
        for m in allow {
            assert!(
                ["eth_", "net_", "web3_"].iter().any(|p| m.starts_with(p)),
                "{m}"
            );
        }
        for denied in [
            "eth_sendTransaction",
            "eth_sign",
            "eth_signTransaction",
            "eth_signTypedData",
            "eth_accounts",
            "eth_newFilter",
            "eth_getFilterChanges",
            "eth_subscribe",
            "admin_nodeInfo",
            "debug_traceTransaction",
            "txpool_content",
        ] {
            assert!(!allow.contains(&denied), "{denied}");
        }
        assert!(allow.contains(&"eth_sendRawTransaction"));
        let unique: HashSet<_> = allow.iter().collect();
        assert_eq!(unique.len(), allow.len(), "no duplicates");
    }

    #[test]
    fn partition_keeps_only_the_allowlist() {
        let registered = [
            "eth_blockNumber",
            "eth_sendRawTransaction",
            "eth_sendTransaction",
            "eth_sign",
            "eth_accounts",
            "net_version",
            "net_peerCount",
            "web3_clientVersion",
            "web3_sha3",
        ];
        let (remove, missing) = partition_methods(registered, PUBLIC_RPC_METHODS);
        assert_eq!(
            remove,
            vec![
                "eth_accounts",
                "eth_sendTransaction",
                "eth_sign",
                "net_peerCount",
                "web3_sha3"
            ]
        );
        assert!(missing.contains(&"eth_call".to_owned()));
        assert!(!missing.contains(&"eth_blockNumber".to_owned()));
    }

    #[test]
    fn profile_parsing() {
        assert_eq!(RpcProfile::parse(None).unwrap(), RpcProfile::Local);
        assert_eq!(RpcProfile::parse(Some("local")).unwrap(), RpcProfile::Local);
        assert_eq!(
            RpcProfile::parse(Some("public")).unwrap(),
            RpcProfile::Public
        );
        assert!(RpcProfile::parse(Some("open")).is_err());
    }

    #[test]
    fn cors_unset_is_todays_config() {
        for raw in [None, Some(""), Some("  ")] {
            let cors = RpcCors::parse(raw).unwrap();
            let mut rpc = base();
            cors.apply(&mut rpc);
            assert_eq!(rpc, base(), "{raw:?}");
            assert!(rpc.http_corsdomain.is_none());
        }
    }

    #[test]
    fn cors_sets_reths_http_corsdomain() {
        for (raw, want) in [
            ("*", "*"),
            (" * ", "*"),
            ("https://sova.io", "https://sova.io"),
            (
                "https://sova.io, http://localhost:5173",
                "https://sova.io, http://localhost:5173",
            ),
        ] {
            let mut rpc = base();
            RpcCors::parse(Some(raw)).unwrap().apply(&mut rpc);
            assert_eq!(rpc.http_corsdomain.as_deref(), Some(want), "{raw:?}");
        }
        // Composes with the public profile: CORS doesn't touch the method
        // or namespace restrictions, and the profile doesn't touch CORS.
        let mut rpc = base();
        RpcProfile::Public.apply(&mut rpc);
        RpcCors::parse(Some("*")).unwrap().apply(&mut rpc);
        assert_eq!(rpc.http_corsdomain.as_deref(), Some("*"));
        assert!(!rpc.ws && rpc.ipcdisable);
    }

    #[test]
    fn cors_rejects_what_reth_would_refuse_at_start() {
        for bad in [
            "*,https://sova.io",
            "https://sova.io,*",
            "https://a,,https://b",
            "https://a b",
        ] {
            let err = RpcCors::parse(Some(bad)).unwrap_err().to_string();
            assert!(err.contains("SOVA_RPC_CORS"), "{bad:?}: {err}");
        }
    }
}
