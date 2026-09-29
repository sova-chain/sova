#!/usr/bin/env bash
# infra/testnet/test/byo-dry-run.sh -- offline proof that a bring-your-own
# host (optional since 2026-09-23, when every server moved to Hetzner; e.g.
# a keeper on other hardware, docs/ops/keeper-aws.md) is a first-class kit
# host: its config validates,
# its files render and lint (and pass systemd-analyze verify with
# --systemd-verify, which needs Docker), and `launch.sh --dry-run` includes
# it in every stage that touches hosts, without ever creating it on
# Hetzner. Also checks that a pending address is refused by a real run and
# that malformed byo entries are rejected.
#
#   test/byo-dry-run.sh [--systemd-verify]
#
# No token, no SSH, no API call: every run is --dry-run, or dies on
# purpose before anything leaves this machine. Uses config.env.example
# with its (Hetzner) keeper line swapped for a byo one at a documentation
# address (198.51.100.20, RFC 5737), in a temp dir; never reads
# secrets.env. Also checks that the unmodified example is all-Hetzner.
set -uo pipefail
KIT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERIFY=()
[[ "${1:-}" == --systemd-verify ]] && VERIFY=(--systemd-verify)
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT
KEEPER_IP=198.51.100.20
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok    %s\n' "$*"; }
bad() { FAIL=$((FAIL + 1)); printf 'FAIL  %s\n' "$*"; }
has() { grep -qE -- "$1" <<<"$2"; }   # ERE text
hasF() { grep -qF -- "$1" <<<"$2"; }  # fixed text
lacks() { ! grep -qE -- "$1" <<<"$2"; }
check() { # description command...
  local d="$1"
  shift
  if "$@"; then ok "${d}"; else bad "${d}"; fi
}

# A clean environment: no tokens, no secrets file, a private out dir.
kit() {
  env -u HCLOUD_TOKEN -u CLOUDFLARE_API_TOKEN -u CLOUDFLARE_ACCOUNT_ID -u CLOUDFLARE_ZONE_ID \
    SOVA_TESTNET_SECRETS="${TMP}/no-secrets.env" SOVA_TESTNET_OUT="${TMP}/out" \
    SOVA_TESTNET_CONFIG="${CFG}" "$@"
}

# The example's keeper is a Hetzner server (the default); swap that one
# line for a byo entry (the option under test).
HZ_KEEPER="sova-keeper-1:cx23:fsn1:40:keeper"
with_keeper() { # entry -> the example with the keeper line replaced, on stdout
  sed "s|\"${HZ_KEEPER}\"|\"$1\"|" "${KIT}/config.env.example"
}
check "config: the example's keeper is a Hetzner server ('${HZ_KEEPER}')" \
  grep -q "\"${HZ_KEEPER}\"" "${KIT}/config.env.example"
check "config: the example has no byo entry (all on Hetzner by default)" \
  lacks '^[[:space:]]*"[^"]*:byo:' "$(cat "${KIT}/config.env.example")"
with_keeper "sova-keeper-1:byo:${KEEPER_IP}:40:keeper:ubuntu" >"${TMP}/config.env"
CFG="${TMP}/config.env"
check "config: the test's keeper is 'sova-keeper-1:byo:${KEEPER_IP}:40:keeper:ubuntu'" \
  grep -q "\"sova-keeper-1:byo:${KEEPER_IP}:40:keeper:ubuntu\"" "${CFG}"

# ---- check + render ---------------------------------------------------------
out="$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
check "deploy.sh check passes" grep -q '^==> config OK' <<<"${out}"
check "deploy.sh check: no 'pending' or 'no keeper' warning once the IP is filled in" \
  lacks "pending|no keeper server" "${out}"

out="$(cd "${KIT}" && kit ./deploy.sh render "${VERIFY[@]+"${VERIFY[@]}"}" 2>&1)"
RENDER_OUT="${out}"
check "deploy.sh render${VERIFY[*]:+ ${VERIFY[*]}} passes" grep -q '^==> config OK' <<<"${out}"
R="${TMP}/out/render/sova-keeper-1"
check "render: keeper gets sova-keeper.service" test -f "${R}/etc/systemd/system/sova-keeper.service"
check "render: keeper gets sova-node.service + zebrad.service" \
  test -f "${R}/etc/systemd/system/sova-node.service" -a -f "${R}/etc/systemd/system/zebrad.service"
check "render: keeper's node is in mine mode (SOVA_MINER_EVM_ADDRESS)" grep -q '^SOVA_MINER_EVM_ADDRESS=' "${R}/etc/sova/sova-node.env"
check "render: keeper's RPC profile is local" grep -q '^SOVA_RPC_PROFILE=local$' "${R}/etc/sova/sova-node.env"
check "render: keeper advertises its byo address (SOVA_NAT=extip:${KEEPER_IP})" \
  grep -q "^SOVA_NAT=extip:${KEEPER_IP}$" "${R}/etc/sova/sova-node.env"
check "render: emission schedule is flat" grep -q '^SOVA_EMISSION_SCHEDULE=flat$' "${R}/etc/sova/sova-node.env"
check "render: keeper's node has SIP-6 on" grep -q '^SOVA_SIP6=1$' "${R}/etc/sova/sova-node.env"
check "render: keeper signs with the node's copy of its miner key" \
  grep -q '^SOVA_SEALER_KEYSTORE=/var/lib/sova/sealer/keystore.json$' "${R}/etc/sova/sova-node.env"
check "render: keeper's datadir (seal journal) is persistent" grep -q '^SOVA_DATADIR=/var/lib/sova/node$' "${R}/etc/sova/sova-node.env"
sip6_follower() { grep -q '^SOVA_SIP6=1$' "$1" && ! grep -q SEALER "$1"; }
for h in sova-seed-1 sova-rpc-1; do
  check "render: ${h}'s node has SIP-6 on, no sealing key" sip6_follower "${TMP}/out/render/${h}/etc/sova/sova-node.env"
done
for h in sova-seed-1 sova-rpc-1 sova-keeper-1; do
  check "render: ${h}'s node has SIP-7 on" grep -q '^SOVA_SIP7=1$' "${TMP}/out/render/${h}/etc/sova/sova-node.env"
done
check "render: keeper budgets rendered" grep -q '^KEEPER_LIFETIME_BUDGET_ZAT=' "${R}/etc/sova/keeper.env"
check "render: keeper burns at most every 30 s by default" \
  grep -qx 'KEEPER_MIN_BURN_INTERVAL_SECS=30' "${R}/etc/sova/keeper.env"
check "render: sova-keeper.service passes the burn interval" \
  grep -qF -- "--min-burn-interval-secs \${KEEPER_MIN_BURN_INTERVAL_SECS}" "${R}/etc/systemd/system/sova-keeper.service"
# Private hosts can't be reached by discovery (their firewall admits SSH
# only), so they dial the seeds as static peers; the seed dials no one.
for h in sova-rpc-1 sova-keeper-1; do
  check "render: ${h} static-peers the seed" grep -q '^SOVA_P2P_PEERS=enode://' "${TMP}/out/render/${h}/etc/sova/sova-node.env"
done
check "render: the seed has no static peers" grep -q '^SOVA_P2P_PEERS=$' "${TMP}/out/render/sova-seed-1/etc/sova/sova-node.env"
check "render: no cloudflared on the keeper (not public)" test ! -e "${R}/etc/systemd/system/cloudflared.service"
if [[ ${#VERIFY[@]} -gt 0 ]]; then
  check "systemd-analyze verify: keeper ok" grep -q 'ok   sova-keeper-1: systemd-analyze verify' <<<"${out}"
  check "systemd-analyze verify: faucet ok (incl. sova-checkout-relayer)" \
    grep -q 'ok   sova-faucet-1: systemd-analyze verify: .*sova-checkout-relayer.service' <<<"${out}"
fi

# ---- the checkout relayer (CHECKOUT_RELAYER=1 in the example) ------------------
F="${TMP}/out/render/sova-faucet-1"
FU="${F}/etc/systemd/system/sova-checkout-relayer.service"
FE="${F}/etc/sova/checkout-relayer.env"
check "render: faucet gets sova-checkout-relayer.service" test -f "${FU}"
check "render: faucet gets /etc/sova/checkout-relayer.env" test -f "${FE}"
ASHW="$(jq -r .ashwings "${KIT}/deployments/sova-testnet.json")"
ASHW_BLOCK="$(jq -r .deployments.ashwings.block "${KIT}/deployments/sova-testnet.json")"
for kv in HOST=127.0.0.1 PORT=18791 CORS_ORIGIN=https://sova.io TRUST_PROXY_HEADER=cf-connecting-ip \
  SOVA_RPC_URL=https://rpc.testnet.sova.io RPC_PER_10S=30 ZCASH_RPC_URL=http://127.0.0.1:18232 ZCASH_NET=test LISTINGS=1 \
  "ASHWINGS=${ASHW}" "START_BLOCK=${ASHW_BLOCK}" STATE_FILE=/var/lib/sova/checkout-relayer/state.json \
  MAX_OPEN_RESERVATIONS=20 RESERVE_PER_IP_PER_HOUR=3 MIN_BALANCE_WEI=100000000000000000; do
  check "render: checkout-relayer.env has ${kv}" grep -qxF "${kv}" "${FE}"
done
check "render: no RELAYER_KEY in any rendered file" lacks '^RELAYER_KEY=' "$(find "${TMP}/out/render" -type f -exec cat {} +)"
check "render: the relayer's key file is not rendered (made on the host)" test ! -e "${F}/etc/sova/checkout-relayer.key.env"
check "render: lint notes the key file is made on the host" has "checkout-relayer.key.env is the relayer's hot key" "${out}"
check "render: relayer runs unprivileged (User=sova-checkout)" grep -qx 'User=sova-checkout' "${FU}"
check "render: relayer reads its key from /etc/sova/checkout-relayer.key.env" \
  grep -qx 'EnvironmentFile=/etc/sova/checkout-relayer.key.env' "${FU}"
check "render: relayer runs /opt/sova-checkout-relayer with the system node" \
  grep -qx 'ExecStart=/usr/bin/node /opt/sova-checkout-relayer/src/server.mjs' "${FU}"
check "render: relayer can write only its state dir" grep -qx 'ReadWritePaths=/var/lib/sova/checkout-relayer' "${FU}"
# Block-latency alerts (host/health.sh): every host's health env carries
# the thresholds and knows SIP-6 is on (the null-run check needs it). The
# null-run rule is time-based (NULL_SEALED_MAX_MIN); the old block count
# (NULL_RUN_ALERT) is no longer rendered anywhere.
for h in sova-seed-1 sova-rpc-1 sova-keeper-1 sova-faucet-1; do
  for kv in BLOCK_AGE_ALERT_MIN=10 NULL_SEALED_MAX_MIN=45 SOVA_SIP6=1; do
    check "render: ${h} health has ${kv}" grep -qx "${kv}" "${TMP}/out/render/${h}/etc/sova/host.env"
  done
  check "render: ${h} host.env (deploy.sh) has NULL_SEALED_MAX_MIN=45" grep -qx 'NULL_SEALED_MAX_MIN=45' "${TMP}/out/render/${h}/host.env"
  check "render: ${h} renders no NULL_RUN_ALERT" lacks '^NULL_RUN_ALERT=' "$(cat "${TMP}/out/render/${h}/host.env" "${TMP}/out/render/${h}/etc/sova/host.env")"
done
check "render: faucet health knows the relayer" grep -qx 'CHECKOUT_RELAYER_PORT=18791' "${F}/etc/sova/host.env"
check "render: the relayer's SOVA drip is on, for chain 82330 only" \
  grep -qx 'DRIP=1' "${F}/etc/sova/checkout-relayer.env"
check "render: ... DRIP_CHAIN_IDS=82330" grep -qx 'DRIP_CHAIN_IDS=82330' "${F}/etc/sova/checkout-relayer.env"
for h in sova-seed-1 sova-rpc-1 sova-keeper-1; do
  check "render: no checkout relayer on ${h}" test ! -e "${TMP}/out/render/${h}/etc/systemd/system/sova-checkout-relayer.service"
done
out="$(cd "${KIT}" && kit ./cloudflare.sh --dry-run tunnels ratelimit 2>&1)"
check "cloudflare.sh tunnels: checkout host on the faucet tunnel, relayer paths only" \
  hasF '{"hostname":"checkout.testnet.sova.io","path":"^/(reserve|claim|drip|status(/[0-9]{1,30})?)$","service":"http://127.0.0.1:18791"' "${out}"
check "cloudflare.sh tunnels: faucet routes unchanged" \
  hasF '{"hostname":"faucet.testnet.sova.io","path":"^/(drip|status)$","service":"http://127.0.0.1:18790"' "${out}"
check "cloudflare.sh tunnels: proxied CNAME for the checkout host" \
  hasF '"name":"checkout.testnet.sova.io","content":"<sova-testnet-sova-faucet-1-id>.cfargotunnel.com","proxied":true' "${out}"
check "cloudflare.sh ratelimit: /reserve and /claim in the WAF rule" \
  hasF 'or (http.request.uri.path eq \"/reserve\") or (http.request.uri.path eq \"/claim\")' "${out}"
out="$(cd "${KIT}" && kit ./deploy.sh --dry-run 2>&1)"
check "deploy.sh --dry-run: relayer code goes to the faucet host" \
  hasF "+ rsync tools/checkout-relayer/{src,package.json,package-lock.json} -> sova-admin@sova-faucet-1:" "${out}"
check "deploy.sh --dry-run: ... and to no other host" test "$(grep -c 'rsync tools/checkout-relayer' <<<"${out}")" == 1
relayer_cfg() { # sed-expr -> a config with it applied
  sed "$1" "${TMP}/config.env" >"${TMP}/relayer.env"
  CFG="${TMP}/relayer.env"
}
relayer_cfg 's/^CHECKOUT_RELAYER=1$/CHECKOUT_RELAYER=0/'
out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-off" 2>&1)"
check "CHECKOUT_RELAYER=0: renders no relayer" test ! -e "${TMP}/render-off/sova-faucet-1/etc/systemd/system/sova-checkout-relayer.service"
check "CHECKOUT_RELAYER=0: no checkout host on the tunnel" lacks "checkout.testnet" "$(cd "${KIT}" && kit ./cloudflare.sh --dry-run tunnels 2>&1)"
relayer_cfg 's|^CHECKOUT_HOST=.*|CHECKOUT_HOST="checkout.example.org"|'
check "refuses a CHECKOUT_HOST outside the zone" has "CHECKOUT_HOST 'checkout.example.org' is not under the zone" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
relayer_cfg 's|^CHECKOUT_RELAYER_CORS_ORIGIN=.*|CHECKOUT_RELAYER_CORS_ORIGIN="*"|'
check "refuses CORS '*' for the relayer" has "CHECKOUT_RELAYER_CORS_ORIGIN must be one https origin" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
relayer_cfg 's|^CHECKOUT_RELAYER_RPC_PER_10S=.*|CHECKOUT_RELAYER_RPC_PER_10S=50|'
check "refuses an RPC budget at or over the edge's per-IP limit" has "CHECKOUT_RELAYER_RPC_PER_10S must stay under" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
relayer_cfg 's|^CHECKOUT_RELAYER_PORT=.*|CHECKOUT_RELAYER_PORT=18790|'
check "refuses a relayer port that is already used" has "CHECKOUT_RELAYER_PORT 18790 is used twice" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
relayer_cfg 's|^CHECKOUT_RELAYER_ALERT_BALANCE_WEI=.*|CHECKOUT_RELAYER_ALERT_BALANCE_WEI=1|'
check "refuses an alert floor under the refusal floor" has "ALERT_BALANCE_WEI must be above" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
CFG="${TMP}/config.env"

# ---- SOVA_SIP7 validation -------------------------------------------------------
sed 's/^SOVA_SIP7=1$/SOVA_SIP7=yes/' "${TMP}/config.env" >"${TMP}/sip7-bad.env"
CFG="${TMP}/sip7-bad.env"
out="$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
check "deploy.sh check refuses SOVA_SIP7=yes" has "SOVA_SIP7 must be 0 or 1" "${out}"
sed 's/^SOVA_SIP7=1$/SOVA_SIP7=0/' "${TMP}/config.env" >"${TMP}/sip7-off.env"
CFG="${TMP}/sip7-off.env"
out="$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
check "deploy.sh check: SOVA_SIP7=0 is an open item, not an error" has "SOVA_SIP7=0: SIP-7 is off" "${out}"
CFG="${TMP}/config.env"

# ---- health-alert thresholds -------------------------------------------------------
CFG="${TMP}/health-bad.env"
for v in 0 -5 45m ""; do
  sed "s/^NULL_SEALED_MAX_MIN=45\$/NULL_SEALED_MAX_MIN=${v}/" "${TMP}/config.env" >"${TMP}/health-bad.env"
  if [[ -z "${v}" ]]; then # empty: the default, 45
    out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-nsm-empty" 2>&1)"
    check "NULL_SEALED_MAX_MIN empty: renders the default 45" grep -qx 'NULL_SEALED_MAX_MIN=45' "${TMP}/render-nsm-empty/sova-rpc-1/etc/sova/host.env"
  else
    check "deploy.sh check refuses NULL_SEALED_MAX_MIN=${v}" has "NULL_SEALED_MAX_MIN must be a positive number of minutes" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
  fi
done
sed 's/^NULL_SEALED_MAX_MIN=45$/NULL_SEALED_MAX_MIN=90/' "${TMP}/config.env" >"${TMP}/health-nsm.env"
CFG="${TMP}/health-nsm.env"
out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-nsm" 2>&1)"
check "NULL_SEALED_MAX_MIN=90: rendered into every host's health env" \
  test "$(grep -lx NULL_SEALED_MAX_MIN=90 "${TMP}"/render-nsm/*/etc/sova/host.env | wc -l | tr -d ' ')" == 4
# An older config.env still naming NULL_RUN_ALERT (even an out-of-range
# one): accepted with a deprecation warning, ignored, not rendered.
{ grep -v '^NULL_SEALED_MAX_MIN=' "${TMP}/config.env"; echo 'NULL_RUN_ALERT=500'; } >"${TMP}/health-nra.env"
CFG="${TMP}/health-nra.env"
out="$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
check "deploy.sh check: NULL_RUN_ALERT is accepted (config OK)" grep -q '^==> config OK' <<<"${out}"
check "deploy.sh check: ... with a deprecation warning" has "NULL_RUN_ALERT is deprecated and ignored" "${out}"
out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-nra" 2>&1)"
check "NULL_RUN_ALERT config: renders NULL_SEALED_MAX_MIN=45 and no NULL_RUN_ALERT" \
  bash -c "grep -qx NULL_SEALED_MAX_MIN=45 '${TMP}/render-nra/sova-seed-1/etc/sova/host.env' && ! grep -q '^NULL_RUN_ALERT=' '${TMP}/render-nra/sova-seed-1/host.env' '${TMP}/render-nra/sova-seed-1/etc/sova/host.env'"
CFG="${TMP}/health-bad.env"
sed 's/^BLOCK_AGE_ALERT_MIN=10$/BLOCK_AGE_ALERT_MIN=0/' "${TMP}/config.env" >"${TMP}/health-bad.env"
check "deploy.sh check refuses BLOCK_AGE_ALERT_MIN=0" has "BLOCK_AGE_ALERT_MIN must be a positive number" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
grep -v '^BLOCK_AGE_ALERT_MIN=\|^NULL_SEALED_MAX_MIN=\|^MEM_ALERT_MB=\|^HEALTH_NETWORK_ALERT_HOSTS=\|^SWAP_GB=\|^KEEPER_ISOLATED_MIN=' "${TMP}/config.env" >"${TMP}/health-old.env"
CFG="${TMP}/health-old.env"
check "deploy.sh check: an older config without the health section still passes" grep -q '^==> config OK' <<<"$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-old" 2>&1)"
check "older config: renders the defaults (SWAP_GB=4, MEM_ALERT_MB=300, NULL_SEALED_MAX_MIN=45, KEEPER_ISOLATED_MIN=5)" \
  bash -c "grep -qx SWAP_GB=4 '${TMP}/render-old/sova-seed-1/host.env' && grep -qx MEM_ALERT_MB=300 '${TMP}/render-old/sova-seed-1/etc/sova/host.env' && grep -qx NULL_SEALED_MAX_MIN=45 '${TMP}/render-old/sova-seed-1/etc/sova/host.env' && grep -qx KEEPER_ISOLATED_MIN=5 '${TMP}/render-old/sova-keeper-1/etc/sova/host.env'"
check "older config: network alerts from the rpc host and the first seed" \
  test "$(grep -lx HEALTH_NETWORK_ALERTS=1 "${TMP}"/render-old/*/etc/sova/host.env | awk -F/ '{ print $(NF-3) }' | sort | tr '\n' ' ')" == "sova-rpc-1 sova-seed-1 "
CFG="${TMP}/config.env"

# ---- split alerts: keeper_isolated, rejecting_blocks (2026-09-28 incident) ----
# Every host's health env carries KEEPER_ISOLATED_MIN (the check itself
# runs where sova-keeper is enabled) and the keeper's node id from its
# recorded enode (empty until one is recorded, as in a plain render).
RR="${TMP}/out/render"
for h in sova-seed-1 sova-rpc-1 sova-keeper-1 sova-faucet-1; do
  check "render: ${h} host.env (deploy.sh) has KEEPER_ISOLATED_MIN=5" grep -qx 'KEEPER_ISOLATED_MIN=5' "${RR}/${h}/host.env"
  check "render: ${h} health has KEEPER_ISOLATED_MIN=5" grep -qx 'KEEPER_ISOLATED_MIN=5' "${RR}/${h}/etc/sova/host.env"
  check "render: ${h} health has an empty KEEPER_NODE_ID (no keeper enode recorded)" grep -qx 'KEEPER_NODE_ID=' "${RR}/${h}/etc/sova/host.env"
done
KID="$(printf 'a1%.0s' $(seq 1 64))"
mkdir -p "${TMP}/out/servers"
echo "enode://${KID}@${KEEPER_IP}:30303" >"${TMP}/out/servers/sova-keeper-1.enode"
out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-kid" 2>&1)"
check "keeper enode recorded: every host's health env names the keeper's node id" \
  test "$(grep -lx "KEEPER_NODE_ID=${KID}" "${TMP}"/render-kid/*/etc/sova/host.env | wc -l | tr -d ' ')" == 4
echo "enode://not-an-id@${KEEPER_IP}:30303" >"${TMP}/out/servers/sova-keeper-1.enode"
out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-kid-bad" 2>&1)"
check "a malformed keeper enode: KEEPER_NODE_ID rendered empty" grep -qx 'KEEPER_NODE_ID=' "${TMP}/render-kid-bad/sova-rpc-1/etc/sova/host.env"
rm -f "${TMP}/out/servers/sova-keeper-1.enode"
CFG="${TMP}/health-bad.env"
for v in 0 -5 5m ""; do
  sed "s/^KEEPER_ISOLATED_MIN=5\$/KEEPER_ISOLATED_MIN=${v}/" "${TMP}/config.env" >"${TMP}/health-bad.env"
  if [[ -z "${v}" ]]; then # empty: the default, 5
    out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-kim-empty" 2>&1)"
    check "KEEPER_ISOLATED_MIN empty: renders the default 5" grep -qx 'KEEPER_ISOLATED_MIN=5' "${TMP}/render-kim-empty/sova-keeper-1/etc/sova/host.env"
  else
    check "deploy.sh check refuses KEEPER_ISOLATED_MIN=${v}" has "KEEPER_ISOLATED_MIN must be a positive number of minutes" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
  fi
done
sed 's/^KEEPER_ISOLATED_MIN=5$/KEEPER_ISOLATED_MIN=8/' "${TMP}/config.env" >"${TMP}/health-kim.env"
CFG="${TMP}/health-kim.env"
out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-kim" 2>&1)"
check "KEEPER_ISOLATED_MIN=8: rendered into every host's health env" \
  test "$(grep -lx KEEPER_ISOLATED_MIN=8 "${TMP}"/render-kim/*/etc/sova/host.env | wc -l | tr -d ' ')" == 4
CFG="${TMP}/config.env"

# ---- memory: swap on every host, mem_low alert (2026-09-27 incident) ---------
RR="${TMP}/out/render"
for h in sova-seed-1 sova-rpc-1 sova-keeper-1 sova-faucet-1; do
  check "render: ${h} host.env (deploy.sh) has SWAP_GB=4" grep -qx 'SWAP_GB=4' "${RR}/${h}/host.env"
  check "render: ${h} health has MEM_ALERT_MB=300" grep -qx 'MEM_ALERT_MB=300' "${RR}/${h}/etc/sova/host.env"
  check "render: ${h} gets /etc/sysctl.d/60-sova-swap.conf with vm.swappiness=10" \
    grep -qx 'vm.swappiness=10' "${RR}/${h}/etc/sysctl.d/60-sova-swap.conf"
done
check "render: every host's setup plans a 4 GB /swapfile with its fstab line" \
  test "$(grep -cF "swap: /swapfile 4 GB if absent (fallocate, chmod 600, mkswap, swapon; fstab '/swapfile none swap sw 0 0'); vm.swappiness=10" <<<"${RENDER_OUT}")" == 4
check "setup-host.sh: the full setup runs setup_swap (before zebrad)" \
  bash -c "awk '/^setup_ufw\$/ { u = NR } /^setup_swap\$/ { s = NR } /^setup_zebrad\$/ { z = NR } END { exit !(u && s > u && z > s) }' '${KIT}/host/setup-host.sh'"
check "setup-host.sh: swapfile made with fallocate, chmod 600, mkswap, swapon" \
  bash -c "f='${KIT}/host/setup-host.sh'; grep -q 'fallocate -l \"\${SWAP_GB}G\"' \"\$f\" && grep -q 'chmod 0600 \"\${f}\"' \"\$f\" && grep -q 'mkswap \"\${f}\"' \"\$f\" && grep -q 'swapon \"\${f}\"' \"\$f\""
# shellcheck disable=SC2016 # the literal source line
check "setup-host.sh: /etc/fstab line added only if absent" \
  grep -qF 'if grep -qE "^[[:space:]]*${f}[[:space:]]" /etc/fstab; then' "${KIT}/host/setup-host.sh"
check "health.sh: mem_low alert on MemAvailable with the top 3 by RSS" \
  bash -c "grep -q 'alert mem_low' '${KIT}/host/health.sh' && grep -q 'ps -eo rss=,comm= --sort=-rss' '${KIT}/host/health.sh'"
sed 's/^SWAP_GB=4$/SWAP_GB=0/' "${TMP}/config.env" >"${TMP}/swap-off.env"
CFG="${TMP}/swap-off.env"
out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-swap-off" 2>&1)"
check "SWAP_GB=0: renders and plans no swapfile" \
  test "$(grep -cF 'swap: SWAP_GB=0, no swapfile managed; vm.swappiness=10' <<<"${out}")" == 4
check "SWAP_GB=0: rendered into host.env" grep -qx 'SWAP_GB=0' "${TMP}/render-swap-off/sova-rpc-1/host.env"
sed 's/^SWAP_GB=4$/SWAP_GB=100/' "${TMP}/config.env" >"${TMP}/swap-bad.env"
CFG="${TMP}/swap-bad.env"
check "deploy.sh check refuses SWAP_GB=100" has "SWAP_GB must be a whole number of GB from 0" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
sed 's/^MEM_ALERT_MB=300$/MEM_ALERT_MB=0/' "${TMP}/config.env" >"${TMP}/mem-bad.env"
CFG="${TMP}/mem-bad.env"
check "deploy.sh check refuses MEM_ALERT_MB=0" has "MEM_ALERT_MB must be a positive number" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
CFG="${TMP}/config.env"

# ---- network alerts from two hosts --------------------------------------------
na_hosts() { # render-dir -> the hosts rendered with HEALTH_NETWORK_ALERTS=1
  grep -lx HEALTH_NETWORK_ALERTS=1 "$1"/*/etc/sova/host.env | awk -F/ '{ print $(NF-3) }' | sort | tr '\n' ' '
}
check "render: network alerts from exactly sova-rpc-1 and sova-seed-1 (the default)" test "$(na_hosts "${RR}")" == "sova-rpc-1 sova-seed-1 "
for h in sova-keeper-1 sova-faucet-1; do
  check "render: ${h} only logs network findings (HEALTH_NETWORK_ALERTS=0)" grep -qx 'HEALTH_NETWORK_ALERTS=0' "${RR}/${h}/etc/sova/host.env"
done
# The live shape: a second seed. Still the first seed + rpc, not both seeds.
{ cat "${TMP}/config.env"; echo 'SERVERS+=("sova-seed-2:cx23:hel1:0:seed")'; } >"${TMP}/two-seeds.env"
CFG="${TMP}/two-seeds.env"
out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-two-seeds" 2>&1)"
check "two seeds: sova-seed-2 rendered" test -f "${TMP}/render-two-seeds/sova-seed-2/etc/sova/host.env"
check "two seeds: network alerts still from sova-rpc-1 and sova-seed-1 only" test "$(na_hosts "${TMP}/render-two-seeds")" == "sova-rpc-1 sova-seed-1 "
sed 's/^HEALTH_NETWORK_ALERT_HOSTS=.*/HEALTH_NETWORK_ALERT_HOSTS="sova-rpc-1,sova-keeper-1"/' "${TMP}/config.env" >"${TMP}/na.env"
CFG="${TMP}/na.env"
out="$(cd "${KIT}" && kit ./deploy.sh render --out "${TMP}/render-na" 2>&1)"
check "HEALTH_NETWORK_ALERT_HOSTS picks the hosts (rpc-1 + keeper-1)" test "$(na_hosts "${TMP}/render-na")" == "sova-keeper-1 sova-rpc-1 "
sed 's/^HEALTH_NETWORK_ALERT_HOSTS=.*/HEALTH_NETWORK_ALERT_HOSTS="sova-rpc-1,sova-faucet-1"/' "${TMP}/config.env" >"${TMP}/na.env"
check "refuses a network-alert host with no sova node (faucet)" has "sova-faucet-1 is a faucet host with no sova node" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
sed 's/^HEALTH_NETWORK_ALERT_HOSTS=.*/HEALTH_NETWORK_ALERT_HOSTS="sova-rpc-9"/' "${TMP}/config.env" >"${TMP}/na.env"
check "refuses a network-alert host not in SERVERS" has "names 'sova-rpc-9', which is not in SERVERS" "$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
sed 's/^HEALTH_NETWORK_ALERT_HOSTS=.*/HEALTH_NETWORK_ALERT_HOSTS="sova-rpc-1"/' "${TMP}/config.env" >"${TMP}/na.env"
out="$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
check "one network-alert host: an open item, not an error" \
  bash -c "grep -q 'only sova-rpc-1 sends network alerts' <<<\"\$1\" && grep -q '^==> config OK' <<<\"\$1\"" _ "${out}"
CFG="${TMP}/config.env"

# ---- bootnodes.sh: the genesis hash comes from the nodes, never a constant ----
# Stand in for what deploy.sh records from each node host's
# `sova genesis-hash` (KIT-OUT genesis_hash / genesis_sip7).
G1="0x$(printf 'ab%.0s' $(seq 1 32))"
G2="0x$(printf 'cd%.0s' $(seq 1 32))"
mkdir -p "${TMP}/out/servers"
echo "enode://$(printf '11%.0s' $(seq 1 64))@203.0.113.1:30303" >"${TMP}/out/servers/sova-seed-1.enode"
record_genesis() { # hash-for-keeper
  local h
  for h in sova-seed-1 sova-rpc-1 sova-keeper-1; do
    echo "${G1}" >"${TMP}/out/servers/${h}.genesis_hash"
    echo 1 >"${TMP}/out/servers/${h}.genesis_sip7"
  done
  echo "$1" >"${TMP}/out/servers/sova-keeper-1.genesis_hash"
}
record_genesis "${G1}"
out="$(cd "${KIT}" && kit ./bootnodes.sh 2>&1)"
check "bootnodes.sh: publishes the nodes' recorded genesis hash in seeds.json" \
  test "$(jq -r .genesis_hash "${TMP}/out/seeds.json" 2>/dev/null)" == "${G1}"
check "bootnodes.sh: seeds.json carries sip6 and sip7" \
  test "$(jq -c '[.sip6, .sip7]' "${TMP}/out/seeds.json" 2>/dev/null)" == '[true,true]'
check "bootnodes.sh: testnet.env sets SOVA_SIP7=1" grep -q '^export SOVA_SIP7=1$' "${TMP}/out/testnet.env"
check "bootnodes.sh: testnet.env names the genesis hash" grep -q "^# Genesis hash: ${G1}$" "${TMP}/out/testnet.env"
check "bootnodes.sh: testnet.env follows only by default" grep -q "^export SOVA_FOLLOW_ONLY=1$" "${TMP}/out/testnet.env"
check "bootnodes.sh: testnet.env sources cleanly" bash -c "set -u; HOME=/nonexistent; . \"${TMP}/out/testnet.env\" && [[ \$SOVA_DATADIR == /nonexistent/.sova-testnet/node && \$SOVA_CHAIN == sova-testnet ]]"
check "bootnodes.sh: no hard-coded genesis hash left in the kit" \
  lacks "0x8b04e8fc|GENESIS_HASH=\"0x" "$(cat "${KIT}"/*.sh "${KIT}"/host/*.sh)"
record_genesis "${G2}"
out="$(cd "${KIT}" && kit ./bootnodes.sh 2>&1)"
check "bootnodes.sh: refuses nodes that disagree on the genesis hash" has "genesis hash disagreement" "${out}"
record_genesis "${G1}"
echo 0 >"${TMP}/out/servers/sova-rpc-1.genesis_sip7"
out="$(cd "${KIT}" && kit ./bootnodes.sh 2>&1)"
check "bootnodes.sh: refuses a host deployed with another SOVA_SIP7" has "sova-rpc-1 was deployed with a different SOVA_SIP7" "${out}"
rm -f "${TMP}/out/servers/sova-rpc-1.genesis_hash"
out="$(cd "${KIT}" && kit ./bootnodes.sh 2>&1)"
check "bootnodes.sh: refuses to publish without every node's recorded hash" has "no genesis hash recorded for sova-rpc-1" "${out}"
rm -rf "${TMP}/out/servers" "${TMP}/out/seeds.json" "${TMP}/out/testnet.env" "${TMP}/out/bootnodes.txt"

# ---- launch.sh --dry-run: the keeper in every host stage --------------------
out="$(cd "${KIT}" && kit TELEGRAM_BOT_TOKEN=dry-run-dummy TELEGRAM_CHAT_ID=0 ./launch.sh --dry-run 2>&1)"
rc=$?
check "launch.sh --dry-run exits 0" test "${rc}" == 0
stage() { awk -v n="==== $1 " 'index($0, n) == 1 { on = 1; next } /^==== / { on = 0 } on' <<<"${out}"; }
s2="$(stage 2)"
check "stage 2 provision: keeper adopted as byo" grep -q "^+ byo sova-keeper-1 (keeper)" <<<"${s2}"
check "stage 2 provision: keeper bootstrapped as ubuntu@${KEEPER_IP}" \
  grep -q "byo-bootstrap.py -> ubuntu@${KEEPER_IP}" <<<"${s2}"
check "stage 2 provision: no Hetzner server or volume for the keeper" \
  lacks "^\+ hcloud .*sova-keeper-1" "${s2}"
check "stage 2 provision: seed still created on Hetzner" has "hcloud server create --name sova-seed-1 " "${s2}"
check "stage 2 provision: rpc still created on Hetzner" has "hcloud server create --name sova-rpc-1 " "${s2}"
s3="$(stage 3)"
check "stage 3 hosts: keeper pass 1 (--enode-only)" \
  grep -q "sova-admin@sova-keeper-1\[byo ${KEEPER_IP}\] sudo bash .*setup-host.sh .*--enode-only" <<<"${s3}"
check "stage 3 hosts: keeper pass 2 (full setup)" \
  grep -qE "sova-admin@sova-keeper-1\[byo ${KEEPER_IP}\] sudo bash .*setup-host.sh [^ ]+host.env $" <<<"${s3}"
s4="$(stage 4)"
check "stage 4 edge: keeper gets alerts" grep -q "sova-admin@sova-keeper-1" <<<"${s4}"
check "stage 4 edge: keeper gets no public DNS/tunnel" lacks "keeper" "$(grep -v "sova-admin@" <<<"${s4}")"
check "stage 5 sync: keeper's zebrad checked" grep -q "sova-keeper-1\[byo ${KEEPER_IP}\]: zebrad" <<<"$(stage 5)"
s6="$(stage 6)"
check "stage 6 chain: keeper re-deployed (sova-node starts)" \
  grep -q "sova-admin@sova-keeper-1\[byo ${KEEPER_IP}\] sudo bash" <<<"${s6}"
check "stage 6 chain: keeper's enode verified" grep -q "bootnodes --verify: sova-keeper-1" <<<"${s6}"
check "stage 8 smoke: keeper checked" grep -q "smoke: sova-keeper-1\[byo ${KEEPER_IP}\] (keeper)" <<<"$(stage 8)"

# ---- the default: the unmodified example, keeper on Hetzner -------------------
CFG="${KIT}/config.env.example"
out="$(cd "${KIT}" && kit ./launch.sh --dry-run 2>&1)"
check "example config: launch.sh --dry-run runs end to end" grep -q 'launch sequence complete' <<<"${out}"
s2="$(stage 2)"
check "example config: stage 2 creates the keeper on Hetzner (cx23, fsn1)" \
  has "hcloud server create --name sova-keeper-1 --type cx23 --image [^ ]+ --location fsn1 " "${s2}"
check "example config: stage 2 creates the keeper's 40 GB volume" \
  has "hcloud volume create --name sova-keeper-1-data --size 40 " "${s2}"
check "example config: stage 2 adopts no byo host" lacks "byo" "${s2}"

# ---- refusals -----------------------------------------------------------------
CFG="${TMP}/pending.env"
with_keeper "sova-keeper-1:byo:<keeper-ip>:40:keeper:ubuntu" >"${CFG}"
out="$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
check "pending byo address: an open item" \
  has "sova-keeper-1: address <keeper-ip> is pending" "${out}"
check "pending byo address: ... and not an error" has "^==> config OK" "${out}"
out="$(cd "${KIT}" && kit ./launch.sh --dry-run 2>&1)"
check "pending byo address: launch.sh --dry-run still runs end to end" grep -q 'launch sequence complete' <<<"${out}"
out="$(cd "${KIT}" && kit ./provision.sh up 2>&1)"
check "a real provision.sh up refuses the pending address (before any token or network use)" \
  grep -q "error: sova-keeper-1: address <keeper-ip> is still pending" <<<"${out}"

bad_entry() { # description entry
  with_keeper "$2" >"${TMP}/bad.env"
  CFG="${TMP}/bad.env"
  local o
  if o="$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"; then
    bad "rejects $1"
  else
    if has "error: config: sova-keeper-1: " "${o}"; then ok "rejects $1: ${o##*error: config: }"; else bad "rejects $1, but: ${o##*$'\n'}"; fi
  fi
}
bad_entry "a byo IPv6 literal" "sova-keeper-1:byo:2001:db8::1:40:keeper:ubuntu"
bad_entry "a 6-field Hetzner entry" "sova-keeper-1:cx23:fsn1:40:keeper:ubuntu"
bad_entry "a byo address with a bad character" "sova-keeper-1:byo:bad_host!:40:keeper"

# ---- the bootstrap reads the kit's cloud-init (needs PyYAML locally) ----------
if python3 -c 'import yaml' 2>/dev/null; then
  CFG="${TMP}/config.env"
  (cd "${KIT}" && kit ./provision.sh up --dry-run --my-ip >/dev/null 2>&1)
  out="$(python3 "${KIT}/host/byo-bootstrap.py" "${TMP}/out/cloud-init.yaml" --dry-run 2>&1)"
  check "byo-bootstrap.py --dry-run applies the rendered cloud-init.yaml" grep -q 'done: base applied' <<<"${out}"
else
  echo "skip  byo-bootstrap.py --dry-run (no PyYAML here; the real host has it)"
fi

echo "${PASS} passed, ${FAIL} failed"
[[ "${FAIL}" == 0 ]]
