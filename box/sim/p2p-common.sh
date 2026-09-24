#!/usr/bin/env bash
# Shared plumbing for the sova/1 (SOVA_GOSSIP=p2p) sim scenarios:
# box/sim/two-node-p2p-scenario.sh, box/sim/ladder-p2p-scenario.sh and
# box/sim/three-node-discovery-scenario.sh. Sourced, not executed. The
# caller sets SCENARIO (log prefix) and WORK_PREFIX before sourcing (and
# P2P_THREE_NODES=1 to get a node C), and defines nothing else of its own.
#
# Isolation (why these scenarios can run beside another agent's box):
# everything is on its own ports and its own compose project/container,
# all overridable --
#   SOVA_P2P_SIM_ZEBRAD_PORT       zebrad RPC host port   (default 18272)
#   SOVA_P2P_SIM_ZEBRAD_CONTAINER  zebrad container name  (default sova-zebrad-p2p-sim)
#   SOVA_P2P_SIM_COMPOSE_PROJECT   compose project        (default sova-p2p-sim)
#   SOVA_P2P_SIM_A_PORTS / _B_PORTS  "http auth p2p"      (default "8745 8751 30411" / "8845 8851 30412")
#   SOVA_P2P_SIM_C_PORTS           "http auth p2p", node C (P2P_THREE_NODES=1 only; default "8945 8951 30413")
#   SOVA_BIN / SOVA_MINER_BIN      binaries (default: $CARGO_TARGET_DIR or ./target debug sova;
#                                  crates/burn-wallet/target/release/sova-miner)
# and teardown touches only what this run started: node PIDs it captured
# and its own compose project. No global pkill, no shared-harness wait.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../.." && pwd)"
REGTEST_DIR="${ROOT}/box/regtest"
BURN_WALLET_DIR="${ROOT}/crates/burn-wallet"

ZEBRAD_PORT="${SOVA_P2P_SIM_ZEBRAD_PORT:-18272}"
ZEBRAD_CONTAINER="${SOVA_P2P_SIM_ZEBRAD_CONTAINER:-sova-zebrad-p2p-sim}"
COMPOSE_PROJECT="${SOVA_P2P_SIM_COMPOSE_PROJECT:-sova-p2p-sim}"
ZEBRAD_RPC="http://127.0.0.1:${ZEBRAD_PORT}"

read -r A_HTTP_PORT A_AUTH_PORT A_P2P_PORT <<<"${SOVA_P2P_SIM_A_PORTS:-8745 8751 30411}"
read -r B_HTTP_PORT B_AUTH_PORT B_P2P_PORT <<<"${SOVA_P2P_SIM_B_PORTS:-8845 8851 30412}"
read -r C_HTTP_PORT C_AUTH_PORT C_P2P_PORT <<<"${SOVA_P2P_SIM_C_PORTS:-8945 8951 30413}"
ENGINE_RPC_A="http://127.0.0.1:${A_HTTP_PORT}"
ENGINE_RPC_B="http://127.0.0.1:${B_HTTP_PORT}"
ENGINE_RPC_C="http://127.0.0.1:${C_HTTP_PORT}"
P2P_THREE_NODES="${P2P_THREE_NODES:-0}"

SOVA_BIN="${SOVA_BIN:-${CARGO_TARGET_DIR:-${ROOT}/target}/debug/sova}"
MINER_BIN="${SOVA_MINER_BIN:-${BURN_WALLET_DIR}/target/release/sova-miner}"

STARTED_STACK=0
A_PID=""
B_PID=""
C_PID=""
AUTO_MINE_PID=""
SOCKET_SAMPLER_PID=""
WORK_DIR=""
FAILURES=0

pass() { echo "PASS: ${SCENARIO}: $*" >&2; }
fail() {
  echo "FAIL: ${SCENARIO}: $*" >&2
  FAILURES=$((FAILURES + 1))
}

compose() {
  (cd "${REGTEST_DIR}" \
    && SOVA_BOX_ZEBRAD_PORT="${ZEBRAD_PORT}" \
      SOVA_BOX_ZEBRAD_CONTAINER="${ZEBRAD_CONTAINER}" \
      docker compose -p "${COMPOSE_PROJECT}" "$@")
}

cleanup() {
  local exit_code=$?
  if [[ -n "${A_PID}" ]]; then
    kill -CONT "${A_PID}" 2>/dev/null || true
  fi
  for pid in "${AUTO_MINE_PID}" "${SOCKET_SAMPLER_PID}"; do
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      kill "${pid}" 2>/dev/null || true
      wait "${pid}" 2>/dev/null || true
    fi
  done
  for pid in "${A_PID}" "${B_PID}" "${C_PID}"; do
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      kill "${pid}" 2>/dev/null || true
      wait "${pid}" 2>/dev/null || true
    fi
  done
  if [[ -n "${WORK_DIR}" ]]; then
    pkill -f "sova-miner --data-dir ${WORK_DIR}" 2>/dev/null || true
  fi

  if [[ "${exit_code}" -ne 0 || "${FAILURES}" -gt 0 ]]; then
    echo "--- ${SCENARIO} failed (exit ${exit_code}, ${FAILURES} assertion failure(s)); logs follow ---" >&2
    for log in node-a.log node-b.log node-c.log node-j.log; do
      if [[ -n "${WORK_DIR}" && -f "${WORK_DIR}/${log}" ]]; then
        echo "--- ${log} (last 80 lines) ---" >&2
        tail -n 80 "${WORK_DIR}/${log}" >&2 || true
        echo "--- ${log}: WARN/ERROR lines (last 40) ---" >&2
        { sed 's/\x1b\[[0-9;]*m//g' "${WORK_DIR}/${log}" | grep -E ' (WARN|ERROR) ' || true; } \
          | tail -n 40 >&2 || true
      fi
    done
  fi
  if [[ -n "${SOVA_P2P_SIM_KEEP_LOGS:-}" && -n "${WORK_DIR}" && -d "${WORK_DIR}" ]]; then
    mkdir -p "${SOVA_P2P_SIM_KEEP_LOGS}"
    cp "${WORK_DIR}"/*.log "${SOVA_P2P_SIM_KEEP_LOGS}/" 2>/dev/null || true
    echo "--- logs kept in ${SOVA_P2P_SIM_KEEP_LOGS} ---"
  fi

  if [[ "${STARTED_STACK}" -eq 1 ]]; then
    echo "--- tearing down compose project ${COMPOSE_PROJECT} ---"
    compose down -v >/dev/null 2>&1 || true
  fi
  if [[ -n "${WORK_DIR}" && -d "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
  if [[ "${exit_code}" -eq 0 && "${FAILURES}" -gt 0 ]]; then
    exit 1
  fi
  exit "${exit_code}"
}

# --- Zcash RPC ---------------------------------------------------------

zc_rpc() {
  curl -s -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"${SCENARIO}\",\"method\":\"$1\",\"params\":$2}" \
    "${ZEBRAD_RPC}/"
}

zc_tip_height() {
  zc_rpc getblockcount "[]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'])"
}

zc_generate_to_address() {
  zc_rpc generatetoaddress "[$1, \"$2\"]" >/dev/null
}

zc_block_txids() {
  local hash
  hash="$(zc_rpc getblockhash "[$1]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'])")"
  zc_rpc getblock "[\"${hash}\", 1]" | python3 -c "
import sys, json
print(' '.join(json.load(sys.stdin)['result']['tx'][1:]))
"
}

# --- Sova RPC ----------------------------------------------------------

eth_rpc() {
  curl -s -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"${SCENARIO}\",\"method\":\"$2\",\"params\":$3}" \
    "$1"
}

eth_block_number() {
  eth_rpc "$1" eth_blockNumber "[]" | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))"
}

# Block number behind a tag ("safe", "finalized"); empty when unset.
eth_tag_number() {
  eth_rpc "$1" eth_getBlockByNumber "[\"$2\", false]" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin).get('result')
    print(int(d['number'], 16) if d else '')
except Exception:
    print('')
"
}

eth_balance_wei() {
  eth_rpc "$1" eth_getBalance "[\"$2\",\"latest\"]" \
    | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))"
}

eth_block_hash() {
  local hex_height
  hex_height="$(printf '0x%x' "$2")"
  eth_rpc "$1" eth_getBlockByNumber "[\"${hex_height}\", false]" | python3 -c "
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
    [[ "${cur}" -ge "${target}" ]] && return 0
    [[ ${SECONDS} -ge ${deadline} ]] && return 1
    sleep 1
  done
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

wait_for_eth_rpc() {
  local url="$1" pid="$2" label="$3" timeout_s="${4:-90}"
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
}

strip_ansi() { sed 's/\x1b\[[0-9;]*m//g' "$1"; }

# The node's own enode (printed by bin/sova in p2p mode), host rewritten
# to loopback: reth advertises its listen address, which is 0.0.0.0.
local_enode() {
  local log="$1" deadline=$((SECONDS + 30)) enode=""
  while [[ -z "${enode}" && ${SECONDS} -lt ${deadline} ]]; do
    enode="$(strip_ansi "${log}" | grep -o 'local enode enode://[0-9a-f]*@[^ ]*' | head -1 | sed 's/^local enode //')"
    [[ -z "${enode}" ]] && sleep 1
  done
  [[ -z "${enode}" ]] && return 1
  local id port
  id="$(echo "${enode}" | sed -E 's#enode://([0-9a-f]+)@.*#\1#')"
  port="$(echo "${enode}" | sed -E 's#.*:([0-9]+)(\?.*)?$#\1#')"
  echo "enode://${id}@127.0.0.1:${port}"
}

# The bare node id (128 hex) of an enode URL.
enode_id() { sed -E 's#enode://([0-9a-f]+)@.*#\1#' <<<"$1"; }

# Distinct peer ids a node's gossip service reported as active sova/1
# peers (logged as 0x-prefixed hex), one per line, without the 0x.
sova_active_peers() {
  strip_ansi "$1" | grep 'sova/1: peer active' | grep -o 'peer_id=0x[0-9a-f]*' \
    | sed 's/^peer_id=0x//' | sort -u
}

# Wait until a node's log shows an active sova/1 session with peer id $2.
wait_for_sova_peer_id() {
  local log="$1" id="$2" deadline=$((SECONDS + ${3:-60})) peers_out
  # Not `sova_active_peers | grep -qx`: under pipefail a match can SIGPIPE
  # the producer and read as "no such peer".
  until peers_out="$(sova_active_peers "${log}")" && grep -qx "${id}" <<<"${peers_out}"; do
    [[ ${SECONDS} -ge ${deadline} ]] && return 1
    sleep 1
  done
}

# Background sampler: every 2s, every internet socket (`lsof -i`, TCP
# and UDP) of the PIDs listed in $2 (one per line; append to it as nodes
# start) is appended to $1. `non_local_sockets` then reports any socket
# ever bound to, or connected to, a non-loopback address.
start_socket_sampler() {
  local out="$1" pidfile="$2"
  : >"${out}"
  (
    while true; do
      local pids
      pids="$(paste -sd, "${pidfile}" 2>/dev/null)"
      if [[ -n "${pids}" ]]; then
        lsof -nP -a -p "${pids}" -i 2>/dev/null | tail -n +2 >>"${out}"
      fi
      sleep 2
    done
  ) &
  SOCKET_SAMPLER_PID=$!
}

# Prints offending lines (non-loopback local binds or remote endpoints);
# empty output means every sampled socket was loopback-only. Local
# wildcard binds (`*:port`) are reported too: with SOVA_P2P_ADDR=127.0.0.1
# there should be none for RLPx/discovery, but zebrad RPC and HTTP are
# explicitly loopback already.
non_local_sockets() {
  python3 - "$1" <<'PY'
import sys, re
bad = set()
for line in open(sys.argv[1]):
    parts = line.split()
    if len(parts) < 9:
        continue
    name = parts[8]
    for ep in name.split('->'):
        ep = ep.strip()
        host = ep.rsplit(':', 1)[0].strip('[]')
        if host not in ('127.0.0.1', '::1', 'localhost'):
            bad.add(f"{parts[0]} pid {parts[1]} {parts[7]} {name}")
for b in sorted(bad):
    print(b)
PY
}

# Wait until a node's log shows an active sova/1 peer.
wait_for_sova_peer() {
  local log="$1" timeout_s="${2:-60}" deadline=$((SECONDS + ${2:-60}))
  until [[ "$(strip_ansi "${log}" | grep -c "sova/1: peer active" || true)" -gt 0 ]]; do
    [[ ${SECONDS} -ge ${deadline} ]] && return 1
    sleep 1
  done
}

# --- stack -------------------------------------------------------------

preflight() {
  local docker_names
  # Not `docker ps | grep -qx`: under pipefail a match can SIGPIPE docker
  # and this guard would silently pass.
  if docker_names="$(docker ps -a --format '{{.Names}}' 2>/dev/null)" \
    && grep -qx "${ZEBRAD_CONTAINER}" <<<"${docker_names}"; then
    echo "error: container ${ZEBRAD_CONTAINER} already exists (another run of this scenario?); not touching it" >&2
    exit 1
  fi
  local port ports=("${ZEBRAD_PORT}" "${A_HTTP_PORT}" "${A_AUTH_PORT}" "${A_P2P_PORT}"
    "${B_HTTP_PORT}" "${B_AUTH_PORT}" "${B_P2P_PORT}")
  if [[ "${P2P_THREE_NODES}" == "1" ]]; then
    ports+=("${C_HTTP_PORT}" "${C_AUTH_PORT}" "${C_P2P_PORT}")
  fi
  for port in "${ports[@]}"; do
    if lsof -nP -iTCP:"${port}" -sTCP:LISTEN >/dev/null 2>&1; then
      echo "error: port ${port} already in use; override SOVA_P2P_SIM_* ports" >&2
      exit 1
    fi
  done
  # Discovery binds UDP on the RLPx port.
  for port in "${A_P2P_PORT}" "${B_P2P_PORT}" "${C_P2P_PORT}"; do
    if [[ "${P2P_THREE_NODES}" == "1" ]] && lsof -nP -iUDP:"${port}" >/dev/null 2>&1; then
      echo "error: UDP port ${port} already in use; override SOVA_P2P_SIM_* ports" >&2
      exit 1
    fi
  done
  if [[ ! -x "${SOVA_BIN}" ]]; then
    echo "--- building bin/sova (debug) ---"
    (cd "${ROOT}" && cargo build -p sova --quiet) || exit 1
  fi
  if [[ ! -x "${MINER_BIN}" ]]; then
    echo "--- building sova-miner (release) ---"
    (cd "${BURN_WALLET_DIR}" && cargo build --release -p sova-miner --quiet) || exit 1
  fi
}

start_stack() {
  echo "--- starting zebrad regtest (project ${COMPOSE_PROJECT}, container ${ZEBRAD_CONTAINER}, :${ZEBRAD_PORT}) ---"
  compose up -d || exit 1
  STARTED_STACK=1
  local deadline=$((SECONDS + 120))
  until [[ "$(docker inspect -f '{{.State.Health.Status}}' "${ZEBRAD_CONTAINER}" 2>/dev/null)" == "healthy" ]]; do
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "error: zebrad RPC did not become healthy within 120s" >&2
      exit 1
    fi
    sleep 2
  done
  echo "zebrad RPC is healthy"
  WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/${WORK_PREFIX}.XXXXXX")"
  echo "--- work dir: ${WORK_DIR} ---"
}

# Common p2p node env: sova/1 transport, no SOVA_AUTH_JWT (each node's
# authrpc keeps reth's own per-datadir secret, bound to 127.0.0.1), no
# SOVA_PEERS (the relay transport is not in play). Always invoked as
# `p2p_env ... &`: the `exec` makes the backgrounded subshell BECOME the
# node, so `$!` is the node's own PID and teardown can kill it.
p2p_env() {
  exec env -u SOVA_AUTH_JWT -u SOVA_PEERS SOVA_GOSSIP=p2p "$@"
}

# Assert a node runs sova/1 with authrpc on loopback and no relay.
check_p2p_node_log() {
  local label="$1" log="$2"
  local clean
  clean="$(strip_ansi "${log}")"
  if grep -q "p2p: sova/1 gossip enabled" <<<"${clean}" \
    && grep -q "authrpc) bound to 127.0.0.1:" <<<"${clean}"; then
    pass "setup: ${label} runs sova/1; authrpc bound to 127.0.0.1"
  else
    fail "setup: ${label} log lacks the sova/1 / loopback-authrpc lines"
  fi
  if grep -q "relay: pushing sealed blocks" <<<"${clean}"; then
    fail "setup: ${label} started the authrpc relay in p2p mode"
  fi
}

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
