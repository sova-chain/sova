#!/usr/bin/env bash
# infra/testnet/test/zebra-kit-stub.sh -- offline tests of the zebrad
# rollout kit (docs/ops/nu7-upgrade.md, K2 K3 K7 K12):
#   zebra-ready  host/zebra-ready.sh (the readiness wait after a zebrad
#                restart) against a fake zebrad and a fake reference
#                (python3 JSON-RPC stub on 127.0.0.1) and a fake clock, with
#                stub journalctl (canned zebrad log lines) and docker: a
#                clean restart, a state format upgrade (finishing / never
#                finishing), no RPC, stuck behind the reference, a syncing
#                zebrad, no reference, a new zebrad, NU7 at / not at the
#                height, the image digest, a panic, `before`, timeout 0
#   setup-host   setup_zebrad's restart sequence (before, mute, restart,
#                wait; a failed wait fails it; unchanged: none of it) and the
#                keeper-pause check, from setup-host.sh as shipped
#   maint        host/maint.sh: mute (bounds, extend-only), unmute,
#                keeper-pause / keeper-resume (marker, stop and start order,
#                a stopped burner stays stopped, a node that doesn't answer)
#   health       host/health.sh: mute active / expired / unreadable / too
#                long (logged, not sent, no stamp), keeper paused vs keeper
#                down, sova-node stopped by the pause
#   smoke        smoke.sh hosts: the zebrad digest and NU7 checks, --only,
#                a paused keeper
# Functions are taken from the scripts as shipped; host commands (systemctl,
# docker, journalctl, curl for Telegram, date, sleep) are stand-ins.
#
#   test/zebra-kit-stub.sh
#
# Needs python3, curl, jq. Touches nothing but a temp dir and the stub.
# pass/flunk always return 0, so `test && pass || flunk` is a safe if/else.
# shellcheck disable=SC2015,SC2001
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
pass() { PASS=$((PASS + 1)); echo "PASS  $*"; }
flunk() { FAIL=$((FAIL + 1)); echo "FAIL  $*"; }
expect() { # description ERE text
  if grep -qE -- "$2" <<<"$3"; then pass "$1"; else flunk "$1 (no /$2/)"; fi
}
expect_not() { # description ERE text
  if grep -qE -- "$2" <<<"$3"; then flunk "$1 (found /$2/)"; else pass "$1"; fi
}
# One function from a script, as shipped (a one-line function ends on its line).
fn() { awk -v f="^$2\\\\(\\\\) \\\\{" '$0 ~ f { on = 1; first = 1 } on { print } on && (/^}/ || (first && /}[[:space:]]*$/)) { exit } { first = 0 }' "$1"; }

T0=1790900000
CLOCK="${TMP}/clock"
echo "${T0}" >"${CLOCK}"
echo "${T0}" >"${TMP}/t0"

# ---- stub commands on PATH (zebra-ready.sh and maint.sh run as scripts) ------
BIN="${TMP}/bin"
mkdir -p "${BIN}" "${TMP}/units"
cat >"${BIN}/date" <<EOF
#!/bin/bash
if [ "\$1" = "+%s" ]; then cat "${CLOCK}"; else exec /bin/date "\$@"; fi
EOF
cat >"${BIN}/sleep" <<EOF
#!/bin/bash
c=\$(cat "${CLOCK}"); echo \$((c + \${1%.*})) >"${CLOCK}"
EOF
# journalctl: the canned zebrad lines ("<offset from t0> <text>") logged by now.
cat >"${BIN}/journalctl" <<EOF
#!/bin/bash
t0=\$(cat "${TMP}/t0"); now=\$(cat "${CLOCK}")
[ -f "${TMP}/journal" ] || exit 0
while IFS=' ' read -r off text; do [ \$((t0 + off)) -le "\$now" ] && echo "\$text"; done <"${TMP}/journal"
exit 0
EOF
# docker: the running container's ref (docker_ref) and RepoDigests (docker_digests).
cat >"${BIN}/docker" <<EOF
#!/bin/bash
case "\$1 \$2" in
  "inspect zebrad")
    [ -f "${TMP}/docker_ref" ] || exit 1
    case "\$*" in *Config.Image*) cat "${TMP}/docker_ref" ;; *) echo sha256:0f0f ;; esac ;;
  "image inspect") cat "${TMP}/docker_digests" 2>/dev/null ;;
  *) exit 1 ;;
esac
EOF
# systemctl: unit states in units/<unit>; a paused keeper's units don't start
# (the ConditionPathExists in both units); every call logged.
cat >"${BIN}/systemctl" <<EOF
#!/bin/bash
echo "\$*" >>"${TMP}/systemctl.log"
[ "\$2" = --quiet ] && set -- "\$1" "\$3"
u="${TMP}/units/\$2"
case "\$1" in
  is-active) s=\$(cat "\$u" 2>/dev/null || echo inactive); echo "\$s"; [ "\$s" = active ] ;;
  cat) [ -f "\$u" ] ;;
  stop) [ -f "\$u" ] && echo inactive >"\$u"; exit 0 ;;
  start)
    if [ -f "${TMP}/etc/.keeper-paused" ] && { [ "\$2" = sova-node ] || [ "\$2" = sova-keeper ]; }; then exit 0; fi
    echo active >"\$u" ;;
  *) exit 0 ;;
esac
EOF
printf '#!/bin/bash\necho sova-test-host\n' >"${BIN}/hostname"
chmod +x "${BIN}"/*
# maint.sh's curl (keeper-resume's RPC probe): answers unless node_rpc_down exists.
MBIN="${TMP}/mbin"
mkdir -p "${MBIN}"
printf '#!/bin/bash\n[ ! -f "%s/node_rpc_down" ] && echo "{\\"result\\":\\"0x10\\"}"\n' "${TMP}" >"${MBIN}/curl"
chmod +x "${MBIN}/curl"

# ---- the fake zebrad (/) and reference (/ref): JSON-RPC, answers from a
# scenario and the fake clock. zebrad answers from t0 + rpc_at (null: never)
# with tip0 + rate * (seconds since), up to tip_cap; ref_tip (null: no
# answer) grows by ref_rate. nu7: upgrades["77190ad9"].activationheight.
cat >"${TMP}/stub.py" <<'EOF'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
SCEN, CLOCK, T0 = sys.argv[1], sys.argv[2], sys.argv[3]
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        s = json.load(open(SCEN))
        now, t0 = int(open(CLOCK).read()), int(open(T0).read())
        req = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        res = None
        if self.path.startswith("/ref"):
            if s.get("ref_tip") is not None:
                res = s["ref_tip"] + int((now - t0) * s.get("ref_rate", 0))
        elif s.get("rpc_at") is not None and now >= t0 + s["rpc_at"]:
            tip = s["tip0"] + int((now - t0 - s["rpc_at"]) * s.get("rate", 0))
            if s.get("tip_cap") is not None: tip = min(tip, s["tip_cap"])
            if req["method"] == "getblockcount":
                res = tip
            else:
                ups = {"37a5165b": {"name": "NU6.3", "activationheight": 4134000, "status": "active"}}
                if s.get("nu7") is not None:
                    ups["77190ad9"] = {"name": "NU7", "activationheight": s["nu7"], "status": "pending"}
                res = {"blocks": tip, "estimatedheight": tip + 1, "upgrades": ups,
                       "consensus": {"chaintip": "37a5165b", "nextblock": "37a5165b"}}
        if res is None:
            self.send_response(503); self.end_headers(); return
        data = json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": res}).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
srv = HTTPServer(("127.0.0.1", 0), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()
EOF
echo '{}' >"${TMP}/scen.json"
python3 -u "${TMP}/stub.py" "${TMP}/scen.json" "${CLOCK}" "${TMP}/t0" >"${TMP}/port" &
STUB_PID=$!
for _ in $(seq 1 50); do [[ -s "${TMP}/port" ]] && break; /bin/sleep 0.1; done
PORT="$(head -1 "${TMP}/port")"
[[ "${PORT}" =~ ^[0-9]+$ ]] || { echo "stub did not start"; exit 1; }
ZURL="http://127.0.0.1:${PORT}/"
REF="http://127.0.0.1:${PORT}/ref"
DIG="sha256:$(printf 'ab%.0s' $(seq 1 32))"

# ---- zebra-ready.sh ----------------------------------------------------------------
echo "==== zebra-ready.sh (the readiness wait)"
# ready <scenario-json> <journal-lines> <env-lines> <args...>: one run at a
# fresh clock (t0 + 14: the restart took 14 s). Sets OUT and RC.
ready() {
  local scen="$1" journal="$2" envl="$3"
  shift 3
  echo "${scen}" >"${TMP}/scen.json"
  printf '%s' "${journal}" >"${TMP}/journal"
  printf '%s\n' "ZEBRA_RPC_PORT=1" "ZCASH_REFERENCE_URLS=${REF},https://explorer.invalid/api/block/{height}" "${envl}" >"${TMP}/ready.env"
  echo $((T0 + 14)) >"${CLOCK}"
  OUT="$(PATH="${BIN}:${PATH}" ZEBRA_READY_ENV="${TMP}/ready.env" ZEBRA_URL="${ZURL}" bash "${KIT}/host/zebra-ready.sh" "$@" 2>&1)"
  RC=$?
  ELAPSED=$(($(cat "${CLOCK}") - T0))
  echo "${OUT}" | sed 's/^/    /'
}
W=(wait --since "${T0}" --started $((T0 + 14)) --pre-tip 1000 --pre-lag 0)
rm -f "${TMP}/docker_ref" "${TMP}/docker_digests"

echo "== a clean restart: RPC at 30 s, no format upgrade, catches up with the reference"
ready '{"rpc_at":30,"tip0":1000,"rate":0.1,"ref_tip":1005}' "20 INFO zebra_state: trying to open current database format running_version=28.0.0
" "" "${W[@]}"
expect "ready (exit 0)" '\[zebra-ready\] READY 54 s after the restart began' "${OUT}"
[[ ${RC} == 0 ]] && pass "... exit 0" || flunk "... exit ${RC}"
expect "... timings: stop+start 14 s, RPC at 34 s, format at 34, caught up at 54" 'stop\+start 14 s, RPC at 34 s, format done at 34 s \(upgrade: none\), caught up at 54 s; tip 1002 .*reference 1005, before the restart 1000' "${OUT}"
expect "... KIT-OUT timings line for deploy.sh" '^KIT-OUT zebra_ready total=54s restart=14s rpc=34s format=34s tip=54s upgrade=none$' "${OUT}"
expect "... says how zebrad opened its state" 'no upgrade on this start \(trying to open current database format\)' "${OUT}"

echo "== a state format upgrade (28.0.0 -> 28.1.0) that takes 3 min"
ready '{"rpc_at":30,"tip0":1000,"rate":1,"ref_tip":1005}' "25 INFO zebra_state: trying to open older database format: launching upgrade task running_version=28.1.0 disk_version=28.0.0
120 INFO zebra_state: marked database format as upgraded running_version=28.1.0 disk_version=28.0.0 format_upgrade_version=28.1.0
200 INFO zebra_state: database format is valid running_version=28.1.0 initial_disk_version=28.0.0
" "" "${W[@]}"
expect "ready once the upgrade is valid (204 s)" 'READY 204 s after' "${OUT}"
expect "... names the upgrade" 'state format upgrade \(running_version=28.1.0 disk_version=28.0.0\) finished 204 s after the restart' "${OUT}"
expect "... progress lines while it runs" '\+[0-9]+ s: rpc up, tip [0-9]+ \(need 1002\), format running_version=28.1.0' "${OUT}"
expect "... KIT-OUT upgrade=yes" 'KIT-OUT zebra_ready total=204s .* upgrade=yes' "${OUT}"

echo "== a format upgrade that never finishes"
ready '{"rpc_at":30,"tip0":1000,"rate":1,"ref_tip":1005}' "25 INFO zebra_state: trying to open older database format: launching upgrade task running_version=28.1.0 disk_version=28.0.0
" "" "${W[@]}"
expect "NOT READY after 15 min: the upgrade is still running" 'NOT READY: after 15 min: state format upgrade \(running_version=28.1.0 disk_version=28.0.0\) still running' "${OUT}"
[[ ${RC} == 1 && ${ELAPSED} -ge 900 && ${ELAPSED} -lt 920 ]] && pass "... exit 1 at the timeout (${ELAPSED} s)" || flunk "... exit ${RC} at ${ELAPSED} s"

echo "== zebrad never answers"
ready '{"rpc_at":null,"tip0":1000,"ref_tip":1005}' "" "" "${W[@]}"
expect "NOT READY: RPC not answering" 'NOT READY: after 15 min: zebrad RPC not answering at http://127.0.0.1:[0-9]+/\. Stop the rollout' "${OUT}"
[[ ${RC} == 1 ]] && pass "... exit 1" || flunk "... exit ${RC}"

echo "== answers but stays 50 behind the reference (was level before)"
ready '{"rpc_at":30,"tip0":1000,"rate":0,"ref_tip":1050}' "" "" "${W[@]}"
expect "NOT READY: tip vs need" 'NOT READY: after 15 min: tip 1000, need >= 1047 \(before the restart 1000, reference 1050\)' "${OUT}"

echo "== a zebrad that was 50 behind before (still syncing) only has to get back there"
ready '{"rpc_at":30,"tip0":995,"rate":0.1,"ref_tip":1050}' "" "" wait --since "${T0}" --started $((T0 + 14)) --pre-tip 1000 --pre-lag 50
expect "ready at tip >= 1050 - 50 - 3 (997, at 54 s)" 'READY [0-9]+ s after .*caught up at 54 s; tip 99[7-9] ' "${OUT}"

echo "== no reference answers: back to the pre-restart tip (- 3)"
ready '{"rpc_at":30,"tip0":990,"rate":0.2,"ref_tip":null}' "" "" "${W[@]}"
expect "ready at tip >= 997" 'READY [0-9]+ s after .*tip 99[7-9] \(estimatedheight [0-9]+, before the restart 1000\)' "${OUT}"
expect "... no format line: goes on after the grace" 'no upgrade on this start \(no format line in the zebrad journal after 30 s\)' "${OUT}"

echo "== a new zebrad (no pre-restart tip), references far ahead: catch-up not gated"
ready '{"rpc_at":20,"tip0":10,"rate":1,"ref_tip":4415000}' "5 INFO zebra_state: creating new database with the current format running_version=28.0.0
" "" wait --since "${T0}" --started $((T0 + 14)) --pre-tip - --pre-lag 0
expect "ready as soon as the RPC answers" 'READY 24 s after' "${OUT}"
expect "... says the catch-up is not gated" "catch-up not gated" "${OUT}"

echo "== NU7_ACTIVATION_HEIGHT set, zebrad knows NU7 there"
ready '{"rpc_at":30,"tip0":1000,"rate":1,"ref_tip":1000,"nu7":4416000}' "" "NU7_ACTIVATION_HEIGHT=4416000" "${W[@]}"
expect "ready, NU7 checked" 'nu7: zebrad activates NU7 \(77190ad9\) at 4416000 = NU7_ACTIVATION_HEIGHT' "${OUT}"
expect "... READY" 'READY .*NU7 yes$' "${OUT}"

echo "== NU7_ACTIVATION_HEIGHT set, a pre-NU7 zebrad"
ready '{"rpc_at":30,"tip0":1000,"rate":1,"ref_tip":1000}' "" "NU7_ACTIVATION_HEIGHT=4416000" "${W[@]}"
expect "NOT READY at once: no NU7" 'NOT READY: zebrad has no NU7 upgrade \(77190ad9\) but NU7_ACTIVATION_HEIGHT is 4416000' "${OUT}"
[[ ${RC} == 1 && ${ELAPSED} -lt 60 ]] && pass "... without waiting out the timeout (${ELAPSED} s)" || flunk "... exit ${RC} at ${ELAPSED} s"

echo "== NU7 at another height"
ready '{"rpc_at":30,"tip0":1000,"rate":1,"ref_tip":1000,"nu7":4416003}' "" "NU7_ACTIVATION_HEIGHT=4416000" "${W[@]}"
expect "NOT READY at once: another height" 'NOT READY: zebrad activates NU7 at 4416003, but NU7_ACTIVATION_HEIGHT is 4416000' "${OUT}"

echo "== ZEBRA_IMAGE_DIGEST pinned, the container runs it"
echo "zfnd/zebra:7.0.0@${DIG}" >"${TMP}/docker_ref"
ready '{"rpc_at":30,"tip0":1000,"rate":1,"ref_tip":1000}' "" "ZEBRA_IMAGE_DIGEST=${DIG}" "${W[@]}"
expect "ready, image checked" "image: the running container is ${DIG}" "${OUT}"
echo "== ... the digest only in RepoDigests (container started by tag)"
echo "zfnd/zebra:7.0.0" >"${TMP}/docker_ref"
echo "zfnd/zebra@${DIG} " >"${TMP}/docker_digests"
ready '{"rpc_at":30,"tip0":1000,"rate":1,"ref_tip":1000}' "" "ZEBRA_IMAGE_DIGEST=${DIG}" "${W[@]}"
expect "ready, image checked" "image: the running container is ${DIG}" "${OUT}"
echo "== ... another image"
echo "zfnd/zebra:6.3.0@sha256:$(printf 'cd%.0s' $(seq 1 32))" >"${TMP}/docker_ref"
: >"${TMP}/docker_digests"
ready '{"rpc_at":30,"tip0":1000,"rate":1,"ref_tip":1000}' "" "ZEBRA_IMAGE_DIGEST=${DIG}" "${W[@]}"
expect "NOT READY at once: wrong image" "NOT READY: the running zebrad is 'zfnd/zebra:6.3.0@sha256:cdcd.*', not ZEBRA_IMAGE_DIGEST ${DIG}" "${OUT}"
rm -f "${TMP}/docker_ref" "${TMP}/docker_digests"

echo "== zebrad panics after the restart"
ready '{"rpc_at":null,"tip0":1000}' "40 The application panicked (crashed).
41 Message:  unexpected invalid database format: delete and re-sync the database at /var/lib/sova/zebrad
" "" "${W[@]}"
expect "NOT READY at once: the panic line" 'NOT READY: zebrad panicked after the restart: The application panicked' "${OUT}"
[[ ${RC} == 1 && ${ELAPSED} -lt 60 ]] && pass "... at once (${ELAPSED} s)" || flunk "... exit ${RC} at ${ELAPSED} s"

echo "== ZEBRA_READY_TIMEOUT_MIN=0"
ready '{"rpc_at":null}' "" "ZEBRA_READY_TIMEOUT_MIN=0" "${W[@]}"
expect "doesn't wait" 'ZEBRA_READY_TIMEOUT_MIN=0: not waiting' "${OUT}"
[[ ${RC} == 0 ]] && pass "... exit 0" || flunk "... exit ${RC}"

echo "== before: tip and lag behind the reference"
echo "${T0}" >"${TMP}/t0"
ready '{"rpc_at":-100,"tip0":1000,"rate":0,"ref_tip":1004}' "" "" before
[[ "${OUT}" == "1000 4" ]] && pass "before = '1000 4'" || flunk "before = '${OUT}'"
ready '{"rpc_at":null,"ref_tip":1004}' "" "" before
[[ "${OUT}" == "- 0" ]] && pass "before, zebrad down = '- 0'" || flunk "before, zebrad down = '${OUT}'"

# ---- setup-host.sh: setup_zebrad's restart sequence, keeper_paused ------------------
echo
echo "==== setup-host.sh (setup_zebrad, keeper_paused)"
SH="${KIT}/host/setup-host.sh"
{ fn "${SH}" setup_zebrad; fn "${SH}" keeper_paused; fn "${SH}" paused_since; } >"${TMP}/setup-fns.sh"
# shellcheck disable=SC2034,SC2329 # used by the sourced functions
run_setup_zebrad() { # ready-exit(0|1) zebrad-active(0|1) same-sum(0|1) [mute-min]
  (
    ETC="${TMP}/setup-etc" HERE="${TMP}/setup-here" UNIT_DIR="${TMP}/setup-units" ENV_FILE="${TMP}/host.env"
    ZEBRA_IMAGE=zfnd/zebra:7.0.0 ZEBRA_IMAGE_DIGEST="${DIG}" ZEBRA_RESTART_MUTE_MIN="${4:-15}"
    rm -rf "${ETC}" "${HERE}" && mkdir -p "${ETC}" "${HERE}" "${UNIT_DIR}"
    : >"${TMP}/calls"
    log() { echo "  log   $*"; }
    die() { echo "  DIE   $*"; exit 1; }
    zebra_ref() { printf '%s@%s' "${ZEBRA_IMAGE}" "${ZEBRA_IMAGE_DIGEST}"; }
    write_zebrad_files() { echo "ZEBRA_IMAGE_REF=$1" >"${ETC}/zebrad.env"; echo "[network]" >"${ETC}/zebrad.toml"; }
    docker() { echo "docker $*" >>"${TMP}/calls"; }
    install() { :; }
    date() { echo "${T0}"; }
    systemctl() {
      [[ "$1" == restart || "$1" == daemon-reload || "$1" == enable ]] && echo "systemctl $*" >>"${TMP}/calls"
      [[ "$1" != is-active ]] || [[ "$2 $3" == "--quiet zebrad" && "${ACTIVE}" == 1 ]]
    }
    bash() { # the kit's own helpers, logged
      echo "$* $(sed -n 's/^ZEBRA_READY_ENV=//p' /dev/null)" >>"${TMP}/calls"
      case "$1" in
        */zebra-ready.sh) [[ "$2" == before ]] && echo "1000 2" && return 0; return "${READY_RC}" ;;
        */maint.sh) return 0 ;;
      esac
    }
    ACTIVE="$2" READY_RC="$1"
    write_zebrad_files "$(zebra_ref)"
    [[ "$3" == 1 ]] && cat "${ETC}/zebrad.env" "${ETC}/zebrad.toml" | sha256sum | cut -d' ' -f1 >"${ETC}/.zebrad.sum"
    # shellcheck source=/dev/null
    source "${TMP}/setup-fns.sh"
    setup_zebrad
  )
}
OUT="$(run_setup_zebrad 0 1 0)"
RC=$?
CALLS="$(cat "${TMP}/calls")"
echo "${OUT}" | sed 's/^/    /'
echo "${CALLS}" | sed 's/^/    call  /'
expect "changed image: before, then mute 15, then restart, then wait" \
  "$(printf '%s' "zebra-ready.sh before.*maint.sh mute 15 zebrad restart \(zebra:7.0.0@${DIG}\).*systemctl restart zebrad.*zebra-ready.sh wait --since ${T0} --started ${T0} --pre-tip 1000 --pre-lag 2")" \
  "$(tr '\n' ' ' <<<"${CALLS}")"
expect "... logs the (re)start as before (runbook greps it)" "zebrad \(re\)started with zfnd/zebra:7.0.0@${DIG}" "${OUT}"
[[ ${RC} == 0 ]] && pass "... returns 0 when ready" || flunk "... returned ${RC}"
OUT="$(run_setup_zebrad 1 1 0)"
RC=$?
echo "${OUT}" | sed 's/^/    /'
expect "not ready: setup-host.sh dies (deploy.sh stops the rollout)" 'DIE   zebrad is not ready after the restart \(above\); the rollout stops here' "${OUT}"
[[ ${RC} != 0 ]] && pass "... non-zero" || flunk "... returned 0"
OUT="$(run_setup_zebrad 0 1 1)"
CALLS="$(cat "${TMP}/calls")"
expect "unchanged and running: no restart" 'zebrad unchanged and running' "${OUT}"
expect_not "... no mute, no wait, no restart" 'maint.sh|zebra-ready.sh|restart zebrad' "${CALLS}"
run_setup_zebrad 0 1 0 0 >/dev/null
CALLS="$(cat "${TMP}/calls")"
expect_not "ZEBRA_RESTART_MUTE_MIN=0: no mute" 'maint.sh' "${CALLS}"
expect "... still restarts and waits" 'zebra-ready.sh wait' "${CALLS}"
run_setup_zebrad 0 0 1 >/dev/null
expect "not running (same config): restarted" 'systemctl restart zebrad' "$(cat "${TMP}/calls")"
# shellcheck disable=SC2034,SC2329
kp() { # role marker(0|1)
  (
    ROLE="$1" KEEPER_PAUSED_FILE="${TMP}/kp-marker"
    rm -f "${KEEPER_PAUSED_FILE}"
    [[ "$2" == 1 ]] && printf 'since=%s\nburner=active\nreason=x\n' "${T0}" >"${KEEPER_PAUSED_FILE}"
    # shellcheck source=/dev/null
    source "${TMP}/setup-fns.sh"
    keeper_paused && echo "paused since $(paused_since)" || echo "not paused"
  )
}
[[ "$(kp keeper 1)" == "paused since ${T0}" ]] && pass "keeper_paused: keeper + marker" || flunk "keeper_paused: keeper + marker: $(kp keeper 1)"
[[ "$(kp keeper 0)" == "not paused" ]] && pass "keeper_paused: keeper, no marker" || flunk "keeper_paused: keeper, no marker"
[[ "$(kp seed 1)" == "not paused" ]] && pass "keeper_paused: a marker on a seed is ignored" || flunk "keeper_paused: seed + marker"
expect "setup_node: returns before any sova-node restart while paused" \
  'if keeper_paused; then.*NOT started or restarted: the keeper is paused.*return 0.*fi.*local bin_changed=0' \
  "$(fn "${SH}" setup_node | tr '\n' ' ')"
expect "setup_keeper: never starts or restarts the burner while paused" \
  'if keeper_paused; then.*sova-keeper NOT started or restarted.*elif ! systemctl is-active --quiet sova-keeper' \
  "$(fn "${SH}" setup_keeper | tr '\n' ' ')"

# ---- maint.sh ---------------------------------------------------------------------------
echo
echo "==== maint.sh (mute, keeper pause)"
ETCD="${TMP}/etc"
maint() { # args... -> OUT, RC
  OUT="$(PATH="${MBIN}:${BIN}:${PATH}" SOVA_ETC="${ETCD}" SOVA_BIN_LINK="${TMP}/no-link" bash "${KIT}/host/maint.sh" "$@" 2>&1)"
  RC=$?
  echo "${OUT}" | sed 's/^/    /'
}
mkdir -p "${ETCD}"
echo "${T0}" >"${CLOCK}"
maint mute 15 "zebrad restart (zebra:7.0.0)"
[[ "$(sed -n 1p "${ETCD}/.mute-until")" == $((T0 + 900)) ]] && pass "mute 15: marker = now + 900 s" || flunk "mute 15: marker $(cat "${ETCD}/.mute-until" 2>/dev/null)"
[[ "$(sed -n 2p "${ETCD}/.mute-until")" == "zebrad restart (zebra:7.0.0)" ]] && pass "... with the reason" || flunk "... reason"
expect "... says until when" 'Telegram alerts muted until .* \(15 min\): zebrad restart' "${OUT}"
maint mute 5
[[ "$(sed -n 1p "${ETCD}/.mute-until")" == $((T0 + 900)) ]] && pass "mute 5 under a longer mute: never shortens it" || flunk "mute 5 shortened it"
maint mute 30
[[ "$(sed -n 1p "${ETCD}/.mute-until")" == $((T0 + 1800)) ]] && pass "mute 30: extends it" || flunk "mute 30 didn't extend"
for v in 0 241 abc ""; do
  maint mute "${v}"
  [[ ${RC} != 0 ]] && grep -q "mute needs 1..240 minutes" <<<"${OUT}" && pass "mute '${v}' refused" || flunk "mute '${v}' accepted"
done
maint unmute
[[ ! -e "${ETCD}/.mute-until" ]] && pass "unmute removes the marker" || flunk "unmute left the marker"

units() { echo "$1" >"${TMP}/units/sova-keeper"; echo "$2" >"${TMP}/units/sova-node"; }
: >"${TMP}/systemctl.log"
rm -f "${TMP}/units/sova-keeper" "${TMP}/units/sova-node"
maint keeper-pause
[[ ${RC} != 0 ]] && grep -q "keeper-pause is for the keeper host" <<<"${OUT}" && pass "keeper-pause refused where there is no sova-keeper unit" || flunk "keeper-pause on a non-keeper"
units active active
: >"${TMP}/systemctl.log"
maint keeper-pause "NU7: old-rules chain"
[[ ${RC} == 0 ]] && pass "keeper-pause: exit 0" || flunk "keeper-pause: exit ${RC}"
expect "... marker: since, burner=active, reason" "^since=${T0} burner=active reason=NU7: old-rules chain $" "$(tr '\n' ' ' <"${ETCD}/.keeper-paused")"
expect "... stops sova-keeper, then sova-node" '^stop sova-keeper stop sova-node $' "$(grep -E '^(stop|start)' "${TMP}/systemctl.log" | tr '\n' ' ')"
[[ "$(cat "${TMP}/units/sova-keeper") $(cat "${TMP}/units/sova-node")" == "inactive inactive" ]] && pass "... both stopped" || flunk "... not both stopped"
expect "... says so" 'keeper PAUSED \(NU7: old-rules chain\): sova-keeper inactive, sova-node inactive' "${OUT}"
PATH="${BIN}:${PATH}" systemctl start sova-node
[[ "$(cat "${TMP}/units/sova-node")" == inactive ]] && pass "(the units' ConditionPathExists, emulated: a start while paused does nothing)" || flunk "start while paused"
echo $((T0 + 60)) >"${CLOCK}"
maint keeper-pause again
expect "keeper-pause again: already paused, marker kept" 'already paused since' "${OUT}"
grep -q "^since=${T0}$" "${ETCD}/.keeper-paused" && pass "... original since kept" || flunk "... since changed"
: >"${TMP}/systemctl.log"
maint keeper-resume
[[ ${RC} == 0 ]] && pass "keeper-resume: exit 0" || flunk "keeper-resume: exit ${RC}"
[[ ! -e "${ETCD}/.keeper-paused" ]] && pass "... marker removed" || flunk "... marker still there"
expect "... starts sova-node, then sova-keeper" '^start sova-node start sova-keeper $' "$(grep -E '^(stop|start)' "${TMP}/systemctl.log" | tr '\n' ' ')"
expect "... waits for the node and settles 60 s" 'sova-node up \(RPC answers on 127.0.0.1:8545\); letting it settle 60 s' "${OUT}"
expect "... says so" 'keeper RESUMED: sova-node active, sova-keeper active' "${OUT}"
: >"${TMP}/systemctl.log"
maint keeper-resume
expect "keeper-resume when not paused: nothing started" 'not paused .*nothing started' "${OUT}"
expect_not "... no start" '^start' "$(cat "${TMP}/systemctl.log")"
units inactive active
maint keeper-pause
maint keeper-resume
expect "a burner stopped when paused stays stopped on resume" 'sova-keeper was inactive when paused, so it stays stopped' "${OUT}"
[[ "$(cat "${TMP}/units/sova-keeper")" == inactive ]] && pass "... still inactive" || flunk "... started"
units active active
maint keeper-pause
touch "${TMP}/node_rpc_down"
maint keeper-resume
[[ ${RC} != 0 ]] && pass "resume: sova-node RPC never answers: fails" || flunk "resume with a dead node: exit 0"
expect "... and doesn't start the burner" 'sova-keeper NOT started' "${OUT}"
[[ "$(cat "${TMP}/units/sova-keeper")" == inactive ]] && pass "... burner still stopped" || flunk "... burner started"
rm -f "${TMP}/node_rpc_down"

# ---- health.sh: mute and keeper pause --------------------------------------------------------
echo
echo "==== health.sh (mute, keeper pause)"
H="${KIT}/host/health.sh"
{
  grep -E '^(MUTE_FILE|MUTE_MAX_MIN|KEEPER_PAUSED_FILE|ALERT_REPEAT_MIN)=' "${H}"
  for f in fmt_time mute_init alert tg_send keeper_paused paused_text check_keeper node_paused; do fn "${H}" "${f}"; done
} >"${TMP}/health-fns.sh"
for f in fmt_time mute_init alert tg_send keeper_paused paused_text check_keeper node_paused; do
  grep -q "^${f}() {" "${TMP}/health-fns.sh" || { echo "FAIL  ${f} not found in health.sh"; exit 1; }
done
# One health pass: mute_init, then an alert and the keeper checks. SENT
# lines are what went to Telegram.
# shellcheck disable=SC2034,SC2329 # used by the sourced functions
hpass() { # now keeper-state node-state
  (
    NOW="$1"
    STATE_DIR="${TMP}/hstate" HOST=sova-test TELEGRAM_BOT_TOKEN=t TELEGRAM_CHAT_ID=1 TELEGRAM_THREAD_ID=7
    mkdir -p "${STATE_DIR}"
    # shellcheck source=/dev/null
    source "${TMP}/health-fns.sh"
    MUTE_FILE="${ETCD}/.mute-until" KEEPER_PAUSED_FILE="${ETCD}/.keeper-paused"
    say() { echo "  log   $*"; }
    date() { if [[ "$1" == +%s ]]; then echo "${NOW}"; else command date "$@"; fi; }
    # alert() sends its output to /dev/null: record what went out in a file.
    curl() { cat >/dev/null; echo "  SENT  $(printf '%s ' "$@" | grep -o 'message_thread_id=.*')" >>"${TMP}/sent"; }
    clear_alert() { echo "  (clear $1)"; }
    systemctl() {
      case "$1 $2" in
        "cat sova-keeper") return 0 ;;
        "is-active --quiet") [[ "$3" == sova-keeper && "$KS" == active || "$3" == sova-node && "$NS" == active ]] ;;
        "is-active sova-keeper") echo "$KS" ;;
      esac
    }
    KS="$2" NS="$3"
    mute_init
    alert zebrad_down "zebrad RPC not answering at http://127.0.0.1:18232"
    check_keeper
    node_paused && echo "  (node checks skipped)"
    [[ ! -f "${TMP}/sent" ]] || cat "${TMP}/sent"
    rm -f "${TMP}/sent"
    return 0
  )
}
rm -rf "${TMP}/hstate" "${ETCD}/.mute-until" "${ETCD}/.keeper-paused"
OUT="$(hpass "${T0}" active active)"
echo "${OUT}"
expect "no mute: the alert is sent (to the thread)" 'SENT  message_thread_id=7 --data-urlencode text=\[sova sova-test\] zebrad_down: zebrad RPC not answering' "${OUT}"
[[ -f "${TMP}/hstate/zebrad_down.last" ]] && pass "... dedupe stamp written" || flunk "... no stamp"
expect_not "... keeper running: no keeper alert" 'ALERT keeper_down' "${OUT}"

rm -rf "${TMP}/hstate"
echo "${T0}" >"${CLOCK}"
maint mute 15 "zebrad restart (zebra:7.0.0)" >/dev/null
OUT="$(hpass $((T0 + 120)) active active)"
echo "${OUT}"
expect "mute active (maint.sh's marker): says so" 'mute: planned maintenance \(zebrad restart \(zebra:7.0.0\)\), no Telegram until .* \(13 min left\); findings are still logged' "${OUT}"
expect "... the finding is still logged, marked muted" 'log   ALERT zebrad_down: zebrad RPC not answering .* \[muted: planned maintenance until .*, not sent\]' "${OUT}"
expect_not "... nothing sent" 'SENT' "${OUT}"
[[ ! -f "${TMP}/hstate/zebrad_down.last" ]] && pass "... no dedupe stamp (sent at once if it outlasts the mute)" || flunk "... stamp written while muted"
[[ -f "${ETCD}/.mute-until" ]] && pass "... marker kept" || flunk "... marker removed while active"

OUT="$(hpass $((T0 + 901)) active active)"
echo "${OUT}"
expect "mute expired: marker removed, says so" 'mute: planned maintenance ended at .* \(zebrad restart \(zebra:7.0.0\)\): marker removed, alerts go to Telegram again' "${OUT}"
[[ ! -e "${ETCD}/.mute-until" ]] && pass "... marker gone" || flunk "... marker still there"
expect "... the alert is sent again" 'SENT  .*text=\[sova sova-test\] zebrad_down' "${OUT}"

printf 'soon\n' >"${ETCD}/.mute-until"
rm -rf "${TMP}/hstate"
OUT="$(hpass "${T0}" active active)"
expect "unreadable marker: removed, alerts on" "mute: .*is unreadable \('soon'\): removed, alerts on" "${OUT}"
expect "... sent" 'SENT' "${OUT}"
printf '%s\nforgotten\n' $((T0 + 2 * 86400)) >"${ETCD}/.mute-until"
rm -rf "${TMP}/hstate"
OUT="$(hpass "${T0}" active active)"
expect "a mute 2 days out: refused and removed" 'is more than 240 min away: refused and removed, alerts on' "${OUT}"
expect "... sent" 'SENT' "${OUT}"

rm -rf "${TMP}/hstate"
OUT="$(hpass "${T0}" inactive active)"
expect "keeper not paused, burner stopped: keeper_down" 'ALERT keeper_down: sova-keeper is inactive' "${OUT}"
units active active
echo "${T0}" >"${CLOCK}"
maint keeper-pause "NU7: old-rules chain" >/dev/null
rm -rf "${TMP}/hstate"
OUT="$(hpass $((T0 + 300)) inactive inactive)"
echo "${OUT}"
expect "keeper paused (maint.sh's marker): reported as planned" 'log   keeper paused \(planned\) since .* \(NU7: old-rules chain\): sova-keeper and sova-node stopped by keeper-pause' "${OUT}"
expect_not "... no keeper_down" 'ALERT keeper_down' "${OUT}"
expect "... keeper_down cleared" '\(clear keeper_down\)' "${OUT}"
expect "... sova-node stopped by the pause: said, sova_down and isolation cleared, node checks skipped" \
  'log   sova-node stopped: keeper paused \(planned\).*\(clear sova_down\).*\(clear keeper_no_peers\).*\(clear keeper_isolated\).*\(node checks skipped\)' "$(tr '\n' ' ' <<<"${OUT}")"
OUT="$(hpass $((T0 + 300)) inactive active)"
expect_not "paused but sova-node running (started by hand): node checks run" 'node checks skipped' "${OUT}"
maint keeper-resume >/dev/null
OUT="$(hpass $((T0 + 300)) active active)"
expect_not "resumed: no pause lines" 'keeper paused|node checks skipped' "${OUT}"
check_top() { grep -qF 'if systemctl is-enabled --quiet sova-node 2>/dev/null && ! node_paused; then' "${H}"; }
check_top && pass "health.sh: the sova-node block is skipped when node_paused" || flunk "health.sh: node_paused not wired into the sova-node block"
grep -qx 'mute_init' "${H}" && pass "health.sh: mute_init runs every pass" || flunk "health.sh: mute_init not called"

# ---- smoke.sh hosts: zebrad digest and NU7, --only, a paused keeper ------------------------
echo
echo "==== smoke.sh hosts (zebrad digest and NU7, --only, keeper pause)"
S="${KIT}/smoke.sh"
{ for f in check_zebra_image check_zebra_nu7 check_keeper_sealer cmd_hosts; do fn "${S}" "${f}"; done; } >"${TMP}/smoke-fns.sh"
# The canned host output. "<host> <line>" per line in ${TMP}/hosts-out.
# shellcheck disable=SC2034,SC2329 # used by the sourced functions
shosts() { # only digest nu7
  (
    # shellcheck source=/dev/null
    source "${KIT}/lib.sh"
    SERVERS=("sova-seed-1:cx33:fsn1:60:seed" "sova-rpc-1:cx23:nbg1:40:rpc" "sova-keeper-1:cx23:fsn1:40:keeper")
    OUT_DIR="${TMP}/sout" ONLY="$1" ZEBRA_IMAGE_DIGEST="$2" NU7_ACTIVATION_HEIGHT="$3" ZEBRA_RPC_PORT=18232 SOVA_SIP6=1 SOVA_SIP7=1
    ok() { echo "  ok    $*"; }
    bad() { echo "  FAIL  $*"; }
    note() { echo "  note  $*"; }
    kit_ssh() { echo "ssh $1" >>"${TMP}/ssh.log"; sed -n "s/^$1 //p" "${TMP}/hosts-out"; }
    # shellcheck source=/dev/null
    source "${TMP}/smoke-fns.sh"
    cmd_hosts
  )
}
common() { # host
  printf '%s svc zebrad active\n%s c5 enforcing\n%s c5rejects 0\n%s sip7feed 3\n' "$1" "$1" "$1" "$1"
}
{
  common sova-seed-1
  echo "sova-seed-1 svc sova-node active"
  echo "sova-seed-1 p2p 0 4"
  echo "sova-seed-1 zimg zfnd/zebra:7.0.0@${DIG} zfnd/zebra@${DIG},"
  echo "sova-seed-1 znu7 4416000"
  common sova-rpc-1
  echo "sova-rpc-1 svc sova-node active"
  echo "sova-rpc-1 p2p 1 3"
  echo "sova-rpc-1 zimg zfnd/zebra:6.3.0@sha256:52a67e543906c98a0ed1599e2ce3ee238fc05b40592ae26ee5914ddb6ede51e3 zfnd/zebra@sha256:52a67e543906c98a0ed1599e2ce3ee238fc05b40592ae26ee5914ddb6ede51e3,"
  echo "sova-rpc-1 znu7 none"
  common sova-keeper-1
  echo "sova-keeper-1 svc sova-node inactive"
  echo "sova-keeper-1 svc sova-keeper inactive"
  echo "sova-keeper-1 sealer 0xabc"
  echo "sova-keeper-1 p2p 1"
  echo "sova-keeper-1 zimg zfnd/zebra:7.0.0@${DIG} zfnd/zebra@${DIG},"
  echo "sova-keeper-1 znu7 4416000"
  echo "sova-keeper-1 paused ${T0}"
  echo "sova-keeper-1 muted $((T0 + 900))"
  echo "sova-keeper-1 health keeper paused (planned) since 2026-09-29 12:00 UTC: sova-keeper and sova-node stopped by keeper-pause"
} >"${TMP}/hosts-out"
: >"${TMP}/ssh.log"
OUT="$(shosts "" "${DIG}" 4416000)"
echo "${OUT}"
expect "rolled host: digest ok" "ok    sova-seed-1: zebrad runs ZEBRA_IMAGE_DIGEST \(zfnd/zebra:7.0.0@${DIG}\)" "${OUT}"
expect "... NU7 ok" 'ok    sova-seed-1: zebrad activates NU7 \(77190ad9\) at 4416000 = NU7_ACTIVATION_HEIGHT' "${OUT}"
expect "host not rolled yet: digest FAIL" "FAIL  sova-rpc-1: zebrad runs zfnd/zebra:6.3.0@sha256:52a6.*not ZEBRA_IMAGE_DIGEST ${DIG}: roll it \(./deploy.sh --only sova-rpc-1 --zebra-only\)" "${OUT}"
expect "... NU7 FAIL" 'FAIL  sova-rpc-1: zebrad has no NU7 upgrade \(77190ad9\), NU7_ACTIVATION_HEIGHT is 4416000: a pre-NU7 zebrad' "${OUT}"
expect "paused keeper: sova-node inactive is ok (planned)" 'ok    sova-keeper-1: sova-node is inactive: keeper paused \(planned; ./deploy.sh keeper-resume\)' "${OUT}"
expect "... sova-keeper inactive is ok (planned)" 'ok    sova-keeper-1: sova-keeper is inactive: keeper paused' "${OUT}"
expect "... pause and mute noted" "note  sova-keeper-1: keeper paused since ${T0}.*note  sova-keeper-1: Telegram alerts muted until $((T0 + 900))" "$(tr '\n' ' ' <<<"${OUT}")"
expect_not "... its P2P peer count is not judged" 'sova-keeper-1: .*P2P peer' "${OUT}"
expect_not "... no FAIL for the keeper" 'FAIL  sova-keeper-1' "${OUT}"
[[ "$(grep -c '^  FAIL' <<<"${OUT}")" == 2 ]] && pass "exactly the 2 FAILs of the host not rolled" || flunk "FAIL count $(grep -c '^  FAIL' <<<"${OUT}")"
OUT="$(shosts "" "" "")"
expect "digest not pinned, NU7 unset: image shown, not compared; no NU7 line" \
  'ok    sova-rpc-1: zebrad runs zfnd/zebra:6.3.0@sha256:52a6[0-9a-f]+ \(ZEBRA_IMAGE_DIGEST not pinned: not compared\)' "${OUT}"
expect_not "... no NU7 line" 'NU7' "${OUT}"
: >"${TMP}/ssh.log"
OUT="$(shosts sova-seed-1 "${DIG}" 4416000)"
[[ "$(cat "${TMP}/ssh.log")" == "ssh sova-seed-1" ]] && pass "--only sova-seed-1: that host alone" || flunk "--only: ssh to $(tr '\n' ' ' <"${TMP}/ssh.log")"
expect_not "... 0 FAIL" 'FAIL' "${OUT}"
sed -i.bak '/^sova-keeper-1 paused/d' "${TMP}/hosts-out"
OUT="$(shosts sova-keeper-1 "${DIG}" 4416000)"
expect "keeper NOT paused with its node stopped: FAIL" 'FAIL  sova-keeper-1: sova-node is inactive' "${OUT}"
sed -i.bak 's/^sova-rpc-1 zimg .*/sova-rpc-1 zimg /' "${TMP}/hosts-out"
OUT="$(shosts sova-rpc-1 "${DIG}" 4416000)"
expect "no zebrad container: FAIL" 'FAIL  sova-rpc-1: no zebrad container' "${OUT}"
out="$(cd "${KIT}" && SOVA_TESTNET_CONFIG="${KIT}/config.env.example" SOVA_TESTNET_OUT="${TMP}/o" ./smoke.sh hosts --only sova-nope 2>&1)"
expect "smoke.sh hosts --only <unknown>: refused" 'error: --only sova-nope: no such server in SERVERS' "${out}"

echo
echo "${PASS} passed, ${FAIL} failed"
[[ "${FAIL}" == 0 ]]
