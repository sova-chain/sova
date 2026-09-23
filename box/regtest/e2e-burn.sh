#!/usr/bin/env bash
# End-to-end proof of the SIP-1 burn-transaction path (D1) against the Sova
# Zcash regtest harness.
#
# Starts the stack (or reuses it if already healthy), then runs
# crates/burn-wallet's `e2e_regtest_burn` integration test, which does the
# actual work: mines 101 blocks to a fresh burn-wallet address, builds and
# signs a real SIP-1 burn transaction, submits it via `sendrawtransaction`
# (the empirical test of Zebra's standardness policy against our OP_RETURN +
# zero-hash-P2PKH outputs), mines a confirming block, fetches the
# transaction back, and asserts the confirmed outputs round-trip through
# `consensus::sip1::extract_burn`. See crates/burn-wallet/tests/e2e_regtest_burn.rs
# for the full assertion list.
#
# Always tears the stack down on exit (success or failure), dumping zebrad
# logs on failure. Exits non-zero on any failure, in which case the test's
# own output (run with --nocapture) contains the verbatim rejection reason
# if `sendrawtransaction` was the failure point.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${HERE}"

BURN_WALLET_DIR="${HERE}/../../crates/burn-wallet"
COMPOSE="docker compose"
STARTED_STACK=0

cleanup() {
  local exit_code=$?
  if [[ "${exit_code}" -ne 0 ]]; then
    echo "--- e2e-burn failed (exit ${exit_code}); zebrad logs follow ---" >&2
    ${COMPOSE} logs --no-color zebrad >&2 || true
  fi
  if [[ "${STARTED_STACK}" -eq 1 ]]; then
    echo "--- tearing down stack ---"
    ${COMPOSE} down -v >/dev/null 2>&1 || true
  else
    echo "--- leaving pre-existing stack running (was already up) ---"
  fi
  exit "${exit_code}"
}
trap cleanup EXIT

echo "--- checking whether the harness is already up ---"
if [[ "$(docker inspect -f '{{.State.Health.Status}}' sova-zebrad-regtest 2>/dev/null)" == "healthy" ]]; then
  echo "harness already healthy, reusing it"
else
  echo "--- starting stack ---"
  ${COMPOSE} up -d
  STARTED_STACK=1

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
fi

echo "--- running e2e_regtest_burn (crates/burn-wallet) ---"
(
  cd "${BURN_WALLET_DIR}" &&
    cargo test --test e2e_regtest_burn -- --ignored --nocapture
)
test_exit_code=$?

if [[ "${test_exit_code}" -ne 0 ]]; then
  echo "error: e2e_regtest_burn failed (exit ${test_exit_code})" >&2
  exit "${test_exit_code}"
fi

echo ""
echo "E2E BURN TEST PASSED"
exit 0
