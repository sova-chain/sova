# SIP-4 §4 gas benchmark

Status: measured 2026-09-23 on branch `z1/gas-bench` (from `z1/sip4-v1`,
`cb26149`). Bench: `crates/engine/tests/sip4_gas_bench.rs`. Nothing here
changes a gas constant. The orchestrator decides prices.

## TL;DR

- **Every method passes the §4 gate at the draft prices, with 5–50× to
  spare, on ordinary Zcash data.** On the reference node (estimated), the
  worst gas-limit block of a normal lookup takes about **0.22 s**
  (`anchor()`, 87,577 calls). Every other method stays under 0.1 s. What
  binds is the EVM's own per-call overhead (a warm `STATICCALL` plus loop
  opcodes), not the index lookup. With the same loop, the stock identity
  precompile (21 gas) is *slower* per block than `anchor()`.
- **The current code fails the gate by 15–150× on one adversarial
  input: a Zcash tx with very large transparent outputs.**
  `ZcashSource::tx()` clones the whole `IndexedTx` (every output
  script) on every call, and `burnInfo` re-runs the SIP-1 parser over
  every output. The calls are flat-priced. A tx with ~58,000 P2PKH
  outputs (what fits in a 2 MB Zcash block) makes one `txInfo` cost
  4–10 ms, and a 30 M-gas block of them takes **48–76 s** on this laptop.
  A tx with 190 × 10 KB scripts: **4.7–8.8 s**. Anyone who can get one
  such tx mined anywhere in `Z[B ..= E]` can build these blocks.
- **Fix it in code, not in gas (no consensus change).** Return
  `Arc<IndexedTx>` from the source, parse the burn once at index time,
  and build/free index records outside the write lock. With that patch
  every pathological block drops to **5–7 ms**. The patch is commit 2 on
  this branch, kept separate so it can be taken or dropped.
- **Recommended prices: keep the draft table** (anchor 200, blockAt
  2,600, txInfo/burnInfo 4,000, txOutput 4,000 + 8/byte), *conditional
  on the fix*. If the fix is not taken, the flat prices must become
  per-output (formula below). That is a consensus change and a worse
  answer.
- **Lock:** uncontended read locks cost 14–37 ns (two per call, <5%).
  Concurrent readers are harmless. A writer looping on insert/unwind
  (not a realistic follower) starves the current code: 0.06–1.5 s per
  20-tx-block loop, 0.4–18.7 s per 58k-output-block loop. With the patch
  the same runs take 47 ms and 6 ms.
- **Memory:** 618 MB for 100,000 blocks × 20 txs (2 M txs, 3.4 M outputs),
  so **~310 B/tx, ~6.2 KB/block**. With the patch it is ~335 B/tx.
- **Side finding (not gas; needs its own look):** the precompile checks
  coverage but not that the indexed chain is still the one the block
  committed to. A Zcash reorg that the follower applies *while* a block
  executes can change answers mid-block on one node only (§ "Side
  finding").

## Setup

| | |
|---|---|
| Machine | MacBook, Apple **M3** (4P + 4E cores), 16 GB, macOS 26. **Heavily loaded:** load average 10–97 during the runs (other agents' cargo builds, a VM, and zebrad pid 80983 at ~100% CPU). Swap was ~14/16 GB used. |
| Build | `--release` (workspace default release profile), `CARGO_TARGET_DIR=/Volumes/Extreme Pro/sova/gasbench-target` |
| Block gas limit | **30,000,000**. The dev and `sova-testnet` genesis `gasLimit` is `0x1c9c380` (`bin/sova/src/chain.rs` inherits reth `DEV`). Sova's payload builder uses `EthereumBuilderConfig::new()` (`crates/engine/src/builder.rs:64`), whose `desired_gas_limit` is `ETHEREUM_BLOCK_GAS_LIMIT_30M`, so the limit stays at 30 M. Times scale linearly if it is raised (36 M: ×1.2; 60 M: ×2). |
| Spec | `SpecId::OSAKA` (dev hardforks activate Osaka at genesis), so EIP-7825 caps a tx at 2^24 gas. A "block" is therefore two txs (16,777,216 + 13,222,784 gas). |
| EVM | `SovaEvmFactory::with_zcash_source(index).create_evm(...)` once per block (as reth does), then `transact_commit` per tx. |
| Attacker contract | Hand-assembled loop: `STATICCALL(gas, 0x5A00, 0, q, 0, 0)` until `gas < price·65/64 + 3,200`. It uses `retSize = 0` and no memory growth, the cheapest shape per call. **hot** repeats one key. **distinct** walks a calldata list of keys spread over the whole index; the caller pays for that calldata (512 gas per key, counted in gas/call). |
| Index | Real `engine::zcash_index::ZcashIndex`, 100,000 Zcash blocks × 20 txs = 2,000,000 txs, 3,432,679 outputs, 94 MB of scripts. Per block: coinbase with 2 outputs, every 4th tx fully shielded (0 outputs), the rest 2 × 25-byte P2PKH. Txids are pseudo-random. Special txs are spread through the range: 64 with one 10,000-byte script, 256 SIP-1 burns, 4 "fat-many" (58,000 × P2PKH outputs, ~2 MB on the wire), and 4 "fat-big" (190 × 10,000-byte scripts, ~1.9 MB). The hit pool is 8,192 random normal txs across all 100 k blocks. Misses are random txids. Build time 1.5–4.5 s. |
| Correctness | Before timing, each scenario makes one direct call and asserts the status word (OK / NOT_FOUND / NOT_YET / NOT_A_BURN, or `E` for `anchor`), so every loop measures what its name says. |

Reproduce:

```sh
CARGO_TARGET_DIR=... cargo test --release -p engine --test sip4_gas_bench \
    -- --ignored --nocapture --test-threads=1
# knobs: SOVA_BENCH_BLOCKS SOVA_BENCH_TXS SOVA_BENCH_GAS_LIMIT SOVA_BENCH_REPS SOVA_BENCH_ONLY
```

Raw logs (not committed): `/Volumes/Extreme Pro/sova/gasbench-full{1,4-base,2-arc,3-arc-burn}.log`.

### Extrapolation to the reference node

The reference is a Hetzner AX42-class box: AMD Ryzen 7 PRO 8700GE, Zen 4,
8 cores [fact per `infra-m1.md` shape; CPU model per Hetzner's listing,
re-check at order time]. Its single-thread speed is roughly 0.8× an M3
P-core [est, public Geekbench 6 single-core: M3 ≈ 3,000–3,100, 8700G/GE
≈ 2,400–2,700]. Everything here is single-threaded execution. I use
**reference time = 2 × the worst median seen on this laptop** across two
independent full runs. That covers the ~1.25× CPU gap, plus the reth
block executor's extra per-tx bookkeeping (receipts, state-root inputs:
2 txs per block here, so small), plus slack for noise. The medians are
probably pessimistic already: the laptop was 2–10× oversubscribed and
threads may have landed on E-cores. A shared-vCPU cloud box (CX43, testnet)
could be another ~1.5–2× slower. Even at 4× laptop time, every
normal-data block below stays under 0.5 s.

## Results: current code (`cb26149`)

Two full runs on the unmodified code (runs 1 and 4). "Worst median" is
the larger of the two medians (5 timed reps each, after a warm-up).
Scenarios whose warm-up exceeded 2 s got 1 timed rep. The "with patch"
column is the median of run 3 (Arc + parsed-once burn + lock scope).

| scenario | draft gas (gas/call incl. overhead) | calls / 30 M block | laptop block ms: min / worst median | µs/call | **ref. est. ms** | < 1 s gate | < 0.5 s (2× headroom) | with patch, median ms |
|---|---|---:|---:|---:|---:|:-:|:-:|---:|
| *baseline: identity precompile 0x04* | 21 (163) | 183,746 | 137.9 / 222.8 | 1.21 | 446 | pass | yes | 92.2 |
| **anchor** | 200 (342) | 87,577 | 69.4 / 109.5 | 1.25 | **219** | **pass** | yes | 114.6 |
| blockAt hit, hot | 2,600 (2,746) | 10,922 | 9.5 / 28.5 | 2.61 | 57 | pass | yes | 8.2 |
| blockAt hit, distinct | 2,600 (2,957) | 10,144 | 13.8 / 20.4 | 2.01 | 41 | pass | yes | 11.7 |
| blockAt NOT_YET | 2,600 (2,746) | 10,922 | 7.1 / 9.3 | 0.85 | 19 | pass | yes | 5.8 |
| txInfo hit, hot | 4,000 (4,148) | 7,230 | 6.7 / 7.3 | 1.01 | 15 | pass | yes | 5.3 |
| txInfo hit, distinct | 4,000 (4,775) | 6,280 | 12.4 / 15.6 | 2.48 | 31 | pass | yes | 6.3 |
| txInfo miss, hot | 4,000 (4,148) | 7,230 | 5.7 / 9.2 | 1.27 | 18 | pass | yes | 4.2 |
| txInfo miss, distinct | 4,000 (4,775) | 6,280 | 9.0 / 13.7 | 2.18 | 27 | pass | yes | 4.6 |
| txOutput hit 25 B P2PKH, hot | 4,200 (4,348) | 6,897 | 6.3 / 7.5 | 1.09 | 15 | pass | yes | 4.3 |
| txOutput hit 25 B P2PKH, distinct | 4,200 (4,975) | 6,028 | 8.1 / 49.3 | 8.18 | 99 | pass | yes | 6.4 |
| txOutput hit 10,000 B, hot | 84,000 (84,265) | 355 | 0.6 / 0.8 | 2.25 | 2 | pass | yes | 0.4 |
| txOutput hit 10,000 B, distinct | 84,000 (84,499) | 354 | 0.7 / 1.1 | 3.11 | 2 | pass | yes | 0.4 |
| txOutput miss, hot | 4,000 (4,148) | 7,230 | 6.1 / 9.4 | 1.30 | 19 | pass | yes | 4.9 |
| txOutput miss, distinct | 4,000 (4,775) | 6,280 | 7.5 / 18.2 | 2.90 | 36 | pass | yes | 6.1 |
| burnInfo burn, hot | 4,000 (4,148) | 7,230 | 7.7 / 17.0 | 2.35 | 34 | pass | yes | 4.4 |
| burnInfo burn, distinct | 4,000 (4,231) | 7,088 | 10.6 / 23.3 | 3.29 | 47 | pass | yes | 5.2 |
| burnInfo not-a-burn, hot | 4,000 (4,148) | 7,230 | 7.1 / 14.1 | 1.95 | 28 | pass | yes | 4.3 |
| burnInfo not-a-burn, distinct | 4,000 (4,775) | 6,280 | 15.3 / 18.7 | 2.98 | 37 | pass | yes | 7.4 |
| burnInfo miss, hot | 4,000 (4,148) | 7,230 | 5.1 / 9.5 | 1.31 | 19 | pass | yes | 4.3 |
| burnInfo miss, distinct | 4,000 (4,775) | 6,280 | 6.4 / 9.1 | 1.45 | 18 | pass | yes | 5.0 |
| **PATH** txInfo on 10,000 B-script tx | 4,000 (4,148) | 7,230 | 10.2 / 13.7 | 1.89 | 27 | pass | yes | 4.4 |
| **PATH** txInfo on 58k-output tx | 4,000 (4,148) | 7,230 | 48,266 / 76,237 | 10,544 | **152,473** | **FAIL** | no | 5.3 |
| **PATH** txOutput(vout 0) on 58k-output tx | 4,200 (4,348) | 6,897 | 25,659 / 28,979 | 4,202 | **57,959** | **FAIL** | no | 4.8 |
| **PATH** burnInfo on 58k-output tx | 4,000 (4,148) | 7,230 | 44,247 / 45,713 | 6,323 | **91,426** | **FAIL** | no | 7.1 |
| **PATH** txInfo on 190 × 10,000 B tx | 4,000 (4,148) | 7,230 | 4,650 / 7,307 | 1,011 | **14,614** | **FAIL** | no | 5.7 |
| **PATH** burnInfo on 190 × 10,000 B tx | 4,000 (4,148) | 7,230 | 5,214 / 8,755 | 1,211 | **17,509** | **FAIL** | no | 5.1 |

Noise: individual reps spiked (anchor max 758 ms in run 4, one rep at load
average ~30). The minimums (anchor 65–108 ms across all four runs)
show the uncontended cost. I priced from the worst median, not the minimum.

### Direct `ZcashSource::tx()` cost (no EVM)

| lookup | current (clones), runs 1 / 4 | with patch (Arc) |
|---|---:|---:|
| normal tx (2 × 25 B), hot | 250 / 207 ns | 37 ns |
| normal tx, distinct keys | 1,959 / 1,729 ns | 81 ns |
| miss, distinct keys | 322 / 90 ns | 42 ns |
| 10,000-byte-script tx | 945 / 590 ns | 45 ns |
| 58k-output tx | **12.1 / 4.1 ms** | 111 ns |
| 190 × 10,000 B tx | **2.09 / 0.70 ms** | 42 ns |
| `indexed_through()` (read lock only) | 37 / 18 ns | 14 ns |

The clone costs about 70–200 ns per output (one allocation plus a copy
per script) and 0.35–1.1 ns per byte.

## Verdict per method at the draft prices

| method | draft | normal data | adversarial large-output tx, current code | with patch |
|---|---|---|---|---|
| `anchor()` | 200 | **pass** (ref ≈ 0.22 s) | n/a (no tx lookup) | pass |
| `blockAt` | 2,600 | **pass** (≤ 0.06 s) | n/a | pass |
| `txInfo` | 4,000 | **pass** (≤ 0.03 s) | **FAIL** (up to ~150 s) | pass (≤ 0.02 s) |
| `txOutput` | 4,000 + 8/B | **pass** (≤ 0.1 s) | **FAIL** (~58 s) | pass |
| `burnInfo` | 4,000 | **pass** (≤ 0.05 s) | **FAIL** (~91 s) | pass |

### Pricing formula and recommended prices

To get a gas-limit block of the cheapest call under 1 s with 2× headroom
(≤ 0.5 s on the reference node):

```
calls/block = L / (p + g_ovh)
T_block     = calls/block × t_ref          ≤ 0.5 s
⇒  p ≥ 2 · L · t_ref − g_ovh               (L = 30,000,000)
```

Here `t_ref` is the reference-node time per call (2 × worst laptop median
per call) and `g_ovh` is the caller's own gas per call (measured gas/call
minus the price: ~142 hot, ~230–775 with calldata keys). Plugging in the
measurements:

| method | t_ref (worst) | minimum price by formula | draft | margin |
|---|---:|---:|---:|---:|
| anchor | 2.5 µs | 150 − 142 ≈ **8** | 200 | 25× |
| blockAt | 5.2 µs | 313 − 146 ≈ **170** | 2,600 | 15× |
| txInfo | 5.0 µs | 298 − 775 → **≤ 0** (hot: 121 − 148 → ≤ 0) | 4,000 | ≫ |
| txOutput (25 B) | 16.4 µs (noisy distinct run) | 982 − 775 ≈ **210** | 4,200 | 20× |
| txOutput per byte | ≈ 0.13 ns/B laptop | ≈ **0.02 gas/B** | 8 | 400× |
| burnInfo | 6.6 µs | 395 − 231 ≈ **165** | 4,000 | 24× |

**Recommendation: keep the draft prices unchanged (with the patch).**
Don't lower them just because the in-memory index is cheap:

- The mainnet index is meant to be persistent (`zcash_index.rs` module
  docs, `sip4-evm-seam.md`). A cold NVMe read is ~10–100 µs, and by the
  formula 100 µs needs ~6,000 gas. The draft's "two cold SLOADs"
  rationale (4,000) fits a disk-backed store. **Re-run this bench when
  the store becomes persistent.**
- The EVM overhead floor is already close to binding. The identity
  precompile at 21 gas gives an estimated 0.45 s reference block with
  this loop, so cheaper prices buy contracts almost nothing.
- `anchor()` at 200 is the cheapest call and still gives a 0.22 s
  reference block. Lower is not needed; higher is not needed either.

**If the patch is not taken**, the flat prices do not meet the gate. The
price would have to follow the looked-up tx's size: for `txInfo`,
`txOutput` and `burnInfo`, `4,000 + 25 × nOutputs + ⌈scriptBytes / 8⌉`,
charged before the answer. The measured worst is ~180 ns per output and
~0.6 ns per byte (laptop), ×2 for the reference, ×2 for headroom, ×30 M.
The 58k-output case then costs ~1.6 M gas per call (≈ 0.38 s per
reference block), and the 190 × 10 KB case ~246 k gas (≈ 0.29 s). This
is a consensus rule (gas that depends on index data, like `txOutput`'s
per-byte charge today). It is harder to explain to contract authors, and
it charges honest callers for work the node never needs to do. I don't
recommend it.

## Pathological cases

### 1. `tx()` clones the record (**matters: fix**)

`ZcashSource::tx` returns `Option<IndexedTx>`, so `ZcashIndex::tx` does
`txs.get(txid).cloned()`: one allocation per output plus a copy of every
script, on every call. That is harmless for the 10 KB single-script tx
(~0.6–0.9 µs, well inside `txOutput`'s 84,000 gas). It is fatal when
the call is flat-priced and the tx is large: `txInfo` only needs
`outputs.len()`, but it pays for copying all 58,000 outputs. Transparent
output count is bounded only by Zcash's 2 MB block size, and SIP-4 §3
itself notes that `nOut` can exceed 65,535.

Fix (commit 2): `fn tx(&self, txid) -> Option<Arc<IndexedTx>>`, and the
index stores `HashMap<[u8; 32], Arc<IndexedTx>>`. A lookup is then a
refcount bump (37–111 ns whatever the tx size). The answers are
byte-identical: same record, same horizon filter.

### 2. `burnInfo` re-parses every output (**matters: fix**)

Even with `Arc`, `burnInfo` ran `sip1::extract_burn` over all 58,000
outputs per call: 535 µs/call and a **3.9 s** block (run 2). Fix (commit
2): `IndexedTx::new(height, index, version, outputs)` runs the same
consensus parser once, at index time, over exactly those outputs. The
precompile reads `t.burn()`. Same parser, same input, same answer. A
new unit test asserts `burn()` equals `extract_burn(outputs)` on the
fixtures. Construction goes through `new` because the cache field is
private. The `outputs` field stays `pub` for readers. Nothing mutates a
record once it is shared behind `Arc`.

After both fixes, every PATH scenario takes 4.8–7.1 ms per block, the
same as a normal lookup.

### 3. RwLock

- **Uncontended:** `indexed_through()` (read lock only) costs 14–37 ns.
  The precompile takes two read locks per call (`indexed_through` plus
  `tx`/`block`), under 5% of a ~1 µs call.
- **Concurrent readers** (3 threads hammering `indexed_through` + `tx`
  at 1–1.8 M ops/s, far above any RPC load): 9.4–17 ms vs 9.8–15 ms
  alone on the current code, and 18.9 ms with the patch (cache-line
  traffic on the lock word plus CPU competition on a loaded laptop).
  That is still 25× under the gate. No action needed.
- **A looping writer** (insert + unwind of block `E+1` back to back, 85–
  115 k/s; the real follower writes once per ~75 s Zcash block, and in
  catch-up it is rate-limited by zebrad RPC):
  - current code, 20-tx block: 57 ms min, **1.5 s** median. macOS's std
    `RwLock` lets the writer re-acquire ahead of queued readers.
  - current code, 58k-output block: **0.43 s median, 14.4–18.7 s worst**.
    The deep copy and the frees ran *inside* the write lock.
  - with the patch (records built before `write()`, replaced and removed
    `Arc`s dropped after unlock): **47 ms** and **6 ms**.

  This isn't a realistic threat today: nothing lets a remote party make
  the follower write in a loop. But the patch removes the worst case
  cheaply. If the index ever gets a high-rate writer, move to snapshot
  reads (e.g. an `Arc` snapshot taken per block; see the side finding).

## Memory

Phys footprint delta (macOS `footprint`, which counts compressed and
swapped pages; RSS undercounted it at 414 MB):

| | total | per tx | per Zcash block (20 txs) |
|---|---:|---:|---:|
| current code | **618 MB** | **309 B** | **6.2 KB** |
| with patch | 670 MB | 335 B | 6.7 KB |

Breakdown (current code, estimated): the txid `HashMap` is 4,194,304
buckets × 73 B ≈ **306 MB**. hashbrown rounds to a power of two, and
2 M txs sit just past the 1.84 M resize point, so the table is ~48% full;
per-tx cost swings roughly 150–300 B with load. Outputs take ≈ 205 MB
(a 64 B `Vec` plus 2 × 32 B script allocations for 1.6 M txs). Blocks
take ≈ 74 MB (BTreeMap entry plus 640 B of txids each). The 8 fat txs
add ≈ 20 MB. The patch adds an `Arc` header and a cached
`Option<Burn>` per tx (+26 B/tx).

Projection at 20 txs/block: one year of Zcash (≈ 420 k blocks, 8.4 M
txs) is ≈ 2.6 GB in memory. Adversarially, a 2 MB Zcash block of minimal
transparent txs (~10 k txs) adds ~3 MB of index per block, for about
1 ZEC in ZIP-317 minimum fees. Mainnet needs the persistent store that
the module docs already defer to.

## Side finding: mid-block Zcash reorg (not gas; please triage)

§1 checks, *before execution*, that the follower's hash at `E_N` equals
the block's committed anchor. The precompile then checks only coverage
(`indexed_through() ≥ E`) on each call. The follower task
(`expectations.rs:279–291`) applies `Rollback`/`unwind_above` and
`insert` concurrently with block execution. So if Zcash reorgs below `E`
while a block is executing, this can happen:

1. Execution starts with the index matching the committed anchor.
2. The follower unwinds and re-inserts the new branch through `E`.
3. Later calls in the same block pass the coverage check and answer
   from the new branch.

That node computes a different state root from its peers, and a bad state
root is a permanent invalid, not a transient hold. Between the unwind
and the re-insert, the coverage check does turn the call Fatal (safe).
The danger is the window after the re-insert. Two cheap mitigations,
both outside this task:

- Per call, check that `block(E).hash` equals the block's committed
  anchor, and go Fatal if not (the EVM env would need the anchor, e.g.
  from the header's `parent_beacon_block_root`).
- Or take a per-block snapshot (`Arc` of an immutable view) when the EVM
  is created.

## Proposed diff to SIP-4 §4

```diff
 ### 4. Gas (draft; the numbers are gated on benchmarks)
 
 | Call | Gas |
 |---|---|
 | `anchor()` | 200 |
 | `blockAt` | 2,600 |
 | `txInfo`, `burnInfo`, `spentBy` | 4,000 |
 | `txOutput` | 4,000 + 8 × `len(script)` |
 
 Rationale: an answer is a keyed read from a local store outside the
 EVM state trie, so it is priced at about two cold `SLOAD`s (2,100
 each). **Negative answers cost the same as positive ones**, so a miss
 buys no griefing discount. The benchmark gate: a block filled to the gas
 limit with the cheapest lookup must execute in under 1 s on the
 reference node (Hetzner AX-class, `infra-m1.md`). Otherwise prices rise.
+
+**Benchmark (2026-09-23, `docs/design/sip4-gas-bench.md`).** At the
+30 M block gas limit, with an in-memory index of 2 M txs, every method
+passes with 5–50× to spare. The worst case is `anchor()`, ≈ 0.22 s per
+block on the reference node (est.). The binding cost is EVM call
+overhead, not the lookup. The prices above stand. Two conditions:
+
+- **A lookup's cost must not depend on the tx's size.** A flat-priced
+  call on a tx with ~58,000 transparent outputs (one 2 MB Zcash block)
+  must not copy or re-parse those outputs. The node shares index records
+  (no per-call clone) and parses the SIP-1 burn once at index time.
+  Without this, a gas-limit block of such calls takes 15–150 s, and the
+  prices would have to grow per output. The bench includes these
+  adversarial txs, and any change to the store must keep passing them.
+- **Re-bench when the store becomes persistent** (mainnet). A cold disk
+  read is 10–100 µs. At 100 µs the gate needs ≈ 6,000 gas per lookup,
+  which is still within the two-cold-`SLOAD` rationale.
+
+Formula for any future re-pricing: `p ≥ 2 · L · t_ref − g_ovh`, where
+`t_ref` is the reference node's time per call and `g_ovh` the caller's
+own gas per call. That keeps a gas-limit block of the call at or under
+0.5 s (2× headroom on the 1 s gate).
 Precedent: the Bitcoin-era Sova priced its pure decode precompile at
 3,000 + 3/byte (`sova-reth/evm/src/precompiles/precompile_utils.rs`).
```

§11's "Gas DoS bench" item should also name the adversarial large-output
txs (many outputs, and big scripts) next to hits and misses.
