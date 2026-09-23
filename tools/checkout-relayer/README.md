# checkout-relayer

Reserves and claims orders on `AshwingsZecCheckout` for buyers who hold no
SOVA, and can watch the seller's Zcash t-address to claim paid orders
automatically. Design and flow: `docs/design/ashwing-zec-checkout.md`.

```sh
npm ci
node src/server.mjs        # config from the environment, see below
npm run e2e                # anvil + mock precompile + headless browser
```

## Configuration

Environment variables only. Put them in a local `.env` (gitignored) and run
`node --env-file=.env src/server.mjs`, or export them in your shell.

| Variable | Required | Meaning |
|---|---|---|
| `CHECKOUT` | yes | `AshwingsZecCheckout` address (`Ashwings.zecCheckout()`; listing 1) |
| `RELAYER_KEY` | yes | Funded private key that pays reserve/claim gas. Keep it out of the repo. |
| `SOVA_RPC_URL` | | Sova JSON-RPC (default `http://127.0.0.1:8545`) |
| `LISTINGS` | | Only serve these listing ids (default: any) |
| `HOST`, `PORT` | | Bind address (default `127.0.0.1:8787`) |
| `CORS_ORIGIN` | | The page's origin in production (default `*`) |
| `TRUST_PROXY` | | `1` = rate-limit by `X-Forwarded-For` (only behind your own proxy) |
| `RESERVE_PER_IP_PER_HOUR` | | Default 10 |
| `CLAIM_PER_IP_PER_HOUR` | | Default 30 |
| `RESERVE_PER_MINUTE` | | Global cap, default 30 |

Optional payment watcher (finds payments on the seller's t-address and
claims them):

| Variable | Meaning |
|---|---|
| `ZCASH_RPC_URL` | zebrad with its address index (`getaddresstxids`, `getrawtransaction`) |
| `ZCASH_RPC_COOKIE` | zebrad's cookie file, or `ZCASH_RPC_USER` / `ZCASH_RPC_PASSWORD` |
| `ZCASH_NET` | `test` (tm…) or `main` (t1…), default `test` |
| `POLL_MS` | Default 5000 |
| `START_BLOCK` | Sova block the checkout was deployed at (log scan start), default 0 |
| `DROP_AFTER_BLOCKS` | Stop watching an unpaid order this many Zcash blocks past its deadline, default 20 |
