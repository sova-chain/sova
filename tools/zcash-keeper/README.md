# zcash-keeper

The reference keeper for SIP-7 §4.3. It watches the node's Zcash block feed
(§4.2 path A), checks your trigger conditions against each anchored Zcash
block, and sends `poke(h)` to a `ZcashTrigger` contract
(`contracts/src/zcash/ZcashTrigger.sol`) when a condition holds at height `h`.

Zcash can't call a Sova contract, so a keeper has to send the transaction.
The contract checks everything again on chain and pays the keeper a bounty.
The keeper only finds the witness height `h`.

```sh
npm ci
node src/main.ts --config config.json --dry-run      # print what it would send
KEEPER_KEY=0x… node src/main.ts --config config.json # send for real
npm test                                              # mocked RPC, no node needed
```

Needs Node 23.6 or newer. It runs the TypeScript directly (Node strips the
types), so there is no build step. Its only dependency is `viem`, for signing.

## The node side

The node must run with `SOVA_SIP7=1`. That turns on the feed:

- `sova_getZcashBlocks(fromHeight, toHeight)` over HTTP. It returns the
  summaries of the Zcash heights in the range that the canonical Sova chain
  anchors: height `h` with `h ≤ head + B − 1`, where the Sova block
  `h − B + 1` commits to that block's hash. A call spans at most 1,000
  heights. It is on the `public` RPC allowlist.
- `sova_subscribe("zcashBlocks")` over WS. It sends one item per height as
  soon as the head anchors it, in order. After a reorg it first sends
  `{"rollback": {"toHeight": N}}` (everything above `N` is void), then the
  new branch. WS is off by default. Set `SOVA_WS_PORT` (or
  `SOVA_BOX_WS_PORT` for the box). It only works with the `local` RPC
  profile, because `public` turns WS off.

One item looks like this. Amounts are in zatoshis, as decimal strings. A
delta is the value that moved **into** the pool, so it is negative on an
outflow:

```json
{"height": 57, "hash": "0x…", "time": 1790190000, "sovaBlock": 57,
 "pools": {"transparent": "…", "sprout": "0", "sapling": "0", "orchard": "0", "lockbox": "…", "ironwood": "0"},
 "chainSupply": "…",
 "deltas": {"transparent": "…", "sprout": "0", "sapling": "0", "orchard": "0", "lockbox": "…", "ironwood": "0"},
 "stats": {"txCount": 2, "shieldedTxCount": 0, "tIn": 1, "tOut": 3, "saplingSpends": 0, "saplingOutputs": 0,
           "orchardActions": 0, "ironwoodActions": 0, "joinSplits": 0},
 "trees": {"sapling": 0, "orchard": 0, "ironwood": 0}}
```

## Config

```json
{
  "rpc": "http://127.0.0.1:8545",
  "ws": "ws://127.0.0.1:8546",
  "triggers": [
    { "name": "shielded-crossed-2M-ZEC", "contract": "0x…",
      "when": { "kind": "poolCrossedAbove", "pool": "shielded", "zat": "200000000000000" } }
  ]
}
```

`config.example.json` has more examples. If you leave out `ws`, the keeper
polls `sova_getZcashBlocks` over HTTP. If you set it, the keeper subscribes.

| `when.kind` | Holds at block `h` when | Fields |
|---|---|---|
| `poolCrossedAbove` | total(h) ≥ zat and total(h−1) < zat | `pool`, `zat` |
| `poolCrossedBelow` | total(h) < zat and total(h−1) ≥ zat | `pool`, `zat` |
| `netOutflowAbove` | −delta(h) > zat | `pool`, `zat` |
| `netInflowAbove` | delta(h) > zat | `pool`, `zat` |
| `statAtLeast` | stats[stat] ≥ value | `stat` (a `stats` key), `value` |

The `pool` field takes any pool id (`transparent`, `sprout`, `sapling`,
`orchard`, `lockbox`, `ironwood`), or `shielded`, which means Sprout +
Sapling + Orchard + Ironwood (the same sum as `ZcashLib.shieldedTotal`). The
keeper computes total(h−1) as total(h) − delta(h), so it needs only one item.

Each trigger fires once. After a successful poke the keeper stops watching
it. Set `"repeat": true` for a contract that overrides `_armed` to fire
again.

## What it does per trigger

1. The condition holds at `h`. The keeper logs `condition` and holds `h` as
   a witness.
2. The keeper waits until `h + minConf − 1` is anchored. It reads `minConf`
   from the contract, because `poke` reverts with `TooShallow` before that.
   If a rollback comes in first and drops `h`, the witness is discarded. The
   new branch can arm the trigger again.
3. The keeper calls `ready(h)` with `eth_call`. In live mode it sends
   `poke(h)` only if that returns true. In dry-run mode it logs `would-poke`
   with the target, calldata and `ready` result, and sends nothing.

Everything is logged as one JSON line per event: `start`, `block`,
`condition`, `rollback`, `would-poke`, `poked`, `receipt`, `skip`, `retry`,
and `warn`. If an RPC call for a witness fails, the witness stays pending
and is retried on the next block (`retry`). This happens, for example,
right after a Zcash reorg, while `eth_call` at `latest` fails until Sova
re-seals. The poller also retries failed feed calls after the first one.

## Options

| Flag | Meaning |
|---|---|
| `--config FILE` | Required. |
| `--dry-run` | Print, don't send. No key needed. |
| `--rpc URL` / `--ws URL` | Override the config. |
| `--from H` | First Zcash height to evaluate. Defaults to the next height after the currently anchored one (`anchor()` on `0x…5A00`). |
| `--lookback N` | Start `N` heights back from the anchored height instead. The node allows at most 1,000 for a subscription. |
| `--max-blocks N` | Stop after `N` blocks (for demos). |
| `--dev-key` | Sign with the public reth/anvil dev key. Works only on chain ID 1337 (the box). |

For anything other than the local dev chain, pass `KEEPER_KEY` in the
environment and keep it out of the repo.

## Against the box

```sh
SOVA_BOX_SIP7=1 SOVA_BOX_WS_PORT=8546 ./box/up.sh
node src/main.ts --config config.json --dry-run --lookback 20 --ws ws://127.0.0.1:8546
```
