#!/usr/bin/env bash
# Day-one dapp kit (G1/G2) into a running Sova node: WSOVA, UniV2
# factory+router (feeToSetter = zero address, forever), Multicall3, the
# Ashwings mint (10,000 owls, SOVA or ZEC; it creates its ZEC checkout) and
# AshwingsMarket -- then the live demo loop: launch a token, seed a
# native-SOVA pool, swap, mint an owl for SOVA, list it, a second account
# buys it (1% fee), mint income and fees go to the treasury; and, where
# the node answers SIP-4, reserve a ZEC order.
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
# ASHWINGS_PRICE_ZAT and MARKET_FEE_BPS. On the box the treasury defaults
# to Rob's treasury address, and the prices and ZEC payee to PLACEHOLDER
# demo values (10 SOVA, 0.25 ZEC, the box's regtest miner t-address, a
# public dev key: never a real payee). SOVA_DEMO_BUYER_KEY is the demo's
# second account (default dev #1; the deployer funds it).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACTS="$(cd "${HERE}/../contracts" && pwd)"
RPC="${1:-http://127.0.0.1:8545}"
# Well-known dev key (anvil/reth account #0): public, box only.
export SOVA_DEPLOYER_KEY="${SOVA_DEPLOYER_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"
export ASHWINGS_TREASURY="${ASHWINGS_TREASURY:-0x8d0123637062f8c15FD6AFDe6F834C85f4AfeEDE}"
export ASHWINGS_ZEC_PAYEE="${ASHWINGS_ZEC_PAYEE:-tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV}" # PLACEHOLDER
export ASHWINGS_PRICE_WEI="${ASHWINGS_PRICE_WEI:-10000000000000000000}"               # PLACEHOLDER 10 SOVA
export ASHWINGS_PRICE_ZAT="${ASHWINGS_PRICE_ZAT:-25000000}"                           # PLACEHOLDER 0.25 ZEC
export MARKET_FEE_BPS="${MARKET_FEE_BPS:-100}"
export SOVA_DEMO_BUYER_KEY="${SOVA_DEMO_BUYER_KEY:-0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d}"
ZCASH=0x0000000000000000000000000000000000005a00

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
echo "--- day-one demo: token, pool, swap; mint an owl for SOVA, list it, sell it (1% fee) ---"
cd "${CONTRACTS}"
# forge estimates gas by simulating every step in one block; on the node
# the pool's first swap lands a block after addLiquidity, and the pair then
# writes its price accumulators (two fresh slots, ~40k gas), which ran the
# swap out of gas at forge's default 130% margin. 200% covers it.
forge script script/Demo.s.sol --tc Demo --rpc-url "${RPC}" --private-key "${SOVA_DEPLOYER_KEY}" --broadcast \
  --gas-estimate-multiplier 200
echo ""

ASHW="$(jq -r .ashwings deployments.json)"
MARKET="$(jq -r .market deployments.json)"
CO="$(cast call "${ASHW}" 'zecCheckout()(address)' --rpc-url "${RPC}")"

# The ZEC path reads Zcash through the SIP-4 precompile. forge simulates
# scripts in its own EVM, which lacks it, so this step talks to the node
# with cast. It only reserves: paying is a real Zcash transaction.
echo "--- ZEC path: reserve an order (needs SIP-4 at 0x…5a00) ---"
if ANCHOR="$(cast call "${ZCASH}" 'anchor()(uint64,bytes32)' --rpc-url "${RPC}" 2>/dev/null | head -1)" && [ -n "${ANCHOR}" ]; then
  BUYER="$(cast wallet address --private-key "${SOVA_DEMO_BUYER_KEY}")"
  # Explicit gas: cast estimates against the pending block, whose Zcash
  # anchor is not indexed until the next Zcash block, and the node then
  # refuses any SIP-4 call. (Estimating at "latest" works: ~154k.)
  if ! cast send "${CO}" 'reserve(uint256,address)' 1 "${BUYER}" --gas-limit 300000 \
    --rpc-url "${RPC}" --private-key "${SOVA_DEPLOYER_KEY}" >/dev/null; then
    echo "reserve failed (see above): ZEC path not demonstrated on this node" >&2
    exit 1
  fi
  RID="$(cast call "${CO}" 'reservationCount()(uint256)' --rpc-url "${RPC}")"
  QUOTE="$(cast call "${CO}" 'quote(uint256)(uint64)' "${RID}" --rpc-url "${RPC}" | awk '{print $1}')"
  DEADLINE="$(cast call "${CO}" 'deadline(uint256)(uint64)' "${RID}" --rpc-url "${RPC}" | awk '{print $1}')"
  ZEC="$(awk -v z="${QUOTE}" 'BEGIN { printf "%d.%08d", int(z / 100000000), z % 100000000 }')"
  echo "zcash anchor ${ANCHOR}: order #${RID} for ${BUYER}"
  echo "pay exactly ${ZEC} ZEC to ${ASHWINGS_ZEC_PAYEE}, mined by zcash block ${DEADLINE}:"
  echo "  zcash:${ASHWINGS_ZEC_PAYEE}?amount=${ZEC}"
  echo "then: cast send ${CO} 'claim(uint256,bytes32,uint32)' ${RID} 0x<txid> <vout> --gas-limit 400000  (or /ashwings/buy)"
else
  echo "SIP-4 not answering on ${RPC}: ZEC path skipped (the SOVA path and market are live)."
fi
echo ""
echo "supply: $(cast call "${ASHW}" 'totalSupply()(uint256)' --rpc-url "${RPC}") / 10000 minted"
echo "pages (npm run dev in site/, or any build; the RPC must allow the page's origin, CORS):"
echo "  /ashwings/mint?rpc=${RPC}&ashw=${ASHW}&market=${MARKET}"
echo "  /ashwings/market?rpc=${RPC}&ashw=${ASHW}&market=${MARKET}"
echo "  /ashwings/buy?rpc=${RPC}&co=${CO}&relayer=none"
echo "addresses in ${CONTRACTS}/deployments.json"
