//! SIP-7 against real Zcash testnet data: the strict follower (value pools
//! required, SIP-7 §3 cross-checks enforced) must emit every block in a
//! range without a hold. Real shielded traffic — Sapling coinbase, Orchard,
//! Ironwood deshields — is what the regtest harness can't provide.
//!
//! Requires a synced testnet zebrad (no cookie auth):
//!
//! ```sh
//! SOVA_TESTNET_RPC=http://127.0.0.1:18234 \
//!   cargo test -p consensus --test sip7_testnet -- --ignored --nocapture
//! ```

use consensus::follower::{Follower, FollowerEvent, ZcashView};
use consensus::zebrad::ZebradClient;

fn client() -> ZebradClient {
    ZebradClient::new(
        std::env::var("SOVA_TESTNET_RPC").unwrap_or_else(|_| "http://127.0.0.1:18234".to_string()),
    )
}

/// Scan `from ..= to` strictly; return (blocks, shielded txs, ironwood txs,
/// SIP-1 burns as (height, txid hex)).
fn scan(from: u64, to: u64) -> (u64, u64, u64, Vec<(u64, String)>) {
    let view = client();
    let mut follower = Follower::new(from, 64).with_strict_pools(true);
    let (mut blocks, mut shielded, mut ironwood) = (0u64, 0u64, 0u64);
    let mut burns = Vec::new();
    let mut next = from;
    while next <= to {
        let events = follower.poll(&view).unwrap_or_else(|e| panic!("poll: {e}"));
        if let Some(why) = follower.hold_reason() {
            panic!("SIP-7 hold at or after {next}: {why}");
        }
        let mut progressed = false;
        for event in events {
            if let FollowerEvent::Epoch(e) = event {
                if e.height > to {
                    break;
                }
                assert!(e.pools.is_some(), "pools present at {}", e.height);
                blocks += 1;
                burns.extend(e.burns.iter().map(|b| (e.height, hex::encode(b.txid))));
                for tx in &e.txs {
                    if let Some(z) = tx.shielded.summary {
                        shielded += 1;
                        if z.ironwood_actions > 0 || z.deltas[3] != 0 {
                            ironwood += 1;
                        }
                    }
                }
                next = e.height + 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    assert!(next > to, "scan stopped at {next} before {to}");
    (blocks, shielded, ironwood, burns)
}

/// The blocks SIP-7 Appendix A documents (Ironwood deshields at 4,384,160).
#[test]
#[ignore = "needs a synced testnet zebrad"]
fn appendix_blocks_pass_the_strict_checks() {
    let (b, s, i, _) = scan(4_384_150, 4_384_210);
    eprintln!("appendix range: {b} blocks, {s} shielded txs, {i} touching Ironwood");
    assert!(
        s > 0 && i > 0,
        "the range has shielded and Ironwood traffic"
    );
}

/// An explicit range, for short chains such as a regtest NU7 activation
/// (`docs/ops/nu7-upgrade.md` 2.3, gap K4): `SOVA_SIP7_FROM` (default 1)
/// to `SOVA_SIP7_TO` (default the tip). Prints one line per block: what
/// the strict follower parsed (pools, transaction versions, shielded
/// summaries), so a hold names its block.
///
/// ```sh
/// SOVA_TESTNET_RPC=http://127.0.0.1:18942 SOVA_SIP7_FROM=1 \
///   cargo test -p consensus --test sip7_testnet -- --ignored --nocapture --exact range_passes_the_strict_checks
/// ```
#[test]
#[ignore = "needs a zebrad; set SOVA_SIP7_FROM / SOVA_SIP7_TO"]
fn range_passes_the_strict_checks() {
    let env = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|v| v.parse::<u64>().unwrap_or_else(|e| panic!("{name}: {e}")))
    };
    let from = env("SOVA_SIP7_FROM").unwrap_or(1);
    let to = env("SOVA_SIP7_TO")
        .unwrap_or_else(|| client().tip_height().unwrap_or_else(|e| panic!("{e}")));
    let view = client();
    for h in from..=to {
        let b = view
            .block_at(h)
            .unwrap_or_else(|e| panic!("block_at {h}: {e}"))
            .unwrap_or_else(|| panic!("no block at {h}"));
        let p = b
            .pools
            .as_deref()
            .unwrap_or_else(|| panic!("{h}: parse_block_pools gave None"));
        let txs: Vec<String> = b
            .txs
            .iter()
            .map(|t| {
                format!(
                    "v{} in={} out={} z={:?}",
                    t.version,
                    t.shielded.n_in,
                    t.outputs.len(),
                    t.shielded.summary.map(|z| (
                        z.deltas,
                        z.sapling_spends,
                        z.sapling_outputs,
                        z.orchard_actions,
                        z.ironwood_actions
                    ))
                )
            })
            .collect();
        eprintln!(
            "{h}: pools {:?} delta {:?} supply {} trees {:?} txs [{}]",
            p.chain_value_zat,
            p.delta_zat,
            p.chain_supply_zat,
            p.trees,
            txs.join("; ")
        );
    }
    let (b, s, i, burns) = scan(from, to);
    eprintln!(
        "range {from}..={to}: {b} blocks, {s} shielded txs, {i} touching Ironwood, burns {burns:?}"
    );
}

/// The latest 200 blocks.
#[test]
#[ignore = "needs a synced testnet zebrad"]
fn recent_blocks_pass_the_strict_checks() {
    let tip = client().tip_height().unwrap_or_else(|e| panic!("{e}"));
    let (b, s, i, _) = scan(tip - 200, tip);
    eprintln!(
        "recent {}..={tip}: {b} blocks, {s} shielded txs, {i} touching Ironwood",
        tip - 200
    );
}
