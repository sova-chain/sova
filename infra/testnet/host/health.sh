#!/usr/bin/env bash
# infra/testnet/host/health.sh -- one health pass on an M1 testnet host
# (sova-health.timer, every 2 min). Every finding goes to the journal
# (`journalctl -t sova-health`); ALERT lines also go to Telegram when
# /etc/sova/health.env sets TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID (and
# optionally TELEGRAM_THREAD_ID, a topic in a forum group). The
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
#   block age    the newest Sova block is older than BLOCK_AGE_ALERT_MIN
#                (default 10 min). A block's time is its Zcash block's, so
#                the alert says whether zebrad's tip is old too (Zcash is
#                slow: Sova waits for it) or not (Sova is stuck).
#   null run     SIP-6 on: the last NULL_RUN_ALERT blocks (default 20) are
#                all null. The head still advances, but nobody is burning,
#                so no transaction can be mined (2026-09-25: the keeper's
#                burn budget ran out; heights looked healthy for an hour).
#   C5           any "settlement mismatch" rejection in the last 3 minutes
#   faucet       not accepting drips, or hot wallet over its limit
#   checkout     the checkout relayer's /status not answering, its SOVA
#                below CHECKOUT_RELAYER_ALERT_BALANCE_WEI, or refusing new
#                orders (low balance, or its open-order cap)
set -uo pipefail

# shellcheck source=/dev/null
[[ -f /etc/sova/host.env ]] && source /etc/sova/host.env
DISK_ALERT_PCT="${DISK_ALERT_PCT:-80}"
ZEBRA_LAG_ALERT="${ZEBRA_LAG_ALERT:-20}"
EPOCH_LAG_ALERT="${EPOCH_LAG_ALERT:-10}"
BLOCK_AGE_ALERT_MIN="${BLOCK_AGE_ALERT_MIN:-10}"
NULL_RUN_ALERT="${NULL_RUN_ALERT:-20}"
SOVA_SIP6="${SOVA_SIP6:-1}"
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
    # A topic in a forum group, when set.
    local thread=()
    [[ -n "${TELEGRAM_THREAD_ID:-}" ]] && thread=(--data-urlencode "message_thread_id=${TELEGRAM_THREAD_ID}")
    # Token in a curl config on stdin, never on the command line.
    printf 'url = "https://api.telegram.org/bot%s/sendMessage"\n' "${TELEGRAM_BOT_TOKEN}" |
      curl -fsS --max-time 10 -K - \
        --data-urlencode "chat_id=${TELEGRAM_CHAT_ID}" \
        "${thread[@]}" \
        --data-urlencode "text=[sova ${HOST}] ${key}: $*" >/dev/null ||
      say "telegram send failed"
  fi
}

clear_alert() { rm -f "${STATE_DIR}/$1.last"; }

rpc() { # url method [params-json]
  curl -fsS --max-time 5 -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":${3:-[]}}" "$1"
}

# ---- block latency (docs/design/faster-blocks.md §4) -------------------------------
# The newest block's age. Its timestamp is its Zcash block's time (SIP-6
# pins it), so compare with zebrad's tip before blaming Sova.
check_block_age() { # head
  local ts age now limit=$((BLOCK_AGE_ALERT_MIN * 60)) zage=""
  ts="$(rpc "${SOVA_URL}" eth_getBlockByNumber '["latest",false]' 2>/dev/null | jq -r '.result.timestamp // empty' 2>/dev/null)" || ts=""
  [[ "${ts}" =~ ^0x[0-9a-fA-F]+$ ]] || return 0
  now="$(date +%s)"
  age=$((now - ts))
  say "newest block $1 is ${age} s old"
  if ((age <= limit)); then
    clear_alert block_age
    return 0
  fi
  [[ "${ztime}" =~ ^[0-9]+$ ]] && zage=$((now - ztime))
  if [[ -n "${zage}" ]] && ((zage > limit)); then
    alert block_age "newest Sova block $1 is $((age / 60)) min old (alert at ${BLOCK_AGE_ALERT_MIN} min); zebrad's tip ${ztip} is $((zage / 60)) min old too: Zcash is slow, Sova waits for it"
  elif [[ -n "${zage}" ]]; then
    alert block_age "SOVA STUCK: newest block $1 is $((age / 60)) min old (alert at ${BLOCK_AGE_ALERT_MIN} min) while zebrad's tip ${ztip} is ${zage} s old"
  else
    alert block_age "newest Sova block $1 is $((age / 60)) min old (alert at ${BLOCK_AGE_ALERT_MIN} min); zebrad's tip time unknown"
  fi
}

# SIP-6: a sealed block's extraData is 97 bytes (0x + 194 hex), a null
# block's is empty. No sealed block in the last NULL_RUN_ALERT means no
# burner, and no transaction can be mined, however healthy heights look.
check_null_run() { # head
  [[ "${SOVA_SIP6}" == 1 ]] || return 0
  local head="$1" n="${NULL_RUN_ALERT}" i x sealed=0 seen=0 keeper=""
  ((head >= n)) || return 0 # a young chain: genesis is exempt, too few blocks
  for ((i = 0; i < n; i++)); do
    x="$(rpc "${SOVA_URL}" eth_getBlockByNumber "[\"$(printf '0x%x' $((head - i)))\",false]" 2>/dev/null | jq -r '.result.extraData // empty' 2>/dev/null)" || x=""
    [[ -n "${x}" ]] || continue
    seen=$((seen + 1))
    ((${#x} == 196)) && sealed=$((sealed + 1))
  done
  ((seen == n)) || return 0 # the RPC hiccuped: don't guess
  say "sealed blocks: ${sealed} of the last ${n}"
  if ((sealed > 0)); then
    clear_alert null_run
    return 0
  fi
  if systemctl is-enabled --quiet sova-keeper 2>/dev/null; then
    keeper="; sova-keeper here is $(systemctl is-active sova-keeper 2>/dev/null) (its burn budget spent?)"
  fi
  alert null_run "the last ${n} blocks (to ${head}) are all null: nobody is burning, so no transaction can be mined${keeper}"
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
ztime=""
if [[ -z "${zinfo}" ]]; then
  alert zebrad_down "zebrad RPC not answering at ${ZEBRA_URL}"
else
  clear_alert zebrad_down
  ztip="$(jq -r '.result.blocks' <<<"${zinfo}")"
  # The tip's own time, to tell a slow Zcash from a stuck Sova (below).
  ztime="$(rpc "${ZEBRA_URL}" getblock "[\"${ztip}\",1]" 2>/dev/null | jq -r '.result.time // empty' 2>/dev/null)" || ztime=""
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
    check_block_age "${head}"
    check_null_run "${head}"
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

# ---- checkout relayer ---------------------------------------------------------
if systemctl is-enabled --quiet sova-checkout-relayer 2>/dev/null; then
  st="$(curl -fsS --max-time 10 "http://127.0.0.1:${CHECKOUT_RELAYER_PORT:-18791}/status" 2>/dev/null)" || st=""
  if [[ -z "${st}" ]]; then
    alert checkout_down "checkout relayer /status not answering"
  else
    clear_alert checkout_down
    floor="${CHECKOUT_RELAYER_ALERT_BALANCE_WEI:-1000000000000000000}"
    # jq compares as doubles: fine for a threshold (wei overflows bash).
    if [[ "$(jq -r --arg f "${floor}" '(.balanceWei | tonumber) < ($f | tonumber)' <<<"${st}")" == true ]]; then
      alert checkout_low "checkout relayer $(jq -r .relayer <<<"${st}") has $(jq -r .balanceSova <<<"${st}") SOVA (alert below $(jq -rn --arg f "${floor}" '$f | tonumber / 1e18'); new orders stop below $(jq -r '.minBalanceWei | tonumber / 1e18' <<<"${st}")): top it up"
    else
      clear_alert checkout_low
    fi
    if [[ "$(jq -r .accepting <<<"${st}")" != true ]]; then
      alert checkout_refusing "checkout relayer refuses new orders: $(jq -r .reason <<<"${st}") ($(jq -r .openReservations <<<"${st}")/$(jq -r .maxOpenReservations <<<"${st}") open)"
    else
      clear_alert checkout_refusing
    fi
  fi
fi
exit 0
