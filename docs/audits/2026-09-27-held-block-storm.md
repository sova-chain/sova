# Held-block storm (G3) and the follower stall beside it (G3b)

2026-09-27. Found in the reorg-stress sim (seed 902411869, 45 min) on the
`g1g2-fixes` build: follower B failed the lag invariant (head 520 while A and
C were at 543, for 180 s). The same storm shows on the `release` build of the
same seed, which passed. Unit tests only here; the sims are re-run separately.

Line numbers below are for `release` at `4f82caf`.

## What the logs show

- B (`node-b-1.log`): block 482 `0x1500cbe8…`, sealed against the Zcash block
  that won a 0.93 s flip-flop at 16:23:37 (zcash 602, branch B
  `804436ba…`), was held ("sova-hold: zcash anchor mismatch at height 482")
  **1,087 times** from 16:23:37 to 16:28:37, then "held block expired;
  forgotten": exactly `MAX_HOLD` (300 s). The same run held a block at 306
  1,394 times (16:08:57–16:13:57). The `release` run: 1,410–1,476 times at
  each of 223, 368, 376 and 455. The `g1g2-902b` run: 1,441 at 306.
- Each attempt spent 231 ms on average (max 607 ms) between "Received new
  payload" and "Invalid block error on new payload". Summed over the storm:
  **250.8 s of the 300 s window** B's engine spent on this one block, and so
  did B's sova/1 task, which awaits each call.
- The lag: B imported every block 521..543 ("peer block accepted", "Block
  added to canonical chain"), but the arbiter's last adoption was 520 at
  16:27:14. Its head stayed at 520 to the end of the run (16:29:22); it did
  not catch up after the expiry at 16:28:37 in this run.
- The storms at 306 (this run) and in `g1g2-902b` came with **no lag at all**
  (`checks.log`: B equal to A within a block throughout).

## (1) Who re-sends the held block

B itself. No peer re-announces it and B does not re-fetch it: the hash stays
in `seen` (`on_block`, `service.rs:432`) and in `held`, and `on_announce`
returns early for a seen hash (`service.rs:342`). (The `peer=` in the "block
held" line is the peer it was first fetched from, stored in `Held.peer`.)

The resubmission is `retry_held` (`service.rs:591-629`), driven by
`held_tick` every `HELD_POLL` = 200 ms (`service.rs:228-229, 243`). A held
block is due when `scanned.is_some_and(|s| s >= h.block.number)`
(`service.rs:598`). That was written for the *unscanned* hold, where it is
true only once our scan reaches the block. For an **anchor-mismatch** hold
the height is already scanned — the mismatch is about which Zcash block —
so the condition is true on every tick for the whole `MAX_HOLD`. Each engine
call takes ~0.2–0.4 s (the parent sits ~40 blocks under the tip; see
"Changeset cache MISS … falling back to aggregate DB-based computation"), so
the missed ticks fire back to back (`MissedTickBehavior::Delay`): one
attempt every ~0.4 s.

## (2) Why B's head stalled

Not because of the storm. Neither of the proposed mechanisms happened:
blocks above the held height were fetched and imported throughout, and
`Held` blocks never gate other heights. What stopped B is a race between a
peer's re-announcement and B's own Zcash rollback:

1. Zcash flip-flopped again at 16:27:04–16:27:07. A saw the rollback first:
   its reset (`on_rollback_reset`, `service.rs:259-269`) re-announced its
   chain up to 521 at 16:27:06.459 (`node-a-6.log`).
2. B fetched and accepted A's 521 at 16:27:07.008 (while B's head was ~513, so
   521 was observed but not yet attached — its parent 520 not canonical).
3. B's own follower applied the rollback at 16:27:07.687 ("unwound
   to_height=640", i.e. Sova 520): `candidates::global().unwind_above(520)`
   (`expectations.rs:439-449` → `candidates.rs:434-442`) dropped every
   candidate above 520 — including 521.
4. B's reset cleared `seen`, but nobody announces 521 again: A's reset had
   already happened, and A only announces new heads after it. The engine has
   521, so nothing re-imports or re-observes it.
5. The arbiter adopted 520 at 16:27:14 and looked for the next candidate
   (`candidates.rs:719-729`): none at 521. Every later block 522..543 was
   observed but never attached: `branch` needs each non-canonical ancestor in
   the tracker (`candidates.rs:508-509`) and 521 was gone. So no `NewBest`,
   no forkchoice update, head 520 until the run ended.

The same shape explains B's earlier minute at 500 (16:25:07–16:26:00, blocks
501..509 imported, none adopted): it ended only because the next rollback
made A re-announce, and B re-fetched the fork blocks it still lacked as
candidates.

The storm matters to this only as latency: B's engine and sova/1 task were
~84 % busy on 482, which delays every fetch and forkchoice update by up to a
few hundred ms and widens windows like (2)–(3). It did not cause this stall
and caused no lag in three other storms.

## (3) Is re-validating the same hash needed?

No. A hold's verdict (`SovaConsensus::check_settlements`,
`consensus.rs:153-208`, and the SIP-6 parent check, `consensus.rs:254-282`)
reads only our follower's records and scan watermark
(`ExpectedSettlements::check_anchor`, `record`, `scanned_through`). A
scanned height's record changes only after `unwind_above` (a Zcash
rollback), which also bumps `candidates::rollback_generation()`. So while the
pair (rollback generation, "scan covers the height") is unchanged,
re-validating gives the same answer. The G2 fix (child of a stale parent,
on `g1g2-fixes`) also reads only records, so it keeps this property.

## Fix (p2p layer only)

No change to consensus, the sealer, the arbiter or candidate ranking, or
`expectations.rs`.

**G3** (`crates/engine/src/p2p/service.rs`):
- A parked block remembers why (`HoldKind`) and our follower's view of its
  height when it was tried (`FollowerMark`: rollback generation + "scan
  covers it"), read *before* the engine call so a change during the round
  trip triggers one more try.
- `retry_held` resubmits a consensus hold only when its `FollowerMark`
  changed: a Zcash rollback, or the scan reaching it. An engine failure (the
  `Err` path, e.g. the SIP-4 index lagging) keeps the timed `HOLD_RETRY`.
- `on_announce` ignores a hash that is held (a rollback reset clears `seen`
  but not `held`, so re-offers used to re-fetch and re-validate it too).
- Expiry is unchanged: dropped and forgotten after `MAX_HOLD`.
- The follower view is read through two new `GossipBackend` methods with
  defaults over the globals, so tests control it.

**G3b** (same file): on a Zcash rollback, resubmit the recently accepted
peer blocks we don't hold canonically, at or above `head − MAX_REPLACE_DEPTH`,
lowest first. The engine answers each from its tree (`AlreadySeen`; reth
runs `convert_payload_to_block` first, `tree/mod.rs:3155-3158`), and the
validator re-observes it on the way, so a candidate dropped by our own
rollback comes back even when no peer re-announces it. A block ahead of the
re-scan waits as a consensus hold until the scan reaches it.

## Tests (`p2p::service::tests`)

- `a_held_block_is_validated_once_per_follower_change`
- `a_held_block_does_not_hold_up_newer_blocks`
- `a_zcash_rollback_re_enables_resubmission`
- `an_engine_failure_is_retried_on_a_timer`
- `a_rollback_resubmits_blocks_whose_candidates_it_dropped` (G3b)
- `a_hold_costs_no_reputation_and_can_be_retried` now retries on a rollback,
  not on a timer.

## Open

- If some future hold reason depends on something other than our follower's
  records (none does today), such a block now waits for the next rollback
  or expiry instead of being retried every tick.
- G3b restores candidates B saw before its rollback. A block B never
  fetched before its rollback, and that no peer re-announces after it, still
  relies on the next announcement. The sims are the check.
