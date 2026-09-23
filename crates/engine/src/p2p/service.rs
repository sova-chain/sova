//! The `sova/1` gossip service: one task per node that owns all
//! propagation policy.
//!
//! - **Announce** (push, tiny): whenever the local canonical head changes,
//!   every canonical block not yet announced (bounded walk back from the
//!   head) is announced to all peers; and every block accepted from a peer
//!   (`VALID`/`ACCEPTED`) is re-announced to all *other* peers. Nothing
//!   else is ever announced — a block this node rejected, or could not yet
//!   connect (`SYNCING`), is never forwarded.
//! - **Fetch** (pull, bodies): an announcement for a hash we have neither
//!   seen nor stored triggers one `GetBlock` to the announcer. Only a
//!   `Block` whose hash we requested *from that peer* is accepted;
//!   anything else is dropped, so a peer cannot push bodies at us.
//! - **Submit**: a fetched block goes to the local engine as an in-process
//!   `new_payload` ([`GossipBackend::submit`]) — the same validation path
//!   (including `convert_payload_to_block`'s C5 check and candidate
//!   observation) as any Engine API payload. **The service never issues a
//!   forkchoice update**: whether a received block becomes our head is
//!   decided by the local arbiter (`crate::candidates::run_arbiter`), fed by
//!   the validator's candidate observation. Relay delivers, arbiter
//!   decides.
//! - **`SYNCING` is not misbehaviour.** It means "parent unknown": the
//!   block is parked as an orphan and its parent is requested from the
//!   same peer, up to [`MAX_ANCESTOR_DEPTH`] ancestors deep; beyond that
//!   the gap is logged and left to backfill (a later step). When a parent
//!   is accepted, its parked children are resubmitted. Only an `INVALID`
//!   status costs the sender reputation
//!   ([`GossipBackend::penalize_invalid_block`]) — except a consensus
//!   *hold* ([`crate::consensus::HOLD_MARKER`]: our zebrad can't vouch for
//!   the block's Zcash epoch yet), which is forgotten so a later
//!   announcement retries it.
//! - **Dedup and bounds**: LRU sets of seen (fetched/known) and announced
//!   hashes; a global cap on in-flight requests with a timeout; per-peer
//!   per-second budgets for announcements and block requests (excess is
//!   dropped); bounded orphan and recently-accepted-block caches.

use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    time::{Duration, Instant},
};

use alloy_primitives::{B256, Bytes};
use alloy_rlp::Decodable;
use alloy_rpc_types::engine::PayloadStatusEnum;
use reth_ethereum::{network::api::PeerId, primitives::SealedBlock};
use schnellru::{ByLength, LruMap};
use tokio::sync::mpsc;

use super::{
    codec::{Announce, GetBlock, MAX_BLOCK_BYTES, SovaMessage},
    protocol::{ConnectionId, PeerEvent, ServiceReceivers},
};

/// How many ancestors of an announced block we will chase over `sova/1`
/// when the engine answers `SYNCING`. Beyond this, catch-up is backfill's
/// job (the engine tree's own download threshold is the same 32).
pub const MAX_ANCESTOR_DEPTH: u32 = 32;
/// Hashes remembered as seen (fetched, or found locally).
pub const SEEN_CAPACITY: u32 = 4096;
/// Hashes remembered as already announced by us.
pub const ANNOUNCED_CAPACITY: u32 = 4096;
/// Recently accepted peer blocks kept (as RLP) so we can serve them even
/// when they are not on our canonical chain.
pub const RECENT_BLOCKS_CAPACITY: u32 = 128;
/// Orphans (`SYNCING` blocks awaiting a parent) kept.
pub const MAX_ORPHANS: u32 = 64;
/// Outstanding `GetBlock` requests, across all peers.
pub const MAX_IN_FLIGHT: usize = 64;
/// A request unanswered for this long is forgotten (a later announcement
/// may retry it, possibly from another peer).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Per-peer, per-second budget of inbound `Announce`s.
pub const MAX_ANNOUNCES_PER_SEC: u32 = 128;
/// Per-peer, per-second budget of inbound `GetBlock`s we serve.
pub const MAX_GET_BLOCKS_PER_SEC: u32 = 64;
/// Held blocks (consensus said "not yet": our zebrad hasn't scanned the
/// block's Zcash epoch) kept for resubmission.
pub const MAX_HELD: u32 = 64;
/// A held block is resubmitted at least this often, and immediately once
/// our scan watermark reaches its height.
pub const HOLD_RETRY: Duration = Duration::from_secs(3);
/// How often held blocks are checked against our scan watermark: a block
/// that arrived a moment before our zebrad's epoch should import as soon
/// as the scan reaches it.
pub const HELD_POLL: Duration = Duration::from_millis(200);
/// A block still held after this long is dropped (and forgotten, so a
/// later announcement can bring it back).
pub const MAX_HOLD: Duration = Duration::from_secs(300);
/// Canonical blocks walked back from a new head when announcing.
pub const MAX_ANNOUNCE_WALK: usize = 64;

/// The node-side capabilities the service needs. Implemented over reth's
/// provider, engine handle, and network by [`super::RethGossipBackend`];
/// mocked in tests.
pub trait GossipBackend: Send + Sync + 'static {
    /// The local canonical head `(height, hash)`.
    fn head(&self) -> Option<(u64, B256)>;
    /// The canonical block hash at `height`.
    fn canonical_hash(&self, height: u64) -> Option<B256>;
    /// Whether the block is known locally (canonical or in-memory chain).
    fn has_block(&self, hash: B256) -> bool;
    /// The block's RLP, if known locally.
    fn block_rlp(&self, hash: B256) -> Option<Bytes>;
    /// Submit a block to the local engine (`new_payload`, in-process).
    /// Never a forkchoice update.
    fn submit(
        &self,
        block: SealedBlock<reth_ethereum::Block>,
    ) -> impl Future<Output = Result<PayloadStatusEnum, String>> + Send;
    /// Reputation hit for a peer that sent a block the engine judged
    /// `INVALID`. The only reputation change the service ever makes.
    fn penalize_invalid_block(&self, peer: PeerId);
}

/// A connected `sova/1` peer.
#[derive(Debug)]
struct PeerState {
    conn_id: ConnectionId,
    outbound: mpsc::Sender<SovaMessage>,
    window_start: Instant,
    announces: u32,
    get_blocks: u32,
}

impl PeerState {
    fn roll_window(&mut self, now: Instant) {
        if now.duration_since(self.window_start) >= Duration::from_secs(1) {
            self.window_start = now;
            self.announces = 0;
            self.get_blocks = 0;
        }
    }
}

/// An outstanding `GetBlock`.
#[derive(Debug, Clone, Copy)]
struct InFlight {
    peer: PeerId,
    sent_at: Instant,
    /// 0 for an announced block, `n` for its `n`-th ancestor.
    depth: u32,
}

/// A `SYNCING` block awaiting its parent.
#[derive(Debug, Clone)]
struct Orphan {
    parent: B256,
    block: SealedBlock<reth_ethereum::Block>,
    rlp: Bytes,
    peer: PeerId,
    depth: u32,
}

/// A block the engine held (see [`MAX_HELD`]).
#[derive(Debug, Clone)]
struct Held {
    block: SealedBlock<reth_ethereum::Block>,
    rlp: Bytes,
    peer: PeerId,
    depth: u32,
    since: Instant,
    last_try: Instant,
}

/// A block to submit.
#[derive(Debug)]
struct Pending {
    peer: PeerId,
    block: SealedBlock<reth_ethereum::Block>,
    rlp: Bytes,
    depth: u32,
}

/// The gossip service state. Drive it with [`GossipService::run`].
pub struct GossipService<B> {
    backend: B,
    peers: HashMap<PeerId, PeerState>,
    seen: LruMap<B256, ()>,
    announced: LruMap<B256, ()>,
    recent_blocks: LruMap<B256, Bytes>,
    orphans: LruMap<B256, Orphan>,
    held: LruMap<B256, Held>,
    in_flight: HashMap<B256, InFlight>,
    last_head: Option<B256>,
}

impl<B> std::fmt::Debug for GossipService<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GossipService")
            .field("peers", &self.peers.len())
            .field("in_flight", &self.in_flight.len())
            .field("orphans", &self.orphans.len())
            .finish_non_exhaustive()
    }
}

impl<B: GossipBackend> GossipService<B> {
    /// A fresh service over `backend`.
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            peers: HashMap::new(),
            seen: LruMap::new(ByLength::new(SEEN_CAPACITY)),
            announced: LruMap::new(ByLength::new(ANNOUNCED_CAPACITY)),
            recent_blocks: LruMap::new(ByLength::new(RECENT_BLOCKS_CAPACITY)),
            orphans: LruMap::new(ByLength::new(MAX_ORPHANS)),
            held: LruMap::new(ByLength::new(MAX_HELD)),
            in_flight: HashMap::new(),
            last_head: None,
        }
    }

    /// Runs the service until every connection-side sender is gone.
    ///
    /// `head_poll` is how often the canonical head is checked for blocks
    /// to announce (the same watch-the-head approach as gossip v1's relay,
    /// `crate::relay`).
    pub async fn run(mut self, mut rx: ServiceReceivers, head_poll: Duration) {
        let mut head_tick = tokio::time::interval(head_poll);
        head_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut housekeeping = tokio::time::interval(Duration::from_secs(1));
        housekeeping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut held_tick = tokio::time::interval(HELD_POLL);
        held_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                ev = rx.lifecycle.recv() => match ev {
                    Some(ev) => self.handle_event(ev, Instant::now()).await,
                    None => break,
                },
                ev = rx.messages.recv() => match ev {
                    Some(ev) => self.handle_event(ev, Instant::now()).await,
                    None => break,
                },
                _ = head_tick.tick() => self.on_head_tick(),
                _ = housekeeping.tick() => self.expire_requests(Instant::now()),
                _ = held_tick.tick() => self.retry_held(Instant::now()).await,
            }
        }
        tracing::warn!("sova/1: gossip service stopped (connection channels closed)");
    }

    /// Handles one connection event.
    pub(crate) async fn handle_event(&mut self, ev: PeerEvent, now: Instant) {
        match ev {
            PeerEvent::Connected {
                peer_id,
                conn_id,
                outbound,
            } => {
                // Greet with our head so a peer that missed announcements
                // (fresh start, reconnect) can pull the tip and, via
                // SYNCING, up to MAX_ANCESTOR_DEPTH ancestors.
                if let Some((height, hash)) = self.backend.head()
                    && height > 0
                {
                    let _ = outbound.try_send(SovaMessage::Announce(Announce { height, hash }));
                }
                self.peers.insert(
                    peer_id,
                    PeerState {
                        conn_id,
                        outbound,
                        window_start: now,
                        announces: 0,
                        get_blocks: 0,
                    },
                );
                tracing::info!(%peer_id, peers = self.peers.len(), "sova/1: peer active");
            }
            PeerEvent::Disconnected { peer_id, conn_id } => {
                if self
                    .peers
                    .get(&peer_id)
                    .is_some_and(|p| p.conn_id == conn_id)
                {
                    self.peers.remove(&peer_id);
                    tracing::info!(%peer_id, peers = self.peers.len(), "sova/1: peer gone");
                }
            }
            PeerEvent::Message { peer_id, msg } => self.handle_message(peer_id, msg, now).await,
        }
    }

    async fn handle_message(&mut self, peer: PeerId, msg: SovaMessage, now: Instant) {
        let Some(state) = self.peers.get_mut(&peer) else {
            // A message racing its connection's teardown.
            return;
        };
        state.roll_window(now);
        match msg {
            SovaMessage::Announce(a) => {
                state.announces += 1;
                if state.announces > MAX_ANNOUNCES_PER_SEC {
                    tracing::debug!(%peer, "sova/1: announce budget exceeded; dropped");
                    return;
                }
                self.on_announce(peer, a, now);
            }
            SovaMessage::GetBlock(g) => {
                state.get_blocks += 1;
                if state.get_blocks > MAX_GET_BLOCKS_PER_SEC {
                    tracing::debug!(%peer, "sova/1: get-block budget exceeded; dropped");
                    return;
                }
                self.on_get_block(peer, g);
            }
            SovaMessage::Block(rlp) => self.on_block(peer, rlp).await,
        }
    }

    fn on_announce(&mut self, peer: PeerId, a: Announce, now: Instant) {
        if self.seen.peek(&a.hash).is_some() || self.in_flight.contains_key(&a.hash) {
            return;
        }
        if self.backend.has_block(a.hash) {
            self.seen.insert(a.hash, ());
            return;
        }
        let local = self.backend.head().map_or(0, |(h, _)| h);
        if a.height > local.saturating_add(u64::from(MAX_ANCESTOR_DEPTH) + 1) {
            tracing::info!(
                %peer,
                announced = a.height,
                local,
                "sova/1: peer is beyond p2p catch-up range; handing the tip to the sync driver"
            );
            crate::candidates::request_sync(crate::candidates::SyncTarget {
                sova_height: a.height,
                block_hash: a.hash.0,
            });
            return;
        }
        self.request(peer, a.hash, 0, now);
    }

    /// Sends a `GetBlock` for `hash` to `peer`, respecting the in-flight
    /// cap. Returns whether a request went out.
    fn request(&mut self, peer: PeerId, hash: B256, depth: u32, now: Instant) -> bool {
        if self.in_flight.len() >= MAX_IN_FLIGHT {
            tracing::debug!(%peer, %hash, "sova/1: in-flight cap reached; not fetching");
            return false;
        }
        let Some(state) = self.peers.get(&peer) else {
            return false;
        };
        if state
            .outbound
            .try_send(SovaMessage::GetBlock(GetBlock { hash }))
            .is_err()
        {
            return false;
        }
        self.in_flight.insert(
            hash,
            InFlight {
                peer,
                sent_at: now,
                depth,
            },
        );
        true
    }

    fn on_get_block(&mut self, peer: PeerId, g: GetBlock) {
        let rlp = match self.recent_blocks.peek(&g.hash) {
            Some(rlp) => Some(rlp.clone()),
            None => self.backend.block_rlp(g.hash),
        };
        let Some(rlp) = rlp else {
            tracing::debug!(%peer, hash = %g.hash, "sova/1: requested block unknown here");
            return;
        };
        if rlp.len() > MAX_BLOCK_BYTES {
            tracing::warn!(hash = %g.hash, bytes = rlp.len(), "sova/1: block too large to serve");
            return;
        }
        if let Some(state) = self.peers.get(&peer) {
            let _ = state.outbound.try_send(SovaMessage::Block(rlp));
        }
    }

    async fn on_block(&mut self, peer: PeerId, rlp: Bytes) {
        let block = match reth_ethereum::Block::decode(&mut rlp.as_ref()) {
            Ok(block) => SealedBlock::seal_slow(block),
            Err(err) => {
                tracing::debug!(%peer, %err, "sova/1: undecodable block; dropped");
                return;
            }
        };
        let hash = block.hash();
        // Pull, not push: only a block we asked *this* peer for.
        match self.in_flight.get(&hash) {
            Some(req) if req.peer == peer => {}
            _ => {
                tracing::debug!(%peer, %hash, "sova/1: unsolicited block; dropped");
                return;
            }
        }
        let Some(req) = self.in_flight.remove(&hash) else {
            return;
        };
        self.seen.insert(hash, ());
        self.submit_chain(Pending {
            peer,
            block,
            rlp,
            depth: req.depth,
        })
        .await;
    }

    /// Submits a block, and — whenever one is accepted — any parked
    /// orphans that were waiting on it.
    async fn submit_chain(&mut self, first: Pending) {
        let mut queue = VecDeque::from([first]);
        while let Some(Pending {
            peer,
            block,
            rlp,
            depth,
        }) = queue.pop_front()
        {
            let hash = block.hash();
            let height = block.number;
            let parent = block.parent_hash;
            // Ahead of our own Zcash scan: consensus would hold it, so park
            // it without a round trip (and without the engine's invalid-
            // block logging) until the scan catches up.
            if crate::expectations::global()
                .scanned_through()
                .is_some_and(|scanned| height > scanned)
            {
                tracing::debug!(%peer, height, %hash, "sova/1: block ahead of our zcash scan; parked");
                self.park_held(peer, block, rlp, depth);
                continue;
            }
            let status = self.backend.submit(block.clone()).await;
            match status {
                Ok(PayloadStatusEnum::Valid | PayloadStatusEnum::Accepted) => {
                    tracing::info!(%peer, height, %hash, "sova/1: peer block accepted");
                    self.recent_blocks.insert(hash, rlp);
                    self.announce(height, hash, Some(peer));
                    let children: Vec<B256> = self
                        .orphans
                        .iter()
                        .filter(|(_, o)| o.parent == hash)
                        .map(|(child, _)| *child)
                        .collect();
                    for child in children {
                        if let Some(o) = self.orphans.remove(&child) {
                            queue.push_back(Pending {
                                peer: o.peer,
                                block: o.block,
                                rlp: o.rlp,
                                depth: o.depth,
                            });
                        }
                    }
                }
                Ok(PayloadStatusEnum::Syncing) => {
                    // Parent unknown: NOT misbehaviour. Park, chase parent.
                    self.orphans.insert(
                        hash,
                        Orphan {
                            parent,
                            block,
                            rlp,
                            peer,
                            depth,
                        },
                    );
                    self.chase_parent(peer, height, hash, parent, depth);
                }
                Ok(PayloadStatusEnum::Invalid { validation_error })
                    if validation_error.contains(crate::consensus::HOLD_MARKER) =>
                {
                    // Held, not bad: our zebrad hasn't reached (or is on
                    // another fork at) the block's Zcash epoch. Not the
                    // peer's fault; park it and resubmit once our scan
                    // catches up (`retry_held`).
                    tracing::info!(%peer, height, %hash, %validation_error, "sova/1: block held");
                    self.park_held(peer, block, rlp, depth);
                }
                Ok(PayloadStatusEnum::Invalid { validation_error }) => {
                    tracing::warn!(
                        %peer,
                        height,
                        %hash,
                        %validation_error,
                        "sova/1: peer sent an INVALID block; reputation hit"
                    );
                    self.backend.penalize_invalid_block(peer);
                    // Descendants parked on it can never connect.
                    self.drop_orphans_of(hash);
                }
                Err(err) => {
                    // Local engine trouble, not the peer's fault (e.g. a
                    // block that reached the SIP-4 precompile before our
                    // index had its anchor): park it for a retry.
                    tracing::warn!(%peer, height, %hash, %err, "sova/1: engine submit failed");
                    self.park_held(peer, block, rlp, depth);
                }
            }
        }
    }

    fn park_held(
        &mut self,
        peer: PeerId,
        block: SealedBlock<reth_ethereum::Block>,
        rlp: Bytes,
        depth: u32,
    ) {
        let now = Instant::now();
        let hash = block.hash();
        let since = self.held.peek(&hash).map_or(now, |h| h.since);
        self.held.insert(
            hash,
            Held {
                block,
                rlp,
                peer,
                depth,
                since,
                last_try: now,
            },
        );
    }

    /// Resubmits held blocks whose Zcash epoch our follower has now
    /// scanned, or that haven't been tried for [`HOLD_RETRY`]; drops those
    /// held longer than [`MAX_HOLD`].
    pub(crate) async fn retry_held(&mut self, now: Instant) {
        let scanned = crate::expectations::global().scanned_through();
        let mut due = Vec::new();
        let mut expired = Vec::new();
        for (hash, h) in self.held.iter() {
            if now.duration_since(h.since) >= MAX_HOLD {
                expired.push(*hash);
            } else if scanned.is_some_and(|s| s >= h.block.number)
                || now.duration_since(h.last_try) >= HOLD_RETRY
            {
                due.push(*hash);
            }
        }
        for hash in expired {
            self.held.remove(&hash);
            self.seen.remove(&hash);
            tracing::info!(%hash, "sova/1: held block expired; forgotten");
        }
        for hash in due {
            let Some(h) = self.held.remove(&hash) else {
                continue;
            };
            if self.backend.has_block(hash) {
                continue;
            }
            // Keep the original hold time across the resubmission.
            let since = h.since;
            self.submit_chain(Pending {
                peer: h.peer,
                block: h.block,
                rlp: h.rlp,
                depth: h.depth,
            })
            .await;
            if let Some(again) = self.held.get(&hash) {
                again.since = since;
            }
        }
    }

    fn chase_parent(&mut self, peer: PeerId, height: u64, hash: B256, parent: B256, depth: u32) {
        if self.seen.peek(&parent).is_some() || self.in_flight.contains_key(&parent) {
            // Already fetched/parked or on its way.
            return;
        }
        if self.backend.has_block(parent) {
            tracing::debug!(
                height,
                %hash,
                "sova/1: engine is syncing although the parent is known; waiting"
            );
            return;
        }
        if depth >= MAX_ANCESTOR_DEPTH {
            tracing::info!(
                %peer,
                height,
                %hash,
                depth,
                "sova/1: ancestor gap deeper than p2p catch-up; handing it to the sync driver"
            );
            crate::candidates::request_sync(crate::candidates::SyncTarget {
                sova_height: height,
                block_hash: hash.0,
            });
            return;
        }
        tracing::info!(%peer, height, %hash, %parent, "sova/1: parent unknown (SYNCING); fetching it");
        let _ = self.request(peer, parent, depth + 1, Instant::now());
    }

    fn drop_orphans_of(&mut self, invalid: B256) {
        let mut doomed = vec![invalid];
        while let Some(bad) = doomed.pop() {
            let children: Vec<B256> = self
                .orphans
                .iter()
                .filter(|(_, o)| o.parent == bad)
                .map(|(child, _)| *child)
                .collect();
            for child in children {
                self.orphans.remove(&child);
                doomed.push(child);
            }
        }
    }

    /// Announces `hash` to every peer except `except`, once per hash.
    fn announce(&mut self, height: u64, hash: B256, except: Option<PeerId>) {
        if self.announced.peek(&hash).is_some() {
            return;
        }
        self.announced.insert(hash, ());
        self.seen.insert(hash, ());
        let msg = SovaMessage::Announce(Announce { height, hash });
        let mut sent = 0usize;
        for (id, state) in &self.peers {
            if Some(*id) == except {
                continue;
            }
            if state.outbound.try_send(msg.clone()).is_ok() {
                sent += 1;
            }
        }
        tracing::info!(height, %hash, peers = sent, "sova/1: announced");
    }

    /// Announces every canonical block not yet announced, oldest first,
    /// walking back at most [`MAX_ANNOUNCE_WALK`] blocks from the head.
    pub(crate) fn on_head_tick(&mut self) {
        let Some((head_height, head_hash)) = self.backend.head() else {
            return;
        };
        if self.last_head == Some(head_hash) {
            return;
        }
        self.last_head = Some(head_hash);
        let mut fresh = Vec::new();
        let (mut height, mut hash) = (head_height, head_hash);
        while height > 0 && fresh.len() < MAX_ANNOUNCE_WALK {
            if self.announced.peek(&hash).is_some() {
                break;
            }
            fresh.push((height, hash));
            height -= 1;
            match self.backend.canonical_hash(height) {
                Some(h) => hash = h,
                None => break,
            }
        }
        for (height, hash) in fresh.into_iter().rev() {
            self.announce(height, hash, None);
        }
    }

    fn expire_requests(&mut self, now: Instant) {
        self.in_flight
            .retain(|_, req| now.duration_since(req.sent_at) < REQUEST_TIMEOUT);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use alloy_rlp::Encodable;

    use super::*;

    #[derive(Default)]
    struct MockState {
        head: Option<(u64, B256)>,
        canonical: HashMap<u64, B256>,
        known: HashMap<B256, Bytes>,
        statuses: VecDeque<PayloadStatusEnum>,
        submitted: Vec<B256>,
        penalized: Vec<PeerId>,
    }

    #[derive(Clone, Default)]
    struct Mock(Arc<Mutex<MockState>>);

    impl Mock {
        fn with<R>(&self, f: impl FnOnce(&mut MockState) -> R) -> R {
            let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut guard)
        }
    }

    impl GossipBackend for Mock {
        fn head(&self) -> Option<(u64, B256)> {
            self.with(|s| s.head)
        }
        fn canonical_hash(&self, height: u64) -> Option<B256> {
            self.with(|s| s.canonical.get(&height).copied())
        }
        fn has_block(&self, hash: B256) -> bool {
            self.with(|s| s.known.contains_key(&hash))
        }
        fn block_rlp(&self, hash: B256) -> Option<Bytes> {
            self.with(|s| s.known.get(&hash).cloned())
        }
        async fn submit(
            &self,
            block: SealedBlock<reth_ethereum::Block>,
        ) -> Result<PayloadStatusEnum, String> {
            self.with(|s| {
                s.submitted.push(block.hash());
                Ok(s.statuses.pop_front().unwrap_or(PayloadStatusEnum::Valid))
            })
        }
        fn penalize_invalid_block(&self, peer: PeerId) {
            self.with(|s| s.penalized.push(peer));
        }
    }

    fn block(number: u64, parent: B256) -> (SealedBlock<reth_ethereum::Block>, Bytes) {
        let mut b = reth_ethereum::Block::default();
        b.header.number = number;
        b.header.parent_hash = parent;
        let mut rlp = Vec::new();
        b.encode(&mut rlp);
        (SealedBlock::seal_slow(b), Bytes::from(rlp))
    }

    struct Harness {
        svc: GossipService<Mock>,
        mock: Mock,
        now: Instant,
    }

    impl Harness {
        fn new() -> Self {
            let mock = Mock::default();
            mock.with(|s| s.head = Some((10, B256::repeat_byte(10))));
            Self {
                svc: GossipService::new(mock.clone()),
                mock,
                now: Instant::now(),
            }
        }

        async fn connect(&mut self, byte: u8) -> (PeerId, mpsc::Receiver<SovaMessage>) {
            let peer = PeerId::repeat_byte(byte);
            let (tx, mut rx) = mpsc::channel(64);
            self.svc
                .handle_event(
                    PeerEvent::Connected {
                        peer_id: peer,
                        conn_id: u64::from(byte),
                        outbound: tx,
                    },
                    self.now,
                )
                .await;
            // Drain the greeting (our head).
            assert_eq!(
                rx.try_recv().ok(),
                Some(SovaMessage::Announce(Announce {
                    height: 10,
                    hash: B256::repeat_byte(10)
                }))
            );
            (peer, rx)
        }

        async fn msg(&mut self, peer: PeerId, msg: SovaMessage) {
            self.svc
                .handle_event(PeerEvent::Message { peer_id: peer, msg }, self.now)
                .await;
        }
    }

    fn drain(rx: &mut mpsc::Receiver<SovaMessage>) -> Vec<SovaMessage> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn announce(height: u64, hash: B256) -> SovaMessage {
        SovaMessage::Announce(Announce { height, hash })
    }

    fn get(hash: B256) -> SovaMessage {
        SovaMessage::GetBlock(GetBlock { hash })
    }

    #[tokio::test]
    async fn announce_is_fetched_once_and_known_blocks_not_at_all() {
        let mut h = Harness::new();
        let (p1, mut rx1) = h.connect(1).await;
        let (p2, mut rx2) = h.connect(2).await;
        let (b, _) = block(11, B256::repeat_byte(10));

        h.msg(p1, announce(11, b.hash())).await;
        h.msg(p1, announce(11, b.hash())).await;
        h.msg(p2, announce(11, b.hash())).await;
        assert_eq!(drain(&mut rx1), vec![get(b.hash())]);
        assert!(drain(&mut rx2).is_empty(), "in-flight hash re-requested");

        let known = B256::repeat_byte(0x77);
        h.mock.with(|s| s.known.insert(known, Bytes::new()));
        h.msg(p2, announce(11, known)).await;
        assert!(
            drain(&mut rx2).is_empty(),
            "fetched a block we already have"
        );
    }

    #[tokio::test]
    async fn accepted_block_is_submitted_and_reannounced_to_others_only() {
        let mut h = Harness::new();
        let (p1, mut rx1) = h.connect(1).await;
        let (_p2, mut rx2) = h.connect(2).await;
        let (b, rlp) = block(11, B256::repeat_byte(10));

        h.msg(p1, announce(11, b.hash())).await;
        drain(&mut rx1);
        h.msg(p1, SovaMessage::Block(rlp.clone())).await;
        assert_eq!(h.mock.with(|s| s.submitted.clone()), vec![b.hash()]);
        assert!(drain(&mut rx1).is_empty(), "echoed back to the source");
        assert_eq!(drain(&mut rx2), vec![announce(11, b.hash())]);

        // A duplicate delivery is unsolicited now: not resubmitted.
        h.msg(p1, SovaMessage::Block(rlp)).await;
        assert_eq!(h.mock.with(|s| s.submitted.len()), 1);
        // And it's servable to the peer we re-announced to.
        h.msg(_p2, get(b.hash())).await;
        assert!(matches!(
            drain(&mut rx2).as_slice(),
            [SovaMessage::Block(_)]
        ));
    }

    #[tokio::test]
    async fn unsolicited_blocks_are_dropped() {
        let mut h = Harness::new();
        let (p1, _rx1) = h.connect(1).await;
        let (p2, _rx2) = h.connect(2).await;
        let (b, rlp) = block(11, B256::repeat_byte(10));
        // Never requested at all.
        h.msg(p1, SovaMessage::Block(rlp.clone())).await;
        // Requested from p1, delivered by p2.
        h.msg(p1, announce(11, b.hash())).await;
        h.msg(p2, SovaMessage::Block(rlp)).await;
        assert!(h.mock.with(|s| s.submitted.is_empty()));
    }

    #[tokio::test]
    async fn syncing_is_not_misbehaviour_it_fetches_the_parent_then_resubmits() {
        let mut h = Harness::new();
        let (p1, mut rx1) = h.connect(1).await;
        let (parent, parent_rlp) = block(11, B256::repeat_byte(10));
        let (child, child_rlp) = block(12, parent.hash());

        h.mock.with(|s| {
            s.statuses = VecDeque::from([
                PayloadStatusEnum::Syncing, // child: parent unknown
                PayloadStatusEnum::Valid,   // parent
                PayloadStatusEnum::Valid,   // child, resubmitted
            ]);
        });
        h.msg(p1, announce(12, child.hash())).await;
        assert_eq!(drain(&mut rx1), vec![get(child.hash())]);
        h.msg(p1, SovaMessage::Block(child_rlp)).await;

        // No reputation hit; the parent is requested from the same peer.
        assert!(h.mock.with(|s| s.penalized.is_empty()));
        assert_eq!(drain(&mut rx1), vec![get(parent.hash())]);

        h.msg(p1, SovaMessage::Block(parent_rlp)).await;
        assert_eq!(
            h.mock.with(|s| s.submitted.clone()),
            vec![child.hash(), parent.hash(), child.hash()]
        );
        assert!(h.mock.with(|s| s.penalized.is_empty()));
    }

    #[tokio::test]
    async fn ancestor_chase_is_bounded() {
        let mut h = Harness::new();
        // Head far below: allow the announce, then answer SYNCING forever.
        h.mock.with(|s| {
            s.head = Some((10, B256::repeat_byte(10)));
            s.statuses = VecDeque::from(vec![
                PayloadStatusEnum::Syncing;
                (MAX_ANCESTOR_DEPTH + 5) as usize
            ]);
        });
        let (p1, mut rx1) = h.connect(1).await;
        // A chain of 40 blocks whose ancestry never reaches us.
        let mut chain = Vec::new();
        let mut parent = B256::repeat_byte(0xEE);
        for n in 0..40u64 {
            let (b, rlp) = block(100 + n, parent);
            parent = b.hash();
            chain.push((b, rlp));
        }
        let by_hash: HashMap<B256, Bytes> =
            chain.iter().map(|(b, r)| (b.hash(), r.clone())).collect();
        let tip = chain.last().map(|(b, _)| b.hash()).unwrap_or_default();
        // Pretend the head is close enough to accept the announcement.
        h.mock.with(|s| s.head = Some((139, B256::repeat_byte(1))));
        h.msg(p1, announce(139, tip)).await;
        let mut fetches = 0;
        while let Some(SovaMessage::GetBlock(GetBlock { hash })) = drain(&mut rx1).first().cloned()
        {
            fetches += 1;
            let Some(rlp) = by_hash.get(&hash) else { break };
            h.msg(p1, SovaMessage::Block(rlp.clone())).await;
        }
        // The tip plus exactly MAX_ANCESTOR_DEPTH ancestors.
        assert_eq!(fetches, MAX_ANCESTOR_DEPTH + 1);
        assert!(h.mock.with(|s| s.penalized.is_empty()));
    }

    #[tokio::test]
    async fn only_invalid_costs_reputation() {
        let mut h = Harness::new();
        let (p1, mut rx1) = h.connect(1).await;
        let (b, rlp) = block(11, B256::repeat_byte(10));
        h.mock.with(|s| {
            s.statuses = VecDeque::from([PayloadStatusEnum::Invalid {
                validation_error: "settlement mismatch".into(),
            }]);
        });
        h.msg(p1, announce(11, b.hash())).await;
        drain(&mut rx1);
        h.msg(p1, SovaMessage::Block(rlp)).await;
        assert_eq!(h.mock.with(|s| s.penalized.clone()), vec![p1]);
        // Never re-fetched, never re-announced.
        h.msg(p1, announce(11, b.hash())).await;
        assert!(drain(&mut rx1).is_empty());
    }

    #[tokio::test]
    async fn a_hold_costs_no_reputation_and_can_be_retried() {
        let mut h = Harness::new();
        let (p1, mut rx1) = h.connect(1).await;
        let (b, rlp) = block(11, B256::repeat_byte(10));
        h.mock.with(|s| {
            s.statuses = VecDeque::from([PayloadStatusEnum::Invalid {
                validation_error: format!(
                    "{}: zcash anchor mismatch at height 11",
                    crate::consensus::HOLD_MARKER
                ),
            }]);
        });
        h.msg(p1, announce(11, b.hash())).await;
        drain(&mut rx1);
        h.msg(p1, SovaMessage::Block(rlp)).await;
        assert!(h.mock.with(|s| s.penalized.is_empty()));
        // Parked, not forgotten: a re-announcement doesn't refetch it...
        h.msg(p1, announce(11, b.hash())).await;
        assert!(drain(&mut rx1).is_empty());
        // ...nothing is retried before HOLD_RETRY...
        h.svc.retry_held(Instant::now()).await;
        assert_eq!(h.mock.with(|s| s.submitted.len()), 1);
        // ...then it is resubmitted, accepted, and re-announced onward.
        h.svc.retry_held(Instant::now() + HOLD_RETRY).await;
        assert_eq!(
            h.mock.with(|s| s.submitted.clone()),
            vec![b.hash(), b.hash()]
        );
        assert!(h.mock.with(|s| s.penalized.is_empty()));
    }

    #[tokio::test]
    async fn a_block_held_too_long_is_forgotten() {
        let mut h = Harness::new();
        let (p1, mut rx1) = h.connect(1).await;
        let (b, rlp) = block(11, B256::repeat_byte(10));
        let hold = || PayloadStatusEnum::Invalid {
            validation_error: format!("{}: not scanned", crate::consensus::HOLD_MARKER),
        };
        h.mock.with(|s| s.statuses = VecDeque::from([hold()]));
        h.msg(p1, announce(11, b.hash())).await;
        drain(&mut rx1);
        h.msg(p1, SovaMessage::Block(rlp)).await;
        h.svc.retry_held(Instant::now() + MAX_HOLD).await;
        assert_eq!(
            h.mock.with(|s| s.submitted.len()),
            1,
            "expired, not retried"
        );
        // Forgotten, so a later announcement brings it back.
        h.msg(p1, announce(11, b.hash())).await;
        assert_eq!(
            drain(&mut rx1),
            vec![SovaMessage::GetBlock(GetBlock { hash: b.hash() })]
        );
    }

    #[tokio::test]
    async fn head_changes_are_announced_once_oldest_first() {
        let mut h = Harness::new();
        let (_p1, mut rx1) = h.connect(1).await;
        let hashes: Vec<B256> = (0..=12u8).map(B256::repeat_byte).collect();
        h.mock.with(|s| {
            for (n, hash) in hashes.iter().enumerate() {
                s.canonical.insert(n as u64, *hash);
            }
            s.head = Some((10, hashes[10]));
        });
        h.svc.on_head_tick();
        let first = drain(&mut rx1);
        assert_eq!(first.len(), 10, "heights 1..=10 announced");
        assert_eq!(first[0], announce(1, hashes[1]));
        assert_eq!(first[9], announce(10, hashes[10]));

        h.svc.on_head_tick();
        assert!(drain(&mut rx1).is_empty(), "unchanged head re-announced");

        h.mock.with(|s| s.head = Some((12, hashes[12])));
        h.svc.on_head_tick();
        assert_eq!(
            drain(&mut rx1),
            vec![announce(11, hashes[11]), announce(12, hashes[12])]
        );

        // A same-height reorg (arbiter micro-reorg) announces the new block.
        let reorged = B256::repeat_byte(0xAB);
        h.mock.with(|s| {
            s.canonical.insert(12, reorged);
            s.head = Some((12, reorged));
        });
        h.svc.on_head_tick();
        assert_eq!(drain(&mut rx1), vec![announce(12, reorged)]);
    }

    #[tokio::test]
    async fn requests_expire_and_can_be_retried_elsewhere() {
        let mut h = Harness::new();
        let (p1, mut rx1) = h.connect(1).await;
        let (p2, mut rx2) = h.connect(2).await;
        let (b, _) = block(11, B256::repeat_byte(10));
        h.msg(p1, announce(11, b.hash())).await;
        assert_eq!(drain(&mut rx1), vec![get(b.hash())]);
        h.svc.expire_requests(h.now + REQUEST_TIMEOUT);
        h.msg(p2, announce(11, b.hash())).await;
        assert_eq!(drain(&mut rx2), vec![get(b.hash())]);
    }

    #[tokio::test]
    async fn announce_budget_is_enforced() {
        let mut h = Harness::new();
        let (p1, mut rx1) = h.connect(1).await;
        for i in 0..(MAX_ANNOUNCES_PER_SEC + 20) {
            let hash = B256::left_padding_from(&i.to_be_bytes());
            h.msg(p1, announce(11, hash)).await;
        }
        // Capped by both the per-second budget and the in-flight cap.
        let fetched = drain(&mut rx1).len();
        assert_eq!(fetched, MAX_IN_FLIGHT.min(MAX_ANNOUNCES_PER_SEC as usize));
    }

    #[tokio::test]
    async fn stale_disconnect_does_not_drop_a_reconnected_peer() {
        let mut h = Harness::new();
        let (p1, _rx_old) = h.connect(1).await;
        let (tx, mut rx_new) = mpsc::channel(64);
        h.svc
            .handle_event(
                PeerEvent::Connected {
                    peer_id: p1,
                    conn_id: 99,
                    outbound: tx,
                },
                h.now,
            )
            .await;
        drain(&mut rx_new);
        h.svc
            .handle_event(
                PeerEvent::Disconnected {
                    peer_id: p1,
                    conn_id: 1,
                },
                h.now,
            )
            .await;
        let (b, _) = block(11, B256::repeat_byte(10));
        h.msg(p1, announce(11, b.hash())).await;
        assert_eq!(drain(&mut rx_new), vec![get(b.hash())]);
    }
}
