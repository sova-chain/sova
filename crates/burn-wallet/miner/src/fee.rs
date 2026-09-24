//! ZIP-317 conventional fee computation.
//!
//! [ZIP-317](https://zips.z.cash/zip-0317) defines the conventional fee as:
//!
//! ```text
//! conventional_fee = marginal_fee * max(grace_actions, logical_actions)
//! ```
//!
//! with `marginal_fee = 5000` zatoshis and `grace_actions = 2`.
//!
//! For a transaction with no shielded components (every SIP-1 burn built
//! by this miner is transparent-only -- see `burn_wallet::tx`),
//! `logical_actions` is the transparent contribution alone:
//!
//! ```text
//! logical_actions = max(ceil(tx_in_total_size / 150), ceil(tx_out_total_size / 34))
//! ```
//!
//! where `tx_in_total_size`/`tx_out_total_size` are the total serialized
//! byte size of the transaction's transparent inputs/outputs, and 150/34
//! are ZIP-317's own reference ("standard") P2PKH input/output sizes.
//!
//! **This module's fee formula is NOT `max(input_count, output_count)`,
//! despite that being the naive reading of "logical actions" -- ZIP-317
//! actually measures *byte size*, not item count.** That distinction is
//! not academic here: this miner's SIP-1 payload output is a 38-byte
//! `OP_RETURN` (8 value + 1 CompactSize length + 29 script bytes), *larger*
//! than ZIP-317's 34-byte P2PKH reference output size, since ZIP-317 has no
//! "standard size" for null-data outputs (only payable P2PKH/P2SH scripts
//! get one -- see the rationale in the ZIP text). A first implementation of
//! this module used the naive `max(count_in, count_out)` reading and
//! computed 15,000 zat for our 1-in/3-out (payload + eater + change) shape;
//! that transaction was **empirically rejected** by Zebra's regtest mempool
//! (`RPC error -25 ... failed to verify ZIP-317 transaction rules ...
//! Unpaid actions is higher than the limit`) when run against
//! `box/regtest` during this task. Redoing the computation by actual byte
//! size gives `ceil(106 / 34) = 4` (106 = 38 payload + 34 eater + 34
//! change) rather than the naive count's `max(1, 3) = 3`, i.e. a real
//! conventional fee of `5000 * 4 = 20,000` zat -- and a transaction built
//! with that fee **was** accepted. This confirms
//! `crates/burn-wallet/tests/e2e_regtest_burn.rs`'s flat 25,000 zat (for
//! the same 1-in/3-out shape) was a safe margin *above* the real 20,000 zat
//! floor, not below it -- consistent with that test's own comment, and
//! with its earlier finding that a flat 1,000 zat fee was rejected outright.

// The ZIP-317 constants, formula and burn payload sizes live in
// `burn_wallet::fee` (shared with `sova-faucet`); this module keeps the
// miner's dust rule and the tests that pin the burn shapes it builds.

pub(crate) use burn_wallet::fee::BurnPayloadVersion;

/// ZIP-317 marginal fee, in zatoshis (only the tests below name it now).
#[cfg(test)]
pub(crate) const MARGINAL_FEE_ZAT: u64 = burn_wallet::fee::MARGINAL_FEE_ZAT;

/// ZIP-317 grace-actions floor: the conventional fee never charges for
/// fewer than this many logical actions, even for very small transactions.
#[cfg(test)]
pub(crate) const GRACE_ACTIONS: u64 = burn_wallet::fee::GRACE_ACTIONS;

/// Zatoshi value below which a would-be change output is folded into the
/// fee instead of being created as its own output. This isn't a Zcash
/// consensus or mempool-policy dust rule (Zebra enforces none for
/// transparent outputs); it's this miner's own housekeeping choice to
/// avoid leaving economically-meaningless outputs in its UTXO pool. Chosen
/// to match `consensus::sip1::MIN_BURN_ZAT`, the smallest value this
/// codebase already treats as "not negligible".
pub(crate) const DUST_THRESHOLD_ZAT: u64 = consensus::sip1::MIN_BURN_ZAT;

/// Computes the ZIP-317 conventional fee, in zatoshis, for a burn
/// transaction with `transparent_inputs` P2PKH inputs, the mandatory
/// payload output (`payload`: SIP-1's 38-byte v1 output, or SIP-8's 74-byte
/// v2 output), the mandatory eater output, and a change output iff
/// `has_change` (see the module docs for why this needs the transaction's
/// *shape*, not just an output count, to compute correctly). The sizes and
/// formula are `burn_wallet::fee::burn_fee_zat`'s.
#[must_use]
pub(crate) fn zip317_fee_zat(
    transparent_inputs: u64,
    payload: BurnPayloadVersion,
    has_change: bool,
) -> u64 {
    burn_wallet::fee::burn_fee_zat(transparent_inputs, payload, has_change)
}

#[cfg(test)]
mod tests {
    use super::*;
    use BurnPayloadVersion::{V1, V2};

    #[test]
    fn matches_the_empirically_confirmed_shape() {
        // 1 input, payload + eater + change (3 outputs): the exact shape
        // both crates/burn-wallet/tests/e2e_regtest_burn.rs (flat 25,000
        // zat fee) and this task's own regtest run against box/regtest
        // exercised. 25,000 > 20,000 confirms the e2e's flat fee was a
        // safe margin over this real floor, and a transaction built with
        // exactly 20,000 zat was empirically accepted by Zebra's mempool
        // where this module's earlier (naive, count-based) 15,000 zat
        // computation was rejected.
        assert_eq!(zip317_fee_zat(1, V1, true), 20_000);
        assert!(zip317_fee_zat(1, V1, true) < 25_000);
    }

    #[test]
    fn no_change_output_shape() {
        // 1 input, payload + eater (2 outputs, no change): tx_out_total_size
        // = 38 + 34 = 72, ceil(72/34) = 3.
        assert_eq!(zip317_fee_zat(1, V1, false), 15_000);
    }

    #[test]
    fn grace_floor_never_binds_for_this_tx_shape() {
        // grace_actions (2) exists to give genuinely tiny transactions a
        // floor, but it never actually applies to *this* miner's burns:
        // even with zero inputs, the mandatory payload + eater outputs
        // alone (72 bytes) already need ceil(72/34) = 3 actions, above the
        // 2-action grace floor. The floor is exercised directly instead,
        // independent of this transaction shape.
        assert_eq!(zip317_fee_zat(0, V1, false), 15_000);
        assert!(MARGINAL_FEE_ZAT * GRACE_ACTIONS < zip317_fee_zat(0, V1, false));
    }

    #[test]
    fn grace_floor_constant_matches_zip_317() {
        assert_eq!(MARGINAL_FEE_ZAT, 5_000);
        assert_eq!(GRACE_ACTIONS, 2);
    }

    #[test]
    fn scales_with_more_inputs() {
        // Enough inputs that the input side dominates the output side.
        // 5 inputs * 150 = 750 bytes, ceil(750/150) = 5 > the output side's
        // 3 or 4 -- input count now drives the fee.
        assert_eq!(zip317_fee_zat(5, V1, false), 25_000);
        assert_eq!(zip317_fee_zat(5, V1, true), 25_000);
    }

    #[test]
    fn output_shape_alone_can_exceed_naive_counting() {
        // The point of this whole module: a naive max(count_in, count_out)
        // reading would give max(1, 3) = 3 actions (15,000 zat) for the
        // with-change shape. The real, byte-size-based rule gives 4
        // actions (20,000 zat) instead, because the SIP-1 payload output
        // (38 bytes) doesn't fit the 34-byte P2PKH reference size.
        let naive_count_based_actions = 3u64; // max(1 input, 3 outputs)
        let real_fee = zip317_fee_zat(1, V1, true);
        assert!(real_fee > MARGINAL_FEE_ZAT * naive_count_based_actions);
    }

    /// SIP-8 §6 "Fees": the 74-byte v2 payload output costs exactly one
    /// more logical action (5,000 zat) in both shapes the miner builds.
    #[test]
    fn sip8_v2_payload_adds_one_action_in_both_shapes() {
        // 1 in, payload + eater + change: 74 + 34 + 34 = 142 -> 5 actions.
        assert_eq!(zip317_fee_zat(1, V2, true), 25_000);
        // 1 in, payload + eater: 74 + 34 = 108 -> 4 actions.
        assert_eq!(zip317_fee_zat(1, V2, false), 20_000);
        assert_eq!(
            zip317_fee_zat(1, V2, true) - zip317_fee_zat(1, V1, true),
            5_000
        );
        assert_eq!(
            zip317_fee_zat(1, V2, false) - zip317_fee_zat(1, V1, false),
            5_000
        );
        assert_eq!(BurnPayloadVersion::V1.output_size(), 38);
        assert_eq!(BurnPayloadVersion::V2.output_size(), 74);
    }
}
