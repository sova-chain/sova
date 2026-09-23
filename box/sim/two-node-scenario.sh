#!/usr/bin/env bash
# Gossip v1: the first multi-node determinism proof.
#
# Runs two independent `bin/sova` processes on one host, wired together
# only by gossip v1's relay task (crates/engine/src/relay.rs,
# docs/design/gossip-v1.md) -- no shared process, no shared in-memory
# state:
#
#   node A -- mine mode (box/up.sh's own flow: miner identity, fund,
#             auto-mine, one burn), with SOVA_PEERS pointing at node B's
#             authrpc. A is the only block producer.
#   node B -- follow-only (SOVA_FOLLOW_ONLY=1), on shifted ports, same
#             SOVA_AUTH_JWT file as A. B never seals a block itself; its
#             entire chain is built by validating what A's relay task
#             pushes to its authrpc (engine_newPayloadV4 +
#             engine_forkchoiceUpdatedV3). Since v2, B also points
#             SOVA_ZEBRAD_RPC at the (shared, read-only) regtest zebrad:
#             it re-derives every imported height's settlements from that
#             view and would reject a contradiction (C5), so this
#             scenario now proves enforcement-on, not import-on-trust.
#             (In production each node runs its OWN zebrad; the box
#             shares one container the way it shares the host.)
#
# Three assertions, all against the two live nodes:
#
#   (a) lockstep    -- B's eth_blockNumber tracks A's, sampled 3x, lag<=1.
#   (b) state roots -- getBlockByNumber(H, false).hash is IDENTICAL on A
#                       and B for 3 heights, including the block that
#                       carried the burn's settlement. Block-hash equality
#                       implies state-root equality (the hash covers the
#                       header, which includes stateRoot) -- this is the
#                       actual multi-node determinism proof: two
#                       independently-validated executions of the same
#                       payload landed on the same state.
#   (c) mint visible through relay -- the miner's EVM balance on B equals
#                       A's (the withdrawal-channel mint, relayed and
#                       replayed on B exactly as A produced it).
#
# Modeled on box/sim/mint-scenario.sh (same debug-binary convention,
# same PASS/FAIL/EXIT-trap shape) but doubled: two `bin/sova` processes,
# a generated shared JWT, and gossip in between instead of one node
# proving its own mint.
#
# Environment courtesy: this script does NOT kill another process/
# container on start. It first waits (bounded 15 minutes, polling) for
# the shared box/regtest harness to be free -- any `sova-zebrad-regtest`
# container or `bin/sova`/`sova-miner`/`auto-mine.sh` process already
# running is treated as another agent's work in progress, not a stray to
# clear. Only once the harness is confirmed idle does this script start
# anything of its own; the EXIT trap then tears down exactly what THIS
# run started (by captured PID/container name), which doubles as the
# stray-process guard for a crashed prior run of this same script.
#
# Always tears its own stack down on exit (success or failure): kills
# node A + node B (by captured pid), stops auto-mine, and brings the
# regtest container down.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../.." && pwd)"
REGTEST_DIR="${ROOT}/box/regtest"
BURN_WALLET_DIR="${ROOT}/crates/burn-wallet"
COMPOSE="docker compose"

ZEBRAD_RPC="${SOVA_REGTEST_RPC:-http://127.0.0.1:18232}"

# Node A (mine mode): reth's own defaults -- nothing shifts here, it's
# the "primary" node in this pair.
A_HTTP_PORT=8545
A_AUTH_PORT=8551
ENGINE_RPC_A="http://127.0.0.1:${A_HTTP_PORT}"
AUTHRPC_A="http://127.0.0.1:${A_AUTH_PORT}"

# Node B (follow-only): shifted HTTP/authrpc/p2p ports so it can run
# alongside node A on the same host (SOVA_HTTP_PORT/SOVA_AUTH_PORT/
# SOVA_P2P_PORT -- see bin/sova/src/main.rs's `apply_port_overrides`).
B_HTTP_PORT=8645
B_AUTH_PORT=8651
B_P2P_PORT=30313
ENGINE_RPC_B="http://127.0.0.1:${B_HTTP_PORT}"
AUTHRPC_B="http://127.0.0.1:${B_AUTH_PORT}"

SOVA_BIN="${ROOT}/target/debug/sova"
MINER_BIN="${BURN_WALLET_DIR}/target/release/sova-miner"

BUDGET_ZAT=200000
PER_EPOCH_ZAT=100000
FUND_BLOCKS=101
AUTO_MINE_INTERVAL_S=2
HARNESS_WAIT_TIMEOUT_S=900 # 15 minutes

STARTED_STACK=0
A_PID=""
B_PID=""
AUTO_MINE_PID=""
WORK_DIR=""
FAILURES=0

pass() { echo "PASS: two-node relay: $*" >&2; }
fail() {
  echo "FAIL: two-node relay: $*" >&2
  FAILURES=$((FAILURES + 1))
}

cleanup() {
  local exit_code=$?
  if [[ -n "${AUTO_MINE_PID}" ]] && kill -0 "${AUTO_MINE_PID}" 2>/dev/null; then
    echo "--- stopping auto-mine (pid ${AUTO_MINE_PID}) ---"
    kill "${AUTO_MINE_PID}" 2>/dev/null || true
    wait "${AUTO_MINE_PID}" 2>/dev/null || true
  fi
  if [[ -n "${A_PID}" ]] && kill -0 "${A_PID}" 2>/dev/null; then
    echo "--- stopping node A (pid ${A_PID}) ---"
    kill "${A_PID}" 2>/dev/null || true
    wait "${A_PID}" 2>/dev/null || true
  fi
  if [[ -n "${B_PID}" ]] && kill -0 "${B_PID}" 2>/dev/null; then
    echo "--- stopping node B (pid ${B_PID}) ---"
    kill "${B_PID}" 2>/dev/null || true
    wait "${B_PID}" 2>/dev/null || true
  fi
  # Belt and suspenders for a lost-PID case (e.g. this script itself was
  # killed before A_PID/B_PID were captured) -- matches
  # box/sim/mint-scenario.sh's own convention. Safe here specifically
  # because wait_for_harness_free (below) already confirmed nothing else
  # was using `target/debug/sova` before this run started anything.
  pkill -f "target/debug/sova$" 2>/dev/null || true
  if [[ -n "${WORK_DIR}" ]]; then
    pkill -f "sova-miner --data-dir ${WORK_DIR}" 2>/dev/null || true
  fi

  if [[ "${exit_code}" -ne 0 || "${FAILURES}" -gt 0 ]]; then
    echo "--- two-node relay scenario failed (exit ${exit_code}, ${FAILURES} assertion failure(s)); logs follow ---" >&2
    if [[ -n "${WORK_DIR}" && -f "${WORK_DIR}/node-a.log" ]]; then
      echo "--- node A log (last 60 lines) ---" >&2
      tail -n 60 "${WORK_DIR}/node-a.log" >&2 || true
    fi
    if [[ -n "${WORK_DIR}" && -f "${WORK_DIR}/node-b.log" ]]; then
      echo "--- node B log (last 60 lines) ---" >&2
      tail -n 60 "${WORK_DIR}/node-b.log" >&2 || true
    fi
    (cd "${REGTEST_DIR}" && ${COMPOSE} logs --no-color zebrad) >&2 || true
  fi

  if [[ "${STARTED_STACK}" -eq 1 ]]; then
    echo "--- tearing down regtest stack ---"
    (cd "${REGTEST_DIR}" && ${COMPOSE} down -v) >/dev/null 2>&1 || true
  fi
  if [[ -n "${WORK_DIR}" && -d "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
  if [[ "${exit_code}" -eq 0 && "${FAILURES}" -gt 0 ]]; then
    exit 1
  fi
  exit "${exit_code}"
}
trap cleanup EXIT

# ---------------------------------------------------------------------
# RPC helpers (Zcash / zebrad)
# ---------------------------------------------------------------------

zc_rpc() {
  local method="$1" params="$2"
  curl -s -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"two-node\",\"method\":\"${method}\",\"params\":${params}}" \
    "${ZEBRAD_RPC}/"
}

zc_tip_height() {
  zc_rpc getblockcount "[]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'])"
}

zc_generate_to_address() {
  local n="$1" addr="$2"
  zc_rpc generatetoaddress "[${n}, \"${addr}\"]" >/dev/null
}

# ---------------------------------------------------------------------
# RPC helpers (Sova EVM), parameterized by node URL
# ---------------------------------------------------------------------

eth_rpc() {
  local url="$1" method="$2" params="$3"
  curl -s -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"two-node\",\"method\":\"${method}\",\"params\":${params}}" \
    "${url}"
}

eth_block_number() {
  eth_rpc "$1" eth_blockNumber "[]" | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))"
}

eth_balance_wei() {
  local url="$1" addr="$2"
  eth_rpc "${url}" eth_getBalance "[\"${addr}\",\"latest\"]" \
    | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))"
}

# getBlockByNumber(height, false).hash -- empty string if the node
# doesn't have that height yet (or the RPC call otherwise failed).
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

wait_for_balance_change() {
  local url="$1" addr="$2" baseline="$3" timeout_s="${4:-90}"
  local deadline=$((SECONDS + timeout_s)) bal
  while true; do
    bal="$(eth_balance_wei "${url}" "${addr}" 2>/dev/null || true)"
    if [[ -n "${bal}" && "${bal}" != "${baseline}" ]]; then
      echo "${bal}"
      return 0
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "${bal:-${baseline}}"
      return 1
    fi
    sleep 2
  done
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
  local deadline=$((SECONDS + timeout_s))
  until eth_rpc "${url}" eth_chainId "[]" | grep -q result; do
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

# ---------------------------------------------------------------------
# Environment courtesy: wait for the shared harness to be free rather
# than killing another agent's run of it.
# ---------------------------------------------------------------------

harness_busy() {
  if docker ps --format '{{.Names}}' 2>/dev/null | grep -qx "sova-zebrad-regtest"; then
    return 0
  fi
  if pgrep -f "target/(debug|release)/sova(\$| )" >/dev/null 2>&1; then
    return 0
  fi
  if pgrep -f "sova-miner .*(init|mine)" >/dev/null 2>&1; then
    return 0
  fi
  if pgrep -f "auto-mine\.sh" >/dev/null 2>&1; then
    return 0
  fi
  return 1
}

wait_for_harness_free() {
  local deadline=$((SECONDS + HARNESS_WAIT_TIMEOUT_S))
  if ! harness_busy; then
    echo "--- harness free (no zebrad container, no sova/sova-miner/auto-mine process) ---"
    return 0
  fi
  echo "--- harness busy (zebrad container or sova/sova-miner/auto-mine process already running) ---"
  echo "--- waiting up to $((HARNESS_WAIT_TIMEOUT_S / 60)) minutes for it to free, rather than killing another agent's run ---"
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

# ---------------------------------------------------------------------
# Build binaries if missing (debug bin/sova, matching mint-scenario.sh's
# own convention; release sova-miner, its own nested workspace)
# ---------------------------------------------------------------------

if [[ ! -x "${SOVA_BIN}" ]]; then
  echo "--- building bin/sova (debug; not found at ${SOVA_BIN}) ---"
  (cd "${ROOT}" && cargo build -p sova --quiet)
fi
if [[ ! -x "${MINER_BIN}" ]]; then
  echo "--- building sova-miner (release; not found at ${MINER_BIN}) ---"
  (cd "${BURN_WALLET_DIR}" && cargo build --release -p sova-miner --quiet)
fi

# ---------------------------------------------------------------------
# Fresh regtest stack
# ---------------------------------------------------------------------

echo "--- starting a fresh regtest stack ---"
(cd "${REGTEST_DIR}" && ${COMPOSE} up -d)
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

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/sova-two-node.XXXXXX")"
echo "--- work dir: ${WORK_DIR} ---"

# ---------------------------------------------------------------------
# Shared JWT: generated once, up front, so both nodes find it already in
# place at launch -- no create-on-first-use race between two processes
# starting close together (see bin/sova/src/main.rs's
# `apply_shared_jwt` doc comment for the fallback that exists for
# standalone/manual use instead of this).
# ---------------------------------------------------------------------

JWT_PATH="${WORK_DIR}/jwt.hex"
openssl rand -hex 32 >"${JWT_PATH}"
echo "--- shared JWT written to ${JWT_PATH} ---"

# ---------------------------------------------------------------------
# Miner identity (init first -- node A needs its EVM address to start in
# mine mode)
# ---------------------------------------------------------------------

MINER_DATA_DIR="${WORK_DIR}/miner"
mkdir -p "${MINER_DATA_DIR}"
echo "--- sova-miner init ---"
"${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest init >"${WORK_DIR}/miner-init.log" 2>&1
TADDR="$(awk '/t-addr to fund/ {print $NF}' "${WORK_DIR}/miner-init.log")"
EVM_ADDR="$(awk '/evm address/ {print $NF}' "${WORK_DIR}/miner-init.log")"
if [[ -z "${TADDR}" || -z "${EVM_ADDR}" ]]; then
  fail "setup: could not parse t-addr/evm address from sova-miner init output"
  cat "${WORK_DIR}/miner-init.log" >&2
  exit 1
fi
echo "miner identity: ${TADDR} / ${EVM_ADDR}"

# ---------------------------------------------------------------------
# Node B first (follow-only, shifted ports): it has no dependency on the
# miner identity or on node A, and coming up first means node A's relay
# has somewhere to push to from its very first block.
# ---------------------------------------------------------------------

echo "--- starting node B (follow-only, C5-enforcing via its own zebrad view) ---"
SOVA_FOLLOW_ONLY=1 \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_EPOCH_BASE=1 \
  SOVA_HTTP_PORT="${B_HTTP_PORT}" \
  SOVA_AUTH_PORT="${B_AUTH_PORT}" \
  SOVA_P2P_PORT="${B_P2P_PORT}" \
  SOVA_AUTH_JWT="${JWT_PATH}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-b.log" 2>&1 &
B_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B" 60 || exit 1
echo "node B up (pid ${B_PID}), http :${B_HTTP_PORT}, authrpc :${B_AUTH_PORT}"
if ! grep -q "expectations: enforcing settlements" "${WORK_DIR}/node-b.log"; then
  fail "setup: node B's log doesn't show C5 settlement enforcement starting"
fi

# ---------------------------------------------------------------------
# Node A (mine mode), peered at node B's authrpc over the same JWT.
# ---------------------------------------------------------------------

echo "--- starting node A (mine mode, SOVA_PEERS -> node B) ---"
SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" \
  SOVA_EPOCH_BASE=1 \
  SOVA_PEERS="${AUTHRPC_B}" \
  SOVA_AUTH_JWT="${JWT_PATH}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-a.log" 2>&1 &
A_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_A}" "${A_PID}" "node A" 60 || exit 1
echo "node A up (pid ${A_PID}), http :${A_HTTP_PORT}, authrpc :${A_AUTH_PORT}"
if ! grep -q "relay: pushing sealed blocks to 1 peer(s)" "${WORK_DIR}/node-a.log"; then
  fail "setup: node A's log doesn't show the relay task starting against node B"
fi

# ---------------------------------------------------------------------
# Fund the miner and drive the chain: 101 blocks to its own t-addr (1
# spendable coinbase once the other 100 mature it), matching box/up.sh's
# and mint-scenario.sh's own proven flow.
# ---------------------------------------------------------------------

echo "--- funding: ${FUND_BLOCKS} blocks to the miner's own address (coinbase maturity) ---"
zc_generate_to_address "${FUND_BLOCKS}" "${TADDR}"
FUNDING_TIP="$(zc_tip_height)"
echo "tip after funding: ${FUNDING_TIP} (expected ${FUND_BLOCKS})"
if [[ "${FUNDING_TIP}" != "${FUND_BLOCKS}" ]]; then
  fail "setup: expected tip ${FUND_BLOCKS} after funding, got ${FUNDING_TIP}"
fi

echo "--- starting auto-mine (1 block every ${AUTO_MINE_INTERVAL_S}s) in the background ---"
"${REGTEST_DIR}/auto-mine.sh" "${AUTO_MINE_INTERVAL_S}" "${ZEBRAD_RPC}" >"${WORK_DIR}/auto-mine.log" 2>&1 &
AUTO_MINE_PID=$!

# ============================================================
# One burn: enough to prove the mint replicates through the relay --
# box/up.sh's own flow generalizes the same way scenario 4
# (box/sim/mint-scenario.sh) does with a second burn, not needed here
# since this scenario isn't re-proving repeated settlement, only that
# gossip carries a settled epoch identically to a second node.
# ============================================================
echo ""
echo "=== burn (node A's miner) ==="
if ! "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
  --budget-zat "${BUDGET_ZAT}" \
  --per-epoch-zat "${PER_EPOCH_ZAT}" \
  --rpc "${ZEBRAD_RPC}" \
  --max-epochs 1 >"${WORK_DIR}/mine-1.log" 2>&1; then
  fail "burn: sova-miner mine exited nonzero"
  cat "${WORK_DIR}/mine-1.log" >&2
fi
cat "${WORK_DIR}/mine-1.log"

echo "--- waiting for the burn's epoch to settle on node A (balance off zero) ---"
BAL_A="$(wait_for_balance_change "${ENGINE_RPC_A}" "${EVM_ADDR}" "0" 90)" || {
  fail "setup: node A's balance never moved off 0 within 90s (last observed: ${BAL_A})"
  exit 1
}
echo "node A settled balance: ${BAL_A} wei"

# The settled epoch's Sova block height, straight from node A's own log
# (`sova epoch trigger height=<zcash_height> settled=true`) rather than
# sampled from a moving tip -- with SOVA_EPOCH_BASE=1 and one Sova block
# per Zcash epoch (no epochs skipped here: a single miner, one burn), the
# logged Zcash height and the Sova block height that carries the
# settlement coincide (see crates/engine/src/driver.rs's
# `expected_sova_height`; box/sim/README.md's scenario 4 documents the
# same correspondence and its limits).
SETTLED_HEIGHT="$(sed 's/\x1b\[[0-9;]*m//g' "${WORK_DIR}/node-a.log" \
  | grep 'sova epoch trigger' | grep 'settled=true' \
  | grep -o 'height=[0-9]*' | head -1 | cut -d= -f2)"
if [[ -z "${SETTLED_HEIGHT}" ]]; then
  fail "setup: could not find a 'sova epoch trigger ... settled=true' line in node A's log"
  exit 1
fi
echo "settled height (node A): ${SETTLED_HEIGHT}"

# ============================================================
# (a) lockstep: node B's eth_blockNumber tracks node A's, 3 samples over
# ~15s while auto-mine keeps the chain moving.
# ============================================================
echo ""
echo "=== (a) lockstep sampling (node B vs node A) ==="
LOCKSTEP_SAMPLES=3
LOCKSTEP_INTERVAL_S=5
i=0
while [[ ${i} -lt ${LOCKSTEP_SAMPLES} ]]; do
  A_BLOCK="$(eth_block_number "${ENGINE_RPC_A}")"
  B_BLOCK="$(eth_block_number "${ENGINE_RPC_B}")"
  LAG=$((A_BLOCK - B_BLOCK))
  echo "  sample $((i + 1))/${LOCKSTEP_SAMPLES}: node_a=${A_BLOCK} node_b=${B_BLOCK} lag=${LAG}"
  if [[ "${LAG}" -lt 0 || "${LAG}" -gt 1 ]]; then
    fail "(a) lockstep: sample $((i + 1)) node_a=${A_BLOCK} node_b=${B_BLOCK} lag=${LAG} -- outside allowed [0,1]"
  else
    pass "(a) lockstep: sample $((i + 1)) node_a=${A_BLOCK} node_b=${B_BLOCK} lag=${LAG} (<=1 OK)"
  fi
  i=$((i + 1))
  if [[ ${i} -lt ${LOCKSTEP_SAMPLES} ]]; then
    sleep "${LOCKSTEP_INTERVAL_S}"
  fi
done

# ============================================================
# (b) state-root equality: getBlockByNumber(H, false).hash identical on
# A and B for 3 heights, including the settled epoch's block.
# ============================================================
echo ""
echo "=== (b) block-hash (state-root) equality across 3 heights ==="
TIP_A="$(eth_block_number "${ENGINE_RPC_A}")"
declare -a CANDIDATES=(1 "${SETTLED_HEIGHT}" "${TIP_A}")
declare -a HEIGHTS=()
for h in "${CANDIDATES[@]}"; do
  dup=0
  for existing in "${HEIGHTS[@]:-}"; do
    if [[ "${existing}" == "${h}" ]]; then
      dup=1
      break
    fi
  done
  if [[ "${dup}" -eq 0 ]]; then
    HEIGHTS+=("${h}")
  fi
done
extra=$((SETTLED_HEIGHT + 1))
while [[ "${#HEIGHTS[@]}" -lt 3 ]]; do
  dup=0
  for existing in "${HEIGHTS[@]}"; do
    if [[ "${existing}" == "${extra}" ]]; then
      dup=1
      break
    fi
  done
  if [[ "${dup}" -eq 0 ]]; then
    HEIGHTS+=("${extra}")
  fi
  extra=$((extra + 1))
done

MAX_HEIGHT=0
for h in "${HEIGHTS[@]}"; do
  if [[ "${h}" -gt "${MAX_HEIGHT}" ]]; then
    MAX_HEIGHT="${h}"
  fi
done
echo "--- waiting for node B to relay-catch-up to height ${MAX_HEIGHT} ---"
if ! wait_for_block_number "${ENGINE_RPC_B}" "${MAX_HEIGHT}" 60; then
  fail "(b) node B never reached height ${MAX_HEIGHT} within 60s (relay stalled?)"
fi

for h in "${HEIGHTS[@]}"; do
  HASH_A="$(eth_block_hash "${ENGINE_RPC_A}" "${h}")"
  HASH_B="$(eth_block_hash "${ENGINE_RPC_B}" "${h}")"
  if [[ -z "${HASH_A}" || -z "${HASH_B}" ]]; then
    fail "(b) height ${h}: missing hash (node A=${HASH_A:-<none>} node B=${HASH_B:-<none>})"
  elif [[ "${HASH_A}" == "${HASH_B}" ]]; then
    pass "(b) height ${h}: identical block hash on A and B (${HASH_A})$([[ "${h}" == "${SETTLED_HEIGHT}" ]] && echo " -- the settled epoch's block")"
  else
    fail "(b) height ${h}: hash mismatch -- node A=${HASH_A} node B=${HASH_B}"
  fi
done

# ============================================================
# (c) the mint is visible through the relay: node B's balance for the
# miner's address equals node A's.
# ============================================================
echo ""
echo "=== (c) miner balance equality across the relay ==="
if ! wait_for_block_number "${ENGINE_RPC_B}" "${SETTLED_HEIGHT}" 30; then
  fail "(c) node B never reached the settled height ${SETTLED_HEIGHT} within 30s"
fi
BAL_A_FINAL="$(eth_balance_wei "${ENGINE_RPC_A}" "${EVM_ADDR}")"
BAL_B_FINAL="$(eth_balance_wei "${ENGINE_RPC_B}" "${EVM_ADDR}")"
if [[ "${BAL_A_FINAL}" == "${BAL_B_FINAL}" && "${BAL_A_FINAL}" != "0" ]]; then
  pass "(c) miner balance: node A == node B == ${BAL_A_FINAL} wei (mint visible through relay)"
else
  fail "(c) miner balance mismatch: node A=${BAL_A_FINAL} wei, node B=${BAL_B_FINAL} wei"
fi

# ---------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------
echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "TWO-NODE RELAY SCENARIO PASSED (all assertions)"
else
  echo "TWO-NODE RELAY SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
