#!/usr/bin/env bash
# Rebuild Sova's full history from a block archive alone (NEAR DA, 2026-10-03):
# no Sova peers, every block verified by the rebuilding node's own consensus
# against its own zebrad.
#
# The claim this backs: "the history doesn't depend on Sova Labs' servers".
# Anyone holding the archive (the NEAR batches; here the same batch format v1
# written to files by `sova-rebuild export`) and a zebrad can rebuild the
# chain, and the node they rebuild into decides every block itself: SIP-6
# seal, C5 settlement re-derived from its zebrad, SIP-4 anchor, execution with
# SIP-4/SIP-7 precompile answers from its own Zcash index, state root.
# `sova-rebuild` only delivers (engine_newPayloadV4 to the node's authrpc, the
# relay's own wire shape and JWT client) and waits for the node's arbiter to
# adopt each block.
#
#   node A -- mine mode (the only producer), SIP-6 + SIP-7, sealing keystore
#             from `sova-miner init`, persistent datadir, sova/1 with no
#             peers, SOVA_RPC_DEBUG=1 when the binary has it.
#   node B -- follow-only, SIP-6 + SIP-7, C5-enforcing, FRESH persistent
#             datadir, the SAME zebrad, relay transport with NO peers
#             (no SOVA_PEERS, no SOVA_P2P_PEERS, no bootnodes, no discovery):
#             blocks reach it only through its authrpc, from sova-rebuild.
#
# Flow:
#   0. Setup: zebrad, miner identity funded (101 blocks), epoch base
#      B = tip + 1, start A, first epoch.
#   1. Chain on A: deploy a recorder contract (calldata = a full 0x5A00
#      precompile call; it stores keccak(returndata) at slot number(), so the
#      state root depends on what the node's Zcash index answered);
#      real SIP-1 burns (sova-miner) every epoch while transactions are
#      mined -- under SIP-6 a burn-less epoch is a NULL block, which carries
#      no transactions -- so every tx lands in a sealed, minting block, plus
#      REBUILD_BURNS mint-only burn epochs; REBUILD_CALLS recorder calls rotating over SIP-4 (anchor, blockAt,
#      txInfo, burnInfo) and SIP-7 (poolTotals, blockStats); null epochs;
#      a LIVE Zcash reorg of depth REBUILD_REORG_DEPTH under a null tip (A
#      re-seals on the new branch); more calls; grow to REBUILD_EPOCHS.
#   2. Export A's blocks 1..N to batch files (`sova-rebuild export`). With
#      debug_getRawBlock available, also export through the eth_*
#      reconstruction and require byte-identical files.
#   3. Archive checks (`--verify-only`, no node): the archive passes (chain
#      id, contiguity, hashes, parent links from A's genesis, body roots,
#      SIP-6 seals); a copy with one middle batch removed is refused (gap);
#      a copy with one byte flipped in a recorder transaction is refused
#      (transactions root).
#   4. Start B fresh, reading zebrad through a proxy that adds
#      REBUILD_B_ZEBRAD_DELAY_MS per request so its Zcash scan lags the
#      rebuild and blocks get HELD. Rebuild B from the TAMPERED archive: B itself must
#      reject the tampered block at its height (sova-rebuild exit 2, the
#      node's reason), having imported everything below it.
#   5. Rebuild the same B from the good archive (resumes above what it
#      has). PASS iff it ends at A's head (number, hash, state root), the
#      tool's own end check and --expect/--expect-rpc agree, B equals A at
#      every height (hash and state root), the recorder's slots match, and
#      a sample of SIP-4/SIP-7 precompile answers at several blocks is
#      identical on A and B. B has zero peers throughout.
#   6. Resume with every block present: run the tool again on the good
#      archive. PASS iff it imports nothing, reports all N present after a
#      handful of lookups (first, every 10,000th, last: the archive's hash
#      links cover the rest), and opens at most a few TCP connections to
#      B's authrpc (counted with netstat/ss, TIME_WAIT included). Before
#      keep-alive and the cheap prefix check it opened one per present
#      block, which ran macOS out of ephemeral ports at ~60k blocks.
#
# Env: REBUILD_EPOCHS (300), REBUILD_CHUNK (50), REBUILD_BURNS (2),
# REBUILD_CALLS (12; half before the reorg, half after),
# REBUILD_REORG_DEPTH (2), REBUILD_BATCH (40 blocks per batch file),
# REBUILD_HOLD_TIMEOUT_S (600), REBUILD_B_ZEBRAD_DELAY_MS (25),
# REBUILD_B_PROXY_PORT (18459), SOVA_REBUILD_BIN (default
# $CARGO_TARGET_DIR or ./target debug sova-rebuild), plus p2p-common.sh's
# SOVA_P2P_SIM_*, SOVA_BIN, SOVA_MINER_BIN, SOVA_P2P_SIM_KEEP_LOGS.
#
# Isolation: zebrad on :18456 (compose project sova-rebuild-sim, container
# sova-zebrad-rebuild), A on 11845/11851/18457, B on 11855/11861/18458, B's
# zebrad proxy on :18459. All
# overridable.

SCENARIO="rebuild-from-archive"
WORK_PREFIX="sova-rebuild-archive"
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18456}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-rebuild}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-rebuild-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=11845 11851 18457}"
: "${SOVA_P2P_SIM_B_PORTS:=11855 11861 18458}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"

EPOCHS="${REBUILD_EPOCHS:-300}"
CHUNK="${REBUILD_CHUNK:-50}"
BURNS="${REBUILD_BURNS:-2}"
CALLS="${REBUILD_CALLS:-12}"
REORG_DEPTH="${REBUILD_REORG_DEPTH:-2}"
BATCH="${REBUILD_BATCH:-40}"
HOLD_TIMEOUT_S="${REBUILD_HOLD_TIMEOUT_S:-600}"
B_ZEBRAD_DELAY_MS="${REBUILD_B_ZEBRAD_DELAY_MS:-25}"
B_PROXY_PORT="${REBUILD_B_PROXY_PORT:-18459}"
REBUILD_BIN="${SOVA_REBUILD_BIN:-${CARGO_TARGET_DIR:-${ROOT}/target}/debug/sova-rebuild}"
FUND_BLOCKS=101
PER_EPOCH_ZAT=100000
# reth's dev chain: account 0 of the public "test test ... junk" mnemonic,
# prefunded in the dev genesis (bin/sova/src/chain.rs). Local use only.
DEV_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
# Proxy recorder (40-byte runtime). Calldata = a full 0x5A00 call:
#   calldatacopy(0, 0, calldatasize())
#   ok := staticcall(gas(), 0x5A00, 0, calldatasize(), 0, 0)
#   if iszero(ok) { revert(0, 0) }
#   returndatacopy(0, 0, returndatasize())
#   sstore(number(), keccak256(0, returndatasize()))
RECORDER_RUNTIME="36600060003760006000366000615a005afa156023573d600060003e3d6000204355005b600080fd"
# CODECOPY the runtime from offset 0x0b and RETURN it.
RECORDER_INIT="0x60$(printf '%02x' $((${#RECORDER_RUNTIME} / 2)))80600b6000396000f3${RECORDER_RUNTIME}"

A_DATADIR=""
A_LOG=""
B_LOG=""
BURNER_PID=""
PROXY_PID=""
RECORDER=""
RECORDER_BLOCKS=()
T0=${SECONDS}

rebuild_cleanup() {
  local rc=$?
  local pid
  for pid in "${BURNER_PID}" "${PROXY_PID}"; do
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      kill "${pid}" 2>/dev/null || true
      wait "${pid}" 2>/dev/null || true
    fi
  done
  if [[ (${rc} -ne 0 || ${FAILURES} -gt 0) && -n "${WORK_DIR}" ]]; then
    local f
    for f in "${WORK_DIR}"/tool-*.out; do
      [[ -f "${f}" ]] || continue
      echo "--- ${f##*/} (last 30 lines) ---" >&2
      tail -n 30 "${f}" >&2 || true
    done
  fi
  (exit "${rc}")
  cleanup
}
trap rebuild_cleanup EXIT

# --- helpers ----------------------------------------------------------------

now_f() { perl -MTime::HiRes=time -e 'printf("%.3f\n", time)'; }
since() { python3 -c "print(f'{$(now_f) - $1:.1f}')"; }
height_of() { eth_block_number "$1" 2>/dev/null || echo 0; }

zc_hash_at() {
  zc_rpc getblockhash "[$1]" | python3 -c "import sys,json
try:
    print((json.load(sys.stdin).get('result') or '').lower())
except Exception:
    print('')"
}

zc_mempool_size() {
  zc_rpc getrawmempool "[]" | python3 -c "import sys,json
try:
    print(len(json.load(sys.stdin).get('result') or []))
except Exception:
    print(0)"
}

# "hash parentHash extraDataBytes parentBeaconBlockRoot stateRoot" of block $2
# on $1 (lowercase); empty when the node has no block there.
block_info() {
  eth_rpc "$1" eth_getBlockByNumber "[\"$(printf '0x%x' "$2")\", false]" | python3 -c "
import sys, json
try:
    b = json.load(sys.stdin).get('result')
except Exception:
    b = None
if b:
    print(b['hash'].lower(), b['parentHash'].lower(), (len(b['extraData']) - 2) // 2,
          (b.get('parentBeaconBlockRoot') or '-').lower(), b['stateRoot'].lower())
"
}

start_node_a() {
  A_LOG="${WORK_DIR}/node-a.log"
  p2p_env \
    RUST_LOG="info" \
    SOVA_SIP6=1 \
    SOVA_SIP7=1 \
    SOVA_RPC_DEBUG=1 \
    SOVA_DATADIR="${A_DATADIR}" \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
    SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" \
    SOVA_SEALER_KEYSTORE="${MINER_DATA_DIR}/keystore.json" \
    SOVA_EPOCH_BASE="${EPOCH_BASE}" \
    SOVA_HTTP_PORT="${A_HTTP_PORT}" \
    SOVA_AUTH_PORT="${A_AUTH_PORT}" \
    SOVA_P2P_PORT="${A_P2P_PORT}" \
    "${SOVA_BIN}" >"${A_LOG}" 2>&1 &
  A_PID=$!
  wait_for_eth_rpc "${ENGINE_RPC_A}" "${A_PID}" "node A"
}

# B: follow-only on the relay transport with no peers at all. `exec` so $!
# is the node (teardown kills it).
relay_env() {
  exec env -u SOVA_PEERS -u SOVA_P2P_PEERS -u SOVA_GOSSIP -u SOVA_BOOTNODES "$@"
}

start_node_b() {
  B_LOG="${WORK_DIR}/node-b.log"
  relay_env \
    RUST_LOG="info" \
    SOVA_SIP6=1 \
    SOVA_SIP7=1 \
    SOVA_FOLLOW_ONLY=1 \
    SOVA_DATADIR="${WORK_DIR}/datadir-b" \
    SOVA_AUTH_JWT="${WORK_DIR}/jwt-b.hex" \
    SOVA_ZEBRAD_RPC="http://127.0.0.1:${B_PROXY_PORT}" \
    SOVA_EPOCH_BASE="${EPOCH_BASE}" \
    SOVA_HTTP_PORT="${B_HTTP_PORT}" \
    SOVA_AUTH_PORT="${B_AUTH_PORT}" \
    SOVA_P2P_PORT="${B_P2P_PORT}" \
    "${SOVA_BIN}" >"${B_LOG}" 2>&1 &
  B_PID=$!
  wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B"
}

# Wait until A's head is the Zcash tip's Sova height; rc 1 on timeout.
wait_a_at_tip() { # <timeout_s>
  local deadline=$((SECONDS + $1)) want h
  while :; do
    want=$(($(zc_tip_height) - EPOCH_BASE + 1))
    h="$(height_of "${ENGINE_RPC_A}")"
    [[ "${h}" -ge "${want}" ]] && return 0
    [[ ${SECONDS} -ge ${deadline} ]] && {
      echo "A=${h} want=${want}"
      return 1
    }
    sleep 0.3
  done
}

# Mine one Zcash block and wait for A to seal it.
step() {
  zc_rpc generate "[1]" >/dev/null
  wait_a_at_tip 90 >/dev/null
}

burner_running() { [[ -n "${BURNER_PID}" ]] && kill -0 "${BURNER_PID}" 2>/dev/null; }

stop_burner() {
  if burner_running; then
    kill "${BURNER_PID}" 2>/dev/null || true
    wait "${BURNER_PID}" 2>/dev/null || true
  fi
  BURNER_PID=""
}

receipt_of() {
  eth_rpc "$1" eth_getTransactionReceipt "[\"$2\"]" | python3 -c "
import sys, json
try:
    r = json.load(sys.stdin).get('result')
except Exception:
    r = None
if r:
    print(int(r['blockNumber'], 16), int(r['status'], 16), (r.get('contractAddress') or '-').lower())
"
}

# Sign with the dev key, submit to A; prints the tx hash.
send_tx() { # <gas> <to|--create> <data...>
  cast send --async --rpc-url "${ENGINE_RPC_A}" --private-key "${DEV_KEY}" \
    --gas-limit "$1" "${@:2}" 2>&1 | grep -oE '0x[0-9a-fA-F]{64}' | tail -1
}

# Mine one Zcash block, after giving the burner (if running) a moment to put
# its burn for that epoch in zebrad's mempool.
burn_step() {
  local deadline=$((SECONDS + 8))
  while [[ ${SECONDS} -lt ${deadline} && "$(zc_mempool_size)" -eq 0 ]] && burner_running; do sleep 0.3; done
  step
}

# Start the burner (one SIP-1 burn per epoch, crediting A) for up to $1 epochs.
start_burner() { # <max_epochs>
  "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
    --budget-zat $((($1 + 2) * (PER_EPOCH_ZAT + 100000))) --per-epoch-zat "${PER_EPOCH_ZAT}" \
    --rpc "${ZEBRAD_RPC}" --max-epochs "$1" --poll-interval-ms 500 >>"${WORK_DIR}/burner.log" 2>&1 &
  BURNER_PID=$!
}

# Stop the burner and mine until zebrad's mempool stays empty.
end_burns() {
  stop_burner
  local _i
  for _i in 1 2 3 4 5; do
    [[ "$(zc_mempool_size)" -eq 0 ]] && break
    step
  done
}

# Send a tx, then mine burn epochs until it has a receipt. Under SIP-6 a
# burn-less epoch's block is a NULL block (no transactions), so the burner
# must be running. Prints "block status contractAddress".
mine_tx() { # <label> <send_tx args...>
  local label="$1" tx r i
  shift
  tx="$(send_tx "$@")"
  if [[ -z "${tx}" ]]; then
    fail "${label}: transaction not accepted by node A"
    return 1
  fi
  for i in 1 2 3 4 5 6; do
    sleep 0.3
    burn_step
    r="$(receipt_of "${ENGINE_RPC_A}" "${tx}")"
    if [[ -n "${r}" ]]; then
      echo "${r}"
      return 0
    fi
  done
  fail "${label}: ${tx} not mined after 6 epochs (burner running: $(burner_running && echo yes || echo no))"
  return 1
}

word() { printf '%064x' "$1"; }

# One recorder call; rotates over the SIP-4 and SIP-7 queries.
record_call() { # <i>
  local i="$1" zt data label r blk st
  zt="$(zc_tip_height)"
  case $((i % 6)) in
    0) data="0xd3fb73b4"; label="anchor()" ;;
    1) data="0x6f8ea15d$(word $((zt - 2)))"; label="blockAt($((zt - 2)))" ;;
    2) data="0x0ac6923d${BURN_TXID}"; label="txInfo(burn)" ;;
    3) data="0x7ce16a38${BURN_TXID}"; label="burnInfo(burn)" ;;
    4) data="0x1c476c7e$(word "${zt}")"; label="poolTotals(${zt})" ;;
    5) data="0x84df4c97$(word $((zt - 1)))"; label="blockStats($((zt - 1)))" ;;
  esac
  r="$(mine_tx "recorder call ${i}" 300000 "${RECORDER}" "${data}")" || return 1
  read -r blk st _ <<<"${r}"
  echo "  recorder call ${i}: ${label} -> block ${blk} status ${st}"
  if [[ "${st}" != "1" ]]; then
    fail "recorder call ${i} (${label}) reverted"
    return 1
  fi
  RECORDER_BLOCKS+=("${blk}")
}

# Grow A until the Zcash tip's Sova height reaches $1, in CHUNK steps.
grow_to() { # <sova_height>
  local have n out
  while :; do
    have=$(($(zc_tip_height) - EPOCH_BASE + 1))
    [[ "${have}" -ge "$1" ]] && break
    n=$(($1 - have))
    [[ "${n}" -gt "${CHUNK}" ]] && n="${CHUNK}"
    zc_rpc generate "[${n}]" >/dev/null
    if ! out="$(wait_a_at_tip 600)"; then
      fail "grow: A did not keep up (${out})"
      return 1
    fi
  done
}

# SIP-4/SIP-7 answers on node $1 at Sova block $2 for each Zcash height in
# $3.. : one line per (height, query), "<h> <query> <result or ERR(...)>".
precompile_answers() { # <url> <sova_block> <zcash heights...>
  python3 - "$1" "$2" "${ZEBRAD_RPC}" "${BURN_TXID}" "${@:3}" <<'PY'
import json, sys, urllib.request
url, block, zurl, burn = sys.argv[1], int(sys.argv[2]), sys.argv[3] + "/", sys.argv[4]
heights = [int(h) for h in sys.argv[5:]]
PRE = "0x0000000000000000000000000000000000005a00"
def rpc(u, m, p):
    req = urllib.request.Request(u, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            d = json.load(r)
            return d.get("result"), d.get("error")
    except Exception as e:
        return None, str(e)
def call(data):
    r, err = rpc(url, "eth_call", [{"to": PRE, "data": data, "gas": "0x100000"}, hex(block)])
    return r if r is not None else f"ERR({str((err or {}).get('message') if isinstance(err, dict) else err)[:60]})"
w = lambda v: format(v, "064x")
print(f"* anchor() {call('0xd3fb73b4')}")
print(f"* txInfo(burn) {call('0x0ac6923d' + burn)}")
print(f"* burnInfo(burn) {call('0x7ce16a38' + burn)}")
for h in heights:
    zh, _ = rpc(zurl, "getblockhash", [h])
    blk, _ = rpc(zurl, "getblock", [zh, 1]) if zh else (None, None)
    print(f"{h} blockAt {call('0x6f8ea15d' + w(h))}")
    if blk:
        print(f"{h} txInfo(coinbase) {call('0x0ac6923d' + blk['tx'][0])}")
    print(f"{h} poolTotals {call('0x1c476c7e' + w(h))}")
    print(f"{h} blockStats {call('0x84df4c97' + w(h))}")
PY
}

# "hash stateRoot" at every height 1..$2 on $1, one per line.
chain_dump() { # <url> <head>
  python3 - "$1" "$2" <<'PY'
import json, sys, urllib.request
url, head = sys.argv[1], int(sys.argv[2])
def rpc(m, p):
    req = urllib.request.Request(url, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.load(r).get("result")
for n in range(0, head + 1):
    b = rpc("eth_getBlockByNumber", [hex(n), False])
    print(n, (b or {}).get("hash", "-"), (b or {}).get("stateRoot", "-"))
PY
}

# =============================================================================
# (0) setup
# =============================================================================

for tool in python3 perl cast cmp openssl; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "error: this scenario needs \`${tool}\` on PATH" >&2
    exit 1
  fi
done

preflight
if lsof -nP -iTCP:"${B_PROXY_PORT}" -sTCP:LISTEN >/dev/null 2>&1; then
  echo "error: port ${B_PROXY_PORT} already in use; set REBUILD_B_PROXY_PORT" >&2
  exit 1
fi
if [[ ! -x "${REBUILD_BIN}" ]]; then
  echo "--- building sova-rebuild (debug) ---"
  (cd "${ROOT}" && cargo build -p sova-rebuild --quiet) || exit 1
fi
start_stack

MINER_DATA_DIR="${WORK_DIR}/miner"
A_DATADIR="${WORK_DIR}/datadir-a"
mkdir -p "${MINER_DATA_DIR}" "${A_DATADIR}" "${WORK_DIR}/datadir-b"
openssl rand -hex 32 >"${WORK_DIR}/jwt-b.hex"
"${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest init >"${WORK_DIR}/miner-init.log" 2>&1
TADDR="$(awk '/t-addr to fund/ {print $NF}' "${WORK_DIR}/miner-init.log")"
EVM_ADDR="$(awk '/evm address/ {print $NF}' "${WORK_DIR}/miner-init.log" | head -1)"
if [[ -z "${TADDR}" || -z "${EVM_ADDR}" || ! -f "${MINER_DATA_DIR}/keystore.json" ]]; then
  fail "setup: could not parse the miner identity / find its keystore"
  cat "${WORK_DIR}/miner-init.log" >&2
  exit 1
fi
zc_generate_to_address "${FUND_BLOCKS}" "${TADDR}"
EPOCH_BASE=$(($(zc_tip_height) + 1))
echo "miner ${TADDR} / ${EVM_ADDR}; epoch base B=${EPOCH_BASE}; target ${EPOCHS} epochs"

echo "--- starting node A (mine mode, SIP-6 + SIP-7, sealing keystore, persistent datadir, no peers) ---"
start_node_a || exit 1
echo ""
echo "=== (0) setup ==="
check_p2p_node_log "node A" "${A_LOG}"
if log_has "${A_LOG}" "sip-6: sealing as" 15; then
  pass "setup: node A seals (SIP-6)"
else
  fail "setup: node A's log lacks 'sip-6: sealing as'"
fi
zc_rpc generate "[1]" >/dev/null
if ! wait_a_at_tip 90 >/dev/null; then
  fail "setup: A did not seal the first epoch"
  exit 1
fi
[[ "${FAILURES}" -eq 0 ]] || exit 1

# =============================================================================
# (1) the chain on A
# =============================================================================

echo ""
echo "=== (1) chain on A: recorder, ${BURNS} burns, ${CALLS} precompile calls, live Zcash reorg (depth ${REORG_DEPTH}), ${EPOCHS} epochs ==="
T_CHAIN=$(now_f)
HALF=$((CALLS / 2))
start_burner $((1 + HALF + BURNS + 6))
r="$(mine_tx "deploy recorder" 200000 --create "${RECORDER_INIT}")" || exit 1
read -r DEPLOY_BLOCK DEPLOY_STATUS RECORDER <<<"${r}"
CODE="$(eth_rpc "${ENGINE_RPC_A}" eth_getCode "[\"${RECORDER}\",\"latest\"]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'])")"
if [[ "${DEPLOY_STATUS}" == "1" && "${CODE}" == "0x${RECORDER_RUNTIME}" ]]; then
  pass "(1) recorder deployed at ${RECORDER} in block ${DEPLOY_BLOCK}"
else
  fail "(1) recorder deploy: status ${DEPLOY_STATUS}, code ${CODE:0:20}.."
  exit 1
fi
# The first burn's txid (the calls' txInfo/burnInfo argument).
BURN_TXID="$(python3 - "${ZEBRAD_RPC}" "${EPOCH_BASE}" <<'PY'
import sys, json, urllib.request
url, lo = sys.argv[1], int(sys.argv[2])
def rpc(m, p):
    req = urllib.request.Request(url + "/", data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.load(r).get("result")
for h in range(lo, rpc("getblockcount", []) + 1):
    txs = rpc("getblock", [str(h), 1])["tx"]
    if len(txs) > 1:
        print(txs[1].lower())
        break
PY
)"
if [[ -z "${BURN_TXID}" ]]; then
  fail "(1) no burn on zebrad's chain after the deploy (burner: $(tail -n 3 "${WORK_DIR}/burner.log" | tr '\n' ' '))"
  exit 1
fi
for ((i = 0; i < HALF; i++)); do record_call "${i}" || exit 1; done
# A few burn epochs without transactions (mint-only sealed blocks).
for ((i = 0; i < BURNS; i++)); do burn_step; done
end_burns

# Burns on zebrad's chain since the epoch base, "height txid" per line.
python3 - "${ZEBRAD_RPC}" "${EPOCH_BASE}" >"${WORK_DIR}/burns.txt" <<'PY'
import sys, json, urllib.request
url, lo = sys.argv[1], int(sys.argv[2])
def rpc(m, p):
    req = urllib.request.Request(url + "/", data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.load(r).get("result")
tip = rpc("getblockcount", [])
for h in range(lo, tip + 1):
    blk = rpc("getblock", [str(h), 1])
    for t in blk["tx"][1:]:
        print(h, t.lower())
PY
BURN_COUNT="$(wc -l <"${WORK_DIR}/burns.txt" | tr -d ' ')"
read -r BURN_HEIGHT _ <<<"$(grep " ${BURN_TXID}$" "${WORK_DIR}/burns.txt")"
SEALED=0
for ((n = 1; n <= $(height_of "${ENGINE_RPC_A}"); n++)); do
  read -r _ _ x _ <<<"$(block_info "${ENGINE_RPC_A}" "${n}")"
  [[ "${x}" == "97" ]] && SEALED=$((SEALED + 1))
done
if [[ "${BURN_COUNT}" -ge 1 && "${SEALED}" -ge 1 ]]; then
  pass "(1) ${BURN_COUNT} burn(s) on Zcash (first ${BURN_TXID:0:16}.. at ${BURN_HEIGHT}); ${SEALED} sealed block(s) on A"
else
  fail "(1) burns: ${BURN_COUNT} on Zcash, ${SEALED} sealed blocks on A (burner: $(tail -n 3 "${WORK_DIR}/burner.log" | tr '\n' ' '))"
  exit 1
fi

grow_to $((EPOCHS / 2)) || exit 1

# Live reorg under a null tip: A sees it and re-seals on the new branch.
for _ in 1 2 3; do
  [[ "$(zc_mempool_size)" -eq 0 ]] && break
  step
done
wait_a_at_tip 60 >/dev/null
H="$(height_of "${ENGINE_RPC_A}")"
ZT="$(zc_tip_height)"
FORK_Z=$((ZT - REORG_DEPTH + 1))
declare -a OLD_HASHES
for ((d = 0; d < REORG_DEPTH; d++)); do
  read -r "OLD_HASHES[d]" _ x _ <<<"$(block_info "${ENGINE_RPC_A}" $((H - d)))"
  if [[ "${x}" != "0" ]]; then
    fail "(1) reorg: A's block $((H - d)) is not a null block (extraData ${x} bytes)"
    exit 1
  fi
done
OLD_FORK_HASH="$(zc_hash_at "${FORK_Z}")"
zc_rpc invalidateblock "[\"${OLD_FORK_HASH}\"]" >"${WORK_DIR}/invalidate.json"
if [[ "$(zc_tip_height)" -ne $((FORK_Z - 1)) ]]; then
  fail "(1) reorg: zebrad tip $(zc_tip_height) after invalidateblock(${FORK_Z}), want $((FORK_Z - 1))"
  exit 1
fi
zc_rpc generate "[$((REORG_DEPTH + 1))]" >/dev/null
if ! out="$(wait_a_at_tip 120)"; then
  fail "(1) reorg: A did not follow the replacement branch (${out})"
  exit 1
fi
# Wait for A's re-sealed blocks to be anchored to the new branch.
deadline=$((SECONDS + 60))
RESEALED=0
while [[ ${SECONDS} -lt ${deadline} ]]; do
  ok=1
  for ((d = 0; d < REORG_DEPTH; d++)); do
    n=$((H - d))
    read -r hh _ _ anchor _ <<<"$(block_info "${ENGINE_RPC_A}" "${n}")"
    want="0x$(zc_hash_at $((n + EPOCH_BASE - 1)))"
    if [[ "${anchor}" != "${want}" || "${hh}" == "${OLD_HASHES[d]}" ]]; then ok=0; fi
  done
  if [[ "${ok}" -eq 1 ]]; then
    RESEALED=1
    break
  fi
  sleep 1
done
if [[ "${RESEALED}" -eq 1 ]]; then
  pass "(1) live Zcash reorg at ${FORK_Z} (depth ${REORG_DEPTH}): A re-sealed $((H - REORG_DEPTH + 1))..${H} on the new branch, head $(height_of "${ENGINE_RPC_A}")"
else
  fail "(1) reorg: A's blocks $((H - REORG_DEPTH + 1))..${H} not re-sealed onto the new Zcash branch within 60 s"
  exit 1
fi
REORG_SOVA=$((H - REORG_DEPTH + 1))

start_burner $((CALLS - HALF + 6))
for ((i = HALF; i < CALLS; i++)); do record_call "${i}" || exit 1; done
end_burns
grow_to "${EPOCHS}" || exit 1
wait_a_at_tip 60 >/dev/null
N="$(height_of "${ENGINE_RPC_A}")"
read -r A_HASH _ _ _ A_ROOT <<<"$(block_info "${ENGINE_RPC_A}" "${N}")"
read -r A_GENESIS _ <<<"$(block_info "${ENGINE_RPC_A}" 0)"
CHAIN_ID="$(eth_rpc "${ENGINE_RPC_A}" eth_chainId "[]" | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))")"
pass "(1) A at head N=${N} ${A_HASH:0:18}.. stateRoot ${A_ROOT:0:18}.. after $(since "${T_CHAIN}") s (chain id ${CHAIN_ID}, recorder blocks: ${RECORDER_BLOCKS[*]})"

# =============================================================================
# (2) export
# =============================================================================

echo ""
echo "=== (2) export A's blocks 1..${N} to batch files (${BATCH} per file) ==="
ARCHIVE="${WORK_DIR}/archive"
T_EXPORT=$(now_f)
"${REBUILD_BIN}" export --rpc "${ENGINE_RPC_A}" --out "${ARCHIVE}" --to "${N}" --batch-size "${BATCH}" \
  >"${WORK_DIR}/tool-export.out" 2>&1
RC=$?
EXPORT_LINE="$(grep 'EXPORT OK' "${WORK_DIR}/tool-export.out")"
FILES=("${ARCHIVE}"/*.sovada)
if [[ ${RC} -eq 0 && -n "${EXPORT_LINE}" && ${#FILES[@]} -ge 3 ]]; then
  pass "(2) export: ${EXPORT_LINE#EXPORT OK: } ($(du -sk "${ARCHIVE}" | cut -f1) KiB, wall $(since "${T_EXPORT}") s)"
else
  fail "(2) export failed (rc ${RC}): $(tail -n 3 "${WORK_DIR}/tool-export.out" | tr '\n' ' ')"
  exit 1
fi
if grep -q 'raw from debug_getRawBlock' <<<"${EXPORT_LINE}"; then
  "${REBUILD_BIN}" export --rpc "${ENGINE_RPC_A}" --out "${WORK_DIR}/archive-eth" --to "${N}" \
    --batch-size "${BATCH}" --raw reconstruct >"${WORK_DIR}/tool-export-eth.out" 2>&1
  DIFFS=0
  for f in "${FILES[@]}"; do
    cmp -s "${f}" "${WORK_DIR}/archive-eth/${f##*/}" || DIFFS=$((DIFFS + 1))
  done
  if [[ ${DIFFS} -eq 0 && "$(find "${WORK_DIR}/archive-eth" -name '*.sovada' | wc -l | tr -d ' ')" -eq ${#FILES[@]} ]]; then
    pass "(2) export: debug_getRawBlock and the eth_* reconstruction give byte-identical batches (${#FILES[@]} files)"
  else
    fail "(2) export: ${DIFFS} batch file(s) differ between debug_getRawBlock and the eth_* reconstruction"
  fi
else
  echo "  NOTE: node A does not serve debug_getRawBlock (no SOVA_RPC_DEBUG in this build); exported via eth_* reconstruction"
fi

# =============================================================================
# (3) archive checks without a node
# =============================================================================

echo ""
echo "=== (3) --verify-only: good archive passes; gap and tampered copies are refused ==="
"${REBUILD_BIN}" --verify-only --chain-id "${CHAIN_ID}" --genesis "${A_GENESIS}" --sip6 "${ARCHIVE}" \
  >"${WORK_DIR}/tool-verify.out" 2>&1
RC=$?
if [[ ${RC} -eq 0 ]] && grep -q "VERIFY OK: chain id ${CHAIN_ID}, blocks #1..#${N} " "${WORK_DIR}/tool-verify.out"; then
  pass "(3) verify-only: $(grep 'VERIFY OK' "${WORK_DIR}/tool-verify.out" | cut -c1-200)"
else
  fail "(3) verify-only on the good archive: rc ${RC}: $(tail -n 2 "${WORK_DIR}/tool-verify.out" | tr '\n' ' ')"
fi

# Gap: drop the second batch file.
mkdir -p "${WORK_DIR}/archive-gap"
cp "${FILES[@]}" "${WORK_DIR}/archive-gap/"
rm "${WORK_DIR}/archive-gap/${FILES[1]##*/}"
"${REBUILD_BIN}" --verify-only --chain-id "${CHAIN_ID}" "${WORK_DIR}/archive-gap" >"${WORK_DIR}/tool-verify-gap.out" 2>&1
RC=$?
if [[ ${RC} -eq 3 ]] && grep -q 'gap: expected height' "${WORK_DIR}/tool-verify-gap.out"; then
  pass "(3) gap refused: $(grep 'VERIFY FAILED' "${WORK_DIR}/tool-verify-gap.out" | sed 's#.*/##' | cut -c1-160)"
else
  fail "(3) a batch set with a gap was not refused (rc ${RC}): $(tail -n 2 "${WORK_DIR}/tool-verify-gap.out" | tr '\n' ' ')"
fi

# Tamper: flip the last byte of the signature's r in the first transaction of
# a recorder-call block after the reorg (so the tampered run below rebuilds
# half the chain first, racing B's Zcash scan). The block still decodes and its header (and
# so its recorded hash) is untouched; only the body changed.
TAMPER_HEIGHT="${RECORDER_BLOCKS[$((CALLS - 4))]}"
TX_R="$(eth_rpc "${ENGINE_RPC_A}" eth_getTransactionByBlockNumberAndIndex "[\"$(printf '0x%x' "${TAMPER_HEIGHT}")\",\"0x0\"]" \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['r'])")"
read -r TAMPER_HASH _ <<<"$(block_info "${ENGINE_RPC_A}" "${TAMPER_HEIGHT}")"
mkdir -p "${WORK_DIR}/archive-tampered"
cp "${FILES[@]}" "${WORK_DIR}/archive-tampered/"
TAMPER_OUT="$(python3 - "${WORK_DIR}/archive-tampered" "${TAMPER_HEIGHT}" "${TX_R}" <<'PY'
import os, struct, sys
d, target, r = sys.argv[1], int(sys.argv[2]), sys.argv[3][2:]
r = bytes.fromhex(r.rjust(len(r) + len(r) % 2, "0"))
for name in sorted(os.listdir(d)):
    p = os.path.join(d, name)
    buf = bytearray(open(p, "rb").read())
    _, _, first, count = struct.unpack_from("<8sQQI", buf, 0)
    if not (first <= target < first + count):
        continue
    off = 28
    for _ in range(count):
        h, = struct.unpack_from("<Q", buf, off)
        ln, = struct.unpack_from("<I", buf, off + 40)
        raw_off = off + 44
        if h == target:
            raw = bytes(buf[raw_off:raw_off + ln])
            i = raw.find(r)
            if i < 0 or raw.find(r, i + 1) >= 0:
                print(f"ERR r not found exactly once in block {h}"); sys.exit(1)
            pos = raw_off + i + len(r) - 1
            buf[pos] ^= 0x01
            open(p, "wb").write(buf)
            print(f"flipped byte {i + len(r) - 1} of block {h}'s {ln}-byte RLP in {name}")
            sys.exit(0)
        off = raw_off + ln
print("ERR block not found"); sys.exit(1)
PY
)" || {
  fail "(3) could not tamper with block ${TAMPER_HEIGHT}: ${TAMPER_OUT}"
  exit 1
}
echo "  tamper: ${TAMPER_OUT}"
"${REBUILD_BIN}" --verify-only --chain-id "${CHAIN_ID}" "${WORK_DIR}/archive-tampered" >"${WORK_DIR}/tool-verify-tampered.out" 2>&1
RC=$?
if [[ ${RC} -eq 3 ]] && grep -q "height ${TAMPER_HEIGHT}: transactions root" "${WORK_DIR}/tool-verify-tampered.out"; then
  pass "(3) tampered copy refused by --verify-only at ${TAMPER_HEIGHT} (transactions root)"
else
  fail "(3) tampered copy not refused by --verify-only at ${TAMPER_HEIGHT} (rc ${RC}): $(tail -n 2 "${WORK_DIR}/tool-verify-tampered.out" | tr '\n' ' ')"
fi

# =============================================================================
# (4) B rejects the tampered block
# =============================================================================

echo ""
echo "=== (4) start B fresh (no peers); rebuild it from the TAMPERED archive ==="
# B reads the same zebrad through a proxy adding ${B_ZEBRAD_DELAY_MS} ms per
# request, so its Zcash scan is slower than the rebuild and the node HOLDS
# blocks ahead of it (SIP-4 "hold, don't accept"): the tool must wait those
# out, not count them as rejections.
python3 - "${B_PROXY_PORT}" "${ZEBRAD_RPC}" "${B_ZEBRAD_DELAY_MS}" >"${WORK_DIR}/proxy.log" 2>&1 <<'PY' &
import sys, time, urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
port, up, delay = int(sys.argv[1]), sys.argv[2] + "/", float(sys.argv[3]) / 1000
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if delay:
            time.sleep(delay)
        req = urllib.request.Request(up, data=body, headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=60) as r:
            out = r.read()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)
ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
PY
PROXY_PID=$!
sleep 1
start_node_b || exit 1
B_CLEAN="$(strip_ansi "${B_LOG}")"
if grep -q "follow-only mode: no local mining" <<<"${B_CLEAN}" && grep -q "expectations: enforcing settlements" <<<"${B_CLEAN}" \
  && ! grep -q "relay: pushing sealed blocks" <<<"${B_CLEAN}" && ! grep -q "sova/1 gossip enabled" <<<"${B_CLEAN}"; then
  pass "setup: B is follow-only, C5-enforcing against zebrad, no relay and no sova/1"
else
  fail "setup: B's startup log lacks follow-only/C5 lines or shows a peer transport"
fi
B_HEAD0="$(height_of "${ENGINE_RPC_B}")"
[[ "${B_HEAD0}" -eq 0 ]] || fail "setup: B is not fresh (head ${B_HEAD0})"
REBUILD_ARGS=(--authrpc "http://127.0.0.1:${B_AUTH_PORT}" --jwt "${WORK_DIR}/jwt-b.hex" --sip6
  --hold-timeout "${HOLD_TIMEOUT_S}")
T_TAMPER=$(now_f)
"${REBUILD_BIN}" "${REBUILD_ARGS[@]}" "${WORK_DIR}/archive-tampered" >"${WORK_DIR}/tool-rebuild-tampered.out" 2>&1
RC=$?
TAMPER_S="$(since "${T_TAMPER}")"
REJECT_LINE="$(grep 'REBUILD FAILED' "${WORK_DIR}/tool-rebuild-tampered.out")"
B_AFTER_TAMPER="$(height_of "${ENGINE_RPC_B}")"
echo "  ${REJECT_LINE}"
if [[ ${RC} -eq 2 ]] && grep -q "rejected block #${TAMPER_HEIGHT} ${TAMPER_HASH}" <<<"${REJECT_LINE}"; then
  pass "(4) B rejected the tampered block #${TAMPER_HEIGHT} itself (exit 2) with the node's reason: ${REJECT_LINE##*: }"
else
  fail "(4) expected B to reject #${TAMPER_HEIGHT} (exit 2); got rc ${RC}: ${REJECT_LINE:-$(tail -n 2 "${WORK_DIR}/tool-rebuild-tampered.out" | tr '\n' ' ')}"
fi
if [[ "${B_AFTER_TAMPER}" -eq $((TAMPER_HEIGHT - 1)) ]]; then
  pass "(4) B imported 1..$((TAMPER_HEIGHT - 1)) before the tampered block and stopped there (${TAMPER_S} s)"
else
  fail "(4) B's head is ${B_AFTER_TAMPER} after the tampered run, want $((TAMPER_HEIGHT - 1))"
fi

# =============================================================================
# (5) rebuild B from the good archive
# =============================================================================

echo ""
echo "=== (5) rebuild B from the good archive (resumes at $((B_AFTER_TAMPER + 1))) ==="
T_REBUILD=$(now_f)
"${REBUILD_BIN}" "${REBUILD_ARGS[@]}" --expect "${N}:${A_HASH}:${A_ROOT}" --expect-rpc "${ENGINE_RPC_A}" \
  "${ARCHIVE}" >"${WORK_DIR}/tool-rebuild.out" 2>&1
RC=$?
REBUILD_S="$(since "${T_REBUILD}")"
OK_LINE="$(grep 'REBUILD OK' "${WORK_DIR}/tool-rebuild.out")"
grep -E '^rebuild: #' "${WORK_DIR}/tool-rebuild.out" | tail -n 3 | sed 's/^/  /'
if [[ ${RC} -eq 0 && -n "${OK_LINE}" ]]; then
  pass "(5) ${OK_LINE} (wall ${REBUILD_S} s)"
  grep '^  matches' "${WORK_DIR}/tool-rebuild.out" | sed 's/^/  /'
else
  fail "(5) rebuild failed (rc ${RC}): $(grep -E 'REBUILD FAILED|error' "${WORK_DIR}/tool-rebuild.out" | tail -n 2 | tr '\n' ' ')"
fi

read -r B_HASH _ _ _ B_ROOT <<<"$(block_info "${ENGINE_RPC_B}" "${N}")"
B_HEAD="$(height_of "${ENGINE_RPC_B}")"
if [[ "${B_HEAD}" -eq "${N}" && "${B_HASH}" == "${A_HASH}" && "${B_ROOT}" == "${A_ROOT}" ]]; then
  pass "(5) B's head = A's head: #${N} ${A_HASH:0:18}.. stateRoot ${A_ROOT:0:18}.. (read from both nodes' RPC)"
else
  fail "(5) B head #${B_HEAD} ${B_HASH:-?} root ${B_ROOT:-?} vs A #${N} ${A_HASH} root ${A_ROOT}"
fi

chain_dump "${ENGINE_RPC_A}" "${N}" >"${WORK_DIR}/chain-a.txt"
chain_dump "${ENGINE_RPC_B}" "${N}" >"${WORK_DIR}/chain-b.txt"
if cmp -s "${WORK_DIR}/chain-a.txt" "${WORK_DIR}/chain-b.txt"; then
  pass "(5) A and B agree on hash and stateRoot at every height 0..${N}"
else
  fail "(5) A and B differ: $(diff "${WORK_DIR}/chain-a.txt" "${WORK_DIR}/chain-b.txt" | head -n 4 | tr '\n' ' ')"
fi

# Recorder slots: keccak of the precompile's answer at each call's block.
SLOT_DIFF=0
SLOT_ZERO=0
for blk in "${RECORDER_BLOCKS[@]}"; do
  sa="$(eth_rpc "${ENGINE_RPC_A}" eth_getStorageAt "[\"${RECORDER}\",\"$(printf '0x%x' "${blk}")\",\"latest\"]" | python3 -c "import sys,json;print(json.load(sys.stdin).get('result'))")"
  sb="$(eth_rpc "${ENGINE_RPC_B}" eth_getStorageAt "[\"${RECORDER}\",\"$(printf '0x%x' "${blk}")\",\"latest\"]" | python3 -c "import sys,json;print(json.load(sys.stdin).get('result'))")"
  [[ "${sa}" != "${sb}" ]] && SLOT_DIFF=$((SLOT_DIFF + 1))
  [[ "${sa}" =~ ^0x0*$ ]] && SLOT_ZERO=$((SLOT_ZERO + 1))
done
if [[ ${SLOT_DIFF} -eq 0 && ${SLOT_ZERO} -eq 0 ]]; then
  pass "(5) recorder: ${#RECORDER_BLOCKS[@]} recorded precompile answers (keccak of returndata) identical on A and B, none empty"
else
  fail "(5) recorder slots: ${SLOT_DIFF} differ between A and B, ${SLOT_ZERO} empty"
fi

# Precompile sample at several blocks: the recorder's first call, the block
# right after the reorg, the middle, and the head.
ZTIP="$(zc_tip_height)"
SAMPLE_BLOCKS=("${RECORDER_BLOCKS[0]}" "$((REORG_SOVA + REORG_DEPTH))" "$((N / 2))" "${N}")
SAMPLE_Z=("${EPOCH_BASE}" "${BURN_HEIGHT}" "${FORK_Z}" "$((FORK_Z + 1))" "$((ZTIP - 1))" "${ZTIP}")
: >"${WORK_DIR}/answers-a.txt"
: >"${WORK_DIR}/answers-b.txt"
for s in "${SAMPLE_BLOCKS[@]}"; do
  zs=()
  for z in "${SAMPLE_Z[@]}"; do [[ ${z} -le $((s + EPOCH_BASE - 1)) ]] && zs+=("${z}"); done
  precompile_answers "${ENGINE_RPC_A}" "${s}" "${zs[@]}" | sed "s/^/${s} /" >>"${WORK_DIR}/answers-a.txt"
  precompile_answers "${ENGINE_RPC_B}" "${s}" "${zs[@]}" | sed "s/^/${s} /" >>"${WORK_DIR}/answers-b.txt"
done
ANSWERS="$(wc -l <"${WORK_DIR}/answers-a.txt" | tr -d ' ')"
ERRS="$(grep -c 'ERR(' "${WORK_DIR}/answers-a.txt" || true)"
if cmp -s "${WORK_DIR}/answers-a.txt" "${WORK_DIR}/answers-b.txt" && [[ ${ANSWERS} -ge 20 && ${ERRS} -eq 0 ]]; then
  pass "(5) ${ANSWERS} SIP-4/SIP-7 precompile answers at Sova blocks ${SAMPLE_BLOCKS[*]} identical on A and B (anchor, blockAt, txInfo, burnInfo, poolTotals, blockStats)"
else
  fail "(5) precompile answers: ${ANSWERS} lines, ${ERRS} errors on A; A vs B: $(diff "${WORK_DIR}/answers-a.txt" "${WORK_DIR}/answers-b.txt" | head -n 4 | tr '\n' ' ')"
fi

PEERS_B="$(eth_rpc "${ENGINE_RPC_B}" net_peerCount "[]" | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))")"
if [[ "${PEERS_B}" -eq 0 ]]; then
  pass "(5) B had no peers (net_peerCount 0): every block came from the archive through its authrpc"
else
  fail "(5) B reports ${PEERS_B} peer(s)"
fi
HOLDS_LINE="$(cat "${WORK_DIR}/tool-rebuild-tampered.out" "${WORK_DIR}/tool-rebuild.out" | grep -c 'held by the node' || true)"
if [[ "${HOLDS_LINE}" -ge 1 ]]; then
  pass "(4/5) the node held ${HOLDS_LINE} block(s) ahead of its own Zcash scan (e.g. '$(grep -h -m1 'held by the node' "${WORK_DIR}"/tool-rebuild*.out | sed 's/^rebuild: //' | cut -c1-140)'); the tool waited and resent, none counted as a rejection"
else
  fail "(4/5) no hold was observed: B's scan never fell behind (raise REBUILD_B_ZEBRAD_DELAY_MS)"
fi

# =============================================================================
# (6) resume with every block present: cheap prefix check, one connection
# =============================================================================

# Sockets touching B's authrpc port, in any state. A connection the tool
# closes stays in TIME_WAIT for 30-60 s, so a count taken right after a run
# includes every connection the run opened.
auth_sockets() {
  local n
  if [[ "$(uname -s)" == Darwin ]]; then
    n="$(netstat -an -p tcp 2>/dev/null | grep -cE "127\.0\.0\.1\.${B_AUTH_PORT}( |$)" || true)"
  else
    n="$(ss -tan 2>/dev/null | grep -cE "127\.0\.0\.1:${B_AUTH_PORT}( |$)" || true)"
  fi
  echo "${n:-0}"
}

echo ""
echo "=== (6) resume: rebuild B again from the good archive (all ${N} blocks present) ==="
AUTH_AFTER_5="$(auth_sockets)"
echo "  sockets on B's authrpc port right after (4)+(5): ${AUTH_AFTER_5}"
# Let earlier runs' TIME_WAIT sockets expire, so the count is this run's.
for _ in $(seq 1 90); do
  [[ "$(auth_sockets)" -le 2 ]] && break
  sleep 1
done
AUTH_BEFORE="$(auth_sockets)"
T_RESUME=$(now_f)
"${REBUILD_BIN}" "${REBUILD_ARGS[@]}" --expect "${N}:${A_HASH}:${A_ROOT}" \
  "${ARCHIVE}" >"${WORK_DIR}/tool-resume.out" 2>&1
RC=$?
RESUME_S="$(since "${T_RESUME}")"
AUTH_AFTER="$(auth_sockets)"
RESUME_CONNS=$((AUTH_AFTER - AUTH_BEFORE))
RESUME_OK="$(grep 'REBUILD OK' "${WORK_DIR}/tool-resume.out")"
PREFIX_LINE="$(grep 'already has' "${WORK_DIR}/tool-resume.out")"
LOOKUPS="$(sed -nE 's/.*: ([0-9]+) looked up.*/\1/p' <<<"${PREFIX_LINE}")"
echo "  ${PREFIX_LINE#rebuild: }"
if [[ ${RC} -eq 0 ]] && grep -q " 0 block(s) imported" <<<"${RESUME_OK}" && grep -q ", ${N} already present" <<<"${RESUME_OK}"; then
  pass "(6) resume: nothing imported, all ${N} blocks already present (wall ${RESUME_S} s)"
else
  fail "(6) resume failed (rc ${RC}): ${RESUME_OK:-$(tail -n 2 "${WORK_DIR}/tool-resume.out" | tr '\n' ' ')}"
fi
if [[ -n "${LOOKUPS}" && "${LOOKUPS}" -le $((2 + N / 10000)) ]]; then
  pass "(6) resume: ${LOOKUPS} block lookup(s) for ${N} present blocks (first, every 10,000th, last)"
else
  fail "(6) resume: expected at most $((2 + N / 10000)) lookups, got '${LOOKUPS:-none}' (${PREFIX_LINE:-no prefix line})"
fi
if [[ ${RESUME_CONNS} -le 4 ]]; then
  pass "(6) resume: ${RESUME_CONNS} new socket(s) on B's authrpc port (${AUTH_BEFORE} before, ${AUTH_AFTER} after): one keep-alive connection, not one per block"
else
  fail "(6) resume: ${RESUME_CONNS} new sockets on B's authrpc port for ${N} present blocks (${AUTH_BEFORE} before, ${AUTH_AFTER} after): connections are not reused"
fi

echo ""
echo "=== diagnostics ==="
echo "  epoch base ${EPOCH_BASE}; N=${N}; burns ${BURN_COUNT} (sealed ${SEALED}); recorder blocks ${RECORDER_BLOCKS[*]}; reorg at Zcash ${FORK_Z} (Sova ${REORG_SOVA}..${H}); tampered #${TAMPER_HEIGHT}"
echo "  tool: tampered run ${TAMPER_S} s to #$((TAMPER_HEIGHT - 1)); good run ${REBUILD_S} s; resume ${RESUME_S} s (${RESUME_CONNS} sockets); hold notes ${HOLDS_LINE}; total $((SECONDS - T0)) s"
echo ""
RATE="$(sed -nE 's/.*\(([0-9.]+) blocks\/s\).*/\1/p' <<<"${OK_LINE}")"
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "REBUILD FROM ARCHIVE SCENARIO PASSED (${N} blocks; rebuild ${RATE:-?} blocks/s)"
else
  echo "REBUILD FROM ARCHIVE SCENARIO: ${FAILURES} ASSERTION(S) FAILED (${N} blocks)" >&2
fi
[[ "${FAILURES}" -eq 0 ]]
