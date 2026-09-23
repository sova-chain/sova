# sova-in-a-box (E1, v1 -- hybrid)

One command, `./box/up.sh`, that brings up a full local burn-to-mine devnet:
a Zcash regtest node, a Sova node in mine mode, and a continuous miner --
so you can watch a real ZEC burn become SOVA on your own machine.

**Time to first mint:** under a minute when the binaries are prebuilt (a
checkout of a tagged release) or already built. The first tagged release
is not out yet, so today a first run compiles them: about 11 minutes on an
idle Apple Silicon laptop, about 25 on a busy one. Details in
[Quickstart](#quickstart).

## Prerequisites

- **Docker** (Docker Desktop on macOS) installed and *running* -- `zebrad`
  runs in a container (`zfnd/zebra:6.3.0`, ~120 MB pulled on first run).
- **Rust** stable toolchain via rustup (`cargo` on `PATH`). The repo does
  not pin a toolchain; last verified with rustc 1.98.1. Not needed if
  [prebuilt binaries](#prebuilt-binaries) are available for your checkout
  (a clone of a release tag: then `git` and `curl` are all it takes).
- **curl** (used by the script, the release download, and the checks
  below).
- **Disk**: the release build writes ~2.4 GB to `target/` (bin/sova and
  sova-miner share one target dir), plus about 0.8 GB of Cargo
  registry/git caches (`~/.cargo`, or `$CARGO_HOME`) on a first-ever
  build. `CARGO_TARGET_DIR` and `CARGO_HOME` can both point at another
  disk.
- **Headroom on the system disk, even with the build elsewhere**: keep
  about 3 GB free on the disk macOS boots from. The build needs a lot of
  memory, and on a 16 GB Mac it pushes the system into swap. macOS keeps
  its swap files on the system volume (`/System/Volumes/VM`, 1 GB each), so
  free space there drops by a GB or more while `bin/sova` compiles (1.1 GB
  measured, matching one new 1 GB swap file). Setting `TMPDIR` does not help:
  in a full 21-minute build with `TMPDIR` on another disk, the build never
  put more than 272 KB there. Fewer parallel jobs
  (`CARGO_BUILD_JOBS=4 ./box/up.sh`) lower the memory peak, but the build
  takes longer.
- **Free local ports**: 18232 (zebrad RPC), 8545 (Sova RPC), 8551 (Sova
  authrpc) and 30303 (Sova p2p). The script checks them before starting
  and names the override variable for any that is taken; see
  [Tunables](#tunables).
- **python3** for `./box/up.sh status`'s decimal SOVA balance (present on
  macOS with the Xcode command-line tools, which the Rust build needs
  anyway). Without it, `status` still prints the hex value.
- Optional, for the dapp demo below: **Foundry** (`forge`).

## Quickstart

```bash
git clone <this repo>   # or just cd into your existing checkout
cd <your-clone>         # the repo root
./box/up.sh
```

That's it. On a machine with the release binaries already built (every run
after the first), this takes well under a minute: get the binaries, start
and health-check `zebrad`, init and fund the miner, launch the three
background processes, and confirm the Sova RPC is live. The first epoch
settles a few seconds later (35-42s from `./box/up.sh` to the first mint,
measured).

**First run only**: if your checkout is at a release tag (or CI has
otherwise built this exact source; see [Prebuilt binaries](#prebuilt-binaries)),
`./box/up.sh` downloads `bin/sova` and `sova-miner` instead of compiling
them, so a cold start is about as fast as a warm one. No release is tagged
yet, so today it builds them in release mode if their binaries aren't
already in the release dir of each workspace's Cargo target directory
(`target/release`, or `$CARGO_TARGET_DIR/release` if you set it; the
script asks `cargo metadata` rather than assuming). How long that one-time
build takes depends on the machine (measured on an 8-core M3 with 16 GB;
see `docs/reports/e1-stranger-test.md`):

| Machine | First build |
| --- | --- |
| Idle or lightly loaded Apple Silicon laptop, Cargo cache already warm | about 11 min (10m42s and 11m13s measured) |
| Same laptop, busy with other heavy work | about 20-25 min (21m02s and 25m30s measured) |
| Empty Cargo cache | add about 2 min to download crates |

Not included: installing Rust, Docker and Foundry if you don't have them,
and the first pull of the zebrad image (~120 MB).

The script prints this range before it starts, then a `still building (Nm
elapsed)` line every minute between Cargo's own `Compiling ...` lines.
Step [1/6] builds before anything else starts, so no container or port is
held during the build, and a build that fails or is interrupted leaves
nothing running. To run the long part on its own first, use `./box/up.sh
binaries`: it gets (downloads or builds) the two binaries without Docker
or ports, and the `./box/up.sh` after it takes under a minute. Every
`./box/up.sh` after the first reuses the built binaries.

When it's done, it prints exactly what to run next -- log tails, a
block-number check, and the balance check. The important one:

```bash
# Check the miner's SOVA balance (climbs as epochs settle):
curl -s -d '{"jsonrpc":"2.0","method":"eth_getBalance","params":["<evm-address-printed-above>","latest"],"id":1}' \
  -H 'Content-Type: application/json' http://127.0.0.1:8545
```

RPC results are hex: the balance is in wei (18 decimals), so
`0x152d02c7e14af680000` is 6,250 SOVA -- one settled epoch. With Foundry
installed, `cast balance <evm-address> --ether --rpc-url http://127.0.0.1:8545`
and `cast block-number --rpc-url http://127.0.0.1:8545` print decimals.
The first epoch typically settles within a few seconds of the script
finishing.

`./box/up.sh status` is the one "is it mining?" command: zebrad health,
the three host processes, the Sova block height in decimal, the miner's
SOVA balance in decimal, and how many epochs have settled this run:

```
=== sova chain (http://127.0.0.1:8545) ===
block height: 136 (0x88)
miner 0x0aff...907c balance: 106,250 SOVA (0x167fd2f45f5fa5e80000 wei)
settled epochs (this run): 17
```

The logs are plain text (no colour codes), so they grep cleanly:
`grep 'settled=true' box/up/.run/logs/sova-node.log`. `settled=false` on
every other height is expected: the miner burns every other Zcash block.

Tear it all down cleanly:

```bash
./box/up.sh down
```

### Optional: the day-one dapp demo

With the box up (and Foundry installed), one more command deploys the
day-one kit (WSOVA, a Uniswap-v2-class factory/router, Multicall3, the
Ashwings owl mint with its ZEC checkout, and the Ashwings market) and runs
the demo loop -- launch a token, seed a pool, swap, mint an Ashwing for
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
  Forge bases this on the chain ID, not on what the node supports. The
  same warning appears against anvil started with `--chain-id 1337`, but
  not against anvil's default 31337, and anvil runs PUSH0 in both. The
  Sova node runs PUSH0 too: an `eth_call` of PUSH0 bytecode succeeds, and
  the demo's contracts deploy and run although they use it (WSOVA's
  runtime code alone has 72 PUSH0s). No `foundry.toml` setting removes
  the warning: even compiling with `--evm-version paris` (no PUSH0 at all)
  still prints it for chain ID 1337. Only a different chain ID would.
- **`Estimated amount required: ... ETH`.** Forge always names the native
  coin ETH. On this chain the gas is paid in SOVA.

## Prebuilt binaries

CI (`.github/workflows/box-binaries.yml`) builds release `sova` and
`sova-miner` for **macOS arm64** and **Linux x86_64** in two situations:

- **A `v*` tag is pushed** (e.g. `v0.1.0`): the binaries are attached to
  that tag's **GitHub Release** as `sova-box-bin-darwin-arm64.tar.gz` and
  `sova-box-bin-linux-x86_64.tar.gz` (each holding `sova`, `sova-miner`,
  their `SHA256SUMS`, and a `BUILD-INFO` with the commit and platform),
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

1. **The GitHub Release for your checkout's tag** -- the `v*` tag `HEAD`
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

To fetch (or build) the binaries without starting anything -- no Docker,
no ports -- run `./box/up.sh binaries`.

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
replaced by a download. Linux binaries are built on `ubuntu-latest`, so
they need a glibc at least as new as that runner's; on an older distro the
`--version` check fails and the script builds from source.

## What you're seeing

- **zebrad (regtest)** -- a disposable local Zcash node ([`box/regtest`](../regtest)),
  with no real peers or PoW. `auto-mine.sh` (reused as-is) calls its
  `generate` RPC every few seconds, so Zcash blocks arrive on a steady
  clock instead of waiting on nothing.
- **the miner (`sova-miner mine`)** -- for every new Zcash block it
  observes over RPC, it submits one SIP-1 burn transaction (a small,
  provably-unspendable OP_RETURN + eater output) up to its budget. This is
  the "burn digital cash" half of the pitch -- a real, standard-compliant
  Zcash transaction, nothing simulated.
- **the Sova node (`bin/sova`, mine mode)** -- follows `zebrad` block by
  block. Every Zcash block fires one trigger: one Sova block per Zcash
  block. When the miner's burn lands in an epoch and it's the epoch's
  rank-0 sealer (the only sealer, in this single-miner devnet), that
  epoch's settlement is staged and the next built Sova block mints the
  epoch's SOVA reward to the miner's EVM address through the withdrawals
  channel -- no admin key, no separate mint transaction.
- **"epoch triggers" / "settled epochs"** -- log lines in
  `box/up/.run/logs/sova-node.log` marking, respectively, a new epoch
  being triggered by a Zcash block and an epoch's reward being settled
  on-chain. `tail -f` that file to watch the loop live.

The whole loop is burn -> Zcash block -> Sova epoch trigger -> settlement.
It was first proven end to end with a single burn (6,250 SOVA minted);
this script automates it and keeps it running.

## Layout

| Path | What |
| --- | --- |
| `box/up.sh` | The one-command entrypoint (`up` / `down` / `status` / `binaries`). |
| `box/up/.run/` | Runtime state, gitignored: miner keystore (`miner/`), per-process logs (`logs/`), PID files (`pids/`), the port/container settings `up` used (`box.env`, read back by `down` and `status`), and the path of this run's node datadir (`node-tmpdir`). Delete freely (`rm -rf box/up/.run`) to force a fully fresh miner identity and chain state next run. |
| `$TMPDIR/sova-box-node.XXXXXX/` | The Sova node's datadir for this run (reth's `testing_node` puts a `reth-test-*` dir inside it). `up` creates it and records its path; `down`, or a failed/interrupted `up`, removes that one dir and nothing else. |
| `box/regtest/` | C1's harness, reused for the zebrad container and `auto-mine.sh`. Its compose file's port and container name are overridable (defaults unchanged). |

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
| `SOVA_BOX_P2P_PORT` | `30303` | Sova p2p port. |
| `SOVA_BOX_ZEBRAD_CONTAINER` | `sova-zebrad-regtest` | zebrad container name. |
| `SOVA_BOX_COMPOSE_PROJECT` | `regtest`, or the container name if you changed it | Compose project name for the zebrad stack. |
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

## Identity flow

No shared Docker volume is needed in the hybrid v1 (that's a
containerized-v2 concern) -- everything runs in one script's process tree,
so the identity handoff is just sequential steps:

1. `sova-miner init --data-dir box/up/.run/miner` creates (or loads) the
   miner's keystore and prints two labeled lines: the t-address to fund,
   and the EVM address its burns will credit.
2. `box/up.sh` parses those two lines with `awk` on the label (the last
   whitespace-delimited field of the labeled line), rather than
   hand-parsing `state.json`'s internal field names in a second place --
   one source of truth for the labels, the same one a human reads.
3. The parsed EVM address is exported as `SOVA_MINER_EVM_ADDRESS` directly
   into `bin/sova`'s environment when it's launched -- no polling, no
   marker file, because there's no process boundary to cross yet in v1.

The EVM address is the miner keystore key's own Ethereum address, so the
SOVA the box mines is spendable: `up` prints the `sova-miner ...
export-evm-key --i-understand` command that shows the key for an EVM wallet
(a regtest key; never reuse it anywhere real). A `box/up/.run/miner` made
before that default still credits the old t-addr-hash160 address, which no
key controls. `init` keeps it rather than switching silently, and `up`
shows its `WARNING` lines. Fix with `sova-miner --data-dir
box/up/.run/miner init --migrate-evm-address` while the box is down (the
next `up` hands the node the new address), or delete `box/up/.run/miner`
for a fresh identity.

## Troubleshooting

- **`zebrad did not become healthy within 120s`**: check Docker Desktop is
  running and has RAM/disk headroom; `docker compose logs zebrad` (from
  `box/regtest/`) for the underlying error.
- **`sova RPC did not come up within 60s`**: check
  `box/up/.run/logs/sova-node.log` -- most likely `SOVA_MINER_EVM_ADDRESS`
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
  why it stopped. Raise the budget and rerun `./box/up.sh` (idempotent --
  reuses the existing miner identity and any Zcash chain state).
- **`down` then `up` keeps the miner identity, not the Zcash chain**: the
  regtest chain is recreated on every `up` after a `down`, while
  `box/up/.run/miner` survives. The miner notices (its `state.json`
  records which chain it was on) and `miner.log` says `zcash chain RESET
  detected`; it sets the dead chain's UTXOs and burns aside and mines on
  the new chain with the same keys. The Sova chain is recreated too, so
  the balance starts from 0 again. See the miner README's "Chain resets".
- **Starting over completely**: `./box/up.sh down && rm -rf box/up/.run`.

## Why "hybrid"

v1 runs `zebrad` in Docker (reusing [`box/regtest`](../regtest), unchanged)
but runs `bin/sova` and `sova-miner` as **host** processes in release mode,
not in containers. Two reasons, both disk/host-shape, not code:

1. `bin/sova` links reth -- a large dependency graph (tens of minutes and
   several GB to build from scratch, with the Cargo registry cache and
   `target/` this repo already carries on disk). Building that *again*
   inside a fresh `linux/arm64` container would resolve and compile a
   second, independent copy of the same graph, on top of a machine that is
   already tight on disk. Building natively on the host reuses the
   registry cache and `target/` directories that already exist here.
2. This machine is Apple Silicon macOS; a native host binary can't run
   inside a Linux container without either a cross-compile toolchain or a
   Linux build stage -- neither of which buys anything for a local devnet
   loop, where "the box" just means "one command, one machine."

Full containerization (a Dockerfile + compose stack for `bin/sova` and
`sova-miner`, alongside `zebrad`) is a natural follow-up once either a
Linux build host or CI-built release images exist to publish from --
tracked as a v2 follow-on to E1. Containers are not what E1's "one
command, cold to a visibly mining chain in under 10 minutes" bar is
waiting on; prebuilt binaries are (see [Quickstart](#quickstart)).

`sova-miner` (`crates/burn-wallet`) never links reth in the first place --
see the standing rule in that crate's own `Cargo.toml` header comment --
so nothing about *it* is hybrid-for-a-reason; it's simply built on the
host because `bin/sova` has to be.

## Evidence from a real cold run

Captured 2026-09-22, this machine (Apple Silicon macOS, Docker Desktop
26.1.4, disk headroom ~5.5-6.8GB free throughout -- see the disk note
below).

**First-run build cost** (one-time; `rm -rf target/release/sova
crates/burn-wallet/target/release/sova-miner` to force it again):

```
   Compiling sova v0.1.0 (bin/sova)
    Finished `release` profile [optimized] target(s) in 10m 42s
```

`bin/sova`'s release build (reth included) took **10m 42s** on a host with
an already-warm Cargo registry cache (no network fetch needed, only
compilation). `sova-miner`'s release build is comparably fast to trigger
(its dependency graph never touches reth) -- budget a few minutes on a
genuinely first-ever checkout with an empty registry cache.

**Cold start with prebuilt binaries** (E1c, 2026-09-22; the artifact
simulated with `SOVA_BOX_PREBUILT_DIR` pointing at a directory laid out
exactly like the CI artifact, since the workflow had not run on GitHub
yet; empty target dir, fresh `box/up/.run/`, zebrad image already
pulled; binaries were step [2/6] then and are step [1/6] now):

```
=== [2/6] release binaries ===
prebuilt: SHA256SUMS OK, platform darwin-arm64, built from 679355dbaa0b (same Rust sources as this checkout)
bin/sova: PREBUILT (local dir ..., commit 679355dbaa0b) -> .../release/sova
sova-miner: PREBUILT (local dir ..., commit 679355dbaa0b) -> .../release/sova-miner
up exit 0 in 25s
```

and 12,500 SOVA (2 settled epochs) 12 seconds later. The download itself
(~100 MB for `bin/sova`, zipped by Actions) comes on top of that for a
real artifact.

**Steady-state `./box/up.sh` (binaries already built)** -- this is the
number that matters for "cold `docker compose up`-equivalent to a visibly
mining chain":

```
$ time ./box/up.sh
...
./box/up.sh  0.15s user 0.17s system 1% cpu 23.895 total
```

**23.9 seconds** from a completely fresh `box/up/.run/` (no prior miner
keystore, no zebrad container running) to a live Sova RPC endpoint and a
funded, mining miner. That is well inside the <10 minute AC, but only
when the binaries already exist; the one-time build is not included.
Re-ran a second time (also deleting the
`sova-miner` binary first, to exercise the "build it if missing" branch
for *both* binaries, not just `bin/sova`): **21.2 seconds**, same result.

**Chain advancing** (`eth_blockNumber`, same run, ~45s apart):

```
t+0s:   {"jsonrpc":"2.0","id":1,"result":"0x6e"}   # 110
t+45s:  {"jsonrpc":"2.0","id":1,"result":"0x84"}   # 132
```

**Epoch triggers / settlements**, straight from `sova-node.log`:

```
INFO sova epoch trigger height=109 settled=true
INFO sova epoch trigger height=110 settled=false
INFO sova epoch trigger height=111 settled=true
...
INFO sova epoch trigger height=135 settled=true
```

**Miner balance climbing** (`eth_getBalance` on the printed EVM address,
same run):

```
t+0s:   0x3f870857a3e0e380000   =  18,750 SOVA   (3 settled epochs)
t+45s:  0x1287626ee52197b00000 =  87,500 SOVA   (14 settled epochs)
```

87,500 / 14 = 6,250 SOVA per settled epoch -- exactly the reward of the
earlier one-off, single-burn proof.

**Second run** (fresh identity, sova-miner binary rebuilt from a
missing-file trigger): block height 0x75 (117) and balance
0x943b1377290cbd80000 = 43,750 SOVA (7 epochs x 6,250) about 30 seconds
after start, then a clean `./box/up.sh down`.

**Teardown**: `./box/up.sh down` stopped all three tracked PIDs, removed
the `sova-zebrad-regtest` container and its network, and both RPC
endpoints (18232, 8545) were confirmed unreachable immediately after --
in well under a second (`0.05s user 0.07s system`).

**Disk note**: this host had 4.7-5.5GB free at the start of this work,
below comfortable headroom for a from-scratch container build (the reason
v1 went hybrid at all). The host-native release build of `bin/sova`
completed without incident, disk actually recovered to ~6.8-7.8GB free
partway through (background system cache pressure relief, not anything
this script did) and stayed there through two full up/down cycles.
Nothing here writes outside `box/up/.run/` and the existing `target/`
directories, so a low-disk host degrades to "the release build fails, ask
for more space" rather than any partial/corrupt state.
