# M1 networking: stranger-safe propagation, discovery, and validated sync

Status: **design, pre-implementation** (board m1-b, orchestrator-owned).
Written 2026-09-22 from a source read of reth v2.6.0 (rev `73a3a00`); every
reth claim below cites that tree. Supersedes gossip v1's transport for any
public network; the box keeps v1 until this lands.

## Why v1 can't go public

Gossip v1 pushes `engine_newPayloadV4` to static peers' authrpc with a shared
JWT (`crates/engine/src/relay.rs`). The engine API is a root-privileged
interface — anyone holding the JWT can also send forkchoice updates — so it can
never face strangers. M1 needs: (1) block propagation over an unprivileged
channel, (2) peer discovery, (3) network isolation from Ethereum and every
other reth chain, and (4) **consensus rules enforced on every import path,
including history a joining node syncs.** The fourth turned out to be the
important one.

## Finding: C5 is not enforced on synced history (fix before any public net)

C5 — each block's withdrawals (the SOVA mint) must equal what *this* node
derives from its own zebrad — lives in `PayloadValidator::convert_payload_to_block`
(`crates/engine/src/validator.rs:61-124`). That hook runs only when a block
arrives as an execution *payload* (engine `newPayload`). reth imports blocks
two other ways, and neither calls it:

- **Engine-tree download path** (FCU to an unknown head, gap ≤ 32 blocks):
  `insert_block` converts with an identity function, so
  `convert_payload_to_block` never runs (`engine/tree/src/tree/mod.rs:3108-3119`).
  It does run `Consensus::validate_header*`, `validate_block_pre_execution*`,
  the executor, and `FullConsensus::validate_block_post_execution`
  (`payload_validator.rs:946-972, :1353`).
- **Backfill pipeline** (gap > 32): header/body downloaders and the execution
  stage call only `Consensus` + executor hooks
  (`net/downloaders/src/headers/reverse_headers.rs:288,308`,
  `bodies/request.rs:188`, `stages/src/stages/execution/mod.rs:377`).

In the box every block arrives via `newPayload`, so this never showed. On a
public network a joining node would sync history through these paths and
accept **any** mint in it — the same class of flaw the old chain died of
(trusting what arrives instead of re-deriving it). So:

**Decision 1 — C5 moves into consensus.** `SovaNode` gets a custom
`ConsensusBuilder` (today `EthereumConsensusBuilder`, `crates/engine/src/node.rs:66`)
whose `SovaConsensus` wraps `EthBeaconConsensus` and checks withdrawals against
the expectations map in `validate_block_pre_execution` (withdrawals are in the
body; no execution needed). The payload-path check stays as-is for candidate
observation and rank; the consensus check is the enforcement point every path
shares.

**Decision 2 — history sync waits for the follower.** A consensus error is
permanent (reth caches invalid blocks), so "I haven't scanned that Zcash
height yet" must never surface as invalid during sync. Rule: the node's
arbiter issues no FCU toward a sync target until its follower has scanned
Zcash through that target's epoch. A joining node therefore scans Zcash first
(fast: local zebrad RPC), then syncs Sova, and every historical block meets a
*known* expectation. At the live tip, where a Sova block can legitimately
outrun our zebrad by seconds, the existing behaviour holds: the payload path
observes it as an `Unknown` candidate at trust rank `usize::MAX` and the
consensus check defers — `Unknown` is **not** an error there, and the arbiter
re-checks once the scan catches up (the documented accept-unknown debt,
bounded to the tip).

Open question for implementation: whether `validate_block_pre_execution` can
distinguish "tip import" from "historical sync" cleanly, or whether the
deferral should key purely on "height ≤ follower-scanned height → enforce;
above → defer". The latter is simpler and is the default.

## Propagation: a Sova RLPx sub-protocol (not eth NewBlock)

Two reth-native options exist:

1. **eth `NewBlock` + custom `BlockImport`.** Wire support is still there on
   eth/66–71 (`eth-wire-types/src/message.rs:123-128`), but reth's default
   `Stake` network mode rejects NewBlock/NewBlockHashes at the session layer
   and disconnects the sender (`eth-wire/src/ethstream.rs:153-157`,
   `network/src/manager.rs:639-666`). Enabling it means `.with_pow()`, which
   turns off those guards network-wide, plus a custom `NetworkBuilder` built
   before the engine handle exists.
2. **A custom RLPx sub-protocol** (`ProtocolHandler`/`ConnectionHandler`,
   `network/src/protocol.rs:22-113`), added after launch with
   `node.network.add_rlpx_sub_protocol(..)` (`network.rs:255,566`; example
   `examples/custom-rlpx-subprotocol`). Keeps the Stake guards, defines our own
   messages and inbound limits.

**Decision 3 — option 2, `sova/1`.** Smallest blast radius, no global mode
flip, and the message set is ours:

- `Announce { height, hash }` — sent to all peers on accepting a block.
- `GetBlock { hash }` / `Block { rlp }` — fetch on an announcement we don't
  have. Blocks are pulled, not pushed, so a peer can't flood us with bodies.
- Received blocks are submitted **in-process** via
  `ConsensusEngineHandle::new_payload` (`engine/primitives/src/message.rs:342`;
  handle already used at `bin/sova/src/main.rs:188`). No authrpc, no JWT, and
  still through `convert_payload_to_block`, so candidate observation works.
- **Relay delivers, arbiter decides** is preserved by construction: `sova/1`
  has no message that can move a head. The only FCU source stays the local
  arbiter.
- `Syncing` from `new_payload` (parent unknown) is *not* peer misbehaviour
  (the in-tree BSC example gets this wrong, `examples/bsc-p2p/src/block_import/service.rs:108`);
  it triggers ancestor catch-up (below) instead of a reputation hit. Only a
  block that is actually `Invalid` costs the peer reputation.
- Dedup: seen-hash LRU; re-announce only blocks we accepted.

**Catch-up** uses what reth already has: the standard eth protocol still
serves headers/bodies in Stake mode, and an arbiter FCU to an unknown head
makes reth download it (≤ 32 blocks) or backfill (> 32)
(`engine/tree/src/tree/mod.rs:1385-1406, 2744-2816, 2951-2986`) — gated by
Decision 2.

## Discovery and isolation

- **Turn discovery on** for non-dev profiles (discv4 + discv5); it is off today
  (`.dev()` side effect / set by hand, `bin/sova/src/main.rs`).
- **Never fall back to Ethereum bootnodes.** reth resolves bootnodes as
  `--bootnodes` → config → `chain_spec.bootnodes()` → `mainnet_nodes()`
  (`node/core/src/args/network.rs:567-578`), and a custom chainspec returns
  `None` (`chainspec/src/spec.rs:783-793`). The `sova-testnet` profile sets its
  bootnode list explicitly (m1-a).
- **Unique genesis → unique fork ID.** Today every node runs reth's `DEV`
  genesis, identical to every `reth --dev` node on earth. Status handshakes
  and the ENR fork-ID filter isolate networks by genesis hash + fork schedule
  (`eth-wire/src/handshake.rs:139-200`, `network/src/swarm.rs:258-285`); chain
  ID is not in the genesis header, so the testnet genesis carries a Sova
  `extra_data` (m1-a). Run with `--enforce-enr-fork-id` semantics on.

### As built (step 4, `bin/sova/src/discovery.rs`)

- **Policy.** Discovery is on iff the profile is not `dev` and
  `SOVA_GOSSIP=p2p`. `dev` (box, sims, nightly) and the relay transport stay
  off, as before. `SOVA_DISCOVERY=off` opts a testnet node out;
  `SOVA_DISCOVERY=on` is refused on `dev` (its genesis/fork ID is every
  `reth --dev` node's, so discovery could not isolate it) and with the relay.
- **discv4 + discv5 on one UDP port**, the RLPx port: reth's shared-socket
  mode kicks in when both bind the same address and port
  (`network/src/discovery.rs:111-160`), so a plain `enode://` bootnode
  serves both (discv5 bootstraps an unsigned enode by `request_enr` to its
  UDP port). **DNS discovery off** (Sova has no EIP-1459 tree). Checked
  by hand with `RUST_LOG=net::discv5=trace` on three loopback nodes: each
  node's discv5 routing table held the other two, and every discovered
  ENR carried the `sova-testnet` `eth` fork hash `a872bd73`.
- **`enforce_enr_fork_id = true`**: a discovered peer joins the peer set
  only after its ENR `eth` fork ID has been fetched and validated against
  our fork filter (`network/src/swarm.rs:258-285`); the Status handshake is
  the second gate. Unit-tested: the `sova-testnet` fork filter rejects
  mainnet's and `reth --dev`'s fork IDs.
- **Bootnodes** are always pinned for non-dev profiles (m1-a), and
  `discovery::apply` refuses to enable discovery if the list is unset, so
  `mainnet_nodes()` is unreachable.
- **Binding.** `SOVA_P2P_ADDR` binds RLPx + discovery; `SOVA_NAT` is reth's
  `--nat`. The default `any` may probe UPnP and a public-IP service; the
  loopback sim uses `extip:127.0.0.1` so nothing leaves the host.
- **The `sova/1` registration gap is closed at build time.** Step 3
  registered the handler after launch (`add_rlpx_sub_protocol` on the
  handle, a message to the running manager). The RLPx listener and
  discovery start inside the component build, so a discovered or inbound
  peer could open a session before that message landed and keep it
  without `sova/1` for its lifetime. `engine::p2p::SovaNetworkBuilder`
  instead puts the handler in `NetworkConfigBuilder::add_rlpx_sub_protocol`
  (`network/src/config.rs:557`), so the `SessionManager` is created with it
  (`manager.rs:317`) and every session from the first offers `sova/1`. The
  handler needs only channels, so it is built before launch; the gossip
  service that drains them is spawned after launch. Events that arrive
  first wait in the channels. Rejected alternatives: delaying discovery
  until after registration doesn't cover inbound dials, and dropping and
  redialing sessions that lack `sova/1` would churn and race.
- **Proof:** `box/sim/three-node-discovery-scenario.sh` (see
  `box/sim/README.md`).

## Build order

1. m1-a lands (chainspec, bootnodes, unique genesis).
2. **SovaConsensus** (Decision 1) + follower gating (Decision 2), with a sim
   scenario: a fresh node joins a two-node chain 100+ blocks in, syncs through
   the backfill path, and (a) converges, (b) **rejects** a history where one
   epoch's mint was tampered — the regression test for the finding above.
3. `sova/1` sub-protocol + in-process submission; ladder scenario re-run over
   `sova/1` only, engine API bound to localhost, no shared JWT.
4. Discovery on for the testnet profile; a three-node scenario where the third
   node finds the others only via a bootnode. **Built** (see "As built" above).

AC (board m1-b): two nodes with no shared secret converge via P2P only; ladder
scenario green over P2P; engine API bound to localhost; the tampered-history
join scenario rejects.
