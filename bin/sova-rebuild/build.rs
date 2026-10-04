//! Stamps the release version into `sova-rebuild --version`: the same logic
//! as `bin/sova` (see `bin/sova/build.rs`), so every binary of a release
//! reports the same version.

include!("../sova/build_version.rs");

fn main() {
    println!("cargo:rerun-if-changed=../sova/build_version.rs");
    stamp_version();
}
