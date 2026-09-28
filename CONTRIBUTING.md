# Contributing to Sova

## AI-assisted changes

Most changes here are written by a person working with an AI coding
assistant. No disclosure is needed. Tests must pass, and the author must be
able to explain what the change does and why it is correct.

## Consensus code needs simulation-harness coverage

Changes to `crates/consensus` and to the settlement logic in `crates/evm`
affect what the network agrees on, so they get a higher bar: PRs touching
that code merge only with simulation-harness coverage for the new
behavior. The harness is [`box/sim`](box/sim/README.md)
(`box/sim/run-scenarios.sh` runs the scenarios against a real regtest
`zebrad`; CI runs them nightly in `.github/workflows/nightly-sim.yml`). Add
or extend a scenario there, and say in the PR which one covers the change.

## SIPs govern protocol changes

Changes to the protocol itself (consensus rules, transaction formats, the
burn mechanism) go through Sova Improvement Proposals (SIPs), in
[`sips/`](sips). Float the idea in the
[SIPs category of Discussions](https://github.com/sova-chain/sova/discussions/categories/sips),
then open the SIP as a pull request to `sips/` before sending code that
changes protocol behavior.

## Issues and pull requests

Bug reports and feature ideas use the issue templates; protocol ideas go
to Discussions first (above). Vulnerabilities never go in a public issue:
see [`SECURITY.md`](SECURITY.md).

Before opening a PR, run what CI runs, in both workspaces (the root one and
the nested `crates/burn-wallet`):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# and the same three with --manifest-path crates/burn-wallet/Cargo.toml
```

and `shellcheck` any shell script you touch.

## PR titles

Use [Conventional Commits](https://www.conventionalcommits.org/) for PR
titles, e.g. `feat: add burn parser skeleton`, `fix: correct genesis hash`,
`chore: bump toolchain`.
