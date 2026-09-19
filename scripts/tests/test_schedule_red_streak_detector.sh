#!/usr/bin/env bash
# Regression test for scripts/detect-schedule-red-streaks.sh, driven entirely
# from recorded/synthetic fixtures. No network, no gh, no credentials.
#
# WHY THIS FILE EXISTS. The detector's whole job is to notice a class of silence.
# An untested detector is the same failure one level up: it reports "0 findings"
# whether the fleet is healthy or its own API read 403'd, and nobody can tell the
# two apart. The specific properties pinned here are the ones a wrong detector
# gets wrong:
#
#   * It NAMES `atlas/exclude.txt freshness` on the historical window that
#     actually happened (2026-08-15 .. 2026-08-24, nine failing scheduled runs
#     on main). Recorded from the live API on 2026-08-24 and frozen.
#   * It does NOT name `Release` (192238698) or `schema.pg.sql.generated
#     freshness` (268755340). Both are routinely red on main and both are
#     `workflow_dispatch`-only there, so flagging them would be a false positive
#     -- and a false positive is as damaging as the silence, because it retrains
#     the reader to ignore the channel.
#   * A `cancelled` run is neutral, not a failure. The 2026-08-19 run in the real
#     streak was an apt-mirror infrastructure kill; counting it would have said
#     ten, and breaking the streak on it would have said four.
#   * A push-baselined workflow is skipped -- coord's merge train already
#     adjudicates those, and double-reporting a signal that has an owner is how a
#     channel becomes noise.
#
# Run locally:
#   bash scripts/tests/test_schedule_red_streak_detector.sh

set -euo pipefail

# The harness's own jq reads must not carry a native Windows jq.exe's CRLF into
# the string comparisons below either (see the wrapper in the detector).
real_jq="$(type -P jq)" || { echo "::error::jq not found on PATH"; exit 1; }
jq() { "$real_jq" "$@" | tr -d '\r'; }

tests_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
scripts_dir="$(dirname "$tests_dir")"
detector="$scripts_dir/detect-schedule-red-streaks.sh"
fixtures="$tests_dir/fixtures/schedule-red-streaks"

[ -f "$detector" ] || { echo "::error::cannot find $detector"; exit 1; }
[ -d "$fixtures" ] || { echo "::error::cannot find $fixtures"; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0
assert() {
  if [ "$2" = "$3" ]; then
    printf '  PASS  %-58s %s\n' "$1" "$3"
  else
    printf '  FAIL  %-58s expected %s, got %s\n' "$1" "$2" "$3"
    failures=$((failures + 1))
  fi
}

run_detector() {
  # run_detector <fixture-subdir> [extra args...]; echoes the exit code,
  # leaves combined output in $work/out.txt
  local dir="$1"; shift
  local rc=0
  bash "$detector" --fixture-dir "$fixtures/$dir" "$@" > "$work/out.txt" 2>&1 || rc=$?
  echo "$rc"
}
saw() { grep -qF "$1" "$work/out.txt" && echo yes || echo no; }

# ---------------------------------------------------------------------------
# The historical window, recorded live on 2026-08-24. This is V5 of plan
# 2026-08-24-runner-atlas-exclude-freshness-nightly-red: the detector must find
# the streak with nobody pointing it at the workflow.
# ---------------------------------------------------------------------------
echo "Recorded real window (qontinui/qontinui-runner@main, read 2026-08-24):"
assert "findings present => exit 1"                1 "$(run_detector real-2026-08-24)"
assert "names the atlas freshness workflow"        yes "$(saw 'atlas/exclude.txt freshness (workflow 317525761)')"
# Anchored on the workflow id: a bare '9 consecutive' would also match
# '19 consecutive', so the assertion whose NAME is "not 10 or 4" has to pin
# both ends of the number.
assert "counts the streak as 9, not 10 or 4"       yes "$(saw '317525761) -- 9 consecutive failing scheduled runs on main')"
assert "cites the newest failing run"              yes "$(saw 'run 32701001502')"
assert "explains why nothing else sees it"         yes "$(saw '0 push runs on main')"

# The two known-spurious advisories. Excluded STRUCTURALLY (no event=schedule
# runs on main), never by a hardcoded id list.
assert "does NOT flag Release (192238698)"         no  "$(saw '192238698')"
assert "does NOT flag schema.pg.sql (268755340)"   no  "$(saw '268755340')"
assert "reports the two as having no sched runs"   yes "$(saw '(skipped 0 push-baselined, 2 with no scheduled runs)')"

# A high threshold must silence the same window -- proof the count is real and
# the finding is not unconditional.
assert "min-streak 10 => no finding on the same data" 0 "$(run_detector real-2026-08-24 --min-streak 10)"
assert "and it says zero findings"                 yes "$(saw ': 0 finding(s)')"
assert "--exit-zero suppresses the exit code only" 0 "$(run_detector real-2026-08-24 --exit-zero)"
assert "--exit-zero still prints the finding"      yes "$(saw 'atlas/exclude.txt freshness (workflow 317525761)')"

# ---------------------------------------------------------------------------
# Synthetic filter cases the real repo does not currently supply.
# ---------------------------------------------------------------------------
echo ""
echo "Synthetic filter cases:"
assert "synthetic set => exit 1"                   1 "$(run_detector synthetic)"
# 5 failures in a row, but the workflow has 12 push runs on main: coord's
# baseline machinery owns it, so this detector must stay quiet.
assert "skips the push-baselined workflow"         no  "$(saw '900000001')"
assert "counts it as push-baselined"               yes "$(saw '(skipped 1 push-baselined,')"
# Newest run is green: streak 0 even though older runs failed.
assert "does not flag a currently-green nightly"   no  "$(saw '900000002')"
# All cancelled: neutral, so the streak is 0, not 5.
assert "does not flag an all-cancelled nightly"    no  "$(saw '900000003')"
# failure, cancelled, failure, failure, success => the cancel is skipped over
# and the streak is 3.
assert "a cancel neither breaks nor pads a streak" yes "$(saw '900000004) -- 3 consecutive')"
# state != active is out of the inventory entirely.
assert "ignores a disabled workflow"               no  "$(saw '900000005')"

# ---------------------------------------------------------------------------
# The detector must fail LOUDLY rather than report zero findings, because
# "0 findings" from a broken read is the exact silence it exists to end.
# ---------------------------------------------------------------------------
echo ""
echo "A broken read is UNKNOWN, never 'no findings':"
mkdir -p "$work/empty"
rc=0; bash "$detector" --fixture-dir "$work/empty" > "$work/out.txt" 2>&1 || rc=$?
assert "missing inventory => exit 2, not 0"        2 "$rc"
assert "and it names the missing fixture"          yes "$(saw 'workflows.json')"
cp -r "$fixtures/real-2026-08-24" "$work/holed"
rm "$work/holed/317525761.schedule.json"
rc=0; bash "$detector" --fixture-dir "$work/holed" > "$work/out.txt" 2>&1 || rc=$?
assert "missing runs payload => exit 2, not 0"     2 "$rc"
assert "and it names the missing fixture"          yes "$(saw '317525761.schedule.json')"

# MALFORMED content, not just a missing file. Each of these lands on a
# different guard; the one that reaches the streak computation itself -- the
# site of the heredoc bug, where jq's exit status used to be discarded -- is
# called out below.
mangle() {
  # mangle <file> <json> ; leaves the run's output in $work/out.txt, echoes rc
  rm -rf "$work/bad"; cp -r "$fixtures/real-2026-08-24" "$work/bad"
  printf '%s' "$2" > "$work/bad/$1"
  local rc=0
  bash "$detector" --fixture-dir "$work/bad" > "$work/out.txt" 2>&1 || rc=$?
  echo "$rc"
}

assert "runs payload with no workflow_runs => exit 2" 2 "$(mangle 317525761.schedule.json '{"total_count": 9}')"
assert "and it does not claim zero findings"       no  "$(saw 'finding(s)')"
assert "and it names the workflow it could not read" yes "$(saw 'workflow 317525761')"

# THIS is the fixture that reaches the streak jq itself. Every other
# malformed shape above is caught by an EARLIER guard (the total_count parse,
# the numeric check, the empty-page check), so with only those, reverting the
# heredoc fix leaves this suite fully green -- a fix site with no test that
# distinguishes it is not covered. An OBJECT for workflow_runs has length 1,
# so it clears the empty-page check; `.workflow_runs[]` then iterates values
# and `1 | select(.event == ...)` makes jq exit non-zero. Measured: fixed
# detector exits 2 naming the streak computation; the pre-fix heredoc form
# exits 0 reporting '0 finding(s)'.
assert "runs payload that breaks the streak jq => 2" 2 "$(mangle 317525761.schedule.json '{"total_count": 9, "workflow_runs": {"a": 1}}')"
assert "and it does not claim zero findings"       no  "$(saw 'finding(s)')"
assert "and it names the streak computation"       yes "$(saw 'could not compute the failure streak')"

assert "total_count>0 but an empty page => exit 2" 2 "$(mangle 317525761.schedule.json '{"total_count": 9, "workflow_runs": []}')"
assert "and it says the page returned none"        yes "$(saw 'returned none')"

assert "unparseable JSON => exit 2, not 0"         2 "$(mangle 317525761.schedule.json 'not json at all')"
assert "unparseable push probe => exit 2, not 0"   2 "$(mangle 317525761.push.json 'not json at all')"
assert "non-numeric total_count => exit 2, not 0"  2 "$(mangle 317525761.push.json '{"total_count": "lots"}')"

# --- Argument validation: a threshold that flags everything says nothing.
echo ""
echo "Argument validation:"
rc=0; bash "$detector" --fixture-dir "$fixtures/real-2026-08-24" --min-streak 0 > "$work/out.txt" 2>&1 || rc=$?
assert "--min-streak 0 is rejected"                2 "$rc"
rc=0; bash "$detector" --fixture-dir "$fixtures/real-2026-08-24" --window 0 > "$work/out.txt" 2>&1 || rc=$?
assert "--window 0 is rejected"                    2 "$rc"
rc=0; bash "$detector" --fixture-dir "$fixtures/real-2026-08-24" --window 500 > "$work/out.txt" 2>&1 || rc=$?
assert "--window above the API page cap rejected"  2 "$rc"

# ---------------------------------------------------------------------------
# The bound-hit arm (plan 2026-09-13-a-timeout-minutes-expiry-renders-as-
# cancelled-and-three-programs-are-blind-to-it, Phases 2b and 3). A JOB-level
# `timeout-minutes` expiry concludes `cancelled`, so a cancelled run counts as
# failing iff one of its jobs carries the bound-hit signature.
# ---------------------------------------------------------------------------
echo ""
echo "Existing corpora carry no job data: the arm abstains, and SAYS so:"
run_detector real-2026-08-24 >/dev/null
# The atlas window has 1 cancelled run (the 2026-08-19 apt kill) among 10
# non-success runs, so the prefilter (>= K NON-SUCCESS runs) fans out, finds
# no recorded job data, and the cancel is counted unclassified -- visibly.
assert "real window: its one cancel is unclassified" yes "$(saw '; cancelled runs: 0 bound-hit (counted as failing), 0 neutral, 1 unclassified (1 no job data, 0 tight cluster with no prior success)')"
assert "real window: the streak is still 9"        yes "$(saw '317525761) -- 9 consecutive failing scheduled runs on main')"
assert "real window: no bound-hit line"            no  "$(saw 'bound-hit cancellation')"
# Synthetic 900000003 has 5 cancelled runs and NO <run-id>.jobs.json: the
# prefilter says fan out, the fixture has no duration data, so its 5
# cancellations are a COUNTED skip -- never a die, never a silent pass.
assert "synthetic: missing jobs files => not a die" 1 "$(run_detector synthetic)"
# (900000004's one cancel sits among 4 non-success runs, so it is fanned out
# and unclassified too: 6 in all.)
assert "synthetic: 6 unclassified for lack of data" yes "$(saw '; cancelled runs: 0 bound-hit (counted as failing), 0 neutral, 6 unclassified (6 no job data, 0 tight cluster with no prior success)')"
assert "synthetic: all-cancelled still not flagged" no  "$(saw '900000003')"
assert "synthetic: 900000004 streak unchanged at 3" yes "$(saw '900000004) -- 3 consecutive')"

# ccfg `lint-frontmatter` (307681598). PROVENANCE: recorded live 2026-09-18
# from the Actions API (workflows, push probe, scheduled runs, and
# /actions/runs/<id>/jobs for each run, trimmed to the fields the detector
# reads), frozen at the three scheduled runs on/before 2026-09-13:
#   34558750407 09-11 success   (guard-roster-windows success  7115 s)
#   34670539505 09-12 cancelled (guard-roster-windows cancelled 10823 s)
#   34735701419 09-13 cancelled (guard-roster-windows cancelled 10825 s)
# Only 2 cancelled runs: condition 1 (>= 3) cannot hold, so both are NEUTRAL
# -- classified, by the prefilter, not unclassified -- and the streak is 0.
echo ""
echo "ccfg lint-frontmatter, frozen at 2026-09-13 (2 bound-hit nights, K=3):"
assert "frozen ccfg => no finding, exit 0"          0 "$(run_detector ccfg-2026-09-13)"
assert "and its 2 cancels are neutral, not unclassified" yes "$(saw '; cancelled runs: 0 bound-hit (counted as failing), 2 neutral, 0 unclassified (0 no job data,')"
# Neutral cancels are skipped over and the 09-11 success ends the walk: the
# streak is 0, so even --min-streak 2 stays silent. The plan's "fires on
# night 3" is a property of K, not of --min-streak.
assert "--min-streak 2 still no finding (neutral)"  0 "$(run_detector ccfg-2026-09-13 --min-streak 2)"
assert "and it says zero findings"                  yes "$(saw ': 0 finding(s)')"
# Lowering K to 2 is the one knob that reads the recorded jobs files: the
# 10823/10825 pair (span 2 s, above the 7115 s success) then qualifies.
assert "--cluster-min 2 --min-streak 2 => finding"  1 "$(run_detector ccfg-2026-09-13 --cluster-min 2 --min-streak 2)"
assert "and it cites the 09-13 run as a bound-hit"  yes "$(saw 'job guard-roster-windows cancelled at 10825 s, in a 2-member cluster spanning 2 s')"
assert "--cluster-min 1 is rejected"                2 "$(run_detector ccfg-2026-09-13 --cluster-min 1)"

# One clearly SYNTHETIC fourth night (id 900000101, created 2026-09-14,
# html_url on example.invalid): guard-roster-windows cancelled at 10824 s,
# everything else green. The cluster reaches n=3, and so does the streak.
echo ""
echo "ccfg plus one synthetic night (the plan's 'fires on night 3'):"
assert "plus-synthetic-night => exit 1"             1 "$(run_detector ccfg-2026-09-13-plus-synthetic-night)"
assert "names lint-frontmatter with a streak of 3"  yes "$(saw 'lint-frontmatter (workflow 307681598) -- 3 consecutive failing scheduled runs')"
assert "cites the synthetic night as newest"        yes "$(saw 'newest failure: run 900000101 @')"
assert "and says the newest is a bound-hit"         yes "$(saw 'newest counted run is a bound-hit cancellation (job guard-roster-windows cancelled at 10824 s, in a 3-member cluster spanning 2 s)')"
assert "tallies all 3 cancels as bound-hit"         yes "$(saw '; cancelled runs: 3 bound-hit (counted as failing), 0 neutral, 0 unclassified')"
# Censoring prohibition: successes are an ordinal separator only.
assert "never prints a healthy-maximum ratio"       no  "$(grep -qiE 'healthy max|[0-9.]+ ?x the' "$work/out.txt" && echo yes || echo no)"

# Same fixture, jobs files removed: the arm must abstain VISIBLY.
rm -rf "$work/nojobs"; cp -r "$fixtures/ccfg-2026-09-13-plus-synthetic-night" "$work/nojobs"
rm "$work/nojobs/"*.jobs.json
rc=0; bash "$detector" --fixture-dir "$work/nojobs" > "$work/out.txt" 2>&1 || rc=$?
assert "no jobs data => no finding, exit 0 (not 2)" 0 "$rc"
assert "and 3 cancels counted unclassified"         yes "$(saw '; cancelled runs: 0 bound-hit (counted as failing), 0 neutral, 3 unclassified (3 no job data, 0 tight cluster with no prior success)')"

# Same fixture minus the 09-11 success: a tight 3-cluster with NO earlier
# success to order it against. Not a bound-hit (no ordinal evidence), and not
# health either -- it must land in the unclassified tally.
rm -rf "$work/nosucc"; cp -r "$fixtures/ccfg-2026-09-13-plus-synthetic-night" "$work/nosucc"
jq '.workflow_runs |= map(select(.id != 34558750407)) | .total_count = (.workflow_runs | length)' \
  "$fixtures/ccfg-2026-09-13-plus-synthetic-night/307681598.schedule.json" > "$work/nosucc/307681598.schedule.json"
rc=0; bash "$detector" --fixture-dir "$work/nosucc" > "$work/out.txt" 2>&1 || rc=$?
assert "cluster w/o prior success => no finding"    0 "$rc"
assert "and it is counted unclassified, not neutral" yes "$(saw '; cancelled runs: 0 bound-hit (counted as failing), 0 neutral, 3 unclassified (0 no job data, 3 tight cluster with no prior success)')"

# A malformed jobs payload that IS present is a failed read, not an absence.
rm -rf "$work/badjobs"; cp -r "$fixtures/ccfg-2026-09-13-plus-synthetic-night" "$work/badjobs"
printf 'not json' > "$work/badjobs/34670539505.jobs.json"
rc=0; bash "$detector" --fixture-dir "$work/badjobs" > "$work/out.txt" 2>&1 || rc=$?
assert "unparseable jobs payload => exit 2"         2 "$rc"
assert "and it names the run"                       yes "$(saw 'jobs of run 34670539505')"
printf '{"total_count": 5, "jobs": []}' > "$work/badjobs/34670539505.jobs.json"
rc=0; bash "$detector" --fixture-dir "$work/badjobs" > "$work/out.txt" 2>&1 || rc=$?
assert "truncated jobs page => exit 2, not a guess" 2 "$rc"

# ---------------------------------------------------------------------------
# The signature itself, unit-tested on the runner Frontend Coverage Producer
# (workflow 290069124). PROVENANCE: recorded live 2026-09-18 from the Actions
# API -- 51 push runs 2026-09-01..13 and /actions/runs/<id>/jobs for each --
# flattened into one array of job rows (API field names kept; `created_at` is
# the run's). 39 cancelled: a 36-member cluster at 2714-2724 s (the 45-minute
# bound), one 651 s genuine supersede (33980851220), a 14 s and a 2047 s.
# 12 successes: 10 at 2145-2669 s before the cluster ended, and 2867/2967 s
# AFTER the bound was raised 45 -> 90 on 2026-09-13.
# ---------------------------------------------------------------------------
echo ""
echo "Bound-hit signature (scripts/lib/bound-hit-signature.jq), runner coverage producer:"
sig="$scripts_dir/lib/bound-hit-signature.jq"
rows="$tests_dir/fixtures/bound-hit-signature/runner-coverage-2026-09-13/job-rows.json"
[ -f "$sig" ] || { echo "::error::cannot find $sig"; exit 1; }
[ -f "$rows" ] || { echo "::error::cannot find $rows"; exit 1; }
jq -f "$sig" "$rows" > "$work/sig.json"
# cov <filter> : <filter> applied to the coverage job's summary object
cov() { jq -r --arg n 'Per-file coverage -> coord' "[.jobs[] | select(.name == \$n)][0] | $1" "$work/sig.json"; }
jobq() { jq -r "$1" "$work/sig.json"; }
assert "one qualifying cluster for the coverage job" 1 "$(cov '[.clusters[] | select(.status == "bound_hit")] | length')"
assert "cluster n"                                  36 "$(cov '.clusters[0].n')"
assert "cluster min..max"                    2714..2724 "$(cov '.clusters[0] | "\(.min)..\(.max)"')"
assert "cluster span"                               10 "$(cov '.clusters[0].span')"
assert "tolerance clamps to the 60 s ceiling"       60 "$(cov '.clusters[0].tolerance')"
assert "separator: max success BEFORE the cluster" 2669 "$(cov '.clusters[0].max_prior_success')"
assert "36 bound-hit rows in total"                 36 "$(jobq '.bound_hits | length')"
assert "651 s supersede 33980851220 is NOT a bound-hit" false "$(jobq '[.bound_hits[].run_id] | index(33980851220) != null')"
assert "14 s cancel 34787696285 is NOT a bound-hit" false "$(jobq '[.bound_hits[].run_id] | index(34787696285) != null')"
assert "2047 s cancel 33567295695 is NOT a bound-hit" false "$(jobq '[.bound_hits[].run_id] | index(33567295695) != null')"
assert "post-raise successes do exist (2867, 2967)" "2867,2967" "$(jq -r '[.[] | select(.conclusion == "success") | ((.completed_at|fromdate)-(.started_at|fromdate)) | select(. > 2724)] | sort | map(tostring) | join(",")' "$rows")"

# MUTATION-STYLE NEGATIVE: the refinement to condition 3 is load-bearing.
# Counting every success (the post-raise 2867/2967 s included) breaks strict
# ordering and the clearest timeout pattern in the corpus disappears.
jq --arg success_scope all -f "$sig" "$rows" > "$work/sig.json"
assert "refinement disabled => zero bound-hits"      0 "$(jobq '.bound_hits | length')"
assert "and the cluster is rejected as success_above" success_above "$(cov '.best_rejected.status')"

# Conditions 1 and 2 on the ccfg rows and on small synthetic rows.
ccfg_rows() {
  # ccfg_rows <fixture-dir> : job rows joined to their run's created_at
  local d="$1" out="[]" rid created
  for f in "$d"/*.jobs.json; do
    rid="$(basename "$f" .jobs.json)"
    created="$(jq -r --argjson r "$rid" '.workflow_runs[] | select(.id == $r) | .created_at' "$d/307681598.schedule.json")"
    out="$(jq -c --argjson acc "$out" --arg ca "$created" '$acc + [.jobs[] | {run_id, created_at: $ca, name, conclusion, started_at, completed_at}]' "$f")"
  done
  printf '%s' "$out"
}
ccfg_rows "$fixtures/ccfg-2026-09-13" | jq -f "$sig" > "$work/sig.json"
assert "ccfg frozen: no bound-hit (n=2 < K)"        0 "$(jq '.bound_hits | length' "$work/sig.json")"
assert "ccfg frozen: g-r-w pair is too_small, span 2" "too_small 2 2" "$(jq -r '.jobs[] | select(.name == "guard-roster-windows") | .best_rejected | "\(.status) \(.n) \(.span)"' "$work/sig.json")"
assert "ccfg frozen: frontmatter n=1 does not fire"  "too_small 1" "$(jq -r '.jobs[] | select(.name == "frontmatter") | .best_rejected | "\(.status) \(.n)"' "$work/sig.json")"
ccfg_rows "$fixtures/ccfg-2026-09-13-plus-synthetic-night" | jq -f "$sig" > "$work/sig.json"
assert "ccfg + night 3: g-r-w cluster fires, n=3"   "bound_hit 3 2" "$(jq -r '.jobs[] | select(.name == "guard-roster-windows") | .clusters[0] | "\(.status) \(.n) \(.span)"' "$work/sig.json")"

synth() {
  # synth <duration>... : one success at 90 s, then one cancelled row per arg
  local out='[{"run_id":1,"created_at":"2026-01-01T00:00:00Z","name":"j","conclusion":"success","started_at":"2026-01-01T00:00:00Z","completed_at":"2026-01-01T00:01:30Z"}]'
  local i=2
  for dsec in "$@"; do
    out="$(jq -c --argjson acc "$out" --argjson i "$i" --argjson d "$dsec" -n \
      '$acc + [{run_id: $i, created_at: ("2026-01-0\($i)T00:00:00Z"), name: "j", conclusion: "cancelled",
                started_at: "2026-01-01T00:00:00Z", completed_at: (1767225600 + $d | todate)}]')"
    i=$((i + 1))
  done
  printf '%s' "$out" | jq -f "$sig"
}
assert "30 s floor: span 29 on a ~120 s job fires"   3 "$(synth 100 120 129 | jq '.bound_hits | length')"
assert "span 31 on a ~120 s job does not"            0 "$(synth 100 120 131 | jq '.bound_hits | length')"
assert "60 s ceiling: span 61 on a ~5000 s job fails" 0 "$(synth 5000 5030 5061 | jq '.bound_hits | length')"
assert "5% band: span 50 on a ~1000 s job fires"     3 "$(synth 1000 1025 1050 | jq '.bound_hits | length')"
assert "strict order: a cancel equal to the success" 0 "$(synth 90 90 90 | jq '.bound_hits | length')"

# ---------------------------------------------------------------------------
# Review fixes (independent review of the first cut).
# ---------------------------------------------------------------------------
echo ""
echo "Review fixes:"
# The signature counts cancelled JOBS, not runs. SYNTHETIC night 900000101
# here concludes `failure` (its `frontmatter` job failed) while
# guard-roster-windows is still cut at 10824 s: only 2 cancelled RUNS, but 3
# cancelled g-r-w JOBS. A prefilter on cancelled runs printed 0 findings here.
assert "failed night: prefilter still fans out => exit 1" 1 "$(run_detector ccfg-2026-09-13-plus-synthetic-failed-night)"
assert "failed night: the 3-streak finding fires"   yes "$(saw 'lint-frontmatter (workflow 307681598) -- 3 consecutive failing scheduled runs')"
assert "failed night: both cancels are bound-hits"  yes "$(saw '; cancelled runs: 2 bound-hit (counted as failing), 0 neutral, 0 unclassified')"
# The newest run concluded `failure`, so it needs no timeout explanation.
assert "failed night: no cancellation line for a failure" no "$(saw 'newest counted run is a bound-hit cancellation')"

# A job name may legally contain `|`; the bound-hit line must not split it.
rm -rf "$work/pipe"; cp -r "$fixtures/ccfg-2026-09-13-plus-synthetic-night" "$work/pipe"
for f in "$work/pipe/"*.jobs.json; do
  jq '(.jobs[] | select(.name == "guard-roster-windows") | .name) = "build | lint"' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
done
rc=0; bash "$detector" --fixture-dir "$work/pipe" > "$work/out.txt" 2>&1 || rc=$?
assert "pipe in a job name => still a finding"      1 "$rc"
assert "and the name and numbers survive intact"    yes "$(saw '(job build | lint cancelled at 10824 s, in a 3-member cluster spanning 2 s)')"

# A tight cluster with NO earlier success is not counted, but is printed per
# job -- the worst case (a bound that never fit) must not be a bare number.
rc=0; bash "$detector" --fixture-dir "$work/nosucc" > "$work/out.txt" 2>&1 || rc=$?
assert "undecided cluster => exit code unchanged (0)" 0 "$rc"
assert "and an UNDECIDED line names workflow and job" yes "$(saw 'schedule-red-streak: UNDECIDED lint-frontmatter (workflow 307681598) job guard-roster-windows: 3 cancellations clustered within 2 s and no earlier success in the window to order them against')"
assert "and it is not counted as a finding"         yes "$(saw ': 0 finding(s)')"
assert "and the summary paren is untouched"         yes "$(saw '(skipped 0 push-baselined, 0 with no scheduled runs); min-streak=3 window=40; cancelled runs:')"

# Jobs cancelled while still QUEUED report started_at == completed_at. They
# never ran, so they cannot have hit a bound; three identical zeros must not
# form the tightest "cluster" of all.
zeros='[{"run_id":1,"created_at":"2026-01-01T00:00:00Z","name":"m","conclusion":"cancelled","started_at":"2026-01-01T00:00:00Z","completed_at":"2026-01-01T00:00:00Z"},
        {"run_id":2,"created_at":"2026-01-02T00:00:00Z","name":"m","conclusion":"cancelled","started_at":"2026-01-02T00:00:00Z","completed_at":"2026-01-02T00:00:00Z"},
        {"run_id":3,"created_at":"2026-01-03T00:00:00Z","name":"m","conclusion":"cancelled","started_at":"2026-01-03T00:00:00Z","completed_at":"2026-01-03T00:00:00Z"},
        {"run_id":4,"created_at":"2026-01-04T00:00:00Z","name":"m","conclusion":"cancelled","started_at":null,"completed_at":"2026-01-04T00:00:00Z"}]'
assert "never-started cancels form no cluster"      "0 0 0" "$(printf '%s' "$zeros" | jq -f "$sig" | jq -r '"\(.bound_hits | length) \(.undecided | length) \([.jobs[].cancelled_n] | add // 0)"')"

# The merge-train steward runs the detector from Git Bash on Windows, where jq
# is a native jq.exe that writes CRLF. A stub with that behaviour, first on
# PATH, must change nothing: without the detector's jq wrapper the streak reads
# `9\r` and dies exit 2, and the tally reads `cancelled\r` and counts no
# bound-hit. The stub keeps jq's exit status, which the detector relies on.
echo ""
echo "Under a CRLF-writing jq (a native Windows jq.exe):"
mkdir -p "$work/crlf-bin"
cat > "$work/crlf-bin/jq" <<EOF_STUB
#!/usr/bin/env bash
set -o pipefail
"$real_jq" "\$@" | sed 's/\$/\r/'
EOF_STUB
chmod +x "$work/crlf-bin/jq"
assert "the stub really writes CRLF"                yes "$(PATH="$work/crlf-bin:$PATH" command jq -n '1' | grep -q $'\r' && echo yes || echo no)"
rc=0; PATH="$work/crlf-bin:$PATH" bash "$detector" --fixture-dir "$fixtures/real-2026-08-24" > "$work/out.txt" 2>&1 || rc=$?
assert "real window: still exit 1, not 2"           1 "$rc"
assert "real window: still a streak of 9"           yes "$(saw '317525761) -- 9 consecutive failing scheduled runs on main')"
rc=0; PATH="$work/crlf-bin:$PATH" bash "$detector" --fixture-dir "$fixtures/ccfg-2026-09-13-plus-synthetic-night" > "$work/out.txt" 2>&1 || rc=$?
assert "night 3: still exit 1"                      1 "$rc"
assert "night 3: still tallies 3 bound-hits"        yes "$(saw '; cancelled runs: 3 bound-hit (counted as failing), 0 neutral, 0 unclassified')"

echo ""
if [ "$failures" -gt 0 ]; then
  echo "::error::schedule-red-streak detector test: $failures failure(s)."
  exit 1
fi
echo "schedule-red-streak detector test: all assertions passed."
exit 0
