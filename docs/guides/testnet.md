# Join the Sova public testnet

Sova is an EVM chain. Its gas coin, SOVA, is issued to people who burn
ZEC on Zcash. One Sova block follows each Zcash block, and every Sova
node runs beside its own Zcash node and re-derives every payout from it.

The public testnet is anchored to **Zcash testnet**, so you burn TAZ
(testnet ZEC). Its chain ID is **82330**. To look around first, browse
[explorer.testnet.sova.io](https://explorer.testnet.sova.io).

This quickstart takes you from nothing to a synced Zcash testnet
`zebrad`, a Sova node on the network, and a `sova-miner` burning TAZ and
earning SOVA. The [testnet reference](testnet-reference.md) has the
rest: requirements, sealing, earnings, troubleshooting and every setting.

You need Linux x86_64 or an Apple Silicon Mac (a laptop or a small VPS),
Docker, `curl`, `jq`, `zstd`, `git`, and about 40 GB free. Testnet coins
have no value, and the chain can reset
([what this testnet is](testnet-reference.md#what-this-testnet-is-and-what-it-isnt)).

<!-- Maintainers: keep this file the short path; details go in testnet-reference.md. -->

Everything lives in `~/.sova-testnet/`. The `rpc` helper is used throughout:

```bash
mkdir -p ~/.sova-testnet/bin ~/.sova-testnet/zebrad-state
export PATH="$HOME/.sova-testnet/bin:$PATH"
rpc() { curl -s -H 'Content-Type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":${3:-[]}}" "$1"; }
```

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

### 1b. Fast path: restore a snapshot

A snapshot saves hours of sync. Get `snapshot.sh` at the release tag,
then download the snapshot (11 GB; `aria2c` resumes and uses 8
connections):

```bash
git clone https://github.com/sova-chain/sova ~/.sova-testnet/src
git -C ~/.sova-testnet/src checkout v0.1.18
SNAP=~/.sova-testnet/src/box/testnet/snapshot.sh
mkdir -p ~/.sova-testnet/snapshot && cd ~/.sova-testnet/snapshot
aria2c -x 8 -s 8 -c https://dl.testnet.sova.io/zebrad-testnet/4390524/zebrad-testnet-4390524.tar.zst
curl -fLO https://dl.testnet.sova.io/zebrad-testnet/4390524/SHA256SUMS
curl -fLO https://dl.testnet.sova.io/zebrad-testnet/4390524/snapshot.json
jq -r '.height, .hash, .sha256, .zebra_version' snapshot.json
```

The four values printed must match these:

```
4390524
000007d5b1a082776d85c9bc5eabc0b093110675c33c78d9eb78c5b0449a57bb
e78e551d89b66a07b6623b3eb02ea71c5adf532addd510e59b55717adfd2c4a3
v6.3.0
```

The snapshot is from zebrad 6.3.0 (state format 28). The zebrad in 1c
(7.0.0-rc.0, which knows Zcash testnet's NU7 upgrade) opens it in place:
on first start it moves the state to format 29, no resync.

Restore into the empty state directory, and keep `cache_dir` as in 1a:

```bash
bash "$SNAP" restore zebrad-testnet-4390524.tar.zst ~/.sova-testnet/zebrad-state
```

After 1c, [check the snapshot](testnet-reference.md#the-zebrad-snapshot)
against a source the project doesn't run.

### 1c. Start zebrad

```bash
sudo chown -R 10001:10001 ~/.sova-testnet/zebrad-state   # Linux only
docker run -d --name zebrad --restart unless-stopped \
  -p 127.0.0.1:18232:18232 -p 18233:18233 \
  -v "$HOME/.sova-testnet/zebrad.toml:/home/zebra/.config/zebrad.toml:ro" \
  -v "$HOME/.sova-testnet/zebrad-state:/var/lib/sova/zebrad" \
  -e RUST_LOG=info \
  zfnd/zebra:7.0.0-rc.0
```

Skipped the snapshot? The same command full-syncs from zero (about 12
hours). Stop it with `docker stop -t 110 zebrad`, so RocksDB flushes
cleanly.

### 1d. Wait until it's synced

```bash
rpc http://127.0.0.1:18232 getblockchaininfo | jq '.result | {blocks, estimatedheight}'
```

Wait until `blocks` is within a few blocks of `estimatedheight`.

## 2. A Sova testnet node

### 2a. Get the binaries

```bash
cd ~/.sova-testnet
TAG=v0.1.18
PLATFORM=linux-x86_64          # or darwin-arm64
BASE=https://github.com/sova-chain/sova/releases/download/$TAG
curl -fLO "$BASE/SHA256SUMS"
curl -fLO "$BASE/sova-box-bin-$PLATFORM.tar.gz"
grep " sova-box-bin-$PLATFORM.tar.gz" SHA256SUMS | sha256sum -c -   # macOS: shasum -a 256 -c -
mkdir -p release && tar -xzf "sova-box-bin-$PLATFORM.tar.gz" -C release
(cd release && sha256sum -c SHA256SUMS)                            # macOS: shasum -a 256 -c SHA256SUMS
install -m 0755 release/sova release/sova-miner bin/
```

Other platforms: [build from source](testnet-reference.md#build-from-source).

### 2b. Get the join files

```bash
cd ~/.sova-testnet
curl -fsSLO https://dl.testnet.sova.io/testnet.env
curl -fsSLO https://dl.testnet.sova.io/seeds.json
```

`testnet.env` holds the network's settings, which every node shares,
and three of your own ([what's in it](testnet-reference.md#the-join-files)).

### 2c. Check the genesis hash

```bash
cd ~/.sova-testnet
. ./testnet.env
sova genesis-hash
jq -r .genesis_hash seeds.json
```

Both must print
`0xb7391a4a83644e1dce95c95348a005febedeaa12fa46eb30ac0dfb5f36f00b71`.
If not, you'd be on another chain.

### 2d. Run it (follow-only)

A follow-only node checks and serves the chain but produces no blocks.

```bash
cd ~/.sova-testnet
. ./testnet.env
sova 2>&1 | tee -a node.log
```

On a VPS with a public IPv4, also `export SOVA_NAT=extip:<your public IPv4>`.
[What a good start looks like](testnet-reference.md#what-a-good-start-looks-like).

### 2e. Check it's on the network

Compare a block hash with the public RPC at the same height:

```bash
H=$(rpc http://127.0.0.1:8545 eth_blockNumber | jq -r .result)
rpc http://127.0.0.1:8545 eth_getBlockByNumber "[\"$H\",false]" | jq -r .result.hash
rpc https://rpc-testnet.sova.io eth_getBlockByNumber "[\"$H\",false]" | jq -r .result.hash
```

The two hashes match: your node is verifying the testnet. It catches up
only as far as your zebrad has scanned. If `H` is still `0x0`, see
[No peers after 5 minutes](testnet-reference.md#no-peers-after-5-minutes).

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

One key holds both the TAZ at the t-addr and the SOVA at the EVM
address. The keystore is plaintext behind file mode `0600`: keep it that
way, and back it up.

### 3b. Get TAZ from the faucet

```bash
curl -s -X POST -d '{"address":"tm..."}' https://faucet-testnet.sova.io/drip
```

It sends 0.1 TAZ to a `tm…` address, once per address and per IP every
24 hours. The TAZ is spendable about one Zcash block later.

### 3c. Mine

```bash
sova-miner --network test --data-dir ~/.sova-testnet/miner mine \
  --rpc http://127.0.0.1:18232 \
  --per-epoch-zat 10000 \
  --budget-zat 5000000
```

It sends at most one burn per new Zcash block:

```
epoch 1: height=... burn=10000zat fee=...zat change=...zat txid=...
```

Each burn costs `--per-epoch-zat` plus a Zcash fee of about 20,000 zat.
One drip is about 330 burns at these values.

## Next

- [How SOVA is paid](testnet-reference.md#how-sova-is-paid), and the
  flags that cap your spend.
- [Seal your own blocks](testnet-reference.md#4-optional-seal-with-your-keystore-sip-6)
  (SIP-6), so your epochs get sealed and you earn the tip.
- [Check your earnings](testnet-reference.md#5-check-your-earnings) and
  spend your SOVA.
- [Mint an Ashwing](../../docs-site/pages/start/ashwings.md) with it.
- Stuck? [Troubleshooting](testnet-reference.md#troubleshooting), or ask
  in [t.me/sovazec](https://t.me/sovazec).
