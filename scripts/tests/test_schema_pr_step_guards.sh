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
# SINCE plan 2026-09-17-atlas-self-heal-files-its-own-fix-as-a-draft-and-nothing-lands-it
# this file also pins what happens AROUND that step, by running the shipped
# bytes of two more steps and evaluating the shipped `if:` conditions:
#
#   3. The PR step hands its draft to scripts/ci/ready-bot-draft-pr.sh, and a
#      failure there is a red through pr_op_failed — never a silent green.
#
#   4. The `Sweep the refresh PR` step runs on EVERY self-heal run, drift or
#      not, while the PR step runs only on drift. A no-drift night is exactly
#      when a parked draft needs re-deciding, so gating the sweep on drift
#      would re-create the park this plan removes.
#
#   5. `fresh_check` publishes `healed=true` EXACTLY when drift=true and
#      self_heal=true, so "green because healed" is distinguishable from
#      "green because fresh" without parsing prose.
#
# Properties 4 and 5 carry MUTATION PROOFS: the last section re-runs this whole
# file against deliberately broken copies of the workflow and asserts each is
# caught by the case that guards it (the child sets SCHEMA_STEP_MUTANT=1, which
# skips that section). The `if:` evaluator below is strict — any expression
# shape it does not model is a loud failure, never a guess.
#
# Plan: plans/2026-08-07-runner-schema-freshness-cross-repo-blind-spot.md
#
# Run locally:
#   bash scripts/tests/test_schema_pr_step_guards.sh

set -euo pipefail

tests_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$tests_dir/../.." && pwd)"
real_workflow="$repo_root/.github/workflows/schema-pg-sql-freshness-nightly.yml"
# Overridable ONLY so the mutation-proof section can point a child run at a
# broken copy. Never set it by hand to make this pass.
workflow="${SCHEMA_WORKFLOW_UNDER_TEST:-$real_workflow}"
ready_script="$repo_root/scripts/ci/ready-bot-draft-pr.sh"

for f in "$workflow" "$ready_script"; do
  if [ ! -f "$f" ]; then
    echo "::error::cannot find $f"
    exit 1
  fi
done

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
guard_re='^[[:space:]]*if (! )?([a-z_]+=)?"?\$?\(?(GH_TOKEN="\$PAT_TOKEN" )?gh pr '
guarded="$(grep -cE "$guard_re" "$code_only" || true)"
assert "gh calls in executable code" 9 "$gh_calls"
assert "guarded gh calls (all but the bare open lookup)" 8 "$guarded"
# The REST reads/writes (`gh api`) the ready-PR guard makes are every one of
# them inside an `if !`: an unread comment list must never read as "not yet
# commented".
gh_api_calls="$( { grep -oE "(^|[^'\"])gh api " "$code_only" || true; } | wc -l | tr -d ' ')"
gh_api_guarded="$(grep -cE '^[[:space:]]*if ! ([a-z_]+="\$\()?gh api ' "$code_only" || true)"
assert "gh api calls in executable code" 3 "$gh_api_calls"
assert "every gh api call is guarded" 3 "$gh_api_guarded"
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
# Born a draft, then handed to the shared script — with a wait, because this
# step force-pushed moments earlier and the check runs it fired take seconds to
# register — and a failure there routes through the step's reporter.
assert "PR step calls the draft->ready script once" 1 "$(grep -c 'bash scripts/ci/ready-bot-draft-pr.sh ' "$code_only" || true)"
assert "PR step waits for the push's checks to register" 1 "$(grep -c 'ready-bot-draft-pr.sh .*--wait-for-checks-seconds [1-9]' "$code_only" || true)"
assert "PR step pins the pushed head" 1 "$(grep -c 'ready-bot-draft-pr.sh .*--expect-head "\$pushed_sha"' "$code_only" || true)"
assert "a ready-script failure goes through pr_op_failed" 1 "$(grep -c 'pr_op_failed "ready-for-review' "$code_only" || true)"

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
#
# It also answers scripts/ci/ready-bot-draft-pr.sh, which the step (and the
# sweep step) call: its PR lookup (`pr list` with isDraft in --json) keys as
# `list:ready`, and its REST/GraphQL calls key as `graphql`, `check-runs`,
# `comments-read` and `comment-post`. Knobs: GH_STUB_READY_LINE (the lookup's
# TSV row; "none" = no open PR), GH_STUB_CHECKS, GH_STUB_GRAPHQL_ISDRAFT.
#
# And the step's own ready-PR guard: `pr view --json isDraft` answers
# GH_STUB_ISDRAFT (default true, i.e. the pre-existing force-push path);
# `api repos/<r>/pulls/<n>` (key `pr-body`) answers GH_STUB_PR_BODY; `pr
# comment` APPENDS its --body-file to GH_STUB_COMMENTS, which `comments-read`
# returns — so dedup across runs is exercised against real accumulated state.
prev=""
json=""
method="GET"
for a in "$@"; do
  if [ "$prev" = "--json" ]; then json="$a"; fi
  if [ "$prev" = "-X" ]; then method="$a"; fi
  prev="$a"
done
verb="${2:-}"
key="$verb"
if [ "$verb" = "view" ]; then key="view:$json"; fi
case "$json" in *isDraft*) [ "$verb" = "list" ] && key="list:ready" ;; esac
if [ "${1:-}" = "api" ]; then
  case "$*" in
    *convertPullRequestToDraft*) key="graphql:convert" ;;
    *" graphql "*)   key="graphql" ;;
    *"/check-runs"*) key="check-runs" ;;
    *"/comments"*)   if [ "$method" = "POST" ]; then key="comment-post"; else key="comments-read"; fi ;;
    *"/pulls/"*)     key="pr-body" ;;
    *)               key="api:unhandled" ;;
  esac
fi
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
  list:ready)
    if [ "${GH_STUB_READY_LINE:-}" != "none" ]; then
      printf '%s\n' "${GH_STUB_READY_LINE:-$(printf '4242\ttrue\t%s\tPR_kwDOtest4242\t2026-09-29T00:00:00Z\thttps://github.com/qontinui/qontinui-runner/pull/4242\tjspinak' a47f223e3a47f223e3a47f223e3a47f223e3a47f)}"
    fi
    ;;
  check-runs)    printf '%s\n' "${GH_STUB_CHECKS:-3}" ;;
  graphql)       printf '%s\n' "${GH_STUB_GRAPHQL_ISDRAFT:-false}" ;;
  comments-read) if [ -n "${GH_STUB_COMMENTS:-}" ] && [ -f "$GH_STUB_COMMENTS" ]; then cat "$GH_STUB_COMMENTS"; fi ;;
  pr-body)       printf '%s\n' "${GH_STUB_PR_BODY:-}" ;;
  view:isDraft)  printf '%s\n' "${GH_STUB_ISDRAFT:-true}" ;;
  view:mergeable,mergeStateStatus) printf '%s\n' "${GH_STUB_MERGE:-MERGEABLE CLEAN}" ;;
  view:id)       echo "PR_kwDOtest77" ;;
  graphql:convert) printf '%s\n' "${GH_STUB_CONVERT_ISDRAFT:-true}" ;;
  comment)
    body_file=""; prev=""
    for a in "$@"; do [ "$prev" = "--body-file" ] && body_file="$a"; prev="$a"; done
    [ -f "$body_file" ] || { echo "gh stub: comment without --body-file" >&2; exit 98; }
    cat "$body_file" >> "${GH_STUB_COMMENTS:?}"
    echo "https://github.com/qontinui/qontinui-runner/pull/77#issuecomment-1"
    ;;
  comment-post)  echo "https://github.com/qontinui/qontinui-runner/pull/4242#issuecomment-1" ;;
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
# `git rev-parse HEAD` names the commit just pushed; it matches the head the
# gh stub's ready lookup reports, so the ready script's --expect-head converges.
if [ "${1:-}" = "rev-parse" ]; then
  echo "a47f223e3a47f223e3a47f223e3a47f223e3a47f"
fi
exit 0
STUB

# The ready script's re-poll sleeps; never actually wait in a unit test.
cat > "$bin/sleep" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB

chmod +x "$bin/gh" "$bin/git" "$bin/sleep"

# --- Fixture inputs the step reads --------------------------------------------
runner_temp="$work/runner-temp"
mkdir -p "$runner_temp" "$work/repo/src-tauri"
printf 'CREATE TABLE project.a ();\nCREATE TABLE coord.b ();\n' > "$runner_temp/schema.fresh.sql"
: > "$work/repo/src-tauri/schema.pg.sql.generated"
# The step runs `bash scripts/ci/ready-bot-draft-pr.sh` from the checkout root,
# so the fixture checkout carries the REAL script — the one CI ships.
mkdir -p "$work/repo/scripts/ci"
cp "$ready_script" "$work/repo/scripts/ci/ready-bot-draft-pr.sh"

STUB_URL="https://github.com/qontinui/qontinui-runner/pull/4242"

# Pin the stub knobs rather than inheriting them. run_step forwards these to the
# step, so an ambient `GH_STUB_EMPTY_URL=1` in the caller's environment would
# silently re-point a dozen assertions at a different scenario.
GH_STUB_EMPTY_URL=""
GH_STUB_FAIL_FIRST_CREATE=""
GIT_STUB_STAGED=1
PAT_TOKEN_STUB="stub-pat-token"
GH_STUB_READY_LINE=""
GH_STUB_CHECKS=3
GH_STUB_GRAPHQL_ISDRAFT=false
GH_STUB_ISDRAFT=true
GH_STUB_MERGE="MERGEABLE CLEAN"
GH_STUB_CONVERT_ISDRAFT=true
GH_STUB_PR_BODY=""
GH_STUB_COMMENTS="$work/comments.txt"
: > "$GH_STUB_COMMENTS"
export GH_STUB_EMPTY_URL GH_STUB_FAIL_FIRST_CREATE GIT_STUB_STAGED PAT_TOKEN_STUB
export GH_STUB_READY_LINE GH_STUB_CHECKS GH_STUB_GRAPHQL_ISDRAFT
export GH_STUB_ISDRAFT GH_STUB_PR_BODY GH_STUB_COMMENTS GH_STUB_MERGE GH_STUB_CONVERT_ISDRAFT

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
    GH_STUB_READY_LINE="${GH_STUB_READY_LINE:-}" \
    GH_STUB_CHECKS="${GH_STUB_CHECKS:-3}" \
    GH_STUB_GRAPHQL_ISDRAFT="${GH_STUB_GRAPHQL_ISDRAFT:-false}" \
    GH_STUB_ISDRAFT="${GH_STUB_ISDRAFT:-true}" \
    GH_STUB_MERGE="${GH_STUB_MERGE:-MERGEABLE CLEAN}" \
    GH_STUB_CONVERT_ISDRAFT="${GH_STUB_CONVERT_ISDRAFT:-true}" \
    GH_STUB_PR_BODY="${GH_STUB_PR_BODY:-}" \
    GH_STUB_COMMENTS="$GH_STUB_COMMENTS" \
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
assert "create path: readied the draft (it carries checks)" 1 "$(grep -c '^readied #4242$' "$work/out.txt" || true)"
assert "create path: ready mutation called once" 1 "$(grep -c '^graphql$' "$work/gh.log" || true)"
assert "create path: summary names the ready state" 1 "$(grep -c 'Refresh PR state: ready for review' "$work/summary.md" || true)"

# 2. Happy path with an existing open PR: edit, never create.
rc="$(run_step "" "77")"
assert "edit path: exit code" 0 "$rc"
assert "edit path: called edit once" 1 "$(grep -c '^edit$' "$work/gh.log" || true)"
assert "edit path: never called create" 0 "$(grep -c '^create$' "$work/gh.log" || true)"
assert "edit path: PR url in step summary" 1 "$(grep -c "$STUB_URL" "$work/summary.md" || true)"
assert "edit path: readied the draft (it carries checks)" 1 "$(grep -c '^readied #4242$' "$work/out.txt" || true)"

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

# 9a. An existing DRAFT PR is still force-pushed and edited, exactly as before.
: > "$GH_STUB_COMMENTS"
rc="$(run_step "" "77")"
assert "draft PR: exit code" 0 "$rc"
assert "draft PR: isDraft was read" 1 "$(grep -c '^view:isDraft$' "$work/gh.log" || true)"
assert "draft PR: force-pushed" 1 "$(grep -c 'push --force' "$work/git.log" || true)"
assert "draft PR: edited" 1 "$(grep -c '^edit$' "$work/gh.log" || true)"
assert "draft PR: no drift comment" 0 "$(grep -c '^comment$' "$work/gh.log" || true)"

# 9b. An existing READY PR (possibly a live merge-train candidate) is NEVER
#     force-pushed. The drift arrives as a comment, deduped across runs.
GH_STUB_ISDRAFT=false
: > "$GH_STUB_COMMENTS"
rc1="$(run_step "" "77")"
push1="$(grep -c 'push' "$work/git.log" || true)"
checkout1="$(grep -c 'checkout -B' "$work/git.log" || true)"
edit_create1="$(grep -cE '^(edit|create|list:ready|graphql)$' "$work/gh.log" || true)"
warn1="$(grep -c '::warning::Refresh PR #77 is marked ready' "$work/out.txt" || true)"
rc2="$(run_step "" "77")"
push2="$(grep -c 'push' "$work/git.log" || true)"
assert "ready PR: exit code (green)" 0 "$rc1"
assert "ready PR: no push" 0 "$push1"
assert "ready PR: branch not even rebuilt" 0 "$checkout1"
assert "ready PR: no edit/create/ready-script call" 0 "$edit_create1"
assert "ready PR: warns it did not force-push" 1 "$warn1"
assert "ready PR, second identical run: exit code" 0 "$rc2"
assert "ready PR, second identical run: no push" 0 "$push2"
assert "ready PR: exactly one comment across two identical runs" 1 "$(grep -c 'schema-pg-sql-refresh-dump:' "$GH_STUB_COMMENTS" || true)"
assert "ready PR: comment says not force-pushed" 1 "$(grep -c 'was \*\*not\*\* force-pushed' "$GH_STUB_COMMENTS" || true)"
# A DIFFERENT drift is news: a second comment.
cp "$runner_temp/schema.fresh.sql" "$work/fresh.orig.sql"
printf 'CREATE TABLE coord.later ();\n' >> "$runner_temp/schema.fresh.sql"
rc="$(run_step "" "77")"
assert "ready PR, different drift: exit code" 0 "$rc"
assert "ready PR, different drift: a second comment" 2 "$(grep -c 'schema-pg-sql-refresh-dump:' "$GH_STUB_COMMENTS" || true)"
assert "ready PR, different drift: still no push" 0 "$(grep -c 'push' "$work/git.log" || true)"
cp "$work/fresh.orig.sql" "$runner_temp/schema.fresh.sql"
# The PR body already carries this dump's marker (it was pushed with it): the
# first ready night must not announce drift the PR already holds.
: > "$GH_STUB_COMMENTS"
GH_STUB_PR_BODY="$(bash -c "awk '!/^-- Dumped by pg_dump version/' '$runner_temp/schema.fresh.sql' | sha256sum | cut -c1-16" | sed 's/^/<!-- schema-pg-sql-refresh-dump:/; s/$/ -->/')"
rc="$(run_step "" "77")"
GH_STUB_PR_BODY=""
assert "ready PR carrying this dump: exit code" 0 "$rc"
assert "ready PR carrying this dump: no comment" 0 "$(grep -c '^comment$' "$work/gh.log" || true)"
# A ready PR that CONFLICTS with main can never land: converted back to a
# draft (from the mutation's own isDraft), then rebuilt and force-pushed like
# any draft, and re-readied by the ready script once its checks register.
for shape in "CONFLICTING DIRTY" "CONFLICTING UNKNOWN" "MERGEABLE DIRTY"; do
  GH_STUB_MERGE="$shape"
  : > "$GH_STUB_COMMENTS"
  rc="$(run_step "" "77")"
  assert "ready+conflicting ($shape): exit code" 0 "$rc"
  assert "ready+conflicting ($shape): converted to draft" 1 "$(grep -c '^graphql:convert$' "$work/gh.log" || true)"
  assert "ready+conflicting ($shape): force-pushed" 1 "$(grep -c 'push --force' "$work/git.log" || true)"
  assert "ready+conflicting ($shape): no drift comment" 0 "$(grep -c '^comment$' "$work/gh.log" || true)"
  assert "ready+conflicting ($shape): re-readied" 1 "$(grep -c '^readied #4242$' "$work/out.txt" || true)"
done
GH_STUB_MERGE="CONFLICTING DIRTY"
GH_STUB_CONVERT_ISDRAFT=false
rc="$(run_step "" "77")"
GH_STUB_CONVERT_ISDRAFT=true
assert "conflict convert returns isDraft=false: exit code" 1 "$rc"
assert "conflict convert returns isDraft=false: never pushed" 0 "$(grep -c 'push' "$work/git.log" || true)"
rc="$(run_step "graphql:convert" "77")"
assert "conflict convert refused: exit code" 1 "$rc"
assert "conflict convert refused: never pushed" 0 "$(grep -c 'push' "$work/git.log" || true)"
# UNKNOWN mergeability is not a conflict: comment only, never a push.
GH_STUB_MERGE="UNKNOWN UNKNOWN"
: > "$GH_STUB_COMMENTS"
rc="$(run_step "" "77")"
assert "ready+UNKNOWN: exit code" 0 "$rc"
assert "ready+UNKNOWN: comment only" 1 "$(grep -c '^comment$' "$work/gh.log" || true)"
assert "ready+UNKNOWN: never pushed" 0 "$(grep -c 'push' "$work/git.log" || true)"
assert "ready+UNKNOWN: not converted" 0 "$(grep -c '^graphql:convert$' "$work/gh.log" || true)"
GH_STUB_MERGE="MERGEABLE CLEAN"
: > "$GH_STUB_COMMENTS"
rc="$(run_step "" "77")"
assert "ready+mergeable: comment only" "1 0 0" "$(grep -c '^comment$' "$work/gh.log" || true) $(grep -c 'push' "$work/git.log" || true) $(grep -c '^graphql:convert$' "$work/gh.log" || true)"
rc="$(run_step "view:mergeable,mergeStateStatus" "77")"
assert "mergeability unreadable: exit code" 1 "$rc"
assert "mergeability unreadable: never pushed" 0 "$(grep -c 'push' "$work/git.log" || true)"
: > "$GH_STUB_COMMENTS"

# Unreadable state is a red, never a guess — and never a push.
rc="$(run_step "view:isDraft" "77")"
assert "isDraft unreadable: exit code" 1 "$rc"
assert "isDraft unreadable: never pushed" 0 "$(grep -c 'push' "$work/git.log" || true)"
rc="$(run_step "comments-read" "77")"
assert "comments unreadable: exit code" 1 "$rc"
assert "comments unreadable: never comments blind" 0 "$(grep -c '^comment$' "$work/gh.log" || true)"
rc="$(run_step "comment" "77")"
assert "comment refused: exit code" 1 "$rc"
assert "comment refused: reporter names the op" 1 "$(grep -c "'comment on #77' operation failed" "$work/out.txt" || true)"
GH_STUB_ISDRAFT=true
: > "$GH_STUB_COMMENTS"

# The body the step writes on the push path carries the marker the guard reads.
rc="$(run_step "" "")"
assert "push path: PR body carries the dump marker" 1 "$(grep -c 'schema-pg-sql-refresh-dump:' "$runner_temp/pr-body.md" || true)"

# 10. The draft->ready script fails AFTER the PR was written => a red through
#     pr_op_failed's ready arm, never a green with the draft undecided.
GH_STUB_GRAPHQL_ISDRAFT=true
rc="$(run_step "" "77")"
GH_STUB_GRAPHQL_ISDRAFT=false
assert "ready script fails: exit code" 1 "$rc"
assert "ready script fails: reporter names the ready step" 1 "$(grep -c 'could not be decided' "$work/out.txt" || true)"
assert "ready script fails: does NOT print create's remediation" 0 "$(grep -c "$REMEDIATION_CREATE" "$work/out.txt" || true)"
rc="$(run_step "list:ready" "77")"
assert "ready lookup refused: exit code" 1 "$rc"

# 11. A draft whose head has no checks after the wait stays a draft, the step
#     stays green (delivery succeeded), and the summary says so.
GH_STUB_CHECKS=0
rc="$(run_step "" "77")"
GH_STUB_CHECKS=3
assert "no checks yet: exit code" 0 "$rc"
assert "no checks yet: never calls the mutation" 0 "$(grep -c '^graphql$' "$work/gh.log" || true)"
assert "no checks yet: summary says still a DRAFT" 1 "$(grep -c 'Refresh PR state: still a DRAFT' "$work/summary.md" || true)"

# =============================================================================
# The steps AROUND the PR step: fresh_check's outputs, the shipped `if:`
# conditions, and the sweep step's body.
# =============================================================================

# extract_run <step name> <out file> — the `run: |` body of a step, verbatim,
# by the same brittle-and-loud rule as the PR step above (empty => FAIL).
extract_run() {
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
  if [ "$(grep -c 'set -euo pipefail' "$2" || true)" -lt 1 ]; then
    echo "::error::step '$1' did not extract from $workflow (renamed or re-indented?)."
    exit 1
  fi
  bash -n "$2"
}

# extract_if <step name> — the step's `if:` expression, a folded (`>-`) value
# joined onto one line. Prints nothing when the step has no `if:`.
extract_if() {
  awk -v name="$1" '
    state == 0 && $0 == "      - name: " name { state = 1; next }
    state == 1 && $0 ~ /^      - name: / { exit }
    state == 1 && $0 ~ /^        if: / {
      v = substr($0, 13)
      if (v == ">-" || v == ">" || v == "|") { state = 2; next }
      print v; exit
    }
    state == 2 {
      if ($0 ~ /^          /) { sub(/^ +/, ""); printf "%s ", $0; next }
      exit
    }
  ' "$workflow"
}

# eval_if <expression> <outputs file> — evaluates a step `if:` against the
# fresh_check outputs, under the implicit success() (every earlier step
# passed and the run was not cancelled). STRICT: it models
# `steps.fresh_check.outputs.<name>`, `cancelled()` (false), string literals,
# ==, !=, &&, ||, ! and parentheses — anything else (a function
# call, another step's outputs) is a loud error, never a guess. Prints
# true/false.
#
# eval_if <expression> <outputs file> failed — the same, on a run where an
# EARLIER step failed: GitHub then adds an implicit success() (false) to any
# expression that calls no status function, so only an expression carrying
# `cancelled()` can still run.
eval_if() {
  python3 - "$1" "$2" "${3:-}" <<'PY'
import re, sys
expr, outputs_path, mode = sys.argv[1].strip(), sys.argv[2], sys.argv[3]
outputs = {}
for line in open(outputs_path):
    line = line.rstrip("\n")
    if "=" in line:
        k, v = line.split("=", 1)
        outputs[k] = v
if expr.startswith("${{") and expr.endswith("}}"):
    expr = expr[3:-2].strip()
if not expr:
    sys.exit("eval_if: empty expression")
tokens = re.findall(r"steps\.fresh_check\.outputs\.[A-Za-z_][A-Za-z0-9_]*|cancelled\(\)|'[^']*'|==|!=|&&|\|\||!|\(|\)|\S+", expr)
out = []
for t in tokens:
    if t.startswith("steps.fresh_check.outputs."):
        out.append(repr(outputs.get(t.rsplit(".", 1)[1], "")))
    elif t.startswith("'") and t.endswith("'"):
        out.append(repr(t[1:-1]))
    elif t in ("==", "!=", "(", ")"):
        out.append(t)
    elif t == "&&":
        out.append("and")
    elif t == "||":
        out.append("or")
    elif t == "!":
        out.append("not")
    elif t == "cancelled()":
        out.append("False")
    else:
        sys.exit("eval_if: unmodelled token %r in %r" % (t, expr))
result = eval(" ".join(out), {"__builtins__": {}})
if mode == "failed" and "cancelled()" not in expr:
    result = False
print("true" if result else "false")
PY
}

FRESH_STEP="Check schema.pg.sql.generated is fresh"
PR_STEP="Open or update the schema refresh PR"
SWEEP_STEP="Sweep the refresh PR (ready it once it carries checks; page if parked)"

fresh_body="$work/fresh-step.sh"
sweep_body="$work/sweep-step.sh"
extract_run "$FRESH_STEP" "$fresh_body"
extract_run "$SWEEP_STEP" "$sweep_body"
pr_if="$(extract_if "$PR_STEP")"
sweep_if="$(extract_if "$SWEEP_STEP")"
[ -n "$pr_if" ] && [ -n "$sweep_if" ] || {
  echo "::error::could not extract the PR step's or the sweep step's if: from $workflow"
  exit 1
}

echo ""
echo "Static properties of the sweep step:"
assert "sweep calls the script with --max-age-hours 48" 1 "$(grep -c 'bash scripts/ci/ready-bot-draft-pr.sh .*--max-age-hours 48' "$sweep_body" || true)"
assert "sweep: no '|| true'" 0 "$(grep -vE '^[[:space:]]*#' "$sweep_body" | grep -c '|| true' || true)"
# Belt and braces over the behavioural case below: the sweep's selector must
# not mention drift at all.
assert "sweep's if: does not reference drift" 0 "$(printf '%s\n' "$sweep_if" | grep -c 'drift' || true)"
# The sweep must be the LAST step that touches the PR: after the PR step.
pr_line_no="$(grep -nF -- "- name: $PR_STEP" "$workflow" | cut -d: -f1)"
sweep_line_no="$(grep -nF -- "- name: $SWEEP_STEP" "$workflow" | cut -d: -f1)"
assert "sweep step comes after the PR step" 1 "$([ "${sweep_line_no:-0}" -gt "${pr_line_no:-0}" ] && echo 1 || echo 0)"

# --- fresh_check fixture ------------------------------------------------------
# A checkout whose regenerate script is a stub that writes $REGEN_STUB_SOURCE
# over the dump. The step's own shape guards need every schema present and a
# non-shrinking line count, so the base dump carries all of them.
fc="$work/fc-repo"
mkdir -p "$fc/src-tauri/scripts"
base_dump="$work/dump.base.sql"
{
  echo "-- Dumped by pg_dump version 16.0"
  for schema in project coord agent auth atlas_managed; do
    echo "CREATE TABLE ${schema}.t ();"
  done
} > "$base_dump"
drift_dump="$work/dump.drift.sql"
{ cat "$base_dump"; echo "CREATE TABLE coord.brand_new_table ();"; } > "$drift_dump"
cat > "$fc/src-tauri/scripts/regenerate_schema_pg_sql.sh" <<'STUB'
#!/usr/bin/env bash
cp "${REGEN_STUB_SOURCE:?}" src-tauri/schema.pg.sql.generated
STUB

# run_fresh <SELF_HEAL> <fresh-dump> — echoes the exit code; outputs in
# $work/fresh.out, summary in $work/fresh.summary.md, log in $work/fresh.log.
run_fresh() {
  cp "$base_dump" "$fc/src-tauri/schema.pg.sql.generated"
  : > "$work/fresh.out"
  : > "$work/fresh.summary.md"
  local rc=0
  (
    cd "$fc"
    PATH="$bin:$PATH" \
    GIT_STUB_LOG="$work/git.log" \
    REGEN_STUB_SOURCE="$2" \
    SELF_HEAL="$1" \
    CLORINDE_PG_CONTAINER="" \
    CLORINDE_PG_HOST=localhost \
    CLORINDE_PG_PORT=5433 \
    PGPASSWORD=stub \
    REGEN_SKIP_ATLAS=1 \
    RUNNER_TEMP="$runner_temp" \
    GITHUB_OUTPUT="$work/fresh.out" \
    GITHUB_STEP_SUMMARY="$work/fresh.summary.md" \
    bash "$fresh_body"
  ) > "$work/fresh.log" 2>&1 || rc=$?
  echo "$rc"
}

# run_sweep — echoes the exit code; stdout+stderr in $work/sweep.log, gh keys in
# $work/gh.log, summary in $work/sweep.summary.md.
run_sweep() {
  : > "$work/gh.log"
  : > "$work/sweep.summary.md"
  local rc=0
  (
    cd "$work/repo"
    PATH="$bin:$PATH" \
    GH_STUB_LOG="$work/gh.log" \
    GH_STUB_FAIL="" \
    GH_STUB_READY_LINE="${GH_STUB_READY_LINE:-}" \
    GH_STUB_CHECKS="${GH_STUB_CHECKS:-3}" \
    GH_STUB_GRAPHQL_ISDRAFT="${GH_STUB_GRAPHQL_ISDRAFT:-false}" \
    GH_TOKEN="stub-token" \
    REPO="qontinui/qontinui-runner" \
    BRANCH="chore/schema-pg-sql-refresh" \
    HEALED="$(sed -n 's/^healed=//p' "$work/fresh.out")" \
    RUNNER_TEMP="$runner_temp" \
    GITHUB_STEP_SUMMARY="$work/sweep.summary.md" \
    bash "$sweep_body"
  ) > "$work/sweep.log" 2>&1 || rc=$?
  echo "$rc"
}

out_of() { sed -n "s/^$1=//p" "$work/fresh.out"; }

echo ""
echo "Workflow cases (fresh_check -> if: -> sweep):"

# A. The case this plan exists for: NO drift on a self-heal run, and an open
#    DRAFT refresh PR from an earlier night whose head carries checks. The PR
#    step must not run (nothing to deliver); the sweep must, and must ready it.
rc="$(run_fresh true "$base_dump")"
assert "no drift: fresh_check exit code" 0 "$rc"
assert "no drift: drift=false" "false" "$(out_of drift)"
assert "no drift: healed=false" "false" "$(out_of healed)"
assert "no drift: PR step does not run" "false" "$(eval_if "$pr_if" "$work/fresh.out")"
assert "no drift: sweep step runs" "true" "$(eval_if "$sweep_if" "$work/fresh.out")"
rc="$(run_sweep)"
assert "no drift: sweep exit code" 0 "$rc"
assert "no drift: sweep invoked the script and readied the draft" 1 "$(grep -c '^readied #4242$' "$work/sweep.log" || true)"
assert "no drift: sweep called the ready mutation once" 1 "$(grep -c '^graphql$' "$work/gh.log" || true)"
assert "no drift: sweep summary names the outcome" 1 "$(grep -c 'sweep outcome: `readied #4242`' "$work/sweep.summary.md" || true)"

# B. Drift on a self-heal run: healed=true, the summary names the drift, and
#    both the PR step and the sweep run.
rc="$(run_fresh true "$drift_dump")"
assert "drift + self-heal: fresh_check exit code (green: healed)" 0 "$rc"
assert "drift + self-heal: drift=true" "true" "$(out_of drift)"
assert "drift + self-heal: healed=true" "true" "$(out_of healed)"
assert "drift + self-heal: summary says HEALED, not fresh" 1 "$(grep -c 'drift HEALED' "$work/fresh.summary.md" || true)"
assert "drift + self-heal: summary names the drifted table" 1 "$(grep -c 'coord.brand_new_table' "$work/fresh.summary.md" || true)"
assert "drift + self-heal: PR step runs" "true" "$(eval_if "$pr_if" "$work/fresh.out")"
assert "drift + self-heal: sweep step runs" "true" "$(eval_if "$sweep_if" "$work/fresh.out")"

# A2. An EARLIER step failed (poetry, alembic, regeneration): that night
#     delivers nothing, and a parked PR still needs its re-decide and page.
#     fresh_check writes self_heal first, so the output is set.
assert "upstream failure: PR step does not run" "false" "$(eval_if "$pr_if" "$work/fresh.out" failed)"
assert "upstream failure: sweep step still runs" "true" "$(eval_if "$sweep_if" "$work/fresh.out" failed)"

# C. No drift, not a self-heal run (a PR or a non-main dispatch): neither runs.
rc="$(run_fresh false "$base_dump")"
assert "no drift, no self-heal: exit code" 0 "$rc"
assert "no drift, no self-heal: healed=false" "false" "$(out_of healed)"
assert "no drift, no self-heal: PR step does not run" "false" "$(eval_if "$pr_if" "$work/fresh.out")"
assert "no drift, no self-heal: sweep does not run" "false" "$(eval_if "$sweep_if" "$work/fresh.out")"

# D. Drift that may NOT self-heal: red, and healed=false (nothing was healed).
rc="$(run_fresh false "$drift_dump")"
assert "drift, no self-heal: exit code (red)" 1 "$rc"
assert "drift, no self-heal: healed=false" "false" "$(out_of healed)"
assert "drift, no self-heal: no HEALED summary" 0 "$(grep -c 'drift HEALED' "$work/fresh.summary.md" || true)"

# E. The sweep over an already-ready PR, and over no PR at all, is a green no-op.
GH_STUB_READY_LINE="$(printf '4242\tfalse\ta47f223e3a47f223e3a47f223e3a47f223e3a47f\tPR_kwDOtest4242\t2026-01-01T00:00:00Z\thttps://github.com/qontinui/qontinui-runner/pull/4242\tjspinak')"
run_fresh true "$base_dump" > /dev/null
rc="$(run_sweep)"
assert "sweep over a ready PR: exit code" 0 "$rc"
assert "sweep over a ready PR: already-ready, no mutation" "already-ready #4242 0" "$(tail -n1 "$work/sweep.log") $(grep -c '^graphql$' "$work/gh.log" || true)"
GH_STUB_READY_LINE="none"
rc="$(run_sweep)"
GH_STUB_READY_LINE=""
assert "sweep with no PR: exit code" 0 "$rc"
assert "sweep with no PR: no-pr" "no-pr" "$(tail -n1 "$work/sweep.log")"

# --- Mutation proofs ----------------------------------------------------------
if [ -z "${SCHEMA_STEP_MUTANT:-}" ]; then
  echo ""
  echo "Mutation proofs (each broken workflow must be caught by its named case):"

  # mutate <name> <awk-program over the workflow> <case that must FAIL>
  mutate() {
    local name="$1" prog="$2" want="$3"
    local mutant="$work/mutant-$name.yml"
    awk "$prog" "$real_workflow" > "$mutant"
    local changed
    changed="$( { diff "$real_workflow" "$mutant" || true; } | { grep -c '^>' || true; } )"
    if [ "$changed" != "1" ]; then
      assert "mutant '$name': changed exactly one line" 1 "$changed"
      return
    fi
    local rc=0
    SCHEMA_STEP_MUTANT=1 SCHEMA_WORKFLOW_UNDER_TEST="$mutant" \
      bash "${BASH_SOURCE[0]}" > "$work/mutant-$name.out" 2>&1 || rc=$?
    assert "mutant '$name': suite exits non-zero" 1 "$([ "$rc" -ne 0 ] && echo 1 || echo 0)"
    assert "mutant '$name': caught by '$want'" 1 \
      "$( { grep -F "FAIL  $want" "$work/mutant-$name.out" || true; } | head -n1 | wc -l | tr -d ' ')"
  }

  # Gate the sweep on drift (the regression: a no-drift night would then never
  # re-decide a parked draft).
  mutate sweep-gated-on-drift \
    "BEGIN { want = \"      - name: $SWEEP_STEP\" }
     \$0 == want { in_sweep = 1 }
     in_sweep && /^        if: / { \$0 = \"        if: steps.fresh_check.outputs.drift == 'true' && steps.fresh_check.outputs.self_heal == 'true'\"; in_sweep = 0 }
     { print }" \
    "no drift: sweep step runs"
  # Hard-code healed=false (green-because-healed indistinguishable again).
  mutate healed-hardcoded-false \
    '$0 == "            echo \"healed=$healed\"" { $0 = "            echo \"healed=false\"" } { print }' \
    "drift + self-heal: healed=true"
  # Remove the ready-PR guard: a ready PR would be force-pushed again.
  mutate ready-guard-removed \
    '$0 == "            if [ \"$existing_draft\" != \"true\" ]; then" { $0 = "            if false; then" } { print }' \
    "ready PR: no push"
  # Break the dedup: every run comments.
  mutate dedup-broken \
    '$0 == "              if grep -qF \"$drift_marker\" \"$seen\"; then" { $0 = "              if false; then" } { print }' \
    "ready PR: exactly one comment across two identical runs"
  # Remove the conflict check: a conflicting ready PR is commented on forever.
  mutate conflict-check-removed \
    '$0 == "              if [ \"$mergeable\" = \"CONFLICTING\" ] || [ \"${merge_status:-}\" = \"DIRTY\" ]; then" { $0 = "              if false; then" } { print }' \
    "ready+conflicting (CONFLICTING DIRTY): force-pushed"
  # Back to the default success(): an upstream failure skips the sweep.
  mutate sweep-success-only \
    "BEGIN { want = \"      - name: $SWEEP_STEP\" }
     \$0 == want { in_sweep = 1 }
     in_sweep && /^        if: / { \$0 = \"        if: steps.fresh_check.outputs.self_heal == 'true'\"; in_sweep = 0 }
     { print }" \
    "upstream failure: sweep step still runs"
  # Drop the head pin at the PR step's call site.
  mutate expect-head-dropped \
    '{ sub(/ --expect-head "\$pushed_sha"/, "") } { print }' \
    "PR step pins the pushed head"
fi
echo ""
if [ "$failures" -ne 0 ]; then
  echo "FAILED: $failures assertion(s)"
  exit 1
fi
echo "All assertions passed."
