// Shared by `bin/sova/build.rs` and `crates/burn-wallet/miner/build.rs`
// (`include!`d, so no inner doc comments here).

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

/// Emits `cargo:rustc-env=SOVA_BUILD_VERSION=...` (see `bin/sova/build.rs`).
/// The caller also emits `rerun-if-changed` for this file's path.
fn stamp_version() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOVA_VERSION");
    let pkg = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let version = match env::var("SOVA_VERSION") {
        Ok(v) if !v.trim().is_empty() => strip_v(v.trim()).to_owned(),
        _ => from_git(&pkg).unwrap_or(pkg),
    };
    println!("cargo:rustc-env=SOVA_BUILD_VERSION={version}");
}

/// `v0.1.3` -> `0.1.3`; anything else unchanged.
fn strip_v(v: &str) -> &str {
    match v.strip_prefix('v') {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
        _ => v,
    }
}

fn git(args: &[&str]) -> Option<String> {
    let dir = env::var("CARGO_MANIFEST_DIR").ok()?;
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!s.is_empty()).then_some(s)
}

fn from_git(pkg: &str) -> Option<String> {
    let described = git(&[
        "describe",
        "--tags",
        "--always",
        "--abbrev=12",
        "--match",
        "v[0-9]*",
    ])?;
    rerun_on_git_changes();
    if described.starts_with('v') {
        Some(strip_v(&described).to_owned())
    } else {
        // No tag reachable: `describe --always` printed the bare commit.
        Some(format!("{pkg}+{described}"))
    }
}

/// Re-run when HEAD moves or a tag is added, and only on paths that exist
/// (cargo treats a missing path as always changed).
fn rerun_on_git_changes() {
    let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) else {
        return;
    };
    let git_dir = PathBuf::from(git_dir);
    let common = git(&["rev-parse", "--git-common-dir"])
        .map(PathBuf::from)
        .map(|p| {
            if p.is_absolute() {
                p
            } else {
                Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap_or_default()).join(p)
            }
        })
        .unwrap_or_else(|| git_dir.clone());
    let mut paths = vec![
        git_dir.join("HEAD"),
        common.join("packed-refs"),
        common.join("refs/tags"),
    ];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        paths.push(common.join(branch));
    }
    for p in paths.iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}
