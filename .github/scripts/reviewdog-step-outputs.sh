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
#   rso_begin "${RUNNER_TEMP}/<name>.stderr"   # rc=unknown; arms the EXIT and INT traps
#   ...an early exit sets rc=not-run-<why> first...
#   rso_run reviewdog <args...> <"$INPUT"      # marks the start, runs it, sets rc
# or, where reviewdog's exit status is the step's verdict:
#   rso_run_retrying "$INPUT" reviewdog <args...>   # the same, retrying a failed post
#
# rso_run marks the window's start, runs the command with stderr captured to
# $RSO_STDERR_LOG, then replays that file into the job log. rc stays `unknown`
# until the command RETURNS, so a cancel or a signal that ends the step while
# reviewdog runs is recorded as `unknown`, never as a pass.
#
# rso_run_retrying does the same for a command that reads <input file> on stdin,
# and runs it again when an attempt exits non-zero AND that attempt's stderr
# matches RSO_UNFINISHED_RUN_RE: reviewdog reporting that it did not finish its
# check run (`failed to create check`, `failed to post`, `check-runs/<id>`).
# That is the grep complete-orphaned-check-runs.sh uses for the same question,
# and the test checks the two still agree. Why: GitHub has answered reviewdog's
# PATCH of a check run it created a second earlier with `404 Not Found` (coord
# findings c58cd00f-7651-4dde-bd3d-d69fcc59f291, 2f3d3b96-7bd6-420e-aff3-f20d29544b74),
# and a step whose verdict is reviewdog's exit status then reds on GitHub's lag
# exactly as it would on a lint finding. Any other exit -- 0, a findings exit 1
# with no such line, any failure with no such line -- returns at once, so a
# finding is never retried and never read as a failed post. Each attempt reads
# <input file> afresh. Between attempts it sleeps RSO_RETRY_DELAYS (default
# "5 10", whole seconds: at most 3 attempts; empty means no retry) with
# RSO_RETRY_SLEEP (default `sleep`). rc is the LAST attempt's status. What the
# completion step depends on is kept for every attempt: the start is marked
# once, before the first, so the window covers them all; each attempt's stderr
# is APPENDED to $RSO_STDERR_LOG as it is written, so every attempt's
# `check-runs/<id>` reaches the completion step, a cancel mid-attempt included;
# and rc stays `unknown` until the last attempt returns, sleeps included. Each
# attempt's stderr is replayed into the job log once it returns. It also sets
# two shell variables for the step, which are not outputs: RSO_ATTEMPTS, the
# attempts run, and RSO_POST_FAILED, 1 when the last attempt still reported an
# unfinished run (the retries are spent) and 0 otherwise. Whenever a retry
# happened, a `::notice` starting `retry outcome:` says how it ended, so the
# job's annotations show whether retrying recovers from GitHub's lag. An unreadable
# <input file> runs nothing: rc=not-run-input-unreadable, no start time, and a
# non-zero return.
#
# OUTPUTS, appended to $GITHUB_OUTPUT exactly once by the EXIT trap whenever
# the step's shell runs it: a normal end, an `exit`, a death under `set -e`,
# and a SIGTERM or SIGINT (bash runs the EXIT trap on those, and rso_begin
# traps SIGINT so that one cannot be swallowed; the test sends a SIGTERM to
# the step's shell, and a SIGINT to its process group and to the shell alone,
# while the command runs and during a retry sleep). A SIGKILL, or the runner dying, writes nothing; the
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
# at 10 s for the start time and 2 s for the end time (the EXIT trap runs as
# the step is ending, including on a cancel, so it must not linger: after its
# SIGTERM the runner kills the step 2.5 s later, and the EXIT trap ignores
# SIGINT and SIGTERM while it writes, so a second signal cannot cut it short). Without a header this falls back to the runner's clock and records
# `clock=runner`, and the consumer widens its skew.

# shellcheck shell=bash
# shellcheck disable=SC2034  # read by the sourcing step's readers and by the test
RSO_OUTPUT_NAMES="rc started_at ended_at clock stderr_log"
# An attempt whose stderr matches this did not finish its check run. The same
# pattern as complete-orphaned-check-runs.sh's own grep; the test compares them.
RSO_UNFINISHED_RUN_RE='failed to create check|failed to post|check-runs/[0-9]+'

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
  # A job cancel sends SIGINT to this shell alone, then SIGTERM. Untrapped,
  # bash defers a SIGINT behind a foreground child that did not die of it and
  # then CARRIES ON, so the step would run its next command (a retry's next
  # attempt, say) and record that command's status. Trapped, the step ends
  # once the running command returns, with rc as it stands. SIGTERM stays
  # untrapped: bash already ends at once on it and runs the EXIT trap, where
  # a trap would wait for the running command.
  trap 'exit 130' INT
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

# rso_run_retrying <input file> <command...>: rso_run, plus a bounded retry of
# an attempt that reported an unfinished check run (see the header).
rso_run_retrying() {
  local input="$1" n=1 max off arc slice d
  local -a delays
  shift
  RSO_ATTEMPTS=0
  RSO_POST_FAILED=0
  if [ ! -r "$input" ]; then
    rc=not-run-input-unreadable
    echo "::warning title=reviewdog-step-outputs::the input '${input}' is not readable, so reviewdog did not run"
    return 1
  fi
  read -r -a delays <<<"${RSO_RETRY_DELAYS-5 10}"
  max=$((${#delays[@]} + 1))
  rso_mark_start
  : >"$RSO_STDERR_LOG"
  while :; do
    RSO_ATTEMPTS="$n"
    off="$(wc -c <"$RSO_STDERR_LOG")"
    off=$((off))
    if "$@" <"$input" 2>>"$RSO_STDERR_LOG"; then
      arc=0
    else
      arc=$?
    fi
    slice="${RSO_STDERR_LOG}.attempt"
    tail -c +"$((off + 1))" -- "$RSO_STDERR_LOG" >"$slice" || true
    cat -- "$slice" >&2 || true
    if [ "$arc" -eq 0 ] || ! grep -qE "$RSO_UNFINISHED_RUN_RE" -- "$slice"; then
      rm -f -- "$slice"
      if [ "$n" -gt 1 ]; then
        echo "::notice title=reviewdog-step-outputs::retry outcome: reviewdog finished its check run on attempt ${n}/${max} (exit ${arc}), after $((n - 1)) attempt(s) that did not"
      fi
      rc="$arc"
      return 0
    fi
    rm -f -- "$slice"
    if [ "$n" -ge "$max" ]; then
      RSO_POST_FAILED=1
      echo "::notice title=reviewdog-step-outputs::retry outcome: reviewdog did not finish its check run on any of ${max} attempt(s) (last exit ${arc})"
      rc="$arc"
      return 0
    fi
    d="${delays[$((n - 1))]}"
    echo "::warning title=reviewdog-step-outputs::reviewdog attempt ${n}/${max} exited ${arc} and reported that it did not finish its check run (a failed check create or post, not a lint finding); retrying from the same input in ${d}s"
    "${RSO_RETRY_SLEEP:-sleep}" "$d" || true
    n=$((n + 1))
  done
}

# rso_finish: the EXIT trap. Writes every output once.
# shellcheck disable=SC2317  # run by the EXIT trap rso_begin arms
rso_finish() {
  local src ended_at
  # A cancel's signal can arrive while this runs; it must not end the shell
  # before the outputs are written.
  trap '' INT TERM
  read -r src ended_at <<<"$(rso_github_now 2)"
  [ "$src" = github ] || RSO_CLOCK=runner
  {
    echo "rc=${rc:-unknown}"
    echo "started_at=${RSO_STARTED_AT}"
    echo "ended_at=${ended_at}"
    echo "clock=${RSO_CLOCK}"
    echo "stderr_log=${RSO_STDERR_LOG}"
  } >>"$GITHUB_OUTPUT"
}
