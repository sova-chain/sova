//! secp256k1 keypairs, P2PKH address derivation, and a minimal file
//! keystore.

use std::fs;
use std::path::Path;

use secp256k1::{PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::Parameters;
use zcash_transparent::address::TransparentAddress;

use crate::network::Network;

/// Errors from key parsing and keystore I/O.
#[derive(Debug, thiserror::Error)]
pub enum KeystoreError {
    /// The underlying file could not be read or written.
    #[error("keystore I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The stored bytes are not a valid secp256k1 scalar.
    #[error("invalid secp256k1 secret key: {0}")]
    InvalidSecretKey(secp256k1::Error),
    /// The `secret_key_hex` field was not valid hex.
    #[error("invalid hex in keystore file: {0}")]
    Hex(#[from] hex::FromHexError),
    /// The keystore file was not well-formed JSON in the expected shape.
    #[error("malformed keystore JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The decoded secret key was not exactly 32 bytes.
    #[error("secret key must be exactly 32 bytes, got {0}")]
    WrongLength(usize),
}

/// A secp256k1 keypair for a single transparent Zcash address.
#[derive(Clone, Copy)]
pub struct Keypair {
    secret_key: SecretKey,
    public_key: PublicKey,
}

impl std::fmt::Debug for Keypair {
    /// Redacts the secret key -- only the (non-secret) public key is
    /// printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keypair")
            .field("public_key", &self.public_key)
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

impl Keypair {
    /// Generates a fresh keypair using the operating system's CSPRNG.
    #[must_use]
    pub fn generate() -> Self {
        let secp = Secp256k1::new();
        let secret_key = SecretKey::new(&mut rand::thread_rng());
        let public_key = PublicKey::from_secret_key(&secp, &secret_key);
        Self {
            secret_key,
            public_key,
        }
    }

    /// Reconstructs a keypair from a raw 32-byte secp256k1 scalar.
    ///
    /// # Errors
    ///
    /// Returns [`KeystoreError::InvalidSecretKey`] if `bytes` is not a
    /// valid secp256k1 secret key (zero, or greater than the curve order).
    pub fn from_secret_bytes(bytes: [u8; 32]) -> Result<Self, KeystoreError> {
        let secret_key = SecretKey::from_slice(&bytes).map_err(KeystoreError::InvalidSecretKey)?;
        let secp = Secp256k1::new();
        let public_key = PublicKey::from_secret_key(&secp, &secret_key);
        Ok(Self {
            secret_key,
            public_key,
        })
    }

    /// Returns the secret key, for signing.
    #[must_use]
    pub fn secret_key(&self) -> SecretKey {
        self.secret_key
    }

    /// Returns the public key.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        self.public_key
    }

    /// Returns the raw 32-byte secret scalar.
    #[must_use]
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret_key.secret_bytes()
    }

    /// Derives this key's P2PKH transparent address (network-agnostic:
    /// the hash160 of the compressed public key).
    #[must_use]
    pub fn transparent_address(&self) -> TransparentAddress {
        TransparentAddress::from_pubkey(&self.public_key)
    }

    /// Encodes this key's P2PKH address as a string for the given network
    /// (base58check with the appropriate mainnet/testnet/regtest prefix --
    /// regtest shares testnet's transparent-address prefix, per
    /// `zcash_protocol`).
    #[must_use]
    pub fn encode_address(&self, network: Network) -> String {
        self.transparent_address()
            .to_zcash_address(network.network_type())
            .encode()
    }

    /// Loads a keypair from a JSON keystore file written by
    /// [`Self::save_to_file`].
    ///
    /// # Errors
    ///
    /// Returns [`KeystoreError`] if the file cannot be read, is not valid
    /// JSON in the expected shape, or does not contain a valid secret key.
    pub fn load_from_file(path: &Path) -> Result<Self, KeystoreError> {
        let contents = fs::read_to_string(path)?;
        let file: KeystoreFile = serde_json::from_str(&contents)?;
        let bytes = hex::decode(&file.secret_key_hex)?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|v: Vec<u8>| KeystoreError::WrongLength(v.len()))?;
        Self::from_secret_bytes(bytes)
    }

    /// Writes this keypair to a JSON keystore file.
    ///
    /// The secret key is stored as **plaintext hex** -- the only
    /// protection is the file's Unix permission bits, set to `0600`
    /// (owner read/write only) after creation. There is no passphrase or
    /// at-rest encryption.
    ///
    /// TODO(pre-mainnet): encrypt the secret key at rest (e.g. an
    /// scrypt/argon2-derived key wrapping AES-GCM or XChaCha20-Poly1305,
    /// as zcashd/zebra-adjacent wallets typically do) before this keystore
    /// is used with anything but disposable regtest/testnet keys.
    ///
    /// # Errors
    ///
    /// Returns [`KeystoreError::Io`] if the file cannot be created,
    /// written, or have its permissions set.
    pub fn save_to_file(&self, path: &Path) -> Result<(), KeystoreError> {
        let file = KeystoreFile {
            version: 1,
            secret_key_hex: hex::encode(self.secret_bytes()),
        };
        let json = serde_json::to_string_pretty(&file)?;
        fs::write(path, json)?;
        restrict_permissions(path)?;
        Ok(())
    }
}

/// On-disk shape of a keystore file.
#[derive(Serialize, Deserialize)]
struct KeystoreFile {
    /// Keystore format version, for forward compatibility.
    version: u8,
    /// The raw 32-byte secp256k1 secret key, hex-encoded, **unencrypted**.
    secret_key_hex: String,
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
    // No portable equivalent of Unix mode bits; this keystore is a v0
    // regtest/testnet tool, and non-Unix support can be added (e.g. via
    // Windows ACLs) when this crate targets those platforms for real use.
    Ok(())
}

#[cfg(test)]
// Test code: an unexpected `Err`/`None` here is a test failure, and a panic
// via `.unwrap()` reports that far more usefully than threading `Result`
// through every assertion would.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn generate_then_from_bytes_roundtrips() {
        let kp = Keypair::generate();
        let kp2 = Keypair::from_secret_bytes(kp.secret_bytes()).unwrap();
        assert_eq!(kp.public_key(), kp2.public_key());
    }

    #[test]
    fn regtest_address_has_t_prefix() {
        let kp = Keypair::generate();
        let addr = kp.encode_address(Network::Regtest);
        // Regtest shares testnet's transparent P2PKH prefix ("tm...").
        assert!(addr.starts_with("tm"), "unexpected address: {addr}");
    }

    #[test]
    fn save_and_load_keystore_roundtrips_and_sets_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keystore.json");
        let kp = Keypair::generate();
        kp.save_to_file(&path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let loaded = Keypair::load_from_file(&path).unwrap();
        assert_eq!(kp.public_key(), loaded.public_key());
        assert_eq!(kp.secret_bytes(), loaded.secret_bytes());
    }
}
