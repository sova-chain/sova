//! SIP-3: the emission schedule as a pure function of the epoch index.
//!
//! Not yet wired into the node (see SIP-3 "Implementation status" and
//! board task C8): the sealer/expectations paths still pay the flat
//! draft reward. This module is the normative math, locked by tests —
//! wiring is a plumbing change, not a consensus-design change.
//!
//! Shape (see `sips/sip-3.md` for rationale):
//! - Zcash-homage slow start: epochs 0..20,000 ramp linearly in exact
//!   0.3125-SOVA steps (the step divides the base reward exactly — no
//!   rounding anywhere in the ramp).
//! - Then flat 6,250 SOVA per epoch, halving every 1,680,000 epochs
//!   (Zcash's own halving interval; Sova epochs are Zcash blocks).
//! - Halving is integer floor in gwei; era 42 pays 1 gwei, era 43 pays
//!   zero and emission ends.
//! - Burn-less epochs mint nothing and nothing is carried over — that
//!   rule lives in the settlement derivation (SIP-2), not here; this
//!   function is the *ceiling* for an epoch, not a guarantee.

/// Full per-epoch reward from the end of the slow start through era 0,
/// in gwei (6,250 SOVA).
pub const BASE_EPOCH_REWARD_GWEI: u128 = 6_250_000_000_000;

/// Epochs per halving era: Zcash's halving interval (~4 years at 75 s).
pub const ERA_EPOCHS: u64 = 1_680_000;

/// Length of the linear slow-start ramp (~17.4 days at 75 s); the
/// number is Zcash's own slow-start block count.
pub const SLOW_START_EPOCHS: u64 = 20_000;

/// Exact ramp step: `BASE_EPOCH_REWARD_GWEI / SLOW_START_EPOCHS`
/// (0.3125 SOVA). The const assert below guarantees exact division.
pub const SLOW_START_STEP_GWEI: u128 = BASE_EPOCH_REWARD_GWEI / SLOW_START_EPOCHS as u128;

const _: () = assert!(
    SLOW_START_STEP_GWEI * (SLOW_START_EPOCHS as u128) == BASE_EPOCH_REWARD_GWEI,
    "slow-start step must divide the base reward exactly"
);

/// The era of first zero reward: `BASE >> 43 == 0` (era 42 pays 1 gwei).
pub const FINAL_ERA: u64 = 43;

/// Scheduled reward ceiling for the 0-based epoch index `E` (epochs
/// since network genesis; the epoch at Sova height `H` has index
/// `H − 1`), in gwei.
#[must_use]
pub const fn epoch_reward_gwei(epoch_index: u64) -> u128 {
    if epoch_index < SLOW_START_EPOCHS {
        SLOW_START_STEP_GWEI * (epoch_index as u128 + 1)
    } else {
        let era = epoch_index / ERA_EPOCHS;
        if era >= FINAL_ERA {
            0
        } else {
            BASE_EPOCH_REWARD_GWEI >> era
        }
    }
}

/// Which emission schedule a network runs. Every consumer of an epoch
/// reward (sealer, expectations, validator) must hold the same value —
/// a node whose sealer and validator disagree rejects its own blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// The same reward every epoch: regtest and the box scenarios,
    /// whose exact-mint assertions depend on a constant.
    Flat {
        /// The per-epoch reward, in gwei.
        reward_gwei: u128,
    },
    /// The SIP-3 schedule ([`epoch_reward_gwei`]): slow start, then
    /// halving eras. Mainnet and public testnet.
    Sip3,
}

impl Schedule {
    /// The reward ceiling for a 0-based epoch index, in gwei.
    #[must_use]
    pub const fn reward_gwei(&self, epoch_index: u64) -> u128 {
        match self {
            Self::Flat { reward_gwei } => *reward_gwei,
            Self::Sip3 => epoch_reward_gwei(epoch_index),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GWEI_PER_SOVA: u128 = 1_000_000_000;

    #[test]
    fn ramp_is_exact_and_monotonic() {
        assert_eq!(epoch_reward_gwei(0), SLOW_START_STEP_GWEI);
        assert_eq!(
            epoch_reward_gwei(SLOW_START_EPOCHS - 1),
            BASE_EPOCH_REWARD_GWEI
        );
        assert_eq!(epoch_reward_gwei(SLOW_START_EPOCHS), BASE_EPOCH_REWARD_GWEI);
        let mut prev = 0u128;
        for e in 0..SLOW_START_EPOCHS {
            let r = epoch_reward_gwei(e);
            assert_eq!(r, prev + SLOW_START_STEP_GWEI, "exact step at {e}");
            prev = r;
        }
    }

    #[test]
    fn era_boundaries_halve() {
        assert_eq!(epoch_reward_gwei(ERA_EPOCHS - 1), BASE_EPOCH_REWARD_GWEI);
        assert_eq!(epoch_reward_gwei(ERA_EPOCHS), BASE_EPOCH_REWARD_GWEI / 2);
        assert_eq!(
            epoch_reward_gwei(2 * ERA_EPOCHS),
            BASE_EPOCH_REWARD_GWEI / 4
        );
        // Base = 2^10 × 5^14: exactly divisible through era 10, floored after.
        assert_eq!(
            epoch_reward_gwei(10 * ERA_EPOCHS),
            BASE_EPOCH_REWARD_GWEI >> 10
        );
        assert_eq!(epoch_reward_gwei(42 * ERA_EPOCHS), 1, "era 42 pays 1 gwei");
        assert_eq!(
            epoch_reward_gwei(43 * ERA_EPOCHS),
            0,
            "era 43 ends emission"
        );
        assert_eq!(epoch_reward_gwei(u64::MAX), 0, "no overflow at the far end");
    }

    /// Supply audit: sum the entire schedule and pin the asymptote.
    /// This is the number SIP-3 quotes; if this test's constant ever
    /// needs to change, SIP-3 changed and that is a soft fork.
    #[test]
    fn supply_audit_pins_the_asymptote() {
        // Ramp: exact arithmetic series.
        let ramp: u128 = SLOW_START_STEP_GWEI
            * ((SLOW_START_EPOCHS as u128) * (SLOW_START_EPOCHS as u128 + 1) / 2);
        assert_eq!(ramp, 62_503_125 * GWEI_PER_SOVA);

        // Era 0 after the ramp, then every era to extinction.
        let mut total = ramp + (ERA_EPOCHS - SLOW_START_EPOCHS) as u128 * BASE_EPOCH_REWARD_GWEI;
        for era in 1..FINAL_ERA {
            total += (ERA_EPOCHS as u128) * (BASE_EPOCH_REWARD_GWEI >> era);
        }

        // Below the 21B mark by the slow-start shortfall + halving dust.
        assert!(total < 21_000_000_000 * GWEI_PER_SOVA);
        assert!(total > 20_900_000_000 * GWEI_PER_SOVA);
        // The pinned asymptote: 20,937,503,124.97144 SOVA, in gwei.
        assert_eq!(total, 20_937_503_124_971_440_000);
    }
}
