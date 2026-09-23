#!/usr/bin/env bash
# infra/testnet/deploy-contracts.sh -- the day-one contracts (WSOVA,
# Uniswap-v2 factory + router, Multicall3, Ashwings, AshwingsMarket once it
# exists) on the public testnet. A thin wrapper: the deploy logic is
# contracts/script/deploy-kit.sh, the same code box/deploy-dapps.sh runs.
#
# Usage:
#   ./deploy-contracts.sh keygen
#       Make the throwaway deployer: a foundry keystore plus a random
#       password file in $DEPLOYER_DIR (default
#       ~/.config/sova-testnet/deployer), both mode 600, never in the repo.
#       Prints the address to fund. Refuses to overwrite an existing one.
#   ./deploy-contracts.sh plan   [--via <server> | --rpc URL]
#       Sends nothing: what is recorded, what would be deployed with which
#       constructor values, estimated gas, the deployer's balance.
#   ./deploy-contracts.sh deploy [--via <server> | --rpc URL] [--redeploy KEY]
#       Deploy what is missing, verify everything, record the addresses in
#       deployments/<DEPLOY_CHAIN_NAME>.json (commit that file). Re-running
#       is a no-op.
#   ./deploy-contracts.sh verify [--via <server> | --rpc URL]
#       Read-only, no key: every recorded contract has this checkout's code
#       and answers its getters. Works through the public edge too:
#       --rpc https://$RPC_HOST (smoke.sh contracts does exactly that).
#   Any command: --out FILE overrides the record path.
#
# RPC: --via sova-rpc-1 opens an SSH tunnel to that host's
# 127.0.0.1:$SOVA_HTTP_PORT for the length of the run. Deploy through the
# tunnel, not the edge: the edge Worker caps a request at 64 KB and the
# router's init code is 22 KB (44 KB as hex), so a bigger contract would be
# refused there. Default RPC: $DEPLOY_RPC from config.env.
#
# Values (config.env or the environment; see docs/ops/wallets.md):
#   ASHWINGS_TREASURY, ASHWINGS_ZEC_PAYEE, ASHWINGS_PRICE_WEI,
#   ASHWINGS_PRICE_ZAT, MARKET_FEE_BPS (default 100). Only the ones the
#   built contracts' constructors take are required.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; }

CMD="${1:-}"
[[ -n "${CMD}" ]] && shift
case "${CMD}" in keygen | plan | deploy | verify) ;; -h | --help | "") usage; exit 0 ;; *) die "unknown command '${CMD}'" ;; esac

VIA=""
RPC=""
OUT=""
PASS_ARGS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --via) VIA="${2:?--via needs a server name}"; shift ;;
    --rpc) RPC="${2:?--rpc needs a URL}"; shift ;;
    --out) OUT="${2:?--out needs a file}"; shift ;;
    --redeploy) PASS_ARGS+=(--redeploy "${2:?--redeploy needs a key}"); shift ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown argument '$1'" ;;
  esac
  shift
done

# Only public config is needed here; the cloud tokens are not.
DRY_RUN=1
load_config
DRY_RUN=0
validate_servers
DEPLOY_CHAIN_NAME="${DEPLOY_CHAIN_NAME:-sova-testnet}"
SOVA_CHAIN_ID="${SOVA_CHAIN_ID:-82330}"
DEPLOYER_DIR="${DEPLOYER_DIR:-${HOME}/.config/sova-testnet/deployer}"
OUT="${OUT:-${KIT_DIR}/deployments/${DEPLOY_CHAIN_NAME}.json}"
ENGINE="${REPO_ROOT}/contracts/script/deploy-kit.sh"
export ASHWINGS_TREASURY="${ASHWINGS_TREASURY:-}" ASHWINGS_ZEC_PAYEE="${ASHWINGS_ZEC_PAYEE:-}" \
  ASHWINGS_PRICE_WEI="${ASHWINGS_PRICE_WEI:-}" ASHWINGS_PRICE_ZAT="${ASHWINGS_PRICE_ZAT:-}" \
  MARKET_FEE_BPS="${MARKET_FEE_BPS:-100}"

cmd_keygen() {
  need_cmd cast "Foundry"
  need_cmd openssl
  local ks="${DEPLOYER_DIR}/keystore.json" pw="${DEPLOYER_DIR}/password" pass addr
  [[ ! -e "${ks}" && ! -e "${pw}" ]] || die "${DEPLOYER_DIR} already holds a deployer; it is reused (move it aside to make a new one)"
  (umask 077 && mkdir -p "${DEPLOYER_DIR}")
  chmod 700 "${DEPLOYER_DIR}"
  pass="$(openssl rand -hex 32)"
  (umask 077 && printf '%s' "${pass}" >"${pw}")
  # The password reaches cast in its environment, never on argv.
  CAST_PASSWORD="${pass}" cast wallet new "${DEPLOYER_DIR}" keystore.json >/dev/null
  unset pass
  chmod 600 "${ks}" "${pw}"
  addr="$(ETH_KEYSTORE="${ks}" ETH_PASSWORD="${pw}" cast wallet address)"
  printf '%s\n' "${addr}" >"${DEPLOYER_DIR}/address"
  log "deployer ${addr} (keystore ${ks}, password file ${pw}, both 0600)"
  log "fund it with a little SOVA (./deploy-contracts.sh plan prints the estimate); docs/ops/wallets.md"
}

TUNNEL_PID=""
close_tunnel() { [[ -z "${TUNNEL_PID}" ]] || kill "${TUNNEL_PID}" 2>/dev/null || true; }

open_tunnel() { # server
  local ip port="${DEPLOY_TUNNEL_PORT:-18545}" i
  ip="$(server_ip "$1")"
  set_ssh_opts
  log "SSH tunnel 127.0.0.1:${port} -> $1 127.0.0.1:${SOVA_HTTP_PORT}"
  ssh "${SSH_OPTS[@]}" -N -o ExitOnForwardFailure=yes \
    -L "127.0.0.1:${port}:127.0.0.1:${SOVA_HTTP_PORT}" "sova-admin@${ip}" &
  TUNNEL_PID=$!
  trap close_tunnel EXIT
  RPC="http://127.0.0.1:${port}"
  for i in 1 2 3 4 5 6 7 8 9 10; do
    cast chain-id --rpc-url "${RPC}" >/dev/null 2>&1 && return 0
    kill -0 "${TUNNEL_PID}" 2>/dev/null || die "SSH tunnel to $1 failed"
    sleep "${i}"
  done
  die "no RPC answer through the tunnel to $1"
}

run_engine() {
  [[ -n "${VIA}" && -n "${RPC}" ]] && die "use --via or --rpc, not both"
  if [[ -n "${VIA}" ]]; then
    open_tunnel "${VIA}"
  fi
  RPC="${RPC:-${DEPLOY_RPC:-}}"
  [[ -n "${RPC}" ]] || die "no RPC: pass --via <server> or --rpc URL (or set DEPLOY_RPC in config.env)"
  local key_args=()
  if [[ "${CMD}" != verify ]]; then
    key_args=(--keystore "${DEPLOYER_DIR}/keystore.json" --password-file "${DEPLOYER_DIR}/password")
    [[ -f "${DEPLOYER_DIR}/keystore.json" ]] || die "no deployer at ${DEPLOYER_DIR}: run ./deploy-contracts.sh keygen first"
  fi
  "${ENGINE}" "${CMD}" --rpc "${RPC}" --out "${OUT}" --expect-chain-id "${SOVA_CHAIN_ID}" \
    "${key_args[@]+"${key_args[@]}"}" "${PASS_ARGS[@]+"${PASS_ARGS[@]}"}"
}

case "${CMD}" in
  keygen) cmd_keygen ;;
  *) run_engine ;;
esac
