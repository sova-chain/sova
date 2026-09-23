# M1 public testnet: launch runbook

M1 is a public Sova testnet anchored to Zcash testnet that strangers can
mine. The infrastructure is the one Rob signed off in
`docs/design/infra-m1.md` (infra-2: Hetzner + Cloudflare). All of it is
**courtesy infra, never load-bearing**: bootnodes, a public RPC, a TAZ
faucet and snapshot downloads. Consensus doesn't depend on any of it.

The kit is `infra/testnet/`. Rob does part A once, in one sitting. The
orchestrator does part B with the scripts. Nothing in the kit holds a
secret. Tokens come from the environment, or from one private file
outside the repo.

| Script | What it does |
| --- | --- |
| `provision.sh` | Hetzner: SSH key, two firewalls, volumes and servers (`hcloud` CLI), idempotent, `--dry-run` |
| `host/cloud-init.yaml` | First boot: admin user, SSH hardening, ufw, unattended upgrades, journald caps, Docker |
| `deploy.sh` → `host/setup-host.sh` | Per-role setup over SSH: zebrad (pinned image), `sova` from the tagged Release (SHA256SUMS checked), faucet, keeper, cloudflared, systemd units, health timer |
| `cloudflare.sh` | DNS, tunnels, RPC firewall Worker, rate-limit rule, R2 bucket + domain, teardown |
| `epoch-base.sh` | Proposes, pins and records the epoch base B |
| `bootnodes.sh` | Final bootnode list, `testnet.env`, `seeds.json`, the `chain.rs` constant, `--verify` |
| `publish.sh` | Join files and zebrad snapshots to R2 |
| `smoke.sh` | Edge, host and mint checks |

### The shape

| Server | Type / location [est] | Runs | Inbound |
| --- | --- | --- | --- |
| `sova-seed-1` | CX43, fsn1, 150 GB volume | zebrad, sova (follow-only, C5-enforcing) | SSH (admin IPs), Sova P2P 30303 tcp+udp, Zcash P2P 18233 |
| `sova-rpc-1` | CX43, nbg1, 100 GB volume | zebrad, sova (follow-only, `SOVA_RPC_PROFILE=public`), cloudflared → `rpc.testnet.sova.io` | SSH only |
| `sova-faucet-1` | CX33, hel1, 60 GB volume | zebrad, `sova-faucet` (its own hot key, D5), cloudflared → `faucet.testnet.sova.io` | SSH only |
| `sova-keeper-1` (optional) | CX33 | zebrad, sova in **mine** mode, `sova-keeper` (disclosed, D8) | SSH only |

The HTTP RPC (8545), authrpc (8551), zebrad RPC (18232) and faucet
(18790) bind to 127.0.0.1 on every host. They are opened in no firewall,
and the RPC and faucet reach the internet only through an outbound
Cloudflare Tunnel. `smoke.sh edge` checks this from outside.

---

## A. Rob's steps (one sitting, about 45 minutes)

Only Rob creates accounts or spends money. **Gate:** ops-1 must confirm
clean control of `sova.io` DNS (D3). If it can't, pick a fresh domain and
tell the orchestrator, who changes five names in `config.env`.

**A1. Hetzner Cloud** (hardware 2FA, the project email, Rob as sole owner, D4)
1. Create the account and add a payment method.
2. Create a Cloud project named `sova-testnet`.
3. In the project, go to Security → API Tokens → Generate. Choose
   **Read & Write** and name it `sova-testnet-kit`. Save it in the password
   manager.

**A2. Cloudflare**
1. Make sure the zone (`sova.io`) is on the Cloudflare account. If it
   isn't, Add a site, then change the nameservers at the registrar.
2. **R2** (UI only): open R2, enable it, and accept the terms. The free
   tier needs a card on file. Then create the bucket **`sova-testnet-dl`**
   (Location: automatic).
3. Create an **R2 API token**: R2 → Manage R2 API Tokens → Create. Set
   **Object Read & Write**, applied to **`sova-testnet-dl` only**. Save the
   Access Key ID and the Secret Access Key.
4. Create the **custom API token**: My Profile → API Tokens → Create
   Token → Custom. Give it exactly these permissions:

   | Scope | Permission | Level | Used for |
   | --- | --- | --- | --- |
   | Account | Cloudflare Tunnel | Edit | tunnels for rpc and faucet |
   | Account | Workers Scripts | Edit | the RPC firewall Worker |
   | Account | Workers R2 Storage | Edit | the bucket's custom domain `dl.` |
   | Zone | DNS | Edit | seed, rpc and faucet records |
   | Zone | Zone WAF | Edit | the rate-limit rule |
   | Zone | Workers Routes | Edit | `rpc.testnet.sova.io/*` → the Worker |

   Account Resources: *Include → your account*. Zone Resources: *Include →
   Specific zone → sova.io*. Optionally restrict it to your IP and give it
   a 30-day expiry (it is only needed at launch and for resets).
5. Note the **Account ID** and **Zone ID**. Both are in the zone's Overview
   page, right-hand column.

**A3. SSH key** (on this laptop)
```bash
ssh-keygen -t ed25519 -f ~/.ssh/sova_testnet_ed25519 -C sova-testnet-admin
```

**A4. Hand over** without pasting anything into chat. Put the values in
one private file that the scripts source and never print:
```bash
mkdir -p ~/.config/sova-testnet
install -m 600 infra/testnet/secrets.env.example ~/.config/sova-testnet/secrets.env
open -e ~/.config/sova-testnet/secrets.env   # fill in the values, save
```

**A5. (Optional) Phone alerts.** Create a Telegram bot with @BotFather,
then add its token and your chat ID to the same file.

**Budget** [est, 2026-09-22 prices, re-check at order time: Hetzner
repriced three times in 2026]:

| Item | Monthly |
| --- | --- |
| 2 × CX43 (€15.99 + €0.50 IPv4) | ≈ €33 |
| CX33 faucet host | ≈ €8–10 |
| Volumes: 150 + 100 + 60 GB at €0.0572/GB | ≈ €18 |
| Optional keeper: CX33 + 60 GB | ≈ €12 |
| Cloudflare: DNS, Tunnel, WAF rule, Workers free tier | $0 ($5 if RPC exceeds 100k requests/day) |
| R2: 10 GB free, then $0.015/GB-month, $0 egress | < $1 |
| **Total** | **≈ €60/month, or ≈ €72 with the keeper** |

The volumes are optional. With `volume_gb` set to 0, state lives on
local disk (160 GB on a CX43) for ≈ €42 plus the faucet host. You lose
the ability to rebuild a server without re-syncing. Traffic: 20 TB per
server is included in the EU. Hetzner has no hard spend cap, so the
project's server limit is the practical ceiling.

That's all for Rob until the announcement, which is his call (the
partner-outreach rule).

---

## B. Orchestrator's steps

All commands run from `infra/testnet/`. `config.env` is git-ignored:
copy it from `config.env.example`.

### B0. Before any money moves
1. **Land `SOVA_DATADIR` on release.** This is commit "bin/sova:
   SOVA_DATADIR" on `infra/m1-launch-kit`. Without it, every `bin/sova`
   start is a fresh chain in a temp dir with a **new node key**, so
   bootnodes change on every restart and a full restart of our nodes
   loses the chain. See "Code prerequisites" below.
2. Tag the release (`vX.Y.Z`) and wait for `box-binaries.yml` to attach
   `sova-box-bin-linux-x86_64.tar.gz` and `SHA256SUMS`. Set
   `SOVA_RELEASE_TAG` in `config.env`.
3. On launch day, `docker pull zfnd/zebra:6.3.0` and put its digest in
   `ZEBRA_IMAGE_DIGEST`.
4. Check the plan with nothing at stake:
   ```bash
   ./provision.sh up --dry-run --my-ip
   ./deploy.sh --dry-run
   ./cloudflare.sh --dry-run all
   ```

### B1. Provision (≈ 10 min)
```bash
./provision.sh up --my-ip   # SSH allowed from this machine's IP only
./provision.sh status
```
This creates the SSH key, `sova-testnet-fw-seed` and
`sova-testnet-fw-private`, the volumes and the servers, then waits for
cloud-init. Re-running it converges. If your IP changes, run
`./provision.sh ssh-allow <cidr>`.

### B2. Deploy (≈ 10 min, then zebrad syncs for hours)
```bash
./deploy.sh
```
Pass 1 makes each node's p2p key and records its enode. Pass 2 installs
everything. zebrad starts syncing Zcash testnet on every host. That takes
**about half a day** from zero. The sova nodes are installed but **don't
start until B is pinned** (the unit's `ConditionPathExists`). The faucet
prints its t-addr (`out/servers/sova-faucet-1.faucet_taddr`).

### B3. Edge (≈ 5 min)
```bash
./cloudflare.sh all      # dns tunnels worker ratelimit r2
./deploy.sh alerts       # if Telegram is set up
```

### B4. Pin B and start the chain (after zebrad is synced everywhere)

**How B is chosen.** B is the first Zcash testnet height that is a Sova
epoch: Sova block N settles Zcash block N + B − 1 (SIP-2). It is
consensus, so every node must run with the same `SOVA_EPOCH_BASE`. We
pick it **just ahead of the tip**: the next multiple of 100 at least 48
blocks (~1 h) out. That gives a round, announceable number, and every
node can be up before block B exists. Burn-less epochs are filled by
rewardless cadence blocks from any mine-mode node, so the chain moves as
soon as one sealer runs (the keeper, B6).

```bash
./epoch-base.sh propose --via sova-seed-1   # prints B and its ETA
./epoch-base.sh pin <B>                     # writes config.env
./deploy.sh                                 # rolls B out, starts sova-node
./bootnodes.sh                              # out/bootnodes.txt, testnet.env, seeds.json
```
Once block B exists, `./epoch-base.sh record --via sova-seed-1` writes
B's hash to `out/epoch-base.json`. Cross-check it against a Zcash testnet
explorer and publish it with the announcement.

**How bootnode enodes are pinned.** `bin/sova` resolves bootnodes as
`SOVA_BOOTNODES` if set, else the compiled `SOVA_TESTNET_BOOTNODES` in
`bin/sova/src/chain.rs`. That list is empty today, and it must always be
explicit so reth never falls back to Ethereum mainnet's list
(`discovery.rs` refuses to run discovery with an unpinned list). Each
node's key is `/var/lib/sova/node/discovery-secret` on its volume. It is
generated before the first start and stable across restarts, so the
enode is known before any node runs. For launch, the list travels as
`SOVA_BOOTNODES`: `deploy.sh` gives each seed the *other* seeds and
everyone else every seed, and `testnet.env` gives it to strangers. For
the next release, paste the Rust constant that `bootnodes.sh` prints into
`chain.rs`, so a plain `SOVA_CHAIN=sova-testnet` finds the network. No
chainspec field names our hosts (infra-m1 §3). Community seeds go into
`EXTRA_BOOTNODES` in `config.env` and into the next release's constant.

```bash
./bootnodes.sh --verify    # running node ids == recorded; seed P2P reachable
./publish.sh join          # seeds.json, testnet.env, bootnodes.txt -> dl.
```
Commit `out/seeds.json` and `out/testnet.env` into the repo's docs too, so
the canonical copy isn't only in our bucket.

### B5. Fund the faucet
Send a few days of drips (≤ 10 TAZ, the balance guard) as a **plain
transfer** to the faucet t-addr, from the laptop's shielded wallet
(`zcash-devtool`, infra-1). Coinbase can't fund drips on testnet
(`docs/ops/faucet.md`, "Funding").

### B6. Keeper (D8: disclosed, recommended for day-1 liveness)
The keeper host's `sova-node` runs in **mine mode**. It seals burn epochs
when it ranks, and it seals rewardless cadence blocks when nobody burned.
Our public boxes are follow-only and never seal. **Without at least one
mine-mode node somewhere, the chain doesn't advance.** Uncomment
`sova-keeper-1` in `config.env`, then run `provision.sh up` and
`deploy.sh`. Publish the disclosure (`docs/ops/keeper-miner.md`, with the
addresses from `out/servers/sova-keeper-1.keeper_*`). Fund the t-addr,
then run `ssh … sudo systemctl start sova-keeper`. The laptop can be the
keeper instead (see the open questions below).

### B7. Smoke test
```bash
./smoke.sh edge    # chain 82330, moving head, denylist, batch cap, faucet, ports
./smoke.sh hosts   # services, "enforcing settlements", zebrad synced, epoch lag, 0 C5 rejects
```
**Stranger test.** The laptop plays the stranger. It already has a
synced testnet zebrad and a funded miner (infra-1).
1. Download the release tarball and verify `SHA256SUMS`. Then run
   `source testnet.env` (from `https://dl.testnet.sova.io/testnet.env`),
   and set `SOVA_DATADIR` to a fresh directory, `SOVA_ZEBRAD_RPC` to the
   laptop zebrad (`http://127.0.0.1:18234`), and `SOVA_MINER_EVM_ADDRESS`
   to the miner's evm address. Run `sova`.
2. Check its log for `p2p: discovery on … 1 bootnode(s)`, `sova/1: peer
   active` (the seed) and `expectations: enforcing settlements`. Its head
   should match `rpc.testnet.sova.io`.
3. `sova-miner --network test --data-dir ~/.sova-testnet-miner mine --rpc
   http://127.0.0.1:18234 --per-epoch-zat 10000 --budget-zat 100000`
4. `./smoke.sh balance <evm address>`. It passes when the mint is
   visible through the public RPC.

**Faucet drip.** Make a fresh t-addr (`sova-miner --network test
--data-dir /tmp/x init`), then run `curl -s -X POST -d
'{"address":"tm…"}' https://faucet.testnet.sova.io/drip`. Expect a txid.
It is mined within a block or two, and a second drip to the same address
gets `429 address_cooldown`.

### B8. Snapshot
```bash
./publish.sh snapshot   # zebrad downtime on seed-1 = archive time
```
Post the height, hash and SHA-256 **outside the bucket** as well
(`docs/ops/snapshots.md`).

### B9. M1 gates still open after launch
These come from infra-m1 §5: the 24 h switch-off drill with our boxes
**and the keeper** dark, and at least 2 non-project seeds listed. Also
pending: Otterscan on Pages and the gitleaks CI scan.

---

## Monitoring and alerts

`sova-health.timer` runs `host/health.sh` every 2 minutes on every host.
Findings go to the journal (`journalctl -t sova-health`). With
`deploy.sh alerts`, they also go to Telegram, at most once an hour per
alert.

| Alert | Fires when | Meaning |
| --- | --- | --- |
| `disk_*` | `/` or `/var/lib/sova` ≥ 80% | Grow the volume (`hcloud volume resize`, then `resize2fs`) |
| `zebrad_down`, `zebrad_lag` | RPC dead, or more than 20 blocks behind `estimatedheight` | Our Zcash view is stale, so C5 stalls |
| `sova_down` | Unit or RPC down | |
| `epoch_lag` | (zebrad tip − B + 1) − sova head > 10 epochs (~12 min) | **"WE LAG (infra)"**: the reference node (public RPC) is ahead of us, so it's our problem. **"NETWORK STALLED (miner matter)"**: the reference is stuck too, and nobody is sealing. That's not an infra failure; check the keeper. On `rpc-1` itself there is no reference, so the alert says it can't tell. |
| `c5_reject` | Any `settlement mismatch` in the last 3 minutes | A peer offered a block that contradicts our zebrad. Investigate: a doctored snapshot, a Zcash fork, or a bad sealer. |
| `faucet_down`, `faucet_dry`, `faucet_over` | `/status` dead, not accepting drips, or the hot wallet is over its limit | Top up (plain transfer), or stop topping up |

From outside: run `./smoke.sh edge` from any machine. A Cloudflare Worker
cron probe is a follow-up.

## Rollback and teardown

- **Bad release:** set `SOVA_RELEASE_TAG` back to the previous tag and
  run `./deploy.sh`. Binaries are kept per tag under
  `/usr/local/lib/sova/<tag>/`, and the symlink is switched. This is safe
  unless the new release changed consensus (then see the reset procedure
  below).
- **One bad host:** rebuild it with `hcloud server rebuild <name> --image
  ubuntu-24.04`, then `./deploy.sh --only <name>`. The volume keeps
  zebrad state and the node key, so the enode is unchanged and nothing
  re-syncs. Note: rebuild **re-runs cloud-init only if user data is
  re-sent**. The simplest path is `hcloud server delete <name>`, then
  `./provision.sh up` (the volume re-attaches), then
  `./deploy.sh --only <name>`.
- **Edge off:** `./cloudflare.sh teardown`. This removes the DNS records,
  tunnels, Worker, route and rate-limit rule. It keeps the R2 bucket.
- **Everything off:** `./cloudflare.sh teardown`, then
  `./provision.sh teardown` (add `--keep-volumes` to keep the zebrad
  state). Both ask you to type the label. Teardown only touches resources
  labelled `project=sova-testnet`.
- **Emergency "project goes dark":** `systemctl stop sova-node` on our
  hosts. The network is unaffected by design; this is the drill.

## Testnet reset (SIP-4, SIP-6 and SIP-7 activate here)

The M1 chain (genesis `sova-testnet-v0`) is abandoned at the reset. SIP-4
§1, SIP-6 and SIP-7 activate from the new genesis, with no fork logic
(SIP-6 §7 and SIP-7 both say "at the testnet reset").

1. **Code on release:**
   - SIP-4 §1, SIP-6 and SIP-7 are merged.
   - `SOVA_TESTNET_GENESIS_EXTRA_DATA` is bumped to `sova-testnet-v1`.
     That gives a new genesis hash and a new fork ID, so old nodes are
     filtered out by the ENR fork-ID check and the Status handshake. No
     coordination is needed to split them off.
   - The pinned genesis-hash and fork-ID tests in `chain.rs` and
     `discovery.rs` are updated.
   - SIP-7's `ZcashBlocks` predeploy is added (code only, zero balance:
     "every wei traces to burned ZEC" still holds).
   - `SOVA_TESTNET_BOOTNODES` is refreshed.
2. **Tag** `vX+1` and let the Release build.
3. **Pick B′:** `./epoch-base.sh propose`. Announce the reset at least
   48 h ahead, with the new tag, B′, the new genesis hash, and (SIP-6
   §1.3) the note that miners must re-`init` their keystores.
4. **Roll out:** `./epoch-base.sh pin <B′>`, set `SOVA_RELEASE_TAG`, and
   use `SOVA_EMISSION_SCHEDULE=sip3` if M1 ran `flat`. Then, on each node
   host, wipe the Sova chain but **keep the node key**, so the enodes
   don't change:
   ```bash
   ssh … 'sudo systemctl stop sova-node && sudo find /var/lib/sova/node -mindepth 1 -maxdepth 1 ! -name discovery-secret -exec rm -rf {} +'
   ```
   Then run `./deploy.sh` and `./bootnodes.sh`, then `./publish.sh join`.
   zebrad state is untouched: it's the same Zcash testnet.
5. **Keeper:** re-init it if SIP-6 requires it (new addresses mean a new
   disclosure). The faucet needs nothing: it's TAZ, unchanged.
6. **Smoke** again (B7). Publish the new `seeds.json` and
   `epoch-base.json`.

## Code prerequisites and known deviations

- **`SOVA_DATADIR` (this branch).** `bin/sova` used reth's `testing_node`:
  a fresh temp datadir per start, a new p2p key each time, and a 64 MB
  test-database cap. With `SOVA_DATADIR` set, the node keeps its datadir,
  uses production MDBX geometry, and its key lives at
  `<datadir>/discovery-secret`. Unset, behavior is unchanged (box, sims,
  CI). **Must be on the launch tag.** Verified locally on a debug build.
  The enode was identical across a restart. A pre-generated key file was
  used as-is, and `host/enode.py` computed the same enode as reth. A dev
  chain resumed from disk after SIGTERM, and the unset path still runs
  ephemeral. **Caveat:** reth persists blocks to disk only once they are
  50 behind the head (`DEFAULT_PERSISTENCE_THRESHOLD`), and `bin/sova`
  has no graceful-shutdown handler: SIGTERM ends it, and SIGINT is
  ignored. So a restart drops up to ~50 recent blocks (about an hour of
  75 s epochs) and re-fetches them from peers. That is harmless while any
  peer is up. The follow-ups are graceful shutdown and a lower persistence
  threshold for persistent datadirs, both tested in the nightly sims.
- **B and the schedule are env vars, not profile constants.** A stranger
  who forgets `SOVA_EPOCH_BASE` gets the default 1 and never agrees with
  the network. The follow-up is to pin both in the `sova-testnet` profile
  in `chain.rs` at launch, the same way bootnodes will be. Until then,
  `testnet.env` carries them.
- **zebrad cookie auth is off** (loopback-only RPC, single-purpose
  hosts), which deviates from infra-m1 §4.3. `bin/sova`'s zebrad client
  can't read a cookie file, and zebrad rewrites the cookie on every
  start. The follow-up is a `SOVA_ZEBRAD_COOKIE_FILE`, after which cookie
  auth goes back on everywhere.
- **`sova-faucet` isn't in the Release tarball.** `setup-host.sh` builds
  it from the tag on the faucet host, a few minutes' work, and checks
  that the tag's commit equals the Release's. Adding it to
  `box-binaries.yml` also needs `box/up.sh`'s `sha256sum -c` to tolerate
  the extra file.
- **`Runtime::test()`** still sizes `bin/sova`'s thread pools (2 tokio
  workers, 2 rayon threads). That's fine for testnet load. Revisit before
  mainnet.
- **The free-plan rate-limit rule matches paths only** (`/` and
  `/drip`), zone-wide. On Pro it can match the hosts.
