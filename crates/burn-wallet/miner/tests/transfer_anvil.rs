//! `sova-miner transfer` end to end against a local `anvil` (Foundry).
//!
//! A real `sova-miner init` keystore in a temp dir, its EVM address funded
//! from an anvil dev account, then the real binary's `transfer` -- checked
//! by the recipient's balance moving by exactly the amount, and by the
//! keystore's secret never appearing in the command's output.
//!
//! Needs `anvil` on `PATH`; skipped (with a note) when it is missing, unless
//! `SOVA_REQUIRE_ANVIL=1`, which makes a missing anvil a failure.
//!
//! ```text
//! cd crates/burn-wallet && cargo test -p sova-miner --test transfer_anvil -- --nocapture
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use burn_wallet::rpc::RpcClient;
use serde_json::{Value, json};

const MINER: &str = env!("CARGO_BIN_EXE_sova-miner");

/// A free local port, never one of the laptop's zcash ports (18232-18235).
fn free_port() -> u16 {
    loop {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if !(18_232..=18_235).contains(&port) {
            return port;
        }
    }
}

fn anvil_available() -> bool {
    let found = Command::new("anvil")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !found {
        assert!(
            std::env::var_os("SOVA_REQUIRE_ANVIL").is_none_or(|v| v != "1"),
            "SOVA_REQUIRE_ANVIL=1 but `anvil` is not on PATH"
        );
        eprintln!("skipping: `anvil` (Foundry) not on PATH");
    }
    found
}

/// A running anvil, killed on drop.
struct Anvil {
    child: Child,
    url: String,
}

impl Anvil {
    fn start(chain_id: u64) -> Self {
        let port = free_port();
        let child = Command::new("anvil")
            .args(["--chain-id", &chain_id.to_string()])
            .args(["--port", &port.to_string()])
            .args(["--host", "127.0.0.1", "--silent"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn anvil");
        let anvil = Self {
            child,
            url: format!("http://127.0.0.1:{port}"),
        };
        let rpc = anvil.rpc();
        let deadline = Instant::now() + Duration::from_secs(30);
        while rpc.call_method("eth_chainId", json!([])).is_err() {
            assert!(Instant::now() < deadline, "anvil did not come up");
            std::thread::sleep(Duration::from_millis(100));
        }
        anvil
    }

    fn rpc(&self) -> RpcClient {
        RpcClient::with_timeout(self.url.clone(), Duration::from_secs(10))
    }
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn quantity(v: &Value) -> u128 {
    u128::from_str_radix(v.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

fn balance(rpc: &RpcClient, addr: &str) -> u128 {
    quantity(
        &rpc.call_method("eth_getBalance", json!([addr, "latest"]))
            .unwrap(),
    )
}

fn nonce(rpc: &RpcClient, addr: &str) -> u128 {
    quantity(
        &rpc.call_method("eth_getTransactionCount", json!([addr, "latest"]))
            .unwrap(),
    )
}

fn miner(data_dir: &Path, args: &[&str]) -> Output {
    Command::new(MINER)
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// `init` a keystore in `dir`; returns (its EVM address, its secret hex).
fn init(dir: &Path) -> (String, String) {
    let out = miner(dir, &["init"]);
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let evm = stdout
        .lines()
        .find(|l| l.contains("evm address"))
        .and_then(|l| l.split_whitespace().last())
        .unwrap()
        .to_string();
    let ks: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("keystore.json")).unwrap()).unwrap();
    let secret = ks["secret_key_hex"].as_str().unwrap().to_ascii_lowercase();
    (evm, secret)
}

/// Sends `wei` from anvil's first dev account to `to` and waits for it.
fn fund(rpc: &RpcClient, to: &str, wei: u128) {
    let accounts = rpc.call_method("eth_accounts", json!([])).unwrap();
    let from = accounts[0].as_str().unwrap();
    let hash = rpc
        .call_method(
            "eth_sendTransaction",
            json!([{ "from": from, "to": to, "value": format!("0x{wei:x}") }]),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while rpc
        .call_method("eth_getTransactionReceipt", json!([hash]))
        .unwrap()
        .is_null()
    {
        assert!(Instant::now() < deadline, "funding tx not mined");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(balance(rpc, to), wei);
}

#[test]
fn transfer_moves_exactly_the_amount_on_sova_testnet_chain_id() {
    if !anvil_available() {
        return;
    }
    let anvil = Anvil::start(82_330);
    let rpc = anvil.rpc();
    let dir = tempfile::tempdir().unwrap();
    let (from, secret) = init(dir.path());
    fund(&rpc, &from, 10 * 10u128.pow(18));

    let to = "0x1111111111111111111111111111111111111111";
    let before = balance(&rpc, to);
    let amount = "1.234567890123456789";
    let base = [
        "transfer",
        "--to",
        to,
        "--amount",
        amount,
        "--sova-rpc",
        &anvil.url,
    ];

    // No --yes and no answer on stdin: nothing is sent.
    let out = miner(dir.path(), &base);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("not confirmed"), "{}", text(&out));
    assert_eq!(balance(&rpc, to), before);
    assert_eq!(nonce(&rpc, &from), 0);

    let args: Vec<&str> = base.iter().copied().chain(["--yes"]).collect();
    let out = miner(dir.path(), &args);
    let all = text(&out);
    println!("{all}");
    assert!(out.status.success(), "{all}");
    assert!(all.contains("status:      success"), "{all}");
    assert!(all.contains("chain id:    82330"), "{all}");
    assert_eq!(balance(&rpc, to) - before, 1_234_567_890_123_456_789);
    assert_eq!(nonce(&rpc, &from), 1);
    assert!(
        !all.to_ascii_lowercase().contains(&secret),
        "the secret key leaked into the output"
    );
}

#[test]
fn transfer_refuses_unknown_chain_without_any_chain() {
    if !anvil_available() {
        return;
    }
    // Sova mainnet's reserved chain id.
    let anvil = Anvil::start(8_233);
    let rpc = anvil.rpc();
    let dir = tempfile::tempdir().unwrap();
    let (from, _) = init(dir.path());
    fund(&rpc, &from, 10u128.pow(18));
    let to = "0x2222222222222222222222222222222222222222";
    let args = [
        "transfer",
        "--to",
        to,
        "--amount",
        "0.5",
        "--sova-rpc",
        &anvil.url,
        "--yes",
    ];

    let out = miner(dir.path(), &args);
    assert!(!out.status.success());
    assert!(text(&out).contains("--any-chain"), "{}", text(&out));
    assert_eq!(balance(&rpc, to), 0);
    assert_eq!(nonce(&rpc, &from), 0);

    // With the override it goes through.
    let args: Vec<&str> = args.iter().copied().chain(["--any-chain"]).collect();
    let out = miner(dir.path(), &args);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(balance(&rpc, to), 10u128.pow(18) / 2);
}
