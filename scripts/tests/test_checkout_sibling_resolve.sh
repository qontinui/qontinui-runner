#!/usr/bin/env bash
# Regression test for the `resolve` step of
# .github/actions/checkout-sibling/action.yml, against a STUBBED `curl` that
# serves GitHub REST fixtures. No network, no real repository.
#
# WHY THIS FILE EXISTS. The action's resolver decides which sibling tree every
# compile gate builds against, and it has a Rust twin
# (src-tauri/src/ci_node/sibling.rs `resolve_declaration`) that is unit-tested
# in-tree. The bash lane had no in-repo test at all: runner#2485 (several
# TRAILING declarations are declined, not ambiguous — the runner#2311 false
# red) was checked only by an out-of-tree harness, so nothing kept the two
# lanes agreeing after that PR merged. This file pins the reconcile table from
# that PR, plus the UNKNOWN and declaration-invalid arms.
#
# HOW IT TESTS THE SHIPPED BYTES. The step's `run: |` body is EXTRACTED from
# the action at run time. A renamed or re-indented step extracts nothing and
# this test FAILS rather than passing vacuously.
#
# `curl` is stubbed as an EXPORTED SHELL FUNCTION, not a PATH shim, so the stub
# also takes effect under Git Bash on windows-latest, where extensionless
# scripts on PATH are not reliably executable.
#
# Set CHECKOUT_SIBLING_ACTION_UNDER_TEST to run against another copy of the
# action (e.g. a pre-#2485 one, which must fail the runner#2311 case).
#
# Run locally:
#   bash scripts/tests/test_checkout_sibling_resolve.sh

set -euo pipefail

tests_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$tests_dir/../.." && pwd)"
action="${CHECKOUT_SIBLING_ACTION_UNDER_TEST:-$repo_root/.github/actions/checkout-sibling/action.yml}"
[ -f "$action" ] || { echo "::error::cannot find $action"; exit 1; }
command -v jq >/dev/null || { echo "::error::jq is required"; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- Extract the step body ----------------------------------------------------
step="$work/resolve.sh"
awk '
  state == 0 && $0 == "    - id: resolve" { state = 1; next }
  state == 1 && $0 == "      run: |" { state = 2; next }
  state == 2 && /^    [^ ]/ { exit }
  state == 2 { sub(/^        /, ""); print }
' "$action" > "$step"
tr -d '\r' < "$step" > "$step.tmp" && mv "$step.tmp" "$step"
if ! grep -q 'Reconcile' "$step" || [ "$(wc -l < "$step")" -lt 100 ]; then
  echo "::error::extracted no usable resolve step from $action — was the step renamed or re-indented?"
  exit 1
fi

failures=0
passes=0
pass() { printf '  PASS  %s\n' "$1"; passes=$((passes + 1)); }
fail() { printf '  FAIL  %s: %s\n' "$1" "$2"; failures=$((failures + 1)); }

# --- The curl stub ------------------------------------------------------------
# Serves "<body>\n<status>" exactly as `curl -w $'\n%{http_code}'` would, from
# files under $FIX:
#   labels.json / labels.status         repos/<this>/issues/<n>/labels
#   pulls_page<N>.json                  repos/<sibling>/pulls?state=open&...
#   pull_<n>.json                       repos/<sibling>/pulls/<n> (absent = 404)
# A file named `transport_down` makes every call fail at the transport layer.
curl() {
  local url="${*: -1}"
  [ -e "$FIX/transport_down" ] && return 7
  # api() parses the status off a `-w '\n%{http_code}'` trailer. Serve one only
  # when it was asked for, so a regression dropping that flag fails here too.
  case " $* " in
    *'%{http_code}'*) : ;;
    *) printf 'garbled'; return 0 ;;
  esac
  case "$url" in
    */issues/*/labels*)
      printf '%s\n%s' "$(cat "$FIX/labels.json")" "$(cat "$FIX/labels.status" 2>/dev/null || echo 200)" ;;
    */pulls\?state=open*)
      local page="${url##*page=}"
      if [ -f "$FIX/pulls_page${page}.json" ]; then
        printf '%s\n200' "$(cat "$FIX/pulls_page${page}.json")"
      else
        printf '[]\n200'
      fi ;;
    */pulls/*)
      local n="${url##*/}"
      if [ -f "$FIX/pull_${n}.json" ]; then
        printf '%s\n200' "$(cat "$FIX/pull_${n}.json")"
      else
        printf '{"message":"Not Found"}\n404'
      fi ;;
    *) printf '{}\n500' ;;
  esac
}
export -f curl

# --- Fixture builders ---------------------------------------------------------
THIS=qontinui/qontinui-runner
SIB=qontinui/ui-bridge
PR=100

new_case() {
  FIX="$work/case-$1"
  rm -rf "$FIX"; mkdir -p "$FIX"
  export FIX
  echo '[]' > "$FIX/labels.json"
}
# self_labels <label>...
self_labels() {
  local out='[]' l
  for l in "$@"; do out="$(jq -c --arg n "$l" '. + [{name: $n}]' <<< "$out")"; done
  echo "$out" > "$FIX/labels.json"
}
# sibling_pr <number> <label>...  — an OPEN sibling PR carrying those labels,
# appended to page 1 of the open-PR list, plus a valid detail record.
sibling_pr() {
  local n="$1"; shift
  local labels='[]' l page="$FIX/pulls_page1.json"
  for l in "$@"; do labels="$(jq -c --arg n "$l" '. + [{name: $n}]' <<< "$labels")"; done
  [ -f "$page" ] || echo '[]' > "$page"
  jq -c --argjson n "$n" --argjson labels "$labels" '. + [{number: $n, labels: $labels}]' "$page" > "$page.tmp"
  mv "$page.tmp" "$page"
  detail "$n" open main "$SIB"
}
# detail <number> <state> <base> <head repo>
detail() {
  jq -nc --arg state "$2" --arg base "$3" --arg repo "$4" --arg ref "feat-$1" \
     --arg sha "$(printf '%040d' "$1")" \
     '{state: $state, base: {ref: $base}, head: {ref: $ref, sha: $sha, repo: {full_name: $repo}}}' \
     > "$FIX/pull_$1.json"
}

# run_case <name> <accept> [pr-number]  -> sets RC, OUT (log), REF, SHA, SOURCE
# $PIN, when set, is the pin-file input (relative to $FIX, the workspace root).
run_case() {
  local out_file="$FIX/github_output"
  : > "$out_file"
  set +e
  OUT="$(env GH_TOKEN=x PR_NUMBER="${3-$PR}" THIS_REPO="$THIS" SIBLING_REPO="$SIB" \
           ACCEPT_TRAILING="$2" PIN_FILE="${PIN:-}" GITHUB_OUTPUT="$out_file" \
           GITHUB_WORKSPACE="$FIX" GITHUB_ACTION_PATH="$FIX" \
           bash "$step" 2>&1)"
  RC=$?
  set -e
  REF="$(sed -n 's/^ref=//p' "$out_file")"
  SHA="$(sed -n 's/^sha=//p' "$out_file")"
  SOURCE="$(sed -n 's/^source=//p' "$out_file")"
}

# expect_ok <name> <ref> <source>
expect_ok() {
  if [ "$RC" -ne 0 ]; then fail "$1" "exit $RC; log:"$'\n'"$OUT"; return; fi
  if [ "$REF" != "$2" ] || [ "$SOURCE" != "$3" ]; then
    fail "$1" "expected ref=$2 source=$3, got ref=$REF source=$SOURCE"; return
  fi
  pass "$1 -> ref=$REF source=$SOURCE"
}
# expect_refused <name> <log substring>
expect_refused() {
  if [ "$RC" -eq 0 ]; then fail "$1" "expected a refusal, got ref=$REF source=$SOURCE"; return; fi
  if [ -s "$FIX/github_output" ]; then fail "$1" "refused but still emitted outputs"; return; fi
  if ! grep -qF -- "$2" <<< "$OUT"; then fail "$1" "refused without '$2'; log:"$'\n'"$OUT"; return; fi
  pass "$1 -> refused ($2)"
}

echo "checkout-sibling resolve step: $action"

# --- No PR / no declaration ---------------------------------------------------
new_case no-pr
run_case no-pr false ""
expect_ok "not a pull_request event" main none

new_case none
sibling_pr 9 "coord:downstream-of=qontinui-runner#999"   # someone else's PR
run_case none false
expect_ok "no declaration naming this PR" main none

# --- Leading forms ------------------------------------------------------------
new_case down-bare
self_labels "coord:downstream-of=ui-bridge#5"
detail 5 open main "$SIB"
run_case down-bare false
expect_ok "downstream-of, bare repo (owner optional)" feat-5 downstream-of
if [ "$SHA" = "$(printf '%040d' 5)" ]; then pass "downstream-of emits the head SHA"; else fail "downstream-of SHA" "got '$SHA'"; fi

new_case down-owner
self_labels "coord:downstream-of=qontinui/ui-bridge#5" "unrelated"
detail 5 open main "$SIB"
run_case down-owner false
expect_ok "downstream-of, owner-qualified" feat-5 downstream-of

new_case up
sibling_pr 5 "coord:upstream-of=qontinui/qontinui-runner#100"
run_case up false
expect_ok "sibling PR declares upstream-of this PR" feat-5 upstream-of

new_case both-leading-same
self_labels "coord:downstream-of=ui-bridge#5"
sibling_pr 5 "coord:upstream-of=qontinui-runner#100"
run_case both-leading-same false
expect_ok "both leading forms, same PR" feat-5 downstream-of+upstream-of

new_case two-downs
self_labels "coord:downstream-of=ui-bridge#5" "coord:downstream-of=ui-bridge#6"
run_case two-downs false
expect_refused "two downstream-of labels naming different PRs" "Contradictory declarations"

new_case two-ups
sibling_pr 5 "coord:upstream-of=qontinui-runner#100"
sibling_pr 6 "coord:upstream-of=qontinui-runner#100"
run_case two-ups false
expect_refused "two sibling PRs declaring upstream-of" "Contradictory declarations"

# --- Trailing forms (the runner#2485 reconcile table) -------------------------
new_case runner-2311
sibling_pr 266 "coord:downstream-of=qontinui-runner#100"
sibling_pr 270 "coord:downstream-of=qontinui-runner#100"
run_case runner-2311 false
expect_ok "several trailing, accept=false (runner#2311)" main trailing-declined
if grep -qF "ui-bridge#266" <<< "$OUT" && grep -qF "ui-bridge#270" <<< "$OUT"; then
  pass "declined trailing PRs are listed in the log"
else
  fail "declined trailing PRs listed" "log:"$'\n'"$OUT"
fi
if [ -z "$SHA" ]; then pass "trailing-declined emits no SHA"; else fail "trailing-declined SHA" "got '$SHA'"; fi

run_case runner-2311 true
expect_refused "several trailing, accept=true" "Contradictory declarations"

new_case mixed-trailing
self_labels "coord:upstream-of=ui-bridge#7"
sibling_pr 8 "coord:downstream-of=qontinui-runner#100"
run_case mixed-trailing false
expect_ok "self-upstream-of + sibling-downstream-of, different PRs, accept=false" main trailing-declined
run_case mixed-trailing true
expect_refused "self-upstream-of + sibling-downstream-of, different PRs, accept=true" "Contradictory declarations"

new_case one-trailing
sibling_pr 9 "coord:downstream-of=qontinui-runner#100"
run_case one-trailing false
expect_ok "one trailing, accept=false" main trailing-declined
run_case one-trailing true
expect_ok "one trailing, accept=true" feat-9 sibling-downstream-of

new_case trailing-same-pr
self_labels "coord:upstream-of=ui-bridge#7"
sibling_pr 7 "coord:downstream-of=qontinui-runner#100"
run_case trailing-same-pr true
expect_ok "both trailing forms, same PR, accept=true" feat-7 self-upstream-of+sibling-downstream-of

new_case lead-plus-two-trailing
self_labels "coord:downstream-of=ui-bridge#5"
detail 5 open main "$SIB"
sibling_pr 266 "coord:downstream-of=qontinui-runner#100"
sibling_pr 270 "coord:downstream-of=qontinui-runner#100"
run_case lead-plus-two-trailing false
expect_refused "leading + two trailing" "name DIFFERENT ui-bridge PRs"

new_case cycle
self_labels "coord:downstream-of=ui-bridge#5"
sibling_pr 5 "coord:downstream-of=qontinui-runner#100"
run_case cycle false
expect_refused "leading + trailing naming the same PR (cycle)" "Contradictory merge ORDER"

# --- Declaration invalid ------------------------------------------------------
new_case closed
self_labels "coord:downstream-of=ui-bridge#5"
detail 5 closed main "$SIB"
run_case closed false
expect_refused "declared PR is closed" "not 'open'"

new_case stacked
self_labels "coord:downstream-of=ui-bridge#5"
detail 5 open some-feature "$SIB"
run_case stacked false
expect_refused "declared PR is not based on main" "not 'main'"

new_case fork
self_labels "coord:downstream-of=ui-bridge#5"
detail 5 open main someone/ui-bridge
run_case fork false
expect_refused "declared PR's head is on a fork" "fork's head commit"

new_case missing
self_labels "coord:downstream-of=ui-bridge#5"
run_case missing false
expect_refused "declared PR does not exist" "HTTP 404"

new_case malformed
self_labels "coord:downstream-of=ui-bridge#abc"
run_case malformed false
expect_refused "malformed label" "Malformed label"

# --- UNKNOWN arms and input validation ----------------------------------------
new_case labels-500
echo 500 > "$FIX/labels.status"
run_case labels-500 false
expect_refused "labels read answers 500" "UNKNOWN"

new_case transport
touch "$FIX/transport_down"
run_case transport false
expect_refused "no answer at all" "UNKNOWN"

new_case bad-accept
run_case bad-accept yes
expect_refused "accept-trailing-sibling not a boolean" "must be 'true' or 'false'"

new_case bad-pr
run_case bad-pr false "12;rm"
expect_refused "non-numeric PR number" "not numeric"

# --- Pagination ---------------------------------------------------------------
new_case paged
jq -nc '[range(1;101) | {number: ., labels: []}]' > "$FIX/pulls_page1.json"
echo '[]' > "$FIX/pulls_page2.json"
jq -c '. + [{number: 266, labels: [{name: "coord:downstream-of=qontinui-runner#100"}]},
            {number: 270, labels: [{name: "coord:downstream-of=qontinui-runner#100"}]}]' \
   "$FIX/pulls_page2.json" > "$FIX/p2" && mv "$FIX/p2" "$FIX/pulls_page2.json"
detail 266 open main "$SIB"
run_case paged true
expect_refused "trailing claimants found past page 1" "Contradictory declarations"

# --- The pin manifest (lowest-priority answer) --------------------------------
PINSHA=$(printf 'a%.0s' $(seq 40))
pins() { printf '%s\n' "$@" > "$FIX/pins.conf"; }
PIN=pins.conf

new_case pin-none
pins "# header" "$SIB $PINSHA  # trailing comment"
run_case pin-none false
expect_ok "no declaration, sibling pinned" main none+pinned
if [ "$SHA" = "$PINSHA" ]; then pass "pinned SHA is emitted"; else fail "pinned SHA" "got '$SHA'"; fi

new_case pin-trailing
pins "$SIB $PINSHA"
sibling_pr 9 "coord:downstream-of=qontinui-runner#100"
run_case pin-trailing false
expect_ok "trailing declined, sibling pinned" main trailing-declined+pinned

new_case pin-outranked
pins "$SIB $PINSHA"
self_labels "coord:downstream-of=ui-bridge#5"
detail 5 open main "$SIB"
run_case pin-outranked false
expect_ok "a declaration outranks the pin" feat-5 downstream-of
if [ "$SHA" = "$(printf '%040d' 5)" ]; then pass "declared head SHA, not the pin"; else fail "pin outranked SHA" "got '$SHA'"; fi

new_case pin-unlisted
pins "qontinui/qontinui-schemas $PINSHA"
run_case pin-unlisted false
expect_ok "sibling absent from the manifest" main none

new_case pin-missing-file
run_case pin-missing-file false
expect_ok "manifest file absent" main none

new_case pin-no-sha
pins "$SIB"
run_case pin-no-sha false
expect_refused "pinned entry with no SHA" "no commit SHA"

new_case pin-dup
pins "$SIB $PINSHA" "$SIB $PINSHA"
run_case pin-dup false
expect_refused "sibling listed twice" "times"

new_case pin-bad-sha
pins "$SIB deadbeef"
run_case pin-bad-sha false
expect_refused "pin is not a full SHA" "not a 40-character"

new_case pin-extra-field
pins "$SIB $PINSHA extra"
run_case pin-extra-field false
expect_refused "pin entry with trailing text" "expected exactly 2"
unset PIN

echo
echo "$passes passed, $failures failed"
[ "$failures" -eq 0 ]
