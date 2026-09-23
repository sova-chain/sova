#!/usr/bin/env bash
# sova/1 + discovery: a third node finds the others ONLY via a bootnode
# (board m1-b step 4; docs/design/p2p-m1.md "Discovery and isolation").
#
# Three independent `bin/sova` processes on the public-testnet profile
# (SOVA_CHAIN=sova-testnet: empty genesis alloc, chain ID 82330, Sova's
# own genesis/fork ID), all SOVA_GOSSIP=p2p, discovery ON (the profile
# default: discv4 + discv5 on the RLPx port, ENR fork-ID enforced, DNS
# discovery off). No SOVA_P2P_PEERS anywhere, no shared JWT.
#
#   node B -- follow-only, C5-enforcing. Started first with NO bootnodes
#             and no static peers: it is the bootnode.
#   node A -- mine mode (the only producer). SOVA_BOOTNODES=<B>.
#   node C -- follow-only, C5-enforcing. SOVA_BOOTNODES=<B> and nothing
#             else: C was never told A exists. It must learn A from B's
#             discovery table, dial it (or be dialed by it), and get a
#             sova/1 session with it.
#
# Everything binds to 127.0.0.1 (SOVA_P2P_ADDR) and advertises
# 127.0.0.1 (SOVA_NAT=extip:127.0.0.1, so no UPnP / public-IP lookup), so
# discovery cannot leak onto the real internet; (6) proves it from the
# processes' actual sockets.
#
# Assertions:
#   (0) setup     -- all three run sova/1 on sova-testnet (chain ID 82330)
#                    with discovery on + fork-id enforcement; authrpc on
#                    loopback; no relay; C was configured with exactly one
#                    bootnode (B) and no static peers.
#   (1) discovery -- C gets sova/1 sessions with B AND with A (A's peer id
#                    appears in C's active-peer set), A with B.
#   (2) lockstep  -- B and C track A (3 samples, lag <= 1).
#   (3) hashes    -- identical on A, B, C at 4 heights incl. the burn's
#                    settled epoch.
#   (4) balance   -- the miner's minted balance identical on A, B, C.
#   (5) transport -- C accepted blocks over sova/1, some directly from A;
#                    no reputation hits.
#   (6) locality  -- every socket the three nodes held during the run
#                    (lsof, sampled every 2s) was bound to and connected
#                    to loopback only.
#
# Isolation: zebrad on :18332 (compose project sova-disc-sim, container
# sova-zebrad-disc), nodes on 8945/8951/30511 (A), 9045/9051/30512 (B),
# 9145/9151/30513 (C); all overridable as in box/sim/p2p-common.sh.

SCENARIO="three-node-discovery"
WORK_PREFIX="sova-three-node-disc"
P2P_THREE_NODES=1
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18332}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-disc}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-disc-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=8945 8951 30511}"
: "${SOVA_P2P_SIM_B_PORTS:=9045 9051 30512}"
: "${SOVA_P2P_SIM_C_PORTS:=9145 9151 30513}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS SOVA_P2P_SIM_C_PORTS
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"
trap cleanup EXIT

BUDGET_ZAT=200000
PER_EPOCH_ZAT=100000
FUND_BLOCKS=101
AUTO_MINE_INTERVAL_S=2
TESTNET_CHAIN_ID=82330

preflight
start_stack

# Common env for every node here: p2p_env's (sova/1, no JWT, no relay
# peers) plus the testnet profile and loopback-only p2p. Anything that
# could add a peer is cleared first; callers pass SOVA_BOOTNODES where
# they want one. Same `exec` trick as p2p_env: `disc_env ... &` makes $!
# the node's PID.
disc_env() {
  exec env -u SOVA_AUTH_JWT -u SOVA_PEERS -u SOVA_P2P_PEERS -u SOVA_DISCOVERY -u SOVA_BOOTNODES \
    SOVA_GOSSIP=p2p \
    SOVA_CHAIN=sova-testnet \
    SOVA_P2P_ADDR=127.0.0.1 \
    SOVA_NAT=extip:127.0.0.1 \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
    SOVA_EPOCH_BASE=1 \
    "$@"
}

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

PIDFILE="${WORK_DIR}/node-pids"
: >"${PIDFILE}"
start_socket_sampler "${WORK_DIR}/sockets.txt" "${PIDFILE}"

# ---------------------------------------------------------------------
# B: the bootnode (follow-only, no bootnodes, no static peers)
# ---------------------------------------------------------------------
echo "--- starting node B (bootnode; follow-only; no bootnodes, no static peers) ---"
disc_env \
  SOVA_FOLLOW_ONLY=1 \
  SOVA_HTTP_PORT="${B_HTTP_PORT}" \
  SOVA_AUTH_PORT="${B_AUTH_PORT}" \
  SOVA_P2P_PORT="${B_P2P_PORT}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-b.log" 2>&1 &
B_PID=$!
echo "${B_PID}" >>"${PIDFILE}"
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B" || exit 1
ENODE_B="$(local_enode "${WORK_DIR}/node-b.log")" || {
  fail "setup: node B never printed its enode"
  exit 1
}
echo "node B up (pid ${B_PID}); enode ${ENODE_B}"

# ---------------------------------------------------------------------
# A: the miner, bootnode = B
# ---------------------------------------------------------------------
echo "--- starting node A (mine mode; SOVA_BOOTNODES=B) ---"
disc_env \
  SOVA_BOOTNODES="${ENODE_B}" \
  SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" \
  SOVA_HTTP_PORT="${A_HTTP_PORT}" \
  SOVA_AUTH_PORT="${A_AUTH_PORT}" \
  SOVA_P2P_PORT="${A_P2P_PORT}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-a.log" 2>&1 &
A_PID=$!
echo "${A_PID}" >>"${PIDFILE}"
wait_for_eth_rpc "${ENGINE_RPC_A}" "${A_PID}" "node A" || exit 1
ENODE_A="$(local_enode "${WORK_DIR}/node-a.log")" || {
  fail "setup: node A never printed its enode"
  exit 1
}
echo "node A up (pid ${A_PID}); enode ${ENODE_A}"
ID_A="$(enode_id "${ENODE_A}")"
ID_B="$(enode_id "${ENODE_B}")"

if wait_for_sova_peer_id "${WORK_DIR}/node-a.log" "${ID_B}" 60; then
  pass "setup: A reached its bootnode B over sova/1"
else
  fail "setup: A never got a sova/1 session with bootnode B within 60s"
  exit 1
fi

# ---------------------------------------------------------------------
# C: bootnode = B, nothing else
# ---------------------------------------------------------------------
echo "--- starting node C (follow-only; SOVA_BOOTNODES=B ONLY; no static peers) ---"
disc_env \
  SOVA_BOOTNODES="${ENODE_B}" \
  SOVA_FOLLOW_ONLY=1 \
  SOVA_HTTP_PORT="${C_HTTP_PORT}" \
  SOVA_AUTH_PORT="${C_AUTH_PORT}" \
  SOVA_P2P_PORT="${C_P2P_PORT}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-c.log" 2>&1 &
C_PID=$!
echo "${C_PID}" >>"${PIDFILE}"
wait_for_eth_rpc "${ENGINE_RPC_C}" "${C_PID}" "node C" || exit 1
ENODE_C="$(local_enode "${WORK_DIR}/node-c.log")" || {
  fail "setup: node C never printed its enode"
  exit 1
}
ID_C="$(enode_id "${ENODE_C}")"
echo "node C up (pid ${C_PID}); enode ${ENODE_C}"

echo ""
echo "=== (0) setup ==="
for n in a b c; do
  label="node $(tr a-z A-Z <<<"${n}")"
  log="${WORK_DIR}/node-${n}.log"
  check_p2p_node_log "${label}" "${log}"
  clean="$(strip_ansi "${log}")"
  if grep -q "p2p: discovery on (discv4 + discv5 on udp 127.0.0.1:" <<<"${clean}" \
    && grep -q "enforce ENR fork id true" <<<"${clean}" \
    && grep -q "dns off" <<<"${clean}" \
    && grep -q "nat extip:127.0.0.1" <<<"${clean}"; then
    pass "setup: ${label} discovery on (discv4+discv5, loopback, fork-id enforced, no DNS)"
  else
    fail "setup: ${label} discovery line missing/wrong: $(grep 'p2p: discovery' <<<"${clean}" || echo '<none>')"
  fi
  if grep -q "sova/1 registered in the network config" <<<"${clean}"; then
    pass "setup: ${label} registered sova/1 before the network started"
  else
    fail "setup: ${label} did not register sova/1 in the network config"
  fi
  if grep -q "chain profile: sova-testnet (chain ID ${TESTNET_CHAIN_ID}, 0 genesis alloc" <<<"${clean}"; then
    pass "setup: ${label} runs sova-testnet (chain ID ${TESTNET_CHAIN_ID}, empty alloc)"
  else
    fail "setup: ${label} is not on sova-testnet"
  fi
done
CLEAN_C="$(strip_ansi "${WORK_DIR}/node-c.log")"
if grep -q "1 bootnode(s)" <<<"${CLEAN_C}" && grep -q "no SOVA_P2P_PEERS" <<<"${CLEAN_C}"; then
  pass "setup: C's only configured peer is its one bootnode (B); no static peers"
else
  fail "setup: C's peer config isn't bootnode-only"
fi
if grep -q "0 bootnode(s)" <<<"$(strip_ansi "${WORK_DIR}/node-b.log")"; then
  pass "setup: B (the bootnode) has no bootnodes of its own"
else
  fail "setup: B has bootnodes configured"
fi

echo ""
echo "=== (1) discovery ==="
if wait_for_sova_peer_id "${WORK_DIR}/node-c.log" "${ID_B}" 90; then
  pass "(1) C has a sova/1 session with its bootnode B"
else
  fail "(1) C never got a sova/1 session with B within 90s"
fi
if wait_for_sova_peer_id "${WORK_DIR}/node-c.log" "${ID_A}" 120; then
  pass "(1) C has a sova/1 session with A -- learned only through discovery"
else
  fail "(1) C never got a sova/1 session with A within 120s"
fi
echo "C's active sova/1 peers:"
while read -r id; do
  [[ -z "${id}" ]] && continue
  case "${id}" in
    "${ID_A}") who="A" ;;
    "${ID_B}") who="B" ;;
    *) who="UNKNOWN" ;;
  esac
  echo "  ${who}: ${id:0:16}..."
  [[ "${who}" == "UNKNOWN" ]] && fail "(1) C peered with a node that is neither A nor B: ${id}"
done <<<"$(sova_active_peers "${WORK_DIR}/node-c.log")"
if sova_active_peers "${WORK_DIR}/node-a.log" | grep -qx "${ID_C}"; then
  pass "(1) A reports C as an active sova/1 peer too"
fi
C_A_DIR="$(strip_ansi "${WORK_DIR}/node-c.log" | grep 'sova/1: connection established' | grep "0x${ID_A}" \
  | grep -o 'direction=[A-Za-z]*' | head -1)"
echo "C<->A session (C's side): ${C_A_DIR:-<not logged>}"

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
# (2) lockstep
# ============================================================
echo ""
echo "=== (2) lockstep sampling (B, C vs A) ==="
# Lockstep is a steady-state property: first let B and C finish catching
# up from the 101-block funding burst (a slow CI runner can trail by a
# dozen blocks for a few seconds), then sample. Never converging is a
# failure of its own.
CATCHUP_DEADLINE=$((SECONDS + 90))
until (( SECONDS >= CATCHUP_DEADLINE )); do
  A_BLOCK="$(eth_block_number "${ENGINE_RPC_A}")"
  B_BLOCK="$(eth_block_number "${ENGINE_RPC_B}")"
  C_BLOCK="$(eth_block_number "${ENGINE_RPC_C}")"
  (( A_BLOCK - B_BLOCK <= 1 && A_BLOCK - C_BLOCK <= 1 )) && break
  sleep 1
done
if (( A_BLOCK - B_BLOCK <= 1 && A_BLOCK - C_BLOCK <= 1 )); then
  pass "(2) B and C caught up with A after the funding burst (A=${A_BLOCK} B=${B_BLOCK} C=${C_BLOCK})"
else
  fail "(2) B/C never caught up within 90s (A=${A_BLOCK} B=${B_BLOCK} C=${C_BLOCK})"
fi
for i in 1 2 3; do
  A_BLOCK="$(eth_block_number "${ENGINE_RPC_A}")"
  B_BLOCK="$(eth_block_number "${ENGINE_RPC_B}")"
  C_BLOCK="$(eth_block_number "${ENGINE_RPC_C}")"
  LAG_B=$((A_BLOCK - B_BLOCK))
  LAG_C=$((A_BLOCK - C_BLOCK))
  # Nodes are read one after another, so a block can land between reads:
  # a follower may read one ahead of A as well as one behind.
  if [[ "${A_BLOCK}" -gt 0 && "${LAG_B}" -ge -1 && "${LAG_B}" -le 1 && "${LAG_C}" -ge -1 && "${LAG_C}" -le 1 ]]; then
    pass "(2) sample ${i}/3: A=${A_BLOCK} B=${B_BLOCK} C=${C_BLOCK}"
  else
    fail "(2) sample ${i}/3: A=${A_BLOCK} B=${B_BLOCK} C=${C_BLOCK} -- lag outside [0,1]"
  fi
  [[ ${i} -lt 3 ]] && sleep 5
done

# ============================================================
# (3) block-hash equality
# ============================================================
echo ""
echo "=== (3) block-hash equality ==="
TIP_A="$(eth_block_number "${ENGINE_RPC_A}")"
HEIGHTS="$(printf '%s\n' 1 "${SETTLED_HEIGHT}" $((SETTLED_HEIGHT + 1)) "${TIP_A}" | sort -n | uniq)"
MAX_HEIGHT="$(echo "${HEIGHTS}" | tail -1)"
for url in "${ENGINE_RPC_B}" "${ENGINE_RPC_C}"; do
  wait_for_block_number "${url}" "${MAX_HEIGHT}" 60 || fail "(3) ${url} never reached height ${MAX_HEIGHT}"
done
for h in ${HEIGHTS}; do
  HASH_A="$(eth_block_hash "${ENGINE_RPC_A}" "${h}")"
  HASH_B="$(eth_block_hash "${ENGINE_RPC_B}" "${h}")"
  HASH_C="$(eth_block_hash "${ENGINE_RPC_C}" "${h}")"
  if [[ -n "${HASH_A}" && "${HASH_A}" == "${HASH_B}" && "${HASH_A}" == "${HASH_C}" ]]; then
    pass "(3) height ${h}: identical on A, B, C (${HASH_A})$([[ "${h}" == "${SETTLED_HEIGHT}" ]] && echo " -- the settled epoch")"
  else
    fail "(3) height ${h}: A=${HASH_A:-<none>} B=${HASH_B:-<none>} C=${HASH_C:-<none>}"
  fi
done

# ============================================================
# (4) balance equality
# ============================================================
echo ""
echo "=== (4) miner balance equality ==="
BAL_A_FINAL="$(eth_balance_wei "${ENGINE_RPC_A}" "${EVM_ADDR}")"
BAL_B_FINAL="$(eth_balance_wei "${ENGINE_RPC_B}" "${EVM_ADDR}")"
BAL_C_FINAL="$(eth_balance_wei "${ENGINE_RPC_C}" "${EVM_ADDR}")"
if [[ "${BAL_A_FINAL}" != "0" && "${BAL_A_FINAL}" == "${BAL_B_FINAL}" && "${BAL_A_FINAL}" == "${BAL_C_FINAL}" ]]; then
  pass "(4) miner balance: A == B == C == ${BAL_A_FINAL} wei"
else
  fail "(4) miner balance mismatch: A=${BAL_A_FINAL} B=${BAL_B_FINAL} C=${BAL_C_FINAL}"
fi

# ============================================================
# (5) transport
# ============================================================
echo ""
echo "=== (5) transport evidence ==="
# reth's buffered log writer can land the last few lines late: poll the
# count (15s) instead of reading it once.
ACCEPTED_DEADLINE=$((SECONDS + 15))
while :; do
  ACCEPTED_C="$(strip_ansi "${WORK_DIR}/node-c.log" | grep -c 'sova/1: peer block accepted' || true)"
  (( ACCEPTED_C >= MAX_HEIGHT || SECONDS >= ACCEPTED_DEADLINE )) && break
  sleep 1
done
ACCEPTED_C_FROM_A="$(strip_ansi "${WORK_DIR}/node-c.log" | grep 'sova/1: peer block accepted' | grep -c "peer=0x${ID_A}" || true)"
ACCEPTED_C_FROM_B="$(strip_ansi "${WORK_DIR}/node-c.log" | grep 'sova/1: peer block accepted' | grep -c "peer=0x${ID_B}" || true)"
echo "C accepted ${ACCEPTED_C} block(s) over sova/1: ${ACCEPTED_C_FROM_A} from A, ${ACCEPTED_C_FROM_B} from B"
if [[ "${ACCEPTED_C}" -ge "${MAX_HEIGHT}" ]]; then
  pass "(5) C accepted ${ACCEPTED_C} block(s) over sova/1 (>= ${MAX_HEIGHT} heights)"
else
  fail "(5) C accepted only ${ACCEPTED_C} block(s) over sova/1 (expected >= ${MAX_HEIGHT})"
fi
if [[ "${ACCEPTED_C_FROM_A}" -ge 1 ]]; then
  pass "(5) ${ACCEPTED_C_FROM_A} of them came straight from A over the discovered session"
else
  fail "(5) no block reached C directly from A"
fi
for n in b c; do
  if [[ "$(strip_ansi "${WORK_DIR}/node-${n}.log" | grep -c 'reputation hit' || true)" -gt 0 ]]; then
    fail "(5) node ${n} penalised a peer"
  fi
done
pass "(5) reputation hits checked on B and C"

# ============================================================
# (6) locality
# ============================================================
echo ""
echo "=== (6) no non-local sockets ==="
kill "${SOCKET_SAMPLER_PID}" 2>/dev/null || true
wait "${SOCKET_SAMPLER_PID}" 2>/dev/null || true
SOCKET_SAMPLER_PID=""
SAMPLES="$(wc -l <"${WORK_DIR}/sockets.txt" | tr -d ' ')"
UDP_BINDS="$(awk '$8 == "UDP" {print $2, $9}' "${WORK_DIR}/sockets.txt" | sort -u | tr '\n' ' ')"
echo "sampled ${SAMPLES} socket line(s); UDP binds (pid addr): ${UDP_BINDS:-<none>}"
BAD="$(non_local_sockets "${WORK_DIR}/sockets.txt")"
if [[ "${SAMPLES}" -gt 0 && -z "${BAD}" ]]; then
  pass "(6) every TCP/UDP socket of A, B, C was loopback-bound and loopback-connected"
else
  fail "(6) non-local sockets seen: ${BAD:-<no samples>}"
fi
if [[ -n "${UDP_BINDS}" ]] && ! grep -qv '127.0.0.1' <<<"$(awk '$8 == "UDP" {print $9}' "${WORK_DIR}/sockets.txt")"; then
  pass "(6) discovery UDP sockets bound to 127.0.0.1 only (a loopback-bound socket cannot send off-host)"
else
  fail "(6) discovery UDP sockets missing or not loopback-bound"
fi
if [[ -n "${SOVA_P2P_SIM_KEEP_LOGS:-}" ]]; then
  mkdir -p "${SOVA_P2P_SIM_KEEP_LOGS}"
  cp "${WORK_DIR}/sockets.txt" "${SOVA_P2P_SIM_KEEP_LOGS}/" 2>/dev/null || true
fi

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "THREE-NODE DISCOVERY SCENARIO PASSED (all assertions)"
else
  echo "THREE-NODE DISCOVERY SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
