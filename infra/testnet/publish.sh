#!/usr/bin/env bash
# infra/testnet/publish.sh -- put the join files and zebrad snapshots on R2
# (https://$DL_HOST), from the first seed host, with rclone.
#
#   ./publish.sh [--dry-run] join
#       Upload out/seeds.json, out/testnet.env, out/bootnodes.txt (and
#       out/epoch-base.json if recorded) to the bucket root.
#   ./publish.sh [--dry-run] snapshot
#       On the first seed: capture the tip over RPC, stop sova-node and
#       zebrad cleanly, archive with box/testnet/snapshot.sh, restart both
#       (downtime = archive time), upload to zebrad-testnet/<height>/ and
#       update zebrad-testnet/latest.json. docs/ops/snapshots.md is the
#       policy: ALSO post height/hash/SHA-256 somewhere other than the
#       bucket (repo docs / release notes).
#
# Env (from Rob's R2 API token, "Object Read & Write" on R2_BUCKET only;
# never stored): R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, CLOUDFLARE_ACCOUNT_ID.
# They travel to the host on SSH stdin inside the upload script and exist
# only in that process's environment.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CMD=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    join | snapshot) CMD="$1" ;;
    *) die "unknown argument '$1'" ;;
  esac
  shift
done
[[ -n "${CMD}" ]] || { sed -n '2,19p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 1; }
load_config
validate_servers
need_env R2_ACCESS_KEY_ID
need_env R2_SECRET_ACCESS_KEY
need_env CLOUDFLARE_ACCOUNT_ID
SEED="$(servers_with_role seed | head -1)"
STAGE=/var/lib/sova/publish

# Runs a bash script (stdin of this function) as root on the seed, with the
# R2 credentials prepended as exports. Nothing is written to the host's
# disk except the files being published.
remote_with_r2() {
  local script
  script="$(cat)"
  if [[ "${DRY_RUN}" == 1 ]]; then
    echo "+ ssh sova-admin@${SEED} sudo bash -s <<'SCRIPT'  (R2 credentials exported from your env)"
    printf '%s\n' "${script}" | sed 's/^/    /'
    echo "  SCRIPT"
    return 0
  fi
  {
    printf 'export RCLONE_CONFIG_R2_TYPE=s3 RCLONE_CONFIG_R2_PROVIDER=Cloudflare\n'
    printf 'export RCLONE_CONFIG_R2_ACCESS_KEY_ID=%q RCLONE_CONFIG_R2_SECRET_ACCESS_KEY=%q\n' \
      "${R2_ACCESS_KEY_ID}" "${R2_SECRET_ACCESS_KEY}"
    printf 'export RCLONE_CONFIG_R2_ENDPOINT=%q\n' "https://${CLOUDFLARE_ACCOUNT_ID}.r2.cloudflarestorage.com"
    printf 'set -euo pipefail\ncommand -v rclone >/dev/null || { apt-get update -qq && apt-get install -y -qq rclone >/dev/null; }\n'
    printf '%s\n' "${script}"
  } | kit_ssh "${SEED}" 'sudo bash -s'
}

cmd_join() {
  local f files=()
  for f in seeds.json testnet.env bootnodes.txt epoch-base.json; do
    [[ -s "${OUT_DIR}/${f}" ]] && files+=("${OUT_DIR}/${f}")
  done
  [[ ${#files[@]} -ge 3 ]] || die "run ./bootnodes.sh first"
  jq -e '.epoch_base != null' "${OUT_DIR}/seeds.json" >/dev/null || die "seeds.json has no epoch base: pin B first"
  if [[ "${DRY_RUN}" == 1 ]]; then
    echo "+ scp ${files[*]##*/} -> ${SEED}:/tmp/sova-publish/"
  else
    kit_ssh "${SEED}" 'mkdir -p /tmp/sova-publish'
    kit_scp "${SEED}" /tmp/sova-publish "${files[@]}"
  fi
  remote_with_r2 <<EOF
rclone copy --s3-no-check-bucket /tmp/sova-publish/ "r2:${R2_BUCKET}/"
rm -rf /tmp/sova-publish
echo "published: https://${DL_HOST}/seeds.json, /testnet.env, /bootnodes.txt"
EOF
}

cmd_snapshot() {
  if [[ "${DRY_RUN}" != 1 ]]; then
    kit_ssh "${SEED}" 'mkdir -p /tmp/sova-publish'
    kit_scp "${SEED}" /tmp/sova-publish "${REPO_ROOT}/box/testnet/snapshot.sh"
  else
    echo "+ scp box/testnet/snapshot.sh -> ${SEED}:/tmp/sova-publish/"
  fi
  remote_with_r2 <<EOF
SNAP=/tmp/sova-publish/snapshot.sh
RPC=http://127.0.0.1:${ZEBRA_RPC_PORT}
rm -rf ${STAGE}/next && mkdir -p ${STAGE}
bash \$SNAP capture --rpc \$RPC --out ${STAGE}/capture.json
sleep 10
# Stop cleanly (RocksDB flush), archive the stopped state, restart.
systemctl stop sova-node || true
systemctl stop zebrad
trap 'systemctl start zebrad; systemctl start sova-node || true' EXIT
bash \$SNAP create /var/lib/sova/zebrad ${STAGE}/next --capture ${STAGE}/capture.json --rpc \$RPC
systemctl start zebrad
systemctl start sova-node || true
trap - EXIT
h=\$(jq -r .height ${STAGE}/next/snapshot.json)
rclone copy --s3-no-check-bucket ${STAGE}/next/ "r2:${R2_BUCKET}/zebrad-testnet/\$h/"
rclone copyto --s3-no-check-bucket ${STAGE}/next/snapshot.json "r2:${R2_BUCKET}/zebrad-testnet/latest.json"
cat ${STAGE}/next/snapshot.json
echo "published https://${DL_HOST}/zebrad-testnet/\$h/ -- now post height/hash/sha256 OUTSIDE the bucket (docs/ops/snapshots.md)"
rm -rf ${STAGE}/next /tmp/sova-publish
EOF
}

"cmd_${CMD}"
