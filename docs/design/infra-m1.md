# M1 public infrastructure: what goes on a box, what goes on the edge

Status: **draft for Rob's sign-off** (board task infra-2). Written 2026-09-22.
Nothing here is provisioned. No accounts, no spend.

Tags used throughout: **[fact]** checked in this repo or a primary source,
**[measured]** observed on our own hardware, **[est]** an estimate that
needs re-checking before money moves. All prices exclude VAT and were read
on 2026-09-22 (sources at the bottom).

## The answer in five lines

1. The nodes can't live at the edge. Each Sova full node is a `sova` node
   **plus its own zebrad**: tens to hundreds of GB of state, long-lived P2P
   sockets, hours of initial sync. So they go on a small number of cheap
   stateful boxes.
2. The **public read path** does belong at the edge. That means a JSON-RPC
   firewall and rate limiter, the docs site, the explorer UI, a peer
   directory, and snapshot/binary downloads. Cloudflare is a good fit for
   all of these, and most of it costs $0.
3. Two boxes on Hetzner Cloud cost about **€45/month** [est]. The same
   shape costs about **$170/month** on AWS Lightsail and **$300–460/month**
   on AWS EC2 [est], mostly because of compute and egress pricing.
4. Our infra is a **courtesy bootstrap**. If every box we own disappears,
   consensus is unaffected and newcomers just lose some convenience. We
   prove that with a scheduled "switch it all off" drill before calling M1
   done.
5. **Hard rule:** no Sova infrastructure ever holds user funds or a key
   with consensus privilege. Sova has no privileged keys in consensus, and
   our operations must never add one.

## 1. Rob's edge instinct, taken seriously

Rob's instinct is right about one thing: a single fat AWS box that *is* the
network doesn't scale, and it looks like a company chain. The fix is not to
move the nodes to the edge, because nodes are the opposite of an edge
workload. The fix is to make our boxes **unimportant**. Scale comes from
other people's nodes. Only the read-path front door, the part many people
hit and nobody validates against, gets edge-scaled.

| Component | Edge? | Why |
| --- | --- | --- |
| zebrad (Zcash node) | **No** | Zcash testnet state is **18 GB at height ~2.64M of an estimated tip of ~3.78M** [measured, `du -sh` on the SSD state, 2026-09-22 14:13 EDT]. That projects to roughly 25–40 GB at the tip [est]. Mainnet is "around 300 GB" and growing [fact, Zebra docs]. It runs a RocksDB store and long-lived TCP peers on port 18233 (testnet) or 8233 (mainnet). Checkpoint sync ran at ~1,500 blocks/min, reaching 70% in about 6 h on a laptop SSD [measured]. Workers are request-scoped, with CPU budgets in milliseconds and no durable local disk. |
| sova node (reth) | **No** | Same shape, plus a **consensus requirement**: every node re-derives each epoch's settlements from *its own* zebrad and rejects mismatches (C5). Without a zebrad it "imports on trust" [fact, `bin/sova/src/main.rs`]. A validating node and its zebrad share one trust boundary, so they cannot be split across an edge. |
| Sealing / mining | **No, and not ours** | The top burner seals the epoch's block with its own key. This belongs to miners. |
| JSON-RPC *execution* | No | Needs chain state. It runs on a box. |
| JSON-RPC *front door* | **Yes** | TLS, DDoS absorption, IP rate limits, method allowlist, batch and size caps, micro-caching of `eth_chainId`/`eth_blockNumber`. |
| Docs site, explorer UI | **Yes** | Static files on Pages. |
| Snapshots, binary mirror, `seeds.json` | **Yes** | R2 charges **$0 egress** [fact]. Serving a 30 GB testnet snapshot (or 300 GB mainnet) to strangers is exactly where egress pricing hurts, and this is the biggest *financial* win the edge gives us. |
| Status / uptime probe | **Yes** | A Worker cron that probes the public endpoints from outside. |

Caveats on the edge pieces:

- **Rate limits can't read request bodies.** Cloudflare's free plan allows
  one rate-limiting rule, counted per IP, over a 10 s window, and it can't
  match request bodies or JSON fields [fact]. JSON-RPC methods live in the
  POST body, so the method allowlist has to run in a **Worker** or at the
  origin. We do both.
- **Workers quota.** The free tier allows 100k requests/day. Workers Paid
  is $5/month for 10M requests/month [fact].
- **P2P can't be proxied.** P2P ports are plain TCP, not HTTP, so
  Cloudflare's free proxy can't front them. Seed IPs are therefore public
  by necessity (see §4).

(Naming aside: "edge" here means the CDN sense. Positioning already rejected
"edge network" for exactly this collision, and this note should not leak
into copy.)

### Cost and shape comparison (monthly, 2 boxes) [est, 2026-09-22]

Shape per box: 4–8 vCPU, 16 GB RAM, ≥160 GB NVMe for testnet (≥500 GB–1 TB
for mainnet later). Egress assumption: 0.5–1 TB/box/month of P2P plus RPC
[est]. Zebra's own guidance is ~300 GB/month up+down for mainnet [fact].

| Option | Per box | Two boxes | Notes |
| --- | --- | --- | --- |
| **Hetzner Cloud CX43** (8 shared vCPU, 16 GB, 160 GB) | €15.99 + €0.50 IPv4 (+€5.72 per extra 100 GB volume) | **≈ €35–45** | 20 TB traffic included in the EU, then €1/TB. Shared vCPU is fine for testnet. |
| Hetzner Cloud CPX42 (8 vCPU, 16 GB, 320 GB) | €69.49 | ≈ €140 | More local disk and more consistent CPU. |
| Hetzner dedicated AX42-1 (64 GB, 2× NVMe DC) | €97.30 + €49 setup | ≈ €195 | Unlimited traffic. This is the **mainnet-era** shape (300 GB+ zebrad). Disk sizes should be re-checked at order time. |
| AWS Lightsail $84 bundle (4 vCPU, 16 GB, 320 GB, 6 TB transfer) | $84 | **≈ $170** | The honest AWS answer: flat price with egress bundled. Caveat: Lightsail vCPUs are **burstable**, so a multi-hour zebrad sync burns through CPU credits. Overage is $0.09/GB. |
| AWS EC2 m7g.xlarge + 200 GB gp3 + IPv4 + ~1 TB egress | $119 + $16 + $3.65 + ~$81 | **≈ $280–460** | Egress is $0.09/GB after 100 GB free per account per month. EBS is $0.08/GB-month. At mainnet sizes add ~$64/box for 1 TB gp3. |
| Cloudflare (DNS, WAF, Pages, Tunnel, R2 30 GB) | — | **$0–5.50** | Workers Paid ($5) only if RPC traffic exceeds 100k req/day. R2 is $0.015/GB-month. |

What AWS genuinely does better: managed snapshots, IAM, regions everywhere,
and possibly startup credits that would make year one free. None of that
matters for a disposable, stateless-by-design bootstrap, and credits would
tie the project to an account identity. The gap is about **4× against
Lightsail and 6–10× against EC2**.

**Re-check before ordering.** Hetzner repriced **three times in 2026**
(1 April, a setup-fee change announced 29 April, and 15 June, all driven by
DRAM costs). CCX-class dedicated-vCPU cloud rose 2–3×. The cheap tiers
above could move again, so prices should be re-checked on the day of
ordering. The table also doesn't cover Hetzner's "-LTD" tiers or its
server auction; both can be cheaper while supply lasts.

## 2. M1 component inventory

```
   strangers' laptops + community nodes  ═══  THE NETWORK
   [sova + own zebrad + miner] ⇄ sova P2P ⇄ [sova + own zebrad] ⇄ ...
          ▲ P2P (plain TCP, public IP)              ▲ P2P outbound only
 ┌────────┴─────────┐                      ┌────────┴──────────┐
 │ box A  seed-1    │                      │ box B  rpc-1      │
 │ zebrad (testnet, │                      │ zebrad (testnet)  │
 │  inbound 18233)  │                      │ sova follow-only  │
 │ sova follow-only │                      │ RPC on 127.0.0.1  │
 │ NO RPC exposed   │                      │ NO inbound ports  │
 └──────────────────┘                      └────────┬──────────┘
                                   cloudflared tunnel (outbound) │
 ═══ Cloudflare ═════════════════════════════════════════════════╪══
  rpc.<d>       Worker: method allowlist, batch≤10, size cap,  ◄─┘
                micro-cache  +  WAF IP rate rule
  <d>, docs     Pages (static; canonical copy stays in the repo)
  explorer.<d>  Otterscan SPA on Pages → rate-limited ots_* hostname
  dl.<d>        R2: zebrad snapshot + manifest, seeds.json, binaries
```

- **Seed / bootnode (box A).** Runs follow-only with P2P listening and no
  public RPC. It doubles as a courtesy Zcash testnet peer. Its address
  appears in docs, in `seeds.json`, and in the client's default list. It
  is never mentioned in the chainspec.
- **Full nodes (A and B), each with its own zebrad.** Both run in
  follow-only mode, so they enforce C5 and never seal.
- **Public JSON-RPC (box B, behind Cloudflare).**
  - *Origin:* the `eth,net,web3` modules only (reth's standard set
    [fact, pinned v2.6.0 `module.rs`]). Also `rpc.gascap`,
    `rpc.max-blocks-per-filter`, `rpc.max-logs-per-response`,
    `rpc.max-connections`, and request/response size caps, all present in
    the pinned reth [fact]. Bind to localhost. Engine `authrpc` never
    leaves the box. Implemented as `SOVA_RPC_PROFILE=public` (m1-c,
    `bin/sova/src/rpc.rs`): `http.api` pinned to `eth,net,web3`, WS/IPC
    off, then reth's `extend_rpc_modules` hook removes every HTTP method
    not in the edge allowlist below — so the origin enforces the same
    method-level list, not just the module set. Keep the two lists in
    sync.
  - *Edge allowlist:*
    - Chain metadata and fees: `eth_chainId`, `net_version`,
      `web3_clientVersion`, `eth_syncing`, `eth_blockNumber`,
      `eth_gasPrice`, `eth_maxPriorityFeePerGas`, `eth_feeHistory`.
    - Blocks and transactions: `eth_getBlockBy{Number,Hash}`,
      `eth_getBlockReceipts`, `eth_getTransactionBy*`,
      `eth_getTransactionReceipt`, `eth_getTransactionCount`.
    - State and calls: `eth_getBalance`, `eth_getCode`,
      `eth_getStorageAt`, `eth_call`, `eth_estimateGas`, `eth_getLogs`
      (range-capped).
    - Broadcast: `eth_sendRawTransaction`.
  - *Denied:* `admin_*`, `debug_*`, `trace_*`, `txpool_*`, `engine_*`,
    `personal_*`, `miner_*`, `eth_sign*`, `eth_sendTransaction`, the filter
    methods (`eth_newFilter`/`getFilterChanges` are stateful and costly),
    and WebSocket `eth_subscribe` at M1. `ots_*` is allowed only on the
    explorer hostname, with a tighter limit.
- **Gas for testnet users (no SOVA faucet is possible).** SOVA is only ever
  minted by burns. Gas works like this:
  - A user gets TAZ, burns it with `sova-miner`, and earns SOVA.
  - A burn-only miner needs no Sova node: rank-1+ fallbacks seal if the
    top burner is absent.
  - The **open problem is TAZ itself**. Every public Zcash testnet faucet
    is dead [fact, infra-1], and our own plan is to CPU-mine TAZ during
    testnet min-difficulty windows. See decision D5.
  - A SOVA "drip" from project-mined SOVA is possible later for dapp
    testers. That would be a capped, testnet-only hot key on its own host.
- **Explorer.**
  - Recommended for M1: **Otterscan** as a static SPA on Pages. Reth ships
    the `ots` namespace [fact], so there's no indexer and no database, and
    users can point it at their own node.
  - This still needs a verification run against v2.6.0 for coverage of the
    Erigon-flavoured calls, and a check of how settlement withdrawals
    render.
  - **Blockscout** (Postgres plus indexer, ~8 GB+ RAM [est]) waits for real
    demand for contract verification (G1 dapp users).
  - A tiny "epochs and burns" page can be built from RPC withdrawals later.
- **Docs.** Pages, built from the repo. The repo stays canonical.
- **Monitoring and alerting.**
  - *On each box:* disk use (alert at 80%, given the laptop fill-to-zero
    incident) and zebrad height against `estimatedheight`.
  - *The Sova-native health metric is* **epoch lag** = (zebrad tip − base
    + 1) − sova head, together with the C5 rejection count.
  - *From outside:* a Worker cron probes RPC liveness and head advancement.
  - *Alerts* go to a phone (Telegram bot or email).
  - The alerts have to separate **"we lag the network"** (our problem)
    from **"the network is stalled because no one burned"** (a miner
    matter, not an infra failure).
- **Backups: none needed, by design.**
  - Nothing on the boxes is irreplaceable. Config is declarative in the
    repo, and state re-syncs.
  - "Rebuild, don't repair" should take under an hour plus sync time
    [est].
  - Snapshots exist for *distribution*, not backup.
- **zebrad state snapshot (a high-leverage courtesy).**
  - *What it buys:* it turns a sync of about half a day (testnet
    [est from measured rate]) or days (mainnet) into a download. R2 makes
    that free to serve.
  - *Trust:* a restored database is trusted, not re-verified, so a
    malicious snapshot could doctor history. Sova changes the damage
    profile. For every Zcash height from the Sova base onward, the Sova
    chain itself commits to the burns: settlements are in the block
    withdrawals. A snapshot that adds or drops a burn makes the restoring
    node's C5 reject a canonical block. The node **stalls loudly** instead
    of diverging silently, and it can only harm itself, never other nodes.
    (This holds modulo the accept-unknown debt noted in the C3 row.)
    Doctored UTXO data could mislead the node's wallet view, but the
    network rejects spends that don't exist.
  - *Policy:* publish height, block hash, and archive SHA-256 in a
    manifest. Tell users to cross-check the block hash against an
    independent source. Snapshots are recommended for testnet. For
    mainnet, sealers should full-sync, and the snapshot is labelled a
    convenience.

## 3. "Never load-bearing", concretely

**If every box and account we own vanished tomorrow:**

| Area | What happens |
| --- | --- |
| Consensus | **Nothing changes.** Miners seal and every node validates against its own zebrad. |
| Existing nodes | Keep peering with each other. |
| Newcomers | Need one reachable peer from somewhere else. |
| RPC users | Lose `rpc.<d>` and repoint their wallet. |
| Explorer | Survives only if someone serves it and points it at another RPC. |
| Snapshots | Gone. New joiners full-sync, which takes hours on testnet. |
| Docs | Remain in the repo. |

Honest caveat: on M1 day one, **joining** depends softly on our seed until
independent seeds exist. The M1 exit metric **"≥2 non-project seeds
listed"** closes that gap.

**Design choices that guarantee this:**

- No consensus rule, chainspec field, or client behavior names our hosts,
  keys, or domain. There is no signer set and no privileged bootnode.
- The bootnode list is *config*: a compiled default plus a flag/env
  override plus `seeds.json` plus docs. Community seeds join the defaults
  as they appear.
- The P2P layer does peer exchange and discovery, not static peers only.
- The miner and node talk only to *their own* zebrad. The public RPC is
  never on any node's or miner's critical path.
- We run no project sealer. A "keeper" miner, if we run one (decision D8),
  is an ordinary, disclosed miner on a machine with its own key, never on
  the public boxes.
- **Drill (M1 gate):** switch our boxes off for 24 h during the public
  testnet. Strangers' chain keeps sealing, and a fresh node joins via a
  community peer. Publish the result. It's a checkable-neutrality receipt.

## 4. Security basics

1. **No Sova infrastructure ever holds user funds or consensus-privileged
   keys.** Public boxes hold no signing keys at all. The only allowed
   secrets are the tunnel credential and SSH host keys. The single
   possible exception is a testnet-only faucet key (D5): capped, on its
   own host, never on mainnet. This rule exists because the old chain ran
   a single hot BIP32 custody seed, had testnet keys committed in git
   history, and kept mainnet keys in AWS SSM.
2. **Nothing secret in git.** Add a CI secret scan (gitleaks-class).
3. **zebrad RPC and engine `authrpc` bind to 127.0.0.1 only.**
   - Zebra RPC exposes mining and node-control calls and is not built to
     face the public.
   - infra-1 turned cookie auth off for a single-user laptop. On servers,
     turn it back on.
   - Never offer "our zebrad" as a public broadcast service. That would
     make us load-bearing and a DoS target.
4. **Public RPC is read-and-broadcast only**, enforced twice: the edge
   allowlist and the origin module set.
5. **Origin hidden.** rpc-1 accepts no inbound connections: it runs a
   Cloudflare Tunnel and its P2P is outbound-only.
6. **DDoS.**
   - HTTP hostnames sit behind Cloudflare.
   - Seed P2P IPs are public and disposable. The answer to a P2P flood is
     *more seeds, run by more people*, not a bigger shield. Arbitrary-TCP
     proxying isn't a free-plan feature.
   - Hetzner includes basic volumetric DDoS protection.
7. **Access.**
   - SSH keys only, no passwords, restricted source addresses (or a
     tailnet), and unattended security upgrades.
   - Cloudflare API tokens are scoped per zone. Credentials live in a
     password manager, never in a cloud secrets store tied to the old org.
8. **Prerequisites that aren't infra, but that infra depends on:**
   - **Chainspec.** The node currently boots reth's dev chainspec with *20
     pre-funded dev accounts whose keys are public* [fact,
     `bin/sova/src/main.rs:413`]. The testnet needs a fresh chain ID and a
     zero-allocation genesis, or "every wei traces to burned ZEC" is false
     from block 0.
   - **Public block propagation.** Gossip v1/v2 pushes blocks to static
     peers' `authrpc` with a *shared JWT* (box-scale trust,
     `docs/design/gossip-v1.md`). That must never face the internet. M1
     needs a public propagation layer with discovery, for example a reth
     RLPx subprotocol. The seed's role (a bootnode for that layer) is the
     same whichever design wins.
   - **Public-RPC hardening profile.** `bin/sova` builds its config in
     code today, so the hardening settings from §2 need a small code
     change. *(Done in m1-c: `SOVA_RPC_PROFILE=public`; `rpc.gascap`,
     filter/log caps and connection caps still at reth defaults.)*

## 5. Phased plan

**M0: no servers.** The box stays local, and docs live in the repo.
Optionally register the domain and park it on Cloudflare DNS, which costs
$0 plus registration.

**M1: the minimal set (2 boxes plus the Cloudflare free plan, ≈ €40–50/month [est]).**

- [ ] Prerequisites land (§4.8): testnet chainspec, public propagation, RPC
      profile, configurable seeds, and a SIP-1 freeze (infra-1).
- [ ] ops-1 is done *before* any new account exists. New accounts are
      Rob-owned, use hardware 2FA and a project email, and have no shared
      logins. Account names (never secrets) are recorded on the board.
- [ ] Domain on Cloudflare DNS. Seeds are grey-cloud A records;
      everything else is proxied.
- [ ] Declarative box config in the repo (`infra/`: cloud-init, systemd
      units, firewall).
- [ ] Box A provisioned: firewall opens SSH (restricted), Zcash P2P
      18233, and Sova P2P only.
- [ ] Box B provisioned: no inbound ports; cloudflared tunnel.
- [ ] zebrad testnet synced on both, or B restored from A's snapshot.
- [ ] Sova nodes synced, with the "enforcing settlements" line confirmed
      in the logs.
- [ ] RPC firewall Worker plus the WAF rule. A denylist test in CI confirms
      that `admin_*`, `debug_*`, `txpool_*`, and `engine_*` are rejected.
- [ ] Pages: docs, Otterscan, and `seeds.json`.
- [ ] R2: testnet snapshot plus manifest (height, hash, SHA-256).
- [ ] Alerts wired: disk, epoch lag, C5 rejections, RPC down.
- [ ] "No keys on boxes" check scripted: no keystore, no funded key.
- [ ] Rebuild-from-zero runbook tested once.
- [ ] **Drill passed:** 24 h with our boxes off. Exit metric: ≥2
      independent seeds listed.

**Later (scale-out triggers):**

| Trigger | Response |
| --- | --- |
| RPC traffic above the Workers free tier | Workers Paid ($5). |
| RPC p95 latency or origin CPU stays high | Add rpc-2 in a second region, with origin selection done in the Worker. |
| Mainnet | Dedicated AX-class boxes (300 GB+ zebrad). Mainnet snapshot on R2 (~$4.50/month for 300 GB) with a full-sync recommendation for sealers. |
| ≥3 community RPC/seed operators | Our endpoints drop to "one of many" in the defaults. |
| Demand for contract verification | Blockscout. |
| Demand for subscriptions | WebSocket on a separate hostname. |

## 6. Decisions for Rob

| # | Decision | Recommendation |
| --- | --- | --- |
| D1 | Provider for stateful boxes | Hetzner Cloud (EU), 2× CX43-class for M1. Revisit dedicated boxes at mainnet. AWS only if credits appear. |
| D2 | Edge provider | Cloudflare (DNS, WAF, Workers, Pages, R2, Tunnel). |
| D3 | Domain | A fresh domain Rob controls, unless ops-1 confirms clean control of `sova.io`, whose DNS audit is pending. |
| D4 | Who holds the accounts | Rob as sole owner: hardware 2FA, a project email, per-zone scoped tokens, and a written recovery path. **Gate: ops-1 first.** Nothing is reused from the old org, especially not its AWS/SSM. |
| D5 | Where strangers get TAZ | Options: (a) a project TAZ faucet (the one testnet-only hot-key exception: isolated, capped, Turnstile-gated); (b) "mine your own TAZ" docs using zebrad `internal-miner`; (c) ask the Zcash community about a maintained faucet, but only once M1 is demoable (partner-outreach rule). **Open.** |
| D6 | Publish zebrad snapshots | Yes for testnet, with a manifest. For mainnet, publish only once trust copy is written and a full-sync recommendation for sealers is in place. |
| D7 | Explorer at M1 | Otterscan on Pages. Blockscout later. |
| D8 | Project "keeper" miner on testnet | Allowed only as a disclosed, ordinary miner on non-public infra, with its own key and budget. |
| D9 | Let seed-1's zebrad serve inbound Zcash testnet peers | Yes. It costs a little bandwidth and makes us a good citizen. |

## Sources (accessed 2026-09-22)

- Hetzner price adjustment of 15 June 2026 (AX42-1, CCX, CPX, CX):
  https://docs.hetzner.com/general/infrastructure-and-availability/price-adjustment/
- Hetzner 2026 repricing history:
  https://webhosting.today/2026/05/29/hetzner-has-now-raised-prices-three-times-in-2026-this-one-is-different/
  and https://bex.co/blog/2026/08/16/hetzner-2026-price-shocks-owning-hardware-pitch
- Hetzner Cloud CX43/CPX42, volume €0.0572/GB, traffic, IPv4 (aggregator,
  updated 2026-09-05): https://costgoat.com/pricing/hetzner
- AWS m7g.xlarge $0.1632/h: https://instances.vantage.sh/aws/ec2/m7g.xlarge
- AWS EBS gp3 ($0.08/GB-month reference rate): https://aws.amazon.com/ebs/pricing/
- AWS Lightsail bundles and overage: https://aws.amazon.com/lightsail/pricing/
- AWS 100 GB/month free egress: https://aws.amazon.com/ec2/pricing/on-demand/
- Cloudflare rate-limiting rules by plan:
  https://developers.cloudflare.com/waf/rate-limiting-rules/
- Cloudflare Workers pricing: https://developers.cloudflare.com/workers/platform/pricing/
- Cloudflare R2 pricing: https://developers.cloudflare.com/r2/pricing/
- Zebra system requirements (300 GB mainnet; its "10 GB testnet" figure is
  stale against our measurement): https://zebra.zfnd.org/user/requirements.html
