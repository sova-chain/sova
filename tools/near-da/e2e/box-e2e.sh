#!/usr/bin/env bash
# NEAR DA end to end on the box regtest chain, posting to NEAR TESTNET.
#
#   1. zebrad regtest (docker compose, own project/ports) + a mine-mode Sova
#      node with SOVA_RPC_DEBUG=1 + the box miner + auto-mine.
#   2. A fresh NEAR testnet account e2e-<time>.<parent>, funded by the parent,
#      with the index contract deployed and initialised for this chain.
#   3. The poster (--once, --depth 2) posts every block 0..head-2, in small
#      batches. Then, after more blocks: again with the state file deleted,
#      which must resume from the contract (no double post, no gap).
#   4. fetch from NEAR (public RPC) into a .sovada set; verify it offline;
#      byte-compare every block with the node's debug_getRawBlock.
#
# Run from a copy outside ~/Documents (Docker Desktop bind mounts from there
# hang): the script copies box/regtest to $WORK itself, so only the
# binaries and the parent key need to exist.
#
# Required env:
#   SOVA_BIN, SOVA_MINER_BIN    built binaries (release)
#   NEAR_DA_BIN                 sova-near-da
#   NEAR_DA_WASM                the contract (cargo near build)
#   NEAR_DA_PARENT              a funded NEAR testnet account, e.g. sova-da.testnet
#   NEAR_DA_PARENT_KEY          its key file (never printed)
# Optional:
#   NEAR_DA_KEY_DIR             where the e2e account's key goes
#                               (default ~/.config/sova-near-da)
#   NEAR_DA_E2E_BLOCKS          blocks before the first post (default 40)
#   NEAR_DA_E2E_FUND            NEAR moved to the e2e account (default 2.2 NEAR:
#                               1.46 locked by the contract code, the rest gas)
#   NEAR_DA_E2E_DELETE=1        delete the NEAR account at the end (its
#                               transactions stay in NEAR history)
#   NEAR_DA_E2E_PORT_BASE       ports BASE+32 zebrad, +45 rpc, +51 auth,
#                               +3 p2p (default 18300)
#   NEAR_DA_E2E_NODE_MODE       which RPC the node serves, so which raw-block
#                               source the poster must auto-pick:
#     debug  (default) local profile + SOVA_RPC_DEBUG=1 -> debug_getRawBlock;
#            `check-sources` must also find every block rebuilt from raw txs
#            and from full tx objects identical to it.
#     local  local profile, no debug (v0.1.17 keeper) -> raw-tx rebuild.
#     public SOVA_RPC_PROFILE=public (v0.1.17 seeds/RPC) -> full-tx rebuild.
#   In local/public mode the node is restarted at the end on the same
#   datadir with local + SOVA_RPC_DEBUG=1 and every archived block is
#   byte-compared with debug_getRawBlock.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "${HERE}/../../.." && pwd)"
: "${SOVA_BIN:?}" "${SOVA_MINER_BIN:?}" "${NEAR_DA_BIN:?}" "${NEAR_DA_WASM:?}"
: "${NEAR_DA_PARENT:?}" "${NEAR_DA_PARENT_KEY:?}"
KEY_DIR="${NEAR_DA_KEY_DIR:-${HOME}/.config/sova-near-da}"
BLOCKS="${NEAR_DA_E2E_BLOCKS:-40}"
BASE="${NEAR_DA_E2E_PORT_BASE:-18300}"
ZEBRAD_PORT=$((BASE + 32))
RPC_PORT=$((BASE + 45))
AUTH_PORT=$((BASE + 51))
P2P_PORT=$((BASE + 3))
PROJECT="sova-near-da-e2e"
CONTAINER="sova-zebrad-near-da-e2e"
WORK="$(mktemp -d /private/tmp/sova-near-da-e2e.XXXXXX)"
ZEBRAD_RPC="http://127.0.0.1:${ZEBRAD_PORT}"
SOVA_RPC="http://127.0.0.1:${RPC_PORT}"
NEAR_RPC="${NEAR_DA_NEAR_RPC:-https://rpc.testnet.fastnear.com}"
PIDS=()
NODE_MODE="${NEAR_DA_E2E_NODE_MODE:-debug}"
case "${NODE_MODE}" in
  debug) WANT_SOURCE="raw blocks: debug_getRawBlock" ;;
  local) WANT_SOURCE="raw blocks: rebuilt from eth_getBlockByNumber + eth_getRawTransactionByBlockHashAndIndex" ;;
  public) WANT_SOURCE="raw blocks: rebuilt from eth_getBlockByNumber(full)" ;;
  *) echo "NEAR_DA_E2E_NODE_MODE must be debug, local or public" >&2; exit 2 ;;
esac
NODE_PID=""

log() { echo "[e2e $(date -u +%H:%M:%S)] $*"; }
fail() { echo "[e2e] FAIL: $*" >&2; exit 1; }

cleanup() {
  local rc=$?
  for p in "${PIDS[@]+"${PIDS[@]}"}"; do kill "$p" 2>/dev/null || true; done
  sleep 1
  for p in "${PIDS[@]+"${PIDS[@]}"}"; do kill -9 "$p" 2>/dev/null || true; done
  (cd "${WORK}/regtest" && SOVA_BOX_ZEBRAD_CONTAINER="${CONTAINER}" SOVA_BOX_ZEBRAD_PORT="${ZEBRAD_PORT}" \
    docker compose -p "${PROJECT}" down -v >/dev/null 2>&1) || true
  log "logs and fetched files kept in ${WORK} (rc=${rc})"
}
trap cleanup EXIT

rpc() { # url method params
  curl -s -m 20 -X POST -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":$3}" "$1"
}
sova_head() { rpc "${SOVA_RPC}" eth_blockNumber '[]' | python3 -c 'import json,sys; print(int(json.load(sys.stdin)["result"],16))' 2>/dev/null || echo -1; }
wait_head() { # min_height timeout_s
  local deadline=$((SECONDS + $2)) h
  while :; do
    h="$(sova_head)"
    [[ "$h" -ge "$1" ]] && { log "sova head ${h}"; return 0; }
    [[ ${SECONDS} -ge ${deadline} ]] && fail "sova head ${h} < $1 after $2 s (see ${WORK}/node.log)"
    sleep 3
  done
}

for port in "${ZEBRAD_PORT}" "${RPC_PORT}" "${AUTH_PORT}" "${P2P_PORT}"; do
  if lsof -nP -iTCP:"${port}" -sTCP:LISTEN >/dev/null 2>&1; then fail "port ${port} is in use"; fi
done

log "work dir ${WORK}"
rsync -a "${REPO}/box/regtest/" "${WORK}/regtest/"

log "[1/6] zebrad regtest (${PROJECT}, :${ZEBRAD_PORT})"
(cd "${WORK}/regtest" && SOVA_BOX_ZEBRAD_CONTAINER="${CONTAINER}" SOVA_BOX_ZEBRAD_PORT="${ZEBRAD_PORT}" \
  docker compose -p "${PROJECT}" up -d) >"${WORK}/compose.log" 2>&1 || fail "compose up (see ${WORK}/compose.log)"
deadline=$((SECONDS + 120))
until rpc "${ZEBRAD_RPC}" getblockcount '[]' | grep -q result; do
  [[ ${SECONDS} -ge ${deadline} ]] && fail "zebrad not up"
  sleep 2
done

log "[2/6] miner identity, funding, node, auto-mine, miner"
"${SOVA_MINER_BIN}" --data-dir "${WORK}/miner" --network regtest init >"${WORK}/miner-init.log" 2>&1
TADDR="$(awk '/t-addr to fund/ {print $NF}' "${WORK}/miner-init.log")"
EVM_ADDR="$(awk '/evm address/ {print $NF}' "${WORK}/miner-init.log")"
[[ -n "${TADDR}" && -n "${EVM_ADDR}" ]] || fail "miner init"
rpc "${ZEBRAD_RPC}" generatetoaddress "[101,\"${TADDR}\"]" | grep -q result || fail "funding"
mkdir -p "${WORK}/node/tmp"
start_node() { # mode(debug|local|public) log
  local dbg=0 profile=local
  [[ "$1" == debug ]] && dbg=1
  [[ "$1" == public ]] && profile=public
  env TMPDIR="${WORK}/node/tmp" SOVA_DATADIR="${WORK}/node/data" NO_COLOR=1 \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" SOVA_EPOCH_BASE=1 \
    SOVA_HTTP_PORT="${RPC_PORT}" SOVA_AUTH_PORT="${AUTH_PORT}" SOVA_P2P_PORT="${P2P_PORT}" \
    SOVA_RPC_DEBUG="${dbg}" SOVA_RPC_PROFILE="${profile}" \
    "${SOVA_BIN}" >"$2" 2>&1 &
  NODE_PID=$!
  PIDS+=("${NODE_PID}")
}
wait_rpc() {
  local deadline=$((SECONDS + 90))
  until rpc "${SOVA_RPC}" eth_chainId '[]' | grep -q result; do
    [[ ${SECONDS} -ge ${deadline} ]] && fail "sova RPC not up (see ${WORK}/node*.log)"
    sleep 2
  done
}
start_node "${NODE_MODE}" "${WORK}/node.log"
"${WORK}/regtest/auto-mine.sh" 2 "${ZEBRAD_RPC}" >"${WORK}/auto-mine.log" 2>&1 &
PIDS+=($!)
wait_rpc
ZERO="0x$(printf '0%.0s' $(seq 1 64))"
case "${NODE_MODE}" in
  debug) grep -q "SOVA_RPC_DEBUG=1" "${WORK}/node.log" || fail "node did not enable debug" ;;
  local)
    rpc "${SOVA_RPC}" debug_getRawBlock '["0x0"]' | grep -q '"code":-32601' || fail "local node serves debug"
    log "node: local profile, no debug namespace (like the v0.1.17 keeper)" ;;
  public)
    rpc "${SOVA_RPC}" debug_getRawBlock '["0x0"]' | grep -q '"code":-32601' || fail "public node serves debug"
    rpc "${SOVA_RPC}" eth_getRawTransactionByBlockHashAndIndex "[\"${ZERO}\",\"0x0\"]" | grep -q '"code":-32601' \
      || fail "public node serves eth_getRawTransactionByBlockHashAndIndex"
    grep -q "rpc profile: public" "${WORK}/node.log" || fail "node not on the public profile"
    log "node: public profile (like the v0.1.17 seeds): no debug, no raw-tx methods" ;;
esac
"${SOVA_MINER_BIN}" --data-dir "${WORK}/miner" --network regtest mine \
  --budget-zat 50000000 --per-epoch-zat 100000 --rpc "${ZEBRAD_RPC}" >"${WORK}/miner.log" 2>&1 &
PIDS+=($!)
CHAIN_ID="$(rpc "${SOVA_RPC}" eth_chainId '[]' | python3 -c 'import json,sys; print(int(json.load(sys.stdin)["result"],16))')"
log "chain id ${CHAIN_ID}; waiting for ${BLOCKS} blocks"
wait_head "${BLOCKS}" 600

log "[2b] load: legacy, 2930, 1559, 7702, 4844 (best effort) txs + 8 with 100 KB of calldata"
DEV_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
DEV_ADDR="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
SINK="0x000000000000000000000000000000000000dEaD"
NONCE="$(cast nonce --rpc-url "${SOVA_RPC}" "${DEV_ADDR}")"
LAST_TX=""
for i in $(seq 0 19); do
  # Half legacy (an RLP list in the block body), half EIP-1559 (an RLP
  # string of type || payload): both paths of the eth_* rebuild.
  LEGACY=()
  [[ $((i % 2)) -eq 0 ]] && LEGACY=(--legacy)
  LAST_TX="$(cast send --async --nonce $((NONCE + i)) --gas-limit 21000 --value 1 ${LEGACY[@]+"${LEGACY[@]}"} \
    --private-key "${DEV_KEY}" --rpc-url "${SOVA_RPC}" "${SINK}")"
done
# EIP-2930 (type 1): legacy pricing plus an access list.
cast send --async --nonce $((NONCE + 20)) --legacy --gas-limit 30000 \
  --access-list "[{\"address\":\"${SINK}\",\"storageKeys\":[\"${ZERO}\"]}]" \
  --private-key "${DEV_KEY}" --rpc-url "${SOVA_RPC}" "${SINK}" >/dev/null
# EIP-7702 (type 4), self-sponsored from the second dev key; EIP-4844
# (type 3) from the third. Best effort: logged if the node refuses them.
KEY1="0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d"
KEY2="0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a"
if cast send --async --auth 0x000000000000000000000000000000000000bEEF --gas-limit 100000 \
  --private-key "${KEY1}" --rpc-url "${SOVA_RPC}" "${SINK}" >"${WORK}/tx-7702.log" 2>&1; then
  log "sent a type-4 (7702) tx"
else
  log "type-4 (7702) tx refused: $(tail -1 "${WORK}/tx-7702.log")"
fi
head -c 1000 /dev/urandom >"${WORK}/blob.bin"
if cast send --async --blob --path "${WORK}/blob.bin" --gas-limit 100000 \
  --private-key "${KEY2}" --rpc-url "${SOVA_RPC}" "${SINK}" >"${WORK}/tx-4844.log" 2>&1; then
  log "sent a type-3 (4844) tx"
else
  log "type-3 (4844) tx refused: $(tail -1 "${WORK}/tx-4844.log")"
fi
for i in $(seq 21 28); do
  # Random bytes: incompressible, and 4 MGas each (EIP-7623 floor), so
  # they span two blocks of several hundred KB.
  BIG="0x$(head -c 100000 /dev/urandom | xxd -p | tr -d '\n')"
  LAST_TX="$(cast send --async --nonce $((NONCE + i)) --gas-limit 4500000 \
    --private-key "${DEV_KEY}" --rpc-url "${SOVA_RPC}" "${SINK}" "${BIG}")"
done
deadline=$((SECONDS + 300))
until LAST_BLOCK="$(cast receipt --rpc-url "${SOVA_RPC}" "${LAST_TX}" blockNumber 2>/dev/null)" && [[ -n "${LAST_BLOCK}" ]]; do
  [[ ${SECONDS} -ge ${deadline} ]] && fail "load txs not mined (see ${WORK}/node.log)"
  sleep 3
done
log "load mined by block ${LAST_BLOCK}"
wait_head "$((LAST_BLOCK + 4))" 300

if [[ "${NODE_MODE}" == debug ]]; then
  "${NEAR_DA_BIN}" check-sources --sova-rpc "${SOVA_RPC}" | sed 's/^/[e2e]   /'
fi

log "[3/6] NEAR testnet account + contract"
ACCOUNT="e2e-$(date -u +%Y%m%d%H%M%S).${NEAR_DA_PARENT}"
KEY="${KEY_DIR}/${ACCOUNT}.json"
mkdir -p "${KEY_DIR}" && chmod 700 "${KEY_DIR}"
PUBKEY="$("${NEAR_DA_BIN}" keygen --account "${ACCOUNT}" --out "${KEY}")"
near account create-account fund-myself "${ACCOUNT}" "${NEAR_DA_E2E_FUND:-2.2 NEAR}" use-manually-provided-public-key "${PUBKEY}" \
  sign-as "${NEAR_DA_PARENT}" network-config testnet sign-with-access-key-file "${NEAR_DA_PARENT_KEY}" send \
  >"${WORK}/near-create.log" 2>&1 || fail "create ${ACCOUNT} (see ${WORK}/near-create.log)"
near contract deploy "${ACCOUNT}" use-file "${NEAR_DA_WASM}" with-init-call new \
  json-args "{\"owner\":\"${ACCOUNT}\",\"chain_id\":${CHAIN_ID},\"start_height\":0}" \
  prepaid-gas '30 Tgas' attached-deposit '0 NEAR' network-config testnet \
  sign-with-access-key-file "${KEY}" send >"${WORK}/near-deploy.log" 2>&1 \
  || fail "deploy (see ${WORK}/near-deploy.log)"
grep -E "Transaction ID" "${WORK}/near-create.log" "${WORK}/near-deploy.log" | sed 's/^/[e2e]   /' || true
log "contract ${ACCOUNT}"

DA=("${NEAR_DA_BIN}")
NEAR_ARGS=(--contract "${ACCOUNT}" --near-rpc "${NEAR_RPC}")
poster() {
  "${DA[@]}" poster "${NEAR_ARGS[@]}" --account "${ACCOUNT}" --key-file "${KEY}" \
    --sova-rpc "${SOVA_RPC}" --state-file "${WORK}/poster-state.json" \
    --status-file "${WORK}/poster-status.json" --depth 2 --max-blocks 16 --max-bytes 1000000 \
    --once "$@"
}

log "[4/6] poster, first run"
poster 2>&1 | tee -a "${WORK}/poster.log"
grep -q "${WANT_SOURCE}" "${WORK}/poster.log" || fail "poster did not log '${WANT_SOURCE}'"
log "poster used: ${WANT_SOURCE#raw blocks: }"
N1="$("${DA[@]}" info "${NEAR_ARGS[@]}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["info"]["next_height"])')"
[[ "${N1}" -ge "$((BLOCKS - 2))" ]] || fail "archive next_height ${N1} after the first run"
log "archived 0..$((N1 - 1))"

log "[5/6] more blocks, then a poster with NO state file (must resume from the contract)"
wait_head "$((N1 + 20))" 600
rm -f "${WORK}/poster-state.json"
poster 2>&1 | tee -a "${WORK}/poster.log"
N2="$("${DA[@]}" info "${NEAR_ARGS[@]}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["info"]["next_height"])')"
[[ "${N2}" -gt "${N1}" ]] || fail "no progress on the second run"
# A third run right away has nothing (or a little) to post and must not
# repeat anything: the contract would refuse a repeat, and the index stays
# contiguous (checked by fetch below).
poster 2>&1 | tee -a "${WORK}/poster.log"

log "[6/6] fetch from NEAR, verify, byte-compare with the node"
"${DA[@]}" info "${NEAR_ARGS[@]}" --batches >"${WORK}/index.json"
python3 - "${WORK}/index.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
bs, info = d["batches"], d["info"]
assert bs, "no batches"
nxt = info["start_height"]
for b in bs:
    assert b["first_height"] == nxt, f"gap/overlap at batch {b['index']}"
    assert b["tx_hash"], f"batch {b['index']} has no tx_hash"
    nxt = b["last_height"] + 1
assert nxt == info["next_height"]
big = max(bs, key=lambda b: b["bytes"])
assert big["bytes"] > 400_000, f"largest batch only {big['bytes']} bytes"
print(f"[e2e]   index: {len(bs)} batches, heights 0..{nxt-1}, all contiguous, all with tx_hash;"
      f" largest batch #{big['index']} {big['bytes']} bytes ({big['count']} blocks)")
PY
"${DA[@]}" fetch "${NEAR_ARGS[@]}" --out "${WORK}/fetched" --expect-chain-id "${CHAIN_ID}" \
  --sova-rpc "${SOVA_RPC}" -v 2>&1 | tee "${WORK}/fetch.log"
"${DA[@]}" verify "${WORK}/fetched" --expect-chain-id "${CHAIN_ID}" --start-height 0 \
  --sova-rpc "${SOVA_RPC}" | tee -a "${WORK}/fetch.log"

if [[ "${NODE_MODE}" != debug ]]; then
  log "restarting the node (local + SOVA_RPC_DEBUG=1) on the same datadir, to compare against debug_getRawBlock"
  kill -TERM "${NODE_PID}"
  deadline=$((SECONDS + 90))
  while kill -0 "${NODE_PID}" 2>/dev/null; do
    [[ ${SECONDS} -ge ${deadline} ]] && fail "node did not stop"
    sleep 1
  done
  start_node debug "${WORK}/node-debug.log"
  wait_rpc
  grep -q "SOVA_RPC_DEBUG=1" "${WORK}/node-debug.log" || fail "restarted node did not enable debug"
  "${DA[@]}" verify "${WORK}/fetched" --expect-chain-id "${CHAIN_ID}" --start-height 0 \
    --sova-rpc "${SOVA_RPC}" --raw-source debug | tee -a "${WORK}/fetch.log"
fi

# What the archive holds: transaction types and withdrawals (the miner's
# mints), per the node.
python3 - "${SOVA_RPC}" "$(ls "${WORK}/fetched"/*.sovada | tail -1 | sed -E 's/.*-0*([0-9]+)\.sovada/\1/')" <<'PY'
import json, sys, urllib.request, collections
url, last = sys.argv[1], int(sys.argv[2])
types, wd, blocks_wd = collections.Counter(), 0, 0
for h in range(0, last + 1):
    req = urllib.request.Request(url, json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber","params":[hex(h), True]}).encode(), {"content-type":"application/json"})
    b = json.load(urllib.request.urlopen(req))["result"]
    types.update(t["type"] for t in b["transactions"])
    n = len(b.get("withdrawals") or [])
    wd += n; blocks_wd += n > 0
print(f"[e2e]   archived tx types: {dict(sorted(types.items()))}; withdrawals (mints): {wd} in {blocks_wd} blocks")
for t in ("0x0", "0x1", "0x2"):
    assert types[t] > 0, f"no type {t} tx archived"
assert wd > 0, "no withdrawals archived"
PY

# Independent check (no shared code with the tool): concatenate every raw
# block from the files and compare with curl'd debug_getRawBlock.
python3 - "${WORK}/fetched" "${SOVA_RPC}" <<'PY'
import glob, json, struct, sys, urllib.request
d, url = sys.argv[1], sys.argv[2]
n = 0
for f in sorted(glob.glob(d + "/*.sovada")):
    b = open(f, "rb").read()
    assert b[:8] == b"SOVADA1\0"
    chain, first, count = struct.unpack_from("<QQI", b, 8)
    off = 28
    for i in range(count):
        h, = struct.unpack_from("<Q", b, off); off += 8
        off += 32
        ln, = struct.unpack_from("<I", b, off); off += 4
        raw = b[off:off+ln]; off += ln
        req = urllib.request.Request(url, json.dumps({"jsonrpc":"2.0","id":1,"method":"debug_getRawBlock","params":[hex(h)]}).encode(), {"content-type":"application/json"})
        node = bytes.fromhex(json.load(urllib.request.urlopen(req))["result"][2:])
        assert node == raw, f"block {h} differs"
        n += 1
    assert off == len(b)
print(f"[e2e]   independent byte-compare: {n} blocks identical to debug_getRawBlock")
PY

# Fetch again from the archival endpoint only (the path a stranger uses
# once regular nodes have pruned): every batch by tx hash.
"${DA[@]}" fetch --contract "${ACCOUNT}" --near-rpc https://archival-rpc.testnet.fastnear.com \
  --archival-rpc '' --block-api '' --out "${WORK}/fetched-archival" --expect-chain-id "${CHAIN_ID}" \
  | tee -a "${WORK}/fetch.log"
diff -r "${WORK}/fetched" "${WORK}/fetched-archival" -x index.json && log "archival fetch identical"

# Without the recorded tx hashes: find every batch from the index's
# near_block alone, via NEAR RPC block/chunk scans, then via neardata.xyz.
for via in rpc block-api; do
  "${DA[@]}" fetch "${NEAR_ARGS[@]}" --scan-only --scan-via "${via}" --out "${WORK}/fetched-scan-${via}" \
    --expect-chain-id "${CHAIN_ID}" | tee -a "${WORK}/fetch.log"
  diff -r "${WORK}/fetched" "${WORK}/fetched-scan-${via}" -x index.json && log "scan-only (${via}) fetch identical"
done
grep "posted batch" "${WORK}/poster.log" | python3 -c 'import re,sys; rows=[(int(m.group(1)),float(m.group(2))) for l in sys.stdin for m in [re.search(r"blocks, (\d+) bytes\) tx \S+ gas ([0-9.]+) Tgas",l)] if m]; b,g=max(rows); print(f"[e2e]   largest post: {b} bytes, {g} Tgas burnt")'

if [[ "${NEAR_DA_E2E_DELETE:-0}" == "1" ]]; then
  near account delete-account "${ACCOUNT}" beneficiary "${NEAR_DA_PARENT}" network-config testnet \
    sign-with-access-key-file "${KEY}" send >"${WORK}/near-delete.log" 2>&1 && log "deleted ${ACCOUNT}" \
    && rm -f "${KEY}"
fi
log "PASS (node ${NODE_MODE}, poster source: ${WANT_SOURCE#raw blocks: }): contract ${ACCOUNT}, chain ${CHAIN_ID}, archived 0..$((N2 - 1)) (and more if the third run posted)"
