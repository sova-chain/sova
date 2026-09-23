#!/usr/bin/env bash
# sova/1: a LATE JOINER catches up from an empty datadir (board m1-b;
# docs/design/p2p-m1.md Decision 2 "history sync waits for the follower"
# and the build order's "a fresh node joins ... 100+ blocks in").
#
# Acceptance test for late-join catch-up. At the default depth it is
# EXPECTED TO FAIL until the arbiter's catch-up lands: sova/1 on its own
# only chases MAX_ANCESTOR_DEPTH (32) ancestors of an announced block,
# refuses to even fetch an announcement more than 33 blocks above the
# local head ("peer is beyond p2p catch-up range"), and nothing triggers
# reth's backfill. Run with JOIN_DEPTH=20 it stays inside the chase
# window and is the control that passes today.
#
#   node A -- mine mode (the only producer), sova/1, no static peers.
#             Mines alone: one early burn-bearing settled epoch (within
#             the first EARLY_LIMIT Sova heights), then empty epochs until
#             its tip is JOIN_DEPTH blocks past the settled height. The
#             Zcash chain is then FROZEN (auto-mine off) so the gap J faces
#             at its first sova/1 greeting is exact.
#   node J -- follow-only, C5-enforcing against the same regtest zebrad,
#             EMPTY datadir (reth's testing_node tempdir), sova/1 static
#             peer = A (dev profile: discovery off, so SOVA_P2P_PEERS is
#             the only way in -- same as two-node-p2p-scenario.sh). J
#             starts after all of A's history exists; auto-mine resumes
#             once J's session is up, so A keeps growing while J catches
#             up.
#
# Why SOVA_CHAIN=dev (like the other p2p sims) and not sova-testnet:
# catch-up is a sova/1 + engine-tree/backfill property, independent of
# the chain profile, and the discovery machinery sova-testnet switches
# on is covered by three-node-discovery-scenario.sh. Keeping discovery
# off makes A the only peer J can possibly sync from, so a pass cannot
# be explained by some other node.
#
# Assertions:
#   (0) setup     -- both nodes run sova/1 (authrpc loopback, no relay);
#                    J enforces C5; the settled epoch is early; the gap J
#                    faces is what JOIN_DEPTH asked for; J's session is up.
#   (a) catch-up  -- J reaches A's height within JOIN_TIMEOUT_S (180s),
#                    then lag <= 1 on 3 samples 5s apart.
#   (b) hashes    -- identical on A and J at height 1, the early settled
#                    height, a middle height, and the tip.
#   (c) balance   -- the miner's minted balance identical on A and J.
#   (d) C5 on the historical settled block -- J's settlement check was
#                    ENFORCED (not deferred / accepted on trust) at the
#                    settled height. The node logs nothing on a successful
#                    enforcement, so this uses J's debug log: J runs with
#                    engine::consensus/engine::validator at debug, and the
#                    only lines those modules emit for a height are the
#                    deferral/trust ones. Any of them at a height <= the
#                    join tip is a FAIL; if the node ever emits a positive
#                    line matching $ENFORCED_PATTERN at the settled height,
#                    that is used instead (see README: proposed log line).
#   (e) no reputation hits (and no INVALID peer blocks) on A or J.
# On a catch-up failure the script prints a STALL diagnosis from J's log.
#
# Env: JOIN_DEPTH (default 150), JOIN_TIMEOUT_S (default 180),
# EARLY_LIMIT (default 20), plus p2p-common.sh's SOVA_P2P_SIM_*,
# SOVA_BIN, SOVA_MINER_BIN, SOVA_P2P_SIM_KEEP_LOGS.
#
# Isolation: zebrad on :18342 (compose project sova-join-sim, container
# sova-zebrad-join), A on 9245/9251/30611, J on 9345/9351/30612 (J uses
# p2p-common's "B" slot). All overridable.

SCENARIO="join"
WORK_PREFIX="sova-join"
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18342}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-join}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-join-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=9245 9251 30611}"
: "${SOVA_P2P_SIM_B_PORTS:=9345 9351 30612}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"
set -euo pipefail
trap cleanup EXIT

JOIN_DEPTH="${JOIN_DEPTH:-150}"
JOIN_TIMEOUT_S="${JOIN_TIMEOUT_S:-180}"
EARLY_LIMIT="${EARLY_LIMIT:-20}"
# A positive "C5 enforced" line, if the node ever logs one (it does not
# today). Must carry `height=<n>` on the same line.
ENFORCED_PATTERN="${ENFORCED_PATTERN:-settlement enforced}"
# J's log filter: reth's default plus the two modules whose debug lines
# say "this height was NOT enforced".
JOIN_RUST_LOG="${SOVA_JOIN_RUST_LOG:-info,engine::consensus=debug,engine::validator=debug}"
BUDGET_ZAT=200000
PER_EPOCH_ZAT=100000
FUND_BLOCKS=101
AUTO_MINE_INTERVAL_S=2
GEN_CHUNK=25
# sova/1's reach from an empty head: an announcement is fetched only if
# height <= local + MAX_ANCESTOR_DEPTH + 1 (crates/engine/src/p2p/service.rs).
CHASE_WINDOW=33

ENGINE_RPC_J="${ENGINE_RPC_B}"
J_LOG_NAME="node-j.log"

if ! [[ "${JOIN_DEPTH}" =~ ^[0-9]+$ ]] || [[ "${JOIN_DEPTH}" -lt 1 ]]; then
  echo "error: JOIN_DEPTH must be a positive integer, got ${JOIN_DEPTH}" >&2
  exit 2
fi

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

# Wait until $1 reports exactly-or-more height $2 AND stops moving (the
# Zcash chain is frozen, so A settles on a fixed tip).
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

preflight
start_stack

# ---------------------------------------------------------------------
# Miner identity; fund BEFORE A starts so the epoch base begins after the
# funding noise and the burn lands in the first few Sova heights.
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

echo "--- funding: ${FUND_BLOCKS} blocks to the miner's own address (coinbase maturity) ---"
zc_generate_to_address "${FUND_BLOCKS}" "${TADDR}"
FUND_TIP="$(zc_tip_height)"
EPOCH_BASE=$((FUND_TIP + 1))
echo "funding tip: ${FUND_TIP}; epoch base: ${EPOCH_BASE} (Sova height h <-> Zcash height ${EPOCH_BASE}+h-1)"

# ---------------------------------------------------------------------
# Node A: mine mode, alone.
# ---------------------------------------------------------------------
echo "--- starting node A (mine mode, sova/1, no static peers) ---"
p2p_env \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" \
  SOVA_EPOCH_BASE="${EPOCH_BASE}" \
  SOVA_HTTP_PORT="${A_HTTP_PORT}" \
  SOVA_AUTH_PORT="${A_AUTH_PORT}" \
  SOVA_P2P_PORT="${A_P2P_PORT}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-a.log" 2>&1 &
A_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_A}" "${A_PID}" "node A" || exit 1
ENODE_A="$(local_enode "${WORK_DIR}/node-a.log")" || {
  fail "setup: node A never printed its enode"
  exit 1
}
echo "node A up (pid ${A_PID}); enode ${ENODE_A}"

echo "--- auto-mine every ${AUTO_MINE_INTERVAL_S}s; one early burn ---"
start_auto_mine
if ! "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
  --budget-zat "${BUDGET_ZAT}" --per-epoch-zat "${PER_EPOCH_ZAT}" \
  --rpc "${ZEBRAD_RPC}" --max-epochs 1 >"${WORK_DIR}/mine-1.log" 2>&1; then
  fail "burn: sova-miner mine exited nonzero"
  cat "${WORK_DIR}/mine-1.log" >&2
fi
if ! BAL_A="$(wait_for_balance_change "${ENGINE_RPC_A}" "${EVM_ADDR}" "0" 120)"; then
  fail "setup: node A's balance never moved off 0 within 120s"
  exit 1
fi
echo "node A settled balance: ${BAL_A} wei"
# `sova epoch trigger height=<zcash> sova_height=<sova> settled=true`
SETTLED_HEIGHT="$(lines_in "${WORK_DIR}/node-a.log" 'sova epoch trigger' | grep 'settled=true' \
  | grep -o 'sova_height=[0-9]*' | sed -n '1s/^sova_height=//p' || true)"
if [[ -z "${SETTLED_HEIGHT}" ]]; then
  fail "setup: no settled trigger in node A's log"
  exit 1
fi
if [[ "${SETTLED_HEIGHT}" -le "${EARLY_LIMIT}" ]]; then
  pass "setup: burn-bearing settled epoch at Sova height ${SETTLED_HEIGHT} (early: <= ${EARLY_LIMIT})"
else
  fail "setup: settled epoch at Sova height ${SETTLED_HEIGHT}, not within the first ${EARLY_LIMIT}"
fi

# ---------------------------------------------------------------------
# Grow A alone to SETTLED + JOIN_DEPTH, then freeze the Zcash chain.
# ---------------------------------------------------------------------
stop_auto_mine
TARGET=$((SETTLED_HEIGHT + JOIN_DEPTH))
TARGET_ZC=$((EPOCH_BASE + TARGET - 1))
ZC_TIP="$(zc_tip_height)"
NEED=$((TARGET_ZC - ZC_TIP))
echo "--- growing A to Sova height ${TARGET} (JOIN_DEPTH=${JOIN_DEPTH} past settled ${SETTLED_HEIGHT}): ${NEED} more Zcash block(s) ---"
while [[ "${NEED}" -gt 0 ]]; do
  n=$((NEED < GEN_CHUNK ? NEED : GEN_CHUNK))
  zc_generate_to_address "${n}" "${TADDR}"
  NEED=$((NEED - n))
  # Let A keep pace chunk by chunk rather than face one huge burst.
  wait_for_block_number "${ENGINE_RPC_A}" $(($(zc_tip_height) - EPOCH_BASE + 1)) 60 || true
done
if ! TIP_AT_JOIN="$(wait_for_stable_height "${ENGINE_RPC_A}" "${TARGET}" $((60 + JOIN_DEPTH)))"; then
  fail "setup: node A did not settle at >= ${TARGET} (at ${TIP_AT_JOIN})"
  exit 1
fi
ZC_FROZEN="$(zc_tip_height)"
echo "A frozen at Sova ${TIP_AT_JOIN} (Zcash ${ZC_FROZEN}); J's gap from an empty head = ${TIP_AT_JOIN} (sova/1 chase window: ${CHASE_WINDOW})"
if [[ "${TIP_AT_JOIN}" -gt "${CHASE_WINDOW}" ]]; then
  echo "NOTE: gap ${TIP_AT_JOIN} > ${CHASE_WINDOW}: sova/1 alone cannot close it; this run exercises catch-up/backfill"
else
  echo "NOTE: gap ${TIP_AT_JOIN} <= ${CHASE_WINDOW}: inside sova/1's ancestor chase (control run)"
fi
MID_HEIGHT=$(((SETTLED_HEIGHT + TIP_AT_JOIN) / 2))

# ---------------------------------------------------------------------
# Node J: follow-only, empty datadir, static peer = A.
# ---------------------------------------------------------------------
echo "--- starting node J (follow-only, EMPTY datadir, sova/1 static peer = A; RUST_LOG=${JOIN_RUST_LOG}) ---"
J_STARTED_AT=${SECONDS}
p2p_env \
  RUST_LOG="${JOIN_RUST_LOG}" \
  SOVA_FOLLOW_ONLY=1 \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_EPOCH_BASE="${EPOCH_BASE}" \
  SOVA_P2P_PEERS="${ENODE_A}" \
  SOVA_HTTP_PORT="${B_HTTP_PORT}" \
  SOVA_AUTH_PORT="${B_AUTH_PORT}" \
  SOVA_P2P_PORT="${B_P2P_PORT}" \
  "${SOVA_BIN}" >"${WORK_DIR}/${J_LOG_NAME}" 2>&1 &
B_PID=$!
J_LOG="${WORK_DIR}/${J_LOG_NAME}"
wait_for_eth_rpc "${ENGINE_RPC_J}" "${B_PID}" "node J" || exit 1
echo "node J up (pid ${B_PID}); head $(height_of "${ENGINE_RPC_J}")"

echo ""
echo "=== (0) setup ==="
check_p2p_node_log "node A" "${WORK_DIR}/node-a.log"
check_p2p_node_log "node J" "${J_LOG}"
if log_has "${J_LOG}" "expectations: enforcing settlements" 15; then
  pass "setup: node J enforces C5 against its own zebrad view (epoch base ${EPOCH_BASE})"
else
  fail "setup: node J isn't enforcing C5"
fi
J_HEAD0="$(height_of "${ENGINE_RPC_J}")"
if [[ "${J_HEAD0}" -eq 0 ]]; then
  pass "setup: node J starts from an empty datadir (head 0)"
else
  fail "setup: node J did not start empty (head ${J_HEAD0})"
fi
if wait_for_sova_peer "${J_LOG}" 60 && wait_for_sova_peer "${WORK_DIR}/node-a.log" 60; then
  pass "setup: sova/1 session A<->J established"
else
  fail "setup: no sova/1 session between A and J within 60s"
  exit 1
fi

echo "--- resuming auto-mine (A keeps growing while J catches up) ---"
start_auto_mine

# ============================================================
# (a) catch-up, then lockstep
# ============================================================
echo ""
echo "=== (a) catch-up within ${JOIN_TIMEOUT_S}s, then lockstep ==="
CAUGHT_UP=0
DEADLINE=$((SECONDS + JOIN_TIMEOUT_S))
LAST_REPORT=${SECONDS}
while :; do
  A_BLOCK="$(height_of "${ENGINE_RPC_A}")"
  J_BLOCK="$(height_of "${ENGINE_RPC_J}")"
  if [[ "${J_BLOCK}" -ge "${TIP_AT_JOIN}" && $((A_BLOCK - J_BLOCK)) -le 1 ]]; then
    CAUGHT_UP=1
    break
  fi
  if [[ ${SECONDS} -ge ${DEADLINE} ]]; then
    break
  fi
  if [[ $((SECONDS - LAST_REPORT)) -ge 20 ]]; then
    echo "  t+$((SECONDS - J_STARTED_AT))s: A=${A_BLOCK} J=${J_BLOCK}"
    LAST_REPORT=${SECONDS}
  fi
  sleep 1
done
if [[ "${CAUGHT_UP}" -eq 1 ]]; then
  pass "(a) J caught up: A=${A_BLOCK} J=${J_BLOCK}, $((SECONDS - J_STARTED_AT))s after J started (join gap ${TIP_AT_JOIN})"
  # Lockstep is about following NEW blocks: wait (up to 60s) for A to mine
  # past the join tip before sampling — catching up can finish before A's
  # next epoch arrives.
  ADVANCE_DEADLINE=$((SECONDS + 60))
  until [[ "$(height_of "${ENGINE_RPC_A}")" -gt "${TIP_AT_JOIN}" ]] || (( SECONDS >= ADVANCE_DEADLINE )); do
    sleep 1
  done
  for i in 1 2 3; do
    A_BLOCK="$(height_of "${ENGINE_RPC_A}")"
    J_BLOCK="$(height_of "${ENGINE_RPC_J}")"
    LAG=$((A_BLOCK - J_BLOCK))
    if [[ "${LAG}" -ge 0 && "${LAG}" -le 1 && "${A_BLOCK}" -gt "${TIP_AT_JOIN}" ]]; then
      pass "(a) sample ${i}/3: A=${A_BLOCK} J=${J_BLOCK} lag=${LAG}"
    else
      fail "(a) sample ${i}/3: A=${A_BLOCK} J=${J_BLOCK} lag=${LAG} -- outside [0,1] (or A not past the join tip)"
    fi
    if [[ ${i} -lt 3 ]]; then sleep 5; fi
  done
else
  fail "(a) J did NOT catch up within ${JOIN_TIMEOUT_S}s: A=${A_BLOCK} J=${J_BLOCK} (join gap ${TIP_AT_JOIN}, sova/1 chase window ${CHASE_WINDOW})"
  echo "--- STALL diagnosis (J's log) ---" >&2
  N_BEYOND="$(count_in "${J_LOG}" 'beyond p2p catch-up range')"
  N_BACKFILL="$(count_in "${J_LOG}" 'left for backfill')"
  N_SYNCING="$(count_in "${J_LOG}" 'parent unknown (SYNCING)')"
  N_ACCEPTED="$(count_in "${J_LOG}" 'sova/1: peer block accepted')"
  N_ADOPTED="$(count_in "${J_LOG}" 'arbiter adopted preferred candidate')"
  echo "  J head: ${J_BLOCK}; A head: ${A_BLOCK}" >&2
  echo "  'peer is beyond p2p catch-up range' lines: ${N_BEYOND}" >&2
  echo "  'ancestor gap deeper than p2p catch-up; left for backfill' lines: ${N_BACKFILL}" >&2
  echo "  'parent unknown (SYNCING); fetching it' lines: ${N_SYNCING}" >&2
  echo "  'sova/1: peer block accepted' lines: ${N_ACCEPTED}; 'arbiter adopted' lines: ${N_ADOPTED}" >&2
  echo "  first/last stall lines:" >&2
  { lines_in "${J_LOG}" 'beyond p2p catch-up range'; lines_in "${J_LOG}" 'left for backfill'; } \
    | sed -n '1p;$p' | sed 's/^/    /' >&2 || true
fi

# ============================================================
# (b) block-hash equality
# ============================================================
echo ""
echo "=== (b) block-hash equality (1, settled, middle, tip) ==="
TIP_A="$(height_of "${ENGINE_RPC_A}")"
HEIGHTS="$(printf '%s\n' 1 "${SETTLED_HEIGHT}" "${MID_HEIGHT}" "${TIP_A}" | sort -n | uniq)"
if [[ "${CAUGHT_UP}" -eq 1 ]]; then
  wait_for_block_number "${ENGINE_RPC_J}" "${TIP_A}" 30 || fail "(b) J never reached height ${TIP_A}"
fi
for h in ${HEIGHTS}; do
  HASH_A="$(eth_block_hash "${ENGINE_RPC_A}" "${h}" || true)"
  HASH_J="$(eth_block_hash "${ENGINE_RPC_J}" "${h}" || true)"
  label=""
  if [[ "${h}" == "${SETTLED_HEIGHT}" ]]; then label=" -- the early settled epoch"; fi
  if [[ "${h}" == "${MID_HEIGHT}" ]]; then label="${label} -- middle"; fi
  if [[ -n "${HASH_A}" && "${HASH_A}" == "${HASH_J}" ]]; then
    pass "(b) height ${h}: identical on A and J (${HASH_A})${label}"
  else
    fail "(b) height ${h}: A=${HASH_A:-<none>} J=${HASH_J:-<none>}${label}"
  fi
done

# ============================================================
# (c) balance equality
# ============================================================
echo ""
echo "=== (c) miner balance equality ==="
BAL_A_FINAL="$(eth_balance_wei "${ENGINE_RPC_A}" "${EVM_ADDR}" 2>/dev/null || echo '?')"
BAL_J_FINAL="$(eth_balance_wei "${ENGINE_RPC_J}" "${EVM_ADDR}" 2>/dev/null || echo '?')"
if [[ "${BAL_A_FINAL}" == "${BAL_J_FINAL}" && "${BAL_A_FINAL}" != "0" && "${BAL_A_FINAL}" != "?" ]]; then
  pass "(c) miner balance: A == J == ${BAL_A_FINAL} wei (the deep-history mint re-derived by J)"
else
  fail "(c) miner balance mismatch: A=${BAL_A_FINAL} J=${BAL_J_FINAL}"
fi

# ============================================================
# (d) C5 enforced (not deferred) on the historical settled block
# ============================================================
echo ""
echo "=== (d) C5 enforcement on the historical settled block (height ${SETTLED_HEIGHT}) ==="
# Heights J imported on trust: the payload path's "no settlement
# expectation yet; accepting on trust" and consensus' "settlement check
# deferred: epoch not yet scanned" (both debug, both carry height=<n>).
UNENFORCED_HIST="$({ lines_in "${J_LOG}" 'accepting on trust'; lines_in "${J_LOG}" 'settlement check deferred'; } \
  | grep -o ' height=[0-9]*' | sed 's/^ height=//' | sort -n | uniq \
  | awk -v tip="${TIP_AT_JOIN}" '$1 <= tip' | tr '\n' ' ' || true)"
N_DEBUG="$(count_in "${J_LOG}" ' DEBUG ')"
POSITIVE="$(lines_in "${J_LOG}" "${ENFORCED_PATTERN}" | grep -E "(^|[^_])height=${SETTLED_HEIGHT}([^0-9]|\$)" || true)"
J_HAS_SETTLED="$(eth_block_hash "${ENGINE_RPC_J}" "${SETTLED_HEIGHT}" || true)"
# Which import path carried the settled block into J: an in-process
# new_payload (sova/1 fetch; C5 checked by the payload validator AND
# SovaConsensus) or reth's engine-tree download / backfill (no payload;
# SovaConsensus::validate_block_pre_execution is the only C5 check).
if [[ -n "$(lines_in "${J_LOG}" 'Received new payload from consensus engine' | grep -E "number=${SETTLED_HEIGHT}([^0-9]|\$)" || true)" ]]; then
  echo "(d) settled block ${SETTLED_HEIGHT} reached J as a sova/1 new_payload"
elif [[ -n "${J_HAS_SETTLED}" ]]; then
  echo "(d) settled block ${SETTLED_HEIGHT} reached J WITHOUT a new_payload: engine-tree download/backfill (C5 via SovaConsensus only)"
fi
if [[ -z "${J_HAS_SETTLED}" ]]; then
  fail "(d) J never imported the settled block (height ${SETTLED_HEIGHT}); nothing was enforced"
elif [[ -n "${POSITIVE}" ]]; then
  pass "(d) J logged enforcement at the settled height: $(sed -n 1p <<<"${POSITIVE}")"
elif [[ -n "${UNENFORCED_HIST// /}" ]]; then
  fail "(d) J imported historical height(s) ON TRUST (deferred/unknown, not enforced): ${UNENFORCED_HIST}"
else
  pass "(d) no historical height (<= ${TIP_AT_JOIN}) was deferred or accepted on trust by J's C5 check -- the settled block met a known expectation"
  echo "NOTE (d): negative evidence only -- the node logs nothing on a successful C5 check (${N_DEBUG} DEBUG line(s) from J's engine::consensus/validator filter). Proposed positive line in box/sim/README.md." >&2
fi
if [[ "$(count_in "${J_LOG}" 'settlement mismatch')" -gt 0 ]]; then
  fail "(d) J reported a settlement mismatch on an honest chain"
fi

# ============================================================
# (e) no reputation hits
# ============================================================
echo ""
echo "=== (e) reputation ==="
for n in a j; do
  log="${WORK_DIR}/node-${n}.log"
  hits="$(count_in "${log}" 'reputation hit')"
  if [[ "${hits}" -gt 0 ]]; then
    fail "(e) node $(tr a-z A-Z <<<"${n}") logged ${hits} reputation hit(s) / INVALID peer block(s)"
  else
    pass "(e) node $(tr a-z A-Z <<<"${n}"): no reputation hits"
  fi
done

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "JOIN SCENARIO PASSED (JOIN_DEPTH=${JOIN_DEPTH}, gap ${TIP_AT_JOIN}; all assertions)"
else
  echo "JOIN SCENARIO: ${FAILURES} ASSERTION(S) FAILED (JOIN_DEPTH=${JOIN_DEPTH}, gap ${TIP_AT_JOIN})" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
