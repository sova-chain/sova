#!/usr/bin/env bash
# infra/testnet/deploy.sh -- configure the provisioned hosts for their
# roles (host/setup-host.sh over SSH). Idempotent; re-run it to roll out a
# new release tag, the pinned epoch base, or a changed bootnode list.
#
# Usage:
#   ./deploy.sh [--dry-run] [--only <server>]
#       Pass 1: on every node host, create (or read) the p2p node key and
#               record its enode (out/servers/<name>.enode).
#       Pass 2: write each host's host.env (seeds get the OTHER seeds as
#               bootnodes, everyone else gets every seed, plus
#               EXTRA_BOOTNODES from config.env) and run setup-host.sh.
#               Seeds go first.
#   ./deploy.sh alerts
#       Push TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID (from YOUR environment)
#       to /etc/sova/health.env (root 0600) on every host, over SSH stdin.
#
# Needs: provision.sh up has run (out/servers/*.ipv4), the SSH key from
# config.env. No cloud API token is used here.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

usage() { sed -n '2,19p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

CMD=deploy
ONLY=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --only) ONLY="${2:?--only needs a server name}"; shift ;;
    alerts) CMD=alerts ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown argument '$1'" ;;
  esac
  shift
done

load_config
validate_servers
EXTRA_BOOTNODES="${EXTRA_BOOTNODES:-}"
REMOTE_DIR=/tmp/sova-infra-kit

is_node_role() { [[ "$1" == seed || "$1" == rpc || "$1" == keeper ]]; }

# Seeds first, then everything else, in config order.
ordered_servers() {
  local s
  for s in "${SERVERS[@]}"; do [[ "$(srv_role "${s}")" == seed ]] && echo "${s}"; done
  for s in "${SERVERS[@]}"; do [[ "$(srv_role "${s}")" != seed ]] && echo "${s}"; done
  return 0
}

host_env() { # server-entry bootnodes
  local s="$1" name role
  name="$(srv_name "${s}")"
  role="$(srv_role "${s}")"
  local ip="203.0.113.1"
  [[ "${DRY_RUN}" == 1 ]] || ip="$(server_ip "${name}")"
  local ref_rpc=""
  # A seed can compare its head with the public RPC (rpc-1) to tell "we
  # lag" from "the network stalled".
  [[ "${role}" == seed ]] && ref_rpc="https://${RPC_HOST}"
  cat <<EOF
ROLE=${role}
PUBLIC_IPV4=${ip}
SOVA_RELEASE_TAG=${SOVA_RELEASE_TAG}
SOVA_RELEASE_REPO=${SOVA_RELEASE_REPO}
SOVA_SOURCE_REPO_URL=${SOVA_SOURCE_REPO_URL}
BUILD_FROM_SOURCE=${BUILD_FROM_SOURCE:-0}
ZEBRA_IMAGE=${ZEBRA_IMAGE}
ZEBRA_IMAGE_DIGEST=${ZEBRA_IMAGE_DIGEST}
SOVA_EPOCH_BASE=${SOVA_EPOCH_BASE}
SOVA_EMISSION_SCHEDULE=${SOVA_EMISSION_SCHEDULE}
SOVA_BOOTNODES=$2
SOVA_P2P_PORT=${SOVA_P2P_PORT}
ZEBRA_P2P_PORT=${ZEBRA_P2P_PORT}
ZEBRA_RPC_PORT=${ZEBRA_RPC_PORT}
SOVA_HTTP_PORT=${SOVA_HTTP_PORT}
SOVA_AUTH_PORT=${SOVA_AUTH_PORT}
FAUCET_PORT=${FAUCET_PORT}
HEALTH_REFERENCE_RPC=${ref_rpc}
KEEPER_PER_EPOCH_ZAT=${KEEPER_PER_EPOCH_ZAT:-10000}
KEEPER_BUDGET_ZAT=${KEEPER_BUDGET_ZAT:-35000000}
KEEPER_LIFETIME_BUDGET_ZAT=${KEEPER_LIFETIME_BUDGET_ZAT:-1000000000}
EOF
}

# Copies host/ and a host.env to the server and runs setup-host.sh with
# extra args; records every "KIT-OUT key value" line as
# out/servers/<name>.<key>.
remote_setup() { # server-entry bootnodes [--enode-only]
  local s="$1" name envf
  name="$(srv_name "${s}")"
  envf="${OUT_DIR}/servers/${name}.host.env"
  host_env "${s}" "$2" >"${envf}"
  if [[ "${DRY_RUN}" == 1 ]]; then
    echo "+ rsync host/ + ${envf##*/} -> sova-admin@${name}:${REMOTE_DIR}/"
    echo "+ ssh sova-admin@${name} sudo bash ${REMOTE_DIR}/setup-host.sh ${REMOTE_DIR}/host.env ${3:-}"
    return 0
  fi
  local ip
  ip="$(server_ip "${name}")"
  set_ssh_opts
  rsync -a --delete -e "ssh ${SSH_OPTS[*]}" "${KIT_DIR}/host/" "sova-admin@${ip}:${REMOTE_DIR}/"
  kit_scp "${name}" "${REMOTE_DIR}" "${envf}"
  kit_ssh "${name}" "mv ${REMOTE_DIR}/${name}.host.env ${REMOTE_DIR}/host.env"
  local logf="${OUT_DIR}/servers/${name}.setup.log"
  if ! kit_ssh "${name}" "sudo bash ${REMOTE_DIR}/setup-host.sh ${REMOTE_DIR}/host.env ${3:-} 2>&1" | tee "${logf}"; then
    die "${name}: setup-host.sh failed (log: ${logf})"
  fi
  local key value
  while read -r _ key value; do
    printf '%s\n' "${value}" >"${OUT_DIR}/servers/${name}.${key}"
  done < <(grep '^KIT-OUT ' "${logf}")
}

# Comma-joined enodes of all seeds except $1, plus EXTRA_BOOTNODES.
bootnodes_for() {
  local self="$1" s name list="" f
  for s in "${SERVERS[@]}"; do
    [[ "$(srv_role "${s}")" == seed ]] || continue
    name="$(srv_name "${s}")"
    [[ "${name}" == "${self}" ]] && continue
    f="${OUT_DIR}/servers/${name}.enode"
    if [[ -s "${f}" ]]; then
      list+="${list:+,}$(cat "${f}")"
    elif [[ "${DRY_RUN}" == 1 ]]; then
      list+="${list:+,}enode://<${name}>@<ip>:${SOVA_P2P_PORT}"
    else
      die "no enode recorded for seed ${name}"
    fi
  done
  [[ -n "${EXTRA_BOOTNODES}" ]] && list+="${list:+,}${EXTRA_BOOTNODES}"
  printf '%s' "${list}"
}

cmd_deploy() {
  mkdir -p "${OUT_DIR}/servers"
  [[ "${DRY_RUN}" == 1 ]] && log "DRY RUN: nothing is copied or run"
  [[ -n "${SOVA_EPOCH_BASE}" ]] || warn "SOVA_EPOCH_BASE is not pinned: sova nodes are installed but will not start (runbook: Pin B)"
  local s name role
  log "Pass 1: node keys and enodes"
  while read -r s; do
    name="$(srv_name "${s}")"
    role="$(srv_role "${s}")"
    is_node_role "${role}" || continue
    [[ -z "${ONLY}" || "${ONLY}" == "${name}" || "${role}" == seed ]] || continue
    remote_setup "${s}" "" --enode-only
    [[ "${DRY_RUN}" == 1 ]] || log "${name}: $(cat "${OUT_DIR}/servers/${name}.enode")"
  done < <(ordered_servers)

  log "Pass 2: full setup"
  while read -r s; do
    name="$(srv_name "${s}")"
    [[ -z "${ONLY}" || "${ONLY}" == "${name}" ]] || continue
    remote_setup "${s}" "$(bootnodes_for "${name}")"
  done < <(ordered_servers)
  log "Deployed. Next: ./cloudflare.sh all, then ./bootnodes.sh (runbook O3-O4)"
}

cmd_alerts() {
  need_env TELEGRAM_BOT_TOKEN
  need_env TELEGRAM_CHAT_ID
  local s name
  for s in "${SERVERS[@]}"; do
    name="$(srv_name "${s}")"
    if [[ "${DRY_RUN}" == 1 ]]; then
      echo "+ ssh sova-admin@${name} \"sudo sh -c 'umask 077 && cat > /etc/sova/health.env'\" <<< (token from env, on stdin)"
      continue
    fi
    printf 'TELEGRAM_BOT_TOKEN=%s\nTELEGRAM_CHAT_ID=%s\n' "${TELEGRAM_BOT_TOKEN}" "${TELEGRAM_CHAT_ID}" |
      kit_ssh "${name}" "sudo sh -c 'umask 077 && cat > /etc/sova/health.env'"
    log "${name}: alerts configured"
  done
}

case "${CMD}" in
  deploy) cmd_deploy ;;
  alerts) cmd_alerts ;;
esac
