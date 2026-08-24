#!/usr/bin/env bash
# Detector: consecutive FAILING `schedule` runs on the default branch, for
# workflows that have NO push baseline on that branch.
#
# WHY THIS EXISTS. `atlas/exclude.txt freshness` was red for nine consecutive
# nights (2026-08-15 .. 2026-08-24) while guarding a `DROP TABLE`-class footgun,
# and nothing noticed. That was the SECOND such streak on the same workflow --
# its own header records twelve unnoticed red nights in 2026-07/08. The reason
# is structural, not inattention:
#
#   * Coord's merge train only reads runs that establish a main baseline, and
#     that predicate admits `push` runs on `main`. A workflow triggered only by
#     `schedule` / `workflow_dispatch` / path-filtered `pull_request` has zero
#     push runs on main, so it can never hold a PR -- and correspondingly
#     nothing ever reads it.
#   * A scheduled run has no author, no PR and no reviewer. GitHub emails the
#     workflow file's last committer, which for a fleet-authored workflow is
#     nobody in particular.
#
# Advisory is not the same as unimportant. Exactly the workflows the train
# cannot see are the ones this detector watches.
#
# SCOPE EXCLUSION IS STRUCTURAL, NOT A DENYLIST. Two workflows in this repo are
# routinely red on `main` and are NOT defects of this class -- `Release`
# (192238698) and `schema.pg.sql.generated freshness` (268755340). Both are
# `workflow_dispatch`-only on `main` (measured 2026-08-24: 2 and 4 main runs
# respectively, zero `schedule`, zero `push`). They are excluded because this
# detector reads `event=schedule` runs and they have none -- not because their
# ids are hardcoded anywhere. A hardcoded id list would rot the first time a
# workflow changed triggers, and a false positive is as damaging as the silence
# it replaces: it retrains the reader to ignore the channel.
#
# CANCELLED RUNS ARE NEUTRAL. A `cancelled` run is an infrastructure kill (the
# 2026-08-19 run in the streak above died in `Install Postgres client` during an
# apt-mirror outage), not a verdict. It neither counts toward a streak nor
# breaks one. `failure`, `timed_out` and `startup_failure` count; `success`,
# `neutral` and `skipped` break the streak.
#
# Usage:
#   scripts/detect-schedule-red-streaks.sh [options]
#     --repo <owner/name>     default qontinui/qontinui-runner
#     --branch <name>         default main
#     --min-streak <n>        report at n or more consecutive failures (default 3)
#     --window <n>            runs examined per workflow (default 40, max 100)
#     --workflow <id>         restrict to one workflow id (repeatable)
#     --fixture-dir <dir>     read recorded JSON instead of calling the API
#     --exit-zero             always exit 0; report findings on stdout only
#
# Exit codes: 0 no findings, 1 one or more findings, 2 the detector itself failed.
#
# Fixture layout (used by scripts/tests/test_schedule_red_streak_detector.sh),
# mirroring the API payloads exactly so the two modes cannot diverge:
#   <dir>/workflows.json      {"workflows":[{"id":…,"name":…,"path":…,"state":…}]}
#   <dir>/<id>.schedule.json  a runs payload, newest first, event=schedule
#   <dir>/<id>.push.json      {"total_count": n}  -- the push-baseline probe

set -euo pipefail

REPO="qontinui/qontinui-runner"
BRANCH="main"
MIN_STREAK=3
WINDOW=40
FIXTURE_DIR=""
EXIT_ZERO=0
ONLY_IDS=""

die() { echo "detect-schedule-red-streaks: $*" >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --repo)        REPO="${2:?--repo needs a value}"; shift 2 ;;
    --branch)      BRANCH="${2:?--branch needs a value}"; shift 2 ;;
    --min-streak)  MIN_STREAK="${2:?--min-streak needs a value}"; shift 2 ;;
    --window)      WINDOW="${2:?--window needs a value}"; shift 2 ;;
    --workflow)    ONLY_IDS="$ONLY_IDS ${2:?--workflow needs a value}"; shift 2 ;;
    --fixture-dir) FIXTURE_DIR="${2:?--fixture-dir needs a value}"; shift 2 ;;
    --exit-zero)   EXIT_ZERO=1; shift ;;
    -h|--help)     sed -n '1,50p' "$0"; exit 0 ;;
    *)             die "unknown option '$1'" ;;
  esac
done

command -v jq >/dev/null 2>&1 || die "jq is required"
if [ -z "$FIXTURE_DIR" ]; then
  command -v gh >/dev/null 2>&1 || die "gh is required in live mode (or pass --fixture-dir)"
fi

case "$MIN_STREAK" in ''|*[!0-9]*) die "--min-streak must be a non-negative integer" ;; esac
case "$WINDOW" in ''|*[!0-9]*) die "--window must be a non-negative integer" ;; esac
[ "$WINDOW" -le 100 ] || die "--window may not exceed the API's 100-per-page cap"

# NO error suppression anywhere below. A read that fails is UNKNOWN, and a
# detector that quietly reports "no findings" because its own API call 403'd
# would reproduce, one level up, precisely the silence it exists to end.
api() {
  # api <relative-path> <fixture-file>
  if [ -n "$FIXTURE_DIR" ]; then
    local f="$FIXTURE_DIR/$2"
    [ -f "$f" ] || die "fixture '$f' not found"
    cat "$f"
  else
    gh api "repos/$REPO/$1"
  fi
}

# --- Workflow inventory -------------------------------------------------------
inventory="$(api "actions/workflows?per_page=100" "workflows.json")" \
  || die "could not read the workflow inventory for $REPO"

ids="$(printf '%s' "$inventory" | jq -r '.workflows[] | select(.state == "active") | .id')"
if [ -n "$ONLY_IDS" ]; then
  filtered=""
  for want in $ONLY_IDS; do
    for have in $ids; do
      [ "$want" = "$have" ] && filtered="$filtered $want"
    done
  done
  ids="$filtered"
fi
[ -n "${ids// /}" ] || die "no active workflows matched"

findings=0
examined=0
skipped_baselined=0
skipped_no_schedule=0

for id in $ids; do
  name="$(printf '%s' "$inventory" | jq -r --argjson id "$id" '.workflows[] | select(.id == $id) | .name')"

  # Push-baseline probe FIRST: a workflow with push runs on the branch is
  # already adjudicated by coord's baseline machinery, and reporting it here
  # would double-report a signal that already has an owner.
  push_json="$(api "actions/workflows/$id/runs?branch=$BRANCH&event=push&per_page=1" "$id.push.json")" \
    || die "could not probe the push baseline for workflow $id"
  push_total="$(printf '%s' "$push_json" | jq -r '.total_count')"
  if [ "$push_total" != "0" ]; then
    skipped_baselined=$((skipped_baselined + 1))
    continue
  fi

  sched_json="$(api "actions/workflows/$id/runs?branch=$BRANCH&event=schedule&per_page=$WINDOW" "$id.schedule.json")" \
    || die "could not read scheduled runs for workflow $id"

  # This is the whole scope exclusion: a dispatch-only workflow has no
  # `event=schedule` runs on the branch and therefore cannot be named.
  sched_total="$(printf '%s' "$sched_json" | jq -r '.total_count')"
  if [ "$sched_total" = "0" ]; then
    skipped_no_schedule=$((skipped_no_schedule + 1))
    continue
  fi

  examined=$((examined + 1))

  # Walk newest-first. Belt-and-braces re-filter on event and branch: a fixture
  # or a future API change that leaked a non-schedule run must not be counted.
  read -r streak newest <<EOF
$(printf '%s' "$sched_json" | jq -r --arg br "$BRANCH" '
    [ .workflow_runs[]
      | select(.event == "schedule" and .head_branch == $br and .status == "completed") ]
    | reduce .[] as $r ({streak: 0, done: false, newest: null};
        if .done then .
        elif ($r.conclusion | IN("failure", "timed_out", "startup_failure"))
          then {streak: (.streak + 1), done: false,
                newest: (if .newest == null then $r else .newest end)}
        elif $r.conclusion == "cancelled"
          then .
        else {streak: .streak, done: true, newest: .newest}
        end)
    | [ (.streak | tostring),
        (if .newest == null then "-"
         else "\(.newest.id)|\(.newest.head_sha[0:8])|\(.newest.created_at)|\(.newest.html_url)"
         end) ]
    | @tsv')
EOF

  if [ "$streak" -ge "$MIN_STREAK" ]; then
    findings=$((findings + 1))
    IFS='|' read -r run_id sha created url <<EOF2
$newest
EOF2
    echo "schedule-red-streak: $name (workflow $id) -- $streak consecutive failing scheduled runs on $BRANCH"
    echo "    newest failure: run $run_id @ $sha ($created)"
    echo "    $url"
    echo "    nothing gates on this workflow: it has 0 push runs on $BRANCH, so coord's"
    echo "    merge train never adjudicates it and no PR author ever sees it."
  fi
done

echo "detect-schedule-red-streaks: $findings finding(s); examined $examined scheduled workflow(s) on $REPO@$BRANCH" \
     "(skipped $skipped_baselined push-baselined, $skipped_no_schedule with no scheduled runs); min-streak=$MIN_STREAK window=$WINDOW"

if [ "$findings" -gt 0 ] && [ "$EXIT_ZERO" -eq 0 ]; then
  exit 1
fi
exit 0
