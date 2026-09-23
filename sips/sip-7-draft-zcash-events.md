# SIP-7: Zcash Pool State and Events

- Status: **Draft, design only** (2026-09-23). No code. Needs Rob's calls
  in "Decisions for Rob" (§10) before a build is dispatched.
- Numbering: **SIP-7.** SIP-5 is reserved for the wZEC peg, and SIP-6 is
  sealer signatures.
- Author: Sova (orchestrated draft)
- Depends on: SIP-4 v1 (the anchor `E_N = N + B − 1`, the node-side
  `ZcashIndex`, the `0x…5A00` precompile, the §7 reorg rollback). The v1
  code is on `z1/sip4-v1`. This SIP extends it and changes none of its
  rules.
- Consensus change: **yes.** New precompile methods (§2). A genesis
  predeploy plus a pre-block system call (§4.1), if Decision 2 is taken.
  Both activate at the testnet reset alongside SIP-4 and SIP-6.
- Origin: Rob, 2026-09-23: *"we probably need a pre-compile or some way
  that the Sova chain can maybe see the amount in the shielded pool to be
  able to react to state events happening on Zcash. We should be able to
  try to react to anything happening on Zcash basically."*

## Summary

Zcash publishes the exact total of every value pool (transparent,
Sprout, Sapling, Orchard, Ironwood, and the dev-fund lockbox) at every
block. It also publishes each transaction's net flow into or out of each
shielded pool, and how many spends, outputs and actions each pool
processed. All of it is public consensus data, the same on every Zcash
node.

SIP-7 lets Sova contracts read that data as of the anchored Zcash
height, deterministically, at a flat gas price. **Contracts can see the
shielded pool's total and every change to it:** per block, per window,
and per transaction. It also gives dapps and bots a way to react. Every
Sova block records a summary of its anchored Zcash block in a canonical
contract, nodes stream those summaries over RPC, and a keeper pattern
turns any Zcash condition into an on-chain action.

Everything here is an **aggregate or a public count**. Who paid whom
inside a shielded pool, and how much, stays private, as Zcash designed
it (§4.4).

## Motivation

SIP-4 v1 lets a contract ask "did this transparent payment happen?" It
cannot ask "what is happening on Zcash?" The data that answers that
question is the pool state: how much ZEC is shielded, whether the pool
is growing or shrinking, how busy it is, and whether a given transaction
moved value out of a shielded pool. SIP-4 §8 lists this as "public, so
possible later." This SIP is that "later."

It is also cheap. The follower already downloads every field this SIP
needs (`getblock <hash> 1` and one `getrawtransaction <txid> 1` per tx,
`crates/consensus/src/zebrad.rs`). Today it discards them. **No new RPC
calls are needed.**

## Specification

### 1. What Zcash exposes (verified)

Verified on 2026-09-23 against the local Zebra 6.3.0 testnet node
(`127.0.0.1:18234`, tip 4,384,201, NU6.3 active) and the Zebra source at
`research/zebra-upstream` `f5c5277`. Appendix A has the raw shapes.

#### 1.1 Per block (`getblock <hash> 1`, already fetched)

| Field | Meaning | Kind |
|---|---|---|
| `valuePools[i].chainValueZat` | Total value in pool `i` after this block | **Monotonic total** (a level, not a counter; can go up or down) |
| `valuePools[i].valueDeltaZat` | Change to pool `i` made by this block | **Per-block delta**, signed |
| `valuePools[i].id` | `transparent`, `sprout`, `sapling`, `orchard`, `lockbox`, `ironwood`, always in this order | Fixed order (`GetBlockchainInfoBalance::value_pools`) |
| `chainSupply.chainValueZat` | Sum of all six pools | Total |
| `trees.{sapling,orchard,ironwood}.size` | Note-commitment tree size after this block, i.e. **every note ever created** in that pool | **Monotonic counter** (never decreases) |
| `nTx`, `tx[]`, `time`, `hash` | As SIP-4 | |

Rules that matter for parsing:

- `valuePools` and `chainSupply` are **optional**. Zebra reads them from
  its per-block `BlockInfo` (`zebra-rpc/src/methods.rs:1528-1569`), which
  can be absent, for example while a database upgrade is still
  backfilling. **Absent means hold, never zero.**
- `valueDeltaZat` is computed by Zebra as `chainValue(h) − chainValue(h−1)`
  (same lines). Checked live over 61 consecutive blocks: identity holds
  for every pool.
- `trees.<pool>` is omitted **exactly when the size is 0**
  (`skip_serializing_if = "…Trees::is_empty"`, `methods.rs:4638-4645`).
  Absent means 0. That is exact, not a guess. For example, height
  4,134,000 (NU6.3 activation) has no `ironwood` tree yet.
- `monitored` is just `chainValue != 0` (`get_blockchain_info.rs:56`).
  Ignore it.
- There is no `finalironwoodroot` in `getblock` 6.3 (only Sapling and
  Orchard roots). Tree sizes are enough for this SIP.

#### 1.2 Per transaction (`getrawtransaction <txid> 1`, already fetched)

| Field | Meaning |
|---|---|
| `valueBalanceZat` (top level) | **Sapling** value balance. Positive means value leaves Sapling. Always present on v4+ |
| `orchard.valueBalanceZat`, `orchard.actions[]` | Orchard balance and actions. `orchard` is **always present** (empty actions, balance 0 when unused) |
| `ironwood.valueBalanceZat`, `ironwood.actions[]` | Ironwood (NU6.3) balance and actions. `ironwood` is **omitted when the tx has no Ironwood bundle** (`transaction.rs:1039`, `tx.ironwood_shielded_data().map(..)`). Absent means 0 |
| `vShieldedSpend[]`, `vShieldedOutput[]` | Sapling spends (one nullifier each) and outputs (one note commitment each) |
| `vjoinsplit[].vpub_oldZat / vpub_newZat` | Sprout: value in and value out |
| `vin[]`, `vout[]` | Transparent inputs and outputs. **`vin[].valueSat` is not filled in by Zebra** (`transaction.rs:899-901`), so a tx's transparent input value and fee are not available from this call |

Every Orchard or Ironwood action reveals one nullifier and one note
commitment, so the action count is the count of both.

**Sign convention in Zcash:** `valueBalance > 0` means value leaves the
shielded pool, toward transparent outputs and the fee. A pool's delta is
therefore `−valueBalance`. For Sprout it is `Σ(vpub_old − vpub_new)`.

**Checked live (61 blocks, 4,384,140 to 4,384,200):** for Sapling,
Orchard and Ironwood, `Σ over txs of −valueBalanceZat` equals the
block's `valueDeltaZat`, with zero mismatches. That gives the follower an
independent cross-check on Zebra's pool accounting (§3).

#### 1.3 Pool facts observed on testnet (for orientation)

- The block subsidy lands in three pools at once. A coinbase-only block
  (4,384,200) moves transparent +0.125, lockbox +0.1875 and Ironwood
  +1.25 TAZ. Since NU6.3, shielded coinbase goes to **Ironwood**, and
  coinbase transactions must have an empty Orchard component (ZIP-229,
  `zebra-consensus/src/transaction/check.rs`). Some miners still pay
  coinbase to Sapling (4,384,160: Sapling +1.25035).
- The sum of all six deltas was exactly 156,250,000 zat (1.5625 TAZ) in
  every block checked, which is the subsidy. Fees move between pools but
  are not removed from supply.
- **From NU6.3, `orchard.valueBalance` must be ≥ 0**
  (`orchard_value_balance_non_negative`). So Orchard can only hold steady
  or shrink, and value leaves it for Ironwood or transparent. On testnet,
  Orchard went from 252,923.67 TAZ just before NU6.3 (4,133,999) to
  239,133.12 TAZ at the tip. Ironwood went from 0 to 137,526.84 TAZ. The
  migration is itself a public, trackable metric.
- Current testnet totals (tip): transparent 15.74 M, Sprout 0.43 M,
  Sapling 1.53 M, Orchard 0.24 M, Ironwood 0.14 M, lockbox 0.16 M TAZ.

#### 1.4 What is deterministic at the anchored height

Everything in §1.1 and §1.2 is a **pure function of the Zcash chain up
to that block**. Pool totals are Zcash consensus quantities (ZIP-209
chain value pool balances, which every validating node computes and
checks for non-negativity). Deltas, counts and tree sizes derive from
block contents. None of it is tip-relative. The one tip-relative field
in these responses is `confirmations`, which SIP-4 §2 already excludes.

So every value is readable at any `h` in `[B, E_N]` under SIP-4's horizon
rule, with the same answer on every honest node.

**Monotonic totals make windows cheap.** "How much did the shielded pool
grow in the last day?" is `total(E_N) − total(E_N − 1152)`: two O(1)
reads, not a loop over 1,152 deltas. (A day is 1,152 blocks at 75 s.)
Tree sizes do the same for note counts.

### 2. Precompile v1.1 queries (`0x…5A00`)

Same address, same Solidity-ABI dispatch, same read-only and
non-cacheable registration, and the same coverage-then-answer rule
(SIP-4 §3, §5). New methods:

| Method | Selector | Returns |
|---|---|---|
| `poolValue(uint64 h, uint8 pool)` | `0x0cd0bdbc` | `(uint8 status, uint64 chainValueZat, int64 deltaZat)` |
| `poolTotals(uint64 h)` | `0x1c476c7e` | `(uint8 status, uint64[] chainValueZat, int64[] deltaZat)`, indexed by pool id |
| `blockStats(uint64 h)` | `0x84df4c97` | `(uint8 status, uint32 txCount, uint32 shieldedTxCount, uint32 tIn, uint32 tOut, uint32 saplingSpends, uint32 saplingOutputs, uint32 orchardActions, uint32 ironwoodActions, uint32 joinSplits, uint64 saplingNotes, uint64 orchardNotes, uint64 ironwoodNotes)` |
| `txShielded(bytes32 txid)` | `0xdaa39583` | `(uint8 status, uint64 height, int64 sproutDelta, int64 saplingDelta, int64 orchardDelta, int64 ironwoodDelta, uint32 nIn, uint32 saplingSpends, uint32 saplingOutputs, uint32 orchardActions, uint32 ironwoodActions, uint32 joinSplits)` |

**Pool ids** follow Zebra's `valuePools` order: `0` transparent, `1`
Sprout, `2` Sapling, `3` Orchard, `4` lockbox, `5` Ironwood. `poolTotals`
returns arrays of length 6 in v1.1. A future Zcash pool is appended as id
6 at a Sova fork height, so the ABI never changes shape. (Returning a
fixed 6-tuple would need a new selector for every new Zcash pool.)

**Sign convention: pool delta, positive = value into the pool**,
everywhere: blocks and transactions alike. It equals Zebra's
`valueDeltaZat` and `−valueBalance`. One convention for all methods beats
matching Zcash's per-tx name. The library docs state the mapping.

**Field semantics:**

- `shieldedTxCount`: transactions with any Sprout, Sapling, Orchard or
  Ironwood component, coinbase included.
- `saplingNotes`, `orchardNotes`, `ironwoodNotes`: cumulative tree sizes
  after block `h`. Per-block note counts are differences of consecutive
  values.
- `txShielded.nIn`: the transparent input count, so "fully shielded" is
  `nIn == 0 && nOut == 0` (`nOut` from `txInfo`).
- Net value that left the shielded pools in a tx is
  `−(sprout + sapling + orchard + ironwood)` deltas. That includes the
  fee, and it nets to about zero for a pure Orchard→Ironwood migration.

**Statuses.** SIP-4's codes are reused: `OK = 0`, `NOT_FOUND = 1`
(`txShielded`), `NOT_YET = 2` (`h > E_N`), `OUT_OF_RANGE = 3` (`h < B`).
There is one new code, **`NO_SUCH_POOL = 6`**: `poolValue` with a pool id
not defined at this height. It is a status and not a revert, so a
contract written for a future pool behaves the same before and after that
pool's fork. Malformed calldata reverts. `int64` words must be correctly
sign-extended, the same strict decoding as v1.

**Gas.** Every answer is precomputed at index time, so each call is one
keyed read, constant-time and independent of block or tx size. The
prices use SIP-4 §4's classes:

| Call | Gas | Key |
|---|---|---|
| `poolValue`, `poolTotals`, `blockStats` | 2,600 | height (like `blockAt`) |
| `txShielded` | 4,000 | txid (like `txInfo`) |

Negative answers cost the same as positive ones. The gate is SIP-4's: a
30 M-gas block of the cheapest call must run in under 1 s on the
reference node. The v1 bench (`docs/design/sip4-gas-bench.md` on
`z1/sip4-v1`) shows that lookups of this shape run 5–50× under the gate
at these prices, with EVM call overhead dominant. §7 re-runs it for the
new methods. The records are fixed-size, so the large-output hazard from
the v1 bench does not apply to them.

### 3. Index and follower changes

No new RPC calls. The follower keeps fields it already receives.

- **`BlockView` / `EpochData`** gain `pools: [u64; 6]` (chainValueZat),
  `deltas: [i64; 6]`, `trees: [u64; 3]` (Sapling, Orchard, Ironwood
  sizes), and the `blockStats` counters, summed from the tx records at
  scan time. About 160 bytes per block.
- **`TxView` / `IndexedTx`** gain `n_in: u32` and an optional
  `ShieldedSummary { deltas: [i64; 4], sapling_spends, sapling_outputs,
  orchard_actions, ironwood_actions, joinsplits }`, stored only when the
  tx has a shielded component. That is ≤ 48 bytes on those txs, against
  the bench's ~310 B/tx today.
- **Strict parsing (`zebrad.rs`).** `valuePools` must list exactly the
  six known ids in the known order, with integer `chainValueZat` and
  `valueDeltaZat`. A missing `valuePools`, an unknown id, or a
  float-only value is a `Backend` error, so the follower retries and the
  chain holds. Omitted `ironwood` in a tx, or omitted `trees.<pool>`,
  means 0, per §1.1–1.2. Values are always the `…Zat` integers, never
  the floats.
- **Follower cross-checks before emitting an epoch** (node-local; a
  failure holds and raises an alert, it never answers):
  1. `chainValue[h] − chainValue[h−1] == delta[h]` for every pool
     (h > B).
  2. For Sprout, Sapling, Orchard and Ironwood,
     `Σ tx deltas == delta[h]`. This is independent of Zebra's
     `BlockInfo`, because it is computed from the transactions.
  3. `Σ pools == chainSupply`.

  A zebrad bug that reports wrong-but-plausible pool totals is caught by
  (2) for the shielded pools. Transparent and lockbox rely on (1) and
  (3), plus the differential sim (§7).
- **`ZcashSource`** gains `pools(h)`, `stats(h)` and `shielded(txid)`,
  returning `Arc`'d fixed-size records (the v1 bench lesson). Rollback,
  generation bump and the coverage rule are unchanged, because the new
  data lives inside the records that `insert` and `unwind_above` already
  manage.
- **Where the data comes from.** Pool totals come from zebrad's
  `valuePools`, cross-checked as above. The alternative is to recompute
  totals from a snapshot at `B − 1` plus our own deltas. That was rejected
  for v1.1: the transparent delta needs every spent output's value, which
  Zebra's `vin` omits, so it would need SIP-4's `spentBy` input index
  first. (Decision 4.)

### 4. Reacting to Zcash

Zcash cannot call a Sova contract, and neither can anything else. A
contract acts only when a transaction calls it. "React to anything on
Zcash" therefore needs three parts: **a record** (what happened, in
Sova state), **a feed** (so bots and indexers know when), and **a
trigger** (someone sends the transaction). Contracts read the data
through the precompile (§2), and the three parts sit around it.

#### 4.1 The record: `ZcashBlocks` system contract (optional, Decision 2)

A canonical contract at **`0x…5A01`**, predeployed in the genesis alloc.
It is written once per Sova block by a pre-block system call, the
EIP-4788 / EIP-2935 pattern.

- **Call.** At the start of every Sova block `N ≥ 1`, after the standard
  system calls, the executor calls `ZCASH_BLOCKS` from `SYSTEM_ADDRESS`
  (`0xff…fe`). The calldata is the packed summary of Zcash block `E_N`:
  height, hash, time, the six chain values, tx and shielded-tx counts,
  per-pool action counts, and tree sizes (about 200 bytes). As in
  EIP-4788, the gas limit is 30 M and it does not count against the
  block. A revert makes the block invalid. The code is fixed, so that
  can only happen through a bug.
- **One Zcash block per Sova block.** Sova block `N` anchors exactly
  `E_N`, and `E_{N+1} = E_N + 1`. The record never skips or batches.
- **Storage.** A ring buffer of the last **8,191** anchored blocks (about
  7.1 days at 75 s), keyed `h mod 8191`, 4 slots each: hash;
  height|time|txCount|shieldedTxCount; transparent|Sprout|Sapling|Orchard;
  lockbox|Ironwood|packed action counts. There is also a `latest` slot.
  Deltas are differences of adjacent entries. State growth is bounded
  (about 33 k slots). Older heights stay readable through the
  precompile.
- **Contract reads.** `latest()`, `summary(h)` (with an `ok` flag for
  outside the window) and `poolValue(h, pool)` are plain `view`
  functions. They work in any tooling, traces and `eth_call`, with no
  precompile ABI needed.
- **Provability.** The summary is in the Sova **state root**, so
  `eth_getProof` on `0x…5A01` proves "Zcash block `h` had these pool
  totals" to anyone holding a Sova header. That is a building block for
  light clients and other chains, which SIP-4's precompile can't give
  (its answers are computed during execution, not stored).
- **Cost.** Once the ring is warm, about 4 cold `SSTORE` updates per
  block (~20–25 k gas-equivalent, ~88 k per block during the first
  8,191 blocks), well under a millisecond. There is no user-visible gas.
- **Determinism.** The calldata comes from the **same index record** the
  precompile serves for `E_N`, under the same guards: the §1 pre-execution
  hold guarantees coverage, a missing record is `Fatal` (refuse and
  retry, never cached invalid), and the index generation is captured
  per EVM. The payload builder and the importer run the same executor
  code, so the sealer and validators compute the same write.
- **Consensus impact, honestly.** Today a block that never calls
  `0x5A00` has a state root independent of index *contents* (only the
  anchor hash matters). With the system call, **every** block's state
  root depends on its anchored record. An index-data disagreement then
  splits the chain at the next block, not latently when some contract
  first reads the bad value. That is a larger surface, but it also makes
  the disagreement surface immediately. §3's cross-checks turn most
  such bugs into a hold instead.
- **Genesis.** The testnet genesis alloc is empty today
  (`bin/sova/src/chain.rs`, `testnet_genesis_alloc_is_empty`). Adding
  the predeploy changes the genesis hash, which is free at the testnet
  reset. Adding it later needs an irregular state change at a fork
  height (setting code from the executor), which is possible but ugly.
  **That makes the reset the cheap moment.**
- **Zcash reorgs (SIP-4 §7).** Nothing extra is needed. The ring buffer
  is ordinary state. A Zcash reorg to `R` unwinds Sova to `R − B + 1`,
  the entries above roll back with it, and the re-sealed blocks write
  the new branch's summaries. Readers see exactly what the canonical
  Sova chain says.
- **Seam.** A `SovaBlockExecutor` wraps `EthBlockExecutor` and extends
  `apply_pre_execution_changes` with one `transact_system_call` plus a
  commit. reth v2.6.0's `examples/custom-beacon-withdrawals` is the
  template (it does the same thing post-execution).

#### 4.2 The feed: how bots and indexers learn "something happened"

**Finding: a log emitted inside a system call is discarded.** In
alloy-evm 0.39 (reth v2.6.0), `SystemCaller` commits the call's
`res.state` and drops `res.result`, logs included
(`block/system_calls/mod.rs:123-137`). A system call has no transaction,
so it has no receipt, and `eth_getLogs` / `eth_subscribe("logs")` never
see it. The reth example does the same. So "the system call emits an
EVM log" does not reach standard tooling by itself. There are three
delivery paths:

| Path | What subscribers use | Consensus change | Effort | Verdict |
|---|---|---|---|---|
| **A. Node RPC feed** | `sova_subscribe("zcashBlocks")` and `sova_getZcashBlocks(from, to)`. Each item is the §4.1 summary, sent once the Sova head commits that height. On a Sova reorg it sends a `rollback{toHeight}` item first (the same meaning as `removed: true` on eth logs) | None | ~3–4 days (reth `node-custom-rpc` / `exex-subscription` examples) | **v1.1** |
| **B. `publish()` lazy logs** | `ZcashBlocks.publish(fromH, toH)`, anyone-can-call, emits `ZcashBlock(uint64 indexed height, bytes32 hash, …)` for recorded heights not yet published, then marks them. The events are real, so `eth_getLogs` works; they land in whichever Sova block ran `publish` | None beyond §4.1 | ~1 day | **v1.1** (a convenience; a sealer or any bot can call it every block) |
| **C. System transaction** | A real tx at index 0 of every block (the OP-stack "L1 info" pattern), so the log sits in a receipt of the block it describes | Yes: an unsigned system tx type (custom node primitives, pool and RPC types), or a publicly known system key restricted by consensus rules | 2–3 weeks | **Later SIP**, only if demo feedback asks for explorer-native events |

Path A is also the lowest latency. A Zcash block at height `h` becomes
Sova block `h − B + 1`, sealed as soon as the follower emits `h`, so
subscribers hear about it seconds after Zcash does. Contracts that must
not act on a reorgable block apply `minConf` as in SIP-4.

#### 4.3 The trigger: keeper pattern

For a contract that must *act* when a Zcash condition becomes true
(settle, release, pay out, rebalance):

```solidity
// Sketch. IZcash is SIP-4's interface plus the §2 methods.
abstract contract ZcashTrigger {
    IZcash constant Z = IZcash(0x0000000000000000000000000000000000005A00);
    uint256 public immutable bounty;   // paid to whoever proves the condition first
    bool public fired;

    /// Anyone can call. `h` is the Zcash height the caller claims satisfies
    /// the condition. The contract re-checks everything itself.
    function poke(uint64 h) external {
        require(!fired, "done");
        (uint64 e, ) = Z.anchor();
        require(h + minConf() - 1 <= e, "too shallow");  // SIP-4 depth rule
        require(condition(h), "not met");                  // reverts cost the caller
        fired = true;
        _act(h);
        payable(msg.sender).transfer(bounty);
    }
    function condition(uint64 h) internal view virtual returns (bool);
    function _act(uint64 h) internal virtual;
    function minConf() internal view virtual returns (uint64);
}
```

- **The caller supplies the witness (a height or txid) and the contract
  verifies it.** Contracts can't search Zcash. They check a claim in
  O(1). For example, "Ironwood crossed 1 M ZEC" checks
  `poolValue(h, 5) ≥ X && poolValue(h−1, 5) < X`. "A payment to my
  t-address arrived" checks `txOutput(txid, vout)` (SIP-4). "The shielded
  pool grew 2% this week" checks two `poolValue` reads 8,064 blocks
  apart.
- **Bounties** pay the first valid caller, so any bot running the
  §4.2 feed will fire the trigger. A failed check reverts, so the
  caller pays for spam. Repeating triggers (fire every time the pool
  drops 1%) keep a "last fired height" and a cooldown instead of
  `fired`.
- **A reference keeper** (a ~150-line TypeScript bot on the §4.2 feed)
  and the `ZcashTrigger` base contract ship in `contracts/` next to
  `ZcashLib.sol`.

#### 4.4 What stays private (by Zcash's design)

Contracts see **aggregates and public counts**: pool totals, per-block
and per-tx net pool flows, action, spend and output counts, and
note-tree sizes. What Zcash keeps private stays private: the amount,
sender, recipient and memo of a shielded transfer, the balance of any
shielded address, and which shield matches which later deshield. Two
honest caveats go in the library docs:

- **Counts are an upper bound on activity.** Orchard and Ironwood
  bundles pad to at least two actions, and wallets add dummy Sapling
  outputs, so action counts measure load, not "number of payments."
- **A tx's net shielded flow includes its fee.** The fee alone is not
  available: Zebra's `vin` carries no input values.

This SIP makes no claim of private smart contracts. Sova contracts are
public. What is new is that they can **see Zcash's shielded economy as a
whole** and respond to it.

### 5. Use cases (short, honest)

1. **Zcash pulse (oracle-free health feed).** A contract and a page show
   shielded supply, its share of total supply, and 1-day and 7-day
   change, read on-chain each block from `ZcashBlocks`. No oracle and no
   API key: the numbers are in Sova's state root. It is the most
   demoable proof that "Sova sees Zcash."
2. **Markets that settle on Zcash metrics.** "Will Ironwood hold more
   than X ZEC at height H?" settles with one `poolValue(H, 5)` call, and
   anyone can trigger settlement (§4.3). There is no oracle to bribe,
   because the answer is Zcash consensus data.
3. **"Paid from shielded" check.** An app that already accepts a ZEC
   payment through SIP-4's `txOutput` can also check `txShielded` on the
   same tx. If the shielded pools lost at least the payment amount, the
   payer funded it from a shielded balance. That supports a badge, a
   discount, or a privacy-respecting receipt, without learning who paid.
4. **The Ironwood migration as a public signal.** Orchard can only shrink
   after NU6.3 (§1.3). Contracts can track the migration and pay out a
   bounty or campaign when Orchard falls below a threshold.
5. **Pricing on Zcash activity.** An app can scale a fee, a mint price or
   a rate limit on shielded actions per block (`blockStats`) or on pool
   inflow. It is simple, but it prices things on real Zcash usage.
6. **Transparent payment triggers.** SIP-4 `txOutput` plus a §4.3 keeper
   gives "when my t-address is paid, do X," with the bot supplying the
   txid.
7. **Lockbox watch.** The dev-fund lockbox pool (+0.1875 TAZ per block on
   testnet today) is public. Governance-minded contracts can react to
   disbursements.

### 6. Failure modes (additions to SIP-4 §6)

| Situation | Node behavior | Can state roots diverge? |
|---|---|---|
| zebrad omits `valuePools` (BlockInfo not backfilled) | Follower errors and retries. The block holds | No. It stalls |
| zebrad reports pools that fail a §3 cross-check | Hold plus alert ("zebrad pool accounting inconsistent") | No |
| Two zebrad versions report different but self-consistent transparent or lockbox totals | Different answers | **Yes.** Caught by the differential sim, and by the §4.1 system call on the first block rather than latently |
| Unknown pool id in `valuePools` (a new Zcash upgrade) | Hold until Sova ships the fork that defines it | No |
| Precompile cache enabled by mistake | Stale answers | **Yes.** Same guard as v1 (`new_stateful` plus a test) |

### 7. Test and simulation plan

- **Golden fixtures** captured from the Zebra 6.3 testnet: 4,384,200
  (coinbase to Ironwood), 4,384,160 (coinbase to Sapling plus an
  Ironwood deshield, v6 txs), 4,384,153, 4,134,000 (NU6.3 activation,
  no Ironwood tree), and height 1,000 (`trees: {}`). Parse exactly, and
  hold on a missing `valuePools` or an unknown id.
- **Cross-check tests:** the three §3 invariants on the fixtures and on
  a mutated fixture (each mutation must hold).
- **Precompile units:** ABI golden vectors for the four selectors,
  `int64` sign-extension, statuses at `B−1`, `B`, `E_N` and `E_N+1`,
  `NO_SUCH_POOL`, equal gas for hit and miss, and
  `supports_caching() == false`.
- **Property:** two indexes fed the same chain answer identically, and
  rollback plus rescan equals a fresh index (extends the v1 test).
- **`ZcashBlocks`:** forge tests (writes only from `SYSTEM_ADDRESS`, ring
  wraparound, `summary` outside the window, `publish` idempotence), and
  an executor test that builder and importer reach equal state roots.
  Update the genesis-hash test.
- **Box scenarios** (added to SIP-4 §11): (6) a Zcash reorg that changes
  pool totals, where both nodes roll the ring buffer back identically and
  the RPC feed emits `rollback`; (7) a differential run of two zebrad
  versions comparing `valuePools` over the same range; (8) a keeper that
  fires a `ZcashTrigger` on a regtest shield or deshield, exactly once.
- **Bench:** re-run `sip4_gas_bench` with the four methods, and time the
  system call per block (target < 0.1 ms).

### 8. Cost and risk

- **Effort (estimate, after SIP-4 v1 lands):** follower parsing,
  cross-checks and index records, about 1 week. Precompile methods and
  tests, 3–4 days. `ZcashBlocks` contract, executor hook and genesis,
  about 1 week. RPC feed, 3–4 days. `ZcashLib` additions, `ZcashTrigger`,
  the keeper bot and the pulse demo, about 1 week. Box scenarios run in
  parallel. **Total ~3–4 weeks.** Without §4.1, about 2–2.5 weeks.
- **Risk:** consensus now depends on zebrad's pool accounting, not only
  on block contents. The §3 cross-checks cover the shielded pools
  independently, and the differential sim covers the rest. §4.1 widens
  the consensus surface to every block (§4.1, "Consensus impact").
- **No new liveness coupling.** SIP-4 already holds every block on
  anchor coverage.

### 9. Recommended v1.1 scope

1. **Precompile:** `poolValue`, `poolTotals`, `blockStats`, `txShielded`
   at the §2 prices, with the pool-delta sign convention and
   `NO_SUCH_POOL`.
2. **Index/follower:** keep the pool, stat and tx-shielded fields already
   fetched, parse strictly, and hold on the §3 cross-checks. No new RPC.
3. **`ZcashBlocks` at `0x…5A01`**, predeployed at the testnet reset and
   written by a pre-block system call: an 8,191-block ring of anchored
   summaries in the state root.
4. **Feed:** `sova_subscribe("zcashBlocks")` / `sova_getZcashBlocks`,
   plus `ZcashBlocks.publish()` for tooling that only speaks
   `eth_getLogs`.
5. **Trigger kit:** `ZcashTrigger.sol` and a reference keeper bot.
6. **Demo:** "Zcash pulse." Out of scope: a native system transaction
   for logs (a later SIP), and viewing-key or proof-based features
   (SIP-4 §8 research).

### 10. Decisions for Rob

1. **Approve the four pool-state reads for the precompile** (`poolValue`,
   `poolTotals`, `blockStats`, `txShielded`). *Recommend: yes.* No new
   RPC, constant-time, and it is exactly "see the amount in the shielded
   pool."
2. **Ship the `ZcashBlocks` system contract at the testnet reset?**
   *Recommend: yes.* The reset is the only cheap moment to change
   genesis. It puts Zcash's pool totals into Sova's state root every
   block, provable with `eth_getProof`. The cost is a wider consensus
   surface, mitigated by §3's cross-checks.
3. **How dapps hear about Zcash events.** *Recommend: RPC feed plus
   `publish()` in v1.1. Defer native per-block logs (a system tx) to a
   later SIP* unless demo users ask for explorer-native events. System
   calls cannot emit visible logs in reth v2.6.0.
4. **Source of pool totals.** *Recommend: zebrad's `valuePools` with the
   §3 cross-checks* (hold on mismatch). Recomputing everything ourselves
   needs an input-value index first.
5. **Sign convention.** *Recommend: "pool delta" everywhere (+ = into the
   pool)*, matching Zebra's `valueDeltaZat`, with the `−valueBalance`
   mapping documented.
6. **First demo.** *Recommend: "Zcash pulse"*, a live, animated page of
   shielded-pool totals and changes read from Sova state each block.
   Second: a pool-level settlement market once the keeper kit exists.

## Appendix A: real RPC shapes (Zebra 6.3.0, testnet, 2026-09-23)

`getblock "4384200" 1` (coinbase-only block), abridged:

```json
{
  "height": 4384200, "version": 4, "nTx": 1, "time": 1790184215,
  "hash": "00809413c7591f17cfe5043546e6ce5d292e595ede7b9f74def7494f1f1cc2bc",
  "chainSupply": {"chainValue": 18231006.37835043, "chainValueZat": 1823100637835043, "monitored": true},
  "valuePools": [
    {"id": "transparent", "chainValueZat": 1573837835978306, "valueDeltaZat": 12500000, "monitored": true, "chainValue": 15738378.35978306, "valueDelta": 0.125},
    {"id": "sprout",      "chainValueZat":   42832983037484, "valueDeltaZat": 0, "...": "..."},
    {"id": "sapling",     "chainValueZat":  152869428798703, "valueDeltaZat": 0},
    {"id": "orchard",     "chainValueZat":   23913312221154, "valueDeltaZat": 0},
    {"id": "lockbox",     "chainValueZat":   15894393750000, "valueDeltaZat": 18750000},
    {"id": "ironwood",    "chainValueZat":   13752684049396, "valueDeltaZat": 125000000}
  ],
  "trees": {"sapling": {"size": 404304}, "orchard": {"size": 248902}, "ironwood": {"size": 354039}},
  "finalsaplingroot": "3ca3…db66", "finalorchardroot": "91aa…b116"
}
```

Block 4,384,160 (3 txs, all v6). Pool deltas: Sapling +125,035,000,
Ironwood −1,035,000, transparent +13,500,000, lockbox +18,750,000.
Per-tx (verbosity 2 / `getrawtransaction … 1`):

```text
tx 5057…6440 (coinbase)  vin=[{coinbase}] vout=[0.125 → t2Hif…]  vShieldedOutput=1
                          valueBalanceZat=-125035000   orchard={actions:[], valueBalanceZat:0}   (no "ironwood" key)
tx 59a6…226a             vin=[] vout=[]   ironwood={actions:2, valueBalanceZat:10000, flags, anchor, proof, bindingSig}
tx 47c0…5c52             vin=[] vout=[1000000 → tmATt…]   ironwood={actions:4, valueBalanceZat:1025000}
```

Check: Sapling `−(−125,035,000) = +125,035,000`; Ironwood
`−(10,000 + 1,025,000) = −1,035,000`. Both match the block's
`valueDeltaZat`. The deshield (47c0…) paid 0.01 TAZ to a t-address out of
Ironwood, plus its fee.

`getblock "4134000" 1` (NU6.3 activation): `trees` has only `sapling`
and `orchard`, and the Ironwood pool is 0. `getblock "1000" 1`:
`trees: {}`, and every pool except transparent is 0.
