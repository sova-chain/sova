# Sova Simulation Harness (C6, worker slice)

Proves multi-instance determinism at the layer that exists today — the
Zcash follower (`crates/consensus::follower::Follower` over
`crates/consensus::zebrad::ZebradClient`) — against a real `zebrad`
regtest node, using a small deterministic-replay binary
(`consensus_sim`) as the probe.

## What this proves today

The follower turns a live Zcash chain into an ordered, reorg-aware
sequence of epochs (`FollowerEvent::Epoch` / `FollowerEvent::Rollback`;
see `crates/consensus/src/follower.rs`'s own determinism note). Sova's
whole burn-to-mine consensus model rests on that stream being a *pure
function of the chain*: every honest node that scans the same Zcash
history must derive the same epoch sequence, independent of process
identity, wall-clock timing, or the exact path (rollback + rescan vs.
one continuous scan) taken to get there.

`consensus_sim` (`crates/consensus/src/bin/consensus_sim.rs`) makes that
claim checkable. It connects a `Follower` (base height 1, window 50) to
a `zebrad` RPC endpoint, polls until it has caught up to the chain's
current tip, then prints:

```text
<height> <hash_hex> <burn_count> <total_burned_zat>
```

one line per epoch, followed by:

```text
STREAM_DIGEST <sha256 hex of every line above, newline-joined>
```

Two runs that print the same `STREAM_DIGEST` observed byte-identical
epoch streams. `run-scenarios.sh` uses that single fact to check three
independent claims against a real regtest chain, real SIP-1 burns
(via the D2 `sova-miner` CLI), and real `zebrad` reorg mechanics
(`invalidateblock`) — not mocks:

1. **Parallel determinism.** Mine a real chain with a few SIP-1 burns in
   it, then run two `consensus_sim` processes *concurrently* against the
   same node. They must print the same digest. This is the base case:
   the follower has no shared mutable state between instances, so
   agreement here is the minimum bar.
2. **Reorg convergence.** Invalidate the chain's tip block, mine a
   replacement branch, then run two *fresh* `consensus_sim` instances.
   They must agree with each other, and their digest must **differ**
   from scenario 1's — proving the reorg actually changed the observed
   stream, not that the check is a no-op. This exercises the exact path
   `crates/consensus/src/follower.rs`'s `Rollback` handling and C2a's
   own live acceptance test (`crates/consensus/tests/follower_regtest.rs`)
   cover, but as an end-to-end multi-instance proof rather than a single
   in-process assertion.
3. **Restart equivalence.** Run `consensus_sim` once, then run it again
   as a brand-new process against the same (now static) chain. Same
   digest. This is the case that matters most for the eventual sealer:
   a node that crashes and restarts must rebuild the exact same view of
   the world from the chain alone, with no in-memory state to recover.

Scenarios 1-3 above are proven at the **follower layer only** — no Sova
node, no Engine API, no sealer, no gossip involved. That was deliberate
at the time this harness was first written: C3's async
follower-to-Engine-API loop hadn't landed yet, so there was no full node
to run.

C3 has since landed (`crates/engine/src/driver.rs`'s `SealerCore`/
`run_sealer`, wired into `bin/sova`'s mine mode). **SCENARIO 4** below
closes the single-node half of the resulting gap: a real `bin/sova`
process, in mine mode, proven to mint correctly from real SIP-1 burns —
lockstep block production, exact reward amounts, repeated settlement —
end to end, not mocked.

Gossip v1 (`docs/design/gossip-v1.md`) has since landed too
(`crates/engine/src/relay.rs`, `bin/sova`'s mine/follow-only modes), and
the **TWO-NODE RELAY SCENARIO** below closes the N=2 case of the
multi-node half: two independent `bin/sova` processes, wired together
only by the relay task over the Engine API, landing on identical block
hashes (state roots) and an identical mint. What's still missing is
scaling that same proof to N≥3 and the fault/reorg scenarios — see
Extension plan below for exactly what that still needs.

## Running it

```bash
box/sim/run-scenarios.sh
```

Reuses `box/regtest`'s compose stack (a fresh one every run — `down -v`
then `up -d`, same as `box/regtest/follower-e2e.sh`) and its
`auto-mine.sh`, plus the D2 miner CLI (`crates/burn-wallet/miner`,
binary `sova-miner`) to produce real burns. Each scenario prints
`PASS`/`FAIL`; the script exits non-zero if any scenario failed, and
always tears the stack down (`docker compose down -v`) on exit,
success or failure. Set `SOVA_REGTEST_RPC` to point at a different
`zebrad` RPC endpoint; defaults to `http://127.0.0.1:18232`.

If `sova-zebrad-regtest` is already running when you start this, the
script tears it down and starts fresh rather than reusing it — the
reorg/restart scenarios depend on controlling the chain's full history
from height 1, which a reused container with unknown prior state can't
guarantee.

Scenarios 1-3 run first, against one shared stack; that stack is then
torn down and scenario 4 (`box/sim/mint-scenario.sh`, delegated to from
the end of `run-scenarios.sh`) gets its own fresh one, since it needs a
real `bin/sova` process bound to `127.0.0.1:8545` rather than
`consensus_sim`. `mint-scenario.sh` is also runnable standalone:

```bash
box/sim/mint-scenario.sh
```

The two-node relay scenario (below) is standalone only — not wired
into `run-scenarios.sh` — since it needs two `bin/sova` processes
instead of one:

```bash
box/sim/two-node-scenario.sh
```

## CI coverage

`.github/workflows/nightly-sim.yml` runs this whole harness — scenarios
1-4 via `run-scenarios.sh`, then `two-node-scenario.sh` — once a day
(~09:00 UTC) and on manual `workflow_dispatch`, on a stock `ubuntu-latest`
runner with a real Docker daemon. It is deliberately **not** wired to
`push`/`pull_request`: these scripts take several minutes and stand up
real `zebrad` regtest chains, so they're a nightly early-warning signal
for the follower/sealer/relay stack rather than a gate on every PR (that
job is `.github/workflows/ci.yml`'s `build-test`). Each scenario script
is allowed to fail independently so one script's failure doesn't hide the
other's result; either failing fails the job (surfacing as a scheduled-
run failure notification) and uploads both scripts' captured output as
the `nightly-sim-logs` artifact, since both scripts' own `EXIT` traps
delete their working directory — logs, PASS/FAIL lines and all — on
every exit, pass or fail.

## Evidence: a green run (scenarios 1-3)

Captured 2026-09-21, `zfnd/zebra:6.3.0`, Docker Desktop on macOS/arm64,
clean regtest stack, no other consumer of the harness running
(`docker ps` checked immediately before, per the standing operating
note that only one process may hold the harness at a time):

```
=== SCENARIO 1: parallel determinism ===
miner address: tmRHEpWbdkTuUbPZaBrpFuvfZTQs1dfSF9R
--- funding: 1 block to the miner, then 100 blocks elsewhere (coinbase maturity) ---
tip after funding: 101 (expected 101)
--- starting auto-mine (1 block every 2s) in the background ---
--- running sova-miner mine for 3 epochs (a few SIP-1 burns sprinkled into the chain) ---
epoch 1: height=103 burn=100000zat fee=20000zat change=624880000zat txid=f835025edbecb41de834aeea282a1a28139d01575c11796ce819152733e0e8ef
epoch 2: height=105 burn=100000zat fee=20000zat change=624760000zat txid=3688f38c3d5843ab2e7e4aadc20c93aa0420fae3a9938c217ec966213d4acf39
epoch 3: height=107 burn=100000zat fee=20000zat change=624640000zat txid=3db0f124fafcfcde4602254dc8966d58911678f55c075cf1699e97d83510ea32
reached --max-epochs 3 -- stopping.
scenario 1 final tip: 131
PASS: scenario 1 (parallel determinism): two concurrent consensus_sim instances agree — STREAM_DIGEST 74edd8ea694276af3d148372e7e8372c7fd323b896d06a2c81187743ece70a80 (131 epoch lines)

=== SCENARIO 2: reorg convergence ===
invalidating the current tip (height 131); replacing with 3 new blocks
scenario 2 final tip: 133 (was 131 before the reorg)
PASS: scenario 2 (reorg convergence): two concurrent consensus_sim instances agree — STREAM_DIGEST 9185627673a021001157d59640eeae04910e0f9f0ff2bc83e10583f42e94df8a (133 epoch lines)
PASS: scenario 2 (reorg convergence): STREAM_DIGEST differs from scenario 1 (74edd8ea694276af3d148372e7e8372c7fd323b896d06a2c81187743ece70a80 -> 9185627673a021001157d59640eeae04910e0f9f0ff2bc83e10583f42e94df8a), proving the reorg changed the stream

=== SCENARIO 3: restart equivalence ===
PASS: scenario 3 (restart equivalence): sequential fresh-process runs agree — STREAM_DIGEST 9185627673a021001157d59640eeae04910e0f9f0ff2bc83e10583f42e94df8a

ALL SCENARIOS PASSED
```

Exit code `0`; `docker ps` confirmed no `sova-zebrad-regtest` container
and no stray `auto-mine.sh`/`sova-miner` processes left running
afterward.

## A real finding along the way (not a Sova bug — reported precisely)

An earlier version of this harness backgrounded `auto-mine.sh` through a
`(cd ... && ./auto-mine.sh ...) &` subshell wrapper. `$!` in that form
captures the *subshell's* PID, not `auto-mine.sh`'s own — so
`kill "$AUTO_MINE_PID"` killed the subshell but left the real
`auto-mine.sh` loop as an orphaned process, still calling zebrad's
`generate` RPC every ~2s indefinitely, invisible to the rest of the
script.

With that orphan still running from a prior invocation and a *second*
`auto-mine.sh` started by the next run against the same RPC port, two
independent processes were racing blocks into the same node. Under that
race, invalidating a block roughly 10+ deep while a competing `generate`
landed concurrently reproducibly crashed `zebrad` itself
(`zfnd/zebra:6.3.0`, exit code 133 / SIGABRT):

```
thread '<unnamed>' (53) panicked at zebra-state/src/service/non_finalized_state.rs:1016:26:
just checked recent fork height
  ...
   5: update_metrics_bars
             at /zebra/zebra-state/src/service/non_finalized_state.rs:1016:26
   6: insert_with<...invalidate_block::{closure_env#1}>
             at /zebra/zebra-state/src/service/non_finalized_state.rs:285:14
   7: invalidate_block
             at /zebra/zebra-state/src/service/non_finalized_state.rs:405:18
   8: run
             at /zebra/zebra-state/src/service/write.rs:362:61
```

This is a genuine upstream `zebra-state` panic (an `.expect()` on a
`fork_height` lookup that the comment claims was "just checked"), but it
is **not a Sova consensus/follower determinism bug** — it's a
concurrency edge case in Zebra's own non-finalized-state bookkeeping,
triggered here purely by a bug in this harness's own process lifecycle
management. Fixed in this harness by:

- Backgrounding `auto-mine.sh` directly (no subshell wrapper), so
  `kill "$AUTO_MINE_PID"` reaches the real process.
- A startup guard (`pgrep -f auto-mine.sh...`) that hunts down and kills
  any stray one from a previous run before starting.
- An `assert_tip_quiescent` check before every digest comparison, so
  "the chain is static" is a checked precondition, not an assumption —
  it would have caught this immediately instead of producing a
  hard-to-diagnose crash three steps later.
- Scenario 2 deliberately invalidates the chain's **current tip**
  (depth 1) rather than a deeper mid-chain block, matching the exact
  pattern already proven safe by C2a's own live acceptance test
  (`crates/consensus/tests/follower_regtest.rs`). This keeps the
  scenario inside a boundary already known not to trigger the Zebra
  panic; it does not by itself prove deeper invalidation is safe under
  concurrency load, which is upstream-Zebra territory outside C6's
  scope. Worth a follow-up issue against `ZcashFoundation/zebra` if
  someone wants to pin down the exact trigger.

No determinism deviation was found in the Sova follower/consensus code
itself across any of the three scenarios above.

## SCENARIO 4: live mint invariants (Act I regression)

Where scenarios 1-3 replay a chain through `consensus_sim`, scenario 4
(`box/sim/mint-scenario.sh`) runs the real thing: a single `bin/sova`
process, started in mine mode (`SOVA_ZEBRAD_RPC`/`SOVA_MINER_EVM_ADDRESS`/
`SOVA_EPOCH_BASE`, per `bin/sova/src/main.rs`'s own doc comment), against
a fresh zebrad regtest chain, minting from real SIP-1 burns submitted by
the D2 `sova-miner` CLI (`crates/burn-wallet/miner`) — the exact flow
proved by hand once (2026-09-21) and turned here into a from-cold,
twice-green regression. It is the first C6 coverage of the actual node
binary end to end (real Engine API, real trigger-mode `LocalMiner`, real
sealer loop), not just the follower layer.

Four assertions, all checked against one continuously-running node:

1. **Lockstep.** `eth_blockNumber` tracks the live Zcash tip — sampled 3
   times over ~15s while `auto-mine.sh` keeps the chain moving, allowing
   at most 1 block of lag (mine mode fires one Sova block per Zcash
   block; see `bin/sova`'s doc comment).
2. **First mint, exact amount.** After a real burn's epoch settles, the
   miner's EVM balance is *exactly* one epoch's reward: 6,250 SOVA in
   wei. The expected value is derived from `DRAFT_EPOCH_REWARD_GWEI`
   (`crates/engine/src/driver.rs:31`, `6_250 * 1_000_000_000` gwei) and
   the script guard-checks that literal against the live source file
   before running anything live — if the constant ever changes without
   this test being updated, it fails immediately and loudly instead of
   silently checking a stale number.
3. **Repeated settlement.** A second burn, in a later epoch, mints
   *exactly* another 6,250 SOVA — cumulative balance 12,500 SOVA in wei.
   This is the regression that matters most: it proves settlement isn't
   a one-shot fluke of the first epoch, and re-exercises the exact
   single-slot `PendingEpoch` mailbox path
   (`crates/engine/src/driver.rs`'s `SealerCore::process`) a second time.
4. **Log invariant.** The node's own log contains `sova epoch trigger …
   settled=true` for exactly the two burn epochs — no more, no fewer.

Robustness: builds `bin/sova` and `sova-miner` only if missing; kills
stray `bin/sova` processes and any leaked `auto-mine.sh`/regtest
container at start *and* in its own `EXIT` trap; prints `PASS`/`FAIL`
per assertion; exits non-zero if any assertion failed; always tears its
stack down.

A real nondeterminism observed while proving this out, documented rather
than hidden: reth's trigger-mode `LocalMiner` occasionally logs a
transient `Error updating fork choice: … too deep reorg` /
`Received invalid forkchoice updated message` pair, self-recovering
within the same ~1s poll interval with no effect on chain progress. It
was observed twice — once in the original by-hand proof this scenario
formalizes, and once more during this scenario's own development (see
"A script bug found and fixed" below, whose run hit ~90s of the sealer
idling under sustained triggers with no burn to settle, the exact
condition it showed up under both times). It was **not** observed in
either of the two official from-cold green runs recorded in the evidence
block below — this scenario's own `EXIT` trap only dumps the node's log
on failure (to avoid bloating a passing run's output), so a definitive
"never happens on a clean run" claim isn't available, only "not seen when
it mattered." The single-slot `PendingEpoch` mailbox also means, in
principle, the Sova block height that carries a mint need not equal the
Zcash epoch height number that produced it — in both green runs below it
did (`height=103` for burn 1, `height=112` for burn 2, matching the
`sova epoch trigger height=…` lines exactly), but this scenario's
assertions poll for the *balance* and *log content* outcomes rather than
asserting a specific block height, exactly so that kind of harmless
timing slop couldn't turn into a false failure if it ever didn't line up.

### A script bug found and fixed (not a Sova bug)

The first cold run of this scenario (pre-fix) failed assertions (c) and
(d): burn 2's `sova-miner mine` exited immediately with
`budget exhausted at height 111: next epoch needs 120000 zat, only 80000
zat remain`, despite being invoked with a fresh `--budget-zat 200000` —
the same value burn 1 used successfully. Root cause, confirmed by reading
`crates/burn-wallet/miner/src/state.rs` and `.../epoch.rs`: while
`MinerState::budget_zat` (the declared cap) *is* overwritten fresh by
every `mine` invocation, the spend it's checked against
(`budget_remaining_zat() = budget_zat - total_spent_zat()`) sums
`total_burned_zat`/`total_fee_zat`, which are **lifetime** running totals
across the whole `state.json` sidecar, accumulated by `record_epoch` and
never reset. So burn 1's 120,000 zat (100,000 burn + 20,000 ZIP-317 fee)
permanently counted against any budget declared afterward — burn 2's
"fresh" 200,000 zat budget minus that lifetime 120,000 zat spend left
only 80,000 zat, short of the 120,000 zat the second epoch needed. Fixed
by declaring a `BUDGET_ZAT` (500,000 zat) that comfortably covers the
*cumulative* cost of both burns from the start, matching the same
generous-budget convention `run-scenarios.sh` and `box/regtest/
miner-ac.sh` already use for the same reason. Not a Sova consensus bug —
a budget-accounting misunderstanding in this scenario's own script,
caught by the very first cold run it was supposed to prove out.

### Evidence: a green run (scenario 4)

Captured 2026-09-22, two consecutive from-cold runs (`docker compose down
-v` between them, fresh `zfnd/zebra:6.3.0` regtest chain each time),
immediately after the fix above, `bin/sova` freshly rebuilt (debug) for
this run. Both passed all 4 assertions; run 1 shown in full, run 2
condensed (identical shape, different miner identity, as expected of two
independent fresh keystores):

```
=== burn 1 ===
--- sova-miner mine (--max-epochs 1; NO --evm-address, identity from init) ---
sova-miner mine: address=tmKoi6Li3qL7m7V7METGDYJ4By63YYYHy5y evm=0x6eb4ab230189a1948ca6faf69835de346c1bb971 rpc=http://127.0.0.1:18232 budget=500000zat per-epoch=100000zat
baseline tip height: 101 (epochs trigger on new blocks past this)
epoch 1: height=103 burn=100000zat fee=20000zat change=624880000zat txid=94a392f861a2a6ee0419b8b838fe60a8c0a7e47a012f25ab10a7d6f8927e1349
reached --max-epochs 1 -- stopping.
--- (b) waiting for burn 1's epoch to settle (EVM balance to move off zero) ---
PASS: scenario 4 (live mint invariants): (b) first mint: balance == 6250000000000000000000 wei (exactly 6,250 SOVA)

=== (a) lockstep sampling ===
  sample 1/3: zcash_tip=104 sova_block=103 lag=1
PASS: scenario 4 (live mint invariants): (a) lockstep: sample 1 zcash_tip=104 sova_block=103 lag=1 (<=1 OK)
  sample 2/3: zcash_tip=107 sova_block=106 lag=1
PASS: scenario 4 (live mint invariants): (a) lockstep: sample 2 zcash_tip=107 sova_block=106 lag=1 (<=1 OK)
  sample 3/3: zcash_tip=110 sova_block=110 lag=0
PASS: scenario 4 (live mint invariants): (a) lockstep: sample 3 zcash_tip=110 sova_block=110 lag=0 (<=1 OK)

=== burn 2 ===
--- sova-miner mine (--max-epochs 1; same data dir, own UTXO chains through change) ---
sova-miner mine: address=tmKoi6Li3qL7m7V7METGDYJ4By63YYYHy5y evm=0x6eb4ab230189a1948ca6faf69835de346c1bb971 rpc=http://127.0.0.1:18232 budget=500000zat per-epoch=100000zat
baseline tip height: 110 (epochs trigger on new blocks past this)
epoch 2: height=112 burn=100000zat fee=20000zat change=624880000zat txid=b09fc2ac72252399463061ef14308655797f3f8ef1ceb1de84370ce60380ea52
reached --max-epochs 1 -- stopping.
--- (c) waiting for burn 2's epoch to settle (EVM balance to move off 6250000000000000000000) ---
PASS: scenario 4 (live mint invariants): (c) repeated settlement: cumulative balance == 12500000000000000000000 wei (exactly 12,500 SOVA)

=== (d) settled=true log invariant ===
--- stopping auto-mine and bin/sova so the log is final before counting ---
settled=true count: 2
PASS: scenario 4 (live mint invariants): (d) log invariant: settled=true appears exactly 2 times (the two burn epochs)

SCENARIO 4 PASSED (all 4 assertions)
```

Run 2 (fresh keystore `tmCAZEm5oTYUHuhus4QLh8AKMv5ryCu2Euk` /
`0x1ae5017bdb5aced4c24a07866fd16fa34ff802e0`): identical shape end to
end — `epoch 1: height=103 …`, `epoch 2: height=112 …`, the same three
lockstep samples (`104/103`, `107/106`, `110/110`), `(b)` at exactly
6,250 SOVA, `(c)` at exactly 12,500 SOVA, `(d)` at exactly 2 — `SCENARIO
4 PASSED (all 4 assertions)`, exit code `0`. The two runs landing on
identical heights and lag values isn't a guarantee this scenario asserts
on (see the polling-based assertion design above, precisely because
timing isn't guaranteed) — it reflects that `auto-mine.sh`'s fixed 2s
interval and this environment's block-production latency were
reproducible enough, on this machine, to make both runs deterministic
down to the block. `docker ps` and a process check confirmed no
`sova-zebrad-regtest` container and no stray `bin/sova`/`auto-mine.sh`/
`sova-miner` processes left running after either run.

## TWO-NODE RELAY SCENARIO: gossip v1 live proof

Where scenario 4 proves one `bin/sova` node mints correctly, this
scenario (`box/sim/two-node-scenario.sh`) proves two *independent*
`bin/sova` processes end up with **identical state** having never
shared a process, a database, or any consensus code path in common at
runtime — only gossip v1's relay task
(`crates/engine/src/relay.rs`, `docs/design/gossip-v1.md`). This is the
first multi-node determinism evidence for Sova (C3/C6's eventual N≥3
case narrows to N=2 here; see "Extension plan" below for what's still
N=2-only).

**Setup**: node A runs in mine mode (`box/up.sh`'s own flow: miner
identity via `sova-miner init`, 101-block funding, `auto-mine.sh`, one
SIP-1 burn) with `SOVA_PEERS` pointing at node B's authrpc. Node B runs
`SOVA_FOLLOW_ONLY=1` on shifted ports (`SOVA_HTTP_PORT=8645`,
`SOVA_AUTH_PORT=8651`, `SOVA_P2P_PORT=30313`), sharing node A's
`SOVA_AUTH_JWT` file (generated once, up front, so both processes find
it already in place — no create-on-first-use race). Node B never seals
a block itself; its entire chain is built by validating what node A's
relay task pushes to its authrpc (`engine_newPayloadV4` +
`engine_forkchoiceUpdatedV3`), through the exact same stateful
validation path any Engine API caller goes through.

Three assertions:

- **(a) lockstep** — node B's `eth_blockNumber` tracks node A's,
  sampled 3× over ~10s while `auto-mine.sh` keeps the chain moving,
  allowing at most 1 block of lag.
- **(b) state-root equality** — `eth_getBlockByNumber(H, false).hash`
  is identical on A and B for 3 heights: block 1, the settled epoch's
  block (read straight from node A's own `sova epoch trigger
  height=... settled=true` log line), and the current tip. Block-hash
  equality is the actual proof here: the hash covers the full header,
  including `stateRoot`, so two nodes agreeing on a block's hash after
  independently validating it (A by building it, B by importing it
  through the relay) means their post-block state is identical, not
  merely that they received the same bytes.
- **(c) mint visible through the relay** — the miner's EVM address
  balance on node B equals node A's exactly (the withdrawal-channel
  mint, relayed and replayed on B identically to how A produced it).

Robustness: builds `bin/sova` (debug, same convention as
`mint-scenario.sh`) and `sova-miner` (release) only if missing;
generates a fresh shared JWT per run; kills neither node's own defaults
nor another agent's use of the shared harness at start (see "harness
courtesy" below) but tears its own two node processes, its own
auto-mine, and the regtest container down unconditionally on exit;
dumps both nodes' logs on failure.

**Harness courtesy**: unlike `mint-scenario.sh` (which kills any stray
process/container unconditionally at start), this scenario first polls
(bounded 15 minutes) for the shared `box/regtest` harness to be idle —
any `sova-zebrad-regtest` container or `sova`/`sova-miner`/
`auto-mine.sh` process already running is treated as another agent's
run in progress, not a stray to clear, and this script waits for it to
finish rather than killing it.

### A real finding along the way: `--engine.accept-execution-requests-hash`

The first cold run of this scenario failed all three assertions: node
B's `eth_blockNumber` never left 0, and node A's log showed the relay
actually running but every peer call failing:

```
WARN relay: peer failed; continuing with remaining peers height=148 peer="http://127.0.0.1:8651" \
  err=rpc error: engine_newPayloadV4: {"code":-38003,"data":{"err":"requests hash cannot be accepted \
  by the API without `--engine.accept-execution-requests-hash` flag"},"message":"Invalid payload attributes"}
```

`SovaEngineTypes::block_to_payload` (via
`ExecutionPayloadSidecar::from_block`) carries Prague's
`execution_requests` as `RequestsOrHash::Hash(requests_hash)` — the
right shape for a relay between two nodes that compute identically (see
`docs/design/gossip-v1.md`'s "Implementation notes"), but reth's
`engine_newPayloadV4` RPC handler rejects that shape unless the
receiving node opted in with `--engine.accept-execution-requests-hash`
at startup. Not discoverable from the type definitions alone — only by
running the real two-node call and reading the peer's rejection. Fixed
by setting `NodeConfig.engine.accept_execution_requests_hash = true`
unconditionally in `bin/sova` (harmless for a node that never receives
relayed calls). Not a Sova consensus bug — a missing node-config flag
this scenario's own first cold run caught immediately.

### Evidence: two green runs, from cold

Captured 2026-09-22, immediately after the fix above, `bin/sova`
rebuilt (debug) for this run, `docker compose down -v` between the two
runs (fresh `zfnd/zebra:6.3.0` regtest chain each time). Run 1 shown in
full; run 2 condensed (identical shape, different miner identity and
block hashes, as expected of two independent fresh regtest chains):

```
--- harness free (no zebrad container, no sova/sova-miner/auto-mine process) ---
--- starting a fresh regtest stack ---
zebrad RPC is healthy
--- shared JWT written to .../sova-two-node.YnmSp5/jwt.hex ---
--- sova-miner init ---
miner identity: tmFxdFq3WNtk2wjmBru35GKL14AujMCEC6R / 0x4483df2e12ec975daec1cef797cea33a37b69246
--- starting node B (follow-only) ---
node B up (pid 40530), http :8645, authrpc :8651
--- starting node A (mine mode, SOVA_PEERS -> node B) ---
node A up (pid 40611), http :8545, authrpc :8551
--- funding: 101 blocks to the miner's own address (coinbase maturity) ---
tip after funding: 101 (expected 101)
--- starting auto-mine (1 block every 2s) in the background ---

=== burn (node A's miner) ===
epoch 1: height=103 burn=100000zat fee=20000zat change=624880000zat txid=f9b2ee4979693be5682b27a7395bb9b4630393f0569025779c93aba15f6828ac
reached --max-epochs 1 -- stopping.
--- waiting for the burn's epoch to settle on node A (balance off zero) ---
node A settled balance: 6250000000000000000000 wei
settled height (node A): 103

=== (a) lockstep sampling (node B vs node A) ===
  sample 1/3: node_a=103 node_b=103 lag=0
PASS: two-node relay: (a) lockstep: sample 1 node_a=103 node_b=103 lag=0 (<=1 OK)
  sample 2/3: node_a=105 node_b=105 lag=0
PASS: two-node relay: (a) lockstep: sample 2 node_a=105 node_b=105 lag=0 (<=1 OK)
  sample 3/3: node_a=108 node_b=108 lag=0
PASS: two-node relay: (a) lockstep: sample 3 node_a=108 node_b=108 lag=0 (<=1 OK)

=== (b) block-hash (state-root) equality across 3 heights ===
--- waiting for node B to relay-catch-up to height 108 ---
PASS: two-node relay: (b) height 1: identical block hash on A and B (0x29438efc9b2eba36c35fc8a0b2a3e33105a91e6543c8793bfb40add0522c6409)
PASS: two-node relay: (b) height 103: identical block hash on A and B (0x1b482b0f9d4e6adbfd23131cf9df47290da28d538df26d497cba0ddf276e2629) -- the settled epoch's block
PASS: two-node relay: (b) height 108: identical block hash on A and B (0xf627e2ec559a035242859a30bb301f28cf1eaa4f5d2394af1d5283a0e135aecd)

=== (c) miner balance equality across the relay ===
PASS: two-node relay: (c) miner balance: node A == node B == 6250000000000000000000 wei (mint visible through relay)

TWO-NODE RELAY SCENARIO PASSED (all assertions)
--- stopping auto-mine (pid 40633) ---
--- stopping node A (pid 40611) ---
--- stopping node B (pid 40530) ---
--- tearing down regtest stack ---
```

Exit code `0`. Run 2 (fresh identity `tmBBAmqyxawbM6Sf7NNNpqvBtQmbZTn45cV` /
`0x100a8b3e5fd8013867e509466c67ec391f177395`): identical shape end to
end — burn at `height=103`, settled balance `6250000000000000000000`
wei, all three lockstep samples at `lag=0`, all three heights
(`1`, `103`, `108`) with matching-but-run-2-specific hashes on both
nodes, balance equality at the same figure —
`TWO-NODE RELAY SCENARIO PASSED (all assertions)`, exit code `0`.
`docker ps` and a process check confirmed no `sova-zebrad-regtest`
container and no stray `sova`/`auto-mine.sh`/`sova-miner` processes left
running after either run.

## Extension plan: multi-node state-root comparison (N≥3, waits on gossip)

The two-node scenario above closes gossip v1's own acceptance bar (the
first multi-node determinism proof) and the N=2 case of "run real
`bin/sova` nodes instead of `consensus_sim`." What's left is scaling
that same proof to N≥3 and to the fault/reorg scenarios gossip v1
explicitly punts on (see `docs/design/gossip-v1.md`'s "Honest v1
limits"):

- Run N ≥ 3 `bin/sova` mine-mode instances (N ≥ 3, matching C6's
  original acceptance criterion in `docs/WORKPLAN.md`: "3+ nodes end
  every scenario with identical state roots") against the same Zcash
  burns, and compare each node's **eth state root** at matching block
  heights (via `eth_getBlockByNumber`), the same way any two
  Ethereum-family clients are checked for consensus agreement — scenario
  4's single-node balance/log assertions generalize directly to
  "N nodes agree," they just need N nodes to compare.
- Scenario 1's shape (parallel determinism) becomes: N nodes sealing
  independently off the same Zcash burns and gossip, ending at identical
  state roots.
- Scenario 2's shape (reorg convergence) becomes: a forced Zcash reorg
  that crosses an epoch boundary a node has already sealed on top of —
  proving the sealer's own bounded micro-reorg handling (C3's "a late
  rank-1 block still wins" acceptance criterion) converges across
  independent nodes, not just that the follower's input stream changed.
- Scenario 3's shape (restart equivalence) becomes: a node crash-and-
  restart mid-sync, proving it rebuilds the identical chain tip from
  Engine API + follower state alone.
- Add sealer-fault scenarios once fault-tolerance behaviors land:
  rank-1 silent (fallback timeout), an equivocating sealer (tie-break),
  and the empty-epoch extension ladder — each run 3+ ways, each ending
  in identical state roots, per C6's acceptance criterion.
- ~~Wire this into CI to run nightly (per C6's AC), not just on demand~~
  — done: `.github/workflows/nightly-sim.yml` (see "CI coverage" above)
  runs `run-scenarios.sh` and `two-node-scenario.sh` nightly on
  `ubuntu-latest`, unmodified, exactly as anticipated here — the
  runner-lifecycle wrapper (start Docker, run, upload the log as an
  artifact, always teardown) sits entirely in the workflow file; neither
  script needed to change.

Scenario 4 already made that probe swap (`consensus_sim` → a real
`bin/sova` node) for the single-node case. The scenario structure itself
(fresh stack, mine + burn, reorg, restart, PASS/FAIL, always-teardown)
carried over unchanged in doing so — the only genuinely new work was N=1
→ N≥3 and the assertion swap (a balance/log check → `STREAM_DIGEST` →
state-root comparison), which is exactly what's left above.

## LADDER SCENARIO: v2 preference live proof (C3's acceptance test)

`box/sim/ladder-scenario.sh` (nightly CI). Two **mine-mode** nodes with
separate miner identities, mutual relay, shared regtest zebrad for C5,
`SOVA_RANK_STEP_SECS=3`. Structure:

1. **Warm-up** — a few auto-mined burn-less epochs with both nodes
   producing concurrently: their empty blocks must converge by hash
   tiebreak (both observed at rank `usize::MAX`; the arbiter adopts the
   lower hash on both sides). Asserted: every warm-up height identical
   on A and B.
2. **Combined epoch, rank 0 silent** — node A is SIGSTOPped; both
   miners' burns are steered into one generated Zcash block (auto-mine
   stopped; trigger block → both txs in mempool → one confirm block),
   so A (bigger burn) is rank 0 and B is rank 1. B seals after its
   1×step rung with its own rank-1 derivation. Asserted: B reaches the
   height, logs a settled trigger, and mints (tip included).
3. **Late win** — SIGCONT node A: it back-fills its queue in order and
   seals its rank-0 block for the same height; B's validator recovers
   rank 0 from the withdrawals, the tracker prefers it, and B's arbiter
   micro-reorgs. Asserted: B's hash at the height *changes* to A's
   block, and B's log shows the adoption.
4. **Convergence** — every height identical on A and B, both miners'
   EVM balances identical across nodes (and exactly conserving the
   6,250 SOVA epoch reward), lockstep resumes once auto-mine restarts.

First green run: 2026-09-22. Earlier red runs of this same script were
load-bearing: they caught the v1 relay's forced FCU overriding a
receiver's correct fork choice (relay is now newPayload-only) and the
un-addressed `PendingEpoch` mailbox draining a settlement into the
wrong height's block (now height-addressed) — see
`docs/design/gossip-v1.md`'s "v2 as built and proven".

## sova/1 P2P SCENARIOS (board m1-b step 3)

Two scenarios run the same flows as the relay ones with the transport
swapped to the `sova/1` RLPx sub-protocol (`SOVA_GOSSIP=p2p`,
`crates/engine/src/p2p/`, `docs/design/p2p-m1.md` "Propagation"): no
`SOVA_AUTH_JWT` anywhere (each node's authrpc keeps reth's own
per-datadir secret on 127.0.0.1), no `SOVA_PEERS`, and the only link
between the nodes is a devp2p session. B dials A's enode
(`SOVA_P2P_PEERS`), discovery stays off (dev profile). Blocks travel as
`Announce` → `GetBlock` → `Block` and are submitted to the receiver's
own engine in-process (`new_payload`). The receiver's arbiter decides
its head.

Both scenarios share `box/sim/p2p-common.sh`, and both are isolated from
the default box: zebrad on `:18272` in compose project/container
`sova-p2p-sim`/`sova-zebrad-p2p-sim`, nodes on 8745/8751/30411 (A) and
8845/8851/30412 (B). Every value can be overridden with `SOVA_P2P_SIM_*`.
Teardown touches only the node PIDs the script captured and its own
compose project. `SOVA_BIN`/`SOVA_MINER_BIN` choose the binaries, and
`SOVA_P2P_SIM_KEEP_LOGS=<dir>` keeps the node logs.

### `two-node-p2p-scenario.sh`: green

A is a mine-mode producer and B is follow-only and C5-enforcing. The
assertions are the relay scenario's (a)–(c) plus (0) and (d). (0)
checks that both nodes run sova/1, that authrpc is on loopback, that
the relay is off and that the session is up. (d) checks that B accepted
every block over sova/1, that B saw no authrpc engine traffic and that
no reputation hits happened. Run on 2026-09-22 from cold:

    PASS (a) sample 1/3: node_a=103 node_b=103 lag=0   (2/3: 104/104, 3/3: 107/107)
    PASS (b) height 1:   0x45e609e3…82da1e identical on A and B
    PASS (b) height 103: 0x2d73daf6…cc2819 identical -- the settled epoch
    PASS (b) height 104: 0x064f466e…3bf5ee identical
    PASS (b) height 107: 0x15558b78…0edeed identical
    PASS (c) miner balance: A == B == 6250000000000000000000 wei
    PASS (d) node B accepted 107 block(s) over sova/1; no authrpc traffic; no reputation hits
    TWO-NODE P2P SCENARIO PASSED (all assertions)

### `ladder-p2p-scenario.sh`: fails at (2). The cause is in the sealer, not the transport

This is `ladder-scenario.sh` with both nodes in p2p mine mode. The
setup, the warm-up (5 concurrent burn-less heights), the combined burn
epoch and (1) all pass: B, at rank 1, seals while A, at rank 0, is
SIGSTOPped. **(2) late win fails**, and it failed the same way in two
runs out of two. After SIGCONT, A's gossip service drains B's queued
announcements first. It pulls B's blocks 6 and 7 and imports them, and
A's arbiter adopts them. About 10 ms later A's sealer, which sampled its
head before the imports landed, fires triggers for epochs 208 and 209.
`SovaMiner` builds on the *current canonical head*, which is now B's
block 7. The rank-0 settlement is staged for height 7 and is never
drained. A therefore builds two empty extra blocks at heights 8 and 9
instead of its rank-0 block 7. Both chains converge: (3) passes and the
heights and balances are identical. But rank 1 wins, and the Sova
height ↔ Zcash epoch alignment slips by 2, so epochs 210 and 211 never
get blocks.

The relay ladder only avoids this because the relay's pushes to a
stopped peer time out and are lost. A resumes without B's blocks and
seals rank 0 first. Reliable delivery exposes three sealer and miner
behaviours, all in orchestrator-owned code (`crates/engine/src/driver.rs`
and `miner.rs`):

1. The covered-height skip drops an epoch even when the covering block
   is worse-ranked than ours.
2. The miner builds on the canonical head, not on the parent of the
   epoch's height.
3. A trigger does not carry its target height, so a trigger that has
   gone stale builds a ghost block.

## Discovery: `three-node-discovery-scenario.sh` (board m1-b step 4)

Three `bin/sova` nodes on the **public-testnet profile**
(`SOVA_CHAIN=sova-testnet`: empty alloc, chain ID 82330, Sova's own
genesis and fork ID), all `SOVA_GOSSIP=p2p`. Discovery is on by the
profile's default (`bin/sova/src/discovery.rs`): discv4 and discv5 share
the RLPx UDP port, the ENR fork ID is enforced, DNS discovery is off and
bootnodes come only from `SOVA_BOOTNODES`. No node has `SOVA_P2P_PEERS`.

- **B** is follow-only and C5-enforcing, started with no bootnodes. It is
  the bootnode.
- **A** is mine mode with `SOVA_BOOTNODES=<B>`.
- **C** is follow-only with `SOVA_BOOTNODES=<B>` and nothing else. It is
  never told A exists.

The testnet profile works unchanged under the regtest harness. Miners are
funded by Zcash burns, not by genesis alloc, so the dev profile isn't
needed. Everything binds to 127.0.0.1 (`SOVA_P2P_ADDR`) and advertises
127.0.0.1 (`SOVA_NAT=extip:127.0.0.1`, so there is no UPnP or public-IP
lookup). A background `lsof` sampler records every socket the three nodes
hold. Assertions:

- (0) Setup: sova/1 is registered in the network config and discovery is
  on as described above. C's only configured peer is its bootnode.
- (1) C gets sova/1 sessions with B **and A**.
- (2) and (3) B and C stay within one block of A, and the hashes match on
  A, B and C at four heights, including the settled epoch.
- (4) The minted balance is equal on all three nodes.
- (5) C accepted blocks over sova/1 directly from A.
- (6) Every sampled TCP/UDP socket is loopback-bound and loopback-connected.

Isolation: zebrad on `:18332` (project `sova-disc-sim`, container
`sova-zebrad-disc`). A, B and C run on 8945/8951/30511, 9045/9051/30512
and 9145/9151/30513, and each overrides as in `p2p-common.sh`
(`SOVA_P2P_SIM_C_PORTS` for C).

First run, 2026-09-22 (green, two runs out of two):

    C network up 21:24:32.01; B dials C at :35.08; C dials A at :37.01 (Outgoing)
    C's active sova/1 peers: A 0880e64a…, B 6c836dd3…
    PASS (2) A=103 B=103 C=103 (105/105/105, 108/108/108)
    PASS (3) height 1:   0x024c6236…71bb4c identical on A, B, C
    PASS (3) height 103: 0x1e19b726…c50518 identical -- the settled epoch
    PASS (3) height 104: 0x0653a365…6ab1a0 identical
    PASS (3) height 108: 0x20185240…a55a21d identical
    PASS (4) A == B == C == 6250000000000000000000 wei
    PASS (5) C accepted 108 blocks over sova/1, all 108 straight from A
    PASS (6) 594 socket samples, all loopback. One UDP socket per node
             (127.0.0.1:3051x): discv4 and discv5 share it
    THREE-NODE DISCOVERY SCENARIO PASSED (all assertions)

## Late join: `join-scenario.sh` (board m1-b, acceptance test for catch-up)

A fresh node with an **empty datadir** joins a chain that already has
history, and must catch up. This is the acceptance test for the arbiter's
late-join catch-up. At its default depth it is **expected to fail until
that catch-up lands**, and it is deliberately **not** in
`nightly-sim.yml` yet. The orchestrator wires it in when catch-up lands.

- **A** is mine mode, sova/1, dev profile, with no static peers. It mines
  alone. One burn-bearing epoch settles early (Sova height ≤ `EARLY_LIMIT`,
  default 20; the epoch base starts right after the 101-block funding, so
  in practice the burn settles at height 2). Then empty epochs follow until
  A's tip is `JOIN_DEPTH` blocks past the settled height. The Zcash chain
  is then frozen, so the gap J faces at its first sova/1 greeting is exact.
- **J** is follow-only and C5-enforcing against the same regtest zebrad.
  It starts with an empty datadir and has one sova/1 static peer, A.
  Discovery is off (dev), so A is the only node J can sync from.
  Auto-mine resumes once the J↔A session is up.

Assertions: (0) setup, including J's head being 0 at start. (a) J
reaches A's height within `JOIN_TIMEOUT_S` (180s), then lag ≤ 1 on 3
samples. (b) Hashes are identical at height 1, the settled height, the
middle and the tip. (c) The miner's balance is equal on A and J. (d) C5
was **enforced**, not deferred, on the historical settled block. (e) No
reputation hits on A or J. If (a) fails, the script prints a STALL
diagnosis from J's log.

```bash
JOIN_DEPTH=20  box/sim/join-scenario.sh   # control: inside sova/1's reach, passes today
JOIN_DEPTH=150 box/sim/join-scenario.sh   # the acceptance test (default): fails today
```

Isolation: zebrad on `:18342` (project `sova-join-sim`, container
`sova-zebrad-join`). A runs on 9245/9251/30611 and J on 9345/9351/30612
(J uses `p2p-common.sh`'s B slot, `SOVA_P2P_SIM_B_PORTS`). J's log is
`node-j.log`. On failure, `p2p-common.sh`'s teardown dumps the last 80
lines and the WARN/ERROR lines of every node log.

**Why dev and not sova-testnet.** Catch-up is a sova/1 plus
engine-tree/backfill property and doesn't depend on the profile.
Discovery on sova-testnet is covered by the three-node scenario. With
discovery off, a pass can't come from some other peer.

### Results on 2026-09-22

**JOIN_DEPTH=20 (gap 22): PASS, twice.** J caught up in 9s. Hashes
matched at 1, 2 (settled), 12 and 27, balances were equal and there were
no reputation hits. The path matters. A greets J with `Announce(22)`. J
fetches it and gets `SYNCING`. The payload validator has already
**observed it as a candidate**, so J's arbiter FCUs the unknown head 22
(`arbiter forkchoice update failed height=22 err=Syncing`, logged as a
WARN but harmless). That FCU makes reth's **engine-tree download**
(gap ≤ 32) fetch heights 1–8 over the eth protocol. None of those
heights ever arrives as a `new_payload`. Heights 9–22 come through
sova/1's ancestor chase. So the settled block (height 2) was imported
through the download path, where `SovaConsensus::validate_block_pre_execution`
is the only C5 check. (d) shows that check was not deferred for any height
≤ 22, while the same debug filter did log deferrals for the tip heights
25–27, which were imported ahead of J's 2s Zcash poll.

**JOIN_DEPTH=150 (gap 152): FAIL, as expected.** J stays at height 0 for
the full 180s while A grows from 152 to 233:

    FAIL (a) J did NOT catch up within 180s: A=233 J=0 (join gap 152, sova/1 chase window 33)
      'peer is beyond p2p catch-up range' lines: 82
      'ancestor gap deeper than p2p catch-up; left for backfill' lines: 0
      'parent unknown (SYNCING); fetching it' lines: 0
      'sova/1: peer block accepted' lines: 0; 'arbiter adopted' lines: 0
    INFO sova/1: peer is beyond p2p catch-up range; not fetching (backfill is a later step) announced=152 local=0
    ...
    INFO sova/1: peer is beyond p2p catch-up range; not fetching (backfill is a later step) announced=233 local=0
    FAIL (b) heights 1, 2, 77, 233: J=<none>;  FAIL (c) J=0;  FAIL (d) J never imported the settled block

The stall is **before** the 32-ancestor chase, not at its end.
`GossipService::on_announce` drops any announcement with
`height > local + MAX_ANCESTOR_DEPTH + 1` (33) without fetching it. This
covers the greeting (152) and every later tip announcement (153…233,
one per block). J therefore submits nothing, so it observes no
candidate, its arbiter never issues an FCU and reth never starts a
download or backfill. J's log has 0 `Received new payload`, 0
forkchoice lines and 0 C5 debug lines. "left for backfill" never
appears because the chase never starts.

What depth 20 suggests for the fix: the mechanism that closed most of
the gap there was *an arbiter FCU to a SYNCING tip*. For gaps > 32 the
same FCU would start reth's backfill pipeline. The missing pieces are
fetching (not dropping) the far-ahead tip announcement, and Decision 2's
gate: no FCU toward a target until J's follower has scanned through its
epoch.

### Observability gap (proposed log lines; orchestrator-owned code)

A **successful** C5 check logs nothing. Only deferral and trust are
logged, at debug. So (d) relies on negative evidence: J runs with
`RUST_LOG=info,engine::consensus=debug,engine::validator=debug`, and (d)
fails if any height ≤ the join tip was "deferred" or "accepted on
trust". Proposed:

1. `crates/engine/src/consensus.rs`, `check_settlements`, on
   `RankedVerdict::Valid { rank }`:
   `info!(height, rank, scanned, "c5: settlement enforced")`. Log it at
   info for burn-bearing heights and at debug for `ValidEmpty`. (d)
   already matches `ENFORCED_PATTERN` (default `settlement enforced`,
   with `height=<settled>` on the line). When the line exists, (d) uses
   it instead of the negative evidence.
2. `crates/engine/src/expectations.rs`, `run_expectations`: an info line
   when a poll advances the watermark,
   `expectations: scanned through sova height N (zcash E)`. Log it on the
   first catch-up and then rate-limited. This makes Decision 2's "scanned
   before FCU" ordering visible in the log.
3. The catch-up itself: one info line when the arbiter starts syncing
   toward a target and one when that target is gated on the follower,
   e.g. `catch-up: target H (hash), follower scanned S; waiting` and
   `catch-up: FCU to H (download|backfill)`.

## Client checkpoints: `checkpoint-scenario.sh` (nightly)

This scenario checks audit F2 measure B, client checkpoints
(`crates/engine/src/checkpoints.rs`, `docs/design/f2-join-and-restart.md`
§B), on the real multi-node join path over sova/1. Operators pin history
with `SOVA_CHECKPOINTS=height:0xhash,...`. The node must not import a block
at a checkpoint height unless it has the listed hash
(`SovaConsensus::validate_header`). It must not backfill toward a target
that contradicts a checkpoint (`candidates::actionable_target`). It must
refuse to start when its stored chain contradicts a checkpoint (the
startup check in `bin/sova`).

- **A** runs in mine mode (sova/1, dev profile, epoch base 1) and is the
  only producer. It mines empty epochs alone up to `GROW_TO` (70). Then
  Zcash is frozen, so both joiners face a 70-block gap. That is more than
  sova/1's 33-block chase window, so the joiners sync through the sync
  driver and reth's backfill pipeline.
- **J1** is follow-only and C5-enforcing. It uses a persistent
  `SOVA_DATADIR`, and `SOVA_CHECKPOINTS=40:<A's real hash at 40>`.
- **J2** is follow-only and C5-enforcing, with an empty datadir and
  `SOVA_CHECKPOINTS=40:0x5e5e…5e`, a hash no chain here produces. It runs
  with `RUST_LOG=info,downloaders::headers=trace` (see the finding below).
  J1 and J2 start together. Both have A as their only static peer.

Assertions:

- **(0)** All three nodes run sova/1. J1 and J2 enforce C5 and log
  `checkpoints: 1 (newest at height 40)`. The gap is larger than the chase
  window. The sessions are up.
- **(1)** J1 catches up (lag ≤ 1 on 3 samples). Hashes on A and J1 match at
  1, 20, 40, the middle and the tip. J1's block 40 is the checkpointed
  hash. J1 logs no mismatch. Once J1 follows live blocks it answers
  `eth_syncing=false`. That is the control for (2).
- **(2)** J2 is watched for `STALL_OBSERVE_S` (45 s) after J1 catches up.
  Its head, sampled every second, never reaches 40, and it has no block
  at 40. It logs `checkpoint mismatch at height 40`. `eth_syncing` never
  answers `false` while J2 is below 40, so it never falsely claims to be
  synced. J2 stays up, answers RPC and does not panic.
- **(c) control.** J2 is stopped with SIGTERM and restarted on the same
  ports with an empty datadir and no checkpoint. It syncs to A, and its
  block 40 is A's. So the stall in (2) comes from the checkpoint, not from
  the slot, the peer or the gap.
- **(3)** J1 is stopped with SIGTERM and restarted on its datadir with
  `SOVA_CHECKPOINTS=20:<wrong hash>`. It exits nonzero and logs `this
  datadir follows another history: block 20 is …`.
- **(3b) control.** J1 is restarted on the same datadir with the correct
  checkpoints for 40 and 20. It starts, still has A's block 20, and
  follows A again.

Isolation: zebrad runs on `:18372` (project `sova-checkpoint-sim`,
container `sova-zebrad-checkpoint`). A uses 10245/10251/30911, J1 uses
10345/10351/30912 (B slot) and J2 uses 10445/10451/30913 (C slot).
`p2p-common.sh`'s teardown now dumps every `node-*.log`, so J1's and J2's
restart logs are included. A run takes about 4 minutes. Local runner name:
`checkpoint-scenario`.

### Results on 2026-09-24: green twice in a row; fails when the check is removed

Runs 6 and 7 on the final script both passed all 33 assertions (`rc=0`).
J1 caught up 6 s after start. J2 stayed at head 0 while A reached 96–97,
and logged 156 `checkpoint mismatch at height 40` lines in about 65 s. J2
without the checkpoint synced in 4 s.

**Negative test.** `checkpoints::check` was temporarily changed to always
return `Ok`, then reverted. The scenario failed 3 assertions, all in (2):
J2 reached head 97 with A's block at 40 and logged no mismatch. (3)
still passed, as expected, because the startup check reads the installed
list directly and does not call `check`.

### Findings

- **The backfill stall is silent at the default log level.** On the
  pipeline path, reth's reverse headers downloader reports a
  `validate_header` failure only at **trace**
  (`downloaders::headers`, "Failed to validate header"). It then retries
  the same range about twice a second. At `info`, J2 shows `Preparing
  stage … Headers checkpoint=0` and a new `catching up to sync target` line
  for each higher announcement, but nothing about why. Proposed
  (orchestrator-owned): a warn line from Sova's own code the first time a
  checkpoint rejects a header, for example in `SovaConsensus::validate_header`,
  rate-limited per height.
- **`caught up to sync target` is not a usable "synced" signal.** It never
  appeared in any run, even for J1 and the J2 control, which did catch up.
  The driver logs it only if the head reaches the target inside the same
  loop turn as the FCU. So (2) checks `eth_syncing` instead. Right after a
  backfill, reth still answers "syncing". It flips to idle on the first
  live canonical head.
- **SIGTERM drops the in-memory tail.** `bin/sova` installs no signal
  handler, so SIGTERM ends the process at once. reth keeps up to
  `DEFAULT_PERSISTENCE_THRESHOLD` (50) canonical blocks in memory. J1's
  head was 99–100 before the stop and 70 after the restart: the backfilled
  range was on disk, the live tail was not. J1 re-fetched the tail within
  seconds, so this is harmless here. It does mean the startup checkpoint
  check only sees persisted blocks. Unpersisted blocks are re-imported
  through `validate_header`, so they are still checked.

## SIP-4 anchor precompile: `anchor-precompile-scenario.sh` (nightly CI)

Proves the SIP-4 `anchor()` precompile (`0x…5A00`, `crates/evm/src/zcash.rs`,
`docs/design/sip4-evm-seam.md`) gives the same answer on every node and
every import path. A 57-byte recorder contract (inline bytecode, listed in
the script header) STATICCALLs `anchor()` and SSTOREs the result at slots
`2N` (`E_N`) and `2N+1` (hash), so a node whose precompile answered
differently would compute a different state root.

- **A** is mine mode, sova/1, dev profile. The epoch base is set after 10
  pre-mined Zcash blocks (`B = 11`), so `E_N = N + B − 1` is not the
  identity.
- **B** is follow-only, C5-enforcing, with sova/1 static peer A.
- **J** is a late joiner. It is follow-only, starts from an empty datadir,
  and has static peer A. It starts after the recorder blocks exist, with
  `JOIN_DEPTH` (default 5) more blocks after them. The gap stays inside
  sova/1's 33-block chase.

All three nodes share one regtest zebrad, as in `two-node-p2p-scenario.sh`.
Each node answers from its **own** follower's index. Transactions are
signed with reth's public dev key through `cast send`, so foundry must be
on PATH. The workflow installs it with `foundry-rs/foundry-toolchain`.
The scenario never burns, so it needs no `sova-miner`.

Assertions:

- (1) At least 5 recorder transactions land in consecutive blocks, and
  every receipt has status 1.
- (a) For every recorder block, the block hash and `stateRoot` match on
  A, B and J, and so does the receipt.
- (b) At every recorder block N, on all three nodes, `storage[2N]` equals
  `N+B−1`. `storage[2N+1]` equals that block's `parentBeaconBlockRoot`,
  which equals zebrad's `getblockhash(N+B−1)`. In the control block (the
  one before the first recorder block), slot `2N` is 0.
- (c) `eth_call` of `anchor()` at `0x5A00`, run at each historical recorder
  block, returns `(N+B−1, root_N)` on all three nodes.
- (d) J catches up. A, B and J have the same hash and `stateRoot` at a
  common tip, and no node logs a reputation hit.

Isolation: zebrad on `:18362` (project `sova-anchor-sim`, container
`sova-zebrad-anchor`), A 9745/9751/30711, B 9845/9851/30712, J
9945/9951/30713 (J uses `p2p-common.sh`'s C slot, log `node-j.log`).

### Results on 2026-09-23: green, two runs from cold

    PASS (1) 5 recorder tx(s); 5 in consecutive blocks (blocks 4 5 6 7 8)
    PASS (d) J reached the join tip 13 in 9s
    PASS (a) blocks 4..8: hash/stateRoot identical on A, B, J (block 4: 0x557365c4…cff16d / 0xece86919…eecbc)
    PASS (b) node A/B/J block 4: stored E_N=14, hash == parentBeaconBlockRoot == zebrad getblockhash(14) (0x27d2964c…f2dbee)
    PASS (b) control: no anchor stored for block 3
    PASS (c) node A/B/J eth_call@4..8: anchor() == (14..18, root_N)
    PASS (d) common tip 19: hash/stateRoot identical on A, B, J
    diagnostics: node B refused precompile calls: 4 ('engine submit failed': 2), A 0, J 0
                 node A: eth_call anchor() at "pending": ERR ... zcash index has no block at anchored height 30 (sova block 20)
    ANCHOR PRECOMPILE SCENARIO PASSED

What the diagnostics show:

- **B refuses blocks and then retries them, as SIP-4 §5 describes.** In
  both runs, some recorder blocks reached B before B's own follower had
  indexed their epoch. For example:
  `fatal: sova zcash precompile: zcash index has no block at anchored height 14 (sova block 4)`.
  The block was refused on the sova/1 `new_payload` path (`engine submit
  failed`) and on the engine tree's downloaded-block path. It was never
  marked invalid. B imported it on the next announcement, one block
  later, with the same state root. The refusal happens *at execution*
  because C5's `check_settlements` *defers* (returns `Ok`) when
  `height > scanned`. The seam doc's still-needed item 4, a transient
  `AnchorUnscanned` hold before execution, would stop these blocks
  before they run.
- **J never refuses a block.** It fetches its whole history through
  sova/1's ancestor chase after its follower has scanned everything, so
  every buffered block executes against a full index.
- **`anchor()` at `pending` is a fatal RPC error.** `N = latest + 1`, and
  that Zcash block does not exist yet. So `eth_call` and `eth_estimateGas`
  at `pending` fail for any transaction that reaches the precompile. The
  scenario passes an explicit gas limit so it never estimates.

## SIP-4 §7 Zcash reorg: `zcash-reorg-scenario.sh` (nightly)

This is the acceptance test for SIP-4 §7 (`sips/sip-4-draft-zcash-state-precompile.md`
§7, and §11 box scenario 2). A Zcash reorg to height `R` must invalidate
every Sova block with `E_N > R` on every node. Sova unwinds to
`N_R = R − B + 1`, re-seals on the new branch, and contract state that
depended on a reorged-out Zcash transaction rolls back the same way on
every node. **It passes since the §7 rollback landed** (37/37 locally):
after a Zcash rollback to `R`, `ExpectedSettlements::effective_head`
reports `N_R` while the canonical block at `N_R + 1` is anchored to an
orphaned Zcash block, so the sealer re-seals `N_R + 1` on its canonical
parent and the arbiter adopts it below the stale tip; reth then reorgs the
stale tail out. It gates the nightly like every other scenario.
Assertion (r) checks that no node seals on the stale tip while Zcash has no
replacement branch (an artificial state: on a live network a reorg *is* a
longer branch arriving); the head itself moves back only once a
replacement block exists, so RPC can serve the stale tip for that gap.
Before the rollback landed, this scenario failed 11 assertions (heads stuck
at the old tip, orphaned mint kept).

- **A** runs in mine mode (sova/1, dev profile) with a `sova-miner init`
  identity, so A is rank 0 for its own burn. **B** is follow-only and
  C5-enforcing, with sova/1 static peer A. Both share one regtest zebrad.
- A 75-byte **recorder** (inline bytecode, listed in the script header)
  takes a Zcash txid as calldata, STATICCALLs `txInfo(txid)` at `0x…5A00`,
  and stores `status+1`, `height` and `confirmations` at slots `3N..3N+2`.
  Block N's state root therefore depends on what the node's index says
  about T as of `E_N`.
- **T** is a real SIP-1 burn (`sova-miner mine --max-epochs 1`) mined at
  Zcash `h`. It mints at `N_burn = h − B + 1`. The epoch base is
  `B = 102`, set after the 101 funding blocks.
- **Reorg.** Zcash is frozen and zebrad runs `invalidateblock(h)`, so
  `R = h − 1`. A replacement branch without T is mined, 2 blocks longer
  than the old one, and auto-mine resumes. T is then re-broadcast and
  re-mined at `h'` (set `REINCLUDE=0` to skip this). Recorder calls land
  before the reorg, on the new branch before `h'`, and after `h'`.

Assertions:

- **(P) control, before the reorg.** A and B match on every block, every
  anchor equals zebrad's hash, the recorder saw T at `h` with
  `E_N − h + 1` confirmations, and the burn minted only at `N_burn`. The
  miner's balance equals its withdrawals. Transactions pay zero priority
  fee, and the miner is also the fee recipient, so fees can't blur the
  mint check.
- **(r)** Within 30 s of `invalidateblock`, before any replacement block
  exists, both heads drop to exactly `N_R`.
- **(a)** At the end, every canonical block `1..head` on both nodes has
  `parentBeaconBlockRoot == getblockhash(N+B−1)` on the new branch. Blocks
  `N_R+1..H_old` carry new hashes, and blocks `1..N_R` keep their old ones.
- **(b)** A and B have identical hash and `stateRoot` at every height
  from `N_R` to the head, and the same head.
- **(c)** Every canonical recorder observation above `N_R` matches the
  new branch's derivation: `NOT_FOUND` while `E_N < h'`, otherwise
  `(h', E_N − h' + 1)`. A and B agree. At least one `NOT_FOUND`
  observation and one found-at-`h'` observation exist.
- **(d)** The orphaned mint is gone. The only canonical withdrawal to the
  miner is at `h' − B + 1`. The balance matches on A and B, equals the
  canonical withdrawals, and equals one epoch reward.
- **(e)** After the reorg both heads keep advancing, B stays within 1 of
  A, and no node logs a reputation hit.

Isolation: zebrad on `:18392` (project `sova-reorg-sim`, container
`sova-zebrad-reorg`), A 9445/9451/30811, B 9645/9651/30812. The script
copies zebrad's log into the work dir as `zebrad.log`, so
`SOVA_P2P_SIM_KEEP_LOGS` keeps it. Foundry's `cast` must be on PATH. A
run takes about 5 minutes.

### A harness finding: zebrad re-submits T on the new branch

The first local runs sometimes had the "T-less" replacement branch
re-mine T at the same height `h`. That makes the reorg invisible to the
recorder and to the mint. The cause is not the mempool reset: after
`invalidateblock`, zebrad logs `re-verifying mempool transactions after a
chain fork transactions=0`. The cause is zebrad's **`sendrawtransaction`
retry queue** (`zebra-rpc/src/queue.rs`). Every transaction sent over RPC
waits there until a queue pass sees it in the mempool or the chain. A
pass runs once per target block spacing, which is **75 s on regtest**,
and only if the tip height moved since the previous pass. If no pass ran
between T's broadcast and the invalidation, the next pass re-submits T
onto the new branch. It happened in 2 of the first 5 runs. The scenario
now waits `RPC_QUEUE_WAIT_S` (90 s) after the burn before freezing, and
mines one block every 15 s meanwhile so the tip moves and the reorg stays
shallow. It also watches the mempool from the invalidation to the
replacement and aborts the setup if T reappears. Any future reorg
scenario that invalidates a block holding an RPC-submitted transaction
needs the same wait.

### Results on 2026-09-23 (`z1/sip4-v1` + this scenario): fails as expected

Three runs in a row (276 to 305 s each) failed identically, with 11
failed assertions each. The control (P) passed every time. A typical run: `B=102`, `h=106`,
`R=105`, `N_R=4`, old Zcash tip 116, old Sova head 15, T re-mined at
`h'=123`.

    PASS (P) A and B identical 1..15; anchors == zebrad; recorder saw T at h=106; mint only at 5
    FAIL (r) node A: head 15 after 30s (lowest seen 15), want N_R=4: blocks 5..15 still anchor orphaned Zcash blocks
    FAIL (r) node B: head 15 after 30s (lowest seen 15), want N_R=4
    FAIL (a) node A/B: 11 canonical block(s) anchored to ORPHANED Zcash blocks: heights 5..15
    FAIL (a) blocks still carrying their PRE-reorg hash above N_R=4: 5..15
    PASS (b) block hash and stateRoot identical on A and B at every height 4..head
    FAIL (c) block 6..9: stored A='1 106 2..5' B='1 106 2..5', want '2 0 0' (T NOT_FOUND on the new branch)
    PASS (c) blocks 19..21: T NOT_FOUND; blocks after h': T at 123 with new confirmations
    FAIL (d) canonical miner withdrawals at '5 22', want '22' (the orphaned burn's mint stayed)
    FAIL (d) balance 12500000000000000000000 wei == 2.0 epoch rewards, want 1
    PASS (e) liveness (B within 1 of A); no reputation hits

What happens on the current code:

- **Nobody unwinds, and nobody stalls.** Both nodes log `expectations and
  candidates unwound (zcash reorg) to_height=R`. A's sealer also logs
  `zcash reorg observed (v0: log-only)`. Both heads stay at the old head
  for the whole 30 s window (`lowest seen` never drops). No block is ever
  held for an anchor mismatch (`'anchor mismatch': 0`). B's only holds are
  the ordinary tip `zcash epoch not scanned yet` holds, which clear within
  a poll.
- **The chain grows on top of the orphaned blocks.** The sealer skips new
  epochs whose expected height is at or below the stale head. Once the
  new branch passes the old Zcash tip, it seals `H_old+1` on top of the
  stale block `H_old`, anchored to the new branch. `check_settlements`
  validates only the new block's own anchor. It never checks that the
  parent chain is anchored to the same Zcash chain, and blocks that are
  already canonical are never re-checked after the expectations are
  unwound. B imports the same blocks, so A and B agree with each other,
  and **(b) passes**, but both disagree with their own zebrad for every
  block in `N_R+1..H_old`.
- **The precompile itself is correct after the reorg.** New blocks see T
  as `NOT_FOUND`, then at `h'` with the right confirmations, because the
  index is unwound and rebuilt from the new branch. Only the stale blocks
  keep their answers. So the missing piece is the head rollback (FCU to
  `N_R`, then re-seal), and B needs the equivalent move when it holds
  canonical blocks above `N_R`. Not run here, but it follows from
  `check_settlements`: a late joiner would hold the stale blocks on
  `AnchorMismatch`, which is transient, so it would stall rather than
  fork. This scenario doesn't cover that case.
