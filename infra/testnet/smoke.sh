#!/usr/bin/env bash
# infra/testnet/smoke.sh -- post-launch checks. Read-only everywhere.
#
#   ./smoke.sh edge        from THIS machine, as a stranger would: public
#                          RPC answers chain 82330 and a moving head; the
#                          denylist is enforced (admin/debug/trace/txpool/
#                          engine/personal/sign/filters); batch cap; faucet
#                          /status up and other faucet paths 404; seed P2P
#                          ports open; every private port (authrpc, RPC,
#                          zebrad RPC, faucet) closed on every host
#   ./smoke.sh hosts       over SSH: services active, "enforcing
#                          settlements" logged, zebrad synced, epoch lag,
#                          zero C5 rejections, no-keys check on public boxes
#   ./smoke.sh balance <0xEVM> [minutes]
#                          the stranger test's last step: poll the public
#                          RPC until <0xEVM> holds SOVA (a burn was minted)
#   ./smoke.sh all         edge + hosts
# ok/bad always return 0, so `test && ok || bad` is a safe if/else here.
# shellcheck disable=SC2015
set -uo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CMD="${1:-all}"
shift || true
load_config
validate_servers
need_cmd jq
need_cmd curl
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok    %s\n' "$*"; }
bad() { FAIL=$((FAIL + 1)); printf 'FAIL  %s\n' "$*"; }

pub_rpc() { # method [params-json]
  curl -fsS --max-time 15 -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" "https://${RPC_HOST}/"
}

port_open() { nc -z -w 5 "$1" "$2" >/dev/null 2>&1; }

cmd_edge() {
  local r id h1 h2 m
  r="$(pub_rpc eth_chainId)" && id="$(jq -r .result <<<"${r}")"
  [[ "${id:-}" == 0x1419a ]] && ok "rpc: eth_chainId = 82330" || bad "rpc: eth_chainId = '${id:-<no answer>}'"
  h1="$(pub_rpc eth_blockNumber | jq -r .result)"
  if [[ "${h1}" =~ ^0x ]]; then
    ok "rpc: head $((h1))"
    sleep 90
    h2="$(pub_rpc eth_blockNumber | jq -r .result)"
    [[ "${h2}" =~ ^0x ]] && (( h2 > h1 )) && ok "rpc: head advanced to $((h2)) in 90 s" ||
      bad "rpc: head did not advance in 90 s ($((h1)) -> ${h2}); epochs are ~75 s, retry once before worrying"
  else
    bad "rpc: eth_blockNumber gave '${h1}'"
  fi
  for m in admin_nodeInfo admin_peers debug_traceTransaction trace_block txpool_content \
    engine_forkchoiceUpdatedV3 personal_listAccounts eth_sign eth_sendTransaction \
    eth_newFilter eth_accounts miner_start ots_getApiLevel; do
    r="$(curl -sS --max-time 15 -H 'Content-Type: application/json' \
      --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${m}\",\"params\":[]}" "https://${RPC_HOST}/")"
    [[ "$(jq -r '.error.code // empty' <<<"${r}" 2>/dev/null)" == -32601 ]] && ok "rpc: ${m} denied" || bad "rpc: ${m} NOT denied: ${r:0:120}"
  done
  r="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 15 -H 'Content-Type: application/json' \
    --data "[$(for i in $(seq 1 11); do printf '%s{"jsonrpc":"2.0","id":%d,"method":"eth_blockNumber","params":[]}' "$([[ $i -gt 1 ]] && echo ,)" "$i"; done)]" \
    "https://${RPC_HOST}/")"
  [[ "${r}" == 400 ]] && ok "rpc: batch of 11 refused" || bad "rpc: batch of 11 gave HTTP ${r} (Worker missing?)"

  r="$(curl -fsS --max-time 15 "https://${FAUCET_HOST}/status")"
  if [[ "$(jq -r .network <<<"${r}" 2>/dev/null)" == test ]]; then
    ok "faucet: /status up (accepting_drips $(jq -r .accepting_drips <<<"${r}"), balance $(jq -r .balance_zat <<<"${r}") zat)"
  else
    bad "faucet: /status gave '${r:0:120}'"
  fi
  r="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 15 "https://${FAUCET_HOST}/admin")"
  [[ "${r}" == 404 ]] && ok "faucet: other paths 404" || bad "faucet: /admin gave HTTP ${r}"

  local s name role ip port
  for s in "${SERVERS[@]}"; do
    name="$(srv_name "${s}")"
    role="$(srv_role "${s}")"
    ip="$(server_ip "${name}")"
    if [[ "${role}" == seed ]]; then
      for port in "${SOVA_P2P_PORT}" "${ZEBRA_P2P_PORT}"; do
        port_open "${ip}" "${port}" && ok "${name}: tcp/${port} open (P2P)" || bad "${name}: tcp/${port} closed"
      done
    else
      port_open "${ip}" "${SOVA_P2P_PORT}" && bad "${name}: tcp/${SOVA_P2P_PORT} open on a no-inbound host" || ok "${name}: no inbound P2P"
    fi
    for port in "${SOVA_AUTH_PORT}" "${SOVA_HTTP_PORT}" "${ZEBRA_RPC_PORT}" "${FAUCET_PORT}"; do
      port_open "${ip}" "${port}" && bad "${name}: tcp/${port} is reachable from the internet" || ok "${name}: tcp/${port} closed"
    done
  done
}

cmd_hosts() {
  local s name role out
  for s in "${SERVERS[@]}"; do
    name="$(srv_name "${s}")"
    role="$(srv_role "${s}")"
    # shellcheck disable=SC2016 # expanded on the host
    out="$(kit_ssh "${name}" '
      for u in zebrad sova-node sova-faucet cloudflared sova-health.timer; do
        systemctl is-enabled --quiet $u 2>/dev/null && echo "svc $u $(systemctl is-active $u)"
      done
      sudo journalctl -u sova-node --no-pager -q 2>/dev/null | grep -q "expectations: enforcing settlements" && echo "c5 enforcing"
      echo "c5rejects $(sudo journalctl -u sova-node --since -1h --no-pager -q 2>/dev/null | grep -c "settlement mismatch")"
      sudo /usr/local/lib/sova-infra/health.sh 2>/dev/null | sed "s/^/health /"
    ' 2>&1)" || { bad "${name}: ssh failed"; continue; }
    while read -r kind a b rest; do
      case "${kind}" in
        svc) [[ "${b}" == active ]] && ok "${name}: ${a} active" || bad "${name}: ${a} is ${b}" ;;
        c5) ok "${name}: C5 enforcing against its own zebrad" ;;
        c5rejects) [[ "${a}" == 0 ]] && ok "${name}: 0 C5 rejections in the last hour" || bad "${name}: ${a} C5 rejections in the last hour" ;;
        health) [[ "${a}" == ALERT ]] && bad "${name}: ${a} ${b} ${rest}" || ok "${name}: ${a} ${b} ${rest}" ;;
      esac
    done <<<"${out}"
    if [[ "${role}" == seed || "${role}" == rpc || "${role}" == keeper ]] && ! grep -q '^c5 ' <<<"${out}"; then
      bad "${name}: no 'expectations: enforcing settlements' line (sova-node not started, or no zebrad)"
    fi
  done
}

cmd_balance() {
  local addr="${1:?usage: smoke.sh balance <0xEVM> [minutes]}" mins="${2:-30}" deadline bal
  [[ "${addr}" =~ ^0x[0-9a-fA-F]{40}$ ]] || die "not an EVM address: ${addr}"
  deadline=$(($(date +%s) + mins * 60))
  while (($(date +%s) < deadline)); do
    bal="$(pub_rpc eth_getBalance "[\"${addr}\",\"latest\"]" | jq -r .result)"
    if [[ "${bal}" =~ ^0x ]] && [[ "${bal}" != 0x0 ]]; then
      ok "mint: ${addr} holds ${bal} wei of SOVA (via https://${RPC_HOST})"
      return 0
    fi
    sleep 30
  done
  bad "mint: ${addr} still has no SOVA after ${mins} min"
}

case "${CMD}" in
  edge) cmd_edge ;;
  hosts) cmd_hosts ;;
  balance) cmd_balance "$@" ;;
  all) cmd_edge; cmd_hosts ;;
  *) die "unknown command '${CMD}'" ;;
esac
echo "${PASS} passed, ${FAIL} failed"
[[ "${FAIL}" == 0 ]]
