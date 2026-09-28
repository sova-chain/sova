#!/usr/bin/env bash
# infra/testnet/test/null-sealed-stub.sh -- offline test of the time-based
# null-run rule (no sealed block for more than NULL_SEALED_MAX_MIN minutes):
# host/health.sh's check_null_run and smoke.sh's check_null_sealed, run
# against a tiny local JSON-RPC stub (python3, 127.0.0.1, a free port) that
# answers batched eth_getBlockByNumber from a scenario. The functions are
# taken from the scripts themselves; the host-side helpers (say, net_alert,
# clear_alert, systemctl) and smoke's ok/bad are stand-ins that print.
#
#   test/null-sealed-stub.sh
#
# Needs python3, curl, jq. Touches nothing but a temp dir and the stub.
set -uo pipefail
KIT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
STUB_PID=""
cleanup() {
  if [[ -n "${STUB_PID}" ]]; then
    kill "${STUB_PID}" 2>/dev/null
    wait "${STUB_PID}" 2>/dev/null
  fi
  rm -rf "${TMP}"
}
trap cleanup EXIT
PASS=0
FAIL=0

# ---- the stub: blocks head..1, block h's time = now - (head - h) * interval
# (+ head_age). Scenario (JSON, re-read on every request): head, interval,
# head_age, sealed (heights), mode: ok | http_error | null_result | short.
cat >"${TMP}/stub.py" <<'EOF'
import json, sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer
SCEN, COUNT = sys.argv[1], sys.argv[2]
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        s = json.load(open(SCEN))
        with open(COUNT, "a") as f: f.write("x\n")
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if s.get("mode") == "http_error":
            self.send_response(503); self.end_headers(); return
        now, out = int(time.time()), []
        for i, req in enumerate(body if isinstance(body, list) else [body]):
            h = int(req["params"][0], 16)
            res = None
            if 1 <= h <= s["head"] and not (s.get("mode") == "null_result" and h == s["head"] - 7):
                ts = now - s.get("head_age", 0) - (s["head"] - h) * s["interval"]
                x = "0x" + "ab" * 97 if h in s["sealed"] else "0x"
                res = {"number": hex(h), "extraData": x, "timestamp": hex(ts)}
            out.append({"jsonrpc": "2.0", "id": req["id"], "result": res})
        if s.get("mode") == "short": out = out[:-1]
        out.reverse()  # answers in any order: the walk sorts them
        data = json.dumps(out).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
srv = HTTPServer(("127.0.0.1", 0), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()
EOF
echo '{"head":1,"interval":75,"sealed":[]}' >"${TMP}/scen.json"
: >"${TMP}/count"
python3 -u "${TMP}/stub.py" "${TMP}/scen.json" "${TMP}/count" >"${TMP}/port" &
STUB_PID=$!
for _ in $(seq 1 50); do [[ -s "${TMP}/port" ]] && break; sleep 0.1; done
PORT="$(head -1 "${TMP}/port")"
[[ "${PORT}" =~ ^[0-9]+$ ]] || { echo "stub did not start"; exit 1; }
STUB="http://127.0.0.1:${PORT}/"

# The functions under test, from the scripts as shipped.
fn() { awk -v f="^$2\\\\(\\\\) \\\\{" '$0 ~ f { on = 1 } on { print } on && /^}/ { exit }' "$1"; }
grep -E '^NULL_SEALED_(WALK|BATCH)=' "${KIT}/host/health.sh" >"${TMP}/health-fns.sh"
{ fn "${KIT}/host/health.sh" last_sealed; fn "${KIT}/host/health.sh" check_null_run; } >>"${TMP}/health-fns.sh"
grep -E '^NULL_SEALED_WALK=' "${KIT}/smoke.sh" >"${TMP}/smoke-fns.sh"
{ fn "${KIT}/smoke.sh" last_sealed; fn "${KIT}/smoke.sh" check_null_sealed; } >>"${TMP}/smoke-fns.sh"

# health.sh's check, with printing stand-ins. Prints its journal lines;
# "ALERT null_run: ..." is what would go to Telegram.
# shellcheck disable=SC2034,SC2329 # used by the sourced functions
run_health() {
  (
    SOVA_SIP6=1 NULL_SEALED_MAX_MIN=45 SOVA_URL="${STUB}"
    say() { echo "  log   $*"; }
    net_alert() { local k="$1"; shift; echo "  ALERT ${k}: $*"; }
    clear_alert() { echo "  (clear $1)"; }
    systemctl() { [[ "$1" == is-active ]] && echo inactive; return 0; }
    # shellcheck source=/dev/null
    source "${TMP}/health-fns.sh"
    check_null_run "$1"
  )
}
# smoke.sh's check, its public RPC pointed at the stub, no pacing sleeps.
# shellcheck disable=SC2034,SC2329 # used by the sourced functions
run_smoke() {
  (
    SOVA_SIP6=1 NULL_SEALED_MAX_MIN=45 RPC_HOST=stub.invalid
    ok() { echo "  ok    $*"; }
    bad() { echo "  FAIL  $*"; }
    sleep() { :; }
    curl() { local a=() x; for x in "$@"; do a+=("${x/https:\/\/stub.invalid\//${STUB}}"); done; command curl "${a[@]}"; }
    # shellcheck source=/dev/null
    source "${TMP}/smoke-fns.sh"
    check_null_sealed "$1"
  )
}

case_() { # name scenario-json expect-health(alert|quiet) expect-smoke(ok|fail) must-match-health-ERE
  local name="$1" scen="$2" eh="$3" es="$4" re="${5:-}" out n head
  echo "${scen}" >"${TMP}/scen.json"
  head="$(jq -r .head <<<"${scen}")"
  echo "== ${name}"
  echo "   scenario ${scen}"
  : >"${TMP}/count"
  out="$(run_health "${head}")"
  n="$(wc -l <"${TMP}/count" | tr -d ' ')"
  echo "${out}"
  echo "   (health.sh: ${n} batched RPC call(s))"
  local got=quiet
  grep -q '^  ALERT null_run' <<<"${out}" && got=alert
  if [[ "${got}" == "${eh}" ]] && { [[ -z "${re}" ]] || grep -qE -- "${re}" <<<"${out}"; }; then
    PASS=$((PASS + 1)); echo "PASS  health.sh: ${eh}"
  else
    FAIL=$((FAIL + 1)); echo "FAIL  health.sh: expected ${eh}${re:+ matching /${re}/}, got ${got}"
  fi
  : >"${TMP}/count"
  out="$(run_smoke "${head}")"
  n="$(wc -l <"${TMP}/count" | tr -d ' ')"
  echo "${out}"
  echo "   (smoke.sh: ${n} batched RPC call(s))"
  got=ok
  grep -q '^  FAIL' <<<"${out}" && got=fail
  if [[ "${got}" == "${es}" ]]; then
    PASS=$((PASS + 1)); echo "PASS  smoke.sh: ${es}"
  else
    FAIL=$((FAIL + 1)); echo "FAIL  smoke.sh: expected ${es}, got ${got}"
  fi
  echo
}

# The four required cases.
case_ "recent sealed block (12 min ago, 75 s blocks)" \
  '{"head":5000,"interval":75,"sealed":[4990,4000]}' quiet ok 'newest sealed block 4990 is 12 min old'
case_ "last sealed 50 min ago (keeper stopped, 75 s blocks)" \
  '{"head":5000,"interval":75,"sealed":[4960]}' alert fail 'no sealed block for 50 min .*sova-keeper here is inactive'
case_ "a 3 s burst, sealed 35 min ago (700 null blocks; smoke's 600-block cap: not judged)" \
  '{"head":5000,"interval":3,"sealed":[4300]}' quiet ok 'newest sealed block 4300 is 35 min old'
case_ "a 3 s burst, keeper stopped 50 min ago (1000 null blocks; smoke: not judged)" \
  '{"head":5000,"interval":3,"sealed":[4000]}' alert ok 'no sealed block for over 45 min: blocks 4[01][0-9]{2}\.\.5000 are all null'
case_ "a 1 s burst, sealed 33 min ago (2000 null blocks, past health's 1800 cap: not judged)" \
  '{"head":5000,"interval":1,"sealed":[3000]}' quiet ok 'span only 29 min; not judged'
case_ "RPC hiccup: HTTP 503" \
  '{"head":5000,"interval":75,"sealed":[],"mode":"http_error"}' quiet fail 'could not read'
case_ "RPC hiccup: one block comes back null" \
  '{"head":5000,"interval":75,"sealed":[],"mode":"null_result"}' quiet fail 'could not read'
case_ "RPC hiccup: a batch answer is one short" \
  '{"head":5000,"interval":75,"sealed":[],"mode":"short"}' quiet fail 'could not read'
# The false alarms the count rule gave, and the edges.
case_ "demand mode + burst: sealed 28 min ago, 560 null blocks since (old rule: alert)" \
  '{"head":5000,"interval":3,"sealed":[4440]}' quiet ok 'newest sealed block 4440 is 28 min old'
case_ "Zcash slow: no block for 50 min, sealed just before (block_age's finding)" \
  '{"head":5000,"interval":75,"head_age":3000,"sealed":[4999]}' quiet ok 'a slow Zcash or a stuck Sova'
case_ "young chain: 10 null blocks since genesis, 11 min" \
  '{"head":10,"interval":75,"sealed":[]}' quiet ok 'no sealed block since genesis'
case_ "old chain never sealed: 50 blocks since genesis, 61 min" \
  '{"head":50,"interval":75,"sealed":[]}' alert fail 'no sealed block since genesis'
case_ "head is sealed" \
  '{"head":5000,"interval":75,"sealed":[5000]}' quiet ok 'newest sealed block 5000 is 0 min old'

echo "${PASS} passed, ${FAIL} failed"
[[ "${FAIL}" == 0 ]]
