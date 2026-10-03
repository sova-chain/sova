#!/usr/bin/env bash
# Seed-1 stall (testnet, 2026-10-01 ~21:00Z): a node that held blocks
# during a Zcash reorg, fell past sova/1's catch-up range and handed the
# tip to the sync driver never reached the tip again. reth's backfill
# pipeline sat at `stage=Headers` and every forkchoice update (arbiter and
# sync driver) answered `Syncing` until `systemctl restart`.
#
# REGRESSION TEST. Failed on release as of f1b0662 (step (s) timed out
# with B stuck at its pre-reorg head); passes since the fix
# (engine::candidates::PIN_FREE_GAP: a catch-up within reach names a held
# block as safe/finalized, so reth downloads blocks instead of pinning a
# backfill pipeline to X).
#
# Mechanism (see the report in the seed-stall branch's commit message):
# the sync driver FCUs the target with safe = finalized = 0, so reth's
# engine tree backfills "optimistically" to that exact head hash
# (reth engine/tree mod.rs backfill_sync_target) and the pipeline run is
# pinned to it (backfill.rs try_spawn_pipeline -> run_as_fut(Some(target))).
# While the pipeline runs, every FCU returns Syncing without touching the
# target (mod.rs validate_forkchoice_state). If the network re-seals that
# block away (a Zcash reorg), no peer ever serves its header chain again,
# the reverse-headers downloader retries forever at trace level, and the
# node never leaves `stage=Headers`.
#
#   zebrad    -- one regtest zebrad (auto-mine).
#   proxy A/B -- box/sim/zebrad-freeze-proxy.py in front of it, one per
#                node: freezing a proxy pins that node's Zcash view, so A
#                and B can see the reorg at different moments (as two real
#                nodes with their own zebrads do).
#   node A    -- mine mode, sova/1, the only producer (the network).
#   node B    -- follow-only, C5-enforcing, sova/1 static peer = A,
#                persistent datadir (plays seed-1).
#
# Flow:
#   1. A and B in sync (both proxies live).
#   2. Freeze B's view at Zcash tip Z0. A keeps sealing; B parks A's
#      blocks (ahead of its scan) and hands the tips beyond 33 blocks to
#      its sync driver, which waits for its scan.
#   3. A reaches H >= Z0 + GAP. Stop mining. X = A's block at H. Freeze
#      A's view.
#   4. Zcash reorg: invalidateblock(Z0 + 1), mine a longer branch
#      (REPLACEMENT_EXTRA past the old tip).
#   5. Thaw B: B now sees the new branch. Every parked block of A's is
#      anchored to the orphaned branch -> "sova/1: block held ... zcash
#      anchor mismatch". B's scan covers H, so the sync driver FCUs X;
#      reth downloads X from A (still on the old branch), finds it
#      > 32 blocks ahead and starts the backfill pipeline pinned to X.
#      Its headers are held by B's consensus (anchor mismatch).
#   6. Thaw A: A sees the reorg and re-seals Z0+1.. on the new branch:
#      X is gone from the network. Auto-mine resumes.
#
# Assertions:
#   (0) setup   -- sova/1 on both nodes; session up; B's sync driver handed
#                  X to the engine (step 5);
#   (p) pipeline -- reth did NOT start a backfill pipeline pinned to X (it
#                  did before the fix);
#                  A re-sealed H (A's block at H != X) and B can validate
#                  A's new branch (anchors == zebrad).
#   (s) stall   -- B reaches A's head within CATCHUP_TIMEOUT_S. Before the
#                  fix this FAILED: B stayed at its pre-reorg head,
#                  `stage=Headers`, every FCU `Syncing` (the seed-1
#                  signature, counted and printed).
#   (r) restart -- control: B restarted on the same datadir catches up
#                  within CATCHUP_TIMEOUT_S (as `systemctl restart` did).
#                  Shows the stall is the running node's pinned pipeline,
#                  not anything on disk or in the network.
#
# Env: GAP (45), REPLACEMENT_EXTRA (10), CATCHUP_TIMEOUT_S (180), plus
# p2p-common.sh's SOVA_P2P_SIM_*, SOVA_BIN, SOVA_P2P_SIM_KEEP_LOGS;
# SEED_STALL_PROXY_PORTS ("18463 18464").
#
# Isolation: zebrad on :18462 (compose project sova-seed-stall-sim,
# container sova-zebrad-seed-stall), proxies on :18463/:18464, A on
# 9745/9751/30911, B on 9845/9851/30912. All overridable. No burns, so no
# sova-miner is needed.

SCENARIO="seed-stall"
WORK_PREFIX="sova-seed-stall"
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18462}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-seed-stall}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-seed-stall-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=9745 9751 30911}"
: "${SOVA_P2P_SIM_B_PORTS:=9845 9851 30912}"
# No burns in this scenario: preflight's sova-miner check is moot.
: "${SOVA_MINER_BIN:=/usr/bin/true}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS SOVA_MINER_BIN
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"

GAP="${GAP:-45}"
REPLACEMENT_EXTRA="${REPLACEMENT_EXTRA:-10}"
CATCHUP_TIMEOUT_S="${CATCHUP_TIMEOUT_S:-180}"
AUTO_MINE_INTERVAL_S=2
read -r PROXY_A_PORT PROXY_B_PORT <<<"${SEED_STALL_PROXY_PORTS:-18463 18464}"
PROXY_A="http://127.0.0.1:${PROXY_A_PORT}"
PROXY_B="http://127.0.0.1:${PROXY_B_PORT}"
# Any address: A seals burn-less epochs only, nothing is ever credited.
MINER_EVM="0x000000000000000000000000000000000000dEaD"
PROXY_A_PID=""
PROXY_B_PID=""

seed_cleanup() {
  local ec=$?
  for pid in "${PROXY_A_PID}" "${PROXY_B_PID}"; do
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      kill "${pid}" 2>/dev/null || true
      wait "${pid}" 2>/dev/null || true
    fi
  done
  (exit "${ec}")
  cleanup
}
trap seed_cleanup EXIT

# --- helpers ------------------------------------------------------------

height_of() { eth_block_number "$1" 2>/dev/null || echo 0; }

count_in() {
  local n
  n="$(sed 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | grep -cF -- "$2" || true)"
  echo "${n:-0}"
}

zc_block_hash_at() {
  zc_rpc getblockhash "[$1]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'].lower())" 2>/dev/null || true
}

proxy_ctl() { # <url> <freeze|thaw>
  curl -s -X POST "$1/__$2"
}

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

wait_for_stable_height() { # <url> <min> <timeout_s>
  local deadline=$((SECONDS + $3)) cur prev=-1
  while :; do
    cur="$(height_of "$1")"
    if [[ "${cur}" -ge "$2" && "${cur}" -eq "${prev}" ]]; then
      echo "${cur}"
      return 0
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "${cur}"
      return 1
    fi
    prev="${cur}"
    sleep 3
  done
}

start_node_b() { # <log>
  local extra=()
  # Diagnostics only, e.g. SEED_STALL_B_RUST_LOG=info,net::peers=debug
  [[ -n "${SEED_STALL_B_RUST_LOG:-}" ]] && extra=("RUST_LOG=${SEED_STALL_B_RUST_LOG}")
  p2p_env \
    ${extra[@]+"${extra[@]}"} \
    SOVA_FOLLOW_ONLY=1 \
    SOVA_ZEBRAD_RPC="${PROXY_B}" \
    SOVA_EPOCH_BASE=1 \
    SOVA_DATADIR="${WORK_DIR}/b-data" \
    SOVA_P2P_PEERS="${ENODE_A}" \
    SOVA_HTTP_PORT="${B_HTTP_PORT}" \
    SOVA_AUTH_PORT="${B_AUTH_PORT}" \
    SOVA_P2P_PORT="${B_P2P_PORT}" \
    "${SOVA_BIN}" >"$1" 2>&1 &
  B_PID=$!
}

# Waits until B is within 1 block of A's (moving) head; prints B's head.
wait_for_b_at_tip() { # <timeout_s>
  local deadline=$((SECONDS + $1)) a b
  while :; do
    a="$(height_of "${ENGINE_RPC_A}")"
    b="$(height_of "${ENGINE_RPC_B}")"
    if [[ "${b}" -gt 0 && $((a - b)) -le 1 ]]; then
      echo "${b}"
      return 0
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "${b}"
      return 1
    fi
    sleep 2
  done
}

# The last reth `Status` line of a log, without the timestamp prefix.
last_status() {
  strip_ansi "$1" | grep ' Status connected_peers=' | tail -1 | sed -E 's/^.*Z +INFO +//'
}

for port in "${PROXY_A_PORT}" "${PROXY_B_PORT}"; do
  if lsof -nP -iTCP:"${port}" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "error: proxy port ${port} already in use; override SEED_STALL_PROXY_PORTS" >&2
    exit 1
  fi
done
preflight
start_stack

python3 "${HERE}/zebrad-freeze-proxy.py" "${PROXY_A_PORT}" "${ZEBRAD_RPC}" >"${WORK_DIR}/proxy-a.log" 2>&1 &
PROXY_A_PID=$!
python3 "${HERE}/zebrad-freeze-proxy.py" "${PROXY_B_PORT}" "${ZEBRAD_RPC}" >"${WORK_DIR}/proxy-b.log" 2>&1 &
PROXY_B_PID=$!
sleep 1
for url in "${PROXY_A}" "${PROXY_B}"; do
  if ! curl -s -X POST -H 'Content-Type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"getblockcount","params":[]}' "${url}/" | grep -q result; then
    fail "setup: zebrad proxy ${url} not answering"
    exit 1
  fi
done

echo "--- starting node A (mine mode, sova/1, zebrad via proxy ${PROXY_A}) ---"
p2p_env \
  SOVA_ZEBRAD_RPC="${PROXY_A}" \
  SOVA_MINER_EVM_ADDRESS="${MINER_EVM}" \
  SOVA_EPOCH_BASE=1 \
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

echo "--- starting node B (follow-only, sova/1 static peer = A, zebrad via proxy ${PROXY_B}) ---"
start_node_b "${WORK_DIR}/node-b.log"
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B" || exit 1
echo "node B up (pid ${B_PID})"

echo ""
echo "=== (0) setup ==="
check_p2p_node_log "node A" "${WORK_DIR}/node-a.log"
check_p2p_node_log "node B" "${WORK_DIR}/node-b.log"
if wait_for_sova_peer "${WORK_DIR}/node-a.log" 60 && wait_for_sova_peer "${WORK_DIR}/node-b.log" 60; then
  pass "setup: sova/1 session A<->B established"
else
  fail "setup: no sova/1 session between A and B within 60s"
  exit 1
fi

start_auto_mine
if ! wait_for_block_number "${ENGINE_RPC_A}" 5 120 || ! wait_for_block_number "${ENGINE_RPC_B}" 5 60; then
  fail "setup: chain did not start (A=$(height_of "${ENGINE_RPC_A}") B=$(height_of "${ENGINE_RPC_B}"))"
  exit 1
fi

# ---------------------------------------------------------------------
# 2. Freeze B's Zcash view; A runs ahead.
# ---------------------------------------------------------------------
Z0="$(proxy_ctl "${PROXY_B}" freeze | python3 -c "import sys,json;print(json.load(sys.stdin)['tip'])")"
B_HEAD0="$(height_of "${ENGINE_RPC_B}")"
TARGET_H=$((Z0 + GAP))
echo "B's Zcash view frozen at tip Z0=${Z0} (B head ${B_HEAD0}); A runs to ${TARGET_H}"
if ! wait_for_block_number "${ENGINE_RPC_A}" "${TARGET_H}" 300; then
  fail "setup: A did not reach ${TARGET_H} (at $(height_of "${ENGINE_RPC_A}"))"
  exit 1
fi

# ---------------------------------------------------------------------
# 3. Freeze A's view at its tip; X = A's block at H.
# ---------------------------------------------------------------------
stop_auto_mine
H="$(wait_for_stable_height "${ENGINE_RPC_A}" "${TARGET_H}" 60)" || {
  fail "setup: A did not settle (at ${H})"
  exit 1
}
X="$(eth_block_hash "${ENGINE_RPC_A}" "${H}")"
OLD_ZTIP="$(zc_tip_height)"
Z1="$(proxy_ctl "${PROXY_A}" freeze | python3 -c "import sys,json;print(json.load(sys.stdin)['tip'])")"
B_HEAD1="$(height_of "${ENGINE_RPC_B}")"
echo "A settled at H=${H}, X=${X}; A's view frozen at Zcash tip ${Z1}; B head ${B_HEAD1}"
if [[ $((H - B_HEAD1)) -le 33 ]]; then
  fail "setup: B (${B_HEAD1}) is not past sova/1's catch-up range of A (${H})"
  exit 1
fi

# ---------------------------------------------------------------------
# 4. Zcash reorg just above B's view.
# ---------------------------------------------------------------------
FORK=$((Z0 + 1))
OLD_FORK_HASH="$(zc_block_hash_at "${FORK}")"
zc_rpc invalidateblock "[\"${OLD_FORK_HASH}\"]" >"${WORK_DIR}/invalidate.json"
ZTIP_AFTER="$(zc_tip_height)"
if [[ "${ZTIP_AFTER}" != "${Z0}" ]]; then
  fail "setup: zebrad tip ${ZTIP_AFTER} after invalidateblock(${FORK}), want ${Z0} ($(cat "${WORK_DIR}/invalidate.json"))"
  exit 1
fi
NEW_BLOCKS=$((OLD_ZTIP - Z0 + REPLACEMENT_EXTRA))
"${REGTEST_DIR}/mine.sh" "${NEW_BLOCKS}" "${ZEBRAD_RPC}" >>"${WORK_DIR}/auto-mine.log" 2>&1 || true
NEW_FORK_HASH="$(zc_block_hash_at "${FORK}")"
echo "Zcash reorg at ${FORK}: ${OLD_FORK_HASH:0:16}.. -> ${NEW_FORK_HASH:0:16}..; tip ${OLD_ZTIP} -> $(zc_tip_height)"
if [[ -z "${NEW_FORK_HASH}" || "${NEW_FORK_HASH}" == "${OLD_FORK_HASH}" ]]; then
  fail "setup: the replacement branch did not replace Zcash block ${FORK}"
  exit 1
fi

# ---------------------------------------------------------------------
# 5. Thaw B: it holds A's blocks and hands X to its sync driver.
# ---------------------------------------------------------------------
STAGES_BEFORE="$(count_in "${WORK_DIR}/node-b.log" "Preparing stage")"
proxy_ctl "${PROXY_B}" thaw >/dev/null
X_HEX="${X#0x}"
PIPELINE_STARTED=0
if log_has "${WORK_DIR}/node-b.log" "hash=${X_HEX}" 60; then
  pass "setup: B's sync driver handed X (${H}) to the engine"
  # Before the fix (engine::candidates::PIN_FREE_GAP) reth started its
  # backfill pipeline here, pinned to X; now the gap is fetched by block
  # downloads that follow every FCU. Watched for 60 s either way.
  DEADLINE=$((SECONDS + 60))
  while [[ ${SECONDS} -lt ${DEADLINE} ]]; do
    if [[ "$(count_in "${WORK_DIR}/node-b.log" "Preparing stage")" -gt "${STAGES_BEFORE}" ]]; then
      PIPELINE_STARTED=1
      break
    fi
    sleep 1
  done
else
  fail "setup: B's sync driver never handed X to the engine within 60s (handoff lines: $(count_in "${WORK_DIR}/node-b.log" "catching up to sync target"))"
  exit 1
fi
if [[ "${PIPELINE_STARTED}" -eq 0 ]]; then
  pass "(p) no backfill pipeline pinned to X: a ${H}-block catch-up goes by block downloads"
else
  fail "(p) reth started a backfill pipeline pinned to X (the pre-fix behaviour)"
fi
sleep 5

# ---------------------------------------------------------------------
# 6. Thaw A: the network re-seals X away.
# ---------------------------------------------------------------------
proxy_ctl "${PROXY_A}" thaw >/dev/null
start_auto_mine
DEADLINE=$((SECONDS + 180))
A_AT_H=""
while [[ ${SECONDS} -lt ${DEADLINE} ]]; do
  A_AT_H="$(eth_block_hash "${ENGINE_RPC_A}" "${H}")"
  if [[ -n "${A_AT_H}" && "${A_AT_H}" != "${X}" ]]; then
    break
  fi
  sleep 2
done
if [[ -n "${A_AT_H}" && "${A_AT_H}" != "${X}" ]]; then
  pass "setup: A re-sealed height ${H} on the new Zcash branch (X ${X:0:18}.. -> ${A_AT_H:0:18}..)"
else
  fail "setup: A still has X at ${H} after the reorg; the scenario's premise did not happen"
  exit 1
fi
A_ANCHOR_H="$(eth_rpc "${ENGINE_RPC_A}" eth_getBlockByNumber "[\"$(printf '0x%x' "${H}")\", false]" \
  | python3 -c "import sys,json;print((json.load(sys.stdin)['result'].get('parentBeaconBlockRoot') or '').lower())")"
if [[ "${A_ANCHOR_H}" == "0x$(zc_block_hash_at "${H}")" ]]; then
  pass "setup: A's new block ${H} anchors zebrad's best chain (valid for B)"
else
  fail "setup: A's block ${H} anchor ${A_ANCHOR_H} != zebrad's $(zc_block_hash_at "${H}")"
fi

# ---------------------------------------------------------------------
# (s) B must reach the tip.
# ---------------------------------------------------------------------
echo ""
echo "=== (s) B reaches A's head within ${CATCHUP_TIMEOUT_S}s ==="
CAUGHT_UP=0
B_FINAL="$(wait_for_b_at_tip "${CATCHUP_TIMEOUT_S}")" || CAUGHT_UP=1
A_FINAL="$(height_of "${ENGINE_RPC_A}")"
B_LOG="${WORK_DIR}/node-b.log"
echo "  B: head ${B_FINAL} (was ${B_HEAD1} before the reorg); A: head ${A_FINAL}"
echo "  B last status:      $(last_status "${B_LOG}")"
echo "  B 'block held':                    $(count_in "${B_LOG}" "sova/1: block held")"
echo "  B 'beyond p2p catch-up range':     $(count_in "${B_LOG}" "peer is beyond p2p catch-up range")"
echo "  B 'catching up to sync target':    $(count_in "${B_LOG}" "catching up to sync target")"
echo "  B 'Preparing stage':               $(count_in "${B_LOG}" "Preparing stage")"
echo "  B FCU answered while syncing:      $(count_in "${B_LOG}" "Received forkchoice updated message when syncing")"
echo "  B arbiter FCU Syncing:             $(count_in "${B_LOG}" "arbiter forkchoice update failed")"
if [[ "${CAUGHT_UP}" -eq 0 ]]; then
  pass "(s) B caught up to A's head (${B_FINAL}/${A_FINAL}) after the target it was handed (X) was re-sealed away"
else
  fail "(s) B stuck at ${B_FINAL} with A at ${A_FINAL} after ${CATCHUP_TIMEOUT_S}s: the backfill pipeline is pinned to X=${X}, which no peer serves any more (seed-1 2026-10-01)"
fi

# ---------------------------------------------------------------------
# (r) control: a restart catches up.
# ---------------------------------------------------------------------
echo ""
echo "=== (r) control: B restarted on the same datadir ==="
kill "${B_PID}" 2>/dev/null || true
wait "${B_PID}" 2>/dev/null || true
B_PID=""
start_node_b "${WORK_DIR}/node-b-restart.log"
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B (restart)" || exit 1
if B_RESTART="$(wait_for_b_at_tip "${CATCHUP_TIMEOUT_S}")"; then
  pass "(r) restarted B caught up to A (${B_RESTART}/$(height_of "${ENGINE_RPC_A}")): the stall lived only in the running node"
else
  fail "(r) restarted B stuck at ${B_RESTART} (A $(height_of "${ENGINE_RPC_A}"))"
fi

if [[ "${FAILURES}" -gt 0 ]]; then
  echo "${SCENARIO}: ${FAILURES} assertion(s) failed"
  exit 1
fi
echo "${SCENARIO}: all assertions passed"
