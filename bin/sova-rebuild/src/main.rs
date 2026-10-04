//! `sova-rebuild` command line. See the crate docs and [`USAGE`].

use std::{path::PathBuf, process::ExitCode, time::Duration};

use alloy_primitives::B256;
use reth_rpc_layer::JwtSecret;
use sova_rebuild::{
    archive::{self, ArchiveReader, CheckOptions, Checker},
    export::{self, ExportOptions, RawSource},
    rebuild::{self, Expect, RebuildOptions},
    rpc::Endpoint,
};

const USAGE: &str = "\
usage:
  sova-rebuild [OPTIONS] <ARCHIVE>...
      Rebuild a FRESH node (own datadir, no peers, its own zebrad) from the
      archive: every block goes to the node's authrpc as engine_newPayloadV4
      and the node's consensus decides; stops on the first rejected block.
        --jwt PATH            the node's authrpc JWT secret (required)
        --authrpc URL         the node's authrpc (default http://127.0.0.1:8551)
        --expect N:HASH[:STATEROOT]  the head the rebuild must end at
        --expect-rpc URL      an RPC (http or https) whose block at the rebuilt height must match
        --chain-id N          the chain id the archive must carry (default: the node's)
        --sip6                also check SIP-6 seals in the archive
        --hold-timeout SECS   longest wait on one held block (default 1800)
        --adopt-timeout SECS  longest wait for an accepted block to become head (default 120)

  sova-rebuild --verify-only [OPTIONS] <ARCHIVE>...
      Check the archive alone (no node): one chain id, contiguous heights,
      each block's keccak(header RLP) = its recorded hash, parent links, tx
      chain ids, body roots vs header.
        --chain-id N          the chain id every batch must carry
        --genesis HASH        the first block's parent (when it starts at 1)
        --sip6                also check SIP-6 seals under the chain id

  sova-rebuild export --rpc URL --out DIR [--from N] [--to N] [--batch-size N]
                      [--raw auto|debug|reconstruct]
      Write a node's blocks 1..head (or --from..--to) as batch files.

  <ARCHIVE>: batch files, directories of them (read in first-height order),
  or `-` for batches on stdin (last).

exit codes: 0 ok, 1 usage/other, 2 block rejected by the node, 3 archive
broken, 4 head mismatch, 5 stuck (hold/adopt/syncing timeout), 6 node unusable";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure(code, msg)) => {
            eprintln!("{msg}");
            ExitCode::from(code)
        }
    }
}

struct Failure(u8, String);

impl From<eyre::Report> for Failure {
    fn from(e: eyre::Report) -> Self {
        Self(1, format!("error: {e:#}"))
    }
}

fn run(args: Vec<String>) -> Result<(), Failure> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    if args[0] == "export" {
        return run_export(&args[1..]);
    }
    let mut p = Args::new(&args);
    let verify_only = p.flag("--verify-only");
    let sip6 = p.flag("--sip6");
    let chain_id = p.parsed::<u64>("--chain-id")?;
    if verify_only {
        let genesis = p.parsed::<B256>("--genesis")?;
        let inputs = p.rest()?;
        return run_verify(&inputs, chain_id, genesis, sip6);
    }
    let jwt = p
        .value("--jwt")?
        .ok_or_else(|| eyre::eyre!("rebuild needs --jwt (the node's authrpc secret)\n{USAGE}"))?;
    let authrpc = p
        .value("--authrpc")?
        .unwrap_or_else(|| "http://127.0.0.1:8551".to_owned());
    let expect = p.parsed::<Expect>("--expect")?;
    let expect_rpc = p.value("--expect-rpc")?.map(Endpoint::http);
    let hold = p.parsed::<u64>("--hold-timeout")?.unwrap_or(1800);
    let adopt = p.parsed::<u64>("--adopt-timeout")?.unwrap_or(120);
    let inputs = p.rest()?;
    let jwt =
        JwtSecret::from_file(&PathBuf::from(&jwt)).map_err(|e| eyre::eyre!("--jwt {jwt}: {e}"))?;
    let opts = RebuildOptions {
        node: Endpoint::auth(authrpc, jwt),
        chain_id,
        sip6,
        expect,
        expect_rpc,
        hold_timeout: Duration::from_secs(hold),
        adopt_timeout: Duration::from_secs(adopt),
        syncing_timeout: Duration::from_secs(60),
        error_timeout: Duration::from_secs(120),
        payload_timeout: Duration::from_secs(60),
        startup_timeout: Duration::from_secs(120),
    };
    let sources = archive::expand(&inputs)?;
    match rebuild::rebuild(sources, &opts) {
        Ok(r) => {
            let rate = if r.secs > 0.0 {
                r.imported as f64 / r.secs
            } else {
                0.0
            };
            println!(
                "REBUILD OK: head {}; {} block(s) imported in {:.1} s ({rate:.1} blocks/s), {} already present; {} hold(s), {:.1} s held; archive: {} block(s), {} tx(s), {} with withdrawals{}",
                r.head,
                r.imported,
                r.secs,
                r.already_had,
                r.holds,
                r.hold_secs,
                r.archive.blocks,
                r.archive.txs,
                r.archive.with_withdrawals,
                if sip6 {
                    format!(
                        ", {} sealed / {} null",
                        r.archive.sealed, r.archive.null_blocks
                    )
                } else {
                    String::new()
                }
            );
            for m in &r.matched {
                println!("  matches {m}");
            }
            Ok(())
        }
        Err(e) => Err(Failure(e.exit_code() as u8, format!("REBUILD FAILED: {e}"))),
    }
}

fn run_verify(
    inputs: &[String],
    chain_id: Option<u64>,
    genesis: Option<B256>,
    sip6: bool,
) -> Result<(), Failure> {
    let sources = archive::expand(inputs)?;
    let mut checker = Checker::new(CheckOptions {
        chain_id,
        parent_of_first: None,
        deep: true,
        sip6,
    });
    let mut first = true;
    let started = std::time::Instant::now();
    for item in ArchiveReader::new(sources) {
        let (label, entry) = item.map_err(|e| Failure(3, format!("VERIFY FAILED: {e}")))?;
        if first {
            first = false;
            if entry.block.height == 1 && genesis.is_some() {
                checker = Checker::new(CheckOptions {
                    chain_id,
                    parent_of_first: genesis,
                    deep: true,
                    sip6,
                });
            }
        }
        if let Err(e) = checker.check(&entry.batch, &entry) {
            return Err(Failure(3, format!("VERIFY FAILED: {label}: {e}")));
        }
    }
    let s = checker.stats;
    let (Some((lo, lo_hash)), Some((hi, hi_hash, root))) = (s.first, s.last) else {
        return Err(Failure(3, "VERIFY FAILED: archive holds no blocks".into()));
    };
    println!(
        "VERIFY OK: chain id {}, blocks #{lo}..#{hi} ({} block(s), {} duplicate(s) skipped), first {lo_hash}, last {hi_hash} stateRoot {root}; {} tx(s), {} with withdrawals{}; {:.2} s",
        checker.chain_id().unwrap_or_default(),
        s.blocks,
        s.duplicates,
        s.txs,
        s.with_withdrawals,
        if sip6 {
            format!(", seals ok: {} sealed / {} null", s.sealed, s.null_blocks)
        } else {
            ", seals not checked (--sip6)".to_owned()
        },
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn run_export(args: &[String]) -> Result<(), Failure> {
    let mut p = Args::new(args);
    let rpc = p
        .value("--rpc")?
        .ok_or_else(|| eyre::eyre!("export needs --rpc URL\n{USAGE}"))?;
    let out = p
        .value("--out")?
        .ok_or_else(|| eyre::eyre!("export needs --out DIR\n{USAGE}"))?;
    let from = p.parsed::<u64>("--from")?.unwrap_or(1);
    let to = p.parsed::<u64>("--to")?;
    let batch_size = p.parsed::<u32>("--batch-size")?.unwrap_or(100);
    let raw = match p.value("--raw")?.as_deref() {
        None | Some("auto") => RawSource::Auto,
        Some("debug") => RawSource::Debug,
        Some("reconstruct") => RawSource::Reconstruct,
        Some(other) => {
            return Err(eyre::eyre!("--raw must be auto|debug|reconstruct, got {other:?}").into());
        }
    };
    let rest = p.rest_allow_empty();
    if !rest.is_empty() {
        return Err(eyre::eyre!("export: unexpected arguments {rest:?}").into());
    }
    let r = export::export(
        &Endpoint::http(rpc),
        &ExportOptions {
            from,
            to,
            batch_size,
            out: PathBuf::from(out),
            raw,
        },
    )?;
    println!(
        "EXPORT OK: chain id {}, blocks #{}..#{} into {} file(s) (raw from {}), last {}; {:.1} s",
        r.chain_id,
        r.from,
        r.to,
        r.files.len(),
        if r.used_debug {
            "debug_getRawBlock"
        } else {
            "eth_* reconstruction"
        },
        r.last_hash,
        r.secs
    );
    Ok(())
}

/// Minimal `--flag` / `--key value` parsing; whatever is left is
/// positional.
struct Args {
    items: Vec<Option<String>>,
}

impl Args {
    fn new(args: &[String]) -> Self {
        Self {
            items: args.iter().cloned().map(Some).collect(),
        }
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.items.iter().position(|a| a.as_deref() == Some(name))
    }

    fn flag(&mut self, name: &str) -> bool {
        match self.position(name) {
            Some(i) => {
                self.items[i] = None;
                true
            }
            None => false,
        }
    }

    fn value(&mut self, name: &str) -> eyre::Result<Option<String>> {
        let Some(i) = self.position(name) else {
            return Ok(None);
        };
        self.items[i] = None;
        match self.items.get_mut(i + 1).and_then(Option::take) {
            Some(v) if !v.starts_with("--") => Ok(Some(v)),
            _ => Err(eyre::eyre!("{name} needs a value")),
        }
    }

    fn parsed<T: std::str::FromStr>(&mut self, name: &str) -> eyre::Result<Option<T>>
    where
        T::Err: std::fmt::Display,
    {
        match self.value(name)? {
            Some(v) => v
                .parse()
                .map(Some)
                .map_err(|e| eyre::eyre!("{name} {v:?}: {e}")),
            None => Ok(None),
        }
    }

    fn rest_allow_empty(self) -> Vec<String> {
        self.items.into_iter().flatten().collect()
    }

    fn rest(self) -> eyre::Result<Vec<String>> {
        let rest = self.rest_allow_empty();
        if let Some(bad) = rest.iter().find(|a| a.starts_with("--")) {
            return Err(eyre::eyre!("unknown option {bad}\n{USAGE}"));
        }
        if rest.is_empty() {
            return Err(eyre::eyre!("no archive given\n{USAGE}"));
        }
        Ok(rest)
    }
}
