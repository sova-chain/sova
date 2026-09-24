//! The epoch model: aggregation, sealer ranking, and reward shares.
//!
//! One Zcash block = one epoch. Every SIP-1 burn confirmed in the epoch's
//! block ([`crate::sip1::extract_burn`]) makes its EVM address a miner of
//! that epoch. This module holds the three consensus-critical pure
//! functions built on top of that:
//!
//! 1. **Aggregation** — burns aggregate per EVM address; an address's
//!    *weight* is the saturating sum of its burns' zatoshis.
//! 2. **Ranking** — miners are ordered by (weight descending, then the
//!    byte-lexicographically smallest txid among the address's burns,
//!    ascending). Rank 1 is the epoch's primary sealer; ranks 2, 3, …
//!    are the liveness fallbacks. Timing never affects this order —
//!    timeouts decide only when a fallback may *produce*, never which
//!    block is *preferred*.
//! 3. **Rewards** — the epoch reward splits into a pro-rata pool and a
//!    sealer tip. Shares are floor-divided; all rounding dust joins the
//!    tip, so the full reward is always distributed exactly. The output
//!    order (rank order) is the settlement-transaction order in the
//!    sealed Sova block.
//!
//! Everything here is deterministic, total over valid inputs, and free of
//! I/O, floats, and platform-dependent behavior. Failure is expressed as
//! `None` and means "protocol invariants violated" — a block deriving
//! `None` is invalid, full stop.

use crate::sip1::{Burn, SovaRef};

/// Upper bound on an epoch reward, in wei (10^-18 SOVA).
///
/// 2^77 wei ≈ 151,000 SOVA — far above any real schedule. The bound
/// exists for arithmetic safety: total burn weight is physically bounded
/// by the ZEC supply (2.1e15 zatoshis < 2^51), so `pool * weight` stays
/// below 2^128 and 128-bit multiplication cannot overflow.
pub const MAX_EPOCH_REWARD_WEI: u128 = 1 << 77;

/// Sealer tip in basis points of the epoch reward. Draft v0: 10%.
pub const SEALER_TIP_BPS: u128 = 1_000;

/// One recognized burn transaction inside an epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochBurn {
    /// Canonical txid (display byte order) of the Zcash burn transaction.
    pub txid: [u8; 32],
    /// The recognized burn.
    pub burn: Burn,
    /// SIP-8: the Sova block a version-2 burn references (`None` for a v1
    /// burn). Ranking, the mint and the ladder never read it; it matters
    /// only as a vote ([`SovaRef::votes_at`]).
    pub reference: Option<SovaRef>,
}

/// A miner's aggregated standing in one epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinerWeight {
    /// The miner's EVM address.
    pub evm_address: [u8; 20],
    /// Total zatoshis burned by this address in the epoch.
    pub weight_zat: u64,
    /// Byte-lexicographically smallest txid among this address's burns —
    /// the deterministic tie-break key.
    pub min_txid: [u8; 32],
}

/// Aggregate an epoch's burns per EVM address and rank them.
///
/// Returns miners ordered best-first: weight descending, `min_txid`
/// ascending on ties. Rank 1 (index 0) is the primary sealer. The
/// result is empty iff `burns` is empty.
#[must_use]
pub fn rank_miners(burns: &[EpochBurn]) -> Vec<MinerWeight> {
    let mut miners: Vec<MinerWeight> = Vec::new();
    for eb in burns {
        match miners
            .iter_mut()
            .find(|m| m.evm_address == eb.burn.evm_address)
        {
            Some(m) => {
                m.weight_zat = m.weight_zat.saturating_add(eb.burn.value_zat);
                if eb.txid < m.min_txid {
                    m.min_txid = eb.txid;
                }
            }
            None => miners.push(MinerWeight {
                evm_address: eb.burn.evm_address,
                weight_zat: eb.burn.value_zat,
                min_txid: eb.txid,
            }),
        }
    }
    miners.sort_by(|a, b| {
        b.weight_zat
            .cmp(&a.weight_zat)
            .then_with(|| a.min_txid.cmp(&b.min_txid))
    });
    miners
}

/// One address's reward for an epoch, in wei.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewardShare {
    /// Credited EVM address.
    pub evm_address: [u8; 20],
    /// Amount in wei. Never zero in [`epoch_rewards`] output.
    pub amount_wei: u128,
}

/// Split an epoch's reward across ranked miners.
///
/// - `ranked` must be the output of [`rank_miners`] (best-first order).
/// - `sealer` must be the address of the miner that actually sealed the
///   epoch's block; it must appear in `ranked`.
/// - The tip ([`SEALER_TIP_BPS`] of the reward) plus all floor-division
///   dust goes to the sealer; the rest of the reward splits pro-rata by
///   weight.
///
/// Returns shares in rank order (the settlement-transaction order), with
/// the sealer's pro-rata share and tip merged into its single entry and
/// zero-amount entries dropped. The amounts always sum to exactly
/// `epoch_reward_wei`.
///
/// Returns `None` on invariant violations: empty `ranked`, reward above
/// [`MAX_EPOCH_REWARD_WEI`], zero total weight, sealer not present, or
/// any arithmetic overflow.
#[must_use]
pub fn epoch_rewards(
    epoch_reward_wei: u128,
    ranked: &[MinerWeight],
    sealer: [u8; 20],
) -> Option<Vec<RewardShare>> {
    if ranked.is_empty() || epoch_reward_wei > MAX_EPOCH_REWARD_WEI {
        return None;
    }
    if !ranked.iter().any(|m| m.evm_address == sealer) {
        return None;
    }
    let total_weight: u128 = ranked
        .iter()
        .try_fold(0u128, |acc, m| acc.checked_add(u128::from(m.weight_zat)))?;
    if total_weight == 0 {
        return None;
    }

    let tip = epoch_reward_wei
        .checked_mul(SEALER_TIP_BPS)?
        .checked_div(10_000)?;
    let pool = epoch_reward_wei.checked_sub(tip)?;

    let mut shares: Vec<RewardShare> = Vec::with_capacity(ranked.len());
    let mut distributed: u128 = 0;
    for m in ranked {
        let amount = pool
            .checked_mul(u128::from(m.weight_zat))?
            .checked_div(total_weight)?;
        distributed = distributed.checked_add(amount)?;
        shares.push(RewardShare {
            evm_address: m.evm_address,
            amount_wei: amount,
        });
    }
    // Tip plus floor-division dust to the sealer: the reward always
    // distributes exactly.
    let dust = pool.checked_sub(distributed)?;
    let sealer_bonus = tip.checked_add(dust)?;
    for s in &mut shares {
        if s.evm_address == sealer {
            s.amount_wei = s.amount_wei.checked_add(sealer_bonus)?;
            break;
        }
    }
    shares.retain(|s| s.amount_wei > 0);
    Some(shares)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> [u8; 20] {
        [b; 20]
    }

    fn txid(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn eb(txid_byte: u8, addr_byte: u8, value_zat: u64) -> EpochBurn {
        EpochBurn {
            txid: txid(txid_byte),
            burn: Burn {
                evm_address: addr(addr_byte),
                signal_bits: 0,
                value_zat,
            },
            reference: None,
        }
    }

    const REWARD: u128 = 1_000_000_000_000_000_000_000; // 1000 SOVA in wei

    #[test]
    fn ranks_by_weight_then_min_txid() {
        let burns = vec![
            eb(0x30, 3, 500),
            eb(0x10, 1, 2_000),
            eb(0x20, 2, 2_000),
            eb(0x35, 3, 1_500), // addr 3 totals 2_000 too, min_txid 0x30
        ];
        let ranked = rank_miners(&burns);
        assert_eq!(ranked.len(), 3);
        // All tie at 2_000; order by min_txid: 0x10 (addr1) < 0x20 (addr2) < 0x30 (addr3).
        assert_eq!(ranked[0].evm_address, addr(1));
        assert_eq!(ranked[1].evm_address, addr(2));
        assert_eq!(ranked[2].evm_address, addr(3));
        assert_eq!(ranked[2].weight_zat, 2_000);
        assert_eq!(ranked[2].min_txid, txid(0x30));
    }

    #[test]
    fn ranking_is_input_order_independent() {
        let mut burns = vec![
            eb(0x10, 1, 2_000),
            eb(0x20, 2, 9_000),
            eb(0x30, 3, 500),
            eb(0x35, 3, 8_500),
            eb(0x40, 4, 9_000),
        ];
        let baseline = rank_miners(&burns);
        // Rotate through several permutations.
        for _ in 0..burns.len() {
            burns.rotate_left(1);
            assert_eq!(rank_miners(&burns), baseline);
        }
        burns.reverse();
        assert_eq!(rank_miners(&burns), baseline);
    }

    #[test]
    fn rewards_conserve_exactly() {
        // Deliberately awkward weights to force rounding dust.
        let burns = vec![eb(1, 1, 3), eb(2, 2, 7), eb(3, 3, 11), eb(4, 4, 13)];
        let ranked = rank_miners(&burns);
        let sealer = ranked[0].evm_address;
        for reward in [REWARD, 1, 999, 10_001, MAX_EPOCH_REWARD_WEI] {
            let shares = epoch_rewards(reward, &ranked, sealer).unwrap_or_default();
            let total: u128 = shares.iter().map(|s| s.amount_wei).sum();
            assert_eq!(total, reward, "reward {reward} must distribute exactly");
        }
    }

    #[test]
    fn sealer_gets_tip_plus_dust() {
        let burns = vec![eb(1, 1, 600), eb(2, 2, 400)];
        let ranked = rank_miners(&burns);
        // Sealer is rank 2 (fallback sealed this epoch).
        let shares = epoch_rewards(10_000, &ranked, addr(2)).unwrap_or_default();
        // pool = 9000: addr1 5400, addr2 3600; tip 1000 (+0 dust) to addr2.
        assert_eq!(
            shares,
            vec![
                RewardShare {
                    evm_address: addr(1),
                    amount_wei: 5_400
                },
                RewardShare {
                    evm_address: addr(2),
                    amount_wei: 4_600
                },
            ]
        );
    }

    #[test]
    fn rank_order_is_settlement_order() {
        let burns = vec![eb(9, 5, 100_000), eb(1, 6, 1_000)];
        let ranked = rank_miners(&burns);
        let shares = epoch_rewards(REWARD, &ranked, addr(5)).unwrap_or_default();
        assert_eq!(shares[0].evm_address, addr(5));
        assert_eq!(shares[1].evm_address, addr(6));
        assert!(shares[0].amount_wei > shares[1].amount_wei);
    }

    #[test]
    fn invalid_inputs_are_none() {
        let burns = vec![eb(1, 1, 1_000)];
        let ranked = rank_miners(&burns);
        // Empty miner set.
        assert_eq!(epoch_rewards(REWARD, &[], addr(1)), None);
        // Sealer not a miner.
        assert_eq!(epoch_rewards(REWARD, &ranked, addr(9)), None);
        // Reward above the arithmetic-safety bound.
        assert_eq!(
            epoch_rewards(MAX_EPOCH_REWARD_WEI + 1, &ranked, addr(1)),
            None
        );
        // Zero total weight.
        let zero = [MinerWeight {
            evm_address: addr(1),
            weight_zat: 0,
            min_txid: txid(1),
        }];
        assert_eq!(epoch_rewards(REWARD, &zero, addr(1)), None);
    }

    #[test]
    fn tiny_reward_rounds_entirely_to_sealer() {
        // Reward smaller than the miner count: every floor share is 0 and
        // the whole reward lands on the sealer as tip + dust.
        let burns = vec![eb(1, 1, 1_000), eb(2, 2, 1_000), eb(3, 3, 1_000)];
        let ranked = rank_miners(&burns);
        let shares = epoch_rewards(2, &ranked, addr(2)).unwrap_or_default();
        assert_eq!(
            shares,
            vec![RewardShare {
                evm_address: addr(2),
                amount_wei: 2
            }]
        );
    }

    #[test]
    fn max_supply_scale_weights_do_not_overflow() {
        // Total weight at the full ZEC supply scale (2.1e15 zat) with the
        // max epoch reward: must stay well-defined and conserve.
        let burns = vec![
            eb(1, 1, 1_050_000_000_000_000),
            eb(2, 2, 1_050_000_000_000_000),
        ];
        let ranked = rank_miners(&burns);
        let shares = epoch_rewards(MAX_EPOCH_REWARD_WEI, &ranked, addr(1)).unwrap_or_default();
        let total: u128 = shares.iter().map(|s| s.amount_wei).sum();
        assert_eq!(total, MAX_EPOCH_REWARD_WEI);
    }
}
