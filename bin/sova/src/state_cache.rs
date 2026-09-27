//! Size of reth's cross-block state cache (`--engine.cross-block-cache-size`).
//!
//! reth v2.6.0 defaults it to 4 GiB, sized for Ethereum mainnet on large
//! machines. Sova's state is tiny, and the project's 4 GB testnet hosts also
//! run zebrad: on 2026-09-27 the cache filled every such host within hours
//! (the `sova` process at ~2.4 GB), zebrad stopped answering and the chain
//! stalled. [`DEFAULT_CROSS_BLOCK_CACHE_MB`] (256 MiB) is ample for Sova;
//! `SOVA_CROSS_BLOCK_CACHE_MB` overrides it.

/// The environment variable that overrides [`DEFAULT_CROSS_BLOCK_CACHE_MB`].
pub(crate) const ENV: &str = "SOVA_CROSS_BLOCK_CACHE_MB";

/// Sova's default cache size, in MiB.
pub(crate) const DEFAULT_CROSS_BLOCK_CACHE_MB: usize = 256;

/// The accepted range, in MiB: below 32 the cache is useless, above 16 GiB
/// is a typo.
const RANGE_MB: std::ops::RangeInclusive<usize> = 32..=16 * 1024;

/// Parse a `SOVA_CROSS_BLOCK_CACHE_MB` value; unset or empty means the
/// default. Anything else must be whole MiB in 32..=16384, so a typo never
/// starts a node with a surprising cache.
pub(crate) fn parse(raw: Option<&str>) -> eyre::Result<usize> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(DEFAULT_CROSS_BLOCK_CACHE_MB);
    };
    match raw.parse::<usize>() {
        Ok(mb) if RANGE_MB.contains(&mb) => Ok(mb),
        _ => Err(eyre::eyre!(
            "{ENV} must be whole MiB from {} to {}, got {raw:?}",
            RANGE_MB.start(),
            RANGE_MB.end()
        )),
    }
}

/// Read the cache size from [`ENV`].
pub(crate) fn from_env() -> eyre::Result<usize> {
    parse(std::env::var(ENV).ok().as_deref())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_empty_is_the_small_default() {
        assert_eq!(parse(None).unwrap(), 256);
        assert_eq!(parse(Some("  ")).unwrap(), 256);
    }

    #[test]
    fn an_override_in_range_is_taken() {
        assert_eq!(parse(Some("512")).unwrap(), 512);
        assert_eq!(parse(Some("32")).unwrap(), 32);
        assert_eq!(parse(Some("16384")).unwrap(), 16384);
    }

    #[test]
    fn a_typo_refuses_to_start() {
        for bad in ["0", "31", "16385", "4g", "-1", "1.5"] {
            assert!(parse(Some(bad)).is_err(), "{bad}");
        }
    }
}
