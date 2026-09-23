//! Live integration test for the follower against the regtest harness
//! (C2a acceptance): epoch streaming, a forced reorg via
//! `invalidateblock`, and rescan determinism — all against a real Zebra.
//!
//! Requires the box/regtest stack to be up (see
//! `box/regtest/follower-e2e.sh`, which drives this test):
//!
//! ```sh
//! cargo test -p consensus --test follower_regtest -- --ignored
//! ```

use consensus::follower::{Follower, FollowerEvent, ZcashView};
use consensus::zebrad::ZebradClient;
use serde_json::{Value, json};

fn rpc_url() -> String {
    std::env::var("SOVA_REGTEST_RPC").unwrap_or_else(|_| "http://127.0.0.1:18232".to_string())
}

/// Raw JSON-RPC for test orchestration (generate/invalidateblock) — kept
/// separate from the production client on purpose: the test drives the
/// chain through a different door than the code under test observes it.
fn raw(method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc":"2.0","id":"t","method":method,"params":params});
    let resp = ureq::post(&rpc_url())
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .unwrap_or_else(|e| panic!("rpc {method} transport failed: {e}"));
    let text = resp
        .into_string()
        .unwrap_or_else(|e| panic!("rpc {method} body read failed: {e}"));
    let v: Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("rpc {method} bad json: {e}"));
    assert!(
        v.get("error").is_none_or(Value::is_null),
        "rpc {method} returned error: {v}"
    );
    v.get("result").cloned().unwrap_or(Value::Null)
}

fn generate(n: u32) {
    let r = raw("generate", json!([n]));
    assert!(
        r.is_array() || r.is_null(),
        "unexpected generate result: {r}"
    );
}

fn epochs(events: &[FollowerEvent]) -> Vec<(u64, [u8; 32])> {
    events
        .iter()
        .filter_map(|e| match e {
            FollowerEvent::Epoch(ep) => Some((ep.height, ep.hash)),
            FollowerEvent::Rollback { .. } => None,
        })
        .collect()
}

#[test]
#[ignore = "requires the box/regtest stack; run via box/regtest/follower-e2e.sh"]
fn follower_regtest_reorg() {
    let client = ZebradClient::new(rpc_url());

    // Baseline: make sure a few blocks exist.
    generate(3);
    let tip = client.tip_height().unwrap_or_else(|e| panic!("tip: {e}"));
    assert!(tip >= 3, "expected at least 3 blocks, tip={tip}");

    // Scan everything from height 1.
    let mut follower = Follower::new(1, 50);
    let initial = follower
        .poll(&client)
        .unwrap_or_else(|e| panic!("poll: {e}"));
    let initial_epochs = epochs(&initial);
    assert_eq!(
        initial_epochs.len() as u64,
        tip,
        "one epoch per block up to the tip"
    );
    assert_eq!(initial_epochs[0].0, 1, "epochs start at base height");
    assert!(
        initial_epochs.windows(2).all(|w| w[1].0 == w[0].0 + 1),
        "epoch heights are contiguous"
    );
    // Idle poll is empty.
    assert!(
        follower
            .poll(&client)
            .unwrap_or_else(|e| panic!("poll: {e}"))
            .is_empty()
    );

    // Force a reorg: invalidate the current tip block, then mine a
    // replacement chain two blocks long.
    let old_tip_hash = initial_epochs
        .last()
        .map(|&(_, h)| h)
        .unwrap_or_else(|| panic!("no epochs"));
    let tip_hash_hex = raw("getblockhash", json!([tip]));
    let tip_hash_hex = tip_hash_hex
        .as_str()
        .unwrap_or_else(|| panic!("getblockhash not a string"));
    raw("invalidateblock", json!([tip_hash_hex]));
    generate(2);

    let after = follower
        .poll(&client)
        .unwrap_or_else(|e| panic!("poll after reorg: {e}"));
    assert!(
        matches!(after.first(), Some(FollowerEvent::Rollback { to_height }) if *to_height == tip - 1),
        "first event must be Rollback to tip-1, got {:?}",
        after.first()
    );
    let new_epochs = epochs(&after);
    assert_eq!(
        new_epochs.iter().map(|&(h, _)| h).collect::<Vec<_>>(),
        vec![tip, tip + 1],
        "replacement epochs cover the reorged tip and the new block"
    );
    assert_ne!(
        new_epochs[0].1, old_tip_hash,
        "replacement block at the old tip height has a new hash"
    );

    // Determinism: a fresh follower over the final chain must agree with
    // the continued follower for every height.
    let mut fresh = Follower::new(1, 50);
    let fresh_all = epochs(
        &fresh
            .poll(&client)
            .unwrap_or_else(|e| panic!("fresh poll: {e}")),
    );
    let mut continued: Vec<(u64, [u8; 32])> = initial_epochs
        .into_iter()
        .filter(|&(h, _)| h < tip)
        .collect();
    continued.extend(new_epochs);
    assert_eq!(fresh_all, continued, "rescan equals continued view");

    println!(
        "FOLLOWER REGTEST PASSED: {} epochs, rollback to {}, deterministic rescan",
        fresh_all.len(),
        tip - 1
    );
}
