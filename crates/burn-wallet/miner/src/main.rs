//! `sova-miner`: the D2 miner CLI.
//!
//! Speaks RPC only to a `zebrad`-compatible Zcash node -- never links reth
//! or `crates/evm` types (see `docs/WORKPLAN.md`'s standing rule and this
//! crate's `Cargo.toml` header comment). Four subcommands:
//!
//! - [`init`](Command::Init): create or load the keystore, print the t-addr
//!   to fund and the EVM address burns credit (by default the keystore
//!   key's own Ethereum address -- see [`evm_address`]).
//! - [`export-evm-key`](Command::ExportEvmKey): print the keystore's
//!   secret key for import into an EVM wallet, to spend the mined SOVA.
//! - [`mine`](Command::Mine): the budget-capped, per-epoch burn mining
//!   loop -- see [`mine::run`].
//! - [`report`](Command::Report): spend/earnings summary from the local
//!   state sidecar, optionally cross-checked against the chain.

mod anchor;
mod chain;
mod epoch;
mod evm_address;
mod fee;
mod funding;
mod mine;
mod node;
mod state;
mod verify;

use std::path::{Path, PathBuf};

use burn_wallet::rpc::RpcClient;
use burn_wallet::{Keypair, Network};
use clap::{Parser, Subcommand};

use crate::evm_address::{CreditTarget, classify, derive_evm_address, legacy_warning};
use crate::state::MinerState;

/// Sova miner CLI: budget-capped per-epoch SIP-1 burn mining against a
/// zebrad node.
#[derive(Debug, Parser)]
#[command(name = "sova-miner", version = env!("SOVA_BUILD_VERSION"), about)]
struct Cli {
    /// Directory holding this miner's keystore and state sidecar
    /// (`keystore.json`, `state.json`). Created by `init` if missing.
    #[arg(long, global = true, default_value = ".sova-miner")]
    data_dir: PathBuf,

    /// The Zcash network to build transactions for. Must match the node
    /// pointed to by `--rpc`.
    #[arg(long, global = true, default_value = "regtest", value_parser = parse_network)]
    network: Network,

    /// zebrad's RPC cookie file (`enable_cookie_auth = true`, the zebrad
    /// default), e.g. `~/.cache/zebra/.cookie`. Authenticates every call
    /// to `--rpc` / `--verify-rpc`, and is re-read if zebrad restarts with
    /// a new cookie. Omit it for a zebrad with cookie auth off.
    #[arg(long, global = true, env = "SOVA_MINER_RPC_COOKIE_FILE")]
    rpc_cookie_file: Option<PathBuf>,

    /// TESTING ONLY, regtest only: treat SIP-8 (anchored burns) as active
    /// from this Zcash height, like a Sova node told the same. No network
    /// has an activation height yet; on testnet and mainnet it will come
    /// with the release and this flag is refused there. It must match the
    /// Sova nodes' setting exactly: a version-2 burn mined below the real
    /// activation height is not a burn (its ZEC is destroyed, nothing is
    /// minted). Used by `mine --sova-rpc` and by `report --verify-rpc`.
    #[arg(long, global = true, value_name = "ZCASH_HEIGHT")]
    sip8_from: Option<u64>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create (or load) the keystore, and print the t-addr to fund and the
    /// EVM address this miner's burns credit.
    ///
    /// The default EVM address is the keystore key's own Ethereum address:
    /// import the key into any EVM wallet (`export-evm-key`) to spend the
    /// SOVA it earns. On an already-initialized data dir the recorded
    /// address is kept unless `--evm-address` or `--migrate-evm-address`
    /// says otherwise.
    Init {
        /// Credit this EVM address instead (hex, with or without a `0x`
        /// prefix, exactly 20 bytes). You need that address's own key to
        /// spend what it earns.
        #[arg(long, conflicts_with = "migrate_evm_address")]
        evm_address: Option<String>,
        /// Switch an existing miner's credit address to this keystore
        /// key's own EVM address -- the fix for keystores initialized with
        /// the old (unspendable) hash160 default. Restart the Sova node
        /// with the new `SOVA_MINER_EVM_ADDRESS` afterwards.
        #[arg(long)]
        migrate_evm_address: bool,
    },
    /// Print this miner's secret key (hex) for importing into an EVM
    /// wallet such as MetaMask, to spend the SOVA mined to the keystore
    /// key's own EVM address. The same key controls the ZEC at the t-addr.
    ExportEvmKey {
        /// Required: confirms you understand the key is printed in
        /// plaintext and that anyone who sees it can take both this
        /// miner's SOVA and the ZEC at its t-addr.
        #[arg(long)]
        i_understand: bool,
    },
    /// The mining loop: poll for new Zcash blocks, and while budget
    /// remains, submit one SIP-1 burn of `--per-epoch-zat` per new block.
    Mine {
        /// Total zatoshis (burn value + fee, summed across every epoch
        /// submitted by THIS run) this run may spend. Per-invocation (D5):
        /// each `mine` run gets its own fresh budget, measured against
        /// only what this run itself spends -- not this keystore's
        /// lifetime total (see `report` for that). Mining stops once the
        /// next epoch's cost would exceed what's left of this run's
        /// budget.
        #[arg(long)]
        budget_zat: u64,
        /// Zatoshis burned to the SIP-1 eater script per epoch.
        #[arg(long)]
        per_epoch_zat: u64,
        /// Optional additional cap on total zatoshis (burn value + fee)
        /// ever spent by this keystore, summed across EVERY `mine`
        /// invocation -- the pre-D5 lifetime-accounting behavior, opt-in
        /// for anyone who wants a hard ceiling across runs. Declared fresh
        /// each invocation like `--budget-zat` (not sticky): omit it to
        /// run with no lifetime cap, even if a prior run declared one.
        #[arg(long)]
        lifetime_budget_zat: Option<u64>,
        /// The zebrad-compatible JSON-RPC endpoint, e.g.
        /// `http://127.0.0.1:18232`.
        #[arg(long)]
        rpc: String,
        /// How often to poll `getblockcount` for a new tip, in
        /// milliseconds.
        #[arg(long, default_value_t = 1000)]
        poll_interval_ms: u64,
        /// Stop after this many epochs have been submitted in this run
        /// (in addition to stopping on budget exhaustion). Unbounded if
        /// omitted.
        #[arg(long)]
        max_epochs: Option<u64>,
        /// SIP-8 anchored burns: the JSON-RPC endpoint of YOUR OWN Sova
        /// node, e.g. `http://127.0.0.1:8545`. Each burn then also votes
        /// for that node's head block, once SIP-8 is active on this network
        /// (it is not active anywhere yet, so until then every burn stays a
        /// SIP-1 v1 burn and this says why at startup). A vote is only as
        /// good as the node it came from: pointing this at someone else's
        /// node hands them your vote. Omitted: v1 burns, and burning never
        /// waits on Sova.
        #[arg(long, value_name = "URL")]
        sova_rpc: Option<String>,
        /// With `--sova-rpc`: how long to wait, after a new Zcash block,
        /// for the Sova block that anchors it before referencing whatever
        /// the head is (SIP-8 §6: 10 s lets most burns vote for the newest
        /// block; about 12% then land one Zcash block later). 0 references
        /// the head at once.
        #[arg(long, value_name = "SECS", default_value_t = 10)]
        vote_wait: u64,
    },
    /// Print a spend/earnings summary from the local state sidecar.
    Report {
        /// If given, also show this miner's funding at the node
        /// (spendable, and coinbase separately: "maturing" on regtest,
        /// "must be shielded first" on testnet/mainnet, per `--network`),
        /// and scan this zebrad-compatible node's chain for
        /// SIP-1 burns crediting this miner's EVM address, and confirm the
        /// total/count matches the local report. Exits non-zero if it
        /// doesn't. The scan covers the blocks from this miner's first
        /// recorded epoch (or `--verify-from-height`) to the tip, never
        /// the whole chain.
        #[arg(long)]
        verify_rpc: Option<String>,
        /// First height `--verify-rpc` scans. Defaults to the lowest
        /// recorded epoch height on the current chain (nothing to scan if
        /// there is none).
        #[arg(long, requires = "verify_rpc")]
        verify_from_height: Option<u64>,
    },
}

fn parse_network(s: &str) -> Result<Network, String> {
    match s.to_ascii_lowercase().as_str() {
        "main" | "mainnet" => Ok(Network::Main),
        "test" | "testnet" => Ok(Network::Test),
        "regtest" => Ok(Network::Regtest),
        other => Err(format!(
            "unknown network {other:?}: expected one of main, test, regtest"
        )),
    }
}

/// Errors from the CLI's top-level command handlers.
#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error(transparent)]
    Keystore(#[from] burn_wallet::KeystoreError),
    #[error(transparent)]
    State(#[from] state::StateError),
    #[error(transparent)]
    Rpc(#[from] burn_wallet::RpcError),
    #[error(transparent)]
    Epoch(#[from] epoch::EpochError),
    #[error(transparent)]
    Verify(#[from] verify::VerifyError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid EVM address {given:?}: expected 20 bytes of hex, got {actual_len}")]
    InvalidEvmAddress { given: String, actual_len: usize },
    #[error("invalid EVM address hex: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("{0}")]
    Message(String),
}

fn parse_evm_address(hex_str: &str) -> Result<[u8; 20], CliError> {
    let trimmed = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    let bytes = hex::decode(trimmed)?;
    let len = bytes.len();
    bytes.try_into().map_err(|_| CliError::InvalidEvmAddress {
        given: hex_str.to_string(),
        actual_len: len,
    })
}

/// An RPC client for `url`, authenticated with the cookie file if one was
/// given.
fn rpc_client(url: &str, cookie_file: Option<&Path>) -> Result<RpcClient, CliError> {
    let rpc = RpcClient::new(url);
    match cookie_file {
        None => Ok(rpc),
        Some(path) => rpc.with_cookie_file(path).map_err(|e| {
            CliError::Message(format!(
                "cannot read --rpc-cookie-file {}: {e}",
                path.display()
            ))
        }),
    }
}

fn keystore_path(data_dir: &Path) -> PathBuf {
    data_dir.join("keystore.json")
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("state.json")
}

/// The one line `init` prints telling the user how to spend their SOVA.
/// Must never contain the lowercase phrase `evm address`: box/up.sh and
/// box/sim/*.sh parse `init` output with `awk '/evm address/'`.
const SPEND_HINT: &str =
    "import this key into an EVM wallet to spend your SOVA (`sova-miner export-evm-key`)";

/// `init`. Which EVM address burns credit, in priority order:
/// `--evm-address`; `--migrate-evm-address` (the key's own address); the
/// address already recorded in `state.json` (never changed silently -- a
/// legacy hash160 one gets a loud warning instead); for a fresh state, the
/// key's own Ethereum address.
fn cmd_init(
    data_dir: &Path,
    network: Network,
    evm_address_override: Option<String>,
    migrate_evm_address: bool,
) -> Result<(), CliError> {
    std::fs::create_dir_all(data_dir)?;
    let ks_path = keystore_path(data_dir);
    let st_path = state_path(data_dir);

    let keypair = if ks_path.exists() {
        println!(
            "keystore already exists at {}; loading it",
            ks_path.display()
        );
        Keypair::load_from_file(&ks_path)?
    } else {
        let kp = Keypair::generate();
        kp.save_to_file(&ks_path)?;
        println!("generated new keystore at {}", ks_path.display());
        kp
    };

    let address = keypair.encode_address(network);
    let existing = if st_path.exists() {
        Some(MinerState::load(&st_path)?)
    } else {
        None
    };
    let recorded = existing
        .as_ref()
        .map(|s| parse_evm_address(&s.evm_address_hex))
        .transpose()?;

    let evm_address = match (evm_address_override, recorded) {
        (Some(hex_str), _) => parse_evm_address(&hex_str)?,
        (None, _) if migrate_evm_address => derive_evm_address(&keypair),
        (None, Some(recorded)) => recorded,
        (None, None) => derive_evm_address(&keypair),
    };
    let evm_address_hex = hex::encode(evm_address);

    let mut state = existing.unwrap_or_else(|| MinerState::new(address.clone(), String::new()));
    state.address = address.clone();
    state.evm_address_hex = evm_address_hex.clone();
    state.save(&st_path)?;

    if let Some(old) = recorded.filter(|old| *old != evm_address) {
        println!(
            "credit target changed: 0x{} -> 0x{evm_address_hex} (burns already made still credit the old one; restart your Sova node with SOVA_MINER_EVM_ADDRESS=0x{evm_address_hex})",
            hex::encode(old)
        );
    }

    println!();
    println!("t-addr to fund:              {address}");
    println!("evm address (SIP-1 credit):  0x{evm_address_hex}");
    println!("keystore:                    {}", ks_path.display());
    println!("state:                       {}", st_path.display());
    println!();
    match classify(&keypair, evm_address) {
        CreditTarget::OwnKey => println!("{SPEND_HINT}"),
        CreditTarget::LegacyUnspendable => eprintln!("{}", legacy_warning(&keypair)),
        CreditTarget::External => println!(
            "burns credit the address given with --evm-address, not this key's own: spend that SOVA with that address's wallet"
        ),
    }
    Ok(())
}

/// What `export-evm-key` prints: the secret and the EVM address it
/// controls, plus the address burns currently credit if that differs.
#[derive(Debug)]
struct EvmKeyExport {
    /// `0x`-prefixed hex of the 32-byte secp256k1 secret.
    secret_hex: String,
    /// The EVM address that secret controls.
    evm_address: [u8; 20],
    /// The recorded credit address, when it is NOT `evm_address`.
    credits_elsewhere: Option<[u8; 20]>,
}

fn export_evm_key(data_dir: &Path, i_understand: bool) -> Result<EvmKeyExport, CliError> {
    if !i_understand {
        return Err(CliError::Message(
            "export-evm-key prints this miner's secret key in plaintext. Anyone who sees it can \
             spend this miner's SOVA AND the ZEC at its t-addr (it is the same key). Re-run with \
             --i-understand, somewhere nobody is watching, and paste it straight into your wallet's \
             \"import private key\" field"
                .to_string(),
        ));
    }
    let ks_path = keystore_path(data_dir);
    let keypair = Keypair::load_from_file(&ks_path).map_err(|e| {
        CliError::Message(format!(
            "{e} (run `sova-miner init` first -- expected a keystore at {})",
            ks_path.display()
        ))
    })?;
    let evm_address = derive_evm_address(&keypair);
    let st_path = state_path(data_dir);
    let credits_elsewhere = if st_path.exists() {
        let recorded = parse_evm_address(&MinerState::load(&st_path)?.evm_address_hex)?;
        (recorded != evm_address).then_some(recorded)
    } else {
        None
    };
    Ok(EvmKeyExport {
        secret_hex: format!("0x{}", hex::encode(keypair.secret_bytes())),
        evm_address,
        credits_elsewhere,
    })
}

fn cmd_export_evm_key(data_dir: &Path, i_understand: bool) -> Result<(), CliError> {
    let export = export_evm_key(data_dir, i_understand)?;
    eprintln!("WARNING: the line below is this miner's SECRET KEY, in plaintext.");
    eprintln!(
        "WARNING: it controls BOTH the SOVA at EVM address 0x{} AND the ZEC at this miner's t-addr.",
        hex::encode(export.evm_address)
    );
    eprintln!(
        "WARNING: import it into a wallet you trust, never paste it anywhere else, and clear your terminal/scrollback."
    );
    if let Some(recorded) = export.credits_elsewhere {
        eprintln!(
            "NOTE: this miner's burns currently credit 0x{}, which this key does NOT control (see `sova-miner init --migrate-evm-address`).",
            hex::encode(recorded)
        );
    }
    println!("{}", export.secret_hex);
    Ok(())
}

fn cmd_report(
    data_dir: &Path,
    network: Network,
    verify_rpc: Option<String>,
    verify_from_height: Option<u64>,
    rpc_cookie_file: Option<&Path>,
    sip8_from: Option<u64>,
) -> Result<(), CliError> {
    let st_path = state_path(data_dir);
    let state = MinerState::load(&st_path)?;

    println!("=== sova-miner report ({}) ===", st_path.display());
    println!("address:                 {}", state.address);
    println!("evm address:             0x{}", state.evm_address_hex);
    if let Ok(keypair) = Keypair::load_from_file(&keystore_path(data_dir))
        && classify(&keypair, parse_evm_address(&state.evm_address_hex)?)
            == CreditTarget::LegacyUnspendable
    {
        eprintln!("{}", legacy_warning(&keypair));
    }
    println!();
    println!("-- lifetime (across every `mine` invocation) --");
    println!("total burned:            {} zat", state.total_burned_zat);
    println!("total fees:              {} zat", state.total_fee_zat);
    println!("total spent:             {} zat", state.total_spent_zat());
    match state.lifetime_budget_zat {
        Some(cap) => {
            println!("lifetime budget cap:     {cap} zat (--lifetime-budget-zat)");
            println!(
                "lifetime budget left:    {} zat",
                state.lifetime_budget_remaining_zat().unwrap_or(0)
            );
        }
        None => println!("lifetime budget cap:     (none set)"),
    }
    println!();
    println!("-- most recent `mine` invocation (per-invocation budget, D5) --");
    println!("per-epoch:               {} zat", state.per_epoch_zat);
    println!("budget (--budget-zat):   {} zat", state.budget_zat);
    println!(
        "spent this invocation:  {} zat",
        state
            .total_spent_zat()
            .saturating_sub(state.invocation_start_spent_zat)
    );
    println!(
        "budget left this run:    {} zat",
        state.budget_remaining_zat()
    );
    println!();
    println!("-- current zcash chain --");
    match &state.chain_anchor {
        Some(a) => println!("chain anchor:            block {} = {}", a.height, a.hash),
        None => println!("chain anchor:            (not yet anchored; set by the next `mine`)"),
    }
    println!("epochs:                  {}", state.epochs.len());
    match &state.pending {
        Some(p) => println!(
            "burn in flight:          {} ({} zat + {} zat fee; expires after height {})",
            p.txid, p.burn_zat, p.fee_zat, p.expiry_height
        ),
        None => println!("burn in flight:          (none)"),
    }
    println!("burned on this chain:    {} zat", state.chain_burned_zat());
    if !state.retired_chains.is_empty() {
        let retired_epochs: usize = state.retired_chains.iter().map(|r| r.epochs.len()).sum();
        println!(
            "retired chains:          {} ({} epochs; still counted in lifetime totals)",
            state.retired_chains.len(),
            retired_epochs
        );
    }
    println!();
    println!("per-epoch history (current chain):");
    for e in &state.epochs {
        println!(
            "  epoch {:>3}  height {:>7}  burn {:>10} zat  fee {:>7} zat  change {:>10} zat  txid {}",
            e.epoch, e.height, e.burn_zat, e.fee_zat, e.change_zat, e.txid
        );
    }

    if let Some(url) = verify_rpc {
        let rpc = rpc_client(&url, rpc_cookie_file)?;
        let sip8 = anchor::Sip8Gate::new(
            anchor::resolve_activation(network, sip8_from).map_err(CliError::Message)?,
        );

        println!();
        println!(
            "=== funding at {url} ({network:?}: {}) ===",
            if network.allows_unshielded_coinbase_spends() {
                "coinbase funds burns once mature"
            } else {
                "coinbase must be shielded before it can fund a burn"
            }
        );
        let snapshot = node::Node::address_utxos(&rpc, &state.address)?;
        let mut funding = funding::Funding::new(network);
        let found = funding.classify(&rpc, &snapshot, &epoch::reserved(&state))?;
        println!("tip:                     {}", snapshot.tip_height);
        for (label, zat) in funding::balance_lines(&found, funding.coinbase_spendable()) {
            println!("{:<34} {zat} zat", format!("{label}:"));
        }
        if found.coinbase_zat > 0 {
            println!(
                "note: {}",
                burn_wallet::utxo::coinbase_must_be_shielded_message(found.coinbase_zat, "burn")
            );
        }

        println!();
        println!("=== on-chain verification against {url} ===");
        let tip = rpc.get_block_count()?;
        // Bounded: our burns can't be below our first recorded epoch, and
        // a from-genesis scan is millions of `getblock` calls on testnet.
        let from = verify_from_height
            .or_else(|| state.epochs.iter().map(|e| e.height).min())
            .unwrap_or(tip.saturating_add(1));
        let burns = if from <= tip {
            verify::scan_chain_burns(&rpc, from, tip, sip8)?
        } else {
            Vec::new()
        };
        let evm_bytes = parse_evm_address(&state.evm_address_hex)?;
        let ours: Vec<_> = burns
            .iter()
            .filter(|b| b.burn.evm_address == evm_bytes)
            .collect();
        let chain_count = ours.len();
        let chain_total: u64 = ours.iter().map(|b| b.burn.value_zat).sum();

        if from <= tip {
            println!("heights scanned:                {from}..={tip}");
        } else {
            println!(
                "heights scanned:                none (no recorded epochs; pass --verify-from-height to scan)"
            );
        }
        println!("chain burns found (our address): {chain_count} (total {chain_total} zat)");
        for b in &ours {
            let vote = b.reference.map_or_else(String::new, |r| {
                format!("  v2 ref {}:0x{}", r.height, hex::encode(r.hash))
            });
            println!(
                "  height {:>7}  txid {}  {} zat{vote}",
                b.height, b.txid, b.burn.value_zat
            );
        }
        println!("local epoch count:               {}", state.epochs.len());
        println!(
            "local burned (this chain):       {} zat",
            state.chain_burned_zat()
        );

        // Counts and totals alone can't catch a right-count-wrong-record
        // mismatch (e.g. a local record whose height or txid doesn't
        // actually correspond to what's on chain) -- compare the full
        // txid sets and call out any diff explicitly, not just a
        // pass/fail count.
        let chain_txids: std::collections::BTreeSet<&str> =
            ours.iter().map(|b| b.txid.as_str()).collect();
        let local_txids: std::collections::BTreeSet<&str> =
            state.epochs.iter().map(|e| e.txid.as_str()).collect();
        let missing_on_chain: Vec<&str> = local_txids.difference(&chain_txids).copied().collect();
        let missing_locally: Vec<&str> = chain_txids.difference(&local_txids).copied().collect();

        if !missing_on_chain.is_empty() {
            println!("txids in local state but NOT found on chain:");
            for t in &missing_on_chain {
                println!("  {t}");
            }
        }
        if !missing_locally.is_empty() {
            println!("txids found on chain but NOT in local state:");
            for t in &missing_locally {
                println!("  {t}");
            }
        }

        let matches = missing_on_chain.is_empty()
            && missing_locally.is_empty()
            && chain_total == state.chain_burned_zat();
        println!("MATCH: {}", if matches { "yes" } else { "no" });
        if !matches {
            return Err(CliError::Message(
                "report does not match on-chain state (see txid diff above)".to_string(),
            ));
        }
    }
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Init {
            evm_address,
            migrate_evm_address,
        } => cmd_init(&cli.data_dir, cli.network, evm_address, migrate_evm_address),
        Command::ExportEvmKey { i_understand } => cmd_export_evm_key(&cli.data_dir, i_understand),
        Command::Mine {
            budget_zat,
            per_epoch_zat,
            lifetime_budget_zat,
            rpc,
            poll_interval_ms,
            max_epochs,
            sova_rpc,
            vote_wait,
        } => mine::run(mine::MineArgs {
            data_dir: cli.data_dir.clone(),
            network: cli.network,
            budget_zat,
            per_epoch_zat,
            lifetime_budget_zat,
            rpc_url: rpc,
            rpc_cookie_file: cli.rpc_cookie_file.clone(),
            poll_interval_ms,
            max_epochs,
            sova_rpc,
            vote_wait: std::time::Duration::from_secs(vote_wait),
            sip8_from: cli.sip8_from,
        }),
        Command::Report {
            verify_rpc,
            verify_from_height,
        } => cmd_report(
            &cli.data_dir,
            cli.network,
            verify_rpc,
            verify_from_height,
            cli.rpc_cookie_file.as_deref(),
            cli.sip8_from,
        ),
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    /// Default unchanged: no cookie unless asked for.
    #[test]
    fn cookie_file_is_optional_and_global() {
        let args = ["sova-miner", "mine", "--rpc", "http://127.0.0.1:18232"];
        let base = ["--budget-zat", "1", "--per-epoch-zat", "1"];
        let cli = Cli::try_parse_from(args.iter().chain(&base)).unwrap();
        // The env var may be set in a developer's shell; only assert the
        // flag wins over it.
        let cli_flag = Cli::try_parse_from(
            args.iter()
                .chain(&base)
                .chain(&["--rpc-cookie-file", "/var/lib/zebrad/.cookie"]),
        )
        .unwrap();
        assert_eq!(
            cli_flag.rpc_cookie_file.as_deref(),
            Some(Path::new("/var/lib/zebrad/.cookie"))
        );
        if std::env::var_os("SOVA_MINER_RPC_COOKIE_FILE").is_none() {
            assert_eq!(cli.rpc_cookie_file, None);
        }
        // Global: accepted before the subcommand, and on `report` too.
        let report = Cli::try_parse_from([
            "sova-miner",
            "--rpc-cookie-file",
            "/c",
            "report",
            "--verify-rpc",
            "http://127.0.0.1:18232",
        ])
        .unwrap();
        assert_eq!(report.rpc_cookie_file.as_deref(), Some(Path::new("/c")));
    }

    fn recorded_evm(dir: &Path) -> [u8; 20] {
        parse_evm_address(&MinerState::load(&state_path(dir)).unwrap().evm_address_hex).unwrap()
    }

    /// A data dir as the pre-fix `init` left it: a keystore, and state
    /// recording the t-addr's hash160 as the credit address.
    fn legacy_data_dir() -> (tempfile::TempDir, Keypair) {
        let dir = tempfile::tempdir().unwrap();
        let kp = Keypair::generate();
        kp.save_to_file(&keystore_path(dir.path())).unwrap();
        let legacy = evm_address::legacy_evm_address(&kp);
        MinerState::new(kp.encode_address(Network::Regtest), hex::encode(legacy))
            .save(&state_path(dir.path()))
            .unwrap();
        (dir, kp)
    }

    #[test]
    fn fresh_init_credits_the_keys_own_ethereum_address() {
        let dir = tempfile::tempdir().unwrap();
        cmd_init(dir.path(), Network::Regtest, None, false).unwrap();
        let kp = Keypair::load_from_file(&keystore_path(dir.path())).unwrap();
        let evm = recorded_evm(dir.path());
        assert_eq!(evm, derive_evm_address(&kp));
        assert_eq!(classify(&kp, evm), CreditTarget::OwnKey);
        assert_ne!(evm, evm_address::legacy_evm_address(&kp));
        // And the exported key is that same key.
        let export = export_evm_key(dir.path(), true).unwrap();
        assert_eq!(
            export.secret_hex,
            format!("0x{}", hex::encode(kp.secret_bytes()))
        );
        assert_eq!(export.evm_address, evm);
        assert_eq!(export.credits_elsewhere, None);
    }

    /// Re-running `init` on a legacy data dir must NOT silently move where
    /// its burns credit; `--migrate-evm-address` does, to the key's own
    /// address, and says so.
    #[test]
    fn legacy_state_is_kept_by_init_and_fixed_by_migrate() {
        let (dir, kp) = legacy_data_dir();
        let legacy = evm_address::legacy_evm_address(&kp);

        cmd_init(dir.path(), Network::Regtest, None, false).unwrap();
        assert_eq!(recorded_evm(dir.path()), legacy);
        assert_eq!(
            classify(&kp, recorded_evm(dir.path())),
            CreditTarget::LegacyUnspendable
        );
        let export = export_evm_key(dir.path(), true).unwrap();
        assert_eq!(export.credits_elsewhere, Some(legacy));

        cmd_init(dir.path(), Network::Regtest, None, true).unwrap();
        assert_eq!(recorded_evm(dir.path()), derive_evm_address(&kp));
        // Idempotent once migrated.
        cmd_init(dir.path(), Network::Regtest, None, false).unwrap();
        assert_eq!(recorded_evm(dir.path()), derive_evm_address(&kp));
    }

    /// An `--evm-address` override survives a later plain `init` (the old
    /// `init` silently reset it to the derived default).
    #[test]
    fn explicit_override_is_kept_across_reinit() {
        let dir = tempfile::tempdir().unwrap();
        let custom = format!("0x{}", "ab".repeat(20));
        cmd_init(dir.path(), Network::Regtest, Some(custom), false).unwrap();
        assert_eq!(recorded_evm(dir.path()), [0xAB; 20]);
        cmd_init(dir.path(), Network::Regtest, None, false).unwrap();
        assert_eq!(recorded_evm(dir.path()), [0xAB; 20]);
    }

    #[test]
    fn export_evm_key_requires_i_understand() {
        let dir = tempfile::tempdir().unwrap();
        cmd_init(dir.path(), Network::Regtest, None, false).unwrap();
        let err = export_evm_key(dir.path(), false).unwrap_err();
        assert!(err.to_string().contains("--i-understand"), "{err}");
        // The flag parses, and only on export-evm-key.
        let cli = Cli::try_parse_from(["sova-miner", "export-evm-key", "--i-understand"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::ExportEvmKey { i_understand: true }
        ));
        let cli = Cli::try_parse_from(["sova-miner", "export-evm-key"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::ExportEvmKey {
                i_understand: false
            }
        ));
        // No keystore: a pointer to `init`, not a bare I/O error.
        let empty = tempfile::tempdir().unwrap();
        let err = export_evm_key(empty.path(), true).unwrap_err();
        assert!(err.to_string().contains("sova-miner init"), "{err}");
    }

    #[test]
    fn migrate_and_override_are_mutually_exclusive() {
        let res = Cli::try_parse_from([
            "sova-miner",
            "init",
            "--migrate-evm-address",
            "--evm-address",
            "0x00",
        ]);
        assert!(res.is_err());
    }

    /// box/up.sh and box/sim/*.sh take the address from `init` output with
    /// `awk '/evm address/ {print $NF}'`: exactly one line may match.
    #[test]
    fn spend_hint_does_not_collide_with_box_parsers() {
        assert!(!SPEND_HINT.contains("evm address"));
        assert!(SPEND_HINT.contains("export-evm-key"));
    }

    #[test]
    fn unreadable_cookie_file_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-cookie");
        let err = rpc_client("http://127.0.0.1:1", Some(&missing))
            .err()
            .unwrap();
        assert!(err.to_string().contains("--rpc-cookie-file"), "{err}");
        assert!(rpc_client("http://127.0.0.1:1", None).is_ok());
    }

    /// SIP-8 flags: off unless given; `--vote-wait` defaults to 10 s;
    /// `--sip8-from` is global (it also scopes `report --verify-rpc`).
    #[test]
    fn sip8_flags_default_to_off() {
        let base = [
            "sova-miner",
            "mine",
            "--rpc",
            "http://127.0.0.1:18232",
            "--budget-zat",
            "1",
            "--per-epoch-zat",
            "1",
        ];
        let cli = Cli::try_parse_from(base).unwrap();
        assert_eq!(cli.sip8_from, None);
        let Command::Mine {
            sova_rpc,
            vote_wait,
            ..
        } = cli.command
        else {
            panic!("expected mine");
        };
        assert_eq!(sova_rpc, None);
        assert_eq!(vote_wait, 10);

        let cli = Cli::try_parse_from(base.iter().chain(&[
            "--sova-rpc",
            "http://127.0.0.1:8545",
            "--vote-wait",
            "0",
            "--sip8-from",
            "150",
        ]))
        .unwrap();
        assert_eq!(cli.sip8_from, Some(150));
        let Command::Mine {
            sova_rpc,
            vote_wait,
            ..
        } = cli.command
        else {
            panic!("expected mine");
        };
        assert_eq!(sova_rpc.as_deref(), Some("http://127.0.0.1:8545"));
        assert_eq!(vote_wait, 0);

        let report = Cli::try_parse_from([
            "sova-miner",
            "--sip8-from",
            "150",
            "report",
            "--verify-rpc",
            "http://127.0.0.1:18232",
        ])
        .unwrap();
        assert_eq!(report.sip8_from, Some(150));
    }
}
