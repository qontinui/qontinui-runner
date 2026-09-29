#!/usr/bin/env bash
# shellcheck disable=SC2016  # literal `$` and backticks (GraphQL variables, markdown, sed patterns) are intended
# ready-bot-draft-pr.sh — promote a bot-filed DRAFT pull request to ready for
# review once its head demonstrably carries checks, and page when one stays
# parked as a draft past a budget.
#
# WHY THIS EXISTS. Two scheduled self-heal workflows —
# schema-pg-sql-freshness-nightly.yml and sibling-pin-bump.yml — open their
# fix PR with `gh pr create --draft`, on purpose: a PR authored with
# GITHUB_TOKEN fires no `pull_request` workflows, so it would carry NO checks,
# and a no-checks PR marked ready is a merge-train candidate with nothing to
# gate on. That safety property is real. But nothing ever re-decided it:
# coord's merge train does not propose drafts, so every such fix parked until a
# human noticed (#1505 and #1496 sat 17 and 18 days, each with 23-25 green
# check runs on its head). This script is the re-decision, made by MEASUREMENT
# rather than by a blanket default: a draft whose head carries >= 1 check run
# is marked ready; a draft whose head carries none stays a draft — exactly the
# case the draft was written for — and, with --max-age-hours, is paged once.
#
# The predicate is a count of check runs on the head commit (the observable),
# never an inference from which token authored the PR, so it stays right if a
# secret is rotated. Red or pending checks do NOT block the un-draft: coord
# gates on the head's merge state, so a ready PR with a red check simply does
# not land — the correct outcome, and not this script's concern.
#
# Plan: plans/2026-09-17-atlas-self-heal-files-its-own-fix-as-a-draft-and-nothing-lands-it.md
#
# USAGE
#   ready-bot-draft-pr.sh --repo <owner/name> --branch <head branch>
#                         [--max-age-hours N] [--wait-for-checks-seconds S]
#
#   --max-age-hours N         page (one PR comment, idempotent on a hidden
#                             marker, plus a ::warning::) when the PR is still
#                             a draft after this run and was created more than
#                             N hours ago. A ready PR of any age never pages.
#   --wait-for-checks-seconds S
#                             when the draft's head has no check runs yet,
#                             re-poll every READY_BOT_POLL_INTERVAL_SECONDS
#                             (default 15) for up to S seconds before deciding
#                             "no checks". For the call made right after a
#                             force-push: a PAT push fires `pull_request`
#                             workflows, but their check runs register seconds
#                             later, and a push-then-count with no wait would
#                             read 0 on every night the branch moves. Default 0.
#
# OUTCOMES (the LAST stdout line is exactly one of these; all exit 0):
#   no-pr                   no open PR for --branch against main
#   already-ready #<n>      the PR is not a draft (never touched, never paged)
#   readied #<n>            it was a draft with >= 1 check run; now ready,
#                           verified by the mutation's OWN returned isDraft
#   draft-no-checks #<n>    a draft whose head carries 0 check runs; left alone
#
# When $GITHUB_OUTPUT is set, also writes result=<word>, pr=<n>, url=<url>,
# checks=<count> and paged=<true|false|already|n/a> to it.
#
# FAILURE POLICY. Any `gh` failure prints ::error:: naming the call and exits
# 1; a usage error exits 2. There is deliberately no `|| true` on any gh call:
# a suppressed failure ends a nightly GREEN with the fix undelivered, which is
# strictly worse than a red.
#
# Un-drafting goes through the explicit GraphQL mutation, never `gh pr ready`
# or `gh pr edit`: that porcelain has been measured failing on a
# Projects-classic prefetch before writing anything, and the mutation returns
# the post-state to assert on.
#
# TEST SEAMS: `gh` and `sleep` are resolved through PATH (stubbable), and
# READY_BOT_NOW_EPOCH overrides "now" for the age computation.
# Tests: scripts/tests/test_ready_bot_draft_pr.sh

set -euo pipefail

PAGE_MARKER='<!-- ready-bot-draft-pr:stale-draft -->'

usage() {
  awk '/^# OUTCOMES/ { exit } /^# USAGE/ { p = 1 } p { sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
}

repo=""
branch=""
max_age_hours=""
wait_seconds="0"
while [ $# -gt 0 ]; do
  case "$1" in
    --repo)                    repo="${2:-}"; shift 2 || { usage; exit 2; } ;;
    --branch)                  branch="${2:-}"; shift 2 || { usage; exit 2; } ;;
    --max-age-hours)           max_age_hours="${2:-}"; shift 2 || { usage; exit 2; } ;;
    --wait-for-checks-seconds) wait_seconds="${2:-}"; shift 2 || { usage; exit 2; } ;;
    -h|--help)                 usage; exit 0 ;;
    *)
      echo "::error::ready-bot-draft-pr.sh: unknown argument '$1'."
      usage
      exit 2
      ;;
  esac
done

case "$repo" in
  ?*/?*) : ;;
  *) echo "::error::ready-bot-draft-pr.sh: --repo must be <owner>/<name>, got '$repo'."; exit 2 ;;
esac
if [ -z "$branch" ]; then
  echo "::error::ready-bot-draft-pr.sh: --branch is required."
  exit 2
fi
case "$max_age_hours" in
  "") : ;;
  *[!0-9]*) echo "::error::ready-bot-draft-pr.sh: --max-age-hours must be a whole number of hours, got '$max_age_hours'."; exit 2 ;;
esac
case "$wait_seconds" in
  ""|*[!0-9]*) echo "::error::ready-bot-draft-pr.sh: --wait-for-checks-seconds must be a whole number, got '$wait_seconds'."; exit 2 ;;
esac
poll_interval="${READY_BOT_POLL_INTERVAL_SECONDS:-15}"
case "$poll_interval" in
  ""|0|*[!0-9]*) echo "::error::ready-bot-draft-pr.sh: READY_BOT_POLL_INTERVAL_SECONDS must be a positive whole number, got '$poll_interval'."; exit 2 ;;
esac

# gh_failed <call> — the one reporter for every gh failure. Called from the MAIN
# shell (never inside a command substitution), so its `exit 1` ends the script.
gh_failed() {
  echo "::error::ready-bot-draft-pr.sh: '$1' failed for $repo (head branch '$branch'); gh's own error is above."
  echo "::error::Not treating an unanswered call as an answer: the PR's draft/ready state was NOT decided by this run."
  exit 1
}

# emit <result> <pr> <url> <checks> <paged> — machine-readable outcome.
emit() {
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
      echo "result=$1"
      echo "pr=$2"
      echo "url=$3"
      echo "checks=$4"
      echo "paged=$5"
    } >> "$GITHUB_OUTPUT"
  fi
}

# --- 1. The open PR for this branch -------------------------------------------
# `id` is the GraphQL node id the ready mutation takes. Pre-flattened to one TSV
# line by gh's own --jq, so no separate jq is needed. `// empty` covers the
# genuinely-absent case; a gh FAILURE is not "no PR".
if ! line="$(gh pr list --repo "$repo" --head "$branch" --base main --state open \
    --json number,isDraft,headRefOid,id,createdAt,url,author \
    --jq '.[0] // empty | [.number, .isDraft, .headRefOid, .id, .createdAt, .url, (.author.login // "unknown")] | @tsv')"; then
  gh_failed "gh pr list --head $branch --state open"
fi

if [ -z "$line" ]; then
  echo "No open PR for '$branch' against main on $repo."
  emit no-pr "" "" "" "n/a"
  echo "no-pr"
  exit 0
fi

IFS=$'\t' read -r pr_num is_draft head_sha node_id created_at pr_url author <<< "$line"

# Shape-check before acting on anything: a garbled read must not become a
# mutation against the wrong node or a page with a nonsense age.
case "$pr_num" in ""|*[!0-9]*) pr_num="" ;; esac
case "$is_draft" in true|false) : ;; *) is_draft="" ;; esac
[[ "$head_sha" =~ ^[0-9a-f]{40}$ ]] || head_sha=""
if [ -z "$pr_num" ] || [ -z "$is_draft" ] || [ -z "$head_sha" ] || [ -z "${node_id:-}" ] || [ -z "${created_at:-}" ]; then
  echo "::error::ready-bot-draft-pr.sh: could not parse gh's PR lookup for '$branch': [$line]"
  exit 1
fi

if [ "$is_draft" != "true" ]; then
  echo "PR #$pr_num ($pr_url) is already ready for review; nothing to do."
  emit already-ready "$pr_num" "$pr_url" "" "n/a"
  echo "already-ready #$pr_num"
  exit 0
fi

# --- 2. Measure: how many check runs does the draft's head carry? -------------
count_checks() {
  if ! checks="$(gh api "repos/$repo/commits/$head_sha/check-runs" --jq '.total_count')"; then
    gh_failed "gh api repos/$repo/commits/$head_sha/check-runs"
  fi
  case "$checks" in
    ""|*[!0-9]*)
      echo "::error::ready-bot-draft-pr.sh: check-runs for $head_sha returned a non-count total_count: [$checks]"
      exit 1
      ;;
  esac
}

extra_polls=$(( wait_seconds / poll_interval ))
count_checks
while [ "$checks" -eq 0 ] && [ "$extra_polls" -gt 0 ]; do
  echo "Draft PR #$pr_num head $head_sha carries no check runs yet; re-polling in ${poll_interval}s ($extra_polls poll(s) left)."
  sleep "$poll_interval"
  extra_polls=$(( extra_polls - 1 ))
  count_checks
done

echo "PR #$pr_num ($pr_url): draft, head $head_sha, $checks check run(s), author $author, created $created_at."

# --- 3. Decide by measurement -------------------------------------------------
ready_mutation='mutation($id:ID!){markPullRequestReadyForReview(input:{pullRequestId:$id}){pullRequest{isDraft}}}'
if [ "$checks" -ge 1 ]; then
  if ! is_draft_after="$(gh api graphql -f query="$ready_mutation" -f id="$node_id" --jq '.data.markPullRequestReadyForReview.pullRequest.isDraft')"; then
    gh_failed "gh api graphql markPullRequestReadyForReview (PR #$pr_num)"
  fi
  # Assert on the mutation's OWN returned post-state, not on a later re-read.
  if [ "$is_draft_after" != "false" ]; then
    echo "::error::ready-bot-draft-pr.sh: markPullRequestReadyForReview on PR #$pr_num returned isDraft=[$is_draft_after], not false. The PR is still a draft; refusing to report it readied."
    exit 1
  fi
  echo "Marked PR #$pr_num ready for review: its head carries $checks check run(s), so it is gated like any other PR."
  emit readied "$pr_num" "$pr_url" "$checks" "n/a"
  echo "readied #$pr_num"
  exit 0
fi

# --- 4. Still a draft with zero checks: bound the parked state ----------------
paged="n/a"
if [ -n "$max_age_hours" ]; then
  now_epoch="${READY_BOT_NOW_EPOCH:-$(date +%s)}"
  if ! created_epoch="$(date -d "$created_at" +%s)"; then
    echo "::error::ready-bot-draft-pr.sh: could not parse PR #$pr_num createdAt [$created_at] as a date."
    exit 1
  fi
  age_seconds=$(( now_epoch - created_epoch ))
  age_hours=$(( age_seconds / 3600 ))
  budget_seconds=$(( max_age_hours * 3600 ))
  paged=false
  if [ "$age_seconds" -gt "$budget_seconds" ]; then
    echo "::warning::Bot-filed PR #$pr_num ($pr_url) has been a draft for ${age_hours}h (budget ${max_age_hours}h) with 0 check runs on its head. Nothing un-drafts a zero-check PR; it is parked until someone acts."

    # Materialize the comments before grepping, never `gh ... | grep -q`: grep
    # exits on the first hit, SIGPIPEs gh, and under pipefail that turns a
    # successful dedup HIT into a failure. --paginate on the REST endpoint has
    # no page cap, so the marker cannot scroll out of view.
    comments="$(mktemp)"
    trap 'rm -f "$comments" "${page_body:-}"' EXIT
    if ! gh api --paginate "repos/$repo/issues/$pr_num/comments" --jq '.[].body' > "$comments"; then
      gh_failed "gh api repos/$repo/issues/$pr_num/comments (read)"
    fi
    if grep -qF "$PAGE_MARKER" "$comments"; then
      echo "PR #$pr_num already carries the stale-draft page; not commenting again."
      paged=already
    else
      page_body="$(mktemp)"
      run_line=""
      if [ -n "${GITHUB_SERVER_URL:-}" ] && [ -n "${GITHUB_REPOSITORY:-}" ] && [ -n "${GITHUB_RUN_ID:-}" ]; then
        run_line="Paged by run: $GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID"
      fi
      {
        echo "$PAGE_MARKER"
        printf '**This bot-filed PR has been a draft for %s hours with %s check runs on its head** (`%s`; budget %s hours). It was opened by `%s`.\n\n' \
          "$age_hours" "$checks" "$head_sha" "$max_age_hours" "$author"
        printf 'It was born a draft on purpose, and `scripts/ci/ready-bot-draft-pr.sh` marks it ready for review automatically once its head carries at least one check run. **Nothing un-drafts a zero-check PR**, and coord'"'"'s merge train does not propose drafts, so this fix is parked until someone acts.\n\n'
        printf 'A PR with no checks was usually authored with `GITHUB_TOKEN`, which by design fires no `pull_request` workflows. To unpark it, make its checks run (push to the branch with a token that fires workflows, or close and reopen the PR by hand); the next scheduled run then marks it ready. This comment is posted once per PR.\n'
        if [ -n "$run_line" ]; then
          printf '\n%s\n' "$run_line"
        fi
      } > "$page_body"
      if ! gh api -X POST "repos/$repo/issues/$pr_num/comments" -F "body=@$page_body" --jq '.html_url'; then
        gh_failed "gh api -X POST repos/$repo/issues/$pr_num/comments (page)"
      fi
      echo "Paged on PR #$pr_num: stale draft, ${age_hours}h old, 0 check runs."
      paged=true
    fi
  else
    echo "PR #$pr_num is ${age_hours}h old, within the ${max_age_hours}h draft budget; not paging."
  fi
fi

emit draft-no-checks "$pr_num" "$pr_url" "$checks" "$paged"
echo "draft-no-checks #$pr_num"
exit 0
