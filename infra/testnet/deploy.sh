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
#               Seeds go first. Each node host records the genesis hash
#               its installed `sova genesis-hash` prints
#               (out/servers/<name>.genesis_hash; bootnodes.sh publishes it).
#   ./deploy.sh alerts
#       Push TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID (from YOUR environment)
#       to /etc/sova/health.env (root 0600) on every host, over SSH stdin.
#   ./deploy.sh check
#       Validate config.env offline (no token, no SSH, no API call).
#   ./deploy.sh render [--out DIR] [--systemd-verify]
#       check, then write every host's files (host.env, /etc/sova/*, the
#       systemd units) under DIR (default out/render/<server>/) with the
#       same code setup-host.sh runs on the host, and lint them: every
#       EnvironmentFile exists, every ${VAR} a unit uses is defined.
#       --systemd-verify also runs `systemd-analyze verify` on them in a
#       throwaway ubuntu:24.04 container (needs Docker). Nothing remote.
#
# Needs: provision.sh up has run (out/servers/*.ipv4; a byo host is
# adopted there, and its address comes from config.env), the SSH key from
# config.env. No cloud API token is used here. Hetzner and byo hosts are
# configured identically, as sova-admin over SSH.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; }

CMD=deploy
ONLY=""
RENDER_DIR=""
SYSTEMD_VERIFY=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --only) ONLY="${2:?--only needs a server name}"; shift ;;
    --out) RENDER_DIR="${2:?--out needs a directory}"; shift ;;
    --systemd-verify) SYSTEMD_VERIFY=1 ;;
    alerts) CMD=alerts ;;
    check | render) CMD="$1"; DRY_RUN=1 ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown argument '$1'" ;;
  esac
  shift
done

load_config
if [[ "${CMD}" == check || "${CMD}" == render ]]; then
  validate_config
else
  validate_servers
  validate_sip6
fi
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
  if [[ "${DRY_RUN}" != 1 ]]; then
    ip="$(server_ip "${name}")"
  elif srv_is_byo "${s}" && [[ "$(srv_address "${s}")" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]]; then
    ip="$(srv_address "${s}")"
  fi
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
SOVA_SIP6=${SOVA_SIP6}
SOVA_SIP7=${SOVA_SIP7}
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
    local at="${name}"
    srv_is_byo "${s}" && at="${name}[byo $(srv_address "${s}")]"
    echo "+ rsync host/ + ${envf##*/} -> sova-admin@${at}:${REMOTE_DIR}/"
    echo "+ ssh sova-admin@${at} sudo bash ${REMOTE_DIR}/setup-host.sh ${REMOTE_DIR}/host.env ${3:-}"
    return 0
  fi
  local ip
  ip="$(server_ip "${name}")"
  set_ssh_opts
  rsync -a --delete -e "ssh ${SSH_OPTS[*]}" "${KIT_DIR}/host/" "sova-admin@${ip}:${REMOTE_DIR}/"
  kit_scp "${name}" "${REMOTE_DIR}" "${envf}"
  kit_ssh "${name}" "mv ${REMOTE_DIR}/${name}.host.env ${REMOTE_DIR}/host.env"
  local logf="${OUT_DIR}/servers/${name}.setup.log"
  # A full setup re-records the node's genesis hash (KIT-OUT genesis_hash,
  # `sova genesis-hash` on the host): drop the old one first, so a stale
  # hash never outlives a change of release or SOVA_SIP7.
  [[ -n "${3:-}" ]] || rm -f "${OUT_DIR}/servers/${name}.genesis_hash" "${OUT_DIR}/servers/${name}.genesis_sip7"
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
      list+="${list:+,}enode://DRYRUN-${name}-node-id@203.0.113.1:${SOVA_P2P_PORT}"
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

cmd_check() {
  log "config OK (${CONFIG_WARNINGS} open item(s) above, none blocking this step)"
}

# Lints one rendered host: every EnvironmentFile a unit names exists (or is
# optional, or is written by a later step), and every ${VAR} in its
# ExecStart is defined by one of them or by an Environment= line.
LINT_FAIL=0
lint_render() { # render-root server
  local root="$1" name="$2" unit ef path opt vars v defined f
  for unit in "${root}"/etc/systemd/system/*.service; do
    local envfiles=()
    while read -r ef; do
      opt=0
      [[ "${ef}" == -* ]] && opt=1 && ef="${ef#-}"
      path="${root}${ef}"
      if [[ -f "${path}" ]]; then
        envfiles+=("${path}")
      elif [[ ${opt} == 1 ]]; then
        :
      elif [[ "${ef}" == /etc/sova/cloudflared.env ]]; then
        echo "  note ${name}/${unit##*/}: ${ef} is written later by cloudflare.sh tunnels"
      else
        echo "  FAIL ${name}/${unit##*/}: EnvironmentFile ${ef} was not rendered"
        LINT_FAIL=$((LINT_FAIL + 1))
      fi
    done < <(sed -n 's/^EnvironmentFile=//p' "${unit}")
    # ExecStart with its continuation lines joined.
    # shellcheck disable=SC2016 # a literal ${NAME} pattern
    vars="$(awk '/^ExecStart=/{on=1} on{print} on && !/\\$/{on=0}' "${unit}" |
      grep -o '\${[A-Z_][A-Z0-9_]*}' | tr -d '${}' | sort -u || true)"
    for v in ${vars}; do
      defined=0
      grep -q "^Environment=${v}=" "${unit}" && defined=1
      for f in "${envfiles[@]+"${envfiles[@]}"}"; do
        grep -q "^${v}=" "${f}" && defined=1
      done
      if [[ ${defined} == 0 ]]; then
        echo "  FAIL ${name}/${unit##*/}: ExecStart uses \${${v}}, which no EnvironmentFile defines"
        LINT_FAIL=$((LINT_FAIL + 1))
      fi
    done
  done
  f="${root}/etc/sova/sova-node.env"
  if [[ -f "${f}" ]]; then
    for v in SOVA_CHAIN SOVA_ZEBRAD_RPC SOVA_DATADIR SOVA_P2P_PORT SOVA_HTTP_PORT SOVA_AUTH_PORT SOVA_RPC_PROFILE SOVA_SIP6 SOVA_SIP7; do
      grep -q "^${v}=." "${f}" || { echo "  FAIL ${name}: sova-node.env has no ${v}"; LINT_FAIL=$((LINT_FAIL + 1)); }
    done
    # SIP-6 mine mode signs: it needs the sealing key and a persistent
    # datadir (the seal journal lives under it).
    if grep -q '^SOVA_SIP6=1$' "${f}" && ! grep -q '^SOVA_FOLLOW_ONLY=1$' "${f}"; then
      grep -q '^SOVA_SEALER_KEYSTORE=/.' "${f}" ||
        { echo "  FAIL ${name}: SIP-6 mine mode without SOVA_SEALER_KEYSTORE"; LINT_FAIL=$((LINT_FAIL + 1)); }
    fi
    grep -q '^SOVA_EPOCH_BASE=.' "${f}" ||
      echo "  note ${name}: SOVA_EPOCH_BASE empty, so no epoch-base-pinned marker: sova-node stays stopped until 'Pin B'"
  fi
}

# systemd-analyze verify in a throwaway container: the real parser, with
# stub binaries at the paths the units name.
systemd_verify() { # render-dir
  need_cmd docker "for --systemd-verify"
  log "systemd-analyze verify in ubuntu:24.04 (throwaway container)"
  docker run --rm -v "$1:/render:ro" ubuntu:24.04 bash -c '
    set -e
    apt-get update -qq >/dev/null && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq systemd >/dev/null
    for b in /usr/local/bin/sova /usr/local/bin/sova-miner /usr/local/bin/sova-faucet /usr/bin/docker \
      /usr/bin/cloudflared /usr/local/lib/sova-infra/health.sh; do
      mkdir -p "$(dirname "$b")"; printf "#!/bin/sh\n" >"$b"; chmod 755 "$b"
    done
    for u in sova sova-faucet sova-keeper; do useradd --system "$u" 2>/dev/null || true; done
    # cloud-init installs Docker on the real hosts; stand in for its unit.
    printf "[Unit]\nDescription=stub\n[Service]\nExecStart=/usr/bin/docker\n" >/etc/systemd/system/docker.service
    rc=0
    for host in /render/*/; do
      rm -rf /etc/sova && cp -r "$host/etc/sova" /etc/sova
      cp "$host"/etc/systemd/system/* /etc/systemd/system/
      units=$(cd "$host/etc/systemd/system" && ls)
      if out=$(cd /etc/systemd/system && systemd-analyze verify $units 2>&1); then
        echo "  ok   $(basename "$host"): systemd-analyze verify: $(echo $units)"
      else
        echo "$out" | grep -v "^$" | sed "s|^|  FAIL $(basename "$host"): |"; rc=1
      fi
      (cd "$host/etc/systemd/system" && rm -f $(printf "/etc/systemd/system/%s " $units))
    done
    exit $rc'
}

cmd_render() {
  local dir="${RENDER_DIR:-${OUT_DIR}/render}" s name envf
  rm -rf "${dir}"
  mkdir -p "${dir}" "${OUT_DIR}/servers"
  for s in "${SERVERS[@]}"; do
    name="$(srv_name "${s}")"
    [[ -z "${ONLY}" || "${ONLY}" == "${name}" ]] || continue
    mkdir -p "${dir}/${name}"
    envf="${dir}/${name}/host.env"
    host_env "${s}" "$(bootnodes_for "${name}")" >"${envf}"
    bash "${KIT_DIR}/host/setup-host.sh" "${envf}" --render "${dir}/${name}" 2>&1 | sed 's/^/  /'
    lint_render "${dir}/${name}" "${name}"
    echo "  ${name} ($(srv_role "${s}")): $(cd "${dir}/${name}" && find etc -type f | sort | tr '\n' ' ')"
  done
  [[ ${LINT_FAIL} == 0 ]] || die "${LINT_FAIL} problem(s) in the rendered files"
  log "rendered and linted: ${dir}"
  if [[ ${SYSTEMD_VERIFY} == 1 ]]; then
    systemd_verify "${dir}" || die "systemd-analyze verify failed"
  fi
  log "config OK (${CONFIG_WARNINGS} open item(s) above)"
}

case "${CMD}" in
  deploy) cmd_deploy ;;
  alerts) cmd_alerts ;;
  check) cmd_check ;;
  render) cmd_render ;;
esac
