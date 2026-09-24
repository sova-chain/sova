# sova-miner

Budget-capped, per-epoch SIP-1 burn mining against a `zebrad`-compatible
node. Four subcommands: `init` (create the keystore, print the t-address
to fund and the EVM address burns credit), `mine` (the burn loop),
`report` (spend/earnings summary, with optional on-chain verification),
and `export-evm-key` (print the key, to spend your SOVA from an EVM
wallet).

## Your SOVA: the EVM address and its key

Every burn names the EVM address Sova mints SOVA to. By default `init`
uses the **Ethereum address of the keystore's own secp256k1 key**
(`keccak256(uncompressed_pubkey[1..])[12..]`, the derivation every EVM
wallet uses). One key therefore holds both the ZEC at the t-address and
the SOVA at the EVM address:

```bash
sova-miner init
# t-addr to fund:              tm...
# evm address (SIP-1 credit):  0x...
# import this key into an EVM wallet to spend your SOVA (`sova-miner export-evm-key`)

sova-miner export-evm-key --i-understand   # prints the key (0x + 64 hex) on stdout
```

Paste that key into your wallet's "import private key" field (MetaMask:
Add account → Import account) and the wallet shows the same EVM address
`init` printed. Anyone who sees the key can take both your SOVA and the
ZEC at your t-address, so `export-evm-key` refuses to run without
`--i-understand` and prints its warnings on stderr.

`init --evm-address <hex>` credits some other address instead (one whose
key you hold elsewhere). Re-running `init` keeps whatever address is
already recorded in `state.json`; `--evm-address` or
`--migrate-evm-address` change it, and `init` prints the old and new
address when they do. Restart your Sova node with the new
`SOVA_MINER_EVM_ADDRESS` after a change.

**Legacy keystores.** Before this default, `init` credited the
t-address's hash160 reinterpreted as an EVM address. No key exists for
that address: **SOVA credited to it is unspendable**. `init`, `mine` and
`report` detect it (it equals the hash160 of this keystore's t-address)
and print a `WARNING` naming both addresses, but keep crediting it rather
than moving your burns without being asked. Fix it with
`sova-miner init --migrate-evm-address`, which switches to the key's own
Ethereum address. SOVA already credited to the legacy address stays
there.

## Budgets

`--budget-zat` is **per-invocation**: each `mine` run declares its own
budget and only its own spend counts against it, regardless of what
earlier runs already spent against this keystore. A fresh `mine`
invocation always has its full declared budget available, even
immediately after a prior run exhausted its own — this matches what
"budget for this run" means to everyone who reads the flag's help text,
and it's the behavior `mine` has as of D5 (earlier builds checked this
flag against the keystore's lifetime total instead, which surprised the
`box/sim` harness — see `box/sim/README.md`'s "script bug found and
fixed").

`--lifetime-budget-zat` is an optional *additional* cap, checked against
the keystore's cumulative spend across every `mine` invocation it has
ever made — the old lifetime-accounting behavior, opt-in for anyone who
wants a hard ceiling that survives across runs. It's declared fresh each
invocation exactly like `--budget-zat` (omitting it on a later run clears
any cap a previous run set, rather than silently carrying one forward),
and it gates independently: a generous `--budget-zat` this run does not
override a tighter `--lifetime-budget-zat`, and vice versa. Lifetime
totals (`total burned`/`total fees`/`total spent`, plus the lifetime cap
and its remaining headroom if one is set) are always visible in `sova-miner
report`, whether or not `--lifetime-budget-zat` is in use.

## Funding

`mine` finds its funding with zebrad's `getaddressutxos` for its own
t-address (zebrad answers it from its address index, well under a second
even at testnet's ~3.9M blocks). Nothing scans the chain. These
confirmed outputs paying the t-address can fund burns:

- **Ordinary transfers** (a faucet drip, a z→t deshield, a top-up from
  another wallet) are spendable from 1 confirmation.
- **Coinbase outputs, on regtest only** (`generatetoaddress`), once they
  have 100 confirmations. `getaddressutxos` doesn't say which outputs are
  coinbase, so for an output younger than 100 blocks `mine` asks
  `getrawtransaction` and caches the answer.
- **Coinbase on testnet and mainnet is never spent.** There, consensus
  only lets transparent coinbase be spent into a transaction whose
  outputs are all shielded, and a burn always has transparent outputs,
  so zebrad would reject it. `mine` looks up every output's coinbase-ness
  (`vin[0].coinbase` from `getrawtransaction`, cached per txid), including
  UTXOs an older state file still tracks, and leaves coinbase out. It
  has to be shielded and sent back to the t-addr as an ordinary transfer
  first: see `docs/ops/keeper-miner.md`, "Coinbase must be shielded
  first".
- **Unconfirmed outputs** are not spent. `getaddressutxos` doesn't show
  the mempool, so a transfer becomes usable one block after it is sent.

At most one burn is in flight at a time. When a burn is broadcast,
`state.json` records it as `pending` right away, and its inputs stay
reserved until it is mined. If `mine` is killed, or the burn takes longer
than the 60 s wait (usual on testnet, where blocks come every 75 s), it is
checked again on each new block. The next burn is built only after it is
mined, and it then spends the burn's change. A burn that is still not
mined when the chain reaches its expiry height (40 blocks after it was
built) is dropped, and its inputs become spendable again. `report` shows
the burn in flight.

### Broadcast

An error from `sendrawtransaction` doesn't always mean the burn was
refused. zebrad can answer `channel closed` and still admit the
transaction. So after any error `mine` asks the node (`getrawtransaction`)
whether it has the burn before treating the error as a rejection. If the
node has it, the burn counts as sent. Otherwise `mine` sends the same
signed bytes again, up to three times. A resend answered with `already
exists in mempool` or `committed to the best chain` also counts as sent.
The txid can't change between sends, so a resend can never double-spend.

A burn counts as rejected only if every send got an error and the node
doesn't have it. Nothing is spent then, and the epoch is retried up to five
times, with funding re-read each time, before `mine` exits. If the node
can't be reached at all, `mine` can't tell whether it got the burn. It
keeps the burn in flight with its inputs reserved, and sends the same
bytes again on each new block until the burn is mined or expires.

Each epoch starts with one `getaddressutxos` call. Tracked UTXOs the node
no longer lists are dropped, and the address's other outputs are added
only when the tracked ones can't cover the burn. If nothing is spendable,
`mine` stops with `insufficient wallet funds` and gives the spendable
amount and, on regtest, the still-maturing coinbase. On testnet/mainnet,
if the address holds coinbase, it instead says `N zat of coinbase must be
shielded before it can fund a transparent burn` and points at the
shielding docs.

`report --verify-rpc <url>` first prints the address's funding at that
node: `spendable`, the coinbase on its own line (`coinbase maturing` on
regtest, `coinbase (must be shielded first)` on testnet/mainnet: pass the
same `--network` as `mine`), and what the burn in flight reserves. It
then checks the chain from this miner's first recorded epoch to the tip,
or from `--verify-from-height`. It never scans from genesis.

## Cookie auth

zebrad turns cookie auth on by default (`enable_cookie_auth = true`). Pass
its cookie file with the global `--rpc-cookie-file <path>` option, or set
`SOVA_MINER_RPC_COOKIE_FILE`. It applies to `mine --rpc` and to `report
--verify-rpc`:

```bash
sova-miner --network test --data-dir ~/.sova-miner \
  --rpc-cookie-file ~/.cache/zebra/.cookie \
  mine --rpc http://127.0.0.1:18232 --budget-zat 1000000 --per-epoch-zat 10000
```

The file holds `__cookie__:<token>`, which is sent as HTTP Basic auth.
zebrad writes a new cookie each time it starts. After an RPC call fails,
the miner reads the file again, and if the cookie changed it retries the
call once. A running miner therefore survives a zebrad restart. Without
the option nothing changes: no auth is sent, which suits a zebrad with
cookie auth off, such as the regtest box.

## Anchored burns (SIP-8)

**Dormant: SIP-8 is not active on any network yet.** Until a release
gives a network its activation height, every burn is the SIP-1 v1 burn
described above, whatever flags you pass.

SIP-8 (`sips/sip-8-draft-anchored-burns.md`) lets a burn also name a Sova
block, `(height, hash)`, in a version-2 payload. That reference is a
**vote**, weighted by the ZEC the burn destroys, and Sova nodes prefer
the history the most burned ZEC has voted for. `mine` casts one when you
point it at a Sova node:

```bash
sova-miner mine --rpc http://127.0.0.1:18232 --budget-zat 1000000 --per-epoch-zat 10000 \
  --sova-rpc http://127.0.0.1:8545      # YOUR OWN Sova node
```

- **`--sova-rpc <url>`**: before each burn, `mine` reads the node's head
  (`eth_getBlockByNumber("latest", false)`) and references it. Without
  `--sova-rpc`, burns are v1 exactly as before and burning never waits on
  Sova.
- **`--vote-wait <secs>`** (default 10): after a new Zcash block, wait up
  to this long for the Sova block that anchors it, then reference whatever
  the head is. Waiting lets most burns vote for the newest block; the cost
  is that about 12% of burns (at 10 s) miss the next Zcash block and land
  one later (SIP-8 §6). `0` references the head at once.
- **Freshness.** The head is voted for only if the Zcash block it anchors
  (its `parentBeaconBlockRoot`, which every Sova block commits to) is on
  this miner's own zebrad's best chain, at most 2 blocks below the tip.
  That block's height is the head's anchor epoch, `number + B − 1`, read
  from the chain, so there is no epoch base `B` to configure. A node that
  is behind, stuck, unreachable, or anchored on another Zcash branch gets
  no vote: the burn goes out as v1 with a `warning:` line saying why.
- **Activation guard.** A v2 burn mined below the network's activation
  height is **not a burn**: the ZEC is destroyed and nothing is minted.
  `mine` sends v2 only when it knows the activation height and the burn
  cannot be mined below it. With no activation height (today, everywhere)
  `--sova-rpc` is accepted, never queried, and `mine` says once at startup
  why its burns stay v1.
- **Fees.** The v2 payload output is 74 bytes (v1: 38), one more ZIP-317
  logical action: 25,000 zat instead of 20,000 for the usual 1-in/3-out
  burn, 20,000 instead of 15,000 without change. The budget counts it.
- `report --verify-rpc` recognizes v2 burns at heights where SIP-8 is
  active, as a Sova node does, and prints each one's reference.

**Trust: a vote is only as good as the node it came from.** The head
`mine` votes for is whatever that node's fork choice says. Point
`--sova-rpc` at a node you run, next to your zebrad. Pointing it at
someone else's node hands them your vote: they choose what your burned
ZEC stands behind. The MCP server (`mcp/`) passes the flag through and
defaults it only to the agent's own node (`SOVA_NODE_RPC_URL`), never a
public one.

**Testing on regtest.** A regtest Sova node recognizes v2 burns only when
told to, so the miner must be told the same height:
`--sip8-from <zcash-height>` (a global option, for `mine` and `report`).
It is refused on testnet and mainnet, where the activation height comes
with the release. It must match the Sova nodes' setting exactly, for the
reason in the activation guard above.

A reorg of Zcash across the activation height could still put a v2 burn
built just above it into a block below it. Mine well clear of the
boundary when testing it.

## Chain resets

`state.json` tracks UTXOs and burns on one particular Zcash chain. `mine`
checks, on startup and whenever the tip moves, that the node still serves
that chain: the hash of block 1 must match the one recorded the first time
`mine` ran (every regtest `zebrad` shares the same genesis, so genesis
can't tell two regtest chains apart), and the node must know every
transaction behind the tracked UTXOs and the latest burn. The second check
matters on regtest: block timestamps there are deterministic and a box
re-funds the same keys the same way, so a recreated chain starts as a
near-replay of the old one, with byte-identical txids.

If either check fails -- a regtest node recreated under a surviving data
dir, or `--rpc` pointed at a node on another network -- `mine` logs `zcash
chain RESET detected`, moves the tracked UTXOs and epoch history into
`retired_chains` in `state.json`, re-anchors to the node's chain,
rediscovers its funding there, and numbers epochs from 1 again. Without
this, every burn would try to spend inputs from the dead chain and fail
with `could not find transparent input UTXO`.

Kept across a reset: the keystore (same t-address and EVM address) and the
lifetime totals, which still include the retired chain's spend -- they back
`--lifetime-budget-zat`, a safety ceiling that should not forget spend.
Nothing is deleted: retired outpoints stay in `state.json`. `report` shows
the current chain's anchor and epochs, and `report --verify-rpc` compares
against the current chain only. If the node's tip is still at height 0 the
check waits for blocks instead of guessing. Pointing `mine` at a node that
is still syncing also resets (it can't see the tracked transactions yet);
mining against an unsynced node doesn't work anyway.

## Anonymous funding

The burn itself is always transparent — SIP-1 requires the eater output to
be visible on-chain, and `sova-miner` has no shielded code at all (it never
touches Orchard or Sapling). What *can* be unlinkable to you is where the
ZEC you burn came from.

### The flow

1. Hold ZEC in your own shielded wallet — Zodl or any other shielded-capable
   wallet. It doesn't need to be, and shouldn't be, anything connected to
   this miner.
2. Run `sova-miner init` and note the t-address it prints ("t-addr to
   fund").
3. From your shielded wallet, send a z→t deshielding transaction to that
   t-address, for at least what you intend to spend across the sessions
   you plan to run (`mine --budget-zat` covers burn value plus fee, summed
   across every epoch in that run).
4. Run `sova-miner mine` normally. Each SIP-1 burn spends UTXOs from that
   deshielded balance.

The identity link breaks at the shielded pool: a z→t transaction has no
visible input, so the t-address's funding source is unlinkable on the
transparent chain. That's the whole mechanism — it happens before you fund
the miner, not inside it.

### What this doesn't buy you

- **Burns from one miner address are linkable to each other.** The
  t-address is a persistent pseudonym for as long as you fund and mine
  from it, not a fresh identity per burn. Anyone can group all of its
  burns together and see its total volume and cadence — they just can't
  tie the address to you. This is unlinkability of funding, not per-burn
  anonymity.
- **The deshielding transaction is itself visible.** Its output address,
  amount, and block height are all public. A round or otherwise
  distinctive amount, or a deshield immediately followed by a burn, gives
  an observer an easy correlation. The unlinkability here is only as good
  as the habits around it.
- **Reward payouts are public.** The EVM address set at `init` (default:
  the keystore key's own Ethereum address; override with `--evm-address`)
  is the address SIP-1 credits in the burn payload and the address epoch
  rewards are paid to. It's a plain EVM account — its balance and
  activity are exactly as visible as any other address on the chain. It
  is linked to your t-address either way: every burn names it next to the
  t-address's inputs.
- **This is funding unlinkability, not private execution.** Sova's EVM is
  fully transparent; every contract call and state change is public. Don't
  describe this as "private mining" or "anonymous contracts" — only the
  funding source is unlinkable.

### Practical guidance

- Use a fresh miner address per mining session: `sova-miner init
  --data-dir <a-new-dir>` generates a new keystore instead of reusing one.
  This bounds the persistent-pseudonym problem above to one session's
  burns instead of your entire mining history.
- Avoid round or distinctive deshield amounts, and vary them between
  sessions — don't fund every session with an identical, memorable number.
- Never reuse a miner's t-address for anything else. Every other use
  (receiving unrelated payments, consolidating other UTXOs) is another
  chance to link it back to you.
