//! One mining epoch: select UTXOs, build and sign a SIP-1 burn transaction
//! for exactly `per_epoch_zat`, and broadcast it -- or report that the
//! declared budget won't stretch to cover this epoch. Then
//! [`resolve_pending`] / [`wait_for_pending`] follow the broadcast burn
//! until it is mined (recording the epoch) or can no longer be.
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
//! broadcast (they are reserved by [`crate::state::PendingBurn`] until it
//! is mined or expires), and its change joins the pool once it confirms --
//! so consecutive epochs spend the previous burn's change without waiting
//! on coinbase maturity (100 confirmations) the way a *fresh* coinbase
//! output would.

use std::collections::BTreeSet;
use std::thread;
use std::time::{Duration, Instant};

use burn_wallet::tx::{BurnTxRequest, DEFAULT_TX_EXPIRY_DELTA, build_burn_transaction};
use burn_wallet::utxo::{decode_rpc_hash, encode_rpc_hash};
use burn_wallet::{Keypair, Network, RpcError, Utxo};
use zcash_transparent::bundle::OutPoint;

use crate::fee::{DUST_THRESHOLD_ZAT, zip317_fee_zat};
use crate::funding::{self, Funding};
use crate::node::{Node, TxStatus};
use crate::state::{EpochRecord, MinerState, OutPointRef, PendingBurn, TrackedUtxo};

/// How often [`wait_for_pending`] polls while waiting.
const CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_millis(500);
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
    /// A burn was built, signed, and accepted by the node; it is now
    /// `state.pending` (the caller saves state, then follows it with
    /// [`wait_for_pending`]).
    Submitted(PendingBurn),
    /// The wallet has funds, but spending them for this epoch would exceed
    /// a declared budget.
    BudgetExhausted {
        /// Which cap ([`BudgetKind::PerInvocation`]'s `--budget-zat` or
        /// [`BudgetKind::Lifetime`]'s `--lifetime-budget-zat`) ran out.
        kind: BudgetKind,
        /// Total zatoshis (burn + fee) this epoch would have cost.
        needed_zat: u64,
        /// Budget remaining, under `kind`'s cap, before this epoch.
        remaining_zat: u64,
    },
}

/// What became of the in-flight burn.
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
    /// It can no longer be mined (the tip reached its expiry height
    /// without it): dropped, nothing recorded, inputs free again.
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
/// therefore fee) from `pool` to cover `burn_zat` plus its ZIP-317 fee,
/// recomputing the fee as inputs are added since fee depends on input
/// count. Returns `None` if no prefix of `pool` (sorted descending) covers
/// it -- i.e. the wallet doesn't have enough total spendable value.
fn select_utxos(pool: &[TrackedUtxo], burn_zat: u64) -> Option<Selection> {
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
        let fee_with_change = zip317_fee_zat(n_in, true);
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
            let fee_no_change = zip317_fee_zat(n_in, false);
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

/// The outpoints reserved by the in-flight burn, if any.
pub(crate) fn reserved(state: &MinerState) -> BTreeSet<OutPointRef> {
    state
        .pending
        .iter()
        .flat_map(|p| p.spent.iter().cloned())
        .collect()
}

/// Re-reads the address's confirmed UTXOs, drops tracked entries the node
/// no longer lists, and -- only if the tracked pool can't cover `burn_zat`
/// -- merges in newly found spendable outputs. Returns the selection, or
/// [`EpochError::InsufficientFunds`] (or, where coinbase can't fund a burn
/// and some is waiting, [`EpochError::CoinbaseMustBeShielded`]).
fn fund_burn(
    node: &impl Node,
    funding: &mut Funding,
    address: &str,
    burn_zat: u64,
    state: &mut MinerState,
) -> Result<Selection, EpochError> {
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
    if let Some(selection) = select_utxos(&state.utxos, burn_zat) {
        return Ok(selection);
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
            "funding: {added} new spendable UTXO(s) for {address} via getaddressutxos at tip {} ({spendable_zat} zat spendable, {held_back}, {} zat reserved by our in-flight burn)",
            snapshot.tip_height, found.reserved_zat
        );
    }
    select_utxos(&state.utxos, burn_zat).ok_or(if found.coinbase_zat > 0 {
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
/// sign and broadcast a burn. On success the burn becomes `state.pending`
/// (its inputs leave `state.utxos`); the caller saves state and follows it
/// with [`wait_for_pending`]. Must not be called while a burn is already in
/// flight -- at most one is, so its inputs and our unconfirmed change are
/// never double-spent.
///
/// `target_height` is only a *target*, used for the transaction's
/// consensus branch id / expiry -- not what gets recorded: the epoch's
/// height is the block the burn actually confirms in (see
/// [`resolve_pending`]).
///
/// Budget is checked against two independent caps (D5, see
/// [`BudgetKind`]): `--budget-zat`, measured against spend since this
/// `mine` invocation started, and the optional `--lifetime-budget-zat`,
/// measured against everything this keystore has ever spent. Either one
/// running short of this epoch's cost produces
/// [`EpochOutcome::BudgetExhausted`] before any funds move.
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
    state: &mut MinerState,
) -> Result<EpochOutcome, EpochError> {
    debug_assert!(state.pending.is_none(), "one burn in flight at a time");
    let address = keypair.encode_address(network);
    let selection = fund_burn(node, funding, &address, burn_zat, state)?;

    let total_cost_zat = burn_zat.saturating_add(selection.fee_zat);

    // Two independent caps (D5): `--budget-zat` (this invocation only) is
    // checked first since it's the primary, always-declared flag; the
    // optional `--lifetime-budget-zat` is checked second as an additional
    // ceiling across every invocation ever run against this keystore. A
    // generous per-invocation budget does not override a tighter lifetime
    // cap, and vice versa -- either one exhausting stops this epoch.
    let remaining_zat = state.budget_remaining_zat();
    if total_cost_zat > remaining_zat {
        return Ok(EpochOutcome::BudgetExhausted {
            kind: BudgetKind::PerInvocation,
            needed_zat: total_cost_zat,
            remaining_zat,
        });
    }
    if let Some(lifetime_remaining_zat) = state.lifetime_budget_remaining_zat()
        && total_cost_zat > lifetime_remaining_zat
    {
        return Ok(EpochOutcome::BudgetExhausted {
            kind: BudgetKind::Lifetime,
            needed_zat: total_cost_zat,
            remaining_zat: lifetime_remaining_zat,
        });
    }

    let inputs: Vec<TrackedUtxo> = selection
        .indices
        .iter()
        .map(|&i| state.utxos[i].clone())
        .collect();
    let utxos: Vec<Utxo> = inputs
        .iter()
        .map(|t| {
            Ok(Utxo {
                outpoint: OutPoint::new(decode_rpc_hash("txid", &t.txid)?, t.vout),
                value_zat: t.value_zat,
            })
        })
        .collect::<Result<_, burn_wallet::utxo::UtxoError>>()?;

    let request = BurnTxRequest {
        network,
        target_height,
        utxos,
        change_and_signing_key: *keypair,
        evm_address,
        signal_bits,
        burn_value_zat: burn_zat,
        fee_zat: selection.fee_zat,
    };
    let built = build_burn_transaction(&request)?;
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
        expiry_height: u64::from(target_height) + u64::from(DEFAULT_TX_EXPIRY_DELTA),
    };

    match broadcast(node, &pending.raw_hex, &pending.txid, BROADCAST_ATTEMPTS) {
        Broadcast::Sent => {}
        // Fail safe: the node may have it. Track it as in flight (inputs
        // reserved) rather than build a second burn from the same inputs;
        // `resolve_pending` re-sends it on each new block until it is
        // mined or expires.
        Broadcast::Ambiguous(e) => eprintln!(
            "warning: burn {}: broadcast outcome unknown ({e}); tracking it as in flight",
            pending.txid
        ),
        Broadcast::Rejected(message) => {
            return Err(EpochError::Rejected {
                txid: pending.txid,
                message,
            });
        }
    }

    // Broadcast: its inputs leave the pool now (reserved by `pending`
    // until it is mined or dead); its change joins once it confirms.
    let spent: BTreeSet<&OutPointRef> = pending.spent.iter().collect();
    state.utxos.retain(|t| {
        !spent.contains(&OutPointRef {
            txid: t.txid.clone(),
            vout: t.vout,
        })
    });
    state.pending = Some(pending.clone());
    Ok(EpochOutcome::Submitted(pending))
}

/// One look at the in-flight burn, if any (`None` if there is none):
/// mined -> record the epoch at its real confirming height and track its
/// change; unknown with the tip at or past its expiry height -> drop it
/// (its inputs are free again: the node still lists them, so the next
/// funding pass picks them back up); otherwise it is still in flight.
///
/// # Errors
///
/// Returns the [`RpcError`] if the node can't be asked.
pub(crate) fn resolve_pending(
    node: &impl Node,
    state: &mut MinerState,
) -> Result<Option<PendingResolution>, RpcError> {
    let Some(pending) = state.pending.clone() else {
        return Ok(None);
    };
    match node.tx_status(&pending.txid)? {
        TxStatus::Confirmed { height } => {
            state.pending = None;
            if pending.change_zat > 0 {
                state.utxos.push(TrackedUtxo {
                    txid: pending.txid.clone(),
                    vout: 2, // payload=0, eater=1, change=2 -- see build_burn_transaction.
                    value_zat: pending.change_zat,
                });
            }
            let record = EpochRecord {
                epoch: u64::try_from(state.epochs.len()).unwrap_or(u64::MAX) + 1,
                height,
                burn_zat: pending.burn_zat,
                fee_zat: pending.fee_zat,
                change_zat: pending.change_zat,
                txid: pending.txid,
            };
            state.record_epoch(record.clone());
            Ok(Some(PendingResolution::Confirmed(record)))
        }
        TxStatus::Mempool => Ok(Some(PendingResolution::InFlight { txid: pending.txid })),
        TxStatus::Unknown => {
            let tip = node.tip_height()?;
            if tip >= pending.expiry_height {
                state.pending = None;
                Ok(Some(PendingResolution::Dropped {
                    txid: pending.txid,
                    reason: format!(
                        "not mined by its expiry height {} (tip {tip}); its inputs are free again",
                        pending.expiry_height
                    ),
                }))
            } else {
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
                Ok(Some(PendingResolution::InFlight { txid: pending.txid }))
            }
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

/// Follows the in-flight burn with [`resolve_pending`] until it is mined or
/// dropped, or `timeout` elapses (then it is still
/// [`PendingResolution::InFlight`]: the caller keeps it reserved and looks
/// again on later blocks -- a slow confirmation is not an error). A
/// transient RPC failure while polling is logged and retried. `None` if
/// nothing is in flight.
pub(crate) fn wait_for_pending(
    node: &impl Node,
    state: &mut MinerState,
    timeout: Duration,
) -> Option<PendingResolution> {
    let deadline = Instant::now() + timeout;
    loop {
        match resolve_pending(node, state) {
            Ok(Some(PendingResolution::InFlight { txid })) => {
                if Instant::now() >= deadline {
                    return Some(PendingResolution::InFlight { txid });
                }
            }
            Ok(done) => return done,
            Err(e) => {
                let txid = state.pending.as_ref().map(|p| p.txid.clone())?;
                eprintln!(
                    "warning: checking burn {txid} failed while awaiting confirmation: {e}; retrying"
                );
                if Instant::now() >= deadline {
                    return Some(PendingResolution::InFlight { txid });
                }
            }
        }
        thread::sleep(CONFIRMATION_POLL_INTERVAL);
    }
}

#[cfg(test)]
// Test code: an unexpected `None` here is a test failure, and `.expect()`
// with a message is more useful here than threading `Result` through.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
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
        let sel = select_utxos(&pool, 100_000).expect("should select");
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
        let sel = select_utxos(&pool, 100_000).expect("should select");
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
        let sel = select_utxos(&pool, 100_000).expect("should select");
        assert_eq!(sel.indices, vec![1]);
    }

    #[test]
    fn combines_multiple_utxos_when_needed() {
        let pool = vec![utxo(70_000), utxo(70_000)];
        let sel = select_utxos(&pool, 100_000).expect("should select");
        assert_eq!(sel.indices.len(), 2);
        assert!(sel.has_change);
        // 2 inputs still leaves the output side (4 actions) dominant over
        // the input side (2 actions): same 20,000 zat as the single-input
        // case above.
        assert_eq!(sel.fee_zat, zip317_fee_zat(2, true));
        assert_eq!(sel.change_zat, 140_000 - 100_000 - 20_000);
    }

    #[test]
    fn returns_none_when_wallet_is_empty() {
        let pool: Vec<TrackedUtxo> = vec![];
        assert!(select_utxos(&pool, 100_000).is_none());
    }

    #[test]
    fn returns_none_when_funds_are_insufficient() {
        let pool = vec![utxo(1_000)];
        assert!(select_utxos(&pool, 100_000).is_none());
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
        attempt_epoch(
            node,
            &mut Funding::new(network),
            network,
            &keypair,
            [0u8; 20],
            0,
            burn_zat,
            target_height,
            state,
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
            } => {
                assert_eq!(kind, BudgetKind::PerInvocation);
                assert_eq!(needed_zat, 120_000);
                assert_eq!(remaining_zat, 50_000);
            }
            other => panic!("expected BudgetExhausted, got {other:?}"),
        }
        assert!(node.sent.borrow().is_empty(), "nothing may be broadcast");
        assert!(state.pending.is_none());
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
            } => {
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
        assert_eq!(state.pending.as_ref(), Some(&pending));
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
        state.pending = Some(p.clone());
        p
    }

    #[test]
    fn mined_pending_burn_is_recorded_at_its_real_height_with_its_change() {
        let node = funded_node();
        let mut state = fresh_state();
        let p = in_flight(&mut state, 241);
        node.txs
            .borrow_mut()
            .insert(p.txid.clone(), TxStatus::Confirmed { height: 203 });

        let Some(PendingResolution::Confirmed(record)) =
            resolve_pending(&node, &mut state).unwrap()
        else {
            panic!("expected Confirmed");
        };
        assert_eq!(record.epoch, 1);
        assert_eq!(record.height, 203);
        assert_eq!(record.txid, p.txid);
        assert!(state.pending.is_none());
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
            resolve_pending(&node, &mut state).unwrap(),
            Some(PendingResolution::InFlight {
                txid: p.txid.clone()
            })
        );
        // Not (yet) known to the node, but the chain hasn't reached its
        // expiry height: it may still be mined. Keep the inputs reserved.
        node.txs.borrow_mut().remove(&p.txid);
        assert!(matches!(
            resolve_pending(&node, &mut state).unwrap(),
            Some(PendingResolution::InFlight { .. })
        ));
        assert!(state.pending.is_some());
        assert!(state.epochs.is_empty());
    }

    #[test]
    fn expired_pending_burn_is_dropped_and_its_inputs_are_funding_again() {
        let node = funded_node(); // tip 200; txid(7):0 still unspent on chain
        let mut state = fresh_state();
        in_flight(&mut state, 200);

        assert!(matches!(
            resolve_pending(&node, &mut state).unwrap(),
            Some(PendingResolution::Dropped { .. })
        ));
        assert!(state.pending.is_none());
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
    fn nothing_pending_resolves_to_none() {
        let node = funded_node();
        let mut state = fresh_state();
        assert_eq!(resolve_pending(&node, &mut state).unwrap(), None);
        assert_eq!(
            wait_for_pending(&node, &mut state, Duration::from_millis(1)),
            None
        );
    }

    #[test]
    fn wait_gives_up_as_in_flight_not_as_an_error() {
        let node = funded_node();
        let mut state = fresh_state();
        let p = in_flight(&mut state, 241);
        node.txs
            .borrow_mut()
            .insert(p.txid.clone(), TxStatus::Mempool);
        assert_eq!(
            wait_for_pending(&node, &mut state, Duration::from_millis(1)),
            Some(PendingResolution::InFlight { txid: p.txid })
        );
        assert!(state.pending.is_some());
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
        assert_eq!(state.pending.as_ref(), Some(&pending));
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
        assert!(state.pending.is_some());
    }

    /// Re-sending a burn the node already has: "already exists in mempool"
    /// and "committed to the best chain" are success, never a rejection.
    #[test]
    fn retry_answered_already_known_is_success() {
        let (node, state, _) = submit_with(vec![]);
        let pending = state.pending.clone().unwrap();
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
        assert!(state.pending.is_none());
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
        assert!(state.pending.is_some());
        assert!(state.utxos.is_empty());
    }

    /// A pending burn the node lost (restart, eviction) is re-sent verbatim
    /// on the next look, while it can still be mined.
    #[test]
    fn unknown_pending_burn_is_resent_before_expiry() {
        let (node, mut state, _) = submit_with(vec![]);
        let pending = state.pending.clone().unwrap();
        node.txs.borrow_mut().remove(&pending.txid); // the node forgot it
        assert_eq!(
            resolve_pending(&node, &mut state).unwrap(),
            Some(PendingResolution::InFlight {
                txid: pending.txid.clone()
            })
        );
        assert_eq!(node.sent.borrow().len(), 2);
        assert_eq!(txid_of(&node.sent.borrow()[1]), Some(pending.txid.clone()));
        assert_eq!(
            node.txs.borrow().get(&pending.txid),
            Some(&TxStatus::Mempool)
        );
    }
}
