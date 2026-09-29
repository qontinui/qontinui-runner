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
      *isDraft*) key="list:ready" ;;
      *)         key="list:$state" ;;
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
      *"/comments"*)   [ "$method" = "GET" ] && key="comments-read" ;;
    esac ;;
esac
[ -n "$key" ] || { echo "gh stub: unhandled invocation '$*'" >&2; exit 97; }
echo "$key" >> "${GH_STUB_LOG:?}"
case " ${GH_STUB_FAIL:-} " in
  *" $key "*) echo "gh: HTTP 403 ($key)" >&2; exit 1 ;;
esac
case "$key" in
  list:open)   printf '%s\n' "${GH_STUB_OPEN:-}" ;;
  list:closed) : ;;
  list:ready)
    printf '77\t%s\t%s\tPR_kwDOtest77\t2026-09-29T00:00:00Z\thttps://github.com/qontinui/qontinui-runner/pull/77\tjspinak\n' \
      "${GH_STUB_READY_ISDRAFT:-true}" "${GH_STUB_HEAD:?}" ;;
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
  : > "$work/gh.log"; : > "$work/git.log"; : > "$work/summary.md"
  local rc=0
  (
    cd "$repo"
    PATH="$bin:$PATH" \
    GH_STUB_LOG="$work/gh.log" GIT_STUB_LOG="$work/git.log" \
    GH_STUB_FAIL="$1" GH_STUB_OPEN="$GH_STUB_OPEN" GH_STUB_ISDRAFT="$GH_STUB_ISDRAFT" \
    GH_STUB_MERGE="$GH_STUB_MERGE" GH_STUB_CONVERT_ISDRAFT="$GH_STUB_CONVERT_ISDRAFT" \
    GH_STUB_READY_RESULT_ISDRAFT="$GH_STUB_READY_RESULT_ISDRAFT" \
    GH_STUB_HEAD="$HEAD_SHA" GH_STUB_COMMENTS="$work/comments.txt" \
    GH_TOKEN=stub PAT_AVAILABLE=true REPO=qontinui/qontinui-runner \
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
