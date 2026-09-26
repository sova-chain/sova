# checkout-relayer

Reserves and claims orders on `AshwingsZecCheckout` for buyers who hold no
SOVA, and can watch the seller's Zcash t-address to claim paid orders
automatically. Design and flow: `docs/design/ashwing-zec-checkout.md`.
The project runs it publicly at `https://checkout.testnet.sova.io`, which
is the default relayer of `/ashwings/buy`. The testnet kit deploys it on
the faucet host (`CHECKOUT_RELAYER=1`; `docs/ops/testnet-launch.md`,
"Checkout relayer").

```sh
npm ci
node src/server.mjs        # config from the environment, see below
npm test                   # limits: unit + anvil (needs anvil and `forge build` in contracts/)
npm run e2e                # anvil + mock precompile + headless browser
```

## API

| | |
|---|---|
| `POST /reserve {listingId, recipient}` | Reserve for `recipient`; the relayer pays the gas. `200 {reservationId, quoteZat, txHash}` once mined, or `202 {txHash, pending: true}` if that takes over `RESPOND_WAIT_MS` (the caller reads the `Reserved` event from the receipt). If this relayer already has an open order for `recipient` with time left, it returns that one (`existing: true`) |
| `POST /claim {reservationId, txid, vout}` | Claim a paid order. Dry-run first, so a claim that would revert costs nothing (`400` with the contract error). `200 {itemId, txHash}` or `202 {txHash, pending: true}`; a second claim for the same order while one is in flight gets the same tx |
| `GET /status` | Relayer address, chain, checkout, listing ids, SOVA balance, the floor, open orders / the cap, whether it takes new orders (and why not), the limits. No secrets |
| `GET /status/:id` | What the watcher found for an order (`detected` txid/vout, `claim` state) |
| `GET /health` | Liveness. Local only: the public tunnel doesn't route it |

Errors are JSON `{error}`: `400` bad input or a contract revert, `403`
a listing not served here or an origin that isn't allowed, `413` body too
large, `415` not `application/json`, `429` a rate limit (with
`Retry-After`), `503` busy (open-order cap) or low on SOVA, `502` Sova
unreachable.

## Configuration

Environment variables only. Put them in a local `.env` (gitignored) and run
`node --env-file=.env src/server.mjs`, or export them in your shell.

| Variable | Default | Meaning |
|---|---|---|
| `RELAYER_KEY` | (required) | Funded private key that pays reserve/claim gas. Keep it out of the repo. `src/keygen.mjs <file>` makes one in a 0600 file and prints only the address |
| `ASHWINGS` | | Ashwings address: the checkout is read as `Ashwings.zecCheckout()` |
| `CHECKOUT` | | `AshwingsZecCheckout` address. With `ASHWINGS` too, they must agree or it refuses to start. One of the two is required |
| `SOVA_RPC_URL` | `http://127.0.0.1:8545` | Sova JSON-RPC |
| `RPC_PER_10S` | 30 | At most this many Sova RPC requests per 10 s; more queue. Keep it under the public RPC's per-IP limit (50 / 10 s) |
| `RPC_POLL_MS` | 4000 | Receipt polling interval |
| `LISTINGS` | any | Only serve these listing ids |
| `HOST`, `PORT` | `127.0.0.1:8787` | Bind address |
| `CORS_ORIGIN` | `*` | The page's origin(s), comma-separated (`https://sova.io` in production). Requests from other origins get no CORS grant and their POSTs are refused (`403`) |
| `TRUST_PROXY_HEADER` | (socket peer) | Client IP for rate limits: `cf-connecting-ip` behind cloudflared, `x-forwarded-for` (rightmost entry) behind another proxy you run. Believed only when the socket peer is loopback. `TRUST_PROXY=1` is the old spelling of `x-forwarded-for` |
| `MAX_BODY_BYTES` | 1024 | Request body cap |
| `RESERVE_PER_IP_PER_HOUR` | 3 | Per client IP (IPv6: per /64) |
| `RESERVE_PER_MINUTE`, `RESERVE_PER_HOUR` | 10, 60 | All clients together |
| `CLAIM_PER_IP_PER_HOUR`, `CLAIM_PER_MINUTE` | 20, 30 | Per IP, and all clients |
| `MAX_OPEN_RESERVATIONS` | 20 | Its own orders that are pending or unpaid inside their payment window. At the cap, `/reserve` answers `503` |
| `REUSE_MIN_BLOCKS_LEFT` | 20 | A repeat `/reserve` for the same recipient gets its open order back if at least this many Zcash blocks of the window are left |
| `MIN_BALANCE_WEI` | 1e17 (0.1 SOVA) | Below this it takes no new orders (`503`); claims go on |
| `STATE_FILE` | (none) | Where the open orders are kept across restarts (0600) |
| `RESPOND_WAIT_MS` | 25000 | How long a POST waits for its receipt before answering `202` |
| `PRUNE_MS` | 30000 | How often open orders are re-checked (Zcash anchor, filled) |
| `LOG_CHUNK` | 1000 | `eth_getLogs` block range per call (the public RPC caps it at 1,000) |

Optional payment watcher (finds payments on the seller's t-address and
claims them):

| Variable | Meaning |
|---|---|
| `ZCASH_RPC_URL` | zebrad with its address index (`getaddresstxids`, `getrawtransaction`). Only those two and `getblockcount` are ever called |
| `ZCASH_RPC_COOKIE` | zebrad's cookie file, or `ZCASH_RPC_USER` / `ZCASH_RPC_PASSWORD` |
| `ZCASH_NET` | `test` (tm…) or `main` (t1…), default `test` |
| `POLL_MS` | Default 5000 |
| `START_BLOCK` | Sova block the checkout was deployed at (log scan start), default 0 |
| `DROP_AFTER_BLOCKS` | Stop watching an unpaid order this many Zcash blocks past its deadline, default 20 |

## Running it publicly

It can't steal: `reserve` and `claim` always deliver to the order's
recipient, so what a relayer risks is its gas money, the owl supply its
orders hold, and its RPC budget. The defaults above are the public ones:

- **Gas.** Rate limits per IP and overall, dry-run before every send, and
  the balance floor. The floor stops new orders while there is still gas
  for claims of orders someone already paid for.
- **Supply.** Each reservation holds one owl until it is claimed or its
  hold lapses (`AshwingsZecCheckout.holdBlocks`: window + minConf + grace).
  The relayer keeps at most `MAX_OPEN_RESERVATIONS` of its own orders open
  at once, one per recipient, however many IPs ask. The count lives in
  `STATE_FILE`, so a restart doesn't reset it.
- **RPC.** One throttle for all of its Sova calls, `eth_getLogs` in
  1,000-block chunks, and no overlapping re-checks, so it never gets its
  own IP blocked at the edge.
- **Browsers.** Strict CORS; POSTs must be `application/json` (no simple
  cross-site form posts); small bodies; slow clients time out.

The key: `node src/keygen.mjs /path/key.env` (0600, created once, prints
the address). To move its SOVA out before a rotation, run
`node src/sweep.mjs <0xTo>` with the same `RELAYER_KEY` and `SOVA_RPC_URL`.
