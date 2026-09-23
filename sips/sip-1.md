# SIP-1: The Burn Transaction Format

- Status: **Frozen** (2026-09-23, approved by Rob). The freeze
  condition, one burn relayed and mined on the public Zcash testnet
  through third-party peers, was met by txid
  `641cc3068557651ed4a0e1d4664a14ad4745bcf7a89ce4d9b251313e6f51d231`
  (10,000 zat burned), broadcast from our testnet zebrad and mined at
  height 4,383,754 in block
  `0021b6fbbae671108ee99f1a1e57d285c9c54deab13085b0575e3a847014cf57`
  by an unrelated miner (coinbase to `tmSQnvRGQptJWESW2RQbygmEw744wpkYzdU`).
  Our node recognized it as a SIP-1 burn (`box/sip1-relay-check.sh`,
  `MATCH: yes`). Changing any rule below now takes a new SIP.
- Implementation: `crates/consensus/src/sip1.rs` (normative reference)
- Author: Sova (orchestrated draft)

## Abstract

A Sova mining act is one Zcash transparent transaction that provably
destroys ZEC and names the EVM address to credit. This SIP defines the
transaction's on-chain format and the total, deterministic recognition
rule every Sova node applies.

## The burn transaction

A transaction is a burn iff its transparent outputs satisfy all of:

1. **Exactly one payload output**: value-0, script exactly
   `OP_RETURN OP_PUSHBYTES_27 <payload>` (29 bytes; no PUSHDATA
   alternates, no trailing bytes). Payload layout, 27 bytes:

   | bytes | field |
   | --- | --- |
   | 0–1 | magic `"SV"` (0x53 0x56) |
   | 2 | version = 0x01 |
   | 3–22 | EVM address credited |
   | 23–26 | signal bits, big-endian u32 (BIP9-style upgrade signaling) |

2. **One or more burn outputs**: value paid to the canonical eater
   script — standard P2PKH whose hash160 is twenty zero bytes
   (`76a914 00…00 88ac`). No preimage of the zero hash is known;
   the value is computationally unspendable.

3. **Dust floor**: summed eater value ≥ 1,000 zatoshis (anti-spam
   constant; soft-forkable upward).

The burn's **weight** is the summed eater value. A transaction with zero
payload outputs, two or more payload outputs (ambiguous), an undecodable
payload, or insufficient eater value **is not a burn** — malformed burns
never error; they simply don't exist. The rule is total: every possible
transaction deterministically either is a burn or is not.

## Relay properties

The payload script (29 bytes) fits Zebra's default 83-byte datacarrier
standardness cap (zcashd `MAX_OP_RETURN_RELAY` parity; policy applied on
all networks, verified in Zebra source). ZIP-317 conventional fees apply
to burn transactions like any other (a 1-in/3-out burn ≈ 3 logical
actions ⇒ 15,000 zat minimum fee); the fee pays Zcash miners and never
counts toward burn weight. Burns MAY be funded by a z→t deshielding
transaction — the shielded pool breaks the funding linkage (see the
miner's "Anonymous funding" documentation for honest caveats).

## Rationale

- One-payload strictness makes crediting unambiguous without ordering
  rules. Linear weight makes splitting pointless (Sybil-proof by
  arithmetic). The zero-hash eater is recognizable without address
  machinery and provably outside anyone's control.
