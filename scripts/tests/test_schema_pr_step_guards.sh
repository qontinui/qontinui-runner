#!/usr/bin/env bash
# Regression test for the `Open or update the schema refresh PR` step of
# .github/workflows/schema-pg-sql-freshness-nightly.yml, against a STUBBED `gh`
# and `git`. No network, no database, no real repository.
#
# WHY THIS FILE EXISTS. That step is the nightly's self-heal remediation: when a
# qontinui-web alembic migration moves the schema, it is what turns the
# regenerated dump into a pull request a human can land. Its sibling —
# atlas-exclude-fresh.yml's identical step — shipped with its success path never
# once exercised, and the first night it ran for real it failed, then failed on
# eight more consecutive nights, always on the same unguarded `gh pr` call. An
# unexercised auto-remediation is indistinguishable from no remediation, and it
# is WORSE than a plain failure, because it convinces the author the void is
# closed. This step is born with the test the sibling acquired the hard way.
#
# The two properties this file pins:
#
#   1. A failing `gh pr *` call must exit NON-ZERO *and* print the actionable
#      remediation — and the RIGHT one. `create` is blocked on a permission this
#      repo does not currently grant, while every other verb rides the job's
#      `pull-requests: write`; telling an operator to re-scope the PAT for an
#      `edit` failure sends them to fix a token that call never used.
#
#   2. The step must NOT go green while delivering nothing. A `|| true` on any
#      of these calls would produce exactly that — a green nightly with real
#      drift undelivered — which is strictly worse than a loud red. Repairing a
#      silent-failure bug by reintroducing silent success is this fleet's
#      most-repeated regression, so the negative is asserted here in CI rather
#      than left to inspection.
#
# HOW IT TESTS THE SHIPPED BYTES. The step body is EXTRACTED from the workflow
# YAML at run time rather than copied here. A copy would drift from the file CI
# actually executes, and a test of a drifted copy is worse than no test. The
# extraction is deliberately brittle-and-loud: if the step is renamed or
# re-indented, extraction yields nothing and this test FAILS rather than
# silently asserting over an empty string.
#
# Plan: plans/2026-08-07-runner-schema-freshness-cross-repo-blind-spot.md
#
# Run locally:
#   bash scripts/tests/test_schema_pr_step_guards.sh

set -euo pipefail

tests_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$tests_dir/../.." && pwd)"
workflow="$repo_root/.github/workflows/schema-pg-sql-freshness-nightly.yml"

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
  state == 0 && $0 == "      - name: Open or update the schema refresh PR" { state = 1; next }
  state == 1 && $0 == "        run: |" { state = 2; next }
  state == 2 {
    if ($0 ~ /^[[:space:]]*$/) { print ""; next }
    if ($0 !~ /^          /) { exit }
    print substr($0, 11)
  }
' "$workflow" > "$step"

# Pin BOTH ends of the extraction, not just its length. A line floor alone would
# let a truncated body through, and every assertion below would then be
# describing a program CI never runs.
body_lines="$(wc -l < "$step" | tr -d ' ')"
# `|| true` on both greps, and for the same reason: when extraction yields
# NOTHING — the exact case this block exists to report — `grep -v` exits 1, and
# under `pipefail` the assignment would fail and `set -e` would kill the script
# BEFORE the ::error:: block below could print. That is "fails loudly but
# illegibly", i.e. the bug this whole file guards against, reappearing inside
# the guard. Neither `|| true` suppresses a real error: an empty value fails the
# comparison below and gets reported.
first_line="$( { grep -m1 -vE '^[[:space:]]*(#|$)' "$step" || true; } )"
last_line="$( { grep -vE '^[[:space:]]*$' "$step" || true; } | tail -n1)"
extract_broken=0
[ "$body_lines" -ge 60 ] || extract_broken=1
[ "$first_line" = "set -euo pipefail" ] || extract_broken=1
[ "$last_line" = '} >> "$GITHUB_STEP_SUMMARY"' ] || extract_broken=1
if [ "$extract_broken" -ne 0 ]; then
  echo "::error::the refresh-PR step body did not extract cleanly from $workflow"
  echo "::error::  lines=$body_lines (want >= 60)"
  echo "::error::  first executable line=[$first_line] (want [set -euo pipefail])"
  echo "::error::  last non-blank line=[$last_line] (want [} >> \"\$GITHUB_STEP_SUMMARY\"])"
  echo "::error::The step was probably renamed, re-indented, or its tail changed."
  echo "::error::Fix the extractor above — do NOT relax this check: asserting over a"
  echo "::error::truncated or empty body would pass vacuously, which is the exact"
  echo "::error::silent-green class this file exists to prevent."
  exit 1
fi

echo "Extracted refresh-PR step body: $body_lines lines"
bash -n "$step"

# --- The harness must supply every ambient var the step reads -----------------
# The step runs under `set -u`, so an UPPERCASE var it reads that run_step does
# not set aborts mid-body with `unbound variable`. That abort is invisible to the
# assertions — the exit code is 1 either way on a failure path — so the harness
# would pass locally for one reason and in Actions (where GitHub sets the var)
# for a different one, exercising the line in NEITHER.
#
# The allow-list is DERIVED from run_step's own env block, never a copy of it. A
# hand-maintained duplicate can only be wrong in the direction that SILENCES this
# guard.
harness_env="$work/harness-env.txt"
{ sed -n '/^run_step() {/,/^}/p' "${BASH_SOURCE[0]}" \
  | grep -oE '^[[:space:]]+[A-Z][A-Z0-9_]*=' || true; } \
  | tr -d ' =' | sort -u > "$harness_env"
[ -s "$harness_env" ] || {
  echo "::error::could not derive run_step's env list from ${BASH_SOURCE[0]} — was the"
  echo "::error::function renamed, or its env assignments re-indented? This check cannot"
  echo "::error::be skipped: an empty list makes every var below read as missing."
  exit 1
}
body_env="$work/body-env.txt"
# `[A-Za-z0-9_]*` rather than `[A-Z0-9_]+`: a mixed-case name would otherwise be
# truncated at the first lowercase letter and reported under a name that does not
# exist. It still starts at `[A-Z]`, so `${url:-}` and friends are ignored.
#
# TWO SHAPES THIS GUARD CANNOT SEE, so do not use them in the step body:
#   ${!INDIRECT}  — the `!` blocks the match
#   $((ARITH))    — names in arithmetic context carry no `$` at all
# For those the `set -u` abort can still happen. No regex reaches them; saying so
# is the honest fix.
#
# Deliberately over-strict: this scans the whole body INCLUDING quoted heredocs,
# so a literal `$SOMETHING` in the PR-body markdown would also be demanded of the
# harness. That fails loud, which is the safe direction.
{ grep -oE '\$\{?[A-Z][A-Za-z0-9_]*' "$step" || true; } | sed 's/[${]//g' | sort -u > "$body_env"
missing="$(comm -23 "$body_env" "$harness_env")"
if [ -n "$missing" ]; then
  echo "::error::the refresh-PR step reads environment the test harness does not set:"
  echo "$missing" | sed 's/^/::error::  /'
  echo "::error::Declare it in the step's env: block AND add it to run_step's env list"
  echo "::error::below — the allow-list is DERIVED from run_step, so there is no third"
  echo "::error::place to edit. Under set -u an unsupplied var aborts the step body"
  echo "::error::mid-run, which no assertion below can see."
  exit 1
fi

# --- Static assertions over the shipped bytes ---------------------------------
echo ""
echo "Static properties of the shipped step body:"

# No suppression anywhere in executable code. Comments may DISCUSS `|| true`;
# only real code counts.
code_only="$work/step.code.sh"
grep -vE '^[[:space:]]*#' "$step" > "$code_only" || true
assert "no '|| true' in executable code" 0 "$(grep -c '|| true' "$code_only" || true)"
assert "no '|| :' in executable code" 0 "$(grep -cE '\|\|[[:space:]]*:' "$code_only" || true)"
assert "no '2>/dev/null' swallowing gh errors" 0 "$(grep -c '2>/dev/null' "$code_only" || true)"

# The reopen path must never appear. coord ff-lands by rebase+close, so
# `mergedAt == null` cannot distinguish "landed" from "rejected" — reopening a
# landed PR resurrects dead work. This is the defect that killed the sibling
# workflow for nine consecutive nights.
assert "no 'gh pr reopen' in executable code" 0 "$(grep -c 'gh pr reopen' "$code_only" || true)"
assert "no '--state closed' lookup" 0 "$(grep -cE 'state[= ]closed' "$code_only" || true)"

# Every gh call except the deliberately-bare open-PR lookup is guarded.
# -o, not -c: `grep -c` counts LINES, so two calls on one line would read as one.
# INVOCATIONS, not mentions: a `gh pr ` preceded by a quote is a string literal
# (the step's own ::warning:: text), and counting it would put a phantom call in
# the unguarded column.
gh_calls="$( { grep -oE "(^|[^'\"])gh pr " "$code_only" || true; } | wc -l | tr -d ' ')"
# "Guarded" is either shape whose failure branch is handled: `if ! ...gh pr`
# (fail => remediation + exit 1) and the PAT create's positive
# `if url="$(GH_TOKEN="$PAT_TOKEN" gh pr create ...)"; then ... else` (fail =>
# ::warning:: + fall back to GITHUB_TOKEN).
guard_re='^[[:space:]]*if (! )?(url=)?"?\$?\(?(GH_TOKEN="\$PAT_TOKEN" )?gh pr '
guarded="$(grep -cE "$guard_re" "$code_only" || true)"
assert "gh calls in executable code" 5 "$gh_calls"
assert "guarded gh calls (all but the bare open lookup)" 4 "$guarded"
# The one bare call must be the open lookup and nothing else. Without this,
# swapping which call is bare (guarding `list`, un-guarding `edit`) keeps both
# counts and passes vacuously.
bare_calls="$( { grep -E "(^|[^'\"])gh pr " "$code_only" || true; } \
  | { grep -vE "$guard_re" || true; } \
  | { grep -oE 'gh pr [a-z]+' || true; } | sort -u | tr '\n' ' ')"
assert "the only bare call is the open-PR lookup" "gh pr list " "$bare_calls"

# The branch must be rebuilt from the TRIGGERING sha, never a bare HEAD, and the
# add must name its path — sibling repos share this workspace.
assert "branch is rebuilt from the triggering sha" 1 "$(grep -c 'checkout -B "\$BRANCH" "\$TRIGGER_SHA"' "$code_only" || true)"
assert "no 'git add -A' (siblings share the workspace)" 0 "$(grep -c 'git add -A' "$code_only" || true)"
assert "PRs are created as drafts" 2 "$(grep -c -- '--draft' "$code_only" || true)"

# --- Stub bin -----------------------------------------------------------------
bin="$work/bin"
mkdir -p "$bin"

cat > "$bin/gh" <<'STUB'
#!/usr/bin/env bash
# Minimal `gh pr <verb>` stub. Keys on the verb plus, for `view`, the --json
# selector. GH_STUB_FAIL is a space-separated list of keys that must fail like a
# real under-scoped token does. Everything the step passes --jq is returned
# PRE-FILTERED, since --jq is gh's own flag and never reaches a real jq here.
#
# The step's PAT arm spells its call `GH_TOKEN="$PAT_TOKEN" gh pr create ...`.
# That env prefix is consumed by bash before PATH lookup, so this stub still
# sees argv[1]=pr argv[2]=create — it keys on argv, never on argv[0]'s position.
# GH_STUB_FAIL_FIRST_CREATE models the measured production shape: the PAT create
# refused, the GITHUB_TOKEN retry accepted.
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
if [ "$key" = "create" ] && [ -n "${GH_STUB_FAIL_FIRST_CREATE:-}" ] \
   && [ "$(grep -c '^create$' "${GH_STUB_LOG:-/dev/null}")" = "1" ]; then
  echo "gh: GraphQL: Resource not accessible by personal access token (createPullRequest)" >&2
  exit 1
fi
case "$key" in
  list)      printf '%s\n' "${GH_STUB_LIST_OPEN:-}" ;;
  # GH_STUB_EMPTY_URL models a gh that exits 0 having printed nothing.
  view:url)  [ -n "${GH_STUB_EMPTY_URL:-}" ] || printf '%s\n' "${GH_STUB_URL:?}" ;;
  create)    [ -n "${GH_STUB_EMPTY_URL:-}" ] || printf '%s\n' "${GH_STUB_URL:?}" ;;
  edit)      echo "edited" ;;
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
mkdir -p "$runner_temp" "$work/repo/src-tauri"
printf 'CREATE TABLE project.a ();\nCREATE TABLE coord.b ();\n' > "$runner_temp/schema.fresh.sql"
: > "$work/repo/src-tauri/schema.pg.sql.generated"

STUB_URL="https://github.com/qontinui/qontinui-runner/pull/4242"

# Pin the stub knobs rather than inheriting them. run_step forwards these to the
# step, so an ambient `GH_STUB_EMPTY_URL=1` in the caller's environment would
# silently re-point a dozen assertions at a different scenario.
GH_STUB_EMPTY_URL=""
GH_STUB_FAIL_FIRST_CREATE=""
GIT_STUB_STAGED=1
PAT_TOKEN_STUB="stub-pat-token"
export GH_STUB_EMPTY_URL GH_STUB_FAIL_FIRST_CREATE GIT_STUB_STAGED PAT_TOKEN_STUB

# run_step <fail-keys> <list-open>
# Echoes the exit code; leaves stdout+stderr in $work/out.txt, the gh call log in
# $work/gh.log, the git call log in $work/git.log and the summary in
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
    GH_STUB_URL="$STUB_URL" \
    GH_STUB_EMPTY_URL="${GH_STUB_EMPTY_URL:-}" \
    GH_STUB_FAIL_FIRST_CREATE="${GH_STUB_FAIL_FIRST_CREATE:-}" \
    GH_TOKEN="stub-token" \
    PAT_TOKEN="${PAT_TOKEN_STUB:-}" \
    REPO="qontinui/qontinui-runner" \
    BRANCH="chore/schema-pg-sql-refresh" \
    TITLE="chore(schema): regenerate schema.pg.sql.generated" \
    RUN_URL="https://github.com/qontinui/qontinui-runner/actions/runs/1" \
    SERVER_URL="https://github.com" \
    TRIGGER_SHA="0000000000000000000000000000000000000000" \
    RUNNER_TEMP="$runner_temp" \
    GITHUB_STEP_SUMMARY="$work/summary.md" \
    bash "$step"
  ) > "$work/out.txt" 2>&1 || rc=$?
  echo "$rc"
}

# The remediations, one per token. These exact substrings are the contract: an
# operator reading a red run must be told which permission to grant, and the
# RIGHT one — the two blockers are independent and live on different tokens.
#
#   create  runs PAT-first, GITHUB_TOKEN-fallback. A refusal of both is fixed by
#           EITHER the repo's "Allow GitHub Actions to create and approve pull
#           requests" switch (a) or the PAT's `Pull requests: write` (b).
#   others  run on GITHUB_TOKEN only, under the job's `pull-requests: write`.
#           Telling that operator to re-scope the PAT would send them to fix a
#           token the call never used.
REMEDIATION_CREATE="Allow GitHub Actions to create and approve pull requests"
REMEDIATION_OTHER="that permissions block has drifted"

echo ""
echo "Behavioural cases:"

# 1. Happy path, no existing PR: create with the PAT.
rc="$(run_step "" "")"
assert "create path: exit code" 0 "$rc"
assert "create path: pushed the branch" 1 "$(grep -c 'push --force origin HEAD:refs/heads/chore/schema-pg-sql-refresh' "$work/git.log" || true)"
assert "create path: called create once" 1 "$(grep -c '^create$' "$work/gh.log" || true)"
assert "create path: no edit call" 0 "$(grep -c '^edit$' "$work/gh.log" || true)"
assert "create path: PR url in step summary" 1 "$(grep -c "$STUB_URL" "$work/summary.md" || true)"

# 2. Happy path with an existing open PR: edit, never create.
rc="$(run_step "" "77")"
assert "edit path: exit code" 0 "$rc"
assert "edit path: called edit once" 1 "$(grep -c '^edit$' "$work/gh.log" || true)"
assert "edit path: never called create" 0 "$(grep -c '^create$' "$work/gh.log" || true)"
assert "edit path: PR url in step summary" 1 "$(grep -c "$STUB_URL" "$work/summary.md" || true)"

# 3. `create` refused by BOTH tokens => non-zero AND the create remediation.
rc="$(run_step "create" "")"
assert "create refused: exit code" 1 "$rc"
assert "create refused: prints (a) repo switch" 1 "$(grep -c "$REMEDIATION_CREATE" "$work/out.txt" || true)"
assert "create refused: prints (b) PAT scope" 1 "$(grep -cF "Pull requests: write" "$work/out.txt" || true)"
assert "create refused: points at the pushed branch" 1 "$(grep -c 'already committed there' "$work/out.txt" || true)"
assert "create refused: does NOT print the other-verb text" 0 "$(grep -c "$REMEDIATION_OTHER" "$work/out.txt" || true)"

# 4. The measured production shape: PAT create refused, GITHUB_TOKEN accepted.
GH_STUB_FAIL_FIRST_CREATE=1
rc="$(run_step "" "")"
GH_STUB_FAIL_FIRST_CREATE=""
assert "PAT refused, token retry: exit code" 0 "$rc"
assert "PAT refused, token retry: two create attempts" 2 "$(grep -c '^create$' "$work/gh.log" || true)"
assert "PAT refused, token retry: warns about the retry" 1 "$(grep -c 'retrying with GITHUB_TOKEN' "$work/out.txt" || true)"

# 5. No PAT configured at all: straight to GITHUB_TOKEN, with a warning.
PAT_TOKEN_STUB=""
rc="$(run_step "" "")"
PAT_TOKEN_STUB="stub-pat-token"
assert "no PAT: exit code" 0 "$rc"
assert "no PAT: warns and uses GITHUB_TOKEN" 1 "$(grep -c 'going straight to GITHUB_TOKEN' "$work/out.txt" || true)"
assert "no PAT: exactly one create attempt" 1 "$(grep -c '^create$' "$work/gh.log" || true)"

# 6. `edit` refused => non-zero AND the OTHER-verb remediation, not create's.
rc="$(run_step "edit" "77")"
assert "edit refused: exit code" 1 "$rc"
assert "edit refused: prints the permissions-drift text" 1 "$(grep -c "$REMEDIATION_OTHER" "$work/out.txt" || true)"
assert "edit refused: does NOT print create's remediation" 0 "$(grep -c "$REMEDIATION_CREATE" "$work/out.txt" || true)"

# 7. `list` refused => the bare lookup still dies (set -e), before any push.
rc="$(run_step "list" "")"
assert "list refused: exit code" 1 "$rc"
assert "list refused: never pushed" 0 "$(grep -c 'push --force' "$work/git.log" || true)"

# 8. The contradiction guard: drift claimed but nothing staged => hard red.
GIT_STUB_STAGED=0
rc="$(run_step "" "")"
GIT_STUB_STAGED=1
assert "nothing staged: exit code" 1 "$rc"
assert "nothing staged: says it is unreachable" 1 "$(grep -c 'should be unreachable' "$work/out.txt" || true)"
assert "nothing staged: never pushed" 0 "$(grep -c 'push --force' "$work/git.log" || true)"

# 9. gh exits 0 printing no URL. The two arms differ ON PURPOSE: the edit arm
#    already succeeded so it synthesizes and stays green; the create arm has no
#    evidence a PR exists, so it is a hard red.
GH_STUB_EMPTY_URL=1
rc="$(run_step "" "77")"
assert "empty url on edit: stays green" 0 "$rc"
assert "empty url on edit: synthesizes the url" 1 "$(grep -c 'pull/77' "$work/out.txt" || true)"
rc="$(run_step "" "")"
GH_STUB_EMPTY_URL=""
assert "empty url on create: hard red" 1 "$rc"
assert "empty url on create: names the read-back" 1 "$(grep -c 'URL read-back' "$work/out.txt" || true)"

echo ""
if [ "$failures" -ne 0 ]; then
  echo "FAILED: $failures assertion(s)"
  exit 1
fi
echo "All assertions passed."
