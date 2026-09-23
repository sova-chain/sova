#!/usr/bin/env bash
# sova/1: two nodes with NO shared secret converge over P2P only
# (board m1-b step 3; docs/design/p2p-m1.md "Propagation").
#
# The p2p twin of box/sim/two-node-scenario.sh. Two independent
# `bin/sova` processes, both SOVA_GOSSIP=p2p:
#
#   node A -- mine mode (one miner identity, fund, auto-mine, one burn).
#             The only block producer. No SOVA_PEERS, no SOVA_AUTH_JWT.
#   node B -- follow-only, C5-enforcing against zebrad, peered with A
#             ONLY via SOVA_P2P_PEERS=<A's enode> (static devp2p peering,
#             discovery off). No SOVA_AUTH_JWT: B's authrpc keeps its own
#             reth-generated secret on 127.0.0.1 and A never learns it.
#             Every block B has arrived as a sova/1 Announce -> GetBlock
#             -> Block pull and was submitted to B's own engine
#             in-process (new_payload); B's own arbiter moved its head.
#
# Assertions:
#   (0) setup      -- both nodes run sova/1 with authrpc on loopback and
#                     no relay; the sova/1 session is established.
#   (a) lockstep   -- B's height tracks A's (3 samples, lag <= 1).
#   (b) hashes     -- getBlockByNumber(H).hash identical on A and B at
#                     4 heights incl. the burn's settled epoch.
#   (c) balance    -- the miner's minted balance is identical on A and B.
#   (d) transport  -- B's log shows blocks accepted over sova/1, and B
#                     never received an engine call on authrpc.
#
# Isolation/teardown: see box/sim/p2p-common.sh (own ports, own compose
# project, PID-scoped teardown).

SCENARIO="two-node-p2p"
WORK_PREFIX="sova-two-node-p2p"
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"
trap cleanup EXIT

BUDGET_ZAT=200000
PER_EPOCH_ZAT=100000
FUND_BLOCKS=101
AUTO_MINE_INTERVAL_S=2

preflight
start_stack

# ---------------------------------------------------------------------
# Miner identity (node A needs its EVM address to start in mine mode)
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
# Node A first (mine mode, no peers of its own): B dials it.
# ---------------------------------------------------------------------
echo "--- starting node A (mine mode, sova/1, no JWT, no static peers) ---"
p2p_env \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" \
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

echo "--- starting node B (follow-only, sova/1 static peer = A, no JWT) ---"
p2p_env \
  SOVA_FOLLOW_ONLY=1 \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_EPOCH_BASE=1 \
  SOVA_P2P_PEERS="${ENODE_A}" \
  SOVA_HTTP_PORT="${B_HTTP_PORT}" \
  SOVA_AUTH_PORT="${B_AUTH_PORT}" \
  SOVA_P2P_PORT="${B_P2P_PORT}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-b.log" 2>&1 &
B_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B" || exit 1
echo "node B up (pid ${B_PID})"

echo ""
echo "=== (0) setup: transport ==="
check_p2p_node_log "node A" "${WORK_DIR}/node-a.log"
check_p2p_node_log "node B" "${WORK_DIR}/node-b.log"
if [[ "$(strip_ansi "${WORK_DIR}/node-b.log" | grep -c "expectations: enforcing settlements" || true)" -gt 0 ]]; then
  pass "setup: node B enforces C5 against its own zebrad view"
else
  fail "setup: node B isn't enforcing C5"
fi
if wait_for_sova_peer "${WORK_DIR}/node-a.log" 60 && wait_for_sova_peer "${WORK_DIR}/node-b.log" 60; then
  pass "setup: sova/1 session established (both sides report an active peer)"
else
  fail "setup: no sova/1 session between A and B within 60s"
  exit 1
fi

# ---------------------------------------------------------------------
# Drive the chain: fund, auto-mine, one burn.
# ---------------------------------------------------------------------
echo "--- funding: ${FUND_BLOCKS} blocks to the miner's own address ---"
zc_generate_to_address "${FUND_BLOCKS}" "${TADDR}"
echo "--- auto-mine every ${AUTO_MINE_INTERVAL_S}s ---"
"${REGTEST_DIR}/auto-mine.sh" "${AUTO_MINE_INTERVAL_S}" "${ZEBRAD_RPC}" >"${WORK_DIR}/auto-mine.log" 2>&1 &
AUTO_MINE_PID=$!

echo ""
echo "=== burn (node A's miner) ==="
if ! "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
  --budget-zat "${BUDGET_ZAT}" --per-epoch-zat "${PER_EPOCH_ZAT}" \
  --rpc "${ZEBRAD_RPC}" --max-epochs 1 >"${WORK_DIR}/mine-1.log" 2>&1; then
  fail "burn: sova-miner mine exited nonzero"
  cat "${WORK_DIR}/mine-1.log" >&2
fi

BAL_A="$(wait_for_balance_change "${ENGINE_RPC_A}" "${EVM_ADDR}" "0" 120)" || {
  fail "setup: node A's balance never moved off 0 within 120s"
  exit 1
}
echo "node A settled balance: ${BAL_A} wei"
SETTLED_HEIGHT="$(strip_ansi "${WORK_DIR}/node-a.log" | grep 'sova epoch trigger' | grep 'settled=true' \
  | grep -o 'height=[0-9]*' | head -1 | cut -d= -f2)"
if [[ -z "${SETTLED_HEIGHT}" ]]; then
  fail "setup: no settled trigger in node A's log"
  exit 1
fi
echo "settled height: ${SETTLED_HEIGHT}"

# ============================================================
# (a) lockstep
# ============================================================
echo ""
echo "=== (a) lockstep sampling (B vs A) ==="
# B imports only as far as its own Zcash scan (SIP-4 hold-don't-accept), so
# right after the ~100-block funding burst it can still be catching up.
# Lockstep is a claim about the steady state: wait for catch-up first.
CATCHUP_TARGET="$(eth_block_number "${ENGINE_RPC_A}")"
if wait_for_block_number "${ENGINE_RPC_B}" "${CATCHUP_TARGET}" 60; then
  echo "node B caught up to A's height ${CATCHUP_TARGET}"
else
  fail "(a) node B never caught up to A's height ${CATCHUP_TARGET} within 60s"
fi
for i in 1 2 3; do
  A_BLOCK="$(eth_block_number "${ENGINE_RPC_A}")"
  B_BLOCK="$(eth_block_number "${ENGINE_RPC_B}")"
  LAG=$((A_BLOCK - B_BLOCK))
  if [[ "${LAG}" -ge 0 && "${LAG}" -le 1 && "${A_BLOCK}" -gt 0 ]]; then
    pass "(a) sample ${i}/3: node_a=${A_BLOCK} node_b=${B_BLOCK} lag=${LAG}"
  else
    fail "(a) sample ${i}/3: node_a=${A_BLOCK} node_b=${B_BLOCK} lag=${LAG} -- outside [0,1]"
  fi
  [[ ${i} -lt 3 ]] && sleep 5
done

# ============================================================
# (b) block-hash equality (4 heights incl. the settled epoch)
# ============================================================
echo ""
echo "=== (b) block-hash equality ==="
TIP_A="$(eth_block_number "${ENGINE_RPC_A}")"
HEIGHTS="$(printf '%s\n' 1 "${SETTLED_HEIGHT}" $((SETTLED_HEIGHT + 1)) "${TIP_A}" | sort -n | uniq)"
MAX_HEIGHT="$(echo "${HEIGHTS}" | tail -1)"
if ! wait_for_block_number "${ENGINE_RPC_B}" "${MAX_HEIGHT}" 60; then
  fail "(b) node B never reached height ${MAX_HEIGHT}"
fi
for h in ${HEIGHTS}; do
  HASH_A="$(eth_block_hash "${ENGINE_RPC_A}" "${h}")"
  HASH_B="$(eth_block_hash "${ENGINE_RPC_B}" "${h}")"
  if [[ -n "${HASH_A}" && "${HASH_A}" == "${HASH_B}" ]]; then
    pass "(b) height ${h}: identical on A and B (${HASH_A})$([[ "${h}" == "${SETTLED_HEIGHT}" ]] && echo " -- the settled epoch")"
  else
    fail "(b) height ${h}: A=${HASH_A:-<none>} B=${HASH_B:-<none>}"
  fi
done

# ============================================================
# (c) balance equality
# ============================================================
echo ""
echo "=== (c) miner balance equality ==="
BAL_A_FINAL="$(eth_balance_wei "${ENGINE_RPC_A}" "${EVM_ADDR}")"
BAL_B_FINAL="$(eth_balance_wei "${ENGINE_RPC_B}" "${EVM_ADDR}")"
if [[ "${BAL_A_FINAL}" == "${BAL_B_FINAL}" && "${BAL_A_FINAL}" != "0" ]]; then
  pass "(c) miner balance: A == B == ${BAL_A_FINAL} wei (mint carried over sova/1)"
else
  fail "(c) miner balance mismatch: A=${BAL_A_FINAL} B=${BAL_B_FINAL}"
fi

# ============================================================
# (d) the transport really was sova/1
# ============================================================
echo ""
echo "=== (d) transport evidence ==="
ACCEPTED_B="$(strip_ansi "${WORK_DIR}/node-b.log" | grep -c 'sova/1: peer block accepted')"
if [[ "${ACCEPTED_B}" -ge "${MAX_HEIGHT}" ]]; then
  pass "(d) node B accepted ${ACCEPTED_B} block(s) over sova/1 (>= ${MAX_HEIGHT} heights)"
else
  fail "(d) node B accepted only ${ACCEPTED_B} block(s) over sova/1 (expected >= ${MAX_HEIGHT})"
fi
if strip_ansi "${WORK_DIR}/node-b.log" | grep -qi 'engine_newPayload\|relay: peer accepted'; then
  fail "(d) node B's log mentions an authrpc engine call"
else
  pass "(d) no authrpc engine traffic on B (no shared JWT existed)"
fi
if [[ "$(strip_ansi "${WORK_DIR}/node-b.log" | grep -c 'reputation hit' || true)" -gt 0 ]]; then
  fail "(d) node B penalised node A"
else
  pass "(d) no reputation hits"
fi

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "TWO-NODE P2P SCENARIO PASSED (all assertions)"
else
  echo "TWO-NODE P2P SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
