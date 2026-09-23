//! Minimal coinbase UTXO discovery.
//!
//! This is deliberately simple: it walks blocks one at a time via
//! `getblock <hash> 2` and looks at each coinbase transaction's outputs for
//! ones paying a given address. That is entirely adequate at regtest scale
//! (dozens to low hundreds of blocks) and for a wallet that only ever
//! receives coinbase rewards to one address; it is not a production
//! indexer (no mempool awareness, no non-coinbase outputs, no chain-reorg
//! handling, O(chain height) per call).

use serde_json::Value;
use zcash_transparent::bundle::OutPoint;

use crate::rpc::{RpcClient, RpcError};

/// Standard Zcash/Bitcoin coinbase maturity: a coinbase output only becomes
/// spendable once the chain tip is at least this many blocks past it.
pub const COINBASE_MATURITY: u32 = 100;

/// Where the operator docs explain shielding coinbase so it can fund
/// transparent transactions (see
/// [`Network::allows_unshielded_coinbase_spends`](crate::Network::allows_unshielded_coinbase_spends)).
pub const COINBASE_SHIELDING_DOC: &str = "docs/ops/keeper-miner.md#coinbase-must-be-shielded-first";

/// The actionable explanation for `coinbase_zat` of transparent coinbase
/// that can't fund a transparent `what` (`"burn"`, `"drip"`) on a network
/// that forbids unshielded coinbase spends (testnet, mainnet).
pub fn coinbase_must_be_shielded_message(coinbase_zat: u64, what: &str) -> String {
    format!(
        "{coinbase_zat} zat of coinbase must be shielded before it can fund a transparent {what} \
         (Zcash consensus on testnet and mainnet only lets transparent coinbase be spent into \
         shielded outputs): shield it, send it back to this t-addr as an ordinary transfer, \
         then retry -- see {COINBASE_SHIELDING_DOC}"
    )
}

#[cfg(test)]
mod coinbase_message_tests {
    use super::*;

    #[test]
    fn message_names_the_amount_and_the_doc() {
        let m = coinbase_must_be_shielded_message(1_500_000_000, "burn");
        assert!(m.starts_with(
            "1500000000 zat of coinbase must be shielded before it can fund a transparent burn"
        ));
        assert!(m.ends_with(COINBASE_SHIELDING_DOC));
    }
}

/// A coinbase output discovered on chain that is confirmed and mature.
#[derive(Debug, Clone)]
pub struct SpendableUtxo {
    /// The outpoint (txid + vout index) to reference as a transaction
    /// input.
    pub outpoint: OutPoint,
    /// The output's value, in zatoshis.
    pub value_zat: u64,
    /// The height of the block containing this coinbase output.
    pub height: u64,
}

/// Errors from UTXO discovery.
#[derive(Debug, thiserror::Error)]
pub enum UtxoError {
    /// An RPC call failed.
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// A block's JSON shape did not match what this helper expects.
    #[error("unexpected getblock response shape at height {height}: {detail}")]
    UnexpectedShape {
        /// The block height being parsed.
        height: u64,
        /// What was missing or malformed.
        detail: String,
    },
    /// A `txid`/block hash field was not valid hex, or not 32 bytes.
    #[error("invalid 32-byte hex {label} {value:?}")]
    InvalidHex {
        /// Which field this was (e.g. `"txid"`).
        label: &'static str,
        /// The offending string.
        value: String,
    },
}

/// Decodes a big-endian ("RPC display order") 32-byte hex hash, such as a
/// `txid` or block hash returned by `zebrad`'s RPC, into the little-endian
/// byte order used internally (e.g. in a serialized [`OutPoint`]).
///
/// Zcash inherits Bitcoin's convention here: RPC/JSON/explorer txids and
/// block hashes are displayed byte-reversed relative to the raw hash bytes
/// used on the wire. This holds for v5 (NU5+) transactions too, even
/// though their txid is computed via a BLAKE2b tree (ZIP 244) rather than
/// double-SHA256 -- only the hash *algorithm* changed, not the *display*
/// convention. This is exercised end-to-end by the `e2e_regtest_burn`
/// integration test: it builds a transaction (computing its txid via
/// `zcash_primitives`), submits it, and confirms the node-reported txid
/// (via [`encode_rpc_hash`], this function's inverse) matches -- a
/// mismatch here would show up as that assertion failing.
///
/// Exposed (not just used internally by [`find_spendable_coinbase`]) because
/// callers that persist their own UTXO tracking across process restarts
/// (e.g. `sova-miner`'s state sidecar, which stores txids in the same
/// RPC-display hex it receives from `sendrawtransaction`/`getblock`) need to
/// turn that hex back into an [`OutPoint`] without re-deriving this
/// byte-reversal logic themselves.
///
/// # Errors
///
/// Returns [`UtxoError::InvalidHex`] if `hex_str` is not valid hex or is not
/// exactly 32 bytes.
pub fn decode_rpc_hash(label: &'static str, hex_str: &str) -> Result<[u8; 32], UtxoError> {
    let mut bytes = hex::decode(hex_str).map_err(|_| UtxoError::InvalidHex {
        label,
        value: hex_str.to_string(),
    })?;
    if bytes.len() != 32 {
        return Err(UtxoError::InvalidHex {
            label,
            value: hex_str.to_string(),
        });
    }
    bytes.reverse();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// The inverse of [`decode_rpc_hash`]: encodes a 32-byte hash that is in
/// the internal (wire) byte order -- e.g. a
/// `zcash_primitives::transaction::Transaction::txid()` -- into the
/// byte-reversed hex string used by `zebrad`'s (and `zcashd`'s) RPC
/// surface for txids and block hashes.
#[must_use]
pub fn encode_rpc_hash(bytes: [u8; 32]) -> String {
    let mut reversed = bytes;
    reversed.reverse();
    hex::encode(reversed)
}

/// Finds mature, spendable coinbase UTXOs paying `address` (a base58check
/// transparent address string, as returned by
/// [`crate::keys::Keypair::encode_address`]).
///
/// Walks every block from height 1 to the current tip, inclusive, so cost
/// is `O(tip height)` RPC round trips -- see the module docs for why that's
/// fine here.
///
/// # Errors
///
/// Returns [`UtxoError`] if any RPC call fails or a block's shape doesn't
/// match what this helper expects.
pub fn find_spendable_coinbase(
    rpc: &RpcClient,
    address: &str,
) -> Result<Vec<SpendableUtxo>, UtxoError> {
    let tip = rpc.get_block_count()?;
    let mut found = Vec::new();

    for height in 1..=tip {
        if tip.saturating_sub(height) + 1 < u64::from(COINBASE_MATURITY) {
            continue;
        }

        let hash = rpc.get_block_hash(height)?;
        let block = rpc.get_block_verbose(&hash)?;
        let coinbase = block
            .get("tx")
            .and_then(Value::as_array)
            .and_then(|txs| txs.first())
            .ok_or_else(|| UtxoError::UnexpectedShape {
                height,
                detail: "missing tx[0] (coinbase)".to_string(),
            })?;

        let txid_str = coinbase
            .get("txid")
            .and_then(Value::as_str)
            .ok_or_else(|| UtxoError::UnexpectedShape {
                height,
                detail: "missing tx[0].txid".to_string(),
            })?;
        let txid = decode_rpc_hash("txid", txid_str)?;

        let vout = coinbase
            .get("vout")
            .and_then(Value::as_array)
            .ok_or_else(|| UtxoError::UnexpectedShape {
                height,
                detail: "missing tx[0].vout".to_string(),
            })?;

        for out in vout {
            let pays_us = out
                .get("scriptPubKey")
                .and_then(|spk| spk.get("addresses"))
                .and_then(Value::as_array)
                .is_some_and(|addrs| addrs.iter().any(|a| a.as_str() == Some(address)));
            if !pays_us {
                continue;
            }

            let n =
                out.get("n")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| UtxoError::UnexpectedShape {
                        height,
                        detail: "missing vout[].n".to_string(),
                    })?;
            let value_zat = out.get("valueZat").and_then(Value::as_u64).ok_or_else(|| {
                UtxoError::UnexpectedShape {
                    height,
                    detail: "missing vout[].valueZat".to_string(),
                }
            })?;

            found.push(SpendableUtxo {
                outpoint: OutPoint::new(txid, u32::try_from(n).unwrap_or(u32::MAX)),
                value_zat,
                height,
            });
        }
    }

    Ok(found)
}

#[cfg(test)]
// Test code: an unexpected `Err` here is a test failure.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn decode_rpc_hash_reverses_bytes() {
        let hex_str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let decoded = decode_rpc_hash("txid", hex_str).unwrap();
        assert_eq!(decoded[0], 0x20);
        assert_eq!(decoded[31], 0x01);
        // Round-trips back to the same display string.
        let mut redisplay = decoded;
        redisplay.reverse();
        assert_eq!(hex::encode(redisplay), hex_str);
    }

    #[test]
    fn decode_rpc_hash_rejects_wrong_length() {
        assert!(decode_rpc_hash("txid", "abcd").is_err());
    }
}
