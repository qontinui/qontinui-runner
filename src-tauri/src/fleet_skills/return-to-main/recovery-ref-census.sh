#!/usr/bin/env bash
# recovery-ref-census.sh — which checkout-restore snapshots on this device have
# no live retention gate?
#
# Plan: 2026-09-13-one-recovery-rule-for-both-checkout-restorers, Phase 3
# (reconciliation). Contract: knowledge-base/qontinui-specific/
# checkout-restore-contract.md, item 5 and "Reconciliation".
#
# Every refs/wip/return-to-main/* snapshot must carry exactly ONE retention gate
# (anchor claim_kind "recovery_ref", resource_key "<device>:<repo>:<wip_ref>").
# Registration can be missed: the sweep run by hand, a RestoreDefault whose
# server-side registration failed or was never sent, a snapshot written before
# the contract, a reap that landed on the wrong device, a gate whose reap
# refused. This script finds every such ref and prints the exact registration
# the caller (/return-to-main Step 4) makes. It READS coord and git only; it
# registers nothing, deletes nothing, and changes no checkout.
#
# A ref's anchor is LIVE when some gate on it is:
#   * `open` (muted or not — a muted gate is a deliberate hold), or
#   * cleared with a continuation that has not finished: not cancelled, not
#     expired, and no consumed outcome yet (dispatch pending or in flight).
# Anything else is terminal without a live reap — `work_abandoned`,
# `work_unreported`, `spawn_failed`, expired, cancelled, notify-only, a bare
# `spawned` with no work outcome, or `work_completed` while the ref still exists
# (a completed reap deletes the ref, so a surviving ref means it did not). Such
# a ref is NEEDS_GATE.
#
# USAGE
#   recovery-ref-census.sh [--root <workspace-root>] [--device <id>] [--only <repo>]...
#   --root     directory whose depth-1 children with a `.git` DIRECTORY are the
#              primaries (default: the current directory).
#   --device   the owning device (default $QONTINUI_MACHINE_ID, else
#              ~/.qontinui/machine.json `device_id`).
#   --only     restrict to these checkout names (repeatable).
# Output: one JSON object on stdout:
#   {"tool","schema","device","root","coord_url","credential","refs":[{"repo",
#    "checkout","wip_ref","sha","status":"GATED"|"NEEDS_GATE"|"UNKNOWN","reason",
#    "tenant_id","gates":[{"gate_id","verdict","consumed_outcome","live"}],
#    "registration":{…}|null}],"counts":{"refs","gated","needs_gate","unknown"},
#    "error"}
#   `registration` (NEEDS_GATE only) is the coord_register_gate argument object,
#   verbatim: claim_kind, resource_key, predicate, continuation,
#   clearance_audience, gate_class.
#   `tenant_id` is the tenant PROVEN to own the checkout's origin repo -- the
#   tenant the gates were looked up in, and so the tenant a NEEDS_GATE
#   registration must be made under (null when unproven, which is UNKNOWN).
# ENV  COORD_HTTP_URL (default https://coord.qontinui.io), RECOVERY_CENSUS_CURL
#      (curl override; the suite's stub), RECOVERY_CENSUS_NO_MINT=1,
#      RECOVERY_CENSUS_TIMEOUT (seconds per call, default 60). Credential: per
#      repo, for the tenant PROVEN to own the checkout's origin
#      (lib/coord-tenant-credential.sh): $COORD_DEVICE_JWT or
#      ~/.qontinui/coord-device-jwt only while its exp is >60 s away AND its
#      tenant_id claim is that tenant, else POST /agents/credential naming it.
#      Staged in a mode-600 header file, passed as `curl -H @file`, never on
#      argv.
#
# EXIT
#   0  every snapshot has a live gate (or there are none)
#   1  at least one snapshot NEEDS_GATE, and every ref was decided
#   3  UNKNOWN: coord could not be read for some ref, the tenant owning some
#      ref's repo could not be proven, or no device id
#   4  usage
# ---- END HELP

set -u

_rc_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -d "$_rc_dir/lib" ]; then LIB_DIR="$_rc_dir/lib"; else LIB_DIR="$_rc_dir/../../../scripts/lib"; fi
if [ -r "$LIB_DIR/git-scope.sh" ]; then . "$LIB_DIR/git-scope.sh"; fi
if declare -F git_scope_strip >/dev/null 2>&1; then
  git_scope_strip
elif [ -n "${GIT_DIR+s}${GIT_WORK_TREE+s}${GIT_COMMON_DIR+s}" ]; then
  echo "recovery-ref-census: FATAL - lib/git-scope.sh is not usable ($LIB_DIR) AND GIT_DIR/GIT_WORK_TREE/GIT_COMMON_DIR is set. Refusing." >&2
  exit 4
fi
if [ -r "$LIB_DIR/native-path.sh" ]; then . "$LIB_DIR/native-path.sh"; fi
declare -F native_path_w >/dev/null 2>&1 || native_path_w() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s\n' "$1"; fi
}
export MSYS_NO_PATHCONV=1

usage() { sed -n '2,/^# ---- END HELP/p' "$0" | sed '$d' | sed 's/^#\{0,1\} \{0,1\}//'; }
usage_err() { echo "recovery-ref-census: $1 (see --help)" >&2; exit 4; }

ROOT="$PWD"; DEVICE=""; ONLY=()
while [ $# -gt 0 ]; do
  case "$1" in
    --root|--device|--only)
      [ $# -ge 2 ] && [ -n "$2" ] || usage_err "$1 needs a value"
      case "$1" in --root) ROOT="$2" ;; --device) DEVICE="$2" ;; *) ONLY+=("$2") ;; esac
      shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage_err "unknown argument $1" ;;
  esac
done
[ -d "$ROOT" ] || usage_err "no such directory: $ROOT"
ROOT="$(cd "$ROOT" && pwd)"

CURL="${RECOVERY_CENSUS_CURL:-curl}"
COORD_URL="${COORD_HTTP_URL:-https://coord.qontinui.io}"; COORD_URL="${COORD_URL%/}"
NET_TIMEOUT="${RECOVERY_CENSUS_TIMEOUT:-60}"
case "$NET_TIMEOUT" in ''|*[!0-9]*) NET_TIMEOUT=60 ;; esac

TMP="$(mktemp -d)" || { echo "recovery-ref-census: cannot mktemp -d" >&2; exit 3; }
trap 'rm -rf "$TMP"' EXIT

json_escape() {
  local s="$1"
  s="${s//\\/\\\\}"; s="${s//\"/\\\"}"; s="${s//$'\n'/\\n}"; s="${s//$'\r'/\\r}"; s="${s//$'\t'/\\t}"
  printf '%s' "$s" | tr -d '\000-\010\013\014\016-\037'
}
js()  { printf '"%s"' "$(json_escape "$1")"; }
jsn() { if [ -n "${1:-}" ]; then js "$1"; else printf 'null'; fi; }

home="${HOME:-${USERPROFILE:-}}"
if [ -z "$DEVICE" ]; then
  DEVICE="${QONTINUI_MACHINE_ID:-}"
  [ -z "$DEVICE" ] && [ -n "$home" ] && [ -r "$home/.qontinui/machine.json" ] && \
    DEVICE="$(grep -o '"\(device_id\|machine_id\)"[[:space:]]*:[[:space:]]*"[^"]*"' "$home/.qontinui/machine.json" | head -1 | sed 's/.*:[[:space:]]*"//; s/"$//')"
fi
if [ -z "$DEVICE" ]; then
  printf '{"tool":"recovery-ref-census.sh","schema":1,"device":null,"root":%s,"refs":[],"counts":{"refs":0,"gated":0,"needs_gate":0,"unknown":0},"error":"no device id: --device, QONTINUI_MACHINE_ID and ~/.qontinui/machine.json all missed"}\n' "$(js "$ROOT")"
  exit 3
fi

# ---------------------------------------------------------------------------
# The credential: per REPO, for the tenant PROVEN to own it
# (lib/coord-tenant-credential.sh; plan
# 2026-09-23-ccfg-scripts-mint-device-credentials-with-no-tenant, Phase 2).
# `/coord/agent-gates` is tenant-scoped, and a tenant-less mint on a device
# bound to several tenants lands on the legacy pointer -- measured 2026-09-23:
# 3 gates under that token, 500 under the tenant that owns qontinui/*. A
# retention gate looked for under the wrong tenant is invisible, so the ref
# would read NEEDS_GATE and be registered a second time. A ref whose owning
# tenant cannot be PROVEN (no GitHub origin, every bound tenant 404s, the door
# or the mint fails) is UNKNOWN -- never NEEDS_GATE, never GATED.
np() { native_path_w "$1"; }
CTC_OK=0
if [ -r "$LIB_DIR/coord-tenant-credential.sh" ]; then
  # shellcheck source=../../../scripts/lib/coord-tenant-credential.sh
  . "$LIB_DIR/coord-tenant-credential.sh" && declare -F ctc_owner_tenant >/dev/null 2>&1 && CTC_OK=1
fi
export CTC_TMP="$TMP" CTC_CURL="$CURL" CTC_COORD_URL="$COORD_URL" CTC_TIMEOUT="$NET_TIMEOUT" CTC_DEVICE="$DEVICE"
[ "${RECOVERY_CENSUS_NO_MINT:-0}" = 1 ] && export CTC_NO_MINT=1
declare -A OWNER_STATE=() OWNER_TENANT=() OWNER_NOTE=()
# owner_of <checkout> -> sets O_SLUG O_STATE O_TENANT O_NOTE (cached per slug)
owner_of() {
  O_SLUG="$(ctc_origin_slug "$1")"; O_TENANT=""
  if [ -z "$O_SLUG" ]; then
    O_STATE=unknown; O_NOTE="its origin is not a github.com owner/name, so no tenant can be proven to own it"; return 0
  fi
  if [ -z "${OWNER_STATE[$O_SLUG]+s}" ]; then
    ctc_owner_tenant "$O_SLUG"
    OWNER_STATE[$O_SLUG]="$CTC_STATE"; OWNER_TENANT[$O_SLUG]="$CTC_TENANT"; OWNER_NOTE[$O_SLUG]="$CTC_NOTE"
  fi
  O_STATE="${OWNER_STATE[$O_SLUG]}"; O_TENANT="${OWNER_TENANT[$O_SLUG]}"; O_NOTE="${OWNER_NOTE[$O_SLUG]}"
}

PY=""
for c in python3 python; do
  command -v "$c" >/dev/null 2>&1 && "$c" -c 'import sys; sys.exit(0 if sys.version_info[0] == 3 else 1)' >/dev/null 2>&1 && { PY="$c"; break; }
done
# The tenant library runs the same interpreter this script proved runs.
[ -n "$PY" ] && export CTC_PY="$PY"

# ---------------------------------------------------------------------------
# The refs.
REFS=()   # "repo<US>checkout<US>ref<US>sha"
for gitdir in "$ROOT"/*/.git; do
  [ -d "$gitdir" ] || continue
  co="$(dirname "$gitdir")"; repo="$(basename "$co")"
  if [ "${#ONLY[@]}" -gt 0 ]; then
    _hit=0; for o in "${ONLY[@]}"; do [ "$o" = "$repo" ] && _hit=1; done
    [ "$_hit" = 1 ] || continue
  fi
  while IFS=' ' read -r sha ref; do
    [ -n "$ref" ] && REFS+=("$repo"$'\x1f'"$co"$'\x1f'"$ref"$'\x1f'"$sha")
  done < <(git --no-optional-locks -C "$(np "$co")" for-each-ref --format='%(objectname) %(refname)' refs/wip/return-to-main 2>/dev/null | tr -d '\r')
done

CRED="not_needed"
[ "${#REFS[@]}" -gt 0 ] && CRED="owner_tenant_per_repo"

registration_json() { # <repo> <wip_ref>
  printf '{"claim_kind":"recovery_ref","resource_key":%s,"predicate":{"kind":"time_elapsed","duration_secs":1209600},"continuation":{"action":"run_skill","skill":"return-to-main","args":["--reap",%s,%s,"--device",%s],"target_device_id":%s},"clearance_audience":"agent","gate_class":"routine-review"}' \
    "$(js "$DEVICE:$1:$2")" "$(js "$1")" "$(js "$2")" "$(js "$DEVICE")" "$(js "$DEVICE")"
}

OUT_REFS=""; N_GATED=0; N_NEED=0; N_UNK=0; ERR=""
[ -n "$PY" ] || ERR="no Python 3 interpreter to parse coord's gate listing"
[ -n "$ERR" ] || [ -r "$LIB_DIR/envelope.py" ] || ERR="lib/envelope.py is not readable in $LIB_DIR"
[ -n "$ERR" ] || [ "$CTC_OK" = 1 ] || ERR="lib/coord-tenant-credential.sh is not usable in $LIB_DIR, so no tenant can be proven to own any repo"

cat >"$TMP/classify.py" <<'PYEOF'
import json, sys
key = sys.argv[1]
sys.path.insert(0, sys.argv[2])
from envelope import EnvelopeUnknown, require_collection, require_key  # typed envelope reads (check #48)
door = "GET /coord/agent-gates"
try:
    d = json.load(sys.stdin)
    rows, _count, _prov = require_collection(d, "gates", door=door)
    total, _ = require_key(d, "total", door)
    truncated, _ = require_key(d, "truncated", door)
except (EnvelopeUnknown, ValueError):
    sys.exit(3)
if not isinstance(total, int) or truncated is not False:
    sys.exit(3)
out, live_any = [], False
for r in rows:
    if r.get("claim_kind") != "recovery_ref" or r.get("resource_key") != key:
        sys.exit(3)  # the door did not honour the filter: nothing it says is about this ref
    verdict = r.get("verdict")
    outcome = r.get("continuation_consumed_outcome")
    spawn = r.get("continuation_spawn")
    finished = bool(r.get("continuation_cancelled_at") or r.get("continuation_expired_at") or outcome)
    live = verdict == "open" or (verdict == "cleared" and spawn is not None and not finished
                                 and (r.get("continuation_action") or (spawn if isinstance(spawn, dict) else {}).get("action")) != "notify_only")
    live_any = live_any or live
    out.append({"gate_id": r.get("gate_id"), "verdict": verdict, "consumed_outcome": outcome, "live": live})
print(("LIVE" if live_any else "NONE") + "\t" + json.dumps(out, separators=(",", ":")))
PYEOF

i=0
for row in ${REFS[@]+"${REFS[@]}"}; do
  IFS=$'\x1f' read -r repo co ref sha <<<"$row"
  i=$((i + 1))
  status=""; reason=""; gates="[]"; reg="null"; tenant=""; rcred=""
  [ -n "$ERR" ] || owner_of "$co"
  if [ -n "$ERR" ]; then
    status=UNKNOWN; reason="$ERR"
  elif [ "$O_STATE" != proven ]; then
    status=UNKNOWN; reason="the tenant owning ${O_SLUG:-the origin repo of this checkout} is not proven, so its retention gates cannot be looked up: $O_NOTE"
  elif rcred="$(ctc_stage_for_tenant "$O_TENANT")"; tenant="$O_TENANT"; [ "${rcred#rejected}" != "$rcred" ]; then
    status=UNKNOWN; reason="no credential for $O_SLUG's tenant $O_TENANT: ${rcred#rejected}"
  else
    : > "$TMP/g.$i.json"
    code="$("$CURL" -sS -G -o "$(np "$TMP/g.$i.json")" -w '%{http_code}' --connect-timeout 10 -m "$NET_TIMEOUT" \
              -H "@$(np "$TMP/bearer.hdr")" \
              --data-urlencode "claim_kind=recovery_ref" \
              --data-urlencode "resource_key=$DEVICE:$repo:$ref" \
              --data-urlencode "limit=100" \
              "$COORD_URL/coord/agent-gates" 2>"$TMP/g.$i.err")"
    if [ "$code" != 200 ]; then
      status=UNKNOWN; reason="GET /coord/agent-gates answered HTTP ${code:-000} (tenant $tenant, credential: $rcred) $(head -1 "$TMP/g.$i.err" | cut -c1-120)"
    elif ! "$PY" "$(np "$TMP/classify.py")" "$DEVICE:$repo:$ref" "$(np "$LIB_DIR")" <"$TMP/g.$i.json" >"$TMP/c.$i.out" 2>/dev/null
    then
      status=UNKNOWN; reason="coord's gate listing for this anchor did not parse, was truncated, or carried a row outside the requested anchor"
    else
      _c="$(tr -d '\r' <"$TMP/c.$i.out")"
      gates="${_c#*$'\t'}"
      if [ "${_c%%$'\t'*}" = LIVE ]; then
        status=GATED; reason="a live retention gate holds this snapshot"
      else
        status=NEEDS_GATE; reg="$(registration_json "$repo" "$ref")"
        if [ "$gates" = "[]" ]; then reason="no gate on this anchor"
        else reason="every gate on this anchor is terminal without a completed reap, and the ref still exists"; fi
      fi
    fi
  fi
  case "$status" in GATED) N_GATED=$((N_GATED + 1)) ;; NEEDS_GATE) N_NEED=$((N_NEED + 1)) ;; *) N_UNK=$((N_UNK + 1)) ;; esac
  OUT_REFS="${OUT_REFS:+$OUT_REFS,}{\"repo\":$(js "$repo"),\"checkout\":$(js "$co"),\"wip_ref\":$(js "$ref"),\"sha\":$(js "$sha"),\"status\":$(js "$status"),\"reason\":$(js "$reason"),\"tenant_id\":$(jsn "$tenant"),\"gates\":$gates,\"registration\":$reg}"
done

printf '{"tool":"recovery-ref-census.sh","schema":1,"device":%s,"root":%s,"coord_url":%s,"credential":%s,"refs":[%s],"counts":{"refs":%d,"gated":%d,"needs_gate":%d,"unknown":%d},"error":%s}\n' \
  "$(js "$DEVICE")" "$(js "$ROOT")" "$(js "$COORD_URL")" "$(js "$CRED")" "$OUT_REFS" "${#REFS[@]}" "$N_GATED" "$N_NEED" "$N_UNK" "$(jsn "$ERR")"
if [ "$N_UNK" -gt 0 ]; then exit 3; fi
if [ "$N_NEED" -gt 0 ]; then exit 1; fi
exit 0
