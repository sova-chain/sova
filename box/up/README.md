# sova-in-a-box

`./box/up.sh` starts a local burn-to-mine devnet: a Zcash regtest node, a
Sova node in mine mode and a continuous miner. You watch real ZEC burns
become SOVA on your own machine.

**Time to first mint:** under a minute when the binaries are prebuilt (a
checkout of a release tag) or already built. Otherwise the first run
compiles them: about 11 minutes on an idle Apple Silicon laptop, about 25
on a busy one.

## Prerequisites

- **Docker** (Docker Desktop on macOS), installed and running. `zebrad`
  runs in a container (`zfnd/zebra:6.3.0`, about 120 MB on first pull).
- **curl**.
- **Free local ports**: 18232 (zebrad RPC), 8545 (Sova RPC), 8551 (Sova
  authrpc) and 30303 (Sova p2p). The script checks them before starting
  and names the override variable for any that is taken; see
  [Tunables](#tunables).
- Optional: **python3** (decimal balances in `status`; without it you get
  hex) and **Foundry** (`forge`, for the dapp demo below).
- **Only when building from source**: Rust stable via rustup (last
  verified with rustc 1.98.1), about 2.4 GB for `target/` plus 0.8 GB of
  Cargo caches on a first-ever build (`CARGO_TARGET_DIR` and `CARGO_HOME`
  can point at another disk), and about 3 GB free on the system disk: the
  build is memory-hungry, and on a 16 GB Mac it pushes macOS into swap,
  which lives on the system volume. `CARGO_BUILD_JOBS=4 ./box/up.sh` lowers
  the memory peak but takes longer.

## Quickstart

```bash
git clone --branch v0.1.19 https://github.com/sova-chain/sova && cd sova
./box/up.sh
```

Step [1/6] gets the binaries. At a release tag it downloads them (see
[Prebuilt binaries](#prebuilt-binaries)); otherwise it builds them in
release mode, once:

| Machine | First build |
| --- | --- |
| Idle Apple Silicon laptop, Cargo cache warm | about 11 min (10m42s and 11m13s measured) |
| Same laptop, busy with other heavy work | about 20-25 min (21m02s and 25m30s measured) |
| Empty Cargo cache | add about 2 min to download crates |

The script prints a `still building (Nm elapsed)` line every minute. No
container or port is held during the build, so a failed or interrupted
build leaves nothing running. `./box/up.sh binaries` runs only this step.

Then it starts and health-checks `zebrad`, funds the miner, starts the
three processes and confirms the Sova RPC is live. The first epoch
settles a few seconds later (35-42 s from `./box/up.sh` to the first
mint, measured). At the end it prints what to run next: log tails, a
block-number check and the balance check:

```bash
curl -s -d '{"jsonrpc":"2.0","method":"eth_getBalance","params":["<evm-address-printed-above>","latest"],"id":1}' \
  -H 'Content-Type: application/json' http://127.0.0.1:8545
```

The balance is hex wei (18 decimals): `0x152d02c7e14af680000` is 6,250
SOVA, one settled epoch. With Foundry, `cast balance <evm-address> --ether
--rpc-url http://127.0.0.1:8545` prints decimals.

`./box/up.sh status` answers "is it mining?": zebrad health, the three
processes, the block height, the miner's balance, and how many epochs
have settled this run:

```
=== sova chain (http://127.0.0.1:8545) ===
block height: 136 (0x88)
miner 0x0aff...907c balance: 106,250 SOVA (0x167fd2f45f5fa5e80000 wei)
settled epochs (this run): 17
```

The logs are plain text: `grep 'settled=true'
box/up/.run/logs/sova-node.log` lists the settled epochs. `settled=false`
marks an epoch in which no burn landed.

Tear it all down:

```bash
./box/up.sh down
```

### Optional: the day-one dapp demo

With the box up (and Foundry installed), one more command deploys the
day-one kit (WSOVA, a Uniswap-v2-class factory/router, Multicall3, the
Ashwings owl mint with its ZEC checkout, and the Ashwings market) and runs
the demo loop: launch a token, seed a pool, swap, mint an Ashwing for
SOVA, list it, sell it to a second account (1% to the treasury), and
reserve a ZEC order where the node answers SIP-4. The Ashwings prices and
ZEC payee are placeholders here; `ASHWINGS_*` variables set them
(`contracts/script/deploy-kit.sh`, `box/deploy-dapps.sh`):

```bash
./box/deploy-dapps.sh                            # default RPC port 8545
./box/deploy-dapps.sh http://127.0.0.1:<port>    # if you set SOVA_BOX_RPC_PORT
```

`./box/up.sh` prints this command, with your RPC URL filled in, at the end
of its output. It takes about 20 seconds, deploys from reth's prefunded
dev account #0, and writes addresses to `contracts/deployments.json`.

Two things in forge's output look wrong but are not:

- **`Warning: EIP-3855 is not supported ... Unsupported Chain IDs: 1337`.**
  Forge judges PUSH0 support by chain ID alone. The Sova node runs PUSH0,
  and the demo's contracts use it. No `foundry.toml` setting removes the
  warning.
- **`Estimated amount required: ... ETH`.** Forge always names the native
  coin ETH. On this chain the gas is paid in SOVA.

## Prebuilt binaries

CI (`.github/workflows/box-binaries.yml`) builds release `sova` and
`sova-miner` for **macOS arm64** and **Linux x86_64** in two situations:

- **A `v*` tag is pushed** (e.g. `v0.1.19`): the binaries are attached to
  that tag's **GitHub Release** as `sova-box-bin-darwin-arm64.tar.gz` and
  `sova-box-bin-linux-x86_64.tar.gz` (each holding `sova`, `sova-miner`,
  their `SHA256SUMS`, and a `BUILD-INFO` with the commit and platform;
  from v0.1.20 also `sova-rebuild` and `sova-near-da`, the NEAR-archive
  tools, which `up` doesn't use: it unpacks and checks only `sova` and
  `sova-miner`),
  plus a release-level `SHA256SUMS` (of both tarballs) and `BUILD-INFO`
  (commit, tag, platforms). Release assets download with plain `curl`, need
  no GitHub account, and never expire. The release is only written if both
  platforms built.
- **By hand** (`workflow_dispatch`): the same binaries are kept as a
  GitHub Actions artifact per platform (`sova-box-bin-darwin-arm64`,
  `sova-box-bin-linux-x86_64`), which needs `gh` logged in to download and
  expires with the repo's artifact retention.

Plain pushes to branches build nothing here (the macOS runner is costly).

When a binary is missing, step [1/6] of `./box/up.sh` tries these, in
order, before compiling anything:

1. **The GitHub Release for your checkout's tag**: the `v*` tag `HEAD`
   is exactly at (`git describe --tags --exact-match`), or the one named by
   `SOVA_BOX_RELEASE`. It fetches `SHA256SUMS`, `BUILD-INFO` and your
   platform's tarball from
   `https://github.com/<SOVA_BOX_REPO>/releases/download/<tag>/`, and
   checks that the tarball's SHA-256 is the one `SHA256SUMS` lists, that
   `BUILD-INFO` names that tag and your platform, and that its commit has
   **the same Rust sources as your checkout** (below) before unpacking.
2. **A CI Actions artifact**: asks GitHub (via `gh`) for successful runs of
   the workflow, newest first, and takes the first whose commit has the
   same Rust sources as your checkout.
3. **Building from source** (as without prebuilt binaries).

"The same Rust sources" means `bin/`, `crates/`, `Cargo.toml`,
`Cargo.lock`, `.cargo/`, `rust-toolchain*` are identical between that
commit and your working tree, so uncommitted or untracked Rust changes mean
"no match" (and the commit has to be in your clone). For 1 and 2 alike, the
unpacked binaries' own `SHA256SUMS` must check out, their `BUILD-INFO` must
name your platform and a matching commit, and `sova-miner --version` must
run on this host; then both binaries are installed, executable, where
`cargo build --release` would have put them (the same `cargo metadata`
target resolution).

Output says which path ran: `bin/sova: PREBUILT (release <tag> (<url>),
commit <sha>) -> <path>` or `bin/sova: PREBUILT (<repo> Actions run <id>,
commit <sha>) -> <path>`, or `bin/sova: BUILDING FROM SOURCE`. Any
failure along the way (not at a tag, release or asset missing, checksum
mismatch, wrong platform, other sources; no `gh`, not logged in, artifact
expired) moves on to the next source.

In the default `auto` mode, the reasons that only mean "nothing is
published for this checkout" (not at a release tag, no `gh` or not logged
in, no GitHub repo to infer, no matching CI run) are not printed. The
script prints one line, `prebuilt: no published binaries match this
checkout; building from source`. Problems with something that *was*
found, such as a missing release asset, a checksum or `BUILD-INFO`
mismatch, a failed download, or a binary that won't run here, are always
printed. To see every reason, run `SOVA_BOX_PREBUILT=1 ./box/up.sh
binaries`: it lists why each source was skipped and exits without
building.

To fetch (or build) the binaries without starting anything (no Docker,
no ports), run `./box/up.sh binaries`.

| Var | Meaning |
| --- | --- |
| `SOVA_BOX_PREBUILT=auto` | Default: prebuilt if a release or artifact matches, else build. |
| `SOVA_BOX_PREBUILT=1` | Prebuilt only: fail (saying why) instead of building. |
| `SOVA_BOX_PREBUILT=0` | Never download; always build from source. |
| `SOVA_BOX_RELEASE=<tag>` | Release to fetch from, when `HEAD` isn't exactly at a `v*` tag (its Rust sources must still match). |
| `SOVA_BOX_REPO=<owner/name>` | Public repo whose Releases hold the binaries. Default `sova-chain/sova` (named). |
| `SOVA_BOX_RELEASE_BASE_URL=<url>` | Fetch release assets from `<url>/<tag>/<asset>` instead of GitHub (mirrors, tests). |
| `SOVA_BOX_PREBUILT_DIR=<dir>` | Use a local directory laid out like the artifact (`sova`, `sova-miner`, `SHA256SUMS`, `BUILD-INFO`) instead of GitHub. Same checks. |
| `SOVA_BOX_PREBUILT_REPO=<owner/name>` | GitHub repo to fetch the Actions artifact from, if `gh` can't infer it from your remote. |

`box/up/test-prebuilt-release.sh` exercises the release source offline: it
serves fake releases from a temp dir over `python3 -m http.server` and
checks the happy path, a tag found by `git describe`, and that a tampered
checksum, a wrong platform (in either `BUILD-INFO`), a foreign commit or a
missing tarball are each rejected and fall through to the next source.

Binaries already present in the target dir are reused as before and never
replaced by a download. Linux binaries are built in an Ubuntu 20.04 container
(`scripts/build-linux-release.sh`: glibc 2.31, x86-64-v2), so they run on
Ubuntu 20.04+, Debian 11+, RHEL 9 and Amazon Linux 2023. On an older
distro the `--version` check fails and the script builds from source.

## What you're seeing

- **zebrad (regtest)**: a disposable local Zcash node
  ([`box/regtest`](../regtest)) with no peers and no real proof-of-work.
  `auto-mine.sh` calls its `generate` RPC every few seconds, so Zcash
  blocks arrive on a steady clock.
- **The miner** (`sova-miner mine`): for every new Zcash block, it sends
  one SIP-1 burn, a standard Zcash transaction that pays ZEC to a provably
  unspendable script, until its budget runs out.
- **The Sova node** (`bin/sova`, mine mode): one Sova block per Zcash
  block. When the miner's burn lands in an epoch, the miner is that
  epoch's top-ranked burner (the only one here), and the next Sova block
  mints the epoch's SOVA to its EVM address through the block's
  withdrawals.

`tail -f box/up/.run/logs/sova-node.log` shows the loop live: burn, Zcash
block, Sova epoch trigger, settlement.

## Miner identity

`sova-miner init --data-dir box/up/.run/miner` creates (or loads) the
miner's keystore and prints the t-address to fund and the EVM address its
burns credit. `up.sh` passes that address to the node as
`SOVA_MINER_EVM_ADDRESS`.

The EVM address is the keystore key's own Ethereum address, so the SOVA
the box mines is spendable: `up` prints the `sova-miner ...
export-evm-key --i-understand` command that shows the key for an EVM
wallet. It is a regtest key; never reuse it anywhere real. A
`box/up/.run/miner` made by an older release may still credit a legacy
address that no key controls; `init` keeps it and prints a `WARNING`. Fix
it with `sova-miner --data-dir box/up/.run/miner init
--migrate-evm-address` while the box is down, or delete
`box/up/.run/miner` for a fresh identity.

## Layout

| Path | What |
| --- | --- |
| `box/up.sh` | The entrypoint (`up` / `down` / `status` / `binaries`). |
| `box/up/.run/` | Runtime state, gitignored: miner keystore (`miner/`), logs (`logs/`), PID files (`pids/`), the settings `up` used (`box.env`, read back by `down` and `status`), and the path of this run's node datadir (`node-tmpdir`). `rm -rf box/up/.run` forces a fresh miner identity and chain next run. |
| `$TMPDIR/sova-box-node.XXXXXX/` | The Sova node's datadir for this run. `down`, or a failed or interrupted `up`, removes it and nothing else. |
| `box/regtest/` | The regtest zebrad harness, reused for the container and `auto-mine.sh`. |

## Tunables

All optional env vars, read by `box/up.sh`:

| Var | Default | Meaning |
| --- | --- | --- |
| `SOVA_BOX_FUND_BLOCKS` | `101` | Blocks generated straight to the miner's own address to mature its first coinbase (100-confirmation maturity + 1). |
| `SOVA_BOX_PER_EPOCH_ZAT` | `100000` | Zatoshis burned to the SIP-1 eater script per epoch. |
| `SOVA_BOX_BUDGET_ZAT` | `6000000` | Total zatoshis (burn + fee, summed) the continuous miner loop may spend before stopping itself. Raise this for a longer-running demo. |
| `SOVA_BOX_AUTO_MINE_INTERVAL` | `3` | Seconds between auto-mined Zcash blocks. |
| `SOVA_BOX_ZEBRAD_PORT` | `18232` | Host port zebrad's RPC is published on (loopback only). |
| `SOVA_BOX_RPC_PORT` | `8545` | Sova HTTP JSON-RPC port. Pass the new URL to `./box/deploy-dapps.sh http://127.0.0.1:<port>` if you change it. |
| `SOVA_BOX_RPC_CORS` | `*` | Browser origins the Sova RPC allows (passed to `bin/sova` as `SOVA_RPC_CORS`), so the site's `/pulse` and `/ashwings/*` pages work against `http://localhost:8545` via `?rpc=` with no proxy. A comma-separated origin list narrows it; empty turns CORS off. |
| `SOVA_BOX_AUTH_PORT` | `8551` | Sova authrpc (Engine API) port. |
| `SOVA_BOX_WS_PORT` | unset (no WS) | Sova WS JSON-RPC port (passed to `bin/sova` as `SOVA_WS_PORT`, bound on 127.0.0.1). Needed for SIP-7's `sova_subscribe("zcashBlocks")`. |
| `SOVA_BOX_SIP7` | `0` | `1` runs `bin/sova` with `SOVA_SIP7=1`: SIP-7 pool reads on `0x…5A00` and the `sova_getZcashBlocks` feed. |
| `SOVA_BOX_P2P_PORT` | `30303` | Sova p2p port. |
| `SOVA_BOX_ZEBRAD_CONTAINER` | `sova-zebrad-regtest` | zebrad container name. |
| `SOVA_BOX_COMPOSE_PROJECT` | `sova-box`, or the container name if you changed it | Compose project name for the zebrad stack. Box-specific, so `./box/up.sh down` (which removes the project's volumes) never touches another `regtest` stack such as `box/regtest`'s own. |
| `SOVA_BOX_PREBUILT` | `auto` | Where missing binaries come from: `auto`, `1` (prebuilt only) or `0` (build only). See [Prebuilt binaries](#prebuilt-binaries), which also covers `SOVA_BOX_RELEASE`, `SOVA_BOX_REPO`, `SOVA_BOX_RELEASE_BASE_URL`, `SOVA_BOX_PREBUILT_DIR` and `SOVA_BOX_PREBUILT_REPO`. |
| `CARGO_TARGET_DIR` | unset | Honored. The script finds each workspace's binaries (the root one for `bin/sova`, `crates/burn-wallet` for `sova-miner`) with `cargo metadata`, so an overridden or symlinked target dir works. |

Set the port/container variables on `up` only. `up` records what it used
in `box/up/.run/box.env`, and `down` and `status` read it back. To run a
second box beside the default one (from another checkout), override all
of them:

```bash
SOVA_BOX_ZEBRAD_PORT=18242 SOVA_BOX_RPC_PORT=9545 SOVA_BOX_AUTH_PORT=9551 \
  SOVA_BOX_P2P_PORT=31303 SOVA_BOX_ZEBRAD_CONTAINER=sova-zebrad-2 ./box/up.sh
```

## Troubleshooting

- **`zebrad did not become healthy within 120s`**: check Docker Desktop is
  running and has RAM/disk headroom; `docker compose logs zebrad` (from
  `box/regtest/`) for the underlying error.
- **`sova RPC did not come up within 60s`**: check
  `box/up/.run/logs/sova-node.log`. Most likely `SOVA_MINER_EVM_ADDRESS`
  parsing failed upstream (see the init log alongside it) or the release
  binary is stale. To force a rebuild, delete the two binaries at the paths
  `up` printed in step [1/6] ("reusing ...") and rerun (with
  `SOVA_BOX_PREBUILT=0` to compile rather than download). With the default
  target dir those are `target/release/sova` and
  `crates/burn-wallet/target/release/sova-miner`.
- **`port N (...) is already in use`**: something else holds that port (a
  Hardhat or anvil node on 8545 is common, or a box from another
  checkout). Stop it, or set the variable the message names (see
  [Tunables](#tunables)).
- **`this box is already up`**: run `./box/up.sh down` first.
- **Miner stops early**: it's budget-capped by design
  (`SOVA_BOX_BUDGET_ZAT`); `box/up/.run/logs/miner.log`'s last line says
  why it stopped. The node keeps running, so a plain `./box/up.sh` refuses
  (`this box is already up`). Restart with a bigger budget:
  `./box/up.sh down && SOVA_BOX_BUDGET_ZAT=20000000 ./box/up.sh`. This keeps
  the miner identity, but the Zcash and Sova chains start over (next item).
- **`down` then `up` keeps the miner identity, not the Zcash chain**: the
  regtest chain is recreated on every `up` after a `down`, while
  `box/up/.run/miner` survives. The miner notices (its `state.json`
  records which chain it was on) and `miner.log` says `zcash chain RESET
  detected`; it sets the dead chain's UTXOs and burns aside and mines on
  the new chain with the same keys. The Sova chain is recreated too, so
  the balance starts from 0 again. See the miner README's "Chain resets".
- **Starting over completely**: `./box/up.sh down && rm -rf box/up/.run`.

## Why zebrad is the only container

`zebrad` runs in Docker; `bin/sova` and `sova-miner` run as host
processes. Building `bin/sova` (which links reth) inside a Linux
container would compile a second copy of a large dependency graph, and a
native macOS binary can't run in a Linux container. A full container
stack is a possible follow-up. Measurements from cold runs are in
[`docs/reports/e1-stranger-test.md`](../../docs/reports/e1-stranger-test.md).
