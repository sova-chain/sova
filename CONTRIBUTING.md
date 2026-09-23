# Contributing to Sova

## Human + AI pair contributions are the default

We expect most changes here to be written by a human and an AI coding
assistant working together, and that's fine — it's the expected default, not
an exception that needs disclosing. What matters is the result: tests pass,
and the author (human) can explain what the change does and why it's
correct. If you can't explain it, it's not ready to submit.

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
burn mechanism, etc.) are governed by Sova Improvement Proposals (SIPs).
Open an SIP before sending a PR that changes protocol behavior. SIPs live
in [`sips/`](sips) in this repository: float the idea first in the SIPs
category of GitHub Discussions, then open the SIP itself as a pull request
adding or changing a file in `sips/`.

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
