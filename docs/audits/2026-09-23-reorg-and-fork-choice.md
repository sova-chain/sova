# Audit: reorgs, fork choice and finality on Sova

- Date: 2026-09-23
- Scope: the paper (`docs/paper/sova-paper-v5.md` on `release`), SIP-2, SIP-4,
  SIP-6 (draft), SIP-7 (accepted), `docs/design/{p2p-m1,gossip-v1}.md`, and
  the consensus code on branch `audit/reorg-security` (the SIP-4 branch at
  `d274e64`): `crates/engine/src/{consensus,expectations,candidates,driver,
  miner,validator,relay,zcash_index}.rs`, `crates/engine/src/p2p/service.rs`,
  `crates/consensus/src/{follower,sealer,epoch}.rs`, `crates/evm/src/zcash.rs`,
  `bin/sova/src/main.rs`; reth v2.6.0 at `73a3a00` for engine-tree behaviour.
- Method: adversarial reading of the claims against the code, with every
  load-bearing behaviour cited by file and line, and five demonstration
  tests that pin the properties the findings rest on
  (`crates/engine/tests/audit_fork_choice.rs`, all passing).
- The question asked: *"Does it really make sense that the only way someone
  can reorg or attack Sova is to do the same at the Zcash level?"*

## Executive summary

1. **No.** Zcash fixes which epochs exist and what each one mints. It does
   not fix which Sova block fills an epoch. Every block that carries the
   epoch's anchor and some rank's withdrawals is valid, whatever its parent
   and transactions, so any number of Sova histories can name the same
   Zcash chain, and nothing in Zcash distinguishes between them.
2. **Today, rewriting Sova's transaction history to any depth costs
   nothing.** Preference is `(rank, hash)` at one height and never looks at
   the parent; the arbiter adopts any preferred tip candidate with a plain
   forkchoice update; reth then reorganizes to that block's ancestry, however
   deep. A grinded copy of the tip block (SIP-6's known attack) is enough to
   carry an entire alternate history with it.
3. **After SIP-6 it is cheap and it is the sealers themselves who can do
   it.** The rank-0 sealer of any epoch can build its block on an alternate
   parent instead of the canonical one, and every node follows, back to the
   last epoch in which the attacker held no ranked key. A burn of 1,000 zat
   per epoch, about a quarter of a ZEC a day, keeps that option open
   forever, and each rewritten epoch pays its tip to the attacker.
4. **A joining, restarted or eclipsed node has no rule at all** for choosing
   between two valid histories: it takes the first tip it is offered
   (restart), the highest height announced (join), or whatever its peers
   say (eclipse). Sova transaction finality is subjective (first-seen and
   social), not objective.
5. **Mints are as robust as claimed.** The amount each burner is paid at
   each height cannot be changed without a Zcash reorg. Only the 10% tip
   and everything that happens after minting (transfers, contracts) are
   rewritable.
6. Two liveness bugs on the catch-up path: one bogus `Announce` wedges a
   joining node for good, and a flood of held blocks silently drops
   legitimate ones.
7. The paper's §3 and §10 claims about rewrite cost, and SIP-6's "the arbiter
   never reorgs below the tip", are false as built and must be corrected.
   The fix path is clear: a sibling-only reorg rule now, a parent-aware
   objective fork choice with SIP-6 for the testnet, and commitments of Sova
   block hashes carried in burns (a new SIP) for mainnet.

## 1. Threat model

- **Attacker A1, outsider:** no burns, one node, network access. Can grind
  headers and serve blocks. (Everything in SIP-6 §1.1.)
- **Attacker A2, minimal burner:** burns 1,000 zat in every epoch. Holds a
  ranked key in every epoch it burned in. After SIP-6 this is the cheapest
  attacker that can sign.
- **Attacker A3, dominant burner:** rank 0 in the epochs it chooses. On the
  public testnet this is the keeper; on mainnet it is whoever burns most in
  a given epoch. Not an outsider: an ordinary participant behaving badly.
- **Attacker A4, Zcash-level:** can reorganize Zcash to depth d. On mainnet,
  d > 2 has not happened in years. On testnet, d in the tens is affordable
  (low hashrate plus the testnet minimum-difficulty rule).
- **Victims:** an online node at the tip, a node that restarts, a node
  joining from genesis, an eclipsed node, and any user or contract that
  treats a Sova transaction at depth k as settled.
- **Assets at risk:** SOVA transfers and contract state (double-spends,
  un-inclusion), the 10% sealer tip per epoch, priority fees and MEV,
  contract timelocks (`timestamp`), and the reputation of the claim
  "finality is Zcash depth".

## 2. What is anchored to Zcash, and what is not

| Anchored (needs a Zcash reorg to change) | Evidence |
|---|---|
| Which epochs exist, in what order, at what Sova height (`E − B + 1`) | `expectations.rs:342-343`, `driver.rs:225-229` |
| The set of burns per epoch, the ranking, and the pro-rata amount paid to each burner at each height | `expectations.rs:235-257` (`check_ranked`), `consensus.rs:155-174` |
| The Zcash block each Sova block names (`parent_beacon_block_root`) | `expectations.rs:215-226`, `consensus.rs:139-154` |
| Every precompile answer, as a function of `Z[B..=E_N]` | `crates/evm/src/zcash.rs` (horizon, coverage, generation guard) |

| Not anchored (free to vary among valid blocks at one height) | Evidence |
|---|---|
| `parent_hash` (which history the block extends) | validity never reads it: `consensus.rs:124-175`, `validator.rs:81-144` |
| Transactions, their order, `transactions_root`, `receipts_root`, `state_root` | same; only reth's stateless checks apply |
| Which rank's derivation is used (the 10% tip's recipient) | any rank is valid, `expectations.rs:253-256` |
| `timestamp` (only `> parent`; no upper bound post-merge) | reth `ethereum/consensus/src/lib.rs:177-208`, `consensus/common/src/validation.rs` `validate_against_parent_timestamp` |
| `beneficiary`, `prev_randao`, `extra_data` | SIP-6 §1.1; `engine/local/src/payload.rs:48-51` |

So: **the mint is anchored; the ledger is not.** A Sova block names Zcash;
Zcash never names Sova. That asymmetry is the whole audit.

## 3. Findings

Severity: Critical = history rewrite at no or trivial cost; High = rewrite
by a normal participant, or a network-wide liveness failure; Medium =
bounded harm or an incorrect security statement; Low = residual.

### F1 (Critical). Fork choice is parent-agnostic; a preferred tip candidate carries any ancestry

**Claimed.** Paper §5: "the node reorganizes one block, at the tip …
Below the tip nothing is reconsidered." Paper §10: "below the tip, its
history is exactly as costly to rewrite as Zcash's … the only
reorganization Sova performs by itself is the one-block replacement of
Section 5." SIP-6 §1.1: the attacker cannot "rewrite history below the tip
epoch"; §2.6, §4: "the arbiter never reorgs below the tip … the stale-height
skip still bounds reorgs to the tip epoch."

**True.** Nothing compares below the tip, but adopting a tip block adopts
its whole ancestry:

- Validity is `(height, anchor, withdrawals)` only:
  `consensus.rs:124-175`; `validator.rs:81-144`. Test
  `validity_ignores_parent_and_transactions`.
- Preference is `(rank, hash)` only, keyed by height:
  `candidates.rs:69-93`, `sealer.rs:45-49`. `Candidate` has no parent
  field (`sealer.rs:35-40`). Test `preference_ignores_parent`.
- The arbiter forwards any best candidate with `sova_height >= head`
  (`candidates.rs:252-260`, strict `<`) as `ForkchoiceState { head:
  candidate }` (`main.rs:338-349`). The miner's one-second re-assert does the
  same with the tracker's best at the tip height (`miner.rs:156-172`). Test
  `arbiter_adopts_a_same_height_candidate_whatever_its_ancestry`.
- reth applies the reorg first and checks safe/finalized afterwards
  (`engine/tree/src/tree/mod.rs:1349-1356`); `on_new_head` walks back to the
  fork point with no depth bound (`mod.rs:906-984`); the disk unwind is
  `remove_blocks_above(fork_point)` (`mod.rs:1411-1420`); the only
  `too_deep_reorg` guard applies to a head that is a *canonical ancestor*
  (`mod.rs:1301-1321`), never to a side chain. Missing ancestors of a side
  chain are downloaded one block at a time with no limit
  (`mod.rs:2951-2986`, the "outdated sidechain" branch). The 64-deep
  "finalized" marker the arbiter passes only prunes in-memory side chains at
  persistence time (`state.rs:224-289`), which runs about once per 45
  blocks; it is not a rule.

**Exploit, today (A1).** Take the honest tip block at height h. Build an
alternate chain from any fork point f < h: at each height copy the canonical
block's withdrawals and anchor, put in your own transactions (a
double-spend of SOVA received in the old history, or nothing at all), and
grind `extra_data` on the block at h until its hash is lower than the honest
tip's. Announce it over `sova/1`. Every node observes it as `NewBest` at
rank 0 (`validator.rs:108-143`), the arbiter FCUs to it, reth pulls the
ancestry from you and reorganizes to f. If the honest tip is a rank-1 block
(rank 0 was slow), you do not even need to grind. Cost: one core-second and
bandwidth. Depth: bounded only by what you can serve within the epoch, or
unbounded if you pre-load the alternate chain over several epochs (reth
keeps executed side-chain blocks in memory until the next persistence
prune) and then win one tip.

**Exploit, after SIP-6 (A2/A3).** You need a signed block at every rewritten
height. Any rank's signature is valid (`SIP-6 §2.3`), so a 1,000-zat burn
per epoch gives you a key for every epoch since you started burning. At the
tip you need preference: be rank 0 in the current epoch, or be the only
signer. Then build your rank-0 block on your alternate parent. Every node
adopts it (rank beats everything), and the reorg reaches back to the fork
point. Nothing distinguishes this from an honest late win. Each rewritten
epoch's tip (10% of R, 625 SOVA at the base reward) now goes to you, so the
rewrite is profitable on its own, before any double-spend.

**Who can do it after SIP-6.** The largest burner of the moment, in every
epoch it is rank 0. On the testnet that is the keeper.

**Fix options.** See §6, F1 row. Minimum: a candidate at height h is a
candidate only if its parent is the node's canonical block at h − 1
(the sibling rule; makes the paper's sentence true). Then a parent-aware,
objective rule (§6.2) and Zcash-carried commitments (§6.3).

**Follow-up (2026-09-23): the sibling rule split the ladder scenario;
replaced by the bounded branch rule.** The sibling rule shipped first
(`446d84e`) and made any split permanent: two nodes that once held
different blocks at one height never converged again, and the nightly
ladder scenario showed it. It is replaced on `engine/sibling-rule`
(`b2635cb`) by the **branch rule** (`candidates.rs`, `CandidateTracker`):
a candidate counts if its ancestry, through blocks the node holds, meets
the node's chain, and at the fork point either the node's chain ends
there (it extends the head) or its block beats the node's block there by
(rank, hash), replacing at most `MAX_REPLACE_DEPTH = 3` of the node's
blocks. Candidates are ordered by their blocks where their branches part,
so every node holding the same blocks prefers the same branch and splits
up to three blocks deep heal. A branch forking deeper is not a candidate,
whatever its rank. Consequences: an online node never replaces a block
once three blocks are built on it, so a transaction is settled after three
more epochs (about 4 minutes), not one; the late rank-0 win is unchanged
(a one-block replacement at the tip is the common case). Before SIP-6
anyone can produce a valid block for any epoch (F5), so within those three
blocks A1 still works at no cost; after SIP-6, beating a block at the
fork point needs a better-ranked signed block for that epoch. Unchanged
and still open: a split deeper than three blocks does not heal by itself,
a joining or restarting node takes the first valid history offered (F2),
and an unobserved block of the node's own after a restart counts as the
lowest rank. Future work as named: cumulative sealer-rank fork choice
(§6.2), SIP-8 anchored burns (§6.3, draft on `docs/sip-8-anchored-burns`),
client checkpoints (§6.1.6).

### F2 (High). No fork choice between whole histories; restart, join and eclipse are first-seen

**Claimed.** Paper §6: "A node that has fallen behind fetches the missing
blocks from peers by the ordinary Ethereum sync"; §7: "A joining node …
checks the oldest mint as carefully as the newest". SIP-2: "v1 nodes
converge on first-arrival". p2p-m1 Decision 2.

**True.** Every historical block's mint and anchor are checked
(`consensus.rs`, all import paths). Nothing chooses between two histories
that both check:

- **Join.** `on_announce` hands any tip more than 33 blocks ahead to
  `request_sync`, which keeps only the highest height
  (`service.rs:322-335`, `candidates.rs:326-341`). The driver FCUs to that
  hash with zero safe/finalized once the scan covers it
  (`candidates.rs:363-415`, `main.rs:372-386`); reth's backfill downloads
  whatever chain that hash names. Equal heights are ignored, so the first
  announcement at the highest height wins.
- **Restart.** The tracker is in memory only. After a restart the first
  candidate at the tip height is `NewBest` at any rank
  (`candidates.rs:73-74`), so a peer can move a freshly restarted node onto
  any valid history with one block. Test
  `after_restart_any_rank_is_adopted_first`. (The sealer side is guarded,
  `driver.rs:303-312`; the arbiter side is not.)
- **Eclipse.** With discovery on and no checkpoints, a node whose peers are
  all the attacker's has no way to notice.

Sova transaction history is therefore final only subjectively: what a node
saw first, or what its operator was told. That is weaker than PoS with
weak-subjectivity checkpoints (which at least ships a checkpoint) and much
weaker than PoW.

### F3 (High). Catch-up wedge: one `Announce` with an absurd height stalls a joining node

`request_sync` keeps the highest target forever (`candidates.rs:333-341`);
the driver waits for the scan to reach it before any FCU
(`candidates.rs:386-396`) and `has_changed` only fires for a higher target.
A peer sends `Announce { height: 10^12, hash: random }` (within the
128/s budget, `service.rs:294-300`); every later, real target is dropped as
"not higher". A joining node never catches up until restart, and after
restart the same peer can do it again. Test
`one_bogus_announce_wedges_the_sync_driver`.

Fix: bound announced heights to `scanned + slack` before accepting a
target; expire targets that make no progress; prefer a target by the rule
of §6.2 rather than by height.

### F4 (Medium, liveness). Held-block flood drops legitimate blocks silently

Blocks above the scan watermark are parked before any validation
(`service.rs:432-439`) in a 64-entry LRU (`held`, `MAX_HELD`). A peer
sends 65 decodable blocks with `number > scanned` (garbage bodies suffice);
the LRU evicts the oldest entry silently, but its hash stays in `seen`
(`service.rs:405`, `SEEN_CAPACITY` 4,096), so a re-announcement of the
evicted legitimate block is ignored (`service.rs:315`) and `chase_parent`
skips it too (`service.rs:577`). The node misses that tip block until 4,096
other hashes pass through `seen`. Fix: remove from `seen` on eviction (as
`retry_held` does on expiry, `service.rs:549-553`), and validate the header
cheaply before parking.

### F5 (High today, closed by SIP-6). Anyone can produce a valid block for any burn epoch

Restated because it is the entry point for F1 today, and because the code
also *relies* on it: the abandoned-epoch fallback has any non-ranked node
seal rank 0's derivation after `(ranks + 2) × step`
(`driver.rs:379-411`). That fallback is a producer policy, not a validity
rule, so it adds nothing to what A1 can already do, but two honest
fallbacks or a fallback racing the real rank-0 block resolve by hash
(`candidates.rs:76`), so the genuine sealer can lose its block to an honest
node's fallback that happened to hash lower. SIP-6's null block replaces
this correctly.

### F6 (Medium). Residual producer power after SIP-6

- **Late-win option.** Rank 0 may watch rank 1's block, then seal a sibling
  with a different transaction set at any time until the next epoch is
  built (about 75 s): free MEV over one block and a one-block reorg of
  transactions users saw confirmed. Inherent to the ladder; document it.
- **Sole-signer equivocation.** If only one burner is ranked, its
  lower-hash block still wins over null (SIP-6 §2.7); combined with F1 it
  rewrites history. Fixed by the sibling rule.
- **Timestamp.** Post-merge reth has no future-timestamp check
  (`ethereum/consensus/src/lib.rs:177-208`), so a sealer can push
  `timestamp` hours ahead; monotonicity then binds every later block. SIP-6
  §2.8 pins this; ship it.
- **`prev_randao`.** Chosen by the builder (`engine/local/src/payload.rs:50`).
  SIP-6 pins it; ship it.
- **Tracker poisoning.** A well-formed block that fails execution occupies
  the epoch's best slot (`candidates.rs` module docs, caveat 1). After
  SIP-6 only the sealer's own key can do this (equivocation).

### F7 (Medium). reth's finality markers give no protection and become inconsistent

The arbiter and miner mark `head − 64` finalized and `head − 32` safe
(`main.rs:328-343`, `miner.rs:129-140`). reth uses these only to prune
in-memory side chains and to reject a canonical-ancestor head below
finalized. A side-chain reorg deeper than 64 is applied first, then the
consistency check fails (`mod.rs:1349-1356`), the arbiter retries a second
later with hashes read from the new canonical chain, and it passes. So
"finalized" on Sova is a label on the local database, not a rule. Do not
describe it as finality anywhere.

### F8 (Medium). Zcash-level interactions

- **Reorg handling (SIP-4 §7) works as specified.** `Rollback` unwinds
  expectations, candidates and the index (`expectations.rs:326-335`);
  `effective_head` holds the sealer and arbiter at `N_R` until the block at
  `N_R + 1` matches the new branch (`expectations.rs:191-209`); the miner
  re-seals a stale height (`miner.rs:188-209`); a mid-execution reorg is
  fatal-and-retried through the generation guard
  (`zcash.rs:283-300`, `zcash_index.rs:95-97, 126`). Reorgs deeper than the
  1,024-block window rescan from base (`expectations.rs:50`); the sealer's
  own follower uses a 100-block window (`main.rs:465-473`), so it rescans
  sooner than the expectations follower. Correct, just inconsistent.
- **What a Zcash reorg buys an attacker (A4).** It replaces the epoch's
  burns, ranking, mints and precompile answers on every node at once. A
  contract that delivered at `minConf = d` against a payment that a
  d-deep reorg removes has its Sova state rolled back (fine) but not its
  off-Sova effects (the contract's risk, as documented). Re-ordering burns
  across the reorg changes sealers and so the re-sealed transaction sets.
- **Testnet.** Zcash testnet hashrate is tiny and the testnet
  minimum-difficulty rule lets a block at the minimum difficulty be mined
  once 6 × 75 s have passed since the previous one, so private-chain reorgs
  of tens of blocks are affordable to one GPU. Sova testnet "finality"
  inherits that. The library's `minConf` default of 3 on testnet is far too
  low for anything with off-chain effect; say so in the docs, and treat
  every testnet security demonstration as a demonstration, not evidence.
- **Reverse direction.** Nothing Sova does reaches Zcash. Confirmed; no
  Sova state is read by any Zcash rule.

### F9 (Low). Precompile determinism

The design is sound: coverage precondition, horizon, tip-free
confirmations, generation guard, `new_stateful` (no reth cache). Residual
node-local inputs are configuration, not attack: `SOVA_EPOCH_BASE`
(`main.rs:235-243`, `zcash.rs:184`) and `SOVA_EMISSION_SCHEDULE`
(`main.rs:251-262`) are per-node env vars; a node with a different value
silently disagrees with the network (it holds or rejects every block). Move
both into the chain profile so they cannot differ.

### F10 (Observation). What is *not* broken

- Mints: a history with one altered pro-rata amount is rejected at that
  height by every node (`consensus.rs:168-170`), on every import path
  (payload, download, backfill). Verified in the code and by the existing
  `join-scenario` tampered-history test.
- Anchors: a block naming a Zcash block the node does not have is held,
  never cached invalid (`consensus.rs:223-234`), and `is_transient_error`
  is honoured on all three paths (`mod.rs:3312-3325`).
- The relay and `sova/1` never move a head (`relay.rs:290-299`,
  `service.rs:17-21`). True, and irrelevant to F1: the arbiter moves it for
  them.

## 4. What is Sova's transaction history final against?

| Adversary | Rewrite cost today | After SIP-6 | After SIP-6 + sibling rule + cumulative-rank fork choice (§6.2) | After burn-carried commitments (§6.3) |
|---|---|---|---|---|
| A1 outsider | ~0 (grind + bandwidth) | impossible (no key) | impossible | impossible |
| A2 minimal burner (1,000 zat/epoch) | ~0 | one rank-0 win at the tip, or be the only signer; then everything since it started burning | needs rank-0 keys of *every* rewritten epoch: infeasible alone | needs to out-burn the honest weight that named the history, from the fork point to now |
| A3 dominant burner | ~0 | free in every epoch it is rank 0; back to the last epoch it did not burn | back to the last epoch in which it was not rank 0 (a coalition of past top burners can go further: the long-range problem) | as A2, plus its own weight counts against it once it has voted honestly |
| A4 Zcash reorg to depth d | rewrites mints and everything at depth ≤ d | same | same | same; and it is the *only* way to remove commitments |

Compared with the systems the paper invokes:

- **Bitcoin.** Rewriting k blocks costs k blocks of hashing against the
  live majority and is refunded only on the winning chain. Sova today: k
  blocks cost nothing. Sova with §6.3: k blocks cost either a Zcash reorg
  or more ZEC destroyed than the honest burners destroyed in the same
  epochs, refunded as SOVA at the market spread. Never more than Zcash's
  cost, so the paper's "as hard as Zcash" is an upper bound Sova can never
  exceed and today does not approach.
- **PoS long-range attacks.** Sova with signatures is a PoS-like system
  whose "validators" of epoch E are E's burners, with a sunk cost of the
  burn and no slashing. Old keys can rewrite old history unless a
  weak-subjectivity checkpoint pins it. Ethereum ships one; Sova must too
  (§6.4).
- **Merge-mining and drivechains (BIP-300/301).** Those commit the
  sidechain's block hash into the parent chain so that the parent's work
  orders the sidechain's blocks, not just its inputs. Sova commits in the
  other direction only. §6.3 is the missing half: burners (who run Sova
  nodes, unlike Zcash miners) carry the commitment.

## 5. Corrections to the paper and SIPs

Wording in the paper's voice; each is a replacement for the quoted
sentence. Where the corrected wording assumes a fix, the fix is named.

**Paper, Abstract.** "so Zcash's proof-of-work orders Sova" → "so Zcash's
proof-of-work orders Sova's epochs and fixes what each one mints".

**Paper §3.** "A Sova block therefore names one Zcash history, and a Sova
history is as hard to rewrite as the Zcash history it names." → "A Sova
block therefore names one Zcash history, and no Sova history can name a
Zcash history that did not happen. The reverse does not hold. Zcash does
not name Sova, so several Sova histories can name the same Zcash chain, and
Sova chooses among them by its own rule (Section 5)."

**Paper §3.** "Fork choice follows each node's own view of Zcash. … and
finality on Sova is Zcash confirmation depth [3]." → "A Zcash
reorganization unwinds the Sova blocks that settled the replaced Zcash
blocks, and they are derived again from the replacement chain; a mint is
final at Zcash confirmation depth [3]. Among Sova blocks that settle the
same Zcash chain, a node's choice is the preference of Section 5 at the tip
and the rule of Section 10 below it."

**Paper §5.** "the node reorganizes one block, at the tip, and builds on
rank 0. Below the tip nothing is reconsidered, because the next epoch has
already been built on top." → (with the sibling rule) "the node replaces the
one block at the tip and builds on rank 0. A block is a candidate for a
height only if it extends the node's own block at the height before, so
the replacement never reaches below the tip, and the next epoch is built on
top." (Today the second sentence is false: a candidate at the tip may
extend any history, and the node follows it.)

**Paper §6, step 5.** Add after "the node moves its head to the best
candidate it knows by the preference of Section 5": "provided the candidate
extends the node's own chain".

**Paper §6.** "A node that has fallen behind fetches the missing blocks from
peers by the ordinary Ethereum sync, but only up to the Zcash height its own
Zcash node has reached, so that every block it imports meets a settlement it
can check." → add: "It checks every block it imports. Between two histories
that both check it prefers [the §6.2 rule]; until that rule ships, it takes
the first tip it is offered, and its operator should pin a recent block
hash."

**Paper §7.** After "a history in which one mint was altered is rejected at
that height by every node that syncs it." add: "A history in which the
mints are intact and the transactions differ is not rejected; it is a
different valid history, and Section 10 says what choosing between two
costs."

**Paper §10, Rewriting a mint.** Keep the first three sentences (they are
true of mints). Replace "Sova adds no proof-of-work of its own and needs
none: below the tip, its history is exactly as costly to rewrite as
Zcash's, and the probability … as computed in [6]. At the tip, the only
reorganization Sova performs by itself is the one-block replacement of
Section 5 …" with: "Rewriting a block without changing its mint is a
different matter. Every block that carries the epoch's settlement at some
rank and the epoch's anchor is valid, whatever it extends and whatever it
contains, so Zcash alone does not decide between two such histories. Sova
adds no proof-of-work of its own. At the tip, Section 5's preference
decides, and a node replaces at most the one block it extends. Below the
tip, [the §6.2 rule: a node prefers the history whose blocks were sealed
by the better ranks, so rewriting from height N needs the key of the rank-0
burner of every epoch since N]. [With §6.3: and every burn since N that
named the honest history counts against the rewrite, so an attacker must
destroy more ZEC than those burners did, or reorganize Zcash to remove
their burns.] A Sova history can never be harder to rewrite than the Zcash
history it names; the rule above says how much easier."

**Paper §10, Censoring at the tip.** After "The transaction is included by
the first sealer who is not the attacker." add "and stays included as long
as no later sealer can rewrite that block (Section 5, Section 10)."

**Paper §11.** "a preference on rank keeps every node's choice the same" →
"a preference on rank keeps every node's choice the same at the tip".

**SIP-2, Epochs.** "Sova fork choice follows the node's Zcash view: a Zcash
reorg implies the corresponding Sova reorg (finality = Zcash confirmation
depth)." → "A Zcash reorg implies the corresponding Sova reorg; a *mint* is
final at Zcash confirmation depth. Among Sova blocks on one Zcash chain,
fork choice is the preference below, restricted to siblings (a candidate
must extend the node's block at the previous height)."

**SIP-2, Ranking and sealing.** "a late rank-0 block displaces an on-time
rank-1 block via a micro-reorg bounded to the epoch" → "… via a one-block
replacement of its sibling; a block on another parent is not a candidate."

**SIP-6 §1.1, "What the attacker cannot do".** Delete "rewrite history below
the tip epoch" from the list, or make it true by adopting the sibling rule
in §2.6 and saying so.

**SIP-6 §2.6.** "the arbiter never reorgs below the tip" → "a candidate is
observed only if its parent is the node's canonical block at the previous
height; the arbiter therefore never reorgs below the tip". **§2.8** "That is
ordinary block producer power … and it lasts for one block." → true only
with the sibling rule; today the rank-0 sealer's power reaches every epoch
in which it holds a ranked key. **§4, Arbiter.** "It is unchanged. The
stale-height skip still bounds reorgs to the tip epoch." → "It gains the
sibling rule (§2.6); the stale-height skip alone does not bound reorgs."

**SIP-4 §7.** Accurate. Add one line: "This rollback is the only reorg Sova
performs because of Zcash; reorgs among Sova blocks on one Zcash chain are
SIP-2's and SIP-6's business."

**docs/design/p2p-m1.md, Decision 2.** Add: "The scan gate makes every
synced block *checkable*; it does not choose between two checkable
histories. That needs a fork-choice rule across histories and a shipped
checkpoint."

## 6. Fixes

### 6.1 Immediate (days), before any public testnet

1. **Sibling rule in observation and adoption.** `Candidate` gains
   `parent_hash`; `convert_payload_to_block` observes a block at height h
   only if `parent_hash == canonical(h − 1)` (or `h == head + 1` and
   `parent_hash == head`); the arbiter re-checks the same condition before
   every FCU (`candidates.rs:252`), as does `miner.rs:156-172`. This makes
   the paper's §5 sentence true and removes F1's free path today and
   A3's path after SIP-6. Cost: about a day plus a sim variant of
   `ladder-p2p-scenario.sh` with a divergent-parent copy (the F1
   regression; control: today's build must follow the copy).
   *Follow-up (2026-09-23): the sibling rule split the ladder scenario;
   replaced by the bounded branch rule* (see F1's follow-up): a branch
   counts if it meets the node's chain and, at the fork point, extends the
   head or beats the node's block there, replacing at most three blocks
   (`MAX_REPLACE_DEPTH = 3`); candidates are ordered where their branches
   part. The paper's §5 and §10 now state three blocks, not the tip, and
   settlement after three more epochs.
2. **Compare to the canonical block, not to a memory-only tracker.** Rank
   of the canonical block at h is recoverable from its withdrawals
   (`identify_sealer`), so the tracker can be reconstructed on demand and
   a restart no longer accepts any rank first (F2, restart).
3. **Sync-target hygiene (F3):** refuse targets above `scanned + slack`;
   expire a target that makes no progress for N polls; allow a lower
   target to replace an expired one.
4. **Held-block hygiene (F4):** evict from `seen` when the LRU evicts from
   `held`; run `validate_header` before parking.
5. **Ship SIP-6** (already accepted), with §2.8's timestamp bound and
   pinned `prev_randao`.
6. **Client checkpoints (F2, join/eclipse):** a `(height, hash)` list in
   the chain profile (`bin/sova/src/chain.rs`), refreshed per release; a
   synced history must contain them; the sync driver refuses a target
   whose ancestry contradicts one. A day of work. This is weak subjectivity
   and should be called that.
7. **Move `SOVA_EPOCH_BASE` and the emission schedule into the chain
   profile (F9).**

### 6.2 Testnet (weeks): an objective rule among Sova histories, given SIP-6

Define for a chain the vector of sealer ranks from the fork point,
`(r_f+1, r_f+2, …, r_tip)`, with null = ∞ and an equivocator demoted as
SIP-6 says. Prefer the chain with the lower **sum of ranks** (ties: lower
tip hash). Rationale: the honest chain is nearly all rank 0, so beating it
from f needs a rank-0 block at (almost) every height since f, which needs
the rank-0 burner's key of each of those epochs; burns are on Zcash, so
those keys are fixed once the epochs are buried. An attacker must have been
the top burner throughout, or assemble a coalition of past top burners
(the PoS long-range case, which the checkpoint of 6.1.6 bounds). The
lexicographic-from-fork-point variant is weaker (only the first block
matters) and should not be used.

Implementation: the tracker becomes history-aware (a candidate is an
alternate tip plus the rank vector of its ancestry back to the fork point,
computed from withdrawals as the ancestry is imported); the arbiter adopts
only if the vector wins; the sync driver applies the same rule to competing
announced tips. About 2 weeks including sims. It keeps the sibling rule for
the common case and allows a deeper reorg only when the alternate history
is objectively better ranked.

### 6.3 Mainnet (a new SIP): Sova commitments carried in burns

SIP-1 is frozen, so this is **SIP-8: Anchored burns**. A version-2 payload
adds a Sova block reference after the signal bits: `height (4 bytes) ‖
block hash (32 bytes)`, 65 bytes in all, inside Zcash's 80-byte standard
OP_RETURN limit. Version-1 burns stay valid and carry no reference. A
version-2 burn in Zcash block E that names Sova block X at height
h ≤ E − B is a **vote of weight w** (its ZEC destroyed) for every history
that contains X. Fork choice: at a fork point f, prefer the branch with the
greater total vote weight in the Zcash blocks scanned so far (GHOST over
burn weight); ties by 6.2's rule.

What it gives: rewriting from f now requires either a Zcash reorganization
that removes the honest votes (cost: Zcash's), or destroying more ZEC than
the honest burners destroyed since f, in future Zcash blocks, in public,
against burners who keep voting for the honest chain every epoch. A joining
node computes the winner from Zcash and the candidate histories with no
first-seen input. A light client (the NEAR bridge of `zec-peg-v2.md`) can
check Sova finality from Zcash headers plus burn-transaction Merkle proofs,
which closes SIP-6 §6 item 1 for practical purposes.

Who pays: nobody new. The burn already exists; the payload grows by 36
bytes. A burner that does not run a node references nothing and loses
nothing but a vote. How often: every burn, so at least every epoch with a
burn. Format is a SIP-1-style total rule: an undecodable or out-of-range
reference is a burn with no vote, never an error.

Cost: parser (`sip1.rs`), follower records references, expectations keep
per-epoch votes, the 6.2 tracker learns weights, sims. About 3 to 4 weeks.
It supersedes neither SIP-6 nor the checkpoint; all three stack.

### 6.4 Not recommended

- **A native finality gadget** (burner-weighted votes on Sova messages):
  the same security as 6.3 with a new message layer, new liveness failure
  modes, and no light-client story. 6.3 uses Zcash as the vote transport
  and inherits Zcash's liveness.
- **Chain length or "earliest-anchored" rules:** length is equal by
  construction (one block per epoch); "earliest" is arrival time, which
  SIP-2 rightly forbids.

## 7. Remediation plan

**Before the public testnet (blocking):**
1. Sibling rule in observation, arbiter and miner (6.1.1), with the F1
   regression sim and its control.
2. Canonical-block comparison, sync-target and held-block hygiene
   (6.1.2 to 6.1.4).
3. SIP-6 with timestamp bound and pinned randao (6.1.5).
4. Client checkpoints in the chain profile (6.1.6); a `sova-checkpoint`
   line in the release notes.
5. Paper §3, §5, §6, §7, §10 and SIP-2/SIP-6 corrections (Section 5 of
   this audit). The paper must not say "as hard to rewrite as Zcash" in any
   version that ships with the testnet.
6. Testnet docs: Zcash-testnet reorgs are cheap; `minConf` 3 is a demo
   number; nothing on the testnet is final in any sense.

**During the testnet:**
7. 6.2, cumulative-rank fork choice across histories, with join/restart
   sims that offer two valid histories and assert the objective winner.
8. Draft SIP-8 (6.3) and get the payload format reviewed while SIP-1 v1
   burns are the only ones on chain.

**Before mainnet (blocking):**
9. SIP-8 live from genesis; fork choice = burn-weighted votes, then
   cumulative rank, then sibling preference; checkpoints kept as the
   weak-subjectivity floor.
10. Restate §10 with the real cost formula, and publish the attack sims as
    the evidence.

## Appendix A. Demonstration tests

`crates/engine/tests/audit_fork_choice.rs` (run with
`CARGO_TARGET_DIR=… cargo test -p engine --test audit_fork_choice`):

| Test | Pins |
|---|---|
| `validity_ignores_parent_and_transactions` | Two blocks at one height, different parents and transaction roots, same copied withdrawals: both `AnchorVerdict::Match` + `Valid{rank:0}`; a rank-1 derivation is equally valid |
| `preference_ignores_parent` | Same rank, lower hash wins whatever the ancestry; rank 0 on any ancestry beats rank 1 on the canonical one |
| `arbiter_adopts_a_same_height_candidate_whatever_its_ancestry` | `run_arbiter` issues an FCU for a same-height candidate with no knowledge of its parent |
| `after_restart_any_rank_is_adopted_first` | An empty tracker accepts rank 7 as `NewBest` |
| `one_bogus_announce_wedges_the_sync_driver` | A 10^12 target blocks a legitimate 400 target for good |

The end-to-end reorg itself (reth following the FCU into a divergent
ancestry) is verified by reading reth v2.6.0 (`engine/tree/src/tree/mod.rs`
lines cited in F1); a box sim that serves a divergent-parent copy over
`sova/1` is the regression to add with fix 6.1.1 (see its control).

## Appendix B. Evidence index

- Validity: `crates/engine/src/consensus.rs:124-175`, `expectations.rs:215-257`, `validator.rs:81-144`.
- Preference: `crates/consensus/src/sealer.rs:35-49`, `crates/engine/src/candidates.rs:53-93`.
- Adoption: `candidates.rs:219-300`, `bin/sova/src/main.rs:302-351`, `crates/engine/src/miner.rs:148-172, 252-268`.
- Catch-up: `candidates.rs:307-415`, `p2p/service.rs:314-337, 576-605`, `main.rs:358-388`.
- Held blocks: `p2p/service.rs:429-439, 510-574`.
- Abandoned epoch: `driver.rs:379-411`.
- Zcash reorg: `expectations.rs:161-209, 326-335`, `miner.rs:183-209`, `crates/consensus/src/follower.rs:151-215`, `crates/evm/src/zcash.rs:283-300`, `zcash_index.rs:95-97, 121-143`.
- reth: `engine/tree/src/tree/mod.rs:906-984` (`on_new_head`), `1288-1366` (`apply_chain_update`), `1376-1420` (`handle_missing_block`, `remove_blocks`), `2951-2986` (sidechain download), `3312-3325` (transient errors); `state.rs:224-330` (finalized pruning); `ethereum/consensus/src/lib.rs:177-208` (timestamp).
