# Follower stale-block gaps (G1, G2)

2026-09-27. Found while analysing the reorg-stress seed-202 failure (board
2026-09-26 21:13 EDT). The trigger — a sealer restarted onto a head whose
anchor was reorged away built on it before its first Zcash scan — is fixed
(`24484db`, regression sim `box/sim/restart-reorg-scenario.sh`). These two
gaps decide what happens **if a stale block reaches the chain anyway**: today
the network does not recover from it. Both are pinned by `#[ignore]`d tests
that fail on current code; un-ignore each with its fix.

## G1: a stale block buried under fresh blocks is never found

Test: `expectations::tests::g1_a_stale_block_buried_under_fresh_ones_is_still_found`.

Shape (every node's chain after seed 202): block N anchored to an orphaned
Zcash block; N+1.. anchored to the current Zcash branch but built on N.
`ExpectedSettlements::effective_head` walks down from the reth head and stops
at the first block whose anchor matches our zebrad, and a pending rollback
floor above N resolves as soon as the block just above the floor matches. So
the effective head is the reth head, the sealer never re-seals N, and
followers never get a replacement to converge on. SIP-4 §7 says a block
anchored to a Zcash block our zebrad doesn't have is invalid, and so is
everything built on it; the walk only looks at the top of the chain.

Candidate fixes: (a) on every Zcash rollback, walk the whole window
(≤ STALE_SCAN_MAX) below the head and take the lowest stale block, not the
first non-stale one from the top; (b) keep the rollback floor pending until
every block above it has been re-checked, not just the one above it.
(a) is simpler and bounded; (b) avoids rescanning when nothing moved.

## G2: a child of a stale parent is accepted

Test: `consensus::tests::g2_a_child_of_a_parent_on_an_orphaned_zcash_block_is_held`.

Import checks each block's own anchor against our zebrad. The parent was
checked once, at its own import, before the Zcash reorg. A child with a good
anchor on a parent whose anchor our zebrad no longer has passes, so seed-202's
followers imported 377..410 on top of the stale 376, and the sync driver's
forkchoice update made them canonical with no further check. The child
descends from a block §7 invalidates, so it should be held like the parent.

Candidate fix: hold a block whose ancestors within the reorg window include
one anchored to a Zcash block our zebrad no longer has (checked against the
expectations map, which already knows the current branch); the sync driver
FCUs only targets that pass the same check.

## When it matters in production

Only after a stale block is built on: today that needs the sealer bug fixed
in `24484db` or a new one like it. The fixes make the network converge
instead of carrying the stale block forward; they touch consensus-adjacent
engine code (orchestrator-authored) and need the reorg-stress seeds plus the
restart-reorg sim before landing.

The un-ignored `candidates::tests::a_follower_holds_blocks_built_on_its_stale_block_and_takes_the_reseal`
passes today: once a re-sealed replacement exists, the candidate tracker
prefers it.
