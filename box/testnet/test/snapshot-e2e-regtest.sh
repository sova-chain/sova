#!/usr/bin/env bash
# End-to-end snapshot round trip on a disposable REGTEST zebrad in Docker:
#
#   node A (fresh state) -> generate blocks -> capture height/hash via RPC
#   -> create refused while A runs -> stop A -> create snapshot
#   -> restore into a fresh dir -> node B on the restored state
#   -> verify: B's tip height/hash == captured, block H hash == manifest.
#
# Uses project sova-snap, container sova-zebrad-snap, RPC 127.0.0.1:18352 and a
# temp dir on the internal disk (a few MB). Tears everything down on exit;
# KEEP=1 keeps the temp dir for inspection.
#
# Usage: box/testnet/test/snapshot-e2e-regtest.sh [num_blocks]   (default 150)

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd -P)"
SNAP="${HERE}/../snapshot.sh"
BLOCKS="${1:-150}"
RPC="http://127.0.0.1:18352"
PROJECT=sova-snap

say() { echo "==> $*"; }
fail() {
  echo "E2E FAIL: $*" >&2
  exit 1
}

avail_kb="$(df -Pk / | awk 'NR==2{print $4}')"
((avail_kb > 6 * 1024 * 1024)) || fail "less than 6 GB free on /; not running"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/sova-snap-e2e.XXXXXX")"
TMP="$(cd "${TMP}" && pwd -P)"

compose() { SNAP_CACHE_DIR="${SNAP_CACHE_DIR:-${TMP}/a}" docker compose -p "${PROJECT}" -f "${HERE}/docker-compose.yml" "$@"; }

cleanup() {
  compose down -t 60 >/dev/null 2>&1 || true
  if [[ "${KEEP:-0}" == 1 ]]; then
    echo "kept ${TMP}"
  else
    rm -rf "${TMP}"
  fi
}
trap cleanup EXIT

rpc() {
  curl -sS -m "${RPC_TIMEOUT:-10}" -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"e2e\",\"method\":\"$1\",\"params\":${2:-[]}}" "${RPC}/"
}

wait_rpc() {
  local i body
  for ((i = 0; i < 90; i++)); do
    body="$(rpc getblockcount 2>/dev/null || true)"
    if [[ "${body}" == *'"result"'* ]]; then return 0; fi
    sleep 1
  done
  compose logs --tail 50 >&2 || true
  fail "zebrad RPC did not come up on ${RPC}"
}

tip() {
  local b
  b="$(rpc getblockchaininfo)"
  jq -r '"\(.result.blocks) \(.result.bestblockhash)"' <<<"${b}"
}

# ---- node A
mkdir -p "${TMP}/a"
say "starting node A (state: ${TMP}/a)"
SNAP_CACHE_DIR="${TMP}/a" compose up -d >/dev/null
wait_rpc
say "generating ${BLOCKS} blocks"
for ((done_blocks = 0; done_blocks < BLOCKS; done_blocks += 10)); do
  n=$((BLOCKS - done_blocks < 10 ? BLOCKS - done_blocks : 10))
  gen="$(RPC_TIMEOUT=300 rpc generate "[${n}]")"
  [[ "$(jq -r '.result | length' <<<"${gen}")" == "${n}" ]] || fail "generate failed: ${gen}"
done

say "capture"
"${SNAP}" capture --rpc "${RPC}" --out "${TMP}/capture.json"
cap_h="$(jq -r .height "${TMP}/capture.json")"
cap_hash="$(jq -r .hash "${TMP}/capture.json")"
[[ "${cap_h}" == "${BLOCKS}" ]] || fail "captured height ${cap_h}, expected ${BLOCKS}"

say "create while node A runs: must be refused"
if "${SNAP}" create "${TMP}/a" "${TMP}/out-refused" \
  --capture "${TMP}/capture.json" --rpc "${RPC}" 2>"${TMP}/refused.err"; then
  fail "create succeeded against a running node"
fi
refusal="$(tail -1 "${TMP}/refused.err")"
[[ "${refusal}" == *"running container"* ]] ||
  fail "create failed for the wrong reason: ${refusal}"
echo "    refused: ${refusal}"

say "waiting 10 s for the non-finalized backup, then stopping node A"
sleep 10
SNAP_CACHE_DIR="${TMP}/a" compose down -t 60 >/dev/null
echo "    node A state:"
(cd "${TMP}/a" && find . -maxdepth 2 | LC_ALL=C sort | sed 's/^/      /')
echo "      non_finalized_state/regtest: $(find "${TMP}/a/non_finalized_state/regtest" -type f | wc -l | tr -d ' ') block files"

say "create"
"${SNAP}" create "${TMP}/a" "${TMP}/out" --capture "${TMP}/capture.json" --rpc "${RPC}"
archive="$(ls "${TMP}"/out/zebrad-regtest-*.tar.*)"
echo "    SHA256SUMS: $(cat "${TMP}/out/SHA256SUMS")"
jq . "${TMP}/out/snapshot.json"
listing="$(zstd -q -dc "${archive}" | tar -tf -)"
if [[ "${listing}" == *".cookie"* || "${listing}" == *"peers"* || "${listing}" == *"/LOG"* || "${listing}" == *"/LOCK"* ]]; then
  fail "archive contains files it must not: ${listing}"
fi
echo "    archive entries: $(wc -l <<<"${listing}" | tr -d ' ') (no cookie/peers/LOG/LOCK)"

say "restore into a fresh dir"
"${SNAP}" restore "${archive}" "${TMP}/b"

say "starting node B on the restored state"
SNAP_CACHE_DIR="${TMP}/b" compose up -d >/dev/null
wait_rpc
read -r b_h b_hash <<<"$(tip)"
echo "    captured on A: ${cap_h} ${cap_hash}"
echo "    tip on B:      ${b_h} ${b_hash}"
[[ "${b_h}" == "${cap_h}" && "${b_hash}" == "${cap_hash}" ]] || fail "restored tip differs from captured tip"

say "verify"
"${SNAP}" verify --rpc "${RPC}" --manifest "${TMP}/out/snapshot.json"

say "node B finalized/non-finalized split (from its log)"
logs="$(SNAP_CACHE_DIR="${TMP}/b" compose logs 2>&1 || true)"
grep -E -m3 'restor|checkpoint validation|database format is valid' <<<"${logs}" | cut -c1-200 || true

say "stopping node B"
SNAP_CACHE_DIR="${TMP}/b" compose down -t 60 >/dev/null
echo "E2E PASS: height ${cap_h} hash ${cap_hash} sha256 $(jq -r .sha256 "${TMP}/out/snapshot.json")"
