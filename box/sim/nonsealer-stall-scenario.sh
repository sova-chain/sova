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
#                        on the null block, once epoch E+1's Zcash block has
#                        been seen for one rank_step (or (ranked + 2) x
#                        rank_step after it first saw E, if sooner); log
#                        line "abandoned burn epoch"
#   - both burned     -> box node is rank 1 and seals its own derivation
#                        1 x rank_step after it first saw the epoch
#
# It samples Zcash height vs eth_blockNumber once a second for
# STALL_BURN_SECS with the stranger burning, stops the stranger, then waits
# up to STALL_DRAIN_SECS for the Sova head to catch up, and prints: max lag
# (Zcash blocks), longest head stall (s), the time to recover, and each
# epoch's seal delay (node's trigger minus when the scenario first saw that
# Zcash block).
#
# Pass/fail (exit 1 on any): the head must recover (lag <= 2) within
# STALL_MAX_RECOVER_SECS of the stranger stopping, the lag must stay
# <= STALL_MAX_LAG Zcash blocks and the head must never sit still longer
# than STALL_MAX_HEAD_STALL seconds. With the box's 3 s blocks and 15 s
# rank_step an abandoned epoch seals ~20 s after its Zcash block (lag ~7),
# whatever STALL_BURN_SECS is. Before the sealer fix (558381b) each wait
# started at the epoch's turn in the queue: the lag grew without bound
# (96 after 45 s of burning) and past 256 epochs the node halted for good.
#
# Knobs: STALL_BURN_SECS (default 180), STALL_DRAIN_SECS (default 900),
# STALL_PER_EPOCH_ZAT (default 200000), MINER_BIN (default: the box's),
# STALL_MAX_LAG (default 15), STALL_MAX_HEAD_STALL (default 45, the
# one-burner ladder: 3 x rank_step), STALL_MAX_RECOVER_SECS (default 60).
#
# SIP-6 on (null blocks instead of rank 0's derivation; same timing): bring
# the box up with the env passed through to bin/sova, after a first `up`
# has created the miner keystore:
#   SOVA_SIP6=1 SOVA_SEALER_KEYSTORE="$PWD/box/up/.run/miner/keystore.json" ./box/up.sh
# For runs longer than ~4 min raise the box miner's budget
# (SOVA_BOX_BUDGET_ZAT) so it keeps burning throughout.
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
MAX_LAG="${STALL_MAX_LAG:-15}"
MAX_HEAD_STALL="${STALL_MAX_HEAD_STALL:-45}"
MAX_RECOVER="${STALL_MAX_RECOVER_SECS:-60}"
# The box miner's own binary, from its pid (the path may contain spaces).
MINER_BIN="${MINER_BIN:-$(ps -o command= -p "$(cat "${RUN_DIR}/pids/miner.pid" 2>/dev/null)" 2>/dev/null |
  sed 's/ --data-dir .*//')}"
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

printf 't\tzcash\tsova\tlag\tunix\n' >"${OUT}"
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
    printf '%s\t%s\t%s\t%s\t%s\n' "${t}" "${z}" "${s}" "${lag}" "$(date -u +%s)" >>"${OUT}"
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
stuck=$((SECONDS - last_move)); ((stuck > longest)) && longest=${stuck}
echo "max lag: ${max_lag} Zcash blocks; longest Sova head stall: ${longest}s"
if ((RECOVERED)); then
  echo "recovered $((t - stopped_at))s after the stranger stopped (lag ${lag:-?})"
else
  echo "NOT recovered: head ${last_s} unchanged for ${stuck}s, lag ${lag:-?}"
fi
echo "abandoned-epoch seals: $(tail -n +"${LOG_MARK}" "${NODE_LOG}" | grep -c 'abandoned burn epoch')"
echo "samples: ${OUT}"
echo
# Seal delay per epoch: the node's first trigger for height H minus the
# first sample that saw Zcash height >= H (1 s resolution; UTC time of day,
# as in the node log).
echo "per-epoch seal delay (node trigger minus Zcash block first seen):"
tail -n +"${LOG_MARK}" "${NODE_LOG}" | grep -E 'sova epoch trigger|abandoned burn epoch' |
  sed -E 's/\x1b\[[0-9;]*m//g' |
  awk -F'\t' '
    FNR == NR {
      if (FNR == 1) next
      z = $2 + 0
      if (top == "") top = z
      for (h = top + 1; h <= z; h++) seen[h] = $5 % 86400
      if (z > top) top = z
      next
    }
    {
      split($0, f, " "); split(substr(f[1], 12), a, ":"); ts = a[1]*3600 + a[2]*60 + a[3]
      h = ""
      for (i = 1; i in f; i++) if (f[i] ~ /^(zcash_)?height=/) { h = f[i]; sub(/^[a-z_]*=/, "", h) }
      if (/abandoned burn epoch/) { ab[h] = 1; next }
      if (h in trig || !(h in seen)) next
      trig[h] = 1
      d = ts - seen[h]; if (d < -43200) d += 86400
      out[h] = d
    }
    END { for (h in out) printf "%s\t%.1f\t%s\n", ((h in ab) ? "abandoned (stranger alone)" : "other (own, rank 1, burn-less)"), out[h], h }
  ' "${OUT}" - | sort -t$'\t' -k1,1 -k2,2n |
  awk -F'\t' '
    function flush() {
      if (cls == "") return
      printf "  %s: n=%d min=%.1fs p50=%.1fs p90=%.1fs max=%.1fs (sova %s)\n", cls, n, d[1], d[int((n + 1) / 2)], d[int(n * 0.9) > 0 ? int(n * 0.9) : 1], d[n], hmax
      printf "    "; for (b = 0; b <= maxb; b += 5) if (hist[b]) printf " %d-%ds:%d", b, b + 5, hist[b]; printf "\n"
    }
    $1 != cls { flush(); cls = $1; n = 0; maxb = 0; split("", hist) }
    { n++; d[n] = $2; hmax = $3; b = int(($2 < 0 ? 0 : $2) / 5) * 5; hist[b]++; if (b > maxb) maxb = b }
    END { flush() }'

fail=0
((RECOVERED)) || { echo "FAIL: head did not recover within ${DRAIN_SECS}s (halted?)"; fail=1; }
((RECOVERED)) && ((t - stopped_at > MAX_RECOVER)) &&
  { echo "FAIL: recovery took $((t - stopped_at))s > ${MAX_RECOVER}s"; fail=1; }
((max_lag > MAX_LAG)) && { echo "FAIL: max lag ${max_lag} > ${MAX_LAG} Zcash blocks"; fail=1; }
((longest > MAX_HEAD_STALL)) && { echo "FAIL: head stalled ${longest}s > ${MAX_HEAD_STALL}s"; fail=1; }
((fail)) && exit 1
echo "PASS: lag <= ${MAX_LAG}, head stall <= ${MAX_HEAD_STALL}s, recovered within ${MAX_RECOVER}s"
