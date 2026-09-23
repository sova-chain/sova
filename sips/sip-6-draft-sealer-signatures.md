# SIP-6: Sealer Signatures

- Status: **Accepted** (Rob, 2026-09-23: recommended defaults in §10); implementation pending, ships at the testnet reset. Earlier: **Draft, design only**. Needs Rob's
  calls in "Decisions for Rob" (§10) before a build is dispatched.
- Numbering: **SIP-6, not SIP-5.** `sips/` holds SIP-1 to SIP-4, but
  SIP-5 is already reserved for the wZEC peg (`docs/WORKPLAN.md` row z-2,
  `docs/design/zec-peg-v2.md` §4.1, `docs/design/zec-on-sova-options.md`).
  The peg doc's "needs its own SIP" for block signing
  (`zec-peg-v2.md` §1.4(a)) is this one.
- Implementation: none. Planned homes are `crates/engine/src/consensus.rs`
  (header and block rules), `crates/engine/src/validator.rs` (payload
  conversion and candidate observation), `crates/engine/src/miner.rs`
  (signing), `crates/engine/src/{candidates,expectations}.rs` (rank from
  signer, equivocation), `crates/consensus/src/sealer.rs` (preference),
  `crates/burn-wallet/miner` (key derivation), and `bin/sova` (keystore).
- Author: Sova (orchestrated draft)
- Depends on: SIP-1 (frozen, unchanged), SIP-2 (changes its sealer
  identification and burn-less rule), SIP-3 (unchanged numbers), SIP-4 §1
  (the Zcash anchor and its hold semantics; SIP-6 must ship with or after
  it).
- Consensus change: **yes**. Activates at the testnet reset, the same as
  SIP-4. No fork logic before a public network exists.

## Summary

Today a Sova block carries no signature. Its sealer is inferred from its
withdrawals (the 1/10 tip lands on the sealer, so each rank's mint is
different). Fork choice among an epoch's candidates is (rank asc, block
hash asc). So anyone can copy the rank-0 block's withdrawals, change
everything else, and grind a lower hash. That block then wins the tip.
No light client can tell the real block from such a copy either.

SIP-6 makes the ranked sealer **sign the block** with the key of the
EVM address its burn credits. That is the identity SIP-1 already
defines. The signature goes into `extra_data` (Clique's layout: 32-byte
vanity, then a 65-byte secp256k1 signature). It signs a domain-separated
hash of the header without the signature. Validation recovers the signer
and requires it to be a ranked burner of the block's epoch. The mint must
then equal that signer's derivation. **The signature, not the tip, now
identifies the sealer.**

Epochs with no signer get a single **null block**, fully determined by
the parent and the Zcash anchor: no transactions, no mint, and fixed
header fields. This covers burn-less epochs, and also burn epochs whose
ranked burners all fail to seal. That second case halts the chain today
(§1.3). With one valid null block per parent, there is nothing to grind.
A sealer that signs two blocks for one slot is demoted below every honest
rank.

## 1. The problem

### 1.1 Tip grinding

**Who can do it.** Anyone who receives the honest block. That includes a
node with no burns, no zebrad and no key. The attacker doesn't have to
derive rank 0's mint. It copies the honest block's withdrawals list and
its `parent_beacon_block_root` (the SIP-4 anchor).

**How.** Build a block on the same parent with those exact withdrawals.
Everything else is free:

- **Transactions.** Any valid subset in any order: drop a target, insert
  the attacker's own, or sandwich.
- **Beneficiary.** The attacker takes the priority fees.
- **Timestamp.** Any value above the parent's. Sova chainspecs are
  post-merge from genesis: reth `DEV` has Paris at block 0
  (`ethereum/hardforks/src/hardforks/dev.rs:27-33`,
  `chainspec/src/spec.rs:254`). So `EthBeaconConsensus::validate_header`
  skips its only future-timestamp check, which is pre-merge only
  (`ethereum/consensus/src/lib.rs:179-208`). The only lower bound is
  `validate_against_parent_timestamp`
  (`consensus/common/src/validation.rs:352-363`).
- **`prev_randao`** (EVM `PREVRANDAO`).
- **`extra_data`** (up to 32 bytes, `lib.rs:209`,
  `validation.rs:273-281`).

`extra_data` is not visible to the EVM. Changing it therefore changes the
block hash without re-executing anything, so grinding costs one header
keccak per try. The honest hash `h` is uniform, so an attacker who tries
`N` variants fails with probability exactly `E[(1−h)^N] = 1/(N+1)`. A
million tries is about a second of one core (*estimate*) and wins
999,999 times in 1,000,000. The honest sealer can't defend by grinding
too, because the attacker can always grind more.

**Why it wins everywhere.** C5 recovers the copy's rank from its
withdrawals (`driver.rs` `identify_sealer`), and the result is the same
rank as the honest block. `candidates.rs` then picks the lower hash
(`sealer::prefer`, `sealer.rs:45-49`). Preference is deterministic, so
every node that sees both blocks adopts the copy, even if it arrived
later. The only limit is the arbiter's stale-height skip
(`candidates.rs:231-239`): the copy must land before the next epoch's
block is built on top, which gives about 75 s.

**What the attacker gains:**

- **Tip-level censorship on every epoch it bothers with.** Examples: an
  oracle update, a liquidation, a peg or escrow challenge, a withdrawal
  request, or anything on a deadline. Repeating this every epoch costs
  nothing.
- **All of the block's MEV and priority fees.**
- **Control of `timestamp` and `PREVRANDAO`** for contracts that read
  them.
- **Cheap griefing.** Each copy forces a micro-reorg at every node, and
  the honest sealer's transactions are dropped from that height.
- Burn-less epochs are worse off, because they have no rank at all. Their
  candidates compete at `usize::MAX` on hash alone
  (`validator.rs`, `candidates.rs` module docs). Anyone can produce and
  grind every burn-less block from scratch.

**What the attacker cannot do:**

- **Change the mint.** Withdrawals must equal a rank's derivation (C5),
  so every burner, and the true sealer's tip, is paid exactly as before.
- **Change the anchor** (SIP-4), rewrite history below the tip epoch,
  include invalid transactions, or change any rank.

### 1.2 The light-client gap

A light client, such as a NEAR contract in `zec-peg-v2.md` §1.4(a), needs
to tell the canonical block from a valid one that nobody chose. Today
nothing in a Sova block says who built it. Anyone can build a valid block
for a recent epoch that contains their own wZEC burn, show it to the
light client, and keep the wZEC on the real chain. A perfect execution
proof doesn't help, because the fake block is valid. It just wasn't
chosen. So **no trust-minimized bridge out of Sova is possible** until
blocks name their author. §6 says what a signature gives a light client
and what it still doesn't.

### 1.3 Related holes found while designing this (they exist today)

1. **One burn can halt the chain.** Suppose a burn-bearing epoch's ranked
   burners never seal. Nobody else produces the block: an unranked node
   holds its queue at that epoch (`driver.rs:357-360`, "Not ours to seal
   at any rank"). In-order production then stops every later epoch
   (`driver.rs:333-336`). A 1,000-zat burn that credits an address with
   no running sealer, such as a dead address, a lost key, or the default
   address in item 2, stalls Sova until someone patches the code. Today
   the patch could be "anyone builds rank 0's derivation". After SIP-6
   that is impossible, because nobody else holds the key. So **SIP-6 must
   define a keyless fallback.** That is the null block (§2.4).
2. **The miner's default EVM address has no private key.** `sova-miner
   init` credits the hash160 of the Zcash pubkey, reinterpreted as an EVM
   address (`crates/burn-wallet/miner/src/evm_address.rs:9-19, 27-31`).
   It is not `keccak256(pubkey)[12..]`, and nobody can find a key for it.
   Every SOVA minted to a default-configured miner is unspendable. Such
   a miner could never sign a seal. The keeper runbook publishes this
   default (`docs/ops/keeper-miner.md:36`). This needs fixing before the
   reset whether SIP-6 ships or not (§3).
3. **Priority fees go to a random address.** Production builds take their
   attributes from reth's `LocalPayloadAttributesBuilder` (wrapped by
   `SovaLocalPayloadAttributesBuilder`, `bin/sova/src/main.rs:431`). It
   sets `suggested_fee_recipient: Address::random()` and
   `prev_randao: B256::random()`
   (`engine/local/src/payload.rs:48-51`). SIP-3's "priority fees ... pay
   sealers" is not true yet.

## 2. Specification

### 2.1 Where the signature lives

A sealed block's `extra_data` is **exactly 97 bytes**:

| bytes | field |
|---|---|
| 0–31 | vanity: sealer's choice, zero-padded, covered by the signature |
| 32–63 | `r` |
| 64–95 | `s` |
| 96 | `v`, the recovery id, 0 or 1 |

A null block's `extra_data` is **exactly empty**. Any other length is
invalid. Genesis is exempt; the testnet genesis keeps its
`sova-testnet-v0` extra data (`bin/sova/src/chain.rs:54`).

Why `extra_data`, and why this layout:

- **The signature must be outside the signed data, and it must not feed
  execution.** The signed hash covers `state_root`, so any field the EVM
  can read would make the signature circular. That rules out `prev_randao`
  (`PREVRANDAO`) and `beneficiary` (`COINBASE`). `nonce` must be zero
  post-merge (`lib.rs:184-186`). A body field such as a system transaction
  or a zero-amount withdrawal changes a root the signature covers.
  `extra_data` is the one header field the EVM never reads. It is also
  what Clique uses for the same job.
- **The signature must be in the header, not in a sidecar.** Catch-up sync
  imports blocks over reth's eth-wire download and backfill paths, which
  carry headers and bodies only (`docs/design/p2p-m1.md`). A sidecar
  signature would be unverifiable on exactly the path that C5 had to be
  moved into consensus to cover. In the header, the block hash also
  commits to the signature.
- **Fixed lengths, no free bytes.** Every byte of `extra_data` is either
  signed (the vanity) or the signature, so a third party can't change
  any of it.

**The 32-byte limit has to be relaxed in two places:**

1. **Consensus.** `EthBeaconConsensus::new` defaults to
   `MAXIMUM_EXTRA_DATA_SIZE` = 32 (`ethereum/consensus/src/lib.rs:61-64`;
   alloy-consensus 2.4.2 `constants.rs:14`), and `validate_header`
   enforces it (`lib.rs:209`, `validation.rs:273-281`).
   `SovaConsensus::new` (`crates/engine/src/consensus.rs:81`) builds the
   inner consensus with `.with_max_extra_data_size(97)` (`lib.rs:78-81`)
   and enforces the exact {0, 97} rule itself.
2. **Engine payload conversion.** This one is **not configurable**.
   alloy-rpc-types-engine 2.4.2 rejects `extra_data.len() > 32` inside
   `ExecutionPayloadV1::into_block_raw_with_transactions_root_opt`
   (`payload.rs:813-819`). Every V2 and V3 conversion delegates to it
   (`payload.rs:1100-1105, 1383-1388`), and reth's
   `ensure_well_formed_payload` calls it
   (`ethereum/payload/src/validator.rs:66-79`). Every block Sova imports
   as a payload goes through there: sova/1 submissions, the box relay, and
   the node's own builds. So `SovaEngineValidator::convert_payload_to_block`
   (`crates/engine/src/validator.rs:61-68`) stops calling
   `EthereumExecutionPayloadValidator::ensure_well_formed_payload` and
   uses a Sova copy of it (about 40 lines) that:
   1. splits `extra_data` into the vanity and the seal;
   2. converts the payload with `extra_data` set to the vanity only;
   3. puts the full 97 bytes back into the header;
   4. runs `seal_slow` and compares the result against the payload's
      `block_hash`;
   5. runs reth's public `shanghai`, `cancun`, `prague` and `amsterdam`
      `ensure_well_formed_fields` helpers
      (`payload/validator/src/*.rs`), as reth does at
      `validator.rs:88-109`.

   The reverse direction, block to payload, copies `extra_data` without a
   length check (`payload.rs:879-899`, `from_block_unchecked`).

   The `--builder.extradata` CLI limit (`node/core/src/args/payload_builder.rs:200`)
   is unaffected. The builder still writes at most 32 bytes, which become
   the vanity.

### 2.2 What is signed

```
header'     = header with extra_data := extra_data[0..32]      (the vanity)
seal_hash   = keccak256(rlp(header'))
seal_digest = keccak256( 0x19 ‖ "SovaSeal/v1" ‖ chain_id as u64 big-endian ‖ seal_hash )
signature   = secp256k1 ECDSA over seal_digest (prehash), low-s, v ∈ {0,1}
```

- `rlp(header')` is the same encoding the block hash uses, with every
  field present for the active forks. So `header'` covers
  `parent_hash`, `number`, `timestamp`, `beneficiary`, `prev_randao`,
  `state_root`, `transactions_root`, `receipts_root`,
  `withdrawals_root` (the mint), `parent_beacon_block_root` (the SIP-4
  anchor), `requests_hash`, the gas and blob fields, and the vanity.
  `seal_hash` is just "the block hash of the header without its
  signature".
- **Domain separation.** A leading `0x19` can't start an RLP-encoded
  transaction, which is EIP-191's rationale. The next byte, `'S'` (0x53),
  differs from `personal_sign` (`0x19 0x45`) and EIP-712 (`0x19 0x01`).
  So a wallet prompt can't be phished into producing a seal, and a seal
  signer can't be turned into a transaction or permit signer. This
  matters because the sealing key is the key that holds the burner's
  SOVA (§3).
- **Chain ID** is 82330 on testnet and 8233 on mainnet
  (`bin/sova/src/chain.rs:41,46`). A header carries no chain ID, and two
  networks with the same genesis (reth `DEV`, or a reset that reuses a
  genesis) would otherwise accept each other's seals.
- **Canonical signatures only.** `r, s ∈ [1, n−1]`, `s ≤ n/2`, and `v`
  must be 0 or 1. The high-s check is explicit and must not be left to a
  library default. Without it, anyone could flip `s` on an honest seal
  and get a second valid block with a different hash, which reopens the
  hash tiebreak (sim variant (d), §8).
- The sealer never signs a digest someone hands it. It computes
  `seal_digest` from the header itself (§3).

### 2.3 Sealer identity: from the signature, not the tip

For a block at Sova height `N` settling epoch `E_N` with ranked miners
`ranked` (SIP-2):

1. Recover `signer` from the seal.
2. `rank` = the position of `signer` in `ranked`. If it isn't there, the
   block is invalid.
3. The withdrawals must equal the SIP-2 derivation with
   `sealer = signer`. This is C5, now keyed by the signer instead of
   searching every rank.
4. Cross-check: `identify_sealer(withdrawals) == Some(rank)`. It is
   redundant, so it is kept as an assertion in debug builds.

Consequences:

- **"Sealed" means "has burns", not "has a reward".** Today an epoch whose
  scheduled reward is 0 (post-emission, SIP-3 era 43 onward) "settles like
  a burn-less epoch" with no rank (`driver.rs:342-356`,
  `expectations.rs:163-169`). The reason is that the tip can't identify a
  sealer when there is no tip. With signatures the rank comes from the
  signer, so burn epochs keep ranked, signed sealers after emission ends,
  and they collect fees. That is SIP-3's fee-transition story. Without
  this, the null-block rule (§2.4) would leave every post-emission block
  empty of transactions.
- The rule "no sealer metadata rides the wire" (SIP-2) is retired. The
  metadata is the signature, and it is what makes the block's author
  checkable.

### 2.4 Null blocks: epochs with no signer

For every epoch there is exactly one **null block** per parent, written
`null(parent, E)`:

| field | value |
|---|---|
| transactions | none |
| withdrawals | none (no mint; see decision D5) |
| `beneficiary` | `0x0` |
| `extra_data` | empty |
| `gas_limit` | `parent.gas_limit` |
| `timestamp` | `max(parent.timestamp + 1, zcash_time(E_N))` |
| `prev_randao` | `keccak256(parent_beacon_block_root)` (§2.8) |
| `parent_beacon_block_root` | the anchor, `hash(E_N)` (SIP-4) |
| everything else | derived as usual (base fee, blob fields, and the roots after executing the system calls only) |

Nothing is left to choose, so every honest node that builds it builds
the same block with the same hash. There is nothing to grind, and the
hash tiebreak among concurrent empty producers goes away. So does the
`candidates.rs` caveat about burn-less forks outliving their epoch
(module docs, second bullet) when producers share a parent.

**Validity.**

- **Burn-less epochs:** the null block is the only valid block. A sealed
  block for an epoch with no burns is invalid, because nobody is ranked
  to sign it.
- **Burn-bearing epochs:** the null block is also valid, at the lowest
  preference (§2.6). Any correctly signed block from any rank displaces
  it. Today empty withdrawals on a burn-bearing epoch are invalid
  ("reward-withholding", `expectations.rs:439-442`). That rule existed to
  stop unranked producers withholding mint. It is relaxed only for this
  one deterministic block. A null block can only become permanent if no
  ranked burner publishes a signed block before the next epoch is built
  on top, which is about 75 s.

**Production is a liveness policy, not a rule.** A node produces
`null(parent, E)` when no signed candidate for `E` is known and either:

- `E` has no burns (immediately, like today's cadence trigger); or
- the ladder has run out, or epoch `E+1`'s Zcash block has arrived, plus
  one `rank_step` of grace.

This ends the §1.3 halt: a dead-address burn costs the chain one epoch
with no transactions and no mint, and nothing more.

**The cost is transaction latency in quiet epochs.** A null block carries
no transactions, so transactions wait for the next epoch that has a
signed block. At SIP-3 rewards, burning every epoch is worth it for
someone while SOVA has any value, and on testnet the disclosed keeper
burns every epoch. So quiet epochs should be rare. Anyone who needs
inclusion during a quiet stretch can burn 1,000 zat and seal the block
themselves ("burn to include"). Alternatives are in §9.

**`zcash_time`.** This is the header time of Zcash block `E_N`. SIP-4's
follower change already records block `time` (SIP-4 §10,
"`crates/consensus`"), and SIP-6 reads it from the same record. If SIP-6
had to ship before that field exists, `timestamp = parent.timestamp + 1`
would also be deterministic, but it lets Sova time fall behind during
quiet stretches.

### 2.5 Validation: which rule runs where

Every rule runs in `SovaConsensus`, which reth calls on every import
path: engine payloads (`engine/tree/src/tree/payload_validator.rs:953-972`),
the ≤32-block download path (same function, `BlockOrPayload::Block`),
and backfill (`net/downloaders/src/headers/reverse_headers.rs:288,308`,
`bodies/request.rs:188`).

| Hook | Rules | On failure |
|---|---|---|
| `validate_header` (stateless) | `extra_data` length ∈ {0, 97}. Sealed: signature canonical (§2.2) and recoverable. Null: `beneficiary == 0`, `gas_used == 0`, `transactions_root` and `withdrawals_root` are empty-trie roots. Both: `prev_randao == keccak256(parent_beacon_block_root)` | permanent |
| `validate_header_against_parent` | Sealed: `parent.ts < ts ≤ max(parent.ts + 1, zcash_time(E_N) + MAX_SEAL_DRIFT)`. Null: `ts == max(parent.ts + 1, zcash_time(E_N))`, `gas_limit == parent.gas_limit` | permanent. If `E_N`'s record is missing: **hold** |
| `validate_block_pre_execution` (after SIP-4's anchor check) | Sealed: `signer ∈ ranked(E_N)`, and withdrawals = derivation with `sealer = signer` (§2.3). Null: body empty. Burn-less epoch with a sealed block: invalid | permanent once the anchor matches. If the anchor is unknown or differs: hold (SIP-4) |

Why each binding failure is permanent: once the anchor matches, our
ranked list is exactly the one the block's epoch has, so a wrong signer is
a fact about the block and not a Zcash-view difference. This is the same
argument SIP-4 makes for C5.

**The payload path must run the same seal check *before* observing the
candidate.** reth calls `convert_payload_to_block` before
`validate_header` and `validate_block_pre_execution`
(`payload_validator.rs:946-972`). Sova's candidate observation happens
inside `convert_payload_to_block` (`validator.rs:61-132`). If observation
only matched withdrawals, a forged copy would be recorded as the epoch's
best at rank 0 with a lower hash, and consensus would reject it a moment
later. The tracker keeps whatever is observed (`candidates.rs` caveat 1),
so the honest rank-0 block would then be ignored as "not better". The
tip-grinding attack would come back as a denial of service. So
`ExpectedSettlements::check_ranked` takes the recovered signer, and the
payload path and `SovaConsensus` share that one function, as they already
do for C5 (`consensus.rs` module docs). The same applies to
`CandidateTracker::rerank` (`expectations.rs:276-292`): its unranked
entries also store the signer.

`MAX_SEAL_DRIFT`: draft 900 s (§2.8).

### 2.6 Fork choice

Candidate rank, replacing `identify_sealer`'s role in `candidates.rs`:

| candidate | rank |
|---|---|
| sealed by `ranked[r]`, no equivocation evidence for its slot | `r` |
| sealed by a signer with equivocation evidence for that slot (§2.7) | `ranked.len()`, below every honest rank |
| null block | `NULL = usize::MAX` |

Preference stays `(rank asc, hash asc)` (`sealer.rs:45-49`), and the
rest of SIP-2 is unchanged: timeouts are liveness-only, a late better
rank wins by micro-reorg, and the arbiter never reorgs below the tip.
The hash tiebreak now only ever separates one equivocator's own blocks.
Nobody else can make a block at a signed rank, and there is one null
block per parent. The ladder's `produce_decision` also treats a seen
null or equivocator candidate as worse than its own rank, so rank `r+1`
still seals.

The trust rank for unscanned heights (`usize::MAX`, `observe_unranked`)
goes away with SIP-4, which holds those blocks instead of accepting them
(SIP-4 §1, "C5's accept-unknown debt closes"). That is why SIP-6 depends
on SIP-4: the unscanned-tip window is exactly where a forged block would
otherwise first be accepted on trust.

### 2.7 Equivocation

**Definition.** Two valid sealed headers with the same signer and the
same **slot**, meaning equal `(number, parent_hash,
parent_beacon_block_root)`, and different `seal_hash`. A re-seal on a
different parent (after a late-win reorg) or a different anchor (after a
Zcash reorg) is honest and is not equivocation. The evidence is the two
headers themselves, which anyone can verify and nobody can forge without
the key.

**Penalty.** There is no stake, so the only lever is fork choice. A
signer with evidence for a slot has **all** its blocks for that slot
demoted to rank `ranked.len()` (§2.6). If any other ranked burner seals
that epoch, the other burner's block wins and the equivocator loses the
tip and the priority fees. Its pro-rata share is untouched, because every
derivation pays every burner. The ladder already lets rank 1 seal once it
sees rank 0's block demoted. If nobody else seals, the equivocator's
lower-hash block still wins over null. That keeps liveness and costs
nothing honest.

**Rejected alternatives:**

- **First-seen.** Arrival time would decide preference, which SIP-2
  forbids. Views would split by network position.
- **Invalidating the equivocator's blocks.** Evidence arrives at
  different times at different nodes, and an invalidity verdict would be
  permanent (reth caches invalid blocks). Preference can change as
  evidence arrives. Validity can't.
- **Slashing.** There is nothing staked to slash. Minted SOVA can't be
  clawed back by withdrawals, which only add. Banning an address is
  pointless, because burns are Sybil-free and a new address costs
  nothing.

**Signer-side protection (required).** An honest sealer must never
equivocate by accident. For example, `RETRIGGER` (`driver.rs:196-198`)
re-fires a build whose head never moved. If the first block was signed
and relayed but its FCU failed, a fresh build would sign a second block
with a new timestamp. The signer keeps a persistent **seal journal**
mapping slot to signed block, fsynced before the signature is released.
It re-publishes the journaled block for a slot it has already signed and
refuses to sign a different one. This is the same idea as an Ethereum
validator's slashing-protection database.

Evidence spreads without a new message. Both blocks are valid, so sova/1
announces both (its dedup is by hash, and it re-announces only accepted
blocks: `p2p-m1.md`, Decision 3). Each receiver's tracker sees two
headers for one `(slot, signer)`. An `Evidence` message would only be
needed for light clients (§6).

### 2.8 What the sealer can still choose, and whether it matters

After SIP-6, only the ranked sealer can vary its own block: transaction
selection and order, beneficiary, vanity, timestamp within bounds, and
which of its candidate blocks to publish. **That is ordinary block
producer power**, the same as a PoW miner's or a PoS proposer's, and it
lasts for one block. The difference from today is cost. Censoring a
transaction at the tip now means **outburning the top burner in every
epoch you want to censor**. A censored transaction lands in the next
epoch that has a different sealer. Hash grinding buys the sealer
nothing: rank beats hash, and publishing two blocks is equivocation.

Two fields should still be pinned because they are cheap to pin:

- **Timestamp upper bound**, `ts ≤ max(parent.ts + 1, zcash_time(E_N) +
  MAX_SEAL_DRIFT)`. Without it, one sealer could push the timestamp hours
  ahead. Monotonicity would then force every later block to follow, which
  breaks every contract timelock. The `max` guarantees a non-empty window
  even if Zcash time runs backwards within its median-time-past rules.
  Sealers clamp `now` into the window.
- **`prev_randao := keccak256(parent_beacon_block_root)`.** Today it is
  `B256::random()`, chosen by whoever builds the block
  (`engine/local/src/payload.rs:50`). Pinning it removes a grindable field
  and gives contracts a value only Zcash miners can bias, at the cost of a
  Zcash block reward. It is still public before the Sova block's
  transactions execute, so it is no randomness for high-value lotteries.
  That is also true on Ethereum.

`beneficiary` stays the sealer's choice. The default becomes the
sealer's own address instead of `Address::random()` (§1.3 item 3).

## 3. Keys: miners, keepers and agents

**The seal key is the private key of the EVM address the burn credits.**
SIP-1 is frozen and needs no change: that address already is the burner's
identity, and the rewards it receives already need that key to spend.
The signal bits are not used.

**Sealing now needs a key online.** Today `bin/sova` mine mode needs only
`SOVA_MINER_EVM_ADDRESS` (`bin/sova/src/main.rs:42,484-492`). After SIP-6:

- **`bin/sova`.** It takes `SOVA_SEALER_KEYSTORE`, a path to a
  burn-wallet-format JSON keystore (`secret_key_hex`, mode 0600), and
  derives the address from it. If `SOVA_MINER_EVM_ADDRESS` is also set and
  disagrees, the node refuses to start. The signer runs in `SovaMiner`
  between `resolve_kind` and `new_payload` (`crates/engine/src/miner.rs:224-233`):
  1. set the vanity;
  2. compute `seal_digest` from the header itself;
  3. check the seal journal (§2.7);
  4. sign, put the signature into `extra_data`, and reseal;
  5. convert to a payload and submit.

  `alloy-signer-local` and `k256` are already in `Cargo.lock`, so no new
  dependency family is needed. A remote-signer interface (the key in a
  separate process that also owns the journal) is v1.1.
- **`crates/burn-wallet` / `sova-miner`.** `init` derives the default
  EVM address as `keccak256(uncompressed_pubkey)[12..]` from the same
  secp256k1 key that funds the burns. The curve is the same, and the
  t-address and EVM address are already linked on-chain by the burn
  itself. `--evm-address` stays available for burn-only setups that
  credit a cold address. The miner warns that such an address can never
  seal. The hash160 default (`evm_address.rs`) is removed. Keystores
  created under it keep working for burning, but their EVM address has no
  key, so they must re-init (§1.3 item 2). The testnet reset is the time
  to do it.
- **The keeper** (`docs/ops/keeper-miner.md`) re-inits under the fixed
  derivation and runs `bin/sova` with `SOVA_SEALER_KEYSTORE` pointing at
  its own keystore. It is still an ordinary miner with its own key on its
  own machine, which is Rob's D8.
- **MCP agents.** The MCP server drives the miner CLI
  (`mcp/src/minerCli.ts`) on the agent's own machine, so the key is
  already there. An agent that only burns and runs no node is ranked but
  never seals. It still gets its pro-rata share whenever any ranked burner
  seals, because every derivation pays every burner. It loses only the
  tip. If it is rank 0, it costs the network one `rank_step` of latency
  per epoch while rank 1 waits. If no ranked burner seals at all, the null
  block mints nothing, and a lone burn-only agent earns nothing. The MCP
  docs must say this plainly. A "seal too" toggle that starts `bin/sova`
  in mine mode with the same keystore is the natural follow-up.

**Hot-key risk.** The sealing key is online and holds the address's
SOVA. Operators should sweep rewards to a cold address. Anyone who wants
the key that holds funds to stay cold needs delegation.

**Delegation is not in v1.** Options, if agents or custodial burners need
it later:

- **A SIP-1 signal bit plus an on-chain registry.** Consensus would have
  to read EVM state in a pre-execution hook, which it can't do today,
  and a light client would need a state proof. Rejected.
- **A second Zcash output naming the sealing key.** SIP-1 is frozen, and
  "exactly one payload output" leaves no room. It would need a new SIP.
  Rejected.
- **A delegation certificate in `extra_data`** (preferred, v1.1). The
  credited address signs once, domain-separated:
  `keccak256(0x19 ‖ "SovaDelegate/v1" ‖ chain_id ‖ delegate ‖ expiry_epoch)`.
  The sealer appends `delegate(20) ‖ expiry(8) ‖ cert_sig(65)` after its
  own seal. Validation checks `cert signer == ranked[r]`,
  `seal signer == delegate`, and `E_N ≤ expiry`. This is stateless and
  light-client-friendly. The only way to revoke is to let it expire. It
  would add a third allowed `extra_data` length (190).

## 4. Interactions

- **SIP-4 anchor.** The anchor sits in `parent_beacon_block_root`, which
  the seal covers, so a signature binds its block to exactly one Zcash
  chain. SIP-4's order stays: anchor first (hold on mismatch), then the
  binding (permanent). A Zcash reorg that changes `hash(E_N)` makes the
  sealer re-seal. The new anchor makes that a new slot, so it is not
  equivocation.
- **Late-win micro-reorg.** It is unchanged. A late rank 0 signs its
  sibling (`BuildTarget { sibling: true }`), and the displaced rank-1
  block was signed by someone else, so no evidence arises. The sealer's
  covered-tip logic still reads `best_seen` from the tracker. A null or
  equivocator candidate counts as beatable.
- **Arbiter.** It is unchanged. The stale-height skip still bounds reorgs
  to the tip epoch. It is also what limits an equivocator: evidence that
  arrives after the next epoch was built on top changes nothing.
- **sova/1 and the box relay.** The wire format is unchanged. Blocks
  carry the seal in their header (+65 bytes). Both submit via
  `new_payload`, so both go through the conversion shim (§2.1). A bad
  seal is permanently `Invalid` and costs the sending peer reputation. A
  missing record is a hold (SIP-4's `HOLD_MARKER`) and costs nothing.
- **Catch-up sync.** Download and backfill call `validate_header` and
  `validate_block_pre_execution` (§2.5), so every historical block's seal
  and binding are checked, gated on the scan as today (`p2p-m1.md`
  Decision 2). The `join-scenario` gets a tampered-seal variant (§8).
- **Execution and state.** None. Nothing the EVM can read changes, apart
  from the pinned `PREVRANDAO` and the beneficiary default.

## 5. Cost

- **Bytes.** +65 bytes per sealed header. At 75 s epochs that is about
  27 MB a year. Null blocks are smaller than today's cadence blocks.
- **CPU.** One ECDSA signature per sealed block, and one recovery per
  imported header (tens of microseconds, *estimate*).
- **Engineering** (*estimates*, worker-days):
  - consensus rules and the conversion shim: 3
  - signer, journal and keystore wiring: 2
  - null-block builder mode (no pool; pinned gas limit, timestamp and
    randao): 2–3
  - rank from signer, equivocation evidence, tracker and rerank: 2
  - burn-wallet key derivation: 0.5
  - sims: 3

  Total: **about 2.5 to 3 weeks** of one worker.
- **Operator cost.** Sealers must keep a key online. Every existing
  testnet miner must re-init.

## 6. What a light client gets, and what it still lacks

**What it gets.** Each sealed block now names its author, and the
header alone proves it. A light client that tracks Zcash headers (NEAR
has one, per `zec-peg-v2.md`) can check the following with a Merkle proof
of the burn transaction against `hash(E_N)`:

- the signer burned in `E_N`;
- the signer appears in the block's withdrawals.

A forged block now needs a real burn and the burner's key. It is no
longer free.

**What it still lacks:**

1. **Proof of rank.** Inclusion proves a burn, not that no heavier burn
   exists. Checking "rank 0" needs every burn in `E_N`. A practical
   design is optimistic: accept a sealed header after a challenge window
   unless someone proves a heavier burn, or an equivocation, for that
   epoch. Until then, a light client should require `k` sealed
   descendants from **distinct** signers. Faking those costs `k` real
   burns plus control of their keys.
2. **Execution correctness.** A sealer can sign a wrong state root. That
   is `zec-peg-v2.md` §1.4(a) item 3 and is unchanged here.
3. **An evidence channel** that brings equivocation proofs to the light
   client. This is a contract-side feature, not a Sova consensus one.

SIP-6 is **necessary** for a trust-minimized bridge out of Sova. It is
not sufficient on its own.

## 7. Activation

At the **testnet reset**, from genesis, together with or after SIP-4 §1.
Prerequisites:

1. SIP-4 §1 (anchor and hold) is merged.
2. The follower records Zcash block time (SIP-4 work).
3. The burn-wallet derivation fix (§3).
4. Keeper and MCP docs are updated.

Any testnet state from before the reset is abandoned anyway. Mainnet
launches with SIP-6 from genesis. Any later change to the seal format is
a fork-height rule.

## 8. Test and sim plan

**Unit tests:**

- Test vectors: a fixed header gives a fixed `seal_hash`, `seal_digest`
  and signature, with a known key, for both chain IDs.
- Rejections:
  - `extra_data` of length 96 or 98;
  - high-s;
  - `v = 27`;
  - `r = 0`;
  - signer not ranked;
  - signer ranked but the withdrawals are another rank's derivation;
  - a sealed block for a burn-less epoch;
  - a null block with a transaction, a beneficiary, a wrong timestamp, a
    wrong gas limit, or a wrong randao.
- The conversion shim round-trips 97-byte `extra_data` through
  `block_to_payload` and `convert_payload_to_block`, and the hash is
  unchanged.
- Candidate tracker:
  - a forged block is never observed (the poisoning regression, §2.5);
  - an equivocator is demoted, and a re-seal on a new anchor is not;
  - a null block ranks below everything.
- Signer journal: after a restart mid-seal, the same block is
  re-published and no second signature is made.
- Post-emission: a burn epoch with reward 0 is still ranked and signed.

**Box scenarios** (`box/sim/`):

1. **`tip-grinder-scenario.sh` (the regression for this SIP).** Nodes:
   A (rank 0), B (rank 1), D (observer), and C, the attacker, with no
   burns and a small box-only `sova-grind` tool. A test account sends tx
   `T`. When A's block at height `h` arrives, C builds on A's parent with
   A's withdrawals and anchor, drops `T`, sets the beneficiary to C, and
   grinds its vanity until its hash is below A's hash. It publishes these
   variants:
   - (a) sealed with C's own key;
   - (b) the pre-SIP-6 layout: ≤32-byte `extra_data`, no seal;
   - (c) A's signature copied onto C's header;
   - (d) A's exact block with `s` flipped;
   - (e) a "null" claim that carries transactions.

   **Pass:**
   - every variant is `Invalid` at A, B and D, and C loses reputation;
   - the tracker's best at `h` stays A's hash on every node;
   - `T` is included at `h`;
   - heights, state roots and balances converge.

   **Control:** the same attack (b) run against a build from before
   SIP-6 must succeed, with C's block becoming head everywhere. That
   proves the scenario really exercises the finding.
2. **Equivocation.** A signs two blocks for one slot, with different
   transactions. Every node demotes A. B seals at its rung and wins
   everywhere. A's tip goes to B's derivation. Variant: nobody else is
   ranked, so A's lower-hash block wins over null.
3. **Abandoned epoch (the §1.3 halt).** A burn credits an address with no
   sealer, and it is the only burner. After the ladder plus grace, every
   node builds `null(parent, E)` with the **same hash**, no SOVA is
   minted, and the next epoch proceeds. Control: before SIP-6, the chain
   stalls.
4. **Burn-less run.** Ten consecutive burn-less epochs across three nodes
   give identical null blocks with no forks. This closes the
   `candidates.rs` caveat 2 measurement.
5. **Ladder, late win and ladder-p2p re-runs** with signatures, all
   green.
6. **Join with tampered history.** A fresh node backfills a chain in
   which one historical block has (i) a bad seal and (ii) a valid seal by
   an unranked key. The node rejects both at that height.
7. **Zcash reorg.** A regtest reorg replaces `E_N` with a block holding
   the same burns. Rank 0 re-seals on the new anchor, and no node records
   equivocation.

## 9. Alternatives considered

- **Burn-less epochs:**
  - *Unsigned plus hash tiebreak, as today*: anyone can grind every
    quiet block, and a light client can't trust any of them. Rejected.
  - *Any signer*: keys are free, so this is the same as unsigned.
    Rejected.
  - *Recent-sealer extension ladder* (`sealer.rs` `extension_rank`):
    keeps transactions flowing in quiet epochs, but adds a consensus-level
    recent-sealer set and a second ladder. It is the right v2 if quiet
    epochs turn out to be common, and it can be added later because it
    only adds candidates above null.
  - *Deterministic null block*: **recommended.**
- **Where the signature goes:**
  - *EIP-2098 64-byte compact signature split across `prev_randao` and
    `extra_data`*: circular, because the EVM reads `prev_randao`.
  - *A sidecar*: breaks catch-up sync.
  - *A BLS or Schnorr key*: the burner's identity is an EVM address,
    which a secp256k1 key controls. A second key type needs delegation
    anyway.

## 10. Decisions for Rob

1. **Number.** This is SIP-6, because SIP-5 is the wZEC peg.
   *Recommend: yes.*
2. **Seal key.** The key of the EVM address the burn credits, with no
   delegation in v1. A certificate-based delegation is the v1.1 path if
   agents need a cold reward address. *Recommend: yes.*
3. **Seal location.** `extra_data` = 32-byte vanity plus a 65-byte low-s
   ECDSA signature over a `0x19‖"SovaSeal/v1"‖chain_id`-prefixed header
   hash. This needs a relaxed extra-data limit and a copy of reth's
   payload conversion. *Recommend: yes.*
4. **Burn-less epochs.** One deterministic null block: no transactions,
   no mint, and fixed fields. Nothing to grind, but transactions wait for
   the next burn epoch ("burn to include"). *Recommend: yes*, with the
   recent-sealer ladder held back as a v2 option.
5. **Abandoned burn epochs**, where no ranked burner seals. The same null
   block is always valid at the lowest preference. This ends today's
   one-burn chain halt. What it mints: **nothing** (recommended: simplest,
   matches SIP-3's "no mint without a seal", and gives the strongest
   reason to run a sealer), or the pro-rata pool without the tip (kinder
   to burn-only agents).
6. **Equivocation.** Demote the equivocating signer below all honest
   ranks for that slot, so it loses the tip if anyone else seals. No
   slashing and no invalidation. Sealers must keep a seal journal.
   *Recommend: yes.*
7. **Pinned fields.** `ts ≤ max(parent+1, zcash_time + 900 s)`, and
   `prev_randao = keccak256(anchor)` for every block. *Recommend: yes*,
   or leave `PREVRANDAO` to the sealer and document that it isn't
   random.
8. **Sealer identity from the signature, not the tip.** Burn epochs stay
   signed even when the reward is 0, so fees still pay sealers after
   emission ends. *Recommend: yes.*
9. **Fix now, independent of SIP-6.** `sova-miner`'s default EVM address
   has no private key (all its SOVA is unspendable), and the fee
   recipient is random. Switch to keccak derivation from the same key,
   and make the sealer the default beneficiary. *Recommend: yes, before
   the reset.*
10. **Activation.** At the testnet reset, together with or after SIP-4 §1.
    Every testnet miner re-inits. *Recommend: yes.*

## Sources

Reth claims are checked against reth v2.6.0 (rev `73a3a00`,
`~/.cargo/git/checkouts/reth-e231042ee7db3fb7/73a3a00/crates/...`).
alloy claims are checked against the `Cargo.lock` versions,
alloy-rpc-types-engine 2.4.2 and alloy-consensus 2.4.2 (cargo registry
sources). Sova claims are checked against `release` at `d483e1d`.
Figures marked *estimate* are not measured.
