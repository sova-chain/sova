#!/usr/bin/env bash
# infra/testnet/test/peer-alerts-stub.sh -- offline test of host/health.sh's
# peer findings from sova-node's journal: keeper_isolated (the keeper host:
# 0 peers for KEEPER_ISOLATED_MIN minutes, or a peer that connects and
# drops at once, again and again) and rejecting_blocks (any node host: one
# peer's blocks judged INVALID again and again). Canned journal lines, in
# `journalctl -o short-unix` form and the node's own wording
# (crates/engine/src/p2p/service.rs, reth's "Status" line), are fed to
# check_peers; the clock is faked, so "6 minutes" takes no time. The
# functions are taken from health.sh as shipped; say, alert, clear_alert,
# systemctl, node_journal and date are stand-ins that print or replay.
#
#   test/peer-alerts-stub.sh
#
# Needs awk. Touches nothing but a temp dir.
set -uo pipefail
KIT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT
PASS=0
FAIL=0

# The code under test, from the script as shipped.
fn() { awk -v f="^$2\\\\(\\\\) \\\\{" '$0 ~ f { on = 1 } on { print } on && /^}/ { exit }' "$1"; }
H="${KIT}/host/health.sh"
{
  grep -E '^(PEER_FLAP_|REJECT_)[A-Z_]+=' "${H}"
  grep -E '^KEEPER_NODE_ID=|^\[\[ "\$\{KEEPER_NODE_ID\}"' "${H}"
  for f in persisted short_id peer_findings check_keeper_isolated check_block_rejects check_peers; do fn "${H}" "${f}"; done
} >"${TMP}/fns.sh"
for f in persisted short_id peer_findings check_keeper_isolated check_block_rejects check_peers; do
  grep -q "^${f}() {" "${TMP}/fns.sh" || { echo "FAIL  ${f} not found in health.sh"; exit 1; }
done

KEEPER_ID="$(printf 'a1%.0s' $(seq 1 64))"
SEED_ID="$(printf '5e%.0s' $(seq 1 64))"
RPC_ID="$(printf 'c0%.0s' $(seq 1 64))"
STRANGER_ID="$(printf 'ff%.0s' $(seq 1 64))"
T0=1790633000 # the fake clock: every pass says its own "now"

# ---- canned journal lines (short-unix: epoch time, host, unit[pid]: message)
J="${TMP}/journal"
line() { # at message...
  local at="$1"
  shift
  printf '%s.%06d sova-host sova[4242]: 2026-09-28T22:16:26.123456Z  %s\n' "${at}" "$((RANDOM % 1000))" "$*"
}
status() { line "$1" "INFO Status connected_peers=$2 latest_block=22594"; }
active() { line "$1" "INFO sova/1: peer active peer_id=0x$2 peers=$3"; }
gone() { line "$1" "INFO sova_engine::p2p::service: sova/1: peer gone peer_id=0x$2 peers=$3"; }
invalid() { # at peer height
  line "$1" "WARN sova/1: peer sent an INVALID block; reputation hit peer=0x$2 height=$3 hash=0x$(printf '9%.0s' $(seq 1 64)) validation_error=sova-seal: timestamp 1790633786 outside [1790633785, 1790633785] for epoch 4410938"
}
started() { printf '%s.000000 sova-host systemd[1]: Started sova-node.service - Sova node (sova-testnet).\n' "$1"; }
# Status lines every 25 s from <from> to <to> saying <n> peers.
statuses() { local t; for ((t = $1; t <= $2; t += 25)); do status "${t}" "$3"; done; }

# One health pass at <now>: this host is the keeper (1) or not (0).
# Prints the journal lines health.sh would write; "ALERT key: ..." is what
# would go to Telegram, "(clear key)" a dedupe reset.
# shellcheck disable=SC2034,SC2329 # set for and called by the sourced functions
run_pass() { # now keeper(0|1) [KEEPER_NODE_ID]
  (
    NOW="$1" IS_KEEPER="$2"
    KEEPER_NODE_ID="${3-${KEEPER_ID}}"
    KEEPER_ISOLATED_MIN=5
    STATE_DIR="${TMP}/state"
    mkdir -p "${STATE_DIR}"
    say() { echo "  log   $*"; }
    alert() { local k="$1"; shift; echo "  ALERT ${k}: $*"; }
    clear_alert() { echo "  (clear $1)"; rm -f "${STATE_DIR}/$1.last" "${STATE_DIR}/$1.since" "${STATE_DIR}/$1.passes"; }
    date() { echo "${NOW}"; }
    systemctl() { [[ "$*" == *is-enabled*sova-keeper* ]] && return $((1 - IS_KEEPER)); return 0; }
    node_journal() { cat "${J}"; }
    # shellcheck source=/dev/null
    source "${TMP}/fns.sh"
    check_peers
  )
}

expect() { # name now keeper expect(alert|quiet) key [must-match-ERE] [KEEPER_NODE_ID]
  local name="$1" now="$2" keeper="$3" want="$4" key="$5" re="${6:-}" out got=quiet
  echo "== ${name}"
  sed 's/^/   journal: /' "${J}" | awk 'NR <= 3' && [[ $(wc -l <"${J}") -gt 3 ]] && echo "   journal: ... ($(wc -l <"${J}" | tr -d ' ') lines)"
  if [[ $# -ge 7 ]]; then out="$(run_pass "${now}" "${keeper}" "$7")"; else out="$(run_pass "${now}" "${keeper}")"; fi
  echo "${out}"
  grep -q "^  ALERT ${key}:" <<<"${out}" && got=alert
  if [[ "${got}" == "${want}" ]] && { [[ -z "${re}" ]] || grep -qE -- "${re}" <<<"${out}"; }; then
    PASS=$((PASS + 1)); echo "PASS  ${want}${re:+ /${re}/}"
  else
    FAIL=$((FAIL + 1)); echo "FAIL  expected ${want}${re:+ matching /${re}/}, got ${got}"
  fi
  echo
}

# ---- keeper_isolated -------------------------------------------------------------
statuses $((T0 - 590)) "${T0}" 0 >"${J}"
expect "keeper: 0 peers, first pass" "${T0}" 1 quiet keeper_isolated 'under 5 min so far'
statuses $((T0 - 350)) $((T0 + 240)) 0 >"${J}"
expect "keeper: 0 peers for 4 min" $((T0 + 240)) 1 quiet keeper_isolated 'under 5 min so far'
statuses $((T0 - 240)) $((T0 + 360)) 0 >"${J}"
expect "keeper: 0 peers for 6 min" $((T0 + 360)) 1 alert keeper_isolated \
  "the keeper is sealing alone: no peer keeps a connection, so its blocks don't reach the network \(seeds may have banned it; see docs/ops/testnet-launch.md\)\. 0 peers for 5\+ min"
{ statuses $((T0 - 120)) $((T0 + 420)) 0; active $((T0 + 430)) "${SEED_ID}" 1; active $((T0 + 431)) "${RPC_ID}" 2; statuses $((T0 + 440)) $((T0 + 480)) 2; } >"${J}"
expect "keeper: normal peers again (2)" $((T0 + 480)) 1 quiet keeper_isolated '\(clear keeper_isolated\)'
T1=$((T0 + 3600))
statuses $((T1 - 600)) "${T1}" 0 >"${J}"
expect "keeper: isolated again, first pass (the count started over)" "${T1}" 1 quiet keeper_isolated 'under 5 min so far'
statuses $((T1 - 360)) $((T1 + 240)) 0 >"${J}"
expect "keeper: ... 4 min" $((T1 + 240)) 1 quiet keeper_isolated 'under 5 min so far'
statuses $((T1 - 240)) $((T1 + 480)) 3 >"${J}"
expect "keeper: 3 peers" $((T1 + 480)) 1 quiet keeper_isolated '\(clear keeper_no_peers\)'

# The 2026-09-28 signature: a seed accepts the keeper and cuts it at once,
# every 45 s. reth's Status line can still count a session.
T2=$((T0 + 7200))
{
  statuses $((T2 - 590)) "${T2}" 1
  for d in 200 155 110; do active $((T2 - d)) "${SEED_ID}" 1; gone $((T2 - d + 1)) "${SEED_ID}" 0; done
} | sort -n >"${J}"
expect "keeper: a seed connects and drops within 1 s, x3 in 5 min" "${T2}" 1 alert keeper_isolated \
  "sealing alone: .*peer 0x5e5e5e5e\.\.\.5e5e connected and dropped within 2 s 3 times in 5 min"
{ statuses $((T2 - 590)) "${T2}" 1; for d in 155 110; do active $((T2 - d)) "${SEED_ID}" 1; gone $((T2 - d + 1)) "${SEED_ID}" 0; done; } | sort -n >"${J}"
expect "keeper: connect/drop x2" "${T2}" 1 quiet keeper_isolated
{
  statuses $((T2 - 590)) "${T2}" 2
  active $((T2 - 500)) "${RPC_ID}" 1
  for d in 200 155 110; do active $((T2 - d)) "${SEED_ID}" 2; gone $((T2 - d + 1)) "${SEED_ID}" 1; done
} | sort -n >"${J}"
expect "keeper: connect/drop x3, but another sova/1 peer stays" "${T2}" 1 quiet keeper_isolated 'but 1 other sova/1 peer\(s\) stay: not isolated'
{ statuses $((T2 - 590)) "${T2}" 1; for d in 280 200 120; do active $((T2 - d)) "${SEED_ID}" 1; gone $((T2 - d + 40)) "${SEED_ID}" 0; active $((T2 - d + 41)) "${SEED_ID}" 1; done; } | sort -n >"${J}"
expect "keeper: sessions of 40 s (drops, but not at once), last one up" "${T2}" 1 quiet keeper_isolated
{ statuses $((T2 - 590)) "${T2}" 1; for d in 500 455 410; do active $((T2 - d)) "${SEED_ID}" 1; gone $((T2 - d + 1)) "${SEED_ID}" 0; done; active $((T2 - 300)) "${SEED_ID}" 1; } | sort -n >"${J}"
expect "keeper: connect/drop x3, all over 5 min ago, connected since" "${T2}" 1 quiet keeper_isolated

# sova/1 gone while reth still counts the session: 0 sova/1 peers.
T3=$((T0 + 10800))
{ statuses $((T3 - 590)) "${T3}" 1; active $((T3 - 580)) "${SEED_ID}" 1; gone $((T3 - 100)) "${SEED_ID}" 0; } | sort -n >"${J}"
expect "keeper: Status says 1, but 0 sova/1 peers (first pass)" "${T3}" 1 quiet keeper_isolated 'sova/1 peers 0'
{ statuses $((T3 - 240)) $((T3 + 360)) 1; gone $((T3 - 100)) "${SEED_ID}" 0; } | sort -n >"${J}"
expect "keeper: ... 6 min" $((T3 + 360)) 1 alert keeper_isolated '0 peers for 5\+ min \(connected_peers 1, sova/1 peers 0\)'
: >"${J}"
expect "keeper: no peer line at all: not judged (no alert, no clear)" $((T3 + 480)) 1 quiet keeper_isolated 'peers not judged'
out="$(run_pass $((T3 + 480)) 1)"
check_no_clear() { ! grep -q '(clear keeper_isolated)' <<<"${out}"; }
if check_no_clear; then PASS=$((PASS + 1)); echo "PASS  ... and keeps the alert's dedupe"; else FAIL=$((FAIL + 1)); echo "FAIL  cleared keeper_isolated on a pass it could not judge"; fi
echo
# ANSI colours (RUST_LOG_STYLE unset on a hand-run node) are stripped.
rm -rf "${TMP}/state"
T4=$((T0 + 14400))
printf '%s.000000 sova-host sova[1]: \033[32m INFO\033[0m Status \033[3mconnected_peers\033[0m\033[2m=\033[0m0 latest_block=1\n' $((T4 - 400)) $((T4 - 60)) >"${J}"
run_pass $((T4 - 400)) 1 >/dev/null
expect "keeper: ANSI-coloured Status lines, 0 peers for 6 min" $((T4 - 40)) 1 alert keeper_isolated 'connected_peers 0'
statuses $((T4 - 100)) "${T4}" 0 >"${J}"
expect "not the keeper: no isolation check at all" "${T4}" 0 quiet keeper_isolated
out="$(run_pass "${T4}" 0)"
if ! grep -q 'keeper peers' <<<"${out}"; then PASS=$((PASS + 1)); echo "PASS  ... (no 'keeper peers' line)"; else FAIL=$((FAIL + 1)); echo "FAIL  the isolation check ran on a non-keeper host"; fi
echo

# ---- rejecting_blocks --------------------------------------------------------------
T5=$((T0 + 18000))
{ statuses $((T5 - 590)) "${T5}" 2; invalid $((T5 - 300)) "${KEEPER_ID}" 22439; invalid $((T5 - 255)) "${KEEPER_ID}" 22440; invalid $((T5 - 210)) "${KEEPER_ID}" 22441; } | sort -n >"${J}"
expect "seed: INVALID x3 from the keeper in 10 min" "${T5}" 0 alert rejecting_blocks \
  "rejecting the keeper's blocks as invalid: possible split \(3 INVALID blocks in 10 min, newest at height 22441: sova-seal: timestamp 1790633786 outside"
expect "seed: ... the same on the keeper host (a finding about this node)" "${T5}" 1 alert rejecting_blocks "rejecting the keeper's blocks"
{ statuses $((T5 - 590)) "${T5}" 2; invalid $((T5 - 255)) "${KEEPER_ID}" 22440; invalid $((T5 - 210)) "${KEEPER_ID}" 22441; } | sort -n >"${J}"
expect "seed: INVALID x2" "${T5}" 0 quiet rejecting_blocks '2 INVALID block\(s\) from peer 0xa1a1a1a1\.\.\.a1a1 in 10 min \(alert at 3\)'
{ statuses $((T5 - 590)) "${T5}" 2; invalid $((T5 - 300)) "${KEEPER_ID}" 1; invalid $((T5 - 255)) "${SEED_ID}" 2; invalid $((T5 - 210)) "${STRANGER_ID}" 3; } | sort -n >"${J}"
expect "seed: INVALID x3, one each from three peers" "${T5}" 0 quiet rejecting_blocks
{ statuses $((T5 - 590)) "${T5}" 2; invalid $((T5 - 700)) "${KEEPER_ID}" 1; invalid $((T5 - 650)) "${KEEPER_ID}" 2; invalid $((T5 - 210)) "${KEEPER_ID}" 3; } | sort -n >"${J}"
expect "seed: INVALID x3, two over 10 min ago" "${T5}" 0 quiet rejecting_blocks
{ statuses $((T5 - 590)) "${T5}" 2; invalid $((T5 - 400)) "${KEEPER_ID}" 1; invalid $((T5 - 350)) "${KEEPER_ID}" 2; invalid $((T5 - 300)) "${KEEPER_ID}" 3; started $((T5 - 200)); } | sort -n >"${J}"
expect "seed: INVALID x3, then sova-node restarted (cache and bans cleared)" "${T5}" 0 quiet rejecting_blocks '\(clear rejecting_blocks\)'
{ statuses $((T5 - 590)) "${T5}" 2; for d in 400 350 300; do invalid $((T5 - d)) "${STRANGER_ID}" 7; done; } | sort -n >"${J}"
expect "seed: INVALID x3 from a peer that is not the keeper" "${T5}" 0 alert rejecting_blocks \
  "rejecting the blocks of peer 0xffffffff\.\.\.ffff as invalid: possible split if it relays the keeper's chain, else a peer on a bad fork"
{ statuses $((T5 - 590)) "${T5}" 2; for d in 400 350 300; do invalid $((T5 - d)) "${KEEPER_ID}" 7; done; } | sort -n >"${J}"
expect "seed: INVALID x3 from the keeper, but its node id unknown here" "${T5}" 0 alert rejecting_blocks \
  "blocks of peer 0xa1a1a1a1\.\.\.a1a1 \(the keeper's node id is unknown here: re-run deploy.sh\)" ""
expect "seed: ... KEEPER_NODE_ID given as 0x + upper case" "${T5}" 0 alert rejecting_blocks \
  "rejecting the keeper's blocks" "0x$(tr 'a-f' 'A-F' <<<"${KEEPER_ID}")"
statuses $((T5 - 590)) "${T5}" 2 >"${J}"
expect "seed: no INVALID lines" "${T5}" 0 quiet rejecting_blocks '\(clear rejecting_blocks\)'

echo "${PASS} passed, ${FAIL} failed"
[[ "${FAIL}" == 0 ]]
