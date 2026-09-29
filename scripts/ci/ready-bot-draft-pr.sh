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
# The predicate is a count of GitHub ACTIONS check runs on the head commit
# (app slug `github-actions`) — the observable for "this PR's CI workflows
# fired" — never an inference from which token authored the PR, so it stays
# right if a secret is rotated. It is deliberately NOT the head's total check
# count: coord posts its own `Qontinui merge gate` run (app
# `qontinui-merge-orchestrator`) on the PRs it evaluates, and GitHub does not
# suppress APP webhooks for events a GITHUB_TOKEN causes (only workflow
# triggers are suppressed) — so coord can post that run on a PR whose own CI
# never fired, and a total_count >= 1 predicate would then ready exactly the
# zero-CI PR the draft exists to hold back. The count is summed over every
# page (`--paginate` emits one number per page).
#
# A draft that CONFLICTS with main (mergeable CONFLICTING, or mergeStateStatus
# DIRTY) is never readied — coord cannot land it — and with
# --close-if-obsolete a conflicting PR is closed instead (see OUTCOMES).
#
# With --expect-head, the head GitHub reports must equal the commit the caller
# just pushed before anything is decided: right after a force-push the lookup
# can still return the OLD head, whose checks say nothing about the new one.
#
# Only same-repository PRs are considered (`isCrossRepository` false): a fork
# PR whose branch happens to share the name is never readied or paged.
#
# Red or pending checks do NOT block the un-draft: coord
# gates on the head's merge state, so a ready PR with a red check simply does
# not land — the correct outcome, and not this script's concern.
#
# Plan: plans/2026-09-17-atlas-self-heal-files-its-own-fix-as-a-draft-and-nothing-lands-it.md
#
# USAGE
#   ready-bot-draft-pr.sh --repo <owner/name> --branch <head branch>
#                         [--max-age-hours N] [--wait-for-checks-seconds S]
#                         [--expect-head <40-hex sha>] [--close-if-obsolete]
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
#                             The same budget also bounds --expect-head's wait.
#   --expect-head SHA         the commit the caller just pushed. Re-poll the PR
#                             lookup (within the wait budget) until GitHub
#                             reports it as the head; if it never does, decide
#                             NOTHING and report head-mismatch.
#   --close-if-obsolete       the caller found main already current (no drift,
#                             no pin move). A PR (draft or ready) that
#                             conflicts with main then carries an obsolete
#                             change: comment (once, hidden marker) and close
#                             it. Nothing else is deleted — the branch stays.
#   --print-close-marker      print that comment's hidden marker and exit 0
#                             (callers match on it; takes no other argument).
#
# OUTCOMES (the LAST stdout line is exactly one of these; all exit 0):
#   no-pr                   no open PR for --branch against main
#   already-ready #<n>      the PR is not a draft (never touched, never paged)
#   readied #<n>            it was a draft with >= 1 Actions check run; now
#                           ready, verified by the mutation's OWN returned isDraft
#   draft-no-checks #<n>    a draft whose head carries 0 Actions check runs;
#                           left alone (and paged past --max-age-hours)
#   draft-conflicting #<n>  a draft that conflicts with main; never readied,
#                           ::warning::, paged past --max-age-hours like
#                           draft-no-checks (the page names the cause)
#   head-mismatch #<n>      --expect-head never became the reported head within
#                           the budget; nothing decided, with a ::warning::
#   closed-obsolete #<n>    --close-if-obsolete and the PR conflicts with main:
#                           commented on and closed
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
CLOSE_MARKER='<!-- ready-bot-draft-pr:closed-obsolete -->'

usage() {
  awk '/^# OUTCOMES/ { exit } /^# USAGE/ { p = 1 } p { sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
}

repo=""
branch=""
max_age_hours=""
wait_seconds="0"
expect_head=""
close_if_obsolete="false"
while [ $# -gt 0 ]; do
  case "$1" in
    --repo)                    repo="${2:-}"; shift 2 || { usage; exit 2; } ;;
    --branch)                  branch="${2:-}"; shift 2 || { usage; exit 2; } ;;
    --max-age-hours)           max_age_hours="${2:-}"; shift 2 || { usage; exit 2; } ;;
    --wait-for-checks-seconds) wait_seconds="${2:-}"; shift 2 || { usage; exit 2; } ;;
    --expect-head)             expect_head="${2:-}"; shift 2 || { usage; exit 2; } ;;
    --close-if-obsolete)       close_if_obsolete="true"; shift ;;
    # Prints the hidden marker an obsolete-close comment carries, and exits.
    # sibling-pin-bump.yml reads it from here (one definition, not two) to
    # tell a PR this script closed from one a maintainer closed.
    --print-close-marker)      printf '%s\n' "$CLOSE_MARKER"; exit 0 ;;
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
if [ -n "$expect_head" ] && ! [[ "$expect_head" =~ ^[0-9a-f]{40}$ ]]; then
  echo "::error::ready-bot-draft-pr.sh: --expect-head must be a 40-hex commit sha, got '$expect_head'."
  exit 2
fi
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
# `id` is the GraphQL node id the mutations take. Pre-flattened to one TSV line
# by gh's own --jq, so no separate jq is needed. `// empty` covers the
# genuinely-absent case; a gh FAILURE is not "no PR". Fork PRs
# (isCrossRepository) are filtered out before `.[0]`.
#
# lookup_pr sets pr_num is_draft head_sha node_id created_at pr_url author
# mergeable merge_status, or exits no-pr. Called from the MAIN shell so its
# exits end the script.
lookup_pr() {
  if ! line="$(gh pr list --repo "$repo" --head "$branch" --base main --state open \
      --json number,isDraft,headRefOid,id,createdAt,url,author,isCrossRepository,mergeable,mergeStateStatus \
      --jq '[.[] | select(.isCrossRepository == false)] | .[0] // empty | [.number, .isDraft, .headRefOid, .id, .createdAt, .url, (.author.login // "unknown"), .mergeable, .mergeStateStatus] | @tsv')"; then
    gh_failed "gh pr list --head $branch --state open"
  fi

  if [ -z "$line" ]; then
    echo "No open same-repository PR for '$branch' against main on $repo."
    emit no-pr "" "" "" "n/a"
    echo "no-pr"
    exit 0
  fi

  IFS=$'\t' read -r pr_num is_draft head_sha node_id created_at pr_url author mergeable merge_status <<< "$line"

  # Shape-check before acting on anything: a garbled read must not become a
  # mutation against the wrong node or a page with a nonsense age.
  case "$pr_num" in ""|*[!0-9]*) pr_num="" ;; esac
  case "$is_draft" in true|false) : ;; *) is_draft="" ;; esac
  case "${mergeable:-}" in MERGEABLE|CONFLICTING|UNKNOWN) : ;; *) mergeable="" ;; esac
  [[ "${merge_status:-}" =~ ^[A-Z_]+$ ]] || merge_status=""
  [[ "$head_sha" =~ ^[0-9a-f]{40}$ ]] || head_sha=""
  if [ -z "$pr_num" ] || [ -z "$is_draft" ] || [ -z "$head_sha" ] || [ -z "${node_id:-}" ] \
     || [ -z "${created_at:-}" ] || [ -z "$mergeable" ] || [ -z "$merge_status" ]; then
    echo "::error::ready-bot-draft-pr.sh: could not parse gh's PR lookup for '$branch': [$line]"
    exit 1
  fi
}

# A PR that conflicts with main can never land: coord will not propose it, and
# readying it only parks it in a slot it cannot use. UNKNOWN mergeability is
# NOT a conflict — GitHub computes it lazily after a push, so UNKNOWN is the
# normal first answer — and it does not block readying: if the PR does turn
# out to conflict, coord still refuses to land it, and the next run sees it.
is_conflicting() {
  [ "$mergeable" = "CONFLICTING" ] || [ "$merge_status" = "DIRTY" ]
}

# One poll budget shared by the head wait and the checks wait.
extra_polls=$(( wait_seconds / poll_interval ))

lookup_pr

# --- 2. Pin the head: is GitHub reporting the commit the caller pushed? -------
# Nothing below — close, ready, page — is decided on a head that is not the one
# the caller named.
if [ -n "$expect_head" ]; then
  while [ "$head_sha" != "$expect_head" ] && [ "$extra_polls" -gt 0 ]; do
    echo "PR #$pr_num still reports head $head_sha, not the expected $expect_head; re-polling in ${poll_interval}s ($extra_polls poll(s) left)."
    sleep "$poll_interval"
    extra_polls=$(( extra_polls - 1 ))
    lookup_pr
  done
  if [ "$head_sha" != "$expect_head" ]; then
    echo "::warning::PR #$pr_num still reports head $head_sha, not the expected commit $expect_head, after the wait budget. Deciding nothing on a head that is not the branch's; the next run re-decides."
    emit head-mismatch "$pr_num" "$pr_url" "" "n/a"
    echo "head-mismatch #$pr_num"
    exit 0
  fi
fi

# --- 3. An obsolete conflicting PR is closed (only when the caller says so) ---
# The caller passes --close-if-obsolete only on a run that found main already
# current (no drift / no pin move): a conflicting PR then carries a change main
# no longer needs, and nothing else will ever clear it. Draft or ready alike.
if [ "$close_if_obsolete" = "true" ] && is_conflicting; then
  echo "::warning::PR #$pr_num ($pr_url) conflicts with main ($mergeable/$merge_status) and this run found main already current, so its change is obsolete. Closing it; the next drift run opens a fresh one."
  close_comments="$(mktemp)"
  close_body="$(mktemp)"
  trap 'rm -f "$close_comments" "$close_body"' EXIT
  if ! gh api --paginate "repos/$repo/issues/$pr_num/comments" --jq '.[].body' > "$close_comments"; then
    gh_failed "gh api repos/$repo/issues/$pr_num/comments (read, before close)"
  fi
  if grep -qF "$CLOSE_MARKER" "$close_comments"; then
    echo "PR #$pr_num already carries the obsolete-close comment; not commenting again."
  else
    {
      echo "$CLOSE_MARKER"
      printf '**Closing: this bot-filed PR is obsolete.** It conflicts with `main` (`%s` / `%s`), and the scheduled run that swept it found `main` already current — no drift, no pin to move — so the change this PR carries is no longer needed.\n\n' "$mergeable" "$merge_status"
      printf 'Nothing is lost: the next run that finds drift rebuilds the branch from `main` and opens a fresh PR. The branch itself is left in place.\n'
    } > "$close_body"
    if ! gh api -X POST "repos/$repo/issues/$pr_num/comments" -F "body=@$close_body" --jq '.html_url'; then
      gh_failed "gh api -X POST repos/$repo/issues/$pr_num/comments (close)"
    fi
  fi
  if ! closed_state="$(gh api -X PATCH "repos/$repo/pulls/$pr_num" -f state=closed --jq '.state')"; then
    gh_failed "gh api -X PATCH repos/$repo/pulls/$pr_num state=closed"
  fi
  if [ "$closed_state" != "closed" ]; then
    echo "::error::ready-bot-draft-pr.sh: closing PR #$pr_num returned state=[$closed_state], not closed."
    exit 1
  fi
  emit closed-obsolete "$pr_num" "$pr_url" "" "n/a"
  echo "closed-obsolete #$pr_num"
  exit 0
fi

if [ "$is_draft" != "true" ]; then
  echo "PR #$pr_num ($pr_url) is already ready for review; nothing to do."
  emit already-ready "$pr_num" "$pr_url" "" "n/a"
  echo "already-ready #$pr_num"
  exit 0
fi

# --- 4. Decide: conflicting drafts are never readied; else measure CI --------
checks=""
park_reason=""
if is_conflicting; then
  echo "::warning::Draft PR #$pr_num ($pr_url) conflicts with main ($mergeable/$merge_status). Not readying it: coord cannot land a conflicting PR. It stays a draft until its branch is rebuilt."
  park_reason="conflicting"
else
  # Only app `github-actions` counts: another app's run (coord's merge gate
  # among them) says nothing about whether a workflow fired. --paginate
  # prints one count per page; they are summed, and each must be a number.
  count_checks() {
    if ! per_page="$(gh api --paginate "repos/$repo/commits/$head_sha/check-runs?per_page=100" \
        --jq '[.check_runs[] | select(.app.slug == "github-actions")] | length')"; then
      gh_failed "gh api repos/$repo/commits/$head_sha/check-runs"
    fi
    if [ -z "$per_page" ]; then
      echo "::error::ready-bot-draft-pr.sh: check-runs for $head_sha returned no page at all."
      exit 1
    fi
    checks=0
    while IFS= read -r n; do
      case "$n" in
        ""|*[!0-9]*)
          echo "::error::ready-bot-draft-pr.sh: check-runs for $head_sha returned a non-count page: [$n]"
          exit 1
          ;;
      esac
      checks=$(( checks + n ))
    done <<< "$per_page"
  }

  count_checks
  while [ "$checks" -eq 0 ] && [ "$extra_polls" -gt 0 ]; do
    echo "Draft PR #$pr_num head $head_sha carries no Actions check runs yet; re-polling in ${poll_interval}s ($extra_polls poll(s) left)."
    sleep "$poll_interval"
    extra_polls=$(( extra_polls - 1 ))
    count_checks
  done

  echo "PR #$pr_num ($pr_url): draft, head $head_sha, $checks Actions check run(s), mergeable $mergeable/$merge_status, author $author, created $created_at."

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
    echo "Marked PR #$pr_num ready for review: its head carries $checks Actions check run(s), so it is gated like any other PR."
    emit readied "$pr_num" "$pr_url" "$checks" "n/a"
    echo "readied #$pr_num"
    exit 0
  fi
  park_reason="no-checks"
fi

# --- 5. Still a draft (no Actions checks, or conflicting): bound the park -----
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
  if [ "$park_reason" = "conflicting" ]; then
    why="it conflicts with main ($mergeable/$merge_status)"
  else
    why="its head carries 0 Actions check runs"
  fi
  if [ "$age_seconds" -gt "$budget_seconds" ]; then
    echo "::warning::Bot-filed PR #$pr_num ($pr_url) has been a draft for ${age_hours}h (budget ${max_age_hours}h) and $why. Nothing un-drafts it; it is parked until someone acts."

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
        if [ "$park_reason" = "conflicting" ]; then
          printf '**This bot-filed PR has been a draft for %s hours and CONFLICTS with `main`** (`%s` / `%s`; head `%s`; budget %s hours). It was opened by `%s`.\n\n' \
            "$age_hours" "$mergeable" "$merge_status" "$head_sha" "$max_age_hours" "$author"
          printf '`scripts/ci/ready-bot-draft-pr.sh` never readies a conflicting PR — coord cannot land one. The branch is rebuilt from `main` on the next night that still finds drift; a night that finds `main` already current closes this PR as obsolete. If neither happens, it is parked until someone acts. This comment is posted once per PR.\n'
        else
          printf '**This bot-filed PR has been a draft for %s hours with %s Actions check runs on its head** (`%s`; budget %s hours). It was opened by `%s`.\n\n' \
            "$age_hours" "$checks" "$head_sha" "$max_age_hours" "$author"
          printf 'It was born a draft on purpose, and `scripts/ci/ready-bot-draft-pr.sh` marks it ready for review automatically once its head carries at least one GitHub Actions check run (runs from other apps, such as coord'"'"'s merge gate, do not count). **Nothing un-drafts a PR whose CI never fired**, and coord'"'"'s merge train does not propose drafts, so this fix is parked until someone acts.\n\n'
          printf 'A PR with no checks was usually authored with `GITHUB_TOKEN`, which by design fires no `pull_request` workflows. To unpark it, make its checks run (push to the branch with a token that fires workflows, or close and reopen the PR by hand); the next scheduled run then marks it ready. This comment is posted once per PR.\n'
        fi
        if [ -n "$run_line" ]; then
          printf '\n%s\n' "$run_line"
        fi
      } > "$page_body"
      if ! gh api -X POST "repos/$repo/issues/$pr_num/comments" -F "body=@$page_body" --jq '.html_url'; then
        gh_failed "gh api -X POST repos/$repo/issues/$pr_num/comments (page)"
      fi
      echo "Paged on PR #$pr_num: stale draft, ${age_hours}h old, $why."
      paged=true
    fi
  else
    echo "PR #$pr_num is ${age_hours}h old, within the ${max_age_hours}h draft budget; not paging."
  fi
fi

emit "draft-$park_reason" "$pr_num" "$pr_url" "$checks" "$paged"
echo "draft-$park_reason #$pr_num"
exit 0
