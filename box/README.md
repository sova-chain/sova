# sova-in-a-box

One command starts a local burn-to-mine devnet: a Zcash regtest node
(`zebrad`, in Docker), a Sova node and a miner. You watch real ZEC burns
become SOVA on your own machine.

```bash
./box/up.sh           # up
./box/up.sh status    # block height, miner's SOVA balance, settled epochs
./box/up.sh down      # clean teardown
```

From a checkout of a release tag (`git clone --branch v0.1.18
https://github.com/sova-chain/sova`), `./box/up.sh` downloads that
release's prebuilt binaries for Linux x86_64 or Apple Silicon and reaches
the first mint in under a minute. Anywhere else, the first run builds from
source: about 11 minutes on an idle Apple Silicon laptop, about 25 on a
busy one. Later runs reuse the binaries and take under a minute.

[`box/up/README.md`](up/README.md) has the prerequisites, what you're
seeing, the dapp demo, tunables and troubleshooting.
