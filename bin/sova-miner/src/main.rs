//! Sova miner CLI: thin shim.
//!
//! Per the D2 decision recorded in `docs/WORKPLAN.md`: the real miner lives
//! beside `burn-wallet`, in burn-wallet's own nested Cargo workspace
//! (`crates/burn-wallet`, package `sova-miner` at
//! `crates/burn-wallet/miner`), because it links `burn-wallet` directly for
//! keys/tx-building/RPC -- and `burn-wallet` can never coexist with reth in
//! one dependency graph (see the long comment in the root `Cargo.toml`'s
//! `exclude` entry: `zcash_primitives`'s pinned `crypto-common` conflicts
//! with reth's). The miner therefore cannot live here, in the root
//! workspace that also builds `bin/sova` (which *does* link reth).
//!
//! This binary stays in the root workspace purely so `cargo build
//! --workspace` / `cargo run -p sova-miner` at the repo root keep working
//! as familiar entry points, without pulling reth and burn-wallet into one
//! build graph. It does the minimum useful thing: if the real miner is
//! already built, it `exec`s straight into it (so `bin/sova-miner`'s own
//! flags/output are otherwise invisible -- this process is fully replaced,
//! not wrapped); otherwise it prints where the real miner lives and how to
//! build/run it.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The real miner's package/binary name, and where it's built from.
const REAL_MINER_MANIFEST_HINT: &str = "crates/burn-wallet/miner";

fn main() {
    let args: Vec<String> = env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    if let Some(real_miner) = find_real_miner_binary() {
        exec_real_miner(&real_miner, &args);
        // exec_real_miner only returns on failure to exec.
    }

    print_shim_banner(&args);
}

/// Looks for an already-built real miner binary next to this crate's
/// nested workspace, in either the `release` or `debug` profile directory.
/// Uses `CARGO_MANIFEST_DIR` (this crate's own directory, baked in at
/// compile time) to locate the repo root reliably regardless of the
/// caller's current working directory.
fn find_real_miner_binary() -> Option<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR")); // <repo>/bin/sova-miner
    let repo_root = manifest_dir.parent()?.parent()?; // <repo>
    let nested_target = repo_root.join("crates/burn-wallet/target");

    let binary_name = if cfg!(windows) {
        "sova-miner.exe"
    } else {
        "sova-miner"
    };

    for profile in ["release", "debug"] {
        let candidate = nested_target.join(profile).join(binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Replaces this process with the real miner binary (Unix: a true `exec`,
/// so no shim process lingers; elsewhere: spawn-and-forward, then exit with
/// the child's status). Only returns if the exec/spawn itself failed.
fn exec_real_miner(path: &Path, args: &[String]) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(path).args(args).exec();
        eprintln!("sova-miner shim: failed to exec {}: {err}", path.display());
    }
    #[cfg(not(unix))]
    {
        match Command::new(path).args(args).status() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(err) => {
                eprintln!("sova-miner shim: failed to run {}: {err}", path.display());
            }
        }
    }
}

fn print_shim_banner(args: &[String]) {
    println!(
        "sova-miner {} (root-workspace shim -- pre-release)",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("The real miner lives in the nested burn-wallet workspace (it links");
    println!("burn-wallet directly and must never link reth -- see docs/WORKPLAN.md's");
    println!("D2 entry). Build and run it there:");
    println!();
    println!("    cd {REAL_MINER_MANIFEST_HINT}");
    println!("    cargo build --release          # or: cargo run --release -- <args>");
    println!();
    println!("Once built, this shim will exec straight into it automatically --");
    println!("re-run this same command from the repo root and it will forward");
    if args.is_empty() {
        println!("your arguments (none given this time) to the real binary.");
    } else {
        println!("your arguments ({}) to the real binary.", args.join(" "));
    }
}
