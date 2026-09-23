#!/usr/bin/env bash
# Day-one dapp kit (G1/G2) into a running Sova node: WSOVA, UniV2
# factory+router (feeToSetter = zero address, forever), Multicall3, and
# the Ashwings mint — then the live demo loop: launch a token, seed a
# native-SOVA pool, swap, mint an owl.
#
# Usage: box/deploy-dapps.sh [rpc-url]
#   RPC defaults to the box/dev node at http://127.0.0.1:8545.
#   SOVA_DEPLOYER_KEY overrides the key; the default is reth's dev
#   account #0 (prefunded on the DEV chainspec bin/sova launches with).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACTS="${HERE}/../contracts"
RPC="${1:-http://127.0.0.1:8545}"
KEY="${SOVA_DEPLOYER_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"

cd "${CONTRACTS}"
# Forge warns "EIP-3855 is not supported ... Chain IDs: 1337" from the
# chain ID alone (it does the same against anvil --chain-id 1337); the
# Sova node runs PUSH0. Its "ETH" amounts are SOVA here. Say so up front.
echo "note: forge's 'EIP-3855 is not supported' warning for chain ID 1337 is harmless here"
echo "      (forge keys it on the chain ID; the Sova node runs PUSH0), and its 'ETH' amounts are SOVA."
echo "--- deploying day-one kit to ${RPC} ---"
forge script script/Deploy.s.sol --tc Deploy --rpc-url "${RPC}" --private-key "${KEY}" --broadcast
echo ""
echo "--- day-one demo: launch token, seed pool, swap, mint an owl ---"
forge script script/Demo.s.sol --tc Demo --rpc-url "${RPC}" --private-key "${KEY}" --broadcast
echo ""
echo "addresses in ${CONTRACTS}/deployments.json"
