#!/usr/bin/env bash
# infra/testnet/test/byo-dry-run.sh -- offline proof that a bring-your-own
# host (the AWS keeper) is a first-class kit host: its config validates,
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
# with the keeper's placeholder replaced by a documentation address
# (198.51.100.20, RFC 5737), in a temp dir; never reads secrets.env.
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

sed "s/<keeper-elastic-ip>/${KEEPER_IP}/" "${KIT}/config.env.example" >"${TMP}/config.env"
CFG="${TMP}/config.env"
check "config: the example's keeper is 'sova-keeper-1:byo:${KEEPER_IP}:40:keeper:ubuntu'" \
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
check "render: keeper budgets rendered" grep -q '^KEEPER_LIFETIME_BUDGET_ZAT=' "${R}/etc/sova/keeper.env"
check "render: no cloudflared on the keeper (not public)" test ! -e "${R}/etc/systemd/system/cloudflared.service"
if [[ ${#VERIFY[@]} -gt 0 ]]; then
  check "systemd-analyze verify: keeper ok" grep -q 'ok   sova-keeper-1: systemd-analyze verify' <<<"${out}"
fi

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

# ---- refusals -----------------------------------------------------------------
CFG="${TMP}/pending.env"
cp "${KIT}/config.env.example" "${CFG}"
out="$(cd "${KIT}" && kit ./deploy.sh check 2>&1)"
check "example config: the pending keeper address is an open item" \
  has "sova-keeper-1: address <keeper-elastic-ip> is pending" "${out}"
check "example config: ... and not an error" has "^==> config OK" "${out}"
out="$(cd "${KIT}" && kit ./launch.sh --dry-run 2>&1)"
check "example config: launch.sh --dry-run still runs end to end" grep -q 'launch sequence complete' <<<"${out}"
out="$(cd "${KIT}" && kit ./provision.sh up 2>&1)"
check "a real provision.sh up refuses the pending address (before any token or network use)" \
  grep -q "error: sova-keeper-1: address <keeper-elastic-ip> is still pending" <<<"${out}"

bad_entry() { # description entry
  sed "s|\"sova-keeper-1:byo:<keeper-elastic-ip>:40:keeper:ubuntu\"|\"$2\"|" "${KIT}/config.env.example" >"${TMP}/bad.env"
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
