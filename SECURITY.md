# Security Policy

Sova is pre-release software under active construction. It has not been
audited. There is no mainnet; the only networks are local devnets
(`./box/up.sh`) and, from M1, a public testnet whose coins have no value.

## Reporting a vulnerability

**Do not open a public issue, discussion or pull request for a
vulnerability.** Report it privately through GitHub private vulnerability
reporting: <https://github.com/sova-chain/sova/security/advisories/new>.

Please include: what is affected (crate, file, commit or release tag), how
to reproduce it (a failing test, a `box/sim` scenario or a script is
ideal), what an attacker gains, and whether it is already public anywhere.

## What happens next

1. **Acknowledgement** within **3** business days.
2. **Assessment**: we confirm or rule out the issue, agree a severity with
   you, and keep you updated at least every **7** days.
3. **Fix**: developed privately (in a GitHub security advisory's private
   fork), with a regression test. Protocol-level fixes that change what
   nodes agree on also get a SIP, published with or after the fix.
4. **Disclosure**: coordinated with you. We aim to publish the fix and an
   advisory within **90** days of the report, sooner once a
   fix ships, and credit you unless you ask us not to.

There is no bug bounty at this stage.

## Scope

In scope -- this repository's code and the artifacts built from it:

- **Consensus**: the burn-to-mine rules and everything that decides what
  nodes accept -- Zcash follower and burn parsing (SIP-1), epoch ranking
  and sealing, block validation and re-derivation of every mint
  (`crates/consensus`, `crates/engine`, `bin/sova`), and the `sova/1` P2P
  protocol. For example: a block other nodes accept that they should not,
  a mint without a matching burn, a way to split honest nodes, or a remote
  crash.
- **Settlement and emission**: how settled epochs become SOVA -- the
  withdrawals-channel mint, reward splits and tips, the emission schedule
  (SIP-2, SIP-3; `crates/evm`, `crates/chainspec`).
- **Miner and faucet keys**: anything that leaks, weakens or misuses a
  key -- the `sova-miner` keystore and burn-transaction builder
  (`crates/burn-wallet`, `crates/miner`, `bin/sova-miner`, `mcp/`), and
  the TAZ faucet's hot key (`crates/burn-wallet/faucet`; see its
  testnet-only guards in `docs/ops/faucet.md`). Also a burn that is not
  provably unspendable, or one that credits the wrong address.
- **Release binaries and the box**: the prebuilt-binary verification in
  `box/up.sh` (checksums, platform and source-commit checks) and the
  release workflow (`.github/workflows/box-binaries.yml`).
- **The day-one contracts** in `contracts/src` (the vendored Uniswap V2
  code as modified here, WSOVA, Ashwings).

Out of scope:

- Bugs in third-party dependencies (reth, zebrad, Foundry, ...) that are
  not specific to how Sova uses them: please report those upstream.
- Test material that is public by design: the Foundry/anvil dev key
  (`0xac0974...`) and the "test test ... junk" mnemonic, regtest and
  testnet miner addresses, and the prefunded dev accounts of the local
  box (a dev-mode chain).
- Denial of service that needs only volume against a local devnet, the
  website's content, and social engineering of contributors.

If you are unsure whether something is in scope, report it privately
anyway.
