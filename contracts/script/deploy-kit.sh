#!/usr/bin/env bash
# contracts/script/deploy-kit.sh -- the ONE deploy path for the day-one kit:
# WSOVA, the Uniswap-v2 fork (factory + router), Multicall3, Ashwings, and
# AshwingsMarket when its source exists. Used by box/deploy-dapps.sh (the
# box) and infra/testnet/deploy-contracts.sh (the public testnet); keep all
# deploy logic here so the two can never drift.
#
# Usage:
#   deploy-kit.sh <plan|deploy|verify> --rpc URL --out FILE <key> [options]
#     plan     what is recorded, what would be deployed, estimated gas and
#              the deployer's balance. Sends nothing.
#     deploy   deploy whatever is not recorded yet, verify everything,
#              record each address as soon as its deploy is mined.
#              Re-running is a no-op once everything is recorded.
#     verify   read-only: every recorded contract has this checkout's
#              runtime code and answers its getters. Needs no key.
#   key (plan and deploy):
#     --keystore FILE --password-file FILE   foundry keystore (testnet);
#                                            nothing secret on any argv
#     --private-key-env NAME                 raw key in env var NAME. For
#                                            public dev keys only (box,
#                                            anvil): forge gets it on argv.
#                                            Refused on chain 8233/82330.
#   options:
#     --expect-chain-id N   refuse any other chain
#     --reset-stale         a recorded address with NO code is treated as
#                           stale and redeployed (the box: every `up` is a
#                           fresh chain). Without it that is an error.
#     --redeploy KEY        deploy KEY again even though it is recorded
#                           (the old entry moves to .superseded)
#
# Constructor arguments are matched by parameter NAME (case, underscores
# ignored), so a contract can change its constructor without a change
# here, as long as each parameter has a mapping:
#   feeToSetter          0x0, always: nobody can ever turn the fee on
#   factory              the recorded factory
#   WETH, wsova          the recorded WSOVA
#   ashwings             the recorded Ashwings
#   treasury             $ASHWINGS_TREASURY
#   zecPayee             $ASHWINGS_ZEC_PAYEE
#   priceWei             $ASHWINGS_PRICE_WEI
#   priceZat             $ASHWINGS_PRICE_ZAT
#   feeBps               $MARKET_FEE_BPS (default 100)
#   anything else        $KIT_ARG_<KEY>_<NAME> (upper case, no
#                        underscores), e.g. KIT_ARG_MARKET_MAXLISTINGS
# A parameter with no value is an error; nothing is ever defaulted to the
# deployer's address.
#
# Verification: the runtime code at each address must equal this
# checkout's build (immutable slots masked), and every constructor
# argument that has a same-named zero-argument getter (router.factory(),
# ashwings.treasury(), ...) must read back as the recorded value.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACTS="$(cd "${HERE}/.." && pwd)"

# key|artifact (file:contract under out/)|required
KIT=(
  "wsova|WSOVA.sol:WSOVA|1"
  "factory|UniswapV2Factory.sol:UniswapV2Factory|1"
  "router|UniswapV2Router02.sol:UniswapV2Router02|1"
  "multicall3|Multicall3.sol:Multicall3|1"
  "ashwings|Ashwings.sol:Ashwings|1"
  "market|AshwingsMarket.sol:AshwingsMarket|0"
)
# Extra read-back checks: key|signature|type|expected
PROBES=(
  "wsova|symbol()|string|WSOVA"
  "factory|feeTo()|address|0x0000000000000000000000000000000000000000"
  "ashwings|name()|string|Ashwings"
)
ZERO_ADDR=0x0000000000000000000000000000000000000000
PUBLIC_CHAIN_IDS=" 8233 82330 "

log() { printf '==> %s\n' "$*" >&2; }
die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}
need_cmd() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required (${2:-})"; }

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; }

CMD="${1:-}"
[[ -n "${CMD}" ]] && shift
case "${CMD}" in plan | deploy | verify) ;; -h | --help | "") usage; exit 0 ;; *) die "unknown command '${CMD}'" ;; esac

RPC=""
OUT=""
KEYSTORE=""
PASSWORD_FILE=""
PK_ENV=""
EXPECT_CHAIN_ID=""
RESET_STALE=0
REDEPLOY=" "
while [[ $# -gt 0 ]]; do
  case "$1" in
    --rpc) RPC="${2:?--rpc needs a URL}"; shift ;;
    --out) OUT="${2:?--out needs a file}"; shift ;;
    --keystore) KEYSTORE="${2:?}"; shift ;;
    --password-file) PASSWORD_FILE="${2:?}"; shift ;;
    --private-key-env) PK_ENV="${2:?}"; shift ;;
    --expect-chain-id) EXPECT_CHAIN_ID="${2:?}"; shift ;;
    --reset-stale) RESET_STALE=1 ;;
    --redeploy) REDEPLOY+="${2:?} "; shift ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown argument '$1'" ;;
  esac
  shift
done
[[ -n "${RPC}" ]] || die "--rpc is required"
[[ -n "${OUT}" ]] || die "--out is required"
need_cmd forge "Foundry: https://getfoundry.sh"
need_cmd cast "Foundry"
need_cmd jq

MARKET_FEE_BPS="${MARKET_FEE_BPS:-100}"

# ---- key --------------------------------------------------------------------------
FORGE_KEY_ARGS=()
DEPLOYER=""
setup_key() {
  if [[ -n "${KEYSTORE}" ]]; then
    [[ -z "${PK_ENV}" ]] || die "use --keystore or --private-key-env, not both"
    [[ -f "${KEYSTORE}" ]] || die "no keystore at ${KEYSTORE}"
    [[ -f "${PASSWORD_FILE}" ]] || die "--keystore needs --password-file (a file, never the password itself)"
    local f mode
    for f in "${KEYSTORE}" "${PASSWORD_FILE}"; do
      mode="$(stat -f %Lp "${f}" 2>/dev/null || stat -c %a "${f}")"
      [[ "${mode}" == 600 || "${mode}" == 400 ]] || die "${f} has mode ${mode}; chmod 600 it"
    done
    # forge/cast read these from the environment: no secret on any argv.
    export ETH_KEYSTORE="${KEYSTORE}" ETH_PASSWORD="${PASSWORD_FILE}"
    DEPLOYER="$(cast wallet address)" || die "could not open ${KEYSTORE} with ${PASSWORD_FILE}"
  elif [[ -n "${PK_ENV}" ]]; then
    [[ "${PUBLIC_CHAIN_IDS}" != *" ${CHAIN_ID} "* ]] ||
      die "chain ${CHAIN_ID} is a public Sova network: use --keystore, not a raw key on argv"
    local pk="${!PK_ENV:-}"
    [[ -n "${pk}" ]] || die "env var ${PK_ENV} is empty"
    FORGE_KEY_ARGS=(--private-key "${pk}")
    DEPLOYER="$(cast wallet address --private-key "${pk}")"
  else
    die "${CMD} needs a key: --keystore FILE --password-file FILE, or --private-key-env NAME"
  fi
}

# ---- chain --------------------------------------------------------------------------
CHAIN_ID=""
GENESIS=""
chain_info() {
  CHAIN_ID="$(cast chain-id --rpc-url "${RPC}")" || die "no answer from ${RPC}"
  GENESIS="$(cast block 0 --field hash --rpc-url "${RPC}")" || die "cannot read block 0 from ${RPC}"
  if [[ -n "${EXPECT_CHAIN_ID}" && "${CHAIN_ID}" != "${EXPECT_CHAIN_ID}" ]]; then
    die "${RPC} is chain ${CHAIN_ID}, expected ${EXPECT_CHAIN_ID}"
  fi
  log "chain ${CHAIN_ID}, genesis ${GENESIS:0:18}..., head $(cast block-number --rpc-url "${RPC}")"
}

# ---- record (the deployments JSON) ---------------------------------------------------
# Shape: flat "<key>": "<address>" at the top (what Demo.s.sol and the site
# read), plus chainId, genesisHash, deployer, and deployments.<key> with
# the full record.
rec_get() { [[ -f "${OUT}" ]] && jq -r "$1 // empty" "${OUT}" || true; }

check_record_chain() {
  [[ -f "${OUT}" ]] || return 0
  jq -e . "${OUT}" >/dev/null 2>&1 || die "${OUT} is not valid JSON"
  local c g
  c="$(rec_get .chainId)"
  g="$(rec_get .genesisHash)"
  if [[ -n "${c}" && "${c}" != "${CHAIN_ID}" ]]; then
    die "${OUT} records chain ${c}, but ${RPC} is chain ${CHAIN_ID}"
  fi
  if [[ -n "${g}" && "${g}" != "${GENESIS}" ]]; then
    die "${OUT} records genesis ${g}, but ${RPC} has ${GENESIS}: a different chain (a reset?). Move the file aside to start a new record."
  fi
}

rec_write() { # jq filter, args...
  local filter="$1" tmp
  shift
  tmp="$(mktemp "${OUT}.XXXXXX")"
  if [[ -f "${OUT}" ]]; then
    jq "$@" "${filter}" "${OUT}" >"${tmp}"
  else
    jq -n "$@" "${filter}" >"${tmp}"
  fi
  mv "${tmp}" "${OUT}"
}

# ---- artifacts -------------------------------------------------------------------------
art_file() { # file:contract -> out/<file>/<contract>.json
  printf '%s/out/%s/%s.json' "${CONTRACTS}" "${1%%:*}" "${1##*:}"
}
# The source that produced the artifact still exists (forge can leave a
# stale artifact behind after a source is deleted or renamed).
art_present() {
  local f src
  f="$(art_file "$1")"
  [[ -f "${f}" ]] || return 1
  src="$(jq -r '.metadata.settings.compilationTarget | keys[0] // empty' "${f}")"
  [[ -n "${src}" && -f "${CONTRACTS}/${src}" ]]
}
art_target() { # file:contract -> src/path.sol:Contract (for forge create)
  local f
  f="$(art_file "$1")"
  jq -r '.metadata.settings.compilationTarget | to_entries[0] | "\(.key):\(.value)"' "${f}"
}
ctor_params() { # file:contract -> "name type" lines
  jq -r '.abi[] | select(.type == "constructor") | .inputs[] | "\(.name) \(.type)"' "$(art_file "$1")"
}
norm() { tr -d '_' <<<"$1" | tr '[:upper:]' '[:lower:]'; }

# Value for constructor parameter $3 (type $4) of kit entry $1, into RA_V;
# RA_SRC says where it came from. Returns 1 (RA_ERR set) if there is none.
# In a plan, a dependency that is not deployed yet stands in as 0x0.
# RA_DEP names the kit entry the value comes from, if any.
RA_V=""
RA_SRC=""
RA_ERR=""
RA_DEP=""
resolve_arg() { # key artifact name type
  local key="$1" name="$3" type="$4" n dep=0
  RA_V=""
  RA_ERR=""
  RA_DEP=""
  n="$(norm "${name}")"
  case "${n}" in
    feetosetter) RA_V="${ZERO_ADDR}"; RA_SRC="fixed: ownerless" ;;
    factory) RA_V="$(rec_get .deployments.factory.address)"; RA_SRC="recorded factory"; dep=1; RA_DEP=factory ;;
    weth | wsova) RA_V="$(rec_get .deployments.wsova.address)"; RA_SRC="recorded wsova"; dep=1; RA_DEP=wsova ;;
    ashwings) RA_V="$(rec_get .deployments.ashwings.address)"; RA_SRC="recorded ashwings"; dep=1; RA_DEP=ashwings ;;
    treasury) RA_V="${ASHWINGS_TREASURY:-}"; RA_SRC="ASHWINGS_TREASURY" ;;
    zecpayee) RA_V="${ASHWINGS_ZEC_PAYEE:-}"; RA_SRC="ASHWINGS_ZEC_PAYEE" ;;
    pricewei) RA_V="${ASHWINGS_PRICE_WEI:-}"; RA_SRC="ASHWINGS_PRICE_WEI" ;;
    pricezat) RA_V="${ASHWINGS_PRICE_ZAT:-}"; RA_SRC="ASHWINGS_PRICE_ZAT" ;;
    feebps) RA_V="${MARKET_FEE_BPS:-}"; RA_SRC="MARKET_FEE_BPS" ;;
    *)
      RA_SRC="KIT_ARG_$(tr '[:lower:]' '[:upper:]' <<<"${key}")_$(tr '[:lower:]' '[:upper:]' <<<"${n}")"
      RA_V="${!RA_SRC:-}"
      ;;
  esac
  if [[ -z "${RA_V}" && ${dep} == 1 && "${CMD}" == plan ]]; then
    RA_V="${ZERO_ADDR}"
    RA_SRC="${RA_SRC} (not deployed yet)"
  fi
  if [[ -z "${RA_V}" ]]; then
    RA_ERR="${key} ($2): constructor parameter '${name}' (${type}) has no value; set ${RA_SRC}"
    return 1
  fi
  case "${type}" in
    address) [[ "${RA_V}" =~ ^0x[0-9a-fA-F]{40}$ ]] || RA_ERR="${key}.${name}: '${RA_V}' is not an address (${RA_SRC})" ;;
    uint* | int*) [[ "${RA_V}" =~ ^[0-9]+$ ]] || RA_ERR="${key}.${name}: '${RA_V}' is not a number (${RA_SRC})" ;;
  esac
  [[ -z "${RA_ERR}" ]]
}

# ARGS (values, in order) and ARGS_JSON ([{name,type,value}]) for one
# entry. ARGS_MISSING lists every problem (a deploy dies on the first);
# ARGS_DEPS the kit entries it takes an address from.
ARGS_JSON="[]"
ARGS=()
ARGS_MISSING=()
ARGS_DEPS=" "
resolve_args() { # key artifact
  local name type
  ARGS=()
  ARGS_MISSING=()
  ARGS_DEPS=" "
  ARGS_JSON="[]"
  while read -r name type; do
    [[ -n "${name}${type}" ]] || continue
    resolve_arg "$1" "$2" "${name}" "${type}" && true
    [[ -z "${RA_DEP}" ]] || ARGS_DEPS+="${RA_DEP} "
    if [[ -n "${RA_ERR}" ]]; then
      [[ "${CMD}" == plan ]] || die "${RA_ERR}"
      ARGS_MISSING+=("${RA_ERR}")
      continue
    fi
    ARGS+=("${RA_V}")
    ARGS_JSON="$(jq -c --arg n "${name}" --arg t "${type}" --arg v "${RA_V}" '. + [{name:$n,type:$t,value:$v}]' <<<"${ARGS_JSON}")"
  done < <(ctor_params "$2")
}

ctor_sig() { # artifact -> "constructor(type,...)"
  printf 'constructor(%s)' "$(jq -r '[.abi[] | select(.type == "constructor") | .inputs[].type] | join(",")' "$(art_file "$1")")"
}

# ---- verification -------------------------------------------------------------------------
# Runtime code at $2 equals the artifact's deployedBytecode, with every
# immutable slot masked on both sides (the router stores factory/WETH).
code_matches() { # artifact address
  local code
  code="$(cast code "$2" --rpc-url "${RPC}")"
  [[ "${code}" != 0x ]] || return 2
  jq -e --arg code "${code}" '
    def mask($s): reduce (.deployedBytecode.immutableReferences // {} | [.[][]] | .[]) as $r
      ($s; .[0:2 + 2 * $r.start] + ("0" * (2 * $r.length)) + .[2 + 2 * ($r.start + $r.length):]);
    (.deployedBytecode.object | ascii_downcase) as $want
    | ($code | ascii_downcase) as $have
    | ($want | length) == ($have | length) and mask($want) == mask($have)
  ' "$(art_file "$1")" >/dev/null
}

readback() { # address signature type expected -> 0 if equal
  local got want
  got="$(cast call "$1" "$2" --rpc-url "${RPC}" 2>/dev/null)" || return 1
  want="$(cast abi-encode "f($3)" "$4")"
  [[ "$(tr '[:upper:]' '[:lower:]' <<<"${got}")" == "$(tr '[:upper:]' '[:lower:]' <<<"${want}")" ]]
}

# Checks one recorded contract; prints one line, returns 1 on any failure.
verify_one() { # key artifact address
  local key="$1" art="$2" addr="$3" notes=() rc=0 name type value getter p k sig ptype expected
  code_matches "${art}" "${addr}" || rc=$?
  case "${rc}" in
    0) notes+=("code = build") ;;
    2) printf '  FAIL %-10s %s  no code at this address\n' "${key}" "${addr}"; return 1 ;;
    *) printf '  FAIL %-10s %s  runtime code differs from this checkout'"'"'s build of %s\n' "${key}" "${addr}" "${art}"; return 1 ;;
  esac
  # Constructor args with a same-named zero-argument getter.
  while read -r name type value; do
    [[ -n "${name}" ]] || continue
    getter="$(jq -r --arg n "$(norm "${name}")" --arg t "${type}" '
      [.abi[] | select(.type == "function" and (.inputs | length) == 0 and (.outputs | length) == 1
        and .outputs[0].type == $t and (.stateMutability == "view" or .stateMutability == "pure")
        and ((.name | gsub("_"; "") | ascii_downcase) == $n)) | .name][0] // empty' "$(art_file "${art}")")"
    [[ -n "${getter}" ]] || continue
    if readback "${addr}" "${getter}()" "${type}" "${value}"; then
      notes+=("${getter}()=${value}")
    else
      printf '  FAIL %-10s %s  %s() does not return the recorded %s\n' "${key}" "${addr}" "${getter}" "${value}"
      return 1
    fi
  done < <(jq -r --arg k "${key}" '.deployments[$k].constructorArgs[]? | "\(.name) \(.type) \(.value)"' "${OUT}")
  for p in "${PROBES[@]}"; do
    IFS='|' read -r k sig ptype expected <<<"${p}"
    [[ "${k}" == "${key}" ]] || continue
    if readback "${addr}" "${sig}" "${ptype}" "${expected}"; then
      notes+=("${sig}=${expected}")
    else
      printf '  FAIL %-10s %s  %s is not %s\n' "${key}" "${addr}" "${sig}" "${expected}"
      return 1
    fi
  done
  if [[ "${key}" == multicall3 ]]; then
    if readback "${addr}" "getChainId()" uint256 "${CHAIN_ID}"; then notes+=("getChainId()=${CHAIN_ID}"); else
      printf '  FAIL %-10s %s  getChainId() is not %s\n' "${key}" "${addr}" "${CHAIN_ID}"
      return 1
    fi
  fi
  printf '  ok   %-10s %s  %s\n' "${key}" "${addr}" "$(printf '%s, ' "${notes[@]}" | sed 's/, $//')"
}

# ---- per-entry state ------------------------------------------------------------------------
# Prints: recorded | stale | missing | skip (optional contract, no source)
entry_state() { # key artifact required
  local addr
  if ! art_present "$2"; then
    [[ "$3" == 1 ]] && die "$1: no build artifact for $2 (did forge build fail?)"
    echo skip
    return 0
  fi
  [[ "${REDEPLOY}" == *" $1 "* ]] && { echo missing; return 0; }
  addr="$(rec_get ".deployments.\"$1\".address")"
  [[ -n "${addr}" ]] || { echo missing; return 0; }
  if [[ "$(cast code "${addr}" --rpc-url "${RPC}")" == 0x ]]; then
    echo stale
  else
    echo recorded
  fi
}

# The recorded args must be what the environment asks for now, or a
# re-run would silently keep a contract the operator meant to change.
# Uses ARGS_JSON from resolve_args; prints the problem, if any.
args_changed() { # key
  local want have
  want="$(jq -c '[.[] | {name, value: (.value | ascii_downcase)}]' <<<"${ARGS_JSON}")"
  have="$(jq -c --arg k "$1" '[.deployments[$k].constructorArgs[]? | {name, value: (.value | ascii_downcase)}]' "${OUT}")"
  [[ "${want}" != "${have}" ]] || return 0
  printf '%s is recorded with constructor args %s, but the environment now resolves %s. Contracts are ownerless and immutable: to change it, re-run with --redeploy %s' "$1" "${have}" "${want}" "$1"
}

build() {
  log "forge build (${CONTRACTS})"
  (cd "${CONTRACTS}" && forge build --quiet) || die "forge build failed"
}

# a >= b for non-negative decimal strings of any length (wei balances
# overflow bash arithmetic).
dec_ge() {
  local a="${1#"${1%%[!0]*}"}" b="${2#"${2%%[!0]*}"}"
  [[ ${#a} -ne ${#b} ]] && { [[ ${#a} -gt ${#b} ]]; return; }
  [[ "${a}" == "${b}" || "${a}" > "${b}" ]]
}

# ---- commands ----------------------------------------------------------------------------------
cmd_verify() {
  chain_info
  [[ -f "${OUT}" ]] || die "no record at ${OUT}"
  check_record_chain
  local e key art req addr fails=0
  log "verifying ${OUT} against ${RPC}"
  for e in "${KIT[@]}"; do
    IFS='|' read -r key art req <<<"${e}"
    addr="$(rec_get ".deployments.\"${key}\".address")"
    if [[ -z "${addr}" ]]; then
      if [[ "${req}" == 1 ]] || art_present "${art}"; then
        printf '  FAIL %-10s not recorded\n' "${key}"
        fails=$((fails + 1))
      fi
      continue
    fi
    verify_one "${key}" "${art}" "${addr}" || fails=$((fails + 1))
  done
  [[ ${fails} == 0 ]] || die "${fails} contract(s) failed verification"
  log "all recorded contracts verified"
}

cmd_plan() {
  chain_info
  setup_key
  check_record_chain
  build
  local e key art req st gas total=0 unknown=0 blocked=0 price bal
  price="$(cast gas-price --rpc-url "${RPC}")"
  log "deployer ${DEPLOYER}, nonce $(cast nonce "${DEPLOYER}" --rpc-url "${RPC}")"
  for e in "${KIT[@]}"; do
    IFS='|' read -r key art req <<<"${e}"
    st="$(entry_state "${key}" "${art}" "${req}")"
    case "${st}" in
      skip) printf '  -    %-10s %s: no source in this checkout, not part of this kit\n' "${key}" "${art}" ;;
      recorded) printf '  keep %-10s %s (recorded)\n' "${key}" "$(rec_get ".deployments.\"${key}\".address")" ;;
      missing | stale)
        resolve_args "${key}" "${art}"
        if [[ ${#ARGS_MISSING[@]} -gt 0 ]]; then
          printf '  NEW  %-10s %s  CANNOT DEPLOY YET:\n' "${key}" "${art}"
          printf '         %s\n' "${ARGS_MISSING[@]}"
          blocked=$((blocked + 1))
          continue
        fi
        gas=""
        # Estimates need the args encoded; dependencies that are not
        # deployed yet resolve to nothing, so those estimates are skipped.
        if [[ ${#ARGS[@]} -eq 0 ]]; then
          gas="$(cast estimate --from "${DEPLOYER}" --rpc-url "${RPC}" --create "$(jq -r .bytecode.object "$(art_file "${art}")")" 2>/dev/null || true)"
        else
          gas="$(cast estimate --from "${DEPLOYER}" --rpc-url "${RPC}" --create "$(jq -r .bytecode.object "$(art_file "${art}")")" "$(ctor_sig "${art}")" "${ARGS[@]}" 2>/dev/null || true)"
        fi
        if [[ "${gas}" =~ ^[0-9]+$ ]]; then total=$((total + gas)); else gas="?"; unknown=$((unknown + 1)); fi
        printf '  NEW  %-10s %s  gas %s  args %s\n' "${key}" "${art}" "${gas}" "$(jq -c '[.[] | "\(.name)=\(.value)"]' <<<"${ARGS_JSON}")"
        ;;
    esac
  done
  bal="$(cast balance "${DEPLOYER}" --rpc-url "${RPC}")"
  log "estimated gas ${total} (+${unknown} not estimable) at $(cast from-wei "${price}" gwei) gwei = $(cast from-wei "$((total * price))") SOVA; deployer holds $(cast from-wei "${bal}") SOVA"
  # Ask for 2x the estimate: fees move, and anything left over is harmless.
  if [[ ${total} -gt 0 ]] && ! dec_ge "${bal}" "$((total * price * 2))"; then
    log "FUND: send at least $(cast from-wei "$((total * price * 2))") SOVA to ${DEPLOYER} before deploy"
  fi
  [[ ${blocked} == 0 ]] || die "${blocked} contract(s) cannot be deployed until the values above are set"
}

# Every value every pending deploy needs is known before the first
# transaction, so a run never stops half way for a missing setting.
# Also: a kept contract must not take an address from one this run
# (re)deploys, and its recorded args must still be what is asked for.
preflight_args() {
  local e key art req st problems=() pending=" " d msg saved="${CMD}"
  CMD=plan # dependencies deployed later in this run stand in as 0x0
  for e in "${KIT[@]}"; do
    IFS='|' read -r key art req <<<"${e}"
    st="$(entry_state "${key}" "${art}" "${req}")"
    [[ "${st}" == skip ]] && continue
    resolve_args "${key}" "${art}"
    problems+=("${ARGS_MISSING[@]+"${ARGS_MISSING[@]}"}")
    if [[ "${st}" == missing || "${st}" == stale ]]; then
      pending+="${key} "
      continue
    fi
    for d in ${ARGS_DEPS}; do
      [[ "${pending}" != *" ${d} "* ]] || problems+=("${key} takes ${d}'s address and ${d} is deployed in this run: add --redeploy ${key}")
    done
    [[ ${#ARGS_MISSING[@]} -gt 0 ]] || { msg="$(args_changed "${key}")"; [[ -z "${msg}" ]] || problems+=("${msg}"); }
  done
  CMD="${saved}"
  if [[ ${#problems[@]} -gt 0 ]]; then
    printf '  %s\n' "${problems[@]}" >&2
    die "nothing deployed: ${#problems[@]} problem(s) above"
  fi
}

cmd_deploy() {
  chain_info
  setup_key
  mkdir -p "$(dirname "${OUT}")"
  check_record_chain
  build
  if [[ ! -f "${OUT}" ]] || [[ -z "$(rec_get .genesisHash)" ]]; then
    # shellcheck disable=SC2016 # $c/$g are jq variables
    rec_write '.chainId = $c | .genesisHash = $g' --argjson c "${CHAIN_ID}" --arg g "${GENESIS}"
  fi
  preflight_args
  [[ "$(cast balance "${DEPLOYER}" --rpc-url "${RPC}")" != 0 ]] || die "deployer ${DEPLOYER} holds no SOVA; fund it first (./deploy-kit.sh plan shows how much)"
  local e key art req st addr old deployed=0 kept=0 out tx block commit line
  commit="$(git -C "${CONTRACTS}" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)"
  [[ -z "$(git -C "${CONTRACTS}" status --porcelain -- . 2>/dev/null)" ]] || commit="${commit}-dirty"
  for e in "${KIT[@]}"; do
    IFS='|' read -r key art req <<<"${e}"
    st="$(entry_state "${key}" "${art}" "${req}")"
    case "${st}" in
      skip) continue ;;
      recorded)
        addr="$(rec_get ".deployments.\"${key}\".address")"
        line="$(verify_one "${key}" "${art}" "${addr}")" || { printf '%s\n' "${line}"; die "${key}: recorded contract failed verification"; }
        log "${key}: already deployed at ${addr}, verified, skipped"
        kept=$((kept + 1))
        continue
        ;;
      stale)
        [[ "${RESET_STALE}" == 1 ]] || die "${key} is recorded at $(rec_get ".deployments.\"${key}\".address") but there is no code there. Wrong RPC, or the chain was reset: move ${OUT} aside (or use --reset-stale on a throwaway chain)."
        log "${key}: recorded address has no code (fresh chain), redeploying"
        ;;
    esac
    resolve_args "${key}" "${art}"
    log "${key}: deploying $(art_target "${art}") $(jq -c '[.[] | "\(.name)=\(.value)"]' <<<"${ARGS_JSON}")"
    local cargs=()
    [[ ${#ARGS[@]} -eq 0 ]] || cargs=(--constructor-args "${ARGS[@]}")
    out="$(cd "${CONTRACTS}" && forge create "$(art_target "${art}")" --rpc-url "${RPC}" "${FORGE_KEY_ARGS[@]+"${FORGE_KEY_ARGS[@]}"}" --broadcast --json "${cargs[@]+"${cargs[@]}"}")" ||
      die "${key}: forge create failed"
    addr="$(jq -r '.deployedTo // empty' <<<"${out}")"
    tx="$(jq -r '.transactionHash // empty' <<<"${out}")"
    [[ "${addr}" =~ ^0x[0-9a-fA-F]{40}$ ]] || die "${key}: forge create printed no address: ${out}"
    block="$(cast receipt "${tx}" blockNumber --rpc-url "${RPC}")"
    old="$(rec_get ".deployments.\"${key}\"")"
    # Record first, then verify: a mined contract is never forgotten.
    # shellcheck disable=SC2016 # $-names are jq variables
    rec_write '
      (if $old != "" and ($old | fromjson | .address) != null then .superseded = ((.superseded // []) + [($old | fromjson) + {key: $k}]) else . end)
      | .chainId = $c | .genesisHash = $g | .deployer = $d
      | .[$k] = $a
      | .deployments[$k] = {address: $a, artifact: $art, txHash: $tx, block: ($b | tonumber),
          constructorArgs: $args, sourceCommit: $commit, deployedAt: (now | todate)}' \
      --arg k "${key}" --arg a "${addr}" --arg art "${art}" --arg tx "${tx}" --arg b "${block}" \
      --argjson args "${ARGS_JSON}" --arg d "${DEPLOYER}" --argjson c "${CHAIN_ID}" --arg g "${GENESIS}" \
      --arg commit "${commit}" --arg old "${old}"
    verify_one "${key}" "${art}" "${addr}" || die "${key}: deployed at ${addr} but failed verification"
    deployed=$((deployed + 1))
  done
  log "deployed ${deployed}, already there ${kept}; record: ${OUT}"
  cmd_verify
}

case "${CMD}" in
  plan) cmd_plan ;;
  deploy) cmd_deploy ;;
  verify) cmd_verify ;;
esac
