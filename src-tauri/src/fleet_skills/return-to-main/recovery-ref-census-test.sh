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
unset COORD_DEVICE_JWT QONTINUI_MACHINE_ID COORD_HTTP_URL
mkdir -p "$HOME"
PY="$(command -v python3 || command -v python)"
DEV="11111111-2222-4333-8444-555555555555"

# --- the curl stub -----------------------------------------------------------
# Fixtures: $STUB_DIR/<sanitised resource_key>.json (the agent-gates body) and
# optional .code (HTTP status, default 200). /agents/credential answers a token.
STUB="$SANDBOX/curl-stub.sh"
cat >"$STUB" <<'STUBEOF'
#!/usr/bin/env bash
out=""; rk=""; url=""
printf '%s\n' "$*" >>"$STUB_DIR/argv.log"
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -w|-m|--connect-timeout|-X|-d) shift 2 ;;
    -H) shift 2 ;;
    --data-urlencode) case "$2" in resource_key=*) rk="${2#resource_key=}" ;; esac; shift 2 ;;
    http*) url="$1"; shift ;;
    *) shift ;;
  esac
done
case "$url" in
  */agents/credential)
    printf '{"token":"eyJhbGciOiJub25lIn0.eyJleHAiOjk5OTk5OTk5OTl9.c2ln"}' >"$out"; printf 200; exit 0 ;;
  */coord/agent-gates)
    key="$(printf '%s' "$rk" | tr -c 'A-Za-z0-9._-' '_')"
    printf '%s\n' "$rk" >>"$STUB_DIR/queried.log"
    if [ -f "$STUB_DIR/$key.json" ]; then cat "$STUB_DIR/$key.json" >"$out"; else printf '{"gates":[],"count":0,"shown":0,"total":0,"offset":0,"truncated":false}' >"$out"; fi
    if [ -f "$STUB_DIR/$key.code" ]; then cat "$STUB_DIR/$key.code"; else printf 200; fi
    exit 0 ;;
esac
printf 000; exit 7
STUBEOF
chmod +x "$STUB"   # Linux refuses to exec a file without it; MSYS would run it anyway, which hid this
export RECOVERY_CENSUS_CURL="$STUB"

mkrepo() { # <ws> <name> -> prints the snapshot ref it wrote
  local d="$1/$2" sha ref
  git init -q "$d" && git -C "$d" commit -q --allow-empty -m "base $2"
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
  if [ "$MC_RED" -lt "$MC_DECLARED" ]; then
    FAIL=$((FAIL + 1)); echo "  FAIL mutation control: $MC_RED of $MC_DECLARED declared mutation(s) reddened the suite"
  fi
fi

echo "recovery-ref-census-test: $PASS passed, $FAIL failed"
mc_is_mutant || mc_trailer "$((PASS + FAIL))"
[ "$FAIL" -eq 0 ]
