#!/usr/bin/env bash
# The keeper restarts while its head's anchor is reorged away: it must
# re-seal that head on the new anchor, not build the next block on it.
#
# Regression test for the reorg-stress seed-202 failure of 2026-09-26
# (fixed in bb8b9de on release, first written as 24484db): node A, the
# keeper, was stopped at head N. While it was down, Zcash replaced block
# Z = N + B - 1 (N's anchor) with a block that carried no burn, and the
# chain moved on one more block. On restart A's sealer built N + 1 on the
# orphaned N within a second, BEFORE its expectations follower's first
# Zcash scan: the SIP-4 §7 gate of 32c7c65 only held the sealer once that
# follower had scanned something (`scanned_through().is_some()`). Every node
# followed A, and the network sat on a block anchored to a Zcash block
# zebrad no longer has. With the fix (`ExpectedSettlements::head_epoch_unknown`
# + `mark_enabled` before any task runs) the sealer waits for the scan,
# sees N is stale and re-seals N on the replacement first.
#
# The random stress only hit this on one seed; this scenario builds the
# shape directly, every time:
#   - N is a NULL block and the replacement at Z pays its coinbase to the
#     same transparent miner address as the block it replaces (the stress's
#     sapling=none). The pools then look the same on both branches, so a
#     block built on the stale N passes SIP-7's ZcashBlocks.record
#     continuity check and nothing but the anchor gives it away.
#   - One more Zcash block (Z + 1) is mined while A is still down, so A has
#     epoch N + 1 to build the moment its sealer first polls. Without it
#     there is nothing to build until after the first scan and the race
#     never opens.
#   - Zcash is then frozen until the chain has converged, so "converged"
#     is a fixed target.
#
# Topology (the testnet's, as in reorg-stress-scenario.sh):
#   node A -- keeper: mine mode, SOVA_SIP6=1 + SOVA_SIP7=1, sealing key =
#             a `sova-miner init` keystore, persistent datadir.
#   node B -- seed: follow-only, C5-enforcing, SIP-6/7, static peer = A.
#   node C -- rpc: follow-only, SOVA_RPC_PROFILE=public, SIP-6/7, static
#             peer = B only.
# All three share one regtest zebrad; the script mines Zcash itself, one
# block at a time, waiting for the nodes after each.
#
# Flow:
#   0. Setup: fund the miner, start A, B, C; sova/1 sessions A<->B, B<->C.
#   1. Warm-up: a burner (sova-miner) runs for RESTART_REORG_BURNS epochs so
#      the chain holds SEALED blocks; then it stops and
#      RESTART_REORG_NULL_BLOCKS null epochs follow (mempool empty).
#      A, B and C sit at N = zcash_tip - B + 1, a NULL block anchored to Z.
#   2. SIGTERM A (graceful: its chain is on disk). invalidateblock(Z), mine
#      a replacement Z' (transparent coinbase, no burn), mine Z + 1, start A
#      again on the same datadir at once.
#   3. Within RESTART_REORG_TIMEOUT_S (180):
#        (a) every node's head == the Zcash-expected height N + 1;
#        (b) every node's block N is anchored to Z' (zebrad's CURRENT block
#            Z), not the orphaned Z;
#        (c) A, B and C hold the same hash at every height 1..head, and
#            every canonical anchor == zebrad's getblockhash(n + B - 1).
#      On a timeout the script says whether A built N + 1 on the orphaned N
#      (the pre-fix signature).
#   4. Liveness: RESTART_REORG_ADVANCE (3) more Zcash blocks; all three
#      nodes follow, and (c) holds over the whole chain again.
#
# Env: RESTART_REORG_TIMEOUT_S (180), RESTART_REORG_BURNS (3),
# RESTART_REORG_NULL_BLOCKS (4), RESTART_REORG_ADVANCE (3),
# RESTART_REORG_A_RUST_LOG (info,engine::miner=debug,engine::driver=debug),
# plus p2p-common.sh's SOVA_P2P_SIM_*, SOVA_BIN, SOVA_MINER_BIN,
# SOVA_P2P_SIM_KEEP_LOGS.
#
# Isolation: zebrad on :18472 (compose project sova-restart-reorg-sim,
# container sova-zebrad-restart-reorg), A on 11045/11051/31211, B on
# 11145/11151/31212, C on 11245/11251/31213. All overridable.

SCENARIO="restart-reorg"
WORK_PREFIX="sova-restart-reorg"
P2P_THREE_NODES=1
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18472}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-restart-reorg}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-restart-reorg-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=11045 11051 31211}"
: "${SOVA_P2P_SIM_B_PORTS:=11145 11151 31212}"
: "${SOVA_P2P_SIM_C_PORTS:=11245 11251 31213}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS SOVA_P2P_SIM_C_PORTS
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"
trap cleanup EXIT

TIMEOUT_S="${RESTART_REORG_TIMEOUT_S:-180}"
BURNS="${RESTART_REORG_BURNS:-3}"
NULL_BLOCKS="${RESTART_REORG_NULL_BLOCKS:-4}"
ADVANCE="${RESTART_REORG_ADVANCE:-3}"
A_RUST_LOG="${RESTART_REORG_A_RUST_LOG:-info,engine::miner=debug,engine::driver=debug}"
FUND_BLOCKS=101
PER_EPOCH_ZAT=100000

A_DATADIR=""
A_GEN=0
A_LOG=""
BURNER_PID=""
T0=${SECONDS}

# --- helpers ----------------------------------------------------------------

now_f() { perl -MTime::HiRes=time -e 'printf("%.3f\n", time)'; }
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

# "hash parentHash extraDataBytes parentBeaconBlockRoot" of block $2 on $1
# (lowercase); empty when the node has no block there.
block_info() {
  eth_rpc "$1" eth_getBlockByNumber "[\"$(printf '0x%x' "$2")\", false]" | python3 -c "
import sys, json
try:
    b = json.load(sys.stdin).get('result')
except Exception:
    b = None
if b:
    print(b['hash'].lower(), b['parentHash'].lower(), (len(b['extraData']) - 2) // 2,
          (b.get('parentBeaconBlockRoot') or '-').lower())
"
}

start_node_a() {
  A_LOG="${WORK_DIR}/node-a-${A_GEN}.log"
  p2p_env \
    RUST_LOG="${A_RUST_LOG}" \
    SOVA_SIP6=1 \
    SOVA_SIP7=1 \
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
  A_GEN=$((A_GEN + 1))
  wait_for_eth_rpc "${ENGINE_RPC_A}" "${A_PID}" "node A"
}

start_node_b() {
  p2p_env \
    SOVA_SIP6=1 \
    SOVA_SIP7=1 \
    SOVA_FOLLOW_ONLY=1 \
    SOVA_DATADIR="${WORK_DIR}/datadir-b" \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
    SOVA_EPOCH_BASE="${EPOCH_BASE}" \
    SOVA_P2P_PEERS="${ENODE_A}" \
    SOVA_HTTP_PORT="${B_HTTP_PORT}" \
    SOVA_AUTH_PORT="${B_AUTH_PORT}" \
    SOVA_P2P_PORT="${B_P2P_PORT}" \
    "${SOVA_BIN}" >"${WORK_DIR}/node-b.log" 2>&1 &
  B_PID=$!
  wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B"
}

start_node_c() {
  p2p_env \
    SOVA_SIP6=1 \
    SOVA_SIP7=1 \
    SOVA_FOLLOW_ONLY=1 \
    SOVA_RPC_PROFILE=public \
    SOVA_DATADIR="${WORK_DIR}/datadir-c" \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
    SOVA_EPOCH_BASE="${EPOCH_BASE}" \
    SOVA_P2P_PEERS="${ENODE_B}" \
    SOVA_HTTP_PORT="${C_HTTP_PORT}" \
    SOVA_AUTH_PORT="${C_AUTH_PORT}" \
    SOVA_P2P_PORT="${C_P2P_PORT}" \
    "${SOVA_BIN}" >"${WORK_DIR}/node-c.log" 2>&1 &
  C_PID=$!
  wait_for_eth_rpc "${ENGINE_RPC_C}" "${C_PID}" "node C"
}

# SIGTERM node A (bin/sova's graceful path writes its in-memory blocks) and
# wait; SIGKILL after 90 s.
stop_node_a() {
  local pid="${A_PID}" deadline=$((SECONDS + 90))
  kill -TERM "${pid}" 2>/dev/null || true
  while kill -0 "${pid}" 2>/dev/null; do
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "node A (pid ${pid}) still running 90s after SIGTERM; killing it" >&2
      kill -KILL "${pid}" 2>/dev/null || true
      break
    fi
    sleep 0.2
  done
  wait "${pid}" 2>/dev/null || true
  A_PID=""
}

burner_running() { [[ -n "${BURNER_PID}" ]] && kill -0 "${BURNER_PID}" 2>/dev/null; }

stop_burner() {
  if burner_running; then
    kill "${BURNER_PID}" 2>/dev/null || true
    wait "${BURNER_PID}" 2>/dev/null || true
  fi
  BURNER_PID=""
}

# Wait until A, B and C all sit at the Zcash tip's Sova height; prints it
# (or the heads on a timeout).
settle_all() { # <timeout_s>
  local deadline=$((SECONDS + $1)) want ha hb hc
  while :; do
    want=$(($(zc_tip_height) - EPOCH_BASE + 1))
    ha="$(height_of "${ENGINE_RPC_A}")"
    hb="$(height_of "${ENGINE_RPC_B}")"
    hc="$(height_of "${ENGINE_RPC_C}")"
    if [[ "${ha}" -eq "${want}" && "${hb}" -eq "${want}" && "${hc}" -eq "${want}" ]]; then
      echo "${want}"
      return 0
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "A=${ha} B=${hb} C=${hc} want=${want}"
      return 1
    fi
    sleep 0.5
  done
}

# Mine one Zcash block and wait for all three nodes to build/import it.
step() {
  zc_rpc generate "[1]" >/dev/null
  settle_all 60 >/dev/null
}

# One-shot convergence check. Prints a status line; rc 0 when every node's
# head is the Zcash-expected height, A, B and C agree on every hash 1..head,
# every anchor is zebrad's getblockhash(n + B - 1), and (with $1) block $1
# is anchored to 0x$2 on every node.
converged() { # [height] [want_anchor_hex]
  python3 - "${EPOCH_BASE}" "${ZEBRAD_RPC}" "${ENGINE_RPC_A}" "${ENGINE_RPC_B}" "${ENGINE_RPC_C}" \
    "${1:-0}" "${2:-}" <<'PY'
import json, sys, urllib.request
base, zurl = int(sys.argv[1]), sys.argv[2] + "/"
nodes = [("A", sys.argv[3]), ("B", sys.argv[4]), ("C", sys.argv[5])]
probe, want_anchor = int(sys.argv[6]), ("0x" + sys.argv[7]) if sys.argv[7] else ""
def rpc(u, m, p):
    req = urllib.request.Request(u, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return json.load(r).get("result")
    except Exception:
        return None
zt = rpc(zurl, "getblockcount", [])
if zt is None:
    print("zebrad unreadable"); sys.exit(2)
expected = zt - base + 1
heads = {}
for n, u in nodes:
    r = rpc(u, "eth_blockNumber", [])
    heads[n] = int(r, 16) if r else None
problems = []
if any(h != expected for h in heads.values()):
    problems.append("heads " + " ".join(f"{n}={h}" for n, h in heads.items()) + f" vs expected {expected}")
top = min([h for h in heads.values() if h is not None] or [0])
zh = {}
disagree, bad_anchor, probe_anchors = [], [], {}
for height in range(1, top + 1):
    e = height + base - 1
    z = rpc(zurl, "getblockhash", [e])
    zh[height] = ("0x" + z.lower()) if z else None
    seen = {}
    for n, u in nodes:
        b = rpc(u, "eth_getBlockByNumber", [hex(height), False])
        seen[n] = (b["hash"].lower(), (b.get("parentBeaconBlockRoot") or "-").lower()) if b else (None, None)
        if seen[n][1] != zh[height]:
            bad_anchor.append(f"{n}@{height}")
        if height == probe:
            probe_anchors[n] = seen[n][1]
    if len({v[0] for v in seen.values()}) != 1 or None in {v[0] for v in seen.values()}:
        disagree.append(f"{height}:" + ",".join(f"{n}={(v[0] or '-')[:10]}" for n, v in seen.items()))
if disagree:
    problems.append("hashes differ at " + " ".join(disagree[:10]))
if bad_anchor:
    problems.append("anchored to Zcash blocks zebrad no longer has: " + " ".join(bad_anchor[:15]))
if probe and want_anchor:
    wrong = {n: a for n, a in probe_anchors.items() if a != want_anchor}
    if wrong or len(probe_anchors) != 3:
        problems.append(f"block {probe} anchors " + " ".join(f"{n}={(a or '-')[:14]}" for n, a in probe_anchors.items())
                        + f" (want {want_anchor[:14]}, the replacement)")
status = f"zcash tip {zt} expected {expected} heads " + " ".join(f"{n}={h}" for n, h in heads.items())
if problems:
    print(status + " | " + " | ".join(problems)); sys.exit(1)
print(status + " | agree on 1.." + str(top) + ", every anchor is zebrad's"); sys.exit(0)
PY
}

# One line per height 1..head on node $1: "n hash parent anchor extraLen zebrad@n+B-1".
dump_chain() {
  python3 - "$1" "$(height_of "$1")" "${EPOCH_BASE}" "${ZEBRAD_RPC}" <<'PY'
import sys, json, urllib.request
url, hi, base, zurl = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
def rpc(u, m, p):
    req = urllib.request.Request(u, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return json.load(r).get("result")
    except Exception:
        return None
print("# n hash parent anchor extraLen zebrad@n+B-1")
for n in range(1, hi + 1):
    zh = rpc(zurl + "/", "getblockhash", [n + base - 1])
    zh = ("0x" + zh.lower()) if zh else "-"
    b = rpc(url, "eth_getBlockByNumber", [hex(n), False])
    if not b:
        print(n, "-", "-", "-", "-", zh)
        continue
    print(n, b["hash"].lower()[:18], b["parentHash"].lower()[:18], (b.get("parentBeaconBlockRoot") or "-").lower()[:18],
          (len(b["extraData"]) - 2) // 2, zh[:18])
PY
}

count_in() {
  local n
  n="$(sed 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | grep -cF -- "$2" || true)"
  echo "${n:-0}"
}

# =============================================================================
# (0) setup
# =============================================================================

for tool in python3 perl; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "error: this scenario needs \`${tool}\` on PATH" >&2
    exit 1
  fi
done

preflight
start_stack

MINER_DATA_DIR="${WORK_DIR}/miner"
A_DATADIR="${WORK_DIR}/datadir-a"
mkdir -p "${MINER_DATA_DIR}" "${A_DATADIR}" "${WORK_DIR}/datadir-b" "${WORK_DIR}/datadir-c"
"${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest init >"${WORK_DIR}/miner-init.log" 2>&1
TADDR="$(awk '/t-addr to fund/ {print $NF}' "${WORK_DIR}/miner-init.log")"
EVM_ADDR="$(awk '/evm address/ {print $NF}' "${WORK_DIR}/miner-init.log" | head -1)"
if [[ -z "${TADDR}" || -z "${EVM_ADDR}" || ! -f "${MINER_DATA_DIR}/keystore.json" ]]; then
  fail "setup: could not parse the miner identity / find its keystore"
  cat "${WORK_DIR}/miner-init.log" >&2
  exit 1
fi
echo "miner identity: ${TADDR} / ${EVM_ADDR}"
zc_generate_to_address "${FUND_BLOCKS}" "${TADDR}"
EPOCH_BASE=$(($(zc_tip_height) + 1))
echo "epoch base B=${EPOCH_BASE} (Sova block N anchors Zcash N+${EPOCH_BASE}-1)"

echo "--- starting node A (keeper: mine mode, SIP-6 + SIP-7, sealing keystore, persistent datadir) ---"
start_node_a || exit 1
ENODE_A="$(local_enode "${A_LOG}")" || {
  fail "setup: node A never printed its enode"
  exit 1
}
echo "--- starting node B (seed: follow-only, static peer A) ---"
start_node_b || exit 1
ENODE_B="$(local_enode "${WORK_DIR}/node-b.log")" || {
  fail "setup: node B never printed its enode"
  exit 1
}
echo "--- starting node C (rpc: follow-only, public RPC profile, static peer B only) ---"
start_node_c || exit 1

echo ""
echo "=== (0) setup ==="
check_p2p_node_log "node A" "${A_LOG}"
check_p2p_node_log "node B" "${WORK_DIR}/node-b.log"
check_p2p_node_log "node C" "${WORK_DIR}/node-c.log"
if log_has "${A_LOG}" "sip-6: sealing as" 15; then
  pass "setup: node A seals (SIP-6)"
else
  fail "setup: node A's log lacks 'sip-6: sealing as'"
fi
if wait_for_sova_peer_id "${A_LOG}" "$(enode_id "${ENODE_B}")" 60 \
  && wait_for_sova_peer "${WORK_DIR}/node-c.log" 60; then
  pass "setup: sova/1 sessions A<->B and B<->C established"
else
  fail "setup: sova/1 sessions not established within 60s"
  exit 1
fi
zc_rpc generate "[1]" >/dev/null
if ! H="$(settle_all 90)"; then
  fail "setup: chain did not start (${H})"
  exit 1
fi
[[ "${FAILURES}" -eq 0 ]] || exit 1

# =============================================================================
# (1) warm-up: sealed blocks, then null ones
# =============================================================================

echo ""
echo "=== (1) warm-up: ${BURNS} burn epochs, then ${NULL_BLOCKS} null epochs ==="
"${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
  --budget-zat $(((BURNS + 2) * (PER_EPOCH_ZAT + 100000))) --per-epoch-zat "${PER_EPOCH_ZAT}" \
  --rpc "${ZEBRAD_RPC}" --max-epochs "${BURNS}" --poll-interval-ms 500 >"${WORK_DIR}/burner.log" 2>&1 &
BURNER_PID=$!
for _ in $(seq 1 $((BURNS * 3 + 4))); do
  burner_running || break
  # Give the burner a moment to put its burn in the mempool, then mine it.
  deadline=$((SECONDS + 6))
  while [[ ${SECONDS} -lt ${deadline} && "$(zc_mempool_size)" -eq 0 ]] && burner_running; do sleep 0.3; done
  step
done
stop_burner
# Flush anything left in the mempool, then null epochs until the mempool
# stays empty and the tip is a null block.
for _ in $(seq 1 "${NULL_BLOCKS}"); do step; done
for _ in 1 2 3 4 5; do
  [[ "$(zc_mempool_size)" -eq 0 ]] && break
  step
done
if ! N="$(settle_all 60)"; then
  fail "(1) warm-up: A, B and C did not settle at the Zcash tip's height (${N})"
  exit 1
fi
SEALED=0
for ((n = 1; n <= N; n++)); do
  read -r _ _ x _ <<<"$(block_info "${ENGINE_RPC_A}" "${n}")"
  [[ "${x}" == "97" ]] && SEALED=$((SEALED + 1))
done
if [[ "${SEALED}" -ge 1 ]]; then
  pass "(1) warm-up: ${SEALED} SEALED (97-byte extraData) block(s) in 1..${N}"
else
  fail "(1) warm-up: no sealed block in 1..${N} (burner log: $(tail -n 3 "${WORK_DIR}/burner.log" | tr '\n' ' '))"
fi
Z=$((N + EPOCH_BASE - 1))
OLD_Z="$(zc_hash_at "${Z}")"
read -r STALE_HASH STALE_PARENT STALE_EXTRA STALE_ANCHOR <<<"$(block_info "${ENGINE_RPC_A}" "${N}")"
read -r B_N _ <<<"$(block_info "${ENGINE_RPC_B}" "${N}")"
read -r C_N _ <<<"$(block_info "${ENGINE_RPC_C}" "${N}")"
if [[ "${STALE_EXTRA}" != "0" || "${STALE_ANCHOR}" != "0x${OLD_Z}" || "${B_N}" != "${STALE_HASH}" \
  || "${C_N}" != "${STALE_HASH}" || "$(zc_mempool_size)" -ne 0 ]]; then
  fail "(1) warm-up: head ${N}: A ${STALE_HASH} (extraData ${STALE_EXTRA}, anchor ${STALE_ANCHOR}), B ${B_N}, C ${C_N}, zebrad@${Z} 0x${OLD_Z}, mempool $(zc_mempool_size); want the same NULL block on all three, anchored to zebrad's ${Z}"
  exit 1
fi
pass "(1) warm-up: A, B and C at head N=${N}, a NULL block ${STALE_HASH:0:18}.. anchored to Zcash Z=${Z} 0x${OLD_Z:0:16}.."
[[ "${FAILURES}" -eq 0 ]] || exit 1

# =============================================================================
# (2) keeper down; Zcash reorgs Z (no burn) and moves one block; restart
# =============================================================================

echo ""
echo "=== (2) SIGTERM A at head ${N}; reorg Zcash ${Z} (no burn) and mine $((Z + 1)); restart A ==="
T_STOP=$(now_f)
stop_node_a
T_DOWN=$(now_f)
zc_rpc invalidateblock "[\"${OLD_Z}\"]" >"${WORK_DIR}/invalidate.json"
if [[ "$(zc_tip_height)" -ne $((Z - 1)) ]]; then
  fail "(2) setup: zebrad tip $(zc_tip_height) after invalidateblock(${Z}), want $((Z - 1)) ($(cat "${WORK_DIR}/invalidate.json"))"
  exit 1
fi
# Transparent coinbase to zebrad's miner address, exactly like the block it
# replaces: the SIP-7 pools don't tell the branches apart (sapling=none).
zc_rpc generate "[1]" >/dev/null
NEW_Z="$(zc_hash_at "${Z}")"
zc_rpc generate "[1]" >/dev/null
if [[ -z "${NEW_Z}" || "${NEW_Z}" == "${OLD_Z}" || "$(zc_tip_height)" -ne $((Z + 1)) ]]; then
  fail "(2) setup: no replacement at ${Z} (hash ${NEW_Z:-<none>}, tip $(zc_tip_height))"
  exit 1
fi
echo "Zcash ${Z}: 0x${OLD_Z:0:16}.. -> 0x${NEW_Z:0:16}.. (transparent coinbase, no burn); tip now $((Z + 1)) (Sova $((N + 1)))"
MARK_B="$(wc -l <"${WORK_DIR}/node-b.log" | tr -d ' ')"
T_START=$(now_f)
start_node_a || {
  fail "(2) node A did not come back after the restart"
  exit 1
}
T_UP=$(now_f)
read -r RA _ <<<"$(block_info "${ENGINE_RPC_A}" "${N}")"
ON_STALE="$([[ "${RA}" == "${STALE_HASH}" ]] && echo yes || echo "no (${RA:-<none>})")"
echo "node A down $(python3 -c "print(f'{${T_START} - ${T_DOWN}:.1f}')") s (stop took $(python3 -c "print(f'{${T_DOWN} - ${T_STOP}:.1f}')") s); back (pid ${A_PID}, RPC up $(python3 -c "print(f'{${T_UP} - ${T_START}:.1f}')") s after exec), head $(height_of "${ENGINE_RPC_A}"); block ${N} still the orphaned-anchor one: ${ON_STALE}"

# =============================================================================
# (3) convergence on the CURRENT Zcash chain
# =============================================================================

echo ""
echo "=== (3) converge within ${TIMEOUT_S}s: heads = $((N + 1)), block ${N} anchored to 0x${NEW_Z:0:16}.., A = B = C ==="
DEADLINE=$((SECONDS + TIMEOUT_S))
OK=0
LAST=""
while :; do
  if LAST="$(converged "${N}" "${NEW_Z}" 2>&1)"; then
    OK=1
    break
  fi
  [[ ${SECONDS} -ge ${DEADLINE} ]] && break
  sleep 1
done
T_CONV=$(now_f)
CONV_S="$(python3 -c "print(f'{${T_CONV} - ${T_START}:.1f}')")"
echo "  ${LAST}"

# Diagnostics: what A did right after the restart.
read -r A_N A_N_PARENT _ A_N_ANCHOR <<<"$(block_info "${ENGINE_RPC_A}" "${N}")"
read -r A_N1 A_N1_PARENT _ A_N1_ANCHOR <<<"$(block_info "${ENGINE_RPC_A}" "$((N + 1))")"
FIRST_TRIGGER="$(strip_ansi "${A_LOG}" | grep -E 'sova epoch (re)?trigger' | head -1 | cut -c1-200)"
RESEALS="$(count_in "${A_LOG}" 're-sealing')"
WAITS="$(count_in "${A_LOG}" "waiting for our Zcash scan to record the head's epoch")"
echo "  A's first trigger after the restart: ${FIRST_TRIGGER:-<none>}"
echo "  A: 're-sealing' lines ${RESEALS}; 'waiting for our Zcash scan' lines ${WAITS}"
if [[ -n "${A_N1}" && "${A_N1_PARENT}" == "${STALE_HASH}" ]]; then
  echo "  A's block $((N + 1)) ${A_N1:0:18}.. is built ON THE ORPHANED ${N} ${STALE_HASH:0:18}.. (the pre-fix bug: sealer built before its first Zcash scan)"
fi

if [[ "${OK}" == "1" ]]; then
  pass "(3a) every node's head is the Zcash-expected $((N + 1)), ${CONV_S} s after A was restarted"
  pass "(3b) block ${N} on A, B and C is anchored to the CURRENT Zcash ${Z} 0x${NEW_Z:0:16}.. (was ${STALE_HASH:0:18}.. on the orphaned 0x${OLD_Z:0:16}..; now ${A_N:0:18}..)"
  if [[ "${A_N}" == "${STALE_HASH}" || "${A_N_PARENT}" != "${STALE_PARENT}" ]]; then
    fail "(3b) block ${N} ${A_N}: want a re-seal of the stale ${STALE_HASH} on its parent ${STALE_PARENT}, got parent ${A_N_PARENT}"
  else
    pass "(3b) block ${N} was re-sealed on its parent ${STALE_PARENT:0:18}.."
  fi
  pass "(3c) A, B and C agree on every hash 1..$((N + 1)); every canonical anchor is zebrad's"
else
  fail "(3) no convergence on the current Zcash chain within ${TIMEOUT_S}s of A's restart: ${LAST}"
  echo "  A@${N}: ${A_N:-<none>} anchor ${A_N_ANCHOR:-<none>}; A@$((N + 1)): ${A_N1:-<none>} parent ${A_N1_PARENT:-<none>} anchor ${A_N1_ANCHOR:-<none>}"
fi

# =============================================================================
# (4) liveness
# =============================================================================

if [[ "${OK}" == "1" ]]; then
  echo ""
  echo "=== (4) liveness: ${ADVANCE} more Zcash blocks ==="
  for _ in $(seq 1 "${ADVANCE}"); do step; done
  if H="$(settle_all 60)" && OUT="$(converged)"; then
    pass "(4) A, B and C followed to ${H}: ${OUT}"
  else
    fail "(4) after ${ADVANCE} more Zcash blocks: ${H}; $(converged 2>&1)"
  fi
fi

for pair in "a ${ENGINE_RPC_A}" "b ${ENGINE_RPC_B}" "c ${ENGINE_RPC_C}"; do
  read -r name url <<<"${pair}"
  dump_chain "${url}" >"${WORK_DIR}/chain-${name}.txt" 2>&1 || true
done
if [[ "${FAILURES}" -gt 0 ]]; then
  echo "--- chain on A (n hash parent anchor extraLen zebrad) ---" >&2
  tail -n 12 "${WORK_DIR}/chain-a.txt" >&2 || true
  echo "--- chain on C ---" >&2
  tail -n 6 "${WORK_DIR}/chain-c.txt" >&2 || true
fi

echo ""
echo "=== diagnostics ==="
echo "  epoch base ${EPOCH_BASE}; N=${N} Z=${Z}; restart->converged ${CONV_S} s; total $((SECONDS - T0)) s"
for log in "${WORK_DIR}"/node-*.log; do
  echo "  ${log##*/}: 're-sealing' $(count_in "${log}" 're-sealing'); 'zcash reorg observed' $(count_in "${log}" 'zcash reorg observed');" \
    "'expectations and candidates unwound' $(count_in "${log}" 'expectations and candidates unwound'); 'reputation hit' $(count_in "${log}" 'reputation hit')"
done
echo "  node-b.log since A's restart: $(tail -n +"$((MARK_B + 1))" "${WORK_DIR}/node-b.log" | sed 's/\x1b\[[0-9;]*m//g' | grep -c 'arbiter adopted' || true) 'arbiter adopted' line(s)"

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "RESTART REORG SCENARIO PASSED (restart->converged ${CONV_S} s)"
else
  echo "RESTART REORG SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi
[[ "${FAILURES}" -eq 0 ]]
