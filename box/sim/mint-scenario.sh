#!/usr/bin/env bash
# C6 SCENARIO 4: live mint invariants (Act I regression).
#
# Turns the ad-hoc "does a real bin/sova mine-mode node actually mint from
# a real Zcash burn" proof (run by hand against this repo on 2026-09-21,
# see the PASS evidence in box/sim/README.md) into a durable, from-cold,
# twice-green regression. Unlike scenarios 1-3 (box/sim/run-scenarios.sh),
# which only exercise the follower layer via `consensus_sim`, this
# scenario runs a real `bin/sova` node in mine mode -- the first C6
# coverage of the actual node binary end to end: real zebrad regtest, a
# real SIP-1 burn via the D2 `sova-miner` CLI, the real async sealer loop
# (crates/engine/src/driver.rs), and a real EVM balance read over
# `eth_getBalance`. Self-contained: its own fresh regtest stack, its own
# `bin/sova` process, its own EXIT-trap teardown. Runnable standalone
# (`box/sim/mint-scenario.sh`) or wired into `run-scenarios.sh` as
# scenario 4.
#
# What it proves:
#
#   (a) lockstep — a running mine-mode node's `eth_blockNumber` tracks the
#       live Zcash tip (one Sova block per Zcash block, per bin/sova's own
#       mine-mode doc comment), sampled 3 times over ~15s while the chain
#       keeps advancing, allowing at most 1 block of lag.
#   (b) first mint — after a real SIP-1 burn's epoch settles, the miner's
#       EVM address balance is EXACTLY one epoch's reward: 6,250 SOVA in
#       wei. The expected value is derived from, and guard-checked
#       against, `DRAFT_EPOCH_REWARD_GWEI` in crates/engine/src/driver.rs
#       (see EXPECTED_REWARD_GUARD below) so if that constant ever
#       changes, this test fails loudly instead of silently checking a
#       stale number.
#   (c) repeated settlement — a second burn, in a later epoch, mints
#       exactly another 6,250 SOVA (cumulative balance 12,500 SOVA in
#       wei). This is the regression that matters most: it proves
#       settlement isn't a one-shot fluke of the first epoch.
#   (d) log invariant — the node's own log contains `settled=true` lines
#       for exactly the two burn epochs (count = 2), no more, no fewer.
#
# A real observed nondeterminism, documented rather than hidden: during
# development, the settling `sova epoch trigger settled=true` log line did
# not always line up 1:1 with a quiet log stream -- reth's own trigger-mode
# `LocalMiner` occasionally logged a transient "Error updating fork choice:
# ... too deep reorg" / "Received invalid forkchoice updated message"
# pair immediately before the correct settled=true trigger fired anyway,
# self-recovering within the same poll interval. The single-slot
# `PendingEpoch` mailbox (engine::PendingEpoch; see crates/engine/src/
# driver.rs's `SealerCore::process` doc comment) also means, in principle,
# that the Sova block height carrying a mint need not equal the Zcash
# epoch height number that produced it -- this scenario's assertions
# therefore poll for the *balance* and *log content* outcomes rather than
# asserting a specific block height mints a specific epoch. See this
# scenario's run output / box/sim/README.md's evidence block for whether
# either was actually observed on a given green run.
#
# Always tears its stack down on exit (success or failure), and kills any
# stray `bin/sova` process it started, dumping zebrad + sova-node log
# tails on failure for diagnosis.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../.." && pwd)"
REGTEST_DIR="${ROOT}/box/regtest"
BURN_WALLET_DIR="${ROOT}/crates/burn-wallet"

RPC_URL="${SOVA_REGTEST_RPC:-http://127.0.0.1:18232}"
ENGINE_RPC="${SOVA_ENGINE_RPC:-http://127.0.0.1:8545}"
COMPOSE="docker compose"

SOVA_BIN="${ROOT}/target/debug/sova"
MINER_BIN="${BURN_WALLET_DIR}/target/release/sova-miner"

# zebrad.toml's own hard-coded default Regtest miner address -- unused
# here (funding goes straight to the miner's own t-addr, matching the
# proven ad-hoc flow), kept only for parity/reference with the other box
# scripts that DO use it as a throwaway sink.
# THROWAWAY_ADDRESS="tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"

# NOTE (D5): `--budget-zat` is PER-INVOCATION -- each `mine` call below
# snapshots MinerState's lifetime spend at startup and measures its own
# budget only against what THAT run spends
# (crates/burn-wallet/miner/src/state.rs's `begin_invocation`/
# `budget_remaining_zat`), not the whole state.json sidecar's running
# total. So burn 1 and burn 2 below each get their own fresh 200,000 zat
# budget, independent of what the other one already spent -- no need to
# size this for their *cumulative* cost the way earlier builds required
# (see box/sim/README.md's "script bug found and fixed" for the pre-D5
# lifetime-accounting surprise that originally forced BUDGET_ZAT up to
# 500,000 here). 200,000 comfortably covers one epoch's 120,000 zat cost
# (100,000 burn + 20,000 fee) with margin, matching box/regtest/
# miner-ac.sh's and run-scenarios.sh's own generous-per-epoch-budget
# convention. Anyone who wants the old cumulative-across-runs cap back can
# pass `--lifetime-budget-zat` explicitly (crates/burn-wallet/miner/
# README.md's "Budgets" section) -- unused here since this scenario
# already sizes each invocation's own budget generously.
BUDGET_ZAT=200000
PER_EPOCH_ZAT=100000
AUTO_MINE_INTERVAL_S=2

STARTED_STACK=0
SOVA_PID=""
AUTO_MINE_PID=""
WORK_DIR=""
FAILURES=0

pass() { echo "PASS: scenario 4 (live mint invariants): $*" >&2; }
fail() {
  echo "FAIL: scenario 4 (live mint invariants): $*" >&2
  FAILURES=$((FAILURES + 1))
}

cleanup() {
  local exit_code=$?
  if [[ -n "${AUTO_MINE_PID}" ]] && kill -0 "${AUTO_MINE_PID}" 2>/dev/null; then
    echo "--- stopping auto-mine (pid ${AUTO_MINE_PID}) ---"
    kill "${AUTO_MINE_PID}" 2>/dev/null || true
    wait "${AUTO_MINE_PID}" 2>/dev/null || true
  fi
  if [[ -n "${SOVA_PID}" ]] && kill -0 "${SOVA_PID}" 2>/dev/null; then
    echo "--- stopping bin/sova (pid ${SOVA_PID}) ---"
    kill "${SOVA_PID}" 2>/dev/null || true
    wait "${SOVA_PID}" 2>/dev/null || true
  fi
  # Belt and suspenders: a process this script itself lost track of (e.g.
  # this script was killed before SOVA_PID/AUTO_MINE_PID were captured).
  pkill -f "target/debug/sova$" 2>/dev/null || true
  if [[ -n "${WORK_DIR}" ]]; then
    pkill -f "sova-miner --data-dir ${WORK_DIR}" 2>/dev/null || true
  fi

  if [[ "${exit_code}" -ne 0 || "${FAILURES}" -gt 0 ]]; then
    echo "--- scenario 4 failed (exit ${exit_code}, ${FAILURES} assertion failure(s)); logs follow ---" >&2
    if [[ -n "${WORK_DIR}" && -f "${WORK_DIR}/sova-node.log" ]]; then
      echo "--- bin/sova log (last 60 lines) ---" >&2
      tail -n 60 "${WORK_DIR}/sova-node.log" >&2 || true
    fi
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
# RPC helpers (Zcash / zebrad)
# ---------------------------------------------------------------------

rpc_call() {
  local method="$1" params="$2"
  curl -s -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"mint\",\"method\":\"${method}\",\"params\":${params}}" \
    "${RPC_URL}/"
}

tip_height() {
  rpc_call getblockcount "[]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'])"
}

generate_to_address() {
  local n="$1" addr="$2"
  rpc_call generatetoaddress "[${n}, \"${addr}\"]" >/dev/null
}

# ---------------------------------------------------------------------
# RPC helpers (Sova EVM / bin/sova)
# ---------------------------------------------------------------------

eth_rpc() {
  local method="$1" params="$2"
  curl -s -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"mint\",\"method\":\"${method}\",\"params\":${params}}" \
    "${ENGINE_RPC}"
}

eth_block_number() {
  eth_rpc eth_blockNumber "[]" | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))"
}

eth_balance_wei() {
  local addr="$1"
  eth_rpc eth_getBalance "[\"${addr}\",\"latest\"]" \
    | python3 -c "import sys,json;print(int(json.load(sys.stdin)['result'],16))"
}

# Poll eth_getBalance(addr) until it differs (as a decimal string) from
# $2, or timeout. Prints the final observed balance on stdout; return code
# 0 if it changed, 1 on timeout. Pure string comparison -- these balances
# (thousands of SOVA in wei) are far past bash's 64-bit arithmetic range,
# so no numeric comparison is done here.
wait_for_balance_change() {
  local addr="$1" baseline="$2" timeout_s="${3:-90}"
  local deadline=$((SECONDS + timeout_s)) bal
  while true; do
    bal="$(eth_balance_wei "${addr}" 2>/dev/null || true)"
    if [[ -n "${bal}" && "${bal}" != "${baseline}" ]]; then
      echo "${bal}"
      return 0
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "${bal:-${baseline}}"
      return 1
    fi
    sleep 2
  done
}

# ---------------------------------------------------------------------
# Guard-check: EXPECTED_REWARD_WEI must track DRAFT_EPOCH_REWARD_GWEI
# ---------------------------------------------------------------------
# crates/engine/src/driver.rs:31 --
#   pub const DRAFT_EPOCH_REWARD_GWEI: u128 = 6_250 * 1_000_000_000;
# (6,250 SOVA/epoch, in gwei). Settlement wire amounts are wei
# (gwei * GWEI_IN_WEI, engine::driver::GWEI_IN_WEI = 1_000_000_000), so one
# epoch's full reward is 6_250 * 1e9 gwei * 1e9 wei/gwei = 6_250 * 1e18 wei
# = 6,250 SOVA. A single-miner epoch's sole miner takes the *entire*
# reward -- pro-rata pool at 100% weight, plus the sealer tip, plus all
# floor-division dust (crates/consensus/src/epoch.rs's `epoch_rewards`
# distributes the reward exactly, no remainder) -- so EXPECTED_REWARD_WEI
# below is not an approximation.
#
# This is deliberately hardcoded rather than computed from the source at
# runtime: a hardcoded expectation checked against a live grep of the
# constant is a stronger regression than deriving the expectation from the
# same source file it's meant to catch drift in. If DRAFT_EPOCH_REWARD_GWEI
# ever changes, the grep below fails LOUDLY and immediately, before any
# live regtest/build time is spent -- update EXPECTED_REWARD_WEI here (and
# TWO_EPOCH_REWARD_WEI) to match.
# Since C8 (SIP-3 wiring) the draft constant aliases the schedule's base
# reward; the value is unchanged (6_250_000_000_000 gwei = 6,250 SOVA), so
# the guard follows the alias to its one definition.
DRIVER_RS="${ROOT}/crates/engine/src/driver.rs"
SCHEDULE_RS="${ROOT}/crates/consensus/src/schedule.rs"
DRIVER_CONST_LINE="$(grep -F 'pub const DRAFT_EPOCH_REWARD_GWEI' "${DRIVER_RS}" 2>/dev/null || true)"
BASE_CONST_LINE="$(grep -F 'pub const BASE_EPOCH_REWARD_GWEI' "${SCHEDULE_RS}" 2>/dev/null || true)"
if [[ "${DRIVER_CONST_LINE}" == *'6_250 * 1_000_000_000'* ]]; then
  :
elif [[ "${DRIVER_CONST_LINE}" == *'consensus::schedule::BASE_EPOCH_REWARD_GWEI'* \
  && "${BASE_CONST_LINE}" == *'= 6_250_000_000_000;'* ]]; then
  :
else
  fail "the draft epoch reward is no longer 6,250 SOVA (driver: ${DRIVER_CONST_LINE:-<not found>}; schedule: ${BASE_CONST_LINE:-<not found>}) -- EXPECTED_REWARD_WEI/TWO_EPOCH_REWARD_WEI in $(basename "$0") are hardcoded from it and MUST be updated to match, or this scenario is silently checking a stale reward amount"
  exit 1
fi
EXPECTED_REWARD_WEI="6250000000000000000000"       # 6,250 SOVA
TWO_EPOCH_REWARD_WEI="12500000000000000000000"      # 12,500 SOVA (cumulative)

# ---------------------------------------------------------------------
# Start: kill stray processes/containers left over from a previous run
# ---------------------------------------------------------------------

echo "--- killing any stray bin/sova process ---"
pkill -f "target/debug/sova$" 2>/dev/null || true

echo "--- killing any leaked auto-mine.sh against ${RPC_URL} ---"
LEAKED_AUTO_MINE_PIDS="$(pgrep -f "auto-mine\.sh.*${RPC_URL##*/}" 2>/dev/null || true)"
if [[ -n "${LEAKED_AUTO_MINE_PIDS}" ]]; then
  echo "warning: killing leaked auto-mine.sh process(es): ${LEAKED_AUTO_MINE_PIDS}" >&2
  # shellcheck disable=SC2086
  kill ${LEAKED_AUTO_MINE_PIDS} 2>/dev/null || true
  sleep 1
fi

echo "--- tearing down any stray regtest container ---"
(cd "${REGTEST_DIR}" && ${COMPOSE} down -v) >/dev/null 2>&1 || true

# ---------------------------------------------------------------------
# Build bin/sova and sova-miner if missing
# ---------------------------------------------------------------------

if [[ ! -x "${SOVA_BIN}" ]]; then
  echo "--- building bin/sova (debug; not found at ${SOVA_BIN}) ---"
  (cd "${ROOT}" && cargo build -p sova --quiet)
fi
if [[ ! -x "${MINER_BIN}" ]]; then
  echo "--- building sova-miner (release; not found at ${MINER_BIN}) ---"
  (cd "${BURN_WALLET_DIR}" && cargo build --release -p sova-miner --quiet)
fi

# ---------------------------------------------------------------------
# Fresh regtest stack
# ---------------------------------------------------------------------

echo "--- starting a fresh regtest stack ---"
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

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/sova-mint-scenario.XXXXXX")"
echo "--- work dir: ${WORK_DIR} ---"

# ---------------------------------------------------------------------
# Miner identity (init first -- the node needs its EVM address to start
# in mine mode)
# ---------------------------------------------------------------------

MINER_DATA_DIR="${WORK_DIR}/miner"
mkdir -p "${MINER_DATA_DIR}"
echo "--- sova-miner init ---"
"${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest init >"${WORK_DIR}/miner-init.log" 2>&1
TADDR="$(awk '/t-addr to fund/ {print $NF}' "${WORK_DIR}/miner-init.log")"
EVM_ADDR="$(awk '/evm address/ {print $NF}' "${WORK_DIR}/miner-init.log")"
if [[ -z "${TADDR}" || -z "${EVM_ADDR}" ]]; then
  fail "setup: could not parse t-addr/evm address from sova-miner init output"
  cat "${WORK_DIR}/miner-init.log" >&2
  exit 1
fi
echo "miner identity: ${TADDR} / ${EVM_ADDR}"

# ---------------------------------------------------------------------
# Start bin/sova in mine mode (identity comes from init -- no
# --evm-address override, matching the proven ad-hoc flow)
# ---------------------------------------------------------------------

echo "--- starting bin/sova (mine mode) ---"
SOVA_ZEBRAD_RPC="${RPC_URL}" SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" SOVA_EPOCH_BASE=1 \
  "${SOVA_BIN}" >"${WORK_DIR}/sova-node.log" 2>&1 &
SOVA_PID=$!

echo "--- waiting for bin/sova's EVM RPC readiness ---"
deadline=$((SECONDS + 60))
# Response into a variable, not `eth_rpc | grep -q`: under pipefail a
# match can SIGPIPE curl and read as "not ready".
until ready_resp="$(eth_rpc eth_chainId "[]")" && grep -q result <<<"${ready_resp}"; do
  if ! kill -0 "${SOVA_PID}" 2>/dev/null; then
    fail "setup: bin/sova exited before becoming ready; log follows"
    cat "${WORK_DIR}/sova-node.log" >&2
    exit 1
  fi
  if [[ ${SECONDS} -ge ${deadline} ]]; then
    fail "setup: bin/sova EVM RPC did not become ready within 60s"
    exit 1
  fi
  sleep 2
done
echo "bin/sova up (pid ${SOVA_PID})"

# ---------------------------------------------------------------------
# Fund the miner: 101 blocks to its own t-addr (1 spendable coinbase once
# the other 100 mature it), matching the proven ad-hoc flow.
# ---------------------------------------------------------------------

echo "--- funding: 101 blocks to the miner's own address (coinbase maturity) ---"
generate_to_address 101 "${TADDR}"
FUNDING_TIP="$(tip_height)"
echo "tip after funding: ${FUNDING_TIP} (expected 101)"
if [[ "${FUNDING_TIP}" != "101" ]]; then
  fail "setup: expected tip 101 after funding, got ${FUNDING_TIP}"
fi

# ---------------------------------------------------------------------
# Drive the chain forward: auto-mine, then the miner reacts to new blocks
# with SIP-1 burns. auto-mine runs for the whole live portion of this
# scenario (both burns plus the lockstep sampling window) and is only
# stopped right before the final log assertion.
# ---------------------------------------------------------------------

echo "--- starting auto-mine (1 block every ${AUTO_MINE_INTERVAL_S}s) in the background ---"
"${REGTEST_DIR}/auto-mine.sh" "${AUTO_MINE_INTERVAL_S}" "${RPC_URL}" >"${WORK_DIR}/auto-mine.log" 2>&1 &
AUTO_MINE_PID=$!

# ============================================================
# Burn 1
# ============================================================
echo ""
echo "=== burn 1 ==="
echo "--- sova-miner mine (--max-epochs 1; NO --evm-address, identity from init) ---"
if ! "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
  --budget-zat "${BUDGET_ZAT}" \
  --per-epoch-zat "${PER_EPOCH_ZAT}" \
  --rpc "${RPC_URL}" \
  --max-epochs 1 >"${WORK_DIR}/mine-1.log" 2>&1; then
  fail "burn 1: sova-miner mine exited nonzero"
  cat "${WORK_DIR}/mine-1.log" >&2
fi
cat "${WORK_DIR}/mine-1.log"

echo "--- (b) waiting for burn 1's epoch to settle (EVM balance to move off zero) ---"
BAL_1="$(wait_for_balance_change "${EVM_ADDR}" "0" 90)" || {
  fail "(b) first mint: balance never moved off 0 within 90s (last observed: ${BAL_1})"
}
if [[ "${BAL_1}" == "${EXPECTED_REWARD_WEI}" ]]; then
  pass "(b) first mint: balance == ${EXPECTED_REWARD_WEI} wei (exactly 6,250 SOVA)"
else
  fail "(b) first mint: balance == ${BAL_1} wei, expected exactly ${EXPECTED_REWARD_WEI} wei (6,250 SOVA)"
fi

# ============================================================
# (a) lockstep: 3 samples over ~15s, chain still advancing via auto-mine
# ============================================================
echo ""
echo "=== (a) lockstep sampling ==="
LOCKSTEP_SAMPLES=3
LOCKSTEP_INTERVAL_S=7
i=0
while [[ ${i} -lt ${LOCKSTEP_SAMPLES} ]]; do
  ZC_TIP="$(tip_height)"
  SV_BLOCK="$(eth_block_number)"
  LAG=$((ZC_TIP - SV_BLOCK))
  echo "  sample $((i + 1))/${LOCKSTEP_SAMPLES}: zcash_tip=${ZC_TIP} sova_block=${SV_BLOCK} lag=${LAG}"
  if [[ "${LAG}" -lt 0 || "${LAG}" -gt 1 ]]; then
    fail "(a) lockstep: sample $((i + 1)) zcash_tip=${ZC_TIP} sova_block=${SV_BLOCK} lag=${LAG} -- outside allowed [0,1]"
  else
    pass "(a) lockstep: sample $((i + 1)) zcash_tip=${ZC_TIP} sova_block=${SV_BLOCK} lag=${LAG} (<=1 OK)"
  fi
  i=$((i + 1))
  if [[ ${i} -lt ${LOCKSTEP_SAMPLES} ]]; then
    sleep "${LOCKSTEP_INTERVAL_S}"
  fi
done

# ============================================================
# Burn 2 (a later epoch; a fresh `mine` invocation, so it gets its own
# fresh BUDGET_ZAT per D5's per-invocation semantics -- see the note on
# BUDGET_ZAT near the top of this script. No need to size this call's
# budget for anything burn 1 already spent.)
# ============================================================
echo ""
echo "=== burn 2 ==="
echo "--- sova-miner mine (--max-epochs 1; same data dir, own UTXO chains through change) ---"
if ! "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
  --budget-zat "${BUDGET_ZAT}" \
  --per-epoch-zat "${PER_EPOCH_ZAT}" \
  --rpc "${RPC_URL}" \
  --max-epochs 1 >"${WORK_DIR}/mine-2.log" 2>&1; then
  fail "burn 2: sova-miner mine exited nonzero"
  cat "${WORK_DIR}/mine-2.log" >&2
fi
cat "${WORK_DIR}/mine-2.log"

echo "--- (c) waiting for burn 2's epoch to settle (EVM balance to move off ${BAL_1}) ---"
BAL_2="$(wait_for_balance_change "${EVM_ADDR}" "${BAL_1}" 90)" || {
  fail "(c) repeated settlement: balance never moved off ${BAL_1} within 90s (last observed: ${BAL_2})"
}
if [[ "${BAL_2}" == "${TWO_EPOCH_REWARD_WEI}" ]]; then
  pass "(c) repeated settlement: cumulative balance == ${TWO_EPOCH_REWARD_WEI} wei (exactly 12,500 SOVA)"
else
  fail "(c) repeated settlement: cumulative balance == ${BAL_2} wei, expected exactly ${TWO_EPOCH_REWARD_WEI} wei (12,500 SOVA)"
fi

# ============================================================
# (d) log invariant: settled=true for exactly the two burn epochs
# ============================================================
echo ""
echo "=== (d) settled=true log invariant ==="
echo "--- stopping auto-mine and bin/sova so the log is final before counting ---"
kill "${AUTO_MINE_PID}" 2>/dev/null || true
wait "${AUTO_MINE_PID}" 2>/dev/null || true
AUTO_MINE_PID=""
kill "${SOVA_PID}" 2>/dev/null || true
wait "${SOVA_PID}" 2>/dev/null || true
SOVA_PID=""

# reth's tracing output is ANSI-colored even when redirected to a file
# (RethTracer doesn't detect non-tty output here); strip escape codes
# before counting so the assertion isn't fooled by color codes sitting
# between "settled" and "=" and the value.
SETTLED_TRUE_COUNT="$(sed 's/\x1b\[[0-9;]*m//g' "${WORK_DIR}/sova-node.log" | grep -c 'sova epoch trigger.*settled=true' || true)"
echo "settled=true count: ${SETTLED_TRUE_COUNT}"
if [[ "${SETTLED_TRUE_COUNT}" == "2" ]]; then
  pass "(d) log invariant: settled=true appears exactly 2 times (the two burn epochs)"
else
  fail "(d) log invariant: settled=true appears ${SETTLED_TRUE_COUNT} times, expected exactly 2"
  sed 's/\x1b\[[0-9;]*m//g' "${WORK_DIR}/sova-node.log" | grep 'sova epoch trigger' >&2 || true
fi

# ---------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------
echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "SCENARIO 4 PASSED (all 4 assertions)"
else
  echo "SCENARIO 4: ${FAILURES} ASSERTION(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
