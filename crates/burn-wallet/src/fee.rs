//! ZIP-317 conventional fee computation for transparent-only transactions.
//!
//! [ZIP-317](https://zips.z.cash/zip-0317) defines the conventional fee as
//!
//! ```text
//! conventional_fee = marginal_fee * max(grace_actions, logical_actions)
//! ```
//!
//! with `marginal_fee = 5000` zatoshis and `grace_actions = 2`. For a
//! transaction with no shielded components (everything this crate builds),
//! `logical_actions` is the transparent contribution alone:
//!
//! ```text
//! logical_actions = max(ceil(tx_in_total_size / 150), ceil(tx_out_total_size / 34))
//! ```
//!
//! ZIP-317 measures **byte size**, not item count: 150 and 34 are its
//! reference ("standard") P2PKH input and output sizes. `sova-miner`'s
//! `fee` module documents the regtest run that proved the difference
//! matters (a 38-byte SIP-1 `OP_RETURN` output pushes a 1-in/3-out burn to
//! four actions, and Zebra's mempool rejected the count-based three). The
//! miner and `sova-faucet` both compute fees through this module.

/// ZIP-317 marginal fee, in zatoshis.
pub const MARGINAL_FEE_ZAT: u64 = 5_000;

/// ZIP-317 grace-actions floor.
pub const GRACE_ACTIONS: u64 = 2;

/// ZIP-317's reference size of one P2PKH transparent input, in bytes. Every
/// P2PKH input is charged at this size, whatever its exact signature
/// length, which is what lets the fee be fixed before signing.
pub const P2PKH_STANDARD_INPUT_SIZE: u64 = 150;

/// ZIP-317's reference size of one P2PKH transparent output, in bytes: 8
/// (value) + 1 (script length) + 25 (P2PKH script).
pub const P2PKH_STANDARD_OUTPUT_SIZE: u64 = 34;

/// The serialized size of one P2SH output: 8 (value) + 1 (script length) +
/// 23 (P2SH script).
pub const P2SH_OUTPUT_SIZE: u64 = 32;

/// Which burn payload a burn transaction carries in its one `OP_RETURN`
/// output: SIP-1's version 1, or SIP-8's version 2 (the same fields plus a
/// reference to a Sova block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnPayloadVersion {
    /// SIP-1: `OP_RETURN OP_PUSHBYTES_27 <27 bytes>`.
    V1,
    /// SIP-8: `OP_RETURN OP_PUSHBYTES_63 <63 bytes>`.
    V2,
}

impl BurnPayloadVersion {
    /// The serialized size of the payload output, in bytes: 8 (value, always
    /// zero) + 1 (script length; both scripts are under 253 bytes) + the
    /// script. 38 for v1, 74 for v2. ZIP-317 has no reference size for
    /// null-data outputs, so the real size is what the fee counts.
    #[must_use]
    pub const fn output_size(self) -> u64 {
        let script_len = match self {
            Self::V1 => consensus::sip1::PAYLOAD_SCRIPT_LEN,
            Self::V2 => consensus::sip1::PAYLOAD_V2_SCRIPT_LEN,
        };
        8 + 1 + script_len as u64
    }
}

/// The ZIP-317 fee, in zatoshis, for a burn transaction: `p2pkh_inputs`
/// P2PKH inputs, the `payload` output, the eater output and, iff
/// `has_change`, one P2PKH change output. The outputs are in that order in
/// every burn [`crate::tx::build_burn_transaction`] builds.
///
/// SIP-8 §6 "Fees": the v2 payload adds one logical action (5,000 zat) to
/// both the usual 1-in/3-out shape (106 → 142 output bytes, 4 → 5 actions)
/// and the no-change shape (72 → 108 bytes, 3 → 4 actions).
#[must_use]
pub fn burn_fee_zat(p2pkh_inputs: u64, payload: BurnPayloadVersion, has_change: bool) -> u64 {
    let change = if has_change {
        P2PKH_STANDARD_OUTPUT_SIZE
    } else {
        0
    };
    transparent_fee_zat(
        p2pkh_inputs,
        &[payload.output_size(), P2PKH_STANDARD_OUTPUT_SIZE, change],
    )
}

/// The ZIP-317 conventional fee, in zatoshis, for a transparent-only
/// transaction whose inputs total `tx_in_total_size` bytes and whose
/// outputs total `tx_out_total_size` bytes.
#[must_use]
pub fn conventional_fee_zat(tx_in_total_size: u64, tx_out_total_size: u64) -> u64 {
    let logical_actions = tx_in_total_size
        .div_ceil(P2PKH_STANDARD_INPUT_SIZE)
        .max(tx_out_total_size.div_ceil(P2PKH_STANDARD_OUTPUT_SIZE));
    MARGINAL_FEE_ZAT.saturating_mul(logical_actions.max(GRACE_ACTIONS))
}

/// The ZIP-317 fee for spending `p2pkh_inputs` P2PKH inputs into outputs
/// whose serialized sizes are `output_sizes` (use
/// [`P2PKH_STANDARD_OUTPUT_SIZE`] / [`P2SH_OUTPUT_SIZE`]).
#[must_use]
pub fn transparent_fee_zat(p2pkh_inputs: u64, output_sizes: &[u64]) -> u64 {
    let tx_in_total_size = p2pkh_inputs.saturating_mul(P2PKH_STANDARD_INPUT_SIZE);
    let tx_out_total_size = output_sizes.iter().fold(0u64, |a, s| a.saturating_add(*s));
    conventional_fee_zat(tx_in_total_size, tx_out_total_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_send_pays_the_grace_floor() {
        // 1 in, recipient + change: max(1, ceil(68/34)=2) = 2 = grace floor.
        let fee = transparent_fee_zat(1, &[P2PKH_STANDARD_OUTPUT_SIZE; 2]);
        assert_eq!(fee, 10_000);
    }

    #[test]
    fn inputs_drive_the_fee_when_they_dominate() {
        let fee = transparent_fee_zat(3, &[P2PKH_STANDARD_OUTPUT_SIZE; 2]);
        assert_eq!(fee, 15_000);
    }

    #[test]
    fn p2sh_output_is_smaller_than_the_reference() {
        let fee = transparent_fee_zat(1, &[P2SH_OUTPUT_SIZE, P2PKH_STANDARD_OUTPUT_SIZE]);
        assert_eq!(fee, 10_000);
    }

    #[test]
    fn matches_the_miner_burn_shape() {
        // 1 in, 38-byte SIP-1 payload + eater + change = 106 bytes -> 4.
        assert_eq!(conventional_fee_zat(150, 38 + 34 + 34), 20_000);
        assert_eq!(burn_fee_zat(1, BurnPayloadVersion::V1, true), 20_000);
    }

    #[test]
    fn payload_output_sizes() {
        assert_eq!(BurnPayloadVersion::V1.output_size(), 38);
        assert_eq!(BurnPayloadVersion::V2.output_size(), 74);
    }

    /// SIP-8 §6 "Fees", figure by figure: a v2 burn costs one more ZIP-317
    /// logical action, 5,000 zat, than a v1 burn, with and without change.
    #[test]
    fn sip8_v2_burn_costs_one_more_action_in_both_shapes() {
        use BurnPayloadVersion::{V1, V2};
        // 1 in / 3 out: 38 + 34 + 34 = 106 bytes -> 4 actions;
        //               74 + 34 + 34 = 142 bytes -> 5 actions.
        assert_eq!(burn_fee_zat(1, V1, true), 20_000);
        assert_eq!(burn_fee_zat(1, V2, true), 25_000);
        // No change: 38 + 34 = 72 -> 3 actions; 74 + 34 = 108 -> 4.
        assert_eq!(burn_fee_zat(1, V1, false), 15_000);
        assert_eq!(burn_fee_zat(1, V2, false), 20_000);
        for has_change in [true, false] {
            assert_eq!(
                burn_fee_zat(1, V2, has_change) - burn_fee_zat(1, V1, has_change),
                5_000
            );
        }
        // Once inputs dominate, the payload no longer matters.
        assert_eq!(burn_fee_zat(6, V1, true), burn_fee_zat(6, V2, true));
    }
}
