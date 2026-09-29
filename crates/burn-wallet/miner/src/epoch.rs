//! One mining epoch: select UTXOs, build and sign a SIP-1 burn transaction
//! for exactly `per_epoch_zat`, and broadcast it -- or report that the
//! declared budget won't stretch to cover this epoch. Then
//! [`resolve_pending`] follows the broadcast burns until each is mined
//! (recording its epoch) or can no longer be.
//!
//! Funding comes from `getaddressutxos` (see `crate::funding`): every
//! attempt first re-reads the address's confirmed UTXOs (one RPC, cheap at
//! any chain height), drops tracked entries the node no longer lists, and,
//! only when the tracked pool can't cover the burn, merges in newly found
//! spendable outputs -- ordinary transfers, and mature coinbase on regtest
//! (on testnet/mainnet coinbase must be shielded first, see
//! [`EpochError::CoinbaseMustBeShielded`]).
//!
//! UTXO chaining: [`crate::state::MinerState::utxos`] tracks both funding
//! and the miner's own change. A burn's inputs leave the pool when it is
//! broadcast (they are reserved by its [`crate::state::PendingBurn`] until
//! it is mined or expires), and its change joins the pool once it confirms
//! -- so consecutive epochs spend the previous burn's change without
//! waiting on coinbase maturity (100 confirmations) the way a *fresh*
//! coinbase output would.
//!
//! Burning ahead: up to [`MAX_IN_FLIGHT`] burns are in flight at once, so a
//! burn can already be waiting in the mempool when the next Zcash block's
//! template is built (see `crate::mine`). When no confirmed coin can fund
//! the next burn, it spends the *unconfirmed* change of a burn still in
//! flight -- a transaction spending another mempool transaction's output,
//! which zebrad's mempool accepts (it resolves such inputs from the mempool
//! and records the dependency; zebra PR 8857, in every release since 2.1)
//! and whose block template includes the child only together with or after
//! its parent. A confirmed coin is always preferred, so a wallet holding
//! several coins burns from independent ones. If the parent is dropped, so
//! is the child ([`resolve_pending`]).

use std::collections::BTreeSet;
use std::thread;
use std::time::Duration;

use burn_wallet::branch;
use burn_wallet::tx::{BurnTxRequest, build_burn_transaction};
use burn_wallet::utxo::{decode_rpc_hash, encode_rpc_hash};
use burn_wallet::{Keypair, Network, RpcError, Utxo};
use zcash_transparent::bundle::OutPoint;

use consensus::sip1::SovaRef;

use crate::fee::{BurnPayloadVersion, DUST_THRESHOLD_ZAT, zip317_fee_zat};
use crate::funding::{self, Funding};
use crate::node::{Node, TxStatus};
use crate::state::{
    EpochRecord, MinerState, OutPointRef, PendingBurn, RecentBurn, StateError, TrackedUtxo,
};

/// At most this many of our burns are in flight (broadcast, not yet
/// mined) at once: one in the block template being mined now and one
/// waiting in the mempool for the next (see `crate::mine`). Also bounds
/// how deep a chain of unconfirmed change gets.
pub(crate) const MAX_IN_FLIGHT: usize = 2;
/// How many blocks a confirmed burn stays watched for a reorg that
/// orphans it (see [`crate::state::MinerState::recent`]). Well past any
/// reorg seen on Zcash testnet or in the reorg stress sims.
pub(crate) const REORG_WATCH_DEPTH: u64 = 10;
/// How many times [`attempt_epoch`] sends the same signed burn before
/// giving up on it (see [`broadcast`]).
const BROADCAST_ATTEMPTS: u32 = 3;
/// Pause between those sends.
const BROADCAST_RETRY_DELAY: Duration = if cfg!(test) {
    Duration::from_millis(1)
} else {
    Duration::from_millis(500)
};
/// After a `sendrawtransaction` error, how many times to ask the node
/// whether it admitted the transaction anyway (see [`node_has_tx`]).
const ADMISSION_CHECKS: u32 = 3;
/// Pause between those checks.
const ADMISSION_CHECK_DELAY: Duration = if cfg!(test) {
    Duration::from_millis(1)
} else {
    Duration::from_millis(300)
};

/// Errors attempting one mining epoch.
#[derive(Debug, thiserror::Error)]
pub(crate) enum EpochError {
    /// The wallet's spendable UTXOs don't sum to enough to cover
    /// `burn_zat` plus any possible fee, regardless of budget.
    #[error(
        "insufficient wallet funds: no combination of spendable UTXOs covers a {burn_zat} zat burn plus fee ({spendable_zat} zat spendable, {immature_zat} zat of coinbase still maturing)"
    )]
    InsufficientFunds {
        /// The burn amount that could not be funded.
        burn_zat: u64,
        /// Total spendable value found.
        spendable_zat: u64,
        /// Coinbase value still short of 100 confirmations.
        immature_zat: u64,
    },
    /// As [`EpochError::InsufficientFunds`], on a network where the
    /// address's coinbase can't fund a burn until shielded (testnet,
    /// mainnet), and some is waiting there.
    #[error(
        "insufficient wallet funds: no combination of spendable UTXOs covers a {burn_zat} zat burn plus fee ({spendable_zat} zat spendable, coinbase excluded). {}",
        burn_wallet::utxo::coinbase_must_be_shielded_message(*coinbase_zat, "burn")
    )]
    CoinbaseMustBeShielded {
        /// The burn amount that could not be funded.
        burn_zat: u64,
        /// Total spendable (non-coinbase) value found.
        spendable_zat: u64,
        /// Transparent coinbase at the address, which must be shielded
        /// first.
        coinbase_zat: u64,
    },
    /// A txid from the node or state was not valid 32-byte hex.
    #[error(transparent)]
    Utxo(#[from] burn_wallet::utxo::UtxoError),
    /// An RPC call failed.
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// Building or signing the transaction failed.
    #[error(transparent)]
    Build(#[from] burn_wallet::BurnTxError),
    /// Saving state before (or after a refused) broadcast failed. When
    /// this is returned before the broadcast, nothing was sent.
    #[error(transparent)]
    State(#[from] crate::state::StateError),
    /// The node refused the burn: every send was answered with an error,
    /// and the node doesn't have the transaction. Nothing was spent.
    #[error("burn {txid} rejected by the node: {message}")]
    Rejected {
        /// The refused burn's txid.
        txid: String,
        /// The node's last error message, verbatim.
        message: String,
    },
}

/// Which budget cap a [`EpochOutcome::BudgetExhausted`] outcome ran out
/// under (D5: `--budget-zat` and `--lifetime-budget-zat` are two
/// independent caps -- see `crate::state::MinerState::begin_invocation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetKind {
    /// `--budget-zat`: this run's own per-invocation cap, measured against
    /// spend since this `mine` invocation started -- the default
    /// semantics as of D5.
    PerInvocation,
    /// `--lifetime-budget-zat`: the optional cumulative cap across every
    /// `mine` invocation ever run against this keystore -- the pre-D5
    /// lifetime semantics, opt-in.
    Lifetime,
}

/// The result of attempting one epoch.
#[derive(Debug)]
pub(crate) enum EpochOutcome {
    /// A burn was built, signed, saved as in flight (the last entry of
    /// `state.pending`, persisted before it was broadcast) and accepted
    /// by the node -- or its broadcast outcome is unknown, which is
    /// tracked the same way. The caller follows it with
    /// [`resolve_pending`] on later blocks.
    Submitted(PendingBurn),
    /// The wallet has funds, but spending them for this epoch would exceed
    /// a declared budget.
    BudgetExhausted {
        /// Which cap ([`BudgetKind::PerInvocation`]'s `--budget-zat` or
        /// [`BudgetKind::Lifetime`]'s `--lifetime-budget-zat`) ran out.
        kind: BudgetKind,
        /// Total zatoshis (burn + fee) this epoch would have cost.
        needed_zat: u64,
        /// Budget remaining, under `kind`'s cap, before this epoch, after
        /// holding back what the burns in flight will cost.
        remaining_zat: u64,
        /// What the burns in flight will cost (held back from
        /// `remaining_zat`); 0 with nothing in flight.
        in_flight_zat: u64,
    },
}

/// What became of one in-flight burn.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PendingResolution {
    /// It was mined: the epoch is recorded (already pushed onto
    /// `state.epochs`, totals updated) and its change tracked.
    Confirmed(EpochRecord),
    /// Still in flight (in the mempool, or not yet seen): inputs stay
    /// reserved.
    InFlight {
        /// Its txid.
        txid: String,
    },
    /// A Zcash reorg orphaned it after its epoch was recorded: the epoch
    /// is taken back and the burn is in flight again (re-sent, before any
    /// child spending its change).
    Reorged {
        /// Its txid.
        txid: String,
        /// The height it had been recorded at.
        height: u64,
    },
    /// It can no longer be mined (the tip reached its expiry height
    /// without it, or it spends the change of a burn that was dropped):
    /// dropped, nothing recorded, inputs free again.
    Dropped {
        /// Its txid.
        txid: String,
        /// Why, human-readable.
        reason: String,
    },
}

/// A chosen set of inputs for one burn transaction, and the fee/change that
/// choice implies.
struct Selection {
    /// Indices into the UTXO pool slice this selection was computed from.
    indices: Vec<usize>,
    /// The real fee this transaction will pay (see [`crate::fee`] for why
    /// this can exceed the raw ZIP-317 formula value: sub-dust change gets
    /// folded in rather than becoming its own output).
    fee_zat: u64,
    /// Change returned to our own address; 0 if [`Self::has_change`] is
    /// `false`.
    change_zat: u64,
    /// Whether this transaction will have a change output (always at
    /// output index 2, after the SIP-1 payload at 0 and the eater output
    /// at 1 -- see `burn_wallet::tx::build_burn_transaction`).
    has_change: bool,
}

/// Greedily selects UTXOs (largest-first, to minimize input count and
/// therefore fee) from `pool` to cover `burn_zat` plus its ZIP-317 fee for
/// a burn carrying `payload`, recomputing the fee as inputs are added since
/// fee depends on input count. Returns `None` if no prefix of `pool` (sorted descending) covers
/// it -- i.e. the wallet doesn't have enough total spendable value.
fn select_utxos(
    pool: &[TrackedUtxo],
    burn_zat: u64,
    payload: BurnPayloadVersion,
) -> Option<Selection> {
    let mut order: Vec<usize> = (0..pool.len()).collect();
    order.sort_unstable_by(|&a, &b| pool[b].value_zat.cmp(&pool[a].value_zat));

    let mut chosen = Vec::new();
    let mut sum: u64 = 0;
    for idx in order {
        chosen.push(idx);
        sum = sum.saturating_add(pool[idx].value_zat);
        let n_in = u64::try_from(chosen.len()).unwrap_or(u64::MAX);

        // First, try the shape with a change output (payload + eater +
        // change).
        let fee_with_change = zip317_fee_zat(n_in, payload, true);
        let Some(needed_with_change) = burn_zat.checked_add(fee_with_change) else {
            continue;
        };
        if let Some(remaining) = sum.checked_sub(needed_with_change) {
            if remaining >= DUST_THRESHOLD_ZAT {
                return Some(Selection {
                    indices: chosen,
                    fee_zat: fee_with_change,
                    change_zat: remaining,
                    has_change: true,
                });
            }
            // The change output would be sub-dust (or exactly zero): drop
            // it (payload + eater only, no change) and fold whatever's
            // left into the fee. fee_no_change <= fee_with_change, and we
            // already know sum >= burn_zat + fee_with_change, so sum
            // covers burn_zat + fee_no_change too.
            let fee_no_change = zip317_fee_zat(n_in, payload, false);
            if let Some(needed_no_change) = burn_zat.checked_add(fee_no_change)
                && sum >= needed_no_change
            {
                let folded_dust = sum - needed_no_change;
                return Some(Selection {
                    indices: chosen,
                    fee_zat: fee_no_change.saturating_add(folded_dust),
                    change_zat: 0,
                    has_change: false,
                });
            }
        }
        // Not enough yet with this many inputs -- add another UTXO and
        // retry.
    }
    None
}

/// The outpoints reserved by the burns in flight.
pub(crate) fn reserved(state: &MinerState) -> BTreeSet<OutPointRef> {
    state
        .pending
        .iter()
        .flat_map(|p| p.spent.iter().cloned())
        .collect()
}

/// A burn's inputs, chosen, and the fee/change they imply.
struct Funded {
    selection: Selection,
    /// The chosen UTXOs, in selection order.
    inputs: Vec<TrackedUtxo>,
}

/// [`select_utxos`] over `pool`, with the chosen UTXOs copied out.
fn pick(pool: &[TrackedUtxo], burn_zat: u64, payload: BurnPayloadVersion) -> Option<Funded> {
    let selection = select_utxos(pool, burn_zat, payload)?;
    let inputs = selection.indices.iter().map(|&i| pool[i].clone()).collect();
    Some(Funded { selection, inputs })
}

/// The change outputs of our burns in flight that no later burn in flight
/// already spends, as spendable UTXOs -- unconfirmed, so only for chaining
/// (see the module docs). Only burns the node has *in its mempool* count:
/// a child of a burn the node doesn't know would be refused, and a burn
/// mined since this tip's resolution has a confirmed change output that
/// the funding pass already found (offering it here too would let one
/// selection spend it twice). An outpoint already in the pool is skipped
/// for the same reason.
fn unconfirmed_change(node: &impl Node, state: &MinerState) -> Result<Vec<TrackedUtxo>, RpcError> {
    let reserved = reserved(state);
    let mut out = Vec::new();
    for p in &state.pending {
        let Some(change) = p.change_outpoint() else {
            continue;
        };
        if reserved.contains(&change)
            || state
                .utxos
                .iter()
                .any(|u| u.txid == change.txid && u.vout == change.vout)
            || node.tx_status(&p.txid)? != TxStatus::Mempool
        {
            continue;
        }
        out.push(TrackedUtxo {
            txid: change.txid,
            vout: change.vout,
            value_zat: p.change_zat,
        });
    }
    Ok(out)
}

/// Re-reads the address's confirmed UTXOs, drops tracked entries the node
/// no longer lists, and -- only if the tracked pool can't cover `burn_zat`
/// -- merges in newly found spendable outputs. Only if confirmed funds
/// still fall short does it add the unconfirmed change of our burns in
/// flight (chaining). Returns the inputs, or
/// [`EpochError::InsufficientFunds`] (or, where coinbase can't fund a burn
/// and some is waiting, [`EpochError::CoinbaseMustBeShielded`]).
fn fund_burn(
    node: &impl Node,
    funding: &mut Funding,
    address: &str,
    burn_zat: u64,
    payload: BurnPayloadVersion,
    state: &mut MinerState,
) -> Result<Funded, EpochError> {
    let snapshot = node.address_utxos(address)?;
    let stale = funding::drop_stale(&mut state.utxos, &snapshot);
    if !stale.is_empty() {
        let zat: u64 = stale.iter().map(|u| u.value_zat).sum();
        println!(
            "funding: dropped {} tracked UTXO(s) ({zat} zat) the node no longer lists for {address}",
            stale.len()
        );
    }
    // Testnet/mainnet: coinbase tracked by an older miner (or state file)
    // can't fund a burn; the node would reject it.
    let coinbase = funding.drop_coinbase(node, &mut state.utxos)?;
    if !coinbase.is_empty() {
        let zat: u64 = coinbase.iter().map(|u| u.value_zat).sum();
        println!(
            "funding: dropped {} tracked coinbase UTXO(s) ({zat} zat): coinbase must be shielded before it can fund a transparent burn on this network",
            coinbase.len()
        );
    }
    if let Some(funded) = pick(&state.utxos, burn_zat, payload) {
        return Ok(funded);
    }
    // The tracked pool is short: take in whatever else the address holds
    // (first epoch, a top-up, a matured coinbase on regtest).
    let found = funding.classify(node, &snapshot, &reserved(state))?;
    let spendable_zat: u64 = found.spendable.iter().map(|u| u.value_zat).sum();
    let added = funding::merge(&mut state.utxos, found.spendable);
    if added > 0 {
        let held_back = if funding.coinbase_spendable() {
            format!("{} zat coinbase maturing", found.immature_zat)
        } else {
            format!(
                "{} zat coinbase (must be shielded first)",
                found.coinbase_zat
            )
        };
        println!(
            "funding: {added} new spendable UTXO(s) for {address} via getaddressutxos at tip {} ({spendable_zat} zat spendable, {held_back}, {} zat reserved by our burns in flight)",
            snapshot.tip_height, found.reserved_zat
        );
    }
    if let Some(funded) = pick(&state.utxos, burn_zat, payload) {
        return Ok(funded);
    }
    // Confirmed funds are short (usually: the only coin is the change of a
    // burn still in flight). Chain onto that change.
    let change = unconfirmed_change(node, state)?;
    if !change.is_empty() {
        let mut pool = state.utxos.clone();
        pool.extend(change);
        if let Some(funded) = pick(&pool, burn_zat, payload) {
            return Ok(funded);
        }
    }
    Err(if found.coinbase_zat > 0 {
        EpochError::CoinbaseMustBeShielded {
            burn_zat,
            spendable_zat,
            coinbase_zat: found.coinbase_zat,
        }
    } else {
        EpochError::InsufficientFunds {
            burn_zat,
            spendable_zat,
            immature_zat: found.immature_zat,
        }
    })
}

/// Attempts one mining epoch: find funding, check both budgets, and build,
/// sign and broadcast a burn. Write-ahead: the burn is appended to
/// `state.pending` (its inputs leave `state.utxos`) and `persist`ed
/// *before* it is broadcast, so a process killed at any point after the
/// broadcast still finds it on restart -- its epoch is recorded and its
/// cost counted against both budgets. A burn saved but never admitted is
/// simply re-sent by [`resolve_pending`] (the same signed bytes, so never a
/// double-spend). If the node cleanly refuses it, it is taken back out and
/// state is persisted again. The caller follows a submitted burn with
/// [`resolve_pending`] on later blocks. The caller
/// keeps at most [`MAX_IN_FLIGHT`] in flight. Inputs reserved by a burn in
/// flight are never selected again, so nothing is double-spent; when no
/// confirmed coin suffices, the unconfirmed change of a burn in flight is
/// spent (see the module docs).
///
/// `target_height` is only a *target*, used for the transaction's expiry
/// -- not what gets recorded: the epoch's height is the block the burn
/// actually confirms in (see [`resolve_pending`]). The consensus branch the
/// burn is signed for is zebrad's, read (`getblockchaininfo`) right before
/// signing: `consensus.nextblock`, so the burn follows a network upgrade
/// (NU7) as soon as zebrad does, with no activation height compiled in. If
/// zebrad has moved on since `target_height` was chosen, its next block is
/// the target instead. A branch this build doesn't know is refused
/// ([`burn_wallet::BurnTxError::Branch`]): nothing is signed or sent.
///
/// `expiry_delta` is `--expiry-delta`: blocks until the burn expires;
/// `None` is 40, or 120 once zebrad's next block is NU7 or later (ZIP 218).
///
/// `sova_ref` makes the burn a SIP-8 version-2 (anchored) burn referencing
/// that Sova block; `None` builds the SIP-1 v1 burn, exactly as before
/// SIP-8. Only [`crate::anchor::Sip8Gate::admit`] may produce a `Some`:
/// it is what checks that SIP-8 is active at `target_height`.
///
/// Budget is checked against two independent caps (D5, see
/// [`BudgetKind`]): `--budget-zat`, measured against spend since this
/// `mine` invocation started, and the optional `--lifetime-budget-zat`,
/// measured against everything this keystore has ever spent. Either one
/// running short of this epoch's cost produces
/// [`EpochOutcome::BudgetExhausted`] before any funds move. What the burns
/// already in flight will cost is held back from both first
/// ([`MinerState::in_flight_cost_zat`]): they are not charged until they
/// confirm, but they will be, so burns confirming after this one was sent
/// can't push either cap past its limit (for `--budget-zat`: burns this
/// run sends; one inherited in flight from an earlier run is charged to
/// whichever run it confirms in).
///
/// # Errors
///
/// Returns [`EpochError`] if the wallet cannot fund `burn_zat` at all
/// (regardless of budget), or if an RPC/build step fails.
#[allow(clippy::too_many_arguments)]
pub(crate) fn attempt_epoch(
    node: &impl Node,
    funding: &mut Funding,
    network: Network,
    keypair: &Keypair,
    evm_address: [u8; 20],
    signal_bits: u32,
    burn_zat: u64,
    target_height: u32,
    expiry_delta: Option<u32>,
    sova_ref: Option<SovaRef>,
    state: &mut MinerState,
    persist: &mut dyn FnMut(&MinerState) -> Result<(), StateError>,
) -> Result<EpochOutcome, EpochError> {
    debug_assert!(
        state.pending.len() < MAX_IN_FLIGHT,
        "at most {MAX_IN_FLIGHT} burns in flight"
    );
    let address = keypair.encode_address(network);
    let payload = if sova_ref.is_some() {
        BurnPayloadVersion::V2
    } else {
        BurnPayloadVersion::V1
    };
    let Funded { selection, inputs } =
        fund_burn(node, funding, &address, burn_zat, payload, state)?;

    let total_cost_zat = burn_zat.saturating_add(selection.fee_zat);
    let in_flight_zat = state.in_flight_cost_zat();

    // Two independent caps (D5): `--budget-zat` (this invocation only) is
    // checked first since it's the primary, always-declared flag; the
    // optional `--lifetime-budget-zat` is checked second as an additional
    // ceiling across every invocation ever run against this keystore. A
    // generous per-invocation budget does not override a tighter lifetime
    // cap, and vice versa -- either one exhausting stops this epoch.
    let remaining_zat = state.budget_remaining_zat().saturating_sub(in_flight_zat);
    if total_cost_zat > remaining_zat {
        return Ok(EpochOutcome::BudgetExhausted {
            kind: BudgetKind::PerInvocation,
            needed_zat: total_cost_zat,
            remaining_zat,
            in_flight_zat,
        });
    }
    if let Some(lifetime_remaining_zat) = state
        .lifetime_budget_remaining_zat()
        .map(|r| r.saturating_sub(in_flight_zat))
        && total_cost_zat > lifetime_remaining_zat
    {
        return Ok(EpochOutcome::BudgetExhausted {
            kind: BudgetKind::Lifetime,
            needed_zat: total_cost_zat,
            remaining_zat: lifetime_remaining_zat,
            in_flight_zat,
        });
    }

    let utxos: Vec<Utxo> = inputs
        .iter()
        .map(|t| {
            Ok(Utxo {
                outpoint: OutPoint::new(decode_rpc_hash("txid", &t.txid)?, t.vout),
                value_zat: t.value_zat,
            })
        })
        .collect::<Result<_, burn_wallet::utxo::UtxoError>>()?;

    // The branch and the height it is for come from one zebrad snapshot,
    // read as late as possible: zebrad checks a new transaction against
    // its next block's branch, and the ZIP 244 sighash commits to it.
    let next = node.next_block()?;
    let target_height = target_height.max(next.next_height());
    let request = BurnTxRequest {
        network,
        target_height,
        consensus_branch_id: next.next_block_branch_id,
        expiry_delta,
        utxos,
        change_and_signing_key: *keypair,
        evm_address,
        signal_bits,
        burn_value_zat: burn_zat,
        fee_zat: selection.fee_zat,
        sova_ref,
    };
    let built = build_burn_transaction(&request)?;
    println!(
        "burn {}: signed for consensus branch {}, zebrad's next block {} (tip on {:08x}), expiry height {}",
        encode_rpc_hash(built.txid),
        branch::describe(built.branch_id),
        next.next_height(),
        next.chain_tip_branch_id,
        built.expiry_height
    );
    let pending = PendingBurn {
        txid: encode_rpc_hash(built.txid),
        raw_hex: hex::encode(&built.raw),
        spent: inputs
            .iter()
            .map(|t| OutPointRef {
                txid: t.txid.clone(),
                vout: t.vout,
            })
            .collect(),
        burn_zat,
        fee_zat: selection.fee_zat,
        change_zat: if selection.has_change {
            selection.change_zat
        } else {
            0
        },
        expiry_height: u64::from(built.expiry_height),
    };

    // Write-ahead: in flight (inputs out of the pool, reserved by
    // `pending`; its change joins once it confirms) and on disk before the
    // node sees it. An input that is another burn's unconfirmed change was
    // never in the pool, and is kept out of it from here by `reserved`.
    let spent: BTreeSet<&OutPointRef> = pending.spent.iter().collect();
    let (taken, kept): (Vec<TrackedUtxo>, Vec<TrackedUtxo>) =
        std::mem::take(&mut state.utxos).into_iter().partition(|t| {
            spent.contains(&OutPointRef {
                txid: t.txid.clone(),
                vout: t.vout,
            })
        });
    state.utxos = kept;
    state.pending.push(pending.clone());
    let undo = |state: &mut MinerState, taken: Vec<TrackedUtxo>| {
        state.pending.pop();
        state.utxos.extend(taken);
    };
    if let Err(e) = persist(state) {
        undo(state, taken);
        return Err(e.into());
    }

    match broadcast(node, &pending.raw_hex, &pending.txid, BROADCAST_ATTEMPTS) {
        Broadcast::Sent => {}
        // Fail safe: the node may have it. Keep it in flight (inputs
        // reserved) rather than build a second burn from the same inputs;
        // `resolve_pending` re-sends it on each new block until it is
        // mined or expires.
        Broadcast::Ambiguous(e) => eprintln!(
            "warning: burn {}: broadcast outcome unknown ({e}); tracking it as in flight",
            pending.txid
        ),
        Broadcast::Rejected(message) => {
            undo(state, taken);
            if let Err(e) = persist(state) {
                // The saved state still holds the refused burn: a restart
                // re-sends it, which is refused again, and its inputs stay
                // reserved until its expiry height. Safe, just idle.
                eprintln!(
                    "warning: burn {}: could not save state after the node refused it: {e}",
                    pending.txid
                );
            }
            return Err(EpochError::Rejected {
                txid: pending.txid,
                message,
            });
        }
    }
    Ok(EpochOutcome::Submitted(pending))
}

/// One look at every burn in flight, oldest (parent) first; returns what
/// became of each, and the RPC error that cut the pass short, if any (the
/// burns not reached stay in flight, and everything resolved before it is
/// already applied to `state`).
///
/// Per burn: mined -> record the epoch at its real confirming height and
/// track its change (unless a later burn in flight already spends it);
/// unknown to the node with the tip at or past its expiry height, or
/// spending the change of a burn dropped in this pass -> drop it (its
/// confirmed inputs are free again: the node still lists them, so the next
/// funding pass picks them back up); unknown but still minable -> send the
/// same signed bytes again (after its parent, by the order); otherwise it
/// is still in flight.
pub(crate) fn resolve_pending(
    node: &impl Node,
    state: &mut MinerState,
) -> (Vec<PendingResolution>, Option<RpcError>) {
    let mut resolutions = match unconfirm_reorged(node, state) {
        Ok(r) => r,
        Err(e) => return (Vec::new(), Some(e)),
    };
    // Change outputs of burns dropped in this pass: a burn spending one
    // can never be mined either.
    let mut dead: BTreeSet<OutPointRef> = BTreeSet::new();
    let mut i = 0;
    while i < state.pending.len() {
        let pending = state.pending[i].clone();
        let resolution = match resolve_one(node, &pending, &dead) {
            Ok(r) => r,
            Err(e) => return (resolutions, Some(e)),
        };
        match &resolution {
            PendingResolution::InFlight { .. } | PendingResolution::Reorged { .. } => i += 1,
            PendingResolution::Confirmed(record) => {
                state.pending.remove(i);
                state.recent.push(RecentBurn {
                    burn: pending.clone(),
                    height: record.height,
                });
                if let Some(change) = pending.change_outpoint()
                    && !reserved(state).contains(&change)
                    && !state
                        .utxos
                        .iter()
                        .any(|u| u.txid == change.txid && u.vout == change.vout)
                {
                    state.utxos.push(TrackedUtxo {
                        txid: change.txid,
                        vout: change.vout,
                        value_zat: pending.change_zat,
                    });
                }
            }
            PendingResolution::Dropped { .. } => {
                state.pending.remove(i);
                dead.extend(pending.change_outpoint());
            }
        }
        let resolution = match resolution {
            PendingResolution::Confirmed(mut record) => {
                record.epoch = u64::try_from(state.epochs.len()).unwrap_or(u64::MAX) + 1;
                state.record_epoch(record.clone());
                PendingResolution::Confirmed(record)
            }
            other => other,
        };
        resolutions.push(resolution);
    }
    // Stop watching burns buried deeper than any reorg we plan for.
    if let Some(top) = state.recent.iter().map(|r| r.height).max() {
        state
            .recent
            .retain(|r| top.saturating_sub(r.height) < REORG_WATCH_DEPTH);
    }
    (resolutions, None)
}

/// The reorg check of [`resolve_pending`]: every recently confirmed burn
/// the node no longer has in its best chain (orphaned by a Zcash reorg;
/// zebrad doesn't return such a tx to its mempool) is un-recorded and put
/// back in flight, ahead of the burns already in flight -- it is their
/// parent if any spends its change. Its change leaves the pool and its
/// inputs are reserved again. One re-mined at another height has its
/// record moved there.
fn unconfirm_reorged(
    node: &impl Node,
    state: &mut MinerState,
) -> Result<Vec<PendingResolution>, RpcError> {
    let mut back = Vec::new();
    let mut resolutions = Vec::new();
    let mut i = 0;
    while i < state.recent.len() {
        let height = state.recent[i].height;
        let txid = state.recent[i].burn.txid.clone();
        match node.tx_status(&txid)? {
            TxStatus::Confirmed { height: now } => {
                if now != height {
                    state.recent[i].height = now;
                    if let Some(e) = state.epochs.iter_mut().find(|e| e.txid == txid) {
                        e.height = now;
                    }
                }
                i += 1;
            }
            TxStatus::Mempool | TxStatus::Unknown => {
                let recent = state.recent.remove(i);
                state.unrecord_epoch(&txid);
                let burn = recent.burn;
                let out: BTreeSet<OutPointRef> = burn
                    .spent
                    .iter()
                    .cloned()
                    .chain(burn.change_outpoint())
                    .collect();
                state.utxos.retain(|u| {
                    !out.contains(&OutPointRef {
                        txid: u.txid.clone(),
                        vout: u.vout,
                    })
                });
                resolutions.push(PendingResolution::Reorged { txid, height });
                back.push(burn);
            }
        }
    }
    if !back.is_empty() {
        back.append(&mut state.pending);
        state.pending = back;
    }
    Ok(resolutions)
}

/// What became of one burn in flight (see [`resolve_pending`]; this only
/// asks the node and re-sends, `state` is updated by the caller). A
/// `Confirmed` record's `epoch` is filled in by the caller.
fn resolve_one(
    node: &impl Node,
    pending: &PendingBurn,
    dead: &BTreeSet<OutPointRef>,
) -> Result<PendingResolution, RpcError> {
    match node.tx_status(&pending.txid)? {
        TxStatus::Confirmed { height } => Ok(PendingResolution::Confirmed(EpochRecord {
            epoch: 0,
            height,
            burn_zat: pending.burn_zat,
            fee_zat: pending.fee_zat,
            change_zat: pending.change_zat,
            txid: pending.txid.clone(),
        })),
        TxStatus::Mempool => Ok(PendingResolution::InFlight {
            txid: pending.txid.clone(),
        }),
        TxStatus::Unknown => {
            if let Some(parent) = pending.spent.iter().find(|o| dead.contains(o)) {
                return Ok(PendingResolution::Dropped {
                    txid: pending.txid.clone(),
                    reason: format!(
                        "it spends the change of burn {}, which was dropped; its other inputs are free again",
                        parent.txid
                    ),
                });
            }
            let tip = node.tip_height()?;
            if tip >= pending.expiry_height {
                return Ok(PendingResolution::Dropped {
                    txid: pending.txid.clone(),
                    reason: format!(
                        "not mined by its expiry height {} (tip {tip}); its inputs are free again",
                        pending.expiry_height
                    ),
                });
            }
            // The node doesn't have it (evicted, restarted, or the
            // original send never arrived) but it can still be mined:
            // send the same signed bytes again. Idempotent -- the txid
            // can't change, so this can never double-spend.
            match broadcast(node, &pending.raw_hex, &pending.txid, 1) {
                Broadcast::Sent => {
                    println!("burn {}: re-sent to the node", pending.txid);
                }
                Broadcast::Rejected(message)
                | Broadcast::Ambiguous(RpcError::RpcFailure { message, .. }) => {
                    eprintln!(
                        "warning: burn {}: re-send refused ({message}); keeping its inputs reserved until its expiry height {}",
                        pending.txid, pending.expiry_height
                    );
                }
                Broadcast::Ambiguous(e) => return Err(e),
            }
            Ok(PendingResolution::InFlight {
                txid: pending.txid.clone(),
            })
        }
    }
}

/// What a broadcast came to.
#[derive(Debug)]
pub(crate) enum Broadcast {
    /// The node has the transaction (it accepted it, answered that it
    /// already has it, or errored but has it anyway).
    Sent,
    /// The node answered with an error every time and doesn't have the
    /// transaction: a real rejection. Carries the node's message.
    Rejected(String),
    /// The node couldn't be reached to find out (transport failure): it
    /// may or may not have the transaction.
    Ambiguous(RpcError),
}

/// `sendrawtransaction` answers meaning "I already have exactly this
/// transaction" (zebrad's mempool `InMempool` / `Mined` errors, and
/// zcashd's wordings), matched case-insensitively. Retrying the same
/// signed bytes after an ambiguous failure lands here.
fn already_has_tx(message: &str) -> bool {
    const KNOWN: [&str; 6] = [
        "already exists in mempool",
        "already in the mempool",
        "already in best chain",
        "committed to the best chain",
        "already in block chain",
        "txn-already-known",
    ];
    let message = message.to_ascii_lowercase();
    KNOWN.iter().any(|k| message.contains(k))
}

/// Whether the node has `txid` (mempool or best chain), asking up to
/// [`ADMISSION_CHECKS`] times: zebrad verifies a submitted transaction
/// asynchronously, so an error answer can come back before admission
/// finishes.
fn node_has_tx(node: &impl Node, txid: &str) -> Result<bool, RpcError> {
    for check in 1..=ADMISSION_CHECKS {
        if node.tx_status(txid)? != TxStatus::Unknown {
            return Ok(true);
        }
        if check < ADMISSION_CHECKS {
            thread::sleep(ADMISSION_CHECK_DELAY);
        }
    }
    Ok(false)
}

/// Broadcasts `raw_hex` (whose txid is `txid`) idempotently, sending the
/// same signed bytes up to `attempts` times. An error from
/// `sendrawtransaction` is not proof the node refused it: zebrad can
/// answer e.g. `channel closed` and still admit the transaction (seen on
/// the box). So after any error the node is asked whether it has the
/// transaction before the error counts; a retry answered "already exists
/// in mempool" / "already in best chain" is success.
pub(crate) fn broadcast(node: &impl Node, raw_hex: &str, txid: &str, attempts: u32) -> Broadcast {
    let mut last = None;
    for attempt in 1..=attempts.max(1) {
        let error = match node.send_raw(raw_hex) {
            Ok(node_txid) => {
                if node_txid != txid {
                    eprintln!(
                        "warning: node returned txid {node_txid} for burn {txid}; tracking the locally computed one"
                    );
                }
                return Broadcast::Sent;
            }
            Err(RpcError::RpcFailure { message, .. }) if already_has_tx(&message) => {
                println!(
                    "burn {txid}: node answered {message:?}; it has the tx, counting it as sent"
                );
                return Broadcast::Sent;
            }
            Err(e) => e,
        };
        match node_has_tx(node, txid) {
            Ok(true) => {
                println!(
                    "burn {txid}: node answered \"{error}\" but has the tx; counting it as sent"
                );
                return Broadcast::Sent;
            }
            Ok(false) => {}
            Err(check) => {
                eprintln!("warning: burn {txid}: could not ask the node about it: {check}");
                last = Some(Broadcast::Ambiguous(error));
                if attempt < attempts {
                    thread::sleep(BROADCAST_RETRY_DELAY);
                }
                continue;
            }
        }
        if attempt < attempts {
            eprintln!(
                "warning: burn {txid}: sendrawtransaction failed ({error}) and the node doesn't have it; re-sending the same tx ({attempt}/{attempts})"
            );
            thread::sleep(BROADCAST_RETRY_DELAY);
        }
        last = Some(match error {
            RpcError::RpcFailure { message, .. } => Broadcast::Rejected(message),
            other => Broadcast::Ambiguous(other),
        });
    }
    last.unwrap_or_else(|| Broadcast::Rejected("no broadcast attempted".to_string()))
}

#[cfg(test)]
// Test code: an unexpected `None` here is a test failure, and `.expect()`
// with a message is more useful here than threading `Result` through.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use zcash_protocol::consensus::{BlockHeight, BranchId};

    use super::*;
    use crate::funding::tests::{FakeNode, SendScript, txid, txid_of};

    fn utxo(value_zat: u64) -> TrackedUtxo {
        TrackedUtxo {
            txid: "ab".repeat(32),
            vout: 0,
            value_zat,
        }
    }

    #[test]
    fn selects_single_large_utxo_with_change() {
        let pool = vec![utxo(1_000_000)];
        let sel = select_utxos(&pool, 100_000, BurnPayloadVersion::V1).expect("should select");
        assert_eq!(sel.indices, vec![0]);
        // 1 input, payload (38B) + eater (34B) + change (34B) = 106B out ->
        // ceil(106/34) = 4 logical actions -> 20,000 zat (see fee.rs).
        assert_eq!(sel.fee_zat, 20_000);
        assert!(sel.has_change);
        assert_eq!(sel.change_zat, 1_000_000 - 100_000 - 20_000);
    }

    #[test]
    fn folds_sub_dust_change_into_fee() {
        // Exactly burn + fee_with_change: change would be 0.
        let pool = vec![utxo(100_000 + 20_000)];
        let sel = select_utxos(&pool, 100_000, BurnPayloadVersion::V1).expect("should select");
        assert!(!sel.has_change);
        assert_eq!(sel.change_zat, 0);
        // Dropping the change output lowers the fee formula's own value
        // (payload + eater only -> 15,000), but every zatoshi that isn't
        // burned or returned as change is folded into the real fee paid,
        // so the total fee actually charged is unchanged: 20,000.
        assert_eq!(sel.fee_zat, 20_000);
    }

    #[test]
    fn prefers_largest_utxos_first() {
        let pool = vec![utxo(500), utxo(2_000_000), utxo(1_000)];
        let sel = select_utxos(&pool, 100_000, BurnPayloadVersion::V1).expect("should select");
        assert_eq!(sel.indices, vec![1]);
    }

    #[test]
    fn combines_multiple_utxos_when_needed() {
        let pool = vec![utxo(70_000), utxo(70_000)];
        let sel = select_utxos(&pool, 100_000, BurnPayloadVersion::V1).expect("should select");
        assert_eq!(sel.indices.len(), 2);
        assert!(sel.has_change);
        // 2 inputs still leaves the output side (4 actions) dominant over
        // the input side (2 actions): same 20,000 zat as the single-input
        // case above.
        assert_eq!(sel.fee_zat, zip317_fee_zat(2, BurnPayloadVersion::V1, true));
        assert_eq!(sel.change_zat, 140_000 - 100_000 - 20_000);
    }

    #[test]
    fn returns_none_when_wallet_is_empty() {
        let pool: Vec<TrackedUtxo> = vec![];
        assert!(select_utxos(&pool, 100_000, BurnPayloadVersion::V1).is_none());
    }

    #[test]
    fn returns_none_when_funds_are_insufficient() {
        let pool = vec![utxo(1_000)];
        assert!(select_utxos(&pool, 100_000, BurnPayloadVersion::V1).is_none());
    }

    /// A regtest-shaped node at tip 200 whose address holds one confirmed
    /// 10,000,000 zat ordinary transfer (txid(7):0, mined at 150).
    fn funded_node() -> FakeNode {
        let node = FakeNode::default();
        node.tip.set(200);
        node.fund(&txid(7), 0, 10_000_000, 150);
        node
    }

    fn fresh_state() -> MinerState {
        MinerState::new("tmAddr".to_string(), "ab".repeat(20))
    }

    fn attempt(
        node: &FakeNode,
        state: &mut MinerState,
        burn_zat: u64,
    ) -> Result<EpochOutcome, EpochError> {
        attempt_on(node, state, burn_zat, Network::Regtest, 201)
    }

    fn attempt_on(
        node: &FakeNode,
        state: &mut MinerState,
        burn_zat: u64,
        network: Network,
        target_height: u32,
    ) -> Result<EpochOutcome, EpochError> {
        let keypair = Keypair::generate();
        if network != Network::Regtest {
            // What a zebrad on `network` reports for `target_height`.
            node.next_branch.set(Some(u32::from(BranchId::for_height(
                &network,
                BlockHeight::from_u32(target_height),
            ))));
        }
        attempt_epoch(
            node,
            &mut Funding::new(network),
            network,
            &keypair,
            [0u8; 20],
            0,
            burn_zat,
            target_height,
            None,
            None,
            state,
            &mut |_| Ok(()),
        )
    }

    /// A testnet height well past NU5 (so the burn builds as v5).
    const TESTNET_TARGET: u32 = 3_300_000;

    /// Testnet: an address holding only (mature) coinbase can't fund a
    /// burn, and the error says the coinbase must be shielded and where
    /// the docs are -- instead of broadcasting a burn zebrad would reject.
    #[test]
    fn testnet_coinbase_alone_must_be_shielded_first() {
        let mut node = FakeNode::default();
        node.tip.set(u64::from(TESTNET_TARGET) - 1);
        node.coinbase.insert(txid(3));
        node.coinbase.insert(txid(4));
        node.fund(&txid(3), 0, 125_000_000, 3_000_000);
        node.fund(&txid(4), 0, 125_000_000, u64::from(TESTNET_TARGET) - 5);
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, None);

        let err = attempt_on(&node, &mut state, 100_000, Network::Test, TESTNET_TARGET)
            .expect_err("coinbase-only funding must fail on testnet");
        match &err {
            EpochError::CoinbaseMustBeShielded {
                spendable_zat,
                coinbase_zat,
                ..
            } => {
                assert_eq!(*spendable_zat, 0);
                assert_eq!(*coinbase_zat, 250_000_000);
            }
            other => panic!("expected CoinbaseMustBeShielded, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains(
                "250000000 zat of coinbase must be shielded before it can fund a transparent burn"
            ),
            "{msg}"
        );
        assert!(msg.contains("docs/ops/keeper-miner.md"), "{msg}");
        assert!(node.sent.borrow().is_empty());
        assert!(state.utxos.is_empty());
    }

    /// Testnet: with a transfer beside the coinbase, the burn spends only
    /// the transfer -- also when an older state file still tracks the
    /// coinbase output.
    #[test]
    fn testnet_burn_spends_only_non_coinbase() {
        let mut node = FakeNode::default();
        node.tip.set(u64::from(TESTNET_TARGET) - 1);
        node.coinbase.insert(txid(3));
        node.fund(&txid(3), 0, 125_000_000, 3_000_000);
        node.fund(&txid(5), 1, 10_000_000, u64::from(TESTNET_TARGET) - 2);
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, None);
        state.utxos.push(TrackedUtxo {
            txid: txid(3),
            vout: 0,
            value_zat: 125_000_000,
        });

        match attempt_on(&node, &mut state, 100_000, Network::Test, TESTNET_TARGET) {
            Ok(EpochOutcome::Submitted(pending)) => {
                assert_eq!(
                    pending.spent,
                    vec![OutPointRef {
                        txid: txid(5),
                        vout: 1
                    }]
                );
            }
            other => panic!("expected Submitted, got {other:?}"),
        }
        assert!(
            state.utxos.iter().all(|u| u.txid != txid(3)),
            "tracked coinbase was dropped"
        );
    }

    /// Regtest is unchanged: coinbase alone (mature) funds the burn.
    #[test]
    fn regtest_burn_still_spends_mature_coinbase() {
        let mut node = FakeNode::default();
        node.tip.set(200);
        node.coinbase.insert(txid(3));
        node.fund(&txid(3), 0, 625_000_000, 100); // 101 confirmations
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, None);
        assert!(matches!(
            attempt(&node, &mut state, 100_000),
            Ok(EpochOutcome::Submitted(_))
        ));
        assert_eq!(node.sent.borrow().len(), 1);
    }

    /// D5: `--budget-zat` gates on this invocation's own spend. A fresh
    /// invocation's `remaining_zat` is exactly its declared budget when
    /// nothing has been spent yet this run -- pinned here through the real
    /// `attempt_epoch` entry point, which is what `crate::mine::run`
    /// actually calls.
    #[test]
    fn attempt_epoch_reports_per_invocation_exhaustion() {
        let node = funded_node();
        let mut state = fresh_state();
        // Budget (50,000 zat) is too small for a 100,000 zat burn (which
        // costs 120,000 zat with fee) even though the wallet has funds.
        state.begin_invocation(50_000, 100_000, None);

        match attempt(&node, &mut state, 100_000).unwrap() {
            EpochOutcome::BudgetExhausted {
                kind,
                needed_zat,
                remaining_zat,
                in_flight_zat,
            } => {
                assert_eq!(in_flight_zat, 0);
                assert_eq!(kind, BudgetKind::PerInvocation);
                assert_eq!(needed_zat, 120_000);
                assert_eq!(remaining_zat, 50_000);
            }
            other => panic!("expected BudgetExhausted, got {other:?}"),
        }
        assert!(node.sent.borrow().is_empty(), "nothing may be broadcast");
        assert!(state.pending.is_empty());
    }

    /// D5: `--lifetime-budget-zat`, when set, is an *additional* cap on top
    /// of `--budget-zat` -- a generous fresh per-invocation budget does not
    /// override a tighter lifetime ceiling.
    #[test]
    fn attempt_epoch_reports_lifetime_exhaustion_despite_generous_invocation_budget() {
        let node = funded_node();
        let mut state = fresh_state();

        // Simulate a prior invocation's spend: 120,000 zat lifetime total.
        state.record_epoch(EpochRecord {
            epoch: 1,
            height: 101,
            burn_zat: 100_000,
            fee_zat: 20_000,
            change_zat: 0,
            txid: "aa".repeat(32),
        });

        // A new invocation with a huge per-invocation budget (plenty of
        // per-run headroom) but a lifetime cap of 150,000 zat -- only
        // 30,000 zat left under it.
        state.begin_invocation(1_000_000, 100_000, Some(150_000));
        assert_eq!(state.budget_remaining_zat(), 1_000_000); // per-invocation: unaffected
        assert_eq!(state.lifetime_budget_remaining_zat(), Some(30_000));

        match attempt(&node, &mut state, 100_000).unwrap() {
            EpochOutcome::BudgetExhausted {
                kind,
                needed_zat,
                remaining_zat,
                in_flight_zat,
            } => {
                assert_eq!(in_flight_zat, 0);
                assert_eq!(kind, BudgetKind::Lifetime);
                assert_eq!(needed_zat, 120_000);
                assert_eq!(remaining_zat, 30_000);
            }
            other => panic!("expected BudgetExhausted, got {other:?}"),
        }
        assert!(node.sent.borrow().is_empty(), "nothing may be broadcast");
    }

    /// The first epoch of a fresh keystore funded only by an ordinary
    /// transfer (a faucet drip / deshield -- no coinbase anywhere): found
    /// via `getaddressutxos`, spent, and the burn left in flight with its
    /// input reserved and out of the pool.
    #[test]
    fn transfer_funded_first_epoch_is_broadcast_and_left_pending() {
        let node = funded_node();
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, Some(1_000_000));

        let pending = match attempt(&node, &mut state, 100_000).unwrap() {
            EpochOutcome::Submitted(p) => p,
            other => panic!("expected Submitted, got {other:?}"),
        };
        assert_eq!(node.sent.borrow().len(), 1);
        assert_eq!(node.address_lookups.get(), 1);
        // One lookup: a 51-confirmation output might be coinbase, and this
        // node says it isn't.
        assert_eq!(node.coinbase_lookups.get(), 1);
        assert_eq!(
            pending.spent,
            vec![OutPointRef {
                txid: txid(7),
                vout: 0
            }]
        );
        assert_eq!(pending.burn_zat, 100_000);
        assert_eq!(pending.fee_zat, 20_000);
        assert_eq!(pending.change_zat, 10_000_000 - 120_000);
        assert_eq!(pending.expiry_height, 201 + 40);
        assert_eq!(state.pending, vec![pending.clone()]);
        assert!(state.utxos.is_empty(), "the input left the pool");
        assert_eq!(
            node.txs.borrow().get(&pending.txid),
            Some(&TxStatus::Mempool)
        );
    }

    /// Only young coinbase at the address: nothing is spendable, and the
    /// error says the funds are maturing rather than absent.
    #[test]
    fn immature_coinbase_alone_is_insufficient_and_says_so() {
        let mut node = FakeNode::default();
        node.tip.set(150);
        node.coinbase.insert(txid(3));
        node.fund(&txid(3), 0, 625_000_000, 100); // 51 confirmations
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, None);

        match attempt(&node, &mut state, 100_000) {
            Err(EpochError::InsufficientFunds {
                spendable_zat,
                immature_zat,
                ..
            }) => {
                assert_eq!(spendable_zat, 0);
                assert_eq!(immature_zat, 625_000_000);
            }
            other => panic!("expected InsufficientFunds, got {other:?}"),
        }
        assert!(node.sent.borrow().is_empty());
    }

    /// Tracked UTXOs the node no longer lists (e.g. an old change output
    /// after a restart) are dropped instead of being spent.
    #[test]
    fn stale_tracked_utxo_is_not_spent() {
        let node = funded_node();
        let mut state = fresh_state();
        state.utxos.push(TrackedUtxo {
            txid: txid(9),
            vout: 2,
            value_zat: 50_000_000, // larger, so it would be picked first
        });
        state.begin_invocation(1_000_000, 100_000, None);

        let EpochOutcome::Submitted(pending) = attempt(&node, &mut state, 100_000).unwrap() else {
            panic!("expected Submitted");
        };
        assert_eq!(
            pending.spent,
            vec![OutPointRef {
                txid: txid(7),
                vout: 0
            }]
        );
    }

    fn in_flight(state: &mut MinerState, expiry_height: u64) -> PendingBurn {
        let p = PendingBurn {
            txid: txid(0xb1),
            raw_hex: String::new(),
            spent: vec![OutPointRef {
                txid: txid(7),
                vout: 0,
            }],
            burn_zat: 100_000,
            fee_zat: 20_000,
            change_zat: 9_880_000,
            expiry_height,
        };
        state.pending = vec![p.clone()];
        p
    }

    /// [`resolve_pending`], for tests that expect no RPC error.
    fn resolve(node: &FakeNode, state: &mut MinerState) -> Vec<PendingResolution> {
        let (resolutions, error) = resolve_pending(node, state);
        assert!(error.is_none(), "{error:?}");
        resolutions
    }

    #[test]
    fn mined_pending_burn_is_recorded_at_its_real_height_with_its_change() {
        let node = funded_node();
        let mut state = fresh_state();
        let p = in_flight(&mut state, 241);
        node.txs
            .borrow_mut()
            .insert(p.txid.clone(), TxStatus::Confirmed { height: 203 });

        let [PendingResolution::Confirmed(record)] = &resolve(&node, &mut state)[..] else {
            panic!("expected Confirmed");
        };
        assert_eq!(record.epoch, 1);
        assert_eq!(record.height, 203);
        assert_eq!(record.txid, p.txid);
        assert!(state.pending.is_empty());
        assert_eq!(state.epochs.len(), 1);
        assert_eq!(state.total_spent_zat(), 120_000);
        assert_eq!(state.utxos.len(), 1);
        assert_eq!(state.utxos[0].txid, p.txid);
        assert_eq!(state.utxos[0].vout, 2);
        assert_eq!(state.utxos[0].value_zat, 9_880_000);
    }

    #[test]
    fn pending_burn_stays_in_flight_while_in_the_mempool_or_before_expiry() {
        let node = funded_node(); // tip 200
        let mut state = fresh_state();
        let p = in_flight(&mut state, 241);

        node.txs
            .borrow_mut()
            .insert(p.txid.clone(), TxStatus::Mempool);
        assert_eq!(
            resolve(&node, &mut state),
            vec![PendingResolution::InFlight {
                txid: p.txid.clone()
            }]
        );
        // Not (yet) known to the node, but the chain hasn't reached its
        // expiry height: it may still be mined. Keep the inputs reserved.
        node.txs.borrow_mut().remove(&p.txid);
        assert!(matches!(
            &resolve(&node, &mut state)[..],
            [PendingResolution::InFlight { .. }]
        ));
        assert_eq!(state.pending.len(), 1);
        assert!(state.epochs.is_empty());
    }

    #[test]
    fn expired_pending_burn_is_dropped_and_its_inputs_are_funding_again() {
        let node = funded_node(); // tip 200; txid(7):0 still unspent on chain
        let mut state = fresh_state();
        in_flight(&mut state, 200);

        assert!(matches!(
            &resolve(&node, &mut state)[..],
            [PendingResolution::Dropped { .. }]
        ));
        assert!(state.pending.is_empty());
        assert!(state.epochs.is_empty());
        assert_eq!(state.total_spent_zat(), 0);

        // The input is no longer reserved: the next epoch can spend it.
        state.begin_invocation(1_000_000, 100_000, None);
        let EpochOutcome::Submitted(next) = attempt(&node, &mut state, 100_000).unwrap() else {
            panic!("expected Submitted");
        };
        assert_eq!(
            next.spent,
            vec![OutPointRef {
                txid: txid(7),
                vout: 0
            }]
        );
    }

    #[test]
    fn nothing_pending_resolves_to_nothing() {
        let node = funded_node();
        let mut state = fresh_state();
        assert!(resolve(&node, &mut state).is_empty());
    }

    fn submit_with(
        script: Vec<SendScript>,
    ) -> (FakeNode, MinerState, Result<EpochOutcome, EpochError>) {
        let node = funded_node();
        *node.send_script.borrow_mut() = script;
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, None);
        let result = attempt(&node, &mut state, 100_000);
        (node, state, result)
    }

    /// The live box bug: zebrad answered `channel closed` but admitted the
    /// burn. That is a sent burn, not an error -- and it is not sent twice.
    #[test]
    fn rpc_error_for_a_burn_the_node_admitted_counts_as_sent() {
        let (node, state, result) =
            submit_with(vec![SendScript::AdmitButFail("channel closed".into())]);
        let EpochOutcome::Submitted(pending) = result.unwrap() else {
            panic!("expected Submitted");
        };
        assert_eq!(node.sent.borrow().len(), 1, "sent exactly once");
        assert_eq!(state.pending, vec![pending.clone()]);
        assert_eq!(
            node.txs.borrow().get(&pending.txid),
            Some(&TxStatus::Mempool)
        );
    }

    /// A send that fails before the node admits anything is re-sent with
    /// the same bytes; if the node then has it, that is success.
    #[test]
    fn transient_rejection_then_acceptance_is_sent_with_identical_bytes() {
        let (node, state, result) = submit_with(vec![SendScript::Reject(
            "mempool is disabled since synchronization is behind the chain tip".into(),
        )]);
        assert!(matches!(result.unwrap(), EpochOutcome::Submitted(_)));
        let sent = node.sent.borrow();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], sent[1], "a retry re-sends the same signed tx");
        assert_eq!(state.pending.len(), 1);
    }

    /// Re-sending a burn the node already has: "already exists in mempool"
    /// and "committed to the best chain" are success, never a rejection.
    #[test]
    fn retry_answered_already_known_is_success() {
        let (node, state, _) = submit_with(vec![]);
        let pending = state.pending[0].clone();
        let raw = pending.raw_hex.clone();
        assert!(matches!(
            broadcast(&node, &raw, &pending.txid, 1),
            Broadcast::Sent
        ));
        node.txs
            .borrow_mut()
            .insert(pending.txid.clone(), TxStatus::Confirmed { height: 202 });
        assert!(matches!(
            broadcast(&node, &raw, &pending.txid, 1),
            Broadcast::Sent
        ));
        assert!(already_has_tx("Transaction already in block chain"));
        assert!(already_has_tx("txn-already-known"));
        assert!(!already_has_tx(
            "transaction inputs were spent, or nullifiers were revealed, in the best chain"
        ));
    }

    /// A real rejection: every send refused, and the node doesn't have the
    /// tx. Nothing is pending, nothing left the pool.
    #[test]
    fn real_rejection_is_an_error_and_spends_nothing() {
        let refuse = || SendScript::Reject("transaction did not pass standard validation".into());
        let (node, state, result) = submit_with(vec![refuse(), refuse(), refuse()]);
        match result {
            Err(EpochError::Rejected { message, .. }) => {
                assert!(message.contains("standard validation"));
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert_eq!(node.sent.borrow().len(), 3);
        assert!(state.pending.is_empty());
        assert_eq!(state.utxos.len(), 1, "the input is still ours to spend");
    }

    /// Transport failures on every send: the node may have it. Fail safe:
    /// track it as in flight, inputs reserved, so no second burn is built
    /// from the same inputs.
    #[test]
    fn unknown_broadcast_outcome_is_tracked_as_in_flight() {
        let (node, state, result) = submit_with(vec![
            SendScript::TransportDown,
            SendScript::TransportDown,
            SendScript::TransportDown,
        ]);
        assert!(matches!(result.unwrap(), EpochOutcome::Submitted(_)));
        assert_eq!(node.sent.borrow().len(), 3);
        assert_eq!(state.pending.len(), 1);
        assert!(state.utxos.is_empty());
    }

    /// A pending burn the node lost (restart, eviction) is re-sent verbatim
    /// on the next look, while it can still be mined.
    #[test]
    fn unknown_pending_burn_is_resent_before_expiry() {
        let (node, mut state, _) = submit_with(vec![]);
        let pending = state.pending[0].clone();
        node.forget(&pending.txid); // the node forgot it
        assert_eq!(
            resolve(&node, &mut state),
            vec![PendingResolution::InFlight {
                txid: pending.txid.clone()
            }]
        );
        assert_eq!(node.sent.borrow().len(), 2);
        assert_eq!(txid_of(&node.sent.borrow()[1]), Some(pending.txid.clone()));
        assert_eq!(
            node.txs.borrow().get(&pending.txid),
            Some(&TxStatus::Mempool)
        );
    }

    // --- burning ahead: several burns in flight, chained change ---

    fn submitted(outcome: Result<EpochOutcome, EpochError>) -> PendingBurn {
        match outcome {
            Ok(EpochOutcome::Submitted(p)) => p,
            other => panic!("expected Submitted, got {other:?}"),
        }
    }

    // --- NU7: the branch comes from zebrad (docs/design/nu7-readiness.md B1/B2) ---

    /// The consensus branch ID in a sent v5 burn's header.
    fn sent_branch_id(raw_hex: &str) -> u32 {
        let raw = hex::decode(raw_hex).unwrap();
        u32::from_le_bytes(raw[8..12].try_into().unwrap())
    }

    /// Before NU7 nothing changes: the burn carries the branch zebrad
    /// reports (NU5 on regtest) and expires 40 blocks out.
    #[test]
    fn burn_is_signed_for_zebrads_next_block_branch() {
        let node = funded_node();
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, None);
        let pending = submitted(attempt(&node, &mut state, 100_000));
        assert_eq!(sent_branch_id(&node.sent.borrow()[0]), 0xc2d6_d0b4);
        assert_eq!(pending.expiry_height, 201 + 40);
    }

    /// zebrad's next block is NU7: the burn is signed for `77190ad9`,
    /// expires 120 blocks out (ZIP 218), and the node takes it.
    #[test]
    fn nu7_next_block_burn_is_signed_for_nu7_with_expiry_120() {
        let node = funded_node();
        node.next_branch.set(Some(0x7719_0ad9));
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, None);
        let pending = submitted(attempt(&node, &mut state, 100_000));
        assert_eq!(sent_branch_id(&node.sent.borrow()[0]), 0x7719_0ad9);
        assert_eq!(pending.expiry_height, 201 + 120);
        assert_eq!(node.mempool(), vec![pending.txid]);
    }

    /// The refusal: a branch this build doesn't know is an error before
    /// anything is signed or sent -- no wrong-branch burn, nothing in
    /// flight, no inputs reserved.
    #[test]
    fn unknown_next_block_branch_is_refused_and_nothing_is_sent() {
        let node = funded_node();
        node.next_branch.set(Some(0xffff_ffff));
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, None);
        let err = attempt(&node, &mut state, 100_000).unwrap_err();
        assert!(
            matches!(
                err,
                EpochError::Build(burn_wallet::BurnTxError::Branch(
                    burn_wallet::BranchError::Unknown {
                        id: 0xffff_ffff,
                        height: 201
                    }
                ))
            ),
            "{err:?}"
        );
        assert!(err.to_string().contains("refusing to sign"), "{err}");
        assert!(node.sent.borrow().is_empty());
        assert!(state.pending.is_empty(), "nothing in flight");
        assert_eq!(
            state
                .utxos
                .iter()
                .map(|u| u.txid.clone())
                .collect::<Vec<_>>(),
            vec![txid(7)],
            "the coin is still free"
        );
    }

    /// If zebrad's tip moved past the one the target was chosen from, the
    /// burn targets zebrad's next block: the branch and the height it is
    /// for come from the same snapshot.
    #[test]
    fn target_follows_zebrad_when_it_has_moved_on() {
        let node = funded_node();
        node.tip.set(260);
        node.next_branch.set(Some(0x7719_0ad9));
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, None);
        let pending = submitted(attempt(&node, &mut state, 100_000));
        assert_eq!(pending.expiry_height, 261 + 120);
    }

    /// `--expiry-delta` overrides the branch's default.
    #[test]
    fn expiry_delta_flag_overrides_the_default() {
        let node = funded_node();
        node.next_branch.set(Some(0x7719_0ad9));
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, None);
        let pending = submitted(attempt_epoch(
            &node,
            &mut Funding::new(Network::Regtest),
            Network::Regtest,
            &Keypair::generate(),
            [0u8; 20],
            0,
            100_000,
            201,
            Some(60),
            None,
            &mut state,
            &mut |_| Ok(()),
        ));
        assert_eq!(pending.expiry_height, 201 + 60);
    }

    /// Two burns sent back to back on a one-coin wallet: the second spends
    /// the first's unconfirmed change, and the node (which, like zebrad,
    /// admits a tx spending a mempool output) takes it.
    fn two_chained(node: &FakeNode, state: &mut MinerState) -> (PendingBurn, PendingBurn) {
        state.begin_invocation(10_000_000, 100_000, None);
        let first = submitted(attempt(node, state, 100_000));
        let second = submitted(attempt_on(node, state, 100_000, Network::Regtest, 202));
        (first, second)
    }

    #[test]
    fn second_burn_chains_onto_the_first_burns_unconfirmed_change() {
        let node = funded_node();
        let mut state = fresh_state();
        let (first, second) = two_chained(&node, &mut state);
        assert_eq!(
            first.spent,
            vec![OutPointRef {
                txid: txid(7),
                vout: 0
            }]
        );
        assert_eq!(second.spent, vec![first.change_outpoint().unwrap()]);
        assert_eq!(second.change_zat, first.change_zat - 120_000);
        assert_eq!(state.pending, vec![first.clone(), second.clone()]);
        assert_eq!(node.mempool(), vec![first.txid, second.txid]);
        assert!(
            state.utxos.is_empty(),
            "unconfirmed change never enters the pool"
        );
    }

    /// A wallet holding several confirmed coins burns from independent
    /// ones: no chaining while a confirmed coin can pay.
    #[test]
    fn a_confirmed_coin_is_preferred_over_unconfirmed_change() {
        let node = funded_node();
        node.fund(&txid(8), 1, 5_000_000, 160);
        let mut state = fresh_state();
        let (first, second) = two_chained(&node, &mut state);
        assert_eq!(
            first.spent,
            vec![OutPointRef {
                txid: txid(7),
                vout: 0
            }]
        );
        assert_eq!(
            second.spent,
            vec![OutPointRef {
                txid: txid(8),
                vout: 1
            }]
        );
    }

    /// A burn the node no longer has can't be chained onto (the child
    /// would be refused): with no confirmed coin left, the wallet is short
    /// -- and nothing is sent.
    #[test]
    fn chaining_skips_a_burn_the_node_does_not_have() {
        let node = funded_node();
        let mut state = fresh_state();
        state.begin_invocation(10_000_000, 100_000, None);
        let first = submitted(attempt(&node, &mut state, 100_000));
        node.forget(&first.txid);
        assert!(matches!(
            attempt(&node, &mut state, 100_000),
            Err(EpochError::InsufficientFunds { .. })
        ));
        assert_eq!(node.sent.borrow().len(), 1);
        assert_eq!(state.pending, vec![first]);
    }

    /// Budget: what the burns in flight will cost is held back before the
    /// next is sent, so neither cap can be overrun when they all confirm.
    #[test]
    fn burns_in_flight_are_held_against_both_budgets() {
        let node = funded_node();
        let mut state = fresh_state();
        // Room for one 120,000 zat burn, not two.
        state.begin_invocation(230_000, 100_000, None);
        submitted(attempt(&node, &mut state, 100_000));
        assert_eq!(state.total_spent_zat(), 0, "charged only when mined");
        assert_eq!(state.in_flight_cost_zat(), 120_000);
        assert_eq!(state.budget_remaining_zat(), 230_000);
        match attempt(&node, &mut state, 100_000).unwrap() {
            EpochOutcome::BudgetExhausted {
                kind,
                needed_zat,
                remaining_zat,
                in_flight_zat,
            } => {
                assert_eq!(kind, BudgetKind::PerInvocation);
                assert_eq!(needed_zat, 120_000);
                assert_eq!(remaining_zat, 110_000);
                assert_eq!(in_flight_zat, 120_000);
            }
            other => panic!("expected BudgetExhausted, got {other:?}"),
        }
        assert_eq!(node.sent.borrow().len(), 1);
        // Once it is mined, it is spent rather than held: same answer.
        node.mine(&node.mempool());
        resolve(&node, &mut state);
        assert_eq!(state.budget_remaining_zat(), 110_000);
        assert!(matches!(
            attempt(&node, &mut state, 100_000),
            Ok(EpochOutcome::BudgetExhausted {
                in_flight_zat: 0,
                remaining_zat: 110_000,
                ..
            })
        ));

        // The lifetime cap too.
        let node = funded_node();
        let mut state = fresh_state();
        state.begin_invocation(10_000_000, 100_000, Some(130_000));
        submitted(attempt(&node, &mut state, 100_000));
        match attempt(&node, &mut state, 100_000).unwrap() {
            EpochOutcome::BudgetExhausted {
                kind,
                remaining_zat,
                in_flight_zat,
                ..
            } => {
                assert_eq!(kind, BudgetKind::Lifetime);
                assert_eq!(remaining_zat, 10_000);
                assert_eq!(in_flight_zat, 120_000);
            }
            other => panic!("expected BudgetExhausted, got {other:?}"),
        }
    }

    /// The parent confirms first: its epoch is recorded, but its change
    /// stays out of the pool -- the child in flight spends it -- so the
    /// next burn chains onto the child instead of double-spending it.
    #[test]
    fn a_confirmed_parents_change_stays_reserved_for_its_child() {
        let node = funded_node();
        let mut state = fresh_state();
        let (first, second) = two_chained(&node, &mut state);
        node.mine(std::slice::from_ref(&first.txid)); // 201
        assert_eq!(
            resolve(&node, &mut state),
            vec![
                PendingResolution::Confirmed(EpochRecord {
                    epoch: 1,
                    height: 201,
                    burn_zat: 100_000,
                    fee_zat: 20_000,
                    change_zat: first.change_zat,
                    txid: first.txid.clone(),
                }),
                PendingResolution::InFlight {
                    txid: second.txid.clone()
                },
            ]
        );
        assert!(state.utxos.is_empty());
        assert!(reserved(&state).contains(&first.change_outpoint().unwrap()));

        let third = submitted(attempt_on(
            &node,
            &mut state,
            100_000,
            Network::Regtest,
            202,
        ));
        assert_eq!(third.spent, vec![second.change_outpoint().unwrap()]);

        node.mine(&node.mempool()); // 202: second and third
        let resolved = resolve(&node, &mut state);
        assert_eq!(resolved.len(), 2);
        assert!(state.pending.is_empty());
        assert_eq!(state.epochs.len(), 3);
        assert_eq!(state.total_spent_zat(), 360_000);
        assert_eq!(
            state.utxos,
            vec![TrackedUtxo {
                txid: third.txid.clone(),
                vout: 2,
                value_zat: 10_000_000 - 360_000,
            }]
        );
    }

    /// Restart with two burns in flight: the reloaded state reserves both,
    /// re-sends both parent-first to a node that lost them (the child is
    /// admitted only because its parent went first), and records both
    /// epochs once mined -- no double-spend, nothing lost.
    #[test]
    fn restart_with_two_burns_in_flight_resends_them_in_order() {
        let node = funded_node();
        let mut state = fresh_state();
        let (first, second) = two_chained(&node, &mut state);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        state.save(&path).unwrap();
        let mut state = MinerState::load(&path).unwrap();
        assert_eq!(state.pending, vec![first.clone(), second.clone()]);

        // The node restarted with an empty mempool.
        node.forget(&first.txid);
        node.forget(&second.txid);
        let sent_before = node.sent.borrow().len();
        assert_eq!(
            resolve(&node, &mut state),
            vec![
                PendingResolution::InFlight {
                    txid: first.txid.clone()
                },
                PendingResolution::InFlight {
                    txid: second.txid.clone()
                },
            ]
        );
        assert_eq!(
            node.sent.borrow()[sent_before..].to_vec(),
            vec![first.raw_hex.clone(), second.raw_hex.clone()]
        );
        assert_eq!(
            node.mempool(),
            vec![first.txid.clone(), second.txid.clone()]
        );

        // Nothing reserved is free to spend again.
        let reserved = reserved(&state);
        assert!(reserved.contains(&OutPointRef {
            txid: txid(7),
            vout: 0
        }));
        assert!(reserved.contains(&first.change_outpoint().unwrap()));

        node.mine(&[first.txid.clone(), second.txid.clone()]);
        assert_eq!(resolve(&node, &mut state).len(), 2);
        assert_eq!(state.epochs.len(), 2);
        assert_eq!(state.total_spent_zat(), 240_000);
    }

    /// Restart after the parent was mined while the miner was down.
    #[test]
    fn restart_after_the_parent_was_mined_records_it_and_keeps_the_child() {
        let node = funded_node();
        let mut state = fresh_state();
        let (first, second) = two_chained(&node, &mut state);
        let json = serde_json::to_string(&state).unwrap();
        node.mine(std::slice::from_ref(&first.txid));
        let mut state: MinerState = serde_json::from_str(&json).unwrap();
        let resolved = resolve(&node, &mut state);
        assert!(matches!(&resolved[0], PendingResolution::Confirmed(r) if r.txid == first.txid));
        assert_eq!(
            resolved[1],
            PendingResolution::InFlight {
                txid: second.txid.clone()
            }
        );
        assert_eq!(state.pending, vec![second]);
    }

    /// A dropped parent takes its child with it, even before the child's
    /// own expiry: the child's input can never exist. The parent's own
    /// (confirmed) input is free again.
    #[test]
    fn a_dropped_parent_drops_its_child() {
        let node = funded_node();
        let mut state = fresh_state();
        let (first, second) = two_chained(&node, &mut state);
        assert_eq!((first.expiry_height, second.expiry_height), (241, 242));
        node.forget(&first.txid);
        node.forget(&second.txid);
        node.tip.set(241);
        let resolved = resolve(&node, &mut state);
        match &resolved[..] {
            [
                PendingResolution::Dropped {
                    txid: a,
                    reason: ra,
                },
                PendingResolution::Dropped {
                    txid: b,
                    reason: rb,
                },
            ] => {
                assert_eq!((a, b), (&first.txid, &second.txid));
                assert!(ra.contains("expiry height 241"), "{ra}");
                assert!(
                    rb.contains(&format!("change of burn {}", first.txid)),
                    "{rb}"
                );
            }
            other => panic!("expected both dropped, got {other:?}"),
        }
        assert!(state.pending.is_empty());
        assert!(state.epochs.is_empty());
        assert_eq!(state.in_flight_cost_zat(), 0, "budget released");
        let next = submitted(attempt_on(
            &node,
            &mut state,
            100_000,
            Network::Regtest,
            242,
        ));
        assert_eq!(
            next.spent,
            vec![OutPointRef {
                txid: txid(7),
                vout: 0
            }]
        );
    }

    /// A child dropped after its parent confirmed frees the (now
    /// confirmed) change it spent.
    #[test]
    fn a_dropped_child_frees_its_parents_confirmed_change() {
        let node = funded_node();
        let mut state = fresh_state();
        let (first, second) = two_chained(&node, &mut state);
        node.mine(std::slice::from_ref(&first.txid)); // 201
        resolve(&node, &mut state);
        node.forget(&second.txid);
        node.tip.set(second.expiry_height);
        assert!(matches!(
            &resolve(&node, &mut state)[..],
            [PendingResolution::Dropped { .. }]
        ));
        let next = submitted(attempt_on(
            &node,
            &mut state,
            100_000,
            Network::Regtest,
            243,
        ));
        assert_eq!(next.spent, vec![first.change_outpoint().unwrap()]);
    }

    /// Review finding 4: the parent is mined between this tip's
    /// resolution (it was still in the mempool) and the next burn's
    /// funding. Its change is now a confirmed UTXO the pool picks up --
    /// and must not also be offered as unconfirmed change, or a burn
    /// needing more than that one coin spends it twice.
    #[test]
    fn a_parent_mined_mid_tick_is_not_spent_twice() {
        let node = FakeNode::default();
        node.tip.set(200);
        node.fund(&txid(7), 0, 200_000, 150);
        let mut state = fresh_state();
        state.begin_invocation(10_000_000, 100_000, None);
        let first = submitted(attempt(&node, &mut state, 100_000));
        assert_eq!(first.change_zat, 80_000);
        node.mine(std::slice::from_ref(&first.txid)); // not resolved yet
        let result = attempt_on(&node, &mut state, 100_000, Network::Regtest, 202);
        assert!(
            matches!(result, Err(EpochError::InsufficientFunds { .. })),
            "{result:?}"
        );
        assert_eq!(node.sent.borrow().len(), 1, "nothing else was sent");
    }

    /// Review finding 2, accounting: a recorded burn orphaned by a reorg
    /// is un-recorded and back in flight, so spent + held for burns in
    /// flight never drops (the budget stays safe); re-mined, it is
    /// recorded once. Burns buried past the watch depth are forgotten.
    #[test]
    fn a_reorged_burn_is_unrecorded_with_its_cost_still_held() {
        let node = funded_node();
        let mut state = fresh_state();
        state.begin_invocation(10_000_000, 100_000, Some(1_000_000));
        let first = submitted(attempt(&node, &mut state, 100_000));
        node.mine(std::slice::from_ref(&first.txid)); // 201
        resolve(&node, &mut state);
        assert_eq!(state.recent.len(), 1);
        let committed = state.total_spent_zat() + state.in_flight_cost_zat();
        assert_eq!(committed, 120_000);

        node.reorg(201);
        node.mine(&[]); // a new, empty 201
        let resolved = resolve(&node, &mut state);
        assert_eq!(
            resolved[0],
            PendingResolution::Reorged {
                txid: first.txid.clone(),
                height: 201
            }
        );
        assert!(state.epochs.is_empty());
        assert!(state.utxos.is_empty(), "its change is gone from the pool");
        assert_eq!(state.pending, vec![first.clone()]);
        assert_eq!(
            state.total_spent_zat() + state.in_flight_cost_zat(),
            committed
        );
        assert_eq!(node.mempool(), vec![first.txid.clone()], "re-sent");

        node.mine(&node.mempool()); // 202
        resolve(&node, &mut state);
        assert_eq!(state.epochs.len(), 1);
        assert_eq!(state.epochs[0].height, 202);
        assert_eq!(state.total_spent_zat(), 120_000);
        for _ in 0..REORG_WATCH_DEPTH {
            node.mine(&[]);
        }
        let second = submitted(attempt_on(
            &node,
            &mut state,
            100_000,
            Network::Regtest,
            213,
        ));
        node.mine(std::slice::from_ref(&second.txid));
        resolve(&node, &mut state);
        assert_eq!(
            state.recent.len(),
            1,
            "the burn at 202 is no longer watched"
        );
        assert_eq!(state.recent[0].burn.txid, second.txid);
    }

    // --- SIP-8: anchored burns ---

    /// An epoch with a fixed key (RFC 6979 signing: fixed bytes) and an
    /// optional reference, on [`funded_node`]; returns the node and the
    /// signed burn it was sent.
    fn fixed_epoch(sova_ref: Option<SovaRef>) -> (FakeNode, PendingBurn, String) {
        let node = funded_node();
        let mut state = fresh_state();
        state.begin_invocation(1_000_000, 100_000, None);
        let keypair = Keypair::from_secret_bytes([0x11; 32]).unwrap();
        let outcome = attempt_epoch(
            &node,
            &mut Funding::new(Network::Regtest),
            Network::Regtest,
            &keypair,
            [0x42; 20],
            0,
            100_000,
            201,
            None,
            sova_ref,
            &mut state,
            &mut |_| Ok(()),
        )
        .unwrap();
        let EpochOutcome::Submitted(pending) = outcome else {
            panic!("expected Submitted, got {outcome:?}");
        };
        let sent = node.sent.borrow()[0].clone();
        (node, pending, sent)
    }

    /// Captured from `attempt_epoch` on `release` before SIP-8 (deba358)
    /// with [`fixed_epoch`]'s inputs. The no-`--sova-rpc` path must send
    /// exactly this: SIP-8 changes nothing for a miner that doesn't ask.
    const PRE_SIP8_EPOCH_BURN: &str = "050000800a27a726b4d0d6c200000000f1000000010707070707070707070707070707070707070707070707070707070707070707000000006a4730440220486816b39d70037867fc625942330564f8dbc06520fcf687f167706427d47f29022067d63f830b8c80dcf59147dc32d6c0643bb192ca25f868d5593e30abcc50a3c60121034f355bdcb7cc0af728ef3cceb9615d90684bb5b2ca5f859ab0f0b704075871aaffffffff0300000000000000001d6a1b535601424242424242424242424242424242424242424200000000a0860100000000001976a914000000000000000000000000000000000000000088acc0c19600000000001976a914fc7250a211deddc70ee5a2738de5f07817351cef88ac000000";

    #[test]
    fn epoch_without_a_reference_is_byte_identical_to_pre_sip8() {
        let (_, pending, sent) = fixed_epoch(None);
        assert_eq!(sent, PRE_SIP8_EPOCH_BURN);
        assert_eq!(pending.fee_zat, 20_000);
    }

    fn sent_outputs(raw_hex: &str) -> Vec<(u64, Vec<u8>)> {
        let raw = hex::decode(raw_hex).unwrap();
        let tx = zcash_primitives::transaction::Transaction::read(
            raw.as_slice(),
            zcash_protocol::consensus::BranchId::Nu5,
        )
        .unwrap();
        tx.transparent_bundle()
            .unwrap()
            .vout
            .iter()
            .map(|o| (o.value().into_u64(), o.script_pubkey().0.0.clone()))
            .collect()
    }

    fn out_refs(outs: &[(u64, Vec<u8>)]) -> Vec<consensus::sip1::TxOutRef<'_>> {
        outs.iter()
            .map(|(v, s)| consensus::sip1::TxOutRef {
                value_zat: *v,
                script: s.as_slice(),
            })
            .collect()
    }

    /// A v2 epoch pays the SIP-8 fee (25,000 zat for 1-in/3-out), keeps
    /// the change at output 2, and round-trips through the node's
    /// recognizer with its reference; without SIP-8 it is not a burn.
    #[test]
    fn epoch_with_a_reference_sends_a_v2_burn() {
        use consensus::sip1::{Burn, extract_burn, extract_burn_at};
        let reference = SovaRef {
            height: 42,
            hash: [0x5e; 32],
        };
        let (_, pending, sent) = fixed_epoch(Some(reference));
        assert_eq!(pending.fee_zat, 25_000);
        assert_eq!(pending.change_zat, 10_000_000 - 125_000);
        let outs = sent_outputs(&sent);
        assert_eq!(outs.len(), 3);
        assert_eq!(outs[2].0, pending.change_zat, "change stays at vout 2");
        let burn = Burn {
            evm_address: [0x42; 20],
            signal_bits: 0,
            value_zat: 100_000,
        };
        assert_eq!(
            extract_burn_at(out_refs(&outs), true),
            Some((burn, Some(reference)))
        );
        assert_eq!(extract_burn_at(out_refs(&outs), false), None);
        assert_eq!(extract_burn(out_refs(&outs)), None);
    }

    #[test]
    fn v2_selection_pays_one_more_action() {
        let pool = vec![utxo(1_000_000)];
        let sel = select_utxos(&pool, 100_000, BurnPayloadVersion::V2).expect("should select");
        assert!(sel.has_change);
        assert_eq!(sel.fee_zat, 25_000);
        assert_eq!(sel.change_zat, 1_000_000 - 125_000);
        // Exactly burn + v2 fee: no change output, 20,000 formula fee, the
        // 5,000 left over folded in.
        let sel =
            select_utxos(&[utxo(125_000)], 100_000, BurnPayloadVersion::V2).expect("should select");
        assert!(!sel.has_change);
        assert_eq!(sel.fee_zat, 25_000);
        // A pool that covers a v1 burn and its fee exactly can't cover the v2 one.
        assert!(select_utxos(&[utxo(120_000)], 100_000, BurnPayloadVersion::V1).is_some());
        assert!(select_utxos(&[utxo(120_000)], 100_000, BurnPayloadVersion::V2).is_none());
    }
}
