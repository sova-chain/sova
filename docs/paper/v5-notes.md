# Paper v5: notes for integration

Companion to `sova-paper-v5.md`. The paper is copy; the orchestrator sets it
in `site/src/components/Paper.astro`. Body text (Sections 1 to 11) is about
3,500 words, the abstract 190; the Bitcoin paper is roughly 3,500. Every
character in the paper is printable ASCII or one of `· × Σ — ’ “ ” … → −`
(checked by script; no en dashes, no `≥ ≤ ≈`, no subscripts). Equations are
written in ASCII (`s_i`, `Σ_j w_j`, `E_N`, `2^k`, `floor(...)`) for the
orchestrator to typeset as (1), (2), (3) are today.

## 1. Old paper (v4) to new paper (v5), section by section

| v4 section | v5 | What changed and why |
| --- | --- | --- |
| Abstract | Abstract | Rewritten in the Bitcoin shape: the problem in one sentence, then the mechanism in order (burn, epoch, reward, sealer, verification, anchor, precompile, privacy). Dropped “the programmable edge of the shielded pool” (a slogan, not a claim). |
| 1 Introduction | 1 Introduction | Rebuilt on Nakamoto’s three moves: the trust-based model and its cost (wrapped ZEC is a claim on a custodian; its contracts cannot see Zcash; the holder gives up possession), “What is needed is…”, “In this paper, we propose…”. The v4 introduction jumped straight to features; it now states the problem first. The wrapped-ZEC contrast is kept only here, where the Bitcoin paper makes its own contrast. |
| 2 Burns | 2 Burns | Same facts, tightened. Added two ideas the v4 text lacked: the rule is total and needs nothing from Sova or Zcash at burn time; linear weight makes splitting pointless (the “Combining and Splitting Value” idea, folded in here rather than given its own section). The regtest transcript figure is cut (an implementation artifact); Figure 1 is now the transaction layout. |
| 3 Epochs and Sealing (intro) | 3 Epochs | Epochs get their own section, mapped to Bitcoin’s “Timestamp Server”: Zcash’s proof-of-work is the clock. The anchor commitment (v4 §6, first sentence) moves here, because it is what chains Sova to Zcash; Verification and Reading Zcash then use it. The simulated block-stream figure is cut; Figure 2 is the two-chain diagram. |
| 3.1 Settlement as Withdrawals | 5 Sealing, paragraph 2 | Merged into Sealing: the withdrawals list is what the sealer’s block carries. One paragraph instead of a subsection; the Ethereum reference stays. |
| 3.2 The Sealing Ladder | 5 Sealing | Kept whole: ranking, the ladder, liveness-only timeouts, preference (rank, hash), late win bounded to the tip, rank recovered from the tip, receiver decides. Added the honest gap and SIP-6’s draft answer (signed seal, one fixed empty block) in one paragraph. |
| 3.3 Finality | 3 Epochs, paragraph 3 | Merged: fork choice follows Zcash, a Zcash reorg unwinds the Sova blocks built on it, finality is Zcash depth. The 32/64 safe/finalized figures are cut (client plumbing, not protocol). |
| 4 Issuance | 4 Issuance | Same equations (1), (2), (3) and constants. Reordered to state the idea before the schedule (fixed reward, pro rata, no carry-over), then the schedule, then fees and the change rule. Added “the genesis allocates nothing”. The two-burner figure moves to Section 10 as a worked example. |
| 5 Verification | 7 Verification | Kept the rule and the every-import-path point. Cut the changelog (“an earlier version ran the check only on the Engine API… design review caught that gap”): a paper describes the system, not its bug history. Cut `sova-miner report --verify-rpc` (tooling). |
| 5.1 Every Import Path | 7, paragraph 1 | Merged: one sentence names the three paths. |
| 5.2 The Tip and Late Joiners | 7, paragraphs 2 and 3 | Merged and updated to SIP-4’s hold semantics (held, not deferred-on-trust; off-fork held; what is permanently invalid). The “150 blocks behind” test anecdote is cut. |
| 6 Contracts That See Zcash | 8 Reading Zcash | Kept every fact; reordered: what can be asked, determinism, hold-not-guess, depth from the anchor, no protocol depth; then the sale pattern; then what cannot be seen. Added the txid-malleability caveat from SIP-4 §3. The status line moves to the Conclusion. |
| 7 Networking | 6 Network | Rebuilt as Bitcoin §5: six numbered steps (what a node does), then the transport facts (sova/1, pull not push, no head-moving message, explicit bootnodes, own fork ID, sync gated on the scan). Protocol names (discv4/discv5, RLPx, ENR) are dropped from the prose. |
| 8 Privacy at the Edges | 9 Privacy | Same mechanism and the same three caveats, restated so the payment case (Section 8) is covered too. Closing sentence names the habits that matter. |
| 9 What Sova Is Not | cut | A list of lacks, which Rob’s language rule forbids. Each fact it carried is stated positively where it belongs: the EVM is public (§9), Sova changes nothing in Zcash and posts nothing to it (§1), SOVA is gas (Abstract, §4), wrapped ZEC is custody and not part of the design (§11). |
| 10 Running a Node | cut; one sentence in §11 | Operations belong on `/node`. The paper keeps “a local network runs from one command; source and specs are public”. |
| 11 Lineage | cut | The predecessor chain’s history is not part of the design; `/story` carries it. |
| Ashwings (Figure 5, §10) | cut entirely | Per Rob. |
| (none) | 10 Calculations | New, mapped to Bitcoin §11. Only what the specs support: rewriting a mint costs a Zcash reorg; censoring costs out-burning every epoch (given SIP-6); the price of a share; emission totals; the worked two-burner epoch that was v4’s Figure 4. |
| References | References | Nakamoto and Buterin added; the Ethereum yellow paper replaced by the white paper (the task’s reference) plus the two EIPs the text names; SIP-6 added because §5 and §10 cite it. |

## 2. Mapping to the Bitcoin paper

| Nakamoto 2008 | Sova v5 | The shared idea |
| --- | --- | --- |
| Abstract | Abstract | Problem, then mechanism in order, then the security condition. |
| 1 Introduction | 1 Introduction | The trust-based model and its cost; “What is needed is…”; “In this paper, we propose…”. |
| 2 Transactions | 2 Burns | The atomic act, defined precisely, recognizable by anyone. |
| 3 Timestamp Server | 3 Epochs | The ordering primitive: Bitcoin hashes items into a chain; Sova borrows Zcash’s chain and anchors to it. |
| 4 Proof-of-Work | 4 Issuance | What it costs to make a block count; here, destroyed ZEC. |
| 6 Incentive | 4 Issuance (reward, tip, fees) | The reward and the fee transition. Folded into one section because in Sova the cost and the reward are one act. |
| 5 Network | 6 Network | Numbered steps of what a node does. |
| 8 Simplified Payment Verification | 7 Verification | What a node checks and what it must hold to check it (Sova has no light client yet; the section says every node verifies fully, and why). |
| 9 Combining and Splitting Value | 2 Burns, last paragraph | Linear weight; splitting changes nothing. |
| 10 Privacy | 9 Privacy | What is public, where the unlinkability is, and what habits it depends on. |
| 11 Calculations | 10 Calculations | The attacker’s cost and the participant’s cost, in figures. |
| 12 Conclusion | 11 Conclusion | Recap in the order built, then status and future work in two sentences. |
| (none) | 5 Sealing | New in Sova: who assembles the block. No Bitcoin analogue because Bitcoin’s block producer is chosen by the proof-of-work itself. Placed after Issuance because the tip in (2) needs the sealer defined. |
| (none) | 8 Reading Zcash | New in Sova and the headline: what a programmable chain that reads another chain offers. Follows the Ethereum paper’s habit of explaining what a contract can do with what the protocol gives it. |
| 7 Reclaiming Disk Space | none | Sova prunes nothing from the mint check (§7 says so); a section would have no content. |

## 3. Claims and sources

Each claim in the paper, by section, with its source. “Derived” means arithmetic on sourced numbers, shown here.

**Abstract, §1**
- Shielded ZEC moves without a ledger trail or permission: `docs/marketing/positioning.md` (frame), Zcash spec [1].
- Wrapped ZEC is a claim on a custodian; its contracts cannot see Zcash: `positioning.md` (Act II), `site/README.md` claim table (`/why` wrapped-ZEC row, owner direction 2026-09-23).
- Every Sova node runs its own Zcash node: `sips/sip-4-draft-zcash-state-precompile.md` §12, `sips/sip-2.md` (Validation).
- SOVA issued only by burns; burn names an EVM address: `sips/sip-1.md`, `sips/sip-3.md` (Summary).
- One Sova block per Zcash block; Zcash PoW orders Sova: `sips/sip-2.md` (Epochs), `positioning.md` (Terminology: PoW-anchored).
- Sova changes nothing in Zcash and posts nothing to it: `positioning.md` (“never Zcash L2”), `docs/ROADMAP.md` (“posts nothing to Zcash”).

**§2 Burns**
- 1,000-zat floor; eater script `76a914 00…00 88ac`; twenty zero bytes; no known preimage; exactly one OP_RETURN of 29 bytes; “SV”, version byte, 20-byte address, 32 signal bits; weight = summed eater value; fee never weight; malformed is not a burn, never an error; rule is total: `sips/sip-1.md` (all).
- Linear weight makes splitting pointless: `sips/sip-1.md` (Rationale, “Sybil-proof by arithmetic”).
- A burn needs nothing from Sova at burn time; recognized after the fact: `sips/sip-1.md` (the rule is applied to the Zcash chain; the SIP-1 freeze burn was mined by an unrelated miner).

**§3 Epochs**
- One Zcash block = one epoch; height E − B + 1: `sips/sip-2.md` (Epochs).
- About 75 seconds per block: `sips/sip-3.md` (Constants: “at 75 s”).
- Anchor in the beacon-root header field; “Zcash is its beacon”; hash chain pins ancestors: `sips/sip-4-draft-zcash-state-precompile.md` §1.
- Fork choice follows Zcash; a Zcash reorg unwinds Sova; finality = Zcash depth: `sips/sip-2.md` (Epochs), SIP-4 §7.
- Burn-less epoch still has a block, mints nothing: `sips/sip-2.md`, `sips/sip-3.md` (No mint without burns).

**§4 Issuance**
- Genesis allocates nothing: `docs/ROADMAP.md` (M1 testnet chainspec; “Premine” under What we won’t do).
- 9/10 pro rata by weight, floor division; 1/10 plus dust to the sealer; gwei arithmetic; exact conservation; equations (1), (2): `sips/sip-2.md` (Rewards), `sips/sip-3.md` (Constants); the formula reproduces the logged C3 amounts (`site/README.md`, Figure 4 row).
- Fixed reward, more burners divide it thinner, price not supply: `sips/sip-3.md` (Why this shape).
- No carry-over, no jackpot epoch, emission tracks demand: `sips/sip-3.md` (No mint without burns).
- Equation (3), 0.3125 step, 20,000-epoch slow start, 1,680,000-epoch eras, gwei floor, era 42 = 1 gwei, era 43 = 0, asymptote 20,937,503,124.97144: `sips/sip-3.md` (Constants, The schedule).
- Slow start mirrors Zcash’s launch and closes the worthless-token window: `sips/sip-3.md` (rationale).
- Halving interval inherited from Zcash: `sips/sip-3.md` (rationale).
- EIP-1559: base fee burned; priority fees and tip to sealers: `sips/sip-3.md` (Summary, “The far future is fees”).
- Schedule changes only by burn-weight signaling; tally not built: `sips/sip-3.md` (Changing this schedule), `docs/ROADMAP.md` (Later: burn-weight signaling needs its own SIP).

**§5 Sealing**
- Rank by weight desc, then byte-lexicographic min txid; rank 0 seals; rank r after r × step; draft step 15 s; non-burners never seal; timeouts liveness-only; preference (rank asc, hash asc); late rank-0 displaces rank-1 by micro-reorg bounded to the epoch: `sips/sip-2.md` (Ranking and sealing, Validation roadmap), `docs/design/gossip-v1.md` (v2 as built).
- Withdrawals: index, validator_index 0, address, gwei, rank order; must match some rank’s derivation; tip makes each rank distinct; rank recovered from withdrawals: `sips/sip-2.md` (Settlement, Validation roadmap).
- Ethereum Shanghai withdrawals (EIP-4895): `sips/sip-2.md` (“inherited unchanged from the execution layer”), `site/README.md` §3.1 row.
- Receiver decides, never sender or arrival time: `docs/design/gossip-v1.md` (“relay delivers, arbiter decides”), `sips/sip-6-draft-sealer-signatures.md` §2.7 (first-seen rejected).
- Block does not name its sealer; author not checkable from the header; SIP-6 draft: signed seal, null block, one-burn halt: `sips/sip-6-draft-sealer-signatures.md` (Summary, §1.1, §1.3, §2.4).

**§6 Network**
- Node follows its Zcash node, extracts burns, computes settlement per rank: `sips/sip-2.md`, `crates/engine` expectations per `site/README.md` §5 row.
- A burn needs no Sova node: `sips/sip-1.md` (a Zcash transaction).
- Sealer builds with settlement, anchor, transactions; announces by height and hash; blocks pulled not pushed; validated then observed as candidate; head moved by local arbiter only; re-announce only accepted blocks: `docs/design/p2p-m1.md` (Decision 3), `docs/design/gossip-v1.md`.
- sova/1 with Announce, GetBlock, Block; no head-moving message: `docs/design/p2p-m1.md`.
- Discovery from explicit bootnodes; own genesis and fork ID; peers refused at handshake: `docs/design/p2p-m1.md` (Discovery and isolation, As built).
- Catch-up by ordinary Ethereum sync, gated on the scan: `docs/design/p2p-m1.md` (Decision 2, Catch-up).

**§7 Verification**
- E_N = N + B − 1: SIP-4 §1.
- Anchor must equal own Zcash node’s hash; withdrawals must match some rank: SIP-4 §1, `sips/sip-2.md` (Validation roadmap).
- Withdrawals in the body, no execution needed; enforced on tip, short-gap and history paths: `docs/design/p2p-m1.md` (Decision 1), `site/README.md` §5.1 row.
- Tampered history rejected by a syncing node: `docs/design/p2p-m1.md` (build order, AC), `docs/ROADMAP.md` (M1 built: late-join catch-up).
- Held when the scan has not reached E_N; held when off-fork; permanently invalid only on a matching anchor with wrong withdrawals or state root: SIP-4 §1, §6.
- Joining node scans Zcash first, syncs Sova as far as the scan; nothing pruned: `docs/design/p2p-m1.md` (Decision 2), `site/README.md` §5.2 row.
- Every Sova node runs a Zcash node (trust-mode following ends): SIP-4 §1, §12.

**§8 Reading Zcash**
- Fixed precompile address; the five v1 methods; answers from B through E_N; pure function of the committed chain; same state root; hold, never guess; “not found” is a result; depth E_N − h + 1; no protocol depth; library requires minConf: SIP-4 (Summary, §2, §3, §5), `contracts/src/zcash/ZcashLib.sol` per `site/README.md`.
- Sale pattern (reserve, pay to a fresh seller address from any wallet incl. shielded, deliver at depth): SIP-4 §9 (use cases 1, 2), `docs/design/ashwing-zec-checkout.md` per the claim table (the pattern only; Ashwings is not named).
- Reorg unwinds Sova on every node; minConf protects what happened outside Sova: SIP-4 §7.
- Shielded amount, sender, recipient, memo, balance invisible; txid, height, position public for shielded txs; z→t output public: SIP-4 §8, §3 (`txInfo` note).
- Pre-v5 txids malleable; key on what an output pays: SIP-4 §3.

**§9 Privacy**
- EVM public; privacy at the funding edge; z→t has no visible input; the three caveats (address linkability, deshield visible, payouts public); habits: `crates/burn-wallet/miner/README.md` (Anonymous funding), `positioning.md` (What we never claim).

**§10 Calculations**
- Rewriting a mint requires a Zcash reorg from E_N; every node follows identically: SIP-4 §1, §7; `sips/sip-2.md` (Epochs).
- Sova adds no proof-of-work: `positioning.md` (Terminology).
- Overtaking probability: Nakamoto [6] §11 (applied to Zcash’s chain).
- Tip reorg only to a better rank for the same epoch: `sips/sip-2.md`.
- Censorship costs out-burning every epoch, given SIP-6: `sips/sip-6-draft-sealer-signatures.md` §2.8.
- Price of a share W / ((9/10) R): derived from (1).
- Floor cost: 1,000 zat burn, about 20,000 zat fee: `sips/sip-3.md` (Cost floor note: 20,000 to 25,000 zat), `sips/sip-1.md` (ZIP-317).
- About a quarter of a ZEC a day: derived. 86,400 / 75 = 1,152 epochs a day × (1,000 + 20,000) zat = 24,192,000 zat = 0.24 ZEC.
- Slow-start total 62,503,125: derived. 0.3125 × 20,000 × 20,001 / 2 = 62,503,125. Shortfall 62,496,875: `sips/sip-3.md`, and 125,000,000 − 62,503,125.
- Era about four years: `sips/sip-3.md` (Constants). Emission ends after 43 eras: `sips/sip-3.md` (era 43 pays 0).
- “A little over 170 years”: derived. 43 × 1,680,000 × 75 s = 5.418 × 10^9 s = 171.7 years. See the discrepancy note below.
- Worked epoch 5 : 2: `docs/WORKPLAN.md` C3 row (logged 4,642.857142858 + 1,607.142857142 = 6,250); the shares and the tip recomputed from (1) and (2): 5,625 × 5/7 = 4,017.857142857 (floor in gwei), 5,625 × 2/7 = 1,607.142857142, tip = 6,250 − sum = 625.000000001, rank 0 total 4,642.857142858.

**§11 Conclusion**
- Local network from one command; source and specs public: `docs/ROADMAP.md` (M0), `box/README.md` per the claim table.
- Anchor and precompile built, ship with the public testnet: `docs/WORKPLAN.md` z-1 row (step 1 landed, v1 in review) per `site/README.md`; SIP-4 status line.
- SIP-6 is a draft: `sips/sip-6-draft-sealer-signatures.md` (status).
- Wrapped ZEC is custody, disclosed as custody, not at launch: `docs/ROADMAP.md` (Later), `positioning.md` (Act III). The paper does not name NEAR; the site does.

## 4. Claims I believe true but could not source, or where sources disagree

1. **“A little over 170 years” to the end of emission.** SIP-3 says “~176 years”. 43 eras × 1,680,000 epochs × 75 s is 171.7 years (44 eras would be 175.7, but era 43 already pays zero, so emission ends when era 43 begins). The paper uses the arithmetic; SIP-3’s figure looks like 44 × 4. Recommend correcting SIP-3 or the paper, whichever Rob prefers.
2. **“Zcash itself used a 20,000-block slow start.”** Stated in SIP-3’s rationale; not independently checked against the Zcash protocol spec in this pass. (Zcash’s slow-start interval is indeed 20,000 blocks in its consensus parameters, but I did not open the spec to cite a section.)
3. **“The transaction is included by the first sealer who is not the attacker.”** From SIP-6 §2.8, which is a draft and conditional on sealer signatures. The paper states the condition (“with the sealer’s signature in the header [9]”). Before SIP-6, tip grinding (SIP-6 §1.1) lets a non-burner replace the tip block, so the censorship cost claim does not hold today. Section 5 says so in one paragraph.
4. **“A Zcash reorganization unwinds the Sova blocks built on it.”** This is the specified rule (SIP-2, SIP-4 §7). SIP-4 §7 notes the automatic rollback is “required work” (the sealer today logs a follower rollback and continues). The paper describes the rule; the Conclusion’s status sentence covers “specified and built, ships with the testnet” for the anchor, and the same applies here.
5. **“About 75 seconds”** for Zcash block time is stated as a constant in SIP-3, not cited to the Zcash spec.
6. **“Only up to the Zcash height its own Zcash node has reached”** (sync gating) is `docs/design/p2p-m1.md` Decision 2 and the WORKPLAN log (“scan-gated catch-up landed”); it is a design note, not a SIP.
7. **Reference [7] and [8] details** (EIP author lists and the ethereum.org whitepaper URL) are from memory and should be checked before publication.

## 5. Figures (three, for the orchestrator to draw)

**Figure 1 (Section 2): the burn transaction.** One Zcash transaction; inputs on the left, outputs on the right. Two outputs are labelled: the eater output with its script and its value (“weight”), and the OP_RETURN output with its 27-byte payload split into fields. Optional third output: change. Caption: “A burn: one transparent Zcash transaction. The eater output is the weight; the payload names the address to credit.”

```
  inputs (any)                    outputs
  ----------------                -------------------------------------------
  t-addr  0.0210 ZEC   ---->      eater   76a914 0000…0000 88ac   0.0200 ZEC  = weight
                                  payload OP_RETURN  "SV" 01 <20-byte address> <4-byte signal>
                                  change  t-addr                   0.0008 ZEC
                                  (fee 0.0002 ZEC to the Zcash miner, not weight)
```

**Figure 2 (Section 3): epochs and the anchor.** Two horizontal chains. Top: Zcash blocks E−1, E, E+1 linked by parent hashes, each carrying its burns (some empty). Bottom: Sova blocks at heights E−B, E−B+1, E−B+2, linked by parent hashes. A vertical arrow from each Sova block up to the Zcash block it settles, labelled “anchor = hash(Zcash E)”. Caption: “One Sova block per Zcash block. The parent hash chains Sova blocks to each other; the anchor chains each to the Zcash block it settles.”

```
  Zcash   [E-1: 2 burns] <-- [E: 0 burns] <-- [E+1: 1 burn] <-- ...
               ^                  ^                 ^
               | anchor           | anchor          | anchor
  Sova    [h-1: mints 2] <-- [h: mints 0]  <-- [h+1: mints 1] <-- ...        h = E - B + 1
```

**Figure 3 (Section 5): the ladder and the late win.** A timeline for one epoch with two ranked burners, rank 0 (weight 5) and rank 1 (weight 2). t = 0: rank 0 may seal, but is silent. t = step: rank 1 seals B1; every node’s head moves to B1. Later: rank 0 publishes B0 on the same parent. Every node prefers (rank, hash), so the head moves to B0: a one-block reorg at the tip. Next epoch builds on B0. Caption: “The ladder keeps the chain live when rank 0 is late; preference by rank keeps every node on the same block once rank 0 arrives. Weights as in the worked epoch of Section 10.”

```
  t=0        rank 0 may seal ........ (silent)
  t=step     rank 1 seals B1  ------> head = B1
  later      rank 0 seals B0 on B1's parent
  arbiter    prefer (rank, hash) ---> head = B0     one-block reorg, tip only
  next epoch builds on B0
```

## 6. Style rules applied

- Plain declarative sentences; the problem before the mechanism in every section; each section one idea, building on the last.
- Sova described positively (verifies, derives, holds nothing, self-custody). The only contrasts are in the Introduction, where the Bitcoin paper makes its own.
- Limits stated as part of the design: what is held vs. invalid (§7), what the precompile cannot see (§8), the three visible things (§9), the unsigned-seal gap (§5), the reorg-depth risk (§8), and the status paragraph (§11).
- Nothing sells: no “fast”, “secure”, “trustless”, “revolutionary”. No price talk. No Ashwings. No NEAR by name. No lineage.
- Status honesty: “ship with the public testnet, which will be the first public network”; SIP-6 “is a draft”; wrapped ZEC is one sentence of future work.
