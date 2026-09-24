# Block explorer at M1: Otterscan, Blockscout, or none at launch

Status: **draft for Rob's decision** (WORKPLAN "Decide (engineering)" item
12; infra-m1 D7). Written 2026-09-24. Nothing here is deployed. No
accounts were created and nothing was bought. No code changed: the
prototype build described in §8 was never committed.

Tags: **[fact]** checked in this repo, in the pinned reth v2.6.0 source,
or in a primary source. **[measured]** observed in the §8 prototype.
**[est]** an estimate to re-check before money moves.

## The decision in four lines

1. **Otterscan works against our node.** reth v2.6.0 serves the `ots_`
   namespace at API level 8. Block, transaction, trace, revert-reason,
   internal-transfer and contract-creator pages rendered against a local
   Sova dev node [measured].
2. **Otterscan has two gaps that matter for Sova.** An address page can't
   list its transactions, because reth leaves `ots_searchTransactions*`
   unimplemented. And SOVA mints are invisible: the block page ignores
   withdrawals and shows "Block Reward: 0 SOVA" for a block that minted
   6,250 [measured].
3. **Blockscout shows mints and address history, but it's a service to
   run.** It needs Postgres, an indexer and a debug-trace node on the same
   box, about 8–16 GB of RAM, and it has to be re-indexed at every testnet
   reset.
4. **Recommendation (§7):** no hosted explorer on launch day. The guide
   and the site say so plainly and give `cast` recipes that work through
   the public RPC. Otterscan on Pages follows once the node and the edge
   serve a bounded `ots_` subset. Blockscout waits until dapp builders ask
   for contract verification.

## 1. What a Sova explorer has to show

Five things differ from a stock EVM chain. Each one is checked against
each option below.

| Sova specific | What it is on the wire |
| --- | --- |
| **Mints** | An epoch's SOVA is minted through the block's EIP-4895 **withdrawals**: one entry per payee, `amount` in **gwei**, `validatorIndex` always `0`, and `index` restarting at 0 in every block (`crates/engine/src/payload.rs:117`, `builder.rs`) [fact]. There is no mint transaction and no log. |
| **Sealed blocks** | `extraData` is **exactly 97 bytes**: a 32-byte vanity, then `r`, `s`, and `v` of the sealer's signature (SIP-6 §2.1) [fact]. `miner` is the sealer's own address. |
| **Null blocks** | An epoch with no ranked sealer gets a block with empty `extraData`, `miner = 0x0`, no transactions and no withdrawals (SIP-6 §2.4) [fact]. Null blocks are normal, not an error. |
| **ZcashBlocks at `0x…5A01`** | Predeployed in genesis, written once per block by a **system call** from `0xff…fe` (SIP-7 §4.1) [fact]. Its storage changes every block, but no transaction ever touches it. Views: `latest()`, `window()`, `summary(h)`, `poolTotals(h)`, `blockStats(h)` (`contracts/src/zcash/ZcashBlocks.sol`). |
| **The SIP-4 precompile at `0x…5A00`** | No code. Calls to it only work on a Sova node, since tooling that re-executes locally doesn't have it (see `cast run`, §5). |

Two limits also bound the cost of anything that re-executes a
transaction. Blocks have a 30,000,000 gas limit, and Osaka's per-tx cap is
16,777,216 gas. The node refused a 29 M-gas transaction with "gas limit
too high" and estimates above 2^24 with "gas required exceeds: 16777216"
[measured].

## 2. What our node and edge serve today

- **The public profile** (`SOVA_RPC_PROFILE=public`, `bin/sova/src/rpc.rs`)
  builds only `eth`, `net` and `web3`, then strips every HTTP method
  outside `PUBLIC_RPC_METHODS`: 23 methods, plus `sova_getZcashBlocks`
  with SIP-7 [fact]. `ots` is in the test's `DENIED` list, so it is never
  built.
- **The edge Worker** (`infra/testnet/worker/rpc-firewall.mjs`) enforces
  the same list, caps a batch at **10 calls** (and refuses the whole
  batch above that), caps bodies at 64 KiB and `eth_getLogs` ranges at
  1,000 blocks, and rate-limits each IP to 50 requests per 10 s [fact].
- **Denial of `ots_` is tested.** Both `infra/testnet/worker/test.mjs`
  (line 105) and `infra/testnet/smoke.sh` (line 77) assert that
  `ots_getApiLevel` is denied [fact].
- **The local profile doesn't serve `ots_` either.** It uses reth's
  standard HTTP set (`eth`, `net`, `web3`), there's no env var to add a
  namespace, and the node builds its config in code, so reth's
  `--http.api` flag isn't reachable [fact, `main.rs`, `rpc.rs`]. So the
  line in infra-m1 §2, "users can point it at their own node", is **not
  true today** without a small code change (§7).

## 3. Option 1: Otterscan

Otterscan is a static single-page app. The browser calls a JSON-RPC URL
directly. There is no server, database or indexer of its own.

### 3.1 What it needs from our node [fact, reth v2.6.0 `crates/rpc/rpc/src/otterscan.rs`]

reth's `OtterscanApi` reports `API_LEVEL = 8`, which is what Otterscan
checks at startup. Methods, grouped by what they cost:

| Tier | Methods | What reth does |
| --- | --- | --- |
| **A. Lookups** (no re-execution) | `ots_getApiLevel`, `ots_getHeaderByNumber` plus its alias `erigon_getHeaderByNumber` (Otterscan calls the alias), `ots_getBlockDetails`, `ots_getBlockDetailsByHash`, `ots_getBlockTransactions`, `ots_hasCode`, `ots_getTransactionBySenderAndNonce` | Header, block and receipt reads. The nonce lookup is a binary search over account history, O(log head) state reads. |
| **B. Bounded replay** | `ots_traceTransaction`, `ots_getInternalOperations`, `ots_getTransactionError`, `ots_getContractCreator` | Each re-executes one transaction, including every transaction before it in its block. The contract-creator lookup binary-searches `eth_getCode`, then traces one whole block. Worst case: one 30 M-gas block. |
| **Not served** | `ots_searchTransactionsBefore`, `ots_searchTransactionsAfter` | `Err("unimplemented")` in reth. They need an address index that reth doesn't keep. |
| **Outside `ots`** | `trace_replayTransaction` (the "State Diff" tab), `ots2_*` (the address "Withdrawals" tab; Erigon 3 only) | Needs the `trace` namespace, or doesn't exist in reth at all. |

Otterscan also calls ordinary `eth_*` methods that are already on the
public allowlist: `eth_getTransactionByHash`, `eth_getTransactionReceipt`,
`eth_getTransactionCount`, `eth_getBalance`, `eth_getCode`, `eth_call`
(token `name()`/`symbol()` probes on every address it shows),
`eth_blockNumber` and `eth_chainId` [measured].

### 3.2 Prototype results [measured, §8]

The setup: a local Sova dev node with `ots` enabled, the
`otterscan/otterscan` image (Otterscan 2.x, built 2025-11-06), and a
logging proxy between them. Test traffic: a transfer, a contract deploy,
a call that forwards value, a factory `CREATE`, an ERC-20-style mint, and
a call that reverts.

| Page | Renders? | RPC calls that failed |
| --- | --- | --- |
| Home (latest block) | Yes | none |
| Block `/block/N` | Yes: height, time, miner, fees, gas, base fee, extraData (as text and hex), parent beacon root | none |
| Block transactions `/block/N/txs` | Yes | `ots_hasCode` ×6: "Invalid params" (see below) |
| Tx overview `/tx/H` | Yes: status, value, fees, internal SOVA transfers, decoded method (where the ABI is known), ERC-20 "Tokens Transferred" | `ots_hasCode` |
| Tx revert | Yes: "Fail with revert message: 'sova says no'" | none |
| Tx trace `/tx/H/trace` | Yes: call tree including the inner `CREATE` and the value call | `ots_hasCode` |
| Tx logs `/tx/H/logs` | Yes, raw. Events are decoded only if in Otterscan's topic0 DB | none |
| Tx state diff `/tx/H/statediff` | **Blank** with the `ots` subset. It renders balance and nonce diffs once `trace` is on | `trace_replayTransaction`: "Method not found" |
| Tx by nonce `/address/A?nonce=N` | Yes | none |
| Address overview `/address/A` | **Partly.** Balance, nonce count, contract creator and name work. The transaction list stays on "Waiting for search results..." forever | `ots_searchTransactionsBefore`: "unimplemented" |
| Contract tab | Yes. Its ABI is estimated from bytecode ("not found in Sourcify") | none |
| Address withdrawals tab | **Empty** | `ots2_getWithdrawalsCount`: "Method not found" |

Three quirks the prototype found:

- **`ots_hasCode` fails on every list page.** Otterscan sends the block
  as a bare integer, `["0x…", 50]`, and reth v2.6.0's EIP-1898
  deserializer rejects it: "invalid type: integer `51`, expected Block
  identifier" [measured]. The effect is cosmetic: the contract/EOA badge
  is missing next to addresses in lists. It's an upstream reth/Otterscan
  mismatch. We shouldn't patch it ourselves.
- **Otterscan batches.** It uses ethers v6's `JsonRpcProvider` with
  default options, so up to 100 calls go in one HTTP batch
  (`batchMaxCount:100` in the bundle) [fact]. A transaction page makes
  about 20–30 calls [measured]. Our Worker refuses **any** batch over 10,
  so hosted Otterscan would break behind today's Worker. The prototype
  proxy didn't cap batches, so this is inferred, not observed.
- **Chain metadata comes from `config.json`.** With
  `chainInfo.nativeCurrency.symbol = "SOVA"` every amount reads "SOVA"
  [measured]. The image ships `chains/eip155-*.json` for 1,081 chains;
  82330 isn't among them [fact]. Our build would add that file.

### 3.3 How it shows Sova specifics

| Specific | Otterscan |
| --- | --- |
| Mints (withdrawals) | **Invisible.** The block page never reads `withdrawals`. The only withdrawals views are the beacon-API slot pages and the Erigon-3-only address tab [fact, bundle]. A test block dressed with two mint withdrawals rendered exactly like one without [measured, synthetic]. "Block Reward" shows `0 + fees`, because reth's `ots_getBlockDetails` returns zero issuance [fact]. **For Sova this is misleading**: it looks like sealers earn nothing. |
| 97-byte sealed extraData | Shown as garbled UTF-8 (`sova` + control bytes) followed by the full hex [measured, synthetic]. The data is all there, but nothing recovers or labels the signer. `Mined by` shows `miner`, which is the sealer. |
| Null blocks | Render cleanly: `Mined by 0x000…000`, `Extra Data: (Hex: 0x)`, 0 transactions [measured, synthetic]. Nothing labels them as null blocks. |
| ZcashBlocks `0x…5A01` | Would show as a contract with no creator and **no transactions**, since system-call writes aren't transactions. Read Contract needs a verified ABI (Sourcify doesn't know chain 82330), so it falls back to a bytecode-estimated ABI. (Inferred: SIP-7 can't produce blocks in dev mode without a zebrad, §8.) |
| `0x…5A00` precompile | An empty address. |

A small fork of Otterscan could add a "Minted" row and a sealed/null
label to the block page. That's a maintenance cost we would carry
ourselves, and upstream wouldn't take it.

### 3.4 Is the `ots_` subset safe to expose publicly?

- **Tier A** is in the same cost class as the reads we already serve
  (`eth_getBlockByNumber`, `eth_getBlockReceipts`) [fact].
- **Tier B costs about what `eth_call` costs, and `eth_call` is already
  public.** On a 13.74 M-gas transaction (debug build, laptop):
  `eth_call` of the same work took 0.12 s, `ots_traceTransaction`
  0.15 s, `ots_getInternalOperations` 0.14 s, `ots_getTransactionError`
  0.13 s, and `ots_getContractCreator` 0.03 s [measured]. Otterscan's
  trace uses reth's parity config, which records call frames, not
  per-opcode steps, so the response is small (276 bytes) [fact,
  measured]. The worst case is replaying one full 30 M-gas block, about
  twice the largest `eth_call`, which the public RPC already lets anyone
  run at the reth gas cap (50 M).
- **Concurrency is already bounded.** reth runs at most
  `max(vCPU − 2, 2)` tracing requests at once, so 6 on a CX43
  (`default_max_tracing_requests`) [fact]. The Worker's per-IP limit and
  the Workers quota apply on top.
- **Keep out:** `debug_*` (arbitrary JS and opcode tracers, whose output
  size grows with the step count), `trace_filter`/`trace_block` (range
  scans), and `trace_replayTransaction`. `trace_replayTransaction` with
  `stateDiff` returned 114 KB for one heavy transaction [measured]; it's
  the only way to get the State Diff tab, and that tab isn't worth
  opening the `trace` namespace for.

Verdict: tiers A and B together are safe to add to the public allowlist.
Tier B must be enforced by method name, as the profile does today, never
by namespace.

### 3.5 Hosting, ops, cost

- **The Docker image is too big for Pages as it stands.** It holds 953,510
  files (3.7 GB), almost all of them the 4-byte signature DB (923,591
  files, 3.5 GB) and mainnet token logos (19,515 files). The app itself is
  153 files, about 4.1 MB [measured]. Pages allows 20,000 files per site on
  Free and 25 MiB per file [fact, Cloudflare docs]. So we'd ship a trimmed
  build: the app, one `chains/eip155-82330.json`, a `config.json`, and a
  small `signatures/` + `topic0/` set for the day-one contracts (WSOVA,
  the AMM, Multicall3, Ashwings). That's a few hundred files. R2 or
  "served from the RPC host" also work, but Pages is simpler and has no
  box to keep up.
- **RPC.** `config.json`'s `erigonURL` would point at the public RPC
  hostname (or an `explorer-rpc.` hostname with its own Worker route). The
  node must serve the `ots` subset, and the Worker must allow it and
  handle batches (§7).
- **Ops burden:** low. A static redeploy when we bump Otterscan, and a
  config edit at a testnet reset (the chain ID stays; nothing
  re-indexes).
- **Cost:** $0 on Pages Free [fact]. Explorer traffic counts toward the
  Workers request quota (100k/day on Free, then $5/month for 10M,
  infra-m1 §1).
- **Never load-bearing.** Anyone can run the same SPA against their own
  node: `docker run -p 5100:80 otterscan/otterscan`, or open the hosted
  page with its RPC pointed at `localhost` once the local profile can
  serve `ots` (§7). If our page disappears, nothing about the chain
  changes.

## 4. Option 2: Blockscout

Blockscout is an Elixir backend (indexer + API), a Next.js frontend and a
PostgreSQL database, plus optional microservices (contract verifier,
signature provider, stats).

### 4.1 What it needs from our node

- **An archive node on loopback.** Our reth runs `pruning_mode=archive`
  [measured, node log].
- **Internal transactions:** with the geth variant,
  `debug_traceBlockByNumber` / `debug_traceTransaction` with
  `callTracer`; with the Erigon/Nethermind variants,
  `trace_replayBlockTransactions` + `trace_block`. Pending transactions
  need `txpool_content` (optional). WebSocket `newHeads` is recommended,
  and it can poll without it [fact, Blockscout docs].
- **The indexer is heavy.** Blockscout asks for about 200 req/s while
  indexing and 100 req/s once caught up [fact]. That's four times the
  Worker's whole per-IP limit, so **the indexer can't use the public
  RPC**. It has to run next to its own node with `SOVA_RPC_PROFILE=local`
  plus `debug` (a code change, §7), on loopback.
- **Nothing extra becomes public at the RPC.** The debug namespace stays
  on the box. The public surface becomes Blockscout's own API and UI,
  which run Postgres queries per request. That's a new DoS surface we'd
  have to rate-limit (a Worker or WAF rule on `explorer.`).

### 4.2 How it shows Sova specifics

| Specific | Blockscout |
| --- | --- |
| Mints (withdrawals) | **Shown.** Blockscout indexes EIP-4895 withdrawals and has block- and address-level "Withdrawals" tabs (upstream PR #6694) [fact]. They're labeled with validator vocabulary ("Validator index 0"), and the frontend gates the tabs behind a beacon-chain setting [est, check the frontend ENVs]. So a burner can see "my mints" per address, unlike in Otterscan. |
| Address history | **Shown**, from its own index. |
| 97-byte extraData | Raw at most. No sealer label [est]. |
| Null blocks | Render as empty blocks with miner `0x0`. |
| ZcashBlocks `0x…5A01` | Contract page. We can verify its source ourselves (the verifier runs in our stack, no Sourcify listing needed), so Read Contract works: `latest()`, `summary(h)`. System-call writes still show no transactions. |
| Contract verification | **Yes.** This is Blockscout's real advantage for dapp builders. |

### 4.3 Ops and cost

- **Resources:** 4–8 vCPU, 8–32 GB RAM and 120–500 GB SSD for a full
  deployment; small test setups report 2 vCPU / 4 GB for the app plus a
  separate 2 vCPU / 4 GB Postgres [est, Blockscout docs and issue #5093].
  A young testnet sits at the bottom of that range.
- **Where it runs.** On `sova-rpc-1`, the CX43 (16 GB) already runs zebrad
  and a Sova node, so Blockscout would crowd the public RPC's origin. The
  clean shape is a **fifth Hetzner server**: its own zebrad, a Sova
  follow-only node with `debug` on loopback, Blockscout and Postgres, and
  a cloudflared tunnel to `explorer.`. That's about €16.49/month for a
  CX43 + IPv4, plus a 60–100 GB volume at €5.72 per 100 GB [est, infra-m1
  §1 prices, re-check]. Or ~€70 for CPX42-class disk.
- **Ops burden:** real, and ongoing. Postgres upkeep and disk alerts,
  Blockscout upgrades (frequent, with schema migrations), a re-index from
  zero at every testnet reset (SIP-8's reset is already on the plan), and
  one more public service to watch in the switch-off drill. It's also
  one more place that holds state our "rebuild, don't repair" rule has to
  cover.
- **Time to launch-ready:** days of integration and testing we don't have
  before October [est].

## 5. Option 3: no explorer at launch

### 5.1 What the guide and the site say

- **Guide** (`docs/guides/testnet.md`): drop `<<EXPLORER_URL>>` from the
  placeholder table (line 43) and from the wallet line (line 547: "explorer
  `<<EXPLORER_URL>>`" is deleted, so the line ends at the RPC). Add a
  short section "Look things up without an explorer" after §5 (recipes
  below). The existing "Which blocks paid you" recipe (§5, `jq '{miner,
  extraData, withdrawals}'`) is already the best mint view any option
  offers.
- **Wording**, in the plain style of the guide: *"There's no block
  explorer yet. Your node answers everything an explorer would, and the
  commands below cover the usual questions. A hosted explorer is planned;
  it will be a convenience, like our RPC."*
- **Site** (`site/src/data/sova.ts` footer entry, `site/README.md` "Still
  at launch"): the footer's testnet entry becomes "Testnet guide and RPC"
  with no explorer link. `/v/block`'s `TestnetStats` already plans live
  height, burns and SOVA minted from RPC. That's the site's
  "epochs and mints" view, and it doesn't need an explorer.
- **Runbook** (`docs/ops/testnet-launch.md` step 9): "Otterscan on Pages"
  stays in the post-launch list, now pointing to this note.

### 5.2 `cast` recipes (tested through `SOVA_RPC_PROFILE=public`)

Every recipe below ran against a dev node in the public profile, so it
uses only allowlisted methods [measured]. `RPC` is the public RPC or your
own `http://127.0.0.1:8545`.

```bash
RPC=<<RPC_URL>>

# Head, and one block's Sova fields: who sealed it, the seal, the mints
cast block-number --rpc-url $RPC
cast block 1234 --rpc-url $RPC --json | jq '{miner, extraData, withdrawals}'

# Is it sealed or null? 97 bytes = sealed, 0 = null
cast block 1234 --rpc-url $RPC --field extraData | awk '{print (length($0)-2)/2, "bytes"}'

# A mint amount is in gwei; in SOVA:
cast to-unit 6250000000000gwei ether          # 6250

# Your mints in one block
cast block 1234 --rpc-url $RPC --json \
  | jq --arg a "$(echo 0xYOURADDR | tr A-F a-f)" '.withdrawals[] | select(.address==$a)'

# A transaction, its receipt, and a full call trace with the revert reason.
# cast run replays it locally from state it reads over RPC, so it needs no
# trace methods. It can't replay calls into the SIP-4 precompile (0x…5A00),
# which exists only in a Sova node.
cast tx 0xTXHASH --rpc-url $RPC
cast receipt 0xTXHASH --rpc-url $RPC
cast run 0xTXHASH --rpc-url $RPC

# Balance, code, events (the public RPC caps a numeric range at 1,000 blocks)
cast balance 0xADDR --ether --rpc-url $RPC
cast code 0xADDR --rpc-url $RPC
cast logs --rpc-url $RPC --from-block 1000 --to-block 1999 --address 0xCONTRACT "Transfer(address indexed,address indexed,uint256)"

# The Zcash block Sova last recorded (ZcashBlocks, SIP-7)
cast call 0x0000000000000000000000000000000000005A01 "latest()(uint64,bytes32)" --rpc-url $RPC
```

In the prototype, `cast run` on the value-forwarding call printed the
full tree (`fwd{value: 0.5}` → `fallback{value: 0.5}` → `emit Ping`), and
on the reverting call it printed `[Revert] sova says no` [measured]. It
reads one account or slot per request, so a big transaction can hit the
public RPC's 50-per-10 s limit. Your own node has no limit.

What's missing without an explorer: an address's transaction history
(Otterscan lacks it too), and anything clickable to share.

- **Ops burden:** none.
- **Cost:** $0.
- **Risk:** none.
- **Demo cost:** there's no URL to paste into a tweet. `/v/block` and
  `/pulse` partly cover that.

## 6. Side by side

| | Otterscan (hosted) | Blockscout | None at launch |
| --- | --- | --- | --- |
| New node surface | `ots` tiers A+B on the public allowlist | `debug` on a loopback-only node | none |
| Edge changes | Allowlist + batch handling | New hostname + rate limit on Blockscout's API | none |
| Public DoS exposure | ≈ `eth_call` (bounded replay) | Postgres-backed API | none |
| New servers | none (Pages) | 1 (zebrad + sova + Blockscout + Postgres) | none |
| Cost / month | $0 (+ Workers quota) | ≈ €20+ [est] | $0 |
| Ops | Static redeploys | Upgrades, Postgres, re-index per reset | none |
| Mints visible | **No** (shows 0 reward) | **Yes** (withdrawals tabs) | Yes, via `cast block … withdrawals` |
| Address history | **No** (reth lacks the index) | Yes | No |
| Traces / revert reasons | Yes | Yes | Yes (`cast run`) |
| Contract verification | No (Sourcify doesn't list 82330) | Yes | No |
| Seal / null labels | No (raw hex) | No | Yes (the recipe above) |
| Works if we vanish | Yes: anyone serves the SPA against any node | Only if someone re-runs the stack | Yes |
| Ready for October | ~2–3 days of kit work [est] | No [est] | Yes |

## 7. Recommendation

**Launch without a hosted explorer.** The guide and the site should say
so plainly and hand users the `cast` recipes, which show mints, seals and
null blocks better than either explorer does out of the box. **Follow
with Otterscan on Pages** once the node and the edge serve a bounded
`ots` subset (tiers A and B, §3.4) and a sim has run it. It should launch
with a visible note that mints show as withdrawals, not as block rewards,
so it doesn't mislead. **Blockscout waits for real demand for contract
verification** (G1 dapp builders), since it's a stateful service to run
and to re-index at every reset.

If Rob wants an explorer URL on launch day anyway, the Otterscan
fast-follow below is the one to pull forward. It is about 2–3 days of kit
work [est], and it ships with the mint gap disclosed.

### 7.1 Launch-day kit changes (the "none" option)

| File | Change |
| --- | --- |
| `docs/guides/testnet.md` | Remove the `<<EXPLORER_URL>>` row (line 43) and the "explorer `<<EXPLORER_URL>>`" clause (line 547). Add "Look things up without an explorer" with the §5.2 recipes. |
| `site/src/data/sova.ts` + `site/README.md` | Footer testnet entry: "Testnet guide and RPC" (no explorer). Update the "Still at launch" row. |
| `docs/ops/testnet-launch.md` | Step 9's open list: "Otterscan on Pages (`docs/design/explorer-m1.md` §7.2)". |
| `docs/design/infra-m1.md` | D7: "None at launch; Otterscan fast-follow; Blockscout on demand". Fix §2's "users can point it at their own node" (true only after §7.2 item 2). |
| `docs/WORKPLAN.md` | Close item 12's explorer half. |

### 7.2 Otterscan fast-follow kit changes (not implemented)

| # | File | Change |
| --- | --- | --- |
| 1 | `bin/sova/src/rpc.rs` | Public profile: add `RethRpcModule::Ots` to `PUBLIC_HTTP_MODULES`. Add an `OTS_PUBLIC_METHODS` list (tiers A+B, including the `erigon_getHeaderByNumber` alias) to the allowlist. Tests: move `Ots` out of `DENIED`; allow the `ots_`/`erigon_getHeaderByNumber` prefix in `allowlist_is_read_and_broadcast_only`; assert that `ots_searchTransactions*`, `trace_*` and `debug_*` stay out. |
| 2 | `bin/sova/src/rpc.rs`, `main.rs` (docs) | Local profile: an opt-in `SOVA_RPC_OTS=1` that adds `ots` to reth's standard set, so anyone's own node can back Otterscan. With `SOVA_RPC_CORS` for the browser. |
| 3 | `infra/testnet/worker/rpc-firewall.mjs` | Add the same `ots` subset to `ALLOWED`. Batches: either raise `MAX_BATCH` to about 50 while counting each call against `RPC_RATELIMIT` (`limit()` once per call, not per request), or ship an Otterscan build with `batchMaxCount: 10`. The second keeps the Worker unchanged but means patching Otterscan. |
| 4 | `infra/testnet/worker/test.mjs`, `infra/testnet/smoke.sh` | Stop asserting `ots_getApiLevel` is denied. Assert that it answers `8`, and that `ots_searchTransactionsBefore`, `trace_replayTransaction` and `debug_traceTransaction` are denied. |
| 5 | `infra/testnet/explorer/` (new) | `build.sh` fetches a pinned Otterscan release (by digest or tag), strips `signatures/`, `topic0/`, `assets/{1,56,137}/` and `chains/*`, and adds `chains/eip155-82330.json`, `config.json` (`erigonURL` = the RPC host, `chainInfo` SOVA/18) and signatures for the day-one contracts from `deployments/sova-testnet.json`. |
| 6 | `infra/testnet/cloudflare.sh` | New `step_pages` (a Pages project plus the `explorer.testnet.sova.io` custom domain) and a teardown entry. |
| 7 | `infra/testnet/config.env.example` | `EXPLORER_HOST="explorer.testnet.sova.io"`, `OTTERSCAN_VERSION=` / digest. |
| 8 | `infra/testnet/bootnodes.sh` | `courtesy.explorer` in `seeds.json`. |
| 9 | `docs/ops/testnet-launch.md` | DNS table row for `explorer.`, a launch step, and a smoke check. |
| 10 | `docs/guides/testnet.md` | Put `<<EXPLORER_URL>>` back, with one sentence: "Mints appear as block withdrawals (amounts in gwei), which the explorer doesn't show; use the recipe in §5." |
| 11 | `box/sim/` | One scenario that brings up a mine-mode node with SIP-6 + SIP-7, points the trimmed Otterscan at it through the Worker (`wrangler dev`), and checks the page RPC log has no refusals. This is the run §8 couldn't do. |

### 7.3 If Blockscout later

A fifth server role `explorer` in `config.env.example` `SERVERS`, a
systemd unit set in `infra/testnet/host/systemd/` (Blockscout +
Postgres, docker compose), `SOVA_RPC_PROFILE=local` + `debug` on that
node only (a code change like §7.2 item 2), a tunnel in `cloudflare.sh`,
a WAF/Worker rate limit on `explorer.`, and a reset runbook entry
("drop the DB, re-index").

## 8. Prototype: how it was run, and what it couldn't show

- **Build.** The node has no switch that turns `ots` on (§2), so a
  temporary copy of `bin/sova/src/main.rs` was built as an example target
  (`cargo build -p sova --example sova_ots`, into the shared
  `anchorsim-target`). The copy had one added line: an env var that sets
  reth's `http_api`. It built in 44 s. The sim binary `debug/sova` was
  untouched, no sim suite was running, and the file was deleted
  afterwards. It was never committed.
- **Node.** Dev chain (chain ID 1337, reth's interval miner), with
  `SOVA_DATADIR` under the job's temp dir, HTTP 18845, auth 18851, P2P
  31503, and `http_api = eth,net,web3,ots` (later `+trace` for the State
  Diff test). It was stopped with SIGTERM.
- **Otterscan.** `otterscan/otterscan:latest` on 127.0.0.1:5100 with a
  mounted `config.json`, rendered in headless Chrome over CDP; the page
  text and console errors were captured. A Python proxy on :5101 logged
  every method with ok/error and could refuse methods the way the Worker
  does. Tier A alone was also tried: the tx overview and trace tab lose
  internal transfers, revert reasons and the call tree, and the contract
  creator disappears. The pages degrade without crashing. The container
  and the image were removed afterwards.
- **Synthetic data.** Dev blocks have no withdrawals and no seal. For
  §3.3's rendering checks, the proxy rewrote one block's header to carry
  a 97-byte `extraData` and two mint withdrawals (6,000 and 250 SOVA in
  gwei), and another to the null-block shape. These show **rendering
  only**, not real consensus data.
- **What it could not show.**
  - A mine-mode run (a regtest zebrad via `box/up.sh` with SIP-6 + SIP-7
    on custom ports) was blocked by this session's permissions.
  - Dev mode with `SOVA_SIP7=1` and no zebrad can't build blocks ("no
    indexed record for zcash block 1") [measured], so the ZcashBlocks
    page, real seals and real null blocks were not rendered.
  - The Worker's batch cap against Otterscan's batching was not tested
    (§3.2).
  - Timings are from an unoptimized debug build on a laptop, not a CX43.
- **`cast` recipes.** A dev node with `SOVA_RPC_PROFILE=public` and the
  same binary: "public (HTTP serves 23 allowlisted method(s), removed
  54)". `cast run`, `cast logs`, `cast block --json` and `cast tx` all
  worked [measured].

## 9. Open questions for Rob

1. **An explorer URL on launch day, or not?** Recommended: not. The
   fast-follow is about 2–3 days of kit work.
2. **Is Otterscan's "0 SOVA block reward" acceptable** with a disclaimer,
   or does the mint have to be visible before we host any explorer? If it
   must be visible, the choice is a small Otterscan fork (a "Minted" row)
   or Blockscout.
3. **Batches:** a larger Worker batch cap that counts each call, or a
   patched Otterscan build with `batchMaxCount: 10`?
4. **Hostname:** `explorer.testnet.sova.io` on Pages, with the RPC at
   `rpc.testnet.sova.io`? Or a separate `explorer-rpc.` Worker route with
   its own, tighter limit (infra-m1 §2 suggested the latter)?
5. **Upstream:** open issues for reth's `ots_searchTransactions*` gap and
   the `ots_hasCode` integer-param mismatch? These are code-level bug
   reports, not partner outreach, but it's Rob's call under the outreach
   rule.

## Sources (accessed 2026-09-24)

- reth v2.6.0 Otterscan API: `crates/rpc/rpc/src/otterscan.rs`,
  `crates/rpc/rpc-api/src/otterscan.rs`; limits in
  `crates/rpc/rpc-server-types/src/constants.rs` (local cargo checkout of
  the pinned tag).
- Cloudflare Pages limits: https://developers.cloudflare.com/pages/platform/limits/
- Blockscout JSON-RPC requirements: https://docs.blockscout.com/setup/requirements/node-tracing-json-rpc-requirements
- Blockscout hardware: https://docs.blockscout.com/for-developers/information-and-settings/requirements
  and https://github.com/blockscout/blockscout/issues/5093
- Blockscout EIP-4895 withdrawals: https://github.com/blockscout/blockscout/pull/6694
- Otterscan image `otterscan/otterscan:latest`, digest
  `sha256:7636f835fcdfc550c205a78876013d6e54c95846f3566e59cc71bc6136c80cc9`
  (bundle inspected for RPC methods, `batchMaxCount` and routes).
