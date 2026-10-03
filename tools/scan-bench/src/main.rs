//! `scan-bench`: time the expectations follower's Zcash rescan against a
//! zebrad, and the alternatives the fast-restart design weighs
//! (`docs/design/fast-restart.md`).
//!
//! Read-only: it only ever calls `getblockcount`, `getblockhash`,
//! `getblock` and `getrawtransaction`.
//!
//! ```text
//! scan-bench --rpc http://127.0.0.1:18234 --from 4388500 --count 2000 --mode follower
//! ```
//!
//! Modes (`--mode`, comma-separated to run several over the same range):
//!
//! - `follower`   the real `consensus::follower::Follower` over the real
//!   `consensus::zebrad::ZebradClient`, one `poll()` (what `run_expectations`
//!   does on start), the tip capped at `from + count - 1`.
//! - `breakdown`  the same RPC sequence as `ZebradClient::block_at`
//!   (`getblockhash`, `getblock <hash> 1`, one `getrawtransaction <txid> 1`
//!   per tx), each call timed: transport vs JSON parse, per method.
//! - `parallel:K` `ZebradClient::block_at` on K threads (height stripes).
//! - `batch:K`    JSON-RPC batch arrays, K heights per round trip
//!   (hashes, then blocks, then every tx of the K blocks).
//! - `v2` / `v2par:K`  `getblock <height> 2` (decoded txs inline): one
//!   call per block, sequential or on K threads.
//!
//! - `replay`     fetch the range into memory (8 threads), then time one
//!   `Follower::poll()` over the in-memory blocks: the CPU floor of a
//!   restart that replays a local block cache instead of calling zebrad,
//!   plus the compact on-disk size such a cache would need.
//!
//! `--verify` makes `v2` also fetch each block the production way and
//! assert the parsed `BlockView`s are identical (the consensus-parity
//! precondition for switching RPCs).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use consensus::follower::{
    BlockView, Follower, FollowerEvent, TxOut, TxView, ViewError, ZcashView,
};
use consensus::zebrad::ZebradClient;
use serde_json::{Value, json};

struct Args {
    rpc: String,
    from: u64,
    count: u64,
    modes: Vec<String>,
    strict_pools: bool,
    verify: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut rpc = "http://127.0.0.1:18234".to_string();
    let mut from = None;
    let mut count = 1000;
    let mut modes = vec!["follower".to_string()];
    let mut strict_pools = false;
    let mut verify = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{a}: missing value"));
        match a.as_str() {
            "--rpc" => rpc = val()?,
            "--from" => from = Some(val()?.parse().map_err(|e| format!("--from: {e}"))?),
            "--count" => count = val()?.parse().map_err(|e| format!("--count: {e}"))?,
            "--mode" => modes = val()?.split(',').map(str::to_string).collect(),
            "--strict-pools" => strict_pools = true,
            "--verify" => verify = true,
            "-h" | "--help" => {
                return Err("usage: scan-bench --rpc URL [--from H] [--count N] \
                    [--mode follower,breakdown,parallel:K,batch:K,v2,v2par:K] \
                    [--strict-pools] [--verify]  (--from defaults to tip - count + 1)"
                    .to_string());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let from = match from {
        Some(f) => f,
        None => {
            let tip = ZebradClient::new(rpc.clone())
                .tip_height()
                .map_err(|e| e.to_string())?;
            tip.saturating_sub(count).saturating_add(1)
        }
    };
    Ok(Args {
        rpc,
        from,
        count,
        modes,
        strict_pools,
        verify,
    })
}

// --- raw JSON-RPC with timing --------------------------------------------

/// Per-method timing accumulator.
#[derive(Default, Clone)]
struct MethodStats {
    calls: u64,
    transport: Vec<Duration>,
    parse: Duration,
    bytes: u64,
}

#[derive(Default)]
struct Stats {
    by_method: std::collections::BTreeMap<String, MethodStats>,
}

impl Stats {
    fn record(&mut self, method: &str, transport: Duration, parse: Duration, bytes: usize) {
        let m = self.by_method.entry(method.to_string()).or_default();
        m.calls += 1;
        m.transport.push(transport);
        m.parse += parse;
        m.bytes += bytes as u64;
    }

    fn print(&self, blocks: u64, wall: Duration) {
        println!(
            "  {:<22} {:>8} {:>9} {:>9} {:>8} {:>8} {:>8} {:>9} {:>10}",
            "method",
            "calls",
            "per-blk",
            "total s",
            "mean ms",
            "p50 ms",
            "p99 ms",
            "parse s",
            "KB/call"
        );
        for (name, m) in &self.by_method {
            let mut t = m.transport.clone();
            t.sort_unstable();
            let total: Duration = t.iter().sum();
            let pct = |p: f64| {
                t.get(((t.len() as f64 - 1.0) * p).round() as usize)
                    .map_or(0.0, |d| d.as_secs_f64() * 1e3)
            };
            println!(
                "  {:<22} {:>8} {:>9.2} {:>9.2} {:>8.3} {:>8.3} {:>8.3} {:>9.2} {:>10.1}",
                name,
                m.calls,
                m.calls as f64 / blocks.max(1) as f64,
                total.as_secs_f64(),
                total.as_secs_f64() * 1e3 / m.calls.max(1) as f64,
                pct(0.5),
                pct(0.99),
                m.parse.as_secs_f64(),
                m.bytes as f64 / 1024.0 / m.calls.max(1) as f64,
            );
        }
        let rpc: Duration = self
            .by_method
            .values()
            .map(|m| m.transport.iter().sum::<Duration>() + m.parse)
            .sum();
        println!(
            "  wall {:.2} s; rpc+parse {:.2} s ({:.0}%); other (block assembly) {:.2} s",
            wall.as_secs_f64(),
            rpc.as_secs_f64(),
            100.0 * rpc.as_secs_f64() / wall.as_secs_f64().max(1e-9),
            (wall.saturating_sub(rpc)).as_secs_f64()
        );
    }
}

struct Raw {
    url: String,
    agent: ureq::Agent,
}

impl Raw {
    fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            agent: ureq::Agent::new(),
        }
    }

    /// One call; returns `result` (None on a JSON-RPC error) and records timing.
    fn call(
        &self,
        stats: &mut Stats,
        method: &str,
        params: Value,
    ) -> Result<Option<Value>, String> {
        let body = json!({"jsonrpc": "2.0", "id": "bench", "method": method, "params": params});
        let label = match (method, params.get(1)) {
            ("getblock", Some(v)) => format!("getblock v{v}"),
            _ => method.to_string(),
        };
        let t0 = Instant::now();
        let text = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(|e| format!("{method}: {e}"))?
            .into_string()
            .map_err(|e| format!("{method}: {e}"))?;
        let t1 = Instant::now();
        let parsed: Value = serde_json::from_str(&text).map_err(|e| format!("{method}: {e}"))?;
        let t2 = Instant::now();
        stats.record(&label, t1 - t0, t2 - t1, text.len());
        if !parsed.get("error").is_none_or(Value::is_null) {
            return Ok(None);
        }
        Ok(parsed.get("result").cloned())
    }

    /// A JSON-RPC batch; returns each item's `result` in request order.
    fn batch(
        &self,
        stats: &mut Stats,
        label: &str,
        calls: &[(&str, Value)],
    ) -> Result<Vec<Option<Value>>, String> {
        let body: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, (m, p))| json!({"jsonrpc": "2.0", "id": i, "method": m, "params": p}))
            .collect();
        let t0 = Instant::now();
        let text = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .send_string(&Value::Array(body).to_string())
            .map_err(|e| format!("batch {label}: {e}"))?
            .into_string()
            .map_err(|e| format!("batch {label}: {e}"))?;
        let t1 = Instant::now();
        let parsed: Value = serde_json::from_str(&text).map_err(|e| format!("batch: {e}"))?;
        let t2 = Instant::now();
        stats.record(&format!("batch {label}"), t1 - t0, t2 - t1, text.len());
        let arr = parsed
            .as_array()
            .ok_or_else(|| format!("batch {label}: not an array answer: {:.200}", text))?;
        let mut out = vec![None; calls.len()];
        for item in arr {
            let id = item.get("id").and_then(Value::as_u64).unwrap_or(u64::MAX) as usize;
            if id < out.len() && item.get("error").is_none_or(Value::is_null) {
                out[id] = item.get("result").cloned();
            }
        }
        Ok(out)
    }
}

/// `TxView` from a decoded transaction object (`getrawtransaction <txid> 1`
/// or an element of `getblock <h> 2`'s `tx`), parsed exactly like
/// `ZebradClient::block_at` parses it.
fn tx_view(tx: &Value) -> Result<TxView, String> {
    let txid_hex = tx
        .get("txid")
        .and_then(Value::as_str)
        .ok_or("tx: no txid")?;
    let version = tx
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or("tx: version")?;
    let mut outputs = Vec::new();
    for out in tx
        .get("vout")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let value_zat = out
            .get("valueZat")
            .and_then(Value::as_u64)
            .ok_or("vout: valueZat")?;
        let script_hex = out
            .get("scriptPubKey")
            .and_then(|s| s.get("hex"))
            .and_then(Value::as_str)
            .ok_or("vout: scriptPubKey.hex")?;
        outputs.push(TxOut {
            value_zat,
            script: hex::decode(script_hex).map_err(|e| e.to_string())?,
        });
    }
    Ok(TxView {
        txid: hash32(txid_hex)?,
        version,
        outputs,
        shielded: consensus::pools::parse_tx_shielded(tx).unwrap_or_default(),
    })
}

fn hash32(s: &str) -> Result<[u8; 32], String> {
    hex::decode(s)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "hash not 32 bytes".to_string())
}

fn block_view(block: &Value, height: u64, txs: Vec<TxView>) -> Result<BlockView, String> {
    let hash = hash32(
        block
            .get("hash")
            .and_then(Value::as_str)
            .ok_or("block: hash")?,
    )?;
    let prev_hash = match block.get("previousblockhash").and_then(Value::as_str) {
        Some(p) => hash32(p)?,
        None => [0; 32],
    };
    let time = block
        .get("time")
        .and_then(Value::as_u64)
        .and_then(|t| u32::try_from(t).ok())
        .ok_or("block: time")?;
    Ok(BlockView {
        height,
        hash,
        prev_hash,
        time,
        txs,
        pools: consensus::pools::parse_block_pools(block)
            .ok()
            .flatten()
            .map(Box::new),
    })
}

// --- modes ------------------------------------------------------------------

/// `ZcashView` over the real client with the tip capped and calls counted.
struct Capped {
    inner: ZebradClient,
    cap: u64,
    block_calls: AtomicU64,
    txs: AtomicU64,
}

impl ZcashView for Capped {
    fn tip_height(&self) -> Result<u64, ViewError> {
        Ok(self.inner.tip_height()?.min(self.cap))
    }
    fn block_at(&self, height: u64) -> Result<Option<BlockView>, ViewError> {
        self.block_calls.fetch_add(1, Ordering::Relaxed);
        let b = self.inner.block_at(height)?;
        if let Some(b) = &b {
            self.txs.fetch_add(b.txs.len() as u64, Ordering::Relaxed);
        }
        Ok(b)
    }
}

fn report(mode: &str, blocks: u64, txs: u64, wall: Duration) {
    let bps = blocks as f64 / wall.as_secs_f64().max(1e-9);
    println!(
        "RESULT mode={mode} blocks={blocks} txs={txs} tx_per_block={:.2} wall_s={:.2} blocks_per_s={:.1} ms_per_block={:.3} \
         proj_70k_s={:.0} proj_100k_s={:.0} proj_250k_s={:.0}",
        txs as f64 / blocks.max(1) as f64,
        wall.as_secs_f64(),
        bps,
        1e3 / bps.max(1e-9),
        70_000.0 / bps,
        100_000.0 / bps,
        250_000.0 / bps,
    );
}

fn mode_follower(a: &Args) -> Result<(), String> {
    let view = Capped {
        inner: ZebradClient::new(a.rpc.clone()),
        cap: a.from + a.count - 1,
        block_calls: AtomicU64::new(0),
        txs: AtomicU64::new(0),
    };
    // REORG_WINDOW as in crates/engine/src/expectations.rs.
    let mut f = Follower::new(a.from, 1024).with_strict_pools(a.strict_pools);
    let t0 = Instant::now();
    let events = f.poll(&view).map_err(|e| e.to_string())?;
    let wall = t0.elapsed();
    let epochs = events
        .iter()
        .filter(|e| matches!(e, FollowerEvent::Epoch(_)))
        .count() as u64;
    let burns: usize = events
        .iter()
        .map(|e| match e {
            FollowerEvent::Epoch(ep) => ep.burns.len(),
            FollowerEvent::Rollback { .. } => 0,
        })
        .sum();
    if let Some(why) = f.hold_reason() {
        println!("  follower held: {why}");
    }
    println!(
        "  follower: one poll() emitted {epochs} epochs ({burns} burns) in {:.2} s, block_at calls {}",
        wall.as_secs_f64(),
        view.block_calls.load(Ordering::Relaxed)
    );
    // The memory the follower held until the poll returned (events vec).
    let held_bytes: usize = events
        .iter()
        .map(|e| match e {
            FollowerEvent::Epoch(ep) => {
                ep.txs
                    .iter()
                    .map(|t| 64 + t.outputs.iter().map(|o| 32 + o.script.len()).sum::<usize>())
                    .sum::<usize>()
                    + 200
            }
            FollowerEvent::Rollback { .. } => 16,
        })
        .sum();
    println!(
        "  events held in memory until poll() returned: ~{:.1} MB (rough lower bound)",
        held_bytes as f64 / 1e6
    );
    report("follower", epochs, view.txs.load(Ordering::Relaxed), wall);
    Ok(())
}

fn mode_breakdown(a: &Args) -> Result<(), String> {
    let raw = Raw::new(&a.rpc);
    let mut stats = Stats::default();
    let mut txs = 0u64;
    let mut max_txs = 0usize;
    let t0 = Instant::now();
    for h in a.from..a.from + a.count {
        let hash = raw
            .call(&mut stats, "getblockhash", json!([h]))?
            .and_then(|v| v.as_str().map(str::to_string))
            .ok_or_else(|| format!("no block at {h}"))?;
        let block = raw
            .call(&mut stats, "getblock", json!([hash, 1]))?
            .ok_or("getblock: error")?;
        let ids = block
            .get("tx")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        max_txs = max_txs.max(ids.len());
        let mut views = Vec::with_capacity(ids.len());
        for id in &ids {
            let tx = raw
                .call(&mut stats, "getrawtransaction", json!([id, 1]))?
                .ok_or("getrawtransaction: error")?;
            views.push(tx_view(&tx)?);
        }
        txs += views.len() as u64;
        let _ = block_view(&block, h, views)?;
    }
    let wall = t0.elapsed();
    println!("  max txs in one block: {max_txs}");
    stats.print(a.count, wall);
    report("breakdown", a.count, txs, wall);
    Ok(())
}

fn mode_parallel(a: &Args, k: u64) -> Result<(), String> {
    let next = Arc::new(AtomicU64::new(a.from));
    let txs = Arc::new(AtomicU64::new(0));
    let end = a.from + a.count;
    let t0 = Instant::now();
    let handles: Vec<_> = (0..k)
        .map(|_| {
            let (next, txs, url) = (next.clone(), txs.clone(), a.rpc.clone());
            std::thread::spawn(move || -> Result<(), String> {
                let c = ZebradClient::new(url);
                loop {
                    let h = next.fetch_add(1, Ordering::Relaxed);
                    if h >= end {
                        return Ok(());
                    }
                    let b = c
                        .block_at(h)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("no block at {h}"))?;
                    txs.fetch_add(b.txs.len() as u64, Ordering::Relaxed);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().map_err(|_| "thread panicked".to_string())??;
    }
    report(
        &format!("parallel:{k}"),
        a.count,
        txs.load(Ordering::Relaxed),
        t0.elapsed(),
    );
    Ok(())
}

fn mode_batch(a: &Args, k: u64) -> Result<(), String> {
    let raw = Raw::new(&a.rpc);
    let mut stats = Stats::default();
    let mut txs = 0u64;
    let t0 = Instant::now();
    let mut h = a.from;
    let end = a.from + a.count;
    while h < end {
        let hi = (h + k).min(end);
        let heights: Vec<u64> = (h..hi).collect();
        let hashes = raw.batch(
            &mut stats,
            "getblockhash",
            &heights
                .iter()
                .map(|x| ("getblockhash", json!([x])))
                .collect::<Vec<_>>(),
        )?;
        let hashes: Vec<String> = hashes
            .into_iter()
            .map(|v| {
                v.and_then(|v| v.as_str().map(str::to_string))
                    .ok_or("batch: no hash")
            })
            .collect::<Result<_, _>>()?;
        let blocks = raw.batch(
            &mut stats,
            "getblock v1",
            &hashes
                .iter()
                .map(|x| ("getblock", json!([x, 1])))
                .collect::<Vec<_>>(),
        )?;
        let blocks: Vec<Value> = blocks
            .into_iter()
            .map(|b| b.ok_or("batch: no block"))
            .collect::<Result<_, _>>()?;
        let ids: Vec<Value> = blocks
            .iter()
            .flat_map(|b| {
                b.get("tx")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        let tx_answers = raw.batch(
            &mut stats,
            "getrawtransaction",
            &ids.iter()
                .map(|x| ("getrawtransaction", json!([x, 1])))
                .collect::<Vec<_>>(),
        )?;
        let mut views = tx_answers
            .into_iter()
            .map(|t| {
                t.ok_or_else(|| "batch: no tx".to_string())
                    .and_then(|t| tx_view(&t))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter();
        for (i, b) in blocks.iter().enumerate() {
            let n = b.get("tx").and_then(Value::as_array).map_or(0, Vec::len);
            let mine: Vec<TxView> = views.by_ref().take(n).collect();
            txs += mine.len() as u64;
            let _ = block_view(b, heights[i], mine)?;
        }
        h = hi;
    }
    let wall = t0.elapsed();
    stats.print(a.count, wall);
    report(&format!("batch:{k}"), a.count, txs, wall);
    Ok(())
}

/// One `getblock <height> 2` call per block, parsed to a `BlockView`.
fn fetch_v2(raw: &Raw, stats: &mut Stats, h: u64) -> Result<BlockView, String> {
    let block = raw
        .call(stats, "getblock", json!([h.to_string(), 2]))?
        .ok_or_else(|| format!("getblock {h} 2: error"))?;
    let txs = block
        .get("tx")
        .and_then(Value::as_array)
        .ok_or("getblock 2: tx")?
        .iter()
        .map(|t| {
            if t.is_string() {
                Err(
                    "getblock 2 returned txids, not decoded txs (verbosity 2 unsupported)"
                        .to_string(),
                )
            } else {
                tx_view(t)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    block_view(&block, h, txs)
}

fn mode_v2(a: &Args) -> Result<(), String> {
    let raw = Raw::new(&a.rpc);
    let prod = ZebradClient::new(a.rpc.clone());
    let mut stats = Stats::default();
    let mut txs = 0u64;
    let mut checked = 0u64;
    let mut t_fetch = Duration::ZERO;
    for h in a.from..a.from + a.count {
        let t = Instant::now();
        let b = fetch_v2(&raw, &mut stats, h)?;
        t_fetch += t.elapsed();
        txs += b.txs.len() as u64;
        if a.verify {
            let p = prod
                .block_at(h)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no block at {h}"))?;
            if p != b {
                return Err(format!(
                    "PARITY MISMATCH at {h}: getblock 2 parse differs from ZebradClient::block_at"
                ));
            }
            checked += 1;
        }
    }
    stats.print(a.count, t_fetch);
    if a.verify {
        println!(
            "  parity: {checked}/{} blocks identical to ZebradClient::block_at",
            a.count
        );
    }
    report("v2", a.count, txs, t_fetch);
    Ok(())
}

fn mode_v2par(a: &Args, k: u64) -> Result<(), String> {
    let next = Arc::new(AtomicU64::new(a.from));
    let txs = Arc::new(AtomicU64::new(0));
    let end = a.from + a.count;
    let t0 = Instant::now();
    let handles: Vec<_> = (0..k)
        .map(|_| {
            let (next, txs, url) = (next.clone(), txs.clone(), a.rpc.clone());
            std::thread::spawn(move || -> Result<(), String> {
                let raw = Raw::new(&url);
                let mut stats = Stats::default();
                loop {
                    let h = next.fetch_add(1, Ordering::Relaxed);
                    if h >= end {
                        return Ok(());
                    }
                    let b = fetch_v2(&raw, &mut stats, h)?;
                    txs.fetch_add(b.txs.len() as u64, Ordering::Relaxed);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().map_err(|_| "thread panicked".to_string())??;
    }
    report(
        &format!("v2par:{k}"),
        a.count,
        txs.load(Ordering::Relaxed),
        t0.elapsed(),
    );
    Ok(())
}

/// `ZcashView` over blocks held in memory (index 0 = `from`).
struct MemView {
    from: u64,
    blocks: Vec<BlockView>,
}

impl ZcashView for MemView {
    fn tip_height(&self) -> Result<u64, ViewError> {
        Ok(self.from + self.blocks.len() as u64 - 1)
    }
    fn block_at(&self, height: u64) -> Result<Option<BlockView>, ViewError> {
        Ok(height
            .checked_sub(self.from)
            .and_then(|i| self.blocks.get(i as usize))
            .cloned())
    }
}

/// Bytes a compact binary record of `b` would take (the cache format
/// sketched in docs/design/fast-restart.md: fixed header, pools, then per
/// tx txid/version/outputs/shielded summary, plus length + checksum).
fn compact_size(b: &BlockView) -> usize {
    let header = 8
        + 32
        + 32
        + 4
        + 1
        + if b.pools.is_some() {
            6 * 8 * 2 + 8 + 3 * 8
        } else {
            0
        }
        + 4;
    let txs: usize = b
        .txs
        .iter()
        .map(|t| {
            32 + 4
                + 4
                + t.outputs
                    .iter()
                    .map(|o| 8 + 4 + o.script.len())
                    .sum::<usize>()
                + 4
                + 1
                + if t.shielded.summary.is_some() {
                    4 * 8 + 5 * 4
                } else {
                    0
                }
        })
        .sum();
    8 + header + txs
}

fn mode_replay(a: &Args) -> Result<(), String> {
    let next = Arc::new(AtomicU64::new(0));
    let end = a.count;
    let t0 = Instant::now();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (next, url, from) = (next.clone(), a.rpc.clone(), a.from);
            std::thread::spawn(move || -> Result<Vec<(u64, BlockView)>, String> {
                let c = ZebradClient::new(url);
                let mut out = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= end {
                        return Ok(out);
                    }
                    let b = c
                        .block_at(from + i)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("no block at {}", from + i))?;
                    out.push((i, b));
                }
            })
        })
        .collect();
    let mut all = Vec::new();
    for h in handles {
        all.extend(h.join().map_err(|_| "thread panicked".to_string())??);
    }
    all.sort_by_key(|(i, _)| *i);
    let blocks: Vec<BlockView> = all.into_iter().map(|(_, b)| b).collect();
    println!(
        "  fetched {} blocks in {:.2} s",
        blocks.len(),
        t0.elapsed().as_secs_f64()
    );
    let bytes: usize = blocks.iter().map(compact_size).sum();
    let txs: u64 = blocks.iter().map(|b| b.txs.len() as u64).sum();
    let view = MemView {
        from: a.from,
        blocks,
    };
    let mut f = Follower::new(a.from, 1024).with_strict_pools(a.strict_pools);
    let t1 = Instant::now();
    let events = f.poll(&view).map_err(|e| e.to_string())?;
    let wall = t1.elapsed();
    println!(
        "  in-memory replay: {} events in {:.3} s; compact cache size {:.2} MB ({:.0} B/block, {:.0} B/tx) -> 250k blocks ~{:.0} MB",
        events.len(),
        wall.as_secs_f64(),
        bytes as f64 / 1e6,
        bytes as f64 / a.count as f64,
        bytes as f64 / txs.max(1) as f64,
        bytes as f64 / a.count as f64 * 250_000.0 / 1e6,
    );
    report("replay", a.count, txs, wall);
    Ok(())
}

fn main() -> std::process::ExitCode {
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return std::process::ExitCode::from(2);
        }
    };
    println!(
        "scan-bench: {} heights {}..={} ({} blocks)",
        a.rpc,
        a.from,
        a.from + a.count - 1,
        a.count
    );
    let mut ok = true;
    for mode in a.modes.clone() {
        println!("== {mode} ==");
        let (name, k) = match mode.split_once(':') {
            Some((n, k)) => (n.to_string(), k.parse::<u64>().unwrap_or(8).max(1)),
            None => (mode.clone(), 8),
        };
        let r = match name.as_str() {
            "follower" => mode_follower(&a),
            "breakdown" => mode_breakdown(&a),
            "parallel" => mode_parallel(&a, k),
            "batch" => mode_batch(&a, k),
            "v2" => mode_v2(&a),
            "v2par" => mode_v2par(&a, k),
            "replay" => mode_replay(&a),
            other => Err(format!("unknown mode {other}")),
        };
        if let Err(e) = r {
            println!("  ERROR: {e}");
            ok = false;
        }
    }
    if ok {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
