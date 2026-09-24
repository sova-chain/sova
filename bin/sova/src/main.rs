//! Sova node binary entry point.
//!
//! Three modes:
//!
//! - **Dev** (default): launches [`engine::SovaNode`] against reth's
//!   built-in dev chain spec with interval auto-mining — the B1/B3a
//!   continuity path, unchanged.
//! - **Mine** (`SOVA_ZEBRAD_RPC` set): the C3 wiring. The node launches
//!   with *no* interval miner; instead we construct reth's `LocalMiner`
//!   ourselves in trigger mode and feed it from the Sova sealer: the
//!   [`engine::driver::SealerCore`] follows Zcash through zebrad, and for
//!   every Zcash block fires one trigger — one Sova block per Zcash epoch.
//!   When our miner address is the epoch's rank-0 sealer, the epoch's
//!   settlement attribute is staged in the [`engine::PendingEpoch`]
//!   mailbox; the payload-attributes builder drains it, and the built
//!   block mints the epoch's rewards through the withdrawals channel. If
//!   `SOVA_PEERS` is set, this mode also runs gossip v1's relay task
//!   ([`engine::relay::run_relay`]), pushing every sealed block to each
//!   peer's authrpc.
//! - **Follow-only** (`SOVA_FOLLOW_ONLY=1`): gossip v1's receiving side
//!   (`docs/design/gossip-v1.md`). No dev interval mining, no sealer, no
//!   local miner at all — the node only serves RPC and accepts blocks
//!   pushed to its own authrpc by a peer's relay task
//!   (`engine_newPayloadV4` + `engine_forkchoiceUpdatedV3`), validating
//!   each one through the same stateful engine path any Engine API caller
//!   goes through.
//!
//! In **any** mode where `SOVA_ZEBRAD_RPC` is set (including follow-only,
//! which uses it purely for verification, never to seal), the node runs
//! the C5 expectations follower — imported settlements must match some
//! rank's derivation from *our own* zebrad — and the v2 preference
//! arbiter, which forkchoice-adopts whichever valid candidate wins
//! rank-then-hash preference for its epoch. Without a zebrad, imports
//! are accepted on trust and the node says so at startup.
//!
//! A shared JWT (`SOVA_AUTH_JWT`, a path both nodes point at the same
//! file) and configurable ports (`SOVA_HTTP_PORT`/`SOVA_AUTH_PORT`/
//! `SOVA_P2P_PORT`) let a mine-mode and a follow-only node coexist on one
//! host, authrpc-to-authrpc.
//!
//! Mine-mode env: `SOVA_ZEBRAD_RPC` (e.g. `http://127.0.0.1:18232`),
//! `SOVA_MINER_EVM_ADDRESS` (0x-hex, the address our burns credit),
//! `SOVA_EPOCH_BASE` (first Zcash height to treat as an epoch, default 1),
//! `SOVA_PEERS` (comma-separated authrpc URLs to relay to).
//!
//! SIP-7 Zcash pool state (`SOVA_SIP7=1`, see `evm::zcash`): contracts can
//! read pool totals, block stats and per-tx shielded flows from 0x…5A00,
//! and the Zcash scan holds (never skips) on missing or inconsistent pool
//! accounting. Every node must agree on it. (`engine::zcash_index::activate_sip7`)
//! With it on, the node also serves SIP-7 §4.2's feed (see [`zcash_feed`]):
//! `sova_getZcashBlocks(from, to)` over HTTP (and WS), and
//! `sova_subscribe("zcashBlocks")` over WS, which is off unless
//! `SOVA_WS_PORT` is set (local profile only; public forces WS off).
//!
//! SIP-6 sealer signatures (`SOVA_SIP6=1`, see `engine::seal`): every node
//! then requires each block to be sealed (or a null block) under this
//! chain's ID; a mine-mode node signs its blocks with the key in
//! `SOVA_SEALER_KEYSTORE` (the `sova-miner` keystore — its address is the
//! one the burns credit; a different `SOVA_MINER_EVM_ADDRESS` is refused).
//!
//! Chain profile (`SOVA_CHAIN`, see [`chain`]): `dev` (default — reth's
//! dev spec with its publicly-keyed prefunded accounts; local use only) or
//! `sova-testnet` (empty genesis alloc, its own chain ID and genesis,
//! bootnodes pinned — `SOVA_BOOTNODES` or none — never reth's mainnet
//! fallback). Orthogonal to the three modes above.
//!
//! Gossip transport (`SOVA_GOSSIP`, see [`gossip`]): `relay` (default —
//! gossip v1's authrpc relay above, unchanged) or `p2p` (the `sova/1` RLPx
//! sub-protocol: every node announces/pulls blocks over devp2p and
//! submits them to its own engine in-process; static peers from
//! `SOVA_P2P_PEERS`, no shared JWT, authrpc stays on localhost).
//!
//! Discovery (see [`discovery`]): on for non-dev profiles with
//! `SOVA_GOSSIP=p2p` (discv4 + discv5 on the RLPx port, ENR fork-ID
//! enforced, bootnodes only from `SOVA_BOOTNODES`/the profile), off for
//! `dev` and for the relay transport. `SOVA_DISCOVERY=off` opts out;
//! `SOVA_P2P_ADDR` / `SOVA_NAT` set the bind IP and NAT resolver.
//!
//! Datadir (`SOVA_DATADIR`, see [`open_datadir`]): unset = a fresh
//! ephemeral datadir per start (box, sims, CI); set = a persistent one, so
//! the chain and the node key (and with it the enode) survive restarts.
//!
//! RPC profile (`SOVA_RPC_PROFILE`, see [`rpc`]): `local` (default —
//! reth's standard `eth`/`net`/`web3` over HTTP on 127.0.0.1, unfiltered)
//! or `public` (read-and-broadcast only: `http.api` pinned to
//! `eth,net,web3`, then every HTTP method outside the allowlist removed).
//! `SOVA_RPC_CORS` (unset = no CORS headers, today's behaviour; `*` or a
//! comma-separated origin list) sets reth's `--http.corsdomain` so browser
//! pages can call the node directly; the box sets `*`.
//!
//! Block tag `pending` (see [`pending_rpc`]): in both profiles, over HTTP
//! and WS, `eth_call`, `eth_estimateGas` and `eth_createAccessList` at
//! `pending` are answered as at `latest`. reth's pending env is head+1,
//! whose Zcash epoch doesn't exist yet, so any call reaching the SIP-4
//! precompile would fail there. Every other method keeps reth's pending
//! semantics.

mod chain;
mod discovery;
mod gossip;
mod pending_rpc;
mod rpc;
mod zcash_feed;

use std::{path::PathBuf, time::Duration};

use alloy_rpc_types::engine::ForkchoiceState;
use consensus::zebrad::ZebradClient;
use engine::{
    PendingEpoch, SovaEngineTypes, SovaLocalPayloadAttributesBuilder, SovaNode,
    driver::{DRAFT_EPOCH_REWARD_GWEI, SealerConfig, SealerCore, run_sealer},
    relay::run_relay,
};
use reth_ethereum::{
    chainspec::ChainSpec,
    node::{
        api::PayloadTypes,
        builder::{Node, NodeBuilder, NodeHandle},
        core::{args::RpcServerArgs, node_config::NodeConfig},
    },
    primitives::SealedBlock,
    provider::{BlockHashReader, BlockNumReader, BlockReader},
    tasks::Runtime,
};
use reth_rpc_layer::JwtSecret;
use reth_tracing::{RethTracer, Tracer};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    // Without an installed subscriber, reth's internal `tracing` calls
    // (RPC server bind confirmation, block production, etc.) go nowhere.
    // Keep the guard alive for the process lifetime.
    let _tracing_guard = RethTracer::new().init()?;

    let runtime = Runtime::test();
    let chain_profile = chain::ChainProfile::from_env()?;
    let rpc_profile = rpc::RpcProfile::from_env()?;
    let rpc_cors = rpc::RpcCors::from_env()?;
    let follow_only = env_flag("SOVA_FOLLOW_ONLY");
    let gossip = gossip::Gossip::from_env()?;
    let p2p_peers = match gossip {
        gossip::Gossip::Relay => Vec::new(),
        gossip::Gossip::P2p => {
            if std::env::var("SOVA_PEERS").is_ok_and(|v| !v.trim().is_empty()) {
                return Err(eyre::eyre!(
                    "SOVA_PEERS is the relay transport's authrpc peer list; with SOVA_GOSSIP=p2p use SOVA_P2P_PEERS (enode URLs)"
                ));
            }
            gossip::parse_p2p_peers(std::env::var("SOVA_P2P_PEERS").ok().as_deref())?
        }
    };
    let discovery_on = discovery::enabled(
        chain_profile,
        gossip,
        std::env::var("SOVA_DISCOVERY").ok().as_deref(),
    )?;
    let addr_overrides = discovery::AddrOverrides::from_env()?;
    // Any mode with a zebrad enforces C5 and arbitrates v2 preference; a
    // follow-only node uses it purely for verification, never to seal.
    let zebrad_rpc = std::env::var("SOVA_ZEBRAD_RPC").ok();
    let mine_rpc = if follow_only {
        None
    } else {
        zebrad_rpc.clone()
    };

    let mut node_config = NodeConfig::new(chain_profile.chain_spec_with(env_flag("SOVA_SIP7")))
        .with_rpc(RpcServerArgs::default().with_http());
    // The relay (crates/engine/src/relay.rs) sends `execution_requests` as
    // `RequestsOrHash::Hash` (the header's `requests_hash`, not the request
    // list itself -- see relay.rs's doc comment on why that's the right
    // shape here). reth's `engine_newPayloadV4` handler rejects that shape
    // over RPC unless the receiving node opts in with this flag
    // (`--engine.accept-execution-requests-hash`); found by running the
    // two-node scenario, not from reading the type definitions -- see this
    // binary's own module doc / the report on this change for the
    // full note. Every node accepts it: harmless for one that never
    // receives relayed calls, and required for one that does.
    node_config.engine.accept_execution_requests_hash = true;

    if follow_only || mine_rpc.is_some() {
        // No `.dev()` here: that flag (`dev.dev`) is what makes reth's
        // `DebugNodeLauncher` spawn *its own* `LocalMiner` — instant mode,
        // since `dev.block_time` is unset — which builds on every pooled
        // transaction and re-asserts its own stale forkchoice every second
        // (logged as "Error updating fork choice: too deep reorg" once
        // depth-lagged finality passes it). A follow-only node must have
        // no producer at all, and a mine-mode node's only producer is
        // `SovaMiner` (below). `.dev()` also disables discovery as a side
        // effect; here discovery is set per profile by
        // `discovery::apply` below.
    } else {
        // Plain dev chain (no zebrad): reth's interval auto-miner is the
        // producer, so `eth_blockNumber` advances without transactions.
        node_config = node_config.dev();
        node_config.dev.block_time = Some(Duration::from_secs(1));
    }

    // Pin the bootnode list for non-dev chains (and honour
    // SOVA_BOOTNODES): reth would otherwise fall back to Ethereum
    // mainnet's for a custom chainspec. Dev without an override is
    // untouched.
    chain_profile.apply_bootnodes(
        &mut node_config.network,
        std::env::var("SOVA_BOOTNODES").ok().as_deref(),
    )?;
    apply_port_overrides(&mut node_config);
    // After bootnodes (discovery refuses an unpinned list) and ports (the
    // discovery UDP port follows the RLPx port). Off for dev — what
    // `.dev()` and the old hand-set flag both did.
    discovery::apply(&mut node_config.network, discovery_on, &addr_overrides)?;
    let discovery_line = discovery::describe(&node_config.network);
    // SOVA_WS_PORT: WS JSON-RPC on 127.0.0.1 (off by default; the public
    // profile forces it off again below).
    let ws_port = env_u16("SOVA_WS_PORT");
    rpc::apply_ws(&mut node_config.rpc, ws_port);
    rpc_profile.apply(&mut node_config.rpc);
    rpc_cors.apply(&mut node_config.rpc);
    let jwt = apply_shared_jwt(&mut node_config)?;
    let (http_port, auth_port) = (node_config.rpc.http_port, node_config.rpc.auth_port);

    let sova_node = SovaNode::default();
    // p2p: `sova/1` goes into the network config, so it is offered on
    // every session from the first one (discovered and inbound peers can
    // connect before `launch` returns). The service that drains
    // `sova_receivers` is spawned after launch.
    let (network_builder, sova_receivers) = gossip::network_builder(gossip);
    let (database, datadir_line) = open_datadir(&mut node_config)?;
    // SIP-6 (`SOVA_SIP6=1`): seals are required, and checked under this
    // chain's ID, from the first import on — activated before launch,
    // never mid-run.
    let sip6_chain_id = env_flag("SOVA_SIP6").then(|| node_config.chain.chain().id());
    if let Some(chain_id) = sip6_chain_id {
        engine::seal::activate(chain_id);
    }
    // SIP-7 (`SOVA_SIP7=1`): the precompile's pool reads exist and the
    // Zcash scan holds on bad pool accounting — from the first block on.
    if env_flag("SOVA_SIP7") {
        engine::zcash_index::activate_sip7();
    }

    // NOTE: bind `node` (not `_`, which drops immediately) — the `FullNode`
    // handle owns the running RPC server.
    let NodeHandle {
        node,
        node_exit_future,
    } = NodeBuilder::new(node_config)
        .with_database(database)
        .with_launch_context(runtime)
        .with_types::<SovaNode>()
        .with_components(sova_node.components_builder().network(network_builder))
        // Every RPC server reth starts (HTTP, WS), in every profile:
        // `pending` → `latest` for the three call-simulation methods.
        .with_add_ons(
            sova_node
                .add_ons()
                .layer_rpc_middleware(pending_rpc::PendingAsLatestLayer),
        )
        .extend_rpc_modules(move |ctx| {
            // SIP-7 §4.2 path A: the `sova` namespace (Zcash block feed),
            // only when SIP-7 is active. Merged before the public filter,
            // which keeps `sova_getZcashBlocks` and drops the rest.
            let sip7 = engine::zcash_index::sip7_active();
            if sip7 {
                let feed = zcash_feed::Feed::new(
                    zcash_feed::NodeChain(ctx.provider().clone()),
                    engine::zcash_index::global(),
                );
                ctx.modules.merge_configured(zcash_feed::module(feed)?)?;
                println!(
                    "sip-7 feed: sova_getZcashBlocks over HTTP{}",
                    if ws_port.is_some() && rpc_profile == rpc::RpcProfile::Local {
                        ", sova_subscribe(\"zcashBlocks\") over WS"
                    } else {
                        " (no WS: sova_subscribe unavailable; set SOVA_WS_PORT, local profile)"
                    }
                );
            }
            // `public` only: strip every HTTP method outside the
            // allowlist. `local` installs no filter (today's behaviour).
            if let Some(allowlist) = rpc_profile.method_allowlist(sip7) {
                let (kept, removed, missing) = rpc::restrict_http_methods(ctx.modules, &allowlist);
                println!(
                    "rpc profile: public (HTTP serves {kept} allowlisted method(s), removed {removed})"
                );
                if !missing.is_empty() {
                    eprintln!(
                        "rpc profile: WARNING allowlisted but not registered by reth: {}",
                        missing.join(", ")
                    );
                }
            }
            Ok(())
        })
        .launch_with_debug_capabilities()
        .await?;

    println!(
        "sova {} (pre-release, under construction)",
        env!("CARGO_PKG_VERSION")
    );
    println!("{datadir_line}");
    println!("{}", rpc_cors.describe());
    // Branch rule (audit 2026-09-23 F1): fork choice only considers
    // candidates attached to our own chain, which needs our canonical hashes.
    // SIP-4 §7: after a Zcash reorg, blocks above the rollback floor are
    // stale, not ours, so the re-sealed branch extends our (effective) head
    // however many stale blocks it replaces.
    {
        let reader_provider = node.provider.clone();
        engine::candidates::set_canonical_reader(Box::new(move |h| {
            let head = reader_provider.best_block_number().unwrap_or(0);
            let effective = engine::expectations::global()
                .effective_head(head, |x| canonical_anchor(&reader_provider, x));
            if h > effective {
                return None;
            }
            reader_provider.block_hash(h).ok().flatten().map(|b| b.0)
        }));
    }
    if chain_profile != chain::ChainProfile::Dev {
        println!(
            "chain profile: {} (chain ID {}, {} genesis alloc account(s))",
            chain_profile.name(),
            node.chain_spec().chain.id(),
            node.chain_spec().genesis.alloc.len()
        );
    }

    let base_height: u64 = std::env::var("SOVA_EPOCH_BASE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    // SIP-4: the Zcash query precompile answers from the node's Zcash
    // index, fed by the expectations follower (empty without a zebrad, so
    // calls are fatal: such a node cannot execute blocks that use the
    // precompile). EVMs resolve the source per block.
    engine::zcash_index::install(base_height);

    // SIP-3 emission schedule (SOVA_EMISSION_SCHEDULE): "flat" (default
    // — regtest/box determinism, the draft 6,250/epoch) or "sip3" (slow
    // start + halving eras; public testnet/mainnet). One value feeds
    // the sealer, the expectations follower, AND the validator's
    // process-global — they must agree or the node rejects its own
    // blocks.
    let schedule = match std::env::var("SOVA_EMISSION_SCHEDULE").as_deref() {
        Ok("sip3") => consensus::schedule::Schedule::Sip3,
        Ok("flat") | Err(_) => consensus::schedule::Schedule::Flat {
            reward_gwei: DRAFT_EPOCH_REWARD_GWEI,
        },
        Ok(other) => {
            return Err(eyre::eyre!(
                "SOVA_EMISSION_SCHEDULE must be \"flat\" or \"sip3\", got {other:?}"
            ));
        }
    };
    engine::expectations::set_schedule(schedule);

    // C5 + v2 preference, for every mode that has a Zcash view: the
    // expectations follower derives each height's valid settlements from
    // our OWN zebrad (the validator rejects contradictions), and the
    // arbiter adopts whichever imported candidate wins rank-then-hash
    // preference. Without a zebrad the node imports on trust — say so.
    // sova/1's service before the arbiter, sealer, and miner (static
    // peers dialed as early as possible), so the first sealed block
    // already has somewhere to go. The service only ever calls
    // `new_payload`; head moves stay with the arbiter below.
    if gossip == gossip::Gossip::P2p {
        if zebrad_rpc.is_none() {
            eprintln!(
                "p2p: WARNING no SOVA_ZEBRAD_RPC, so no arbiter: blocks received over sova/1 are validated but never adopted as head"
            );
        }
        let receivers =
            sova_receivers.ok_or_else(|| eyre::eyre!("p2p transport without sova/1 receivers"))?;
        gossip::start(
            node.provider.clone(),
            node.add_ons_handle.beacon_engine_handle.clone(),
            node.network.clone(),
            receivers,
            p2p_peers,
        );
        println!("{discovery_line}");
        println!(
            "p2p: engine API (authrpc) bound to {}:{auth_port} (not used for propagation)",
            node.config.rpc.auth_addr
        );
    }

    if let Some(url) = &zebrad_rpc {
        tokio::spawn(engine::expectations::run_expectations(
            ZebradClient::new(url.clone()),
            base_height,
            schedule,
            Duration::from_secs(2),
        ));
        if let Some(arbiter_rx) = engine::candidates::install_arbiter() {
            let head_provider = node.provider.clone();
            let lag_provider = node.provider.clone();
            let engine_handle = node.add_ons_handle.beacon_engine_handle.clone();
            tokio::spawn(engine::candidates::run_arbiter(
                arbiter_rx,
                // SIP-4 §7: after a Zcash reorg, stale blocks don't count,
                // so a replacement below the stale tip can be adopted.
                move || {
                    let head = head_provider.best_block_number().unwrap_or(0);
                    engine::expectations::global()
                        .effective_head(head, |h| canonical_anchor(&head_provider, h))
                },
                || {
                    Some(
                        engine::expectations::global()
                            .scanned_through()
                            .unwrap_or(0),
                    )
                },
                move |best: engine::candidates::BestCandidate| {
                    let engine_handle = engine_handle.clone();
                    // Depth-lagged safe/finalized from the canonical
                    // chain. Counted from the lower of the candidate and
                    // our effective head: a branch the arbiter adopts
                    // forks at most MAX_REPLACE_DEPTH - 1 below the
                    // effective head, so these are always ancestors of
                    // the new head. Never `same_hash`: finalizing the
                    // adopted tip would forbid the next micro-reorg.
                    let lag = {
                        let provider = lag_provider.clone();
                        let head = provider.best_block_number().unwrap_or(0);
                        let effective = engine::expectations::global()
                            .effective_head(head, |h| canonical_anchor(&provider, h));
                        let base = best.sova_height.min(effective);
                        move |depth: u64| {
                            provider
                                .block_hash(base.saturating_sub(depth))
                                .ok()
                                .flatten()
                                .unwrap_or(alloy_primitives::B256::ZERO)
                        }
                    };
                    async move {
                        let state = ForkchoiceState {
                            head_block_hash: alloy_primitives::B256::from(best.block_hash),
                            safe_block_hash: lag(engine::candidates::SAFE_DEPTH),
                            finalized_block_hash: lag(engine::candidates::FINALIZED_DEPTH),
                        };
                        match engine_handle.fork_choice_updated(state, None).await {
                            Ok(outcome) if outcome.payload_status.is_valid() => Ok(()),
                            Ok(outcome) => Err(format!("{:?}", outcome.payload_status.status)),
                            Err(err) => Err(err.to_string()),
                        }
                    }
                },
            ));
        }
        // Late-join catch-up: sova/1 hands over tips beyond its bounded
        // ancestor chase; the driver FCUs toward them only once our own
        // zebrad scan covers the target (every synced block then meets an
        // enforced C5 check). Zero safe/finalized: reth backfills to the
        // head optimistically only when finalized is unset.
        if let Some(sync_rx) = engine::candidates::install_sync() {
            let head_provider = node.provider.clone();
            let engine_handle = node.add_ons_handle.beacon_engine_handle.clone();
            tokio::spawn(engine::candidates::run_sync_driver(
                sync_rx,
                move || head_provider.best_block_number().unwrap_or(0),
                // With a zebrad, "nothing scanned yet" means wait, not "no gate".
                || {
                    Some(
                        engine::expectations::global()
                            .scanned_through()
                            .unwrap_or(0),
                    )
                },
                move |target: engine::candidates::SyncTarget| {
                    let engine_handle = engine_handle.clone();
                    async move {
                        let state = ForkchoiceState {
                            head_block_hash: alloy_primitives::B256::from(target.block_hash),
                            safe_block_hash: alloy_primitives::B256::ZERO,
                            finalized_block_hash: alloy_primitives::B256::ZERO,
                        };
                        match engine_handle.fork_choice_updated(state, None).await {
                            Ok(outcome) if outcome.payload_status.is_valid() => Ok(()),
                            Ok(outcome) => Err(format!("{:?}", outcome.payload_status.status)),
                            Err(err) => Err(err.to_string()),
                        }
                    }
                },
            ));
        }
        println!(
            "expectations: enforcing settlements against zebrad at {url} (epoch base {base_height})"
        );
    } else {
        println!("no SOVA_ZEBRAD_RPC: importing without settlement enforcement (C5 off)");
    }

    if follow_only {
        match gossip {
            gossip::Gossip::Relay => println!(
                "follow-only mode: no local mining; serving RPC on :{http_port}, accepting relayed blocks via authrpc on :{auth_port}"
            ),
            gossip::Gossip::P2p => println!(
                "follow-only mode: no local mining; serving RPC on :{http_port}, receiving blocks over sova/1"
            ),
        }
    } else if let Some(zebrad_url) = mine_rpc {
        let peers = parse_peers();
        if gossip == gossip::Gossip::Relay && !peers.is_empty() {
            let jwt = jwt.ok_or_else(|| {
                eyre::eyre!(
                    "SOVA_PEERS is set but SOVA_AUTH_JWT is not; the relay needs a JWT its peers share"
                )
            })?;
            // Spawned before the sealer so the relay's own block-1 floor
            // (see `run_relay`'s doc comment) is never racing the first
            // trigger.
            let head_provider = node.provider.clone();
            let fetch_provider = node.provider.clone();
            let relay_peers = peers.clone();
            tokio::spawn(run_relay(
                move || head_provider.best_block_number().unwrap_or(0),
                move |height| {
                    let block = fetch_provider.block_by_number(height).ok().flatten()?;
                    Some(SovaEngineTypes::block_to_payload(
                        SealedBlock::new_unhashed(block),
                        None,
                    ))
                },
                relay_peers,
                jwt,
                Duration::from_secs(1),
            ));
            println!(
                "relay: pushing sealed blocks to {} peer(s): {}",
                peers.len(),
                peers.join(", ")
            );
        }

        let signer = match sip6_chain_id {
            Some(chain_id) => Some(std::sync::Arc::new(load_signer(chain_id)?)),
            None => None,
        };
        let our_address = match &signer {
            Some(signer) => sealer_address(signer.address().into())?,
            None => parse_miner_address()?,
        };
        let pending = PendingEpoch::default();
        let attrs_builder =
            SovaLocalPayloadAttributesBuilder::with_pending(node.chain_spec(), pending.clone())
                .with_fee_recipient(our_address.into());
        let (trigger_tx, trigger_rx) = tokio::sync::mpsc::channel::<engine::miner::BuildTarget>(16);

        // SovaMiner, not reth's LocalMiner: the stock miner re-asserts
        // its own built lineage as forkchoice and would fight the v2
        // arbiter (see crates/engine/src/miner.rs's module doc).
        let miner = engine::miner::SovaMiner::new(
            node.provider.clone(),
            attrs_builder,
            node.add_ons_handle.beacon_engine_handle.clone(),
            trigger_rx,
            node.payload_builder_handle.clone(),
        );
        let miner = match signer {
            Some(signer) => {
                println!("sip-6: sealing as 0x{}", hex::encode(signer.address()));
                miner.with_signer(signer)
            }
            None => miner,
        };
        tokio::spawn(miner.run());

        // Ladder step (SOVA_RANK_STEP_SECS): how long each successive
        // rank waits before sealing in the rank-0 sealer's absence.
        // Scenario scripts shrink it; the default is SIP-2's draft 15s.
        let rank_step = std::env::var("SOVA_RANK_STEP_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map_or(consensus::sealer::DEFAULT_RANK_STEP, Duration::from_secs);
        let core = SealerCore::new(
            SealerConfig {
                our_address,
                schedule,
                rank_step,
                sip6: sip6_chain_id.is_some(),
            },
            base_height,
            100,
        );
        let head_provider = node.provider.clone();
        tokio::spawn(run_sealer(
            core,
            ZebradClient::new(zebrad_url.clone()),
            // SIP-4 §7: stale blocks above a Zcash rollback floor are re-sealed.
            move || {
                let head = head_provider.best_block_number().unwrap_or(0);
                engine::expectations::global()
                    .effective_head(head, |h| canonical_anchor(&head_provider, h))
            },
            pending,
            trigger_tx,
            Duration::from_secs(2),
        ));

        println!(
            "mine mode: following zebrad at {zebrad_url}, epoch base {base_height}, one Sova block per Zcash block"
        );
    } else {
        println!(
            "dev chain launched: reth v2.6.0 SovaNode, auto-mining, HTTP RPC on 127.0.0.1:{http_port}"
        );
    }

    node_exit_future.await
}

/// SIP-6: the sealing key from `SOVA_SEALER_KEYSTORE` (a `sova-miner`
/// keystore), journaling under the datadir (persistent) or a per-process
/// temp dir (ephemeral), one journal per sealing address.
fn load_signer(chain_id: u64) -> eyre::Result<engine::signer::Signer> {
    let keystore = std::env::var_os("SOVA_SEALER_KEYSTORE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            eyre::eyre!(
                "SOVA_SIP6=1 mine mode requires SOVA_SEALER_KEYSTORE (a sova-miner keystore.json)"
            )
        })?;
    let base = std::env::var_os("SOVA_DATADIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("sova-{}", std::process::id())))
        .join("seal-journal");
    // The address isn't known until the key loads; load once to learn it.
    let probe = engine::signer::Signer::from_keystore(&keystore, chain_id, base.join("probe"))?;
    let journal = base.join(format!("{}", probe.address()));
    let _ = std::fs::remove_dir_all(base.join("probe"));
    Ok(engine::signer::Signer::from_keystore(
        &keystore, chain_id, journal,
    )?)
}

/// SIP-6: the sealer's own address is the one its burns credit. If
/// `SOVA_MINER_EVM_ADDRESS` is also set it must agree.
fn sealer_address(signer: [u8; 20]) -> eyre::Result<[u8; 20]> {
    if std::env::var("SOVA_MINER_EVM_ADDRESS").is_ok_and(|v| !v.trim().is_empty()) {
        let configured = parse_miner_address()?;
        if configured != signer {
            return Err(eyre::eyre!(
                "SOVA_MINER_EVM_ADDRESS 0x{} is not the sealing key's address 0x{}: burns must credit the key that seals",
                hex::encode(configured),
                hex::encode(signer)
            ));
        }
    }
    Ok(signer)
}

/// Parse `SOVA_MINER_EVM_ADDRESS` (0x-prefixed 20-byte hex).
fn parse_miner_address() -> eyre::Result<[u8; 20]> {
    let raw = std::env::var("SOVA_MINER_EVM_ADDRESS")
        .map_err(|_| eyre::eyre!("mine mode requires SOVA_MINER_EVM_ADDRESS"))?;
    let hexstr = raw.strip_prefix("0x").unwrap_or(&raw);
    let bytes = hex::decode(hexstr).map_err(|e| eyre::eyre!("bad SOVA_MINER_EVM_ADDRESS: {e}"))?;
    bytes
        .try_into()
        .map_err(|_| eyre::eyre!("SOVA_MINER_EVM_ADDRESS must be 20 bytes"))
}

/// `SOVA_PEERS`: a comma-separated list of peer authrpc URLs to relay
/// sealed blocks to (gossip v1). Empty/unset means no relay task runs.
fn parse_peers() -> Vec<String> {
    std::env::var("SOVA_PEERS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// A boolean env flag: set and exactly `"1"` means on.
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// Reads an env var as a `u16`, e.g. a port override.
fn env_u16(name: &str) -> Option<u16> {
    std::env::var(name).ok().and_then(|s| s.parse().ok())
}

/// `SOVA_HTTP_PORT` / `SOVA_AUTH_PORT` / `SOVA_P2P_PORT`: let a mine-mode
/// and a follow-only node run on the same host without colliding on
/// reth's default ports.
///
/// Also unconditionally disables the IPC server: its default endpoint
/// (`/tmp/reth.ipc`) is a single fixed path, not per-datadir, so two
/// `bin/sova` processes on one host would otherwise race to bind it —
/// the same problem these env vars solve for HTTP/authrpc/p2p, just
/// without a knob of its own since nothing in this codebase uses IPC.
fn apply_port_overrides(node_config: &mut NodeConfig<ChainSpec>) {
    node_config.rpc.ipcdisable = true;
    if let Some(port) = env_u16("SOVA_HTTP_PORT") {
        node_config.rpc.http_port = port;
    }
    if let Some(port) = env_u16("SOVA_AUTH_PORT") {
        node_config.rpc.auth_port = port;
    }
    if let Some(port) = env_u16("SOVA_P2P_PORT") {
        node_config.network.port = port;
    }
}

/// `SOVA_DATADIR`: where the node keeps its database, static files and p2p
/// identity.
///
/// - **Unset** (the box, the sims, CI): a fresh `$TMPDIR/reth-test-*`
///   directory per start with reth's small test-database geometry — the
///   datadir `testing_node` used to create, so every start is a new chain
///   and a new node key, exactly as before.
/// - **Set** (any long-lived node, e.g. `infra/testnet`): that directory,
///   created if missing, reused across restarts, with reth's production
///   MDBX geometry. The p2p secret key lives at `<dir>/discovery-secret`
///   (created on first start), so the node's enode — which bootnode lists
///   pin — survives restarts, and a restart resumes from the stored chain
///   instead of re-syncing it.
fn open_datadir(
    node_config: &mut NodeConfig<ChainSpec>,
) -> eyre::Result<(
    std::sync::Arc<reth_ethereum::provider::db::DatabaseEnv>,
    String,
)> {
    use reth_ethereum::{
        node::core::{
            args::DatadirArgs,
            dirs::{DataDirPath, MaybePlatformPath},
        },
        provider::db::{ClientVersion, init_db, mdbx::DatabaseArguments, test_utils},
    };

    let persistent = std::env::var_os("SOVA_DATADIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let (dir, args, line) = match persistent {
        Some(dir) => {
            std::fs::create_dir_all(&dir)
                .map_err(|e| eyre::eyre!("SOVA_DATADIR {}: {e}", dir.display()))?;
            let line = format!(
                "datadir: {} (persistent; node key {})",
                dir.display(),
                dir.join("discovery-secret").display()
            );
            (dir, DatabaseArguments::new(ClientVersion::default()), line)
        }
        None => {
            let dir = test_utils::tempdir_path();
            let line = format!(
                "datadir: {} (ephemeral: new chain and node key every start; set SOVA_DATADIR to keep them)",
                dir.display()
            );
            (dir, DatabaseArguments::test(), line)
        }
    };
    node_config.datadir = DatadirArgs {
        datadir: MaybePlatformPath::<DataDirPath>::from(dir.clone()),
        ..Default::default()
    };
    let db = init_db(dir.join("db"), args)
        .map_err(|e| eyre::eyre!("opening the node database under {}: {e}", dir.display()))?;
    Ok((std::sync::Arc::new(db), line))
}

/// Wires `SOVA_AUTH_JWT` (a path) into the node's own authrpc secret, and
/// returns the loaded secret for the relay task to sign outgoing calls
/// with — both nodes in a gossip-v1 box point this at the same file, so
/// each one's authrpc accepts the other's relayed calls. Absent, the node
/// falls back to reth's own default (a secret generated under its
/// datadir), which no peer would share — fine for a lone dev/mine-mode
/// node, but relay setup below requires this to be set.
fn apply_shared_jwt(node_config: &mut NodeConfig<ChainSpec>) -> eyre::Result<Option<JwtSecret>> {
    let Some(raw) = std::env::var("SOVA_AUTH_JWT").ok() else {
        return Ok(None);
    };
    let path = PathBuf::from(raw);
    if !path.exists() {
        // Convenience for standalone/manual use; the two-node scenario
        // script creates this file itself before launching either node,
        // so in that flow both processes find it already there and this
        // branch never runs (never races a concurrent create).
        JwtSecret::try_create_random(&path)?;
    }
    let secret = JwtSecret::from_file(&path)?;
    node_config.rpc.auth_jwtsecret = Some(path);
    Ok(Some(secret))
}

/// The canonical block's SIP-4 Zcash anchor (`parent_beacon_block_root`)
/// at `height`, if any.
fn canonical_anchor<P: reth_ethereum::provider::HeaderProvider>(
    provider: &P,
    height: u64,
) -> Option<[u8; 32]> {
    use reth_ethereum::primitives::AlloyBlockHeader;
    provider
        .sealed_header(height)
        .ok()
        .flatten()
        .and_then(|h| h.parent_beacon_block_root())
        .map(|r| r.0)
}
