//! A minimal JSON-RPC client for a `zebrad`-compatible node.
//!
//! This is intentionally small: just enough of the Zcash/`zcashd`-style
//! JSON-RPC surface to drive `box/regtest` for the end-to-end burn test
//! (mining, submitting, and fetching transactions). It is not a general
//! Zcash RPC client.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Errors from calling the RPC endpoint.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The HTTP transport itself failed (connection refused, timeout,
    /// TLS error, etc).
    #[error("RPC transport error calling {method}: {source}")]
    Transport {
        /// The RPC method being called.
        method: String,
        /// The underlying transport error.
        #[source]
        source: Box<ureq::Error>,
    },
    /// The response body was not valid JSON, or not the JSON-RPC envelope
    /// shape expected.
    #[error("malformed RPC response for {method}: {source}")]
    MalformedResponse {
        /// The RPC method being called.
        method: String,
        /// The underlying (de)serialization error.
        #[source]
        source: serde_json::Error,
    },
    /// The node returned a JSON-RPC error object. `message` is the
    /// **exact, verbatim** text the node returned -- callers that need to
    /// inspect a rejection reason (e.g. standardness-policy failures on
    /// `sendrawtransaction`) should match on this.
    #[error("RPC error {code} calling {method}: {message}")]
    RpcFailure {
        /// The RPC method being called.
        method: String,
        /// The JSON-RPC error code.
        code: i64,
        /// The JSON-RPC error message, verbatim.
        message: String,
    },
    /// The response had no `result` and no `error` field.
    #[error("RPC response for {method} had neither `result` nor `error`")]
    EmptyResponse {
        /// The RPC method being called.
        method: String,
    },
}

/// A JSON-RPC client bound to one `zebrad`-compatible endpoint.
pub struct RpcClient {
    agent: ureq::Agent,
    url: String,
    /// `Authorization` header value (HTTP Basic), if the node requires
    /// auth. Never printed: [`RpcClient`] deliberately has no `Debug`.
    /// Behind a lock so a rotated cookie can be picked up through `&self`
    /// (see [`Self::with_cookie_file`]).
    authorization: Mutex<Option<String>>,
    /// zebrad's cookie file, when auth came from one: re-read if a call
    /// fails at the transport level, since zebrad writes a new cookie
    /// every time it starts.
    cookie_file: Option<PathBuf>,
}

/// One entry of `getaddressutxos`: an unspent transparent output in the
/// node's best chain.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct AddressUtxo {
    /// The address the output pays.
    pub address: String,
    /// The funding transaction's txid (RPC display order).
    pub txid: String,
    /// The output index within that transaction.
    #[serde(rename = "outputIndex")]
    pub output_index: u32,
    /// The output's value, in zatoshis.
    pub satoshis: u64,
    /// The height of the block that mined the funding transaction.
    pub height: u64,
}

/// The HTTP Basic `Authorization` header value for `user:password`
/// credentials (surrounding whitespace, e.g. a trailing newline in a
/// cookie file, is ignored).
fn basic_auth_header(credentials: &str) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(credentials.trim());
    format!("Basic {encoded}")
}

/// `getaddressutxos` with `chainInfo: true`: the UTXO list and the tip it
/// was read at (zebrad's `GetAddressUtxosResponseObject`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct AddressUtxosAtTip {
    /// Every unspent transparent output paying the requested address(es),
    /// in chain order.
    pub utxos: Vec<AddressUtxo>,
    /// The tip block hash the list was read at (RPC display order).
    pub hash: String,
    /// The tip height the list was read at.
    pub height: u64,
}

impl RpcClient {
    /// Constructs a client for the given RPC endpoint, e.g.
    /// `http://127.0.0.1:18232`.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        // `http_status_as_error(false)`: zebrad's JSON-RPC error responses
        // (e.g. a rejected `sendrawtransaction`) may or may not carry a
        // non-2xx HTTP status. We want the JSON-RPC `error` object either
        // way, so we disable ureq's default of turning 4xx/5xx into a
        // transport-level `Err` before we ever get to look at the body.
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build();
        Self {
            agent: config.into(),
            url: url.into(),
            authorization: Mutex::new(None),
            cookie_file: None,
        }
    }

    /// Like [`Self::new`], but with a global per-request timeout, so a hung
    /// node can't hang the caller forever (long-running services).
    #[must_use]
    pub fn with_timeout(url: impl Into<String>, timeout: std::time::Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .build();
        Self {
            agent: config.into(),
            url: url.into(),
            authorization: Mutex::new(None),
            cookie_file: None,
        }
    }

    /// Sends HTTP Basic auth with every call. `credentials` is `user:password`
    /// -- exactly the contents of zebrad's cookie file (`__cookie__:<token>`),
    /// see [`Self::with_cookie_file`].
    #[must_use]
    pub fn with_basic_auth(self, credentials: &str) -> Self {
        *self.auth() = Some(basic_auth_header(credentials));
        self
    }

    /// Authenticates with zebrad's RPC cookie (`enable_cookie_auth = true`,
    /// the zebrad default): the file at `path` holds `__cookie__:<token>`.
    ///
    /// zebrad writes a fresh cookie each time it starts, so after a call
    /// fails at the transport level (zebrad answers bad credentials with an
    /// empty response, not a JSON-RPC error) the file is read again, and if
    /// the cookie changed the call is retried once with it. A long-running
    /// client thus survives a zebrad restart.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the cookie file can't be read.
    pub fn with_cookie_file(mut self, path: &Path) -> std::io::Result<Self> {
        let credentials = std::fs::read_to_string(path)?;
        self.cookie_file = Some(path.to_path_buf());
        Ok(self.with_basic_auth(&credentials))
    }

    fn auth(&self) -> MutexGuard<'_, Option<String>> {
        // A poisoned lock only means another thread panicked mid-update of
        // a plain `Option<String>`; the value is still usable.
        self.authorization
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Re-reads the cookie file; returns whether the credentials changed.
    fn reload_cookie(&self) -> bool {
        let Some(path) = &self.cookie_file else {
            return false;
        };
        let Ok(credentials) = std::fs::read_to_string(path) else {
            return false;
        };
        let header = basic_auth_header(&credentials);
        let mut auth = self.auth();
        if auth.as_deref() == Some(header.as_str()) {
            return false;
        }
        *auth = Some(header);
        true
    }

    /// Calls `method` with `params`, returning the raw `result` value.
    fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": "burn-wallet",
            "method": method,
            "params": params,
        });
        match self.call_once(method, &body) {
            Err(RpcError::Transport { .. }) if self.reload_cookie() => {
                self.call_once(method, &body)
            }
            other => other,
        }
    }

    /// One HTTP round trip for `body`.
    fn call_once(&self, method: &str, body: &Value) -> Result<Value, RpcError> {
        let mut request = self.agent.post(&self.url);
        let authorization = self.auth().clone();
        if let Some(authorization) = &authorization {
            request = request.header("Authorization", authorization);
        }
        let mut response = request.send_json(body).map_err(|e| RpcError::Transport {
            method: method.to_string(),
            source: Box::new(e),
        })?;

        let envelope: Value = response
            .body_mut()
            .read_json()
            .map_err(|e| RpcError::Transport {
                method: method.to_string(),
                source: Box::new(e),
            })?;

        if let Some(error) = envelope.get("error")
            && !error.is_null()
        {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| error.to_string(), ToString::to_string);
            return Err(RpcError::RpcFailure {
                method: method.to_string(),
                code,
                message,
            });
        }

        envelope
            .get("result")
            .cloned()
            .ok_or_else(|| RpcError::EmptyResponse {
                method: method.to_string(),
            })
    }

    /// Calls `method` with `params` and deserializes `result` as `T`.
    fn call_typed<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, RpcError> {
        let result = self.call(method, params)?;
        serde_json::from_value(result).map_err(|e| RpcError::MalformedResponse {
            method: method.to_string(),
            source: e,
        })
    }

    /// `getblockchaininfo`, raw. zebrad reports `chain` as `"main"` for
    /// Mainnet and `"test"` for both Testnet and Regtest (BIP70 names).
    pub fn get_blockchain_info(&self) -> Result<Value, RpcError> {
        self.call_typed("getblockchaininfo", json!([]))
    }

    /// `getaddressutxos {"addresses": [address]}`: every unspent transparent
    /// output paying `address` in the node's best chain (confirmed only --
    /// mempool transactions are not reflected, neither their new outputs nor
    /// the outputs they spend). Coinbase outputs are included whatever their
    /// maturity.
    pub fn get_address_utxos(&self, address: &str) -> Result<Vec<AddressUtxo>, RpcError> {
        self.call_typed("getaddressutxos", json!([{ "addresses": [address] }]))
    }

    /// `getaddressutxos {"addresses": [address], "chainInfo": true}`: like
    /// [`Self::get_address_utxos`], plus the height and hash of the tip the
    /// answer was read at. zebrad takes both from one state snapshot, so
    /// confirmation counts (e.g. coinbase maturity) computed against
    /// [`AddressUtxosAtTip::height`] are consistent with the UTXO list,
    /// where a separate `getblockcount` could race a new block.
    pub fn get_address_utxos_at_tip(&self, address: &str) -> Result<AddressUtxosAtTip, RpcError> {
        self.call_typed(
            "getaddressutxos",
            json!([{ "addresses": [address], "chainInfo": true }]),
        )
    }

    /// `getblockcount`: the height of the current tip.
    pub fn get_block_count(&self) -> Result<u64, RpcError> {
        self.call_typed("getblockcount", json!([]))
    }

    /// `getblockhash <height>`.
    pub fn get_block_hash(&self, height: u64) -> Result<String, RpcError> {
        self.call_typed("getblockhash", json!([height]))
    }

    /// `getblock <hash> 2`: the full verbose block, with fully decoded
    /// transactions (including `vout[].valueZat` and
    /// `vout[].scriptPubKey.addresses`).
    pub fn get_block_verbose(&self, hash: &str) -> Result<Value, RpcError> {
        self.call_typed("getblock", json!([hash, 2]))
    }

    /// `generate <n>`: mines `n` blocks immediately (regtest-only; Zebra
    /// rejects this on any network where PoW is not disabled). Returns the
    /// mined block hashes.
    pub fn generate(&self, n: u32) -> Result<Vec<String>, RpcError> {
        self.call_typed("generate", json!([n]))
    }

    /// `generatetoaddress <n> <address>`: mines `n` blocks, with coinbase
    /// rewards paid to `address` (regtest-only).
    pub fn generate_to_address(&self, n: u32, address: &str) -> Result<Vec<String>, RpcError> {
        self.call_typed("generatetoaddress", json!([n, address]))
    }

    /// `sendrawtransaction <hex>`: broadcasts a signed transaction.
    ///
    /// On rejection (e.g. a standardness-policy failure), the returned
    /// [`RpcError::RpcFailure`] carries the node's exact error message.
    pub fn send_raw_transaction(&self, tx_hex: &str) -> Result<String, RpcError> {
        self.call_typed("sendrawtransaction", json!([tx_hex]))
    }

    /// `getrawtransaction <txid> 1`: the verbose (decoded) transaction, if
    /// known to the node (mempool or best chain).
    pub fn get_raw_transaction_verbose(&self, txid: &str) -> Result<Value, RpcError> {
        self.call_typed("getrawtransaction", json!([txid, 1]))
    }
}

#[cfg(test)]
// Test code: an unexpected `Err` here is a test failure.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The exact shapes zebrad 6.3 returns (`GetAddressUtxosResponse` in
    /// zebra-rpc): a bare list without `chainInfo`, an object with it. The
    /// empty object is verbatim from the public-testnet node.
    #[test]
    fn deserializes_zebrad_getaddressutxos_shapes() {
        let bare: Vec<AddressUtxo> = serde_json::from_value(json!([{
            "address": "tmTjmjkkJibwuLyGTYj3vYa1S6RjuB5BD8D",
            "txid": "ab".repeat(32),
            "outputIndex": 2,
            "script": "76a914000000000000000000000000000000000000000088ac",
            "satoshis": 625_000_000u64,
            "height": 3_256_800u64,
        }]))
        .unwrap();
        assert_eq!(bare[0].output_index, 2);
        assert_eq!(bare[0].satoshis, 625_000_000);

        let empty: AddressUtxosAtTip = serde_json::from_value(json!({
            "utxos": [],
            "hash": "0094ef4d2236b9d7456a128b1f5ef6014eabfee3f33fd32cfa65fef3f7c7707a",
            "height": 3_256_842u64,
        }))
        .unwrap();
        assert!(empty.utxos.is_empty());
        assert_eq!(empty.height, 3_256_842);
    }

    /// A one-endpoint HTTP server standing in for zebrad with cookie auth:
    /// answers `getblockcount` with 42 when the `Authorization` header
    /// carries `expected` credentials, and with zebrad's empty response
    /// otherwise. Serves `requests` connections, then stops.
    fn cookie_node(expected: &str, requests: usize) -> String {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let want = basic_auth_header(expected);
        std::thread::spawn(move || {
            for stream in listener.incoming().take(requests) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut auth, mut len) = (String::new(), 0usize);
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if lower.starts_with("authorization:") {
                        auth = line["authorization:".len()..].trim().to_string();
                    } else if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; len];
                reader.read_exact(&mut body).unwrap();
                let reply = if auth == want {
                    let json = r#"{"jsonrpc":"2.0","id":"burn-wallet","result":42}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        json.len()
                    )
                } else {
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        url
    }

    /// zebrad writes a new cookie on every start: a client built from the
    /// old cookie re-reads the file after the auth failure and carries on.
    #[test]
    fn rotated_cookie_is_reread_and_the_call_retried() {
        let dir = tempfile::tempdir().unwrap();
        let cookie = dir.path().join(".cookie");
        std::fs::write(&cookie, "__cookie__:before-restart").unwrap();
        let url = cookie_node("__cookie__:after-restart", 2);
        let rpc = RpcClient::new(url).with_cookie_file(&cookie).unwrap();

        std::fs::write(&cookie, "__cookie__:after-restart\n").unwrap();
        assert_eq!(rpc.get_block_count().unwrap(), 42);
    }

    #[test]
    fn wrong_cookie_that_did_not_change_is_an_error_not_a_loop() {
        let dir = tempfile::tempdir().unwrap();
        let cookie = dir.path().join(".cookie");
        std::fs::write(&cookie, "__cookie__:stale").unwrap();
        let url = cookie_node("__cookie__:right", 1);
        let rpc = RpcClient::new(url).with_cookie_file(&cookie).unwrap();
        assert!(matches!(
            rpc.get_block_count(),
            Err(RpcError::Transport { .. })
        ));
    }

    #[test]
    fn matching_cookie_authenticates() {
        let dir = tempfile::tempdir().unwrap();
        let cookie = dir.path().join(".cookie");
        std::fs::write(&cookie, "__cookie__:right\n").unwrap();
        let url = cookie_node("__cookie__:right", 1);
        let rpc = RpcClient::new(url).with_cookie_file(&cookie).unwrap();
        assert_eq!(rpc.get_block_count().unwrap(), 42);
    }
}
