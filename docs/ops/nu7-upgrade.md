# NU7 upgrade runbook (Zcash testnet, 2026-10-05/06)

Zcash NU7 (ZIP 218: 25 s blocks; branch ID `77190ad9`) activates on the
**Zcash testnet on 2026-10-06**. Its activation height `H7` is set on
**10-05** (ZIP 259), and the NU7 Zebra release is expected the same day.
Every zebrad we run must be on that release before `H7`, or it stops at
`H7 − 1` or follows an old-rules chain, and Sova on that host follows it.
Why and what else changes: `docs/design/nu7-readiness.md` (the plan; §4.1
checklist, §4.2 fallback). This file is the exact procedure.

Who: the orchestrator runs everything here. **[Rob]** marks the only two
things Rob does: one command on the laptop (starting a daemon there is
permission-gated) and posting the Telegram notices at the end.

Everything below runs from the **main checkout on `release`**
(`~/Documents/GitHub/sova-chain/infra/testnet`): `config.env` and `out/`
live there, git-ignored, and `deploy.sh` ships that checkout's `host/`.

---

## 0. Values, hosts, helpers

### Values

| Name | Value | Where it comes from |
| --- | --- | --- |
| `H7` | NU7's Zcash testnet activation height | ZIP 259 ("To be set on OCT 5"; open zips PR #1370 would make it divisible by 3), and the release's own table (2.2c). Both must agree. |
| `S7` | `H7 − 4,388,499`: the Sova block anchored to the first NU7 Zcash block | Epoch base B = 4,388,500 (`seeds.json`), `S = H − B + 1` |
| `REL` | The NU7 Zebra release as Docker Hub tags it, e.g. `7.0.0` (no `v`; the git tag is `v$REL`) | ZcashFoundation/zebra releases |
| `DIGEST` | `sha256:…` of the `zfnd/zebra:$REL` multi-arch index | 2.2b |
| NU7 branch | `77190ad9` | ZIP 259 |
| NU6.3 branch | `37a5165b` (today's) | zebrad `getblockchaininfo` |
| Pinned today | `zfnd/zebra:6.3.0` @ `sha256:52a67e543906c98a0ed1599e2ce3ee238fc05b40592ae26ee5914ddb6ede51e3`, state format 28.0.0 | live `config.env`; `https://dl.testnet.sova.io/zebrad-testnet/latest.json` |
| Latest pre-NU7 | `zfnd/zebra:6.4.2` @ `sha256:6faf86c426d6fbdb2c10c9abde9c78d60bb0ef6d8742f05fa05eee9e7975c11d` (index), state format 28.0.0. Provenance verified 2026-09-29 (`refs/tags/v6.4.2`) | 1.5 dry run |
| Expected state format | **28.1.0**: Zebra's `nu7-zips` branch (2026-09-25) bumps the minor version for the NSM reserve and says "No resync or data migration is needed". Wider records appear only from NU7 on, so a pre-NU7 zebrad can't read a database that has passed `H7` | `zebra-state/src/constants.rs` on `nu7-zips`; confirm on the release (2.2d) |

Fill in the four placeholders on the board the moment each is known.

### Hosts

| Host | Role | zebrad data | Upgrading its zebrad pauses |
| --- | --- | --- | --- |
| `sova-seed-2` | seed (second bootnode) | root disk only (no volume) | nothing public: the canary |
| `sova-seed-1` | seed, `seed-1.testnet.sova.io`, network alerts, snapshot source | 60 GB volume | one of two bootnodes |
| `sova-rpc-1` | public RPC (`rpc-testnet.sova.io`), network alerts | 40 GB volume | the public RPC's view of new blocks |
| `sova-faucet-1` | faucet + checkout relayer (both read this zebrad) | 40 GB volume | drips and checkout claims |
| `sova-keeper-1` | the only guaranteed sealer (mine-mode node + `sova-keeper` burner) | 40 GB volume | **all new Sova blocks** for the restart |

Plus the laptop's native zebrad (RPC `127.0.0.1:18234`, P2P 18233, state
on the SSD, a source build with Zebra's **internal miner on**, see 2.6)
and the local box (regtest image, 2.7).

### Helpers (paste into the shell first)

```bash
cd ~/Documents/GitHub/sova-chain/infra/testnet
h() { ssh -i ~/.ssh/sova_testnet_ed25519 -o BatchMode=yes -o UserKnownHostsFile=out/known_hosts \
  "sova-admin@$(cat "out/servers/$1.ipv4")" "${@:2}"; }
# One JSON line per host: running image, tip, branches, NU7 as zebrad knows it.
zinfo() {
  h "$1" 'bash -s' <<'EOF'
img=$(sudo docker inspect zebrad --format '{{.Config.Image}}' 2>/dev/null)
curl -fsS -m 10 -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}' http://127.0.0.1:18232 |
  jq -c --arg img "$img" '.result | {img: $img, blocks, estimatedheight,
    chaintip: .consensus.chaintip, nextblock: .consensus.nextblock, nu7: .upgrades["77190ad9"]}'
EOF
}
HOSTS="sova-seed-2 sova-seed-1 sova-rpc-1 sova-faucet-1 sova-keeper-1"
laptop() { curl -fsS -m 10 -H 'Content-Type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" http://127.0.0.1:18234; }
```

SSH refused? The allowlist is the laptop's /32 and its IP rotates:
`./provision.sh up --my-ip`, then retry.

### Timing

| Step | Expect | Notes |
| --- | --- | --- |
| Verify the release (2.2) | 20 min | mostly reading the notes |
| `nu7-burn.sh` (2.3) | 5–15 min | cargo build of the burn-wallet tests, then ~2 min of regtest |
| One host (2.5) | 10–15 min [est] | image pull (~120 MB) happens **before** the restart; `docker stop -t 110` (seconds when clean, 110 s worst); RocksDB open + format check + re-validating ~1,000 non-finalized blocks; catching up the blocks missed. `deploy.sh --zebra-only` waits for all of it (at most `ZEBRA_READY_TIMEOUT_MIN`, 15) and records the split in `out/servers/<host>.zebra_ready` (`total= restart= rpc= format= tip=`, seconds from the restart). Replace [est] with the 1.5 dry-run numbers |
| All five hosts | ~1 h | one at a time, gated |
| Laptop build (2.6) | ~10 min | zebrad from source built in 8 min on 2026-09-22 |

**`H7` can arrive hours early.** Zcash testnet runs long bursts of 3–7 s
blocks (2026-09-26/27: ~7,000 blocks a day). Don't turn `H7 − tip` into a
clock time at 75 s or 25 s. Start the rollout as soon as 2.2–2.3 pass, and
re-read the tip before each host.

---

## 1. Pre-flight (10-03 / 10-04)

### 1.1 v0.1.16 on every host

v0.1.16 (miner and faucet sign for zebrad's next-block branch;
`finalized` 300) deploys on 10-03 the usual way (`SOVA_RELEASE_TAG` in
`config.env`, `./deploy.sh`). Then:

```bash
cat out/servers/*.release | sort | uniq -c                  # 5 v0.1.16
for s in $HOSTS; do echo "$s $(h $s 'cat /etc/sova/release; readlink /usr/local/bin/sova-miner')"; done
# The burner and the faucet actually RUN v0.1.16 (setup-host restarts the
# burner only when its binary changed; check the process, not the symlink):
h sova-keeper-1 'sudo readlink -f /proc/$(systemctl show -p MainPID --value sova-keeper)/exe'
h sova-faucet-1 'sudo readlink -f /proc/$(systemctl show -p MainPID --value sova-faucet)/exe'
# both: /usr/local/lib/sova/v0.1.16/...
./smoke.sh all                                               # 0 failed
```

`SOVA_RELEASE_TAG="v0.1.16"` must still be in `config.env` on 10-05. The
zebrad rollout (`--zebra-only`) doesn't touch the binaries, but any full
`./deploy.sh` that day (a fallback, a rollback) does, and an older tag there
would switch them back.

### 1.2 Burns and drips sign for `37a5165b`, and blocks get sealed

```bash
h sova-keeper-1 'sudo journalctl -u sova-keeper --since -3h --no-pager | grep -E "zcash consensus:|signed for consensus branch" | tail -4'
```

Expect the startup line `zcash consensus: zebrad's next block N is on
branch 37a5165b (Nu6_3); burns are signed for zebrad's next block,
expiring 40 blocks out`, and burns like `burn <txid>: signed for consensus
branch 37a5165b (Nu6_3), zebrad's next block N (tip on 37a5165b), expiry
height N+40`.

```bash
h sova-faucet-1 'sudo journalctl -u sova-faucet --since -24h --no-pager | grep -E "zcash consensus:|^.*drip .* branch" | tail -4'
```

Expect `drip <txid>: … (fee … zat, … input(s), branch 37a5165b (Nu6_3),
expiry height …)`. No drip in 24 h? Make one: `sova-miner --network test
--data-dir /tmp/nu7-preflight init`, then `curl -s -X POST -d
'{"address":"tm…"}' https://faucet-testnet.sova.io/drip`, and check the
line again.

Sealed blocks: `./smoke.sh edge` (a sealed block within 45 min) and
`systemctl is-active sova-keeper` on the keeper. A moving height alone is
not health.

### 1.3 Keeper runway

After `H7` there are 3× more Zcash blocks a day. The keeper is throttled
(`KEEPER_MIN_BURN_INTERVAL_SECS=30`: at most 2,880 burns a day), so spend
stays bounded, but the activation day is the wrong day to run dry:

```bash
T=$(cat out/servers/sova-keeper-1.keeper_taddr)
h sova-keeper-1 "curl -fsS -H 'Content-Type: application/json' --data '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getaddressbalance\",\"params\":[{\"addresses\":[\"$T\"]}]}' http://127.0.0.1:18232" | jq .result
```

Want at least 3 TAZ (about 3 days at the throttle's ceiling). Top up with a
plain transfer (`docs/ops/keeper-miner.md`).

### 1.4 Disk headroom

The upgrade itself needs little: the new image (~120 MB, root disk) and an
in-place format bump (28.0 → 28.1 expected, no copy). The snapshot in 1.6
needs about one state's size again on `sova-seed-1`.

```bash
for s in $HOSTS; do echo "== $s"; h $s 'df -h --output=target,size,used,avail,pcent / /var/lib/sova | tail -n +2; sudo du -sh /var/lib/sova/zebrad'; done
```

Pass: every filesystem under 70 % (health alerts at 80 %) with at least
5 GB free; `sova-seed-1`'s `/var/lib/sova` free ≥ zebrad state + 5 GB.
`sova-seed-2` has no volume, so everything is on `/`: look at it first.
Short? `hcloud volume resize` + `resize2fs` (a volume), or `hcloud server
change-type` (seed-2's root disk).

### 1.5 Dry run on `sova-seed-2`: 6.3.0 → 6.4.2 → 6.3.0

The same mechanism as release day, with a release that exists, so 10-05 is
only a tag change and the timing table gets real numbers.

```bash
cp config.env out/config.env.pre-nu7      # rollback copy
# config.env:
#   ZEBRA_IMAGE="zfnd/zebra:6.4.2"
#   ZEBRA_IMAGE_DIGEST="sha256:6faf86c426d6fbdb2c10c9abde9c78d60bb0ef6d8742f05fa05eee9e7975c11d"
./deploy.sh check
./deploy.sh --only sova-seed-2 --zebra-only 2>&1 | tee -a out/nu7-dryrun.log
cat out/servers/sova-seed-2.zebra_ready
h sova-seed-2 'sudo journalctl -u zebrad --since -15min --no-pager | grep -iE "format|version|upgrade|restor" | tail -20'
./smoke.sh hosts --only sova-seed-2
```

`--zebra-only` mutes seed-2's Telegram for 15 min, restarts only zebrad,
and returns once zebrad is ready (the `[zebra-ready]` lines; section 2.5
says what "ready" means), or fails. Record `zebra_ready` (total, stop+start,
RPC, format, caught up) and the format lines. Then roll back (proves the
rollback path): `cp out/config.env.pre-nu7 config.env` and the same
commands. Keep 6.3.0 on the fleet afterwards (fewest moving parts before
the 10-05 change; 6.4.2 is a mainnet DoS fix).

### 1.6 A pre-NU7 zebrad snapshot (the rollback anchor)

If a zebrad ever follows an old-rules chain deeper than its 1,000-block
reorg window, those blocks are finalized and it needs a state from below
`H7`. It also refreshes the guide's 10-day-old snapshot (4,390,524).

```bash
./publish.sh snapshot        # on sova-seed-1: capture, stop sova-node + zebrad, archive, restart, upload
```

Downtime on seed-1 = archive time. Record height, hash and SHA-256 on the
board and in the guide commit (outside the bucket, `docs/ops/snapshots.md`).
A 28.0.0 snapshot restores under the NU7 image too (opened in place), so
it stays usable for joiners until the post-activation snapshot (3.4).

### 1.7 Rollback plan (write the pins down now)

| When | Rollback | Notes |
| --- | --- | --- |
| Release day, before `H7` | `config.env` back to `out/config.env.pre-nu7` (6.3.0 pin, `NU7_ACTIVATION_HEIGHT=""`), `./deploy.sh --only <host> --zebra-only` | Only if the NU7 image misbehaves (crash loop, won't sync). Zebra's notes say pre-NU7 writes keep the 28.0 layout, so 6.3.0 should reopen the state; if it doesn't, restore 1.6's snapshot. 6.3.0 still stops at `H7`: this buys time, it doesn't fix anything |
| After `H7` | **No rollback to a pre-NU7 zebrad** | It rejects NU7 blocks and can't read the NU7-wide records. Forward only: a patched Zebra release, or 1.6's snapshot + the fixed release |
| sova-miner / sova-faucet | `SOVA_RELEASE_TAG` back one tag, `./deploy.sh --only <host>` | Anything older than v0.1.16 can't sign for `77190ad9`: after `H7` there is no older tag to go back to |
| Laptop | swap the two binary paths in the 2.6 command | before `H7` only |

### 1.8 Post notice (a) (end of this file) once 1.1 passes.

---

## 2. Release day (10-05)

### 2.1 Watch for the two inputs

```bash
curl -fsS https://zips.z.cash/zip-0259 | grep -io 'testnet[^<]*' | head          # "Testnet: TBD" until set
gh release list -R ZcashFoundation/zebra -L 3
```

Also the forum thread "NU7 timeline" (the "Testnet activation heights
decided" item). Put `H7` and `S7` on the board as soon as the height is
out, even before the release.

### 2.2 Verify the release before anything runs it

```bash
REL=7.0.0   # example: the Docker Hub tag
```

**a. The GitHub release, its tag and commit.**

```bash
gh release view "v$REL" -R ZcashFoundation/zebra --json tagName,isPrerelease,publishedAt
gh api "repos/ZcashFoundation/zebra/commits/v$REL" --jq '{sha, verified: .commit.verification.verified}'
```

`isPrerelease` must be `false`: Zebra's workflow pushes Docker Hub images
only for full releases. Zebra's tags are made by `zebra-release[bot]` and
are not signed; the commit is (GitHub-verified). Note the commit `sha`
for 2.6.

**b. The image digest and its build provenance.**

```bash
DIGEST=$(docker buildx imagetools inspect "zfnd/zebra:$REL" --format '{{json .Manifest}}' | jq -r .digest)
echo "$DIGEST"
gh attestation verify "oci://docker.io/zfnd/zebra@$DIGEST" --owner ZcashFoundation \
  --signer-workflow ZcashFoundation/zebra/.github/workflows/zfnd-build-docker-image.yml --format json |
  jq -r '.[].verificationResult.signature.certificate.sourceRepositoryRef' | sort -u
```

Must print exactly `refs/tags/v$REL`. (Run on 6.4.2 on 2026-09-29: exit 0,
`refs/tags/v6.4.2`.) Optionally also the cosign signature Zebra's release
job checks (`brew install cosign`; not on the laptop today):
`cosign verify "docker.io/zfnd/zebra@$DIGEST"
--certificate-identity-regexp='^https://github\.com/ZcashFoundation/zebra/\.github/workflows/zfnd-build-docker-image\.yml@'
--certificate-oidc-issuer='https://token.actions.githubusercontent.com'`.

**c. It is that version, and it activates NU7 at `H7` on testnet.**

```bash
docker pull "zfnd/zebra@$DIGEST"
docker run --rm "zfnd/zebra@$DIGEST" zebrad --version          # zebrad $REL
P=$(mktemp -d); cat >"$P/zebrad.toml" <<'EOF'
[network]
network = "Testnet"
[state]
ephemeral = true
cache_dir = "/home/zebra/.cache/zebra"
[rpc]
listen_addr = "0.0.0.0:18232"
enable_cookie_auth = false
[tracing]
use_color = false
EOF
docker run -d --rm --name sova-nu7-probe -p 127.0.0.1:18952:18232 \
  -v "$P/zebrad.toml:/home/zebra/.config/zebrad.toml:ro" "zfnd/zebra@$DIGEST" >/dev/null
sleep 15; curl -s -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}' http://127.0.0.1:18952 |
  jq -c '.result.upgrades | to_entries | map(.key + " " + .value.name + " @" + (.value.activationheight|tostring))'
docker rm -f sova-nu7-probe; rm -rf "$P"
```

An empty, ephemeral testnet node lists every compiled-in upgrade within
seconds (tried with 6.3.0 on 2026-09-29: `37a5165b NU6.3 @4134000` last).
The NU7 one must read `77190ad9 NU7 @<H7>`, with `H7` equal to ZIP 259's.
Missing, or another height: **stop**, this image doesn't activate NU7 where
the network does.

**d. State format.**

```bash
gh api "repos/ZcashFoundation/zebra/contents/zebra-state/src/constants.rs?ref=v$REL" --jq .content | base64 -d |
  grep -nE '^const DATABASE_FORMAT_(VERSION|MINOR_VERSION|PATCH_VERSION)'
```

Expected `28 / 1 / 0` (in-place, no resync). If the major is not 28, read
the version-history comment above it: "restorable from the previous major"
means in place (as 27 → 28 was); otherwise the new zebrad full-syncs, about
12 h per host. In that case upgrade `sova-seed-2` first and let it sync,
then carry its state to the others with `box/testnet/snapshot.sh`
(create on seed-2, restore on each), and tell joiners in notice (b).

**e. Release notes and CHANGELOG** (`gh release view "v$REL" -R
ZcashFoundation/zebra`). Look for:

- the testnet NU7 height (again: must equal ZIP 259's);
- config changes to keys we use: `[network] network, listen_addr`,
  `[state] cache_dir`, `[rpc] listen_addr, enable_cookie_auth`,
  `[tracing] use_color` (hosts, guide), and `[mining] miner_address,
  internal_miner` (laptop). A quick diff of defaults:
  `diff <(docker run --rm zfnd/zebra:6.3.0 zebrad generate) <(docker run --rm "zfnd/zebra@$DIGEST" zebrad generate)`;
- the Docker image: config path `/home/zebra/.config/zebrad.toml`, uid
  10001, entrypoint changes;
- RPC changes to `getblockchaininfo` (`consensus`, `upgrades`,
  `estimatedheight`: health.sh and sova-miner read them) and to `getblock`
  `valuePools` / `chainSupply` (SIP-7, row F1);
- ZIP 317 fee changes (row B3); a required upgrade path; known issues.

### 2.3 Regtest gates on the laptop

**Burns across NU7** (B1/B4), with the exact image:

```bash
cd ~/Documents/GitHub/sova-chain
export CARGO_TARGET_DIR="/Volumes/Extreme Pro/sova/coinbase-target/bw"   # build on the SSD
ZEBRAD_IMAGE="zfnd/zebra@$DIGEST" box/regtest/nu7-burn.sh                  # NU7 at 120, RPC 127.0.0.1:18942
```

Pass: `NU7 BURN TEST PASSED` (a burn before NU7, one in the activation
block signed `77190ad9`, one after; the old-branch burn refused).

**Value pools across NU7** (F1). Not scripted yet (gap K4); by hand:

```bash
W=$(mktemp -d); cat >"$W/zebrad.toml" <<'EOF'
[network]
network = "Regtest"
listen_addr = "0.0.0.0:18233"
[network.testnet_parameters.activation_heights]
NU5 = 1
NU6 = 1
"NU6.1" = 1
"NU6.2" = 1
"NU6.3" = 1
NU7 = 220
[state]
ephemeral = true
cache_dir = "/home/zebra/.cache/zebra"
[rpc]
listen_addr = "0.0.0.0:18232"
enable_cookie_auth = false
[mining]
miner_address = "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"
[tracing]
use_color = false
EOF
docker run -d --name sova-f1 -p 127.0.0.1:18943:18232 -v "$W/zebrad.toml:/home/zebra/.config/zebrad.toml:ro" "zfnd/zebra@$DIGEST"
sleep 10
# fee-paying transactions across 220 (the same test nu7-burn.sh runs):
(cd crates/burn-wallet && BURN_WALLET_REGTEST_RPC=http://127.0.0.1:18943 BURN_WALLET_REGTEST_NU7_HEIGHT=220 \
  cargo test --test e2e_regtest_burn -- --ignored --nocapture --exact e2e_regtest_burn_across_nu7)
r() { curl -fsS -H 'Content-Type: application/json' --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" http://127.0.0.1:18943; }
tip=$(r getblockcount | jq .result); [ "$tip" -lt 260 ] && r generate "[$((260 - tip))]" >/dev/null
r getblock '["221",1]' | jq -c '.result | {pools: [.valuePools[].id], chainSupply: .chainSupply.chainValueZat}'
# SIP-7's strict follower over tip-200..tip (spans 220):
(cd ~/Documents/GitHub/sova-chain && SOVA_TESTNET_RPC=http://127.0.0.1:18943 \
  cargo test -p consensus --test sip7_testnet -- --ignored --nocapture --exact recent_blocks_pass_the_strict_checks)
docker rm -f sova-f1; rm -rf "$W"
```

Pass: pools are exactly `transparent, sprout, sapling, orchard, lockbox,
ironwood` and the scan passes without `SIP-7 hold`. (If the strict scan
can't run against regtest, the post-activation scan in 3.4 is the real
check; the Sova nodes themselves would hold at `S7`.)

**If a gate fails:**

- `nu7-burn.sh` fails on the NU7-signed burn: the release changed signing
  since `nu7-zips`. A sova-miner/faucet hotfix (v0.1.17) is needed before
  `H7`. **Upgrade zebrad anyway**: a right-chain zebrad with no burns keeps
  Sova on null blocks on the real Zcash chain; an old zebrad doesn't.
- F1 fails (a seventh pool, or pools no longer summing to `chainSupply`):
  fix `POOL_IDS` / `check_pools` first if there is time before `H7`. If
  there isn't, upgrade zebrad anyway: a SIP-7 hold is a stall on the right
  chain, which §4.2 prefers. A changed pool list is Rob's call (SIP-7).

### 2.4 The config change

In `~/Documents/GitHub/sova-chain/infra/testnet/config.env`
(`out/config.env.pre-nu7` still holds the 6.3.0 lines):

```bash
ZEBRA_IMAGE="zfnd/zebra:$REL"            # the literal tag, e.g. "zfnd/zebra:7.0.0"
ZEBRA_IMAGE_DIGEST="$DIGEST"             # the literal sha256:… from 2.2b
NU7_ACTIVATION_HEIGHT="$H7"              # the literal height
# unchanged, but check: SOVA_RELEASE_TAG="v0.1.16"
```

```bash
./deploy.sh check
./deploy.sh render --only sova-seed-2 && grep -E '^(ZEBRA_IMAGE|ZEBRA_IMAGE_DIGEST|NU7_ACTIVATION_HEIGHT|SOVA_RELEASE_TAG)=' out/render/sova-seed-2/host.env
```

`deploy.sh --only` renders only that host's `host.env`, so each host gets
the new image and the NU7 health check together. Hosts not yet rolled keep
an empty `NU7_ACTIVATION_HEIGHT`, so they don't page `zebrad_nu7` while
they wait their turn. (For the same reason a plain `./smoke.sh hosts`
fails the digest and NU7 checks on every host not rolled yet: gate each
host with `--only`.)

### 2.5 Roll the hosts, one at a time

**Order:** `sova-seed-2` (canary) → `sova-seed-1` → `sova-rpc-1` →
`sova-faucet-1` → `sova-keeper-1`. The keeper goes last because its
restart pauses every new Sova block; by then the procedure has worked four
times. **If `H7 − tip` is under ~500 blocks when you start** (a burst),
reorder by risk: `sova-keeper-1`, `sova-rpc-1`, then the rest.

Before `H7` the old and the new zebrad agree on every block, so a
half-upgraded fleet is safe until `H7 − 1`.

For each host `S`:

```bash
S=sova-seed-2
zinfo "$S"                                                   # before
./deploy.sh --only "$S" --zebra-only 2>&1 | tee -a out/nu7-deploy.log
```

What that does, on `S` only: writes its `host.env`; on the host, installs
the health env first (the new `NU7_ACTIVATION_HEIGHT`), pulls the image,
rewrites `zebrad.env`/`zebrad.toml`, and restarts zebrad if they changed.
No pass 1, no binaries, no `sova-node`, faucet, relayer, keeper or
cloudflared step: nothing but zebrad restarts. Around the restart:

- **Mute** (K12): the host's Telegram alerts are off for
  `ZEBRA_RESTART_MUTE_MIN` (15) min; findings are still logged, marked
  `[muted …, not sent]`, and one that outlasts the mute is sent at once.
  The first health pass after it expires removes the marker.
- **Readiness wait** (K2, `host/zebra-ready.sh`), at most
  `ZEBRA_READY_TIMEOUT_MIN` (15) min from the restart: the RPC answers; the
  container runs `ZEBRA_IMAGE_DIGEST`; `upgrades["77190ad9"]` activates at
  `NU7_ACTIVATION_HEIGHT` (a wrong or missing NU7, a wrong image, or a
  zebrad panic fails at once); a state format upgrade launched on this start
  (`launching upgrade task`) has logged `database format is valid`; and the
  tip is no more than `ZEBRA_READY_LAG` (3) blocks further behind the
  `ZCASH_REFERENCE_URLS` JSON-RPC reference than it was before the restart
  (no reference answering: at least its pre-restart tip − 3).

Pass: the output has `zebrad (re)started with zfnd/zebra:$REL@$DIGEST`,
`nu7: zebrad activates NU7 (77190ad9) at $H7`, and `[zebra-ready] READY …`
with the timings (also in `out/servers/$S.zebra_ready`). Not ready:
`deploy.sh` exits non-zero with `NOT READY: <what is missing>` and
`zebrad is not ready after the restart (above); the rollout stops here`.
**Stop the rollout** there, read `journalctl -u zebrad`, and use 1.7 on
that host only. (`zebrad unchanged, not restarted`: the host already runs
this image and config.) Format lines, if you want them:
`h "$S" 'sudo journalctl -u zebrad --since -20min --no-pager | grep -iE "format|version|upgrade|panic|error" | tail -20'`.

Gate before the next host (K3):

```bash
./smoke.sh hosts --only "$S"                                  # 0 failed
```

It checks, besides the services and health: `zebrad runs
ZEBRA_IMAGE_DIGEST` (the container's image ref or RepoDigests) and `zebrad
activates NU7 (77190ad9) at H7 = NU7_ACTIVATION_HEIGHT`, and runs
`health.sh` on the host (its `nu7:` line; a muted host shows a `note`).

Role checks:

- **`sova-faucet-1`**: `--zebra-only` doesn't restart `sova-faucet` or the
  relayer; they ride through the zebrad restart (their zebrad calls fail
  for the minute or two it is down). `./smoke.sh edge` for the faucet and
  the checkout relayer. A fresh drip (1.2) signs for `37a5165b`.
- **`sova-keeper-1`**: the burner and the node are not restarted and ride
  through the zebrad restart. `h sova-keeper-1 'systemctl is-active
  sova-keeper; sudo journalctl -u sova-keeper --since -10min --no-pager |
  grep "signed for consensus branch" | tail -2'`: active, burns resume on
  `37a5165b`. Then `./smoke.sh edge` sees a sealed block within 45 min.

Expected noise: none from the host itself (muted). The network alert hosts
(`sova-rpc-1`, `sova-seed-1`) are not muted by another host's restart: a
long keeper restart can bring `block_age` from them after 10 min. To
silence that too, `./deploy.sh mute 20 --only sova-rpc-1` and `--only
sova-seed-1` before the keeper (`./deploy.sh unmute` after).

### 2.6 The laptop's native zebrad

It is a source build of Zebra 6.3.0
(`/Volumes/Extreme Pro/sova/zebra-target/release/zebrad`, config
`/Volumes/Extreme Pro/sova/zebrad-testnet-mining-sapling.toml`, log
`/Volumes/Extreme Pro/sova/zebrad-testnet-mining.log`), running with
**`internal_miner = true`**. Left on 6.3.0 past `H7`, it would mine
min-difficulty old-rules blocks itself: exactly the old-rules chain of
§4.2, made by us, and any of our zebrads still on 6.3.0 that peer with it
would follow. **It must be on the NU7 build, or stopped, by `H7 − 50`.**

Build (orchestrator; no daemon starts, the running one is untouched). A
separate worktree and target dir on the SSD, so the running binary and its
checkout stay as they are:

```bash
Z=~/Documents/GitHub/sova-review-audit/research/zebra-upstream
git -C "$Z" fetch origin "refs/tags/v$REL:refs/tags/v$REL"
git -C "$Z" rev-parse "v$REL^{commit}"                       # = the sha from 2.2a
git -C "$Z" worktree add --detach "/Volumes/Extreme Pro/sova/zebra-nu7-src" "v$REL"
(cd "/Volumes/Extreme Pro/sova/zebra-nu7-src" &&
  CARGO_TARGET_DIR="/Volumes/Extreme Pro/sova/zebra-nu7-target" cargo build --release --locked --bin zebrad)
"/Volumes/Extreme Pro/sova/zebra-nu7-target/release/zebrad" --version    # zebrad $REL
```

If 2.2e shows the `[mining]` keys changed, fix the config file first.

**[Rob]** the swap, one line in a terminal (clean stop of 6.3.0, then the
NU7 build on the same config, state, ports and log):

```bash
pid=$(pgrep -f '/zebra-target/release/zebrad -c'); [ -n "$pid" ] && kill -INT $pid && while kill -0 $pid 2>/dev/null; do sleep 1; done; nohup "/Volumes/Extreme Pro/sova/zebra-nu7-target/release/zebrad" -c "/Volumes/Extreme Pro/sova/zebrad-testnet-mining-sapling.toml" start >> "/Volumes/Extreme Pro/sova/zebrad-testnet-mining.log" 2>&1 &
```

Then (orchestrator):

```bash
ps -axo pid,command | grep '[z]ebra-nu7-target'
laptop getblockchaininfo | jq '.result | {blocks, estimatedheight, nu7: .upgrades["77190ad9"]}'
tail -50 "/Volumes/Extreme Pro/sova/zebrad-testnet-mining.log" | grep -iE "format|version|error|panic"
```

**If the build isn't ready by `H7 − 50`, [Rob]** stops the old one (a
stall is harmless; an old-rules miner isn't):
`pkill -INT -f '/zebra-target/release/zebrad -c'`.

Rollback before `H7`: the same swap line with `zebra-target` and
`zebra-nu7-target` exchanged.

### 2.7 Join files, guides, box

**Join files** (seeds.json carries `zebra_image`):

```bash
./bootnodes.sh && ./publish.sh join
curl -fsS https://dl.testnet.sova.io/seeds.json | jq -r .zebra_image     # zfnd/zebra:$REL
cp out/seeds.json out/testnet.env out/bootnodes.txt out/epoch-base.json published/
```

Commit `published/` (the committed copy still says v0.1.7; the live one
says v0.1.14 today, v0.1.16 after 10-03).

**Guides and copy** (commit on `release`, then sync the public repo):

| File | Change |
| --- | --- |
| `docs/guides/testnet.md` 1c | `zfnd/zebra:6.3.0` → `zfnd/zebra:$REL` |
| `docs/guides/testnet.md` 1b | one line: the snapshot is from 6.3.0 (state 28.0), and `$REL` opens it in place. (`TAG=`/`checkout` lines went to v0.1.16 on 10-03.) |
| `docs/guides/testnet-reference.md` | line 34 (Tools: image), line 59 (snapshot format), "Keeping it running": a **Zcash network upgrades** bullet with notice (b)'s upgrade command. (Line 95 already says 25 s from NU7 on.) |
| `infra/testnet/config.env.example` | `ZEBRA_IMAGE` line and its comment ("snapshots must match its state format (6.3.0 = state v28)") |
| `infra/testnet/host/zebrad.toml` | the "proves for zfnd/zebra:6.3.0" comment |
| `docs/ops/snapshots.md` | the 6.3.0 / state 28 notes: NU7 zebrad reads 28.0, writes 28.1 |
| `site/src/pages/node.astro:23`, `site/README.md:186` | only with the box bump below (they describe the box) |

**Box** (regtest; NU7 stays off there, `box/regtest/zebrad.toml` has only
`NU5 = 1`). Not on the 10-05 critical path: bump on 10-06/07, after the
fleet. `box/regtest/docker-compose.yml:3`,
`box/testnet/test/docker-compose.yml:7`, `box/up/README.md:15`,
`box/regtest/README.md` → `zfnd/zebra:$REL`. Gate, with the new image:
`box/regtest/smoke.sh`, `box/regtest/e2e-burn.sh`,
`box/testnet/test/snapshot-e2e-regtest.sh`, and one `box/sim` scenario
(SSD target, one suite at a time). A newer Zebra may change regtest
defaults; these catch it.

### 2.8 Post notice (b) once 2.5 is done and 2.7's join files are live.

### 2.9 Go / no-go, by block height

| At | Must be true | Else |
| --- | --- | --- |
| `H7 − 300` | 2.2–2.3 passed; rollout started | Release missing or failing verification: go to 3.3 "release late" now, while there's time to warn |
| `H7 − 50` | All five hosts `nu7.activationheight == H7` (`./smoke.sh hosts`, 0 failed); laptop on the NU7 build or stopped | A host that can't be upgraded: a follower may stall (fine); **the keeper may not**: `./deploy.sh keeper-pause` (3.3, "Pause the keeper") at `H7 − 5` |
| `H7 − 1` | `zinfo` on every host: `nextblock` = `77190ad9` | See 3.3 |

---

## 3. Activation watch (10-06)

### 3.1 What to watch

| What | Command | Healthy |
| --- | --- | --- |
| Zcash tip and branches, every host | `for s in $HOSTS; do echo "$s $(zinfo $s)"; done` | at `H7 − 1`: `nextblock` `77190ad9`; from `H7`: `chaintip` `77190ad9`, `nu7.status` `active`; tips within a few blocks of each other |
| Laptop | `laptop getblockchaininfo \| jq -c '.result \| {blocks, c: .consensus, s: .upgrades["77190ad9"].status}'` | same |
| Same chain as the world | `curl -fsS https://api.testnet.cipherscan.app/api/block/$H7 \| jq -r .hash` and `curl -fsS -H 'Content-Type: application/json' --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getblockhash\",\"params\":[$H7]}" https://zcash-testnet-zebrad.gateway.tatum.io \| jq -r .result`, vs `h <host> "curl -fsS -H 'Content-Type: application/json' --data '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getblockhash\",\"params\":[$H7]}' http://127.0.0.1:18232" \| jq -r .result` | one hash everywhere |
| Health, every host | `h <host> 'sudo journalctl -t sova-health --since -10min --no-pager \| grep -E "nu7:\|zcash reference\|ALERT"'` | `nu7: zebrad is on NU7 (tip …, activation H7, chaintip 77190ad9, nextblock 77190ad9)`; `zcash reference: block … here and at 2 of 2 reference(s)` |
| Keeper burns | `h sova-keeper-1 'sudo journalctl -u sova-keeper --since -30min --no-pager \| grep -E "signed for consensus branch\|error\|rejected" \| tail -8'` | the burn for the activation block: `signed for consensus branch 77190ad9 (Nu7), zebrad's next block H7 (tip on 37a5165b), expiry height ≈ H7+120`; after it `(tip on 77190ad9)` |
| Faucet drips | a fresh drip (as in 1.2), then its journal line | `branch 77190ad9 (Nu7)`, and the txid mined (`laptop getrawtransaction "[\"<txid>\",1]" \| jq .result.height`) |
| Sova block `S7` | `cast block $S7 --field extraData --rpc-url https://rpc-testnet.sova.io` and the scan below | exists; sealed blocks (97-byte `extraData`) resume after `S7` |
| Sova nodes don't hold | `h <host> 'sudo journalctl -u sova-node --since -30min --no-pager \| grep -cE "sip-7 hold\|sova-hold: zcash anchor mismatch"'` on seeds, rpc, keeper | 0 |
| Telegram | `zcash_fork`, `zcash_ref_fork`, `zebrad_nu7`, `block_age`, `null_run`, `epoch_lag`, `c5_reject`, `rejecting_blocks` | none |

Sealed share after activation (server-side on `sova-rpc-1`; the laptop's
path to the RPC is slow). Run it over 300–500 blocks and quote the Zcash
spacing with it; short samples overstate it:

```bash
h sova-rpc-1 "bash -s $S7 400" <<'EOF'
from=$1; n=$2
rpc() { curl -fsS -H 'Content-Type: application/json' --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}" http://127.0.0.1:8545; }
head=$(( $(rpc eth_blockNumber '[]' | jq -r .result) )); to=$(( from + n - 1 < head ? from + n - 1 : head ))
sealed=0; total=0; t0=""; t1=""
for ((b = from; b <= to; b++)); do
  blk=$(rpc eth_getBlockByNumber "[\"$(printf '0x%x' "$b")\",false]")
  x=$(jq -r .result.extraData <<<"$blk"); t=$(( $(jq -r .result.timestamp <<<"$blk") ))
  [[ -z $t0 ]] && t0=$t; t1=$t; total=$((total + 1)); [[ ${#x} -eq 196 ]] && sealed=$((sealed + 1))
done
echo "Sova $from..$to: $sealed/$total sealed ($((100 * sealed / total))%), mean Zcash spacing $(( (t1 - t0) / (total > 1 ? total - 1 : 1) )) s"
EOF
```

### 3.2 What "done" looks like

Every zebrad (5 hosts + laptop) on `77190ad9` past `H7` with the same hash
at `H7` as both references; the keeper's burns signed `Nu7` and mined;
sealed blocks after `S7`; no `sip-7 hold`; a drip signed `Nu7` mined;
`./smoke.sh all` 0 failed. Then notice (c).

### 3.3 Decision tree

**Pause the keeper** (used below; K7). §4.2 says "stop the keeper so no
burns go into it". Stopping only `sova-keeper` still leaves the keeper's
mine-mode `sova-node` building null blocks on whatever its zebrad follows,
so the Sova chain would keep growing on a dead anchor. To get the stall
§4.2 prefers, pause both:

```bash
./deploy.sh keeper-pause --reason "NU7: <why>"
```

It writes `/etc/sova/.keeper-paused` on the keeper, then stops
`sova-keeper` and then `sova-node`. While the marker exists nothing starts
them: not systemd (`ConditionPathExists=!` in both units, so not a reboot
or a hand-typed start either), not `setup-host.sh` (a full `./deploy.sh`
or `--zebra-only` on the keeper is safe and logs `NOT started … the keeper
is paused`). The keeper's `health.sh` logs `keeper paused (planned) since …`
instead of `keeper_down`, `sova_down` or `keeper_isolated`, and `./smoke.sh
hosts` passes the two stopped units as planned. The network alert hosts
still send `block_age` (the network really has stopped).

Resume only when the keeper's zebrad is on `77190ad9` and agrees with the
references at `tip − 6`:

```bash
./deploy.sh keeper-resume
```

It removes the marker, starts `sova-node`, waits for its RPC and 60 s, then
starts `sova-keeper` if it was running when paused (a burner that was
already stopped stays stopped, and it says so).

| Symptom | How you see it | Response |
| --- | --- | --- |
| **Release late** (not out, or fails 2.2, by `H7 − 300`) | 2.1 | Post "testnet pauses at Zcash `H7`; resumes when Zebra ships NU7" (Rob; adapt notice (b)). Keep every host on 6.3.0: they stop at `H7 − 1` unless someone mines old rules. **[Rob]** stops the laptop's internal miner (2.6 `pkill` line) by `H7 − 50`. At `H7 − 5`, `./deploy.sh keeper-pause`. When the release lands: 2.2–2.5, then `./deploy.sh keeper-resume`. No new Sova genesis: a stall is recoverable (§4.2) |
| **Stall: our tip stays at `H7 − 1`** | `block_age` "zebrad's tip … is N s old"; `zinfo` blocks = `H7 − 1` | Ask the references for `H7`. **They don't have it either:** Zcash testnet hasn't mined an NU7 block yet (under NU7 a min-difficulty block is allowed after 150 s). Nothing to do; Sova resumes by itself. **They have it:** our zebrad rejects NU7 blocks. `zinfo`: is `nu7.activationheight` `H7`? Is the image the verified digest? `journalctl -u zebrad` for the rejection. Fix the image (a patched release through 2.2–2.5). Sova is stalled on the right chain meanwhile: that is the safe state. |
| **Old-rules chain** | `zcash_fork` ("past NU7's activation height … on branch 37a5165b"), `zcash_ref_fork`; or a follower's `sova-hold: zcash anchor mismatch` / `c5_reject` | **On the keeper: pause the keeper at once** (`./deploy.sh keeper-pause`), then upgrade its zebrad (`./deploy.sh --only sova-keeper-1 --zebra-only`; the pause holds through it), and `./deploy.sh keeper-resume` once that zebrad is on `77190ad9`. **On a follower** (seed, rpc, faucet): it only hurts itself (it holds the keeper's blocks against its wrong zebrad); upgrade it (`--zebra-only`), `sova-rpc-1` first since it is the public view. The upgraded zebrad reorgs to the NU7 chain if the wrong branch is under 1,000 blocks; deeper, restore 1.6's snapshot. Find who mined the old-rules blocks (the laptop? 2.6). If Sova blocks were built on the dead anchor more than 300 deep (`finalized`, v0.1.16), those nodes wedge: stop, write it up, don't improvise a reset. Post a pause notice |
| **Burns or drips rejected** | Keeper journal: a send error after a `signed for consensus branch` line; no sealed blocks after `S7`; `null_run` after 45 min; faucet drips fail | Which branch was signed? **`37a5165b` for a block ≥ `H7`:** that host's zebrad is pre-NU7 (`zinfo`): upgrade it. **`77190ad9` and still rejected:** a signing bug. Stop `sova-keeper` (only the burner; the node keeps null blocks on the right chain), capture the error, hotfix v0.1.17, redeploy miner and faucet. Post "no new transactions until the fix; the chain keeps running" |
| **Burns built just before `H7` expire** | burns signed `37a5165b` for `H7 − 1` that missed it | Expected, not an incident: they can't be mined after `H7` and expire after 40 blocks (~17 min at 25 s), freeing their inputs. A short gap in keeper burns right after `H7` can follow if its UTXOs were in those burns |
| **SIP-7 hold at `S7`** | `sip-7 hold: zcash scan stopped before this block` in `sova-node`; `epoch_lag` "NETWORK BEHIND ZCASH" on every host | Row F1: the release changed `valuePools` / `chainSupply`. Diff `getblock $H7 1` pools with a pre-NU7 block, fix `crates/consensus/src/pools.rs`, release. Rob's call only if the pool list changes. Sova is stalled on the right chain until then |
| **Low sealed share after activation** | 3.1 scan | Not an incident by itself: 25 s blocks with a 30 s keeper throttle (`KEEPER_MIN_BURN_INTERVAL_SECS`) and the 15 s rank step (F5) mean fewer sealed blocks per Zcash block. Measure over 300–500 blocks for a day before changing anything; F5 is a draft SIP-6 parameter (Rob) |

### 3.4 After activation (10-06 → 10-07)

- **Spend**: keeper spend per hour from its journal over the first day,
  not per-epoch arithmetic (bursts).
- **F1 on real data**, once `H7 + 200` exists: `SOVA_TESTNET_RPC=http://127.0.0.1:18234
  cargo test -p consensus --test sip7_testnet -- --ignored --nocapture
  --exact recent_blocks_pass_the_strict_checks` against the laptop node.
- **New snapshot** once `H7 + 1,100` exists (NU7 blocks then sit in the
  finalized database, not only in the non-finalized backup):
  `./publish.sh snapshot`. Its `snapshot.json` should say `zebra_version`
  `v$REL`, `state_version` `28.1.0`. Test-restore it as in
  `docs/ops/snapshots.md` step 5, then update guide 1b (URL height and the
  four values) and post height, hash and SHA-256 in that commit.
- The box bump (2.7), then `site/` copy that names the image.
- Post notice (c).

---

## 4. Kit gaps

K1, K2, K3, K7 and K12 were built on 2026-09-29 (branch `zebra-kit`; tests:
`infra/testnet/test/zebra-kit-stub.sh`, `test/byo-dry-run.sh`). The rest
are follow-ups.

- **K1. Done: `./deploy.sh --only <host> --zebra-only`.** One host, no pass
  1; on the host the health env, then `setup_zebrad` (pull, `zebrad.env`,
  `zebrad.toml`, restart if changed) and nothing else: no binaries, no
  `sova-node`, faucet, relayer, keeper or cloudflared step. The full
  `./deploy.sh` is unchanged (it gets K2 and K12 on a zebrad restart too).
- **K2. Done: the readiness wait** (`host/zebra-ready.sh`, run by
  `setup_zebrad` after every restart, both paths): RPC, image digest, NU7
  height, a launched state format upgrade finished, caught up (no more than
  `ZEBRA_READY_LAG` further behind the references than before the
  restart). Timeout `ZEBRA_READY_TIMEOUT_MIN` (15; 0 = off); a failure
  fails `setup-host.sh` and `deploy.sh`. Timings in
  `out/servers/<host>.zebra_ready`. A zebrad that wasn't answering before
  the restart (a new host) isn't held to a catch-up. `zebrad --version`
  isn't read: the digest check covers it.
- **K3. Done: `./smoke.sh hosts --only <host>`**, and per host: the running
  zebrad is `ZEBRA_IMAGE_DIGEST` (when pinned), and with
  `NU7_ACTIVATION_HEIGHT` set `upgrades["77190ad9"].activationheight`
  equals it.
- **K4. F1 isn't scripted.** `nu7-burn.sh` tears its node down before a
  pool check could run, and `recent_blocks_pass_the_strict_checks` scans
  `tip − 200` (needs a from/to override for short regtest chains). A
  `--pools` step would replace 2.3's manual block.
- **K5. Zebra image verification isn't scripted**: digest lookup,
  `gh attestation verify` with the tag check, `zebrad --version`, the
  ephemeral-testnet NU7-height probe. A `zebra-verify.sh <tag> <H7>` that
  prints the two `config.env` lines would make 2.2 one command.
- **K6. `seeds.json` publishes `zebra_image` without a digest**, and the
  guide runs a tag. Add `zebra_image_digest`, and let the guide pin
  `zfnd/zebra@sha256:…`.
- **K7. Done: `./deploy.sh keeper-pause` / `keeper-resume`**
  (`host/maint.sh`; marker `/etc/sova/.keeper-paused`, respected by
  `setup-host.sh` and, through `ConditionPathExists`, by systemd; health
  reports "keeper paused (planned)"). Still open: an automatic burner stop
  on `zcash_fork` on the keeper host.
- **K8. `publish.sh snapshot`** has no test-restore (snapshots.md step 5)
  and doesn't update the guide's four values; both are manual.
- **K9. `infra/testnet/published/`** drifts from what is live (v0.1.7
  committed vs v0.1.14 live): `publish.sh join` could copy there.
- **K10. No Docker-free zebrad path on the hosts.** Zebra pushes Docker
  Hub images only for full releases. If NU7 ships as a pre-release (or
  the image lags), the hosts have no route to its cosign-signed Linux
  tarball. A `ZEBRA_BINARY_URL` + `SHA256SUMS.sigstore.json` path, or a
  documented local image build from the tag, would cover it.
- **K11. The laptop zebrad is hand-run** (source build, internal miner
  on, `nohup`), so every restart is a Rob step, and a forgotten upgrade
  makes it an old-rules miner. A launchd job, or turning the internal
  miner off around network upgrades, would remove both.
- **K12. Done: the planned-maintenance mute** (`/etc/sova/.mute-until`,
  epoch seconds + reason): `health.sh` logs every finding but sends no
  Telegram until it expires, writes no dedupe stamp meanwhile, and removes
  the marker on the first pass after. Every zebrad restart sets
  `ZEBRA_RESTART_MUTE_MIN` (15) on its host; by hand, `./deploy.sh mute
  <1..240> [--only <host>]` / `unmute`. Other hosts' network alerts are not
  muted by it.

---

## Notices (drafts, Rob posts)

Rob posts these in t.me/sovazec. Fill in `<REL>`, `<DIGEST>`, `<H7>`,
`<S7>` from section 0. Each assumes the step before it is done.

### (a) ~10-03, after 1.1: "upgrade to v0.1.16 before 10-06"

> **Sova testnet: v0.1.16 is out. Upgrade before Oct 6.**
>
> Zcash testnet turns on NU7 on Oct 6: 25 s blocks, and a new consensus
> branch. After it, only burns signed for the new branch get mined.
> v0.1.16 signs for whatever branch your zebrad says comes next, so it
> burns straight across the switch. Older sova-miner burns will be
> rejected.
>
> Running a node or miner from the join guide:
>
> 1. Stop `sova-miner` and `sova` (ctrl-c).
> 2. Get v0.1.16:
> ```
> cd ~/.sova-testnet
> TAG=v0.1.16
> PLATFORM=linux-x86_64          # or darwin-arm64
> BASE=https://github.com/sova-chain/sova/releases/download/$TAG
> curl -fLO "$BASE/SHA256SUMS"
> curl -fLO "$BASE/sova-box-bin-$PLATFORM.tar.gz"
> grep " sova-box-bin-$PLATFORM.tar.gz" SHA256SUMS | sha256sum -c -   # macOS: shasum -a 256 -c -
> mkdir -p release && tar -xzf "sova-box-bin-$PLATFORM.tar.gz" -C release
> (cd release && sha256sum -c SHA256SUMS)                            # macOS: shasum -a 256 -c SHA256SUMS
> install -m 0755 release/sova release/sova-miner bin/
> ```
> 3. Start `sova` again (`. ./testnet.env && sova`), then `sova-miner`
>    with the same flags.
> 4. The miner's first lines should say `on branch 37a5165b (Nu6_3)`.
>
> Same chain, same genesis, same keystore. No reset.
>
> Next: zebrad, on Oct 5, when Zebra ships its NU7 release. We'll post
> the exact tag.

### (b) ~10-05, after 2.5 and 2.7: "update zebrad today"

> **Zcash testnet NU7 is tomorrow, at block <H7>. Update zebrad to <REL>
> today.** A zebrad that isn't updated stops at block <H7 − 1>, and your
> Sova node stops with it.
>
> From the join guide (stop `sova-miner` first; `sova` can keep running,
> it waits for zebrad):
> ```
> docker pull zfnd/zebra:<REL>
> docker stop -t 110 zebrad && docker rm zebrad
> docker run -d --name zebrad --restart unless-stopped \
>   -p 127.0.0.1:18232:18232 -p 18233:18233 \
>   -v "$HOME/.sova-testnet/zebrad.toml:/home/zebra/.config/zebrad.toml:ro" \
>   -v "$HOME/.sova-testnet/zebrad-state:/var/lib/sova/zebrad" \
>   -e RUST_LOG=info \
>   zfnd/zebra:<REL>
> ```
> Your state carries over. No resync. Check it knows NU7:
> ```
> curl -s -H 'Content-Type: application/json' \
>   --data '{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}' \
>   http://127.0.0.1:18232 | jq '.result.upgrades["77190ad9"]'
> ```
> It should say `"name": "NU7"` and `"activationheight": <H7>`. Once
> `blocks` is back at the tip, start `sova-miner` again.
>
> Image digest: `zfnd/zebra@<DIGEST>`. Also need v0.1.16 of `sova` and
> `sova-miner` (posted Oct 3).

(Keep "No resync" only if 2.2d found 28.x in place.)

### (c) ~10-06, after 3.2: "done"

> **NU7 is live on Zcash testnet. Sova followed it.**
>
> - First NU7 Zcash block: <H7>. It anchors Sova block <S7>.
> - Zcash testnet now targets 25 s blocks (was 75 s). One Sova block per
>   Zcash block, so about 3× more Sova blocks a day.
> - Burns and drips are signed for NU7 (branch `77190ad9`). Burns expire
>   after 120 blocks now, about the same 50 minutes as before.
> - Testnet still pays a flat 6,250 SOVA per block. Mainnet follows ZIP
>   218 (SIP-3 revision 2).
>
> Updated zebrad to <REL> and sova-miner to v0.1.16? Nothing to do.
> Still on the old zebrad? Your node stopped at <H7 − 1>. Update it (the
> Oct 5 post) and it catches up.
