# SIP-5: Wrapped ZEC (wZEC), custodied by NEAR Chain Signatures

- Status: **Draft, design only** (2026-09-23). No peg code exists. Needs
  Rob's calls in "Decisions for Rob" (§10) before anything is built.
- Direction: decided by Rob on 2026-09-23 (`docs/design/zec-peg-v2.md`
  §4.3). This SIP turns that direction into a specification.
- Implementation: none. Planned homes are `contracts/src/wzec/`
  (`WZEC.sol`, `WzecBridge.sol`) on Sova, and a separate Rust NEAR
  contract (the vault) in its own repository with a reproducible build.
- Author: Sova (orchestrated draft)
- Depends on: SIP-4 v1 (code complete), SIP-4 v1.1 `spentBy` (not
  started), SIP-6 (draft), and NEAR support for verifying Sova (external).
  See §8.
- Consensus change: **none.** Everything here is contracts. The only
  node-side work is SIP-4 v1.1, which is SIP-4's own change.

## Custody, stated first

**wZEC is custodied.** The ZEC behind it sits in a Zcash address whose
key is held by NEAR's MPC network (Chain Signatures). Spending it needs
**11 of the 17 NEAR MPC operators**. Hardware enclaves (TEE) do not yet
constrain those operators: 9 of the 17 run real TDX attestations. A
keyless, immutable NEAR contract decides what the network is asked to
sign. The Sova project holds no key, runs no relayer and runs no RPC
that NEAR trusts.

Every place wZEC appears, in the UI, the docs, the token's own metadata
and any announcement, says this in the same breath. wZEC is **never**
described as trustless or custody-free.

## Summary

wZEC is ZEC as an ERC-20 on Sova, 1 wZEC unit per zatoshi of ZEC held in
a NEAR-custodied vault on Zcash.

- **Deposit.** Each Sova account has its own Zcash deposit address: a
  tagged P2SH over the vault key. Pay it from any Zcash wallet,
  including from a shielded balance. Once the payment is `minConf` deep,
  anyone can claim it. The Sova contract checks the payment through the
  SIP-4 precompile and mints. **No signer takes part in minting.**
- **Withdraw.** Burn wZEC on Sova and name a transparent Zcash address.
  The Sova contract writes out the exact Zcash transaction it expects
  (inputs, outputs, fee). NEAR learns of it, waits a delay, checks its
  own limits, builds the ZIP-244 sighash itself and asks the MPC network
  to sign. Anyone broadcasts the signed transaction.
- **Watch.** Sova keeps its own ledger of every vault coin, fed only by
  Zcash through SIP-4. Any vault spend that doesn't match a withdrawal
  Sova planned is provable on Sova by anyone, and a proof **halts minting
  automatically**. Reserves are live contract state. There is no admin
  key and no pause key.

## 1. Motivation

SIP-4 makes Sova **the EVM that can see Zcash**: a contract can verify
that a real ZEC payment landed on Zcash, with no bridge, oracle or
custodian. That covers "pay ZEC, get something on Sova" and custody-free
ZEC↔SOVA trades, and it stays Sova's headline.

What SIP-4 can't do is put ZEC *inside* contracts. The ZEC never leaves
Zcash, so there is no ZEC collateral, no ZEC in an AMM pool, no
ZEC-denominated lending, and no moving ZEC between Sova accounts in one
transaction. Those need a ZEC-denominated token on Sova, and fungible,
redeemable wrapped ZEC always needs someone who can move the backing ZEC
without the depositor (`zec-peg-v2.md` §3.3). That someone is a
custodian. No design removes it.

So this SIP picks the custodian carefully and makes the most of what
Sova can see:

- **The custodian is not the project.** It is NEAR's MPC network behind
  a contract nobody can change.
- **Minting needs nobody's word.** Every mint is backed by a Zcash
  payment that every Sova node has checked against its own Zcash node.
- **The vault is watched from Zcash.** Sova sees every vault coin and
  every spend of one. A spend that Sova didn't plan is a public,
  on-chain proof, and minting stops by itself.
- **Reserves are a contract read**, not an attestation.

wZEC is a convenience layer on top of SIP-4, not a replacement for it.
Custody-free use of ZEC (SIP-4 checkout and escrow) stays the default
recommendation wherever it fits.

## 2. Overview

```
 Zcash                           Sova                              NEAR
 ─────                           ────                              ────
 user pays t3 deposit addr ──►  claimDeposit: SIP-4 txOutput
   (tagged P2SH, K_vault)        + minConf → mint wZEC
                                 (vault ledger += outpoint)

                                 requestWithdrawal: burn wZEC,
                                 plan = inputs + outputs + fee  ──► vault.submit: verify the
                                 (inputs locked)                     Sova log (§4.2), delay,
                                                                     re-verify, daily cap,
                                                                     build ZIP-244 sighash,
                                                                     v1.signer.sign per input
 anyone broadcasts the     ◄─────────────────────────────────────── signed tx logged on NEAR
 signed payout tx
                                 provePayout: SIP-4 spentBy +
                                 txOutput match the plan → close;
                                 change joins the vault ledger

 any other spend of a vault ──► proveBreach: halt minting,
 coin                            mark reserves down
```

Actors:

| Actor | Role | Trusted for |
|---|---|---|
| **Sova bridge contract** (`WzecBridge`) | Deposit addresses, mint, burn, payout plans, vault ledger, monitoring | Nothing beyond its code. No owner, no upgrade |
| **wZEC token** (`WZEC`) | ERC-20; only the bridge mints and burns | Nothing beyond its code |
| **NEAR vault contract** | Checks plans, enforces delay and cap, builds sighashes, requests signatures | Its code. No access keys, no upgrade method |
| **NEAR MPC network** (`v1.signer`) | Holds the key share for `K_vault`; signs what the vault asks | **Custody:** 11 of 17 operators |
| **Sova→NEAR channel** | Tells the vault that a plan exists on finalized Sova (§4.2) | **Honest reporting** of Sova logs |
| **Anyone** | Claims deposits, submits plans to NEAR, broadcasts payouts, submits proofs | Nothing; every action is verified |

## 3. Deposits

### 3.1 Deposit addresses: tagged P2SH over one vault key

Each Sova account `a` gets its own Zcash P2SH address:

```
tag(a)        = bytes20(keccak256("SovaWZEC/v1/deposit" ‖ uint64 chain_id ‖ bridge ‖ a))
redeem(tag)   = 0x14 ‖ tag ‖ 0x75 ‖ 0x21 ‖ K_vault ‖ 0xac
              =  <tag> OP_DROP <K_vault> OP_CHECKSIG                (57 bytes)
script(tag)   = OP_HASH160 <ripemd160(sha256(redeem(tag)))> OP_EQUAL
```

- `K_vault` is one compressed secp256k1 key: the key the NEAR MPC network
  derives for (the NEAR vault account, the path `sova-wzec/<network>/v1`).
  Derivation is public, so `K_vault` is known before anything is deployed.
- The bridge computes `script(tag(a))` with the EVM's own SHA-256 and
  RIPEMD-160 precompiles. The address is `t3…` on mainnet and `t2…` on
  testnet. The bridge exposes the script; wallets and the UI encode it.
- Change from payouts goes to the same template with a fixed
  `CHANGE_TAG = bytes20(keccak256("SovaWZEC/v1/change" ‖ chain_id ‖ bridge))`.
- The tag binds a deposit to one account **and** to one deployment on one
  chain. A payment to a testnet address can't be claimed on mainnet or on
  a later deployment, even if a key were ever reused.

**Why this and not NEAR-derived per-account keys.** The alternative gives
each account its own MPC-derived key (`path = account`) and a plain
`t1…` P2PKH address. The EVM can't check that address: derivation needs
SHA3-256 (not keccak) and secp256k1 point addition, neither of which the
EVM offers. The bridge would have to take someone's word that an address
belongs to the vault, and that is exactly the signer-trusted mint this
SIP exists to avoid. Per-account keys also add no safety, because the
same 11 of 17 operators hold every derived key anyway. The tagged P2SH
keeps the mint fully verifiable on Sova. Its costs:

- The address is P2SH (`t3`). Most wallets and exchanges send to P2SH,
  but this is **not yet tested wallet by wallet** (to verify: Zashi/Zodl,
  YWallet, zcashd/zallet, and the exchanges that matter).
- Spending a P2SH input costs a little more: about 172 bytes per input
  instead of about 150 *(estimate)*, so roughly 15% more in ZIP-317 fees
  on inputs.
- The redeem script is non-standard in shape (not a multisig template),
  but Zcash's P2SH policy checks only the sigop count (1 here, the limit
  is 15) and the 520-byte push limit (57 here).

**Privacy.** Anyone can compute any account's deposit address, so a
deposit publicly links that Zcash output to that Sova account. The payer
can still fund it from a shielded balance (z→t), which hides where the
ZEC came from, exactly as for burns.

### 3.2 Claiming a deposit (mint)

`claimDeposit(txid, vout, account)`. Anyone can call it; the wZEC always
goes to `account`.

1. `ZcashLib.requireOutputPays(txid, vout, depositScript(account),
   MIN_DEPOSIT, MIN_CONF)`: the output exists on the anchored Zcash chain,
   pays at least `MIN_DEPOSIT` to exactly that account's script, and is at
   least `MIN_CONF` deep. Its value `v` is read from the output.
2. `spentBy(txid, vout)` (SIP-4 v1.1) returns *unspent*. A vault output
   that is already spent before it was ever claimed is a breach (§5.2),
   not a deposit.
3. The outpoint has never been claimed, refunded or tracked before.
4. Mintability (§3.3): `v ≤ MAX_DEPOSIT`, the total cap has room, and
   minting isn't halted.
5. Mint `v − DEPOSIT_FEE` wZEC to `account`. Add `(txid, vout, v,
   tag(account))` to the vault ledger as a free coin.

Nobody signs anything. A Zcash reorg that removes the payment reorgs Sova
with it (SIP-4 §7), and the mint disappears on every node at once.
`MIN_CONF` exists for whatever holders do *outside* Sova in the meantime.
Payouts wait much longer anyway (§4.7).

### 3.3 Deposits that can't be minted

A payment can reach a deposit address and still not be mintable: below
`MIN_DEPOSIT`, above `MAX_DEPOSIT`, over the total cap, or during a mint
halt. The ZEC is already in the vault, so it must be recoverable:

- `claimDeposit` records it as **held** (`DepositHeld` event) instead of
  reverting, when the output is otherwise valid.
- A held deposit can be claimed again later if the reason was the cap or
  an overdue halt and that has cleared.
- **The tagged account can always refund it** with
  `refundDeposit(txid, vout, destScript)`. That is a normal withdrawal
  plan (§4) with exactly that one input and no change, paying
  `v − DEPOSIT_FEE − WITHDRAW_FEE` to `destScript`. It spends only that
  coin, so it is never affected by a haircut (§5.4).
- Payments below `DEPOSIT_FEE + WITHDRAW_FEE` can't pay for their own
  refund. They stay in the vault as surplus backing. The UI must refuse
  to produce such a payment request.

The UI checks capacity before showing a deposit address, so a held
deposit should be rare.

## 4. Withdrawals

### 4.1 Request and plan (on Sova)

`requestWithdrawal(amountZat, destScript, inputs)`:

1. `destScript` is a standard transparent script: P2PKH (25 bytes, `t1`/
   `tm`) or P2SH (23 bytes, `t3`/`t2`). Nothing else. Payouts are
   transparent in v1 because only transparent outputs can be verified on
   Sova. The user shields afterwards.
2. `amountZat ≥ MIN_WITHDRAWAL`, and the rolling request window has room:
   the sum of requests in the last `WINDOW` anchor blocks stays within
   `DAILY_CAP` (§4.7). Otherwise it reverts, and the user retries later.
3. The bridge burns `amountZat` wZEC from the caller.
4. `payout = amountZat − WITHDRAW_FEE`, scaled down pro rata only after a
   proven shortfall (§5.4).
5. **The caller proposes the inputs** (a UI picks them). The bridge
   checks that each is a free coin in its ledger, that there are at most
   `MAX_INPUTS`, and computes the exact ZIP-317 fee of the transaction
   the plan describes (it knows every input and output size).
   `change = Σ inputs − payout − fee` must be `0` or at least
   `MIN_CHANGE`. The inputs become **locked** to this plan.
6. It emits `WithdrawalPlanned` with the full plan: id, account,
   `destScript`, `payout`, `change`, `fee`, every input
   `(txid, vout, value, tag)`, the deadline, and
   `planHash = keccak256(abi.encode("SovaWZEC/v1/plan", chain_id, bridge,
   plan))`.

**Sova plans the transaction, NEAR checks and signs it.** The alternative
is for the NEAR contract to keep its own view of the vault's coins and
choose inputs. That needs a second, NEAR-side ledger fed through the same
channel, makes the immutable NEAR contract larger, and means Sova's
theft check has to guess what NEAR will sign. With Sova planning, there
is one ledger, every expected spend is public before it is signed, and
the theft check (§5.2) compares a spend against an exact plan.

The NEAR contract never has to trust the input values in a plan. ZIP-244
signature digests commit to **every input's amount and scriptPubKey**, so
a plan with a wrong value or script gets a signature that Zcash rejects.
A lie about inputs can make a payout fail. It can't make one steal.

### 4.2 How NEAR learns of the burn

The NEAR vault may sign only for a plan that really exists on canonical,
final Sova. This is the weakest link of any Sova→NEAR design
(`zec-peg-v2.md` §1.4). Three channels exist. **Each vault deployment is
fixed to exactly one of them**; changing channel means a new vault.

**(a) Preferred: NEAR MPC foreign-chain verification of Sova, each MPC
operator querying its own Sova node.**

- NEAR's MPC network already verifies transactions on other chains
  (`verify_foreign_transaction`). It is live on mainnet for Bitcoin,
  Abstract, Starknet and Aptos, and **not for Sova**. Adding Sova is a
  near/mpc code change, a contract upgrade vote and node upgrades: NEAR's
  decision, on NEAR's timeline.
- The vault calls `verify_foreign_transaction` with the Sova transaction
  id, the extractors `BlockHash` and `Log{log_index}`, and the finality
  level `Finalized`. The MPC network signs the extracted log with its
  domain-3 key, and the vault checks that signature.
- **The shape Sova asks for: every MPC operator runs its own Sova node
  (with its own zebrad) and queries only that.** Then a verified log means
  11 independent full Sova nodes agree, which mirrors Sova's own-node rule.
  NEAR's current model is a whitelist of provider URLs with a quorum per
  chain, so this needs NEAR to agree to operator-local providers.
- **If the queried RPCs were the project's, the project would be the
  oracle.** That is ruled out, on testnet too.
- `Finalized` must mean Sova's depth-lagged finality: 64 blocks, about
  80 minutes (`docs/design/gossip-v1.md`). Sova's producers already set
  `finalized` 64 blocks behind the head (`crates/engine/src/miner.rs`,
  `FINALITY_DEPTH`). A node that doesn't seal, which is what an MPC
  operator would run, must be checked to report the same tag (to verify).

**(b) Fallback: independent attestors.** An m-of-n set of named attestors
(recommended 3-of-5 on testnet), each running its own Sova node, signs
`planHash` once the plan is 64 blocks deep. The vault checks m
signatures against keys fixed at its deployment. **Neither the project
nor Rob is ever an attestor.** This is plain trust in the attestors, and
it is disclosed as such, alongside the MPC trust.

**(c) Long term: a Sova light client on NEAR.** It needs Sova blocks that
name their author, which is **SIP-6** (sealer signatures). Even with
SIP-6 it still needs a proof of rank and of execution (SIP-6 §6), so it
is research, not a v1 channel.

**SIP-6 is a prerequisite for a peg testnet, whichever channel is used.**

- Without it, anyone can replace the tip block with a lower-hash copy at
  no cost (SIP-6 §1.1) and drop chosen transactions. The peg's safety
  runs on permissionless transactions that must land: breach proofs,
  payout proofs, withdrawal requests. A free, repeatable tip censor is not
  acceptable under a custodial peg.
- It is the hard prerequisite for (c), and it lets attestors and MPC
  nodes name the author of the block they attest to.

**Double verification.** The vault verifies a plan when it is submitted,
and again when the delay ends (§4.3). A plan whose Sova transaction is no
longer on the canonical chain at the second check is dropped. That makes
the effective finality of a burn 64 Sova blocks **plus the delay**, which
also covers a Zcash reorg deep enough to reorg Sova.

### 4.3 NEAR signs (the vault contract)

All permissionless. Whoever calls pays the NEAR gas and the storage
deposit. A UI can do this for the user, and so can the user.

1. **`submit(sova_tx, log_index)`.** Verify through the channel (§4.2).
   Check that the log came from the configured bridge address with the
   `WithdrawalPlanned` topic, and decode the plan. Then check the plan
   against the vault's own rules:
   - outputs are exactly `[destScript: payout]` plus, if `change > 0`,
     `[CHANGE script: change]`;
   - `destScript` is P2PKH or P2SH;
   - at most `MAX_INPUTS` inputs;
   - `fee ≤ n_inputs × DEPOSIT_FEE + WITHDRAW_FEE`, the most the bridge
     can ever charge;
   - `payout > 0` (it can be below the usual minimum after a haircut);
   - no input is already bound to a different `planHash`.

   Record the plan under `planHash`, bind its inputs to it, and set
   `ready_at = now + DELAY`.
2. **`release(planHash, tx_version, branch_id)`**, after `ready_at`:
   - verify again; if it fails, drop the plan and unbind its inputs;
   - check the rolling 24-hour outflow cap (`DAILY_CAP`); if releasing
     would exceed it, wait;
   - **build the transaction itself** (v5 or v6, `nLockTime = 0`,
     `nExpiryHeight = 0`, the plan's inputs and outputs, each input's
     redeem script rebuilt from its tag);
   - **compute each input's ZIP-244 signature digest itself**
     (`SIGHASH_ALL`), as Omni Bridge's Zcash connector already does on
     NEAR;
   - call `v1.signer.sign` once per input with the path
     `sova-wzec/<network>/v1`, across several calls if gas requires it;
   - assemble the scriptSigs (`<sig‖0x01> <redeem>`), then store and log
     the complete raw transaction.
3. **Anyone** reads the raw transaction from NEAR and broadcasts it to
   Zcash. Broadcasting needs no trust: the transaction is already fully
   signed and pays exactly what the plan says.

**The vault never signs a digest someone hands it.** If it did, the
caller would be the custodian.

### 4.4 Closing a withdrawal on Sova

`provePayout(id, payoutTxid)`. Anyone can call it.

- `spentBy` shows **every** planned input spent by `payoutTxid`.
- `txInfo(payoutTxid)`: at least `MIN_CONF` deep, and `nOut` is exactly
  1, or 2 with change.
- `txOutput(payoutTxid, 0)` pays exactly `payout` to `destScript`.
  `txOutput(payoutTxid, 1)`, if there is change, pays exactly `change` to
  the change script.

Then the plan is paid, its inputs leave the ledger, and the change output
joins it as a free coin. The match is on **what the transaction pays**,
not on its txid, so a re-signed version of the same plan (§4.5) closes it
just as well.

### 4.5 Zcash network upgrades

Zcash signature digests commit to the consensus branch ID, and every
network upgrade changes it. Upgrades have come 8 weeks to 6 months apart,
one of them an emergency (`zec-peg-v2.md` §3.3). An immutable vault can't
hard-code branch IDs, so:

- **The caller of `release` supplies `branch_id` and `tx_version`.** This
  is safe. Every version of a plan spends the same inputs and pays the
  same outputs, so at most one of them can ever be mined. A wrong branch
  ID only produces a transaction Zcash rejects.
- **Re-signing.** A plan already signed may be signed again with another
  branch ID or version, at most once per `RESIGN_INTERVAL` (draft: 1 hour)
  so nobody can spam the MPC network. The outputs never change.
- **Tail risk, disclosed.** The vault builds v5 and v6 transactions. If
  Zcash ever retired both formats, an immutable vault could no longer pay
  out, and its ZEC would be stuck until NEAR's MPC operators acted outside
  the contract. Zcash has so far kept older formats valid across
  upgrades: v5 stays valid after NU6.3. A retirement would come through a
  ZIP with long notice, leaving time for holders to withdraw.

### 4.6 Fees

The peg charges only what Zcash charges. **The project takes nothing**,
and SIP-1 burns are untouched.

- **Deposit:** `DEPOSIT_FEE` (draft 6,000 zat) is withheld from the mint.
  It pre-pays the ZIP-317 cost of later spending that coin: about 172
  bytes of P2SH input against ZIP-317's 150-byte unit at 5,000 zat
  *(estimate; fixed against ZIP-317 at build with a 72-byte signature)*.
- **Withdrawal:** `WITHDRAW_FEE` (draft 10,000 zat) is withheld from the
  payout. It covers ZIP-317's two grace actions, which cover the outputs.
- **The actual Zcash fee** of each payout is the exact ZIP-317
  conventional fee, which the bridge computes from the plan. It is always
  at most what was charged: `5,000 × max(2, ⌈172n/150⌉) ≤ 6,000n +
  10,000` for `n` inputs. The difference stays in the vault as **surplus
  backing** that nobody can withdraw.
- **NEAR costs** (gas for `submit` and `release`, the 1 yoctoNEAR sign
  deposit, storage) are paid by whoever calls, typically cents
  *(estimate)*. No fee is paid to the MPC network today beyond that
  deposit.

### 4.7 Limits and timing

| Parameter | Testnet (this SIP) | Mainnet |
|---|---|---|
| `TOTAL_CAP` (wZEC supply + pending payouts) | 1,000 TAZ | set by a later SIP |
| `MAX_DEPOSIT` | 100 TAZ | later SIP |
| `MIN_DEPOSIT` | 0.001 TAZ (100,000 zat) | later SIP |
| `MIN_WITHDRAWAL` | 0.01 TAZ | later SIP |
| `DAILY_CAP` (Sova requests and NEAR releases, per rolling 24 h) | 250 TAZ | later SIP; a few percent of the cap |
| `WINDOW` (Sova's 24 h, in anchor blocks at 75 s) | 1,152 | 1,152 |
| `MIN_CONF` (deposits and payout proofs) | 3 | 10 (SIP-4 default), more for size |
| Sova finality before NEAR acts | 64 blocks | 64 blocks |
| `DELAY` (NEAR, then re-verify) | 1 h | 12–24 h (later SIP) |
| `DEADLINE` (request → overdue, anchor blocks) | 1,440 (~30 h) | later SIP |
| `MAX_INPUTS` | 16 *(estimate: NEAR gas per sign call)* | same |
| `MIN_CHANGE` | 10,000 zat | same |

- **Sova enforces the daily cap on requests, and NEAR enforces the same
  cap on releases.** Honest traffic never reaches NEAR's cap, so a
  deadline can't be missed through queueing. NEAR's cap is there for a
  lying channel or a Sova bug.
- **Withdrawals take hours by design.** On testnet: about 80 minutes to
  finality, 1 hour of delay, minutes to sign, then Zcash confirmation.
  On mainnet the delay dominates.
- All parameters are immutable per deployment. Changing one means a new
  deployment, which needs a SIP, and users migrate by withdrawing and
  depositing again.

## 5. Monitoring and safety on Sova

All of this comes from Zcash alone, through SIP-4. **Sova reads nothing
from NEAR**, and there is no NEAR-reading precompile. It would break the
own-node rule and add nothing to safety (`zec-peg-v2.md` §2).

### 5.1 The vault ledger and proof of reserves

The bridge knows every vault coin, because each one entered its ledger in
one of two ways: a SIP-4-verified deposit (§3.2) or a SIP-4-verified
change output (§4.4). A coin leaves the ledger only through a proven
payout or a proven breach.

- `reserves()`: the sum of ledger coins, free and locked.
- `liabilities()`: wZEC supply, plus planned payouts not yet paid, plus
  their fees.
- `reserves() ≥ liabilities()` holds by construction while nothing has
  been stolen, with the fee surplus on top. Anyone can read both numbers,
  every block.

Reserves are only as fresh as the latest proof someone submitted. The
contract can't notice a spend by itself; someone has to show it. Anyone
can run a watcher, including the project. A watcher only submits proofs
that the contract checks. It holds no key, and neither NEAR nor the
contract trusts it.

### 5.2 Theft proof: minting halts automatically

`proveBreach(txid, vout, tag)`. Anyone can call it.

1. `txOutput(txid, vout)` pays to `script(tag)`: it is a vault coin,
   whether or not the ledger tracked it.
2. `spentBy(txid, vout)` returns a spender. One confirmation is enough,
   because a Zcash reorg that removes the spend also reorgs this proof
   away.
3. The spend is **not** the payout of the plan that locked this coin: the
   coin isn't locked, or the spender fails §4.4's output checks for that
   plan.

Then:

- `halt = BREACH`, **permanently for this deployment**: no more mints.
- The coin leaves the ledger, so `reserves()` drops.
- `VaultBreach` is emitted.
- Withdrawals keep working, with a pro-rata haircut (§5.4).

This catches every way the custodian can fail on Zcash: a lying channel,
a bug in the vault contract, or MPC operators signing outside the
contract. **It detects theft; it does not prevent it.** SIP-4 has no
mempool, so the proof lands after the theft is mined. Nothing on Sova can
stop a Zcash transaction.

One gap, disclosed: SIP-4 can't see a transaction's shielded parts. A
spend that pays exactly the planned transparent outputs could route the
remainder into a shielded pool instead of the fee. The remainder *is*
the fee, capped by the vault's fee rule (§4.3), so the most such a spend
can divert is one payout's fee.

### 5.3 Overdue withdrawals: minting halts until they're paid

`proveOverdue(id)`: the anchor height is past the plan's deadline, and
the plan isn't proven paid. Then `halt = OVERDUE`, which stops new mints,
and `WithdrawalOverdue` is emitted. The halt **lifts by itself** when the
last overdue plan is proven paid. A breach halt never lifts.

An overdue plan is never cancelled, and the wZEC is never re-minted. The
NEAR side could still sign later, and re-minting would then create wZEC
with no ZEC behind it. The request stays payable forever.

### 5.4 After a proven shortfall

After a breach, `reserves() < liabilities()`. Every later payout
(`amount − WITHDRAW_FEE`) is multiplied by `reserves / liabilities` at
request time. The ratio stays the same as holders withdraw, so every
holder gets the same fraction whatever their place in the queue. Refunds
of held deposits (§3.3) are exempt, because they spend only their own
coin. The alternative, first come first served, rewards whoever exits
first and punishes everyone else (Decision 4).

### 5.5 Caps, no admin key, no pause

- The caps in §4.7 are compiled in. Testnet: **≤ 1,000 TAZ in total.**
  Mainnet caps come from a later SIP, start small, and rise only by SIP
  and a new deployment.
- **No owner, no admin key, no upgrade proxy, no pause key**, on Sova or
  on NEAR. The only stops are the automatic ones above, triggered by
  on-chain proofs anyone can check.
- The bridge deploys the token in its own constructor and is its only
  minter and burner, forever.

### 5.6 What monitoring can't do

- It can't stop a theft, only prove it after it is mined, halt minting
  and share the loss fairly.
- It can't make NEAR pay. A freeze shows up as overdue plans on Sova;
  holders can only wait.
- It can't see NEAR. It attributes nothing (who broke: channel, contract
  or operators) and gets no warning of MPC upgrades. A NEAR light client
  on Sova could add that later, backed by a pure Ed25519 precompile
  (`zec-peg-v2.md` §2.4). It is optional and outside this SIP.

## 6. Trust model

| Power | Who holds it | What bounds it | What Sova does |
|---|---|---|---|
| **Steal the vault's ZEC** | **11 of 17 NEAR MPC operators**, colluding or compromised. They can sign any digest for `K_vault` and bypass the vault contract, because that contract coordinates signing but doesn't enforce it cryptographically. TEE isn't enforced (9 of 17 attested) | Their stake and reputation. The same root key secures Omni Bridge and every other Chain Signatures user, so it is a large shared target | Breach proof: minting halts, reserves drop, haircut |
| | **The same 11, by upgrading `v1.signer`.** Upgrades apply as soon as 11 vote, with **no timelock** | As above | As above |
| | **The Sova→NEAR channel.** (a): a quorum of MPC nodes reporting a Sova log that isn't on the canonical chain. (b): m of n attestors | NEAR's `DELAY` + re-verification + `DAILY_CAP`: a lying channel drains at most the cap per day | Breach proof on the first bad payout |
| | **A bug** in the NEAR vault, the bridge, or SIP-4 | Audit (§8); caps | Breach proof, if the bug is on the NEAR side |
| | **The Sova project** | — | **Holds no key.** Nothing to bound |
| **Freeze withdrawals** | **7 of 17 MPC operators** refusing or offline (signing needs 11) | — | Overdue proof: mint halt |
| | **NEAR governance** dropping Sova from foreign-chain verification, or changing its provider rules (channel a) | — | Overdue proof. An immutable vault can't switch channel |
| | **n − m + 1 attestors** offline (channel b) | — | Overdue proof |
| | NEAR chain halt; a future Zcash upgrade that retires v5 and v6 (§4.5) | — | Overdue proof |
| **Mint unbacked wZEC** | **Nobody holding a key.** Only a bug in SIP-4 (precompile or index) or the bridge | Audit; SIP-4's differential sims | — |
| | A Zcash reorg deeper than `MIN_CONF` | It reorgs Sova too, taking the mint with it. Only effects outside Sova survive | Consistent on every node |
| **Censor a user** | The Sova block sealer, for one block. After SIP-6 that means outburning the top burner every epoch | SIP-6; every other epoch has another sealer | — |
| | **MPC operators, selectively.** They see each payout's destination, and 7 of 17 can refuse to sign particular ones | — | Overdue proof |
| | NEAR validators, for NEAR transactions | Anyone can submit, from any account | — |
| **Change the rules** | Bridge and token: **nobody** (no owner, no proxy). NEAR vault: **nobody** (no access keys, no upgrade method; anyone can check with `view_access_key_list` and a reproducible build hash) | — | — |
| | `v1.signer`: 11 of 17, instantly. NEAR protocol: NEAR's validators, through protocol upgrades, as on any L1 | — | — |

**What NEAR's 11-of-17 means in practice.** The custodian is whoever can
assemble 11 of 17 named node operators, plus the code of one NEAR
contract that nobody can change. Resharing keeps the same public key, so
operator-set changes don't move the vault address. The flip side, a
standard caveat of resharing: former operators who kept their old shares
could still sign if 11 of them colluded, so the trust reaches past the
current set (not checked in NEAR's code). NEAR's public docs still say
"8 nodes". The live contract says 17 with a threshold of 11 (checked
2026-09-23), and we cite the live figure.

**What a wZEC holder accepts:**

1. NEAR's MPC network can take or freeze the ZEC behind wZEC, and nothing
   on Sova can stop it. Sova can only prove it and share the loss evenly.
2. The channel that tells NEAR about burns is trusted too: NEAR's MPC
   nodes reading their own Sova nodes, or named attestors.
3. Withdrawals take hours, are capped per day, and pay only transparent
   Zcash addresses.
4. Deposit addresses are public per account, so a deposit links a Zcash
   output to a Sova account.
5. The contracts can't be fixed or paused. A bug means a new deployment
   and a migration, not a patch.
6. wZEC is only as good as its redemption. Its market price is its
   holders' business.

**Regulatory flag (not advice).** A custodial ZEC peg raises
custody-service and money-transmission questions, and the EU's AMLR bars
regulated providers from handling anonymity-enhancing coins from July
2027 (`zec-on-sova-options.md`). The project holds no key and takes no
fee, which helps, but it is not a legal analysis. Get one before mainnet.

## 7. Interfaces

### 7.1 wZEC token (sketch)

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20} from "forge-std/interfaces/IERC20.sol";

/// @title wZEC: ZEC held by NEAR's MPC network, as an ERC-20 on Sova.
/// @notice CUSTODIED. The ZEC behind this token is held in a Zcash address
/// whose key is controlled by NEAR Chain Signatures (11 of 17 MPC
/// operators). Not trustless. See custody().
/// 1 unit = 1 zatoshi (decimals = 8, the same as ZEC).
interface IWZEC is IERC20 {
    /// name()   = "Wrapped ZEC (NEAR custody)"
    /// symbol() = "wZEC"
    /// decimals() = 8

    /// @notice The only minter and burner, fixed at construction.
    function bridge() external view returns (address);

    /// @notice Plain-language custody disclosure, for wallets and explorers.
    function custody() external pure returns (string memory);

    /// @dev onlyBridge.
    function mint(address to, uint256 zat) external;

    /// @dev onlyBridge. The bridge only ever burns its own caller's balance.
    function burn(address from, uint256 zat) external;
}
```

The token has no owner, no pause, no blocklist and no upgrade path. EIP-2612
`permit` is optional and has no bearing on custody.

### 7.2 Sova bridge (sketch)

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title WzecBridge: deposits, withdrawals and vault monitoring for wZEC.
/// @notice No owner, no admin, no upgrade, no pause. Every parameter is an
/// immutable constructor argument. Clock = SIP-4 anchor height.
interface IWzecBridge {
    struct Coin {          // one vault output on Zcash
        bytes32 txid;      // display order (SIP-4)
        uint32 vout;
        uint64 valueZat;
        bytes20 tag;       // deposit tag, or CHANGE_TAG
    }

    struct Plan {
        uint64 id;
        address account;
        bytes destScript;  // P2PKH (25 bytes) or P2SH (23 bytes)
        uint64 payoutZat;
        uint64 changeZat;  // 0 = no change output
        uint64 feeZat;     // exact ZIP-317 conventional fee of this tx
        Coin[] inputs;     // <= MAX_INPUTS
        uint64 deadline;   // anchor height
    }

    enum Halt { NONE, OVERDUE, BREACH }
    enum Held { NONE, BELOW_MIN, ABOVE_MAX, OVER_CAP, HALTED }

    // ---- events -------------------------------------------------------
    event DepositMinted(bytes32 indexed txid, uint32 vout, address indexed account, uint64 valueZat, uint64 mintedZat);
    event DepositHeld(bytes32 indexed txid, uint32 vout, address indexed account, uint64 valueZat, Held reason);
    /// @notice The NEAR-facing event. Its layout is part of this SIP and
    /// never changes within a deployment; the NEAR vault decodes it.
    event WithdrawalPlanned(bytes32 indexed planHash, uint64 indexed id, Plan plan);
    event PayoutProven(uint64 indexed id, bytes32 payoutTxid);
    event VaultBreach(bytes32 indexed txid, uint32 vout, bytes32 spender, uint64 valueZat);
    event WithdrawalOverdue(uint64 indexed id);
    event HaltChanged(Halt halt);

    // ---- configuration (immutable) ------------------------------------
    function token() external view returns (address);
    function vaultPubKey() external view returns (bytes memory);   // 33-byte K_vault
    function nearVault() external view returns (string memory);    // NEAR account id, display only
    function custody() external pure returns (string memory);      // same disclosure as the token

    // ---- addresses ----------------------------------------------------
    function depositTag(address account) external view returns (bytes20);
    function changeTag() external view returns (bytes20);
    function redeemScript(bytes20 tag) external view returns (bytes memory);
    function depositScript(address account) external view returns (bytes memory); // P2SH scriptPubKey

    // ---- deposits -----------------------------------------------------
    /// Anyone; mints to `account`. SIP-4: txOutput + minConf + spentBy(unspent).
    function claimDeposit(bytes32 txid, uint32 vout, address account) external returns (uint64 mintedZat);
    /// The tagged account only; for held deposits. A one-input plan, no change.
    function refundDeposit(bytes32 txid, uint32 vout, bytes calldata destScript) external returns (uint64 id);

    // ---- withdrawals --------------------------------------------------
    /// Burns `amountZat` from msg.sender, locks `inputs`, emits WithdrawalPlanned.
    function requestWithdrawal(uint64 amountZat, bytes calldata destScript, bytes32[] calldata inputTxids, uint32[] calldata inputVouts)
        external returns (uint64 id, bytes32 planHash);
    /// Anyone. SIP-4: every input spentBy payoutTxid; outputs match the plan; minConf.
    function provePayout(uint64 id, bytes32 payoutTxid) external;

    // ---- monitoring ---------------------------------------------------
    /// Anyone. A vault-script output spent other than by its plan's payout.
    function proveBreach(bytes32 txid, uint32 vout, bytes20 tag) external;
    /// Anyone. Plan past its deadline and not proven paid.
    function proveOverdue(uint64 id) external;

    function reserves() external view returns (uint64 zat);
    function liabilities() external view returns (uint64 zat);
    function halt() external view returns (Halt);
    function plan(uint64 id) external view returns (Plan memory, bool paid);
    function isFree(bytes32 txid, uint32 vout) external view returns (bool);
}
```

Notes for the build:

- `planHash = keccak256(abi.encode("SovaWZEC/v1/plan", uint64(block.chainid),
  address(this), plan))`. The NEAR vault keys everything by it, so a Sova
  reorg that renumbers ids can't confuse it.
- The bridge is an **immutable, custodian-pluggable template**. The
  redeem script is `push20(tag) ‖ OP_DROP ‖ SUFFIX`, where `SUFFIX` is a
  constructor argument: `push33(K_vault) OP_CHECKSIG` for NEAR, or
  `m <K1..Kn> n OP_CHECKMULTISIG` for a P2SH federation. The Sova side
  doesn't care who holds the key.
- `K_vault` is computed off-chain from NEAR's public derivation before
  either contract is deployed. The bridge is deployed with it, the vault
  with the bridge's address, and the vault's `init` asserts that
  `v1.signer.derived_public_key(path, self) == K_vault`.

### 7.3 NEAR vault contract (responsibilities)

A Rust contract on NEAR. It has no Solidity interface, so this is prose.

- **Keyless and immutable.** After `init`, every access key on the
  account is deleted. There is no upgrade method, no owner and no pause.
  The code hash is published with a reproducible build, and anyone can
  check both the hash and the empty key list. A new version is a new
  account, a new `K_vault` and a new Sova bridge.
- **Fixed configuration** set at `init`:
  - the MPC contract (`v1.signer`) and the derivation path;
  - `K_vault`;
  - the Sova chain id, the bridge address and the `WithdrawalPlanned`
    topic;
  - the channel: foreign-chain verification at `Finalized`, or the
    attestor keys and threshold;
  - `DELAY`, `DAILY_CAP`, `MAX_INPUTS`, `DEPOSIT_FEE`, `WITHDRAW_FEE`,
    `MIN_WITHDRAWAL`, `RESIGN_INTERVAL`, and `CHANGE_TAG`.
- **`submit`**: verify, decode, check the plan's shape (§4.3), bind its
  inputs to its `planHash`, start the delay. An input is bound to at most
  one plan, forever, once that plan has been signed.
- **`release`**: re-verify, apply the rolling daily cap, build the
  v5/v6 transaction and every ZIP-244 digest **inside the contract**,
  request one MPC signature per input, and store and log the signed raw
  transaction. Omni Bridge's Zcash connector (`Near-One/btc-bridge`,
  `omni-main`, `psbt_wrapper.rs`) is the reference (licence to check).
- **`resign`**: the same plan, a new branch ID or version, at most once
  per `RESIGN_INTERVAL`.
- **Views**: plan status by `planHash`, the signed transaction, the
  outflow in the current window, and the configuration.
- **Never**: accept a digest from a caller, sign an output that isn't
  the plan's destination or change, sign an input bound to another plan,
  or read anything but the configured channel.

The vault learns nothing about deposits. It doesn't need to: every coin it
spends arrives in a plan, and ZIP-244 makes a wrong coin fail rather than
steal (§4.1).

### 7.4 What SIP-5 needs from SIP-4 v1.1

- `spentBy(txid, vout)` for outputs created at or after the epoch base
  `B`, answered as of `E_N`. It must tell three cases apart: **spent**
  (spender txid and height), **unspent as of `E_N`** (proposed: a new
  status `UNSPENT = 6`, or `OK` with a zero spender; the choice belongs
  to SIP-4), and **no such output**.
- The follower indexes `vin` outpoints for every transaction it scans,
  and rolls them back like everything else in the index.
- Nothing else. `txInfo`'s `nOut` and `txOutput` already exist in v1.

## 8. Dependencies and rollout

| Dependency | State (2026-09-23) | Needed for |
|---|---|---|
| SIP-4 v1 (anchor, `txInfo`, `txOutput`, `blockAt`, `burnInfo`, Zcash-reorg rollback) | **Code complete** on `z1/sip4-v1`, in CI. Contract side (`IZcash`, `ZcashLib`) on `release`. Ships at the testnet reset | Everything |
| SIP-4 v1.1 `spentBy` (§7.4) | **Not started** | Deposit claims, payout proofs, breach proofs: any peg testnet |
| SIP-6 sealer signatures | Draft, waiting on Rob's §10 decisions | Any peg testnet (§4.2) |
| NEAR foreign-chain verification of Sova, with operator-run Sova nodes | **External, not asked.** Outreach only once a SIP-4 demo runs on the public testnet (partner-outreach rule) | Channel (a) |
| Independent attestors | Not recruited | Channel (b), only if chosen (Decision 3) |
| NEAR vault contract | Not written. Omni's Zcash code is the reference | Any NEAR-side testing |
| NEAR testnet MPC and its foreign-chain support | **Unverified** | Phase 1 |
| Wallet support for sending to `t3`/`t2` | Unverified (§3.1) | Deposit UX |
| Legal read on custody | Not started | Mainnet |

**Phases:**

0. **Regtest box.** The bridge and token run against SIP-4 on the local
   box. A local stand-in signer replaces NEAR, labelled in code and docs
   as a test signer that never runs on a public network. Scenarios:
   deposit and claim; withdraw and prove; a stolen coin gives a breach
   proof and halts minting; an overdue plan halts minting and lifts when
   paid; the haircut; a Zcash reorg across a claim; a branch-ID re-sign.
1. **Public testnet, TAZ only.** Gated on SIP-4 v1.1, SIP-6, this SIP
   being published, and a working channel on NEAR testnet. Caps from
   §4.7. Every surface carries the custody disclosure. A drill: a
   deliberately unplanned spend from a **separate drill deployment**
   whose test key is disclosed must produce a breach proof and a halt.
2. **Audit gate.** SIP-5 Accepted, and an external audit of:
   - the SIP-4 precompile and index, `spentBy` included;
   - the bridge and token;
   - the NEAR vault contract.

   Plus a clean testnet record: at least N deposits and withdrawals with
   zero ledger discrepancies, and one payout signed across a Zcash
   branch-ID change, or a regtest equivalent.
3. **Mainnet, capped.** Caps, `DELAY` and `DEADLINE` are set by their own
   SIP, start small, and rise only by SIP. The roadmap's "no peg at
   launch" stands. wZEC comes after mainnet launch, not with it.

*Effort (estimates):*

| Part | Estimate |
|---|---|
| SIP-4 v1.1 `spentBy` (follower `vin` index, status, tests) | ~1 week |
| Bridge + token + regtest scenarios | 2–3 weeks |
| NEAR vault contract (Omni's Zcash code as reference) | 3–4 weeks |
| NEAR testnet integration and testnet run | 1–2 weeks, plus the record period |
| NEAR's side of channel (a) | NEAR's timeline |

## 9. Alternatives considered

- **Per-account NEAR-derived deposit keys.** The EVM can't verify the
  address, so minting would trust whoever supplies it (§3.1). Rejected.
- **One vault address plus an OP_RETURN tag or a reservation.** It works,
  but many wallets can't add OP_RETURN, and reservations add a step.
  Rejected in favour of the tagged P2SH.
- **The NEAR contract chooses inputs.** A second ledger on NEAR, fed
  through the channel. Rejected (§4.1).
- **A precompile that reads NEAR.** It breaks the own-node rule and
  couples Sova's liveness to NEAR's. Rejected by Rob.
- **A discretionary pause key.** An admin key under another name.
  Rejected; the halts here are automatic and proof-triggered only.
- **Omni Bridge listing Sova.** It needs no vault of our own, but Omni's
  contracts have DAO upgrade, pause and relayer roles, so the custody
  would include admin keys. It stays a possible later conversation with
  NEAR, not this SIP.
- **Self-custody vaults (2-of-2 + CLTV).** They can't back fungible
  wZEC (`zec-peg-v2.md` §3). They are not wZEC. The custodian-free
  time-locked ZEC lock ships separately as a SIP-4 app.

## 10. Decisions for Rob

1. **Deposit addresses.** A per-account tagged P2SH over one MPC key
   (`t3…`), computed and checked entirely on Sova, or per-account
   NEAR-derived `t1…` keys, which Sova can't verify. *Recommend: tagged
   P2SH*, with a wallet-by-wallet check that `t3` sends work before the
   testnet.
2. **Who plans the payout.** The Sova bridge writes the exact Zcash
   transaction (inputs, outputs, fee), and the NEAR vault checks it
   against its own fixed rules and signs. *Recommend: yes.* It gives one
   ledger, public plans before signing, and an exact theft check.
3. **Channel for the testnet.** Build the vault for NEAR foreign-chain
   verification with operator-run Sova nodes, and ask NEAR only after the
   SIP-4 demo runs on the public testnet. If NEAR testnet can't do it in
   time, run a separate testnet vault on 3-of-5 disclosed volunteer
   attestors, none of them the project or Rob. *Recommend: yes*, with one
   channel per vault and no automatic fallback between channels. A
   fallback would itself be a new party able to trigger payouts.
4. **After a proven theft.** A pro-rata haircut on every later payout, or
   first come, first served. *Recommend: pro-rata* (§5.4).
5. **Immutable vault versus Zcash upgrades.** Caller-supplied branch IDs,
   v5 and v6 builders, rate-limited re-signing, no upgrade path, and the
   format-retirement tail risk disclosed. *Recommend: yes.* The
   alternative, an upgradeable vault, puts an admin key back in.
6. **Fees.** Cost only: a deposit pre-pays its own future input (draft
   6,000 zat), a withdrawal pays the outputs (draft 10,000 zat), any
   surplus stays in the vault as backing, and callers pay their own NEAR
   gas. **The project takes nothing.** *Recommend: yes.*
7. **Testnet limits.** 1,000 TAZ total, 100 TAZ per deposit, 250 TAZ a
   day, a 1-hour NEAR delay after Sova's 64-block finality, a 30-hour
   deadline, deposits at 3 confirmations. Mainnet numbers come from a
   later SIP. *Recommend: yes.*
8. **Unpaid withdrawals.** No cancel and no re-mint. An overdue plan
   halts minting automatically until it is paid, then the halt lifts by
   itself. *Recommend: yes.* Re-minting could create unbacked wZEC if
   NEAR pays late.

## Sources

Everything external here was checked on 2026-09-23 for
`docs/design/zec-peg-v2.md`, which lists the primary sources: the live
`v1.signer` views (version 3.15.1, 17 participants, threshold 11, 9 of 17
TDX attestations, foreign chains Bitcoin/Abstract/Starknet/Aptos), near/mpc
at 3.15.1 and `b4e5d19`, Omni Bridge's Zcash connector, ZIP-244, ZIP-229,
ZIP-317, and Zebra 6.3.0. Additionally checked in Zebra 6.3.0
(`research/zebra-upstream`, `f5c5277`):

- `zebra-chain/src/transaction/unmined/zip317.rs`: `MARGINAL_FEE = 5,000`,
  `GRACE_ACTIONS = 2`, a 150-byte input unit and a 34-byte output unit.
- `zebra-chain/src/parameters/constants.rs`: `MAX_BLOCK_REORG_HEIGHT =
  1000`, a local rollback window and not consensus. So Zcash has no hard
  finality short of that, which is why NEAR re-verifies after the delay
  (§4.2).

Sova sources: SIP-4 (`sips/sip-4-draft-zcash-state-precompile.md`), SIP-6
(`sips/sip-6-draft-sealer-signatures.md`), `contracts/src/zcash/`,
`docs/design/gossip-v1.md` (finality 64, safe 32),
`docs/design/zec-on-sova-options.md`, `docs/design/zec-peg-v2.md`.

**Not verified:** NEAR testnet's MPC foreign-chain support; NEAR's
willingness to accept operator-run Sova nodes as providers; the exact
`Log{log_index}` semantics for EVM chains in foreign-chain verification;
gas for `MAX_INPUTS` sign calls in one NEAR transaction; wallet and
exchange support for `t3` sends; the licence of Omni's Zcash code; every
figure marked *(estimate)*.
