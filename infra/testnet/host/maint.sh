#!/usr/bin/env bash
# infra/testnet/host/maint.sh -- planned maintenance on one host. Run as
# root: by deploy.sh over SSH (this file on stdin, `sudo bash -s -- <cmd>`),
# by setup-host.sh (the mute around a zebrad restart), or by hand
# (/usr/local/lib/sova-infra/maint.sh).
#
#   maint.sh mute <minutes> [reason]
#       Planned-maintenance mute (K12): health.sh keeps logging every
#       finding but sends no Telegram until then. 1..240 minutes; extends a
#       running mute, never shortens it. The marker, /etc/sova/.mute-until
#       (epoch seconds, then the reason), is removed by health.sh once it
#       has expired.
#   maint.sh unmute
#   maint.sh keeper-pause [reason]
#       (the keeper host) The keeper pause (K7, docs/ops/nu7-upgrade.md
#       3.3): writes /etc/sova/.keeper-paused, then stops sova-keeper (no
#       more burns) and the keeper's mine-mode sova-node (no more blocks on
#       whatever its zebrad follows). While the marker exists, systemd
#       won't start either (ConditionPathExists in both units), setup-host.sh
#       doesn't start them, and health.sh reports "keeper paused (planned)"
#       instead of keeper_down / sova_down.
#   maint.sh keeper-resume
#       Removes the marker, starts sova-node, waits for its RPC plus
#       RESUME_SETTLE_SECS (60), then starts sova-keeper if it was running
#       when paused (a burner that was stopped stays stopped: starting it is
#       a decision, docs/ops/keeper-miner.md).
#   maint.sh status
set -euo pipefail

ETC="${SOVA_ETC:-/etc/sova}"
MUTE_FILE="${ETC}/.mute-until"
PAUSED_FILE="${ETC}/.keeper-paused"
MUTE_MAX_MIN=240
SOVA_BIN_LINK="${SOVA_BIN_LINK:-/usr/local/bin/sova}"
# shellcheck source=/dev/null
[[ -f "${ETC}/host.env" ]] && source "${ETC}/host.env"
RESUME_SETTLE_SECS="${RESUME_SETTLE_SECS:-60}"
RESUME_RPC_WAIT_SECS="${RESUME_RPC_WAIT_SECS:-180}"
HOST="$(hostname)"

say() { printf '[maint %s] %s\n' "${HOST}" "$*"; }
die() {
  printf '[maint %s] error: %s\n' "${HOST}" "$*" >&2
  exit 1
}
fmt_time() { date -u -d "@$1" '+%Y-%m-%d %H:%M UTC' 2>/dev/null || date -u -r "$1" '+%Y-%m-%d %H:%M UTC' 2>/dev/null || printf '@%s' "$1"; }
# One line, no control characters, at most 200 characters.
clean() { printf '%s' "$*" | tr -d '\000-\037' | cut -c1-200; }
state() { systemctl is-active "$1" 2>/dev/null || true; }

cmd_mute() {
  local min="${1:-}" reason until now old=""
  shift || true
  reason="$(clean "$*")"
  if ! [[ "${min}" =~ ^[0-9]+$ ]] || ((min < 1 || min > MUTE_MAX_MIN)); then
    die "mute needs 1..${MUTE_MAX_MIN} minutes, got '${min}'"
  fi
  now="$(date +%s)"
  until=$((now + min * 60))
  [[ -f "${MUTE_FILE}" ]] && old="$(sed -n 1p "${MUTE_FILE}")"
  if [[ "${old}" =~ ^[0-9]+$ ]] && ((old > until)); then
    say "already muted until $(fmt_time "${old}"), later than ${min} min from now: left as is"
    return 0
  fi
  install -d -m 0755 "${ETC}"
  printf '%s\n%s\n' "${until}" "${reason:-planned maintenance}" >"${MUTE_FILE}.new"
  mv -f "${MUTE_FILE}.new" "${MUTE_FILE}"
  say "Telegram alerts muted until $(fmt_time "${until}") (${min} min): ${reason:-planned maintenance}; health.sh still logs every finding"
}

cmd_unmute() {
  if [[ -f "${MUTE_FILE}" ]]; then
    rm -f "${MUTE_FILE}"
    say "mute removed: alerts go to Telegram again"
  else
    say "not muted"
  fi
}

is_keeper_host() { systemctl cat sova-keeper >/dev/null 2>&1; }

cmd_keeper_pause() {
  local reason burner
  reason="$(clean "$*")"
  is_keeper_host || die "no sova-keeper unit here: keeper-pause is for the keeper host"
  if [[ -f "${PAUSED_FILE}" ]]; then
    say "already paused since $(fmt_time "$(sed -n 's/^since=//p' "${PAUSED_FILE}")"); making sure both are stopped"
  else
    burner="$(state sova-keeper)"
    install -d -m 0755 "${ETC}"
    # The marker first: from here on nothing (systemd, setup-host.sh) starts them.
    printf 'since=%s\nburner=%s\nreason=%s\n' "$(date +%s)" "${burner:-unknown}" "${reason:-planned}" >"${PAUSED_FILE}.new"
    mv -f "${PAUSED_FILE}.new" "${PAUSED_FILE}"
  fi
  # The burner first: no burn goes out for blocks the node won't build.
  systemctl stop sova-keeper
  systemctl stop sova-node
  say "keeper PAUSED (${reason:-planned}): sova-keeper $(state sova-keeper), sova-node $(state sova-node). No burns, no new Sova blocks from this keeper. Resume: ./deploy.sh keeper-resume"
}

cmd_keeper_resume() {
  local burner port t
  is_keeper_host || die "no sova-keeper unit here: keeper-resume is for the keeper host"
  if [[ ! -f "${PAUSED_FILE}" ]]; then
    say "not paused (no ${PAUSED_FILE}): sova-keeper $(state sova-keeper), sova-node $(state sova-node); nothing started"
    return 0
  fi
  burner="$(sed -n 's/^burner=//p' "${PAUSED_FILE}")"
  rm -f "${PAUSED_FILE}"
  systemctl start sova-node
  port="${SOVA_HTTP_PORT:-8545}"
  for ((t = 0; ; t += 5)); do
    if curl -fsS --max-time 5 -H 'Content-Type: application/json' \
      --data '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' "http://127.0.0.1:${port}" >/dev/null 2>&1; then
      break
    fi
    ((t < RESUME_RPC_WAIT_SECS)) ||
      die "sova-node RPC not answering ${RESUME_RPC_WAIT_SECS} s after the start: sova-keeper NOT started (the pause marker is gone; journalctl -u sova-node)"
    sleep 5
  done
  say "sova-node up (RPC answers on 127.0.0.1:${port}); letting it settle ${RESUME_SETTLE_SECS} s"
  sleep "${RESUME_SETTLE_SECS}"
  # setup-host.sh restarts sova-node when this differs: record what runs now.
  [[ -L "${SOVA_BIN_LINK}" ]] && readlink "${SOVA_BIN_LINK}" >"${ETC}/.sova-node.bin"
  if [[ "${burner}" == active ]]; then
    systemctl start sova-keeper
    say "keeper RESUMED: sova-node $(state sova-node), sova-keeper $(state sova-keeper)"
  else
    say "keeper RESUMED: sova-node $(state sova-node); sova-keeper was ${burner:-unknown} when paused, so it stays stopped (start it: sudo systemctl start sova-keeper)"
  fi
}

cmd_status() {
  local until=""
  [[ -f "${MUTE_FILE}" ]] && until="$(sed -n 1p "${MUTE_FILE}")"
  if [[ "${until}" =~ ^[0-9]+$ ]] && ((until > $(date +%s))); then
    say "muted until $(fmt_time "${until}"): $(sed -n 2p "${MUTE_FILE}")"
  else
    say "not muted"
  fi
  if [[ -f "${PAUSED_FILE}" ]]; then
    say "keeper paused since $(fmt_time "$(sed -n 's/^since=//p' "${PAUSED_FILE}")") ($(sed -n 's/^reason=//p' "${PAUSED_FILE}")): sova-keeper $(state sova-keeper), sova-node $(state sova-node)"
  elif is_keeper_host; then
    say "keeper not paused: sova-keeper $(state sova-keeper), sova-node $(state sova-node)"
  fi
}

cmd="${1:-}"
shift || true
case "${cmd}" in
  mute) cmd_mute "$@" ;;
  unmute) cmd_unmute ;;
  keeper-pause) cmd_keeper_pause "$@" ;;
  keeper-resume) cmd_keeper_resume ;;
  status) cmd_status ;;
  *) die "usage: maint.sh mute <minutes> [reason] | unmute | keeper-pause [reason] | keeper-resume | status" ;;
esac
