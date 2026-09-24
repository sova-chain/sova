//! Transparent-only Zcash burn transaction builder.
//!
//! This crate builds and signs Zcash v5 transactions that burn ZEC to the
//! SIP-1 eater script (see [`consensus::sip1`]), using transparent
//! addresses only -- no shielded pools are touched anywhere in this crate.
//!
//! Layout:
//! - [`keys`] -- secp256k1 keypairs, P2PKH address derivation, and a
//!   minimal on-disk JSON keystore.
//! - [`network`] -- the [`Parameters`](zcash_protocol::consensus::Parameters)
//!   implementation used for transaction building, including a regtest
//!   configuration matching `box/regtest`'s `zebrad.toml`.
//! - [`tx`] -- construction and ZIP-244 signing of SIP-1 burn transactions
//!   (and SIP-8 anchored ones, which also reference a Sova block), and of
//!   plain transparent transfers (used by `sova-faucet`).
//! - [`fee`] -- the ZIP-317 conventional fee for transparent transactions,
//!   including both burn payload sizes.
//! - [`rpc`] -- a minimal JSON-RPC client for a `zebrad`-compatible node.
//! - [`utxo`] -- coinbase UTXO discovery over that RPC client.

pub mod fee;
pub mod keys;
pub mod network;
pub mod rpc;
pub mod tx;
pub mod utxo;

pub use keys::{Keypair, KeystoreError};
pub use network::Network;
pub use rpc::{RpcClient, RpcError};
pub use tx::{BuiltBurnTx, BuiltTransferTx, BurnTxError, BurnTxRequest, TransferTxRequest, Utxo};
