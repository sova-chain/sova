//! `sova-faucet`: a small, capped, rate-limited TAZ (testnet ZEC) faucet.
//!
//! This holds the ONE hot key allowed on Sova project infrastructure
//! (infra-2 decision D5; `docs/design/infra-m1.md` §4 rule 1): a
//! testnet-only transparent key, on its own host, holding only a few days
//! of drips. Its blast radius is testnet TAZ, which has no market value.
//! It refuses to start against mainnet (config and node both checked) and
//! has no admin endpoints. See `docs/ops/faucet.md`.
//!
//! Subcommands:
//! - `init`: create the faucet's own keystore (never a miner's) and print
//!   the t-address to fund.
//! - `address`: print the faucet t-address for a config.
//! - `run`: preflight checks, then serve `POST /drip` and `GET /status`.

mod address;
mod config;
mod faucet;
mod http;
mod node;
mod state;

use std::path::{Path, PathBuf};
use std::time::Duration;

use burn_wallet::{Keypair, RpcClient};
use clap::{Parser, Subcommand};

use crate::config::{FaucetConfig, parse_network};
use crate::faucet::Faucet;

/// A capped, rate-limited TAZ faucet with an isolated testnet-only key.
#[derive(Debug, Parser)]
#[command(name = "sova-faucet", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create the faucet keystore (refuses to overwrite) and print its
    /// t-address.
    Init {
        /// Where to write the keystore (mode 0600).
        #[arg(long)]
        keystore: PathBuf,
        /// `test` or `regtest` (only affects the printed address).
        #[arg(long, default_value = "test")]
        network: String,
    },
    /// Print the faucet t-address for a config.
    Address {
        #[arg(long)]
        config: PathBuf,
    },
    /// Run the faucet.
    Run {
        #[arg(long)]
        config: PathBuf,
    },
}

type AnyError = Box<dyn std::error::Error + Send + Sync>;

/// Loads the faucet key, refusing a keystore readable by group/others.
fn load_key(path: &Path) -> Result<Keypair, AnyError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|e| format!("faucet keystore {}: {e}", path.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "faucet keystore {} has mode {:o}; it is a hot key and must be 0600 (chmod 600 it)",
                path.display(),
                mode & 0o777
            )
            .into());
        }
    }
    Ok(Keypair::load_from_file(path)?)
}

fn cmd_init(keystore: &Path, network: &str) -> Result<(), AnyError> {
    let network = parse_network(network)?;
    if network == burn_wallet::Network::Main {
        return Err("sova-faucet keys are testnet/regtest only".into());
    }
    if keystore.exists() {
        return Err(format!(
            "{} already exists; refusing to overwrite a key",
            keystore.display()
        )
        .into());
    }
    if let Some(dir) = keystore.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let kp = Keypair::generate();
    kp.save_to_file(keystore)?;
    println!("faucet keystore: {} (mode 0600)", keystore.display());
    println!("faucet t-addr:   {}", kp.encode_address(network));
    println!("This is a HOT key: fund it with a few days of drips at most.");
    Ok(())
}

fn cmd_address(config: &Path) -> Result<(), AnyError> {
    let cfg = FaucetConfig::load(config)?;
    let kp = load_key(&cfg.keystore)?;
    println!("{}", kp.encode_address(cfg.network()));
    Ok(())
}

fn cmd_run(config: &Path) -> Result<(), AnyError> {
    let cfg = FaucetConfig::load(config)?;
    let keypair = load_key(&cfg.keystore)?;
    let mut rpc = RpcClient::with_timeout(cfg.zebrad_rpc.clone(), Duration::from_secs(30));
    if let Some(cookie) = &cfg.zebrad_cookie_file {
        rpc = rpc
            .with_cookie_file(cookie)
            .map_err(|e| format!("zebrad cookie file {}: {e}", cookie.display()))?;
    }
    let now = http::unix_now();
    let mut faucet = Faucet::start(cfg.clone(), keypair, rpc, now)?;
    let status = faucet.status(now)?;
    println!(
        "sova-faucet: network={} address={} zebrad={} tip={}",
        cfg.network,
        faucet.address(),
        cfg.zebrad_rpc,
        status.tip_height
    );
    println!(
        "  balance={} zat (immature {} zat, coinbase (must be shielded first) {} zat, in flight {} zat)  drip={} zat  daily cap={} zat (left today {} zat)",
        status.balance_zat,
        status.immature_zat,
        status.coinbase_unshielded_zat,
        status.in_flight_zat,
        status.drip_zat,
        status.daily_cap_zat,
        status.remaining_today_zat
    );
    if status.coinbase_unshielded_zat > 0 {
        println!(
            "  note: {}",
            burn_wallet::utxo::coinbase_must_be_shielded_message(
                status.coinbase_unshielded_zat,
                "drip"
            )
        );
    }
    println!(
        "  cooldowns: address {}s, ip {}s  proxy header: {}  max balance guard: {} zat{}",
        cfg.address_cooldown_secs,
        cfg.ip_cooldown_secs,
        cfg.trusted_proxy_header
            .as_deref()
            .unwrap_or("(none; socket peer)"),
        status.max_balance_zat,
        if status.over_max_balance {
            " -- OVER LIMIT"
        } else {
            ""
        }
    );
    http::serve(&cfg, &mut faucet)
}

fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Init { keystore, network } => cmd_init(keystore, network),
        Command::Address { config } => cmd_address(config),
        Command::Run { config } => cmd_run(config),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
