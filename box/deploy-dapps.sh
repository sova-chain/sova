#!/usr/bin/env bash
# Day-one dapp kit (G1/G2) into a running Sova node: WSOVA, UniV2
# factory+router (feeToSetter = zero address, forever), Multicall3, the
# Ashwings mint (and AshwingsMarket once it exists) -- then the live demo
# loop: launch a token, seed a native-SOVA pool, swap, mint an owl.
#
# Usage: box/deploy-dapps.sh [rpc-url]
#   RPC defaults to the box/dev node at http://127.0.0.1:8545.
#   SOVA_DEPLOYER_KEY overrides the key; the default is reth's dev
#   account #0 (prefunded on the DEV chainspec bin/sova launches with).
#
# The deploy itself is contracts/script/deploy-kit.sh, the same code the
# public testnet uses (infra/testnet/deploy-contracts.sh). It is
# idempotent: a second run on the same chain deploys nothing. Addresses go
# to contracts/deployments.json. Ashwings/market constructor values come
# from ASHWINGS_TREASURY, ASHWINGS_ZEC_PAYEE, ASHWINGS_PRICE_WEI,
# ASHWINGS_PRICE_ZAT and MARKET_FEE_BPS when the contracts take them; on
# the box the treasury defaults to the deployer (a demo chain, not money).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACTS="$(cd "${HERE}/../contracts" && pwd)"
RPC="${1:-http://127.0.0.1:8545}"
# Well-known dev key (anvil/reth account #0): public, box only.
export SOVA_DEPLOYER_KEY="${SOVA_DEPLOYER_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"
ASHWINGS_TREASURY="${ASHWINGS_TREASURY:-$(cast wallet address --private-key "${SOVA_DEPLOYER_KEY}")}"
export ASHWINGS_TREASURY

# Forge warns "EIP-3855 is not supported ... Chain IDs: 1337" from the
# chain ID alone (it does the same against anvil --chain-id 1337); the
# Sova node runs PUSH0. Its "ETH" amounts are SOVA here. Say so up front.
echo "note: forge's 'EIP-3855 is not supported' warning for chain ID 1337 is harmless here"
echo "      (forge keys it on the chain ID; the Sova node runs PUSH0), and its 'ETH' amounts are SOVA."
echo "--- deploying day-one kit to ${RPC} ---"
# --reset-stale: every `box/up.sh` is a fresh chain, so addresses recorded
# by an earlier box have no code; they are redeployed, not an error.
"${CONTRACTS}/script/deploy-kit.sh" deploy --rpc "${RPC}" --out "${CONTRACTS}/deployments.json" \
  --private-key-env SOVA_DEPLOYER_KEY --reset-stale
echo ""
echo "--- day-one demo: launch token, seed pool, swap, mint an owl ---"
cd "${CONTRACTS}"
forge script script/Demo.s.sol --tc Demo --rpc-url "${RPC}" --private-key "${SOVA_DEPLOYER_KEY}" --broadcast
echo ""
echo "addresses in ${CONTRACTS}/deployments.json"
