//! The drip logic: startup network checks, the wallet view (UTXOs minus
//! in-flight drips and coinbase it can't spend), coin selection with a
//! ZIP-317 fee, and the order of checks every drip goes through.
//!
//! Coinbase: on testnet (and mainnet, which the faucet refuses anyway)
//! Zcash consensus only lets transparent coinbase be spent into shielded
//! outputs, and a drip always has transparent outputs, so coinbase paid to
//! the faucet is never selected there; it shows as "coinbase (must be
//! shielded first)" until the operator shields it and sends it back as an
//! ordinary transfer. On regtest (Zebra's default
//! `should_allow_unshielded_coinbase_spends = true`) mature coinbase funds
//! drips as before.

use std::collections::HashMap;
use std::path::PathBuf;

use burn_wallet::fee::{P2PKH_STANDARD_OUTPUT_SIZE, P2SH_OUTPUT_SIZE, transparent_fee_zat};
use burn_wallet::rpc::AddressUtxo;
use burn_wallet::tx::{DEFAULT_TX_EXPIRY_DELTA, build_transfer_transaction};
use burn_wallet::utxo::{COINBASE_MATURITY, decode_rpc_hash, encode_rpc_hash};
use burn_wallet::{Keypair, Network, RpcError, TransferTxRequest, Utxo};
use serde::Serialize;
use zcash_transparent::address::TransparentAddress;
use zcash_transparent::bundle::OutPoint;

use crate::address::{AddressError, validate_recipient};
use crate::config::FaucetConfig;
use crate::node::{MAINNET_GENESIS, Node, TESTNET_GENESIS, TxStatus};
use crate::state::{
    DAY_SECS, FaucetState, LimitRejection, Limits, OutPointRef, PendingDrip, StateError,
};

/// Change below this is folded into the fee instead of becoming a dust
/// output (the same threshold `sova-miner` uses: SIP-1's minimum burn).
const DUST_THRESHOLD_ZAT: u64 = consensus::sip1::MIN_BURN_ZAT;

/// At most this many inputs per drip, so a faucet funded with many tiny
/// outputs can't build an oversized transaction.
const MAX_INPUTS: usize = 50;

/// Minimum seconds between two max-balance warnings in the log.
const BALANCE_WARNING_INTERVAL_SECS: u64 = 600;

/// Why the faucet refuses to start.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StartError {
    #[error(
        "REFUSING TO START: zebrad at the configured RPC is on Zcash MAINNET ({0}). sova-faucet only ever runs against testnet or regtest."
    )]
    Mainnet(String),
    #[error("REFUSING TO START: {0}")]
    WrongNetwork(String),
    #[error("cannot reach zebrad: {0}")]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    State(#[from] StateError),
}

/// Why one drip failed. `Display` is safe to show users.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DripError {
    #[error("{0}")]
    InvalidAddress(#[from] AddressError),
    #[error("{0}")]
    Limit(#[from] LimitRejection),
    #[error("all faucet funds are in flight; try again after the next block")]
    Busy,
    #[error("the faucet's funds are newly mined and still maturing; try again later")]
    Maturing,
    #[error(
        "the faucet's funds are newly mined coinbase that must be shielded before they can be dripped; the operator has been alerted"
    )]
    CoinbaseMustBeShielded,
    #[error("the faucet is empty")]
    InsufficientFunds,
    #[error("the faucet is paused; the operator has been alerted")]
    Halted,
    #[error("zcash node error")]
    Node(#[source] RpcError),
    #[error("the zcash node rejected the transaction")]
    Rejected(String),
    #[error("internal error building the transaction")]
    Build(#[from] burn_wallet::BurnTxError),
}

/// A successful drip.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DripReceipt {
    pub txid: String,
    pub address: String,
    pub amount_zat: u64,
    pub fee_zat: u64,
}

/// The public `/status` document.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatusReport {
    pub network: String,
    pub faucet_address: String,
    pub tip_height: u64,
    /// Confirmed, mature, not reserved by an in-flight drip.
    pub balance_zat: u64,
    /// Coinbase outputs still short of 100 confirmations (regtest).
    pub immature_zat: u64,
    /// Coinbase (must be shielded first): transparent coinbase that can
    /// never fund a drip on this network until the operator shields it
    /// and sends it back as an ordinary transfer (testnet; always 0 on
    /// regtest).
    pub coinbase_unshielded_zat: u64,
    /// Inputs of drips broadcast but not yet mined (their change comes
    /// back once they are).
    pub in_flight_zat: u64,
    pub in_flight_drips: usize,
    pub drip_zat: u64,
    pub daily_cap_zat: u64,
    pub spent_today_zat: u64,
    pub remaining_today_zat: u64,
    pub resets_in_secs: u64,
    pub address_cooldown_secs: u64,
    pub ip_cooldown_secs: u64,
    pub max_balance_zat: u64,
    /// True when the hot wallet holds more than `max_balance_zat`.
    pub over_max_balance: bool,
    pub accepting_drips: bool,
}

/// The wallet as the node sees it, minus what in-flight drips reserve.
#[derive(Debug, Default)]
struct WalletView {
    tip: u64,
    spendable: Vec<AddressUtxo>,
    spendable_zat: u64,
    immature_zat: u64,
    /// Coinbase this network won't let a drip spend (must be shielded).
    coinbase_zat: u64,
    reserved_zat: u64,
}

impl WalletView {
    /// Everything the faucet key holds.
    fn total_zat(&self) -> u64 {
        self.spendable_zat + self.immature_zat + self.coinbase_zat + self.reserved_zat
    }
}

/// A chosen set of inputs.
#[derive(Debug, PartialEq, Eq)]
struct Selection {
    utxos: Vec<AddressUtxo>,
    fee_zat: u64,
}

/// Checks, before the faucet key is ever used, that `node` is on the
/// configured network -- and above all not on mainnet. Two independent
/// signals must both say "not mainnet": `getblockchaininfo.chain` and the
/// genesis block hash.
pub(crate) fn preflight(node: &impl Node, network: Network) -> Result<(), StartError> {
    let chain = node.chain()?;
    let genesis = node.block_hash(0)?;
    if chain == "main" {
        return Err(StartError::Mainnet(format!(
            "getblockchaininfo.chain = {chain:?}"
        )));
    }
    if genesis == MAINNET_GENESIS {
        return Err(StartError::Mainnet(format!(
            "genesis block {genesis} is mainnet's"
        )));
    }
    if chain != "test" {
        return Err(StartError::WrongNetwork(format!(
            "zebrad reports chain {chain:?}; expected \"test\""
        )));
    }
    match network {
        Network::Test if genesis != TESTNET_GENESIS => Err(StartError::WrongNetwork(format!(
            "config says network = \"test\" but zebrad's genesis block {genesis} is not Zcash testnet's (is it a regtest node?)"
        ))),
        Network::Regtest if genesis == TESTNET_GENESIS => Err(StartError::WrongNetwork(
            "config says network = \"regtest\" but zebrad is on Zcash testnet".to_string(),
        )),
        Network::Main => Err(StartError::Mainnet("configured network".to_string())),
        _ => Ok(()),
    }
}

/// Serialized size of the payment output to `to`.
fn output_size(to: &TransparentAddress) -> u64 {
    match to {
        TransparentAddress::PublicKeyHash(_) => P2PKH_STANDARD_OUTPUT_SIZE,
        TransparentAddress::ScriptHash(_) => P2SH_OUTPUT_SIZE,
    }
}

/// Largest-first selection covering `amount_zat` plus its ZIP-317 fee.
/// Sub-dust change is folded into the fee rather than created.
fn select(pool: &[AddressUtxo], amount_zat: u64, recipient_size: u64) -> Option<Selection> {
    let mut sorted: Vec<&AddressUtxo> = pool.iter().collect();
    sorted.sort_unstable_by_key(|u| std::cmp::Reverse(u.satoshis));
    let mut chosen = Vec::new();
    let mut sum: u64 = 0;
    for u in sorted.into_iter().take(MAX_INPUTS) {
        chosen.push(u.clone());
        sum = sum.saturating_add(u.satoshis);
        let n = u64::try_from(chosen.len()).unwrap_or(u64::MAX);
        let fee_with_change = transparent_fee_zat(n, &[recipient_size, P2PKH_STANDARD_OUTPUT_SIZE]);
        let Some(change) = sum.checked_sub(amount_zat.saturating_add(fee_with_change)) else {
            continue;
        };
        if change >= DUST_THRESHOLD_ZAT {
            return Some(Selection {
                utxos: chosen,
                fee_zat: fee_with_change,
            });
        }
        let fee_no_change = transparent_fee_zat(n, &[recipient_size]);
        if sum >= amount_zat.saturating_add(fee_no_change) {
            return Some(Selection {
                utxos: chosen,
                fee_zat: sum - amount_zat,
            });
        }
    }
    None
}

/// The faucet: config, its one hot key, its state, and a node.
pub(crate) struct Faucet<N: Node> {
    cfg: FaucetConfig,
    network: Network,
    keypair: Keypair,
    address: String,
    node: N,
    state: FaucetState,
    state_path: PathBuf,
    /// Set when the state file couldn't be written after a broadcast: the
    /// cooldowns on disk are stale, so no more drips until a restart.
    halted: bool,
    coinbase_cache: HashMap<String, bool>,
    last_balance_warning: Option<u64>,
    last_coinbase_warning: Option<u64>,
}

impl<N: Node> Faucet<N> {
    /// Runs [`preflight`], then loads (or creates) the state file.
    pub(crate) fn start(
        cfg: FaucetConfig,
        keypair: Keypair,
        node: N,
        now: u64,
    ) -> Result<Self, StartError> {
        let network = cfg.network();
        preflight(&node, network)?;
        let address = keypair.encode_address(network);
        let state_path = cfg.state_file.clone();
        let state = FaucetState::load_or_new(&state_path, &address, now)?;
        Ok(Self {
            cfg,
            network,
            keypair,
            address,
            node,
            state,
            state_path,
            halted: false,
            coinbase_cache: HashMap::new(),
            last_balance_warning: None,
            last_coinbase_warning: None,
        })
    }

    pub(crate) fn address(&self) -> &str {
        &self.address
    }

    fn limits(&self) -> Limits {
        Limits {
            address_cooldown_secs: self.cfg.address_cooldown_secs,
            ip_cooldown_secs: self.cfg.ip_cooldown_secs,
            daily_cap_zat: self.cfg.daily_cap_zat,
        }
    }

    fn save_state(&mut self, now: u64) -> Result<(), StateError> {
        let limits = self.limits();
        self.state.prune(&limits, now);
        self.state.save(&self.state_path)
    }

    fn is_coinbase(&mut self, txid: &str) -> Result<bool, RpcError> {
        if let Some(known) = self.coinbase_cache.get(txid) {
            return Ok(*known);
        }
        let is = self.node.is_coinbase(txid)?;
        self.coinbase_cache.insert(txid.to_string(), is);
        Ok(is)
    }

    /// Drops in-flight drips that were mined, or that expired unmined (so
    /// their inputs are spendable again), then builds the wallet view.
    fn wallet(&mut self, now: u64) -> Result<WalletView, RpcError> {
        let tip = self.node.tip_height()?;
        let before = self.state.pending.len();
        let mut keep = Vec::with_capacity(before);
        for p in &self.state.pending {
            keep.push(match self.node.tx_status(&p.txid)? {
                TxStatus::Confirmed => false,
                TxStatus::Unknown if tip >= p.expiry_height => {
                    eprintln!(
                        "drip {} expired unmined at height {tip}; its inputs are free again",
                        p.txid
                    );
                    false
                }
                TxStatus::Unknown | TxStatus::Mempool => true,
            });
        }
        let mut keep = keep.into_iter();
        self.state.pending.retain(|_| keep.next().unwrap_or(true));
        if self.state.pending.len() != before
            && let Err(e) = self.save_state(now)
        {
            eprintln!("warning: could not save state after reconciling drips: {e}");
        }

        let reserved = self.state.reserved();
        let mut view = WalletView {
            tip,
            ..WalletView::default()
        };
        for u in self.node.address_utxos(&self.address)? {
            let outpoint = OutPointRef {
                txid: u.txid.clone(),
                vout: u.output_index,
            };
            if reserved.contains(&outpoint) {
                view.reserved_zat += u.satoshis;
                continue;
            }
            if !self.network.allows_unshielded_coinbase_spends() {
                // Testnet: coinbase can't fund a drip at any age.
                if self.is_coinbase(&u.txid)? {
                    view.coinbase_zat += u.satoshis;
                    continue;
                }
            } else {
                let confirmations = tip.saturating_sub(u.height) + 1;
                if confirmations < u64::from(COINBASE_MATURITY) && self.is_coinbase(&u.txid)? {
                    view.immature_zat += u.satoshis;
                    continue;
                }
            }
            view.spendable_zat += u.satoshis;
            view.spendable.push(u);
        }
        Ok(view)
    }

    /// Logs a loud warning (rate-limited) when the hot wallet holds more
    /// than the configured multiple of the daily cap. Returns whether it
    /// does.
    fn balance_guard(&mut self, total_zat: u64, now: u64) -> bool {
        let max = self.cfg.max_balance_zat();
        let over = total_zat > max;
        let due = self
            .last_balance_warning
            .is_none_or(|t| now.saturating_sub(t) >= BALANCE_WARNING_INTERVAL_SECS);
        if over && due {
            self.last_balance_warning = Some(now);
            eprintln!(
                "WARNING: HOT WALLET OVER LIMIT: the faucet key holds {total_zat} zat, more than max_balance_multiple ({}) x daily_cap_zat ({}) = {max} zat. This is a hot key on a server; move the excess to cold storage.",
                self.cfg.max_balance_multiple, self.cfg.daily_cap_zat
            );
        }
        over
    }

    /// Logs (rate-limited) the operator's fix when drips fail because the
    /// only funds left are coinbase that must be shielded first.
    fn coinbase_warning(&mut self, coinbase_zat: u64, now: u64) {
        let due = self
            .last_coinbase_warning
            .is_none_or(|t| now.saturating_sub(t) >= BALANCE_WARNING_INTERVAL_SECS);
        if due {
            self.last_coinbase_warning = Some(now);
            eprintln!(
                "WARNING: FAUCET CAN'T DRIP: {}",
                burn_wallet::utxo::coinbase_must_be_shielded_message(coinbase_zat, "drip")
            );
        }
    }

    /// Current status (also runs the balance guard).
    pub(crate) fn status(&mut self, now: u64) -> Result<StatusReport, RpcError> {
        let view = self.wallet(now)?;
        let over_max_balance = self.balance_guard(view.total_zat(), now);
        self.state.roll_day(now);
        let limits = self.limits();
        Ok(StatusReport {
            network: self.cfg.network.clone(),
            faucet_address: self.address.clone(),
            tip_height: view.tip,
            balance_zat: view.spendable_zat,
            immature_zat: view.immature_zat,
            coinbase_unshielded_zat: view.coinbase_zat,
            in_flight_zat: view.reserved_zat,
            in_flight_drips: self.state.pending.len(),
            drip_zat: self.cfg.drip_zat,
            daily_cap_zat: self.cfg.daily_cap_zat,
            spent_today_zat: self.state.spent_today_zat,
            remaining_today_zat: self.state.remaining_today_zat(&limits),
            resets_in_secs: DAY_SECS - now % DAY_SECS,
            address_cooldown_secs: self.cfg.address_cooldown_secs,
            ip_cooldown_secs: self.cfg.ip_cooldown_secs,
            max_balance_zat: self.cfg.max_balance_zat(),
            over_max_balance,
            accepting_drips: !self.halted,
        })
    }

    /// One drip request: validate the address, check cooldowns and the
    /// daily cap, select coins, build, broadcast, record. Nothing is
    /// recorded unless the transaction was (or may have been) broadcast.
    pub(crate) fn drip(
        &mut self,
        address_input: &str,
        ip_key: &str,
        now: u64,
    ) -> Result<DripReceipt, DripError> {
        if self.halted {
            return Err(DripError::Halted);
        }
        let recipient = validate_recipient(
            address_input,
            self.network,
            &self.keypair.transparent_address(),
        )?;
        let limits = self.limits();
        let amount = self.cfg.drip_zat;
        // Cheap checks first (no node calls); the cap is re-checked with the
        // real fee below.
        self.state
            .check(&limits, &recipient.canonical, ip_key, amount, now)?;

        let view = self.wallet(now).map_err(DripError::Node)?;
        self.balance_guard(view.total_zat(), now);
        let Some(selection) = select(&view.spendable, amount, output_size(&recipient.address))
        else {
            let need = amount + transparent_fee_zat(1, &[P2PKH_STANDARD_OUTPUT_SIZE; 2]);
            let unconfirmed = view.spendable_zat + view.reserved_zat;
            return Err(if unconfirmed >= need {
                DripError::Busy
            } else if unconfirmed + view.immature_zat >= need {
                DripError::Maturing
            } else if view.coinbase_zat > 0 {
                self.coinbase_warning(view.coinbase_zat, now);
                DripError::CoinbaseMustBeShielded
            } else {
                DripError::InsufficientFunds
            });
        };
        self.state.check(
            &limits,
            &recipient.canonical,
            ip_key,
            amount + selection.fee_zat,
            now,
        )?;

        let mut utxos = Vec::with_capacity(selection.utxos.len());
        for u in &selection.utxos {
            let hash = decode_rpc_hash("txid", &u.txid)
                .map_err(|e| DripError::Rejected(format!("bad txid from node: {e}")))?;
            utxos.push(Utxo {
                outpoint: OutPoint::new(hash, u.output_index),
                value_zat: u.satoshis,
            });
        }
        let target_height = u32::try_from(view.tip + 1).unwrap_or(u32::MAX);
        let built = build_transfer_transaction(&TransferTxRequest {
            network: self.network,
            target_height,
            utxos,
            change_and_signing_key: self.keypair,
            recipient: recipient.address,
            amount_zat: amount,
            fee_zat: selection.fee_zat,
        })?;
        let txid = encode_rpc_hash(built.txid);

        let broadcast = match self.node.send_raw(&hex::encode(&built.raw)) {
            Ok(node_txid) => {
                if node_txid != txid {
                    eprintln!("warning: node returned txid {node_txid}, we computed {txid}");
                }
                Ok(())
            }
            // An RPC-level error is not proof nothing was sent: zebrad can
            // answer e.g. "channel closed" and still admit the tx (seen on
            // the box, where sova-miner's retry then hit "already exists in
            // mempool"). Ask the node before calling it a rejection.
            Err(RpcError::RpcFailure { message, .. }) => match self.node.tx_status(&txid) {
                Ok(TxStatus::Mempool | TxStatus::Confirmed) => {
                    eprintln!(
                        "drip {txid}: zebrad answered {message:?} but has the tx; treating as sent"
                    );
                    Ok(())
                }
                Ok(TxStatus::Unknown) => {
                    eprintln!(
                        "drip to {} rejected by zebrad: {message}",
                        recipient.canonical
                    );
                    return Err(DripError::Rejected(message));
                }
                Err(e) => Err(e),
            },
            // Transport failure: the tx may or may not have reached the
            // node. Fail safe: record it as sent (cooldowns, budget, and
            // inputs reserved until it expires).
            Err(e) => Err(e),
        };

        let drip = PendingDrip {
            txid: txid.clone(),
            recipient: recipient.canonical.clone(),
            amount_zat: amount,
            fee_zat: selection.fee_zat,
            spent: selection
                .utxos
                .iter()
                .map(|u| OutPointRef {
                    txid: u.txid.clone(),
                    vout: u.output_index,
                })
                .collect(),
            expiry_height: u64::from(target_height) + u64::from(DEFAULT_TX_EXPIRY_DELTA),
            sent_at: now,
        };
        self.state.record(ip_key, drip, now);
        if let Err(e) = self.save_state(now) {
            self.halted = true;
            eprintln!(
                "ERROR: drip {txid} was broadcast but the state file could not be written ({e}); refusing further drips until restart"
            );
        }
        match broadcast {
            Ok(()) => {
                println!(
                    "drip {txid}: {amount} zat to {} (fee {} zat, {} input(s))",
                    recipient.canonical,
                    selection.fee_zat,
                    selection.utxos.len()
                );
                Ok(DripReceipt {
                    txid,
                    address: recipient.canonical,
                    amount_zat: amount,
                    fee_zat: selection.fee_zat,
                })
            }
            Err(e) => {
                eprintln!("drip {txid}: broadcast outcome unknown ({e}); recorded as sent");
                Err(DripError::Node(e))
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};

    use zcash_primitives::transaction::Transaction;
    use zcash_protocol::consensus::{BlockHeight, BranchId};

    use super::*;
    use crate::config::FaucetConfig;

    const NOON: u64 = 1_790_078_400;
    const REGTEST_GENESIS: &str =
        "029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327";

    #[derive(Default)]
    struct FakeNode {
        chain: String,
        genesis: String,
        tip: RefCell<u64>,
        utxos: RefCell<Vec<AddressUtxo>>,
        coinbase: BTreeSet<String>,
        mempool: RefCell<BTreeSet<String>>,
        mined: RefCell<BTreeSet<String>>,
        sent: RefCell<Vec<Vec<u8>>>,
        reject_with: Option<String>,
        /// Answer `reject_with` but admit the tx anyway (zebrad's
        /// "channel closed" case).
        admit_despite_error: bool,
    }

    impl FakeNode {
        fn regtest() -> Self {
            Self {
                chain: "test".into(),
                genesis: REGTEST_GENESIS.into(),
                tip: RefCell::new(200),
                ..Self::default()
            }
        }
        fn fund(&self, n: u8, satoshis: u64, height: u64) -> String {
            let txid = format!("{n:02x}").repeat(32);
            self.utxos.borrow_mut().push(AddressUtxo {
                address: String::new(),
                txid: txid.clone(),
                output_index: 0,
                satoshis,
                height,
            });
            txid
        }
        /// Mines every mempool tx: spent UTXOs vanish (the fake doesn't add
        /// change back; tests that need it fund again).
        fn mine_block(&self, spent: &[OutPointRef]) {
            *self.tip.borrow_mut() += 1;
            let pool = std::mem::take(&mut *self.mempool.borrow_mut());
            self.mined.borrow_mut().extend(pool);
            self.utxos.borrow_mut().retain(|u| {
                !spent
                    .iter()
                    .any(|s| s.txid == u.txid && s.vout == u.output_index)
            });
        }
    }

    impl Node for FakeNode {
        fn chain(&self) -> Result<String, RpcError> {
            Ok(self.chain.clone())
        }
        fn block_hash(&self, _height: u64) -> Result<String, RpcError> {
            Ok(self.genesis.clone())
        }
        fn tip_height(&self) -> Result<u64, RpcError> {
            Ok(*self.tip.borrow())
        }
        fn address_utxos(&self, _address: &str) -> Result<Vec<AddressUtxo>, RpcError> {
            Ok(self.utxos.borrow().clone())
        }
        fn tx_status(&self, txid: &str) -> Result<TxStatus, RpcError> {
            Ok(if self.mined.borrow().contains(txid) {
                TxStatus::Confirmed
            } else if self.mempool.borrow().contains(txid) {
                TxStatus::Mempool
            } else {
                TxStatus::Unknown
            })
        }
        fn is_coinbase(&self, txid: &str) -> Result<bool, RpcError> {
            Ok(self.coinbase.contains(txid))
        }
        fn send_raw(&self, raw_hex: &str) -> Result<String, RpcError> {
            let raw = hex::decode(raw_hex).unwrap();
            let tx = Transaction::read(raw.as_slice(), BranchId::Nu5).unwrap();
            let txid = encode_rpc_hash(tx.txid().into());
            if let Some(message) = &self.reject_with {
                if self.admit_despite_error {
                    self.mempool.borrow_mut().insert(txid);
                    self.sent.borrow_mut().push(raw);
                }
                return Err(RpcError::RpcFailure {
                    method: "sendrawtransaction".into(),
                    code: -25,
                    message: message.clone(),
                });
            }
            self.mempool.borrow_mut().insert(txid.clone());
            self.sent.borrow_mut().push(raw);
            Ok(txid)
        }
    }

    fn config(dir: &std::path::Path, extra: &str) -> FaucetConfig {
        config_for("regtest", dir, extra)
    }

    fn config_for(network: &str, dir: &std::path::Path, extra: &str) -> FaucetConfig {
        FaucetConfig::parse(&format!(
            r#"
network = "{network}"
zebrad_rpc = "http://127.0.0.1:1"
keystore = "{0}/k.json"
state_file = "{0}/state.json"
drip_zat = 10000000
daily_cap_zat = 30100000
address_cooldown_secs = 3600
ip_cooldown_secs = 600
{extra}
"#,
            dir.display()
        ))
        .unwrap()
    }

    fn user() -> String {
        Keypair::generate().encode_address(Network::Regtest)
    }

    fn faucet(node: FakeNode, dir: &std::path::Path) -> Faucet<FakeNode> {
        Faucet::start(config(dir, ""), Keypair::generate(), node, NOON).unwrap()
    }

    /// A testnet node at a post-NU5 tip, and a faucet configured for it.
    fn testnet_faucet(mut node: FakeNode, dir: &std::path::Path) -> Faucet<FakeNode> {
        node.genesis = TESTNET_GENESIS.into();
        *node.tip.borrow_mut() = TESTNET_TIP;
        Faucet::start(config_for("test", dir, ""), Keypair::generate(), node, NOON).unwrap()
    }

    const TESTNET_TIP: u64 = 3_300_000;

    /// Testnet: coinbase never funds a drip, however old; the requester
    /// is told the operator must shield it, and `/status` shows it apart
    /// from the spendable balance.
    #[test]
    fn testnet_coinbase_is_never_dripped() {
        let dir = tempfile::tempdir().unwrap();
        let mut node = FakeNode::regtest();
        let cb = node.fund(1, 125_000_000, 3_000_000); // 300k confirmations
        node.coinbase.insert(cb);
        let mut f = testnet_faucet(node, dir.path());
        let err = f.drip(&user(), "1.1.1.1", NOON).unwrap_err();
        assert!(matches!(err, DripError::CoinbaseMustBeShielded), "{err}");
        assert!(err.to_string().contains("must be shielded"));
        assert!(f.node.sent.borrow().is_empty());
        let s = f.status(NOON).unwrap();
        assert_eq!(s.balance_zat, 0);
        assert_eq!(s.immature_zat, 0);
        assert_eq!(s.coinbase_unshielded_zat, 125_000_000);
    }

    /// Testnet: an ordinary transfer beside coinbase funds the drip, and
    /// the drip spends only it.
    #[test]
    fn testnet_drip_spends_only_non_coinbase() {
        let dir = tempfile::tempdir().unwrap();
        let mut node = FakeNode::regtest();
        let cb = node.fund(1, 900_000_000, 3_000_000);
        node.coinbase.insert(cb);
        let transfer = node.fund(2, 50_000_000, TESTNET_TIP - 1);
        let mut f = testnet_faucet(node, dir.path());
        f.drip(&user(), "1.1.1.1", NOON).unwrap();
        assert_eq!(f.state.pending.len(), 1);
        assert_eq!(
            f.state.pending[0].spent,
            vec![OutPointRef {
                txid: transfer,
                vout: 0
            }]
        );
        let s = f.status(NOON).unwrap();
        assert_eq!(s.coinbase_unshielded_zat, 900_000_000);
    }

    #[test]
    fn preflight_refuses_mainnet_by_chain_name() {
        let node = FakeNode {
            chain: "main".into(),
            ..FakeNode::regtest()
        };
        let err = preflight(&node, Network::Test).unwrap_err();
        assert!(matches!(err, StartError::Mainnet(_)), "{err}");
    }

    #[test]
    fn preflight_refuses_mainnet_by_genesis_even_if_chain_says_test() {
        let node = FakeNode {
            genesis: MAINNET_GENESIS.into(),
            ..FakeNode::regtest()
        };
        let err = preflight(&node, Network::Regtest).unwrap_err();
        assert!(matches!(err, StartError::Mainnet(_)), "{err}");
    }

    #[test]
    fn start_refuses_a_mainnet_node_before_touching_state() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode {
            chain: "main".into(),
            genesis: MAINNET_GENESIS.into(),
            ..FakeNode::regtest()
        };
        let res = Faucet::start(config(dir.path(), ""), Keypair::generate(), node, NOON);
        assert!(matches!(res, Err(StartError::Mainnet(_))));
        assert!(!dir.path().join("state.json").exists());
    }

    #[test]
    fn preflight_checks_testnet_vs_regtest() {
        let testnet = FakeNode {
            genesis: TESTNET_GENESIS.into(),
            ..FakeNode::regtest()
        };
        assert!(preflight(&testnet, Network::Test).is_ok());
        assert!(matches!(
            preflight(&testnet, Network::Regtest),
            Err(StartError::WrongNetwork(_))
        ));
        let regtest = FakeNode::regtest();
        assert!(preflight(&regtest, Network::Regtest).is_ok());
        assert!(matches!(
            preflight(&regtest, Network::Test),
            Err(StartError::WrongNetwork(_))
        ));
    }

    #[test]
    fn drip_sends_the_fixed_amount_with_a_zip317_fee() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode::regtest();
        node.fund(1, 500_000_000, 50);
        let mut f = faucet(node, dir.path());
        let to = user();
        let receipt = f.drip(&to, "1.2.3.4", NOON).unwrap();
        assert_eq!(receipt.amount_zat, 10_000_000);
        // 1 input, recipient + change: ZIP-317 grace floor, 2 x 5000.
        assert_eq!(receipt.fee_zat, 10_000);
        assert_eq!(receipt.address, to);

        let raw = f.node.sent.borrow()[0].clone();
        let tx = Transaction::read(raw.as_slice(), BranchId::Nu5).unwrap();
        let b = tx.transparent_bundle().unwrap();
        assert_eq!(b.vout[0].value().into_u64(), 10_000_000);
        assert_eq!(b.vout[1].value().into_u64(), 500_000_000 - 10_010_000);
        assert_eq!(
            BranchId::for_height(&Network::Regtest, BlockHeight::from_u32(201)),
            BranchId::Nu5
        );
        // Recorded and persisted.
        let saved: BTreeMap<String, serde_json::Value> =
            serde_json::from_slice(&std::fs::read(dir.path().join("state.json")).unwrap()).unwrap();
        assert_eq!(saved["spent_today_zat"], 10_010_000);
        assert_eq!(f.state.pending.len(), 1);
    }

    #[test]
    fn cooldowns_and_cap_are_enforced_through_drip() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode::regtest();
        for n in 1..=5 {
            node.fund(n, 100_000_000, 50);
        }
        let mut f = faucet(node, dir.path());
        let a = user();
        f.drip(&a, "1.1.1.1", NOON).unwrap();
        // Same address, new IP.
        assert!(matches!(
            f.drip(&a, "2.2.2.2", NOON + 1),
            Err(DripError::Limit(LimitRejection::AddressCooldown { .. }))
        ));
        // New address, same IP.
        assert!(matches!(
            f.drip(&user(), "1.1.1.1", NOON + 1),
            Err(DripError::Limit(LimitRejection::IpCooldown { .. }))
        ));
        f.drip(&user(), "3.3.3.3", NOON + 2).unwrap();
        f.drip(&user(), "4.4.4.4", NOON + 3).unwrap();
        // Cap 30,100,000: three drips + fees = 30,030,000; a fourth won't fit.
        assert!(matches!(
            f.drip(&user(), "5.5.5.5", NOON + 4),
            Err(DripError::Limit(LimitRejection::DailyCapReached { .. }))
        ));
        assert_eq!(f.node.sent.borrow().len(), 3);
    }

    #[test]
    fn rejected_requests_send_nothing_and_record_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode::regtest();
        node.fund(1, 100_000_000, 50);
        let mut f = faucet(node, dir.path());
        let main = Keypair::generate().encode_address(Network::Main);
        assert!(matches!(
            f.drip(&main, "1.1.1.1", NOON),
            Err(DripError::InvalidAddress(_))
        ));
        let own = f.address().to_string();
        assert!(matches!(
            f.drip(&own, "1.1.1.1", NOON),
            Err(DripError::InvalidAddress(AddressError::FaucetItself))
        ));
        assert!(f.node.sent.borrow().is_empty());
        assert!(f.state.ip_last_drip.is_empty());
        // So the same IP can still drip.
        f.drip(&user(), "1.1.1.1", NOON).unwrap();
    }

    #[test]
    fn node_rejection_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode {
            reject_with: Some("bad-txns".into()),
            ..FakeNode::regtest()
        };
        node.fund(1, 100_000_000, 50);
        let mut f = faucet(node, dir.path());
        let a = user();
        assert!(matches!(
            f.drip(&a, "1.1.1.1", NOON),
            Err(DripError::Rejected(_))
        ));
        assert!(f.state.pending.is_empty());
        assert_eq!(f.state.spent_today_zat, 0);
        assert!(f.state.address_last_drip.is_empty());
    }

    #[test]
    fn rpc_error_for_a_tx_the_node_admitted_counts_as_sent() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode {
            reject_with: Some("channel closed".into()),
            admit_despite_error: true,
            ..FakeNode::regtest()
        };
        node.fund(1, 100_000_000, 50);
        let mut f = faucet(node, dir.path());
        let a = user();
        let receipt = f.drip(&a, "1.1.1.1", NOON).unwrap();
        assert_eq!(f.state.pending.len(), 1);
        assert_eq!(f.state.pending[0].txid, receipt.txid);
        // Cooldown and budget were charged, so no second drip slips through.
        assert!(matches!(
            f.drip(&a, "2.2.2.2", NOON + 1),
            Err(DripError::Limit(LimitRejection::AddressCooldown { .. }))
        ));
    }

    #[test]
    fn in_flight_inputs_are_reserved_until_mined() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode::regtest();
        node.fund(1, 100_000_000, 50);
        let mut f = faucet(node, dir.path());
        f.drip(&user(), "1.1.1.1", NOON).unwrap();
        // The only UTXO is spent by the in-flight drip: busy, not a double spend.
        assert!(matches!(
            f.drip(&user(), "2.2.2.2", NOON + 1),
            Err(DripError::Busy)
        ));
        assert_eq!(f.node.sent.borrow().len(), 1);
        // Mined: the pending entry is dropped; new funds are usable.
        let spent = f.state.pending[0].spent.clone();
        f.node.mine_block(&spent);
        f.node.fund(2, 100_000_000, 201);
        f.drip(&user(), "2.2.2.2", NOON + 2).unwrap();
        assert_eq!(f.state.pending.len(), 1);
    }

    #[test]
    fn expired_unmined_drip_releases_its_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode::regtest();
        node.fund(1, 100_000_000, 50);
        let mut f = faucet(node, dir.path());
        f.drip(&user(), "1.1.1.1", NOON).unwrap();
        // The node forgets it (evicted) and the chain passes its expiry.
        f.node.mempool.borrow_mut().clear();
        *f.node.tip.borrow_mut() = f.state.pending[0].expiry_height;
        f.drip(&user(), "2.2.2.2", NOON + 1).unwrap();
        assert_eq!(f.node.sent.borrow().len(), 2);
    }

    #[test]
    fn immature_coinbase_is_not_spent() {
        let dir = tempfile::tempdir().unwrap();
        let mut node = FakeNode::regtest();
        // Tip 200: height 150 has 51 confirmations.
        let cb = node.fund(1, 625_000_000, 150);
        node.coinbase.insert(cb);
        let mut f = faucet(node, dir.path());
        assert!(matches!(
            f.drip(&user(), "1.1.1.1", NOON),
            Err(DripError::Maturing)
        ));
        let s = f.status(NOON).unwrap();
        assert_eq!(s.balance_zat, 0);
        assert_eq!(s.immature_zat, 625_000_000);
        // Regtest: coinbase funds drips once mature, never "must be shielded".
        assert_eq!(s.coinbase_unshielded_zat, 0);
        // 100 confirmations: mature.
        *f.node.tip.borrow_mut() = 249;
        f.drip(&user(), "1.1.1.1", NOON).unwrap();
    }

    #[test]
    fn empty_faucet_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode::regtest();
        node.fund(1, 5_000_000, 50);
        let mut f = faucet(node, dir.path());
        assert!(matches!(
            f.drip(&user(), "1.1.1.1", NOON),
            Err(DripError::InsufficientFunds)
        ));
    }

    #[test]
    fn status_reports_budget_and_the_max_balance_guard() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode::regtest();
        // Cap 30,100,000 x default multiple 5 = 150,500,000.
        node.fund(1, 100_000_000, 50);
        let mut f = faucet(node, dir.path());
        let s = f.status(NOON).unwrap();
        assert_eq!(s.balance_zat, 100_000_000);
        assert_eq!(s.remaining_today_zat, 30_100_000);
        assert_eq!(s.max_balance_zat, 150_500_000);
        assert!(!s.over_max_balance);
        assert_eq!(s.resets_in_secs, 43_200);
        f.node.fund(2, 100_000_000, 60);
        assert!(f.status(NOON).unwrap().over_max_balance);

        f.drip(&user(), "1.1.1.1", NOON).unwrap();
        let s = f.status(NOON).unwrap();
        assert_eq!(s.spent_today_zat, 10_010_000);
        assert_eq!(s.in_flight_drips, 1);
        assert_eq!(s.in_flight_zat, 100_000_000);
    }

    #[test]
    fn selection_folds_dust_change_into_the_fee() {
        let pool = vec![AddressUtxo {
            address: String::new(),
            txid: "11".repeat(32),
            output_index: 0,
            satoshis: 10_010_500,
            height: 1,
        }];
        let sel = select(&pool, 10_000_000, P2PKH_STANDARD_OUTPUT_SIZE).unwrap();
        assert_eq!(sel.fee_zat, 10_500);
        assert!(select(&pool, 10_000_001 + 10_000, P2PKH_STANDARD_OUTPUT_SIZE).is_none());
    }
}
