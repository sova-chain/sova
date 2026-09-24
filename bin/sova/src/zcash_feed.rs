//! SIP-7 §4.2 path A: the node's Zcash block feed (`sova` namespace).
//!
//! Two methods, registered only when SIP-7 is active (`SOVA_SIP7=1`):
//!
//! - `sova_getZcashBlocks(fromHeight, toHeight)`: the summaries of the Zcash
//!   heights in `fromHeight ..= toHeight` (inclusive, at most
//!   [`MAX_RANGE`] heights per call) that the canonical Sova chain anchors.
//! - `sova_subscribe("zcashBlocks")` (WS only), notifications on
//!   `sova_subscription`, cancelled with `sova_unsubscribe`: one item per
//!   Zcash height, in order, as soon as the canonical Sova head anchors it.
//!   When a Sova or Zcash reorg invalidates heights already sent, a
//!   `{"rollback": {"toHeight": N}}` item comes first (everything above `N`
//!   is void, like `removed: true` on eth logs), then the new branch's items
//!   from `N + 1`. An optional second parameter `{"fromHeight": h}` replays
//!   from `h` (at most [`MAX_RANGE`] below the current anchored height).
//!
//! **Anchored** means SIP-4's rule: Sova block `N` anchors Zcash height
//! `E_N = N + B − 1` and commits to that block's hash in
//! `parent_beacon_block_root`. Height `h` is served only when all of these
//! hold, so the feed says exactly what the canonical Sova chain says:
//!
//! 1. `B <= h <= E_head`, where `head` is the *effective* canonical head
//!    (stale blocks above a Zcash rollback floor excluded, the same view the
//!    sealer and arbiter use);
//! 2. the node's Zcash index has the block and its SIP-7 summary;
//! 3. canonical Sova block `h − B + 1` commits to the indexed hash.
//!
//! A range answer is the contiguous prefix of the request that satisfies
//! them (it stops at the first height that doesn't), so a client can page
//! with `fromHeight = last + 1`.
//!
//! The subscription is a poll (every [`POLL_INTERVAL`]) of the head and a
//! diff against what this subscriber was already sent ([`Cursor::step`],
//! a pure function): no consensus hooks. Zcash block hashes chain, so if
//! the last height sent still carries the same hash, everything below it
//! does too; only a mismatch walks back to find the fork.
//!
//! Item shape (zatoshi amounts as decimal strings, deltas signed, pool
//! delta = value **into** the pool, SIP-7 §2; `hash` is the Zcash block
//! hash in display order, i.e. what zebrad prints, 0x-prefixed, the same
//! bytes32 the 0x…5A00 precompile returns):
//!
//! ```json
//! {"height": 4384200, "hash": "0x0080…c2bc", "time": 1790184215, "sovaBlock": 4384200,
//!  "pools": {"transparent": "1573837835978306", "sprout": "…", "sapling": "…",
//!            "orchard": "…", "lockbox": "…", "ironwood": "…"},
//!  "chainSupply": "1823100637835043",
//!  "deltas": {"transparent": "12500000", …, "ironwood": "125000000"},
//!  "stats": {"txCount": 1, "shieldedTxCount": 1, "tIn": 0, "tOut": 1, "saplingSpends": 0,
//!            "saplingOutputs": 0, "orchardActions": 0, "ironwoodActions": 2, "joinSplits": 0},
//!  "trees": {"sapling": 404304, "orchard": 248902, "ironwood": 354039}}
//! ```

use std::{collections::VecDeque, sync::Arc, time::Duration};

use consensus::pools::{BlockPools, BlockStats, POOL_IDS};
use engine::zcash_index::ZcashIndex;
use evm::zcash::{BlockSummary, ZcashSource};
use jsonrpsee::{
    PendingSubscriptionSink, RpcModule, SubscriptionMessage,
    types::{ErrorObjectOwned, Params},
};
use serde_json::{Map, Value, json};

/// Most heights one `sova_getZcashBlocks` call may span, and how far back a
/// subscription may replay.
pub(crate) const MAX_RANGE: u64 = 1_000;

/// How often a subscription re-reads the head.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Heights (and hashes) a subscription remembers to detect a reorg: far
/// deeper than any Zcash reorg Sova follows. Beyond it, the rollback goes
/// to the oldest remembered height.
pub(crate) const REMEMBER: usize = 1_024;

/// JSON-RPC "invalid params".
const INVALID_PARAMS: i32 = -32602;

/// The subscription's only kind.
pub(crate) const ZCASH_BLOCKS: &str = "zcashBlocks";

/// Everything the feed needs from the canonical Sova chain.
pub(crate) trait SovaChain: Send + Sync + 'static {
    /// The effective canonical head: `best_block_number`, minus any stale
    /// blocks above a pending Zcash rollback floor.
    fn head(&self) -> u64;
    /// Canonical block `number`'s SIP-4 anchor (`parent_beacon_block_root`).
    fn anchor(&self, number: u64) -> Option<[u8; 32]>;
}

/// One anchored Zcash block, as the feed serves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ZcashBlockItem {
    /// Zcash height.
    pub(crate) height: u64,
    /// The Sova block that anchors it (`height − B + 1`).
    pub(crate) sova_block: u64,
    /// Zcash block hash, display order.
    pub(crate) hash: [u8; 32],
    /// Zcash header time.
    pub(crate) time: u32,
    /// Pools and counters from the node's index.
    pub(crate) summary: BlockSummary,
}

/// The highest Zcash height the effective head `head` anchors that the
/// index also covers: `min(head + B − 1, indexed_through)`. `None` before
/// block 1 or with an empty index.
#[must_use]
pub(crate) fn anchored_through(head: u64, base: u64, indexed_through: Option<u64>) -> Option<u64> {
    if head == 0 {
        return None;
    }
    let e = head.checked_add(base)?.checked_sub(1)?;
    Some(e.min(indexed_through?))
}

/// Refuse a range that is backwards or wider than [`MAX_RANGE`].
pub(crate) fn check_range(from: u64, to: u64) -> Result<(), String> {
    if to < from {
        return Err(format!("toHeight {to} is below fromHeight {from}"));
    }
    if to - from >= MAX_RANGE {
        return Err(format!(
            "range {from}..={to} spans {} heights; at most {MAX_RANGE} per call",
            u128::from(to - from) + 1
        ));
    }
    Ok(())
}

/// The feed over the node's Zcash index and a view of the Sova chain.
#[derive(Debug)]
pub(crate) struct Feed<C> {
    chain: C,
    index: &'static ZcashIndex,
}

impl<C: SovaChain> Feed<C> {
    pub(crate) const fn new(chain: C, index: &'static ZcashIndex) -> Self {
        Self { chain, index }
    }

    fn base(&self) -> u64 {
        self.index.epoch_base()
    }

    /// Highest anchored-and-indexed Zcash height (condition 1 and the
    /// index's contiguous coverage; see [`anchored_through`]).
    pub(crate) fn horizon(&self) -> Option<u64> {
        anchored_through(self.chain.head(), self.base(), self.index.indexed_through())
    }

    /// The item at `height` if the canonical chain anchors it (conditions
    /// 2 and 3; the caller bounds `height` by [`Self::horizon`]).
    pub(crate) fn item(&self, height: u64) -> Option<ZcashBlockItem> {
        let base = self.base();
        let sova_block = height.checked_sub(base)?.checked_add(1)?;
        let (hash, time) = self.index.block(height)?;
        let summary = self.index.block_summary(height)?;
        if self.chain.anchor(sova_block)? != hash {
            return None;
        }
        Some(ZcashBlockItem {
            height,
            sova_block,
            hash,
            time,
            summary: *summary,
        })
    }

    /// `sova_getZcashBlocks`: the anchored prefix of `from ..= to`.
    pub(crate) fn range(&self, from: u64, to: u64) -> Result<Vec<ZcashBlockItem>, String> {
        check_range(from, to)?;
        let Some(top) = self.horizon() else {
            return Ok(Vec::new());
        };
        let start = from.max(self.base());
        let end = to.min(top);
        let mut out = Vec::new();
        for h in start..=end {
            match self.item(h) {
                Some(item) => out.push(item),
                None => break,
            }
        }
        Ok(out)
    }

    /// One subscription tick: what to send now.
    pub(crate) fn poll(&self, cursor: &mut Cursor) -> Vec<FeedEvent<ZcashBlockItem>> {
        let top = self.horizon();
        cursor.step(top, MAX_RANGE as usize, |h| {
            self.item(h).map(|item| (item.hash, item))
        })
    }

    /// A cursor for a new subscription: from `from_height` if given (at most
    /// [`MAX_RANGE`] below the horizon), else from the next height to be
    /// anchored.
    pub(crate) fn cursor(&self, from_height: Option<u64>) -> Result<Cursor, String> {
        let base = self.base();
        let top = self.horizon();
        let next = match from_height {
            None => top.map_or(base, |t| t + 1),
            Some(h) => {
                let h = h.max(base);
                let floor = top.map_or(base, |t| t.saturating_sub(MAX_RANGE - 1).max(base));
                if h < floor {
                    return Err(format!(
                        "fromHeight {h} is more than {MAX_RANGE} heights below the anchored height; backfill with sova_getZcashBlocks"
                    ));
                }
                h
            }
        };
        // Seed with the hash just below the start, so a reorg across the
        // starting point is reported like any other.
        let seed = next
            .checked_sub(1)
            .filter(|h| top.is_some_and(|t| *h <= t))
            .and_then(|h| self.item(h).map(|i| (h, i.hash)));
        Ok(Cursor::new(next, seed))
    }
}

/// What a subscription sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FeedEvent<T> {
    /// Every height above `to_height` that was sent is void.
    Rollback {
        /// Highest height still canonical.
        to_height: u64,
    },
    /// The next anchored block.
    Block(T),
}

/// A subscriber's position: the next height to send and the (height, hash)
/// of what it was recently sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cursor {
    next: u64,
    sent: VecDeque<(u64, [u8; 32])>,
}

impl Cursor {
    /// Start at `next`; `seed` is the (height, hash) just below it, if known.
    pub(crate) fn new(next: u64, seed: Option<(u64, [u8; 32])>) -> Self {
        Self {
            next,
            sent: seed.into_iter().collect(),
        }
    }

    /// The next height this cursor will send.
    #[cfg(test)]
    pub(crate) const fn next(&self) -> u64 {
        self.next
    }

    /// Diff the chain against what was sent. `top` is the anchored horizon
    /// now; `at(h)` is the canonical (hash, item) at `h <= top`, or `None`.
    /// Returns a rollback if anything remembered is no longer canonical,
    /// then up to `max_items` new items in height order.
    ///
    /// The seed (the height just below a subscription's start) counts as
    /// known to the subscriber, so a reorg across the starting point is
    /// reported too, even before anything was sent.
    pub(crate) fn step<T>(
        &mut self,
        top: Option<u64>,
        max_items: usize,
        at: impl Fn(u64) -> Option<([u8; 32], T)>,
    ) -> Vec<FeedEvent<T>> {
        let mut out = Vec::new();
        // Hashes chain: if the newest remembered height is still canonical,
        // all of them are. Otherwise walk back to the fork.
        let mut lowest_dropped = None;
        while let Some(&(h, hash)) = self.sent.back() {
            let canonical =
                top.is_some_and(|t| h <= t) && at(h).is_some_and(|(got, _)| got == hash);
            if canonical {
                break;
            }
            lowest_dropped = Some(h);
            self.sent.pop_back();
        }
        if let Some(lowest) = lowest_dropped {
            // A remembered height that survived is verified canonical. With
            // none left, nothing below the oldest one was checked: the fork
            // is at most the anchored top (heights above it aren't anchored),
            // and at most just below the oldest dropped height.
            let to_height = match (self.sent.back(), top) {
                (Some(&(h, _)), _) => h,
                (None, Some(t)) => t.min(lowest.saturating_sub(1)),
                (None, None) => lowest.saturating_sub(1),
            };
            out.push(FeedEvent::Rollback { to_height });
            self.next = to_height.saturating_add(1);
        }
        let Some(top) = top else {
            return out;
        };
        let mut sent_now = 0;
        while self.next <= top && sent_now < max_items {
            let Some((hash, item)) = at(self.next) else {
                break;
            };
            self.sent.push_back((self.next, hash));
            if self.sent.len() > REMEMBER {
                self.sent.pop_front();
            }
            out.push(FeedEvent::Block(item));
            self.next += 1;
            sent_now += 1;
        }
        out
    }
}

/// Decimal strings, by pool name.
fn by_pool<T: ToString>(values: &[T]) -> Value {
    let mut m = Map::new();
    for (id, v) in POOL_IDS.iter().zip(values) {
        m.insert((*id).to_owned(), Value::String(v.to_string()));
    }
    Value::Object(m)
}

fn stats_json(s: &BlockStats) -> Value {
    json!({
        "txCount": s.tx_count,
        "shieldedTxCount": s.shielded_tx_count,
        "tIn": s.t_in,
        "tOut": s.t_out,
        "saplingSpends": s.sapling_spends,
        "saplingOutputs": s.sapling_outputs,
        "orchardActions": s.orchard_actions,
        "ironwoodActions": s.ironwood_actions,
        "joinSplits": s.joinsplits,
    })
}

fn pools_json(p: &BlockPools) -> (Value, Value, Value) {
    (
        by_pool(&p.chain_value_zat),
        by_pool(&p.delta_zat),
        json!({
            "sapling": p.trees.sapling,
            "orchard": p.trees.orchard,
            "ironwood": p.trees.ironwood,
        }),
    )
}

/// The JSON of one item (see the module doc).
pub(crate) fn item_json(item: &ZcashBlockItem) -> Value {
    let (pools, deltas, trees) = pools_json(&item.summary.pools);
    json!({
        "height": item.height,
        "hash": format!("0x{}", hex::encode(item.hash)),
        "time": item.time,
        "sovaBlock": item.sova_block,
        "pools": pools,
        "chainSupply": item.summary.pools.chain_supply_zat.to_string(),
        "deltas": deltas,
        "stats": stats_json(&item.summary.stats),
        "trees": trees,
    })
}

/// The JSON of one subscription event.
pub(crate) fn event_json(event: &FeedEvent<ZcashBlockItem>) -> Value {
    match event {
        FeedEvent::Rollback { to_height } => json!({"rollback": {"toHeight": to_height}}),
        FeedEvent::Block(item) => item_json(item),
    }
}

/// A height parameter: a JSON number, a decimal string, or a 0x-hex
/// quantity (what EVM tooling sends).
pub(crate) fn parse_height(v: &Value, name: &str) -> Result<u64, String> {
    let bad = || format!("{name} must be a non-negative integer or 0x-hex quantity, got {v}");
    match v {
        Value::Number(n) => n.as_u64().ok_or_else(bad),
        Value::String(s) => match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            Some(hex) if !hex.is_empty() => u64::from_str_radix(hex, 16).map_err(|_| bad()),
            Some(_) => Err(bad()),
            None => s.parse().map_err(|_| bad()),
        },
        _ => Err(bad()),
    }
}

fn invalid(msg: impl Into<String>) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(INVALID_PARAMS, msg.into(), None::<()>)
}

/// Positional or named `(fromHeight, toHeight)`.
fn range_params(params: &Params<'_>) -> Result<(u64, u64), ErrorObjectOwned> {
    let value: Value = params
        .parse()
        .map_err(|_| invalid("expected [fromHeight, toHeight]"))?;
    let (from, to) = match &value {
        Value::Array(a) if a.len() == 2 => (&a[0], &a[1]),
        Value::Object(o) => match (o.get("fromHeight"), o.get("toHeight")) {
            (Some(f), Some(t)) => (f, t),
            _ => return Err(invalid("expected {fromHeight, toHeight}")),
        },
        _ => return Err(invalid("expected [fromHeight, toHeight]")),
    };
    Ok((
        parse_height(from, "fromHeight").map_err(invalid)?,
        parse_height(to, "toHeight").map_err(invalid)?,
    ))
}

/// `("zcashBlocks")` or `("zcashBlocks", {"fromHeight": h})`.
fn subscribe_params(params: &Params<'_>) -> Result<Option<u64>, String> {
    let value: Value = params
        .parse()
        .map_err(|_| format!("expected [\"{ZCASH_BLOCKS}\"]"))?;
    let items = match value {
        Value::Array(a) => a,
        _ => return Err(format!("expected [\"{ZCASH_BLOCKS}\"]")),
    };
    match items.first().and_then(Value::as_str) {
        Some(ZCASH_BLOCKS) => {}
        Some(other) => {
            return Err(format!(
                "unknown subscription {other:?}; only \"{ZCASH_BLOCKS}\""
            ));
        }
        None => return Err(format!("expected [\"{ZCASH_BLOCKS}\"]")),
    }
    match items.get(1) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(o)) => match o.get("fromHeight") {
            None | Some(Value::Null) => Ok(None),
            Some(h) => parse_height(h, "fromHeight").map(Some),
        },
        Some(other) => Err(format!(
            "options must be an object like {{\"fromHeight\": h}}, got {other}"
        )),
    }
}

/// The `sova` namespace. Merge it into every configured transport; the
/// subscription is only reachable where WS is on.
pub(crate) fn module<C: SovaChain>(feed: Feed<C>) -> eyre::Result<RpcModule<Feed<C>>> {
    let mut module = RpcModule::new(feed);
    module.register_blocking_method("sova_getZcashBlocks", |params, feed, _| {
        let (from, to) = range_params(&params)?;
        let items = feed.range(from, to).map_err(invalid)?;
        Ok::<_, ErrorObjectOwned>(Value::Array(items.iter().map(item_json).collect()))
    })?;
    module.register_subscription(
        "sova_subscribe",
        "sova_subscription",
        "sova_unsubscribe",
        |params, pending: PendingSubscriptionSink, feed: Arc<Feed<C>>, _| async move {
            let cursor = subscribe_params(&params).and_then(|from| feed.cursor(from));
            let mut cursor = match cursor {
                Ok(c) => c,
                Err(msg) => {
                    pending.reject(invalid(msg)).await;
                    return Ok(());
                }
            };
            let sink = pending.accept().await?;
            let mut tick = tokio::time::interval(POLL_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = sink.closed() => break,
                    _ = tick.tick() => {}
                }
                for event in feed.poll(&mut cursor) {
                    let raw = serde_json::value::to_raw_value(&event_json(&event))?;
                    sink.send(SubscriptionMessage::from(raw)).await?;
                }
            }
            Ok::<(), jsonrpsee::core::SubscriptionError>(())
        },
    )?;
    Ok(module)
}

/// [`SovaChain`] over the node's provider: the effective head (as the
/// sealer and arbiter see it) and canonical anchors.
#[derive(Debug, Clone)]
pub(crate) struct NodeChain<P>(pub(crate) P);

impl<P> SovaChain for NodeChain<P>
where
    P: reth_ethereum::provider::BlockNumReader
        + reth_ethereum::provider::HeaderProvider
        + Send
        + Sync
        + 'static,
{
    fn head(&self) -> u64 {
        let head = self.0.best_block_number().unwrap_or(0);
        engine::expectations::global().effective_head(head, |h| crate::canonical_anchor(&self.0, h))
    }

    fn anchor(&self, number: u64) -> Option<[u8; 32]> {
        crate::canonical_anchor(&self.0, number)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::{collections::HashMap, sync::RwLock};

    use consensus::{
        follower::EpochData,
        pools::{BlockPools, TreeSizes},
    };

    use super::*;

    const B: u64 = 100;

    /// A Sova chain whose head and anchors the test sets.
    #[derive(Debug, Default)]
    struct FakeChain {
        head: RwLock<u64>,
        anchors: RwLock<HashMap<u64, [u8; 32]>>,
    }

    impl SovaChain for Arc<FakeChain> {
        fn head(&self) -> u64 {
            *self.head.read().unwrap()
        }
        fn anchor(&self, number: u64) -> Option<[u8; 32]> {
            self.anchors.read().unwrap().get(&number).copied()
        }
    }

    fn hash(h: u64, branch: u8) -> [u8; 32] {
        let mut x = [branch; 32];
        x[24..].copy_from_slice(&h.to_be_bytes());
        x
    }

    fn pools(h: u64) -> BlockPools {
        BlockPools {
            chain_value_zat: [h, 0, 0, 0, 0, h * 2],
            delta_zat: [1, 0, 0, 0, 0, -2],
            chain_supply_zat: h * 3,
            trees: TreeSizes::default(),
        }
    }

    /// Index Zcash heights `from..=to` on `branch`, and seal the Sova
    /// blocks that anchor them (head = the one anchoring `to`).
    fn grow(index: &ZcashIndex, chain: &FakeChain, from: u64, to: u64, branch: u8) {
        for h in from..=to {
            index.insert(&EpochData {
                height: h,
                hash: hash(h, branch),
                burns: Vec::new(),
                time: 1_000 + h as u32,
                txs: Vec::new(),
                pools: Some(Box::new(pools(h))),
            });
            chain
                .anchors
                .write()
                .unwrap()
                .insert(h - B + 1, hash(h, branch));
        }
        *chain.head.write().unwrap() = to - B + 1;
    }

    fn feed() -> (Feed<Arc<FakeChain>>, Arc<FakeChain>, &'static ZcashIndex) {
        let index: &'static ZcashIndex = Box::leak(Box::new(ZcashIndex::with_base(B)));
        let chain = Arc::new(FakeChain::default());
        (Feed::new(chain.clone(), index), chain, index)
    }

    fn heights(items: &[ZcashBlockItem]) -> Vec<u64> {
        items.iter().map(|i| i.height).collect()
    }

    /// Golden: SIP-7 Appendix A's block 4,384,200 (testnet, coinbase-only).
    #[test]
    fn item_json_golden() {
        let mut hash = [0u8; 32];
        hex::decode_to_slice(
            "00809413c7591f17cfe5043546e6ce5d292e595ede7b9f74def7494f1f1cc2bc",
            &mut hash,
        )
        .unwrap();
        let item = ZcashBlockItem {
            height: 4_384_200,
            sova_block: 4_384_200,
            hash,
            time: 1_790_184_215,
            summary: BlockSummary {
                pools: BlockPools {
                    chain_value_zat: [
                        1_573_837_835_978_306,
                        42_832_983_037_484,
                        152_869_428_798_703,
                        23_913_312_221_154,
                        15_894_393_750_000,
                        13_752_684_049_396,
                    ],
                    delta_zat: [12_500_000, 0, 0, -3, 18_750_000, 125_000_000],
                    chain_supply_zat: 1_823_100_637_835_043,
                    trees: TreeSizes {
                        sapling: 404_304,
                        orchard: 248_902,
                        ironwood: 354_039,
                    },
                },
                stats: BlockStats {
                    tx_count: 1,
                    shielded_tx_count: 1,
                    t_in: 0,
                    t_out: 1,
                    sapling_spends: 0,
                    sapling_outputs: 0,
                    orchard_actions: 0,
                    ironwood_actions: 2,
                    joinsplits: 0,
                },
            },
        };
        let want = r#"{"chainSupply":"1823100637835043","deltas":{"ironwood":"125000000","lockbox":"18750000","orchard":"-3","sapling":"0","sprout":"0","transparent":"12500000"},"hash":"0x00809413c7591f17cfe5043546e6ce5d292e595ede7b9f74def7494f1f1cc2bc","height":4384200,"pools":{"ironwood":"13752684049396","lockbox":"15894393750000","orchard":"23913312221154","sapling":"152869428798703","sprout":"42832983037484","transparent":"1573837835978306"},"sovaBlock":4384200,"stats":{"ironwoodActions":2,"joinSplits":0,"orchardActions":0,"saplingOutputs":0,"saplingSpends":0,"shieldedTxCount":1,"tIn":0,"tOut":1,"txCount":1},"time":1790184215,"trees":{"ironwood":354039,"orchard":248902,"sapling":404304}}"#;
        assert_eq!(
            item_json(&item),
            serde_json::from_str::<Value>(want).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&event_json(&FeedEvent::Rollback { to_height: 7 })).unwrap(),
            r#"{"rollback":{"toHeight":7}}"#
        );
    }

    #[test]
    fn range_cap() {
        assert!(check_range(5, 5).is_ok());
        assert!(check_range(1, MAX_RANGE).is_ok());
        let err = check_range(1, MAX_RANGE + 1).unwrap_err();
        assert!(err.contains("at most 1000"), "{err}");
        assert!(check_range(0, u64::MAX).is_err());
        assert!(check_range(6, 5).unwrap_err().contains("below"));
        let (feed, _, _) = feed();
        assert!(feed.range(B, B + MAX_RANGE).is_err());
    }

    #[test]
    fn anchored_through_formula() {
        assert_eq!(
            anchored_through(0, B, Some(B + 5)),
            None,
            "genesis anchors nothing"
        );
        assert_eq!(anchored_through(1, B, None), None, "empty index");
        assert_eq!(anchored_through(1, B, Some(B + 5)), Some(B));
        assert_eq!(anchored_through(3, B, Some(B + 5)), Some(B + 2));
        assert_eq!(
            anchored_through(30, B, Some(B + 5)),
            Some(B + 5),
            "index-bound"
        );
        assert_eq!(anchored_through(u64::MAX, B, Some(7)), None, "overflow");
    }

    /// Indexed but not yet anchored heights are not served; neither are
    /// heights below the base, or ones whose Sova anchor disagrees.
    #[test]
    fn anchored_horizon_cutoff() {
        let (feed, chain, index) = feed();
        assert_eq!(feed.range(B, B + 10).unwrap(), vec![]);
        grow(index, &chain, B, B + 9, 1);
        // The follower is 5 ahead of the sealed head.
        *chain.head.write().unwrap() = 5;
        assert_eq!(feed.horizon(), Some(B + 4));
        assert_eq!(
            heights(&feed.range(0, B + 50).unwrap()),
            (B..=B + 4).collect::<Vec<_>>()
        );
        assert_eq!(heights(&feed.range(B + 3, B + 3).unwrap()), vec![B + 3]);
        assert_eq!(feed.range(B + 5, B + 9).unwrap(), vec![]);
        // A stale anchor at the top truncates the answer there.
        chain.anchors.write().unwrap().insert(4, hash(B + 3, 9));
        assert_eq!(
            heights(&feed.range(B, B + 9).unwrap()),
            (B..=B + 2).collect::<Vec<_>>()
        );
        let item = feed.item(B + 1).unwrap();
        assert_eq!((item.sova_block, item.hash), (2, hash(B + 1, 1)));
        assert_eq!(item.summary.pools, pools(B + 1));
    }

    fn items<T: Clone>(events: &[FeedEvent<T>]) -> Vec<T> {
        events
            .iter()
            .filter_map(|e| match e {
                FeedEvent::Block(t) => Some(t.clone()),
                FeedEvent::Rollback { .. } => None,
            })
            .collect()
    }

    /// The pure diff over a plain map: new heights stream in order, a
    /// capped batch resumes where it stopped, and a reorg rolls back to the
    /// fork then re-sends the new branch.
    #[test]
    fn cursor_streams_then_rolls_back_to_the_fork() {
        let chain = RwLock::new(HashMap::<u64, [u8; 32]>::new());
        let at = |h: u64| chain.read().unwrap().get(&h).map(|x| (*x, h));
        for h in 0..=10 {
            chain.write().unwrap().insert(h, hash(h, 1));
        }
        let mut c = Cursor::new(6, Some((5, hash(5, 1))));
        assert_eq!(items(&c.step(Some(10), 3, at)), vec![6, 7, 8]);
        assert_eq!(
            c.step(Some(10), 100, at),
            vec![FeedEvent::Block(9), FeedEvent::Block(10)]
        );
        assert_eq!(c.step(Some(10), 100, at), vec![], "nothing new");
        // Zcash reorg at 8: 8.. replaced, and the anchored top drops to 9.
        for h in 8..=12 {
            chain.write().unwrap().insert(h, hash(h, 2));
        }
        assert_eq!(
            c.step(Some(9), 100, at),
            vec![
                FeedEvent::Rollback { to_height: 7 },
                FeedEvent::Block(8),
                FeedEvent::Block(9)
            ]
        );
        assert_eq!(items(&c.step(Some(12), 100, at)), vec![10, 11, 12]);
        // The anchored height lowers with the same hashes (the head was
        // unwound to a rollback floor): roll back, then re-send as it regrows.
        assert_eq!(
            c.step(Some(10), 100, at),
            vec![FeedEvent::Rollback { to_height: 10 }]
        );
        assert_eq!(c.next(), 11);
        assert_eq!(items(&c.step(Some(12), 100, at)), vec![11, 12]);
        // Nothing anchored at all: everything remembered is void.
        let mut d = Cursor::new(3, None);
        assert_eq!(items(&d.step(Some(4), 100, at)), vec![3, 4]);
        assert_eq!(
            d.step(None, 100, at),
            vec![FeedEvent::Rollback { to_height: 2 }]
        );
        // A gap (height missing in the source) stops the batch there.
        chain.write().unwrap().remove(&6);
        let mut e = Cursor::new(5, None);
        assert_eq!(items(&e.step(Some(10), 100, at)), vec![5]);
        assert_eq!(e.next(), 6);
    }

    /// A reorg across the subscription's start (seed) is reported even if
    /// nothing was sent yet; a deep reorg past the memory window rolls back
    /// to its oldest height.
    #[test]
    fn cursor_seed_and_memory_window() {
        let chain = RwLock::new(HashMap::<u64, [u8; 32]>::new());
        let at = |h: u64| chain.read().unwrap().get(&h).map(|x| (*x, h));
        for h in 0..=5 {
            chain.write().unwrap().insert(h, hash(h, 1));
        }
        let mut c = Cursor::new(6, Some((5, hash(5, 1))));
        assert_eq!(c.step(Some(5), 10, at), vec![]);
        chain.write().unwrap().insert(5, hash(5, 2));
        assert_eq!(
            c.step(Some(5), 10, at),
            vec![FeedEvent::Rollback { to_height: 4 }, FeedEvent::Block(5)]
        );

        // Found live on the box: the seed (and everything sent) sits above
        // a reorg whose fork is below the seed. The rollback must reach the
        // anchored top, not stop just under the seed.
        let mut s = Cursor::new(21, Some((20, hash(20, 1))));
        for h in 17..=21 {
            chain.write().unwrap().insert(h, hash(h, 1));
        }
        assert_eq!(items(&s.step(Some(21), 10, at)), vec![21]);
        for h in 19..=21 {
            chain.write().unwrap().insert(h, hash(h, 5));
        }
        assert_eq!(
            s.step(Some(18), 10, at),
            vec![FeedEvent::Rollback { to_height: 18 }]
        );
        assert_eq!(items(&s.step(Some(21), 10, at)), vec![19, 20, 21]);

        let n = REMEMBER as u64 + 50;
        for h in 0..=n {
            chain.write().unwrap().insert(h, hash(h, 3));
        }
        let mut d = Cursor::new(0, None);
        assert_eq!(items(&d.step(Some(n), usize::MAX, at)).len() as u64, n + 1);
        for h in 0..=n {
            chain.write().unwrap().insert(h, hash(h, 4));
        }
        let events = d.step(Some(n), 1, at);
        assert_eq!(
            events[0],
            FeedEvent::Rollback {
                to_height: n - REMEMBER as u64
            }
        );
    }

    /// End to end over the feed: a Zcash reorg that the Sova chain follows
    /// (unwind, then re-seal on the new branch) reaches a subscriber as
    /// rollback + new items; heights are only sent once anchored.
    #[test]
    fn feed_poll_follows_the_canonical_chain() {
        let (feed, chain, index) = feed();
        grow(index, &chain, B, B + 4, 1);
        let mut c = feed.cursor(Some(B)).unwrap();
        let got = feed.poll(&mut c);
        assert_eq!(heights(&items(&got)), (B..=B + 4).collect::<Vec<_>>());
        // Zcash advances but Sova hasn't sealed yet: nothing to send.
        index.insert(&EpochData {
            height: B + 5,
            hash: hash(B + 5, 1),
            burns: Vec::new(),
            time: 0,
            txs: Vec::new(),
            pools: Some(Box::new(pools(B + 5))),
        });
        assert_eq!(feed.poll(&mut c), vec![]);
        // Zcash reorg to B+2: index unwound and rescanned, Sova re-sealed.
        index.unwind_above(B + 2);
        grow(index, &chain, B + 3, B + 6, 2);
        let got = feed.poll(&mut c);
        assert_eq!(got[0], FeedEvent::Rollback { to_height: B + 2 });
        assert_eq!(heights(&items(&got)), (B + 3..=B + 6).collect::<Vec<_>>());
        assert_eq!(
            items(&got).first().map(|i| i.hash),
            Some(hash(B + 3, 2)),
            "the new branch"
        );
        // A new subscription starts after the anchored height; one that asks
        // too far back is refused.
        let mut fresh = feed.cursor(None).unwrap();
        assert_eq!(feed.poll(&mut fresh), vec![]);
        assert_eq!(
            feed.cursor(Some(0)).unwrap().next(),
            B,
            "clamped to the base"
        );
    }

    #[test]
    fn height_params() {
        for (v, want) in [
            (json!(7), Some(7)),
            (json!("7"), Some(7)),
            (json!("0x10"), Some(16)),
            (json!("0x"), None),
            (json!(-1), None),
            (json!(1.5), None),
            (json!("ten"), None),
            (json!(null), None),
        ] {
            assert_eq!(parse_height(&v, "h").ok(), want, "{v}");
        }
    }

    /// The module on a real jsonrpsee server: shape, the cap as an
    /// invalid-params error, and positional or named params.
    #[tokio::test(flavor = "multi_thread")]
    async fn module_over_http() {
        let (feed, chain, index) = feed();
        grow(index, &chain, B, B + 2, 1);
        let server = jsonrpsee::server::Server::builder()
            .build("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let handle = server.start(module(feed).unwrap());
        let post = |params: &str| {
            let url = url.clone();
            let body = format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"sova_getZcashBlocks","params":{params}}}"#
            );
            tokio::task::spawn_blocking(move || {
                let text = ureq::post(&url)
                    .set("content-type", "application/json")
                    .send_string(&body)
                    .unwrap()
                    .into_string()
                    .unwrap();
                serde_json::from_str::<Value>(&text).unwrap()
            })
        };
        let got = post(&format!("[{B}, {}]", B + 50)).await.unwrap();
        let arr = got["result"].as_array().unwrap();
        assert_eq!(arr.len(), 3, "{got}");
        assert_eq!(arr[1]["height"], B + 1);
        assert_eq!(arr[1]["sovaBlock"], 2);
        assert_eq!(arr[1]["pools"]["ironwood"], ((B + 1) * 2).to_string());
        assert_eq!(arr[1]["deltas"]["ironwood"], "-2");
        let got = post(&format!(r#"{{"fromHeight":"0x{:x}","toHeight":{}}}"#, B, B))
            .await
            .unwrap();
        assert_eq!(got["result"].as_array().unwrap().len(), 1, "{got}");
        let got = post(&format!("[0, {MAX_RANGE}]")).await.unwrap();
        assert_eq!(got["error"]["code"], INVALID_PARAMS, "{got}");
        let got = post("[1]").await.unwrap();
        assert_eq!(got["error"]["code"], INVALID_PARAMS, "{got}");
        handle.stop().unwrap();
    }
}
