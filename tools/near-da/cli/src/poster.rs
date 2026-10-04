//! The poster: follows a Sova node, posts every finalized block to the NEAR
//! index contract in SOVADA1 batches, and records each batch's transaction.
//!
//! Correctness comes from the contract, not from this process: the contract
//! accepts a batch only if it starts at its `next_height` and links to its
//! last block, so a restart, a crash between broadcast and confirmation, a
//! late duplicate, or two posters at once can waste a transaction but can
//! never post a range twice or skip one. The poster always reads
//! `next_height` from the contract before building a batch.
//!
//! The local state file is a convenience: the last nonce used, when the
//! last batch went out (for `--max-wait`), and a journal of submitted
//! transaction hashes so `set_tx` can be filled in without scanning NEAR.
//! Deleting it is safe.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiny_keccak::{Hasher as _, Keccak};

use crate::archive::{self, Info, Sources};
use crate::near::{self, FunctionCall, Key, Rpc};
use crate::sova::{BlockTag, Sova};

/// Poster settings.
pub struct Config {
    /// The Sova node (raw blocks from `debug_getRawBlock`, or rebuilt from
    /// `eth_*` when the node doesn't serve it).
    pub sova: Sova,
    /// The NEAR RPC used to post and to read the index.
    pub near: Rpc,
    /// Where `set_tx` back-fill looks for a transaction it has no record of.
    pub sources: Sources,
    /// The index contract's account.
    pub contract: String,
    /// The account that signs (the contract's owner).
    pub account: String,
    /// Its key.
    pub key: Key,
    /// Local state file.
    pub state_path: PathBuf,
    /// Status file for health checks (JSON), if any.
    pub status_path: Option<PathBuf>,
    /// Most blocks per batch.
    pub max_blocks: u32,
    /// Most bytes per batch (a batch always holds at least one block).
    pub max_bytes: usize,
    /// Post a short batch once this long has passed since the last one.
    pub max_wait: Duration,
    /// How often to look for new blocks.
    pub poll: Duration,
    /// `None`: post up to the node's `finalized` block. `Some(d)`: up to
    /// head − d (for a dev chain with no finality yet).
    pub depth: Option<u64>,
    /// Gas attached to `post`.
    pub post_gas: u64,
    /// Exit once everything available at start is posted (ignores
    /// `max_wait`).
    pub once: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    contract: String,
    #[serde(default)]
    last_nonce: u64,
    #[serde(default)]
    last_post_unix: u64,
    /// First batch index that may still lack its `tx_hash` in the index.
    #[serde(default)]
    tx_filled_from: u64,
    #[serde(default)]
    journal: Vec<Journal>,
    /// `--once`: the target seen on the first pass; never posts past it.
    #[serde(skip)]
    once_cap: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    index: u64,
    first_height: u64,
    count: u32,
    sha256: String,
    tx_hash: String,
    submitted_unix: u64,
}

const JOURNAL_KEEP: usize = 500;

#[derive(Debug, Default, Serialize)]
struct Status {
    state: &'static str,
    updated_unix: u64,
    contract: String,
    account: String,
    chain_id: u64,
    sova_head: Option<u64>,
    sova_target: Option<u64>,
    archived_through: Option<u64>,
    next_height: Option<u64>,
    lag_blocks: Option<u64>,
    batch_count: Option<u64>,
    bytes_posted: Option<u64>,
    last_batch: Option<Value>,
    last_error: Option<String>,
}

/// Why a step stopped.
enum Stop {
    /// Retry after a short wait (RPC hiccup, NEAR down, node not synced).
    Transient(String),
    /// Stop posting until a human looks (the archive and the node disagree,
    /// wrong contract, wrong key). Retried slowly so a fix is picked up.
    Halt(String),
}

enum Progress {
    Posted,
    Idle,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for log lines.
pub fn utc(ts: u64) -> String {
    let days = (ts / 86_400) as i64;
    let secs = ts % 86_400;
    // Civil-from-days (H. Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

macro_rules! log {
    ($($t:tt)*) => {
        println!("{} {}", utc(now_unix()), format!($($t)*))
    };
}

/// keccak-256.
pub fn keccak(data: &[u8]) -> [u8; 32] {
    let mut k = Keccak::v256();
    k.update(data);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

fn load_state(path: &Path, contract: &str) -> Result<State, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let s: State = serde_json::from_str(&text)
                .map_err(|e| format!("state file {}: {e}", path.display()))?;
            if s.contract != contract {
                return Err(format!(
                    "state file {} belongs to contract {}, not {contract}; move it away to start over",
                    path.display(),
                    s.contract
                ));
            }
            Ok(s)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State {
            contract: contract.to_owned(),
            ..State::default()
        }),
        Err(e) => Err(format!("state file {}: {e}", path.display())),
    }
}

/// Write `data` to `path` atomically (temp file in the same directory,
/// fsync, rename).
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(data)
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn save_state(path: &Path, s: &State) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    write_atomic(path, &data)
}

/// Run the poster until a fatal setup error (or, with `once`, until caught
/// up).
pub fn run(cfg: &Config) -> Result<(), String> {
    let mut state = load_state(&cfg.state_path, &cfg.contract)?;
    let chain_id = cfg.sova.chain_id()?;
    let mut status = Status {
        contract: cfg.contract.clone(),
        account: cfg.account.clone(),
        chain_id,
        ..Status::default()
    };
    let source = cfg.sova.raw_source()?;
    log!(
        "poster: sova chain {chain_id}, contract {} as {} ({}), target {}, raw blocks: {}",
        cfg.contract,
        cfg.account,
        cfg.key.public_key(),
        match cfg.depth {
            None => "finalized".to_owned(),
            Some(d) => format!("head-{d}"),
        },
        source.describe()
    );
    let mut last_error: Option<String> = None;
    loop {
        let r = step(cfg, chain_id, &mut state, &mut status);
        status.updated_unix = now_unix();
        let sleep = match r {
            Ok(Progress::Posted) => {
                status.state = "ok";
                status.last_error = None;
                last_error = None;
                Duration::ZERO
            }
            Ok(Progress::Idle) => {
                status.state = "ok";
                status.last_error = None;
                last_error = None;
                if cfg.once {
                    write_status(cfg, &status);
                    log!("poster: caught up (--once), exiting");
                    return Ok(());
                }
                cfg.poll
            }
            Err(Stop::Transient(e)) => {
                status.state = "retrying";
                if last_error.as_deref() != Some(&e) {
                    log!("poster: error (will retry): {e}");
                }
                status.last_error = Some(e.clone());
                last_error = Some(e);
                cfg.poll.max(Duration::from_secs(5))
            }
            Err(Stop::Halt(e)) => {
                status.state = "halted";
                if last_error.as_deref() != Some(&e) {
                    log!("poster: HALTED, not posting: {e}");
                }
                status.last_error = Some(e.clone());
                last_error = Some(e);
                if cfg.once {
                    write_status(cfg, &status);
                    return Err("halted".to_owned());
                }
                cfg.poll.max(Duration::from_secs(60))
            }
        };
        write_status(cfg, &status);
        if let Err(e) = save_state(&cfg.state_path, &state) {
            log!("poster: cannot save state: {e}");
        }
        std::thread::sleep(sleep);
    }
}

fn write_status(cfg: &Config, status: &Status) {
    if let Some(p) = &cfg.status_path
        && let Ok(data) = serde_json::to_vec_pretty(status)
        && let Err(e) = write_atomic(p, &data)
    {
        log!("poster: cannot write status: {e}");
    }
}

fn step(
    cfg: &Config,
    chain_id: u64,
    state: &mut State,
    status: &mut Status,
) -> Result<Progress, Stop> {
    let info = archive::info(&cfg.near, &cfg.contract).map_err(Stop::Transient)?;
    if info.format != "SOVADA1" {
        return Err(Stop::Halt(format!(
            "contract format is {}, not SOVADA1",
            info.format
        )));
    }
    if info.owner != cfg.account {
        return Err(Stop::Halt(format!(
            "contract owner is {}, but this poster signs as {}",
            info.owner, cfg.account
        )));
    }
    if info.chain_id != chain_id {
        return Err(Stop::Halt(format!(
            "contract archives chain {}, but the Sova node is chain {chain_id}",
            info.chain_id
        )));
    }
    status.next_height = Some(info.next_height);
    status.archived_through = info
        .next_height
        .checked_sub(1)
        .filter(|&h| h >= info.start_height);
    status.batch_count = Some(info.batch_count);
    status.bytes_posted = Some(info.bytes_posted);

    // Best effort: a missing tx_hash only slows readers down.
    if let Err(e) = fill_tx_hashes(cfg, state, &info) {
        log!("poster: set_tx back-fill: {e}");
    }

    let next = info.next_height;
    let prev_hash = info.last_hash_bytes().map_err(Stop::Halt)?;
    if let Some(want) = prev_hash {
        let have = cfg
            .sova
            .block(BlockTag::Number(next - 1))
            .map_err(Stop::Transient)?;
        match have {
            None => {
                return Err(Stop::Transient(format!(
                    "Sova node has no block {} yet",
                    next - 1
                )));
            }
            Some((_, h)) if h != want => {
                return Err(Stop::Halt(format!(
                    "the archive's block {} is 0x{} but the node's is 0x{}: a reorg below the posting depth, or the wrong node/chain",
                    next - 1,
                    hex::encode(want),
                    hex::encode(h)
                )));
            }
            Some(_) => {}
        }
    }

    let head = cfg.sova.head().map_err(Stop::Transient)?;
    status.sova_head = Some(head);
    let target = match cfg.depth {
        Some(d) => head.checked_sub(d),
        None => cfg
            .sova
            .block(BlockTag::Finalized)
            .map_err(Stop::Transient)?
            .map(|(n, _)| n),
    };
    // `--once` stops at what was available when it started, so a fast
    // chain cannot keep it running.
    let target = if cfg.once {
        if state.once_cap.is_none() {
            state.once_cap = target;
        }
        target.min(state.once_cap)
    } else {
        target
    };
    status.sova_target = target;
    status.lag_blocks = target.map(|t| (t + 1).saturating_sub(next));
    let Some(target) = target.filter(|&t| t >= next) else {
        return Ok(Progress::Idle);
    };
    let avail = target - next + 1;
    let waited = now_unix().saturating_sub(state.last_post_unix);
    let due = cfg.once
        || avail >= u64::from(cfg.max_blocks)
        || state.last_post_unix == 0
        || waited >= cfg.max_wait.as_secs();
    if !due {
        return Ok(Progress::Idle);
    }

    // Build the batch from the node, checking it as the contract will.
    let last_wanted = target.min(next + u64::from(cfg.max_blocks) - 1);
    let mut raws: Vec<(u64, [u8; 32], Vec<u8>)> = Vec::new();
    let mut size = sovada::HEADER_LEN;
    for h in next..=last_wanted {
        let (hash, raw) = cfg.sova.raw_block(h).map_err(Stop::Transient)?;
        if !raws.is_empty() && size + sovada::ENTRY_HEADER_LEN + raw.len() > cfg.max_bytes {
            break;
        }
        size += sovada::ENTRY_HEADER_LEN + raw.len();
        raws.push((h, hash, raw));
    }
    let blocks: Vec<sovada::Block<'_>> = raws
        .iter()
        .map(|(height, hash, raw)| sovada::Block {
            height: *height,
            hash: *hash,
            raw,
        })
        .collect();
    let bytes = sovada::encode(chain_id, &blocks).map_err(|e| Stop::Halt(e.to_string()))?;
    let batch = sovada::parse(&bytes).map_err(|e| Stop::Halt(e.to_string()))?;
    let link = prev_hash.or(if next == 0 { Some([0u8; 32]) } else { None });
    sovada::verify_blocks(&batch, &keccak, link)
        .map_err(|e| Stop::Transient(format!("node returned blocks that do not check: {e}")))?;
    // The last block must still be the node's (no reorg while reading).
    let (first, last) = (batch.header.first_height, batch.header.last_height());
    let last_hash = raws[raws.len() - 1].1;
    match cfg
        .sova
        .block(BlockTag::Number(last))
        .map_err(Stop::Transient)?
    {
        Some((_, h)) if h == last_hash => {}
        _ => {
            return Err(Stop::Transient(format!(
                "block {last} changed while building the batch"
            )));
        }
    }

    let index = info.batch_count;
    let sha = archive::sha256_hex(&bytes);
    let call = FunctionCall {
        method: "post",
        args: &bytes,
        gas: cfg.post_gas,
        deposit: 0,
    };
    let signed = sign(cfg, state, &call)?;
    state.journal.push(Journal {
        index,
        first_height: first,
        count: batch.header.count,
        sha256: sha.clone(),
        tx_hash: signed.hash.clone(),
        submitted_unix: now_unix(),
    });
    if state.journal.len() > JOURNAL_KEEP {
        let drop = state.journal.len() - JOURNAL_KEEP;
        state.journal.drain(..drop);
    }
    // Journal first: a crash after broadcast still knows the hash.
    save_state(&cfg.state_path, state).map_err(Stop::Transient)?;

    let result = submit(cfg, &signed)?;
    let burnt = burnt_gas(&result);
    state.last_post_unix = now_unix();
    let line = json!({
        "index": index, "first_height": first, "last_height": last, "count": batch.header.count,
        "bytes": bytes.len(), "sha256": sha, "tx_hash": signed.hash, "gas_burnt": burnt,
    });
    log!(
        "posted batch #{index} heights {first}..={last} ({} blocks, {} bytes) tx {} gas {:.2} Tgas",
        batch.header.count,
        bytes.len(),
        signed.hash,
        burnt as f64 / 1e12
    );
    status.last_batch = Some(line);
    status.next_height = Some(last + 1);
    status.archived_through = Some(last);
    status.lag_blocks = Some(target.saturating_sub(last));
    // Record the transaction in the index now; the back-fill retries later
    // if this fails.
    match set_tx(cfg, state, index, &signed.hash) {
        Ok(g) => log!("set_tx #{index} gas {:.2} Tgas", g as f64 / 1e12),
        Err(e) => log!("poster: set_tx #{index} failed (will retry): {e}"),
    }
    Ok(Progress::Posted)
}

fn sign(cfg: &Config, state: &mut State, call: &FunctionCall<'_>) -> Result<near::SignedTx, Stop> {
    let (ak_nonce, block_hash) = cfg
        .near
        .access_key(&cfg.account, &cfg.key.public_key())
        .map_err(|e| match e.name() {
            Some("UNKNOWN_ACCESS_KEY") | Some("QUERY_ERROR") => Stop::Halt(format!(
                "key {} on {}: {e}",
                cfg.key.public_key(),
                cfg.account
            )),
            _ => Stop::Transient(e.to_string()),
        })?;
    let nonce = ak_nonce.max(state.last_nonce) + 1;
    state.last_nonce = nonce;
    Ok(near::sign_function_call(
        &cfg.key,
        &cfg.account,
        nonce,
        &cfg.contract,
        &block_hash,
        call,
    ))
}

/// Broadcast and wait for finality. On a timeout or transport error, poll
/// the transaction by hash for a while before giving up (the caller then
/// re-reads the contract, which tells whether it landed).
fn submit(cfg: &Config, signed: &near::SignedTx) -> Result<Value, Stop> {
    let first = cfg.near.send_tx(&signed.bytes, "FINAL");
    let result = match first {
        Ok(r) => r,
        Err(e) => {
            log!("poster: send_tx {}: {e}; polling", signed.hash);
            let mut found = None;
            for _ in 0..12 {
                std::thread::sleep(Duration::from_secs(5));
                match cfg.near.tx(&signed.hash, &cfg.account, "FINAL") {
                    Ok(r) if near::outcome(&r).is_some() => {
                        found = Some(r);
                        break;
                    }
                    Ok(_) => {}
                    Err(e2) if e2.name() == Some("UNKNOWN_TRANSACTION") => {}
                    Err(e2) => log!("poster: tx status {}: {e2}", signed.hash),
                }
            }
            found.ok_or_else(|| {
                Stop::Transient(format!("transaction {} not confirmed: {e}", signed.hash))
            })?
        }
    };
    match near::outcome(&result) {
        Some(Ok(())) => Ok(result),
        Some(Err(f)) => Err(Stop::Transient(format!(
            "transaction {} failed: {f}",
            signed.hash
        ))),
        None => Err(Stop::Transient(format!(
            "transaction {} not final",
            signed.hash
        ))),
    }
}

fn burnt_gas(result: &Value) -> u64 {
    let tx = result["transaction_outcome"]["outcome"]["gas_burnt"]
        .as_u64()
        .unwrap_or(0);
    let receipts: u64 = result["receipts_outcome"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .map(|r| r["outcome"]["gas_burnt"].as_u64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0);
    tx + receipts
}

/// Record batch `index`'s transaction in the index. Returns gas burnt.
fn set_tx(cfg: &Config, state: &mut State, index: u64, tx_hash: &str) -> Result<u64, String> {
    let args = json!({"index": index, "tx_hash": tx_hash}).to_string();
    let call = FunctionCall {
        method: "set_tx",
        args: args.as_bytes(),
        gas: 10_000_000_000_000,
        deposit: 0,
    };
    let signed = sign(cfg, state, &call).map_err(|s| match s {
        Stop::Transient(e) | Stop::Halt(e) => e,
    })?;
    let r = submit(cfg, &signed).map_err(|s| match s {
        Stop::Transient(e) | Stop::Halt(e) => e,
    })?;
    Ok(burnt_gas(&r))
}

/// Give every indexed batch from `state.tx_filled_from` its `tx_hash`:
/// from the journal (checked against NEAR), else by scanning NEAR blocks.
fn fill_tx_hashes(cfg: &Config, state: &mut State, info: &Info) -> Result<(), String> {
    if state.tx_filled_from >= info.batch_count {
        return Ok(());
    }
    let entries = archive::list(&cfg.near, &cfg.contract, state.tx_filled_from)?;
    for e in entries {
        if e.tx_hash.is_none() {
            let journaled = state
                .journal
                .iter()
                .rev()
                .find(|j| j.index == e.index && j.sha256 == e.sha256)
                .map(|j| j.tx_hash.clone());
            let hash = match journaled {
                Some(h) => match cfg.near.tx(&h, &cfg.account, "FINAL") {
                    Ok(r) if matches!(near::outcome(&r), Some(Ok(()))) => h,
                    _ => archive::fetch(&cfg.sources, &cfg.contract, &cfg.account, &e)?.tx_hash,
                },
                None => archive::fetch(&cfg.sources, &cfg.contract, &cfg.account, &e)?.tx_hash,
            };
            set_tx(cfg, state, e.index, &hash)?;
            log!("set_tx #{} -> {hash} (back-fill)", e.index);
        }
        state.tx_filled_from = e.index + 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_format() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_791_082_800), "2026-10-04T03:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn keccak_vector() {
        assert_eq!(
            hex::encode(keccak(b"")),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }
}
