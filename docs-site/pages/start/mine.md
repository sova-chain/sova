---
title: Mine
description: Burn ZEC with sova-miner, earn SOVA. Where to mine and how you're paid.
---

Mining Sova means burning ZEC. `sova-miner` sends one burn per new Zcash
block, inside a budget you set, and the burns credit your EVM address
with SOVA.

```bash
sova-miner --network test --data-dir ~/.sova-testnet/miner init
sova-miner --network test --data-dir ~/.sova-testnet/miner mine \
  --rpc http://127.0.0.1:18232 \
  --per-epoch-zat 10000 \
  --budget-zat 5000000
```

## Where to mine

| Where | What you burn | Start here |
| --- | --- | --- |
| Public testnet | TAZ (testnet ZEC), from the faucet | [Quickstart, step 3](../../../docs/guides/testnet.md#3-mine-burn-taz-earn-sova) |
| Your machine | Regtest ZEC, made on the spot | [Run it locally](../../../box/up/README.md) |
| An agent | Either, with a hard budget | [Let an agent mine](../../../docs/guides/testnet-reference.md#let-an-agent-mine) |

## How it pays

Each Zcash block is an epoch. Burners rank by ZEC burned, the top burner
seals the Sova block, and the epoch's SOVA is split by burn weight.
The exact rules: [How SOVA is paid](../../../docs/guides/testnet-reference.md#how-sova-is-paid).

Seal your own blocks to earn the tip:
[step 4](../../../docs/guides/testnet-reference.md#4-optional-seal-with-your-keystore-sip-6).

## Details

- Every flag, budgets, funding and broadcast: [`sova-miner` CLI](../../../crates/burn-wallet/miner/README.md).
- Burn from a shielded wallet so the source of your ZEC stays private:
  [Anonymous funding](../../../crates/burn-wallet/miner/README.md#anonymous-funding).
- The burn format itself: [SIP-1](../../../sips/sip-1.md).
