#!/usr/bin/env bash
# shellcheck disable=SC2016  # literal `$` in awk programs and grep patterns is intended
# Regression test for the `Open or update the pin bump PR` step of
# .github/workflows/sibling-pin-bump.yml, against a STUBBED `gh` and `git`. No
# network, no real repository.
#
# WHY THIS FILE EXISTS. That step files the nightly's fix as a PR that is born
# a draft and handed to scripts/ci/ready-bot-draft-pr.sh, which readies it once
# its head carries Actions check runs. Once it is ready it may be a live
# merge-train candidate, so the step must never force-push under it — except
# when it CONFLICTS with main, which no lander can clear: then it converts the
# PR back to a draft and rebuilds it. Each of those branches is pinned here.
#
# HOW IT TESTS THE SHIPPED BYTES. The step's `run: |` body is EXTRACTED from the
# workflow at run time (the same brittle-and-loud rule as
# test_schema_pr_step_guards.sh: a renamed or re-indented step extracts
# nothing and this test FAILS). The real ready script is placed in the fixture
# checkout, so the hand-off is exercised end to end.
#
# MUTATION PROOFS. The last section re-runs this file against deliberately
# broken copies of the workflow (child sets PIN_BUMP_STEP_MUTANT=1).
#
# Plan: plans/2026-09-17-atlas-self-heal-files-its-own-fix-as-a-draft-and-nothing-lands-it.md
#
# Run locally:
#   bash scripts/tests/test_pin_bump_pr_step.sh

set -euo pipefail

tests_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$tests_dir/../.." && pwd)"
real_workflow="$repo_root/.github/workflows/sibling-pin-bump.yml"
workflow="${PIN_BUMP_WORKFLOW_UNDER_TEST:-$real_workflow}"
ready_script="$repo_root/scripts/ci/ready-bot-draft-pr.sh"
for f in "$workflow" "$ready_script"; do
  [ -f "$f" ] || { echo "::error::cannot find $f"; exit 1; }
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0
assert() {
  if [ "$2" = "$3" ]; then
    printf '  PASS  %-62s %s\n' "$1" "$3"
  else
    printf '  FAIL  %-62s expected %s, got %s\n' "$1" "$2" "$3"
    failures=$((failures + 1))
  fi
}
count() { grep -cE -- "$1" "$2" || true; }

# --- Extract the step body ----------------------------------------------------
STEP="Open or update the pin bump PR"
step="$work/step.sh"
awk -v name="$STEP" '
  state == 0 && $0 == "      - name: " name { state = 1; next }
  state == 1 && $0 ~ /^      - name: / { exit }
  state == 1 && $0 == "        run: |" { state = 2; next }
  state == 2 {
    if ($0 ~ /^[[:space:]]*$/) { print ""; next }
    if ($0 !~ /^          /) { exit }
    print substr($0, 11)
  }
' "$workflow" > "$step"
first_line="$( { grep -m1 -vE '^[[:space:]]*(#|$)' "$step" || true; } )"
last_line="$( { grep -vE '^[[:space:]]*$' "$step" || true; } | tail -n1)"
if [ "$(wc -l < "$step" | tr -d ' ')" -lt 100 ] || [ "$first_line" != "set -euo pipefail" ] \
   || [ "$last_line" != '} >> "$GITHUB_STEP_SUMMARY"' ]; then
  echo "::error::the bump-PR step body did not extract cleanly from $workflow"
  echo "::error::  first=[$first_line] last=[$last_line]. Fix the extractor — do not relax the check."
  exit 1
fi
bash -n "$step"
echo "Extracted bump-PR step body: $(wc -l < "$step" | tr -d ' ') lines"

code_only="$work/step.code.sh"
{ grep -vE '^[[:space:]]*#' "$step" || true; } > "$code_only"
echo ""
echo "Static properties of the shipped step body:"
assert "PR is still created as a draft" 1 "$(count '--draft' "$code_only")"
assert "ready call pins the pushed head" 1 "$(count 'ready-bot-draft-pr.sh .*--expect-head "\$pushed_sha"' "$code_only")"
assert "ready call waits 180s for checks" 1 "$(count 'ready-bot-draft-pr.sh .*--wait-for-checks-seconds 180' "$code_only")"
# The sweep step's selector: every default-branch schedule/dispatch (`sweep`,
# not the drift-bearing `self_heal`), and under !cancelled() so an upstream
# failure does not suppress the re-decide and the page.
sweep_if="$(awk '
  $0 == "      - name: Sweep the bump PR (ready it once it carries checks; page if parked)" { s = 1; next }
  s && /^      - name: / { exit }
  s && /^        if: / { print substr($0, 13); exit }
' "$workflow")"
assert "sweep if: is keyed on the scan's sweep output" 1 "$(printf '%s\n' "$sweep_if" | grep -c "steps.scan.outputs.sweep == 'true'" || true)"
assert "sweep if: runs under !cancelled()" 1 "$(printf '%s\n' "$sweep_if" | grep -c '^\${{ !cancelled() && ' || true)"
assert "pushed_sha is read after the push" 1 "$(count '^[[:space:]]*pushed_sha="\$\(git rev-parse HEAD\)"' "$code_only")"

# --- Stubs --------------------------------------------------------------------
bin="$work/bin"
mkdir -p "$bin"
HEAD_SHA="fb5f4d0d5fb5f4d0d5fb5f4d0d5fb5f4d0d5fb5f"

cat > "$bin/gh" <<'STUB'
#!/usr/bin/env bash
# Keys each call and logs the key; answers are PRE-FILTERED (--jq never
# reaches a real jq here). GH_STUB_FAIL lists keys that must fail.
json=""; state=""; method="GET"; prev=""; body_file=""
for a in "$@"; do
  [ "$prev" = "--json" ] && json="$a"
  [ "$prev" = "--state" ] && state="$a"
  [ "$prev" = "-X" ] && method="$a"
  [ "$prev" = "--body-file" ] && body_file="$a"
  prev="$a"
done
key=""
case "${1:-} ${2:-}" in
  "pr list")
    case "$json" in
      *headRefOid*) key="list:ready" ;;     # the ready script's lookup
      *isDraft*)    key="list:schemas" ;;   # the schemas step's ready-PR probe
      *)            key="list:$state" ;;
    esac ;;
  "pr view")   key="view:$json" ;;
  "pr edit")   key="edit" ;;
  "pr create") key="create" ;;
  "pr reopen") key="reopen" ;;
  "pr comment") key="comment" ;;
  api\ *)
    case "$*" in
      *convertPullRequestToDraft*) key="graphql:convert" ;;
      *" graphql "*)   key="graphql" ;;
      *"/check-runs"*) key="check-runs" ;;
      *"/comments"*)   if [ "$method" = "GET" ]; then key="comments-read"; else key="comment-post"; fi ;;
      *"/pulls/"*)     [ "$method" = "PATCH" ] && key="pr-close" ;;
    esac ;;
esac
[ -n "$key" ] || { echo "gh stub: unhandled invocation '$*'" >&2; exit 97; }
echo "$key" >> "${GH_STUB_LOG:?}"
# Which token each call ran on — the in-step ready decision must use
# ACTIONS_TOKEN (GITHUB_TOKEN), never the PAT.
echo "$key ${GH_TOKEN:-<unset>}" >> "${GH_STUB_TOKEN_LOG:-/dev/null}"
case " ${GH_STUB_FAIL:-} " in
  *" $key "*) echo "gh: HTTP 403 ($key)" >&2; exit 1 ;;
esac
case "$key" in
  list:open)   printf '%s\n' "${GH_STUB_OPEN:-}" ;;
  list:closed) printf '%s\n' "${GH_STUB_CLOSED:-}" ;;
  list:ready)
    printf '77\t%s\t%s\tPR_kwDOtest77\t2026-09-29T00:00:00Z\thttps://github.com/qontinui/qontinui-runner/pull/77\tjspinak\t%s\t%s\n' \
      "${GH_STUB_READY_ISDRAFT:-true}" "${GH_STUB_HEAD:?}" "${GH_STUB_READY_MERGEABLE:-MERGEABLE}" "${GH_STUB_READY_MSS:-CLEAN}" ;;
  list:schemas) printf '%s\n' "${GH_STUB_SCHEMAS_READY:-}" ;;
  comment-post) echo "https://github.com/qontinui/qontinui-runner/pull/77#issuecomment-2" ;;
  pr-close)    echo "closed" ;;
  view:isDraft) printf '%s\n' "${GH_STUB_ISDRAFT:-true}" ;;
  view:mergeable,mergeStateStatus) printf '%s\n' "${GH_STUB_MERGE:-MERGEABLE CLEAN}" ;;
  view:id)     echo "PR_kwDOtest77" ;;
  view:url)    echo "https://github.com/qontinui/qontinui-runner/pull/77" ;;
  edit|reopen) : ;;
  create)      echo "https://github.com/qontinui/qontinui-runner/pull/77" ;;
  comment)     cat "$body_file" >> "${GH_STUB_COMMENTS:?}" ;;
  comments-read) cat "${GH_STUB_COMMENTS:?}" ;;
  graphql:convert) printf '%s\n' "${GH_STUB_CONVERT_ISDRAFT:-true}" ;;
  graphql)     printf '%s\n' "${GH_STUB_READY_RESULT_ISDRAFT:-false}" ;;
  check-runs)  echo "${GH_STUB_CHECKS:-3}" ;;
esac
exit 0
STUB

cat > "$bin/git" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "${GIT_STUB_LOG:?}"
case "${1:-}" in
  diff)      exit 1 ;;                       # staged changes: the normal bump path
  rev-parse) echo "${GH_STUB_HEAD:?}" ;;     # the pushed commit
  ls-remote)                                 # the sweep's head pin
    case "${GIT_STUB_REMOTE_HEAD:-}" in
      fail) echo "fatal: could not read from remote" >&2; exit 128 ;;
      none) : ;;
      "")   printf '%s\trefs/heads/chore/sibling-pin-bump\n' "${GH_STUB_HEAD:?}" ;;
      *)    printf '%s\trefs/heads/chore/sibling-pin-bump\n' "$GIT_STUB_REMOTE_HEAD" ;;
    esac ;;
esac
exit 0
STUB
cat > "$bin/sleep" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
chmod +x "$bin/gh" "$bin/git" "$bin/sleep"

# --- Fixture checkout ---------------------------------------------------------
repo="$work/repo"
temp="$work/runner-temp"
mkdir -p "$repo/.github" "$repo/scripts/ci" "$temp"
cp "$ready_script" "$repo/scripts/ci/ready-bot-draft-pr.sh"
printf 'qontinui/ui-bridge 1111111111111111111111111111111111111111\n' > "$repo/.github/sibling-pins.conf"
printf 'qontinui/ui-bridge 2222222222222222222222222222222222222222\n' > "$temp/sibling-pins.fresh.conf"
printf 'qontinui/ui-bridge  BEHIND  1111 -> 2222\n' > "$temp/sibling-pins.report.txt"
: > "$work/comments.txt"

GH_STUB_OPEN=""; GH_STUB_ISDRAFT=true; GH_STUB_MERGE="MERGEABLE CLEAN"
GH_STUB_CONVERT_ISDRAFT=true; GH_STUB_READY_RESULT_ISDRAFT=false

# run_step <fail-keys> — echoes the exit code; out.txt, gh.log, git.log.
run_step() {
  : > "$work/gh.log"; : > "$work/git.log"; : > "$work/summary.md"; : > "$work/token.log"
  local rc=0
  (
    cd "$repo"
    PATH="$bin:$PATH" \
    GH_STUB_LOG="$work/gh.log" GIT_STUB_LOG="$work/git.log" \
    GH_STUB_FAIL="$1" GH_STUB_OPEN="$GH_STUB_OPEN" GH_STUB_ISDRAFT="$GH_STUB_ISDRAFT" \
    GH_STUB_MERGE="$GH_STUB_MERGE" GH_STUB_CONVERT_ISDRAFT="$GH_STUB_CONVERT_ISDRAFT" \
    GH_STUB_READY_RESULT_ISDRAFT="$GH_STUB_READY_RESULT_ISDRAFT" \
    GH_STUB_HEAD="$HEAD_SHA" GH_STUB_COMMENTS="$work/comments.txt" \
    GH_STUB_TOKEN_LOG="$work/token.log" GH_STUB_CLOSED="${GH_STUB_CLOSED:-}" \
    REFRESH_SKIPPED_FOR_READY="${REFRESH_SKIPPED_FOR_READY_STUB:-}" \
    GH_TOKEN=pat-token-stub ACTIONS_TOKEN=actions-token-stub PAT_AVAILABLE=true REPO=qontinui/qontinui-runner \
    BRANCH=chore/sibling-pin-bump TITLE="chore(ci): bump sibling pins" \
    RUN_URL=https://github.com/qontinui/qontinui-runner/actions/runs/1 \
    MANIFEST=.github/sibling-pins.conf RUNNER_TEMP="$temp" GITHUB_SHA="$HEAD_SHA" \
    GITHUB_STEP_SUMMARY="$work/summary.md" \
    bash "$step"
  ) > "$work/out.txt" 2>&1 || rc=$?
  echo "$rc"
}
pushes() { count 'push --force' "$work/git.log"; }
calls() { count "^$1\$" "$work/gh.log"; }

echo ""
echo "Behavioural cases:"

# 1. No PR: created as a draft, pushed, then readied on the pushed head.
GH_STUB_OPEN=""
rc="$(run_step "")"
assert "no PR: exit code" 0 "$rc"
assert "no PR: pushed" 1 "$(pushes)"
assert "no PR: created" 1 "$(calls create)"
assert "no PR: readied" 1 "$(count '^readied #77$' "$work/out.txt")"
assert "no PR: summary names the state" 1 "$(count 'draft/ready state: `readied #77`' "$work/summary.md")"

# 1b. The in-step ready decision runs on ACTIONS_TOKEN (GITHUB_TOKEN, which
#     the job's permissions block grants checks: read), never on the PAT.
assert "ready script's gh calls all ran on ACTIONS_TOKEN" 0 "$(grep -E '^(list:ready|check-runs|graphql) ' "$work/token.log" | grep -vc ' actions-token-stub$' || true)"
assert "ready script's gh calls were made at all" 1 "$( { grep -E '^graphql actions-token-stub$' "$work/token.log" || true; } | wc -l | tr -d ' ')"

# 1c. No open PR, but a CLOSED unmerged one (coord ff-lands close PRs with
#     mergedAt null, so it may be a LANDED one). A fresh draft PR is opened;
#     a closed PR is never looked up, let alone reopened. The stub DOES
#     answer a closed lookup (PR 55), so a reintroduced reopen path would
#     find something to reopen and fail the assertions below.
GH_STUB_OPEN=""; GH_STUB_CLOSED=55
rc="$(run_step "")"
GH_STUB_CLOSED=""
assert "closed-only history: exit code" 0 "$rc"
assert "closed-only history: closed PRs never listed" 0 "$(calls list:closed)"
assert "closed-only history: never reopened" 0 "$(calls reopen)"
assert "closed-only history: fresh PR created" 1 "$(calls create)"
assert "no 'gh pr reopen' in executable code" 0 "$(count 'gh pr reopen' "$code_only")"
assert "no '--state closed' lookup in executable code" 0 "$(count 'state[= ]closed' "$code_only")"

# 2. Existing DRAFT PR: force-pushed, edited, readied, as before.
GH_STUB_OPEN=77; GH_STUB_ISDRAFT=true
rc="$(run_step "")"
assert "draft PR: exit code" 0 "$rc"
assert "draft PR: pushed" 1 "$(pushes)"
assert "draft PR: edited" 1 "$(calls edit)"
assert "draft PR: mergeability not even read" 0 "$(calls 'view:mergeable,mergeStateStatus')"
assert "draft PR: readied" 1 "$(count '^readied #77$' "$work/out.txt")"

# 3. Ready + mergeable: comment only, deduped; never pushed.
GH_STUB_ISDRAFT=false; GH_STUB_MERGE="MERGEABLE CLEAN"
: > "$work/comments.txt"
rc1="$(run_step "")"; p1="$(pushes)"
rc2="$(run_step "")"; p2="$(pushes)"
assert "ready+mergeable: exit codes" "0 0" "$rc1 $rc2"
assert "ready+mergeable: never pushed" "0 0" "$p1 $p2"
assert "ready+mergeable: exactly one comment across two runs" 1 "$(count 'sibling-pin-drift:' "$work/comments.txt")"
assert "ready+mergeable: not converted" 0 "$(calls graphql:convert)"

# 4. Ready + CONFLICTING (or DIRTY): converted back to a draft, rebuilt,
#    pushed, edited and re-readied — never commented on.
for shape in "CONFLICTING DIRTY" "MERGEABLE DIRTY"; do
  GH_STUB_MERGE="$shape"
  : > "$work/comments.txt"
  rc="$(run_step "")"
  assert "ready+conflicting ($shape): exit code" 0 "$rc"
  assert "ready+conflicting ($shape): converted to draft" 1 "$(calls graphql:convert)"
  assert "ready+conflicting ($shape): force-pushed" 1 "$(pushes)"
  assert "ready+conflicting ($shape): edited" 1 "$(calls edit)"
  assert "ready+conflicting ($shape): no comment" 0 "$(calls comment)"
  assert "ready+conflicting ($shape): re-readied" 1 "$(count '^readied #77$' "$work/out.txt")"
done

# 4b. N1 — decided ONCE. The schemas step saw a ready PR (mergeability not yet
#     computed) and skipped the Cargo.lock refresh a moving schemas pin needs;
#     the PR step then reads CONFLICTING. Converting now would push a
#     lock-stale bump, so it must comment instead.
GH_STUB_MERGE="CONFLICTING DIRTY"
REFRESH_SKIPPED_FOR_READY_STUB=true
: > "$work/comments.txt"
rc="$(run_step "")"
REFRESH_SKIPPED_FOR_READY_STUB=""
assert "refresh skipped + conflicting: exit code" 0 "$rc"
assert "refresh skipped + conflicting: not converted" 0 "$(calls graphql:convert)"
assert "refresh skipped + conflicting: never pushed" 0 "$(pushes)"
assert "refresh skipped + conflicting: comment instead" 1 "$(calls comment)"

# 5. Ready + UNKNOWN: comment only — never force-push on an unknown.
GH_STUB_MERGE="UNKNOWN UNKNOWN"
: > "$work/comments.txt"
rc="$(run_step "")"
assert "ready+UNKNOWN: exit code" 0 "$rc"
assert "ready+UNKNOWN: never pushed" 0 "$(pushes)"
assert "ready+UNKNOWN: comment only" 1 "$(calls comment)"
assert "ready+UNKNOWN: not converted" 0 "$(calls graphql:convert)"

# 6. Failures before any push are reds that push nothing.
GH_STUB_MERGE="CONFLICTING DIRTY"; GH_STUB_CONVERT_ISDRAFT=false
rc="$(run_step "")"
GH_STUB_CONVERT_ISDRAFT=true
assert "convert returns isDraft=false: exit code" 1 "$rc"
assert "convert returns isDraft=false: never pushed" 0 "$(pushes)"
rc="$(run_step "graphql:convert")"
assert "convert refused: exit code" 1 "$rc"
assert "convert refused: never pushed" 0 "$(pushes)"
rc="$(run_step "view:mergeable,mergeStateStatus")"
assert "mergeability unreadable: exit code" 1 "$rc"
assert "mergeability unreadable: never pushed" 0 "$(pushes)"

# 7. The ready hand-off failing after the push is a red through the reporter.
GH_STUB_OPEN=77; GH_STUB_ISDRAFT=true; GH_STUB_READY_RESULT_ISDRAFT=true
rc="$(run_step "")"
GH_STUB_READY_RESULT_ISDRAFT=false
assert "ready hand-off fails: exit code" 1 "$rc"
assert "ready hand-off fails: reporter names it" 1 "$(count 'Marking the bump PR ready for review' "$work/out.txt")"

# =============================================================================
# The schemas step: it alone decides whether a lock refresh was skipped.
# =============================================================================
extract_body() {
  awk -v name="$1" '
    state == 0 && $0 == "      - name: " name { state = 1; next }
    state == 1 && $0 ~ /^      - name: / { exit }
    state == 1 && $0 == "        run: |" { state = 2; next }
    state == 2 {
      if ($0 ~ /^[[:space:]]*$/) { print ""; next }
      if ($0 !~ /^          /) { exit }
      print substr($0, 11)
    }
  ' "$workflow" > "$2"
  [ "$(grep -c 'set -euo pipefail' "$2" || true)" -ge 1 ] || { echo "::error::step '$1' did not extract"; exit 1; }
  bash -n "$2"
}
schemas_body="$work/schemas.sh"
extract_body "Detect a moving qontinui-schemas pin" "$schemas_body"
sfx="$work/schemas-fx"
mkdir -p "$sfx/.github"
OLD_SCHEMAS="3333333333333333333333333333333333333333"
NEW_SCHEMAS="4444444444444444444444444444444444444444"
# run_schemas <ready-pr-number-or-empty> <fresh schemas sha>
run_schemas() {
  printf 'qontinui/qontinui-schemas %s
' "$OLD_SCHEMAS" > "$sfx/.github/sibling-pins.conf"
  printf 'qontinui/qontinui-schemas %s
' "$2" > "$temp/sibling-pins.fresh.conf"
  : > "$work/schemas.out"; : > "$work/gh.log"
  local rc=0
  (
    cd "$sfx"
    PATH="$bin:$PATH" GH_STUB_LOG="$work/gh.log" GH_STUB_SCHEMAS_READY="$1" GH_STUB_HEAD="$HEAD_SHA" \
    MANIFEST=.github/sibling-pins.conf GH_TOKEN=stub REPO=qontinui/qontinui-runner \
    BRANCH=chore/sibling-pin-bump RUNNER_TEMP="$temp" GITHUB_OUTPUT="$work/schemas.out" \
    bash "$schemas_body"
  ) > "$work/schemas.log" 2>&1 || rc=$?
  echo "$rc"
}
sout() { sed -n "s/^$1=//p" "$work/schemas.out"; }
echo ""
echo "Schemas step cases:"
rc="$(run_schemas 77 "$NEW_SCHEMAS")"
assert "ready PR + schemas moves: exit code" 0 "$rc"
assert "ready PR + schemas moves: refresh skipped" "false" "$(sout moved)"
assert "ready PR + schemas moves: refresh_skipped_for_ready=true" "true" "$(sout refresh_skipped_for_ready)"
rc="$(run_schemas 77 "$OLD_SCHEMAS")"
assert "ready PR + schemas unchanged: refresh_skipped_for_ready=false" "false" "$(sout refresh_skipped_for_ready)"
rc="$(run_schemas "" "$NEW_SCHEMAS")"
assert "no ready PR + schemas moves: moved=true" "true" "$(sout moved)"
assert "no ready PR + schemas moves: refresh_skipped_for_ready=false" "false" "$(sout refresh_skipped_for_ready)"
schemas_code="$work/schemas.code.sh"
{ grep -vE '^[[:space:]]*#' "$schemas_body" || true; } > "$schemas_code"
assert "schemas probe: a conflicting ready PR is NOT skipped" 1 "$(count 'select\(.isDraft == false and .isCrossRepository == false and .mergeable != "CONFLICTING" and .mergeStateStatus != "DIRTY"\)' "$schemas_code")"
printf 'qontinui/ui-bridge 2222222222222222222222222222222222222222\n' > "$temp/sibling-pins.fresh.conf"

# =============================================================================
# The sweep step: pinned head, and --close-if-obsolete only on an explicit
# no-move night.
# =============================================================================
sweep_body="$work/sweep.sh"
extract_body "Sweep the bump PR (ready it once it carries checks; page if parked)" "$sweep_body"
# run_sweep <DRIFT value> — echoes the exit code; the ready lookup reports a
# READY PR that conflicts with main unless the caller overrides.
run_sweep() {
  : > "$work/gh.log"; : > "$work/summary.md"
  local rc=0
  (
    cd "$repo"
    PATH="$bin:$PATH" GH_STUB_LOG="$work/gh.log" GIT_STUB_LOG="$work/git.log" \
    GH_STUB_HEAD="$HEAD_SHA" GH_STUB_COMMENTS="$work/comments.txt" \
    GH_STUB_READY_ISDRAFT="${SW_ISDRAFT:-false}" GH_STUB_READY_MERGEABLE="${SW_MERGEABLE:-CONFLICTING}" \
    GH_STUB_READY_MSS="${SW_MSS:-DIRTY}" GIT_STUB_REMOTE_HEAD="${SW_REMOTE_HEAD:-}" \
    GH_TOKEN=stub REPO=qontinui/qontinui-runner BRANCH=chore/sibling-pin-bump DRIFT="$1" \
    RUNNER_TEMP="$temp" GITHUB_STEP_SUMMARY="$work/summary.md" \
    bash "$sweep_body"
  ) > "$work/sweep.log" 2>&1 || rc=$?
  echo "$rc"
}
echo ""
echo "Sweep step cases:"
: > "$work/comments.txt"
rc="$(run_sweep false)"
assert "no-move night + conflicting PR: exit code" 0 "$rc"
assert "no-move night + conflicting PR: sweep passes the flag -> closed" "closed-obsolete #77" "$(tail -n1 "$work/sweep.log")"
rc="$(run_sweep true)"
assert "move night + conflicting PR: no flag -> not closed" "0 already-ready #77" "$(calls pr-close) $(tail -n1 "$work/sweep.log")"
rc="$(run_sweep "")"
assert "empty drift output + conflicting PR: no flag -> not closed" "0 already-ready #77" "$(calls pr-close) $(tail -n1 "$work/sweep.log")"
SW_ISDRAFT=true; SW_MERGEABLE=MERGEABLE; SW_MSS=CLEAN
# A no-move night closes a MERGEABLE bump PR too: its pins are obsolete.
rc="$(run_sweep false)"
assert "no-move night + mergeable draft: closed, never readied" "closed-obsolete #77 0" "$(tail -n1 "$work/sweep.log") $(calls graphql)"
SW_REMOTE_HEAD="c69f445a5c69f445a5c69f445a5c69f445a5c69f"
rc="$(run_sweep true)"
assert "sweep, stale head: head-mismatch, not readied" "head-mismatch #77 0" "$(tail -n1 "$work/sweep.log") $(calls graphql)"
SW_REMOTE_HEAD=""
rc="$(run_sweep true)"
assert "sweep, pinned head matches: readied" "readied #77" "$(tail -n1 "$work/sweep.log")"
SW_REMOTE_HEAD="fail"
rc="$(run_sweep true)"
SW_REMOTE_HEAD=""
assert "sweep, ls-remote refused: red, undecided" "1 0" "$rc $(calls graphql)"
SW_ISDRAFT=""; SW_MERGEABLE=""; SW_MSS=""

# --- Mutation proofs ----------------------------------------------------------
if [ -z "${PIN_BUMP_STEP_MUTANT:-}" ]; then
  echo ""
  echo "Mutation proofs (each broken workflow must be caught by its named case):"
  mutate() {
    local name="$1" prog="$2" want="$3" mutant="$work/mutant-$1.yml" changed rc=0
    awk "$prog" "$real_workflow" > "$mutant"
    changed="$( { diff "$real_workflow" "$mutant" || true; } | { grep -c '^>' || true; } )"
    if [ "$changed" != "1" ]; then
      assert "mutant '$name': changed exactly one line" 1 "$changed"
      return
    fi
    PIN_BUMP_STEP_MUTANT=1 PIN_BUMP_WORKFLOW_UNDER_TEST="$mutant" \
      bash "${BASH_SOURCE[0]}" > "$work/mutant-$name.out" 2>&1 || rc=$?
    assert "mutant '$name': suite exits non-zero" 1 "$([ "$rc" -ne 0 ] && echo 1 || echo 0)"
    assert "mutant '$name': caught by '$want'" 1 \
      "$( { grep -F "FAIL  $want" "$work/mutant-$name.out" || true; } | head -n1 | wc -l | tr -d ' ')"
  }
  mutate conflict-check-removed \
    '$0 == "              if [ \"$mergeable\" = \"CONFLICTING\" ] || [ \"${merge_status:-}\" = \"DIRTY\" ]; then" { $0 = "              if false; then" } { print }' \
    "ready+conflicting (CONFLICTING DIRTY): force-pushed"
  mutate ready-guard-removed \
    '$0 == "            if [ \"$existing_draft\" != \"true\" ]; then" { $0 = "            if false; then" } { print }' \
    "ready+mergeable: never pushed"
  mutate sweep-success-only \
    '/^        if: \$\{\{ !cancelled\(\) && steps\.scan\.outputs\.sweep == / { sub(/\$\{\{ !cancelled\(\) && /, ""); sub(/ \}\}$/, "") } { print }' \
    "sweep if: runs under !cancelled()"
  mutate refresh-skip-ignored \
    '$0 == "                if [ \"${REFRESH_SKIPPED_FOR_READY:-}\" = \"true\" ]; then" { $0 = "                if false; then" } { print }' \
    "refresh skipped + conflicting: never pushed"
  mutate actions-token-dropped \
    '{ sub(/if ! GH_TOKEN="\$ACTIONS_TOKEN" bash /, "if ! bash ") } { print }' \
    "ready script's gh calls all ran on ACTIONS_TOKEN"
  mutate close-flag-unconditional \
    '$0 == "          if [ \"$DRIFT\" = \"false\" ]; then" { $0 = "          if true; then" } { print }' \
    "move night + conflicting PR: no flag -> not closed"
  mutate closed-lookup-reintroduced \
    '$0 == "          if [ -n \"$existing_num\" ]; then" && !done { print "          [ -n \"$existing_num\" ] || existing_num=\"$(gh pr list --repo \"$REPO\" --head \"$BRANCH\" --base main --state closed --json number --jq .)\""; done = 1 } { print }' \
    "closed-only history: closed PRs never listed"
  mutate expect-head-dropped \
    '{ sub(/ --expect-head "\$pushed_sha"/, "") } { print }' \
    "ready call pins the pushed head"
fi

echo ""
if [ "$failures" -ne 0 ]; then
  echo "FAILED: $failures assertion(s)"
  exit 1
fi
echo "All assertions passed."
