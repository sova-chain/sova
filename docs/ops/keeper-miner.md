# Keeper miner (disclosed)

Decision infra-2 **D8**: on the public testnet the project may run one
**keeper miner**. It is an ordinary `sova-miner` that keeps epochs from
going empty while outside miners are still few. It is allowed only as a
publicly disclosed miner, with its own key, on a machine that is not
public.

## Rules

- **No special privilege.** It competes in the same ranking as everyone
  else: weight (zatoshis burned) descending, then the smallest txid
  (`crates/consensus/src/epoch.rs`). No consensus rule, chainspec field,
  or client names its addresses. Anyone who burns more in an epoch
  outranks it, and strangers should be able to. That is why it burns a
  deliberately small amount.
- **Own key, own machine.** It is a fresh `sova-miner init` keystore,
  used only by the keeper. It is not the faucet key and not anyone's
  personal miner. It runs on a non-public machine that has its own
  zebrad, and it never runs on the seed, RPC, or faucet hosts, which
  hold no miner keys. The keeper talks only to its own zebrad
  (`--rpc http://127.0.0.1:18232`).
- **Budget-capped, twice.** `--budget-zat` caps each run.
  `--lifetime-budget-zat` is the hard ceiling for the whole testnet
  phase and counts spend across every run of this keystore (see the
  miner README, "Budgets").
- **It is part of the 24 h switch-off drill.** When the project's
  machines go dark for the M1 gate, the keeper goes dark too. Otherwise
  the drill proves nothing about strangers' liveness.

## Where it runs (M1 testnet)

On the M1 public testnet the keeper is `sova-keeper-1`, an **AWS EC2**
instance (Rob, 2026-09-23). The seed and RPC servers stay on Hetzner, so
the keeper doesn't share a provider with them. Rob creates it by hand
(`docs/ops/keeper-aws.md`), and the launch kit adopts it over SSH and sets
it up like every other host (`infra/testnet`, role `keeper`). Its
security group admits only SSH from the operator's IP, so it takes part in
Zcash and Sova P2P through outbound connections only.

The kit's layout differs from the generic commands below. The keystore is
`/var/lib/sova/keeper`, the burner is the `sova-keeper` systemd unit (its
budgets are in `/etc/sova/keeper.env`), and `sova` runs in mine mode as
`sova-node`. zebrad's RPC is `127.0.0.1:18232` with cookie auth off (a
documented deviation, `docs/ops/testnet-launch.md`), so `--rpc-cookie-file`
isn't needed there. `setup-host.sh` runs `init` itself and prints both
addresses (`out/servers/sova-keeper-1.keeper_*`).

## Set up

```bash
# On the keeper machine, with testnet zebrad synced and RPC on loopback:
sova-miner --network test --data-dir /var/lib/sova-keeper init
# Prints "t-addr to fund" and "evm address (SIP-1 credit)": publish both.
```

The EVM address is the keystore key's own Ethereum address, so the
keeper's SOVA is spendable: `sova-miner --data-dir /var/lib/sova-keeper
export-evm-key --i-understand` prints the key for an EVM wallet. That is
the same key that holds the keeper's TAZ, so export it only on the keeper
machine and only when a spend is actually needed.

**Keystores made before this default existed** record the t-addr's
hash160 as their EVM address. No key exists for that address, so the
SOVA it earns can never be spent. `init`, `mine` and `report` print a
`WARNING: ... LEGACY default EVM address ... UNSPENDABLE` line for such a
keystore; they keep crediting it until told otherwise, because the
sealing node has to switch at the same time. To fix it: stop the keeper
and its `bin/sova`, run `sova-miner --network test --data-dir
/var/lib/sova-keeper init --migrate-evm-address`, restart `bin/sova` with
the new `SOVA_MINER_EVM_ADDRESS`, start the keeper again, and update the
published EVM address. SOVA already credited to the old address stays
there and can't be moved.

Fund the keeper with project-mined TAZ, never from the faucet. Top it up
to about one run's budget at a time, with a **plain transfer** to its
t-addr, which can be spent after 1 confirmation. `sova-miner` finds it
with `getaddressutxos` and never scans the chain (miner README,
"Funding"). Coinbase paid straight to the t-addr (zebrad's internal miner
with `miner_address` set to it) **can't** fund a burn on testnet: it has
to be shielded and sent back first (next section).

## Coinbase must be shielded first

Zcash consensus on testnet and mainnet only lets a transparent coinbase
output be spent by a transaction whose outputs are **all shielded**.
Every burn has transparent outputs (the SIP-1 payload, the eater output,
change), so zebrad rejects a burn that spends coinbase:

```
unshielded transparent coinbase spend ... must be spent in a transaction which only has shielded outputs
```

Zebra's regtest allows it by default (`should_allow_unshielded_coinbase_spends
= true`), which is why the box funds its miners straight from
`generatetoaddress`. The testnet keeper can't.

`sova-miner` (and `sova-faucet`) therefore never select coinbase on
testnet or mainnet. `report --verify-rpc` shows it on its own line as
`coinbase (must be shielded first)`, and when coinbase is all there is,
`mine` stops with:

```
N zat of coinbase must be shielded before it can fund a transparent burn ... see docs/ops/keeper-miner.md#coinbase-must-be-shielded-first
```

The fix is a round trip through a shielded pool with a Zcash wallet. Sova
doesn't ship one: its wallet code is the transparent burn builder only.

1. **Shield.** Spend the coinbase (each output needs 100 confirmations)
   in a transaction that has no transparent outputs and pays everything,
   minus the ZIP-317 fee, to a **Sapling** address the wallet controls.
   Since NU6.3 no new value may enter Orchard, so don't use an Orchard or
   Unified receiver.
2. **Unshield.** Once the wallet sees that note as spendable, send
   from it to the keeper's t-addr. The result is an ordinary transparent
   output, spendable by `sova-miner` after 1 confirmation.

To avoid the round trip for future rewards, point zebrad's `[mining]
miner_address` at the wallet's Sapling address, so the coinbase lands
already shielded and only step 2 is needed. This changes zebrad's config,
so it needs a restart. Alternatively, top up the keeper with transfers
from a wallet that already holds non-coinbase TAZ.

Keep zebrad's cookie auth on (the default in zebrad 6.x) and give the
miner the cookie with `--rpc-cookie-file`, or with
`SOVA_MINER_RPC_COOKIE_FILE` in the service environment. The cookie is
`.cookie` in zebrad's `cookie_dir`, which by default is its cache
directory. zebrad writes a new cookie each time it starts. After an RPC
failure the miner reads the file again, so restarting zebrad doesn't mean
restarting the keeper. The user the keeper runs as must be able to read
the file, which zebrad creates with mode `0600`.

## Budget

A keeper burn costs `per-epoch-zat` plus the ZIP-317 fee, which is 20,000
zat for the usual one-input shape. There is at most one burn per Zcash
block. The box shows roughly one burn every other block, because the
miner waits for each burn to confirm. Testnet makes about 1,150 blocks a
day, so with `--per-epoch-zat 10000` a full day costs at most about
1,150 × 30,000 ≈ 0.35 TAZ.

```bash
sova-miner --network test --data-dir /var/lib/sova-keeper \
  --rpc-cookie-file /var/lib/zebrad/.cookie mine \
  --rpc http://127.0.0.1:18232 \
  --per-epoch-zat 10000 \
  --budget-zat 35000000 \
  --lifetime-budget-zat 1000000000   # 10 TAZ for the whole testnet phase
```

`sova-miner --network test --data-dir /var/lib/sova-keeper
--rpc-cookie-file /var/lib/zebrad/.cookie report --verify-rpc
http://127.0.0.1:18232` shows spend against both caps, and the keeper's
funding at the node: spendable, and any `coinbase (must be shielded
first)`. It also cross-checks every burn on chain from the keeper's first
epoch onward. Publish its totals alongside the disclosure.

## On / off

Run `mine` as a service, for example a systemd unit `sova-keeper` whose
`ExecStart` is the command above.

- **On:** `systemctl start sova-keeper`.
- **Off:** `systemctl stop sova-keeper`. State is saved after every
  broadcast and every confirmation. A burn still in flight when the
  keeper stops is picked back up on the next start: it is recorded if it
  was mined, or dropped at its expiry height. Its inputs are never spent
  twice.
- The service stops on its own when `--budget-zat` is used up. Restarting
  it starts a new run with a fresh per-run budget.
  `--lifetime-budget-zat` still holds.
- Log every on/off change, with its date, next to the disclosure.

To also *seal* when no one else is sealing, run `bin/sova` in mine mode on
the same machine with `SOVA_MINER_EVM_ADDRESS=<keeper EVM address>`, the
same wiring as `box/up.sh`. A burn-only keeper still counts toward
ranking, and rank-1+ fallbacks seal when the top burner is absent.

## Public disclosure text

> **Sova project keeper miner (testnet).** The Sova project runs one
> ordinary miner on the public testnet to keep epochs from going empty
> while outside miners are few. Zcash testnet t-address: `<t-addr>`.
> Sova EVM address (burn credit and rewards): `0x<evm>`. It has no
> special role: it burns a small fixed amount per epoch (`<per-epoch>`
> zat) and is outranked by anyone who burns more. Its total testnet spend
> is capped at `<lifetime cap>` TAZ, and its full burn history is
> verifiable on chain. We switch it off for the 24-hour infrastructure
> drill. Status and on/off log: `<link>`.
