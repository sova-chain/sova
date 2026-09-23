# Gossip v1: propagation without new protocols

Status: **implemented**. No-double-production (below) and the relay
task + node modes below are both merged; the two-node live proof
(`box/sim/two-node-scenario.sh`) is green from cold, twice — see
`box/sim/README.md`'s "TWO-NODE RELAY SCENARIO" section for the run
evidence. Implements toward C3 multi-node; C6's state-root comparison
consumes this.

## The insight

A sealed Sova block already has a canonical wire form: the Engine API
execution payload. Instead of inventing a gossip protocol, v1 relays
blocks peer-to-peer over the Engine API itself:

- Each node runs a **relay task**: watch own chain head; on a new block,
  fetch it, convert (`block_to_payload`), and push to each configured
  static peer's authrpc as `engine_newPayloadV4` followed by
  `engine_forkchoiceUpdated` to that head. Shared JWT inside the box.
- The receiving node validates the payload through its full stateful
  engine path — the same validation a real network would perform. No
  listener, no new port, no libp2p (that arrives only when peer discovery
  matters, post-M1).

## No-double-production (this commit)

With 1 Sova block per epoch from `base_height`, the expected Sova height
for epoch E is `E - base + 1`. Before firing a production trigger for E,
the sealer checks its own chain head: if `head >= expected`, a block for
this epoch already landed (ours or a peer's) — skip the trigger. This is
suppression only; it makes two v1 nodes converge on first-arrival.

## Implementation notes (where this note met reality)

The relay task (`crates/engine/src/relay.rs`) and node modes
(`bin/sova/src/main.rs`) implement the shape above exactly, with a few
details reality forced that this note didn't spell out:

- **`engine_forkchoiceUpdated` is V3, not version-generic** — the relay
  always calls `engine_forkchoiceUpdatedV3`, matching `newPayloadV4`'s
  own Cancun/Prague pairing (`fork_choice_state`, `payload_attributes:
  null`). head/safe/finalized are all set to the relayed block's own
  hash (`ForkchoiceState::same_hash`) — v1 has no finality gadget yet,
  so there's no other hash to offer.
- **`execution_requests` travels as a hash, not a list, and the
  receiving node must opt in.** `ExecutionPayloadSidecar::from_block`
  (which `SovaEngineTypes::block_to_payload` uses) recovers Prague's
  requests as `RequestsOrHash::Hash(requests_hash)` from the sealed
  block's header, not the original request bytes — the right shape
  here, since the receiving node's own validator re-derives and
  compares the hash rather than needing the (always-empty, for Sova
  today) request list itself. reth's `engine_newPayloadV4` handler
  rejects that shape over RPC unless the node was started with
  `--engine.accept-execution-requests-hash`
  (`NodeConfig.engine.accept_execution_requests_hash`) — found only by
  running the two-node scenario cold and reading the peer's rejection
  (`-38003 Invalid payload attributes`), not from the type definitions.
  `bin/sova` now sets this unconditionally for every node/mode; it's a
  no-op for a node that never receives relayed calls.
- **A follow-only node must skip `NodeConfig::dev()` entirely, not just
  leave `dev.block_time` unset.** reth's own node launcher
  (`DebugNodeLauncher`) spawns *its own* `LocalMiner` in instant mode
  whenever `dev.dev` is true and `dev.block_time` is `None` — exactly
  mine mode's own trigger-mode setup, just pointed at a different,
  disconnected `PendingEpoch` mailbox. A follow-only node therefore
  never calls `.dev()` (it sets `network.discovery.disable_discovery`
  by hand instead, which `.dev()` would otherwise have done as a side
  effect) — the design note's "no sealer, no miner" only holds with
  this distinction made explicit.
- **Two `bin/sova` processes on one host need the IPC server off, not
  just shifted HTTP/authrpc/p2p ports.** reth's default IPC endpoint
  (`/tmp/reth.ipc`) is a single fixed path, not per-datadir, so a second
  node would otherwise race the first to bind it. `bin/sova` disables
  IPC (`rpc.ipcdisable = true`) unconditionally — nothing in this
  codebase uses it regardless.

## Honest v1 limits (v2 scope)

- **First-arrival is not rank preference.** v1 nodes accept whichever
  valid block lands first; the rank-then-hash rule (`sealer::prefer`)
  needs candidate metadata alongside payloads and a bounded intra-epoch
  reorg to the better candidate. v2 carries `(sealer_rank, epoch)` with
  the relay and applies `prefer` before FCU.
- **Settlement validation on import (C5).** The importing node must
  re-derive the epoch's expected withdrawals from its own zebrad and
  reject mismatches; v1 relies on both nodes computing identically (which
  C6 scenario 1-3 already proves at the follower layer). C5 makes it
  enforced, not assumed.
- Static peers only; JWT shared; box-scale trust.

## v2 design addendum: receiver-side preference (written 2026-09-22, pre-implementation)

The v1 push model lets the sender dictate head via FCU. v2 moves
arbitration to the receiver: it tracks candidate blocks per epoch and
issues its own FCU to the `prefer`-winning candidate (rank asc, hash
asc), reorging within the epoch when a better candidate arrives late.

Key insight — **no metadata channel**: a candidate's sealer rank is
recoverable from the block itself. The sealer tip means each possible
sealer produces a distinct withdrawals derivation; the receiver, holding
the epoch's ranked miners, matches the block's withdrawals against
`derive(sealer = ranked[r])` for each r — the unique match identifies
the sealer and hence the rank (`identify_sealer`, implemented with
tests). Burn-less epochs carry no settlements; all their candidates are
rewardless and tie-break by hash alone. Implication: relayed payloads
stay pure Engine API objects; v2 adds only receiver logic.

Remaining v2 work: the candidate-tracker + FCU arbiter task on the
receiver (bounded intra-epoch reorg), and relay-side restraint (send
newPayload always; send FCU only when locally best). After that, the
ladder in `sealer::produce_decision` gets its `best_seen` wired from the
tracker instead of `None`.

## v2 as built and proven (2026-09-22, ladder scenario green)

All of the above landed, with three corrections the live scenario forced
on the pre-implementation sketch:

- **The relay sends no FCU at all** — stronger than the sketched
  "FCU only when locally best". Any relayed FCU is a trust channel: a
  node resuming from a stall relays its (worse) backlog head and yanks a
  peer off a *preferred* lineage, which we watched happen. The rule is
  now **relay delivers, arbiter decides**: `engine_newPayloadV4` only;
  the receiver's validator observes every accepted block as a candidate
  (`crates/engine/src/candidates.rs`), and its FCU arbiter is the only
  thing that moves the head. Unknown-verdict imports (heights our
  follower hasn't scanned) observe at trust rank `usize::MAX`, so a
  lagging receiver still advances and a properly-ranked candidate
  displaces the trust winner once the scan catches up. Burn-less
  epochs' empty blocks also compete at `usize::MAX` — hash tiebreak
  converges concurrent producers.
- **Nothing may finalize the tip.** `ForkchoiceState::same_hash` marks
  the head finalized, and reth treats finalized as irreversible — one
  such FCU anywhere and the micro-reorg becomes impossible. Producers
  assert depth-lagged finality (64 blocks, 32 for safe) via the vendored
  `SovaMiner` (stock `LocalMiner` also re-asserts its *own* built
  lineage from an internal ledger every second, fighting the arbiter —
  `SovaMiner` reads the canonical head from the provider instead, so an
  arbiter adoption becomes the next build's parent).
- **Production is in-order and the settlement mailbox is
  height-addressed.** Epoch E's block sits at height `E − base + 1`, so
  the sealer queue only produces at the front, and a staged settlement
  names the height it belongs to — a cadence build can no longer drain
  a later epoch's settlement into the wrong block (our own C5 rejected
  exactly that and the epoch was lost).

Proof: `box/sim/ladder-scenario.sh` (nightly CI) — combined two-miner
epoch with rank 0 SIGSTOPped: rank 1 seals after its rung (liveness),
the resumed rank 0's block wins by arbiter micro-reorg (preference),
every height and balance converges exactly, lockstep resumes.
