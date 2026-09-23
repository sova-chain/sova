//! SIP-6 sealer signatures: the seal format, what is signed, signing and
//! recovery (`sips/sip-6-draft-sealer-signatures.md` §2.1–2.2).
//!
//! A sealed block's `extra_data` is exactly [`SEALED_EXTRA_LEN`] bytes:
//! a 32-byte vanity (signed), then `r ‖ s ‖ v` (v ∈ {0, 1}). A null block's
//! `extra_data` is empty. The signature covers the header with its
//! `extra_data` cut to the vanity, under a domain tag and the chain ID:
//!
//! ```text
//! seal_hash   = keccak256(rlp(header with extra_data := vanity))
//! seal_digest = keccak256(0x19 ‖ "SovaSeal/v1" ‖ chain_id (u64 BE) ‖ seal_hash)
//! ```
//!
//! Only canonical signatures recover: `r, s ∈ [1, n−1]`, `s ≤ n/2`,
//! `v ∈ {0, 1}`. The high-s check is explicit here, not left to a library
//! default: flipping `s` on an honest seal would otherwise give a second
//! valid block with a different hash (SIP-6 §2.2).

use std::sync::OnceLock;

use alloy_primitives::{Address, B256, Bytes, Signature, U256, b256, keccak256};
use reth_ethereum::primitives::Header;
use reth_ethereum::primitives::crypto::secp256k1;

/// Length of the signed vanity at the start of a sealed `extra_data`.
pub const VANITY_LEN: usize = 32;

/// Length of `r ‖ s ‖ v`.
pub const SIGNATURE_LEN: usize = 65;

/// Exact `extra_data` length of a sealed block.
pub const SEALED_EXTRA_LEN: usize = VANITY_LEN + SIGNATURE_LEN;

/// The seal's domain tag, after the leading `0x19` (SIP-6 §2.2).
pub const DOMAIN: &[u8] = b"SovaSeal/v1";

/// secp256k1 group order `n`.
const SECP256K1N: U256 = U256::from_be_bytes([
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE,
    0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36, 0x41, 0x41,
]);

/// The empty-trie root: a null block's transactions and withdrawals roots.
pub const EMPTY_ROOT: B256 =
    b256!("56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421");

/// Longest `extra_data` before SIP-6 activates (Ethereum's limit).
pub const PRE_SIP6_MAX_EXTRA: usize = 32;

static ACTIVE: OnceLock<u64> = OnceLock::new();

/// Switch SIP-6 on for this process, signing and checking seals under
/// `chain_id`. bin/sova calls it at startup on chains that seal; until then
/// the pre-SIP-6 header rule (at most 32 bytes of `extra_data`) applies.
/// Only the first call takes effect.
pub fn activate(chain_id: u64) -> bool {
    ACTIVE.set(chain_id).is_ok()
}

/// The chain ID seals are checked under, when SIP-6 is active.
#[must_use]
pub fn active_chain_id() -> Option<u64> {
    ACTIVE.get().copied()
}

/// Why a header's seal does not verify.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SealError {
    /// `extra_data` is neither empty (null block) nor exactly 97 bytes.
    #[error("sova-seal: extra_data is {0} bytes; want 0 (null block) or 97 (sealed)")]
    Length(usize),
    /// `r` or `s` is zero or not below the group order.
    #[error("sova-seal: signature r or s out of range")]
    OutOfRange,
    /// `s` is above `n/2` (the malleable twin of a canonical signature).
    #[error("sova-seal: high-s signature")]
    HighS,
    /// The recovery byte is not 0 or 1.
    #[error("sova-seal: recovery id {0} (want 0 or 1)")]
    RecoveryId(u8),
    /// No public key recovers from the signature.
    #[error("sova-seal: signature does not recover")]
    Unrecoverable,
    /// Signing failed (bad secret key).
    #[error("sova-seal: cannot sign: {0}")]
    Sign(String),
    /// Before SIP-6: more than 32 bytes of `extra_data`.
    #[error("sova-seal: extra_data is {0} bytes; at most 32 before SIP-6")]
    PreSip6Length(usize),
    /// A null block (empty `extra_data`) with a field it must not set.
    #[error("sova-seal: null block has {0}")]
    NullNotEmpty(&'static str),
    /// `prev_randao` is not `keccak256(parent_beacon_block_root)`.
    #[error("sova-seal: prev_randao is not keccak256 of the zcash anchor")]
    Randao,
    /// The timestamp is outside SIP-6's window for its parent and epoch.
    #[error(
        "sova-seal: timestamp {ts} outside {min}..={max} (parent {parent}, zcash time {zcash})"
    )]
    Timestamp {
        /// The block's timestamp.
        ts: u64,
        /// Earliest allowed (for a null block: the only allowed value).
        min: u64,
        /// Latest allowed.
        max: u64,
        /// The parent's timestamp.
        parent: u64,
        /// The epoch's Zcash block time.
        zcash: u64,
    },
    /// A null block whose gas limit differs from its parent's.
    #[error("sova-seal: null block gas limit {0} differs from its parent's {1}")]
    NullGasLimit(u64, u64),
}

/// The rule against the parent (SIP-6 §2.5, `validate_header_against_parent`):
/// a null block's timestamp is exactly `max(parent + 1, zcash_time)` and its
/// gas limit its parent's; a sealed block's timestamp is after its parent's
/// and at most `max(parent + 1, zcash_time + MAX_SEAL_DRIFT)`.
///
/// # Errors
/// [`SealError::Timestamp`] or [`SealError::NullGasLimit`].
pub fn check_against_parent(
    header: &Header,
    parent: &Header,
    zcash_time: u64,
) -> Result<(), SealError> {
    let (min, max) = timestamp_window(parent.timestamp, zcash_time);
    let ts = header.timestamp;
    let bad = || SealError::Timestamp {
        ts,
        min,
        max,
        parent: parent.timestamp,
        zcash: zcash_time,
    };
    if header.extra_data.is_empty() {
        if ts != min {
            return Err(bad());
        }
        if header.gas_limit != parent.gas_limit {
            return Err(SealError::NullGasLimit(header.gas_limit, parent.gas_limit));
        }
    } else if ts <= parent.timestamp || ts > max {
        return Err(bad());
    }
    Ok(())
}

/// The stateless header rule (SIP-6 §2.5, `validate_header`): before
/// activation (`chain_id` = `None`), at most 32 bytes of `extra_data`;
/// after, a null block (empty `extra_data`, zero beneficiary, no gas, no
/// transactions, no withdrawals) or a canonical seal that recovers.
/// Genesis is exempt. Returns the signer of a sealed header.
///
/// # Errors
/// The first rule the header breaks.
pub fn check_header(header: &Header, chain_id: Option<u64>) -> Result<Option<Address>, SealError> {
    if header.number == 0 {
        return Ok(None);
    }
    let Some(chain_id) = chain_id else {
        let n = header.extra_data.len();
        return if n > PRE_SIP6_MAX_EXTRA {
            Err(SealError::PreSip6Length(n))
        } else {
            Ok(None)
        };
    };
    let kind = kind(header)?;
    if header.mix_hash != pinned_randao(header.parent_beacon_block_root.unwrap_or_default()) {
        return Err(SealError::Randao);
    }
    match kind {
        SealKind::Sealed => recover(header, chain_id).map(Some),
        SealKind::Null => {
            if header.beneficiary != Address::ZERO {
                return Err(SealError::NullNotEmpty("a beneficiary"));
            }
            if header.gas_used != 0 {
                return Err(SealError::NullNotEmpty("gas used"));
            }
            if header.transactions_root != EMPTY_ROOT {
                return Err(SealError::NullNotEmpty("transactions"));
            }
            if header.withdrawals_root.is_some_and(|r| r != EMPTY_ROOT) {
                return Err(SealError::NullNotEmpty("withdrawals"));
            }
            Ok(None)
        }
    }
}

/// What a header's `extra_data` says the block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealKind {
    /// Empty `extra_data`: a null block (SIP-6 §2.4).
    Null,
    /// 97 bytes: vanity and a signature (not yet verified).
    Sealed,
}

/// How far past its Zcash block's time a sealed block's timestamp may run
/// (SIP-6 §2.8, draft).
pub const MAX_SEAL_DRIFT: u64 = 900;

/// SIP-6 §2.8: every block's `prev_randao` is `keccak256(anchor)` — a value
/// only Zcash miners can bias, not the Sova producer.
#[must_use]
pub fn pinned_randao(anchor: B256) -> B256 {
    keccak256(anchor)
}

/// SIP-6 §2.4/§2.8: the timestamp window after `parent_ts` for a block
/// settling a Zcash block with time `zcash_time`: `(min, max)` where `min`
/// is also the null block's exact timestamp.
#[must_use]
pub fn timestamp_window(parent_ts: u64, zcash_time: u64) -> (u64, u64) {
    let floor = parent_ts.saturating_add(1);
    (
        floor.max(zcash_time),
        floor.max(zcash_time.saturating_add(MAX_SEAL_DRIFT)),
    )
}

/// Who a block says produced it, for the settlement check (SIP-6 §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sealer {
    /// SIP-6 is not active: rank is read from the withdrawals (the tip).
    Unsealed,
    /// A null block: nobody signed it, and it mints nothing.
    Null,
    /// Sealed by this address (recovered from the signature).
    Signed(Address),
}

/// The block's [`Sealer`] under activation `chain_id` (`None` = SIP-6 off).
///
/// # Errors
/// The header's seal does not verify ([`check_header`]'s rules).
pub fn sealer(header: &Header, chain_id: Option<u64>) -> Result<Sealer, SealError> {
    if chain_id.is_none() {
        return Ok(Sealer::Unsealed);
    }
    match check_header(header, chain_id)? {
        Some(signer) => Ok(Sealer::Signed(signer)),
        None => Ok(Sealer::Null),
    }
}

/// Classify a header by its `extra_data` length alone.
///
/// # Errors
/// [`SealError::Length`] for any length other than 0 or 97.
pub fn kind(header: &Header) -> Result<SealKind, SealError> {
    match header.extra_data.len() {
        0 => Ok(SealKind::Null),
        SEALED_EXTRA_LEN => Ok(SealKind::Sealed),
        n => Err(SealError::Length(n)),
    }
}

/// `vanity` zero-padded (or cut) to exactly [`VANITY_LEN`] bytes.
#[must_use]
pub fn vanity(raw: &[u8]) -> [u8; VANITY_LEN] {
    let mut out = [0u8; VANITY_LEN];
    let n = raw.len().min(VANITY_LEN);
    out[..n].copy_from_slice(&raw[..n]);
    out
}

/// The hash the seal signs over: the header's hash with `extra_data` cut
/// to its vanity. For a header being sealed, `extra_data` is the vanity
/// (up to 32 bytes, zero-padded); for a sealed header, its first 32 bytes.
#[must_use]
pub fn seal_hash(header: &Header) -> B256 {
    let mut unsealed = header.clone();
    unsealed.extra_data = Bytes::copy_from_slice(&vanity(&header.extra_data));
    unsealed.hash_slow()
}

/// `keccak256(0x19 ‖ "SovaSeal/v1" ‖ chain_id ‖ seal_hash)` (SIP-6 §2.2).
#[must_use]
pub fn seal_digest(seal_hash: B256, chain_id: u64) -> B256 {
    let mut buf = Vec::with_capacity(1 + DOMAIN.len() + 8 + 32);
    buf.push(0x19);
    buf.extend_from_slice(DOMAIN);
    buf.extend_from_slice(&chain_id.to_be_bytes());
    buf.extend_from_slice(seal_hash.as_slice());
    keccak256(buf)
}

/// Seal `header` in place: set its `extra_data` to the padded vanity plus a
/// signature by `secret` over [`seal_digest`]. The caller reseals the
/// block (its hash changes).
///
/// # Errors
/// [`SealError::Sign`] if `secret` is not a valid secp256k1 key.
pub fn sign(
    header: &mut Header,
    vanity_bytes: &[u8],
    secret: B256,
    chain_id: u64,
) -> Result<(), SealError> {
    let vanity = vanity(vanity_bytes);
    header.extra_data = Bytes::copy_from_slice(&vanity);
    let digest = seal_digest(seal_hash(header), chain_id);
    let sig =
        secp256k1::sign_message(secret, digest).map_err(|e| SealError::Sign(e.to_string()))?;
    // libsecp256k1 signs low-s; normalize anyway so a backend change can
    // never emit the malleable twin.
    let sig = sig.normalize_s().unwrap_or(sig);
    let mut extra = Vec::with_capacity(SEALED_EXTRA_LEN);
    extra.extend_from_slice(&vanity);
    extra.extend_from_slice(&sig.r().to_be_bytes::<32>());
    extra.extend_from_slice(&sig.s().to_be_bytes::<32>());
    extra.push(u8::from(sig.v()));
    header.extra_data = Bytes::from(extra);
    Ok(())
}

/// The canonical signature carried in a sealed header's `extra_data`.
///
/// # Errors
/// The length, range, high-s and recovery-id rules of SIP-6 §2.2.
pub fn signature(header: &Header) -> Result<Signature, SealError> {
    let extra = &header.extra_data;
    if extra.len() != SEALED_EXTRA_LEN {
        return Err(SealError::Length(extra.len()));
    }
    let r = U256::from_be_slice(&extra[VANITY_LEN..VANITY_LEN + 32]);
    let s = U256::from_be_slice(&extra[VANITY_LEN + 32..VANITY_LEN + 64]);
    let v = extra[SEALED_EXTRA_LEN - 1];
    if r.is_zero() || s.is_zero() || r >= SECP256K1N || s >= SECP256K1N {
        return Err(SealError::OutOfRange);
    }
    if s > secp256k1_half() {
        return Err(SealError::HighS);
    }
    let parity = match v {
        0 => false,
        1 => true,
        other => return Err(SealError::RecoveryId(other)),
    };
    Ok(Signature::new(r, s, parity))
}

/// The address that sealed `header` on chain `chain_id`.
///
/// # Errors
/// Any [`SealError`] from [`signature`], or [`SealError::Unrecoverable`].
pub fn recover(header: &Header, chain_id: u64) -> Result<Address, SealError> {
    let sig = signature(header)?;
    let digest = seal_digest(seal_hash(header), chain_id);
    secp256k1::recover_signer(&sig, digest).map_err(|_| SealError::Unrecoverable)
}

/// The EVM address of a secret key.
///
/// # Errors
/// [`SealError::Sign`] if `secret` is not a valid secp256k1 key.
pub fn address_of(secret: B256) -> Result<Address, SealError> {
    // Recover from a throwaway signature: the one public-key-to-address
    // path shared with verification.
    let digest = keccak256(b"sova-seal address probe");
    let sig =
        secp256k1::sign_message(secret, digest).map_err(|e| SealError::Sign(e.to_string()))?;
    secp256k1::recover_signer_unchecked(&sig, digest).map_err(|_| SealError::Unrecoverable)
}

fn secp256k1_half() -> U256 {
    SECP256K1N >> 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const TESTNET: u64 = 82_330;
    const MAINNET: u64 = 8_233;

    fn key() -> B256 {
        B256::repeat_byte(0x42)
    }

    fn header() -> Header {
        Header {
            number: 7,
            parent_hash: B256::repeat_byte(0x11),
            timestamp: 1_790_000_000,
            gas_limit: 30_000_000,
            parent_beacon_block_root: Some(B256::repeat_byte(0x22)),
            extra_data: Bytes::from_static(b"sova"),
            ..Header::default()
        }
    }

    fn sealed(chain_id: u64) -> Header {
        let mut h = header();
        sign(&mut h, b"sova", key(), chain_id).unwrap_or_else(|e| panic!("{e}"));
        h
    }

    #[test]
    fn a_seal_recovers_its_signer_and_fills_97_bytes() {
        let h = sealed(TESTNET);
        assert_eq!(h.extra_data.len(), SEALED_EXTRA_LEN);
        assert_eq!(&h.extra_data[..4], b"sova");
        assert_eq!(kind(&h), Ok(SealKind::Sealed));
        assert_eq!(recover(&h, TESTNET), address_of(key()));
    }

    /// SIP-6 §8 test vectors: fixed key (`0x42` × 32), fixed header, both
    /// chain IDs. Cross-checked with Foundry `cast` (address, both digests,
    /// and `cast wallet sign --no-hash` gives the same r ‖ s, v = 27 ↔ 0).
    #[test]
    fn test_vectors_are_stable() {
        let h = sealed(TESTNET);
        assert_eq!(
            address_of(key()).ok(),
            Some(alloy_primitives::address!(
                "17c5185167401ed00cf5f5b2fc97d9bbfdb7d025"
            ))
        );
        let sh = seal_hash(&h);
        assert_eq!(
            sh,
            header_with_vanity().hash_slow(),
            "hash of the vanity-only header"
        );
        assert_eq!(
            sh,
            alloy_primitives::b256!(
                "c1673e622d63d6144d0961936fe816fd22b532a2bbabc927eb835788020c3eb4"
            )
        );
        assert_eq!(
            seal_digest(sh, TESTNET),
            alloy_primitives::b256!(
                "01989a2f734736bc3ff3ace9022168148a736684feb489a63e7436f5b8868ecb"
            )
        );
        assert_eq!(
            seal_digest(sh, MAINNET),
            alloy_primitives::b256!(
                "38444fd04f6a77753a048d309b3d9358fe46f3d95d6e4113eb4b77457c181b45"
            )
        );
        assert_eq!(
            alloy_primitives::hex::encode(&h.extra_data),
            concat!(
                "736f766100000000000000000000000000000000000000000000000000000000",
                "1d4ce966abb47c93a16e92868a4d893d0924cadf2a5b1be53e12ec9cffab1d47",
                "7586c222ee128d63bb488e6ddaa7503a39f22c30750ec4e4fecbd78267c5b9ff",
                "00"
            )
        );
    }

    fn header_with_vanity() -> Header {
        let mut h = header();
        h.extra_data = Bytes::copy_from_slice(&vanity(b"sova"));
        h
    }

    #[test]
    fn a_seal_for_another_chain_names_another_signer() {
        let h = sealed(MAINNET);
        assert_ne!(recover(&h, TESTNET), address_of(key()));
    }

    #[test]
    fn the_seal_covers_every_header_field_and_the_vanity() {
        let signer = address_of(key());
        let mut h = sealed(TESTNET);
        h.state_root = B256::repeat_byte(0x33);
        assert_ne!(recover(&h, TESTNET), signer, "state root is covered");
        let mut h = sealed(TESTNET);
        let mut extra = h.extra_data.to_vec();
        extra[0] ^= 1;
        h.extra_data = Bytes::from(extra);
        assert_ne!(recover(&h, TESTNET), signer, "vanity is covered");
    }

    #[test]
    fn non_canonical_seals_are_rejected() {
        let h = sealed(TESTNET);
        let with = |f: &dyn Fn(&mut Vec<u8>)| {
            let mut x = h.clone();
            let mut e = x.extra_data.to_vec();
            f(&mut e);
            x.extra_data = Bytes::from(e);
            x
        };
        // Lengths 96 and 98.
        assert_eq!(
            recover(
                &with(&|e| {
                    e.pop();
                }),
                TESTNET
            ),
            Err(SealError::Length(96))
        );
        assert_eq!(
            recover(&with(&|e| e.push(0)), TESTNET),
            Err(SealError::Length(98))
        );
        // v = 27.
        assert_eq!(
            recover(&with(&|e| e[96] = 27), TESTNET),
            Err(SealError::RecoveryId(27))
        );
        // r = 0.
        assert_eq!(
            recover(&with(&|e| e[32..64].fill(0)), TESTNET),
            Err(SealError::OutOfRange)
        );
        // The high-s twin: s' = n − s, v flipped. It would recover the same
        // signer through a library that accepts high s.
        let twin = with(&|e| {
            let s = U256::from_be_slice(&e[64..96]);
            e[64..96].copy_from_slice(&(SECP256K1N - s).to_be_bytes::<32>());
            e[96] ^= 1;
        });
        assert_eq!(recover(&twin, TESTNET), Err(SealError::HighS));
    }

    /// `header()` with SIP-6's pinned randao (vectors keep the plain one).
    fn pinned_header() -> Header {
        let mut h = header();
        h.mix_hash = pinned_randao(h.parent_beacon_block_root.unwrap_or_default());
        h
    }

    fn pinned_sealed() -> Header {
        let mut h = pinned_header();
        sign(&mut h, b"sova", key(), TESTNET).unwrap_or_else(|e| panic!("{e}"));
        h
    }

    #[test]
    fn the_header_rule_before_and_after_activation() {
        let signer = address_of(key()).ok();
        // Before SIP-6: up to 32 bytes, no seal needed.
        assert_eq!(check_header(&header(), None), Ok(None));
        assert_eq!(
            check_header(&sealed(TESTNET), None),
            Err(SealError::PreSip6Length(97))
        );
        // After: sealed recovers its signer; an unsealed 4-byte vanity fails.
        assert_eq!(check_header(&pinned_sealed(), Some(TESTNET)), Ok(signer));
        assert_eq!(
            check_header(&pinned_header(), Some(TESTNET)),
            Err(SealError::Length(4))
        );
        // The randao is pinned to the anchor for every block.
        assert_eq!(
            check_header(&sealed(TESTNET), Some(TESTNET)),
            Err(SealError::Randao)
        );
        // A null block must be empty.
        let mut null = pinned_header();
        null.extra_data = Bytes::new();
        null.transactions_root = EMPTY_ROOT;
        null.withdrawals_root = Some(EMPTY_ROOT);
        assert_eq!(check_header(&null, Some(TESTNET)), Ok(None));
        let mut paid = null.clone();
        paid.beneficiary = Address::repeat_byte(1);
        assert_eq!(
            check_header(&paid, Some(TESTNET)),
            Err(SealError::NullNotEmpty("a beneficiary"))
        );
        let mut minting = null.clone();
        minting.withdrawals_root = Some(B256::repeat_byte(9));
        assert_eq!(
            check_header(&minting, Some(TESTNET)),
            Err(SealError::NullNotEmpty("withdrawals"))
        );
        let mut busy = null.clone();
        busy.transactions_root = B256::repeat_byte(9);
        assert_eq!(
            check_header(&busy, Some(TESTNET)),
            Err(SealError::NullNotEmpty("transactions"))
        );
        let mut random = null;
        random.mix_hash = B256::repeat_byte(3);
        assert_eq!(check_header(&random, Some(TESTNET)), Err(SealError::Randao));
        // Genesis is exempt either way.
        let mut genesis = header();
        genesis.number = 0;
        genesis.extra_data = Bytes::from_static(b"sova-testnet-v0");
        assert_eq!(check_header(&genesis, Some(TESTNET)), Ok(None));
    }

    /// SIP-6 §2.5 against the parent: the null block's timestamp is the
    /// one value `max(parent + 1, zcash_time)`; a sealed block's lies in
    /// `(parent, max(parent + 1, zcash_time + 900)]`.
    #[test]
    fn timestamps_and_gas_against_the_parent() {
        let mut parent = header();
        parent.timestamp = 1_000;
        let at = |ts: u64, null: bool| {
            let mut h = header();
            h.timestamp = ts;
            if null {
                h.extra_data = Bytes::new();
            }
            h
        };
        // Zcash time ahead of the parent: the null block sits at it.
        assert_eq!(
            check_against_parent(&at(1_500, true), &parent, 1_500),
            Ok(())
        );
        assert!(check_against_parent(&at(1_501, true), &parent, 1_500).is_err());
        // Zcash time behind: parent + 1.
        assert_eq!(check_against_parent(&at(1_001, true), &parent, 10), Ok(()));
        let mut wide = at(1_001, true);
        wide.gas_limit += 1;
        assert!(matches!(
            check_against_parent(&wide, &parent, 10),
            Err(SealError::NullGasLimit(..))
        ));
        // Sealed: after the parent, at most zcash_time + 900.
        let sealed_at = |ts| {
            let mut h = at(ts, false);
            h.extra_data = Bytes::from(vec![0u8; SEALED_EXTRA_LEN]);
            h
        };
        assert_eq!(
            check_against_parent(&sealed_at(2_400), &parent, 1_500),
            Ok(())
        );
        assert!(check_against_parent(&sealed_at(2_401), &parent, 1_500).is_err());
        assert!(check_against_parent(&sealed_at(1_000), &parent, 1_500).is_err());
        assert_eq!(check_against_parent(&sealed_at(1_001), &parent, 10), Ok(()));
    }

    #[test]
    fn null_and_bad_lengths_classify() {
        let mut h = header();
        h.extra_data = Bytes::new();
        assert_eq!(kind(&h), Ok(SealKind::Null));
        h.extra_data = Bytes::from_static(&[0; 32]);
        assert_eq!(kind(&h), Err(SealError::Length(32)));
    }
}
