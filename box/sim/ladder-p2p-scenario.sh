#!/usr/bin/env bash
# sova/1: the ladder + late-win proof over P2P only (board m1-b AC:
# "ladder scenario green over P2P"; docs/design/p2p-m1.md build order 3).
#
# box/sim/ladder-scenario.sh with the transport swapped: both nodes run
# SOVA_GOSSIP=p2p, peer only via sova/1 static devp2p peering (B dials A's
# enode), share no JWT, and keep authrpc on 127.0.0.1. Everything else --
# two miner identities, the SIGSTOPped rank 0, the combined burn epoch,
# the assertions -- is identical:
#
#   (1) ladder     -- B (rank 1) seals the epoch after its rung while A is
#                     SIGSTOPped.
#   (2) late win   -- A resumes, seals rank 0 for the same height and
#                     announces it over sova/1; B pulls it, submits it via
#                     new_payload, and B's OWN arbiter micro-reorgs to it
#                     (sova/1 has no message that can move a head).
#   (3) converge   -- every height and both balances agree; lockstep
#                     resumes.
#
# STATUS (2026-09-22): (1) and (3) pass; (2) FAILS deterministically. On
# resume, A imports B's rank-1 block over sova/1 before its sealer runs,
# and the sealer/miner then builds on top of it instead of sealing
# rank 0. See box/sim/README.md "sova/1 P2P SCENARIOS" for the trace.
# That is a sealer race in driver.rs/miner.rs, not a transport fault.
#
# Isolation/teardown: see box/sim/p2p-common.sh.

SCENARIO="ladder-p2p"
WORK_PREFIX="sova-ladder-p2p"
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"
trap cleanup EXIT

RANK_STEP_S=3
FUND_BLOCKS=101
AUTO_MINE_INTERVAL_S=2

preflight
start_stack

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
# Both nodes, mine mode, sova/1 only: A first (no static peers), then B
# with A's enode as its one static peer. No JWT anywhere, no SOVA_PEERS.
# ---------------------------------------------------------------------

echo "--- starting node A (mine mode, sova/1, rank step ${RANK_STEP_S}s) ---"
p2p_env \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_MINER_EVM_ADDRESS="${EVM_A}" \
  SOVA_EPOCH_BASE="${EPOCH_BASE}" \
  SOVA_RANK_STEP_SECS="${RANK_STEP_S}" \
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

echo "--- starting node B (mine mode, sova/1 static peer = A, rank step ${RANK_STEP_S}s) ---"
p2p_env \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_MINER_EVM_ADDRESS="${EVM_B}" \
  SOVA_EPOCH_BASE="${EPOCH_BASE}" \
  SOVA_RANK_STEP_SECS="${RANK_STEP_S}" \
  SOVA_P2P_PEERS="${ENODE_A}" \
  SOVA_HTTP_PORT="${B_HTTP_PORT}" \
  SOVA_AUTH_PORT="${B_AUTH_PORT}" \
  SOVA_P2P_PORT="${B_P2P_PORT}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-b.log" 2>&1 &
B_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B" || exit 1
echo "node A pid ${A_PID} (${ENODE_A}), node B pid ${B_PID}"

check_p2p_node_log "node A" "${WORK_DIR}/node-a.log"
check_p2p_node_log "node B" "${WORK_DIR}/node-b.log"
if wait_for_sova_peer "${WORK_DIR}/node-a.log" 60 && wait_for_sova_peer "${WORK_DIR}/node-b.log" 60; then
  pass "setup: sova/1 session established"
else
  fail "setup: no sova/1 session between A and B within 60s"
  exit 1
fi

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

# One Sova block per epoch: the Sova tip must track the Zcash tip
# (height = epoch - base + 1, within one epoch of in-flight sealing).
# Ghost blocks built on a stale trigger make Sova run AHEAD of Zcash.
Z_TIP="$(zc_tip_height)"
EXPECTED_TIP=$((Z_TIP - EPOCH_BASE + 1))
if [[ "${A_BLOCK}" -le "${EXPECTED_TIP}" && "${A_BLOCK}" -ge $((EXPECTED_TIP - 1)) ]]; then
  pass "(3) heights track epochs (sova ${A_BLOCK} vs zcash ${Z_TIP} -> expected ${EXPECTED_TIP})"
else
  fail "(3) height/epoch slip: sova ${A_BLOCK} vs expected ${EXPECTED_TIP} (zcash tip ${Z_TIP}, base ${EPOCH_BASE})"
fi

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "LADDER P2P SCENARIO PASSED (all assertions)"
else
  echo "LADDER P2P SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
