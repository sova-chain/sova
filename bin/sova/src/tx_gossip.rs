//! Transaction gossip on a chain without a consensus client.
//!
//! Transactions travel over reth's own devp2p `eth` transaction gossip
//! (`TransactionsManager`, reth `crates/net/network/src/transactions/mod.rs`);
//! sova/1 carries blocks only. Stock reth assumes a CL-driven node and a
//! steady flow of new blocks, and two of its rules then lose a transaction
//! for good on Sova (public testnet, 2026-09-25: the deployer's first tx sat
//! in `sova-rpc-1`'s pool while the seed and the keeper never saw it):
//!
//! 1. **"Initially syncing" lasts until the first new block.** The node
//!    launcher marks the network `Syncing` at every start and flips it to
//!    `Idle` only on the first canonical-chain commit (reth
//!    `node/builder/src/launch/engine.rs:153` and `:356`). Until then the
//!    `TransactionsManager` drops every announcement and every full
//!    transaction a peer sends (`transactions/mod.rs:662`, `:1323`), and
//!    skips announcing its own pool to a peer whose session is established
//!    (`:1205`). Sova blocks follow Zcash blocks (~75 s apart, and none at
//!    all while Zcash stalls), and static peers connect within a second of
//!    start, so a restarted node always meets its peers in that state.
//! 2. **Nothing is ever announced twice.** A pending transaction is
//!    announced once, when it becomes pending (`:822`), and a pool once per
//!    session (`:1205`). The sender marks a hash as seen by a peer when it
//!    announces it, so a receiver that dropped it (rule 1) is never offered
//!    it again; a local transaction restored from the datadir backup at
//!    restart, or submitted while the node had no peers, is never announced
//!    at all. There is no periodic rebroadcast.
//!
//! The fixes, both outside consensus:
//!
//! - [`apply`]: reth's `--debug.startup-sync-state-idle`. The network is
//!   `Idle` as soon as the engine starts (unless reth itself starts a
//!   backfill, which still sets `Syncing` and back), so a restarted node
//!   accepts announcements and announces its pool to peers at once. This is
//!   also what `eth_syncing` reports; a Sova node has no CL to wait for.
//! - [`run_rebroadcast`]: every [`REBROADCAST_INTERVAL`], announce the
//!   hashes of the pool's pending, propagatable transactions to every
//!   connected peer, bypassing the "already seen" cache. A peer that holds
//!   one already ignores it (no fetch; reth's penalty for a re-announced
//!   hash is zero), a peer that lacks one fetches it. That repairs any gap,
//!   whatever its cause, within one interval, and each hop forwards what it
//!   receives, so an RPC node's transaction reaches a sealer behind a seed.

use std::time::Duration;

use reth_ethereum::{
    chainspec::ChainSpec,
    network::{NetworkHandle, NetworkPrimitives},
    node::core::node_config::NodeConfig,
    pool::TransactionPool,
};

/// How often the pending pool is re-announced to every peer.
pub(crate) const REBROADCAST_INTERVAL: Duration = Duration::from_secs(15);

/// At most this many hashes per peer per round: one `eth` announcement
/// message (reth's soft cap is 4096 hashes), a few tens of kB.
pub(crate) const REBROADCAST_MAX_HASHES: usize = 1024;

/// The network is `Idle` from engine start, not from the first new block.
pub(crate) fn apply(node_config: &mut NodeConfig<ChainSpec>) {
    node_config.debug.startup_sync_state_idle = true;
}

/// The hashes one rebroadcast round announces: pending and propagatable
/// (reth never gossips a transaction whose `propagate` flag is off), local
/// submissions first, then oldest first, at most `max`.
pub(crate) fn rebroadcast_hashes<P: TransactionPool>(
    pool: &P,
    max: usize,
) -> Vec<alloy_primitives::B256> {
    let mut txs: Vec<_> = pool
        .pending_transactions()
        .into_iter()
        .filter(|tx| tx.propagate)
        .collect();
    txs.sort_by_key(|tx| (!tx.origin.is_local(), tx.timestamp));
    txs.into_iter().take(max).map(|tx| *tx.hash()).collect()
}

/// Re-announce the pending pool to every connected peer, forever (see the
/// module doc). Returns only if the network has no transaction task.
pub(crate) async fn run_rebroadcast<P, N>(pool: P, network: NetworkHandle<N>)
where
    P: TransactionPool + 'static,
    N: NetworkPrimitives,
{
    let Some(txs) = network.transactions_handle().await else {
        eprintln!(
            "tx gossip: WARNING no transaction task; pending transactions are not rebroadcast"
        );
        return;
    };
    let mut tick = tokio::time::interval(REBROADCAST_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick is immediate; the pool is empty or just restored then.
    tick.tick().await;
    loop {
        tick.tick().await;
        let hashes = rebroadcast_hashes(&pool, REBROADCAST_MAX_HASHES);
        if hashes.is_empty() {
            continue;
        }
        let Ok(peers) = txs.get_active_peers().await else {
            return;
        };
        if peers.is_empty() {
            continue;
        }
        reth_tracing::tracing::debug!(
            target: "sova::tx_gossip",
            txs = hashes.len(),
            peers = peers.len(),
            "re-announcing pending transactions"
        );
        for peer in peers {
            txs.propagate_hashes_to(hashes.iter().copied(), peer);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use reth_ethereum::pool::{
        PoolTransaction, TransactionOrigin,
        test_utils::{MockTransaction, testing_pool},
    };

    use super::*;

    #[test]
    fn apply_sets_startup_idle() {
        let mut config = NodeConfig::new(reth_ethereum::chainspec::DEV.clone());
        assert!(!config.debug.startup_sync_state_idle);
        apply(&mut config);
        assert!(config.debug.startup_sync_state_idle);
    }

    #[tokio::test]
    async fn rebroadcast_takes_pending_local_first_and_caps() {
        let pool = testing_pool();
        let external = MockTransaction::eip1559();
        let local = MockTransaction::eip1559();
        // A nonce gap: queued, not pending, so never announced.
        let queued = MockTransaction::eip1559().with_nonce(5);
        // Pending but not propagatable: reth never gossips it, neither do we.
        let private = MockTransaction::eip1559();
        for (origin, tx) in [
            (TransactionOrigin::External, &external),
            (TransactionOrigin::Local, &local),
            (TransactionOrigin::External, &queued),
            (TransactionOrigin::Private, &private),
        ] {
            pool.add_transaction(origin, tx.clone()).await.unwrap();
        }
        assert_eq!(pool.pending_transactions().len(), 3);

        let all = rebroadcast_hashes(&pool, 10);
        assert_eq!(all, vec![*local.hash(), *external.hash()]);
        assert_eq!(rebroadcast_hashes(&pool, 1), vec![*local.hash()]);
    }
}
