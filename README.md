# Sova

The programmable edge of the shielded pool. Sova is an EVM chain that can
see Zcash: contracts verify real ZEC payments on Zcash itself (the SIP-4
precompile). Permissionless, oracle-less, self-custody. SOVA, the gas, is
mined by burning ZEC, and every node verifies every mint against its own
Zcash node.

Pre-release and unaudited: read [`SECURITY.md`](SECURITY.md) before relying
on it, and to report a vulnerability. Site: <https://sova.io>. Protocol
changes go through [SIPs](sips/).

## Quickstart

Run a full local burn-to-mine devnet (Zcash regtest + Sova node + miner)
with one command -- see [`box/README.md`](box/README.md):

```bash
git clone https://github.com/sova-chain/sova && cd sova
./box/up.sh          # up; the first run gets the node and miner binaries
./box/up.sh down     # clean teardown
```

How long the first run takes depends on where the binaries come from. From
a checkout of a tagged release, it downloads prebuilt binaries and the box
is mining in under a minute. No release is tagged yet, so today the first
run builds from source: about 11 minutes on an idle Apple Silicon laptop,
up to about 25 on a busy one. Later runs reuse the binaries and take under
a minute.

## Layout

| Path                  | Purpose                                                                |
| ---------------------- | ----------------------------------------------------------------------- |
| `crates/chainspec`    | Sova chain constants, genesis configuration                           |
| `crates/consensus`    | Burn-to-mine consensus client: Zcash follower, burn parser, sealer, gossip |
| `crates/engine`       | Sova Engine API payload/node types                                    |
| `crates/evm`          | Sova EVM extensions: settlement transactions, Zcash query precompile  |
| `crates/burn-wallet`  | Transparent-only Zcash burn transaction builder, `sova-miner` CLI, TAZ faucet (a separate workspace) |
| `crates/miner`        | Miner daemon logic shared by CLI and MCP                              |
| `bin/sova`            | Sova node binary                                                       |
| `bin/sova-miner`      | Sova miner CLI binary                                                  |
| `box`                 | sova-in-a-box: one-command local devnet ([`box/README.md`](box/README.md)) |
| `contracts`           | Day-one dapp kit (Foundry): WSOVA, Uniswap-V2-class AMM, Multicall3, Ashwings |
| `mcp`                 | MCP server wrapping the miner CLI                                      |
| `sips`                | Sova Improvement Proposals (protocol specs)                            |
| `site`                | Project website                                                        |

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). Protocol changes go through a
SIP ([`sips/`](sips)).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option, **except** third-party code
that keeps its own license. Most notably, the vendored Uniswap V2 contracts
in `contracts/src/vendor/{v2-core,v2-periphery,uniswap-lib}` are
**GPL-3.0**, and the AMM contracts the dapp kit deploys are compiled from
that GPL-3.0 source. See
[`NOTICE`](NOTICE) for the full list and
[`contracts/src/vendor/README.md`](contracts/src/vendor/README.md) for
provenance and the one modification.
