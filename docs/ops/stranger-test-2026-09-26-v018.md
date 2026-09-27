# Stranger test of the public testnet guide, v0.1.8 (2026-09-26)

A second stranger test, one day after the
[v0.1.3 run](stranger-test-2026-09-25.md), on release `v0.1.8` (published
22:24Z). The only inputs were the **public** guide
(`raw.githubusercontent.com/sova-chain/sova/main/docs/guides/testnet.md`,
identical to `release` at `598bc59`) and the artifacts it links. The
machine was a clean `ubuntu:22.04` container, `linux/amd64` (Rosetta on
the Apple M3 laptop), run as an ordinary user with `sudo`. Times are UTC
from `date` in the container.

**Result: the release, the checks and the published values all pass, but
a stranger on this laptop's network still can't join.** v0.1.8's Linux
binary now runs on glibc 2.35 and under Rosetta (F4 of 09-25 is fixed),
`sova --version` works, the genesis, the snapshot checksum and the
explorer check all match, and the keeper disclosure is live and
checkable. But the node sat at **0 peers and block 0** for the whole run
(plain `testnet.env`, then static peers, then trace). New evidence below
(F1) says this is **not only the network path**: over the same path, in
the same minute, ordinary Ethereum mainnet nodes (including one in the
same Hetzner datacenter as seed-1) answer discovery and complete the RLPx
handshake, while both Sova seeds never do. The faucet refused the one
drip with `ip_cooldown`: another drip had gone to this laptop's public IP
about an hour earlier.

## Pass/fail by guide section

| Section | Result | Time | Evidence |
| --- | --- | --- | --- |
| What you need (tools) | **Pass, with friction** (F5) | 1 min 45 s (`apt-get`) | Fresh 22.04 has no `curl`, `jq`, `zstd`, `git`, `aria2c` or `python3`; the guide listed them but gave no install line. Docker wasn't installed in the container (zebrad skipped, see Scope) |
| 1b snapshot: clone + checkout `v0.1.8` | **Pass** | 8 s | `HEAD is now at 6f3dbc8`, `box/testnet/snapshot.sh` present |
| 1b snapshot: `SHA256SUMS`, `snapshot.json` | **Pass** | 5 s | `4390524`, `000007d5…57bb`, `e78e551d…c4a3`, `v6.3.0`: all equal the guide |
| 1b snapshot: archive download (`aria2c -x 8 -s 8 -c`) | **Pass** | 9 min 10 s | 11,037,447,512 B, ~19 MiB/s average (21 MiB/s peak). Still `cf-cache-status: BYPASS`, `cf-ray …-SIN` |
| 1b snapshot: `sha256sum -c SHA256SUMS` | **Pass** | 1 min 40 s | `zebrad-testnet-4390524.tar.zst: OK`. Archive deleted afterwards; no restore (Scope) |
| 1b CipherScan check | **Pass** | 4 s | The guide's `curl … \| grep -o '000007d5b1a0…'` printed the full manifest hash |
| 1c, 1d zebrad | **Skipped** (Scope) | | |
| 2a download + checks | **Pass** | 16 s | `sova-box-bin-linux-x86_64.tar.gz: OK`, `sova: OK`, `sova-miner: OK`. `BUILD-INFO`: `ref=v0.1.8`, `build=ubuntu-20.04-container glibc<=2.31 target-cpu=x86-64-v2`. Highest symbol version needed: `GLIBC_2.30` (both binaries) |
| 2a `sova --version` | **Pass** | | `sova 0.1.8`; `sova-miner --version` → `sova-miner 0.1.8` |
| 2b join files | **Pass, stale labels** (F3) | 4 s | Both files downloaded; two bootnodes, as the guide says. But both still name `v0.1.7` |
| 2c genesis | **Pass** | 0.09 s | `sova genesis-hash` and `seeds.json` both `0xb7391a4a…0b71`, as in the guide |
| 2d run follow-only | **Pass (starts), fail (peers)** | | Every "good start" line appeared, with `2 bootnode(s)`, banner `sova 0.1.8`. No `Illegal instruction` under Rosetta (09-25's F4 is gone). No beacon-client warning. 0 peers: F1 |
| 2e check | **Fail, and the guide's check passes vacuously** (F2) | | `eth_chainId` `0x1419a`, block 0 hash right, head `0x0`. The "compare at your head" step compares block 0 and "matches". Public RPC head at 22:41Z: 10,801 |
| No peers after 5 minutes, steps 1–5 | **Followed; no fix** | 15 min | Step 1 OK (2 bootnodes). Step 2: TCP 30303 opens to both seeds (611–629 ms). Step 4 (`SOVA_P2P_PEERS`): `static peer … not connected; redialing` for both. Step 5: `ecies auth failed error=stream closed due to not being readable` for both. The `grep connected_peers` recipe printed nothing (F6) |
| 3 keeper disclosure `cast balance` | **Pass** (after F7) | 2.3 s | `20307187.500000000018313018`. Last 12 public blocks (10,839–10,850): 11 sealed by `0xbd8a…b7c9` with 97-byte `extraData`, one null (10,842). CipherScan's page for the keeper t-addr loads (200) |
| 3a `sova-miner init` | **Pass** | 0.14 s | `tmJxtk7tP5ZvCoUxryCszses8PePBcpxfyo`, `0x5117c0754e0a04da4d5cba94dcf36ae50bae720d`, keystore `0600` |
| 3b faucet `/status` | **Pass** | | `accepting_drips: true`, balance 4.8 TAZ, `spent_today_zat: 10010000` (one drip today, before ours) |
| 3b faucet `/drip` (one attempt) | **Refused** (F8) | 2 s | `{"error":"ip_cooldown","message":"a drip was already sent to your network; try again in 82939s","retry_after_secs":82939}`. Not retried |
| 3c mine, 4 seal, 5 `report` | **Skipped** (no zebrad, no TAZ) | | |
| 5 balance and payout recipes | **Pass** | | Own node balance `0x0` (head 0). The `python3` conversion gives `6250.0 SOVA` for the guide's example. The "which blocks paid" recipe against the public RPC for `h = 4399346` gave block 10,847, keeper-sealed, one withdrawal of `0x5af3107a400` gwei = 6,250 SOVA |
| 5 `export-evm-key` into `KEY` | **Pass** | | Warnings on stderr only; `KEY` is 66 chars, `cast wallet address` gives the `init` address. `cast send` not run (no SOVA, and no writes allowed) |
| 5 Foundry install | **Pass, with friction** (F7) | 8 min 11 s installer + 41 s `foundryup` | After `curl -L https://foundry.paradigm.xyz \| bash`, `foundryup: command not found` |

## Findings

Severity as in the 09-25 report: **B** blocks joining, **M** a stranger
hits it and needs a workaround, **m** minor.

### F1 (B): the seeds refuse this network; other nodes over the same path don't

The node, with the unedited `testnet.env`, logged `connected_peers=0
latest_block=0` in every `Status` line from 22:36:45Z to the end of the
run (plain config 22:36–22:41Z and 22:56–23:11Z; diagnostics in between).
Egress was this laptop's VPN exit (a Zenlayer datacenter range; the 09-25 run
used the same range).

What each check showed:

- **Discovery.** The node pinged both seeds (`discv4: pinging boot node`,
  130-byte payloads to `2.28.138.164:30303` and `62.238.45.222:30303`) and
  never got a pong: `evicting nodes due to failed pong num=2`.
- **RLPx.** TCP 30303 opens to both seeds. Then every handshake fails:
  41 of 41 in the container and 16 of 16 from the Mac host, all
  `ecies auth failed` (`stream closed due to not being readable`, or
  `Connection reset by peer` from the host). A firewalled port on seed-1
  (30304, 8545) times out instead, so the TCP connect is real, not a proxy
  answering locally.
- **Control, same path, same minute.** With four Ethereum mainnet
  enodes added as static peers:
  - `157.90.35.166` (Hetzner, Falkenstein, same site as seed-1) completed
    the ECIES handshake and got as far as the `eth` Status exchange
    (`MismatchedGenesis`).
  - `18.138.108.67` and `65.108.70.101` completed it and then said
    `TooManyPeers`.
  - With mainnet bootnodes added to discovery (host run, 40 s), **15
    distinct nodes ponged**, `157.90.35.166` among them. Neither Sova
    seed did, in the same run.
- **Not Docker.** The same results came from the `darwin-arm64` binary
  run directly on the Mac (temporary datadir, ports 30313/8555/8561, and
  `SOVA_ZEBRAD_RPC` pointed at a dead port so the laptop's zebrad wasn't
  touched).

**So the 09-25 follow-up ("F1 is this laptop's network path, not the
seed") is at most half right.** The path carries discovery and RLPx to
other nodes, including a Hetzner Falkenstein one. The two Sova seeds both
ignore this client's UDP and drop its TCP right after the auth message.

That is exactly what reth does to a banned IP. In the reth we pin
(`v2.6.0`, `73a3a00`), `crates/net/network/src/peers.rs`:

- `on_incoming_pending_session` returns `IpBanned` for an IP on the ban
  list. The listener has already accepted the TCP connection, so the
  dialer sees the connect succeed and then the stream close
  (`ecies auth failed … stream closed`).
- `on_incoming_pending_session_dropped` bans the remote IP for
  `ban_duration` (default **12 h**, `network-types/src/peers/config.rs`)
  whenever an inbound handshake ends in a fatal protocol error. If the
  error also `merits_discovery_ban` (most `eth` Status errors other than
  `InvalidFork`, such as a genesis or chain mismatch, and several ECIES
  errors), it sends `DiscoveryBanIp`. discv4's `ban_ip` bans with **no
  expiry** (`net/banlist`), so the seed ignores every discovery packet
  from that IP until the process restarts.

So one bad handshake from any node behind this IP is enough: a node on
another genesis (a dev chain, an old release) that dials a seed from the
same NAT. That locks every Sova node behind the IP out of that seed, for
12 h over RLPx and until restart over discovery. It also fits the
WORKPLAN incident (line 164): the keeper saw the same
`ecies auth failed: stream closed`, and it cleared when the **seed**
restarted. Ban lists are in memory, so a restart clears them. Which
handshake triggered the ban here is unknown. It could be a dev build or
sim on this laptop, or another user of the same Zenlayer egress. The ban
itself is an inference from the symptoms and the code; the seeds' logs
would confirm it.

**To check on the seeds** (not done here: no server access in this
test). Run `tcpdump -ni any 'port 30303 and net <the tester\'s /24>'` on
seed-1 while a node here redials, and read the seed's log at
`net::session=trace` / `discv4=trace` for this IP. If packets arrive and
the seed drops them, it's a ban. Restarting `sova-node` on both seeds
should let this laptop in at once; re-run the no-peers check from here
right after to confirm. The lasting fix is a node change for the seeds
(and probably every node): a short `ban_duration` and no indefinite
discovery IP ban. A public bootnode that bans a whole NAT'd IP for a
day, and from discovery until restart, will lock out strangers behind
VPNs and carrier NAT whenever one misconfigured node shares their IP.
Re-run this test before announcing.

Context from the private repo (not visible to a stranger): the seeds'
`infra/testnet/out/servers/sova-seed-{1,2}.release` still say `v0.1.7`
(written 14:50 EDT), and their `setup.log`s were rewritten at
18:25–18:26 EDT (22:25Z), eleven minutes before this node first dialed.
If that was a restart, the ban is only minutes older than the test. That
points at a node on this network dialing the seeds between 22:25Z and
22:36Z.

The guide's step 3 in "No peers after 5 minutes" ("suspect a VPN, proxy
or unusual network path … a machine whose traffic left through Hong
Kong") rests on the 09-25 conclusion. It isn't changed in this branch,
because the cause isn't settled. If the seed check confirms a ban, reword
it.

### F2 (m): 2e's hash comparison passes at block 0

2e says to compare the block hash at your head with the public RPC, and
"The two hashes match. Your node is now verifying the testnet." With 0
peers the head is `0x0`, and both sides return the genesis hash, so the
check "passes". **Fixed in this branch:** the guide now says a match at
`0x0` proves nothing and points to the no-peers section.

### F3 (m): the published join files still say v0.1.7

`dl.testnet.sova.io/testnet.env` begins `(bin/sova v0.1.7)`, and
`seeds.json` has `"release": {"tag": "v0.1.7"}`, while the guide says
`v0.1.8`. The consensus values and bootnodes are right, and the genesis
matches, so nothing breaks, but a careful stranger will wonder which is
current. The repo copies (`infra/testnet/published/`) say the same.
**Suggest:** republish both with `v0.1.8` at the next `publish.sh` run.
Not changed here (a repo edit doesn't republish them).

### F4: v0.1.8 binary portability (09-25 F4, fixed)

- Runs on glibc 2.35 (Ubuntu 22.04). The binaries need at most
  `GLIBC_2.30`, matching `BUILD-INFO`'s `glibc<=2.31`.
- Runs under Rosetta in a `linux/amd64` container: the node started, bound
  P2P and discovery, and ran for about 35 minutes (with restarts) without `Illegal
  instruction`. 09-25's release died there with status 132.

### F5 (m): no install line for the tools

A fresh Ubuntu 22.04 has none of `curl`, `jq`, `zstd`, `git`, `aria2c`
or `python3`. The table named them but a newcomer had to work out the
package names. **Fixed in this branch:** one `sudo apt install` line in
the table.

### F6 (m): the peer-count `grep` prints nothing

"No peers after 5 minutes" says `grep -o 'connected_peers=[0-9]*'
node.log`. `node.log` (from `sova 2>&1 | tee`) carries ANSI color codes:
the line is `connected_peers\e[0m\e[2m=\e[0m0`, so the grep matches
nothing. The same section says a `Status` line comes every 25 seconds;
it came every 75 s (22:36:45, 22:38:00, 22:39:15, 22:40:30). Step 1 also
still spoke of one bootnode. **Fixed in this branch:** strip the codes
with `sed` first, 75 s, two bootnodes.

### F7 (m): Foundry's installer doesn't put `foundryup` on the PATH

The current installer at `foundry.paradigm.xyz` never edits the shell
profile. It prints "add foundryup to your PATH: `export
PATH="$PATH:$HOME/.foundry/bin"`", so the guide's "then `foundryup`"
answers `command not found`, and so would `cast` later. The installer
also took 8 min 11 s here, mostly fetching `foundryup` from GitHub.
**Fixed in this branch:** the export is in the guide.

### F8 (M): `ip_cooldown` locks out everyone behind one public IP

The one drip was refused: `ip_cooldown`, "a drip was already sent to your
network; try again in 82939s". That puts the earlier drip at about
21:57Z. `/status` showed exactly one drip today, before ours, and the
09-25 drip from this laptop was 30 h earlier, so another session on
this laptop or network took it. The faucet behaved as designed (IPv4
keyed per address). But a stranger behind a VPN, an office NAT or CGNAT
gets the same message for a drip they never made, and the guide didn't
say so. **Fixed in this branch:** the troubleshooting row explains it
and names the other way to get TAZ. No second drip was tried.

### What worked as written

- 2a download and both checksum layers; `sova --version`.
- 2c genesis check; 2d "good start" lines (all present).
- 1b metadata values, `aria2c` download, the archive checksum, and the
  CipherScan one-liner (the 09-25 F6 explorer problem is solved).
- The keeper disclosure: addresses, `cast balance`, CipherScan address
  page, and the public chain agrees (keeper seals nearly every block).
- 3a `init` output format; faucet `/status`; the 429 body is clear.
- 5: `python3` conversion, the "which blocks paid" recipe and
  `export-evm-key` capture.
- The node's startup and shutdown: `SIGINT` stopped it in 2 s each time,
  and no beacon-client warning appeared (09-25's F11 is fixed).

## Scope and deviations

- **zebrad not run.** Docker-in-Docker was out of scope, and a restore
  plus catch-up would have taken 12 GB more and hours. The node ran with
  the guide's `SOVA_ZEBRAD_RPC=http://127.0.0.1:18232` (the container's own
  loopback, where nothing listened), so it logged `expectations poll
  failed; retrying` every 2 s. That doesn't affect peering: the 09-25
  node had a synced zebrad and got 0 peers the same way. With peers, it
  still wouldn't have imported past block 0 without a zebrad, so "syncs
  Sova blocks" couldn't have passed here either way.
- **Mining, sealing, `report`, `cast send`:** skipped (no zebrad, no TAZ,
  and no testnet writes beyond the one drip attempt).
- **Diagnostics beyond the guide:** mainnet enodes as static peers and
  bootnodes (container and Mac host), trace logging, TCP probes of
  30303/30304/8545 on seed-1. Nothing was sent to the Sova testnet but
  the P2P dials and ~40 read-only public RPC calls, spaced out.
- **Laptop safety:** no ports published from the container; the host-side
  runs used 30313/8555/8561 and never touched 18232–18235 or the running
  zebrad. The Mac kept ≥ 135 GB free; the container peaked at ~11.6 GB.
- **Cleanup:** container `sova-stranger-018` removed at the end; the
  miner keystore went with it.

## Addendum (orchestrator, 2026-09-26 23:40 UTC)

Checked after the report, before believing "the seeds are the cause":

- **Not a wrong key.** seed-2's live enode (its startup log) equals the published `441e2f90…`.
- **Not only a ban.** After restarting seed-2's `sova-node` (which clears reth's in-memory bans), a node on this laptop still got 0 peers. A capture on seed-2 showed the laptop's discovery packets arriving and seed-2 answering each one; the laptop's node never accepted the answers, so it never dialled.
- **Not clocks.** Both seeds are NTP-synced and within seconds of the laptop.
- **A fresh node elsewhere joins at once.** The v0.1.8 Linux binary with only the published `testnet.env`, run on sova-faucet-1 (Hetzner, not a Sova peer): `sova/1: peer active` for both seeds within 2 s, `connected_peers=2`.

So a newcomer on an ordinary network joins; this laptop's egress (a Zenlayer VPN/datacenter range) is what fails, as the guide's step 3 already says. The ban exposure the report found is still real for shared IPs (VPN exits, carrier NAT): one bad handshake bans the IP for 12 h and from discovery until restart. Hardening that (shorter bans on the seeds) is tracked on the board.
