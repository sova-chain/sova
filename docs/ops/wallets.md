# Testnet keys and addresses

Every key and address the M1 public testnet uses: who makes it, where it
lives, how it's backed up, what a thief could do with it, and how to
replace it. Launch order: `docs/ops/testnet-launch.md`.

## Rules for all of them

- **Never in git.** Only public addresses are committed (`config.env`,
  `deployments/*.json`, the keeper disclosure).
- **Never pasted into chat**, a ticket or a doc. Hand over a public
  address, or say "done" once a file is filled in.
- **Rob's keys stay with Rob.** The orchestrator's machine holds only
  testnet-only throwaways (the deployer, a test miner). It never holds
  Rob's treasury key, his Zcash wallet, or any mainnet key.
- **Nothing on our servers can sign for a user or for consensus**
  (infra-m1 §4). The faucet's hot key is the one exception (D5), and it
  only holds testnet TAZ.
- The day-one contracts are **ownerless**: no admin, no owner, no
  upgrade. After deployment, the deployer key has no power over them.

## The list

| Key | Made by | Lives | Backup | If stolen | Replace it |
| --- | --- | --- | --- | --- | --- |
| **Deployer** (EVM, throwaway) | Orchestrator: `./deploy-contracts.sh keygen` | `~/.config/sova-testnet/deployer/` on the machine running the kit: an encrypted foundry keystore plus a random password file, both mode 600 | None needed | The thief gets its leftover gas money, a few SOVA at most. They can't change the deployed contracts, which have no owner. The deployments file in git defines the official addresses, not the deployer's name. | Run `keygen` again after moving the old directory aside. Use a new one at each chain reset. |
| **Faucet hot key** (Zcash testnet t-addr) | `setup-host.sh` on `sova-faucet-1`, the first time it sets up (`sova-faucet init`) | `/var/lib/sova/faucet/keystore.json` on the faucet host only, mode 600, owned by the `sova-faucet` user | None, on purpose. A rebuilt host makes a new key. | The thief gets the faucet's TAZ balance, which is capped at 5 days of drips (10 TAZ). TAZ has no market value. The key can't touch SOVA or consensus. | Rebuild the host, or delete the keystore and re-run `deploy.sh --only sova-faucet-1`. Then fund the new t-addr, which `deploy.sh` prints. |
| **Keeper miner** (Zcash t-addr + its EVM address) | `setup-host.sh` on `sova-keeper-1` (`sova-miner init`), or on the laptop if the keeper runs there | `/var/lib/sova/keeper/` on the keeper machine only, mode 600 | None. It is a disclosed testnet miner, so a new key is fine. | The thief gets its TAZ (topped up one run's budget at a time, about 0.35 TAZ a day) and its mined testnet SOVA. They could also burn as "the keeper", which only earns them testnet SOVA. | `init` a new keystore, publish the new addresses in the disclosure, fund the new t-addr, restart `sova-keeper` and `sova-node`. |
| **Ashwings / market treasury** (EVM) | **Rob**, in his own wallet (a hardware wallet is recommended). A multisig later. | Rob's wallet. The kit only knows the public address: `ASHWINGS_TREASURY` in `config.env`, **`0x8d0123637062f8c15FD6AFDe6F834C85f4AfeEDE`** | Rob's wallet backup (seed phrase, kept offline) | The thief gets what the treasury has collected: mint proceeds and market fees, in testnet SOVA. They can't change prices, mint or pause, provided the contracts give the treasury no admin power (check this when `dapps/ashwings-v2` lands). | The address is a constructor argument, so it can't be changed. Redeploy: `./deploy-contracts.sh deploy --via sova-rpc-1 --redeploy ashwings --redeploy market` with the new address. That's cheap on testnet. On mainnet, use the multisig from day one. |
| **ZEC payee** (Zcash **testnet** t-addr, `tm…`) | **Rob**, in his own Zcash wallet | Rob's wallet. The kit knows only the address: `ASHWINGS_ZEC_PAYEE` in `config.env` (**pending from Rob**) | Rob's wallet seed | The thief gets the TAZ paid for ZEC-priced mints, which has no market value | Same as the treasury: it's a constructor argument, so redeploy with the new address |
| **Laptop test miner** (the stranger test) | Orchestrator: `sova-miner --network test init`, with the flag `--evm-address <deployer>` | `~/.sova-testnet-*-miner/` on the orchestrator's laptop | None | Its TAZ (a small budget) | `init` a new one |
| **Node P2P keys** (`discovery-secret`, one per node) | `setup-host.sh` pass 1 | `/var/lib/sova/node/discovery-secret` on each node host's volume, mode 600 | The volume. Losing it changes that node's enode. | Someone could impersonate that bootnode's identity. No funds and no consensus power. | Delete it, re-run `deploy.sh`, `bootnodes.sh` and `publish.sh join`, and update `EXTRA_BOOTNODES` / the `chain.rs` constant at the next release |
| **NEAR / wz.cash keys** | Later. Out of scope for M1 | — | — | — | — |

Account credentials (Hetzner and Cloudflare API tokens, the R2 key, the
SSH key, the Telegram bot) aren't wallets. Rob makes them in
testnet-launch.md step R3–R6, and they live only in
`~/.config/sova-testnet/secrets.env` (mode 600) and `~/.ssh/`. Rotate a
token by revoking it in the provider's dashboard and filling in the new
one.

## Deployer: funding and use

The testnet genesis has no pre-funded accounts (m1-a), so SOVA exists only
after a burn. Fund the deployer by mining into it. That's the stranger
test's burn, so one step proves two things:

```bash
DEPLOYER=$(cat ~/.config/sova-testnet/deployer/address)
sova-miner --network test --data-dir ~/.sova-testnet-launch-miner init --evm-address "$DEPLOYER"
# fund that miner's t-addr with a little TAZ (plain transfer), run the
# laptop node with SOVA_MINER_EVM_ADDRESS="$DEPLOYER", and mine one epoch
./smoke.sh balance "$DEPLOYER"          # the mint, via the public RPC
./deploy-contracts.sh plan --via sova-rpc-1   # estimate vs balance
```

The whole kit costs about 13.8 M gas (measured on anvil, 2026-09-23).
One epoch's reward covers that many times over. `deploy` records each
address in `infra/testnet/deployments/sova-testnet.json` as soon as its
deployment is mined. Re-running it deploys nothing. Commit that file. It
is the canonical list of day-one addresses.

## Before mainnet

- The treasury becomes a multisig, and Rob holds a quorum of its keys.
- There is no project faucet and no hot keys on mainnet.
- The deployer stays a throwaway. The contracts stay ownerless.
