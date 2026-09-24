# Join the Sova public testnet

Sova is an EVM chain. Its gas coin, SOVA, is issued to people who burn
ZEC on Zcash. Miners burn with `sova-miner`, one Sova block follows each
Zcash block, and each block pays that epoch's burners. Every Sova node
runs beside its own Zcash node and re-derives every payout from it.

The public testnet is anchored to **Zcash testnet**, so you burn TAZ
(testnet ZEC). Its chain ID is **82330**. SIP-6 (sealer signatures) and
SIP-7 (Zcash pool state) are on from its genesis.

This guide takes you from nothing to:

1. a synced Zcash testnet `zebrad`,
2. a Sova testnet node that checks the published genesis hash and peers
   with the bootnodes,
3. a funded `sova-miner` burning TAZ and earning SOVA,
4. optionally, sealing blocks with your miner's keystore (SIP-6),
5. checking what you earned.

A laptop (Linux x86_64 or Apple Silicon Mac) or a small VPS is enough.

---

## Filled at launch

Values marked `<<LIKE_THIS>>` exist only once the testnet is launched.
Maintainers: replace each one before publishing this guide, then delete
this section.

| Placeholder | What it is | Produced by |
| --- | --- | --- |
| `<<RELEASE_TAG>>` | The release tag the network runs (the kit's value is `v0.1.0`) | `SOVA_RELEASE_TAG` in `infra/testnet/config.env`; the release assets come from the `box-binaries` workflow on that tag push (`docs/ops/testnet-launch.md`, B0) |
| `<<DL_URL>>` | Base URL of the download host (join files and snapshots) | `DL_HOST` in `config.env`, made live by `cloudflare.sh r2`; files uploaded by `publish.sh join` |
| `<<GENESIS_HASH>>` | The genesis hash every node must boot | `bootnodes.sh` (`genesis_hash` in `seeds.json`, and the header of `testnet.env`), taken from each node host's `sova genesis-hash` |
| `<<BOOTNODES>>` | The comma-separated enode list | `bootnodes.sh` (`SOVA_BOOTNODES` in `testnet.env`, `bootnodes.txt`, `seeds.json`) |
| `<<EPOCH_BASE>>` | B, the first Zcash testnet height that is a Sova epoch | `epoch-base.sh pin`, published by `bootnodes.sh` in `testnet.env` and `seeds.json`; its hash recorded by `epoch-base.sh record` (`epoch-base.json`) |
| `<<RPC_URL>>` | The project's public RPC | `cloudflare.sh tunnels` + `cloudflare.sh worker`; listed by `bootnodes.sh` as `courtesy.rpc` in `seeds.json` |
| `<<FAUCET_URL>>` | The TAZ faucet | `cloudflare.sh tunnels`; listed by `bootnodes.sh` as `courtesy.faucet` in `seeds.json` |
| `<<SNAPSHOT_URL>>` | The directory holding the latest zebrad snapshot (`<<DL_URL>>/zebrad-testnet/<height>/`) | `publish.sh snapshot` (also writes `zebrad-testnet/latest.json`) |
| `<<SNAPSHOT_HEIGHT>>`, `<<SNAPSHOT_HASH>>`, `<<SNAPSHOT_SHA256>>` | That snapshot's block height, block hash and archive SHA-256, posted somewhere other than the download bucket | `publish.sh snapshot` prints `snapshot.json`; posting it outside the bucket is the step in `docs/ops/snapshots.md` |
| `<<KEEPER_DISCLOSURE>>` | Where the project's keeper-miner disclosure (its addresses and budget) is posted | `deploy.sh` records the addresses (`out/servers/sova-keeper-1.keeper_*`); text template in `docs/ops/keeper-miner.md` |
| `<<EXPLORER_URL>>` | A block explorer for the testnet | No kit script. Otterscan is still an open M1 item (`docs/ops/testnet-launch.md`, step 9). If none exists at launch, delete the lines that use it |

---

## What you'll run

| Program | Job | Ports (defaults) |
| --- | --- | --- |
| `zebrad` (Zcash testnet) | Your own view of Zcash. Your Sova node checks every payout against it, and your miner broadcasts burns through it | RPC `127.0.0.1:18232`; Zcash P2P `18233` |
| `sova` | The Sova node: follows the chain, checks every block, serves RPC; optionally seals | HTTP RPC `127.0.0.1:8545`; Engine API `127.0.0.1:8551`; P2P `30303` TCP and UDP |
| `sova-miner` | Burns TAZ once per new Zcash block, inside a budget you set | none (talks to zebrad's RPC) |

The RPC ports stay on 127.0.0.1. Inbound P2P is optional: both nodes
work with outbound connections only. Opening `30303` (TCP and UDP) and
`18233` (TCP) lets other nodes reach you.

## What you need

| | |
| --- | --- |
| **OS** | Prebuilt binaries: Linux x86_64 and macOS on Apple Silicon. Anything else: build from source. |
| **Tools** | Docker (zebrad runs from the official `zfnd/zebra:6.3.0` image), `curl`, `jq`, `zstd` (snapshot restore), `git`, `python3` (optional, for decimal balances). |
| **Disk** | Zcash testnet state measured **12 GB** at height 4,382,331 (2026-09-22). A snapshot restore needs about twice that while the archive and the state both exist. The project's own nodes use 40 GB volumes. Plan on 40 GB free. |
| **Time** | From zero, a zebrad testnet sync took about **12 hours** in our own run (2026-09-22, native zebrad on a laptop with an external SSD). The snapshot restore below is the fast path: download, check, start, and zebrad only syncs from the snapshot's height. |
| **Machine size** | The project's keeper (zebrad, a sealing Sova node and a miner) runs on a Hetzner CX33 with a 40 GB volume. |
| **Build from source** (fallback) | Rust stable, about 4 GB of disk, and time: the `sova` release build takes about 11 minutes cold on an Apple M3, 20–40 minutes on a small VPS. On Linux, also `build-essential`, `pkg-config` and `clang`. |

Everything below lives in `~/.sova-testnet/`:

```bash
mkdir -p ~/.sova-testnet/bin ~/.sova-testnet/zebrad-state
export PATH="$HOME/.sova-testnet/bin:$PATH"
```

A small helper for JSON-RPC calls, used throughout:

```bash
rpc() { curl -s -H 'Content-Type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":${3:-[]}}" "$1"; }
```

---

## 1. A synced Zcash testnet zebrad

### 1a. Config

Write `~/.sova-testnet/zebrad.toml`:

```toml
[network]
network = "Testnet"
listen_addr = "0.0.0.0:18233"

[state]
cache_dir = "/var/lib/sova/zebrad"

[rpc]
# Inside the container. Docker publishes it on the host's 127.0.0.1 only (1c).
listen_addr = "0.0.0.0:18232"
# The Sova node can't read zebrad's auth cookie yet, so cookie auth is off.
# Keep the RPC on loopback.
enable_cookie_auth = false

[tracing]
use_color = false
```

The paths are inside the container: `cache_dir` is where the state
directory is mounted in 1c.

### 1b. Fast path: restore a snapshot

The project publishes snapshots of a stopped testnet zebrad's state. A
snapshot is a sync shortcut, not a source of truth. You check it against
a source the publisher doesn't control, and if the check fails you throw
it away and full-sync.

Get `snapshot.sh` from a checkout of the repo at the release tag:

```bash
git clone https://github.com/sova-chain/sova ~/.sova-testnet/src
git -C ~/.sova-testnet/src checkout <<RELEASE_TAG>>
SNAP=~/.sova-testnet/src/box/testnet/snapshot.sh
```

Download the three files into one directory:

```bash
mkdir -p ~/.sova-testnet/snapshot && cd ~/.sova-testnet/snapshot
curl -fLO <<SNAPSHOT_URL>>/zebrad-testnet-<<SNAPSHOT_HEIGHT>>.tar.zst
curl -fLO <<SNAPSHOT_URL>>/SHA256SUMS
curl -fLO <<SNAPSHOT_URL>>/snapshot.json
jq -r '.height, .hash, .sha256, .zebra_version' snapshot.json
```

The height, hash and SHA-256 must equal `<<SNAPSHOT_HEIGHT>>`,
`<<SNAPSHOT_HASH>>` and `<<SNAPSHOT_SHA256>>`, the copy posted outside
the download bucket. A checksum that only sits next to the file proves
the download wasn't corrupted, not who made it.

Restore into the empty state directory:

```bash
bash "$SNAP" restore zebrad-testnet-<<SNAPSHOT_HEIGHT>>.tar.zst ~/.sova-testnet/zebrad-state
```

It checks `SHA256SUMS`, checks that `snapshot.json` belongs to this
archive, refuses a non-empty target, and refuses archives holding
anything but zebrad state. The state format must match the zebrad
version in `snapshot.json` (`zfnd/zebra:6.3.0`, state format 28). An older
zebrad ignores it and full-syncs.

Start zebrad (1c), then check the snapshot's block on your own node:

```bash
bash "$SNAP" verify --rpc http://127.0.0.1:18232 --manifest ~/.sova-testnet/snapshot/snapshot.json
```

Then compare that hash at that height with any public Zcash testnet
block explorer. If it differs, or your zebrad stalls and won't follow
the network: stop zebrad, empty `~/.sova-testnet/zebrad-state`, and
full-sync.

### 1c. Start zebrad

On Linux, the image runs zebrad as uid 10001, so give it the state
directory first:

```bash
sudo chown -R 10001:10001 ~/.sova-testnet/zebrad-state   # Linux only
```

Then:

```bash
docker run -d --name zebrad --restart unless-stopped \
  -p 127.0.0.1:18232:18232 -p 18233:18233 \
  -v "$HOME/.sova-testnet/zebrad.toml:/home/zebra/.config/zebrad.toml:ro" \
  -v "$HOME/.sova-testnet/zebrad-state:/var/lib/sova/zebrad" \
  -e RUST_LOG=info \
  zfnd/zebra:6.3.0
```

Leave out `-p 18233:18233` if you don't want inbound Zcash peers. Skipped
the snapshot? This same command full-syncs from zero.

Stop it with a timeout, so RocksDB flushes cleanly:

```bash
docker stop -t 110 zebrad
```

### 1d. Wait until it's synced

```bash
rpc http://127.0.0.1:18232 getblockchaininfo | jq '.result | {blocks, estimatedheight}'
```

`blocks` is your tip, and `estimatedheight` is the network's. Wait until
they are within a few blocks of each other. The project's own hosts
alert when zebrad falls more than 20 blocks behind. Zcash testnet makes
a block about every 75 seconds.

---

## 2. A Sova testnet node

### 2a. Get the binaries

**Prebuilt.** The public repo, `github.com/sova-chain/sova`, builds
`linux-x86_64` and `darwin-arm64` binaries with its `box-binaries`
workflow and attaches them to the GitHub Release for the launch tag. Each
tarball holds `sova`, `sova-miner`, `SHA256SUMS` and `BUILD-INFO`.

```bash
cd ~/.sova-testnet
TAG=<<RELEASE_TAG>>
PLATFORM=linux-x86_64          # or darwin-arm64
BASE=https://github.com/sova-chain/sova/releases/download/$TAG
curl -fLO "$BASE/SHA256SUMS"
curl -fLO "$BASE/sova-box-bin-$PLATFORM.tar.gz"
grep " sova-box-bin-$PLATFORM.tar.gz" SHA256SUMS | sha256sum -c -   # macOS: shasum -a 256 -c -
mkdir -p release && tar -xzf "sova-box-bin-$PLATFORM.tar.gz" -C release
(cd release && sha256sum -c SHA256SUMS)                            # macOS: shasum -a 256 -c SHA256SUMS
install -m 0755 release/sova release/sova-miner bin/
```

The Linux binary is built on GitHub's `ubuntu-latest` runner. If it
won't start on an older distribution, build from source.

**From source** (fallback, or any other platform):

```bash
git clone https://github.com/sova-chain/sova ~/.sova-testnet/src   # skip if you cloned in 1b
cd ~/.sova-testnet/src && git checkout <<RELEASE_TAG>>
cargo build --release --locked -p sova
cargo build --release --locked -p sova-miner --manifest-path crates/burn-wallet/Cargo.toml
install -m 0755 target/release/sova crates/burn-wallet/target/release/sova-miner ~/.sova-testnet/bin/
```

`sova-miner` lives in its own Cargo workspace, hence the
`--manifest-path`.

### 2b. Get the join files

```bash
cd ~/.sova-testnet
curl -fsSLO <<DL_URL>>/testnet.env
curl -fsSLO <<DL_URL>>/seeds.json
```

`testnet.env` holds the network's settings. Every node must use the same
values for these:

```
SOVA_CHAIN=sova-testnet
SOVA_GOSSIP=p2p
SOVA_EPOCH_BASE=<<EPOCH_BASE>>
SOVA_EMISSION_SCHEDULE=flat
SOVA_SIP6=1
SOVA_SIP7=1
SOVA_BOOTNODES=<<BOOTNODES>>
```

Below its `---- yours ----` line are two values of your own:
`SOVA_ZEBRAD_RPC=http://127.0.0.1:18232` (your zebrad) and
`SOVA_DATADIR=$HOME/.sova-testnet/node` (where your node keeps its chain
and its node key).

`seeds.json` repeats the same values in JSON, with the genesis hash:
`jq . seeds.json`.

### 2c. Check the genesis hash

Every line in it is an `export`, so sourcing it hands the values to the
programs you start from that shell:

```bash
cd ~/.sova-testnet
. ./testnet.env
sova genesis-hash
jq -r .genesis_hash seeds.json
```

`sova genesis-hash` prints the genesis your node would boot with this
env, and exits without starting anything. It must print
`<<GENESIS_HASH>>`, and so must `seeds.json`. If it doesn't, you have a
different release or a different `SOVA_SIP7`, and you'd be on another
chain: peers filter it out.

### 2d. Run it (follow-only)

A follow-only node checks and serves the chain but produces no blocks.
`testnet.env` sets `SOVA_FOLLOW_ONLY=1`, so this is what you get by
default; step 4 turns on sealing.

```bash
cd ~/.sova-testnet
. ./testnet.env
sova 2>&1 | tee -a node.log
```

On a VPS with a public IPv4, also tell the node which address to
advertise, and open `30303` TCP and UDP if you want inbound peers:

```bash
export SOVA_NAT=extip:<your public IPv4>
```

The default (`any`) may ask UPnP and a public-IP service instead. It's
fine behind a home router.

**What a good start looks like** (in `node.log`):

```
datadir: .../.sova-testnet/node (persistent; node key .../.sova-testnet/node/discovery-secret)
chain profile: sova-testnet (chain ID 82330, 1 genesis alloc account(s))
p2p: sova/1 gossip enabled; local enode enode://...
p2p: discovery on (discv4 + discv5 on udp 0.0.0.0:30303; dns off; enforce ENR fork id true; nat any; N bootnode(s), no mainnet fallback)
expectations: enforcing settlements against zebrad at http://127.0.0.1:18232 (epoch base <<EPOCH_BASE>>)
follow-only mode: no local mining; serving RPC on :8545, receiving blocks over sova/1
```

The one genesis account is SIP-7's `ZcashBlocks` contract at
`0x…5A01`: no account holds SOVA at genesis. You'll also see
`sip-7 feed: sova_getZcashBlocks over HTTP`, and once a peer connects,
`sova/1: peer active`.

### 2e. Check it's on the network

```bash
rpc http://127.0.0.1:8545 eth_chainId                                   # "0x1419a" = 82330
rpc http://127.0.0.1:8545 eth_getBlockByNumber '["0x0",false]' | jq -r .result.hash   # <<GENESIS_HASH>>
rpc http://127.0.0.1:8545 eth_blockNumber | jq -r .result
```

Sova block N anchors Zcash height N + B − 1, so a caught-up node's head
is about your zebrad tip − `<<EPOCH_BASE>>` + 1. A new node catches up
only as far as its own zebrad has scanned: every block it syncs is
checked against your Zcash view first.

Compare a block hash with the public RPC at the same height:

```bash
H=$(rpc http://127.0.0.1:8545 eth_blockNumber | jq -r .result)
rpc http://127.0.0.1:8545 eth_getBlockByNumber "[\"$H\",false]" | jq -r .result.hash
rpc <<RPC_URL>> eth_getBlockByNumber "[\"$H\",false]" | jq -r .result.hash
```

The two hashes match. Your node is now verifying the testnet.

---

## 3. Mine: burn TAZ, earn SOVA

### 3a. Create the miner

Always pass `--network test`. The default is `regtest`.

```bash
sova-miner --network test --data-dir ~/.sova-testnet/miner init
```

```
t-addr to fund:              tm...
evm address (SIP-1 credit):  0x...
keystore:                    .../.sova-testnet/miner/keystore.json
state:                       .../.sova-testnet/miner/state.json
```

One key holds both: the TAZ at the t-addr and the SOVA at the EVM
address. The keystore is plaintext hex protected by file mode `0600`.
Keep it that way, and back it up if you care about the address.

### 3b. Get TAZ from the faucet

```bash
curl -s -X POST -d '{"address":"tm..."}' <<FAUCET_URL>>/drip
```

It sends 0.1 TAZ (10,000,000 zat) and answers with a `txid`. Limits: one
drip per address and one per IP per 24 hours, a daily cap for everyone,
and a transparent address (`tm…`) only. `curl -s <<FAUCET_URL>>/status`
shows whether it's accepting drips.

The drip can be spent once it's in a block, about one Zcash block after
it's sent. Any plain transfer of TAZ to the t-addr works the same way.
Coinbase paid straight to the t-addr can't fund a burn on testnet: it
has to go through a shielded pool first (`docs/ops/keeper-miner.md`,
"Coinbase must be shielded first").

### 3c. Mine

```bash
sova-miner --network test --data-dir ~/.sova-testnet/miner mine \
  --rpc http://127.0.0.1:18232 \
  --per-epoch-zat 10000 \
  --budget-zat 5000000
```

It sends at most one burn per new Zcash block and waits for each to
confirm, so on testnet it burns about every other block. Each burn line
looks like:

```
epoch 1: height=... burn=10000zat fee=...zat change=...zat txid=...
```

| Flag | Meaning |
| --- | --- |
| `--per-epoch-zat` | ZEC burned per epoch. At least 1,000 zat. |
| `--budget-zat` | This run's cap: burns plus fees. It stops when the next burn won't fit. |
| `--lifetime-budget-zat` | Optional ceiling across every run of this keystore. |
| `--max-epochs` | Optional: stop after this many burns. |

Each burn costs `--per-epoch-zat` plus a ZIP-317 fee, about 20,000 zat
for the usual one-input burn. The fee goes to Zcash miners and doesn't
count as burn weight. With the values above, a burn costs about 30,000
zat, so this run's budget is about 160 burns, and one 0.1 TAZ drip is
about 330.

### How SOVA is paid

- On this testnet, every epoch pays a flat **6,250 SOVA**
  (`SOVA_EMISSION_SCHEDULE=flat`).
- A burn in Zcash block `h` (at or after `<<EPOCH_BASE>>`) belongs to
  epoch `h`, paid in Sova block `h − <<EPOCH_BASE>> + 1`.
- Burners are ranked by ZEC burned in that epoch, most first; ties go to
  the smallest txid. The top-ranked burner seals the block. If it doesn't
  within 15 seconds, the next rank may, and so on.
- The sealer gets a 10% tip (625 SOVA). The other 90% (5,625 SOVA) is
  split among all of the epoch's burners in proportion to what each
  burned.
- An epoch needs a sealer ranked in it. If no ranked burner seals, the
  epoch gets a null block and mints nothing.

So a burn-only miner is paid whenever someone else ranked in the same
epoch seals. The project runs a disclosed keeper miner that burns a small
fixed amount (10,000 zat per epoch by default) about every other block
and seals its epochs (`<<KEEPER_DISCLOSURE>>`). It has no special
standing, and anyone who burns more outranks it. To be sure your epochs
get sealed, and to earn the tip, seal them yourself (step 4).

### Optional: let an agent mine

The MCP server in `mcp/` wraps `sova-miner` in tools an agent can call
(see `mcp/README.md` to build and register it). On testnet: pass
`network: "test"` and `rpcUrl: "http://127.0.0.1:18232"`, point
`SOVA_MINER_BIN` at your `sova-miner`, skip `sova_fund_regtest` (regtest
only), and fund the t-addr from the faucet. Sealing still needs the node
from step 4.

---

## 4. Optional: seal with your keystore (SIP-6)

With SIP-6, every block is either signed by a burner ranked in its
epoch, or a null block. A sealing node signs with your `sova-miner`
keystore, the same key your burns credit. It also builds null blocks for
epochs nobody burned in, which keeps the chain moving.

Stop the follow-only node (Ctrl-C), then start it in mine mode:

```bash
chmod 600 ~/.sova-testnet/miner/keystore.json
cd ~/.sova-testnet
. ./testnet.env
unset SOVA_FOLLOW_ONLY
export SOVA_SEALER_KEYSTORE="$HOME/.sova-testnet/miner/keystore.json"
sova 2>&1 | tee -a node.log
```

Look for:

```
sip-6: sealing as 0x<your evm address>
mine mode: following zebrad at http://127.0.0.1:18232, epoch base <<EPOCH_BASE>>, one Sova block per Zcash block
```

The address must be the `evm address` that `init` printed. Rules:

- **Burns must credit the key that seals.** A keystore made by today's
  `init` does. If `init` printed a `LEGACY` warning, run
  `sova-miner --network test --data-dir ~/.sova-testnet/miner init --migrate-evm-address`
  first. A miner set up with `--evm-address` for some other address
  can't seal. `SOVA_MINER_EVM_ADDRESS` is optional here, and if you set
  it, it must equal the sealing address or the node won't start.
- **Keep `SOVA_DATADIR`, and never delete its `seal-journal/`.** The node
  records every block it signs there, so a restart can't sign a second,
  different block for the same slot. Signing two blocks for one slot is
  equivocation: other nodes demote your blocks for that slot.
- **One sealing node per keystore.** Two nodes means two journals, which
  can sign twice.
- **The key is online.** It holds your TAZ and your SOVA. Move SOVA you
  want to keep to another address.

Keep `sova-miner mine` running as in 3c. The node seals, the miner
burns.

---

## 5. Check your earnings

**Your burns**, checked against the Zcash chain, txid by txid:

```bash
sova-miner --network test --data-dir ~/.sova-testnet/miner report --verify-rpc http://127.0.0.1:18232
```

It shows lifetime spend, every epoch you burned in, your funding at the
node, and ends with `MATCH: yes` when your local record equals the
chain.

**Your SOVA balance**, from your own node (use your `evm address`):

```bash
rpc http://127.0.0.1:8545 eth_getBalance '["0x<evm address>","latest"]' | jq -r .result
python3 -c 'import sys; print(int(sys.argv[1], 16) / 10**18, "SOVA")' 0x<result>
```

The balance is in wei: `0x152d02c7e14af680000` is 6,250 SOVA.

**Which blocks paid you.** Payouts are the block's withdrawals. For a
burn at Zcash height `h`, look at Sova block `h − <<EPOCH_BASE>> + 1`:

```bash
N=$(( h - <<EPOCH_BASE>> + 1 ))
rpc http://127.0.0.1:8545 eth_getBlockByNumber "[\"$(printf '0x%x' $N)\",false]" \
  | jq '.result | {miner, extraData, withdrawals}'
```

Each withdrawal's `amount` is in gwei. A sealed block has a 97-byte
`extraData`; a null block has none and pays nothing.

**Spending it.** The keystore key is an ordinary EVM key:

```bash
sova-miner --data-dir ~/.sova-testnet/miner export-evm-key --i-understand
```

It prints the key on stdout. Anyone who sees it can take both your SOVA
and your TAZ. Import it into an EVM wallet and add the network: chain ID
`82330`, currency `SOVA`, RPC `<<RPC_URL>>` (or your own
`http://127.0.0.1:8545`), explorer `<<EXPLORER_URL>>`.

The public RPC is read-and-broadcast only (no signing, no admin or
debug methods) and allows 50 requests per 10 seconds per IP. Your own
node has no such limit.

---

## Keeping it running

- **Restarts.** The node keeps its chain and node key in `SOVA_DATADIR`.
  A restart drops up to about 50 recent blocks and fetches them again
  from peers. That's expected.
- **As a service.** `infra/testnet/host/systemd/sova-node.service` is
  the unit the project's hosts use (with `EnvironmentFile=`). If you
  adapt it, write `SOVA_DATADIR` as an absolute path: systemd doesn't
  expand `$HOME`.
- **New releases.** Download the new tag (2a), check `sova genesis-hash`
  again, and restart.
- **Resets.** If the testnet restarts from a new genesis, it's announced
  ahead with the new tag and genesis hash. Then: stop the node, delete
  everything in `SOVA_DATADIR` except `discovery-secret` and
  `seal-journal`, get the new release and join files, and start again.
  Keep your zebrad state: it's the same Zcash testnet. Follow the
  announcement on whether keystores need `init` again.

---

## Troubleshooting

| You see | Why | Fix |
| --- | --- | --- |
| `SOVA_SIP6=1 mine mode requires SOVA_SEALER_KEYSTORE` and the node exits | `SOVA_FOLLOW_ONLY` was unset without a keystore | Keep `SOVA_FOLLOW_ONLY=1` (2d), or set `SOVA_SEALER_KEYSTORE` (4) |
| `no SOVA_ZEBRAD_RPC: importing without settlement enforcement (C5 off)` | The env didn't reach `sova` | Run `. ./testnet.env` in the same shell that starts `sova` |
| `usage: sova ...` and the node exits | An argument other than `genesis-hash` | `sova` takes no arguments; everything is `SOVA_*` env |
| `sova genesis-hash` or block 0 isn't `<<GENESIS_HASH>>` | Wrong release, or `SOVA_SIP7` isn't `1` | Use `<<RELEASE_TAG>>` and the unedited `testnet.env` |
| `0 bootnode(s)` in the discovery line, or never `sova/1: peer active` | `SOVA_BOOTNODES` empty or not exported, or outbound `30303` blocked | Check `echo $SOVA_BOOTNODES`; allow outbound TCP and UDP `30303` |
| `bad SOVA_BOOTNODES entry` | A mangled enode | Copy the line from `testnet.env` exactly |
| Head stays low while peers are connected | Your zebrad isn't synced: the node syncs only as far as its zebrad has scanned | Finish 1d |
| Head stopped moving | Compare `eth_blockNumber` with `<<RPC_URL>>`. If the public RPC is stuck too, the network is waiting for a sealer, not you | Nothing to fix locally. Running a sealing node (4) helps |
| `settlement mismatch at height ...` | A block contradicts your own zebrad | Check your zebrad is on Zcash testnet and synced. If you restored a snapshot, re-check its hash with an explorer; if in doubt, full-sync |
| Everything disagrees with the network | Missing or wrong `SOVA_EPOCH_BASE` (it defaults to 1) | Use the value in `testnet.env` |
| zebrad RPC refuses connections | Container down, or RPC not reachable | `docker ps`; `docker logs zebrad`; keep `listen_addr = "0.0.0.0:18232"` inside the container and `-p 127.0.0.1:18232:18232` |
| Port already in use | Something else holds 8545, 8551 or 30303 | `SOVA_HTTP_PORT`, `SOVA_AUTH_PORT`, `SOVA_P2P_PORT` |
| `Too many open files` | Low file-descriptor limit | Raise it (`ulimit -n`); the project's unit sets `LimitNOFILE=1048576` |
| `insufficient wallet funds` from `sova-miner` | The drip isn't in a block yet, or it's spent | Wait one Zcash block; check with `report --verify-rpc` |
| `... zat of coinbase must be shielded before it can fund a transparent burn` | The t-addr was funded with coinbase | Fund it with a plain transfer (3b) |
| `zcash chain RESET detected` from `sova-miner` | Pointed at an unsynced zebrad or another network | Mine only against a synced testnet zebrad, always with `--network test` |
| Faucet `429` `address_cooldown`, `ip_cooldown`, `daily_cap_reached` | A limit was hit | Wait for `Retry-After` |
| Faucet `503` `busy` | Every faucet coin is in an unmined drip | Retry after the next block |
| Burns confirm but no SOVA arrives | Your epochs had no ranked sealer (null blocks), or they were before `<<EPOCH_BASE>>`, or the credit address is `LEGACY` | Seal yourself (4); check `report` for a `WARNING`; check the block's withdrawals (5) |

Still stuck: open an issue at `github.com/sova-chain/sova/issues` with
your `node.log` lines, or ask in `t.me/sovazec`.

---

## What this testnet is, and what it isn't

- **Experimental software.** The node calls itself pre-release at
  startup. Expect bugs, and report them.
- **Resets happen.** The chain may restart from a new genesis. Testnet
  SOVA and chain history don't carry over.
- **No value.** TAZ and testnet SOVA are for testing. Don't buy or sell
  them.
- **The project's servers are conveniences.** Bootnodes, the public RPC,
  the faucet and snapshots save you time. Consensus doesn't depend on
  them: any peer works as a bootnode, and your node checks everything
  against your own zebrad.
- **Everything on the chain is public.** Burns are public on Zcash, and
  every Sova transaction, balance and contract call is visible. Funding
  your t-addr from your own shielded wallet makes the source of the TAZ
  unlinkable; everything after that is public
  (`crates/burn-wallet/miner/README.md`, "Anonymous funding").
- **Testnet keys are hot.** They sit on disk as plaintext behind file
  permissions. Never use one for anything of value.

---

## Reference: `sova` environment

| Variable | Value on this testnet | Notes |
| --- | --- | --- |
| `SOVA_CHAIN` | `sova-testnet` | Chain ID 82330, empty genesis alloc |
| `SOVA_GOSSIP` | `p2p` | Blocks travel over devp2p (`sova/1`); needed for discovery |
| `SOVA_EPOCH_BASE` | `<<EPOCH_BASE>>` | Consensus: same on every node |
| `SOVA_EMISSION_SCHEDULE` | `flat` | Consensus: 6,250 SOVA per epoch |
| `SOVA_SIP6` | `1` | Consensus: sealed or null blocks only |
| `SOVA_SIP7` | `1` | Consensus: part of the genesis |
| `SOVA_BOOTNODES` | `<<BOOTNODES>>` | Comma-separated enodes |
| `SOVA_ZEBRAD_RPC` | `http://127.0.0.1:18232` | Your zebrad |
| `SOVA_DATADIR` | `$HOME/.sova-testnet/node` | Chain, node key, seal journal |
| `SOVA_FOLLOW_ONLY` | `1` (the `testnet.env` default) | Unset to seal |
| `SOVA_SEALER_KEYSTORE` | your `keystore.json` | Sealing only |
| `SOVA_MINER_EVM_ADDRESS` | optional | If set, must be the sealing key's address |
| `SOVA_CHECKPOINTS` | optional, `height:0xhash,...` | Extra checkpoints published after your release; one that contradicts a built-in entry stops the node |
| `SOVA_NAT` | `extip:<IPv4>` on a VPS | Default `any` |
| `SOVA_P2P_ADDR` | optional | Bind IP for P2P, default `0.0.0.0` |
| `SOVA_DISCOVERY` | optional `off` | Static peers only (`SOVA_P2P_PEERS`) |
| `SOVA_P2P_PEERS` | optional | Comma-separated enodes to stay connected to |
| `SOVA_HTTP_PORT`, `SOVA_AUTH_PORT`, `SOVA_P2P_PORT` | `8545`, `8551`, `30303` | Port overrides |
| `SOVA_WS_PORT` | optional | WebSocket RPC on 127.0.0.1 (enables `sova_subscribe("zcashBlocks")`) |
| `SOVA_RPC_CORS` | optional | `*` or a list of origins, for browser pages calling your node |

`sova-miner` also reads `SOVA_MINER_RPC_COOKIE_FILE` (or
`--rpc-cookie-file`) for a zebrad with cookie auth on. With the config in
1a, it isn't needed.
