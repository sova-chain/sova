//! Sova chain constants, genesis configuration.
//!
//! This crate will hold the Sova network's chain IDs, genesis parameters,
//! and consensus-relevant constants. It is currently a build-scaffold stub.

/// Returns the name of this crate, used as a placeholder smoke test until
/// real chain-spec types land.
#[must_use]
pub fn crate_name() -> &'static str {
    "chainspec"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_crate_name() {
        assert_eq!(crate_name(), "chainspec");
    }
}
