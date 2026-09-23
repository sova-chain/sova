# Wrapped ZEC and NEAR: status (2026-09-23)

**Short answer:** the direction is decided and the spec is drafted. No
peg code exists yet. The peg waits on two Sova changes (SIP-4 v1.1
`spentBy` and SIP-6) and on one NEAR-side capability that we won't ask
NEAR for until a SIP-4 demo runs on the public testnet.

**Decided (Rob, 2026-09-23; `zec-peg-v2.md` §4.3)**
- wZEC ships **custodied by NEAR Chain Signatures** (11 of 17 MPC
  operators today; TEE not yet enforced), behind a keyless, immutable
  NEAR vault contract. The custody is stated every time wZEC is mentioned,
  and wZEC is never called trustless.
- Sova provides the contracts and the monitoring. The project holds no
  signer key and runs no relayer or RPC that NEAR trusts.
- Sova watches the vault from Zcash through SIP-4. There is no
  NEAR-reading precompile.
- Self-custody vaults are not wZEC.
- NEAR outreach only after a SIP-4 demo exists.

**Designed**
- `sips/sip-5-draft-wrapped-zec.md` (draft, 8 decisions for Rob in §10):
  - per-account tagged-P2SH deposit addresses, minted on SIP-4 proof with
    no signer involved;
  - Sova plans each payout transaction exactly, and NEAR checks it, waits
    a delay, re-verifies, applies a daily cap, and signs;
  - theft and overdue proofs on Sova halt minting automatically, with a
    pro-rata haircut after a theft;
  - no admin or pause key anywhere; testnet capped at 1,000 TAZ.
- `sips/sip-6-draft-sealer-signatures.md`: signed Sova blocks, needed
  before any peg testnet, and the base for a future Sova light client on
  NEAR.

**Built**
- **Nothing peg-specific.** No bridge, token or NEAR vault contract.
- The foundation is in place: SIP-4 v1 is code complete on `z1/sip4-v1`
  (in CI, ships at the testnet reset), `IZcash`/`ZcashLib` are on
  `release`, and the buy-an-Ashwing-with-ZEC checkout demo is on
  `release`.

**Blocked on**

| Item | Blocked on |
|---|---|
| Peg testnet | SIP-4 v1.1 `spentBy` (not started); SIP-6 (Rob's §10 calls, then about 3 weeks of work); Rob's SIP-5 §10 calls |
| NEAR learning of Sova burns | NEAR adding Sova to MPC foreign-chain verification, with operators running their own Sova nodes. That is NEAR's decision, and we ask only after the demo |
| The outreach itself | SIP-4 live on the public testnet with the checkout demo running |
| Mainnet wZEC | An external audit (SIP-4, bridge and token, NEAR vault), a clean testnet record, a caps SIP, and a legal read. Not at mainnet launch |

**Next 3 steps**
1. Land SIP-4 v1 and put the ZEC checkout demo on the public testnet.
   That demo is what makes a NEAR conversation possible.
2. Rob decides SIP-5 §10 and SIP-6 §10. Then build SIP-4 v1.1 `spentBy`
   and SIP-6 for the testnet reset.
3. Build the Sova-side bridge and token on the regtest box against a
   clearly labelled local test signer, and prototype the NEAR vault on
   NEAR testnet. Then open the NEAR conversation with the demo in hand.
