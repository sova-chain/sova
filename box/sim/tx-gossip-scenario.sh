#!/usr/bin/env bash
# Transaction gossip: a transaction submitted to the public RPC node must
# reach the sealer and be mined (public testnet incident 2026-09-25: the
# deployer's first tx sat in sova-rpc-1's pool for over an hour while the
# seed and the keeper never saw it).
#
# The testnet's shape, on loopback, over sova/1 + devp2p `eth` (static
# peering, discovery off):
#
#   node A -- "keeper": mine mode, the only block producer.
#   node B -- "seed": follow-only, static peer = A, persistent datadir.
#   node C -- "rpc": follow-only, SOVA_RPC_PROFILE=public, static peer = B
#             ONLY (never talks to A), persistent datadir.
#
# Every transaction is signed offline (cast mktx, reth dev-genesis keys)
# and submitted to C's public HTTP RPC with eth_sendRawTransaction; it has
# to travel C -> B -> A in reth's `eth` transaction gossip to be mined.
#
# Assertions:
#   (0) setup    -- three nodes up, sova/1 sessions C-B and B-A, chain moving.
#   (1) live     -- all nodes long past startup: a tx to C reaches B's and
#                   A's pools and is mined. (The baseline.)
#   (2) receiver -- B restarts while the chain is paused (no new block, as
#                   between two Zcash blocks or during a Zcash stall). A tx
#                   sent to C then must still reach B, and be mined once
#                   the chain moves again. Without the fix B's network sits
#                   in reth's "initially syncing" state from start until
#                   its first canonical-chain commit, drops every
#                   transaction announcement in that window, and C (which
#                   marked the hash as seen by B) never announces it again.
#   (3) sender   -- C holds a local tx it could not send (it was started
#                   with no peers), restarts with its peer, and restores
#                   the tx from its datadir backup (as sova-rpc-1 did).
#                   The tx must reach B and A and be mined. Without the fix
#                   reth announces a pool only when a session is
#                   established, and skips that while "initially syncing",
#                   which a restarted node is when its static peers connect.
#
# The fix is bin/sova/src/tx_gossip.rs (network Idle from engine start;
# the pending pool re-announced to every peer every 15 s). Before it, (2)
# and (3) failed exactly like the testnet: the tx stayed in C's pool alone,
# every node answering eth_syncing=false, while 70+ blocks went by.
#
# Isolation/teardown: see box/sim/p2p-common.sh (own ports, own compose
# project, PID-scoped teardown), plus C's isolated RLPx port (phase 3,
# SOVA_TXG_C_ISOLATED_P2P_PORT, default 30414). Needs foundry's `cast`.

SCENARIO="tx-gossip"
WORK_PREFIX="sova-tx-gossip"
P2P_THREE_NODES=1
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"
trap cleanup EXIT

AUTO_MINE_INTERVAL_S=2
CHAIN_ID=1337
# reth's dev genesis (bin/sova/src/chain.rs `dev`): accounts 0, 2, 3 of the
# public "test test ... junk" mnemonic, one per phase so nonces never
# interfere. Local use only.
KEY_LIVE="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
ADDR_LIVE="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
KEY_RECV="0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a"
ADDR_RECV="0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"
KEY_SEND="0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6"
ADDR_SEND="0x90F79bf6EB2c4f870365E785982E1f101E93b906"
# Mine mode needs a miner address; no burn ever credits it here.
MINER_EVM_ADDR="0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
SINK="0x000000000000000000000000000000000000dEaD"
# Enough for the pool's view and our debugging; reth's tx gossip logs at
# debug/trace under `net::tx`.
NODE_LOG_FILTER="${TXG_RUST_LOG:-info,net::tx=debug}"

# Node C's RLPx port while isolated (phase 3): one nobody knows.
C_ISOLATED_P2P_PORT="${SOVA_TXG_C_ISOLATED_P2P_PORT:-30414}"

B_DATADIR=""
C_DATADIR=""

if ! command -v cast >/dev/null 2>&1; then
  echo "error: this scenario needs foundry's \`cast\` on PATH (signs the dev-key transactions)" >&2
  exit 1
fi

# --- helpers ------------------------------------------------------------

height_of() { eth_block_number "$1" 2>/dev/null || echo 0; }

# eth_getTransactionCount(addr, "pending") as a decimal; -1 if unreadable.
pending_nonce() {
  eth_rpc "$1" eth_getTransactionCount "[\"$2\", \"pending\"]" | python3 -c "
import sys, json
try:
    print(int(json.load(sys.stdin)['result'], 16))
except Exception:
    print(-1)
" 2>/dev/null || echo -1
}

# eth_syncing, compact: "false" or "syncing".
syncing_of() {
  eth_rpc "$1" eth_syncing "[]" | python3 -c "
import sys, json
try:
    r = json.load(sys.stdin)['result']
    print('false' if r is False else 'syncing')
except Exception:
    print('error')
" 2>/dev/null || echo error
}

wait_for_pending_nonce() { # <url> <addr> <want> <timeout_s>
  local deadline=$((SECONDS + $4)) n
  while :; do
    n="$(pending_nonce "$1" "$2")"
    [[ "${n}" -ge "$3" ]] && return 0
    [[ ${SECONDS} -ge ${deadline} ]] && return 1
    sleep 1
  done
}

# Receipt "blockNumber status" (decimal); empty until mined.
receipt_of() {
  eth_rpc "$1" eth_getTransactionReceipt "[\"$2\"]" | python3 -c "
import sys, json
try:
    r = json.load(sys.stdin).get('result')
except Exception:
    r = None
if r:
    print(int(r['blockNumber'], 16), int(r['status'], 16))
" 2>/dev/null
}

wait_for_receipt() { # <url> <txhash> <timeout_s>
  local deadline=$((SECONDS + $3)) r
  while :; do
    r="$(receipt_of "$1" "$2")"
    if [[ -n "${r}" ]]; then
      echo "${r}"
      return 0
    fi
    [[ ${SECONDS} -ge ${deadline} ]] && return 1
    sleep 1
  done
}

# Sign a 1-wei transfer offline (nonce 0: each phase has its own fresh
# account) and submit it to $1 with eth_sendRawTransaction. Prints the tx
# hash, or nothing if the node refused it.
send_raw() { # <url> <key>
  local raw resp
  raw="$(cast mktx --private-key "$2" --chain "${CHAIN_ID}" --nonce 0 --gas-limit 21000 \
    --gas-price 10gwei --priority-gas-price 1gwei "${SINK}" --value 1 2>/dev/null)" || return 1
  resp="$(eth_rpc "$1" eth_sendRawTransaction "[\"${raw}\"]")"
  python3 -c "
import sys, json
d = json.loads(sys.argv[1])
if 'result' in d:
    print(d['result'])
else:
    print('send_raw: ' + json.dumps(d.get('error', d)), file=sys.stderr)
" "${resp}"
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

# SIGTERM a node and wait (up to 60s) for it to exit; SIGKILL after that.
# SIGTERM is bin/sova's graceful path, which is also when reth writes the
# local-transaction backup the sender phase relies on.
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

wait_for_stable_height() { # <url> <timeout_s>; prints the height
  local deadline=$((SECONDS + $2)) cur prev=-1
  while :; do
    cur="$(height_of "$1")"
    if [[ "${cur}" -eq "${prev}" ]]; then
      echo "${cur}"
      return 0
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "${cur}"
      return 1
    fi
    prev="${cur}"
    sleep 4
  done
}

start_b() { # <log>
  p2p_env \
    RUST_LOG="${NODE_LOG_FILTER}" \
    SOVA_FOLLOW_ONLY=1 \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
    SOVA_EPOCH_BASE=1 \
    SOVA_DATADIR="${B_DATADIR}" \
    SOVA_P2P_PEERS="${ENODE_A}" \
    SOVA_HTTP_PORT="${B_HTTP_PORT}" \
    SOVA_AUTH_PORT="${B_AUTH_PORT}" \
    SOVA_P2P_PORT="${B_P2P_PORT}" \
    "${SOVA_BIN}" >"$1" 2>&1 &
  B_PID=$!
}

# C with its static peer B, or with none ("isolated"); $3 overrides the
# RLPx port.
start_c() { # <log> <peers> [p2p_port]
  p2p_env \
    RUST_LOG="${NODE_LOG_FILTER}" \
    SOVA_FOLLOW_ONLY=1 \
    SOVA_RPC_PROFILE=public \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
    SOVA_EPOCH_BASE=1 \
    SOVA_DATADIR="${C_DATADIR}" \
    SOVA_P2P_PEERS="$2" \
    SOVA_HTTP_PORT="${C_HTTP_PORT}" \
    SOVA_AUTH_PORT="${C_AUTH_PORT}" \
    SOVA_P2P_PORT="${3:-${C_P2P_PORT}}" \
    "${SOVA_BIN}" >"$1" 2>&1 &
  C_PID=$!
}

state_line() {
  echo "  heights A=$(height_of "${ENGINE_RPC_A}") B=$(height_of "${ENGINE_RPC_B}") C=$(height_of "${ENGINE_RPC_C}");" \
    "eth_syncing A=$(syncing_of "${ENGINE_RPC_A}") B=$(syncing_of "${ENGINE_RPC_B}") C=$(syncing_of "${ENGINE_RPC_C}")"
}

# A tx sent to C: pools of B and A, then mined (receipt on A and on C).
# $1 phase label, $2 tx hash, $3 sender, $4 pool timeout, $5 mine timeout.
check_tx_travels() {
  local label="$1" tx="$2" addr="$3" pool_s="$4" mine_s="$5" r
  if wait_for_pending_nonce "${ENGINE_RPC_B}" "${addr}" 1 "${pool_s}"; then
    pass "${label}: tx reached B (seed) pool: pending nonce 1"
  else
    fail "${label}: tx never reached B (seed) within ${pool_s}s: pending nonce $(pending_nonce "${ENGINE_RPC_B}" "${addr}") (C has $(pending_nonce "${ENGINE_RPC_C}" "${addr}"))"
  fi
  if r="$(wait_for_receipt "${ENGINE_RPC_A}" "${tx}" "${mine_s}")"; then
    pass "${label}: tx mined by A (keeper) in block ${r% *} (status ${r#* })"
  else
    fail "${label}: tx ${tx} not mined within ${mine_s}s (pending nonce A=$(pending_nonce "${ENGINE_RPC_A}" "${addr}") B=$(pending_nonce "${ENGINE_RPC_B}" "${addr}") C=$(pending_nonce "${ENGINE_RPC_C}" "${addr}"))"
    state_line
    return
  fi
  if wait_for_receipt "${ENGINE_RPC_C}" "${tx}" 60 >/dev/null; then
    pass "${label}: C (rpc) serves the receipt"
  else
    fail "${label}: C (rpc) never served the receipt"
  fi
}

preflight
start_stack
B_DATADIR="${WORK_DIR}/datadir-b"
C_DATADIR="${WORK_DIR}/datadir-c"

# ---------------------------------------------------------------------
# (0) setup
# ---------------------------------------------------------------------
echo "--- starting node A (keeper: mine mode, no static peers) ---"
p2p_env \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_MINER_EVM_ADDRESS="${MINER_EVM_ADDR}" \
  SOVA_EPOCH_BASE=1 \
  SOVA_HTTP_PORT="${A_HTTP_PORT}" \
  SOVA_AUTH_PORT="${A_AUTH_PORT}" \
  SOVA_P2P_PORT="${A_P2P_PORT}" \
  RUST_LOG="${NODE_LOG_FILTER}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-a.log" 2>&1 &
A_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_A}" "${A_PID}" "node A" || exit 1
ENODE_A="$(local_enode "${WORK_DIR}/node-a.log")" || {
  fail "setup: node A never printed its enode"
  exit 1
}
echo "node A up (pid ${A_PID}); enode ${ENODE_A}"

echo "--- starting node B (seed: follow-only, static peer A) ---"
start_b "${WORK_DIR}/node-b.log"
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B" || exit 1
ENODE_B="$(local_enode "${WORK_DIR}/node-b.log")" || {
  fail "setup: node B never printed its enode"
  exit 1
}
echo "node B up (pid ${B_PID}); enode ${ENODE_B}"

echo "--- starting node C (rpc: follow-only, public RPC profile, static peer B only) ---"
start_c "${WORK_DIR}/node-c.log" "${ENODE_B}"
wait_for_eth_rpc "${ENGINE_RPC_C}" "${C_PID}" "node C" || exit 1
echo "node C up (pid ${C_PID})"

echo ""
echo "=== (0) setup ==="
if log_has "${WORK_DIR}/node-c.log" "rpc profile: public" 15; then
  pass "setup: node C serves the public RPC profile"
else
  fail "setup: node C's log lacks 'rpc profile: public'"
fi
if wait_for_sova_peer_id "${WORK_DIR}/node-a.log" "$(enode_id "${ENODE_B}")" 60 \
  && wait_for_sova_peer "${WORK_DIR}/node-c.log" 60; then
  pass "setup: sova/1 sessions A<->B and B<->C established"
else
  fail "setup: sova/1 sessions not established within 60s"
  exit 1
fi
start_auto_mine
if wait_for_block_number "${ENGINE_RPC_A}" 3 90 && wait_for_block_number "${ENGINE_RPC_C}" 3 90; then
  pass "setup: chain moving and relayed A -> B -> C (A=$(height_of "${ENGINE_RPC_A}") C=$(height_of "${ENGINE_RPC_C}"))"
else
  fail "setup: chain did not reach height 3 on A and C (A=$(height_of "${ENGINE_RPC_A}") C=$(height_of "${ENGINE_RPC_C}"))"
  exit 1
fi
state_line

# ---------------------------------------------------------------------
# (1) live
# ---------------------------------------------------------------------
echo ""
echo "=== (1) live: tx to C with every node long past startup ==="
sleep 6
TX1="$(send_raw "${ENGINE_RPC_C}" "${KEY_LIVE}")"
if [[ -z "${TX1}" ]]; then
  fail "(1) C refused the tx"
else
  echo "tx ${TX1} submitted to C"
  check_tx_travels "(1) live" "${TX1}" "${ADDR_LIVE}" 30 90
fi

# ---------------------------------------------------------------------
# (2) receiver restarted while the chain is paused
# ---------------------------------------------------------------------
echo ""
echo "=== (2) receiver: B restarts while no new block arrives ==="
stop_auto_mine
PAUSED="$(wait_for_stable_height "${ENGINE_RPC_A}" 60)" || fail "(2) A's height did not settle"
wait_for_block_number "${ENGINE_RPC_B}" "${PAUSED}" 30 || true
wait_for_block_number "${ENGINE_RPC_C}" "${PAUSED}" 30 || true
echo "chain paused at ${PAUSED}"
stop_node "${B_PID}" "node B"
B_PID=""
start_b "${WORK_DIR}/node-b-restart.log"
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B (restarted)" || exit 1
if wait_for_sova_peer_id "${WORK_DIR}/node-b-restart.log" "$(enode_id "${ENODE_A}")" 60 \
  && wait_for_sova_peer "${WORK_DIR}/node-b-restart.log" 60; then
  # Both of B's sessions: A (dialed by B) and C (C re-dials its static peer).
  N_B_PEERS="$(sova_active_peers "${WORK_DIR}/node-b-restart.log" | wc -l | tr -d ' ')"
  DEADLINE=$((SECONDS + 60))
  while [[ "${N_B_PEERS}" -lt 2 && ${SECONDS} -lt ${DEADLINE} ]]; do
    sleep 1
    N_B_PEERS="$(sova_active_peers "${WORK_DIR}/node-b-restart.log" | wc -l | tr -d ' ')"
  done
  if [[ "${N_B_PEERS}" -ge 2 ]]; then
    pass "(2) restarted B has sova/1 sessions with A and C (still at height $(height_of "${ENGINE_RPC_B}"))"
  else
    fail "(2) restarted B has ${N_B_PEERS} sova/1 session(s), want 2 (A and C)"
  fi
else
  fail "(2) restarted B never re-established sova/1 with A"
fi
sleep 3
state_line
TX2="$(send_raw "${ENGINE_RPC_C}" "${KEY_RECV}")"
if [[ -z "${TX2}" ]]; then
  fail "(2) C refused the tx"
else
  echo "tx ${TX2} submitted to C (chain still paused)"
  # Chain paused: the pool is the only place the tx can be. Then let the
  # chain move and require it mined.
  if wait_for_pending_nonce "${ENGINE_RPC_B}" "${ADDR_RECV}" 1 20; then
    pass "(2) tx reached the restarted B's pool before any new block"
  else
    fail "(2) restarted B dropped the tx announcement (pending nonce $(pending_nonce "${ENGINE_RPC_B}" "${ADDR_RECV}"); C has $(pending_nonce "${ENGINE_RPC_C}" "${ADDR_RECV}"))"
  fi
  state_line
  start_auto_mine
  check_tx_travels "(2) receiver" "${TX2}" "${ADDR_RECV}" 60 90
fi

# ---------------------------------------------------------------------
# (3) sender restarts holding a local tx it never sent
# ---------------------------------------------------------------------
echo ""
echo "=== (3) sender: C restarts with a restored local tx ==="
stop_node "${C_PID}" "node C"
C_PID=""
echo "--- C restarted with NO peers; tx submitted there ---"
# Really isolated: reth reloads the peers it saved at shutdown
# (known-peers.json) and B re-dials C's known address, so C also forgets
# its peers and listens on another port for this run.
find "${C_DATADIR}" -name known-peers.json -delete
start_c "${WORK_DIR}/node-c-isolated.log" "" "${C_ISOLATED_P2P_PORT}"
wait_for_eth_rpc "${ENGINE_RPC_C}" "${C_PID}" "node C (isolated)" || exit 1
TX3="$(send_raw "${ENGINE_RPC_C}" "${KEY_SEND}")"
if [[ -z "${TX3}" ]]; then
  fail "(3) isolated C refused the tx"
  exit 1
fi
echo "tx ${TX3} submitted to isolated C"
if [[ "$(pending_nonce "${ENGINE_RPC_C}" "${ADDR_SEND}")" -eq 1 && "$(pending_nonce "${ENGINE_RPC_B}" "${ADDR_SEND}")" -eq 0 ]]; then
  pass "(3) tx held only by C (C pending nonce 1, B 0)"
else
  fail "(3) unexpected pools: C=$(pending_nonce "${ENGINE_RPC_C}" "${ADDR_SEND}") B=$(pending_nonce "${ENGINE_RPC_B}" "${ADDR_SEND}")"
fi
if [[ "$(sova_active_peers "${WORK_DIR}/node-c-isolated.log" | wc -l | tr -d ' ')" -eq 0 ]]; then
  pass "(3) isolated C has no peers"
else
  fail "(3) isolated C has a sova/1 peer"
fi
sleep 2
stop_node "${C_PID}" "node C (isolated)"
C_PID=""
echo "--- C restarted with its static peer B ---"
start_c "${WORK_DIR}/node-c-restart.log" "${ENODE_B}"
wait_for_eth_rpc "${ENGINE_RPC_C}" "${C_PID}" "node C (restarted)" || exit 1
if [[ "$(pending_nonce "${ENGINE_RPC_C}" "${ADDR_SEND}")" -eq 1 ]]; then
  pass "(3) C restored the local tx from its datadir backup"
else
  fail "(3) C did not restore the local tx (pending nonce $(pending_nonce "${ENGINE_RPC_C}" "${ADDR_SEND}"))"
fi
if wait_for_sova_peer "${WORK_DIR}/node-c-restart.log" 60; then
  pass "(3) restarted C has its sova/1 session with B"
else
  fail "(3) restarted C never re-established sova/1 with B"
fi
state_line
check_tx_travels "(3) sender" "${TX3}" "${ADDR_SEND}" 60 90
state_line

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "TX GOSSIP SCENARIO PASSED (all assertions)"
else
  echo "TX GOSSIP SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi
[[ "${FAILURES}" -eq 0 ]]
