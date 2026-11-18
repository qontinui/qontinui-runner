# reviewdog-step-outputs.sh — SOURCED (not run) by a workflow step that runs
# reviewdog with `-reporter=github-pr-check`. It writes the outputs that
# complete-orphaned-check-runs.sh, in a later step, reads.
#
# CANONICAL COPY: qontinui-coord/.github/scripts/reviewdog-step-outputs.sh,
# tested by .github/scripts/test-reviewdog-step-outputs.sh (ci.yml,
# ci-scripts-tests). Both files are copied BYTE-IDENTICAL into qontinui-runner,
# whose clippy-tiers.yml checks them against
# .github/scripts/complete-orphaned-check-runs.sha256 there. Edit them here
# first, then copy them and update that pin file; never edit a copy alone.
# Plan: 2026-11-16-finish-session-refuses-a-finish-whose-only-owed-pr-is-ci-pending.
#
# USE, inside the step's `run:` (bash, `set -e` as GitHub runs it):
#   source .github/scripts/reviewdog-step-outputs.sh
#   rso_begin "${RUNNER_TEMP}/<name>.stderr"   # rc=unknown; arms the EXIT trap
#   ...an early exit sets rc=not-run-<why> first...
#   rso_run reviewdog <args...> <"$INPUT"      # marks the start, runs it, sets rc
#
# rso_run marks the window's start, runs the command with stderr captured to
# $RSO_STDERR_LOG, then replays that file into the job log. rc stays `unknown`
# until the command RETURNS, so a cancel or a signal that ends the step while
# reviewdog runs is recorded as `unknown`, never as a pass.
#
# OUTPUTS, appended to $GITHUB_OUTPUT exactly once by the EXIT trap whenever
# the step's shell runs it: a normal end, an `exit`, a death under `set -e`,
# and a SIGTERM or SIGINT (bash runs the EXIT trap on those; the test sends a
# SIGTERM to the step's shell, and a SIGINT to its process group, while the
# command runs). A SIGKILL, or the runner dying, writes nothing; the
# completion step then reads an empty rc as unknown.
#   rc          reviewdog's exit status; `unknown` when the step ended before
#               reviewdog returned; `not-run-<why>` when it never ran.
#   started_at  RFC 3339 UTC, right before reviewdog ran; empty if it never ran.
#   ended_at    RFC 3339 UTC, when the step ended.
#   clock       `github` when every time above came from GitHub's clock,
#               `runner` when any came from this runner's own clock.
#   stderr_log  the file reviewdog's stderr was captured to.
# RSO_OUTPUT_NAMES lists them; test-reviewdog-step-outputs.sh checks that every
# `steps.<id>.outputs.*` a workflow reads from a step sourcing this file is one
# of them.
#
# CLOCK: a check run's `started_at` is stamped by GitHub (reviewdog v0.21.2
# sends none; check run 132651515896, which it created, reads `started_at:
# 2026-11-15T01:06:20Z` via `gh api
# repos/qontinui/qontinui-coord/check-runs/132651515896`, read by an
# independent reviewer on 2026-11-16), so the window is read
# from GitHub's clock — the `Date` header of an unauthenticated api.github.com
# response, which every status carries (a rate-limited 403 included), bounded
# at 10 s for the start time and 3 s for the end time (the EXIT trap runs as
# the step is ending, including on a cancel, so it must not linger). Without a header this falls back to the runner's clock and records
# `clock=runner`, and the consumer widens its skew.

# shellcheck shell=bash
# shellcheck disable=SC2034  # read by the sourcing step's readers and by the test
RSO_OUTPUT_NAMES="rc started_at ended_at clock stderr_log"

# rso_github_now [max seconds]: print "<github|runner> <RFC 3339 UTC>".
rso_github_now() {
  local d="" max="${1:-10}"
  d="$(curl -sS -o /dev/null -D - --max-time "$max" https://api.github.com/ 2>/dev/null \
    | tr -d '\r' | sed -n 's/^[Dd]ate: //p' | head -n 1)" || true
  if [ -n "$d" ] && d="$(date -u -d "$d" +%FT%TZ 2>/dev/null)"; then
    echo "github $d"
  else
    echo "runner $(date -u +%FT%TZ)"
  fi
}

# rso_begin <stderr log path>: initialise the step's state and arm the trap.
rso_begin() {
  rc=unknown
  RSO_STARTED_AT=""
  RSO_CLOCK=github
  RSO_STDERR_LOG="$1"
  rm -f -- "$RSO_STDERR_LOG"
  trap rso_finish EXIT
}

# rso_mark_start: record the window's lower bound, right before reviewdog runs.
rso_mark_start() {
  local src
  read -r src RSO_STARTED_AT <<<"$(rso_github_now)"
  [ "$src" = github ] || RSO_CLOCK=runner
}

# rso_run <command...>: mark the start, run the command with its stderr in
# $RSO_STDERR_LOG, and set rc only once it has returned. Then replay the log.
rso_run() {
  rso_mark_start
  if "$@" 2>"$RSO_STDERR_LOG"; then
    rc=0
  else
    rc=$?
  fi
  cat -- "$RSO_STDERR_LOG" >&2 || true
}

# rso_finish: the EXIT trap. Writes every output once.
# shellcheck disable=SC2317  # run by the EXIT trap rso_begin arms
rso_finish() {
  local src ended_at
  read -r src ended_at <<<"$(rso_github_now 3)"
  [ "$src" = github ] || RSO_CLOCK=runner
  {
    echo "rc=${rc:-unknown}"
    echo "started_at=${RSO_STARTED_AT}"
    echo "ended_at=${ended_at}"
    echo "clock=${RSO_CLOCK}"
    echo "stderr_log=${RSO_STDERR_LOG}"
  } >>"$GITHUB_OUTPUT"
}
