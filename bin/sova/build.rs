//! Stamps the release version into the binary as `SOVA_BUILD_VERSION`,
//! which `sova --version` and the startup banner print. `sova-miner`'s
//! build script includes this file, so both binaries report the same.
//!
//! In order:
//!
//! 1. `SOVA_VERSION` from the environment (the release workflow sets it to
//!    the tag, e.g. `v0.1.3`), with a leading `v` dropped: `0.1.3`.
//! 2. `git describe` of the checkout, if a `v*` tag is reachable:
//!    `0.1.3` on the tag itself, `0.1.3-4-g<sha>` four commits after it.
//! 3. The crate version plus the short commit: `0.1.0+<sha>`.
//! 4. The crate version alone (no git, e.g. a source tarball).

include!("build_version.rs");

fn main() {
    println!("cargo:rerun-if-changed=build_version.rs");
    stamp_version();
}
