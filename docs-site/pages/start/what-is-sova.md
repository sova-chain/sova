---
title: What is Sova
description: An EVM chain that runs beside Zcash. SOVA is minted only by burning ZEC, and contracts can read Zcash.
---

Sova is an EVM chain that runs beside Zcash. Every Sova node runs its own
Zcash node, so contracts can check real ZEC payments and every node checks
every mint.

SOVA, the gas coin, is minted only by burning ZEC. One Sova block follows
each Zcash block.

:::caution[Pre-release]
Sova is pre-release and unaudited. The public testnet runs on Zcash
testnet, so mining costs only testnet ZEC. Read
[`SECURITY.md`](../../../SECURITY.md) before relying on it.
:::

## How it works

- **Burn to mine.** A burn is one transparent Zcash transaction that pays
  ZEC to a provably unspendable script and names the EVM address to credit
  ([SIP-1](../../../sips/sip-1.md)). Burning is the only way SOVA is issued.
- **One Zcash block, one Sova block.** Each Zcash block is an epoch, so
  Zcash's proof-of-work orders Sova. Burners rank by burn weight, and the
  top burner seals the block ([SIP-2](../../../sips/sip-2.md)).
- **Exact mints.** Each epoch's reward is a 10% sealer tip plus a pool
  split pro rata by burn weight, paid as the block's withdrawals.
  [SIP-3](../../../sips/sip-3.md) sets the schedule: 6,250 SOVA per epoch
  after a 20,000-epoch slow start, halving every 1,680,000 epochs.
- **Every node checks every mint.** Each node runs its own `zebrad`,
  re-derives every mint from it, and rejects blocks that disagree,
  including the history a new node syncs.
- **Contracts that read Zcash.** The SIP-4 precompile answers questions
  about transparent Zcash state (is this transaction mined, how deep, what
  does this output pay) as of the Zcash block each Sova block commits to.
  Shielded data stays shielded. [Read Zcash from a contract](../build/reading-zcash.md).

## Where to go next

- [Join the testnet](../../../docs/guides/testnet.md): mine testnet SOVA with testnet ZEC.
- [Run it locally](../../../box/up/README.md): the whole loop on your machine, in a minute.
- [The specs](../specs/index.md): every protocol rule, as SIPs.
- The long form: [the whitepaper](https://sova.io/paper).
