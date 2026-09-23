# sova-in-a-box

One command that brings up a `zebrad` regtest node, a Sova node (mine
mode), and a continuous miner together, so anyone can run a full local
burn-to-mine devnet and watch it mine.

```bash
./box/up.sh          # up
./box/up.sh status    # block height, miner's SOVA balance, settled epochs
./box/up.sh down      # clean teardown
```

See [`box/up/README.md`](up/README.md) for the full quickstart, what
you're seeing, tunables, and evidence from a real cold run.

**How long the first run takes.** With prebuilt binaries, `./box/up.sh`
reaches the first mint in under a minute (35-42s measured). Those come from
a checkout of a tagged release, which downloads that release's CI-built
binaries with plain `curl` and no GitHub login (`box/up/README.md`,
"Prebuilt binaries"). The first tagged release is not out yet. Until it
is, the first run builds from source: about 11 minutes on an idle Apple
Silicon laptop, and about 25 on a busy one. Later runs reuse the binaries
and take under a minute.

v1 is a **hybrid** stack: `zebrad` runs in Docker (reusing
[`box/regtest`](regtest); only its port and container name became
overridable), while `bin/sova` and `sova-miner` run
as host processes in release mode -- see `box/up/README.md`'s "Why hybrid"
section for the disk/host-shape reasoning. Full containerization (a
Dockerfile + compose stack for all three) is a tracked follow-up. E1's
acceptance bar is "one command, cold to a visibly mining chain in under 10
minutes". It is not met yet. Locally, with the binaries already present,
`./box/up.sh` reaches the first mint in under a minute, but a stranger
has to build from source until the first tagged release ships prebuilt
binaries.
