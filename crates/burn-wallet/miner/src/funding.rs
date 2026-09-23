//! Funding discovery by address: which confirmed outputs paying the miner's
//! t-address it may spend, from zebrad's `getaddressutxos` (its transparent
//! address index), at any chain height. Replaces the old walk of every
//! block from height 1 looking only at coinbase outputs, which was
//! impractical on testnet (~3.9M blocks) and blind to ordinary transfers
//! (faucet drips, z->t deshields, top-ups).
//!
//! Policy, per output `getaddressutxos` reports (all of them confirmed --
//! the RPC never reflects the mempool):
//!
//! - **Reserved** by our own in-flight burn (its inputs, see
//!   [`crate::state::PendingBurn`]): skipped. The node still lists them
//!   until the burn is mined.
//! - **Coinbase with fewer than [`COINBASE_MATURITY`] confirmations**
//!   (regtest): skipped (immature). `getaddressutxos` doesn't flag
//!   coinbase, so for an output that young we ask `getrawtransaction`
//!   whether `vin[0]` is a coinbase input, and cache the answer per txid
//!   (it never changes). On regtest, outputs with 100+ confirmations are
//!   spendable either way and cost no lookup.
//! - **Coinbase on a network that forbids unshielded coinbase spends**
//!   (testnet, mainnet -- see
//!   [`burn_wallet::Network::allows_unshielded_coinbase_spends`]): never
//!   spendable, mature or not, because a burn always has transparent
//!   outputs. Counted as "coinbase (must be shielded first)". On these
//!   networks every output's coinbase-ness is looked up (once per txid),
//!   including tracked ones carried over from an older state file.
//! - **Everything else** (ordinary transfers at 1+ confirmation, mature
//!   coinbase on regtest): spendable.
//!
//! Mempool policy: unconfirmed inbound transfers are not spent (the address
//! index doesn't show them; they become spendable one block later), and the
//! miner never spends its own unconfirmed change -- it has at most one burn
//! in flight and waits for it to confirm before building the next.

use std::collections::{BTreeSet, HashMap};

use burn_wallet::utxo::COINBASE_MATURITY;
use burn_wallet::{Network, RpcError};

use crate::node::{AddressSnapshot, Node};
use crate::state::{OutPointRef, TrackedUtxo};

/// Spendable funding found at one snapshot, and what was held back.
#[derive(Debug, Default)]
pub(crate) struct Classified {
    /// Outputs the miner may spend now.
    pub spendable: Vec<TrackedUtxo>,
    /// Sum of coinbase outputs still short of [`COINBASE_MATURITY`] (on a
    /// network where coinbase can fund a burn at all).
    pub immature_zat: u64,
    /// Sum of coinbase outputs that can never fund a burn on this network
    /// until shielded (testnet, mainnet; always 0 on regtest).
    pub coinbase_zat: u64,
    /// Sum of outputs reserved by our own in-flight burn.
    pub reserved_zat: u64,
}

/// Classifies address snapshots for one network, caching coinbase-ness
/// per txid for the life of the process.
#[derive(Debug)]
pub(crate) struct Funding {
    /// Whether coinbase may fund a burn (see
    /// [`Network::allows_unshielded_coinbase_spends`]).
    coinbase_spendable: bool,
    coinbase: HashMap<String, bool>,
}

impl Funding {
    /// An empty cache, applying `network`'s coinbase spending rule.
    pub(crate) fn new(network: Network) -> Self {
        Self {
            coinbase_spendable: network.allows_unshielded_coinbase_spends(),
            coinbase: HashMap::new(),
        }
    }

    /// Whether coinbase may fund a burn on this network.
    pub(crate) fn coinbase_spendable(&self) -> bool {
        self.coinbase_spendable
    }

    fn is_coinbase(&mut self, node: &impl Node, txid: &str) -> Result<bool, RpcError> {
        if let Some(&known) = self.coinbase.get(txid) {
            return Ok(known);
        }
        let is = node.is_coinbase(txid)?;
        self.coinbase.insert(txid.to_string(), is);
        Ok(is)
    }

    /// Splits `snapshot` into spendable / immature / reserved (see the
    /// module docs for the policy).
    ///
    /// # Errors
    ///
    /// Returns the [`RpcError`] of a failed coinbase lookup.
    pub(crate) fn classify(
        &mut self,
        node: &impl Node,
        snapshot: &AddressSnapshot,
        reserved: &BTreeSet<OutPointRef>,
    ) -> Result<Classified, RpcError> {
        let mut out = Classified::default();
        for u in &snapshot.utxos {
            let outpoint = OutPointRef {
                txid: u.txid.clone(),
                vout: u.output_index,
            };
            if reserved.contains(&outpoint) {
                out.reserved_zat = out.reserved_zat.saturating_add(u.satoshis);
                continue;
            }
            if !self.coinbase_spendable {
                if self.is_coinbase(node, &u.txid)? {
                    out.coinbase_zat = out.coinbase_zat.saturating_add(u.satoshis);
                    continue;
                }
            } else {
                let confirmations = snapshot.tip_height.saturating_sub(u.height) + 1;
                if confirmations < u64::from(COINBASE_MATURITY)
                    && self.is_coinbase(node, &u.txid)?
                {
                    out.immature_zat = out.immature_zat.saturating_add(u.satoshis);
                    continue;
                }
            }
            out.spendable.push(TrackedUtxo {
                txid: u.txid.clone(),
                vout: u.output_index,
                value_zat: u.satoshis,
            });
        }
        Ok(out)
    }

    /// On a network where coinbase can't fund a burn, removes coinbase
    /// outputs from the tracked pool and returns them (e.g. ones an older
    /// `sova-miner` tracked from a testnet state file). A no-op on
    /// regtest.
    ///
    /// # Errors
    ///
    /// Returns the [`RpcError`] of a failed coinbase lookup.
    pub(crate) fn drop_coinbase(
        &mut self,
        node: &impl Node,
        utxos: &mut Vec<TrackedUtxo>,
    ) -> Result<Vec<TrackedUtxo>, RpcError> {
        if self.coinbase_spendable {
            return Ok(Vec::new());
        }
        // Look everything up before touching the pool, so a failed lookup
        // leaves it as it was.
        let mut coinbase = Vec::with_capacity(utxos.len());
        for u in utxos.iter() {
            coinbase.push(self.is_coinbase(node, &u.txid)?);
        }
        let (dropped, kept): (Vec<_>, Vec<_>) = std::mem::take(utxos)
            .into_iter()
            .zip(coinbase)
            .partition(|(_, is)| *is);
        *utxos = kept.into_iter().map(|(u, _)| u).collect();
        Ok(dropped.into_iter().map(|(u, _)| u).collect())
    }
}

/// The balance lines `report` prints for a classified snapshot: label and
/// zatoshis. Coinbase appears as "coinbase maturing" where it can fund a
/// burn once mature (regtest), and as "coinbase (must be shielded first)"
/// where it never can (testnet, mainnet).
pub(crate) fn balance_lines(c: &Classified, coinbase_spendable: bool) -> Vec<(&'static str, u64)> {
    let spendable: u64 = c.spendable.iter().map(|u| u.value_zat).sum();
    let coinbase = if coinbase_spendable {
        ("coinbase maturing", c.immature_zat)
    } else {
        ("coinbase (must be shielded first)", c.coinbase_zat)
    };
    vec![
        ("spendable", spendable),
        coinbase,
        ("reserved by burn in flight", c.reserved_zat),
    ]
}

/// Drops tracked UTXOs that `snapshot` no longer lists (spent outside this
/// miner, or on a chain the node no longer serves), returning them. The
/// snapshot shows only confirmed outputs, so this must run while no burn of
/// ours is in flight whose change is tracked -- which holds, since a
/// burn's change is only tracked once it confirmed (see
/// [`crate::epoch::resolve_pending`]).
pub(crate) fn drop_stale(
    utxos: &mut Vec<TrackedUtxo>,
    snapshot: &AddressSnapshot,
) -> Vec<TrackedUtxo> {
    let live: BTreeSet<(&str, u32)> = snapshot
        .utxos
        .iter()
        .map(|u| (u.txid.as_str(), u.output_index))
        .collect();
    let (kept, dropped): (Vec<_>, Vec<_>) = std::mem::take(utxos)
        .into_iter()
        .partition(|t| live.contains(&(t.txid.as_str(), t.vout)));
    *utxos = kept;
    dropped
}

/// Adds every `found` UTXO not already in `utxos`; returns how many were
/// added.
pub(crate) fn merge(utxos: &mut Vec<TrackedUtxo>, found: Vec<TrackedUtxo>) -> usize {
    let mut added = 0;
    for f in found {
        if !utxos.iter().any(|t| t.txid == f.txid && t.vout == f.vout) {
            utxos.push(f);
            added += 1;
        }
    }
    added
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use std::cell::{Cell, RefCell};

    use burn_wallet::rpc::AddressUtxo;
    use zcash_primitives::transaction::Transaction;
    use zcash_protocol::consensus::BranchId;

    use super::*;
    use crate::node::TxStatus;

    /// An in-memory node for funding/epoch tests: a UTXO list, a tip, a set
    /// of coinbase txids, the txs it knows, and scripted
    /// `sendrawtransaction` behaviour.
    #[derive(Default)]
    pub(crate) struct FakeNode {
        pub tip: Cell<u64>,
        pub utxos: RefCell<Vec<AddressUtxo>>,
        pub coinbase: BTreeSet<String>,
        /// txid -> status the node reports.
        pub txs: RefCell<HashMap<String, TxStatus>>,
        pub coinbase_lookups: Cell<u32>,
        pub address_lookups: Cell<u32>,
        /// Raw txs handed to `send_raw`, in order.
        pub sent: RefCell<Vec<String>>,
        /// Scripted `send_raw` answers, consumed front first.
        pub send_script: RefCell<Vec<SendScript>>,
    }

    impl FakeNode {
        pub(crate) fn fund(&self, txid: &str, vout: u32, satoshis: u64, height: u64) {
            self.utxos.borrow_mut().push(AddressUtxo {
                address: "tmAddr".to_string(),
                txid: txid.to_string(),
                output_index: vout,
                satoshis,
                height,
            });
            self.txs
                .borrow_mut()
                .insert(txid.to_string(), TxStatus::Confirmed { height });
        }
    }

    impl Node for FakeNode {
        fn tip_height(&self) -> Result<u64, RpcError> {
            Ok(self.tip.get())
        }
        fn address_utxos(&self, _address: &str) -> Result<AddressSnapshot, RpcError> {
            self.address_lookups.set(self.address_lookups.get() + 1);
            Ok(AddressSnapshot {
                utxos: self.utxos.borrow().clone(),
                tip_height: self.tip.get(),
            })
        }
        fn is_coinbase(&self, txid: &str) -> Result<bool, RpcError> {
            self.coinbase_lookups.set(self.coinbase_lookups.get() + 1);
            Ok(self.coinbase.contains(txid))
        }
        fn tx_status(&self, txid: &str) -> Result<TxStatus, RpcError> {
            Ok(self
                .txs
                .borrow()
                .get(txid)
                .copied()
                .unwrap_or(TxStatus::Unknown))
        }
        fn send_raw(&self, raw_hex: &str) -> Result<String, RpcError> {
            self.sent.borrow_mut().push(raw_hex.to_string());
            let Some(txid) = txid_of(raw_hex) else {
                return Err(rpc_failure("failed to deserialize transaction"));
            };
            let script = {
                let mut queue = self.send_script.borrow_mut();
                (!queue.is_empty()).then(|| queue.remove(0))
            };
            let known = self.txs.borrow().get(&txid).copied();
            match script {
                Some(SendScript::AdmitButFail(message)) => {
                    self.txs.borrow_mut().insert(txid, TxStatus::Mempool);
                    Err(rpc_failure(&message))
                }
                Some(SendScript::Reject(message)) => Err(rpc_failure(&message)),
                Some(SendScript::TransportDown) => Err(RpcError::EmptyResponse {
                    method: "sendrawtransaction".to_string(),
                }),
                // zebrad's answers for a tx it already has.
                None if known == Some(TxStatus::Mempool) => {
                    Err(rpc_failure("transaction already exists in mempool"))
                }
                None if matches!(known, Some(TxStatus::Confirmed { .. })) => {
                    Err(rpc_failure("transaction was committed to the best chain"))
                }
                None => {
                    self.txs
                        .borrow_mut()
                        .insert(txid.clone(), TxStatus::Mempool);
                    Ok(txid)
                }
            }
        }
    }

    /// One scripted `sendrawtransaction` behaviour (consumed in order;
    /// with none left, the node admits the tx like zebrad would).
    #[derive(Debug, Clone)]
    pub(crate) enum SendScript {
        /// Admit the tx, but answer with this JSON-RPC error (the zebrad
        /// `channel closed` case seen on the box).
        AdmitButFail(String),
        /// Don't admit it; answer with this JSON-RPC error.
        Reject(String),
        /// Don't admit it; fail as if the connection dropped.
        TransportDown,
    }

    pub(crate) fn rpc_failure(message: &str) -> RpcError {
        RpcError::RpcFailure {
            method: "sendrawtransaction".to_string(),
            code: -1,
            message: message.to_string(),
        }
    }

    /// The RPC-display txid of a raw transaction, as zebrad computes it
    /// (`None` if it doesn't parse).
    pub(crate) fn txid_of(raw_hex: &str) -> Option<String> {
        let raw = hex::decode(raw_hex).ok()?;
        let tx = Transaction::read(raw.as_slice(), BranchId::Nu5).ok()?;
        Some(burn_wallet::utxo::encode_rpc_hash(tx.txid().into()))
    }

    pub(crate) fn txid(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    #[test]
    fn transfers_are_spendable_at_one_confirmation() {
        let node = FakeNode::default();
        node.tip.set(1_000);
        node.fund(&txid(1), 0, 10_000_000, 1_000); // a faucet drip, 1 conf
        let snap = node.address_utxos("tmAddr").unwrap();
        for network in [Network::Regtest, Network::Test, Network::Main] {
            let c = Funding::new(network)
                .classify(&node, &snap, &BTreeSet::new())
                .unwrap();
            assert_eq!(c.spendable.len(), 1, "{network:?}");
            assert_eq!(c.spendable[0].value_zat, 10_000_000);
            assert_eq!(c.immature_zat, 0);
            assert_eq!(c.coinbase_zat, 0);
        }
    }

    /// Testnet/mainnet: coinbase never funds a burn, mature or not; it is
    /// reported as coinbase to shield, not as maturing. Ordinary transfers
    /// beside it stay spendable.
    #[test]
    fn coinbase_is_never_spendable_where_consensus_forbids_it() {
        let mut node = FakeNode::default();
        node.tip.set(1_000);
        node.coinbase.insert(txid(1));
        node.coinbase.insert(txid(2));
        node.fund(&txid(1), 0, 125_000_000, 100); // 901 confs: mature
        node.fund(&txid(2), 0, 125_000_000, 990); // 11 confs: immature
        node.fund(&txid(3), 1, 10_000_000, 999); // a transfer, 2 confs
        let snap = node.address_utxos("tmAddr").unwrap();
        for network in [Network::Test, Network::Main] {
            let mut funding = Funding::new(network);
            assert!(!funding.coinbase_spendable());
            let c = funding.classify(&node, &snap, &BTreeSet::new()).unwrap();
            assert_eq!(c.spendable.len(), 1, "{network:?}");
            assert_eq!(c.spendable[0].txid, txid(3));
            assert_eq!(c.coinbase_zat, 250_000_000);
            assert_eq!(c.immature_zat, 0);
        }
    }

    /// Regtest (Zebra's `should_allow_unshielded_coinbase_spends = true`
    /// default): the same address funds from its mature coinbase.
    #[test]
    fn regtest_still_spends_mature_coinbase() {
        let mut node = FakeNode::default();
        node.tip.set(1_000);
        node.coinbase.insert(txid(1));
        node.fund(&txid(1), 0, 125_000_000, 100);
        let snap = node.address_utxos("tmAddr").unwrap();
        let mut funding = Funding::new(Network::Regtest);
        assert!(funding.coinbase_spendable());
        let c = funding.classify(&node, &snap, &BTreeSet::new()).unwrap();
        assert_eq!(c.spendable.len(), 1);
        assert_eq!(c.coinbase_zat, 0);
        // Mature outputs cost no lookup on regtest.
        assert_eq!(node.coinbase_lookups.get(), 0);
    }

    #[test]
    fn report_lines_show_coinbase_separately_per_network() {
        let mut node = FakeNode::default();
        node.tip.set(1_000);
        node.coinbase.insert(txid(1));
        node.fund(&txid(1), 0, 125_000_000, 100);
        node.fund(&txid(2), 0, 7_000_000, 999);
        let snap = node.address_utxos("tmAddr").unwrap();

        let mut testnet = Funding::new(Network::Test);
        let c = testnet.classify(&node, &snap, &BTreeSet::new()).unwrap();
        assert_eq!(
            balance_lines(&c, testnet.coinbase_spendable()),
            vec![
                ("spendable", 7_000_000),
                ("coinbase (must be shielded first)", 125_000_000),
                ("reserved by burn in flight", 0),
            ]
        );

        let mut regtest = Funding::new(Network::Regtest);
        let c = regtest.classify(&node, &snap, &BTreeSet::new()).unwrap();
        assert_eq!(
            balance_lines(&c, regtest.coinbase_spendable()),
            vec![
                ("spendable", 132_000_000),
                ("coinbase maturing", 0),
                ("reserved by burn in flight", 0),
            ]
        );
    }

    #[test]
    fn tracked_coinbase_is_dropped_only_where_consensus_forbids_it() {
        let mut node = FakeNode::default();
        node.coinbase.insert(txid(1));
        let tracked = vec![
            TrackedUtxo {
                txid: txid(1),
                vout: 0,
                value_zat: 125_000_000,
            },
            TrackedUtxo {
                txid: txid(2),
                vout: 1,
                value_zat: 9_000_000,
            },
        ];

        let mut regtest = tracked.clone();
        let dropped = Funding::new(Network::Regtest)
            .drop_coinbase(&node, &mut regtest)
            .unwrap();
        assert!(dropped.is_empty());
        assert_eq!(regtest, tracked);

        let mut testnet = tracked.clone();
        let dropped = Funding::new(Network::Test)
            .drop_coinbase(&node, &mut testnet)
            .unwrap();
        assert_eq!(dropped, vec![tracked[0].clone()]);
        assert_eq!(testnet, vec![tracked[1].clone()]);
    }

    #[test]
    fn young_coinbase_is_immature_and_mature_coinbase_is_spendable() {
        let mut node = FakeNode::default();
        node.tip.set(200);
        node.coinbase.insert(txid(1));
        node.coinbase.insert(txid(2));
        node.fund(&txid(1), 0, 625_000_000, 101); // 100 confs: mature
        node.fund(&txid(2), 0, 625_000_000, 102); // 99 confs: immature
        let snap = node.address_utxos("tmAddr").unwrap();
        let c = Funding::new(Network::Regtest)
            .classify(&node, &snap, &BTreeSet::new())
            .unwrap();
        assert_eq!(c.spendable.len(), 1);
        assert_eq!(c.spendable[0].txid, txid(1));
        assert_eq!(c.immature_zat, 625_000_000);
        // Only the young output needed a lookup.
        assert_eq!(node.coinbase_lookups.get(), 1);
    }

    #[test]
    fn coinbase_lookups_are_cached() {
        let mut node = FakeNode::default();
        node.tip.set(150);
        node.coinbase.insert(txid(1));
        node.fund(&txid(1), 0, 625_000_000, 140);
        node.fund(&txid(2), 0, 5_000_000, 145);
        let snap = node.address_utxos("tmAddr").unwrap();
        let mut funding = Funding::new(Network::Regtest);
        for _ in 0..3 {
            funding.classify(&node, &snap, &BTreeSet::new()).unwrap();
        }
        assert_eq!(node.coinbase_lookups.get(), 2);
    }

    #[test]
    fn reserved_outputs_are_not_spendable() {
        let node = FakeNode::default();
        node.tip.set(500);
        node.fund(&txid(1), 0, 1_000_000, 400);
        node.fund(&txid(1), 1, 2_000_000, 400);
        let reserved: BTreeSet<OutPointRef> = [OutPointRef {
            txid: txid(1),
            vout: 1,
        }]
        .into();
        let snap = node.address_utxos("tmAddr").unwrap();
        let c = Funding::new(Network::Regtest)
            .classify(&node, &snap, &reserved)
            .unwrap();
        assert_eq!(c.spendable.len(), 1);
        assert_eq!(c.spendable[0].vout, 0);
        assert_eq!(c.reserved_zat, 2_000_000);
    }

    #[test]
    fn stale_tracked_utxos_are_dropped_and_merge_dedupes() {
        let node = FakeNode::default();
        node.tip.set(500);
        node.fund(&txid(1), 2, 1_000_000, 400);
        let snap = node.address_utxos("tmAddr").unwrap();
        let mut tracked = vec![
            TrackedUtxo {
                txid: txid(1),
                vout: 2,
                value_zat: 1_000_000,
            },
            TrackedUtxo {
                txid: txid(9),
                vout: 0,
                value_zat: 7,
            },
        ];
        let dropped = drop_stale(&mut tracked, &snap);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].txid, txid(9));
        assert_eq!(tracked.len(), 1);

        let found = Funding::new(Network::Regtest)
            .classify(&node, &snap, &BTreeSet::new())
            .unwrap()
            .spendable;
        assert_eq!(merge(&mut tracked, found), 0);
        assert_eq!(tracked.len(), 1);
    }
}
