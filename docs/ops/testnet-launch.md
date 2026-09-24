# M1 public testnet: launch checklist

M1 is a public Sova testnet anchored to Zcash testnet that strangers can
mine. The infrastructure is the one Rob signed off in
`docs/design/infra-m1.md` (infra-2: Hetzner + Cloudflare): all four
servers (seed, RPC, faucet, keeper) on Hetzner, Cloudflare for the edge,
one server account (Rob, 2026-09-23). All of it is
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
| **SIP-6 sealer signatures** | On from genesis, or off (debug only). A consensus switch: every node, ours and strangers', must use the same value, so `testnet.env` and `seeds.json` carry it. | **Decided: on** (SIP-6 is Accepted and activates at the reset, from genesis, SIP-6 §7). Every node requires sealed (97-byte `extraData`) or null (empty) blocks, and the keeper signs with its miner key (`docs/ops/keeper-miner.md`, "Sealing key (SIP-6)"). | `SOVA_SIP6=1` |
| **SIP-7 Zcash pool state** | On from genesis, or off (debug only). A consensus switch that is also in the genesis: with it on, the chain spec predeploys the `ZcashBlocks` contract at `0x…5A01`, which changes the genesis hash and fork ID. Every node, ours and strangers', must use the same value, so `testnet.env` and `seeds.json` carry it. | **SIP-7: on at the reset (Rob accepted SIP-7, 2026-09-23).** Contracts read Zcash pool totals, block stats and shielded flows (`0x…5A00`, `ZcashBlocks` at `0x…5A01`), and every node serves `sova_getZcashBlocks`. | `SOVA_SIP7=1` |
| **Chain ID at the reset** | Keep **82330**: the new genesis already gets a new fork ID, and chainlist doesn't change. Or take a fresh ID (e.g. 82331), so wallets don't mix up the old chain's nonces and history with the new one's. | **Decided 2026-09-23: keep 82330.** | `SOVA_CHAIN_ID`, plus `chain.rs` at the reset |
| **Servers** | Where the seed, RPC, faucet and keeper run | **Decided: all on Hetzner (Rob, 2026-09-23)**, one server account, Cloudflare for the edge. This replaces the same day's earlier "keeper on AWS". | `SERVERS` |
| **Keeper host** | Without a mine-mode node somewhere, the chain doesn't advance (see B5b). | **Decided 2026-09-23: Hetzner CX33** (fsn1, 40 GB volume), created by the kit like the other three. A keeper elsewhere stays possible as an option ("Optional: a keeper elsewhere", below R9). | `SERVERS` (`sova-keeper-1:cx33:fsn1:40:keeper`) |
| **Volume sizes** | Testnet zebrad state is 12 GB today, so small volumes still leave room for a year. | **Decided 2026-09-23: seed 60 GB, RPC 40 GB, keeper 40 GB** (Hetzner volumes). The faucet wasn't part of the decision: it's set to **40 GB** to match. | `SERVERS` (4th field) |
| **Ashwings prices** | Mint price in SOVA (wei) and in ZEC (zatoshis) | **Decided 2026-09-23:** 625 SOVA, 0.05 ZEC; payee = a project testnet key (`tmQKm7CN5LaVg83YNRzXPLNy1qBMs8qcqNz`, keystore on the orchestrator's SSD), Rob's own t-address before mainnet | `ASHWINGS_PRICE_WEI`, `ASHWINGS_PRICE_ZAT`, `ASHWINGS_ZEC_PAYEE` |
| **Market fee** | basis points | 100 (1%) | `MARKET_FEE_BPS` |
| **Release tag** | The `vX.Y.Z` that goes live | `v0.1.0` | `SOVA_RELEASE_TAG` |

The epoch base B isn't a decision: `launch.sh` computes it at the moment
the chain starts (step 6).

---

## A. Rob's steps (one sitting, about 45 minutes)

Only Rob creates accounts or spends money.

**R1. Decisions.** Answer the open rows of the table in section 0. The
emission schedule, chain ID, servers (all on Hetzner), keeper host and
volume sizes were decided on 2026-09-23.
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

**R3. Hetzner Cloud** (all four servers: seed, RPC, faucet and keeper; the
kit creates them, you only make the account and a token). Set it up with
hardware 2FA and the project email, with you as sole owner (D4).
1. Create the account and add a payment method.
2. Create a Cloud project named `sova-testnet`.
3. In the project, open Security → API Tokens → Generate API token.
   Choose **Read & Write**, name it `sova-testnet-kit`, and save it in the
   password manager.
*Hand back:* nothing in chat. The token goes into `secrets.env` (R6).

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
   | Account | Workers Scripts | Edit | the RPC firewall Worker and its rate-limit binding |
   | Account | Workers R2 Storage | Edit | the bucket's `dl.` custom domain |
   | Zone | DNS | Edit | the seed, rpc and faucet records |
   | Zone | Zone WAF | Edit | the faucet's rate-limit rule |
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

**Optional: a keeper elsewhere** (not a required step; not the default
since 2026-09-23). The kit can adopt a keeper that isn't on Hetzner (a
bring-your-own host, e.g. AWS or other hardware) over SSH, and sets it up
like any other host. The AWS runbook is `docs/ops/keeper-aws.md` (about 30
minutes, ≈ $70/month [est]); you hand back its IP, and the orchestrator
swaps the keeper's line in `config.env` for
`sova-keeper-1:byo:<ip>:40:keeper:ubuntu`. Offline proof that this path
still works: `./test/byo-dry-run.sh`.

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
   ./test/byo-dry-run.sh --systemd-verify  # the optional byo path (a keeper elsewhere) still works
   ```

### B1–B8. Launch: `./launch.sh`, re-run until it finishes

```bash
./launch.sh          # runs until the next wait, then says why it stopped
./launch.sh --go     # the same, and it is allowed to start the chain (pin B)
```

| Step | What `launch.sh` runs | Stops when |
| --- | --- | --- |
| 1 check | `deploy.sh check`, and checks that the four cloud tokens are in the environment | config error, missing token |
| 2 provision | `provision.sh up --my-ip`: on Hetzner, the SSH key, two firewalls, four volumes and four servers (seed, RPC, faucet, keeper; SSH is allowed only from this machine's IP). An optional byo host would be **adopted** here instead (`host/byo-bootstrap.py`); the default has none | a byo host's IP is still a placeholder, or SSH to it fails (its firewall); never with the default |
| 3 hosts | `deploy.sh`: node keys and enodes, zebrad (pinned image), `sova` from the tagged Release (SHA256SUMS checked), the faucet, the keeper, cloudflared, units, the health timer | — |
| 4 edge | `cloudflare.sh all` (DNS, tunnels, Worker + RPC rate limit, faucet WAF rule, R2), then `deploy.sh alerts` if Telegram is set | — |
| 5 sync | reads every host's zebrad | **any zebrad is still syncing** (about half a day from zero) |
| 6 chain | `epoch-base.sh propose` → `pin B` → `deploy.sh` (the sova nodes start; each node host records its `sova genesis-hash`) → `bootnodes.sh` (publishes that genesis hash) + `--verify` (each node's block 0 matches it) → `publish.sh join` | **without `--go`**: it prints B and stops. Pinning B starts this genesis |
| 7 contracts | `deploy-contracts.sh keygen` (once), then `deploy --via sova-rpc-1` | **the deployer holds no SOVA yet** (fund it: B5c), or a constructor value is missing (R8) |
| 8 smoke | `smoke.sh all` (edge, hosts, contracts) | — |

How B is chosen: it is the next multiple of 100 at least 48 Zcash blocks
(about 1 h) past the tip. That makes it round and announceable, and every
node can be up before block B exists. With SIP-6, a burn epoch is sealed
by a ranked burner's key, and a burn-less epoch gets its deterministic
null block, which any mine-mode node (the keeper) builds. So the chain
moves as soon as one sealer runs. Bootnodes travel as
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
   as B is pinned: it signs (SIP-6) the burn epochs where it ranks and
   builds null blocks otherwise. Its log says `sip-6: sealing as 0x…`,
   which must be the keeper's EVM address (`smoke.sh hosts` checks). Publish the disclosure
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

f. **Stranger guide.** Fill every `<<…>>` placeholder in
   `docs/guides/testnet.md` (its "Filled at launch" table names the
   script behind each), then wire the `data-placeholder="testnet"` spans
   in `site/src/pages/mine.astro` and `site/src/pages/node.astro` to it.
   Point the live pages at the network: `SOVA_RPC` in
   `site/src/data/sova.ts` becomes `https://rpc.testnet.sova.io` (`/pulse`
   needs nothing else; `ZcashBlocks` is the fixed predeploy), and the
   `/ashwings/*` pages' contract addresses come from
   `deployments/sova-testnet.json`. Deploy the site.

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
   `SHA256SUMS`. Then source the `testnet.env` from
   `https://dl.testnet.sova.io/testnet.env` (every line is an `export`,
   and it defaults to `SOVA_FOLLOW_ONLY=1`), set `SOVA_DATADIR` to a fresh
   directory and `SOVA_ZEBRAD_RPC=http://127.0.0.1:18234`, and run
   `sova` (`testnet.env` sets `SOVA_SIP6=1`, so it checks every seal, and
   `SOVA_SIP7=1`, so it boots the same genesis: `sova genesis-hash`
   with that env prints the hash `seeds.json` publishes).
   Pass: its log shows `p2p: discovery on … bootnode(s)`, `sova/1:
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
| `sova-keeper-1` | Hetzner CX33, fsn1, 40 GB volume | zebrad, sova in **mine** mode (SIP-6: signs with the keeper's miner key), `sova-keeper` (disclosed, D8) | SSH only |

| Script | What it does |
| --- | --- |
| `launch.sh` | The launch-day sequence below, re-runnable, `--dry-run`, `--go` |
| `provision.sh` | Hetzner: SSH key, two firewalls, volumes and servers (`hcloud`), all four by default. Optional byo hosts: adopted over SSH. Idempotent, `--dry-run` |
| `host/cloud-init.yaml` | First boot: admin user, SSH hardening, ufw, unattended upgrades, journald caps, Docker |
| `host/byo-bootstrap.py` | Applies that same `cloud-init.yaml` over SSH to a byo host, so it matches a Hetzner one |
| `test/byo-dry-run.sh` | Offline proof that the optional byo path works (a byo keeper validates, renders and appears in every launch stage) and that the default example is all-Hetzner |
| `deploy.sh` → `host/setup-host.sh` | Per-role setup over SSH. `deploy.sh check` validates the config, `deploy.sh render` writes and lints every host's files locally |
| `cloudflare.sh` | DNS, tunnels, the RPC firewall Worker (with the RPC rate limit), the faucet's WAF rate-limit rule, R2 bucket + domain, teardown |
| `epoch-base.sh` | Proposes, pins and records the epoch base B |
| `bootnodes.sh` | The final bootnode list, `testnet.env`, `seeds.json` (with the genesis hash the node hosts printed), the `chain.rs` constant, `--verify` |
| `deploy-contracts.sh` | The day-one contracts: `keygen`, `plan`, `deploy`, `verify` (runs `contracts/script/deploy-kit.sh`, the same code as `box/deploy-dapps.sh`) |
| `publish.sh` | Join files and zebrad snapshots to R2 |
| `smoke.sh` | Edge (incl. the published genesis hash, and SIP-7's `ZcashBlocks.latest()` and `sova_getZcashBlocks`), host, mint and contract checks |

**Budget** [est, 2026-09-22/23 prices; re-check at order time, because
Hetzner repriced three times in 2026]: **Hetzner only** (all four servers,
Rob, 2026-09-23). 2 × CX43 (seed, RPC) ≈ €33; 2 × CX33 (faucet, keeper)
≈ €16–24 (€8–12 each [est]: the CX33 price isn't in infra-m1's sourced
table); the volumes (60 + 40 + 40 + 40 GB at €0.0572/GB) ≈ €10. Cloudflare
is $0 ($5 if the RPC exceeds 100k requests a day), and R2 is under $1.
**Total ≈ €60–70 a month [est], Hetzner only.** Traffic: 20 TB per server
is included on Hetzner in the EU. There is no hard spend cap: the
project's server limit is the ceiling.

### Monitoring and alerts

`sova-health.timer` runs `host/health.sh` every 2 minutes on every host.
Findings go to the journal (`journalctl -t sova-health`). With
`deploy.sh alerts`, they also go to Telegram, at most once an hour per
alert.

| Alert | Fires when | Meaning |
| --- | --- | --- |
| `disk_*` | `/` or `/var/lib/sova` ≥ 80% | Grow the volume (`hcloud volume resize`, then `resize2fs`; an optional byo keeper on AWS: `docs/ops/keeper-aws.md`, "Operating it") |
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
  `./deploy.sh --only <name>`. The keeper's key lives on its volume, so
  a volume that re-attaches keeps it (and its seal journal). A new volume
  means a new keeper key, so it also needs a new disclosure. (An optional
  byo keeper on AWS: `docs/ops/keeper-aws.md`, "Rebuild".)
- **Edge off:** `./cloudflare.sh teardown`. This removes the DNS records,
  tunnels, Worker, route and rate-limit rule. It keeps the R2 bucket.
- **Everything off:** `./cloudflare.sh teardown`, then
  `./provision.sh teardown` (add `--keep-volumes` to keep the zebrad
  state). Both ask you to type the label, and they only touch resources
  labelled `project=sova-testnet`, which with the default config is all
  four servers. (An optional byo host is never touched: stop it in its
  provider's console; on AWS, release its Elastic IP.)
- **Emergency "project goes dark":** `systemctl stop sova-node` on our
  hosts. The network is unaffected by design; this is the drill.

### Checkpoint refresh (every release; first one about a day in)

Client checkpoints (audit F2 measure B, `docs/design/f2-join-and-restart.md`
§B) stop a joining node from being fed another history below them. At
launch the list is empty (genesis is pinned by the chainspec), so it
protects nothing until the first refresh, once the chain is 1,000 Zcash
blocks old (about 21 hours).

1. **Pick a height.** The newest Sova block `N` whose epoch
   `E = N + B − 1` is at least 1,000 blocks below the Zcash testnet tip.
   Zebra never rolls back that far, so the checkpoint adds no new
   assumption about Zcash reorgs.
2. **Compare on two nodes run independently** (the keeper and one other,
   ideally not ours):
   `cast block N --field hash --rpc-url <node>` must print the same hash
   on both. If they differ, stop: the network is split and needs a look,
   not a checkpoint.
3. **Build it in.** Add `(N, hash)` to `SOVA_TESTNET_CHECKPOINTS` in
   `bin/sova/src/chain.rs`, keeping older entries. Tag the release.
4. **Publish** the line `sova-checkpoint N 0xhash` in the release notes,
   the public repo and on sova.io, so anyone can check it against any node
   with `eth_getBlockByNumber`.
5. **Operators ahead of a release** can add it themselves:
   `SOVA_CHECKPOINTS=N:0xhash` (comma-separated for several). An entry that
   contradicts a built-in one makes the node refuse to start, never a
   silent override. A node whose database already contradicts a
   checkpoint also refuses to start and says to unwind or resync.

What it trusts: whoever chose the list, to have named the history the
network followed. Not validity: every block is still checked against the
node's own zebrad, so a wrong checkpoint can stop a node or put it on
another valid history, never mint anything.

### Testnet reset (SIP-4, SIP-6 and SIP-7 activate here)

The M1 chain (genesis `sova-testnet-v0`) is abandoned at the reset. SIP-4
§1, SIP-6 and SIP-7 activate from the new genesis, with no fork logic.
Section 0 settled the schedule (`flat`) and the chain ID (82330) on
2026-09-23.

1. **Code on release:** SIP-4 §1, SIP-6 and SIP-7 merged;
   `SOVA_TESTNET_GENESIS_EXTRA_DATA` bumped to `sova-testnet-v1` (a new
   genesis hash and fork ID, so old nodes are filtered out by the ENR
   fork-ID check and the Status handshake); the pinned genesis-hash and
   fork-ID tests updated (`chain.rs` pins the genesis hash with and
   without SIP-7); SIP-7's `ZcashBlocks` predeploy (code only, zero
   balance, at `0x…5A01`), which is in the genesis state only with
   `SOVA_SIP7=1` (the kit's default); `SOVA_TESTNET_BOOTNODES` refreshed.
   The chain ID stays 82330 (Rob, 2026-09-23).
2. **Tag** `vX+1` (B0).
3. **Announce** at least 48 h ahead: the new tag, the new genesis hash,
   and (SIP-6 §1.3) that miners must re-`init` their keystores. The
   genesis hash is not hard-coded anywhere in the kit: take it from the
   new binary (`SOVA_CHAIN=sova-testnet SOVA_SIP7=1 sova genesis-hash`, the
   same chain spec the nodes boot). At launch the node hosts print it
   again at `deploy.sh`, `bootnodes.sh` publishes it in `seeds.json` and
   `testnet.env`, `bootnodes.sh --verify` checks it against each running
   node's block 0, and `smoke.sh edge` against the public RPC's.
4. **Roll out:** clear `SOVA_EPOCH_BASE` in `config.env`, set
   `SOVA_RELEASE_TAG`, and keep `SOVA_EMISSION_SCHEDULE=flat` (Rob,
   2026-09-23: SIP-3's schedule is for mainnet). On each node host,
   including the keeper, wipe the Sova chain but **keep the node key**:
   ```bash
   ssh … 'sudo systemctl stop sova-node && sudo find /var/lib/sova/node -mindepth 1 -maxdepth 1 ! -name discovery-secret ! -name seal-journal -exec rm -rf {} +'
   ```
   The seal journal stays: it is never deleted (`docs/ops/keeper-miner.md`,
   "Sealing key (SIP-6)"). Its old entries name the old chain's parents,
   so they never match a new slot.
   Move `deployments/sova-testnet.json` to `deployments/sova-testnet-v0.json`
   (`deploy-contracts.sh` refuses a record whose genesis differs), and
   `keygen` a new deployer. Then run `./launch.sh --go`: it pins the new B,
   rolls out, re-publishes the join files and redeploys the contracts.
   zebrad state is untouched: it's the same Zcash testnet.
5. **Keeper:** SIP-6 seals with the keeper's miner key, so its burns must
   credit that key's own EVM address. A keystore made by today's
   `sova-miner init` does. `deploy.sh` stops with the fix if the keeper's
   doesn't (a legacy hash160 or `--evm-address` credit target): migrate it
   (`docs/ops/keeper-miner.md`) and publish the new EVM address. The
   faucet needs nothing: it's TAZ, unchanged.
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
- **B and the schedule are profile constants now (audit F9); SIP-6 and
  SIP-7 are still env vars.** `sova-testnet` fixes the schedule (flat)
  and, once `SOVA_TESTNET_EPOCH_BASE` in `chain.rs` is set, B: an env
  value that contradicts either refuses to start, and a malformed one is
  an error. Until a release compiles B in, `SOVA_EPOCH_BASE` is required
  for `sova-testnet` (no silent default of 1): the release that follows
  `epoch-base.sh pin` should set it. One who forgets `SOVA_SIP6=1` can't
  import sealed blocks (their 97-byte `extraData`), and one who forgets
  `SOVA_SIP7=1` boots a genesis without the `ZcashBlocks` predeploy: a
  different genesis hash and fork ID, so no peer accepts it. The follow-up is to pin those two in the `sova-testnet` profile
  as well. Until then, `testnet.env` carries them.
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
- **Rate limits.** Both are per client IP and counted per Cloudflare
  location.

  | Endpoint | Where | Limit | Over it |
  | --- | --- | --- | --- |
  | `rpc.testnet.sova.io` | the RPC Worker's `RPC_RATELIMIT` binding (`RPC_RATELIMIT_REQUESTS` / `RPC_RATELIMIT_PERIOD`) | 50 per 10 s; `OPTIONS` preflights not counted | HTTP 429, JSON-RPC error -32005, CORS, `Retry-After: 10` (exposed to pages) |
  | `faucet.testnet.sova.io/drip` | the zone's one free-plan WAF rule (`CF_RATELIMIT_REQUESTS_PER_10S`) | 50 per 10 s, then blocked 10 s | Cloudflare's own 429, no CORS: fine, no page calls the faucet cross-origin |

  The RPC's limit moved into the Worker because the WAF rule runs first
  and its 429 has no CORS headers, so `/pulse` and `/ashwings` saw a
  network error instead of a 429. The free plan's single WAF rule can't
  also hold a higher safety net on `/` (all matched paths share one
  counter and one threshold), so there is none; on Pro (2 rules, host
  matching) add one. If the Worker has no binding it **fails open** and
  logs `no RPC_RATELIMIT binding` (`wrangler tail` or Workers Logs): the
  allowlist and caps still apply, and a config slip shouldn't take the
  public RPC down. `smoke.sh edge` bursts twice the limit and expects a
  429 with CORS. If an account can't use the binding, set
  `RPC_RATELIMIT_AT=waf` and re-run `./cloudflare.sh worker ratelimit`:
  the WAF rule covers `/` again (the old behaviour, CORS-less 429s). The
  WAF rule matches paths only, zone-wide; on Pro it can match hosts.
