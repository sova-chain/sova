#!/usr/bin/env bash
# infra/testnet/cloudflare.sh -- the M1 edge on Cloudflare, via its v4 API
# (curl + jq). Idempotent: each object is looked up first and updated or
# created. The API token is sent in a curl config on stdin, never on a
# command line.
#
# Usage:
#   ./cloudflare.sh [--dry-run] <step>...
#     dns        grey-cloud A/AAAA for each seed (seed-N.<suffix>): P2P
#                can't be proxied
#     tunnels    one remotely-managed tunnel per rpc/faucet host; ingress
#                rules; proxied CNAMEs rpc./faucet. -> the tunnel; the
#                tunnel token goes to the host over SSH stdin
#                (/etc/sova/cloudflared.env, root 0600) and cloudflared
#                is restarted
#     worker     the RPC firewall Worker (worker/rpc-firewall.mjs) on
#                the route <RPC_HOST>/*
#     ratelimit  the zone's one free-plan rate-limit rule
#     r2         bucket R2_BUCKET + custom domain DL_HOST
#     all        dns tunnels worker ratelimit r2
#     teardown   delete this kit's DNS records, tunnels, Worker + route and
#                rate-limit rule (the R2 bucket is kept: empty it and
#                delete it in the dashboard if you mean it)
#
# Env (from Rob, never in files): CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID,
# CLOUDFLARE_ZONE_ID. --dry-run needs none of them and calls nothing.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

usage() { sed -n '2,27p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

STEPS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    -h | --help) usage; exit 0 ;;
    all) STEPS+=(dns tunnels worker ratelimit r2) ;;
    dns | tunnels | worker | ratelimit | r2 | teardown) STEPS+=("$1") ;;
    *) die "unknown step '$1'" ;;
  esac
  shift
done
[[ ${#STEPS[@]} -gt 0 ]] || { usage; exit 1; }

load_config
validate_servers
need_cmd jq
need_cmd curl
need_env CLOUDFLARE_API_TOKEN
need_env CLOUDFLARE_ACCOUNT_ID
need_env CLOUDFLARE_ZONE_ID
API=https://api.cloudflare.com/client/v4
ACCT="${CLOUDFLARE_ACCOUNT_ID:-<account-id>}"
ZONE="${CLOUDFLARE_ZONE_ID:-<zone-id>}"
WORKER_NAME="${HC_PROJECT_LABEL}-rpc-firewall"
RL_DESCRIPTION="${HC_PROJECT_LABEL}: per-IP limit on RPC + faucet"
# Free plan: rule expressions may only use URI path fields (no host), one
# rule, 10 s period, 10 s mitigation, characteristics colo + IP. "/" is
# the JSON-RPC path, "/drip" the faucet's. On Pro+, narrow it to the hosts.
CF_RATELIMIT_EXPRESSION="${CF_RATELIMIT_EXPRESSION:-(http.request.uri.path eq \"/\") or (http.request.uri.path eq \"/drip\")}"

# cf METHOD PATH [JSON-BODY-FILE] -> response JSON on stdout; dies on
# success=false. Dry run: prints the call, returns an empty success.
cf() {
  local method="$1" path="$2" body="${3:-}"
  if [[ "${DRY_RUN}" == 1 ]]; then
    echo "+ curl -X ${method} ${API}${path}${body:+ --data @${body##*/}}" >&2
    if [[ -n "${body}" ]]; then sed 's/^/    /' "${body}" >&2 && echo >&2; fi
    echo '{"success":true,"result":null}'
    return 0
  fi
  local resp
  resp="$(printf 'header = "Authorization: Bearer %s"\n' "${CLOUDFLARE_API_TOKEN}" |
    curl -sS -K - -X "${method}" -H 'Content-Type: application/json' \
      ${body:+--data @"${body}"} "${API}${path}")" || die "Cloudflare API ${method} ${path}: transport error"
  if [[ "$(jq -r '.success' <<<"${resp}" 2>/dev/null)" != true ]]; then
    die "Cloudflare API ${method} ${path}: $(jq -c '.errors' <<<"${resp}" 2>/dev/null || echo "${resp}")"
  fi
  printf '%s' "${resp}"
}

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT
jfile() { # name json -> path
  printf '%s' "$2" >"${TMP}/$1.json"
  echo "${TMP}/$1.json"
}

# Upsert one DNS record (type, name, content, proxied).
dns_upsert() {
  local type="$1" name="$2" content="$3" proxied="$4" id body
  id="$(cf GET "/zones/${ZONE}/dns_records?type=${type}&name=${name}" | jq -r '.result[0].id // empty')"
  body="$(jfile "dns-${type}-${name}" "$(jq -nc --arg t "${type}" --arg n "${name}" --arg c "${content}" \
    --argjson p "${proxied}" --arg cm "${HC_PROJECT_LABEL} (infra/testnet)" \
    '{type:$t,name:$n,content:$c,proxied:$p,ttl:1,comment:$cm}')")"
  if [[ -n "${id}" ]]; then
    cf PUT "/zones/${ZONE}/dns_records/${id}" "${body}" >/dev/null
  else
    cf POST "/zones/${ZONE}/dns_records" "${body}" >/dev/null
  fi
  log "DNS ${type} ${name} -> ${content} (proxied ${proxied})"
}

ip_of() { # server field(ipv4|ipv6)
  local f="${OUT_DIR}/servers/$1.$2"
  if [[ -s "${f}" ]]; then cat "${f}"; elif [[ "${DRY_RUN}" == 1 ]]; then echo "<$1-$2>"; fi
}

step_dns() {
  local i=0 name ip6
  while read -r name; do
    [[ -n "${name}" ]] || continue
    i=$((i + 1))
    local host="${SEED_HOST_PREFIX}-${i}.${SEED_DOMAIN_SUFFIX}"
    dns_upsert A "${host}" "$(ip_of "${name}" ipv4)" false
    ip6="$(ip_of "${name}" ipv6)"
    # hcloud prints the /64; the host's address is ::1 in it.
    if [[ -n "${ip6}" ]]; then
      ip6="${ip6%%/*}"
      [[ "${ip6}" == *:: ]] && ip6="${ip6}1"
      dns_upsert AAAA "${host}" "${ip6}" false
    fi
    printf '%s\n' "${host}" >"${OUT_DIR}/servers/${name}.dns"
  done < <(servers_with_role seed)
}

# Tunnel for the first server of role $1, serving hostname $2 -> $3, with
# ingress path regex $4.
tunnel_for_role() {
  local role="$1" hostname="$2" service="$3" path="$4" server tname tid body token
  server="$(servers_with_role "${role}" | head -1)"
  [[ -n "${server}" ]] || { warn "no ${role} server; skipping its tunnel"; return 0; }
  tname="${HC_PROJECT_LABEL}-${server}"
  tid="$(cf GET "/accounts/${ACCT}/cfd_tunnel?name=${tname}&is_deleted=false" | jq -r '.result[0].id // empty')"
  if [[ -z "${tid}" ]]; then
    tid="$(cf POST "/accounts/${ACCT}/cfd_tunnel" "$(jfile "tunnel-${server}" \
      "$(jq -nc --arg n "${tname}" '{name:$n,config_src:"cloudflare"}')")" | jq -r '.result.id // empty')"
    [[ -n "${tid}" || "${DRY_RUN}" == 1 ]] || die "tunnel ${tname}: no id returned"
    log "tunnel ${tname} created"
  else
    log "tunnel ${tname} exists (${tid})"
  fi
  [[ -n "${tid}" ]] || tid="<${tname}-id>"
  body="$(jfile "ingress-${server}" "$(jq -nc --arg h "${hostname}" --arg s "${service}" --arg p "${path}" \
    '{config:{ingress:[{hostname:$h,path:$p,service:$s,originRequest:{}},{service:"http_status:404"}]}}')")"
  cf PUT "/accounts/${ACCT}/cfd_tunnel/${tid}/configurations" "${body}" >/dev/null
  dns_upsert CNAME "${hostname}" "${tid}.cfargotunnel.com" true
  if [[ "${DRY_RUN}" == 1 ]]; then
    echo "+ curl ${API}/accounts/${ACCT}/cfd_tunnel/${tid}/token | ssh sova-admin@${server} (TUNNEL_TOKEN -> /etc/sova/cloudflared.env, restart cloudflared)" >&2
    return 0
  fi
  token="$(cf GET "/accounts/${ACCT}/cfd_tunnel/${tid}/token" | jq -r '.result')"
  [[ -n "${token}" && "${token}" != null ]] || die "tunnel ${tname}: no token"
  printf 'TUNNEL_TOKEN=%s\n' "${token}" |
    kit_ssh "${server}" "sudo sh -c 'umask 077 && cat > /etc/sova/cloudflared.env && systemctl restart cloudflared'"
  log "${server}: cloudflared has its token and is running"
}

step_tunnels() {
  tunnel_for_role rpc "${RPC_HOST}" "http://127.0.0.1:${SOVA_HTTP_PORT}" '^/$'
  tunnel_for_role faucet "${FAUCET_HOST}" "http://127.0.0.1:${FAUCET_PORT}" '^/(drip|status)$'
}

step_worker() {
  local src="${KIT_DIR}/worker/rpc-firewall.mjs" meta
  meta="$(jq -nc '{main_module:"rpc-firewall.mjs",compatibility_date:"2026-09-01"}')"
  if [[ "${DRY_RUN}" == 1 ]]; then
    echo "+ curl -X PUT ${API}/accounts/${ACCT}/workers/scripts/${WORKER_NAME} -F metadata=${meta} -F rpc-firewall.mjs=@worker/rpc-firewall.mjs" >&2
  else
    local resp
    resp="$(printf 'header = "Authorization: Bearer %s"\n' "${CLOUDFLARE_API_TOKEN}" |
      curl -sS -K - -X PUT "${API}/accounts/${ACCT}/workers/scripts/${WORKER_NAME}" \
        -F "metadata=${meta};type=application/json" \
        -F "rpc-firewall.mjs=@${src};type=application/javascript+module")"
    [[ "$(jq -r '.success' <<<"${resp}")" == true ]] || die "worker upload: $(jq -c '.errors' <<<"${resp}")"
    log "worker ${WORKER_NAME} uploaded"
  fi
  local pattern="${RPC_HOST}/*" rid
  rid="$(cf GET "/zones/${ZONE}/workers/routes" | jq -r --arg p "${pattern}" '.result // [] | .[] | select(.pattern == $p) | .id')"
  local body
  body="$(jfile route "$(jq -nc --arg p "${pattern}" --arg s "${WORKER_NAME}" '{pattern:$p,script:$s}')")"
  if [[ -n "${rid}" ]]; then
    cf PUT "/zones/${ZONE}/workers/routes/${rid}" "${body}" >/dev/null
  else
    cf POST "/zones/${ZONE}/workers/routes" "${body}" >/dev/null
  fi
  log "route ${pattern} -> ${WORKER_NAME}"
}

step_ratelimit() {
  local path="/zones/${ZONE}/rulesets/phases/http_ratelimit/entrypoint" current others body
  if [[ "${DRY_RUN}" == 1 ]]; then
    current='{"result":{"rules":[]}}'
    echo "+ curl -X GET ${API}${path}" >&2
  else
    current="$(printf 'header = "Authorization: Bearer %s"\n' "${CLOUDFLARE_API_TOKEN}" |
      curl -sS -K - "${API}${path}")"
  fi
  # Free plan holds one rule. Refuse to replace a rule this kit didn't make.
  others="$(jq -r --arg d "${RL_DESCRIPTION}" '[.result.rules // [] | .[] | select(.description != $d)] | length' <<<"${current}" 2>/dev/null || echo 0)"
  [[ "${others}" == 0 ]] || die "the zone already has ${others} rate-limit rule(s) not made by this kit; review them in the dashboard (Security > WAF > Rate limiting rules) first"
  body="$(jfile ratelimit "$(jq -nc --arg d "${RL_DESCRIPTION}" --arg e "${CF_RATELIMIT_EXPRESSION}" \
    --argjson n "${CF_RATELIMIT_REQUESTS_PER_10S}" \
    '{rules:[{description:$d,expression:$e,action:"block",ratelimit:{characteristics:["cf.colo.id","ip.src"],period:10,requests_per_period:$n,mitigation_timeout:10}}]}')")"
  cf PUT "${path}" "${body}" >/dev/null
  log "rate limit: ${CF_RATELIMIT_REQUESTS_PER_10S} req / 10 s per IP on: ${CF_RATELIMIT_EXPRESSION}"
}

step_r2() {
  if [[ "${DRY_RUN}" == 1 ]] || ! cf GET "/accounts/${ACCT}/r2/buckets/${R2_BUCKET}" >/dev/null 2>&1; then
    cf POST "/accounts/${ACCT}/r2/buckets" "$(jfile r2 "$(jq -nc --arg n "${R2_BUCKET}" '{name:$n}')")" >/dev/null
    log "R2 bucket ${R2_BUCKET} created"
  else
    log "R2 bucket ${R2_BUCKET} exists"
  fi
  local have
  have="$(cf GET "/accounts/${ACCT}/r2/buckets/${R2_BUCKET}/domains/custom" |
    jq -r --arg d "${DL_HOST}" '[.result.domains // [] | .[] | select(.domain == $d)] | length')"
  if [[ "${have}" == 0 ]]; then
    cf POST "/accounts/${ACCT}/r2/buckets/${R2_BUCKET}/domains/custom" "$(jfile r2dom \
      "$(jq -nc --arg d "${DL_HOST}" --arg z "${ZONE}" '{domain:$d,zoneId:$z,enabled:true}')")" >/dev/null
    log "R2 custom domain ${DL_HOST} attached (Cloudflare creates its DNS record)"
  else
    log "R2 custom domain ${DL_HOST} already attached"
  fi
}

step_teardown() {
  local name host id tname
  if [[ "${DRY_RUN}" != 1 ]]; then
    printf 'Delete the Cloudflare objects of %s? Type the label to continue: ' "${HC_PROJECT_LABEL}" >&2
    local answer
    read -r answer
    [[ "${answer}" == "${HC_PROJECT_LABEL}" ]] || die "not confirmed; nothing deleted"
  fi
  for host in "${RPC_HOST}" "${FAUCET_HOST}" $(cat "${OUT_DIR}"/servers/*.dns 2>/dev/null); do
    for id in $(cf GET "/zones/${ZONE}/dns_records?name=${host}" | jq -r '.result // [] | .[].id'); do
      cf DELETE "/zones/${ZONE}/dns_records/${id}" >/dev/null && log "DNS ${host} deleted"
    done
  done
  for name in $(servers_with_role rpc) $(servers_with_role faucet); do
    tname="${HC_PROJECT_LABEL}-${name}"
    for id in $(cf GET "/accounts/${ACCT}/cfd_tunnel?name=${tname}&is_deleted=false" | jq -r '.result // [] | .[].id'); do
      cf DELETE "/accounts/${ACCT}/cfd_tunnel/${id}" >/dev/null && log "tunnel ${tname} deleted"
    done
  done
  for id in $(cf GET "/zones/${ZONE}/workers/routes" | jq -r --arg p "${RPC_HOST}/*" '.result // [] | .[] | select(.pattern == $p) | .id'); do
    cf DELETE "/zones/${ZONE}/workers/routes/${id}" >/dev/null && log "route deleted"
  done
  if cf DELETE "/accounts/${ACCT}/workers/scripts/${WORKER_NAME}" >/dev/null; then log "worker deleted"; fi
  local body
  body="$(jfile rl-empty '{"rules":[]}')"
  cf PUT "/zones/${ZONE}/rulesets/phases/http_ratelimit/entrypoint" "${body}" >/dev/null && log "rate-limit rule removed"
  log "R2 bucket ${R2_BUCKET} kept (delete it in the dashboard once empty, if intended)"
}

[[ "${DRY_RUN}" == 1 ]] && log "DRY RUN: no API calls, no SSH"
for step in "${STEPS[@]}"; do
  log "step: ${step}"
  "step_${step}"
done
