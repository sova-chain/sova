# Sova: An EVM Chain That Reads Zcash

Sova
sova.io
September 2026 (pre-release draft)

**Abstract.** Contracts that accept ZEC today accept a wrapped token instead: a claim on coins that a custodian holds on Zcash, on a chain whose contracts cannot read Zcash at all. We propose an EVM chain whose every node runs a Zcash node, so that a contract can verify a ZEC payment against Zcash itself and the ZEC can stay where it was paid. The chain’s gas, SOVA, is issued only by destroying ZEC in a transparent Zcash transaction that names an EVM address. Burns are grouped by Zcash block, one Sova block per Zcash block, so Zcash’s proof-of-work orders Sova’s epochs and fixes what each one mints. Each epoch’s fixed reward is shared among that epoch’s burners in proportion to the ZEC each destroyed, and the largest burner assembles the block. Every node re-derives every mint from its own Zcash node and rejects a block that disagrees, and every Sova block commits to the Zcash block it settles, so a precompile can answer a contract’s questions about the transparent Zcash chain with the same answer on every node. Privacy is at the funding edge, where a shielded balance funds a burn or a payment; execution is public.

## 1. Introduction

Shielded ZEC is digital cash: it moves without a ledger trail and without permission [1]. It cannot run programs. To let a program react to a ZEC payment, the practice has been to move the value onto a chain that runs programs: a custodian, a multisignature group or a bridge takes the ZEC and issues a token in its place. The token is a claim on the custodian, and the contracts that use it see the token, never the coin. They cannot tell whether a payment happened on Zcash, and the holder has given up what made the cash worth programming: possession of it. The arrangement works as long as the custodian is honest and solvent, and everything built on it inherits that condition.

What is needed is a chain whose contracts can verify a Zcash payment directly, so that the payment can be made on Zcash, to the payee, in the payee’s own keys; and whose own asset comes into existence without anyone holding anything on the other side.

In this paper, we propose an EVM chain that runs beside Zcash and reads it. Every Sova node runs its own Zcash node. The chain’s asset, SOVA, is issued only when ZEC is destroyed in a Zcash transaction that names the address to credit: the burn is the mining act, and afterward the ZEC exists for no one. Burns are grouped by Zcash block, one Sova block per Zcash block, so Sova’s order is Zcash’s, and every Sova block commits to the Zcash block it settles. Because every node derives every mint from Zcash, a node can also answer a contract’s question about Zcash deterministically, and that is what lets a contract verify a payment. Sova changes nothing in Zcash and posts nothing to it.

## 2. Burns

We define a burn as one transparent Zcash transaction [1, 2] that destroys ZEC and names the EVM address to credit. It pays at least 1,000 zatoshis to the eater script `76a914 00…00 88ac`, a standard pay-to-public-key-hash output whose key hash is twenty zero bytes. No preimage of that hash is known, so the value is unspendable. The transaction carries exactly one OP_RETURN output of 29 bytes: the two-byte magic “SV”, a version byte, the 20-byte EVM address, and 32 signal bits for upgrade signaling. The burn’s weight is the ZEC it destroys, summed over its eater outputs (Figure 1). The Zcash fee is paid to Zcash miners as usual and is not weight.

The rule is total: every transaction either is a burn or is not. A transaction with no payload output, two payload outputs, an undecodable payload or too little eater value is not a burn, and no node treats it as an error. A burn needs nothing from Sova when it is made, and Zcash needs to know nothing about Sova: the burn is recognized after the fact by anyone who reads the Zcash chain with the rule above.

Weight is linear in the ZEC destroyed, so splitting a burn into several transactions, or several addresses, earns exactly what one burn of the same total earns. There is no advantage in appearing to be many burners.

## 3. Epochs

To order Sova without a proof-of-work of its own, we use Zcash’s. One Zcash block is one epoch, and each epoch has exactly one Sova block. From a base height B, the Zcash height at which the network began, the Sova block for epoch E sits at Sova height E − B + 1 [3]. That block settles the burns confirmed in the epoch’s Zcash block: it credits their addresses, and nothing else can. Zcash blocks arrive about every 75 seconds, and so do Sova’s.

Each Sova block also commits to the hash of the Zcash block it settles, in a header field the EVM inherited from Ethereum’s beacon chain and that Sova had no other use for [5]: Zcash is Sova’s beacon (Figure 2). The parent hash chains Sova blocks to one another, and the anchor chains each of them to a Zcash block, whose own header chains to every Zcash block before it. A Sova block therefore names one Zcash history, and no Sova history can name a Zcash history that did not happen. The reverse does not hold: Zcash does not name Sova, so several Sova histories can name the same Zcash chain, and Sova chooses among them by rules of its own (Sections 5 and 10).

If Zcash reorganizes, the Sova blocks that settled the replaced Zcash blocks are unwound and derived again from the replacement chain, so a mint is final at Zcash confirmation depth [3]. Among Sova blocks that settle the same Zcash chain, a node chooses near the tip by the preference of Section 5; Section 10 says what holds deeper. An epoch with no burns still has its block, which mints nothing.

## 4. Issuance

SOVA is created only by settling burns [4]. The genesis allocates nothing, so every SOVA in existence was minted to someone who destroyed ZEC. Each epoch has a scheduled reward R, and the burners of that epoch share it. Nine tenths of R is divided among them in proportion to weight; the remainder, one tenth of R plus the rounding dust, is a tip to the burner who seals the block (Section 5). All arithmetic is in gwei with floor division, so the shares sum to R exactly:

    s_i = floor( (9/10) · R · w_i / Σ_j w_j )                       (1)
    t   = R − Σ_i s_i                                                (2)

The reward does not grow with the ZEC burned. More burners in an epoch divide the same R more thinly, so competition sets the price of a SOVA in destroyed ZEC and never the supply. A burner who wants a larger share must destroy more ZEC; nothing else counts. An epoch with no burns mints nothing, and its reward is never made up: there is no accumulated reward waiting for the first burn after a quiet stretch, and the amount minted tracks demand.

The schedule, for the 0-based epoch index E, is

    R(E) = 0.3125 · (E + 1) SOVA                    if E < 20,000
         = 6,250 / 2^k SOVA, k = floor(E / 1,680,000)   otherwise      (3)

with halvings rounded down to whole gwei. The first 20,000 epochs ramp the reward from 0.3125 SOVA to 6,250 in equal steps, the slow start Zcash itself used at launch, so that the first days, when the tooling is new and SOVA has no market, cannot be captured cheaply. The reward then halves every 1,680,000 epochs, which is Zcash’s own halving interval, because an epoch is a Zcash block. Era 42 pays 1 gwei per epoch and era 43 pays nothing. Total issuance can therefore never exceed 20,937,503,124.97 SOVA, and it reaches that only if every epoch has a burn (Section 10).

As the reward halves, sealers are paid by fees. Fees follow EIP-1559 [7]: the base fee is destroyed, and priority fees, with the tip, go to the sealer. The schedule can change only by a soft fork that burners signal in the burns themselves, through the 32 signal bits of Section 2, and adopt by burn weight; the tally rule is future work.

## 5. Sealing

Some burner has to assemble the epoch’s block: choose transactions, execute them, and publish. We rank the epoch’s burners by weight, heaviest first, ties to the smaller transaction id compared byte by byte [3]. Rank 0 seals. If rank 0 has not sealed, rank r may seal once r × step has passed since the epoch became sealable; the draft step is 15 seconds. A node that did not burn in the epoch never seals it. The ladder is for liveness only: it says who may seal now, and nothing about which block is valid or preferred.

The rewards enter the chain as the block’s withdrawals, the balance-credit list Ethereum added at Shanghai [8], one entry per burner in rank order with the amount in gwei. The list is a pure function of the epoch’s burns and the sealer’s rank: rank the burners, apply (1) and (2), emit one entry each. A block claiming epoch E must carry exactly the list that some rank of E produces, or it is invalid. Because the tip goes to the sealer, each rank’s list is different, and a node recovers the sealer’s rank from the withdrawals alone.

Among an epoch’s valid blocks every node prefers the lower rank, then the lower block hash. A rank-0 block that arrives after a rank-1 block was accepted displaces it (Figure 3): the node replaces the one block at the tip and builds on rank 0. The same preference decides between branches. A block is a candidate if its ancestry, through blocks the node holds, meets the node’s chain, and where it leaves that chain it either extends the node’s head or is preferred to the node’s block at that height, with at most three of the node’s blocks to replace. Candidates are compared by their blocks at the height where their branches part. A branch that would replace more is not a candidate, however it is ranked, so a node never replaces a block once three blocks are built on it. Preference is decided by the receiving node from the block’s content, never by the sender and never by arrival time, so two nodes that see the same blocks choose the same head, even after building on different blocks for up to three epochs.

A block does not yet name its sealer; the rank is read from the tip. What remains open in this rule is that a block’s author cannot be checked from its header alone. A draft rule [9] has the sealer sign the header with the key of the address its burn credits, and gives an epoch that no ranked burner seals one fixed empty block, so that a single burn to a dead address cannot stop the chain. Section 10 assumes that rule.

## 6. Network

The steps to run the network are as follows:

1) Each node runs a Zcash node and follows it block by block. For every new Zcash block it extracts the burns, ranks the burners, and computes the epoch’s settlement for every rank.
2) A burner broadcasts a burn to the Zcash network, like any Zcash transaction. It needs no Sova node to do so.
3) When the epoch becomes sealable, the ranked burner whose turn it is assembles the block, with the epoch’s settlement as its withdrawals, the anchor in its header and the pending transactions, executes it, and announces it to its peers by height and hash.
4) A peer that lacks an announced block requests it and receives it. Blocks are pulled, never pushed.
5) Each node validates a received block against its own Zcash view (Section 7). A valid block becomes a candidate for its epoch if its branch meets the node’s own chain as Section 5 requires, the node moves its head to the best candidate it knows by the preference of Section 5, and it announces a block onward only after accepting it.
6) The next epoch’s sealer builds on the head its own node has chosen.

Nodes speak sova/1, a small sub-protocol of the Ethereum wire stack with three messages: Announce, GetBlock and Block. No message can move a node’s head; only the node’s own choice does. Peers find each other by the standard discovery protocols from bootnodes that every node sets explicitly, and the network has its own genesis and fork identifier, so a peer from another chain built on the same software is refused at the handshake. A node that has fallen behind fetches the missing blocks from peers by the ordinary Ethereum sync, but only up to the Zcash height its own Zcash node has reached, so that every block it imports meets a settlement it can check. Between two histories that both check, it does not yet choose by a rule of its own: it takes the first tip it is offered, and its operator should pin a recent block hash. A rule that ranks whole histories, and checkpoints shipped with the client, are future work (Section 10).

## 7. Verification

A node accepts a block only if its own Zcash node justifies it. The check is a consensus rule. For a block at height N, which settles epoch E_N = N + B − 1, the node requires that the anchor equals the hash of Zcash block E_N as its own Zcash node has it, and that the withdrawals equal the settlement it derived for E_N at some rank. The withdrawals are in the block body, so the check needs no execution, and it runs on every path by which a block can enter: delivered at the tip, fetched to fill a short gap, or synced as history. A mint is vouched for by the Zcash chain alone, as each node reads it. A peer cannot mint what the receiving node’s Zcash node does not show, and a history in which one mint was altered is rejected at that height by every node that syncs it. A history in which every mint is intact and the transactions differ is not rejected; it is another valid history, and Section 10 says how a node chooses between two and what it costs to make one.

A block may arrive seconds before the node’s own Zcash node has the block it settles. Such a block is held, not rejected: the node retries once its scan reaches E_N. A block whose anchor names, at E_N, a hash the node’s Zcash node does not have is also held, because Zcash may return to that branch. Only a block that is wrong on a matching anchor, with a withdrawals list that no rank produces or a bad state root, is invalid for good.

A joining node scans Zcash first, which is fast against a local Zcash node, then syncs Sova only as far as its scan reaches. It therefore checks the oldest mint as carefully as the newest, and nothing in the check is ever pruned. The cost of this is that every Sova node runs a Zcash node. That is also what makes the next section possible.

## 8. Reading Zcash

Since every node holds the Zcash chain up to the block its head commits to, a contract can be allowed to read it. A precompile at a fixed address answers questions about the transparent Zcash chain from the base height B through the block E_N that the executing Sova block settles [5]: the anchor itself, by height and hash; the hash and time of the block at a height; where a transaction was mined, its position and its depth; what a transparent output pays and to which script; and whether a transaction is a burn, decoded by the rule consensus uses. Every answer is a pure function of the committed chain. Two nodes that accept the same Sova block compute the same answers, whatever their own Zcash tips at the time, so the state root is the same on every node. A node that cannot answer yet holds the block; it never guesses. “Not found” is an answer, and it is the same on every honest node. Depth is counted from the committed block, E_N − h + 1, never from a node’s tip. The protocol fixes no confirmation depth, because the anchor already makes every answer deterministic; the depth a contract requires is its own economic choice, and the contract library makes every check state it.

This is enough to be paid in ZEC. A seller’s contract records an order with a fresh Zcash address that the seller controls; the buyer pays it from any Zcash wallet, a shielded balance included; and once the precompile shows at least the price at that address, at the depth the contract demands, the contract delivers. The ZEC went from buyer to seller on Zcash, and the seller held it from the moment it was paid. A Zcash reorganization deeper than the payment unwinds the Sova blocks built on it, on every node alike, and the delivery with them, which is why the depth is the contract’s to choose and should exceed whatever could reverse what the contract did outside Sova.

What the precompile cannot see is what Zcash hides from everyone: the amount, sender, recipient and memo of a shielded transfer, and the balance of a shielded address. A transaction’s id, height and position are public even when it is fully shielded, and so is a transparent output paid from a shielded balance. Transaction ids before Zcash’s v5 format are malleable, so a contract keys on what an output pays, not on an id chosen in advance. What Zcash does publish about its shielded pools, the value held in each and every change to it, can be read the same way; a draft extension of the precompile does so [10].

## 9. Privacy

The EVM is public. Every call, balance and state change on Sova is visible, as on Ethereum. Privacy in this design is at the edge where value enters: a burn, and a payment a contract verifies, are both transparent Zcash outputs, and both can be funded from the shielded pool. A z→t transfer has no visible input, so the transparent address that then burns or pays is not linked on the ledger to whoever funded it [1]. The link is broken before the burn, by Zcash, and Sova never sees it.

Three things stay visible. Burns from one address are linked to each other, so the address is a persistent pseudonym for as long as it is used. The deshielding transfer is public in amount, address and height; a distinctive amount, or a deshield followed at once by a burn, is an easy correlation. And the SOVA minted, and everything done with it, is public. Each of these is a matter of habit rather than protocol: fresh addresses, unremarkable amounts, and time between funding and use.

## 10. Calculations

We consider what it costs to attack the chain and what it costs to take part in it.

**Rewriting a mint.** A mint is asserted by no one; it is derived from a Zcash block, and the Sova block that carries it names that Zcash block by hash. To change a mint at Sova height N, an attacker must replace Zcash block E_N and every Zcash block after it, that is, reorganize Zcash from that depth, and every Sova node would follow the reorganization identically. The probability that an attacker overtakes the honest Zcash chain from z blocks behind is as computed in [6].

**Rewriting a transaction.** Rewriting a block without changing its mint is a different matter. Every block that carries the epoch’s settlement at some rank and the epoch’s anchor is valid, whatever it extends and whatever it contains, so Zcash alone does not decide between two such histories, and Sova adds no proof-of-work of its own. Near the tip, the preference of Section 5 decides: a node replaces at most three of its blocks, and only for a branch whose block, where it leaves the node’s chain, has the better rank for the same epoch. The common case is the one block at the tip, replaced by a block of better rank, which pays every burner the same share and moves the tip to its sealer. A transaction is therefore settled once three more epochs are built on its block, about four minutes, not before. Within those three blocks, a rewrite costs a better-ranked block for the epoch where it begins: with the sealer’s signature [9], a block signed by a better-ranked burner of that epoch; until the signature ships, a block that claims the better rank, which anyone can produce. Deeper, a node that is online is not moved at all: a branch that would replace more than three of its blocks is not a candidate (Section 5), however ranked. The same bound keeps a split deeper than three blocks from healing by itself. A node that is joining or catching up has no chain of its own yet to hold to; between two valid histories it takes the first it is offered (Section 6), and until checkpoints ship with the client, its operator pins a recent block hash. Two stronger rules are future work: a preference among whole histories by the ranks of their sealers, so that rewriting from height N takes the key of the rank-0 burner of every epoch since N [9], which still leaves those sealers able, together, to rewrite what they sealed; and burns that name a recent Sova block, so that the ZEC destroyed since N counts for the history it named, and a rewrite must destroy more or reorganize Zcash. A Sova history can never be harder to rewrite than the Zcash history it names; the rules above say how much easier.

**Censoring at the tip.** A sealer chooses its block’s transactions for one epoch. With the sealer’s signature in the header [9], keeping a transaction out for k epochs means being rank 0 in each of them, that is, out-burning every other burner k times, and the ZEC is destroyed whether or not the transaction was worth excluding. The transaction is included by the first sealer who is not the attacker, and stays included as long as that block is not rewritten (Sections 5 and 10).

**The price of a share.** In an epoch with total weight W = Σ w, a SOVA from the pro-rata pool costs W / ((9/10) R) in destroyed ZEC. Since R is fixed by the schedule, competition raises this price and never the amount minted. At the floor, a burn of 1,000 zatoshis costs more in Zcash fees, about 20,000 zatoshis, than in ZEC destroyed; a miner that burns in every epoch spends roughly a quarter of a ZEC a day, most of it to Zcash miners.

**Emission.** The slow start mints 62,503,125 SOVA over its 20,000 epochs, 62,496,875 less than a flat reward would. Each era of 1,680,000 epochs lasts about four years, and with halvings floored to whole gwei, era 42 pays 1 gwei per epoch and era 43 pays nothing, so emission ends after 43 eras, a little over 170 years, at an upper bound of 20,937,503,124.97 SOVA. Every epoch without a burn lowers the total that will ever exist.

**A worked epoch.** Two burners with weights in the ratio 5 : 2, and R = 6,250 SOVA. By (1), rank 0’s share is floor(5,625 × 5/7) = 4,017.857142857 SOVA and rank 1’s is 1,607.142857142; by (2) the tip is 625.000000001, which rank 0 also receives as sealer, for 4,642.857142858 in all. The three amounts sum to 6,250 exactly. If rank 1 seals instead, the shares are the same and the tip moves to rank 1, so the two blocks differ and a node can tell which rank sealed.

## 11. Conclusion

We have proposed a chain for programs that can verify Zcash payments, with an asset that is issued without an issuer. We started with burns: a Zcash transaction that destroys ZEC and names an address, recognized by a rule anyone can apply. Grouping burns by Zcash block gives the chain its order and its clock from Zcash’s proof-of-work, and lets each Sova block commit to the Zcash block it settles. A fixed reward shared by weight makes destroying ZEC the only way to earn SOVA, and makes competition raise its price rather than its supply. Ranking the burners chooses a sealer without an election, and a preference on rank keeps every node’s choice the same near the tip. Because every node derives every mint from its own Zcash node, a node trusts no other node for anything, and the same derivation gives contracts a view of Zcash that is identical everywhere. The ZEC a contract verifies stays with its payee on Zcash, and the chain holds nothing.

Today the design runs as a local network, a Zcash regtest node, a Sova node and a miner started by one command, and the source and these specifications are public [2, 3, 4, 5]. The anchor and the precompile of Sections 3, 7 and 8 are specified and built, and ship with the public testnet, which will be the first public network. The sealer signature of Section 5 is built and switches on with the public testnet. Mints are final at Zcash depth; transaction history is not yet: an online node replaces no block once three are built on it, a joining node takes the first valid history it is offered, and the rules of Section 10 that would rank histories objectively are future work. A wrapped ZEC held by a third party’s signers is possible later work; it would be custody, it would be called custody, and it is no part of this design.

## References

[1] D. Hopwood, S. Bowe, T. Hornby, N. Wilcox, “Zcash Protocol Specification,” https://zips.z.cash/protocol/protocol.pdf.
[2] Sova, “SIP-1: The Burn Transaction Format,” https://github.com/sova-chain/sova/blob/main/sips/sip-1.md.
[3] Sova, “SIP-2: Epochs, Rewards, and Settlement,” https://github.com/sova-chain/sova/blob/main/sips/sip-2.md.
[4] Sova, “SIP-3: Emission Schedule,” https://github.com/sova-chain/sova/blob/main/sips/sip-3.md.
[5] Sova, “SIP-4: Zcash State Precompile,” https://github.com/sova-chain/sova/blob/main/sips/sip-4-draft-zcash-state-precompile.md.
[6] S. Nakamoto, “Bitcoin: A Peer-to-Peer Electronic Cash System,” 2008, https://bitcoin.org/bitcoin.pdf.
[7] V. Buterin, “Ethereum: A Next-Generation Smart Contract and Decentralized Application Platform,” 2014, https://ethereum.org/whitepaper; V. Buterin et al., “EIP-1559: Fee market change for ETH 1.0 chain,” https://eips.ethereum.org/EIPS/eip-1559.
[8] A. Dietrichs et al., “EIP-4895: Beacon chain push withdrawals as operations,” https://eips.ethereum.org/EIPS/eip-4895.
[9] Sova, “SIP-6: Sealer Signatures” (accepted), https://github.com/sova-chain/sova/blob/main/sips/sip-6-draft-sealer-signatures.md.
[10] Sova, “SIP-7: Zcash Pool State and Events” (accepted), https://github.com/sova-chain/sova/blob/main/sips/sip-7-draft-zcash-events.md.
