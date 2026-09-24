#!/usr/bin/env bash
# Non-sealing burner vs. the box: how long does the Sova head stall?
#
# Runs against a box that is already up (./box/up.sh). Adds a second
# `sova-miner` ("the stranger") with its own keystore, funded from the
# box's regtest zebrad the way box/up.sh funds its own miner, burning MORE
# per epoch than the box miner (so it is rank 0 whenever it burns) and
# running NO node: it never seals anything.
#
# The box miner keeps burning every other block or so, so both epoch kinds
# occur on their own:
#   - stranger alone  -> box node is unranked; with SIP-6 off (the box
#                        default) it seals rank 0's derivation, with SIP-6
#                        on the null block, after (ranked + 2) x rank_step;
#                        log line "abandoned burn epoch"
#   - both burned     -> box node is rank 1 and seals its own derivation
#                        after 1 x rank_step
#
# It samples Zcash height vs eth_blockNumber once a second for
# STALL_BURN_SECS with the stranger burning, stops the stranger, then waits
# up to STALL_DRAIN_SECS for the Sova head to catch up, and prints: max lag
# (Zcash blocks), longest head stall (s), per-epoch seal delays from the
# node log, and the time to recover.
#
# Knobs: STALL_BURN_SECS (default 180), STALL_DRAIN_SECS (default 900),
# STALL_PER_EPOCH_ZAT (default 200000), MINER_BIN (default: the box's).
#
# SIP-6 on (null blocks instead of rank 0's derivation; same timing): bring
# the box up with the env passed through to bin/sova, after a first `up`
# has created the miner keystore:
#   SOVA_SIP6=1 SOVA_SEALER_KEYSTORE="$PWD/box/up/.run/miner/keystore.json" ./box/up.sh
#
# Keep STALL_BURN_SECS short (<= ~60 s) to see the stall-then-recover the
# box shows; at 180 s the lag passes the sealer's QUEUE_RETAIN (256 epochs)
# and the node halts for good (see crates/engine/tests/nonsealer_stall.rs).
#
# Leaves the box up; removes only its own stranger datadir.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../.." && pwd)"
RUN_DIR="${ROOT}/box/up/.run"
LOG_DIR="${RUN_DIR}/logs"
BOX_ENV_FILE="${RUN_DIR}/box.env"

[[ -f "${BOX_ENV_FILE}" ]] || { echo "no ${BOX_ENV_FILE}: run ./box/up.sh first" >&2; exit 1; }
# shellcheck disable=SC1090
source "${BOX_ENV_FILE}"
ZEBRAD_RPC="http://127.0.0.1:${ZEBRAD_PORT:-18232}"
SOVA_RPC="http://127.0.0.1:${RPC_PORT:-8545}"
NODE_LOG="${LOG_DIR}/sova-node.log"

BURN_SECS="${STALL_BURN_SECS:-180}"
DRAIN_SECS="${STALL_DRAIN_SECS:-900}"
PER_EPOCH="${STALL_PER_EPOCH_ZAT:-200000}"
MINER_BIN="${MINER_BIN:-$(pgrep -fl 'sova-miner.*--data-dir' | awk '{print $2}' | head -1)}"
[[ -x "${MINER_BIN}" ]] || { echo "no sova-miner binary (set MINER_BIN)" >&2; exit 1; }

STRANGER_DIR="${RUN_DIR}/stranger"
OUT="${RUN_DIR}/stall-samples.tsv"

rpc_zebra() {
  curl -s -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}" "${ZEBRAD_RPC}/"
}
zheight() { rpc_zebra getblockcount '[]' | sed -n 's/.*"result":\([0-9]*\).*/\1/p'; }
sheight() {
  local hex
  hex="$(curl -s -X POST -H 'Content-Type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' "${SOVA_RPC}" |
    sed -n 's/.*"result":"0x\([0-9a-fA-F]*\)".*/\1/p')"
  [[ -n "${hex}" ]] && echo $((16#${hex}))
}

cleanup() {
  [[ -n "${STRANGER_PID:-}" ]] && kill "${STRANGER_PID}" 2>/dev/null
  rm -rf -- "${STRANGER_DIR}"
}
trap cleanup EXIT

rm -rf -- "${STRANGER_DIR}"
mkdir -p "${STRANGER_DIR}"
"${MINER_BIN}" --data-dir "${STRANGER_DIR}" --network regtest init >"${STRANGER_DIR}/init.log" 2>&1
TADDR="$(awk '/t-addr to fund/ {print $NF}' "${STRANGER_DIR}/init.log")"
EVM="$(awk '/evm address/ {print $NF}' "${STRANGER_DIR}/init.log" | head -1)"
[[ -n "${TADDR}" ]] || { echo "stranger init failed:" >&2; cat "${STRANGER_DIR}/init.log" >&2; exit 1; }
echo "stranger: t-addr ${TADDR} evm ${EVM} (no node; burns ${PER_EPOCH} zat/epoch)"

# Funding generates 101 burn-less blocks at once; let the box catch up
# before measuring so the baseline lag is ~0.
rpc_zebra generatetoaddress "[101,\"${TADDR}\"]" | grep -q result || { echo "funding failed" >&2; exit 1; }
echo -n "waiting for the box to absorb the funding blocks "
for _ in $(seq 1 120); do
  z="$(zheight)"; s="$(sheight)"
  [[ -n "${z}" && -n "${s}" && $((z - s)) -le 2 ]] && break
  echo -n "."; sleep 1
done
echo " zcash ${z} sova ${s}"

LOG_MARK=$(($(wc -l <"${NODE_LOG}") + 1))
"${MINER_BIN}" --data-dir "${STRANGER_DIR}" --network regtest mine \
  --budget-zat 100000000 --per-epoch-zat "${PER_EPOCH}" --rpc "${ZEBRAD_RPC}" \
  >"${STRANGER_DIR}/miner.log" 2>&1 &
STRANGER_PID=$!

printf 't\tzcash\tsova\tlag\n' >"${OUT}"
RECOVERED=1; start=${SECONDS}; max_lag=0; last_s=-1; last_move=${SECONDS}; longest=0; stopped_at=""
while :; do
  t=$((SECONDS - start))
  z="$(zheight)"; s="$(sheight)"
  if [[ -n "${z}" && -n "${s}" ]]; then
    lag=$((z - s))
    ((lag > max_lag)) && max_lag=${lag}
    if [[ "${s}" != "${last_s}" ]]; then
      stall=$((SECONDS - last_move)); ((stall > longest)) && longest=${stall}
      last_move=${SECONDS}; last_s=${s}
    fi
    printf '%s\t%s\t%s\t%s\n' "${t}" "${z}" "${s}" "${lag}" >>"${OUT}"
  fi
  if [[ -z "${stopped_at}" && ${t} -ge ${BURN_SECS} ]]; then
    kill "${STRANGER_PID}" 2>/dev/null; wait "${STRANGER_PID}" 2>/dev/null; STRANGER_PID=""
    stopped_at=${t}
    echo "t=${t}s stranger stopped; lag ${lag:-?} (max ${max_lag}); draining"
  fi
  if [[ -n "${stopped_at}" ]]; then
    [[ -n "${lag:-}" && ${lag} -le 2 ]] && break
    ((t - stopped_at >= DRAIN_SECS)) && { echo "not recovered after ${DRAIN_SECS}s"; RECOVERED=0; break; }
  fi
  ((t % 15 == 0)) && echo "t=${t}s zcash=${z:-?} sova=${s:-?} lag=${lag:-?}"
  sleep 1
done

echo
echo "=== result ==="
echo "stranger burns: $(grep -cE '^epoch [0-9]+:' "${STRANGER_DIR}/miner.log" 2>/dev/null)"
echo "max lag: ${max_lag} Zcash blocks; longest Sova head stall: ${longest}s"
stuck=$((SECONDS - last_move))
if ((RECOVERED)); then
  echo "recovered $((t - stopped_at))s after the stranger stopped (lag ${lag:-?})"
else
  echo "NOT recovered: head ${last_s} unchanged for ${stuck}s, lag ${lag:-?}"
  ((lag > 256)) && echo "lag is past the sealer's QUEUE_RETAIN (256): the front epoch was pruned; the sealer has halted for good"
fi
echo "abandoned-epoch seals: $(tail -n +"${LOG_MARK}" "${NODE_LOG}" | grep -c 'abandoned burn epoch')"
echo "samples: ${OUT}"
echo
echo "per-epoch trigger delays (first trigger minus previous height's trigger, >5s only):"
tail -n +"${LOG_MARK}" "${NODE_LOG}" | grep -E 'sova epoch trigger|abandoned burn epoch' |
  sed -E 's/\x1b\[[0-9;]*m//g' |
  awk '
    function secs(ts,   a) { split(substr(ts, 12, 12), a, ":"); return a[1]*3600 + a[2]*60 + a[3] }
    /abandoned/ { ab = 1; next }
    {
      t = secs($1); h = ""; st = ""
      for (i = 1; i <= NF; i++) { if ($i ~ /^sova_height=/) h = substr($i, 13); if ($i ~ /^settled=/) st = substr($i, 9) }
      if (prev != "" && t - prev > 5) printf "  sova %s  +%.1fs  settled=%s%s\n", h, t - prev, st, (ab ? "  (abandoned: stranger alone)" : (st == "true" ? "  (ranked fallback or own)" : ""))
      prev = t; ab = 0
    }'
