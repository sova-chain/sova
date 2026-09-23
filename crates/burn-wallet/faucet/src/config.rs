//! Faucet configuration: a TOML file, every limit with a conservative
//! default, validated before anything touches the key or the network.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use burn_wallet::Network;
use serde::Deserialize;

/// 1 TAZ in zatoshis.
pub(crate) const ZAT_PER_TAZ: u64 = 100_000_000;

/// Hard ceiling on the configured drip: anything above 10 TAZ is treated
/// as a typo (an extra zero) rather than a policy.
pub(crate) const MAX_DRIP_ZAT: u64 = 10 * ZAT_PER_TAZ;

/// Smallest drip worth sending: well above `consensus::sip1::MIN_BURN_ZAT`
/// plus a burn's ZIP-317 fee, so one drip can always fund at least one burn.
pub(crate) const MIN_DRIP_ZAT: u64 = 100_000;

/// Errors loading or validating the config.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    #[error("reading config {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// The on-disk config. Field docs double as the operator reference; see
/// `docs/ops/faucet.md` and `faucet/faucet.example.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FaucetConfig {
    /// `"test"` or `"regtest"`. `"main"` is refused: this faucet never runs
    /// against mainnet.
    pub network: String,
    /// HTTP listen address. Loopback only unless `allow_public_listen`.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Opt-in escape hatch for a non-loopback `listen`. The intended
    /// deployment is loopback behind a Cloudflare Tunnel, so leave it off.
    #[serde(default)]
    pub allow_public_listen: bool,
    /// zebrad JSON-RPC URL (must be the operator's own node, on loopback).
    pub zebrad_rpc: String,
    /// zebrad RPC cookie file (`enable_cookie_auth = true`, zebrad's
    /// default). Omit only for a cookie-less local regtest node.
    #[serde(default)]
    pub zebrad_cookie_file: Option<PathBuf>,
    /// The faucet's own keystore (`sova-faucet init` writes it). Must be
    /// mode 0600 and must not be any miner's keystore.
    pub keystore: PathBuf,
    /// Small JSON file holding cooldowns, the daily budget, and in-flight
    /// drips. The only state the faucet keeps.
    pub state_file: PathBuf,
    /// Fixed amount sent per drip, in zatoshis.
    #[serde(default = "default_drip_zat")]
    pub drip_zat: u64,
    /// Minimum seconds between two drips to the same address.
    #[serde(default = "default_cooldown_secs")]
    pub address_cooldown_secs: u64,
    /// Minimum seconds between two drips to the same client IP (IPv6
    /// clients are grouped by /64).
    #[serde(default = "default_cooldown_secs")]
    pub ip_cooldown_secs: u64,
    /// Total zatoshis (drips + fees) the faucet may send per UTC day.
    #[serde(default = "default_daily_cap_zat")]
    pub daily_cap_zat: u64,
    /// Warn loudly when the hot wallet holds more than this many times
    /// `daily_cap_zat`: a hot key should hold only a few days of drips.
    #[serde(default = "default_max_balance_multiple")]
    pub max_balance_multiple: u64,
    /// Header carrying the real client IP (e.g. `CF-Connecting-IP`). Unset
    /// (the default): the socket peer address is the client IP and every
    /// such header is ignored.
    #[serde(default)]
    pub trusted_proxy_header: Option<String>,
    /// Socket peers whose `trusted_proxy_header` is believed (the tunnel
    /// daemon's address). Requests from anyone else are keyed by their
    /// socket address, header or not.
    #[serde(default = "default_trusted_proxy_peers")]
    pub trusted_proxy_peers: Vec<IpAddr>,
    /// Seconds `/status` answers are cached (each one queries zebrad).
    #[serde(default = "default_status_cache_secs")]
    pub status_cache_secs: u64,
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 18790))
}
fn default_drip_zat() -> u64 {
    // 0.1 TAZ: dozens of burns at the box's 100,000-zat epoch size.
    ZAT_PER_TAZ / 10
}
fn default_cooldown_secs() -> u64 {
    24 * 60 * 60
}
fn default_daily_cap_zat() -> u64 {
    // 2 TAZ/day: about 20 drips.
    2 * ZAT_PER_TAZ
}
fn default_max_balance_multiple() -> u64 {
    5
}
fn default_trusted_proxy_peers() -> Vec<IpAddr> {
    vec![
        IpAddr::from([127, 0, 0, 1]),
        IpAddr::from([0u16, 0, 0, 0, 0, 0, 0, 1]),
    ]
}
fn default_status_cache_secs() -> u64 {
    10
}

impl FaucetConfig {
    /// Reads and validates the config at `path`.
    pub(crate) fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let cfg = Self::parse(&text).map_err(|e| match e {
            ConfigError::Parse { source, .. } => ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            },
            other => other,
        })?;
        Ok(cfg)
    }

    /// Parses and validates config text.
    pub(crate) fn parse(text: &str) -> Result<Self, ConfigError> {
        let cfg: Self = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: PathBuf::new(),
            source: Box::new(source),
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// The network, parsed. Validation guarantees it is not mainnet.
    pub(crate) fn network(&self) -> Network {
        parse_network(&self.network).unwrap_or(Network::Test)
    }

    /// The max-balance guard threshold, in zatoshis.
    pub(crate) fn max_balance_zat(&self) -> u64 {
        self.daily_cap_zat.saturating_mul(self.max_balance_multiple)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        match parse_network(&self.network) {
            Ok(Network::Main) => {
                return invalid(
                    "network = \"main\" is refused: sova-faucet is testnet/regtest only".into(),
                );
            }
            Ok(_) => {}
            Err(e) => return invalid(e),
        }
        if !self.listen.ip().is_loopback() && !self.allow_public_listen {
            return invalid(format!(
                "listen = {} is not loopback; the faucet belongs behind a tunnel on 127.0.0.1 (set allow_public_listen = true to override)",
                self.listen
            ));
        }
        if !(MIN_DRIP_ZAT..=MAX_DRIP_ZAT).contains(&self.drip_zat) {
            return invalid(format!(
                "drip_zat = {} is outside {MIN_DRIP_ZAT}..={MAX_DRIP_ZAT}",
                self.drip_zat
            ));
        }
        if self.daily_cap_zat < self.drip_zat {
            return invalid(format!(
                "daily_cap_zat = {} is below drip_zat = {}: no drip could ever be sent",
                self.daily_cap_zat, self.drip_zat
            ));
        }
        if self.max_balance_multiple == 0 {
            return invalid("max_balance_multiple must be at least 1".into());
        }
        if let Some(h) = &self.trusted_proxy_header {
            if h.trim().is_empty() {
                return invalid("trusted_proxy_header is empty; remove it instead".into());
            }
            if self.trusted_proxy_peers.is_empty() {
                return invalid(
                    "trusted_proxy_header is set but trusted_proxy_peers is empty".into(),
                );
            }
        }
        Ok(())
    }
}

/// Parses a network name; `main` parses (so it can be refused by name).
pub(crate) fn parse_network(s: &str) -> Result<Network, String> {
    match s.to_ascii_lowercase().as_str() {
        "main" | "mainnet" => Ok(Network::Main),
        "test" | "testnet" => Ok(Network::Test),
        "regtest" => Ok(Network::Regtest),
        other => Err(format!(
            "unknown network {other:?}: expected \"test\" or \"regtest\""
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
network = "test"
zebrad_rpc = "http://127.0.0.1:18232"
keystore = "/tmp/k.json"
state_file = "/tmp/s.json"
"#;

    #[test]
    fn minimal_config_gets_conservative_defaults() {
        let cfg = FaucetConfig::parse(MINIMAL).unwrap();
        assert_eq!(cfg.network(), Network::Test);
        assert_eq!(cfg.drip_zat, 10_000_000);
        assert_eq!(cfg.daily_cap_zat, 200_000_000);
        assert_eq!(cfg.address_cooldown_secs, 86_400);
        assert_eq!(cfg.ip_cooldown_secs, 86_400);
        assert_eq!(cfg.max_balance_zat(), 1_000_000_000);
        assert!(cfg.listen.ip().is_loopback());
        assert!(cfg.trusted_proxy_header.is_none());
    }

    #[test]
    fn shipped_example_config_parses_to_the_defaults() {
        let cfg = FaucetConfig::parse(include_str!("../faucet.example.toml")).unwrap();
        let min = FaucetConfig::parse(MINIMAL).unwrap();
        assert_eq!(cfg.drip_zat, min.drip_zat);
        assert_eq!(cfg.daily_cap_zat, min.daily_cap_zat);
        assert_eq!(cfg.address_cooldown_secs, min.address_cooldown_secs);
        assert_eq!(cfg.ip_cooldown_secs, min.ip_cooldown_secs);
        assert_eq!(cfg.max_balance_multiple, min.max_balance_multiple);
        assert_eq!(cfg.listen, min.listen);
    }

    #[test]
    fn mainnet_is_refused() {
        for name in ["main", "mainnet", "MAIN"] {
            let text = MINIMAL.replace("\"test\"", &format!("\"{name}\""));
            let err = FaucetConfig::parse(&text).unwrap_err().to_string();
            assert!(err.contains("refused"), "{name}: {err}");
        }
    }

    #[test]
    fn unknown_network_and_unknown_keys_are_refused() {
        assert!(FaucetConfig::parse(&MINIMAL.replace("\"test\"", "\"signet\"")).is_err());
        assert!(FaucetConfig::parse(&format!("{MINIMAL}\ndrip_amount = 5\n")).is_err());
    }

    #[test]
    fn public_listen_needs_explicit_opt_in() {
        let text = format!("{MINIMAL}\nlisten = \"0.0.0.0:18790\"\n");
        assert!(FaucetConfig::parse(&text).is_err());
        let text = format!("{text}allow_public_listen = true\n");
        assert!(FaucetConfig::parse(&text).is_ok());
    }

    #[test]
    fn drip_and_cap_bounds() {
        assert!(FaucetConfig::parse(&format!("{MINIMAL}\ndrip_zat = 99999\n")).is_err());
        assert!(FaucetConfig::parse(&format!("{MINIMAL}\ndrip_zat = 1000000001\n")).is_err());
        let text = format!("{MINIMAL}\ndrip_zat = 5000000\ndaily_cap_zat = 4000000\n");
        assert!(FaucetConfig::parse(&text).is_err());
        assert!(FaucetConfig::parse(&format!("{MINIMAL}\nmax_balance_multiple = 0\n")).is_err());
    }
}
