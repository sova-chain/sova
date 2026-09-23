//! The zebrad queries `mine` makes to fund, broadcast and confirm burns, as
//! a trait so that logic is unit-testable against an in-memory node (the
//! same pattern as `sova-faucet`'s `node.rs`).
//!
//! Every query here is cheap at any chain height: `getaddressutxos` is
//! served from zebrad's transparent address index, and `getrawtransaction`
//! from its transaction index. Nothing walks blocks.

use burn_wallet::RpcClient;
use burn_wallet::RpcError;
use burn_wallet::rpc::AddressUtxo;
use serde_json::Value;

/// zebrad's JSON-RPC code for "No such mempool or main chain transaction"
/// (`getrawtransaction` on a txid it doesn't have).
const RPC_NOT_FOUND: i64 = -5;

/// Where the node places a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TxStatus {
    /// Unknown to the node: never seen, rejected, evicted, or expired.
    Unknown,
    /// In the mempool, not yet mined.
    Mempool,
    /// Mined in the best chain at `height`.
    Confirmed {
        /// The height of the block that mined it.
        height: u64,
    },
}

/// The confirmed UTXOs paying one address, and the tip they were read at.
#[derive(Debug, Clone, Default)]
pub(crate) struct AddressSnapshot {
    /// Unspent transparent outputs in the best chain (never mempool ones).
    pub utxos: Vec<AddressUtxo>,
    /// The tip height the list was read at.
    pub tip_height: u64,
}

/// The node operations the burn loop needs.
pub(crate) trait Node {
    /// `getblockcount`.
    fn tip_height(&self) -> Result<u64, RpcError>;
    /// `getaddressutxos` (with `chainInfo`) for one address.
    fn address_utxos(&self, address: &str) -> Result<AddressSnapshot, RpcError>;
    /// Whether `txid` (known to the node) is a coinbase transaction.
    fn is_coinbase(&self, txid: &str) -> Result<bool, RpcError>;
    /// Where the node places `txid`. "Couldn't ask" (a transport error) is
    /// an `Err`, never [`TxStatus::Unknown`].
    fn tx_status(&self, txid: &str) -> Result<TxStatus, RpcError>;
    /// `sendrawtransaction`; returns the node's txid.
    fn send_raw(&self, raw_hex: &str) -> Result<String, RpcError>;
}

/// Parses a verbose `getrawtransaction` answer into a [`TxStatus`]: zebrad
/// reports `height: -1` (and `confirmations: 0`) for a mempool
/// transaction, the block height once mined.
fn status_of(tx: &Value) -> TxStatus {
    let height = tx.get("height").and_then(Value::as_i64).unwrap_or(-1);
    let confirmations = tx.get("confirmations").and_then(Value::as_u64);
    match u64::try_from(height) {
        Ok(height) if confirmations.is_none_or(|c| c > 0) => TxStatus::Confirmed { height },
        _ => TxStatus::Mempool,
    }
}

/// Whether a verbose `getrawtransaction` answer is a coinbase transaction:
/// its first input carries a `coinbase` field (zebrad's `Input::Coinbase`).
fn is_coinbase_tx(tx: &Value) -> bool {
    tx.get("vin")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
        .is_some_and(|input| input.get("coinbase").is_some())
}

impl Node for RpcClient {
    fn tip_height(&self) -> Result<u64, RpcError> {
        self.get_block_count()
    }

    fn address_utxos(&self, address: &str) -> Result<AddressSnapshot, RpcError> {
        let answer = self.get_address_utxos_at_tip(address)?;
        Ok(AddressSnapshot {
            utxos: answer.utxos,
            tip_height: answer.height,
        })
    }

    fn is_coinbase(&self, txid: &str) -> Result<bool, RpcError> {
        Ok(is_coinbase_tx(&self.get_raw_transaction_verbose(txid)?))
    }

    fn tx_status(&self, txid: &str) -> Result<TxStatus, RpcError> {
        match self.get_raw_transaction_verbose(txid) {
            Ok(tx) => Ok(status_of(&tx)),
            Err(RpcError::RpcFailure { code, .. }) if code == RPC_NOT_FOUND => {
                Ok(TxStatus::Unknown)
            }
            Err(e) => Err(e),
        }
    }

    fn send_raw(&self, raw_hex: &str) -> Result<String, RpcError> {
        self.send_raw_transaction(raw_hex)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn mempool_and_mined_transactions_are_told_apart() {
        assert_eq!(
            status_of(&json!({"height": -1, "confirmations": 0})),
            TxStatus::Mempool
        );
        assert_eq!(
            status_of(&json!({"height": 3_256_800, "confirmations": 3})),
            TxStatus::Confirmed { height: 3_256_800 }
        );
        // Older zebrad omitted `confirmations`; a height alone means mined.
        assert_eq!(
            status_of(&json!({"height": 12})),
            TxStatus::Confirmed { height: 12 }
        );
        assert_eq!(status_of(&json!({})), TxStatus::Mempool);
    }

    #[test]
    fn coinbase_is_read_from_the_first_input() {
        assert!(is_coinbase_tx(
            &json!({"vin": [{"coinbase": "5100", "sequence": 4_294_967_295u64}]})
        ));
        assert!(!is_coinbase_tx(
            &json!({"vin": [{"txid": "ab", "vout": 0, "scriptSig": {}}]})
        ));
        assert!(!is_coinbase_tx(&json!({"vin": []})));
    }
}
