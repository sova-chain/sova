//! Miner daemon logic shared by CLI and MCP.
//!
//! This crate will hold the miner daemon's core logic (burn scheduling,
//! block templates, submission), shared between the `sova-miner` CLI binary
//! and an MCP server front end. It is currently a build-scaffold stub.

/// Returns the name of this crate, used as a placeholder smoke test until
/// real miner daemon types land.
#[must_use]
pub fn crate_name() -> &'static str {
    "miner"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_crate_name() {
        assert_eq!(crate_name(), "miner");
    }
}
