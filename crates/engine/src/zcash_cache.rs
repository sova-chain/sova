//! The expectations follower's Zcash input, persisted
//! (docs/design/fast-restart.md §3.1).
//!
//! A restart used to rescan every Zcash block since the epoch base before
//! the node could seal again (2026-10-03 keeper: ~63,000 blocks, ~12 min,
//! growing daily). The SIP-4/SIP-7 index, SIP-8 votes and the settlement
//! expectations need all of that history, so it cannot be skipped. Instead
//! each block is stored exactly as [`ZcashView::block_at`] returned it, in
//! an append-only file in the datadir, and replayed on start through the
//! unchanged follower and the unchanged application code: the cache holds
//! the follower's *input*, never derived state, so a replay derives every
//! store with the same code (and the running binary's rules) a fresh scan
//! would.
//!
//! The cache is an accelerator, never an authority:
//! - each record carries a CRC32C, heights are contiguous from the base and
//!   every record is hash-linked to the one before; loading stops at the
//!   first record that fails any of these and the file is cut there;
//! - the header binds the file to the epoch base, the Sova chain id and the
//!   zebrad identity (network + build): a different zebrad discards the file
//!   (an upgraded zebrad may describe old blocks differently, e.g. NU7's
//!   pools; design R2);
//! - on start the file's tip is walked down against zebrad's `getblockhash`
//!   until they agree (at most the reorg window), which, by the hash links,
//!   puts every record below on zebrad's current chain; then the top 33
//!   records and 32 random older ones are refetched and must be identical,
//!   or the whole file is discarded and the node rescans;
//! - after the replay serves its last verified block the view goes live for
//!   good: every later call, including the follower's own reorg check, asks
//!   zebrad.
//!
//! `SOVA_ZCASH_CACHE=off` disables it; `SOVA_ZCASH_CACHE=rebuild` discards
//! the file once at start.

use std::{
    collections::{HashMap, VecDeque},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use consensus::{
    follower::{BlockView, FollowerEvent, TxOut, TxView, ViewError, ZcashView},
    pools::{BlockPools, POOLS, ShieldedSummary, TreeSizes, TxShielded},
};

/// File name inside `<datadir>/sova/`.
pub const FILE_NAME: &str = "zcash-blocks.v1";

const MAGIC: &[u8; 8] = b"SOVAZCB\0";
/// Bump on any change to the record encoding.
const FORMAT: u32 = 1;
/// No real block record comes near this; a larger length is corruption.
const MAX_RECORD: u32 = 64 << 20;
/// Records below the verified tip refetched and compared on start.
const SPOT_RECENT: u64 = 32;
/// Older records, picked at random, refetched and compared on start.
const SPOT_RANDOM: usize = 32;

/// What a cache file is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheHeader {
    /// Zcash height of the first epoch.
    pub epoch_base: u64,
    /// Sova chain id.
    pub chain_id: u64,
    /// [`ZcashView::node_identity`] of the zebrad that produced it.
    pub zebrad: String,
}

/// Where and for which chain to keep the cache.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// The cache file.
    pub path: PathBuf,
    /// Sova chain id.
    pub chain_id: u64,
    /// Discard any existing file first (`SOVA_ZCASH_CACHE=rebuild`).
    pub rebuild: bool,
}

impl CacheConfig {
    /// The cache for a persistent datadir, unless `SOVA_ZCASH_CACHE=off`.
    #[must_use]
    pub fn from_env(datadir: &Path, chain_id: u64) -> Option<Self> {
        let mode = std::env::var("SOVA_ZCASH_CACHE").unwrap_or_default();
        if mode == "off" {
            return None;
        }
        Some(Self {
            path: datadir.join("sova").join(FILE_NAME),
            chain_id,
            rebuild: mode == "rebuild",
        })
    }
}

// ---------------------------------------------------------------- encoding

fn put_u16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_i64(b: &mut Vec<u8>, v: i64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_len(b: &mut Vec<u8>, n: usize) {
    put_u32(b, u32::try_from(n).unwrap_or(u32::MAX));
}

/// A bounds-checked little-endian reader: every getter is `None` past the end.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    const fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    fn arr<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.arr().map(u16::from_le_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.arr().map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.arr().map(u64::from_le_bytes)
    }
    fn i64(&mut self) -> Option<i64> {
        self.arr().map(i64::from_le_bytes)
    }
    /// A count that must fit in what's left (each item >= `min_item` bytes),
    /// so a corrupt count can't ask for a huge allocation.
    fn count(&mut self, min_item: usize) -> Option<usize> {
        let n = usize::try_from(self.u32()?).ok()?;
        (n.checked_mul(min_item)? <= self.b.len() - self.at).then_some(n)
    }
    const fn done(&self) -> bool {
        self.at == self.b.len()
    }
}

fn encode_header(h: &CacheHeader) -> Vec<u8> {
    let mut b = Vec::with_capacity(32 + h.zebrad.len());
    b.extend_from_slice(MAGIC);
    put_u32(&mut b, FORMAT);
    put_u64(&mut b, h.epoch_base);
    put_u64(&mut b, h.chain_id);
    let z = h.zebrad.as_bytes();
    let zlen = u16::try_from(z.len()).unwrap_or(u16::MAX);
    put_u16(&mut b, zlen);
    b.extend_from_slice(&z[..usize::from(zlen)]);
    b
}

/// The header at the start of `b` and its length, if it is one.
fn decode_header(b: &[u8]) -> Option<(CacheHeader, usize)> {
    let mut r = Reader::new(b);
    if r.take(MAGIC.len())? != MAGIC || r.u32()? != FORMAT {
        return None;
    }
    let epoch_base = r.u64()?;
    let chain_id = r.u64()?;
    let zlen = usize::from(r.u16()?);
    let zebrad = String::from_utf8(r.take(zlen)?.to_vec()).ok()?;
    Some((
        CacheHeader {
            epoch_base,
            chain_id,
            zebrad,
        },
        r.at,
    ))
}

/// One block, every field of [`BlockView`].
#[must_use]
pub fn encode_block(blk: &BlockView) -> Vec<u8> {
    let mut b = Vec::with_capacity(256);
    put_u64(&mut b, blk.height);
    b.extend_from_slice(&blk.hash);
    b.extend_from_slice(&blk.prev_hash);
    put_u32(&mut b, blk.time);
    match blk.pools.as_deref() {
        None => b.push(0),
        Some(p) => {
            b.push(1);
            b.push(u8::try_from(POOLS).unwrap_or(u8::MAX));
            for v in p.chain_value_zat {
                put_u64(&mut b, v);
            }
            for v in p.delta_zat {
                put_i64(&mut b, v);
            }
            put_u64(&mut b, p.chain_supply_zat);
            put_u64(&mut b, p.trees.sapling);
            put_u64(&mut b, p.trees.orchard);
            put_u64(&mut b, p.trees.ironwood);
        }
    }
    put_len(&mut b, blk.txs.len());
    for tx in &blk.txs {
        b.extend_from_slice(&tx.txid);
        put_u32(&mut b, tx.version);
        put_len(&mut b, tx.outputs.len());
        for o in &tx.outputs {
            put_u64(&mut b, o.value_zat);
            put_len(&mut b, o.script.len());
            b.extend_from_slice(&o.script);
        }
        put_u32(&mut b, tx.shielded.n_in);
        match &tx.shielded.summary {
            None => b.push(0),
            Some(s) => {
                b.push(1);
                for d in s.deltas {
                    put_i64(&mut b, d);
                }
                put_u32(&mut b, s.sapling_spends);
                put_u32(&mut b, s.sapling_outputs);
                put_u32(&mut b, s.orchard_actions);
                put_u32(&mut b, s.ironwood_actions);
                put_u32(&mut b, s.joinsplits);
            }
        }
    }
    b
}

/// Inverse of [`encode_block`]; `None` unless `b` is exactly one block.
#[must_use]
pub fn decode_block(b: &[u8]) -> Option<BlockView> {
    let mut r = Reader::new(b);
    let height = r.u64()?;
    let hash = r.arr()?;
    let prev_hash = r.arr()?;
    let time = r.u32()?;
    let pools = match r.u8()? {
        0 => None,
        1 => {
            if usize::from(r.u8()?) != POOLS {
                return None;
            }
            let mut p = BlockPools::default();
            for v in &mut p.chain_value_zat {
                *v = r.u64()?;
            }
            for v in &mut p.delta_zat {
                *v = r.i64()?;
            }
            p.chain_supply_zat = r.u64()?;
            p.trees = TreeSizes {
                sapling: r.u64()?,
                orchard: r.u64()?,
                ironwood: r.u64()?,
            };
            Some(Box::new(p))
        }
        _ => return None,
    };
    let ntx = r.count(32 + 4 + 4 + 4 + 1)?;
    let mut txs = Vec::with_capacity(ntx);
    for _ in 0..ntx {
        let txid = r.arr()?;
        let version = r.u32()?;
        let nout = r.count(8 + 4)?;
        let mut outputs = Vec::with_capacity(nout);
        for _ in 0..nout {
            let value_zat = r.u64()?;
            let slen = r.count(1)?;
            outputs.push(TxOut {
                value_zat,
                script: r.take(slen)?.to_vec(),
            });
        }
        let n_in = r.u32()?;
        let summary = match r.u8()? {
            0 => None,
            1 => {
                let mut deltas = [0i64; 4];
                for d in &mut deltas {
                    *d = r.i64()?;
                }
                Some(ShieldedSummary {
                    deltas,
                    sapling_spends: r.u32()?,
                    sapling_outputs: r.u32()?,
                    orchard_actions: r.u32()?,
                    ironwood_actions: r.u32()?,
                    joinsplits: r.u32()?,
                })
            }
            _ => return None,
        };
        txs.push(TxView {
            txid,
            version,
            outputs,
            shielded: TxShielded { n_in, summary },
        });
    }
    r.done().then_some(BlockView {
        height,
        hash,
        prev_hash,
        time,
        txs,
        pools,
    })
}

/// CRC-32C (Castagnoli), table-driven.
fn crc32c(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 == 1 {
                    (c >> 1) ^ 0x82F6_3B78
                } else {
                    c >> 1
                };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    let mut crc = !0u32;
    for &byte in data {
        crc = TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

// ------------------------------------------------------------------- file

/// The append-only file: header, then `len u32 | crc32c u32 | block` records,
/// one per Zcash height from the epoch base, hash-linked.
#[derive(Debug)]
pub struct ZcashCache {
    file: File,
    header_len: u64,
    base: u64,
    /// End offset of each record; index `i` is height `base + i`.
    ends: Vec<u64>,
    /// Hash of each record, same indexing.
    hashes: Vec<[u8; 32]>,
    dirty: bool,
}

impl ZcashCache {
    /// Open (or create) the cache at `path` for `header`, returning it and
    /// every record that loads cleanly. A file for another header is reset;
    /// a torn, corrupt, non-contiguous or unlinked tail is cut off.
    ///
    /// # Errors
    /// The file can't be created, read or written.
    pub fn open(path: &Path, header: &CacheHeader) -> std::io::Result<(Self, Vec<BlockView>)> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let mut cache = Self {
            file,
            header_len: 0,
            base: header.epoch_base,
            ends: Vec::new(),
            hashes: Vec::new(),
            dirty: false,
        };
        let header_len = match decode_header(&bytes) {
            Some((found, len)) if &found == header => len,
            found => {
                if !bytes.is_empty() {
                    tracing::warn!(
                        found = ?found.map(|(h, _)| h),
                        want = ?header,
                        "zcash cache: written for another chain, base or zebrad; starting a new one"
                    );
                }
                let encoded = encode_header(header);
                cache.file.set_len(0)?;
                cache.file.seek(SeekFrom::Start(0))?;
                cache.file.write_all(&encoded)?;
                cache.file.sync_data()?;
                cache.header_len = encoded.len() as u64;
                return Ok((cache, Vec::new()));
            }
        };
        cache.header_len = header_len as u64;
        let mut blocks = Vec::new();
        let mut at = header_len;
        let mut why = None;
        while at < bytes.len() {
            match Self::record_at(&bytes, at) {
                Ok((block, next)) => {
                    let want = header.epoch_base + blocks.len() as u64;
                    if block.height != want {
                        why = Some(format!("record for {} where {want} belongs", block.height));
                        break;
                    }
                    if let Some(prev) = cache.hashes.last()
                        && block.prev_hash != *prev
                    {
                        why = Some(format!("{} does not link to its parent", block.height));
                        break;
                    }
                    cache.ends.push(next as u64);
                    cache.hashes.push(block.hash);
                    blocks.push(block);
                    at = next;
                }
                Err(e) => {
                    why = Some(e);
                    break;
                }
            }
        }
        if let Some(why) = why {
            let keep = cache.end();
            tracing::warn!(
                %why,
                kept = blocks.len(),
                dropped_bytes = bytes.len() as u64 - keep,
                "zcash cache: cutting the file at its last good record"
            );
            cache.file.set_len(keep)?;
            cache.file.sync_data()?;
        }
        Ok((cache, blocks))
    }

    /// The record at byte `at` and the offset after it.
    fn record_at(bytes: &[u8], at: usize) -> Result<(BlockView, usize), String> {
        let mut r = Reader::new(&bytes[at..]);
        let (Some(len), Some(crc)) = (r.u32(), r.u32()) else {
            return Err("torn record header".into());
        };
        if len > MAX_RECORD {
            return Err(format!("record length {len}"));
        }
        let Some(payload) = r.take(len as usize) else {
            return Err("torn record".into());
        };
        if crc32c(payload) != crc {
            return Err("record checksum mismatch".into());
        }
        let block = decode_block(payload).ok_or("undecodable record")?;
        Ok((block, at + 8 + len as usize))
    }

    fn end(&self) -> u64 {
        self.ends.last().copied().unwrap_or(self.header_len)
    }

    /// Highest stored height.
    #[must_use]
    pub fn tip(&self) -> Option<u64> {
        (!self.hashes.is_empty()).then(|| self.base + self.hashes.len() as u64 - 1)
    }

    /// Stored hash at `height`.
    #[must_use]
    pub fn hash_at(&self, height: u64) -> Option<[u8; 32]> {
        let i = usize::try_from(height.checked_sub(self.base)?).ok()?;
        self.hashes.get(i).copied()
    }

    /// Keep records up to and including `height` (below the base: none).
    ///
    /// # Errors
    /// The file can't be truncated.
    pub fn truncate_after(&mut self, height: u64) -> std::io::Result<()> {
        let keep = usize::try_from((height + 1).saturating_sub(self.base)).unwrap_or(usize::MAX);
        if keep >= self.hashes.len() {
            return Ok(());
        }
        self.ends.truncate(keep);
        self.hashes.truncate(keep);
        self.file.set_len(self.end())?;
        self.dirty = true;
        Ok(())
    }

    /// Append the block one above the tip (or at the base), linked to it.
    ///
    /// # Errors
    /// The block isn't the next height or doesn't link; the write fails.
    pub fn append(&mut self, block: &BlockView) -> std::io::Result<()> {
        let want = self.base + self.hashes.len() as u64;
        if block.height != want || self.hashes.last().is_some_and(|p| *p != block.prev_hash) {
            return Err(std::io::Error::other(format!(
                "zcash cache: block {} doesn't extend the cache (next {want})",
                block.height
            )));
        }
        let payload = encode_block(block);
        let mut rec = Vec::with_capacity(8 + payload.len());
        put_len(&mut rec, payload.len());
        put_u32(&mut rec, crc32c(&payload));
        rec.extend_from_slice(&payload);
        let end = self.end();
        self.file.seek(SeekFrom::Start(end))?;
        self.file.write_all(&rec)?;
        self.ends.push(end + rec.len() as u64);
        self.hashes.push(block.hash);
        self.dirty = true;
        Ok(())
    }

    /// Flush appended records and truncations to disk.
    ///
    /// # Errors
    /// The sync fails.
    pub fn sync(&mut self) -> std::io::Result<()> {
        if self.dirty {
            self.file.sync_data()?;
            self.dirty = false;
        }
        Ok(())
    }
}

// ------------------------------------------------------------------- view

#[derive(Debug, Default)]
struct ViewState {
    /// Verified records not yet served, in height order.
    replay: VecDeque<BlockView>,
    /// The last record served (the follower's reorg check asks for it again).
    last: Option<BlockView>,
    /// Live blocks fetched since the last [`CachedView::take_recorded`].
    recorded: HashMap<u64, BlockView>,
    recording: bool,
}

/// A [`ZcashView`] that serves verified cached blocks, in order, until it
/// has served the last one, then answers from `inner` for good.
#[derive(Debug)]
pub struct CachedView<V> {
    inner: V,
    state: Mutex<ViewState>,
}

impl<V: ZcashView> CachedView<V> {
    /// No cache: every call goes to `inner`.
    pub fn live(inner: V) -> Self {
        Self {
            inner,
            state: Mutex::new(ViewState::default()),
        }
    }

    fn replaying(inner: V, blocks: Vec<BlockView>) -> Self {
        Self {
            inner,
            state: Mutex::new(ViewState {
                replay: blocks.into(),
                recording: true,
                ..ViewState::default()
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ViewState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The live blocks fetched since the last call, by height (latest fetch).
    pub fn take_recorded(&self) -> HashMap<u64, BlockView> {
        std::mem::take(&mut self.state().recorded)
    }

    /// Stop remembering live blocks (the cache is gone).
    pub fn stop_recording(&self) {
        let mut s = self.state();
        s.recording = false;
        s.recorded.clear();
    }

    /// Blocks still to be replayed.
    pub fn replay_left(&self) -> usize {
        self.state().replay.len()
    }
}

impl<V: ZcashView> ZcashView for CachedView<V> {
    fn tip_height(&self) -> Result<u64, ViewError> {
        self.inner.tip_height()
    }

    fn block_at(&self, height: u64) -> Result<Option<BlockView>, ViewError> {
        {
            let mut s = self.state();
            if s.replay.front().is_some_and(|b| b.height == height) {
                let block = s.replay.pop_front();
                s.last = if s.replay.is_empty() {
                    // Served the verified tip: live from here on.
                    None
                } else {
                    block.clone()
                };
                return Ok(block);
            }
            if let Some(last) = s.last.as_ref().filter(|b| b.height == height) {
                return Ok(Some(last.clone()));
            }
        }
        let block = self.inner.block_at(height)?;
        if let Some(b) = &block {
            let mut s = self.state();
            if s.recording {
                s.recorded.insert(height, b.clone());
            }
        }
        Ok(block)
    }

    fn hash_at(&self, height: u64) -> Result<Option<[u8; 32]>, ViewError> {
        self.inner.hash_at(height)
    }

    fn node_identity(&self) -> Result<Option<String>, ViewError> {
        self.inner.node_identity()
    }
}

// ---------------------------------------------------------------- startup

/// Retry a zebrad call until it answers (the follower can't run without it
/// either); logs every ~30 s.
fn retry<T>(what: &str, mut f: impl FnMut() -> Result<T, ViewError>) -> T {
    let mut tries = 0u32;
    loop {
        match f() {
            Ok(v) => return v,
            Err(err) => {
                if tries.is_multiple_of(15) {
                    tracing::warn!(%err, what, "zcash cache: zebrad not answering; retrying");
                }
                tries = tries.wrapping_add(1);
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }
}

/// Open the cache, prove its records are on `inner`'s chain, and return the
/// view the follower should scan (replaying the verified records first) and
/// the cache to append to. Blocking: zebrad calls and file IO.
///
/// Anything wrong with the cache costs a rescan, never a wrong answer: no
/// identity, an IO error, no agreement within `reorg_window`, or any
/// spot-checked record that differs from zebrad's answer.
pub fn prepare<V: ZcashView>(
    inner: V,
    config: &CacheConfig,
    epoch_base: u64,
    reorg_window: u64,
) -> (CachedView<V>, Option<ZcashCache>) {
    let Some(zebrad) = retry("identity", || inner.node_identity()) else {
        tracing::warn!("zcash cache: zebrad reports no build; caching off");
        return (CachedView::live(inner), None);
    };
    let header = CacheHeader {
        epoch_base,
        chain_id: config.chain_id,
        zebrad,
    };
    if config.rebuild {
        tracing::info!(path = %config.path.display(), "zcash cache: rebuild requested; discarding");
        let _ = std::fs::remove_file(&config.path);
    }
    let started = std::time::Instant::now();
    let (mut cache, mut blocks) = match ZcashCache::open(&config.path, &header) {
        Ok(opened) => opened,
        Err(err) => {
            tracing::warn!(%err, path = %config.path.display(), "zcash cache: can't open; caching off");
            return (CachedView::live(inner), None);
        }
    };
    match verify(&inner, &mut cache, &mut blocks, reorg_window) {
        Ok(()) => {}
        Err(err) => {
            tracing::warn!(%err, "zcash cache: can't trim to the verified tip; caching off");
            return (CachedView::live(inner), None);
        }
    }
    let Some(tip) = cache.tip() else {
        tracing::info!("zcash cache: empty; the first scan fills it");
        let view = CachedView::live(inner);
        view.state().recording = true;
        return (view, Some(cache));
    };
    tracing::info!(
        blocks = blocks.len(),
        tip,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "zcash cache: verified against zebrad; replaying"
    );
    (CachedView::replaying(inner, blocks), Some(cache))
}

/// Walk the tip down to agreement with `inner`, then spot-check contents;
/// trims `cache` and `blocks` to what is proven (possibly nothing).
fn verify<V: ZcashView>(
    inner: &V,
    cache: &mut ZcashCache,
    blocks: &mut Vec<BlockView>,
    reorg_window: u64,
) -> std::io::Result<()> {
    let Some(tip) = cache.tip() else {
        return Ok(());
    };
    let base = cache.base;
    let lowest = tip.saturating_sub(reorg_window).max(base);
    let mut verified = None;
    let mut h = tip;
    loop {
        if retry("getblockhash", || inner.hash_at(h)) == cache.hash_at(h) {
            verified = Some(h);
            break;
        }
        if h == lowest {
            break;
        }
        h -= 1;
    }
    let Some(verified) = verified else {
        tracing::warn!(
            tip,
            window = reorg_window,
            "zcash cache: no agreement with zebrad within the reorg window; rescanning"
        );
        return discard(cache, blocks);
    };
    if verified < tip {
        tracing::info!(
            tip,
            verified,
            "zcash cache: tip reorged away while down; trimmed"
        );
    }
    cache.truncate_after(verified)?;
    blocks.truncate(usize::try_from(verified - base + 1).unwrap_or(usize::MAX));

    let mut heights: Vec<u64> =
        (verified.saturating_sub(SPOT_RECENT).max(base)..=verified).collect();
    let older = verified.saturating_sub(SPOT_RECENT).saturating_sub(base);
    if older > 0 {
        let mut x = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64)
            | 1;
        for _ in 0..SPOT_RANDOM {
            // xorshift64
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            heights.push(base + x % older);
        }
    }
    for h in heights {
        let fresh = retry("getblock", || inner.block_at(h));
        let stored = usize::try_from(h - base).ok().and_then(|i| blocks.get(i));
        if fresh.as_ref() != stored {
            tracing::error!(
                height = h,
                "zcash cache: a stored block differs from zebrad's answer; discarding the cache and rescanning"
            );
            return discard(cache, blocks);
        }
    }
    cache.sync()
}

fn discard(cache: &mut ZcashCache, blocks: &mut Vec<BlockView>) -> std::io::Result<()> {
    blocks.clear();
    cache.truncate_after(cache.base.saturating_sub(1))?;
    cache.sync()
}

/// After a poll: persist what the follower applied. A rollback truncates; an
/// epoch already stored with the same hash is skipped (the replay); a new
/// epoch is appended from the block the view fetched for it.
///
/// # Errors
/// The block for an epoch wasn't fetched through `view`, doesn't extend the
/// cache, or the file write fails. The caller turns caching off for the run;
/// what was synced stays valid.
pub fn persist<V: ZcashView>(
    cache: &mut ZcashCache,
    view: &CachedView<V>,
    events: &[FollowerEvent],
) -> std::io::Result<()> {
    let mut recorded = view.take_recorded();
    for event in events {
        match event {
            FollowerEvent::Rollback { to_height } => cache.truncate_after(*to_height)?,
            FollowerEvent::Epoch(epoch) => {
                if cache.hash_at(epoch.height) == Some(epoch.hash) {
                    continue;
                }
                cache.truncate_after(epoch.height.saturating_sub(1))?;
                let block = recorded
                    .remove(&epoch.height)
                    .filter(|b| b.hash == epoch.hash)
                    .ok_or_else(|| {
                        std::io::Error::other(format!(
                            "zcash cache: no fetched block for epoch {}",
                            epoch.height
                        ))
                    })?;
                cache.append(&block)?;
            }
        }
    }
    cache.sync()
}

#[cfg(test)]
mod tests {
    use super::*;
    use consensus::follower::Follower;
    use std::cell::{Cell, RefCell};

    fn h32(tag: u64, salt: u8) -> [u8; 32] {
        let mut h = [salt; 32];
        h[..8].copy_from_slice(&tag.to_le_bytes());
        h
    }

    fn rich_block(height: u64, prev: [u8; 32], salt: u8) -> BlockView {
        BlockView {
            height,
            hash: h32(height, salt),
            prev_hash: prev,
            time: 1_700_000_000 + u32::try_from(height).unwrap_or(0),
            txs: vec![
                TxView {
                    txid: h32(height * 10, salt),
                    version: 5,
                    outputs: vec![TxOut {
                        value_zat: 312_500_000,
                        script: vec![0x76, 0xa9, 0x14, 1, 2, 3],
                    }],
                    shielded: TxShielded {
                        n_in: 0,
                        summary: None,
                    },
                },
                TxView {
                    txid: h32(height * 10 + 1, salt),
                    version: 4,
                    outputs: vec![
                        TxOut {
                            value_zat: 30_000,
                            script: vec![0x6a, 0x20, 9, 9],
                        },
                        TxOut {
                            value_zat: 0,
                            script: Vec::new(),
                        },
                    ],
                    shielded: TxShielded {
                        n_in: 2,
                        summary: Some(ShieldedSummary {
                            deltas: [-1, 2, -3, i64::MIN],
                            sapling_spends: 1,
                            sapling_outputs: 2,
                            orchard_actions: 3,
                            ironwood_actions: 4,
                            joinsplits: 5,
                        }),
                    },
                },
            ],
            pools: (!height.is_multiple_of(3)).then(|| {
                Box::new(BlockPools {
                    chain_value_zat: [1, 2, 3, 4, 5, u64::MAX],
                    delta_zat: [-1, 0, 1, i64::MAX, i64::MIN, 7],
                    chain_supply_zat: 99,
                    trees: TreeSizes {
                        sapling: 10,
                        orchard: 11,
                        ironwood: 12,
                    },
                })
            }),
        }
    }

    /// A chain with counted fetches; `salt` changes every hash (a fork).
    struct Chain {
        blocks: RefCell<Vec<BlockView>>,
        block_calls: Cell<u64>,
        identity: RefCell<Option<String>>,
    }

    impl Chain {
        fn new(base: u64, n: u64) -> Self {
            let c = Self {
                blocks: RefCell::new(Vec::new()),
                block_calls: Cell::new(0),
                identity: RefCell::new(Some("test|/Zebra:6.3.0/|v6.3.0".into())),
            };
            // Heights 0..base-1 exist on the real chain; the follower never
            // reads them, but the parent link of `base` points at one.
            let mut prev = h32(base - 1, 0);
            for h in base..base + n {
                let b = rich_block(h, prev, 0);
                prev = b.hash;
                c.blocks.borrow_mut().push(b);
            }
            c
        }
        fn base(&self) -> u64 {
            self.blocks.borrow()[0].height
        }
        /// Replace everything above `keep_tip` with `n` blocks of fork `salt`.
        fn reorg(&self, keep_tip: u64, n: u64, salt: u8) {
            let base = self.base();
            let mut b = self.blocks.borrow_mut();
            b.truncate(usize::try_from(keep_tip + 1 - base).unwrap_or(0));
            let mut prev = b.last().map_or(h32(base - 1, 0), |x| x.hash);
            for h in keep_tip + 1..=keep_tip + n {
                let blk = rich_block(h, prev, salt);
                prev = blk.hash;
                b.push(blk);
            }
        }
        fn grow(&self, n: u64) {
            let tip = self.blocks.borrow().last().map_or(0, |b| b.height);
            let salt = self.blocks.borrow().last().map_or(0, |b| b.hash[31]);
            self.reorg(tip, n, salt);
        }
    }

    impl ZcashView for &Chain {
        fn tip_height(&self) -> Result<u64, ViewError> {
            Ok(self.blocks.borrow().last().map_or(0, |b| b.height))
        }
        fn block_at(&self, height: u64) -> Result<Option<BlockView>, ViewError> {
            self.block_calls.set(self.block_calls.get() + 1);
            let b = self.blocks.borrow();
            let base = b.first().map_or(0, |x| x.height);
            Ok(height
                .checked_sub(base)
                .and_then(|i| b.get(usize::try_from(i).ok()?))
                .cloned())
        }
        fn hash_at(&self, height: u64) -> Result<Option<[u8; 32]>, ViewError> {
            let b = self.blocks.borrow();
            let base = b.first().map_or(0, |x| x.height);
            Ok(height
                .checked_sub(base)
                .and_then(|i| b.get(usize::try_from(i).ok()?))
                .map(|x| x.hash))
        }
        fn node_identity(&self) -> Result<Option<String>, ViewError> {
            Ok(self.identity.borrow().clone())
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sova-zcash-cache-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join(FILE_NAME)
    }

    fn config(path: &Path) -> CacheConfig {
        CacheConfig {
            path: path.to_path_buf(),
            chain_id: 82330,
            rebuild: false,
        }
    }

    type Epoch = consensus::follower::EpochData;

    /// Every epoch emitted, with every field (burns, txs, pools-derived
    /// data): what the expectations, index and votes are built from.
    fn epochs(events: &[FollowerEvent]) -> Vec<Epoch> {
        events
            .iter()
            .filter_map(|e| match e {
                FollowerEvent::Epoch(d) => Some(d.clone()),
                FollowerEvent::Rollback { .. } => None,
            })
            .collect()
    }

    /// One node run: prepare the cache, then poll in chunks (as
    /// `run_expectations` does) until caught up, persisting after each poll.
    /// Returns every event, in order.
    fn run_node(chain: &Chain, path: &Path, window: usize) -> Vec<FollowerEvent> {
        let (view, mut cache) = prepare(chain, &config(path), chain.base(), window as u64);
        let mut follower = Follower::new(chain.base(), window);
        let mut all = Vec::new();
        loop {
            let before = follower.next_height();
            let events = follower
                .poll(&Capped {
                    inner: &view,
                    cap: before + 49,
                })
                .unwrap_or_default();
            if let Some(c) = cache.as_mut() {
                persist(c, &view, &events).unwrap_or_else(|e| panic!("persist: {e}"));
            }
            let done = events.is_empty() && follower.next_height() == before;
            all.extend(events);
            if done {
                return all;
            }
        }
    }

    struct Capped<'a, V> {
        inner: &'a V,
        cap: u64,
    }
    impl<V: ZcashView> ZcashView for Capped<'_, V> {
        fn tip_height(&self) -> Result<u64, ViewError> {
            Ok(self.inner.tip_height()?.min(self.cap))
        }
        fn block_at(&self, height: u64) -> Result<Option<BlockView>, ViewError> {
            self.inner.block_at(height)
        }
    }

    fn fresh_epochs(chain: &Chain, window: usize) -> Vec<Epoch> {
        let mut f = Follower::new(chain.base(), window);
        epochs(&f.poll(&chain).unwrap_or_default())
    }

    #[test]
    fn every_field_round_trips() {
        for h in [1u64, 2, 3, 4_465_026] {
            let b = rich_block(h, h32(h - 1, 7), 7);
            assert_eq!(decode_block(&encode_block(&b)), Some(b.clone()));
            // Not exactly one block: rejected.
            let mut long = encode_block(&b);
            long.push(0);
            assert_eq!(decode_block(&long), None);
            let enc = encode_block(&b);
            for cut in [0, 1, enc.len() / 2, enc.len() - 1] {
                assert_eq!(decode_block(&enc[..cut]), None);
            }
        }
        let empty = BlockView {
            height: 9,
            hash: [1; 32],
            prev_hash: [2; 32],
            time: 3,
            txs: Vec::new(),
            pools: None,
        };
        assert_eq!(decode_block(&encode_block(&empty)), Some(empty));
        let hdr = CacheHeader {
            epoch_base: 4_388_500,
            chain_id: 82330,
            zebrad: "test|/Zebra:7.0.0/|v7.0.0-rc.0".into(),
        };
        let enc = encode_header(&hdr);
        assert_eq!(decode_header(&enc), Some((hdr, enc.len())));
    }

    #[test]
    fn crc32c_known_answer() {
        // RFC 3720 B.4: 32 bytes of zeros.
        assert_eq!(crc32c(&[0u8; 32]), 0x8A91_36AA);
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    /// The parity property: a restarted node replaying its cache emits
    /// exactly the epochs a fresh full scan emits, so every derived store
    /// (index, votes, expectations) is the same; and it fetches only the
    /// verification blocks plus what is new.
    #[test]
    fn restart_replays_what_a_fresh_scan_derives_and_fetches_little() {
        let chain = Chain::new(100, 400);
        let path = tmp("parity");
        let first = run_node(&chain, &path, 64);
        assert_eq!(epochs(&first), fresh_epochs(&chain, 64));

        chain.grow(5);
        chain.block_calls.set(0);
        let second = run_node(&chain, &path, 64);
        // 33 recent + 32 random spot checks, 5 new blocks, a few reorg
        // checks; nowhere near the 405 a rescan costs. (Read before the
        // comparison scan below, which fetches everything itself.)
        let calls = chain.block_calls.get();
        assert!(calls <= 33 + 32 + 5 + 20, "fetched {calls}");
        assert_eq!(epochs(&second), fresh_epochs(&chain, 64));

        // A third start with nothing new: same again.
        let third = run_node(&chain, &path, 64);
        assert_eq!(epochs(&third), fresh_epochs(&chain, 64));
    }

    #[test]
    fn a_reorg_while_down_trims_the_cache_and_matches_a_fresh_scan() {
        let chain = Chain::new(100, 300);
        let path = tmp("reorg");
        run_node(&chain, &path, 64);
        // Replace the top 10 blocks (and add 3) while the node is down.
        chain.reorg(389, 13, 9);
        let again = run_node(&chain, &path, 64);
        assert_eq!(epochs(&again), fresh_epochs(&chain, 64));
        // The file now holds the new branch: a further restart agrees too.
        let third = run_node(&chain, &path, 64);
        assert_eq!(epochs(&third), fresh_epochs(&chain, 64));
    }

    #[test]
    fn a_reorg_while_running_is_persisted_as_a_truncate_and_new_branch() {
        let chain = Chain::new(100, 200);
        let path = tmp("live-reorg");
        let (view, mut cache) = prepare(&chain, &config(&path), 100, 64);
        let mut follower = Follower::new(100, 64);
        let mut cache_ref = cache.take().unwrap_or_else(|| panic!("cache"));
        let events = follower.poll(&view).unwrap_or_default();
        persist(&mut cache_ref, &view, &events).unwrap_or_else(|e| panic!("{e}"));
        chain.reorg(290, 4, 5);
        let events = follower.poll(&view).unwrap_or_default();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, FollowerEvent::Rollback { to_height: 290 }))
        );
        persist(&mut cache_ref, &view, &events).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cache_ref.tip(), Some(294));
        assert_eq!(cache_ref.hash_at(294), chain.hash_at_test(294));
        drop(cache_ref);
        let again = run_node(&chain, &path, 64);
        assert_eq!(epochs(&again), fresh_epochs(&chain, 64));
    }

    impl Chain {
        fn hash_at_test(&self, h: u64) -> Option<[u8; 32]> {
            (&self).hash_at(h).ok().flatten()
        }
    }

    #[test]
    fn torn_corrupt_and_foreign_files_cost_a_rescan_never_a_wrong_answer() {
        let chain = Chain::new(100, 120);
        let path = tmp("torn");
        run_node(&chain, &path, 64);
        let full = std::fs::read(&path).unwrap_or_default();
        let header = CacheHeader {
            epoch_base: 100,
            chain_id: 82330,
            zebrad: "test|/Zebra:6.3.0/|v6.3.0".into(),
        };

        // Torn tail: cut mid-record.
        std::fs::write(&path, &full[..full.len() - 7]).unwrap_or_default();
        let (cache, blocks) = ZcashCache::open(&path, &header).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(blocks.len(), 119);
        assert_eq!(cache.tip(), Some(218));
        drop(cache);
        assert_eq!(
            epochs(&run_node(&chain, &path, 64)),
            fresh_epochs(&chain, 64)
        );

        // A flipped byte in the middle: cut there, the rest refetched.
        let mut bad = std::fs::read(&path).unwrap_or_default();
        let mid = bad.len() / 2;
        bad[mid] ^= 0x40;
        std::fs::write(&path, &bad).unwrap_or_default();
        let (cache, blocks) = ZcashCache::open(&path, &header).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            blocks.len() < 120 && blocks.len() > 30,
            "kept {}",
            blocks.len()
        );
        drop(cache);
        assert_eq!(
            epochs(&run_node(&chain, &path, 64)),
            fresh_epochs(&chain, 64)
        );

        // Another zebrad build: the file is reset.
        *chain.identity.borrow_mut() = Some("test|/Zebra:7.0.0/|v7.0.0-rc.0".into());
        chain.block_calls.set(0);
        assert_eq!(
            epochs(&run_node(&chain, &path, 64)),
            fresh_epochs(&chain, 64)
        );
        assert!(chain.block_calls.get() >= 120, "rescanned");

        // No identity: no cache at all, still correct.
        *chain.identity.borrow_mut() = None;
        let (view, cache) = prepare(&chain, &config(&path), 100, 64);
        assert!(cache.is_none());
        assert_eq!(view.replay_left(), 0);
    }

    #[test]
    fn a_stored_block_zebrad_now_describes_differently_discards_the_cache() {
        let chain = Chain::new(100, 50);
        let path = tmp("spot");
        run_node(&chain, &path, 64);
        // Same hashes, different contents (what a zebrad parser change
        // would look like): the CRC and the links still hold.
        for b in chain.blocks.borrow_mut().iter_mut() {
            b.time += 1;
        }
        let (view, cache) = prepare(&chain, &config(&path), 100, 64);
        assert_eq!(view.replay_left(), 0, "nothing replayed");
        assert_eq!(
            cache.as_ref().and_then(ZcashCache::tip),
            None,
            "file emptied"
        );
        drop((view, cache));
        assert_eq!(
            epochs(&run_node(&chain, &path, 64)),
            fresh_epochs(&chain, 64)
        );
    }

    #[test]
    fn append_refuses_a_gap_or_an_unlinked_block() {
        let path = tmp("append");
        let header = CacheHeader {
            epoch_base: 10,
            chain_id: 1,
            zebrad: "x".into(),
        };
        let (mut c, _) = ZcashCache::open(&path, &header).unwrap_or_else(|e| panic!("{e}"));
        let b10 = rich_block(10, [0; 32], 0);
        assert!(c.append(&rich_block(11, b10.hash, 0)).is_err(), "gap");
        assert!(c.append(&b10).is_ok());
        assert!(c.append(&rich_block(11, [3; 32], 0)).is_err(), "unlinked");
        assert!(c.append(&rich_block(11, b10.hash, 0)).is_ok());
        c.truncate_after(10).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.tip(), Some(10));
        c.truncate_after(5).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.tip(), None);
    }
}
