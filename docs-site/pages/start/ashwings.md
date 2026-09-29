---
title: Mint an Ashwing
description: Ashwings are on-chain owls whose contract draws each one itself. Mint one on the testnet with SOVA or testnet ZEC.
---

Ashwings are on-chain owls whose contract draws each one itself. The
testnet mint is open at [sova.io/ashwings](https://sova.io/ashwings), and
testnet owls go away when the testnet resets.

There are two ways to pay:

- **625 SOVA**, from the SOVA you [mined](../../../docs/guides/testnet.md#3-mine-burn-taz-earn-sova).
  Connect a wallet on chain 82330 (the mint page adds the network for
  you) holding that SOVA and a little extra for gas.
- **0.05 testnet ZEC (TAZ)**, from a Zcash wallet running on testnet that
  can pay a transparent (`tm…`) address (tested with `zcash-devtool`),
  funded with TAZ from the [faucet](../../../docs/guides/testnet.md#3b-get-taz-from-the-faucet).
  `sova-miner` only burns, so it can't pay here. A mainnet Zcash wallet
  will refuse the address.

Paying in TAZ, the buy page reserves your order through a relayer, so you
need no SOVA, only an address to receive the owl. Pay the exact amount it
shows within 40 Zcash blocks (about 50 minutes today, about 17 after NU7
on 6 October), and the owl arrives about three Zcash blocks later.

The contract checks the ZEC payment on Zcash itself, through the SIP-4
precompile: [Read Zcash from a contract](../build/reading-zcash.md).
Addresses: [Contracts](../build/contracts.md).
