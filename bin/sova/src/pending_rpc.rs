//! `pending` answered as `latest` for the three call-simulation methods.
//!
//! `eth_call`, `eth_estimateGas` and `eth_createAccessList` at block tag
//! `pending` would otherwise run against reth's *pending* EVM env: the head
//! block's state, but a synthetic head+1 block (`rpc-eth-api`'s
//! `evm_env_at` → `pending_block_env_and_cfg`). On Sova, block N anchors
//! Zcash epoch N+B−1, and the epoch for head+1 does not exist yet, so any
//! call that reaches the SIP-4 precompile (0x…5A00) fails with
//! "zcash index does not cover anchored height …". That precondition is
//! right for block execution and stays; the RPC layer is what changes.
//! `cast send` (and most wallets) estimate gas at `pending`, so without
//! this a plain `cast send` to any contract that reads Zcash fails.
//!
//! [`PendingAsLatestLayer`] is a jsonrpsee RPC middleware that rewrites
//! the block parameter `"pending"` to `"latest"` for exactly
//! [`PENDING_AS_LATEST_METHODS`], before reth parses it. Everything else
//! passes through untouched: other methods keep reth's pending semantics
//! (`eth_getTransactionCount`/`eth_getBalance` at `pending`,
//! `eth_getBlockByNumber("pending")`, …), and so do these three at any
//! other block (`latest`, a number, a hash, or no block at all — reth
//! already treats an omitted block as `latest`).
//!
//! Recognised shapes, as reth's jsonrpsee server parses them:
//! - positional params (`[call, block, …]`), block at index 1;
//! - named params (`{"request": …, "block_number": …}`; jsonrpsee also
//!   accepts the key `blockNumber`);
//! - the block as a tag string (`"pending"`, any ASCII case, as alloy's
//!   parser accepts) or as an EIP-1898 object (`{"blockNumber":"pending"}`).
//!
//! Only the block value is re-encoded; every other param keeps its exact
//! bytes. The layer handles single calls and each call inside a batch
//! (jsonrpsee runs batches through `RpcServiceT::batch`, not `call`).
//!
//! Transports: reth applies RPC middleware to its HTTP and WS servers
//! (separate or shared port) but, in v2.6.0, not to IPC — `bin/sova`
//! always disables IPC (`apply_port_overrides`), so every transport it
//! serves is covered. The authenticated Engine API server (JWT, authrpc)
//! is not a user surface and is left alone.

use std::{borrow::Cow, collections::BTreeMap, future::Future};

use jsonrpsee::server::middleware::rpc::{Batch, BatchEntry, Notification, Request, RpcServiceT};
use serde_json::{Value, value::RawValue};
use tower::Layer;

/// The methods whose `pending` block parameter is answered as `latest`.
pub(crate) const PENDING_AS_LATEST_METHODS: [&str; 3] =
    ["eth_call", "eth_estimateGas", "eth_createAccessList"];

/// Index of the block parameter in positional params (all three methods
/// take `(request, block, …)`).
const BLOCK_PARAM_INDEX: usize = 1;

/// Keys jsonrpsee's server accepts for reth's `block_number` argument in
/// named params (the argument name and its lowerCamelCase alias).
const BLOCK_PARAM_KEYS: [&str; 2] = ["block_number", "blockNumber"];

/// The params `method` should run with instead of `params`, or `None`
/// when nothing changes (another method, or a block other than pending).
pub(crate) fn rewrite_params(method: &str, params: &RawValue) -> Option<Box<RawValue>> {
    if !PENDING_AS_LATEST_METHODS.contains(&method) {
        return None;
    }
    let text = params.get().trim_start();
    let encoded = if text.starts_with('[') {
        let mut items: Vec<&RawValue> = serde_json::from_str(text).ok()?;
        let latest = latest_if_pending(items.get(BLOCK_PARAM_INDEX)?)?;
        items[BLOCK_PARAM_INDEX] = &latest;
        serde_json::to_string(&items).ok()?
    } else if text.starts_with('{') {
        let mut fields: BTreeMap<String, &RawValue> = serde_json::from_str(text).ok()?;
        let mut replaced = Vec::new();
        for key in BLOCK_PARAM_KEYS {
            if let Some(latest) = fields.get(key).and_then(|b| latest_if_pending(b)) {
                replaced.push((key, latest));
            }
        }
        if replaced.is_empty() {
            return None;
        }
        for (key, latest) in &replaced {
            fields.insert((*key).to_owned(), latest);
        }
        serde_json::to_string(&fields).ok()?
    } else {
        return None;
    };
    RawValue::from_string(encoded).ok()
}

/// `"latest"` in the same shape as `block`, if `block` names the pending
/// block; `None` for anything else (including values reth would reject,
/// which then fail in reth exactly as before).
fn latest_if_pending(block: &RawValue) -> Option<Box<RawValue>> {
    let latest = match serde_json::from_str::<Value>(block.get()).ok()? {
        Value::String(tag) if is_pending(&tag) => Value::from("latest"),
        Value::Object(mut id) => {
            match id.get("blockNumber") {
                Some(Value::String(tag)) if is_pending(tag) => {}
                _ => return None,
            }
            id.insert("blockNumber".to_owned(), Value::from("latest"));
            Value::Object(id)
        }
        _ => return None,
    };
    RawValue::from_string(serde_json::to_string(&latest).ok()?).ok()
}

/// alloy parses block tags case-insensitively.
fn is_pending(tag: &str) -> bool {
    tag.eq_ignore_ascii_case("pending")
}

/// Rewrite one request in place (see [`rewrite_params`]).
fn rewrite_request(req: &mut Request<'_>) {
    let rewritten = req
        .params
        .as_deref()
        .and_then(|params| rewrite_params(&req.method, params));
    if let Some(params) = rewritten {
        req.params = Some(Cow::Owned(params));
    }
}

/// Installs [`PendingAsLatest`] on reth's RPC servers (see the module doc).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PendingAsLatestLayer;

impl<S> Layer<S> for PendingAsLatestLayer {
    type Service = PendingAsLatest<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PendingAsLatest { inner }
    }
}

/// The per-connection RPC service [`PendingAsLatestLayer`] builds.
#[derive(Debug, Clone)]
pub(crate) struct PendingAsLatest<S> {
    inner: S,
}

impl<S> RpcServiceT for PendingAsLatest<S>
where
    S: RpcServiceT + Send + Sync + Clone + 'static,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(
        &self,
        mut req: Request<'a>,
    ) -> impl Future<Output = S::MethodResponse> + Send + 'a {
        rewrite_request(&mut req);
        self.inner.call(req)
    }

    fn batch<'a>(
        &self,
        mut batch: Batch<'a>,
    ) -> impl Future<Output = S::BatchResponse> + Send + 'a {
        for entry in batch.iter_mut().flatten() {
            if let BatchEntry::Call(req) = entry {
                rewrite_request(req);
            }
        }
        self.inner.batch(batch)
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = S::NotificationResponse> + Send + 'a {
        // No response, so nothing to answer at `latest`.
        self.inner.notification(n)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use jsonrpsee::{
        RpcModule,
        server::{Server, middleware::rpc::RpcServiceBuilder},
    };

    fn raw(json: &str) -> Box<RawValue> {
        RawValue::from_string(json.to_owned()).unwrap()
    }

    /// The rewritten params as a JSON value, or `None` if unchanged.
    fn rewrite(method: &str, params: &str) -> Option<Value> {
        rewrite_params(method, &raw(params)).map(|p| serde_json::from_str(p.get()).unwrap())
    }

    const CALL: &str = r#"{"to":"0x0000000000000000000000000000000000005a00","data":"0xd3fb73b4"}"#;

    #[test]
    fn pending_becomes_latest_for_exactly_the_three_methods() {
        for method in PENDING_AS_LATEST_METHODS {
            let got = rewrite(method, &format!(r#"[{CALL},"pending"]"#)).unwrap();
            assert_eq!(got[1], "latest", "{method}");
            assert_eq!(got[0], serde_json::from_str::<Value>(CALL).unwrap());
        }
        // Everything else keeps reth's pending semantics.
        for (method, params) in [
            (
                "eth_getTransactionCount",
                r#"["0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266","pending"]"#,
            ),
            (
                "eth_getBalance",
                r#"["0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266","pending"]"#,
            ),
            ("eth_getBlockByNumber", r#"["pending",false]"#),
            ("eth_callMany", r#"[[],"pending"]"#),
            ("eth_simulateV1", r#"[{},"pending"]"#),
            ("eth_estimategas", r#"[{},"pending"]"#),
        ] {
            assert_eq!(rewrite(method, params), None, "{method}");
        }
    }

    #[test]
    fn other_blocks_and_omitted_blocks_are_untouched() {
        for params in [
            format!("[{CALL}]"),
            format!("[{CALL},null]"),
            format!(r#"[{CALL},"latest"]"#),
            format!(r#"[{CALL},"safe"]"#),
            format!(r#"[{CALL},"0x10"]"#),
            format!(r#"[{CALL},"0x{}"]"#, "ab".repeat(32)),
            format!(r#"[{CALL},{{"blockHash":"0x{}"}}]"#, "ab".repeat(32)),
            format!(r#"[{CALL},{{"blockNumber":"0x10"}}]"#),
            // "pending" in another position is not the block parameter.
            format!(r#"["pending",{CALL}]"#),
            r#"[]"#.to_owned(),
            r#"{"request":{},"block_number":"latest"}"#.to_owned(),
            r#"{"request":{}}"#.to_owned(),
            // Params that aren't an array or object reach reth as they
            // were (and fail there). jsonrpsee rejects invalid JSON before
            // any middleware runs.
            r#""pending""#.to_owned(),
            "null".to_owned(),
        ] {
            for method in PENDING_AS_LATEST_METHODS {
                assert_eq!(rewrite(method, &params), None, "{method} {params}");
            }
        }
    }

    #[test]
    fn every_pending_spelling_reth_accepts() {
        let at = |params: &str| rewrite("eth_call", params).unwrap();
        assert_eq!(at(&format!(r#"[{CALL},"PENDING"]"#))[1], "latest");
        assert_eq!(at(&format!(r#"[{CALL},"Pending"]"#))[1], "latest");
        // EIP-1898 object form.
        assert_eq!(
            at(&format!(r#"[{CALL},{{"blockNumber":"pending"}}]"#))[1],
            serde_json::json!({"blockNumber": "latest"})
        );
        // Named params, both keys jsonrpsee accepts.
        for key in BLOCK_PARAM_KEYS {
            let got = at(&format!(r#"{{"request":{CALL},"{key}":"pending"}}"#));
            assert_eq!(got[key], "latest", "{key}");
            assert_eq!(got["request"], serde_json::from_str::<Value>(CALL).unwrap());
        }
        // Whitespace around the params.
        assert_eq!(at(&format!(" \n[ {CALL} , \"pending\" ]"))[1], "latest");
    }

    #[test]
    fn other_params_keep_their_exact_bytes() {
        // State overrides with a number serde_json would round through f64,
        // and odd-but-valid spacing: only the block value is re-encoded.
        let overrides = r#"{"0x0000000000000000000000000000000000000001":{"balance":"0x1","nonce":123456789012345678901234567890}}"#;
        let call = r#"{ "to" : "0x0000000000000000000000000000000000005a00" , "value": 100000000000000000000000 }"#;
        let params = format!(r#"[{call}, "pending", {overrides}]"#);
        let got = rewrite_params("eth_estimateGas", &raw(&params)).unwrap();
        assert_eq!(got.get(), format!(r#"[{call},"latest",{overrides}]"#));
    }

    /// A jsonrpsee server with the layer installed, whose methods echo the
    /// params they receive: proves the rewrite reaches the handler for
    /// single calls and batches, and nothing else changes on the wire.
    #[tokio::test(flavor = "multi_thread")]
    async fn layer_rewrites_on_a_real_server_including_batches() {
        const ECHO: [&str; 5] = [
            "eth_call",
            "eth_estimateGas",
            "eth_createAccessList",
            "eth_getTransactionCount",
            "eth_getBlockByNumber",
        ];
        let mut module = RpcModule::new(());
        for method in ECHO {
            module
                .register_method(method, |params, _, _| {
                    serde_json::from_str::<Value>(params.as_str().unwrap_or("null")).unwrap()
                })
                .unwrap();
        }
        let server = Server::builder()
            .set_rpc_middleware(RpcServiceBuilder::new().layer(PendingAsLatestLayer))
            .build("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let handle = server.start(module);

        let post = move |body: String| {
            let url = url.clone();
            async move {
                tokio::task::spawn_blocking(move || {
                    let text = ureq::post(&url)
                        .set("content-type", "application/json")
                        .send_string(&body)
                        .unwrap()
                        .into_string()
                        .unwrap();
                    serde_json::from_str::<Value>(&text).unwrap()
                })
                .await
                .unwrap()
            }
        };
        let req = |id: u32, method: &str, params: &str| {
            format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#)
        };
        let addr = "\"0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266\"";

        for method in ["eth_call", "eth_estimateGas", "eth_createAccessList"] {
            let got = post(req(1, method, &format!(r#"[{CALL},"pending"]"#))).await;
            assert_eq!(got["result"][1], "latest", "{method}: {got}");
        }
        let got = post(req(
            1,
            "eth_getTransactionCount",
            &format!(r#"[{addr},"pending"]"#),
        ))
        .await;
        assert_eq!(got["result"][1], "pending", "{got}");
        let got = post(req(1, "eth_getBlockByNumber", r#"["pending",false]"#)).await;
        assert_eq!(got["result"][0], "pending", "{got}");
        let got = post(req(1, "eth_call", &format!("[{CALL}]"))).await;
        assert_eq!(got["result"].as_array().unwrap().len(), 1, "{got}");

        let batch = format!(
            "[{},{},{}]",
            req(1, "eth_estimateGas", &format!(r#"[{CALL},"pending"]"#)),
            req(
                2,
                "eth_getTransactionCount",
                &format!(r#"[{addr},"pending"]"#)
            ),
            req(
                3,
                "eth_call",
                &format!(r#"{{"request":{CALL},"block_number":"pending"}}"#)
            ),
        );
        let got = post(batch).await;
        let by_id = |id: u64| {
            got.as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id)
                .unwrap()["result"]
                .clone()
        };
        assert_eq!(by_id(1)[1], "latest", "{got}");
        assert_eq!(by_id(2)[1], "pending", "{got}");
        assert_eq!(by_id(3)["block_number"], "latest", "{got}");

        handle.stop().unwrap();
    }
}
