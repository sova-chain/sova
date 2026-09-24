//! SIP-1 (draft v0): the burn transaction format.
//!
//! A Sova mining act is one Zcash transparent transaction containing:
//!
//! 1. **Exactly one payload output**: a zero-value `OP_RETURN` output whose
//!    script is exactly `OP_RETURN OP_PUSHBYTES_27 <payload>`, where the
//!    27-byte payload is `magic (2) ‖ version (1) ‖ evm_address (20) ‖
//!    signal_bits (4, big-endian)`.
//! 2. **One or more burn outputs**: value paid to the canonical eater
//!    script — a P2PKH whose hash160 is all zeroes. No preimage of the
//!    zero hash is known, so these outputs are computationally
//!    unspendable; the value is destroyed.
//!
//! The burn's *weight* is the sum of all eater-output values in the
//! transaction, and must meet [`MIN_BURN_ZAT`]. A transaction with zero
//! valid payload scripts, more than one payload script (ambiguous), an
//! undecodable payload, or insufficient eater value is **not a burn** and
//! is ignored entirely — malformed burns never error, they simply don't
//! exist. This keeps the consensus rule total: every possible transaction
//! deterministically either is a burn or is not.
//!
//! Everything here is pure and allocation-light on the parse path: the
//! Zcash follower hands us raw output (value, script) pairs and we decide.
//! These constants freeze when SIP-1 freezes; until then they are draft.

/// Two-byte payload magic: `"SV"`.
pub const MAGIC: [u8; 2] = *b"SV";

/// Payload format version this module implements.
pub const VERSION_V0: u8 = 1;

/// Total payload length in bytes: magic (2) + version (1) + address (20) +
/// signal bits (4).
pub const PAYLOAD_LEN: usize = 27;

/// Exact length of a well-formed payload script:
/// `OP_RETURN (1) + OP_PUSHBYTES_27 (1) + payload (27)`.
pub const PAYLOAD_SCRIPT_LEN: usize = 2 + PAYLOAD_LEN;

/// The canonical eater hash160: twenty zero bytes. A P2PKH output paying
/// this hash is computationally unspendable (no known preimage), and
/// trivially recognizable without base58/bech32 machinery.
pub const BURN_HASH160: [u8; 20] = [0u8; 20];

/// Exact length of the eater lock script (standard P2PKH).
pub const BURN_SCRIPT_LEN: usize = 25;

/// Minimum total eater value (in zatoshis) for a transaction to count as
/// a burn. Anti-dust only — there is deliberately no maximum and no
/// difficulty: the market prices an epoch. Draft value: 1,000 zatoshis.
pub const MIN_BURN_ZAT: u64 = 1_000;

// The payload script must relay under Zebra's default 83-byte datacarrier
// cap (zcashd `MAX_OP_RETURN_RELAY` parity); enforced at compile time.
const _: () = assert!(PAYLOAD_SCRIPT_LEN <= 83);

/// `OP_RETURN` opcode.
const OP_RETURN: u8 = 0x6a;
/// Direct push of 27 bytes.
const OP_PUSHBYTES_27: u8 = PAYLOAD_LEN as u8;

/// A decoded SIP-1 payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurnPayload {
    /// The Sova EVM address credited with this burn's weight.
    pub evm_address: [u8; 20],
    /// BIP9-style upgrade signal bits (big-endian on the wire).
    pub signal_bits: u32,
}

impl BurnPayload {
    /// Encode to the 27-byte wire payload.
    #[must_use]
    pub fn encode(&self) -> [u8; PAYLOAD_LEN] {
        let mut out = [0u8; PAYLOAD_LEN];
        out[0..2].copy_from_slice(&MAGIC);
        out[2] = VERSION_V0;
        out[3..23].copy_from_slice(&self.evm_address);
        out[23..27].copy_from_slice(&self.signal_bits.to_be_bytes());
        out
    }

    /// Strict decode of a 27-byte wire payload. Returns `None` on any
    /// deviation: wrong length, wrong magic, unknown version.
    #[must_use]
    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() != PAYLOAD_LEN {
            return None;
        }
        if payload[0..2] != MAGIC || payload[2] != VERSION_V0 {
            return None;
        }
        let mut evm_address = [0u8; 20];
        evm_address.copy_from_slice(&payload[3..23]);
        let mut bits = [0u8; 4];
        bits.copy_from_slice(&payload[23..27]);
        Some(Self {
            evm_address,
            signal_bits: u32::from_be_bytes(bits),
        })
    }

    /// Build the full payload output script:
    /// `OP_RETURN OP_PUSHBYTES_27 <payload>`.
    #[must_use]
    pub fn to_script(&self) -> [u8; PAYLOAD_SCRIPT_LEN] {
        let mut script = [0u8; PAYLOAD_SCRIPT_LEN];
        script[0] = OP_RETURN;
        script[1] = OP_PUSHBYTES_27;
        script[2..].copy_from_slice(&self.encode());
        script
    }

    /// Strict parse of a payload output script. The script must be exactly
    /// `OP_RETURN OP_PUSHBYTES_27 <27 bytes>` — no other pushes, no
    /// trailing bytes, no `OP_PUSHDATA1` alternate encoding.
    #[must_use]
    pub fn from_script(script: &[u8]) -> Option<Self> {
        if script.len() != PAYLOAD_SCRIPT_LEN
            || script[0] != OP_RETURN
            || script[1] != OP_PUSHBYTES_27
        {
            return None;
        }
        Self::decode(&script[2..])
    }
}

/// The canonical eater lock script: `OP_DUP OP_HASH160 <zero hash160>
/// OP_EQUALVERIFY OP_CHECKSIG`.
#[must_use]
pub fn burn_lock_script() -> [u8; BURN_SCRIPT_LEN] {
    let mut script = [0u8; BURN_SCRIPT_LEN];
    script[0] = 0x76; // OP_DUP
    script[1] = 0xa9; // OP_HASH160
    script[2] = 0x14; // OP_PUSHBYTES_20
    script[3..23].copy_from_slice(&BURN_HASH160);
    script[23] = 0x88; // OP_EQUALVERIFY
    script[24] = 0xac; // OP_CHECKSIG
    script
}

/// Returns `true` if `script` is exactly the canonical eater lock script.
#[must_use]
pub fn is_burn_lock_script(script: &[u8]) -> bool {
    script == burn_lock_script()
}

/// One transparent output, as the Zcash follower hands them to us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxOutRef<'a> {
    /// Output value in zatoshis.
    pub value_zat: u64,
    /// Raw lock script bytes.
    pub script: &'a [u8],
}

/// A recognized burn: the consensus-relevant summary of one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Burn {
    /// EVM address credited.
    pub evm_address: [u8; 20],
    /// Upgrade signal bits carried by the payload.
    pub signal_bits: u32,
    /// Total zatoshis destroyed (sum of all eater outputs).
    pub value_zat: u64,
}

/// Decide whether a transaction's transparent outputs constitute a burn.
///
/// Rules (total, deterministic):
/// - exactly one well-formed payload script must be present — zero means
///   not a burn, two or more is ambiguous and therefore not a burn;
/// - eater-output values are summed with saturating arithmetic;
/// - the sum must be at least [`MIN_BURN_ZAT`].
///
/// This is SIP-1 alone: [`extract_burn_at`] with SIP-8 inactive.
#[must_use]
pub fn extract_burn<'a, I>(outputs: I) -> Option<Burn>
where
    I: IntoIterator<Item = TxOutRef<'a>>,
{
    extract_burn_at(outputs, false).map(|(burn, _)| burn)
}

// ---------------------------------------------------------------------------
// SIP-8: anchored burns (payload version 2)
// ---------------------------------------------------------------------------

/// SIP-8 payload version byte.
pub const VERSION_V2: u8 = 2;

/// SIP-8 payload length: the SIP-1 fields (27) + reference height (4) +
/// reference hash (32).
pub const PAYLOAD_V2_LEN: usize = 63;

/// Exact length of a well-formed version-2 payload script:
/// `OP_RETURN (1) + OP_PUSHBYTES_63 (1) + payload (63)`.
pub const PAYLOAD_V2_SCRIPT_LEN: usize = 2 + PAYLOAD_V2_LEN;

const _: () = assert!(PAYLOAD_V2_SCRIPT_LEN <= 83);

/// Direct push of 63 bytes.
const OP_PUSHBYTES_63: u8 = PAYLOAD_V2_LEN as u8;

/// A reference to a Sova block: its number and its hash, in the byte order
/// `eth_getBlockByNumber(..).hash` prints (not reversed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SovaRef {
    /// Sova block number.
    pub height: u32,
    /// Sova block hash.
    pub hash: [u8; 32],
}

impl SovaRef {
    /// SIP-8 §2.3: whether a burn confirmed in Zcash block `zcash_height`
    /// casts a vote for this reference, on a chain whose epoch base is
    /// `epoch_base`. It does iff `1 ≤ height ≤ zcash_height − epoch_base`:
    /// genesis is never voted for, and a burn can only vote for a block that
    /// could exist before its own Zcash block. Anything else is a burn with
    /// no vote, never an error.
    #[must_use]
    pub fn votes_at(&self, zcash_height: u64, epoch_base: u64) -> bool {
        let h = u64::from(self.height);
        h >= 1
            && zcash_height
                .checked_sub(epoch_base)
                .is_some_and(|top| h <= top)
    }
}

/// A decoded SIP-8 (version-2) payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurnPayloadV2 {
    /// The Sova EVM address credited with this burn's weight.
    pub evm_address: [u8; 20],
    /// BIP9-style upgrade signal bits (big-endian on the wire).
    pub signal_bits: u32,
    /// The Sova block this burn references.
    pub reference: SovaRef,
}

impl BurnPayloadV2 {
    /// Encode to the 63-byte wire payload. Bytes 0–26 sit at SIP-1's
    /// offsets; only the version byte differs.
    #[must_use]
    pub fn encode(&self) -> [u8; PAYLOAD_V2_LEN] {
        let mut out = [0u8; PAYLOAD_V2_LEN];
        out[0..2].copy_from_slice(&MAGIC);
        out[2] = VERSION_V2;
        out[3..23].copy_from_slice(&self.evm_address);
        out[23..27].copy_from_slice(&self.signal_bits.to_be_bytes());
        out[27..31].copy_from_slice(&self.reference.height.to_be_bytes());
        out[31..63].copy_from_slice(&self.reference.hash);
        out
    }

    /// Strict decode of a 63-byte wire payload: wrong length, wrong magic
    /// or any version but 2 is `None`.
    #[must_use]
    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() != PAYLOAD_V2_LEN {
            return None;
        }
        if payload[0..2] != MAGIC || payload[2] != VERSION_V2 {
            return None;
        }
        let mut evm_address = [0u8; 20];
        evm_address.copy_from_slice(&payload[3..23]);
        let mut bits = [0u8; 4];
        bits.copy_from_slice(&payload[23..27]);
        let mut height = [0u8; 4];
        height.copy_from_slice(&payload[27..31]);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&payload[31..63]);
        Some(Self {
            evm_address,
            signal_bits: u32::from_be_bytes(bits),
            reference: SovaRef {
                height: u32::from_be_bytes(height),
                hash,
            },
        })
    }

    /// Build the full payload output script:
    /// `OP_RETURN OP_PUSHBYTES_63 <payload>`.
    #[must_use]
    pub fn to_script(&self) -> [u8; PAYLOAD_V2_SCRIPT_LEN] {
        let mut script = [0u8; PAYLOAD_V2_SCRIPT_LEN];
        script[0] = OP_RETURN;
        script[1] = OP_PUSHBYTES_63;
        script[2..].copy_from_slice(&self.encode());
        script
    }

    /// Strict parse of a version-2 payload output script: exactly
    /// `OP_RETURN OP_PUSHBYTES_63 <63 bytes>`, no `OP_PUSHDATA1` alternate,
    /// no trailing bytes.
    #[must_use]
    pub fn from_script(script: &[u8]) -> Option<Self> {
        if script.len() != PAYLOAD_V2_SCRIPT_LEN
            || script[0] != OP_RETURN
            || script[1] != OP_PUSHBYTES_63
        {
            return None;
        }
        Self::decode(&script[2..])
    }
}

/// SIP-1 recognition extended by SIP-8 §2.2, total and deterministic.
///
/// With `P1` well-formed SIP-1 payload outputs, `P2` well-formed version-2
/// payload outputs and `w` the summed eater value:
///
/// | `P1` | `P2` | `w ≥ MIN_BURN_ZAT` | result |
/// |---|---|---|---|
/// | 1 | any | yes | v1 burn, no reference |
/// | ≥ 2 | any | any | not a burn |
/// | 0 | 1 | yes | v2 burn with its reference (only if `sip8_active`) |
/// | 0 | ≥ 2 | any | not a burn |
/// | 0 | 0 | any | not a burn |
/// | any | any | no | not a burn |
///
/// With `sip8_active` false, version-2 outputs are ignored and the result is
/// exactly SIP-1's. A v1 + v2 transaction is a v1 burn either way, so SIP-8
/// never changes the credit, weight or signal bits of a burn SIP-1 already
/// recognizes. Whether a v2 burn's reference is also a vote is
/// [`SovaRef::votes_at`]'s question, not this one.
#[must_use]
pub fn extract_burn_at<'a, I>(outputs: I, sip8_active: bool) -> Option<(Burn, Option<SovaRef>)>
where
    I: IntoIterator<Item = TxOutRef<'a>>,
{
    let mut v1: Option<BurnPayload> = None;
    let mut v1_count = 0usize;
    let mut v2: Option<BurnPayloadV2> = None;
    let mut v2_count = 0usize;
    let mut burned: u64 = 0;
    for out in outputs {
        if let Some(p) = BurnPayload::from_script(out.script) {
            v1_count += 1;
            v1 = Some(p);
        } else if let Some(p) = BurnPayloadV2::from_script(out.script) {
            v2_count += 1;
            v2 = Some(p);
        } else if is_burn_lock_script(out.script) {
            burned = burned.saturating_add(out.value_zat);
        }
    }
    if burned < MIN_BURN_ZAT {
        return None;
    }
    let (evm_address, signal_bits, reference) = match (v1_count, v2_count) {
        (1, _) => v1.map(|p| (p.evm_address, p.signal_bits, None))?,
        (0, 1) if sip8_active => v2.map(|p| (p.evm_address, p.signal_bits, Some(p.reference)))?,
        _ => return None,
    };
    let burn = Burn {
        evm_address,
        signal_bits,
        value_zat: burned,
    };
    Some((burn, reference))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: [u8; 20] = [0xab; 20];

    fn payload() -> BurnPayload {
        BurnPayload {
            evm_address: ADDR,
            signal_bits: 0xdead_beef,
        }
    }

    #[test]
    fn payload_roundtrip() {
        let p = payload();
        assert_eq!(BurnPayload::decode(&p.encode()), Some(p));
        assert_eq!(BurnPayload::from_script(&p.to_script()), Some(p));
    }

    #[test]
    fn payload_wire_layout_is_stable() {
        let bytes = payload().encode();
        assert_eq!(&bytes[0..2], b"SV");
        assert_eq!(bytes[2], VERSION_V0);
        assert_eq!(&bytes[3..23], &ADDR);
        assert_eq!(&bytes[23..27], &0xdead_beef_u32.to_be_bytes());
    }

    #[test]
    fn decode_rejects_bad_inputs() {
        let good = payload().encode();

        let mut wrong_magic = good;
        wrong_magic[0] = b'X';
        assert_eq!(BurnPayload::decode(&wrong_magic), None);

        let mut wrong_version = good;
        wrong_version[2] = 2;
        assert_eq!(BurnPayload::decode(&wrong_version), None);

        assert_eq!(BurnPayload::decode(&good[..26]), None);
        let mut long = good.to_vec();
        long.push(0);
        assert_eq!(BurnPayload::decode(&long), None);
    }

    #[test]
    fn script_parse_is_strict() {
        let good = payload().to_script();
        assert!(BurnPayload::from_script(&good).is_some());

        // Trailing byte.
        let mut long = good.to_vec();
        long.push(0x00);
        assert_eq!(BurnPayload::from_script(&long), None);

        // OP_PUSHDATA1 alternate encoding of the same payload.
        let mut alt = vec![OP_RETURN, 0x4c, PAYLOAD_LEN as u8];
        alt.extend_from_slice(&payload().encode());
        assert_eq!(BurnPayload::from_script(&alt), None);

        // Missing OP_RETURN.
        let mut no_ret = good;
        no_ret[0] = 0x00;
        assert_eq!(BurnPayload::from_script(&no_ret), None);
    }

    #[test]
    fn eater_script_shape() {
        let s = burn_lock_script();
        assert_eq!(s.len(), BURN_SCRIPT_LEN);
        assert!(is_burn_lock_script(&s));
        // Any other hash160 is not the eater.
        let mut other = s;
        other[3] = 1;
        assert!(!is_burn_lock_script(&other));
    }

    fn eater(value_zat: u64) -> (u64, Vec<u8>) {
        (value_zat, burn_lock_script().to_vec())
    }

    fn outs(parts: &[(u64, Vec<u8>)]) -> Vec<TxOutRef<'_>> {
        parts
            .iter()
            .map(|(v, s)| TxOutRef {
                value_zat: *v,
                script: s.as_slice(),
            })
            .collect()
    }

    #[test]
    fn extracts_simple_burn() {
        let parts = vec![(0, payload().to_script().to_vec()), eater(5_000)];
        let burn = extract_burn(outs(&parts));
        assert_eq!(
            burn,
            Some(Burn {
                evm_address: ADDR,
                signal_bits: 0xdead_beef,
                value_zat: 5_000,
            })
        );
    }

    #[test]
    fn sums_multiple_eater_outputs() {
        let parts = vec![
            eater(1_500),
            (0, payload().to_script().to_vec()),
            eater(2_500),
            // Change output to some unrelated P2PKH is ignored.
            (10_000, {
                let mut s = burn_lock_script().to_vec();
                s[10] = 0x42;
                s
            }),
        ];
        let burn = extract_burn(outs(&parts));
        assert_eq!(burn.map(|b| b.value_zat), Some(4_000));
    }

    #[test]
    fn rejects_no_payload_zero_value_and_dust() {
        // Eater value but no payload: not a burn.
        let parts = vec![eater(5_000)];
        assert_eq!(extract_burn(outs(&parts)), None);

        // Payload but no eater value: not a burn.
        let parts = vec![(0, payload().to_script().to_vec())];
        assert_eq!(extract_burn(outs(&parts)), None);

        // Below the dust floor: not a burn.
        let parts = vec![(0, payload().to_script().to_vec()), eater(MIN_BURN_ZAT - 1)];
        assert_eq!(extract_burn(outs(&parts)), None);

        // Exactly at the floor: a burn.
        let parts = vec![(0, payload().to_script().to_vec()), eater(MIN_BURN_ZAT)];
        assert!(extract_burn(outs(&parts)).is_some());
    }

    #[test]
    fn rejects_ambiguous_double_payload() {
        let other = BurnPayload {
            evm_address: [0xcd; 20],
            signal_bits: 0,
        };
        let parts = vec![
            (0, payload().to_script().to_vec()),
            (0, other.to_script().to_vec()),
            eater(5_000),
        ];
        assert_eq!(extract_burn(outs(&parts)), None);
    }

    // --- SIP-8 (§10 unit tests) ---

    const REF_HASH: [u8; 32] = [0x22; 32];

    fn v2(height: u32) -> BurnPayloadV2 {
        BurnPayloadV2 {
            evm_address: [0x11; 20],
            signal_bits: 1,
            reference: SovaRef {
                height,
                hash: REF_HASH,
            },
        }
    }

    /// Hand-assembled from the SIP-8 §2.1 table, not from the encoder.
    const V2_SCRIPT_HEX: &str = concat!(
        "6a3f",                                     // OP_RETURN OP_PUSHBYTES_63
        "5356",                                     // "SV"
        "02",                                       // version 2
        "1111111111111111111111111111111111111111", // address
        "00000001",                                 // signal bits
        "0000abcd",                                 // reference height
        "2222222222222222222222222222222222222222222222222222222222222222", // reference hash
    );

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn v2_test_vector() {
        let script = v2(0xabcd).to_script();
        assert_eq!(script.len(), 65);
        assert_eq!(hex(&script), V2_SCRIPT_HEX);
        assert_eq!(BurnPayloadV2::from_script(&script), Some(v2(0xabcd)));
        // The SIP-1 prefix sits at the SIP-1 offsets.
        let v1 = BurnPayload {
            evm_address: [0x11; 20],
            signal_bits: 1,
        }
        .encode();
        assert_eq!(&script[2..4], &v1[0..2]);
        assert_eq!(script[4], VERSION_V2);
        assert_eq!(&script[5..29], &v1[3..27]);
    }

    #[test]
    fn v2_parse_is_strict() {
        let good = v2(7).to_script();
        // PUSHDATA1 alternate encoding.
        let mut alt = vec![OP_RETURN, 0x4c, PAYLOAD_V2_LEN as u8];
        alt.extend_from_slice(&v2(7).encode());
        assert_eq!(BurnPayloadV2::from_script(&alt), None);
        // 62- and 64-byte pushes.
        let mut short = vec![OP_RETURN, 62];
        short.extend_from_slice(&good[2..64]);
        assert_eq!(BurnPayloadV2::from_script(&short), None);
        let mut long = vec![OP_RETURN, 64];
        long.extend_from_slice(&good[2..]);
        long.push(0);
        assert_eq!(BurnPayloadV2::from_script(&long), None);
        // Trailing byte after a correct push.
        let mut trailing = good.to_vec();
        trailing.push(0);
        assert_eq!(BurnPayloadV2::from_script(&trailing), None);
        // Version 3, version 1 and a wrong magic.
        for (i, b) in [(4usize, 3u8), (4, VERSION_V0), (2, b'X')] {
            let mut bad = good;
            bad[i] = b;
            assert_eq!(BurnPayloadV2::from_script(&bad), None, "byte {i} = {b}");
        }
        // Neither codec accepts the other's script.
        assert_eq!(BurnPayload::from_script(&good), None);
        assert_eq!(BurnPayloadV2::from_script(&payload().to_script()), None);
    }

    fn v1_out() -> (u64, Vec<u8>) {
        (0, payload().to_script().to_vec())
    }

    fn v2_out(height: u32) -> (u64, Vec<u8>) {
        (0, v2(height).to_script().to_vec())
    }

    #[test]
    fn recognition_table_row_by_row() {
        let v1_burn = Burn {
            evm_address: ADDR,
            signal_bits: 0xdead_beef,
            value_zat: 5_000,
        };
        let v2_burn = Burn {
            evm_address: [0x11; 20],
            signal_bits: 1,
            value_zat: 5_000,
        };
        let r = SovaRef {
            height: 9,
            hash: REF_HASH,
        };
        for active in [false, true] {
            // P1 = 1, P2 = 0: v1 burn.
            let p = vec![v1_out(), eater(5_000)];
            assert_eq!(extract_burn_at(outs(&p), active), Some((v1_burn, None)));
            // P1 = 1, P2 = 1: still a v1 burn, no vote.
            let p = vec![v2_out(9), v1_out(), eater(5_000)];
            assert_eq!(extract_burn_at(outs(&p), active), Some((v1_burn, None)));
            // P1 = 1, P2 = 2: still a v1 burn.
            let p = vec![v2_out(9), v1_out(), v2_out(8), eater(5_000)];
            assert_eq!(extract_burn_at(outs(&p), active), Some((v1_burn, None)));
            // P1 ≥ 2: not a burn, whatever else.
            let p = vec![v1_out(), v1_out(), v2_out(9), eater(5_000)];
            assert_eq!(extract_burn_at(outs(&p), active), None);
            // P1 = 0, P2 = 2: ambiguous.
            let p = vec![v2_out(9), v2_out(9), eater(5_000)];
            assert_eq!(extract_burn_at(outs(&p), active), None);
            // P1 = 0, P2 = 0.
            let p = vec![eater(5_000)];
            assert_eq!(extract_burn_at(outs(&p), active), None);
            // Dust: not a burn in any shape.
            for p in [
                vec![v1_out(), eater(MIN_BURN_ZAT - 1)],
                vec![v2_out(9), eater(MIN_BURN_ZAT - 1)],
            ] {
                assert_eq!(extract_burn_at(outs(&p), active), None);
            }
        }
        // P1 = 0, P2 = 1: a v2 burn only once SIP-8 is active.
        let p = vec![v2_out(9), eater(2_000), eater(3_000)];
        assert_eq!(extract_burn_at(outs(&p), true), Some((v2_burn, Some(r))));
        assert_eq!(extract_burn_at(outs(&p), false), None);
        assert_eq!(extract_burn(outs(&p)), None);
    }

    #[test]
    fn sip8_off_is_exactly_sip1() {
        // extract_burn is extract_burn_at(.., false) minus the reference; the
        // SIP-1 tests above run through it, and a v2 output never changes a
        // v1 result.
        let with_v2 = vec![v1_out(), v2_out(3), eater(5_000)];
        let without = vec![v1_out(), eater(5_000)];
        assert_eq!(extract_burn(outs(&with_v2)), extract_burn(outs(&without)));
    }

    #[test]
    fn vote_range() {
        // Epoch base B = 1000; a burn in Zcash block E = 1010 may vote for
        // Sova heights 1..=10.
        let (e, b) = (1_010u64, 1_000u64);
        let at = |h: u32| {
            SovaRef {
                height: h,
                hash: REF_HASH,
            }
            .votes_at(e, b)
        };
        assert!(!at(0), "genesis is never voted for");
        assert!(at(1));
        assert!(at(10), "h = E − B");
        assert!(!at(11), "h = E − B + 1");
        assert!(!at(u32::MAX));
        // A burn at or below the base can vote for nothing.
        let r = SovaRef {
            height: 1,
            hash: REF_HASH,
        };
        assert!(!r.votes_at(b, b));
        assert!(!r.votes_at(b - 1, b));
        assert!(r.votes_at(b + 1, b));
    }

    #[test]
    fn value_sum_saturates() {
        let parts = vec![
            (0, payload().to_script().to_vec()),
            eater(u64::MAX),
            eater(u64::MAX),
        ];
        assert_eq!(
            extract_burn(outs(&parts)).map(|b| b.value_zat),
            Some(u64::MAX)
        );
    }
}
