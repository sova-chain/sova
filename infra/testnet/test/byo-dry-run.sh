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
HZ_KEEPER="sova-keeper-1:cx33:fsn1:40:keeper"
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
check "render: no cloudflared on the keeper (not public)" test ! -e "${R}/etc/systemd/system/cloudflared.service"
if [[ ${#VERIFY[@]} -gt 0 ]]; then
  check "systemd-analyze verify: keeper ok" grep -q 'ok   sova-keeper-1: systemd-analyze verify' <<<"${out}"
fi

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
check "example config: stage 2 creates the keeper on Hetzner (cx33, fsn1)" \
  has "hcloud server create --name sova-keeper-1 --type cx33 --image [^ ]+ --location fsn1 " "${s2}"
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
bad_entry "a 6-field Hetzner entry" "sova-keeper-1:cx33:fsn1:40:keeper:ubuntu"
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
