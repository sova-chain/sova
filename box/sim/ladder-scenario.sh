#!/usr/bin/env bash
# v2 preference: the ladder + late-win proof (C3's acceptance scenario).
#
# Two mine-mode `bin/sova` nodes (A and B), each with its own miner
# identity, relaying to each other (mutual SOVA_PEERS, shared JWT), both
# enforcing C5 against the same regtest zebrad, with a short ladder step
# (SOVA_RANK_STEP_SECS=3).
#
# The script then constructs one *combined* burn epoch deterministically
# (auto-mine stopped; both miners' burns land in a single generated
# block; A burns more, so A is rank 0 and B is rank 1) — with node A
# SIGSTOPped:
#
#   (1) ladder     -- B (rank 1) seals the epoch after its 1*step rung,
#                     with B's own derivation (tip to B), because rank 0
#                     is silent. Liveness without the top burner.
#   (2) late win   -- node A resumes (SIGCONT), seals its own rank-0
#                     block for the same height, and relays it; B's
#                     arbiter adopts it: B's hash at that height CHANGES
#                     (micro-reorg observed) to A's block.
#   (3) converge   -- afterwards A and B agree on every checked height,
#                     the miners' EVM balances are identical across
#                     nodes, and the chain keeps advancing in lockstep
#                     once auto-mine resumes.
#
# Also live-tests burn-less convergence between two concurrent
# producers (both nodes seal empty cadence epochs during a warm-up
# window; hash-tiebreak arbitration must keep them in agreement).
#
# Environment courtesy + teardown conventions match
# box/sim/two-node-scenario.sh.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../.." && pwd)"
REGTEST_DIR="${ROOT}/box/regtest"
BURN_WALLET_DIR="${ROOT}/crates/burn-wallet"
COMPOSE="docker compose"

ZEBRAD_RPC="${SOVA_REGTEST_RPC:-http://127.0.0.1:18232}"

A_HTTP_PORT=8545
A_AUTH_PORT=8551
ENGINE_RPC_A="http://127.0.0.1:${A_HTTP_PORT}"
AUTHRPC_A="http://127.0.0.1:${A_AUTH_PORT}"

B_HTTP_PORT=8645
B_AUTH_PORT=8651
B_P2P_PORT=30313
ENGINE_RPC_B="http://127.0.0.1:${B_HTTP_PORT}"
AUTHRPC_B="http://127.0.0.1:${B_AUTH_PORT}"

SOVA_BIN="${ROOT}/target/debug/sova"
MINER_BIN="${BURN_WALLET_DIR}/target/release/sova-miner"

RANK_STEP_S=3
FUND_BLOCKS=101
AUTO_MINE_INTERVAL_S=2
HARNESS_WAIT_TIMEOUT_S=900

STARTED_STACK=0
A_PID=""
B_PID=""
AUTO_MINE_PID=""
WORK_DIR=""
FAILURES=0

pass() { echo "PASS: ladder: $*" >&2; }
fail() {
  echo "FAIL: ladder: $*" >&2
  FAILURES=$((FAILURES + 1))
}

cleanup() {
  local exit_code=$?
  if [[ -n "${A_PID}" ]]; then
    kill -CONT "${A_PID}" 2>/dev/null || true
  fi
  if [[ -n "${AUTO_MINE_PID}" ]] && kill -0 "${AUTO_MINE_PID}" 2>/dev/null; then
    kill "${AUTO_MINE_PID}" 2>/dev/null || true
    wait "${AUTO_MINE_PID}" 2>/dev/null || true
  fi
  for pid in "${A_PID}" "${B_PID}"; do
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      kill "${pid}" 2>/dev/null || true
      wait "${pid}" 2>/dev/null || true
    fi
  done
  pkill -f "target/debug/sova$" 2>/dev/null || true
  if [[ -n "${WORK_DIR}" ]]; then
    pkill -f "sova-miner --data-dir ${WORK_DIR}" 2>/dev/null || true
  fi

  if [[ "${exit_code}" -ne 0 || "${FAILURES}" -gt 0 ]]; then
    echo "--- ladder scenario failed (exit ${exit_code}, ${FAILURES} assertion failure(s)); logs follow ---" >&2
    for log in node-a.log node-b.log; do
      if [[ -n "${WORK_DIR}" && -f "${WORK_DIR}/${log}" ]]; then
        echo "--- ${log} (last 60 lines) ---" >&2
        tail -n 60 "${WORK_DIR}/${log}" >&2 || true
      fi
    done
  fi

  if [[ "${STARTED_STACK}" -eq 1 ]]; then
    (cd "${REGTEST_DIR}" && ${COMPOSE} down -v) >/dev/null 2>&1 || true
  fi
  if [[ -n "${WORK_DIR}" && -d "${WORK_DIR}" ]]; then
    if [[ -n "${SOVA_SIM_KEEP_LOGS:-}" ]]; then
      mkdir -p "${SOVA_SIM_KEEP_LOGS}"
      cp "${WORK_DIR}"/*.log "${SOVA_SIM_KEEP_LOGS}/" 2>/dev/null || true
    fi
    rm -rf "${WORK_DIR}"
  fi
  if [[ "${exit_code}" -eq 0 && "${FAILURES}" -gt 0 ]]; then
    exit 1
  fi
  exit "${exit_code}"
}
trap cleanup EXIT

zc_rpc() {
  local method="$1" params="$2"
  curl -s -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"ladder\",\"method\":\"${method}\",\"params\":${params}}" \
    "${ZEBRAD_RPC}/"
}

zc_tip_height() {
  zc_rpc getblockcount "[]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'])"
}

zc_generate_to_address() {
  local n="$1" addr="$2"
  zc_rpc generatetoaddress "[${n}, \"${addr}\"]" >/dev/null
}

# txids (space-separated) of non-coinbase txs in the block at HEIGHT.
zc_block_txids() {
  local height="$1"
  local hash
  hash="$(zc_rpc getblockhash "[${height}]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'])")"
  zc_rpc getblock "[\"${hash}\", 1]" | python3 -c "
import sys, json
txs = json.load(sys.stdin)['result']['tx']
print(' '.join(txs[1:]))
"
}

eth_rpc() {
  local url="$1" method="$2" params="$3"
  curl -s -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"ladder\",\"method\":\"${method}\",\"params\":${params}}" \
    "${url}"
}

eth_block_number() {
  eth_rpc "$1" eth_blockNumber "[]" | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))"
}

eth_balance_wei() {
  eth_rpc "$1" eth_getBalance "[\"$2\",\"latest\"]" \
    | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))"
}

eth_block_hash() {
  local url="$1" height="$2" hex_height
  hex_height="$(printf '0x%x' "${height}")"
  eth_rpc "${url}" eth_getBlockByNumber "[\"${hex_height}\", false]" \
    | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin).get('result')
    print(d['hash'] if d else '')
except Exception:
    print('')
"
}

wait_for_block_number() {
  local url="$1" target="$2" timeout_s="${3:-60}"
  local deadline=$((SECONDS + timeout_s)) cur
  while true; do
    cur="$(eth_block_number "${url}" 2>/dev/null || echo 0)"
    if [[ "${cur}" -ge "${target}" ]]; then
      return 0
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      return 1
    fi
    sleep 1
  done
}

wait_for_eth_rpc() {
  local url="$1" pid="$2" label="$3" timeout_s="${4:-60}"
  local deadline=$((SECONDS + timeout_s)) ready_resp
  # Response into a variable, not `eth_rpc | grep -q`: under pipefail a
  # match can SIGPIPE curl and read as "not ready".
  until ready_resp="$(eth_rpc "${url}" eth_chainId "[]")" && grep -q result <<<"${ready_resp}"; do
    if ! kill -0 "${pid}" 2>/dev/null; then
      fail "setup: ${label} exited before becoming ready"
      return 1
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      fail "setup: ${label} RPC did not become ready within ${timeout_s}s"
      return 1
    fi
    sleep 2
  done
  return 0
}

harness_busy() {
  local docker_names
  # Not `docker ps | grep -qx`: under pipefail a match can SIGPIPE docker
  # and a busy harness would read as idle.
  docker_names="$(docker ps --format '{{.Names}}' 2>/dev/null)" \
    && grep -qx "sova-zebrad-regtest" <<<"${docker_names}" && return 0
  pgrep -f "target/(debug|release)/sova(\$| )" >/dev/null 2>&1 && return 0
  pgrep -f "sova-miner .*(init|mine)" >/dev/null 2>&1 && return 0
  pgrep -f "auto-mine\.sh" >/dev/null 2>&1 && return 0
  return 1
}

wait_for_harness_free() {
  local deadline=$((SECONDS + HARNESS_WAIT_TIMEOUT_S))
  if ! harness_busy; then
    echo "--- harness free ---"
    return 0
  fi
  echo "--- harness busy; waiting up to $((HARNESS_WAIT_TIMEOUT_S / 60)) minutes rather than killing another agent's run ---"
  while harness_busy; do
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "error: harness still busy after $((HARNESS_WAIT_TIMEOUT_S / 60)) minutes; giving up" >&2
      exit 1
    fi
    sleep 15
  done
  echo "--- harness now free ---"
}

wait_for_harness_free

if [[ ! -x "${SOVA_BIN}" ]]; then
  echo "--- building bin/sova (debug) ---"
  (cd "${ROOT}" && cargo build -p sova --quiet)
fi
if [[ ! -x "${MINER_BIN}" ]]; then
  echo "--- building sova-miner (release) ---"
  (cd "${BURN_WALLET_DIR}" && cargo build --release -p sova-miner --quiet)
fi

echo "--- starting a fresh regtest stack ---"
(cd "${REGTEST_DIR}" && ${COMPOSE} up -d)
STARTED_STACK=1

deadline=$((SECONDS + 120))
until [[ "$(docker inspect -f '{{.State.Health.Status}}' sova-zebrad-regtest 2>/dev/null)" == "healthy" ]]; do
  if [[ ${SECONDS} -ge ${deadline} ]]; then
    echo "error: zebrad RPC did not become healthy within 120s" >&2
    exit 1
  fi
  sleep 2
done
echo "zebrad RPC is healthy"

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/sova-ladder.XXXXXX")"
echo "--- work dir: ${WORK_DIR} ---"

JWT_PATH="${WORK_DIR}/jwt.hex"
openssl rand -hex 32 >"${JWT_PATH}"

# ---------------------------------------------------------------------
# Two miner identities; fund both BEFORE the nodes start, so the nodes'
# epoch base begins after the funding noise.
# ---------------------------------------------------------------------

for who in a b; do
  mkdir -p "${WORK_DIR}/miner-${who}"
  "${MINER_BIN}" --data-dir "${WORK_DIR}/miner-${who}" --network regtest init \
    >"${WORK_DIR}/miner-${who}-init.log" 2>&1
done
TADDR_A="$(awk '/t-addr to fund/ {print $NF}' "${WORK_DIR}/miner-a-init.log")"
EVM_A="$(awk '/evm address/ {print $NF}' "${WORK_DIR}/miner-a-init.log")"
TADDR_B="$(awk '/t-addr to fund/ {print $NF}' "${WORK_DIR}/miner-b-init.log")"
EVM_B="$(awk '/evm address/ {print $NF}' "${WORK_DIR}/miner-b-init.log")"
if [[ -z "${TADDR_A}" || -z "${EVM_A}" || -z "${TADDR_B}" || -z "${EVM_B}" ]]; then
  fail "setup: could not parse miner identities"
  exit 1
fi
echo "miner A: ${TADDR_A} / ${EVM_A}"
echo "miner B: ${TADDR_B} / ${EVM_B}"

echo "--- funding both miners (coinbase maturity) ---"
zc_generate_to_address "${FUND_BLOCKS}" "${TADDR_A}"
zc_generate_to_address 1 "${TADDR_B}"
zc_generate_to_address 100 "${TADDR_A}"
FUND_TIP="$(zc_tip_height)"
EPOCH_BASE=$((FUND_TIP + 1))
echo "funding tip: ${FUND_TIP}; epoch base: ${EPOCH_BASE}"

# ---------------------------------------------------------------------
# Both nodes, mine mode, mutual peers, short ladder step.
# ---------------------------------------------------------------------

echo "--- starting node B (mine mode, rank step ${RANK_STEP_S}s) ---"
SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_MINER_EVM_ADDRESS="${EVM_B}" \
  SOVA_EPOCH_BASE="${EPOCH_BASE}" \
  SOVA_RANK_STEP_SECS="${RANK_STEP_S}" \
  SOVA_PEERS="${AUTHRPC_A}" \
  SOVA_HTTP_PORT="${B_HTTP_PORT}" \
  SOVA_AUTH_PORT="${B_AUTH_PORT}" \
  SOVA_P2P_PORT="${B_P2P_PORT}" \
  SOVA_AUTH_JWT="${JWT_PATH}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-b.log" 2>&1 &
B_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B" 60 || exit 1

echo "--- starting node A (mine mode, rank step ${RANK_STEP_S}s) ---"
SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_MINER_EVM_ADDRESS="${EVM_A}" \
  SOVA_EPOCH_BASE="${EPOCH_BASE}" \
  SOVA_RANK_STEP_SECS="${RANK_STEP_S}" \
  SOVA_PEERS="${AUTHRPC_B}" \
  SOVA_AUTH_JWT="${JWT_PATH}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-a.log" 2>&1 &
A_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_A}" "${A_PID}" "node A" 60 || exit 1
echo "node A pid ${A_PID}, node B pid ${B_PID}"

# ---------------------------------------------------------------------
# Warm-up: a few burn-less epochs with BOTH nodes producing — the
# hash-tiebreak convergence for empty blocks must keep them agreeing.
# ---------------------------------------------------------------------

echo ""
echo "=== warm-up: concurrent burn-less production converges ==="
"${REGTEST_DIR}/auto-mine.sh" "${AUTO_MINE_INTERVAL_S}" "${ZEBRAD_RPC}" >"${WORK_DIR}/auto-mine.log" 2>&1 &
AUTO_MINE_PID=$!
sleep $((AUTO_MINE_INTERVAL_S * 5))
kill "${AUTO_MINE_PID}" 2>/dev/null || true
wait "${AUTO_MINE_PID}" 2>/dev/null || true
AUTO_MINE_PID=""
WARM_TIP="$(zc_tip_height)"
WARM_SOVA=$((WARM_TIP - EPOCH_BASE + 1))
if ! wait_for_block_number "${ENGINE_RPC_A}" "${WARM_SOVA}" 30 \
  || ! wait_for_block_number "${ENGINE_RPC_B}" "${WARM_SOVA}" 30; then
  fail "warm-up: nodes did not reach sova height ${WARM_SOVA}"
fi
sleep 3 # let late arbitration settle before comparing
WARM_OK=1
for h in $(seq 1 "${WARM_SOVA}"); do
  HA="$(eth_block_hash "${ENGINE_RPC_A}" "${h}")"
  HB="$(eth_block_hash "${ENGINE_RPC_B}" "${h}")"
  if [[ -z "${HA}" || "${HA}" != "${HB}" ]]; then
    WARM_OK=0
    fail "warm-up: burn-less height ${h} differs (A=${HA:-<none>} B=${HB:-<none>})"
  fi
done
if [[ "${WARM_OK}" -eq 1 ]]; then
  pass "warm-up: ${WARM_SOVA} concurrent burn-less heights identical on A and B"
fi

# ---------------------------------------------------------------------
# The controlled ladder epoch: node A SIGSTOPped; both miners burn into
# one generated block (A > B, so A=rank0, B=rank1).
# ---------------------------------------------------------------------

echo ""
echo "=== (1) ladder: rank 1 seals while rank 0 is silent ==="
echo "--- SIGSTOP node A ---"
kill -STOP "${A_PID}"

"${MINER_BIN}" --data-dir "${WORK_DIR}/miner-a" --network regtest mine \
  --budget-zat 200000 --per-epoch-zat 100000 --rpc "${ZEBRAD_RPC}" \
  --max-epochs 1 >"${WORK_DIR}/mine-a.log" 2>&1 &
MINE_A_PID=$!
"${MINER_BIN}" --data-dir "${WORK_DIR}/miner-b" --network regtest mine \
  --budget-zat 100000 --per-epoch-zat 40000 --rpc "${ZEBRAD_RPC}" \
  --max-epochs 1 >"${WORK_DIR}/mine-b.log" 2>&1 &
MINE_B_PID=$!

sleep 2 # miners capture their baseline tip
zc_generate_to_address 1 "${TADDR_A}" # trigger block: miners submit burns
# Wait for both burn txs to hit the MEMPOOL (the miners' own logs only
# print txids after confirmation, which needs the next block we mine),
# then mine the combined block.
deadline=$((SECONDS + 30))
while true; do
  MEMPOOL_COUNT="$(zc_rpc getrawmempool "[]" | python3 -c "import sys,json;print(len(json.load(sys.stdin)['result']))" 2>/dev/null || echo 0)"
  if [[ "${MEMPOOL_COUNT}" -ge 2 ]]; then
    break
  fi
  if [[ ${SECONDS} -ge ${deadline} ]]; then
    fail "(1) both burns did not reach the mempool within 30s (saw ${MEMPOOL_COUNT})"
    break
  fi
  sleep 1
done
zc_generate_to_address 1 "${TADDR_A}" # the combined burn epoch
BURN_TIP="$(zc_tip_height)"
wait "${MINE_A_PID}" 2>/dev/null || true
wait "${MINE_B_PID}" 2>/dev/null || true

TXID_A="$(grep -o 'txid=[0-9a-f]*' "${WORK_DIR}/mine-a.log" | head -1 | cut -d= -f2)"
TXID_B="$(grep -o 'txid=[0-9a-f]*' "${WORK_DIR}/mine-b.log" | head -1 | cut -d= -f2)"
BLOCK_TXIDS="$(zc_block_txids "${BURN_TIP}")"
if [[ "${BLOCK_TXIDS}" == *"${TXID_A}"* && "${BLOCK_TXIDS}" == *"${TXID_B}"* ]]; then
  pass "(1) combined epoch at zcash height ${BURN_TIP}: both burns in one block"
else
  fail "(1) burns not combined (block txs: ${BLOCK_TXIDS}; A=${TXID_A} B=${TXID_B})"
fi

LADDER_SOVA=$((BURN_TIP - EPOCH_BASE + 1))
echo "ladder epoch: zcash ${BURN_TIP} -> sova height ${LADDER_SOVA}"

# reth logs through a buffered background writer, so a line can land in the
# file a moment after the event it describes. Poll instead of grepping once.
log_has() { # <file> <fixed-string> [timeout_s]
  local deadline=$((SECONDS + ${3:-15}))
  while (( SECONDS < deadline )); do
    # grep -c reads to EOF: `grep -q` exits on the first match, SIGPIPEs
    # sed, and under pipefail the pipeline then reports failure.
    local n
    n="$(sed 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | grep -cF -- "$2" || true)"
    (( ${n:-0} > 0 )) && return 0
    sleep 1
  done
  return 1
}


# B (rank 1) must seal it after ~1*step; wait generously.
if wait_for_block_number "${ENGINE_RPC_B}" "${LADDER_SOVA}" $((RANK_STEP_S * 4 + 10)); then
  pass "(1) node B sealed the ladder epoch (rank 0 silent)"
else
  fail "(1) node B never sealed the ladder epoch within $((RANK_STEP_S * 4 + 10))s"
fi
if log_has "${WORK_DIR}/node-b.log" "settled=true"; then
  pass "(1) node B's log shows a settled trigger of its own"
else
  fail "(1) node B's log shows no settled trigger"
fi
B_HASH_BEFORE="$(eth_block_hash "${ENGINE_RPC_B}" "${LADDER_SOVA}")"
B_BAL_SELF="$(eth_balance_wei "${ENGINE_RPC_B}" "${EVM_B}")"
if [[ -n "${B_HASH_BEFORE}" && "${B_BAL_SELF}" != "0" ]]; then
  pass "(1) rank-1 block ${B_HASH_BEFORE} minted (B's balance ${B_BAL_SELF} wei includes the sealer tip)"
else
  fail "(1) rank-1 mint not visible on B (hash=${B_HASH_BEFORE:-<none>} bal=${B_BAL_SELF})"
fi

# ---------------------------------------------------------------------
# (2) late win: A resumes, seals rank 0 for the same height; B reorgs.
# ---------------------------------------------------------------------

echo ""
echo "=== (2) late win: resumed rank 0 displaces the rank-1 block ==="
kill -CONT "${A_PID}"
deadline=$((SECONDS + 45))
REORGED=0
while [[ ${SECONDS} -lt ${deadline} ]]; do
  B_HASH_NOW="$(eth_block_hash "${ENGINE_RPC_B}" "${LADDER_SOVA}")"
  if [[ -n "${B_HASH_NOW}" && "${B_HASH_NOW}" != "${B_HASH_BEFORE}" ]]; then
    REORGED=1
    break
  fi
  sleep 1
done
if [[ "${REORGED}" -eq 1 ]]; then
  pass "(2) node B micro-reorged height ${LADDER_SOVA}: ${B_HASH_BEFORE} -> ${B_HASH_NOW}"
else
  fail "(2) node B never adopted the late rank-0 block within 45s"
fi
if log_has "${WORK_DIR}/node-b.log" "arbiter adopted preferred candidate"; then
  pass "(2) node B's arbiter logged the adoption"
else
  fail "(2) node B's arbiter never logged an adoption"
fi

# ---------------------------------------------------------------------
# (3) convergence: hashes equal on every height so far; balances equal
# across nodes; lockstep resumes with auto-mine.
# ---------------------------------------------------------------------

echo ""
echo "=== (3) convergence and continued lockstep ==="
sleep 3
CONV_OK=1
for h in $(seq 1 "${LADDER_SOVA}"); do
  HA="$(eth_block_hash "${ENGINE_RPC_A}" "${h}")"
  HB="$(eth_block_hash "${ENGINE_RPC_B}" "${h}")"
  if [[ -z "${HA}" || "${HA}" != "${HB}" ]]; then
    CONV_OK=0
    fail "(3) height ${h} differs after convergence (A=${HA:-<none>} B=${HB:-<none>})"
  fi
done
if [[ "${CONV_OK}" -eq 1 ]]; then
  pass "(3) all ${LADDER_SOVA} heights identical on A and B after the reorg"
fi

for evm in "${EVM_A}" "${EVM_B}"; do
  BAL_A="$(eth_balance_wei "${ENGINE_RPC_A}" "${evm}")"
  BAL_B="$(eth_balance_wei "${ENGINE_RPC_B}" "${evm}")"
  if [[ "${BAL_A}" == "${BAL_B}" ]]; then
    pass "(3) ${evm}: identical balance on A and B (${BAL_A} wei)"
  else
    fail "(3) ${evm}: balance mismatch A=${BAL_A} B=${BAL_B}"
  fi
done

"${REGTEST_DIR}/auto-mine.sh" "${AUTO_MINE_INTERVAL_S}" "${ZEBRAD_RPC}" >>"${WORK_DIR}/auto-mine.log" 2>&1 &
AUTO_MINE_PID=$!
sleep $((AUTO_MINE_INTERVAL_S * 4))
A_BLOCK="$(eth_block_number "${ENGINE_RPC_A}")"
B_BLOCK="$(eth_block_number "${ENGINE_RPC_B}")"
LAG=$((A_BLOCK - B_BLOCK))
if [[ "${LAG}" -ge -1 && "${LAG}" -le 1 && "${A_BLOCK}" -gt "${LADDER_SOVA}" ]]; then
  pass "(3) lockstep resumed past the ladder epoch (A=${A_BLOCK} B=${B_BLOCK} lag=${LAG})"
else
  fail "(3) lockstep broken after the ladder epoch (A=${A_BLOCK} B=${B_BLOCK} lag=${LAG})"
fi

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "LADDER SCENARIO PASSED (all assertions)"
else
  echo "LADDER SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
