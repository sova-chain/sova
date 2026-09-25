# Faster Sova blocks: worth it?

Status: **decision note for Rob.** Written 2026-09-25. Research only: no
code, no SIP, and nothing on the roadmap changes until Rob decides. Code
references are to `release` at `6c700c8`. Wait-time figures are
*computed* from an exponential model of Zcash block intervals, not
measured, unless marked otherwise.

## The answer

**Not now.** Faster blocks would make Sova *feel* faster, but they would
not make anything *settle* faster. Settlement is bound to Zcash: mints
are final at Zcash depth, a transaction is settled once three more
epochs are built on it (about four minutes), and ZEC payments wait for
3–10 Zcash confirmations whatever Sova does. The one design that fits
Sova's model is sub-blocks: the epoch's sealer makes several blocks
until the next Zcash block. Stacks tried that design ("microblocks") and
removed it. The reason they gave also applies to Sova: the next leader
can orphan the tail, so a sub-block receipt is only a signed promise.
Stacks fixed it by adding a signer set that approves every block. Sova
has no such set, and adding one would be a new kind of consensus
participant. So sub-blocks would cost about 10–13 worker-weeks (*estimate*) of changes to
exactly the code the 2026-09-23 reorg audit hardened. What they buy is an
earlier *signal*. About a week of UX work gets most of that: pending
states, sending the Ashwings `reserve` early, batched agent
transactions, and a documented latency budget. **Revisit with testnet
data by end of November 2026.** That is the last point at which
sub-blocks could still ship at mainnet genesis without fork logic.

## 1. Who waits, and how long

Sova makes one block per Zcash block. Zcash's target is 75 s, and its
block intervals are roughly exponential (proof-of-work). Exponential
intervals are memoryless, so a transaction sent at a random moment
waits for the *next* Zcash block. That wait averages the full 75 s, not
half of it:

| Chain | Block time | Wait for first inclusion |
|---|---|---|
| Bitcoin | 600 s mean, exponential | median ~7 min, p90 ~23 min |
| **Zcash and Sova today** | 75 s mean, exponential | **median ~52 s, p90 ~2.9 min, p99 ~5.8 min**, plus 2–5 s of sealing |
| Ethereum L1 | 12 s fixed slots | ~6–12 s |
| Stacks (after Nakamoto) | ~5 s blocks inside each Bitcoin-block tenure | ~5 s [1] |
| Base and other OP-stack L2s | 2 s blocks, 200 ms Flashblocks preconfirmations | < 1 s signal [2] |

Measured on the public testnet: an average of 49 s over 20 blocks
(minimum 1 s, maximum 192 s). That is a small sample, and Zcash testnet
difficulty doesn't behave like mainnet, so plan with the mainnet model.

**The tail is worse than the mean.** About 1.8 % of Zcash intervals
exceed 5 minutes. At 1,152 blocks a day, that is about 20 five-minute
stalls a day (*computed*). Users will notice these more than the
average.

**Who is hurt:**

- **Dapp UX: swaps, mints, wallets.** Every action takes one block, a
  median of about 50 s. Flows that need several dependent transactions
  pay that per step: approve then swap is about 2 minutes. A swap has to
  cover a minute or more of price movement in its slippage. **This is
  the real cost.** No EVM chain with lively DeFi runs at this cadence.
  But Sova's use cases are not high-frequency trading.
- **Ashwings mint for SOVA.** One transaction, about a minute. That is
  tolerable with a good pending state ("your owl is being minted").
- **Ashwings mint for ZEC** (`docs/design/ashwing-zec-checkout.md`). The
  flow is: `reserve`, which needs one Sova block before the QR code can
  show the tag; then the Zcash payment plus `minConf` confirmations (3 on
  testnet, about 4 min; 10 on mainnet, about 12.5 min); then `claim`,
  which lands one Sova block after the confirming Zcash block is
  anchored. On mainnet that is about 14–15 min end to end today. With
  sub-blocks it would be about 13 min. With the two cheap fixes in §4 it
  is about 13–13.5 min, with no consensus change. **Faster Sova barely
  helps ZEC payment verification**, because the wait is Zcash's.
- **Agents paying gas.** An agent doesn't mind waiting. It minds
  sequential round trips. Independent actions already fit in one block
  (nonce pipelining). Dependent ones can be batched: `Multicall3` is
  vendored (`contracts/src/vendor/Multicall3.sol`), and EIP-7702 is live
  because reth's DEV schedule activates Prague at genesis. Pay-per-call
  micropayments are too slow even at 12 s. Those flows belong off-chain
  or on accepted-pending transactions, on any chain.
- **Burn-to-mine.** Unaffected. Mints happen once per Zcash block by
  definition.
- **Exchanges and bridges.** None are planned, and wZEC is Sova Labs'
  custodial product with its own relayer. Exchanges care about
  *finality*, not block time, and sub-blocks don't change finality (§2b).

## 2. Options

### a. Keep 1:1, fix the UX

Nothing in consensus changes. What reth v2.6.0 already offers:

- **Pending state.** `--rpc.pending-block full` is the default. It
  serves a local pending block built from the mempool on top of
  `latest`, so wallets and pages can simulate against it and show an
  optimistic result. Caveat: that block's Zcash anchor isn't the next
  block's anchor, so SIP-4/SIP-7 reads in it may say `NOT_YET` or be
  refused (*to verify*). Don't build UX on pending precompile reads.
- **`eth_sendRawTransactionSync`** returns the receipt in one call, but
  its default timeout is **30 s**
  (`RPC_DEFAULT_SEND_RAW_TX_SYNC_TIMEOUT_SECS`). That is shorter than a
  median Sova block, so it times out about half the time. Raise it
  (`--rpc.send-raw-transaction-sync-timeout`) or document it. The WAF or
  proxy idle timeout in front of the public RPC also has to allow it.
- **Optimistic receipts in our own pages.** Show "submitted, then in
  block N, then settled (3 epochs)" with an honest countdown, because the
  next Zcash block really is unpredictable.

Cost: about 3–5 worker-days in total (list in §4). It breaks nothing.

### b. Sub-blocks: several Sova blocks per Zcash block

This is the only design that fits Sova's model, so here it is worked
through.

**How it would work.** Zcash block `E` arrives. The epoch's sealer, the
rank-0 burner of `E`, or a lower rank under the ladder, seals block 0 of
epoch `E`. That block carries the mint (withdrawals) and the
`ZcashBlocks` system call. The same signer then seals a sub-block every
~12–15 s, with no mint and the same anchor, until Zcash block `E+1`
arrives. The first block of `E+1` has a new signer. **The number of
sub-blocks can't be fixed at k**, because the Zcash interval is random
(1 s to many minutes). So it has to be "sub-blocks until the next
anchor", capped at `K_MAX`, with timestamp spacing enforced.

**What it breaks or complicates:**

- **The height↔epoch identity** (`E_N = N + B − 1`). This is load-bearing
  in SIP-2, SIP-3 (reward by height), SIP-4 §1, SIP-7, and SIP-8's vote
  bound (`h ≤ E − B`), and in 11 source files (`driver.rs`,
  `expectations.rs`, `votes.rs`, `zcash_index.rs`, `evm/src/{zcash,blocks}.rs`,
  `follower.rs`, `bin/sova`, and others). The epoch would be read from
  the anchor instead of computed from the height. Every one of those
  sites changes, and each one is a split risk.
- **Depth rules are counted in blocks.** `MAX_REPLACE_DEPTH = 3`,
  `SAFE_DEPTH = 3` and `FINALIZED_DEPTH = 100` are all in blocks
  (`candidates.rs`). With 6 sub-blocks per epoch, "3 blocks" would be
  about 37 s. A late rank-0 block could then no longer displace a rank-1
  epoch, so the ladder's preference would weaken without anyone noticing.
  `FINALIZED_DEPTH` must exceed Zcash's 99-block reorg limit, so it would
  have to become 100 *epochs*. The 2026-09-23 wedge (finalized = 10) was
  exactly this class of bug. All three would have to be counted in
  epochs.
- **Late wins get bigger.** Suppose rank 1 seals at +15 s and makes three
  sub-blocks, and then rank 0's block arrives. Rank 0 now displaces four
  blocks, and every transaction in them goes back to the pool. Today it
  displaces one. Any user who saw a receipt in those sub-blocks loses it.
- **The tail is orphanable by design.** A block from `E+1` has to be able
  to beat a sub-block of `E` at the same height, or the chain can't move
  on. So `E+1`'s sealer builds on whichever sub-block of `E` it had seen,
  and every sub-block after that is orphaned. That comes from latency,
  and also from greed: the `E+1` sealer can drop `E`'s tail and take its
  transactions and fees. This is Stacks' microblock failure: "no
  consensus-critical procedure … forces the next miner to build upon the
  latest microblock" [1]. Which sub-blocks survive then depends on what
  arrived first. That brings back the first-seen input the audit worked
  to remove, at every epoch boundary.
- **Zcash reorgs** (SIP-4 §7). A one-block Zcash reorg at the tip unwinds
  the whole epoch, block 0 plus all its sub-blocks, not one block. The
  replacing Zcash block may have a different rank 0, so the re-seal comes
  from a different leader. Tip churn in block count grows by k.
- **SIP-4 and SIP-7 reads.** These are fine but flat: every sub-block of
  `E` has the same anchor, so confirmations and pool totals don't move
  between Zcash blocks. The `ZcashBlocks` system call must run only in
  block 0. `PREVRANDAO = keccak(anchor)` (SIP-6) would repeat for the
  whole epoch.
- **Null blocks.** A burn-less epoch has no signer, so it gets no
  sub-blocks either. Transactions still wait a whole epoch in quiet
  stretches, as they do today.
- **Ladder and leader liveness.** The ladder step is 15 s
  (`DEFAULT_RANK_STEP`), which is longer than a sub-block. An absent rank
  0 already costs the first 15 s. Worse, the leader is known for the
  whole window as soon as `E` is mined, so it can be DoSed for exactly
  that window. Then rank 1 needs a second ladder for "the leader went
  silent mid-epoch". That means more rules and more equivocation cases
  (SIP-6 §2.7 slots become per sub-block).
- **Censorship and MEV.** Today one sealer orders about 75 s of
  transactions in one block. With sub-blocks one sealer orders the same
  75 s across k blocks. The censorship window in time is the same. MEV
  gets somewhat richer, because controlling k consecutive blocks allows
  multi-block manipulation, such as TWAP oracles. Mostly the leader moves
  one epoch earlier: `E`'s winner orders the transactions sent between
  `E` and `E+1`, where today `E+1`'s winner does.
- **The public story.** The paper says "one Zcash block is one epoch,
  and each epoch has exactly one Sova block" and "Zcash blocks arrive
  about every 75 seconds, and so do Sova's" (`Paper.astro` §3). SIP-3
  sells halvings "in lockstep" with Zcash. That clarity would be lost.

**The key point.** A sub-block is final only once `E+1`'s first block
builds on it, which is about when the 1:1 block would have landed anyway.
So a sub-block receipt amounts to a preconfirmation signed by `E`'s
leader, which the next leader can break for free. Stacks got real
intra-tenure blocks only by making a 70 % stacker signer set approve
every block ("This blockchain will only fork if 70% of Stackers approve
the fork") [1]. Sova has no staked set, and building one is option d.

**Effort** (*estimate*, worker-weeks): SIP 1; header and block rules
(epoch from anchor, signer continuity, mint only in block 0, `K_MAX`,
timestamp spacing) 2; tracker, arbiter and branch rule re-keyed to
epochs, plus late wins across sub-blocks, 2–3; sealer and miner
sub-block loop with a leader-silence fallback 1–1.5; SIP-3/4/7/8 formula
and index updates 1; explorer, RPC, MCP and contract review 0.5–1; sims
(re-run all, plus new ones: tail orphaning, a late rank 0 over rank-1
sub-blocks, a k-block Zcash reorg, leader DoS, join) 2–3; reorg audit
re-run 1. **About 10–13 worker-weeks, plus a testnet reset.**

### c. Preconfirmations from the current sealer

Under 1:1 there is **nobody who can credibly preconfirm**. A transaction
sent now lands in the block of epoch `E+1`. The sealer of that block is
`E+1`'s heaviest burner, and nobody knows who that is until Zcash block
`E+1` is mined. This is a security property: nobody can bribe or DoS a
leader who isn't known yet. The only candidate issuer is a burner who is
*usually* rank 0, which in practice means the Sova Labs keeper. Its
promise breaks whenever someone outburns it, and it trusts Sova Labs.
Enforcement could be a SOVA bond in an ordinary contract, slashable on a
signed-but-unincluded proof. That needs no consensus change and about
2–3 weeks. But it would make Sova Labs the chain's fast path, which is
the wrong look for a public-good experiment. **Not recommended.**
Preconfirmations only become natural with (b), where the leader is
known, and then they have (b)'s weakness.

### d. Other options

- **Decouple blocks from epochs entirely.** Leaders would be elected
  from recent burners on a fixed ~12 s clock, with BFT finality among a
  committee, and mints inserted when the anchor advances. This is
  Nakamoto-style, and it is a second consensus protocol: a committee,
  thresholds, liveness assumptions, and months of work. It strains
  "burn decides" and invites staking. **No.**
- **Shave the latency inside 1:1.** This is cheap and worth doing. The
  sealer polls zebrad every 2 s (`bin/sova/src/main.rs`) and waits up to
  5 s for its scan (`SCAN_WAIT`). An absent rank 0 costs 15 s per rung.
  Measure "Zcash block seen → Sova block at the public RPC" and keep the
  p95 under about 5 s. Keep the keeper burning every epoch, so the ladder
  rarely waits and null epochs, which starve transactions, stay rare.

## 3. Invariants

| Invariant | a. UX | b. Sub-blocks | c. Preconf | d. Decoupled |
|---|---|---|---|---|
| Burn stays 100 % pure | kept | kept, if the mint stays in block 0 and sub-blocks earn only fees | kept (a bond is SOVA, not burn) | **at risk**: committees invite staking |
| No admin keys in consensus | kept | kept | kept in consensus; **trusted party in UX** | **at risk**: committee membership |
| Every node re-derives from its own zebrad | kept | kept (the anchor still decides) | kept | **strained**: where a mint lands becomes a rule |
| Relay delivers, arbiter decides | kept | **strained**: tail survival depends on what the next sealer saw first | kept (outside consensus) | replaced by committee votes |

## 4. Recommendation

**Take option a now. Don't open a sub-block SIP yet.** The work, about
1 week in total (*estimate*):

1. **Pending and optimistic UI** on sova.io, the explorer and the
   Ashwings pages: "submitted, in block N, settled", with an honest
   next-Zcash-block countdown. 1–2 days.
2. **Ashwings ZEC checkout.** Fire `reserve` as soon as the Sova address
   is pasted, so its block overlaps with the buyer opening their wallet.
   Have the relayer send `claim` as soon as its own zebrad shows
   `minConf`, instead of waiting for a dry run against the anchored
   block. A claim that lands early reverts and costs the relayer a little
   gas, and the next block retries it. This makes the claim land in the
   block that anchors the confirming Zcash block, not the one after
   (*to verify* against the relayer's code). 1 day.
3. **RPC.** Raise the `eth_sendRawTransactionSync` timeout, or document
   it, and check the WAF idle timeout. Half a day.
4. **Latency metrics on testnet.** A histogram of inclusion latency and
   of "Zcash block → Sova block at the RPC" lag, with an alert above
   10 s. 1 day.
5. **Agent and MCP docs.** Explain the cadence (median about 50 s, and a
   5-minute gap is normal) and show batching (Multicall3, EIP-7702) and
   nonce pipelining. Half a day.
6. **Keep the door open.** Library docs and our contracts should use
   Zcash anchor height or `block.timestamp` as a clock, never
   `block.number`. `ZecCheckout` already counts its window in Zcash
   blocks. Then sub-blocks later wouldn't break deployed contracts.

**What would change the answer.** A real dapp showing drop-off
attributable to latency. Interactive use, such as games or an active
DEX, becoming the goal. A demo partner who needs sub-10 s feedback. Or a
signer set appearing for other reasons. SIP-8's burn votes can't police
sub-blocks, because they land one Zcash block later.

**Decision date: end of November 2026, with about 6 weeks of testnet
data.** Sub-blocks from genesis avoid fork logic. After mainnet they
would be a fork-height change.

**If it's ever yes, a SIP-9 "Sub-blocks" must specify:**

- epoch identity from the anchor, not the height, with every SIP-2/3/4/7/8
  formula restated;
- sub-block validity: same anchor as block 0, same signer as block 0, no
  withdrawals, no system call, `ts ≥ parent + SUB_MIN`, and at most
  `K_MAX` per epoch;
- per-block gas at `limit / k_target`, so throughput and state growth
  don't change;
- every depth rule counted in epochs;
- fork choice between `E`'s sub-blocks and `E+1`'s first block, and what
  a tail orphan means for receipts;
- a leader-silence fallback and per-sub-block equivocation slots;
- Zcash-reorg re-seal of a whole epoch;
- what RPC `latest`, `safe` and `finalized` mean;
- a plain statement of the trust in a sub-block receipt.

The build is about 10–13 worker-weeks and a testnet reset.

## Sources

1. Stacks documentation, "What is the Nakamoto Release?" and "Nakamoto
   in 10 Minutes" (microblocks removed; ~5 s blocks per tenure; the 70 %
   stacker approval quote), retrieved 2026-09-25:
   <https://docs.stacks.co/reference/nakamoto-upgrade/what-is-the-nakamoto-release>,
   <https://docs.stacks.co/reference/nakamoto-upgrade/nakamoto-in-10-minutes>.
2. Base, "Flashblocks deep dive" (200 ms Flashblocks inside 2 s blocks,
   mainnet July 2025): <https://blog.base.dev/flashblocks-deep-dive>.

The reth facts (`--rpc.pending-block`, the 30 s sync-send timeout, and
Prague at genesis in DEV) were checked in the reth v2.6.0 checkout at
rev `73a3a00`. The wait percentiles use `75 s × ln(1/(1−p))` (and
`600 s × …` for Bitcoin).
