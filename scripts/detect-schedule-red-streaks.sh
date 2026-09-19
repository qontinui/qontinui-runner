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
# CANCELLED RUNS ARE NEUTRAL -- UNLESS THEY ARE A TIMEOUT. `failure`,
# `timed_out` and `startup_failure` count toward a streak; `success`, `neutral`
# and `skipped` break it. A `cancelled` run is usually an infrastructure kill
# (the 2026-08-19 run in the streak above died in `Install Postgres client`
# during an apt-mirror outage) or a supersede -- not a verdict -- so it neither
# counts nor breaks.
#
# THE MEASURED FACT this used to get wrong: GitHub renders a JOB-level
# `timeout-minutes` expiry as `cancelled` -- never `failure`, never `timed_out`.
# A STEP-level expiry concludes `failure`. `timed_out` is counted above, but
# GitHub does not use it for a job timeout, so a nightly that blew its bound
# every night was invisible here (ccfg `lint-frontmatter`, 307681598, whose
# `guard-roster-windows` job was cut at 10823 s and 10825 s on 2026-09-12/13
# against a 180-minute bound). Do not re-derive this: the in-band marker that
# tells the two apart from inside a run is qontinui-web
# `backend-coverage-producer.yml`, step "Explain infrastructure timeout vs
# external cancellation", and its runner copy in `frontend-coverage-producer.yml`
# (budget arms only). Plan: 2026-09-13-a-timeout-minutes-expiry-renders-as-
# cancelled-and-three-programs-are-blind-to-it.
#
# So a cancelled run counts EXACTLY as `failure` does -- including as the
# newest cited run -- when one of its jobs carries the BOUND-HIT SIGNATURE
# (scripts/lib/bound-hit-signature.jq, which states it in full): >= K cancelled
# durations of the same job in a cluster whose span is <= clamp(5% x median,
# 30 s, 60 s), every member above every success of that job that ran no later
# than the cluster's newest member. Job-level durations, from
# `/actions/runs/<id>/jobs`; no declared bound is read. Every other cancelled
# run stays neutral -- a supersede outside the cluster never counts.
#
# COST AND ITS PREFILTER. The signature needs every run's jobs (successes too),
# one API call per run. It is fetched ONLY for a workflow whose examined window
# holds >= K NON-SUCCESS runs. The signature counts cancelled JOBS, not runs:
# a night on which the bound cut one job while ANOTHER job failed concludes
# `failure`, yet its cancelled job still belongs to the cluster -- so counting
# cancelled RUNS here would hide exactly the pattern this arm exists for. The
# premise the prefilter does rest on: a run holding a cancelled job does not
# conclude `success`. So below K non-success runs no job can reach K cancelled
# durations, and every cancellation is neutral with no read at all. The
# jobs API returns only the LATEST attempt of a run; a re-run hides the
# original attempt's jobs. That blind spot is bounded and accepted.
#
# ABSENCE IS COUNTED, NEVER SILENT. The summary line ends with a tally of the
# examined windows' cancelled runs: bound-hit (counted as failing), neutral, and
# unclassified -- either no job data, or a tight cluster with no earlier
# success to order it against (the signature refuses to call that a bound-hit,
# and it is not health either). The asymmetry between the two modes is
# deliberate: in LIVE mode a jobs read that fails is a failed read and exits 2,
# like every other read below; in FIXTURE mode a missing `<run-id>.jobs.json`
# means no duration data was RECORDED, so the arm abstains for that workflow
# and its cancellations are counted unclassified. A fixture corpus predating
# this arm is therefore still a valid regression surface.
#
# Usage:
#   scripts/detect-schedule-red-streaks.sh [options]
#     --repo <owner/name>     default qontinui/qontinui-runner
#     --branch <name>         default main
#     --min-streak <n>        report at n or more consecutive failures (default 3)
#     --window <n>            runs examined per workflow (default 40, max 100)
#     --workflow <id>         restrict to one workflow id (repeatable)
#     --cluster-min <k>       K, the bound-hit cluster size (default 3, min 2)
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
#   <dir>/<run-id>.jobs.json  a /actions/runs/<run-id>/jobs payload (OPTIONAL:
#                             absent => that workflow's cancellations are
#                             counted unclassified, see ABSENCE above)

set -euo pipefail

REPO="qontinui/qontinui-runner"
BRANCH="main"
MIN_STREAK=3
WINDOW=40
FIXTURE_DIR=""
EXIT_ZERO=0
ONLY_IDS=""
CLUSTER_MIN=3

# The signature lives beside this script, located from its own path so the
# detector works from any cwd and from a `git show origin/main:` export that
# keeps the tree shape.
SIGNATURE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/bound-hit-signature.jq"

die() { echo "detect-schedule-red-streaks: $*" >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --repo)        REPO="${2:?--repo needs a value}"; shift 2 ;;
    --branch)      BRANCH="${2:?--branch needs a value}"; shift 2 ;;
    --min-streak)  MIN_STREAK="${2:?--min-streak needs a value}"; shift 2 ;;
    --window)      WINDOW="${2:?--window needs a value}"; shift 2 ;;
    --workflow)    ONLY_IDS="$ONLY_IDS ${2:?--workflow needs a value}"; shift 2 ;;
    --cluster-min) CLUSTER_MIN="${2:?--cluster-min needs a value}"; shift 2 ;;
    --fixture-dir) FIXTURE_DIR="${2:?--fixture-dir needs a value}"; shift 2 ;;
    --exit-zero)   EXIT_ZERO=1; shift ;;
    -h|--help)     sed -n '1,/^set -euo pipefail$/p' "$0" | sed '$d'; exit 0 ;;
    *)             die "unknown option '$1'" ;;
  esac
done

command -v jq >/dev/null 2>&1 || die "jq is required"
if [ -z "$FIXTURE_DIR" ]; then
  command -v gh >/dev/null 2>&1 || die "gh is required in live mode (or pass --fixture-dir)"
fi

case "$MIN_STREAK" in ''|*[!0-9]*) die "--min-streak must be a positive integer" ;; esac
case "$WINDOW" in ''|*[!0-9]*) die "--window must be a positive integer" ;; esac
# Zero is rejected rather than clamped: `--min-streak 0` would name every
# scheduled workflow, including ones with no failing run at all, and print a
# citation line with a blank run id and URL. A detector that flags everything
# says nothing.
[ "$MIN_STREAK" -ge 1 ] || die "--min-streak must be at least 1"
[ "$WINDOW" -ge 1 ] || die "--window must be at least 1"
[ "$WINDOW" -le 100 ] || die "--window may not exceed the API's 100-per-page cap"
# K is an option rather than a constant for the same reason --min-streak is:
# the default (3) is the plan's measured choice, and a caller auditing a
# workflow with few scheduled nights may want to see a 2-night cluster. It is
# floored at 2 because a single cancellation is not a pattern -- K=1 would make
# every cancelled job above its prior successes a "timeout".
case "$CLUSTER_MIN" in ''|*[!0-9]*) die "--cluster-min must be a positive integer" ;; esac
[ "$CLUSTER_MIN" -ge 2 ] || die "--cluster-min must be at least 2"
[ -f "$SIGNATURE" ] || die "bound-hit signature '$SIGNATURE' not found"

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

if ! ids="$(printf '%s' "$inventory" | jq -r '.workflows[] | select(.state == "active") | .id')"; then
  die "could not parse the workflow inventory for $REPO"
fi
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
cancel_bound_hit=0
cancel_neutral=0
cancel_no_data=0
cancel_undecided=0

for id in $ids; do
  if ! name="$(printf '%s' "$inventory" | jq -r --argjson id "$id" '.workflows[] | select(.id == $id) | .name')"; then
    die "could not read the name of workflow $id"
  fi

  # Push-baseline probe FIRST: a workflow with push runs on the branch is
  # already adjudicated by coord's baseline machinery, and reporting it here
  # would double-report a signal that already has an owner.
  push_json="$(api "actions/workflows/$id/runs?branch=$BRANCH&event=push&per_page=1" "$id.push.json")" \
    || die "could not probe the push baseline for workflow $id"
  if ! push_total="$(printf '%s' "$push_json" | jq -r '.total_count')"; then
    die "could not parse the push-baseline payload for workflow $id ($name)"
  fi
  case "$push_total" in ''|*[!0-9]*) die "non-numeric push total_count '$push_total' for workflow $id ($name)" ;; esac
  if [ "$push_total" != "0" ]; then
    skipped_baselined=$((skipped_baselined + 1))
    continue
  fi

  sched_json="$(api "actions/workflows/$id/runs?branch=$BRANCH&event=schedule&per_page=$WINDOW" "$id.schedule.json")" \
    || die "could not read scheduled runs for workflow $id"

  # This is the whole scope exclusion: a dispatch-only workflow has no
  # `event=schedule` runs on the branch and therefore cannot be named.
  if ! sched_total="$(printf '%s' "$sched_json" | jq -r '.total_count')"; then
    die "could not parse the scheduled-runs payload for workflow $id ($name)"
  fi
  case "$sched_total" in ''|*[!0-9]*) die "non-numeric schedule total_count '$sched_total' for workflow $id ($name)" ;; esac
  if [ "$sched_total" = "0" ]; then
    skipped_no_schedule=$((skipped_no_schedule + 1))
    continue
  fi

  # `total_count` and the returned page can disagree. A non-zero count with an
  # empty page is a read we cannot interpret -- treating it as "no streak" would
  # report a healthy 0 findings off no evidence at all.
  if ! sched_returned="$(printf '%s' "$sched_json" | jq -r '.workflow_runs | length')"; then
    die "could not count the scheduled runs returned for workflow $id ($name)"
  fi
  [ "$sched_returned" -gt 0 ] \
    || die "workflow $id ($name) reports total_count=$sched_total scheduled runs but returned none"

  examined=$((examined + 1))

  # --- Cancellation classification (the bound-hit arm) -----------------------
  # The window the streak reduce below walks, re-filtered identically. Every
  # cancelled run in it lands in exactly one tally bucket.
  #
  # This read is deliberately LENIENT about shape (non-object entries are
  # dropped rather than fatal) and that is not error suppression: the STRICT
  # parse of this same payload is the streak reduce below, which dies on any
  # shape this read tolerated. Keeping the strict parse there, and only there,
  # keeps the test that pins the heredoc fix at that site reachable -- a strict
  # guard here would die first and leave that fix site with no test that
  # distinguishes it.
  if ! window_runs="$(printf '%s' "$sched_json" | jq -r --arg br "$BRANCH" '
    (.workflow_runs | if type == "array" or type == "object" then .[] else empty end)
    | objects
    | select(.event == "schedule" and .head_branch == $br and .status == "completed")
    | [(.id | tostring), .created_at, .conclusion] | @tsv')"; then
    die "could not list the scheduled-run window of workflow $id ($name)"
  fi
  n_cancelled=0
  n_not_success=0
  while IFS=$'\t' read -r _rid _created concl; do
    if [ "$concl" = "cancelled" ]; then n_cancelled=$((n_cancelled + 1)); fi
    if [ -n "$concl" ] && [ "$concl" != "success" ]; then n_not_success=$((n_not_success + 1)); fi
  done <<EOF_WIN
$window_runs
EOF_WIN

  # run id -> {name, duration, n, span} for every run holding a bound-hit job.
  bound_hits='{}'
  undecided='[]'
  undecided_clusters=""
  if [ "$n_not_success" -lt "$CLUSTER_MIN" ]; then
    # Prefilter: fewer than K non-success runs means no job can have K cancelled
    # durations, so no cancellation here can be a bound-hit. Neutral by
    # construction; no jobs read is spent.
    cancel_neutral=$((cancel_neutral + n_cancelled))
  else
    rows_file="$(mktemp)"
    no_job_data=0
    while IFS=$'\t' read -r rid created _concl; do
      [ -n "$rid" ] || continue
      # FIXTURE mode only: a missing jobs file is "no duration data recorded",
      # an abstention, never a die. LIVE mode has no such branch -- a failed
      # jobs read goes through api() and exits 2 like every other read.
      if [ -n "$FIXTURE_DIR" ] && [ ! -f "$FIXTURE_DIR/$rid.jobs.json" ]; then
        no_job_data=1
        break
      fi
      jobs_json="$(api "actions/runs/$rid/jobs?per_page=100" "$rid.jobs.json")" \
        || { rm -f "$rows_file"; die "could not read the jobs of run $rid (workflow $id, $name)"; }
      # A run with more jobs than one page returned would feed the signature a
      # silently partial job list; refuse it rather than guess.
      if ! printf '%s' "$jobs_json" | jq -c --arg rid "$rid" --arg ca "$created" '
          if (.jobs | length) < .total_count then error("truncated jobs page") else . end
          | .jobs[]
          | {run_id: ($rid | tonumber), created_at: $ca, name, conclusion, started_at, completed_at}' \
          >> "$rows_file"; then
        rm -f "$rows_file"
        die "could not parse the jobs of run $rid (workflow $id, $name)"
      fi
    done <<EOF_WIN
$window_runs
EOF_WIN

    if [ "$no_job_data" -eq 1 ]; then
      rm -f "$rows_file"
      cancel_no_data=$((cancel_no_data + n_cancelled))
    else
      if ! signature="$(jq -s '.' "$rows_file" | jq --argjson cluster_min "$CLUSTER_MIN" -f "$SIGNATURE")"; then
        rm -f "$rows_file"
        die "could not evaluate the bound-hit signature for workflow $id ($name)"
      fi
      rm -f "$rows_file"
      if ! bound_hits="$(printf '%s' "$signature" | jq -c '
          [ .jobs[] as $j | $j.clusters[] | select(.status == "bound_hit") as $c
            | $c.members[]
            | {key: (.run_id | tostring),
               value: {name: $j.name, duration: .duration, n: $c.n, span: $c.span}} ]
          | from_entries')" \
         || ! undecided="$(printf '%s' "$signature" | jq -c '[.undecided[].run_id | tostring] | unique')" \
         || ! undecided_clusters="$(printf '%s' "$signature" | jq -r '
             .jobs[] | .name as $nm | .clusters[] | select(.status == "no_prior_success")
             | [(.n | tostring), (.span | tostring), $nm] | @tsv')"; then
        die "could not read the bound-hit signature output for workflow $id ($name)"
      fi
      while IFS=$'\t' read -r rid _created concl; do
        [ "$concl" = "cancelled" ] || continue
        if printf '%s' "$bound_hits" | jq -e --arg r "$rid" 'has($r)' >/dev/null; then
          cancel_bound_hit=$((cancel_bound_hit + 1))
        elif printf '%s' "$undecided" | jq -e --arg r "$rid" 'index($r) != null' >/dev/null; then
          cancel_undecided=$((cancel_undecided + 1))
        else
          cancel_neutral=$((cancel_neutral + 1))
        fi
      done <<EOF_WIN
$window_runs
EOF_WIN
    fi
  fi

  # Walk newest-first.
  #
  # `sort_by(.created_at) | reverse` rather than trusting the API's order: the
  # reduce below is order-DEPENDENT (`newest` is the first failure seen and
  # `done` latches on the first non-failing run), so an ordering change would
  # silently invert both the streak and the run it cites. Making the property
  # structural costs one jq pass and removes an assumption no test could catch.
  #
  # Belt-and-braces re-filter on event and branch: a fixture or a future API
  # change that leaked a non-schedule run must not be counted.
  #
  # NOT `read ... <<EOF $(jq ...) EOF`. A command substitution inside a heredoc
  # has its exit status DISCARDED -- `set -e` and `pipefail` never see it -- so a
  # jq failure (a payload with `total_count` but no `workflow_runs`, say) would
  # leave `streak` empty, skip the workflow, and let the run end
  # "0 finding(s)" / exit 0. That is precisely the silence this detector exists
  # to end, reproduced one level up inside the detector itself.
  #
  # A cancelled run whose id is in $bh (a bound-hit, see the header) counts
  # EXACTLY as `failure` does, and may be the cited `newest`. Any other
  # cancelled run is skipped over, neither counting nor breaking -- including
  # one tallied "unclassified" above: an abstention must not manufacture a
  # finding, and the tally is what keeps it from reading as health.
  if ! streak_tsv="$(printf '%s' "$sched_json" | jq -r --arg br "$BRANCH" --argjson bh "$bound_hits" '
    [ .workflow_runs[]
      | select(.event == "schedule" and .head_branch == $br and .status == "completed") ]
    | sort_by(.created_at) | reverse
    | reduce .[] as $r ({streak: 0, done: false, newest: null};
        if .done then .
        elif ($r.conclusion | IN("failure", "timed_out", "startup_failure"))
             or ($r.conclusion == "cancelled" and $bh[$r.id | tostring] != null)
          then {streak: (.streak + 1), done: false,
                newest: (if .newest == null then $r else .newest end)}
        elif $r.conclusion == "cancelled"
          then .
        else {streak: .streak, done: true, newest: .newest}
        end)
    | [ (.streak | tostring),
        (if .newest == null then "-"
         else "\(.newest.id)|\(.newest.head_sha[0:8])|\(.newest.created_at)|\(.newest.html_url)"
         end),
        (if .newest == null then "-"
         # Only a CANCELLED newest run needs the explanation: a `failure` run
         # holding a bound-hit job already reads as what it is.
         elif .newest.conclusion != "cancelled" then "-"
         else ($bh[.newest.id | tostring] // null) as $h
         | if $h == null then "-" else "\($h.duration)|\($h.n)|\($h.span)|\($h.name)" end
         end) ]
    | @tsv')"; then
    die "could not compute the failure streak for workflow $id ($name)"
  fi

  IFS=$'\t' read -r streak newest newest_bh <<EOF
$streak_tsv
EOF
  # An empty or non-numeric streak means the payload was not what we think it
  # is. `[ "" -ge 3 ]` returns 2, and inside an `if` condition `set -e` is
  # exempt -- so without this the workflow would be skipped silently.
  case "$streak" in ''|*[!0-9]*) die "non-numeric streak '$streak' for workflow $id ($name)" ;; esac
  [ -n "$newest" ] || die "empty streak citation for workflow $id ($name)"

  if [ "$streak" -ge "$MIN_STREAK" ]; then
    findings=$((findings + 1))
    IFS='|' read -r run_id sha created url <<EOF2
$newest
EOF2
    echo "schedule-red-streak: $name (workflow $id) -- $streak consecutive failing scheduled runs on $BRANCH"
    echo "    newest failure: run $run_id @ $sha ($created)"
    echo "    $url"
    if [ -n "$newest_bh" ] && [ "$newest_bh" != "-" ]; then
      # The job NAME goes LAST and `read` hands the remainder to the last
      # variable: a name may legally contain `|` (`build | lint`), and any
      # position but the last would split it.
      IFS='|' read -r bh_dur bh_n bh_span bh_job <<EOF3
$newest_bh
EOF3
      # Deliberately NO comparison with the job's successes here: see the
      # CENSORING PROHIBITION in scripts/lib/bound-hit-signature.jq.
      echo "    newest counted run is a bound-hit cancellation (job $bh_job cancelled at $bh_dur s," \
           "in a $bh_n-member cluster spanning $bh_span s) -- the signature of a job-level timeout-minutes expiry"
    fi
    echo "    nothing gates on this workflow: it has 0 push runs on $BRANCH, so coord's"
    echo "    merge train never adjudicates it and no PR author ever sees it."
  fi

  # A tight cluster with no earlier success is NOT counted (no ordinal
  # evidence; see the signature), but it is the worst case -- a bound that
  # never once fit -- so it is printed per job, not only tallied. Not a
  # finding: it changes neither `findings` nor the exit code.
  if [ -n "$undecided_clusters" ]; then
    while IFS=$'\t' read -r u_n u_span u_job; do
      [ -n "$u_n" ] || continue
      echo "schedule-red-streak: UNDECIDED $name (workflow $id) job $u_job: $u_n cancellations clustered within" \
           "$u_span s and no earlier success in the window to order them against -- a bound that never fit looks exactly like this"
    done <<EOF_UND
$undecided_clusters
EOF_UND
  fi
done

echo "detect-schedule-red-streaks: $findings finding(s); examined $examined scheduled workflow(s) on $REPO@$BRANCH" \
     "(skipped $skipped_baselined push-baselined, $skipped_no_schedule with no scheduled runs); min-streak=$MIN_STREAK window=$WINDOW; cancelled runs: $cancel_bound_hit bound-hit (counted as failing), $cancel_neutral neutral," \
     "$((cancel_no_data + cancel_undecided)) unclassified ($cancel_no_data no job data, $cancel_undecided tight cluster with no prior success)"

if [ "$findings" -gt 0 ] && [ "$EXIT_ZERO" -eq 0 ]; then
  exit 1
fi
exit 0
