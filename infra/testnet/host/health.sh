#!/usr/bin/env bash
# infra/testnet/host/health.sh -- one health pass on an M1 testnet host
# (sova-health.timer, every 2 min). Every finding goes to the journal
# (`journalctl -t sova-health`); ALERT lines also go to Telegram when
# /etc/sova/health.env sets TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID. The
# same alert is re-sent at most once an hour.
#
# Checks (infra-m1 §2 "Monitoring and alerting"):
#   disk         / and /var/lib/sova >= DISK_ALERT_PCT (default 80)
#   zebrad lag   estimatedheight - blocks > ZEBRA_LAG_ALERT (default 20)
#   epoch lag    (zebrad tip - B + 1) - sova head > EPOCH_LAG_ALERT
#                (default 10 epochs, ~12 min). Split into "WE LAG" (a
#                reference node is ahead of us: our problem) and "NETWORK
#                STALLED" (the reference is stuck too: nobody is sealing,
#                a miner matter, not an infra failure).
#   C5           any "settlement mismatch" rejection in the last 3 minutes
#   faucet       not accepting drips, or hot wallet over its limit
set -uo pipefail

# shellcheck source=/dev/null
[[ -f /etc/sova/host.env ]] && source /etc/sova/host.env
DISK_ALERT_PCT="${DISK_ALERT_PCT:-80}"
ZEBRA_LAG_ALERT="${ZEBRA_LAG_ALERT:-20}"
EPOCH_LAG_ALERT="${EPOCH_LAG_ALERT:-10}"
# Another node's public RPC to tell "we lag" from "network stalled"
# (e.g. https://rpc.testnet.sova.io on a seed host). Empty: can't tell.
HEALTH_REFERENCE_RPC="${HEALTH_REFERENCE_RPC:-}"
STATE_DIR=/var/lib/sova-health
mkdir -p "${STATE_DIR}"
HOST="$(hostname)"

say() { logger -t sova-health -- "$*"; echo "$*"; }

alert() {
  local key="$1"
  shift
  say "ALERT ${key}: $*"
  local stamp="${STATE_DIR}/${key}.last" now
  now="$(date +%s)"
  if [[ -f "${stamp}" ]] && ((now - $(cat "${stamp}") < 3600)); then
    return 0
  fi
  echo "${now}" >"${stamp}"
  if [[ -n "${TELEGRAM_BOT_TOKEN:-}" && -n "${TELEGRAM_CHAT_ID:-}" ]]; then
    # Token in a curl config on stdin, never on the command line.
    printf 'url = "https://api.telegram.org/bot%s/sendMessage"\n' "${TELEGRAM_BOT_TOKEN}" |
      curl -fsS --max-time 10 -K - \
        --data-urlencode "chat_id=${TELEGRAM_CHAT_ID}" \
        --data-urlencode "text=[sova ${HOST}] ${key}: $*" >/dev/null ||
      say "telegram send failed"
  fi
}

clear_alert() { rm -f "${STATE_DIR}/$1.last"; }

rpc() { # url method [params-json]
  curl -fsS --max-time 5 -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":${3:-[]}}" "$1"
}

# ---- disk -------------------------------------------------------------------
for mount in / /var/lib/sova; do
  [[ -d "${mount}" ]] || continue
  pct="$(df --output=pcent "${mount}" | tail -1 | tr -dc '0-9')"
  key="disk$(tr '/' '_' <<<"${mount}")"
  if [[ -n "${pct}" && "${pct}" -ge "${DISK_ALERT_PCT}" ]]; then
    alert "${key}" "${mount} is ${pct}% full (alert at ${DISK_ALERT_PCT}%)"
  else
    clear_alert "${key}"
  fi
done

# ---- zebrad -------------------------------------------------------------------
ZEBRA_URL="http://127.0.0.1:${ZEBRA_RPC_PORT:-18232}"
zinfo="$(rpc "${ZEBRA_URL}" getblockchaininfo 2>/dev/null)" || zinfo=""
ztip=""
if [[ -z "${zinfo}" ]]; then
  alert zebrad_down "zebrad RPC not answering at ${ZEBRA_URL}"
else
  clear_alert zebrad_down
  ztip="$(jq -r '.result.blocks' <<<"${zinfo}")"
  zest="$(jq -r '.result.estimatedheight // empty' <<<"${zinfo}")"
  if [[ -n "${zest}" ]] && ((zest - ztip > ZEBRA_LAG_ALERT)); then
    alert zebrad_lag "zebrad at ${ztip}, network ~${zest} ($((zest - ztip)) behind)"
  else
    clear_alert zebrad_lag
  fi
fi

# ---- sova node ------------------------------------------------------------------
if systemctl is-enabled --quiet sova-node 2>/dev/null; then
  SOVA_URL="http://127.0.0.1:${SOVA_HTTP_PORT:-8545}"
  if ! systemctl is-active --quiet sova-node; then
    alert sova_down "sova-node is not running"
  elif ! head_hex="$(rpc "${SOVA_URL}" eth_blockNumber 2>/dev/null | jq -r '.result // empty')" || [[ -z "${head_hex}" ]]; then
    alert sova_down "sova-node RPC not answering at ${SOVA_URL}"
  else
    clear_alert sova_down
    head=$((head_hex))
    if [[ -n "${ztip}" && -n "${SOVA_EPOCH_BASE:-}" ]] && ((ztip >= SOVA_EPOCH_BASE)); then
      lag=$(((ztip - SOVA_EPOCH_BASE + 1) - head))
      say "epoch lag ${lag} (zebrad ${ztip}, base ${SOVA_EPOCH_BASE}, sova head ${head})"
      if ((lag > EPOCH_LAG_ALERT)); then
        ref=""
        if [[ -n "${HEALTH_REFERENCE_RPC}" ]]; then
          ref_hex="$(rpc "${HEALTH_REFERENCE_RPC}" eth_blockNumber 2>/dev/null | jq -r '.result // empty')" || ref_hex=""
          [[ -n "${ref_hex}" ]] && ref=$((ref_hex))
        fi
        if [[ -n "${ref}" ]] && ((ref > head + 2)); then
          alert epoch_lag "WE LAG (infra): head ${head}, reference ${ref}, epoch lag ${lag}"
        elif [[ -n "${ref}" ]]; then
          alert epoch_lag "NETWORK STALLED (miner matter, not infra): head ${head} = reference ${ref}, epoch lag ${lag}; nobody is sealing"
        else
          alert epoch_lag "epoch lag ${lag} (head ${head}); no reference RPC to tell our lag from a network stall"
        fi
      else
        clear_alert epoch_lag
      fi
    fi
  fi
  rejects="$(journalctl -u sova-node --since '-3min' --no-pager -q 2>/dev/null | grep -c 'settlement mismatch' || true)"
  if [[ "${rejects}" -gt 0 ]]; then
    alert c5_reject "${rejects} C5 settlement-mismatch rejection(s) in the last 3 min"
  else
    clear_alert c5_reject
  fi
fi

# ---- faucet -------------------------------------------------------------------
if systemctl is-enabled --quiet sova-faucet 2>/dev/null; then
  st="$(curl -fsS --max-time 5 "http://127.0.0.1:${FAUCET_PORT:-18790}/status" 2>/dev/null)" || st=""
  if [[ -z "${st}" ]]; then
    alert faucet_down "faucet /status not answering"
  else
    clear_alert faucet_down
    if [[ "$(jq -r '.accepting_drips' <<<"${st}")" != "true" ]]; then
      alert faucet_dry "faucet not accepting drips (balance $(jq -r '.balance_zat // "?"' <<<"${st}") zat, unshielded coinbase $(jq -r '.coinbase_unshielded_zat // "?"' <<<"${st}") zat)"
    else
      clear_alert faucet_dry
    fi
    if [[ "$(jq -r '.over_max_balance' <<<"${st}")" == "true" ]]; then
      alert faucet_over "HOT WALLET OVER LIMIT: stop topping it up"
    else
      clear_alert faucet_over
    fi
  fi
fi
exit 0
