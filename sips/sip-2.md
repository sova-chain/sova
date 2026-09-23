# SIP-2: Epochs, Rewards, and Settlement

- Status: **Draft** (implemented; single-node live-proven on regtest —
  exact-conservation mints; multi-node relay proof in progress)
- Implementation: `crates/consensus/src/{epoch.rs,sealer.rs,follower.rs}`,
  `crates/engine/src/{payload.rs,builder.rs,driver.rs}` (normative)
- Author: Sova (orchestrated draft)

## Epochs

One Zcash block = one epoch. Every SIP-1 burn confirmed in the epoch's
block makes its EVM address a miner of that epoch; an address's weight is
the saturating sum of its burns. One Sova block is produced per epoch;
from base height `B`, epoch `E`'s expected Sova height is `E − B + 1`.
Sova fork choice follows the node's Zcash view: a Zcash reorg implies the
corresponding Sova reorg (finality = Zcash confirmation depth).

## Ranking and sealing

Miners rank by (weight desc, then byte-lexicographic min-txid asc).
Rank 0 is the epoch's sealer; ranks 1… are liveness fallbacks on a
timeout ladder (rank r may produce at elapsed ≥ r × step; draft step
15 s). **Timeouts are liveness-only, never validity or preference**:
among an epoch's candidate blocks, preference is (rank asc, block hash
asc) — a late rank-0 block displaces an on-time rank-1 block via a
micro-reorg bounded to the epoch; equivocations tie-break by hash.
A node whose chain head already covers an epoch's expected height does
not produce for it. Empty epochs (no burns) may be extended rewardless
by recent sealers on the same ladder.

## Rewards

Draft schedule: 6,250 SOVA per epoch (21B cap, 4-year halvings at 75 s
epochs; final numbers are SIP-3). All reward math runs in gwei so every
share is gwei-exact. The reward splits: a 10% sealer tip, and a pro-rata
pool by weight (floor division); all rounding dust joins the tip.
**Conservation is exact**: shares always sum to the full epoch reward.
The sealer must be a ranked miner of the epoch.

## Settlement

An epoch's rewards enter the EVM as the block's **withdrawals**
(Ethereum's consensus-grade balance-increment channel, inherited
unchanged from the execution layer): withdrawal i = { index: i,
validator_index: 0 (reserved marker), address, amount in gwei }, in rank
order. A block claiming epoch E must carry exactly the withdrawals this
derivation produces — carried withdrawals that differ from the
derivation make the build/import invalid. Amounts must be gwei-aligned,
non-zero, and fit u64 gwei (guaranteed by the gwei-domain reward math).

## Validation roadmap (normative intent)

Every node re-derives an imported block's expected settlements from its
own Zcash view and rejects mismatches (C5); v1 nodes converge on
first-arrival via relay with rank preference following (documented in
docs/design/gossip-v1.md).

Implemented and proven 2026-09-22 (ladder scenario, nightly CI): a
block for epoch E is valid iff its withdrawals match the derivation for
*some* rank of E (the sealer tip makes each rank's derivation distinct,
so the sealer's rank is recoverable from the withdrawals alone — no
sealer metadata rides the wire); preference (rank asc, hash asc)
arbitrates between valid candidates on the receiver, never the sender
("relay delivers, arbiter decides"); burn-less epochs require empty
withdrawals and their candidates tie-break by hash. A late rank-0
block displacing an on-time rank-1 block via bounded micro-reorg is
observed behavior, not just intent.
