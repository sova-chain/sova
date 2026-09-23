//! `consensus_sim`: deterministic-replay binary for the C6 simulation
//! harness.
//!
//! Connects a [`Follower`] (base height 1, window 50) via [`ZebradClient`]
//! to a zebrad-compatible JSON-RPC endpoint, polls until the follower has
//! caught up to the chain's current tip, then prints the resulting epoch
//! stream, one line per epoch, in order:
//!
//! ```text
//! <height> <hash_hex> <burn_count> <total_burned_zat>
//! ```
//!
//! followed by a final line:
//!
//! ```text
//! STREAM_DIGEST <sha256 hex digest of every line above, newline-joined>
//! ```
//!
//! This binary is the proof surface for C6 at the follower layer: the
//! follower is a pure function of the Zcash chain (see
//! `crates/consensus/src/follower.rs`'s determinism note), so two
//! instances run against the same chain — concurrently, after a reorg, or
//! as a fresh process on restart — must print byte-identical
//! `STREAM_DIGEST` values. Any divergence is a determinism bug in the
//! follower or the burn-recognition rule it drives, not a flake.
//!
//! `box/sim/run-scenarios.sh` drives this binary through exactly those
//! three checks. See `box/sim/README.md` for what the harness proves
//! today and the extension plan for when full Sova nodes replace this
//! follower-only replay (C3's async Engine-API loop).
//!
//! Usage:
//!
//! ```text
//! consensus_sim [rpc_url]
//! ```
//!
//! `rpc_url` defaults to `$SOVA_REGTEST_RPC`, then
//! `http://127.0.0.1:18232`.

use std::process::ExitCode;
use std::time::Duration;

use consensus::follower::{Follower, FollowerEvent, ZcashView};
use consensus::zebrad::ZebradClient;
use sha2::{Digest, Sha256};

/// Zcash height the follower starts scanning from.
const BASE_HEIGHT: u64 = 1;
/// Reorg window, in blocks.
const WINDOW: usize = 50;
/// Delay between poll attempts while waiting for the follower to reach the
/// chain's current tip.
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Give up after this many consecutive polls produce no progress towards
/// the tip observed at loop start.
const MAX_IDLE_POLLS: u32 = 150;

/// Resolve the zebrad RPC URL: the first CLI argument, else
/// `$SOVA_REGTEST_RPC`, else the harness default.
fn rpc_url() -> String {
    std::env::args()
        .nth(1)
        .or_else(|| std::env::var("SOVA_REGTEST_RPC").ok())
        .unwrap_or_else(|| "http://127.0.0.1:18232".to_string())
}

/// Sum of `value_zat` across an epoch's recognized burns, saturating.
fn total_burned_zat(epoch: &consensus::follower::EpochData) -> u64 {
    epoch
        .burns
        .iter()
        .fold(0u64, |acc, b| acc.saturating_add(b.burn.value_zat))
}

/// Drop any buffered epoch lines above `to_height` — a rollback voids
/// them, and a correct replay never prints an epoch that isn't canonical
/// in the final chain observed.
fn apply_rollback(lines: &mut Vec<String>, to_height: u64) {
    lines.retain(|line| {
        line.split_whitespace()
            .next()
            .and_then(|h| h.parse::<u64>().ok())
            .is_some_and(|h| h <= to_height)
    });
}

/// Poll `client` via `follower` until the emitted epoch stream has caught
/// up to the chain's current tip, returning the ordered, deterministic
/// epoch lines.
fn replay(client: &ZebradClient, follower: &mut Follower) -> Result<Vec<String>, String> {
    let mut lines: Vec<String> = Vec::new();
    let mut idle_polls: u32 = 0;

    loop {
        let events = follower
            .poll(client)
            .map_err(|e| format!("follower poll failed: {e}"))?;

        if events.is_empty() {
            idle_polls = idle_polls.saturating_add(1);
        } else {
            idle_polls = 0;
        }

        for event in events {
            match event {
                FollowerEvent::Rollback { to_height } => apply_rollback(&mut lines, to_height),
                FollowerEvent::Epoch(epoch) => {
                    lines.push(format!(
                        "{} {} {} {}",
                        epoch.height,
                        hex::encode(epoch.hash),
                        epoch.burns.len(),
                        total_burned_zat(&epoch)
                    ));
                }
            }
        }

        let tip = client
            .tip_height()
            .map_err(|e| format!("tip_height failed: {e}"))?;
        let reached_height = lines
            .last()
            .and_then(|l| l.split_whitespace().next())
            .and_then(|h| h.parse::<u64>().ok());
        let caught_up = match reached_height {
            Some(h) => h >= tip,
            // No epochs printed yet: caught up only if the chain has
            // nothing at or above our base height to offer.
            None => tip < BASE_HEIGHT,
        };
        if caught_up {
            return Ok(lines);
        }
        if idle_polls >= MAX_IDLE_POLLS {
            return Err(format!(
                "gave up after {MAX_IDLE_POLLS} idle polls; stuck below tip {tip}"
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Hex-encoded sha256 over every line, each terminated by `\n` — exactly
/// the bytes this binary prints to stdout before this final line.
fn stream_digest(lines: &[String]) -> String {
    let mut hasher = Sha256::new();
    for line in lines {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

fn run() -> Result<(), String> {
    let url = rpc_url();
    let client = ZebradClient::new(url);
    let mut follower = Follower::new(BASE_HEIGHT, WINDOW);

    let lines = replay(&client, &mut follower)?;
    for line in &lines {
        println!("{line}");
    }
    println!("STREAM_DIGEST {}", stream_digest(&lines));
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("consensus_sim: error: {e}");
            ExitCode::FAILURE
        }
    }
}
