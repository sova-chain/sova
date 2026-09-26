//! Stamps the release version into `sova-miner --version`: the same logic
//! as `bin/sova` (see `bin/sova/build.rs`), so both binaries of a release
//! report the same version.

include!("../../../bin/sova/build_version.rs");

fn main() {
    println!("cargo:rerun-if-changed=../../../bin/sova/build_version.rs");
    stamp_version();
}
