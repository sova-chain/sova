#!/usr/bin/env bash
# Ashwings art parity: the contract must render byte-for-byte what the
# design lab generator (design/ashwing24.py) renders.
#
#   1. Regenerate the golden files from the generator into a temp dir and
#      diff them against the committed ones in test/fixtures/ashwings24/
#      (catches a generator edit that was never re-exported).
#   2. Run the forge parity test, which reads the committed goldens with
#      vm.readLine (no FFI) and compares svgOf + tokenURI for every seed.
#
# The generator needs pycryptodome (keccak); PARITY_PYTHON picks the
# interpreter, e.g. a venv:  PARITY_PYTHON=/path/to/venv/bin/python
# After an intentional art change, refresh the goldens with
#   python3 design/ashwing24.py --fixtures test/fixtures/ashwings24
set -euo pipefail
cd "$(dirname "$0")/.."
PYBIN="${PARITY_PYTHON:-python3}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

PYTHONDONTWRITEBYTECODE=1 "$PYBIN" design/ashwing24.py --fixtures "$tmp"
if ! diff -rq "$tmp" test/fixtures/ashwings24; then
  echo "golden files are stale: regenerate with design/ashwing24.py --fixtures" >&2
  exit 1
fi
echo "goldens match the generator"
forge test --match-contract AshwingsParityTest -vv
