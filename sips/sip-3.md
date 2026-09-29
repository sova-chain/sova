# SIP-3: Emission Schedule

- Status: **Accepted** (Rob sign-off 2026-09-22 — numbers locked as
  drafted; changeable only via SIP-1 burn-weight signaling, per
  "Changing this schedule"). **Revision 2** accepted (Rob sign-off
  2026-09-29): follows Zcash NU7 / ZIP 218 — see "Revision 2".
  It precedes mainnet genesis, so no fork is involved.
- Implementation: `crates/consensus/src/schedule.rs` (normative once
  wired)
- Author: Sova (orchestrated draft)
- Depends on: SIP-1 (burn recognition), SIP-2 (epochs, ranking,
  settlement derivation)

## Summary

SOVA is minted only by epoch rewards to burners (SIP-2). This SIP locks
the reward schedule: a **Zcash-style slow start**, then a flat per-epoch
reward that **halves every 5,040,000 epochs** — Zcash's own halving
interval after NU7 (ZIP 218), which Sova inherits naturally because one
epoch *is* one Zcash block. Burn-less epochs mint nothing and their rewards are never
made up. Long-term security funding transitions to fees: the EIP-1559
basefee is burned, priority fees and the sealer tip pay sealers.

## Constants

| Constant | Value | Meaning |
|---|---|---|
| `BASE_EPOCH_REWARD_GWEI` | 2,083,333,320,000 gwei (2,083.33332 SOVA) | full per-epoch reward, era 0 |
| `ERA_EPOCHS` | 5,040,000 | epochs per halving era (~4 years at 25 s; = Zcash's post-NU7 halving interval) |
| `SLOW_START_EPOCHS` | 60,000 | linear ramp length (~17.4 days at 25 s) |
| `SLOW_START_STEP_GWEI` | 34,722,222 gwei | exact ramp step (`BASE / SLOW_START`, divides exactly) |
| sealer tip | 1/10 of the epoch reward | SIP-2's derivation; the fraction is locked here |
| `MIN_BURN_ZAT` | 1,000 | SIP-1's dust floor, restated for the economics section |

All reward arithmetic is in the gwei domain (SIP-2): every value above
is integer-exact, no rounding anywhere in the ramp.

## The schedule

For 0-based epoch index `E` (epochs since network genesis; the epoch at
Sova height `H` has index `H − 1`):

```
reward(E) = SLOW_START_STEP_GWEI × (E + 1)          if E < 60,000
          = BASE_EPOCH_REWARD_GWEI >> (E / 5,040,000)  otherwise
```

- **Slow start** (epochs 0–59,999): the reward ramps linearly from
  0.034722222 SOVA to the full 2,083.33332 SOVA in exact 34,722,222-gwei
  steps.
- **Eras**: from epoch 60,000 to 5,039,999 the reward is flat 2,083.33332
  SOVA; each subsequent era halves it (integer gwei floor). Era 40's
  reward is 1 gwei; era 41's is 0 — emission ends (~164 years).
- **Asymptotic supply**: exactly 20,937,500,907.57594 SOVA — "just
  under 21 billion" the same way Bitcoin's cap is just under 21
  million. The gap below 21B is the slow-start shortfall (the ramp
  deliberately never mints about half an epoch-reward per ramp epoch)
  plus sub-gwei halving dust. We do not shift era boundaries to claw
  it back; the schedule stays a pure function of the epoch index.

The exact asymptote is pinned by a supply-audit test in
`schedule.rs` that sums the whole schedule.

## No mint without burns

A burn-less epoch mints **zero** and the scheduled reward for it is
gone — there is no carry-over, no accumulation, no retroactive claim.
Consequences, all intended:

- Realized emission is ≤ the schedule and automatically tracks real
  participation: no demand, no dilution.
- There is no "jackpot epoch" to time: carrying unminted rewards
  forward would turn the first burn after a quiet stretch into a
  lottery and make conservation checking stateful. Rejected.
- The supply asymptote above is therefore an upper bound; the honest
  statement is "at most ~20.94B, minted only against demonstrated
  demand."

## Why this shape (rationale)

**Fixed reward + pro-rata split is the anti-farming mechanism.** The
epoch reward does not grow with burn volume; more burners slice the
same per-epoch reward thinner. Farming intensity adjusts the *price* of a
share (in destroyed ZEC), never the *supply*. Early over-farming
self-corrects: it permanently destroys ZEC for a thinner slice.

**The slow start closes the worthless-token window.** In the first days
SOVA has no market and dust burns would otherwise capture full per-epoch
rewards. Ramping the reward makes week-one capture proportionally
small while everyone learns the tooling — and it is a direct homage to
Zcash's own launch (Zcash ramped its block subsidy over its first
20,000 blocks, about 17 days, for exactly this reason). Sova keeps the
17 days: 60,000 epochs at 25 s.

**Halving at 5,040,000 epochs is inherited, not invented.** Sova epochs
are Zcash blocks; adopting Zcash's halving interval means Sova's eras
tick in lockstep with the host chain's own emission cadence.

**The far future is fees.** As the subsidy halves away, sealers are
paid by (a) the 1/10 sealer tip while subsidies last, and (b) EIP-1559
priority fees. The basefee is **burned** (standard EIP-1559), so SOVA
is deflationary under sustained usage. This is Bitcoin's fee-transition
story with Ethereum's fee mechanics, on top of a chain whose block
cadence never depends on the reward (cadence blocks are produced
rewardlessly per SIP-2 — liveness needs nodes, not burns).

**Cost floor note (from SIP-1's economics).** At the dust floor, the
dominant per-epoch cost of mining is the Zcash ZIP-317 fee
(~20,000–25,000 zat for the burn tx shape), not the burn itself
(1,000 zat minimum). Keeping a miner alive every epoch costs pennies
per day and is dominated by fees paid to Zcash miners — a spam floor
for Sova and a small ongoing gift to the host chain's security budget.

## Revision 2: Zcash NU7 (ZIP 218)

Zcash NU7 (testnet 2026-10-06, mainnet 2026-11-05) cuts Zcash's block
spacing from 75 s to 25 s and, in ZIP 218, triples its halving interval
(1,680,000 → 5,040,000 blocks) and divides the block subsidy by 3, so
Zcash's issuance per unit of time is unchanged. Sova epochs are Zcash
blocks, so revision 1's numbers would have tripled SOVA's issuance per
day and shortened each era to ~1.3 years. Revision 2 mirrors ZIP 218:

| | Revision 1 (75 s) | Revision 2 (25 s) |
|---|---|---|
| `BASE_EPOCH_REWARD_GWEI` | 6,250,000,000,000 | 2,083,333,320,000 |
| `SLOW_START_EPOCHS` | 20,000 | 60,000 |
| `SLOW_START_STEP_GWEI` | 312,500,000 | 34,722,222 |
| `ERA_EPOCHS` | 1,680,000 | 5,040,000 |
| final era (reward 0) | 43 | 41 |
| asymptotic supply (SOVA) | 20,937,503,124.97144 | 20,937,500,907.57594 |

The base is 6,250 SOVA ÷ 3 rounded down to a multiple of 60,000 gwei, so
the ramp step stays exact; it sits 13,333 gwei below a true third, which
over the whole schedule mints about 2,217 SOVA less (0.00001%). Per day,
the slow start, the eras and the supply are what revision 1 published.

Sova mainnet starts after Zcash mainnet NU7, so it only ever sees 25 s
epochs: revision 2 applies from genesis and needs no activation height.
The public testnet mints a flat 6,250 SOVA per epoch and keeps it through
NU7 (three times the SOVA per day; harmless on a testnet, and changing it
would split running nodes).

## Changing this schedule

Only by soft fork via SIP-1 burn-weight signaling (the version-bits
tally machinery, workstream C7). No admin keys, no discretionary
issuance, no council.

## Implementation status

`crates/consensus/src/schedule.rs` implements `epoch_reward_gwei(E)`
with boundary and supply-audit tests, and C8 wired it through the node:
the `Schedule` type feeds the sealer, the expectations follower, and
the validator from one construction site (`SOVA_EMISSION_SCHEDULE=sip3`
selects this schedule; the default stays flat for regtest and the box
scenarios' deterministic exact-mint assertions). The sip3 mode's live
debut is mainnet genesis: the public testnet mints a flat 6,250 SOVA per
epoch (Rob, 2026-09-23), so testers hold usable balances from the first
epoch. Revision 2's constants are in `schedule.rs`; the testnet's flat
reward is the literal `DRAFT_EPOCH_REWARD_GWEI` in `crates/engine`,
independent of them.
