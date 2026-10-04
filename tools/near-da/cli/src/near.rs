//! A minimal NEAR client: JSON-RPC over HTTP, ed25519 keys, and the one
//! transaction shape the poster sends (a single `FunctionCall` action).
//!
//! Hand-rolled on purpose (see `docs/design/near-da.md` §6): the poster
//! needs four RPC methods and one Borsh layout that NEAR keeps stable
//! (`Transaction` V0), and the official Rust clients pull NEAR's whole
//! primitives stack and an async runtime into what is otherwise a small
//! blocking tool. The layout is checked against a fixed vector below and,
//! end to end, by NEAR testnet accepting the signatures.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A NEAR JSON-RPC error, or a transport failure.
#[derive(Debug)]
pub enum RpcError {
    /// HTTP/transport failure or an unreadable body.
    Transport(String),
    /// The node answered with a JSON-RPC `error`.
    Node {
        /// `error.cause.name` (e.g. `UNKNOWN_TRANSACTION`, `TIMEOUT_ERROR`)
        /// or `error.name`.
        name: String,
        /// The whole error object, compact.
        detail: String,
    },
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "NEAR RPC transport: {e}"),
            Self::Node { name, detail } => write!(f, "NEAR RPC {name}: {detail}"),
        }
    }
}

impl std::error::Error for RpcError {}

impl RpcError {
    /// The error's name, if the node returned one.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Node { name, .. } => Some(name),
            Self::Transport(_) => None,
        }
    }
}

/// One NEAR JSON-RPC endpoint.
#[derive(Clone)]
pub struct Rpc {
    url: String,
    agent: ureq::Agent,
}

impl Rpc {
    /// A client for `url` with a per-request `timeout`.
    pub fn new(url: &str, timeout: Duration) -> Self {
        Self {
            url: url.to_owned(),
            agent: ureq::AgentBuilder::new().timeout(timeout).build(),
        }
    }

    /// The endpoint URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Call `method` with `params`; returns `result`.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body =
            json!({"jsonrpc": "2.0", "id": "sova-near-da", "method": method, "params": params});
        let resp = match self.agent.post(&self.url).send_json(body) {
            Ok(r) => r,
            // NEAR RPC answers some errors with HTTP 4xx/5xx and a JSON body.
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                match serde_json::from_str::<Value>(&text) {
                    Ok(v) if v.get("error").is_some() => return Err(node_error(&v["error"])),
                    _ => {
                        return Err(RpcError::Transport(format!(
                            "HTTP {code} from {}: {}",
                            self.url,
                            truncate(&text, 200)
                        )));
                    }
                }
            }
            Err(e) => return Err(RpcError::Transport(format!("{}: {e}", self.url))),
        };
        let v: Value = resp
            .into_json()
            .map_err(|e| RpcError::Transport(format!("{}: bad JSON: {e}", self.url)))?;
        if let Some(err) = v.get("error") {
            return Err(node_error(err));
        }
        // `query` reports some failures inside `result.error`.
        if let Some(err) = v["result"].get("error").and_then(Value::as_str) {
            return Err(RpcError::Node {
                name: "QUERY_ERROR".to_owned(),
                detail: err.to_owned(),
            });
        }
        Ok(v["result"].clone())
    }

    /// Call a view method with JSON `args`; parses its JSON return value.
    pub fn view(&self, contract: &str, method: &str, args: Value) -> Result<Value, RpcError> {
        let r = self.call(
            "query",
            json!({
                "request_type": "call_function",
                "finality": "final",
                "account_id": contract,
                "method_name": method,
                "args_base64": B64.encode(args.to_string()),
            }),
        )?;
        let bytes: Vec<u8> = r["result"]
            .as_array()
            .ok_or_else(|| RpcError::Transport("view: no result bytes".to_owned()))?
            .iter()
            .map(|b| b.as_u64().unwrap_or(0) as u8)
            .collect();
        serde_json::from_slice(&bytes)
            .map_err(|e| RpcError::Transport(format!("view {method}: bad JSON return: {e}")))
    }

    /// The access key's current nonce and the latest final block hash.
    pub fn access_key(&self, account: &str, public_key: &str) -> Result<(u64, [u8; 32]), RpcError> {
        let r = self.call(
            "query",
            json!({
                "request_type": "view_access_key",
                "finality": "final",
                "account_id": account,
                "public_key": public_key,
            }),
        )?;
        let nonce = r["nonce"]
            .as_u64()
            .ok_or_else(|| RpcError::Transport("view_access_key: no nonce".to_owned()))?;
        let hash = decode_hash(r["block_hash"].as_str().unwrap_or(""))
            .ok_or_else(|| RpcError::Transport("view_access_key: bad block_hash".to_owned()))?;
        Ok((nonce, hash))
    }

    /// Broadcast a signed transaction and wait until `wait_until`
    /// (`INCLUDED`, `EXECUTED_OPTIMISTIC`, `FINAL`, ...).
    pub fn send_tx(&self, signed: &[u8], wait_until: &str) -> Result<Value, RpcError> {
        self.call(
            "send_tx",
            json!({"signed_tx_base64": B64.encode(signed), "wait_until": wait_until}),
        )
    }

    /// A transaction's status and outcome by hash (base58) and sender.
    pub fn tx(&self, hash: &str, sender: &str, wait_until: &str) -> Result<Value, RpcError> {
        self.call(
            "tx",
            json!({"tx_hash": hash, "sender_account_id": sender, "wait_until": wait_until}),
        )
    }

    /// A block by height.
    pub fn block(&self, height: u64) -> Result<Value, RpcError> {
        self.call("block", json!({"block_id": height}))
    }

    /// A chunk by hash.
    pub fn chunk(&self, chunk_hash: &str) -> Result<Value, RpcError> {
        self.call("chunk", json!({"chunk_id": chunk_hash}))
    }
}

fn node_error(err: &Value) -> RpcError {
    let name = err["cause"]["name"]
        .as_str()
        .or_else(|| err["name"].as_str())
        .unwrap_or("ERROR")
        .to_owned();
    RpcError::Node {
        name,
        detail: truncate(&err.to_string(), 600),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_owned()
    } else {
        let mut end = n;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

/// Decode a base58 32-byte hash (block or transaction hash).
pub fn decode_hash(s: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    match bs58::decode(s).onto(&mut out[..]) {
        Ok(32) => Some(out),
        _ => None,
    }
}

/// An ed25519 NEAR key.
pub struct Key {
    signing: SigningKey,
}

impl Key {
    /// Parse `ed25519:<base58>` holding the 64-byte secret (seed ‖ public
    /// key), as NEAR CLIs write it, or a 32-byte seed.
    pub fn from_secret(s: &str) -> Result<Self, String> {
        let b58 = s
            .strip_prefix("ed25519:")
            .ok_or("private key must start with ed25519:")?;
        let bytes = bs58::decode(b58)
            .into_vec()
            .map_err(|_| "private key is not base58")?;
        let seed: [u8; 32] = match bytes.len() {
            64 | 32 => bytes[..32].try_into().map_err(|_| "bad key length")?,
            _ => return Err("private key must be 32 or 64 bytes".into()),
        };
        let signing = SigningKey::from_bytes(&seed);
        if bytes.len() == 64 && signing.verifying_key().as_bytes()[..] != bytes[32..] {
            return Err("private key's public half does not match its seed".into());
        }
        Ok(Self { signing })
    }

    /// Read a NEAR CLI key file (`{"private_key": "ed25519:...", ...}`).
    /// The secret is never logged.
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading key file {}: {e}", path.display()))?;
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| format!("key file {} is not JSON: {e}", path.display()))?;
        let secret = v["private_key"]
            .as_str()
            .or_else(|| v["secret_key"].as_str())
            .ok_or_else(|| format!("key file {} has no private_key", path.display()))?;
        Self::from_secret(secret)
    }

    /// Raw public key bytes.
    pub fn public_bytes(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// `ed25519:<base58>` public key.
    pub fn public_key(&self) -> String {
        format!(
            "ed25519:{}",
            bs58::encode(self.public_bytes()).into_string()
        )
    }

    fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing.sign(msg).to_bytes()
    }
}

/// A `FunctionCall` action.
pub struct FunctionCall<'a> {
    /// Method name.
    pub method: &'a str,
    /// Raw argument bytes.
    pub args: &'a [u8],
    /// Prepaid gas.
    pub gas: u64,
    /// Attached deposit, yoctoNEAR.
    pub deposit: u128,
}

fn borsh_str(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s);
}

/// Borsh of a NEAR `Transaction` (V0) with one `FunctionCall` action.
pub fn transaction_bytes(
    signer: &str,
    public_key: &[u8; 32],
    nonce: u64,
    receiver: &str,
    block_hash: &[u8; 32],
    call: &FunctionCall<'_>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(160 + call.args.len());
    borsh_str(&mut out, signer.as_bytes());
    out.push(0); // KeyType::ED25519
    out.extend_from_slice(public_key);
    out.extend_from_slice(&nonce.to_le_bytes());
    borsh_str(&mut out, receiver.as_bytes());
    out.extend_from_slice(block_hash);
    out.extend_from_slice(&1u32.to_le_bytes()); // one action
    out.push(2); // Action::FunctionCall
    borsh_str(&mut out, call.method.as_bytes());
    borsh_str(&mut out, call.args);
    out.extend_from_slice(&call.gas.to_le_bytes());
    out.extend_from_slice(&call.deposit.to_le_bytes());
    out
}

/// A signed transaction, ready to broadcast.
pub struct SignedTx {
    /// Borsh of the `SignedTransaction`.
    pub bytes: Vec<u8>,
    /// The transaction hash (sha256 of the unsigned transaction), base58.
    pub hash: String,
}

/// Sign a one-`FunctionCall` transaction.
pub fn sign_function_call(
    key: &Key,
    signer: &str,
    nonce: u64,
    receiver: &str,
    block_hash: &[u8; 32],
    call: &FunctionCall<'_>,
) -> SignedTx {
    let tx = transaction_bytes(
        signer,
        &key.public_bytes(),
        nonce,
        receiver,
        block_hash,
        call,
    );
    let digest: [u8; 32] = Sha256::digest(&tx).into();
    let sig = key.sign(&digest);
    let mut bytes = tx;
    bytes.push(0); // Signature::ED25519
    bytes.extend_from_slice(&sig);
    SignedTx {
        bytes,
        hash: bs58::encode(digest).into_string(),
    }
}

/// From a transaction result (`send_tx` / `tx`): `Ok(())` if it succeeded,
/// `Err(message)` with the failure otherwise, `None` if still pending.
pub fn outcome(result: &Value) -> Option<Result<(), String>> {
    let status = &result["status"];
    if status.get("SuccessValue").is_some() {
        return Some(Ok(()));
    }
    if let Some(f) = status.get("Failure") {
        return Some(Err(failure_message(f)));
    }
    None
}

fn failure_message(f: &Value) -> String {
    // Contract panics: ActionError.kind.FunctionCallError.ExecutionError.
    let exec = &f["ActionError"]["kind"]["FunctionCallError"]["ExecutionError"];
    if let Some(s) = exec.as_str() {
        return s.to_owned();
    }
    truncate(&f.to_string(), 400)
}

/// The `FunctionCall` action of a transaction result, if it is one call of
/// `method` on `receiver`: returns the raw argument bytes.
pub fn function_call_args(tx: &Value, receiver: &str, method: &str) -> Option<Vec<u8>> {
    if tx["receiver_id"].as_str() != Some(receiver) {
        return None;
    }
    let actions = tx["actions"].as_array()?;
    if actions.len() != 1 {
        return None;
    }
    let fc = &actions[0]["FunctionCall"];
    if fc["method_name"].as_str() != Some(method) {
        return None;
    }
    B64.decode(fc["args"].as_str()?).ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A fixed vector: the Borsh layout byte for byte, and its hash.
    #[test]
    fn transaction_layout_vector() {
        let key = Key::from_secret(&format!(
            "ed25519:{}",
            bs58::encode([7u8; 32]).into_string()
        ))
        .unwrap();
        let call = FunctionCall {
            method: "post",
            args: &[0xde, 0xad],
            gas: 300_000_000_000_000,
            deposit: 0,
        };
        let tx = transaction_bytes(
            "a.testnet",
            &key.public_bytes(),
            5,
            "b.testnet",
            &[9u8; 32],
            &call,
        );
        let mut want = Vec::new();
        want.extend_from_slice(&9u32.to_le_bytes());
        want.extend_from_slice(b"a.testnet");
        want.push(0);
        want.extend_from_slice(&key.public_bytes());
        want.extend_from_slice(&5u64.to_le_bytes());
        want.extend_from_slice(&9u32.to_le_bytes());
        want.extend_from_slice(b"b.testnet");
        want.extend_from_slice(&[9u8; 32]);
        want.extend_from_slice(&1u32.to_le_bytes());
        want.push(2);
        want.extend_from_slice(&4u32.to_le_bytes());
        want.extend_from_slice(b"post");
        want.extend_from_slice(&2u32.to_le_bytes());
        want.extend_from_slice(&[0xde, 0xad]);
        want.extend_from_slice(&300_000_000_000_000u64.to_le_bytes());
        want.extend_from_slice(&0u128.to_le_bytes());
        assert_eq!(tx, want);

        let signed = sign_function_call(&key, "a.testnet", 5, "b.testnet", &[9u8; 32], &call);
        assert_eq!(&signed.bytes[..tx.len()], &tx[..]);
        assert_eq!(signed.bytes.len(), tx.len() + 1 + 64);
        let digest: [u8; 32] = Sha256::digest(&tx).into();
        assert_eq!(signed.hash, bs58::encode(digest).into_string());
        // The signature verifies under the public key.
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&key.public_bytes()).unwrap();
        let sig =
            ed25519_dalek::Signature::from_bytes(&signed.bytes[tx.len() + 1..].try_into().unwrap());
        vk.verify_strict(&digest, &sig).unwrap();
    }

    #[test]
    fn key_parsing() {
        let seed = [3u8; 32];
        let sk = SigningKey::from_bytes(&seed);
        let mut full = seed.to_vec();
        full.extend_from_slice(sk.verifying_key().as_bytes());
        let k =
            Key::from_secret(&format!("ed25519:{}", bs58::encode(&full).into_string())).unwrap();
        assert_eq!(k.public_bytes(), sk.verifying_key().to_bytes());
        assert!(k.public_key().starts_with("ed25519:"));
        full[40] ^= 1;
        assert!(
            Key::from_secret(&format!("ed25519:{}", bs58::encode(&full).into_string())).is_err()
        );
        assert!(Key::from_secret("secp256k1:abc").is_err());
    }

    #[test]
    fn outcome_and_args() {
        assert_eq!(
            outcome(&json!({"status": {"SuccessValue": ""}})),
            Some(Ok(()))
        );
        let fail = json!({"status": {"Failure": {"ActionError": {"index": 0, "kind": {"FunctionCallError": {"ExecutionError": "Smart contract panicked: batch starts at 0, expected 3"}}}}}});
        assert_eq!(
            outcome(&fail),
            Some(Err(
                "Smart contract panicked: batch starts at 0, expected 3".to_owned()
            ))
        );
        assert_eq!(outcome(&json!({"status": "Started"})), None);
        let tx = json!({"receiver_id": "c.testnet", "actions": [{"FunctionCall": {"method_name": "post", "args": B64.encode([1u8, 2, 3]), "gas": 1, "deposit": "0"}}]});
        assert_eq!(
            function_call_args(&tx, "c.testnet", "post"),
            Some(vec![1, 2, 3])
        );
        assert_eq!(function_call_args(&tx, "d.testnet", "post"), None);
        assert_eq!(function_call_args(&tx, "c.testnet", "set_tx"), None);
    }
}
