# Sova Zcash Regtest Harness (C1)

A local, disposable Zebra `regtest` node with on-demand block production, for
deterministically testing burn transactions and forced reorgs. Everything
here runs in Docker, entirely offline, with no external peers, faucets, or
non-determinism.

Built and smoke-tested on 2026-09-21 against `zfnd/zebra:6.3.0` (Docker Hub,
official Zcash Foundation image), cross-checked against the local Zebra
source clone at `research/zodl-zebra` (zebrad 5.2.0, synced from
`ZcashFoundation/zebra` main on 2026-06-23).

## TL;DR

```bash
docker compose up -d      # start the node (fresh, empty regtest chain)
./mine.sh 5                # mine 5 blocks on demand
./smoke.sh                  # full start -> mine 5 -> verify -> teardown check
docker compose down -v      # stop and discard state
```

## What's in this directory

| File | Purpose |
| --- | --- |
| `docker-compose.yml` | Runs `zfnd/zebra:6.3.0`, publishes RPC on `127.0.0.1:18232`, healthchecks via `getblockcount`. |
| `zebrad.toml` | Regtest network config, mining address, RPC (auth disabled for local convenience), ephemeral state. |
| `mine.sh` | Mines N blocks on demand via Zebra's native `generate` RPC. |
| `smoke.sh` | Starts the stack, mines 5 blocks, asserts `getblockcount == 5` and NU5-active, tears down. |

No Dockerfile is needed: the official `zfnd/zebra` image already supports
everything this harness needs (see "Why the official image" below).

## Starting the stack

```bash
docker compose up -d
docker compose logs -f zebrad   # watch it come up
```

Startup is fast (a few seconds) because Regtest has no peers to sync from and
no genesis download — Zebra validates its hard-coded Regtest genesis block
locally. `docker-compose.yml` has a healthcheck that polls `getblockcount`
over RPC, so `docker compose up -d && docker compose wait` (or the polling
loop in `smoke.sh`) is a reliable readiness signal.

To confirm manually:

```bash
curl -s -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":"1","method":"getblockcount","params":[]}' \
  http://127.0.0.1:18232/
# {"jsonrpc":"2.0","id":"1","result":0}
```

## Mining blocks on demand

```bash
./mine.sh          # mine 1 block
./mine.sh 20        # mine 20 blocks
```

`mine.sh` calls Zebra's `generate` RPC (`zebra-rpc/src/methods.rs`), which
internally runs a real `getblocktemplate` -> `proposal_block_from_template`
-> `submitblock` cycle per block — the same path Zebra's own Regtest
acceptance tests use (`zebrad/tests/integration/regtest.rs`,
`crate::common::regtest::submit_blocks_test`). It is gated in Zebra's source
to only work when `network.disable_pow()` is true, which is hard-coded `true`
only for Regtest (`Parameters::new_regtest`, `.with_disable_pow(true)`, in
`zebra-chain/src/parameters/network/testnet.rs`) — so a successful `generate`
call is itself proof the node is really on Regtest.

Coinbase rewards land at the address configured in `zebrad.toml`'s
`[mining] miner_address` (Zebra's own hard-coded default Regtest transparent
address, from `zebra-rpc/src/config/mining.rs`'s `MINER_ADDRESS` table).

## Running the smoke test

```bash
./smoke.sh
```

Actual output from a clean run (2026-09-21, `zfnd/zebra:6.3.0`, Docker
Desktop 26.1.4 on macOS/arm64):

```
--- starting stack ---
...
--- waiting for zebrad RPC readiness (healthcheck) ---
zebrad RPC is healthy
--- baseline getblockcount ---
baseline: {"jsonrpc":"2.0","id":"smoke","result":0}
--- mining 5 blocks ---
mined block: 4997c2178bb5795f7b65ed0754882a6cc1aa760fb19a901bf022a49277a12553
mined block: e02cfa4d1212417bbe96f9a22681fdf832378f4a632366085365e81e71214faa
mined block: 8927a031d3624b9d963b99e206a44e0b0c61411f0973de4d7ee7fcb4766026e6
mined block: 278064eee93434ffb12043607b6166b9e93c7e21811ea026a38459f45581b877
mined block: f75f032ab957bf552609538db656a11fa96ef2f63d69a32636fe93a771c056bd
--- checking getblockcount ---
final: {"jsonrpc":"2.0","id":"smoke","result":5}
--- checking getblockchaininfo sanity ---
chaininfo: {"jsonrpc":"2.0","id":"smoke","result":{"chain":"test","blocks":5,...,
  "upgrades":{...,"c2d6d0b4":{"name":"NU5","activationheight":1,"status":"active"}},...}}

SMOKE TEST PASSED: mined 5 blocks on Regtest, getblockcount == 5, chain == test
--- tearing down stack ---
EXIT CODE: 0
```

`smoke.sh` exits `0` on success and non-zero (with `docker compose logs`
dumped to stderr) on failure. It always tears the stack down (`docker compose
down -v`) on exit, so re-running it is idempotent and starts from a fresh
chain each time (`state.ephemeral = true`).

## Confirmed RPC surface on Regtest

All of the below were exercised live against the running container as part
of building this harness (not just read from source):

| RPC | Verified behavior |
| --- | --- |
| `generate` | Mines N blocks immediately, returns block hashes. Regtest-only (errors on other networks). |
| `getblockcount` / `getbestblockhash` / `getblockhash` | Standard tip/height queries; used by `smoke.sh`. |
| `getblockchaininfo` | Works; `chain` field is `"test"` for **both** Testnet and Regtest — see caveat below. `upgrades` map confirms NU activation status/heights. |
| `getblocktemplate` | Returns a full template (`capabilities`, `previousblockhash`, roots, etc.) — the manual/lower-level path `mine.sh` avoids by using `generate` instead. |
| `submitblock` | Used internally by `generate`; confirmed accepted blocks via `submit block accepted` log lines. |
| `sendrawtransaction` | Present and reachable — fed garbage hex and got a real parse error (`-22 parse error: bad tx header`), not "method not found", confirming the method exists and is wired to transaction-parsing logic. Real burn-tx testing will need a validly constructed signed transaction (out of scope for this harness; a wallet/tx-builder is a separate concern per Zebra's own scope — Zebra is validator-only, no wallet). |
| `getrawtransaction` | Present; queried with a bogus txid and got `-5 Transaction not found in mempool or best chain`, confirming wiring. |
| `getmininginfo` | Works; reports `blocks`, `chain`, `testnet: true`. |
| `invalidateblock` | **Works as tested.** Invalidated the block at height 6 of a 10-block chain; `getblockcount` immediately dropped to `5`. |
| `reconsiderblock` | **Works as tested.** Reconsidering that same hash restored the chain to height `10` and returned the array of reconsidered block hashes. |

Full manual verification transcript (also captured during this session):

```
== mine 10 blocks ==  (height -> 10)
== getblockhash 6 ==   aff8ba4f...
== invalidateblock aff8ba4f... ==  {"result":null}
== getblockcount ==    5
== reconsiderblock aff8ba4f... == {"result":[[...5 hashes as byte arrays...]]}
== getblockcount ==    10
```

Caveats found along the way:

- **`getblockchaininfo.chain` is `"test"`, not `"regtest"`.** This is
  `network.bip70_network_name()` (`zebra-chain/src/parameters/network.rs`),
  which intentionally collapses Testnet and Regtest to BIP70's `"test"`. Do
  not use this field to detect Regtest specifically; use activation heights,
  `generate` succeeding, or your own out-of-band knowledge of which node you
  started.
- **`reconsiderblock`'s result** is an array of hash byte arrays (Rust's
  default `Hash` serialization), not hex strings — decode accordingly if you
  script against it.
- **PoW is not "very low difficulty," it's fully disabled** on Regtest
  (`disable_pow: true`, hard-coded, not configurable) — see
  `zebrad/tests/integration/regtest.rs` and `book/src/user/regtest.md`
  ("Proof of Work validation is currently disabled on Regtest"). This is
  fine for our purposes (block production/reorg mechanics), but means this
  harness cannot be used to test PoW/Equihash-dependent code paths.

## Config notes (`zebrad.toml`)

- `network.network = "Regtest"`, `network.testnet_parameters.activation_heights.NU5 = 1`
  — per `book/src/user/regtest.md`, this also defaults
  Overwinter/Sapling/Blossom/Heartwood/Canopy to height 1
  (`ConfiguredActivationHeights::for_regtest`,
  `zebra-chain/src/parameters/network/testnet.rs`). NU6/NU6.1/NU6.2 are left
  unset (never active); add e.g. `NU6 = 1` if a future test needs them.
- `state.ephemeral = true` — every `docker compose up` starts from height 0.
  Switch to `false` + a named volume if you want state to persist across
  restarts (useful for the snapshot-based reorg approach below).
- `rpc.enable_cookie_auth = false` — the RPC defaults to cookie-file auth
  (`zebra-rpc/src/config/rpc.rs`); disabled here purely for script
  convenience, since this port is only ever published to `127.0.0.1`.
- `mining.miner_address` — Zebra's own hard-coded default Regtest address
  (`zebra-rpc/src/config/mining.rs`).

## Why the official image, not a source build

Zebra ships an official multi-arch (`amd64`/`arm64`) image at
`docker.io/zfnd/zebra`, built from the same `docker/Dockerfile` present in
`research/zodl-zebra`. As of 2026-09-21 (verified via Docker Hub and
`newreleases.io`), `zfnd/zebra:latest` is `6.3.0` (pushed ~1 month ago), and
`6.2.0` (July 17, 2026) added a Regtest-only `generatetoaddress` RPC for
funding multiple test wallets from one node — confirming the image is
actively maintained and Regtest is a first-class, current use case, not a
legacy corner.

Everything this harness needs — `generate`, `getblocktemplate`,
`submitblock`, `sendrawtransaction`, `getrawtransaction`, `invalidateblock`,
`reconsiderblock` — is compiled into the **default** release build. None of
it requires the experimental `internal-miner` Cargo feature
(`zebrad/Cargo.toml`, `zebra-rpc/Cargo.toml`): that feature only gates a
background auto-mining *thread* Zebra can run on startup. Since we mine
on-demand via RPC instead, we don't need it, and so we don't need to build
from source at all. (If a future need does call for `internal-miner` — e.g.
truly “fire and forget” continuous mining — a source build is still
available: `docker build -f ../zodl-zebra/docker/Dockerfile
--build-arg FEATURES="default-release-binaries internal-miner"
../zodl-zebra`. Expect a genuinely long Rust build, on the order of tens of
minutes.)

## Forcing reorgs

**Recommended: `invalidateblock` / `reconsiderblock`.** Verified live (see
transcript above). From the point of view of anything watching the chain
over RPC (e.g. a Sova indexer or bridge watcher), invalidating a recent block
and mining a different block on top is indistinguishable from an organic
reorg: the tip hash changes, `getblockcount` drops and climbs again,
previously-confirmed transactions get orphaned. This is deterministic,
single-node, and scriptable — exactly what's needed for reliable test
fixtures. Recipe:

```bash
# fork at height H: invalidate the block at H+1, mine a new one on top
hash=$(curl -s ... getblockhash H+1)
curl ... invalidateblock "$hash"      # chain now back at height H
./mine.sh 3                            # mine a *different* 3-block tip
# (optional) curl ... reconsiderblock "$hash"   # restores the original fork
```

One caveat carried over from Zebra's own source: `invalidateblock` currently
only affects blocks still in the **non-finalized** state (Zebra's TODO,
`zebra-rpc/src/methods.rs`: "Invalidate block hashes even if they're not
present in the non-finalized state (#9553)"). In practice this means recent
tip blocks — on the order of the last ~100 blocks
(`MAX_BLOCK_REORG_HEIGHT`) — which is exactly the range real reorgs happen
in, so it's not a practical limitation for this use case.

**Fallback: two competing regtest nodes.** If a test specifically needs to
exercise Zebra's own P2P fork-choice / longest-chain-work logic (rather than
a downstream consumer's reorg handling), run two Zebra Regtest containers
that are *not* peered, mine divergent chains on each with `mine.sh`, then
briefly peer them (or swap `submitblock` payloads between them) and observe
which chain wins. This is more moving parts and more setup per scenario, so
treat it as a secondary tool, only when `invalidateblock`/`reconsiderblock`
isn't representative enough (e.g. actually testing Zebra's consensus code,
not a client's reaction to reorgs).

Restart-from-snapshot (stop the container, `cp -r` the state volume, restart,
mine a different tip) is also possible if `state.ephemeral = false`, but is
strictly more work than `invalidateblock`/`reconsiderblock` for the same
result, so it isn't the primary recommendation.

## Why not just use Zcash Testnet

Considered and rejected for this task:

- **Sync time**: Testnet has been running continuously since 2018 with real
  (if lower) difficulty; a full sync is realistically hours, and is
  variable depending on peers/bandwidth — not something you want in a CI
  smoke test.
- **TAZ (testnet ZEC)**: burn-tx tests need funded UTXOs. On regtest, coinbase
  rewards from `generate` fund the miner address instantly and
  deterministically. On Testnet, funding requires a faucet, which is
  rate-limited, sometimes empty, and a third-party dependency outside our
  control — a bad fit for repeatable automated tests.
- **Non-determinism**: Testnet blocks arrive on their own schedule
  (~75s target) from real miners; you cannot force a specific block at a
  specific instant, and reorgs happen (or don't) according to real network
  conditions, not your test's needs. `invalidateblock`/`reconsiderblock`
  don't give you a way to fabricate a Testnet reorg either — a block you
  invalidate might get re-mined by someone else at any moment.
- **Network dependency**: contradicts "everything local" — Testnet requires
  outbound internet access to real Zcash peers, which this harness
  deliberately avoids (Regtest, per Zebra's docs, "won't connect to any
  peers").

Regtest with `generate` + `invalidateblock`/`reconsiderblock` gives strictly
more control for strictly less cost, with no real gaps found for this task's
needs.

## Source citations (all inside `research/zodl-zebra`)

- `book/src/user/regtest.md` — documented Regtest usage, config shape, both
  mining approaches (internal-miner vs. RPC).
- `zebra-network/src/config.rs` (`DNetwork`, `DConfig::deserialize`, around
  lines 620-807) — how `[network] network = "Regtest"` and
  `[network.testnet_parameters.activation_heights]` are parsed.
- `zebra-chain/src/parameters/network/testnet.rs` — `Parameters::new_regtest`
  (~line 1021, `with_disable_pow(true)` hard-coded), `disable_pow()`
  (~1145/1210), `ConfiguredActivationHeights::for_regtest` (~381-419).
- `zebra-rpc/src/methods.rs` — RPC trait + impls for `generate` (~712-726
  trait, ~2941-3005 impl, checks `network.disable_pow()`), `invalidate_block`
  / `reconsider_block` (~694-710 trait, ~2912-2939 impl), `chain:
  network.bip70_network_name()` (~1109).
- `zebra-rpc/src/config/mining.rs` — `miner_address`, `internal_miner` flag,
  hard-coded default Regtest addresses.
- `zebra-rpc/src/config/rpc.rs` — `enable_cookie_auth` default `true`.
- `zebrad/Cargo.toml` / `zebra-rpc/Cargo.toml` — `internal-miner` feature
  definition (proves it only affects the internal miner thread, not RPC
  availability).
- `zebrad/tests/integration/regtest.rs` — `invalidate_and_reconsider_block`,
  `regtest_block_templates_are_valid_block_submissions`,
  `getrawtransaction_confirmations_include_non_finalized_blocks` (uses
  `client.generate()`) — real integration tests exercising exactly the RPCs
  this harness relies on.
- `docker/Dockerfile`, `docker/entrypoint.sh`, `docker/docker-compose.yml`,
  `docker/default-zebra-config.toml`, `docker/.env` — the official Docker
  packaging this harness's `docker-compose.yml` is modeled on.

External (verified 2026-09-21 via Docker Hub and newreleases.io, since the
local clone is a few months behind upstream `main`): `zfnd/zebra` image tags
(`latest` = `6.3.0`), Zebra `v6.2.0` changelog entry for the Regtest-only
`generatetoaddress` RPC.
