//! SIP-7 §1–§3: Zcash value pools and per-transaction shielded flows, as
//! zebrad 6.3 reports them in the `getblock <hash> 1` and
//! `getrawtransaction <txid> 1` answers the follower already fetches (no new
//! RPC calls).
//!
//! **Pool ids** follow Zebra's `valuePools` order: `0` transparent, `1`
//! Sprout, `2` Sapling, `3` Orchard, `4` lockbox, `5` Ironwood.
//! **Sign convention:** a delta is value *into* the pool, everywhere — a
//! block's `valueDeltaZat`, and `−valueBalance` for a transaction.
//! Only the `…Zat` integers are read, never the float fields.
//!
//! The three cross-checks of SIP-7 §3 ([`check_pools`]) are what makes a
//! plausible-but-wrong zebrad answer a hold instead of a state root: pool
//! continuity, transaction flows summing to the block's shielded deltas,
//! and pools summing to the chain supply.

use serde_json::Value;

use crate::follower::TxView;

/// Number of value pools SIP-7 v1.1 knows (ids `0..POOLS`).
pub const POOLS: usize = 6;

/// Zebra's `valuePools[].id`, in id order.
pub const POOL_IDS: [&str; POOLS] = [
    "transparent",
    "sprout",
    "sapling",
    "orchard",
    "lockbox",
    "ironwood",
];

/// Pool ids of the shielded pools a transaction can move value in, in the
/// order of [`ShieldedSummary::deltas`].
pub const SHIELDED_POOLS: [usize; 4] = [1, 2, 3, 5];

/// Note-commitment tree sizes after a block (cumulative).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TreeSizes {
    /// Sapling tree size.
    pub sapling: u64,
    /// Orchard tree size.
    pub orchard: u64,
    /// Ironwood tree size.
    pub ironwood: u64,
}

/// A block's value pools as zebrad reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockPools {
    /// Each pool's total after the block (`chainValueZat`), by pool id.
    pub chain_value_zat: [u64; POOLS],
    /// Each pool's change in the block (`valueDeltaZat`), by pool id.
    pub delta_zat: [i64; POOLS],
    /// Total supply after the block (`chainSupply.chainValueZat`).
    pub chain_supply_zat: u64,
    /// Note-commitment tree sizes after the block (0 when absent).
    pub trees: TreeSizes,
}

/// A transaction's shielded components (stored only when it has any).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShieldedSummary {
    /// Value into Sprout, Sapling, Orchard, Ironwood ([`SHIELDED_POOLS`]).
    pub deltas: [i64; 4],
    /// Sapling spends.
    pub sapling_spends: u32,
    /// Sapling outputs.
    pub sapling_outputs: u32,
    /// Orchard actions.
    pub orchard_actions: u32,
    /// Ironwood actions.
    pub ironwood_actions: u32,
    /// Sprout JoinSplits.
    pub joinsplits: u32,
}

/// What SIP-7 keeps about one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TxShielded {
    /// Transparent inputs (the coinbase input is not one).
    pub n_in: u32,
    /// The shielded components, when the transaction has any.
    pub summary: Option<ShieldedSummary>,
}

/// A block's activity counters (SIP-7 `blockStats`), summed from its
/// transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockStats {
    /// Transactions, coinbase included.
    pub tx_count: u32,
    /// Transactions with any shielded component, coinbase included.
    pub shielded_tx_count: u32,
    /// Transparent inputs.
    pub t_in: u32,
    /// Transparent outputs.
    pub t_out: u32,
    /// Sapling spends.
    pub sapling_spends: u32,
    /// Sapling outputs.
    pub sapling_outputs: u32,
    /// Orchard actions.
    pub orchard_actions: u32,
    /// Ironwood actions.
    pub ironwood_actions: u32,
    /// Sprout JoinSplits.
    pub joinsplits: u32,
}

/// Why zebrad's pool accounting was refused (SIP-7 §3: hold and alert).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PoolCheckError {
    /// A pool's total moved by something other than its reported delta.
    #[error("pool {pool}: {before} + {delta} != {after} (continuity)")]
    Continuity {
        /// Pool id.
        pool: usize,
        /// Total after the previous block.
        before: u64,
        /// Reported delta.
        delta: i64,
        /// Total after this block.
        after: u64,
    },
    /// The block's shielded delta is not the sum of its transactions'.
    #[error("pool {pool}: transactions move {from_txs}, block reports {reported}")]
    TxSum {
        /// Pool id.
        pool: usize,
        /// Sum over the block's transactions.
        from_txs: i64,
        /// The block's `valueDeltaZat`.
        reported: i64,
    },
    /// The pools don't add up to the chain supply.
    #[error("pools sum to {sum}, chain supply is {supply}")]
    Supply {
        /// Sum of the pools.
        sum: u128,
        /// Reported supply.
        supply: u64,
    },
}

fn int_u64(v: &Value, what: &str) -> Result<u64, String> {
    v.as_u64()
        .ok_or_else(|| format!("{what}: not an unsigned integer"))
}

fn int_i64(v: &Value, what: &str) -> Result<i64, String> {
    v.as_i64().ok_or_else(|| format!("{what}: not an integer"))
}

/// A count given as an array (its length) or a number; absent is 0.
fn count(v: Option<&Value>, what: &str) -> Result<u32, String> {
    match v {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Array(a)) => u32::try_from(a.len()).map_err(|_| format!("{what}: too many")),
        Some(n) => {
            let n = int_u64(n, what)?;
            u32::try_from(n).map_err(|_| format!("{what}: too many"))
        }
    }
}

/// The block's pools from a `getblock <hash> 1` answer. `Ok(None)` when
/// `valuePools` is absent (an older backend; SIP-7 then holds if active).
///
/// # Errors
/// `valuePools` present but not exactly the six known ids in order with
/// integer `chainValueZat` / `valueDeltaZat`, or a malformed `chainSupply`
/// or `trees` entry.
pub fn parse_block_pools(block: &Value) -> Result<Option<BlockPools>, String> {
    let Some(pools) = block.get("valuePools") else {
        return Ok(None);
    };
    let pools = pools.as_array().ok_or("valuePools: not an array")?;
    if pools.len() != POOLS {
        return Err(format!(
            "valuePools: {} entries, expected {POOLS}",
            pools.len()
        ));
    }
    let mut out = BlockPools::default();
    for (i, (pool, id)) in pools.iter().zip(POOL_IDS).enumerate() {
        let got = pool.get("id").and_then(Value::as_str).unwrap_or("");
        if got != id {
            return Err(format!("valuePools[{i}]: id {got:?}, expected {id:?}"));
        }
        let ctx = format!("valuePools.{id}");
        out.chain_value_zat[i] = int_u64(
            pool.get("chainValueZat").unwrap_or(&Value::Null),
            &format!("{ctx}.chainValueZat"),
        )?;
        out.delta_zat[i] = int_i64(
            pool.get("valueDeltaZat").unwrap_or(&Value::Null),
            &format!("{ctx}.valueDeltaZat"),
        )?;
    }
    out.chain_supply_zat = int_u64(
        block
            .get("chainSupply")
            .and_then(|s| s.get("chainValueZat"))
            .unwrap_or(&Value::Null),
        "chainSupply.chainValueZat",
    )?;
    let tree = |name: &str| -> Result<u64, String> {
        match block.get("trees").and_then(|t| t.get(name)) {
            None | Some(Value::Null) => Ok(0),
            Some(t) => int_u64(
                t.get("size").unwrap_or(&Value::Null),
                &format!("trees.{name}.size"),
            ),
        }
    };
    out.trees = TreeSizes {
        sapling: tree("sapling")?,
        orchard: tree("orchard")?,
        ironwood: tree("ironwood")?,
    };
    Ok(Some(out))
}

/// A transaction's shielded flows and counts from a
/// `getrawtransaction <txid> 1` answer. Absent components are zero.
///
/// # Errors
/// A present field with the wrong type.
pub fn parse_tx_shielded(tx: &Value) -> Result<TxShielded, String> {
    let n_in = match tx.get("vin") {
        None | Some(Value::Null) => 0,
        Some(v) => {
            let v = v.as_array().ok_or("vin: not an array")?;
            let n = v.iter().filter(|i| i.get("coinbase").is_none()).count();
            u32::try_from(n).map_err(|_| "vin: too many".to_string())?
        }
    };
    let balance = |v: Option<&Value>, what: &str| -> Result<i64, String> {
        match v {
            None | Some(Value::Null) => Ok(0),
            Some(b) => int_i64(b, what),
        }
    };
    // Sprout: value into the pool is vpub_old − vpub_new per JoinSplit.
    let mut sprout: i64 = 0;
    if let Some(js) = tx.get("vJoinSplit").and_then(Value::as_array) {
        for j in js {
            let old = balance(j.get("vpub_oldZat"), "vJoinSplit.vpub_oldZat")?;
            let new = balance(j.get("vpub_newZat"), "vJoinSplit.vpub_newZat")?;
            sprout = sprout
                .checked_add(old.checked_sub(new).ok_or("vJoinSplit: overflow")?)
                .ok_or("vJoinSplit: overflow")?;
        }
    }
    let neg = |b: i64, what: &str| b.checked_neg().ok_or_else(|| format!("{what}: overflow"));
    let sapling = neg(
        balance(tx.get("valueBalanceZat"), "valueBalanceZat")?,
        "valueBalanceZat",
    )?;
    let orchard = neg(
        balance(
            tx.get("orchard").and_then(|o| o.get("valueBalanceZat")),
            "orchard.valueBalanceZat",
        )?,
        "orchard.valueBalanceZat",
    )?;
    let ironwood = neg(
        balance(
            tx.get("ironwood").and_then(|o| o.get("valueBalanceZat")),
            "ironwood.valueBalanceZat",
        )?,
        "ironwood.valueBalanceZat",
    )?;
    let summary = ShieldedSummary {
        deltas: [sprout, sapling, orchard, ironwood],
        sapling_spends: count(tx.get("vShieldedSpend"), "vShieldedSpend")?,
        sapling_outputs: count(tx.get("vShieldedOutput"), "vShieldedOutput")?,
        orchard_actions: count(
            tx.get("orchard").and_then(|o| o.get("actions")),
            "orchard.actions",
        )?,
        ironwood_actions: count(
            tx.get("ironwood").and_then(|o| o.get("actions")),
            "ironwood.actions",
        )?,
        joinsplits: count(tx.get("vJoinSplit"), "vJoinSplit")?,
    };
    let any = summary.deltas.iter().any(|d| *d != 0)
        || summary.sapling_spends
            + summary.sapling_outputs
            + summary.orchard_actions
            + summary.ironwood_actions
            + summary.joinsplits
            > 0;
    Ok(TxShielded {
        n_in,
        summary: any.then_some(summary),
    })
}

/// The block's `blockStats` counters, summed from its transactions.
#[must_use]
pub fn block_stats(txs: &[TxView]) -> BlockStats {
    let mut s = BlockStats {
        tx_count: u32::try_from(txs.len()).unwrap_or(u32::MAX),
        ..BlockStats::default()
    };
    for tx in txs {
        s.t_in = s.t_in.saturating_add(tx.shielded.n_in);
        s.t_out = s
            .t_out
            .saturating_add(u32::try_from(tx.outputs.len()).unwrap_or(u32::MAX));
        if let Some(z) = tx.shielded.summary {
            s.shielded_tx_count = s.shielded_tx_count.saturating_add(1);
            s.sapling_spends = s.sapling_spends.saturating_add(z.sapling_spends);
            s.sapling_outputs = s.sapling_outputs.saturating_add(z.sapling_outputs);
            s.orchard_actions = s.orchard_actions.saturating_add(z.orchard_actions);
            s.ironwood_actions = s.ironwood_actions.saturating_add(z.ironwood_actions);
            s.joinsplits = s.joinsplits.saturating_add(z.joinsplits);
        }
    }
    s
}

/// SIP-7 §3's cross-checks, before an epoch is emitted: (1) each pool's
/// total is its previous total plus its delta (skipped without `prev`),
/// (2) the block's shielded deltas equal the sum of its transactions'
/// flows, (3) the pools sum to the chain supply.
///
/// # Errors
/// The first check that fails.
pub fn check_pools(
    prev: Option<&BlockPools>,
    block: &BlockPools,
    txs: &[TxView],
) -> Result<(), PoolCheckError> {
    if let Some(prev) = prev {
        for pool in 0..POOLS {
            let before = prev.chain_value_zat[pool];
            let delta = block.delta_zat[pool];
            let after = block.chain_value_zat[pool];
            if i128::from(before) + i128::from(delta) != i128::from(after) {
                return Err(PoolCheckError::Continuity {
                    pool,
                    before,
                    delta,
                    after,
                });
            }
        }
    }
    for (slot, &pool) in SHIELDED_POOLS.iter().enumerate() {
        let from_txs: i128 = txs
            .iter()
            .filter_map(|t| t.shielded.summary)
            .map(|z| i128::from(z.deltas[slot]))
            .sum();
        let reported = block.delta_zat[pool];
        if from_txs != i128::from(reported) {
            return Err(PoolCheckError::TxSum {
                pool,
                from_txs: i64::try_from(from_txs).unwrap_or(i64::MAX),
                reported,
            });
        }
    }
    let sum: u128 = block.chain_value_zat.iter().map(|v| u128::from(*v)).sum();
    if sum != u128::from(block.chain_supply_zat) {
        return Err(PoolCheckError::Supply {
            sum,
            supply: block.chain_supply_zat,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Testnet 4,384,200 (coinbase only), from SIP-7 Appendix A: pools sum
    /// exactly to the chain supply.
    fn testnet_4384200() -> Value {
        json!({
            "height": 4_384_200u64,
            "chainSupply": {"chainValue": 18_231_006.378_350_43, "chainValueZat": 1_823_100_637_835_043u64},
            "valuePools": [
                {"id": "transparent", "chainValueZat": 1_573_837_835_978_306u64, "valueDeltaZat": 12_500_000},
                {"id": "sprout", "chainValueZat": 42_832_983_037_484u64, "valueDeltaZat": 0},
                {"id": "sapling", "chainValueZat": 152_869_428_798_703u64, "valueDeltaZat": 0},
                {"id": "orchard", "chainValueZat": 23_913_312_221_154u64, "valueDeltaZat": 0},
                {"id": "lockbox", "chainValueZat": 15_894_393_750_000u64, "valueDeltaZat": 18_750_000},
                {"id": "ironwood", "chainValueZat": 13_752_684_049_396u64, "valueDeltaZat": 125_000_000}
            ],
            "trees": {"sapling": {"size": 404_304}, "orchard": {"size": 248_902}, "ironwood": {"size": 354_039}}
        })
    }

    fn tx(summary_json: Value) -> TxView {
        TxView {
            txid: [0; 32],
            version: 6,
            outputs: Vec::new(),
            shielded: parse_tx_shielded(&summary_json).unwrap_or_else(|e| panic!("{e}")),
        }
    }

    #[test]
    fn a_real_testnet_block_parses_and_adds_up() {
        let pools = parse_block_pools(&testnet_4384200())
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|| panic!("pools present"));
        assert_eq!(pools.chain_value_zat[5], 13_752_684_049_396);
        assert_eq!(
            pools.delta_zat,
            [12_500_000, 0, 0, 0, 18_750_000, 125_000_000]
        );
        assert_eq!(pools.trees.ironwood, 354_039);
        // No shielded transactions in the block's tx list here, so the
        // Ironwood coinbase delta (125,000,000) is its coinbase's: the
        // tx-sum check needs that coinbase, supplied below.
        let coinbase = tx(json!({
            "vin": [{"coinbase": "03"}],
            "ironwood": {"actions": [{}, {}], "valueBalanceZat": -125_000_000}
        }));
        assert_eq!(check_pools(None, &pools, &[coinbase]), Ok(()));
    }

    /// Regtest zebrad 6.3 (captured 2026-09-23): every shielded pool 0,
    /// `trees: {}`.
    #[test]
    fn the_regtest_shape_parses() {
        let block = json!({
            "chainSupply": {"chainValueZat": 1_250_000_000u64},
            "valuePools": [
                {"id": "transparent", "chainValueZat": 1_250_000_000u64, "valueDeltaZat": 625_000_000},
                {"id": "sprout", "chainValueZat": 0, "valueDeltaZat": 0},
                {"id": "sapling", "chainValueZat": 0, "valueDeltaZat": 0},
                {"id": "orchard", "chainValueZat": 0, "valueDeltaZat": 0},
                {"id": "lockbox", "chainValueZat": 0, "valueDeltaZat": 0},
                {"id": "ironwood", "chainValueZat": 0, "valueDeltaZat": 0}
            ],
            "trees": {}
        });
        let pools = parse_block_pools(&block).unwrap_or_else(|e| panic!("{e}"));
        let pools = pools.unwrap_or_else(|| panic!("present"));
        assert_eq!(pools.trees, TreeSizes::default());
        let prev = BlockPools {
            chain_value_zat: [625_000_000, 0, 0, 0, 0, 0],
            chain_supply_zat: 625_000_000,
            ..BlockPools::default()
        };
        assert_eq!(check_pools(Some(&prev), &pools, &[]), Ok(()));
    }

    #[test]
    fn malformed_pools_are_refused_and_absent_ones_are_none() {
        assert_eq!(parse_block_pools(&json!({"height": 1})), Ok(None));
        let mut b = testnet_4384200();
        if let Some(a) = b["valuePools"].as_array_mut() {
            a.swap(2, 3);
        }
        assert!(parse_block_pools(&b).is_err(), "order is fixed");
        let mut b = testnet_4384200();
        b["valuePools"][0]["chainValueZat"] = json!(1.5);
        assert!(parse_block_pools(&b).is_err(), "integers only");
        let mut b = testnet_4384200();
        if let Some(a) = b["valuePools"].as_array_mut() {
            a.push(json!({"id": "future"}));
        }
        assert!(parse_block_pools(&b).is_err(), "an unknown pool holds");
    }

    /// Testnet 4,384,160 (SIP-7 Appendix A): Sapling +125,035,000 from the
    /// coinbase, Ironwood −1,035,000 from two transactions.
    #[test]
    fn transaction_flows_sum_to_the_block_deltas() {
        let txs = [
            tx(
                json!({"vin": [{"coinbase": "03"}], "valueBalanceZat": -125_035_000,
                      "vShieldedOutput": [{}], "orchard": {"actions": [], "valueBalanceZat": 0}}),
            ),
            tx(
                json!({"vin": [], "vout": [], "ironwood": {"actions": 2, "valueBalanceZat": 10_000}}),
            ),
            tx(json!({"vin": [], "ironwood": {"actions": 4, "valueBalanceZat": 1_025_000}})),
        ];
        assert_eq!(
            txs[0].shielded.n_in, 0,
            "a coinbase input is not a transparent input"
        );
        let z0 = txs[0].shielded.summary.unwrap_or_default();
        assert_eq!(z0.deltas, [0, 125_035_000, 0, 0]);
        assert_eq!(z0.sapling_outputs, 1);
        assert_eq!(txs[2].shielded.summary.map(|z| z.ironwood_actions), Some(4));
        let block = BlockPools {
            delta_zat: [13_500_000, 0, 125_035_000, 0, 18_750_000, -1_035_000],
            chain_value_zat: [13_500_000, 0, 125_035_000, 0, 18_750_000, 0],
            chain_supply_zat: 13_500_000 + 125_035_000 + 18_750_000,
            ..BlockPools::default()
        };
        // Supply check aside (synthetic totals), the flows must match.
        assert!(matches!(
            check_pools(None, &block, &txs),
            Ok(()) | Err(PoolCheckError::Supply { .. })
        ));
        let stats = block_stats(&txs);
        assert_eq!(
            (
                stats.tx_count,
                stats.shielded_tx_count,
                stats.ironwood_actions
            ),
            (3, 3, 6)
        );
        // Drop one transaction: the Ironwood sum no longer matches.
        assert!(matches!(
            check_pools(None, &block, &txs[..2]),
            Err(PoolCheckError::TxSum { pool: 5, .. })
        ));
    }

    #[test]
    fn continuity_and_supply_are_checked() {
        let prev = BlockPools {
            chain_value_zat: [100, 0, 50, 0, 0, 0],
            chain_supply_zat: 150,
            ..BlockPools::default()
        };
        let next = BlockPools {
            chain_value_zat: [110, 0, 50, 0, 0, 0],
            delta_zat: [10, 0, 0, 0, 0, 0],
            chain_supply_zat: 160,
            ..BlockPools::default()
        };
        assert_eq!(check_pools(Some(&prev), &next, &[]), Ok(()));
        let jumped = BlockPools {
            chain_value_zat: [111, 0, 50, 0, 0, 0],
            chain_supply_zat: 161,
            ..next
        };
        assert!(matches!(
            check_pools(Some(&prev), &jumped, &[]),
            Err(PoolCheckError::Continuity { pool: 0, .. })
        ));
        let short = BlockPools {
            chain_supply_zat: 159,
            ..next
        };
        assert!(matches!(
            check_pools(Some(&prev), &short, &[]),
            Err(PoolCheckError::Supply { .. })
        ));
    }
}
