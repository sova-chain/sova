#!/usr/bin/env bash
# SIP-7: the ZcashBlocks runtime bytecode the node predeploys at 0x…5A01
# (bin/sova/src/zcash_blocks.runtime.hex) must equal what forge builds from
# contracts/src/zcash/ZcashBlocks.sol. Default: check (exit 1 on drift).
# --write: regenerate the file. A change changes every SIP-7 genesis hash.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
file="${here}/../bin/sova/src/zcash_blocks.runtime.hex"
built="$(cd "${here}" && forge inspect ZcashBlocks deployedBytecode | tr -d '\n')"
if [[ "${1:-}" == "--write" ]]; then
  printf '%s' "${built}" >"${file}"
  echo "wrote ${file}"
  exit 0
fi
if [[ "$(tr -d '\n' <"${file}")" == "${built}" ]]; then
  echo "zcash_blocks.runtime.hex matches forge"
else
  echo "DRIFT: bin/sova/src/zcash_blocks.runtime.hex != forge build of ZcashBlocks (run with --write)" >&2
  exit 1
fi
