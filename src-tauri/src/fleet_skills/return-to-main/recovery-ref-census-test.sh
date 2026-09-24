#!/usr/bin/env bash
# recovery-ref-census-test.sh — suite for recovery-ref-census.sh
# (plan 2026-09-13-one-recovery-rule-for-both-checkout-restorers, Phase 3).
#
# Hermetic: a temp workspace of local git repos, and coord replaced by a curl
# stub (RECOVERY_CENSUS_CURL) that answers from per-anchor fixture files and
# logs its argv. No network.
#   A  the Phase 3 gate: one GATED ref and one ungated ref -> exactly ONE
#      registration, in the contract shape, exit 1
#   B  liveness per gate row: open (muted too) and a cleared continuation still
#      in flight are LIVE; work_abandoned, a bare spawned, notify_only, expired,
#      cancelled, and work_completed-with-the-ref-still-there are NOT
#   C  coord answers 500 -> UNKNOWN, exit 3, no registration
#   D  a listing row outside the requested anchor (the filter was ignored) ->
#      UNKNOWN; a truncated page -> UNKNOWN
#   E  no refs at all -> exit 0 and coord is never asked
#   F  a linked worktree (`.git` FILE) is not a primary; --only narrows the scan
#   G  no device id -> exit 3; the bearer never reaches curl's argv
#   H  the gates are read under the tenant PROVEN to own the ref's origin repo;
#      a mint that ignores tenant_id (+ a 200-empty page), a repo no bound
#      tenant owns, and a non-GitHub origin are all UNKNOWN, never NEEDS_GATE
# Exit 0 all passed, 1 otherwise.

set -uo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SELF="$HERE/$(basename "${BASH_SOURCE[0]}")"
SUITE_REL=".claude/skills/return-to-main/recovery-ref-census-test.sh"
SUBJECT_REAL="$HERE/recovery-ref-census.sh"
SUBJECT="${MC_SUBJECT:-$SUBJECT_REAL}"
LIBSRC="$(cd "$HERE/../../../scripts/lib" && pwd)"

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad() { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$1"; [ $# -gt 1 ] && printf '       %s\n' "${2:0:600}"; }
eq()  { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "expected [$2] got [$3]"; fi; }
has() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "[$2] not in [${3:0:600}]" ;; esac; }
lacks(){ case "$3" in *"$2"*) bad "$1" "[$2] present in [${3:0:600}]" ;; *) ok "$1" ;; esac; }

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

# shellcheck source=../../../scripts/lib/mutation-control.sh
. "$LIBSRC/mutation-control.sh"
mc_init "$SUITE_REL" "$SANDBOX"

# A mutant copy runs from a temp dir with no lib/ beside it.
[ -d "$(dirname "$SUBJECT")/lib" ] || [ "$SUBJECT" = "$SUBJECT_REAL" ] || ln -s "$LIBSRC" "$(dirname "$SUBJECT")/lib" 2>/dev/null || cp -R "$LIBSRC" "$(dirname "$SUBJECT")/lib"

export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_COUNT=2
export GIT_CONFIG_KEY_0=user.name GIT_CONFIG_VALUE_0=census-test
export GIT_CONFIG_KEY_1=user.email GIT_CONFIG_VALUE_1=census-test@example.invalid
export RECOVERY_CENSUS_NO_MINT=0 HOME="$SANDBOX/home" USERPROFILE="$SANDBOX/home"
unset COORD_DEVICE_JWT QONTINUI_MACHINE_ID COORD_HTTP_URL QONTINUI_TENANT_ID
mkdir -p "$HOME"
# Resolved by RUNNING it: on Windows `python3` is often the Store alias,
# present on PATH and unable to run.
PY=""
for _c in python3 python; do
  "$_c" -c 'import sys; sys.exit(0 if sys.version_info[0] == 3 else 1)' >/dev/null 2>&1 && { PY="$(command -v "$_c")"; break; }
done
[ -n "$PY" ] || { echo "FATAL: no runnable Python 3 (python3 or python)"; exit 1; }
DEV="11111111-2222-4333-8444-555555555555"

# --- the curl stub -----------------------------------------------------------
# Fixtures: $STUB_DIR/<sanitised resource_key>.json (the agent-gates body) and
# optional .code (HTTP status, default 200). /agents/credential mints a fake JWT
# whose tenant_id claim is the tenant asked for (STUB_MINT=ignore: always the
# pointer tenant $TP, the multi-tenant-device defect); POST /mcp answers the
# device's bindings; the author-session door answers 200 only when the bearer's
# claim is the owner in $STUB_OWNERS. Every request's bearer claim is logged.
STUB="$SANDBOX/curl-stub.sh"
# The stub is BASH, deliberately: the census suite runs on the windows-latest
# roster, and a Python stub invoked as curl answered nothing there (every mint
# read HTTP 000) while this shape has run there since the suite landed.
cat >"$STUB" <<'STUBEOF'
#!/usr/bin/env bash
out=""; rk=""; url=""; body=""; hdr=""
printf '%s\n' "$*" >>"$STUB_DIR/argv.log"
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -d) body="$2"; shift 2 ;;
    -H) case "$2" in @*) hdr="${2#@}" ;; esac; shift 2 ;;
    -w|-m|--connect-timeout|-X) shift 2 ;;
    --data-urlencode) case "$2" in resource_key=*) rk="${2#resource_key=}" ;; esac; shift 2 ;;
    http*) url="$1"; shift ;;
    *) shift ;;
  esac
done
b64url() { base64 | tr -d '\n=' | tr '/+' '_-'; }
jwt() { # <tenant>
  printf '%s.%s.c2ln' "$(printf '{"alg":"none"}' | b64url)" \
    "$(printf '{"tenant_id":"%s","exp":%s}' "$1" "$(( $(date +%s) + 3600 ))" | b64url)"
}
claim() { # the bearer's tenant_id claim, read from the -H @file
  local t seg
  [ -n "$hdr" ] && [ -r "$hdr" ] || return 0
  t="$(sed -n 's/^Authorization: Bearer //p' "$hdr" | tr -d '\r\n')"
  seg="$(printf '%s' "$t" | cut -d. -f2 | tr '_-' '/+')"
  case $(( ${#seg} % 4 )) in 2) seg="$seg==" ;; 3) seg="$seg=" ;; esac
  printf '%s' "$seg" | base64 -d 2>/dev/null | sed -n 's/.*"tenant_id":"\([^"]*\)".*/\1/p'
}
case "$url" in
  */agents/credential)
    t="$(printf '%s' "$body" | sed -n 's/.*"tenant_id":"\([^"]*\)".*/\1/p')"
    { [ "${STUB_MINT:-honour}" = honour ] && [ -n "$t" ]; } || t="$TP"
    printf '{"token":"%s"}' "$(jwt "$t")" >"$out"; printf 200; exit 0 ;;
  */mcp)
    ids=""; for x in $STUB_BINDINGS; do ids="${ids:+$ids,}{\"tenant_id\":\"$x\"}"; done
    printf '{"result":{"structuredContent":{"device_tenant_bindings":{"tenant_ids":[%s]}}}}' "$ids" >"$out"; printf 200; exit 0 ;;
  */0/author-session)
    repo="${url#*/pr-merge/}"; repo="${repo%/0/author-session}"; repo="${repo//%2F//}"
    c="$(claim)"
    for pair in $STUB_OWNERS; do
      if [ "${pair%%=*}" = "$repo" ] && [ "${pair#*=}" = "$c" ] && [ -n "$c" ]; then
        printf '{"repo":"%s","pr":0,"resolved":false}' "$repo" >"$out"; printf 200; exit 0
      fi
    done
    printf '{"error":"no author session"}' >"$out"; printf 404; exit 0 ;;
  */coord/agent-gates)
    key="$(printf '%s' "$rk" | tr -c 'A-Za-z0-9._-' '_')"
    printf '%s\n' "$rk" >>"$STUB_DIR/queried.log"
    printf '%s\n' "$(claim)" >>"$STUB_DIR/gates-claims.log"
    if [ -f "$STUB_DIR/$key.json" ]; then cat "$STUB_DIR/$key.json" >"$out"; else printf '{"gates":[],"count":0,"shown":0,"total":0,"offset":0,"truncated":false}' >"$out"; fi
    if [ -f "$STUB_DIR/$key.code" ]; then cat "$STUB_DIR/$key.code"; else printf 200; fi
    exit 0 ;;
esac
printf 000; exit 7
STUBEOF
chmod +x "$STUB"   # Linux refuses to exec a file without it; MSYS would run it anyway, which hid this
export RECOVERY_CENSUS_CURL="$STUB"
export TA="aaaaaaaa-0000-4000-8000-000000000001" TB="bbbbbbbb-0000-4000-8000-000000000002" TP="cccccccc-0000-4000-8000-000000000003"
# Every fixture repo's origin is github.com/org/<name>, owned by $TA; the
# device is bound to $TB (owns nothing) and $TA, in that order, so a proof has
# to survive a refutation first.
export STUB_BINDINGS="$TB $TA" STUB_MINT=honour
stub_owners() { # every repo under <ws> -> owned by $TA
  local g; STUB_OWNERS=""
  for g in "$1"/*/.git; do [ -d "$g" ] && STUB_OWNERS="$STUB_OWNERS org/$(basename "$(dirname "$g")")=$TA"; done
  export STUB_OWNERS
}

mkrepo() { # <ws> <name> -> prints the snapshot ref it wrote
  local d="$1/$2" sha ref
  git init -q "$d" && git -C "$d" commit -q --allow-empty -m "base $2"
  git -C "$d" remote add origin "https://github.com/org/$2.git"
  sha="$(git -C "$d" rev-parse HEAD)"
  ref="refs/wip/return-to-main/20260913T060442Z-$2-${sha:0:7}"
  git -C "$d" update-ref "$ref" "$sha"
  printf '%s' "$ref"
}
fixture() { # <stubdir> <resource_key> <gates-json-array> [total] [truncated]
  local key; key="$(printf '%s' "$2" | tr -c 'A-Za-z0-9._-' '_')"
  local n; n="$(printf '%s' "$3" | "$PY" -c 'import json,sys; print(len(json.load(sys.stdin)))')"
  printf '{"gates":%s,"count":%s,"shown":%s,"total":%s,"offset":0,"truncated":%s}' "$3" "$n" "$n" "${4:-$n}" "${5:-false}" >"$1/$key.json"
}
row() { # <rk> <verdict> <action> <outcome|null> [extra json fields]
  local oc="null"; [ "$4" != null ] && oc="\"$4\""
  printf '{"gate_id":"g-%s","claim_kind":"recovery_ref","resource_key":"%s","verdict":"%s","continuation_action":"%s","continuation_spawn":{"action":"%s"},"continuation_consumed_outcome":%s%s}' \
    "$RANDOM" "$1" "$2" "$3" "$3" "$oc" "${5:-}"
}
run() { # <ws> [args...] -> sets OUT RC
  local ws="$1"; shift
  [ -n "${KEEP_OWNERS:-}" ] || stub_owners "$ws"
  OUT="$(QONTINUI_MACHINE_ID="${CENSUS_DEV-$DEV}" bash "$SUBJECT" --root "$ws" "$@" 2>"$SANDBOX/err")"; RC=$?
}

# --- A: one gated, one ungated -> exactly one registration ---------------------
WS="$SANDBOX/wsA"; mkdir -p "$WS"; export STUB_DIR="$SANDBOX/stubA"; mkdir -p "$STUB_DIR"
REF_G="$(mkrepo "$WS" gated)"; REF_U="$(mkrepo "$WS" ungated)"
fixture "$STUB_DIR" "$DEV:gated:$REF_G" "[$(row "$DEV:gated:$REF_G" open run_skill null)]"
run "$WS"
eq "A1 exit 1 when a snapshot needs a gate" 1 "$RC"
eq "A2 exactly one NEEDS_GATE" 1 "$(printf '%s' "$OUT" | grep -o '"status":"NEEDS_GATE"' | wc -l | tr -d ' ')"
eq "A3 exactly one GATED" 1 "$(printf '%s' "$OUT" | grep -o '"status":"GATED"' | wc -l | tr -d ' ')"
REG="$(printf '%s' "$OUT" | "$PY" -c '
import json,sys
d=json.load(sys.stdin); regs=[r["registration"] for r in d["refs"] if r["registration"]]
print(json.dumps(regs, sort_keys=True))')"
WANT="$($PY -c '
import json,sys
dev,ref=sys.argv[1],sys.argv[2]
print(json.dumps([{"claim_kind":"recovery_ref","resource_key":f"{dev}:ungated:{ref}",
 "predicate":{"kind":"time_elapsed","duration_secs":1209600},
 "continuation":{"action":"run_skill","skill":"return-to-main","args":["--reap","ungated",ref,"--device",dev],"target_device_id":dev},
 "clearance_audience":"agent","gate_class":"routine-review"}], sort_keys=True))' "$DEV" "$REF_U")"
eq "A4 the one registration is the contract shape, for the ungated ref, verbatim" "$WANT" "$REG"
eq "A5 coord was asked about both anchors" 2 "$(wc -l <"$STUB_DIR/queried.log" | tr -d ' ')"

# --- B: liveness per gate row --------------------------------------------------
WS="$SANDBOX/wsB"; mkdir -p "$WS"; export STUB_DIR="$SANDBOX/stubB"; mkdir -p "$STUB_DIR"
b_case() { # <label> <expected status> <verdict> <action> <outcome> [extra]
  local name="b$1" ref rk st
  ref="$(mkrepo "$WS" "$name")"; rk="$DEV:$name:$ref"
  fixture "$STUB_DIR" "$rk" "[$(row "$rk" "$3" "$4" "$5" "${6:-}")]"
  B_EXPECT+=("$name=$2")
}
B_EXPECT=()
b_case open GATED open run_skill null
b_case muted GATED open run_skill null ',"muted":true'
b_case inflight GATED cleared run_skill null ',"continuation_dispatched_at":"2026-09-14T00:00:00Z"'
b_case pending GATED cleared run_skill null
b_case abandoned NEEDS_GATE cleared run_skill "work_abandoned: refused: sole holder"
b_case spawned NEEDS_GATE cleared run_skill spawned
b_case completed NEEDS_GATE cleared run_skill work_completed
b_case notify NEEDS_GATE cleared notify_only null
b_case expired NEEDS_GATE cleared run_skill null ',"continuation_expired_at":"2026-09-14T00:00:00Z"'
b_case cancelled NEEDS_GATE cleared run_skill null ',"continuation_cancelled_at":"2026-09-14T00:00:00Z"'
b_case failed NEEDS_GATE failed run_skill null
run "$WS"
for e in "${B_EXPECT[@]}"; do
  n="${e%%=*}"; want="${e#*=}"
  got="$(printf '%s' "$OUT" | "$PY" -c 'import json,sys; d=json.load(sys.stdin); print(next(r["status"] for r in d["refs"] if r["repo"]==sys.argv[1]))' "$n")"
  eq "B ${n#b}: $want" "$want" "$got"
done
eq "B exit 1 (some need a gate, all decided)" 1 "$RC"

# --- C: coord 500 -> UNKNOWN ---------------------------------------------------
WS="$SANDBOX/wsC"; mkdir -p "$WS"; export STUB_DIR="$SANDBOX/stubC"; mkdir -p "$STUB_DIR"
REF="$(mkrepo "$WS" c1)"; key="$(printf '%s' "$DEV:c1:$REF" | tr -c 'A-Za-z0-9._-' '_')"
printf '{"error":"boom"}' >"$STUB_DIR/$key.json"; printf 500 >"$STUB_DIR/$key.code"
run "$WS"
eq "C1 coord 500 -> exit 3" 3 "$RC"
has "C2 ... the ref is UNKNOWN" '"status":"UNKNOWN"' "$OUT"
has "C3 ... with no registration" '"registration":null' "$OUT"

# --- D: filter not honoured, or a truncated page -> UNKNOWN ---------------------
WS="$SANDBOX/wsD"; mkdir -p "$WS"; export STUB_DIR="$SANDBOX/stubD"; mkdir -p "$STUB_DIR"
REF="$(mkrepo "$WS" d1)"
fixture "$STUB_DIR" "$DEV:d1:$REF" "[$(row "$DEV:other:refs/wip/return-to-main/x" open run_skill null)]"
run "$WS"
eq "D1 a row outside the anchor (the door ignored the filter) -> exit 3" 3 "$RC"
has "D2 ... UNKNOWN, never GATED" '"status":"UNKNOWN"' "$OUT"
WS="$SANDBOX/wsD2"; mkdir -p "$WS"; export STUB_DIR="$SANDBOX/stubD2"; mkdir -p "$STUB_DIR"
REF="$(mkrepo "$WS" d2)"
fixture "$STUB_DIR" "$DEV:d2:$REF" "[]" 150 true
run "$WS"
eq "D3 a truncated page -> exit 3" 3 "$RC"

# --- E: no refs -> exit 0, coord never asked ------------------------------------
WS="$SANDBOX/wsE"; mkdir -p "$WS"; export STUB_DIR="$SANDBOX/stubE"; mkdir -p "$STUB_DIR"
git init -q "$WS/plain" && git -C "$WS/plain" commit -q --allow-empty -m base
run "$WS"
eq "E1 no snapshots -> exit 0" 0 "$RC"
if [ -f "$STUB_DIR/argv.log" ]; then bad "E2 coord is never asked when there is nothing to reconcile" "$(cat "$STUB_DIR/argv.log")"; else ok "E2 coord is never asked when there is nothing to reconcile"; fi

# --- F: linked worktrees are not primaries; --only ------------------------------
WS="$SANDBOX/wsF"; mkdir -p "$WS"; export STUB_DIR="$SANDBOX/stubF"; mkdir -p "$STUB_DIR"
REF1="$(mkrepo "$WS" f1)"; REF2="$(mkrepo "$WS" f2)"
git -C "$WS/f1" worktree add -q "$WS/f1-linked" -b linked >/dev/null 2>&1
run "$WS"
eq "F1 the linked worktree is not scanned: two refs, not three" 2 "$(printf '%s' "$OUT" | "$PY" -c 'import json,sys; print(json.load(sys.stdin)["counts"]["refs"])')"
run "$WS" --only f2
eq "F2 --only f2 scans f2 alone" '["f2"]' "$(printf '%s' "$OUT" | "$PY" -c 'import json,sys; print(json.dumps([r["repo"] for r in json.load(sys.stdin)["refs"]]))')"

# --- G: no device; the bearer stays off argv --------------------------------------
CENSUS_DEV="" run "$WS"
eq "G1 no device id -> exit 3" 3 "$RC"
lacks "G2 the minted bearer never appears on curl's argv" "eyJhbGciOiJub25lIn0" "$(cat "$SANDBOX"/stub*/argv.log 2>/dev/null)"
lacks "G3 ... nor does an Authorization value" "Bearer" "$(cat "$SANDBOX"/stub*/argv.log 2>/dev/null)"

# --- H: the tenant the gates are read under ---------------------------------------
# (plan 2026-09-23-ccfg-scripts-mint-device-credentials-with-no-tenant, Phase 2)
WS="$SANDBOX/wsH"; mkdir -p "$WS"; export STUB_DIR="$SANDBOX/stubH"; mkdir -p "$STUB_DIR"
REF="$(mkrepo "$WS" h1)"
fixture "$STUB_DIR" "$DEV:h1:$REF" "[$(row "$DEV:h1:$REF" open run_skill null)]"
run "$WS"
eq "H1 the owning tenant's gate is found: exit 0" 0 "$RC"
[ "$RC" = 0 ] || printf '       reasons: %s\n' "$(printf '%s' "$OUT" | "$PY" -c 'import json,sys; print([r.get("reason") for r in json.load(sys.stdin).get("refs",[])])' 2>&1 | cut -c1-1500)"
eq "H2 the gates were read under the OWNING tenant's token" "$TA" "$(sort -u "$STUB_DIR/gates-claims.log")"
has "H3 the ref names the tenant it was looked up in" "\"tenant_id\":\"$TA\"" "$OUT"
# A coord that ignores tenant_id hands back the pointer tenant's token, whose
# gate page is EMPTY. That must read UNKNOWN, never NEEDS_GATE.
WS="$SANDBOX/wsH4"; mkdir -p "$WS"; export STUB_DIR="$SANDBOX/stubH4"; mkdir -p "$STUB_DIR"
REF="$(mkrepo "$WS" h4)"
STUB_MINT=ignore run "$WS"
eq "H4 a mint that ignores tenant_id + a 200-empty page -> exit 3" 3 "$RC"
has "H5 ... the ref is UNKNOWN" '"status":"UNKNOWN"' "$OUT"
lacks "H6 ... and no registration is offered" '"status":"NEEDS_GATE"' "$OUT"
if [ -f "$STUB_DIR/queried.log" ]; then bad "H7 ... and no gate page was read under the pointer's token" "$(cat "$STUB_DIR/gates-claims.log")"; else ok "H7 ... and no gate page was read under the pointer's token"; fi
WS="$SANDBOX/wsH8"; mkdir -p "$WS"; export STUB_DIR="$SANDBOX/stubH8"; mkdir -p "$STUB_DIR"
REF="$(mkrepo "$WS" h8)"
KEEP_OWNERS=1 STUB_OWNERS="org/h8=$TP" run "$WS"
eq "H8 a repo no bound tenant owns -> exit 3" 3 "$RC"
has "H9 ... UNKNOWN, naming the refutation" "no tenant this device is bound to owns org/h8" "$OUT"
WS="$SANDBOX/wsH10"; mkdir -p "$WS"; export STUB_DIR="$SANDBOX/stubH10"; mkdir -p "$STUB_DIR"
REF="$(mkrepo "$WS" h10)"; git -C "$WS/h10" remote set-url origin "/srv/git/h10.git"
run "$WS"
eq "H10 a non-GitHub origin -> exit 3" 3 "$RC"
has "H11 ... UNKNOWN, saying why" "not a github.com owner/name" "$OUT"

# --- mutation control ---------------------------------------------------------------
if ! mc_is_mutant; then
  echo "  -- mutation control"
  mkdir -p "$SANDBOX/control"; cp "$SUBJECT_REAL" "$SANDBOX/control/recovery-ref-census.sh"
  if MC_MUTANT=1 MC_SUBJECT="$SANDBOX/control/recovery-ref-census.sh" bash "$SELF" >"$SANDBOX/control.log" 2>&1; then
    ok "M0 control: an unmutated staged copy passes"
  else
    bad "M0 control: an UNMUTATED staged copy fails -- the staging would redden every mutant" "$(tail -5 "$SANDBOX/control.log")"
  fi
  mc_expect_red "M1 a consumed outcome no longer ends liveness (a bare spawned reads live)" "$SUBJECT_REAL" 's/ or outcome)/)/' -- bash "$SELF"
  mc_expect_red "M2 rows outside the requested anchor are trusted" "$SUBJECT_REAL" 's/        sys.exit(3)  # the door did not honour the filter.*/        continue/' -- bash "$SELF"
  mc_expect_red "M3 the registration's retention window drifts from 14 days" "$SUBJECT_REAL" 's/"duration_secs":1209600/"duration_secs":86400/' -- bash "$SELF"
  mc_expect_red "M4 a ref stops naming the tenant its gates were read in" "$SUBJECT_REAL" 's/\\"tenant_id\\":\$(jsn "\$tenant"),//' -- bash "$SELF"
  if [ "$MC_RED" -lt "$MC_DECLARED" ]; then
    FAIL=$((FAIL + 1)); echo "  FAIL mutation control: $MC_RED of $MC_DECLARED declared mutation(s) reddened the suite"
  fi
fi

echo "recovery-ref-census-test: $PASS passed, $FAIL failed"
mc_is_mutant || mc_trailer "$((PASS + FAIL))"
[ "$FAIL" -eq 0 ]
