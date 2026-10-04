#!/usr/bin/env bash
# tools/near-da/deploy/build-linux.sh -- build `sova-near-da` for the testnet
# hosts (Ubuntu 24.04, x86_64) from any machine with Docker.
#
# Same recipe as scripts/build-linux-release.sh (the box-binaries release
# build): Ubuntu 20.04 (glibc 2.31) so the binary runs on any newer distro,
# `-C target-cpu=x86-64-v2`, and a check that no symbol needs a newer glibc.
# The tool is small (no reth), so this is a few minutes natively on x86_64
# and ~10-20 minutes emulated on Apple Silicon.
#
# Docker Desktop on this Mac hangs on bind mounts from ~/Documents, so the
# workspace is copied to a temp dir under /private/tmp (Linux: /tmp) first
# and only that copy is mounted. Cargo's target dir and registry live in
# named Docker volumes (sova-near-da-target, sova-near-da-cargo).
#
# Usage:
#   tools/near-da/deploy/build-linux.sh <out_dir>
# Writes <out_dir>/sova-near-da (linux x86_64) and <out_dir>/SHA256SUMS.
set -euo pipefail

die() { echo "build-linux: error: $*" >&2; exit 1; }
[[ $# -eq 1 && -n "$1" ]] || die "usage: $0 <out_dir>"
command -v docker >/dev/null 2>&1 || die "docker is required"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
WS="$(cd "${HERE}/.." && pwd -P)"
mkdir -p "$1"
OUT="$(cd "$1" && pwd -P)"
GLIBC_MAX="${SOVA_GLIBC_MAX:-2.31}"
TOOLCHAIN="${RUST_TOOLCHAIN:-stable}"
IMAGE="sova-near-da-build:ubuntu20.04-${TOOLCHAIN}"
BASE=/private/tmp
[[ -d "${BASE}" ]] || BASE=/tmp
SRC="$(mktemp -d "${BASE}/sova-near-da-src.XXXXXX")"
trap 'rm -rf "${SRC}"' EXIT

rsync -a --exclude target "${WS}/" "${SRC}/near-da/"

docker build --platform linux/amd64 -t "${IMAGE}" --build-arg "TOOLCHAIN=${TOOLCHAIN}" - <<'DOCKERFILE'
FROM ubuntu:20.04
ARG TOOLCHAIN
ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential ca-certificates curl pkg-config binutils \
 && rm -rf /var/lib/apt/lists/*
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "${TOOLCHAIN}"
ENV PATH=/root/.cargo/bin:${PATH}
DOCKERFILE

# The output dir must be mountable too: build into the source copy, then
# copy out.
docker run --rm --platform linux/amd64 \
  -v "${SRC}:/src" \
  -v sova-near-da-target:/target \
  -v sova-near-da-cargo:/root/.cargo/registry \
  -e CARGO_TARGET_DIR=/target \
  -e RUSTFLAGS="-C target-cpu=x86-64-v2" \
  -e GLIBC_MAX="${GLIBC_MAX}" \
  -w /src/near-da "${IMAGE}" bash -euo pipefail -c '
    cargo build --release --locked -p sova-near-da
    need=$(objdump -T /target/release/sova-near-da | grep -o "GLIBC_[0-9.]*" | sort -uV | tail -1)
    echo "build-linux: highest glibc symbol: ${need}"
    [[ "$(printf "%s\n%s\n" "${need#GLIBC_}" "${GLIBC_MAX}" | sort -V | tail -1)" == "${GLIBC_MAX}" ]] \
      || { echo "build-linux: needs ${need}, above ${GLIBC_MAX}" >&2; exit 1; }
    /target/release/sova-near-da --version
    install -m 0755 /target/release/sova-near-da /src/sova-near-da'

install -m 0755 "${SRC}/sova-near-da" "${OUT}/sova-near-da"
(cd "${OUT}" && shasum -a 256 sova-near-da >SHA256SUMS)
echo "build-linux: ${OUT}/sova-near-da"
cat "${OUT}/SHA256SUMS"
