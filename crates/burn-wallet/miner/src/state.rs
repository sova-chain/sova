//! Persisted miner state: a minimal JSON sidecar written next to the
//! keystore file, tracking budget/spend history and the miner's own
//! locally-known spendable UTXOs (including its own change, so consecutive
//! epochs don't need to wait out coinbase maturity or re-scan the chain).
//!
//! This is deliberately *not* a general-purpose wallet database: it only
//! tracks what `sova-miner` itself created or was told about, in service of
//! `report` and of chaining change across epochs.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Current on-disk state format version.
const STATE_VERSION: u8 = 1;

/// One transparent UTXO the miner believes is spendable: funding discovered
/// with `getaddressutxos` (coinbase or an ordinary transfer -- see
/// `crate::funding`), or change from one of the miner's own confirmed burns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TrackedUtxo {
    /// The outpoint's txid, in RPC-display (byte-reversed) hex -- the same
    /// form `sendrawtransaction`/`getblock` use, via
    /// [`burn_wallet::utxo::encode_rpc_hash`].
    pub txid: String,
    /// Output index within that transaction.
    pub vout: u32,
    /// The output's value, in zatoshis.
    pub value_zat: u64,
}

/// A transparent outpoint (txid in RPC-display hex, output index).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct OutPointRef {
    /// The funding transaction's txid, RPC-display hex.
    pub txid: String,
    /// Output index within that transaction.
    pub vout: u32,
}

/// A burn that was broadcast but is not yet known to be mined. Saved to
/// `state.json` right after the broadcast, so a restart neither loses the
/// epoch nor spends its inputs a second time: they stay reserved until the
/// burn is mined, or can no longer be (the chain passed its expiry height).
///
/// Up to `crate::epoch::MAX_IN_FLIGHT` are in flight at once, and a later
/// one may spend an earlier one's unconfirmed change (see
/// [`MinerState::pending`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PendingBurn {
    /// The burn's txid (computed locally from the signed tx), RPC-display
    /// hex.
    pub txid: String,
    /// The signed transaction, hex, so it can be re-broadcast verbatim.
    pub raw_hex: String,
    /// The inputs it spends -- reserved while it is in flight.
    pub spent: Vec<OutPointRef>,
    /// The amount burned, in zatoshis.
    pub burn_zat: u64,
    /// The fee paid (including any folded sub-dust change), in zatoshis.
    pub fee_zat: u64,
    /// Change back to our address at output index 2 (0 if none).
    pub change_zat: u64,
    /// The last height it can be mined at; once the tip reaches it
    /// unmined, the burn is dead and its inputs are free again.
    pub expiry_height: u64,
}

impl PendingBurn {
    /// What this burn costs once mined: the burn plus its fee.
    #[must_use]
    pub(crate) fn cost_zat(&self) -> u64 {
        self.burn_zat.saturating_add(self.fee_zat)
    }

    /// Its change output (always output 2, see
    /// `burn_wallet::tx::build_burn_transaction`), if it has one.
    #[must_use]
    pub(crate) fn change_outpoint(&self) -> Option<OutPointRef> {
        (self.change_zat > 0).then(|| OutPointRef {
            txid: self.txid.clone(),
            vout: 2,
        })
    }
}

/// One completed mining epoch: one new Zcash block observed, with a burn
/// submitted against it (while budget remained).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EpochRecord {
    /// Sequential epoch counter (1-based), scoped to the Zcash chain the
    /// state is anchored to (it restarts at 1 when a chain is retired --
    /// see [`MinerState::retire_chain`]) -- not the same thing as the
    /// block height.
    pub epoch: u64,
    /// The Zcash block height the burn actually confirmed in -- read back
    /// from the node after submission (see
    /// `crate::epoch::wait_for_confirmation`), *not* the tip height that
    /// triggered the epoch. Since this process only submits in reaction to
    /// a block someone else already mined, confirmation always lands at
    /// least one block later than the trigger.
    pub height: u64,
    /// The amount burned to the SIP-1 eater script, in zatoshis.
    pub burn_zat: u64,
    /// The ZIP-317 fee paid, in zatoshis (see [`crate::fee`]). When the
    /// change output an exact-change transaction would have produced was
    /// below the dust threshold, the leftover is folded into this figure
    /// rather than creating a dust output -- so this is always the true
    /// total fee cost of the transaction, not just the formula value.
    pub fee_zat: u64,
    /// Change returned to the miner's own address (0 if none was created).
    pub change_zat: u64,
    /// The submitted transaction's txid, in RPC-display hex.
    pub txid: String,
}

/// The full persisted state sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MinerState {
    /// State format version.
    pub version: u8,
    /// This miner's transparent (t-addr) funding address.
    pub address: String,
    /// The Sova EVM address credited by every burn this miner submits, hex
    /// encoded (no `0x` prefix), set at `init` time: by default the
    /// keystore key's own Ethereum address
    /// ([`crate::evm_address::derive_evm_address`]). State written before
    /// that default existed may hold the unspendable legacy hash160 one;
    /// `init` keeps whatever is recorded unless told otherwise (see
    /// [`crate::evm_address`]).
    pub evm_address_hex: String,
    /// The `--budget-zat` value most recently passed to `mine`. Budget is
    /// **per-invocation** (D5): each run declares its own budget, and
    /// [`Self::budget_remaining_zat`] measures spend only *since this
    /// invocation started* (see [`Self::invocation_start_spent_zat`] and
    /// [`Self::begin_invocation`]) -- not the full lifetime total. This
    /// field simply reflects the most recent declaration, so `report`
    /// between runs still shows something meaningful.
    pub budget_zat: u64,
    /// The `--per-epoch-zat` value most recently passed to `mine`.
    pub per_epoch_zat: u64,
    /// Sum of every epoch's `burn_zat`, across the lifetime of this
    /// keystore (every `mine` invocation ever run against it) -- see
    /// [`Self::total_spent_zat`]. `report` always shows this lifetime
    /// figure regardless of per-invocation budget semantics.
    pub total_burned_zat: u64,
    /// Sum of every epoch's `fee_zat`, lifetime (see
    /// [`Self::total_burned_zat`]).
    pub total_fee_zat: u64,
    /// Full per-epoch history, in order.
    pub epochs: Vec<EpochRecord>,
    /// The miner's current locally-tracked spendable UTXO pool (our own
    /// change chains through here; see the module docs).
    pub utxos: Vec<TrackedUtxo>,
    /// Snapshot of [`Self::total_spent_zat`] taken when the most recent
    /// `mine` invocation started (see [`Self::begin_invocation`]).
    /// [`Self::budget_remaining_zat`] subtracts this from the current
    /// lifetime total so `--budget-zat` measures only what *this run* has
    /// spent, not everything ever spent by this keystore. Additive field
    /// (D5): absent in state.json written before this change, which
    /// deserializes it as `0` -- the correct value for a keystore with no
    /// prior invocation snapshot.
    #[serde(default)]
    pub invocation_start_spent_zat: u64,
    /// The `--lifetime-budget-zat` value most recently passed to `mine`,
    /// if any (D5). An optional *additional* cap, checked against the full
    /// lifetime [`Self::total_spent_zat`] regardless of per-invocation
    /// spend -- the old (pre-D5) behavior, opt-in for anyone who wants a
    /// hard ceiling across every run against this keystore. `None` (the
    /// default) means no lifetime cap is enforced. Like `budget_zat`, this
    /// is declared fresh by each `mine` invocation (not sticky): omitting
    /// `--lifetime-budget-zat` on a run clears any previously-declared cap
    /// for that run, rather than silently carrying one forward. Additive
    /// field: absent in pre-D5 state.json, which deserializes it as
    /// `None`.
    #[serde(default)]
    pub lifetime_budget_zat: Option<u64>,
    /// The Zcash chain that [`Self::utxos`] and [`Self::epochs`] belong to
    /// (E1d): the block hash at a fixed low height, recorded the first time
    /// `mine` talks to a node. `mine` compares it against the node on
    /// startup and whenever the tip moves (see `crate::chain`); a mismatch
    /// means the chain this state was built on is gone (a regtest reset, or
    /// `--rpc` pointed at a different network), and the chain-derived state
    /// is retired into [`Self::retired_chains`]. Additive field: absent in
    /// state.json written before E1d, which deserializes it as `None`
    /// ("never anchored"; the tracked-txid check in `crate::chain` still
    /// applies).
    #[serde(default)]
    pub chain_anchor: Option<ChainAnchor>,
    /// Chain-derived state set aside because its chain disappeared from
    /// the node (E1d), oldest first. Kept rather than deleted so nothing is
    /// silently lost (`report` counts it, and the outpoints are still here
    /// if a node ever serves that chain again). Not used for mining.
    /// Additive field.
    #[serde(default)]
    pub retired_chains: Vec<RetiredChain>,
    /// Our burns that were broadcast but not yet seen mined, oldest
    /// first (see [`PendingBurn`]): at most
    /// `crate::epoch::MAX_IN_FLIGHT`. Their inputs are already removed
    /// from [`Self::utxos`]; a burn's change is added once it confirms,
    /// unless a later burn in flight already spends it (chaining: see
    /// `crate::epoch`). Oldest first is also parent-before-child, the
    /// order a re-broadcast must follow.
    ///
    /// On disk (see [`pending_serde`]): `null` for none, the burn itself
    /// for one -- the shape every release since the field was added reads
    /// -- and an array only for two or more, which an older release
    /// refuses to load rather than silently ignore.
    #[serde(default, with = "pending_serde")]
    pub pending: Vec<PendingBurn>,
    /// Our burns confirmed in the last `crate::epoch::REORG_WATCH_DEPTH`
    /// blocks, with their signed bytes: a Zcash reorg can orphan one after
    /// its epoch was recorded, and zebrad doesn't return a reorged-out tx
    /// to its mempool. `crate::epoch::resolve_pending` checks these each
    /// tip and moves one the chain no longer has back into
    /// [`Self::pending`] (un-recording its epoch), so it is re-sent before
    /// any child spending its change. Additive field, omitted when empty;
    /// older releases ignore it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent: Vec<RecentBurn>,
}

/// A burn confirmed recently, kept in case a reorg orphans it (see
/// [`MinerState::recent`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RecentBurn {
    /// The burn as it was in flight (signed bytes included).
    pub burn: PendingBurn,
    /// The height it confirmed at.
    pub height: u64,
}

/// (De)serializes the in-flight burns: `null` / one object / an array of
/// two or more, reading any of the three (see [`MinerState::pending`]).
mod pending_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::PendingBurn;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        Many(Vec<PendingBurn>),
        One(Box<PendingBurn>),
    }

    pub(super) fn serialize<S: Serializer>(
        pending: &[PendingBurn],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match pending {
            [] => serializer.serialize_none(),
            [one] => one.serialize(serializer),
            many => many.serialize(serializer),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<PendingBurn>, D::Error> {
        Ok(match Option::<OneOrMany>::deserialize(deserializer)? {
            None => Vec::new(),
            Some(OneOrMany::One(one)) => vec![*one],
            Some(OneOrMany::Many(many)) => many,
        })
    }
}

/// A chain identity: the hash of the block at `height`, in RPC-display hex
/// (as `getblockhash` returns it). A block hash commits to every ancestor,
/// so a match at `height` means the node's chain is the same chain up to
/// there. See `crate::chain` for which height is used and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChainAnchor {
    /// The anchored block height.
    pub height: u64,
    /// The block hash at `height`, RPC-display hex.
    pub hash: String,
}

/// Chain-derived state retired because its chain disappeared from the
/// node -- see [`MinerState::retire_chain`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RetiredChain {
    /// The anchor that chain was recorded under (`None` for state written
    /// before E1d that was found stale before it was ever anchored).
    pub anchor: Option<ChainAnchor>,
    /// Why it was retired, human-readable.
    pub reason: String,
    /// That chain's epoch history (burns are still counted in the lifetime
    /// totals -- see [`MinerState::retire_chain`]).
    pub epochs: Vec<EpochRecord>,
    /// The UTXOs that were tracked on that chain when it was retired.
    pub utxos: Vec<TrackedUtxo>,
    /// The burns that were in flight on that chain. Additive field.
    #[serde(default, with = "pending_serde")]
    pub pending: Vec<PendingBurn>,
}

/// Errors loading or saving miner state.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StateError {
    /// Underlying file I/O failed.
    #[error("state I/O error at {path}: {source}")]
    Io {
        /// The path being read or written.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The state file was not well-formed JSON in the expected shape.
    #[error("malformed state JSON at {path}: {source}")]
    Json {
        /// The path being read.
        path: PathBuf,
        /// The underlying (de)serialization error.
        #[source]
        source: serde_json::Error,
    },
}

impl MinerState {
    /// Constructs fresh, empty state for a newly-initialized keystore.
    #[must_use]
    pub(crate) fn new(address: String, evm_address_hex: String) -> Self {
        Self {
            version: STATE_VERSION,
            address,
            evm_address_hex,
            budget_zat: 0,
            per_epoch_zat: 0,
            total_burned_zat: 0,
            total_fee_zat: 0,
            epochs: Vec::new(),
            utxos: Vec::new(),
            invocation_start_spent_zat: 0,
            lifetime_budget_zat: None,
            chain_anchor: None,
            retired_chains: Vec::new(),
            pending: Vec::new(),
            recent: Vec::new(),
        }
    }

    /// Sum of `burn_zat` over [`Self::epochs`] -- the burns on the chain
    /// this state is currently anchored to, which is what an on-chain scan
    /// of that chain can find. Differs from the lifetime
    /// [`Self::total_burned_zat`] once any chain has been retired.
    #[must_use]
    pub(crate) fn chain_burned_zat(&self) -> u64 {
        self.epochs
            .iter()
            .fold(0u64, |acc, e| acc.saturating_add(e.burn_zat))
    }

    /// The chain this state was built on is gone (E1d): set its
    /// chain-derived state -- the UTXO pool, the burns in flight, and the
    /// epoch history, and with
    /// it the per-chain epoch counter, which is `epochs.len() + 1` -- aside
    /// in [`Self::retired_chains`], and re-anchor to `new_anchor`.
    ///
    /// Kept as-is: the keystore (identity) and every budget field. The
    /// lifetime totals ([`Self::total_burned_zat`]/[`Self::total_fee_zat`])
    /// still include the retired chain's spend: they back
    /// `--lifetime-budget-zat`, a safety ceiling, and a ceiling that forgot
    /// spend whenever a node changed would under-count -- the conservative
    /// direction is to keep it. `invocation_start_spent_zat` is left alone
    /// too, so a mid-run reset doesn't hand the current invocation a fresh
    /// `--budget-zat`.
    pub(crate) fn retire_chain(&mut self, reason: String, new_anchor: ChainAnchor) {
        self.retired_chains.push(RetiredChain {
            anchor: self.chain_anchor.take(),
            reason,
            epochs: std::mem::take(&mut self.epochs),
            utxos: std::mem::take(&mut self.utxos),
            pending: std::mem::take(&mut self.pending),
        });
        self.recent.clear();
        self.chain_anchor = Some(new_anchor);
    }

    /// Loads state from `path`.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] if the file cannot be read or is not
    /// well-formed JSON in the expected shape.
    pub(crate) fn load(path: &Path) -> Result<Self, StateError> {
        let contents = fs::read_to_string(path).map_err(|source| StateError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&contents).map_err(|source| StateError::Json {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Writes state to `path`, overwriting any existing file. Called after
    /// every state-changing step (not just at process exit) so the sidecar
    /// is always resumable and `report` run concurrently or after an abrupt
    /// exit reflects true progress.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] if the file cannot be written.
    pub(crate) fn save(&self, path: &Path) -> Result<(), StateError> {
        let json = serde_json::to_string_pretty(self).map_err(|source| StateError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        // Atomic replace (write a sibling temp file, then rename over the
        // real one): a crash mid-write must not leave a truncated
        // state.json, now that a save also records an in-flight burn.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)
            .and_then(|()| fs::rename(&tmp, path))
            .map_err(|source| StateError::Io {
                path: path.to_path_buf(),
                source,
            })
    }

    /// Takes back the epoch of `txid` (its burn was orphaned by a reorg):
    /// removes its record, subtracts it from the lifetime totals and
    /// renumbers the epochs after it. The caller puts the burn back in
    /// flight, so what it will cost is still held against both budgets.
    /// Returns the removed record, if there was one.
    pub(crate) fn unrecord_epoch(&mut self, txid: &str) -> Option<EpochRecord> {
        let i = self.epochs.iter().position(|e| e.txid == txid)?;
        let record = self.epochs.remove(i);
        self.total_burned_zat = self.total_burned_zat.saturating_sub(record.burn_zat);
        self.total_fee_zat = self.total_fee_zat.saturating_sub(record.fee_zat);
        for (n, e) in self.epochs.iter_mut().enumerate().skip(i) {
            e.epoch = u64::try_from(n).unwrap_or(u64::MAX) + 1;
        }
        Some(record)
    }

    /// Records a completed epoch and updates the running (lifetime)
    /// totals.
    pub(crate) fn record_epoch(&mut self, record: EpochRecord) {
        self.total_burned_zat = self.total_burned_zat.saturating_add(record.burn_zat);
        self.total_fee_zat = self.total_fee_zat.saturating_add(record.fee_zat);
        self.epochs.push(record);
    }

    /// Total spent, lifetime (burned + fees, across every epoch this
    /// keystore has ever recorded) -- the figure `report` always shows and
    /// [`Self::lifetime_budget_remaining_zat`] is measured against.
    /// [`Self::budget_remaining_zat`] is measured against a *subset* of
    /// this (see its own doc comment), not this figure directly.
    #[must_use]
    pub(crate) fn total_spent_zat(&self) -> u64 {
        self.total_burned_zat.saturating_add(self.total_fee_zat)
    }

    /// Starts a new `mine` invocation (D5 per-invocation budget
    /// semantics): declares this run's `--budget-zat` and
    /// `--per-epoch-zat`, snapshots the current lifetime spend into
    /// [`Self::invocation_start_spent_zat`] so [`Self::budget_remaining_zat`]
    /// measures only what happens from here on, and declares (or clears,
    /// if `None`) the optional `--lifetime-budget-zat` cap. Does not save
    /// to disk -- callers persist state as needed. Call this once, at the
    /// start of `mine`, before any epochs are attempted in that run.
    pub(crate) fn begin_invocation(
        &mut self,
        budget_zat: u64,
        per_epoch_zat: u64,
        lifetime_budget_zat: Option<u64>,
    ) {
        self.budget_zat = budget_zat;
        self.per_epoch_zat = per_epoch_zat;
        self.invocation_start_spent_zat = self.total_spent_zat();
        self.lifetime_budget_zat = lifetime_budget_zat;
    }

    /// Remaining budget for the **current invocation only** (D5): the most
    /// recently declared `--budget-zat`, minus whatever has been spent
    /// *since [`Self::begin_invocation`] was last called* -- not the full
    /// lifetime spend. A fresh invocation (even one started right after a
    /// prior run exhausted its own budget) gets its full declared budget
    /// back, because [`Self::invocation_start_spent_zat`] snapshots the
    /// lifetime total at that moment and this subtracts it back out.
    #[must_use]
    pub(crate) fn budget_remaining_zat(&self) -> u64 {
        let spent_this_invocation = self
            .total_spent_zat()
            .saturating_sub(self.invocation_start_spent_zat);
        self.budget_zat.saturating_sub(spent_this_invocation)
    }

    /// Remaining budget under the optional `--lifetime-budget-zat` cap, if
    /// one is currently declared (`None` if not -- see
    /// [`Self::lifetime_budget_zat`]). Unlike [`Self::budget_remaining_zat`],
    /// this is checked against the full lifetime [`Self::total_spent_zat`],
    /// exactly the pre-D5 behavior, so it still enforces a hard ceiling
    /// across every `mine` invocation ever run against this keystore.
    #[must_use]
    pub(crate) fn lifetime_budget_remaining_zat(&self) -> Option<u64> {
        self.lifetime_budget_zat
            .map(|cap| cap.saturating_sub(self.total_spent_zat()))
    }

    /// What the burns in flight will cost once mined (burns plus fees).
    /// Not spent yet -- a burn is charged when it confirms -- but already
    /// committed: both budget checks hold it back (see
    /// `crate::epoch::attempt_epoch`), so their confirming can't push a
    /// cap past what was checked when the last one was sent.
    #[must_use]
    pub(crate) fn in_flight_cost_zat(&self) -> u64 {
        self.pending
            .iter()
            .fold(0u64, |acc, p| acc.saturating_add(p.cost_zat()))
    }
}

#[cfg(test)]
// Test code: an unexpected `Err` here is a test failure.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample_epoch(epoch: u64, burn: u64, fee: u64) -> EpochRecord {
        EpochRecord {
            epoch,
            height: 100 + epoch,
            burn_zat: burn,
            fee_zat: fee,
            change_zat: 0,
            txid: format!("{epoch:064x}"),
        }
    }

    #[test]
    fn records_epochs_and_tracks_totals() {
        let mut state = MinerState::new("tmAddr".to_string(), "ab".repeat(20));
        state.budget_zat = 1_000_000;
        state.record_epoch(sample_epoch(1, 100_000, 15_000));
        state.record_epoch(sample_epoch(2, 100_000, 15_000));

        assert_eq!(state.total_burned_zat, 200_000);
        assert_eq!(state.total_fee_zat, 30_000);
        assert_eq!(state.total_spent_zat(), 230_000);
        assert_eq!(state.budget_remaining_zat(), 1_000_000 - 230_000);
        assert_eq!(state.epochs.len(), 2);
    }

    #[test]
    fn budget_remaining_saturates_at_zero() {
        let mut state = MinerState::new("tmAddr".to_string(), "ab".repeat(20));
        state.budget_zat = 10_000;
        state.record_epoch(sample_epoch(1, 100_000, 15_000));
        assert_eq!(state.budget_remaining_zat(), 0);
    }

    #[test]
    fn save_and_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let mut state = MinerState::new("tmAddr".to_string(), "cd".repeat(20));
        state.budget_zat = 500_000;
        state.per_epoch_zat = 100_000;
        state.record_epoch(sample_epoch(1, 100_000, 15_000));
        state.utxos.push(TrackedUtxo {
            txid: "ef".repeat(32),
            vout: 1,
            value_zat: 42,
        });
        state.chain_anchor = Some(ChainAnchor {
            height: 1,
            hash: "01".repeat(32),
        });
        state.save(&path).unwrap();

        let loaded = MinerState::load(&path).unwrap();
        assert_eq!(loaded.address, "tmAddr");
        assert_eq!(loaded.budget_zat, 500_000);
        assert_eq!(loaded.epochs.len(), 1);
        assert_eq!(loaded.utxos.len(), 1);
        assert_eq!(loaded.utxos[0].value_zat, 42);
        assert_eq!(loaded.chain_anchor, state.chain_anchor);
        assert!(loaded.retired_chains.is_empty());
        assert!(loaded.pending.is_empty());
    }

    fn sample_pending() -> PendingBurn {
        PendingBurn {
            txid: "b1".repeat(32),
            raw_hex: "0500".to_string(),
            spent: vec![OutPointRef {
                txid: "ef".repeat(32),
                vout: 1,
            }],
            burn_zat: 10_000,
            fee_zat: 20_000,
            change_zat: 970_000,
            expiry_height: 241,
        }
    }

    /// A second burn, spending the first one's unconfirmed change.
    fn sample_child() -> PendingBurn {
        PendingBurn {
            txid: "b2".repeat(32),
            raw_hex: "0501".to_string(),
            spent: vec![OutPointRef {
                txid: "b1".repeat(32),
                vout: 2,
            }],
            burn_zat: 10_000,
            fee_zat: 20_000,
            change_zat: 940_000,
            expiry_height: 242,
        }
    }

    /// The in-flight burn survives a save/load (it is saved right after
    /// the broadcast, so a restart finds it), and the write leaves no temp
    /// file behind.
    #[test]
    fn pending_burn_roundtrips_and_save_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = MinerState::new("tmAddr".to_string(), "cd".repeat(20));
        state.pending = vec![sample_pending()];
        state.save(&path).unwrap();
        state.save(&path).unwrap();
        assert_eq!(MinerState::load(&path).unwrap().pending, state.pending);
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("state.json")]);
    }

    /// E1d: a chain reset retires the in-flight burn with the rest of the
    /// chain-derived state -- its inputs are on the dead chain.
    #[test]
    fn retire_chain_takes_the_pending_burn() {
        let mut state = MinerState::new("tmAddr".to_string(), "cd".repeat(20));
        state.pending = vec![sample_pending(), sample_child()];
        state.retire_chain(
            "test".to_string(),
            ChainAnchor {
                height: 1,
                hash: "02".repeat(32),
            },
        );
        assert!(state.pending.is_empty());
        assert_eq!(
            state.retired_chains[0].pending,
            vec![sample_pending(), sample_child()]
        );
    }

    /// Two burns in flight survive a restart in order (parent first), and
    /// their cost is held against the budget until they resolve.
    #[test]
    fn two_burns_in_flight_roundtrip_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = MinerState::new("tmAddr".to_string(), "cd".repeat(20));
        state.pending = vec![sample_pending(), sample_child()];
        state.save(&path).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(json["pending"].is_array(), "two or more: an array");
        let loaded = MinerState::load(&path).unwrap();
        assert_eq!(loaded.pending, state.pending);
        assert_eq!(loaded.in_flight_cost_zat(), 60_000);
        assert_eq!(
            loaded.pending[0].change_outpoint(),
            Some(loaded.pending[1].spent[0].clone())
        );
    }

    /// Downgrade safety: with none or one burn in flight, `pending` keeps
    /// the shape older releases read (`null` / one object); both shapes,
    /// as older releases wrote them, still load.
    #[test]
    fn zero_or_one_burn_in_flight_keeps_the_old_on_disk_shape() {
        let mut state = MinerState::new("tmAddr".to_string(), "cd".repeat(20));
        let json = serde_json::to_value(&state).unwrap();
        assert!(json["pending"].is_null());
        state.pending = vec![sample_pending()];
        let json = serde_json::to_value(&state).unwrap();
        assert!(json["pending"].is_object());
        assert_eq!(json["pending"]["txid"], "b1".repeat(32));

        let old_one: MinerState = serde_json::from_value(json).unwrap();
        assert_eq!(old_one.pending, vec![sample_pending()]);
        let mut old_none = serde_json::to_value(&state).unwrap();
        old_none["pending"] = serde_json::Value::Null;
        let old_none: MinerState = serde_json::from_value(old_none).unwrap();
        assert!(old_none.pending.is_empty());
    }

    /// D5: a fresh `mine` invocation gets its *full* declared budget back,
    /// even immediately after a prior invocation already spent heavily
    /// against this same keystore's lifetime totals -- the core
    /// per-invocation semantics this task pins. Before D5,
    /// `budget_remaining_zat` was checked against the lifetime total
    /// directly, so the second `begin_invocation` below would have started
    /// with only 30,000 zat remaining (150,000 - 120,000 already spent),
    /// not the full 150,000 -- this is exactly the surprise the sim
    /// harness hit (see box/sim/README.md's "script bug found and fixed").
    #[test]
    fn fresh_invocation_can_spend_full_budget_after_prior_lifetime_spend() {
        let mut state = MinerState::new("tmAddr".to_string(), "ab".repeat(20));

        // Run 1: declares a 200,000 zat budget, spends 120,000 of it.
        state.begin_invocation(200_000, 100_000, None);
        state.record_epoch(sample_epoch(1, 100_000, 20_000));
        assert_eq!(state.total_spent_zat(), 120_000);
        assert_eq!(state.budget_remaining_zat(), 200_000 - 120_000);

        // Run 2 (a fresh `mine` invocation, same keystore/state.json):
        // declares its own 150,000 zat budget. Per-invocation semantics
        // mean this run sees the *full* 150,000 available, not
        // 150,000 minus run 1's already-spent 120,000.
        state.begin_invocation(150_000, 100_000, None);
        assert_eq!(state.budget_remaining_zat(), 150_000);

        // And it can actually spend that full amount: a 120,000 zat epoch
        // (well within run 1's already-exhausted lifetime headroom) still
        // fits comfortably in run 2's fresh budget.
        state.record_epoch(sample_epoch(2, 100_000, 20_000));
        assert_eq!(state.total_spent_zat(), 240_000); // lifetime, across both runs
        assert_eq!(state.budget_remaining_zat(), 150_000 - 120_000); // this run only
    }

    /// D5: with no `--lifetime-budget-zat` declared, there is no lifetime
    /// cap -- only the per-invocation `--budget-zat` gates spend.
    #[test]
    fn lifetime_budget_remaining_is_none_when_not_set() {
        let mut state = MinerState::new("tmAddr".to_string(), "ab".repeat(20));
        state.begin_invocation(200_000, 100_000, None);
        assert_eq!(state.lifetime_budget_remaining_zat(), None);
        state.record_epoch(sample_epoch(1, 100_000, 20_000));
        assert_eq!(state.lifetime_budget_remaining_zat(), None);
    }

    /// D5: `--lifetime-budget-zat`, when set, is checked against the full
    /// lifetime spend and keeps shrinking across invocations even while
    /// each invocation's own `--budget-zat` is generously re-upped -- the
    /// explicit opt-in for the old (pre-D5) "checks against everything
    /// ever spent" behavior.
    #[test]
    fn lifetime_budget_cap_tracks_cumulative_spend_across_invocations() {
        let mut state = MinerState::new("tmAddr".to_string(), "ab".repeat(20));

        state.begin_invocation(200_000, 100_000, Some(300_000));
        state.record_epoch(sample_epoch(1, 100_000, 20_000)); // lifetime: 120,000
        assert_eq!(
            state.lifetime_budget_remaining_zat(),
            Some(300_000 - 120_000)
        );

        // A second invocation re-declares a large fresh per-invocation
        // budget (plenty of per-run headroom) and the same lifetime cap.
        state.begin_invocation(1_000_000, 100_000, Some(300_000));
        assert_eq!(state.budget_remaining_zat(), 1_000_000); // fresh, per-invocation
        state.record_epoch(sample_epoch(2, 100_000, 20_000)); // lifetime: 240,000
        assert_eq!(
            state.lifetime_budget_remaining_zat(),
            Some(300_000 - 240_000)
        );

        // A third epoch's cost (120,000) would fit easily inside the huge
        // per-invocation budget, but not inside the 60,000 zat left under
        // the lifetime cap -- proving the lifetime cap is a real
        // *additional* constraint, not superseded by a generous
        // per-invocation budget.
        let lifetime_remaining = state.lifetime_budget_remaining_zat().unwrap();
        assert_eq!(lifetime_remaining, 60_000);
        assert!(120_000 > lifetime_remaining);
    }

    /// Backward compatibility (D5): state.json written before this change
    /// lacks `invocation_start_spent_zat`/`lifetime_budget_zat` entirely.
    /// Loading it must still succeed, defaulting the new fields to their
    /// "no prior invocation, no lifetime cap" values rather than erroring.
    #[test]
    fn loads_pre_d5_state_json_missing_new_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let pre_d5_json = r#"{
            "version": 1,
            "address": "tmAddr",
            "evm_address_hex": "ab0000000000000000000000000000000000ab",
            "budget_zat": 500000,
            "per_epoch_zat": 100000,
            "total_burned_zat": 100000,
            "total_fee_zat": 20000,
            "epochs": [],
            "utxos": []
        }"#;
        fs::write(&path, pre_d5_json).unwrap();

        let loaded = MinerState::load(&path).unwrap();
        assert_eq!(loaded.budget_zat, 500_000);
        assert_eq!(loaded.total_burned_zat, 100_000);
        assert_eq!(loaded.invocation_start_spent_zat, 0);
        assert_eq!(loaded.lifetime_budget_zat, None);
        // E1d fields are additive too: never anchored, nothing retired.
        assert_eq!(loaded.chain_anchor, None);
        assert!(loaded.retired_chains.is_empty());
        // Nothing in flight either (field added with getaddressutxos
        // funding).
        assert!(loaded.pending.is_empty());
        // A fresh invocation on this loaded state behaves exactly as if it
        // always had these fields: full budget available, no lifetime cap.
        let mut loaded = loaded;
        loaded.begin_invocation(500_000, 100_000, None);
        assert_eq!(loaded.budget_remaining_zat(), 500_000);
    }
}
