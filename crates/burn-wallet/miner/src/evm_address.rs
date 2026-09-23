//! Deriving the Sova EVM address a miner's burns credit.
//!
//! Every SIP-1 burn payload commits to a 20-byte EVM address
//! ([`consensus::sip1::BurnPayload::evm_address`]); Sova mints the miner's
//! SOVA to it. Absent an explicit `--evm-address` at `init`, the default is
//! the **real Ethereum address of the keystore's own secp256k1 key**:
//! `keccak256(uncompressed_pubkey[1..])[12..]` -- the standard derivation
//! every EVM wallet uses. The same 32-byte secret therefore controls both
//! the Zcash t-addr that funds the burns and the EVM account the SOVA lands
//! in, and `sova-miner export-evm-key` hands it out for import into any EVM
//! wallet (MetaMask etc.) to spend that SOVA.
//!
//! # The legacy default (unspendable)
//!
//! Before this, the default was the t-addr's hash160
//! (`RIPEMD160(SHA256(compressed_pubkey))`) reinterpreted as an EVM
//! address -- see [`legacy_evm_address`]. No private key corresponds to
//! that EVM address, so **every SOVA credited to it is unspendable**.
//! Keystores initialized that way still have it recorded in `state.json`;
//! `init` and `mine` detect it ([`CreditTarget::LegacyUnspendable`]) and
//! warn, and `init --migrate-evm-address` switches such a miner to the
//! spendable default. It is never changed silently: where burns credit is
//! consensus-visible, and the node (`SOVA_MINER_EVM_ADDRESS`) must be told
//! about a change too.

use burn_wallet::Keypair;
use sha3::{Digest, Keccak256};
use zcash_transparent::address::TransparentAddress;

/// Derives the default EVM address for `keypair`: the Ethereum address of
/// the same secp256k1 key, `keccak256(uncompressed_pubkey[1..])[12..]`
/// (see module docs). Importing the keystore's secret key into an EVM
/// wallet yields exactly this address.
#[must_use]
pub(crate) fn derive_evm_address(keypair: &Keypair) -> [u8; 20] {
    let uncompressed = keypair.public_key().serialize_uncompressed();
    // Byte 0 is the SEC1 0x04 tag; Ethereum hashes only the 64-byte X||Y.
    let hash = Keccak256::digest(&uncompressed[1..]);
    let mut out = [0u8; 20];
    out.copy_from_slice(&hash[12..]);
    out
}

/// The pre-fix default: the t-addr's hash160 reinterpreted as an EVM
/// address. Nobody holds a key for it -- kept only to *detect* keystores
/// initialized with it (see module docs). Never use it as a credit target.
#[must_use]
pub(crate) fn legacy_evm_address(keypair: &Keypair) -> [u8; 20] {
    match keypair.transparent_address() {
        TransparentAddress::PublicKeyHash(hash) | TransparentAddress::ScriptHash(hash) => hash,
    }
}

/// What a miner's recorded credit address is, relative to its own key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CreditTarget {
    /// The keystore key's own Ethereum address: spendable after
    /// `export-evm-key`.
    OwnKey,
    /// The legacy hash160 default: SOVA credited here is unspendable.
    LegacyUnspendable,
    /// Some other address (set with `--evm-address`); spendable only by
    /// whoever holds that address's key.
    External,
}

/// Classifies `evm_address` against `keypair` (see [`CreditTarget`]).
#[must_use]
pub(crate) fn classify(keypair: &Keypair, evm_address: [u8; 20]) -> CreditTarget {
    if evm_address == derive_evm_address(keypair) {
        CreditTarget::OwnKey
    } else if evm_address == legacy_evm_address(keypair) {
        CreditTarget::LegacyUnspendable
    } else {
        CreditTarget::External
    }
}

/// The warning printed (to stderr) by `init` and `mine` for a miner whose
/// burns credit the legacy hash160 address. Deliberately never contains
/// the lowercase phrase the box scripts grep `init` output for.
#[must_use]
pub(crate) fn legacy_warning(keypair: &Keypair) -> String {
    let legacy = hex::encode(legacy_evm_address(keypair));
    let own = hex::encode(derive_evm_address(keypair));
    format!(
        "WARNING: this miner's burns credit 0x{legacy}, the LEGACY default EVM address \
         (the t-addr's hash160). No private key exists for it: all SOVA credited there \
         is UNSPENDABLE, and so is anything mined to it from now on.\n\
         WARNING: to fix, run `sova-miner init --migrate-evm-address` (switches to \
         0x{own}, this keystore key's own EVM address; spend with \
         `sova-miner export-evm-key`), or `sova-miner init --evm-address <your address>`. \
         Then restart your Sova node with the new SOVA_MINER_EVM_ADDRESS. SOVA already \
         credited to the legacy address stays there."
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn key(last_byte: u8) -> Keypair {
        let mut secret = [0u8; 32];
        secret[31] = last_byte;
        Keypair::from_secret_bytes(secret).unwrap()
    }

    /// Guards against accidentally using NIST SHA3-256 (different padding)
    /// instead of Ethereum's Keccak-256.
    #[test]
    fn hash_is_keccak256_not_sha3() {
        assert_eq!(
            hex::encode(Keccak256::digest(b"")),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }

    /// Standard published vectors: the Ethereum addresses of secp256k1
    /// private keys 1 and 2.
    #[test]
    fn known_ethereum_address_vectors() {
        assert_eq!(
            hex::encode(derive_evm_address(&key(1))),
            "7e5f4552091a69125d5dfcb7b8c2659029395bdf"
        );
        assert_eq!(
            hex::encode(derive_evm_address(&key(2))),
            "2b5ad5c4795c026514f8317c7a215e218dccd6cf"
        );
    }

    /// The legacy default for private key 1 is the well-known hash160 of
    /// its compressed pubkey (Bitcoin's 1BgGZ9tcN4rm9KBzDn7KprQz87SZ26SAMH).
    #[test]
    fn legacy_address_is_the_taddr_hash160() {
        assert_eq!(
            hex::encode(legacy_evm_address(&key(1))),
            "751e76e8199196d454941c45d1b3a323f1433bd6"
        );
    }

    #[test]
    fn classify_detects_legacy_own_and_external() {
        let kp = Keypair::generate();
        assert_eq!(classify(&kp, derive_evm_address(&kp)), CreditTarget::OwnKey);
        assert_eq!(
            classify(&kp, legacy_evm_address(&kp)),
            CreditTarget::LegacyUnspendable
        );
        assert_eq!(classify(&kp, [0xAB; 20]), CreditTarget::External);
        // Another key's legacy address is just "external" for this one.
        let other = Keypair::generate();
        assert_eq!(
            classify(&kp, legacy_evm_address(&other)),
            CreditTarget::External
        );
    }

    #[test]
    fn legacy_warning_names_both_addresses_and_the_fix() {
        let w = legacy_warning(&key(1));
        assert!(
            w.contains("0x751e76e8199196d454941c45d1b3a323f1433bd6"),
            "{w}"
        );
        assert!(
            w.contains("0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"),
            "{w}"
        );
        assert!(w.contains("UNSPENDABLE"), "{w}");
        assert!(w.contains("--migrate-evm-address"), "{w}");
        // box/up.sh and box/sim/*.sh parse `init` output (stdout+stderr)
        // with awk '/evm address/'; a warning line must not match it.
        assert!(!w.contains("evm address"), "{w}");
    }

    #[test]
    fn is_deterministic_per_keypair_and_differs_between_keypairs() {
        let kp = Keypair::generate();
        assert_eq!(derive_evm_address(&kp), derive_evm_address(&kp));
        assert_ne!(
            derive_evm_address(&kp),
            derive_evm_address(&Keypair::generate()),
            "two freshly generated keypairs collided (1 in 2^160)"
        );
    }
}
