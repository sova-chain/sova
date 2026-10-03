#!/usr/bin/env bash
# The keeper restarts on a LONG chain: it must seal again within a budget,
# and its RPC must answer while it comes back.
#
# Acceptance test for the fast-restart fix (docs/design/fast-restart.md).
# The testnet outage of 2026-10-03: on every start the expectations
# follower (`run_expectations`) rescans every Zcash block from the epoch
# base to the tip in ONE synchronous `Follower::poll()` and records nothing
# until it returns; the sealer waits for the head's epoch
# (`head_epoch_unknown`), and then its own follower (`SealerCore`) rescans
# the same range again from the base. ~63k epochs took ~11.5 min with the
# node's RPC timing out. The cost grows with every block the chain makes.
#
# Topology: one node, the keeper (mine mode, SOVA_SIP6=1 + SOVA_SIP7=1,
# sealing keystore from `sova-miner init`, persistent datadir), on a
# regtest zebrad of its own. No peers: this is about one producer coming
# back, not about propagation.
#
# Flow:
#   0. Setup: zebrad, keystore, epoch base B = tip + 1, start A.
#   1. Grow: mine RESTART_LONG_EPOCHS Zcash blocks in chunks of
#      RESTART_LONG_CHUNK; after each chunk wait for A to seal up to the
#      tip. Every block is a null block (no burns): the restart cost is the
#      number of epochs, not their contents.
#   2. Snapshot A's SIP-4/SIP-7 precompile answers (blockAt, txInfo of the
#      coinbase, blockStats) for sampled Zcash heights from B to the tip,
#      at A's head, and check them against zebrad.
#   3. SIGTERM A (graceful), mine ONE more Zcash block (so there is an epoch
#      to seal the moment A is back), start A on the same datadir. A prober
#      calls eth_blockNumber every RESTART_LONG_PROBE_S with a
#      RESTART_LONG_RPC_TIMEOUT_S timeout from the exec on.
#   4. Assert, timed from the exec:
#        (a) A seals the new epoch (head = N + 1) within RESTART_LONG_BUDGET_S;
#        (b) A's RPC answers within RESTART_LONG_RPC_UP_S of the exec, and
#            every probe after the first answer returns within the timeout
#            until A has sealed (no RPC outage during the restart);
#        (c) A's precompile answers at the snapshot block equal the
#            pre-restart snapshot (a resumed / persisted index must answer
#            exactly as the rescanned one did: SIP-4/SIP-7 consensus);
#        (d) A fetched at most RESTART_LONG_MAX_BLOCK_FETCHES Zcash blocks
#            (`getblock`, counted by the proxy) to come back: restart cost
#            must not grow with the chain. Without (d) a faster rescan could
#            pass (a) at 3,000 epochs and still take minutes at 250k.
#      The script keeps waiting up to RESTART_LONG_MAX_WAIT_S after a missed
#      budget so a failing run still reports the real restart time.
#   5. Liveness: RESTART_LONG_ADVANCE more Zcash blocks are sealed.
#
# Env: RESTART_LONG_EPOCHS (3000), RESTART_LONG_CHUNK (250),
# RESTART_LONG_BUDGET_S (60), RESTART_LONG_MAX_WAIT_S (900),
# RESTART_LONG_RPC_UP_S (30), RESTART_LONG_RPC_TIMEOUT_S (5),
# RESTART_LONG_PROBE_S (1), RESTART_LONG_ADVANCE (3),
# RESTART_LONG_TOKIO_WORKERS (1; empty = all cores),
# RESTART_LONG_ZEBRAD_DELAY_MS (4: per-request latency the counting proxy in
# front of zebrad adds, so each block costs ~25 ms and 3,000 epochs rescan
# like ~14k testnet ones), RESTART_LONG_MAX_BLOCK_FETCHES (1000),
# RESTART_LONG_PROXY_PORT (18483), RESTART_LONG_A_RUST_LOG
# (info,engine::driver=debug),
# plus p2p-common.sh's SOVA_P2P_SIM_*, SOVA_BIN, SOVA_MINER_BIN,
# SOVA_P2P_SIM_KEEP_LOGS.
#
# Isolation: zebrad on :18482 (compose project sova-restart-long-sim,
# container sova-zebrad-restart-long), A on 11545/11551/31231; the B ports
# (11645/11651/31232) are only checked free, never used; the counting proxy
# on :18483. All overridable.

SCENARIO="restart-long-chain"
WORK_PREFIX="sova-restart-long"
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18482}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-restart-long}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-restart-long-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=11545 11551 31231}"
: "${SOVA_P2P_SIM_B_PORTS:=11645 11651 31232}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"

EPOCHS="${RESTART_LONG_EPOCHS:-3000}"
CHUNK="${RESTART_LONG_CHUNK:-250}"
BUDGET_S="${RESTART_LONG_BUDGET_S:-60}"
MAX_WAIT_S="${RESTART_LONG_MAX_WAIT_S:-900}"
RPC_UP_S="${RESTART_LONG_RPC_UP_S:-30}"
RPC_TIMEOUT_S="${RESTART_LONG_RPC_TIMEOUT_S:-5}"
PROBE_S="${RESTART_LONG_PROBE_S:-1}"
ADVANCE="${RESTART_LONG_ADVANCE:-3}"
DELAY_MS="${RESTART_LONG_ZEBRAD_DELAY_MS:-4}"
A_RUST_LOG="${RESTART_LONG_A_RUST_LOG:-info,engine::driver=debug}"
PROXY_PORT="${RESTART_LONG_PROXY_PORT:-18483}"
# A restart may refetch the sealer's ~600-block reach, verification
# spot-checks and the blocks mined while it was down, never history.
MAX_FETCHES="${RESTART_LONG_MAX_BLOCK_FETCHES:-1000}"
# The testnet keeper is a Hetzner CX23 (2 vCPU, zebrad on the same box) and
# its RPC timed out for the whole rescan. On the laptop, 2 tokio workers
# keep RPC up while one is blocked by the scan; 1 worker reproduces the
# keeper's outage (RPC dead, sealer task never scheduled, logs quiet), so it
# is the default. tokio reads TOKIO_WORKER_THREADS; empty = all cores.
TOKIO_WORKERS="${RESTART_LONG_TOKIO_WORKERS-1}"

A_DATADIR=""
A_GEN=0
A_LOG=""
PROBER_PID=""
PROXY_PID=""
NODE_ZEBRAD_RPC=""
T0=${SECONDS}

long_cleanup() {
  local rc=$?
  for pid in "${PROBER_PID}" "${PROXY_PID}"; do
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      kill "${pid}" 2>/dev/null || true
      wait "${pid}" 2>/dev/null || true
    fi
  done
  (exit "${rc}")
  cleanup
}
trap long_cleanup EXIT

# --- helpers ----------------------------------------------------------------

now_f() { perl -MTime::HiRes=time -e 'printf("%.3f\n", time)'; }
since() { python3 -c "print(f'{$(now_f) - $1:.1f}')"; }
height_of() { eth_block_number "$1" 2>/dev/null || echo 0; }

start_node_a() {
  A_LOG="${WORK_DIR}/node-a-${A_GEN}.log"
  p2p_env \
    ${TOKIO_WORKERS:+TOKIO_WORKER_THREADS="${TOKIO_WORKERS}"} \
    RUST_LOG="${A_RUST_LOG}" \
    SOVA_SIP6=1 \
    SOVA_SIP7=1 \
    SOVA_DATADIR="${A_DATADIR}" \
    SOVA_ZEBRAD_RPC="${NODE_ZEBRAD_RPC}" \
    SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" \
    SOVA_SEALER_KEYSTORE="${MINER_DATA_DIR}/keystore.json" \
    SOVA_EPOCH_BASE="${EPOCH_BASE}" \
    SOVA_HTTP_PORT="${A_HTTP_PORT}" \
    SOVA_AUTH_PORT="${A_AUTH_PORT}" \
    SOVA_P2P_PORT="${A_P2P_PORT}" \
    "${SOVA_BIN}" >"${A_LOG}" 2>&1 &
  A_PID=$!
  A_GEN=$((A_GEN + 1))
}

# SIGTERM A (bin/sova's graceful path persists its in-memory blocks) and
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
    sleep 0.5
  done
}

# SIP-4/SIP-7 precompile answers at Sova block $2 on node $1 for each Zcash
# height in $3.. (space separated), one line per height:
#   "<zh> blockAt=<hex> txInfo=<hex> blockStats=<hex>"
# The coinbase txid (display order, as the index stores it) comes from
# zebrad. Also checks blockAt against zebrad's hash and prints MISMATCH.
precompile_answers() { # <url> <sova_block> <zcash heights...>
  python3 - "$1" "$2" "${ZEBRAD_RPC}" "${@:3}" <<'PY'
import json, sys, urllib.request
url, block, zurl, heights = sys.argv[1], int(sys.argv[2]), sys.argv[3] + "/", [int(h) for h in sys.argv[4:]]
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
    return r if r is not None else f"ERR({str(err)[:80]})"
for h in heights:
    zh, _ = rpc(zurl, "getblockhash", [h])
    blk, _ = rpc(zurl, "getblock", [zh, 1])
    coinbase = blk["tx"][0]
    w = lambda v: format(v, "064x")
    block_at = call("0x6f8ea15d" + w(h))
    tx_info = call("0x0ac6923d" + coinbase)
    stats = call("0x84df4c97" + w(h))
    flag = ""
    if not block_at.startswith("0x") or block_at[2 + 64:2 + 128] != zh.lower() or int(block_at[2:66], 16) != 0:
        flag = f" MISMATCH(zebrad {zh[:16]})"
    print(f"{h} blockAt={block_at} txInfo={tx_info} blockStats={stats}{flag}")
PY
}

# Background prober: one line per probe, "<t_since_exec> ok|fail <latency>".
start_prober() { # <t_exec> <out>
  local t_exec="$1" out="$2"
  : >"${out}"
  (
    while :; do
      python3 - "${ENGINE_RPC_A}" "${t_exec}" "${RPC_TIMEOUT_S}" >>"${out}" <<'PY'
import json, sys, time, urllib.request
url, t_exec, timeout = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
t = time.time()
req = urllib.request.Request(url, data=b'{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}',
                             headers={"Content-Type": "application/json"})
try:
    with urllib.request.urlopen(req, timeout=timeout) as r:
        h = int(json.load(r)["result"], 16)
    print(f"{t - t_exec:.1f} ok {time.time() - t:.3f} {h}")
except Exception as e:
    kind = "refused" if "refused" in str(e).lower() else "fail"
    print(f"{t - t_exec:.1f} {kind} {time.time() - t:.3f} {type(e).__name__}")
PY
      sleep "${PROBE_S}"
    done
  ) &
  PROBER_PID=$!
}

# Timestamp (epoch seconds) of the first log line in $1 matching $2, or "".
first_log_time() {
  strip_ansi "$1" | grep -m1 -F -- "$2" | grep -oE '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z' \
    | python3 -c "import sys,datetime
s=sys.stdin.read().strip()
print(datetime.datetime.strptime(s[:26].rstrip('Z'),'%Y-%m-%dT%H:%M:%S.%f').replace(tzinfo=datetime.timezone.utc).timestamp() if s else '')" 2>/dev/null
}
last_log_time() {
  strip_ansi "$1" | grep -F -- "$2" | tail -1 | grep -oE '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z' \
    | python3 -c "import sys,datetime
s=sys.stdin.read().strip()
print(datetime.datetime.strptime(s[:26].rstrip('Z'),'%Y-%m-%dT%H:%M:%S.%f').replace(tzinfo=datetime.timezone.utc).timestamp() if s else '')" 2>/dev/null
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
if lsof -nP -iTCP:"${PROXY_PORT}" -sTCP:LISTEN >/dev/null 2>&1; then
  echo "error: port ${PROXY_PORT} already in use; set RESTART_LONG_PROXY_PORT" >&2
  exit 1
fi
start_stack

# Node A reads zebrad through a local proxy that counts requests per method
# (GET /__stats) and can add per-request latency (RESTART_LONG_ZEBRAD_DELAY_MS)
# to model a slower zebrad than a local regtest one. The script itself talks
# to zebrad directly.
python3 - "${PROXY_PORT}" "${ZEBRAD_RPC}" "${DELAY_MS}" >"${WORK_DIR}/proxy.log" 2>&1 <<'PY' &
import json, sys, threading, time, urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
port, up, delay = int(sys.argv[1]), sys.argv[2] + "/", float(sys.argv[3]) / 1000
lock, counts = threading.Lock(), {}
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def reply(self, out):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)
    def do_GET(self):
        with lock:
            self.reply(json.dumps(counts).encode())
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        try:
            reqs = json.loads(body)
            for r in reqs if isinstance(reqs, list) else [reqs]:
                with lock:
                    counts[r.get("method", "?")] = counts.get(r.get("method", "?"), 0) + 1
        except Exception:
            pass
        if delay:
            time.sleep(delay)
        req = urllib.request.Request(up, data=body, headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=60) as r:
            self.reply(r.read())
ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
PY
PROXY_PID=$!
NODE_ZEBRAD_RPC="http://127.0.0.1:${PROXY_PORT}"
sleep 1
echo "node A reads zebrad through a counting proxy on :${PROXY_PORT} (+${DELAY_MS} ms/request)"

# Requests node A has made to zebrad, per method, as "method=count ...".
proxy_count() { # <method>
  curl -s "${NODE_ZEBRAD_RPC}/__stats" | python3 -c "import sys,json
print(json.load(sys.stdin).get('$1', 0))"
}

MINER_DATA_DIR="${WORK_DIR}/miner"
A_DATADIR="${WORK_DIR}/datadir-a"
mkdir -p "${MINER_DATA_DIR}" "${A_DATADIR}"
"${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest init >"${WORK_DIR}/miner-init.log" 2>&1
EVM_ADDR="$(awk '/evm address/ {print $NF}' "${WORK_DIR}/miner-init.log" | head -1)"
if [[ -z "${EVM_ADDR}" || ! -f "${MINER_DATA_DIR}/keystore.json" ]]; then
  fail "setup: could not parse the miner identity / find its keystore"
  cat "${WORK_DIR}/miner-init.log" >&2
  exit 1
fi
zc_rpc generate "[1]" >/dev/null
EPOCH_BASE=$(($(zc_tip_height) + 1))
echo "epoch base B=${EPOCH_BASE} (Sova block N anchors Zcash N+${EPOCH_BASE}-1); growing to ${EPOCHS} epochs"

echo "--- starting node A (keeper: mine mode, SIP-6 + SIP-7, sealing keystore, persistent datadir) ---"
start_node_a
wait_for_eth_rpc "${ENGINE_RPC_A}" "${A_PID}" "node A" || exit 1
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
# (1) grow the chain
# =============================================================================

echo ""
echo "=== (1) grow: ${EPOCHS} epochs in chunks of ${CHUNK} ==="
T_GROW=$(now_f)
while :; do
  have=$(($(zc_tip_height) - EPOCH_BASE + 1))
  [[ "${have}" -ge "${EPOCHS}" ]] && break
  n=$((EPOCHS - have))
  [[ "${n}" -gt "${CHUNK}" ]] && n="${CHUNK}"
  zc_rpc generate "[${n}]" >/dev/null
  if ! out="$(wait_a_at_tip 600)"; then
    fail "(1) grow: A did not keep up (${out})"
    exit 1
  fi
  echo "  $((have + n)) epochs sealed ($(since "${T_GROW}") s)"
done
N="$(height_of "${ENGINE_RPC_A}")"
ZTIP="$(zc_tip_height)"
pass "(1) grow: A at head N=${N} = Zcash tip ${ZTIP} - B + 1 after $(since "${T_GROW}") s"

# =============================================================================
# (2) snapshot the precompile answers
# =============================================================================

SAMPLE_HEIGHTS=("${EPOCH_BASE}" "$((EPOCH_BASE + 1))" "$((EPOCH_BASE + N / 4))" "$((EPOCH_BASE + N / 2))" "$((ZTIP - 1))" "${ZTIP}")
SAMPLE_LIST="$(IFS=,; echo "${SAMPLE_HEIGHTS[*]}")"
precompile_answers "${ENGINE_RPC_A}" "${N}" "${SAMPLE_HEIGHTS[@]}" >"${WORK_DIR}/answers-before.txt"
if grep -q -E 'MISMATCH|ERR\(' "${WORK_DIR}/answers-before.txt"; then
  fail "(2) snapshot: A's precompile answers at ${N} disagree with zebrad or error:"
  cat "${WORK_DIR}/answers-before.txt" >&2
else
  pass "(2) snapshot: blockAt/txInfo/blockStats at Sova ${N} for Zcash ${SAMPLE_LIST} match zebrad"
fi

# =============================================================================
# (3) restart the keeper
# =============================================================================

echo ""
echo "=== (3) SIGTERM A at head ${N}; mine one Zcash block; restart A ==="
T_STOP=$(now_f)
stop_node_a
echo "  A stopped in $(since "${T_STOP}") s"
zc_rpc generate "[1]" >/dev/null
WANT=$((N + 1))
FETCH0="$(proxy_count getblock)"
T_EXEC=$(now_f)
start_node_a
start_prober "${T_EXEC}" "${WORK_DIR}/probes.txt"

T_SEALED=""
DEADLINE_S=$((BUDGET_S > MAX_WAIT_S ? BUDGET_S : MAX_WAIT_S))
while :; do
  if ! kill -0 "${A_PID}" 2>/dev/null; then
    fail "(3) node A exited after the restart"
    break
  fi
  h="$(curl -s -m 2 -X POST -H 'Content-Type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' "${ENGINE_RPC_A}" \
    | python3 -c "import sys,json
try: print(int(json.load(sys.stdin)['result'],16))
except Exception: print(0)" 2>/dev/null)"
  if [[ "${h:-0}" -ge "${WANT}" ]]; then
    T_SEALED=$(now_f)
    break
  fi
  el="$(since "${T_EXEC}")"
  if python3 -c "import sys; sys.exit(0 if ${el} >= ${DEADLINE_S} else 1)"; then
    break
  fi
  # Progress every ~30 s.
  if [[ $((${el%.*} % 30)) -eq 0 ]]; then echo "  ... ${el} s after exec, head ${h:-?} (want ${WANT})"; fi
  sleep 0.5
done
FETCHES=$(($(proxy_count getblock) - FETCH0))
kill "${PROBER_PID}" 2>/dev/null || true
wait "${PROBER_PID}" 2>/dev/null || true
PROBER_PID=""

# =============================================================================
# (4) assertions
# =============================================================================

echo ""
echo "=== (4) restart: seal within ${BUDGET_S} s, RPC answers throughout ==="
if [[ -n "${T_SEALED}" ]]; then
  SEAL_S="$(python3 -c "print(f'{${T_SEALED} - ${T_EXEC}:.1f}')")"
  if python3 -c "import sys; sys.exit(0 if ${SEAL_S} <= ${BUDGET_S} else 1)"; then
    pass "(4a) A sealed ${WANT} ${SEAL_S} s after the exec (budget ${BUDGET_S} s; ${N} epochs)"
  else
    fail "(4a) A sealed ${WANT} only ${SEAL_S} s after the exec: over the ${BUDGET_S} s budget (${N} epochs)"
  fi
else
  SEAL_S=">${DEADLINE_S}"
  fail "(4a) A did not seal ${WANT} within ${DEADLINE_S} s of the exec (head $(height_of "${ENGINE_RPC_A}"))"
fi

# (4b) RPC: up within RPC_UP_S, then every probe answered until sealed.
read -r RPC_UP_AT PROBES OK_N BAD_N WORST_GAP MAX_LAT BAD_LIST <<<"$(python3 - "${WORK_DIR}/probes.txt" "${SEAL_S#>}" <<'PY'
import sys
lines = [l.split() for l in open(sys.argv[1]) if l.strip()]
end = float(sys.argv[2])
up = None
ok = bad = 0
max_lat = 0.0
bad_at = []
last_ok = None
worst_gap = 0.0
for l in lines:
    t, kind, lat = float(l[0]), l[1], float(l[2])
    if t > end:
        break
    if kind == "ok":
        if up is None:
            up = t
        ok += 1
        max_lat = max(max_lat, lat)
        if last_ok is not None:
            worst_gap = max(worst_gap, t - last_ok)
        last_ok = t
    elif up is not None:
        bad += 1
        bad_at.append(f"{t:.0f}s:{kind}")
print(f"{up if up is not None else -1} {len(lines)} {ok} {bad} {worst_gap:.1f} {max_lat:.2f} {','.join(bad_at[:12]) or '-'}")
PY
)"
echo "  probes: ${PROBES} (every ${PROBE_S} s, timeout ${RPC_TIMEOUT_S} s); first answer ${RPC_UP_AT} s after exec; ${OK_N} ok, ${BAD_N} failed after it; max latency ${MAX_LAT} s; worst gap between answers ${WORST_GAP} s"
if python3 -c "import sys; sys.exit(0 if 0 <= ${RPC_UP_AT} <= ${RPC_UP_S} else 1)"; then
  pass "(4b) A's RPC answered ${RPC_UP_AT} s after the exec (limit ${RPC_UP_S} s)"
else
  fail "(4b) A's RPC did not answer within ${RPC_UP_S} s of the exec (first answer: ${RPC_UP_AT})"
fi
if [[ "${BAD_N}" -eq 0 ]]; then
  pass "(4b) every RPC probe between the first answer and the seal returned within ${RPC_TIMEOUT_S} s"
else
  fail "(4b) ${BAD_N} RPC probe(s) failed or timed out (${RPC_TIMEOUT_S} s) while A was coming back: ${BAD_LIST}"
fi

# (4c) precompile answers unchanged.
if [[ -n "${T_SEALED}" ]]; then
  precompile_answers "${ENGINE_RPC_A}" "${N}" "${SAMPLE_HEIGHTS[@]}" >"${WORK_DIR}/answers-after.txt"
  if diff -q "${WORK_DIR}/answers-before.txt" "${WORK_DIR}/answers-after.txt" >/dev/null; then
    pass "(4c) A's SIP-4/SIP-7 answers at Sova ${N} after the restart equal the snapshot (${SAMPLE_LIST})"
  else
    fail "(4c) A's precompile answers changed across the restart:"
    diff "${WORK_DIR}/answers-before.txt" "${WORK_DIR}/answers-after.txt" >&2 || true
  fi
fi

# (4d) the restart's zebrad work must not grow with the chain: count the
# blocks A fetched (getblock) between the exec and the seal.
if [[ -n "${T_SEALED}" ]]; then
  if [[ "${FETCHES}" -le "${MAX_FETCHES}" ]]; then
    pass "(4d) A fetched ${FETCHES} Zcash blocks from zebrad to come back (limit ${MAX_FETCHES}; the chain has ${N})"
  else
    fail "(4d) A fetched ${FETCHES} Zcash blocks from zebrad to come back (limit ${MAX_FETCHES}; the chain has ${N}): the restart rescans history"
  fi
fi

# Where the time went (bin/sova's own log; engine::driver=debug).
T_WAIT_FIRST="$(first_log_time "${A_LOG}" "waiting for our Zcash scan to record the head's epoch")"
T_WAIT_LAST="$(last_log_time "${A_LOG}" "waiting for our Zcash scan to record the head's epoch")"
T_TRIGGER="$(first_log_time "${A_LOG}" "sova epoch trigger")"
WAITS="$(strip_ansi "${A_LOG}" | grep -cF "waiting for our Zcash scan to record the head's epoch" || true)"
python3 - "${T_EXEC}" "${T_WAIT_FIRST:-}" "${T_WAIT_LAST:-}" "${T_TRIGGER:-}" "${WAITS}" <<'PY'
import sys
t0 = float(sys.argv[1])
f = lambda s: f"{float(s) - t0:.1f} s" if s else "-"
print(f"  A's log: sealer gate 'waiting for our Zcash scan' x{sys.argv[5]}, first {f(sys.argv[2])}, last {f(sys.argv[3])} after exec;"
      f" first 'sova epoch trigger' {f(sys.argv[4])} after exec")
if sys.argv[3] and sys.argv[4]:
    print(f"  => expectations rescan (gate held) ~{float(sys.argv[3]) - t0:.1f} s; gate lifted -> first trigger"
          f" {float(sys.argv[4]) - float(sys.argv[3]):.1f} s (the sealer's own follower rescans from the base too)")
PY

# =============================================================================
# (5) liveness
# =============================================================================

if [[ -n "${T_SEALED}" ]]; then
  echo ""
  echo "=== (5) liveness: ${ADVANCE} more Zcash blocks ==="
  zc_rpc generate "[${ADVANCE}]" >/dev/null
  if out="$(wait_a_at_tip 120)"; then
    pass "(5) A sealed through $(height_of "${ENGINE_RPC_A}")"
  else
    fail "(5) A did not follow ${ADVANCE} more Zcash blocks (${out})"
  fi
fi

echo ""
echo "=== diagnostics ==="
echo "  epoch base ${EPOCH_BASE}; epochs ${N}; tokio workers ${TOKIO_WORKERS:-all}; zebrad delay ${DELAY_MS} ms/request; restart->sealed ${SEAL_S} s; total $((SECONDS - T0)) s"
echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "RESTART LONG-CHAIN SCENARIO PASSED (${N} epochs; restart->sealed ${SEAL_S} s)"
else
  echo "RESTART LONG-CHAIN SCENARIO: ${FAILURES} ASSERTION(S) FAILED (${N} epochs; restart->sealed ${SEAL_S} s)" >&2
fi
[[ "${FAILURES}" -eq 0 ]]
