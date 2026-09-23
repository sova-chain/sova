#!/usr/bin/env bash
# Continuously mines one block every N seconds against the Sova Zcash
# regtest harness, via the same `generate` RPC `mine.sh` uses -- this is
# just `mine.sh 1` in a loop, for driving a *sustained* chain (e.g. for
# D2's miner acceptance test, which needs one new block roughly every few
# seconds for many consecutive epochs) rather than mining a fixed batch
# on demand.
#
# Runs until killed (Ctrl+C, or `kill` from a driving script); exits
# cleanly on SIGINT/SIGTERM rather than mid-request.
#
# Usage:
#   ./auto-mine.sh [interval_seconds] [rpc_url]
#
# Examples:
#   ./auto-mine.sh                          # 1 block every 3s
#   ./auto-mine.sh 5                        # 1 block every 5s
#   ./auto-mine.sh 3 http://127.0.0.1:18232

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

INTERVAL_SECONDS="${1:-3}"
RPC_URL="${2:-http://127.0.0.1:18232}"

if ! [[ "${INTERVAL_SECONDS}" =~ ^[0-9]+$ ]]; then
  echo "error: interval_seconds must be a non-negative integer, got: ${INTERVAL_SECONDS}" >&2
  exit 1
fi

STOP=0
trap 'STOP=1' SIGINT SIGTERM

echo "auto-mine: generating 1 block every ${INTERVAL_SECONDS}s against ${RPC_URL} (Ctrl+C to stop)"

while [[ "${STOP}" -eq 0 ]]; do
  "${HERE}/mine.sh" 1 "${RPC_URL}" || echo "warning: mine.sh failed this round" >&2
  sleep "${INTERVAL_SECONDS}" &
  wait $! 2>/dev/null
done

echo "auto-mine: stopped"
