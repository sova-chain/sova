#!/usr/bin/env bash
# box/up/test-prebuilt-release.sh -- offline test of box/up.sh's GitHub
# Release source for prebuilt binaries (source (a); see box/up/README.md,
# "Prebuilt binaries").
#
# Serves fake releases from a temp dir over `python3 -m http.server` on a
# free loopback port, points `./box/up.sh binaries` at it with
# SOVA_BOX_RELEASE_BASE_URL, and checks each case ends where it should:
#
#   happy        valid release            -> installed from the release
#   tag-describe checkout exactly at a v* tag, no SOVA_BOX_RELEASE
#                                          -> installed from the release
#   no-tag       no tag, no override       -> skips (a), goes on to (b)
#   bad-sum      tarball tampered after SHA256SUMS -> rejected, (b) next
#   wrong-plat   release lists only the other platform -> rejected
#   wrong-inner  tarball's own BUILD-INFO is for the other platform
#                (checksums all valid)     -> rejected by verify_prebuilt
#   foreign      release built from a commit not in this clone -> rejected
#   missing      no tarball for this platform -> rejected, (b) next
#   to-source    bad-sum again in auto mode -> falls through (b) to (c),
#                a source build (cargo stubbed: no 11-minute build here);
#                the checksum problem is printed, the logged-out `gh` is not
#   auto-quiet   no tag, auto mode          -> one neutral line, none of the
#                per-source "nothing published" reasons, then (c)
#
# The binaries are tiny shell scripts, `gh` is stubbed as logged-out so
# source (b) fails fast without touching GitHub, and every case installs
# into its own temp CARGO_TARGET_DIR, so nothing outside a temp dir is
# written and no Docker, zebrad or chain port is used.
#
# Usage: box/up/test-prebuilt-release.sh   (exit 0 = all cases passed)

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HERE}/../.." && pwd)"
UP="${REPO_ROOT}/box/up.sh"

case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) PLATFORM=darwin-arm64 OTHER=linux-x86_64 ;;
  Linux/x86_64) PLATFORM=linux-x86_64 OTHER=darwin-arm64 ;;
  *)
    echo "skip: box/up.sh publishes no prebuilt binaries for $(uname -s)/$(uname -m)"
    exit 0
    ;;
esac

HEAD_SHA="$(git -C "${REPO_ROOT}" rev-parse HEAD)"
if ! git -C "${REPO_ROOT}" diff --quiet HEAD -- bin crates Cargo.toml Cargo.lock; then
  echo "error: uncommitted Rust changes; the fake release is 'built' from HEAD, so they must match" >&2
  exit 1
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sova-prebuilt-test.XXXXXX")"
SERVE="${WORK}/serve"
SERVER_PID=""
cleanup() {
  if [[ -n "${SERVER_PID}" ]]; then
    kill "${SERVER_PID}" 2>/dev/null || true
    wait "${SERVER_PID}" 2>/dev/null || true
  fi
  rm -rf -- "${WORK}"
}
trap cleanup EXIT
mkdir -p "${SERVE}" "${WORK}/stubbin"

sha256_of() {
  local out
  if command -v sha256sum >/dev/null 2>&1; then
    out="$(sha256sum "$1")"
  else
    out="$(shasum -a 256 "$1")"
  fi
  printf '%s\n' "${out%% *}"
}

# `gh` that is installed but logged out: source (b) fails at `gh auth
# status`, offline.
cat >"${WORK}/stubbin/gh" <<'EOF'
#!/bin/sh
echo "stub gh: not logged in" >&2
exit 1
EOF
# `cargo` that answers `metadata` for real (box/up.sh resolves the target
# dir with it) but refuses to build, so reaching source (c) is visible and
# cheap.
REAL_CARGO="$(command -v cargo)"
cat >"${WORK}/stubbin/cargo" <<EOF
#!/bin/sh
if [ "\$1" = build ]; then
  echo "STUB-CARGO-BUILD \$*"
  exit 3
fi
exec "${REAL_CARGO}" "\$@"
EOF
chmod 0755 "${WORK}/stubbin/gh" "${WORK}/stubbin/cargo"

# make_release <tag> <commit> <release-platforms> <inner-platform>
# Writes <SERVE>/<tag>/{SHA256SUMS,BUILD-INFO,sova-box-bin-$PLATFORM.tar.gz}
# the way box-binaries.yml's release job lays them out.
make_release() {
  local tag="$1" commit="$2" rel_platforms="$3" inner_platform="$4"
  local stage="${WORK}/stage-${tag}" dir="${SERVE}/${tag}"
  mkdir -p "${stage}" "${dir}"
  printf '#!/bin/sh\necho "sova fake %s"\n' "${tag}" >"${stage}/sova"
  printf '#!/bin/sh\necho "sova-miner 0.0.0-fake %s"\n' "${tag}" >"${stage}/sova-miner"
  chmod 0755 "${stage}/sova" "${stage}/sova-miner"
  (cd "${stage}" && {
    printf '%s  sova\n' "$(sha256_of sova)"
    printf '%s  sova-miner\n' "$(sha256_of sova-miner)"
  } >SHA256SUMS)
  printf 'commit=%s\nplatform=%s\nref=%s\nrustc=fake\nrun_id=0\n' \
    "${commit}" "${inner_platform}" "${tag}" >"${stage}/BUILD-INFO"
  COPYFILE_DISABLE=1 tar -czf "${dir}/sova-box-bin-${PLATFORM}.tar.gz" \
    -C "${stage}" sova sova-miner SHA256SUMS BUILD-INFO
  (cd "${dir}" && printf '%s  %s\n' "$(sha256_of "sova-box-bin-${PLATFORM}.tar.gz")" \
    "sova-box-bin-${PLATFORM}.tar.gz" >SHA256SUMS)
  printf 'commit=%s\ntag=%s\nplatforms=%s\nrun_id=0\n' \
    "${commit}" "${tag}" "${rel_platforms}" >"${dir}/BUILD-INFO"
}

make_release v0.0.0-happy "${HEAD_SHA}" "darwin-arm64 linux-x86_64" "${PLATFORM}"
make_release v0.0.0-badsum "${HEAD_SHA}" "darwin-arm64 linux-x86_64" "${PLATFORM}"
printf 'tampered' >>"${SERVE}/v0.0.0-badsum/sova-box-bin-${PLATFORM}.tar.gz"
make_release v0.0.0-wrongplat "${HEAD_SHA}" "${OTHER}" "${PLATFORM}"
make_release v0.0.0-wronginner "${HEAD_SHA}" "darwin-arm64 linux-x86_64" "${OTHER}"
make_release v0.0.0-foreign "0123456789abcdef0123456789abcdef01234567" "darwin-arm64 linux-x86_64" "${PLATFORM}"
make_release v0.0.0-missing "${HEAD_SHA}" "darwin-arm64 linux-x86_64" "${PLATFORM}"
rm -f "${SERVE}/v0.0.0-missing/sova-box-bin-${PLATFORM}.tar.gz"
make_release v0.0.0-describe "${HEAD_SHA}" "darwin-arm64 linux-x86_64" "${PLATFORM}"

PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
python3 -m http.server "${PORT}" --bind 127.0.0.1 --directory "${SERVE}" \
  >"${WORK}/http.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 50); do
  curl -fsS -o /dev/null "http://127.0.0.1:${PORT}/v0.0.0-happy/BUILD-INFO" 2>/dev/null && break
  sleep 0.1
done
BASE_URL="http://127.0.0.1:${PORT}"
echo "fake releases served from ${SERVE} at ${BASE_URL} (host platform ${PLATFORM}, commit ${HEAD_SHA:0:12})"

FAILED=0

# run_case <name> <up.sh> <expected exit: 0|nonzero> <env...> -- <regex>...
# Each regex must match the output; one written as !<regex> must not.
run_case() {
  local name="$1" up="$2" want="$3" rc=0 out target pat
  shift 3
  local envs=()
  while [[ $# -gt 0 && "$1" != "--" ]]; do
    envs+=("$1")
    shift
  done
  shift
  target="${WORK}/target-${name}"
  out="${WORK}/out-${name}.log"
  env PATH="${WORK}/stubbin:${PATH}" CARGO_TARGET_DIR="${target}" \
    SOVA_BOX_RELEASE_BASE_URL="${BASE_URL}" "${envs[@]}" \
    "${up}" binaries >"${out}" 2>&1 || rc=$?
  local ok=1
  if [[ "${want}" == 0 && ${rc} -ne 0 ]] || [[ "${want}" != 0 && ${rc} -eq 0 ]]; then
    ok=0
    echo "  exit ${rc}, wanted ${want}"
  fi
  for pat in "$@"; do
    if [[ "${pat}" == '!'* ]]; then
      if grep -Eq -- "${pat#!}" "${out}"; then
        ok=0
        echo "  unexpected: ${pat#!}"
      fi
    elif ! grep -Eq -- "${pat}" "${out}"; then
      ok=0
      echo "  missing: ${pat}"
    fi
  done
  if [[ "${want}" == 0 ]]; then
    if [[ "$("${target}/release/sova-miner" --version 2>/dev/null || true)" != "sova-miner 0.0.0-fake"* ]]; then
      ok=0
      echo "  installed sova-miner is not the release's"
    fi
  elif [[ -e "${target}/release/sova" || -e "${target}/release/sova-miner" ]]; then
    ok=0
    echo "  a rejected release still installed binaries"
  fi
  if [[ ${ok} -eq 1 ]]; then
    echo "PASS ${name} (exit ${rc})"
  else
    echo "FAIL ${name} (exit ${rc}); output:"
    sed 's/^/    | /' "${out}"
    FAILED=1
  fi
  grep -E '^(prebuilt|bin/sova|sova-miner|error|STUB)' "${out}" | sed 's/^/    > /' || true
}

run_case happy "${UP}" 0 SOVA_BOX_PREBUILT=1 SOVA_BOX_RELEASE=v0.0.0-happy -- \
  "SHA256SUMS OK, platform ${PLATFORM}" \
  "bin/sova: PREBUILT \(release v0.0.0-happy" \
  "sova-miner: PREBUILT \(release v0.0.0-happy"

run_case no-tag "${UP}" 1 SOVA_BOX_PREBUILT=1 SOVA_BOX_RELEASE= -- \
  "not exactly at a v\* release tag" \
  "not authenticated" \
  "SOVA_BOX_PREBUILT=1 but no usable prebuilt"

run_case bad-sum "${UP}" 1 SOVA_BOX_PREBUILT=1 SOVA_BOX_RELEASE=v0.0.0-badsum -- \
  "SHA-256 mismatch" "not authenticated"

run_case wrong-plat "${UP}" 1 SOVA_BOX_PREBUILT=1 SOVA_BOX_RELEASE=v0.0.0-wrongplat -- \
  "lists platforms '${OTHER}', not ${PLATFORM}" "not authenticated"

run_case wrong-inner "${UP}" 1 SOVA_BOX_PREBUILT=1 SOVA_BOX_RELEASE=v0.0.0-wronginner -- \
  "artifact is for '${OTHER}', this host is ${PLATFORM}" "not authenticated"

run_case foreign "${UP}" 1 SOVA_BOX_PREBUILT=1 SOVA_BOX_RELEASE=v0.0.0-foreign -- \
  "does not have this checkout's Rust sources" "not authenticated"

run_case missing "${UP}" 1 SOVA_BOX_PREBUILT=1 SOVA_BOX_RELEASE=v0.0.0-missing -- \
  "has no sova-box-bin-${PLATFORM}.tar.gz" "not authenticated"

run_case to-source "${UP}" 1 SOVA_BOX_PREBUILT=auto SOVA_BOX_RELEASE=v0.0.0-badsum -- \
  "SHA-256 mismatch" '!not authenticated' \
  "no published binaries match this checkout; building from source" \
  "bin/sova: BUILDING FROM SOURCE" "STUB-CARGO-BUILD build --release -p sova"

run_case auto-quiet "${UP}" 1 SOVA_BOX_PREBUILT=auto SOVA_BOX_RELEASE= -- \
  "no published binaries match this checkout; building from source" \
  '!not exactly at a v\* release tag' '!not authenticated' '!could not tell which GitHub repo' \
  "bin/sova: BUILDING FROM SOURCE" "STUB-CARGO-BUILD build --release -p sova"

# Tag from `git describe`: a throwaway local clone (objects shared, the tag
# never leaves ${WORK}) with this checkout's box/up.sh, tagged at HEAD.
CLONE="${WORK}/clone"
git clone -q --shared "${REPO_ROOT}" "${CLONE}"
git -C "${CLONE}" checkout -q "${HEAD_SHA}"
cp "${UP}" "${CLONE}/box/up.sh"
git -C "${CLONE}" -c user.name=test -c user.email=test@invalid tag v0.0.0-describe "${HEAD_SHA}"
run_case tag-describe "${CLONE}/box/up.sh" 0 SOVA_BOX_PREBUILT=1 -- \
  "fetching release v0.0.0-describe" \
  "bin/sova: PREBUILT \(release v0.0.0-describe"

if [[ ${FAILED} -ne 0 ]]; then
  echo "SOME CASES FAILED"
  exit 1
fi
echo "all prebuilt-release cases passed"
