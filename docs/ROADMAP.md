# Sova roadmap

Sova is an EVM chain for Zcash. Its only issuance is burn-mining: destroy
ZEC on Zcash, and the next Sova block mints SOVA, the gas token, to you.

This page gives the order of the work. It is not a schedule. A phase is done
when its exit criteria are met, and we don't give dates for work that still
carries technical risk. The page changes by pull request, like the code.

Checked boxes are done. Unchecked items say whether they are *in progress*,
*next* or *later*.

## M0: code, spec and a chain you can run (announcement before the testnet)

The code, the specs and a runnable local chain go public together. There is
no public network at M0.

- [x] Burns recognized on Zcash; one Sova block per Zcash block; exact mints
- [x] Every node re-derives every mint from its own Zcash node and rejects
      blocks that disagree
- [x] sova-in-a-box: `./box/up.sh` runs a local Zcash regtest node, a Sova
      node and a miner
- [x] `sova-miner` CLI, and an MCP server so an agent can mine
- [x] Specs in [`sips/`](../sips): SIP-1 burn format (Draft), SIP-2 epochs
      and settlement (Draft), SIP-3 emission (Accepted)
- [ ] Prebuilt binaries on a tagged release (in progress)
- [ ] Public repository (in progress)

**Exit:** a stranger on macOS or Linux clones the tagged release, runs
`./box/up.sh` and watches a burn mint SOVA, in under 10 minutes with the
prebuilt binaries. Building from source instead takes about 11 minutes on
an idle Apple Silicon laptop and about 25 on a busy one.

## M1: strangers mine a public testnet (target October 2026)

A public Sova testnet anchored to Zcash testnet. Mining burns testnet ZEC
(TAZ), so it costs nothing real.

Built:

- [x] **SovaConsensus.** The mint check is a consensus rule on every import
      path: blocks at the tip, short gaps fetched from peers, and synced
      history.
- [x] **`sova/1`.** Block propagation over RLPx, announce and pull. No peer
      message can move a node's head.
- [x] **Discovery.** Explicit bootnodes and Sova's own fork ID. A Sova node
      never falls back to Ethereum's peers.
- [x] **Late-join catch-up.** A new node scans Zcash first and checks every
      historical mint as it syncs.
- [x] **Testnet chainspec.** Chain ID 82330; the genesis allocates nothing,
      and a test asserts it.
- [x] **Public RPC profile.** Read-only method allowlist.
- [x] **Ops kit.** A capped TAZ faucet, verifiable zebrad testnet snapshots,
      and a runbook for one disclosed keeper miner (see [`docs/ops/`](ops)).

Remaining:

- [ ] **SIP-1 freeze.** One burn relayed through public Zcash testnet peers
      and mined, then the format is frozen (in progress)
- [ ] **Infrastructure.** One seed node and one rate-limited public RPC. A
      courtesy bootstrap: nothing in consensus names them (next)
- [ ] **Testnet genesis and parameters.** Zcash testnet anchor height, a
      flat 6,250 SOVA per epoch (SIP-3's schedule starts at mainnet) (next)
- [ ] **Explorer** (next)

**Exit:**

- Strangers mine unassisted, from the docs alone, with the CLI or the MCP
  server.
- At least 3 epochs with several independent burners.
- Two consecutive weeks with no consensus fault.
- At least 2 seed nodes listed that the project doesn't run.
- Switch-off drill: every project machine, the keeper miner included, off
  for 24 hours. The chain keeps sealing and a fresh node joins through a
  community peer. We publish the result.

## Mainnet (target Q1 2027, gated)

Target: Q1 2027 (Rob, 2026-09-23; public on sova.io/why). Still gated: mainnet launches only when all of these hold:

- [ ] M1 exit met, and the testnet has been uneventful for a good while
      after that.
- [ ] Independent review of the consensus and settlement code, published.
- [ ] SIP-1 and SIP-2 frozen; every mainnet parameter set by SIP.
- [ ] A genesis dry run on testnet.
- [ ] An incident runbook, written before launch.

Mainnet chain ID is 8233. Real ZEC burns from block one. No peg at launch,
and no project machine ever holds user funds or a key with consensus power.

## Later and research

- **Burn-weight signaling.** Every burn already carries 32 signal bits
  (SIP-1). Tally them per window and activate parameter changes at a set
  height, BIP9-style. Needs its own SIP and one change carried end to end on
  testnet.
- **Contracts that read Zcash (SIP-4).** A precompile lets a contract check a
  Zcash payment (txid, outputs, confirmations) against the node's own Zcash
  node, with no bridge or oracle. Approved and in development; it ships at
  the testnet reset. First demos: buy with ZEC from any Zcash wallet, and
  custody-free ZEC↔SOVA swaps.
- **Contracts that see the shielded pool (SIP-7).** Pool totals and every
  change to them, readable by contracts, plus a per-block Zcash summary in
  Sova's state. Accepted; targets the testnet reset.
- **Wrapped ZEC is a Sova Labs product, not the protocol.** wZcash at
  wz.cash: ZEC held by NEAR's MPC network, a Sova Labs relayer, a small fee.
  Custodial and labelled that way; the network makes no guarantees about it.
  Former text for reference: ZEC held by NEAR's MPC network
  (Chain Signatures) and minted as wZEC on Sova. This is custody, and it is
  disclosed as custody: holding wZEC means trusting NEAR's signer set. Sova
  provides the contracts and monitors the vault from Zcash; the project
  holds no key. Its own specification (SIP-5), public review and an audit
  come first; not at mainnet launch.
- **Burning from a regular Zcash wallet**, without the CLI. Exploring.

## What we won't do

- **Call it a privacy chain or promise private smart contracts.** The EVM
  is transparent. Privacy lives at the funding edge: a burn can be
    funded from a shielded wallet.
- **Call it a Zcash L2.** It is a sidechain with its own consensus, anchored
  to Zcash by burns. It posts nothing to Zcash.
- **Premine.** SOVA is minted only by burns (SIP-3). The testnet genesis
  allocates nothing, and mainnet's won't either. The local box uses
  throwaway dev accounts that never exist on a public network.
- **Admin keys.** No signer set, no privileged bootnode, no privileged
  sealer. The one project miner on testnet is disclosed and ranks like
  anyone else.
- **Custody.** Sova itself holds nobody's ZEC, and the project holds no
  signer key. wZEC, when it ships, is custodied by NEAR and labelled that
  way.
- **Sell a token.** No sale, no investor allocation, no price talk. SOVA
  is gas.

## Follow and contribute

- **Run the box.** "It didn't work on my machine" is a useful issue.
- **Mine the testnet** once M1 opens.
- **Change the protocol with a SIP.** Float the idea in the SIPs category of
  [Discussions](https://github.com/sova-chain/sova/discussions), then open the SIP as a pull request to
  [`sips/`](../sips). See [CONTRIBUTING.md](../CONTRIBUTING.md).
- **Consensus code** merges only with simulation coverage (`box/sim`, run
  nightly in CI).
