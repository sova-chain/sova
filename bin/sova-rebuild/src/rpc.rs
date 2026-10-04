//! JSON-RPC to a node: its authrpc (JWT-signed, through crates/engine's
//! relay client, the code path gossip v1 pushes peer blocks with) or a
//! plain HTTP(S) RPC (export, `--expect-rpc`).
//!
//! Each [`Endpoint`] owns one `ureq::Agent`, so its calls reuse a single
//! keep-alive connection. A rebuild makes several calls per block (the
//! payload, then polls until the node adopts it); with a fresh connection
//! per call, a resume over tens of thousands of blocks left that many
//! sockets in TIME_WAIT and ran macOS out of ephemeral ports
//! (`Can't assign requested address (os error 49)`). `https://` URLs work
//! too (ureq's `tls` feature: rustls + webpki roots, already in the node's
//! dependency graph), e.g. `--expect-rpc https://rpc-testnet.sova.io`.

use std::time::Duration;

use alloy_primitives::B256;
use reth_rpc_layer::JwtSecret;
use serde_json::{Value, json};

/// An RPC failure.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The server answered with a JSON-RPC error object.
    #[error("{method}: rpc error {code}: {message}")]
    Rpc {
        /// The method called.
        method: String,
        /// JSON-RPC error code (0 when the authrpc client didn't keep it).
        code: i64,
        /// The error message.
        message: String,
    },
    /// Connection, HTTP or body trouble.
    #[error("{0}")]
    Transport(String),
}

impl RpcError {
    /// Whether the method doesn't exist on this server.
    pub fn is_method_not_found(&self) -> bool {
        matches!(self, Self::Rpc { code: -32601, .. })
            || matches!(self, Self::Rpc { message, .. } if message.contains("not found") && message.contains("ethod"))
    }
}

/// A node endpoint: where to call, and the connection pool to call it
/// through. Clones share the pool.
#[derive(Debug, Clone)]
pub struct Endpoint {
    target: Target,
    agent: ureq::Agent,
}

#[derive(Debug, Clone)]
enum Target {
    /// Plain HTTP(S) JSON-RPC.
    Http(String),
    /// The Engine API authrpc, every request signed with a fresh JWT.
    Auth { url: String, jwt: JwtSecret },
}

impl Endpoint {
    /// A plain HTTP(S) JSON-RPC endpoint.
    pub fn http(url: impl Into<String>) -> Self {
        Self {
            target: Target::Http(url.into()),
            agent: agent(),
        }
    }

    /// A node's Engine API authrpc, every request signed with a fresh JWT.
    pub fn auth(url: impl Into<String>, jwt: JwtSecret) -> Self {
        Self {
            target: Target::Auth {
                url: url.into(),
                jwt,
            },
            agent: agent(),
        }
    }

    /// Calls `method` with a timeout. A transport failure other than a
    /// timeout is retried once on a fresh connection: a pooled keep-alive
    /// connection the server has meanwhile closed fails the next POST
    /// (ureq retries only idempotent methods by itself). Every call this
    /// tool makes is safe to repeat (reads, and `engine_newPayloadV4`,
    /// which a node answers the same way for a block it already has).
    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        match self.call_once(method, params.clone(), timeout) {
            Err(RpcError::Transport(m)) if !m.contains("timed out") => {
                self.call_once(method, params, timeout)
            }
            other => other,
        }
    }

    fn call_once(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        match &self.target {
            Target::Auth { url, jwt } => engine::relay::call_authrpc_with_agent(
                &self.agent,
                url,
                jwt,
                method,
                params,
                timeout,
            )
            .map_err(|e| match e {
                engine::relay::RelayError::Rpc(m) => RpcError::Rpc {
                    method: method.to_owned(),
                    code: extract_code(&m),
                    message: m,
                },
                other => RpcError::Transport(other.to_string()),
            }),
            Target::Http(url) => http_call(&self.agent, url, method, params, timeout),
        }
    }

    /// `eth_chainId`.
    pub fn chain_id(&self) -> Result<u64, RpcError> {
        let v = self.call("eth_chainId", json!([]), SHORT)?;
        quantity(&v).ok_or_else(|| RpcError::Transport(format!("eth_chainId: bad result {v}")))
    }

    /// `eth_blockNumber`.
    pub fn block_number(&self) -> Result<u64, RpcError> {
        let v = self.call("eth_blockNumber", json!([]), SHORT)?;
        quantity(&v).ok_or_else(|| RpcError::Transport(format!("eth_blockNumber: bad result {v}")))
    }

    /// `eth_getBlockByNumber(tag, false)` reduced to what we compare:
    /// `None` when the node has no such block.
    pub fn head_at(&self, tag: &str) -> Result<Option<BlockRef>, RpcError> {
        let v = self.call("eth_getBlockByNumber", json!([tag, false]), SHORT)?;
        if v.is_null() {
            return Ok(None);
        }
        BlockRef::from_json(&v)
            .map(Some)
            .ok_or_else(|| RpcError::Transport(format!("eth_getBlockByNumber({tag}): bad block")))
    }

    /// The block at `height`, if any.
    pub fn block_at(&self, height: u64) -> Result<Option<BlockRef>, RpcError> {
        self.head_at(&format!("0x{height:x}"))
    }
}

/// One agent per endpoint. The tool calls each endpoint from one thread,
/// one request at a time, so one idle connection per host is all the pool
/// ever holds.
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .max_idle_connections_per_host(1)
        .build()
}

/// Default timeout for small reads.
pub const SHORT: Duration = Duration::from_secs(10);

/// A block reduced to its identity and state commitment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRef {
    /// Height.
    pub number: u64,
    /// Block hash.
    pub hash: B256,
    /// State root.
    pub state_root: B256,
}

impl BlockRef {
    /// From an `eth_getBlockByNumber` result.
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            number: quantity(v.get("number")?)?,
            hash: v.get("hash")?.as_str()?.parse().ok()?,
            state_root: v.get("stateRoot")?.as_str()?.parse().ok()?,
        })
    }
}

impl std::fmt::Display for BlockRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "#{} hash {} stateRoot {}",
            self.number, self.hash, self.state_root
        )
    }
}

/// A `0x…` quantity.
pub fn quantity(v: &Value) -> Option<u64> {
    let s = v.as_str()?;
    u64::from_str_radix(s.strip_prefix("0x")?, 16).ok()
}

/// `0x…` hex bytes.
pub fn hex_bytes(v: &Value) -> Option<Vec<u8>> {
    hex::decode(v.as_str()?.strip_prefix("0x")?).ok()
}

/// The authrpc client folds the error object into a string
/// (`method: {"code":-32601,...}`); recover the code when it is there.
fn extract_code(m: &str) -> i64 {
    m.find('{')
        .and_then(|i| serde_json::from_str::<Value>(&m[i..]).ok())
        .and_then(|v| v.get("code").and_then(Value::as_i64))
        .unwrap_or(0)
}

fn http_call(
    agent: &ureq::Agent,
    url: &str,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, RpcError> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let text = match agent
        .post(url)
        .set("Content-Type", "application/json")
        .timeout(timeout)
        .send_string(&body.to_string())
    {
        Ok(resp) => resp
            .into_string()
            .map_err(|e| RpcError::Transport(format!("{method}: bad response body: {e}")))?,
        // Some servers answer a JSON-RPC error with an HTTP error status.
        Err(ureq::Error::Status(code, resp)) => resp
            .into_string()
            .map_err(|e| RpcError::Transport(format!("{method}: HTTP {code}: {e}")))?,
        Err(e) => return Err(RpcError::Transport(format!("{method}: {e}"))),
    };
    let parsed: Value = serde_json::from_str(&text).map_err(|e| {
        RpcError::Transport(format!("{method}: bad response json: {e}: {text:.200}"))
    })?;
    if let Some(err) = parsed.get("error").filter(|v| !v.is_null()) {
        return Err(RpcError::Rpc {
            method: method.to_owned(),
            code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_quantities() {
        assert_eq!(
            extract_code(r#"debug_getRawBlock: {"code":-32601,"message":"Method not found"}"#),
            -32601
        );
        assert_eq!(extract_code("x: transport"), 0);
        assert_eq!(quantity(&json!("0x1f")), Some(31));
        assert_eq!(quantity(&json!("1f")), None);
        let e = RpcError::Rpc {
            method: "m".into(),
            code: -32601,
            message: String::new(),
        };
        assert!(e.is_method_not_found());
    }

    /// Many calls, one TCP connection: the endpoint's agent keeps it alive.
    #[test]
    fn calls_reuse_one_connection() {
        let srv = test_server::FakeNode::start(9, 30, test_server::Mode::KeepAlive);
        let ep = Endpoint::http(srv.url());
        for _ in 0..200 {
            assert_eq!(ep.block_number().unwrap(), 30);
        }
        assert_eq!(ep.block_at(7).unwrap().unwrap().number, 7);
        assert_eq!(
            srv.connections(),
            1,
            "one keep-alive connection for 201 calls"
        );
    }

    /// A server that drops each connection after one answer (as a server
    /// closing an idle keep-alive connection does): the next call finds
    /// its pooled connection dead and is retried on a fresh one.
    #[test]
    fn a_closed_pooled_connection_is_retried() {
        let srv = test_server::FakeNode::start(9, 30, test_server::Mode::CloseAfterEach);
        let ep = Endpoint::http(srv.url());
        for _ in 0..20 {
            assert_eq!(ep.chain_id().unwrap(), 9);
        }
        assert!(srv.connections() >= 20);
    }

    /// `--expect-rpc https://…`: TLS works (it failed with "Unknown
    /// Scheme" before ureq's `tls` feature). Network; run with
    /// `cargo test -p sova-rebuild -- --ignored`. Block 66,239 is the head
    /// the 2026-10-04 rebuild from NEAR matched (docs/design/near-da.md §9.1).
    #[test]
    #[ignore = "network: reads the public testnet RPC"]
    fn https_public_rpc() {
        let url = std::env::var("SOVA_REBUILD_HTTPS_RPC")
            .unwrap_or_else(|_| "https://rpc-testnet.sova.io".to_owned());
        let ep = Endpoint::http(url);
        assert_eq!(ep.chain_id().unwrap(), 82330);
        let b = ep.block_at(66_239).unwrap().unwrap();
        assert!(b.hash.to_string().starts_with("0x79166f6b"), "{b}");
        assert!(b.state_root.to_string().starts_with("0x2e76ff01"), "{b}");
        // Same connection for a second read.
        assert!(ep.block_number().unwrap() >= 66_239);
    }
}

/// A minimal JSON-RPC node over HTTP/1.1 for tests: `eth_chainId`,
/// `eth_blockNumber` and `eth_getBlockByNumber` over a fixed chain whose
/// block `n` has hash `hash_of(n)` (or a caller-given override), counting
/// accepted connections and block lookups.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod test_server {
    use std::{
        collections::HashMap,
        io::{BufRead, BufReader, Read, Write},
        net::{TcpListener, TcpStream},
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    use alloy_primitives::B256;
    use serde_json::{Value, json};

    /// How the server treats a connection after answering.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Mode {
        /// Keep it open for the next request.
        KeepAlive,
        /// Close it (no `Connection: close` header).
        CloseAfterEach,
    }

    pub(crate) struct FakeNode {
        addr: std::net::SocketAddr,
        conns: Arc<AtomicU64>,
        lookups: Arc<AtomicU64>,
    }

    #[derive(Clone)]
    struct Chain {
        chain_id: u64,
        head: u64,
        hashes: HashMap<u64, B256>,
        roots: HashMap<u64, B256>,
    }

    /// The default hash of block `n`.
    pub(crate) fn hash_of(n: u64) -> B256 {
        B256::left_padding_from(&n.to_be_bytes())
    }

    impl FakeNode {
        pub(crate) fn start(chain_id: u64, head: u64, mode: Mode) -> Self {
            Self::with_blocks(chain_id, head, mode, &[])
        }

        /// `blocks`: (height, hash, stateRoot) overriding the defaults.
        pub(crate) fn with_blocks(
            chain_id: u64,
            head: u64,
            mode: Mode,
            blocks: &[(u64, B256, B256)],
        ) -> Self {
            let mut chain = Chain {
                chain_id,
                head,
                hashes: (0..=head).map(|n| (n, hash_of(n))).collect(),
                roots: HashMap::new(),
            };
            for (n, h, r) in blocks {
                chain.hashes.insert(*n, *h);
                chain.roots.insert(*n, *r);
            }
            let chain = Arc::new(Mutex::new(chain));
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let conns = Arc::new(AtomicU64::new(0));
            let lookups = Arc::new(AtomicU64::new(0));
            let (c, l) = (conns.clone(), lookups.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { return };
                    c.fetch_add(1, Ordering::SeqCst);
                    let (chain, l) = (chain.clone(), l.clone());
                    std::thread::spawn(move || serve(stream, &chain, &l, mode));
                }
            });
            Self {
                addr,
                conns,
                lookups,
            }
        }

        pub(crate) fn url(&self) -> String {
            format!("http://{}", self.addr)
        }

        pub(crate) fn connections(&self) -> u64 {
            self.conns.load(Ordering::SeqCst)
        }

        /// `eth_getBlockByNumber` calls answered.
        pub(crate) fn lookups(&self) -> u64 {
            self.lookups.load(Ordering::SeqCst)
        }
    }

    fn serve(stream: TcpStream, chain: &Mutex<Chain>, lookups: &AtomicU64, mode: Mode) {
        let mut r = BufReader::new(stream.try_clone().unwrap());
        let mut w = stream;
        loop {
            let mut len = 0usize;
            let mut line = String::new();
            // Request line, then headers up to the blank line.
            loop {
                line.clear();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let l = line.trim_end();
                if l.is_empty() {
                    break;
                }
                if let Some((k, v)) = l.split_once(':')
                    && k.eq_ignore_ascii_case("content-length")
                {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).unwrap();
            let req: Value = serde_json::from_slice(&body).unwrap();
            let result = answer(&req, &chain.lock().unwrap(), lookups);
            let out = json!({"jsonrpc": "2.0", "id": req["id"], "result": result}).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{out}",
                out.len()
            );
            if w.write_all(resp.as_bytes()).is_err() {
                return;
            }
            if mode == Mode::CloseAfterEach {
                return;
            }
        }
    }

    fn answer(req: &Value, c: &Chain, lookups: &AtomicU64) -> Value {
        match req["method"].as_str().unwrap() {
            "eth_chainId" => json!(format!("0x{:x}", c.chain_id)),
            "eth_blockNumber" => json!(format!("0x{:x}", c.head)),
            "eth_getBlockByNumber" => {
                lookups.fetch_add(1, Ordering::SeqCst);
                let tag = req["params"][0].as_str().unwrap();
                let n = if tag == "latest" {
                    c.head
                } else {
                    u64::from_str_radix(tag.trim_start_matches("0x"), 16).unwrap()
                };
                match c.hashes.get(&n) {
                    Some(h) if n <= c.head => json!({
                        "number": format!("0x{n:x}"),
                        "hash": h.to_string(),
                        "stateRoot": c.roots.get(&n).copied().unwrap_or_default().to_string(),
                    }),
                    _ => Value::Null,
                }
            }
            m => panic!("fake node: unexpected method {m}"),
        }
    }
}
