#!/usr/bin/env bash
# infra/testnet/lib.sh -- shared helpers for the M1 launch kit. Sourced, not
# run. Nothing here reads a credential file; tokens come from the
# environment only, and are never echoed.

KIT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC2034 # used by publish.sh
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

# SERVERS entries are name:type:location:volume_gb:role.
srv_name() { cut -d: -f1 <<<"$1"; }
srv_type() { cut -d: -f2 <<<"$1"; }
srv_location() { cut -d: -f3 <<<"$1"; }
srv_volume_gb() { cut -d: -f4 <<<"$1"; }
srv_role() { cut -d: -f5 <<<"$1"; }

# Names of the servers with role $1, one per line.
servers_with_role() {
  local s
  for s in "${SERVERS[@]}"; do
    [[ "$(srv_role "${s}")" == "$1" ]] && srv_name "${s}"
  done
  return 0
}

validate_servers() {
  local s role name seen=" "
  for s in "${SERVERS[@]}"; do
    name="$(srv_name "${s}")"
    role="$(srv_role "${s}")"
    [[ "${name}" =~ ^[a-z0-9-]+$ ]] || die "config: bad server name '${name}'"
    [[ "${seen}" != *" ${name} "* ]] || die "config: duplicate server '${name}'"
    seen+="${name} "
    case "${role}" in seed | rpc | faucet | keeper) ;; *) die "config: ${name}: unknown role '${role}'" ;; esac
    [[ "$(srv_volume_gb "${s}")" =~ ^[0-9]+$ ]] || die "config: ${name}: volume_gb must be a number (0 = none)"
  done
  [[ -n "$(servers_with_role seed)" ]] || die "config: at least one seed server is required"
}

# Public IPv4 of a provisioned server, from the record provision.sh wrote.
server_ip() {
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

# ssh as the admin user (created by cloud-init) to server $1.
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
