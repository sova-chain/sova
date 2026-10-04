//! `sova-near-da`: Sova's NEAR data-availability archive
//! (`docs/design/near-da.md`).
//!
//! - `poster`: follow a Sova node and post every finalized block to the
//!   NEAR index contract, in SOVADA1 batches.
//! - `fetch`: download every batch from NEAR into a `.sovada` file set,
//!   checking everything the index claims.
//! - `verify`: check a `.sovada` file set offline (and against a node).
//! - `check-sources`: compare blocks rebuilt from `eth_*` with
//!   `debug_getRawBlock`.
//! - `info`: print the index contract's state.
//! - `keygen`: make an ed25519 key file for a NEAR account.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

mod archive;
mod fetch;
mod near;
mod poster;
mod sova;

/// Public NEAR testnet endpoints (best-effort, free, no key as of
/// 2026-10). The first answers recent transactions; the archival one
/// answers old ones. Override for mainnet or your own node.
const DEFAULT_NEAR_RPC: &str = "https://rpc.testnet.fastnear.com";
const DEFAULT_ARCHIVAL_RPC: &str = "https://archival-rpc.testnet.fastnear.com";
const DEFAULT_BLOCK_API: &str = "https://testnet.neardata.xyz";

#[derive(Parser)]
#[command(
    name = "sova-near-da",
    version,
    about = "Sova's NEAR DA archive: post, fetch, verify"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct NearArgs {
    /// The index contract's account.
    #[arg(long, env = "SOVA_DA_CONTRACT")]
    contract: String,
    /// NEAR JSON-RPC for posting and reading the index.
    #[arg(long, env = "SOVA_DA_NEAR_RPC", default_value = DEFAULT_NEAR_RPC)]
    near_rpc: String,
    /// More NEAR RPCs to fetch old batches from (archival), comma-separated.
    #[arg(long, env = "SOVA_DA_ARCHIVAL_RPC", value_delimiter = ',', default_value = DEFAULT_ARCHIVAL_RPC)]
    archival_rpc: Vec<String>,
    /// neardata.xyz-style block APIs for the scan fallback, comma-separated
    /// (empty to skip).
    #[arg(long, env = "SOVA_DA_BLOCK_API", value_delimiter = ',', default_value = DEFAULT_BLOCK_API)]
    block_api: Vec<String>,
    /// NEAR blocks before the index's `near_block` that a scan looks at.
    #[arg(long, default_value_t = 8)]
    scan_back: u64,
    /// Ignore recorded tx hashes and find every batch by scanning NEAR
    /// blocks (slow; proves the index alone is enough).
    #[arg(long)]
    scan_only: bool,
    /// Where a scan looks: NEAR RPCs, block APIs, or both.
    #[arg(long, value_enum, default_value_t = ScanVia::All)]
    scan_via: ScanVia,
    /// Per-request timeout, seconds.
    #[arg(long, default_value_t = 30)]
    timeout_secs: u64,
}

impl NearArgs {
    fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
    fn index_rpc(&self) -> near::Rpc {
        near::Rpc::new(&self.near_rpc, self.timeout())
    }
    fn sources(&self) -> archive::Sources {
        let mut rpcs = vec![self.index_rpc()];
        rpcs.extend(
            self.archival_rpc
                .iter()
                .filter(|u| !u.is_empty())
                .map(|u| near::Rpc::new(u, self.timeout())),
        );
        let apis = self
            .block_api
            .iter()
            .filter(|u| !u.is_empty())
            .cloned()
            .collect();
        let mut s = archive::Sources::new(rpcs, apis, self.scan_back, self.timeout());
        s.scan_only = self.scan_only;
        s.scan_rpcs = self.scan_via != ScanVia::BlockApi;
        s.scan_block_apis = self.scan_via != ScanVia::Rpc;
        s
    }
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ScanVia {
    All,
    Rpc,
    BlockApi,
}

#[derive(Subcommand)]
enum Cmd {
    /// Follow a Sova node and post finalized blocks to NEAR.
    Poster {
        #[command(flatten)]
        near: NearArgs,
        /// Sova node JSON-RPC (loopback).
        #[arg(
            long,
            env = "SOVA_DA_SOVA_RPC",
            default_value = "http://127.0.0.1:8545"
        )]
        sova_rpc: String,
        /// Raw block source: debug (debug_getRawBlock, SOVA_RPC_DEBUG=1),
        /// raw-tx (eth_getRawTransactionByBlockHashAndIndex, local profile),
        /// full-tx (eth_getBlockByNumber full objects, public profile), or
        /// auto (the first the node serves, in that order).
        #[arg(long, env = "SOVA_DA_RAW_SOURCE", value_enum, default_value_t = sova::RawSource::Auto)]
        raw_source: sova::RawSource,
        /// The signing NEAR account (the contract's owner).
        #[arg(long, env = "SOVA_DA_ACCOUNT")]
        account: String,
        /// Key file for the account (NEAR CLI JSON with `private_key`).
        #[arg(long, env = "SOVA_DA_KEY_FILE")]
        key_file: PathBuf,
        /// Local state file (progress journal; safe to delete).
        #[arg(long, env = "SOVA_DA_STATE_FILE")]
        state_file: PathBuf,
        /// Status JSON for health checks.
        #[arg(long, env = "SOVA_DA_STATUS_FILE")]
        status_file: Option<PathBuf>,
        /// Most blocks per batch (120 = 50 minutes at 25 s blocks).
        #[arg(long, env = "SOVA_DA_MAX_BLOCKS", default_value_t = 120)]
        max_blocks: u32,
        /// Most bytes per batch (NEAR's transaction limit is 1.5 MiB).
        #[arg(long, env = "SOVA_DA_MAX_BYTES", default_value_t = 1_000_000)]
        max_bytes: usize,
        /// Post a short batch once this many seconds pass without a post.
        #[arg(long, env = "SOVA_DA_MAX_WAIT_SECS", default_value_t = 3600)]
        max_wait_secs: u64,
        /// Seconds between polls of the node.
        #[arg(long, env = "SOVA_DA_POLL_SECS", default_value_t = 15)]
        poll_secs: u64,
        /// Post up to head - DEPTH instead of the node's `finalized` block
        /// (dev chains only; the testnet uses `finalized`, 300 deep).
        #[arg(long, env = "SOVA_DA_DEPTH")]
        depth: Option<u64>,
        /// Gas attached to each post, in Tgas. Measured: ~2 Tgas + 35
        /// Mgas/byte (31 Tgas for 814 KB), so 100 covers a 1 MB batch with
        /// room. NEAR holds 10x this at purchase (min_gas_purchase_price),
        /// refunded after: keep 0.1 NEAR spare per 100 Tgas.
        #[arg(long, env = "SOVA_DA_POST_TGAS", default_value_t = 100)]
        post_tgas: u64,
        /// Exit once everything available has been posted.
        #[arg(long)]
        once: bool,
    },
    /// Download every batch from NEAR into a .sovada file set and check it.
    Fetch {
        #[command(flatten)]
        near: NearArgs,
        /// Output directory.
        #[arg(long)]
        out: PathBuf,
        /// Refuse an archive of any other chain.
        #[arg(long)]
        expect_chain_id: Option<u64>,
        /// Also byte-compare every block with this Sova node.
        #[arg(long)]
        sova_rpc: Option<String>,
        /// Where the byte-compare gets the node's raw blocks.
        #[arg(long, value_enum, default_value_t = sova::RawSource::Auto)]
        raw_source: sova::RawSource,
        /// Print one line per downloaded batch.
        #[arg(long, short)]
        verbose: bool,
    },
    /// Check a .sovada file set offline.
    Verify {
        /// Directory of .sovada files.
        dir: PathBuf,
        /// Refuse any other chain.
        #[arg(long)]
        expect_chain_id: Option<u64>,
        /// The first file must start at this height.
        #[arg(long)]
        start_height: Option<u64>,
        /// Also byte-compare every block with this Sova node.
        #[arg(long)]
        sova_rpc: Option<String>,
        /// Where the byte-compare gets the node's raw blocks.
        #[arg(long, value_enum, default_value_t = sova::RawSource::Auto)]
        raw_source: sova::RawSource,
    },
    /// Rebuild blocks from eth_* (full tx objects, and raw txs if served)
    /// and compare them with debug_getRawBlock (needs SOVA_RPC_DEBUG=1).
    CheckSources {
        /// Sova node JSON-RPC.
        #[arg(long, default_value = "http://127.0.0.1:8545")]
        sova_rpc: String,
        /// First height.
        #[arg(long, default_value_t = 0)]
        from: u64,
        /// Last height (default: the head).
        #[arg(long)]
        to: Option<u64>,
    },
    /// Print the index contract's state (JSON).
    Info {
        #[command(flatten)]
        near: NearArgs,
        /// Also list every batch.
        #[arg(long)]
        batches: bool,
    },
    /// Write a new ed25519 key file (0600) and print its public key.
    Keygen {
        /// Account the key is for (recorded in the file).
        #[arg(long)]
        account: String,
        /// Output file; refuses to overwrite.
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sova-near-da: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.cmd {
        Cmd::Poster {
            near,
            sova_rpc,
            raw_source,
            account,
            key_file,
            state_file,
            status_file,
            max_blocks,
            max_bytes,
            max_wait_secs,
            poll_secs,
            depth,
            post_tgas,
            once,
        } => {
            if max_blocks == 0 || max_blocks > sovada::MAX_BLOCKS_PER_BATCH {
                return Err(format!(
                    "--max-blocks must be 1..={}",
                    sovada::MAX_BLOCKS_PER_BATCH
                ));
            }
            if max_bytes > 1_500_000 {
                return Err("--max-bytes must stay under NEAR's 1.5 MiB transaction limit".into());
            }
            let cfg = poster::Config {
                sova: sova::Sova::with_source(&sova_rpc, near.timeout(), raw_source),
                near: near.index_rpc(),
                sources: near.sources(),
                contract: near.contract.clone(),
                account,
                key: near::Key::from_file(&key_file)?,
                state_path: state_file,
                status_path: status_file,
                max_blocks,
                max_bytes,
                max_wait: Duration::from_secs(max_wait_secs),
                poll: Duration::from_secs(poll_secs),
                depth,
                post_gas: post_tgas * 1_000_000_000_000,
                once,
            };
            poster::run(&cfg)
        }
        Cmd::Fetch {
            near,
            out,
            expect_chain_id,
            sova_rpc,
            raw_source,
            verbose,
        } => {
            let r = fetch::fetch(
                &near.index_rpc(),
                &near.sources(),
                &near.contract,
                &out,
                expect_chain_id,
                verbose,
            )?;
            match r.range {
                Some((a, b)) => println!(
                    "fetched {} batch(es), {} blocks, heights {a}..={b} ({} downloaded) into {}",
                    r.batches,
                    r.blocks,
                    r.downloaded,
                    out.display()
                ),
                None => println!("the archive is empty"),
            }
            if let Some(url) = sova_rpc
                && r.batches > 0
            {
                let node = sova::Sova::with_source(&url, near.timeout(), raw_source);
                let v = fetch::verify(&out, expect_chain_id, None, Some(&node))?;
                println!(
                    "byte-compared {} block(s) with {url} ({}): identical",
                    v.compared,
                    node.raw_source()?.describe()
                );
            }
            Ok(())
        }
        Cmd::Verify {
            dir,
            expect_chain_id,
            start_height,
            sova_rpc,
            raw_source,
        } => {
            let node =
                sova_rpc.map(|u| sova::Sova::with_source(&u, Duration::from_secs(30), raw_source));
            let v = fetch::verify(&dir, expect_chain_id, start_height, node.as_ref())?;
            println!(
                "ok: {} file(s), {} blocks, heights {}..={}, chain {}, last block 0x{}{}",
                v.files,
                v.blocks,
                v.range.0,
                v.range.1,
                v.chain_id,
                hex::encode(v.last_hash),
                if node.is_some() {
                    format!(", {} byte-identical to the node", v.compared)
                } else {
                    String::new()
                }
            );
            Ok(())
        }
        Cmd::CheckSources { sova_rpc, from, to } => {
            let node = sova::Sova::new(&sova_rpc, Duration::from_secs(30));
            let to = match to {
                Some(t) => t,
                None => node.head()?,
            };
            // The raw-tx rebuild needs the local profile; skip it if absent.
            let raw_tx = node.raw_via_eth(from, false).is_ok();
            let mut bytes = 0usize;
            let mut txs = 0usize;
            for h in from..=to {
                let raw = node.raw_via_debug(h)?;
                let (_, full) = node.raw_via_eth(h, true)?;
                if full != raw {
                    return Err(format!(
                        "block {h}: rebuilt from full tx objects ({} bytes) differs from debug_getRawBlock ({} bytes)",
                        full.len(),
                        raw.len()
                    ));
                }
                if raw_tx {
                    let (_, rebuilt) = node.raw_via_eth(h, false)?;
                    if rebuilt != raw {
                        return Err(format!(
                            "block {h}: rebuilt from raw txs ({} bytes) differs from debug_getRawBlock ({} bytes)",
                            rebuilt.len(),
                            raw.len()
                        ));
                    }
                }
                txs += node.tx_types(h)?.len();
                bytes += raw.len();
            }
            println!(
                "ok: blocks {from}..={to} ({bytes} bytes, {txs} txs) rebuilt from full tx objects{} are byte-identical to debug_getRawBlock",
                if raw_tx { " and from raw txs" } else { "" }
            );
            Ok(())
        }
        Cmd::Info { near, batches } => {
            let rpc = near.index_rpc();
            let info = archive::info(&rpc, &near.contract)?;
            let mut v = serde_json::json!({"contract": near.contract, "info": info});
            if batches {
                v["batches"] = serde_json::to_value(archive::list(&rpc, &near.contract, 0)?)
                    .map_err(|e| e.to_string())?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Cmd::Keygen { account, out } => keygen(&account, &out),
    }
}

fn keygen(account: &str, out: &std::path::Path) -> Result<(), String> {
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut seed))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    let mut full = seed.to_vec();
    full.extend_from_slice(signing.verifying_key().as_bytes());
    let public = format!(
        "ed25519:{}",
        bs58::encode(signing.verifying_key().as_bytes()).into_string()
    );
    let body = serde_json::json!({
        "account_id": account,
        "public_key": public,
        "private_key": format!("ed25519:{}", bs58::encode(&full).into_string()),
    });
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(out)
        .map_err(|e| format!("{}: {e}", out.display()))?;
    f.write_all(body.to_string().as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("{}: {e}", out.display()))?;
    println!("{public}");
    Ok(())
}
