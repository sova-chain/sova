#!/usr/bin/env bash
# C6 simulation harness: proves follower-layer determinism against a real
# zebrad regtest node, using `consensus_sim` (crates/consensus/src/bin/
# consensus_sim.rs) as the replay probe. See box/sim/README.md for what
# this proves today and the plan to extend it to full Sova nodes once
# C3's async Engine-API loop lands.
#
# Reuses box/regtest's compose stack (a fresh one: `down -v` then `up -d`,
# same as box/regtest/follower-e2e.sh) and box/regtest's mine.sh/
# auto-mine.sh, and reuses the D2 miner CLI (crates/burn-wallet/miner,
# binary `sova-miner`) to produce real SIP-1 burns the same way
# box/regtest/miner-ac.sh does (fund one coinbase, mature it with 100
# blocks to an unrelated address, then mine epochs while auto-mine drives
# the chain forward).
#
# Four scenarios. 1-3 run in order against ONE continuously-running stack
# (scenario 2 reorgs scenario 1's own chain; scenario 3 replays scenario
# 2's final chain), torn down once that trio finishes regardless of
# outcome. Scenario 4 then gets its own fresh stack and is delegated to
# box/sim/mint-scenario.sh (self-contained: own stack lifecycle, own
# bin/sova process, own EXIT-trap teardown) -- see that script's own doc
# comment for why it needs a separate process, and box/sim/README.md's
# "Extension plan" note on what's still gated behind gossip.
#
#   1. parallel determinism  -- two consensus_sim instances, run
#      concurrently against the same live chain, must agree.
#   2. reorg convergence     -- invalidate the chain's tip block, mine a
#      replacement branch, then two fresh instances must agree with each
#      other AND must disagree with scenario 1 (proving the reorg
#      actually changed the stream, not that the harness is a no-op).
#   3. restart equivalence   -- run consensus_sim, then run it again as a
#      brand new process against the same (now static) chain; must agree.
#   4. live mint invariants  -- a single real bin/sova node, in mine mode,
#      minting from real SIP-1 burns: eth_blockNumber stays in lockstep
#      with the Zcash tip, a burn's settled epoch mints exactly one
#      epoch's reward, a second burn in a later epoch mints exactly
#      another one (cumulative), and the node's own log shows settled=true
#      for exactly those two epochs. The first C6 coverage of a real node
#      binary end to end, not just the follower layer.
#
# Each scenario prints PASS/FAIL. Exits non-zero if any scenario failed.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../.." && pwd)"
REGTEST_DIR="${ROOT}/box/regtest"
BURN_WALLET_DIR="${ROOT}/crates/burn-wallet"

RPC_URL="${SOVA_REGTEST_RPC:-http://127.0.0.1:18232}"
COMPOSE="docker compose"

# zebrad.toml's hard-coded default Regtest miner address (see
# box/regtest/zebrad.toml's [mining] section) -- unrelated to the miner's
# own keystore, used only as a dumping ground for non-funding blocks. Also
# doubles as the plain-block miner for scenario 1's auto-mine-driven fill
# and scenario 2's replacement branch.
THROWAWAY_ADDRESS="tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"

MINER_EPOCHS=3
MINER_PER_EPOCH_ZAT=100000
MINER_BUDGET_ZAT=500000
# Total new blocks (post-funding) scenario 1 grows the chain by, burns
# sprinkled in among them by the miner's ${MINER_EPOCHS} epochs.
SCENARIO1_NEW_BLOCKS=30
# Scenario 2 invalidates the chain's current tip block (depth 1) and mines
# this many replacement blocks on top -- deliberately the same shallow,
# proven-safe pattern crates/consensus/tests/follower_regtest.rs's C2a
# acceptance test uses (invalidate the tip, `generate` more than 1 block
# on top), NOT a deeper mid-chain target. See box/sim/README.md's "Zebra
# reliability finding": a deeper invalidateblock target, combined with a
# second miner racing blocks in concurrently, reproducibly panicked
# zebra-state 6.3.0 (`non_finalized_state.rs:1016`,
# `update_metrics_bars`'s `.expect("just checked recent fork height")`).
# That race was a bug in an earlier version of this harness script (a
# leaked background auto-mine.sh), now fixed -- but the shallow,
# already-AC-proven invalidate depth is kept as the safer default even so.
REPLACEMENT_BLOCKS=3

STARTED_STACK=0
AUTO_MINE_PID=""
WORK_DIR=""
FAILURES=0

pass() { echo "PASS: $*" >&2; }
fail() {
  echo "FAIL: $*" >&2
  FAILURES=$((FAILURES + 1))
}

cleanup() {
  local exit_code=$?
  if [[ -n "${AUTO_MINE_PID}" ]] && kill -0 "${AUTO_MINE_PID}" 2>/dev/null; then
    echo "--- stopping auto-mine (pid ${AUTO_MINE_PID}) ---"
    kill "${AUTO_MINE_PID}" 2>/dev/null || true
    wait "${AUTO_MINE_PID}" 2>/dev/null || true
  fi
  if [[ "${exit_code}" -ne 0 || "${FAILURES}" -gt 0 ]]; then
    echo "--- run-scenarios failed (exit ${exit_code}, ${FAILURES} scenario failure(s)); zebrad logs follow ---" >&2
    (cd "${REGTEST_DIR}" && ${COMPOSE} logs --no-color zebrad) >&2 || true
  fi
  if [[ "${STARTED_STACK}" -eq 1 ]]; then
    echo "--- tearing down stack ---"
    (cd "${REGTEST_DIR}" && ${COMPOSE} down -v) >/dev/null 2>&1 || true
  fi
  if [[ -n "${WORK_DIR}" && -d "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
  if [[ "${exit_code}" -eq 0 && "${FAILURES}" -gt 0 ]]; then
    exit 1
  fi
  exit "${exit_code}"
}
trap cleanup EXIT

# ---------------------------------------------------------------------
# RPC helpers
# ---------------------------------------------------------------------

rpc_call() {
  local method="$1" params="$2"
  curl -s -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"sim\",\"method\":\"${method}\",\"params\":${params}}" \
    "${RPC_URL}/"
}

tip_height() {
  rpc_call getblockcount "[]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'])"
}

block_hash_at() {
  local h="$1"
  rpc_call getblockhash "[${h}]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'])"
}

generate_to_address() {
  local n="$1" addr="$2"
  rpc_call generatetoaddress "[${n}, \"${addr}\"]" >/dev/null
}

invalidate_block() {
  local hash="$1"
  rpc_call invalidateblock "[\"${hash}\"]" >/dev/null
}

wait_for_tip_at_least() {
  local target="$1" timeout_s="${2:-180}"
  local deadline=$((SECONDS + timeout_s))
  local t
  while true; do
    t="$(tip_height)"
    if [[ "${t}" =~ ^[0-9]+$ ]] && [[ "${t}" -ge "${target}" ]]; then
      echo "${t}"
      return 0
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "error: tip did not reach ${target} within ${timeout_s}s (last=${t:-?})" >&2
      return 1
    fi
    sleep 1
  done
}

# Assert the tip is not moving -- every scenario below compares stream
# digests on the premise that the chain is static while consensus_sim
# runs. This turns that premise into a checked precondition instead of an
# assumption (a leaked background miner silently violates it otherwise).
assert_tip_quiescent() {
  # Retries rather than failing on the first observed change: a miner we
  # just asked to stop may have one RPC call already in flight (the block
  # lands after `kill` returns, before the process actually exits), which
  # is a benign one-block straggler, not "still mining." Only a tip that
  # keeps moving across every check is a real precondition violation.
  local label="$1" settle_s="${2:-3}" max_checks="${3:-5}"
  local last cur check=0
  last="$(tip_height)"
  while [[ "${check}" -lt "${max_checks}" ]]; do
    sleep "${settle_s}"
    cur="$(tip_height)"
    if [[ "${cur}" == "${last}" ]]; then
      return 0
    fi
    last="${cur}"
    check=$((check + 1))
  done
  fail "${label}: chain tip kept advancing across ${max_checks} quiescence checks (last=${last}) — something is still mining; scenario precondition violated"
  return 1
}

# ---------------------------------------------------------------------
# consensus_sim helpers
# ---------------------------------------------------------------------

run_sim_capture() {
  # $1 = output file. Writes stdout there, stderr alongside; returns the
  # binary's own exit code.
  local out_file="$1"
  "${CONSENSUS_SIM_BIN}" "${RPC_URL}" >"${out_file}" 2>"${out_file}.err"
}

digest_of() {
  grep '^STREAM_DIGEST ' "$1" | awk '{print $2}'
}

epoch_line_count() {
  grep -c -v '^STREAM_DIGEST ' "$1"
}

# Run two consensus_sim instances concurrently against the live chain and
# report whether they agree. Echoes the agreed digest on stdout if so.
run_concurrent_pair() {
  local tag="$1" out_a="$2" out_b="$3"
  run_sim_capture "${out_a}" &
  local pid_a=$!
  run_sim_capture "${out_b}" &
  local pid_b=$!
  local rc_a=0 rc_b=0
  wait "${pid_a}" || rc_a=$?
  wait "${pid_b}" || rc_b=$?

  if [[ "${rc_a}" -ne 0 || "${rc_b}" -ne 0 ]]; then
    fail "${tag}: consensus_sim exited nonzero (a=${rc_a}, b=${rc_b}); stderr follows"
    cat "${out_a}.err" >&2 || true
    cat "${out_b}.err" >&2 || true
    return 1
  fi

  local da db
  da="$(digest_of "${out_a}")"
  db="$(digest_of "${out_b}")"
  if [[ -z "${da}" || "${da}" != "${db}" ]]; then
    fail "${tag}: STREAM_DIGEST mismatch between concurrent instances (a=${da:-<none>}, b=${db:-<none>})"
    return 1
  fi
  local n_epochs
  n_epochs="$(epoch_line_count "${out_a}")"
  pass "${tag}: two concurrent consensus_sim instances agree — STREAM_DIGEST ${da} (${n_epochs} epoch lines)"
  echo "${da}"
}

# ---------------------------------------------------------------------
# Stack lifecycle
# ---------------------------------------------------------------------

echo "--- checking docker for a pre-existing harness ---"
docker ps -a --filter "name=sova-zebrad-regtest" --format '{{.Names}}\t{{.Status}}'

echo "--- checking for a leaked auto-mine.sh from a previous run ---"
# A prior run's background auto-mine.sh, if ever backgrounded through a
# `(...) &` subshell wrapper, can outlive that run's own cleanup (`kill`
# on the subshell's PID does not reach its child) and keep calling
# `generate` against this same RPC URL indefinitely. Two independent
# miners racing blocks into zebrad like that isn't just noisy: it can
# trigger a genuine zebra-state concurrency panic (observed once during
# this harness's development — see box/sim/README.md's Zebra finding).
# Belt and suspenders: hunt down and kill any stray one before starting.
LEAKED_AUTO_MINE_PIDS="$(pgrep -f "auto-mine\.sh.*${RPC_URL##*/}" 2>/dev/null || true)"
if [[ -n "${LEAKED_AUTO_MINE_PIDS}" ]]; then
  echo "warning: killing leaked auto-mine.sh process(es): ${LEAKED_AUTO_MINE_PIDS}" >&2
  # shellcheck disable=SC2086
  kill ${LEAKED_AUTO_MINE_PIDS} 2>/dev/null || true
  sleep 1
fi

echo "--- starting a fresh regtest stack ---"
(cd "${REGTEST_DIR}" && ${COMPOSE} down -v) >/dev/null 2>&1 || true
(cd "${REGTEST_DIR}" && ${COMPOSE} up -d)
STARTED_STACK=1

echo "--- waiting for zebrad RPC readiness (healthcheck) ---"
deadline=$((SECONDS + 120))
until [[ "$(docker inspect -f '{{.State.Health.Status}}' sova-zebrad-regtest 2>/dev/null)" == "healthy" ]]; do
  if [[ ${SECONDS} -ge ${deadline} ]]; then
    echo "error: zebrad RPC did not become healthy within 120s" >&2
    exit 1
  fi
  sleep 2
done
echo "zebrad RPC is healthy"

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/sova-sim.XXXXXX")"
echo "--- work dir: ${WORK_DIR} ---"

echo "--- building consensus_sim (release) ---"
(cd "${ROOT}" && cargo build --release -p consensus --bin consensus_sim)
CONSENSUS_SIM_BIN="${ROOT}/target/release/consensus_sim"

echo "--- building sova-miner (release, nested burn-wallet workspace) ---"
(cd "${BURN_WALLET_DIR}" && cargo build --release -p sova-miner)
MINER_BIN="${BURN_WALLET_DIR}/target/release/sova-miner"

# ============================================================
# SCENARIO 1: parallel determinism
# ============================================================
echo ""
echo "=== SCENARIO 1: parallel determinism ==="

MINER_DATA_DIR="${WORK_DIR}/miner"
mkdir -p "${MINER_DATA_DIR}"
"${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest init
MINER_ADDRESS="$(python3 -c "import json;print(json.load(open('${MINER_DATA_DIR}/state.json'))['address'])")"
echo "miner address: ${MINER_ADDRESS}"

echo "--- funding: 1 block to the miner, then 100 blocks elsewhere (coinbase maturity) ---"
generate_to_address 1 "${MINER_ADDRESS}"
generate_to_address 100 "${THROWAWAY_ADDRESS}"
FUNDING_TIP="$(tip_height)"
echo "tip after funding: ${FUNDING_TIP} (expected 101)"

echo "--- starting auto-mine (1 block every 2s) in the background ---"
# Invoked directly (NOT wrapped in a `(cd ... && ...) &` subshell): `$!`
# must be auto-mine.sh's own PID, or `kill "${AUTO_MINE_PID}"` later only
# kills a subshell wrapper and orphans the real auto-mine.sh loop, which
# then keeps calling `generate` indefinitely -- silently violating every
# later scenario's "the chain is static" assumption.
"${REGTEST_DIR}/auto-mine.sh" 2 "${RPC_URL}" >"${WORK_DIR}/auto-mine.log" 2>&1 &
AUTO_MINE_PID=$!

echo "--- running sova-miner mine for ${MINER_EPOCHS} epochs (a few SIP-1 burns sprinkled into the chain) ---"
if ! "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
  --budget-zat "${MINER_BUDGET_ZAT}" \
  --per-epoch-zat "${MINER_PER_EPOCH_ZAT}" \
  --rpc "${RPC_URL}" \
  --poll-interval-ms 300 \
  --max-epochs "${MINER_EPOCHS}"; then
  fail "scenario 1 setup: sova-miner mine failed"
fi

SCENARIO1_TARGET=$((FUNDING_TIP + SCENARIO1_NEW_BLOCKS))
echo "--- waiting for the chain to reach ${SCENARIO1_TARGET} (auto-mine filling in plain blocks) ---"
SCENARIO1_TIP="$(wait_for_tip_at_least "${SCENARIO1_TARGET}" 180)" || fail "scenario 1 setup: chain did not reach ${SCENARIO1_TARGET}"

echo "--- stopping auto-mine ---"
kill "${AUTO_MINE_PID}" 2>/dev/null || true
wait "${AUTO_MINE_PID}" 2>/dev/null || true
AUTO_MINE_PID=""

echo "--- confirming the chain is quiescent before comparing ---"
assert_tip_quiescent "scenario 1 (parallel determinism)" 3

echo "scenario 1 final tip: ${SCENARIO1_TIP}"
echo "--- running two concurrent consensus_sim instances ---"
DIGEST1="$(run_concurrent_pair "scenario 1 (parallel determinism)" "${WORK_DIR}/s1_a.out" "${WORK_DIR}/s1_b.out")"

# ============================================================
# SCENARIO 2: reorg convergence
# ============================================================
echo ""
echo "=== SCENARIO 2: reorg convergence ==="

# Re-read the tip fresh rather than trusting the value scenario 1 captured
# earlier -- nothing should have mined since, but this is cheap insurance
# against exactly the kind of drift that motivated assert_tip_quiescent.
REORG_HEIGHT="$(tip_height)"
echo "invalidating the current tip (height ${REORG_HEIGHT}); replacing with ${REPLACEMENT_BLOCKS} new blocks"

REORG_HASH="$(block_hash_at "${REORG_HEIGHT}")"
if [[ -z "${REORG_HASH}" || "${REORG_HASH}" == "None" ]]; then
  fail "scenario 2 setup: could not fetch block hash at height ${REORG_HEIGHT}"
else
  invalidate_block "${REORG_HASH}"
  TIP_AFTER_INVALIDATE="$(tip_height)"
  EXPECTED_TIP_AFTER_INVALIDATE=$((REORG_HEIGHT - 1))
  if [[ "${TIP_AFTER_INVALIDATE}" != "${EXPECTED_TIP_AFTER_INVALIDATE}" ]]; then
    fail "scenario 2 setup: expected tip ${EXPECTED_TIP_AFTER_INVALIDATE} after invalidateblock, got ${TIP_AFTER_INVALIDATE}"
  fi

  echo "--- mining the replacement branch ---"
  generate_to_address "${REPLACEMENT_BLOCKS}" "${THROWAWAY_ADDRESS}"
  SCENARIO2_TARGET=$((EXPECTED_TIP_AFTER_INVALIDATE + REPLACEMENT_BLOCKS))
  SCENARIO2_TIP="$(wait_for_tip_at_least "${SCENARIO2_TARGET}" 120)" || fail "scenario 2 setup: chain did not reach ${SCENARIO2_TARGET}"
  echo "scenario 2 final tip: ${SCENARIO2_TIP} (was ${SCENARIO1_TIP} before the reorg)"

  echo "--- confirming the chain is quiescent before comparing ---"
  assert_tip_quiescent "scenario 2 (reorg convergence)" 3

  echo "--- running two fresh concurrent consensus_sim instances over the reorged chain ---"
  DIGEST2="$(run_concurrent_pair "scenario 2 (reorg convergence)" "${WORK_DIR}/s2_a.out" "${WORK_DIR}/s2_b.out")"

  if [[ -n "${DIGEST1:-}" && -n "${DIGEST2:-}" ]]; then
    if [[ "${DIGEST1}" == "${DIGEST2}" ]]; then
      fail "scenario 2 (reorg convergence): STREAM_DIGEST unchanged after the reorg (${DIGEST2}) — the reorg did not change the observed stream"
    else
      pass "scenario 2 (reorg convergence): STREAM_DIGEST differs from scenario 1 (${DIGEST1} -> ${DIGEST2}), proving the reorg changed the stream"
    fi
  fi
fi

# ============================================================
# SCENARIO 3: restart equivalence
# ============================================================
echo ""
echo "=== SCENARIO 3: restart equivalence ==="
echo "--- confirming the chain is quiescent before comparing ---"
assert_tip_quiescent "scenario 3 (restart equivalence)" 3
echo "--- running consensus_sim once ---"
RC3A=0
run_sim_capture "${WORK_DIR}/s3_a.out" || RC3A=$?
echo "--- running consensus_sim again, as a brand new process, against the same static chain ---"
sleep 1
RC3B=0
run_sim_capture "${WORK_DIR}/s3_b.out" || RC3B=$?

if [[ "${RC3A}" -ne 0 || "${RC3B}" -ne 0 ]]; then
  fail "scenario 3 (restart equivalence): consensus_sim exited nonzero (run1=${RC3A}, run2=${RC3B})"
  cat "${WORK_DIR}/s3_a.out.err" >&2 || true
  cat "${WORK_DIR}/s3_b.out.err" >&2 || true
else
  DIGEST3A="$(digest_of "${WORK_DIR}/s3_a.out")"
  DIGEST3B="$(digest_of "${WORK_DIR}/s3_b.out")"
  if [[ -z "${DIGEST3A}" || "${DIGEST3A}" != "${DIGEST3B}" ]]; then
    fail "scenario 3 (restart equivalence): STREAM_DIGEST mismatch across restart (run1=${DIGEST3A:-<none>}, run2=${DIGEST3B:-<none>})"
  else
    pass "scenario 3 (restart equivalence): sequential fresh-process runs agree — STREAM_DIGEST ${DIGEST3A}"
  fi
fi

# ============================================================
# SCENARIO 4: live mint invariants (Act I regression)
# ============================================================
echo ""
echo "=== SCENARIO 4: live mint invariants ==="
echo "--- tearing down the scenarios 1-3 stack (scenario 4 needs its own fresh chain from height 1, and its own bin/sova process on ${ENGINE_RPC:-127.0.0.1:8545}) ---"
if [[ -n "${AUTO_MINE_PID}" ]] && kill -0 "${AUTO_MINE_PID}" 2>/dev/null; then
  kill "${AUTO_MINE_PID}" 2>/dev/null || true
  wait "${AUTO_MINE_PID}" 2>/dev/null || true
  AUTO_MINE_PID=""
fi
(cd "${REGTEST_DIR}" && ${COMPOSE} down -v) >/dev/null 2>&1 || true
STARTED_STACK=0

echo "--- delegating to box/sim/mint-scenario.sh (own stack, own bin/sova process, own teardown) ---"
MINT_SCENARIO_RC=0
"${HERE}/mint-scenario.sh" || MINT_SCENARIO_RC=$?
if [[ "${MINT_SCENARIO_RC}" -ne 0 ]]; then
  fail "scenario 4 (live mint invariants): mint-scenario.sh exited nonzero (rc=${MINT_SCENARIO_RC}) -- see its own PASS/FAIL lines above for which assertion(s) failed"
fi

# ---------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------
echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "ALL SCENARIOS PASSED"
else
  echo "${FAILURES} SCENARIO(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
