# Ashwings ZEC checkout, live on the public testnet (2026-09-25)

The first Ashwing bought with ZEC on the public testnet (chain 82330),
done the way a buyer with an EVM wallet would do it on
`/ashwings/buy`: reserve on Sova, pay TAZ on Zcash, wait for `minConf`,
claim. The calldata is the page's own (`site/src/scripts/checkout/app.ts`
and `chain.ts`), sent with `cast`. Every selector and event topic in
`chain.ts` was checked against `cast sig` / `cast keccak`, and they all
match. No public checkout relayer runs, so the buyer's own EVM wallet
sent `reserve` and `claim` (the page's "injected wallet" path).

## Setup

| | |
| --- | --- |
| Ashwings | `0x1695C7F5DeF320874076120607feb6c2d5C00708` |
| Checkout (`Ashwings.zecCheckout()`) | `0x3bdb5f58C0a4F3e7df6327E8c3A94F48E6eD0565` |
| Listing #1 (read on-chain) | price 5,000,000 zat, payee hash `a0442ac6…9e9d9b` (P2PKH), window 40, minConf 3, active |
| Buyer / recipient (EVM) | `0x0a138EBfBe42408B9605f957e3838A0B2aBFBB44` (testnet deployer keystore) |
| Zcash payer | zcash-devtool testnet wallet (`research-tools/sova-testnet-shield-wallet`, Sapling balance), broadcast via `testnet.zec.rocks` |
| SIP-4 at start | `anchor()` = 4,392,804 = the laptop zebrad's tip; `txInfo`/`txOutput` answered for a real tx in block 4,392,800 |

## The run

Times are UTC from `date -u`.

| Step | What | Evidence | Time |
| --- | --- | --- | --- |
| 1. Reserve | `reserve(1, buyer)`, calldata `0x03339bcb…01…0a138e…bb44` (page's `calldata(SEL.reserve, u256(listing), addrWord(addr))`) | tx `0x20616e6244720718f2093e609f99213667438f15ef8f0edafe3fbf7f6fe0b165`, Sova block 4308, 152,374 gas. `Reserved`: order #1, quote 5,000,001 zat, reservedAt 4,392,807, deadline 4,392,847 | sent 15:29:58, mined 15:30:47 (49 s) |
| 2. Quote → URI | The page's `tAddr` and `zec` logic on `reservations(1)` | `zcash:tmQKm7CN5LaVg83YNRzXPLNy1qBMs8qcqNz?amount=0.05000001`, equal to the deploy record's `zecPayee` | |
| 3. Pay | `zcash-devtool wallet send --address tmQKm7… --value 5000001` (Sapling → transparent, no memo) | txid `e0309f5dc86a88764f5ca8ae7d6db4087779430f2ef19dc6de77e0ed44eb5832`, fee 15,000 zat (ZIP-317), v6 tx, 1 transparent output. It reached the laptop zebrad's mempool from zec.rocks unaided (no rebroadcast needed) | sent 15:31:21 → broadcast 15:31:24; in mempool by 15:31:32 |
| 4. Mined | Zcash testnet block 4,392,809, inside the window (4,392,808 … 4,392,847) | Sova `txInfo`: status 0, height 4,392,809, index 3, nOut 1; `txOutput(…, 0)`: 5,000,001 zat to `76a914a0442ac6…9e9d9b88ac` (the page's vout search picks vout 0) | Sova saw it at 1/3 by 15:32:44 |
| 5. Too early | `claim` dry run at 1/3 | reverts `0x93ef3b03` (`ZcashInsufficientConfirmations`, conf 1, need 3), which the page maps to "not enough confirmations yet" | 15:33 |
| 6. Confirmed | Sova `txInfo` confirmations 3/3 (anchor 4,392,811) | the page's claim dry run (`eth_call` of `claim(1, txid, 0)`) returns item 2 | 15:35:22 (about 4 min after broadcast) |
| 7. Claim | `claim(1, e0309f5d…5832, 0)`, calldata = the page's `claimData()` | tx `0xb28e55996be0dee48e7c8173b1f50bd6d3e5c0ff1ef8e7453a1a39a51cadcee7`, Sova block 4314, 129,265 gas (the design doc's mock estimate was ~128k). `Transfer(0 → buyer, 2)` and `Claimed(1, buyer, e0309f5d…, vout 0, height 4,392,809, item 2)` | sent 15:35:42, mined 15:38:04 (2 min 22 s: it just missed block 4313 at 15:35:43, and 4314 came 2 min 20 s later, one Zcash block interval) |
| 8. Owl | `ownerOf(2)` = buyer; `tokenURI(2)` is `data:application/json;base64`, which decodes to "Ashwing #2" with 8 traits and a 12.5 KB SVG. The page's `claimed()` lookup (`eth_getLogs` from block 0) finds the event | `totalSupply` 2, `zecHeld` 0 (the hold was used by the claim), `reservations(1).filled` true | 15:38 |
| 9. Replay | the same claim again | reverts `0x41a26a63` (`AlreadyFilled`), shown by the page as "order already filled" | |

**Total:** reserve sent at 15:29:58, owl owned at 15:38:04, so **8 min 6 s**
end to end. Of that, 49 s was the reserve, 3 s the ZEC send, about 4 min
waiting for 3 confirmations, and 2 min 22 s the claim's Sova block. The
TAZ spent was 0.05015001 (5,000,001 zat to the payee + 15,000 zat fee;
the wallet went from 32.00155000 to 31.95139999). The EVM gas was 281,639
at 7–8 wei, about 2×10⁻¹² SOVA.

## Findings

### 1. A ZEC buyer also needs an EVM wallet holding SOVA (blocks a ZEC-only buyer)

The contract doesn't need this: `reserve(1, recipient)` and `claim` can be
sent by anyone, for anyone. The live page does. `buy.astro` ships
`relayer: ''` because no public relayer runs, so:

- **With no injected wallet** (for example a phone browser without an
  EVM wallet, and phones are where most Zcash wallets are), **reserve** fails with "no relayer configured:
  connect a wallet". The "connect wallet" button stays hidden, and the
  buyer has no way forward.
- **With a wallet holding 0 SOVA**, the send can't be made. The public
  RPC answers `eth_estimateGas` from a zero-balance address with "gas
  required exceeds allowance (0)" (the base fee is 7 wei). The page shows
  the raw node or wallet message.
- **How much SOVA:** any dust. Reserve plus claim is about 282k gas, about
  2×10⁻¹² SOVA at today's base fee. But a newcomer has exactly zero, and
  the testnet has no SOVA faucet (genesis is empty; the faucet drips TAZ
  only). So the only ways to get it are burn-mining (zebrad + `sova` +
  `sova-miner`, per the join guide) or someone sending it.

So "0.05 ZEC from any Zcash wallet" isn't true as written for someone who
holds only ZEC. The design doc's user story, step 1 ("You don't need
SOVA: the page's relayer calls `reserve`"), describes a relayer that
isn't deployed. Ways to make it true (Rob's call; none are made here):
(a) run `tools/checkout-relayer` publicly (it reserves and claims with its
own gas, and its watcher fills in the txid) and set `relayer` in
`buy.astro`; (b) a SOVA dust drip; (c) say it plainly in the posts
(wording below).

### 2. "Any Zcash wallet" works only for wallets that meet five conditions

Proven here: zcash-devtool paying from a **Sapling** balance to the
**transparent** payee (z→t, v6 tx, ZIP-317 fee), broadcast through
zec.rocks. It propagated on its own. **No memo is needed**, and a
t-address output can't carry one, so memo support doesn't matter. The
wallet must:

1. **Run on testnet.** The payee is `tm…`, the coin is TAZ. A
   mainnet-only wallet can't pay it (not checked wallet by wallet).
2. **Pay a transparent address.** A wallet that refuses t-addresses
   can't pay.
3. **Send an exact 8-decimal amount.** The quote is the price plus a
   per-order tag: order #1 paid 0.05000001, and later orders pay up to
   0.05099999. One zatoshi off can't be claimed, and the TAZ still goes to
   the payee (refunds are off-chain). A wallet that reads the ZIP-321 URI
   `amount=` gets this right. So does one where you can type 8 decimals.
4. **Get the payment mined within 40 Zcash blocks (~50 min) of the
   reserve.** This run's payment was mined 2 blocks after the reserve.
5. **Show the txid**, because with no watcher the buyer has to paste it
   into the page.

A faucet drip (0.1 TAZ to a `tm` address) covers one purchase plus fees,
but paying from that balance is a t→t send, so the wallet must spend
transparent funds.

Not tested: Zashi, Zingo, YWallet. The design doc's "To run it live"
item 5 (one wallet dry run each) is still open.

### 3. Contract, page and SIP-4 agree: no mismatch found

Every selector and topic in `chain.ts` matches `cast`. The page's
`reservations` decoding, `tAddr`/`zec` derivation, vout search, window
checks, claim dry run and error mapping (`0x93ef3b03` too early,
`0x41a26a63` already filled) all behave as the contract does. The
anchor tracked the laptop zebrad's tip exactly. The supply hold was
taken at reserve and consumed by the claim (`zecHeld` back to 0).

### 4. Smaller page issues

- **Raw errors.** The out-of-gas and "connect a wallet" errors aren't
  explained. A line on the page ("you'll need an EVM wallet with a
  little SOVA to reserve and claim") would stop the dead end.
- **Polling load.** The page ticks every 3 s with 3–4 RPCs per tick
  (`reservations`, `anchor`, `txInfo`, the claim dry run), about 10–13
  requests per 10 s per tab. The public RPC allows 50 per 10 s per IP. One
  buyer is fine. Several tabs, or buyers behind one NAT on launch day,
  can trip the limit. Not tested here.

### 5. Ops note

`cast` reads `ETH_PASSWORD` as the **path** of the password file:
`ETH_KEYSTORE=~/.config/sova-testnet/deployer/keystore.json
ETH_PASSWORD=~/.config/sova-testnet/deployer/password`. If you pass the
password itself, cast prints it back in its "does not exist" error.

## Wording for the announcement

**Post 8/9, true today** (269 characters; 276 as X counts the link):

> 8/9 Ashwings: 10,000 owls drawn by their own contract, on Sova.
>
> Testnet mint open: 625 SOVA, or 0.05 ZEC paid to a t-address, checked by the contract on Zcash itself. Both need an EVM wallet with a little SOVA for gas.
>
> Testnet owls go away at reset.
>
> sova.io/ashwings

**If a public checkout relayer is running before posting** (252; 259):

> 8/9 Ashwings: 10,000 owls drawn by their own contract, on Sova.
>
> Testnet mint open: 625 SOVA, or 0.05 ZEC from a Zcash wallet that can pay a t-address, checked by the contract on Zcash itself.
>
> Testnet owls go when the testnet resets.
>
> sova.io/ashwings

Post 9/9 needs no change for this. "0.05 ZEC" is the price. The amount
actually sent is up to 0.00099999 more (the order tag), and the page shows
it exact. That is fine for a post, and the FAQ can spell it out.

**FAQ #11, suggested:** "…One fixed price: 625 SOVA, or 0.05 ZEC paid to
a transparent address, with the contract checking the payment on Zcash
itself (SIP-4). The page gives you an exact amount, 0.05 ZEC plus a few
zatoshis that identify your order; send exactly that, within about 50
minutes. On the testnet today you also need an EVM wallet with a little
SOVA to send the reserve and claim transactions (the gas is a tiny
fraction of one SOVA, but SOVA comes only from burning ZEC). Checked end
to end on 2026-09-25: Ashwing #2, paid in TAZ, minted in about 8 minutes…"

The same "any Zcash wallet" line is in `buy.astro` (lead and meta
description) and in the design doc's user story. Both need the same fix,
or the relayer.

## Part 2: a ZEC-only buyer, through the public relayer (same day, 16:49–16:53 UTC)

After finding 1, Rob chose to run the checkout relayer publicly
(`https://checkout.testnet.sova.io`, relayer `0x73aa9Fa93e4EECDb0A49c3df50F50A0eFb01F9b9`,
funded 5 SOVA, watcher on). `/ashwings/buy` now defaults to it (`fa2c4da`).
This run proves a buyer with **no EVM wallet and no SOVA** gets an owl.

**The buyer.** A brand-new recipient made with `cast wallet new`:
`0xe7124299736Fe9Dcc7d570271120B4be869907e9`, with balance 0 and nonce 0
before the run. It only ever received the owl: its key was never used
and it sent nothing. The ZEC came from the same zcash-devtool wallet as
part 1.

**The calls.** The same requests the page makes with no wallet connected
(`app.ts` `relay()`): `POST /reserve` with `content-type:
application/json`, `Origin: https://sova.io` and body
`{"listingId":1,"recipient":"0xe712…07e9"}`. On a `202`, read the order
id from the receipt's `Reserved` event. The page shows the QR and waits.
The relayer's watcher fills the txid in (`GET /status/:id`) and claims.
**Nobody called `/claim` or `claim()` by hand.**

| Step | What | Evidence | Time (UTC) |
| --- | --- | --- | --- |
| 0. Relayer before | `GET /status` | balance 5.000000000000000000 SOVA, accepting, `openReservations` 0/20, watching | 16:49:28 |
| 1. Reserve | `POST /reserve` → **`202 {txHash, pending: true}`** after 26.7 s (the block took longer than `RESPOND_WAIT_MS` 25 s); then the receipt | tx `0xafa5befc28ff5d5f498ca50915e2d14abcd756143d072720ebaea3333328d120`, **sent by the relayer**, Sova block 4539 (16:50:21), 145,191 gas. `Reserved`: **order #2** for the fresh address, **quote 5,000,002 zat** (`zcash:tmQKm7CN5LaVg83YNRzXPLNy1qBMs8qcqNz?amount=0.05000002`), reservedAt 4,393,038, **deadline 4,393,078** | POST 16:49:38 → 202 at 16:50:04 → mined 16:50:21 |
| 2. Pay | `zcash-devtool wallet send --identity <age file> --address tmQKm7… --value 5000002` (Sapling → t) | txid **`aabb6a3326e8aae7254dd320f8d754f42deccd2c07f57a0d98ae0eb5010e105a`**, fee 15,000 zat. Mined at Zcash **4,393,045** (block time 16:51:50), 7 blocks into the 40-block window | started 16:51:39, broadcast 16:51:49 |
| 3. Watcher finds it | `GET /status/2` | `detected {txid aabb6a33…, vout 0, height 4,393,045}`, `claim.state "sending"` | by 16:52:17 (first poll) |
| 4. Auto-claim | the relayer's watcher | tx **`0x34f34fb3a9f89f26f7d449df8acd3e1e60a91a9006b0dc680df1ba84c5cc79bb`**, **from the relayer**, Sova block 4550 (16:52:27; anchor 4,393,049, 5 confirmations), 146,365 gas. `Transfer(0 → 0xe712…07e9, 3)`, `Claimed(2, …, aabb6a33…, vout 0, height 4,393,045, item 3)`. `/status/2` → `claim.state "claimed"`, `watching false` | 16:52:27 |
| 5. Owl | reads | **`ownerOf(3)` = `0xe7124299736Fe9Dcc7d570271120B4be869907e9`**. `tokenURI(3)` decodes to "Ashwing #3", 8 traits, 11.8 KB SVG. The fresh address has **balance 0, nonce 0** after. `totalSupply` 3, `zecHeld` 0 | 16:53–16:54 |
| 6. Relayer after | `GET /status` | balance **4.999999999997959108** SOVA: down 2,040,892 wei = (145,191 + 146,365) gas × 7 wei. `openReservations` **0**, accepting, watching | 16:54:01 |

**Timing.** From the reserve POST to the owl: **2 min 49 s**. From the ZEC
broadcast to the owl: **38 s**. That was luck. The Zcash testnet was in a
min-difficulty burst (4,393,045–4,393,049 in 32 s), so the payment was
mined 1 s after broadcast and reached 3 confirmations fast. At the
target 75 s per block, expect about 4 min for 3 confirmations plus one
Sova block, as in part 1. The claim path used: **the watcher's
auto-claim**. The `/claim` fallback wasn't needed.

### Findings, part 2

1. **Finding 1 is resolved for the page's flow.** A buyer needs a Zcash
   wallet and an EVM **address** to receive the owl, nothing more: no
   injected wallet, no SOVA, no transaction from that address, and no
   txid to paste, because the watcher detects and claims. The owl sits in
   the address. *Moving or selling it later* is an EVM transaction and
   needs SOVA for gas.
2. **A slow block gives a `202`, and the page handles it.** Reserve took
   longer than the relayer's 25 s wait, so it answered `202 {txHash,
   pending}`. At ~75 s Sova blocks, most reserves will do that. `app.ts`
   then polls `eth_getTransactionReceipt` every 1.5 s (up to 5 min) and
   reads the order id from the `Reserved` event. That works, and it adds
   about 7 RPC requests per 10 s per tab while it waits. Fine for one
   buyer; the part 1 NAT note still applies.
3. **Relayer gas isn't the limit. Its caps are.** One ZEC sale cost the
   relayer about 291.6k gas, 2.04×10⁻¹² SOVA at 7 wei, so 5 SOVA covers
   any plausible launch at today's base fee. The binding limits are the
   rate limits (3 reserves per IP per hour, 60 per hour overall, 10 per
   minute) and **20 open unpaid orders**. Anyone with a few IPs could
   hold all 20 slots for a payment window (~50 min), and new buyers
   would get `503` (the page says "relayer 503"). Not tested; worth
   watching on launch day.
4. **What a wallet still has to do** (finding 2 of part 1, minus the
   txid): run on testnet, pay a transparent `tm…` address, send the
   exact 8-decimal amount from the QR/URI, and get it mined within 40
   Zcash blocks (~50 min). Zashi, Zingo and YWallet are still untested
   wallet by wallet.
5. **Ops.** The only secret this run used is the devtool's age identity,
   passed as a file (`--identity`). The fresh key is in a 0600 file in
   the job's scratch directory and was never printed.

### Final wording, now that the relayer is live

**Post 8/9** (268 characters; 275 as X counts the link):

> 8/9 Ashwings: 10,000 owls drawn by their own contract, on Sova.
>
> Testnet mint open: 625 SOVA, or 0.05 ZEC from any Zcash wallet that can pay a t-address. The contract checks the payment on Zcash itself; no SOVA needed.
>
> Testnet owls go away at reset.
>
> sova.io/ashwings

This supersedes both part 1 versions. "No SOVA needed" is true of paying:
the relayer pays the reserve and claim gas.

**FAQ #11:**

> 10,000 pixel owls drawn by their own contract on Sova, from a seed fixed at mint; the contract has no owner and serves its own metadata. One fixed price: 625 SOVA, or 0.05 ZEC from any Zcash wallet that can pay a transparent address, with the contract checking the payment on Zcash itself (SIP-4). To pay in ZEC you need no SOVA and no EVM wallet, just an address to receive the owl: sova.io/ashwings/buy reserves your order through a relayer that pays the gas, shows a QR with your exact amount (0.05 ZEC plus a few zatoshis that identify the order), and the owl arrives about three Zcash blocks after your payment is mined. Pay the exact amount within about 50 minutes of reserving. Moving or selling an owl later needs a little SOVA for gas. There's a market in SOVA with a 1% fee. The testnet mint is open at sova.io/ashwings. Testnet owls are testnet-only and go away when the testnet resets.

The same "any Zcash wallet" line in `buy.astro` (lead and meta
description) is now true enough with the relayer, but is better
qualified the same way ("that can pay a t-address").
