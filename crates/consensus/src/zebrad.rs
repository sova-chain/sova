//! [`ZcashView`] implementation over a zebrad JSON-RPC endpoint.
//!
//! Uses the same RPC surface the burn-wallet e2e proved against Zebra
//! 6.3.0: `getblockcount`, `getblockhash`, `getblock <hash> 1` (txids +
//! `previousblockhash`), and `getrawtransaction <txid> 1` (decoded
//! `vout[].valueZat` + `vout[].scriptPubKey.hex`), plus SIP-7's pool and
//! shielded-flow fields from those same answers ([`crate::pools`]). Plain HTTP, blocking,
//! no TLS — the node is always local or on a trusted link; anything else
//! is out of scope for the follower.
//!
//! Error mapping: transport failures and malformed responses are
//! [`ViewError::Backend`]; a JSON-RPC *error object* from `getblockhash`
//! (out-of-range height, mid-reorg races) maps to `Ok(None)` per the
//! [`ZcashView`] contract.

use serde_json::{Value, json};

use crate::follower::{BlockView, TxOut, TxView, ViewError, ZcashView};

/// Blocking zebrad JSON-RPC client.
#[derive(Debug, Clone)]
pub struct ZebradClient {
    url: String,
    agent: ureq::Agent,
}

impl ZebradClient {
    /// Client for a zebrad RPC endpoint, e.g. `http://127.0.0.1:18232`.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            agent: ureq::Agent::new(),
        }
    }

    /// Raw JSON-RPC call returning the `result` value; a JSON-RPC error
    /// object returns `Ok(None)`.
    fn call(&self, method: &str, params: Value) -> Result<Option<Value>, ViewError> {
        let body = json!({"jsonrpc": "2.0", "id": "sova", "method": method, "params": params});
        let resp = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(|e| ViewError::Backend(format!("{method}: {e}")))?;
        let text = resp
            .into_string()
            .map_err(|e| ViewError::Backend(format!("{method}: bad body: {e}")))?;
        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| ViewError::Backend(format!("{method}: bad json: {e}")))?;
        if !parsed.get("error").is_none_or(serde_json::Value::is_null) {
            return Ok(None);
        }
        Ok(parsed.get("result").cloned())
    }

    /// Like [`Self::call`] but treats a JSON-RPC error as a backend
    /// failure — for methods where errors are never expected.
    fn call_required(&self, method: &str, params: Value) -> Result<Value, ViewError> {
        self.call(method, params)?
            .ok_or_else(|| ViewError::Backend(format!("{method}: rpc error or null result")))
    }
}

/// Decode a display-order hex hash into 32 bytes.
fn hash_from_hex(s: &str) -> Result<[u8; 32], ViewError> {
    let bytes = hex::decode(s).map_err(|e| ViewError::Backend(format!("bad hash hex: {e}")))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| ViewError::Backend("hash is not 32 bytes".to_string()))?;
    Ok(arr)
}

fn field<'a>(v: &'a Value, name: &str, ctx: &str) -> Result<&'a Value, ViewError> {
    v.get(name)
        .ok_or_else(|| ViewError::Backend(format!("{ctx}: missing {name}")))
}

impl ZcashView for ZebradClient {
    fn tip_height(&self) -> Result<u64, ViewError> {
        self.call_required("getblockcount", json!([]))?
            .as_u64()
            .ok_or_else(|| ViewError::Backend("getblockcount: not a u64".to_string()))
    }

    fn block_at(&self, height: u64) -> Result<Option<BlockView>, ViewError> {
        // Out-of-range height (or a mid-reorg race) is None, not an error.
        let Some(hash_val) = self.call("getblockhash", json!([height]))? else {
            return Ok(None);
        };
        let hash_hex = hash_val
            .as_str()
            .ok_or_else(|| ViewError::Backend("getblockhash: not a string".to_string()))?
            .to_owned();
        let hash = hash_from_hex(&hash_hex)?;

        let Some(block) = self.call("getblock", json!([hash_hex, 1]))? else {
            // The block vanished between the two calls: reorg race.
            return Ok(None);
        };
        let prev_hash = match block.get("previousblockhash").and_then(Value::as_str) {
            Some(p) => hash_from_hex(p)?,
            None => [0u8; 32], // genesis
        };
        let time = field(&block, "time", "getblock")?
            .as_u64()
            .and_then(|t| u32::try_from(t).ok())
            .ok_or_else(|| ViewError::Backend("getblock: time not a u32".to_string()))?;
        let txids = field(&block, "tx", "getblock")?
            .as_array()
            .ok_or_else(|| ViewError::Backend("getblock: tx not an array".to_string()))?
            .clone();

        let mut txs = Vec::with_capacity(txids.len());
        for txid_val in &txids {
            let txid_hex = txid_val
                .as_str()
                .ok_or_else(|| ViewError::Backend("getblock: txid not a string".to_string()))?;
            let tx = self.call_required("getrawtransaction", json!([txid_hex, 1]))?;
            let version = field(&tx, "version", "getrawtransaction")?
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| ViewError::Backend("tx: version not a u32".to_string()))?;
            let vout = field(&tx, "vout", "getrawtransaction")?
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut outputs = Vec::with_capacity(vout.len());
            for out in &vout {
                let value_zat = field(out, "valueZat", "vout")?
                    .as_u64()
                    .ok_or_else(|| ViewError::Backend("vout: valueZat not a u64".to_string()))?;
                let script_hex = field(out, "scriptPubKey", "vout")?
                    .get("hex")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ViewError::Backend("vout: scriptPubKey.hex missing".to_string())
                    })?;
                let script = hex::decode(script_hex)
                    .map_err(|e| ViewError::Backend(format!("vout: bad script hex: {e}")))?;
                outputs.push(TxOut { value_zat, script });
            }
            txs.push(TxView {
                txid: hash_from_hex(txid_hex)?,
                version,
                outputs,
                // Lenient here, strict in the SIP-7 cross-checks: a field
                // we can't read counts as absent, so a node without SIP-7
                // keeps following, and one with it holds on the mismatch
                // (`pools::check_pools`) rather than here.
                shielded: crate::pools::parse_tx_shielded(&tx).unwrap_or_default(),
            });
        }

        Ok(Some(BlockView {
            height,
            hash,
            prev_hash,
            time,
            txs,
            // Same leniency: unreadable pools are `None`, which SIP-7
            // treats as missing (hold) once active.
            pools: crate::pools::parse_block_pools(&block)
                .ok()
                .flatten()
                .map(Box::new),
        }))
    }
}
