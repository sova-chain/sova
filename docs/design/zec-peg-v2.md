# ZEC peg v2: NEAR MPC custody, monitoring, and self-custody vaults

Status: **for Rob's decision**. Written 2026-09-23 as a companion to
`docs/design/zec-on-sova-options.md` (read that first). This is design
only. Nothing here is built, and **no peg exists today**. The roadmap's
"no peg at launch" still stands. The external facts were checked on
2026-09-23 against the primary sources listed at the end; live NEAR
facts come from read-only view calls on NEAR mainnet at around block
216,910,600. Anything marked *(estimate)* is an estimate.

It answers the two ideas Rob raised on 2026-09-23:

1. NEAR MPC (Chain Signatures) as the custodian, with the project
   holding no key, "and then some sort of additional precompile that
   reads NEAR MPC state, so we can monitor what's happening there."
2. "What if people could just self-custody ZEC in a multisig sort of
   wallet, and then we minted a wrapped ZEC on Sova by watching their
   address?"

## The answers, first

- **NEAR MPC can be the custodian, and the project needs no key.**
  The MPC signs any 32-byte secp256k1 digest, which is enough for
  transparent Zcash spends as long as the NEAR contract computes the
  ZIP-244 sighash itself. Omni Bridge already does exactly this for
  Zcash. The real custodian is **the NEAR vault contract's code plus
  11 of the 17 MPC operators**. TEE hardware does not yet protect the
  key: only 9 of the 17 nodes run real TDX attestations.
- **The hard part is telling NEAR that a wZEC burn happened on Sova.**
  A Sova light client on NEAR is the right answer and is not buildable
  today. Sova blocks carry no signature and no work, so a light client
  can't tell the canonical block from a valid fork, and fixing that is a
  Sova consensus change. NEAR's MPC foreign-chain verification exists
  but covers only Bitcoin, Abstract, Starknet and Aptos on mainnet.
  Adding Sova is NEAR's decision and trusts RPC providers. **The
  realistic first version is NEAR adding Sova to foreign-chain
  verification, with MPC operators running their own Sova nodes.**
  Whatever the channel, the NEAR contract should enforce a delay and a
  daily rate limit.
- **Don't build a precompile that reads NEAR.** It would force every
  Sova node to run a NEAR node or trust a NEAR RPC, which breaks the
  own-node invariant. **SIP-4 already lets Sova watch the vault on
  Zcash**, and that alone catches theft and proves reserves. If NEAR
  visibility is wanted later, build a **NEAR light-client contract on
  Sova**, fed by anyone, ideally backed by a pure Ed25519 precompile.
- **Self-custody as literally stated breaks backing**, because the user
  can spend the ZEC while the wZEC circulates. The version that works
  is a per-user 2-of-2 (user + MPC) P2SH with a CLTV refund. It is safe,
  but it **can't back fungible wZEC**: someone other than the depositor
  has to be able to pay a stranger, and that someone is a custodian.
  It fits as **non-transferable, ZEC-backed credit**. Its simplest
  form, a CLTV-only lock, needs **no custodian at all** and can ship
  right after SIP-4.

---

## 1. NEAR MPC as the custodian

### 1.1 How Chain Signatures works today (verified)

| Question | Answer | Source |
|---|---|---|
| Who signs | The `v1.signer` contract on NEAR mainnet, version **3.15.1**. **17 participants, threshold 11.** NEAR's docs still say "8 independent nodes". That is out of date. | live `state` and `version` views; docs page |
| Domains | 0: secp256k1 ECDSA (`sign`); 1: Ed25519 (`sign`); 2: BLS12-381 key derivation; 3: secp256k1 for foreign-transaction verification. Each needs 11 shares to sign. **There is no RedJubjub or RedPallas domain**, so the MPC **cannot** sign shielded Zcash spends. | live `state` view |
| TEE | Of the 17 nodes, **9 present real Intel TDX (Dstack) attestations and 8 present "Mock" attestations**. The threshold is 11, so **TEE does not currently constrain the key**. The operator guide says mainnet "does not yet require TEE" (2026-06). | live `get_attestation` for all 17; `docs/guide/operating-an-mpc-node.md` |
| Changing the set | `vote_new_parameters`. It needs 11 current participants, every incoming participant must vote, and at least 11 old participants must stay. Resharing **keeps the same public key**, so derived addresses survive set changes. | `api/governance.rs`, `state/running.rs`, `state/resharing.rs` |
| Upgrading the contract | Participants propose, and the update runs **immediately** once 11 have voted. There is **no timelock**. `v1.signer` has no access keys. | `api/update.rs`; live `view_access_key_list` |
| The `sign` call | `sign({request:{path, payload_v2:{Ecdsa:"<32 bytes hex>"}, domain_id:0}})`, with a 1 yoctoNEAR deposit and at least 15 TGas. It uses yield/resume: a node answers with `respond`, and the contract checks the signature against the derived key before returning `{big_r, s, recovery_id}`. It **times out after 200 blocks** (about 2 min at 0.61 s blocks) and the caller's transaction fails. | `api/sign.rs`, `deposits.rs`, nearcore `parameters.yaml` |
| What gets signed | The 32-byte payload is signed **as a prehash, with no further hashing**. The only constraint is that it must be less than the secp256k1 group order. **Nothing in the contract is Zcash-aware.** | `api/sign.rs`, `primitives.rs` |
| Key derivation | `tweak = SHA3-256("near-mpc-recovery v0.1.0 epsilon derivation:" ‖ predecessor ‖ "," ‖ path)`, and `derived_pk = root_pk + tweak·G`. Derivation is additive and public, so anyone can compute a derived key off-chain. The contract also has a `derived_public_key` view. | `near-mpc-crypto-types/src/kdf.rs`, `crypto_shared/kdf.rs`, `api/keys.rs` |
| Who can sign for a key | **Only the predecessor account**, meaning the NEAR account or contract that calls `sign` directly. The tweak is derived from `env::predecessor_account_id()`. | `api/common.rs`, `api/sign.rs` |

**Is a generic secp256k1 payload enough for Zcash?** Yes, for
transparent inputs. A transparent Zcash signature is ECDSA over the
ZIP-244 signature digest (32 bytes of BLAKE2b-256). Compute the digest,
hand it over as the payload, and attach the returned `(r, s)` in DER
form plus the hash-type byte. The chance that a digest lands at or
above the group order is about 4 × 10⁻³⁹, so ignore it, but handle it
by changing a field and re-hashing. v6 transactions (NU6.3, ZIP-229)
reuse the v5 transparent sighash unchanged, and v5 stays valid after
NU6.3. One `sign` call is needed per transaction input, since each
input has its own digest.

**Proof that it works: Omni Bridge already does it.** NEAR's Omni Bridge
supports Zcash in both directions. Its Zcash connector (`satoshi-bridge`
built with `--features zcash`, in `Near-One/btc-bridge`, branch
`omni-main`) **builds the v5/v6 transaction inside the NEAR contract**
and computes the ZIP-244 digest on-chain with `zcash_primitives`
(`psbt_wrapper.rs: get_hash_to_sign`, `SighashType::ALL`). It then
sends that digest to `sign`. Inbound deposits are checked against a
**Zcash light client on NEAR** that verifies Equihash (200, 9) on-chain
(`Near-One/btc-light-client-contract`, `contract/src/zcash.rs`).
Header submission there is permissioned (`#[trusted_relayer]`).

### 1.2 Who the custodian really is

Only the NEAR account that owns a derivation path can get signatures
for it. So "the custodian" is not a company. It is:

1. **The NEAR vault contract's logic.** Whatever it agrees to sign, the
   MPC signs.
2. **Whoever can change that contract**, through a full-access key on
   its account or an upgrade method. With Rob's "no project key", the
   vault account must hold **no access keys and have no upgrade
   method**. A new version would mean a new contract, a new vault
   address, and users choosing to migrate.
3. **11 of 17 MPC operators.** Those operators can sign any payload for
   any derived key, bypassing every contract, because the contract is
   coordination rather than cryptography. With TEE not enforced, this
   is a plain 11-of-17 honesty assumption across named staking
   companies.
4. **The same 11 of 17, as the MPC contract's upgraders.** They can
   change the contract instantly.
5. **Whatever tells the vault contract that a Sova burn happened**
   (§1.4). This is the weakest link.

The vault contract must build the Zcash transaction and compute the
sighash itself, as Omni does. **If it accepted a sighash from a caller,
it would be signing blind, and the caller would be the custodian.**

The MPC's root key also secures every other Chain Signatures user,
including Omni Bridge's BTC and ZEC. That makes it a large shared
target, but its operators have a lot at stake. Sova can't change any of
this. It can only disclose it.

### 1.3 The Sova side (no NEAR needed to build it)

The Sova half works as in option A1 of the options paper, with one
improvement on its option B.

- **Per-user deposit addresses without point arithmetic.** The options
  paper said per-user MPC addresses can't be derived in the EVM. That
  is true of *derived keys*, because they need SHA3-256 (not keccak)
  and secp256k1 point addition. It is not true of *scripts*. Use one
  MPC-derived vault key `K_vault` (path, say, `sova-wzec-v1`) inside a
  per-user **tagged P2SH**: `<evm20> OP_DROP <K_vault> OP_CHECKSIG`. A
  Sova contract computes that address with the SHA-256 and RIPEMD-160
  precompiles, just as in A1. Each Sova account gets its own `t3…`
  address, and no OP_RETURN is needed. The NEAR contract signs P2SH
  inputs by rebuilding the redeem script from the tag.
- **Mint** is SIP-4-verified. Anyone submits the outpoint, and the
  contract checks `txOutput` against the tagged script at `minConf`.
  The MPC takes no part in minting.
- **Withdraw.** A user burns wZEC and names a transparent destination
  and an amount. The request is recorded on Sova.
- **Watch** (§2): SIP-4 v1.1 `spentBy` catches any vault spend that
  doesn't match a request.

### 1.4 The crux: how the NEAR contract learns about a Sova burn

The vault contract may sign a payout only for a burn that really
happened on canonical Sova. There are three ways it could know.

#### (a) A Sova light client on NEAR

A light client needs a cheap way to check that a claimed block is **the
canonical** one. For Sova that means:

1. **The Zcash chain up to the anchor.** Under SIP-4, Sova block `N`
   commits to the hash of Zcash block `E_N`. NEAR already has a Zcash
   PoW light client (Omni's), so this part exists.
2. **That the block is canonical.** This part is missing. **Nothing in
   a Sova block proves who built it.** SIP-2 recovers the sealer's rank
   from the withdrawals ("no sealer metadata rides the wire"). There is
   no signature and no proof-of-work on the Sova block itself. Nodes
   pick among valid candidates for an epoch by `(rank asc, hash asc)`
   (`crates/engine/src/candidates.rs`) and treat depth 64 as final. So
   anyone can assemble a *valid but non-canonical* Sova block for a
   recent epoch, put their own wZEC burn in it, and show it to a light
   client, while keeping the wZEC on the real chain. This costs nothing
   to attempt. **Even a perfect execution proof doesn't stop it,**
   because the fake block is valid, just not chosen.
3. **That execution is correct.** The state root would need a zk proof
   of reth execution, or an optimistic fraud-proof game. Neither
   exists for Sova.

The fix for (2) is a consensus change, for example requiring the ranked
burner's key (the EVM address the burn credits) to sign the block, plus
an equivocation rule. It needs its own SIP. The same change is the
prerequisite for any trust-minimized bridge *out of* Sova, to NEAR or
anywhere else. *Estimate: 6+ months, research-grade.* (Side note: hash
tie-breaking among same-rank candidates also means anyone can grind a
lower-hash block for an epoch. That belongs in a consensus review, not
this doc.)

#### (b) NEAR MPC foreign-chain verification

**It exists, and it is live on mainnet for four chains.**

- A NEAR contract calls `verify_foreign_transaction` on `v1.signer`
  with a transaction id, "extractors" (for EVM, `BlockHash` and
  `Log{log_index}`), and a finality level (`Latest`, `Safe` or
  `Finalized`).
- Each MPC node queries the foreign chain through RPC providers, and
  the network signs `sha256(borsh(request, extracted values))` with the
  domain-3 root key. The caller's contract checks that signature.
- The live `get_available_foreign_chains` view returns
  `["Bitcoin","Abstract","Starknet","Aptos"]`. The code also knows
  Ethereum, Base, Arbitrum, Polygon and other chains, but **no general
  EVM chain is enabled on mainnet**, not even Ethereum.
- RPC providers sit on an **on-chain whitelist of provider URLs with a
  quorum per chain**, changed by a participant vote. Bitcoin uses a
  single provider with quorum 1. The others use quorum 3 across
  Alchemy, QuickNode and public endpoints.
- **Adding a chain takes a near/mpc code change** (each chain is an enum
  variant), a contract upgrade vote, node upgrades, and a whitelist
  vote. The feature started around 3.5.0 (February 2026) and is still
  changing fast.

For Sova, the trust becomes **11-of-17 MPC plus the honesty of the
queried RPCs**. No commercial Sova RPC exists. **If the RPCs are ours,
we are the oracle**, which amounts to holding a key. The acceptable
version has each MPC operator run its own Sova node and zebrad and
query that. This mirrors Sova's own invariant, but NEAR's whitelist
model is provider URLs, so it needs NEAR to agree to that shape.
"Finalized" should map to Sova's 64-block depth (about 80 minutes at
75 s *(estimate)*). Omni already uses this path for some chains
(`near/omni-prover/mpc-omni-prover`).

#### (c) A relayer or attestor set

Named parties sign "burn X happened", and the NEAR contract checks
m-of-n signatures. It is simple to build, and it is plain trust. If the
project runs one of the attestors, the project holds a key, which Rob
has ruled out. Bonds on Sova don't help: a theft proven on Sova can't
reach a NEAR-side bond unless the Sova→NEAR channel it depends on
already works.

#### Comparison

| | (a) Sova light client on NEAR | (b) MPC foreign-chain verification | (c) Attestor set |
|---|---|---|---|
| Trust added beyond the MPC | None (code only) | The RPCs the MPC queries | m-of-n attestors |
| Exists today | **No.** It needs a Sova consensus change to make canonical blocks light-client-checkable, plus an execution prover | Mechanism **yes**. For Sova **no**: NEAR code, a vote and operator setup are needed | Could be built in weeks |
| Who decides | Sova (SIP) plus our engineering | NEAR MPC operators and near/mpc maintainers | Us plus the attestors |
| Fits "project holds no key" | Yes | Yes, **if operators run their own Sova nodes**; no, if they query our RPC | Only if no attestor is ours |
| Failure mode | A bug in the light client or prover | A lying provider quorum, or a quorum on a stale fork | Attestors collude |
| Effort *(estimate)* | 6+ months, research | Weeks on our side; NEAR's timeline on theirs | 2–3 weeks |

Whichever channel is used, the NEAR vault contract should add two
cheap limits that also cover a bug in its own logic:

- **A withdrawal delay**, for example 12–24 h *(estimate)*.
- **A daily outflow cap**, for example a few percent of reserves
  *(estimate)*.

A bad message then drains a slice of the vault, not all of it. Sova's
theft proof (§2) fires on the first bad payout and stops new minting.

### 1.5 The shortcut: Omni Bridge lists Sova

The lowest-effort route is for Omni Bridge to add Sova as a destination,
using the ZEC it already bridges. Sova would then need no vault of its
own. But the Omni contracts have DAO upgrade, pause and relayer roles
(`rainbowbridge.sputnik-dao.near`, `#[access_control]`, `#[pausable]`,
`#[upgradable]`), so the custodian would include admin keys. They
aren't the project's keys, but a wZEC based on Omni fails the brand's
"no admin keys" test unless we say so plainly. This is NEAR's decision,
and it belongs to the conversation that follows the demo.

---

## 2. "A precompile that reads NEAR MPC state"

### 2.1 Why not a precompile

A precompile that answers questions about NEAR has three problems:

- **It breaks the own-node invariant.** Every Sova node would need its
  own NEAR node, or a NEAR RPC it trusts. The first adds a second full
  node of a fast chain (0.61 s blocks, 414 validators) to every Sova
  operator's box. The second is the Bitcoin-era pattern that failed.
- **It repeats SIP-4's determinism work for a second chain.** It needs
  a committed NEAR anchor in every Sova header, hold-not-guess rules,
  and rollback handling.
- **It couples Sova's liveness to NEAR's.** A NEAR stall or RPC outage
  would stall Sova blocks.

This is the same analysis SIP-4 §13 made for "live zebrad RPC inside the
precompile", except worse, because Sova's consensus needs Zcash but
doesn't need NEAR.

### 2.2 What Zcash alone already shows (SIP-4)

The vault is a set of transparent scripts on Zcash, and Sova can read
transparent Zcash. With SIP-4 v1.1 (`spentBy`), the Sova peg contract
can do three things with no NEAR data:

- **Theft proof.** The contract knows every vault outpoint, because it
  minted against each one. When one is spent, anyone can submit the
  spending transaction. The contract checks, through `txOutput`, that
  the outputs are exactly:
  - one registered withdrawal request (its script and its amount minus
    the published fee), plus
  - change back to vault scripts.
  If not, **minting halts automatically** and an alarm event fires.
  Nobody holds a pause key.
- **Proof of reserves.** The sum of unspent vault outpoints versus wZEC
  supply is live contract state.
- **Freeze detection.** A request still unpaid after its deadline is
  visible on Sova. The contract can then stop accepting deposits.

This catches every way the custodian can fail on Zcash, whether a bad
Sova→NEAR message, a vault-contract bug or MPC collusion. Its limit is
timing: it detects theft once the payout is mined (SIP-4 has no
mempool), so it limits damage but can't prevent it. **That is true of
any Sova-side monitor, including one that reads NEAR.** Sova can't stop
a Zcash transaction.

### 2.3 What reading NEAR adds

| NEAR-side fact | What Sova gains | How much it matters |
|---|---|---|
| The vault contract's sign-request log (the full transaction it is about to sign) | Sees a bad payout seconds after it is requested, instead of after a Zcash block (~75 s) plus `minConf` | Small. It halts minting a few minutes earlier and can't stop the payout |
| A theft with **no** matching NEAR request | Attribution: the MPC signed outside the contract, which means operator collusion | Medium. It tells users which assumption broke |
| MPC set change, contract upgrade, TEE policy change | Alerts, or an automatic rule such as "new deposits pause for N days after a `v1.signer` upgrade" | Medium. It is the only NEAR signal with any lead time, and MPC upgrades have none, so its value is in the rule, not the warning |
| Pending or timed-out sign requests | Liveness: withdrawals stuck on the NEAR side | Low. Unpaid-by-deadline on Sova already shows this |

In short, NEAR data adds **attribution and governance alerts, not
safety**. Safety comes from limits on NEAR and the Zcash-side proof on
Sova.

### 2.4 If we want NEAR data: a NEAR light-client contract on Sova

NEAR is well suited to a light client, because its blocks are signed.

- **The rule** (nomicon `ChainSpec/LightClient`). A `LightClientBlockView`
  is accepted when block producers holding **more than 2/3 of the
  epoch's stake** have signed approvals, with Ed25519 keys, over the
  next block's hash at height h+2. `next_bps` hands over the next
  epoch's producer set, checked against `next_bp_hash`. A client must
  see **at least one block per epoch**: 43,200 blocks, about 7.4 h.
  Mainnet has 100 block-producer seats.
- **What it can prove.** Execution outcomes, meaning receipts and their
  logs. This uses the `EXPERIMENTAL_light_client_proof` RPC: a Merkle
  path to `outcome_root`, then a path to `block_merkle_root`. Contract
  **state** proofs (code hash, access keys, the MPC participant list)
  exist in principle (`view_state` with `include_proof`), but the
  binding to the light-client header is **not documented**, so treat it
  as unverified. Design for **logs**: the vault contract logs every
  sign request and every configuration event. Note that a light client
  proves that something happened, never that something didn't.
- **Rainbow Bridge's precedent, and why it was optimistic.** Its NEAR
  light client on Ethereum (`NearBridge.sol`) stored relayed headers
  **without checking signatures**. A relayer posted a 5 ETH bond, and
  anyone could challenge one signature during a **4-hour window**. A
  successful challenge slashed the bond, half of it to the challenger.
  - The reason was gas. Ed25519 in Solidity costs about 500–700k gas
    per signature (Rainbow README: ~697k to submit a header, ~700k to
    challenge). A full check of 67+ signatures is roughly 35–70M gas
    *(estimate)*, which is over Ethereum's block limit.
  - In August 2022 an attacker relayed a fabricated NEAR block.
    Watchdogs challenged it within 31 seconds, and no funds were lost.
    So the design works, but only while an honest, funded watcher is
    online.
  - NEAR has since moved NEAR→ETH to MPC signatures (Omni Bridge).
- **What Sova can do that Ethereum couldn't: add a pure Ed25519-verify
  precompile.** It reads no external data, so it doesn't touch the
  own-node invariant. It is the same class as `ecrecover`. There is
  precedent: EIP-665 (Stagnant) proposed 2,000 gas; Celo shipped one
  (CIP-25, later removed); Bittensor's EVM has one. At about 2–3k gas
  per signature, a header with ~70 signatures verifies fully for
  roughly 0.5 M gas *(estimate)*, once per 7.4 h epoch. That means **no
  bond, no challenge window, and no watcher.** It needs a small SIP of
  its own. Without it, copy Rainbow's optimistic design, bond paid in
  SOVA.
- **Relaying is permissionless.** Anyone can submit headers and outcome
  proofs, and the contract verifies them. No project relayer is needed,
  but someone must post one header per epoch. At about 0.5 M gas every
  7.4 h, that is cheap enough for any watcher *(estimate)*.

### 2.5 Recommended monitoring design

1. **Phase 1 (part of SIP-5, required): monitoring from Zcash alone.**
   It uses SIP-4 v1.1 `spentBy` and needs no NEAR data. It provides a
   theft proof with an automatic mint halt, live reserves, and
   unpaid-request detection. So **`spentBy` must ship before any peg
   testnet** (SIP-4 open question 2).
2. **Phase 2 (optional, after the peg runs on testnet): a NEAR
   light-client contract on Sova.**
   - It is fed permissionlessly and verified fully through a pure
     Ed25519 precompile.
   - It reads the vault contract's logs for attribution, and
     `v1.signer` upgrade and participant-set receipts for automatic
     deposit pauses.
3. **Never:** a precompile that queries NEAR.

---

## 3. Self-custody vaults (Rob's second idea)

### 3.1 As stated, it breaks backing

If the user keeps sole control of the ZEC and Sova mints wZEC by
watching the address, the user can spend the ZEC at any moment while
the wZEC circulates. SIP-4 would see the spend a block later, but the
wZEC is already in other people's hands, and nothing can make it whole.
**That wZEC is unbacked by construction.** It doesn't work.

### 3.2 The version that works: 2-of-2 plus a timelocked refund

A per-user P2SH vault:

```
OP_IF
    <K_custodian> OP_CHECKSIGVERIFY <K_user> OP_CHECKSIG
OP_ELSE
    <T> OP_CHECKLOCKTIMEVERIFY OP_DROP <K_user> OP_CHECKSIG
OP_ENDIF
```

(An `<evm20> OP_DROP` tag in front binds the vault to a Sova account.)

- **Before height `T`,** spending needs both the user and the
  custodian, so neither can move the ZEC alone. The user can't double
  spend, and the custodian can't steal.
- **After `T`,** the user alone can take the ZEC back. The custodian
  can't freeze it forever.
- **It is feasible on Zcash as it is.** CLTV (BIP-65) has applied since
  genesis. The script is about 115 bytes (about 140 with the tag), well
  under the 520-byte P2SH limit, with 3 sigops against the policy limit
  of 15.
- **There is no CSV** (BIP-68/112 don't apply to Zcash, and ZIP-112 is
  a Draft). `T` must be an absolute height, so every vault has a fixed
  end date, not "N blocks after deposit".
- **Sova verifies it with SIP-4.** A contract rebuilds the script from
  `(user key, custodian key, T, tag)`, hashes it with the SHA-256 and
  RIPEMD-160 precompiles, and checks the deposit output with
  `txOutput`.
- `K_custodian` can be an MPC-derived key owned by a NEAR contract, or
  a P2SH federation.

### 3.3 Fungibility: where it fails

Say Alice deposits and Bob ends up holding the wZEC. To pay Bob ZEC,
some transaction must spend Alice's vault, which before `T` needs
**Alice's signature**. There are three ways to get it:

1. **Alice signs when Bob redeems.** Then Bob's money depends on
   Alice's cooperation. She can refuse, or wait for `T`. Forcing her
   requires collateral, which is option C1 (XCLAIM-style) with its
   price-oracle problem.
2. **Alice pre-signs when she deposits.** She doesn't know Bob, so her
   signature can't commit to Bob's address. ZIP-244 allows only ALL,
   NONE and SINGLE, each optionally with ANYONECANPAY. ALL and SINGLE
   fix outputs she can't know. **NONE** commits to no outputs, so the
   custodian, who adds the second signature, picks the destination
   alone, **and that is custody again**, one vault at a time.
   ANYONECANPAY only frees the other inputs and doesn't help with the
   destination. A pre-signed spend to a fixed "redemption pool"
   address moves the custody to whoever controls the pool.
3. **wZEC is per vault.** A "vault receipt" token can move between
   holders, but redeeming it still needs Alice, so it isn't money.

**Covenant-free constructions from Bitcoin don't carry over to Zcash's
transparent script:**

| Construction | What it needs | On Zcash transparent |
|---|---|---|
| **BitVM2 bridge**: an n-of-n committee pre-signs a transaction graph and deletes its keys (1-of-n honest), operators front payouts and are reimbursed after a dispute window | Taproot trees of large disprove scripts, one-time signatures, **relative timelocks** | **No.** No Taproot, no CSV, a 10 kB script limit and a 520-byte redeem script. Worse, "delete the keys" clashes with Zcash's upgrade cycle (below) |
| **Babylon staking**: timelock + covenant-committee path + slashing via extractable one-time signatures (EOTS) | Taproot/tapscript `OP_CHECKSIGADD`, **CSV**, **Schnorr** EOTS | **No.** It lacks all three |
| **Lightning-style pre-signed exits** | Non-malleable txids (v5 has them), relative timelocks for revocation | **Partial.** Absolute-lock designs are possible, but see below |
| **DLCs**: 2-of-2 plus ECDSA adaptor signatures, oracle-triggered | 2-of-2, a non-malleable funding txid, an absolute-lock refund | **Yes in principle.** The dlcspecs ECDSA adaptor scheme predates Taproot. It is peer-to-peer and needs an oracle key, which is new trust |

**The Zcash-specific catch: pre-signed transactions die at every
network upgrade.** ZIP-244 commits both the signature digest **and the
v5 txid** to the consensus branch ID, and nodes reject a transaction
whose branch ID doesn't match the current upgrade (Zebra
`WrongConsensusBranchId`). v6 (ZIP-229) does the same.

So every pre-signed transaction, and every pre-signed child of it,
becomes unusable at the next network upgrade and must be re-signed by
everyone involved. Recent upgrades came fast, and two were emergencies:

| Upgrade | Date | Gap from the previous one |
|---|---|---|
| NU6.1 | 2025-11-24 | |
| NU6.2 (emergency) | 2026-06-03 | ~6 months |
| NU6.3 | 2026-07-28 | ~8 weeks |

**Plan for re-signing every 2–12 months, sometimes with under a week's
notice** *(estimate)*. The CLTV refund path is signed at spend time, so
it survives upgrades. Anything that relies on long-lived pre-signed
transactions does not.

**Conclusion:** fungible, redeemable wZEC needs some party that can
move the backing ZEC **without the depositor**. That party is a
custodian. The best we can do is choose it and bound it, which is §1.

### 3.4 What the timelock means for wZEC

At `T`, Alice can take her ZEC back alone. So **any wZEC minted against
her vault is unbacked from `T` onward** unless it was burned first. An
"expiring wZEC" (one series per `T`, which must be redeemed before
`T − Δ`) doesn't fix this, because redeeming before `T` still needs
Alice or a custodian able to spend alone (§3.3). The timelock turns
every claim on the vault into a **dated claim on one person's
cooperation**. That is a loan or a bond, not money.

### 3.5 Where self-custody vaults fit

| Use | Script | Who can move the ZEC before `T` | Custodian or Sova→NEAR channel needed | Verdict |
|---|---|---|---|---|
| **Time-locked ZEC credit**: prove you've locked Z ZEC until `T`. Uses include Sybil resistance, access gating, reputation and proof of funds | `<T> CLTV DROP <K_user> CHECKSIG` (+ tag) | **Nobody**, the user included | **No** | **Works today, with zero custody.** Needs SIP-4 v1 plus a contract. Not a peg; it moves no ZEC. It is **not collateral**, because nobody can seize it |
| **Borrow against locked ZEC** (peer-to-peer): Alice locks, borrows on Sova, and the lender gets the ZEC on default | 2-of-2 + CLTV refund; Alice pre-signs a liquidation to the lender's address with SIGHASH_ALL | The custodian, with Alice's pre-signature, and only to the lender | **Yes.** The custodian must learn "defaulted" from Sova (the crux, §1.4) | **Research.** The loan must end before `T` and before the next upgrade, or be re-signed. An emergency upgrade makes the collateral unseizable until re-signed |
| **Fungible wZEC** | Any of the above | — | — | **Doesn't work** without a custodian able to spend alone. Use §1 |

**Summary:** Rob's instinct is right for **ZEC-backed, non-transferable
credit**, and its purest form, the time-locked lock, needs no custodian,
no NEAR, and no peg. Fungible wZEC needs a custodian, and the NEAR MPC
design in §1 is the way to bound it.

---

## 4. Recommendation

### 4.1 SIP-5 direction: "wZEC, custodied by NEAR Chain Signatures"

**Sova contracts.** Keep the custodian pluggable, as the options paper
recommended.
- **Deposits** go to per-account tagged P2SH `<evm20> DROP <K_vault>
  CHECKSIG`, where `K_vault` is the MPC-derived key of the NEAR vault
  contract.
- **Mint** is SIP-4-verified and signer-free.
- **Withdrawal requests** burn wZEC and name a transparent destination.
- **Monitoring** is Phase 1 of §2.5, with an automatic halt only and no
  pause key.
- **Parameters** are fixed at deploy and change only through a SIP and a
  redeploy.
- **Caps** are hard-coded and per deposit.

**NEAR vault contract.**
- **The account holds no access keys and has no upgrade method.** A
  change means a new contract, a new vault address, and users
  migrating.
- **It builds each Zcash transaction and computes the ZIP-244/ZIP-229
  sighash itself**, reusing Omni's pattern. It signs only payouts that
  match verified requests, with change to vault scripts.
- **It enforces a withdrawal delay and a daily outflow cap.**
- **Fees** are paid by peg users. The project takes nothing, and burn
  stays pure.

**Sova→NEAR channel, in order of preference.**
1. **Foreign-chain verification**: MPC foreign-chain verification with
   Sova added and **each operator querying its own Sova node**. Ask
   NEAR only once the SIP-4 escrow demo runs, per the outreach rule.
2. **Fallback: independent attestors.** An m-of-n set of named,
   independent attestors that includes neither the project nor Rob,
   behind the same delay and cap.
3. **Long term: a Sova light client on NEAR**, once Sova blocks are
   light-client-checkable. That is its own consensus SIP.

**Disclosed plainly in SIP-5:**
- 11 of 17 MPC operators can sign anything today.
- TEE is not yet enforced (9 of 17 nodes attested).
- `v1.signer` can be upgraded instantly by the same 11.
- The Sova→NEAR channel's trust, whichever is used.
- The theft proof detects theft; it does not prevent it.

**Gates, unchanged:** no peg at launch; a TAZ-only testnet with a cap
after the SIP-5 draft is public; mainnet only after an audit of the
SIP-4 precompile, the Sova contracts and the NEAR vault contract.

*Effort (estimate):*

| Part | Estimate |
|---|---|
| Sova contracts | 2–3 weeks |
| NEAR vault contract (Omni's Zcash code as reference) | 3–4 weeks |
| NEAR testnet run | 1–2 weeks |
| Phase 2 light client plus an Ed25519 precompile SIP | ~4 weeks, later |

NEAR's side of (b) runs on NEAR's timeline.

### 4.2 Self-custody

- **Time-locked ZEC credit** is a zero-custody app. Ship it after
  SIP-4 v1, alongside the D1 escrow. It is not a peg, so it stays out
  of SIP-5, and it gives "ZEC-backed" a use with no custodian at all.
- **2-of-2 collateral vaults** stay research until the Sova→NEAR
  channel exists, and until someone solves re-signing across network
  upgrades.

### 4.3 Decisions (Rob, 2026-09-23)

Rob agreed with this paper's recommendations. Recorded decisions:

- **Wrapped ZEC ships, custodied by NEAR.** SIP-5 targets wZEC held by
  NEAR Chain Signatures behind a keyless, immutable NEAR vault contract.
  The custody is **stated plainly wherever wZEC appears**: holding wZEC
  means relying on NEAR's MPC network (11-of-17 today, TEE not yet
  enforced). It is a convenience layer, not Sova's headline, and the
  custodian stays pluggable on the Sova side.
- **Sova's part is the contracts and the monitoring.** The project holds
  no signer key and runs no relayer or RPC that NEAR trusts. Vault
  monitoring comes from Zcash alone through SIP-4, so `spentBy` ships
  before any peg testnet. No NEAR-reading precompile.
- **How NEAR learns of burns:** ask NEAR for foreign-chain verification
  of Sova (each MPC operator querying its own Sova node) **after a SIP-4
  demo exists** (partner-outreach rule). Fallback: independent attestors
  behind a delay and a daily cap.
- **Self-custody vaults are not wZEC.** The custodian-free time-locked
  ZEC lock is an app after SIP-4; 2-of-2 collateral vaults stay research.
- **Light-client-checkable blocks** are now drafted as SIP-6 (sealer
  signatures), proposed for the testnet reset rather than after mainnet,
  pending Rob's SIP-6 decisions.
- **Positioning:** the headline is **"the EVM that can see Zcash"**
  (SIP-4): contracts verify real ZEC payments on Zcash with no bridge,
  oracle or custodian. Burn-to-mine is how the chain starts; wZEC via NEAR
  is a disclosed-custody convenience on top.

The original recommendations, for the record:

1. **Custodian for SIP-5: NEAR Chain Signatures behind a keyless,
   immutable NEAR vault contract?** Recommended: **yes.** Accept and
   disclose the 11-of-17 trust with TEE not yet enforced, and keep the
   Sova contract custodian-pluggable.
2. **How NEAR learns about burns.** Recommended: **ask NEAR, after the
   demo, for foreign-chain verification of Sova with each MPC operator
   querying its own Sova node.** The fallback is independent attestors
   behind a 12–24 h delay and a daily cap. Never a relayer or RPC the
   project runs.
3. **NEAR monitoring.** Recommended: **no NEAR precompile.**
   - Monitor from Zcash alone through SIP-4, which means `spentBy`
     ships before any peg testnet.
   - A NEAR light-client contract on Sova, backed by a pure Ed25519
     precompile, comes later and only if attribution and governance
     alerts are worth one more SIP.
4. **Self-custody vaults.** Recommended: **not as wZEC.** Build the
   no-custodian time-locked ZEC lock as an app after SIP-4, and leave
   2-of-2 collateral vaults as research.
5. **Light-client-checkable Sova blocks** (sealer signatures and an
   equivocation rule). Recommended: **not now; schedule after
   mainnet.** It is the prerequisite for any trust-minimized bridge out
   of Sova, so it should be written down as a known gap now.

---

## Sources (accessed 2026-09-23)

**NEAR MPC / Chain Signatures**
- Live read-only views on `v1.signer` through `https://rpc.mainnet.near.org`
  (`version` = 3.15.1; `state`: 17 participants, threshold 11, domains
  0–3; `get_attestation` for each participant: 9 Dstack, 8 Mock;
  `get_available_foreign_chains`), around block 216,910,600.
- near/mpc at tag 3.15.1: https://github.com/near/mpc/tree/3.15.1
  - `crates/contract/src/api/sign.rs`, `api/common.rs`, `api/keys.rs`, `api/attestation.rs`
  - `crates/near-mpc-crypto-types/src/kdf.rs`, `src/sign.rs`, `src/primitives.rs`
  - `crates/contract/src/crypto_shared/kdf.rs`
  - `crates/near-mpc-contract-interface/src/deposits.rs`
- near/mpc main at `b4e5d19`: https://github.com/near/mpc/tree/b4e5d1986253624a2840a04262803c27fce9832b
  - `crates/contract/src/api/{governance.rs,update.rs}`
  - `crates/contract/src/state/{running.rs,resharing.rs}`
  - `crates/contract/src/primitives/thresholds.rs`
  - `crates/contract/src/api/foreign_chain/{requests.rs,support.rs}`
  - `crates/near-mpc-contract-interface/src/types/foreign_chain.rs`
  - `docs/archive/design/foreign-chain-transactions.md`
  - `docs/design/calculating-supported-foreign-chains.md`
  - `docs/guide/operating-an-mpc-node.md`
  - `CHANGELOG.md`
- NEAR docs (still say 8 nodes): https://docs.near.org/chain-abstraction/chain-signatures
- Yield timeout (200 blocks), nearcore `core/parameters/res/runtime_configs/parameters.yaml`:
  https://github.com/near/nearcore ; https://docs.near.org/smart-contracts/anatomy/yield-resume

**Omni Bridge and Zcash on NEAR**
- https://docs.near.org/chain-abstraction/omnibridge/overview ;
  https://docs.near.org/chain-abstraction/omnibridge/how-it-works
- https://github.com/Near-One/omni-bridge (README; `near/omni-prover/mpc-omni-prover`)
- Zcash connector, `Near-One/btc-bridge` branch `omni-main`:
  https://github.com/Near-One/btc-bridge/tree/omni-main
  - `contracts/satoshi-bridge/src/zcash_utils/{psbt_wrapper.rs,transaction.rs,orchard_policy.rs}`
  - `src/kdf.rs`, `src/deposit_msg.rs`
  - `migrate/create_proposal.sh` (Sputnik DAO upgrades)
- Zcash light client on NEAR: https://github.com/Near-One/btc-light-client-contract
  (`contract/src/zcash.rs`, Equihash 200/9; `contract/src/lib.rs`,
  `submit_blocks` is `#[trusted_relayer]`)

**NEAR light client, Rainbow, Ed25519**
- NEAR light client spec: https://nomicon.io/ChainSpec/LightClient
- Epochs (43,200 blocks; 100 producer seats): https://docs.near.org/protocol/network/epoch
- `view_state` `include_proof`: nearcore `core/primitives/src/views.rs`
- `NearBridge.sol`, `Ed25519.sol`, README gas figures:
  https://github.com/Near-One/rainbow-bridge
- 4-hour challenge period: https://aurora.dev/blog/the-fast-rainbow-bridge-for-near-to-ethereum-token-transfers-is-live
- August 2022 fake-block attempt: https://www.coindesk.com/tech/2022/08/23/hackers-lose-5-ether-while-trying-to-attack-near-protocols-rainbow-bridge
- Ed25519 in Solidity, ~500k gas: https://ethresear.ch/t/verify-ed25519-signatures-cheaply-on-eth-using-zk-snarks/13139
- EIP-665 (Stagnant): https://eips.ethereum.org/EIPS/eip-665 ; Celo CIP-25:
  https://github.com/celo-org/celo-proposals/blob/master/CIPs/cip-0025.md
- ZK NEAR light client (R&D): https://github.com/near/near-light-client

**Zcash**
- ZIP-244: https://zips.z.cash/zip-0244 ; ZIP-243: https://zips.z.cash/zip-0243 ;
  ZIP-200: https://zips.z.cash/zip-0200 ; ZIP-203: https://zips.z.cash/zip-0203 ;
  ZIP-229: https://zips.z.cash/zip-0229 ; ZIP-112 (Draft): https://zips.z.cash/zip-0112
- zcash_script 0.4.5 (CLTV only, no CSV, limits): `src/opcode/mod.rs`,
  `src/interpreter.rs`, `src/signature.rs`, `src/script/mod.rs`
- Zebra 6.3.0 (`research/zebra-upstream`, `f5c5277`):
  - `zebra-script/src/lib.rs` (consensus flags P2SH | CLTV)
  - `zebra-consensus/src/transaction/check.rs` (branch-ID rule, P2SH sigops)
  - `zebra-chain/src/parameters/constants.rs` (upgrade heights)
- Upgrade dates:
  - NU6.1: https://zips.z.cash/zip-0255
  - NU6.2: https://www.cryptotimes.io/2026/06/03/zcash-activates-nu6-2-hard-fork-following-double-spend-risk-discovery/
  - NU6.3: https://www.coindesk.com/tech/2026/07/28/zcash-seals-usd1-7-billion-shielded-pool-as-ironwood-upgrade-activates

**Bitcoin constructions**
- BitVM2 paper: https://eprint.iacr.org/2025/1158.pdf ; https://bitvm.org/bitvm2.html
- Babylon staking script: https://github.com/babylonlabs-io/babylon/blob/main/docs/staking-script.md
- DLC ECDSA adaptor signatures: https://github.com/discreetlogcontracts/dlcspecs/blob/master/ECDSA-adaptor.md ;
  https://github.com/discreetlogcontracts/dlcspecs/blob/master/Transactions.md

**Sova (this repo)**
- `sips/sip-2.md`: rank recovered from withdrawals; no sealer metadata.
- `crates/engine/src/candidates.rs`: preference is `(rank asc, hash asc)`.
- `docs/design/gossip-v1.md`: depth-lagged finality of 64.

## Not verified

- `view_state` proofs bound to a light-client header for code hash,
  access keys or MPC state. Treat state proofs as unproven; logs are
  the supported path.
- Whether Omni's Zcash light client has `skip_pow_verification`
  switched off on mainnet, and the mainnet account IDs of Omni's Zcash
  contracts.
- NEAR's appetite for adding Sova to foreign-chain verification, or
  for operator-run Sova nodes as the provider. Not asked, per the
  outreach rule.
- The NEAR testnet MPC deployment and its foreign-chain support, for a
  TAZ-side prototype.
- Hardware requirements for a NEAR RPC node (the §2.1 cost argument is
  qualitative).
- All gas and effort numbers marked *(estimate)*.
