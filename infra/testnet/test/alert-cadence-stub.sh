#!/usr/bin/env bash
# infra/testnet/test/alert-cadence-stub.sh -- offline test of how often
# host/health.sh sends an alert to Telegram (Rob 2026-10-02: hourly repeats
# were too loud): once when it fires, again only after ALERT_REPEAT_MIN,
# and one "resolved" line when it clears, nothing while muted. alert,
# tg_send, clear_alert and fmt_time are taken from health.sh as shipped;
# say, date and curl are stand-ins; the clock is faked.
#
#   test/alert-cadence-stub.sh
#
# Needs awk. Touches nothing but a temp dir.
set -uo pipefail
KIT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
H="${KIT}/host/health.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT
PASS=0 FAIL=0
pass() { PASS=$((PASS + 1)); echo "PASS  $*"; }
flunk() { FAIL=$((FAIL + 1)); echo "FAIL  $*"; }
expect() { if grep -qE -- "$2" <<<"$3"; then pass "$1"; else flunk "$1 (no /$2/)"; fi; }
expect_not() { if grep -qE -- "$2" <<<"$3"; then flunk "$1 (found /$2/)"; else pass "$1"; fi; }
# One function from a script, as shipped (a one-line function ends on its line).
fn() { awk -v f="^$2\\\\(\\\\) \\\\{" '$0 ~ f { on = 1; first = 1 } on { print } on && (/^}/ || (first && /}[[:space:]]*$/)) { exit } { first = 0 }' "$1"; }

{
  grep -E '^ALERT_REPEAT_MIN=' "${H}"
  for f in fmt_time alert tg_send clear_alert; do fn "${H}" "${f}"; done
} >"${TMP}/fns.sh"
for f in fmt_time alert tg_send clear_alert; do
  grep -q "^${f}() {" "${TMP}/fns.sh" || { echo "FAIL  ${f} not found in health.sh"; exit 1; }
done

T0=1790900000
# step <now> <muted 0|1> <command...>: one call at a faked time; prints
# what went to Telegram as SENT lines.
# shellcheck disable=SC2034,SC2329 # used by the sourced functions
step() {
  (
    NOW="$1" M="$2"
    shift 2
    STATE_DIR="${TMP}/state" HOST=sova-test TELEGRAM_BOT_TOKEN=t TELEGRAM_CHAT_ID=1 TELEGRAM_THREAD_ID=7
    mkdir -p "${STATE_DIR}"
    MUTED_UNTIL=""
    ((M == 1)) && MUTED_UNTIL=$((NOW + 600))
    # shellcheck source=/dev/null
    source "${TMP}/fns.sh"
    say() { echo "  log   $*"; }
    date() { if [[ "$1" == +%s ]]; then echo "${NOW}"; else command date "$@"; fi; }
    # tg_send sends curl's output to /dev/null: record what went out in a file.
    curl() { cat >/dev/null; echo "  SENT  $(printf '%s ' "$@" | grep -o 'text=.*')" >>"${TMP}/sent"; }
    "$@"
    [[ ! -f "${TMP}/sent" ]] || cat "${TMP}/sent"
    rm -f "${TMP}/sent"
  )
}

OUT="$(step "${T0}" 0 alert block_age "SOVA STUCK: newest block 1 is 20 min old")"
echo "${OUT}"
expect "first firing: sent" 'SENT  text=\[sova sova-test\] block_age: SOVA STUCK' "${OUT}"

OUT="$(step $((T0 + 3600)) 0 alert block_age "SOVA STUCK: newest block 1 is 80 min old")"
echo "${OUT}"
expect "an hour later, still true: logged" 'log   ALERT block_age' "${OUT}"
expect_not "... but not sent again (was hourly before 2026-10-02)" 'SENT' "${OUT}"

OUT="$(step $((T0 + 6 * 3600 - 60)) 0 alert block_age "still stuck")"
expect_not "just under 6 h: not sent" 'SENT' "${OUT}"

OUT="$(step $((T0 + 6 * 3600)) 0 alert block_age "still stuck after 6 h")"
echo "${OUT}"
expect "6 h after the first: sent again" 'SENT  text=\[sova sova-test\] block_age: still stuck after 6 h' "${OUT}"

OUT="$(step $((T0 + 6 * 3600 + 120)) 0 clear_alert block_age)"
echo "${OUT}"
expect "cleared after an alert went out: one resolved line" 'SENT  text=\[sova sova-test\] block_age: resolved after 362 min' "${OUT}"

OUT="$(step $((T0 + 6 * 3600 + 240)) 0 clear_alert block_age)"
expect_not "cleared again: nothing (no repeat of resolved)" 'SENT' "${OUT}"

OUT="$(step $((T0 + 7200)) 0 clear_alert epoch_lag)"
expect_not "a key that never alerted clears silently" 'SENT' "${OUT}"

OUT="$(step $((T0 + 9000)) 0 alert keeper_down "sova-keeper is failed")"
expect "a new alert after a resolve: sent at once" 'SENT  text=\[sova sova-test\] keeper_down' "${OUT}"
OUT="$(step $((T0 + 9300)) 1 clear_alert keeper_down)"
echo "${OUT}"
expect "resolved while muted: logged" 'log   RESOLVED keeper_down .*\[muted, not sent\]' "${OUT}"
expect_not "... not sent" 'SENT' "${OUT}"
if [[ ! -e "${TMP}/state/keeper_down.first" ]]; then pass "... state cleared"; else flunk "... .first left behind"; fi

echo
echo "${PASS} passed, ${FAIL} failed"
((FAIL == 0))
