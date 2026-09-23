//! Construction and ZIP-244 signing of SIP-1 burn transactions.
//!
//! # Why not `zcash_primitives::transaction::builder::Builder`
//!
//! librustzcash's top-level [`Builder`](zcash_primitives::transaction::builder::Builder)
//! composes transparent + Sapling + Orchard bundles together, and its
//! `build()` method is generic over `SpendProver`/`OutputProver` (the
//! zk-SNARK proving backends for the shielded pools) *unconditionally* --
//! even when, as here, zero Sapling/Orchard spends or outputs are ever
//! added. Satisfying those bounds for real means depending on the
//! `circuits` proving machinery and (for anything beyond
//! `mock_build`/test-only paths) loading multi-megabyte Sapling/Orchard
//! proving parameters at runtime, none of which a transparent-only wallet
//! should need.
//!
//! Instead this module drives [`zcash_transparent`]'s own bundle builder
//! directly (`TransparentBuilder`, `TransparentSigningSet`,
//! `Bundle::apply_signatures`) -- fully supported, public API from the same
//! `zcash/librustzcash` release train -- and assembles the final
//! `TransactionData` by hand with `sapling_bundle`/`orchard_bundle` left
//! `None`. Signing still goes through librustzcash's real ZIP-244
//! implementation: `zcash_primitives::transaction::{txid::TxIdDigester,
//! sighash::signature_hash}`, the same digest/sighash code the top-level
//! `Builder` itself calls internally. Nothing here reimplements or
//! hand-rolls the sighash algorithm.

use zcash_primitives::transaction::sighash::{SignableInput, signature_hash};
use zcash_primitives::transaction::txid::TxIdDigester;
use zcash_primitives::transaction::{self, TransactionData, TxVersion};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zcash_protocol::value::Zatoshis;
use zcash_transparent::address::TransparentAddress;
use zcash_transparent::builder::{TransparentBuilder, TransparentSigningSet};
use zcash_transparent::bundle::{OutPoint, TxOut};

use consensus::sip1::{BURN_HASH160, BurnPayload, MIN_BURN_ZAT};

use crate::keys::Keypair;
use crate::network::Network;

/// Transaction expiry window, in blocks past `target_height`, used for every
/// transaction this module builds --
/// mirrors `zcash_primitives::transaction::builder::DEFAULT_TX_EXPIRY_DELTA`
/// (kept as our own constant so this module doesn't need the `Builder`
/// import purely for one constant).
pub const DEFAULT_TX_EXPIRY_DELTA: u32 = 40;

/// One spendable transparent coin, referencing a previous output, to be
/// consumed as a transaction input.
///
/// This v0 builder assumes every input is a P2PKH output paying
/// `keypair`'s address (i.e. all inputs share one signing key) -- true for
/// this crate's regtest coinbase-funded flow, and the natural next thing
/// to generalize (per-input keys) if/when this wallet needs to spend
/// UTXOs across multiple addresses.
#[derive(Debug, Clone)]
pub struct Utxo {
    /// The outpoint being spent.
    pub outpoint: OutPoint,
    /// The value of the referenced output, in zatoshis.
    pub value_zat: u64,
}

/// Everything needed to build and sign one SIP-1 burn transaction.
#[derive(Debug, Clone)]
pub struct BurnTxRequest {
    /// The network to build for (selects the consensus branch id and
    /// address encoding).
    pub network: Network,
    /// The current chain tip height. Used to select the consensus branch
    /// id ([`BranchId::for_height`]) and to compute a default expiry
    /// height.
    pub target_height: u32,
    /// Inputs to spend. Must all be P2PKH outputs paying `keypair`'s
    /// address (see [`Utxo`]).
    pub utxos: Vec<Utxo>,
    /// The signing key for every input in `utxos`, and the recipient of
    /// any change output.
    pub change_and_signing_key: Keypair,
    /// The Sova EVM address to credit with this burn's weight.
    pub evm_address: [u8; 20],
    /// BIP9-style upgrade signal bits.
    pub signal_bits: u32,
    /// The amount to burn (paid to the SIP-1 eater script), in zatoshis.
    /// Must be at least [`MIN_BURN_ZAT`].
    pub burn_value_zat: u64,
    /// Flat transaction fee, in zatoshis.
    pub fee_zat: u64,
}

/// A fully built and signed transaction, ready for `sendrawtransaction`.
#[derive(Debug, Clone)]
pub struct BuiltBurnTx {
    /// The transaction's txid.
    pub txid: [u8; 32],
    /// The serialized transaction bytes (consensus encoding -- hex-encode
    /// this for `sendrawtransaction`).
    pub raw: Vec<u8>,
    /// The change returned to `change_and_signing_key`'s address, in
    /// zatoshis (0 if the inputs summed exactly to burn + fee).
    pub change_zat: u64,
}

/// Errors building a burn transaction.
#[derive(Debug, thiserror::Error)]
pub enum BurnTxError {
    /// `utxos` was empty.
    #[error("at least one UTXO is required")]
    NoInputs,
    /// `burn_value_zat` was below [`MIN_BURN_ZAT`].
    #[error("burn value {given} zat is below the SIP-1 minimum of {min} zat")]
    BelowMinBurn {
        /// The requested burn value.
        given: u64,
        /// [`MIN_BURN_ZAT`].
        min: u64,
    },
    /// The inputs did not cover `burn_value_zat + fee_zat`.
    #[error(
        "insufficient input value: have {input_zat} zat, need {needed_zat} zat (burn {burn_zat} + fee {fee_zat})"
    )]
    InsufficientFunds {
        /// Total input value.
        input_zat: u64,
        /// Total needed (burn + fee).
        needed_zat: u64,
        /// The requested burn value.
        burn_zat: u64,
        /// The requested fee.
        fee_zat: u64,
    },
    /// A value did not fit the Zcash `{0..MAX_MONEY}` zatoshi range.
    #[error("value out of range: {0}")]
    ValueOutOfRange(#[from] zcash_protocol::value::BalanceError),
    /// The `zcash_transparent` bundle builder rejected an input or output.
    #[error("transparent bundle builder error: {0}")]
    TransparentBuilder(#[from] zcash_transparent::builder::Error),
    /// Signing or final transaction assembly failed.
    #[error("transaction assembly error: {0}")]
    Assembly(String),
    /// A transfer of zero zatoshis was requested.
    #[error("transfer amount must be non-zero")]
    ZeroAmount,
}

/// Builds and signs a SIP-1 burn transaction: one zero-value SIP-1 payload
/// output (`OP_RETURN`, per [`BurnPayload::to_script`]), one eater output
/// paying [`BURN_HASH160`], and (if any value remains) one change output
/// back to the signing key's own address.
///
/// # Errors
///
/// See [`BurnTxError`]'s variants.
pub fn build_burn_transaction(req: &BurnTxRequest) -> Result<BuiltBurnTx, BurnTxError> {
    if req.utxos.is_empty() {
        return Err(BurnTxError::NoInputs);
    }
    if req.burn_value_zat < MIN_BURN_ZAT {
        return Err(BurnTxError::BelowMinBurn {
            given: req.burn_value_zat,
            min: MIN_BURN_ZAT,
        });
    }

    let input_zat: u64 = req.utxos.iter().map(|u| u.value_zat).sum();
    let needed_zat =
        req.burn_value_zat
            .checked_add(req.fee_zat)
            .ok_or(BurnTxError::InsufficientFunds {
                input_zat,
                needed_zat: u64::MAX,
                burn_zat: req.burn_value_zat,
                fee_zat: req.fee_zat,
            })?;
    let change_zat = input_zat
        .checked_sub(needed_zat)
        .ok_or(BurnTxError::InsufficientFunds {
            input_zat,
            needed_zat,
            burn_zat: req.burn_value_zat,
            fee_zat: req.fee_zat,
        })?;

    let public_key = req.change_and_signing_key.public_key();
    let our_address = req.change_and_signing_key.transparent_address();
    let our_coin_script: zcash_transparent::address::Script = our_address.script().into();

    // -- Build the unsigned transparent bundle. --
    let mut builder = TransparentBuilder::empty();
    for utxo in &req.utxos {
        let coin = TxOut::new(Zatoshis::from_u64(utxo.value_zat)?, our_coin_script.clone());
        builder.add_p2pkh_input(public_key, utxo.outpoint.clone(), coin)?;
    }

    let payload = BurnPayload {
        evm_address: req.evm_address,
        signal_bits: req.signal_bits,
    };
    builder.add_null_data_output(&payload.encode())?;

    let eater_address = TransparentAddress::PublicKeyHash(BURN_HASH160);
    builder.add_output(&eater_address, Zatoshis::from_u64(req.burn_value_zat)?)?;

    if change_zat > 0 {
        builder.add_output(&our_address, Zatoshis::from_u64(change_zat)?)?;
    }

    let (txid, raw) = sign_and_serialize(
        req.network,
        req.target_height,
        builder,
        &req.change_and_signing_key,
    )?;

    Ok(BuiltBurnTx {
        txid,
        raw,
        change_zat,
    })
}

/// Builds the transparent bundle from `builder`, wraps it in a v5
/// transaction for `target_height`, signs every input with `key` via
/// librustzcash's ZIP-244 sighash, and serializes it. Shared by burns and
/// transfers: only the outputs differ between them.
fn sign_and_serialize(
    network: Network,
    target_height: u32,
    builder: TransparentBuilder,
    key: &Keypair,
) -> Result<([u8; 32], Vec<u8>), BurnTxError> {
    let unauthorized_bundle = builder
        .build()
        .ok_or_else(|| BurnTxError::Assembly("empty transparent bundle".to_string()))?;

    // -- Assemble transaction metadata and compute the ZIP-244 txid digest
    //    over the *unsigned* bundle (script_sig is not covered by the
    //    sighash, so this is well-defined before signing). --
    let branch_id = BranchId::for_height(&network, BlockHeight::from_u32(target_height));
    let expiry_height =
        BlockHeight::from_u32(target_height.saturating_add(DEFAULT_TX_EXPIRY_DELTA));
    let lock_time = 0u32;

    let unauthorized_tx_data: TransactionData<transaction::Unauthorized> =
        TransactionData::from_parts(
            TxVersion::V5,
            branch_id,
            lock_time,
            expiry_height,
            Some(unauthorized_bundle.clone()),
            None,
            None,
            None,
        );
    let txid_parts = unauthorized_tx_data.digest(TxIdDigester);

    // -- Sign. --
    let mut signing_set = TransparentSigningSet::new();
    signing_set.add_key(key.secret_key());

    let signed_bundle = unauthorized_bundle.apply_signatures(
        |signable_input| {
            *signature_hash(
                &unauthorized_tx_data,
                &SignableInput::Transparent(signable_input),
                &txid_parts,
            )
            .as_ref()
        },
        &signing_set,
    )?;

    let authorized_tx_data: TransactionData<transaction::Authorized> = TransactionData::from_parts(
        TxVersion::V5,
        branch_id,
        lock_time,
        expiry_height,
        Some(signed_bundle),
        None,
        None,
        None,
    );

    let tx = authorized_tx_data
        .freeze()
        .map_err(|e| BurnTxError::Assembly(e.to_string()))?;

    let mut raw = Vec::new();
    tx.write(&mut raw)
        .map_err(|e| BurnTxError::Assembly(e.to_string()))?;

    Ok((tx.txid().into(), raw))
}

/// Everything needed to build and sign one plain transparent transfer (a
/// payment to one address, plus change back to the signing key). Used by
/// `sova-faucet`.
#[derive(Debug, Clone)]
pub struct TransferTxRequest {
    /// The network to build for.
    pub network: Network,
    /// The height the transaction targets (next block): selects the
    /// consensus branch id and the expiry height.
    pub target_height: u32,
    /// Inputs to spend. Must all be P2PKH outputs paying `change_and_signing_key`.
    pub utxos: Vec<Utxo>,
    /// The signing key for every input, and the recipient of any change.
    pub change_and_signing_key: Keypair,
    /// The payee (P2PKH or P2SH).
    pub recipient: TransparentAddress,
    /// The amount paid to `recipient`, in zatoshis. Must be non-zero.
    pub amount_zat: u64,
    /// The transaction fee, in zatoshis.
    pub fee_zat: u64,
}

/// A fully built and signed transfer, ready for `sendrawtransaction`.
#[derive(Debug, Clone)]
pub struct BuiltTransferTx {
    /// The transaction's txid (internal byte order; see
    /// [`crate::utxo::encode_rpc_hash`] for the RPC display form).
    pub txid: [u8; 32],
    /// The serialized transaction.
    pub raw: Vec<u8>,
    /// The change returned to the signing key's address (output index 1),
    /// or 0 if there is no change output.
    pub change_zat: u64,
}

/// Builds and signs a transparent transfer: output 0 pays `amount_zat` to
/// `recipient`; output 1 (only if non-zero) returns the change to the
/// signing key's own P2PKH address. The caller decides the fee (and
/// whether dust change is folded into it) -- see [`crate::fee`].
///
/// # Errors
///
/// [`BurnTxError::NoInputs`] with no UTXOs, [`BurnTxError::ZeroAmount`] for
/// a zero payment, [`BurnTxError::InsufficientFunds`] (its `burn_zat` field
/// holds the transfer amount) when the inputs don't cover amount + fee, and
/// the builder/assembly variants.
pub fn build_transfer_transaction(req: &TransferTxRequest) -> Result<BuiltTransferTx, BurnTxError> {
    if req.utxos.is_empty() {
        return Err(BurnTxError::NoInputs);
    }
    if req.amount_zat == 0 {
        return Err(BurnTxError::ZeroAmount);
    }
    let input_zat: u64 = req.utxos.iter().map(|u| u.value_zat).sum();
    let insufficient = |needed_zat| BurnTxError::InsufficientFunds {
        input_zat,
        needed_zat,
        burn_zat: req.amount_zat,
        fee_zat: req.fee_zat,
    };
    let needed_zat = req
        .amount_zat
        .checked_add(req.fee_zat)
        .ok_or_else(|| insufficient(u64::MAX))?;
    let change_zat = input_zat
        .checked_sub(needed_zat)
        .ok_or_else(|| insufficient(needed_zat))?;

    let public_key = req.change_and_signing_key.public_key();
    let our_address = req.change_and_signing_key.transparent_address();
    let our_coin_script: zcash_transparent::address::Script = our_address.script().into();

    let mut builder = TransparentBuilder::empty();
    for utxo in &req.utxos {
        let coin = TxOut::new(Zatoshis::from_u64(utxo.value_zat)?, our_coin_script.clone());
        builder.add_p2pkh_input(public_key, utxo.outpoint.clone(), coin)?;
    }
    builder.add_output(&req.recipient, Zatoshis::from_u64(req.amount_zat)?)?;
    if change_zat > 0 {
        builder.add_output(&our_address, Zatoshis::from_u64(change_zat)?)?;
    }

    let (txid, raw) = sign_and_serialize(
        req.network,
        req.target_height,
        builder,
        &req.change_and_signing_key,
    )?;
    Ok(BuiltTransferTx {
        txid,
        raw,
        change_zat,
    })
}

#[cfg(test)]
// Test code: an unexpected `Err`/`None` here is a test failure.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use consensus::sip1::{TxOutRef, extract_burn};

    fn request(utxo_value_zat: u64, burn_zat: u64, fee_zat: u64) -> BurnTxRequest {
        BurnTxRequest {
            network: Network::Regtest,
            target_height: 101,
            utxos: vec![Utxo {
                outpoint: OutPoint::new([7u8; 32], 0),
                value_zat: utxo_value_zat,
            }],
            change_and_signing_key: Keypair::generate(),
            evm_address: [0x42; 20],
            signal_bits: 0xdead_beef,
            burn_value_zat: burn_zat,
            fee_zat,
        }
    }

    #[test]
    fn builds_and_parses_as_a_sip1_burn() {
        let req = request(1_000_000, 100_000, 1_000);
        let built = build_burn_transaction(&req).unwrap();
        assert_eq!(built.change_zat, 1_000_000 - 100_000 - 1_000);

        // Re-parse with the real V5 transaction reader, using the same
        // branch id we signed for, and feed the decoded outputs through
        // `consensus::sip1::extract_burn` -- the exact check the on-chain
        // proof (the `e2e_regtest_burn` integration test) also performs
        // against outputs fetched back from Zebra.
        let branch_id =
            BranchId::for_height(&req.network, BlockHeight::from_u32(req.target_height));
        let tx = transaction::Transaction::read(built.raw.as_slice(), branch_id).unwrap();
        let bundle = tx.transparent_bundle().unwrap();
        let refs: Vec<TxOutRef<'_>> = bundle
            .vout
            .iter()
            .map(|o| TxOutRef {
                value_zat: o.value().into_u64(),
                script: o.script_pubkey().0.0.as_slice(),
            })
            .collect();
        let burn = extract_burn(refs).unwrap();
        assert_eq!(burn.evm_address, req.evm_address);
        assert_eq!(burn.signal_bits, req.signal_bits);
        assert_eq!(burn.value_zat, req.burn_value_zat);
    }

    #[test]
    fn rejects_burn_below_minimum() {
        let req = request(1_000_000, MIN_BURN_ZAT - 1, 1_000);
        assert!(matches!(
            build_burn_transaction(&req),
            Err(BurnTxError::BelowMinBurn { .. })
        ));
    }

    #[test]
    fn rejects_insufficient_funds() {
        let req = request(1_000, 100_000, 1_000);
        assert!(matches!(
            build_burn_transaction(&req),
            Err(BurnTxError::InsufficientFunds { .. })
        ));
    }

    #[test]
    fn omits_change_output_when_exact() {
        let req = request(101_000, 100_000, 1_000);
        let built = build_burn_transaction(&req).unwrap();
        assert_eq!(built.change_zat, 0);

        let branch_id =
            BranchId::for_height(&req.network, BlockHeight::from_u32(req.target_height));
        let tx = transaction::Transaction::read(built.raw.as_slice(), branch_id).unwrap();
        let bundle = tx.transparent_bundle().unwrap();
        // Exactly two outputs: SIP-1 payload + eater. No change output.
        assert_eq!(bundle.vout.len(), 2);
    }

    fn transfer(utxo_value_zat: u64, amount_zat: u64, fee_zat: u64) -> TransferTxRequest {
        TransferTxRequest {
            network: Network::Regtest,
            target_height: 101,
            utxos: vec![Utxo {
                outpoint: OutPoint::new([9u8; 32], 1),
                value_zat: utxo_value_zat,
            }],
            change_and_signing_key: Keypair::generate(),
            recipient: Keypair::generate().transparent_address(),
            amount_zat,
            fee_zat,
        }
    }

    #[test]
    fn transfer_pays_recipient_then_change() {
        let req = transfer(1_000_000, 300_000, 10_000);
        let built = build_transfer_transaction(&req).unwrap();
        assert_eq!(built.change_zat, 690_000);

        let branch_id =
            BranchId::for_height(&req.network, BlockHeight::from_u32(req.target_height));
        let tx = transaction::Transaction::read(built.raw.as_slice(), branch_id).unwrap();
        assert_eq!(<[u8; 32]>::from(tx.txid()), built.txid);
        let bundle = tx.transparent_bundle().unwrap();
        assert_eq!(bundle.vin.len(), 1);
        assert_eq!(bundle.vout.len(), 2);
        assert_eq!(bundle.vout[0].value().into_u64(), 300_000);
        assert_eq!(bundle.vout[0].recipient_address(), Some(req.recipient));
        assert_eq!(bundle.vout[1].value().into_u64(), 690_000);
        assert_eq!(
            bundle.vout[1].recipient_address(),
            Some(req.change_and_signing_key.transparent_address())
        );
    }

    #[test]
    fn transfer_without_change_has_one_output() {
        let req = transfer(310_000, 300_000, 10_000);
        let built = build_transfer_transaction(&req).unwrap();
        assert_eq!(built.change_zat, 0);
        let branch_id =
            BranchId::for_height(&req.network, BlockHeight::from_u32(req.target_height));
        let tx = transaction::Transaction::read(built.raw.as_slice(), branch_id).unwrap();
        assert_eq!(tx.transparent_bundle().unwrap().vout.len(), 1);
    }

    #[test]
    fn transfer_rejects_zero_and_underfunded() {
        assert!(matches!(
            build_transfer_transaction(&transfer(1_000_000, 0, 10_000)),
            Err(BurnTxError::ZeroAmount)
        ));
        assert!(matches!(
            build_transfer_transaction(&transfer(300_000, 300_000, 10_000)),
            Err(BurnTxError::InsufficientFunds { .. })
        ));
    }
}
