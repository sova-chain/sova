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

/// Scan `from ..= to` strictly; return (blocks, shielded txs, ironwood txs).
fn scan(from: u64, to: u64) -> (u64, u64, u64) {
    let view = client();
    let mut follower = Follower::new(from, 64).with_strict_pools(true);
    let (mut blocks, mut shielded, mut ironwood) = (0u64, 0u64, 0u64);
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
    (blocks, shielded, ironwood)
}

/// The blocks SIP-7 Appendix A documents (Ironwood deshields at 4,384,160).
#[test]
#[ignore = "needs a synced testnet zebrad"]
fn appendix_blocks_pass_the_strict_checks() {
    let (b, s, i) = scan(4_384_150, 4_384_210);
    eprintln!("appendix range: {b} blocks, {s} shielded txs, {i} touching Ironwood");
    assert!(
        s > 0 && i > 0,
        "the range has shielded and Ironwood traffic"
    );
}

/// The latest 200 blocks.
#[test]
#[ignore = "needs a synced testnet zebrad"]
fn recent_blocks_pass_the_strict_checks() {
    let tip = client().tip_height().unwrap_or_else(|e| panic!("{e}"));
    let (b, s, i) = scan(tip - 200, tip);
    eprintln!(
        "recent {}..={tip}: {b} blocks, {s} shielded txs, {i} touching Ironwood",
        tip - 200
    );
}
