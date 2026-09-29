#!/usr/bin/env bash
# infra/testnet/host/zebra-ready.sh -- is this host's zebrad ready again
# after a restart? setup-host.sh runs it around every zebrad restart (full
# deploy and --zebra-only), so a rollout stops on a zebrad that doesn't come
# back (docs/ops/nu7-upgrade.md, K2). Also usable by hand on a host:
# `sudo /usr/local/lib/sova-infra/zebra-ready.sh wait`.
#
#   zebra-ready.sh before
#       One line "<tip> <lag>": zebrad's tip now, and how far it is behind
#       the highest ZCASH_REFERENCE_URLS JSON-RPC tip (0 when none answers).
#       "- 0" when zebrad doesn't answer. Taken just before the restart.
#   zebra-ready.sh wait [--since EPOCH] [--started EPOCH] [--pre-tip N] [--pre-lag N]
#       Polls until zebrad is ready, or fails (exit 1) with what is missing.
#       --since: when the restart began (the timeout and every timing count
#       from it; default now); --started: when `systemctl restart` returned.
#       Ready means all of:
#         rpc     getblockchaininfo answers;
#         image   the running container is ZEBRA_IMAGE_DIGEST (when pinned;
#                 a mismatch fails at once);
#         nu7     NU7_ACTIVATION_HEIGHT set: upgrades["77190ad9"] activates
#                 at it (a mismatch or a missing NU7 fails at once);
#         format  a state format upgrade zebrad launched on this start
#                 ("trying to open older database format: launching upgrade
#                 task") has finished ("database format is valid"); a panic
#                 or "unexpected invalid database format" fails at once;
#         tip     tip >= pre-tip - ZEBRA_READY_LAG, and, while a reference
#                 answers, tip >= reference tip - pre-lag - ZEBRA_READY_LAG
#                 (no further behind the network than before the restart).
#                 No pre-tip (zebrad wasn't answering before: a new or
#                 stopped zebrad, which may sync for hours): not gated.
#       Prints the timings, and a "KIT-OUT zebra_ready ..." line (seconds
#       from --since) that deploy.sh records as out/servers/<host>.zebra_ready.
#
# Settings: /etc/sova/host.env, or the file in $ZEBRA_READY_ENV (setup-host.sh
# passes its host.env): ZEBRA_RPC_PORT, ZEBRA_IMAGE_DIGEST,
# NU7_ACTIVATION_HEIGHT, ZCASH_REFERENCE_URLS, ZEBRA_READY_TIMEOUT_MIN (15;
# 0 = don't wait), ZEBRA_READY_LAG (3 blocks). ZEBRA_URL and
# ZEBRA_READY_POLL_SECS (10) are for tests.
set -uo pipefail

ENVF="${ZEBRA_READY_ENV:-/etc/sova/host.env}"
# shellcheck source=/dev/null
[[ -f "${ENVF}" ]] && source "${ENVF}"
ZEBRA_URL="${ZEBRA_URL:-http://127.0.0.1:${ZEBRA_RPC_PORT:-18232}}"
ZEBRA_IMAGE_DIGEST="${ZEBRA_IMAGE_DIGEST:-}"
NU7_ACTIVATION_HEIGHT="${NU7_ACTIVATION_HEIGHT:-}"
[[ "${NU7_ACTIVATION_HEIGHT}" =~ ^[1-9][0-9]*$ ]] || NU7_ACTIVATION_HEIGHT=""
NU7_BRANCH_ID=77190ad9
ZCASH_REFERENCE_URLS="${ZCASH_REFERENCE_URLS:-}"
ZEBRA_READY_TIMEOUT_MIN="${ZEBRA_READY_TIMEOUT_MIN:-15}"
[[ "${ZEBRA_READY_TIMEOUT_MIN}" =~ ^[0-9]+$ ]] || ZEBRA_READY_TIMEOUT_MIN=15
ZEBRA_READY_LAG="${ZEBRA_READY_LAG:-3}"
[[ "${ZEBRA_READY_LAG}" =~ ^[0-9]+$ ]] || ZEBRA_READY_LAG=3
POLL="${ZEBRA_READY_POLL_SECS:-10}"
# Ask the references at most this often (they are free public services).
REF_EVERY=60
# How often a progress line is printed while waiting.
SAY_EVERY=30
# How long after the RPC answers the journal gets to show how zebrad opened
# its state (it logs that before the RPC starts).
FORMAT_LOG_GRACE=30

say() { printf '[zebra-ready] %s\n' "$*"; }
fail() {
  printf '[zebra-ready] NOT READY: %s\n' "$*" >&2
  exit 1
}
now() { date +%s; }

rpc() { # url method [params-json]
  curl -fsS --max-time 5 -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":${3:-[]}}" "$1" 2>/dev/null
}

# The highest tip among the JSON-RPC references (explorer URLs, with
# {height}, can't give a tip and are skipped). Empty when none answers.
ref_tip() {
  local url urls best="" t
  [[ -n "${ZCASH_REFERENCE_URLS}" ]] || return 0
  IFS=, read -ra urls <<<"${ZCASH_REFERENCE_URLS}"
  for url in ${urls[@]+"${urls[@]}"}; do
    [[ -n "${url}" && "${url}" != *'{height}'* ]] || continue
    t="$(rpc "${url}" getblockcount | jq -r '.result // empty' 2>/dev/null)"
    [[ "${t}" =~ ^[0-9]+$ ]] || continue
    [[ -z "${best}" ]] || ((t > best)) && best="${t}"
  done
  printf '%s' "${best}"
}

zebra_tip() {
  local t
  t="$(rpc "${ZEBRA_URL}" getblockcount | jq -r '.result // empty' 2>/dev/null)"
  [[ "${t}" =~ ^[0-9]+$ ]] && printf '%s' "${t}"
}

cmd_before() {
  local tip ref lag=0
  tip="$(zebra_tip)"
  if [[ -z "${tip}" ]]; then
    echo "- 0"
    return 0
  fi
  ref="$(ref_tip)"
  [[ -n "${ref}" ]] && ((ref > tip)) && lag=$((ref - tip))
  echo "${tip} ${lag}"
}

# zebrad's log lines since <epoch>, from the unit's journal (the attached
# `docker run`) and the container's own journald entries.
zlog() { # since-epoch
  {
    journalctl -u zebrad --since "@$1" --no-pager -q -o cat 2>/dev/null
    journalctl CONTAINER_NAME=zebrad --since "@$1" --no-pager -q -o cat 2>/dev/null
  } | tr -d '\r'
}

# The running container's image, as the unit asked for it (ZEBRA_IMAGE_REF,
# image@digest) and as Docker recorded it (RepoDigests). "" if none runs.
running_image() {
  local cfg id digests
  cfg="$(docker inspect zebrad --format '{{.Config.Image}}' 2>/dev/null)" || return 0
  id="$(docker inspect zebrad --format '{{.Image}}' 2>/dev/null)"
  digests="$(docker image inspect "${id}" --format '{{range .RepoDigests}}{{.}} {{end}}' 2>/dev/null)"
  printf '%s %s' "${cfg}" "${digests}"
}

cmd_wait() {
  local since="" started="" pre_tip="" pre_lag=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --since) since="$2"; shift ;;
      --started) started="$2"; shift ;;
      --pre-tip) pre_tip="$2"; shift ;;
      --pre-lag) pre_lag="$2"; shift ;;
      *) fail "unknown argument '$1'" ;;
    esac
    shift
  done
  [[ "${since}" =~ ^[0-9]+$ ]] || since="$(now)"
  [[ "${started}" =~ ^[0-9]+$ ]] || started="${since}"
  [[ "${pre_tip}" =~ ^[0-9]+$ ]] || pre_tip=""
  [[ "${pre_lag}" =~ ^[0-9]+$ ]] || pre_lag=0
  if [[ "${ZEBRA_READY_TIMEOUT_MIN}" == 0 ]]; then
    say "ZEBRA_READY_TIMEOUT_MIN=0: not waiting for zebrad"
    return 0
  fi
  local deadline=$((since + ZEBRA_READY_TIMEOUT_MIN * 60)) t
  local t_rpc="" t_format="" t_tip="" image_ok="" nu7_ok="" upgrade="none"
  local info tip="" est need ref="" ref_at=0 last_say=0 lines state img
  say "waiting for zebrad at ${ZEBRA_URL} (timeout ${ZEBRA_READY_TIMEOUT_MIN} min from the restart; stop+start took $((started - since)) s)"
  if [[ -z "${pre_tip}" ]]; then
    say "zebrad was not answering before the restart: catch-up not gated (a new or stopped zebrad may sync for hours; health.sh's zebrad_lag watches it)"
  else
    say "before the restart: tip ${pre_tip}, ${pre_lag} behind the references; ready at tip >= ${pre_tip} - ${ZEBRA_READY_LAG}, and within ${pre_lag} + ${ZEBRA_READY_LAG} of a reference that answers"
  fi
  [[ -n "${ZEBRA_IMAGE_DIGEST}" ]] || image_ok="not pinned"
  [[ -n "${NU7_ACTIVATION_HEIGHT}" ]] || nu7_ok="not set"
  while :; do
    t="$(now)"
    info="$(rpc "${ZEBRA_URL}" getblockchaininfo)" || info=""
    tip="$(jq -r '.result.blocks // empty' <<<"${info}" 2>/dev/null)"
    [[ "${tip}" =~ ^[0-9]+$ ]] || tip=""
    # From the new zebrad only: the old one's shutdown lines come before
    # `systemctl restart` returned.
    lines="$(zlog "${started}")"
    # Hard failures first: waiting won't fix them.
    state="$(grep -m1 -E 'unexpected invalid database format|panicked' <<<"${lines}")"
    [[ -z "${state}" ]] || fail "zebrad panicked after the restart: ${state:0:300} (journalctl -u zebrad; runbook 1.7 rollback)"
    if [[ -n "${tip}" ]]; then
      [[ -n "${t_rpc}" ]] || { t_rpc="${t}"; say "RPC answers after $((t - since)) s (tip ${tip})"; }
      if [[ -z "${image_ok}" ]]; then
        img="$(running_image)"
        if [[ " ${img} " == *"@${ZEBRA_IMAGE_DIGEST} "* ]]; then
          image_ok=yes
          say "image: the running container is ${ZEBRA_IMAGE_DIGEST}"
        elif [[ -n "${img// /}" ]]; then
          fail "the running zebrad is '${img}', not ZEBRA_IMAGE_DIGEST ${ZEBRA_IMAGE_DIGEST} (check /etc/sova/zebrad.env and docker ps)"
        fi
      fi
      if [[ -z "${nu7_ok}" ]]; then
        local at
        at="$(jq -r --arg b "${NU7_BRANCH_ID}" '.result.upgrades[$b].activationheight // empty' <<<"${info}" 2>/dev/null)"
        if [[ "${at}" == "${NU7_ACTIVATION_HEIGHT}" ]]; then
          nu7_ok=yes
          say "nu7: zebrad activates NU7 (${NU7_BRANCH_ID}) at ${at} = NU7_ACTIVATION_HEIGHT"
        elif [[ -z "${at}" ]]; then
          fail "zebrad has no NU7 upgrade (${NU7_BRANCH_ID}) but NU7_ACTIVATION_HEIGHT is ${NU7_ACTIVATION_HEIGHT}: a pre-NU7 image (runbook 2.2c, 1.7)"
        else
          fail "zebrad activates NU7 at ${at}, but NU7_ACTIVATION_HEIGHT is ${NU7_ACTIVATION_HEIGHT}: a zebrad for another NU7 height, or a wrong config.env (runbook 2.2c)"
        fi
      fi
    fi
    # State format: only an upgrade launched on this start is waited for.
    if [[ -z "${t_format}" ]]; then
      if grep -q 'launching upgrade task' <<<"${lines}"; then
        upgrade="$(grep -m1 'launching upgrade task' <<<"${lines}" | grep -oE '(running_version|disk_version)=[^ ]+' | tr '\n' ' ')"
        upgrade="${upgrade% }"
        [[ -n "${upgrade}" ]] || upgrade="launched"
        if grep -q 'database format is valid' <<<"${lines}"; then
          t_format="${t}"
          say "state format upgrade (${upgrade}) finished $((t - since)) s after the restart"
        fi
      elif [[ -n "${tip}" ]]; then
        # zebrad logs how it opened the state before its RPC starts; give
        # the journal FORMAT_LOG_GRACE s to show it, then go without.
        state="$(grep -m1 -oE 'trying to open (current|newer) database format|creating new database with the current format' <<<"${lines}")"
        if [[ -n "${state}" ]] || ((t - t_rpc >= FORMAT_LOG_GRACE)); then
          t_format="${t}"
          say "state format: no upgrade on this start (${state:-no format line in the zebrad journal after ${FORMAT_LOG_GRACE} s})"
        fi
      fi
    fi
    # Catch-up.
    if [[ -n "${tip}" && -z "${t_tip}" ]]; then
      if [[ -z "${pre_tip}" ]]; then
        t_tip="${t}"
      else
        if ((t - ref_at >= REF_EVERY)); then
          ref="$(ref_tip)"
          ref_at="${t}"
        fi
        need=$((pre_tip - ZEBRA_READY_LAG))
        [[ -n "${ref}" ]] && ((ref - pre_lag - ZEBRA_READY_LAG > need)) && need=$((ref - pre_lag - ZEBRA_READY_LAG))
        ((tip >= need)) && t_tip="${t}"
      fi
    fi
    if [[ -n "${t_rpc}" && -n "${t_format}" && -n "${t_tip}" && -n "${image_ok}" && -n "${nu7_ok}" ]]; then
      est="$(jq -r '.result.estimatedheight // "?"' <<<"${info}" 2>/dev/null)"
      say "READY $((t - since)) s after the restart began: stop+start $((started - since)) s, RPC at $((t_rpc - since)) s, format done at $((t_format - since)) s (upgrade: ${upgrade}), caught up at $((t_tip - since)) s; tip ${tip} (estimatedheight ${est}${ref:+, reference ${ref}}${pre_tip:+, before the restart ${pre_tip}}); image ${image_ok}; NU7 ${nu7_ok}"
      printf 'KIT-OUT zebra_ready total=%ss restart=%ss rpc=%ss format=%ss tip=%ss upgrade=%s\n' \
        "$((t - since))" "$((started - since))" "$((t_rpc - since))" "$((t_format - since))" "$((t_tip - since))" \
        "$([[ "${upgrade}" == none ]] && echo none || echo yes)" >&2
      return 0
    fi
    if ((t >= deadline)); then
      local why=""
      [[ -n "${t_rpc}" ]] || why="zebrad RPC not answering at ${ZEBRA_URL}"
      [[ -n "${t_rpc}" && -z "${image_ok}" ]] && why="${why:+${why}; }no zebrad container to check the image of"
      [[ -n "${t_format}" || "${upgrade}" == none ]] || why="${why:+${why}; }state format upgrade (${upgrade}) still running: $(grep -E 'upgrad|format' <<<"${lines}" | tail -1 | cut -c1-200)"
      [[ -n "${t_rpc}" && -z "${t_tip}" ]] && why="${why:+${why}; }tip ${tip:-?}, need >= ${need:-?} (before the restart ${pre_tip}${ref:+, reference ${ref}})"
      fail "after ${ZEBRA_READY_TIMEOUT_MIN} min: ${why}. Stop the rollout: journalctl -u zebrad, runbook 1.7"
    fi
    if ((t - last_say >= SAY_EVERY)); then
      last_say="${t}"
      say "+$((t - since)) s: rpc $([[ -n "${t_rpc}" ]] && echo up || echo down), tip ${tip:--}${need:+ (need ${need})}, format $([[ -n "${t_format}" ]] && echo finished || echo "${upgrade}")"
    fi
    sleep "${POLL}"
  done
}

case "${1:-}" in
  before) cmd_before ;;
  wait) shift; cmd_wait "$@" ;;
  *) fail "usage: zebra-ready.sh before | wait [--since EPOCH] [--started EPOCH] [--pre-tip N] [--pre-lag N]" ;;
esac
