#!/usr/bin/env bash
# scripts/build-linux-release.sh -- build the linux-x86_64 release binaries
# (`sova` and `sova-miner`) so they run on older distributions.
#
# A binary linked on a new glibc needs that glibc (or newer) at run time:
# built on ubuntu-latest (24.04, glibc 2.39), `sova` needed GLIBC_2.38 and
# didn't start on Ubuntu 22.04, Debian 12 or RHEL 9 (stranger test
# 2026-09-25, F4). This script builds inside Ubuntu 20.04 (glibc 2.31),
# the image reth's own release builds use (cross's x86_64 image), and
# fails if either binary needs anything newer than SOVA_GLIBC_MAX. The
# result runs on Ubuntu 20.04+, Debian 11+, RHEL 9 and Amazon Linux 2023.
# (Not Debian 11: its LTS ended in August 2026 and its mirrors are
# dropping packages, which broke `apt-get install` in the container.
# Ubuntu 20.04 stays in the main archive under ESM.)
#
# CPU: `-C target-cpu=x86-64-v2` (SSE4.2/POPCNT, any x86-64 CPU since
# about 2009). This also stops the C dependencies from choosing ISA
# extensions by looking at the build machine: blst compiles its ADX/MULX
# code in when the build host has ADX, unless a target-cpu is given
# (blst's build.rs). v0.1.3 was built that way and dies with SIGILL in
# blst's mulx_mont_384 (loading the KZG setup, right after "Loaded
# storage settings") on a CPU without ADX/BMI2, e.g. `qemu-x86_64 -cpu
# Nehalem`; this build runs there.
#
# The release workflow (.github/workflows/box-binaries.yml) runs this on
# its Linux leg; it also runs locally (Docker; on an Apple Silicon Mac the
# container is emulated, so a cold build takes a while).
#
# Usage:
#   scripts/build-linux-release.sh <out_dir>
#
# Writes <out_dir>/sova and <out_dir>/sova-miner.
#
# Env:
#   SOVA_VERSION        version stamped into `--version` (the release tag)
#   SOVA_LINUX_TARGET   CARGO_TARGET_DIR for the container: an absolute host
#                       path or a Docker volume name (default: <repo>/target/linux-release)
#   SOVA_LINUX_CARGO    CARGO_HOME (registry cache) for the container: a host
#                       path or a Docker volume name (default: <repo>/target/linux-cargo-home)
#   SOVA_BUILD_IMAGE    base image (default ubuntu:20.04)
#   SOVA_GLIBC_MAX      highest GLIBC_x.y symbol version allowed (default 2.31)
#   RUST_TOOLCHAIN      rustup toolchain to install (default stable)
#   CARGO_BUILD_JOBS    passed through (cap it on a small Docker VM)

set -euo pipefail

die() {
  echo "build-linux-release: error: $*" >&2
  exit 1
}

[[ $# -eq 1 && -n "$1" ]] || die "usage: $0 <out_dir>"
command -v docker >/dev/null 2>&1 || die "docker is required"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
mkdir -p "$1"
OUT="$(cd "$1" && pwd -P)"
BASE_IMAGE="${SOVA_BUILD_IMAGE:-ubuntu:20.04}"
TOOLCHAIN="${RUST_TOOLCHAIN:-stable}"
GLIBC_MAX="${SOVA_GLIBC_MAX:-2.31}"
TARGET_MOUNT="${SOVA_LINUX_TARGET:-${REPO_ROOT}/target/linux-release}"
CARGO_MOUNT="${SOVA_LINUX_CARGO:-${REPO_ROOT}/target/linux-cargo-home}"
IMAGE="sova-linux-build:$(printf '%s' "${BASE_IMAGE}-${TOOLCHAIN}" | tr -c 'A-Za-z0-9_.-' '-')"
PLATFORM=linux/amd64

# A host path must exist before Docker mounts it (a volume name needn't).
for m in "${TARGET_MOUNT}" "${CARGO_MOUNT}"; do
  if [[ "${m}" == /* ]]; then mkdir -p "${m}"; fi
done

echo "build-linux-release: image ${IMAGE} (from ${BASE_IMAGE}, rust ${TOOLCHAIN}), glibc floor ${GLIBC_MAX}"
# Build dependencies as reth's Cross.toml lists them: clang/libclang for
# bindgen (reth-mdbx-sys), a C/C++ toolchain for RocksDB, blst, secp256k1.
docker build --platform "${PLATFORM}" -t "${IMAGE}" \
  --build-arg "BASE_IMAGE=${BASE_IMAGE}" --build-arg "TOOLCHAIN=${TOOLCHAIN}" - <<'DOCKERFILE'
ARG BASE_IMAGE=ubuntu:20.04
FROM ${BASE_IMAGE}
ARG TOOLCHAIN=stable
ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install --assume-yes --no-install-recommends \
      ca-certificates curl git build-essential pkg-config clang libclang-dev llvm-dev m4 binutils \
 && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup PATH=/opt/cargo/bin:${PATH}
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | CARGO_HOME=/opt/cargo sh -s -- -y --no-modify-path --profile minimal --default-toolchain "${TOOLCHAIN}" \
 && chmod -R a+rX /opt/rustup /opt/cargo \
 && git config --system --add safe.directory '*'
DOCKERFILE

HOST_UID="$(id -u)"
HOST_GID="$(id -g)"
run() {
  docker run --rm --platform "${PLATFORM}" \
    -v "${REPO_ROOT}:/src" -v "${TARGET_MOUNT}:/target" -v "${CARGO_MOUNT}:/cargo" -v "${OUT}:/out" \
    -w /src "$@"
}

# The mount points of fresh volumes belong to root; hand them to the
# caller so the build (and the files it leaves) are theirs.
run --user 0:0 "${IMAGE}" chown "${HOST_UID}:${HOST_GID}" /target /cargo

# shellcheck disable=SC2016 # the script below expands inside the container
run --user "${HOST_UID}:${HOST_GID}" \
  -e HOME=/tmp -e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/target \
  -e CARGO_INCREMENTAL=0 -e CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-auto}" \
  -e "RUSTFLAGS=-C target-cpu=x86-64-v2" \
  -e "SOVA_VERSION=${SOVA_VERSION:-}" \
  -e "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-}" \
  -e "GLIBC_MAX=${GLIBC_MAX}" \
  "${IMAGE}" bash -euo pipefail -c '
    [[ -n "${CARGO_BUILD_JOBS}" ]] || unset CARGO_BUILD_JOBS
    rustc --version
    ldd --version 2>&1 | sed -n 1p
    cargo build --release --locked -p sova
    # crates/burn-wallet is its own workspace; one shared target dir is fine.
    cargo build --release --locked -p sova-miner --manifest-path crates/burn-wallet/Cargo.toml
    cp /target/release/sova /target/release/sova-miner /out/
    fail=0
    for b in sova sova-miner; do
      need="$(objdump -T "/out/${b}" | grep -o "GLIBC_[0-9][0-9.]*" | sed "s/GLIBC_//" | sort -Vu | tail -1)"
      echo "${b}: needs glibc ${need} (floor ${GLIBC_MAX})"
      if [[ "$(printf "%s\n%s\n" "${need}" "${GLIBC_MAX}" | sort -V | tail -1)" != "${GLIBC_MAX}" ]]; then
        echo "${b}: glibc ${need} is above the floor ${GLIBC_MAX}" >&2
        fail=1
      fi
    done
    exit "${fail}"
  '

echo "build-linux-release: wrote ${OUT}/sova ${OUT}/sova-miner"
