# Node memory: what grows, what is bounded

2026-09-27. Code-reading audit of `bin/sova`, `crates/engine`,
`crates/consensus`, `crates/evm` at `release` `04547ff` (v0.1.12), plus the
reth v2.6.0 code Sova configures (checkout `73a3a00`). Nothing here changes
code. Two numbers were measured locally: the per-block heap cost of the two
unbounded Sova stores, and what `alloc_zeroed` does at reth's cache alignment
(see [Method](#method)).

Symptom: after v0.1.12 (cross-block cache capped at 256 MiB) the `sova`
process is 375-391 MB right after a restart and 415-445 MB about 80 minutes
later, on all four nodes (keeper, seed-1, seed-2, rpc-1). That is roughly
30-60 MB/h while the chain moves about 300 blocks/h.

**Update from the RSS log ([below](#rss-log)):** over the next 30 minutes
three nodes grew by 1-2 MB, and seed-1 dropped by 128 MB without a restart.
The early slope was warm-up, and it has leveled off. There is no fast leak.
What still grows without bound is small (~0.7 MB/h, points 1 and 5 below).

## Summary

1. **No Sova structure grows fast enough to explain 30-60 MB/h, and the log shows the slope has stopped.** The two
   Sova stores that are never pruned, `ZcashIndex` and `ExpectedSettlements`,
   cost about 2.4 KB per Sova block when the keeper burns every epoch
   (measured). At 300 blocks/h that is about 0.7 MB/h, 7-17 MB/day at
   3,000-7,000 blocks/day, and about 4 GB/year at 5,000/day. They are the
   long-term problem, not today's slope.
2. **The 256 MiB cache commits 136.5 MiB the moment it is created.** reth's
   execution cache is three `fixed-cache` tables with 128-byte-aligned buckets.
   Rust's `System::alloc_zeroed` handles an alignment above 16 with
   `posix_memalign` and then an explicit memset, so every page is touched
   and resident at once. Measured: 256 MiB `alloc_zeroed` at align 16 adds
   0 MB of RSS, at align 128 it adds 256 MB. That also explains the
   2026-09-27 incident exactly: a 4 GiB setting makes a 2.13 GiB table, plus
   about 0.3 GB of everything else, gives the 2.4 GB seen 17 minutes after a
   restart. The cache is bounded and does not add to the slope, but it is
   the largest single item in the 375-391 MB baseline. A new instance is
   built whenever the saved one is busy or missing, and the old one is freed
   later, so RSS briefly holds two.
3. **What is left for the slope is reth's own warm-up plus the allocator and
   file-backed pages.** reth's RPC caches take every canonical block
   whether or not anyone calls RPC: fee history is 7.2 KB/block up to 1,124
   blocks, blocks up to 5,000, receipts up to 2,000. Together that is
   about 3 MB/h for the first ~4 h, then under 0.5 MB/h, then flat at about
   20 MB. The rest is not visible in code: glibc `malloc` arenas (Sova runs
   the system allocator, while reth's own binary ships jemalloc) and MDBX's
   `WRITEMAP` mapping, whose written pages count in the process's RSS as
   file-backed memory. Both level off. File-backed pages can be reclaimed
   and are not a leak.
4. **One measurement settles point 3** (orchestrator, read-only, on any
   host): `grep -E 'Rss(Anon|File|Shmem)' /proc/$(pidof sova)/status` and
   `du -sh <datadir>/db` a few hours apart. If `RssFile` carries the growth,
   it is MDBX/static-file page cache and the node is healthy. If `RssAnon`
   carries it, set `MALLOC_ARENA_MAX=2` on one seed and compare. With the
   slope now flat, this is for understanding the node, not urgent.

Fix order: (a) `SOVA_CROSS_BLOCK_CACHE_MB=32` (config; saves about 119 MiB
per node now and shrinks every rebuild); (b) `MALLOC_ARENA_MAX=2` in
`sova-node.env` (config), or jemalloc as reth ships it (bin change, not
consensus); (c) prune `ExpectedSettlements` below head - 1024 (consensus
path, orchestrator); (d) a persistent or bounded `ZcashIndex`
(consensus-visible, before mainnet). Details are in
[Recommended fixes](#recommended-fixes).

## Ranked: what can make RSS grow over time

The ranking is by likely share of the post-restart warm-up (the first ~75 minutes; the log shows it leveled off after that). Rates
assume ~300 Sova blocks/h (observed) and 3,000-7,000/day (Zcash testnet),
with the keeper burning every epoch, so a typical Zcash block holds the
coinbase and one burn tx.

| # | What | Where | Grows with | Bounded? | Est. share of the slope |
|---|------|-------|------------|----------|-------------------------|
| 1 | glibc malloc arenas: fragmentation, and freed chunks kept in per-thread arenas (no jemalloc) | `bin/sova/src/main.rs` (no `#[global_allocator]`); reth's `bin/reth/src/main.rs:3-4` sets one | allocation churn: follower polls, JSON parsing, block execution, cache rebuilds | levels off at the arenas' high-water marks (8 x cores arenas) | **likely the largest anonymous part**, perhaps 10-40 MB/h early (inferred from the mechanism; not visible in code) |
| 2 | MDBX `WRITEMAP` + static-file mmaps: pages the process touched in the DB files | reth `crates/storage/db/src/implementation/mdbx/mod.rs:409-411` | DB size and working set | limited by the DB size; kernel can reclaim | **likely large**, roughly datadir growth plus the working set (inferred; measure `RssFile`) |
| 3 | reth RPC caches fed by every canonical block: fee history, blocks, receipts, tx-hash index | reth `rpc-eth-types/src/cache/mod.rs:789-808`, `fee_history.rs:175-178,198`, `rpc-server-types/src/constants.rs:118-133` | blocks | yes: 1,124 / 5,000 / 2,000 / 100,000 entries | ~3 MB/h for the first ~4 h, then <0.5 MB/h to ~17 h, then 0 (cap ~20 MB) |
| 4 | `ZcashIndex` (SIP-4/7 precompile store) | `crates/engine/src/zcash_index.rs:38-43`, insert `:74` | Zcash blocks and txs since the epoch base | **no** | ~1.2 KB/block with a burn tx, so ~0.35 MB/h at 300/h (measured) |
| 5 | `ExpectedSettlements` (C5 records) | `crates/engine/src/expectations.rs:120`, insert `:470` | Sova heights | **no** ("history is never pruned", `:24-30`) | ~1.2 KB/block with a burn, 0.44 KB without, so ~0.35 MB/h (measured) |

Items 4 and 5 together hold about 39 MB today (16.4k heights × 2.4 KB) and
are rebuilt by the rescan on every start. That is part of the baseline, not
the slope.

Not in the slope but the biggest static item: **the execution cache**
(reth `engine/execution-cache/src/cached_state.rs:1130-1150`, `fixed-cache`
`lib.rs:161-169`). It commits 136.5 MiB when created at 256 MiB, and
another 136.5 MiB for a while each time reth builds a replacement before
dropping the old one. See [the section below](#the-execution-cache-a-step-not-a-slope).

## Findings in detail

### 1. glibc arenas (no jemalloc)

`bin/sova/src/main.rs` installs no global allocator, so the release binary
(built on ubuntu:20.04 glibc, `scripts/build-linux-release.sh`) uses glibc
`malloc`. reth's own binary uses jemalloc (`bin/reth/src/main.rs:3-4`,
`reth_cli_util::allocator`) to avoid exactly this. glibc keeps up to
8 × cores arenas (16 on a 2-vCPU host, 32 on a 4-vCPU host) and returns freed memory only
from the top of each heap. Churn therefore leaves RSS at each arena's
high-water mark, and the arena heaps fill in over hours. Sova-specific
sources of that churn:

- Each follower poll re-fetches the entire window-top block to compare one
  hash (`crates/consensus/src/follower.rs:201-203` calls `block_at`, which
  in `zebrad.rs` runs `getblock` plus one `getrawtransaction` per tx, all
  parsed into `serde_json::Value`). Two followers run on the keeper
  (expectations and sealer), one on the other nodes, each every 2 s. The
  calls are blocking `ureq` requests inside tokio tasks, so they land on
  whichever worker thread, and so whichever arena, is current.
- `ExpectedSettlements::record()` clones a whole `HeightRecord` (burns,
  ranked, withdrawals, boxed pools) on every check: three to four per
  import (`consensus.rs:169,191,266`, `validator.rs:137,162`) and one per
  step of every `effective_head` walk, which the sealer, arbiter, canonical
  reader and ranker each call.
- Every execution-cache rebuild allocates the 8 MiB account table and the
  0.5 MiB code table. Once one such chunk has been freed, glibc's dynamic
  mmap threshold (at most 32 MiB) rises above them, so later ones can come
  from an arena and stay there after `free`. The 128 MiB storage table is
  always mmapped and goes back to the kernel.

Bound: the arenas' high-water marks, so it levels off. Fix: config
`MALLOC_ARENA_MAX=2` in `/etc/sova/sova-node.env` (no code), or jemalloc as
reth ships it: `reth-cli-util` with its `jemalloc` feature and the two lines
from `bin/reth/src/main.rs`. That is a bin change, not consensus. Reducing
the churn (fetch only `getblockhash` to re-validate the window; hand out
`Arc<HeightRecord>` instead of clones) is also safe, but `follower.rs` is
consensus input, so it needs orchestrator authorship.

### 2. File-backed RSS: MDBX `WRITEMAP`, static files, the binary

reth opens MDBX read-write with `write_map()`
(`crates/storage/db/src/implementation/mdbx/mod.rs:409-411`), so every page
it writes goes through a shared mapping of `mdbx.dat`. Static files
(headers, bodies, receipts) are mmapped too (`nippy-jar`). `ps rss` counts
those resident file pages, and they grow as the DB grows and as more of it
is touched. They are page cache: the kernel reclaims clean pages under
pressure, which is why the `mem_low` alert on `MemAvailable` (not RSS) is
the right one. The size of this component has to be measured
(`RssFile`, `du -sh <datadir>/db`). Nothing in code bounds it except the DB
size.

### 3. reth RPC caches, fed by every canonical block

`cache_new_blocks_task` (`rpc-eth-types/src/cache/mod.rs:789-808`) and
`fee_history_cache_new_blocks_task` (`rpc/src/eth/builder.rs:557`) insert
every committed block whether or not anyone calls RPC:

- Fee history: `rewards: Vec<u128>` with `100 × resolution + 1 = 401`
  entries, even for an empty block (`fee_history.rs:175-178`, `:325`),
  so about 7.2 KB/block with the header, capped at `MAX_HEADER_HISTORY + 100
  = 1,124` blocks (`:198`). Cap about 8 MB, reached after about 3.7 h at
  300 blocks/h.
- Blocks: `DEFAULT_BLOCK_CACHE_MAX_LEN = 5000` (`constants.rs:118`), about
  1.5 KB per Sova block (header with the 97-byte seal, a couple of
  withdrawals, rarely txs). Cap about 7.5 MB, reached after ~17 h.
- Receipts 2,000 (`:121`) and the tx-hash index of 100,000 (`:133`): small
  on Sova, which carries few transactions.

Fix (config, safe): lower `node_config.rpc.rpc_state_cache.max_blocks` and
`max_receipts`, and the fee-history `max_blocks` (reth flags
`--rpc-cache.max-blocks`, `--rpc-cache.max-receipts`). Worth about 15 MB at
most, so low priority.

### 4. `ZcashIndex`: unbounded, and consensus reads all of it

`crates/engine/src/zcash_index.rs:38-43`: `blocks: BTreeMap<u64, Block>`
(hash, time, `txids: Vec<[u8;32]>`, `Arc<BlockSummary>`) and
`txs: HashMap<[u8;32], Arc<IndexedTx>>` (height, index, version, every
transparent output with its script, the parsed burn, `TxShielded`). One
entry per Zcash block and per Zcash tx since the epoch base. Removed only by
a Zcash reorg (`unwind_above`). Rebuilt from zebrad on every start
(`:12-15`).

Measured (RSS delta over 200k synthetic testnet-shaped blocks): **~770 B
per block** (block, summary, coinbase), **~440-590 B per further tx** (two
P2PKH outputs). Block with coinbase and keeper burn: ~1.2 KB. At 300
blocks/h that is ~0.35 MB/h, 3.5-8 MB/day at 3-7k blocks/day. At mainnet's
~1,150 blocks/day and ~20 tx/block it would be ~10 KB/block, about
11 MB/day and 4 GB/year. The `HashMap` doubles its table as it grows, so
RSS moves in steps.

Readers (verified): the SIP-4/7 precompile at any height in `[B, E_N]` and
any txid since `B` (`crates/evm/src/zcash.rs`, `blockAt`, `txInfo`,
`txOutput`, `burnInfo`, `poolValue`, `blockStats`, `txShielded`),
`eth_call` at historical blocks, and `sova_getZcashBlocks`
(`bin/sova/src/zcash_feed.rs`). **Pruning it changes EVM results.** A
contract asking about a Zcash tx older than the cut would get NOT_FOUND on
a pruned node and OK on an unpruned one: a consensus split. Options, none
urgent on testnet:

- **Persistent store** (redb, as `docs/design/sip4-evm-seam.md` plans
  "once rescans get slow"). Answers stay identical, so it is not a consensus
  change, and it also removes the rescan from startup. It feeds the
  precompile, so it needs orchestrator authorship.
- **A protocol lookback window** (heights or txs older than `E_N - W`
  answer OUT_OF_RANGE / NOT_FOUND), like the `RING = 8191` that SIP-7's
  `ZcashBlocks` contract already has. This is a SIP amendment and a hard
  fork.
- **Trims that change no answer**: keep `Block::txids` only for the last
  `REORG_WINDOW` blocks (it exists only to unwind), which saves 32 B/tx.
  Store outputs as one `Box<[u8]>` instead of `Vec<(u64, Vec<u8>)>`.
  Perhaps 20-30% in total. Not consensus, but it is the precompile's store,
  so orchestrator review.

### 5. `ExpectedSettlements`: unbounded, and safe to prune below head - 1024

`crates/engine/src/expectations.rs:120`, `map: Mutex<BTreeMap<u64,
HeightRecord>>`. One record per Sova height: `withdrawals` (rank-0
derivation), `epoch: EpochData` (hash, time, burns, `pools:
Option<Box<BlockPools>>`, and `txs`, which is emptied at `:456`), and
`ranked`. Removed only above a Zcash rollback (`unwind_above`, `:192`).

Measured: **~440 B/height burn-less, ~1.2 KB/height with one burn**
(`size_of::<HeightRecord>()` = 152, `EpochData` 104, `EpochBurn` 104,
`MinerWeight` 64, `Withdrawal` 48, `BlockPools` 128 boxed). ~0.35 MB/h at
300/h. The `pools` box (~140 B/record) is never read from a record (every
reader uses `hash`, `time`, `burns`, `ranked`, `withdrawals`), so dropping
it is free.

Readers of old heights (all verified):

| Reader | Heights it reads |
|---|---|
| `SovaConsensus::check_settlements` / `validate_header_against_parent` (`consensus.rs:169,191,266`) | any imported block's height: on a synced node, the head and the few above/below it; on a syncing node, every height above its own reth head |
| `validator.rs:137,162` (payload import, candidate rank) | the imported height |
| `effective_head` / `is_stale` (`expectations.rs:209-251`; called from `main.rs:444,462,567,589,758`, `zcash_feed.rs:507`, `miner.rs:213`) | head - `STALE_SCAN_MAX` (100) .. head |
| `canonical_record` via `seed_canonical` (`main.rs:1014`, `candidates.rs:231-251`) | height - `MAX_REPLACE_DEPTH` (3) .. height |
| `rerank` (`expectations.rs:480`) | the newly scanned height |
| arbiter safe/finalized lag | reads reth, not the map |

Nothing reads more than `FINALIZED_DEPTH` (100) below the reth head, and
reth refuses any head below the finalized block it was given ("too deep
reorg", `candidates.rs:66-73`). **Safe bound: drop records below
`reth_head - 1024`** (the follower's `REORG_WINDOW`, well past every depth
above), keyed on the reth canonical head, never on the scan watermark, so a
node that is still syncing keeps every height above its own head. What
changes: a block below `head - 1024` offered to a synced node gets
`MissingRecord`. That is a hold, transient by design (`consensus.rs:61-68`,
`is_transient_error`), so it is never cached invalid. Such a block could
not become canonical anyway. The `MissingRecord` doc ("should be
impossible (history is never pruned)") and the module doc (`:24-30`) need
updating. A restart rebuilds the full map from the base; the prune then
trims it on the next insert. **Consensus-sensitive** (it sits in the C5
enforcement path), so orchestrator authorship. Worth ~1.2 KB/height: about
39 MB today, and the difference between flat and ~4 GB/year.

### The execution cache: a step, not a slope

reth v2.6.0's cross-block cache (`--engine.cross-block-cache-size`, set at
`bin/sova/src/main.rs:281` from `state_cache.rs`) is
`ExecutionCache::new(total)` (`cached_state.rs:1130-1150`). It splits the
total 88.88% storage, 5.56% accounts and 5.56% code, sizes each as a
power-of-two count of 128-byte buckets rounded **down**
(`bytes_to_entries`, `:1117`, with entry sizes from `:33-77`), and
allocates each with `fixed_cache::Cache::new`, which is `alloc_zeroed` of
`Bucket`s that are `#[repr(C, align(128))]` (`fixed-cache-0.1.10`
`lib.rs:161-169`, `:571`). With alignment above 16, Rust's
`System::alloc_zeroed` does not use `calloc`: it calls `posix_memalign` and
then zeroes the whole block, so all of it is resident at once.

| Setting | storage | account | code | committed at creation |
|---|---|---|---|---|
| 4 GiB (reth default) | 2 GiB | 128 MiB | 2 MiB | **2.13 GiB**, which is the 2.4 GB incident |
| 256 MiB (v0.1.12) | 128 MiB | 8 MiB | 0.5 MiB | **136.5 MiB** |
| 64 MiB | 32 MiB | 2 MiB | 0.5 MiB | 34.5 MiB |
| 32 MiB (minimum Sova accepts) | 16 MiB | 1 MiB | 0.5 MiB | **17.5 MiB** |

When a new instance is built: `PayloadProcessor::cache_for` misses because
the saved cache is still held by a prewarm task, or there is none
(`payload_processor/mod.rs:462-474`, `:513`). The old instance is dropped
later, so RSS carries two for a while. On a parent-hash mismatch (a fork
block: a sibling candidate, or a block re-submitted after a hold) the saved
cache is instead `clear()`ed (`execution-cache/src/lib.rs:89-95`), which is
O(1) except every 1,024th time, when `clear_slow` rewrites every bucket
(`fixed-cache` `lib.rs:255-285`). Note that reth runs consensus
(`validate_block_pre_execution`, which holds on SIP-4 anchor mismatch)
**concurrently** with acquiring the cache (`payload_validator.rs:519`), so
held blocks that are re-submitted still take and clear it.

Sova's per-block working set is tiny: about nine storage slots written
(six in SIP-7's `ZcashBlocks` ring at 0x5A01, two for EIP-4788, one for
EIP-2935) and a handful of accounts. A 32 MiB cache (131,072 storage
buckets) is ample. **Fix: `SOVA_CROSS_BLOCK_CACHE_MB=32`** in
`sova-node.env` (config, no rebuild, not consensus: the cache is a read
cache), or lower `DEFAULT_CROSS_BLOCK_CACHE_MB` in `state_cache.rs` (bin
change, not consensus). That saves about 119 MiB per node now and cuts the
cost of each rebuild by 8×. The `state_cache.rs` doc should say what the
setting commits, not only the cap.

## Everything else checked: bounded

| Structure | Where | Bound | Worst case |
|---|---|---|---|
| Follower reorg window | `follower.rs:130`, trimmed `:269-271` | `REORG_WINDOW` 1024 × 40 B (`expectations.rs:50`); the sealer's follower keeps 100 (`main.rs:749`) | 40 KB |
| Follower `last_pools` | `follower.rs:143` | one block | 128 B |
| SIP-8 `VoteStore` | `votes.rs:51` | unbounded by code, but **empty**: `activate_sip8` has no caller in `bin/sova`, so no v2 burn is recognized | 0 |
| Candidate tracker `seen` / `unranked` | `candidates.rs:183,194`, trimmed `:309-314`, `:338-343` | `RETAIN` 1024 heights (`:36`) | ~0.5 MB |
| Sync targets | `candidates.rs:770-781` | 64 | tiny |
| Arbiter channel | `candidates.rs:615-622` (unbounded mpsc) | drained continuously by `run_arbiter` | tiny |
| SIP-6 seal journal | `signer.rs:29,206` | 1024 **files on disk**, not memory | 0 |
| SealerCore queue (keeper) | `driver.rs:200`, trimmed `:309-310` | `SETTLED_KEEP` 100 below head (`:205`); burns only, `txs` emptied `:291` | ~100 KB + backlog |
| `PendingEpoch` | `local.rs:25,64` | 256 | small |
| Relay `sent` (relay transport only; testnet runs p2p) | `relay.rs:76,171` | 256 | small |
| sova/1 `seen`, `announced` | `p2p/service.rs:60-62,179-180` | 4096 each (schnellru) | ~0.4 MB |
| sova/1 `recent_blocks` | `:65,181` | 128 blocks | ~0.2 MB typical |
| sova/1 `orphans`, `held` | `:67,79,182-183` | 64 blocks each, `held` expires after `MAX_HOLD` 300 s | typical tiny; **worst case ~1 GiB** (64 × 8 MiB `MAX_BLOCK_BYTES` × block+RLP, from a peer serving junk blocks above our scan; they are parked without validation at `:459`) |
| sova/1 `in_flight`, peer queues | `:69,184`; `protocol.rs:51-53` | 64; 256 out / 1024 in per peer | small |
| Tx gossip rebroadcast | `bin/sova/src/tx_gossip.rs:57` | 1024 hashes per round, nothing kept | 0 |
| `sova_getZcashBlocks` | `zcash_feed.rs:67,75` | 1,000 blocks per call (transient); subscriptions remember 1024, WS off in public | transient |
| reth engine tree in-memory blocks | reth `engine/primitives/src/config.rs:7,17` | persistence threshold 50, buffer target 5, forks until finalized (head - 100) | a few MB |
| reth trie changeset cache | `engine/tree/src/tree/mod.rs:114,1547-1567` | evicted below `min(finalized, persisted - 64)`; bounded because the arbiter moves finalized (`FINALIZED_DEPTH` 100) | small; would grow if finalized stopped moving |
| reth block buffer, invalid headers | `config.rs:42-43` | 64, 256 | small |
| reth precompile cache | `engine/tree/src/tree/precompile_cache.rs:15-18` | 1 MiB of key+output weight per cacheable precompile; the SIP-4 precompile is `new_stateful`, never cached (`evm/src/zcash.rs:18-20`) | ~1 MiB each |
| reth txpool | `transaction-pool/src/config.rs:15-18` | 10,000 tx / 20 MB per subpool | 20 MB per subpool under spam |
| reth thread pools | `Runtime::test()` (`main.rs:231`; reth `tasks/src/runtime.rs:403-432`) | 2 threads per pool: fewer stacks and arenas than reth's defaults (a side benefit) | - |

The `held`/`orphans` worst case is a DoS bound, not the observed growth.
Capping those two LRUs by total bytes (for example 32 MiB) is a
non-consensus hardening in `p2p/service.rs`.

## Recommended fixes

| # | Fix | Kind | Consensus-sensitive? | Effect |
|---|-----|------|----------------------|--------|
| 0 | Measure `RssAnon`/`RssFile`/`RssShmem` (`/proc/<pid>/status`, `smaps_rollup`) and `du -sh <datadir>/db` twice, hours apart, on one seed; optionally turn on reth metrics on localhost for `execution_cache_created`/`in_use` | ops, read-only | no | says whether the slope is page cache (healthy) or heap |
| 1 | `SOVA_CROSS_BLOCK_CACHE_MB=32` in `/etc/sova/sova-node.env` (and later the default in `state_cache.rs`) | config | no | about -119 MiB per node at once; rebuilds cost 17.5 MiB instead of 136.5 |
| 2 | `MALLOC_ARENA_MAX=2` in `sova-node.env`; if that flattens `RssAnon`, adopt jemalloc as reth does (`reth-cli-util` `jemalloc` + `#[global_allocator]`) | config, then bin | no | caps arena growth and fragmentation |
| 3 | Lower RPC caches: `rpc_state_cache.max_blocks` ~500, `max_receipts` ~500, fee-history `max_blocks` ~256 | bin config (`NodeConfig`) | no | about -15 MB at plateau; removes ~3 MB/h of warm-up |
| 4 | `ExpectedSettlements`: drop records below `reth_head - 1024` on insert; store `pools: None` in records | engine code | **yes** (C5 path) | flat instead of ~1.2 KB/height; -39 MB now |
| 5 | Follower window check via `getblockhash` only; `Arc<HeightRecord>` from `record()` | consensus-crate/engine code | **yes** (consensus input), low risk | less churn feeding item 1 |
| 6 | `ZcashIndex`: persistent store (no answer changes) or a SIP'd lookback window; meanwhile the trims above | engine/evm code (+SIP for a window) | **yes** (precompile answers) | flat memory, and no full rescan on start; required before mainnet |
| 7 | Byte cap on sova/1 `held`/`orphans` | engine p2p code | no | removes a ~1 GiB DoS bound |

Items 1-3 are safe config for the orchestrator to deploy. Items 4-6 touch
consensus inputs and need orchestrator authorship. Item 4 is small and
mechanical. Item 6 is a design task.

## Method

- **Per-entry heap cost.** A temporary integration test (not committed)
  inserted 200,000/(1 + extra txs) synthetic testnet-shaped epochs into a
  `ZcashIndex` and into `ExpectedSettlements`, built the way
  `run_expectations` builds them, and divided the RSS delta. Blocks had a
  coinbase with two P2PKH outputs, `extra` plain txs with two outputs each,
  optionally one SIP-1 burn tx, and `pools: Some`. Release build, macOS
  allocator; glibc is similar in magnitude.

  | txs/block | burn | index B/block | expectations B/block |
  |---|---|---|---|
  | 1 | no | 772 | 439 |
  | 2 | yes | 1,180 | 1,201 |
  | 4 | yes | 2,135 | 1,201 |
  | 22 | yes | 9,678 | 1,206 |

- **`alloc_zeroed` at alignment 128.** A 20-line program allocating
  256 MiB with `std::alloc::alloc_zeroed`: RSS +0 MB at align 16, +256 MB
  at align 128 and 4096. This is Rust std's `System` path (unix), the same
  on Linux: `posix_memalign` followed by a memset.
- Everything else is from reading the code at the file:line references
  above.

## RSS log

Latest samples from the 30-minute log (`memlog.tsv`: time, host, RSS MB,
uptime s) at the time of writing:

```
2026-09-27T21:15:05Z  sova-keeper-1  415  5430
2026-09-27T21:15:11Z  sova-seed-1    445  5187
2026-09-27T21:15:18Z  sova-seed-2    426  5315
2026-09-27T21:15:23Z  sova-rpc-1     443  5046
2026-09-27T21:45:30Z  sova-keeper-1  416  7254
2026-09-27T21:45:36Z  sova-seed-1    317  7012
2026-09-27T21:45:42Z  sova-seed-2    427  7140
2026-09-27T21:45:48Z  sova-rpc-1     445  6871
```

From about 85 to 115 minutes of uptime, keeper, seed-2 and rpc-1 grew by
1-2 MB (2-4 MB/h). The 30-60 MB/h of the first ~75 minutes was warm-up, and
it has leveled off. seed-1 **dropped 128 MB** with no restart (uptime kept
counting). That is the size of the 256 MiB setting's storage table
(128 MiB), so the likeliest cause is a second execution-cache instance
being freed (see [the execution cache](#the-execution-cache-a-step-not-a-slope)).
It could also be the kernel reclaiming file-backed pages. Either way, it
confirms that a large share of this RSS is the cache and page cache, not a
leak. Keep the log running for a day to confirm the remaining ~0.7 MB/h
from the unbounded Sova stores and the reth RPC-cache warm-up.
