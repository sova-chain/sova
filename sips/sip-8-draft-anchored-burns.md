# SIP-8: Anchored Burns

- Status: **Accepted** (Rob, 2026-09-23): a mainnet requirement, from
  genesis; on testnet it switches on at an activation epoch after SIP-6 and
  the cumulative-rank rule are live. Reference = the full 32-byte hash.
  The other decisions of §11 take their recommended defaults. Not yet
  implemented. Until it activates, fork choice keeps the bounded branch
  rule (at most 3 blocks replaced) and nodes report `safe` at 3 blocks and
  `finalized` at 10 (Rob, 2026-09-23); once votes count, depth follows
  §2.4 and the confirmation margin of §2.6. The paper keeps SIP-8 as named
  future work until it is built (§11 item 14 applies then).
- Numbering: **SIP-8.** SIP-5 is withdrawn (wrapped ZEC is a Sova Labs
  product, `docs/design/wz-cash.md`), SIP-6 is
  sealer signatures, SIP-7 is Zcash pool state. The audit names this SIP
  (`docs/audits/2026-09-23-reorg-and-fork-choice.md` §6.3, remediation
  items 8 and 9).
- Implementation: none. Planned homes are `crates/consensus/src/sip1.rs`
  (payload version 2 and the extended recognition rule),
  `crates/consensus/src/{follower,epoch}.rs` (burns carry their
  reference), a new `crates/engine/src/votes.rs` (per-Zcash-block vote
  store with rollback), `crates/engine/src/candidates.rs` (fork choice),
  `crates/engine/src/p2p/service.rs` (fetching voted blocks),
  `crates/burn-wallet` and `crates/burn-wallet/miner` (building v2 burns),
  and `bin/sova` (wiring, RPC).
- Author: Sova (orchestrated draft)
- Depends on: SIP-1 (**Frozen**; unchanged, extended by a new payload
  version), SIP-2 (ranking, mint and the sealing ladder unchanged; fork
  choice extended), SIP-4 §1 and §7 (the anchor and the Zcash-reorg
  rollback), SIP-6 (Accepted, not yet implemented; should ship first),
  and the cumulative-rank rule of `docs/design/f2-join-and-restart.md`
  §C, which SIP-8 uses as its tie-break.
- Consensus change: **yes.** Transactions that SIP-1 does not recognize
  become burns (so new mints appear), and fork choice changes. It
  activates from genesis on mainnet and at a fork height or a reset on
  the testnet (§9).

## Summary

Each Sova block names the Zcash block it settles. Nothing names the Sova
block. So Zcash fixes which epochs exist and what each one mints, but not
which of several valid Sova histories is the real one, and a node that
joins, restarts or is eclipsed picks between them by what it saw first
(audit F2).

SIP-8 closes that gap with data Zcash already carries. A **version-2
burn** is the same SIP-1 burn with 36 more bytes in its one OP_RETURN
output: the height and hash of the Sova block the burner's node was
building on. That reference is a **vote**, weighted by the ZEC the burn
destroys. Every node reads the votes from its own Zcash node, exactly as
it reads burns today, and prefers the Sova history that the most
destroyed ZEC has voted for (GHOST over burn weight). Below the tip, the
choice between two Sova histories becomes a function of the Zcash chain
and nothing else. To rewrite Sova from a fork point, an attacker must
either reorganize Zcash to remove the honest votes, or destroy more ZEC,
in public, than the honest burners have destroyed since that point.

The burn stays an ordinary transparent Zcash transaction. It is
permissionless to make and recognized by a total rule anyone can apply.
The payload script grows from 29 to 65 bytes, well inside Zcash's
83-byte relay limit, and a typical burn costs one more ZIP-317 logical
action (5,000 zat). Version-1 burns stay valid and mint as before. They
simply carry no vote.

## 1. Motivation

### 1.1 What Zcash fixes, and what it does not

From the audit (§2): the anchor, the set of burns per epoch, the ranking
and every burner's pro-rata amount are fixed by Zcash, because a Sova
block names Zcash block `E_N` by hash and every node re-derives the mint
from its own zebrad. What is not fixed is everything else a sealer
chooses: `parent_hash`, the transactions, the timestamp, the
beneficiary, and which rank's derivation the block carries. Many Sova
histories can name the same Zcash chain. In the audit's words: **the
mint is anchored; the ledger is not.**

The sibling rule (fix 6.1.1, on `engine/sibling-rule`) and SIP-6 close
the cheap tip attacks for an online node that already holds the honest
chain. They do not give a node a way to *choose* between two whole
histories that both check.

### 1.2 The first-seen problem (audit F2)

- **Join.** The sync driver acts on the highest target its scan covers
  (`candidates.rs` `actionable_target`, on `engine/sibling-rule`), and
  reth backfills whatever chain that hash names. Between two targets at
  the same height, the later announcement replaces the earlier one
  (`remember_target` keys targets by height).
- **Restart.** The candidate tracker lives in memory. After a restart,
  the first attached candidate at the tip height is `NewBest` at any
  rank (`after_restart_any_rank_is_adopted_first`).
- **Eclipse.** A node whose peers are all the attacker's sees only the
  attacker's history and has no way to notice.
- **Partition.** With the sibling rule, two nodes whose chains differ
  below the tip never reconverge: each ignores the other's blocks as
  unattached.

In each case the node's choice depends on what arrived first, from whom,
or on what its operator was told. Client checkpoints and a cumulative
sealer-rank rule (the interim measures in
`docs/design/f2-join-and-restart.md`) narrow this. Checkpoints trust
whoever publishes them. The rank rule trusts that past top burners do
not collude. Neither gives a node an answer it can compute from Zcash.

### 1.3 Why burns are the right carrier

Merge-mined sidechains commit the sidechain's block hash into the parent
chain so the parent's work orders the sidechain's blocks. Zcash miners
have no reason to run Sova, so asking them to commit anything is not
available. Sova's burners are different. They already put one
transaction into every epoch, they already pay for an OP_RETURN, and
they are the participants who run Sova nodes. SIP-8 has them carry the
commitment in the transaction they already send. No new message layer,
no new party, and every vote is ordered and timestamped by Zcash's
proof-of-work.

## 2. Specification

### 2.1 Payload version 2

A version-2 payload output is value-0 with script exactly
`OP_RETURN OP_PUSHBYTES_63 <payload>` (`0x6a 0x3f` then 63 bytes: 65
bytes in all; no PUSHDATA alternates, no trailing bytes). Payload layout,
63 bytes:

| bytes | field |
| --- | --- |
| 0–1 | magic `"SV"` (0x53 0x56) |
| 2 | version = 0x02 |
| 3–22 | EVM address credited |
| 23–26 | signal bits, big-endian u32 (the same bits as SIP-1) |
| 27–30 | reference height: a Sova block number, big-endian u32 |
| 31–62 | reference hash: that Sova block's hash, 32 bytes |

Bytes 0–26 are laid out exactly as SIP-1's, apart from the version byte,
so every existing field sits at the same offset. The reference hash is
the Keccak block hash in the byte order the Ethereum JSON-RPC prints
(`eth_getBlockByNumber(..).hash`). Unlike Zcash hashes, it is not
reversed.

### 2.2 Recognition rule (total)

SIP-1's rule is unchanged. SIP-8 only turns some transactions that SIP-1
classifies as "not a burn" into burns. Let `P1` be the number of
well-formed SIP-1 payload outputs in the transaction and `P2` the number
of well-formed version-2 payload outputs (§2.1, magic and version
checked). Let `w` be the summed eater value, as in SIP-1.

| `P1` | `P2` | `w ≥ 1,000` | Result |
|---|---|---|---|
| 1 | any | yes | **v1 burn** (SIP-1, unchanged): no vote, whatever else the transaction carries |
| ≥ 2 | any | any | not a burn (SIP-1, unchanged) |
| 0 | 1 | yes | **v2 burn**: credited, weighted and ranked exactly as a v1 burn, plus a reference |
| 0 | ≥ 2 | any | not a burn (ambiguous) |
| 0 | 0 | any | not a burn |
| any | any | no | not a burn |

Before the activation epoch (§9), version-2 outputs are ignored and only
SIP-1 applies. A v2 burn's address, signal bits and weight enter SIP-2
exactly as a v1 burn's do. **Ranking, the mint and the sealing ladder do
not look at the reference.**

Why a transaction carrying both a v1 and a v2 payload is a v1 burn and
not "ambiguous": that keeps SIP-8 strictly additive. Every transaction
SIP-1 recognizes keeps the same credit, weight and signal bits. Such a
transaction is also non-standard (Zebra relays at most one OP_RETURN
output, §3), so it will be rare.

### 2.3 Votes

A v2 burn confirmed in Zcash block `E` (so in epoch `E`, whose Sova
block is `N(E) = E − B + 1`) with reference `(h, H)` is a **vote of
weight `w` for block `H`** iff

```
1 ≤ h ≤ E − B        (that is, h ≤ N(E) − 1)
```

Otherwise it is a burn with no vote. That is never an error. The upper
bound says a burn can only vote for a block that could have existed
before the burn's own Zcash block: block `N(E)` is sealed only after `E`
is mined. A reference to height 0 (genesis) is not a vote, so a burner
without a Sova node can use the v2 format with a zero reference.

The node records every vote as `(zcash_height E, h, H, w)`, keyed by `E`
like the SIP-4 index, so a follower `Rollback` removes exactly the votes
of the unwound Zcash blocks.

A vote is a statement about block `H` and everything under it. It does
not say whether `H` is valid. A vote for a block the node rejects (a bad
mint, a bad seal, a wrong anchor on the node's Zcash chain), or for a
descendant of one, counts for nothing.

### 2.4 Fork choice

Definitions, over the node's own view:

- `T`: the Sova blocks the node holds and has validated, rooted at `R`,
  which is the node's highest client checkpoint, or genesis.
- `V`: the votes in Zcash blocks `B ..= Z`, where `Z` is the node's
  scanned Zcash tip.
- `W(X)` for `X ∈ T`: the sum of `w(v)` over votes `v ∈ V` whose
  referenced block is in `T` and is `X` or a descendant of `X`.
- `U(h)`: the sum of `w(v)` over votes whose referenced block is not in
  `T` and whose reference height is at least `h`. This is
  **unattributed weight**, votes for blocks the node does not have.

**Rule.** Start at `R`. At each block, move to the child `c` with the
greatest `W(c)`. Between children with equal `W`, apply in order:

1. the cumulative sealer-rank comparison of their preferred chains
   (`docs/design/f2-join-and-restart.md` §C: lower sum of bounded rank
   scores over the common height range, then the longer chain);
2. SIP-2/SIP-6 preference of `c` itself (rank ascending, then hash
   ascending);
3. the child already on the node's canonical chain, if any.

Stop at a block with no valid children. That block is the head.

Consequences:

- **At the tip nothing changes.** A fresh block has no votes yet, and
  votes for block `N` can only land in Zcash block `E_{N+1}` or later.
  So the newest one or two blocks are chosen exactly as SIP-2 and SIP-6
  choose them today: by rank, and a late rank-0 block still displaces an
  on-time rank-1 sibling. What SIP-8 adds is that this window closes as
  soon as votes for the sibling land in Zcash.
- **Below the tip, Zcash decides.** Two nodes with the same Zcash chain
  and the same set of available blocks compute the same head, whatever
  order the blocks arrived in and whoever sent them. There is no
  first-seen input anywhere in the rule.
- **The sibling rule becomes a special case.** A candidate that does not
  extend the node's chain is not ignored. It is adopted exactly when its
  branch carries more votes. With no votes anywhere, the tie-breaks
  reduce to the cumulative-rank rule and then to the sibling rule
  (tie-break 3 keeps the incumbent).
- **No protocol reorg depth limit.** A reorg is as deep as the votes
  make it. Client checkpoints (`R`) bound it from below. The node alerts
  on any vote-driven reorg deeper than a configurable depth (default
  32).

The sealer of block `N` builds on the head computed after its follower
has applied Zcash block `E_N`, which carries the votes that burners cast
while `N − 1` was the tip. If some of those burners waited for block
`N − 1` before broadcasting (§6), the choice of parent is settled by
their votes. Otherwise it is settled by rank, as today, and the votes
arrive one epoch later.

### 2.5 Blocks the node does not have

A vote names a block by hash. If `H` is unknown, the node:

1. requests `H` by hash from its `sova/1` peers (`GetBlock`), then its
   ancestors back to a block it holds (the existing orphan chase in
   `service.rs`, `chase_parent`), handing gaps deeper than
   `MAX_ANCESTOR_DEPTH` to the sync driver;
2. counts the vote in `U(h)` until `H` and its ancestry are validated,
   and then in `W`;
3. never expires the vote. Availability is a local observation, and a
   rule that dropped votes for "unavailable" blocks would make fork
   choice depend on it again.

This is what lets **a joining or restarting node pick the canonical
chain from Zcash**: it scans Zcash (as today, p2p-m1 Decision 2), builds
the vote table, fetches the headers of the most-voted references from
any peers by hash, computes the rule over the header tree, and then
backfills and validates the winning branch. Peers supply data whose
hashes Zcash has already committed. An eclipsing peer can withhold
blocks but cannot substitute a history: the node sees votes it cannot
attribute and says so (§2.6), instead of silently following a
lighter history. The part not covered is the newest one or two blocks,
which no burn has voted on yet.

### 2.6 Burn confirmation (node RPC, not consensus)

For a block `X` on the node's head chain, the **margin** is

```
m(Y) = W(Y) − Σ_{S sibling of Y} W(S) − U(height(Y))
M(X) = min over Y on the path from R to X (inclusive, excluding R) of m(Y)
```

`M(X) > 0` means no set of votes cast so far, known or not, can make a
different block win at `X`'s height or below it. Overturning `X` then
needs new burns totalling more than `M(X)` zat, or a Zcash reorg that
removes the votes that make up the margin. `bin/sova` serves
`sova_burnConfirmation(blockHash) → { marginZat, weightZat,
unattributedZat, zcashHeight }`. Wallets, exchanges and bridge watchers
should use the margin instead of Sova block depth. The arbiter marks a
block `safe` or `finalized` for reth only when its margin is above a
configured floor, never at a fixed depth (audit F7).

The margin is deterministic given the Zcash chain and the executing
block's own ancestry. That makes a later precompile method possible
(§11, decision 13). It is not part of this SIP.

## 3. Format: alternatives evaluated

SIP-1 is Frozen, so the reference has to be additive. Two shapes are
possible: a new payload version in the one OP_RETURN output, or a
second output next to the unchanged v1 payload.

**A. New payload version (recommended).** §2.1. A 65-byte script is
under Zebra's default 83-byte datacarrier cap
(`zebrad/src/components/mempool/config.rs:78`,
`DEFAULT_MAX_DATACARRIER_BYTES`, which counts the opcode and push
bytes), with the single-byte push `OP_PUSHBYTES_63`. One output, one
strict parser, one place for every Sova field. The cost is that nodes
without SIP-8 do not see v2 burns at all, so activation is a hard fork
of the mint rule. Sova activates it at genesis on mainnet and at a fork
height or reset on the testnet, so that cost is paid once.

**B1. A second OP_RETURN output carrying only the reference.** The v1
payload is untouched, old nodes see the same burns, and the reference
would affect fork choice only, which is not even a fork. But
**Zebra's mempool rejects a transaction with more than one OP_RETURN
output** (`zebrad/src/components/mempool/storage.rs:369`,
`NonStandardTransactionError::MultiOpReturn`; zcashd has the same
policy). Such burns would reach Zcash blocks only through a miner who
accepts them directly. That makes burning permissioned in practice.
Rejected.

**B2. A companion output shaped as P2PKH or P2SH whose 20-byte hash is
the commitment.** Standard, if its value clears Zebra's dust threshold
(`3 × (100 × (34 + 148) / 1000) = 54` zat for a 34-byte output,
`zebra-chain/src/transparent.rs:369-380`). But it carries 20 bytes, not
36, so it needs a truncated hash with no height or two outputs. It adds
an unspendable entry to every Zcash node's UTXO set for every burn,
forever. Its value is lost without counting as weight. And a rule has
to say which of several payment-shaped outputs is the commitment.
Rejected.

**B3. A separate companion transaction naming the burn's txid.** A
second full transaction fee per epoch, and the vote must be
authenticated as the burner's (spend the burn's change, or sign with its
key), or anyone could spend someone else's weight. Rejected.

**C. Reuse SIP-1's 32 signal bits as a truncated block hash.** No format
change at all, but 32 bits is grindable: a sealer can make two blocks
with the same 32-bit prefix in about 2^32 header hashes, which is seconds
of work, and split the vote. It would also take the bits that C7's
upgrade signaling needs. Rejected.

**Reference length.** The full 32-byte hash is recommended. A 26-byte
hash prefix (payload 57 bytes, script 59, output 68) keeps the usual
1-in/3-out burn at exactly four ZIP-317 actions, the same fee as v1
(outputs 68 + 34 + 34 = 136 bytes, `⌈136/34⌉ = 4`), and 208 bits is far
beyond collision reach. The cost is operational: `sova/1` and eth-wire
fetch blocks by full hash, so a node that sees a vote for an unknown
prefix cannot simply ask for it. It must ask peers for blocks at that
height and match. With the full hash, the fee difference is 5,000 zat
per burn (§8). Decision 3.

## 4. Interactions

### 4.1 SIP-1 (Frozen)

Unchanged, byte for byte and rule for rule. Every v1 burn is recognized
exactly as before (§2.2). SIP-1's text stays as it is. An editorial
pointer ("extended by SIP-8, payload version 2") in its header is for
Rob to allow or not, since SIP-1 is frozen. `crates/consensus/src/sip1.rs`
keeps its v1 types, and `extract_burn` gains the v2 branch and returns
the reference as `Option<SovaRef>`.

### 4.2 SIP-2 (ranking, settlement, the ladder)

- **Ranking** is by `(weight desc, min-txid asc)` over all burns, v1 and
  v2 together. The reference plays no part.
- **Settlement** is unchanged: the withdrawals of every rank's
  derivation, the 10% tip, gwei-exact conservation.
- **The ladder** is unchanged: rank `r` may seal once `r × step` has
  passed. It remains liveness-only.
- **Preference** among an epoch's candidates, `(rank asc, hash asc)`,
  is now tie-break 2 of §2.4. It decides whenever votes do not, which
  in practice means the newest block or two.
- SIP-2's "micro-reorg bounded to the epoch" becomes "bounded to the
  window before votes for the displaced block land in Zcash". That caps
  the late-win option (audit F6) at about one Zcash block.

### 4.3 SIP-4 §7 (Zcash reorgs)

Votes are Zcash data, so they roll back with Zcash. On
`Rollback { to_height: R }`:

1. the vote store drops every vote from Zcash blocks above `R` (the same
   keyed deletion the SIP-4 index performs);
2. Sova unwinds to `N_R = R − B + 1` as SIP-4 §7 specifies, and
   `effective_head` holds the sealer and arbiter there until `N_R + 1`
   matches the new branch (unchanged);
3. as the replacement Zcash blocks are scanned, their votes are added,
   and fork choice is recomputed from `R`.

Two consequences, both intended and both to be documented:

- **A Zcash reorg can move Sova fork choice below `N_R`.** If the votes
  in the removed Zcash blocks were what made one Sova branch heavier
  than another, and the replacement blocks carry different votes, the
  node may reorganize Sova deeper than the Zcash reorg. It only happens
  where the margin (§2.6) was smaller than the weight of the removed
  votes. It is the same computation every node makes, so all nodes move
  together.
- **A re-mined burn is re-checked in its new block.** Its vote range
  (§2.3) is evaluated against the Zcash height it now sits at. If the
  block it referenced settled a Zcash block that the reorg replaced,
  that block is gone and the vote counts for nothing.

What the vote range guarantees: a burn broadcast while the Zcash tip was
`E_c` references at most `N(E_c) = E_c − B + 1`, and it can only be
mined at `E ≥ E_c + 1`, where the bound is `E − B ≥ N(E_c)`. So an
honest burn's reference is always in range, unless a Zcash reorg makes
the chain shorter under it.

SIP-4 §1's anchor rule is untouched. Votes never make a block valid, and
a block held for an unknown or off-fork anchor is not in `T` (§2.4).

### 4.4 SIP-4 precompile

`burnInfo(txid)` uses the consensus parser (`sip1::extract_burn`), so it
recognizes v2 burns after activation with an unchanged ABI: `(status,
credited, signal, weightZat)`. A method that returns a burn's reference,
and one for the margin of §2.6, are possible later additions that need
their own review for determinism.

### 4.5 SIP-6 (sealer signatures; Accepted, not implemented)

- **Votes name signed blocks.** The block hash covers the seal
  (SIP-6 §2.1), so a vote is for one exact signed block, never for "any
  block with these withdrawals".
- **SIP-8 does not depend on SIP-6 for its main guarantee.** Before
  SIP-6, anyone can build valid copies of a block (audit F5), but a copy
  has a different hash and gets no votes unless someone burns for it.
  SIP-6 is still needed at the tip, where votes have not arrived yet,
  and it keeps outsiders from filling the tree with valid siblings. Ship
  SIP-6 first.
- **Equivocation.** SIP-6 demotes an equivocating sealer in
  *preference*. Votes come before preference, so once burners have voted
  for one of an equivocator's blocks, that block stands. Equivocation
  therefore matters only in the tip window, which is where SIP-6 already
  handles it.
- **Null blocks** can be voted for like any block. See §5.2 item 9 for
  what that allows a vote majority to do.
- **Light clients** (SIP-6 §6). A client that tracks Zcash headers can
  verify votes *for* a block with Merkle proofs of the burn
  transactions. It cannot prove that no heavier competing votes exist
  without every burn in each Zcash block. That is the same completeness
  gap SIP-6 §6 describes for rank, and the same optimistic answer
  applies: accept after a challenge window unless someone proves more
  weight for a competing block. The gap narrows from "proof of rank at
  every height" to "proof of a heavier vote set", which is one Merkle
  proof per competing vote.

### 4.6 Interim F2 measures

Client checkpoints stay as the floor `R` of the rule, so the node never
walks the tree below them. With SIP-8 they are a convenience and a
bootstrap sanity check, not the only defense against long-range
rewrites. The canonical-block comparison on restart is subsumed: a
restarted node recomputes the head from votes. The cumulative-rank rule
is tie-break 1. See `docs/design/f2-join-and-restart.md`.

### 4.7 Signal bits (C7)

Unchanged, at the same offset in both versions. A C7 tally counts v1 and
v2 burns alike.

## 5. Security

### 5.1 What a rewrite costs

Let `f` be the fork point, and `W_hon(f)` the weight of votes in Zcash
blocks so far that reference honest blocks above `f`. To make every node
switch to an alternate branch from `f`, an attacker needs one of:

- **a Zcash reorganization** that removes enough of those votes, paid at
  Zcash's price (attacker A4), possibly combined with its own burns in
  the replacement blocks; or
- **new burns referencing its branch totalling more than `W_hon(f)`**,
  confirmed in Zcash blocks after the fork, in public, while the honest
  burners keep voting for the honest branch every epoch.

With a fraction `ρ` of each epoch's burn weight in v2 burns, an average
epoch weight `W̄`, and a reference lag of `L` epochs (§6), a block that
has been buried `k` epochs has `W_hon ≈ (k − L) · ρ · W̄` behind it.
*This is an estimate that assumes stable burn weight.* A sustained
attacker holding a fraction `α > 1/2` of each epoch's burn weight gains
on the honest branch at `(2α − 1) · W̄` per epoch. That is the SIP-8
equivalent of a 51% miner, and it should be described that way.

The attacker's burns are not wasted: each one also earns its pro-rata
share of its epoch's mint, on every branch. So the net cost of a rewrite
is the ZEC destroyed minus the SOVA it mints at market. A one-epoch dump
of `k · W̄` collects at most one epoch's reward, so its net cost is
roughly `(k − 1)` epochs of honest weight *(estimate, at the equilibrium
where burners roughly break even)*.

Audit §4's table, extended:

| Adversary | After SIP-6 + sibling rule + cumulative rank | After SIP-8 |
|---|---|---|
| A1 outsider | impossible | impossible |
| A2 minimal burner | needs the rank-0 keys of every rewritten epoch | needs more vote weight than the honest burners since `f`; its 1,000 zat per epoch does not come close |
| A3 dominant burner | back to the last epoch in which it was not rank 0; a coalition of past top burners goes further | needs more vote weight than everyone else since `f`, including its own past votes if it voted honestly; a sustained majority is a 51% attack |
| A4 Zcash reorg to depth `d` | rewrites mints and everything at depth ≤ `d` | same; and it is the only way to *remove* votes |

### 5.2 Grinding, withholding, and the rule's response

1. **Block-hash grinding at the tip.** Unchanged by SIP-8 and closed by
   SIP-6. Below the tip, hashes decide nothing: votes decide.
2. **Reference grinding.** A burner chooses what to reference, but
   cannot create weight by choosing. The 36 new bytes are one more free
   field for grinding a burn's txid (SIP-2's rank tie-break), which the
   change output and amounts already allowed. No change.
3. **Vote splitting at the tip.** An equivocating sealer, or two ranks'
   blocks racing, can split the next epoch's votes. An attacker then
   adds weight to pick the winner. The harm is the choice between two
   sibling blocks the network already saw, a one-block reorg of the
   kind audit F6 describes. With SIP-6, an equivocator also loses
   preference in the tip window.
4. **Withholding a block while voting for it** (a private branch). The
   votes are public, so every node sees them as unattributed weight
   `U`, and every margin (§2.6) drops accordingly. When the branch is
   released, it wins only if its weight wins, which costs exactly what a
   public attack costs. Delay changes when the reorg happens, not
   whether the attacker can afford it. **Residual:** a party that relies
   on block depth instead of the margin can be surprised. The RPC and
   docs must make the margin the thing people check.
5. **Withholding votes.** A large burner that burns v1 (or references
   nothing) does not add to `W_hon`. It can later vote as it likes. Its
   past silence lowers `ρ`, and lowers the cost of a rewrite. The
   reference client emits v2 by default (decision 9) so that this takes
   deliberate action.
6. **Sudden weight.** Saving up ZEC and burning it in one epoch against
   a deep fork point. The cost is the full `W_hon(f)`, and it grows with
   depth. There is no discount for timing.
7. **Vote buying.** A burner's own mint is paid on every branch, so it
   has little direct stake in which branch wins, and an attacker could
   pay burners to reference its branch. The protocol has no lever here
   that does not make mints branch-dependent, which would give up the
   anchored mint (audit finding 5). **Residual, stated plainly:** SIP-8's
   security assumes most burn weight runs the default client or has a
   stake in the honest ledger (holdings, contracts, reputation). That is
   the same kind of assumption Bitcoin makes about miners, with weaker
   incentives behind it.
8. **Zcash-miner censorship of votes.** A Zcash miner can leave v2 burns
   out of its blocks, as it can leave out any burn today. The burns land
   in the next honest miner's block. Votes are delayed, not lost.
9. **A vote majority can erase mints.** SIP-6 makes a null block valid
   (at the lowest preference) in a burn epoch. A branch of null blocks
   needs no keys. An attacker that out-burns the honest votes since `f`
   can therefore make a null-block branch win and erase the mints of
   every burn epoch it replaces. The threshold is the same as for any
   rewrite, and an attacker at that scale also collects most mints of
   the epochs in which it burned. **Residual:** after SIP-8, "a mint
   cannot change without a Zcash reorg" becomes "a mint cannot change
   without a Zcash reorg or an out-burn of every honest vote since that
   epoch". The paper must say so.
10. **Long range.** Old sealer keys (SIP-6) are not enough to rewrite
    old history, because the honest votes accumulated since then count
    against it. This is the property PoS lacks without checkpoints.
    Checkpoints remain as a floor.
11. **Spam.** Each vote costs a real burn (at least 1,000 zat plus about
    25,000 zat of fee). A node stores 44 bytes per vote. Fetch attempts
    for unknown hashes are rate-limited per peer and per reference, and a
    reference no peer can serve stays in `U` without further cost.

## 6. Miner side

**The burner must know the Sova block it builds on.** Today
`sova-miner mine` talks only to zebrad: it waits for a new Zcash block,
then builds and broadcasts one burn
(`crates/burn-wallet/miner/src/mine.rs`, `run`). SIP-8 adds:

- **`--sova-rpc <url>`.** Before building the burn, call
  `eth_getBlockByNumber("latest", false)` and take `number` and `hash`.
  "Latest" on the burner's own node is that node's fork-choice head.
- **A freshness check.** The head's anchor epoch, `number + B − 1`, must
  be within 2 of the Zcash tip. Otherwise the node is behind or stuck,
  and its head is not worth voting for. Then the miner sends a v1 burn,
  or v2 with a zero reference, and warns.
- **No Sova RPC configured:** v1, as today. Burning never waits on Sova.
- **Payload and fee.** `crates/burn-wallet/src/tx.rs`
  `build_burn_transaction` takes an optional reference and emits the v2
  payload through `add_null_data_output`. The fee uses the 74-byte
  payload output (`crates/burn-wallet/src/fee.rs`).
- **Activation guard.** The miner refuses to send a v2 burn before the
  network's activation epoch. A v2 burn mined earlier is not a burn
  (§2.2): its ZEC is destroyed and nothing is minted.
- **Trust.** A vote is only as good as the node it came from. A burner
  that points `--sova-rpc` at someone else's node hands its vote to that
  operator. The docs must say this plainly, and the MCP miner tools
  (`mcp/src/minerCli.ts`) must pass the flag through and default to the
  agent's own node.

**Latency and staleness.** Zcash block `E_N` arrives. Sova block `N` is
sealed a few seconds later at rank 0, or `r × 15 s` later at rank `r`.
The burn that the miner broadcasts for epoch `E_{N+1}` can reference:

- **`N − 1`, if it broadcasts at once** (today's behavior). Reference lag
  `L = 2`: block `N` gets its first votes in `E_{N+2}`.
- **`N`, if it waits for block `N`.** `L = 1`. The risk is that
  `E_{N+1}` is found while it waits, so the burn lands in `E_{N+2}`. It
  is not lost, just one epoch later (and its vote is then one behind).
  With exponential 75-second blocks, the chance is `1 − e^{−t/75}`: 2.6%
  for a 2-second wait, 6.4% for 5 s, 12.5% for 10 s, 18% for 15 s. A
  late broadcast can also miss the template a Zcash miner is working on.

Stale votes are harmless. A vote for `N − 1` counts for every ancestor,
so it protects everything at `N − 1` and below. It just says nothing
about `N` and its siblings. The cost of lag is that the newest `L`
blocks are chosen by rank and not by votes. Recommended default:
**wait for the next Sova block for up to 10 s, then reference whatever
the head is** (`--vote-wait 10`). Decision 4.

**Fees.** For a 1-in/3-out burn (payload, eater, change), v1 outputs
total 38 + 34 + 34 = 106 bytes (4 logical actions, 20,000 zat), and v2
outputs total 74 + 34 + 34 = 142 bytes (5 actions, 25,000 zat). Without
change the figures are 3 and 4 actions. So a v2 burn costs **5,000 zat
more** in either shape. A floor miner burning every epoch (1,152 epochs
a day) goes from about 0.24 ZEC a day to about 0.30.

## 7. Node side

- **`crates/consensus`.** `sip1.rs`: `BurnPayloadV2`, `SovaRef { height:
  u32, hash: [u8; 32] }`, and the §2.2 table in `extract_burn`, with the
  activation epoch as a parameter. `epoch.rs`: `EpochBurn` gains
  `reference: Option<SovaRef>`. `follower.rs`: nothing new, since it
  already hands every output to the parser.
- **`crates/engine/src/votes.rs` (new).** A per-Zcash-height vote store
  with `apply(EpochData)` and `rollback(to_height)`, fed by the same
  follower events as `expectations.rs` and `zcash_index.rs`. It is
  persisted, so a restart does not rescan for votes.
- **`crates/engine/src/candidates.rs`.** The tracker becomes a block
  tree (it already stores `parent` per `Seen` on `engine/sibling-rule`),
  with per-block vote weight and subtree sums. `best(height)` stays for
  the tip. A new `head()` runs §2.4. `attached()` stays as the fast path
  when no votes differ. `run_arbiter` FCUs to `head()` whenever a block
  is validated, an epoch's votes are applied, or a rollback happens.
  `run_sync_driver` takes its target from `head()` over voted headers
  instead of the highest announced height. `miner.rs`'s one-second
  re-assert uses the same `head()`.
- **`crates/engine/src/p2p/service.rs`.** Voted-but-unknown hashes enter
  the fetch path (`request`, `chase_parent`) with their own per-peer
  budget. A header-first fetch over reth's `FetchClient` handles
  branches deeper than `MAX_ANCESTOR_DEPTH`.
- **`bin/sova`.** The `sova_burnConfirmation` RPC (public profile
  allowlist). The safe/finalized markers come from the margin (§2.6),
  not from `head − 32`/`head − 64`. The activation epoch lives in the
  chain profile (`bin/sova/src/chain.rs`), not an env var (audit F9).
- **Computation.** One pass per applied Zcash block: at most a few
  hundred votes, each walking up the tree to the nearest block with a
  single known child. The tree is bounded by the checkpoint floor and by
  reth's in-memory side-chain retention.

## 8. Cost

- **Bytes on Zcash.** +36 bytes per v2 burn, in an output Zebra already
  relays.
- **Fees.** +5,000 zat per burn (+1 ZIP-317 action), about a quarter
  more for a floor burn (§6).
- **Node storage.** 44 bytes per vote, about 18 MB a year at one vote
  per epoch.
- **Engineering** (*estimates*, worker-days):
  - parser, recognition table, activation and test vectors: 2
  - follower, vote store and rollback: 2–3
  - fork choice in the tracker, arbiter, miner re-assert and sync
    driver: 5–6
  - fetching voted blocks, header-first path: 3
  - burn-wallet and miner (`--sova-rpc`, `--vote-wait`, fees, guard): 2
  - RPC, markers from the margin: 1–2
  - sims (§10): 4–5

  Total: **about 4 weeks** for one worker. The audit's §6.3 estimate was
  3 to 4 weeks. It assumed the §6.2 tracker already existed, which is
  on the same path.

## 9. Activation

- **Mainnet: from genesis.** Version-2 recognition and vote-weighted
  fork choice are live at Sova height 1. The audit made this blocking
  for mainnet (remediation item 9).
- **Testnet: at an activation epoch set in the chain profile.** From
  Zcash height `A`, version-2 outputs are recognized and votes counted.
  Below `A`, SIP-1 alone applies, forever, so history replays the same
  way. Every testnet node and miner upgrades before `A`. This avoids a
  third testnet reset and exercises the fork-height machinery that C7
  signaling will need anyway. Order on the testnet: SIP-4 and SIP-6 at
  the reset, the cumulative-rank rule during the testnet, SIP-8 at `A`.
- **Alternative: bundle SIP-8 into the reset** if the reset slips far
  enough for it to be ready. That is simpler, with one rule set from
  genesis, and it moves the reset.

## 10. Test and sim plan

**Unit tests:**

- Test vectors: a fixed v2 payload has fixed bytes and a fixed script.
  The strict parser rejects PUSHDATA1 encodings, 62- and 64-byte pushes,
  version 0x03, and a wrong magic.
- The §2.2 table, one case per row, including a v1 plus v2 transaction
  (a v1 burn with no vote), two v2 outputs (not a burn), and v2 before
  activation (not a burn).
- Vote range: `h = 0`, `h = E − B`, `h = E − B + 1`.
- Fork choice: fixed trees and vote sets. The result does not depend on
  insertion order (a property test over permutations). This is the
  "no first-seen input" guarantee.
- Margin: known cases, including unattributed weight.
- Rollback: store after rollback and rescan equals a fresh store.

**Box scenarios** (`box/sim/`):

1. **Join by votes (the regression for F2).** Two peers serve two valid
   histories from a common fork point, and only one has been voted for.
   A fresh node picks the voted history whichever peer it connects to
   first. **Control:** a pre-SIP-8 build picks by connection order.
2. **Restart.** Node C restarts while a peer offers a better-ranked
   sibling on another parent. C returns to the voted chain.
3. **Dominant sealer rewrite (A3).** The rank-0 sealer builds on an
   alternate parent. Honest votes keep the canonical chain everywhere.
   Control: with the sibling rule only, nodes that restart can be moved.
4. **Out-burn.** An attacker burns more than the honest weight since
   `f`, referencing its branch, and every node switches. This shows the
   rule does what §5.1 says and prices the attack honestly.
5. **Withholding.** Votes for a hidden block: `U` and the margins
   change on every node. When the block is released, nodes switch if and
   only if it outweighs the honest branch.
6. **Zcash reorg.** A regtest `invalidateblock` removes the Zcash blocks
   carrying the decisive votes, and the replacement carries others.
   Nodes move together.
7. **Mixed miners.** v1 and v2 miners in the same epochs. Ranks and
   mints are identical to an all-v1 run, and only v2 weight votes.
8. **Activation boundary.** A v2 burn at `A − 1` mints nothing and a v2
   burn at `A` mints. Every node agrees.

## 11. Decisions for Rob

1. **Number and name.** SIP-8, "Anchored burns", strictly additive to
   the frozen SIP-1. *Recommend: yes.*
2. **Format.** A new payload version (0x02) in the single OP_RETURN:
   63-byte payload, 65-byte script, fields 0–26 as in SIP-1, then
   reference height and hash. Companion outputs rejected (§3).
   *Recommend: yes.*
3. **Reference length.** The full 32-byte hash (+5,000 zat per burn), or
   a 26-byte prefix that keeps the v1 fee but makes fetching by hash
   impossible. *Recommend: full 32 bytes.*
4. **What the miner references, and when.** Its own node's head. Wait up
   to 10 s for the next Sova block, then broadcast (`L = 1` most epochs,
   a burn slips an epoch about 12% of the time). The alternative is to
   broadcast at once (`L = 2`, no change to miner timing).
   *Recommend: wait up to 10 s.*
5. **Vote range.** `1 ≤ h ≤ E − B`. Out of range is a burn with no vote,
   never an error. *Recommend: yes.*
6. **Fork-choice order.** Votes (GHOST over burn weight), then
   cumulative sealer rank, then SIP-2/SIP-6 preference, then the
   incumbent. *Recommend: yes.*
7. **When votes count.** At the node's scanned Zcash tip, with no
   confirmation depth, the same as mints and the SIP-4 anchor.
   *Recommend: yes.*
8. **Votes for blocks the node lacks.** Reported as unattributed weight,
   never expired, subtracted from every margin. *Recommend: yes.*
9. **v1 burns.** Remain valid, mint and rank, and carry no vote. The
   reference client emits v2 by default when it has a fresh Sova node.
   *Recommend: yes. Do not deprecate v1.*
10. **Reorg depth.** No protocol cap. Checkpoints are the floor, with an
    operator alert beyond 32 blocks. *Recommend: yes.*
11. **Bind the sealer to its own reference?** A validity rule that rank
    `r`'s block must extend the block its own burn referenced. It would
    stop a sealer from building on an alternate parent without committing
    to it on Zcash first. But an honest sealer whose reference lost a
    late-win race could no longer seal on the winning chain, and validity
    would depend on ancestry. *Recommend: no. Votes already cover this.*
12. **Activation.** Mainnet from genesis. Testnet at an activation epoch
    in the chain profile, after SIP-6 and the cumulative-rank rule are
    live. *Recommend: yes*, or bundle into the reset if it slips.
13. **Burn confirmation for contracts.** The margin of §2.6 is served by
    node RPC now. A precompile method is a later, separately reviewed
    addition. *Recommend: RPC now, precompile later.*
14. **Language.** The paper's "Sova changes nothing in Zcash and posts
    nothing to it" becomes "Sova changes nothing in Zcash; the burns
    that mint SOVA also carry a reference to the Sova block they build
    on". §10's rewrite cost is restated with §5.1's formula, and mints
    with §5.2 item 9. *Recommend: yes, when SIP-8 is accepted.*

## Sources

Zebra claims are checked against Zebra 6.3.0 (`f5c5277`,
`research/zebra-upstream` in the review workspace):
`zebrad/src/components/mempool/config.rs:74-78` (83-byte datacarrier
default, including opcode and push bytes),
`zebrad/src/components/mempool/storage.rs:318-372` (one OP_RETURN per
transaction, dust rule), `zebra-chain/src/transparent.rs:59, 369-380`
(dust threshold). Sova claims are checked against `release` at `620df69`
and `engine/sibling-rule` at `ffbcc66` (`crates/engine/src/candidates.rs`,
`crates/engine/src/p2p/service.rs`), and against
`crates/consensus/src/sip1.rs`, `crates/burn-wallet/src/{tx,fee}.rs` and
`crates/burn-wallet/miner/src/mine.rs` on `release`. Fee arithmetic
follows ZIP-317 as implemented in `crates/burn-wallet/src/fee.rs`.
Figures marked *estimate* are not measured.
