#!/usr/bin/env bash
# Mine N blocks on demand against the Sova Zcash regtest harness, using
# Zebra's native `generate` RPC (zebra-rpc/src/methods.rs, `generate` method,
# only enabled when network.disable_pow() is true -- i.e. Regtest).
#
# `generate` builds a real getblocktemplate -> proposal_block_from_template
# -> submitblock cycle internally, so this is not a workaround: it's the
# same code path Zebra's own regtest acceptance tests use.
#
# Usage:
#   ./mine.sh [num_blocks] [rpc_url]
#
# Examples:
#   ./mine.sh          # mine 1 block
#   ./mine.sh 5         # mine 5 blocks
#   ./mine.sh 5 http://127.0.0.1:18232

set -euo pipefail

NUM_BLOCKS="${1:-1}"
RPC_URL="${2:-http://127.0.0.1:18232}"

if ! [[ "${NUM_BLOCKS}" =~ ^[0-9]+$ ]]; then
  echo "error: num_blocks must be a non-negative integer, got: ${NUM_BLOCKS}" >&2
  exit 1
fi

response="$(curl -s -X POST \
  -H 'Content-Type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":\"mine\",\"method\":\"generate\",\"params\":[${NUM_BLOCKS}]}" \
  "${RPC_URL}/")"

if command -v jq >/dev/null 2>&1; then
  if echo "${response}" | jq -e '.error' >/dev/null 2>&1 && [[ "$(echo "${response}" | jq -r '.error')" != "null" ]]; then
    echo "error: generate RPC failed: ${response}" >&2
    exit 1
  fi
  echo "${response}" | jq -r '.result[]' | while read -r hash; do
    echo "mined block: ${hash}"
  done
else
  echo "${response}"
fi
