# F2 before SIP-8: restart, checkpoints, and a cumulative-rank fork choice

Status: **design, pre-implementation** (2026-09-23). Scope: the interim
measures for audit finding F2 (`docs/audits/2026-09-23-reorg-and-fork-choice.md`)
until SIP-8 (`sips/sip-8-draft-anchored-burns.md`) is live. Code
references are to `engine/sibling-rule` at `ffbcc66` unless marked
`release`.

## Where F2 stands after the sibling rule

`engine/sibling-rule` fixed F1, F3 and F4. A candidate now counts only if
it is attached to the node's chain (`candidates.rs`, `attached()`: its
parent is our canonical block at `h − 1`, or an attached candidate above
the head). The sync driver keeps a target set and acts on the highest
target our scan covers (`actionable_target`). What F2 still leaves open:

- **Restart.** `CandidateTracker::seen` lives in memory. After a restart
  it is empty, so the first attached candidate at the tip height is
  `NewBest` at any rank (`observe`), and `run_arbiter` FCUs to it. The
  sibling rule shrinks this from "any history" to "a sibling of our tip",
  but a restarted node can still be moved from a rank-0 tip to a rank-5
  sibling by the first peer that offers one. The audit test
  `after_restart_any_rank_is_adopted_first` still holds. The sealer side
  is already guarded: `driver.rs` treats "no candidate known" as covered
  (`release`, `driver.rs:303-312`).
- **Join.** `on_announce` hands any tip more than 33 blocks ahead to
  `request_sync` (`p2p/service.rs:314-335`). `remember_target` keys
  targets by height, so the driver follows the latest announcement at the
  highest height the scan covers. That is arrival order.
- **Eclipse.** A node whose peers are all the attacker's follows the
  attacker's history and has no way to notice.
- **Partition.** New with the sibling rule, and worth stating: two nodes
  whose canonical chains differ below the tip never reconverge. Each
  ignores the other's blocks as unattached, and neither chain gets 33
  blocks ahead of the other, since both advance one block per epoch. An
  operator has to resolve it by hand.

Three measures follow. Each one closes something and leaves something
open. SIP-8 is the measure that removes the first-seen input.

## A. Canonical-block comparison on restart

**Idea.** A restarted node does not start from an empty slot at each
height. It starts from its own canonical block, whose rank it can
recompute from the withdrawals (`driver::identify_sealer`, or
`expectations::check_ranked`, the function `validator.rs` already uses
when it observes). A same-height candidate must then beat the canonical
block by SIP-2 preference, just as it would have before the restart.
Separately, the node watches for peers that announce blocks at heights
it already holds that are not ours, and reports it.

**Closes.** The restart case of F2: a restarted node keeps a rank-0 tip
against a worse sibling, as it would have if it had not restarted. It
also gives operators a signal when their history diverges from their
peers'. Today a divergence below the tip is silent.

**Leaves open.** Join (a fresh node has no canonical chain to compare
against). Eclipse. A node that was already on the wrong branch when it
stopped stays there, because the sibling rule keeps it there. Partition
splits. The divergence signal is advisory: switching to whatever most
peers announce would make peer count (Sybil-cheap) the fork choice.

**Sketch.**

- `crates/engine/src/candidates.rs`: a second installed reader next to
  `set_canonical_reader`, `set_canonical_block_reader(Box<dyn Fn(u64) ->
  Option<CanonicalBlock>>)`, where `CanonicalBlock` holds `hash`,
  `parent` and `withdrawals`. In `observe` and `observe_unranked`, when
  the height is at or below the head and `seen` has no entry for the
  canonical hash at that height, insert the canonical block first. It is
  ranked if its epoch is scanned, and otherwise goes through the existing
  `unranked` and `rerank` path. The new candidate then has to be strictly
  preferred to it to be `NewBest`.
- `bin/sova/src/main.rs`: install the reader over `node.provider` next to
  the existing `set_canonical_reader` call (around line 237 on the
  branch), reading the header hash, the parent and the withdrawals.
- `crates/engine/src/p2p/service.rs`, `on_announce`: when
  `a.height <= local` and `backend.canonical_hash(a.height) !=
  Some(a.hash)`, count it per peer. Log a warning and set a metric
  (`sova_history_divergence{depth}`) when announcements that are not
  ours arrive at depth ≥ 2, which rules out ordinary tip siblings. The
  block is still fetched and still goes through the tracker, where the
  sibling rule decides. Nothing switches automatically.
- Tests: flip `after_restart_any_rank_is_adopted_first` into
  `after_restart_the_canonical_block_sets_the_bar` (a rank-7 sibling is
  `NotBetter` against a canonical rank-0 block; a rank-0 sibling against
  a canonical rank-2 block is still `NewBest`, the late win). Add a box
  sim: restart node C, serve it a rank-1 sibling of its rank-0 tip,
  assert no FCU. Control: the current branch build adopts it.

**Effort.** 1 to 2 days, plus a day for the sim.

## B. Client checkpoints

**Idea.** A list of `(height, hash)` pairs. A node accepts no block at a
checkpoint height other than the listed one, so it follows no history
that contradicts the list. This is weak subjectivity, and it should be
called that.

**Trust model, plainly.** A node that runs with checkpoints trusts
whoever chose them to have named the history the network followed: Sova
Labs for the list built into a release, and the operator (or whoever the
operator copied) for entries added by configuration. It does not trust
them for validity. Every block is still checked against the node's own
zebrad: the mint (C5), the anchor (SIP-4 §1), and the seal after SIP-6.
A wrong checkpoint cannot create SOVA or change a mint. It can put the
node on a different valid history, or stop it at that height. Anyone
can check a published checkpoint against any node with
`eth_getBlockByNumber`.

**Choosing and distributing checkpoints.**

- A checkpoint is a Sova block whose Zcash epoch is at least 1,000
  Zcash blocks deep. Zebra 6.3.0 does not roll back past 1,000 blocks
  (`MAX_BLOCK_REORG_HEIGHT`, `zebra-chain/src/parameters/constants.rs:30`),
  so at that depth the checkpoint adds no new assumption about Zcash
  reorgs. The block must also be identical on at least two independently
  run nodes (the keeper and one other).
- It is built into the chain profile (`bin/sova/src/chain.rs`, a
  `SOVA_TESTNET_CHECKPOINTS: &[(u64, [u8; 32])]` next to the bootnodes),
  refreshed every release, and printed as a `sova-checkpoint <height>
  <hash>` line in the release notes, in the public repo and on sova.io.
- Operators can add entries with `SOVA_CHECKPOINTS=height:0xhash,...`.
  An operator entry that contradicts a built-in entry at the same height
  makes the node refuse to start. It never silently overrides.

**Closes.** Join and eclipse below the newest checkpoint: a joining
node cannot be fed a different history there, whoever its peers are. A
node that restarts on the wrong branch finds out at startup if the fork
is below a checkpoint. After SIP-6, it also closes long-range rewrites
with old sealer keys below the checkpoint (audit §4, the PoS
comparison).

**Leaves open.** Everything above the newest checkpoint, which covers
one release cycle (days to weeks). At the testnet launch the list holds
only genesis, so it protects nothing until the first refresh, about a
day in (1,000 Zcash blocks is about 21 hours). It depends on trust in
whoever publishes the list. A Zcash reorg deeper than a checkpoint would
make checkpointed nodes hold at that height instead of following. That
is intended, and at 1,000 blocks it is outside what zebrad itself does.

**Sketch.**

- `crates/engine/src/consensus.rs`, `SovaConsensus::validate_header`:
  if `header.number` is a checkpoint height and the sealed hash differs,
  return a permanent `CheckpointMismatch`. reth calls `validate_header`
  on all three import paths (payload, download, backfill; p2p-m1
  Decision 1, SIP-6 §2.5), so no path escapes it. Permanent is correct
  here, since no block at that height with another hash is ever
  acceptable to this node, and reth's invalid-ancestor handling then
  rejects its descendants.
- `candidates.rs`, `actionable_target`: drop a target that sits at a
  checkpoint height with another hash. A target above the newest
  checkpoint is still first-seen: backfill fails at the checkpoint and
  the driver moves on to the next target.
- `bin/sova/src/main.rs`, startup: for every checkpoint at or below the
  head, compare `provider.block_hash(height)`. On a mismatch, exit with
  a message that says the database follows another history and must be
  unwound or resynced.
- `bin/sova/src/chain.rs`: the list, the env parser, and the conflict
  check. This is the same place the audit's F9 fix moves
  `SOVA_EPOCH_BASE` and the emission schedule.
- `docs/ops/testnet-launch.md`: the refresh procedure (pick a height,
  compare on two nodes, publish).

**Effort.** 1 to 2 days of code, and half a day for the release
procedure.

## C. Cumulative sealer-rank fork choice

**Idea** (audit §6.2). Compare two branches by how well-ranked their
sealers were, summed over the heights where they differ. The honest
chain is nearly all rank 0. After SIP-6 only the key of the epoch's
rank-0 burner can make a rank-0 block. So beating the honest chain from
a fork point `f` needs a rank-0 block at almost every height since `f`,
and therefore the keys of every one of those epochs' top burners.

**Definition.**

- Score per block: `s = min(r, 63)` for a block sealed by `ranked[r]`,
  `64` for a sealer demoted for equivocation (SIP-6 §2.7), and `128`
  for a null block or a burn-less or rewardless block (today's rank
  `usize::MAX`). Bounded scores keep sums from overflowing and keep null
  clearly last.
- For tips `A` and `B` with last common ancestor at height `f`, let
  `m = min(height(A), height(B))` and `S_X = Σ s(X_h)` for `h` in
  `f+1 ..= m`. The lower `S` wins. On a tie, the longer branch wins. On
  a further tie, the node keeps the branch it is on (the incumbent). A
  joining node with no incumbent takes the branch whose first block
  after `f` has the lower hash.
- At one height (`m = f + 1`) this is exactly SIP-2's `(rank, hash)`
  preference, so the tip and the late win behave as today.
- **Not lexicographic.** Comparing rank vectors from the fork point lets
  the first block decide everything. The audit rejects that, and so does
  this note.
- **Not burn-weight sums.** Summing the sealers' burn weights gives the
  same order in nearly every case, because weight is monotone in rank
  within an epoch. But it loses SIP-2's min-txid tie-break between equal
  weights, and it needs per-rank weights for every block compared. Burn
  weight enters fork choice properly in SIP-8, as votes.

**Closes.**

- Join, restart and partition among histories whose scores differ.
  The result depends on the blocks, not on arrival order.
- A3 after SIP-6: a dominant burner can rewrite only back to the last
  epoch in which it was not rank 0, not every epoch in which it holds a
  key (audit §4, third column).
- Partition healing: two halves whose sealers differed converge on the
  better-ranked side, which the sibling rule alone never does.

**Leaves open.**

- **Before SIP-6 it gives convergence, not security.** Rank is read
  from withdrawals, and anyone can copy the rank-0 withdrawals (F5), so
  an attacker ties the honest score everywhere. An online node keeps its
  incumbent, and a joining node falls to the hash tie-break, which the
  attacker can grind. Ship it with SIP-6.
- A burner that was rank 0 in every epoch since `f` can present an
  equal-score alternative. Online nodes keep theirs, and a joining node
  picks by hash.
- A coalition of past top burners can assemble a better-scored old
  history (the long-range case). Checkpoints (B) bound it.
- It needs every candidate branch's headers to score them. For a
  joining node that means a header-first comparison before backfill.
  After SIP-6 rank comes from the seal in the header. Before SIP-6 it
  needs bodies.

**Sketch.**

- `crates/consensus/src/sealer.rs`: `rank_score` and `prefer_branch`
  as pure functions next to `prefer`, with unit tests.
- `crates/engine/src/candidates.rs`: `Seen` gains `score`. A new
  `fork_point(tip)` walks `seen` parents down to a block the canonical
  reader knows (bounded by `RETAIN`, 1,024), and canonical scores come
  from A's reader. `best(height)` stays for siblings. A new
  `preferred_tip()` compares each unattached leaf with the head through
  `prefer_branch`. `attached()` stays as the fast path, and unattached
  candidates are kept for scoring instead of being ignored.
- `run_arbiter`: the stale-height skip (`best.sova_height < head`) is
  replaced by "adopt if `prefer_branch` says the candidate's branch
  beats the head's". Reorgs deeper than the tip become possible, but
  only to a strictly better branch.
- `bin/sova/src/main.rs`, the arbiter FCU: the `lag(32)`/`lag(64)`
  safe/finalized markers must not sit above a possible fork point (audit
  F7). Use the newest checkpoint as finalized until SIP-8 margins exist.
- `run_sync_driver` and `actionable_target`: choose among remembered
  targets by `prefer_branch` over their headers, fetched through reth's
  `FetchClient` (the `HeadersClient` the backfill downloader uses), not
  by height.
- `crates/engine/src/miner.rs`, the one-second re-assert
  (`release`, `miner.rs:156-172`): use `preferred_tip()`.
- Tests: `deeper_better_ranked_branch_wins`,
  `equal_score_keeps_incumbent`, `null_branch_loses` in
  `crates/engine/tests/audit_fork_choice.rs`. Sims:
  `two-histories-join-scenario.sh` (two peers, two valid histories; a
  fresh node picks the better-scored one in either connection order) and
  `partition-heal-scenario.sh`.

**Effort.** 2 to 3 weeks, including the header-first join and the sims
(the audit estimated 2 weeks without the header-first part).

## Summary

| Measure | Closes | Leaves open | Needs | Effort |
|---|---|---|---|---|
| A. Canonical comparison on restart | restart tip downgrade; silent divergence | join, eclipse, wrong-branch restart, partition | nothing | 2–3 days |
| B. Client checkpoints | join and eclipse below the checkpoint; long-range below it | everything above the newest checkpoint; trust in the publisher | a release procedure | 2–3 days |
| C. Cumulative sealer rank | join, restart and partition where scores differ; A3 depth | ties from a burner that was always rank 0; past-top-burner coalitions | SIP-6 for security | 2–3 weeks |
| SIP-8 anchored burns | the first-seen input below the tip, for every case | the newest 1–2 blocks; vote buying; a sustained burn majority | SIP-6 first | about 4 weeks |

## Recommended order

1. **A now.** It is pure code with no process attached, it closes the
   one F2 case the audit tests, and C reuses its canonical-block
   reader.
2. **B before the public testnet**, with a genesis-only list at launch
   and the first real checkpoint about a day in. The audit makes it
   blocking (remediation item 4), and it is the only one of the three
   that helps a joining node before SIP-6.
3. **C with SIP-6 at the testnet reset.** Without signatures it buys
   convergence only, so shipping it earlier would suggest protection it
   does not give.
4. **SIP-8** through review during the testnet. It activates at a
   testnet fork height and from genesis on mainnet. It makes C the
   tie-break and B the floor, and removes the need for A.

Until SIP-8, the testnet docs should say it plainly: transaction history
above the newest checkpoint is final only as far as the node's peers are
honest and its sealers are well ranked. Mints are final at Zcash depth.
