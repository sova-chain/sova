# Fast restart: a keeper restart must not cost a full Zcash rescan

Status: proposal (worker, 2026-10-03). For the orchestrator to implement in
`crates/engine` / `bin/sova` (and one small `crates/consensus` change, optional).
Acceptance test: `box/sim/restart-long-chain-scenario.sh` (fails on `release`
today; see §6). Measurement tool: `tools/scan-bench`.

## 1. The problem, precisely

On every start `bin/sova` runs **two** Zcash followers, and both start at the
epoch base `B` (4,388,500 on the public testnet):

| Follower | Where | Window | What it does with history |
|---|---|---|---|
| expectations | `engine::expectations::run_expectations` → `Follower::new(base, 1024)` | 1024 | feeds `ExpectedSettlements` (C5 + SIP-4 §7), `zcash_index` (SIP-4/SIP-7 precompile) and `votes` (SIP-8) with **every** epoch |
| sealer | `engine::driver::SealerCore::new(.., base, 100)` | 100 | drops every epoch more than `SETTLED_KEEP` (300) below the head (`process`: "a rescan from the base after a restart never queues it") |

Each `Follower::poll()` scans from `next_height` to the tip in one synchronous
call and returns its events only at the end. On a restart:

1. `run_expectations`' first poll fetches ~63,000 blocks (`getblockhash`,
   `getblock <hash> 1`, one `getrawtransaction <txid> 1` per tx), holding every
   `EpochData` **with all its transactions** in one `Vec` until it returns, then
   applies them. It runs inside an async task (`sova_tasks.spawn`), so it pins
   one tokio worker for the whole scan. The keeper is a CX23 (2 vCPU) whose
   runtime therefore has 2 workers, one of them blocked.
2. The sealer loop sleeps while `head_epoch_unknown(head)` (gate added for the
   restart-onto-stale-tip bug, 24484db). Nothing is sealed.
3. When the gate lifts, `SealerCore::process` calls **its own** follower's
   first `poll()`, which rescans the same ~63,000 blocks again from `B`, again
   synchronously on a worker, and throws all but the last 300 away.

So a keeper restart costs **two full scans in series** and grows linearly with
chain age. The board's data points fit two scans: 09-26 (~14k epochs)
expectations scan ~53 s, sealing held ~2 min; 10-03 (~63k epochs) ~11.5 min.

What history is actually needed after a restart:

- **`zcash_index`: all of it.** `ZcashSource::indexed_through` must be
  contiguous from `B`, and `txInfo`/`txOutput`/`burnInfo`/`blockAt`/SIP-7
  queries can name any height or txid in `Z[B ..= E_N]`. Every node must give
  the same answer, so this cannot be skipped or approximated.
- **`votes`: all of it.** `VoteStore::weights()` sums every stored vote.
- **`ExpectedSettlements`: in practice the last few hundred heights** for a
  restarted node (reth resumes from its persisted head and never re-validates
  old blocks; `effective_head` looks back 300; imports below
  `head − FINALIZED_DEPTH` don't happen). But `SovaConsensus` assumes "every
  height ≤ `scanned_through` has a record", so a gap would turn into
  `MissingRecord` holds. Keep it complete too.
- **The sealer follower: only `head − SETTLED_KEEP` and up.**

## 2. Measurements

### 2.1 What the restart costs (sim, release `3686661`)

`box/sim/restart-long-chain-scenario.sh`, run 1 (3,000 epochs, 2 tokio
workers like the CX23's 2 vCPUs, a +4 ms/request proxy in front of regtest zebrad so each block costs
~25 ms, i.e. the 3,000-epoch chain rescans like ~14k testnet epochs):

| phase (from the exec) | time |
|---|---|
| RPC up | 1.2 s |
| expectations rescan, sealer gate held (`waiting for our Zcash scan` ×39) | 0 → 76.6 s |
| sealer's own follower rescans from `B` again (gate lifted → first trigger) | 76.6 → 155.7 s |
| new epoch sealed | **156.2 s** (budget 60 s → FAIL) |

The two halves are equal: **the restart is two full scans in series**. With
2 workers RPC answered every 1 s probe (max latency 0.04 s) on the laptop.

With **1 worker** (1,500 epochs) the sim reproduces the testnet symptom
exactly: RPC first answered **74.8 s** after the exec (dead for the whole
restart), the sealer task never even logged its gate line (`waiting for our
Zcash scan` ×0: it was never scheduled), and the seal came at 77.2 s (two
~38 s scans). Why 2 workers were not enough on the keeper is not proven
(its 2 vCPUs are shared with the zebrad the scan saturates; a blocked tokio
worker also strands the task in its LIFO slot), but the mechanism is the
blocking scan on the async runtime. With the defaults (1 worker, 3,000
epochs) the restart sealed at 153.8 s, RPC first answered at 148.5 s, and A
fetched **6,003** blocks from zebrad (2 × 3,000 + 3). Precompile answers were
identical across the restart in every run.

### 2.2 Where the time goes (`tools/scan-bench`, regtest zebrad 6.3.0 in Docker)

The laptop's testnet zebrad (`127.0.0.1:18234`, pid 27947) **did not answer a
single RPC** during this work (60 s timeouts for over an hour): it has been in
that state since the SSD disconnect of 10-01 (board, 2026-10-01 17:50) and the
reconnect did not revive it. Its files on `/Volumes/Extreme Pro` are also not
readable from this session (macOS denies the volume: "Operation not
permitted"). The public RPC was off-limits to this session's permission
classifier. So the per-call numbers below are from a regtest zebrad
(`zfnd/zebra:6.3.0`, Docker on the laptop, 2,999 coinbase-only blocks), and the
testnet projection is calibrated against the keeper's measured 10-03 restart.

Production path (`Follower::poll` over `ZebradClient`, `--strict-pools`):
**288.8 blocks/s, 3.46 ms/block** (1 tx/block).

Per-RPC breakdown (same call sequence as `ZebradClient::block_at`):

| method | calls/block | mean ms | p50 ms | p99 ms | JSON parse | KB/call |
|---|---|---|---|---|---|---|
| `getblockhash` | 1 | 0.80 | 0.77 | 1.45 | ~0 | 0.1 |
| `getblock <hash> 1` | 1 | 1.35 | 1.31 | 2.32 | 0.02 ms | 4.3 |
| `getrawtransaction <txid> 1` | 1 per tx | 1.11 | 1.08 | 1.97 | 0.02 ms | 1.3 |

97 % of the wall time is the round trips themselves (request → zebrad →
answer); JSON parsing and block assembly are ~3 %. Each block costs
`~2.15 ms + ~1.1 ms × txs`. The work is latency-bound and strictly serial.

Alternatives over the same 2,999 blocks:

| mode | blocks/s | ms/block | 70k | 100k | 250k |
|---|---|---|---|---|---|
| production (`follower`) | 289 | 3.46 | 242 s | 346 s | 866 s |
| `parallel:4` (block_at on 4 threads) | 969 | 1.03 | 72 s | 103 s | 258 s |
| `parallel:8` | 1,439 | 0.70 | 49 s | 69 s | 174 s |
| `batch:50` (JSON-RPC batch arrays) | 885 | 1.13 | 79 s | 113 s | 282 s |
| `batch:200` | 943 | 1.06 | 74 s | 106 s | 265 s |
| `v2` (`getblock <height> 2`, 1 call/block) | 658 | 1.52 | 106 s | 152 s | 380 s |
| `v2par:8` | 3,145 | 0.32 | 22 s | 32 s | 79 s |
| `replay` (Follower over blocks already in memory) | 2.6 M | 0.0004 | 0.03 s | 0.04 s | 0.1 s |

(projections are per scan; today's restart does two.) zebrad 6.3.0 accepts
JSON-RPC batch arrays and `getblock <height> 2`; on these 500 blocks the
verbosity-2 parse was identical to `ZebradClient::block_at`
(`scan-bench --mode v2 --verify`), but regtest blocks are coinbase-only, so
that says nothing yet about shielded fields (§4, risk R6).

Compact on-disk size of the follower's input (the cache format of §3.1):
~300 B per coinbase-only block; testnet blocks with ~3 txs ≈ 0.9 KB → ~60 MB
at 63k, ~230 MB at 250k.

### 2.3 Testnet projection

The keeper's 10-03 restart: ~63,000 epochs, 18:33Z → 18:45Z ≈ 690 s, two
scans (as the sim shows) ⇒ **~5.5 ms/block ≈ 180 blocks/s per scan** on the CX23 (zebrad on the
same 2 vCPUs; ≈ regtest's 2.15 ms + ~3 txs × 1.1 ms). Today's code
(two serial scans):

| epochs | ≈ date | today (2 scans) | one scan (sealer fix only) | cache replay (recommended) |
|---|---|---|---|---|
| 63k | 2026-10-03 | ~11.5 min (measured) | ~5.8 min | seconds + new blocks since stop |
| 70k | ~10-04 | ~13 min | ~6.4 min | " |
| 100k | ~10-08 | ~18 min | ~9 min | " |
| 250k | ~10-25 at today's rate | ~46 min | ~23 min | " |

(Testnet has been producing ~9k epochs/day in burst periods; NU7's 25 s blocks
from 10-06 mean ~3,450/day steady.)

## 3. Options

### 3.1 Persist the follower's *input* and replay it (recommended core)

Persist each block exactly as `ZcashView::block_at` returned it (a
`BlockView`: height, hash, prev_hash, time, txs with outputs and shielded
summary, pools) in an append-only file in the datadir. On start, wrap
`ZebradClient` in a `CachedView` that answers `block_at(h)` from the file for
verified heights and from zebrad above them. The **unchanged** `Follower`
then replays history through the **unchanged** application code
(`zcash_index.insert`, `votes.insert`, `ExpectedSettlements::insert`,
withdrawals derivation) at memory speed, and continues live from where the
file ends.

Why the input, not the derived state (expectations map, index, votes):

- **One derivation path.** A restarted node builds its state with the same
  code a fresh rescan uses. Persisting the three derived stores means three
  formats that must each equal what the code would derive, and stay mutually
  consistent across crashes.
- **Rule changes replay correctly.** Burns (SIP-1/SIP-8 activation via
  `sip8_from`), SIP-7 strict-pool holds, the emission schedule and the
  withdrawals derivation are all applied at replay time under the running
  binary's config. Persisted derived state would bake in the old rules.
- **No crates/consensus change needed.** `CachedView` is a `ZcashView`.

Format (`<datadir>/sova/zcash-blocks.v1`, hand-rolled, no new dependency):

```
header  := magic "SOVAZCB\0" | format u32 | epoch_base u64 | chain_id u64
         | zebrad_build_len u16 | zebrad_build (getinfo.build / subversion)
record  := len u32 | crc32c u32 | payload            (len = payload bytes)
payload := height u64 | hash [32] | prev_hash [32] | time u32
         | pools: 0 | 1 BlockPools{chain_value[6]u64, delta[6]i64, supply u64, trees[3]u64}
         | ntx u32 | tx*
tx      := txid [32] | version u32 | nout u32 | (value u64 | slen u32 | script)*
         | n_in u32 | summary: 0 | 1 {deltas[4]i64, sapling_spends u32, sapling_outputs u32,
                                      orchard_actions u32, ironwood_actions u32, joinsplits u32}
```

Every field of `BlockView`/`TxView`/`TxShielded`/`BlockPools` round-trips;
encode/decode lives in `engine` (new module `zcash_cache.rs`) with a
round-trip test per field and a "decode(encode(b)) == b" property over
arbitrary blocks. ~60 MB at 63k epochs.

Writes and crash safety:

- Append-only. After each poll's events are applied, append the new blocks
  and `fdatasync` (one write per poll; a chunk of 500 during catch-up).
- Load reads records in order and stops at the first record that is torn
  (length past EOF), fails its CRC, is not `height = previous + 1`, or whose
  `prev_hash` is not the previous record's `hash`; the file is truncated
  there. The cache is an accelerator: losing its tail only means refetching.
- A header mismatch (format, `epoch_base`, chain ID, **zebrad build**)
  discards the file and the node rescans once (see R2 for why the zebrad
  build is in the header).
- `SOVA_ZCASH_CACHE=off` disables it; `SOVA_ZCASH_CACHE=rebuild` discards it.

Reorgs:

- `FollowerEvent::Rollback { to_height }`: apply it in memory (as today),
  then truncate the file to the end of record `to_height` (an in-memory
  `Vec<u64>` of record offsets, 8 B/epoch, gives the offset) and fsync.
- A crash between the in-memory rollback and the truncate leaves a stale
  tail on disk; the startup verification below finds it, as it finds any
  reorg that happened while the node was down.

Proving the file matches this node's zebrad (startup):

1. Load and link-check the whole file (above). Every record is then on one
   hash-linked chain from `B` (each `prev_hash` equals the previous `hash`).
2. Walk down from the file's tip: `getblockhash(h)` until it equals the
   record's hash. That height is `verified_tip`; truncate above it. One call
   normally; at most `REORG_WINDOW` (1024) before giving up and treating the
   reorg as deeper than the window (truncate to `B − 1`: a full rescan, exactly
   what the follower itself does for a reorg deeper than its window).
   Because the records are hash-linked, matching at `verified_tip` proves
   every record below it is on zebrad's current canonical chain.
3. Spot-check contents: refetch the record at `verified_tip`, the 32 records
   below it and 32 random older records with `ZebradClient::block_at` and
   require `BlockView` equality; any difference discards the file (rescan,
   loud error log). This catches a parser/format change that the format
   number missed, which the hash chain cannot (we store the hash zebrad gave
   us, not a header we could recompute it from).
4. `CachedView::block_at(h)` serves the file for `h ≤ verified_tip` until the
   follower has asked for `verified_tip` once (the replay is a forward scan
   in order), then goes live for good: every later call, including window
   revalidation, hits zebrad. A reorg below `verified_tip` that lands during
   the few seconds of replay is then caught by the next poll's window
   revalidation, exactly like a reorg landing mid-scan today.

Restart cost: load + link-check ~60 MB (< 1 s), 1-65 RPCs of verification,
replay (CPU only: the follower itself is ~0.4 µs/block; the engine-side
inserts are a few µs/block, so ~0.5 s at 63k, ~2 s at 250k), then a live scan
of the blocks mined while the node was down (~5.5 ms each on the keeper).
Memory: unchanged (the stores are rebuilt in memory as today).

Trade-offs: the first start of a binary with the cache still pays one full
scan (no file yet). Disk grows ~1 KB/epoch. New code on the consensus input
path (mitigated by the replay-through-the-same-code design, verification,
and the parity test in §5).

### 3.2 Incremental emission + off the async runtime

Two independent changes, both consensus-neutral:

- **Chunks.** Wrap the view so `tip_height()` returns
  `min(real_tip, next_height + CHUNK − 1)` (the follower reads the tip once
  per poll, so each poll scans at most `CHUNK` blocks), and have the loop
  re-poll without sleeping while it is behind. No `crates/consensus` change.
  Events are applied every `CHUNK` blocks: memory stays bounded (today the
  whole history's transactions sit in one `Vec` until the scan ends),
  `scanned_through`/`indexed_through` advance during the scan (and a log
  line per chunk shows progress, so a scanning keeper never again looks
  hung), and the cache of §3.1 is appended per chunk, so an interrupted
  first scan keeps its progress.
- **`spawn_blocking`** (or one dedicated `std::thread` per follower loop) for
  `follower.poll` in `run_expectations` and for `core.process` in
  `run_sealer`: the scan stops pinning a tokio worker, so RPC, P2P and the
  engine keep running during it.

Alone, this does **not** make the keeper seal sooner: the head's epoch is the
last one the scan reaches, so `head_epoch_unknown` still lifts at the end of
a full scan. It is necessary for RPC responsiveness and for a sane first
scan, not sufficient for the 1-minute goal.

Consensus notes: progressive `scanned_through` is the steady-state behaviour
(the watermark already advances block by block once caught up), so it adds no
new state. Each poll re-validates the window top with a full `block_at`
(3+ RPCs): one extra block per chunk, negligible at `CHUNK ≥ 200`.

### 3.3 Batching / parallel zebrad requests

Measured above: 4-8 parallel fetchers give 3.3-5x, batch arrays ~3.2x,
`getblock <height> 2` 2.3x (more with more txs per block, since it removes
the per-tx calls), and both combined ~11x.

- **Parallel prefetch** can sit in a `PrefetchView` that fetches
  `next..next+K` with `ZebradClient::block_at` on K threads and serves them
  to the follower in order, going live for any height at or below one it
  already served (so window revalidation is never answered from a prefetch).
  A prefetched block from before a reorg is just a block of the old branch;
  the parent-linkage check and the next poll's revalidation handle it as
  they handle a reorg mid-scan today. Consensus-neutral (same parser), ~3-5x.
- **Verbosity 2 / height-addressed `getblock`** change *which zebrad answer*
  the parser reads. Equal on regtest coinbase blocks; unproven on testnet
  blocks with Sapling/Orchard/JoinSplit data (R6). Not before NU7.

Even 11x does not reach one minute at 250k epochs for a full rescan (~80 s
per scan on the laptop, more on the CX23 with zebrad sharing 2 vCPUs), and
cost still grows with chain age. Useful later for a fresh node's first sync
(seeds, joiners), not the restart fix.

### 3.4 Sealer follower starts near the head (trivial, do it regardless)

`SealerCore` discards everything below `head − SETTLED_KEEP`, so starting its
follower at the Zcash height of Sova height `head − 2 × SETTLED_KEEP`
(`base + head − 601`, floored at `base`) changes nothing it acts on and
removes the second full scan. `SealerCore::new` is constructed after the node
is built, so `node.provider.best_block_number()` is available. A reorg deeper
than its window now unwinds to that start height instead of `B`; the queue
retains `h ≤ to_height` either way, and nothing below `head − 300` is ever
queued, so the result is the same. The head used must be the head at start
(the effective head can only be lower than it by `STALE_SCAN_MAX` = 300 or by
a Zcash rollback floor; 2 × 300 covers the former, and a rollback deeper than
600 Zcash blocks is beyond Zebra's own limits).

### 3.5 Rejected for now: scan the recent window first, backfill history later

Expectations for the last ~1,000 heights would let the sealer seal in
seconds, but the precompile needs contiguous coverage from `B` before any
block that calls it can be built or validated, and `scanned_through`'s
contiguity is what `SovaConsensus` relies on. Two watermarks and out-of-order
coverage is a bigger, riskier change than §3.1 and buys nothing §3.1 doesn't.

## 4. Consensus-relevant risks

- **R1 Cache ≠ zebrad.** A record that differs from what `block_at` would
  return now (disk corruption, a bug in encode/decode, a stale format)
  silently changes this node's SIP-4/SIP-7 answers and C5 records. Mitigations:
  CRC per record, hash linkage, tip verification, content spot-checks (§3.1
  step 3), exhaustive round-trip tests, the parity test of §5, and the sim's
  check that precompile answers are identical across a restart.
- **R2 zebrad upgrade changes historical answers.** NU7 (10-06) adds the
  `ironwood` pool. If an upgraded zebrad reports *historical* blocks
  differently (e.g. a 7th `valuePools` entry, which `parse_block_pools`
  rejects → `pools: None` → strict-pool hold), a fresh rescan and a cached
  replay would diverge. Hence the zebrad build string in the header: a
  zebrad upgrade discards the cache and forces one rescan. **The NU7 zebrad
  upgrade will therefore cost each node one full scan** (§3.2 keeps it
  responsive; plan the keeper's for a quiet hour, or pre-build, §5 step 6).
- **R3 Reorg while down / during replay.** Covered by the startup walk-down
  (≤ 1024) and by going live after replay; deeper than the window falls
  back to a full rescan, as the follower already does.
- **R4 Crash between rollback and truncate, or mid-append.** Stale or torn
  tails are cut at load (link/CRC) or by the walk-down.
- **R5 Partial state at startup.** With chunked emission, consumers see
  `scanned_through`/`indexed_through` grow from `B`. Imports above the
  watermark are held (`Unscanned`), precompile calls above coverage are
  Fatal (block refused, retried), the sealer gate still waits for the head's
  epoch. All are existing, tested states. (Observation, not new:
  `check_settlements` returns `Ok` while `scanned_through()` is `None`, even
  on a node whose follower is enabled; on testnet SIP-6's timestamp rule
  holds such blocks with `MissingRecord`, so it is covered there, but the
  orchestrator may want `check_settlements` to consult
  `expectations.enabled()` as `head_epoch_unknown` does.)
- **R6 Changing RPCs.** `getblock <h> 2` / batch answers must parse
  bit-identically to today's `getblockhash` + `getblock 1` +
  `getrawtransaction 1` path on real testnet history (Sapling, Orchard,
  JoinSplit txs), or two nodes using different paths answer the precompile
  differently. `parse_tx_shielded` is lenient (a missing field counts as
  zero), so a difference would be silent. Gate any such switch on
  `scan-bench --mode v2 --verify` over the full testnet range.
- **R7 Sealer follower start.** §3.4: identical behaviour for every epoch
  the sealer acts on; the only difference is where a deeper-than-window
  reorg unwinds to.
- **R8 ureq has no timeouts.** A zebrad that stops answering (the laptop's
  did) blocks a follower forever. Off the runtime (§3.2) this no longer
  freezes RPC, but `ZebradClient::new` should set connect/read timeouts
  (e.g. 30 s) so the loop logs and retries.

## 5. Recommendation for v0.1.18 (by 2026-10-05)

All of §3.4 + §3.2 + §3.1; §3.3 later.

1. **Sealer follower near the head** (`crates/engine/src/driver.rs`
   `SealerCore::new` takes a `start_height`; `bin/sova/src/main.rs` passes
   `max(base, base + head.saturating_sub(2 * SETTLED_KEEP) - 1)` from
   `node.provider.best_block_number()`). Halves today's restart on its own.
2. **Off the runtime + chunked** (`run_expectations`, `run_sealer`): run each
   `poll`/`process` in `tokio::task::spawn_blocking` (move the follower/core
   in and out) and cap each expectations poll at `CHUNK = 500` blocks via a
   tip-capping view wrapper; loop without sleeping while behind; log
   `zcash scan: through H (N blocks/s)` per chunk.
3. **`engine::zcash_cache`**: the file of §3.1 (`ZcashCache::open(datadir,
   base, chain_id, zebrad_build)`, `load() -> verified records`,
   `append(&[BlockView])`, `truncate_after(height)`), and
   `CachedView<V: ZcashView>` (serves verified records until it has served
   `verified_tip`, then live; records each live block it returns so the loop
   can append after applying). `run_expectations` takes the cached view and
   the cache handle; the sealer keeps a plain `ZebradClient` (it only reads
   ~600 blocks). Note: `EpochData` carries no `prev_hash`; append the
   `BlockView`s from the view (it sees them) rather than reconstructing them
   from events, or add `prev_hash` to `EpochData` (the one
   `crates/consensus` change this design could use; optional).
4. **Tests**: (a) round-trip every field; (b) **parity**: on a mock chain
   with burns, v2 burns, pools and reorgs, state built by a fresh rescan ==
   state built by cache replay + live tail, for `ExpectedSettlements`
   records, `zcash_index` (blocks, txs, summaries, `indexed_through`) and
   `votes` (`weights`, `at`); (c) torn/CRC/unlinked tails truncate; (d) tip
   reorged while down → walk-down + rollback; (e) header mismatch → rescan;
   (f) spot-check mismatch → rescan.
5. **Gate**: `restart-long-chain-scenario.sh` must pass (seal ≤ 60 s, RPC up,
   answers unchanged, ≤ 1,000 block fetches: the sealer's ~600, the ~65
   spot-checks and the window revalidation fit), plus the regular suite and `restart-reorg-scenario.sh`
   (the stale-tip gate must still hold on a resumed node).
6. **Deploy note.** The keeper's first start on v0.1.18 has no cache: it
   pays one full scan (~6 min at 70k with step 1, RPC responsive with step 2).
   Optionally add `sova zcash-cache build` (read-only against the local
   zebrad, run while the old keeper still seals) so the switch-over restart
   only replays. The NU7 zebrad upgrade invalidates caches (R2): do the
   keeper's v0.1.18 restart after its zebrad upgrade, not before, or it pays
   twice.

Expected keeper restart with 1-3 at 63k-250k epochs: < 5 s of replay and
verification, plus ~5.5 ms per Zcash block mined while it was down (a 5-minute
outage at 25 s blocks is 12 blocks). Every node's SIP-4/SIP-7/SIP-8 answers
come from the same derivation code over byte-identical inputs.

## 6. Acceptance test

`box/sim/restart-long-chain-scenario.sh`; run with

```
SOVA_BIN=$PWD/target/release/sova \
SOVA_MINER_BIN=$PWD/target/burn-wallet/release/sova-miner \
  box/sim/restart-long-chain-scenario.sh
```

Defaults: 3,000 epochs, 1 tokio worker, +4 ms/request counting proxy,
60 s budget, ≤ 1,000 block fetches. A run takes ~15 min, ~12 of them regtest
zebrad mining the 3,000 blocks (~4.5 blocks/s).

Results on `release` `3686661` (2026-10-03):

| run | workers | epochs | sealed after | RPC first answer | blocks fetched | result |
|---|---|---|---|---|---|---|
| 1 (2-worker variant) | 2 | 3,000 | 156.2 s (gate 76.6 s + sealer scan 79.2 s) | 1.2 s, 0 failed probes | not counted yet | FAIL (4a) |
| 2 (1,500 epochs) | 1 | 1,500 | 77.2 s | 74.8 s | not counted yet | FAIL (4a), (4b) |
| 3 (defaults) | 1 | 3,000 | **153.8 s** | **148.5 s** | **6,003** (= 2 × 3,000 + 3) | **FAIL (4a), (4b), (4d)**; (4c) PASS |

What makes it pass: (a) seal ≤ 60 s, (b) RPC up ≤ 30 s and every probe
answered within 5 s, (c) precompile answers unchanged across the restart,
(d) ≤ 1,000 `getblock` calls from the exec to the seal. (d) is what keeps
the test honest about scaling: §3.4 alone (one scan, ~77 s) or §3.3
(a 5x faster rescan, ~30 s) could pass (a) at 3,000 epochs and still take
minutes at 250k; only a restart whose zebrad work is independent of chain
length passes (d). Not in nightly yet: it fails until the fix lands.
