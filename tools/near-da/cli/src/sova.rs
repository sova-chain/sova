//! The Sova node's JSON-RPC, read-only: heights, hashes and raw blocks.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{Value, json};

/// Where raw block bytes come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum RawSource {
    /// The first the node serves, in order: `debug`, `raw-tx`, `full-tx`.
    Auto,
    /// `debug_getRawBlock` (node started with `SOVA_RPC_DEBUG=1`).
    Debug,
    /// Rebuilt from `eth_getBlockByNumber` +
    /// `eth_getRawTransactionByBlockHashAndIndex` (local RPC profile),
    /// checked against the header's hash and roots.
    #[value(alias = "eth")]
    RawTx,
    /// Rebuilt from `eth_getBlockByNumber(n, true)` alone, each transaction
    /// object re-encoded to its EIP-2718 bytes (works on the `public` RPC
    /// profile), checked against each tx hash and the header's hash and
    /// roots.
    FullTx,
}

impl RawSource {
    /// For log lines.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Debug => "debug_getRawBlock",
            Self::RawTx => {
                "rebuilt from eth_getBlockByNumber + eth_getRawTransactionByBlockHashAndIndex (hash- and root-checked)"
            }
            Self::FullTx => {
                "rebuilt from eth_getBlockByNumber(full) with transactions re-encoded (tx-hash-, hash- and root-checked)"
            }
        }
    }
}

/// A JSON-RPC failure.
enum CallError {
    /// -32601: the node does not serve the method.
    MethodNotFound,
    Other(String),
}

/// A Sova (reth) JSON-RPC endpoint.
pub struct Sova {
    url: String,
    agent: ureq::Agent,
    requested: RawSource,
    resolved: OnceLock<RawSource>,
}

impl Sova {
    /// A client for `url` that picks the raw block source automatically.
    pub fn new(url: &str, timeout: Duration) -> Self {
        Self::with_source(url, timeout, RawSource::Auto)
    }

    /// A client for `url` with a fixed (or `Auto`) raw block source.
    pub fn with_source(url: &str, timeout: Duration, source: RawSource) -> Self {
        Self {
            url: url.to_owned(),
            agent: ureq::AgentBuilder::new().timeout(timeout).build(),
            requested: source,
            resolved: OnceLock::new(),
        }
    }

    fn try_call(&self, method: &str, params: Value) -> Result<Value, CallError> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let v: Value = self
            .agent
            .post(&self.url)
            .send_json(body)
            .map_err(|e| CallError::Other(format!("sova {method}: {e}")))?
            .into_json()
            .map_err(|e| CallError::Other(format!("sova {method}: bad JSON: {e}")))?;
        if let Some(err) = v.get("error") {
            if err["code"].as_i64() == Some(-32601) {
                return Err(CallError::MethodNotFound);
            }
            return Err(CallError::Other(format!("sova {method}: {err}")));
        }
        Ok(v["result"].clone())
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.try_call(method, params).map_err(|e| match e {
            CallError::MethodNotFound => format!("sova {method}: method not found"),
            CallError::Other(e) => e,
        })
    }

    /// `eth_chainId`.
    pub fn chain_id(&self) -> Result<u64, String> {
        parse_qty(&self.call("eth_chainId", json!([]))?)
    }

    /// `eth_blockNumber`.
    pub fn head(&self) -> Result<u64, String> {
        parse_qty(&self.call("eth_blockNumber", json!([]))?)
    }

    /// Number and hash of the block at `tag` (`finalized` or a height).
    /// `None` if the node has no such block.
    pub fn block(&self, tag: BlockTag) -> Result<Option<(u64, [u8; 32])>, String> {
        let r = self.call("eth_getBlockByNumber", json!([tag.param(), false]))?;
        if r.is_null() {
            return Ok(None);
        }
        let number = parse_qty(&r["number"])?;
        let hash = parse_hash(&r["hash"])?;
        Ok(Some((number, hash)))
    }

    /// The raw block source in use: the requested one, or for `Auto` the
    /// result of probing `debug_getRawBlock` once.
    pub fn raw_source(&self) -> Result<RawSource, String> {
        if let Some(s) = self.resolved.get() {
            return Ok(*s);
        }
        let s = match self.requested {
            RawSource::Auto => match self.try_call("debug_getRawBlock", json!(["0x0"])) {
                Ok(_) => RawSource::Debug,
                Err(CallError::Other(e)) => return Err(e),
                Err(CallError::MethodNotFound) => match self.try_call(
                    "eth_getRawTransactionByBlockHashAndIndex",
                    json!([format!("0x{}", "00".repeat(32)), "0x0"]),
                ) {
                    Ok(_) => RawSource::RawTx,
                    Err(CallError::MethodNotFound) => RawSource::FullTx,
                    Err(CallError::Other(e)) => return Err(e),
                },
            },
            other => other,
        };
        Ok(*self.resolved.get_or_init(|| s))
    }

    /// Block `height`'s hash and RLP (exactly `debug_getRawBlock`'s bytes),
    /// from the source in use. Either way keccak-256 of the header in the
    /// returned bytes is the node's block hash, or this fails.
    pub fn raw_block(&self, height: u64) -> Result<([u8; 32], Vec<u8>), String> {
        match self.raw_source()? {
            RawSource::RawTx => self.raw_via_eth(height, false),
            RawSource::FullTx => self.raw_via_eth(height, true),
            _ => {
                let (_, hash) = self
                    .block(BlockTag::Number(height))?
                    .ok_or_else(|| format!("Sova node has no block {height}"))?;
                // By hash, so the bytes are the block just named even if
                // the chain moves meanwhile.
                let r = self.call(
                    "debug_getRawBlock",
                    json!([format!("0x{}", hex::encode(hash))]),
                )?;
                let s = r
                    .as_str()
                    .ok_or_else(|| format!("debug_getRawBlock {height}: not a string"))?;
                let raw = hex::decode(s.trim_start_matches("0x"))
                    .map_err(|e| format!("debug_getRawBlock {height}: {e}"))?;
                check_header_hash(height, &raw, hash)?;
                Ok((hash, raw))
            }
        }
    }

    /// Rebuild a block's RLP from the standard `eth` namespace.
    ///
    /// Ported from `sova-rebuild export` (bin/sova-rebuild/src/export.rs,
    /// branch `rebuild`), which proved it byte-identical to
    /// `debug_getRawBlock` on the box. Differences: transactions are not
    /// decoded (each one's EIP-2718 bytes go into the body as they are: a
    /// legacy transaction is its own RLP list, a typed one an RLP string of
    /// `type || payload`), and besides the header hash this checks the body
    /// against the header's transactions, withdrawals and ommers roots, so
    /// a reconstruction that is not exact is refused.
    ///
    /// `full`: take the transactions from `eth_getBlockByNumber(n, true)`
    /// instead (the `public` profile serves no raw-transaction method):
    /// each JSON transaction is parsed into alloy's `TxEnvelope` (every
    /// Ethereum type: legacy, 2930, 1559, 4844, 7702; Sova's primitives
    /// are reth's `EthPrimitives`) and re-encoded with `encoded_2718`. A
    /// type alloy doesn't know, or any field lost on the way, fails the tx
    /// hash or `transactionsRoot` check and the block is refused.
    pub fn raw_via_eth(&self, height: u64, full: bool) -> Result<([u8; 32], Vec<u8>), String> {
        use alloy_rlp::Encodable as _;
        let v = self.call(
            "eth_getBlockByNumber",
            json!([format!("0x{height:x}"), full]),
        )?;
        if v.is_null() {
            return Err(format!("Sova node has no block {height}"));
        }
        let hash = parse_hash(&v["hash"])?;
        let header: alloy_consensus::Header = serde_json::from_value(v.clone())
            .map_err(|e| format!("block {height}: unexpected header shape: {e}"))?;
        let header_rlp = alloy_rlp::encode(&header);
        if crate::poster::keccak(&header_rlp) != hash {
            return Err(format!(
                "block {height}: the header rebuilt from eth_getBlockByNumber does not hash to the block hash (a header field this tool does not know?); refusing it"
            ));
        }
        if v["uncles"].as_array().is_some_and(|u| !u.is_empty())
            || header.ommers_hash != alloy_consensus::EMPTY_OMMER_ROOT_HASH
        {
            return Err(format!(
                "block {height} has ommers; only debug_getRawBlock can archive it"
            ));
        }
        let list = v["transactions"]
            .as_array()
            .ok_or_else(|| format!("block {height}: no transactions array"))?;
        let n = list.len();
        let mut txs = Vec::with_capacity(n);
        if full {
            txs = list
                .iter()
                .enumerate()
                .map(|(i, t)| reencode_tx(height, i, t))
                .collect::<Result<_, _>>()?;
        }
        for i in (0..n).filter(|_| !full) {
            let r = self.call(
                "eth_getRawTransactionByBlockHashAndIndex",
                json!([format!("0x{}", hex::encode(hash)), format!("0x{i:x}")]),
            )?;
            let raw = r
                .as_str()
                .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
                .filter(|b| !b.is_empty())
                .ok_or_else(|| format!("block {height} tx {i}: bad raw transaction"))?;
            txs.push(raw);
        }
        let tx_root = alloy_consensus::proofs::ordered_trie_root_with_encoder(&txs, |t, buf| {
            buf.extend_from_slice(t)
        });
        if tx_root != header.transactions_root {
            return Err(format!(
                "block {height}: rebuilt transactions do not match transactionsRoot; refusing it"
            ));
        }
        let withdrawals: Option<Vec<alloy_eips::eip4895::Withdrawal>> = match v.get("withdrawals") {
            None | Some(Value::Null) => None,
            Some(w) => Some(
                serde_json::from_value(w.clone())
                    .map_err(|e| format!("block {height}: bad withdrawals: {e}"))?,
            ),
        };
        let w_root = withdrawals
            .as_deref()
            .map(alloy_consensus::proofs::calculate_withdrawals_root);
        if w_root != header.withdrawals_root {
            return Err(format!(
                "block {height}: rebuilt withdrawals do not match withdrawalsRoot; refusing it"
            ));
        }

        let mut tx_list = Vec::new();
        for t in &txs {
            if t[0] >= 0xc0 {
                // Legacy: already an RLP list.
                tx_list.extend_from_slice(t);
            } else {
                alloy_rlp::Header {
                    list: false,
                    payload_length: t.len(),
                }
                .encode(&mut tx_list);
                tx_list.extend_from_slice(t);
            }
        }
        let mut payload = header_rlp;
        alloy_rlp::Header {
            list: true,
            payload_length: tx_list.len(),
        }
        .encode(&mut payload);
        payload.extend_from_slice(&tx_list);
        payload.push(alloy_rlp::EMPTY_LIST_CODE); // no ommers
        if let Some(w) = &withdrawals {
            w.encode(&mut payload);
        }
        let mut out = Vec::with_capacity(payload.len() + 5);
        alloy_rlp::Header {
            list: true,
            payload_length: payload.len(),
        }
        .encode(&mut out);
        out.extend_from_slice(&payload);
        check_header_hash(height, &out, hash)?;
        Ok((hash, out))
    }

    /// The `type` of every transaction in block `height` (for test
    /// summaries).
    pub fn tx_types(&self, height: u64) -> Result<Vec<String>, String> {
        let v = self.call(
            "eth_getBlockByNumber",
            json!([format!("0x{height:x}"), true]),
        )?;
        Ok(v["transactions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|t| t["type"].as_str().unwrap_or("?").to_owned())
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `debug_getRawBlock` by height, unconditionally (for `check-sources`).
    pub fn raw_via_debug(&self, height: u64) -> Result<Vec<u8>, String> {
        let r = self.call("debug_getRawBlock", json!([format!("0x{height:x}")]))?;
        let s = r
            .as_str()
            .ok_or_else(|| format!("debug_getRawBlock {height}: not a string"))?;
        hex::decode(s.trim_start_matches("0x"))
            .map_err(|e| format!("debug_getRawBlock {height}: {e}"))
    }
}

/// One transaction object from `eth_getBlockByNumber(n, true)` back to its
/// EIP-2718 bytes, checked against the object's own `hash`.
fn reencode_tx(height: u64, i: usize, t: &Value) -> Result<Vec<u8>, String> {
    use alloy_eips::eip2718::Encodable2718 as _;
    let tx: alloy_rpc_types_eth::Transaction = serde_json::from_value(t.clone()).map_err(|e| {
        format!(
            "block {height} tx {i}: cannot re-encode (type {}): {e}",
            t["type"].as_str().unwrap_or("?")
        )
    })?;
    let raw = tx.inner.into_inner().encoded_2718();
    let want = parse_hash(&t["hash"])?;
    if crate::poster::keccak(&raw) != want {
        return Err(format!(
            "block {height} tx {i}: re-encoded bytes do not hash to the tx hash; refusing the block"
        ));
    }
    Ok(raw)
}

fn check_header_hash(height: u64, raw: &[u8], hash: [u8; 32]) -> Result<(), String> {
    let header = sovada::rlp::block_header(raw).map_err(|e| format!("block {height}: {e}"))?;
    if crate::poster::keccak(header) != hash {
        return Err(format!(
            "block {height}: raw block's header does not hash to the block hash"
        ));
    }
    Ok(())
}

/// A block selector.
#[derive(Clone, Copy, Debug)]
pub enum BlockTag {
    /// A height.
    Number(u64),
    /// The node's `finalized` block.
    Finalized,
}

impl BlockTag {
    fn param(self) -> Value {
        match self {
            Self::Number(n) => json!(format!("0x{n:x}")),
            Self::Finalized => json!("finalized"),
        }
    }
}

fn parse_qty(v: &Value) -> Result<u64, String> {
    let s = v.as_str().ok_or_else(|| format!("not a quantity: {v}"))?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|e| format!("bad quantity {s}: {e}"))
}

fn parse_hash(v: &Value) -> Result<[u8; 32], String> {
    let s = v.as_str().ok_or_else(|| format!("not a hash: {v}"))?;
    let b = hex::decode(s.trim_start_matches("0x")).map_err(|e| format!("bad hash {s}: {e}"))?;
    b.try_into()
        .map_err(|_| format!("hash {s} is not 32 bytes"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy_consensus::transaction::Recovered;
    use alloy_consensus::{
        SignableTransaction, TxEip1559, TxEip2930, TxEip4844, TxEip7702, TxEnvelope, TxLegacy,
    };
    use alloy_eips::eip2718::Encodable2718;
    use alloy_primitives::{Address, Bytes, Signature, TxKind, U256};

    /// The `full-tx` path for every Ethereum tx type: a transaction as the
    /// node's RPC serializes it, parsed back and re-encoded, must give the
    /// exact EIP-2718 bytes (and so the tx hash) of the original. Type 3
    /// (4844) is covered only here: the box node is post-Osaka and cast
    /// can't build the EIP-7594 sidecar it wants.
    #[test]
    fn full_tx_reencode_every_type() {
        let sig = Signature::test_signature();
        let to = Address::repeat_byte(0xde);
        let input = Bytes::from(vec![1u8, 2, 3]);
        let envs: Vec<TxEnvelope> = vec![
            TxLegacy {
                chain_id: Some(1337),
                nonce: 1,
                gas_price: 7,
                gas_limit: 21_000,
                to: TxKind::Call(to),
                value: U256::from(5),
                input: input.clone(),
            }
            .into_signed(sig)
            .into(),
            TxLegacy {
                chain_id: None,
                nonce: 2,
                gas_price: 7,
                gas_limit: 21_000,
                to: TxKind::Create,
                value: U256::ZERO,
                input: input.clone(),
            }
            .into_signed(sig)
            .into(),
            TxEip2930 {
                chain_id: 1337,
                nonce: 3,
                gas_price: 7,
                gas_limit: 30_000,
                to: TxKind::Call(to),
                input: input.clone(),
                ..Default::default()
            }
            .into_signed(sig)
            .into(),
            TxEip1559 {
                chain_id: 1337,
                nonce: 4,
                gas_limit: 21_000,
                max_fee_per_gas: 9,
                max_priority_fee_per_gas: 1,
                to: TxKind::Call(to),
                input: input.clone(),
                ..Default::default()
            }
            .into_signed(sig)
            .into(),
            TxEip4844 {
                chain_id: 1337,
                nonce: 5,
                gas_limit: 50_000,
                max_fee_per_gas: 9,
                max_priority_fee_per_gas: 1,
                to,
                blob_versioned_hashes: vec![alloy_primitives::B256::repeat_byte(1)],
                max_fee_per_blob_gas: 3,
                input: input.clone(),
                ..Default::default()
            }
            .into_signed(sig)
            .into(),
            TxEip7702 {
                chain_id: 1337,
                nonce: 6,
                gas_limit: 60_000,
                max_fee_per_gas: 9,
                max_priority_fee_per_gas: 1,
                to,
                input,
                ..Default::default()
            }
            .into_signed(sig)
            .into(),
        ];
        for (i, env) in envs.into_iter().enumerate() {
            let want = env.encoded_2718();
            let rpc = alloy_rpc_types_eth::Transaction {
                inner: Recovered::new_unchecked(env, Address::ZERO),
                block_hash: None,
                block_number: Some(7),
                transaction_index: Some(i as u64),
                effective_gas_price: None,
                block_timestamp: None,
            };
            let json = serde_json::to_value(&rpc).unwrap();
            assert_eq!(
                reencode_tx(7, i, &json).unwrap(),
                want,
                "tx {i} ({})",
                json["type"]
            );
        }
    }

    #[test]
    fn full_tx_refuses_a_wrong_hash() {
        let env: TxEnvelope = TxEip1559 {
            chain_id: 1337,
            ..Default::default()
        }
        .into_signed(Signature::test_signature())
        .into();
        let rpc = alloy_rpc_types_eth::Transaction {
            inner: Recovered::new_unchecked(env, Address::ZERO),
            block_hash: None,
            block_number: None,
            transaction_index: None,
            effective_gas_price: None,
            block_timestamp: None,
        };
        let mut json = serde_json::to_value(&rpc).unwrap();
        json["hash"] = json!(format!("0x{}", "11".repeat(32)));
        assert!(reencode_tx(1, 0, &json).is_err());
    }
}
