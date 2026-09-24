#!/usr/bin/env bash
# Client checkpoints on the real multi-node join path (audit 2026-09-23
# F2, measure B; crates/engine/src/checkpoints.rs,
# docs/design/f2-join-and-restart.md §B). Operators pin history with
# SOVA_CHECKPOINTS=height:0xhash,...; the node must accept no block at a
# checkpoint height other than the listed one (SovaConsensus::
# validate_header, every import path), never backfill toward a target that
# contradicts one (candidates::actionable_target), and refuse to start on
# a datadir whose stored chain contradicts one (bin/sova startup check).
#
#   node A  -- mine mode, the only producer, sova/1, no static peers. Mines
#              alone (empty epochs, no burn needed) until its tip is
#              GROW_TO; the Zcash chain is then frozen so both joiners face
#              the same gap (> sova/1's 33-block chase window: the joins
#              go through the sync driver and the engine's download/
#              backfill, like join-scenario.sh).
#   node J1 -- follow-only, C5-enforcing, PERSISTENT datadir (SOVA_DATADIR),
#              static peer A, SOVA_CHECKPOINTS=<CP_HEIGHT>:<A's real hash>.
#              p2p-common's "B" slot.
#   node J2 -- follow-only, C5-enforcing, empty datadir, static peer A,
#              SOVA_CHECKPOINTS=<CP_HEIGHT>:<a hash A never produced>.
#              p2p-common's "C" slot. Started together with J1.
# Auto-mine resumes once both sessions are up, so A keeps growing.
#
# Assertions:
#   (0) setup   -- A, J1, J2 run sova/1 (authrpc loopback, no relay); J1
#                  and J2 enforce C5 and log their installed checkpoint;
#                  the gap is > the chase window; sessions are up.
#   (1) J1 (correct checkpoint) catches up to A within JOIN_TIMEOUT_S and
#       tracks it (lag <= 1); identical hashes on A and J1 at 1, CP_HEIGHT,
#       a middle height and the tip; J1's block at CP_HEIGHT is the
#       checkpointed hash; J1 logs no checkpoint mismatch.
#   (2) J2 (wrong checkpoint), observed for STALL_OBSERVE_S after J1 caught
#       up: its head never reaches CP_HEIGHT (sampled every second, max
#       reported); it has no block at CP_HEIGHT; it logs
#       "checkpoint mismatch at height <CP_HEIGHT>" (J2 runs with
#       downloaders::headers at trace: on the backfill path reth logs a
#       header validation failure ONLY at trace, so at the default level
#       the stall is silent -- see box/sim/README.md); eth_syncing never
#       answers false while its head is below CP_HEIGHT (no false
#       "synced"; J1 answering false once caught up is the control); it is
#       still alive and answering RPC; no panic.
#   (c) control -- J2 stopped (SIGTERM) and restarted on the same slot,
#       empty datadir, WITHOUT the checkpoint: it syncs to A's head and
#       its block at CP_HEIGHT is A's. So (2)'s stall is the checkpoint,
#       not the slot, the peer or the gap.
#   (3) J1 stopped (SIGTERM; bin/sova installs no signal handler, so this
#       is the default termination) and restarted on its datadir with
#       SOVA_CHECKPOINTS=<RESTART_HEIGHT>:<wrong hash>, RESTART_HEIGHT a
#       height it stores: it exits nonzero within RESTART_EXIT_S, logging
#       "this datadir follows another history: block <RESTART_HEIGHT> is".
#   (3b) control -- the same datadir restarted with the CORRECT checkpoint
#       starts, still stores A's block at RESTART_HEIGHT, and follows A
#       again (lag <= 1).
#
# Env: GROW_TO (70), CP_HEIGHT (40), RESTART_HEIGHT (20), JOIN_TIMEOUT_S
# (240), STALL_OBSERVE_S (45), RESTART_EXIT_S (120), plus p2p-common.sh's
# SOVA_P2P_SIM_*, SOVA_BIN, SOVA_MINER_BIN, SOVA_P2P_SIM_KEEP_LOGS.
#
# Isolation: zebrad on :18372 (compose project sova-checkpoint-sim,
# container sova-zebrad-checkpoint), A on 10245/10251/30911, J1 on
# 10345/10351/30912, J2 on 10445/10451/30913. All overridable.

SCENARIO="checkpoint"
WORK_PREFIX="sova-checkpoint"
P2P_THREE_NODES=1
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18372}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-checkpoint}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-checkpoint-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=10245 10251 30911}"
: "${SOVA_P2P_SIM_B_PORTS:=10345 10351 30912}"
: "${SOVA_P2P_SIM_C_PORTS:=10445 10451 30913}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS SOVA_P2P_SIM_C_PORTS
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"
set -euo pipefail
trap cleanup EXIT

GROW_TO="${GROW_TO:-70}"
CP_HEIGHT="${CP_HEIGHT:-40}"
RESTART_HEIGHT="${RESTART_HEIGHT:-20}"
JOIN_TIMEOUT_S="${JOIN_TIMEOUT_S:-240}"
STALL_OBSERVE_S="${STALL_OBSERVE_S:-45}"
RESTART_EXIT_S="${RESTART_EXIT_S:-120}"
# J2's log filter: the default plus the one place the backfill path says
# why it rejected a header (reverse_headers.rs: trace "Failed to validate
# header", carrying the consensus error).
J2_RUST_LOG="${SOVA_CHECKPOINT_J2_RUST_LOG:-info,downloaders::headers=trace}"
AUTO_MINE_INTERVAL_S=2
GEN_CHUNK=25
# sova/1's reach from an empty head (crates/engine/src/p2p/service.rs).
CHASE_WINDOW=33
# A 32-byte hash no chain here produces.
WRONG_HASH="0x$(printf '5e%.0s' {1..32})"

for v in GROW_TO CP_HEIGHT RESTART_HEIGHT; do
  if ! [[ "${!v}" =~ ^[0-9]+$ ]] || [[ "${!v}" -lt 1 ]]; then
    echo "error: ${v} must be a positive integer, got ${!v}" >&2
    exit 2
  fi
done
if [[ "${CP_HEIGHT}" -ge "${GROW_TO}" || "${RESTART_HEIGHT}" -ge "${GROW_TO}" ]]; then
  echo "error: CP_HEIGHT (${CP_HEIGHT}) and RESTART_HEIGHT (${RESTART_HEIGHT}) must be below GROW_TO (${GROW_TO})" >&2
  exit 2
fi

ENGINE_RPC_J1="${ENGINE_RPC_B}"
ENGINE_RPC_J2="${ENGINE_RPC_C}"
LOG_A="" # set once WORK_DIR exists
LOG_J1=""
LOG_J2=""

# --- helpers (all safe under set -euo pipefail) -------------------------

# Height, or 0 when the RPC is down/unparseable (never aborts the script).
height_of() { eth_block_number "$1" 2>/dev/null || echo 0; }

# Count lines in a (de-ANSI'd) log matching a fixed string. grep -c reads
# to EOF (no SIGPIPE), and `|| true` covers "0 matches" under pipefail.
count_in() {
  local n
  n="$(sed 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | grep -cF -- "$2" || true)"
  echo "${n:-0}"
}

# Lines of a (de-ANSI'd) log matching a fixed string; empty if none.
lines_in() { sed 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | grep -F -- "$2" || true; }

stop_auto_mine() {
  if [[ -n "${AUTO_MINE_PID}" ]] && kill -0 "${AUTO_MINE_PID}" 2>/dev/null; then
    kill "${AUTO_MINE_PID}" 2>/dev/null || true
    wait "${AUTO_MINE_PID}" 2>/dev/null || true
  fi
  AUTO_MINE_PID=""
}

start_auto_mine() {
  "${REGTEST_DIR}/auto-mine.sh" "${AUTO_MINE_INTERVAL_S}" "${ZEBRAD_RPC}" >>"${WORK_DIR}/auto-mine.log" 2>&1 &
  AUTO_MINE_PID=$!
}

# Wait until $1 reports at least height $2 AND stops moving.
wait_for_stable_height() {
  local url="$1" target="$2" timeout_s="$3"
  local deadline=$((SECONDS + timeout_s)) cur prev=-1
  while :; do
    cur="$(height_of "${url}")"
    if [[ "${cur}" -ge "${target}" && "${cur}" -eq "${prev}" ]]; then
      echo "${cur}"
      return 0
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "${cur}"
      return 1
    fi
    prev="${cur}"
    sleep 2
  done
}

# eth_syncing as one word: "false" (the node says it is synced), "syncing"
# (a sync-status object), or "error" (no/unparseable answer).
syncing_of() {
  eth_rpc "$1" eth_syncing "[]" 2>/dev/null | python3 -c "
import sys, json
try:
    r = json.load(sys.stdin)['result']
    print('false' if r is False else 'syncing')
except Exception:
    print('error')
" 2>/dev/null || echo error
}

# SIGTERM a node and wait (up to 60s) for it to exit; SIGKILL after that.
stop_node() { # <pid> <label>
  local pid="$1" label="$2" deadline=$((SECONDS + 60))
  [[ -z "${pid}" ]] && return 0
  kill -TERM "${pid}" 2>/dev/null || true
  while kill -0 "${pid}" 2>/dev/null; do
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "WARN: ${label} (pid ${pid}) still alive 60s after SIGTERM; SIGKILL" >&2
      kill -KILL "${pid}" 2>/dev/null || true
      break
    fi
    sleep 1
  done
  wait "${pid}" 2>/dev/null || true
}

# Wait until $1 (a joiner) is at >= $2 and within 1 of A. Prints "A J".
wait_caught_up() { # <url> <min_height> <timeout_s> <label>
  local url="$1" min="$2" timeout_s="$3" label="$4"
  local deadline=$((SECONDS + timeout_s)) started=${SECONDS} last=${SECONDS} a j
  while :; do
    a="$(height_of "${ENGINE_RPC_A}")"
    j="$(height_of "${url}")"
    if [[ "${j}" -ge "${min}" && $((a - j)) -le 1 ]]; then
      echo "${a} ${j}"
      return 0
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "${a} ${j}"
      return 1
    fi
    if [[ $((SECONDS - last)) -ge 20 ]]; then
      echo "  ${label} t+$((SECONDS - started))s: A=${a} ${label}=${j}" >&2
      last=${SECONDS}
    fi
    sleep 1
  done
}

# Start a follow-only joiner in the background; sets JOIN_PID. Extra env
# (SOVA_CHECKPOINTS, SOVA_DATADIR) comes after the slot arguments.
start_joiner() { # <http> <auth> <p2p> <log> [VAR=value ...]
  local http="$1" auth="$2" p2p="$3" log="$4"
  shift 4
  p2p_env \
    SOVA_FOLLOW_ONLY=1 \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
    SOVA_EPOCH_BASE=1 \
    SOVA_P2P_PEERS="${ENODE_A}" \
    SOVA_HTTP_PORT="${http}" \
    SOVA_AUTH_PORT="${auth}" \
    SOVA_P2P_PORT="${p2p}" \
    "$@" \
    "${SOVA_BIN}" >"${log}" 2>&1 &
  JOIN_PID=$!
}

preflight
start_stack
LOG_A="${WORK_DIR}/node-a.log"
LOG_J1="${WORK_DIR}/node-j1.log"
LOG_J2="${WORK_DIR}/node-j2.log"

# ---------------------------------------------------------------------
# Miner identity (A's EVM address; the t-addr receives regtest coinbase).
# ---------------------------------------------------------------------
MINER_DATA_DIR="${WORK_DIR}/miner"
mkdir -p "${MINER_DATA_DIR}"
"${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest init >"${WORK_DIR}/miner-init.log" 2>&1
TADDR="$(awk '/t-addr to fund/ {print $NF}' "${WORK_DIR}/miner-init.log")"
EVM_ADDR="$(awk '/evm address/ {print $NF}' "${WORK_DIR}/miner-init.log")"
if [[ -z "${TADDR}" || -z "${EVM_ADDR}" ]]; then
  fail "setup: could not parse miner identity"
  cat "${WORK_DIR}/miner-init.log" >&2
  exit 1
fi
echo "miner identity: ${TADDR} / ${EVM_ADDR}"

# ---------------------------------------------------------------------
# Node A: mine mode, alone, grown to GROW_TO, then frozen.
# ---------------------------------------------------------------------
echo "--- starting node A (mine mode, sova/1, no static peers) ---"
p2p_env \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" \
  SOVA_EPOCH_BASE=1 \
  SOVA_HTTP_PORT="${A_HTTP_PORT}" \
  SOVA_AUTH_PORT="${A_AUTH_PORT}" \
  SOVA_P2P_PORT="${A_P2P_PORT}" \
  "${SOVA_BIN}" >"${LOG_A}" 2>&1 &
A_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_A}" "${A_PID}" "node A" || exit 1
ENODE_A="$(local_enode "${LOG_A}")" || {
  fail "setup: node A never printed its enode"
  exit 1
}
echo "node A up (pid ${A_PID}); enode ${ENODE_A}"

echo "--- growing A to Sova height ${GROW_TO} (Sova h <-> Zcash h; epoch base 1) ---"
NEED=$((GROW_TO - $(zc_tip_height)))
while [[ "${NEED}" -gt 0 ]]; do
  n=$((NEED < GEN_CHUNK ? NEED : GEN_CHUNK))
  zc_generate_to_address "${n}" "${TADDR}"
  NEED=$((NEED - n))
  wait_for_block_number "${ENGINE_RPC_A}" "$(zc_tip_height)" 90 || true
done
if ! TIP_AT_JOIN="$(wait_for_stable_height "${ENGINE_RPC_A}" "${GROW_TO}" 120)"; then
  fail "setup: node A did not settle at >= ${GROW_TO} (at ${TIP_AT_JOIN})"
  exit 1
fi
echo "A frozen at Sova ${TIP_AT_JOIN} (Zcash $(zc_tip_height))"

CP_HASH="$(eth_block_hash "${ENGINE_RPC_A}" "${CP_HEIGHT}" || true)"
RESTART_HASH="$(eth_block_hash "${ENGINE_RPC_A}" "${RESTART_HEIGHT}" || true)"
if [[ -z "${CP_HASH}" || -z "${RESTART_HASH}" ]]; then
  fail "setup: A has no block at ${CP_HEIGHT} or ${RESTART_HEIGHT}"
  exit 1
fi
if [[ "${CP_HASH}" == "${WRONG_HASH}" ]]; then
  fail "setup: A's block ${CP_HEIGHT} is the 'wrong' hash (impossible)"
  exit 1
fi
echo "checkpoint: height ${CP_HEIGHT} = ${CP_HASH} (A's real block); wrong hash ${WRONG_HASH}"

# ---------------------------------------------------------------------
# J1 (correct checkpoint, persistent datadir) and J2 (wrong checkpoint).
# ---------------------------------------------------------------------
J1_DATADIR="${WORK_DIR}/j1-datadir"
echo "--- starting J1 (follow-only, datadir ${J1_DATADIR}, SOVA_CHECKPOINTS=${CP_HEIGHT}:<A's hash>) ---"
start_joiner "${B_HTTP_PORT}" "${B_AUTH_PORT}" "${B_P2P_PORT}" "${LOG_J1}" \
  SOVA_DATADIR="${J1_DATADIR}" SOVA_CHECKPOINTS="${CP_HEIGHT}:${CP_HASH}"
B_PID="${JOIN_PID}"
echo "--- starting J2 (follow-only, empty datadir, SOVA_CHECKPOINTS=${CP_HEIGHT}:<wrong hash>; RUST_LOG=${J2_RUST_LOG}) ---"
start_joiner "${C_HTTP_PORT}" "${C_AUTH_PORT}" "${C_P2P_PORT}" "${LOG_J2}" \
  SOVA_CHECKPOINTS="${CP_HEIGHT}:${WRONG_HASH}" RUST_LOG="${J2_RUST_LOG}"
C_PID="${JOIN_PID}"
JOINERS_STARTED_AT=${SECONDS}
wait_for_eth_rpc "${ENGINE_RPC_J1}" "${B_PID}" "J1" || exit 1
wait_for_eth_rpc "${ENGINE_RPC_J2}" "${C_PID}" "J2" || exit 1
echo "J1 up (pid ${B_PID}), J2 up (pid ${C_PID})"

echo ""
echo "=== (0) setup ==="
check_p2p_node_log "node A" "${LOG_A}"
check_p2p_node_log "J1" "${LOG_J1}"
check_p2p_node_log "J2" "${LOG_J2}"
for pair in "J1:${LOG_J1}" "J2:${LOG_J2}"; do
  label="${pair%%:*}"
  log="${pair#*:}"
  if log_has "${log}" "expectations: enforcing settlements" 15; then
    pass "setup: ${label} enforces C5 against its own zebrad view"
  else
    fail "setup: ${label} isn't enforcing C5"
  fi
  if log_has "${log}" "checkpoints: 1 (newest at height ${CP_HEIGHT})" 15; then
    pass "setup: ${label} installed its checkpoint at height ${CP_HEIGHT}"
  else
    fail "setup: ${label} did not log 'checkpoints: 1 (newest at height ${CP_HEIGHT})'"
  fi
done
if [[ "${TIP_AT_JOIN}" -gt "${CHASE_WINDOW}" ]]; then
  pass "setup: join gap ${TIP_AT_JOIN} > sova/1 chase window ${CHASE_WINDOW} (sync driver + engine download/backfill)"
else
  fail "setup: join gap ${TIP_AT_JOIN} <= chase window ${CHASE_WINDOW}; raise GROW_TO"
fi
if wait_for_sova_peer "${LOG_J1}" 60 && wait_for_sova_peer "${LOG_J2}" 60; then
  pass "setup: sova/1 sessions J1<->A and J2<->A established"
else
  fail "setup: a joiner has no sova/1 session with A within 60s"
  exit 1
fi

echo "--- resuming auto-mine (A keeps growing while the joiners sync) ---"
start_auto_mine

# ============================================================
# (1) J1: correct checkpoint -> syncs and agrees
# ============================================================
echo ""
echo "=== (1) J1 (correct checkpoint) catches up within ${JOIN_TIMEOUT_S}s ==="
J2_MAX=0
if HEADS="$(wait_caught_up "${ENGINE_RPC_J1}" "${TIP_AT_JOIN}" "${JOIN_TIMEOUT_S}" J1)"; then
  read -r A_BLOCK J1_BLOCK <<<"${HEADS}"
  pass "(1) J1 caught up: A=${A_BLOCK} J1=${J1_BLOCK}, $((SECONDS - JOINERS_STARTED_AT))s after the joiners started"
  J1_CAUGHT_UP=1
else
  read -r A_BLOCK J1_BLOCK <<<"${HEADS}"
  fail "(1) J1 did NOT catch up within ${JOIN_TIMEOUT_S}s: A=${A_BLOCK} J1=${J1_BLOCK} (gap ${TIP_AT_JOIN})"
  J1_CAUGHT_UP=0
fi
J2_NOW="$(height_of "${ENGINE_RPC_J2}")"
[[ "${J2_NOW}" -gt "${J2_MAX}" ]] && J2_MAX="${J2_NOW}"
if [[ "${J1_CAUGHT_UP}" -eq 1 ]]; then
  ADVANCE_DEADLINE=$((SECONDS + 60))
  until [[ "$(height_of "${ENGINE_RPC_A}")" -gt "${TIP_AT_JOIN}" ]] || ((SECONDS >= ADVANCE_DEADLINE)); do
    sleep 1
  done
  for i in 1 2 3; do
    A_BLOCK="$(height_of "${ENGINE_RPC_A}")"
    J1_BLOCK="$(height_of "${ENGINE_RPC_J1}")"
    LAG=$((A_BLOCK - J1_BLOCK))
    if [[ "${LAG}" -ge 0 && "${LAG}" -le 1 && "${A_BLOCK}" -gt "${TIP_AT_JOIN}" ]]; then
      pass "(1) lockstep sample ${i}/3: A=${A_BLOCK} J1=${J1_BLOCK} lag=${LAG}"
    else
      fail "(1) lockstep sample ${i}/3: A=${A_BLOCK} J1=${J1_BLOCK} lag=${LAG} -- outside [0,1] (or A not past the join tip)"
    fi
    if [[ ${i} -lt 3 ]]; then sleep 5; fi
  done
fi
TIP_A="$(height_of "${ENGINE_RPC_A}")"
wait_for_block_number "${ENGINE_RPC_J1}" "${TIP_A}" 30 || true
MID_HEIGHT=$(((CP_HEIGHT + TIP_AT_JOIN) / 2))
for h in $(printf '%s\n' 1 "${RESTART_HEIGHT}" "${CP_HEIGHT}" "${MID_HEIGHT}" "${TIP_A}" | sort -n | uniq); do
  HASH_A="$(eth_block_hash "${ENGINE_RPC_A}" "${h}" || true)"
  HASH_J1="$(eth_block_hash "${ENGINE_RPC_J1}" "${h}" || true)"
  if [[ -n "${HASH_A}" && "${HASH_A}" == "${HASH_J1}" ]]; then
    pass "(1) height ${h}: identical on A and J1 (${HASH_A})"
  else
    fail "(1) height ${h}: A=${HASH_A:-<none>} J1=${HASH_J1:-<none>}"
  fi
done
# Control for (2)'s eth_syncing check: once J1 follows live blocks it
# answers false. (Right after a backfill it still answers "syncing": reth
# flips the network's sync state to idle on the first live canonical head,
# node/builder/src/launch/engine.rs.)
J1_SYNCING="$(syncing_of "${ENGINE_RPC_J1}")"
if [[ "${J1_SYNCING}" == "false" ]]; then
  pass "(1) J1 (caught up, following live blocks) answers eth_syncing=false"
else
  fail "(1) J1 (caught up, following live blocks) answers eth_syncing=${J1_SYNCING}"
fi
J1_CP_HASH="$(eth_block_hash "${ENGINE_RPC_J1}" "${CP_HEIGHT}" || true)"
if [[ "${J1_CP_HASH}" == "${CP_HASH}" ]]; then
  pass "(1) J1's block ${CP_HEIGHT} is the checkpointed hash"
else
  fail "(1) J1's block ${CP_HEIGHT} is ${J1_CP_HASH:-<none>}, checkpoint ${CP_HASH}"
fi
N_MISMATCH_J1="$(count_in "${LOG_J1}" 'checkpoint mismatch')"
if [[ "${N_MISMATCH_J1}" -eq 0 ]]; then
  pass "(1) J1 logged no checkpoint mismatch"
else
  fail "(1) J1 logged ${N_MISMATCH_J1} checkpoint mismatch line(s) on the checkpointed history"
fi

# ============================================================
# (2) J2: wrong checkpoint -> never imports CP_HEIGHT, says why, stays up
# ============================================================
echo ""
echo "=== (2) J2 (wrong checkpoint) observed for ${STALL_OBSERVE_S}s more ==="
OBSERVE_END=$((SECONDS + STALL_OBSERVE_S))
J2_SAMPLES=0
J2_SAID_SYNCED=0
while [[ ${SECONDS} -lt ${OBSERVE_END} ]]; do
  J2_NOW="$(height_of "${ENGINE_RPC_J2}")"
  [[ "${J2_NOW}" -gt "${J2_MAX}" ]] && J2_MAX="${J2_NOW}"
  J2_SAMPLES=$((J2_SAMPLES + 1))
  # "synced" while below the checkpoint (and so far behind A) is false.
  if [[ "$(syncing_of "${ENGINE_RPC_J2}")" == "false" && "${J2_NOW}" -lt "${CP_HEIGHT}" ]]; then
    J2_SAID_SYNCED=$((J2_SAID_SYNCED + 1))
  fi
  sleep 1
done
A_NOW="$(height_of "${ENGINE_RPC_A}")"
J2_NOW="$(height_of "${ENGINE_RPC_J2}")"
[[ "${J2_NOW}" -gt "${J2_MAX}" ]] && J2_MAX="${J2_NOW}"
echo "  J2 head ${J2_NOW} (max seen ${J2_MAX}); A head ${A_NOW}; $((SECONDS - JOINERS_STARTED_AT))s since the joiners started"
if [[ "${J2_MAX}" -lt "${CP_HEIGHT}" ]]; then
  pass "(2) J2's head never reached the checkpoint height ${CP_HEIGHT} (max ${J2_MAX}; A at ${A_NOW})"
else
  fail "(2) J2's head reached ${J2_MAX} >= checkpoint height ${CP_HEIGHT}"
fi
J2_CP_HASH="$(eth_block_hash "${ENGINE_RPC_J2}" "${CP_HEIGHT}" || true)"
if [[ -z "${J2_CP_HASH}" ]]; then
  pass "(2) J2 has no block at height ${CP_HEIGHT}"
else
  fail "(2) J2 has block ${J2_CP_HASH} at height ${CP_HEIGHT} (checkpoint names ${WRONG_HASH})"
fi
if log_has "${LOG_J2}" "checkpoint mismatch at height ${CP_HEIGHT}" 15; then
  pass "(2) J2 logged: $(lines_in "${LOG_J2}" "checkpoint mismatch at height ${CP_HEIGHT}" | sed -n 1p | grep -o 'checkpoint mismatch at height [0-9]*: block is 0x[0-9a-f]*' || true)"
else
  fail "(2) J2 never logged 'checkpoint mismatch at height ${CP_HEIGHT}'"
fi
N_MM_ALL="$(count_in "${LOG_J2}" "checkpoint mismatch at height ${CP_HEIGHT}")"
N_MM_VISIBLE="$(lines_in "${LOG_J2}" "checkpoint mismatch at height ${CP_HEIGHT}" | grep -cvE ' (TRACE|DEBUG) ' || true)"
echo "  J2 'checkpoint mismatch at height ${CP_HEIGHT}' lines: ${N_MM_ALL} (above DEBUG, i.e. visible at the default log level: ${N_MM_VISIBLE:-0})"
if [[ "${J2_SAID_SYNCED}" -eq 0 ]]; then
  pass "(2) J2 never answered eth_syncing=false while below ${CP_HEIGHT} (${J2_SAMPLES} samples; no false synced)"
else
  fail "(2) J2 answered eth_syncing=false in ${J2_SAID_SYNCED}/${J2_SAMPLES} samples while below ${CP_HEIGHT} (false synced)"
fi
if kill -0 "${C_PID}" 2>/dev/null && [[ -n "$(eth_block_hash "${ENGINE_RPC_J2}" 0 || true)" ]]; then
  pass "(2) J2 is still running and answering RPC"
else
  fail "(2) J2 exited or stopped answering RPC"
fi
N_PANIC_J2="$(count_in "${LOG_J2}" 'panicked at')"
if [[ "${N_PANIC_J2}" -eq 0 ]]; then
  pass "(2) J2 did not panic"
else
  fail "(2) J2 panicked (${N_PANIC_J2} line(s))"
fi
echo "  J2 diagnostics: 'catching up to sync target' $(count_in "${LOG_J2}" 'catching up to sync target'), 'reputation hit' $(count_in "${LOG_J2}" 'reputation hit'), 'peer block accepted' $(count_in "${LOG_J2}" 'sova/1: peer block accepted')"

# ============================================================
# (c) control: J2's slot, no checkpoint -> syncs
# ============================================================
echo ""
echo "=== (c) control: J2 restarted WITHOUT the checkpoint (empty datadir) ==="
stop_node "${C_PID}" "J2"
C_PID=""
LOG_J2C="${WORK_DIR}/node-j2-control.log"
start_joiner "${C_HTTP_PORT}" "${C_AUTH_PORT}" "${C_P2P_PORT}" "${LOG_J2C}"
C_PID="${JOIN_PID}"
wait_for_eth_rpc "${ENGINE_RPC_J2}" "${C_PID}" "J2 (control)" || exit 1
CONTROL_STARTED_AT=${SECONDS}
if HEADS="$(wait_caught_up "${ENGINE_RPC_J2}" "$(height_of "${ENGINE_RPC_A}")" "${JOIN_TIMEOUT_S}" J2c)"; then
  read -r A_BLOCK J2_BLOCK <<<"${HEADS}"
  pass "(c) J2 without the checkpoint caught up: A=${A_BLOCK} J2=${J2_BLOCK}, $((SECONDS - CONTROL_STARTED_AT))s"
else
  read -r A_BLOCK J2_BLOCK <<<"${HEADS}"
  fail "(c) J2 without the checkpoint did NOT catch up within ${JOIN_TIMEOUT_S}s: A=${A_BLOCK} J2=${J2_BLOCK}"
fi
J2C_CP_HASH="$(eth_block_hash "${ENGINE_RPC_J2}" "${CP_HEIGHT}" || true)"
if [[ "${J2C_CP_HASH}" == "${CP_HASH}" ]]; then
  pass "(c) J2 (no checkpoint) imported A's block ${CP_HEIGHT}"
else
  fail "(c) J2 (no checkpoint) has ${J2C_CP_HASH:-<none>} at ${CP_HEIGHT}, A has ${CP_HASH}"
fi

# ============================================================
# (3) J1 restarted with a checkpoint its datadir contradicts
# ============================================================
echo ""
echo "=== (3) J1 restarted with a contradicting checkpoint at stored height ${RESTART_HEIGHT} ==="
J1_HEAD_BEFORE="$(height_of "${ENGINE_RPC_J1}")"
stop_node "${B_PID}" "J1"
B_PID=""
LOG_J1R="${WORK_DIR}/node-j1-restart-wrong.log"
start_joiner "${B_HTTP_PORT}" "${B_AUTH_PORT}" "${B_P2P_PORT}" "${LOG_J1R}" \
  SOVA_DATADIR="${J1_DATADIR}" SOVA_CHECKPOINTS="${RESTART_HEIGHT}:${WRONG_HASH}"
B_PID="${JOIN_PID}"
EXIT_DEADLINE=$((SECONDS + RESTART_EXIT_S))
while kill -0 "${B_PID}" 2>/dev/null && [[ ${SECONDS} -lt ${EXIT_DEADLINE} ]]; do
  sleep 1
done
if kill -0 "${B_PID}" 2>/dev/null; then
  fail "(3) J1 with a contradicting checkpoint is still running ${RESTART_EXIT_S}s after start (head $(height_of "${ENGINE_RPC_J1}"))"
  stop_node "${B_PID}" "J1 (contradicting checkpoint)"
else
  RC=0
  wait "${B_PID}" || RC=$?
  if [[ "${RC}" -ne 0 ]]; then
    pass "(3) J1 refused to start (exit ${RC}) on a datadir whose block ${RESTART_HEIGHT} contradicts the checkpoint"
  else
    fail "(3) J1 exited 0 with a contradicting checkpoint"
  fi
fi
B_PID=""
if log_has "${LOG_J1R}" "this datadir follows another history: block ${RESTART_HEIGHT} is" 5; then
  pass "(3) J1 said: $(lines_in "${LOG_J1R}" 'this datadir follows another history' | sed -n 1p | sed 's/^.*\(this datadir\)/\1/' | cut -c1-120)..."
else
  fail "(3) J1's log lacks 'this datadir follows another history: block ${RESTART_HEIGHT} is'"
  tail -n 20 "${LOG_J1R}" >&2 || true
fi

echo ""
echo "=== (3b) control: the same datadir with the CORRECT checkpoint ==="
LOG_J1OK="${WORK_DIR}/node-j1-restart-ok.log"
start_joiner "${B_HTTP_PORT}" "${B_AUTH_PORT}" "${B_P2P_PORT}" "${LOG_J1OK}" \
  SOVA_DATADIR="${J1_DATADIR}" SOVA_CHECKPOINTS="${CP_HEIGHT}:${CP_HASH},${RESTART_HEIGHT}:${RESTART_HASH}"
B_PID="${JOIN_PID}"
if wait_for_eth_rpc "${ENGINE_RPC_J1}" "${B_PID}" "J1 (correct checkpoints)"; then
  J1_HEAD_AFTER="$(height_of "${ENGINE_RPC_J1}")"
  echo "  J1 head before SIGTERM ${J1_HEAD_BEFORE}, after restart ${J1_HEAD_AFTER}"
  sleep 5
  if kill -0 "${B_PID}" 2>/dev/null \
    && [[ "$(eth_block_hash "${ENGINE_RPC_J1}" "${RESTART_HEIGHT}" || true)" == "${RESTART_HASH}" ]]; then
    pass "(3b) J1 restarted with correct checkpoints; its stored block ${RESTART_HEIGHT} is A's (head ${J1_HEAD_AFTER}, was ${J1_HEAD_BEFORE})"
  else
    fail "(3b) J1 with correct checkpoints is not running or lost block ${RESTART_HEIGHT}"
  fi
  if HEADS="$(wait_caught_up "${ENGINE_RPC_J1}" "${J1_HEAD_BEFORE}" 120 J1)"; then
    pass "(3b) J1 follows A again after the restart (A J1 = ${HEADS})"
  else
    fail "(3b) J1 did not rejoin A within 120s (A J1 = ${HEADS})"
  fi
fi

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "CHECKPOINT SCENARIO PASSED (checkpoint ${CP_HEIGHT}, restart check ${RESTART_HEIGHT}, gap ${TIP_AT_JOIN}; all assertions)"
else
  echo "CHECKPOINT SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
