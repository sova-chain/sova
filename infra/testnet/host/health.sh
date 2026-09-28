#!/usr/bin/env bash
# infra/testnet/host/health.sh -- one health pass on an M1 testnet host
# (sova-health.timer, every 2 min). Every finding goes to the journal
# (`journalctl -t sova-health`); ALERT lines also go to Telegram when
# /etc/sova/health.env sets TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID (and
# optionally TELEGRAM_THREAD_ID, a topic in a forum group). The
# same alert is re-sent at most once an hour.
#
# Findings about the whole network (a stall, all-null blocks, an old
# newest block) look the same from every host, so only the hosts with
# HEALTH_NETWORK_ALERTS=1 send them: two, so one dead host can't silence
# them (deploy.sh: HEALTH_NETWORK_ALERT_HOSTS, by default the rpc host and
# the first seed; 2026-09-27 the rpc host, then the only sender, ran out of
# memory and no alert went out). The once-an-hour dedupe is per host, so a
# network finding reaches Telegram up to twice an hour, once from each: an
# accepted cost. Unset (a host.env from an older kit): 1 on the rpc host
# and on a seed whose hostname ends in -seed-1. The other hosts only log
# them. Findings about the host itself (memory, disk, zebrad, sova-node,
# "we lag") come from every host.
#
# Checks (infra-m1 §2 "Monitoring and alerting"):
#   memory       MemAvailable < MEM_ALERT_MB (default 300) on 2 passes in a
#                row (~2 min): alert mem_low with the top 3 processes by
#                RSS. Checked first, so it still goes out while zebrad or
#                sova-node have stopped answering.
#   disk         / and /var/lib/sova >= DISK_ALERT_PCT (default 80)
#   zebrad lag   estimatedheight - blocks > ZEBRA_LAG_ALERT (default 20)
#   epoch lag    (zebrad tip - B + 1) - sova head > EPOCH_LAG_ALERT
#                (default 10 epochs) for EPOCH_LAG_PERSIST_MIN (default
#                10) minutes in a row: Zcash testnet mines in bursts and
#                Sova is a dozen blocks behind for a few minutes after
#                one, which is not a fault. Split into "WE LAG" (a
#                reference node is ahead of us: our problem) and "NETWORK
#                BEHIND ZCASH" (the reference is behind too: the network
#                isn't keeping up, a network alert).
#   block age    the newest Sova block is older than BLOCK_AGE_ALERT_MIN
#                (default 10 min). A block's time is its Zcash block's, so
#                when zebrad's tip is old too, Zcash is slow and Sova is
#                waiting for it: that is only logged. Otherwise Sova is
#                stuck: an alert.
#   null run     SIP-6 on: no sealed block for more than NULL_SEALED_MAX_MIN
#                (default 45) minutes. The head still advances on null
#                blocks, but nobody is burning, so no transaction can be
#                mined (2026-09-25: the keeper's burn budget ran out;
#                heights looked healthy for an hour). Time, not a count of
#                null blocks: a demand-mode keeper burns only for pending
#                transactions plus a heartbeat (KEEPER_HEARTBEAT_SECS, 30
#                min), and Zcash bursts of 3 s blocks make long null runs
#                that are no fault. NULL_RUN_ALERT (the old block count) is
#                ignored.
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
EPOCH_LAG_PERSIST_MIN="${EPOCH_LAG_PERSIST_MIN:-10}"
if [[ -z "${HEALTH_NETWORK_ALERTS:-}" ]]; then
  HEALTH_NETWORK_ALERTS=0
  [[ "${ROLE:-}" == rpc ]] && HEALTH_NETWORK_ALERTS=1
  [[ "${ROLE:-}" == seed && "$(hostname)" == *-seed-1 ]] && HEALTH_NETWORK_ALERTS=1
fi
MEM_ALERT_MB="${MEM_ALERT_MB:-300}"
BLOCK_AGE_ALERT_MIN="${BLOCK_AGE_ALERT_MIN:-10}"
NULL_SEALED_MAX_MIN="${NULL_SEALED_MAX_MIN:-45}"
[[ "${NULL_SEALED_MAX_MIN}" =~ ^[1-9][0-9]*$ ]] || NULL_SEALED_MAX_MIN=45
# How far back the null-run check walks at most (blocks: 60 min of 2 s
# blocks; it stops sooner, once the walk spans NULL_SEALED_MAX_MIN), and how
# many blocks one batched RPC call reads.
NULL_SEALED_WALK=1800
NULL_SEALED_BATCH=50
SOVA_SIP6="${SOVA_SIP6:-1}"
# Another node's public RPC to tell "we lag" from "network stalled"
# (e.g. https://rpc.testnet.sova.io on a seed host). Empty: can't tell.
HEALTH_REFERENCE_RPC="${HEALTH_REFERENCE_RPC:-}"
STATE_DIR=/var/lib/sova-health
mkdir -p "${STATE_DIR}"
HOST="$(hostname)"

say() { logger -t sova-health -- "$*"; echo "$*"; }

# A host.env from an older kit sets NULL_RUN_ALERT (a count of null blocks,
# replaced by NULL_SEALED_MAX_MIN): accepted, ignored, noted.
[[ -n "${NULL_RUN_ALERT:-}" ]] &&
  say "NULL_RUN_ALERT=${NULL_RUN_ALERT} is deprecated and ignored: the null-run alert is time-based (NULL_SEALED_MAX_MIN=${NULL_SEALED_MAX_MIN} min); re-run deploy.sh to drop it"

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

clear_alert() { rm -f "${STATE_DIR}/$1.last" "${STATE_DIR}/$1.since" "${STATE_DIR}/$1.passes"; }

# A finding about the whole network: sent from two hosts only (see the top).
net_alert() {
  if [[ "${HEALTH_NETWORK_ALERTS}" == 1 ]]; then
    alert "$@"
  else
    local key="$1"
    shift
    say "network ${key} (sent by the hosts with HEALTH_NETWORK_ALERTS=1): $*"
  fi
}

# persisted <key> <minutes>: true once the condition behind <key> has held
# on every pass for <minutes>. clear_alert <key> resets it.
persisted() {
  local since="${STATE_DIR}/$1.since" now
  now="$(date +%s)"
  [[ -f "${since}" ]] || echo "${now}" >"${since}"
  ((now - $(cat "${since}") >= $2 * 60))
}

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
    clear_alert block_age
    say "newest Sova block $1 is $((age / 60)) min old; zebrad's tip ${ztip} is $((zage / 60)) min old too: Zcash is slow, Sova waits for it"
  elif [[ -n "${zage}" ]]; then
    net_alert block_age "SOVA STUCK: newest block $1 is $((age / 60)) min old (alert at ${BLOCK_AGE_ALERT_MIN} min) while zebrad's tip ${ztip} is ${zage} s old"
  else
    net_alert block_age "newest Sova block $1 is $((age / 60)) min old (alert at ${BLOCK_AGE_ALERT_MIN} min); zebrad's tip time unknown"
  fi
}

# last_sealed <url> <head> <limit-secs>: walk back from <head> (at most
# NULL_SEALED_WALK blocks, NULL_SEALED_BATCH per batched call, never to
# genesis, which is exempt) to the newest sealed block, stopping early at a
# null block more than <limit-secs> older than the head (the verdict is
# known then). SIP-6: a sealed
# block's extraData is 97 bytes (0x + 194 hex), a null block's is empty.
# Sets LS_HEAD_TS (the head's timestamp), LS_NUM and LS_TS (the sealed
# block; empty when none was found), LS_OLDEST and LS_OLDEST_TS (the oldest
# block read). Returns 1 unless every block it asked for came
# back readable: an RPC hiccup is not a finding.
last_sealed() {
  local url="$1" head="$2" limit="$3" lo hi b i hex req rows want num x ts
  LS_HEAD_TS="" LS_NUM="" LS_TS="" LS_OLDEST="" LS_OLDEST_TS=""
  lo=$((head - NULL_SEALED_WALK + 1))
  ((lo < 1)) && lo=1
  for ((hi = head; hi >= lo; hi = b - 1)); do
    b=$((hi - NULL_SEALED_BATCH + 1))
    ((b < lo)) && b=${lo}
    req=""
    for ((i = hi; i >= b; i--)); do
      printf -v hex '0x%x' "${i}"
      req+="${req:+,}{\"jsonrpc\":\"2.0\",\"id\":${i},\"method\":\"eth_getBlockByNumber\",\"params\":[\"${hex}\",false]}"
    done
    rows="$(curl -fsS --max-time 10 -H 'Content-Type: application/json' --data "[${req}]" "${url}" 2>/dev/null |
      jq -r 'sort_by(-.id)[] | "\(.id) \(.result.extraData // "-") \(.result.timestamp // "-")"' 2>/dev/null)" || return 1
    want=${hi}
    while read -r num x ts; do
      [[ "${num}" == "${want}" && "${x}" =~ ^0x[0-9a-fA-F]*$ && "${ts}" =~ ^0x[0-9a-fA-F]+$ ]] || return 1
      want=$((want - 1))
      ((num == head)) && LS_HEAD_TS=$((ts))
      if ((${#x} == 196)); then
        LS_NUM=${num} LS_TS=$((ts))
        return 0
      fi
      LS_OLDEST=${num} LS_OLDEST_TS=$((ts))
    done <<<"${rows}"
    ((want == b - 1)) || return 1 # fewer answers than asked
    ((LS_HEAD_TS - LS_OLDEST_TS > limit)) && return 0
  done
  return 0
}

# No sealed block for more than NULL_SEALED_MAX_MIN minutes means no burner,
# and no transaction can be mined, however healthy heights look. A block's
# time is its Zcash block's, so the gap is also measured from the head's
# time: when no Sova block at all has come for a while (Zcash slow, or Sova
# stuck), that is block_age's finding, not this one.
check_null_run() { # head
  [[ "${SOVA_SIP6}" == 1 ]] || return 0
  local head="$1" limit=$((NULL_SEALED_MAX_MIN * 60)) now age gap what keeper=""
  ((head >= 1)) || return 0 # genesis only: exempt, nothing to judge
  if ! last_sealed "${SOVA_URL}" "${head}" "${limit}"; then
    say "null run: could not read the last blocks to ${head}; not judged this pass"
    return 0
  fi
  now="$(date +%s)"
  if [[ -n "${LS_NUM}" || "${LS_OLDEST}" == 1 ]]; then
    if [[ -n "${LS_NUM}" ]]; then
      age=$((now - LS_TS))
      gap=$((LS_HEAD_TS - LS_TS))
      say "newest sealed block ${LS_NUM} is $((age / 60)) min old (head ${head}; alert over ${NULL_SEALED_MAX_MIN} min)"
      what="no sealed block for $((age / 60)) min (alert over ${NULL_SEALED_MAX_MIN} min): the newest is ${LS_NUM}, and $((head - LS_NUM)) null blocks follow it to ${head}"
    else # a young chain, null since genesis: timed from block 1
      age=$((now - LS_OLDEST_TS))
      gap=$((LS_HEAD_TS - LS_OLDEST_TS))
      say "no sealed block since genesis: blocks 1..${head} are null, block 1 is $((age / 60)) min old"
      what="no sealed block since genesis: all ${head} blocks are null, the first $((age / 60)) min old (alert over ${NULL_SEALED_MAX_MIN} min)"
    fi
    if ((age <= limit)); then
      clear_alert null_run
      return 0
    fi
    if ((gap <= limit)); then
      say "null run: no sealed block for $((age / 60)) min, but no Sova block at all for $(((now - LS_HEAD_TS) / 60)) min: a slow Zcash or a stuck Sova (block_age), not a missing burner"
      return 0
    fi
  elif ((LS_HEAD_TS - LS_OLDEST_TS > limit)); then
    # Null blocks spanning more than the limit, back from the head.
    say "no sealed block in blocks ${LS_OLDEST}..${head} (back to $(((now - LS_OLDEST_TS) / 60)) min ago)"
    what="no sealed block for over ${NULL_SEALED_MAX_MIN} min: blocks ${LS_OLDEST}..${head} are all null, back to $(((now - LS_OLDEST_TS) / 60)) min ago"
  else
    # The walk's cap, inside the limit: a Zcash burst faster than 2 s a
    # block. Judged on a later pass, once the null run spans the limit.
    say "null run: no sealed block in the last ${NULL_SEALED_WALK} blocks, but they span only $(((LS_HEAD_TS - LS_OLDEST_TS) / 60)) min; not judged this pass"
    return 0
  fi
  if systemctl is-enabled --quiet sova-keeper 2>/dev/null; then
    keeper="; sova-keeper here is $(systemctl is-active sova-keeper 2>/dev/null) (its burn budget spent?)"
  fi
  net_alert null_run "${what}: nobody is burning, so no transaction can be mined${keeper}"
}

# The keeper host's own burner (a host finding, from that host). It stops
# by design when its per-run budget is spent; either way blocks go null.
check_keeper() {
  systemctl cat sova-keeper >/dev/null 2>&1 || return 0
  if systemctl is-active --quiet sova-keeper; then
    clear_alert keeper_down
  else
    alert keeper_down "sova-keeper is $(systemctl is-active sova-keeper 2>/dev/null): nothing here is burning (budget spent? journalctl -u sova-keeper)"
  fi
}

# ---- memory -----------------------------------------------------------------
# MemAvailable (what can be had without swapping) under MEM_ALERT_MB on two
# passes in a row: one low pass is often a burst (a RocksDB compaction, a
# build). 2026-09-27: reth's state cache filled the 4 GB hosts; zebrad
# stopped answering and the public RPC host thrashed, with no alert.
check_memory() {
  local avail_kb avail_mb passes f="${STATE_DIR}/mem_low.passes" top swap
  avail_kb="$(awk '/^MemAvailable:/ { print $2 }' /proc/meminfo 2>/dev/null)"
  [[ "${avail_kb}" =~ ^[0-9]+$ ]] || return 0
  avail_mb=$((avail_kb / 1024))
  if ((avail_mb >= MEM_ALERT_MB)); then
    [[ -f "${f}" ]] && say "memory recovered: ${avail_mb} MB available"
    clear_alert mem_low
    return 0
  fi
  passes="$(cat "${f}" 2>/dev/null)"
  [[ "${passes}" =~ ^[0-9]+$ ]] || passes=0
  passes=$((passes + 1))
  echo "${passes}" >"${f}"
  swap="$(awk '/^SwapTotal:/ { t = $2 } /^SwapFree:/ { u = t - $2 } END { printf "%d of %d MB", u / 1024, t / 1024 }' /proc/meminfo)"
  if ((passes < 2)); then
    say "memory low: ${avail_mb} MB available (alert under ${MEM_ALERT_MB} MB on 2 passes in a row); swap used ${swap}"
    return 0
  fi
  # awk takes the first 3 and reads to EOF: no SIGPIPE to ps.
  top="$(ps -eo rss=,comm= --sort=-rss 2>/dev/null |
    awk 'NR <= 3 { printf "%s%s %d MB", (NR > 1 ? ", " : ""), $2, $1 / 1024 }')"
  alert mem_low "only ${avail_mb} MB available (alert under ${MEM_ALERT_MB} MB, ${passes} passes in a row); swap used ${swap}; top RSS: ${top:-?}"
}
check_memory
check_keeper

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
      if ((lag > EPOCH_LAG_ALERT)) && ! persisted epoch_lag "${EPOCH_LAG_PERSIST_MIN}"; then
        say "epoch lag ${lag} over ${EPOCH_LAG_ALERT}, under ${EPOCH_LAG_PERSIST_MIN} min so far (a Zcash burst catches up in minutes)"
      elif ((lag > EPOCH_LAG_ALERT)); then
        ref=""
        if [[ -n "${HEALTH_REFERENCE_RPC}" ]]; then
          ref_hex="$(rpc "${HEALTH_REFERENCE_RPC}" eth_blockNumber 2>/dev/null | jq -r '.result // empty')" || ref_hex=""
          [[ -n "${ref_hex}" ]] && ref=$((ref_hex))
        fi
        if [[ -n "${ref}" ]] && ((ref > head + 2)); then
          alert epoch_lag "WE LAG (infra): head ${head}, reference ${ref}, epoch lag ${lag}"
        elif [[ -n "${ref}" ]]; then
          net_alert epoch_lag "NETWORK BEHIND ZCASH for ${EPOCH_LAG_PERSIST_MIN}+ min: head ${head}, reference ${ref}, epoch lag ${lag}"
        else
          net_alert epoch_lag "epoch lag ${lag} (head ${head}); no reference RPC to tell our lag from a network stall"
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
