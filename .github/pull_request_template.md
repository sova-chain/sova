<!--
PR title: Conventional Commits, e.g. `feat: add burn parser skeleton`,
`fix: correct genesis hash`, `chore: bump toolchain` (see CONTRIBUTING.md).
Security fixes do not go through a public PR first: see SECURITY.md.
-->

## What and why

<!-- What this changes and why it is correct. Human + AI pairing is the
default here; what matters is that you can explain the change. -->

## How it was tested

<!-- Commands you ran and what they showed. -->

## Checklist

- [ ] CI's checks pass locally in both workspaces (root and `crates/burn-wallet`): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- [ ] Shell scripts touched are `shellcheck`-clean
- [ ] **Consensus or settlement change** (`crates/consensus`, settlement in `crates/evm`, or anything else that changes what nodes agree on)? Then:
  - [ ] a `box/sim` scenario covers the new behavior: <!-- which one -->
  - [ ] it implements an agreed SIP: <!-- link to the sips/ file or SIP PR -->
- [ ] Not a consensus change
- [ ] Docs updated where behavior changed (READMEs, `box/up/README.md`, SIPs)
