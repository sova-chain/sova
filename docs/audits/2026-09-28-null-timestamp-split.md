# 2026-09-28: a Zcash reorg split the testnet for 28 minutes

## What happened

- 22:16:26 UTC. The keeper sealed null block 22439 on 22438. A null block's
  timestamp is exactly `max(parent + 1, zcash_time)`, and the keeper's zebrad
  had Zcash block 4,410,938 with time 1790633786.
- 22:16:28. seed-1's zebrad had a different block at 4,410,938 (time
  1790633785): Zcash testnet was reorging (seed-1 unwound to 4,410,937 two
  seconds later). seed-1 judged 22439's timestamp against its own Zcash time,
  found 786 ≠ 785, and rejected it as `sova-seal: timestamp … outside …`.
- That error is permanent: reth cached 22439 as invalid, every descendant
  was then "links to previously rejected block", each one cost the keeper
  reputation, and the seeds banned it. The keeper (the only sealer) redialed
  every 45 s and was dropped each time. seed-1, seed-2 and rpc-1 (the public
  RPC) stayed at 22438; the keeper sealed on alone.
- The Zcash chain that won had time 1790633786 at 4,410,938: 22439 was valid.
- 22:40–22:44. Restarting `sova-node` on seed-2, seed-1 and rpc-1 cleared the
  in-memory invalid-block cache and the ban; all nodes converged on the
  keeper's chain (same hash at 22594; smoke 51/51).

## Cause

`SovaConsensus::validate_header_against_parent` checked the SIP-6 timestamp
rule against *our* Zcash record for the epoch before anything checked that
the block commits to that same Zcash block. The block's own anchor is only
checked later, in `check_settlements`, where a mismatch is a hold. So a block
anchored to a Zcash block we don't (yet, or no longer) have could be judged
on a timestamp derived from the wrong Zcash block, and the verdict was
permanent.

Not new in v0.1.13: the order predates the G1–G3b fixes. It needs a Zcash
reorg at the tip that changes the tip block's time while a null block is in
flight, which Zcash testnet's fast bursts make likely enough.

## Fix

Before the timestamp rule, check the header's own anchor against our record
(`check_anchor`); a mismatch returns `SettlementError::AnchorMismatch`, a hold
(transient, never cached, no reputation hit), as `check_settlements` would.
With our anchor, a wrong timestamp stays permanently invalid. Test:
`a_null_block_anchored_elsewhere_is_held_not_judged_on_its_timestamp`
(fails without the fix with tonight's exact error).

## Follow-ups

- Any other rule that reads our Zcash record before the anchor is checked
  must hold on an anchor mismatch too; this was the only one found in
  `validate_header_against_parent`.
- A split with no sealer on the seeds' side shows as `block_age` / `epoch_lag`
  on the seeds and `0 P2P peers` on the keeper; the alerts fired (18 min) but
  the smoke test caught it first. A keeper-isolated alert would say it
  directly.
