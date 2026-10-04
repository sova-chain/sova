//! Stamps the release version into `sova-near-da --version`.
//!
//! The release workflow (`.github/workflows/box-binaries.yml`) sets
//! `SOVA_VERSION` to the tag (`v0.1.20` -> `0.1.20`), so the tool in a
//! release tarball reports the same version as `sova`, `sova-miner` and
//! `sova-rebuild`. Otherwise it is the crate version. Self-contained on
//! purpose: this workspace builds from a copy of `tools/near-da` alone
//! (`deploy/build-linux.sh`), so it can't `include!` `bin/sova`'s
//! `build_version.rs` (whose `git describe` fallback it doesn't need).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOVA_VERSION");
    let pkg = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let version = match std::env::var("SOVA_VERSION") {
        Ok(v) if !v.trim().is_empty() => {
            let v = v.trim();
            match v.strip_prefix('v') {
                Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest.to_owned(),
                _ => v.to_owned(),
            }
        }
        _ => pkg,
    };
    println!("cargo:rustc-env=SOVA_NEAR_DA_VERSION={version}");
}
