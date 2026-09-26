#!/usr/bin/env bash
# Randomized Zcash-reorg stress on the testnet's topology, with SIP-6 and
# SIP-7 on. A long, seeded, reproducible mix of Zcash reorgs, reorg
# flip-flops, node restarts, burns and transactions, with the chain's
# invariants checked every ~10 s and strictly at the end.
#
# Why: both serious public-testnet stalls came from a Zcash reorg hitting
# consensus code at an awkward moment:
#   2026-09-24, height 323 -- a stale NULL block vs its re-seal
#     (fixed 52d02c6, 072d633; pinned by null-reorg-scenario.sh).
#   2026-09-26, height 6225 -- the keeper built 6225 during a Zcash reorg
#     FLIP-FLOP (A -> B -> A within seconds). The retried build hit the same
#     SIP-6 seal-journal slot (height, parent, anchor) and re-published the
#     first block, which re-executed to a different state root; the journal
#     forbade a new seal and the arbiter retried the invalid candidate every
#     second: a stall (fixed 6a5aaa6: Signer::discard_invalid,
#     CandidateTracker::forget_invalid).
# This scenario looks for a third.
#
# How the 6225 shape arises (and what the flip-flop event aims at): the
# anchor of a build comes from the SEALER's follower, but SIP-7's
# ZcashBlocks.record system call is executed from the zcash INDEX, which the
# separate EXPECTATIONS follower feeds (crates/engine/src/driver.rs
# run_sealer, crates/engine/src/expectations.rs run_expectations, both 2 s
# poll loops; crates/evm/src/blocks.rs record_zcash_block). When Zcash block
# Z_A at height e lives only briefly, the sealer can see it while the
# index only ever sees its replacement Z_B: the sealed block is anchored to
# Z_A but records Z_B, it is journaled under (N, parent, Z_A), our own
# node holds it (anchor mismatch), and once Zcash flips back to Z_A every
# rebuild of N gets that journaled block back -- which now re-executes to a
# different state root. The flip-flop's dwell on Z_A is drawn from
# 0.2..2.5 s so that some flips straddle the two poll loops.
#
# Topology (the testnet's, on loopback, over sova/1):
#   node A -- "keeper": mine mode, SOVA_SIP6=1 + SOVA_SIP7=1, sealing key =
#             a `sova-miner init` keystore, persistent datadir.
#   node B -- "seed": follow-only, C5-enforcing, SIP-6/7, static peer = A,
#             persistent datadir.
#   node C -- "rpc": follow-only, SOVA_RPC_PROFILE=public, SIP-6/7, static
#             peer = B ONLY, persistent datadir.
# All three share one regtest zebrad. The script itself mines Zcash (one
# block every REORG_STRESS_BLOCK_S seconds) so events can pause it.
#
# The schedule is generated up front from a seeded RNG (printed, and
# written to the run dir as schedule.txt); REORG_STRESS_SEED reproduces
# it. Events, one every 30..90 s (not before their scheduled time; a slow
# event pushes the rest back):
#   reorg      depth 1 (and 2..4) -- invalidateblock + replacement blocks,
#              under a NULL tip, a SEALED (burn) tip, or wherever the chain
#              is; replacement coinbase to Sapling for none / the first /
#              all replacement blocks (so SIP-7 pool deltas change; see
#              null-reorg-scenario.sh); sometimes one block longer.
#   flipflop   mine Z_A (depth 1, sometimes 2) right as A is about to
#              build; after dwell_a (0.2..2.5 s) invalidate it and mine Z_B (Sapling
#              coinbase half the time); after dwell_b reconsiderblock Z_A and
#              invalidate Z_B, so the original chain wins again; sometimes
#              a third flip back to B. Mostly in burn epochs (A's burn
#              waiting in the mempool): only a SEALED block is journaled, so
#              only that can hit the 6225 shape. Half the flip-flops are
#              AIMED (see aim_mine): A's two poll loops are phase-locked, the
#              index's ~10-150 ms ahead of the sealer's, and Z_A is timed to
#              appear inside that gap (a random time hits it 1-3% of the time).
#   keeper     SIGTERM node A, (half the time) a depth 1..2 reorg while it
#              is down, Zcash keeps moving, restart on the same datadir.
#   seed       (exactly once) SIGTERM node B, restart on the same datadir.
#   burn       run sova-miner for 4..12 epochs (one burn per Zcash block).
#   tx         1..3 transfers signed with `cast mktx` and sent to C with
#              eth_sendRawTransaction (they travel C -> B -> A); also starts
#              a burn stretch, since only SEALED blocks carry transactions.
#
# Invariants, checked every REORG_STRESS_CHECK_S (10) seconds -- a
# violation must PERSIST for the recovery allowance K (REORG_STRESS_RECOVERY_MIN,
# default 3 minutes) before it fails the run, since every event legitimately
# disturbs the chain for a few seconds:
#   (lag)     every node's head within 2 of the Zcash-expected height
#             N = zcash_tip - B + 1 (a node down for a scheduled restart is
#             exempt while down);
#   (agree)   A, B and C hold the same hash at every height <= their common
#             head (last REORG_STRESS_WINDOW=40 heights continuously; all of
#             them at the end);
#   (anchor)  every canonical block's parentBeaconBlockRoot == zebrad's
#             getblockhash(N + B - 1) (same window; all at the end);
#   (stuck)   no node on the same head for > 5 min while Zcash advances;
#   (tx)      every tx sent to C is mined (receipt on C) before 5 SEALED
#             blocks have gone by on C (and it is at least 30 s old; null
#             blocks carry no transactions); at the end every tx has the
#             same receipt block on A, B and C;
#   (sip7)    no DeltaMismatch / 0x78bab1c2 log line more than
#             REORG_STRESS_DM_GRACE_S (60) s after the latest Zcash reorg;
#   (arbiter) never more than 60 consecutive "arbiter forkchoice update
#             failed" lines for one height on any node (the 6225 signature).
# Those without a grace (sip7, arbiter) fail at once. After the stress
# period the script lets the chain settle (burns on, no events) and requires
# full convergence within K minutes, then runs the strict final check.
#
# On failure the run stops at once. Either way the run dir
# (REORG_STRESS_RUN_DIR, default $TMPDIR/sova-reorg-stress/<utc>-seed<seed>)
# receives: seed, schedule.txt, timeline.txt (every executed step with
# hashes and dwell times), checks.log, summary.txt, chain dumps, txs, and
# every node / burner / zebrad log (gzipped).
#
# Env: REORG_STRESS_MINUTES (45), REORG_STRESS_SEED (random),
# REORG_STRESS_RECOVERY_MIN (3), REORG_STRESS_BLOCK_S (5),
# REORG_STRESS_CHECK_S (10), REORG_STRESS_WINDOW (40),
# REORG_STRESS_STUCK_MIN (5), REORG_STRESS_DM_GRACE_S (60),
# REORG_STRESS_FCU_MAX (60), REORG_STRESS_TX_BLOCKS (5),
# REORG_STRESS_GAP_S ("30 90"), REORG_STRESS_WEIGHTS
# ("flipflop=34 reorg1=18 reorgN=14 keeper=8 burn=8 tx=18"), REORG_STRESS_MAX_DEPTH
# (4; caps every drawn reorg/flip-flop depth without changing the rest of
# the schedule, e.g. 1 to leave out depth >= 2), REORG_STRESS_AIM_ALL (0; 1
# aims every flip-flop),
# REORG_STRESS_RUN_DIR, REORG_STRESS_A_RUST_LOG (info,engine::miner=debug),
# plus p2p-common.sh's SOVA_P2P_SIM_*, SOVA_BIN, SOVA_MINER_BIN.
# Needs foundry's `cast`, python3 and perl.
#
# Isolation: zebrad on :18452 (compose project sova-reorg-stress-sim,
# container sova-zebrad-reorg-stress), A on 10745/10751/31111, B on
# 10845/10851/31112, C on 10945/10951/31113. All overridable.

SCENARIO="reorg-stress"
WORK_PREFIX="sova-reorg-stress"
P2P_THREE_NODES=1
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18452}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-reorg-stress}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-reorg-stress-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=10745 10751 31111}"
: "${SOVA_P2P_SIM_B_PORTS:=10845 10851 31112}"
: "${SOVA_P2P_SIM_C_PORTS:=10945 10951 31113}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS SOVA_P2P_SIM_C_PORTS
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"

MINUTES="${REORG_STRESS_MINUTES:-45}"
SEED="${REORG_STRESS_SEED:-$(python3 -c 'import random; print(random.SystemRandom().randrange(1, 2**31))')}"
RECOVERY_MIN="${REORG_STRESS_RECOVERY_MIN:-3}"
BLOCK_S="${REORG_STRESS_BLOCK_S:-5}"
CHECK_S="${REORG_STRESS_CHECK_S:-10}"
WINDOW="${REORG_STRESS_WINDOW:-40}"
STUCK_MIN="${REORG_STRESS_STUCK_MIN:-5}"
DM_GRACE_S="${REORG_STRESS_DM_GRACE_S:-60}"
FCU_MAX="${REORG_STRESS_FCU_MAX:-60}"
TX_BLOCKS="${REORG_STRESS_TX_BLOCKS:-5}"
GAP_S="${REORG_STRESS_GAP_S:-30 90}"
WEIGHTS="${REORG_STRESS_WEIGHTS:-flipflop=34 reorg1=18 reorgN=14 keeper=8 burn=8 tx=18}"
MAX_DEPTH="${REORG_STRESS_MAX_DEPTH:-4}"
AIM_ALL="${REORG_STRESS_AIM_ALL:-0}"
A_RUST_LOG="${REORG_STRESS_A_RUST_LOG:-info,engine::miner=debug}"
RUN_DIR="${REORG_STRESS_RUN_DIR:-${TMPDIR:-/tmp}/sova-reorg-stress/$(date -u +%Y%m%dT%H%M%SZ)-seed${SEED}}"
RUN_DIR="${RUN_DIR%/}"

FUND_BLOCKS=120
PER_EPOCH_ZAT=100000
# zebrad's default regtest Sapling miner address (see null-reorg-scenario.sh).
SAPLING_ADDR="zregtestsapling1xl84ekz6stprmvrp39s77mf9t953nqjndwlcjtzfrr3cgjjez87639xm4u9pfuvylrhecx0c2j8"
# reth dev genesis account 0 (public "test test ... junk" mnemonic). Local only.
TX_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
SINK="0x000000000000000000000000000000000000dEaD"

for tool in cast python3 perl; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "error: this scenario needs \`${tool}\` on PATH" >&2
    exit 1
  fi
done

A_DATADIR=""
B_DATADIR=""
C_DATADIR=""
A_GEN=0
B_GEN=0
A_LOG=""
BURNER_PID=""
BURNER_RUNS=0
TX_NONCE=0
TIMELINE=""
T0=${SECONDS}
NEXT_BLOCK=0
NEXT_CHECK=0
EVENTS_DONE=0
FAIL_MSG=""

# --- run dir: saved on every exit, before p2p-common's cleanup deletes WORK_DIR

save_run_dir() {
  [[ -n "${WORK_DIR}" && -d "${WORK_DIR}" ]] || return 0
  mkdir -p "${RUN_DIR}" || return 0
  local f
  for f in schedule.txt timeline.txt checks.log txs.txt reorg-times.txt check-state.json summary.txt; do
    [[ -f "${WORK_DIR}/${f}" ]] && cp "${WORK_DIR}/${f}" "${RUN_DIR}/"
  done
  cp "${WORK_DIR}"/chain-*.txt "${RUN_DIR}/" 2>/dev/null || true
  echo "${SEED}" >"${RUN_DIR}/seed"
  docker logs "${ZEBRAD_CONTAINER}" >"${WORK_DIR}/zebrad.log" 2>&1 || true
  for f in "${WORK_DIR}"/*.log; do
    [[ -f "${f}" ]] || continue
    gzip -c "${f}" >"${RUN_DIR}/${f##*/}.gz" 2>/dev/null || true
  done
  echo "--- run dir: ${RUN_DIR} (seed ${SEED}) ---"
}
trap 'rc=$?; save_run_dir; (exit ${rc}); cleanup' EXIT

# --- small helpers --------------------------------------------------------

now_f() { perl -MTime::HiRes=time -e 'printf("%.3f\n", time)'; }

tl() {
  local line
  line="$(date -u +%Y-%m-%dT%H:%M:%SZ) +$((SECONDS - T0))s $*"
  echo "${line}" >>"${TIMELINE}"
  echo "  [tl] ${line}"
}

height_of() { eth_block_number "$1" 2>/dev/null || echo 0; }

zc_json_result() { python3 -c "import sys,json
try:
    r = json.load(sys.stdin).get('result')
    print('' if r is None else (r if isinstance(r, str) else json.dumps(r)))
except Exception:
    print('')"; }

zc_hash_at() { zc_rpc getblockhash "[$1]" | zc_json_result | tr 'A-F' 'a-f'; }
zc_mempool_size() { zc_rpc getrawmempool "[]" | python3 -c "import sys,json
try:
    print(len(json.load(sys.stdin).get('result') or []))
except Exception:
    print(0)"; }

# Mine $1 Zcash blocks (coinbase to zebrad's transparent miner address, or
# to $2).
zc_mine() {
  if [[ -n "${2:-}" ]]; then
    zc_rpc generatetoaddress "[$1, \"$2\"]" >/dev/null
  else
    zc_rpc generate "[$1]" >/dev/null
  fi
  NEXT_BLOCK=$((SECONDS + BLOCK_S))
}

mark_reorg() { now_f >>"${WORK_DIR}/reorg-times.txt"; }

# extraData length (bytes) of block $2 on node $1; "" if none.
extra_len() {
  eth_rpc "$1" eth_getBlockByNumber "[\"$(printf '0x%x' "$2")\", false]" | python3 -c "import sys,json
try:
    b = json.load(sys.stdin).get('result')
    print((len(b['extraData']) - 2) // 2 if b else '')
except Exception:
    print('')"
}

node_down() { echo "$1" >>"${WORK_DIR}/down"; }
node_up() {
  if [[ -f "${WORK_DIR}/down" ]]; then
    grep -vx "$1" "${WORK_DIR}/down" >"${WORK_DIR}/down.tmp" || true
    mv "${WORK_DIR}/down.tmp" "${WORK_DIR}/down"
  fi
}

# --- nodes ---------------------------------------------------------------

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
    SOVA_DATADIR="${B_DATADIR}" \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
    SOVA_EPOCH_BASE="${EPOCH_BASE}" \
    SOVA_P2P_PEERS="${ENODE_A}" \
    SOVA_HTTP_PORT="${B_HTTP_PORT}" \
    SOVA_AUTH_PORT="${B_AUTH_PORT}" \
    SOVA_P2P_PORT="${B_P2P_PORT}" \
    "${SOVA_BIN}" >"${WORK_DIR}/node-b-${B_GEN}.log" 2>&1 &
  B_PID=$!
  B_GEN=$((B_GEN + 1))
  wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B"
}

start_node_c() {
  p2p_env \
    SOVA_SIP6=1 \
    SOVA_SIP7=1 \
    SOVA_FOLLOW_ONLY=1 \
    SOVA_RPC_PROFILE=public \
    SOVA_DATADIR="${C_DATADIR}" \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
    SOVA_EPOCH_BASE="${EPOCH_BASE}" \
    SOVA_P2P_PEERS="${ENODE_B}" \
    SOVA_HTTP_PORT="${C_HTTP_PORT}" \
    SOVA_AUTH_PORT="${C_AUTH_PORT}" \
    SOVA_P2P_PORT="${C_P2P_PORT}" \
    "${SOVA_BIN}" >"${WORK_DIR}/node-c-0.log" 2>&1 &
  C_PID=$!
  wait_for_eth_rpc "${ENGINE_RPC_C}" "${C_PID}" "node C"
}

# SIGTERM (bin/sova's graceful path) and wait; SIGKILL after 90 s.
stop_pid() { # <pid> <label>
  local pid="$1" deadline=$((SECONDS + 90))
  [[ -z "${pid}" ]] && return 0
  kill -TERM "${pid}" 2>/dev/null || true
  while kill -0 "${pid}" 2>/dev/null; do
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      tl "WARN $2 (pid ${pid}) still alive 90s after SIGTERM; SIGKILL"
      kill -KILL "${pid}" 2>/dev/null || true
      break
    fi
    sleep 1
  done
  wait "${pid}" 2>/dev/null || true
}

# --- burner (sova-miner) ------------------------------------------------------

burner_running() { [[ -n "${BURNER_PID}" ]] && kill -0 "${BURNER_PID}" 2>/dev/null; }

ensure_burner() { # <epochs>
  burner_running && return 0
  BURNER_RUNS=$((BURNER_RUNS + 1))
  echo "=== burner run ${BURNER_RUNS}: ${1} epochs ===" >>"${WORK_DIR}/burner.log"
  "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
    --budget-zat $((($1 + 2) * (PER_EPOCH_ZAT + 100000))) --per-epoch-zat "${PER_EPOCH_ZAT}" \
    --rpc "${ZEBRAD_RPC}" --max-epochs "$1" --poll-interval-ms 500 >>"${WORK_DIR}/burner.log" 2>&1 &
  BURNER_PID=$!
  tl "burner start run=${BURNER_RUNS} epochs=$1 pid=${BURNER_PID}"
}

stop_burner() {
  if burner_running; then
    kill "${BURNER_PID}" 2>/dev/null || true
    wait "${BURNER_PID}" 2>/dev/null || true
    tl "burner stop run=${BURNER_RUNS}"
  fi
  BURNER_PID=""
}

# --- checker -------------------------------------------------------------------

export RS_WORK RS_EPOCH_BASE RS_A="${ENGINE_RPC_A}" RS_B="${ENGINE_RPC_B}" RS_C="${ENGINE_RPC_C}" \
  RS_ZEBRAD="${ZEBRAD_RPC}" RS_RECOVERY_S=$((RECOVERY_MIN * 60)) RS_STUCK_S=$((STUCK_MIN * 60)) \
  RS_WINDOW="${WINDOW}" RS_DM_GRACE_S="${DM_GRACE_S}" RS_FCU_MAX="${FCU_MAX}" RS_TX_BLOCKS="${TX_BLOCKS}"

# run_check <tick|converged|final>: prints a status line (and VIOLATION
# lines); rc 0 ok, 1 violation, 2 (converged mode) not converged yet.
run_check() {
  python3 - "$1" <<'PY'
import glob, json, os, re, sys, time, urllib.request
E = os.environ
mode = sys.argv[1]
work = E["RS_WORK"]
base = int(E["RS_EPOCH_BASE"])
NODES = [("A", E["RS_A"]), ("B", E["RS_B"]), ("C", E["RS_C"])]
ZURL = E["RS_ZEBRAD"] + "/"
K = float(E["RS_RECOVERY_S"])
STUCK = float(E["RS_STUCK_S"])
W = int(E["RS_WINDOW"])
DM_GRACE = float(E["RS_DM_GRACE_S"])
FCU_MAX = int(E["RS_FCU_MAX"])
TX_BLOCKS = int(E["RS_TX_BLOCKS"])
TX_MIN_AGE = 30.0
statef = os.path.join(work, "check-state.json")
try:
    st = json.load(open(statef))
except Exception:
    st = {}
for k in ("lag", "last", "anchor", "agree", "off", "fcu", "tx", "counts", "partial"):
    st.setdefault(k, {})
st.setdefault("dm_tolerated", 0)
now = time.time()
viol, notes = [], []

# A host that sleeps (a closed laptop lid) freezes the sim, zebrad and the
# nodes together, and on wake every timer below would read the pause as the
# chain being stuck. The monotonic clock stops during a suspend and the wall
# clock doesn't, so their difference since the last check is time spent
# asleep: move every timer forward by it and say so.
mono = time.monotonic()
prev = st.get("prev_clock")
st["prev_clock"] = [now, mono]
if prev:
    slept = (now - prev[0]) - (mono - prev[1])
    if slept > 30:
        for bucket in ("lag", "anchor", "agree"):
            for key in st[bucket]:
                st[bucket][key] += slept
        for last in st["last"].values():
            last[1] += slept
        st["suspended_s"] = st.get("suspended_s", 0) + slept
        print(f"check: host was suspended ~{int(slept)}s; timers moved forward")
ANSI = re.compile(r"\x1b\[[0-9;]*m")

def call(url, method, params, timeout=8):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            d = json.load(r)
    except Exception:
        return ("err", None)
    if "error" in d and d["error"]:
        return ("rpcerr", d["error"])
    return ("ok", d.get("result"))

def batch(url, calls, timeout=15):
    """[(method, params)] -> [result|None]; falls back to single calls."""
    out = []
    for i in range(0, len(calls), 100):
        chunk = calls[i:i + 100]
        body = json.dumps([{"jsonrpc": "2.0", "id": j, "method": m, "params": p}
                           for j, (m, p) in enumerate(chunk)]).encode()
        req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
        res = None
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                d = json.load(r)
            if isinstance(d, list):
                by = {x.get("id"): x.get("result") for x in d}
                res = [by.get(j) for j in range(len(chunk))]
        except Exception:
            res = None
        if res is None:
            res = [call(url, m, p)[1] for (m, p) in chunk]
        out.extend(res)
    return out

def blocks(url, lo, hi):
    """{n: (hash, anchor, extra_len)} for heights lo..hi."""
    if hi < lo:
        return {}
    ns = list(range(lo, hi + 1))
    res = batch(url, [("eth_getBlockByNumber", [hex(n), False]) for n in ns])
    out = {}
    for n, b in zip(ns, res):
        if b:
            out[n] = (b["hash"].lower(), (b.get("parentBeaconBlockRoot") or "-").lower(),
                      (len(b["extraData"]) - 2) // 2)
    return out

zcache = {}
def zhash(e):
    if e not in zcache:
        s, r = call(ZURL, "getblockhash", [e])
        zcache[e] = ("0x" + r.lower()) if s == "ok" and r else None
    return zcache[e]

def persist(bucket, key, bad, what):
    """A condition may persist for K seconds before it is a violation."""
    b = st[bucket]
    if not bad:
        b.pop(key, None)
        return
    since = b.setdefault(key, now)
    if now - since > K:
        viol.append(f"({bucket}) {what} -- for {int(now - since)}s (> {int(K)}s allowance)")

down = set()
try:
    down = {l.strip() for l in open(os.path.join(work, "down")) if l.strip()}
except Exception:
    pass

s, zt = call(ZURL, "getblockcount", [])
if s != "ok" or zt is None:
    print(f"check: zebrad unreadable ({s}); skipped")
    sys.exit(0)
expected = zt - base + 1

heads = {}
for name, url in NODES:
    s, r = call(url, "eth_blockNumber", [])
    heads[name] = int(r, 16) if s == "ok" and r else None

# (lag) and (stuck)
for name, _ in NODES:
    h = heads[name]
    if name in down:
        st["lag"].pop(name, None)
        st["last"][name] = [h, now, zt]
        continue
    bad = h is None or abs(expected - h) > 2
    persist("lag", name, bad, f"node {name} head {h} vs Zcash-expected {expected} (zcash tip {zt})")
    last = st["last"].get(name)
    if h is None or last is None or last[0] != h:
        st["last"][name] = [h, now, zt]
    elif now - last[1] > STUCK and zt > last[2]:
        viol.append(f"(stuck) node {name} on head {h} for {int(now - last[1])}s while Zcash went {last[2]} -> {zt}")

up = [(n, u) for n, u in NODES if heads[n] is not None and n not in down]
full = mode == "final"
views = {}
for name, url in up:
    lo = 1 if full else max(1, heads[name] - W)
    views[name] = blocks(url, lo, heads[name])

# (anchor)
anchor_bad = []
for name, _ in up:
    for n, (h, anc, _x) in sorted(views[name].items()):
        z = zhash(n + base - 1)
        bad = anc != z
        if full:
            if bad:
                anchor_bad.append(f"{name}@{n}")
        else:
            persist("anchor", f"{name}:{n}", bad,
                    f"node {name} block {n} {h[:14]}.. anchored to {anc[:14]}.., zebrad has {(z or '<none>')[:14]}..")
    if not full:
        for key in [k for k in st["anchor"] if k.startswith(name + ":")]:
            if int(key.split(":")[1]) not in views[name]:
                st["anchor"].pop(key, None)
if full and anchor_bad:
    viol.append(f"(anchor) canonical blocks anchored to Zcash blocks zebrad no longer has: {' '.join(anchor_bad[:30])}")

# (agree)
agree_bad = []
if len(up) >= 2:
    common = min(heads[n] for n, _ in up)
    lo = 1 if full else max(1, common - W)
    seen_keys = set()
    for n in range(lo, common + 1):
        hs = {name: views[name].get(n, (None,))[0] for name, _ in up}
        bad = len(set(hs.values())) != 1 or None in hs.values()
        if full:
            if bad:
                agree_bad.append(f"{n}:" + ",".join(f"{k}={(v or '-')[:10]}" for k, v in hs.items()))
        else:
            seen_keys.add(str(n))
            persist("agree", str(n), bad, f"nodes disagree at height {n}: " +
                    " ".join(f"{k}={(v or '-')[:14]}" for k, v in hs.items()))
    if not full:
        for key in [k for k in st["agree"] if k not in seen_keys]:
            st["agree"].pop(key, None)
if full and agree_bad:
    viol.append(f"(agree) nodes disagree at: {' '.join(agree_bad[:20])}")

# Log scan: (sip7) DeltaMismatch after the grace, (arbiter) FCU failure runs,
# and diagnostic counters.
reorg_times = []
try:
    reorg_times = sorted(float(l) for l in open(os.path.join(work, "reorg-times.txt")) if l.strip())
except Exception:
    pass
COUNTERS = {
    "slot_freed": "slot freed for a new seal",
    "republish": "slot already signed; re-publishing",
    "resealing": "anchored to an orphaned zcash block; re-sealing",
    "reorg_observed": "zcash reorg observed",
    "unwound": "expectations and candidates unwound",
    "payload_rejected": "built payload rejected",
    "reputation_hit": "reputation hit",
    "fcu_failed": "arbiter forkchoice update failed",
    "delta_mismatch": "DeltaMismatch|78bab1c2",
    "loses_preference": "loses preference",
}
TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?)Z")
def line_ts(line):
    m = TS.match(line)
    if not m:
        return now
    t = m.group(1)
    frac = 0.0
    if "." in t:
        t, f = t.split(".")
        frac = float("0." + f)
    import calendar
    return calendar.timegm(time.strptime(t, "%Y-%m-%dT%H:%M:%S")) + frac

HEIGHT = re.compile(r"height=(\d+)")
for path in sorted(glob.glob(os.path.join(work, "node-*.log"))):
    base_name = os.path.basename(path)
    node = base_name.split("-")[1].upper()
    off = st["off"].get(path, 0)
    try:
        with open(path, "rb") as f:
            f.seek(off)
            data = f.read()
    except Exception:
        continue
    cut = data.rfind(b"\n")
    if cut < 0:
        continue
    st["off"][path] = off + cut + 1
    counts = st["counts"].setdefault(base_name, {})
    for raw in data[:cut + 1].decode("utf-8", "replace").splitlines():
        line = ANSI.sub("", raw)
        for key, pat in COUNTERS.items():
            if re.search(pat, line, re.I if key == "delta_mismatch" else 0):
                counts[key] = counts.get(key, 0) + 1
        if re.search(r"DeltaMismatch|78bab1c2", line, re.I):
            ts = line_ts(line)
            prior = [r for r in reorg_times if r <= ts + 2]
            if prior and ts - prior[-1] <= DM_GRACE:
                st["dm_tolerated"] += 1
            else:
                since = f"{int(ts - prior[-1])}s after the latest reorg" if prior else "with no reorg before it"
                viol.append(f"(sip7) {base_name}: DeltaMismatch {since}: {line[:300]}")
        if "arbiter forkchoice update failed" in line:
            m = HEIGHT.search(line)
            hgt = int(m.group(1)) if m else -1
            run = st["fcu"].get(node)
            if run and run[0] == hgt:
                run[1] += 1
            else:
                run = [hgt, 1]
            st["fcu"][node] = run
            if run[1] > FCU_MAX:
                viol.append(f"(arbiter) node {node}: {run[1]} consecutive 'arbiter forkchoice update failed' for height {hgt}: {line[:300]}")
        elif "arbiter adopted preferred candidate" in line:
            st["fcu"].pop(node, None)

# (tx)
txs = []
try:
    for l in open(os.path.join(work, "txs.txt")):
        p = l.split()
        if len(p) >= 3:
            txs.append((p[0], float(p[1]), int(p[2])))
except Exception:
    pass
urlC = E["RS_C"]
pending = 0
for h, t_sub, c_head in txs:
    rec = st["tx"].setdefault(h, {"block": None})
    if heads["C"] is None:
        continue
    s, r = call(urlC, "eth_getTransactionReceipt", [h])
    if s == "ok" and r:
        rec["block"] = int(r["blockNumber"], 16)
        rec["hash"] = r["blockHash"].lower()
        continue
    rec["block"] = None
    pending += 1
    if now - t_sub < TX_MIN_AGE:
        continue
    hi = heads["C"]
    bl = blocks(urlC, c_head + 1, hi) if hi > c_head else {}
    sealed = sum(1 for v in bl.values() if v[2] == 97)
    rec["sealed_since"] = sealed
    if sealed >= TX_BLOCKS:
        viol.append(f"(tx) {h} sent {int(now - t_sub)}s ago (C head then {c_head}) not mined on C after {sealed} sealed blocks (C head {hi})")

if mode == "final":
    for h, _, _ in txs:
        got = {}
        for name, url in NODES:
            s, r = call(url, "eth_getTransactionReceipt", [h])
            got[name] = r["blockHash"].lower() if s == "ok" and r else None
        if None in got.values() or len(set(got.values())) != 1:
            viol.append(f"(tx) {h}: receipt blocks differ or missing at the end: " +
                        " ".join(f"{k}={(v or '-')[:14]}" for k, v in got.items()))

json.dump(st, open(statef + ".tmp", "w"))
os.replace(statef + ".tmp", statef)

hs = " ".join(f"{n}={heads[n] if heads[n] is not None else 'down'}{'(down)' if n in down else ''}" for n, _ in NODES)
print(f"check[{mode}] zcash={zt} expected={expected} {hs} tx_pending={pending}/{len(txs)} "
      f"grace:lag={len(st['lag'])},anchor={len(st['anchor'])},agree={len(st['agree'])} dm_tolerated={st['dm_tolerated']}")
for v in viol:
    print("VIOLATION: " + v)
if viol:
    sys.exit(1)
if mode == "converged":
    hv = [heads[n] for n, _ in NODES]
    ok = (None not in hv and len(set(hv)) == 1 and hv[0] == expected and pending == 0
          and not st["anchor"] and not st["agree"])
    sys.exit(0 if ok else 2)
sys.exit(0)
PY
}

dump_chains() { # <tag>
  local name url hi lo
  for pair in "a ${ENGINE_RPC_A}" "b ${ENGINE_RPC_B}" "c ${ENGINE_RPC_C}"; do
    read -r name url <<<"${pair}"
    hi="$(height_of "${url}")"
    lo=$((hi > 80 ? hi - 80 : 1))
    python3 - "${url}" "${lo}" "${hi}" "${EPOCH_BASE}" "${ZEBRAD_RPC}" >"${WORK_DIR}/chain-${name}-$1.txt" <<'PY'
import sys, json, urllib.request
url, lo, hi, base, zurl = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
def rpc(u, m, p):
    req = urllib.request.Request(u, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return json.load(r).get("result")
    except Exception:
        return None
print("# N hash parent anchor extraLen zebrad@N+B-1 stateRoot txs")
for n in range(lo, hi + 1):
    zh = rpc(zurl + "/", "getblockhash", [n + base - 1])
    zh = ("0x" + zh.lower()) if zh else "-"
    b = rpc(url, "eth_getBlockByNumber", [hex(n), False])
    if not b:
        print(n, "-", "-", "-", "-", zh, "-", "-")
        continue
    print(n, b["hash"].lower(), b["parentHash"].lower(), (b.get("parentBeaconBlockRoot") or "-").lower(),
          (len(b["extraData"]) - 2) // 2, zh, b["stateRoot"].lower(), len(b.get("transactions") or []))
PY
  done
}

fail_stop() {
  FAIL_MSG="$*"
  fail "$*"
  tl "FAIL $*"
  stop_burner
  dump_chains fail
  write_summary "FAIL"
  exit 1
}

maybe_check() {
  [[ ${SECONDS} -lt ${NEXT_CHECK} ]] && return 0
  NEXT_CHECK=$((SECONDS + CHECK_S))
  local out rc
  out="$(run_check tick 2>&1)"
  rc=$?
  echo "$(date -u +%H:%M:%S) +$((SECONDS - T0))s ${out}" >>"${WORK_DIR}/checks.log"
  if [[ ${rc} -ne 0 ]]; then
    echo "${out}"
    fail_stop "invariant violated: $(grep '^VIOLATION' <<<"${out}" | head -3 | tr '\n' ' ')"
  fi
}

maybe_mine() {
  if [[ ${SECONDS} -ge ${NEXT_BLOCK} ]]; then
    zc_mine 1
  fi
}

# Run the Zcash cadence and the checks for $1 seconds.
pump() {
  local until=$((SECONDS + $1))
  while [[ ${SECONDS} -lt ${until} ]]; do
    maybe_mine
    maybe_check
    sleep 0.5
  done
}

# Wait until node A sits at the Zcash tip's Sova height (checks keep running).
settle_a() { # <timeout_s>
  local deadline=$((SECONDS + $1)) want
  while [[ ${SECONDS} -lt ${deadline} ]]; do
    want=$(($(zc_tip_height) - EPOCH_BASE + 1))
    [[ "$(height_of "${ENGINE_RPC_A}")" -eq "${want}" ]] && return 0
    maybe_check
    sleep 0.5
  done
  return 1
}

# Put the chain where an event wants it: A's tip a NULL block, a SEALED
# block, or anything ("any"). Sets TIP_KIND to what A's tip actually is.
TIP_KIND=""
prepare_tip() { # <null|sealed|any>
  local kind="$1" deadline=$((SECONDS + 120)) x
  case "${kind}" in
    sealed) ensure_burner 10 ;;
    null)
      if burner_running; then
        stop_burner
        # Flush a burn it may have left in the mempool.
        zc_mine 1
        settle_a 20 || true
      fi
      ;;
  esac
  while [[ ${SECONDS} -lt ${deadline} ]]; do
    [[ "${kind}" == "sealed" ]] && ensure_burner 10
    settle_a 20 || true
    x="$(extra_len "${ENGINE_RPC_A}" "$(height_of "${ENGINE_RPC_A}")")"
    case "${x}" in
      0) TIP_KIND=null ;;
      97) TIP_KIND=sealed ;;
      *) TIP_KIND="extra${x}" ;;
    esac
    [[ "${kind}" == "any" || "${kind}" == "${TIP_KIND}" ]] && return 0
    # Give the burner a moment to put its burn in the mempool first.
    sleep 1.5
    zc_mine 1
    maybe_check
  done
  return 1
}

# For a sealed epoch the burn must be in the mempool before the block.
wait_mempool() { # <timeout_s>
  local deadline=$((SECONDS + $1))
  while [[ ${SECONDS} -lt ${deadline} ]]; do
    [[ "$(zc_mempool_size)" -gt 0 ]] && return 0
    sleep 0.3
  done
  return 1
}

sapling_for() { # <mode none|first|all> <index 1..>
  case "$1" in
    all) echo "${SAPLING_ADDR}" ;;
    first) [[ "$2" -eq 1 ]] && echo "${SAPLING_ADDR}" ;;
    *) echo "" ;;
  esac
}

# invalidateblock at tip-depth+1 and mine depth+extra replacement blocks.
raw_reorg() { # <depth> <sapling mode> <extra>
  local depth="$1" sap="$2" extra="$3" tip fork old new i n
  tip="$(zc_tip_height)"
  fork=$((tip - depth + 1))
  old="$(zc_hash_at "${fork}")"
  zc_rpc invalidateblock "[\"${old}\"]" >/dev/null
  if [[ "$(zc_tip_height)" -ne $((fork - 1)) ]]; then
    tl "WARN invalidateblock ${fork} ${old}: zebrad tip $(zc_tip_height), want $((fork - 1))"
  fi
  n=$((depth + extra))
  for ((i = 1; i <= n; i++)); do
    zc_mine 1 "$(sapling_for "${sap}" "${i}")"
  done
  new="$(zc_hash_at "${fork}")"
  mark_reorg
  REORG_DESC="zcash ${fork}..${tip}: ${old:0:16}.. -> ${new:0:16}.. (+${n} blocks, sapling=${sap}), new tip $(zc_tip_height)"
}

# --- events --------------------------------------------------------------

ev_reorg() { # depth under sapling extra
  local depth="$1" under="$2" sap="$3" extra="$4" hA
  prepare_tip "${under}" || true
  hA="$(height_of "${ENGINE_RPC_A}")"
  raw_reorg "${depth}" "${sap}" "${extra}"
  tl "reorg depth=${depth} under=${under} (A tip ${hA} was ${TIP_KIND}) ${REORG_DESC}"
}

# Mine one Zcash block timed to become visible in the gap between node A's
# two Zcash poll loops: the expectations follower (which feeds the zcash
# index) polls a little BEFORE the sealer's follower (both every ~2 s; the
# gap, delta, is read from A's last pair of "unwound" / "reorg observed"
# lines, typically 10-150 ms). A block that appears inside the gap is seen by
# the sealer but not by the index until the index's next poll -- the 6225
# precondition. The sealer's next poll is predicted from its log (period
# estimated from its recent trigger lines); the block's visible time is aimed
# at predicted_poll - 5 ms - frac * (delta + 15 ms) (frac from the schedule). Z_A is
# mined just after the previous sealer poll and hidden with invalidateblock,
# then revealed with reconsiderblock one measured latency before the target
# (generate alone takes 0.2-0.5 s, too variable to aim). Prints a description.
AIM_DESC=""
aim_mine() { # <frac>
  AIM_DESC="$(python3 - "${A_LOG}" "$1" "${ZEBRAD_RPC}" "${WORK_DIR}/gen-latency" <<'PY'
import calendar, json, re, statistics, sys, time, urllib.request
log, frac, url, latf = sys.argv[1], float(sys.argv[2]), sys.argv[3] + "/", sys.argv[4]
ANSI = re.compile(r"\x1b\[[0-9;]*m")
def ts(line):
    t = line[:26]
    base, f = t.split(".")
    return calendar.timegm(time.strptime(base, "%Y-%m-%dT%H:%M:%S")) + float("0." + f)
sealer, unw, pairs = [], None, []
for raw in open(log, errors="replace"):
    line = ANSI.sub("", raw)
    if not line[:4].isdigit():
        continue
    if "engine::driver: sova epoch" in line or "engine::driver: zcash reorg observed" in line:
        t = ts(line)
        sealer.append(t)
        if "reorg observed" in line and unw is not None and 0 < t - unw < 0.5:
            pairs.append(t - unw)
        unw = None
    elif "expectations and candidates unwound" in line:
        unw = ts(line)
sealer = sealer[-12:]
periods = []
for a, b in zip(sealer, sealer[1:]):
    n = round((b - a) / 2.0)
    if n >= 1:
        periods.append((b - a) / n)
P = statistics.median(periods) if periods else 2.004
P = min(max(P, 1.99), 2.2)
delta = statistics.median(pairs[-5:]) if pairs else 0.05
try:
    lat = float(open(latf).read())
except Exception:
    lat = 0.01
now = time.time()
if sealer:
    T = sealer[-1]
    k = max(1, int((now + 0.4 - T) / P) + 1)
    poll = T + k * P
    # Measured: the sealer's poll starts ~5 ms after this prediction and the
    # index's ~delta before it, so aim inside [poll - delta - 20 ms, poll - 5 ms].
    target = poll - 0.005 - frac * (delta + 0.015)
def rpc(m, params):
    req = urllib.request.Request(url, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": params}).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=30)).get("result")
def sleep_until(t):
    while time.time() < t:
        time.sleep(min(0.005, max(0.0, t - time.time())))
if not sealer:
    rpc("generate", [1])
    print("aim: no sealer line yet; mined unaimed")
    sys.exit(0)
# Mine Z_A just after a sealer poll (both loops have just polled; ~1.9 s
# until either polls again), hide it at once with invalidateblock, then
# reveal it with reconsiderblock (fast, steady) at the aimed instant.
# generate alone takes 0.2-0.5 s and varies too much to aim with.
prev = poll - P
if prev - now < 0.05:
    prev, poll = poll, poll + P
    target = poll - 0.005 - frac * (delta + 0.015)
sleep_until(prev + 0.03)
t_gen = time.time()
h = rpc("generate", [1])[0]
rpc("invalidateblock", [h])
t_hid = time.time()
if t_hid > poll - delta - 0.15:
    print(f"aim: generate+hide took {1000*(t_hid - t_gen):.0f}ms, too slow for this poll; revealed at once")
sleep_until(target - lat)
t0 = time.time()
rpc("reconsiderblock", [h])
t1 = time.time()
open(latf, "w").write(f"{0.5 * lat + 0.5 * (t1 - t0):.4f}")
print(f"aimed: predicted sealer poll {poll:.3f} (period {P:.4f}), delta {delta*1000:.0f}ms, frac {frac}, "
      f"Z_A mined+hidden in {1000*(t_hid - t_gen):.0f}ms, revealed at {t1:.3f} = poll{(t1 - poll)*1000:+.0f}ms "
      f"(target {1000*(target - poll):+.0f}ms, reconsiderblock {1000*(t1 - t0):.0f}ms)")
PY
)"
}

ev_flipflop() { # depth under dwell_a dwell_b sapling_b flips dwell_c aim aim_frac
  local depth="$1" under="$2" da="$3" db="$4" sap="$5" flips="$6" dc="$7" aim="${8:-0}" frac="${9:-0.5}"
  [[ "${AIM_ALL}" == "1" ]] && aim=1
  local tip0 x y i hA mp="-"
  if [[ "${aim}" == "1" ]]; then
    # Aimed: depth 1, and Z_B must be visible before the index's next poll.
    depth=1
    da="$(python3 -c "print(min(${da}, 1.4))")"
  fi
  prepare_tip "${under}" || true
  if [[ "${under}" == "sealed" ]]; then
    # Z_A must carry A's burn: only a SEALED block is journaled, so only a
    # burn epoch can hit the 6225 shape. sova-miner burns once its previous
    # burn has confirmed (every other block), so mine until one is waiting.
    mp=no
    for _ in 1 2 3 4 5; do
      ensure_burner 10
      if wait_mempool 3; then
        mp=yes
        break
      fi
      zc_mine 1
      settle_a 20 || true
    done
    # A at the tip, so its next build is exactly Z_A's epoch.
    settle_a 20 || true
  fi
  tip0="$(zc_tip_height)"
  hA="$(height_of "${ENGINE_RPC_A}")"
  # A-branch: the next block(s), right as A is about to build.
  AIM_DESC=""
  if [[ "${aim}" == "1" ]]; then
    aim_mine "${frac}"
    NEXT_BLOCK=$((SECONDS + BLOCK_S))
  else
    zc_mine "${depth}"
  fi
  x="$(zc_hash_at $((tip0 + 1)))"
  sleep "${da}"
  zc_rpc invalidateblock "[\"${x}\"]" >/dev/null
  for ((i = 1; i <= depth; i++)); do
    if [[ "${sap}" == "1" ]]; then zc_mine 1 "${SAPLING_ADDR}"; else zc_mine 1; fi
  done
  y="$(zc_hash_at $((tip0 + 1)))"
  mark_reorg
  sleep "${db}"
  zc_rpc reconsiderblock "[\"${x}\"]" >/dev/null
  zc_rpc invalidateblock "[\"${y}\"]" >/dev/null
  mark_reorg
  local back ok=ok
  back="$(zc_hash_at $((tip0 + 1)))"
  [[ "${back}" == "${x}" && "$(zc_tip_height)" -eq $((tip0 + depth)) ]] || ok="FLIP-BACK-FAILED(tip $(zc_tip_height) hash ${back:0:16})"
  local third=""
  if [[ "${flips}" == "3" ]]; then
    sleep "${dc}"
    zc_rpc reconsiderblock "[\"${y}\"]" >/dev/null
    zc_rpc invalidateblock "[\"${x}\"]" >/dev/null
    mark_reorg
    third=" -> B again after ${dc}s (tip $(zc_hash_at $((tip0 + 1)) | cut -c1-16)..)"
  fi
  NEXT_BLOCK=$((SECONDS + BLOCK_S))
  tl "flipflop depth=${depth} under=${under} (A tip ${hA} was ${TIP_KIND}; burn in mempool: ${mp}) zcash $((tip0 + 1)): A=${x:0:16}.. ${da}s, B=${y:0:16}.. (sapling=${sap}) ${db}s, back to A: ${ok}${third}${AIM_DESC:+; ${AIM_DESC}}"
}

ev_keeper() { # reorg_while_down depth sapling down_s
  local rwd="$1" depth="$2" sap="$3" down_s="$4" h0 desc=""
  h0="$(height_of "${ENGINE_RPC_A}")"
  tl "keeper restart: SIGTERM node A (head ${h0})"
  node_down A
  stop_pid "${A_PID}" "node A"
  A_PID=""
  if [[ "${rwd}" == "1" ]]; then
    raw_reorg "${depth}" "${sap}" 0
    desc=" reorg while down: ${REORG_DESC};"
  fi
  pump "${down_s}"
  start_node_a || fail_stop "node A did not come back after a restart"
  node_up A
  tl "keeper restart: node A back (pid ${A_PID}, log ${A_LOG##*/}, head $(height_of "${ENGINE_RPC_A}")) after ${down_s}s down;${desc}"
}

ev_seed() { # down_s
  local down_s="$1" enode
  tl "seed restart: SIGTERM node B (head $(height_of "${ENGINE_RPC_B}"))"
  node_down B
  stop_pid "${B_PID}" "node B"
  B_PID=""
  pump "${down_s}"
  start_node_b || fail_stop "node B did not come back after a restart"
  node_up B
  enode="$(local_enode "${WORK_DIR}/node-b-$((B_GEN - 1)).log" || true)"
  [[ "${enode}" == "${ENODE_B}" ]] || tl "WARN node B came back with enode ${enode:-<none>} (was ${ENODE_B}); C's static peer is stale"
  tl "seed restart: node B back (pid ${B_PID}, head $(height_of "${ENGINE_RPC_B}")) after ${down_s}s down"
}

ev_burn() { ensure_burner "$1"; }

ev_tx() { # n
  local i raw resp hash
  for ((i = 0; i < $1; i++)); do
    raw="$(cast mktx --private-key "${TX_KEY}" --chain "${CHAIN_ID}" --nonce "${TX_NONCE}" --gas-limit 21000 \
      --gas-price 20gwei --priority-gas-price 1gwei "${SINK}" --value "$((TX_NONCE + 1))" 2>/dev/null)"
    resp="$(eth_rpc "${ENGINE_RPC_C}" eth_sendRawTransaction "[\"${raw}\"]")"
    hash="$(zc_json_result <<<"${resp}")"
    if [[ -z "${hash}" ]]; then
      fail_stop "(tx) node C refused tx nonce ${TX_NONCE}: ${resp}"
    fi
    echo "${hash} $(now_f) $(height_of "${ENGINE_RPC_C}")" >>"${WORK_DIR}/txs.txt"
    tl "tx nonce=${TX_NONCE} ${hash} -> C (C head $(height_of "${ENGINE_RPC_C}"))"
    TX_NONCE=$((TX_NONCE + 1))
  done
  ensure_burner 8
}

write_summary() { # <PASS|FAIL>
  local s="${WORK_DIR}/summary.txt" final
  final="$(run_check tick 2>&1 | head -1)"
  {
    echo "reorg-stress: $1"
    [[ -n "${FAIL_MSG}" ]] && echo "failure: ${FAIL_MSG}"
    echo "seed: ${SEED}   (reproduce: REORG_STRESS_SEED=${SEED} REORG_STRESS_MINUTES=${MINUTES} REORG_STRESS_MAX_DEPTH=${MAX_DEPTH} REORG_STRESS_AIM_ALL=${AIM_ALL} REORG_STRESS_GAP_S='${GAP_S}' REORG_STRESS_WEIGHTS='${WEIGHTS}' box/sim/reorg-stress-scenario.sh)"
    echo "binary: ${SOVA_BIN}"
    echo "duration: ${MINUTES} min stress, ran $((SECONDS - T0))s; events executed: ${EVENTS_DONE}/$(wc -l <"${WORK_DIR}/schedule.txt" | tr -d ' ')"
    echo "epoch base: ${EPOCH_BASE}; zcash tip $(zc_tip_height)"
    echo "last check: ${final}"
    echo "executed events by type:"
    grep -oE '^[^ ]+ \+[0-9]+s EVENT [0-9]+ [a-z]+' "${TIMELINE}" | awk '{print $5}' | sort | uniq -c | sed 's/^/  /'
    echo "zcash reorg actions (invalidate/reconsider flips): $(wc -l <"${WORK_DIR}/reorg-times.txt" 2>/dev/null | tr -d ' ')"
    echo "txs sent: ${TX_NONCE}; burner runs: ${BURNER_RUNS}"
    echo "log counters (per node log):"
    python3 - "${WORK_DIR}/check-state.json" <<'PY'
import json, sys
try:
    st = json.load(open(sys.argv[1]))
except Exception:
    st = {}
for f, c in sorted(st.get("counts", {}).items()):
    print("  " + f + ": " + ", ".join(f"{k}={v}" for k, v in sorted(c.items())))
print(f"  DeltaMismatch lines inside the post-reorg grace: {st.get('dm_tolerated', 0)}")
PY
  } >"${s}"
  echo ""
  cat "${s}"
}

# =========================================================================
# setup
# =========================================================================

preflight
start_stack
mkdir -p "${RUN_DIR}"
RS_WORK="${WORK_DIR}"
TIMELINE="${WORK_DIR}/timeline.txt"
: >"${TIMELINE}"
: >"${WORK_DIR}/txs.txt"
: >"${WORK_DIR}/reorg-times.txt"
: >"${WORK_DIR}/down"
echo ""
echo "=== reorg stress: seed ${SEED} (reproduce with REORG_STRESS_SEED=${SEED}), ${MINUTES} min, recovery allowance ${RECOVERY_MIN} min, run dir ${RUN_DIR} ==="
echo ""
tl "start seed=${SEED} minutes=${MINUTES} recovery_min=${RECOVERY_MIN} block_s=${BLOCK_S} max_depth=${MAX_DEPTH} aim_all=${AIM_ALL} gap=${GAP_S// /..} weights=${WEIGHTS// /,} bin=${SOVA_BIN}"

# Schedule, from the seed alone.
python3 - "${SEED}" "${MINUTES}" "${GAP_S}" "${WEIGHTS}" "${MAX_DEPTH}" >"${WORK_DIR}/schedule.txt" <<'PY'
import random, sys
seed, minutes = int(sys.argv[1]), float(sys.argv[2])
gmin, gmax = (float(x) for x in sys.argv[3].split())
weights = dict((k, float(v)) for k, v in (w.split("=") for w in sys.argv[4].split()))
# Depths are drawn first and clamped after, so a cap never changes the RNG
# sequence: the same seed gives the same schedule apart from the depths.
maxd = int(sys.argv[5])
r = random.Random(seed)
end = minutes * 60
t = 20.0
events = []
def pick(d):
    ks = list(d)
    return r.choices(ks, weights=[d[k] for k in ks])[0]
while t < end - 20:
    kind = pick(weights)
    if kind == "flipflop":
        p = [r.choices([1, 2], [3, 1])[0], r.choices(["sealed", "null"], [3, 1])[0],
             round(r.uniform(0.2, 2.5), 2), round(r.uniform(0.5, 8.0), 2), r.choice([0, 1]),
             r.choices([2, 3], [4, 1])[0], round(r.uniform(0.5, 5.0), 2),
             r.choice([0, 1]), round(r.uniform(0.0, 1.0), 2)]
    elif kind == "reorg1":
        kind, p = "reorg", [1, pick({"null": 4, "sealed": 4, "any": 2}),
                            pick({"none": 3, "first": 3, "all": 4}), r.choices([0, 1], [7, 3])[0]]
    elif kind == "reorgN":
        kind, p = "reorg", [r.randint(2, 4), pick({"null": 4, "sealed": 4, "any": 2}),
                            pick({"none": 3, "first": 3, "all": 4}), r.choices([0, 1], [7, 3])[0]]
    elif kind == "keeper":
        p = [r.choice([0, 1]), r.randint(1, 2), pick({"none": 1, "first": 1, "all": 1}), r.randint(2, 20)]
    elif kind == "burn":
        p = [r.randint(4, 12)]
    elif kind == "tx":
        p = [r.randint(1, 3)]
    else:
        raise SystemExit(f"unknown event kind {kind}")
    if kind in ("flipflop", "reorg"):
        p[0] = min(p[0], maxd)
    elif kind == "keeper":
        p[1] = min(p[1], maxd)
    events.append((t, kind, p))
    t += r.uniform(gmin, gmax)
# Exactly one seed restart, somewhere in the middle.
events.append((round(r.uniform(0.3, 0.7) * end, 1), "seed", [r.randint(5, 30)]))
events.sort(key=lambda e: e[0])
for i, (t, kind, p) in enumerate(events):
    print(i + 1, int(t), kind, *p)
PY
echo "--- schedule ($(wc -l <"${WORK_DIR}/schedule.txt" | tr -d ' ') events): ${WORK_DIR}/schedule.txt ---"
awk '{print $3}' "${WORK_DIR}/schedule.txt" | sort | uniq -c | sed 's/^/  /'

MINER_DATA_DIR="${WORK_DIR}/miner"
A_DATADIR="${WORK_DIR}/datadir-a"
B_DATADIR="${WORK_DIR}/datadir-b"
C_DATADIR="${WORK_DIR}/datadir-c"
mkdir -p "${MINER_DATA_DIR}" "${A_DATADIR}" "${B_DATADIR}" "${C_DATADIR}"
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
RS_EPOCH_BASE="${EPOCH_BASE}"
echo "epoch base B=${EPOCH_BASE} (Sova block N anchors Zcash N+${EPOCH_BASE}-1)"

echo "--- starting node A (keeper: mine mode, SIP-6 + SIP-7, sealing keystore, persistent datadir) ---"
start_node_a || exit 1
ENODE_A="$(local_enode "${A_LOG}")" || {
  fail "setup: node A never printed its enode"
  exit 1
}
echo "--- starting node B (seed: follow-only, static peer A, persistent datadir) ---"
start_node_b || exit 1
ENODE_B="$(local_enode "${WORK_DIR}/node-b-0.log")" || {
  fail "setup: node B never printed its enode"
  exit 1
}
echo "--- starting node C (rpc: follow-only, public RPC profile, static peer B only) ---"
start_node_c || exit 1

echo ""
echo "=== (0) setup ==="
check_p2p_node_log "node A" "${A_LOG}"
check_p2p_node_log "node B" "${WORK_DIR}/node-b-0.log"
check_p2p_node_log "node C" "${WORK_DIR}/node-c-0.log"
if log_has "${A_LOG}" "sip-6: sealing as" 15; then
  pass "setup: node A seals (SIP-6)"
else
  fail "setup: node A's log lacks 'sip-6: sealing as'"
fi
if log_has "${WORK_DIR}/node-c-0.log" "rpc profile: public" 15; then
  pass "setup: node C serves the public RPC profile"
else
  fail "setup: node C's log lacks 'rpc profile: public'"
fi
if wait_for_sova_peer_id "${A_LOG}" "$(enode_id "${ENODE_B}")" 60 \
  && wait_for_sova_peer "${WORK_DIR}/node-c-0.log" 60; then
  pass "setup: sova/1 sessions A<->B and B<->C established"
else
  fail "setup: sova/1 sessions not established within 60s"
  exit 1
fi
CHAIN_ID="$(eth_rpc "${ENGINE_RPC_C}" eth_chainId "[]" | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))")"
echo "chain id ${CHAIN_ID}"
zc_mine 1
if ! wait_for_block_number "${ENGINE_RPC_A}" 1 90 || ! wait_for_block_number "${ENGINE_RPC_C}" 1 90; then
  fail "setup: chain did not start (A=$(height_of "${ENGINE_RPC_A}") C=$(height_of "${ENGINE_RPC_C}"))"
  exit 1
fi
[[ "${FAILURES}" -eq 0 ]] || exit 1

# One burn stretch first so the chain holds sealed blocks and minted SOVA.
ensure_burner 4
pump 30

# =========================================================================
# stress
# =========================================================================

echo ""
echo "=== stress: ${MINUTES} min of scheduled events (seed ${SEED}) ==="
T_STRESS=${SECONDS}
END=$((T_STRESS + MINUTES * 60))
NEXT_CHECK=0
while read -r -u 3 idx at kind p1 p2 p3 p4 p5 p6 p7 p8 p9; do
  while [[ ${SECONDS} -lt $((T_STRESS + at)) ]]; do
    maybe_mine
    maybe_check
    sleep 0.5
  done
  [[ ${SECONDS} -ge ${END} ]] && break
  tl "EVENT ${idx} ${kind} ${p1:-} ${p2:-} ${p3:-} ${p4:-} ${p5:-} ${p6:-} ${p7:-} ${p8:-} ${p9:-}"
  case "${kind}" in
    reorg) ev_reorg "${p1}" "${p2}" "${p3}" "${p4}" ;;
    flipflop) ev_flipflop "${p1}" "${p2}" "${p3}" "${p4}" "${p5}" "${p6}" "${p7}" "${p8}" "${p9}" ;;
    keeper) ev_keeper "${p1}" "${p2}" "${p3}" "${p4}" ;;
    seed) ev_seed "${p1}" ;;
    burn) ev_burn "${p1}" ;;
    tx) ev_tx "${p1}" ;;
  esac
  EVENTS_DONE=$((EVENTS_DONE + 1))
  maybe_check
done 3<"${WORK_DIR}/schedule.txt"
while [[ ${SECONDS} -lt ${END} ]]; do
  maybe_mine
  maybe_check
  sleep 0.5
done

# =========================================================================
# settle, then the strict final check
# =========================================================================

echo ""
echo "=== settle: no more events; burns on; convergence within ${RECOVERY_MIN} min ==="
tl "settle"
DEADLINE=$((SECONDS + RECOVERY_MIN * 60))
CONVERGED=0
while [[ ${SECONDS} -lt ${DEADLINE} ]]; do
  ensure_burner 6
  pump 5
  # pump keeps mining, so a check right after it always lands on a Zcash
  # block the nodes haven't built yet: wait for A to reach the tip, then
  # give the followers a moment, before judging convergence.
  settle_a 10 || true
  sleep 2
  out="$(run_check converged 2>&1)"
  rc=$?
  echo "$(date -u +%H:%M:%S) +$((SECONDS - T0))s ${out}" >>"${WORK_DIR}/checks.log"
  [[ ${rc} -eq 1 ]] && fail_stop "invariant violated while settling: $(grep '^VIOLATION' <<<"${out}" | head -3 | tr '\n' ' ')"
  if [[ ${rc} -eq 0 ]]; then
    CONVERGED=1
    break
  fi
done
[[ "${CONVERGED}" == "1" ]] || fail_stop "no convergence within ${RECOVERY_MIN} min after the last event: $(run_check tick 2>&1 | head -1)"
stop_burner
# Freeze Zcash and let every node reach the same head before the strict check.
sleep 3
for _ in $(seq 1 30); do
  run_check converged >/dev/null 2>&1 && break
  sleep 2
done
out="$(run_check final 2>&1)"
rc=$?
echo "$(date -u +%H:%M:%S) +$((SECONDS - T0))s ${out}" >>"${WORK_DIR}/checks.log"
echo "${out}"
dump_chains final
if [[ ${rc} -ne 0 ]]; then
  fail_stop "final check: $(grep '^VIOLATION' <<<"${out}" | head -3 | tr '\n' ' ')"
fi
pass "final: A, B and C agree on every block 1..head, every anchor is zebrad's, every tx has the same receipt on all three"
tl "PASS"
write_summary "PASS"
echo ""
echo "REORG STRESS SCENARIO PASSED (seed ${SEED}, ${MINUTES} min, ${EVENTS_DONE} events)"
[[ "${FAILURES}" -eq 0 ]]
