#!/usr/bin/env bash
# infra/testnet/provision.sh -- create the M1 testnet's Hetzner Cloud
# resources with the `hcloud` CLI, and adopt the bring-your-own hosts
# (SERVERS type "byo", e.g. the AWS keeper). Idempotent: every resource is
# looked up by name first and only created if absent; firewall rules are
# re-applied from config every run, so a re-run converges.
#
# A byo host is never created or firewalled here: the operator made it
# (Ubuntu 24.04, x86_64, the kit's SSH key; its firewall/security group
# per docs/ops/keeper-aws.md). `up` records its address and, if sova-admin
# can't log in yet, bootstraps it over SSH as the entry's first-login user
# with host/byo-bootstrap.py, which applies the same host/cloud-init.yaml
# a Hetzner server boots with. From then on it is an ordinary kit host.
#
# Why hcloud + bash, not Terraform: three or four servers, two firewalls
# and one SSH key, and nothing that needs a dependency graph. The CLI keeps
# no state file to lose or guard (no second copy of the truth next to the
# project), every step is a readable command that `--dry-run` prints
# verbatim, and it matches the rest of the repo's ops tooling (bash, curl,
# jq). A re-run is the drift check.
#
# Usage:
#   ./provision.sh up [--dry-run] [--my-ip]   create/converge everything
#   ./provision.sh status                     servers, IPs (Hetzner + byo)
#   ./provision.sh ssh-allow <cidr,...>       replace the SSH source list
#   ./provision.sh teardown [--dry-run] [--keep-volumes]
#                                             delete everything labelled
#                                             project=$HC_PROJECT_LABEL
#                                             (asks you to type the label;
#                                             never touches a byo host)
#
# Needs: HCLOUD_TOKEN in the environment (a Read & Write API token of the
# Hetzner project Rob created), unless every server is byo. --dry-run needs
# no token, makes no API call and no SSH connection: it prints the
# commands a run on an EMPTY project would issue.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; }

CMD="${1:-}"
[[ -n "${CMD}" ]] && shift
MY_IP=0
KEEP_VOLUMES=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --my-ip) MY_IP=1 ;;
    --keep-volumes) KEEP_VOLUMES=1 ;;
    -h | --help) usage; exit 0 ;;
    *) break ;;
  esac
  shift
done

load_config
validate_servers
LABEL="project=${HC_PROJECT_LABEL}"
FW_SEED="${HC_PROJECT_LABEL}-fw-seed"
FW_PRIVATE="${HC_PROJECT_LABEL}-fw-private"
SSH_KEY_NAME="${HC_PROJECT_LABEL}-admin"
mkdir -p "${OUT_DIR}/servers"

hc() { hcloud "$@"; }

# Does resource kind $1 named $2 exist? (dry run: unknown -> "no", so the
# printed plan is the full create sequence.)
exists() {
  [[ "${DRY_RUN}" == 1 ]] && return 1
  hc "$1" describe "$2" >/dev/null 2>&1
}

check_admin_cidrs() {
  if [[ "${MY_IP}" == 1 ]]; then
    if [[ "${DRY_RUN}" == 1 ]]; then
      ADMIN_CIDRS="203.0.113.7/32" # placeholder in a dry run: no lookup
      warn "--my-ip: dry run uses the placeholder ${ADMIN_CIDRS}"
    else
      local ip
      ip="$(curl -fsS --max-time 10 https://api.ipify.org)" || die "--my-ip: could not look up this machine's public IP"
      [[ "${ip}" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "--my-ip: unexpected answer '${ip}'"
      ADMIN_CIDRS="${ip}/32"
      log "SSH allowed from ${ADMIN_CIDRS} (this machine)"
    fi
  fi
  [[ -n "${ADMIN_CIDRS}" ]] || die "ADMIN_CIDRS is empty: set it in config.env or pass --my-ip"
  local c
  for c in ${ADMIN_CIDRS//,/ }; do
    [[ "${c}" =~ ^[0-9a-fA-F:.]+/[0-9]+$ ]] || die "ADMIN_CIDRS: '${c}' is not a CIDR"
    if [[ "${c}" == "0.0.0.0/0" || "${c}" == "::/0" ]] && [[ "${SOVA_ALLOW_SSH_FROM_ANYWHERE:-0}" != 1 ]]; then
      die "ADMIN_CIDRS contains ${c}; set SOVA_ALLOW_SSH_FROM_ANYWHERE=1 if you really mean it"
    fi
  done
}

# JSON array of CIDRs from ADMIN_CIDRS.
admin_cidrs_json() {
  local c out="" sep=""
  for c in ${ADMIN_CIDRS//,/ }; do
    out+="${sep}\"${c}\""
    sep=","
  done
  printf '[%s]' "${out}"
}

# Firewall rules (inbound only; Hetzner firewalls are stateful and allow
# all outbound when no outbound rule exists). authrpc (8551), the Sova
# HTTP RPC (8545), zebrad RPC (18232) and the faucet (18790) are never
# opened anywhere: they bind to 127.0.0.1 and reach the internet only via
# the Cloudflare Tunnel (outbound).
write_rules() {
  local admin
  admin="$(admin_cidrs_json)"
  local any='["0.0.0.0/0","::/0"]'
  cat >"${OUT_DIR}/fw-private.json" <<JSON
[
  {"direction":"in","protocol":"tcp","port":"22","source_ips":${admin},"description":"SSH (admin CIDRs only)"},
  {"direction":"in","protocol":"icmp","source_ips":${any},"description":"ping"}
]
JSON
  cat >"${OUT_DIR}/fw-seed.json" <<JSON
[
  {"direction":"in","protocol":"tcp","port":"22","source_ips":${admin},"description":"SSH (admin CIDRs only)"},
  {"direction":"in","protocol":"icmp","source_ips":${any},"description":"ping"},
  {"direction":"in","protocol":"tcp","port":"${SOVA_P2P_PORT}","source_ips":${any},"description":"Sova P2P (RLPx)"},
  {"direction":"in","protocol":"udp","port":"${SOVA_P2P_PORT}","source_ips":${any},"description":"Sova discovery (discv4+discv5)"},
  {"direction":"in","protocol":"tcp","port":"${ZEBRA_P2P_PORT}","source_ips":${any},"description":"Zcash testnet P2P (courtesy peer, D9)"}
]
JSON
  jq -e . "${OUT_DIR}/fw-private.json" "${OUT_DIR}/fw-seed.json" >/dev/null || die "generated firewall rules are not valid JSON"
}

write_cloud_init() {
  [[ -f "${SSH_PUBLIC_KEY_FILE}" ]] || {
    if [[ "${DRY_RUN}" == 1 ]]; then
      warn "no ${SSH_PUBLIC_KEY_FILE} (dry run uses a placeholder key)"
      PUBKEY="ssh-ed25519 AAAA...placeholder sova-testnet"
    else
      die "no SSH public key at ${SSH_PUBLIC_KEY_FILE} (ssh-keygen -t ed25519 -f ${SSH_PRIVATE_KEY_FILE%.pub})"
    fi
  }
  [[ -n "${PUBKEY:-}" ]] || PUBKEY="$(cat "${SSH_PUBLIC_KEY_FILE}")"
  [[ "${PUBKEY}" == ssh-* ]] || die "${SSH_PUBLIC_KEY_FILE} does not look like a public key"
  [[ "${PUBKEY}" != *PRIVATE* ]] || die "${SSH_PUBLIC_KEY_FILE} is a PRIVATE key"
  # Only the public key is templated in; the file holds nothing secret.
  awk -v k="${PUBKEY}" '{ gsub(/@SSH_PUBLIC_KEY@/, k); print }' \
    "${KIT_DIR}/host/cloud-init.yaml" >"${OUT_DIR}/cloud-init.yaml"
}

# ssh/scp to a byo host as its first-login user (before adoption).
byo_ssh() { # user ip command
  local user="$1" ip="$2"
  shift 2
  set_ssh_opts
  # shellcheck disable=SC2029 # remote commands are composed on purpose
  ssh "${SSH_OPTS[@]}" "${user}@${ip}" "$@"
}

# Adopts one byo host: record it, and bootstrap it unless sova-admin can
# already log in (a re-run, or a host launched with the kit's cloud-init as
# its user data).
adopt_byo() { # server-entry
  local s="$1" name role addr user ip disk
  name="$(srv_name "${s}")"
  role="$(srv_role "${s}")"
  addr="$(srv_address "${s}")"
  user="$(srv_first_login_user "${s}")"
  echo "${role}" >"${OUT_DIR}/servers/${name}.role"
  if [[ "${DRY_RUN}" == 1 ]]; then
    echo "+ byo ${name} (${role}): nothing created; firewall = the operator's security group (docs/ops/keeper-aws.md)"
    echo "+ resolve ${addr} > ${OUT_DIR}/servers/${name}.ipv4"
    echo "+ ssh sova-admin@${addr} true  || bootstrap as ${user:-<first-login-user>}@${addr}:"
    echo "+   scp out/cloud-init.yaml host/byo-bootstrap.py -> ${user:-<first-login-user>}@${addr}:/tmp/sova-bootstrap/"
    echo "+   ssh ${user:-<first-login-user>}@${addr} sudo python3 /tmp/sova-bootstrap/byo-bootstrap.py /tmp/sova-bootstrap/cloud-init.yaml"
    echo "+ ssh sova-admin@${addr} true   (adopted: from here on like a Hetzner host)"
    return 0
  fi
  ip="$(server_ip "${name}")"
  echo "${ip}" >"${OUT_DIR}/servers/${name}.ipv4"
  rm -f "${OUT_DIR}/servers/${name}.ipv6"
  if kit_ssh "${name}" true 2>/dev/null; then
    log "${name} (byo, ${ip}): sova-admin logs in; already adopted"
  else
    [[ -n "${user}" ]] || die "${name}: sova-admin@${ip} does not log in and the entry names no first-login user (6th field, 'ubuntu' on AWS)"
    local tries=0
    until byo_ssh "${user}" "${ip}" true 2>/dev/null; do
      tries=$((tries + 1))
      [[ ${tries} -lt 20 ]] || die "${name}: cannot ssh ${user}@${ip}. Is the instance running, does its security group allow tcp/22 from ${ADMIN_CIDRS} (this machine), and was it launched with the ${SSH_PUBLIC_KEY_FILE} key pair?"
      sleep 15
    done
    log "${name}: bootstrapping as ${user}@${ip} (the kit's cloud-init base; ~3-6 min)"
    byo_ssh "${user}" "${ip}" 'rm -rf /tmp/sova-bootstrap && mkdir -m 700 /tmp/sova-bootstrap'
    set_ssh_opts
    scp -q "${SSH_OPTS[@]}" "${OUT_DIR}/cloud-init.yaml" "${KIT_DIR}/host/byo-bootstrap.py" "${user}@${ip}:/tmp/sova-bootstrap/"
    local logf="${OUT_DIR}/servers/${name}.bootstrap.log"
    # The image's own first boot may still be running apt: wait for it.
    if ! byo_ssh "${user}" "${ip}" 'sudo cloud-init status --wait >/dev/null 2>&1; sudo python3 /tmp/sova-bootstrap/byo-bootstrap.py /tmp/sova-bootstrap/cloud-init.yaml 2>&1' | tee "${logf}"; then
      die "${name}: bootstrap failed (log: ${logf})"
    fi
    kit_ssh "${name}" true || die "${name}: bootstrapped, but sova-admin@${ip} still cannot log in (log: ${logf})"
    log "${name}: adopted; ${user} can no longer log in over SSH (AllowUsers sova-admin)"
  fi
  disk="$(kit_ssh "${name}" "df -BG --output=size / | tail -1 | tr -dc 0-9" 2>/dev/null || true)"
  if [[ "${disk}" =~ ^[0-9]+$ ]] && ((disk * 10 < $(srv_volume_gb "${s}") * 9)); then
    warn "${name}: root disk is ${disk} GB, config says $(srv_volume_gb "${s}") GB"
  fi
}

cmd_up() {
  need_cmd jq
  local s
  if [[ "${DRY_RUN}" != 1 ]]; then
    for s in "${SERVERS[@]}"; do
      if srv_is_byo "${s}" && srv_address_pending "${s}"; then
        die "$(srv_name "${s}"): address $(srv_address "${s}") is still pending in config.env (docs/ops/keeper-aws.md: the Elastic IP)"
      fi
    done
  fi
  if has_hetzner_servers; then
    [[ "${DRY_RUN}" == 1 ]] || need_cmd hcloud "brew install hcloud"
    need_env HCLOUD_TOKEN
  fi
  check_admin_cidrs
  write_rules
  write_cloud_init
  [[ "${DRY_RUN}" == 1 ]] && log "DRY RUN: no API calls, no SSH; this is the create sequence for an empty project"

  if has_hetzner_servers; then
    log "SSH key"
    if exists ssh-key "${SSH_KEY_NAME}"; then
      log "ssh-key ${SSH_KEY_NAME} exists"
    else
      run hcloud ssh-key create --name "${SSH_KEY_NAME}" --public-key-from-file "${SSH_PUBLIC_KEY_FILE}" --label "${LABEL}"
    fi

    log "Firewalls"
    local fw
    for fw in "${FW_SEED}:fw-seed" "${FW_PRIVATE}:fw-private"; do
      if exists firewall "${fw%%:*}"; then
        run hcloud firewall replace-rules "${fw%%:*}" --rules-file "${OUT_DIR}/${fw#*:}.json"
      else
        run hcloud firewall create --name "${fw%%:*}" --rules-file "${OUT_DIR}/${fw#*:}.json" --label "${LABEL}"
      fi
    done
  fi

  log "Volumes and servers"
  local name type loc vol role firewall vol_args
  for s in "${SERVERS[@]}"; do
    if srv_is_byo "${s}"; then
      adopt_byo "${s}"
      continue
    fi
    name="$(srv_name "${s}")"
    type="$(srv_type "${s}")"
    loc="$(srv_location "${s}")"
    vol="$(srv_volume_gb "${s}")"
    role="$(srv_role "${s}")"
    firewall="${FW_PRIVATE}"
    [[ "${role}" == seed ]] && firewall="${FW_SEED}"
    vol_args=()
    if [[ "${vol}" -gt 0 ]]; then
      if exists volume "${name}-data"; then
        log "volume ${name}-data exists"
      else
        # Formatted, not automounted: setup-host.sh mounts it at
        # /var/lib/sova by UUID, so the path never depends on the id.
        run hcloud volume create --name "${name}-data" --size "${vol}" --location "${loc}" \
          --format ext4 --label "${LABEL}" --label "server=${name}"
      fi
      vol_args=(--volume "${name}-data")
    fi
    if exists server "${name}"; then
      log "server ${name} exists (not recreated; firewall ${firewall} re-applied above)"
    else
      run hcloud server create --name "${name}" --type "${type}" --image "${HC_IMAGE}" \
        --location "${loc}" --ssh-key "${SSH_KEY_NAME}" --firewall "${firewall}" \
        ${vol_args[@]+"${vol_args[@]}"} --label "${LABEL}" --label "role=${role}" \
        --user-data-from-file "${OUT_DIR}/cloud-init.yaml"
    fi
    echo "${role}" >"${OUT_DIR}/servers/${name}.role"
    if [[ "${DRY_RUN}" == 1 ]]; then
      echo "+ hcloud server ip ${name} > ${OUT_DIR}/servers/${name}.ipv4"
    else
      hc server ip "${name}" >"${OUT_DIR}/servers/${name}.ipv4"
      hc server ip -6 "${name}" >"${OUT_DIR}/servers/${name}.ipv6" 2>/dev/null || true
      log "${name} (${role}) at $(cat "${OUT_DIR}/servers/${name}.ipv4")"
    fi
  done

  if [[ "${DRY_RUN}" != 1 ]]; then
    log "Waiting for cloud-init on every server (first boot: ~3-6 min)"
    for s in "${SERVERS[@]}"; do
      srv_is_byo "${s}" && continue # adopt_byo waited for it
      name="$(srv_name "${s}")"
      local tries=0
      local ci_out
      # Output into a variable first: `kit_ssh | grep -q` can SIGPIPE ssh
      # on a match, and pipefail then reads "done" as not done.
      until ci_out="$(kit_ssh "${name}" 'cloud-init status --wait >/dev/null 2>&1; cloud-init status' 2>/dev/null)" \
        && grep -q 'status: done' <<<"${ci_out}"; do
        tries=$((tries + 1))
        [[ ${tries} -lt 40 ]] || die "${name}: cloud-init did not finish (ssh sova-admin@$(server_ip "${name}") and check /var/log/cloud-init-output.log)"
        sleep 15
      done
      log "${name}: cloud-init done"
    done
    log "Provisioned. Next: ./deploy.sh (docs/ops/testnet-launch.md step O2)"
  fi
}

cmd_status() {
  local s
  if has_hetzner_servers; then
    need_cmd hcloud
    need_env HCLOUD_TOKEN
    hc server list --selector "${LABEL}" -o columns=name,status,ipv4,ipv6,type,location,labels
    hc volume list --selector "${LABEL}"
    hc firewall list --selector "${LABEL}"
  fi
  for s in "${SERVERS[@]}"; do
    srv_is_byo "${s}" || continue
    printf 'byo  %-16s %-8s %-20s %s\n' "$(srv_name "${s}")" "$(srv_role "${s}")" "$(srv_address "${s}")" \
      "$(kit_ssh "$(srv_name "${s}")" 'echo sova-admin ok' 2>/dev/null || echo 'sova-admin: no login (not adopted yet?)')"
  done
}

# Byo hosts' firewalls are not ours to change: say what to change.
byo_firewall_reminder() {
  local s
  for s in "${SERVERS[@]}"; do
    srv_is_byo "${s}" || continue
    warn "$(srv_name "${s}") is byo: set its security group's SSH (tcp/22) source to ${ADMIN_CIDRS} yourself (docs/ops/keeper-aws.md)"
  done
}

cmd_ssh_allow() {
  ADMIN_CIDRS="${1:-}"
  need_cmd hcloud
  need_env HCLOUD_TOKEN
  check_admin_cidrs
  write_rules
  run hcloud firewall replace-rules "${FW_SEED}" --rules-file "${OUT_DIR}/fw-seed.json"
  run hcloud firewall replace-rules "${FW_PRIVATE}" --rules-file "${OUT_DIR}/fw-private.json"
  byo_firewall_reminder
  log "Also update ADMIN_CIDRS in config.env so the next 'up' keeps it."
}

# Deletes only resources labelled project=$HC_PROJECT_LABEL.
cmd_teardown() {
  [[ "${DRY_RUN}" == 1 ]] || need_cmd hcloud
  need_env HCLOUD_TOKEN
  # (byo hosts are listed at the end, never deleted)
  if [[ "${DRY_RUN}" != 1 ]]; then
    printf 'This deletes every server%s, firewall and SSH key labelled %s.\nType the label (%s) to continue: ' \
      "$([[ "${KEEP_VOLUMES}" == 1 ]] || echo ', VOLUME (zebrad + node state)')" "${LABEL}" "${HC_PROJECT_LABEL}" >&2
    local answer
    read -r answer
    [[ "${answer}" == "${HC_PROJECT_LABEL}" ]] || die "not confirmed; nothing deleted"
  fi
  local name
  list() {
    if [[ "${DRY_RUN}" == 1 ]]; then
      case "$1" in
        server) local s; for s in "${SERVERS[@]}"; do srv_is_byo "${s}" || srv_name "${s}"; done ;;
        volume) local s; for s in "${SERVERS[@]}"; do srv_is_byo "${s}" || { [[ "$(srv_volume_gb "${s}")" -gt 0 ]] && echo "$(srv_name "${s}")-data"; }; done ;;
        firewall) printf '%s\n' "${FW_SEED}" "${FW_PRIVATE}" ;;
        ssh-key) echo "${SSH_KEY_NAME}" ;;
      esac
    else
      hc "$1" list --selector "${LABEL}" -o noheader -o columns=name
    fi
  }
  for name in $(list server); do run hcloud server delete "${name}"; done
  if [[ "${KEEP_VOLUMES}" == 1 ]]; then
    log "keeping volumes (detached; ~EUR 0.057/GB-month until deleted)"
  else
    for name in $(list volume); do run hcloud volume delete "${name}"; done
  fi
  for name in $(list firewall); do run hcloud firewall delete "${name}"; done
  for name in $(list ssh-key); do run hcloud ssh-key delete "${name}"; done
  local s
  if [[ "${DRY_RUN}" != 1 ]]; then
    for s in "${SERVERS[@]}"; do
      srv_is_byo "${s}" || rm -f "${OUT_DIR}/servers/$(srv_name "${s}").ipv4" "${OUT_DIR}/servers/$(srv_name "${s}").ipv6"
    done
  fi
  for s in "${SERVERS[@]}"; do
    srv_is_byo "${s}" && warn "$(srv_name "${s}") is byo and was NOT touched: stop or terminate it (and release its Elastic IP) in its provider's console"
  done
  log "Teardown done. Remove the Cloudflare records with ./cloudflare.sh teardown."
}

case "${CMD}" in
  up) cmd_up ;;
  status) cmd_status ;;
  ssh-allow) cmd_ssh_allow "${1:-}" ;;
  teardown) cmd_teardown ;;
  "" | -h | --help | help) usage ;;
  *) die "unknown command '${CMD}' (up | status | ssh-allow | teardown)" ;;
esac
