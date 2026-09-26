# Stranger test of the public testnet guide (2026-09-25)

The M1 gate asks whether a stranger can join the public testnet (chain
82330, release v0.1.3) by following only `docs/guides/testnet.md` and
the public artifacts it links. This run did that from clean Ubuntu 24.04
containers on the Apple M3 laptop, with nothing from the private repo,
servers or secrets.

**Result: a stranger can't join yet.** The one published bootnode
(`seed-1`, `2.28.138.164:30303`) didn't accept a single discovery or
RLPx connection in 1 h 45 min of trying, so a node running the published
`testnet.env` stays at 0 peers and block 0. The Zcash side works: the
snapshot restore verified, zebrad reached the tip, the faucet paid, and
six burns confirmed. Three of the six earned SOVA, visible on the public
RPC. The SOVA transfer only worked through the public RPC, because the
stranger's own node never synced.

Times are from `date` in the containers (UTC) or on the laptop (EDT,
UTC−4), as marked.

## Acceptance criteria

| AC | Result | Evidence |
| --- | --- | --- |
| 1. Download and verify the release, zebrad from the snapshot, node at the tip, block 0 hash | **Partial.** The release checksum verified, the snapshot SHA-256 matched the guide, zebrad reached the tip, and `sova genesis-hash` matched. The Sova node never passed block 0, so "`eth_blockNumber` within 2 blocks of the public RPC" fails | `sova-box-bin-linux-x86_64.tar.gz: OK`, `sova: OK`, `sova-miner: OK`. Snapshot `e78e551d…c4a3` equals the guide's value, and `snapshot.sh verify` printed `node block 4390524: 000007d5…57bb == manifest OK`. `sova genesis-hash` printed `0xb7391a4a…0b71`, as does `seeds.json`. The node's `eth_blockNumber` stayed `0x0` while the public RPC was at ~4,830 |
| 2. Peer with the public network using only what the guide and `testnet.env` give | **Fail** | `Status connected_peers=0 latest_block=0` for 72 min on the unedited `testnet.env` (and 0 over every earlier attempt from 16:27Z). Details in F1 |
| 3. Mine: `init`, faucet drip, burn, SOVA on the node and on the public RPC | **Pass on Zcash and on the public RPC. Fail on the own node** (at block 0, no peers) | Below |
| 4. Send one tx from the mined SOVA through your own node, confirm it on the public RPC | **Fail through the own node, pass through the public RPC** | The own node answered `insufficient funds for gas * price + value: have 0 want 1000000000000000000`. Sent through `https://rpc.testnet.sova.io` instead: tx `0x481097bd05097ff300c4fdd1f1506abb56bda0bac12de6d53ebc6448d5073172`, 1 SOVA to `0x…dEaD`, block 4822, status 1, 21,000 gas, mined 10 s after sending |

### AC3 detail

- Miner: t-addr `tmG5D2kgJUcf25fXZ1wFgxC7FUdEnvQHkRr`, EVM `0x66a4f2721544a3b2698ce6b3e6d94437e6fed7fe`.
- Faucet `POST /drip` at 16:54:54Z returned
  `{"amount_zat":10000000,"fee_zat":10000,"txid":"566afe0ad92e61618f4d830fbb5a4399a4db1d8a4368eaae3ee0f5e5022598f8"}`.
  It was mined in Zcash block 4,393,064 (block time 16:55:14Z), and zebrad's
  `getaddressbalance` then showed 10,000,000 zat.
- `mine --per-epoch-zat 10000 --budget-zat 5000000`, plus `--max-epochs 6`
  to stop after six burns. Every burn cost 10,000 + 20,000 fee = 30,000
  zat, as the guide says. **The guide's amounts fit a single 0.1 TAZ drip.**
- `report --verify-rpc` found `chain burns found (our address): 6 (total 60000 zat)` and ended `MATCH: yes`.

| Epoch | Zcash height | Burn txid | Sova block | Sealer | Paid to us |
| --- | --- | --- | --- | --- | --- |
| 1 | 4,393,294 | `dfbaca34…f826` | 4795 | keeper `0xbd8a…b7c9` (97-byte extraData) | 2,812.5 SOVA |
| 2 | 4,393,300 | `0f50059e…669b` | 4801 | null block | 0 |
| 3 | 4,393,309 | `fb0f8d54…39b2` | 4810 | null block | 0 |
| 4 | 4,393,321 | `35a21698…a17c` | 4822 | keeper | 2,812.5 SOVA |
| 5 | 4,393,326 | `2df0f1f3…10ad` | 4827 | null block | 0 |
| 6 | 4,393,332 | `2fefd7c3…4376` | 4833 | keeper | 2,812.5 SOVA |

The public RPC showed a balance of 8,436.4999… SOVA (3 × 2,812.5, minus
the 1 SOVA sent and gas). The own node showed 0 at head 0. The unpaid
epochs are the rule the guide describes: when the keeper doesn't burn,
the only ranked burner is a miner that can't seal, so the block is null.
A stranger could only change that by sealing (step 4), which needs a
synced node, and that needs F1 fixed.

## Phase timings

| Phase | Time | Notes |
| --- | --- | --- |
| DNS for `dl.testnet.sova.io` to resolve on this network | 36 min (NXDOMAIN 15:15Z to 15:51Z) | F3 |
| Release download and checksum check (32.6 MB) | 15 s | |
| `git clone` and checkout of `v0.1.3` | 8 s | |
| Build from source (linux-aarch64, 8-vCPU Docker VM on an M3, rustup stable 1.98.1) | `sova` 25 min 24 s, `sova-miner` 9 min 02 s | It ran alongside the snapshot download. F4 explains why the prebuilt binary couldn't be used |
| Snapshot download (11,037,447,512 B) | 47 min total: 288 s single-stream at 0.17–0.7 MB/s (abandoned), then 42 min with `aria2c -x 8` at 4.1 MiB/s average | F5 |
| Snapshot restore (`snapshot.sh restore`: checksum and extract) | 4 min 12 s, 12 GB on disk | |
| zebrad start to RPC up | 15 s (1,000 non-finalized blocks restored) | |
| zebrad catch-up, 4,390,525 to tip 4,393,291 | 70 min 26 s, with 11 restarts | F2. Without restarts it stalled after each burst |
| Sova node sync | never started (0 peers) | F1 |
| Faucet drip to mined | 20 s | |
| Burn to SOVA credit on the public RPC | epoch 4: broadcast 18:10:21Z, confirmed by the miner 18:10:34Z, payout visible 18:10:45Z (24 s end to end). Epoch 6: 18:11:30Z, 18:11:42Z, ≤ 18:12:06Z (≤ 36 s) | Polled every 15 s |
| SOVA transfer (public RPC) | sent to mined in 10 s | |
| Zero-sync zebrad, for comparison | 39,953 blocks in 33 min (~1,200 blocks/min, early chain) | Stopped when the snapshot became reachable. At that rate a full sync would take days here, not the guide's 12 h |

## Findings

Severity: **B** = blocks the M1 gate. **M** = a stranger hits it and
needs a workaround. **m** = minor, wording or noise.

### F1 (B): the only bootnode doesn't answer, so a new node gets 0 peers

The guide (2b) and `testnet.env` give one bootnode:
`enode://4788bec8…c17f@2.28.138.164:30303` (`seed-1.testnet.sova.io`).

- **Discovery:** the node pinged it every 20 s. It never answered: 0 UDP
  packets came back, and the log showed `discv4: evicting nodes due to
  failed pong`. discv5 logged `failed adding boot node … err=Timeout`.
- **UDP works from this machine.** In the same run, pointing the node at
  two Ethereum mainnet bootnodes returned 12 `Pong`s and several
  `Neighbours` packets. STUN (Google, Cloudflare) and NTP also answered.
- **TCP connects but the handshake doesn't.** `seed-1` accepts TCP on
  30303, then closes the stream as soon as our RLPx auth arrives. The
  trace:

  ```
  net::session: ecies auth failed error=stream closed due to not being readable remote_addr=2.28.138.164:30303
  ```

  This is also why the workaround below fails. It fits either a node key
  that doesn't match the published enode (the peer can't decrypt our
  auth), or a peer that drops inbound connections before the handshake
  (for example, full inbound slots).
- **Workaround tried:** `SOVA_P2P_PEERS="$SOVA_BOOTNODES"` (the guide's
  reference table lists this variable). It failed the same way:
  `static peer 0x4788… (2.28.138.164:30303) not connected; redialing`.
- **Duration:** checked from 16:27Z to 18:12Z. The last probe, at
  18:12Z, still got `5 × ecies auth failed` and no pong.
- **Context:** in the private repo, `infra/testnet/out/servers/sova-seed-1.*`
  (`enode`, `setup.log`, `host.env`) were rewritten at 12:43–12:45 EDT
  today, so the seed was redeployed during this run. The recorded enode
  equals the published one. `fw-seed.json` opens `udp 30303` and
  `tcp 30303`.

**To fix before announcing:** on `seed-1`, check that `sova` is running,
that it listens on udp and tcp 30303 with discovery on, that its
`discovery-secret` matches `4788bec8…`, and how full its inbound slots
are. Publish at least a second public bootnode. Add a check from outside
the project's hosts to the launch checklist: a fresh node on another
network, running the unedited `testnet.env`, must log `sova/1: peer
active` within 5 minutes. Until F1 is fixed, criteria 1, 2, 4 and
"SOVA on your own node" can't pass for anyone.

### F2 (M/B): zebrad 6.3.0 stalls repeatedly while catching up from the snapshot

After the restore, zebrad advanced in bursts, then sat without
advancing. The stalls ended only when zebrad was restarted
(`docker stop -t 110` / `docker start`). The heights it stopped at:

| Time (EDT) | Height |
| --- | --- |
| 12:54 | 4,390,529 |
| 13:00 | 4,390,533 |
| 13:07 | 4,390,568 |
| 13:12 | 4,390,599 |
| 13:26 | 4,390,629 |
| 13:32 | 4,390,763 |
| 13:37 | 4,391,347 |
| … | … |
| 14:03 | tip |

- During a stall, CPU was about 0.5 % and zebrad had 24–29 peers.
- About every 6 minutes it logged:

  ```
  ERROR … a BlockDownloadVerifyError that should have been filtered out was detected …
  WARN  … error downloading and verifying block e=Invalid { error: Block { source: Transaction(TransparentInputNotFound) }, height: Height(4390532) …
  ```

  Then it logged `waiting to restart sync timeout=67s` and moved at most
  a few blocks.
- The failing block changes from one restart to the next, and it
  verified fine after a restart. So the snapshot's state isn't corrupt.
  More likely, a dependent block timed out waiting for a parent download
  that never finished.
- This network reaches the internet through a Hong Kong egress (Cloudflare
  `cf-ray … -HKG`, external IP 156.59.50.240, ~480 ms RTT to Hetzner). So
  this may partly be the environment.
- The guide's remedy, "your zebrad stalls and won't follow the network:
  … full-sync", would take days here (see the zero-sync row in the
  timings).

**Suggest** adding a troubleshooting row: "zebrad stops advancing after
a snapshot restore → restart it with `docker restart -t 110 zebrad`
before considering a full sync." Also try the restore from a second
network to see whether it reproduces.

### F3 (M): DNS changes were still propagating during the run

- From 15:15Z, the laptop's resolver (and so Docker, and WebFetch)
  answered **NXDOMAIN** for `dl.testnet.sova.io`, while Google and
  Cloudflare DoH already returned the Cloudflare A records. It resolved
  at 15:51Z.
- `faucet.testnet.sova.io` stayed cached as `CNAME cname.vercel-dns.com`
  (TTL 4502 s left at 15:50Z). `curl https://faucet.testnet.sova.io/status`
  failed with a TLS error (exit 60, peer 66.33.60.130) until about
  16:54Z.
- The resolver had also cached `sova.io NS tate/barbara.ns.cloudflare.com`,
  while the live delegation is `pablo/rayne`. So the zone was recently
  moved between Cloudflare accounts, and some of the records changed.

A stranger whose resolver saw the old records gets "Could not resolve
host" on step 1b and a certificate error from the faucet. **Suggest:**
announce no sooner than 48 h after the last DNS change, and delete the
old zone (tate/barbara) if it still exists.

### F4 (M): the prebuilt `linux-x86_64` binary needs glibc 2.38; under Rosetta it dies silently

- `objdump -T` shows `sova` needs `GLIBC_2.38` (and `sova-miner` needs
  2.34). So it won't start on Ubuntu 22.04, Debian 12, RHEL 9 or Amazon
  Linux 2023. Those are common VPS images.
- The guide said only: "If it won't start on an older distribution,
  build from source." **Fixed in this branch:** the guide now names the
  glibc floor and the distributions.
- In an `ubuntu:24.04 --platform linux/amd64` container on the M3
  (Docker Desktop, Rosetta), `sova genesis-hash` works. But the node
  exits with status 132 (`Illegal instruction`) and no message, right
  after `Loaded storage settings`, as P2P initialises. That's why this
  run built from source for linux/arm64 (the guide's own fallback).
- The binary has AVX-512, ADX and SHA-NI code paths. Rosetta reports
  none of these. **Worth a check:** run the release binary on a real
  x86_64 CPU without AVX-512 (or under `qemu-x86_64 -cpu Haswell`), to
  make sure no unconditional ISA dependency crashes older strangers'
  machines.
- **Fixed in this branch:** the guide now notes the Rosetta case.
- The task brief called this laptop an Intel Mac. It is an Apple M3,
  and Docker's server arch is arm64.

### F5 (M): the snapshot downloads slowly as a single stream

- `curl -fLO` (the guide's command) ran at 0.17–0.7 MB/s. That would
  take 5–18 h for the 11 GB archive.
- Four parallel ranged requests each got 0.3–0.5 MB/s, and `aria2c -x 8
  -s 8` averaged 4.1 MiB/s (42 min).
- Response headers: `cf-cache-status: BYPASS`, `cache-control:
  max-age=14400`. Every download goes to the origin through the nearest
  PoP (HKG here).

**Suggest:** enable edge caching for `/zebrad-testnet/*`, and have the
guide suggest `aria2c -x 8 -s 8 -c` (it resumes, too) or `curl -C -` for
resumes.

### F6 (m): no named block explorer for the snapshot's independent check

1b says: "compare that hash at that height with any public Zcash testnet
block explorer."

- `testnet.zcashexplorer.app/block/4390524` and `/blocks/…` returned 404.
- `blockexplorer.one` renders the hash only with JavaScript.
- `testnet.zcashblockexplorer.com` doesn't resolve.

This run couldn't do the check. The practical evidence was that zebrad
kept extending from the snapshot tip on the network's PoW. **Suggest:**
name one explorer that works, with a URL template, or show `getblockhash`
against a second source.

### F7 (m): `snapshot.sh restore` gives advice that contradicts the guide's Docker setup

The restore's "NEXT" block says:

```
Point zebrad at it: [state] cache_dir = "/root/.sova-testnet/zebrad-state"
```

The guide's `zebrad.toml` sets `cache_dir = "/var/lib/sova/zebrad"`,
which is the container path that the host directory is mounted on. A
stranger could "fix" the config into a broken one. **Suggest:** have the
script say "the directory zebrad sees as `cache_dir` (in Docker, the
mount target)". The restore also leaves the files owned by the user who
ran it (root here). 1c's `chown` fixes that, so 1c must follow 1b, as
the guide already orders it.

### F8 (m): wording in 2b

- **"Below its `---- yours ----` line are two values of your own":**
  there are three. `SOVA_FOLLOW_ONLY=1` is also below the line.
  **Fixed in this branch.**
- The block above that sentence lists the values without `export`, while
  2c says "Every line in it is an `export`". The file does use `export`,
  so this is clear enough once you have the file.

### F9 (m): a literal `<<KEEPER_DISCLOSURE>>` and a maintainer-only section are public

The public guide opens with "Filled at launch … Maintainers: replace
each one … then delete this section", and 3c shows the literal
`(<<KEEPER_DISCLOSURE>>)`. A stranger sees both. **Suggest:** post the
disclosure (the keeper sealing our paid epochs is `0xbd8a560dfb415d4babb99662dc157267d166b7c9`)
before announcing, or drop the placeholder.

### F10 (m): version identification

- `sova --version` isn't accepted: it prints the usage message with
  `got: ["--version"]` and exits.
- The startup banner says `sova 0.1.0 (pre-release, under construction)`,
  and `sova-miner --version` prints `sova-miner 0.1.0`. The release is
  `v0.1.3`.
- Only `BUILD-INFO` (in the tarball) says `ref=v0.1.3`.

The "New releases" section tells people to check `sova genesis-hash`,
which is fine. But a stranger asked "which version are you on?" can't
answer from the binary.

### F11 (m): misleading log noise on a follow-only node

- `WARN Post-merge network, but never seen beacon client. Please launch
  one to follow the chain!` appears every 5 min (13 times in the run).
  A Sova node has no beacon client, so a stranger will think something
  is missing. **Suggest:** filter it out or reword it.
- While zebrad restarts, `expectations poll failed; retrying` appears
  every 2 s (100 lines). That's fine, but worth one troubleshooting row.

### F12 (m): no headless way to spend SOVA

Step 5 says to "import it into an EVM wallet". On a server, the obvious
tool is `cast`. This run installed Foundry and used `cast send
--private-key "$(sova-miner … export-evm-key --i-understand | tail -1)"`.
**Suggest:** a two-line `cast` example next to the wallet instructions.

### What worked as written

- 1a config, 1c `docker run` and the restore/verify tooling.
- 2c genesis check.
- The 2d "good start" lines (every line appeared, with `1 bootnode(s)`).
- 3a `init` output format, the 3b faucet (including without a
  `Content-Type` header), and the 3c burn line format and fee (20,000
  zat).
- Step 5 `report --verify-rpc` ending `MATCH: yes`, and the payout
  arithmetic (6,250 × 90 % / 2 burners = 2,812.5 SOVA).
- `sova-miner` retried cleanly through a zebrad restart mid-run.

## How the run was set up

- **Containers:**
  - `stranger-host`: `ubuntu:24.04`, `linux/amd64`. It held the network
    namespace, so every program saw `127.0.0.1:18232` and `:8545` as in
    the guide.
  - `stranger-arm`: `ubuntu:24.04`, `linux/arm64`, joined to the same
    namespace and the same home volume.
  - `stranger-zebrad`: `zfnd/zebra:6.3.0` (the native arm64 image),
    mounting the guide's `zebrad.toml` and `zebrad-state` from the home
    volume with `volume-subpath`.
- **Ports:** none were published to the laptop, so the laptop's own
  zebrad (18233/18234) was never involved.
- **Deviations from the guide:**
  - Built from source for arm64 (F4).
  - Downloaded the snapshot with `aria2c` (F5).
  - Restarted zebrad on stalls (F2).
  - Used `--max-epochs 6` to keep the burn short.
  - Tried `SOVA_P2P_PEERS` as a diagnostic (F1).
  - Sent the tx through the public RPC (AC4).
- **Cleanup:** containers and the volume were removed after the logs were
  copied to the job's scratch directory (`logs/`: node, miner, zebrad,
  probe traces, payouts). The miner keystore went with the volume.
  Re-running from this laptop's IP can't use the faucet again until
  2026-09-26 16:55Z (24 h cooldown).


## Orchestrator follow-up (2026-09-25, after the run)

**F1 is this laptop's network path, not the seed.** A fresh follow-only node started on sova-keeper-1 (Hetzner) with only the published `SOVA_BOOTNODES` (no static peer, new datadir, new ports) established a sova/1 session with seed-1 within seconds, found the keeper through discovery (2 peers), and executed blocks up to 4,820 of 5,026 in 2.5 min, with zero ECIES errors. The test machine egresses through Hong Kong (~480 ms RTT; earlier traffic from it showed up at Cloudflare SIN), and that path accepts the TCP connect but breaks the RLPx handshake and drops discv4 pongs. A stranger on such a path would still fail, so a second bootnode on a different network and a documented "no peers after 5 min" troubleshooting step are still worth doing.
