//! The handful of zebrad queries the faucet makes, as a trait so the drip
//! logic is testable against an in-memory node.

use burn_wallet::RpcClient;
use burn_wallet::RpcError;
use burn_wallet::rpc::AddressUtxo;
use serde_json::Value;

/// Zcash Mainnet's genesis block hash (RPC display order).
pub(crate) const MAINNET_GENESIS: &str =
    "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08";
/// Zcash Testnet's genesis block hash (RPC display order).
pub(crate) const TESTNET_GENESIS: &str =
    "05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38";

/// Where the node places a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TxStatus {
    /// Unknown to the node (never seen, evicted, or expired).
    Unknown,
    /// In the mempool, not yet mined.
    Mempool,
    /// Mined in the best chain.
    Confirmed,
}

pub(crate) trait Node {
    /// `getblockchaininfo.chain`: `"main"` or `"test"` (zebrad reports
    /// regtest as `"test"`).
    fn chain(&self) -> Result<String, RpcError>;
    fn block_hash(&self, height: u64) -> Result<String, RpcError>;
    fn tip_height(&self) -> Result<u64, RpcError>;
    fn address_utxos(&self, address: &str) -> Result<Vec<AddressUtxo>, RpcError>;
    fn tx_status(&self, txid: &str) -> Result<TxStatus, RpcError>;
    fn is_coinbase(&self, txid: &str) -> Result<bool, RpcError>;
    /// Broadcasts a raw transaction; returns the node's txid.
    fn send_raw(&self, raw_hex: &str) -> Result<String, RpcError>;
}

impl Node for RpcClient {
    fn chain(&self) -> Result<String, RpcError> {
        let info = self.get_blockchain_info()?;
        Ok(info
            .get("chain")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    fn block_hash(&self, height: u64) -> Result<String, RpcError> {
        self.get_block_hash(height)
    }

    fn tip_height(&self) -> Result<u64, RpcError> {
        self.get_block_count()
    }

    fn address_utxos(&self, address: &str) -> Result<Vec<AddressUtxo>, RpcError> {
        self.get_address_utxos(address)
    }

    fn tx_status(&self, txid: &str) -> Result<TxStatus, RpcError> {
        match self.get_raw_transaction_verbose(txid) {
            // zebrad: `height` is -1 (and `confirmations` 0) for a mempool
            // transaction, the block height once mined.
            Ok(tx) => {
                let height = tx.get("height").and_then(Value::as_i64).unwrap_or(-1);
                let confirmations = tx.get("confirmations").and_then(Value::as_u64);
                if height >= 0 && confirmations.is_none_or(|c| c > 0) {
                    Ok(TxStatus::Confirmed)
                } else {
                    Ok(TxStatus::Mempool)
                }
            }
            // The node answered and doesn't have it (-5). Transport errors
            // still propagate: "couldn't ask" is not "not there".
            Err(RpcError::RpcFailure { .. }) => Ok(TxStatus::Unknown),
            Err(e) => Err(e),
        }
    }

    fn is_coinbase(&self, txid: &str) -> Result<bool, RpcError> {
        let tx = self.get_raw_transaction_verbose(txid)?;
        Ok(tx
            .get("vin")
            .and_then(Value::as_array)
            .and_then(|v| v.first())
            .is_some_and(|input| input.get("coinbase").is_some()))
    }

    fn send_raw(&self, raw_hex: &str) -> Result<String, RpcError> {
        self.send_raw_transaction(raw_hex)
    }
}
