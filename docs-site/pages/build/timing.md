---
title: Timing
description: Sova makes one block per Zcash block. How to design for it - batch, pipeline, wait for receipts, and count time in Zcash blocks.
---

Sova makes one block per Zcash block: about a minute on average, and a
five-minute gap is normal. Zcash testnet also has bursts of blocks a few
seconds apart. Design for few, full transactions.

## Four habits

- **Batch.** Put many calls in one transaction: Multicall3 at
  <!-- generate: address multicall3 --> on the testnet, or an
  EIP-7702 batch from your own account (Prague rules apply).
- **Pipeline.** Send a run of transactions with consecutive nonces at
  once. Don't wait for each receipt before sending the next.
- **Wait.** `eth_sendRawTransactionSync` returns the receipt. A node waits
  up to 300 s (`SOVA_SEND_SYNC_TIMEOUT_SECS`); the public RPC waits up to
  90 s. After a timeout, poll `eth_getTransactionReceipt`: the
  transaction is still in the pool.
- **Count Zcash blocks.** Use `anchor()`'s height, or `block.timestamp`
  (the Zcash block's time). Not `block.number`: Sova's block rate may
  change, Zcash's height won't. `ZecCheckout` counts its payment window
  in Zcash blocks.

## Settlement

Sova settles on Zcash. Mints are final at Zcash depth, and a transaction
is settled once three more epochs are built on it (about four minutes).
ZEC payments wait for their own confirmations: 3 on testnet, 10 on
mainnet.

On the testnet, the RPC's `safe` (3 blocks) and `finalized` (100 blocks)
labels are conveniences for tools, not guarantees:
[what this testnet is](../../../docs/guides/testnet-reference.md#what-this-testnet-is-and-what-it-isnt).

## Mapping heights

Sova block N anchors Zcash height N + B − 1, where B is the network's
epoch base. On the testnet B is `4388500`, so a burn in Zcash block `h`
is paid in Sova block `h − 4388500 + 1`. Contracts read the anchor with
`anchor()`.
