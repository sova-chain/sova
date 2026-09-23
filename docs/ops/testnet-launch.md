# M1 public testnet: launch checklist

M1 is a public Sova testnet anchored to Zcash testnet that strangers can
mine. The infrastructure is the one Rob signed off in
`docs/design/infra-m1.md` (infra-2: Hetzner + Cloudflare), except the
keeper miner, which runs on AWS (Rob, 2026-09-23; `docs/ops/keeper-aws.md`).
All of it is
**courtesy infra, never load-bearing**: bootnodes, a public RPC, a TAZ
faucet and snapshot downloads. Consensus doesn't depend on any of it.

**How launch day works:** Rob does the **[Rob]** steps below (accounts,
tokens, a few public values), then the orchestrator runs **one command**,
`infra/testnet/launch.sh`, re-running it after each wait. Every other step
is a script in `infra/testnet/`. None of them hold a secret: tokens come
from `~/.config/sova-testnet/secrets.env` (outside the repo, mode 600), and
keys are listed in `docs/ops/wallets.md`.

Legend: **[Rob]** = Rob, by hand, with exactly what to hand back.
Everything else is a command, run from `infra/testnet/`.

---

## 0. Decisions [Rob]

The kit runs with the values below. Each one is a single line in
`config.env`. Rows marked **Decided** are settled. Change an open one
before step 1.

| Decision | Options | Kit value | Where |
| --- | --- | --- | --- |
| **Emission schedule** | `flat` (a constant 6,250 SOVA per epoch) or `sip3` (SIP-3's schedule) | **Decided 2026-09-23: `flat`.** SIP-3's schedule is for mainnet. | `SOVA_EMISSION_SCHEDULE` |
| **Chain ID at the reset** | Keep **82330**: the new genesis already gets a new fork ID, and chainlist doesn't change. Or take a fresh ID (e.g. 82331), so wallets don't mix up the old chain's nonces and history with the new one's. | **Decided 2026-09-23: keep 82330.** | `SOVA_CHAIN_ID`, plus `chain.rs` at the reset |
| **Keeper host** | Without a mine-mode node somewhere, the chain doesn't advance (see B5b). | **Decided 2026-09-23: AWS EC2** (`t3a.large`, Frankfurt, ≈ $70/month [est]), a bring-your-own host the kit adopts over SSH. Rob's step R3b, `docs/ops/keeper-aws.md`. The seed and RPC stay on Hetzner. | `SERVERS` (`sova-keeper-1:byo:<Elastic IP>:40:keeper:ubuntu`) |
| **Volume sizes** | Testnet zebrad state is 12 GB today, so small volumes still leave room for a year. | **Decided 2026-09-23: seed 60 GB, RPC 40 GB, keeper 40 GB** (the keeper's is its AWS gp3 root disk). The faucet wasn't part of the decision: it's set to **40 GB** to match. | `SERVERS` (4th field) |
| **Ashwings prices** | Mint price in SOVA (wei) and in ZEC (zatoshis) | **Decided 2026-09-23:** 625 SOVA, 0.05 ZEC; payee = a project testnet key (`tmQKm7CN5LaVg83YNRzXPLNy1qBMs8qcqNz`, keystore on the orchestrator's SSD), Rob's own t-address before mainnet | `ASHWINGS_PRICE_WEI`, `ASHWINGS_PRICE_ZAT`, `ASHWINGS_ZEC_PAYEE` |
| **Market fee** | basis points | 100 (1%) | `MARKET_FEE_BPS` |
| **Release tag** | The `vX.Y.Z` that goes live | `v0.1.0` | `SOVA_RELEASE_TAG` |

The epoch base B isn't a decision: `launch.sh` computes it at the moment
the chain starts (step 6).

---

## A. Rob's steps (one sitting, about 75 minutes with the AWS keeper)

Only Rob creates accounts or spends money.

**R1. Decisions.** Answer the open rows of the table in section 0. The
emission schedule, chain ID, keeper host and volume sizes were decided on
2026-09-23.
*Hand back:* your choice for each open row, or "defaults".

**R2. Domain (the ops-1 gate).** In Cloudflare → Websites, check that
**sova.io** is listed as *Active*. If it isn't, click *Add a site*, then
set the registrar's nameservers to the two that Cloudflare shows. Then open
sova.io → DNS → Records and search for `testnet`. Delete any old record
there, and any `*` wildcard record: the kit creates its own names (see
"DNS records" below).
*Hand back:* "sova.io is active on Cloudflare, no testnet records". If
you'd rather use a different domain, give its name; the orchestrator
changes five lines in `config.env`.

**R3. Hetzner Cloud** (the seed and RPC servers, and the faucet; the keeper
is on AWS, R3b). Set it up with hardware 2FA and the project email, with
you as sole owner (D4).
1. Create the account and add a payment method.
2. Create a Cloud project named `sova-testnet`.
3. In the project, open Security → API Tokens → Generate API token.
   Choose **Read & Write**, name it `sova-testnet-kit`, and save it in the
   password manager.
*Hand back:* nothing in chat. The token goes into `secrets.env` (R6).

**R3b. AWS: the keeper instance** (Rob, 2026-09-23; about 30 minutes). Do
R5 first, since this step uses the SSH key. The click-by-click steps are
`docs/ops/keeper-aws.md`, section A. In short, in the AWS console, region
**Europe (Frankfurt) eu-central-1**:
1. Account hygiene: MFA on the root user, a **$100 monthly budget**
   alert, and no access keys (the kit never calls AWS).
2. *EC2 → Key Pairs → Import key pair* `sova-testnet-admin`: paste
   `~/.ssh/sova_testnet_ed25519.pub` (the public key only).
3. *EC2 → Security Groups → Create* `sova-testnet-keeper`. Inbound:
   **SSH tcp/22 from My IP** and ICMP echo from anywhere, nothing else.
   Outbound: the default (all).
4. *EC2 → Launch instance* `sova-keeper-1`: **Ubuntu Server 24.04 LTS,
   64-bit (x86)**, **t3a.large**, key pair `sova-testnet-admin`, security
   group `sova-testnet-keeper`, **40 GiB gp3** (encrypted), termination
   protection on, credit specification *Unlimited*, user data empty.
5. *EC2 → Elastic IPs → Allocate*, then *Associate* it with
   `sova-keeper-1`.
*Hand back:* **the Elastic IP** (public, so chat is fine). Nothing else.

**R4. Cloudflare.**
1. **R2:** open R2 in the sidebar, enable it and accept the terms (the
   free tier needs a card on file). Create the bucket **`sova-testnet-dl`**
   (Location: automatic).
2. **R2 token:** R2 → Manage R2 API Tokens → Create API token. Set
   **Object Read & Write**, applied to **`sova-testnet-dl` only**. Save the
   *Access Key ID* and the *Secret Access Key*.
3. **API token:** My Profile → API Tokens → Create Token → *Create
   Custom Token*. Give it exactly these permissions:

   | Scope | Permission | Level | Used for |
   | --- | --- | --- | --- |
   | Account | Cloudflare Tunnel | Edit | the rpc and faucet tunnels |
   | Account | Workers Scripts | Edit | the RPC firewall Worker |
   | Account | Workers R2 Storage | Edit | the bucket's `dl.` custom domain |
   | Zone | DNS | Edit | the seed, rpc and faucet records |
   | Zone | Zone WAF | Edit | the rate-limit rule |
   | Zone | Workers Routes | Edit | `rpc.testnet.sova.io/*` → the Worker |

   Set Account Resources to *Include → your account*, and Zone Resources
   to *Include → Specific zone → sova.io*. A 30-day expiry is fine: the
   token is only needed at launch and at resets.
4. Copy the **Account ID** and the **Zone ID** from sova.io → Overview
   (right-hand column).
*Hand back:* nothing in chat. All five values go into `secrets.env` (R6).

**R5. SSH key** (on the machine that runs the kit):
```bash
ssh-keygen -t ed25519 -f ~/.ssh/sova_testnet_ed25519 -C sova-testnet-admin
```
*Hand back:* nothing (that is the path the kit expects).

**R6. Fill in the secrets file.** Do this without pasting anything into chat:
```bash
mkdir -p ~/.config/sova-testnet
install -m 600 infra/testnet/secrets.env.example ~/.config/sova-testnet/secrets.env
open -e ~/.config/sova-testnet/secrets.env   # fill in the values, save
```
*Hand back:* "secrets.env is filled".

**R7. (Optional) Phone alerts.** In Telegram, message @BotFather →
`/newbot`, then add the bot token and your chat ID to the same file.
*Hand back:* "Telegram set", or skip this step.

**R8. Public wallet values** (`docs/ops/wallets.md`). These are public
addresses and numbers, so they're fine to give in chat and to commit:
- Treasury: **done**, `0x8d0123637062f8c15FD6AFDe6F834C85f4AfeEDE`
  (your wallet; a multisig later).
- ZEC payee: **done for testnet**, a project testnet key
  (`tmQKm7CN5LaVg83YNRzXPLNy1qBMs8qcqNz`). Before mainnet, give your own
  mainnet t-address (`t1…`).
- Prices: **done**, 625 SOVA and 0.05 ZEC.

**R9. Announcement.** It's your call, after the "done" proof (step 9),
under the partner-outreach rule. The orchestrator doesn't post anything.

---

## B. The command (orchestrator)

### B0. Before launch day (no money moves)
1. **Release.** Sync `release` to the public repo, then tag and push. This
   runs `box-binaries.yml`, which attaches `sova-box-bin-linux-x86_64.tar.gz`
   and `SHA256SUMS` (`docs/ops/public-repo.md`):
   ```bash
   scripts/sync-public.sh /path/to/sova-public && git -C /path/to/sova-public push origin main
   git -C /path/to/sova-public tag -a vX.Y.Z -m vX.Y.Z && git -C /path/to/sova-public push origin vX.Y.Z
   ```
   Set `SOVA_RELEASE_TAG` in `config.env` (`cp config.env.example config.env` first).
2. **Pin zebra.** Run `docker pull zfnd/zebra:6.3.0` and put the digest it
   prints into `ZEBRA_IMAGE_DIGEST`.
3. **Check it all offline:**
   ```bash
   ./deploy.sh check                     # config.env: errors stop, open items are listed
   ./deploy.sh render --systemd-verify   # every host's files, linted and systemd-verified
   ./launch.sh --dry-run                 # the whole launch, printed, nothing touched
   ./test/byo-dry-run.sh --systemd-verify  # the AWS keeper renders and is in every launch stage
   ```

### B1–B8. Launch: `./launch.sh`, re-run until it finishes

```bash
./launch.sh          # runs until the next wait, then says why it stopped
./launch.sh --go     # the same, and it is allowed to start the chain (pin B)
```

| Step | What `launch.sh` runs | Stops when |
| --- | --- | --- |
| 1 check | `deploy.sh check`, and checks that the four cloud tokens are in the environment | config error, missing token |
| 2 provision | `provision.sh up --my-ip`: on Hetzner, the SSH key, two firewalls, volumes and servers (SSH is allowed only from this machine's IP). The AWS keeper is **adopted**: the kit logs in as `ubuntu`, applies the same `cloud-init.yaml` base (`host/byo-bootstrap.py`), then uses `sova-admin` like everywhere else | the keeper's IP is still a placeholder, or SSH to it fails (its security group) |
| 3 hosts | `deploy.sh`: node keys and enodes, zebrad (pinned image), `sova` from the tagged Release (SHA256SUMS checked), the faucet, the keeper, cloudflared, units, the health timer | — |
| 4 edge | `cloudflare.sh all` (DNS, tunnels, Worker, rate limit, R2), then `deploy.sh alerts` if Telegram is set | — |
| 5 sync | reads every host's zebrad | **any zebrad is still syncing** (about half a day from zero) |
| 6 chain | `epoch-base.sh propose` → `pin B` → `deploy.sh` (the sova nodes start) → `bootnodes.sh` + `--verify` → `publish.sh join` | **without `--go`**: it prints B and stops. Pinning B starts this genesis |
| 7 contracts | `deploy-contracts.sh keygen` (once), then `deploy --via sova-rpc-1` | **the deployer holds no SOVA yet** (fund it: B5c), or a constructor value is missing (R8) |
| 8 smoke | `smoke.sh all` (edge, hosts, contracts) | — |

How B is chosen: it is the next multiple of 100 at least 48 Zcash blocks
(about 1 h) past the tip. That makes it round and announceable, and every
node can be up before block B exists. Burn-less epochs are filled by
rewardless cadence blocks from any mine-mode node (the keeper), so the
chain moves as soon as one sealer runs. Bootnodes travel as
`SOVA_BOOTNODES`: each seed gets the *other* seeds and every other host
gets all of them. `testnet.env` and `seeds.json` hand the list to
strangers. Paste the Rust constant that `bootnodes.sh` prints into
`chain.rs` for the next release.

### B5. The steps that move TAZ or SOVA (between runs)
These need a transfer, so `launch.sh` doesn't do them.

a. **Faucet.** Send a few days of drips (the balance guard caps it at 10
   TAZ) as a **plain transfer** to the t-addr in
   `out/servers/sova-faucet-1.faucet_taddr`, from the laptop's shielded
   wallet (`zcash-devtool`, infra-1). Coinbase can't fund drips on testnet
   (`docs/ops/faucet.md`, "Funding").

b. **Keeper (D8, disclosed).** Its `sova-node` runs in mine mode as soon
   as B is pinned: it seals burn epochs when it ranks and rewardless
   cadence blocks otherwise. Publish the disclosure
   (`docs/ops/keeper-miner.md`, with the addresses from
   `out/servers/sova-keeper-1.keeper_*`). Fund the t-addr with a plain
   transfer, then run `ssh … sudo systemctl start sova-keeper`.

c. **Deployer.** Mine into it; the recipe is in `docs/ops/wallets.md`,
   "Deployer". That's also the stranger test's burn (9c). Then re-run
   `./launch.sh --go`, and step 7 deploys the contracts.

d. **Epoch base record.** Once block B exists, run `./epoch-base.sh record
   --via sova-seed-1`. Cross-check the hash on a Zcash testnet explorer.

e. **Snapshot.** Run `./publish.sh snapshot`. Post the height, hash and
   SHA-256 outside the bucket too (`docs/ops/snapshots.md`).

Commit `out/seeds.json`, `out/testnet.env`, `out/epoch-base.json` and
`deployments/sova-testnet.json`, so the canonical copies aren't only in
our bucket.

### DNS records (testnet subdomains of sova.io)

`cloudflare.sh` creates every one of these, so Rob creates none by hand.
R2 only makes sure the names are free.

| Name | Type | Points to | Proxy | Made by |
| --- | --- | --- | --- | --- |
| `seed-1.testnet.sova.io` | A + AAAA | `sova-seed-1`'s IPv4/IPv6 | **DNS only** (P2P can't be proxied, so seed IPs are public) | `cloudflare.sh dns` |
| `rpc.testnet.sova.io` | CNAME | `<tunnel-id>.cfargotunnel.com` | proxied | `cloudflare.sh tunnels` |
| `rpc.testnet.sova.io/*` | Worker route | `sova-testnet-rpc-firewall` | — | `cloudflare.sh worker` |
| `faucet.testnet.sova.io` | CNAME | `<tunnel-id>.cfargotunnel.com` | proxied | `cloudflare.sh tunnels` |
| `dl.testnet.sova.io` | R2 custom domain (Cloudflare manages the record) | bucket `sova-testnet-dl` | proxied | `cloudflare.sh r2` |

More seeds get `seed-2.`, `seed-3.` and so on. The HTTP RPC (8545),
authrpc (8551), zebrad RPC (18232) and faucet (18790) bind to 127.0.0.1
on every host. They're open in no firewall. The RPC and the faucet
reach the internet only through the outbound tunnel. `smoke.sh edge`
checks this from outside.

### 9. Done: the proof

M1 is live when all four of these pass, run from a machine that isn't
ours. The laptop plays the stranger. It already has a synced testnet
zebrad on 127.0.0.1:18234.

a. **A stranger's node syncs.** Download the release tarball and verify
   `SHA256SUMS`. Then `source` the `testnet.env` from
   `https://dl.testnet.sova.io/testnet.env`, set `SOVA_DATADIR` to a
   fresh directory and `SOVA_ZEBRAD_RPC=http://127.0.0.1:18234`, and run
   `sova`. Pass: its log shows `p2p: discovery on … bootnode(s)`, `sova/1:
   peer active` and `expectations: enforcing settlements`, and
   `cast block latest --field hash` matches between it and
   `https://rpc.testnet.sova.io` at the same height.

b. **A faucet drip.** Make a fresh address with `sova-miner --network test
   --data-dir /tmp/x init`, then run `curl -s -X POST -d
   '{"address":"tm…"}' https://faucet.testnet.sova.io/drip`. Pass: you
   get a txid, it confirms within a block or two, and a second drip to
   the same address gets `429 address_cooldown`.

c. **A burn mints.** Use the laptop miner from B5c:
   `sova-miner --network test --data-dir ~/.sova-testnet-launch-miner
   mine --rpc http://127.0.0.1:18234 --per-epoch-zat 10000 --budget-zat
   100000`. Pass: `./smoke.sh balance <deployer address>` sees the SOVA
   through the public RPC.

d. **The day-one contracts answer.** Run `./smoke.sh contracts`. Pass:
   each of WSOVA, the factory, the router, Multicall3, Ashwings (and the
   market) shows `ok`. That means the code matches the release build and
   the getters (`factory()`, `WETH()`, `treasury()`, …) return the
   recorded values, all through `https://rpc.testnet.sova.io`.

Also: `./smoke.sh all` shows 0 failed. Still open for M1 after launch
(infra-m1 §5): the 24 h switch-off drill, with our boxes **and the
keeper** dark; at least 2 non-project seeds listed; Otterscan on Pages;
the gitleaks CI scan.

---

## Reference

### The shape

| Server | Type / location [est] | Runs | Inbound |
| --- | --- | --- | --- |
| `sova-seed-1` | Hetzner CX43, fsn1, 60 GB volume | zebrad, sova (follow-only, C5-enforcing) | SSH (admin IPs), Sova P2P 30303 tcp+udp, Zcash P2P 18233 |
| `sova-rpc-1` | Hetzner CX43, nbg1, 40 GB volume | zebrad, sova (follow-only, `SOVA_RPC_PROFILE=public`), cloudflared → `rpc.testnet.sova.io` | SSH only |
| `sova-faucet-1` | Hetzner CX33, hel1, 40 GB volume | zebrad, `sova-faucet` (its own hot key, D5), cloudflared → `faucet.testnet.sova.io` | SSH only |
| `sova-keeper-1` | **AWS EC2** t3a.large, eu-central-1, 40 GB gp3 root disk, Elastic IP (byo: Rob creates it, the kit adopts it) | zebrad, sova in **mine** mode, `sova-keeper` (disclosed, D8) | SSH only (admin IP), via its security group `sova-testnet-keeper` |

| Script | What it does |
| --- | --- |
| `launch.sh` | The launch-day sequence below, re-runnable, `--dry-run`, `--go` |
| `provision.sh` | Hetzner: SSH key, two firewalls, volumes and servers (`hcloud`). Byo hosts (the AWS keeper): adopted over SSH. Idempotent, `--dry-run` |
| `host/cloud-init.yaml` | First boot: admin user, SSH hardening, ufw, unattended upgrades, journald caps, Docker |
| `host/byo-bootstrap.py` | Applies that same `cloud-init.yaml` over SSH to a byo host, so it matches a Hetzner one |
| `test/byo-dry-run.sh` | Offline proof that a byo keeper validates, renders and appears in every launch stage |
| `deploy.sh` → `host/setup-host.sh` | Per-role setup over SSH. `deploy.sh check` validates the config, `deploy.sh render` writes and lints every host's files locally |
| `cloudflare.sh` | DNS, tunnels, the RPC firewall Worker, the rate-limit rule, R2 bucket + domain, teardown |
| `epoch-base.sh` | Proposes, pins and records the epoch base B |
| `bootnodes.sh` | The final bootnode list, `testnet.env`, `seeds.json`, the `chain.rs` constant, `--verify` |
| `deploy-contracts.sh` | The day-one contracts: `keygen`, `plan`, `deploy`, `verify` (runs `contracts/script/deploy-kit.sh`, the same code as `box/deploy-dapps.sh`) |
| `publish.sh` | Join files and zebrad snapshots to R2 |
| `smoke.sh` | Edge, host, mint and contract checks |

**Budget** [est, 2026-09-22/23 prices; re-check at order time, because
Hetzner repriced three times in 2026]: 2 × CX43 ≈ €33, the CX33 faucet
≈ €8–10, the volumes (60 + 40 + 40 GB at €0.0572/GB) ≈ €8, so Hetzner
≈ €50/month. The AWS keeper (t3a.large + 40 GB gp3 + Elastic IP) is
≈ $70/month, plus ~$3 once for the initial sync
(`docs/ops/keeper-aws.md`, "Cost"). Cloudflare is $0 ($5 if the RPC
exceeds 100k requests a day), and R2 is under $1. **Total ≈ €50 + $70 a
month.** Traffic: 20 TB per server is included on Hetzner in the EU. On
AWS, the first 100 GB out per month is free. Neither has a hard spend cap:
the project's server limit is the ceiling on Hetzner, and the $100 budget
alert (R3b) on AWS.

### Monitoring and alerts

`sova-health.timer` runs `host/health.sh` every 2 minutes on every host.
Findings go to the journal (`journalctl -t sova-health`). With
`deploy.sh alerts`, they also go to Telegram, at most once an hour per
alert.

| Alert | Fires when | Meaning |
| --- | --- | --- |
| `disk_*` | `/` or `/var/lib/sova` ≥ 80% | Grow the volume (`hcloud volume resize`, then `resize2fs`; the AWS keeper: `docs/ops/keeper-aws.md`, "Operating it") |
| `zebrad_down`, `zebrad_lag` | RPC dead, or more than 20 blocks behind `estimatedheight` | Our Zcash view is stale, so C5 stalls |
| `sova_down` | Unit or RPC down | |
| `epoch_lag` | (zebrad tip − B + 1) − sova head > 10 epochs (~12 min) | **"WE LAG (infra)"**: the reference node (public RPC) is ahead of us, so it's our problem. **"NETWORK STALLED (miner matter)"**: the reference is stuck too, and nobody is sealing. That's not an infra failure; check the keeper. On `rpc-1` itself there is no reference, so the alert says it can't tell. |
| `c5_reject` | Any `settlement mismatch` in the last 3 minutes | A peer offered a block that contradicts our zebrad. Investigate: a doctored snapshot, a Zcash fork, or a bad sealer. |
| `faucet_down`, `faucet_dry`, `faucet_over` | `/status` dead, not accepting drips, or the hot wallet is over its limit | Top up (plain transfer), or stop topping up |

### Rollback and teardown

- **Bad release:** set `SOVA_RELEASE_TAG` back to the previous tag and
  run `./deploy.sh`. Binaries are kept per tag under
  `/usr/local/lib/sova/<tag>/`, and the symlink is switched. This is safe
  unless the new release changed consensus (then see the reset below).
- **One bad host:** `hcloud server delete <name>`, then
  `./provision.sh up` (the volume re-attaches, so zebrad state and the
  node key survive and the enode doesn't change), then
  `./deploy.sh --only <name>`. For the AWS keeper: Rob rebuilds it with
  the same Elastic IP (`docs/ops/keeper-aws.md`, "Rebuild"), then
  `ssh-keygen -R <ip> -f out/known_hosts`, `./provision.sh up` and
  `./deploy.sh --only sova-keeper-1`. That's a new keeper key, so it also
  needs a new disclosure.
- **Edge off:** `./cloudflare.sh teardown`. This removes the DNS records,
  tunnels, Worker, route and rate-limit rule. It keeps the R2 bucket.
- **Everything off:** `./cloudflare.sh teardown`, then
  `./provision.sh teardown` (add `--keep-volumes` to keep the zebrad
  state). Both ask you to type the label, and they only touch resources
  labelled `project=sova-testnet`. Neither touches the AWS keeper: Rob
  terminates it and **releases its Elastic IP** in the console.
- **Emergency "project goes dark":** `systemctl stop sova-node` on our
  hosts. The network is unaffected by design; this is the drill.

### Testnet reset (SIP-4, SIP-6 and SIP-7 activate here)

The M1 chain (genesis `sova-testnet-v0`) is abandoned at the reset. SIP-4
§1, SIP-6 and SIP-7 activate from the new genesis, with no fork logic.
Section 0 settled the schedule (`flat`) and the chain ID (82330) on
2026-09-23.

1. **Code on release:** SIP-4 §1, SIP-6 and SIP-7 merged;
   `SOVA_TESTNET_GENESIS_EXTRA_DATA` bumped to `sova-testnet-v1` (a new
   genesis hash and fork ID, so old nodes are filtered out by the ENR
   fork-ID check and the Status handshake); the pinned genesis-hash and
   fork-ID tests updated; SIP-7's `ZcashBlocks` predeploy added (code
   only, zero balance); `SOVA_TESTNET_BOOTNODES` refreshed. The chain ID
   stays 82330 (Rob, 2026-09-23).
2. **Tag** `vX+1` (B0).
3. **Announce** at least 48 h ahead: the new tag, the new genesis hash,
   and (SIP-6 §1.3) that miners must re-`init` their keystores.
4. **Roll out:** clear `SOVA_EPOCH_BASE` in `config.env`, set
   `SOVA_RELEASE_TAG`, and keep `SOVA_EMISSION_SCHEDULE=flat` (Rob,
   2026-09-23: SIP-3's schedule is for mainnet). On each node host,
   including the AWS keeper, wipe the Sova chain but **keep the node
   key**:
   ```bash
   ssh … 'sudo systemctl stop sova-node && sudo find /var/lib/sova/node -mindepth 1 -maxdepth 1 ! -name discovery-secret -exec rm -rf {} +'
   ```
   Move `deployments/sova-testnet.json` to `deployments/sova-testnet-v0.json`
   (`deploy-contracts.sh` refuses a record whose genesis differs), and
   `keygen` a new deployer. Then run `./launch.sh --go`: it pins the new B,
   rolls out, re-publishes the join files and redeploys the contracts.
   zebrad state is untouched: it's the same Zcash testnet.
5. **Keeper:** re-init it if SIP-6 requires it (new addresses mean a new
   disclosure). The faucet needs nothing: it's TAZ, unchanged.
6. **Done proof** again (step 9).

### Code prerequisites and known deviations

- **`SOVA_DATADIR` is on release.** With it set, the node keeps its
  datadir, uses production MDBX geometry, and its key lives at
  `<datadir>/discovery-secret`. **Caveat:** reth persists blocks to disk
  only once they are 50 behind the head, and `bin/sova` has no
  graceful-shutdown handler. So a restart drops up to ~50 recent blocks
  (about an hour of 75 s epochs) and re-fetches them from peers. That is
  harmless while any peer is up. The follow-ups are graceful shutdown and
  a lower persistence threshold for persistent datadirs.
- **B and the schedule are env vars, not profile constants.** A stranger
  who forgets `SOVA_EPOCH_BASE` gets the default 1 and never agrees with
  the network. The follow-up is to pin both in the `sova-testnet` profile
  in `chain.rs`, the same way bootnodes will be. Until then, `testnet.env`
  carries them.
- **zebrad cookie auth is off** (loopback-only RPC, single-purpose
  hosts), which deviates from infra-m1 §4.3. `bin/sova`'s zebrad client
  can't read a cookie file. The follow-up is `SOVA_ZEBRAD_COOKIE_FILE`.
- **`sova-faucet` isn't in the Release tarball.** `setup-host.sh` builds
  it from the tag on the faucet host (a few minutes) and checks that the
  tag's commit equals the Release's.
- **Deploy through the tunnel, not the edge.** The RPC Worker caps a
  request at 64 KB. The router's init code is 22 KB (44 KB as hex), so
  `deploy-contracts.sh deploy --via sova-rpc-1` sends through an SSH
  tunnel to the node's own loopback RPC. `verify` works through the edge.
- **`Runtime::test()`** still sizes `bin/sova`'s thread pools. That's fine
  for testnet load. Revisit before mainnet.
- **The free-plan rate-limit rule matches paths only** (`/` and
  `/drip`), zone-wide. On Pro it can match the hosts.
