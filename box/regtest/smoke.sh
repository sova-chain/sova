#!/usr/bin/env bash
# Smoke test for the Sova Zcash regtest harness.
#
# Starts the stack, waits for zebrad's RPC to come up, mines 5 blocks with
# mine.sh, confirms getblockcount == 5, and tears the stack down. Exits 0 on
# success, non-zero (with logs) on failure.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${HERE}"

RPC_URL="http://127.0.0.1:18232"
EXPECTED_BLOCKS=5
COMPOSE="docker compose"

cleanup() {
  local exit_code=$?
  if [[ "${exit_code}" -ne 0 ]]; then
    echo "--- smoke test failed (exit ${exit_code}); zebrad logs follow ---" >&2
    ${COMPOSE} logs --no-color zebrad >&2 || true
  fi
  echo "--- tearing down stack ---"
  ${COMPOSE} down -v >/dev/null 2>&1 || true
  exit "${exit_code}"
}
trap cleanup EXIT

echo "--- starting stack ---"
${COMPOSE} up -d

echo "--- waiting for zebrad RPC readiness (healthcheck) ---"
deadline=$((SECONDS + 120))
until [[ "$(docker inspect -f '{{.State.Health.Status}}' sova-zebrad-regtest 2>/dev/null)" == "healthy" ]]; do
  if [[ ${SECONDS} -ge ${deadline} ]]; then
    echo "error: zebrad RPC did not become healthy within 120s" >&2
    exit 1
  fi
  sleep 2
done
echo "zebrad RPC is healthy"

echo "--- baseline getblockcount ---"
baseline_response="$(curl -s -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":"smoke","method":"getblockcount","params":[]}' \
  "${RPC_URL}/")"
echo "baseline: ${baseline_response}"
baseline_count="$(echo "${baseline_response}" | jq -r '.result')"

if [[ "${baseline_count}" != "0" ]]; then
  echo "error: expected fresh regtest chain at height 0, got ${baseline_count}" >&2
  exit 1
fi

echo "--- mining ${EXPECTED_BLOCKS} blocks ---"
./mine.sh "${EXPECTED_BLOCKS}" "${RPC_URL}"

echo "--- checking getblockcount ---"
count_response="$(curl -s -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":"smoke","method":"getblockcount","params":[]}' \
  "${RPC_URL}/")"
echo "final: ${count_response}"
block_count="$(echo "${count_response}" | jq -r '.result')"

if [[ "${block_count}" != "${EXPECTED_BLOCKS}" ]]; then
  echo "error: expected getblockcount == ${EXPECTED_BLOCKS}, got ${block_count}" >&2
  exit 1
fi

echo "--- checking getblockchaininfo sanity ---"
info_response="$(curl -s -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":"smoke","method":"getblockchaininfo","params":[]}' \
  "${RPC_URL}/")"
echo "chaininfo: ${info_response}"
# Note: Zebra's getblockchaininfo `chain` field is network.bip70_network_name()
# (zebra-chain/src/parameters/network.rs), which returns "test" for BOTH
# Testnet and Regtest -- it is not a Regtest-specific discriminator. Instead
# we confirm Regtest via NU5 having activated at height 1, which is only
# possible with our [network.testnet_parameters.activation_heights] config,
# combined with the fact that `generate` (mine.sh) succeeded at all: the
# `generate` RPC hard-errors on any network where PoW is not disabled, i.e.
# it only works on Regtest (zebra-rpc/src/methods.rs, `generate` method).
chain_name="$(echo "${info_response}" | jq -r '.result.chain')"
nu5_status="$(echo "${info_response}" | jq -r '.result.upgrades["c2d6d0b4"].status // empty')"

if [[ "${chain_name}" != "test" ]]; then
  echo "error: expected bip70 chain name == test (Zebra's name for Testnet/Regtest), got ${chain_name}" >&2
  exit 1
fi

if [[ "${nu5_status}" != "active" ]]; then
  echo "error: expected NU5 to be active at tip (activation_heights.NU5=1 in zebrad.toml), got status=${nu5_status}" >&2
  exit 1
fi

echo ""
echo "SMOKE TEST PASSED: mined ${EXPECTED_BLOCKS} blocks on Regtest, getblockcount == ${block_count}, chain == ${chain_name}"
exit 0
