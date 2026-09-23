//! On-chain verification: scans a range of blocks for SIP-1 burns, the same
//! way `crates/burn-wallet/tests/e2e_regtest_burn.rs` confirms one burn --
//! by feeding real, node-confirmed transaction outputs through
//! `consensus::sip1::extract_burn` -- generalized here to a full block
//! range so `report --verify-rpc` can independently corroborate this
//! miner's own bookkeeping against the chain itself.

use consensus::sip1::{Burn, TxOutRef, extract_burn};
use serde_json::Value;

use burn_wallet::rpc::{RpcClient, RpcError};

/// One burn found on chain.
#[derive(Debug, Clone)]
pub(crate) struct ChainBurn {
    /// The block height it was confirmed in.
    pub height: u64,
    /// The transaction's txid, in RPC-display hex.
    pub txid: String,
    /// The recognized burn (evm address, signal bits, total value).
    pub burn: Burn,
}

/// Errors scanning the chain.
#[derive(Debug, thiserror::Error)]
pub(crate) enum VerifyError {
    /// An RPC call failed.
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// A block or transaction's JSON shape didn't match what we expect.
    #[error("unexpected getblock response shape at height {height}: {detail}")]
    UnexpectedShape {
        /// The block height being parsed.
        height: u64,
        /// What was missing or malformed.
        detail: String,
    },
}

/// Scans every block in `from_height..=to_height` (inclusive) for SIP-1
/// burns, across *all* transactions. Costs one `getblock` per height, so
/// callers bound the range (`report --verify-rpc` starts at the miner's
/// first recorded epoch, never at genesis).
///
/// # Errors
///
/// Returns [`VerifyError`] if any RPC call fails or a block's shape doesn't
/// match what this helper expects.
pub(crate) fn scan_chain_burns(
    rpc: &RpcClient,
    from_height: u64,
    to_height: u64,
) -> Result<Vec<ChainBurn>, VerifyError> {
    let mut found = Vec::new();
    for height in from_height..=to_height {
        let hash = rpc.get_block_hash(height)?;
        let block = rpc.get_block_verbose(&hash)?;
        let txs = block.get("tx").and_then(Value::as_array).ok_or_else(|| {
            VerifyError::UnexpectedShape {
                height,
                detail: "missing tx[]".to_string(),
            }
        })?;

        for tx in txs {
            let txid = tx.get("txid").and_then(Value::as_str).ok_or_else(|| {
                VerifyError::UnexpectedShape {
                    height,
                    detail: "missing tx[].txid".to_string(),
                }
            })?;
            let vout = tx.get("vout").and_then(Value::as_array).ok_or_else(|| {
                VerifyError::UnexpectedShape {
                    height,
                    detail: "missing tx[].vout".to_string(),
                }
            })?;

            let decoded: Result<Vec<(u64, Vec<u8>)>, VerifyError> = vout
                .iter()
                .map(|out| {
                    let value_zat =
                        out.get("valueZat").and_then(Value::as_u64).ok_or_else(|| {
                            VerifyError::UnexpectedShape {
                                height,
                                detail: "missing vout[].valueZat".to_string(),
                            }
                        })?;
                    let script_hex = out
                        .get("scriptPubKey")
                        .and_then(|spk| spk.get("hex"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| VerifyError::UnexpectedShape {
                            height,
                            detail: "missing vout[].scriptPubKey.hex".to_string(),
                        })?;
                    let script =
                        hex::decode(script_hex).map_err(|_| VerifyError::UnexpectedShape {
                            height,
                            detail: "vout[].scriptPubKey.hex was not valid hex".to_string(),
                        })?;
                    Ok((value_zat, script))
                })
                .collect();
            let decoded = decoded?;

            let refs: Vec<TxOutRef<'_>> = decoded
                .iter()
                .map(|(value_zat, script)| TxOutRef {
                    value_zat: *value_zat,
                    script: script.as_slice(),
                })
                .collect();

            if let Some(burn) = extract_burn(refs) {
                found.push(ChainBurn {
                    height,
                    txid: txid.to_string(),
                    burn,
                });
            }
        }
    }
    Ok(found)
}
