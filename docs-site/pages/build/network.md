---
title: Connect a wallet
description: Sova testnet network parameters for wallets and tools. Chain ID 82330, currency SOVA, public RPC and explorer.
---

Add the Sova testnet to any EVM wallet or tool:

| Setting | Value |
| --- | --- |
| Network name | Sova testnet |
| Chain ID | `82330` (`0x1419a`) |
| Currency | `SOVA`, 18 decimals |
| RPC | `https://rpc.testnet.sova.io` |
| Explorer | [`https://explorer.testnet.sova.io`](https://explorer.testnet.sova.io) |

The Ashwings mint page adds the network for you. Foundry works as is:
`cast balance --ether --rpc-url https://rpc.testnet.sova.io <address>`.

## Getting SOVA

SOVA comes only from burning ZEC. [Mine some](../start/mine.md) with testnet
ZEC from the faucet at
[`faucet.testnet.sova.io`](https://faucet.testnet.sova.io). A miner's key
is an ordinary EVM key: `sova-miner export-evm-key --i-understand` prints
it for your wallet ([spending it](../../../docs/guides/testnet-reference.md#5-check-your-earnings)).

## The public RPC

It is read-and-broadcast only (no signing, no admin or debug methods) and
allows 50 requests per 10 seconds per IP. Your own node has no such
limit, and serves the same chain at `http://127.0.0.1:8545`.
The method list: [RPC methods](../reference/rpc.md).

## Local networks

The box on your machine runs its own chain at `http://127.0.0.1:8545`,
chain ID `1337`: [Run it locally](../../../box/up/README.md).
