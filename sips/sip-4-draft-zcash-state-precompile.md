# SIP-4: Zcash State Precompile

- Status: **Draft, build approved** (Rob, 2026-09-23: "SIP-4 v1 + D1
  escrow"). Open questions 2–5 take the proposed defaults. Step 1 (the
  anchor commitment, §1) is in review on `z1/anchor`; the contract side
  (IZcash, ZcashLib, ZecEscrow) is on `release` against a mock.
- Implementation: none yet. The planned home is `crates/evm`
  (`SovaExecutorBuilder`, today a no-op seam) plus a consensus check in
  `crates/engine/src/consensus.rs`.
- Author: Sova (orchestrated draft, 2026-09-23)
- Depends on: SIP-1 (burn recognition), SIP-2 (epochs, settlement)
- Consensus change: **yes**. The Sova header gains a Zcash anchor
  commitment (§1). It activates at the testnet reset (no fork logic
  needed before any public network exists) or at a fork height after.

## Summary

A precompile that lets EVM contracts read **transparent Zcash chain
state**: whether a transaction is mined and how deep, what a
transparent output pays and to which script, and block hashes and
times. Every answer is a **pure function of the Zcash chain prefix the
Sova block commits to**. Two nodes that accept the same Sova block
compute the same answers, whatever their own zebrad's tip is at the
moment of execution. A node that cannot answer **holds the block**. It
never guesses, and it never marks the block invalid.

Shielded data stays shielded. §8 says plainly what the precompile can't
see.

## Motivation

Sova already verifies Zcash: every node re-derives each epoch's mint
from its own zebrad (SIP-2, C5). But contracts can't see any of it.
A contract can't tell whether a ZEC payment happened, so today nothing
on Sova can react to ZEC moving, and the only link between the two
chains is the burn. That is the blind spot behind "it's hard to
understand the value prop."

The precompile gives contracts the same view of Zcash that consensus
already has, with no custodian involved. It also makes the
*deposit* half of any future ZEC peg trustless
(`docs/design/zec-on-sova-options.md`).

## The determinism problem, stated precisely

Re-execution must be bit-identical. A Sova block's state root depends
on every precompile answer given during its execution. The inputs a
node has are its **own** zebrad, which may be:

1. **behind** (hasn't seen the Zcash block the Sova block settles),
2. **ahead** (has seen blocks the sealer hadn't),
3. **on another fork** (a Zcash reorg in flight, or a zebrad version
   that disagrees about validity), or
4. **unreachable** (RPC error, process down).

A design is acceptable only if, in all four cases, a node either
computes exactly the sealer's answers or refuses to execute the block
for now. Any answer that depends on the tip ("confirmations" as zebrad
reports it, `gettxout` unspent-ness, mempool state) is excluded by
construction. So is any error path that turns "I couldn't ask" into
"not found."

## Specification

### 1. Anchor commitment (consensus)

Sova block `N` settles epoch `E_N = N + B − 1`, where `B` is the
network's epoch base (SIP-2; `SOVA_EPOCH_BASE`). SIP-4 adds:

- **Header rule.** `header.parent_beacon_block_root` MUST equal the
  hash of Zcash block `E_N`, as 32 display-order bytes, the convention
  `crates/consensus/src/follower.rs` already uses. Sova has no beacon
  chain; Zcash is its beacon. The field already exists, reth requires
  it to be present post-Cancun (`consensus/common/src/validation.rs:238-247`),
  and today it carries no meaning. (If the genesis includes the EIP-4788
  beacon-roots contract, the system call also stores the anchor there.
  That gives contracts a second, timestamp-keyed way to read it. To
  verify at genesis-build time.)
- **Validity check** in `SovaConsensus::validate_block_pre_execution`,
  next to C5:
  - The follower has scanned `E_N` and its hash equals the
    commitment: proceed to C5 and execution.
  - The follower has not scanned `E_N` yet: **hold** (transient error).
  - The follower's hash at `E_N` differs: **off-fork** (transient
    error). It is transient because Zcash can reorg back, and a block
    that is valid on a chain we might return to must never land in
    reth's invalid-header cache.
- **Transient means transient.** Both errors return `true` from
  `SovaConsensus::is_transient_error`. reth then skips the
  invalid-header cache (`engine/tree/src/tree/mod.rs:3283-3330`, v2.6.0),
  and the arbiter retries once the follower catches up. Only blocks
  that are internally wrong (withdrawals that match no rank on a
  matching anchor, a bad state root) are permanently invalid.

Consequences, all intended:

- **SIP-2's reorg intent becomes enforced.** Today a Zcash reorg that
  replaces block `E` with a block holding the same burns leaves the Sova
  block valid, because nothing binds it to `E`'s hash. With the anchor,
  every Sova block names exactly one Zcash chain, and the hash chain
  pins all of `E_N`'s ancestors.
- **C5's accept-unknown debt closes.** Today a block above the scanned
  watermark is accepted with a warning (`consensus.rs`, "deferred").
  After SIP-4 it is held. Execution needs the answers, so accepting on
  trust is no longer possible.
- **Nodes without a zebrad can no longer follow.** A node with no zebrad
  "imports on trust" today (`infra-m1.md`). After SIP-4 it cannot execute
  a block that calls the precompile. §11 covers a later light-node path.

### 2. Query horizon

Answers are a function of the anchored chain segment `Z[B .. E_N]` and
nothing else:

- **Heights** `h > E_N` return `NOT_YET`. Heights `h < B` return
  `OUT_OF_RANGE` (v1 indexes only from the epoch base; see open
  questions).
- **Confirmations** are computed as `E_N − h + 1`, never read from
  zebrad. zebrad's `confirmations` field is tip-relative.
- **No mempool, no tip, no "unspent now".** zebrad's `gettxout` answers
  against the current UTXO set, so it is never used. Spentness is
  answered only from the index, as of `E_N` (§3, `spentBy`).

**No protocol confirmation depth.** An earlier idea answered only up to
`E_N − K`. Once the anchor hash is committed, `K` adds nothing to
determinism: every validating node must already hold block `E_N` to
check the mint, and any reorg of `Z[.. E_N]` reorgs the Sova block
whatever `K` is. Economic safety is the contract's business, so every
answer reports its depth. The Solidity library requires an explicit
`minConf` on every check and has no default.

### 3. Interface

One precompile address (provisional): `0x0000000000000000000000000000000000005A00`.
It uses Solidity ABI dispatch (4-byte selectors), so contracts call it
through an ordinary `interface IZcash`. It is read-only: a call with
`value > 0` reverts, and `STATICCALL` and `DELEGATECALL` both work.

Byte order: `txid` and block hashes are **display-order** bytes (what
explorers and RPC hex show), as in the follower. Values are integer
**zatoshis** (zebrad's `valueZat`, never the float `value`).

| Method (v1) | Returns | Notes |
|---|---|---|
| `anchor()` | `(uint64 height, bytes32 hash)` | `E_N` and its committed hash |
| `blockAt(uint64 h)` | `(uint8 status, bytes32 hash, uint32 time)` | header time, for `B ≤ h ≤ E_N` |
| `txInfo(bytes32 txid)` | `(uint8 status, uint64 height, uint32 index, uint64 confirmations, uint16 nOut, uint32 version)` | any tx, fully shielded ones included (txid, height and position are public) |
| `txOutput(bytes32 txid, uint32 vout)` | `(uint8 status, uint64 valueZat, bytes script)` | transparent outputs only; script ≤ 10,000 bytes |
| `burnInfo(bytes32 txid)` | `(uint8 status, address credited, uint32 signal, uint64 weightZat)` | the exact consensus SIP-1 parser (`sip1::extract_burn`) |
| `spentBy(bytes32 txid, uint32 vout)` *(v1.1)* | `(uint8 status, bytes32 spender, uint64 height)` | outputs created at `≥ B`; needs the follower to index inputs |

Status codes: `OK = 0`, `NOT_FOUND = 1` (not in `Z[B .. E_N]` on the
anchored chain), `NOT_YET = 2`, `OUT_OF_RANGE = 3`, `NO_SUCH_OUTPUT = 4`.
"Not found" is a **result**, and it is consensus-identical on every
honest node. Malformed calldata reverts.

Two contract-facing caveats go in the library docs:

- **Pre-v5 txids are malleable.** v4 transparent signatures can be
  re-encoded by third parties, which changes the txid. ZIP-244 (v5+)
  txids are not. Contracts should key on what an output pays (script
  and value), or reserve an order before payment (§9), rather than
  trusting a txid chosen in advance.
- **Coinbase transactions are included.** Their outputs cannot be spent
  transparently on testnet/mainnet until shielded (seen live on
  2026-09-23: `WORKPLAN.md`).

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
Precedent: the Bitcoin-era Sova priced its pure decode precompile at
3,000 + 3/byte (`sova-reth/evm/src/precompiles/precompile_utils.rs`).

### 5. Where answers come from (node side)

- **Only from a local, hash-checked index, never from RPC during
  execution.** The follower already fetches every transaction's
  transparent outputs for every epoch (`zebrad.rs`: `getblock` plus a
  `getrawtransaction` per tx). SIP-4 persists what it already fetches,
  keyed by block hash: txid → (height, index, version, outputs); height
  → (hash, time); and, in v1.1, spent outpoint → (spender, height).
  Follower `Rollback` events delete the unwound blocks' entries. The
  store is written by the same scan that feeds C5, so the mint and the
  precompile can't see different Zcash chains.
- **The precondition is checked before execution.** §1 guarantees that
  a block reaches execution only when the index covers `E_N` on the
  committed chain. The sealer builds epoch `E` only after its follower
  emitted `E` (already true in `driver.rs`).
- **The belt-and-braces path is crash, not divergence.** If execution
  still hits a missing record at or below `E_N` (a bug or a corrupt
  store), the precompile returns `PrecompileError::Fatal`. revm treats
  that as an abort, not a revert. reth v2.6.0 maps
  `BlockExecutionError::Internal` to `InsertBlockFatalError`
  (`engine/primitives/src/error.rs:109-132`), which halts the engine.
  A stopped node is recoverable; a node with a divergent state root
  silently forks.
- **The reth precompile cache must be off for this address.** reth's
  engine wraps "cacheable" precompiles in a cache keyed on
  `(calldata, spec_id)` **across blocks**
  (`engine/tree/src/tree/payload_validator.rs:1049-1066`,
  `precompile_cache.rs:183-190`). `txInfo(txid)` returns different
  confirmations at `N` and `N+1`, so a cached answer would split
  importers from builders. It must be registered with
  `DynPrecompile::new_stateful`, which sets `supports_caching = false`.
  A test asserts this (§11).
- **RPC.** `eth_call` or `eth_estimateGas` against a block whose anchor
  is not indexed returns an RPC error. There is no consensus effect.

### 6. Failure modes

| Situation | Node behavior | Can state roots diverge? |
|---|---|---|
| Own zebrad behind `E_N` | Pre-execution transient hold; imports when the follower catches up | No. It stalls (liveness only) |
| Own zebrad on another Zcash fork at `E_N` | Transient off-fork; held; imports if Zcash converges on the committed chain | No |
| zebrad RPC down or erroring | Follower retries; the index doesn't advance, so blocks hold | No |
| Two zebrad versions disagree on Zcash validity (e.g. a missed NU) | They are on different Zcash chains, so anchors mismatch and the minority holds | No. Visible as a stall and alertable ("epoch lag", `infra-m1.md`) |
| Index missing a record `≤ E_N` (bug) | `PrecompileError::Fatal`, engine halts | No, by design |
| reth precompile cache enabled by mistake | Would replay stale answers | **Yes**. Prevented by `new_stateful` plus a test |
| Answer read from tip or mempool by mistake | Would differ per node | **Yes**. Prevented by the spec (§2) and the differential sim (§11) |
| Deep Zcash reorg (> follower window, 1,024) | Follower unwinds to base and rescans; Sova reorgs accordingly | No; expensive |

### 7. Zcash reorgs

- **Protocol.** A Zcash reorg to height `R` invalidates (transiently)
  every Sova block with `E_N > R`, on every node, at the same moment
  its zebrad sees the reorg. Sova unwinds to `N = R − B + 1` and
  re-seals on the new branch. Contract state that depended on a
  reorged-out Zcash tx rolls back with it. That is consistent, because
  every node rolls back identically.
- **Required work.** Today the sealer logs follower `Rollback` and
  continues (`driver.rs`, "v0 logs and continues"), and the arbiter
  never moves the head backwards on a Zcash event. SIP-4 needs a real
  rollback: on `Rollback{to_height: R}`, FCU the Sova head to
  `R − B + 1` and resume sealing. Without it, a node keeps a head
  whose anchor its own zebrad now rejects. The C5 story needs the same
  fix, so it is not SIP-4-only work.
- **Contracts.** A Sova reorg can't undo anything outside Sova, such as
  goods shipped or assets bridged elsewhere. That is what `minConf` is
  for. Suggested defaults for the library docs: testnet 3, mainnet 10
  (≈ 12.5 min at 75 s), and more for large values.

### 8. What is impossible, and what is merely later

**Impossible (by Zcash's design, not our engineering):** the amount,
recipient, sender or memo of a shielded transfer; the balance of any
shielded address; "did shielded address X get paid". Nobody can read
these without keys, and neither can the precompile.

**Public, so possible later:**

- **Per-pool value balances** (`valueBalanceSapling`, Orchard,
  Ironwood) and action counts of any tx. For example, "this tx
  deshielded ≥ 5 ZEC".
- **Nullifier and note-commitment membership.** Of little use without
  in-EVM verification of Zcash proofs.

**Possible only with a key someone chooses to publish (research):** a
merchant or vault that publishes an incoming viewing key could have
every node trial-decrypt Sapling/Orchard/Ironwood outputs to that key.
That is deterministic, but costly per output per registered key, and it
reveals the publisher's incoming history (not the payers'). Verifying a
ZIP-311 payment disclosure (Draft) in-EVM would need Halo2 verification
in a precompile. It is research, not a SIP-4 item.

**Pool facts that matter here** (verified in Zebra 6.3.0 source,
`research/zebra-upstream` at `f5c5277`): NU6.3 "Ironwood" is active on
testnet from height 4,134,000 and scheduled on mainnet at 3,428,143
(`zebra-chain/src/parameters/constants.rs`). It introduces v6
transactions and a new Ironwood shielded pool. From NU6.3,
`valueBalanceOrchard` MUST be non-negative: no net new value enters
Orchard (`zebra-consensus/src/transaction/check.rs`,
`orchard_value_balance_non_negative`). The follower must parse v6
transactions' transparent parts. An unparseable transaction must stall
the follower (hold), never be skipped.

### 9. Use cases

1. **Custody-free ZEC ↔ SOVA trades (escrow and OTC).** The SOVA side
   locks SOVA in a Sova escrow contract. The ZEC side **reserves** the
   order on Sova (with a small bond, for a window `T`), then pays the
   agreed ZEC to a fresh maker-controlled t-address named in the order.
   Once `txOutput` shows at least the price to that script, at
   `≥ minConf` inside the window, the contract releases the SOVA to the
   reserver. No one holds anyone's ZEC, and the ZEC never leaves Zcash.
   Because the address is per order and the reservation names the
   claimant, the payer needs no OP_RETURN, so **any Zcash wallet that
   can send to a t-address can pay, including from a shielded balance**.
   Honest limits: the payer holds a free option during `T` (bonds and
   short windows limit it), and the maker's receiving address is
   transparent (they can sweep it to shielded afterwards).
2. **Pay in ZEC, get something on Sova.** An Ashwings mint, an agent's
   API credit, a subscription. The same reserve-then-pay pattern pays
   the seller's own Zcash address directly, and the contract delivers
   on proof. This is the simplest thing that makes "shielded money can
   pay for a contract outcome" true today, with privacy at the funding
   edge (a z→t payment), exactly as burns have it.
3. **Proof-of-burn as an app primitive, and trustless peg deposits.**
   Apps can recognize their own burns: ZEC paid to SIP-1's eater with an
   app-specific payload magic. That payload is deliberately *not*
   SIP-1's `"SV"`, so it mints no SOVA. Uses include name registration,
   Sybil-resistant badges, and spam fees that nobody collects. No
   proceeds recipient exists, so the pure-burn rule carries over. The
   same `txOutput` check lets a peg contract mint wZEC against a
   verified deposit with no signer's word involved (options paper,
   option A).

### 10. Implementation sketch (reth v2.6.0 seams)

- **`crates/evm`.** `SovaEvmFactory` wraps `EthEvmFactory`. `create_evm`
  builds the spec's `PrecompilesMap` and then calls
  `extend_precompiles([(ZCASH_QUERY, DynPrecompile::new_stateful(id, f))])`.
  `f` holds an `Arc<ZcashIndex>`, reads the Sova height from
  `input.internals().block_number()`, derives `E_N`, and answers from
  the index. `SovaExecutorBuilder::build_evm` returns
  `EthEvmConfig::new_with_evm_factory(chain_spec, SovaEvmFactory{..})`
  (`ethereum/evm/src/lib.rs:115`). reth's `examples/custom-evm` is the
  template.
- **`crates/consensus`.** The follower records block `time` (already in
  `getblock` verbosity 1) and, in v1.1, `vin` outpoints. `ZcashIndex` is
  a small embedded store with a `Rollback` API. `EpochData` gains
  `time`.
- **`crates/engine`.** The anchor check lives in `SovaConsensus`, as two
  new transient `SettlementError` variants (`AnchorUnscanned` and
  `AnchorOffFork`). The payload-attributes builder sets
  `parent_beacon_block_root` from the `PendingEpoch` hash. The
  sealer/arbiter FCUs back on a follower `Rollback` (§7).
- **`contracts/`.** `IZcash.sol` and a `ZcashLib.sol` helper
  (`requireOutputPays(txid, vout, script, minZat, minConf)` and P2PKH /
  P2SH script builders), plus the escrow from use case 1 as the demo.
- **Process.** Global wiring follows the existing `expectations::global()`
  pattern until `SovaNodeAddOns` gets real dependency injection.

### 11. Test and simulation plan

- **Unit tests.** ABI golden vectors per method; status semantics at
  `B − 1`, `B`, `E_N`, and `E_N + 1`; the confirmations formula; the gas
  table; `burnInfo` equal to `sip1::extract_burn` on the SIP-1 fixtures;
  and an assertion that the registered precompile reports
  `supports_caching() == false`.
- **Property tests.** Two indexes fed the same mock chain answer
  identically. An index after `Rollback` plus a rescan equals a fresh
  index (mirrors the follower's existing determinism tests).
- **Consensus tests.** An unscanned anchor is transient and stays out of
  the invalid cache; it imports after the scan. An off-fork anchor is
  transient, and the block becomes valid again when the mock Zcash
  chain reorgs back. A tampered anchor with correct withdrawals is
  rejected.
- **Box scenarios** (nightly, `box/sim`):
  1. **Lagging zebrad.** SIGSTOP node B's zebrad while A seals blocks
     whose transactions call every method. B holds, resumes, and ends
     on the same state root as A.
  2. **Zcash reorg across a dependent tx.** A regtest
     `invalidateblock` removes the paying tx. Both nodes reorg Sova, the
     escrow contract state rolls back identically, and the tx re-mined
     at a new height gives new confirmations on both nodes.
  3. **Late joiner.** A fresh node syncs history full of precompile
     calls through the catch-up path. State roots match at every
     height.
  4. **Differential.** Nodes on two zebrad versions (current and
     previous) produce identical answers over the same chain.
  5. **Gas DoS bench.** Blocks at the gas limit with only lookups
     (hits and misses) must meet the §4 time gate.
- **Mint regression.** All existing scenarios stay green, which proves
  the anchor rule didn't disturb C5 or the ladder.

### 12. Cost and risk (honest)

- **Consensus surface grows.** Execution now depends on each node's
  Zcash index, so an index bug is a chain split. Mitigations: the index
  shares its scanner with C5, §6's crash-over-diverge rule, and the
  differential sim.
- **Liveness is coupled to zebrad.** A zebrad hiccup stalls its Sova
  node. That is the price of never guessing. Operators need the
  epoch-lag alert (`infra-m1.md`).
- **Trust-mode following ends** (§1). Every Sova node needs a zebrad,
  which it already needs to validate mints. Public RPC and indexer
  operators feel this most.
- **Sova tip churn grows.** Any 1-block Zcash reorg at the tip now
  reorgs the Sova tip. SIP-2 intended this, but it becomes visible.
- **Dapp risk.** Contracts that use `minConf = 1` get reorged. The
  library forces the choice; docs recommend depths.
- **Effort (estimate):** anchor, consensus rule and rollback wiring,
  1–2 weeks. Index, precompile, gas and tests, about 2 weeks. Solidity
  library plus the escrow demo, about 1 week. Box scenarios run in
  parallel. Total **~4–6 weeks** before a testnet demo, at the current
  pace.

### 13. Alternatives considered

- **Live zebrad RPC inside the precompile** (the Bitcoin-era Sova
  pattern). Its precompiles made HTTP calls to an operator service
  during execution and ignored broadcast errors
  (`sova-reth/evm/src/precompiles/bitcoin_precompile.rs`). Rejected:
  tip-dependent, and error handling decides consensus.
- **A fixed depth `K` without an anchor hash.** Rejected: a reorg deeper
  than `K` silently gives late-syncing nodes different answers, so
  history forks.
- **The sealer puts the answers in the block** (an oracle in the body).
  Equivalent to the anchor rule if validators re-check the answers.
  Blocks get heavier and nothing is gained while every node has a
  zebrad.
- **Proof-carrying queries (the v2 direction).** The caller supplies the
  raw transaction plus a merkle branch. The precompile recomputes the
  ZIP-244 txid, checks the branch against the committed header chain,
  and needs **only Zcash headers**. That is the path to light Sova nodes
  that don't run zebrad. The costs are more calldata, wallet-side proof
  building, and a ZIP-244/v6 txid digest implementation inside the
  precompile. It is worth doing after v1, not instead of it.

## Open questions (Rob)

1. **Approve the consensus change**: the anchor in
   `parent_beacon_block_root`, the hold-not-accept rule, and the end of
   no-zebrad "trust-mode" following. It ships at the testnet reset.
2. **v1 scope**: the five methods in §3 (`anchor`, `blockAt`, `txInfo`,
   `txOutput`, `burnInfo`), with `spentBy` as v1.1. Is `spentBy` needed
   for the testnet demo? (Peg theft detection needs it; the escrow
   demo doesn't.)
3. **Lookback**: index from the epoch base `B` only (proposed), or
   earlier Zcash history (a bigger index, and payments made before
   genesis become visible)?
4. **Library confirmation defaults** in the docs: testnet 3, mainnet 10?
5. **First demo**: the ZEC↔SOVA escrow (proposed) or pay-in-ZEC for an
   Ashwings mint?
