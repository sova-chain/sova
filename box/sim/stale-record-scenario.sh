#!/usr/bin/env bash
# The keeper must never seal a block whose SIP-7 ZcashBlocks record
# describes a different Zcash block than the block's anchor.
#
# REGRESSION TEST for the public-testnet freezes of 2026-10-02 (Sova
# #47,667) and 2026-10-04 (#66,641). A Sova block's anchor
# (`parent_beacon_block_root`) is the Zcash hash from the SEALER's own
# follower (engine::driver::SealerCore, engine::local); its SIP-7
# `ZcashBlocks.record()` system call (evm::blocks::record_zcash_block)
# reads the expectations follower's index (engine::expectations,
# engine::zcash_index). During a Zcash reorg the two can briefly sit on
# different branches. Twice the keeper sealed a block anchored to the
# canonical Zcash hash while recording the summary of the Zcash block the
# reorg had replaced: the header's state root committed to that stale
# record, every validator (recording from its own, canonical index)
# computed another root and rejected the block, and the public chain froze
# while the keeper sealed alone. The fix (`6be7bbb`, v0.1.19,
# `check_record_anchor`): the indexed hash at the anchored height must
# equal the anchor, else the block is refused as an execution error
# (retried, never cached invalid).
#
# What has to be held open. Lagging the whole expectations follower behind
# the sealer is NOT enough: the same follower feeds the C5 records, and
# the keeper's own SIP-4 anchor check (SovaConsensus) then holds its own
# block ("sova-hold: zcash anchor mismatch") until the follower moves, and
# the rebuilt block is consistent. Measured on 340579f (pre-fix) on
# 2026-10-04: no split. For the keeper to accept a stale record, its anchor
# check must already see the new branch while `record()` still reads the
# replaced block. In one process that is an instant: reth 2.6 runs the
# pre-execution consensus checks on a background thread concurrently with
# execution (engine/tree payload_validator.rs, spawn_convert_and_validate),
# so a follower update landing between execution's `record()` and the
# consensus thread's anchor check lets the stale record through. The test
# hook below holds that instant open for as long as the scenario needs.
#
#   zebrad   -- one regtest zebrad.
#   proxy S  -- box/sim/zebrad-freeze-proxy.py: node A's SOVA_ZEBRAD_RPC
#               (the sealer, the C5 records and the SIP-4 anchor check).
#   proxy I  -- a second instance: node A's SOVA_TEST_INDEX_ZEBRAD_RPC, a
#               test-only hook in bin/sova (unset on every real node): a
#               second follower feeds the SIP-7 index alone from it
#               (engine::expectations::Feeds::INDEX_ONLY).
#   node A   -- mine mode (the keeper), sova/1, SIP-6 + SIP-7, the only
#               producer. No burns: every block is a SIP-6 null block, as
#               #47,667 was.
#   node B   -- follow-only, C5-enforcing, SIP-6 + SIP-7, sova/1 static
#               peer = A, zebrad direct, no hook (a validator on the
#               canonical chain).
#
# Flow (one round; STALE_RECORD_ROUNDS for more):
#   1. A and B settle at the Zcash tip's Sova height H0 (auto-mine off).
#   2. Freeze S: A's sealer and anchor check stop seeing new Zcash blocks.
#   3. Mine X at Zcash Z = Z0 + 1 (transparent coinbase). A's index (via I,
#      live) indexes X; B indexes X. Nobody seals H = H0 + 1. Freeze I with
#      X in its view.
#   4. Zcash reorg at the tip: invalidateblock(X), mine X' at Z to the same
#      transparent address: the same pools as X, another hash, as on the
#      testnet (#47,667's replaced sibling had identical time, pools and
#      stats). B follows X'.
#   5. Thaw S: A's sealer and anchor check see X'; A builds H anchored to
#      X' while its index still holds X at Z.
#   6. After STALE_RECORD_HOLD_S, thaw I: A's index follows the reorg.
#      Auto-mine resumes.
#
# Assertions:
#   (0) setup   -- sova/1 on both nodes, the test hook active on A only, B
#                  enforces C5, A seals as its keystore address.
#   (r) refuse  -- while A's index disagrees with the anchor, A seals
#                  nothing at H (head stays H0) and refuses to build it,
#                  naming both hashes (check_record_anchor's error).
#   (a) anchor  -- once I thaws, A seals H anchored to X' and its record
#                  for Z (ZcashBlocks.latest() at H) is (Z, X') on A and B.
#   (b) B       -- B accepts every block: B reaches A's head; no
#                  state-root rejection in B's log.
#   (d) agree   -- A and B hold the same hash and state root at every
#                  height 1..head, and every canonical anchor == zebrad's
#                  getblockhash(N + B - 1).
#
# Results on 2026-10-04: on 340579f (the release before the fix, plus the
# hook) A seals H anchored to X' recording X, B rejects it on the state
# root and stays at H0 while A seals on alone: (r), (a), (b), (d) FAIL.
# (With X' mined to Sapling instead, X and X' differ in pools too, and A
# stalls as well: SIP-7's continuity check, DeltaMismatch, refuses H + 1
# on the stale record. The testnet keeper sealed on, so X' matches X's
# pools here.)
# On release (6be7bbb + hook) every assertion passes. See box/sim/README.md.
#
# Env: STALE_RECORD_ROUNDS (1), STALE_RECORD_HOLD_S (20), ADVANCE (5),
# STALE_RECORD_ADVANCE_TIMEOUT_S (120), STALE_RECORD_A_RUST_LOG
# (info,engine::miner=debug), STALE_RECORD_PROXY_PORTS ("18601 18602"),
# plus p2p-common.sh's SOVA_P2P_SIM_*, SOVA_BIN, SOVA_MINER_BIN (only
# `sova-miner init` is used, for A's sealing keystore),
# SOVA_P2P_SIM_KEEP_LOGS.
#
# Isolation: zebrad on :18600 (compose project sova-stale-record-sim,
# container sova-zebrad-stale-record), proxies on :18601/:18602, A on
# 12045/12051/12011, B on 12046/12052/12012. All overridable.

SCENARIO="stale-record"
WORK_PREFIX="sova-stale-record"
: "${SOVA_P2P_SIM_ZEBRAD_PORT:=18600}"
: "${SOVA_P2P_SIM_ZEBRAD_CONTAINER:=sova-zebrad-stale-record}"
: "${SOVA_P2P_SIM_COMPOSE_PROJECT:=sova-stale-record-sim}"
: "${SOVA_P2P_SIM_A_PORTS:=12045 12051 12011}"
: "${SOVA_P2P_SIM_B_PORTS:=12046 12052 12012}"
export SOVA_P2P_SIM_ZEBRAD_PORT SOVA_P2P_SIM_ZEBRAD_CONTAINER SOVA_P2P_SIM_COMPOSE_PROJECT \
  SOVA_P2P_SIM_A_PORTS SOVA_P2P_SIM_B_PORTS
# shellcheck source=box/sim/p2p-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/p2p-common.sh"

ROUNDS="${STALE_RECORD_ROUNDS:-1}"
HOLD_S="${STALE_RECORD_HOLD_S:-20}"
ADVANCE="${ADVANCE:-5}"
ADVANCE_TIMEOUT_S="${STALE_RECORD_ADVANCE_TIMEOUT_S:-120}"
A_RUST_LOG="${STALE_RECORD_A_RUST_LOG:-info,engine::miner=debug}"
read -r PROXY_S_PORT PROXY_I_PORT <<<"${STALE_RECORD_PROXY_PORTS:-18601 18602}"
PROXY_S="http://127.0.0.1:${PROXY_S_PORT}"
PROXY_I="http://127.0.0.1:${PROXY_I_PORT}"
AUTO_MINE_INTERVAL_S=2
FUND_BLOCKS=101
# SIP-7's ZcashBlocks predeploy and its latest() selector.
ZCASH_BLOCKS="0x0000000000000000000000000000000000005A01"
LATEST_SELECTOR="0x52bfe789"
# check_record_anchor's error (crates/evm/src/blocks.rs).
REFUSAL="our index is on another Zcash branch"
# reth's ConsensusError::BodyStateRootDiff.
STATE_ROOT_REJECT="mismatched block state root"
PROXY_S_PID=""
PROXY_I_PID=""

stale_cleanup() {
  local ec=$?
  for pid in "${PROXY_S_PID}" "${PROXY_I_PID}"; do
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      kill "${pid}" 2>/dev/null || true
      wait "${pid}" 2>/dev/null || true
    fi
  done
  (exit "${ec}")
  cleanup
}
trap stale_cleanup EXIT

# --- helpers ------------------------------------------------------------

height_of() { eth_block_number "$1" 2>/dev/null || echo 0; }

count_in() {
  local n
  n="$(sed 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null | grep -cF -- "$2" || true)"
  echo "${n:-0}"
}

lines_of() { wc -l <"$1" 2>/dev/null | tr -d ' ' || echo 0; }

# Lines of $1 after line $2 containing the fixed string $3.
count_after() {
  local n
  n="$(tail -n +"$(($2 + 1))" "$1" 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | grep -ciF -- "$3" || true)"
  echo "${n:-0}"
}

zc_block_hash_at() {
  zc_rpc getblockhash "[$1]" | python3 -c "import sys,json;print(json.load(sys.stdin)['result'].lower())" 2>/dev/null || true
}

proxy_ctl() { # <url> <freeze|thaw>
  curl -s -X POST "$1/__$2"
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

# "hash stateRoot anchor" of block $2 on $1 (lowercase); empty if none.
block_info() {
  eth_rpc "$1" eth_getBlockByNumber "[\"$(printf '0x%x' "$2")\", false]" | python3 -c "
import sys, json
try:
    b = json.load(sys.stdin).get('result')
except Exception:
    b = None
if b:
    print(b['hash'].lower(), b['stateRoot'].lower(), (b.get('parentBeaconBlockRoot') or '-').lower())
"
}

# ZcashBlocks.latest() at block $2 on $1: "<height> <hash>" (hash 0x-hex).
latest_record() {
  eth_rpc "$1" eth_call "[{\"to\":\"${ZCASH_BLOCKS}\",\"data\":\"${LATEST_SELECTOR}\"},\"$(printf '0x%x' "$2")\"]" \
    | python3 -c "
import sys, json
try:
    r = json.load(sys.stdin).get('result') or ''
    r = r[2:]
    print(int(r[:64], 16), '0x' + r[64:128])
except Exception:
    print('- -')
"
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

# One line per Sova height 1..$2 on node $1: "N hash stateRoot anchor zebrad@N+B-1".
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
        print(n, "-", "-", "-", zh)
        continue
    print(n, b["hash"].lower(), b["stateRoot"].lower(), (b.get("parentBeaconBlockRoot") or "-").lower(), zh)
PY
}

ROUND_SUMMARY=()

# One stale-index round. Returns 2 on a setup failure (the scenario stops),
# else 0 (assertions counted in FAILURES).
stale_round() {
  local label="$1"
  local h0 h z0 z x_hash xp_hash mark_a mark_b
  echo ""
  echo "=== ${label}: Zcash reorg while A's index lags A's sealer ==="
  stop_auto_mine
  if ! h0="$(settle_heads 120)"; then
    fail "${label} setup: A and B did not settle at the Zcash tip's height (${h0})"
    return 2
  fi
  z0=$((h0 + EPOCH_BASE - 1))
  z=$((z0 + 1))
  h=$((h0 + 1))
  echo "A and B at Sova H0=${h0} (Zcash ${z0}); the race is at Sova H=${h} / Zcash Z=${z}"

  # 2. Freeze the sealer's (and the anchor check's) view.
  local s_tip
  s_tip="$(proxy_ctl "${PROXY_S}" freeze | python3 -c "import sys,json;print(json.load(sys.stdin)['tip'])")"
  if [[ "${s_tip}" != "${z0}" ]]; then
    fail "${label} setup: sealer view frozen at ${s_tip}, want ${z0}"
    return 2
  fi

  # 3. X at Z, seen by A's index (and B), not by A's sealer or anchor check.
  zc_generate_to_address 1 "${TADDR}"
  x_hash="$(zc_block_hash_at "${z}")"
  if [[ "$(zc_tip_height)" != "${z}" || -z "${x_hash}" ]]; then
    fail "${label} setup: no Zcash block at ${z} (tip $(zc_tip_height))"
    return 2
  fi
  # A's index follower polls every 2 s; let it index X before its
  # view is pinned (the frozen view holds X either way).
  sleep 5
  local i_tip
  i_tip="$(proxy_ctl "${PROXY_I}" freeze | python3 -c "import sys,json;print(json.load(sys.stdin)['tip'])")"
  if [[ "${i_tip}" != "${z}" ]]; then
    fail "${label} setup: index view frozen at ${i_tip}, want ${z}"
    return 2
  fi
  sleep 3
  if [[ "$(height_of "${ENGINE_RPC_A}")" != "${h0}" ]]; then
    fail "${label} setup: A sealed past H0 with its sealer frozen (head $(height_of "${ENGINE_RPC_A}"))"
    return 2
  fi
  echo "Zcash ${z} = X 0x${x_hash}: in A's index (view frozen at ${i_tip}), not in A's sealer/anchor-check view (frozen at ${s_tip})"

  # 4. The reorg at the tip.
  zc_rpc invalidateblock "[\"${x_hash}\"]" >"${WORK_DIR}/invalidate-${label// /-}.json"
  if [[ "$(zc_tip_height)" != "${z0}" ]]; then
    fail "${label} setup: zebrad tip $(zc_tip_height) after invalidateblock, want ${z0} ($(cat "${WORK_DIR}/invalidate-${label// /-}.json"))"
    return 2
  fi
  zc_generate_to_address 1 "${TADDR}"
  xp_hash="$(zc_block_hash_at "${z}")"
  if [[ "$(zc_tip_height)" != "${z}" || -z "${xp_hash}" || "${xp_hash}" == "${x_hash}" ]]; then
    fail "${label} setup: no replacement block at ${z} (tip $(zc_tip_height), hash ${xp_hash:-<none>})"
    return 2
  fi
  echo "Zcash reorg at ${z}: X 0x${x_hash} -> X' 0x${xp_hash} (same coinbase address and pools)"

  # 5. Thaw the sealer: it anchors H to X' (and the anchor check agrees)
  # while the index holds X.
  mark_a="$(lines_of "${A_LOG}")"
  mark_b="$(lines_of "${B_LOG}")"
  proxy_ctl "${PROXY_S}" thaw >/dev/null
  echo "--- sealer/anchor-check view thawed (A sees X'); index view stays frozen on X for ${HOLD_S}s ---"
  local deadline=$((SECONDS + HOLD_S)) seen_h="" seen_info=""
  while [[ ${SECONDS} -lt ${deadline} ]]; do
    seen_info="$(block_info "${ENGINE_RPC_A}" "${h}")"
    if [[ -n "${seen_info}" && -z "${seen_h}" ]]; then
      seen_h="${seen_info}"
      echo "  A sealed ${h} with its index on X: ${seen_info}"
    fi
    sleep 1
  done
  local refusals stale_rec_a="- -" stale_hash="" stale_root="" stale_anchor=""
  refusals="$(count_after "${A_LOG}" "${mark_a}" "${REFUSAL}")"
  if [[ -n "${seen_h}" ]]; then
    read -r stale_hash stale_root stale_anchor <<<"${seen_h}"
    stale_rec_a="$(latest_record "${ENGINE_RPC_A}" "${h}")"
  fi
  echo "  while A's index lagged: A head $(height_of "${ENGINE_RPC_A}"), B head $(height_of "${ENGINE_RPC_B}"); check_record_anchor refusals in A's log: ${refusals}"
  if [[ "${refusals}" -gt 0 ]]; then
    echo "  first refusal: $(tail -n +"$((mark_a + 1))" "${A_LOG}" | sed 's/\x1b\[[0-9;]*m//g' | grep -F -- "${REFUSAL}" | head -1 | cut -c1-600)"
  fi
  if [[ -n "${seen_h}" ]]; then
    # The incident: anchor X', record X.
    fail "(r) ${label}: A sealed ${h} ${stale_hash} while its index held X: anchor ${stale_anchor}, ZcashBlocks.latest() = ${stale_rec_a} (X 0x${x_hash}, X' 0x${xp_hash}), stateRoot ${stale_root}"
  else
    pass "(r) ${label}: A sealed nothing at ${h} while its index held X (head stays ${h0})"
  fi
  if [[ "${refusals}" -gt 0 ]] \
    && [[ "$(tail -n +"$((mark_a + 1))" "${A_LOG}" | sed 's/\x1b\[[0-9;]*m//g' | grep -F -- "${REFUSAL}" | head -1)" == *"${x_hash}"*"${xp_hash}"* ]]; then
    pass "(r) ${label}: A refused to build ${h}: 'indexed zcash block ${z} is 0x${x_hash:0:12}.., but the block anchors 0x${xp_hash:0:12}..' (${refusals} refusals)"
  else
    fail "(r) ${label}: no check_record_anchor refusal naming X and X' in A's log (${refusals} '${REFUSAL}' lines)"
  fi

  # 6. Thaw the index; the chain goes on.
  proxy_ctl "${PROXY_I}" thaw >/dev/null
  echo "--- index view thawed ---"
  start_auto_mine
  local target=$((h + ADVANCE)) live_ok=1
  wait_for_block_number "${ENGINE_RPC_A}" "${target}" "${ADVANCE_TIMEOUT_S}" || live_ok=0
  wait_for_block_number "${ENGINE_RPC_B}" "${target}" 60 || live_ok=0
  stop_auto_mine
  local la lb
  la="$(height_of "${ENGINE_RPC_A}")"
  lb="$(height_of "${ENGINE_RPC_B}")"

  # (a) A's block at H: anchored to X', recording X'.
  local ha_hash ha_anchor hb_hash rec_a rec_b
  read -r ha_hash _ ha_anchor <<<"$(block_info "${ENGINE_RPC_A}" "${h}")"
  read -r hb_hash _ _ <<<"$(block_info "${ENGINE_RPC_B}" "${h}")"
  rec_a="$(latest_record "${ENGINE_RPC_A}" "${h}")"
  rec_b="$(latest_record "${ENGINE_RPC_B}" "${h}")"
  echo "  block ${h}: A ${ha_hash:-<none>} anchor ${ha_anchor:-<none>} record ${rec_a}; B ${hb_hash:-<none>} record ${rec_b}"
  if [[ "${ha_anchor}" == "0x${xp_hash}" ]]; then
    pass "(a) ${label}: A's block ${h} is anchored to X' (0x${xp_hash:0:12}..)"
  else
    fail "(a) ${label}: A's block ${h} anchor ${ha_anchor:-<none>}, want X' 0x${xp_hash}"
  fi
  if [[ "${rec_a}" == "${z} 0x${xp_hash}" ]]; then
    pass "(a) ${label}: A's ZcashBlocks record at ${h} == (${z}, X') == its anchor"
  else
    fail "(a) ${label}: A's ZcashBlocks.latest() at ${h} = ${rec_a}, want (${z}, 0x${xp_hash}) (X was 0x${x_hash})"
  fi
  if [[ "${rec_b}" == "${z} 0x${xp_hash}" ]]; then
    pass "(a) ${label}: B's ZcashBlocks record at ${h} == (${z}, X')"
  else
    fail "(a) ${label}: B's ZcashBlocks.latest() at ${h} = ${rec_b}, want (${z}, 0x${xp_hash})"
  fi

  # (b) B accepts every block.
  local b_state_root b_held
  b_state_root="$(count_after "${B_LOG}" "${mark_b}" "${STATE_ROOT_REJECT}")"
  b_held="$(count_after "${B_LOG}" "${mark_b}" "zcash anchor mismatch")"
  if [[ "${live_ok}" == "1" && "${lb}" -ge "${la}" ]]; then
    pass "(b) ${label}: B followed A to ${lb} (A ${la}, >= ${ADVANCE} past ${h})"
  else
    fail "(b) ${label}: B at ${lb}, A at ${la} (want both >= ${target}): the chain split or stalled"
  fi
  if [[ "${b_state_root}" -eq 0 ]]; then
    pass "(b) ${label}: no state-root rejection in B's log"
  else
    fail "(b) ${label}: B rejected ${b_state_root} block(s) on the state root (and held ${b_held} on 'zcash anchor mismatch'); first: $(tail -n +"$((mark_b + 1))" "${B_LOG}" | sed 's/\x1b\[[0-9;]*m//g' | grep -iF -- "${STATE_ROOT_REJECT}" | head -1 | cut -c1-400)"
  fi

  # (d) agreement on hash, state root and anchors.
  local final tag diff_heights bad_anchors
  final="$(height_of "${ENGINE_RPC_A}")"
  wait_for_block_number "${ENGINE_RPC_B}" "${final}" 30 || true
  tag="${label// /-}"
  dump_chain "${ENGINE_RPC_A}" "${final}" >"${WORK_DIR}/chain-a-${tag}.txt"
  dump_chain "${ENGINE_RPC_B}" "${final}" >"${WORK_DIR}/chain-b-${tag}.txt"
  diff_heights="$(paste -d' ' "${WORK_DIR}/chain-a-${tag}.txt" "${WORK_DIR}/chain-b-${tag}.txt" \
    | awk '$2 != $7 || $3 != $8 || $2 == "-" {print $1}' | tr '\n' ' ')"
  if [[ -z "${diff_heights}" ]]; then
    pass "(d) ${label}: A and B hold the same hash and state root at every height 1..${final}"
  else
    fail "(d) ${label}: A and B differ (hash or state root) at heights: ${diff_heights}"
  fi
  bad_anchors="$(awk '$4 != $5 {print $1}' "${WORK_DIR}/chain-a-${tag}.txt" | tr '\n' ' ')"
  if [[ -z "${bad_anchors}" ]]; then
    pass "(d) ${label}: every canonical anchor 1..${final} on A == zebrad getblockhash(N+B-1)"
  else
    fail "(d) ${label}: A's canonical blocks anchored off zebrad's chain: ${bad_anchors}"
  fi

  ROUND_SUMMARY+=("${label}: H=${h} Z=${z} X=0x${x_hash:0:12}.. X'=0x${xp_hash:0:12}..; refusals ${refusals}; stale block sealed: ${stale_hash:-none}; block ${h}: A ${ha_hash:0:14}.. B ${hb_hash:0:14}..; B state-root rejections ${b_state_root}, anchor holds ${b_held}; heads A=${la} B=${lb}")
  return 0
}

for port in "${PROXY_S_PORT}" "${PROXY_I_PORT}"; do
  if lsof -nP -iTCP:"${port}" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "error: proxy port ${port} already in use; override STALE_RECORD_PROXY_PORTS" >&2
    exit 1
  fi
done
preflight
start_stack

python3 "${HERE}/zebrad-freeze-proxy.py" "${PROXY_S_PORT}" "${ZEBRAD_RPC}" >"${WORK_DIR}/proxy-s.log" 2>&1 &
PROXY_S_PID=$!
python3 "${HERE}/zebrad-freeze-proxy.py" "${PROXY_I_PORT}" "${ZEBRAD_RPC}" >"${WORK_DIR}/proxy-i.log" 2>&1 &
PROXY_I_PID=$!
sleep 1
for url in "${PROXY_S}" "${PROXY_I}"; do
  if ! curl -s -X POST -H 'Content-Type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"getblockcount","params":[]}' "${url}/" | grep -q result; then
    fail "setup: zebrad proxy ${url} not answering"
    exit 1
  fi
done

# ---------------------------------------------------------------------
# Sealing key (SIP-6); fund nothing, burn nothing: every block is null.
# ---------------------------------------------------------------------
MINER_DATA_DIR="${WORK_DIR}/miner"
mkdir -p "${MINER_DATA_DIR}"
"${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest init >"${WORK_DIR}/miner-init.log" 2>&1
TADDR="$(awk '/t-addr to fund/ {print $NF}' "${WORK_DIR}/miner-init.log")"
EVM_ADDR="$(awk '/evm address/ {print $NF}' "${WORK_DIR}/miner-init.log" | head -1)"
if [[ -z "${TADDR}" || -z "${EVM_ADDR}" || ! -f "${MINER_DATA_DIR}/keystore.json" ]]; then
  fail "setup: could not parse the miner identity / find its keystore"
  cat "${WORK_DIR}/miner-init.log" >&2
  exit 1
fi
zc_generate_to_address "${FUND_BLOCKS}" "${TADDR}"
EPOCH_BASE=$(($(zc_tip_height) + 1))
echo "sealer ${EVM_ADDR}; epoch base B=${EPOCH_BASE} (Sova block N anchors Zcash N+${EPOCH_BASE}-1)"

echo "--- starting node A (mine mode, SIP-6 + SIP-7; sealer + C5 via ${PROXY_S}, SIP-7 index via ${PROXY_I}) ---"
A_LOG="${WORK_DIR}/node-a.log"
p2p_env \
  RUST_LOG="${A_RUST_LOG}" \
  SOVA_SIP6=1 \
  SOVA_SIP7=1 \
  SOVA_ZEBRAD_RPC="${PROXY_S}" \
  SOVA_TEST_INDEX_ZEBRAD_RPC="${PROXY_I}" \
  SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" \
  SOVA_SEALER_KEYSTORE="${MINER_DATA_DIR}/keystore.json" \
  SOVA_EPOCH_BASE="${EPOCH_BASE}" \
  SOVA_HTTP_PORT="${A_HTTP_PORT}" \
  SOVA_AUTH_PORT="${A_AUTH_PORT}" \
  SOVA_P2P_PORT="${A_P2P_PORT}" \
  "${SOVA_BIN}" >"${A_LOG}" 2>&1 &
A_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_A}" "${A_PID}" "node A" || exit 1
ENODE_A="$(local_enode "${A_LOG}")" || {
  fail "setup: node A never printed its enode"
  exit 1
}
echo "node A up (pid ${A_PID}); enode ${ENODE_A}"

echo "--- starting node B (follow-only, SIP-6 + SIP-7, zebrad direct, sova/1 static peer = A) ---"
B_LOG="${WORK_DIR}/node-b.log"
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
  "${SOVA_BIN}" >"${B_LOG}" 2>&1 &
B_PID=$!
wait_for_eth_rpc "${ENGINE_RPC_B}" "${B_PID}" "node B" || exit 1
echo "node B up (pid ${B_PID})"

echo ""
echo "=== (0) setup ==="
check_p2p_node_log "node A" "${A_LOG}"
check_p2p_node_log "node B" "${B_LOG}"
if log_has "${A_LOG}" "test hook SOVA_TEST_INDEX_ZEBRAD_RPC: the SIP-7 index follows ${PROXY_I}" 15; then
  pass "setup: node A's SIP-7 index follows ${PROXY_I} (test hook); its sealer and C5 records ${PROXY_S}"
else
  fail "setup: node A's log lacks the test-hook line: this binary has no SOVA_TEST_INDEX_ZEBRAD_RPC, so the race cannot be staged"
  exit 1
fi
if [[ "$(count_in "${B_LOG}" "SOVA_TEST_INDEX_ZEBRAD_RPC")" -eq 0 ]] \
  && log_has "${B_LOG}" "expectations: enforcing settlements" 15; then
  pass "setup: node B enforces C5 against zebrad directly (no test hook)"
else
  fail "setup: node B isn't enforcing C5 against zebrad directly"
fi
log_has "${A_LOG}" "sip-6: sealing as" 15 || true
A_SEALS_AS="$(strip_ansi "${A_LOG}" | grep -o 'sip-6: sealing as 0x[0-9a-fA-F]*' | head -1 | awk '{print tolower($NF)}')"
if [[ -n "${A_SEALS_AS}" && "${A_SEALS_AS}" == "$(tr 'A-F' 'a-f' <<<"${EVM_ADDR}")" ]]; then
  pass "setup: node A seals (SIP-6) as ${EVM_ADDR}"
else
  fail "setup: node A's log lacks 'sip-6: sealing as ${EVM_ADDR}'"
fi
if wait_for_sova_peer "${A_LOG}" 60 && wait_for_sova_peer "${B_LOG}" 60; then
  pass "setup: sova/1 session A<->B established"
else
  fail "setup: no sova/1 session between A and B within 60s"
  exit 1
fi

echo "--- auto-mine every ${AUTO_MINE_INTERVAL_S}s; waiting for Sova height 3 on A and B ---"
start_auto_mine
if ! wait_for_block_number "${ENGINE_RPC_A}" 3 120 || ! wait_for_block_number "${ENGINE_RPC_B}" 3 60; then
  fail "setup: chain did not start (A=$(height_of "${ENGINE_RPC_A}") B=$(height_of "${ENGINE_RPC_B}"))"
  exit 1
fi

for ((round = 1; round <= ROUNDS; round++)); do
  stale_round "round ${round}"
  [[ $? -eq 2 ]] && exit 1
  [[ "${FAILURES}" -gt 0 ]] && break
  start_auto_mine
done

echo ""
echo "=== diagnostics ==="
echo "  epoch base ${EPOCH_BASE}"
for s in "${ROUND_SUMMARY[@]}"; do
  echo "  ${s}"
done
for log in "${A_LOG}" "${B_LOG}"; do
  echo "  ${log##*/}: '${REFUSAL}': $(count_in "${log}" "${REFUSAL}");" \
    "'zcash anchor mismatch': $(count_in "${log}" 'zcash anchor mismatch');" \
    "'built payload rejected': $(count_in "${log}" 'built payload rejected');" \
    "'${STATE_ROOT_REJECT}': $(count_in "${log}" "${STATE_ROOT_REJECT}");" \
    "'expectations and candidates unwound': $(count_in "${log}" 'expectations and candidates unwound')"
done

echo ""
if [[ "${FAILURES}" -eq 0 ]]; then
  echo "STALE RECORD SCENARIO PASSED (${ROUNDS} round(s); all assertions)"
else
  echo "STALE RECORD SCENARIO: ${FAILURES} ASSERTION(S) FAILED" >&2
fi

# Nonzero exit on any failed assertion, so CI (and callers) see it.
[[ "${FAILURES}" -eq 0 ]]
