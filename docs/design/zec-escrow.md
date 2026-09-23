# ZEC → SOVA escrow (D1, SIP-4 demo)

Buy SOVA with real ZEC. Nobody holds anybody's ZEC: it goes from the
buyer straight to the seller on Zcash, and Sova reads the payment through
the SIP-4 precompile. Code: `contracts/src/zcash/` (`ZecEscrow.sol`,
`ZcashLib.sol`, `IZcash.sol`). Demo: `contracts/script/ZecEscrowDemo.s.sol`
(**needs SIP-4 live**). Status: contracts and tests done against a mock
precompile. It runs on a devnet once the node side ships.

## Flow

1. **Maker** makes a fresh t-address and calls `createOrder`. It locks
   the SOVA and sets the price in zatoshis, the address (its 20-byte hash),
   the bond, the window `T` (in Zcash blocks, ~75 s each) and `minConf`.
2. **Taker** calls `reserve(id)` and posts the bond. The reservation
   names the taker and records the current Zcash anchor height `A`. The
   window runs until anchor `A + T`.
3. **Taker** sends at least the price to the maker's t-address from any
   Zcash wallet, then waits for `minConf` blocks.
4. **Anyone** calls `claim(id, txid, vout)`. The contract checks through
   the precompile that the output pays at least the price to exactly
   `OP_DUP OP_HASH160 <hash> OP_EQUALVERIFY OP_CHECKSIG`, is mined at a
   height above `A`, has at least `minConf` confirmations, and that the
   anchor is still at or below `A + T`. The SOVA and the bond go to the
   taker.
5. **Window lapses:** `expire(id)` (anyone) sends the bond to the maker
   and reopens the order. A new `reserve` does the same implicitly. The
   maker can `cancel` whenever no reservation is live.

## Guarantees

- No custody, no admin, no owner, no upgrade path and no fees. SOVA
  leaves the contract only to the reserving taker on a verified payment,
  or back to the maker.
- One payment fills at most one order: each `(txid, vout)` is recorded
  on fill. Only one open order can use a given maker address at a time,
  so nobody can claim someone else's payment on a twin order.
- A payment made before the reservation can't be claimed. That includes
  payments made to the address before the order existed.
- Every answer comes from the Zcash chain prefix the Sova block commits
  to, so all nodes agree. A Zcash reorg that removes the payment reorgs
  Sova with it, and the claim rolls back too.

## Limits (honest)

- **Free option.** During `T` the taker can watch the price and simply
  not pay, losing only the bond. The maker controls the bond size and
  `T`.
- **Pay promptly.** A payment that confirms after the window is lost to
  its sender. The maker has the ZEC and the bond is forfeited. If the
  order has reopened, the next reserver can claim that late payment.
  Never pay after your deadline.
- **Transparent maker address.** The maker's receiving address and the
  amount are public on Zcash. Use one address per order and sweep it to
  shielded afterwards.
- **Reorgs below `minConf`** can undo a fill on Sova. They can't undo
  what the taker did outside Sova in the meantime. Use testnet 3 and
  mainnet 10, and more for size.

## Which wallets can pay

Any Zcash wallet that can send to a **transparent (t-) address**. The
payment needs no memo or OP_RETURN, because the address is per order
and the reservation already names the claimant. That includes paying
**from a shielded balance** (z→t): the buyer's funds stay private up to
that one output. Full-node wallets (`z_sendmany`), mainstream mobile
wallets and most exchange withdrawals can send to a t-address; not yet
tested wallet by wallet. The taker
needs the txid (as the explorer shows it) and the output index that pays
the maker.
