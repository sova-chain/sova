#!/usr/bin/env bash
# D2 acceptance test: the `sova-miner` CLI (crates/burn-wallet/miner)
# sustains at least one SIP-1 burn per epoch for 20 consecutive epochs,
# within budget, against this regtest harness -- with blocks driven by
# `auto-mine.sh` calling `generate 1` every few seconds (not by the miner
# itself; the miner only *reacts* to blocks it observes over RPC, per the
# D2 decision that it speaks RPC only and never links reth/zebrad
# internals). `sova-miner report --verify-rpc` then independently re-scans
# the chain for SIP-1 burns (the same way
# crates/burn-wallet/tests/e2e_regtest_burn.rs proves one burn, generalized
# to a full block range -- see crates/burn-wallet/miner/src/verify.rs) and
# confirms it matches the miner's own local report exactly.
#
# Funding: mines exactly ONE block to the miner's own address (giving it a
# single mature coinbase once 100 confirmations pass), then 100 more blocks
# to zebrad.toml's unrelated default mining address -- deliberately NOT to
# the miner's own address again. Funding every block to the same address
# (as the simpler D1 e2e test does, since it only submits one transaction
# total) would leave a long backlog of the miner's *own* still-maturing
# coinbase outputs trickling in throughout this test, which are always
# larger than the miner's own change and would keep getting preferred by
# largest-first coin selection -- masking the UTXO-chaining behavior this
# test exists to demonstrate. With only one coinbase ever paying the miner,
# every epoch after the first *must* spend the previous epoch's own change.
#
# auto-mine.sh runs for the whole script, not just while the miner itself
# is running: it's started once after funding and only stopped by
# cleanup() at exit, so `report --verify-rpc` (which runs after `mine`
# returns) still has a live node to scan.
#
# Always tears the stack down on exit, dumping zebrad logs on failure.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${HERE}"

RPC_URL="${SOVA_MINER_AC_RPC_URL:-http://127.0.0.1:18232}"
BLOCK_INTERVAL_SECONDS="${SOVA_MINER_AC_BLOCK_INTERVAL:-3}"
EPOCHS=20
PER_EPOCH_ZAT=100000
# 20 epochs * (100,000 burn + 20,000 ZIP-317 fee, see miner/src/fee.rs) =
# 2,400,000 zat minimum; comfortable margin above that.
BUDGET_ZAT=3000000
# zebrad.toml's own hard-coded default Regtest miner address (see its
# [mining] section) -- an address entirely unrelated to the miner's own
# keystore, used only as a dumping ground for the 100 non-funding blocks.
THROWAWAY_ADDRESS="tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"

MINER_DIR="${HERE}/../../crates/burn-wallet/miner"
BURN_WALLET_DIR="${HERE}/../../crates/burn-wallet"
COMPOSE="docker compose"
STARTED_STACK=0
AUTO_MINE_PID=""
DATA_DIR=""

cleanup() {
  local exit_code=$?
  if [[ -n "${AUTO_MINE_PID}" ]] && kill -0 "${AUTO_MINE_PID}" 2>/dev/null; then
    echo "--- stopping auto-mine (pid ${AUTO_MINE_PID}) ---"
    kill "${AUTO_MINE_PID}" 2>/dev/null || true
    wait "${AUTO_MINE_PID}" 2>/dev/null || true
  fi
  if [[ "${exit_code}" -ne 0 ]]; then
    echo "--- miner-ac failed (exit ${exit_code}); zebrad logs follow ---" >&2
    ${COMPOSE} logs --no-color zebrad >&2 || true
  fi
  if [[ "${STARTED_STACK}" -eq 1 ]]; then
    echo "--- tearing down stack ---"
    ${COMPOSE} down -v >/dev/null 2>&1 || true
  else
    echo "--- leaving pre-existing stack running (was already up) ---"
  fi
  if [[ -n "${DATA_DIR}" && -d "${DATA_DIR}" ]]; then
    rm -rf "${DATA_DIR}"
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

echo "--- building sova-miner (release) ---"
(cd "${BURN_WALLET_DIR}" && cargo build --release -p sova-miner)
MINER_BIN="${BURN_WALLET_DIR}/target/release/sova-miner"

DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/sova-miner-ac.XXXXXX")"
echo "--- data dir: ${DATA_DIR} ---"

echo "--- sova-miner init ---"
"${MINER_BIN}" --data-dir "${DATA_DIR}" --network regtest init
ADDRESS="$(python3 -c "import json;print(json.load(open('${DATA_DIR}/state.json'))['address'])")"
echo "miner address: ${ADDRESS}"

echo "--- funding: 1 block to the miner, then 100 blocks elsewhere (see header comment) ---"
curl -s -X POST -H 'Content-Type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":\"fund\",\"method\":\"generatetoaddress\",\"params\":[1,\"${ADDRESS}\"]}" \
  "${RPC_URL}/" >/dev/null
curl -s -X POST -H 'Content-Type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":\"fund\",\"method\":\"generatetoaddress\",\"params\":[100,\"${THROWAWAY_ADDRESS}\"]}" \
  "${RPC_URL}/" >/dev/null

TIP_AFTER_FUNDING="$(curl -s -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":"h","method":"getblockcount","params":[]}' "${RPC_URL}/" \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result'])")"
echo "tip after funding: ${TIP_AFTER_FUNDING} (expected 101)"

echo "--- starting auto-mine (1 block every ${BLOCK_INTERVAL_SECONDS}s) in the background ---"
./auto-mine.sh "${BLOCK_INTERVAL_SECONDS}" "${RPC_URL}" >"${DATA_DIR}/auto-mine.log" 2>&1 &
AUTO_MINE_PID=$!

echo "--- running sova-miner mine for ${EPOCHS} epochs (budget ${BUDGET_ZAT} zat, ${PER_EPOCH_ZAT} zat/epoch) ---"
"${MINER_BIN}" --data-dir "${DATA_DIR}" --network regtest mine \
  --budget-zat "${BUDGET_ZAT}" \
  --per-epoch-zat "${PER_EPOCH_ZAT}" \
  --rpc "${RPC_URL}" \
  --poll-interval-ms 300 \
  --max-epochs "${EPOCHS}"
mine_exit_code=$?

if [[ "${mine_exit_code}" -ne 0 ]]; then
  echo "error: sova-miner mine exited ${mine_exit_code}" >&2
  exit "${mine_exit_code}"
fi

# Deliberately NOT stopping auto-mine here: `mine` blocks until its last
# epoch actually confirms (see epoch.rs's wait_for_confirmation), but
# `report --verify-rpc` below still needs a live, block-producing node to
# have anything meaningful to scan -- and keeping the block producer alive
# through verification costs nothing. It's stopped once, uniformly, by
# cleanup() at exit below, whichever way this script ends.

echo ""
echo "=== sova-miner report --verify-rpc ==="
"${MINER_BIN}" --data-dir "${DATA_DIR}" --network regtest report --verify-rpc "${RPC_URL}"
report_exit_code=$?

if [[ "${report_exit_code}" -ne 0 ]]; then
  echo "error: report --verify-rpc did not match on-chain state (exit ${report_exit_code})" >&2
  exit "${report_exit_code}"
fi

EPOCH_COUNT="$(python3 -c "import json;print(len(json.load(open('${DATA_DIR}/state.json'))['epochs']))")"
if [[ "${EPOCH_COUNT}" -ne "${EPOCHS}" ]]; then
  echo "error: expected exactly ${EPOCHS} epochs, got ${EPOCH_COUNT}" >&2
  exit 1
fi

echo ""
echo "MINER AC TEST PASSED: ${EPOCH_COUNT} consecutive epochs, each with a burn, within budget, report matches on-chain state"
exit 0
