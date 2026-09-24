#!/usr/bin/env bash
# infra/testnet/bootnodes.sh -- the final bootnode list after provisioning,
# and the files a stranger joins with.
#
#   ./bootnodes.sh [--verify]
#
# Reads the seed enodes deploy.sh recorded (out/servers/<seed>.enode, made
# from each seed's persistent node key), adds EXTRA_BOOTNODES from
# config.env (community seeds), and writes:
#   out/bootnodes.txt   one enode per line
#   out/testnet.env     the env a stranger's node sources (SOVA_CHAIN,
#                       SOVA_EPOCH_BASE, SOVA_EMISSION_SCHEDULE,
#                       SOVA_SIP6, SOVA_SIP7, SOVA_BOOTNODES, ...)
#   out/seeds.json      machine-readable: chain ID, genesis hash, epoch
#                       base, schedule, sip6/sip7, release, bootnodes
#                       (+ DNS names), RPC/faucet/download URLs
# and prints the Rust constant to paste into bin/sova/src/chain.rs
# (SOVA_TESTNET_BOOTNODES), so the next release has them compiled in.
#
# The genesis hash is not hard-coded: it is what every node host's own
# installed binary printed at deploy (`sova genesis-hash` with that host's
# SOVA_CHAIN / SOVA_SIP7; out/servers/<name>.genesis_hash). SIP-7's
# ZcashBlocks predeploy is in the genesis state, so the hash moves with
# SOVA_SIP7 and with any genesis change in chain.rs. Every node host must
# have recorded one, and they must all agree.
#
# --verify: SSH to every node host and check that the enode the running
# node logged ("p2p: sova/1 gossip enabled; local enode ...") has the same
# node id as the recorded one, that its block 0 (its own loopback RPC) is
# the published genesis hash, and that seeds answer on the P2P TCP port.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

VERIFY=0
[[ "${1:-}" == --verify ]] && VERIFY=1
load_config
validate_servers
validate_sip6
need_cmd jq
EXTRA_BOOTNODES="${EXTRA_BOOTNODES:-}"
CHAIN_ID=82330

# The genesis hash, from the node hosts' own `sova genesis-hash` (see the
# header): all recorded, all equal.
GENESIS_HASH=""
GENESIS_FROM=""
for s in "${SERVERS[@]}"; do
  name="$(srv_name "${s}")"
  case "$(srv_role "${s}")" in seed | rpc | keeper) ;; *) continue ;; esac
  f="${OUT_DIR}/servers/${name}.genesis_hash"
  [[ -s "${f}" ]] || die "no genesis hash recorded for ${name} (out/servers/${name}.genesis_hash): re-run ./deploy.sh with a release whose sova has 'genesis-hash'"
  h="$(cat "${f}")"
  [[ "${h}" =~ ^0x[0-9a-f]{64}$ ]] || die "${name}: malformed genesis hash '${h}'"
  [[ "$(cat "${OUT_DIR}/servers/${name}.genesis_sip7" 2>/dev/null)" == "${SOVA_SIP7}" ]] ||
    die "${name} was deployed with a different SOVA_SIP7 than config.env's ${SOVA_SIP7}: re-run ./deploy.sh"
  if [[ -z "${GENESIS_HASH}" ]]; then
    GENESIS_HASH="${h}"
    GENESIS_FROM="${name}"
  elif [[ "${h}" != "${GENESIS_HASH}" ]]; then
    die "genesis hash disagreement: ${name} boots ${h}, ${GENESIS_FROM} boots ${GENESIS_HASH} (different release or SOVA_SIP7? re-run ./deploy.sh on every host)"
  fi
done
[[ -n "${GENESIS_HASH}" ]] || die "no node host in SERVERS: nothing to take the genesis hash from"
log "genesis hash ${GENESIS_HASH} (SIP-7 $([[ "${SOVA_SIP7}" == 1 ]] && echo on || echo off); printed by every node host's sova)"

enodes=()
dns=()
while read -r name; do
  [[ -n "${name}" ]] || continue
  f="${OUT_DIR}/servers/${name}.enode"
  [[ -s "${f}" ]] || die "no enode for seed ${name}; run ./deploy.sh first"
  e="$(cat "${f}")"
  [[ "${e}" =~ ^enode://[0-9a-f]{128}@[0-9.]+:[0-9]+$ ]] || die "${name}: malformed enode '${e}'"
  enodes+=("${e}")
  d=""
  [[ -s "${OUT_DIR}/servers/${name}.dns" ]] && d="$(cat "${OUT_DIR}/servers/${name}.dns")"
  dns+=("${d}")
done < <(servers_with_role seed)
for e in ${EXTRA_BOOTNODES//,/ }; do
  enodes+=("${e}")
  dns+=("")
done
[[ ${#enodes[@]} -gt 0 ]] || die "no bootnodes"

printf '%s\n' "${enodes[@]}" >"${OUT_DIR}/bootnodes.txt"
joined="$(IFS=,; echo "${enodes[*]}")"

[[ -n "${SOVA_EPOCH_BASE}" ]] || warn "SOVA_EPOCH_BASE is not pinned yet: testnet.env / seeds.json carry an empty base (not publishable)"

if [[ "${SOVA_SIP6}" == 1 ]]; then
  mining_note="# Mining (SIP-6 sealing): set SOVA_SEALER_KEYSTORE to your sova-miner's
# keystore.json. Its key signs your blocks and holds your SOVA (keep it
# 0600), and its EVM address must be the one your burns credit. Keep
# SOVA_DATADIR: the seal journal under it must survive restarts."
else
  mining_note="# Mining: set SOVA_MINER_EVM_ADDRESS to your sova-miner's \"evm address\"."
fi

cat >"${OUT_DIR}/testnet.env" <<EOF
# Sova public testnet (M1). Source this in the environment of YOUR node
# (bin/sova ${SOVA_RELEASE_TAG}), next to your own zebrad on Zcash testnet.
# Every value above the line must match the network's; the ones below are
# yours. Published at https://${DL_HOST}/testnet.env and in the repo.
# Genesis hash: ${GENESIS_HASH}
# (\`sova genesis-hash\` with this env sourced prints it; SOVA_SIP7 is part
# of the genesis, so a node with another value is on another chain).
SOVA_CHAIN=sova-testnet
SOVA_GOSSIP=p2p
SOVA_EPOCH_BASE=${SOVA_EPOCH_BASE}
SOVA_EMISSION_SCHEDULE=${SOVA_EMISSION_SCHEDULE}
SOVA_SIP6=${SOVA_SIP6}
SOVA_SIP7=${SOVA_SIP7}
SOVA_BOOTNODES=${joined}
# ---- yours ----
SOVA_ZEBRAD_RPC=http://127.0.0.1:${ZEBRA_RPC_PORT}
SOVA_DATADIR=\$HOME/.sova-testnet/node
${mining_note}
# Follow only: SOVA_FOLLOW_ONLY=1 instead.
EOF

bn_json="$(for i in "${!enodes[@]}"; do jq -nc --arg e "${enodes[$i]}" --arg d "${dns[$i]}" '{enode:$e} + (if $d == "" then {} else {dns:$d} end)'; done | jq -sc .)"
jq -n --argjson chain_id "${CHAIN_ID}" --arg genesis "${GENESIS_HASH}" \
  --arg base "${SOVA_EPOCH_BASE}" --arg sched "${SOVA_EMISSION_SCHEDULE}" \
  --argjson sip6 "$([[ "${SOVA_SIP6}" == 1 ]] && echo true || echo false)" \
  --argjson sip7 "$([[ "${SOVA_SIP7}" == 1 ]] && echo true || echo false)" \
  --arg rel "${SOVA_RELEASE_TAG}" --arg repo "${SOVA_RELEASE_REPO}" \
  --argjson bootnodes "${bn_json}" --arg rpc "https://${RPC_HOST}" \
  --arg faucet "https://${FAUCET_HOST}" --arg dl "https://${DL_HOST}" \
  --arg zebra "${ZEBRA_IMAGE}" \
  '{network:"sova-testnet", chain_id:$chain_id, genesis_hash:$genesis,
    epoch_base:(if $base == "" then null else ($base|tonumber) end),
    zcash_network:"testnet", emission_schedule:$sched, sip6:$sip6, sip7:$sip7,
    release:{tag:$rel, repo:$repo}, zebra_image:$zebra,
    bootnodes:$bootnodes,
    courtesy:{note:"Project-run conveniences, never load-bearing. Any peer works as a bootnode.",
              rpc:$rpc, faucet:$faucet, downloads:$dl}}' >"${OUT_DIR}/seeds.json"

log "wrote ${OUT_DIR}/bootnodes.txt, testnet.env, seeds.json (${#enodes[@]} bootnode(s))"
echo
echo "// bin/sova/src/chain.rs -- compiled-in defaults for the next release:"
echo "pub(crate) const SOVA_TESTNET_BOOTNODES: &[&str] = &["
for e in "${enodes[@]}"; do echo "    \"${e}\","; done
echo "];"

if [[ "${VERIFY}" == 1 ]]; then
  echo
  log "verifying against the running nodes"
  fail=0
  for s in "${SERVERS[@]}"; do
    name="$(srv_name "${s}")"
    role="$(srv_role "${s}")"
    [[ "${role}" == seed || "${role}" == rpc || "${role}" == keeper ]] || continue
    rec="$(cat "${OUT_DIR}/servers/${name}.enode" 2>/dev/null || true)"
    logged="$(kit_ssh "${name}" "sudo journalctl -u sova-node --no-pager -q | grep -o 'local enode enode://[0-9a-f]*@[^ ]*' | tail -1" | sed 's/^local enode //' || true)"
    if [[ -z "${logged}" ]]; then
      warn "${name}: node has not logged its enode yet (started? epoch base pinned?)"
      fail=1
    elif [[ "${logged%%@*}" == "${rec%%@*}" ]]; then
      log "${name}: node id matches (${rec%%@*})"
    else
      warn "${name}: running node id ${logged%%@*} != recorded ${rec%%@*}"
      fail=1
    fi
    # Block 0 of the running node (its loopback RPC) is the published hash.
    g0="$(kit_ssh "${name}" "curl -fsS --max-time 10 -H 'Content-Type: application/json' \
      --data '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getBlockByNumber\",\"params\":[\"0x0\",false]}' \
      http://127.0.0.1:${SOVA_HTTP_PORT}" 2>/dev/null | jq -r '.result.hash // empty' 2>/dev/null || true)"
    if [[ -z "${g0}" ]]; then
      warn "${name}: node RPC did not answer block 0 (started?)"
      fail=1
    elif [[ "${g0}" == "${GENESIS_HASH}" ]]; then
      log "${name}: block 0 is the published genesis ${GENESIS_HASH}"
    else
      warn "${name}: running node's block 0 is ${g0}, not the published ${GENESIS_HASH}"
      fail=1
    fi
    if [[ "${role}" == seed ]]; then
      ip="$(server_ip "${name}")"
      if nc -z -w 5 "${ip}" "${SOVA_P2P_PORT}" 2>/dev/null; then
        log "${name}: P2P tcp/${SOVA_P2P_PORT} reachable from here"
      else
        warn "${name}: P2P tcp/${SOVA_P2P_PORT} NOT reachable from here"
        fail=1
      fi
    fi
  done
  [[ "${fail}" == 0 ]] || die "verification failed"
  log "bootnodes verified"
fi
