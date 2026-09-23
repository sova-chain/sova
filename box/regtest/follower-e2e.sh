#!/usr/bin/env bash
# C2a acceptance runner: fresh regtest stack, then the follower's live
# reorg integration test (cargo test -p consensus --test follower_regtest).
set -euo pipefail

cd "$(dirname "$0")"
RPC_URL="${SOVA_REGTEST_RPC:-http://127.0.0.1:18232}"

cleanup() {
  echo "--- tearing down stack ---"
  docker compose down -v >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "--- starting fresh regtest stack ---"
docker compose down -v >/dev/null 2>&1 || true
docker compose up -d

echo "--- waiting for zebrad RPC ---"
for _ in $(seq 1 60); do
  if curl -s -X POST -H 'Content-Type: application/json' \
    --data '{"jsonrpc":"2.0","id":"w","method":"getblockcount","params":[]}' \
    "$RPC_URL" | grep -q result; then
    break
  fi
  sleep 2
done

echo "--- running follower integration test ---"
cd ../..
SOVA_REGTEST_RPC="$RPC_URL" cargo test -p consensus --test follower_regtest -- --ignored --nocapture

echo "FOLLOWER E2E PASSED"
