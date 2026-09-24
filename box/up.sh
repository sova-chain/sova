#!/usr/bin/env bash
# box/up.sh -- sova-in-a-box, v1 (E1): the one-command mining chain.
#
# v1 is a HYBRID stack, by design:
#   - zebrad (Zcash regtest) runs in Docker, reusing box/regtest's already
#     proven compose harness (C1).
#   - bin/sova (mine mode) and sova-miner run as HOST processes, in
#     release mode.
#
# Why hybrid and not fully containerized: bin/sova links reth, a large
# dependency graph (~20GB resolved on this host between the Cargo registry
# cache and build artifacts). Building that fresh inside a linux/arm64
# container -- a second, independent dependency graph and target/ dir --
# would duplicate all of it on top of an already-tight disk budget and can
# abort the build with ENOSPC. Building natively on the host instead reuses
# the Cargo registry cache and target/ directories that already exist here,
# so the only new disk cost is the release build's own object/binary
# output, not a second full toolchain download + full recompile. Native
# host binaries also can't run inside a Linux container on this (Apple
# Silicon macOS) box without a cross-compile or a Linux build stage, which
# is out of scope for v1. Full containerization (a Dockerfile + compose
# stack for bin/sova and sova-miner) is a follow-up once either a Linux
# build host or CI-built images are available -- see box/up/README.md.
#
# `sova-miner` itself has never linked reth (see the standing rule in
# crates/burn-wallet/miner/Cargo.toml's header comment), so building it on
# the host has no such conflict either way.
#
# Usage:
#   ./box/up.sh              # bring the whole stack up (on first run,
#                              downloads CI-built release binaries for
#                              this exact source if it can, else builds
#                              them), then prints
#                              how to watch it mine
#   ./box/up.sh down          # stop the host processes and the zebrad
#                              container, remove this run's node datadir;
#                              clean teardown
#   ./box/up.sh status        # liveness check of a running stack: process
#                              state, decimal block height, miner's SOVA
#                              balance, settled-epoch count
#   ./box/up.sh binaries      # only step [1/6]: download (or build) the two
#                              release binaries; no Docker, no ports, no
#                              processes started
#
# Order: `up` gets the binaries first and only then starts zebrad, so a
# first-run source build (11-25+ min) never holds a container or a port,
# and a build that fails or is Ctrl-C'd leaves nothing running.
#
# Tunables (env vars, all optional):
#   SOVA_BOX_FUND_BLOCKS       blocks generated straight to the miner's
#                               own address to mature its first coinbase
#                               (default 101, matches box/regtest's own
#                               100-confirmation maturity window)
#   SOVA_BOX_PER_EPOCH_ZAT      zatoshis burned to the SIP-1 eater script
#                               per epoch (default 100000)
#   SOVA_BOX_BUDGET_ZAT         total zatoshis (burn + fee, summed across
#                               every epoch) the continuous miner loop may
#                               spend before it stops itself (default
#                               6000000 -- tens of epochs' headroom)
#   SOVA_BOX_AUTO_MINE_INTERVAL seconds between auto-mined Zcash blocks
#                               (default 3, same default as auto-mine.sh)
#   SOVA_BOX_ZEBRAD_PORT        host port for zebrad's RPC (default 18232)
#   SOVA_BOX_RPC_PORT           Sova HTTP JSON-RPC port (default 8545)
#   SOVA_BOX_RPC_CORS           browser origins the Sova RPC allows, passed
#                               to bin/sova as SOVA_RPC_CORS (default "*",
#                               so sova.io's /pulse and /ashwings pages work
#                               against the box with ?rpc=; set it empty to
#                               send no CORS headers)
#   SOVA_BOX_AUTH_PORT          Sova authrpc (Engine API) port (default 8551)
#   SOVA_BOX_WS_PORT            Sova WS JSON-RPC port, passed to bin/sova as
#                               SOVA_WS_PORT (default: unset = no WS, as
#                               before); needed for SIP-7's
#                               sova_subscribe("zcashBlocks")
#   SOVA_BOX_SIP7               1 = run bin/sova with SOVA_SIP7=1 (SIP-7
#                               pool reads on 0x...5A00 and the
#                               sova_getZcashBlocks feed); default 0 (off)
#   SOVA_BOX_P2P_PORT           Sova p2p port (default 30303)
#   SOVA_BOX_ZEBRAD_CONTAINER   zebrad container name
#                               (default sova-zebrad-regtest)
#   SOVA_BOX_COMPOSE_PROJECT    compose project name (default "regtest"
#                               when the container name is the default,
#                               otherwise the container name, so a second
#                               box never touches the first one's project)
#   CARGO_TARGET_DIR            honored: binary paths are resolved with
#                               `cargo metadata`, per workspace, never
#                               assumed to be ./target
#   SOVA_BOX_PREBUILT           where missing release binaries come from
#                               (E1c): `auto` (default) = prebuilt if one
#                               of the same Rust sources can be fetched --
#                               first a GitHub Release asset (plain curl,
#                               no login), then a CI Actions artifact (via
#                               `gh`) -- else build from source;
#                               `1` = prebuilt only (fail instead of
#                               building); `0` = always build from source
#   SOVA_BOX_RELEASE            release tag to fetch binaries from (e.g.
#                               v0.1.0); default: the v* tag this checkout
#                               is exactly at (`git describe --tags
#                               --exact-match`), if any
#   SOVA_BOX_REPO               owner/name of the public GitHub repo whose
#                               Releases hold the binaries
#                               (default sova-chain/sova)
#   SOVA_BOX_RELEASE_BASE_URL   base URL release assets are fetched from,
#                               as <base>/<tag>/<asset> (default
#                               https://github.com/$SOVA_BOX_REPO/releases/download);
#                               for mirrors and tests
#   SOVA_BOX_PREBUILT_DIR       use this local directory (laid out like
#                               the CI artifact: sova, sova-miner,
#                               SHA256SUMS, BUILD-INFO) as the prebuilt
#                               source instead of GitHub; same checks
#   SOVA_BOX_PREBUILT_REPO      owner/name of the GitHub repo to fetch the
#                               Actions artifact from (default: what `gh`
#                               infers from this checkout's remote)
#
# `up` records the port/container settings it used in box/up/.run/box.env;
# `down` and `status` read them back, so they act on the stack that was
# actually started without the env vars having to be repeated.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" # .../sova-chain/box
REPO_ROOT="$(cd "${HERE}/.." && pwd)"
REGTEST_DIR="${HERE}/regtest"
BURN_WALLET_DIR="${REPO_ROOT}/crates/burn-wallet"
RUN_DIR="${HERE}/up/.run"
MINER_DATA_DIR="${RUN_DIR}/miner"
LOG_DIR="${RUN_DIR}/logs"
PID_DIR="${RUN_DIR}/pids"
BOX_ENV_FILE="${RUN_DIR}/box.env"
# Path of the temp dir this box run gave bin/sova as its TMPDIR. bin/sova
# (mine mode) uses reth's `testing_node`, which puts its datadir in a fresh
# `$TMPDIR/reth-test-XXXX/` and never removes it; pointing TMPDIR at a dir
# we created and recorded lets `down` remove exactly that one and nothing
# else.
NODE_TMPDIR_FILE="${RUN_DIR}/node-tmpdir"
NODE_TMPDIR_PREFIX="sova-box-node."

FUND_BLOCKS="${SOVA_BOX_FUND_BLOCKS:-101}"
PER_EPOCH_ZAT="${SOVA_BOX_PER_EPOCH_ZAT:-100000}"
BUDGET_ZAT="${SOVA_BOX_BUDGET_ZAT:-6000000}"
AUTO_MINE_INTERVAL="${SOVA_BOX_AUTO_MINE_INTERVAL:-3}"

DEFAULT_ZEBRAD_CONTAINER="sova-zebrad-regtest"
ZEBRAD_PORT="${SOVA_BOX_ZEBRAD_PORT:-18232}"
RPC_PORT="${SOVA_BOX_RPC_PORT:-8545}"
# `-` not `:-`: an explicitly empty value turns CORS off.
RPC_CORS="${SOVA_BOX_RPC_CORS-*}"
AUTH_PORT="${SOVA_BOX_AUTH_PORT:-8551}"
WS_PORT="${SOVA_BOX_WS_PORT:-}"
SIP7="${SOVA_BOX_SIP7:-0}"
P2P_PORT="${SOVA_BOX_P2P_PORT:-30303}"
ZEBRAD_CONTAINER="${SOVA_BOX_ZEBRAD_CONTAINER:-${DEFAULT_ZEBRAD_CONTAINER}}"
if [[ "${ZEBRAD_CONTAINER}" == "${DEFAULT_ZEBRAD_CONTAINER}" ]]; then
  COMPOSE_PROJECT="${SOVA_BOX_COMPOSE_PROJECT:-regtest}"
else
  COMPOSE_PROJECT="${SOVA_BOX_COMPOSE_PROJECT:-${ZEBRAD_CONTAINER}}"
fi

# Set by ensure_release_binaries (resolved from `cargo metadata`).
SOVA_BIN=""
MINER_BIN=""

# E1c prebuilt binaries -- see the "prebuilt release binaries" section
# below and .github/workflows/box-binaries.yml.
PREBUILT_MODE="${SOVA_BOX_PREBUILT:-auto}"
PREBUILT_DIR="${SOVA_BOX_PREBUILT_DIR:-}"
PREBUILT_REPO="${SOVA_BOX_PREBUILT_REPO:-}"
PREBUILT_WORKFLOW="box-binaries.yml"
PREBUILT_ARTIFACT_PREFIX="sova-box-bin-"
# GitHub Release source (M0): <base>/<tag>/{SHA256SUMS,BUILD-INFO,
# sova-box-bin-<platform>.tar.gz}, fetched with plain curl.
RELEASE_TAG_OVERRIDE="${SOVA_BOX_RELEASE:-}"
RELEASE_REPO="${SOVA_BOX_REPO:-sova-chain/sova}"
RELEASE_BASE_URL="${SOVA_BOX_RELEASE_BASE_URL:-https://github.com/${RELEASE_REPO}/releases/download}"
# The Rust build inputs. An artifact is only used if its commit and this
# checkout (working tree included) are identical under these paths, so the
# downloaded binaries are exactly what `cargo build` here would produce.
# Keep in sync with the `paths:` filter in box-binaries.yml.
RUST_INPUT_PATHS=(bin crates Cargo.toml Cargo.lock .cargo rust-toolchain rust-toolchain.toml)

TADDR=""
EVM_ADDR=""
UP_OK=0
# Set once this `up` has started (or found and reused) zebrad, so a failed
# `up` only talks about a container it actually touched.
ZEBRAD_TOUCHED=0
# 1 once ensure_release_binaries compiled something (slow path).
BUILT_FROM_SOURCE=0
# PID of the "still building" ticker while cargo runs (see start_ticker).
TICKER_PID=""

# Plain-text logs: reth colours its output by default even when stdout is
# a file, which breaks `grep settled=true` on the node log. reth's tracer
# honors RUST_LOG_STYLE=never; NO_COLOR=1 covers anything else.
NO_COLOR_ENV=(NO_COLOR=1 RUST_LOG_STYLE=never)

set_endpoints() {
  ZEBRAD_RPC="http://127.0.0.1:${ZEBRAD_PORT}"
  SOVA_RPC="http://127.0.0.1:${RPC_PORT}"
}

# down/status: act on what `up` actually started, not on today's env.
load_recorded_config() {
  if [[ -f "${BOX_ENV_FILE}" ]]; then
    # shellcheck source=/dev/null
    source "${BOX_ENV_FILE}"
  fi
  set_endpoints
}

record_config() {
  {
    printf 'ZEBRAD_PORT=%q\n' "${ZEBRAD_PORT}"
    printf 'RPC_PORT=%q\n' "${RPC_PORT}"
    printf 'AUTH_PORT=%q\n' "${AUTH_PORT}"
    printf 'WS_PORT=%q\n' "${WS_PORT}"
    printf 'P2P_PORT=%q\n' "${P2P_PORT}"
    printf 'ZEBRAD_CONTAINER=%q\n' "${ZEBRAD_CONTAINER}"
    printf 'COMPOSE_PROJECT=%q\n' "${COMPOSE_PROJECT}"
  } >"${BOX_ENV_FILE}"
}

compose() {
  (cd "${REGTEST_DIR}" &&
    SOVA_BOX_ZEBRAD_PORT="${ZEBRAD_PORT}" \
      SOVA_BOX_ZEBRAD_CONTAINER="${ZEBRAD_CONTAINER}" \
      docker compose -p "${COMPOSE_PROJECT}" "$@")
}

zebrad_health() {
  docker inspect -f '{{.State.Health.Status}}' "${ZEBRAD_CONTAINER}" 2>/dev/null || true
}

pid_alive() {
  local pidfile="${PID_DIR}/$1.pid"
  [[ -f "${pidfile}" ]] && kill -0 "$(cat "${pidfile}")" 2>/dev/null
}

port_in_use() {
  if command -v lsof >/dev/null 2>&1; then
    lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1
  elif command -v nc >/dev/null 2>&1; then
    nc -z 127.0.0.1 "$1" >/dev/null 2>&1
  else
    return 1
  fi
}

preflight() {
  local name
  for name in sova-node miner auto-mine; do
    if pid_alive "${name}"; then
      echo "error: this box is already up (${name} pid $(cat "${PID_DIR}/${name}.pid")); run ./box/up.sh down first" >&2
      exit 1
    fi
  done

  local entry var port what failed=0
  local checks=(
    "SOVA_BOX_RPC_PORT ${RPC_PORT} Sova-RPC"
    "SOVA_BOX_AUTH_PORT ${AUTH_PORT} Sova-authrpc"
    "SOVA_BOX_P2P_PORT ${P2P_PORT} Sova-p2p"
  )
  if [[ -n "${WS_PORT}" ]]; then
    checks+=("SOVA_BOX_WS_PORT ${WS_PORT} Sova-WS-RPC")
  fi
  if [[ "${SIP7}" != "0" && "${SIP7}" != "1" ]]; then
    echo "error: SOVA_BOX_SIP7 must be 0 or 1, got: ${SIP7}" >&2
    failed=1
  fi
  # A healthy zebrad under our container name is reused, so its port being
  # bound is expected.
  if [[ "$(zebrad_health)" != "healthy" ]]; then
    checks+=("SOVA_BOX_ZEBRAD_PORT ${ZEBRAD_PORT} zebrad-RPC")
  fi
  for entry in "${checks[@]}"; do
    read -r var port what <<<"${entry}"
    if ! [[ "${port}" =~ ^[0-9]+$ ]]; then
      echo "error: ${var} must be a port number, got: ${port}" >&2
      failed=1
    elif port_in_use "${port}"; then
      echo "error: port ${port} (${what//-/ }) is already in use; free it or set ${var}=<free port>" >&2
      failed=1
    fi
  done
  [[ ${failed} -eq 0 ]] || exit 1
}

ensure_zebrad() {
  ZEBRAD_TOUCHED=1
  if [[ "$(zebrad_health)" == "healthy" ]]; then
    echo "zebrad already healthy, reusing"
    return
  fi
  if ! compose up -d; then
    echo "error: \`docker compose up\` for zebrad failed (is Docker running?)" >&2
    exit 1
  fi
  echo -n "waiting for zebrad RPC readiness "
  local deadline=$((SECONDS + 120))
  until [[ "$(zebrad_health)" == "healthy" ]]; do
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo
      echo "error: zebrad did not become healthy within 120s" >&2
      compose logs --no-color zebrad >&2 || true
      exit 1
    fi
    echo -n "."
    sleep 2
  done
  echo " healthy"
}

# Where cargo puts a workspace's build output: honors CARGO_TARGET_DIR,
# build.target-dir in any .cargo/config.toml, and symlinked target/ dirs
# alike, because cargo itself answers.
cargo_target_dir() {
  local ws="$1" meta dir
  if ! command -v cargo >/dev/null 2>&1; then
    # No Rust toolchain: only the prebuilt path can work, and without
    # cargo there is no config to consult -- use cargo's own defaults.
    printf '%s\n' "${CARGO_TARGET_DIR:-${ws}/target}"
    return 0
  fi
  if ! meta="$(cd "${ws}" && cargo metadata --format-version 1 --no-deps)"; then
    echo "error: \`cargo metadata\` failed in ${ws}; cannot locate its target directory" >&2
    return 1
  fi
  dir="$(printf '%s\n' "${meta}" | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
  if [[ -z "${dir}" ]]; then
    echo "error: no target_directory in \`cargo metadata\` output for ${ws}" >&2
    return 1
  fi
  printf '%s\n' "${dir}"
}

# ---- prebuilt release binaries (E1c) --------------------------------------
#
# CI (.github/workflows/box-binaries.yml) builds release `sova` +
# `sova-miner` per platform. On a `v*` tag it attaches them to that tag's
# GitHub Release; on a manual dispatch it keeps them as an Actions
# artifact. When a binary is missing, `up` tries, in order:
#   (a) the GitHub Release for this checkout's tag (or SOVA_BOX_RELEASE),
#       fetched with plain curl -- no GitHub login needed;
#   (b) a CI Actions artifact of the same Rust sources, via `gh`;
# verifying SHA256SUMS and BUILD-INFO (platform, and a commit with the same
# Rust sources as this checkout) each time, and installs the binaries where
# `cargo build --release` would have put them. Anything missing or
# mismatched moves on to the next source, and finally (c) to building from
# source (unless SOVA_BOX_PREBUILT=1). SOVA_BOX_PREBUILT_DIR replaces (a)
# and (b) with a local directory.

# A prebuilt source that simply doesn't apply to this checkout (no release
# tag, no `gh` or not logged in, no inferable repo, no matching CI run).
# A tag whose release assets are missing is still reported. In `auto` mode these are the normal state of a fresh
# clone and nothing the user can act on, so they stay quiet and `up`
# prints one neutral line instead. With SOVA_BOX_PREBUILT=1 (or 0, where
# they never run) each reason is printed. Real problems -- a checksum or
# BUILD-INFO mismatch, a broken download, a binary that won't run -- are
# always printed with plain `echo`.
prebuilt_skip() {
  [[ "${PREBUILT_MODE}" == "auto" ]] || echo "$1"
}

# darwin-arm64 | linux-x86_64 (the artifact suffixes), or fails.
box_platform() {
  case "$(uname -s)/$(uname -m)" in
    Darwin/arm64) echo "darwin-arm64" ;;
    Linux/x86_64) echo "linux-x86_64" ;;
    *) return 1 ;;
  esac
}

sha256_check() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$1"
  else
    shasum -a 256 -c "$1"
  fi
}

# Prints the hex SHA-256 of file $1.
sha256_of() {
  local out
  if command -v sha256sum >/dev/null 2>&1; then
    out="$(sha256sum "$1")" || return 1
  else
    out="$(shasum -a 256 "$1")" || return 1
  fi
  printf '%s\n' "${out%% *}"
}

# The release tag to fetch from: SOVA_BOX_RELEASE, else the v* tag HEAD is
# exactly at. Fails if there is neither.
release_tag() {
  if [[ -n "${RELEASE_TAG_OVERRIDE}" ]]; then
    printf '%s\n' "${RELEASE_TAG_OVERRIDE}"
    return 0
  fi
  git -C "${REPO_ROOT}" describe --tags --exact-match --match 'v*' HEAD 2>/dev/null
}

# Whether commit $1's Rust sources are identical to this checkout's
# (working tree, uncommitted edits and untracked files included).
sources_match_commit() {
  local sha="$1"
  [[ "${sha}" =~ ^[0-9a-f]{40}$ ]] || return 1
  git -C "${REPO_ROOT}" cat-file -e "${sha}^{commit}" 2>/dev/null || return 1
  git -C "${REPO_ROOT}" diff --quiet "${sha}" -- "${RUST_INPUT_PATHS[@]}" 2>/dev/null || return 1
  [[ -z "$(git -C "${REPO_ROOT}" ls-files --others --exclude-standard -- "${RUST_INPUT_PATHS[@]}")" ]]
}

# Downloads the matching artifact for platform $2 into staging dir $1.
# Prints why not and fails if there isn't one.
fetch_prebuilt_from_github() {
  local staging="$1" platform="$2" repo_path runs id sha branch chosen=""
  if ! command -v gh >/dev/null 2>&1; then
    prebuilt_skip "prebuilt: \`gh\` (GitHub CLI) not installed"
    return 1
  fi
  if ! gh auth status >/dev/null 2>&1; then
    prebuilt_skip "prebuilt: \`gh\` is not authenticated (gh auth login)"
    return 1
  fi
  repo_path="${PREBUILT_REPO:-$(cd "${REPO_ROOT}" && gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null)}"
  if [[ -z "${repo_path}" ]]; then
    prebuilt_skip "prebuilt: could not tell which GitHub repo this checkout is (set SOVA_BOX_PREBUILT_REPO=owner/name)"
    return 1
  fi
  if ! runs="$(gh api "repos/${repo_path}/actions/workflows/${PREBUILT_WORKFLOW}/runs?status=success&per_page=30" \
    --jq '.workflow_runs[] | "\(.id) \(.head_sha) \(.head_branch)"' 2>/dev/null)"; then
    prebuilt_skip "prebuilt: no ${PREBUILT_WORKFLOW} runs visible in ${repo_path}"
    return 1
  fi
  # Newest first. The exact commit is the common case; an older run on any
  # branch is just as good if nothing Rust changed since.
  while read -r id sha branch; do
    [[ -n "${id}" ]] || continue
    if sources_match_commit "${sha}"; then
      chosen="${id} ${sha} ${branch}"
      break
    fi
  done <<<"${runs}"
  if [[ -z "${chosen}" ]]; then
    prebuilt_skip "prebuilt: no successful ${PREBUILT_WORKFLOW} run in ${repo_path} was built from this checkout's Rust sources"
    return 1
  fi
  read -r id sha branch <<<"${chosen}"
  echo "prebuilt: downloading ${PREBUILT_ARTIFACT_PREFIX}${platform} from ${repo_path} run ${id} (${branch} @ ${sha:0:12})"
  if ! gh run download "${id}" --repo "${repo_path}" \
    --name "${PREBUILT_ARTIFACT_PREFIX}${platform}" --dir "${staging}" >/dev/null 2>&1; then
    echo "prebuilt: download failed (artifact expired, or none for ${platform} in run ${id})"
    return 1
  fi
  PREBUILT_ORIGIN="${repo_path} Actions run ${id}"
}

# Downloads this checkout's GitHub Release assets for platform $2 with
# plain curl and unpacks them into staging dir $1 (laid out like the
# artifact). Checks, before unpacking anything:
#   - the tarball's SHA-256 matches the release-level SHA256SUMS;
#   - the release-level BUILD-INFO names this tag, lists this platform,
#     and a commit with this checkout's Rust sources;
# and after: the tarball held exactly the four regular files, and its own
# BUILD-INFO names the same commit. verify_prebuilt then re-checks the
# binaries' own SHA256SUMS and BUILD-INFO platform/commit.
fetch_prebuilt_from_release() {
  local staging="$1" platform="$2" tag base asset dl f expected actual
  local rel_tag rel_commit rel_platforms inner_commit
  if ! tag="$(release_tag)" || [[ -z "${tag}" ]]; then
    prebuilt_skip "prebuilt: this checkout is not exactly at a v* release tag (SOVA_BOX_RELEASE=vX.Y.Z picks one)"
    return 1
  fi
  if ! [[ "${tag}" =~ ^[A-Za-z0-9._+-]+$ ]]; then
    echo "prebuilt: '${tag}' is not a usable release tag"
    return 1
  fi
  if ! command -v curl >/dev/null 2>&1; then
    prebuilt_skip "prebuilt: \`curl\` not installed, cannot fetch release ${tag}"
    return 1
  fi
  base="${RELEASE_BASE_URL%/}/${tag}"
  asset="${PREBUILT_ARTIFACT_PREFIX}${platform}.tar.gz"
  dl="${staging}/release"
  mkdir -p "${dl}" || return 1
  echo "prebuilt: fetching release ${tag} (${asset}) from ${base}"
  for f in SHA256SUMS BUILD-INFO "${asset}"; do
    if ! curl -fsSL --connect-timeout 15 --retry 2 -o "${dl}/${f}" "${base}/${f}"; then
      echo "prebuilt: release ${tag} has no ${f} at ${base}/${f}"
      return 1
    fi
  done

  expected="$(awk -v n="${asset}" '$2 == n || $2 == ("*" n) { print $1; exit }' "${dl}/SHA256SUMS")"
  if ! [[ "${expected}" =~ ^[0-9a-f]{64}$ ]]; then
    echo "prebuilt: release ${tag} SHA256SUMS does not list ${asset}"
    return 1
  fi
  actual="$(sha256_of "${dl}/${asset}")" || return 1
  if [[ "${actual}" != "${expected}" ]]; then
    echo "prebuilt: release ${tag} ${asset} SHA-256 mismatch (SHA256SUMS ${expected:0:12}, got ${actual:0:12}) -- not using it"
    return 1
  fi

  rel_tag="$(sed -n 's/^tag=//p' "${dl}/BUILD-INFO")"
  rel_commit="$(sed -n 's/^commit=//p' "${dl}/BUILD-INFO")"
  rel_platforms="$(sed -n 's/^platforms=//p' "${dl}/BUILD-INFO")"
  if [[ "${rel_tag}" != "${tag}" ]]; then
    echo "prebuilt: release BUILD-INFO says tag '${rel_tag}', expected ${tag}"
    return 1
  fi
  if [[ " ${rel_platforms} " != *" ${platform} "* ]]; then
    echo "prebuilt: release ${tag} BUILD-INFO lists platforms '${rel_platforms}', not ${platform}"
    return 1
  fi
  if ! sources_match_commit "${rel_commit}"; then
    echo "prebuilt: release ${tag} was built from '${rel_commit}', which does not have this checkout's Rust sources (or is not in this clone)"
    return 1
  fi

  if ! tar -xzf "${dl}/${asset}" -C "${staging}" sova sova-miner SHA256SUMS BUILD-INFO 2>/dev/null; then
    echo "prebuilt: ${asset} does not unpack to sova, sova-miner, SHA256SUMS, BUILD-INFO"
    return 1
  fi
  for f in sova sova-miner SHA256SUMS BUILD-INFO; do
    if [[ -L "${staging}/${f}" || ! -f "${staging}/${f}" ]]; then
      echo "prebuilt: ${asset}: ${f} is not a regular file"
      return 1
    fi
  done
  inner_commit="$(sed -n 's/^commit=//p' "${staging}/BUILD-INFO")"
  if [[ "${inner_commit}" != "${rel_commit}" ]]; then
    echo "prebuilt: ${asset} BUILD-INFO commit '${inner_commit}' differs from the release's '${rel_commit}'"
    return 1
  fi
  PREBUILT_ORIGIN="release ${tag} (${base})"
}

# Copies a local directory laid out like the artifact into staging dir $1.
fetch_prebuilt_from_dir() {
  local staging="$1" f
  if [[ ! -d "${PREBUILT_DIR}" ]]; then
    echo "prebuilt: SOVA_BOX_PREBUILT_DIR=${PREBUILT_DIR} is not a directory"
    return 1
  fi
  for f in sova sova-miner SHA256SUMS BUILD-INFO; do
    [[ -f "${PREBUILT_DIR}/${f}" ]] && cp "${PREBUILT_DIR}/${f}" "${staging}/${f}"
  done
  PREBUILT_ORIGIN="local dir ${PREBUILT_DIR}"
}

# Checks a staged artifact in $1 is complete, intact, for platform $2, and
# built from this checkout's Rust sources.
verify_prebuilt() {
  local staging="$1" platform="$2" f info_commit info_platform
  for f in sova sova-miner SHA256SUMS BUILD-INFO; do
    if [[ ! -f "${staging}/${f}" ]]; then
      echo "prebuilt: artifact is missing ${f}"
      return 1
    fi
  done
  if ! (cd "${staging}" && sha256_check SHA256SUMS >/dev/null 2>&1); then
    echo "prebuilt: SHA256SUMS check FAILED -- not using these binaries"
    return 1
  fi
  if ! grep -Eq '^[0-9a-f]{64}  sova$' "${staging}/SHA256SUMS" ||
    ! grep -Eq '^[0-9a-f]{64}  sova-miner$' "${staging}/SHA256SUMS"; then
    echo "prebuilt: SHA256SUMS does not cover both binaries"
    return 1
  fi
  info_commit="$(sed -n 's/^commit=//p' "${staging}/BUILD-INFO")"
  info_platform="$(sed -n 's/^platform=//p' "${staging}/BUILD-INFO")"
  if [[ "${info_platform}" != "${platform}" ]]; then
    echo "prebuilt: artifact is for '${info_platform}', this host is ${platform}"
    return 1
  fi
  if ! sources_match_commit "${info_commit}"; then
    echo "prebuilt: artifact commit '${info_commit}' does not have this checkout's Rust sources"
    return 1
  fi
  PREBUILT_COMMIT="${info_commit}"
  echo "prebuilt: SHA256SUMS OK, platform ${platform}, built from ${info_commit:0:12} (same Rust sources as this checkout)"
}

# Installs staged binary $2 to path $3 (mode 0755, atomic rename).
install_prebuilt_binary() {
  local staging="$1" name="$2" dest="$3"
  mkdir -p "$(dirname "${dest}")" || return 1
  cp "${staging}/${name}" "${dest}.prebuilt-tmp" &&
    chmod 0755 "${dest}.prebuilt-tmp" &&
    mv -f "${dest}.prebuilt-tmp" "${dest}"
}

# One prebuilt source ($1 = dir | release | artifact) for platform $3,
# staged in $2: fetch, verify, smoke-run, install whichever of SOVA_BIN /
# MINER_BIN is missing. Fails (printing why) at the first problem.
try_prebuilt_source() {
  local source="$1" staging="$2" platform="$3"
  PREBUILT_ORIGIN=""
  PREBUILT_COMMIT=""
  case "${source}" in
    dir) fetch_prebuilt_from_dir "${staging}" || return 1 ;;
    release) fetch_prebuilt_from_release "${staging}" "${platform}" || return 1 ;;
    artifact) fetch_prebuilt_from_github "${staging}" "${platform}" || return 1 ;;
    *) return 1 ;;
  esac
  verify_prebuilt "${staging}" "${platform}" || return 1
  # Last line of defence against a binary this host can't load (e.g. a
  # glibc older than the CI runner's): sova-miner shares the toolchain
  # and libc with sova and, unlike sova, has a harmless --version.
  chmod 0755 "${staging}/sova-miner"
  if ! "${staging}/sova-miner" --version >/dev/null 2>&1; then
    echo "prebuilt: sova-miner from ${PREBUILT_ORIGIN} does not run on this host"
    return 1
  fi
  if [[ ! -x "${SOVA_BIN}" ]]; then
    install_prebuilt_binary "${staging}" sova "${SOVA_BIN}" || return 1
    echo "bin/sova: PREBUILT (${PREBUILT_ORIGIN}, commit ${PREBUILT_COMMIT:0:12}) -> ${SOVA_BIN}"
  fi
  if [[ ! -x "${MINER_BIN}" ]]; then
    install_prebuilt_binary "${staging}" sova-miner "${MINER_BIN}" || return 1
    echo "sova-miner: PREBUILT (${PREBUILT_ORIGIN}, commit ${PREBUILT_COMMIT:0:12}) -> ${MINER_BIN}"
  fi
}

# Tries to fill in whichever of SOVA_BIN / MINER_BIN is missing from a
# prebuilt source: SOVA_BOX_PREBUILT_DIR if set, else (a) the GitHub
# Release, then (b) an Actions artifact. Fails (printing why) if none works.
try_prebuilt_binaries() {
  local platform staging source rc=1
  local sources=(release artifact)
  if ! platform="$(box_platform)"; then
    prebuilt_skip "prebuilt: none published for $(uname -s)/$(uname -m) (only darwin-arm64, linux-x86_64)"
    return 1
  fi
  [[ -n "${PREBUILT_DIR}" ]] && sources=(dir)
  for source in "${sources[@]}"; do
    staging="$(mktemp -d "${TMPDIR:-/tmp}/sova-box-prebuilt.XXXXXX")" || return 1
    if try_prebuilt_source "${source}" "${staging}" "${platform}"; then
      rc=0
    fi
    rm -rf -- "${staging}"
    [[ ${rc} -eq 0 ]] && break
  done
  return ${rc}
}

# Prints "<what>: still building (Nm elapsed)" once a minute until
# stop_ticker, so a long cargo build visibly stays alive between its own
# `Compiling` lines. The ticker also stops by itself if this script dies.
# Background jobs of a non-interactive shell ignore SIGINT, so every exit
# path (build done, build failed, Ctrl-C via the EXIT trap) calls
# stop_ticker explicitly.
start_ticker() {
  local what="$1" start parent=$$
  start="$(date +%s)"
  (
    sleeper=""
    trap '[[ -n "${sleeper}" ]] && kill "${sleeper}" 2>/dev/null; exit 0' TERM
    while kill -0 "${parent}" 2>/dev/null; do
      sleep 60 &
      sleeper=$!
      wait "${sleeper}"
      sleeper=""
      kill -0 "${parent}" 2>/dev/null || break
      echo "... ${what}: still building ($((($(date +%s) - start) / 60))m elapsed)"
    done
  ) &
  TICKER_PID=$!
}

stop_ticker() {
  [[ -n "${TICKER_PID}" ]] || return 0
  kill "${TICKER_PID}" 2>/dev/null || true
  wait "${TICKER_PID}" 2>/dev/null || true
  TICKER_PID=""
}

build_from_source() {
  local what="$1" ws="$2" pkg="$3" bin="$4" target="$5" t0=${SECONDS} rc=0
  if ! command -v cargo >/dev/null 2>&1; then
    echo "error: ${what} is missing and \`cargo\` is not installed, so it can't be built from source (install Rust via rustup, or make a prebuilt artifact available -- see box/up/README.md)" >&2
    exit 1
  fi
  echo "${what}: BUILDING FROM SOURCE (release) into ${target} -- first run only"
  BUILT_FROM_SOURCE=1
  start_ticker "${what}"
  (cd "${ws}" && cargo build --release -p "${pkg}") || rc=$?
  stop_ticker
  if [[ ${rc} -ne 0 ]]; then
    echo "error: cargo build --release -p ${pkg} failed (exit ${rc}) after $((SECONDS - t0))s" >&2
    exit 1
  fi
  if [[ ! -x "${bin}" ]]; then
    echo "error: build finished but ${bin} is missing (target dir from \`cargo metadata\` in ${ws})" >&2
    exit 1
  fi
  echo "${what} built in $(((SECONDS - t0) / 60))m$(((SECONDS - t0) % 60))s: ${bin}"
}

ensure_release_binaries() {
  local sova_target miner_target
  case "${PREBUILT_MODE}" in
    auto | 0 | 1) ;;
    *)
      echo "error: SOVA_BOX_PREBUILT must be auto, 0 or 1, got: ${PREBUILT_MODE}" >&2
      exit 1
      ;;
  esac
  sova_target="$(cargo_target_dir "${REPO_ROOT}")" || exit 1
  miner_target="$(cargo_target_dir "${BURN_WALLET_DIR}")" || exit 1
  SOVA_BIN="${sova_target}/release/sova"
  MINER_BIN="${miner_target}/release/sova-miner"

  if [[ -x "${SOVA_BIN}" ]]; then
    echo "bin/sova release binary already present, reusing ${SOVA_BIN}"
  fi
  if [[ -x "${MINER_BIN}" ]]; then
    echo "sova-miner release binary already present, reusing ${MINER_BIN}"
  fi
  if [[ -x "${SOVA_BIN}" && -x "${MINER_BIN}" ]]; then
    return
  fi

  if [[ "${PREBUILT_MODE}" == "0" ]]; then
    echo "prebuilt: skipped (SOVA_BOX_PREBUILT=0)"
  elif try_prebuilt_binaries; then
    return
  elif [[ "${PREBUILT_MODE}" == "1" ]]; then
    echo "error: SOVA_BOX_PREBUILT=1 but no usable prebuilt binaries (see above); unset it to build from source" >&2
    exit 1
  else
    echo "prebuilt: no published binaries match this checkout; building from source"
  fi

  if [[ ! -x "${SOVA_BIN}" ]]; then
    echo "first-run release build (later runs reuse the binaries). How long it takes depends on the machine:"
    echo "  about 11 min on an idle 8-core Apple Silicon laptop (bin/sova ~10 min, sova-miner ~1 min);"
    echo "  20-25+ min if the machine is busy with other heavy work; add ~2 min if the Cargo cache is empty (crates download first)."
    echo "  A 'still building (Nm elapsed)' line prints every minute."
  else
    echo "release build of sova-miner only: about 1-3 min"
  fi
  if [[ ! -x "${SOVA_BIN}" ]]; then
    build_from_source bin/sova "${REPO_ROOT}" sova "${SOVA_BIN}" "${sova_target}"
  fi
  if [[ ! -x "${MINER_BIN}" ]]; then
    build_from_source sova-miner "${BURN_WALLET_DIR}" sova-miner "${MINER_BIN}" "${miner_target}"
  fi
}

# Runs `sova-miner init` and parses the two labeled output lines for the
# t-address and EVM address (awk on the labeled line, last field), rather
# than hand-parsing state.json's internal field names in a second place.
# `init` is itself idempotent (loads an existing keystore under --data-dir
# instead of overwriting it), so re-running this on an already-initialized
# data dir is safe and just reprints the same identity.
ensure_miner_identity() {
  mkdir -p "${MINER_DATA_DIR}"
  local init_log="${LOG_DIR}/miner-init.log"
  env "${NO_COLOR_ENV[@]}" \
    "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest init >"${init_log}" 2>&1
  TADDR="$(awk '/t-addr to fund/ {print $NF}' "${init_log}")"
  EVM_ADDR="$(awk '/evm address/ {print $NF}' "${init_log}")"
  if [[ -z "${TADDR}" || -z "${EVM_ADDR}" ]]; then
    echo "error: failed to parse miner identity from ${MINER_BIN} init output (${init_log}):" >&2
    cat "${init_log}" >&2
    exit 1
  fi
  echo "miner t-addr:    ${TADDR}"
  echo "miner evm addr:  ${EVM_ADDR}"
  # The EVM address is the keystore key's own Ethereum address, so the SOVA
  # it earns can be spent from any EVM wallet. A data dir made before that
  # default still credits the old hash160 address (unspendable); init keeps
  # it and prints WARNING lines, surfaced here. Both greps are no-ops with
  # an older sova-miner binary, which prints neither.
  if grep -q 'export-evm-key' "${init_log}" && ! grep -q '^WARNING' "${init_log}"; then
    echo "spend its SOVA: ${MINER_BIN} --data-dir ${MINER_DATA_DIR} export-evm-key --i-understand (import into an EVM wallet)"
  fi
  grep '^WARNING' "${init_log}" >&2 || true
}

fund_miner() {
  local resp
  resp="$(curl -s -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"fund\",\"method\":\"generatetoaddress\",\"params\":[${FUND_BLOCKS},\"${TADDR}\"]}" \
    "${ZEBRAD_RPC}/")"
  if ! grep -q '"result"' <<<"${resp}"; then
    echo "error: generatetoaddress funding failed: ${resp}" >&2
    exit 1
  fi
  echo "funded miner with ${FUND_BLOCKS} blocks (matures its first coinbase)"
}

# Removes the node temp dir recorded by `up` -- only that one, and only if
# its name is one we create.
remove_node_tmpdir() {
  [[ -f "${NODE_TMPDIR_FILE}" ]] || return 0
  local dir
  dir="$(cat "${NODE_TMPDIR_FILE}")"
  if [[ -z "${dir}" || "$(basename "${dir}")" != "${NODE_TMPDIR_PREFIX}"* ]]; then
    echo "warning: not removing unexpected node datadir path '${dir}' recorded in ${NODE_TMPDIR_FILE}" >&2
  elif [[ -d "${dir}" ]]; then
    rm -rf -- "${dir}"
    echo "removed node datadir ${dir}"
  fi
  rm -f "${NODE_TMPDIR_FILE}"
}

start_background_processes() {
  local node_tmpdir
  node_tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/${NODE_TMPDIR_PREFIX}XXXXXX")" || {
    echo "error: could not create a temp dir under ${TMPDIR:-/tmp} for the node datadir" >&2
    exit 1
  }
  node_tmpdir="$(cd "${node_tmpdir}" && pwd -P)"
  echo "${node_tmpdir}" >"${NODE_TMPDIR_FILE}"

  # Optional bin/sova env, only when asked for (unset = bin/sova's default).
  local opt_env=()
  [[ -n "${WS_PORT}" ]] && opt_env+=(SOVA_WS_PORT="${WS_PORT}")
  [[ "${SIP7}" == "1" ]] && opt_env+=(SOVA_SIP7=1)
  env "${NO_COLOR_ENV[@]}" ${opt_env[@]+"${opt_env[@]}"} \
    TMPDIR="${node_tmpdir}" \
    SOVA_ZEBRAD_RPC="${ZEBRAD_RPC}" \
    SOVA_MINER_EVM_ADDRESS="${EVM_ADDR}" \
    SOVA_EPOCH_BASE=1 \
    SOVA_HTTP_PORT="${RPC_PORT}" \
    SOVA_RPC_CORS="${RPC_CORS}" \
    SOVA_AUTH_PORT="${AUTH_PORT}" \
    SOVA_P2P_PORT="${P2P_PORT}" \
    nohup "${SOVA_BIN}" >"${LOG_DIR}/sova-node.log" 2>&1 &
  echo $! >"${PID_DIR}/sova-node.pid"

  # Reuse the regtest harness's own proven driver script rather than
  # reimplementing a block-generation loop here.
  nohup "${REGTEST_DIR}/auto-mine.sh" "${AUTO_MINE_INTERVAL}" "${ZEBRAD_RPC}" \
    >"${LOG_DIR}/auto-mine.log" 2>&1 &
  echo $! >"${PID_DIR}/auto-mine.pid"

  # Continuous, budget-capped mining loop: no --max-epochs, so it only
  # stops when the budget runs out (or it's killed by `down`).
  # ${MINER_DATA_DIR} outlives the regtest chain (`down` recreates zebrad,
  # the miner keystore is kept). The miner itself detects that its
  # state.json was built on a different Zcash chain and retires the stale
  # UTXOs/epochs on startup (sova-miner's chain guard, E1d), so nothing
  # needs wiping here.
  env "${NO_COLOR_ENV[@]}" \
    nohup "${MINER_BIN}" --data-dir "${MINER_DATA_DIR}" --network regtest mine \
    --budget-zat "${BUDGET_ZAT}" --per-epoch-zat "${PER_EPOCH_ZAT}" \
    --rpc "${ZEBRAD_RPC}" \
    >"${LOG_DIR}/miner.log" 2>&1 &
  echo $! >"${PID_DIR}/miner.pid"

  echo "sova-node pid $(cat "${PID_DIR}/sova-node.pid"), auto-mine pid $(cat "${PID_DIR}/auto-mine.pid"), miner pid $(cat "${PID_DIR}/miner.pid")"
  echo "node datadir: ${node_tmpdir}/reth-test-* (removed by ./box/up.sh down)"
}

wait_for_liveness() {
  echo -n "waiting for sova RPC "
  local deadline=$((SECONDS + 60)) resp
  # Response into a variable, not `curl | grep -q`: under pipefail a match
  # can SIGPIPE curl and read as "not up yet".
  until resp="$(curl -s -X POST -H 'Content-Type: application/json' \
    --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' \
    "${SOVA_RPC}")" && grep -q result <<<"${resp}"; do
    if [[ ${SECONDS} -ge ${deadline} ]]; then
      echo
      echo "error: sova RPC did not come up within 60s on ${SOVA_RPC}; see ${LOG_DIR}/sova-node.log" >&2
      tail -40 "${LOG_DIR}/sova-node.log" >&2
      exit 1
    fi
    echo -n "."
    sleep 2
  done
  echo " live"
}

print_watch_it_work() {
  cat <<EOF

sova-in-a-box is up. Watch it mine:

  ./box/up.sh status                    # block height, miner's SOVA balance, settled epochs

  tail -f ${LOG_DIR}/sova-node.log      # epoch triggers / settled epochs
  tail -f ${LOG_DIR}/miner.log          # burns submitted, budget remaining
  tail -f ${LOG_DIR}/auto-mine.log      # Zcash blocks being generated
  grep 'settled=true' ${LOG_DIR}/sova-node.log

Check the Sova chain is advancing:

  curl -s -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \\
    -H 'Content-Type: application/json' ${SOVA_RPC}

Check the miner's balance (climbs as epochs settle):

  curl -s -d '{"jsonrpc":"2.0","method":"eth_getBalance","params":["${EVM_ADDR}","latest"],"id":1}' \\
    -H 'Content-Type: application/json' ${SOVA_RPC}

Optional, with Foundry installed: deploy the day-one dapp kit (WSOVA, an
AMM, Multicall3, the Ashwings mint and market) and run its demo loop (~30s):

  ./box/deploy-dapps.sh ${SOVA_RPC}

Tear down:

  ./box/up.sh down
EOF
  if [[ -n "${WS_PORT}" ]]; then
    echo
    echo "WS JSON-RPC: ws://127.0.0.1:${WS_PORT}"
  fi
  if [[ "${SIP7}" == "1" ]]; then
    cat <<EOF

SIP-7 is on (SOVA_SIP7=1). Anchored Zcash block summaries:

  curl -s -d '{"jsonrpc":"2.0","method":"sova_getZcashBlocks","params":[1,10],"id":1}' \\
    -H 'Content-Type: application/json' ${SOVA_RPC}
EOF
    if [[ -n "${WS_PORT}" ]]; then
      echo '  and sova_subscribe(["zcashBlocks"]) over the WS endpoint above'
    fi
  fi
  return 0
}

# SIGTERM a tracked host process and wait (up to ~10s, then SIGKILL) until
# it has actually exited -- `down` runs in a different shell from `up`, so
# `wait` can't be used, and the node's datadir must not be removed while
# the node still has it open.
stop_process() {
  local name="$1" pidfile pid i
  pidfile="${PID_DIR}/${name}.pid"
  if [[ ! -f "${pidfile}" ]]; then
    echo "${name}: no pidfile, nothing to stop"
    return
  fi
  pid="$(cat "${pidfile}")"
  if kill -0 "${pid}" 2>/dev/null; then
    kill "${pid}" 2>/dev/null || true
    for ((i = 0; i < 50; i++)); do
      kill -0 "${pid}" 2>/dev/null || break
      sleep 0.2
    done
    if kill -0 "${pid}" 2>/dev/null; then
      kill -9 "${pid}" 2>/dev/null || true
      sleep 0.2
      echo "stopped ${name} (pid ${pid}, SIGKILL after 10s)"
    else
      echo "stopped ${name} (pid ${pid})"
    fi
  else
    echo "${name} (pid ${pid}) already stopped"
  fi
  rm -f "${pidfile}"
}

stop_host_processes() {
  local name
  for name in miner sova-node auto-mine; do
    stop_process "${name}"
  done
}

# `up` failed or was interrupted: stop what this run started and remove its
# node datadir. zebrad is left running (the next `up` reuses it; `down`
# removes it).
on_up_exit() {
  local rc=$?
  stop_ticker
  [[ ${UP_OK} -eq 1 ]] && return
  echo "box up did not complete (exit ${rc}); cleaning up what this run started" >&2
  if compgen -G "${PID_DIR}/*.pid" >/dev/null; then
    stop_host_processes >&2
  fi
  remove_node_tmpdir >&2
  if [[ ${ZEBRAD_TOUCHED} -eq 1 ]]; then
    echo "zebrad container '${ZEBRAD_CONTAINER}' left running for reuse; ./box/up.sh down removes it" >&2
  else
    # Failed before step [2/6]: this run started no container (and has not
    # rewritten box.env, so a previous run's record stays valid for `down`).
    echo "no zebrad container was started by this run" >&2
  fi
}

cmd_up() {
  mkdir -p "${LOG_DIR}" "${PID_DIR}"
  set_endpoints
  preflight

  trap on_up_exit EXIT
  trap 'exit 130' INT TERM

  # A previous run that died without `down` (the node is not running --
  # preflight checked) may have left its recorded datadir behind.
  remove_node_tmpdir

  # Binaries first: a first-run source build takes 11-25+ min, and nothing
  # (no container, no port) should be held while it runs or be left behind
  # if it fails or is interrupted.
  echo "=== [1/6] release binaries ==="
  ensure_release_binaries

  if [[ ${BUILT_FROM_SOURCE} -eq 1 ]]; then
    # The ports were checked before a long build; check again now.
    preflight
  fi
  record_config

  echo "=== [2/6] zebrad regtest ==="
  ensure_zebrad

  echo "=== [3/6] miner identity ==="
  ensure_miner_identity

  echo "=== [4/6] funding miner ==="
  fund_miner

  echo "=== [5/6] starting sova node, auto-mine, and the miner loop ==="
  start_background_processes

  echo "=== [6/6] waiting for the chain to come up ==="
  wait_for_liveness

  UP_OK=1
  print_watch_it_work
}

cmd_down() {
  load_recorded_config
  echo "=== stopping host processes ==="
  stop_host_processes
  remove_node_tmpdir

  echo "=== tearing down zebrad ==="
  compose down -v || true
  rm -f "${BOX_ENV_FILE}"

  echo "down. Logs kept under ${LOG_DIR} for postmortem (rm -rf ${RUN_DIR} to fully reset, including the miner keystore)."
}

# JSON-RPC call against the Sova node; prints the hex "result" or nothing.
sova_rpc_result() {
  curl -s -m 5 -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"method\":\"$1\",\"params\":$2,\"id\":1}" \
    "${SOVA_RPC}" 2>/dev/null | sed -n 's/.*"result":"\(0x[0-9a-fA-F]*\)".*/\1/p'
}

# 0x-hex wei -> decimal SOVA (18 decimals), e.g. "68,750". python3 because
# balances overflow shell/awk integer math.
wei_hex_to_sova() {
  python3 -c '
import sys
whole, frac = divmod(int(sys.argv[1], 16), 10**18)
out = f"{whole:,}"
if frac:
    out += ("." + f"{frac:018d}").rstrip("0")
print(out)' "$1" 2>/dev/null
}

cmd_status() {
  load_recorded_config
  echo "=== zebrad (${ZEBRAD_CONTAINER}, ${ZEBRAD_RPC}) ==="
  local health
  health="$(zebrad_health)"
  echo "health: ${health:-not running}"
  echo "=== host processes ==="
  local name pidfile pid
  for name in sova-node auto-mine miner; do
    pidfile="${PID_DIR}/${name}.pid"
    if [[ -f "${pidfile}" ]]; then
      pid="$(cat "${pidfile}")"
      if kill -0 "${pid}" 2>/dev/null; then
        echo "${name}: running (pid ${pid})"
      else
        echo "${name}: pidfile present but process is dead (pid ${pid})"
      fi
    else
      echo "${name}: not started"
    fi
  done

  echo "=== sova chain (${SOVA_RPC}) ==="
  local height_hex evm_addr bal_hex bal_sova settled
  height_hex="$(sova_rpc_result eth_blockNumber '[]')"
  if [[ -z "${height_hex}" ]]; then
    echo "block height: unavailable (RPC not answering)"
    return
  fi
  printf 'block height: %d (%s)\n' "${height_hex}" "${height_hex}"

  evm_addr="$(awk '/evm address/ {print $NF}' "${LOG_DIR}/miner-init.log" 2>/dev/null)"
  if [[ -n "${evm_addr}" ]]; then
    bal_hex="$(sova_rpc_result eth_getBalance "[\"${evm_addr}\",\"latest\"]")"
    bal_sova="$(wei_hex_to_sova "${bal_hex:-0x0}")"
    echo "miner ${evm_addr} balance: ${bal_sova:-?} SOVA (${bal_hex:-?} wei)"
  else
    echo "miner balance: unknown (no ${LOG_DIR}/miner-init.log)"
  fi

  if [[ -f "${LOG_DIR}/sova-node.log" ]]; then
    settled="$(grep -c 'settled=true' "${LOG_DIR}/sova-node.log")"
    echo "settled epochs (this run): ${settled}"
  fi
}

cmd="${1:-up}"
case "${cmd}" in
  up) cmd_up ;;
  down) cmd_down ;;
  status) cmd_status ;;
  binaries)
    trap stop_ticker EXIT
    trap 'exit 130' INT TERM
    ensure_release_binaries
    echo "bin/sova:   ${SOVA_BIN}"
    echo "sova-miner: ${MINER_BIN}"
    ;;
  *)
    echo "usage: $0 [up|down|status|binaries]" >&2
    exit 1
    ;;
esac
