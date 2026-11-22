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
#   rso_run_retrying "$INPUT" reviewdog <args...>   # the same, classifying a no-verdict exit
#
# rso_run marks the window's start, runs the command with stderr captured to
# $RSO_STDERR_LOG, then replays that file into the job log. rc stays `unknown`
# until the command RETURNS, so a cancel or a signal that ends the step while
# reviewdog runs is recorded as `unknown`, never as a pass.
#
# rso_run_retrying does the same for a command that reads <input file> on stdin,
# and classifies each attempt that exits NON-ZERO by a POSITIVE marker in that
# attempt's own stderr (the bytes it appended to $RSO_STDERR_LOG), tried in
# this order:
#   post        RSO_UNFINISHED_RUN_RE: reviewdog reporting that it did not
#               finish its check run (`failed to create check`, `failed to
#               post`, `check-runs/<id>`). That is the grep
#               complete-orphaned-check-runs.sh uses for the same question, and
#               the test checks the two still agree. GitHub has answered
#               reviewdog's PATCH of a check run it created a second earlier
#               with `404 Not Found` (coord findings
#               c58cd00f-7651-4dde-bd3d-d69fcc59f291,
#               2f3d3b96-7bd6-420e-aff3-f20d29544b74). RETRIED.
#   diff-fetch  RSO_DIFF_FETCH_RE, a stderr line BEGINNING
#               `reviewdog: fail to get diff: `: the PR diff fetch (a GitHub
#               API call, or on a 406 its `git fetch`/`git diff` fallback)
#               failed, before any check run existed. RETRIED: a 5xx, a
#               secondary rate limit or a reset can clear between attempts.
#   diff-parse  RSO_DIFF_PARSE_RE, a stderr line BEGINNING
#               `reviewdog: fail to parse diff: `: the fetched diff did not
#               parse. NOT retried: the same bytes parse the same way again.
# Every class means reviewdog delivered NO verdict on the PR. reviewdog v0.21.2
# exits 1 for each of them and for a findings result alike (cmd/reviewdog/
# main.go prints every error as `reviewdog: <err>` and exits 1), so a step whose
# verdict is reviewdog's exit status would red on them exactly as on a lint
# finding. The two diff markers are anchored at the start of a line, so a slog
# line (`time=... level=... msg="reviewdog: ..."`) quoting one cannot classify.
# `reviewdog: parse error:` (reading stdin) has no class: no errorformat parser
# (`-f=clippy` included) can return it in v0.21.2. An exit matching no marker --
# 0, a findings exit 1 however it is worded, any other failure -- is the verdict
# and returns at once. Classification only ever widens "finding", never narrows
# it, so a finding is never retried and never read as no verdict. The wording
# was captured from the v0.21.2 binary; re-check it on any version bump (a
# reworded line falls back to "finding", the safe direction).
#
# A retried class runs the command again. Each attempt reads <input file>
# afresh. Between attempts it sleeps RSO_RETRY_DELAYS (default "5 10", whole
# seconds: at most 3 attempts; empty means no retry) with RSO_RETRY_SLEEP
# (default `sleep`). rc is the LAST attempt's status. What the completion step
# depends on is kept for every attempt: the start is marked once, before the
# first, so the window covers them all; each attempt's stderr is APPENDED to
# $RSO_STDERR_LOG as it is written, so every attempt's `check-runs/<id>` reaches
# the completion step, a cancel mid-attempt included; and rc stays `unknown`
# until the last attempt returns, sleeps included. Each attempt's stderr is
# replayed into the job log once it returns. It also sets two shell variables
# for the step, which are not outputs: RSO_ATTEMPTS, the attempts run, and
# RSO_NO_VERDICT, the LAST attempt's class name (`post`, `diff-fetch`,
# `diff-parse`) when it ended with no verdict, and empty otherwise (so a failed
# post followed by a findings exit is a finding). Before each retry a
# `::warning` names the class, and whenever a retry happened a `::notice`
# starting `retry outcome:` says how it ended and names every retried
# attempt's class, so the job's annotations show which class a retry recovers
# from. An unreadable <input file> runs nothing: rc=not-run-input-unreadable,
# no start time, and a non-zero return.
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
# reviewdog v0.21.2's own `reviewdog: <err>` lines for the two diff failures
# (reviewdog.go Run, printed by cmd/reviewdog/main.go), anchored at a line's
# start. A class RSO_RETRIED_CLASSES names is retried; any other is not.
RSO_DIFF_FETCH_RE='^reviewdog: fail to get diff: '
RSO_DIFF_PARSE_RE='^reviewdog: fail to parse diff: '
RSO_RETRIED_CLASSES="post diff-fetch"

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

# rso_no_verdict_class <stderr slice>: print the no-verdict class the slice's
# marker names (see the header), or nothing when it names none.
rso_no_verdict_class() {
  if grep -qE "$RSO_UNFINISHED_RUN_RE" -- "$1"; then
    echo post
  elif grep -qE "$RSO_DIFF_FETCH_RE" -- "$1"; then
    echo diff-fetch
  elif grep -qE "$RSO_DIFF_PARSE_RE" -- "$1"; then
    echo diff-parse
  fi
}

# rso_run_retrying <input file> <command...>: rso_run, plus classifying each
# failed attempt and a bounded retry of a retried class (see the header).
rso_run_retrying() {
  local input="$1" n=1 max off arc slice d cls seen="" why
  local -a delays
  shift
  RSO_ATTEMPTS=0
  RSO_NO_VERDICT=""
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
    cls=""
    if [ "$arc" -ne 0 ]; then
      cls="$(rso_no_verdict_class "$slice")"
    fi
    rm -f -- "$slice"
    RSO_NO_VERDICT="$cls"
    if [ -z "$cls" ] || [[ " ${RSO_RETRIED_CLASSES} " != *" ${cls} "* ]]; then
      if [ "$n" -gt 1 ] && [ -z "$cls" ]; then
        echo "::notice title=reviewdog-step-outputs::retry outcome: reviewdog reached a verdict on attempt ${n}/${max} (exit ${arc}), after $((n - 1)) attempt(s) with no verdict (${seen})"
      elif [ "$n" -gt 1 ]; then
        echo "::notice title=reviewdog-step-outputs::retry outcome: reviewdog ended with no verdict (${cls}, not retried) on attempt ${n}/${max} (exit ${arc}), after $((n - 1)) attempt(s) with no verdict (${seen})"
      fi
      rc="$arc"
      return 0
    fi
    seen="${seen:+${seen},}${cls}"
    if [ "$n" -ge "$max" ]; then
      echo "::notice title=reviewdog-step-outputs::retry outcome: reviewdog reached no verdict on any of ${max} attempt(s) (${seen}; last exit ${arc})"
      rc="$arc"
      return 0
    fi
    case "$cls" in
      post) why="it did not finish its check run: a failed check create or post" ;;
      diff-fetch) why="it could not fetch the PR diff" ;;
      *) why="see reviewdog-step-outputs.sh's header" ;;
    esac
    d="${delays[$((n - 1))]}"
    echo "::warning title=reviewdog-step-outputs::reviewdog attempt ${n}/${max} exited ${arc} with no verdict, class ${cls} (${why}, not a lint finding); retrying from the same input in ${d}s"
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
