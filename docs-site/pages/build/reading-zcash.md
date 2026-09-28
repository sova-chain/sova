---
title: Read Zcash from a contract
description: The SIP-4 precompile lets a Sova contract read transparent Zcash state. Every node answers from its own Zcash node, and every node gives the same answer.
---

A Sova contract can read Zcash. Every node answers from its own Zcash
node, and every node gives the same answer. The reads live at the SIP-4
precompile, `0x0000000000000000000000000000000000005a00`, live on the
public testnet.

## What a contract can read

| Method | Answer |
| --- | --- |
| `anchor()` | The Zcash block this Sova block commits to: height and hash. |
| `blockAt(h)` | A Zcash block's hash and header time. |
| `txInfo(txid)` | Where a transaction was mined and how deep. Fully shielded transactions too. |
| `txOutput(txid, vout)` | What a transparent output pays, and to which script. |
| `burnInfo(txid)` | The SIP-1 burn a transaction carries: who it credits and how much ZEC it destroyed. |

`spentBy(txid, vout)`, for "has this output been spent", follows in v1.1.

## Checking a payment

Most contracts use `ZcashLib`, which turns a check into one call:

```solidity
// paid at least `price` zatoshis to the seller, 3 or more blocks deep?
ZcashLib.Payment memory p = ZcashLib.requireOutputPays(
    txid, vout, ZcashLib.p2pkh(sellerHash), price, 3);
```

txids and hashes are display order, as explorers print them. Values are
integer zatoshis.

## The rules

- **Deterministic.** Every answer is a pure function of the Zcash chain
  the block commits to. Two nodes that accept the same block give the
  same answers, whatever their own Zcash tip.
- **Not found is a result.** A status, identical on every honest node. A
  node that can't answer yet holds the block and retries.
- **Confirmations** are counted from the committed Zcash block.
  `ZcashLib` asks for a minimum depth on every check: 3 on testnet, 10 on
  mainnet (about 12.5 minutes), more for large amounts.
- **Reorgs.** A Zcash reorg deeper than a payment reorgs Sova with it,
  identically on every node. Your minimum depth protects whatever happens
  off Sova.
- **txids.** Pre-v5 txids can change before they are mined. Key on what
  an output pays, or reserve the order before payment.

## Shielded stays shielded

The precompile reads transparent Zcash state. Shielded amounts,
recipients, senders and memos stay private, from contracts as from
everyone. A shielded wallet pays a transparent address with a z→t send;
that one output is public, and it is what a contract verifies.

## Pool state and events (SIP-7)

The same precompile also answers pool-level questions: the value in each
Zcash pool after a block, the change a block made, a block's public
activity counts, and a transaction's shielded flow. These are aggregates
and public counts.

Each Sova block also records a summary of its Zcash block in the
`ZcashBlocks` system contract at `0x0000000000000000000000000000000000005A01`.
It keeps the last 8,191 summaries. Anyone can read them, and anyone can
call `publish(fromH, toH)` to turn recorded heights into ordinary
`ZcashBlock` logs. Nodes also stream the summaries over RPC:
[`sova_getZcashBlocks`](../reference/rpc.md#sova-methods).

## Patterns

- **Sell for ZEC.** The buyer reserves an order, pays the seller's own
  Zcash address from any wallet, and anyone calls claim. The contract
  verifies the payment and delivers. `ZecCheckout.sol`, live in the
  [Ashwings](../start/ashwings.md) mint.
- **Swap ZEC for SOVA.** A maker locks SOVA and names a price. A taker
  reserves with a small bond, pays the ZEC straight to the maker on Zcash,
  and claims the SOVA. `ZecEscrow.sol`.
- **Proof-of-burn.** Apps recognize their own burns, marked with their
  own payload: names, Sybil-resistant badges, spam fees that burn. SIP-4 §9.

Source: [`contracts/src/zcash/`](../../../contracts/src/zcash). The full
text: [SIP-4](../../../sips/sip-4-draft-zcash-state-precompile.md) and [SIP-7](../../../sips/sip-7-draft-zcash-events.md).

## Try it locally

The contracts, a stand-in for the precompile, the checkout page and its
relayer, all on your machine. Needs Foundry, Node 22 and a Chromium
(`CHROME=/path`).

```bash
git clone https://github.com/sova-chain/sova && cd sova
(cd contracts && forge build && forge test)
(cd site && npm ci && npm run build)
cd tools/checkout-relayer && npm ci
npm run e2e
```

`e2e` starts anvil, puts a mock at `0x…5a00`, deploys the checkout, and
drives a headless browser through a ZEC purchase: reserve, pay, confirm,
claim, owl.

## The interfaces

<!-- include: contracts/src/zcash/IZcash.sol -->

<details>
<summary><code>IZcashPools.sol</code> (SIP-7 pool reads, same address)</summary>

<!-- include: contracts/src/zcash/IZcashPools.sol -->

</details>
