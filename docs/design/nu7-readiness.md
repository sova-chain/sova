# NU7 readiness

Status: **plan, for the orchestrator and Rob.** Written 2026-09-29 on
`nu7-readiness` from `release` at `d9ea8d3`. No code changes here. File
and line references are to that commit. Upstream facts are cited by URL.
Where something upstream is not published yet, this note says so and does
not guess.

## The short version

Zcash NU7 activates on the **Zcash testnet on 2026-10-06** and on
**mainnet on 2026-11-05** ([forum, ebfull, 2026-09-17][fwd-timeline]). It
brings 25-second blocks (ZIP 218), the NSM fee burn and issuance smoothing
(ZIPs 234/235), and it makes v4 transactions invalid. v5 and v6 stay
valid; there is no new format.

Sova runs on the Zcash testnet today. On 2026-10-06:

1. **Every zebrad we run must be on an NU7-aware Zebra**, or it rejects
   the first NU7 block and stops at the fork. Sova then stops too, or
   follows an abandoned minority chain (§4.2). No such Zebra release
   exists yet, and the testnet activation height is only set on **Oct 5**
   (§1). Expect about one day to upgrade.
2. **Every burn and every faucet drip must be signed with the NU7
   consensus branch ID, `0x77190AD9`.** Our transaction builder takes the
   branch ID from the compile-time table in `zcash_protocol` 0.10.6, which
   has no usable NU7 entry (§1). Unchanged, zebrad rejects every burn after
   activation. No burns means no sealed blocks, so no Sova transaction can
   be mined (row B1). Fix before Oct 5: read the branch ID from zebrad and
   build against a librustzcash that knows `0x77190AD9`.
3. **`finalized` = 100 is already too shallow.** The engine assumes
   Zebra rolls back at most 99 blocks. Zebra's limit has been 1,000 since
   5.2.0, and at 25 s, 100 blocks is 42 minutes (row F2). The number is
   Rob's call.

The SIP-7 value-pool check (row F1) could also hold every node at the
first NU7 block if the NU7 Zebra changed its pool reporting. Zebra's NU7
reference branch doesn't, so it is a verification step, not expected work.

Everything else is either a wall-clock change (3× more Zcash blocks per
day, so 3× more Sova epochs per day) or copy. The one big design question
is **Rob's**: whether SIP-3 emission follows ZIP 218 (§5).

## 1. Upstream status: heights, branch ID, releases

Checked 2026-09-29.

| Item | Status | Source |
|---|---|---|
| Deployment ZIP | **ZIP 259, "Deployment of the NU7 Network Upgrade", Status: Draft**, merged 2026-09-22 (zips PR #1363). ZIP 254, the earlier NU7 deployment ZIP, is Withdrawn. | [ZIP 259][zip259] |
| NU7 consensus branch ID | **`0x77190AD9`** ("CONSENSUS_BRANCH_ID: `0x77190AD9`", ZIP 259). The earlier `0x77190ad8` is retired. | [ZIP 259][zip259]; librustzcash `92859218` "expose NU7 with branch ID 0x77190AD9" |
| Testnet activation height | **Not published.** ZIP 259: "Testnet: TBD (To be set on OCT 5)", one day before activation. Open zips PR #1370 would require NU7 heights divisible by 3. The forum's "Testnet activation heights decided" item is unchecked. | [ZIP 259][zip259], [forum][fwd-timeline] |
| Mainnet activation height | **Not published.** ZIP 259: "Mainnet: TBD (To be set on OCT 20)". | [ZIP 259][zip259] |
| `zcash_protocol` (crates.io) | Latest **0.10.6** (2026-09-08): NU7 is behind `cfg(zcash_unstable = "nu7")`, placeholder branch ID `0xffff_ffff`, height `None` on both networks (`consensus.rs:503-504, 538-539, 775-776, 798-799` in the registry copy). This is what we build with. | crates.io; `~/.cargo/registry/src/*/zcash_protocol-0.10.6` |
| `zcash_protocol` (librustzcash `main`) | Commit `92859218` (2026-09-22) removes the cfg gate, `0x7719_0ad9 => BranchId::Nu7`, rejects `0x77190ad8` and `0xffff_ffff`. Heights are still `None` for both networks; changelog: "Mainnet and Testnet have no NU7 activation height." Not released. | [consensus.rs on main][lrz-consensus] |
| `zcash_primitives` | Latest release 0.30.1 (2026-08-19). On `main`: v4 invalid under NU7 (`Nu7 => false // ZIP 2003`), v5 and v6 valid; `suggested_for_branch(Nu7)` is v6. We build v5 explicitly, which stays valid. | librustzcash `main`, `zcash_primitives/src/transaction/mod.rs` |
| Zebra release | Latest **6.4.2** (2026-09-25, a v6 DoS fix); 6.4.0/6.4.1 on 2026-09-23. **No release supports NU7.** Docker Hub `zfnd/zebra` has `6.4.2` and `latest`. On `main`, NU7 exists only under `cfg(any(test, feature = "zebra-test"))` with placeholder `0xfffffffe`. | [Zebra releases][zebra-rel] |
| Zebra NU7 code | Unmerged stack: umbrella PR #11484 "Implements NU7" (branch `nu7-zips`), with #11527 (ZIP 259), #11528 (ZIP 2003), #11529 (ZIP 218), #11530 (ZIP 237). `nu7-zips` has `(Nu7, ConsensusBranchId(0x77190ad9))`, `POST_NU7_POW_TARGET_SPACING = 25`, `POST_NU7_POW_AVERAGING_WINDOW = 102`, and no NU7 height for Mainnet or Testnet. | [PR #11484][zebra-11484] |
| Zebra value pools under NU7 | On `nu7-zips` (`20a11ee8f2`, 2026-09-25) `getblock`/`getblockchaininfo` still report the same six pools (`value_pools(...) -> [Self; 6]`), and: "The NU7 NSM reserve is deliberately not a pool here: it is not part of the Issued Supply, so `chain_supply()` correctly falls as fees are burned." `chain_supply` is the sum of the six pools. | `zebra-rpc/src/methods/types/get_blockchain_info.rs` on `nu7-zips` |
| Zebra max reorg | `MAX_BLOCK_REORG_HEIGHT: u32 = 1000` on `main`, in v6.4.2, in `nu7-zips`, and already in 6.3.0 (what we run): raised from 99 in **Zebra 5.2.0 (2026-06-18)**, [#10650][zebra-10650]. "A local-only node policy; it is not part of consensus." The **"99 → 600"** figure is ZIP 218's table (`MAX_REORG_LENGTH` 99 → 600, "Scale by 6"), which is out of date for Zebra. | `zebra-chain/src/parameters/constants.rs:30`; Zebra `CHANGELOG.md:382-384` (6.3.0); [ZIP 218][zip218] |
| Zebra end of support | Enforced **only on Mainnet** (`end_of_support.rs:59, 77-79` in 6.3.0: "Release always valid in Testnet"). 6.4.2: `ESTIMATED_RELEASE_HEIGHT = 3_444_000`, `EOS_PANIC_AFTER = 84` days × 1,152 blocks = halt above mainnet height 3,540,768, about 2026-11-03 (estimate from the 2026-09-29 tip). Our testnet zebrads are not affected. | Zebra v6.4.2 source, 6.4.0 release notes |
| zcashd | Repository archived (last push 2026-07-19); last release v6.20.0 (NU6.2). No NU7 release. | github.com/zcash/zcash |
| ZIP 218 details that touch us | 25-s spacing; averaging window 17 → 102; halving interval 5,040,000; subsidy `floor(156250000 / 6)` zat per block after NU7 at this halving; testnet min-difficulty gap 6 × 25 = 150 s; default expiry delta "SHOULD change to ... 120 blocks after activation"; `COINBASE_MATURITY` unchanged at 100; anchor depth stays 3. | [ZIP 218][zip218] §"Default expiry delta" and the block-count constants table |
| Transaction versions | ZIP 259: "NU7 introduces no new transaction format. Once NU7 activates, version 4 transactions are invalid, while version 5 and version 6 transactions remain valid." and "This does not imply that a transaction is valid across NU7 activation, because its signatures commit to a consensus branch ID." Zebra rejects a v5+ transaction whose branch ID is not the current one (`WrongConsensusBranchId`, `zebra-consensus/src/transaction/check.rs`). | [ZIP 259][zip259] |

**What this means for timing.** The testnet height is fixed on Oct 5 and
activation is Oct 6, so the NU7 Zebra release and any crate release that
carries the height will probably land on Oct 5 at the earliest. Plan for
one day. Everything that doesn't need the height must be built, tested and
deployed before then (§4.1).

## 2. Inventory: where Sova depends on Zcash timing, branch IDs, tx versions, reorg depth or subsidy

Severity: **halt** = the testnet stops producing sealed blocks or stops
following Zcash; **wrong** = keeps running but does or says the wrong
thing; **cosmetic** = copy or estimates. Owner: **mech** = mechanical
(dependency bump, config, copy), orchestrator's call; **Rob** =
consensus/emission or a public commitment.

### Transaction building (burns, faucet)

| # | Where | What changes at NU7 | Severity | Fix | Owner |
|---|---|---|---|---|---|
| B1 | `crates/burn-wallet/src/tx.rs:278` (`BranchId::for_height(&network, target_height)`), `:285`/`:313` (`TxVersion::V5`); `crates/burn-wallet/src/network.rs:89-95` (`TEST_NETWORK`/`MAIN_NETWORK` from `zcash_protocol`) | The branch ID is looked up in `zcash_protocol` 0.10.6 (`crates/burn-wallet/Cargo.toml:70`, lock `crates/burn-wallet/Cargo.lock:2469`). There, NU7 exists only under `cfg(zcash_unstable = "nu7")`, with activation height `None` for both networks and a placeholder branch ID `0xffff_ffff` (`zcash_protocol-0.10.6/src/consensus.rs:503-504, 538-539, 775-776, 798-799`). So after activation the builder keeps signing with NU6.3's `0x37a5_165b`, and an NU7 zebrad rejects the burn (ZIP 244's sighash commits to the branch ID). Callers already target `tip + 1` (`miner/src/mine.rs:509`, `faucet/src/faucet.rs:479`), which is right at the boundary. | **halt** (no burns → no sealed blocks → no Sova transactions; keeper and every outside miner) | Two parts. (1) A crate where `BranchId::Nu7 = 0x77190ad9` exists outside `zcash_unstable`: today only librustzcash `main` (§1), so a `[patch.crates-io]` of `zcash_protocol`, `zcash_primitives` and `zcash_transparent` at one `main` commit until a release lands (§4.3). (2) **Take the branch ID from zebrad, not from a height table:** `getblockchaininfo` → `consensus.nextblock`, parsed with `BranchId::try_from(u32)`, cross-checked against `for_height` where the table knows the height. Then the NU7 height, which is only set on Oct 5, never has to be compiled into sova-miner or sova-faucet: upgrading zebrad is enough. This is the design `wzcash/relayer/src/zcash.mjs:11-12` already chose. Tests: signing with `Nu7` round-trips through `Transaction::read`; a `nextblock` of `77190ad9` picks `Nu7`. Ship sova-miner and sova-faucet releases before Oct 5. | mech |
| B2 | `crates/burn-wallet/src/tx.rs:48` `DEFAULT_TX_EXPIRY_DELTA = 40`; `miner/src/epoch.rs:566, 785-806`; `faucet/src/faucet.rs:537, 308` | 40 blocks is ~50 min at 75 s, ~17 min at 25 s. ZIP 218 says the default expiry delta "SHOULD change to ... 120 blocks after activation". A burn built just before activation with the old branch ID can't be mined after it, and a re-send is refused; the miner keeps its inputs reserved until expiry (`epoch.rs:805`), then frees them. | wrong (brief) | Use 120 once the target height is at or past NU7 (with B1's zebrad-reported branch: when `nextblock` is NU7 or later). Optionally pause the keeper for a few blocks around the activation height. | mech |
| B3 | `crates/burn-wallet/src/fee.rs:25` `MARGINAL_FEE_ZAT = 5_000`, `:28` `GRACE_ACTIONS = 2` (ZIP 317) | NU7's NSM burns 60 % of fees (ZIP 235) but does not change what a transaction must pay. No new fee rule is announced. | none known | Re-check ZIP 317 constants against the NU7 Zebra release notes. | mech |
| B4 | `crates/burn-wallet/src/network.rs:63-77` (regtest `LocalNetwork`: NU5 at 1, NU6+ `None`) and `box/regtest/zebrad.toml` (`NU5 = 1`) | Regtest does not activate NU7 unless configured, so the box keeps working unchanged. It also means the box never rehearses the switch. | none (gap) | After the bumps in B1: a box scenario with `NU6..NU6_3 = 1`, `NU7 = 50`, burning across height 50. Needs `LocalNetwork` to gain `nu7` (non-`cfg`) in the new crate. | mech |
| B5 | `crates/burn-wallet/src/utxo.rs:18` `COINBASE_MATURITY = 100` (regtest funding only) | ZIP 218 keeps `COINBASE_MATURITY` at 100 (§1). | none | None. | — |
| B6 | `wzcash/near-vault/src/payout.rs:41-44` (`branch_id` passed in, `expiry_height: 0`); `wzcash/near-vault/src/lib.rs:404-421` (`resign` for another branch ID); `wzcash/relayer/src/zcash.mjs:11-17` | Already designed for NUs: the relayer is to read `getblockchaininfo.consensus.nextblock` from zebrad and the vault can re-sign. `currentBranchId` is still a `TODO(phase 2)` stub, so nothing is live. Tests hard-code NU6.1 `0x4dec_4df0` (`payout.rs:106`, `tests.rs:148,174`). | none today | When phase 2 lands, read the branch ID from zebrad as planned (B1 adopts the same design now). Bump its tests to the NU7 ID. | mech |

### Following Zcash (consensus, engine)

| # | Where | What changes at NU7 | Severity | Fix | Owner |
|---|---|---|---|---|---|
| F1 | `crates/consensus/src/pools.rs:21-31` (`POOLS = 6`, ids transparent, sprout, sapling, orchard, lockbox, ironwood), `:174-191` (exactly six, in order, or error), `:335-376` (`check_pools`: continuity, tx sums, **pools sum to `chainSupply`**); `crates/consensus/src/follower.rs:236-251` (strict: hold, never skip); on for the public testnet via `SOVA_SIP7=1` (`crates/engine/src/expectations.rs:450`, `bin/sova/src/chain.rs:400`, `infra/testnet/host/setup-host.sh:34`) | NU7's ZIPs 234/235 change how issued and burned ZEC is counted. If the NU7 Zebra's `getblock` added a seventh `valuePools` entry, or the six pools stopped summing to `chainSupply`, every SIP-7 node would hold at the first NU7 block for good. On Zebra's reference branch `nu7-zips` neither happens: still six pools, and the NSM reserve "is deliberately not a pool", with `chain_supply` the sum of the six (§1). So this is **expected to be fine**, but it is a branch, not a release. | **halt** if the release differs; expected none | When the release lands, run it on regtest with NU7 set a few blocks ahead, send a fee-paying transaction across the height, and diff `getblock <h> 1` against 6.3.0: pool ids and order, `chainSupply`, per-tx `valueBalance*` fields. If anything differs, update `POOL_IDS`/`check_pools` and SIP-7 §3 before the fleet upgrades. | mech (Rob only if the pool list changes) |
| F2 | `crates/engine/src/candidates.rs:65-73` `FINALIZED_DEPTH = 100`; `crates/engine/src/expectations.rs:135-137` `STALE_SCAN_MAX = 100`; `crates/engine/src/driver.rs:203-205` `SETTLED_KEEP` | The comments say "Zebra rolls back at most 99 blocks". That is out of date: **Zebra 5.2.0 (2026-06-18) raised `MAX_BLOCK_REORG_HEIGHT` from 99 to 1,000** ([#10650][zebra-10650]; `zebra-chain/src/parameters/constants.rs:30` in 6.3.0: `pub const MAX_BLOCK_REORG_HEIGHT: u32 = 1000;`, described as "a local-only node policy; it is not part of consensus"). Our own `docs/ops/snapshots.md:38` already says 1,000. It was never a consensus limit, and zebrad now follows reorgs up to 1,000 deep, so a Zcash reorg of 101+ blocks can reach Sova today and wedge a node (reth refuses a head below `finalized`, `candidates.rs:66-71`). ZIP 218 makes this 3× more likely per unit of time: 100 blocks is ~2 h 5 min of Zcash at 75 s but ~42 min at 25 s. The "600 after NU7" figure is ZIP 218's `MAX_REORG_LENGTH` row, which is out of date for Zebra: Zebra is at 1,000 before and after NU7 (§1). | wrong now; **halt** (node wedge) on a deep reorg | Raise `FINALIZED_DEPTH` and `STALE_SCAN_MAX` together. 300 keeps the old ~2 h at 25 s; 1,000 matches Zebra's window. Fix the four "99-block" comments (`candidates.rs:67`, `expectations.rs:136,232`, `driver.rs:204`) and `sips/sip-8-draft-anchored-burns.md:9`, `docs/design/faster-blocks.md:140`. `finalized` is a public label (Rob set it on 2026-09-23), so the number is his; the code change is mechanical. | Rob (label), mech (code) |
| F3 | `crates/engine/src/expectations.rs:48-50` `REORG_WINDOW = 1024` | Follower's window, in Zcash blocks. At 25 s it is ~7 h. Matches Zebra's 1,000. | none | None. | — |
| F4 | `crates/engine/src/seal.rs:204-224` `MAX_SEAL_DRIFT = 900` and `timestamp_window` | Time-based (seconds past the Zcash block time, floor parent + 1). 25-s blocks and 3-7 s testnet bursts both fit. | none | None. | — |
| F5 | `crates/consensus/src/sealer.rs:29-31` `DEFAULT_RANK_STEP = 15 s`; `crates/engine/src/driver.rs:219-225` `ABANDON_GRACE_RUNGS = 2`, `RETRIGGER = 15 s` | Fallback sealers wait `rank × 15 s`. At 75 s, rank 2 fires before the next Zcash block most of the time; at 25 s, rank 2 (30 s) usually fires after it. Not a halt: the testnet already runs 3-7 s bursts (`miner/src/mine.rs:24-28`) and epochs queue in order. It is a latency cost when rank 0 is offline. | wrong (latency) | Measure sealed % and time-to-seal over a few hundred blocks after 2026-10-06. If it degrades, halve the step. It is a draft SIP-6 parameter. | Rob if changed (SIP parameter) |
| F6 | `crates/consensus/src/sip1.rs` / follower: burns are read from any v5 transparent output (`follower.rs:254-262`, `zebrad.rs:140-152` reads `version`) | NU7 makes v4 invalid, keeps v5 and v6 (v6 came with NU6.3), and adds no new format ([ZIP 259][zip259]). We build v5 (`tx.rs:285, 313`). | none | None for NU7. | — |
| F7 | `sips/sip-8-draft-anchored-burns.md:595-612` and `--vote-wait` (`crates/burn-wallet/miner/src/anchor.rs:253-270`) | The miss chance for a 10-s vote wait is `1 − e^{−10/75}` = 12.5 % at 75 s but `1 − e^{−10/25}` = 33 % at 25 s. `MAX_ANCHOR_LAG = 2` (`anchor.rs:58`) is in blocks, so it shrinks in time. | wrong (more lagged votes) | Re-derive the recommended `--vote-wait` for 25 s (about 3-4 s for the same ~12 % miss) before SIP-8 is final. | Rob (SIP-8 draft decision 4) |
| F8 | `crates/burn-wallet/miner/src/epoch.rs:61` `REORG_WATCH_DEPTH = 10` | 10 blocks is ~12.5 min at 75 s, ~4 min at 25 s. A reorged-out burn deeper than that is not re-sent. | wrong (rare) | Raise to 30 to keep the time. | mech |

### Emission and supply (SIP-2, SIP-3)

| # | Where | What changes at NU7 | Severity | Fix | Owner |
|---|---|---|---|---|---|
| E1 | `crates/consensus/src/schedule.rs:21-29` (`BASE_EPOCH_REWARD_GWEI` 6,250 SOVA, `ERA_EPOCHS = 1_680_000` "Zcash's halving interval (~4 years at 75 s)", `SLOW_START_EPOCHS = 20_000` "~17.4 days at 75 s"); `sips/sip-3.md:16, 25-28, 50, 91-93`; `sips/sip-2.md:53` | ZIP 218 changes Zcash's halving interval from 1,680,000 to 5,040,000 blocks and divides the per-block subsidy by 3 so ZEC issuance per day is unchanged ([ZIP 218][zip218]). Sova's schedule counts epochs, and one epoch is one Zcash block. **If SIP-3 stays as written, SOVA issuance per day triples** (6,250 × 3,456 = 21.6 M SOVA/day instead of 7.2 M), eras last ~1.33 years instead of ~4, the slow start lasts ~5.8 days instead of ~17.4, and emission ends after ~57 years instead of ~172. The total supply in SOVA does not change. The SIP-3 rationale "halving at 1,680,000 epochs is inherited, not invented" (`sip-3.md:91`) stops being true: Zcash's interval is now 5,040,000. The schedule is not wired into the node yet (`schedule.rs:3-6`); the public testnet mints a flat 6,250/epoch (`bin/sova/src/chain.rs:118`). Sova mainnet (Q1 2027) starts after Zcash mainnet NU7 (2026-11-05), so mainnet will only ever see 25-s blocks. | wrong (economics and public claims); nothing breaks | **Rob's decision** (§5). No code change is needed before 2026-10-06. | **Rob** |
| E2 | Testnet flat 6,250/epoch (`bin/sova/src/chain.rs:118`) | 3× more testnet SOVA per day. Harmless for a testnet. | cosmetic | None, unless Rob wants testnet to preview the mainnet choice in E1. | Rob (optional) |
| E3 | `docs/paper/v5-notes.md:133`, `sips/sip-8-draft-anchored-burns.md:617` ("1,152 epochs a day", "about 0.24 ZEC a day" for a floor miner) | A miner burning every epoch pays per epoch: 3,456 epochs a day at 25 s, so ~0.73 ZEC/day (v1) or ~0.90 (v2). This is the real cost of "burn every block" on mainnet. | wrong (copy, miner economics) | Update the numbers. The fairness/cost framing is Rob's. | mech (numbers), Rob (framing) |

### Contracts that count Zcash blocks as time

These are fixed at deploy. Testnet deployments are in
`infra/testnet/deployments/sova-testnet.json`.

| # | Where | 75 s → 25 s | Severity | Fix | Owner |
|---|---|---|---|---|---|
| C1 | `contracts/src/Ashwings.sol:60-65` `ZEC_WINDOW = 40` ("~50 min"), `ZEC_MINCONF_MAINNET = 10` ("~12.5 min"), `ZEC_MINCONF_TESTNET = 3`; `site/src/scripts/checkout/app.ts:225` (`* 75 / 60` countdown) | The ZEC quote window becomes ~17 min; the live testnet checkout page overstates the time left by 3× after Oct 6. Mainnet 10 confirmations become ~4 min. | **wrong** (user-facing, testnet, from Oct 6) | Now: make `app.ts` read the measured block rate or use 25 s after activation. Mainnet deploy: choose `ZEC_WINDOW` / `ZEC_MINCONF_MAINNET` in 25-s blocks (for example 120 and 30 keep the old times). | mech (UI), Rob (mainnet constants) |
| C2 | `contracts/src/wzcash/WzecBridge.sol:146-147` `WINDOW = 1_152` ("24 h at 75 s") | The daily cap becomes an ~8-hour cap: 3× the intended daily throughput. | wrong (risk limit) | 3,456 for mainnet. wz.cash is Sova Labs' product, so its owner signs off. | mech |
| C3 | `contracts/src/zcash/ZcashBlocks.sol:16, 112` ring 8,191 blocks ("about 7.1 days"); `sips/sip-7-draft-zcash-events.md:148, 283` | ~2.4 days of history at 25 s. It is part of the state root, so the size is consensus. | wrong (docs) | Update the copy. Whether to grow the ring is a SIP-7 decision. | Rob if resized |
| C4 | `contracts/src/zcash/IZcash.sol:59`, `ZcashLib.sol:12, 211`, `ZecCheckout.sol:154`, `ZecEscrow.sol:116`, `AshwingsZecCheckout.sol:44`; `sips/sip-4-draft-zcash-state-precompile.md:274-276` ("mainnet 10 ≈ 12.5 min at 75 s") | Recommended `minConf` defaults are in blocks. The same number of blocks is a third of the time. Each 25-s block carries about a third of the work, so the same assurance needs about 3× the blocks. | wrong (guidance) | Library docs: testnet 3 → keep; mainnet 10 → 30. | mech |

### Infrastructure (testnet hosts, box, laptop)

| # | Where | What changes | Severity | Fix | Owner |
|---|---|---|---|---|---|
| I1 | `infra/testnet/config.env.example:20-22` and live `infra/testnet/config.env:21-22` (`zfnd/zebra:6.3.0`, digest pinned); `infra/testnet/published/seeds.json:14`; `infra/testnet/host/zebrad.toml:3`; `box/regtest/docker-compose.yml:3`; `box/testnet/test/docker-compose.yml:7`; `box/up/README.md:15`; `docs/guides/testnet.md:101`, `docs/guides/testnet-reference.md:34` | Zebra 6.3.0 has no NU7 heights (§1) and will reject NU7 blocks. (End-of-support halts do not apply here: Zebra only enforces them on Mainnet, `zebrad/src/components/sync/end_of_support.rs:59, 77-79` in 6.3.0.) | **halt** (all 5 hosts, the laptop node, every outside node that followed the guide) | Upgrade every zebrad to the NU7 release before the activation height; pin tag and digest; update `seeds.json`, the guides, and the box. | mech |
| I2 | `docs/guides/testnet.md:57-89`, `box/testnet/snapshot.sh`, `https://dl.testnet.sova.io/zebrad-testnet/4390524/snapshot.json`; `config.env.example:20` ("snapshots must match its state format (6.3.0 = state v28)") | If the NU7 release bumps Zebra's state format, the published snapshot is either upgraded in place on first start or refused. A 6.3.0 snapshot restored under the new image must be tested. | wrong (joiners) | After upgrading, cut a new snapshot above the activation height with the new `zebra_version`, publish it, update the guide. | mech |
| I3 | `infra/testnet/host/health.sh:28-41, 78-93` (`EPOCH_LAG_ALERT = 10` epochs for `EPOCH_LAG_PERSIST_MIN = 10`, `BLOCK_AGE_ALERT_MIN = 10`, `NULL_SEALED_MAX_MIN = 45`, `NULL_SEALED_WALK = 1800`); `infra/testnet/smoke.sh:75, 152`; `infra/testnet/config.env.example:230-247` | The alerts are already time-based, which is right. `EPOCH_LAG_ALERT = 10` epochs is ~4 min at 25 s instead of 12.5 min, but it must persist 10 min, so no new false alarms. `smoke.sh:152` ("epochs are ~75 s", 90-s head check) will pass more often, not less. `NULL_SEALED_WALK = 1800` blocks covers 12.5 h ≥ 45 min. | cosmetic | Update comments; consider `EPOCH_LAG_ALERT=30`. Add the fork alert in §4.2. | mech |
| I4 | Keeper: `infra/testnet/host/setup-host.sh:762-764` (`KEEPER_MIN_BURN_INTERVAL_SECS=30`), live `config.env:202-203` (`KEEPER_BUDGET_ZAT=900000000`, lifetime 3,000,000,000); `crates/burn-wallet/miner/src/mine.rs:24-39` | The keeper runs in demand mode with a 30-min heartbeat, and the throttle bounds it at 2,880 burns a day whatever the block rate. So spend stays bounded. A keeper burning every block would pay 3× per day. | none (bounded) | None. Watch the spend rate for a day after Oct 6. | mech |
| I5 | `infra/testnet/epoch-base.sh:71-73, 91` (`* 75 / 60` minute estimates) | Estimates only. | cosmetic | Use 25. | mech |

### Copy (site, docs, SIPs, paper)

All cosmetic, all mechanical except where the number comes from E1.

| # | Where | Says | After NU7 |
|---|---|---|---|
| D1 | `site/src/data/sova.ts:46, 49, 117` (`HALVING_EPOCHS = '1,680,000'`, `BLOCK_SECONDS = 75`, "halving every 1,680,000 epochs, Zcash's own interval"); `site/src/pages/v/terminal.astro:75, 83`, `v/2009.astro:57`, `v/burn.astro:76` ("like Zcash", "as Zcash") | 75 s; Zcash's interval | "like Zcash" is false once Zcash halves every 5,040,000. Depends on E1. |
| D2 | `site/src/components/Paper.astro:268, 384, 709-710`; `docs/paper/sova-paper-v5.md:45, 108`; `docs/paper/v5-notes.md:75, 133-151` | ~75 s; "Zcash's own halving"; eras "about four years"; "a little over 170 years" | Depends on E1. |
| D3 | `README.md:81`; `docs-site/pages/start/what-is-sova.md:31` | "halving every 1,680,000 epochs" | Depends on E1. |
| D4 | `sips/sip-3.md:16, 27-28, 50, 91-93`; `sips/sip-2.md:53` | "~4 years at 75 s", "~17.4 days", "~176 years", "4-year halvings at 75 s" | Depends on E1. |
| D5 | `sips/sip-6-draft-sealer-signatures.md:96, 352, 666`; `sips/sip-7-draft-zcash-events.md:148, 283`; `sips/sip-8-draft-anchored-burns.md:602-617`; `sips/sip-4-draft-zcash-state-precompile.md:276` | 75-s arithmetic | Recompute at 25 s. |
| D6 | `docs/guides/testnet-reference.md:95, 454`; `bin/sova/src/tx_gossip.rs:17`; `bin/sova/src/send_sync.rs:4`; `site/src/scripts/checkout/app.ts:84`; `docs/design/faster-blocks.md` §1 (its whole latency table is at 75 s) | "about every 75 seconds" | "about every 25 seconds (75 before NU7)". `faster-blocks.md`'s conclusion gets stronger: a 25-s median wait of ~17 s removes much of the case for sub-blocks. |

## 3. What happens on each date

**Testnet, 2026-10-06 (Zcash testnet NU7 height, §1).**
With nothing done: our six zebrads (5 hosts + laptop) reject the first
NU7 block (I1). Sova follows its own zebrad, so either the Sova head stops
and `block_age` fires in 10 minutes with "zebrad's tip is N s old", or,
if some old-rules miner keeps mining, Sova follows that dead chain, with
burns that land nowhere the upgraded world sees (§4.2). With only the
Zebra upgrade done: Zebra follows NU7, but every burn and drip is rejected
(B1): the head keeps moving on null blocks, and `null_run` fires after
45 min. With B1 and I1 done (and F1 confirmed), the testnet runs at about
3,456 epochs a day. The first NU7 Zcash block `H7` settles as Sova block
`H7 − 4,388,500 + 1` (SIP-4 anchor `E_N = N + B − 1`).

**Mainnet, 2026-11-05.** Sova has no mainnet yet. What matters is that
public copy, SIPs and the paper stop describing a 75-s world, and that
SIP-3 is decided for the Q1 2027 genesis (E1). The wz.cash product and any
Zcash-mainnet-facing tool (Ashwings ZEC checkout on mainnet, if any)
needs NU7-aware zebrad and branch ID handling by then.

## 4. Checklist and fallback

### 4.1 Dated checklist

Before **2026-10-06** (testnet). The testnet height is only set on
Oct 5, so everything that doesn't need it is done and deployed by
**Oct 4**, and Oct 5 is only a zebrad upgrade.

| When | Item | Refs |
|---|---|---|
| Sep 30 | Watch (a) ZIP 259 for the testnet height `H7`, (b) Zebra releases / PR #11484 for the NU7 release, (c) librustzcash for a `zcash_protocol` release with `Nu7 = 0x77190ad9`. Record `H7` and its Sova height `H7 − 4,388,500 + 1` on the board when set. | §1 |
| Sep 30 – Oct 2 | **B1:** burn-wallet reads the branch ID from zebrad's `getblockchaininfo.consensus.nextblock` and builds with `[patch.crates-io]` of `zcash_protocol`/`zcash_primitives`/`zcash_transparent` at one librustzcash `main` commit (at or after `92859218`) until a release exists. Expiry delta 120 once `nextblock` is NU7 (B2). Tests as in B1. Local suite, then public CI (private Actions are unusable). | B1, B2, §4.3 |
| Sep 30 – Oct 2 | **F2:** Rob picks the `finalized` depth; raise `FINALIZED_DEPTH` and `STALE_SCAN_MAX` together; fix the "99-block" comments. Same node release as below. | F2 |
| Sep 30 – Oct 2 | Fork detection in `health.sh` (§4.2); checkout countdown stops hard-coding 75 s (C1); `REORG_WATCH_DEPTH` 30 (F8). | §4.2, C1, F8 |
| Oct 3 | Cut sova-miner, sova-faucet and sova node releases. Deploy the miner to the keeper and the faucet to the faucet host; confirm they still burn and drip on today's (NU6.3) chain, with the branch now coming from zebrad. Update `docs/guides/testnet.md` minimum versions. Post in t.me/sovazec: upgrade sova-miner now, zebrad on Oct 5 when the NU7 release is out. | B1 |
| Oct 3 – 4 | Dry-run the zebrad upgrade path on one seed with 6.4.2 (image swap, digest pin, state format migration, restart, re-sync to tip), so Oct 5 is only a tag change. | I1, I2 |
| Oct 5, release + ~1 h | Regtest check with the NU7 image, NU7 a few blocks ahead: a burn across the height is mined; `getblock 1` pools and `chainSupply` still pass SIP-7 (F1). If F1 fails: stop and fix before the fleet upgrade. | F1, B4 |
| Oct 5, release + ~2 h | Upgrade zebrad on all five hosts (`ZEBRA_IMAGE` + `ZEBRA_IMAGE_DIGEST`, `deploy.sh`), one seed first, then the rest; then the laptop node and the box compose files. Confirm each reports the NU7 height in `getblockchaininfo.upgrades`. Update `published/seeds.json`, `box/regtest/README.md`, `box/up/README.md`, guides. Re-post in t.me/sovazec. | I1 |
| `H7 − 2` .. `H7 + 3` | Watch: zebrad tip passes `H7`; `consensus.chaintip` = `77190ad9`; first post-`H7` burn mined; sealed blocks resume; no SIP-7 hold. | B2, F1 |
| Oct 6 – 7 | Measure sealed % and time-to-seal over several hundred blocks; keeper spend per day; alert noise. | F5, I3, I4 |
| Oct 7 – 10 | New zebrad snapshot above `H7`, publish, update the guide. | I2 |

Before **2026-11-05** (mainnet NU7):

| Item | Refs |
|---|---|
| Rob decides SIP-3 (§5). Then update `schedule.rs` constants and tests if they change, SIP-3, SIP-2 §Rewards, the paper, the site constants and pages, README, docs-site. | E1, D1-D4 |
| Recompute the 75-s arithmetic in SIP-4/6/7/8 drafts and `faster-blocks.md`. | D5, D6, F7 |
| Mainnet contract constants in 25-s blocks: Ashwings `ZEC_WINDOW` / `ZEC_MINCONF_MAINNET`, WzecBridge `WINDOW`, library `minConf` guidance. | C1, C2, C4 |
| wz.cash relayer: implement `currentBranchId` from zebrad before any mainnet payout. | B6 |
| Any Zcash-mainnet zebrad we run (wz.cash, ZEC checkout) on the NU7 release by Oct 20 (when the mainnet height is set) and before Nov 5. Mainnet enforces end of support: 6.4.2 halts above height 3,540,768, about Nov 3, so pre-NU7 releases stop on their own just before activation. | I1 |
| Drop the librustzcash `[patch]` for the first release that carries NU7 (B1, §4.3). | B1 |

### 4.2 If our zebrads don't upgrade in time: what happens, how to see it

A non-upgraded zebrad treats the first NU7 block as invalid: its coinbase
carries the NU7 branch ID, and ZIP 218 changes difficulty and subsidy.
Two cases:

- **Nobody mines old rules:** the old zebrad's tip stops at `H7 − 1`.
  Sova's follower sees no new Zcash blocks, so the Sova head stops.
  `block_age` (`health.sh:180-197`) fires after 10 min, and its message
  already says "zebrad's tip ... is N s old", which points at Zcash, not
  Sova. Nothing is lost. Upgrading zebrad resumes the chain, and Sova
  catches up.
- **Someone keeps mining old rules:** the old zebrad follows that chain.
  On testnet this is cheap. Min-difficulty blocks are allowed after a
  gap (6 × 75 s under the old rules), so one leftover CPU miner can keep
  an old-rules chain going. Sova then keeps producing blocks anchored to
  an abandoned fork, with burns that only old-rules miners include. When
  zebrad is upgraded it reorgs to the real chain. If the fork is deeper
  than `FINALIZED_DEPTH` (100 today, F2), Sova nodes wedge and need a
  state reset. Everything since `H7` is lost.

**Detection** (add to `health.sh` before Oct 6; it runs every 2 min):

1. `getblockchaininfo` → `consensus.chaintip`. From the Oct 6 wall-clock
   date on, anything other than `77190ad9` after the tip has passed `H7`,
   or an `upgrades` list without NU7, means a pre-NU7 zebrad: alert
   `zcash_fork`.
2. Compare our zebrad's block hash at `tip − 3` with the other hosts'
   zebrads (they already compare heads for `epoch_lag`) and with a public
   testnet explorer if one is reachable. A mismatch alerts.
3. `smoke.sh`: every host's zebrad image digest matches the pinned NU7
   release.

**If the NU7 Zebra release is late** (not out by Oct 6): our zebrads
stop at `H7 − 1` or follow an old-rules chain. Prefer the stall. If an
old-rules chain appears (detection 2, or the tip keeps moving past `H7`
with a pre-NU7 branch ID), stop the keeper so no burns go into it, and
say in t.me/sovazec that the testnet is paused until the Zebra release.
When it lands, upgrade and let the chain resume. If our nodes followed
the old chain deeper than `FINALIZED_DEPTH`, restore from a snapshot
below `H7`. Don't start a new Sova genesis: a stall is recoverable.

### 4.3 If librustzcash is late

The branch ID is only data in the sighash. No `zcash_protocol` release has
`BranchId::Nu7 = 0x77190ad9` yet, but librustzcash `main` does (commit
`92859218`, outside `zcash_unstable`). So B1 doesn't wait on a release: a
`[patch.crates-io]` in `crates/burn-wallet/Cargo.toml` (and the miner and
faucet manifests, which share its lockfile) pins `zcash_protocol`,
`zcash_primitives` and `zcash_transparent` to one `main` commit. Two
cautions: `zcash_primitives` 0.30.x hard-pins pre-release crypto crates
(`Cargo.toml:13`, `crates/burn-wallet/Cargo.toml:4`), so a `main` build may
move those pins; and `main` has no NU7 heights, which is why B1 takes the
branch ID from zebrad instead of from `for_height`. Remove the patch when
a release lands. Without one of the two, the miner cannot burn after `H7`.

## 5. Decisions for Rob

1. **SIP-3 after ZIP 218 (E1).** Sova mainnet will only see 25-s blocks.
   Options:
   - **(a) Keep SIP-3 as written** (6,250/epoch, 1,680,000-epoch eras,
     20,000-epoch slow start). SOVA issues 3× faster per day than the
     paper says: eras of ~1.33 years, a ~5.8-day slow start, emission ends
     in ~57 years. The total is the same. "Zcash's own interval" and "like
     Zcash" must go from the copy.
   - **(b) Follow ZIP 218** (per-epoch reward ÷ 3, eras × 3, keep the
     wall-clock schedule and the "Zcash's own interval" story). Needs new
     exact numbers: 6,250 / 3 is not a whole number of gwei, and the slow
     start's step must still divide the base exactly
     (`schedule.rs:35-38`). One exact pair: base 2,083.33332 SOVA
     (2,083,333,320,000 gwei) with a 60,000-epoch slow start (step
     34,722,222 gwei). `FINAL_ERA` becomes 41 (`schedule.rs:40-41`). The
     total changes very slightly.
   - **(c) Follow ZIP 218 but with round numbers**, e.g. 2,000 SOVA/epoch,
     5,040,000-epoch eras. Simpler; a different total.

   The testnet does not need this answered by Oct 6 (it mints a flat
   6,250). The public copy does need it by Nov 5, when "halving every
   1,680,000 epochs, like Zcash" becomes wrong.
2. **`finalized` depth (F2).** 100 is below Zebra's 1,000-block rollback
   window today and is only ~42 min at 25 s. Recommend 300 (the old ~2 h)
   or 1,000 (Zebra's window). Code change is small once decided.
3. **SIP-7 pool list (F1)**, only if the NU7 Zebra adds a pool: whether
   contracts see it.
4. **Draft SIP timing parameters** (F5 rank step, F7 vote wait, C3 ring
   size, C1 mainnet contract windows): recompute for 25 s, or leave until
   testnet data exists. None blocks Oct 6.

[fwd-timeline]: https://forum.zcashcommunity.com/t/nu7-timeline/57655
[zip218]: https://zips.z.cash/zip-0218
[zebra-10650]: https://github.com/ZcashFoundation/zebra/pull/10650
[zebra-11484]: https://github.com/ZcashFoundation/zebra/pull/11484
[zebra-rel]: https://github.com/ZcashFoundation/zebra/releases
[zip259]: https://zips.z.cash/zip-0259
[lrz-consensus]: https://github.com/zcash/librustzcash/blob/main/components/zcash_protocol/src/consensus.rs
