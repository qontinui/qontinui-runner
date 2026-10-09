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
#   1. It un-drafts ONLY on measurement: >= 1 GitHub ACTIONS check run on the
#      head, summed over every page. Coord's own `Qontinui merge gate` run is
#      posted on every PR coord evaluates, CI or not, and must not count.
#   2. With --expect-head, nothing is decided until GitHub reports the pushed
#      head; a head that never converges is `head-mismatch`, never `readied`.
#   3. The un-draft is verified by the mutation's OWN returned isDraft.
#   4. Every gh failure is a red with an ::error:: naming the call — never a
#      green with the fix undelivered.
#   5. A zero-check draft older than the budget is paged exactly ONCE (the
#      marker makes it idempotent across nights); within budget, or ready, it
#      is never paged. Fork PRs are never considered.
#   6. A draft that CONFLICTS with main is never readied (draft-conflicting,
#      paged like a zero-check draft); UNKNOWN mergeability does not block.
#      With --close-if-obsolete a conflicting PR — draft or ready — is
#      commented on once and closed; without it, nothing is closed.
#
# HOW THE STUB ANSWERS. For the PR lookup, the check-runs pages and the
# GraphQL mutation, the stub holds realistic JSON fixtures and applies the
# script's OWN --jq expression to them with a real `jq` — so the filters the
# script ships (the app-slug filter, the fork filter, the per-page count) are
# exercised, not assumed. Only the comment list is returned pre-filtered.
#
# MUTATION PROOFS. The last section re-runs this whole suite against
# deliberately broken copies of the script and asserts each copy is CAUGHT by
# the specific case that guards it (the child sets READY_BOT_MUTANT=1, which
# skips that section).
#
# Plan: plans/2026-09-17-atlas-self-heal-files-its-own-fix-as-a-draft-and-nothing-lands-it.md
#
# Run locally (needs jq):
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
command -v jq >/dev/null || { echo "::error::this test needs jq on PATH"; exit 1; }
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
fx="$work/fixtures"
mkdir -p "$bin" "$fx"

cat > "$bin/gh" <<'STUB'
#!/usr/bin/env bash
# Keys each call; logs the key to GH_STUB_LOG; fails any key listed in
# GH_STUB_FAIL like a real refusal (stderr + exit 1). For `list`, `check-runs`
# and `graphql` it applies the caller's --jq to JSON fixtures with a real jq.
# Fixtures live in GH_STUB_DIR: prs.<n>.json (the n-th lookup; the last one
# repeats), checks.<n> (one JSON page per line, the n-th check read; the last
# repeats).
key=""
jq_expr=""
method="GET"
prev=""
for a in "$@"; do
  [ "$prev" = "--jq" ] && jq_expr="$a"
  [ "$prev" = "-X" ] && method="$a"
  prev="$a"
done
case "${1:-}" in
  pr) [ "${2:-}" = "list" ] && key="list" ;;
  api)
    case "$*" in
      *"/pulls/"*)     [ "$method" = "PATCH" ] && key="pr-close" ;;
      *" graphql "*)   key="graphql" ;;
      *"/check-runs"*) key="check-runs" ;;
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
# nth <prefix> — the fixture for the n-th call of this key, last one repeating.
nth() {
  local n i f=""
  n="$(grep -c "^$key\$" "$GH_STUB_LOG")"
  for i in $(seq 1 "$n"); do
    [ -f "$GH_STUB_DIR/$1.$i" ] && f="$GH_STUB_DIR/$1.$i"
  done
  [ -n "$f" ] || { echo "gh stub: no $1 fixture" >&2; exit 96; }
  printf '%s' "$f"
}
case "$key" in
  list)
    jq -r "$jq_expr" "$(nth prs)"
    ;;
  check-runs)
    # --paginate: the --jq expression runs once PER PAGE.
    while IFS= read -r page; do
      printf '%s\n' "$page" | jq -r "$jq_expr"
    done < "$(nth checks)"
    ;;
  graphql)
    printf '{"data":{"markPullRequestReadyForReview":{"pullRequest":{"isDraft":%s}}}}' \
      "${GH_STUB_GRAPHQL_ISDRAFT:-false}" | jq -r "$jq_expr"
    ;;
  comments-read)
    [ -f "${GH_STUB_COMMENTS:?}" ] && cat "$GH_STUB_COMMENTS"
    ;;
  pr-close)
    printf '{"state":"%s"}' "${GH_STUB_CLOSE_STATE:-closed}" | jq -r "$jq_expr"
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
NEW_SHA="b58e334f4b58e334f4b58e334f4b58e334f4b58e"
NOW_EPOCH="$(date -u -d '2026-09-30T00:00:00Z' +%s)"
CREATED_STALE="2026-09-27T00:00:00Z"   # 72h before NOW: over a 48h budget
CREATED_FRESH="2026-09-29T00:00:00Z"   # 24h before NOW: within it
PR_URL="https://github.com/qontinui/qontinui-runner/pull/4242"

# pr <isDraft> <createdAt> [head] [isCrossRepository] [number] [mergeable]
#    [mergeStateStatus] — one PR object in the shape `gh pr list --json ...`
#    returns.
pr() {
  jq -nc --argjson d "$1" --arg c "$2" --arg h "${3:-$HEAD_SHA}" \
    --argjson x "${4:-false}" --argjson n "${5:-4242}" --arg u "$PR_URL" \
    --arg m "${6:-MERGEABLE}" --arg ms "${7:-CLEAN}" \
    '{number: $n, isDraft: $d, headRefOid: $h, id: ("PR_kwDOtest" + ($n|tostring)),
      createdAt: $c, url: $u, author: {login: "jspinak"}, isCrossRepository: $x,
      mergeable: $m, mergeStateStatus: $ms}'
}
# set_prs <array-json>... — the 1st, 2nd, ... lookup's answer.
set_prs() {
  rm -f "$fx"/prs.*
  local i=0 a
  for a in "$@"; do i=$((i + 1)); printf '%s\n' "$a" > "$fx/prs.$i"; done
}
# set_checks <spec>... — the 1st, 2nd, ... check read. A spec is
# `<actions>[:<coord>]`: that many github-actions runs plus that many runs from
# coord's merge orchestrator, split into pages of 100 exactly as the API does.
set_checks() {
  rm -f "$fx"/checks.*
  local i=0 spec a c
  for spec in "$@"; do
    i=$((i + 1))
    a="${spec%%:*}"
    c=0; [ "$spec" != "$a" ] && c="${spec#*:}"
    jq -nc --argjson a "$a" --argjson c "$c" '
      ([range($a) | {name: "ci", app: {slug: "github-actions"}}]
       + [range($c) | {name: "Qontinui merge gate", app: {slug: "qontinui-merge-orchestrator"}}]) as $all
      | if ($all | length) == 0 then {total_count: 0, check_runs: []}
        else ($all | _nwise(100) | {total_count: ($all | length), check_runs: .}) end
    ' > "$fx/checks.$i"
  done
}

# run_script <args...> — echoes the exit code. Leaves stdout+stderr in
# $work/out.txt, the gh key log in $work/gh.log, sleeps in $work/sleep.log and
# GITHUB_OUTPUT in $work/output.txt. The comment store ($work/comments.txt) is
# NOT reset here — idempotency spans runs.
run_script() {
  : > "$work/gh.log"
  : > "$work/sleep.log"
  : > "$work/output.txt"
  local rc=0
  (
    PATH="$bin:$PATH" \
    GH_STUB_LOG="$work/gh.log" \
    GH_STUB_DIR="$fx" \
    SLEEP_STUB_LOG="$work/sleep.log" \
    GH_STUB_COMMENTS="$work/comments.txt" \
    GH_STUB_FAIL="$S_FAIL" \
    GH_STUB_GRAPHQL_ISDRAFT="$S_GRAPHQL" \
    GH_STUB_CLOSE_STATE="${S_CLOSE_STATE:-closed}" \
    GITHUB_OUTPUT="$work/output.txt" \
    READY_BOT_NOW_EPOCH="$NOW_EPOCH" \
    READY_BOT_POLL_INTERVAL_SECONDS=15 \
    bash "$script" --repo qontinui/qontinui-runner --branch chore/schema-pg-sql-refresh "$@"
  ) > "$work/out.txt" 2>&1 || rc=$?
  echo "$rc"
}

# reset <prs-array-json> <check-spec>... — default knobs for a fresh case.
reset() {
  S_FAIL=""
  S_GRAPHQL="false"
  set_prs "$1"
  shift
  set_checks "$@"
  : > "$work/comments.txt"
}

last_line() { tail -n1 "$work/out.txt"; }
MARKER='<!-- ready-bot-draft-pr:stale-draft -->'
DRAFT_FRESH="[$(pr true "$CREATED_FRESH")]"
DRAFT_STALE="[$(pr true "$CREATED_STALE")]"

echo "Behavioural cases (script: $script):"

# 1. Draft with Actions checks -> readied, mutation exactly once, outputs.
reset "$DRAFT_FRESH" "23"
rc="$(run_script)"
assert "draft with checks: exit code" 0 "$rc"
assert "draft with checks: last line" "readied #4242" "$(last_line)"
assert "draft with checks: mutation called exactly once" 1 "$(count graphql "$work/gh.log")"
assert "draft with checks: result=readied in GITHUB_OUTPUT" 1 "$(count 'result=readied' "$work/output.txt")"
assert "draft with checks: pr=4242 in GITHUB_OUTPUT" 1 "$(count 'pr=4242' "$work/output.txt")"
assert "draft with checks: url in GITHUB_OUTPUT" 1 "$(count "url=$PR_URL" "$work/output.txt")"
assert "draft with checks: checks=23 in GITHUB_OUTPUT" 1 "$(count 'checks=23' "$work/output.txt")"

# 2. Draft with ZERO checks -> stays a draft; the mutation is never called.
reset "$DRAFT_FRESH" "0"
rc="$(run_script)"
assert "0 checks: exit code" 0 "$rc"
assert "0 checks: last line" "draft-no-checks #4242" "$(last_line)"
assert "0 checks: mutation never called" 0 "$(count graphql "$work/gh.log")"
assert "0 checks: no page without --max-age-hours" 0 "$(count comment "$work/gh.log")"

# 2b. The live shape of #1505/#1496 on a GITHUB_TOKEN PR: the ONLY run on the
#     head is coord's merge gate. No workflow fired, so it must stay a draft.
reset "$DRAFT_FRESH" "0:1"
rc="$(run_script)"
assert "only coord's merge gate: exit code" 0 "$rc"
assert "only coord's merge gate: stays a draft" "draft-no-checks #4242" "$(last_line)"
assert "only coord's merge gate: mutation never called" 0 "$(count graphql "$work/gh.log")"

# 2c. Counts are SUMMED across pages: 150 Actions runs + coord's = 2 pages.
reset "$DRAFT_FRESH" "150:1"
rc="$(run_script)"
assert "paginated checks: readied" "readied #4242" "$(last_line)"
assert "paginated checks: summed across pages (checks=150)" 1 "$(count 'checks=150' "$work/output.txt")"

# 3. The mutation answers but the PR is STILL a draft -> red.
reset "$DRAFT_FRESH" "5"
S_GRAPHQL="true"
rc="$(run_script)"
assert "mutation returns isDraft=true: exit code" 1 "$rc"
assert "mutation returns isDraft=true: ::error:: says still a draft" 1 "$(count 'The PR is still a draft' "$work/out.txt")"
assert "mutation returns isDraft=true: never prints readied" 0 "$(count 'readied #' "$work/out.txt")"

# 4. gh failures, one per call: each is a red with an ::error:: naming it.
reset "$DRAFT_FRESH" "5"
S_FAIL="list"
rc="$(run_script)"
assert "list fails: exit code" 1 "$rc"
assert "list fails: ::error:: names the call" 1 "$(count "::error::ready-bot-draft-pr.sh: 'gh pr list" "$work/out.txt")"
assert "list fails: never reads as no-pr" 0 "$(count 'no-pr' "$work/out.txt")"

reset "$DRAFT_FRESH" "5"
S_FAIL="check-runs"
rc="$(run_script)"
assert "check-runs fails: exit code" 1 "$rc"
assert "check-runs fails: ::error:: names the call" 1 "$(count "check-runs' failed" "$work/out.txt")"
assert "check-runs fails: mutation never called" 0 "$(count graphql "$work/gh.log")"

reset "$DRAFT_FRESH" "5"
S_FAIL="graphql"
rc="$(run_script)"
assert "graphql fails: exit code" 1 "$rc"
assert "graphql fails: ::error:: names the failed call" 1 "$(count "markPullRequestReadyForReview (PR #4242)' failed" "$work/out.txt")"

reset "$DRAFT_STALE" "0"
S_FAIL="comments-read"
rc="$(run_script --max-age-hours 48)"
assert "comments read fails: exit code" 1 "$rc"
assert "comments read fails: ::error:: names the call" 1 "$(count "comments (read)' failed" "$work/out.txt")"
assert "comments read fails: never posts blind" 0 "$(count comment-post "$work/gh.log")"

reset "$DRAFT_STALE" "0"
S_FAIL="comment-post"
rc="$(run_script --max-age-hours 48)"
assert "comment post fails: exit code" 1 "$rc"
assert "comment post fails: ::error:: names the call" 1 "$(count "comments (page)' failed" "$work/out.txt")"

# 5. No open PR -> no-pr, and nothing else is called.
reset "[]" "5"
rc="$(run_script)"
assert "no PR: exit code" 0 "$rc"
assert "no PR: last line" "no-pr" "$(last_line)"
assert "no PR: only the lookup was called" "list" "$(tr '\n' ' ' < "$work/gh.log" | sed 's/ $//')"

# 5b. Fork PRs are never considered: a cross-repo PR alone reads as no-pr, and
#     beside a same-repo PR the same-repo one is chosen.
reset "[$(pr true "$CREATED_STALE" "$HEAD_SHA" true 9999)]" "5"
rc="$(run_script --max-age-hours 48)"
assert "fork PR only: no-pr" "no-pr" "$(last_line)"
assert "fork PR only: mutation never called" 0 "$(count graphql "$work/gh.log")"
reset "[$(pr true "$CREATED_FRESH" "$HEAD_SHA" true 9999), $(pr true "$CREATED_FRESH")]" "5"
rc="$(run_script)"
assert "fork PR listed first: the same-repo PR is readied" "readied #4242" "$(last_line)"

# 6. Already-ready PR -> untouched: no check read, no mutation.
reset "[$(pr false "$CREATED_FRESH")]" "5"
rc="$(run_script)"
assert "ready PR: exit code" 0 "$rc"
assert "ready PR: last line" "already-ready #4242" "$(last_line)"
assert "ready PR: mutation never called" 0 "$(count graphql "$work/gh.log")"
assert "ready PR: checks never read" 0 "$(count check-runs "$work/gh.log")"

# 7. Stale zero-check draft -> paged exactly ONCE across two runs.
reset "$DRAFT_STALE" "0:1"
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
assert "stale draft: page names the Actions check count" 1 "$(count 'with 0 Actions check runs' "$work/comments.txt")"
assert "stale draft: page says nothing un-drafts it" 1 "$(count 'Nothing un-drafts a PR whose CI never fired' "$work/comments.txt")"
assert "stale draft: still reports draft-no-checks" "draft-no-checks #4242" "$(last_line)"

# 8. Zero-check draft within budget -> no page, comments never read.
reset "$DRAFT_FRESH" "0"
rc="$(run_script --max-age-hours 48)"
assert "within budget: exit code" 0 "$rc"
assert "within budget: no page" 0 "$(count "$MARKER" "$work/comments.txt")"
assert "within budget: no comment call at all" 0 "$(count comment "$work/gh.log")"
assert "within budget: paged=false" 1 "$(count 'paged=false' "$work/output.txt")"

# 9. Ready PR of any age -> never paged.
reset "[$(pr false "2025-01-01T00:00:00Z")]" "0"
rc="$(run_script --max-age-hours 48)"
assert "ancient ready PR: exit code" 0 "$rc"
assert "ancient ready PR: no page" 0 "$(count "$MARKER" "$work/comments.txt")"
assert "ancient ready PR: no comment call" 0 "$(count comment "$work/gh.log")"

# 10. A stale draft that DOES carry checks is readied, not paged.
reset "$DRAFT_STALE" "3"
rc="$(run_script --max-age-hours 48)"
assert "stale draft with checks: readied" "readied #4242" "$(last_line)"
assert "stale draft with checks: no page" 0 "$(count comment "$work/gh.log")"

# 11. --wait-for-checks-seconds: checks registering late are waited for...
reset "$DRAFT_FRESH" "0:1" "0:1" "2:1"
rc="$(run_script --wait-for-checks-seconds 300)"
assert "late checks: exit code" 0 "$rc"
assert "late checks: readied after they register" "readied #4242" "$(last_line)"
assert "late checks: three check reads" 3 "$(count check-runs "$work/gh.log")"
assert "late checks: slept twice" 2 "$(count 'sleep 15' "$work/sleep.log")"
# ...and the wait is BOUNDED: 30s / 15s = 2 extra polls, then decide.
reset "$DRAFT_FRESH" "0"
rc="$(run_script --wait-for-checks-seconds 30)"
assert "no checks ever: exit code" 0 "$rc"
assert "no checks ever: draft-no-checks" "draft-no-checks #4242" "$(last_line)"
assert "no checks ever: bounded to 1 + 2 check reads" 3 "$(count check-runs "$work/gh.log")"
assert "no checks ever: mutation never called" 0 "$(count graphql "$work/gh.log")"

# 12. --expect-head. Right after a force-push the lookup can still report the
#     OLD head — which carries checks that say nothing about the new commit.
reset "$DRAFT_FRESH" "5"
rc="$(run_script --expect-head "$NEW_SHA" --wait-for-checks-seconds 30)"
assert "stale head never converges: exit code" 0 "$rc"
assert "stale head never converges: head-mismatch" "head-mismatch #4242" "$(last_line)"
assert "stale head never converges: mutation never called" 0 "$(count graphql "$work/gh.log")"
assert "stale head never converges: checks never read" 0 "$(count check-runs "$work/gh.log")"
assert "stale head never converges: ::warning::" 1 "$(count '::warning::PR #4242 still reports head' "$work/out.txt")"
assert "stale head never converges: bounded to 1 + 2 lookups" 3 "$(count list "$work/gh.log")"
# ...and when GitHub catches up within the budget, it is decided normally.
reset "$DRAFT_FRESH" "5"
set_prs "$DRAFT_FRESH" "[$(pr true "$CREATED_FRESH" "$NEW_SHA")]"
rc="$(run_script --expect-head "$NEW_SHA" --wait-for-checks-seconds 30)"
assert "head converges: readied" "readied #4242" "$(last_line)"
assert "head converges: two lookups" 2 "$(count list "$work/gh.log")"
# A head that already matches costs no extra lookup.
reset "$DRAFT_FRESH" "5"
rc="$(run_script --expect-head "$HEAD_SHA")"
assert "head already pushed one: readied" "readied #4242" "$(last_line)"
assert "head already pushed one: one lookup" 1 "$(count list "$work/gh.log")"

# 14. Conflicting PRs. A draft that conflicts with main is never readied —
#     coord cannot land it — even with green CI; UNKNOWN does not block.
CONFLICT_DRAFT="[$(pr true "$CREATED_FRESH" "$HEAD_SHA" false 4242 CONFLICTING DIRTY)]"
reset "$CONFLICT_DRAFT" "23"
rc="$(run_script)"
assert "conflicting draft w/ checks: exit code" 0 "$rc"
assert "conflicting draft w/ checks: not readied" "draft-conflicting #4242" "$(last_line)"
assert "conflicting draft w/ checks: mutation never called" 0 "$(count graphql "$work/gh.log")"
assert "conflicting draft w/ checks: ::warning::" 1 "$(count '::warning::Draft PR #4242' "$work/out.txt")"
assert "conflicting draft w/ checks: result=draft-conflicting" 1 "$(count 'result=draft-conflicting' "$work/output.txt")"
reset "[$(pr true "$CREATED_FRESH" "$HEAD_SHA" false 4242 MERGEABLE DIRTY)]" "23"
rc="$(run_script)"
assert "DIRTY draft w/ checks: not readied" "draft-conflicting #4242" "$(last_line)"
reset "[$(pr true "$CREATED_FRESH" "$HEAD_SHA" false 4242 UNKNOWN UNKNOWN)]" "23"
rc="$(run_script)"
assert "UNKNOWN-mergeable draft w/ checks: readied" "readied #4242" "$(last_line)"
# ...and a conflicting draft parked past the budget is paged, naming the cause.
reset "[$(pr true "$CREATED_STALE" "$HEAD_SHA" false 4242 CONFLICTING DIRTY)]" "23"
run_script --max-age-hours 48 > /dev/null
run_script --max-age-hours 48 > /dev/null
assert "stale conflicting draft: exactly one page across two runs" 1 "$(count "$MARKER" "$work/comments.txt")"
assert "stale conflicting draft: page names the conflict" 1 "$(count 'CONFLICTS with `main`' "$work/comments.txt")"
assert "stale conflicting draft: page is not the no-checks text" 0 "$(count 'Nothing un-drafts a PR whose CI never fired' "$work/comments.txt")"

# 15. --close-if-obsolete: a conflicting PR (ready or draft) is closed, with
#     one comment across runs; without the flag nothing is closed; a
#     mergeable PR is never closed by it.
CLOSE_MARKER='<!-- ready-bot-draft-pr:closed-obsolete -->'
CONFLICT_READY="[$(pr false "$CREATED_STALE" "$HEAD_SHA" false 4242 CONFLICTING DIRTY)]"
reset "$CONFLICT_READY" "23"
rc="$(run_script --close-if-obsolete)"
assert "conflicting ready + flag: exit code" 0 "$rc"
assert "conflicting ready + flag: closed" "closed-obsolete #4242" "$(last_line)"
assert "conflicting ready + flag: close call made once" 1 "$(count pr-close "$work/gh.log")"
run_script --close-if-obsolete > /dev/null
assert "conflicting ready + flag: one close comment across two runs" 1 "$(count "$CLOSE_MARKER" "$work/comments.txt")"
assert "conflicting ready + flag: comment says main is already current" 1 "$(count 'found `main` already current' "$work/comments.txt")"
reset "$CONFLICT_READY" "23"
rc="$(run_script)"
assert "conflicting ready, no flag: already-ready" "already-ready #4242" "$(last_line)"
assert "conflicting ready, no flag: never closed" 0 "$(count pr-close "$work/gh.log")"
assert "conflicting ready, no flag: no comment" 0 "$(count comment "$work/gh.log")"
reset "$CONFLICT_DRAFT" "23"
rc="$(run_script --close-if-obsolete)"
assert "conflicting draft + flag: closed" "closed-obsolete #4242" "$(last_line)"
# With the flag main is already current, so EVERY open bot PR is obsolete —
# a mergeable one too: readying it could land stale content.
reset "[$(pr false "$CREATED_FRESH")]" "23"
rc="$(run_script --close-if-obsolete)"
assert "non-conflicting ready + flag: closed" "closed-obsolete #4242" "$(last_line)"
assert "non-conflicting ready + flag: close call made once" 1 "$(count pr-close "$work/gh.log")"
reset "$DRAFT_FRESH" "23"
rc="$(run_script --close-if-obsolete)"
assert "non-conflicting draft w/ checks + flag: closed" "closed-obsolete #4242" "$(last_line)"
assert "non-conflicting draft w/ checks + flag: never readied" 0 "$(count graphql "$work/gh.log")"
reset "$DRAFT_FRESH" "23"
rc="$(run_script)"
assert "non-conflicting draft w/ checks, no flag: readied, not closed" "readied #4242 0" "$(last_line) $(count pr-close "$work/gh.log")"
reset "[$(pr true "$CREATED_FRESH")]" "23"
rc="$(run_script --close-if-obsolete --expect-head "$NEW_SHA")"
assert "flag + stale head: head pin first, nothing closed" "head-mismatch #4242 0" "$(last_line) $(count pr-close "$work/gh.log")"
reset "$CONFLICT_READY" "23"
S_FAIL="pr-close"
rc="$(run_script --close-if-obsolete)"
assert "close refused: exit code" 1 "$rc"
assert "close refused: ::error:: names the call" 1 "$(count "state=closed' failed" "$work/out.txt")"
reset "$CONFLICT_READY" "23"
S_CLOSE_STATE="open"
rc="$(run_script --close-if-obsolete)"
S_CLOSE_STATE="closed"
assert "close returns state=open: exit code" 1 "$rc"
assert "close returns state=open: never reports closed" 0 "$(count 'closed-obsolete' "$work/out.txt")"

# 13. Garbled lookup / usage errors are reds, not guesses.
reset '[{"number": 4242, "isDraft": "maybe", "headRefOid": "nothex", "isCrossRepository": false}]' "5"
rc="$(run_script)"
assert "garbled lookup: exit code" 1 "$rc"
assert "garbled lookup: mutation never called" 0 "$(count graphql "$work/gh.log")"
rc=0
( PATH="$bin:$PATH" GH_STUB_LOG="$work/gh.log" bash "$script" --branch x ) > "$work/out.txt" 2>&1 || rc=$?
assert "missing --repo: usage exit code" 2 "$rc"
rc=0
( PATH="$bin:$PATH" GH_STUB_LOG="$work/gh.log" bash "$script" --repo a/b --branch x --max-age-hours 4x ) > "$work/out.txt" 2>&1 || rc=$?
assert "non-numeric --max-age-hours: usage exit code" 2 "$rc"
rc=0
( PATH="$bin:$PATH" GH_STUB_LOG="$work/gh.log" bash "$script" --repo a/b --branch x --expect-head HEAD ) > "$work/out.txt" 2>&1 || rc=$?
assert "non-sha --expect-head: usage exit code" 2 "$rc"

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
    caught "$name" "$want"
  }
  # caught <mutant> <case> — that case FAILED in the mutant's run.
  caught() {
    assert "mutant '$1': caught by '$2'" 1 \
      "$( { grep -F "FAIL  $2" "$work/mutant-$1.out" || true; } | head -n1 | wc -l | tr -d ' ')"
  }

  # (a) the measurement predicate made unconditional
  mutate unconditional-ready \
    's/if \[ "\$checks" -ge 1 \]; then/if true; then/' \
    "0 checks: mutation never called"
  # (a') the Actions-only filter dropped: coord's merge gate would count
  mutate app-filter-dropped \
    's/\[\.check_runs\[\] | select(\.app\.slug == "github-actions")\] | length/.check_runs | length/' \
    "only coord's merge gate: mutation never called"
  # (b) `|| true` appended to the ready mutation call
  mutate mutation-or-true \
    "s/\(pullRequest.isDraft'\))\"; then/\1 || true)\"; then/" \
    "graphql fails: ::error:: names the failed call"
  # (b') the page post's failure exit replaced by a no-op
  mutate page-failure-swallowed \
    's/gh_failed \("gh api -X POST [^"]*(page)"\)/: \1/' \
    "comment post fails: exit code"
  # (c) the age comparison inverted
  mutate age-inverted \
    's/if \[ "\$age_seconds" -gt "\$budget_seconds" \]; then/if [ "$age_seconds" -le "$budget_seconds" ]; then/' \
    "stale draft: exactly one page across two runs"
  caught age-inverted "within budget: no page"
  # (d) the head comparison skipped: a stale head's checks would be trusted
  mutate head-check-skipped \
    's/  if \[ "\$head_sha" != "\$expect_head" \]; then/  if false; then/' \
    "stale head never converges: mutation never called"
  # (f) the conflicting check dropped from the ready branch
  mutate conflict-check-dropped \
    's/^if is_conflicting; then/if false; then/' \
    "conflicting draft w/ checks: not readied"
  # (g) the close branch never taken
  mutate close-branch-dropped \
    's/^if \[ "\$close_if_obsolete" = "true" \]; then/if false; then/' \
    "conflicting ready + flag: closed"
  # (h) the close restricted to conflicting PRs again (round-3 N5)
  mutate close-conflicting-only \
    's/^if \[ "\$close_if_obsolete" = "true" \]; then/if [ "$close_if_obsolete" = "true" ] \&\& is_conflicting; then/' \
    "non-conflicting ready + flag: closed"
  # (e) the fork filter dropped
  mutate fork-filter-dropped \
    's/\[\.\[\] | select(\.isCrossRepository == false)\] | \.\[0\]/.[0]/' \
    "fork PR only: no-pr"
fi

echo ""
if [ "$failures" -ne 0 ]; then
  echo "FAILED: $failures assertion(s)"
  exit 1
fi
echo "All assertions passed."
