#!/usr/bin/env bash
# infra-1's one-shot: relay ONE SIP-1 burn through real public Zcash
# testnet peers, watch it get mined, then freeze SIP-1.
#
# Prereqs (see docs/WORKPLAN.md infra-1):
#   - a SYNCED testnet zebrad with RPC on 127.0.0.1:18234 (the
#     SSD-hosted native node; cookie auth off)
#   - a funded testnet miner identity at ~/.sova-testnet-miner
#     (t-addr printed by `sova-miner --network testnet init`; funding
#     comes from the internal-miner coinbase plan or any TAZ source —
#     coinbases need 100 confirmations, ordinary transfers 1. sova-miner
#     finds either with getaddressutxos, no chain scan)
#
# What it proves: the SIP-1 tx shape (OP_RETURN payload + eater output,
# ZIP-317 fee) passes REAL network relay policy on nodes we don't run,
# and gets mined by miners we don't know. Regtest proved the rules are
# network-unconditional in Zebra's source; this is the empirical cap.
set -euo pipefail

RPC="${SOVA_TESTNET_RPC:-http://127.0.0.1:18234}"
MINER_BIN="${MINER_BIN:-$HOME/Documents/GitHub/sova-chain/crates/burn-wallet/target/release/sova-miner}"
DATA_DIR="${SOVA_TESTNET_MINER_DIR:-$HOME/.sova-testnet-miner}"

rpc() {
  curl -s --max-time 20 -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"sip1\",\"method\":\"$1\",\"params\":$2}" "${RPC}/"
}

echo "--- sync check ---"
INFO="$(rpc getblockchaininfo '[]')"
BLOCKS=$(echo "$INFO" | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['blocks'])")
EST=$(echo "$INFO" | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['estimatedheight'])")
echo "height ${BLOCKS} / estimated ${EST}"
if (( EST - BLOCKS > 10 )); then
  echo "not synced (lag $((EST-BLOCKS))); refusing to run the check early" >&2
  exit 1
fi

echo "--- one burn, tiny budget, through the real mempool ---"
"${MINER_BIN}" --data-dir "${DATA_DIR}" --network testnet mine \
  --budget-zat 60000 --per-epoch-zat 10000 \
  --rpc "${RPC}" --max-epochs 1

echo ""
echo "--- verification: burn recognized from OUR node's view ---"
"${MINER_BIN}" --data-dir "${DATA_DIR}" --network testnet report --verify-rpc "${RPC}"

echo ""
echo "SIP-1 RELAY CHECK COMPLETE."
echo "Next (manual, deliberate): the freeze commit —"
echo "  1. sips/sip-1.md: status Draft -> Frozen, cite this run's txid+height"
echo "  2. docs/WORKPLAN.md: infra-1 -> done"
