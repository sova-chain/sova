---
title: Node configuration
description: How the sova node is configured - its three commands, the SOVA_* environment, RPC profiles, ports and the data directory.
---

`sova` is configured by environment variables. On the command line it
takes only these:

<!-- generate: sova-usage -->

Anything else is refused, so a typo never starts a node.

## The environment

Joining the public testnet sets everything through `testnet.env`. Every
variable, with its testnet value:
[the `sova` environment](../../../docs/guides/testnet-reference.md#the-sova-environment).

A few matter beyond the testnet:

| Variable | Default | Notes |
| --- | --- | --- |
| `SOVA_CHAIN` | `dev` | `dev` (a local chain, chain ID 1337, with publicly keyed prefunded accounts) or `sova-testnet` |
| `SOVA_GOSSIP` | `relay` | `p2p` for devp2p (`sova/1`) with discovery; the testnet uses `p2p` |
| `SOVA_RPC_PROFILE` | `local` | `local`: reth's standard `eth`, `net` and `web3` over HTTP on 127.0.0.1. `public`: read-and-broadcast only, for an RPC strangers reach ([the method list](rpc.md)); WebSocket off |
| `SOVA_RPC_CORS` | unset (no CORS headers) | `*` or a comma-separated list of origins, for browser pages calling your node |
| `SOVA_WS_PORT` | unset (no WebSocket) | WebSocket RPC on 127.0.0.1. Needed for `sova_subscribe("zcashBlocks")` |
| `SOVA_SEND_SYNC_TIMEOUT_SECS` | `300` | How long `eth_sendRawTransactionSync` waits for the receipt. Whole seconds, 1 to 3600 |
| `SOVA_CROSS_BLOCK_CACHE_MB` | `256` | reth's cross-block state cache, in MiB (reth's own default is 4 GiB). 32 to 16384 |

A value out of range stops the node at startup with a message naming the
variable. Other `SOVA_*` variables in the source serve the simulation
harness.

## Ports

| Port | What | Binds |
| --- | --- | --- |
| `8545` | HTTP JSON-RPC (`SOVA_HTTP_PORT`) | 127.0.0.1 |
| `8551` | Engine API (`SOVA_AUTH_PORT`) | 127.0.0.1 |
| `30303` | P2P, TCP and UDP (`SOVA_P2P_PORT`) | `SOVA_P2P_ADDR`, default `0.0.0.0` |
| set by `SOVA_WS_PORT` | WebSocket JSON-RPC | 127.0.0.1 |

Inbound P2P is optional: a node works with outbound connections only.

## The data directory

`SOVA_DATADIR` holds the chain, the node key (`discovery-secret`) and, on
a sealing node, `seal-journal/`. Keep it across restarts, and never delete
the seal journal: it stops a restart from signing two blocks for one slot.
[Sealing](../../../docs/guides/testnet-reference.md#4-optional-seal-with-your-keystore-sip-6).

## Running as a service

The project's hosts use
[`sova-node.service`](../../../infra/testnet/host/systemd/sova-node.service)
with an `EnvironmentFile=`. If you adapt it, write `SOVA_DATADIR` as an
absolute path: systemd doesn't expand `$HOME`. Stop the node with SIGTERM
so it writes its recent blocks first:
[keeping it running](../../../docs/guides/testnet-reference.md#keeping-it-running).
