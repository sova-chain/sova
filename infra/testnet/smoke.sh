#!/usr/bin/env bash
# infra/testnet/smoke.sh -- post-launch checks. Read-only everywhere.
#
#   ./smoke.sh edge        from THIS machine, as a stranger would: public
#                          RPC answers chain 82330 and a moving head; the
#                          latest block is SIP-6 sealed (97-byte extraData)
#                          or null (empty) when SOVA_SIP6=1; block 0 is
#                          the published genesis hash (seeds.json); with
#                          SOVA_SIP7=1, ZcashBlocks.latest() at the head
#                          is its anchored Zcash height (head + B - 1) and
#                          sova_getZcashBlocks serves that height with the
#                          same hash; the
#                          denylist is enforced (admin/debug/trace/txpool/
#                          engine/personal/sign/filters); batch cap; the
#                          per-IP limit answers a burst with a 429 a
#                          browser can read (CORS + Retry-After); faucet
#                          /status up and other faucet paths 404; with
#                          CHECKOUT_RELAYER=1, the checkout relayer's
#                          /status through CHECKOUT_HOST (chain, listings,
#                          accepting, balance above
#                          CHECKOUT_RELAYER_ALERT_BALANCE_WEI, the recorded
#                          address, CORS for the page only, no secrets),
#                          its routing (a bad /reserve is a 400 from the
#                          relayer, spending nothing) and its other paths
#                          404; seed P2P ports open; P2P closed on every
#                          non-seed host (incl. byo ones); every private
#                          port (authrpc, RPC, zebrad RPC, faucet, checkout
#                          relayer) closed on every host
#   ./smoke.sh hosts       over SSH: services active, "enforcing
#                          settlements" logged, SIP-7 feed logged (with
#                          SOVA_SIP7=1), zebrad synced, epoch lag,
#                          zero C5 rejections, no-keys check on public boxes
#   ./smoke.sh balance <0xEVM> [minutes]
#                          the stranger test's last step: poll the public
#                          RPC until <0xEVM> holds SOVA (a burn was minted)
#   ./smoke.sh contracts   the day-one contracts answer through the public
#                          RPC (deploy-contracts.sh verify: code matches the
#                          build, getters return the recorded values)
#   ./smoke.sh all         edge + hosts (+ contracts once recorded)
# ok/bad always return 0, so `test && ok || bad` is a safe if/else here.
# shellcheck disable=SC2015
set -uo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CMD="${1:-all}"
shift || true
load_config
validate_servers
validate_edge_config
validate_sip6
validate_checkout_config
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
    # A null block (empty extraData, SIP-6) carries no transactions; only a
    # sealed block (a burn's sealer) does. A head that advances on null
    # blocks alone looks live but accepts nothing (2026-09-25: the keeper's
    # per-run burn budget ran out and ~70 null blocks in a row followed).
    if [[ "${SOVA_SIP6}" == 1 && "${h2}" =~ ^0x ]]; then
      local i x sealed=0
      for ((i = 0; i < 20; i++)); do
        x="$(pub_rpc eth_getBlockByNumber "[\"$(printf '0x%x' $((h2 - i)))\",false]" | jq -r '.result.extraData // ""')"
        ((${#x} == 196)) && sealed=$((sealed + 1))
      done
      ((sealed > 0)) && ok "rpc: ${sealed} of the last 20 blocks sealed (can carry transactions)" ||
        bad "rpc: the last 20 blocks are all null: no one is burning (is sova-keeper running?), so no transaction can be mined"
    fi
  else
    bad "rpc: eth_blockNumber gave '${h1}'"
  fi
  check_sip6_latest
  check_genesis
  check_sip7_latest
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
  # Per-IP limit: twice the limit, in parallel, from this machine's IP.
  # Counters are eventually consistent, so one readable 429 is the bar.
  # eth_chainId is answered at the edge: the burst never reaches rpc-1.
  # (%header{} in -w needs curl 7.84+.)
  local burst limited
  if [[ "${RPC_RATELIMIT_AT}" == waf ]]; then
    ok "rpc: rate limit is the WAF rule (RPC_RATELIMIT_AT=waf; its 429 has no CORS), burst not checked"
  else
    burst=$((RPC_RATELIMIT_REQUESTS * 2))
    # One command, run here and (on a miss) on a seed over SSH.
    local burst_cmd
    burst_cmd="seq 1 ${burst} | xargs -P 16 -I{} curl -sS -o /dev/null --max-time 15 \
      -H 'Content-Type: application/json' -H 'Origin: https://sova.io' \
      -w '%{http_code} %header{access-control-allow-origin} %header{retry-after}\n' \
      --data '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_chainId\",\"params\":[]}' 'https://${RPC_HOST}/' 2>/dev/null |
      grep -c '^429 \* [0-9]' || true"
    limited="$(bash -c "${burst_cmd}")"
    # The binding's counters are per Cloudflare location and best effort. In
    # a large location (seen 2026-09-24: SIN) a one-IP burst spreads over
    # enough machines that no counter trips, while the same burst from a
    # Hetzner box (PRG) gets 429s. So a miss from here is retried from the
    # first seed before it counts as a failure.
    local seed where="from here"
    seed="$(servers_with_role seed | head -1)"
    if [[ "${limited}" -eq 0 && -n "${seed}" ]]; then
      limited="$(kit_ssh "${seed}" "${burst_cmd}" 2>/dev/null || true)"
      limited="${limited:-0}"
      where="from ${seed} (none from here: this Cloudflare location's counters didn't trip)"
    fi
    if [[ "${limited}" -gt 0 ]]; then
      ok "rpc: burst of ${burst} ${where}: ${limited} x 429 with CORS + Retry-After"
    else
      bad "rpc: burst of ${burst} gave no 429 with CORS + Retry-After, from here or a seed (RPC_RATELIMIT binding missing? the Worker logs a warning)"
    fi
    sleep $((RPC_RATELIMIT_PERIOD + 1)) # let this IP's window pass
  fi

  r="$(curl -fsS --max-time 15 "https://${FAUCET_HOST}/status")"
  if [[ "$(jq -r .network <<<"${r}" 2>/dev/null)" == test ]]; then
    ok "faucet: /status up (accepting_drips $(jq -r .accepting_drips <<<"${r}"), balance $(jq -r .balance_zat <<<"${r}") zat)"
  else
    bad "faucet: /status gave '${r:0:120}'"
  fi
  r="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 15 "https://${FAUCET_HOST}/admin")"
  [[ "${r}" == 404 ]] && ok "faucet: other paths 404" || bad "faucet: /admin gave HTTP ${r}"
  check_checkout

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
      # Covers byo hosts too: their security group is the operator's, so
      # this is the only check that it matches the kit's no-inbound rule.
      for port in "${SOVA_P2P_PORT}" "${ZEBRA_P2P_PORT}"; do
        port_open "${ip}" "${port}" && bad "${name}: tcp/${port} open on a no-inbound host" || ok "${name}: tcp/${port} closed (no inbound P2P)"
      done
    fi
    for port in "${SOVA_AUTH_PORT}" "${SOVA_HTTP_PORT}" "${ZEBRA_RPC_PORT}" "${FAUCET_PORT}" \
      $([[ "${CHECKOUT_RELAYER}" == 1 ]] && echo "${CHECKOUT_RELAYER_PORT}"); do
      port_open "${ip}" "${port}" && bad "${name}: tcp/${port} is reachable from the internet" || ok "${name}: tcp/${port} closed"
    done
  done
}

# The checkout relayer, as the page and a stranger see it (CHECKOUT_HOST).
# Spends nothing: /status is read-only and the one POST is refused by the
# relayer's own validation before any limit or transaction.
check_checkout() {
  if [[ "${CHECKOUT_RELAYER}" != 1 ]]; then
    ok "checkout: relayer off (CHECKOUT_RELAYER=0)"
    return 0
  fi
  local url="https://${CHECKOUT_HOST}" hdr r code want="" faucet
  hdr="$(mktemp)"
  r="$(curl -fsS --max-time 20 -D "${hdr}" -H "Origin: ${CHECKOUT_RELAYER_CORS_ORIGIN}" "${url}/status")" || r=""
  if [[ "$(jq -r '.ok' <<<"${r}" 2>/dev/null)" != true ]]; then
    bad "checkout: ${url}/status gave '${r:0:160}'"
    rm -f "${hdr}"
    return 0
  fi
  local addr bal chain listings accepting
  addr="$(jq -r .relayer <<<"${r}")"
  bal="$(jq -r .balanceSova <<<"${r}")"
  chain="$(jq -r .chainId <<<"${r}")"
  listings="$(jq -r '.listings | if type == "array" then join(",") else . end' <<<"${r}")"
  accepting="$(jq -r .accepting <<<"${r}")"
  ok "checkout: /status up (relayer ${addr}, ${bal} SOVA, $(jq -r .openReservations <<<"${r}")/$(jq -r .maxOpenReservations <<<"${r}") open, watcher $(jq -r .watching <<<"${r}"))"
  [[ "${chain}" == 82330 ]] && ok "checkout: chain 82330" || bad "checkout: relayer is on chain ${chain}"
  [[ "${listings}" == "${CHECKOUT_RELAYER_LISTINGS}" ]] && ok "checkout: serves listing(s) ${listings}" ||
    bad "checkout: serves listings '${listings}', config says ${CHECKOUT_RELAYER_LISTINGS}"
  faucet="$(servers_with_role faucet | sed -n 1p)"
  [[ -s "${OUT_DIR}/servers/${faucet}.checkout_relayer_address" ]] && want="$(cat "${OUT_DIR}/servers/${faucet}.checkout_relayer_address")"
  if [[ -z "${want}" ]]; then
    ok "checkout: no recorded address to compare (out/servers/${faucet}.checkout_relayer_address; deploy.sh writes it)"
  elif [[ "$(tr 'A-F' 'a-f' <<<"${want}")" == "$(tr 'A-F' 'a-f' <<<"${addr}")" ]]; then
    ok "checkout: relayer address = the one deploy.sh recorded"
  else
    bad "checkout: relayer is ${addr}, deploy.sh recorded ${want}"
  fi
  if [[ "$(jq -r --arg f "${CHECKOUT_RELAYER_ALERT_BALANCE_WEI}" '(.balanceWei | tonumber) >= ($f | tonumber)' <<<"${r}")" == true ]]; then
    ok "checkout: balance ${bal} SOVA >= the alert floor $(jq -rn --arg f "${CHECKOUT_RELAYER_ALERT_BALANCE_WEI}" '$f | tonumber / 1e18')"
  else
    bad "checkout: balance ${bal} SOVA is below the alert floor $(jq -rn --arg f "${CHECKOUT_RELAYER_ALERT_BALANCE_WEI}" '$f | tonumber / 1e18') (it refuses new orders below $(jq -r '.minBalanceWei | tonumber / 1e18' <<<"${r}")): fund ${addr}"
  fi
  [[ "${accepting}" == true ]] && ok "checkout: accepting new orders" || bad "checkout: refusing new orders ($(jq -r .reason <<<"${r}"))"
  grep -qi "^access-control-allow-origin: ${CHECKOUT_RELAYER_CORS_ORIGIN}"$'\r'"\?$" "${hdr}" &&
    ok "checkout: CORS grants ${CHECKOUT_RELAYER_CORS_ORIGIN}" || bad "checkout: no CORS grant for ${CHECKOUT_RELAYER_CORS_ORIGIN}"
  code="$(curl -sS -o /dev/null -D - --max-time 15 -H 'Origin: https://example.com' "${url}/status" | grep -ci '^access-control-allow-origin' || true)"
  [[ "${code}" == 0 ]] && ok "checkout: no CORS grant for other origins" || bad "checkout: CORS granted to https://example.com"
  # No key or other 32-byte secret in what it serves (it serves no hashes at all).
  grep -Eqi '[0-9a-f]{64}' <<<"${r}" && bad "checkout: /status contains a 64-hex string" || ok "checkout: /status carries no 32-byte hex (no key)"
  r="$(curl -sS --max-time 15 -H 'Content-Type: application/json' -H "Origin: ${CHECKOUT_RELAYER_CORS_ORIGIN}" \
    --data '{"listingId":1,"recipient":"0x0000000000000000000000000000000000000000"}' -w '\n%{http_code}' "${url}/reserve")"
  [[ "${r##*$'\n'}" == 400 && "$(jq -r '.error // empty' <<<"${r%$'\n'*}" 2>/dev/null)" == *recipient* ]] &&
    ok "checkout: /reserve routed to the relayer (bad recipient: 400, nothing sent)" || bad "checkout: /reserve with a zero recipient gave '${r:0:160}'"
  for code in /health /admin /; do
    r="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 15 "${url}${code}")"
    [[ "${r}" == 404 ]] && ok "checkout: ${code} is 404 at the edge" || bad "checkout: ${code} gave HTTP ${r}"
  done
  rm -f "${hdr}"
}

# SIP-6 §2.1: a sealed block's extraData is exactly 97 bytes (vanity +
# signature), a null block's is empty; genesis is exempt.
check_sip6_latest() {
  if [[ "${SOVA_SIP6}" != 1 ]]; then
    ok "rpc: SIP-6 off (SOVA_SIP6=0), extraData not checked"
    return 0
  fi
  local r n x len
  r="$(pub_rpc eth_getBlockByNumber '["latest",false]')"
  n="$(jq -r '.result.number // empty' <<<"${r}" 2>/dev/null)"
  x="$(jq -r '.result.extraData // empty' <<<"${r}" 2>/dev/null)"
  if ! [[ "${n}" =~ ^0x[0-9a-f]+$ && "${x}" =~ ^0x[0-9a-fA-F]*$ ]]; then
    bad "rpc: latest block unreadable: ${r:0:120}"
    return 0
  fi
  len=$(((${#x} - 2) / 2))
  if ((n == 0)); then
    bad "rpc: head is still genesis (exempt from SIP-6): nothing sealed yet"
  elif ((len == 97)); then
    ok "rpc: block $((n)) is SIP-6 sealed (97-byte extraData)"
  elif ((len == 0)); then
    ok "rpc: block $((n)) is a SIP-6 null block (empty extraData)"
  else
    bad "rpc: block $((n)) has ${len}-byte extraData: neither sealed (97) nor null (0); is SOVA_SIP6=1 on every node?"
  fi
}

# Block 0 through the public RPC is the genesis hash the kit published
# (out/seeds.json, else the copy on the download host): bootnodes.sh took
# it from the nodes' own `sova genesis-hash`, never from a constant.
check_genesis() {
  local want="" src g0
  if [[ -s "${OUT_DIR}/seeds.json" ]]; then
    want="$(jq -r '.genesis_hash // empty' "${OUT_DIR}/seeds.json" 2>/dev/null)"
    src="out/seeds.json"
  else
    want="$(curl -fsS --max-time 15 "https://${DL_HOST}/seeds.json" | jq -r '.genesis_hash // empty' 2>/dev/null)"
    src="https://${DL_HOST}/seeds.json"
  fi
  if ! [[ "${want}" =~ ^0x[0-9a-f]{64}$ ]]; then
    bad "rpc: no published genesis hash to compare with (out/seeds.json or ${DL_HOST}; run ./bootnodes.sh)"
    return 0
  fi
  g0="$(pub_rpc eth_getBlockByNumber '["0x0",false]' | jq -r '.result.hash // empty' 2>/dev/null)"
  [[ "${g0}" == "${want}" ]] && ok "rpc: block 0 is the published genesis ${want} (${src})" ||
    bad "rpc: block 0 is '${g0:-<no answer>}', ${src} says ${want}"
}

# SIP-7 §4.1/§4.2 through the public RPC, at one head N: the ZcashBlocks
# predeploy's latest() (0x...5A01, selector 0x52bfe789, returns (uint64
# height, bytes32 hash)) is Zcash height E_N = N + B - 1, the one block N
# anchors (the pre-block call records it), with block N's anchor
# (parentBeaconBlockRoot) as its hash; and the feed,
# sova_getZcashBlocks(E_N, E_N), serves that height with the same hash,
# anchored by block N.
ZCASH_BLOCKS=0x0000000000000000000000000000000000005a01
check_sip7_latest() {
  if [[ "${SOVA_SIP7}" != 1 ]]; then
    ok "rpc: SIP-7 off (SOVA_SIP7=0), ZcashBlocks and sova_getZcashBlocks not checked"
    return 0
  fi
  local n r data height hash want item root
  n="$(pub_rpc eth_blockNumber | jq -r '.result // empty' 2>/dev/null)"
  if ! [[ "${n}" =~ ^0x[0-9a-f]+$ ]] || ((n == 0)); then
    bad "rpc: no head past genesis to check SIP-7 at (eth_blockNumber '${n}')"
    return 0
  fi
  r="$(pub_rpc eth_call "[{\"to\":\"${ZCASH_BLOCKS}\",\"data\":\"0x52bfe789\"},\"${n}\"]")"
  data="$(jq -r '.result // empty' <<<"${r}" 2>/dev/null | tr 'A-F' 'a-f')"
  if ! [[ "${data}" =~ ^0x[0-9a-f]{128}$ ]]; then
    bad "rpc: ZcashBlocks.latest() at block $((n)) gave '${r:0:160}' (predeploy missing? SOVA_SIP7=1 on every node?)"
    return 0
  fi
  # uint64 in the low 8 bytes of word 0; word 1 is the hash.
  height=$((16#${data:50:16}))
  hash="0x${data:66:64}"
  if [[ -n "${SOVA_EPOCH_BASE}" ]]; then
    want=$((n + SOVA_EPOCH_BASE - 1))
    ((height == want)) && ok "rpc: ZcashBlocks.latest() at block $((n)) = Zcash height ${height} (N + B - 1)" ||
      bad "rpc: ZcashBlocks.latest() at block $((n)) = height ${height}, want ${want} (N + B - 1, B = ${SOVA_EPOCH_BASE})"
  else
    bad "rpc: SOVA_EPOCH_BASE is not pinned: cannot check ZcashBlocks.latest() (height ${height}) against N + B - 1"
  fi
  # The recorded hash is block N's own SIP-4 anchor.
  root="$(pub_rpc eth_getBlockByNumber "[\"${n}\",false]" | jq -r '.result.parentBeaconBlockRoot // empty' 2>/dev/null | tr 'A-F' 'a-f')"
  [[ "${hash}" == "${root}" && "${hash}" != "0x$(printf '%064d' 0)" ]] &&
    ok "rpc: ZcashBlocks.latest() hash is block $((n))'s anchor (parentBeaconBlockRoot)" ||
    bad "rpc: ZcashBlocks.latest() at block $((n)) has hash ${hash}, the block's anchor is '${root}'"
  r="$(pub_rpc sova_getZcashBlocks "[${height},${height}]")"
  item="$(jq -c '.result[0] // empty' <<<"${r}" 2>/dev/null)"
  if [[ -z "${item}" ]]; then
    bad "rpc: sova_getZcashBlocks(${height}, ${height}) gave '${r:0:160}'"
  elif [[ "$(jq -r '.height' <<<"${item}")" == "${height}" &&
    "$(jq -r '.hash | ascii_downcase' <<<"${item}")" == "${hash}" &&
    "$(jq -r '.sovaBlock' <<<"${item}")" == "$((n))" ]]; then
    ok "rpc: sova_getZcashBlocks serves height ${height} (hash ${hash:0:18}..., Sova block $((n))), matching ZcashBlocks"
  else
    bad "rpc: sova_getZcashBlocks(${height}) = ${item:0:200}; ZcashBlocks says hash ${hash} at Sova block $((n))"
  fi
}

# The keeper's node logs "sip-6: sealing as 0x..." at start: that must be
# the keeper's published EVM address (what its burns credit).
check_keeper_sealer() { # name logged-address
  local want=""
  [[ -s "${OUT_DIR}/servers/$1.keeper_evm" ]] && want="$(tr 'A-F' 'a-f' <"${OUT_DIR}/servers/$1.keeper_evm")"
  if [[ -z "$2" ]]; then
    bad "$1: no 'sip-6: sealing as' line (sova-node not started in SIP-6 mine mode?)"
  elif [[ -n "${want}" && "$2" != "${want}" ]]; then
    bad "$1: seals as $2, but the keeper's EVM address is ${want}"
  else
    ok "$1: SIP-6 sealing as $2${want:+ (= out/servers/$1.keeper_evm)}"
  fi
}

cmd_hosts() {
  local s name role out
  for s in "${SERVERS[@]}"; do
    name="$(srv_name "${s}")"
    role="$(srv_role "${s}")"
    # shellcheck disable=SC2016 # expanded on the host
    out="$(kit_ssh "${name}" '
      for u in zebrad sova-node sova-faucet sova-checkout-relayer cloudflared sova-health.timer; do
        systemctl is-enabled --quiet $u 2>/dev/null && echo "svc $u $(systemctl is-active $u)"
      done
      # The keeper burner is started by hand (not enabled) and stops itself at
      # its per-run budget; while it is stopped every block is null.
      [ -f /etc/systemd/system/sova-keeper.service ] && echo "svc sova-keeper $(systemctl is-active sova-keeper)"
      sudo journalctl -u sova-node --no-pager -q 2>/dev/null | grep -q "expectations: enforcing settlements" && echo "c5 enforcing"
      echo "c5rejects $(sudo journalctl -u sova-node --since -1h --no-pager -q 2>/dev/null | grep -c "settlement mismatch")"
      echo "sip7feed $(sudo journalctl -u sova-node --no-pager -q 2>/dev/null | grep -c "sip-7 feed: sova_getZcashBlocks")"
      echo "sealer $(sudo journalctl -u sova-node --no-pager -q 2>/dev/null | grep -o "sip-6: sealing as 0x[0-9a-f]*" | tail -1 | awk "{print \$NF}")"
      if systemctl is-enabled --quiet sova-node 2>/dev/null; then
        echo "p2p $(sudo grep -c "^SOVA_P2P_PEERS=enode" /etc/sova/sova-node.env 2>/dev/null) $(sudo journalctl -u sova-node --since -5min --no-pager -q 2>/dev/null | grep -o "connected_peers=[0-9]*" | tail -1 | cut -d= -f2)"
      fi
      sudo /usr/local/lib/sova-infra/health.sh 2>/dev/null | sed "s/^/health /"
    ' 2>&1)" || { bad "${name}: ssh failed"; continue; }
    while read -r kind a b rest; do
      case "${kind}" in
        svc) [[ "${b}" == active ]] && ok "${name}: ${a} active" || bad "${name}: ${a} is ${b}" ;;
        c5) ok "${name}: C5 enforcing against its own zebrad" ;;
        c5rejects) [[ "${a}" == 0 ]] && ok "${name}: 0 C5 rejections in the last hour" || bad "${name}: ${a} C5 rejections in the last hour" ;;
        sip7feed)
          if [[ "${role}" == seed || "${role}" == rpc || "${role}" == keeper ]] && [[ "${SOVA_SIP7}" == 1 ]]; then
            [[ "${a:-0}" -gt 0 ]] && ok "${name}: SIP-7 on (logged 'sip-7 feed: sova_getZcashBlocks')" ||
              bad "${name}: no 'sip-7 feed' line: sova-node not started with SOVA_SIP7=1?"
          fi
          ;;
        sealer) [[ "${role}" == keeper && "${SOVA_SIP6}" == 1 ]] && check_keeper_sealer "${name}" "${a:-}" ;;
        p2p)
          # Private hosts can't be dialled (firewall: SSH only), so they
          # must dial the seeds themselves: without SOVA_P2P_PEERS a seed
          # restart leaves them isolated for good (2026-09-25: the keeper
          # sealed alone for ~20 min after a snapshot restarted the seed).
          if [[ "${role}" != seed ]]; then
            [[ "${a:-0}" -ge 1 ]] && ok "${name}: static-peers the seeds (SOVA_P2P_PEERS)" ||
              bad "${name}: no SOVA_P2P_PEERS: it won't re-dial a restarted seed (re-run deploy.sh)"
          fi
          [[ "${b:-0}" -gt 0 ]] && ok "${name}: ${b} P2P peer(s) connected" ||
            bad "${name}: 0 P2P peers in its last status line (isolated: blocks don't flow)"
          ;;
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

cmd_contracts() {
  # verify makes a few dozen calls; the public RPC's WAF limit (50 per 10 s
  # per IP) can refuse some of them, so one retry after the window passes.
  if "${KIT_DIR}/deploy-contracts.sh" verify --rpc "https://${RPC_HOST}" ||
    { sleep 11 && "${KIT_DIR}/deploy-contracts.sh" verify --rpc "https://${RPC_HOST}"; }; then
    ok "contracts: every recorded day-one contract verified via https://${RPC_HOST}"
  else
    bad "contracts: deploy-contracts.sh verify failed (output above)"
  fi
}

case "${CMD}" in
  edge) cmd_edge ;;
  contracts) cmd_contracts ;;
  hosts) cmd_hosts ;;
  balance) cmd_balance "$@" ;;
  all)
    cmd_edge
    cmd_hosts
    [[ ! -f "${KIT_DIR}/deployments/${DEPLOY_CHAIN_NAME:-sova-testnet}.json" ]] || cmd_contracts
    ;;
  *) die "unknown command '${CMD}'" ;;
esac
echo "${PASS} passed, ${FAIL} failed"
[[ "${FAIL}" == 0 ]]
