//! The `mine` subcommand: poll a zebrad node for new blocks, and while
//! budget remains, send one SIP-1 burn of `per_epoch_zat` per new tip
//! observed, keeping up to [`MAX_IN_FLIGHT`] burns in flight (broadcast,
//! not yet mined), and follow each until it confirms or is dropped (see
//! `crate::epoch::resolve_pending`). A run that stops on `--max-epochs` or
//! its budget first waits for its burns in flight, so its last epoch is
//! confirmed on-chain by the time this function returns.
//!
//! Burning ahead (the cadence): a burn sent right after block `h` is seen
//! usually reaches the Zcash miners after they built `h+1`'s block
//! template, so it lands in `h+2`. Waiting for each burn to confirm before
//! sending the next (releases up to v0.1.8) therefore left a null Sova
//! block after most burns. Instead, every new tip gets a burn even while
//! the previous one is still in the mempool: that one is already in the
//! template being mined, and the new one waits in the mempool for the
//! next template. At steady state two are in flight, the newer spending
//! the older's unconfirmed change when no confirmed coin is free (see
//! `crate::epoch`), and each Zcash block carries one of them. Where
//! miners pick up mempool changes quickly, a burn is mined in the very
//! next block and only one is ever in flight. Either way it is at most one
//! burn per Zcash block observed: the cost per block is unchanged, only
//! the blocks no longer sat out are now paid for.
//!
//! State (including the UTXO pool and the burns in flight) is saved to the
//! sidecar before every broadcast (write-ahead: see
//! `crate::epoch::attempt_epoch`) and after every confirmation, not
//! batched until exit -- so `report` reflects true progress at any point,
//! and a process killed at any moment (Ctrl+C or otherwise) leaves every
//! burn it may have broadcast on disk: a restart picks the burns in flight
//! back up (re-sending any the node never got) instead of forgetting them
//! or spending their inputs again. What this can't cover: a lost or
//! restored-from-backup `state.json`, or two miners sharing one key.
//!
//! Budget (D5): `--budget-zat` is **per-invocation** -- each `mine` run
//! snapshots the keystore's lifetime spend at startup
//! (`MinerState::begin_invocation`) and measures its own budget against
//! only what happens from there, so a fresh run always gets its full
//! declared budget regardless of what prior runs already spent. An
//! optional `--lifetime-budget-zat` adds back a cumulative cap across
//! every invocation, for anyone who wants the old (pre-D5) behavior
//! explicitly. Burns in flight are held against both caps before another
//! is sent (`crate::epoch::attempt_epoch`). See the miner README's
//! "Budgets" section for the user-facing statement of both flags'
//! semantics.
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
    BudgetKind, EpochError, EpochOutcome, MAX_IN_FLIGHT, PendingResolution, attempt_epoch,
    resolve_pending,
};
use crate::evm_address::{CreditTarget, classify, legacy_warning};
use crate::funding::Funding;
use crate::node::Node;
use crate::state::{MinerState, StateError};
use crate::{CliError, keystore_path, parse_evm_address, rpc_client, state_path};

/// How many times to retry one epoch after a transient RPC failure before
/// giving up on the whole run. Node hiccups (a `sendrawtransaction` racing
/// a block being produced, a momentarily busy node) are the expected
/// failure mode here; a deterministic build/funding error is not retried at
/// all (see [`attempt_epoch_with_retries`]).
const MAX_RPC_RETRIES: u32 = 5;
/// Delay between retries of a transient RPC failure.
const RPC_RETRY_DELAY: Duration = if cfg!(test) {
    Duration::from_millis(1)
} else {
    Duration::from_millis(500)
};

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
    // Burns an earlier run left in flight (it was killed, or they were
    // still unmined when it stopped): settle them before this run's budget
    // snapshot, so a burn that confirmed in the meantime is charged to the
    // run that sent it. Ones still in flight stay reserved -- inputs and
    // cost -- and are followed on every new block like this run's own.
    let (resolutions, error) = resolve_pending(&rpc, &mut state);
    for resolution in &resolutions {
        report_resolution(resolution, "from an earlier run");
    }
    if let Some(e) = error {
        return Err(e.into());
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

    // `--max-epochs 0` with nothing in flight: nothing to do (and nothing
    // is sent). With burns in flight it waits for them below: the drain a
    // rollback needs (see docs/ops/keeper-miner.md).
    if args.max_epochs == Some(0) && state.pending.is_empty() {
        println!("--max-epochs 0 and no burns in flight -- stopping.");
        return Ok(());
    }

    let mut last_height = rpc.get_block_count()?;
    println!("baseline tip height: {last_height} (epochs trigger on new blocks past this)");

    let mut burner = Burner {
        network: args.network,
        keypair: &keypair,
        evm_address,
        per_epoch_zat: args.per_epoch_zat,
        max_epochs: args.max_epochs,
        epochs_this_run: 0,
    };

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
        last_height = height;

        // SIP-8: short of a Zcash reorg, the burn can't be mined below
        // `target_height` (the tip is already `height`), so that is the
        // height the activation guard checks. Freshness is judged against
        // the tip as it is when the burn is built.
        let sova_ref = |target_height: u32| {
            anchoring.as_mut().and_then(|anchoring| {
                let tip = rpc.get_block_count().unwrap_or(height);
                anchoring.reference_for(&rpc, u64::from(target_height), tip)
            })
        };
        let step = on_new_tip(
            &rpc,
            &mut funding,
            &mut burner,
            &mut state,
            height,
            &mut |s: &MinerState| s.save(&st_path),
            sova_ref,
        )?;
        if step == TipStep::Stop {
            return Ok(());
        }
    }
}

/// The fixed parameters of this run's burns, and its epoch count.
pub(crate) struct Burner<'a> {
    /// The network to build transactions for.
    pub network: burn_wallet::Network,
    /// The miner's key: funds, signs, and takes the change.
    pub keypair: &'a Keypair,
    /// The Sova address every burn credits.
    pub evm_address: [u8; 20],
    /// Zatoshis burned per epoch.
    pub per_epoch_zat: u64,
    /// `--max-epochs`, if given.
    pub max_epochs: Option<u64>,
    /// Burns confirmed during this run (including ones an earlier run left
    /// in flight that confirm during this one).
    pub epochs_this_run: u64,
}

/// Whether `mine` carries on after a tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TipStep {
    /// Keep watching for blocks.
    Continue,
    /// Done: `--max-epochs` reached or the budget exhausted, with nothing
    /// left in flight.
    Stop,
}

/// Everything `mine` does on a new Zcash tip `height` (once the chain
/// identity checked out): settle the burns in flight, then -- unless
/// [`MAX_IN_FLIGHT`] are already in flight, or `--max-epochs` is covered
/// by what confirmed plus what is in flight -- send one new burn aimed at
/// `height + 1`. `save` persists state after every change; `sova_ref`
/// gives the SIP-8 reference for a burn's target height (called only when
/// a burn is built).
///
/// Running out of funds, a node rejection, or the budget while burns are
/// in flight is not an error: those burns hold the funds and budget, so
/// the send is retried on the next tip once they settle. With nothing in
/// flight, a funding or node failure is an error (as it always was) and
/// budget exhaustion stops the run.
///
/// # Errors
///
/// A state save failure, or an epoch failure with nothing in flight.
pub(crate) fn on_new_tip(
    node: &impl Node,
    funding: &mut Funding,
    burner: &mut Burner<'_>,
    state: &mut MinerState,
    height: u64,
    save: &mut dyn FnMut(&MinerState) -> Result<(), StateError>,
    sova_ref: impl FnOnce(u32) -> Option<SovaRef>,
) -> Result<TipStep, CliError> {
    if !state.pending.is_empty() {
        let (resolutions, error) = resolve_pending(node, state);
        if !resolutions.is_empty() {
            save(state)?;
        }
        for resolution in &resolutions {
            report_resolution(resolution, "");
            match resolution {
                PendingResolution::Confirmed(_) => burner.epochs_this_run += 1,
                PendingResolution::Reorged { .. } => {
                    burner.epochs_this_run = burner.epochs_this_run.saturating_sub(1);
                }
                _ => {}
            }
        }
        if let Some(e) = error {
            // Not knowing where the burns in flight stand, don't add one.
            eprintln!("warning: checking the burns in flight failed: {e}; will retry");
            return Ok(TipStep::Continue);
        }
    }

    let in_flight = state.pending.len();
    if let Some(max) = burner.max_epochs {
        if burner.epochs_this_run >= max && in_flight == 0 {
            println!("reached --max-epochs {max} -- stopping.");
            return Ok(TipStep::Stop);
        }
        if burner.epochs_this_run.saturating_add(in_flight as u64) >= max {
            println!(
                "tip {height}: {in_flight} burn(s) in flight cover --max-epochs {max}; waiting for them"
            );
            return Ok(TipStep::Continue);
        }
    }
    if in_flight >= MAX_IN_FLIGHT {
        println!(
            "tip {height}: {in_flight} burns already in flight ({}); not adding another",
            in_flight_txids(state)
        );
        return Ok(TipStep::Continue);
    }

    // A target, not a promise: the burn can't be mined before the next
    // block, and is often mined in the one after (see the module docs).
    // The real confirming height is read back once it happens (see
    // `resolve_pending`).
    let target_height = u32::try_from(height + 1).unwrap_or(u32::MAX);
    let sova_ref = sova_ref(target_height);
    // The burn is saved as in flight before it is broadcast (write-ahead,
    // see `attempt_epoch`): a kill at any point after the broadcast
    // neither loses the epoch nor lets a restart spend its inputs again.
    let outcome = attempt_epoch_with_retries(
        node,
        funding,
        burner.network,
        burner.keypair,
        burner.evm_address,
        0,
        burner.per_epoch_zat,
        target_height,
        sova_ref,
        state,
        save,
    );
    match outcome {
        Ok(EpochOutcome::Submitted(pending)) => {
            let vote = sova_ref.map_or_else(String::new, |r| {
                format!(" payload=v2 ref={}:0x{}", r.height, hex::encode(r.hash))
            });
            let chained = state
                .pending
                .iter()
                .find(|p| {
                    p.change_outpoint()
                        .is_some_and(|c| pending.spent.contains(&c))
                })
                .map_or_else(String::new, |p| {
                    format!(" spends-unconfirmed-change-of={}", p.txid)
                });
            println!(
                "burn broadcast at tip {height}: txid={} burn={}zat fee={}zat change={}zat{vote}{chained} in-flight={}",
                pending.txid,
                pending.burn_zat,
                pending.fee_zat,
                pending.change_zat,
                state.pending.len()
            );
            Ok(TipStep::Continue)
        }
        Ok(EpochOutcome::BudgetExhausted {
            kind,
            needed_zat,
            remaining_zat,
            in_flight_zat,
        }) => {
            let cap_flag = match kind {
                BudgetKind::PerInvocation => "--budget-zat (this run)",
                BudgetKind::Lifetime => "--lifetime-budget-zat (across all runs)",
            };
            if in_flight == 0 {
                println!(
                    "budget exhausted at height {target_height}: next epoch needs {needed_zat} zat, only {remaining_zat} zat remain under {cap_flag} -- stopping."
                );
                Ok(TipStep::Stop)
            } else {
                println!(
                    "budget: next epoch needs {needed_zat} zat, {remaining_zat} zat remain under {cap_flag} after the {in_flight_zat} zat held for {in_flight} burn(s) in flight; waiting for them"
                );
                Ok(TipStep::Continue)
            }
        }
        Err(e) if in_flight > 0 => {
            eprintln!(
                "warning: no new burn at tip {height} with {in_flight} in flight ({e}); trying again on the next block"
            );
            Ok(TipStep::Continue)
        }
        Err(e) => Err(e.into()),
    }
}

/// The txids of the burns in flight, comma-separated, for the log.
fn in_flight_txids(state: &MinerState) -> String {
    state
        .pending
        .iter()
        .map(|p| p.txid.as_str())
        .collect::<Vec<_>>()
        .join(", ")
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
            "burn {txid} still in flight{context}; its inputs stay reserved and it is checked again on each new block"
        ),
        PendingResolution::Reorged { txid, height } => println!(
            "burn {txid} (recorded at height {height}) orphaned by a zcash reorg{context}: epoch taken back, burn in flight again"
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
    rpc: &impl Node,
    funding: &mut Funding,
    network: Network,
    keypair: &Keypair,
    evm_address: [u8; 20],
    signal_bits: u32,
    burn_zat: u64,
    target_height: u32,
    sova_ref: Option<SovaRef>,
    state: &mut MinerState,
    persist: &mut dyn FnMut(&MinerState) -> Result<(), StateError>,
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
            persist,
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

#[cfg(test)]
// Test code: an unexpected `Err`/`None` here is a test failure.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::funding::tests::{FakeNode, tx_io, txid};
    use crate::node::TxStatus;

    /// How the simulated Zcash miners build block templates.
    #[derive(Clone, Copy)]
    enum Templates {
        /// Built when the previous block arrives, from the mempool as it
        /// was then, and never refreshed: a burn sent in reaction to block
        /// `h` misses `h+1` (the testnet diagnosis, in its pure form).
        Lagged,
        /// Built at the moment the block is found, from the whole mempool
        /// (regtest's `generate`).
        Instant,
    }

    struct Sim {
        node: FakeNode,
        state: MinerState,
        funding: Funding,
        keypair: Keypair,
        /// `pending.len()` right after each tip's step.
        in_flight: Vec<usize>,
    }

    impl Sim {
        /// Tip 200, one confirmed 10,000,000 zat coin.
        fn new(budget_zat: u64) -> Self {
            let node = FakeNode::default();
            node.tip.set(200);
            node.fund(&txid(7), 0, 10_000_000, 150);
            let mut state = MinerState::new("tmAddr".to_string(), "ab".repeat(20));
            state.begin_invocation(budget_zat, 100_000, None);
            Self {
                node,
                state,
                funding: Funding::new(burn_wallet::Network::Regtest),
                keypair: Keypair::generate(),
                in_flight: Vec::new(),
            }
        }

        /// One Zcash block: the miner reacts to the current tip, then the
        /// next block is found with `templates`' view of the mempool.
        /// Returns what the miner's step said.
        fn block(&mut self, templates: Templates, max_epochs: Option<u64>) -> TipStep {
            // Lagged: the template for the next block was built as the
            // current one arrived -- before the miner could react.
            let template = self.node.mempool();
            let keypair = self.keypair;
            let mut burner = Burner {
                network: burn_wallet::Network::Regtest,
                keypair: &keypair,
                evm_address: [0u8; 20],
                per_epoch_zat: 100_000,
                max_epochs,
                epochs_this_run: 0,
            };
            let step = self.step(&mut burner);
            self.in_flight.push(self.state.pending.len());
            match templates {
                Templates::Lagged => self.node.mine(&template),
                Templates::Instant => self.node.mine(&self.node.mempool()),
            };
            step
        }

        fn step(&mut self, burner: &mut Burner<'_>) -> TipStep {
            let tip = self.node.tip.get();
            on_new_tip(
                &self.node,
                &mut self.funding,
                burner,
                &mut self.state,
                tip,
                &mut |_: &MinerState| Ok(()),
                |_| None,
            )
            .unwrap()
        }

        /// Our burns per block height.
        fn burns_per_block(&self) -> BTreeMap<u64, usize> {
            let mut out = BTreeMap::new();
            for (id, _) in self.node.admitted.borrow().iter() {
                if let Some(TxStatus::Confirmed { height }) = self.node.txs.borrow().get(id) {
                    *out.entry(*height).or_insert(0) += 1;
                }
            }
            out
        }

        /// Every outpoint any of our txs spends, once: a repeat would be
        /// a double-spend.
        fn assert_no_double_spend(&self) {
            let mut seen = BTreeSet::new();
            for (id, raw) in self.node.admitted.borrow().iter() {
                for input in tx_io(raw).0 {
                    assert!(seen.insert(input.clone()), "{input:?} spent twice ({id})");
                }
            }
        }
    }

    /// The testnet case: templates are built before the miner can react
    /// to a block. Burning only after the last burn confirmed filled at
    /// most every other block; with one burn waiting in the mempool, every
    /// block from the second on carries exactly one burn, two are in
    /// flight at steady state, and the waiting one chains onto the
    /// unconfirmed change of the one being mined.
    #[test]
    fn lagged_templates_get_a_burn_into_every_block() {
        let mut sim = Sim::new(100_000_000);
        for _ in 0..12 {
            assert_eq!(sim.block(Templates::Lagged, None), TipStep::Continue);
        }
        let per_block = sim.burns_per_block();
        // Block 201's template predates the first burn; 202..=212 each
        // carry one.
        assert_eq!(
            per_block.keys().copied().collect::<Vec<_>>(),
            (202..=212).collect::<Vec<_>>()
        );
        assert!(per_block.values().all(|&n| n == 1), "{per_block:?}");
        assert_eq!(sim.in_flight[0], 1);
        assert!(
            sim.in_flight[1..].iter().all(|&n| n == MAX_IN_FLIGHT),
            "{:?}",
            sim.in_flight
        );
        // Epochs are recorded at their real, consecutive heights.
        let heights: Vec<u64> = sim.state.epochs.iter().map(|e| e.height).collect();
        assert_eq!(heights, (202..=211).collect::<Vec<_>>());
        // Every burn after the first spent the unconfirmed change of the
        // one before it (still in the mempool when it was sent).
        let admitted = sim.node.admitted.borrow();
        for pair in admitted.windows(2) {
            assert_eq!(tx_io(&pair[1].1).0, vec![(pair[0].0.clone(), 2)]);
        }
        drop(admitted);
        sim.assert_no_double_spend();
    }

    /// Regtest's `generate` (instant templates): each burn is mined in the
    /// very next block, so only one is ever in flight, no change is
    /// chained, and there are no doubles either.
    #[test]
    fn instant_templates_keep_one_burn_in_flight_and_fill_every_block() {
        let mut sim = Sim::new(100_000_000);
        for _ in 0..8 {
            sim.block(Templates::Instant, None);
        }
        let per_block = sim.burns_per_block();
        assert_eq!(
            per_block.keys().copied().collect::<Vec<_>>(),
            (201..=208).collect::<Vec<_>>()
        );
        assert!(per_block.values().all(|&n| n == 1), "{per_block:?}");
        assert!(sim.in_flight.iter().all(|&n| n == 1), "{:?}", sim.in_flight);
        for (_, raw) in sim.node.admitted.borrow().iter() {
            let (inputs, _) = tx_io(raw);
            let parent = &inputs[0].0;
            assert!(
                !matches!(sim.node.txs.borrow().get(parent), Some(TxStatus::Mempool)),
                "spent confirmed coins only"
            );
        }
        sim.assert_no_double_spend();
    }

    /// `--max-epochs`: exactly that many burns are sent even with burns
    /// sent ahead, and the run stops only once all of them are mined.
    #[test]
    fn max_epochs_sends_exactly_that_many_and_stops_once_they_are_mined() {
        let mut sim = Sim::new(100_000_000);
        let mut burner_epochs = 0;
        let mut steps = Vec::new();
        for _ in 0..8 {
            let template = sim.node.mempool();
            let keypair = sim.keypair;
            let mut burner = Burner {
                network: burn_wallet::Network::Regtest,
                keypair: &keypair,
                evm_address: [0u8; 20],
                per_epoch_zat: 100_000,
                max_epochs: Some(3),
                epochs_this_run: burner_epochs,
            };
            let step = sim.step(&mut burner);
            burner_epochs = burner.epochs_this_run;
            steps.push(step);
            if step == TipStep::Stop {
                break;
            }
            sim.node.mine(&template);
        }
        assert_eq!(sim.node.admitted.borrow().len(), 3, "three burns, no more");
        assert_eq!(steps.last(), Some(&TipStep::Stop));
        assert!(sim.state.pending.is_empty());
        assert_eq!(sim.state.epochs.len(), 3);
        assert_eq!(burner_epochs, 3);
    }

    /// Budget: with burns sent ahead, the run still never commits more
    /// than `--budget-zat` -- the burn in flight is held against it -- and
    /// stops only once nothing is left in flight.
    #[test]
    fn budget_is_never_exceeded_counting_burns_in_flight() {
        // Room for three 120,000 zat burns and a bit.
        let mut sim = Sim::new(400_000);
        let mut stopped = false;
        for _ in 0..10 {
            if sim.block(Templates::Lagged, None) == TipStep::Stop {
                stopped = true;
                break;
            }
            let spent = sim.state.total_spent_zat() - sim.state.invocation_start_spent_zat;
            assert!(spent + sim.state.in_flight_cost_zat() <= 400_000);
        }
        assert!(stopped);
        assert_eq!(sim.node.admitted.borrow().len(), 3);
        assert!(sim.state.pending.is_empty());
        assert_eq!(sim.state.total_spent_zat(), 360_000);
    }

    /// Killed with two burns in flight and restarted from state.json (a
    /// fresh process: new funding cache, new epoch count): nothing is
    /// double-spent, no epoch is lost, and blocks keep getting one burn
    /// each.
    #[test]
    fn restart_with_two_in_flight_neither_double_spends_nor_loses_an_epoch() {
        let mut sim = Sim::new(100_000_000);
        for _ in 0..5 {
            sim.block(Templates::Lagged, None);
        }
        assert_eq!(sim.state.pending.len(), 2);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        sim.state.save(&path).unwrap();
        // The miner is down for a block, which mines the burn that was in
        // its template.
        sim.node.mine(&sim.node.mempool()[..1]);
        sim.state = MinerState::load(&path).unwrap();
        sim.funding = Funding::new(burn_wallet::Network::Regtest);
        let (resolutions, error) = resolve_pending(&sim.node, &mut sim.state);
        assert!(error.is_none());
        assert!(matches!(resolutions[0], PendingResolution::Confirmed(_)));
        sim.state.begin_invocation(100_000_000, 100_000, None);
        for _ in 0..5 {
            sim.block(Templates::Lagged, None);
        }
        sim.assert_no_double_spend();
        // Settle the last block's burn too.
        assert!(resolve_pending(&sim.node, &mut sim.state).1.is_none());
        let mined: usize = sim.burns_per_block().values().sum();
        assert_eq!(
            sim.state.epochs.len(),
            mined,
            "every mined burn is recorded once"
        );
        // One burn per block throughout, except 207: its burn would have
        // been sent at tip 206, while the miner was down.
        let heights: Vec<u64> = sim.state.epochs.iter().map(|e| e.height).collect();
        assert_eq!(heights, [202, 203, 204, 205, 206, 208, 209, 210, 211]);
    }

    /// Review finding 1: the process is killed right after a broadcast,
    /// before anything else is saved. What is on disk at that moment must
    /// already hold the burn, or a restart forgets a burn that is mined.
    #[test]
    fn a_burn_is_on_disk_before_the_node_sees_it() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut sim = Sim::new(100_000_000);
        let disk: Rc<RefCell<String>> = Rc::default();
        let at_send: Rc<RefCell<Vec<String>>> = Rc::default();
        let (d, a) = (disk.clone(), at_send.clone());
        *sim.node.on_send.borrow_mut() =
            Some(Box::new(move || a.borrow_mut().push(d.borrow().clone())));
        let keypair = sim.keypair;
        let mut burner = Burner {
            network: burn_wallet::Network::Regtest,
            keypair: &keypair,
            evm_address: [0u8; 20],
            per_epoch_zat: 100_000,
            max_epochs: None,
            epochs_this_run: 0,
        };
        let tip = sim.node.tip.get();
        on_new_tip(
            &sim.node,
            &mut sim.funding,
            &mut burner,
            &mut sim.state,
            tip,
            &mut |s: &MinerState| {
                *disk.borrow_mut() = serde_json::to_string(s).unwrap();
                Ok(())
            },
            |_| None,
        )
        .unwrap();
        let sent = sim.state.pending[0].clone();
        let on_disk: MinerState = serde_json::from_str(&at_send.borrow()[0]).unwrap();
        assert_eq!(on_disk.pending, vec![sent]);
    }

    /// Review finding 1, the probe: killed after every broadcast and
    /// restarted from what was on disk then, with a 360,000 zat lifetime
    /// cap (three burns). Nothing sent may go unrecorded, so no more than
    /// three burns are ever mined.
    #[test]
    fn killed_after_every_broadcast_never_overruns_the_lifetime_budget() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut sim = Sim::new(100_000_000);
        sim.state
            .begin_invocation(100_000_000, 100_000, Some(360_000));
        let disk: Rc<RefCell<String>> =
            Rc::new(RefCell::new(serde_json::to_string(&sim.state).unwrap()));
        let at_send: Rc<RefCell<Option<String>>> = Rc::default();
        let (d, a) = (disk.clone(), at_send.clone());
        *sim.node.on_send.borrow_mut() =
            Some(Box::new(move || *a.borrow_mut() = Some(d.borrow().clone())));
        for _ in 0..10 {
            let keypair = sim.keypair;
            let mut burner = Burner {
                network: burn_wallet::Network::Regtest,
                keypair: &keypair,
                evm_address: [0u8; 20],
                per_epoch_zat: 100_000,
                max_epochs: None,
                epochs_this_run: 0,
            };
            let tip = sim.node.tip.get();
            let step = on_new_tip(
                &sim.node,
                &mut sim.funding,
                &mut burner,
                &mut sim.state,
                tip,
                &mut |s: &MinerState| {
                    *disk.borrow_mut() = serde_json::to_string(s).unwrap();
                    Ok(())
                },
                |_| None,
            );
            if let Some(snapshot) = at_send.borrow_mut().take() {
                // Killed right after the broadcast; restarted from disk.
                sim.state = serde_json::from_str(&snapshot).unwrap();
                sim.funding = Funding::new(burn_wallet::Network::Regtest);
                let _ = resolve_pending(&sim.node, &mut sim.state);
                sim.state
                    .begin_invocation(100_000_000, 100_000, Some(360_000));
                *disk.borrow_mut() = serde_json::to_string(&sim.state).unwrap();
            } else if matches!(step, Ok(TipStep::Stop)) {
                break;
            }
            sim.node.mine(&sim.node.mempool());
        }
        let mined: usize = sim.burns_per_block().values().sum();
        assert!(
            mined <= 3,
            "{mined} burns mined under a three-burn lifetime cap"
        );
        let _ = resolve_pending(&sim.node, &mut sim.state);
        assert_eq!(
            sim.state.epochs.len(),
            mined,
            "every mined burn is recorded"
        );
        assert!(sim.state.total_spent_zat() <= 360_000);
    }

    /// Review finding 2: a Zcash reorg orphans a burn that was already
    /// recorded, while its child (spending its change) is in flight.
    /// zebrad clears its mempool and doesn't take the orphaned burn back,
    /// so the child can't be re-sent until its parent is. Burning must
    /// resume within a few blocks -- not stall until the child's expiry --
    /// and no epoch may stay recorded for a burn the chain no longer has.
    #[test]
    fn a_reorg_orphaning_a_recorded_parent_does_not_stall_burning() {
        let mut sim = Sim::new(100_000_000);
        for _ in 0..6 {
            sim.block(Templates::Lagged, None);
        }
        // One more tip's step: the burn mined in 206 is recorded, its
        // child is in flight, and a new one chains onto the child.
        let keypair = sim.keypair;
        let mut burner = Burner {
            network: burn_wallet::Network::Regtest,
            keypair: &keypair,
            evm_address: [0u8; 20],
            per_epoch_zat: 100_000,
            max_epochs: None,
            epochs_this_run: 0,
        };
        sim.step(&mut burner);
        let orphaned = sim.state.epochs.last().unwrap().clone();
        assert_eq!(orphaned.height, 206);
        assert_eq!(sim.state.pending.len(), 2);
        // Block 206 is replaced by an empty one.
        sim.node.reorg(206);
        sim.node.mine(&[]);
        for _ in 0..6 {
            sim.block(Templates::Lagged, None);
        }
        // 206..=212 on the new branch: the orphaned burn and the two in
        // flight are re-sent (mined together), then one burn a block.
        let per_block = sim.burns_per_block();
        let late: Vec<u64> = (210..=212).filter(|h| !per_block.contains_key(h)).collect();
        assert!(late.is_empty(), "no burn in {late:?}: {per_block:?}");
        assert!(matches!(
            sim.node.txs.borrow().get(&orphaned.txid),
            Some(TxStatus::Confirmed { height }) if *height > 206
        ));
        sim.assert_no_double_spend();
        assert!(resolve_pending(&sim.node, &mut sim.state).1.is_none());
        let on_chain: BTreeSet<String> = sim
            .node
            .txs
            .borrow()
            .iter()
            .filter(|(_, st)| matches!(st, TxStatus::Confirmed { .. }))
            .map(|(id, _)| id.clone())
            .collect();
        for e in &sim.state.epochs {
            assert!(
                on_chain.contains(&e.txid),
                "epoch {} ({}) is not on chain",
                e.epoch,
                e.txid
            );
        }
        let ids: BTreeSet<&str> = sim.state.epochs.iter().map(|e| e.txid.as_str()).collect();
        assert_eq!(ids.len(), sim.state.epochs.len(), "no burn recorded twice");
    }

    /// Review finding 3, rollback: `mine --max-epochs 0` sends nothing,
    /// waits for the burns in flight to be mined, and stops -- leaving a
    /// `state.json` that v0.1.8 (which reads `pending` as one optional
    /// burn) loads. With two in flight, that release refuses the file.
    #[test]
    fn max_epochs_zero_drains_burns_in_flight_and_leaves_the_old_state_shape() {
        /// The fields v0.1.8's `MinerState` requires, as it declares them.
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct V018State {
            version: u8,
            address: String,
            evm_address_hex: String,
            budget_zat: u64,
            per_epoch_zat: u64,
            total_burned_zat: u64,
            total_fee_zat: u64,
            epochs: Vec<crate::state::EpochRecord>,
            utxos: Vec<crate::state::TrackedUtxo>,
            #[serde(default)]
            pending: Option<crate::state::PendingBurn>,
        }
        let mut sim = Sim::new(100_000_000);
        for _ in 0..5 {
            sim.block(Templates::Lagged, None);
        }
        assert_eq!(sim.state.pending.len(), 2);
        let two = serde_json::to_string(&sim.state).unwrap();
        assert!(
            serde_json::from_str::<V018State>(&two).is_err(),
            "v0.1.8 refuses two in flight"
        );

        let sent_before = sim.node.admitted.borrow().len();
        let mut stopped = false;
        for _ in 0..4 {
            if sim.block(Templates::Lagged, Some(0)) == TipStep::Stop {
                stopped = true;
                break;
            }
        }
        assert!(stopped, "drained and stopped");
        assert_eq!(
            sim.node.admitted.borrow().len(),
            sent_before,
            "nothing new sent"
        );
        assert!(sim.state.pending.is_empty());
        let drained = serde_json::to_string(&sim.state).unwrap();
        let old: V018State = serde_json::from_str(&drained).unwrap();
        assert_eq!(old.pending, None);
        assert_eq!(old.epochs.len(), sent_before, "every burn recorded");
    }

    /// A burn waiting in the mempool is evicted (a node restart): it is
    /// re-sent on the next tip, the chain of burns carries on, and nothing
    /// is double-spent.
    #[test]
    fn an_evicted_burn_in_flight_is_resent_and_burning_carries_on() {
        let mut sim = Sim::new(100_000_000);
        for _ in 0..4 {
            sim.block(Templates::Lagged, None);
        }
        let waiting = sim.state.pending.last().unwrap().txid.clone();
        sim.node.forget(&waiting);
        for _ in 0..4 {
            sim.block(Templates::Lagged, None);
        }
        assert!(matches!(
            sim.node.txs.borrow().get(&waiting),
            Some(TxStatus::Confirmed { .. })
        ));
        sim.assert_no_double_spend();
        assert!(sim.burns_per_block().values().all(|&n| n <= 2));
    }
}
