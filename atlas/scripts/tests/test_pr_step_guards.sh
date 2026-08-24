#!/usr/bin/env bash
# Regression test for the `Open or update the exclude.txt refresh PR` step of
# .github/workflows/atlas-exclude-fresh.yml, against a STUBBED `gh` and `git`.
# No network, no database, no real repository.
#
# WHY THIS FILE EXISTS. That step is the workflow's self-heal remediation: when
# a qontinui-web migration adds a project/coord table, the step is what turns
# the regenerated exclude.txt into a pull request a human can land. It shipped
# 2026-08-08 and its success path was NEVER EXERCISED before it was trusted --
# its first six nights were no-drift no-ops, and the very first night it ran for
# real (2026-08-15) it failed, then failed on eight more consecutive nights,
# always on the same unguarded `gh pr reopen`. An unexercised auto-remediation
# is indistinguishable from no remediation, and it is WORSE than a plain
# failure, because it convinces the author the void is closed.
#
# The two properties this file pins, both of which were violated in production:
#
#   1. A failing `gh pr *` call must exit NON-ZERO *and* print the actionable
#      remediation. Nine nights of logs said only
#      `Resource not accessible by personal access token` because `set -e`
#      killed the step before the ::error:: block could run. Fail loudly is
#      right; fail illegibly is the bug.
#
#   2. The step must NOT go green while delivering nothing. `|| true` on any of
#      these calls would produce exactly that -- a green nightly with real drift
#      undelivered -- which is strictly worse than today's loud red. Repairing a
#      silent-failure bug by reintroducing silent success is this fleet's
#      most-repeated regression, so the negative is asserted here in CI rather
#      than left to inspection.
#
# HOW IT TESTS THE SHIPPED BYTES. The step body is EXTRACTED from the workflow
# YAML at run time rather than copied here. A copy would drift from the file CI
# actually executes, and a test of a drifted copy is worse than no test. The
# extraction is deliberately brittle-and-loud: if the step is renamed, or its
# indentation changes, extraction yields nothing and this test FAILS rather than
# silently asserting over an empty string.
#
# Run locally:
#   bash atlas/scripts/tests/test_pr_step_guards.sh

set -euo pipefail

tests_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$tests_dir/../../.." && pwd)"
workflow="$repo_root/.github/workflows/atlas-exclude-fresh.yml"

if [ ! -f "$workflow" ]; then
  echo "::error::cannot find $workflow"
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0
assert() {
  # assert <name> <expected> <actual>
  if [ "$2" = "$3" ]; then
    printf '  PASS  %-58s %s\n' "$1" "$3"
  else
    printf '  FAIL  %-58s expected %s, got %s\n' "$1" "$2" "$3"
    failures=$((failures + 1))
  fi
}

# --- Extract the step body verbatim from the workflow -------------------------
# The `run: |` block scalar is indented 10 spaces inside a step at 6. Take every
# line at >= 10 spaces (blank lines included) until the first non-blank line
# that is shallower, then strip the 10-space prefix.
step="$work/step.sh"
awk '
  state == 0 && $0 == "      - name: Open or update the exclude.txt refresh PR" { state = 1; next }
  state == 1 && $0 == "        run: |" { state = 2; next }
  state == 2 {
    if ($0 ~ /^[[:space:]]*$/) { print ""; next }
    if ($0 !~ /^          /) { exit }
    print substr($0, 11)
  }
' "$workflow" > "$step"

# Pin BOTH ends of the extraction, not just its length. A line floor alone
# would let a truncated body through, and every assertion below would then be
# describing a program CI never runs.
body_lines="$(wc -l < "$step" | tr -d ' ')"
first_line="$(grep -m1 -vE '^[[:space:]]*(#|$)' "$step" || true)"
# `|| true` on BOTH, and for the same reason: when extraction yields NOTHING
# -- the exact case this block exists to report -- `grep -v` exits 1, and
# under `pipefail` the assignment would fail and `set -e` would kill the
# script BEFORE the ::error:: block below could print. That is 'fails loudly
# but illegibly', i.e. the bug this whole commit removes, reappearing inside
# the test that guards against it. Neither `|| true` suppresses a real error:
# an empty value fails the comparison below and gets reported.
last_line="$( { grep -vE '^[[:space:]]*$' "$step" || true; } | tail -n1)"
extract_broken=0
[ "$body_lines" -ge 100 ] || extract_broken=1
[ "$first_line" = "set -euo pipefail" ] || extract_broken=1
[ "$last_line" = '} >> "$GITHUB_STEP_SUMMARY"' ] || extract_broken=1
if [ "$extract_broken" -ne 0 ]; then
  echo "::error::the refresh-PR step body did not extract cleanly from $workflow"
  echo "::error::  lines=$body_lines (want >= 100)"
  echo "::error::  first executable line=[$first_line] (want [set -euo pipefail])"
  echo "::error::  last non-blank line=[$last_line] (want [} >> \"\$GITHUB_STEP_SUMMARY\"])"
  echo "::error::The step was probably renamed, re-indented, or its tail changed."
  echo "::error::Fix the extractor above -- do NOT relax this check: asserting over a"
  echo "::error::truncated or empty body would pass vacuously, which is the exact"
  echo "::error::silent-green class this file exists to prevent."
  exit 1
fi

echo "Extracted refresh-PR step body: $body_lines lines"
bash -n "$step"

# --- The harness must supply every ambient var the step reads ---------------
# The step runs under `set -u`, so an UPPERCASE var it reads that run_step
# does not set aborts mid-body with `unbound variable`. That abort is
# invisible to the assertions -- the exit code is 1 either way on a failure
# path -- so the harness would pass locally for one reason and in Actions
# (where GitHub sets the var) for a different one, exercising the line in
# NEITHER. Measured, not eyeballed: every `$UPPER` the body reads must be in
# this list. Add to the list only after adding to run_step's env, never to
# silence this.
harness_env="$work/harness-env.txt"
# DERIVED from run_step's own env list, never a copy of it. A hand-maintained
# duplicate can only be wrong in the direction that SILENCES this guard: add a
# name to the list without adding it to run_step, and the body reads an
# unsupplied var, comm reports nothing, and the step aborts under `set -u` --
# exactly what this check exists to prevent. "Remember to update both" is a
# convention; deriving it is a mechanism. (The extra GH_STUB_*/GIT_STUB_*/PATH
# names it picks up are harmless: the step body never reads them.)
# `|| true` inside the braces, on the sed|grep half ONLY. Without it, a renamed
# or deleted run_step makes grep exit 1, pipefail propagate and `set -e` kill the
# script before the emptiness check below can say why -- the same loud-but-
# illegible shape this whole file exists to remove, and it bit here first.
{ sed -n '/^run_step() {/,/^}/p' "${BASH_SOURCE[0]}" \
  | grep -oE '^[[:space:]]+[A-Z][A-Z0-9_]*=' || true; } \
  | tr -d ' =' | sort -u > "$harness_env"
[ -s "$harness_env" ] || {
  echo "::error::could not derive run_step's env list from $0 -- was the function"
  echo "::error::renamed, or its env assignments re-indented? This check cannot be"
  echo "::error::skipped: an empty list would make every var below read as missing."
  exit 1
}
body_env="$work/body-env.txt"
# `[A-Za-z0-9_]*` rather than `[A-Z0-9_]+`: a mixed-case name would otherwise
# be truncated at the first lowercase letter and reported under a name that
# does not exist. It still starts at `[A-Z]`, so `${url:-}` and friends are
# correctly ignored.
#
# TWO SHAPES THIS GUARD CANNOT SEE, so do not use them in the step body:
#   ${!INDIRECT}  -- the `!` blocks the match
#   $((ARITH))    -- names in arithmetic context carry no `$` at all
# For those, the `set -u` abort this guard exists to prevent can still
# happen. No regex can reach them; saying so is the honest fix.
#
# Deliberately over-strict: this scans the whole body INCLUDING quoted
# heredocs, so a literal `$SOMETHING` written into the PR-body markdown would
# also be demanded of the harness. That fails loud, which is the safe
# direction.
{ grep -oE '\$\{?[A-Z][A-Za-z0-9_]*' "$step" || true; } | sed 's/[${]//g' | sort -u > "$body_env"
missing="$(comm -23 "$body_env" "$harness_env")"
if [ -n "$missing" ]; then
  echo "::error::the refresh-PR step reads environment the test harness does not set:"
  echo "$missing" | sed 's/^/::error::  /'
  echo "::error::Declare it in the step's env: block (see SERVER_URL/RUN_URL) AND add it"
  echo "::error::to run_step's env list below -- the allow-list is DERIVED from run_step,"
  echo "::error::so there is no third place to edit. Under set -u an unsupplied var aborts"
  echo "::error::the step body mid-run, which no assertion below can see."
  exit 1
fi

# --- Static assertions over the shipped bytes ---------------------------------
echo ""
echo "Static properties of the shipped step body:"

# No suppression anywhere in executable code. Comments may (and do) DISCUSS
# `|| true`; only real code counts.
code_only="$work/step.code.sh"
grep -vE '^[[:space:]]*#' "$step" > "$code_only" || true
assert "no '|| true' in executable code" 0 "$(grep -c '|| true' "$code_only" || true)"
assert "no '|| :' in executable code" 0 "$(grep -c '||[[:space:]]*:' "$code_only" || true)"
assert "no '2>/dev/null' swallowing gh errors" 0 "$(grep -c '2>/dev/null' "$code_only" || true)"

# The reopen path is gone and must stay gone (it latched onto human-authored
# #896 and killed the step for nine consecutive nights).
assert "no 'gh pr reopen' in executable code" 0 "$(grep -c 'gh pr reopen' "$code_only" || true)"
assert "no '--state closed' lookup" 0 "$(grep -cE 'state[= ]closed' "$code_only" || true)"

# Every gh call except the deliberately-bare open-PR lookup is guarded.
# -o, not -c: `grep -c` counts LINES, so two calls on one line would read as
# one and the guarded/unguarded arithmetic below would be wrong.
#
# The braces put `|| true` on `grep` ALONE, not on the whole pipeline, so a
# `wc` failure still surfaces. Without it, a body containing NO `gh pr ` at
# all -- exactly the regression this assertion exists to report -- makes
# grep exit 1, pipefail propagate, and `set -e` kill the script before the
# assert can print 'expected 7, got 0'. Loud but illegible again.
gh_calls="$( { grep -o 'gh pr ' "$code_only" || true; } | wc -l | tr -d ' ')"
guarded="$(grep -cE 'if ! (url=|existing_draft=)?"?\$?\(?gh pr |if ! gh pr ' "$code_only" || true)"
assert "gh calls in executable code" 7 "$gh_calls"
assert "guarded gh calls (all but the bare open lookup)" 6 "$guarded"

# --- Stub bin -----------------------------------------------------------------
bin="$work/bin"
mkdir -p "$bin"

cat > "$bin/gh" <<'STUB'
#!/usr/bin/env bash
# Minimal `gh pr <verb>` stub. Keys on the verb plus, for `view`, the --json
# selector, because the step makes three semantically different `pr view` calls.
# GH_STUB_FAIL is a space-separated list of keys that must fail like the real
# PAT does. Everything the step passes --jq is returned PRE-FILTERED, since --jq
# is gh's own flag and never reaches a real jq here.
prev=""
json=""
for a in "$@"; do
  if [ "$prev" = "--json" ]; then json="$a"; fi
  prev="$a"
done
verb="${2:-}"
key="$verb"
if [ "$verb" = "view" ]; then key="view:$json"; fi
echo "$key" >> "${GH_STUB_LOG:-/dev/null}"
case " ${GH_STUB_FAIL:-} " in
  *" $key "*)
    echo "gh: Resource not accessible by personal access token ($key)" >&2
    exit 1
    ;;
esac
case "$key" in
  list)          printf '%s\n' "${GH_STUB_LIST_OPEN:-}" ;;
  view:isDraft)  printf '%s\n' "${GH_STUB_ISDRAFT:-true}" ;;
  view:comments) printf '%s' "${GH_STUB_COMMENTS:-}" ;;
  # GH_STUB_EMPTY_URL models a gh that exits 0 having printed nothing.
  view:url)      [ -n "${GH_STUB_EMPTY_URL:-}" ] || printf '%s\n' "${GH_STUB_URL:?}" ;;
  create)        [ -n "${GH_STUB_EMPTY_URL:-}" ] || printf '%s\n' "${GH_STUB_URL:?}" ;;
  edit)          echo "edited" ;;
  comment)       echo "commented" ;;
  *)
    echo "gh stub: unhandled invocation '$*'" >&2
    exit 97
    ;;
esac
exit 0
STUB

cat > "$bin/git" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "${GIT_STUB_LOG:-/dev/null}"
# `git diff --cached --quiet` is the step's contradiction guard: exit 1 means
# there ARE staged changes, i.e. the normal drift path.
if [ "${1:-}" = "diff" ]; then
  if [ "${GIT_STUB_STAGED:-1}" = "1" ]; then exit 1; fi
  exit 0
fi
exit 0
STUB

chmod +x "$bin/gh" "$bin/git"

# --- Fixture inputs the step reads --------------------------------------------
runner_temp="$work/runner-temp"
mkdir -p "$runner_temp" "$work/repo/atlas"
printf 'coord.a\ncoord.b\ncoord.session_policy_reads\n' > "$runner_temp/exclude.fresh.txt"
printf '  + (fresh only)     coord.session_policy_reads\n' > "$runner_temp/exclude.drift.txt"
drift_sha="$(sha256sum "$runner_temp/exclude.drift.txt" | cut -c1-16)"

STUB_URL="https://github.com/qontinui/qontinui-runner/pull/4242"

# Pin the stub knobs rather than inheriting them. run_step forwards these to
# the step, so an ambient `GH_STUB_EMPTY_URL=1` in the caller's environment
# would silently re-point a dozen assertions at a different scenario.
GH_STUB_EMPTY_URL=""
GIT_STUB_STAGED=1
export GH_STUB_EMPTY_URL GIT_STUB_STAGED

# run_step <fail-keys> <list-open> <isDraft> <comments-file-or-empty>
# Echoes the exit code; leaves stdout+stderr in $work/out.txt, the gh call log
# in $work/gh.log, the git call log in $work/git.log and the step summary in
# $work/summary.md.
run_step() {
  : > "$work/gh.log"
  : > "$work/git.log"
  : > "$work/summary.md"
  local rc=0
  (
    cd "$work/repo"
    PATH="$bin:$PATH" \
    GH_STUB_LOG="$work/gh.log" \
    GIT_STUB_LOG="$work/git.log" \
    GH_STUB_FAIL="$1" \
    GH_STUB_LIST_OPEN="$2" \
    GH_STUB_ISDRAFT="$3" \
    GH_STUB_COMMENTS="$4" \
    GH_STUB_URL="$STUB_URL" \
    GH_STUB_EMPTY_URL="${GH_STUB_EMPTY_URL:-}" \
    GH_TOKEN="stub-token" \
    PAT_AVAILABLE="true" \
    REPO="qontinui/qontinui-runner" \
    BRANCH="chore/atlas-exclude-refresh" \
    TITLE="chore(atlas): refresh exclude.txt against the current qontinui-web schema" \
    RUN_URL="https://github.com/qontinui/qontinui-runner/actions/runs/1" \
    SERVER_URL="https://github.com" \
    RUNNER_TEMP="$runner_temp" \
    GITHUB_SHA="0000000000000000000000000000000000000000" \
    GITHUB_STEP_SUMMARY="$work/summary.md" \
    bash "$step"
  ) > "$work/out.txt" 2>&1 || rc=$?
  echo "$rc"
}

# The remediation the workflow has been unable to reach since 2026-08-15. This
# exact substring is the contract: an operator reading a red run must be told
# which permission to grant.
REMEDIATION="grant the CLORINDE_AUTOCOMMIT_TOKEN PAT 'Pull requests: write'"

has() { grep -qF "$1" "$work/out.txt" && echo yes || echo no; }
log_has() { grep -qxF "$1" "$work/$2" && echo yes || echo no; }
summary_has_url() { grep -qF "$STUB_URL" "$work/summary.md" && echo yes || echo no; }

# ---------------------------------------------------------------------------
# (i) Every guarded verb: a failure must exit 1 AND print the remediation.
# ---------------------------------------------------------------------------
echo ""
echo "Guarded gh failures are legible AND still fail (the nine-night bug):"

# create: no open PR, so the step pushes and creates.
assert "create fails => exit 1"                1 "$(run_step 'create' '' 'true' '')"
assert "create failure prints remediation"     yes "$(has "$REMEDIATION")"
assert "create failure prints option (a)"      yes "$(has "Allow GitHub Actions to create and approve pull requests")"
assert "create failure names the operation"    yes "$(has "'create' operation failed")"
assert "create failure says branch was pushed" yes "$(has "the fix is already committed there")"

# edit + post-write url read: an OPEN DRAFT PR already exists.
assert "edit fails => exit 1"                  1 "$(run_step 'edit' '1234' 'true' '')"
assert "edit failure prints remediation"       yes "$(has "$REMEDIATION")"
assert "edit failure names the operation"      yes "$(has "'edit #1234' operation failed")"

assert "view --json url fails => exit 1"       1 "$(run_step 'view:url' '1234' 'true' '')"
assert "url-read failure prints remediation"   yes "$(has "$REMEDIATION")"

# The ready-for-review branch: an OPEN NON-DRAFT PR exists, so the step must not
# touch the branch and instead comments the drift.
assert "view --json isDraft fails => exit 1"   1 "$(run_step 'view:isDraft' '1234' 'true' '')"
assert "isDraft failure prints remediation"    yes "$(has "$REMEDIATION")"

assert "view --json comments fails => exit 1"  1 "$(run_step 'view:comments' '1234' 'false' '')"
assert "comments failure prints remediation"   yes "$(has "$REMEDIATION")"

assert "comment fails => exit 1"               1 "$(run_step 'comment' '1234' 'false' '')"
assert "comment failure prints remediation"    yes "$(has "$REMEDIATION")"
assert "comment failure names the operation"   yes "$(has "'comment on #1234' operation failed")"
# Nothing was pushed on this path, so the by-hand remediation must not claim it.
assert "comment failure does not claim a push" no "$(has "the fix is already committed there")"

# The one deliberately-bare call: the open-PR lookup. Its loud failure is
# correct -- a suppressed lookup reads as "no PR exists" and would let the step
# force-push under a live merge-train candidate. It must still be non-zero.
assert "bare open-PR lookup failure => exit 1" 1 "$(run_step 'list' '' 'true' '')"

# ---------------------------------------------------------------------------
# (ii) All gh calls succeed => real delivery, never a silent green.
#      A step that exits 0 with an EMPTY summary is the silent-green signature.
# ---------------------------------------------------------------------------
echo ""
echo "Success paths deliver something (silent-green negative):"

assert "create path => exit 0"                 0 "$(run_step '' '' 'true' '')"
assert "create path wrote a PR URL to summary" yes "$(summary_has_url)"
assert "create path called gh pr create"       yes "$(log_has 'create' gh.log)"
assert "create path did NOT reopen anything"   no  "$(log_has 'reopen' gh.log)"

assert "edit path => exit 0"                   0 "$(run_step '' '1234' 'true' '')"
assert "edit path wrote a PR URL to summary"   yes "$(summary_has_url)"
assert "edit path called gh pr edit"           yes "$(log_has 'edit' gh.log)"
assert "edit path did NOT call gh pr create"   no  "$(log_has 'create' gh.log)"

# Ready-for-review: exits 0 having delivered a COMMENT (not a summary URL --
# nothing was opened or updated), and must leave the branch alone.
assert "ready-for-review path => exit 0"       0 "$(run_step '' '1234' 'false' '')"
assert "ready path commented the drift"        yes "$(log_has 'comment' gh.log)"
assert "ready path did not touch the branch"   no  "$(grep -q 'push' "$work/git.log" && echo yes || echo no)"

# Dedup: the same drift must not be commented twice on the same PR.
assert "ready path, drift already commented"   0 "$(run_step '' '1234' 'false' "<!-- atlas-drift:$drift_sha -->")"
assert "dedup suppressed the second comment"   no  "$(log_has 'comment' gh.log)"

# A `gh` that exits 0 while printing nothing must NOT reach the summary block:
# a green step with an empty summary is exactly the silent-green signature
# (V6 of the plan names it by that name).
echo ""
echo "Exit 0 is not the same as 'a pull request exists':"
GH_STUB_EMPTY_URL=1
export GH_STUB_EMPTY_URL
assert "create returns no URL => exit 1"           1 "$(run_step '' '' 'true' '')"
assert "and it says the URL came back empty"       yes "$(has 'returned no URL')"
assert "and it prints the remediation"             yes "$(has "$REMEDIATION")"
assert "and it wrote NO summary section"           no  "$(grep -q . "$work/summary.md" && echo yes || echo no)"
# The EDIT arm is deliberately different: `gh pr edit` already succeeded, so
# the drift IS delivered and the URL is derivable. Reddening there would add
# a false-red class to the one workflow whose disease is unowned reds. It is
# not silent green -- the summary still carries a real PR URL -- and these
# assertions are what keep those two apart.
assert "edit path returns no URL => exit 0"        0 "$(run_step '' '1234' 'true' '')"
assert "and it warns about the empty read-back"    yes "$(has '::warning::Reading back the URL')"
assert "and it derives the canonical PR link"      yes "$(has '/qontinui/qontinui-runner/pull/1234')"
assert "and the summary carries that link"         yes "$(grep -qF '/qontinui/qontinui-runner/pull/1234' "$work/summary.md" && echo yes || echo no)"
assert "edit arm does NOT say open one by hand"    no  "$(has 'the fix is already committed there')"
assert "edit arm does not emit an ::error::"       no  "$(has '::error::')"
GH_STUB_EMPTY_URL=""

# ---------------------------------------------------------------------------
# The contradiction guard must stay non-silent.
# ---------------------------------------------------------------------------
echo ""
echo "Contradiction guard:"
GIT_STUB_STAGED=0
assert "drift reported but nothing staged => 1" 1 "$(run_step '' '' 'true' '')"
assert "and it says so"                         yes "$(has "already matches the fresh list")"
GIT_STUB_STAGED=1

echo ""
if [ "$failures" -gt 0 ]; then
  echo "::error::refresh-PR step guard test: $failures failure(s)."
  exit 1
fi
echo "refresh-PR step guard test: all assertions passed."
exit 0
