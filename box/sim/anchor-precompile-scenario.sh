#!/usr/bin/env bash
# SIP-4: the Zcash query precompile's anchor() is DETERMINISTIC across
# nodes and import paths (sips/sip-4-draft-zcash-state-precompile.md §1,
# §3; crates/evm/src/zcash.rs; docs/design/sip4-evm-seam.md).
#
# anchor() at 0x…5A00 returns (uint64 E_N, bytes32 hash) for the executing
# block N, with E_N = N + B − 1 and hash the Zcash block at E_N as this
# node's own follower indexed it. Every Sova block also commits to that
# Zcash block in parent_beacon_block_root (SIP-4 §1). This scenario makes
# the answer consensus-visible: a tiny recorder contract STATICCALLs
# anchor() and SSTOREs the result keyed by block number, so any node
# that computed a different answer computes a different state root.
#
#   node A -- mine mode (the only producer), sova/1, dev profile.
#   node B -- follow-only, C5-enforcing, sova/1 static peer = A. Imports
#             every block over sova/1 (in-process new_payload) and
#             executes the precompile against ITS OWN follower's index.
#   node J -- late joiner: follow-only, EMPTY datadir, static peer = A,
#             started only after every recorder block exists (a short
#             JOIN_DEPTH past the last one, inside sova/1's 33-block
#             chase window). It syncs through those blocks on whatever
#             path the engine takes (sova/1 chase, engine-tree download)
#             and re-executes every anchor() call during import.
#
# All three share one regtest zebrad (as two-node-p2p-scenario.sh does):
# the precompile answers from each node's own follower, and three
# independent followers over one Zcash chain is what must agree. (Two
# unpeered regtest zebrads would be two different Zcash chains, and every
# anchor would legitimately differ.)
#
# The recorder (inline bytecode, 57-byte runtime; any calldata runs it):
#   mstore(0, 0xd3fb73b4 << 224)
#   ok := staticcall(gas(), 0x5A00, 0, 4, 0, 0x40)
#   if iszero(ok) { revert(0, 0) }
#   sstore(2*number(),     mload(0x00))   // E_N
#   sstore(2*number() + 1, mload(0x20))   // zcash hash
#   return(0, 0x40)
#
# Assertions:
#   (0) setup    -- all three run sova/1 (authrpc loopback, no relay), B
#                   and J enforce C5, J starts empty, sessions are up, the
#                   recorder's code is identical on A and B.
#   (1) recorder -- >= MIN_CONSECUTIVE recorder txs landed in consecutive
#                   blocks, every receipt status 1.
#   (a) for EVERY recorder block N: block hash AND stateRoot identical on
#       A, B and J; the receipt's block hash and status identical on all
#       three.
#   (b) for every recorder block N, on A, B and J: storage[2N] == N+B−1,
#       storage[2N+1] == that block's parent_beacon_block_root == zebrad's
#       getblockhash(N+B−1). Control: the block before the first recorder
#       block has storage[2N] == 0.
#   (c) eth_call of anchor() directly at 0x5A00 at historical block N
#       returns abi(N+B−1, root_N) on A, B and J -- the RPC path, and the
#       answer follows the block number (no cross-block caching).
#   (d) J caught up from an empty datadir and all three end on the same
#       block hash and stateRoot at a common tip; no reputation hits.
#   Diagnostics (printed, not asserted): each node's count of refused
#   precompile calls ("sova zcash precompile" fatal lines -- a block
#   executed before the node's follower indexed its epoch is refused and
#   retried, SIP-4 §5), A's skipped builds, and anchor() at "pending".
#
# Needs `cast` (foundry) on PATH to sign transactions with reth's dev key.
# Env: MIN_CONSECUTIVE (5), MAX_RECORDER_TXS (14), JOIN_DEPTH (5),
# JOIN_TIMEOUT_S (180), PRE_BLOCKS (10 Zcash blocks before the epoch base,
# so B != 1), plus p2p-common.sh's SOVA_P2P_SIM_*, SOVA_BIN,
# SOVA_P2P_SIM_KEEP_LOGS.
#
# Isolation: zebrad on :18362 (compose project sova-anchor-sim, container
# sova-zebrad-anchor), A on 9745/9751/30711, B on 9845/9851/30712, J on
# 9945/9951/30713 (J uses p2p-common's "C" slot). All overridable.

SCENARIO="anchor-precompile"
WORK_PREFIX="sova-anchor"
P2P_THREE_NODES=1
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18362}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-anchor}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-anchor-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=9745 9751 30711}"
: "${SOVA_P2P_SIM_B_PORTS:=9845 9851 30712}"
: "${SOVA_P2P_SIM_C_PORTS:=9945 9951 30713}"
# No burns here, so sova-miner is never run: point preflight at a no-op
# instead of letting it build one.
: "${SOVA_MINER_BIN:=$(type -P true)}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS SOVA_P2P_SIM_C_PORTS SOVA_MINER_BIN
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"
trap cleanup EXIT

MIN_CONSECUTIVE="${MIN_CONSECUTIVE:-5}"
MAX_RECORDER_TXS="${MAX_RECORDER_TXS:-14}"
JOIN_DEPTH="${JOIN_DEPTH:-5}"
JOIN_TIMEOUT_S="${JOIN_TIMEOUT_S:-180}"
PRE_BLOCKS="${PRE_BLOCKS:-10}"
AUTO_MINE_INTERVAL_S=2
CHASE_WINDOW=33
# reth's dev chain: account 0 of the public "test test ... junk" mnemonic,
# prefunded in the dev genesis (bin/sova/src/chain.rs). Local use only.
DEV_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
# Any address: mine mode needs one, and no burn ever credits it here.
MINER_EVM_ADDR="0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
ZCASH_QUERY="0x0000000000000000000000000000000000005A00"
ANCHOR_SELECTOR="0xd3fb73b4"
RECORDER_RUNTIME="63d3fb73b460e01b6000526040600060046000615a005afa156034576000514360011b556020514360011b6001175560406000f35b600080fd"
# CODECOPY the 0x39-byte runtime from offset 0x0b and RETURN it.
RECORDER_INIT="0x603980600b6000396000f3${RECORDER_RUNTIME}"

ENGINE_RPC_J="${ENGINE_RPC_C}"
J_LOG_NAME="node-j.log"

if ! command -v cast >/dev/null 2>&1; then
  echo "error: this scenario needs foundry's \`cast\` on PATH (signs the dev-key transactions)" >&2
  exit 1
fi

# --- helpers ------------------------------------------------------------

height_of() { eth_block_number "$1" 2>/dev/null || echo 0; }

count_in() {
  local n
  n="$(sed 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | grep -cF -- "$2" || true)"
  echo "${n:-0}"
}

hexnum() { printf '0x%x' "$1"; }

# One field of eth_getBlockByNumber(N) (hash, stateRoot, parentBeaconBlockRoot),
# lowercased; empty if the node lacks the block.
block_field() {
  eth_rpc "$1" eth_getBlockByNumber "[\"$(hexnum "$2")\", false]" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin).get('result')
    print((d.get('$3') or '').lower() if d else '')
except Exception:
    print('')
"
}

# eth_getStorageAt(addr, slot) at block N, as a lowercased 0x + 64-hex word.
storage_at() {
  eth_rpc "$1" eth_getStorageAt "[\"$2\", \"$(hexnum "$3")\", \"$(hexnum "$4")\"]" | python3 -c "
import sys, json
try:
    r = json.load(sys.stdin).get('result')
    print('0x' + format(int(r, 16), '064x') if r else '')
except Exception:
    print('')
"
}

# eth_call anchor() at 0x5A00 at block tag/number $2: the 128-hex result
# (no 0x), or ERR:<message>.
anchor_call() {
  local tag="$2"
  [[ "${tag}" =~ ^[0-9]+$ ]] && tag="$(hexnum "${tag}")"
  eth_rpc "$1" eth_call "[{\"to\":\"${ZCASH_QUERY}\",\"data\":\"${ANCHOR_SELECTOR}\"}, \"${tag}\"]" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
except Exception as e:
    print('ERR:unparseable response'); sys.exit()
if 'result' in d:
    print(d['result'][2:].lower())
else:
    print('ERR:' + str(d.get('error', {}).get('message', d)))
"
}

# Receipt fields "blockNumber status blockHash contractAddress" (decimal
# block number); empty until mined.
receipt_of() {
  eth_rpc "$1" eth_getTransactionReceipt "[\"$2\"]" | python3 -c "
import sys, json
try:
    r = json.load(sys.stdin).get('result')
except Exception:
    r = None
if r:
    print(int(r['blockNumber'], 16), int(r['status'], 16), r['blockHash'].lower(), (r.get('contractAddress') or '-').lower())
"
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
    sleep 0.5
  done
}

# Sign with the dev key and submit to node A; prints the tx hash. Explicit
# gas limit: no eth_estimateGas round trip.
send_tx() {
  cast send --async --rpc-url "${ENGINE_RPC_A}" --private-key "${DEV_KEY}" --gas-limit "$1" "${@:2}" 2>&1 \
    | grep -oE '0x[0-9a-fA-F]{64}' | tail -1
}

zc_block_hash_at() {
  zc_rpc getblockhash "[$1]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'].lower())" 2>/dev/null || true
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
    sleep 2
  done
}

preflight
start_stack

# ---------------------------------------------------------------------
# Epoch base after some Zcash history, so B != 1 and E_N = N + B − 1 is a
# real test of the formula, not an identity.
# ---------------------------------------------------------------------
echo "--- ${PRE_BLOCKS} Zcash block(s) before the epoch base ---"
"${REGTEST_DIR}/mine.sh" "${PRE_BLOCKS}" "${ZEBRAD_RPC}" >"${WORK_DIR}/pre-mine.log" 2>&1
PRE_TIP="$(zc_tip_height)"
EPOCH_BASE=$((PRE_TIP + 1))
echo "zcash tip ${PRE_TIP}; epoch base B=${EPOCH_BASE} (Sova block N anchors Zcash N+${EPOCH_BASE}-1)"

echo "--- starting node A (mine mode, sova/1, no static peers) ---"
p2p_env \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_MINER_EVM_ADDRESS="${MINER_EVM_ADDR}" \
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

echo "--- starting node B (follow-only, sova/1 static peer = A) ---"
p2p_env \
  SOVA_FOLLOW_ONLY=1 \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_EPOCH_BASE="${EPOCH_BASE}" \
  SOVA_P2P_PEERS="${ENODE_A}" \
  SOVA_HTTP_PORT="${B_HTTP_PORT}" \
  SOVA_AUTH_PORT="${B_AUTH_PORT}" \
  SOVA_P2P_PORT="${B_P2P_PORT}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-b.log" 2>&1 &
B_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B" || exit 1
echo "node B up (pid ${B_PID})"

echo ""
echo "=== (0) setup: A and B ==="
check_p2p_node_log "node A" "${WORK_DIR}/node-a.log"
check_p2p_node_log "node B" "${WORK_DIR}/node-b.log"
if log_has "${WORK_DIR}/node-b.log" "expectations: enforcing settlements" 15; then
  pass "setup: node B enforces C5 against its own zebrad view (epoch base ${EPOCH_BASE})"
else
  fail "setup: node B isn't enforcing C5"
fi
if wait_for_sova_peer "${WORK_DIR}/node-a.log" 60 && wait_for_sova_peer "${WORK_DIR}/node-b.log" 60; then
  pass "setup: sova/1 session A<->B established"
else
  fail "setup: no sova/1 session between A and B within 60s"
  exit 1
fi

echo "--- auto-mine every ${AUTO_MINE_INTERVAL_S}s; waiting for Sova height 2 on A and B ---"
start_auto_mine
if ! wait_for_block_number "${ENGINE_RPC_A}" 2 90 || ! wait_for_block_number "${ENGINE_RPC_B}" 2 60; then
  fail "setup: chain did not start (A=$(height_of "${ENGINE_RPC_A}") B=$(height_of "${ENGINE_RPC_B}"))"
  exit 1
fi

# ---------------------------------------------------------------------
# Deploy the recorder.
# ---------------------------------------------------------------------
echo ""
echo "=== deploy the anchor recorder ==="
DEPLOY_TX="$(send_tx 300000 --create "${RECORDER_INIT}")"
if [[ -z "${DEPLOY_TX}" ]]; then
  fail "setup: deploy transaction was not accepted by node A"
  exit 1
fi
if ! DEPLOY_R="$(wait_for_receipt "${ENGINE_RPC_A}" "${DEPLOY_TX}" 90)"; then
  fail "setup: deploy tx ${DEPLOY_TX} never mined"
  exit 1
fi
read -r DEPLOY_BLOCK DEPLOY_STATUS _ RECORDER <<<"${DEPLOY_R}"
echo "recorder ${RECORDER} deployed in block ${DEPLOY_BLOCK} (status ${DEPLOY_STATUS})"
wait_for_block_number "${ENGINE_RPC_B}" "${DEPLOY_BLOCK}" 60 || true
CODE_A="$(eth_rpc "${ENGINE_RPC_A}" eth_getCode "[\"${RECORDER}\", \"latest\"]" | python3 -c "import sys,json;print(json.load(sys.stdin).get('result',''))" 2>/dev/null)"
CODE_B="$(eth_rpc "${ENGINE_RPC_B}" eth_getCode "[\"${RECORDER}\", \"latest\"]" | python3 -c "import sys,json;print(json.load(sys.stdin).get('result',''))" 2>/dev/null)"
if [[ "${DEPLOY_STATUS}" == "1" && "${CODE_A}" == "0x${RECORDER_RUNTIME}" && "${CODE_A}" == "${CODE_B}" ]]; then
  pass "setup: recorder code identical on A and B (57-byte runtime)"
else
  fail "setup: recorder deploy status=${DEPLOY_STATUS}; code A=${CODE_A:-<none>} B=${CODE_B:-<none>}"
  exit 1
fi

# ---------------------------------------------------------------------
# (1) Recorder calls in consecutive blocks: send, wait for the receipt,
# send the next right away (lands in the next block). A gap resets the
# streak; stop once MIN_CONSECUTIVE consecutive blocks carry one.
# ---------------------------------------------------------------------
echo ""
echo "=== (1) recorder txs (want ${MIN_CONSECUTIVE} consecutive blocks, at most ${MAX_RECORDER_TXS} txs) ==="
TX_BLOCKS=()
TX_HASHES=()
STREAK=0
BEST_STREAK=0
PREV_BLOCK=-1
BAD_STATUS=0
for ((i = 1; i <= MAX_RECORDER_TXS; i++)); do
  TX="$(send_tx 200000 "${RECORDER}")"
  if [[ -z "${TX}" ]]; then
    fail "(1) recorder tx ${i} was not accepted by node A"
    break
  fi
  if ! R="$(wait_for_receipt "${ENGINE_RPC_A}" "${TX}" 60)"; then
    fail "(1) recorder tx ${i} (${TX}) never mined on A within 60s"
    break
  fi
  read -r BLK STATUS _ _ <<<"${R}"
  echo "  tx ${i}: ${TX} -> block ${BLK} status ${STATUS}"
  TX_BLOCKS+=("${BLK}")
  TX_HASHES+=("${TX}")
  [[ "${STATUS}" != "1" ]] && BAD_STATUS=$((BAD_STATUS + 1))
  if [[ "${BLK}" -eq $((PREV_BLOCK + 1)) ]]; then
    STREAK=$((STREAK + 1))
  else
    STREAK=1
  fi
  PREV_BLOCK="${BLK}"
  [[ "${STREAK}" -gt "${BEST_STREAK}" ]] && BEST_STREAK="${STREAK}"
  [[ "${STREAK}" -ge "${MIN_CONSECUTIVE}" ]] && break
done
if [[ "${BEST_STREAK}" -ge "${MIN_CONSECUTIVE}" ]]; then
  pass "(1) ${#TX_BLOCKS[@]} recorder tx(s); ${BEST_STREAK} in consecutive blocks (blocks ${TX_BLOCKS[*]})"
else
  fail "(1) longest run of consecutive recorder blocks was ${BEST_STREAK} < ${MIN_CONSECUTIVE} (blocks ${TX_BLOCKS[*]:-none})"
fi
if [[ "${#TX_BLOCKS[@]}" -gt 0 && "${BAD_STATUS}" -eq 0 ]]; then
  pass "(1) every recorder receipt has status 1 (anchor() answered; the STATICCALL succeeded)"
else
  fail "(1) ${BAD_STATUS} recorder receipt(s) reverted (or none mined)"
fi
if [[ "${#TX_BLOCKS[@]}" -eq 0 ]]; then
  exit 1
fi
FIRST_TX_BLOCK="${TX_BLOCKS[0]}"
LAST_TX_BLOCK="${TX_BLOCKS[${#TX_BLOCKS[@]} - 1]}"

# ---------------------------------------------------------------------
# Grow A to LAST_TX_BLOCK + JOIN_DEPTH, freeze Zcash, start J from empty.
# ---------------------------------------------------------------------
TARGET=$((LAST_TX_BLOCK + JOIN_DEPTH))
echo ""
echo "--- growing A to Sova height ${TARGET} (JOIN_DEPTH=${JOIN_DEPTH} past the last recorder block), then freezing Zcash ---"
wait_for_block_number "${ENGINE_RPC_A}" "${TARGET}" $((60 + 4 * JOIN_DEPTH)) || true
stop_auto_mine
if ! TIP_AT_JOIN="$(wait_for_stable_height "${ENGINE_RPC_A}" "${TARGET}" 60)"; then
  fail "setup: node A did not settle at >= ${TARGET} (at ${TIP_AT_JOIN})"
  exit 1
fi
echo "A frozen at Sova ${TIP_AT_JOIN} (Zcash $(zc_tip_height)); J's gap from an empty head = ${TIP_AT_JOIN} (sova/1 chase window ${CHASE_WINDOW})"
if [[ "${TIP_AT_JOIN}" -gt "${CHASE_WINDOW}" ]]; then
  echo "NOTE: gap ${TIP_AT_JOIN} > ${CHASE_WINDOW}: J's catch-up relies on the arbiter's backfill path, not sova/1's chase"
fi

echo "--- starting node J (follow-only, EMPTY datadir, sova/1 static peer = A) ---"
J_STARTED_AT=${SECONDS}
p2p_env \
  SOVA_FOLLOW_ONLY=1 \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_EPOCH_BASE="${EPOCH_BASE}" \
  SOVA_P2P_PEERS="${ENODE_A}" \
  SOVA_HTTP_PORT="${C_HTTP_PORT}" \
  SOVA_AUTH_PORT="${C_AUTH_PORT}" \
  SOVA_P2P_PORT="${C_P2P_PORT}" \
  "${SOVA_BIN}" >"${WORK_DIR}/${J_LOG_NAME}" 2>&1 &
C_PID=$!
J_LOG="${WORK_DIR}/${J_LOG_NAME}"
wait_for_eth_rpc "${ENGINE_RPC_J}" "${C_PID}" "node J" || exit 1
echo "node J up (pid ${C_PID})"

echo ""
echo "=== (0) setup: J ==="
check_p2p_node_log "node J" "${J_LOG}"
if log_has "${J_LOG}" "expectations: enforcing settlements" 15; then
  pass "setup: node J enforces C5 against its own zebrad view"
else
  fail "setup: node J isn't enforcing C5"
fi
J_HEAD0="$(height_of "${ENGINE_RPC_J}")"
if [[ "${J_HEAD0}" -eq 0 ]]; then
  pass "setup: node J starts from an empty datadir (head 0)"
else
  fail "setup: node J did not start empty (head ${J_HEAD0})"
fi
if wait_for_sova_peer "${J_LOG}" 60; then
  pass "setup: sova/1 session A<->J established"
else
  fail "setup: no sova/1 session between A and J within 60s"
  exit 1
fi
# Resume Zcash: A's fresh announcements are what retry any block J's
# engine refused while its follower was still scanning.
start_auto_mine

echo ""
echo "=== (d) J catches up through the recorder blocks ==="
if wait_for_block_number "${ENGINE_RPC_J}" "${TIP_AT_JOIN}" "${JOIN_TIMEOUT_S}"; then
  pass "(d) J reached the join tip ${TIP_AT_JOIN} in $((SECONDS - J_STARTED_AT))s (now A=$(height_of "${ENGINE_RPC_A}") J=$(height_of "${ENGINE_RPC_J}"))"
else
  fail "(d) J did NOT reach ${TIP_AT_JOIN} within ${JOIN_TIMEOUT_S}s (A=$(height_of "${ENGINE_RPC_A}") J=$(height_of "${ENGINE_RPC_J}"))"
fi
wait_for_block_number "${ENGINE_RPC_B}" "${TIP_AT_JOIN}" 30 || fail "(d) B never reached ${TIP_AT_JOIN}"

NODES=("A ${ENGINE_RPC_A}" "B ${ENGINE_RPC_B}" "J ${ENGINE_RPC_J}")

# ============================================================
# (a) block hash + stateRoot + receipt agreement per recorder block
# ============================================================
echo ""
echo "=== (a) block hash and stateRoot identical on A, B, J at every recorder block ==="
for idx in "${!TX_BLOCKS[@]}"; do
  n="${TX_BLOCKS[${idx}]}"
  tx="${TX_HASHES[${idx}]}"
  vals=""
  for node in "${NODES[@]}"; do
    read -r label url <<<"${node}"
    vals+="${label}:$(block_field "${url}" "${n}" hash)/$(block_field "${url}" "${n}" stateRoot) "
  done
  uniq_vals="$(tr ' ' '\n' <<<"${vals}" | sed '/^$/d' | cut -d: -f2 | sort -u)"
  if [[ "$(wc -l <<<"${uniq_vals}" | tr -d ' ')" -eq 1 && "${uniq_vals}" =~ ^0x[0-9a-f]{64}/0x[0-9a-f]{64}$ ]]; then
    pass "(a) block ${n}: hash/stateRoot identical on A, B, J (${uniq_vals%%/*} / ${uniq_vals##*/})"
  else
    fail "(a) block ${n}: ${vals}"
  fi
  rvals=""
  for node in "${NODES[@]}"; do
    read -r label url <<<"${node}"
    read -r rb rs rh _ <<<"$(receipt_of "${url}" "${tx}")"
    rvals+="${label}:${rb:-?}/${rs:-?}/${rh:-?} "
  done
  if [[ "$(tr ' ' '\n' <<<"${rvals}" | sed '/^$/d' | cut -d: -f2 | sort -u | wc -l | tr -d ' ')" -eq 1 \
    && "${rvals}" == *"A:${n}/1/0x"* ]]; then
    pass "(a) block ${n}: recorder receipt identical on A, B, J (status 1)"
  else
    fail "(a) block ${n}: receipts differ: ${rvals}"
  fi
done

# ============================================================
# (b) stored anchor == (N+B-1, parent_beacon_block_root) == zebrad
# ============================================================
echo ""
echo "=== (b) stored anchor == (N+B-1, parentBeaconBlockRoot) on A, B, J ==="
for n in "${TX_BLOCKS[@]}"; do
  want_h=$((n + EPOCH_BASE - 1))
  want_h_word="0x$(printf '%064x' "${want_h}")"
  zc_hash="0x$(zc_block_hash_at "${want_h}")"
  for node in "${NODES[@]}"; do
    read -r label url <<<"${node}"
    root="$(block_field "${url}" "${n}" parentBeaconBlockRoot)"
    got_h="$(storage_at "${url}" "${RECORDER}" $((2 * n)) "${n}")"
    got_hash="$(storage_at "${url}" "${RECORDER}" $((2 * n + 1)) "${n}")"
    if [[ "${got_h}" == "${want_h_word}" && -n "${root}" && "${got_hash}" == "${root}" && "${root}" == "${zc_hash}" ]]; then
      pass "(b) node ${label} block ${n}: stored E_N=${want_h}, hash == parentBeaconBlockRoot == zebrad getblockhash(${want_h}) (${root})"
    else
      fail "(b) node ${label} block ${n}: stored E_N=${got_h:-<none>} (want ${want_h}), stored hash=${got_hash:-<none>}, parentBeaconBlockRoot=${root:-<none>}, zebrad=${zc_hash}"
    fi
  done
done
CONTROL=$((FIRST_TX_BLOCK - 1))
ZERO_WORD="0x$(printf '%064x' 0)"
for node in "${NODES[@]}"; do
  read -r label url <<<"${node}"
  c="$(storage_at "${url}" "${RECORDER}" $((2 * CONTROL)) "${LAST_TX_BLOCK}")"
  if [[ "${c}" == "${ZERO_WORD}" ]]; then
    pass "(b) node ${label} control: no anchor stored for block ${CONTROL} (no recorder call there)"
  else
    fail "(b) node ${label} control: slot for block ${CONTROL} = ${c:-<none>}, want 0"
  fi
done

# ============================================================
# (c) eth_call anchor() at historical blocks
# ============================================================
echo ""
echo "=== (c) eth_call anchor() at each historical recorder block ==="
PREV_ANSWER=""
for n in "${TX_BLOCKS[@]}"; do
  want_h=$((n + EPOCH_BASE - 1))
  for node in "${NODES[@]}"; do
    read -r label url <<<"${node}"
    root="$(block_field "${url}" "${n}" parentBeaconBlockRoot)"
    want="$(printf '%064x' "${want_h}")${root#0x}"
    got="$(anchor_call "${url}" "${n}")"
    if [[ -n "${root}" && "${got}" == "${want}" ]]; then
      pass "(c) node ${label} eth_call@${n}: anchor() == (${want_h}, ${root})"
    else
      fail "(c) node ${label} eth_call@${n}: got ${got:-<empty>}, want (${want_h}, ${root:-<no root>})"
    fi
  done
  if [[ -n "${PREV_ANSWER}" && "${PREV_ANSWER}" == "${got}" ]]; then
    fail "(c) eth_call@${n} returned the same bytes as the previous recorder block (cached across blocks?)"
  fi
  PREV_ANSWER="${got}"
done

# ============================================================
# (d) convergence at a common tip; reputation
# ============================================================
echo ""
echo "=== (d) A, B, J end on the same block hash and stateRoot ==="
sleep 4
COMMON="$(height_of "${ENGINE_RPC_A}")"
wait_for_block_number "${ENGINE_RPC_B}" "${COMMON}" 30 || true
wait_for_block_number "${ENGINE_RPC_J}" "${COMMON}" 30 || true
vals=""
for node in "${NODES[@]}"; do
  read -r label url <<<"${node}"
  vals+="${label}:$(block_field "${url}" "${COMMON}" hash)/$(block_field "${url}" "${COMMON}" stateRoot) "
done
uniq_vals="$(tr ' ' '\n' <<<"${vals}" | sed '/^$/d' | cut -d: -f2 | sort -u)"
if [[ "$(wc -l <<<"${uniq_vals}" | tr -d ' ')" -eq 1 && "${uniq_vals}" =~ ^0x[0-9a-f]{64}/0x[0-9a-f]{64}$ ]]; then
  pass "(d) common tip ${COMMON}: hash/stateRoot identical on A, B, J (${uniq_vals%%/*} / ${uniq_vals##*/})"
else
  fail "(d) common tip ${COMMON}: ${vals}"
fi
for n in a b j; do
  log="${WORK_DIR}/node-${n}.log"
  hits="$(count_in "${log}" 'reputation hit')"
  if [[ "${hits}" -gt 0 ]]; then
    fail "(d) node $(tr a-z A-Z <<<"${n}") logged ${hits} reputation hit(s) / INVALID peer block(s)"
  else
    pass "(d) node $(tr a-z A-Z <<<"${n}"): no reputation hits"
  fi
done

# ============================================================
# Diagnostics (not asserted)
# ============================================================
echo ""
echo "=== diagnostics ==="
for n in a b j; do
  log="${WORK_DIR}/node-${n}.log"
  echo "  node $(tr a-z A-Z <<<"${n}"): refused precompile calls ('sova zcash precompile'): $(count_in "${log}" 'sova zcash precompile');" \
    "'engine submit failed': $(count_in "${log}" 'engine submit failed'); 'block held': $(count_in "${log}" 'sova/1: block held')"
done
echo "  node A: 'sova miner: build skipped': $(count_in "${WORK_DIR}/node-a.log" 'sova miner: build skipped'); 'sova epoch retrigger': $(count_in "${WORK_DIR}/node-a.log" 'sova epoch retrigger')"
echo "  node A: eth_call anchor() at \"pending\": $(anchor_call "${ENGINE_RPC_A}" pending)"

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "ANCHOR PRECOMPILE SCENARIO PASSED (${#TX_BLOCKS[@]} recorder blocks ${FIRST_TX_BLOCK}..${LAST_TX_BLOCK}, B=${EPOCH_BASE}, join gap ${TIP_AT_JOIN}; all assertions)"
else
  echo "ANCHOR PRECOMPILE SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
