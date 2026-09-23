# E1 stranger test: cold start to a visibly mining chain

**Acceptance criterion (E1):** "Cold start to a visibly mining chain in
<10 min; tested by someone who didn't build it."

**Tester:** an agent that had not worked on the box (claude-opus-5-5[1m]),
2026-09-22. It followed only `README.md`, `box/README.md` and the page it
links to (`box/up/README.md`). The one exception was reading `box/up.sh`
to check whether the script honours `CARGO_TARGET_DIR`, which this host's
disk rules require.

**Verdict: FAIL on the literal AC.** The chain was visibly mining 12m54s
after `git clone` (11m44s after `./box/up.sh`). The one-time release build
alone took 11m13s. Everything except the build took about 31 seconds, so
the non-build path passes easily. The first-run build is what breaks the
10-minute bar, and this run had favourable conditions: a warm Cargo cache,
the zebra image already pulled, and all tools already installed.

## Environment

| | |
|---|---|
| Host | Apple M3, 8 cores, 16 GB RAM, macOS 26.5.1 |
| Background load | A native testnet `zebrad` sync (on the SSD) and one other zebra container were running throughout, so the build competed for CPU |
| Rust | rustc / cargo 1.98.1 (stable, via rustup; the repo has no `rust-toolchain.toml`) |
| Docker | Docker Desktop 26.1.4. `zfnd/zebra:6.3.0` (117 MB) was **already pulled** |
| Foundry | forge 1.3.5 (already installed) |
| Cargo caches | **Warm**: `~/.cargo/registry` 1.5 GB, `~/.cargo/git` 180 MB. The log shows no crate downloads |
| Clone | `git clone` of the local `sova-chain` at `main` (`ad8f74e`) to `~/Documents/GitHub/sova-e1-stranger` (internal disk) |
| Build output | `CARGO_TARGET_DIR=/Volumes/Extreme Pro/sova/e1-stranger-target` (external SSD), enforced by the internal-disk rule. See friction point F2 |
| Ports | 18232 and 8545 were free. Other zebrads were on 18233, 18234 and 18235. There was no collision |

## Timeline (wall clock, EDT)

| Time | Step | Duration |
|---|---|---|
| 14:13:33 | `git clone` starts (T0) | 0.5s |
| 14:13:34 to 14:14:43 | Read README → box/README → box/up/README. Pre-flight: port check, check that `box/up.sh` honours `CARGO_TARGET_DIR` (it does not), add target symlinks | ~70s |
| 14:14:43 | `./box/up.sh` starts | |
| 14:14:51 | [1/6] zebrad container created and healthy | 8s |
| 14:14:51 to 14:25:02 | [2/6] `cargo build --release -p sova` (902 crates, "Finished in 10m 10s") | **611s** |
| 14:25:02 to 14:26:04 | [2/6] `cargo build --release -p sova-miner` | **62s** |
| 14:26:05 | [3/6] miner identity created | 1s |
| 14:26:18 | [4/6] funded the miner with 101 regtest blocks | 13s |
| 14:26:18 | [5/6] sova-node, auto-mine and miner started | <1s |
| 14:26:23 | [6/6] Sova RPC live. The script exits 0 and prints the "watch it mine" commands | 5s |
| **14:26:27** | **First epoch settled** (`sova epoch trigger height=103 settled=true`) | 4s |
| 14:26:31 | First balance check: `0x152d02c7e14af680000` = **6,250 SOVA**, block 104 | |
| 14:27:01 | Block 114, balance 37,500 SOVA (6 epochs) | |
| ~14:27:30 | Block 123, 11 settled epochs, **68,750 SOVA** (= 11 × 6,250) | |
| 14:28:01 | Block 131, tracking zebrad at 132 (one Sova block per ~3s Zcash block) | |
| 14:27:4x | `./box/deploy-dapps.sh` (optional demo): kit deployed, token launched, pool seeded, swap done, Relic #1 minted (collection since renamed Ashwings) | **6s**, rc=0 |
| 14:28:24 | `./box/up.sh down` | 0.67s |

### Totals vs the 10-minute AC

| Measure | Time | vs 10 min |
|---|---|---|
| Clone → first SOVA minted (true cold, this host) | **12m54s** | FAIL (+2m54s) |
| `./box/up.sh` → first SOVA minted | 11m44s | FAIL |
| Build only (bin/sova + sova-miner) | **11m13s** | Over budget on its own |
| Run only (everything except the build, up to the first mint) | **~31s** | PASS by a wide margin |
| Re-run with binaries built (not re-measured here; the README reports 21-24s) | <1 min | PASS |

A real stranger's machine would add three costs this run did not pay: the
crate download (~1.7 GB of registry and git data), the zebra image pull,
and installing Rust, Docker and Foundry if missing. The true first-run
number for a newcomer is therefore likely 13-20 minutes.

## What "visibly mining" looked like

The script's closing output told me exactly what to run: the block-number
curl, the balance curl, and three `tail -f` log commands. All the evidence
the AC asks for was there:

- **Blocks advancing:** `eth_blockNumber` went 104 → 114 → 123 → 131, one
  Sova block per regtest Zcash block, every ~3s.
- **Epochs minting SOVA:** the miner's EVM balance rose in exact steps of
  6,250 SOVA per settled epoch (6,250 → 37,500 → 68,750). The miner log
  shows one burn (100,000 zat plus a 20,000 zat fee) every other Zcash
  block.
- Teardown was clean. See the end of this report.

## Friction points and suggested fixes

Items marked **[patched]** are pure-doc fixes committed with this report.
The rest are suggestions only. No scripts were changed.

### F1. The first-run build alone exceeds the 10-minute AC (blocker for the literal AC)

`bin/sova` (reth) took 10m10s and `sova-miner` took 62s, on a warm cache
and a machine that was also running other work. `box/README.md` says v1
"meets" the "cold to a visibly mining chain in under 10 minutes" bar. That
claim only holds if the build is excluded ("steady-state ... this is the
number that matters"). The README's own evidence section reports a
**10m42s** build. A stranger reads "10-minute quickstart", then waits more
than 11 minutes.

Suggestions, in rough order of impact:
- **Ship prebuilt binaries.** Have CI build `sova` and `sova-miner` for
  aarch64-darwin and x86_64/aarch64-linux. `box/up.sh` then downloads and
  verifies them (checksum) when `target/release` is empty, and falls back
  to `cargo build` only when asked (`SOVA_BOX_BUILD=1`). This alone brings
  a cold start under a minute plus the download.
- Alternatively, publish a container image for sova and sova-miner. This
  is the tracked v2 full containerization, and it gives the same win.
- Or restate the AC honestly as "<10 min excluding a one-time ~11 min
  build". That is a product decision, not a doc fix.
- Cheaper build-time options are worth measuring, but they will not close
  the whole gap: a `[profile.box]` with `codegen-units=256` and
  `debug=false`, or `-p sova --no-default-features` if reth features can
  be trimmed. The root `Cargo.toml` has no `[profile.release]`
  customisation today.
- **[patched]** `box/up/README.md` now says "about 11-12 minutes", not
  "several minutes", and cites this run's numbers.

### F2. `box/up.sh` ignores `CARGO_TARGET_DIR`: the binary paths are hard-coded

```bash
SOVA_BIN="${REPO_ROOT}/target/release/sova"
MINER_BIN="${REPO_ROOT}/crates/burn-wallet/target/release/sova-miner"
```

With `CARGO_TARGET_DIR` exported (common on disk-constrained machines, and
required on this one), the `-x` check fails, so cargo builds into the
override dir. The script then execs a binary that does not exist at
`${REPO_ROOT}/target/release/sova`. Step [3/6] would fail with a
misleading parse error ("failed to parse miner identity"). The
troubleshooting section's `rm -rf target/release/sova ...` advice has the
same hard-coded assumption.

- **Workaround used here:** `ln -s "$CARGO_TARGET_DIR" target` and
  `ln -s "$CARGO_TARGET_DIR" crates/burn-wallet/target`. Both workspaces
  share one target dir. Both are gitignored by `**/target`.
- **Suggested script fix:** resolve the directory, don't assume it:
  `TARGET_DIR="${CARGO_TARGET_DIR:-$(cd "$REPO_ROOT" && cargo metadata --no-deps --format-version 1 | jq -r .target_directory)}"`,
  and the same for the burn-wallet workspace. Or, simpler, build with
  `--artifact-dir`, or `cargo install --path ... --root box/up/.run/bin`
  and exec from there.

### F3. No prerequisites were listed anywhere

Nothing says Docker must be installed *and running*, which Rust version is
needed (there is no `rust-toolchain.toml`), how much disk the build needs,
or which ports must be free. `README.md` did not even point to `box/`: its
layout table said "local devnet Compose stack (coming in WS-E)", which is
stale.

- **[patched]** `README.md` now has a Quickstart that points to
  `box/README.md`, and the stale "coming in WS-E" row is fixed.
- **[patched]** `box/up/README.md` now has a Prerequisites section: Docker
  running, Rust stable (verified 1.98.1), curl, ~2.3 GB for `target/` plus
  ~1.7 GB of Cargo caches, ports 18232/8545, and Foundry for the demo.
  Measured: the release `target/` for both binaries came to 2.3 GB, not the
  15+ GB of a full debug/test build.
- Script suggestions:
  - Add a preflight step before [1/6]. It would check `docker info`
    (daemon up), `cargo --version`, `curl`, free disk, and that 18232 and
    8545 are not already bound.
  - Fail fast with a single clear message. Today a port collision on 8545
    shows up only as "sova RPC did not come up within 60s".
  - Consider a `rust-toolchain.toml`.

### F4. Reading "is it mining?" needs outside knowledge

- The printed checks return hex wei (`0x152d02c7e14af680000`). The README
  never says the balance is 18-decimal wei or how to decode it.
  **[patched]**: `box/up/README.md` now explains the units, gives a worked
  example (that value = 6,250 SOVA = one epoch), and shows `cast balance
  --ether` and `cast block-number`.
- **Log colour codes break grep.** `sova-node.log` is written with ANSI
  colour codes, so `grep 'settled=true' box/up/.run/logs/sova-node.log`
  returns **0 matches** even while epochs are settling. It found 7 after
  stripping escapes. The README tells people to watch exactly those lines.
  Suggestion: launch the node with `RUST_LOG_STYLE=never`/`NO_COLOR=1`
  (or the node's `--color never`) when stdout is a file.
- `./box/up.sh status` exists but was only in the Layout table. It reports
  the tip in hex and no balance. **[patched]**: it is now mentioned in the
  quickstart. Suggestion: have `status` print the decimal height, the
  settled-epoch count and the miner's SOVA balance. That would be the
  single "is it mining?" command a stranger wants.
- The `settled=false` lines on alternate heights are expected (the miner
  burns every other block), but nothing explains them. A stranger may read
  them as failures. Suggestion: add one sentence under "What you're
  seeing".

### F5. An ERROR line every second in the node log while everything works

```
ERROR Error updating fork choice: forkchoice update error: too deep reorg
  Location: .../reth/crates/engine/local/src/miner.rs:236:19
INFO  Received invalid forkchoice updated message head_block_hash=0x6837...
```

This line appeared 115 times in the ~2 minutes of the run, once per second
and steady, starting at node launch, while blocks and settlements proceeded
normally. It looks like reth's engine `LocalMiner` is still ticking in mine
mode and repeatedly sending a forkchoice update to a stale head. To a
stranger tailing the log, which is what the README tells them to do, a
wall of ERRORs reads as "broken".

Suggestion (needs a developer, not a doc change): make sure the local
miner or its forkchoice loop is not started in mine mode, or keep its head
in sync with the sealer's canonical head. If it is truly benign, lower it
below ERROR, or at least document it. Also worth a check that this is not
masking a real issue with the engine's view of the head.

### F6. The dapp demo works but can't be found

`box/deploy-dapps.sh` ran cleanly in 6 seconds. It deployed WSOVA,
Factory, Router, Multicall3 and Relics (since renamed Ashwings), launched a token, seeded a pool,
swapped, and minted Relic #1. It is referenced only in `docs/WORKPLAN.md`,
not in any box README. Its output also includes a scary "EIP-3855 is not
supported ... Unsupported Chain IDs: 1337" warning and denominates costs
in "ETH". **[patched]**: an "Optional: the day-one dapp demo" section in
`box/up/README.md` now covers the command, its prerequisites (Foundry, box
up) and the harmless warning. Suggestions:
- Have `box/up.sh` print the demo as a "next step".
- Pass `--evm-version paris` or set it in `foundry.toml` if the warning is
  worth silencing.
- Note in the README that the deployer is reth's prefunded dev account,
  not the miner's mined SOVA. A curious stranger will ask where the gas
  came from.

### F7. Docs reference files and context that a stranger doesn't have

- `box/up/README.md` cites `/tmp/act1.sh` ("proved end to end tonight")
  three times. That file is not in the repo. Suggestion: drop the
  reference or commit the script under `box/`.
- The quickstart said `cd sova-chain`, but a clone can have any name.
  **[patched]** to `cd <your-clone>`.
- The "Why hybrid" section is written about "this machine" (disk levels on
  the author's laptop). Suggestion: move it below the quickstart, or into
  a design note, so the page opens with what to do.

### F8. Temp datadirs leak on every run (disk hygiene)

`bin/sova` in mine mode uses reth's `testing_node`, which creates
`$TMPDIR/reth-test-XXXX/` (db + rocksdb). SIGTERM from `up.sh down` does
not remove it. This host had **36** such leaked dirs (86 MB) from earlier
runs. This run's dir (5.3 MB) was also left behind, and I removed it by
hand. It is small per run, but it grows without bound on a disk-tight
machine. Suggestion: pass an explicit `--datadir` under `box/up/.run/`,
wipe it on `down` (or `up`), and handle SIGTERM gracefully so drops run.

### F9. Fixed ports and container name, with no overrides

The zebrad RPC (18232), Sova RPC (8545), container name
`sova-zebrad-regtest` and compose project `regtest` are all fixed. There
was no collision here: the other zebrads on this host used 18233, 18234
and 18235. Still, two checkouts cannot run a box side by side, and a
stranger with anything on 8545 (a Hardhat or anvil node is very common)
gets a 60-second timeout with no hint. Suggestion: add
`SOVA_BOX_ZEBRAD_PORT` and `SOVA_BOX_RPC_PORT` tunables, plus the preflight
port check from F3.

### F10. No progress indication during the long wait

For about 11 minutes the only output is ~900 lines of `Compiling ...`.
There is no ETA and no "step 2/6 is the slow one, ~11 min" message. The
script says "can take several minutes". Suggestion: print a realistic
estimate. Optionally run cargo with `--quiet` and a periodic
"still building (N/902 crates)" line, or at least "this is the one-time
~11 min step".

### Minor observations

- The internal disk went from 16.47 GB free to a low of **14.45 GB** during
  the build, even though all build output went to the external SSD. It
  recovered to ~15 GB. The cause is not determined: rustc temp files in
  `$TMPDIR`, Cargo cache writes and unrelated processes are all
  candidates. It is worth knowing on a machine that has bricked itself at
  0 GB before.
- `cargo` warns about `proc-macro-error2 v2.0.1` future incompatibility.
  It is harmless today.
- The RocksDB warning "will not keep all files open" appears at node
  start. It is harmless.
- `docker compose` uses project name `regtest` (the directory basename).
  It did not conflict with the non-compose `zebra-repro-700` container.

## Teardown verification

`./box/up.sh down` (0.67s) stopped the miner, sova-node and auto-mine
PIDs and removed the `sova-zebrad-regtest` container and the
`regtest_default` network. Checked afterwards:
- All three PIDs are gone, and no process references the clone.
- `docker ps -a` shows only the pre-existing `zebra-repro-700`, which was
  not touched.
- 18232, 8545, 8551 and 30303 are not listening. 18233, 18234 and 18235
  are still held by the pre-existing zebrads (not touched).
- The leaked `$TMPDIR/reth-test-NFi2T1Qb` was removed by hand (see F8).
- Runtime state is kept in `box/up/.run/` (gitignored), as documented.
- The build output (2.3 GB) is left at
  `/Volumes/Extreme Pro/sova/e1-stranger-target`.

---

# Re-test 2026-09-23 (current `release`, after E1b/E1c/E1d)

**Why:** the first test above ran on `ad8f74e`, before the box fixes landed
(E1b `CARGO_TARGET_DIR` resolution, E1c prebuilt-binary path, E1d
ports/logs/status work, and the `ORG_TBD` release-download path). This
re-test measures the same flow on `release` at `ecfe59a`.

**Tester:** a fresh agent that had not worked on the box
(claude-opus-5-5[1m]). It followed only `README.md`, `box/README.md` and
`box/up/README.md`, literally. It did not need to read `box/up.sh` this time.

**Verdict: FAIL on the literal AC, and by more than last time.** Clone to
the first SOVA minted took **27m20s**, and `./box/up.sh` alone took
26m25s. The source build (crate fetch included) took **25m30s** of that.
Everything else took **61s**. With binaries already present (the
prebuilt case), a fresh `./box/up.sh` reached the first mint in **35s** and
2 settled epochs in **42s**. That is a clear PASS. So the AC now depends
entirely on shipping prebuilt binaries. The code path for them exists and
falls back correctly, but nothing is published yet.

## Environment

| | |
|---|---|
| Host | Apple M3, 8 cores, 16 GB RAM, macOS 26.5.1 (same machine as the first test) |
| Background load | **Heavy.** The live native testnet `zebrad` (on the SSD) was running and used 250-275% CPU during the second half of the build. Load average was about 59-64 at 07:55. Other agent sessions were also active. The first test had a similar but lighter competing sync |
| Rust | rustc / cargo 1.98.1 (stable, rustup). There is still no `rust-toolchain.toml` |
| Docker | Docker Desktop 26.1.4 (engine 28.0.4). `zfnd/zebra:6.3.0` was **already pulled** (not removed, because other work shares it) |
| Foundry | forge 1.3.5 |
| Cargo caches | **Cold.** `CARGO_HOME=/Volumes/Extreme Pro/sova/e1s2-cargo-home` was a new empty dir, so the crates.io index, the reth and discv5 git checkouts and every crate were downloaded. It ended at 776 MB |
| Target dir | **Cold.** `CARGO_TARGET_DIR=/Volumes/Extreme Pro/sova/e1s2-target` was a new empty dir on the external SSD. It ended at 2.4 GB |
| Clone | `git clone` of the local `sova-chain` (`release`, `ecfe59a`) to `~/Documents/GitHub/sova-e1-stranger2` (internal disk, 29 MB) |
| Ports | All overridden through the documented tunables: `SOVA_BOX_ZEBRAD_PORT=18362 SOVA_BOX_RPC_PORT=18765 SOVA_BOX_AUTH_PORT=18767 SOVA_BOX_P2P_PORT=31463 SOVA_BOX_ZEBRAD_CONTAINER=sova-zebrad-e1s2` |

## Timeline (wall clock, EDT)

| Time | Step | Duration |
|---|---|---|
| 07:32:54 | `git clone` (T0) | 1s |
| 07:32:55 to 07:33:43 | Read README → box/README → box/up/README. Checked prerequisites (all already installed) | ~48s |
| 07:33:43 | `./box/up.sh` starts, with the port/container overrides and both cold dirs exported | |
| 07:33:51 | [1/6] zebrad container created and healthy | 8s |
| 07:33:51 to 07:33:52 | [2/6] prebuilt lookup: `not exactly at a v* release tag`, then `could not tell which GitHub repo this checkout is`, then `falling back to building from source`. Printed `budget ~11 min` | 1s |
| 07:33:53 to 07:35:42 | [2/6] `bin/sova`: cold fetch (crates.io index, reth and discv5 git, crate downloads) up to the first `Compiling` | **1m49s** |
| 07:35:42 to 07:57:07 | [2/6] `bin/sova` compile (902 crates; cargo reports `Finished ... in 23m 14s` including the fetch) | **21m25s** |
| 07:57:08 to 07:59:22 | [2/6] `sova-miner` fetch and compile (`Finished ... in 2m 13s`) | **2m14s** |
| 07:59:23 | [3/6] miner identity created | 1s |
| 08:00:02 | [4/6] funded the miner with 101 regtest blocks | 39s (13s last time; CPU contention) |
| 08:00:02 | [5/6] sova-node, auto-mine and miner started | <1s |
| 08:00:09 | [6/6] Sova RPC live. The script exits 0 (`time`: 26:25.56 total, 4285s user CPU) | 7s |
| **08:00:14** | **First epoch settled** (`height=103 ... settled=true`). `status`: block 104, 6,250 SOVA | 5s |
| 08:00:20 | **2 settled epochs**, 12,500 SOVA (block 105) | 6s |
| 08:00:42 | `status`: block 111, 5 epochs, 31,250 SOVA | |
| 08:00:42 to 08:00:58 | `./box/deploy-dapps.sh http://127.0.0.1:18765`: kit deployed, DAY1 launched, pool seeded, swapped, Ashwing #1 minted. rc=0. This included forge's first compile of 53 files | **16s** |
| 08:01:04 | `status`: block 117, 8 epochs, 50,000 SOVA | |
| 08:01:0x | `./box/up.sh down` | 1.5s |

**Prebuilt-equivalent run.** The binaries were already built, so this is the
closest local stand-in for "the download succeeded". It used a fresh
`box/up/.run/` (new miner identity), a new zebrad container, the same
overrides and the same load. `./box/up.sh` exited in **31s**. The first mint
came at **35s** and 2 settled epochs at **42s**.

**Release-download path, probed directly.** With an empty target dir and
`SOVA_BOX_PREBUILT=1 SOVA_BOX_RELEASE=v0.1.0 ./box/up.sh binaries`, the
script tried
`https://github.com/ORG_TBD/sova/releases/download/v0.1.0/SHA256SUMS`, got
a 404, said so, tried the Actions artifact (no repo inferable), and failed
in 3s with a clear message. That is the correct behaviour. The repo has no
`v*` tags and no published release yet.

### Totals vs the 10-minute AC

| Measure | This re-test | First test (09-22) | vs 10 min |
|---|---|---|---|
| Clone → first SOVA minted | **27m20s** | 12m54s | FAIL (+17m20s) |
| Clone → 2 settled epochs | 27m26s | (about 12m58s) | FAIL |
| `./box/up.sh` → first SOVA minted | 26m31s | 11m44s | FAIL |
| Build total (fetch + `bin/sova` + `sova-miner`) | **25m30s** (cold registry) | 11m13s (warm registry) | Over budget on its own |
|   of which crate fetch | 1m49s (+ a few seconds for sova-miner) | 0 | |
| Everything except the build, up to the first mint | **61s** | ~31s | PASS |
| **Prebuilt case** (binaries present): `up.sh` → first mint / 2 epochs | **35s / 42s** | not measured (README: 21-25s) | **PASS** |
| Prebuilt case, projected clone-to-mint: clone 1s + about 100 MB download + 35s | **about 1-2 min** | | **PASS** |

**Warm-registry build: not measured.** The cold run alone used 27 of the
45 minutes set aside. The fetch took 1m49s, so under the same load a
warm-registry build would be about **23.5 min**. Under the lighter load of
the first test it was 11m13s.

**Load dominates the build.** The build used about 71 CPU-minutes (4285s
user). On an idle 8-core M3 that is at best about 9-11 min of wall time,
which matches the README's 10m42s and the first test's 11m13s. With a busy
host it doubled. The <10-min AC cannot be met by building from source on
this class of laptop, even with nothing else running. **Prebuilt binaries
are the only path to PASS**, and the measured prebuilt-equivalent number
(35s) passes by a wide margin.

## Status of the first test's friction points

| # | First test | Now |
|---|---|---|
| F1 | Build exceeds the AC | **Still failing.** The prebuilt path exists (release download, then gh artifact, then source) and falls back cleanly, but nothing is published: no `v*` tag, and `SOVA_BOX_REPO` defaults to `ORG_TBD/sova`. See N1 |
| F2 | `CARGO_TARGET_DIR` ignored | **Fixed.** No symlinks needed. The script built into and ran from `/Volumes/Extreme Pro/sova/e1s2-target/release/` |
| F3 | No prerequisites; no preflight | **Fixed** for the docs (Prerequisites section). The script has a port-in-use check that names the override variable (not exercised; all ports were overridden) |
| F4 | Hex output, colour codes, weak `status` | **Fixed.** `status` prints the decimal height, the decimal SOVA balance and the settled-epoch count. `sova-node.log` has 0 ANSI escapes, and `grep 'settled=true'` works. The README explains `settled=false` |
| F5 | ERROR every second in the node log | **Fixed.** 0 ERROR lines. The only WARN is the harmless RocksDB fd one |
| F6 | Dapp demo undiscoverable | **Mostly fixed.** It is documented and works with a custom RPC URL. `up.sh` still does not print it as a next step, and the EIP-3855 warning and "ETH" wording remain |
| F7 | References to `/tmp/act1.sh`; "Why hybrid" first | **Still open.** `box/up/README.md` cites `/tmp/act1.sh` 3 times (lines 243, 295, 414), and "Why hybrid" still comes before Prerequisites and the Quickstart |
| F8 | Leaked `reth-test-*` temp dirs | **Fixed.** `up` records `$TMPDIR/sova-box-node.*` and `down` removed it. (`$TMPDIR` still holds 79 `reth-test-*` dirs from older runs and other tools, which this box did not create) |
| F9 | Fixed ports and container name | **Fixed.** All five overrides worked. `box.env` let `status`, `down` and `deploy-dapps.sh` (given the URL) find them with no env on later commands |
| F10 | No progress or ETA during the build | **Partly fixed.** It prints `budget ~11 min`, but the only progress after that is ~900 `Compiling` lines. See N2 |

## New and remaining friction points (with fixes)

### N1. The headline promise is prebuilt download, but no prebuilt exists yet (blocker for the AC)

`README.md`'s Quickstart says `./box/up.sh` "first run downloads the release
binaries (checkout of a release tag), else builds them from source (~11
min)". `box/README.md` still says v1 "meets" the <10-min bar. A stranger
today always gets the source build: there are no tags, the release URL is
`ORG_TBD/sova`, and the Actions artifact needs `gh` plus an inferable
GitHub remote.
- **Fix (the one that closes E1):** name the public org, set
  `SOVA_BOX_REPO`'s default to it, push `v0.1.0` so
  `box-binaries.yml` attaches the tarballs, and have a stranger re-run from
  a clone of that tag. Expected result: about 1-2 min clone-to-mint
  (35s measured, plus the download).
- **Until then (doc fix):** in `README.md` and `box/README.md`, say plainly
  that "no release is published yet, so the first run builds from source:
  about 11 min on an idle Apple Silicon laptop, 20-25+ min if the machine
  is busy or the Cargo cache is empty". Also drop "which this v1 meets"
  until a release exists.

### N2. The build budget message is wrong under realistic conditions

The script printed `budget ~11 min ... bin/sova ~10 min, sova-miner ~1 min`.
Actual: 23m14s and 2m13s. A cold registry adds ~2 min. Contention added
~12 min. Nothing tells the user the estimate assumes an idle machine.
- **Fix:** print a range and its assumption ("~11 min on an idle 8-core
  Apple Silicon laptop; slower if other heavy work is running or the Cargo
  cache is empty"). Optionally print elapsed time and the crate count every
  60s (for example `cargo build --message-format=json` counted by
  `compiler-artifact` lines, or just a background `still building (Nm
  elapsed)` ticker).

### N3. The "could not tell which GitHub repo" line reads like a to-do for the user

On a clone whose remote isn't GitHub (or without `gh`), step [2/6] prints
`prebuilt: could not tell which GitHub repo this checkout is (set
SOVA_BOX_PREBUILT_REPO=owner/name)`. A stranger can't act on it. There is
no public repo to name, and the artifact path needs a logged-in `gh`
anyway.
- **Fix:** in `auto` mode, when there is no `gh` or no inferable repo,
  collapse it to one neutral line ("prebuilt: no published release for
  this checkout; building from source"). Keep the detailed per-source
  reasons for `SOVA_BOX_PREBUILT=1` or a verbose flag.

### N4. zebrad starts, then sits idle for the whole build

Step [1/6] brings up the zebrad container before step [2/6] compiles for
25 minutes. It is harmless, but it holds a port and a container through a
step that can fail or be Ctrl-C'd. And the "10-minute quickstart" framing
hides that the real long step is the build.
- **Fix:** run the binaries step first (the `./box/up.sh binaries`
  subcommand already exists), then start zebrad. Or have the README
  recommend `./box/up.sh binaries` as an explicit, separately-timed first
  step for source builds.

### N5. F7 leftovers: `/tmp/act1.sh` and the page order

This is unchanged from the first test. `/tmp/act1.sh` "tonight" appears 3
times in `box/up/README.md`, and "Why hybrid", about the author's disk,
opens the page.
- **Fix:** remove the `/tmp/act1.sh` references or commit the script, and
  move "Why hybrid" below Troubleshooting.

### N6. Dapp demo polish (F6 remainder)

`deploy-dapps.sh` works (16s, rc=0, with the URL argument documented under
Tunables). But `up.sh`'s closing text doesn't mention it, and the output
still shows `Warning: EIP-3855 is not supported` twice and prices gas in
"ETH".
- **Fix:** add `./box/deploy-dapps.sh [http://127.0.0.1:<rpc-port>]` to
  the printed next steps, pre-filled with the actual RPC port. Set
  `evm_version` in `contracts/foundry.toml` to silence the warning.

### N7. Internal disk dips during the final link

All build output went to the SSD, but internal free space fell from 11.78
GB to a low of **8.63 GB**. The dips came during the final `bin/sova`
compile and link (07:50-07:54) and around node start. It ended at 9.1 GB
(`$TMPDIR` was 673 MB). Other sessions were active, so the cause was not
isolated, but rustc/linker temp files in `$TMPDIR` are the likely source.
- **Fix (doc):** in Prerequisites, say the build also needs about 2-3 GB of
  temporary space on the system volume even with `CARGO_TARGET_DIR`
  elsewhere, or set `TMPDIR` alongside it.

### Minor

- `proc-macro-error2 v2.0.1` future-incompat warning: unchanged.
- Funding (101 blocks) took 39s under load, against 13s before and 21s on
  the warm re-run. It is not a problem, but the whole non-build path
  (61s) is load-sensitive too.
- The zebra image was already present, so its ~120 MB pull was not paid.
  A true first-timer also pays for installing Rust, Docker and Foundry.

## Teardown verification (re-test)

`./box/up.sh down` (1.5s) stopped the miner, sova-node and auto-mine,
removed `$TMPDIR/sova-box-node.WfINlC`, and removed the
`sova-zebrad-e1s2` container and the `sova-zebrad-e1s2_default` network.
The warm re-run was torn down the same way. Afterwards:
- `docker ps -a` shows no `e1s2` container, and ports 18362, 18765, 18767
  and 31463 are not listening.
- No process references the clone, and no `sova-box-node.*` dir remains.
- The live testnet zebrad on 18233/18234 was never touched.
- `git status` in the clone was clean after the dapp demo
  (`contracts/deployments.json` and the forge broadcast/cache dirs are
  ignored).
- The build output (2.4 GB) and the cold Cargo home (776 MB) are left at
  `/Volumes/Extreme Pro/sova/e1s2-target` and `.../e1s2-cargo-home`.
