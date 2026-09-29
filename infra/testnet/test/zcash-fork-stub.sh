#!/usr/bin/env bash
# infra/testnet/test/zcash-fork-stub.sh -- offline test of host/health.sh's
# Zcash fork findings (docs/design/nu7-readiness.md §4.2):
#   zcash_ref_fork  our zebrad's block hash at tip - 6 differs from every
#                   ZCASH_REFERENCE_URLS source that answers, for 6 min
#   zebrad_nu7      NU7_ACTIVATION_HEIGHT set, and zebrad has no NU7 upgrade
#                   at that height (before activation)
#   zcash_fork      at or past that height, zebrad's nextblock / chaintip
#                   branch is not NU7's
# A fake zebrad (rpc) and fake references (rpc, curl) answer from one of
# two chains, "main" and "fork"; the clock is faked, so "7 minutes" takes no
# time. The functions are taken from health.sh as shipped; say, alert,
# clear_alert, rpc, curl and date are stand-ins that print or replay.
#
#   test/zcash-fork-stub.sh
#
# Needs jq and awk. Touches nothing but a temp dir.
set -uo pipefail
KIT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT
PASS=0
FAIL=0

# The code under test, from the script as shipped.
fn() { awk -v f="^$2\\\\(\\\\) \\\\{" '$0 ~ f { on = 1 } on { print } on && /^}/ { exit }' "$1"; }
H="${KIT}/host/health.sh"
FNS="persisted ref_hash ref_name check_zcash_reference check_nu7"
{
  grep -E '^(ZCASH_REF_[A-Z_]+|ZCASH_REFERENCE_URLS|NU7_[A-Z_]+)=' "${H}"
  grep -E '^\[\[ "\$\{NU7_ACTIVATION_HEIGHT\}"' "${H}"
  for f in ${FNS}; do fn "${H}" "${f}"; done
} >"${TMP}/fns.sh"
for f in ${FNS}; do
  grep -q "^${f}() {" "${TMP}/fns.sh" || { echo "FAIL  ${f} not found in health.sh"; exit 1; }
done
grep -q '^ZCASH_REF_DEPTH=6$' "${TMP}/fns.sh" || { echo "FAIL  ZCASH_REF_DEPTH=6 not found in health.sh"; exit 1; }
grep -q '^NU7_BRANCH_ID=77190ad9$' "${TMP}/fns.sh" || { echo "FAIL  NU7_BRANCH_ID=77190ad9 not found in health.sh"; exit 1; }

T0=1790633000
TIP=4415229
RPC_REF=https://rpc.ref.example
EXP_REF='https://api.ref.example/api/block/{height}'
# hash_of <chain> <height>: the height, then 0 (main) or f (fork), as 64 hex.
hash_of() { if [[ "$1" == main ]]; then printf '%08x%056x' "$2" 0; else printf '%08xf%055x' "$2" 0; fi; }
M16="$(hash_of main $((TIP - 6)) | cut -c1-16)" # 00435ef700000000
F16="$(hash_of fork $((TIP - 6)) | cut -c1-16)" # 00435ef7f0000000

# One pass at <now>. OUR / REF1 (JSON-RPC) / REF2 (explorer, upper-case hex):
# main, fork or none (no answer). INFO: getblockchaininfo JSON for check_nu7.
# shellcheck disable=SC2034,SC2329 # set for and called by the sourced functions
run_pass() { # now what(ref|nu7) [tip]
  (
    NOW="$1"
    local what="$2" tip="${3:-${TIP}}"
    STATE_DIR="${TMP}/state"
    mkdir -p "${STATE_DIR}"
    ZEBRA_URL=http://127.0.0.1:18232
    say() { echo "  log   $*"; }
    alert() { local k="$1"; shift; echo "  ALERT ${k}: $*"; }
    clear_alert() { echo "  (clear $1)"; rm -f "${STATE_DIR}/$1.last" "${STATE_DIR}/$1.since" "${STATE_DIR}/$1.passes"; }
    date() { echo "${NOW}"; }
    rpc() { # url method params
      local h chain
      h="$(tr -dc 0-9 <<<"${3:-}")"
      echo "rpc $1 $2 ${h}" >>"${TMP}/calls"
      case "$1" in
        "${ZEBRA_URL}") chain="${OUR}" ;;
        "${RPC_REF}") chain="${REF1}" ;;
        *) return 7 ;;
      esac
      [[ "${chain}" == none ]] && return 22
      printf '{"jsonrpc":"2.0","id":1,"result":"%s"}' "$(hash_of "${chain}" "${h}")"
    }
    curl() { # ... url (last)
      local url="${!#}"
      echo "get ${url}" >>"${TMP}/calls"
      [[ "${url}" == https://api.ref.example/api/block/* && "${REF2}" != none ]] || return 22
      printf '{"height":"%s","hash":"%s"}' "${url##*/}" "$(hash_of "${REF2}" "${url##*/}" | tr 'a-f' 'A-F')"
    }
    # shellcheck source=/dev/null
    source "${TMP}/fns.sh"
    if [[ "${what}" == ref ]]; then check_zcash_reference "${tip}"; else check_nu7 "${INFO}" "${tip}"; fi
  )
}

verdict() { # name want(alert|quiet) key out [must-match-ERE]
  local name="$1" want="$2" key="$3" out="$4" re="${5:-}" got=quiet
  echo "== ${name}"
  [[ -n "${out}" ]] && echo "${out}"
  grep -q "^  ALERT ${key}:" <<<"${out}" && got=alert
  if [[ "${got}" == "${want}" ]] && { [[ -z "${re}" ]] || grep -qE -- "${re}" <<<"${out}"; }; then
    PASS=$((PASS + 1)); echo "PASS  ${want}${re:+ /${re}/}"
  else
    FAIL=$((FAIL + 1)); echo "FAIL  expected ${want}${re:+ matching /${re}/}, got ${got}"
  fi
  echo
}
ref() { # name now want re [tip]
  local out
  : >"${TMP}/calls"
  out="$(run_pass "$2" ref "${5:-${TIP}}")"
  verdict "$1" "$3" zcash_ref_fork "${out}" "${4:-}"
}

# ---- zcash_ref_fork -------------------------------------------------------------
export ZCASH_REFERENCE_URLS="" OUR=main REF1=main REF2=main
ref "references unset: skipped, nothing asked" "${T0}" quiet '^$'
if [[ ! -s "${TMP}/calls" ]]; then PASS=$((PASS + 1)); echo "PASS  ... no call made"; else FAIL=$((FAIL + 1)); echo "FAIL  asked something with no references set"; fi
echo

export ZCASH_REFERENCE_URLS="${RPC_REF},${EXP_REF}"
ref "both references agree" "${T0}" quiet "block $((TIP - 6)) \(tip - 6\) is ${M16}\.\.\. here and at 2 of 2 reference\(s\)$"
if grep -qx "rpc ${RPC_REF} getblockhash $((TIP - 6))" "${TMP}/calls" && grep -qx "get https://api.ref.example/api/block/$((TIP - 6))" "${TMP}/calls"; then
  PASS=$((PASS + 1)); echo "PASS  ... asked both for tip - 6 = $((TIP - 6)) (JSON-RPC getblockhash; explorer GET with {height} filled)"
else
  FAIL=$((FAIL + 1)); echo "FAIL  expected getblockhash and GET for $((TIP - 6)); calls:"; cat "${TMP}/calls"
fi
echo
OUR=fork
ref "... 5 min later we are on a fork, but the last agreement was under 10 min ago: not asked" $((T0 + 300)) quiet '^$'
ref "10 min after the agreement: every reference disagrees (first pass)" $((T0 + 600)) quiet 'under 6 min so far \(a reorg settles in minutes\)'
ref "... 4 min" $((T0 + 840)) quiet 'under 6 min so far'
ref "... 6 min" $((T0 + 960)) alert \
  "ALERT zcash_ref_fork: ZCASH FORK\? zebrad here is on another chain than every reference that answers, for 6\+ min: block $((TIP - 6)) \(tip - 6\) is ${F16}\.\.\. here, but rpc\.ref\.example has ${M16}\.\.\., api\.ref\.example has ${M16}\.\.\.\. Sova on this host follows zebrad\. .*nu7-readiness\.md §4\.2"
OUR=main
ref "back on the main chain: agreement clears" $((T0 + 1080)) quiet '\(clear zcash_ref_fork\)'

rm -rf "${TMP}/state"
OUR=main REF1=fork REF2=main
ref "one reference is on another chain, the other agrees with us: that source is the odd one" "${T0}" quiet \
  "at 1 of 2 reference\(s\); but rpc\.ref\.example has ${F16}\.\.\.: that source is on another chain"
rm -rf "${TMP}/state"
OUR=fork REF1=none REF2=main
ref "one reference silent, the other disagrees (first pass)" "${T0}" quiet 'rpc\.ref\.example gave no hash'
ref "... 7 min: every one that answers disagrees" $((T0 + 420)) alert 'every reference that answers.*api\.ref\.example has'
rm -rf "${TMP}/state"
OUR=fork REF1=none REF2=none
ref "no reference answers: not judged" "${T0}" quiet 'none of 2 reference\(s\) answered for block'
ref "... 7 min of silence: still not judged, no alert" $((T0 + 420)) quiet 'none of 2'
OUR=none REF1=main REF2=main
ref "our zebrad gives no hash: not judged" $((T0 + 540)) quiet 'our zebrad gave no hash'
if ! grep -q ref.example "${TMP}/calls"; then PASS=$((PASS + 1)); echo "PASS  ... and no reference asked"; else FAIL=$((FAIL + 1)); echo "FAIL  asked a reference without our own hash"; fi
echo
ZCASH_REFERENCE_URLS="${RPC_REF}" OUR=main REF1=main
rm -rf "${TMP}/state"
ref "one JSON-RPC reference only, agrees" "${T0}" quiet 'at 1 of 1 reference'

# ---- NU7: zebrad_nu7 / zcash_fork -----------------------------------------------------
N7=4416000
nu7() { # name want key re tip nu7-at chaintip nextblock
  local ups out
  if [[ -n "$6" ]]; then
    ups="\"37a5165b\":{\"name\":\"NU6.3\",\"activationheight\":4134000,\"status\":\"active\"},\"77190ad9\":{\"name\":\"NU7\",\"activationheight\":$6,\"status\":\"pending\"}"
  else
    ups="\"37a5165b\":{\"name\":\"NU6.3\",\"activationheight\":4134000,\"status\":\"active\"}"
  fi
  # shellcheck disable=SC2089,SC2090 # JSON text, used as data, never re-parsed as shell
  INFO="{\"result\":{\"blocks\":$5,\"upgrades\":{${ups}},\"consensus\":{\"chaintip\":\"$7\",\"nextblock\":\"$8\"}}}"
  # shellcheck disable=SC2090
  export INFO
  out="$(run_pass "${T0}" nu7 "$5")"
  verdict "$1" "$2" "$3" "${out}" "$4"
}
export NU7_ACTIVATION_HEIGHT=""
nu7 "NU7_ACTIVATION_HEIGHT unset: skipped" quiet zebrad_nu7 '^$' $((N7 - 800)) "" 37a5165b 37a5165b
export NU7_ACTIVATION_HEIGHT=soon
nu7 "NU7_ACTIVATION_HEIGHT malformed: skipped" quiet zebrad_nu7 '^$' $((N7 - 800)) "" 37a5165b 37a5165b
export NU7_ACTIVATION_HEIGHT="${N7}"
nu7 "before NU7: zebrad has no NU7 upgrade (6.3.0)" alert zebrad_nu7 \
  "ALERT zebrad_nu7: zebrad here has no NU7 upgrade \(branch 77190ad9\); NU7 activates at Zcash height ${N7}, 800 blocks from its tip $((N7 - 800))\. Upgrade zebrad" \
  $((N7 - 800)) "" 37a5165b 37a5165b
nu7 "before NU7: zebrad knows NU7 at the height" quiet zebrad_nu7 "zebrad activates NU7 \(77190ad9\) at ${N7}, 800 blocks from its tip $((N7 - 800))$" \
  $((N7 - 800)) "${N7}" 37a5165b 37a5165b
nu7 "before NU7: zebrad has NU7 at another height" alert zebrad_nu7 "activates NU7 at $((N7 + 3)), but NU7_ACTIVATION_HEIGHT is ${N7}" \
  $((N7 - 800)) $((N7 + 3)) 37a5165b 37a5165b
nu7 "one block before NU7, upgraded: next block is NU7" quiet zcash_fork 'nu7: zebrad is on NU7' \
  $((N7 - 1)) "${N7}" 37a5165b 77190ad9
nu7 "one block before NU7, not upgraded: next block on NU6.3 (stalls here)" alert zcash_fork \
  "zebrad here is at $((N7 - 1)), one block before NU7 \(${N7}\), but will build the next block on branch 37a5165b, not NU7's 77190ad9: a pre-NU7 zebrad" \
  $((N7 - 1)) "" 37a5165b 37a5165b
nu7 "past NU7, upgraded" quiet zcash_fork '\(clear zcash_fork\)' $((N7 + 5)) "${N7}" 77190ad9 77190ad9
nu7 "past NU7 on the NU6.3 branch: an old-rules chain" alert zcash_fork \
  "ALERT zcash_fork: ZCASH FORK: zebrad here is at $((N7 + 5)), past NU7's activation height ${N7}, on branch 37a5165b \(next 37a5165b\), not NU7's 77190ad9" \
  $((N7 + 5)) "" 37a5165b 37a5165b
nu7 "past NU7, upper-case branch ids from zebrad" quiet zcash_fork 'nu7: zebrad is on NU7' $((N7 + 5)) "${N7}" 77190AD9 77190AD9

echo "${PASS} passed, ${FAIL} failed"
[[ "${FAIL}" == 0 ]]
