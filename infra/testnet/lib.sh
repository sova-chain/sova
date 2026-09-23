#!/usr/bin/env bash
# infra/testnet/lib.sh -- shared helpers for the M1 launch kit. Sourced, not
# run. Nothing here reads a credential file; tokens come from the
# environment only, and are never echoed.

KIT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC2034 # used by publish.sh and deploy-contracts.sh
REPO_ROOT="$(cd "${KIT_DIR}/../.." && pwd)"
# Generated, non-secret outputs (IPs, enodes, seeds.json). Git-ignored.
OUT_DIR="${SOVA_TESTNET_OUT:-${KIT_DIR}/out}"

DRY_RUN=0

log() { printf '==> %s\n' "$*" >&2; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

# Loads config.env (or $SOVA_TESTNET_CONFIG). It is plain bash, so it may
# hold the SERVERS array.
load_config() {
  local cfg="${SOVA_TESTNET_CONFIG:-${KIT_DIR}/config.env}"
  [[ -f "${cfg}" ]] || die "no ${cfg}; copy config.env.example to config.env and edit it"
  # shellcheck source=/dev/null
  source "${cfg}"
  [[ ${#SERVERS[@]} -gt 0 ]] || die "config: SERVERS is empty"
  [[ "${DRY_RUN}" == 1 ]] || load_secrets
}

# Tokens: already-exported env vars win; otherwise, if Rob keeps them in
# $SOVA_TESTNET_SECRETS (default ~/.config/sova-testnet/secrets.env, see
# secrets.env.example), that file is sourced -- only if it is private
# (mode 600/400), never printed, never copied. Dry runs never read it.
load_secrets() {
  local f="${SOVA_TESTNET_SECRETS:-${HOME}/.config/sova-testnet/secrets.env}" mode
  [[ -f "${f}" ]] || return 0
  mode="$(stat -f %Lp "${f}" 2>/dev/null || stat -c %a "${f}")"
  [[ "${mode}" == 600 || "${mode}" == 400 ]] || die "${f} has mode ${mode}; chmod 600 it"
  local had_token_vars
  had_token_vars="$(env | grep -cE '^(HCLOUD_TOKEN|CLOUDFLARE_API_TOKEN)=' || true)"
  set -a
  # shellcheck source=/dev/null
  source "${f}"
  set +a
  [[ "${had_token_vars}" == 0 ]] || warn "tokens were already in the environment; ${f} was sourced on top"
}

# Prints a command, then runs it unless --dry-run. Arguments are shown
# shell-quoted; nothing secret is ever passed as an argument (tokens travel
# in the environment or on stdin).
run() {
  printf '+'
  printf ' %q' "$@"
  printf '\n'
  [[ "${DRY_RUN}" == 1 ]] && return 0
  "$@"
}

# Requires an environment variable. In a dry run a missing one is only a
# warning, so the plan can be printed before Rob has created anything.
need_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    if [[ "${DRY_RUN}" == 1 ]]; then
      warn "${name} is not set (dry run continues)"
      return 0
    fi
    die "${name} is not set (see docs/ops/testnet-launch.md, Rob's steps)"
  fi
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not installed${2:+ ($2)}"
}

# SERVERS entries are name:type:location:volume_gb:role (a Hetzner server
# provision.sh creates), or, for a host the kit does not create ("bring
# your own", e.g. the AWS keeper):
#   name:byo:<ipv4-or-hostname>:<disk_gb>:role[:<first-login-user>]
# A byo host is adopted over SSH (provision.sh up) and from then on is
# configured exactly like a Hetzner one. Its firewall is the operator's
# job (docs/ops/keeper-aws.md). An address still written as <...> is
# pending: dry runs use a placeholder, real runs refuse it.
srv_name() { cut -d: -f1 <<<"$1"; }
srv_type() { cut -d: -f2 <<<"$1"; }
srv_location() { cut -d: -f3 <<<"$1"; }
srv_volume_gb() { cut -d: -f4 <<<"$1"; }
srv_role() { cut -d: -f5 <<<"$1"; }
srv_is_byo() { [[ "$(srv_type "$1")" == byo ]]; }
srv_address() { srv_location "$1"; } # byo only
srv_first_login_user() { cut -s -d: -f6 <<<"$1"; }
srv_address_pending() { [[ "$(srv_address "$1")" == \<*\> ]]; }

# The SERVERS entry named $1.
server_entry() {
  local s
  for s in "${SERVERS[@]}"; do
    [[ "$(srv_name "${s}")" == "$1" ]] && { printf '%s' "${s}"; return 0; }
  done
  return 1
}

# Does any entry need Hetzner (i.e. is not byo)?
has_hetzner_servers() {
  local s
  for s in "${SERVERS[@]}"; do srv_is_byo "${s}" || return 0; done
  return 1
}

# Names of the servers with role $1, one per line.
servers_with_role() {
  local s
  for s in "${SERVERS[@]}"; do
    [[ "$(srv_role "${s}")" == "$1" ]] && srv_name "${s}"
  done
  return 0
}

validate_servers() {
  local s role name fields addr seen=" "
  for s in "${SERVERS[@]}"; do
    name="$(srv_name "${s}")"
    role="$(srv_role "${s}")"
    [[ "${name}" =~ ^[a-z0-9-]+$ ]] || die "config: bad server name '${name}'"
    [[ "${seen}" != *" ${name} "* ]] || die "config: duplicate server '${name}'"
    seen+="${name} "
    fields="$(awk -F: '{ print NF }' <<<"${s}")"
    if srv_is_byo "${s}"; then
      [[ "${fields}" == 5 || "${fields}" == 6 ]] ||
        die "config: ${name}: a byo entry is name:byo:<ipv4-or-hostname>:<disk_gb>:role[:<first-login-user>] (an IPv6 literal can't be used: give an IPv4 or a hostname)"
      addr="$(srv_address "${s}")"
      if ! srv_address_pending "${s}"; then
        [[ "${addr}" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ || "${addr}" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]] ||
          die "config: ${name}: '${addr}' is not an IPv4 address or a hostname"
      fi
      [[ -z "$(srv_first_login_user "${s}")" || "$(srv_first_login_user "${s}")" =~ ^[a-z_][a-z0-9_-]*$ ]] ||
        die "config: ${name}: bad first-login user '$(srv_first_login_user "${s}")'"
    else
      [[ "${fields}" == 5 ]] || die "config: ${name}: a Hetzner entry is name:type:location:volume_gb:role (5 fields)"
    fi
    case "${role}" in seed | rpc | faucet | keeper) ;; *) die "config: ${name}: unknown role '${role}'" ;; esac
    [[ "$(srv_volume_gb "${s}")" =~ ^[0-9]+$ ]] || die "config: ${name}: volume_gb must be a number (0 = none)"
  done
  [[ -n "$(servers_with_role seed)" ]] || die "config: at least one seed server is required"
}

# Everything in config.env that can be checked without an account or a
# network call. Hard errors die; things that are fine to leave open until
# a later launch step are counted in CONFIG_WARNINGS and printed.
CONFIG_WARNINGS=0
cfg_warn() {
  CONFIG_WARNINGS=$((CONFIG_WARNINGS + 1))
  warn "config: $*"
}
# The edge's rate limits (config.env.example, "Edge"). Defaults keep an
# older config.env working. RPC_RATELIMIT_AT says where the public RPC's
# per-IP limit lives:
#   worker (default)  the RPC Worker's RPC_RATELIMIT binding (Workers Rate
#                     Limiting). Its 429 is a JSON-RPC error with CORS and
#                     Retry-After, so browser pages can back off; OPTIONS
#                     preflights don't count. The zone's one WAF rule then
#                     covers only the faucet's /drip.
#   waf               the zone's WAF rule covers "/" as well (the old
#                     setup; its 429 has no CORS headers). Only for an
#                     account where the binding can't be used.
validate_edge_config() {
  RPC_RATELIMIT_AT="${RPC_RATELIMIT_AT:-worker}"
  RPC_RATELIMIT_REQUESTS="${RPC_RATELIMIT_REQUESTS:-50}"
  RPC_RATELIMIT_PERIOD="${RPC_RATELIMIT_PERIOD:-10}"
  RPC_RATELIMIT_NAMESPACE_ID="${RPC_RATELIMIT_NAMESPACE_ID:-82330}"
  CF_RATELIMIT_REQUESTS_PER_10S="${CF_RATELIMIT_REQUESTS_PER_10S:-50}"
  case "${RPC_RATELIMIT_AT}" in worker | waf) ;; *) die "config: RPC_RATELIMIT_AT must be worker or waf" ;; esac
  [[ "${RPC_RATELIMIT_REQUESTS}" =~ ^[1-9][0-9]*$ ]] || die "config: RPC_RATELIMIT_REQUESTS must be a positive integer"
  [[ "${RPC_RATELIMIT_PERIOD}" == 10 || "${RPC_RATELIMIT_PERIOD}" == 60 ]] ||
    die "config: RPC_RATELIMIT_PERIOD must be 10 or 60 (the binding's only periods)"
  [[ "${RPC_RATELIMIT_NAMESPACE_ID}" =~ ^[1-9][0-9]*$ ]] || die "config: RPC_RATELIMIT_NAMESPACE_ID must be a positive integer"
  [[ "${CF_RATELIMIT_REQUESTS_PER_10S}" =~ ^[1-9][0-9]*$ ]] || die "config: CF_RATELIMIT_REQUESTS_PER_10S must be a positive integer"
}

validate_config() {
  validate_servers
  validate_edge_config
  local v h p ports=" " n s
  [[ "${SOVA_RELEASE_TAG:-}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] ||
    die "config: SOVA_RELEASE_TAG '${SOVA_RELEASE_TAG:-}' is not a vX.Y.Z tag"
  [[ "${SOVA_RELEASE_REPO:-}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "config: SOVA_RELEASE_REPO must be owner/name"
  if [[ -z "${ZEBRA_IMAGE_DIGEST:-}" ]]; then
    cfg_warn "ZEBRA_IMAGE_DIGEST is empty (pin it on launch day)"
  else
    [[ "${ZEBRA_IMAGE_DIGEST}" =~ ^sha256:[0-9a-f]{64}$ ]] || die "config: ZEBRA_IMAGE_DIGEST must be sha256:<64 hex>"
  fi
  if [[ -z "${SOVA_EPOCH_BASE:-}" ]]; then
    cfg_warn "SOVA_EPOCH_BASE is not pinned yet (expected until 'Pin B'; sova-node will not start)"
  else
    [[ "${SOVA_EPOCH_BASE}" =~ ^[1-9][0-9]*$ ]] || die "config: SOVA_EPOCH_BASE must be a positive integer"
  fi
  case "${SOVA_EMISSION_SCHEDULE:-}" in sip3 | flat) ;; *) die "config: SOVA_EMISSION_SCHEDULE must be sip3 or flat" ;; esac
  [[ "${CF_ZONE_NAME:-}" =~ ^[a-z0-9.-]+\.[a-z]+$ ]] || die "config: CF_ZONE_NAME '${CF_ZONE_NAME:-}' is not a domain"
  for h in "${RPC_HOST:-}" "${FAUCET_HOST:-}" "${DL_HOST:-}" "${SEED_DOMAIN_SUFFIX:-}"; do
    [[ "${h}" == *".${CF_ZONE_NAME}" ]] || die "config: '${h}' is not under the zone ${CF_ZONE_NAME}"
  done
  for v in SOVA_P2P_PORT ZEBRA_P2P_PORT ZEBRA_RPC_PORT SOVA_HTTP_PORT SOVA_AUTH_PORT FAUCET_PORT; do
    p="${!v:-}"
    if ! [[ "${p}" =~ ^[0-9]+$ ]] || ((p < 1 || p > 65535)); then die "config: ${v} '${p}' is not a port"; fi
    [[ "${ports}" != *" ${p} "* ]] || die "config: ${v} ${p} is used twice"
    ports+="${p} "
  done
  if [[ -z "${ADMIN_CIDRS:-}" ]]; then
    cfg_warn "ADMIN_CIDRS is empty (provision.sh up --my-ip fills it)"
  elif [[ ",${ADMIN_CIDRS}," == *",0.0.0.0/0,"* && "${SOVA_ALLOW_SSH_FROM_ANYWHERE:-0}" != 1 ]]; then
    die "config: ADMIN_CIDRS includes 0.0.0.0/0"
  fi
  [[ -f "${SSH_PUBLIC_KEY_FILE:-}" ]] || cfg_warn "no SSH public key at ${SSH_PUBLIC_KEY_FILE:-<unset>} (Rob's step: SSH key)"
  n="$(servers_with_role rpc | wc -l | tr -d ' ')"
  [[ "${n}" == 1 ]] || die "config: exactly one rpc server expected (RPC_HOST's tunnel), found ${n}"
  n="$(servers_with_role faucet | wc -l | tr -d ' ')"
  [[ "${n}" -le 1 ]] || die "config: at most one faucet server (one hot key), found ${n}"
  [[ "${n}" == 1 ]] || cfg_warn "no faucet server: strangers get no TAZ from us"
  [[ -n "$(servers_with_role keeper)" ]] ||
    cfg_warn "no keeper server: some mine-mode node outside this kit must run or the chain does not advance"
  for s in "${SERVERS[@]}"; do
    srv_is_byo "${s}" || continue
    if srv_address_pending "${s}"; then
      cfg_warn "$(srv_name "${s}"): address $(srv_address "${s}") is pending (bring-your-own host; for the keeper: docs/ops/keeper-aws.md, hand back the Elastic IP)"
    fi
  done
  [[ "${SOVA_CHAIN_ID:-82330}" == 82330 ]] || cfg_warn "SOVA_CHAIN_ID is ${SOVA_CHAIN_ID}, not the testnet's 82330"
  # Day-one contract values: public, and only needed at the contracts step.
  if [[ -n "${ASHWINGS_TREASURY:-}" ]]; then
    [[ "${ASHWINGS_TREASURY}" =~ ^0x[0-9a-fA-F]{40}$ ]] || die "config: ASHWINGS_TREASURY is not an address"
  else
    cfg_warn "ASHWINGS_TREASURY is empty (Rob's address; docs/ops/wallets.md)"
  fi
  if [[ -n "${ASHWINGS_ZEC_PAYEE:-}" ]]; then
    [[ "${ASHWINGS_ZEC_PAYEE}" =~ ^t[m2][1-9A-HJ-NP-Za-km-z]{33}$ ]] ||
      die "config: ASHWINGS_ZEC_PAYEE must be a Zcash TESTNET transparent address (tm... or t2..., 35 chars)"
  else
    cfg_warn "ASHWINGS_ZEC_PAYEE is empty (Rob's Zcash testnet t-address, if the contracts take one)"
  fi
  for v in ASHWINGS_PRICE_WEI ASHWINGS_PRICE_ZAT; do
    if [[ -n "${!v:-}" ]]; then
      [[ "${!v}" =~ ^[0-9]+$ ]] || die "config: ${v} must be an integer"
    else
      cfg_warn "${v} is empty (Rob's decision, if the contracts take it)"
    fi
  done
  if ! [[ "${MARKET_FEE_BPS:-100}" =~ ^[0-9]+$ ]] || ((${MARKET_FEE_BPS:-100} > 1000)); then
    die "config: MARKET_FEE_BPS must be 0..1000 (AshwingsMarket caps it at 10%)"
  fi
}

# IPv4 of a hostname (or the address itself if it already is one).
resolve_ipv4() {
  local a="$1" ip=""
  if [[ "${a}" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]]; then
    printf '%s\n' "${a}"
    return 0
  fi
  if command -v dig >/dev/null 2>&1; then
    ip="$(dig +short A "${a}" 2>/dev/null | grep -E '^[0-9]+(\.[0-9]+){3}$' | head -1 || true)"
  fi
  if [[ -z "${ip}" ]] && command -v python3 >/dev/null 2>&1; then
    ip="$(python3 -c 'import socket, sys; print(socket.gethostbyname(sys.argv[1]))' "${a}" 2>/dev/null || true)"
  fi
  [[ -n "${ip}" ]] || return 1
  printf '%s\n' "${ip}"
}

# Public IPv4 of a server: a byo host's comes from config.env (the source
# of truth: a new Elastic IP is one edit), a Hetzner server's from the
# record provision.sh wrote.
server_ip() {
  local s
  if s="$(server_entry "$1")" && srv_is_byo "${s}"; then
    srv_address_pending "${s}" && die "$1: address $(srv_address "${s}") is still pending in config.env"
    resolve_ipv4 "$(srv_address "${s}")" || die "$1: cannot resolve $(srv_address "${s}")"
    return 0
  fi
  local f="${OUT_DIR}/servers/$1.ipv4"
  [[ -s "${f}" ]] || die "no recorded IP for $1 (run provision.sh first)"
  cat "${f}"
}

# Local scripts must run on macOS's bash 3.2: no mapfile, no associative
# arrays, no namerefs.
SSH_OPTS=()
set_ssh_opts() {
  SSH_OPTS=(-o BatchMode=yes -o StrictHostKeyChecking=accept-new
    -o UserKnownHostsFile="${OUT_DIR}/known_hosts" -o ConnectTimeout=15
    -i "${SSH_PRIVATE_KEY_FILE}")
}

# ssh as the admin user (created by cloud-init, or on a byo host by
# provision.sh's adoption) to server $1.
kit_ssh() {
  local name="$1" ip
  shift
  ip="$(server_ip "${name}")" || return 1
  set_ssh_opts
  # shellcheck disable=SC2029 # remote commands are composed on purpose
  ssh "${SSH_OPTS[@]}" "sova-admin@${ip}" "$@"
}

# scp local files ($2...) into directory $1 on server... (last arg style
# kept simple: kit_scp <server> <remote-dir> <local-path>...).
kit_scp() {
  local name="$1" dir="$2" ip
  shift 2
  ip="$(server_ip "${name}")" || return 1
  set_ssh_opts
  scp -q -r "${SSH_OPTS[@]}" "$@" "sova-admin@${ip}:${dir}/"
}
