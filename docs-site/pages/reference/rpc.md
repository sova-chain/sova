---
title: RPC methods
description: The Sova node speaks standard Ethereum JSON-RPC, plus the sova_ namespace for the SIP-7 Zcash block feed.
---

A Sova node speaks standard Ethereum JSON-RPC (reth). Wallets, Foundry,
viem and ethers work unchanged. On top, the `sova` namespace streams the
Zcash blocks the chain anchors.

## Public RPC

`https://rpc-testnet.sova.io`, and any node run with
`SOVA_RPC_PROFILE=public`, serves only these methods:

<!-- generate: rpc-allowlist -->

It allows 50 requests per 10 seconds per IP. Your own node, on the default
`local` profile, serves reth's full `eth`, `net` and `web3` namespaces.

`eth_call`, `eth_estimateGas` and `eth_createAccessList` answer at block
tag `pending` as at `latest`.

## Sova methods

Registered when SIP-7 is on (`SOVA_SIP7=1`, as on the testnet).

### `sova_getZcashBlocks`

```json
{"jsonrpc":"2.0","id":1,"method":"sova_getZcashBlocks","params":[fromHeight, toHeight]}
```

The summaries of the Zcash heights `fromHeight` to `toHeight`
(inclusive, at most 1,000 per call) that the canonical Sova chain
anchors. Heights are numbers, decimal strings or `0x` hex. The answer
stops at the first height the chain doesn't anchor yet, so page with
`fromHeight = last + 1`.

Each item (zatoshi amounts as decimal strings; a pool delta is value
**into** the pool; `hash` is the Zcash block hash as zebrad prints it):

```json
{"height": 4384200, "hash": "0x0080…c2bc", "time": 1790184215, "sovaBlock": 4384200,
 "pools": {"transparent": "1573837835978306", "sprout": "…", "sapling": "…",
           "orchard": "…", "lockbox": "…", "ironwood": "…"},
 "chainSupply": "1823100637835043",
 "deltas": {"transparent": "12500000", …, "ironwood": "125000000"},
 "stats": {"txCount": 1, "shieldedTxCount": 1, "tIn": 0, "tOut": 1, "saplingSpends": 0,
           "saplingOutputs": 0, "orchardActions": 0, "ironwoodActions": 2, "joinSplits": 0},
 "trees": {"sapling": 404304, "orchard": 248902, "ironwood": 354039}}
```

### `sova_subscribe("zcashBlocks")`

WebSocket only (`SOVA_WS_PORT`; the public RPC has no WebSocket).
Notifications arrive on `sova_subscription`, one item per Zcash height,
in order, as soon as the canonical Sova head anchors it. Cancel with
`sova_unsubscribe`.

```json
{"jsonrpc":"2.0","id":1,"method":"sova_subscribe","params":["zcashBlocks", {"fromHeight": 4390000}]}
```

The optional `fromHeight` replays from that height, at most 1,000 below
the current anchored height. When a reorg voids heights already sent, a
rollback item comes first, then the new branch from `toHeight + 1`:

```json
{"rollback": {"toHeight": 4390123}}
```

Everything above `toHeight` is void, like `removed: true` on eth logs.

The same summaries are in contract state at `ZcashBlocks`
([Read Zcash](../build/reading-zcash.md#pool-state-and-events-sip-7)). The
full design: [SIP-7 §4](../../../sips/sip-7-draft-zcash-events.md).
