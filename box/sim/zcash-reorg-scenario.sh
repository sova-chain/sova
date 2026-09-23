#!/usr/bin/env bash
# SIP-4 §7: a Zcash reorg across a Zcash-dependent transaction rolls Sova
# back identically on every node (sips/sip-4-draft-zcash-state-precompile.md
# §1, §7, §11 box scenario 2).
#
# ACCEPTANCE TEST for the §7 rollback. EXPECTED TO FAIL until it lands:
# today the follower's Rollback unwinds the expectations, the Zcash index
# and the candidates, but nothing moves the Sova head back (driver.rs,
# "zcash reorg observed (v0: log-only)"), so blocks anchored to the
# orphaned Zcash branch stay canonical. The pre-reorg phase (P) is the
# control and passes today.
#
#   node A -- mine mode (the only producer), sova/1, dev profile, miner
#             identity from `sova-miner init` (A is rank 0 for its burn).
#   node B -- follow-only, C5-enforcing, sova/1 static peer = A.
# Both share one regtest zebrad, so both see the same reorg at once.
#
# The recorder (inline bytecode, 75-byte runtime). Calldata = a Zcash txid
# (32 bytes, display order):
#   mstore(0, 0x0ac6923d << 224)                     // txInfo(bytes32)
#   calldatacopy(4, 0, 32)
#   ok := staticcall(gas(), 0x5A00, 0, 0x24, 0, 0xc0)
#   if iszero(ok) { revert(0, 0) }
#   sstore(3*number(),     mload(0x00) + 1)  // status + 1 (0 = no call)
#   sstore(3*number() + 1, mload(0x20))      // Zcash height of the tx
#   sstore(3*number() + 2, mload(0x60))      // confirmations = E_N - h + 1
# so block N's state root depends on what the node's index says about T
# as of E_N = N + B - 1.
#
# Flow:
#   1. Fund the miner (101 Zcash blocks), epoch base B = tip + 1. Start A
#      and B, auto-mine, deploy the recorder.
#   2. T = a real SIP-1 burn (sova-miner, crediting A's miner) mined at
#      Zcash height h. Its epoch mints at Sova N_burn = h - B + 1.
#      PRE_RECORDER_TXS recorder calls land in blocks after N_burn and see
#      T found at h.
#   3. Freeze Zcash (after RPC_QUEUE_WAIT_S from the burn, trickling one
#      block per 15 s, so zebrad's sendrawtransaction retry queue has let
#      go of T and won't re-submit it on the new branch; see the freeze
#      step). invalidateblock(h): Zcash tip R = h - 1. Every Sova
#      block with E_N > R, i.e. N > N_R = R - B + 1, is now anchored to an
#      orphaned Zcash block.
#   4. Mine a replacement branch WITHOUT T, REPLACEMENT_EXTRA blocks longer
#      than the old one; resume auto-mine; MID_RECORDER_TXS recorder calls.
#   5. Re-broadcast T (unless REINCLUDE=0): it is mined at h' on the new
#      branch; POST_RECORDER_TXS recorder calls after its new mint block.
#
# Assertions:
#   (0) setup   -- sova/1 on both nodes (authrpc loopback, no relay), B
#                  enforces C5, session up, recorder code identical on A/B.
#   (P) control, before the reorg -- A and B agree on every block; every
#                  anchor == zebrad; recorder blocks after N_burn saw T at h
#                  with confirmations E_N - h + 1; the burn minted at N_burn;
#                  the miner's balance == the sum of its withdrawals (the
#                  txs pay zero priority fee, so fees can't blur the check).
#   (r) rollback -- while Zcash has no replacement branch yet (a state
#                  the real network can't produce: a reorg IS a longer branch
#                  arriving), neither node seals on the stale tip: heads stay
#                  at H_old for ROLLBACK_TIMEOUT_S. reth moves a head back only
#                  when a replacement block exists; the re-seal on the new
#                  branch is asserted by (a)-(d). SIP-4 §7 documents the
#                  stale-read window this leaves on RPC.
#   (a) anchors  -- at the end, on A and B, every canonical block
#                  1..head has parentBeaconBlockRoot == zebrad's
#                  getblockhash(N + B - 1) on the NEW branch. Blocks
#                  N_R+1..H_old were re-sealed (new hashes); 1..N_R kept
#                  their pre-reorg hashes.
#   (b) convergence -- A and B identical block hash AND stateRoot at every
#                  height N_R..head; same head.
#   (c) recorder -- every canonical recorder observation above N_R (slot
#                  3N != 0) equals the new branch's derivation: NOT_FOUND
#                  while E_N < h', else (h', E_N - h' + 1); identical on A
#                  and B. At least one NOT_FOUND observation and (REINCLUDE)
#                  one found-at-h' observation exist.
#   (d) mint     -- the orphaned burn's mint is gone: the only canonical
#                  withdrawal to the miner is at h' - B + 1 (or none with
#                  REINCLUDE=0), and the balance on A == B == the canonical
#                  withdrawal total.
#   (e) health   -- after the reorg both heads keep advancing with B within
#                  1 of A (nobody stuck holding blocks); no reputation hits.
#   Diagnostics (printed, not asserted): the heads sampled right after
#   invalidateblock, hold / engine-submit / reorg log counts per node.
#
# Needs `cast` (foundry) on PATH. Env: PRE_RECORDER_TXS (4),
# MID_RECORDER_TXS (3), POST_RECORDER_TXS (3), REPLACEMENT_EXTRA (2),
# REINCLUDE (1), ROLLBACK_TIMEOUT_S (30), RPC_QUEUE_WAIT_S (90), plus
# p2p-common.sh's
# SOVA_P2P_SIM_*, SOVA_BIN, SOVA_MINER_BIN, SOVA_P2P_SIM_KEEP_LOGS.
#
# Isolation: zebrad on :18392 (compose project sova-reorg-sim, container
# sova-zebrad-reorg), A on 9445/9451/30811, B on 9645/9651/30812. All
# overridable.

SCENARIO="zcash-reorg"
WORK_PREFIX="sova-zcash-reorg"
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18392}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-reorg}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-reorg-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=9445 9451 30811}"
: "${SOVA_P2P_SIM_B_PORTS:=9645 9651 30812}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"
trap cleanup EXIT

PRE_RECORDER_TXS="${PRE_RECORDER_TXS:-4}"
MID_RECORDER_TXS="${MID_RECORDER_TXS:-3}"
POST_RECORDER_TXS="${POST_RECORDER_TXS:-3}"
REPLACEMENT_EXTRA="${REPLACEMENT_EXTRA:-2}"
REINCLUDE="${REINCLUDE:-1}"
ROLLBACK_TIMEOUT_S="${ROLLBACK_TIMEOUT_S:-30}"
# > zebrad's regtest target spacing (75 s): see the freeze step.
RPC_QUEUE_WAIT_S="${RPC_QUEUE_WAIT_S:-90}"
AUTO_MINE_INTERVAL_S=2
FUND_BLOCKS=101
BUDGET_ZAT=200000
PER_EPOCH_ZAT=100000
# Where replacement-branch coinbase goes (zebrad's default regtest miner
# address, box/regtest/zebrad.toml): anywhere but the burn wallet.
THROWAWAY_TADDR="tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"
# reth's dev chain: account 0 of the public "test test ... junk" mnemonic,
# prefunded in the dev genesis (bin/sova/src/chain.rs). Local use only.
DEV_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
RECORDER_RUNTIME="630ac6923d60e01b6000526020600060043760c0600060246000615a005afa15604557600160005101436003025560205143600302600101556060514360030260020155005b60006000fd"
# CODECOPY the 0x4b-byte runtime from offset 0x0b and RETURN it.
RECORDER_INIT="0x604b80600b6000396000f3${RECORDER_RUNTIME}"

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

# Sign with the dev key and submit to node A; prints the tx hash. Zero
# priority fee: the fee recipient is the miner, whose balance must equal
# its minted withdrawals exactly. Explicit gas limit: no estimateGas.
send_tx() {
  cast send --async --rpc-url "${ENGINE_RPC_A}" --private-key "${DEV_KEY}" \
    --priority-gas-price 0 --gas-limit "$1" "${@:2}" 2>&1 \
    | grep -oE '0x[0-9a-fA-F]{64}' | tail -1
}

zc_block_hash_at() {
  zc_rpc getblockhash "[$1]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'].lower())" 2>/dev/null || true
}

# Height of txid $1 on zebrad's current best chain, scanning [$2, tip];
# empty if it is not there.
zc_find_tx() {
  python3 - "${ZEBRAD_RPC}" "$1" "$2" <<'PY'
import sys, json, urllib.request
url, txid, lo = sys.argv[1], sys.argv[2].lower(), int(sys.argv[3])
def rpc(m, p):
    req = urllib.request.Request(url + "/", data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.load(r).get("result")
tip = rpc("getblockcount", [])
for h in range(lo, tip + 1):
    blk = rpc("getblock", [str(h), 1])
    if blk and txid in [t.lower() for t in blk["tx"]]:
        print(h)
        break
PY
}

# "yes" if txid $1 is in zebrad's mempool, else "no". Every call is also a
# mempool request, which makes zebrad process pending tip changes.
zc_mempool_has() {
  zc_rpc getrawmempool "[]" | python3 -c "
import sys, json
try:
    ids = [t.lower() for t in (json.load(sys.stdin).get('result') or [])]
except Exception:
    ids = []
print('yes' if '$1'.lower() in ids else 'no')
"
}

# First non-coinbase txid in Zcash blocks [$1, tip] as "height txid".
zc_first_non_coinbase() {
  python3 - "${ZEBRAD_RPC}" "$1" <<'PY'
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
    if blk and len(blk["tx"]) > 1:
        print(h, blk["tx"][1].lower())
        break
PY
}

# One line per Sova height in [$2, $3] on node $1:
#   N hash stateRoot parentBeaconBlockRoot zcashHash(N+B-1) wdGweiToMiner rec0 rec1 rec2
# rec* are the recorder's slots 3N..3N+2 in the state after block N.
# zcashHash is zebrad's CURRENT best chain at E_N ("-" if none).
dump_chain() {
  python3 - "$1" "$2" "$3" "${EVM_ADDR}" "${RECORDER}" "${EPOCH_BASE}" "${ZEBRAD_RPC}" <<'PY'
import sys, json, urllib.request
url, lo, hi, miner, rec, base, zurl = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4].lower(), sys.argv[5], int(sys.argv[6]), sys.argv[7]
def rpc(u, m, p):
    req = urllib.request.Request(u, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return json.load(r).get("result")
    except Exception:
        return None
for n in range(lo, hi + 1):
    zh = rpc(zurl + "/", "getblockhash", [n + base - 1])
    zh = ("0x" + zh.lower()) if zh else "-"
    b = rpc(url, "eth_getBlockByNumber", [hex(n), False])
    if not b:
        print(n, "-", "-", "-", zh, 0, 0, 0, 0)
        continue
    wd = sum(int(w["amount"], 16) for w in (b.get("withdrawals") or []) if w["address"].lower() == miner)
    def slot(k):
        r = rpc(url, "eth_getStorageAt", [rec, hex(k), hex(n)])
        return int(r, 16) if r else 0
    print(n, b["hash"].lower(), b["stateRoot"].lower(), (b.get("parentBeaconBlockRoot") or "-").lower(), zh,
          wd, slot(3 * n), slot(3 * n + 1), slot(3 * n + 2))
PY
}

# zebrad's own log (mempool resets, invalidation) into the work dir, so
# SOVA_P2P_SIM_KEEP_LOGS keeps it.
save_zebrad_log() {
  docker logs "${ZEBRAD_CONTAINER}" >"${WORK_DIR}/zebrad.log" 2>&1 || true
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

# Send $2 recorder calls (calldata $1 = txid), each after the previous
# one's receipt, and print the blocks they landed in (on A, at the time).
RECORDER_BLOCKS=()
record_calls() { # <txid> <count> <label>
  local i tx r blk st
  RECORDER_BLOCKS=()
  for ((i = 1; i <= $2; i++)); do
    tx="$(send_tx 200000 "${RECORDER}" "0x$1")"
    if [[ -z "${tx}" ]]; then
      fail "$3: recorder tx ${i} was not accepted by node A"
      return 1
    fi
    if ! r="$(wait_for_receipt "${ENGINE_RPC_A}" "${tx}" 90)"; then
      fail "$3: recorder tx ${i} (${tx}) never mined on A within 90s (A head $(height_of "${ENGINE_RPC_A}"))"
      return 1
    fi
    read -r blk st _ _ <<<"${r}"
    echo "  $3 tx ${i}: ${tx} -> block ${blk} status ${st}"
    [[ "${st}" != "1" ]] && fail "$3: recorder tx ${tx} reverted (status ${st})"
    RECORDER_BLOCKS+=("${blk}")
  done
}

NOT_FOUND_PLUS1=2 # status NOT_FOUND (1) + 1

preflight
start_stack

# ---------------------------------------------------------------------
# Miner identity; fund it; epoch base after the funding.
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
EPOCH_BASE=$(($(zc_tip_height) + 1))
echo "epoch base B=${EPOCH_BASE} (Sova block N anchors Zcash N+${EPOCH_BASE}-1)"

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
echo "=== (0) setup ==="
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

DEPLOY_TX="$(send_tx 300000 --create "${RECORDER_INIT}")"
if [[ -z "${DEPLOY_TX}" ]] || ! DEPLOY_R="$(wait_for_receipt "${ENGINE_RPC_A}" "${DEPLOY_TX}" 90)"; then
  fail "setup: recorder deploy tx ${DEPLOY_TX:-<not accepted>} never mined"
  exit 1
fi
read -r DEPLOY_BLOCK DEPLOY_STATUS _ RECORDER <<<"${DEPLOY_R}"
wait_for_block_number "${ENGINE_RPC_B}" "${DEPLOY_BLOCK}" 60 || true
CODE_A="$(eth_rpc "${ENGINE_RPC_A}" eth_getCode "[\"${RECORDER}\", \"latest\"]" | python3 -c "import sys,json;print(json.load(sys.stdin).get('result',''))" 2>/dev/null)"
CODE_B="$(eth_rpc "${ENGINE_RPC_B}" eth_getCode "[\"${RECORDER}\", \"latest\"]" | python3 -c "import sys,json;print(json.load(sys.stdin).get('result',''))" 2>/dev/null)"
if [[ "${DEPLOY_STATUS}" == "1" && "${CODE_A}" == "0x${RECORDER_RUNTIME}" && "${CODE_A}" == "${CODE_B}" ]]; then
  pass "setup: recorder ${RECORDER} (block ${DEPLOY_BLOCK}) code identical on A and B (75-byte runtime)"
else
  fail "setup: recorder deploy status=${DEPLOY_STATUS}; code A=${CODE_A:-<none>} B=${CODE_B:-<none>}"
  exit 1
fi

# ---------------------------------------------------------------------
# T: a real SIP-1 burn crediting A's miner.
# ---------------------------------------------------------------------
echo ""
echo "=== burn T (sova-miner, credits ${EVM_ADDR}) ==="
BURN_SENT_AT=${SECONDS}
if ! "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
  --budget-zat "${BUDGET_ZAT}" --per-epoch-zat "${PER_EPOCH_ZAT}" \
  --rpc "${ZEBRAD_RPC}" --max-epochs 1 >"${WORK_DIR}/mine-1.log" 2>&1; then
  fail "setup: sova-miner mine exited nonzero"
  cat "${WORK_DIR}/mine-1.log" >&2
  exit 1
fi
read -r T_HEIGHT T_TXID <<<"$(zc_first_non_coinbase "${EPOCH_BASE}")"
if [[ -z "${T_TXID:-}" ]]; then
  fail "setup: no burn tx found on zebrad at or above the epoch base"
  exit 1
fi
N_BURN=$((T_HEIGHT - EPOCH_BASE + 1))
R=$((T_HEIGHT - 1))
N_R=$((R - EPOCH_BASE + 1))
echo "T = ${T_TXID} at Zcash h=${T_HEIGHT}; mints at Sova N_burn=${N_BURN}; a reorg to R=${R} must roll Sova back to N_R=${N_R}"
if ! wait_for_block_number "${ENGINE_RPC_A}" "${N_BURN}" 90; then
  fail "setup: A never sealed the burn epoch's block ${N_BURN}"
  exit 1
fi

echo ""
echo "=== recorder calls observing T (${PRE_RECORDER_TXS}) ==="
record_calls "${T_TXID}" "${PRE_RECORDER_TXS}" "pre-reorg" || exit 1
PRE_BLOCKS=("${RECORDER_BLOCKS[@]}")

# ---------------------------------------------------------------------
# Freeze Zcash; snapshot the pre-reorg chain.
# ---------------------------------------------------------------------
stop_auto_mine
# zebrad keeps every sendrawtransaction tx in an RPC retry queue
# (zebra-rpc/src/queue.rs) and drops it only when a queue pass -- one per
# target block spacing, 75 s on regtest, and only if the tip height moved
# since the last pass -- finds it in the mempool or the chain. If no pass
# has run between T's broadcast and invalidateblock, the next one
# re-submits T on the new branch and the replacement branch re-mines it at
# h (hit in 2 of the first 5 local runs). So let one full pass happen,
# with a slow trickle of blocks so the tip keeps moving but the reorg
# stays shallow.
while [[ $((SECONDS - BURN_SENT_AT)) -lt "${RPC_QUEUE_WAIT_S}" ]]; do
  echo "  waiting out zebrad's RPC retry queue: $((SECONDS - BURN_SENT_AT))s/${RPC_QUEUE_WAIT_S}s since the burn; one Zcash block"
  "${REGTEST_DIR}/mine.sh" 1 "${ZEBRAD_RPC}" >>"${WORK_DIR}/auto-mine.log" 2>&1 || true
  sleep 15
done
OLD_ZTIP="$(zc_tip_height)"
H_OLD="$(wait_for_stable_height "${ENGINE_RPC_A}" "${PRE_BLOCKS[${#PRE_BLOCKS[@]} - 1]}" 60)" || {
  fail "setup: node A did not settle before the reorg (at ${H_OLD})"
  exit 1
}
wait_for_block_number "${ENGINE_RPC_B}" "${H_OLD}" 60 || true
echo "frozen: Zcash tip ${OLD_ZTIP}, Sova head A=${H_OLD} B=$(height_of "${ENGINE_RPC_B}")"
dump_chain "${ENGINE_RPC_A}" 1 "${H_OLD}" >"${WORK_DIR}/pre-a.txt"
dump_chain "${ENGINE_RPC_B}" 1 "${H_OLD}" >"${WORK_DIR}/pre-b.txt"

echo ""
echo "=== (P) control: the pre-reorg chain ==="
if cmp -s "${WORK_DIR}/pre-a.txt" "${WORK_DIR}/pre-b.txt"; then
  pass "(P) A and B identical at every height 1..${H_OLD} (hash, stateRoot, anchor, withdrawals, recorder slots)"
else
  fail "(P) A and B differ before the reorg: $(diff "${WORK_DIR}/pre-a.txt" "${WORK_DIR}/pre-b.txt" | head -4 | tr '\n' ' ')"
fi
BAD_ANCHORS="$(awk '$4 != $5 {print $1}' "${WORK_DIR}/pre-a.txt" | tr '\n' ' ')"
if [[ -z "${BAD_ANCHORS}" ]]; then
  pass "(P) every anchor 1..${H_OLD} == zebrad getblockhash(N+B-1)"
else
  fail "(P) anchors differing from zebrad before the reorg at: ${BAD_ANCHORS}"
fi
for n in "${PRE_BLOCKS[@]}"; do
  read -r _ _ _ _ _ _ s0 s1 s2 <<<"$(awk -v n="${n}" '$1 == n' "${WORK_DIR}/pre-a.txt")"
  e=$((n + EPOCH_BASE - 1))
  if [[ "${s0}" == "1" && "${s1}" == "${T_HEIGHT}" && "${s2}" == "$((e - T_HEIGHT + 1))" ]]; then
    pass "(P) block ${n} (E=${e}): recorder saw T found at h=${T_HEIGHT}, confirmations $((e - T_HEIGHT + 1))"
  else
    fail "(P) block ${n} (E=${e}): recorder stored status+1=${s0} height=${s1} conf=${s2}; want 1/${T_HEIGHT}/$((e - T_HEIGHT + 1))"
  fi
done
WD_PRE="$(awk '$6 > 0 {print $1 ":" $6}' "${WORK_DIR}/pre-a.txt" | tr '\n' ' ')"
REWARD_GWEI="$(awk -v n="${N_BURN}" '$1 == n {print $6}' "${WORK_DIR}/pre-a.txt")"
if [[ "${WD_PRE}" == "${N_BURN}:${REWARD_GWEI} " && "${REWARD_GWEI:-0}" -gt 0 ]]; then
  pass "(P) the burn minted ${REWARD_GWEI} gwei to the miner at N_burn=${N_BURN}, and nowhere else"
else
  fail "(P) miner withdrawals before the reorg: '${WD_PRE}' (want only ${N_BURN})"
fi
BAL_PRE_A="$(eth_balance_wei "${ENGINE_RPC_A}" "${EVM_ADDR}")"
if [[ "${BAL_PRE_A}" == "$(python3 -c "print(${REWARD_GWEI:-0} * 10**9)")" ]]; then
  pass "(P) miner balance ${BAL_PRE_A} wei == its withdrawals (no fee income blurs the mint check)"
else
  fail "(P) miner balance ${BAL_PRE_A} wei != its withdrawals (${REWARD_GWEI:-0} gwei); the (d) balance check is unreliable"
fi

# ---------------------------------------------------------------------
# The reorg: invalidate h. Zcash tip R; nothing above N_R is valid.
# ---------------------------------------------------------------------
echo ""
echo "=== reorg: invalidateblock(h=${T_HEIGHT}) (depth $((OLD_ZTIP - T_HEIGHT + 1))) ==="
RAW_T="$(zc_rpc getrawtransaction "[\"${T_TXID}\", 0]" | python3 -c "import sys,json;print(json.load(sys.stdin).get('result') or '')")"
H_HASH="$(zc_block_hash_at "${T_HEIGHT}")"
zc_rpc invalidateblock "[\"${H_HASH}\"]" >"${WORK_DIR}/invalidate.json"
ZTIP_AFTER="$(zc_tip_height)"
if [[ "${ZTIP_AFTER}" != "${R}" ]]; then
  fail "setup: zebrad tip ${ZTIP_AFTER} after invalidateblock, want ${R} ($(cat "${WORK_DIR}/invalidate.json"))"
  exit 1
fi
# The fork reset (and any re-verification) runs on the mempool's next
# polls. Watch it here and through the whole rollback window below: T
# must stay out, or the replacement branch would re-mine it at h.
T_IN_MEMPOOL=no
for _ in 1 2 3 4 5; do
  sleep 1
  [[ "$(zc_mempool_has "${T_TXID}")" == "yes" ]] && T_IN_MEMPOOL=yes
done
echo "zebrad tip now ${ZTIP_AFTER} (= R)"

echo ""
echo "=== (r) no sealing on the stale tip while Zcash has no replacement branch (N_R=${N_R}, H_old=${H_OLD}) ==="
MAX_A=0
MAX_B=0
DEADLINE=$((SECONDS + ROLLBACK_TIMEOUT_S))
while [[ ${SECONDS} -lt ${DEADLINE} ]]; do
  ha="$(height_of "${ENGINE_RPC_A}")"
  hb="$(height_of "${ENGINE_RPC_B}")"
  [[ "${ha}" -gt "${MAX_A}" ]] && MAX_A="${ha}"
  [[ "${hb}" -gt "${MAX_B}" ]] && MAX_B="${hb}"
  [[ "$(zc_mempool_has "${T_TXID}")" == "yes" ]] && T_IN_MEMPOOL=yes
  sleep 0.5
done
for pair in "A ${MAX_A}" "B ${MAX_B}"; do
  read -r label max <<<"${pair}"
  if [[ "${max}" -le "${H_OLD}" ]]; then
    pass "(r) node ${label}: nothing sealed on the stale tip for ${ROLLBACK_TIMEOUT_S}s (max head ${max} <= H_old ${H_OLD})"
  else
    fail "(r) node ${label}: head grew to ${max} on a tip anchored to orphaned Zcash blocks (H_old ${H_OLD})"
  fi
done

# ---------------------------------------------------------------------
# Replacement branch without T, longer than the old one; then resume.
# ---------------------------------------------------------------------
echo "T back in zebrad's mempool at any point since invalidateblock: ${T_IN_MEMPOOL}"
if [[ "${T_IN_MEMPOOL}" == "yes" ]]; then
  save_zebrad_log
  fail "setup: zebrad put T back in its mempool after invalidateblock; the replacement branch would re-mine it at h (see zebrad.log)"
  exit 1
fi

REPLACEMENT=$((OLD_ZTIP - R + REPLACEMENT_EXTRA))
echo ""
echo "--- mining the replacement branch: ${REPLACEMENT} blocks (new tip $((R + REPLACEMENT)) > old tip ${OLD_ZTIP}) ---"
zc_generate_to_address "${REPLACEMENT}" "${THROWAWAY_TADDR}"
save_zebrad_log
T_AFTER_REPL="$(zc_find_tx "${T_TXID}" "${EPOCH_BASE}")"
if [[ -z "${T_AFTER_REPL}" ]]; then
  echo "replacement branch (tip $(zc_tip_height)) does not contain T"
else
  fail "setup: zebrad re-mined T into the replacement branch at ${T_AFTER_REPL}; the branch was meant to be T-less"
fi
start_auto_mine

echo ""
echo "=== recorder calls on the new branch (${MID_RECORDER_TXS}) ==="
if ! wait_for_block_number "${ENGINE_RPC_A}" $((H_OLD + 1)) 90; then
  fail "(e) A never sealed past the old head ${H_OLD} on the new branch (A=$(height_of "${ENGINE_RPC_A}"))"
fi
record_calls "${T_TXID}" "${MID_RECORDER_TXS}" "mid" || true

T_NEW=""
if [[ "${REINCLUDE}" == "1" && -z "${T_AFTER_REPL}" ]]; then
  echo ""
  echo "=== re-broadcast T on the new branch ==="
  SEND_R="$(zc_rpc sendrawtransaction "[\"${RAW_T}\"]")"
  echo "sendrawtransaction: ${SEND_R}"
  DEADLINE=$((SECONDS + 60))
  while [[ ${SECONDS} -lt ${DEADLINE} ]]; do
    T_NEW="$(zc_find_tx "${T_TXID}" $((R + 1)))"
    [[ -n "${T_NEW}" ]] && break
    sleep 2
  done
  if [[ -z "${T_NEW}" ]]; then
    fail "setup: re-broadcast T was not mined within 60s"
  else
    echo "T re-mined at Zcash h'=${T_NEW}; its mint belongs at Sova $((T_NEW - EPOCH_BASE + 1))"
    wait_for_block_number "${ENGINE_RPC_A}" $((T_NEW - EPOCH_BASE + 1)) 90 || true
    echo ""
    echo "=== recorder calls after T's new mint block (${POST_RECORDER_TXS}) ==="
    record_calls "${T_TXID}" "${POST_RECORDER_TXS}" "post" || true
  fi
elif [[ -n "${T_AFTER_REPL}" ]]; then
  T_NEW="${T_AFTER_REPL}"
fi

# ============================================================
# (e) liveness after the reorg: both keep advancing, B tracks A
# ============================================================
echo ""
echo "=== (e) liveness after the reorg ==="
LA0="$(height_of "${ENGINE_RPC_A}")"
sleep $((AUTO_MINE_INTERVAL_S * 4))
LIVE_OK=0
for i in 1 2 3; do
  la="$(height_of "${ENGINE_RPC_A}")"
  lb="$(height_of "${ENGINE_RPC_B}")"
  lag=$((la - lb))
  echo "  sample ${i}: A=${la} B=${lb} lag=${lag}"
  # A is sampled first, so B can read one ahead: |lag| <= 1.
  [[ "${lag}" -ge -1 && "${lag}" -le 1 ]] && LIVE_OK=$((LIVE_OK + 1))
  [[ ${i} -lt 3 ]] && sleep 3
done
if [[ "${la}" -gt "${LA0}" ]]; then
  pass "(e) A keeps sealing after the reorg (${LA0} -> ${la})"
else
  fail "(e) A stuck at ${la} after the reorg"
fi
if [[ "${LIVE_OK}" -eq 3 ]]; then
  pass "(e) B tracks A within 1 block on 3 samples (not stuck holding blocks)"
else
  fail "(e) A and B more than 1 apart on $((3 - LIVE_OK))/3 samples (stuck holding blocks?)"
fi

stop_auto_mine
FINAL="$(wait_for_stable_height "${ENGINE_RPC_A}" 1 60)" || true
wait_for_block_number "${ENGINE_RPC_B}" "${FINAL}" 60 || true
FINAL_B="$(wait_for_stable_height "${ENGINE_RPC_B}" 1 20)" || true
echo ""
echo "final: Zcash tip $(zc_tip_height), Sova head A=${FINAL} B=${FINAL_B}; T at h'=${T_NEW:-<not on the new branch>}"
dump_chain "${ENGINE_RPC_A}" 1 "${FINAL}" >"${WORK_DIR}/post-a.txt"
dump_chain "${ENGINE_RPC_B}" 1 "${FINAL}" >"${WORK_DIR}/post-b.txt"

# ============================================================
# (a) every canonical anchor is on the new branch
# ============================================================
echo ""
echo "=== (a) anchors: every canonical block commits to zebrad's new branch ==="
for n in a b; do
  label="$(tr a-z A-Z <<<"${n}")"
  bad="$(awk '$4 != $5 {print $1}' "${WORK_DIR}/post-${n}.txt")"
  if [[ -z "${bad}" ]]; then
    pass "(a) node ${label}: every anchor 1..${FINAL} == zebrad getblockhash(N+B-1) on the new branch"
  else
    fail "(a) node ${label}: $(wc -l <<<"${bad}" | tr -d ' ') canonical block(s) anchored to ORPHANED Zcash blocks: heights $(head -1 <<<"${bad}")..$(tail -1 <<<"${bad}") (want none above N_R=${N_R})"
  fi
done
RESEALED=0
KEPT=0
STALE=""
for ((n = 1; n <= H_OLD; n++)); do
  pre="$(awk -v n="${n}" '$1 == n {print $2}' "${WORK_DIR}/pre-a.txt")"
  post="$(awk -v n="${n}" '$1 == n {print $2}' "${WORK_DIR}/post-a.txt")"
  if [[ "${n}" -le "${N_R}" ]]; then
    [[ "${pre}" == "${post}" ]] && KEPT=$((KEPT + 1))
  elif [[ "${pre}" != "${post}" ]]; then
    RESEALED=$((RESEALED + 1))
  else
    STALE+="${n} "
  fi
done
if [[ "${KEPT}" -eq "${N_R}" ]]; then
  pass "(a) blocks 1..${N_R} (anchored at or below R) kept their pre-reorg hashes"
else
  fail "(a) only ${KEPT}/${N_R} blocks at or below N_R kept their pre-reorg hash (rolled back too far?)"
fi
if [[ -z "${STALE}" ]]; then
  pass "(a) blocks $((N_R + 1))..${H_OLD} were re-sealed on the new branch (${RESEALED} new hashes)"
else
  fail "(a) blocks still carrying their PRE-reorg hash above N_R=${N_R}: ${STALE}"
fi

# ============================================================
# (b) convergence
# ============================================================
echo ""
echo "=== (b) A and B identical from N_R to the head ==="
if [[ "${FINAL}" -eq "${FINAL_B}" ]]; then
  pass "(b) same head on A and B (${FINAL})"
else
  fail "(b) heads differ: A=${FINAL} B=${FINAL_B}"
fi
# Both dumps cover 1..FINAL line for line: columns 2-3 are A's hash and
# stateRoot, 11-12 B's.
DIFF_HEIGHTS="$(paste -d' ' "${WORK_DIR}/post-a.txt" "${WORK_DIR}/post-b.txt" \
  | awk -v lo="${N_R}" '$1 >= lo && ($2 != $11 || $3 != $12 || $2 == "-") {print $1}' | tr '\n' ' ')"
if [[ -z "${DIFF_HEIGHTS}" ]]; then
  read -r _ tip_hash tip_root _ <<<"$(awk -v n="${FINAL}" '$1 == n' "${WORK_DIR}/post-a.txt")"
  pass "(b) block hash and stateRoot identical on A and B at every height ${N_R}..${FINAL} (tip ${tip_hash} / ${tip_root})"
else
  fail "(b) A and B differ (hash or stateRoot) at: ${DIFF_HEIGHTS}"
fi

# ============================================================
# (c) recorder observations follow the new branch
# ============================================================
echo ""
echo "=== (c) recorder observations above N_R follow the new branch ==="
OBS_NOT_FOUND=0
OBS_FOUND_NEW=0
OBS_BAD=0
while read -r n _ _ _ _ _ s0 s1 s2; do
  [[ "${n}" -le "${N_R}" || "${s0}" == "0" ]] && continue
  e=$((n + EPOCH_BASE - 1))
  if [[ -n "${T_NEW}" && "${T_NEW}" -le "${e}" ]]; then
    want="1 ${T_NEW} $((e - T_NEW + 1))"
  else
    want="${NOT_FOUND_PLUS1} 0 0"
  fi
  read -r _ _ _ _ _ _ b0 b1 b2 <<<"$(awk -v n="${n}" '$1 == n' "${WORK_DIR}/post-b.txt")"
  if [[ "${s0} ${s1} ${s2}" == "${want}" && "${b0} ${b1} ${b2}" == "${want}" ]]; then
    if [[ "${want}" == "${NOT_FOUND_PLUS1} 0 0" ]]; then
      OBS_NOT_FOUND=$((OBS_NOT_FOUND + 1))
      pass "(c) block ${n} (E=${e}): T NOT_FOUND on A and B (new branch, T not yet re-mined)"
    else
      OBS_FOUND_NEW=$((OBS_FOUND_NEW + 1))
      pass "(c) block ${n} (E=${e}): T found at h'=${T_NEW}, confirmations $((e - T_NEW + 1)), on A and B"
    fi
  else
    OBS_BAD=$((OBS_BAD + 1))
    fail "(c) block ${n} (E=${e}): stored status+1/height/conf A='${s0} ${s1} ${s2}' B='${b0} ${b1} ${b2}', want '${want}' (new-branch derivation)"
  fi
done <"${WORK_DIR}/post-a.txt"
if [[ "${OBS_NOT_FOUND}" -ge 1 ]]; then
  pass "(c) ${OBS_NOT_FOUND} canonical observation(s) of T as NOT_FOUND on the new branch"
else
  fail "(c) no canonical recorder block observed T as NOT_FOUND after the reorg"
fi
if [[ "${REINCLUDE}" == "1" ]]; then
  if [[ "${OBS_FOUND_NEW}" -ge 1 ]]; then
    pass "(c) ${OBS_FOUND_NEW} canonical observation(s) of T at its new height ${T_NEW} with new confirmations"
  else
    fail "(c) no canonical recorder block observed T at its new height ${T_NEW:-<none>}"
  fi
fi

# ============================================================
# (d) the orphaned burn's mint is gone
# ============================================================
echo ""
echo "=== (d) mint follows the new branch ==="
WD_POST="$(awk '$6 > 0 {print $1}' "${WORK_DIR}/post-a.txt" | tr '\n' ' ')"
WANT_WD=""
if [[ -n "${T_NEW}" && $((T_NEW - EPOCH_BASE + 1)) -le "${FINAL}" ]]; then
  WANT_WD="$((T_NEW - EPOCH_BASE + 1)) "
fi
if [[ "${WD_POST}" == "${WANT_WD}" ]]; then
  pass "(d) canonical miner withdrawals only at '${WANT_WD:-<none>}' (the orphaned burn's mint at ${N_BURN} is gone)"
else
  fail "(d) canonical miner withdrawals at '${WD_POST}', want '${WANT_WD:-<none>}' (orphaned burn minted at N_burn=${N_BURN})"
fi
WD_SUM_GWEI="$(awk '{s += $6} END {print s + 0}' "${WORK_DIR}/post-a.txt")"
WANT_BAL="$(python3 -c "print(${WD_SUM_GWEI} * 10**9)")"
BAL_A="$(eth_balance_wei "${ENGINE_RPC_A}" "${EVM_ADDR}")"
BAL_B="$(eth_balance_wei "${ENGINE_RPC_B}" "${EVM_ADDR}")"
if [[ "${BAL_A}" == "${BAL_B}" && "${BAL_A}" == "${WANT_BAL}" ]]; then
  pass "(d) miner balance A == B == ${BAL_A} wei == canonical withdrawals"
else
  fail "(d) miner balance A=${BAL_A} B=${BAL_B} wei, want ${WANT_BAL} (canonical withdrawals; pre-reorg ${BAL_PRE_A})"
fi
EXPECTED_MINTS=0
[[ -n "${WANT_WD}" ]] && EXPECTED_MINTS=1
if [[ "${REWARD_GWEI:-0}" -gt 0 ]]; then
  if [[ "${BAL_A}" == "$(python3 -c "print(${EXPECTED_MINTS} * ${REWARD_GWEI} * 10**9)")" ]]; then
    pass "(d) balance == ${EXPECTED_MINTS} epoch reward(s): exactly the burns on the new branch"
  else
    fail "(d) balance ${BAL_A} wei == $(python3 -c "print(${BAL_A} / (${REWARD_GWEI} * 10**9))") epoch rewards, want ${EXPECTED_MINTS}"
  fi
fi

# ============================================================
# (e) reputation
# ============================================================
echo ""
echo "=== (e) reputation ==="
for n in a b; do
  hits="$(count_in "${WORK_DIR}/node-${n}.log" 'reputation hit')"
  if [[ "${hits}" -gt 0 ]]; then
    fail "(e) node $(tr a-z A-Z <<<"${n}") logged ${hits} reputation hit(s) / INVALID peer block(s)"
  else
    pass "(e) node $(tr a-z A-Z <<<"${n}"): no reputation hits"
  fi
done

# ============================================================
# Diagnostics (not asserted)
# ============================================================
echo ""
echo "=== diagnostics ==="
save_zebrad_log
echo "  heights: B=${EPOCH_BASE} h=${T_HEIGHT} R=${R} N_R=${N_R} N_burn=${N_BURN} old Zcash tip ${OLD_ZTIP} old Sova head ${H_OLD}; T in mempool after invalidate: ${T_IN_MEMPOOL}; h'=${T_NEW:-none}"
echo "  heads within ${ROLLBACK_TIMEOUT_S}s of invalidateblock (no replacement yet): A max ${MAX_A}, B max ${MAX_B} (H_old ${H_OLD})"
echo "  recorder blocks: pre ${PRE_BLOCKS[*]}"
for n in a b; do
  log="${WORK_DIR}/node-${n}.log"
  echo "  node $(tr a-z A-Z <<<"${n}"): 'zcash reorg observed': $(count_in "${log}" 'zcash reorg observed');" \
    "'expectations and candidates unwound': $(count_in "${log}" 'expectations and candidates unwound');" \
    "'block held': $(count_in "${log}" 'sova/1: block held'); 'anchor mismatch': $(count_in "${log}" 'zcash anchor mismatch');" \
    "'engine submit failed': $(count_in "${log}" 'engine submit failed'); 'sova zcash precompile': $(count_in "${log}" 'sova zcash precompile')"
done
echo "  post-reorg chain on A (N hash anchor zcash@E wd rec):"
awk -v lo="${N_R}" '$1 >= lo {printf "    %s %s anchor=%s zebrad=%s wd=%s rec=%s/%s/%s\n", $1, substr($2,1,12), substr($4,1,12), substr($5,1,12), $6, $7, $8, $9}' "${WORK_DIR}/post-a.txt"

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "ZCASH REORG SCENARIO PASSED (h=${T_HEIGHT} -> h'=${T_NEW:-none}, rolled back ${H_OLD} -> ${N_R}, head ${FINAL}; all assertions)"
else
  echo "ZCASH REORG SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
