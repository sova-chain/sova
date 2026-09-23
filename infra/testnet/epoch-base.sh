#!/usr/bin/env bash
# infra/testnet/epoch-base.sh -- choose, pin and record the testnet's epoch
# base B: the first Zcash testnet height that is a Sova epoch (SIP-2: Sova
# block N settles epoch N + B - 1). B is consensus: every node in the
# network must run with the same SOVA_EPOCH_BASE.
#
#   ./epoch-base.sh propose [--rpc URL | --via <server>]
#       Read the Zcash testnet tip and propose B = the next multiple of
#       ROUND (default 100) at least MIN_AHEAD blocks (default 48, ~1 h)
#       past the tip: a round, announceable number, in the future, so
#       every node (ours and early strangers') can be up before epoch B
#       exists. Burn-less epochs before the first burn are filled by
#       rewardless cadence blocks from any mine-mode node (the keeper).
#   ./epoch-base.sh pin <B>
#       Write SOVA_EPOCH_BASE=<B> into config.env. Then ./deploy.sh rolls it
#       out and starts the sova nodes, ./bootnodes.sh writes it into
#       testnet.env and seeds.json.
#   ./epoch-base.sh record [--rpc URL | --via <server>]
#       Once block B exists: write out/epoch-base.json with B's hash and
#       time, for the announcement and the repo (the base is then
#       checkable against any Zcash testnet explorer).
#
# Reads only public chain data from a zebrad you run (default: the laptop
# testnet zebrad at http://127.0.0.1:18234, or a provisioned host via SSH).
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CMD="${1:-}"
[[ -n "${CMD}" ]] && shift
RPC="${ZEBRA_RPC_URL:-http://127.0.0.1:18234}"
VIA=""
PIN=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --rpc) RPC="${2:?}"; shift ;;
    --via) VIA="${2:?}"; shift ;;
    [0-9]*) PIN="$1" ;;
    *) die "unknown argument '$1'" ;;
  esac
  shift
done
ROUND="${ROUND:-100}"
MIN_AHEAD="${MIN_AHEAD:-48}"
load_config
need_cmd jq

zrpc() { # method params-json
  local data="{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}"
  if [[ -n "${VIA}" ]]; then
    kit_ssh "${VIA}" "curl -fsS --max-time 10 -H 'Content-Type: application/json' --data '${data}' http://127.0.0.1:${ZEBRA_RPC_PORT}"
  else
    curl -fsS --max-time 10 -H 'Content-Type: application/json' --data "${data}" "${RPC}"
  fi
}

check_testnet() {
  local info chain
  info="$(zrpc getblockchaininfo)" || die "zebrad RPC not reachable"
  chain="$(jq -r '.result.chain' <<<"${info}")"
  [[ "${chain}" == test ]] || die "that zebrad is on '${chain}', not Zcash testnet"
  TIP="$(jq -r '.result.blocks' <<<"${info}")"
  EST="$(jq -r '.result.estimatedheight // .result.blocks' <<<"${info}")"
  ((EST - TIP <= 5)) || die "that zebrad is not synced (${TIP} of ~${EST}); B must be chosen from the real tip"
}

case "${CMD}" in
  propose)
    check_testnet
    b=$(((TIP + MIN_AHEAD + ROUND - 1) / ROUND * ROUND))
    mins=$(((b - TIP) * 75 / 60))
    log "Zcash testnet tip ${TIP}"
    echo "proposed B = ${b}  (${b} - ${TIP} = $((b - TIP)) blocks, ~${mins} min at 75 s/block)"
    echo "pin it with: ./epoch-base.sh pin ${b}"
    ;;
  pin)
    [[ "${PIN}" =~ ^[1-9][0-9]*$ ]] || die "usage: epoch-base.sh pin <B>"
    cfg="${SOVA_TESTNET_CONFIG:-${KIT_DIR}/config.env}"
    grep -q '^SOVA_EPOCH_BASE=' "${cfg}" || die "no SOVA_EPOCH_BASE= line in ${cfg}"
    old="$(sed -n 's/^SOVA_EPOCH_BASE="\{0,1\}\([0-9]*\)"\{0,1\}.*/\1/p' "${cfg}")"
    if [[ -n "${old}" && "${old}" != "${PIN}" ]]; then
      warn "changing a pinned B (${old} -> ${PIN}) forks every node that already runs ${old}: only at launch or a reset"
    fi
    tmp="$(mktemp)"
    sed "s/^SOVA_EPOCH_BASE=.*/SOVA_EPOCH_BASE=\"${PIN}\"/" "${cfg}" >"${tmp}" && mv "${tmp}" "${cfg}"
    log "config.env: SOVA_EPOCH_BASE=\"${PIN}\". Next: ./deploy.sh && ./bootnodes.sh"
    ;;
  record)
    [[ -n "${SOVA_EPOCH_BASE}" ]] || die "SOVA_EPOCH_BASE is not pinned"
    check_testnet
    ((TIP >= SOVA_EPOCH_BASE)) || die "block ${SOVA_EPOCH_BASE} does not exist yet (tip ${TIP}, ~$(((SOVA_EPOCH_BASE - TIP) * 75 / 60)) min)"
    blk="$(zrpc getblock "[\"${SOVA_EPOCH_BASE}\", 1]")"
    hash="$(jq -r '.result.hash' <<<"${blk}")"
    time="$(jq -r '.result.time' <<<"${blk}")"
    mkdir -p "${OUT_DIR}"
    jq -n --argjson b "${SOVA_EPOCH_BASE}" --arg h "${hash}" --argjson t "${time}" --argjson tip "${TIP}" \
      '{zcash_network:"testnet", epoch_base:$b, block_hash:$h, block_time:$t, confirmations_at_record:($tip - $b + 1)}' \
      >"${OUT_DIR}/epoch-base.json"
    cat "${OUT_DIR}/epoch-base.json"
    log "cross-check ${hash} at ${SOVA_EPOCH_BASE} on a Zcash testnet explorer, then publish it with the announcement"
    ;;
  *) sed -n '2,27p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
