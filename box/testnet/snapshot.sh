#!/usr/bin/env bash
# Verifiable zebrad state snapshots (infra-2 D6; runbook: docs/ops/snapshots.md).
#
# A snapshot is a sync shortcut, not a source of truth. The manifest names
# one block (height + hash) that the restored node must have. The restorer
# checks that block against a public explorer or a second node.
#
#   snapshot.sh capture --rpc URL [--rpc-cookie-file F] [--out FILE]
#       On the RUNNING node: record network, tip height, tip hash, zebra version.
#   snapshot.sh create <state_dir> <out_dir> (--capture FILE | --height H --hash HASH)
#                      [--zebra-version V] [--network N] [--rpc URL]
#       On the STOPPED node's cache_dir: write zebrad-<network>-<height>.tar.zst,
#       SHA256SUMS and snapshot.json into <out_dir> (empty or new).
#       Refuses if anything still holds the database. With --rpc it also
#       refuses when that RPC still answers.
#   snapshot.sh restore <archive> <state_dir> [--sums FILE] [--manifest FILE]
#       Check SHA256SUMS, refuse a non-empty target, extract, print what to check.
#   snapshot.sh verify --rpc URL (--manifest FILE | --height H --hash HASH)
#                      [--rpc-cookie-file F] [--reference-rpc URL [--reference-cookie-file F]]
#       On the RESTORED, running node: its block at H must have the manifest's hash.
#       With --reference-rpc it also asks a second, independent node.
#
# <state_dir> is zebrad's state.cache_dir: the directory that holds state/vN/<network>.
# The archive holds only state/vN/<network>/ (RocksDB, minus LOCK and LOG*) and
# non_finalized_state/<network>/ (zebrad's backup of its last ~1000 blocks). It never
# holds the RPC cookie (.cookie), the peer cache (network/*.peers) or anything else.

set -euo pipefail

PROG="$(basename "$0")"

die() {
  echo "${PROG}: error: $*" >&2
  exit 1
}
warn() { echo "${PROG}: warning: $*" >&2; }
info() { echo "${PROG}: $*" >&2; }

usage() {
  sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not installed"; }

sha256_of() {
  local out
  if command -v sha256sum >/dev/null 2>&1; then
    out="$(sha256sum "$1")"
  else
    out="$(shasum -a 256 "$1")"
  fi
  echo "${out%% *}"
}

file_size() {
  local n
  n="$(wc -c <"$1")"
  echo "${n//[[:space:]]/}"
}

# Canonical absolute path of an existing directory (resolves /var -> /private/var).
canon_dir() { (cd "$1" 2>/dev/null && pwd -P); }

norm_hash() {
  local h="${1#0x}"
  h="$(tr '[:upper:]' '[:lower:]' <<<"${h}")"
  [[ "${h}" =~ ^[0-9a-f]{64}$ ]] || die "not a 32-byte hex block hash: $1"
  echo "${h}"
}

check_height() { [[ "$1" =~ ^[0-9]+$ ]] || die "not a block height: $1"; }

# JSON-RPC call; prints the raw response body. $1 url, $2 cookie file or "", $3 method, $4 params JSON.
rpc() {
  local url="$1" cookie="$2" method="$3" params="${4:-[]}"
  local -a auth=()
  if [[ -n "${cookie}" ]]; then
    [[ -r "${cookie}" ]] || die "cannot read RPC cookie file ${cookie}"
    auth=(-u "$(cat "${cookie}")")
  fi
  curl -sS -m 15 "${auth[@]+"${auth[@]}"}" -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":\"snapshot\",\"method\":\"${method}\",\"params\":${params}}" \
    "${url%/}/"
}

# RPC result field via jq, failing loudly on an RPC error.
rpc_result() {
  local body
  body="$(rpc "$@")" || die "RPC $3 to $1 failed (is the node up?)"
  if [[ "$(jq -r '.error // empty | tostring' <<<"${body}")" != "" ]]; then
    die "RPC $3 returned an error: $(jq -c '.error' <<<"${body}")"
  fi
  jq -c '.result' <<<"${body}"
}

# Genesis hashes (same constants as crates/burn-wallet/faucet/src/node.rs, plus regtest).
GENESIS_MAINNET=00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08
GENESIS_TESTNET=05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38
GENESIS_REGTEST=029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327

# zebrad reports chain "test" for testnet AND regtest, so tell them apart by genesis.
net_from_genesis() {
  case "$1" in
    "${GENESIS_MAINNET}") echo mainnet ;;
    "${GENESIS_TESTNET}") echo testnet ;;
    "${GENESIS_REGTEST}") echo regtest ;;
    *) die "unknown genesis block $1 (custom test network?); not supported" ;;
  esac
}

# ---------------------------------------------------------------- capture

cmd_capture() {
  local url="" cookie="" out=""
  while (($#)); do
    case "$1" in
      --rpc) url="${2:?}"; shift 2 ;;
      --rpc-cookie-file) cookie="${2:?}"; shift 2 ;;
      --out) out="${2:?}"; shift 2 ;;
      -h | --help) usage 0 ;;
      *) die "capture: unknown argument $1" ;;
    esac
  done
  [[ -n "${url}" ]] || die "capture: --rpc URL is required"
  need curl
  need jq

  local bci height hash info build genesis network
  bci="$(rpc_result "${url}" "${cookie}" getblockchaininfo)"
  genesis="$(rpc_result "${url}" "${cookie}" getblockhash "[0]")"
  network="$(net_from_genesis "$(jq -r '.' <<<"${genesis}")")"
  height="$(jq -r '.blocks' <<<"${bci}")"
  hash="$(jq -r '.bestblockhash' <<<"${bci}")"
  info="$(rpc_result "${url}" "${cookie}" getinfo)"
  build="$(jq -r '.build // .subversion // "unknown"' <<<"${info}")"
  check_height "${height}"
  hash="$(norm_hash "${hash}")"

  # Cross-check: the hash at that height, asked separately, must agree.
  local again
  again="$(rpc_result "${url}" "${cookie}" getblockhash "[${height}]")"
  [[ "$(jq -r '.' <<<"${again}")" == "${hash}" ]] ||
    die "tip moved while capturing (getblockhash ${height} != bestblockhash); run capture again"

  local json
  json="$(jq -n --arg network "${network}" --argjson height "${height}" \
    --arg hash "${hash}" --arg zebra_version "${build}" \
    --arg captured_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{network:$network, height:$height, hash:$hash, zebra_version:$zebra_version, captured_at:$captured_at}')"
  if [[ -n "${out}" ]]; then
    echo "${json}" >"${out}"
    info "captured height ${height} hash ${hash} -> ${out}"
  else
    echo "${json}"
  fi
  info "next: wait ~10 s (zebrad writes its non-finalized backup at most every 5 s), stop zebrad, then run create"
}

# ---------------------------------------------------------------- liveness

# Refuses if anything still holds the RocksDB database under $1 (the db dir).
# RocksDB takes a POSIX fcntl write lock on LOCK for as long as the db is open.
assert_db_not_open() {
  local db="$1" state_dir="$2" rpc_url="$3" lock="$1/LOCK" checked=0
  # RocksDB creates LOCK on first open and never deletes it; no LOCK = never opened.
  [[ -e "${lock}" ]] || checked=1

  if [[ -e "${lock}" ]] && command -v python3 >/dev/null 2>&1; then
    checked=1
    # Probe with a shared (read) lock: it conflicts with RocksDB's write lock,
    # needs only read permission, and works across users on the same kernel.
    if ! python3 - "${lock}" <<'PY'
import fcntl, sys
with open(sys.argv[1], "rb") as f:
    try:
        fcntl.lockf(f, fcntl.LOCK_SH | fcntl.LOCK_NB)
    except OSError:
        sys.exit(3)
PY
    then
      die "the database at ${db} is LOCKED by a running process. Stop that zebrad first (never snapshot a live RocksDB)."
    fi
  fi

  if [[ -e "${lock}" ]] && command -v lsof >/dev/null 2>&1; then
    checked=1
    local holders pid comm
    holders="$(lsof -t "${lock}" 2>/dev/null || true)"
    while IFS= read -r pid; do
      [[ -n "${pid}" ]] || continue
      comm="$(ps -o comm= -p "${pid}" 2>/dev/null || true)"
      case "${comm}" in
        # Docker Desktop's VM keeps bind-mounted files open (virtiofs) even after the
        # container is gone. It is not a zebrad; the container check below decides.
        *com.apple.Virtualization.VirtualMachine* | *com.docker.* | *qemu-system*) continue ;;
      esac
      die "process ${pid} (${comm}) has ${lock} open. Stop that zebrad first."
    done <<<"${holders}"
  fi

  ((checked)) || die "need python3 or lsof to prove the database is not open; install one"

  # A zebrad inside a Docker Desktop VM holds its lock in the VM's kernel, which the
  # host cannot see. Refuse if a running container bind-mounts this directory.
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    local sd id src csrc ids mounts
    sd="$(canon_dir "${state_dir}")"
    ids="$(docker ps -q)"
    while IFS= read -r id; do
      [[ -n "${id}" ]] || continue
      mounts="$(docker inspect --format '{{range .Mounts}}{{println .Source}}{{end}}' "${id}")"
      while IFS= read -r src; do
        [[ -n "${src}" ]] || continue
        csrc="$(canon_dir "${src}" || echo "${src}")"
        if [[ "${sd}" == "${csrc}" || "${sd}" == "${csrc}"/* || "${csrc}" == "${sd}"/* ]]; then
          die "running container $(docker inspect --format '{{.Name}}' "${id}") mounts ${src}; stop it first"
        fi
      done <<<"${mounts}"
    done <<<"${ids}"
  fi

  if [[ -n "${rpc_url}" ]]; then
    need curl
    local body=""
    body="$(rpc "${rpc_url}" "" getblockcount 2>/dev/null)" || true
    if [[ -n "${body}" ]]; then
      die "zebrad RPC at ${rpc_url} still answers; stop the node first"
    fi
  fi
}

# ---------------------------------------------------------------- create

cmd_create() {
  (($# >= 2)) || usage 1
  local state_dir="$1" out_dir="$2"
  shift 2
  local capture="" height="" hash="" zver="" network="" rpc_url=""
  while (($#)); do
    case "$1" in
      --capture) capture="${2:?}"; shift 2 ;;
      --height) height="${2:?}"; shift 2 ;;
      --hash) hash="${2:?}"; shift 2 ;;
      --zebra-version) zver="${2:?}"; shift 2 ;;
      --network) network="${2:?}"; shift 2 ;;
      --rpc) rpc_url="${2:?}"; shift 2 ;;
      -h | --help) usage 0 ;;
      *) die "create: unknown argument $1" ;;
    esac
  done
  need jq
  need tar
  [[ -d "${state_dir}" ]] || die "state dir ${state_dir} does not exist"
  state_dir="$(canon_dir "${state_dir}")"
  [[ -d "${state_dir}/state" ]] || die "${state_dir} has no state/ subdirectory; pass zebrad's state.cache_dir"

  # Metadata: captured while the node ran (see `capture`), or given by hand.
  local cap_net=""
  if [[ -n "${capture}" ]]; then
    [[ -r "${capture}" ]] || die "cannot read ${capture}"
    [[ -z "${height}${hash}" ]] || die "use either --capture or --height/--hash, not both"
    height="$(jq -r '.height' "${capture}")"
    hash="$(jq -r '.hash' "${capture}")"
    cap_net="$(jq -r '.network // empty' "${capture}")"
    [[ -n "${zver}" ]] || zver="$(jq -r '.zebra_version // empty' "${capture}")"
  fi
  [[ -n "${height}" && -n "${hash}" ]] ||
    die "need the tip that was captured while the node ran: --capture FILE, or --height H --hash HASH"
  check_height "${height}"
  hash="$(norm_hash "${hash}")"
  [[ -n "${zver}" ]] || zver="unknown"

  # Locate state/vN/<network>.
  local vdir="" v
  for v in "${state_dir}"/state/v*; do
    [[ -d "${v}" && "$(basename "${v}")" =~ ^v[0-9]+$ ]] || continue
    if [[ -z "${vdir}" || "${v##*/v}" -gt "${vdir##*/v}" ]]; then vdir="${v}"; fi
  done
  [[ -n "${vdir}" ]] || die "no state/vN directory under ${state_dir}"
  if [[ -z "${network}" ]]; then
    network="${cap_net}"
  fi
  if [[ -z "${network}" ]]; then
    local nets=() n
    for n in "${vdir}"/*/; do nets+=("$(basename "${n}")"); done
    ((${#nets[@]} == 1)) || die "found networks (${nets[*]}) under ${vdir}; pass --network"
    network="${nets[0]}"
  fi
  [[ -z "${cap_net}" || "${cap_net}" == "${network}" ]] ||
    die "capture file is for ${cap_net} but --network is ${network}"
  case "${network}" in
    testnet | regtest) ;;
    mainnet) die "mainnet snapshots are not published yet (infra-2 D6: testnet first)" ;;
    *) die "unsupported network ${network}" ;;
  esac
  local db="${vdir}/${network}"
  [[ -d "${db}" ]] || die "no database at ${db}"
  [[ -f "${db}/CURRENT" ]] || die "${db} is not a RocksDB database (no CURRENT)"
  [[ -f "${db}/version" ]] || die "${db}/version missing; not a zebrad state database"
  local major="${vdir##*/v}" state_version
  state_version="$(tr -d '[:space:]' <"${db}/version")"
  # Older zebrad wrote only minor.patch into this file.
  [[ "${state_version}" == "${major}".* ]] || state_version="${major}.${state_version}"

  assert_db_not_open "${db}" "${state_dir}" "${rpc_url}"

  local nf_rel="non_finalized_state/${network}" nf_note
  if [[ -f "${state_dir}/${nf_rel}/${hash}" ]]; then
    nf_note="block ${hash} is in the non-finalized backup: capture and state dir agree"
  elif [[ -d "${state_dir}/${nf_rel}" ]]; then
    nf_note="block ${hash} is not in the non-finalized backup (captured deep, or stopped <5 s after it arrived); check with a test restore"
    warn "${nf_note}"
  else
    nf_note="no non-finalized backup directory; the restored tip will be the finalized tip (up to ~100 blocks below the captured tip)"
    warn "${nf_note}"
  fi

  # Output dir must be new or empty: one snapshot per directory.
  mkdir -p "${out_dir}"
  out_dir="$(canon_dir "${out_dir}")"
  [[ -z "$(ls -A "${out_dir}")" ]] || die "out dir ${out_dir} is not empty"
  case "${out_dir}/" in
    "${state_dir}"/*) die "out dir must not be inside the state dir" ;;
  esac

  # File list: RocksDB files minus LOCK (recreated on open) and LOG* (info logs:
  # host paths, not needed), plus the non-finalized backup. Nothing else.
  local list="${out_dir}/.files" f rel
  : >"${list}"
  local excluded=()
  while IFS= read -r f; do
    rel="${f#"${state_dir}"/}"
    case "$(basename "${f}")" in
      LOCK | LOG | LOG.old.*) excluded+=("${rel}") ;;
      *) echo "${rel}" >>"${list}" ;;
    esac
  done < <(find "${db}" -type f | LC_ALL=C sort)
  if [[ -d "${state_dir}/${nf_rel}" ]]; then
    find "${state_dir}/${nf_rel}" -type f | LC_ALL=C sort | sed "s|^${state_dir}/||" >>"${list}"
  fi
  if [[ -n "$(find "${db}" "${state_dir}/${nf_rel}" ! -type f ! -type d 2>/dev/null)" ]]; then
    die "unexpected non-regular files (symlinks?) in the state; refusing"
  fi
  # Everything else in the cache dir stays behind; say what, so nobody is surprised.
  local other
  other="$(cd "${state_dir}" && find . -mindepth 1 -maxdepth 3 \
    ! -path './state' ! -path "./state/v${major}" ! -path "./state/v${major}/${network}*" \
    ! -path './non_finalized_state' ! -path "./${nf_rel}*" | LC_ALL=C sort)"
  [[ -z "${other}" ]] || info "left out of the archive (peers, cookie, other networks/versions):"$'\n'"${other}"
  ((${#excluded[@]} == 0)) || info "left out: ${excluded[*]}"

  # Space check: archive <= state size (SSTs are already LZ4-compressed).
  local need_kb avail_kb df_out
  need_kb="$(cd "${state_dir}" && du -sk "state/v${major}/${network}" | awk '{print $1}')"
  df_out="$(df -Pk "${out_dir}")"
  avail_kb="$(awk 'NR==2{print $4}' <<<"${df_out}")"
  ((avail_kb > need_kb + 102400)) ||
    die "not enough space in ${out_dir}: need ~$((need_kb / 1024)) MB, have $((avail_kb / 1024)) MB"

  local comp ext
  if command -v zstd >/dev/null 2>&1; then
    comp=zstd ext=tar.zst
  else
    comp=gzip ext=tar.gz
    warn "zstd not found; falling back to gzip (bigger, slower). Install zstd for published snapshots."
  fi
  local name="zebrad-${network}-${height}.${ext}"
  local archive="${out_dir}/${name}"

  local -a tarflags=()
  local tarver
  tarver="$(tar --version 2>/dev/null || true)"
  if [[ "${tarver}" == *bsdtar* ]]; then
    tarflags=(--no-mac-metadata --no-xattrs --no-acls)
  fi
  info "archiving $(wc -l <"${list}" | tr -d ' ') files from ${state_dir} ($((need_kb / 1024)) MB) with ${comp}..."
  if [[ "${comp}" == zstd ]]; then
    COPYFILE_DISABLE=1 tar "${tarflags[@]+"${tarflags[@]}"}" -C "${state_dir}" -cf - -T "${list}" |
      zstd -q -T0 -3 -o "${archive}.partial"
  else
    COPYFILE_DISABLE=1 tar "${tarflags[@]+"${tarflags[@]}"}" -C "${state_dir}" -cf - -T "${list}" |
      gzip -6 >"${archive}.partial"
  fi
  mv "${archive}.partial" "${archive}"
  rm -f "${list}"

  local sha size
  sha="$(sha256_of "${archive}")"
  size="$(file_size "${archive}")"
  echo "${sha}  ${name}" >"${out_dir}/SHA256SUMS"
  jq -n --arg network "${network}" --arg zebra_version "${zver}" --arg state_version "${state_version}" \
    --argjson height "${height}" --arg hash "${hash}" \
    --arg created_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg sha256 "${sha}" --argjson size "${size}" \
    --arg archive "${name}" --arg compression "${comp}" --argjson state_bytes "$((need_kb * 1024))" \
    --arg check "${nf_note}" \
    '{network:$network, zebra_version:$zebra_version, state_version:$state_version,
      height:$height, hash:$hash, created_at:$created_at, sha256:$sha256, size:$size,
      archive:$archive, compression:$compression, state_bytes_approx:$state_bytes,
      contents:["state/v\($state_version|split(".")[0])/\($network)/ (minus LOCK, LOG*)",
                "non_finalized_state/\($network)/"],
      offline_check:$check}' >"${out_dir}/snapshot.json"

  echo "archive:  ${archive}"
  echo "size:     ${size} bytes"
  echo "sha256:   ${sha}"
  echo "block:    height ${height} hash ${hash}"
  echo "manifest: ${out_dir}/snapshot.json"
  echo "sums:     ${out_dir}/SHA256SUMS"
  echo "You can restart the node now. Before publishing, test-restore and run verify."
}

# ---------------------------------------------------------------- restore

cmd_restore() {
  (($# >= 2)) || usage 1
  local archive="$1" target="$2"
  shift 2
  local sums="" manifest=""
  while (($#)); do
    case "$1" in
      --sums) sums="${2:?}"; shift 2 ;;
      --manifest) manifest="${2:?}"; shift 2 ;;
      -h | --help) usage 0 ;;
      *) die "restore: unknown argument $1" ;;
    esac
  done
  need tar
  [[ -f "${archive}" ]] || die "archive ${archive} not found"
  local adir name
  adir="$(canon_dir "$(dirname "${archive}")")"
  name="$(basename "${archive}")"
  archive="${adir}/${name}"
  [[ -n "${sums}" ]] || sums="${adir}/SHA256SUMS"
  [[ -n "${manifest}" ]] || manifest="${adir}/snapshot.json"

  # Refuse a non-empty target before spending minutes hashing.
  if [[ -e "${target}" ]]; then
    [[ -d "${target}" ]] || die "target ${target} exists and is not a directory"
    [[ -z "$(ls -A "${target}")" ]] ||
      die "target ${target} is not empty; restore only into a new or empty directory"
  fi

  # 1. Checksum.
  [[ -f "${sums}" ]] || die "no SHA256SUMS at ${sums}; download it next to the archive (or pass --sums)"
  local want="" line
  while IFS= read -r line; do
    if [[ "${line}" =~ ^([0-9a-fA-F]{64})[[:space:]]+\*?(.+)$ && "${BASH_REMATCH[2]}" == "${name}" ]]; then
      want="$(tr '[:upper:]' '[:lower:]' <<<"${BASH_REMATCH[1]}")"
    fi
  done <"${sums}"
  [[ -n "${want}" ]] || die "${sums} has no entry for ${name}"
  info "checking sha256 of ${name}..."
  local got
  got="$(sha256_of "${archive}")"
  [[ "${got}" == "${want}" ]] ||
    die "CHECKSUM MISMATCH for ${name}: expected ${want}, got ${got}. Do not use this file."
  info "sha256 OK: ${got}"

  # 2. Manifest (optional, but it is where height/hash come from).
  local height="" hash="" network="" zver="" sver="" sbytes=""
  if [[ -f "${manifest}" ]]; then
    need jq
    [[ "$(jq -r '.sha256' "${manifest}")" == "${got}" ]] ||
      die "snapshot.json sha256 does not match the archive; manifest and archive are from different snapshots"
    height="$(jq -r '.height' "${manifest}")"
    hash="$(jq -r '.hash' "${manifest}")"
    network="$(jq -r '.network' "${manifest}")"
    zver="$(jq -r '.zebra_version' "${manifest}")"
    sver="$(jq -r '.state_version' "${manifest}")"
    sbytes="$(jq -r '.state_bytes_approx // empty' "${manifest}")"
  else
    warn "no snapshot.json next to the archive; get the height and hash from the publisher"
  fi

  mkdir -p "${target}"
  target="$(canon_dir "${target}")"
  if [[ -n "${sbytes}" ]]; then
    local avail_kb df_out
    df_out="$(df -Pk "${target}")"
    avail_kb="$(awk 'NR==2{print $4}' <<<"${df_out}")"
    ((avail_kb * 1024 > sbytes + 104857600)) ||
      die "not enough space in ${target}: need ~$((sbytes / 1048576)) MB, have $((avail_kb / 1024)) MB"
  fi

  # 3. Extract. Both GNU tar and bsdtar refuse absolute paths and '..' by default.
  info "extracting into ${target}..."
  case "${name}" in
    *.tar.zst) need zstd; zstd -q -dc "${archive}" | tar -C "${target}" -xf - ;;
    *.tar.gz) gzip -dc "${archive}" | tar -C "${target}" -xf - ;;
    *) die "unknown archive type ${name} (want .tar.zst or .tar.gz)" ;;
  esac

  # 4. Shape check: only state/ and non_finalized_state/, only files and dirs.
  local top
  top="$(ls -A "${target}")"
  while IFS= read -r line; do
    [[ "${line}" == state || "${line}" == non_finalized_state ]] ||
      die "unexpected top-level entry '${line}' in archive; delete ${target} and do not use this snapshot"
  done <<<"${top}"
  [[ -z "$(find "${target}" ! -type f ! -type d)" ]] ||
    die "archive contained links or special files; delete ${target} and do not use this snapshot"
  local vfile
  vfile="$(find "${target}/state" -mindepth 3 -maxdepth 3 -name version -type f)"
  [[ -n "${vfile}" ]] || die "no state/vN/<network>/version in archive; not a zebrad snapshot"

  echo "restored: ${target}"
  echo "database: ${vfile%/version}"
  if [[ -n "${height}" ]]; then
    echo "network:  ${network}   zebra: ${zver}   state format: ${sver}"
    echo "block:    height ${height}   hash ${hash}"
  fi
  cat <<EOF

NEXT (the snapshot is only a sync shortcut; this check is what makes it safe):
  1. Point zebrad at it: [state] cache_dir must be the path where zebrad SEES
     ${target}: that path itself for a native zebrad; in Docker, the mount
     target it is mounted on (docs/guides/testnet.md mounts it on
     /var/lib/sova/zebrad, and its zebrad.toml already says so). Same network,
     and a zebrad whose state format major version matches (${sver:-see snapshot.json}).
     The restored files belong to the user who ran this; the Docker image runs
     zebrad as uid 10001 (Linux: sudo chown -R 10001:10001 ${target}).
  2. Start zebrad and let it come up (it restores the non-finalized blocks and
     checks the hard-coded checkpoints below the tip).
  3. Check the published block on YOUR node:
       ${PROG} verify --rpc http://127.0.0.1:<rpc-port> --manifest ${manifest}
  4. Compare that hash with an INDEPENDENT source: a public Zcash ${network:-testnet}
     block explorer, or 'getblockhash ${height:-<height>}' on a second node you run.
     If it differs, stop zebrad, delete ${target} and full-sync instead.
EOF
}

# ---------------------------------------------------------------- verify

cmd_verify() {
  local url="" cookie="" manifest="" height="" hash="" ref="" refcookie=""
  while (($#)); do
    case "$1" in
      --rpc) url="${2:?}"; shift 2 ;;
      --rpc-cookie-file) cookie="${2:?}"; shift 2 ;;
      --manifest) manifest="${2:?}"; shift 2 ;;
      --height) height="${2:?}"; shift 2 ;;
      --hash) hash="${2:?}"; shift 2 ;;
      --reference-rpc) ref="${2:?}"; shift 2 ;;
      --reference-cookie-file) refcookie="${2:?}"; shift 2 ;;
      -h | --help) usage 0 ;;
      *) die "verify: unknown argument $1" ;;
    esac
  done
  [[ -n "${url}" ]] || die "verify: --rpc URL is required"
  need curl
  need jq
  if [[ -n "${manifest}" ]]; then
    height="$(jq -r '.height' "${manifest}")"
    hash="$(jq -r '.hash' "${manifest}")"
  fi
  [[ -n "${height}" && -n "${hash}" ]] || die "verify: need --manifest or --height and --hash"
  check_height "${height}"
  hash="$(norm_hash "${hash}")"

  local bci tip tiphash got r
  bci="$(rpc_result "${url}" "${cookie}" getblockchaininfo)"
  tip="$(jq -r '.blocks' <<<"${bci}")"
  tiphash="$(jq -r '.bestblockhash' <<<"${bci}")"
  echo "node tip: height ${tip} hash ${tiphash}"
  ((tip >= height)) || die "node tip ${tip} is below the snapshot block ${height}; is it the restored node?"
  r="$(rpc_result "${url}" "${cookie}" getblockhash "[${height}]")"
  got="$(jq -r '.' <<<"${r}")"
  [[ "${got}" == "${hash}" ]] ||
    die "MISMATCH at height ${height}: node has ${got}, manifest says ${hash}"
  echo "node block ${height}: ${got}  == manifest  OK"

  if [[ -n "${ref}" ]]; then
    local refhash
    r="$(rpc_result "${ref}" "${refcookie}" getblockhash "[${height}]")"
    refhash="$(jq -r '.' <<<"${r}")"
    [[ "${refhash}" == "${hash}" ]] ||
      die "MISMATCH: reference node ${ref} has ${refhash} at height ${height}"
    echo "reference block ${height}: ${refhash}  == manifest  OK"
  else
    echo "Now compare ${hash} (height ${height}) with an independent explorer or a second node."
  fi
}

# ---------------------------------------------------------------- main

(($#)) || usage 1
sub="$1"
shift
case "${sub}" in
  capture) cmd_capture "$@" ;;
  create) cmd_create "$@" ;;
  restore) cmd_restore "$@" ;;
  verify) cmd_verify "$@" ;;
  -h | --help | help) usage 0 ;;
  *) die "unknown command ${sub} (capture | create | restore | verify)" ;;
esac
