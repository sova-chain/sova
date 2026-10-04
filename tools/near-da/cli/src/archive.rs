//! Reading the archive from NEAR: the index contract's views, and the
//! batch bytes from transaction history.
//!
//! Finding a batch's bytes, in order:
//! 1. its `tx_hash` from the index, looked up with `tx` on each NEAR RPC
//!    given (a regular node knows only the last ~1.5 days; an archival node
//!    knows everything it has kept);
//! 2. without a `tx_hash` (or if every lookup fails), the NEAR blocks just
//!    before and at the index's `near_block`, scanned chunk by chunk on the
//!    same RPCs, then on neardata.xyz-style block APIs.
//!
//! Either way the bytes must hash to the index's `sha256`, so a source can
//! withhold a batch but never substitute one.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::near::{self, Rpc};

/// The contract's `info` view.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Info {
    /// Batch format (`SOVADA1`).
    pub format: String,
    /// Posting account.
    pub owner: String,
    /// Sova chain id.
    pub chain_id: u64,
    /// First archived height.
    pub start_height: u64,
    /// Next height to post.
    pub next_height: u64,
    /// Batches posted.
    pub batch_count: u64,
    /// Last archived block hash, 0x-hex.
    pub last_hash: Option<String>,
    /// Bytes posted.
    pub bytes_posted: u64,
}

impl Info {
    /// The last archived block's hash, decoded.
    pub fn last_hash_bytes(&self) -> Result<Option<[u8; 32]>, String> {
        self.last_hash.as_deref().map(parse_0x_hash).transpose()
    }
}

/// One entry of the contract's `batches` view.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Entry {
    /// Position in the index.
    pub index: u64,
    /// First height.
    pub first_height: u64,
    /// Last height.
    pub last_height: u64,
    /// Blocks.
    pub count: u32,
    /// Batch size.
    pub bytes: u32,
    /// sha256 of the batch, hex.
    pub sha256: String,
    /// Last block hash, 0x-hex.
    pub last_hash: String,
    /// NEAR block the post executed in.
    pub near_block: u64,
    /// Carrying transaction, base58.
    pub tx_hash: Option<String>,
}

/// Parse `0x`-prefixed (or bare) 32-byte hex.
pub fn parse_0x_hash(s: &str) -> Result<[u8; 32], String> {
    let b = hex::decode(s.trim_start_matches("0x")).map_err(|e| format!("bad hash {s}: {e}"))?;
    b.try_into()
        .map_err(|_| format!("hash {s} is not 32 bytes"))
}

/// sha256, hex.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// The contract's `info`.
pub fn info(rpc: &Rpc, contract: &str) -> Result<Info, String> {
    let v = rpc
        .view(contract, "info", json!({}))
        .map_err(|e| e.to_string())?;
    serde_json::from_value(v).map_err(|e| format!("info: unexpected shape: {e}"))
}

/// Every batch from `from_index` on, paging through the `batches` view.
pub fn list(rpc: &Rpc, contract: &str, from_index: u64) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    let mut next = from_index;
    loop {
        let v = rpc
            .view(
                contract,
                "batches",
                json!({"from_index": next, "limit": 100}),
            )
            .map_err(|e| e.to_string())?;
        let page: Vec<Entry> =
            serde_json::from_value(v).map_err(|e| format!("batches: unexpected shape: {e}"))?;
        if page.is_empty() {
            return Ok(out);
        }
        for e in page {
            if e.index != next {
                return Err(format!("batches: expected index {next}, got {}", e.index));
            }
            next += 1;
            out.push(e);
        }
    }
}

/// Where batch bytes may come from.
pub struct Sources {
    /// NEAR JSON-RPC endpoints, tried in order (put an archival one last).
    pub rpcs: Vec<Rpc>,
    /// neardata.xyz-style block APIs (`<base>/v0/block/<height>`).
    pub block_apis: Vec<String>,
    /// How many NEAR blocks before `near_block` a scan looks at.
    pub scan_back: u64,
    /// Ignore the index's `tx_hash` and always scan (tests the path a
    /// reader takes when a batch has no recorded transaction).
    pub scan_only: bool,
    /// Scan NEAR RPCs (`block` + `chunk`).
    pub scan_rpcs: bool,
    /// Scan the block APIs.
    pub scan_block_apis: bool,
    agent: ureq::Agent,
}

impl Sources {
    /// Sources with an HTTP timeout for the block APIs.
    pub fn new(rpcs: Vec<Rpc>, block_apis: Vec<String>, scan_back: u64, timeout: Duration) -> Self {
        Self {
            rpcs,
            block_apis,
            scan_back,
            scan_only: false,
            scan_rpcs: true,
            scan_block_apis: true,
            agent: ureq::AgentBuilder::new().timeout(timeout).build(),
        }
    }
}

/// A batch's bytes and where they were found.
pub struct Fetched {
    /// The SOVADA1 bytes (sha256 checked against the index).
    pub bytes: Vec<u8>,
    /// The carrying transaction (base58).
    pub tx_hash: String,
    /// Which source answered.
    pub source: String,
}

/// Fetch one batch's bytes. `contract` is the receiver of the `post`
/// call, `owner` its signer.
pub fn fetch(src: &Sources, contract: &str, owner: &str, e: &Entry) -> Result<Fetched, String> {
    let mut errors = Vec::new();
    if let Some(h) = e.tx_hash.as_ref().filter(|_| !src.scan_only) {
        for rpc in &src.rpcs {
            match by_tx_hash(rpc, contract, owner, h, &e.sha256) {
                Ok(bytes) => {
                    return Ok(Fetched {
                        bytes,
                        tx_hash: h.clone(),
                        source: format!("tx@{}", rpc.url()),
                    });
                }
                Err(err) => errors.push(format!("tx@{}: {err}", rpc.url())),
            }
        }
    }
    let lo = e.near_block.saturating_sub(src.scan_back);
    for rpc in src.rpcs.iter().filter(|_| src.scan_rpcs) {
        match scan_rpc(rpc, contract, &e.sha256, lo, e.near_block) {
            Ok(Some((bytes, tx_hash))) => {
                return Ok(Fetched {
                    bytes,
                    tx_hash,
                    source: format!("scan@{}", rpc.url()),
                });
            }
            Ok(None) => errors.push(format!(
                "scan@{}: not in blocks {lo}..={}",
                rpc.url(),
                e.near_block
            )),
            Err(err) => errors.push(format!("scan@{}: {err}", rpc.url())),
        }
    }
    for api in src.block_apis.iter().filter(|_| src.scan_block_apis) {
        match scan_block_api(&src.agent, api, contract, &e.sha256, lo, e.near_block) {
            Ok(Some((bytes, tx_hash))) => {
                return Ok(Fetched {
                    bytes,
                    tx_hash,
                    source: format!("scan@{api}"),
                });
            }
            Ok(None) => errors.push(format!("scan@{api}: not in blocks {lo}..={}", e.near_block)),
            Err(err) => errors.push(format!("scan@{api}: {err}")),
        }
    }
    Err(format!(
        "batch {} (heights {}..={}) not found: {}",
        e.index,
        e.first_height,
        e.last_height,
        errors.join("; ")
    ))
}

fn by_tx_hash(
    rpc: &Rpc,
    contract: &str,
    owner: &str,
    hash: &str,
    sha: &str,
) -> Result<Vec<u8>, String> {
    // Only the bytes matter (checked against the index's sha256 below), so
    // the transaction's own outcome is not required to be a success: a
    // duplicate that the contract refused carries the same bytes.
    let r = rpc.tx(hash, owner, "FINAL").map_err(|e| e.to_string())?;
    let args = near::function_call_args(&r["transaction"], contract, "post")
        .ok_or("not a single post call on the contract")?;
    if sha256_hex(&args) != sha {
        return Err("argument bytes do not match the index's sha256".to_owned());
    }
    Ok(args)
}

/// Look through the transactions of `txs` (RPC `chunk.transactions` or
/// neardata's `transactions[].transaction`) for a `post` to `contract`
/// whose argument hashes to `sha`.
fn match_txs(txs: &[Value], contract: &str, sha: &str) -> Option<(Vec<u8>, String)> {
    for t in txs {
        let tx = if t.get("transaction").is_some() {
            &t["transaction"]
        } else {
            t
        };
        if let Some(args) = near::function_call_args(tx, contract, "post")
            && sha256_hex(&args) == sha
        {
            return Some((args, tx["hash"].as_str().unwrap_or_default().to_owned()));
        }
    }
    None
}

fn scan_rpc(
    rpc: &Rpc,
    contract: &str,
    sha: &str,
    lo: u64,
    hi: u64,
) -> Result<Option<(Vec<u8>, String)>, String> {
    for h in (lo..=hi).rev() {
        let block = match rpc.block(h) {
            Ok(b) => b,
            // Heights with no block (skipped) are normal.
            Err(e) if e.name() == Some("UNKNOWN_BLOCK") => continue,
            Err(e) => return Err(e.to_string()),
        };
        for c in block["chunks"].as_array().cloned().unwrap_or_default() {
            // Only chunks produced at this height carry new transactions.
            if c["height_included"].as_u64() != Some(h) {
                continue;
            }
            let hash = c["chunk_hash"].as_str().unwrap_or_default();
            let chunk = rpc.chunk(hash).map_err(|e| e.to_string())?;
            let txs = chunk["transactions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if let Some(found) = match_txs(&txs, contract, sha) {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

fn scan_block_api(
    agent: &ureq::Agent,
    base: &str,
    contract: &str,
    sha: &str,
    lo: u64,
    hi: u64,
) -> Result<Option<(Vec<u8>, String)>, String> {
    for h in (lo..=hi).rev() {
        let url = format!("{}/v0/block/{h}", base.trim_end_matches('/'));
        let v: Value = match agent.get(&url).call() {
            Ok(r) => r.into_json().map_err(|e| format!("{url}: {e}"))?,
            Err(ureq::Error::Status(404, _)) => continue,
            Err(e) => return Err(format!("{url}: {e}")),
        };
        // A skipped height comes back as JSON null.
        for s in v["shards"].as_array().cloned().unwrap_or_default() {
            let txs = s["chunk"]["transactions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if let Some(found) = match_txs(&txs, contract, sha) {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn matches_rpc_and_neardata_shapes() {
        let args = b"SOVADA1\0rest".to_vec();
        let sha = sha256_hex(&args);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&args);
        let tx = json!({"hash": "H1", "receiver_id": "c.testnet", "actions": [{"FunctionCall": {"method_name": "post", "args": b64}}]});
        let other = json!({"hash": "H0", "receiver_id": "x.testnet", "actions": []});
        assert_eq!(
            match_txs(&[other.clone(), tx.clone()], "c.testnet", &sha),
            Some((args.clone(), "H1".to_owned()))
        );
        let wrapped = json!({"transaction": tx, "outcome": {}});
        assert_eq!(
            match_txs(&[json!({"transaction": other}), wrapped], "c.testnet", &sha),
            Some((args, "H1".to_owned()))
        );
        assert_eq!(match_txs(&[], "c.testnet", &sha), None);
    }

    #[test]
    fn entry_and_info_shapes() {
        let e: Entry = serde_json::from_value(json!({
            "index": 0, "first_height": 0, "last_height": 9, "count": 10, "bytes": 1000,
            "sha256": "ab", "last_hash": format!("0x{}", "11".repeat(32)), "near_block": 5, "tx_hash": null
        }))
        .unwrap();
        assert_eq!(e.count, 10);
        assert_eq!(parse_0x_hash(&e.last_hash).unwrap(), [0x11; 32]);
        let i: Info = serde_json::from_value(json!({
            "format": "SOVADA1", "owner": "a.testnet", "chain_id": 82330, "start_height": 0,
            "next_height": 10, "batch_count": 1, "last_hash": null, "bytes_posted": 1000
        }))
        .unwrap();
        assert_eq!(i.last_hash_bytes().unwrap(), None);
    }
}
