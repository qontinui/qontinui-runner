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

echo ""
if [ "$failures" -gt 0 ]; then
  echo "::error::schedule-red-streak detector test: $failures failure(s)."
  exit 1
fi
echo "schedule-red-streak detector test: all assertions passed."
exit 0
