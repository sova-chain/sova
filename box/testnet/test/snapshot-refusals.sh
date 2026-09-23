#!/usr/bin/env bash
# Refusal-path tests for box/testnet/snapshot.sh (no Docker, no zebrad, ~1 s).
# bats is not a repo dependency, so this is plain bash: each case runs the
# script and checks its exit status and error text.
#
# Usage: box/testnet/test/snapshot-refusals.sh

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd -P)"
SNAP="${HERE}/../snapshot.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/sova-snap-refusals.XXXXXX")"
TMP="$(cd "${TMP}" && pwd -P)"
PIDS=()
cleanup() {
  local p
  for p in "${PIDS[@]+"${PIDS[@]}"}"; do kill "${p}" 2>/dev/null || true; done
  rm -rf "${TMP}"
}
trap cleanup EXIT

pass=0
failed=0
HASH=00000c62774299a26bd3448c99f06e446968c0c38847743025ff27bea1f8c66f

# expect_fail <name> <substring of stderr> <cmd...>
expect_fail() {
  local name="$1" want="$2" err
  shift 2
  if "$@" >"${TMP}/.stdout" 2>"${TMP}/.stderr"; then
    echo "FAIL  ${name}: succeeded, expected refusal"
    failed=$((failed + 1))
    return
  fi
  err="$(cat "${TMP}/.stderr")"
  if [[ "${err}" == *"${want}"* ]]; then
    echo "ok    ${name}"
    pass=$((pass + 1))
  else
    echo "FAIL  ${name}: wrong error: ${err}"
    failed=$((failed + 1))
  fi
}

expect_ok() {
  local name="$1"
  shift
  if "$@" >"${TMP}/.stdout" 2>"${TMP}/.stderr"; then
    echo "ok    ${name}"
    pass=$((pass + 1))
  else
    echo "FAIL  ${name}: $(cat "${TMP}/.stderr")"
    failed=$((failed + 1))
  fi
}

# A fake stopped zebrad cache_dir: shape of state/v28/testnet + backup + things
# that must never be archived (cookie, peers, RocksDB LOG).
mkstate() {
  local d="$1" net="${2:-testnet}"
  mkdir -p "${d}/state/v28/${net}" "${d}/non_finalized_state/${net}" "${d}/network"
  echo "MANIFEST-000005" >"${d}/state/v28/${net}/CURRENT"
  echo "28.0.0" >"${d}/state/v28/${net}/version"
  head -c 4096 /dev/urandom >"${d}/state/v28/${net}/000010.sst"
  : >"${d}/state/v28/${net}/MANIFEST-000005"
  : >"${d}/state/v28/${net}/LOCK"
  echo "rocksdb info log" >"${d}/state/v28/${net}/LOG"
  head -c 512 /dev/urandom >"${d}/non_finalized_state/${net}/${HASH}"
  echo "__cookie__:secret" >"${d}/.cookie"
  echo "203.0.113.7:18233" >"${d}/network/${net}.peers"
}

S="${TMP}/state"
mkstate "${S}"
LOCKF="${S}/state/v28/testnet/LOCK"

echo "== create refusals"

# 1. A process holding RocksDB's fcntl lock (what a running zebrad does).
python3 - "${LOCKF}" <<'PY' &
import fcntl, sys, time
f = open(sys.argv[1], "r+b")
fcntl.lockf(f, fcntl.LOCK_EX)
time.sleep(60)
PY
holder=$!
PIDS+=("${holder}")
sleep 0.5
expect_fail "create: db locked by a running process" "LOCKED" \
  "${SNAP}" create "${S}" "${TMP}/o1" --height 100 --hash "${HASH}"
kill "${holder}" 2>/dev/null || true
wait "${holder}" 2>/dev/null || true

# 2. A process with LOCK open but no lock (lsof path).
if command -v lsof >/dev/null 2>&1; then
  sleep 60 3<"${LOCKF}" &
  holder=$!
  PIDS+=("${holder}")
  sleep 0.3
  expect_fail "create: LOCK held open by a process" "has ${LOCKF} open" \
    "${SNAP}" create "${S}" "${TMP}/o2" --height 100 --hash "${HASH}"
  kill "${holder}" 2>/dev/null || true
  wait "${holder}" 2>/dev/null || true
fi

# 3. The node's RPC still answers (--rpc).
python3 - "${TMP}/port" <<'PY' &
import http.server, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = b'{"jsonrpc":"2.0","id":"x","result":5}'
        self.send_response(200); self.send_header("Content-Length", str(len(body))); self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass
s = http.server.HTTPServer(("127.0.0.1", 0), H)
open(sys.argv[1], "w").write(str(s.server_address[1]))
s.serve_forever()
PY
PIDS+=($!)
disown
for _ in 1 2 3 4 5 6 7 8 9 10; do [[ -s "${TMP}/port" ]] && break; sleep 0.2; done
expect_fail "create: RPC still answers" "still answers" \
  "${SNAP}" create "${S}" "${TMP}/o3" --height 100 --hash "${HASH}" --rpc "http://127.0.0.1:$(cat "${TMP}/port")"

expect_fail "create: no height/hash" "need the tip" "${SNAP}" create "${S}" "${TMP}/o4"
expect_fail "create: bad hash" "not a 32-byte hex" "${SNAP}" create "${S}" "${TMP}/o5" --height 1 --hash 0xzz
mkdir -p "${TMP}/busy" && touch "${TMP}/busy/x"
expect_fail "create: non-empty out dir" "is not empty" \
  "${SNAP}" create "${S}" "${TMP}/busy" --height 100 --hash "${HASH}"
echo '{"network":"regtest","height":100,"hash":"'"${HASH}"'"}' >"${TMP}/cap-regtest.json"
expect_fail "create: capture network != state network" "no database" \
  "${SNAP}" create "${S}" "${TMP}/o6" --capture "${TMP}/cap-regtest.json"
M="${TMP}/mainstate"
mkstate "${M}" mainnet
expect_fail "create: mainnet refused" "mainnet snapshots are not published" \
  "${SNAP}" create "${M}" "${TMP}/o7" --height 100 --hash "${HASH}"
expect_fail "create: not a cache_dir" "no state/ subdirectory" \
  "${SNAP}" create "${TMP}/busy" "${TMP}/o8" --height 100 --hash "${HASH}"

echo "== create (happy path on the fake state)"
OUT="${TMP}/out"
expect_ok "create: stopped node" "${SNAP}" create "${S}" "${OUT}" --height 100 --hash "0x${HASH}" --zebra-version v6.3.0
ARCH="${OUT}/zebrad-testnet-100.tar.zst"
[[ -f "${ARCH}" ]] || ARCH="${OUT}/zebrad-testnet-100.tar.gz"
if [[ -f "${ARCH}" ]]; then
  if [[ "${ARCH}" == *.zst ]]; then
    listing="$(zstd -q -dc "${ARCH}" | tar -tf -)"
  else
    listing="$(gzip -dc "${ARCH}" | tar -tf -)"
  fi
  if [[ "${listing}" == *cookie* || "${listing}" == *peers* || "${listing}" == *LOCK* || "${listing}" == */LOG* ]]; then
    echo "FAIL  create: archive leaked excluded files: ${listing}"
    failed=$((failed + 1))
  else
    echo "ok    create: archive has no cookie/peers/LOCK/LOG"
    pass=$((pass + 1))
  fi
else
  echo "FAIL  create: no archive"
  failed=$((failed + 1))
fi

echo "== restore refusals"
mkdir -p "${TMP}/full" && touch "${TMP}/full/keep"
expect_fail "restore: non-empty target" "is not empty" "${SNAP}" restore "${ARCH}" "${TMP}/full"

BAD="${TMP}/bad"
mkdir -p "${BAD}"
cp "${ARCH}" "${OUT}/SHA256SUMS" "${OUT}/snapshot.json" "${BAD}/"
printf 'x' >>"${BAD}/$(basename "${ARCH}")"
expect_fail "restore: bad checksum (tampered archive)" "CHECKSUM MISMATCH" \
  "${SNAP}" restore "${BAD}/$(basename "${ARCH}")" "${TMP}/r1"
[[ ! -e "${TMP}/r1" ]] && echo "ok    restore: nothing created after a bad checksum" && pass=$((pass + 1))

NOSUMS="${TMP}/nosums"
mkdir -p "${NOSUMS}" && cp "${ARCH}" "${NOSUMS}/"
expect_fail "restore: missing SHA256SUMS" "no SHA256SUMS" "${SNAP}" restore "${NOSUMS}/$(basename "${ARCH}")" "${TMP}/r2"

WRONGM="${TMP}/wrongmanifest"
mkdir -p "${WRONGM}" && cp "${ARCH}" "${OUT}/SHA256SUMS" "${WRONGM}/"
jq '.sha256 = "00"' "${OUT}/snapshot.json" >"${WRONGM}/snapshot.json"
expect_fail "restore: manifest from another snapshot" "different snapshots" \
  "${SNAP}" restore "${WRONGM}/$(basename "${ARCH}")" "${TMP}/r3"

# A correctly-checksummed archive with an unexpected entry (and a symlink).
EVIL="${TMP}/evil"
mkdir -p "${EVIL}/src/state/v28/testnet" "${EVIL}/pub"
echo 28.0.0 >"${EVIL}/src/state/v28/testnet/version"
echo "#!/bin/sh" >"${EVIL}/src/run-me.sh"
ln -s /etc/passwd "${EVIL}/src/state/v28/testnet/link"
(cd "${EVIL}/src" && COPYFILE_DISABLE=1 tar -czf "${EVIL}/pub/zebrad-testnet-1.tar.gz" state run-me.sh)
(cd "${EVIL}/pub" && sums="$(shasum -a 256 zebrad-testnet-1.tar.gz)" && echo "${sums}" >SHA256SUMS)
expect_fail "restore: unexpected entry in archive" "unexpected top-level entry" \
  "${SNAP}" restore "${EVIL}/pub/zebrad-testnet-1.tar.gz" "${TMP}/r4"

echo "== restore (happy path)"
expect_ok "restore: good snapshot" "${SNAP}" restore "${ARCH}" "${TMP}/r5"
if [[ -f "${TMP}/r5/state/v28/testnet/000010.sst" && -f "${TMP}/r5/non_finalized_state/testnet/${HASH}" &&
  ! -e "${TMP}/r5/.cookie" && ! -e "${TMP}/r5/network" ]]; then
  echo "ok    restore: db + backup restored, no cookie/peers"
  pass=$((pass + 1))
else
  echo "FAIL  restore: unexpected restored tree"
  failed=$((failed + 1))
fi

echo "== gzip fallback (zstd hidden from PATH)"
NOZ="${TMP}/nozstd-bin"
mkdir -p "${NOZ}"
for tool in python3 jq curl lsof shasum; do
  t="$(command -v "${tool}" || true)"
  [[ -n "${t}" ]] && ln -s "${t}" "${NOZ}/${tool}"
done
if [[ ! -x /usr/bin/zstd && ! -x /bin/zstd ]]; then
  expect_ok "create: falls back to gzip" env PATH="${NOZ}:/usr/bin:/bin:/usr/sbin:/sbin" \
    "${SNAP}" create "${S}" "${TMP}/outgz" --height 100 --hash "${HASH}"
  if [[ -f "${TMP}/outgz/zebrad-testnet-100.tar.gz" && "$(cat "${TMP}/.stderr")" == *"falling back to gzip"* ]]; then
    echo "ok    create: wrote .tar.gz and said so"
    pass=$((pass + 1))
  else
    echo "FAIL  create: gzip fallback output"
    failed=$((failed + 1))
  fi
  expect_ok "restore: .tar.gz" env PATH="${NOZ}:/usr/bin:/bin:/usr/sbin:/sbin" \
    "${SNAP}" restore "${TMP}/outgz/zebrad-testnet-100.tar.gz" "${TMP}/r6"
else
  echo "skip  gzip fallback (zstd is in a system bin dir here)"
fi

echo
echo "${pass} passed, ${failed} failed"
((failed == 0))
