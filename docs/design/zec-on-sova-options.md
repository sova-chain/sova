# ZEC on Sova: options paper

Status: **for Rob's decision** (board z-2). Written 2026-09-23. Design
only: nothing here is built, and nothing here changes the roadmap's
"no peg at launch" until Rob decides. External facts were checked on
2026-09-23; sources are at the end.

**Follow-up:** [`zec-peg-v2.md`](zec-peg-v2.md) takes option B further.
It covers NEAR MPC as the custodian with no project key, how NEAR can
learn about Sova burns, monitoring the custodian (and why a precompile
that reads NEAR is the wrong tool), and self-custody vaults. It also
corrects two facts below: the MPC network is now **17 nodes with a
threshold of 11**, and per-user deposit addresses under B **can** be
computed in the EVM by using a tagged P2SH around one MPC key.

## The one fact that frames every option

Moving ZEC onto Sova has two halves, and they are not equally hard.

- **Deposits (ZEC → Sova) can be trustless.** Sova already verifies
  Zcash: every node re-derives mints from its own zebrad. With the
  SIP-4 precompile (`sips/sip-4-draft-zcash-state-precompile.md`), a
  contract can check "this transparent output paid ≥ X zat to the vault
  script, ≥ `minConf` deep" and mint on that proof alone. No signer's
  word is needed.
- **Withdrawals (Sova → ZEC) cannot be trustless.** Zcash has no smart
  contracts and no covenants. Nothing on Zcash can check a Sova burn,
  so releasing vault ZEC always needs **someone holding a key to sign a
  Zcash transaction**. No design removes that someone. Designs differ
  only in who holds the key, how many of them must collude, and how
  quickly Sova can notice if they misbehave.

The one exception is a one-way peg (C2): ZEC goes in and never comes
out.

A second framing point: much of what Rob wants ("contracts that can use
ZEC") does **not** need ZEC on Sova. With SIP-4, a contract can settle
an outcome on Sova when ZEC moves on Zcash. The ZEC never leaves Zcash
and nobody holds it (option D1).

## Ground truth (verified, not remembered)

- **FROST covers shielded spends only.** ZIP-312 (Status: Draft) defines
  FROST(Pallas) and FROST(Jubjub) threshold signing for Orchard and
  Sapling spend authorization. It needs no consensus change. It trusts
  the coordinator with unlinkability, and key generation is out of
  scope. The Zcash Foundation's `frost-core`/`reddsa` crates were
  audited by NCC and its `frostd`/`frost-client` tools by Least
  Authority. As of the ZF's May 2025 status post, wallet integration is
  "the missing piece." **FROST cannot control transparent funds.**
  Transparent Zcash uses ECDSA script with no Schnorr, so a transparent
  threshold vault is either a **P2SH m-of-n script** or **threshold
  ECDSA** (MPC, e.g. NEAR).
- **P2SH multisig limits.** Zebra enforces zcashd-parity standardness:
  at most 15 sigops in a P2SH redeem script, and a 520-byte push limit.
  In practice that means **at most 15 keys** (15 × 34 + 3 = 513 bytes),
  or **at most 14** with A1's 21-byte address tag (14 × 34 + 25 = 501).
  (Zebra 6.3.0: `mempool/storage/policy.rs`, `MAX_P2SH_SIGOPS`.)
- **NU6.3 "Ironwood"** is active on testnet since height 4,134,000 and
  scheduled on mainnet at 3,428,143. It adds v6 transactions and a new
  Ironwood pool. **No net new value may enter Orchard** (Zebra
  `check.rs`: `valueBalanceOrchard` MUST be ≥ 0). A new shielded vault
  would therefore live in Sapling or Ironwood. FROST over Ironwood is
  plausible, since Ironwood reuses the Orchard action/RedPallas
  machinery per Zebra's source comments, but **ZF should confirm it**.
  Community multisig work has open Ironwood PCZT issues.
- **External-signer tooling for v5/v6 is immature.** We hit it
  ourselves on 2026-09-23: zcash-devtool's external-signer path panics,
  and it signs with the v5 sighash (`WORKPLAN.md`). Any signer design
  builds its own tooling or waits for upstream.
- **NEAR Chain Signatures** signs secp256k1 (Bitcoin/EVM) and ed25519.
  NEAR's docs describe **8 independent nodes**, none of which can sign
  alone, and a participant set that can be changed by node vote. Nodes
  can run in Intel TDX TEEs. The MPC network can **verify foreign-chain
  transactions** before signing by having nodes query configured RPC
  providers. **Omni Bridge lists Zcash**: inbound is verified by light
  client, and outbound transfers are signed by Chain Signatures.
  **NEAR Intents** supports native ZEC swaps (Zashi since October 2025;
  a NEAR–Zodl swap partnership was announced 2026-08-20).

## The options

### A. Federated threshold vault → wZEC

**A1, the recommended variant: a transparent tagged P2SH vault.**

- **Deposit.** Each Sova address gets its own deposit address:
  `P2SH(<evm20> OP_DROP m <K1..Kn> n OP_CHECKMULTISIG)`. A contract can
  recompute this with the EVM's own SHA-256 and RIPEMD-160 precompiles.
  The user sends ZEC to that `t3…` address from any wallet, including
  from a shielded balance. After `minConf`, **anyone** submits the
  txid:vout. The contract checks it through SIP-4 and mints wZEC once
  per outpoint. Signers take no part in minting.
- **Withdrawal.** The user burns wZEC on Sova, naming a transparent
  Zcash destination and an amount. Signers watch Sova, then build,
  co-sign and broadcast the payout. Anyone submits the payout txid, and
  the contract checks it through SIP-4 and closes the request.
- **Watching the custodian.** With SIP-4 v1.1's `spentBy`, the contract
  knows every vault outpoint, because it minted against each one. If a
  vault outpoint is spent by a transaction that does not match
  registered payouts, anyone can prove it on Sova. The contract then
  **halts minting automatically** and emits an alarm. Proof of reserves
  is the contract's own live accounting. This is detection, not
  prevention: stolen ZEC is gone. Optional signer bonds in SOVA (A1+)
  can add partial compensation.
- **Privacy.** In: a z→t deposit, with the same honest caveats as a
  burn. Out: a transparent payout that the user then shields. Payouts
  go only to transparent addresses in v1, because only those can be
  verified on Sova.

**A2: a FROST shielded vault (Sapling/Ironwood).** The vault is private,
but Sova **cannot verify deposits**, because amounts and recipients are
shielded. So signers must attest to deposits, which means **signers can
mint unbacked wZEC**. Theft is detectable only if the vault publishes a
viewing key. Not recommended as the first peg.

### B. NEAR Chain Signatures as the key-holder

A NEAR contract controls a derived secp256k1 key whose transparent
P2PKH address is the vault. The Sova side works the same as A1:
deposits are verified by SIP-4. There is one difference: per-user
deposit addresses can't be derived in-EVM, because that needs secp256k1
point operations the EVM doesn't offer cheaply. So deposits use one
vault address with an OP_RETURN tag, or a reservation.

The hard part is on NEAR's side. The NEAR contract must learn "N wZEC
burned on Sova, pay address Y" before it asks the MPC to sign. There are
three ways it could:

1. **MPC foreign-chain verification**, with Sova added as a configured
   chain. That is NEAR governance's decision. If the RPC the nodes
   query is ours, **we become the oracle**.
2. **A Sova light client on NEAR.** Sova has no validator signatures;
   its validity is Zcash burns plus execution. That would need Zcash
   PoW header verification plus a zk or optimistic proof of Sova
   execution. It doesn't exist.
3. **A relayer with a challenge window.** An honest watcher must exist
   and be paid.

(Update: `zec-peg-v2.md` §1 checks each of these three against primary
sources. Foreign-chain verification is live on mainnet only for
Bitcoin, Abstract, Starknet and Aptos. A Sova light client also needs
Sova blocks that are light-client-checkable, which they are not today.
The per-user deposit address problem is solved by a tagged P2SH:
`<evm20> OP_DROP <K_vault> OP_CHECKSIG`.)

**The weak link is the Sova→NEAR message, not the MPC.** In B's favor:
no federation to recruit, an operating MPC network, and existing Zcash
outbound signing in Omni Bridge. Against it: Sova's vault sits inside
a large shared honeypot, NEAR governance can change the signer set, and
the NEAR vault contract has an upgrade authority that must be locked.

### C. Proof-based (light-client) verification

To say it plainly: **the deposit half is trustless** (SIP-4, for
transparent deposits, as in A1 and B). **The withdrawal half cannot
be.** Zcash script can't read Sova. There are two proof-flavored ways
to bound the key-holder:

- **C1: collateralized operators** (XCLAIM/Interlay style). Each
  operator holds its own ZEC vault and posts SOVA collateral of, say,
  150% or more. A redeem request must be answered with a SIP-4-proven
  payout before a deadline, or the collateral is slashed to the user.
  Theft becomes a loss for the operator rather than a trust assumption.
  It needs a **SOVA/ZEC price oracle**, which is new trust, and it
  needs deep SOVA liquidity. Young, volatile collateral is weak
  collateral. This is research.
- **C2: a one-way peg.** Burn ZEC to the eater with a separate payload
  (not SIP-1's `"SV"`) and mint a non-redeemable "bZEC." It is
  trustless and irreversible. It is **a receipt for destroyed ZEC, not
  ZEC**, worth only what the market decides, and it muddies SIP-1's
  story. Not recommended.

### D. No custody at all

- **D1: a SIP-4 escrow market (custody-free).** The SOVA side locks
  SOVA in a Sova contract. The ZEC side reserves the order, then pays
  ZEC on Zcash to a per-order address. The contract verifies the payment
  and releases. It is the same pattern for "buy an Ashwing with ZEC."
  **What it is:** SOVA and any Sova asset, priced and settled in real
  ZEC, with zero custody, from ordinary wallets. **What it isn't:** ZEC
  inside contracts. There is no ZEC collateral, no ZEC in AMMs, no
  ZEC-denominated lending. The payer holds a free option during the
  reservation window.
- **D2: NEAR Intents.** Solvers quote SOVA↔ZEC. During the swap, custody
  is NEAR Intents' (minutes), not ours. It requires NEAR to support Sova.
  Chain Signatures can already sign Sova transactions, since Sova is
  EVM, so this is their integration decision. It is a market, not a
  peg. Burn purity holds: solvers source SOVA from the market or burn
  on users' behalf, and nobody receives burn proceeds.

## Trust: who can do what

| Option | Who can steal ZEC | Who can freeze it | Who can mint unbacked | What failure looks like |
|---|---|---|---|---|
| **A1** tagged P2SH + SIP-4 | `m` of `n` signers colluding or compromised | `n−m+1` signers refusing or offline | **Nobody** (a contract-verified mint; barring a SIP-4 bug) | Vault drained; Sova proves it on-chain, minting halts, wZEC depegs |
| **A2** FROST shielded | `m` of `n` | `n−m+1` | **Signers** | Silent unbacked mint or drain; seen only via a published viewing key |
| **B** NEAR MPC | MPC threshold (+ TEE break); NEAR vault-contract upgrade authority; **whoever feeds Sova events to NEAR** | MPC/NEAR outage; NEAR governance | Nobody (Sova side as A1) | Wrong release through a bad Sova→NEAR message; shared-honeypot event |
| **C1** collateralized ops | An operator can take its own vault, but is slashed | An operator stalls, and the user is paid in SOVA, not ZEC | Nobody | Collateral shortfall if SOVA falls faster than the oracle |
| **C2** one-way | Nobody | Nobody | Nobody | bZEC trades far below ZEC |
| **D1** escrow | Nobody holds ZEC | — | — (nothing minted) | A failed trade refunds; the counterparty eats the option risk |
| **D2** NEAR Intents | NEAR Intents' custody, briefly | NEAR | — | A NEAR-side incident, not Sova's |

## Delivery: UX, exposure, effort, fit

| Option | UX | Custody/regulatory exposure (flag, not advice) | Effort (est.) | Positioning fit | Testnet / mainnet |
|---|---|---|---|---|---|
| **A1** | Deposit from any wallet; transparent withdrawals | **High.** Signers custody user funds (money-transmission or custodial-service questions; EU AMLR bars CASPs from handling anonymity-enhancing coins from July 2027) | Contracts ~2 wks; signer tooling 3–4 wks; SIP-5 ~1 wk | Fits **after** SIP-5 plus review plus audit; mint is not signer-controlled | Testnet: TAZ, capped, after the SIP-5 draft. Mainnet: post-audit, capped |
| **A2** | Private vault | High, plus unverifiable reserves | 4–6 wks | Weak: signers mint | Testnet only |
| **B** | Like A1; one address plus a tag | Custody is NEAR MPC's; we run the release logic ("control" questions) | Sova side ~2 wks; NEAR side unknown, their roadmap | Fits (NEAR in the architecture, never in consensus) | Gated on NEAR |
| **C1** | Redeem through operators | Operators are custodians | 3+ months plus an oracle | Fits; the oracle is new trust | Research |
| **C2** | Trivial | Low | ~1 wk | Confusing next to SIP-1 | Not recommended |
| **D1** | Any wallet pays; a Sova dapp settles | **Lowest.** No custody; the escrow is code | ~1 wk after SIP-4 | **Best.** No custody, pure burn untouched | Testnet now; mainnet at launch (no peg gate, it isn't a peg) |
| **D2** | Swap UI | NEAR's, not ours | Solver 2–3 wks (reuses pivot-era solver code) | Fits | When NEAR lists Sova |

## Against the rules and the old chain

- **Burn stays 100% pure.** No option routes burn value anywhere. Peg
  fees, if any, come from peg users, never from SIP-1 burns. C2 would
  need a distinct payload magic.
- **No admin keys in consensus.** Every option lives in contracts, and
  signers are contract-level. But a **discretionary pause key is an
  admin key at the app level**, and the brand will be judged on it
  anyway. Hence the recommendation below: an automatic, proof-triggered
  halt only.
- **No custody before a specified, reviewed peg.** D1 holds no custody
  and can ship now. A1 and B hold custody, so: SIP-5 is written first,
  a TAZ-only testnet run follows, and mainnet waits for review plus
  audit. The roadmap's "no peg at launch" stands.
- **The Bitcoin-era failure** was one hot BIP32 seed with no real
  enclave, an operator able to mint and burn anyone's balance, a
  double-spend guard that failed open, and consensus depending on live
  external services (audit, 2026-09-21). A1 differs on each point:
  there are `m` of `n` independent keys, **minting is proof-verified
  rather than signer-controlled**, theft is **provable on Sova by
  anyone**, and SIP-4 reads a local index, never a service. It still
  has custody. The difference is that the custody is bounded and
  watched, not trusted.

## Recommended path

1. **Now: SIP-4 v1 plus the D1 escrow on testnet.** This addresses both
   halves of the complaint ("can't see Zcash," "can't use ZEC") with
   zero custody. It is also the demo that makes NEAR and Zodl
   conversations possible under the partner-outreach rule. Estimate:
   ~5–7 weeks in total.
2. **Next: write SIP-5 (the wZEC peg) around A1, with a pluggable
   custodian.** The Sova contract should not care whether the vault key
   set is a P2SH federation or a NEAR-derived key. That keeps the choice
   between A and B open until the mainnet gate. Run a TAZ-only
   prototype with disclosed volunteer signers and a hard cap, and
   **only after the SIP-5 draft is public**.
3. **Then: open NEAR with the demo in hand**, carrying one question:
   can MPC foreign-chain verification watch wZEC burns on Sova without
   trusting an RPC we operate?
4. **Mainnet: unchanged.** No peg at launch. wZEC only after SIP-5 is
   Accepted, an external audit covers the SIP-4 precompile, the peg
   contracts and the signer tooling, and a clean testnet record exists
   with signers disclosed and caps set.

Not now: A2 (unverifiable mint), C1 (oracle plus illiquid collateral),
C2 (a receipt, not ZEC).

## Decisions for Rob

1. **Direction.** Approve SIP-4 v1 plus the D1 escrow as the immediate
   Zcash-linkage work? (Recommended: yes.)
2. **Peg model for SIP-5.** A1 federation / B NEAR / **A1 contract with
   a pluggable custodian (recommended)** / defer the peg entirely.
3. **Signer set.** Who, and how many. Does the project or Rob hold a
   key? (Recommended: independent, disclosed operators; the project
   holds at most one key, ideally none.)
4. **Threshold.** Recommended: testnet 3-of-5; mainnet at least 7-of-11
   (steal needs 7, freeze needs 5; a tagged P2SH allows at most 14 keys).
5. **Caps.** Recommended: testnet total ≤ 1,000 TAZ. Mainnet starts with
   a small total cap and a per-deposit cap (numbers set in SIP-5), and
   raises them only by SIP.
6. **Emergency pause.** (a) none, (b) **automatic halt of minting on an
   on-chain theft proof only (recommended)**, or (c) a discretionary
   pause key, which is an admin key.
7. **Audit gate** before mainnet wZEC. Recommended: SIP-5 Accepted plus
   an external audit of the precompile, contracts and signer tooling,
   plus a testnet record of at least N deposits and withdrawals with
   zero discrepancies.
8. **Payout privacy.** Transparent-only payouts first (verifiable;
   recommended), or shielded payouts on the signers' word.
9. **Fees.** Do signers earn a withdrawal fee in ZEC? Recommended: yes,
   disclosed, and **the project takes nothing**.
10. **NEAR timing.** Open the conversation after the D1 demo runs on
    testnet (recommended), or now.

## Sources (accessed 2026-09-23)

- Zebra v6.3.0 source (`research/zebra-upstream`, `f5c5277`):
  `zebra-chain/src/parameters/constants.rs` (NU6.2/NU6.3 heights),
  `zebra-consensus/src/transaction/check.rs` (Orchard inflow freeze),
  `zebrad/src/components/mempool/storage/policy.rs` (P2SH limits),
  and `CHANGELOG.md` (Ironwood, v6).
- ZIP-312, FROST for Spend Authorization Multisignatures (Draft):
  https://zips.z.cash/zip-0312
- ZIP-311, Zcash Payment Disclosures (Draft): https://zips.z.cash/zip-0311
- Zcash Foundation, "The State of FROST for Zcash" (2025-05-20):
  https://zfnd.org/the-state-of-frost-for-zcash/ ; tools:
  https://github.com/ZcashFoundation/frost-tools
- NEAR Chain Signatures docs: https://docs.near.org/chain-abstraction/chain-signatures
- NEAR MPC node (TEE, foreign-chain verification): https://github.com/near/mpc
- Omni Bridge README (Zcash listed; light-client inbound, MPC outbound):
  https://github.com/Near-One/omni-bridge
- NEAR Intents and Zashi (CoinDesk, 2025-10-09):
  https://www.coindesk.com/markets/2025/10/09/near-intents-activity-spikes-as-zcash-s-zashi-wallet-taps-it-for-private-swaps
- NEAR–Zodl swap partnership (2026-08-20):
  https://en.cryptonomist.ch/2026/08/20/near-protocol-zcash-swap/
- EU AMLR anonymity-enhancing coins (flag only):
  https://cointelegraph.com/news/eu-crypto-ban-anonymous-privacy-tokens-2027
