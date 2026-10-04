# NEAR as Sova's data-availability archive

Status: built and tested end to end against NEAR testnet (worker, 2026-10-04).
Phase 1 = **archive**: posting is not a consensus rule, and a NEAR outage
never stops Sova. Code: `tools/near-da/` (format, poster, fetch/verify, index
contract). Test: `tools/near-da/e2e/box-e2e.sh`. Decision (Rob, 2026-10-03):
"Zcash secures it, NEAR keeps its history."

## 1. Summary

- Every finalized Sova block goes to NEAR in **SOVADA1 batches** (§4). Each
  batch is the raw argument of one NEAR function call, `post`, on Sova's
  **index contract** (§5).
- The contract checks every batch before it accepts it:
  - the batch comes from the owner, for this chain;
  - it starts exactly where the archive ends;
  - every block's hash is keccak-256 of its header;
  - every header's number is its height;
  - every block's parent is the block before it, across batches too.

  It then records an index entry: height range, sha256, last block hash, the
  NEAR block, and the carrying NEAR transaction.
- The archive's address is one NEAR account. With nothing but that name and
  any NEAR RPC, a stranger lists every batch (`batches` view) and downloads
  each one by transaction hash. Without the hash, they scan the few NEAR
  blocks the entry names. They check it against the index, then verify every
  block with their own Sova node and zebrad. Our servers are not involved.
- **Only blocks at least `FINALIZED_DEPTH` (300) deep are posted** (§7.2).
  The archive is append-only and never holds a block that a Zcash reorg could
  replace. The cost is that the archive trails the head by about 2 h 5 min,
  plus up to one batch interval.
- Cost (§8):
  - Testnet: free (faucet NEAR).
  - Mainnet at today's block sizes: about 0.03 NEAR/day of gas (~$0.15/day
    at $4.81) and about 0.05 NEAR/day of storage stake.
  - Heavy future use (10 KB blocks): about 0.15 NEAR/day.
- **What NEAR guarantees, and what it doesn't** (§3):
  - Guaranteed: NEAR consensus includes each batch, and the index contract's
    state persists on mainnet.
  - Not guaranteed: keeping old transaction bodies retrievable. That is done
    by archival operators (FastNEAR, the near.org archival RPC,
    neardata.xyz) on a best-effort basis.

  The public copy in §11 says exactly that.

## 2. Options considered (research, 2026-10-04 UTC)

Sources were checked between 02:55 and 03:10 UTC on 2026-10-04. They are
primary repos and docs plus live RPC calls.

### (a) NEAR DA (Nuffle Labs blob store + `near-da` clients)

- **Status: dormant.**
  - Repo: https://github.com/Nuffle-Labs/data-availability. The old
    `near/rollup-data-availability` URL redirects there.
  - Last commit on `main`: 2024-12-06. Last release: v0.4.0 (2024-05-14).
    Only unmerged dependabot PRs since (the latest 2026-04-23).
  - NFFL (https://github.com/Nuffle-Labs/nffl) was last committed
    2024-12-05.
  - Secondary press (not verified at a primary source) says Nuffle rebranded
    to MoreMarkets in March 2025.
- **Storage.** The blob-store contract's `submit()`
  (`contracts/blob-store/src/lib.rs`) checks the owner and reads the input,
  then stores nothing. The blob lives only in the transaction's
  function-call arguments. That is exactly what option (b) does.
- **Reference.** In current code a blob reference is the 32-byte NEAR tx hash
  (`BlobRef { transaction_id }`). The docs page
  (https://docs.near.org/chain-abstraction/data-availability) still describes
  the legacy 64-byte `tx_id ‖ commitment`.
- **Clients.**
  - Rust: `near-da-rpc`, never published to crates.io. It pins near-* 0.21
    and needs a signer key even to read.
  - Also an HTTP sidecar, a Go package and an FFI crate. No JS client.
  - Reads hard-code `rpc.*` / `archival-rpc.*.near.org`, which are now
    heavily rate-limited (§3).
- **Verdict.** It is the same storage mechanism as (b), but with no index:
  you must already hold the tx hash. Its clients are unmaintained and pinned
  to deprecated endpoints. We copy its approach, not its code.

### (b) Plain function calls to our own contract (chosen)

Limits come from the live `EXPERIMENTAL_protocol_config`: mainnet protocol 86
(nearcore 2.13.4) and testnet protocol 87 (2.14.0-rc.2). They are identical
on both networks.

| Limit | Value | Note |
|---|---|---|
| `max_transaction_size` | 1,572,864 B (1.5 MiB) | The binding limit. The poster caps batches at 1,000,000 B. |
| `max_arguments_length` | 4 MiB | |
| `max_receipt_size` | 4 MiB | |
| `max_total_prepaid_gas` | 1 PGas | |
| `transaction_validity_period` | 86,400 blocks | |
| Gas price | 1e8 yoctoNEAR/gas (0.0001 NEAR per Tgas) | Both networks. |

- **Gas purchase.** Since protocol 85 (NEP-642, nearcore #15907), prepaid gas
  is bought at `min_gas_purchase_price` = 1e9, and the difference is refunded
  after. Keep about 10× the attached gas in balance.
- **Argument bytes cost** (`function_call_cost_per_byte`):
  - 4.47 Mgas/byte when signer == receiver.
  - 49.9 Mgas/byte when signer ≠ receiver.

  So the **contract lives on the posting account** (§5.3).
- **Measured by us on NEAR testnet** (§9): a `post` burns ≈ 2.0 Tgas
  + 0.033 Tgas/block + 35 Mgas/byte. That includes the contract reading the
  input, sha256 over it and keccak over every header.
  - 10.8 KB / 16 blocks: 2.92 Tgas.
  - 814 KB / 16 blocks: 31.2 Tgas.
- **Storage stake.** `storage_amount_per_byte` = 1e19 yocto (100 KB locks
  1 NEAR), plus 40 B overhead per record. That applies only to our index
  entries, not to the batch bytes.
- **Libraries.**
  - Rust:
    - `near-api` 0.8.6 (async; pulls openssl, secp256k1, reqwest).
    - `near-jsonrpc-client` 0.22 (pulls `near-primitives`, `near-crypto`
      0.37, which pins `aws-lc-rs = 1.16.2` exactly).
  - JS: `near-api-js` 7.3.1.
  - CLI: `near-cli-rs` 0.30.1 (we used 0.23.5).

### (c) Anything newer

- No data-availability NEP exists (the newest NEPs go to the 645–657 range:
  gas keys, post-quantum signatures, universal accounts).
- There is no new DA product page.
- Stateless validation, 600 ms blocks and 10 shards don't change how
  transaction data is kept.
- **NEAR Lake (S3) is deprecated.** It has received no new blocks since
  2026-03-24 (docs.near.org, commit 2026-05-07). Its replacements are
  neardata.xyz, Goldsky, the Data APIs, or running your own indexer.

## 3. Retention: what is guaranteed and what is best-effort

| | Guarantee | Source |
|---|---|---|
| A batch is included and final (≈1.2 s on mainnet) | **Protocol** | consensus |
| The index contract's state (ranges, sha256, tx hashes) stays readable | **Protocol, mainnet**: "mainnet is the only network where state is guaranteed to persist" | https://docs.near.org/protocol/network/networks |
| Regular RPC nodes keep tx bodies | **Short.** "Nodes garbage collect blocks after 5 epochs (~1.5 days) unless they are archival nodes." Epoch = 43,200 blocks ≈ 7.2 h. We observed ~20 h on mainnet RPCs and ~34 h on testnet. | https://docs.near.org/protocol/network/epoch; nearcore `client_config.rs` (default 5, min 3) |
| Archival RPC keeps every tx body | **Best-effort, operator-run.** `archival-rpc.{mainnet,testnet}.near.org` are "severely rate limited" (deprecated; from 2025-08-01: 4 req/min on mainnet archival, 20/min on testnet). `archival-rpc.*.fastnear.com` is "paid only" in the docs but answered without a key on 2026-10-04. | https://docs.near.org/api/rpc/providers (2026-09-18); https://docs.fastnear.com/rpc (2026-09-24) |
| neardata.xyz serves every block (with tx args) from genesis | **Best-effort** (FastNEAR, free, no key today) | live calls |
| A light-client proof of a tx's outcome | **Protocol.** `light_client_proof` returns an outcome proof, an outcome-root proof and a block proof. The tx hash is sha256 of the Borsh tx, so it commits to the arguments. It worked for 2024 blobs on both archival endpoints. | live calls |

- **Empirical check.** Two NEAR DA blobs from January and April 2024 on
  testnet are still fully readable from both archival RPCs and from
  `testnet.neardata.xyz`. Regular RPCs answer `UNKNOWN_TRANSACTION` for
  them.
- **Bottom line.** NEAR gives no protocol promise that history is retained.
  An archive on NEAR is as durable as NEAR's archival ecosystem. There are
  several independent operators today, and anyone can run an archival node
  (it is open source) and replay from genesis.
- Our mitigation is cheap: run, or pay for, one archival source of our own
  (an operational to-do, not done yet). `fetch` already takes any number of
  sources.

## 4. Batch format v1 (`SOVADA1`)

This format is shared with the rebuild tool. It is implemented in
`tools/near-da/format` (crate `sovada`, no dependencies, also compiled into
the contract).

```
file   := magic "SOVADA1\0" (8) | chain_id u64 LE | first_height u64 LE | count u32 LE | block*count
block  := height u64 LE | hash [32] | len u32 LE | raw [len]
```

- `raw` is the block exactly as `debug_getRawBlock` returns it: the RLP list
  `[header, transactions, ommers, withdrawals, …]`.
- `hash` is the Sova block hash, keccak-256 of the header RLP (the list's
  first item).
- Little-endian integers, no padding, nothing after the last block.
- **Rules.** Readers must refuse a batch that breaks any of them.
  - `count` is between 1 and 10,000.
  - Heights are `first_height, first_height+1, …`.
  - The bytes end exactly after the last block.
- **Across the archive:**
  - each batch starts at the previous batch's last height + 1;
  - every block's `parentHash` is the previous block's hash;
  - the genesis block's parent is all zeros.
- **Compression.** The magic `SOVADA1Z` is reserved (zstd of the body) and is
  neither produced nor accepted. At today's sizes it isn't worth it: §8 puts
  a year of testnet gas at about 12 NEAR. Uncompressed bytes also let the
  contract check every block. Any future format change gets a new magic.
- **On disk**, `fetch` writes one file per batch, holding the exact posted
  bytes:
  - `NNNNNNNNNNNN-MMMMMMMMMMMM.sovada`: first and last height, 12-digit
    zero-padded, so a lexical sort is height order;
  - plus `index.json` (the contract info and every entry).

## 5. The index contract

It lives in `tools/near-da/contract` (near-sdk 5.29, `cargo near build`,
146 KB wasm).

### 5.1 State and methods

```
new(owner, chain_id, start_height)        init
post()                                    raw input = one SOVADA1 batch; owner only
set_tx(index, tx_hash)                    owner only, once per batch (base58)
set_owner(owner)                          only the contract account itself
info() -> {format, owner, chain_id, start_height, next_height, batch_count,
           last_hash, bytes_posted}
batches(from_index, limit<=100) -> [{index, first_height, last_height, count,
           bytes, sha256, last_hash, near_block, tx_hash?}]
find(height) -> batch containing height (binary search)
```

`post` checks, in order:

1. The caller is the owner.
2. The input parses (`sovada::parse`).
3. `chain_id` matches.
4. `first_height == next_height`.
5. `verify_blocks`: hash = keccak(header), number = height, parent linkage,
   the first block linked to the stored `last_hash` (or to zero at genesis).

Then it pushes the entry and logs an NEP-297 event
(`EVENT_JSON:{"standard":"sovada","event":"batch",...}`).

A wrong, repeated, gapped or forked batch fails, so **the index cannot
contain a double-posted range, a gap, or a block that doesn't link**. That
holds whatever the poster does: crash, restart, retry, two posters at once.

### 5.2 How a stranger finds every batch (no Sova servers)

1. Call `info` and `batches` on the archive account (`sova-da.testnet` for
   the public testnet) through any NEAR RPC, at `finality: final`.
2. For each entry:
   - call `tx(tx_hash, sender = owner)` on an archival RPC, and take the
     `post` action's base64 arguments;
   - if `tx_hash` is missing or every lookup fails, scan NEAR blocks
     `near_block-8 ..= near_block` chunk by chunk (RPC `block` + `chunk`, or
     neardata.xyz `/v0/block/<h>`) for a `post` to the contract whose
     argument sha256 matches.

   With signer == receiver, the tx and its receipt land in the same block;
   we saw `near_block` = the tx's block every time.
3. Check sha256 against the index, then the format, chain, range, parent
   linkage and the entry's `last_hash`.
4. Verify each block with a Sova node that has its own zebrad: import them
   (the rebuild tool does). The contract proves only that the headers form
   one hash-linked chain. Seals, burns and mints are Sova consensus.

`sova-near-da fetch` does steps 1–3. Run it with `--scan-only`
(`--scan-via rpc|block-api`) to prove the index alone is enough.

### 5.3 Accounts and keys

- The contract is deployed on the **posting account itself**. Signer ==
  receiver makes argument bytes cost 4.47 instead of 49.9 Mgas/byte. Owner =
  that account.
- The poster runs with a **function-call access key** on that account:
  methods `post` and `set_tx`, receiver = the account, unlimited allowance
  (an allowance must cover 10× the prepaid gas, §2).
- The **full-access key stays offline**. A leaked poster key can append junk
  that links (fake headers), and that stalls the real archive. It cannot
  rewrite history or redeploy. A leaked full-access key can redeploy the
  contract and rewrite the index state.

  The posted bytes themselves are immutable NEAR history whatever happens,
  and readers verify blocks against Sova consensus. For the consensus SIP,
  lock the account (delete its full-access keys) or move ownership to a
  multisig.

## 6. The tool: `tools/near-da` (Rust)

It is Rust, in its own workspace (own `Cargo.lock`, like `tools/scan-bench`),
so near-sdk and the NEAR client never touch reth's dependency graph.

**NEAR client.** It is hand-rolled (`cli/src/near.rs`, about 300 lines). The
poster needs `query` (view call, view access key), `send_tx`, `tx`, `block`
and `chunk`, plus one Borsh layout NEAR keeps stable: `Transaction` V0 with
one `FunctionCall`.
- The official Rust clients pull NEAR's primitives stack, an async runtime
  and OpenSSL or exact-pinned aws-lc into a small blocking tool (§2).
- Correctness is checked three ways:
  - a byte-for-byte layout vector plus signature verification in unit
    tests;
  - testnet validators accepting every signature;
  - recovery from a real `send_tx` timeout seen during the e2e (§9).
- Node was the alternative. It would match `checkout-relayer`, but it would
  duplicate the format parser that the contract (Rust) and the rebuild tool
  already share.

`sova-near-da` subcommands:

| Command | What |
|---|---|
| `poster` | Follow a Sova node and post finalized blocks (§7). |
| `fetch --out DIR` | Download and check the whole archive into a `.sovada` set. `--sova-rpc` also byte-compares with a node. |
| `verify DIR` | Offline check of a `.sovada` set: contiguity, chain, hash links. `--sova-rpc` byte-compares with a node. |
| `info [--batches]` | Contract state as JSON. |
| `keygen` | New ed25519 key file (0600). Prints only the public key. |

**Node side: raw block bytes.** The poster picks its source at startup,
in this order, and logs which one it uses (`--raw-source
auto|debug|raw-tx|full-tx`, env `SOVA_DA_RAW_SOURCE`):

| Source | Needs | Used on |
|---|---|---|
| `debug` | `debug_getRawBlock`: `SOVA_RPC_DEBUG=1` on the `local` profile (the rebuild worker's commit, cherry-picked here; refused with `public`) | nodes from the next release, if the operator turns it on |
| `raw-tx` | `eth_getBlockByNumber(n, false)` + `eth_getRawTransactionByBlockHashAndIndex`: the `local` profile (any version) | the keeper (v0.1.17) |
| `full-tx` | `eth_getBlockByNumber(n, true)` only. That is in `PUBLIC_RPC_METHODS` (`bin/sova/src/rpc.rs`), which has no raw-transaction method | **the seeds and the RPC host (v0.1.17, `public` profile)** |

The two rebuilds work as follows.

- **Header.** It comes from the block JSON, deserialized into alloy's
  `Header` at the node's own alloy version (2.4.2, pinned) and RLP-encoded.
  It must hash to the node's block hash.
- **Transactions, `raw-tx`.** Each transaction's EIP-2718 bytes are used
  as-is.
- **Transactions, `full-tx`.** Each JSON transaction object is parsed into
  alloy's `TxEnvelope` and re-encoded with `encoded_2718`. It covers every
  type in reth's `EthPrimitives`, which Sova uses: legacy, 2930, 1559, 4844
  and 7702. Each re-encoding must hash to the object's own `hash`.
- **Body.** Legacy txs go in as RLP lists, typed txs as RLP strings. Then
  come the empty ommers list and the withdrawals from the block JSON.
- **Checks.** The block is refused unless the body matches
  `transactionsRoot`, `withdrawalsRoot` and the empty ommers hash, and the
  header matches the block hash.
  - So a rebuild that isn't byte-exact can't be posted. The poster retries
    and logs why.
  - A future tx type alloy 2.4.2 doesn't know fails the same way. The
    poster then stalls on that block. It never archives wrong bytes.
- **Origin.** The `raw-tx` path is ported from `sova-rebuild export`
  (branch `rebuild`); the root checks and the `full-tx` path are new.
- **Comparing sources.** `sova-near-da check-sources` compares both rebuilds
  with `debug_getRawBlock`, block by block, on a node that serves debug.

## 7. Posting

### 7.1 Cadence

- A batch goes out when **120 finalized blocks** are waiting (50 min at 25 s
  blocks), or when **an hour** has passed since the last post with anything
  waiting.
- A batch never exceeds 1,000,000 bytes (NEAR's limit is 1.5 MiB). It always
  holds at least one block, and a single block can't exceed Sova's own size
  limits.
- **Bounded latency:** a block reaches NEAR at most about FINALIZED_DEPTH ×
  25 s + 50 min ≈ **2 h 55 min** after it is sealed, while NEAR and the
  poster are up.
- **Catch-up after downtime:** full batches back to back, about 10 s each
  (post + `set_tx`, both waited to `FINAL`).
- **First run on the live testnet:** ~66k blocks at ~700 B is ~46 MB. That
  is about 550 batches of 120 blocks (~1.5 h), or run
  `--max-blocks 1000 --once` first (~70 batches, ~12 min).

### 7.2 Reorgs: post only finalized blocks

The rebuild worker asked for this decision. Sova blocks are re-sealed when
Zcash reorganizes, block for block (SIP-4 §7), up to `FINALIZED_DEPTH` = 300
(`crates/engine/src/candidates.rs`).

**Decision: the poster posts only blocks at or below the node's `finalized`
block. It never posts above it, and nothing is ever superseded.**

- **Never wrong.** reth refuses any head below the finalized block it was
  given ("too deep reorg"; the comment on `FINALIZED_DEPTH`). A finalized
  block can't be re-sealed without the node itself wedging. So the archive
  never holds a block that later stops being canonical. Readers (the rebuild
  tool, strangers) need no conflict rules: one height, one block, forever.
- **Simple to enforce.** The contract only appends, and it checks parent
  linkage, so it refuses a fork. "Supersede from H" would need: a contract
  that unwinds entries, readers that resolve the latest view, and a public
  claim ("this is Sova's history") that is true only as of some time. It
  also forces the rebuild tool to handle conflicts. All of that buys ~2 h of
  latency for an archive. Sova's P2P sync already serves the unfinalized
  tip.
- **Defence in depth.**
  - Before every batch, the poster checks that the node's block at
    `next_height − 1` has the contract's `last_hash`. If not, it **halts**:
    it logs `HALTED`, sets status `halted` and posts nothing.
  - It re-checks the last block's hash after reading the batch.
  - It verifies the batch exactly as the contract will before signing.

  A reorg deeper than 300 would already be a network-wide incident. Recovery
  is a human decision (for example, a fresh contract from a new start
  height), not an automatic rewrite.

The consensus SIP would need unfinalized blocks on NEAR, and so supersede
semantics (§10). That is a later, separate design.

### 7.3 Failure handling

| Event | Behaviour |
|---|---|
| NEAR RPC down or slow | `send_tx` error or timeout. The poster polls `tx(hash)` for 60 s; the hash is journaled before broadcast. If still unknown, it retries with a fresh read of `next_height`. If the first tx lands late, the contract refuses the duplicate (`batch starts at X, expected Y`). The data is posted twice, but the index is unaffected. Seen once in the e2e: the 814 KB post timed out on FastNEAR and was found by polling. |
| Poster restart or crash, state file lost | Harmless. The contract's `next_height` is the source of truth. A missing `tx_hash` is filled from the journal, or by scanning `near_block`. Tested: the e2e's second run deletes the state file. |
| Sova node behind or restarting | Transient: retry every poll. |
| Archive and node disagree at `next_height − 1` | **Halt** (above). |
| Wrong contract, chain or key | **Halt** with the reason (owner, chain id, `UNKNOWN_ACCESS_KEY`). |
| NEAR down for days | Sova is unaffected (phase 1). The archive catches up when NEAR returns. Status `lag_blocks` grows, which is what to alert on. |
| Poster key out of NEAR | `send_tx` fails ("not enough balance"). Retried; status `retrying` with the error. |

### 7.4 Status and logs

- One line per batch:

  ```
  2026-10-04T03:56:15Z posted batch #6 heights 96..=111 (16 blocks, 814158 bytes) tx HkzmxJzYrGDnuHDoYGSo3Dgf7EMp8jNHDGPYKr5fBftt gas 31.23 Tgas
  ```

  then `set_tx #6 gas 2.02 Tgas`.
- `--status-file` (JSON, written atomically every poll) holds:
  - `state`: `ok` / `retrying` / `halted`;
  - `sova_head`, `sova_target` (finalized), `archived_through`, `lag_blocks`,
    `batch_count`, `bytes_posted`, `last_batch` (with tx hash and gas),
    `last_error`.
- Suggested alert: `state != "ok"` for 15 min, or `lag_blocks > 300` (more
  than two batch intervals behind finality).

## 8. Costs

Model (measured on testnet, §9): gas per `post` ≈ 2.0 Tgas + 0.033 Tgas per
block + 35 Mgas per byte. `set_tx` adds 2.02 Tgas. 1 Tgas = 0.0001 NEAR,
and NEAR = $4.81 (CoinGecko, 2026-10-04). Index storage is ~166 B per batch
(121 B entry, plus key, plus 40 B overhead), so ~0.0017 NEAR locked per
batch.

| Scenario | Data/day | Batches/day | Gas/day | Storage stake/day |
|---|---|---|---|---|
| **Testnet today**: 3,456 blocks × ~700 B (sampled `size` of blocks 65212–66212) | 2.4 MB | 29 (120 blocks) | 29 × (8.9 + 2) ≈ 0.32 Pgas = **0.032 NEAR** | 0.048 NEAR |
| Mainnet, light use (2 KB blocks) | 6.9 MB | 29 | ≈ 0.05 NEAR (~$0.23) | 0.048 NEAR |
| Mainnet, busy (10 KB blocks) | 35 MB | 35 (1 MB cap) | ≈ 0.15 NEAR (~$0.70) | 0.06 NEAR |
| Mainnet, heavy (50 KB blocks) | 173 MB | 173 (20 blocks) | ≈ 0.69 NEAR (~$3.30) | 0.29 NEAR |

- **Testnet.** Everything is free, but the balance has to be kept up.
  Contract code locks 1.46 NEAR, and gas plus stake run about 0.08 NEAR/day
  at today's sizes. The faucet account's 10 NEAR therefore lasts roughly
  three months after deploy. Top up from the faucet (near-faucet.io gives
  2 NEAR per request, rate-limited) and alert on the account's balance.
- **Mainnet.** At today's block sizes it is under $100/year of gas, plus
  ~17 NEAR/year of storage stake. The stake stays locked but is not spent.
- If heavy use made the stake matter, there are two options:
  - drop `last_hash` and `sha256` from entries. Readers can re-derive them,
    at the cost of the contract's per-batch commitments;
  - fold entries into one rolling commitment.

## 9. Evidence (NEAR testnet, 2026-10-04)

| Item | Value |
|---|---|
| Archive account | `sova-da.testnet`, created with the faucet (tx `7zqSjViPuy7CyHnxrZZzaSo9iSq5qXdGWbuGSR42RHEr`). Key in `~/.config/sova-near-da/` (0600, outside the repo). |
| e2e (final run) | Box regtest chain 1337, binaries built from this branch, contract `e2e-20261004035415.sova-da.testnet` (create `H3GMhdshxXptHgTm96NB23K9TEvTKhn97h6geVeeLg9Q`, deploy + init `7dA5SiYAmJNX62bGHvAgN27qZ5HgMd5LNQTU4MwaxDq8`). Since deleted to recover its NEAR; the transactions stay in NEAR history. The kept e2e contract is the `public` run's, below. |
| What it archived | 14 batches, heights 0–191: 20 transfers, plus 8 txs × 100 KB random calldata. First batch `3teCM3D8C2offRaKnu9NftpaA55rniBJqs2mCnxXSXdr` (2.93 Tgas). Largest batch #6 is 814,158 bytes, tx `HkzmxJzYrGDnuHDoYGSo3Dgf7EMp8jNHDGPYKr5fBftt`, 31.23 Tgas with 100 Tgas attached. Each `set_tx` costs 2.02 Tgas. Index contiguous, every entry has its `tx_hash`. |
| Restart | Second poster run with the state file deleted resumed from the contract (batches #8–#11). The third run posted only new blocks (#12–#13). |
| Fetch | **192 blocks byte-identical** to the node's `debug_getRawBlock`, checked by the tool and by an independent Python reader. Same files fetched via the archival RPC only, via a `--scan-only` RPC block/chunk scan, and via a `--scan-only` neardata.xyz scan. Run 3 (contract since deleted) gave the same results for 195 blocks. |
| All three raw-block sources (box e2e, `NEAR_DA_E2E_NODE_MODE=public`, `debug`, `local`; 2026-10-04) | Each run loaded legacy, EIP-2930, EIP-1559 and EIP-7702 txs, plus 8 × 100 KB calldata. Every block also carries the miner's mint withdrawals (71–74 per run). **`public`** (v0.1.17 seeds): the poster picked `full-tx`; 12 batches, 177 blocks; contract `e2e-20261004045632.sova-da.testnet`, kept (create `5qWb6njWqSxE5CXw4LTPaJSDpYsxpCusVNjGnxW7SFG5`). **`local`** (keeper): it picked `raw-tx`; 12 batches, 175 blocks. **`debug`**: it picked `debug_getRawBlock`; 12 batches, 178 blocks, and `check-sources` found blocks 0–109 (873 KB, 30 txs of every type above) rebuilt by both `full-tx` and `raw-tx` byte-identical to `debug_getRawBlock`. In `public` and `local` mode the node was then restarted with `SOVA_RPC_DEBUG=1` on the same datadir, and every archived block was byte-identical to `debug_getRawBlock`, checked by the tool and by the independent reader. EIP-4844 couldn't be sent: the box node is post-Osaka and wants an EIP-7594 sidecar that cast 1.3.5 can't build. A unit test covers type 3 (and 0, 1, 2, 4): RPC JSON → re-encode → exact EIP-2718 bytes. Logs: `/Volumes/Extreme Pro/sova/near-da-e2e-r7-{public,debug,local}.log`. |
| Timeout recovery | Observed live in runs 3 and 4: the 814 KB `send_tx` timed out on FastNEAR after 30 s, polling found it final, then `set_tx` ran. |

Unit tests (`cargo test` in `tools/near-da`) cover:
- **format:** layout, round trip, truncation, trailing bytes, magic,
  compressed magic, count bounds, gaps, height overflow, wrong hash, wrong
  number, fork mid-batch, link to the previous batch, RLP edge cases;
- **contract:** contiguous posts, `find`, `set_tx` once, double post, gap,
  fork across batches, stranger, wrong chain, wrong hash, genesis parent,
  owner change;
- **client:** the Borsh vector, key parsing, outcomes, both tx JSON shapes.

### 9.1 The live testnet rebuilt from NEAR (2026-10-04)

The orchestrator rebuilt the whole public testnet from the NEAR archive
alone, on a laptop, with the strict path: every block through the node's
own Engine API and consensus, no peers.

| Item | Value |
|---|---|
| Archive | `sova-near-da fetch` from `sova-da.testnet`: 68 batches, heights 0–66,239 (44 MB, 40 s), `verify` ok; last hash = the public RPC's block 66,239. |
| Node | A fresh node (own datadir, relay transport with no peers, `SOVA_DISCOVERY=off`, `net_peerCount` 0 in every sample) and its own zebrad 7.0.0-rc.0 on Zcash testnet. |
| Tool | `sova-rebuild` (§6's batch format) pushing each block as `engine_newPayloadV4` to the node's authrpc and waiting for the node's own arbiter to adopt it. |
| Builds | #1–#47,666 on **v0.1.18**; #47,667–#66,239 on a build with the record-anchor fix (branch `record-anchor`, `5ff3760`). |
| Result | **Head #66,239, hash `0x79166f6b…`, stateRoot `0x2e76ff01…`, equal to the public RPC** (`rpc-testnet.sova.io`). Every block checked by the node: SIP-6 seal, settlement against its own zebrad, SIP-4 anchor, execution, state root. |
| Speed | About 180–230 blocks/s while the node's Zcash scan was ahead. When the rebuild outran the scan, the node held blocks (SIP-4 "hold, don't accept", e.g. 609 holds in the first run, 8 in the second) and the tool waited them out; nothing was skipped. |

**Replaying #47,667 needs v0.1.19 or later.** On 2026-10-02 the keeper
sealed #47,667 anchored to Zcash 4,436,166's canonical block while its
ZcashBlocks record carried the hash of the sibling a Zcash reorg had
replaced (`0x000004623f…`). The header's state root, and every header up
to #55,857, commit to that record. Nodes that pipeline-synced past it only
check the final state root, so they never noticed; a strict
block-by-block replay with v0.1.18 stops there with a state-root mismatch.
The fix (`crates/evm/src/blocks.rs` on `record-anchor`) does two things:

- `check_record_anchor`: from now on a block whose indexed Zcash hash
  differs from its anchor is refused (retried, never cached invalid), so a
  sealer can't build on a stale index again;
- `RECORD_EXCEPTIONS`: a testnet-only historical exception keyed by chain
  ID, height and parent hash (the pattern of Bitcoin's BIP30 exceptions)
  that replays exactly what #47,667 recorded. No rule changes for any other
  block.

So: a stranger rebuilding the testnet from NEAR uses a release that has
this fix (v0.1.19+). With v0.1.18 the rebuild stops at #47,667.

Tool fixes found by this run (branch `rebuild-2`):

- `--expect-rpc` takes `https://` URLs (ureq's rustls backend, already in
  the node's dependency graph). Before, it failed with "Unknown Scheme".
- One keep-alive connection per endpoint instead of one per request.
- A resume no longer asks the node about every present block, which had
  exhausted macOS's ephemeral ports (os error 49) re-checking tens of
  thousands of them. The archive is hash-linked: the tool recomputes each
  block's hash from its header and checks each parent link. A block hash
  commits to its parent's hash, so a node whose block at height H has the
  archive's hash holds the archive's whole chain below H. The tool looks
  up the first present block, every 10,000th, and the last one (the
  node's head, or the archive's end).

Measured after the fix (2026-10-04):

- A full resume over the real NEAR archive, against a stand-in node
  serving its 66,240 hashes: 1.2 s, 1 connection, 10 lookups, and
  `--expect-rpc https://rpc-testnet.sova.io` matched #66,239.
- On 3,000 of those blocks the old binary opened 3,003 connections and
  failed on HTTPS; the new one opened 1.
- Box sim `rebuild-from-archive` with 2,500 blocks: PASS. Importing
  1,246 blocks through B's real authrpc left 2 sockets on its port, and
  the step-(6) resume made 2 lookups and opened 1 new socket.

## 10. Later: DA as a consensus rule (sketch, not built)

A SIP that makes "the block is on NEAR" part of validity would need the
following.

1. **Post before finality.** Blocks would go to NEAR within a bound of
   their sealing (k Sova blocks or t seconds). That needs supersede
   semantics: an entry for heights H.. that replaces earlier entries ≥ H
   when Zcash reorgs, with the contract keeping branches keyed by parent
   hash rather than only appending.
2. **A NEAR light client in every Sova node.** It must check that block B's
   batch is included, by tx outcome proof (`light_client_proof`) against
   NEAR block headers it tracks, with no RPC trust. NEAR's light client
   follows block producers per epoch: a moderate amount of code plus a
   trusted checkpoint.
3. **Who posts.** Today one owner posts. Under a rule, the sealer of B must
   be able to post, either by:
   - letting the contract accept any poster whose batch links (the parent
     check makes that safe); or
   - having each sealer post its own block.

   Each sealer then needs NEAR to pay gas.
4. **Liveness coupling: the real cost.** If NEAR halts or censors Sova's
   posts, Sova must either stop finalizing (DA-as-validity) or fall back to
   a timeout rule that reintroduces the gap. A SIP must pick the failure
   mode, because any external chain can halt. A plausible shape: blocks
   need a DA proof only to become *finalized*, not to be built on, so a
   NEAR outage stalls finality but not block production.
5. **Retention stays best-effort.** A consensus rule proves *publication at
   the time*, not retrieval years later. Archival copies remain operational
   (§3), as on every DA layer.

## 11. Scope statement for public copy

True today, if the poster is running on the testnet:

- "Every Sova block is posted to NEAR once it is final on Sova (about two
  hours after it's mined), in batches. NEAR consensus orders and includes
  every batch."
- "Sova's NEAR index contract (`sova-da.testnet`) lists every batch, and
  refuses gaps, duplicates and blocks that don't link to the block before.
  With only that account name and public NEAR infrastructure, anyone can
  download Sova's full history and verify every block with their own Sova
  node and zebrad."
- "Old batches are served by NEAR archival nodes and neardata.xyz. These are
  community-run, so we keep our own copy too."

Don't say:
- "stored on NEAR forever / permanently";
- "stored in NEAR state" (the bytes are transaction data; only the index is
  state);
- "NEAR guarantees Sova's data availability";
- "uses NEAR DA" as a product name (that product is dormant);
- anything implying a NEAR outage affects Sova (phase 1: it doesn't).

A short form: "Zcash secures it. NEAR keeps its history: every final Sova
block is posted to NEAR, and anyone can rebuild Sova from NEAR alone."

Backed by §9.1 (2026-10-04), true once v0.1.19 is released:

- "We rebuilt the whole public testnet (66,239 blocks) from NEAR alone, on
  a laptop with no Sova peers. Every block went through the node's own
  consensus against its own zebrad, and the result matched the live
  chain's head hash and state root."
- "The node imported about 200 blocks a second whenever its own Zcash
  scan was ahead."

Don't say a stranger can do it with v0.1.18 (it stops at #47,667, §9.1),
or that it was a "clean machine" run: it was the orchestrator's laptop
with a fresh datadir and no peers.

## 12. Running it on the testnet

**Done 2026-10-04:** steps 1 and 2.
- Contract deployed and initialised on `sova-da.testnet`: chain 82330, start
  height 0, owner `sova-da.testnet`, tx
  `pTpZMCueW3NHyqqmYNKwKmepPmp1rv41Yjv8VfeJNpb`.
- Poster function-call key `ed25519:BGQnMQPsAAy7QWWUyJnPWggC2hpcuWXFm812SyaifFhc`
  added (`post`, `set_tx`, unlimited allowance; tx
  `F2Z5EbQHdzJgrdfEcvWG4JJfZj1anxFawpPPbwr2TVJU`). Its key file is
  `~/.config/sova-near-da/sova-da.testnet.poster.json` on the orchestrator's
  laptop.
- The full-access key is `~/.config/sova-near-da/sova-da.testnet.json`. It
  never goes to a host.
- Nothing has been posted yet: step 3 is still to do.

Steps 1 and 2 are a record of what was run. Step 3 is the seed-1 install.

1. **Contract** (done):

   ```
   cargo near build non-reproducible-wasm   (in tools/near-da/contract)
   near contract deploy sova-da.testnet use-file <wasm> with-init-call new \
     json-args '{"owner":"sova-da.testnet","chain_id":82330,"start_height":0}' \
     prepaid-gas '30 Tgas' attached-deposit '0 NEAR' network-config testnet \
     sign-with-access-key-file ~/.config/sova-near-da/sova-da.testnet.json send
   ```

2. **Poster key** (done):

   ```
   sova-near-da keygen --account sova-da.testnet --out ~/.config/sova-near-da/sova-da.testnet.poster.json
   near account add-key sova-da.testnet grant-function-call-access --allowance unlimited \
     --contract-account-id sova-da.testnet --function-names post,set_tx \
     use-manually-provided-public-key <pk> network-config testnet \
     sign-with-access-key-file ~/.config/sova-near-da/sova-da.testnet.json send
   ```

3. **Install on seed-1.**
   - seed-1, like seed-2 and rpc-1, runs a follow-only node on the
     **`public`** RPC profile at `127.0.0.1:8545`. Only the keeper runs
     `local`, and the poster stays off the keeper.
   - `public` serves `eth_getBlockByNumber` but neither `debug_*` nor the
     raw-transaction methods. The poster therefore picks `full-tx` and logs
     `raw blocks: rebuilt from eth_getBlockByNumber(full) with transactions
     re-encoded (tx-hash-, hash- and root-checked)`.
   - **No node change or release is needed.** The e2e runs the node in
     `public` mode for exactly this case (§9).
   - The first install (2026-10-04) posted batch #0 (heights 0–999,
     685,796 B, tx `7UXHdzQZdRaMLeNMNzAemgUiFg6n4Np4Ejwqm1dpsLRc`) and then
     stalled. That build had only `debug` and `raw-tx`, and the second needs
     a method `public` strips. Replacing the binary is enough. The restart
     resumes at height 1000 from the contract.

   a. **Build the binary** for linux-x86_64 on the laptop, with Docker:

      ```
      tools/near-da/deploy/build-linux.sh /private/tmp/near-da-linux
      ```

      This is the release recipe (`scripts/build-linux-release.sh`): Ubuntu
      20.04 / glibc 2.31, `-C target-cpu=x86-64-v2`, and a glibc-floor check.
      It runs from a copy under `/private/tmp`, because of the `~/Documents`
      bind-mount hang. Output: `sova-near-da` and `SHA256SUMS`. A later CI
      option is a `tools/near-da` leg in `box-binaries.yml` that runs the
      same script and ships `sova-near-da` in the release tarball.

   b. **Copy to the host** (`provision.sh up --my-ip` first if the laptop's
      IP has rotated):

      ```
      scp -i ~/.ssh/sova_testnet_ed25519 /private/tmp/near-da-linux/{sova-near-da,SHA256SUMS} \
          tools/near-da/deploy/{sova-near-da.service,near-da.env.example} root@<seed-1>:/root/near-da/
      scp -i ~/.ssh/sova_testnet_ed25519 ~/.config/sova-near-da/sova-da.testnet.poster.json \
          root@<seed-1>:/root/near-da/near-da.key.json
      ```

      Only the poster key leaves the laptop. It can call only `post` and
      `set_tx` on `sova-da.testnet`. The full-access key never goes to a
      host.

   c. **Install** (on seed-1, as root):

      ```
      cd /root/near-da && sha256sum -c SHA256SUMS
      install -m 0755 sova-near-da /usr/local/bin/sova-near-da
      id sova-near-da >/dev/null 2>&1 || \
        useradd --system --user-group --home-dir /nonexistent --shell /usr/sbin/nologin sova-near-da
      install -m 0644 near-da.env.example /etc/sova/near-da.env
      install -m 0600 -o root -g root near-da.key.json /etc/sova/near-da.key.json
      shred -u near-da.key.json
      install -m 0644 sova-near-da.service /etc/systemd/system/sova-near-da.service
      systemctl daemon-reload
      ```

      The key stays root-only. systemd hands it to the service with
      `LoadCredential=` (readable only by that unit, under
      `$CREDENTIALS_DIRECTORY`).
      - State and status go to `/var/lib/sova/near-da/` (`StateDirectory=`,
        owned by `sova-near-da`).
      - The unit runs with `ProtectSystem=strict`, no capabilities, and 256 MB
        of memory.
      - The poster talks only to `127.0.0.1:8545` and to the NEAR endpoints
        in `near-da.env`. It opens no ports.

   d. **Catch up, then the steady cadence.** The archive starts at genesis,
      and the chain is ~66k blocks (~46 MB). The poster always posts full
      batches back to back while it is behind, so a catch-up is just a
      larger batch size for the first run:

      ```
      sed -i 's/^SOVA_DA_MAX_BLOCKS=.*/SOVA_DA_MAX_BLOCKS=1000/' /etc/sova/near-da.env
      systemctl enable --now sova-near-da
      journalctl -u sova-near-da -f        # one "posted batch #N ..." line per ~700 KB batch
      ```

      That is about 70 batches at ~10 s each. When
      `jq .lag_blocks /var/lib/sova/near-da/status.json` is below 1000:

      ```
      sed -i 's/^SOVA_DA_MAX_BLOCKS=.*/SOVA_DA_MAX_BLOCKS=120/' /etc/sova/near-da.env
      systemctl restart sova-near-da
      ```

      A restart at any point is safe: the contract is the source of truth
      for where to resume.

   e. **Health.** Watch `/var/lib/sova/near-da/status.json`:
      - `state` must be `ok`: `retrying` means NEAR or the node is
        unreachable; `halted` means the archive and the node disagree, or
        the key or contract is wrong, and needs a human;
      - `lag_blocks` stays below ~300 in steady state.

      A line for `host/health.sh` (not added yet):

      ```
      jq -e '.state=="ok" and .lag_blocks<300 and (now-.updated_unix)<300' /var/lib/sova/near-da/status.json
      ```

      Also keep `sova-da.testnet`'s balance above ~1 NEAR (§8).

4. **Check from anywhere:**

   ```
   sova-near-da info --contract sova-da.testnet
   sova-near-da fetch --contract sova-da.testnet --out /tmp/sova-da --expect-chain-id 82330
   sova-near-da verify /tmp/sova-da --expect-chain-id 82330 --start-height 0
   ```
