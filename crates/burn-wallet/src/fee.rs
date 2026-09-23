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
    }
}
