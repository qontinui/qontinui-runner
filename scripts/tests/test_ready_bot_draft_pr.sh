#!/usr/bin/env bash
# shellcheck disable=SC2016  # literal `$` and backticks (GraphQL variables, markdown, sed patterns) are intended
# Regression test for scripts/ci/ready-bot-draft-pr.sh, against a STUBBED `gh`
# and `sleep`. No network, no real repository.
#
# WHY THIS FILE EXISTS. That script is the only thing in the fleet that takes a
# bot-filed self-heal PR out of draft. Before it existed, the two nightlies that
# file such PRs (schema-pg-sql-freshness-nightly.yml, sibling-pin-bump.yml)
# parked their fixes as drafts for 17-18 days, because coord does not propose
# drafts and nothing re-decided the draft. An unexercised remediation is
# indistinguishable from no remediation, so every outcome is pinned here.
#
# The properties pinned:
#   1. It un-drafts ONLY on measurement: >= 1 check run on the head. A draft
#      with zero check runs is exactly the case the draft exists for and must
#      stay a draft.
#   2. The un-draft is verified by the mutation's OWN returned isDraft.
#   3. Every gh failure is a red with an ::error:: naming the call — never a
#      green with the fix undelivered.
#   4. A zero-check draft older than the budget is paged exactly ONCE (the
#      marker makes it idempotent across nights); within budget, or ready, it
#      is never paged.
#
# MUTATION PROOFS. A test that has never been seen to fail proves nothing, so
# the last section re-runs this whole suite against deliberately broken copies
# of the script and asserts each copy is CAUGHT by the specific case that
# guards it. (The child run sets READY_BOT_MUTANT=1, which skips that section.)
#
# Plan: plans/2026-09-17-atlas-self-heal-files-its-own-fix-as-a-draft-and-nothing-lands-it.md
#
# Run locally:
#   bash scripts/tests/test_ready_bot_draft_pr.sh

set -euo pipefail

tests_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$tests_dir/../.." && pwd)"
real_script="$repo_root/scripts/ci/ready-bot-draft-pr.sh"
script="${READY_BOT_SCRIPT_UNDER_TEST:-$real_script}"

for f in "$real_script" "$script"; do
  if [ ! -f "$f" ]; then
    echo "::error::cannot find $f"
    exit 1
  fi
done
bash -n "$script"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0
assert() {
  # assert <name> <expected> <actual>
  if [ "$2" = "$3" ]; then
    printf '  PASS  %-62s %s\n' "$1" "$3"
  else
    printf '  FAIL  %-62s expected %s, got %s\n' "$1" "$2" "$3"
    failures=$((failures + 1))
  fi
}

# count <fixed-string> <file> — occurrences (lines), 0 when absent. The
# `|| true` is grep's no-match exit 1, never a suppressed gh failure.
count() { grep -cF -- "$1" "$2" || true; }

# --- Stubs --------------------------------------------------------------------
bin="$work/bin"
mkdir -p "$bin"

cat > "$bin/gh" <<'STUB'
#!/usr/bin/env bash
# Keys each call; logs the key to GH_STUB_LOG; fails any key listed in
# GH_STUB_FAIL like a real refusal (stderr + exit 1). --jq is gh's own flag and
# never reaches a real jq here, so every answer is returned PRE-FILTERED.
key=""
case "${1:-}" in
  pr)
    case "${2:-}" in
      list) key="list" ;;
    esac
    ;;
  api)
    method="GET"
    prev=""
    for a in "$@"; do
      [ "$prev" = "-X" ] && method="$a"
      prev="$a"
    done
    case "$*" in
      *" graphql "*)             key="graphql" ;;
      *"/check-runs"*)           key="check-runs" ;;
      *"/comments"*)
        if [ "$method" = "POST" ]; then key="comment-post"; else key="comments-read"; fi ;;
    esac
    ;;
esac
if [ -z "$key" ]; then
  echo "gh stub: unhandled invocation '$*'" >&2
  exit 97
fi
echo "$key" >> "${GH_STUB_LOG:?}"
case " ${GH_STUB_FAIL:-} " in
  *" $key "*)
    echo "gh: HTTP 403: Resource not accessible by integration ($key)" >&2
    exit 1
    ;;
esac
case "$key" in
  list)
    [ -z "${GH_STUB_PR_LINE:-}" ] || printf '%s\n' "$GH_STUB_PR_LINE"
    ;;
  check-runs)
    # GH_STUB_CHECKS_SEQ: the answer for the 1st, 2nd, ... call; the last
    # value repeats.
    n="$(grep -c '^check-runs$' "$GH_STUB_LOG")"
    set -- ${GH_STUB_CHECKS_SEQ:-0}
    if [ "$n" -gt "$#" ]; then n="$#"; fi
    eval "printf '%s\n' \"\${$n}\""
    ;;
  graphql)
    printf '%s\n' "${GH_STUB_GRAPHQL_ISDRAFT:-false}"
    ;;
  comments-read)
    [ -f "${GH_STUB_COMMENTS:?}" ] && cat "$GH_STUB_COMMENTS"
    ;;
  comment-post)
    body_file=""
    for a in "$@"; do
      case "$a" in body=@*) body_file="${a#body=@}" ;; esac
    done
    [ -f "$body_file" ] || { echo "gh stub: comment post without a body file" >&2; exit 98; }
    cat "$body_file" >> "$GH_STUB_COMMENTS"
    printf '\n' >> "$GH_STUB_COMMENTS"
    echo "https://github.com/qontinui/qontinui-runner/pull/4242#issuecomment-1"
    ;;
esac
exit 0
STUB

cat > "$bin/sleep" <<'STUB'
#!/usr/bin/env bash
echo "sleep $*" >> "${SLEEP_STUB_LOG:?}"
STUB
chmod +x "$bin/gh" "$bin/sleep"

# --- Fixtures -----------------------------------------------------------------
HEAD_SHA="a47f223e3a47f223e3a47f223e3a47f223e3a47f"
NOW_EPOCH="$(date -u -d '2026-09-30T00:00:00Z' +%s)"
CREATED_STALE="2026-09-27T00:00:00Z"   # 72h before NOW: over a 48h budget
CREATED_FRESH="2026-09-29T00:00:00Z"   # 24h before NOW: within it
PR_URL="https://github.com/qontinui/qontinui-runner/pull/4242"

# pr_line <isDraft> <createdAt> — one TSV row exactly as the script's --jq emits.
pr_line() {
  printf '4242\t%s\t%s\tPR_kwDOtest4242\t%s\t%s\tjspinak' "$1" "$HEAD_SHA" "$2" "$PR_URL"
}

# run_script <args...> — echoes the exit code. Knobs are read from the
# GH_STUB_* shell variables below, pinned rather than inherited so an ambient
# value in the caller's environment cannot re-point a case.
# Leaves stdout+stderr in $work/out.txt, the gh key log in $work/gh.log, sleeps
# in $work/sleep.log and GITHUB_OUTPUT in $work/output.txt. The comment store
# ($work/comments.txt) is NOT reset here — idempotency spans runs.
run_script() {
  : > "$work/gh.log"
  : > "$work/sleep.log"
  : > "$work/output.txt"
  local rc=0
  (
    PATH="$bin:$PATH" \
    GH_STUB_LOG="$work/gh.log" \
    SLEEP_STUB_LOG="$work/sleep.log" \
    GH_STUB_COMMENTS="$work/comments.txt" \
    GH_STUB_FAIL="$S_FAIL" \
    GH_STUB_PR_LINE="$S_PR_LINE" \
    GH_STUB_CHECKS_SEQ="$S_CHECKS" \
    GH_STUB_GRAPHQL_ISDRAFT="$S_GRAPHQL" \
    GITHUB_OUTPUT="$work/output.txt" \
    READY_BOT_NOW_EPOCH="$NOW_EPOCH" \
    READY_BOT_POLL_INTERVAL_SECONDS=15 \
    bash "$script" --repo qontinui/qontinui-runner --branch chore/schema-pg-sql-refresh "$@"
  ) > "$work/out.txt" 2>&1 || rc=$?
  echo "$rc"
}

# reset <pr-line> <checks-seq> — default knobs for a fresh case.
reset() {
  S_FAIL=""
  S_PR_LINE="$1"
  S_CHECKS="$2"
  S_GRAPHQL="false"
  : > "$work/comments.txt"
}

last_line() { tail -n1 "$work/out.txt"; }
MARKER='<!-- ready-bot-draft-pr:stale-draft -->'

echo "Behavioural cases (script: $script):"

# 1. Draft with checks -> readied, mutation exactly once, outputs written.
reset "$(pr_line true "$CREATED_FRESH")" "23"
rc="$(run_script)"
assert "draft with checks: exit code" 0 "$rc"
assert "draft with checks: last line" "readied #4242" "$(last_line)"
assert "draft with checks: mutation called exactly once" 1 "$(count graphql "$work/gh.log")"
assert "draft with checks: result=readied in GITHUB_OUTPUT" 1 "$(count 'result=readied' "$work/output.txt")"
assert "draft with checks: pr=4242 in GITHUB_OUTPUT" 1 "$(count 'pr=4242' "$work/output.txt")"
assert "draft with checks: url in GITHUB_OUTPUT" 1 "$(count "url=$PR_URL" "$work/output.txt")"

# 2. Draft with ZERO checks -> stays a draft; the mutation is never called.
reset "$(pr_line true "$CREATED_FRESH")" "0"
rc="$(run_script)"
assert "0 checks: exit code" 0 "$rc"
assert "0 checks: last line" "draft-no-checks #4242" "$(last_line)"
assert "0 checks: mutation never called" 0 "$(count graphql "$work/gh.log")"
assert "0 checks: no page without --max-age-hours" 0 "$(count comment "$work/gh.log")"

# 3. The mutation answers but the PR is STILL a draft -> red.
reset "$(pr_line true "$CREATED_FRESH")" "5"
S_GRAPHQL="true"
rc="$(run_script)"
assert "mutation returns isDraft=true: exit code" 1 "$rc"
assert "mutation returns isDraft=true: ::error:: says still a draft" 1 "$(count 'The PR is still a draft' "$work/out.txt")"
assert "mutation returns isDraft=true: never prints readied" 0 "$(count 'readied #' "$work/out.txt")"

# 4. gh failures, one per call: each is a red with an ::error:: naming it.
reset "$(pr_line true "$CREATED_FRESH")" "5"
S_FAIL="list"
rc="$(run_script)"
assert "list fails: exit code" 1 "$rc"
assert "list fails: ::error:: names the call" 1 "$(count "::error::ready-bot-draft-pr.sh: 'gh pr list" "$work/out.txt")"
assert "list fails: never reads as no-pr" 0 "$(count 'no-pr' "$work/out.txt")"

reset "$(pr_line true "$CREATED_FRESH")" "5"
S_FAIL="check-runs"
rc="$(run_script)"
assert "check-runs fails: exit code" 1 "$rc"
assert "check-runs fails: ::error:: names the call" 1 "$(count "check-runs' failed" "$work/out.txt")"
assert "check-runs fails: mutation never called" 0 "$(count graphql "$work/gh.log")"

reset "$(pr_line true "$CREATED_FRESH")" "5"
S_FAIL="graphql"
rc="$(run_script)"
assert "graphql fails: exit code" 1 "$rc"
assert "graphql fails: ::error:: names the failed call" 1 "$(count "markPullRequestReadyForReview (PR #4242)' failed" "$work/out.txt")"

reset "$(pr_line true "$CREATED_STALE")" "0"
S_FAIL="comments-read"
rc="$(run_script --max-age-hours 48)"
assert "comments read fails: exit code" 1 "$rc"
assert "comments read fails: ::error:: names the call" 1 "$(count "comments (read)' failed" "$work/out.txt")"
assert "comments read fails: never posts blind" 0 "$(count comment-post "$work/gh.log")"

reset "$(pr_line true "$CREATED_STALE")" "0"
S_FAIL="comment-post"
rc="$(run_script --max-age-hours 48)"
assert "comment post fails: exit code" 1 "$rc"
assert "comment post fails: ::error:: names the call" 1 "$(count "comments (page)' failed" "$work/out.txt")"

# 5. No open PR -> no-pr, and nothing else is called.
reset "" "5"
rc="$(run_script)"
assert "no PR: exit code" 0 "$rc"
assert "no PR: last line" "no-pr" "$(last_line)"
assert "no PR: only the lookup was called" "list" "$(tr '\n' ' ' < "$work/gh.log" | sed 's/ $//')"

# 6. Already-ready PR -> untouched: no check read, no mutation.
reset "$(pr_line false "$CREATED_FRESH")" "5"
rc="$(run_script)"
assert "ready PR: exit code" 0 "$rc"
assert "ready PR: last line" "already-ready #4242" "$(last_line)"
assert "ready PR: mutation never called" 0 "$(count graphql "$work/gh.log")"
assert "ready PR: checks never read" 0 "$(count check-runs "$work/gh.log")"

# 7. Stale zero-check draft -> paged exactly ONCE across two runs.
reset "$(pr_line true "$CREATED_STALE")" "0"
rc1="$(run_script --max-age-hours 48)"
warn1="$(count '::warning::Bot-filed PR #4242' "$work/out.txt")"
paged1="$(count 'paged=true' "$work/output.txt")"
rc2="$(run_script --max-age-hours 48)"
warn2="$(count '::warning::Bot-filed PR #4242' "$work/out.txt")"
assert "stale draft: first run exit code" 0 "$rc1"
assert "stale draft: second run exit code" 0 "$rc2"
assert "stale draft: exactly one page across two runs" 1 "$(count "$MARKER" "$work/comments.txt")"
assert "stale draft: first run paged=true" 1 "$paged1"
assert "stale draft: second run paged=already" 1 "$(count 'paged=already' "$work/output.txt")"
assert "stale draft: ::warning:: on both runs" "1 1" "$warn1 $warn2"
assert "stale draft: page names the age" 1 "$(count 'a draft for 72 hours' "$work/comments.txt")"
assert "stale draft: page names the check count" 1 "$(count 'with 0 check runs' "$work/comments.txt")"
assert "stale draft: page says nothing un-drafts it" 1 "$(count 'Nothing un-drafts a zero-check PR' "$work/comments.txt")"
assert "stale draft: still reports draft-no-checks" "draft-no-checks #4242" "$(last_line)"

# 8. Zero-check draft within budget -> no page, comments never read.
reset "$(pr_line true "$CREATED_FRESH")" "0"
rc="$(run_script --max-age-hours 48)"
assert "within budget: exit code" 0 "$rc"
assert "within budget: no page" 0 "$(count "$MARKER" "$work/comments.txt")"
assert "within budget: no comment call at all" 0 "$(count comment "$work/gh.log")"
assert "within budget: paged=false" 1 "$(count 'paged=false' "$work/output.txt")"

# 9. Ready PR of any age -> never paged.
reset "$(pr_line false "2025-01-01T00:00:00Z")" "0"
rc="$(run_script --max-age-hours 48)"
assert "ancient ready PR: exit code" 0 "$rc"
assert "ancient ready PR: no page" 0 "$(count "$MARKER" "$work/comments.txt")"
assert "ancient ready PR: no comment call" 0 "$(count comment "$work/gh.log")"

# 10. A stale draft that DOES carry checks is readied, not paged.
reset "$(pr_line true "$CREATED_STALE")" "3"
rc="$(run_script --max-age-hours 48)"
assert "stale draft with checks: readied" "readied #4242" "$(last_line)"
assert "stale draft with checks: no page" 0 "$(count comment "$work/gh.log")"

# 11. --wait-for-checks-seconds: checks registering late are waited for...
reset "$(pr_line true "$CREATED_FRESH")" "0 0 2"
rc="$(run_script --wait-for-checks-seconds 300)"
assert "late checks: exit code" 0 "$rc"
assert "late checks: readied after they register" "readied #4242" "$(last_line)"
assert "late checks: three check reads" 3 "$(count check-runs "$work/gh.log")"
assert "late checks: slept twice" 2 "$(count 'sleep 15' "$work/sleep.log")"
# ...and the wait is BOUNDED: 30s / 15s = 2 extra polls, then decide.
reset "$(pr_line true "$CREATED_FRESH")" "0"
rc="$(run_script --wait-for-checks-seconds 30)"
assert "no checks ever: exit code" 0 "$rc"
assert "no checks ever: draft-no-checks" "draft-no-checks #4242" "$(last_line)"
assert "no checks ever: bounded to 1 + 2 check reads" 3 "$(count check-runs "$work/gh.log")"
assert "no checks ever: mutation never called" 0 "$(count graphql "$work/gh.log")"

# 12. Garbled lookup / usage errors are reds, not guesses.
reset $'4242\tmaybe\tnothex' "5"
rc="$(run_script)"
assert "garbled lookup: exit code" 1 "$rc"
assert "garbled lookup: mutation never called" 0 "$(count graphql "$work/gh.log")"
reset "" "0"
rc=0
( PATH="$bin:$PATH" GH_STUB_LOG="$work/gh.log" bash "$script" --branch x ) > "$work/out.txt" 2>&1 || rc=$?
assert "missing --repo: usage exit code" 2 "$rc"
rc=0
( PATH="$bin:$PATH" GH_STUB_LOG="$work/gh.log" bash "$script" --repo a/b --branch x --max-age-hours 4x ) > "$work/out.txt" 2>&1 || rc=$?
assert "non-numeric --max-age-hours: usage exit code" 2 "$rc"

# --- Static: no suppression on any gh call ------------------------------------
echo ""
echo "Static properties of the script:"
code_only="$work/script.code.sh"
{ grep -vE '^[[:space:]]*#' "$script" || true; } > "$code_only"
assert "no '|| true' in executable code" 0 "$(count '|| true' "$code_only")"
assert "no 'gh pr ready' / 'gh pr edit' porcelain" 0 "$( { grep -cE 'gh pr (ready|edit)' "$code_only" || true; } )"

# --- Mutation proofs ----------------------------------------------------------
if [ -z "${READY_BOT_MUTANT:-}" ]; then
  echo ""
  echo "Mutation proofs (each broken copy must be caught by its named case):"

  # mutate <name> <sed-expr> <case-substring-that-must-FAIL>
  mutate() {
    local name="$1" expr="$2" want="$3"
    local mutant="$work/mutant-$name.sh"
    sed -e "$expr" "$real_script" > "$mutant"
    # The sed must have changed exactly one line, or the "proof" proves nothing.
    local changed
    changed="$( { diff "$real_script" "$mutant" || true; } | { grep -c '^>' || true; } )"
    if [ "$changed" != "1" ]; then
      assert "mutant '$name': sed changed exactly one line" 1 "$changed"
      return
    fi
    local rc=0
    READY_BOT_MUTANT=1 READY_BOT_SCRIPT_UNDER_TEST="$mutant" \
      bash "${BASH_SOURCE[0]}" > "$work/mutant-$name.out" 2>&1 || rc=$?
    assert "mutant '$name': suite exits non-zero" 1 "$([ "$rc" -ne 0 ] && echo 1 || echo 0)"
    assert "mutant '$name': caught by '$want'" 1 \
      "$( { grep -F "FAIL  $want" "$work/mutant-$name.out" || true; } | head -n1 | wc -l | tr -d ' ')"
  }

  # (a) the measurement predicate made unconditional
  mutate unconditional-ready \
    's/if \[ "\$checks" -ge 1 \]; then/if true; then/' \
    "0 checks: mutation never called"
  # (b) `|| true` appended to the ready mutation call
  mutate mutation-or-true \
    "s/\(pullRequest.isDraft'\))\"; then/\1 || true)\"; then/" \
    "graphql fails: ::error:: names the failed call"
  # (b') the page post's failure exit replaced by a no-op
  mutate page-failure-swallowed \
    's/  gh_failed "gh api -X POST/  : "gh api -X POST/' \
    "comment post fails: exit code"
  # (c) the age comparison inverted
  mutate age-inverted \
    's/if \[ "\$age_seconds" -gt "\$budget_seconds" \]; then/if [ "$age_seconds" -le "$budget_seconds" ]; then/' \
    "stale draft: exactly one page across two runs"
  # (c) must ALSO be caught on the other side of the comparison.
  assert "mutant 'age-inverted': caught by 'within budget: no page'" 1 \
    "$( { grep -F "FAIL  within budget: no page" "$work/mutant-age-inverted.out" || true; } | head -n1 | wc -l | tr -d ' ')"
fi

echo ""
if [ "$failures" -ne 0 ]; then
  echo "FAILED: $failures assertion(s)"
  exit 1
fi
echo "All assertions passed."
