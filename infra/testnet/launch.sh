#!/usr/bin/env bash
# infra/testnet/launch.sh -- the launch-day command. Runs the kit's scripts
# in order; every one of them is idempotent, so this is safe to re-run and
# re-running is how you continue after a wait. It stops, and says why, at
# each point that needs time or a person:
#
#   1 check      config.env is valid; the cloud tokens are present
#   2 provision  provision.sh up --my-ip          (Hetzner; byo hosts such
#                                                  as the AWS keeper are
#                                                  adopted over SSH)
#   3 hosts      deploy.sh                        (zebrad starts syncing)
#   4 edge       cloudflare.sh all [+ deploy.sh alerts]
#   5 sync       STOP until every host's zebrad is at the Zcash tip
#   6 chain      STOP unless --go. Then: epoch-base.sh propose + pin,
#                deploy.sh (sova nodes start), bootnodes.sh, publish.sh join
#   7 contracts  deploy-contracts.sh keygen (once) + deploy --via the rpc
#                host; STOP with funding instructions while the deployer
#                holds no SOVA
#   8 smoke      smoke.sh all
#
# Exit status: 0 = the whole sequence ran, 2 = stopped at a wait (the
# reason is printed), anything else = an error.
#
# Usage: ./launch.sh [--dry-run] [--go]
#   --dry-run  print every step, touch nothing (no token needed)
#   --go       allowed to pin the epoch base B, i.e. to start the chain.
#              Without it the run stops at step 6 and prints the B it
#              would pin.
# Runbook: docs/ops/testnet-launch.md. Keys: docs/ops/wallets.md.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; }

GO=0
DRY=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1; DRY=(--dry-run) ;;
    --go) GO=1 ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown argument '$1'" ;;
  esac
  shift
done
cd "${KIT_DIR}"

step() { printf '\n==== %s\n' "$*" >&2; }
stop() {
  printf '\n==== STOP: %s\n' "$*" >&2
  printf '     Re-run ./launch.sh%s to continue.\n' "$([[ ${GO} == 1 ]] && echo ' --go')" >&2
  exit 2
}

step "1 check"
./deploy.sh check
load_config
if [[ "${DRY_RUN}" != 1 ]]; then
  if has_hetzner_servers; then need_env HCLOUD_TOKEN; fi
  need_env CLOUDFLARE_API_TOKEN
  need_env CLOUDFLARE_ACCOUNT_ID
  need_env CLOUDFLARE_ZONE_ID
fi

step "2 provision"
./provision.sh up --my-ip "${DRY[@]+"${DRY[@]}"}"

step "3 hosts"
./deploy.sh "${DRY[@]+"${DRY[@]}"}"

step "4 edge"
./cloudflare.sh "${DRY[@]+"${DRY[@]}"}" all
if [[ -n "${TELEGRAM_BOT_TOKEN:-}" ]]; then
  ./deploy.sh "${DRY[@]+"${DRY[@]}"}" alerts
fi

step "5 sync"
# "name" or "name[byo address]", for the dry-run plan.
host_label() {
  if srv_is_byo "$1"; then echo "$(srv_name "$1")[byo $(srv_address "$1")]"; else srv_name "$1"; fi
}
if [[ "${DRY_RUN}" == 1 ]]; then
  for s in "${SERVERS[@]}"; do
    echo "+ ssh sova-admin@$(host_label "${s}"): zebrad getblockchaininfo, blocks within 5 of estimatedheight"
  done
else
  behind=""
  for s in "${SERVERS[@]}"; do
    name="$(srv_name "${s}")"
    info="$(kit_ssh "${name}" "curl -fsS --max-time 10 -H 'Content-Type: application/json' \
      --data '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getblockchaininfo\",\"params\":[]}' \
      http://127.0.0.1:${ZEBRA_RPC_PORT}" 2>/dev/null)" || info=""
    tip="$(jq -r '.result.blocks // empty' <<<"${info}" 2>/dev/null || true)"
    est="$(jq -r '.result.estimatedheight // .result.blocks // empty' <<<"${info}" 2>/dev/null || true)"
    if [[ -z "${tip}" || -z "${est}" ]]; then
      behind+=" ${name}(zebrad not answering yet)"
    elif ((est - tip > 5)); then
      behind+=" ${name}(${tip}/${est}, $((tip * 100 / est))%)"
    else
      log "${name}: zebrad at the tip (${tip})"
    fi
  done
  [[ -z "${behind}" ]] || stop "zebrad still syncing:${behind}. From zero this takes about half a day."
fi

step "6 chain"
if [[ -z "${SOVA_EPOCH_BASE}" ]]; then
  seed="$(servers_with_role seed | head -1)"
  if [[ "${DRY_RUN}" == 1 ]]; then
    echo "+ ./epoch-base.sh propose --via ${seed}; ./epoch-base.sh pin <B>  (only with --go)"
  else
    proposal="$(./epoch-base.sh propose --via "${seed}")"
    printf '%s\n' "${proposal}"
    b="$(sed -n 's/^proposed B = \([0-9]*\).*/\1/p' <<<"${proposal}")"
    [[ "${b}" =~ ^[0-9]+$ ]] || die "could not read the proposed B"
    [[ ${GO} == 1 ]] || stop "ready to start the chain at B = ${b}. That is the point of no return for this genesis: re-run with --go."
    ./epoch-base.sh pin "${b}"
    load_config
  fi
fi
./deploy.sh "${DRY[@]+"${DRY[@]}"}"
if [[ "${DRY_RUN}" == 1 ]]; then
  echo "+ ./bootnodes.sh && ./bootnodes.sh --verify && ./publish.sh join"
  for s in "${SERVERS[@]}"; do
    case "$(srv_role "${s}")" in seed | rpc | keeper) echo "+   bootnodes --verify: $(host_label "${s}") logged enode == recorded" ;; esac
  done
else
  ./bootnodes.sh
  ./bootnodes.sh --verify || stop "bootnode check failed (nodes may still be starting; output above)"
  ./publish.sh join
fi

step "7 contracts"
rpc_host="$(servers_with_role rpc | head -1)"
if [[ "${DRY_RUN}" == 1 ]]; then
  echo "+ ./deploy-contracts.sh keygen   (once)"
  echo "+ ./deploy-contracts.sh deploy --via ${rpc_host}"
else
  [[ -f "${DEPLOYER_DIR:-${HOME}/.config/sova-testnet/deployer}/keystore.json" ]] || ./deploy-contracts.sh keygen
  if ! ./deploy-contracts.sh deploy --via "${rpc_host}"; then
    stop "day-one contracts not deployed (reason above). If the deployer holds no SOVA yet, mine into it: docs/ops/wallets.md, 'Deployer'."
  fi
fi

step "8 smoke"
if [[ "${DRY_RUN}" == 1 ]]; then
  echo "+ ./smoke.sh all"
  for s in "${SERVERS[@]}"; do
    echo "+   smoke: $(host_label "${s}") ($(srv_role "${s}")): edge ports from here + services/C5/health over SSH"
  done
else
  ./smoke.sh all
fi
log "launch sequence complete. The 'done' proof and the announcement: docs/ops/testnet-launch.md, step 9."
