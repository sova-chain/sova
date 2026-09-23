# Buy an Ashwing with ZEC (SIP-4 demo 1)

Pay ZEC from any normal Zcash wallet, get an owl on Sova. There is no
bridge and no wrapped token. The ZEC goes straight to the seller on Zcash
and never leaves it. Sova reads the payment through the SIP-4 precompile.
Code: `contracts/src/zcash/ZecCheckout.sol` (the generic checkout) and
`contracts/src/AshwingsZecCheckout.sol` (the Ashwings ZEC mint, created by
the `Ashwings` constructor). Tests: `contracts/test/ZecCheckout.t.sol` and
`contracts/test/AshwingsV2.t.sol` (mock precompile). Script:
`contracts/script/AshwingZecCheckoutDemo.s.sol`. **Needs SIP-4 live.**

**Ashwings v2 (2026-09-23).** Ashwings is capped at 10,000 and has one
fixed price, payable in SOVA (`Ashwings.mint`, `priceWei` to `treasury`)
or in ZEC through this checkout. The terms are constructor arguments
with no setters, `Ashwings(treasury, zecPayee, priceWei, priceZat)`, each
readable through a same-named getter (what `contracts/script/deploy-kit.sh`
matches and verifies). `zecPayee` is the t-address string, decoded
on-chain; its network sets minConf (t1/t3: 10, tm/t2: 3). The window is
40 blocks. The treasury only receives. The constructor creates `AshwingsZecCheckout`, a
`ZecCheckout` with one listing (#1) fixed at deploy (`list`/`update`
always revert), so the buyer ABI below, the page and the relayer are
unchanged: point them at `Ashwings.zecCheckout()` with listing 1. Two
additions, both in the subclass:

- **Supply holds.** A reservation holds one unit of the 10,000 until it
  is claimed, or until `reservedAt + window + minConf + 96` Zcash blocks
  (~2 h of grace after confirmations). A buyer who pays in the window and
  claims in the grace cannot be sold out by SOVA mints. Expired holds are
  released lazily (`sweep()`, called by `reserve` and by a sold-out SOVA
  mint; anyone may call it). A claim after its hold was released still
  mints while supply is left.
- **Tags wrap** instead of running out, since the terms can never move to
  fresh ones. Safe because at most 10,000 reservations can be live or
  minted within any one hold period (< 99,999 tags), so a tag only comes
  round again after its earlier order's window closed.

## The user story (Zashi / Zingo style wallet)

1. **Open the page, paste a Sova address** (any EVM wallet). You don't
   need SOVA: the page's relayer calls `reserve(listing, you)`.
2. **The page shows a QR code**: `zcash:t1…?amount=0.25000042`. That's
   the seller's address and your exact amount: price plus a tag unique to
   your order.
3. **Scan and send** from your wallet. A shielded balance works (z→t);
   only the one output to the seller is public.
4. **Wait ~4 min** (testnet `minConf` 3; mainnet 10, ~12.5 min). The
   payment has to be *mined* inside the window (40 blocks, ~50 min).
   Confirmations can finish after it.
5. **The owl appears.** Anyone calls `claim(order, txid, vout)`. The
   contract checks the payment and mints the Ashwing to you.

## Design in one paragraph

Prices are multiples of 0.001 ZEC. Each reservation gets a tag
(1–99,999 zat) from a counter per (seller address, price), and pays
exactly `price + tag`. A (seller address, amount) pair therefore belongs
to one reservation, ever. Many buyers can share one seller address. A
late payment can't be claimed by the next buyer, which fixes the
ZecEscrow reuse hazard. A reservation locks no address, so it is
**free: no bond** (in Ashwings v2 it does hold one unit of supply for a
bounded time; see above). The `(txid, vout)` set stays in as a second
one-payment-one-fill guard. Delivery calls `Ashwings.mintForZec`, which
only the checkout may call; the art is untouched (parity 155/155). Claim
gas is about 128k with the mock precompile (`AshwingsV2.t.sol`,
`testGasMintPaths`).

We considered one fresh address per order (the ZecEscrow approach) and
chose tags. Fresh addresses allow any amount ≥ price and give unlinkable
sales. But each address has to be pre-registered, a reservation locks
one (so it needs a bond against griefing), concurrency is limited by the
pool size, and a reused address carries the late-payment hazard.

## What's trustless, and what isn't

- **Trustless:** the payment check is consensus: every node reads the
  same anchored Zcash chain. Nobody holds ZEC. There's no admin. The
  seller can't block an order that is already reserved: edits and pausing
  only affect new orders. Anyone can claim, so the relayer can't keep
  your owl.
- **Not trustless:** the page has to show the address and amount the
  contract holds (check with `quote` and `payeeScript`). Paying the wrong
  amount, paying late or overpaying can't be claimed. The ZEC still
  reaches the seller, and any refund is up to them, off-chain. A Zcash
  reorg deeper than `minConf` rolls Sova back too, but not anything done
  off Sova. The seller's address and sale amounts are public, so the
  seller should sweep them to shielded.
- **Supply is shared.** SOVA mints and ZEC orders draw on the same
  10,000. A reservation holds its unit only for its hold period; an
  order paid in time but claimed after the hold, once the collection is
  sold out, cannot mint (the ZEC reached the payee; a refund is
  off-chain). Free reservations can hold the last units for a hold period
  at a time (gas only). Open question: accept that, or add a small bond
  or a cap on open holds.

## To run it live

1. **SIP-4 `txInfo` + `txOutput` in the node.** *Not built.* Only
   `anchor()` exists, on `research/evm-seam` (d2421a8). Also needed: the
   follower's persisted tx index (§5), the precompile cache kept off
   (`new_stateful`), and a real Zcash-reorg rollback (§7). Without the
   rollback, the reorg guarantee above doesn't hold.
2. A Sova testnet with the anchor rule and a Zcash-testnet follower.
3. Deploy Ashwings with the real terms (`box/deploy-dapps.sh` /
   `contracts/script/deploy-kit.sh`, `ASHWINGS_*` env); the checkout comes
   with it. There is no listing step.
4. The page and relayer: *built* (below). Point them at the testnet RPC,
   the deployed checkout and a zebrad; see "Page and relayer".
5. One wallet dry run each for Zashi and Zingo: exact 8-decimal amount,
   z→t send. **Not yet tested wallet by wallet.**

## Page and relayer

- **Page:** `site/src/pages/ashwings/buy.astro` → `/ashwings/buy`.
  Unlisted: `noindex`, not in the nav or sitemap. The client JS
  (`site/src/scripts/checkout/`, ~14 KB built) is hand-rolled JSON-RPC,
  a precomputed-selector ABI and a QR encoder. No library, no
  third-party request.
- **Relayer:** `tools/checkout-relayer/` (Node, one dependency: `viem`).
  It runs `POST /reserve` and `POST /claim` with its own gas, dry-runs
  every send and rate-limits per IP. It can't redirect an owl, because
  the recipient is fixed at reserve. An optional watcher finds payments
  and claims them itself. Config is env only (`tools/checkout-relayer/README.md`).

Page config is a block in `buy.astro`, overridden by query params:
`?rpc=&co=&listing=&relayer=<url|none>&net=test|main`. The order and
txid are kept in the URL (`&r=&tx=`), so a reload resumes the order.
With an injected wallet the page sends `reserve`/`claim` itself.
Without one it asks the relayer.

### How the page learns the payment landed

The contract can only check a txid someone hands it. The address has
no "what paid me" query, and SIP-4 v1 is keyed by txid by design.

- **(a) Paste the txid. This is v1, and it's built.** The page reads
  `txInfo`/`txOutput` straight from the precompile over `eth_call`,
  finds the output that pays exactly the quote to the payee, and shows
  `N/minConf`. It lights **claim** only after an `eth_call` dry run of
  `claim()` succeeds, and it names the reason when a payment can't be
  claimed (wrong amount, late, too early). This needs nothing beyond
  SIP-4.
- **(b) The relayer watches the seller's address. Built, optional.** It
  collects open orders from `Reserved` logs, then asks zebrad for
  `getaddresstxids` on the seller's t-address (from `reservedAt + 1`)
  and `getrawtransaction <txid> 1`. It matches `valueZat == quote` and
  `scriptPubKey.hex == payee` inside the window, including a payment at
  any `vout`. zebrad only *finds* candidates. Sova is the judge: the
  relayer dry-runs `claim()` each poll and sends it once that passes.
  While the anchor catches up, `TxNotFound` and `InsufficientConfirmations`
  just mean "wait". A wrong or lagging zebrad can waste a dry run, but
  never gas and never an owl. The page polls `GET /status/:id`, so the
  txid fills itself in.
- **Not an option today: asking the Sova node.** The follower's tx
  index (§5) is keyed by txid too, so the node can't answer "payments to
  script S". A later `sova_zcashPaymentsTo(script, fromHeight)` RPC over
  that index would drop zebrad from the relayer. It would be an RPC, not
  consensus. A browser-only variant could ask lightwalletd
  (`GetTaddressTxids` over gRPC-web), but that means one more service to
  trust for liveness.

### Run it locally (no SIP-4 node)

```bash
(cd contracts && forge build)
(cd site && npm ci && npm run build)
cd tools/checkout-relayer && npm ci
npm run e2e        # SHOTS=/some/dir to keep the screenshots
```

`e2e/run.mjs` starts anvil on a free port and deploys Ashwings (10 SOVA /
0.25 ZEC, window 40, minConf 3; the checkout comes with it) and the
market. It etches `MockZcash` at `0x…5a00` in place of the precompile, and
starts a fake zebrad and the relayer. It then drives headless Chromium
through five flows:

- **A.** Relayer reserve → QR (decoded with jsQR and checked against the
  contract) → paste txid → 1/3 → claimable → claim → owl.
- **B.** Injected wallet reserve → the watcher finds the payment at
  vout 1 and claims it on its own.
- **C.** Phone width → wrong amount rejected.
- **D.** `/ashwings/mint`: injected wallet mints for SOVA; supply, prices
  and the ZEC hand-off link come from the contract.
- **E.** `/ashwings/market`: list (approve + list), a second wallet buys
  (seller +99%, 1% booked for the treasury), list and cancel another.

Everything it starts, it kills. To click through it by hand instead:
run `anvil` on 8545 and `box/deploy-dapps.sh` (the pages default to the
addresses it deploys: checkout `0x856e…eae5`), then etch and `init` the
mock. Then start the relayer and `npm run dev` in `site/`:

```bash
CHECKOUT=0x856e4424f806D16E8CBC702B3c0F2ede5468eae5 RELAYER_KEY=<anvil key #2> \
  node tools/checkout-relayer/src/server.mjs
# http://localhost:4321/ashwings/buy
```

To "pay", call `MockZcash(0x…5a00).pay(txid, script, quote)` and then
`mine(n)` to move the anchor.

### What the live testnet needs

1. **SIP-4 `txInfo` + `txOutput` in the node**, with the ABI in
   `IZcash.sol`. The page and relayer call the same selectors the mock
   answers. `anchor()` alone isn't enough. This is the blocker from "To
   run it live" (§5 tx index, §7 reorg rollback).
2. Ashwings deployed with the real `tm…` payee (the checkout comes with
   it). Serve the page with `?rpc=<testnet rpc>&co=<checkout>`
   (or edit the config block), and set `net=main` for `t1…` URIs later.
3. The relayer with a funded key and `CORS_ORIGIN` set to the page's
   origin. Put it behind a proxy with `TRUST_PROXY=1`. For the watcher,
   add `ZCASH_RPC_URL` and a cookie for a zebrad whose address RPCs
   answer on that network, and set `START_BLOCK` to the deploy block.
4. The browser must reach the Sova RPC. An https page can't call a
   plain-http RPC unless the RPC is on localhost, and the RPC must allow
   CORS.
5. Wallet dry runs (Zashi, Zingo): scan the QR, check the 8-decimal
   amount, do a z→t send. Tapping the URI on a phone should open the
   wallet.
