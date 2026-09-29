#!/usr/bin/env bash
# Burns across a regtest NU7 activation (docs/design/nu7-readiness.md, B1/B4).
#
# Starts a throwaway, NU7-aware zebrad on a fresh regtest chain with
# NU5..NU6.3 active from height 1 and NU7 at NU7_HEIGHT, then runs
# crates/burn-wallet's `e2e_regtest_burn_across_nu7`: a burn before NU7, one
# in the activation block itself (tip on NU6.3, zebrad's `nextblock` NU7),
# and one after, each signed for the branch zebrad reports; plus a burn
# signed for the old branch at the activation block, which zebrad must
# refuse. Tears the node down on exit.
#
# Needs a zebrad that knows NU7 (branch ID 77190ad9). Until a Zebra release
# does, build one from Zebra's `nu7-zips` branch (PR #11484):
#
#   git clone -b nu7-zips https://github.com/ZcashFoundation/zebra
#   cd zebra && cargo build --release --locked --bin zebrad
#   ZEBRAD_BIN=$PWD/target/release/zebrad box/regtest/nu7-burn.sh
#
# Once an NU7 Zebra release exists (expected Oct 5), use its image instead:
#
#   ZEBRAD_IMAGE=zfnd/zebra:<nu7 release> box/regtest/nu7-burn.sh
#
# Env: ZEBRAD_BIN or ZEBRAD_IMAGE (one is required); NU7_HEIGHT (default
# 120, at least 110: 101 blocks of coinbase maturity come first); RPC_PORT
# (default 18942 -- not 18232-18235, which the box and the laptop's testnet
# node use).

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BURN_WALLET_DIR="${HERE}/../../crates/burn-wallet"
NU7_HEIGHT="${NU7_HEIGHT:-120}"
RPC_PORT="${RPC_PORT:-18942}"
CONTAINER="sova-nu7-burn-zebrad-${RPC_PORT}"
WORK="$(mktemp -d)"
ZEBRAD_PID=""

if [[ -z "${ZEBRAD_BIN:-}" && -z "${ZEBRAD_IMAGE:-}" ]]; then
  echo "error: set ZEBRAD_BIN (a zebrad binary) or ZEBRAD_IMAGE (a docker image) to an NU7-aware zebrad" >&2
  exit 2
fi

cleanup() {
  local exit_code=$?
  if [[ "${exit_code}" -ne 0 ]]; then
    echo "--- nu7-burn failed (exit ${exit_code}); last zebrad log lines ---" >&2
    if [[ -n "${ZEBRAD_PID}" ]]; then
      tail -40 "${WORK}/zebrad.log" >&2 || true
    else
      docker logs --tail 40 "${CONTAINER}" >&2 || true
    fi
  fi
  if [[ -n "${ZEBRAD_PID}" ]]; then
    kill "${ZEBRAD_PID}" 2>/dev/null || true
    wait "${ZEBRAD_PID}" 2>/dev/null || true
  elif [[ -n "${ZEBRAD_IMAGE:-}" ]]; then
    docker rm -f "${CONTAINER}" >/dev/null 2>&1 || true
  fi
  rm -rf "${WORK}"
  exit "${exit_code}"
}
trap cleanup EXIT

# The box's zebrad.toml, with every upgrade through NU6.3 at height 1 and
# NU7 at NU7_HEIGHT, and (for a native binary) its own ports and state.
rpc_listen="0.0.0.0:18232"
p2p_listen="0.0.0.0:18233"
if [[ -n "${ZEBRAD_BIN:-}" ]]; then
  rpc_listen="127.0.0.1:${RPC_PORT}"
  p2p_listen="127.0.0.1:$((RPC_PORT + 1))"
fi
cat >"${WORK}/zebrad.toml" <<EOF
[network]
network = "Regtest"
listen_addr = "${p2p_listen}"

[network.testnet_parameters.activation_heights]
NU5 = 1
NU6 = 1
"NU6.1" = 1
"NU6.2" = 1
"NU6.3" = 1
NU7 = ${NU7_HEIGHT}

[state]
ephemeral = true
cache_dir = "${WORK}/state"

[rpc]
listen_addr = "${rpc_listen}"
enable_cookie_auth = false

[mining]
miner_address = "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"

[tracing]
use_color = false
EOF

if [[ -n "${ZEBRAD_BIN:-}" ]]; then
  echo "--- starting ${ZEBRAD_BIN} (regtest, NU7 at ${NU7_HEIGHT}) on 127.0.0.1:${RPC_PORT} ---"
  "${ZEBRAD_BIN}" -c "${WORK}/zebrad.toml" start >"${WORK}/zebrad.log" 2>&1 &
  ZEBRAD_PID=$!
else
  echo "--- starting ${ZEBRAD_IMAGE} (regtest, NU7 at ${NU7_HEIGHT}) on 127.0.0.1:${RPC_PORT} ---"
  sed -i.bak 's|^cache_dir = .*$|cache_dir = "/home/zebra/.cache/zebra"|' "${WORK}/zebrad.toml"
  docker run -d --name "${CONTAINER}" -p "127.0.0.1:${RPC_PORT}:18232" \
    -v "${WORK}/zebrad.toml:/home/zebra/.config/zebrad.toml:ro" "${ZEBRAD_IMAGE}" >/dev/null
fi

rpc_url="http://127.0.0.1:${RPC_PORT}"
deadline=$((SECONDS + 120))
until curl -s -m 2 -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}' \
  "${rpc_url}" | grep -q '"result"'; do
  if [[ ${SECONDS} -ge ${deadline} ]]; then
    echo "error: zebrad RPC did not answer within 120s" >&2
    exit 1
  fi
  sleep 2
done
echo "zebrad is up: $(curl -s -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}' "${rpc_url}" |
  grep -o '"consensus":{[^}]*}')"

echo "--- running e2e_regtest_burn_across_nu7 (crates/burn-wallet) ---"
(
  cd "${BURN_WALLET_DIR}" &&
    BURN_WALLET_REGTEST_RPC="${rpc_url}" BURN_WALLET_REGTEST_NU7_HEIGHT="${NU7_HEIGHT}" \
      cargo test --test e2e_regtest_burn -- --ignored --nocapture --exact e2e_regtest_burn_across_nu7
)
echo ""
echo "NU7 BURN TEST PASSED"
