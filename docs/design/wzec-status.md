# Wrapped ZEC: status (2026-09-23)

**Short answer:** wrapped ZEC is a **Sova Labs product, wZcash at wz.cash**,
not part of the Sova protocol (SIP-5 withdrawn). It is openly custodial:
NEAR Chain Signatures hold the ZEC, a Sova Labs relayer tells the NEAR vault
about burns on Sova, and Sova Labs takes a small operating fee. The network
makes no guarantees about it; sova.io will link it from an ecosystem page.
Design: `docs/design/wz-cash.md`. No product code exists yet.

**Depends on:** SIP-4 v1 (landing), SIP-4 v1.1 `spentBy` (vault monitoring
from Zcash), SIP-6 (accepted; censorship-resistance for the product's own
transactions), a NEAR vault contract, the Sova Labs relayer, and the
wz.cash wrap/unwrap interface.

**Next:** land SIP-4 and the public testnet; build SIP-6 and SIP-7; then the
wz.cash testnet product (bridge + token on Sova, NEAR vault on NEAR testnet,
relayer, the site at wz.cash), capped as drafted.
