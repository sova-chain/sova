# Vendored third-party contracts

Everything under `contracts/src/vendor/` is third-party code copied into
this repo so the day-one dapp kit builds with Foundry alone (no npm, no git
submodules). It keeps its **original licenses**, which are not the
MIT / Apache-2.0 terms of the rest of the repository. See the top-level
[`NOTICE`](../../../NOTICE).

| Path | Upstream | License | Changed here? |
| --- | --- | --- | --- |
| `v2-core/` | [Uniswap/v2-core](https://github.com/Uniswap/v2-core) `contracts/` | GPL-3.0 ([`v2-core/LICENSE`](v2-core/LICENSE)) | No |
| `v2-periphery/` | [Uniswap/v2-periphery](https://github.com/Uniswap/v2-periphery) `contracts/` (router, its libraries and interfaces) | GPL-3.0 ([`v2-periphery/LICENSE`](v2-periphery/LICENSE)) | **Yes, one line** (below) |
| `uniswap-lib/` | [Uniswap/solidity-lib](https://github.com/Uniswap/solidity-lib) `contracts/libraries/TransferHelper.sol` | GPL-3.0-or-later (SPDX header; [`uniswap-lib/LICENSE`](uniswap-lib/LICENSE)) | No |
| `Multicall3.sol` | [mds1/multicall3](https://github.com/mds1/multicall3) `src/Multicall3.sol` | MIT (SPDX header in the file) | No |

The three `LICENSE` files are the upstream repositories' own `LICENSE`
files, unchanged: the GNU General Public License, version 3 (SHA-256
`3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986`).

## Modifications

Compared file by file against upstream `master` on 2026-09-23 (v2-core
`6a9e7c9`, v2-periphery `ed24991`, solidity-lib `c01640b`, multicall3
`main`): every vendored file is byte-identical to upstream except one line.

- `v2-periphery/libraries/UniswapV2Library.sol`, `pairFor`: the
  `UniswapV2Pair` init code hash is replaced with the hash of the pair
  bytecode this repo compiles
  (`0x763a4a238e517c36b1ecd875f9da6eb82f3d8f91463be5fc00d4e699f33e294f`,
  recomputed with `contracts/script/InitHash.s.sol`), so the router
  derives the addresses of pairs created by this repo's factory. Upstream
  has `0x96e8ac42...845f`. Modified 2026-09-22 by the Sova contributors.

Import paths are unchanged; `contracts/foundry.toml` remaps
`@uniswap/v2-core/contracts/` and `@uniswap/lib/contracts/` to these
directories.

## What is built from it

`contracts/script/Deploy.s.sol` (run by `box/deploy-dapps.sh`) deploys
`UniswapV2Factory` (which creates `UniswapV2Pair` contracts) and
`UniswapV2Router02` from this code, by artifact name, as separate
contracts. Sova's own contracts (`WSOVA.sol`, `Ashwings.sol`, the scripts)
are MIT and do not import the GPL files; the node and miner (Rust) contain
no Uniswap code. The deployed factory, pair and router bytecode is compiled
from GPL-3.0 source.
