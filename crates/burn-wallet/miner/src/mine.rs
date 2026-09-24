//! The `mine` subcommand: poll a zebrad node for new blocks, and while
//! budget remains, submit one SIP-1 burn of `per_epoch_zat` per new block
//! observed -- at most one burn in flight at a time, followed until it
//! actually confirms (see `crate::epoch::resolve_pending`) before the next
//! is built, so the last epoch of a run is always confirmed on-chain by the
//! time this function returns.
//!
//! State (including the UTXO pool and the in-flight burn) is saved to the
//! sidecar right after every broadcast and every confirmation, not batched
//! until exit -- so `report` reflects true progress at any point, and
//! killing the process (Ctrl+C or otherwise) loses nothing: a restart
//! picks the in-flight burn back up instead of spending its inputs again.
//!
//! Budget (D5): `--budget-zat` is **per-invocation** -- each `mine` run
//! snapshots the keystore's lifetime spend at startup
//! (`MinerState::begin_invocation`) and measures its own budget against
//! only what happens from there, so a fresh run always gets its full
//! declared budget regardless of what prior runs already spent. An
//! optional `--lifetime-budget-zat` adds back a cumulative cap across
//! every invocation, for anyone who wants the old (pre-D5) behavior
//! explicitly. See the miner README's "Budgets" section for the
//! user-facing statement of both flags' semantics.
//!
//! SIP-8 (anchored burns): with `--sova-rpc`, and only once SIP-8 is active
//! on the network, each burn also references the head of the miner's own
//! Sova node (a vote) -- see `crate::anchor`. Without `--sova-rpc`, or
//! before activation, every burn is the SIP-1 v1 burn this loop has always
//! sent, and nothing here waits on Sova.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use burn_wallet::rpc::RpcClient;
use burn_wallet::{Keypair, Network};
use consensus::sip1::SovaRef;

use crate::anchor::{Anchoring, SOVA_RPC_TIMEOUT, Sip8Gate, resolve_activation};
use crate::chain::{ANCHOR_HEIGHT, ChainCheck, check_chain};
use crate::epoch::{
    BudgetKind, EpochError, EpochOutcome, PendingResolution, attempt_epoch, resolve_pending,
    wait_for_pending,
};
use crate::evm_address::{CreditTarget, classify, legacy_warning};
use crate::funding::Funding;
use crate::state::MinerState;
use crate::{CliError, keystore_path, parse_evm_address, rpc_client, state_path};

/// How many times to retry one epoch after a transient RPC failure before
/// giving up on the whole run. Node hiccups (a `sendrawtransaction` racing
/// a block being produced, a momentarily busy node) are the expected
/// failure mode here; a deterministic build/funding error is not retried at
/// all (see [`attempt_epoch_with_retries`]).
const MAX_RPC_RETRIES: u32 = 5;
/// Delay between retries of a transient RPC failure.
const RPC_RETRY_DELAY: Duration = Duration::from_millis(500);
/// How long to block on a just-broadcast burn before going back to watching
/// blocks. Generous relative to `box/regtest`'s few-second block cadence;
/// on testnet (75 s blocks) a burn often takes longer, and that is fine: it
/// stays in flight (inputs reserved, state saved) and is checked again on
/// every new block until it is mined or passes its expiry height.
const CONFIRMATION_WAIT: Duration = Duration::from_secs(60);

/// Parsed arguments for the `mine` subcommand.
pub(crate) struct MineArgs {
    /// The `--data-dir` holding `keystore.json`/`state.json`.
    pub data_dir: PathBuf,
    /// The network to build transactions for.
    pub network: Network,
    /// Total zatoshis (burn + fee, across the whole run) this run may
    /// spend. Per-invocation (D5): measured against spend since *this*
    /// invocation started, not this keystore's lifetime total -- see
    /// `crate::state::MinerState::begin_invocation`.
    pub budget_zat: u64,
    /// Zatoshis burned per epoch.
    pub per_epoch_zat: u64,
    /// Optional additional cap (D5) on total zatoshis ever spent by this
    /// keystore, summed across every `mine` invocation -- the old
    /// (pre-D5) lifetime-accounting semantics, opt-in. `None` means no
    /// lifetime cap is enforced this run.
    pub lifetime_budget_zat: Option<u64>,
    /// The zebrad-compatible RPC endpoint.
    pub rpc_url: String,
    /// zebrad's RPC cookie file, if its cookie auth is on.
    pub rpc_cookie_file: Option<PathBuf>,
    /// Poll interval for `getblockcount`, in milliseconds.
    pub poll_interval_ms: u64,
    /// Optional cap on epochs submitted in this run.
    pub max_epochs: Option<u64>,
    /// `--sova-rpc`: the miner's own Sova node, for SIP-8 votes. `None`:
    /// v1 burns only, and nothing waits on Sova.
    pub sova_rpc: Option<String>,
    /// `--vote-wait`: how long to wait for the Sova block anchoring a new
    /// Zcash tip before referencing the head as it is.
    pub vote_wait: Duration,
    /// `--sip8-from` (regtest testing only): SIP-8's activation height,
    /// when the network has none built in.
    pub sip8_from: Option<u64>,
}

/// Runs the mining loop until budget is exhausted, `max_epochs` is
/// reached, or an unrecoverable error occurs.
///
/// # Errors
///
/// Returns [`CliError`] if the keystore/state can't be loaded, or if an
/// epoch fails in a way retries can't recover from (see
/// [`attempt_epoch_with_retries`]).
pub(crate) fn run(args: MineArgs) -> Result<(), CliError> {
    let ks_path = keystore_path(&args.data_dir);
    let st_path = state_path(&args.data_dir);

    let keypair = Keypair::load_from_file(&ks_path).map_err(|e| {
        CliError::Message(format!(
            "{e} (run `sova-miner init` first -- expected a keystore at {})",
            ks_path.display()
        ))
    })?;
    let mut state = MinerState::load(&st_path).map_err(|e| {
        CliError::Message(format!(
            "{e} (run `sova-miner init` first -- expected state at {})",
            st_path.display()
        ))
    })?;

    let evm_address = parse_evm_address(&state.evm_address_hex)?;
    let rpc = rpc_client(&args.rpc_url, args.rpc_cookie_file.as_deref())?;
    let mut funding = Funding::new(args.network);

    let lifetime_suffix = match args.lifetime_budget_zat {
        Some(cap) => format!(" lifetime-budget={cap}zat"),
        None => String::new(),
    };
    println!(
        "sova-miner mine: address={} evm=0x{} rpc={}{} budget={}zat per-epoch={}zat{}",
        state.address,
        state.evm_address_hex,
        args.rpc_url,
        if args.rpc_cookie_file.is_some() {
            " (cookie auth)"
        } else {
            ""
        },
        args.budget_zat,
        args.per_epoch_zat,
        lifetime_suffix
    );
    // A keystore initialized with the old hash160 default keeps crediting
    // it (never switched silently mid-run -- the node must be told too),
    // but loudly: SOVA there is unspendable. See `crate::evm_address`.
    if classify(&keypair, evm_address) == CreditTarget::LegacyUnspendable {
        eprintln!("{}", legacy_warning(&keypair));
    }
    // SIP-8: `None` (no `--sova-rpc`) is today's v1 miner exactly. With it,
    // say once whether votes will be cast, and if not, why.
    let gate =
        Sip8Gate::new(resolve_activation(args.network, args.sip8_from).map_err(CliError::Message)?);
    let mut anchoring = args.sova_rpc.as_ref().map(|url| {
        Anchoring::new(
            RpcClient::with_timeout(url.clone(), SOVA_RPC_TIMEOUT),
            url.clone(),
            gate,
            args.vote_wait,
        )
    });
    if let Some(anchoring) = &anchoring {
        println!("{}", anchoring.startup_line(args.network));
    }

    // E1d: before trusting any tracked UTXO, make sure the node still
    // serves the chain this state was built on (see `crate::chain`).
    report_chain_check(check_chain(&rpc, &mut state)?);
    // A burn an earlier run left in flight (it was killed, or the burn
    // outlasted its confirmation wait): settle it before this run's budget
    // snapshot, so a burn that confirmed in the meantime is charged to the
    // run that sent it.
    if let Some(resolution) = resolve_pending(&rpc, &mut state)? {
        report_resolution(&resolution, "from an earlier run");
    }

    // D5: `begin_invocation` snapshots the lifetime spend so far into
    // `invocation_start_spent_zat`, so `budget_remaining_zat` below
    // measures only what THIS run spends -- not everything this keystore
    // has ever spent. See the module docs and `MinerState::begin_invocation`.
    state.begin_invocation(
        args.budget_zat,
        args.per_epoch_zat,
        args.lifetime_budget_zat,
    );
    state.save(&st_path)?;

    let mut last_height = rpc.get_block_count()?;
    println!("baseline tip height: {last_height} (epochs trigger on new blocks past this)");

    let mut epochs_this_run: u64 = 0;

    loop {
        thread::sleep(Duration::from_millis(args.poll_interval_ms));

        let height = match rpc.get_block_count() {
            Ok(h) => h,
            Err(e) => {
                eprintln!("warning: getblockcount failed: {e}; will retry");
                continue;
            }
        };

        if height == last_height {
            continue;
        }
        // The tip moved: re-check the chain identity before spending, so a
        // node recreated under a running miner (tip regressing, or even
        // jumping past the old height) is caught before a burn tries to
        // spend inputs from the dead chain. One `getblockhash` per new tip.
        match check_chain(&rpc, &mut state) {
            Ok(ChainCheck::Same | ChainCheck::Anchored) => {}
            Ok(ChainCheck::Undetermined { tip }) => {
                report_chain_check(ChainCheck::Undetermined { tip });
                last_height = height;
                continue;
            }
            Ok(check @ ChainCheck::Reset { .. }) => {
                report_chain_check(check);
                state.save(&st_path)?;
                // A different chain: its heights have nothing to do with
                // the old ones. Start counting new blocks from here.
                last_height = height;
                println!("new baseline tip height: {last_height}");
                continue;
            }
            Err(e) => {
                eprintln!("warning: chain identity check failed: {e}; will retry");
                continue;
            }
        }

        // At most one burn in flight: one still unmined from an earlier
        // block (a slow confirmation) is followed up before a new one is
        // built -- its inputs stay reserved meanwhile.
        if state.pending.is_some() {
            match resolve_pending(&rpc, &mut state) {
                Ok(Some(PendingResolution::InFlight { txid })) => {
                    println!(
                        "burn {txid} still unmined at tip {height}; waiting for it before the next epoch"
                    );
                    last_height = height;
                    continue;
                }
                Ok(Some(resolution)) => {
                    state.save(&st_path)?;
                    report_resolution(&resolution, "");
                    if let PendingResolution::Confirmed(record) = &resolution {
                        last_height = last_height.max(record.height);
                        epochs_this_run += 1;
                        if reached_max_epochs(args.max_epochs, epochs_this_run) {
                            return Ok(());
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("warning: checking the in-flight burn failed: {e}; will retry");
                    continue;
                }
            }
        }

        while last_height < height {
            // A target, not a promise: this process only reacts to blocks
            // someone else already mined, so the epoch we're about to
            // submit can, at absolute earliest, confirm in the *next*
            // block after this one -- the real confirming height is read
            // back once it happens (see `resolve_pending`), and that's what
            // we advance `last_height` to below, not this.
            let next_height = last_height + 1;
            let target_height = u32::try_from(next_height).unwrap_or(u32::MAX);
            // SIP-8: short of a Zcash reorg, the burn can't be mined below
            // `target_height` (the tip is already at least `last_height`),
            // so that is the height the activation guard checks. Freshness
            // is judged against the tip as it is now, not when this pass
            // started.
            let sova_ref = anchoring.as_mut().and_then(|anchoring| {
                let tip = rpc.get_block_count().unwrap_or(height);
                anchoring.reference_for(&rpc, u64::from(target_height), tip)
            });

            let outcome = attempt_epoch_with_retries(
                &rpc,
                &mut funding,
                args.network,
                &keypair,
                evm_address,
                0,
                args.per_epoch_zat,
                target_height,
                sova_ref,
                &mut state,
            )?;

            match outcome {
                EpochOutcome::Submitted(pending) => {
                    // Persist the in-flight burn before waiting on it: a
                    // kill from here on neither loses the epoch nor lets a
                    // restart spend its inputs again.
                    state.save(&st_path)?;
                    let vote = sova_ref.map_or_else(String::new, |r| {
                        format!(" payload=v2 ref={}:0x{}", r.height, hex::encode(r.hash))
                    });
                    println!(
                        "burn broadcast: txid={} burn={}zat fee={}zat change={}zat{vote} (awaiting confirmation)",
                        pending.txid, pending.burn_zat, pending.fee_zat, pending.change_zat
                    );
                    // Block until the node confirms it, so the recorded
                    // height is the real confirming height and the last
                    // epoch of a run is on-chain before we return.
                    match wait_for_pending(&rpc, &mut state, CONFIRMATION_WAIT) {
                        Some(PendingResolution::Confirmed(record)) => {
                            state.save(&st_path)?;
                            report_resolution(&PendingResolution::Confirmed(record.clone()), "");
                            // Advance past the block the burn actually
                            // confirmed in -- at least `next_height`, later
                            // if confirmation took more than one block
                            // interval. Using the real confirmed height
                            // keeps this loop's "new block" trigger honest.
                            last_height = record.height;
                            epochs_this_run += 1;
                        }
                        Some(resolution) => {
                            // Still unmined after the wait (or dead): not
                            // an error. It stays reserved and is followed
                            // up on each new block (above).
                            state.save(&st_path)?;
                            report_resolution(&resolution, "");
                            last_height = height;
                            break;
                        }
                        None => break,
                    }
                }
                EpochOutcome::BudgetExhausted {
                    kind,
                    needed_zat,
                    remaining_zat,
                } => {
                    let cap_flag = match kind {
                        BudgetKind::PerInvocation => "--budget-zat (this run)",
                        BudgetKind::Lifetime => "--lifetime-budget-zat (across all runs)",
                    };
                    println!(
                        "budget exhausted at height {next_height}: next epoch needs {needed_zat} zat, only {remaining_zat} zat remain under {cap_flag} -- stopping."
                    );
                    return Ok(());
                }
            }

            if reached_max_epochs(args.max_epochs, epochs_this_run) {
                return Ok(());
            }
        }
    }
}

/// Whether `--max-epochs` (if given) has been reached; says so if it has.
fn reached_max_epochs(max_epochs: Option<u64>, epochs_this_run: u64) -> bool {
    match max_epochs {
        Some(max) if epochs_this_run >= max => {
            println!("reached --max-epochs {max} -- stopping.");
            true
        }
        _ => false,
    }
}

/// Prints what became of an in-flight burn for the log.
fn report_resolution(resolution: &PendingResolution, context: &str) {
    let context = if context.is_empty() {
        String::new()
    } else {
        format!(" ({context})")
    };
    match resolution {
        PendingResolution::Confirmed(record) => println!(
            "epoch {}: height={} burn={}zat fee={}zat change={}zat txid={}{context}",
            record.epoch,
            record.height,
            record.burn_zat,
            record.fee_zat,
            record.change_zat,
            record.txid
        ),
        PendingResolution::InFlight { txid } => println!(
            "burn {txid} not mined yet{context}; its inputs stay reserved and it is checked again on each new block"
        ),
        PendingResolution::Dropped { txid, reason } => {
            println!("burn {txid} dropped{context}: {reason}");
        }
    }
}

/// Prints the outcome of a [`check_chain`] call for the log.
fn report_chain_check(check: ChainCheck) {
    match check {
        ChainCheck::Same => println!("zcash chain: same chain as recorded state"),
        ChainCheck::Anchored => {
            println!("zcash chain: state anchored to block {ANCHOR_HEIGHT} of the node's chain")
        }
        ChainCheck::Undetermined { tip } => println!(
            "zcash chain: node tip {tip} is below anchor height {ANCHOR_HEIGHT}; waiting for blocks before comparing"
        ),
        ChainCheck::Reset { reason } => {
            println!("zcash chain RESET detected: {reason}");
            println!(
                "  retired this chain's tracked UTXOs and epoch history (kept under retired_chains in state.json); keystore and lifetime totals kept; will re-discover funding on the node's chain"
            );
        }
    }
}

/// Attempts one epoch, retrying RPC failures and node rejections up to
/// [`MAX_RPC_RETRIES`] times. Each retry re-reads the address's funding
/// first, so a rejection over a stale input is not repeated; a burn the
/// node actually has is never reported as a failure here (see
/// `crate::epoch::broadcast`), so a retry can't build a second one.
/// Insufficient funds and build failures are deterministic given the same
/// state and are not retried.
#[allow(clippy::too_many_arguments)]
fn attempt_epoch_with_retries(
    rpc: &RpcClient,
    funding: &mut Funding,
    network: Network,
    keypair: &Keypair,
    evm_address: [u8; 20],
    signal_bits: u32,
    burn_zat: u64,
    target_height: u32,
    sova_ref: Option<SovaRef>,
    state: &mut MinerState,
) -> Result<EpochOutcome, EpochError> {
    let mut attempt = 0;
    loop {
        match attempt_epoch(
            rpc,
            funding,
            network,
            keypair,
            evm_address,
            signal_bits,
            burn_zat,
            target_height,
            sova_ref,
            state,
        ) {
            Ok(outcome) => return Ok(outcome),
            Err(e @ (EpochError::Rpc(_) | EpochError::Rejected { .. }))
                if attempt < MAX_RPC_RETRIES =>
            {
                attempt += 1;
                eprintln!(
                    "warning: epoch at height {target_height} failed ({e}); retry {attempt}/{MAX_RPC_RETRIES} in {RPC_RETRY_DELAY:?}"
                );
                thread::sleep(RPC_RETRY_DELAY);
            }
            Err(e) => return Err(e),
        }
    }
}
