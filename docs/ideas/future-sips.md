# What could come next

Ideas that could become SIPs. None of them is a rule yet. Each becomes
consensus only if someone writes it as a SIP, the community discusses it,
and it is accepted. Revised 29 September 2026.

Four lead. The rest follow.

## Lead ideas

### 1. Follow NU7 (ZIP 218)

Zcash's NU7 upgrade reaches testnet on 6 October 2026 and targets mainnet
on 5 November 2026. [ZIP 218](https://zips.z.cash/zip-0218) sets the block
target to 25 seconds, multiplies the halving interval by 3 and divides the
per-block subsidy by 3.

One Sova epoch is one Zcash block, so Sova epochs arrive three times as
often. Settlement, three epochs deep, takes about 75 seconds.

This is now revision 2 of [SIP-3](../../sips/sip-3.md), accepted
2026-09-29. It mirrors ZIP 218: the reward per epoch is divided by
3, to 2,083.33332 SOVA (rounded down so the slow-start step stays exact in
gwei), the slow start stretches to 60,000 epochs and an era lasts 5,040,000
epochs. Daily issuance, the roughly 17-day slow start and the roughly
four-year eras stay as revision 1 published them; the supply bound moves
by about 2,217 SOVA, to 20,937,500,907.58. Sova's mainnet starts after NU7,
so it only sees 25-second epochs. The testnet keeps its flat 6,250 SOVA per
epoch. The `finalized` depth moves from 100 to 300 blocks, keeping about
two hours.

A miner who burns every epoch pays three times the Zcash fees per day.

### 2. Wallet-native burns

Mine from any Zcash wallet, Zodl or an exchange included, by paying a
unspendable burn address. The burn needs no OP_RETURN.

**Risks:** Sova needs an on-chain map from burn address to EVM address;
signal bits have nowhere to go; the address must be provably unspendable.
The burn stays a transparent output, like every burn under
[SIP-1](../../sips/sip-1.md).

### 3. Burn-weight signaling

Every burn already carries 32 signal bits (SIP-1). Tally them per window
and activate a change at a set height, BIP9-style. Upgrades are decided by
the ZEC miners destroy.

**Risks:** low turnout; weight concentrated in a few burners. It needs one
change carried end to end on testnet before it governs mainnet.

### 4. Read shielded-asset supply

Zcash Shielded Assets ([ZIP 226](https://zips.z.cash/zip-0226),
[ZIP 227](https://zips.z.cash/zip-0227)) make issuance transparent, and
every Zcash node tracks each asset's supply. A contract could read that
supply and the asset's finalize flag, the way
[SIP-7](../../sips/sip-7-draft-zcash-events.md) reads pool totals.
Transfers stay shielded; only aggregate supply is read.

**Risks:** ZSAs are not in NU7, and their timing is still debated.

## More ideas

| Idea | Lets a contract or user | Weighs |
| --- | --- | --- |
| Spent outputs | See whether, and where, a transparent output was spent; prove ZEC stayed put with no one holding it | A larger index. Planned for [SIP-4](../../sips/sip-4-draft-zcash-state-precompile.md) v1.1 (`spentBy`) |
| Burns into the NSM | Burn by removing ZEC through [ZIP 233](https://zips.z.cash/zip-0233), so it returns to future Zcash block subsidies ([ZIP 235](https://zips.z.cash/zip-0235)) | Changes what a burn means: "destroyed for good" becomes "returns to Zcash issuance". The community decides. ZIP 233 needs a transaction format NU7 does not ship |
| Published-key payments | Verify shielded payments to a payee who publishes an incoming viewing key | The highest privacy stakes on this list: the payee's whole incoming history, amounts and memos included, becomes public; payers stay hidden. It is disclosure by choice. Every node trial-decrypts every output for every key. Research (SIP-4, section 8) |
| Pay-from-any-wallet conventions | [ZIP 321](https://zips.z.cash/zip-0321) links for Sova orders, one checkout interface, a registry of app burn tags | Conventions only; an informational SIP |
| Zcash events in receipts | Explorers and indexers see Zcash block and pool events as ordinary logs | A new transaction type in pool and RPC code (SIP-7, section 4.2) |
| Light Sova clients | Run RPC, indexers and wallets from Zcash headers and proofs, with no zebrad | ZIP-244 and v6 txid digests enter consensus (SIP-4, section 13) |
| SIP-0 | Statuses, numbering, editors, and a standing rule for following Zcash upgrades | Process only |

## Load on Zcash

Every idea here keeps Sova's footprint on Zcash to burns: ordinary
transactions paying ordinary fees. Wallet-native burns and NSM burns change
only what a burn looks like.

## Watching

- [Crosslink](https://shieldedlabs.net/crosslink/), in development and
  unscheduled, could one day give Sova finality from Zcash.
- [Tachyon](https://tachyon.z.cash/roadmap/) and its oblivious sync.
- [ZIP 231](https://zips.z.cash/zip-0231) memo bundles. Memos stay
  unreadable to Sova either way.

The dev-fund and lockbox debate is Zcash's to settle.

## How to take part

1. Float an idea in the
   [SIPs category of Discussions](https://github.com/sova-chain/sova/discussions/categories/sips).
2. Open the SIP as a pull request to [`sips/`](../../sips).
3. Once live, consensus changes activate by burn-weight signaling.

What we'd like to hear:

- Which Zcash facts would your contract read?
- Should burned ZEC leave supply for good, or return to Zcash miners
  through the NSM?
- What would let you mine from the wallet you already use?
- Which shielded asset would you build on first?
- Where could Sova add load to Zcash?

## Sources

- NU7 dates and contents: [Zcash forum, NU7 timeline](https://forum.zcashcommunity.com/t/nu7-timeline/57655);
  [The Crypto Times, 18 September 2026](https://www.cryptotimes.io/2026/09/18/zcash-nu7-mainnet-upgrade-targets-november-5-with-25-second-blocks/)
- [ZIP 218](https://zips.z.cash/zip-0218), 25-second block target spacing
- [ZIP 226](https://zips.z.cash/zip-0226), [ZIP 227](https://zips.z.cash/zip-0227), shielded assets (Draft)
- [ZIP 233](https://zips.z.cash/zip-0233), [ZIP 235](https://zips.z.cash/zip-0235), the Network Sustainability Mechanism (Draft)
- [ZIP 231](https://zips.z.cash/zip-0231), memo bundles (Draft)
- [ZIP 321](https://zips.z.cash/zip-0321), payment request URIs (Active)
- [Zashi becomes Zodl](https://zodl.com/zashi-is-becoming-zodl/)
- [Zcash forum, defer lockbox distribution](https://forum.zcashcommunity.com/t/draft-zip-defer-lockbox-distribution-and-extend-the-current-dev-fund/50757)
- In this repository: [SIP-4](../../sips/sip-4-draft-zcash-state-precompile.md),
  [SIP-7](../../sips/sip-7-draft-zcash-events.md),
  [the roadmap](../ROADMAP.md),
  [faster blocks](../design/faster-blocks.md)
