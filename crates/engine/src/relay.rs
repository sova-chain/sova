//! Gossip v1's relay task (`docs/design/gossip-v1.md`).
//!
//! A sealed Sova block already has a canonical wire form: the Engine API
//! execution payload. Instead of a new gossip protocol, v1 relays blocks
//! peer-to-peer over the Engine API itself: watch the local chain head, and
//! for each new block, POST it to every configured static peer's authrpc as
//! `engine_newPayloadV4` followed by `engine_forkchoiceUpdatedV3` to that
//! head. The receiving node validates the payload through its full
//! stateful engine path — the same validation a real network would
//! perform; this task is transport only.

use std::time::Duration;

use alloy_rpc_types::engine::ExecutionData;
use reth_rpc_layer::{Claims, JwtSecret};
use serde_json::{Value, json};

/// Watches the local chain head and relays each new block to every
/// configured peer's authrpc.
///
/// `chain_head` and `fetch_block` mirror [`crate::driver::run_sealer`]'s
/// closure-pair pattern rather than taking a provider handle directly, so
/// this task stays agnostic of reth's node-internal types. `fetch_block` is
/// expected to have already converted the sealed block via
/// `SovaEngineTypes::block_to_payload` (`PayloadTypes`) — this task only
/// assembles wire params from the [`ExecutionData`] it's handed and does
/// not know about `SovaEngineTypes` itself.
///
/// A fresh JWT is minted per request (`iat` = now, the Engine API's own
/// freshness requirement) and sent as `Authorization: Bearer`.
///
/// Relaying always starts from height 1: both nodes in a gossip-v1 box
/// share the same genesis by construction (same chain spec), so there is
/// nothing to relay at height 0. Starting `last_relayed` at a fixed floor
/// (rather than sampling `chain_head()` on entry) guarantees every block a
/// peer needs gets pushed even if this task happens to be spawned a moment
/// before the sealer produces its first block — callers should still spawn
/// this before the sealer/miner to keep that window closed.
///
/// Rollback handling: v1 has no bounded micro-reorg handling yet (see
/// `docs/design/gossip-v1.md`'s "honest v1 limits"). If the local head is
/// ever observed lower than the last relayed height, this task documents
/// rather than special-cases the situation: it simply resumes relaying
/// forward from the new head. It does not attempt to un-relay the
/// now-orphaned blocks to peers — each peer converges on the rollback
/// independently once its own view of the chain (or its own relay, if it
/// has peers of its own) catches up.
///
/// Per-peer errors (unreachable peer, rejected payload, JWT/transport
/// failure) are logged and do not stop the loop or affect other peers.
pub async fn run_relay(
    chain_head: impl Fn() -> u64,
    fetch_block: impl Fn(u64) -> Option<ExecutionData>,
    peers: Vec<String>,
    jwt: JwtSecret,
    poll_interval: Duration,
) {
    if peers.is_empty() {
        tracing::info!("relay: no peers configured (SOVA_PEERS empty); task idle");
    }

    let mut last_relayed: u64 = 0;

    loop {
        let head = chain_head();
        if head < last_relayed {
            tracing::warn!(
                head,
                last_relayed,
                "relay: local head rolled back; resuming relay from the new head"
            );
            last_relayed = head;
        }
        while last_relayed < head {
            let next = last_relayed + 1;
            let Some(data) = fetch_block(next) else {
                tracing::warn!(
                    height = next,
                    "relay: head advanced past this height but the block isn't fetchable yet; retrying next poll"
                );
                break;
            };
            relay_block(&peers, &jwt, next, data).await;
            last_relayed = next;
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Assembles one block's `engine_newPayloadV4` / `engine_forkchoiceUpdatedV3`
/// params and pushes them to every peer, isolating each peer's failures.
async fn relay_block(peers: &[String], jwt: &JwtSecret, height: u64, data: ExecutionData) {
    if peers.is_empty() {
        return;
    }

    let ExecutionData { payload, sidecar } = data;
    let block_hash = payload.block_hash();

    // v1's blocks are always Cancun+Prague-shaped (the dev chain spec
    // activates every hardfork through Prague at genesis, and Sova's
    // payload builder never computes a block access list), so this is
    // expected to always succeed; the error path exists so a future
    // hardfork change fails loudly here rather than relaying a malformed
    // call.
    let Some(payload_v3) = payload.as_v3() else {
        tracing::error!(
            height,
            %block_hash,
            "relay: sealed block is not V3-shaped (pre-Cancun, or BAL-bearing/Amsterdam); \
             gossip v1 only relays engine_newPayloadV4-shaped blocks, skipping"
        );
        return;
    };
    let Some(parent_beacon_block_root) = sidecar.parent_beacon_block_root() else {
        tracing::error!(
            height,
            %block_hash,
            "relay: sealed block has no parent beacon block root; newPayloadV4 requires \
             Cancun fields, skipping"
        );
        return;
    };
    let versioned_hashes = sidecar.versioned_hashes().cloned().unwrap_or_default();
    // `ExecutionPayloadSidecar::from_block` (used by
    // `SovaEngineTypes::block_to_payload`) always recovers the Prague
    // fields as `RequestsOrHash::Hash` (the header's `requests_hash`), not
    // the original request list — exactly the shape a relay between two
    // nodes that compute identically needs: the receiving node's own
    // validator re-derives and compares the hash, it doesn't need the
    // (here, always-empty) request bytes themselves.
    let execution_requests = sidecar
        .into_prague()
        .map(|fields| fields.requests)
        .unwrap_or_default();

    let new_payload_params = json!([
        payload_v3,
        versioned_hashes,
        parent_beacon_block_root,
        execution_requests
    ]);

    // ureq is blocking; do the peer round-trips off the async runtime's
    // worker thread. One `spawn_blocking` per block, looping over peers
    // sequentially inside it, keeps this small — v1 targets a handful of
    // static box-scale peers, not a fan-out that needs its own
    // concurrency.
    let peers_owned = peers.to_vec();
    let jwt = *jwt;
    let outcome = tokio::task::spawn_blocking(move || {
        peers_owned
            .into_iter()
            .map(|peer| {
                let result = relay_to_peer(&peer, &jwt, &new_payload_params);
                (peer, result)
            })
            .collect::<Vec<_>>()
    })
    .await;

    match outcome {
        Ok(results) => {
            for (peer, result) in results {
                match result {
                    Ok(()) => {
                        tracing::info!(height, %block_hash, peer, "relay: peer accepted block");
                    }
                    Err(err) => {
                        tracing::warn!(
                            height,
                            %block_hash,
                            peer,
                            %err,
                            "relay: peer failed; continuing with remaining peers"
                        );
                    }
                }
            }
        }
        Err(join_err) => {
            tracing::warn!(height, %block_hash, %join_err, "relay: blocking relay task panicked");
        }
    }
}

/// Pushes one block to one peer: `engine_newPayloadV4` only, with a
/// fresh JWT. **The relay delivers; the receiver's arbiter decides.**
/// v1 also sent `engine_forkchoiceUpdatedV3`, which made the receiver
/// adopt the sender's head on trust — with two producers that yanks a
/// peer off a *preferred* lineage the moment a lagging node catches up
/// (seen live in the ladder scenario). Since v2 the receiver observes
/// every imported block ([`crate::SovaEngineValidator`]) and its own
/// FCU arbiter adopts the preference winner, so a forced FCU here is
/// not just unnecessary — it is the one message that can override
/// another node's correct fork choice.
fn relay_to_peer(
    authrpc_url: &str,
    jwt: &JwtSecret,
    new_payload_params: &Value,
) -> Result<(), RelayError> {
    call_authrpc(
        authrpc_url,
        jwt,
        "engine_newPayloadV4",
        new_payload_params.clone(),
    )?;
    Ok(())
}

/// One JSON-RPC call to a peer's authrpc, authenticated with a freshly
/// minted JWT (`iat` = now) as `Authorization: Bearer`. Plain HTTP, blocking
/// — the peer is always local or on a trusted link inside the box, matching
/// `crates/consensus/src/zebrad.rs`'s own client.
fn call_authrpc(
    url: &str,
    jwt: &JwtSecret,
    method: &str,
    params: Value,
) -> Result<Value, RelayError> {
    let token = jwt
        .encode(&Claims::with_current_timestamp())
        .map_err(|e| RelayError::Jwt(e.to_string()))?;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let resp = ureq::post(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(5))
        .send_string(&body.to_string())
        .map_err(|e| RelayError::Transport(format!("{method}: {e}")))?;
    let text = resp
        .into_string()
        .map_err(|e| RelayError::Transport(format!("{method}: bad response body: {e}")))?;
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|e| RelayError::Transport(format!("{method}: bad response json: {e}")))?;
    if let Some(error) = parsed.get("error").filter(|v| !v.is_null()) {
        return Err(RelayError::Rpc(format!("{method}: {error}")));
    }
    Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
}

/// Why one peer's relay attempt failed. Always logged and swallowed by the
/// caller — never fatal to the relay loop.
#[derive(Debug, thiserror::Error)]
enum RelayError {
    /// Minting the per-request JWT failed.
    #[error("jwt encode failed: {0}")]
    Jwt(String),
    /// The HTTP request itself failed (connect/timeout/bad response body).
    #[error("transport: {0}")]
    Transport(String),
    /// The peer answered with a JSON-RPC error object.
    #[error("rpc error: {0}")]
    Rpc(String),
}
