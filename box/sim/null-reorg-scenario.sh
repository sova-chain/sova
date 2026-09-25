#!/usr/bin/env bash
# A Zcash reorg under a NULL Sova tip, with SIP-6 and SIP-7 on: the stale
# null block is re-sealed on its parent and the chain keeps going, live and
# across a restart of the mine-mode node.
#
# Regression test for the public-testnet stall of 2026-09-24 (21:13 UTC). A
# Zcash reorg replaced the Zcash block that Sova block 323, a NULL block,
# was anchored to. SIP-4 §7 makes such a block stale: it must be re-sealed
# on its parent. Two bugs kept that from happening:
#   (a) live: the canonical ranker re-seeded the stale 323 into the
#       candidate tracker. Two null blocks tie on rank, the stale one won on
#       hash, the re-seal was never promoted, and every build of 324 on the
#       stale 323 failed SIP-7's ZcashBlocks.record continuity check,
#       DeltaMismatch(0) (selector 0x78bab1c2). Fixed in 52d02c6.
#   (b) restart: the effective head dropped below a stale tip only while a
#       live reorg's unwind marker was pending, so a restarted keeper took
#       the stale tip as built and never re-sealed. Fixed in 072d633.
# zcash-reorg-scenario.sh did not catch either: it reorgs under a burn
# block (a stale SEALED block is ranked Mismatch and never re-seeded), runs
# without SIP-6/SIP-7, and never restarts a node.
#
#   node A -- mine mode (the keeper), sova/1, dev profile, SOVA_SIP6=1 +
#             SOVA_SIP7=1, sealing key = a `sova-miner init` keystore,
#             PERSISTENT datadir (phase 2 restarts it).
#   node B -- follow-only, C5-enforcing, SOVA_SIP6=1 + SOVA_SIP7=1, sova/1
#             static peer = A.
# Both share one regtest zebrad.
#
# Making the regtest reorg look like the testnet one. Every regtest block
# auto-mine produces is coinbase-only with a transparent coinbase, so a
# replacement block mined the same way would carry the SAME value pools as
# the block it replaces, and a block built on the stale tip would pass
# SIP-7's pool continuity check silently. The replacement block is
# therefore mined with its coinbase paid to a Sapling address (zebrad's own
# default regtest Sapling miner address): the orphaned block moved the
# subsidy into the transparent pool, the replacement into Sapling. A block
# built on the stale tip then fails exactly as on the testnet,
# DeltaMismatch(0).
#
# Flow:
#   1. Fund the miner (101 Zcash blocks), epoch base B = tip + 1. Start A
#      and B, auto-mine. One real SIP-1 burn (sova-miner) mints a SEALED
#      block; the burn-less epochs after it are NULL blocks (SIP-6).
#   2. Phase 1 (live), per round: stop auto-mine; wait until A and B both
#      sit at the Zcash tip's Sova height H, a NULL block anchored to
#      Zcash Z = H + B - 1. invalidateblock(Z), mine one replacement block
#      at Z (Sapling coinbase). Both nodes see the reorg live.
#      A tie between the stale block and its re-seal only goes wrong when
#      the stale hash is the lower one (the testnet ordering): about half
#      the time. So phase 1 runs rounds until one has had that ordering
#      (at least NULL_REORG_MIN_ROUNDS, at most NULL_REORG_MAX_ROUNDS).
#   3. Phase 2 (restart), one round: as above, but node A is stopped
#      (SIGTERM; graceful, its chain is on disk) before the reorg and
#      started again on the same datadir after it. It comes back on a stale
#      tip with no reorg event ever seen, as the testnet keeper did.
#
# Assertions, per round:
#   (a) re-seal  -- within NULL_REORG_RESEAL_TIMEOUT_S of the replacement
#                  block, A and B hold a different block at H: a NULL block
#                  (empty extraData) whose parent is the stale block's
#                  parent and whose anchor is the replacement Zcash block.
#   (b) liveness -- auto-mine resumes; both heads reach H + ADVANCE
#                  (default 5) within NULL_REORG_ADVANCE_TIMEOUT_S.
#   (c) SIP-7    -- no DeltaMismatch / 0x78bab1c2 in either node's log
#                  after the re-seal (after the reorg when there was none),
#                  and ZcashBlocks.latest() at H == (Z, replacement hash) on
#                  both nodes.
#   (d) convergence -- after the round: the same head on A and B, the same
#                  hash at every height 1..head, and every canonical anchor
#                  == zebrad's getblockhash(N + B - 1).
# Setup (0): sova/1 on both nodes, B enforces C5, A seals as its miner
# address, the burn's block is sealed (97-byte extraData) and minted.
# A setup failure inside a round ends the scenario; an assertion failure in
# phase 1 skips phase 2 (the chain is usually stuck by then).
#
# Env: NULL_REORG_PHASES ("1 2"; "2" runs the restart round alone),
# NULL_REORG_MIN_ROUNDS (1), NULL_REORG_MAX_ROUNDS (6), ADVANCE (5),
# NULL_REORG_RESEAL_TIMEOUT_S (60), NULL_REORG_ADVANCE_TIMEOUT_S (90),
# NULL_REORG_A_RUST_LOG (info,engine::miner=debug), plus p2p-common.sh's
# SOVA_P2P_SIM_*, SOVA_BIN, SOVA_MINER_BIN, SOVA_P2P_SIM_KEEP_LOGS.
#
# Isolation: zebrad on :18412 (compose project sova-null-reorg-sim,
# container sova-zebrad-null-reorg), A on 10545/10551/31011, B on
# 10645/10651/31012. All overridable.

SCENARIO="null-reorg"
WORK_PREFIX="sova-null-reorg"
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18412}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-null-reorg}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-null-reorg-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=10545 10551 31011}"
: "${SOVA_P2P_SIM_B_PORTS:=10645 10651 31012}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"
trap cleanup EXIT

PHASES="${NULL_REORG_PHASES:-1 2}"
MIN_ROUNDS="${NULL_REORG_MIN_ROUNDS:-1}"
MAX_ROUNDS="${NULL_REORG_MAX_ROUNDS:-6}"
ADVANCE="${ADVANCE:-5}"
RESEAL_TIMEOUT_S="${NULL_REORG_RESEAL_TIMEOUT_S:-60}"
ADVANCE_TIMEOUT_S="${NULL_REORG_ADVANCE_TIMEOUT_S:-90}"
A_RUST_LOG="${NULL_REORG_A_RUST_LOG:-info,engine::miner=debug}"
AUTO_MINE_INTERVAL_S=2
FUND_BLOCKS=101
BUDGET_ZAT=200000
PER_EPOCH_ZAT=100000
# zebrad's hard-coded default regtest Sapling miner address
# (zebra-rpc/src/config/mining.rs, MINER_ADDRESS[Regtest][Sapling]): the
# replacement block's coinbase goes into the Sapling pool (see the header).
SAPLING_ADDR="zregtestsapling1xl84ekz6stprmvrp39s77mf9t953nqjndwlcjtzfrr3cgjjez87639xm4u9pfuvylrhecx0c2j8"
# SIP-7's ZcashBlocks predeploy and its latest() selector.
ZCASH_BLOCKS="0x0000000000000000000000000000000000005A01"
LATEST_SELECTOR="0x52bfe789"
BAD_PATTERN='DeltaMismatch|78bab1c2'

# --- helpers ------------------------------------------------------------

height_of() { eth_block_number "$1" 2>/dev/null || echo 0; }

# "hash parentHash extraDataBytes parentBeaconBlockRoot" of block $2 on $1
# (lowercase); empty when the node has no block there.
block_info() {
  eth_rpc "$1" eth_getBlockByNumber "[\"$(printf '0x%x' "$2")\", false]" | python3 -c "
import sys, json
try:
    b = json.load(sys.stdin).get('result')
except Exception:
    b = None
if b:
    print(b['hash'].lower(), b['parentHash'].lower(), (len(b['extraData']) - 2) // 2,
          (b.get('parentBeaconBlockRoot') or '-').lower())
"
}

zc_block_hash_at() {
  zc_rpc getblockhash "[$1]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'].lower())" 2>/dev/null || true
}

# "transparent sapling" chain values (zat) after Zcash block hash $1.
zc_pools_of() {
  zc_rpc getblock "[\"$1\", 1]" | python3 -c "
import sys, json
try:
    r = json.load(sys.stdin)['result']
    p = {x['id']: x.get('chainValueZat') for x in r.get('valuePools', [])}
    print(p.get('transparent'), p.get('sapling'))
except Exception:
    print('? ?')
"
}

lines_of() { wc -l <"$1" 2>/dev/null | tr -d ' ' || echo 0; }

# Lines of $1 after line $2 matching the SIP-7 continuity failure.
bad_after() {
  local n
  n="$(tail -n +"$(($2 + 1))" "$1" 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | grep -ciE -- "${BAD_PATTERN}" || true)"
  echo "${n:-0}"
}

count_in() {
  local n
  n="$(sed 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | grep -cF -- "$2" || true)"
  echo "${n:-0}"
}

stop_auto_mine() {
  if [[ -n "${AUTO_MINE_PID}" ]] && kill -0 "${AUTO_MINE_PID}" 2>/dev/null; then
    kill "${AUTO_MINE_PID}" 2>/dev/null || true
    wait "${AUTO_MINE_PID}" 2>/dev/null || true
  fi
  AUTO_MINE_PID=""
}

start_auto_mine() {
  "${REGTEST_DIR}/auto-mine.sh" "${AUTO_MINE_INTERVAL_S}" "${ZEBRAD_RPC}" >>"${WORK_DIR}/auto-mine.log" 2>&1 &
  AUTO_MINE_PID=$!
}

# Node A (mine mode) on its persistent datadir, logging to $1.
start_node_a() {
  A_LOG="$1"
  p2p_env \
    RUST_LOG="${A_RUST_LOG}" \
    SOVA_SIP6=1 \
    SOVA_SIP7=1 \
    SOVA_DATADIR="${A_DATADIR}" \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
    SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" \
    SOVA_SEALER_KEYSTORE="${MINER_DATA_DIR}/keystore.json" \
    SOVA_EPOCH_BASE="${EPOCH_BASE}" \
    SOVA_HTTP_PORT="${A_HTTP_PORT}" \
    SOVA_AUTH_PORT="${A_AUTH_PORT}" \
    SOVA_P2P_PORT="${A_P2P_PORT}" \
    "${SOVA_BIN}" >"${A_LOG}" 2>&1 &
  A_PID=$!
  wait_for_eth_rpc "${ENGINE_RPC_A}" "${A_PID}" "node A"
}

# SIGTERM node A and wait for it to exit (bin/sova shuts down gracefully,
# writing the blocks it holds in memory first).
stop_node_a() {
  local pid="${A_PID}" deadline=$((SECONDS + 90))
  kill -TERM "${pid}" 2>/dev/null || true
  while kill -0 "${pid}" 2>/dev/null; do
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "node A (pid ${pid}) still running 90s after SIGTERM; killing it" >&2
      kill -KILL "${pid}" 2>/dev/null || true
      break
    fi
    sleep 1
  done
  wait "${pid}" 2>/dev/null || true
  A_PID=""
}

# Wait until A and B both sit at the Zcash tip's Sova height; prints it.
settle_heads() { # <timeout_s>
  local deadline=$((SECONDS + $1)) zt want ha hb
  while :; do
    zt="$(zc_tip_height)"
    want=$((zt - EPOCH_BASE + 1))
    ha="$(height_of "${ENGINE_RPC_A}")"
    hb="$(height_of "${ENGINE_RPC_B}")"
    if [[ "${ha}" -eq "${want}" && "${hb}" -eq "${want}" ]]; then
      echo "${want}"
      return 0
    fi
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo "A=${ha} B=${hb} want=${want} (zcash tip ${zt})"
      return 1
    fi
    sleep 1
  done
}

latest_record() { # <url> <block>
  eth_rpc "$1" eth_call "[{\"to\":\"${ZCASH_BLOCKS}\",\"data\":\"${LATEST_SELECTOR}\"},\"$(printf '0x%x' "$2")\"]" \
    | python3 -c "import sys,json;print(json.load(sys.stdin).get('result','')[2:])" 2>/dev/null || true
}

# One line per Sova height 1..$2 on node $1: "N hash anchor zebrad@N+B-1".
dump_chain() {
  python3 - "$1" "$2" "${EPOCH_BASE}" "${ZEBRAD_RPC}" <<'PY'
import sys, json, urllib.request
url, hi, base, zurl = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
def rpc(u, m, p):
    req = urllib.request.Request(u, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return json.load(r).get("result")
    except Exception:
        return None
for n in range(1, hi + 1):
    zh = rpc(zurl + "/", "getblockhash", [n + base - 1])
    zh = ("0x" + zh.lower()) if zh else "-"
    b = rpc(url, "eth_getBlockByNumber", [hex(n), False])
    if not b:
        print(n, "-", "-", zh)
        continue
    print(n, b["hash"].lower(), (b.get("parentBeaconBlockRoot") or "-").lower(), zh)
PY
}

STALE_LOWER_ROUNDS=0
ROUND_SUMMARY=()

# One reorg under a null tip. <label> <restart: 0|1>. Returns 2 on a setup
# failure (the scenario stops), else 0 (assertions counted in FAILURES).
reorg_round() {
  local label="$1" restart="$2"
  local h z_old old_zhash new_zhash stale_hash stale_parent stale_extra stale_anchor
  local b_hash mark_a mark_b a_log_at_reorg deadline ia ib ra rb
  echo ""
  echo "=== ${label}: Zcash reorg under a NULL tip$([[ "${restart}" == 1 ]] && echo ', node A restarted across it') ==="
  stop_auto_mine
  if ! h="$(settle_heads 90)"; then
    fail "${label} setup: A and B did not settle at the Zcash tip's height (${h})"
    return 2
  fi
  z_old=$((h + EPOCH_BASE - 1))
  old_zhash="$(zc_block_hash_at "${z_old}")"
  read -r stale_hash stale_parent stale_extra stale_anchor <<<"$(block_info "${ENGINE_RPC_A}" "${h}")"
  read -r b_hash _ <<<"$(block_info "${ENGINE_RPC_B}" "${h}")"
  if [[ "${stale_extra}" != "0" ]]; then
    fail "${label} setup: tip ${h} on A is not a null block (extraData ${stale_extra} bytes)"
    return 2
  fi
  if [[ "${stale_anchor}" != "0x${old_zhash}" || "${b_hash}" != "${stale_hash}" ]]; then
    fail "${label} setup: tip ${h}: A ${stale_hash} anchor ${stale_anchor}, B ${b_hash}, zebrad@${z_old} 0x${old_zhash}"
    return 2
  fi
  echo "Sova tip H=${h} on A and B: NULL block ${stale_hash} (parent ${stale_parent}), anchored to Zcash ${z_old} 0x${old_zhash}"

  if [[ "${restart}" == "1" ]]; then
    echo "--- stopping node A (SIGTERM) before the reorg ---"
    stop_node_a
  fi
  mark_a="$(lines_of "${A_LOG}")"
  mark_b="$(lines_of "${WORK_DIR}/node-b.log")"
  a_log_at_reorg="${A_LOG}"

  # Read before invalidateblock: zebrad won't serve an invalidated block.
  local old_pools
  old_pools="$(zc_pools_of "${old_zhash}")"
  echo "--- invalidateblock(${z_old}); one replacement block at ${z_old}, coinbase to Sapling ---"
  zc_rpc invalidateblock "[\"${old_zhash}\"]" >"${WORK_DIR}/invalidate-${label// /-}.json"
  if [[ "$(zc_tip_height)" != "$((z_old - 1))" ]]; then
    fail "${label} setup: zebrad tip $(zc_tip_height) after invalidateblock, want $((z_old - 1)) ($(cat "${WORK_DIR}/invalidate-${label// /-}.json"))"
    return 2
  fi
  zc_generate_to_address 1 "${SAPLING_ADDR}"
  new_zhash="$(zc_block_hash_at "${z_old}")"
  if [[ "$(zc_tip_height)" != "${z_old}" || -z "${new_zhash}" || "${new_zhash}" == "${old_zhash}" ]]; then
    fail "${label} setup: no replacement block at ${z_old} (tip $(zc_tip_height), hash ${new_zhash:-<none>})"
    return 2
  fi
  echo "Zcash ${z_old}: 0x${old_zhash} -> 0x${new_zhash}; pools (transparent sapling) orphaned: ${old_pools}, replacement: $(zc_pools_of "${new_zhash}")"

  local restarted_on_stale="-"
  if [[ "${restart}" == "1" ]]; then
    echo "--- starting node A again on its datadir ---"
    start_node_a "${WORK_DIR}/node-a-restart.log" || return 2
    mark_a=0
    read -r ra _ <<<"$(block_info "${ENGINE_RPC_A}" "${h}")"
    restarted_on_stale="$([[ "${ra}" == "${stale_hash}" ]] && echo yes || echo "no (${ra:-<none>})")"
    echo "node A back (pid ${A_PID}), head $(height_of "${ENGINE_RPC_A}"); block ${h} still the stale one: ${restarted_on_stale}"
    if ! wait_for_sova_peer "${A_LOG}" 60; then
      fail "${label} setup: no sova/1 session after node A restarted"
      return 2
    fi
  fi

  # (a) the re-seal
  deadline=$((SECONDS + RESEAL_TIMEOUT_S))
  while :; do
    read -r ia _ _ ra <<<"$(block_info "${ENGINE_RPC_A}" "${h}")"
    read -r ib _ _ rb <<<"$(block_info "${ENGINE_RPC_B}" "${h}")"
    if [[ -n "${ia}" && "${ia}" != "${stale_hash}" && "${ra}" == "0x${new_zhash}" \
      && "${ib}" == "${ia}" ]]; then
      break
    fi
    [[ ${SECONDS} -ge ${deadline} ]] && break
    sleep 0.5
  done
  local reseal_hash reseal_parent reseal_extra reseal_anchor resealed=0
  read -r reseal_hash reseal_parent reseal_extra reseal_anchor <<<"$(block_info "${ENGINE_RPC_A}" "${h}")"
  read -r ib _ _ rb <<<"$(block_info "${ENGINE_RPC_B}" "${h}")"
  if [[ "${reseal_hash}" != "${stale_hash}" && "${reseal_anchor}" == "0x${new_zhash}" ]]; then
    resealed=1
    if [[ "${reseal_extra}" == "0" && "${reseal_parent}" == "${stale_parent}" ]]; then
      pass "(a) ${label}: node A re-sealed ${h} on its parent: NULL block ${reseal_hash} (was ${stale_hash}), anchored to the replacement Zcash block"
    else
      fail "(a) ${label}: node A's new block ${h} ${reseal_hash}: extraData ${reseal_extra} bytes, parent ${reseal_parent} (want a null block on ${stale_parent})"
    fi
  else
    fail "(a) ${label}: node A still holds ${reseal_hash:-<none>} at ${h} (anchor ${reseal_anchor:-<none>}) ${RESEAL_TIMEOUT_S}s after the replacement block; want a re-seal anchored to 0x${new_zhash}"
  fi
  if [[ "${ib}" == "${reseal_hash}" && "${resealed}" == "1" ]]; then
    pass "(a) ${label}: node B adopted the re-seal at ${h}"
  else
    fail "(a) ${label}: node B holds ${ib:-<none>} at ${h} (anchor ${rb:-<none>}), A ${reseal_hash:-<none>}"
  fi
  local ordering="-"
  if [[ "${resealed}" == "1" ]]; then
    if [[ "${stale_hash}" < "${reseal_hash}" ]]; then
      ordering="stale hash LOWER than the re-seal (the testnet ordering: the stale block wins a plain hash tie)"
      STALE_LOWER_ROUNDS=$((STALE_LOWER_ROUNDS + 1))
    else
      ordering="re-seal hash lower (a plain hash tie already favours it)"
    fi
    echo "  tie-break: ${ordering}"
  fi
  local reseal_mark_a reseal_mark_b
  if [[ "${resealed}" == "1" ]]; then
    reseal_mark_a="$(lines_of "${A_LOG}")"
    reseal_mark_b="$(lines_of "${WORK_DIR}/node-b.log")"
  else
    reseal_mark_a="${mark_a}"
    reseal_mark_b="${mark_b}"
  fi

  # (b) liveness
  start_auto_mine
  local target=$((h + ADVANCE)) live_ok=1
  if ! wait_for_block_number "${ENGINE_RPC_A}" "${target}" "${ADVANCE_TIMEOUT_S}"; then
    live_ok=0
  fi
  wait_for_block_number "${ENGINE_RPC_B}" "${target}" 30 || live_ok=0
  local la lb
  la="$(height_of "${ENGINE_RPC_A}")"
  lb="$(height_of "${ENGINE_RPC_B}")"
  if [[ "${live_ok}" == "1" ]]; then
    pass "(b) ${label}: both heads advanced >= ${ADVANCE} past ${h} (A=${la} B=${lb})"
  else
    fail "(b) ${label}: heads did not reach ${target} (A=${la} B=${lb}; Zcash tip $(zc_tip_height), Sova height $(($(zc_tip_height) - EPOCH_BASE + 1)))"
  fi
  stop_auto_mine

  # (c) SIP-7
  local bad_a bad_b
  bad_a="$(bad_after "${A_LOG}" "${reseal_mark_a}")"
  bad_b="$(bad_after "${WORK_DIR}/node-b.log" "${reseal_mark_b}")"
  local since="after the re-seal"
  [[ "${resealed}" == "1" ]] || since="after the reorg (no re-seal)"
  if [[ "${bad_a}" -eq 0 && "${bad_b}" -eq 0 ]]; then
    pass "(c) ${label}: no DeltaMismatch / 0x78bab1c2 in A's or B's log ${since}"
  else
    fail "(c) ${label}: DeltaMismatch / 0x78bab1c2 ${since}: A ${bad_a} line(s), B ${bad_b} line(s)"
    sed 's/\x1b\[[0-9;]*m//g' "${A_LOG}" | tail -n +"$((reseal_mark_a + 1))" | grep -iE -- "${BAD_PATTERN}" | head -3 | cut -c1-400 >&2 || true
  fi
  local want_rec rec_a rec_b
  want_rec="$(printf '%064x' "${z_old}")${new_zhash}"
  rec_a="$(latest_record "${ENGINE_RPC_A}" "${h}")"
  rec_b="$(latest_record "${ENGINE_RPC_B}" "${h}")"
  if [[ "${rec_a}" == "${want_rec}" && "${rec_b}" == "${want_rec}" ]]; then
    pass "(c) ${label}: ZcashBlocks.latest() @${h} == (${z_old}, 0x${new_zhash}) on A and B"
  else
    fail "(c) ${label}: ZcashBlocks.latest() @${h}: A=${rec_a:-<none>} B=${rec_b:-<none>}, want (${z_old}, 0x${new_zhash})"
  fi

  # (d) convergence
  local final final_b
  final="$(height_of "${ENGINE_RPC_A}")"
  wait_for_block_number "${ENGINE_RPC_B}" "${final}" 30 || true
  final_b="$(height_of "${ENGINE_RPC_B}")"
  local tag="${label// /-}"
  dump_chain "${ENGINE_RPC_A}" "${final}" >"${WORK_DIR}/chain-a-${tag}.txt"
  dump_chain "${ENGINE_RPC_B}" "${final}" >"${WORK_DIR}/chain-b-${tag}.txt"
  if [[ "${final}" -eq "${final_b}" ]]; then
    pass "(d) ${label}: same head on A and B (${final})"
  else
    fail "(d) ${label}: heads differ: A=${final} B=${final_b}"
  fi
  local diff_heights bad_anchors
  diff_heights="$(paste -d' ' "${WORK_DIR}/chain-a-${tag}.txt" "${WORK_DIR}/chain-b-${tag}.txt" \
    | awk '$2 != $6 || $2 == "-" {print $1}' | tr '\n' ' ')"
  if [[ -z "${diff_heights}" ]]; then
    pass "(d) ${label}: A and B hold the same hash at every height 1..${final}"
  else
    fail "(d) ${label}: A and B differ at heights: ${diff_heights}"
  fi
  bad_anchors="$(awk '$3 != $4 {print $1}' "${WORK_DIR}/chain-a-${tag}.txt" | tr '\n' ' ')"
  if [[ -z "${bad_anchors}" ]]; then
    pass "(d) ${label}: every canonical anchor 1..${final} == zebrad getblockhash(N+B-1)"
  else
    fail "(d) ${label}: canonical blocks anchored to Zcash blocks zebrad no longer has: ${bad_anchors}"
  fi

  local outcome="stale ${stale_hash:0:14}.. -> ${reseal_hash:0:14}..; ${ordering%% (*}"
  [[ "${resealed}" == "1" ]] || outcome="stale ${stale_hash:0:14}.. NOT re-sealed"
  ROUND_SUMMARY+=("${label}: H=${h} Z=${z_old} ${outcome}; restarted on stale tip: ${restarted_on_stale}; DeltaMismatch lines in A's log since the reorg: $(bad_after "${a_log_at_reorg}" "${mark_a}"); head ${final}")
  return 0
}

preflight
start_stack

# ---------------------------------------------------------------------
# Miner identity (the sealing key); fund it; epoch base after the funding.
# ---------------------------------------------------------------------
MINER_DATA_DIR="${WORK_DIR}/miner"
A_DATADIR="${WORK_DIR}/datadir-a"
mkdir -p "${MINER_DATA_DIR}" "${A_DATADIR}"
"${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest init >"${WORK_DIR}/miner-init.log" 2>&1
TADDR="$(awk '/t-addr to fund/ {print $NF}' "${WORK_DIR}/miner-init.log")"
EVM_ADDR="$(awk '/evm address/ {print $NF}' "${WORK_DIR}/miner-init.log" | head -1)"
if [[ -z "${TADDR}" || -z "${EVM_ADDR}" || ! -f "${MINER_DATA_DIR}/keystore.json" ]]; then
  fail "setup: could not parse the miner identity / find its keystore"
  cat "${WORK_DIR}/miner-init.log" >&2
  exit 1
fi
echo "miner identity: ${TADDR} / ${EVM_ADDR}"
echo "--- funding: ${FUND_BLOCKS} blocks to the miner's own address (coinbase maturity) ---"
zc_generate_to_address "${FUND_BLOCKS}" "${TADDR}"
EPOCH_BASE=$(($(zc_tip_height) + 1))
echo "epoch base B=${EPOCH_BASE} (Sova block N anchors Zcash N+${EPOCH_BASE}-1)"

echo "--- starting node A (mine mode, SIP-6 + SIP-7, persistent datadir) ---"
start_node_a "${WORK_DIR}/node-a.log" || exit 1
ENODE_A="$(local_enode "${A_LOG}")" || {
  fail "setup: node A never printed its enode"
  exit 1
}
echo "node A up (pid ${A_PID}); enode ${ENODE_A}"

echo "--- starting node B (follow-only, SIP-6 + SIP-7, sova/1 static peer = A) ---"
p2p_env \
  SOVA_SIP6=1 \
  SOVA_SIP7=1 \
  SOVA_FOLLOW_ONLY=1 \
  SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
  SOVA_EPOCH_BASE="${EPOCH_BASE}" \
  SOVA_P2P_PEERS="${ENODE_A}" \
  SOVA_HTTP_PORT="${B_HTTP_PORT}" \
  SOVA_AUTH_PORT="${B_AUTH_PORT}" \
  SOVA_P2P_PORT="${B_P2P_PORT}" \
  "${SOVA_BIN}" >"${WORK_DIR}/node-b.log" 2>&1 &
B_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B" || exit 1
echo "node B up (pid ${B_PID})"

echo ""
echo "=== (0) setup ==="
check_p2p_node_log "node A" "${A_LOG}"
check_p2p_node_log "node B" "${WORK_DIR}/node-b.log"
log_has "${A_LOG}" "sip-6: sealing as" 15 || true
A_SEALS_AS="$(strip_ansi "${A_LOG}" | grep -o 'sip-6: sealing as 0x[0-9a-fA-F]*' | head -1 | awk '{print tolower($NF)}')"
if [[ -n "${A_SEALS_AS}" && "${A_SEALS_AS}" == "$(tr 'A-F' 'a-f' <<<"${EVM_ADDR}")" ]]; then
  pass "setup: node A seals (SIP-6) as its miner address ${EVM_ADDR}"
else
  fail "setup: node A's log lacks 'sip-6: sealing as ${EVM_ADDR}'"
fi
if log_has "${WORK_DIR}/node-b.log" "expectations: enforcing settlements" 15; then
  pass "setup: node B enforces C5 against its own zebrad view (epoch base ${EPOCH_BASE})"
else
  fail "setup: node B isn't enforcing C5"
fi
if wait_for_sova_peer "${A_LOG}" 60 && wait_for_sova_peer "${WORK_DIR}/node-b.log" 60; then
  pass "setup: sova/1 session A<->B established"
else
  fail "setup: no sova/1 session between A and B within 60s"
  exit 1
fi

echo "--- auto-mine every ${AUTO_MINE_INTERVAL_S}s; waiting for Sova height 2 on A and B ---"
start_auto_mine
if ! wait_for_block_number "${ENGINE_RPC_A}" 2 90 || ! wait_for_block_number "${ENGINE_RPC_B}" 2 60; then
  fail "setup: chain did not start (A=$(height_of "${ENGINE_RPC_A}") B=$(height_of "${ENGINE_RPC_B}"))"
  exit 1
fi

echo ""
echo "=== one burn (a sealed block); the epochs after it are null ==="
if ! "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
  --budget-zat "${BUDGET_ZAT}" --per-epoch-zat "${PER_EPOCH_ZAT}" \
  --rpc "${ZEBRAD_RPC}" --max-epochs 1 >"${WORK_DIR}/mine-1.log" 2>&1; then
  fail "setup: sova-miner mine exited nonzero"
  cat "${WORK_DIR}/mine-1.log" >&2
  exit 1
fi
grep -E '^epoch [0-9]+:' "${WORK_DIR}/mine-1.log" || true
if ! BAL="$(wait_for_balance_change "${ENGINE_RPC_A}" "${EVM_ADDR}" 0 90)"; then
  fail "setup: the burn never minted on A (balance ${BAL})"
  exit 1
fi
N_BURN="$(strip_ansi "${A_LOG}" | grep 'sova epoch trigger' | grep 'settled=true' | grep -oE 'sova_height=[0-9]+' | head -1 | cut -d= -f2)"
if [[ -z "${N_BURN}" ]]; then
  # Fall back: the first block with a withdrawal to the miner.
  for ((n = 1; n <= $(height_of "${ENGINE_RPC_A}"); n++)); do
    if [[ "$(eth_rpc "${ENGINE_RPC_A}" eth_getBlockByNumber "[\"$(printf '0x%x' "${n}")\", false]" \
      | python3 -c "import sys,json;print(len(json.load(sys.stdin)['result'].get('withdrawals') or []))")" != "0" ]]; then
      N_BURN="${n}"
      break
    fi
  done
fi
read -r _ _ BURN_EXTRA _ <<<"$(block_info "${ENGINE_RPC_A}" "${N_BURN:-0}")"
if [[ "${BURN_EXTRA}" == "97" ]]; then
  pass "setup: the burn minted ${BAL} wei at Sova ${N_BURN} in a SEALED block (97-byte extraData)"
else
  fail "setup: the burn's block ${N_BURN:-<unknown>} has a ${BURN_EXTRA:-?}-byte extraData (want 97, sealed)"
fi
wait_for_block_number "${ENGINE_RPC_A}" $((${N_BURN:-1} + 3)) 60 || true

for phase in ${PHASES}; do
  case "${phase}" in
    1)
      round=0
      while [[ ${round} -lt ${MAX_ROUNDS} ]]; do
        round=$((round + 1))
        before=${FAILURES}
        reorg_round "phase 1 round ${round}" 0
        rc=$?
        [[ ${rc} -eq 2 ]] && exit 1
        [[ ${FAILURES} -gt ${before} ]] && break
        [[ ${round} -ge ${MIN_ROUNDS} && ${STALE_LOWER_ROUNDS} -gt 0 ]] && break
      done
      if [[ ${FAILURES} -eq 0 && ${STALE_LOWER_ROUNDS} -eq 0 ]]; then
        echo "NOTE: in ${round} round(s) the re-seal always had the lower hash, so phase 1 never met the testnet ordering (stale block lower); raise NULL_REORG_MAX_ROUNDS" >&2
      fi
      ;;
    2)
      if [[ ${FAILURES} -gt 0 ]]; then
        echo ""
        echo "--- skipping phase 2: ${FAILURES} assertion(s) already failed (the chain is likely stuck) ---"
        continue
      fi
      reorg_round "phase 2 restart" 1
      [[ $? -eq 2 ]] && exit 1
      ;;
    *)
      echo "error: unknown phase '${phase}' in NULL_REORG_PHASES" >&2
      exit 1
      ;;
  esac
done

echo ""
echo "=== diagnostics ==="
echo "  epoch base ${EPOCH_BASE}; burn minted at Sova ${N_BURN:-?}; rounds with the testnet ordering (stale hash lower): ${STALE_LOWER_ROUNDS}"
for s in "${ROUND_SUMMARY[@]}"; do
  echo "  ${s}"
done
for log in "${WORK_DIR}"/node-*.log; do
  echo "  ${log##*/}: 're-sealing': $(count_in "${log}" 're-sealing');" \
    "'zcash reorg observed': $(count_in "${log}" 'zcash reorg observed');" \
    "'loses preference': $(count_in "${log}" 'loses preference');" \
    "'DeltaMismatch|78bab1c2': $(bad_after "${log}" 0); 'reputation hit': $(count_in "${log}" 'reputation hit')"
done

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "NULL REORG SCENARIO PASSED (phases ${PHASES}; all assertions)"
else
  echo "NULL REORG SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
