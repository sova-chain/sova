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
- **Nothing on our public servers can sign for a user or for consensus**
  (infra-m1 §4). Two keys are online by design: the faucet's hot key
  (D5), which only holds testnet TAZ, and, with SIP-6, the keeper's
  miner key, which seals the keeper's blocks on the non-public
  `sova-keeper-1`. The seed and RPC hosts hold neither (`setup-host.sh`
  checks).
- The day-one contracts are **ownerless**: no admin, no owner, no
  upgrade. After deployment, the deployer key has no power over them.

## The list

| Key | Made by | Lives | Backup | If stolen | Replace it |
| --- | --- | --- | --- | --- | --- |
| **Deployer** (EVM, throwaway) | Orchestrator: `./deploy-contracts.sh keygen` | `~/.config/sova-testnet/deployer/` on the machine running the kit: an encrypted foundry keystore plus a random password file, both mode 600 | None needed | The thief gets its leftover gas money, a few SOVA at most. They can't change the deployed contracts, which have no owner. The deployments file in git defines the official addresses, not the deployer's name. | Run `keygen` again after moving the old directory aside. Use a new one at each chain reset. |
| **Faucet hot key** (Zcash testnet t-addr) | `setup-host.sh` on `sova-faucet-1`, the first time it sets up (`sova-faucet init`) | `/var/lib/sova/faucet/keystore.json` on the faucet host only, mode 600, owned by the `sova-faucet` user | None, on purpose. A rebuilt host makes a new key. | The thief gets the faucet's TAZ balance, which is capped at 5 days of drips (10 TAZ). TAZ has no market value. The key can't touch SOVA or consensus. | Rebuild the host, or delete the keystore and re-run `deploy.sh --only sova-faucet-1`. Then fund the new t-addr, which `deploy.sh` prints. |
| **Keeper miner** (Zcash t-addr + its EVM address). With SIP-6 it is also the keeper's **sealing key**, online: `sova-node` signs the keeper's blocks with it | `setup-host.sh` on `sova-keeper-1`, the Hetzner keeper server (Rob, 2026-09-23: all four servers on Hetzner), via `sova-miner init` | On that server's Hetzner volume (`/var/lib/sova`) only, in two copies, each mode 600 and readable by one service user: `/var/lib/sova/keeper/keystore.json` (owner `sova-keeper`, the burner) and `/var/lib/sova/sealer/keystore.json` (owner `sova`, the node; `SOVA_SEALER_KEYSTORE`; each deploy refreshes it from the first). Never copied off it (no Hetzner snapshots or backups of that volume, no copy on the laptop). Its seal journal is `/var/lib/sova/node/seal-journal/`: **never delete it while `sova-node` runs** (it is what stops the node signing two blocks for one slot) | None. It is a disclosed testnet miner, so a new key is fine. A rebuild with a new volume makes a new one. | The thief gets its TAZ (topped up one run's budget at a time, about 0.35 TAZ a day) and its mined testnet SOVA. They could burn as "the keeper", and **sign blocks for the keeper's rank**: seal the epochs where the keeper's burn ranks, choosing their transactions and taking their fees, or equivocate and get the keeper's blocks for that slot demoted. They can't sign for another rank or change what an epoch mints. | `init` a new keystore, publish the new addresses in the disclosure, fund the new t-addr, then `./deploy.sh --only sova-keeper-1` (installs the new sealing copy, restarts `sova-node`) and restart `sova-keeper`. `docs/ops/keeper-miner.md`, "Sealing key (SIP-6)". |
| **Ashwings / market treasury** (EVM) | **Rob**, in his own wallet (a hardware wallet is recommended). A multisig later. | Rob's wallet. The kit only knows the public address: `ASHWINGS_TREASURY` in `config.env`, **`0x8d0123637062f8c15FD6AFDe6F834C85f4AfeEDE`** | Rob's wallet backup (seed phrase, kept offline) | The thief gets what the treasury has collected: mint proceeds and market fees, in testnet SOVA. They can't change prices, mint or pause, provided the contracts give the treasury no admin power (check this when `dapps/ashwings-v2` lands). | The address is a constructor argument, so it can't be changed. Redeploy: `./deploy-contracts.sh deploy --via sova-rpc-1 --redeploy ashwings --redeploy market` with the new address. That's cheap on testnet. On mainnet, use the multisig from day one. |
| **ZEC payee** (Zcash **testnet** t-addr, `tm…`) | Testnet: a **project key** made by the orchestrator (Rob, 2026-09-23: `tmQKm7CN5LaVg83YNRzXPLNy1qBMs8qcqNz`). Mainnet: **Rob**, in his own wallet | Testnet: a `sova-miner` keystore (mode 0600) on the orchestrator's SSD, `/Volumes/Extreme Pro/sova/ashwings-testnet-payee/`. The kit knows only the address: `ASHWINGS_ZEC_PAYEE` in `config.env` | Testnet: none needed (TAZ has no value). Mainnet: Rob's wallet seed | The thief gets the TAZ paid for ZEC-priced mints, which has no market value | Same as the treasury: it's a constructor argument, so redeploy with the new address |
| **Laptop test miner** (the stranger test) | Orchestrator: `sova-miner --network test init`, with the flag `--evm-address <deployer>` | `~/.sova-testnet-*-miner/` on the orchestrator's laptop | None | Its TAZ (a small budget) | `init` a new one |
| **Node P2P keys** (`discovery-secret`, one per node) | `setup-host.sh` pass 1 | `/var/lib/sova/node/discovery-secret` on each node host's volume, mode 600 | The volume. Losing it changes that node's enode. | Someone could impersonate that bootnode's identity. No funds and no consensus power. | Delete it, re-run `deploy.sh`, `bootnodes.sh` and `publish.sh join`, and update `EXTRA_BOOTNODES` / the `chain.rs` constant at the next release |
| **NEAR / wz.cash keys** | Later. Out of scope for M1 | — | — | — | — |

Account credentials (Hetzner and Cloudflare API tokens, the R2 key, the
SSH key, the Telegram bot) aren't wallets. Rob makes them in
testnet-launch.md step R3–R6, and they live only in
`~/.config/sova-testnet/secrets.env` (mode 600) and `~/.ssh/`. Rotate a
token by revoking it in the provider's dashboard and filling in the new
one. Since 2026-09-23 all four servers are on Hetzner, so there is no
AWS account in the default launch. If a keeper is ever run on AWS
instead (optional, `docs/ops/keeper-aws.md`), that account has **no** API
keys: the kit never calls AWS, and the instance is reached with the same
SSH key as the Hetzner hosts.

## Deployer: funding and use

The testnet genesis has no pre-funded accounts (m1-a), so SOVA exists only
after a burn. Fund the deployer by mining into it. That's the stranger
test's burn, so one step proves two things:

```bash
DEPLOYER=$(cat ~/.config/sova-testnet/deployer/address)
sova-miner --network test --data-dir ~/.sova-testnet-launch-miner init --evm-address "$DEPLOYER"
# fund that miner's t-addr with a little TAZ (plain transfer) and mine
# a few epochs while the keeper burns (B5b)
./smoke.sh balance "$DEPLOYER"          # the mint, via the public RPC
./deploy-contracts.sh plan --via sova-rpc-1   # estimate vs balance
```

With SIP-6 this miner is **burn-only**: its burns credit the deployer,
whose key is a foundry keystore, not a `sova-miner` one, so no node can
seal as it (a mine-mode laptop node with `SOVA_MINER_EVM_ADDRESS=$DEPLOYER`
would need that key and refuses to start without it). It doesn't need
to: when it outranks the keeper, the keeper seals the epoch at rank 1 one
`rank_step` later, and every sealed block pays every ranked burner its
share. An epoch where the keeper didn't also burn has nobody ranked to
seal it, so it gets a null block and mints nothing. The keeper burns
about every other block, so start it first and give the laptop miner a
few epochs of budget.

The whole kit costs about 13.8 M gas (measured on anvil, 2026-09-23).
One epoch's reward covers that many times over. `deploy` records each
address in `infra/testnet/deployments/sova-testnet.json` as soon as its
deployment is mined. Re-running it deploys nothing. Commit that file. It
is the canonical list of day-one addresses.

## Before mainnet

- The treasury becomes a multisig, and Rob holds a quorum of its keys.
- There is no project faucet and no hot keys on mainnet.
- The deployer stays a throwaway. The contracts stay ownerless.
