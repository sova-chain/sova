//! SIP-4 §4 gas benchmark: blocks filled to the gas limit with Zcash query
//! precompile calls, executed through the real [`SovaEvmFactory`] EVM over
//! a real [`ZcashIndex`] populated at realistic scale.
//!
//! Results and the pricing analysis: `docs/design/sip4-gas-bench.md`.
//!
//! ```text
//! CARGO_TARGET_DIR=... cargo test --release -p engine --test sip4_gas_bench \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Knobs (env): `SOVA_BENCH_BLOCKS` (Zcash blocks indexed, default
//! 100,000), `SOVA_BENCH_TXS` (txs per block, default 20),
//! `SOVA_BENCH_GAS_LIMIT` (Sova block gas limit, default 30,000,000 — the
//! dev/testnet genesis `gasLimit` 0x1c9c380, which `EthereumBuilderConfig::
//! new()` in `crates/engine/src/builder.rs` keeps as its target),
//! `SOVA_BENCH_REPS` (timed repetitions per scenario, default 5).
//!
//! What one "block" is: a fresh EVM from `SovaEvmFactory::create_evm` (as
//! reth makes one per block), then transactions to a loop contract, each at
//! most the Osaka EIP-7825 cap (2^24 gas; Osaka is active at genesis in the
//! dev hardfork schedule), until their gas limits sum to the block gas
//! limit. The loop contract `STATICCALL`s `0x5A00` until its remaining gas
//! is below one more call — the cheapest-overhead attacker shape
//! (`retSize = 0`, no memory growth). "hot" repeats one key; "distinct"
//! walks a calldata list of keys spread across the whole index (the
//! calldata costs the attacker 512 gas per key, which is counted).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use consensus::follower::{EpochData, TxOut, TxView};
use consensus::sip1::{BurnPayload, burn_lock_script};
use engine::zcash_index::ZcashIndex;
use evm::zcash::{
    ANCHOR_SELECTOR, BLOCK_AT_SELECTOR, BURN_INFO_SELECTOR, SovaEvmFactory, TX_INFO_SELECTOR,
    TX_OUTPUT_SELECTOR, ZCASH_QUERY, ZcashSource, status,
};
use reth_ethereum::evm::{
    primitives::{Evm, EvmEnv, EvmFactory},
    revm::{
        bytecode::Bytecode,
        context::{BlockEnv, CfgEnv, TxEnv},
        context_interface::result::ExecutionResult,
        db::{CacheDB, EmptyDB},
        primitives::{Address, Bytes, TxKind, U256, address, hardfork::SpecId},
        state::AccountInfo,
    },
};

const BASE: u64 = 3_100_000;
const CALLER: Address = address!("0x00000000000000000000000000000000000c0ffe");
const LOOPER: Address = address!("0x000000000000000000000000000000000000100b");
/// EIP-7825 (Osaka) per-transaction gas cap.
const TX_CAP: u64 = 1 << 24;
/// Zcash's 2 MB block bounds one transaction's transparent outputs: at 34
/// wire bytes per P2PKH output (8 value + 1 length + 25 script) that is
/// ~58,800. The pathological "many outputs" transaction.
const FAT_MANY_OUTPUTS: usize = 58_000;
/// The pathological "big scripts" transaction: 190 × 10,000-byte scripts
/// (~1.9 MB).
const FAT_BIG_OUTPUTS: usize = 190;

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.replace('_', "").parse().ok())
        .unwrap_or(default)
}

// ---------------------------------------------------------------- fixture

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn bytes32(seed: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, chunk) in out.chunks_mut(8).enumerate() {
        chunk.copy_from_slice(&splitmix(seed.wrapping_mul(4).wrapping_add(i as u64)).to_le_bytes());
    }
    out
}

fn txid(block: u64, j: u64) -> [u8; 32] {
    bytes32((block << 20) | j)
}

fn p2pkh(tag: u64) -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend_from_slice(&bytes32(tag ^ 0x5eed)[..20]);
    s.extend_from_slice(&[0x88, 0xac]);
    s
}

fn out(value_zat: u64, script: Vec<u8>) -> TxOut {
    TxOut { value_zat, script }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Special {
    None,
    Big,
    Burn,
    FatMany,
    FatBig,
}

struct Fixture {
    index: Arc<ZcashIndex>,
    /// Anchored height of the benchmark's Sova block: the index tip.
    e: u64,
    sova_block: u64,
    normal: Vec<[u8; 32]>,
    big: Vec<[u8; 32]>,
    burns: Vec<[u8; 32]>,
    fat_many: Vec<[u8; 32]>,
    fat_big: Vec<[u8; 32]>,
    n_txs: u64,
    n_outputs: u64,
    script_bytes: u64,
}

fn normal_tx(block: u64, j: u64) -> TxView {
    // Coinbase: two outputs. Every 4th tx fully shielded (no transparent
    // outputs). The rest: payment + change, P2PKH.
    let outputs = if j == 0 {
        vec![
            out(312_500_000, p2pkh(block)),
            out(62_500_000, p2pkh(block + 1)),
        ]
    } else if j.is_multiple_of(4) {
        Vec::new()
    } else {
        vec![
            out(1_000 + j, p2pkh(block ^ j)),
            out(7_000 + j, p2pkh(block ^ (j << 8))),
        ]
    };
    TxView {
        txid: txid(block, j),
        version: 5,
        outputs,
    }
}

fn special_tx(kind: Special, block: u64) -> TxView {
    let outputs = match kind {
        Special::None => unreachable!(),
        Special::Big => vec![out(1_000, vec![0x51; 10_000])],
        Special::Burn => vec![
            out(
                0,
                BurnPayload {
                    evm_address: [0xab; 20],
                    signal_bits: 1,
                }
                .to_script()
                .to_vec(),
            ),
            out(90_000_000, burn_lock_script().to_vec()),
        ],
        Special::FatMany => (0..FAT_MANY_OUTPUTS as u64)
            .map(|k| out(1_000 + k, p2pkh(block ^ (k << 24))))
            .collect(),
        Special::FatBig => (0..FAT_BIG_OUTPUTS as u64)
            .map(|k| out(1_000 + k, vec![0x51; 10_000]))
            .collect(),
    };
    TxView {
        txid: txid(block, 1),
        version: 5,
        outputs,
    }
}

fn epoch(height: u64, txs: Vec<TxView>) -> EpochData {
    EpochData {
        height,
        hash: bytes32(height ^ 0xb10c),
        burns: Vec::new(),
        time: 1_700_000_000 + (height * 75) as u32,
        txs,
    }
}

fn build_fixture(n_blocks: u64, txs_per_block: u64) -> Fixture {
    let index = Arc::new(ZcashIndex::with_base(BASE));
    let mut f = Fixture {
        index: Arc::clone(&index),
        e: BASE + n_blocks - 1,
        sova_block: n_blocks,
        normal: Vec::new(),
        big: Vec::new(),
        burns: Vec::new(),
        fat_many: Vec::new(),
        fat_big: Vec::new(),
        n_txs: 0,
        n_outputs: 0,
        script_bytes: 0,
    };
    let every = |k: u64| (n_blocks / k).max(1);
    for i in 0..n_blocks {
        let h = BASE + i;
        let kind = if i % every(4) == 29 % every(4) {
            Special::FatMany
        } else if i % every(4) == 31 % every(4) {
            Special::FatBig
        } else if i % every(64) == 7 % every(64) {
            Special::Big
        } else if i % every(256) == 13 % every(256) {
            Special::Burn
        } else {
            Special::None
        };
        let mut txs = Vec::with_capacity(txs_per_block as usize);
        for j in 0..txs_per_block {
            let tx = if j == 1 && kind != Special::None {
                special_tx(kind, h)
            } else {
                normal_tx(h, j)
            };
            f.n_txs += 1;
            f.n_outputs += tx.outputs.len() as u64;
            f.script_bytes += tx
                .outputs
                .iter()
                .map(|o| o.script.len() as u64)
                .sum::<u64>();
            txs.push(tx);
        }
        match kind {
            Special::None => {}
            Special::Big => f.big.push(txid(h, 1)),
            Special::Burn => f.burns.push(txid(h, 1)),
            Special::FatMany => f.fat_many.push(txid(h, 1)),
            Special::FatBig => f.fat_big.push(txid(h, 1)),
        }
        index.insert(&epoch(h, txs));
    }
    // Hit pool: random normal payments (j not coinbase, not shielded, not
    // the special slot), spread over the whole index.
    let mut s = 0xfeed_u64;
    while f.normal.len() < 8_192 {
        s = splitmix(s);
        let block = BASE + s % n_blocks;
        let j = 2 + (s >> 32) % (txs_per_block - 2);
        if !j.is_multiple_of(4) {
            f.normal.push(txid(block, j));
        }
    }
    assert_eq!(index.indexed_through(), Some(f.e));
    f
}

/// Physical footprint in KiB: macOS `footprint` (counts compressed and
/// swapped dirty pages, which RSS drops), else `ps` RSS.
fn rss_kib() -> u64 {
    let pid = std::process::id().to_string();
    let fp = std::process::Command::new("footprint")
        .args(["-f", "bytes", "-p", &pid])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            let rest = &s[s.find("Footprint: ")? + 11..];
            rest.split_whitespace().next()?.parse::<u64>().ok()
        });
    if let Some(bytes) = fp {
        return bytes / 1024;
    }
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

// --------------------------------------------------------------- the EVM

fn word_u64(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

/// The loop contract. Stack `[cnt, ptr]`; memory `[0, q)` holds the query.
/// `distinct`: before each call, `mem[4..36] = calldata[ptr]`, `ptr += 32`,
/// wrapping to `q` at the end of calldata.
fn loop_code(q: u8, distinct: bool, target: u16, threshold: u32) -> Bytecode {
    let mut c = vec![0x60, q, 0x60, 0x00, 0x60, 0x00, 0x37]; // CALLDATACOPY(0,0,q)
    c.extend([0x60, q, 0x60, 0x00]); // ptr = q, cnt = 0
    let top = c.len() as u8;
    c.push(0x5b); // JUMPDEST
    if distinct {
        c.extend([0x90, 0x80, 0x35, 0x60, 0x04, 0x52]); // SWAP1 DUP1 CALLDATALOAD mstore(4, w)
        c.extend([0x60, 0x20, 0x01]); // ptr += 32
        c.extend([0x80, 0x36, 0x11]); // CALLDATASIZE > ptr ?
        let fix = c.len() + 1;
        c.extend([0x60, 0x00, 0x57]); // JUMPI cont
        c.extend([0x50, 0x60, q]); // POP; ptr = q
        let cont = c.len() as u8;
        c.push(0x5b);
        c[fix] = cont;
        c.push(0x90); // SWAP1
    }
    // STATICCALL(gas, target, 0, q, 0, 0); cnt += ok
    c.extend([0x60, 0x00, 0x60, 0x00, 0x60, q, 0x60, 0x00, 0x61]);
    c.extend(target.to_be_bytes());
    c.extend([0x5a, 0xfa, 0x01]);
    // while gas > threshold
    c.push(0x62);
    c.extend(&threshold.to_be_bytes()[1..]);
    c.extend([0x5a, 0x11, 0x60, top, 0x57]);
    // return cnt
    c.extend([0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3]);
    assert!(c.len() < 256);
    Bytecode::new_raw(Bytes::from(c))
}

fn evm_env(sova_block: u64, gas_limit: u64) -> EvmEnv {
    let block = BlockEnv {
        number: U256::from(sova_block),
        gas_limit,
        ..Default::default()
    };
    EvmEnv::new(CfgEnv::new_with_spec(SpecId::OSAKA), block)
}

fn base_db(code: Bytecode) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(CALLER, AccountInfo::from_balance(U256::from(u64::MAX)));
    db.insert_account_info(LOOPER, AccountInfo::from_bytecode(code));
    db
}

fn tx_env(to: Address, data: Bytes, gas_limit: u64, nonce: u64) -> TxEnv {
    TxEnv {
        caller: CALLER,
        kind: TxKind::Call(to),
        data,
        gas_limit,
        nonce,
        ..Default::default()
    }
}

#[derive(Clone)]
struct Scenario {
    name: String,
    method: &'static str,
    /// Precompile gas per call (the draft price, incl. per-byte).
    price: u64,
    target: u16,
    template: Vec<u8>,
    list: Vec<[u8; 32]>,
    /// Expected first word of the answer to `template` with `list[0]`.
    expect: u64,
}

fn calldata(sel: [u8; 4], words: &[[u8; 32]]) -> Vec<u8> {
    let mut d = sel.to_vec();
    for w in words {
        d.extend_from_slice(w);
    }
    d
}

struct BlockRun {
    elapsed: Duration,
    calls: u64,
    gas_used: u64,
}

fn run_block(factory: &SovaEvmFactory, f: &Fixture, sc: &Scenario, gas_limit: u64) -> BlockRun {
    let q = sc.template.len() as u8;
    let distinct = !sc.list.is_empty();
    let threshold = u32::try_from((sc.price + 200) * 65 / 64 + 3_000).unwrap();
    let code = loop_code(q, distinct, sc.target, threshold);
    let mut data = sc.template.clone();
    if distinct {
        // Enough keys to never repeat within one transaction where the
        // pool allows, without paying for keys that are never used; a
        // short pool wraps inside the EVM.
        let per_tx = (TX_CAP.min(gas_limit) / (sc.price + 700)) as usize + 1;
        for w in sc.list.iter().take(per_tx) {
            data.extend_from_slice(w);
        }
    }
    let data = Bytes::from(data);
    let db = base_db(code);

    let start = Instant::now();
    let mut evm = factory.create_evm(db, evm_env(f.sova_block, gas_limit));
    let (mut remaining, mut nonce, mut calls, mut gas_used) = (gas_limit, 0, 0, 0);
    while remaining > 100_000 {
        let tx_gas = remaining.min(TX_CAP);
        let res = evm
            .transact_commit(tx_env(LOOPER, data.clone(), tx_gas, nonce))
            .expect("loop tx executes");
        let ExecutionResult::Success { .. } = &res else {
            panic!("{}: loop tx failed: {res:?}", sc.name);
        };
        let out = res.output().expect("output");
        calls += U256::from_be_slice(&out[..32]).to::<u64>();
        gas_used += res.tx_gas_used();
        remaining -= tx_gas;
        nonce += 1;
    }
    let elapsed = start.elapsed();
    black_box(evm);
    BlockRun {
        elapsed,
        calls,
        gas_used,
    }
}

/// One direct call with the first key: the answer must have the expected
/// status (so the loop measures what its name says).
fn check_answer(factory: &SovaEvmFactory, f: &Fixture, sc: &Scenario) {
    if sc.target != 0x5a00 {
        return;
    }
    let mut d = sc.template.clone();
    if let Some(k) = sc.list.first() {
        d[4..36].copy_from_slice(k);
    }
    let mut evm = factory.create_evm(base_db(Bytecode::new()), evm_env(f.sova_block, 30_000_000));
    let res = evm
        .transact(tx_env(ZCASH_QUERY, Bytes::from(d), 1_000_000, 0))
        .expect("direct call executes");
    assert!(res.result.is_success(), "{}: {:?}", sc.name, res.result);
    let out = res.result.output().expect("output");
    let w0 = U256::from_be_slice(&out[..32]).to::<u64>();
    assert_eq!(w0, sc.expect, "{}: unexpected status/answer", sc.name);
}

struct Row {
    name: String,
    method: &'static str,
    price: u64,
    calls: u64,
    gas_per_call: f64,
    min_ms: f64,
    med_ms: f64,
    max_ms: f64,
}

fn measure(factory: &SovaEvmFactory, f: &Fixture, sc: &Scenario, gas_limit: u64, reps: u64) -> Row {
    check_answer(factory, f, sc);
    let warm = run_block(factory, f, sc, gas_limit);
    let mut times = vec![];
    let slow = warm.elapsed > Duration::from_secs(2);
    let reps = if slow { 1 } else { reps };
    let mut last = warm;
    for _ in 0..reps {
        last = run_block(factory, f, sc, gas_limit);
        times.push(last.elapsed.as_secs_f64() * 1e3);
    }
    times.sort_by(f64::total_cmp);
    let row = Row {
        name: sc.name.clone(),
        method: sc.method,
        price: sc.price,
        calls: last.calls,
        gas_per_call: last.gas_used as f64 / last.calls.max(1) as f64,
        min_ms: times[0],
        med_ms: times[times.len() / 2],
        max_ms: times[times.len() - 1],
    };
    println!(
        "  {:<44} calls {:>7}  gas/call {:>8.0}  block ms min {:>9.1} med {:>9.1} max {:>9.1}  ns/call {:>10.0}",
        row.name,
        row.calls,
        row.gas_per_call,
        row.min_ms,
        row.med_ms,
        row.max_ms,
        row.med_ms * 1e6 / row.calls.max(1) as f64
    );
    row
}

// One scenario per line: a table reads better than rustfmt's layout.
#[rustfmt::skip]
fn scenarios(f: &Fixture) -> Vec<Scenario> {
    let txo = |id: [u8; 32]| calldata(TX_OUTPUT_SELECTOR, &[id, word_u64(0)]);
    let misses: Vec<[u8; 32]> = (0..8_192u64).map(|k| bytes32(0xdead_0000_0000 + k)).collect();
    let heights: Vec<[u8; 32]> = (0..8_192u64)
        .map(|k| word_u64(BASE + splitmix(k ^ 0x4e16) % (f.e - BASE + 1)))
        .collect();
    let n0 = f.normal[0];
    let m0 = misses[0];
    let sc = |name: &str,
              method: &'static str,
              price: u64,
              template: Vec<u8>,
              list: Vec<[u8; 32]>,
              expect: u64| Scenario {
        name: name.to_string(),
        method,
        price,
        target: 0x5a00,
        template,
        list,
        expect,
    };
    let ok = u64::from(status::OK);
    let nf = u64::from(status::NOT_FOUND);
    let p2pkh_gas = 4_000 + 8 * 25;
    let big_gas = 4_000 + 8 * 10_000;
    vec![
        Scenario {
            name: "baseline: identity (0x04), 36 B".into(),
            method: "-",
            price: 21,
            target: 0x0004,
            template: calldata(TX_INFO_SELECTOR, &[n0]),
            list: vec![],
            expect: 0,
        },
        sc("anchor", "anchor", 200, ANCHOR_SELECTOR.to_vec(), vec![], f.e),
        sc("blockAt hit, hot", "blockAt", 2_600, calldata(BLOCK_AT_SELECTOR, &[word_u64(f.e - 7)]), vec![], ok),
        sc("blockAt hit, distinct", "blockAt", 2_600, calldata(BLOCK_AT_SELECTOR, &[heights[0]]), heights.clone(), ok),
        sc("blockAt NOT_YET", "blockAt", 2_600, calldata(BLOCK_AT_SELECTOR, &[word_u64(f.e + 1_000)]), vec![], u64::from(status::NOT_YET)),
        sc("txInfo hit, hot", "txInfo", 4_000, calldata(TX_INFO_SELECTOR, &[n0]), vec![], ok),
        sc("txInfo hit, distinct", "txInfo", 4_000, calldata(TX_INFO_SELECTOR, &[n0]), f.normal.clone(), ok),
        sc("txInfo miss, hot", "txInfo", 4_000, calldata(TX_INFO_SELECTOR, &[m0]), vec![], nf),
        sc("txInfo miss, distinct", "txInfo", 4_000, calldata(TX_INFO_SELECTOR, &[m0]), misses.clone(), nf),
        sc("txOutput hit 25 B P2PKH, hot", "txOutput", p2pkh_gas, txo(n0), vec![], ok),
        sc("txOutput hit 25 B P2PKH, distinct", "txOutput", p2pkh_gas, txo(n0), f.normal.clone(), ok),
        sc("txOutput hit 10,000 B, hot", "txOutput", big_gas, txo(f.big[0]), vec![], ok),
        sc("txOutput hit 10,000 B, distinct", "txOutput", big_gas, txo(f.big[0]), f.big.clone(), ok),
        sc("txOutput miss, hot", "txOutput", 4_000, txo(m0), vec![], nf),
        sc("txOutput miss, distinct", "txOutput", 4_000, txo(m0), misses.clone(), nf),
        sc("burnInfo burn, hot", "burnInfo", 4_000, calldata(BURN_INFO_SELECTOR, &[f.burns[0]]), vec![], ok),
        sc("burnInfo burn, distinct", "burnInfo", 4_000, calldata(BURN_INFO_SELECTOR, &[f.burns[0]]), f.burns.clone(), ok),
        sc("burnInfo not-a-burn, hot", "burnInfo", 4_000, calldata(BURN_INFO_SELECTOR, &[n0]), vec![], u64::from(status::NOT_A_BURN)),
        sc("burnInfo not-a-burn, distinct", "burnInfo", 4_000, calldata(BURN_INFO_SELECTOR, &[n0]), f.normal.clone(), u64::from(status::NOT_A_BURN)),
        sc("burnInfo miss, hot", "burnInfo", 4_000, calldata(BURN_INFO_SELECTOR, &[m0]), vec![], nf),
        sc("burnInfo miss, distinct", "burnInfo", 4_000, calldata(BURN_INFO_SELECTOR, &[m0]), misses, nf),
        // Pathological: flat-priced calls on transactions whose outputs
        // are large. `tx()` clones every output on every call.
        sc("PATH txInfo on 10,000 B-script tx", "txInfo", 4_000, calldata(TX_INFO_SELECTOR, &[f.big[0]]), vec![], ok),
        sc("PATH txInfo on 58k-output tx", "txInfo", 4_000, calldata(TX_INFO_SELECTOR, &[f.fat_many[0]]), vec![], ok),
        sc("PATH txOutput(vout 0) on 58k-output tx", "txOutput", p2pkh_gas, txo(f.fat_many[0]), vec![], ok),
        sc("PATH burnInfo on 58k-output tx", "burnInfo", 4_000, calldata(BURN_INFO_SELECTOR, &[f.fat_many[0]]), vec![], u64::from(status::NOT_A_BURN)),
        sc("PATH txInfo on 190 x 10,000 B tx", "txInfo", 4_000, calldata(TX_INFO_SELECTOR, &[f.fat_big[0]]), vec![], ok),
        sc("PATH burnInfo on 190 x 10,000 B tx", "burnInfo", 4_000, calldata(BURN_INFO_SELECTOR, &[f.fat_big[0]]), vec![], u64::from(status::NOT_A_BURN)),
    ]
}

// ------------------------------------------------------------ the bench

#[test]
#[ignore = "benchmark: run in release with --ignored --nocapture (docs/design/sip4-gas-bench.md)"]
fn sip4_gas_bench() {
    let n_blocks = env_u64("SOVA_BENCH_BLOCKS", 100_000);
    let txs_per_block = env_u64("SOVA_BENCH_TXS", 20);
    let gas_limit = env_u64("SOVA_BENCH_GAS_LIMIT", 30_000_000);
    let reps = env_u64("SOVA_BENCH_REPS", 5);
    let only = std::env::var("SOVA_BENCH_ONLY").ok();

    // ---- index at scale + memory
    let rss0 = rss_kib();
    let t = Instant::now();
    let f = build_fixture(n_blocks, txs_per_block);
    let build = t.elapsed();
    let rss1 = rss_kib();
    let fat_bytes = (f.fat_many.len() * FAT_MANY_OUTPUTS * 25
        + f.fat_big.len() * FAT_BIG_OUTPUTS * 10_000) as u64;
    println!("\n== SIP-4 gas bench ==");
    println!(
        "index: {n_blocks} blocks x {txs_per_block} txs = {} txs, {} outputs, {:.1} MB scripts \
         (of which {:.1} MB in the {} pathological fat txs); built in {:.2} s",
        f.n_txs,
        f.n_outputs,
        f.script_bytes as f64 / 1e6,
        fat_bytes as f64 / 1e6,
        f.fat_many.len() + f.fat_big.len(),
        build.as_secs_f64()
    );
    let delta = (rss1.saturating_sub(rss0)) * 1024;
    println!(
        "index memory (phys footprint) delta: {:.1} MB  => {:.0} B/tx, {:.0} B/block ({} -> {} KiB)",
        delta as f64 / 1e6,
        delta as f64 / f.n_txs as f64,
        delta as f64 / n_blocks as f64,
        rss0,
        rss1
    );
    println!(
        "gas limit {gas_limit}, tx cap {TX_CAP}, sova block {} -> anchored E = {}",
        f.sova_block, f.e
    );

    // ---- direct source micro-benchmarks (no EVM): the clone cost
    println!("\n-- ZcashSource::tx() direct (clones IndexedTx) --");
    let src: &dyn ZcashSource = &*f.index;
    let micro = |label: &str, keys: &[[u8; 32]], iters: u64| {
        let t = Instant::now();
        for i in 0..iters {
            black_box(src.tx(black_box(&keys[(i as usize) % keys.len()])));
        }
        let ns = t.elapsed().as_nanos() as f64 / iters as f64;
        println!("  {label:<40} {ns:>12.0} ns/call");
        ns
    };
    micro("normal tx (2 x 25 B), hot", &f.normal[..1], 2_000_000);
    micro("normal tx, distinct (8,192 keys)", &f.normal, 2_000_000);
    let misses: Vec<[u8; 32]> = (0..8_192u64)
        .map(|k| bytes32(0xdead_0000_0000 + k))
        .collect();
    micro("miss, distinct", &misses, 2_000_000);
    micro("10,000 B-script tx", &f.big[..1], 500_000);
    micro("58k-output tx", &f.fat_many[..1], 200);
    micro("190 x 10,000 B tx", &f.fat_big[..1], 2_000);
    let t = Instant::now();
    for _ in 0..2_000_000 {
        black_box(src.indexed_through());
    }
    println!(
        "  {:<40} {:>12.0} ns/call",
        "indexed_through() (read lock only)",
        t.elapsed().as_nanos() as f64 / 2e6
    );

    // ---- blocks through the EVM
    let factory = SovaEvmFactory::with_zcash_source(Arc::clone(&f.index) as Arc<dyn ZcashSource>);
    println!("\n-- gas-limit blocks through SovaEvmFactory (reps {reps}) --");
    let mut rows = Vec::new();
    for sc in scenarios(&f) {
        if only.as_deref().is_some_and(|o| !sc.name.contains(o)) {
            continue;
        }
        rows.push(measure(&factory, &f, &sc, gas_limit, reps));
    }

    // ---- lock contention
    if only.as_deref().is_none_or(|o| o.contains("contention")) {
        println!("\n-- RwLock contention (txInfo hit, distinct) --");
        let sc = scenarios(&f)
            .into_iter()
            .find(|s| s.name == "txInfo hit, distinct")
            .unwrap();
        rows.push(measure(&factory, &f, &sc, gas_limit, reps));
        for (label, readers, writer) in [
            ("+3 reader threads (RPC eth_call load)", 3, None),
            (
                "+writer: insert/unwind a 20-tx block, looping",
                0,
                Some(false),
            ),
            (
                "+writer: insert/unwind a 58k-output block, looping",
                0,
                Some(true),
            ),
        ] {
            let stop = Arc::new(AtomicBool::new(false));
            let ops = Arc::new(AtomicU64::new(0));
            let mut handles = Vec::new();
            for r in 0..readers {
                let (idx, stop, ops, keys) = (
                    Arc::clone(&f.index),
                    Arc::clone(&stop),
                    Arc::clone(&ops),
                    f.normal.clone(),
                );
                handles.push(std::thread::spawn(move || {
                    let mut i = r * 7;
                    while !stop.load(Ordering::Relaxed) {
                        black_box(idx.indexed_through());
                        black_box(idx.tx(&keys[i % keys.len()]));
                        i += 1;
                        ops.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }
            if let Some(fat) = writer {
                let (idx, stop, ops, e) = (
                    Arc::clone(&f.index),
                    Arc::clone(&stop),
                    Arc::clone(&ops),
                    f.e,
                );
                handles.push(std::thread::spawn(move || {
                    let h = e + 1;
                    let txs: Vec<TxView> = (0..20)
                        .map(|j| {
                            let mut t = if fat && j == 1 {
                                special_tx(Special::FatMany, h)
                            } else {
                                normal_tx(h, j)
                            };
                            t.txid = bytes32(0x77_0000_0000 + j);
                            t
                        })
                        .collect();
                    let ep = epoch(h, txs);
                    while !stop.load(Ordering::Relaxed) {
                        idx.insert(&ep);
                        idx.unwind_above(e);
                        ops.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }
            let mut s2 = sc.clone();
            s2.name = format!("contention {label}");
            let t = Instant::now();
            rows.push(measure(&factory, &f, &s2, gas_limit, reps));
            let secs = t.elapsed().as_secs_f64();
            stop.store(true, Ordering::Relaxed);
            for h in handles {
                h.join().unwrap();
            }
            println!(
                "    background ops: {:.0}/s",
                ops.load(Ordering::Relaxed) as f64 / secs
            );
        }
    }

    // ---- summary (markdown)
    let baseline_ns = rows
        .iter()
        .find(|r| r.method == "-")
        .map(|r| r.med_ms * 1e6 / r.calls as f64);
    println!(
        "\n| scenario | price | calls/block | gas/call | block ms (min / med / max) | ns/call (med) |"
    );
    println!("|---|---:|---:|---:|---:|---:|");
    for r in &rows {
        println!(
            "| {} | {} | {} | {:.0} | {:.1} / {:.1} / {:.1} | {:.0} |",
            r.name,
            r.price,
            r.calls,
            r.gas_per_call,
            r.min_ms,
            r.med_ms,
            r.max_ms,
            r.med_ms * 1e6 / r.calls.max(1) as f64
        );
    }
    if let Some(b) = baseline_ns {
        println!("\nEVM loop baseline (identity precompile): {b:.0} ns/call");
    }
}
