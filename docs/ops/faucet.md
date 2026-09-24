# TAZ faucet (`sova-faucet`)

Strangers on the M1 public testnet need testnet ZEC (TAZ) to burn, and
every public Zcash testnet faucet is dead. Decision infra-2 **D5**: the
project runs its own TAZ faucet, capped and rate-limited, with an isolated
testnet-only hot key. Source: `crates/burn-wallet/faucet`.

## The one hot key

Project infrastructure holds no signing keys (`docs/design/infra-m1.md`
§4, rule 1). **The faucet key is the single exception.** It is bounded
like this:

- **Testnet only.** The faucet refuses `network = "main"` in its config.
  At startup it also asks zebrad and refuses if either
  `getblockchaininfo.chain` is `"main"` or block 0 is mainnet's genesis
  hash. It checks the genesis hash against the configured network too, so
  a `test` config pointed at a regtest node is refused as well. It also
  refuses mainnet recipient addresses.
- **Blast radius: testnet TAZ, nothing else.** TAZ has no market value. A
  stolen key loses at most the wallet balance, and the balance guard below
  keeps that to a few days of drips. The key can't touch SOVA, any
  mainnet funds, or any consensus role. The faucet is not a miner and not
  a sealer.
- **Own key, own host.** `sova-faucet init` creates a fresh keystore. The
  key is never a miner key, and no miner uses it. The faucet runs on its
  own small host (or at least its own Unix user), never on the public
  seed or RPC boxes.
- **File hygiene.** The keystore is plaintext hex protected by mode
  `0600`, the same format as `sova-miner`'s. `run` refuses to start if
  group or other can read it. The key is never logged or printed; `init`
  prints only the address. Never commit it, and never copy it into
  config management. Recreate it on a rebuild instead: rotating means
  running `init` again and funding the new address.
- **Holds little.** Top the wallet up in small amounts. If it holds more
  than `max_balance_multiple × daily_cap_zat` (default 5 × 2 TAZ), it logs
  `WARNING: HOT WALLET OVER LIMIT` at startup and every 10 minutes while
  serving, and `/status` shows `"over_max_balance": true`. There is no
  sweep command, so the fix for over-funding is to stop topping up. The
  faucet has no admin endpoints of any kind.

## API

`POST /drip` with the body `{"address": "<t-addr>"}` sends the fixed
drip and returns `200 {"txid", "address", "amount_zat", "fee_zat"}`.
Errors come back as `{"error": <code>, "message": ..., "retry_after_secs"?}`:

| HTTP | `error` | When |
| --- | --- | --- |
| 400 | `invalid_address` | Not a Zcash address; wrong network; shielded or unified (only t-addrs: P2PKH, P2SH, and ZIP-320 TEX); or the faucet's own address |
| 400 | `bad_request` | The body isn't `{"address": ...}` |
| 429 | `address_cooldown`, `ip_cooldown`, `daily_cap_reached` | A limit was hit. `Retry-After` is set |
| 503 | `busy` | Every spendable coin is in an unmined drip. Retry after the next block |
| 503 | `maturing` | Only coinbase younger than 100 blocks is left (regtest) |
| 503 | `coinbase_unshielded` | Only coinbase is left, which testnet consensus won't let a drip spend until the operator shields it (see "Funding" below) |
| 503 | `faucet_empty`, `paused` | The wallet is empty, or the state file couldn't be written (see below) |
| 502 | `node_error` | zebrad rejected the transaction or is unreachable |

`GET /status` returns the network, faucet address, tip, spendable,
immature, coinbase (must be shielded first, `coinbase_unshielded_zat`)
and in-flight balances, the drip size, the daily cap, today's
spend and what's left, seconds until the 00:00 UTC reset, both cooldowns,
the balance guard, and `accepting_drips`. The answer is cached for
`status_cache_secs`. Every other path returns 404.

## Config and defaults

Start from `crates/burn-wallet/faucet/faucet.example.toml`. Every value in
it is the built-in default, and a test keeps the two in sync. Unknown keys
are an error.

| Key | Default | Meaning |
| --- | --- | --- |
| `network` | *(required)* | `test` or `regtest`. `main` is refused |
| `listen` | `127.0.0.1:18790` | A non-loopback address needs `allow_public_listen = true`. Don't set it |
| `zebrad_rpc`, `zebrad_cookie_file` | *(required)*, none | Your own zebrad on loopback. Use its cookie auth (on by default in zebrad 6.x) |
| `keystore`, `state_file` | *(required)* | The faucet's own key, and its only state |
| `drip_zat` | 10,000,000 (0.1 TAZ) | Fixed per drip. Must be between 100,000 and 10 TAZ, and a typo outside that range is refused |
| `address_cooldown_secs` | 86,400 | One drip per address per 24 h. A TEX address and the t-addr of the same key share one cooldown |
| `ip_cooldown_secs` | 86,400 | One drip per client IP per 24 h. IPv6 clients are grouped by /64 |
| `daily_cap_zat` | 200,000,000 (2 TAZ) | Drips plus fees per UTC day |
| `max_balance_multiple` | 5 | Balance-guard threshold, as a multiple of the daily cap |
| `trusted_proxy_header` | unset | For example `CF-Connecting-IP`. When unset, the socket peer is the client IP |
| `trusted_proxy_peers` | `127.0.0.1`, `::1` | The header is believed only from these socket peers |

How each drip is handled:

1. Validate the address.
2. Check both cooldowns and the cap. These checks cost nothing on the
   node.
3. Select coins. Inputs of unmined drips are excluded, and so is
   coinbase: all of it on testnet, immature coinbase on regtest. Change
   under 1,000 zat is folded into the fee.
4. Compute the ZIP-317 fee (`burn_wallet::fee`, shared with
   `sova-miner`) and re-check the cap with that fee included.
5. Sign and broadcast.
6. Record the drip.

Nothing is recorded for a request that is refused, and nothing is
recorded when zebrad rejects the transaction. An RPC error on
`sendrawtransaction` counts as a rejection only if zebrad then doesn't
know the transaction: zebrad can answer `channel closed` and still admit
it. If the broadcast outcome is unknown (a transport error), the drip is
recorded as sent. That is the
safe direction: the drip counts against both cooldowns and the cap, and
its inputs stay reserved until the transaction's expiry height. Requests
are served one at a time, so check-then-record can't race.

## State

`state_file` is a small JSON file, mode `0600` and written atomically. It
holds:

- per-address and per-IP last-drip times, pruned once a cooldown lapses;
- today's spend;
- in-flight drips (txid and reserved inputs), cleared when a drip is mined
  or passes its expiry height.

The file is tied to the faucet address, and the faucet refuses to start
if it names a different key. **If you lose it, every cooldown and today's
spend reset**, so keep it on persistent disk. If it can't be written after
a broadcast, the faucet refuses all further drips (`paused`) until it is
restarted.

## Run it (localhost)

```bash
cd crates/burn-wallet && cargo build --release -p sova-faucet
sova-faucet init --keystore /var/lib/sova-faucet/keystore.json --network test
# fund the printed t-addr with a few days of drips (a plain transfer, not
# coinbase: see "Funding" below), then:
sova-faucet run --config /etc/sova-faucet.toml
curl -s localhost:18790/status
curl -s -X POST -d '{"address":"tm..."}' localhost:18790/drip
```

### Funding

On testnet, fund the faucet with a **plain transfer** to its t-addr.
Coinbase can't fund a drip there: Zcash consensus only lets transparent
coinbase be spent into a transaction whose outputs are all shielded, and
a drip pays a t-addr. So the faucet never selects coinbase on testnet,
however old it is. It reports it as `coinbase (must be shielded first)`
at startup and as `coinbase_unshielded_zat` in `/status`. When nothing
else is left, drips fail with `coinbase_unshielded`, and the log says
`WARNING: FAUCET CAN'T DRIP: N zat of coinbase must be shielded before it
can fund a transparent drip ...`, at most every 10 minutes. The fix is
the shield-and-return round trip in `docs/ops/keeper-miner.md`,
"Coinbase must be shielded first".

On regtest (`./box/up.sh`, see `box/up/README.md`), Zebra allows
unshielded coinbase spends by default. Fund the faucet with
`generatetoaddress 1 <faucet t-addr>`, then mature that coinbase with
`generate 100`, the same way the box funds its miner.

## Behind Cloudflare (what the testnet kit sets up)

`infra/testnet` does all of this on `sova-faucet-1` (`setup-host.sh`
writes the config, `cloudflare.sh tunnels` and `ratelimit` do the edge).

1. Keep `listen` on loopback. Run `cloudflared` on the same host, with an
   ingress rule that sends `faucet.<domain>` to `http://127.0.0.1:18790`.
   The host needs no inbound ports.
2. Set `trusted_proxy_header = "CF-Connecting-IP"`. Keep
   `trusted_proxy_peers` at loopback, because cloudflared connects from
   there. Only then do per-IP cooldowns see real client IPs. Without the
   header, every request shares cloudflared's IP and therefore one
   cooldown, which is safe but strict.
3. At the edge, a WAF rate-limit rule on `/drip` (`cloudflare.sh
   ratelimit`: the zone's one free-plan rule, 50 per 10 s per IP, blocked
   for 10 s; the free plan has no per-minute period) and pass through only
   `/drip` and `/status` (the tunnel's ingress rule). Put
   Cloudflare Turnstile on the web form. The faucet doesn't verify
   Turnstile tokens yet: doing that server-side means calling Cloudflare's
   `siteverify` from the faucet, and that is follow-up work.

## Known limits

- **One drip per spendable coin per block.** A drip's change is spent only
  after it is mined. A burst is served from as many coins as the wallet
  holds, and the rest get `busy`. Several top-ups mean several coins.
- **A drip is spendable by `sova-miner` one block after it is sent.**
  The miner finds its funding with `getaddressutxos`, which lists
  confirmed outputs only, so a drip (or a z→t deshield) shows up once it
  is mined.
