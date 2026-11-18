#!/usr/bin/env bash
# complete-orphaned-check-runs.sh — complete a check run that the step posting it
# left `in_progress`, so it stops reading as "CI is still running".
#
# CANONICAL COPY: qontinui-coord/.github/scripts/complete-orphaned-check-runs.sh,
# tested by .github/scripts/test-complete-orphaned-check-runs.sh (ci.yml,
# ci-scripts-tests). Both files are copied BYTE-IDENTICAL into qontinui-runner,
# whose clippy-tiers.yml checks them against
# .github/scripts/complete-orphaned-check-runs.sha256 there. Edit them here
# first, then copy them and update that pin file; never edit a copy alone.
# Plan: 2026-11-16-finish-session-refuses-a-finish-whose-only-owed-pr-is-ci-pending
# (Phase 2 for coord, Phase 2b for the runner).
#
# WHY: reviewdog's `-reporter=github-pr-check` CREATES its check run through the
# API and completes it with a later PATCH. When that PATCH fails, the run stays
# `in_progress` with nothing left running, for good. On qontinui-coord#4159,
# rust-ci job 132648536974 logged `reviewdog: failed to post result: PATCH
# .../check-runs/132651515896: 404 Not Found` at 01:06:21.105Z, about 1 s after
# the step started at 01:06:20.22Z (log line 4367; verified by an independent
# reviewer's read of job 132648536974's log on 2026-11-17, and recorded in
# coord finding c58cd00f-7651-4dde-bd3d-d69fcc59f291); the likely cause is
# read-after-write lag at GitHub (a hypothesis, not a measurement). That
# finding also records that coord held qontinui-coord#4159 and #4053 at
# block_reason_code ci-pending with the orphan as the only non-terminal check.
#
# WHEN. Only when reviewdog itself did not finish its run. reviewdog v0.21.2
# sends no started_at, so GitHub stamps it: check run 132651515896, created by
# that version, reads `started_at: 2026-11-15T01:06:20Z` via `gh api
# repos/qontinui/qontinui-coord/check-runs/132651515896` (read by an
# independent reviewer on 2026-11-16). Every failure after it creates
# its check surfaces on stderr as `failed to create check:` or `failed to post
# result:`; a failed annotation call names `check-runs/<id>`. (A 403 at create
# is not among them: v0.21.2 in Actions falls back to log annotations and
# exits 0, so it makes no run to orphan; a failed create that is reported is
# a 5xx or network failure, and only then can a wait below happen.) So:
#   - a WIRING error: STEP_OUTCOME `success` (the posting step ran to its
#     end, so its EXIT trap wrote outputs) but STEP_RC, the stderr log and both
#     times are all empty, so the completion step is reading outputs that do
#     not exist: a `::warning` names it, and no API call is made. An outcome
#     of `failure`, `cancelled` or empty with no outputs is NOT taken for one:
#     a step shell killed by SIGKILL, or one that died before the helper's
#     EXIT trap was armed, fails and writes none. (A failed `source` of the
#     helper itself writes rc=not-run-source-failed instead.) That case is
#     treated as unknown and looked for as below, with the time guard off (no
#     bounds were written), and the guard-off warning says so.
#   - reviewdog never ran (STEP_RC `not-run-*`; STEP_OUTCOME `skipped`; or
#     STEP_RC `unknown` or empty with an end time but no start time, i.e. the
#     step ended before it reached reviewdog): nothing is read or completed,
#     and no API call is made;
#   - STEP_RC is a number below 128 and REVIEWDOG_STDERR_LOG holds none of
#     those markers (or there is no such file): reviewdog finished its run, so
#     no API call is made either; a one-line notice says so. That covers rc 0
#     and a runner rc 1 for diff findings alike;
#   - otherwise -- a marker in stderr; STEP_RC `unknown` or empty after a start
#     time, when the step ended before reviewdog returned; or STEP_RC 128 or
#     more, reviewdog killed by a signal (137, 143) -- the run is looked for
#     as below.
#
# WHICH RUNS. The aim is the run THIS job's reviewdog created. What is
# guaranteed is narrower, and is exactly this:
#   1. EXACT ID. When REVIEWDOG_STDERR_LOG names a file holding reviewdog's
#      stderr and a line there names `check-runs/<id>` (the failing call), only
#      those ids are candidates, and no listing is made.
#   2. LISTING, when no id parses: runs named <check_name> on <head_sha>
#      (`filter=all`, paginated) that belong to app `github-actions` and whose
#      `started_at` lies in [REVIEWDOG_STARTED_AT - skew, REVIEWDOG_ENDED_AT +
#      skew]. A run outside that window cannot be this job's. A run INSIDE it
#      can still belong to a concurrent workflow run on the same head whose
#      reviewdog overlapped this one; the window narrows that case, it does not
#      exclude it. A run with an unreadable `started_at` is skipped.
#      CLOCK: both bounds should be read from GitHub's clock (the `Date` header
#      of an API response), the clock `started_at` is stamped with; then the
#      skew is ${SKEW_GITHUB_SECS} s (the header's 1 s resolution plus margin).
#      When REVIEWDOG_CLOCK is not `github`, a bound came from the runner's own
#      clock, whose offset from GitHub's is unknown, so the skew widens to
#      ${SKEW_RUNNER_SECS} s: fewer of this job's runs missed for a wider
#      window a concurrent run can fall into.
#      An absent or unparseable bound turns that SIDE of the guard OFF: the
#      listing is still used, so a known orphan is not silently left behind,
#      but the script prints a `::warning` naming the missing bound and every
#      summary it writes says so too. UNKNOWN neither skips everything nor
#      widens silently.
#   Every candidate is READ AGAIN immediately before every PATCH, the first one
#   included. It is skipped when it is already `completed` (a run reviewdog
#   finished keeps reviewdog's own conclusion), or when its name, app or head
#   sha is not the expected one. A run completed as of that re-read is not
#   PATCHed; a run a concurrent job completes between the re-read and the
#   PATCH can still be overwritten.
#
# RETRIES, with a bounded backoff (default 2, 5, 10 s) on the list, the re-read
# and the PATCH alike: a 404 (the error that made the orphan is what this
# script's own first call can meet); a 5xx; a 403 or 429 whose text names a
# secondary rate limit or a retry-after; and a failure with no HTTP status
# whose text names a connection-class failure (connection reset or refused,
# a timeout, a TLS handshake failure, an unexpected EOF, no such host, a DNS
# or network failure, a broken pipe, curl's could not resolve host). Every gh
# call runs under `timeout -k 5 20` (at most 25 s each; a curl call under
# `--max-time 20`), and a call that hits that bound is a retryable no-status
# failure. A no-status failure that is gh's own auth or
# config error is not retried. WALL CLOCK: the sleep budget does not bound
# the calls themselves, so no new API call is STARTED once
# COMPLETE_ORPHANS_DEADLINE_SECS (default 180) seconds have passed since the
# script began; it then warns and exits 0. The last call can still take 25 s and a sleep can be under way, so the
# script ends in about 215 s at most, inside the step's 4-minute timeout. A
# failed re-read is retried, never skipped: nothing is PATCHed without a fresh
# read. All sleeps of one invocation share one budget (default 30 s). When the
# posting step's stderr reports a failed create or post -- or reviewdog started
# and the rc is unknown or 128+ -- and the listing shows NO
# run of that name at all, the listing is retried too, since the run that step
# created may not be visible yet. Runs already `completed` (an earlier
# attempt's) do not count as visible for that wait; only a non-completed
# candidate ends it, and runs the guard skipped do not end it either. When
# no failure was reported and runs were visible but the guard skipped
# them, it says so and stops without waiting. A page after the first that
# cannot be read ends the listing with a warning, and the candidates already
# read are still completed. After the last attempt the script prints a
# `::warning` and exits 0.
#
# FAILURE: advisory, like the step it follows. Every API, parse or tool failure
# prints one `::warning` and exits 0; only a wiring error (wrong arguments)
# exits 2.
# ARGUMENTS are checked against an allowlist first (see the ALLOWLIST comment
# below): a repo, head_sha or check_name outside it is also a wiring error.
#
# USAGE: complete-orphaned-check-runs.sh <owner/repo> <head_sha> <check_name> <reason>
#   <reason> is one sentence on why the posting step may have left the run, and
#   goes into the summary verbatim.
# ENV:
#   GH_TOKEN              the job's GITHUB_TOKEN (needs `checks: write`). gh reads
#                         it; when gh is not on PATH the same REST calls go over
#                         curl with it (sent in a curl config file, never on argv).
#   GITHUB_API_URL        the REST base for that curl path (default
#                         https://api.github.com).
#   STEP_RC               the posting step's exit status; empty reads as unknown.
#   STEP_OUTCOME          the posting step's outcome; `skipped` means it never ran.
#   REVIEWDOG_STARTED_AT  just before reviewdog ran, RFC 3339 UTC; the lower bound.
#   REVIEWDOG_ENDED_AT    when the posting step ended, RFC 3339 UTC; the upper bound.
#   REVIEWDOG_CLOCK       `github` when both bounds came from GitHub's clock.
#   REVIEWDOG_STDERR_LOG  optional; a file holding reviewdog's stderr.
#   JOB_CHECK_RUN_ID      optional; the job's own check run id (`job.check_run_id`),
#                         used for a job URL. Without it the run-attempt URL is used.
#   GITHUB_SERVER_URL, GITHUB_REPOSITORY, GITHUB_RUN_ID, GITHUB_RUN_ATTEMPT
#                         set by Actions; they build that URL.
#   COMPLETE_ORPHANS_RETRY_DELAYS  seconds between attempts (default "2 5 10").
#   COMPLETE_ORPHANS_SLEEP_BUDGET  total seconds all retries may sleep (default 30).
#   COMPLETE_ORPHANS_SLEEP         the sleep command (default `sleep`; tests inject one).
#   COMPLETE_ORPHANS_PER_PAGE      list page size (default 100, GitHub's maximum).
#   COMPLETE_ORPHANS_MAX_PAGES     page cap (default 10).
#   COMPLETE_ORPHANS_CALL_TIMEOUT  seconds one gh call may take (default 20).
#   COMPLETE_ORPHANS_DEADLINE_SECS no new API call after this many seconds (default 180).
set -uo pipefail
# The allowlist ranges below are ASCII ranges only in the C locale.
export LC_ALL=C

TITLE='complete-orphaned-check-runs'
APP_SLUG='github-actions'
SKEW_GITHUB_SECS=5
SKEW_RUNNER_SECS=30

warn() {
  echo "::warning title=${TITLE}::$1"
}

give_up() {
  warn "$1 This step is advisory: nothing failed, but a check run of this name may still read as in progress."
  exit 0
}

if [ "$#" -ne 4 ] || [ -z "$1" ] || [ -z "$2" ] || [ -z "$3" ]; then
  echo "usage: $0 <owner/repo> <head_sha> <check_name> <reason>" >&2
  exit 2
fi
repo="$1" head_sha="$2" check_name="$3" reason="$4"

# ALLOWLIST: repo and head_sha are interpolated into API paths, and check_name
# into ::warning lines and summaries, so each must match a narrow shape before
# any use. A value outside it is a wiring error (exit 2), never sanitized into
# something else. check_name is also URL-encoded for the query string.
[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { echo "refusing repo '${repo//[^[:print:]]/?}': expected <owner>/<repo> of [A-Za-z0-9_.-]" >&2; exit 2; }
[[ "$head_sha" =~ ^[0-9a-fA-F]{7,64}$ ]] \
  || { echo "refusing head_sha '${head_sha//[^[:print:]]/?}': expected 7 to 64 hex digits" >&2; exit 2; }
[[ "$check_name" =~ ^[A-Za-z0-9\ ._\(\)/:,+#-]{1,100}$ ]] \
  || { echo "refusing check_name '${check_name//[^[:print:]]/?}': expected 1 to 100 of [A-Za-z0-9 ._()/:,+#-] (no control characters)" >&2; exit 2; }

case "${STEP_RC:-}" in
  not-run*)
    echo "the posting step reports that reviewdog never ran (${STEP_RC}); no '${check_name}' run can be this job's, so nothing is read or completed"
    exit 0
    ;;
esac
case "${STEP_OUTCOME:-}" in
  success)
    if [ -z "${STEP_RC:-}" ] && [ -z "${REVIEWDOG_STDERR_LOG:-}" ] \
      && [ -z "${REVIEWDOG_STARTED_AT:-}" ] && [ -z "${REVIEWDOG_ENDED_AT:-}" ]; then
      warn "the posting step ended with outcome '${STEP_OUTCOME}', which always writes rc, but STEP_RC, REVIEWDOG_STDERR_LOG, REVIEWDOG_STARTED_AT and REVIEWDOG_ENDED_AT are all empty: this step reads outputs the posting step does not write (a wrong step id or output name). Nothing is read or completed; fix the wiring."
      exit 0
    fi
    ;;
esac
if { [ -z "${STEP_RC:-}" ] || [ "${STEP_RC:-}" = unknown ]; } \
  && [ -z "${REVIEWDOG_STARTED_AT:-}" ] && [ -n "${REVIEWDOG_ENDED_AT:-}" ]; then
  echo "the posting step ended (${REVIEWDOG_ENDED_AT}) with rc '${STEP_RC:-}' and no reviewdog start time, so reviewdog never ran; no '${check_name}' run can be this job's, so nothing is read or completed"
  exit 0
fi
if [ "${STEP_OUTCOME:-}" = skipped ]; then
  echo "the posting step was skipped, so reviewdog never ran; no '${check_name}' run can be this job's, so nothing is read or completed"
  exit 0
fi

# Did reviewdog say it failed to finish its run?
stderr_state=none
if [ -n "${REVIEWDOG_STDERR_LOG:-}" ] && [ -f "$REVIEWDOG_STDERR_LOG" ]; then
  if grep -qE 'failed to create check|failed to post|check-runs/[0-9]+' "$REVIEWDOG_STDERR_LOG"; then
    stderr_state=failed
  else
    stderr_state=clean
  fi
fi
case "${STEP_RC:-}" in
  '' | unknown) ;;
  *)
    if [[ "$STEP_RC" =~ ^[0-9]+$ ]] && [ "$STEP_RC" -ge 128 ]; then
      : # killed by a signal mid-run: as unknown, look for the orphan
    elif [ "$stderr_state" != failed ]; then
      echo "::notice title=${TITLE}::reviewdog exited ${STEP_RC} and its stderr (${stderr_state}) reports no failed check create or post, so it finished its '${check_name}' run; nothing is read or completed"
      exit 0
    fi
    ;;
esac

read -r -a delays <<<"${COMPLETE_ORPHANS_RETRY_DELAYS:-2 5 10}"
sleep_budget="${COMPLETE_ORPHANS_SLEEP_BUDGET:-30}"
sleep_cmd="${COMPLETE_ORPHANS_SLEEP:-sleep}"
per_page="${COMPLETE_ORPHANS_PER_PAGE:-100}"
call_timeout="${COMPLETE_ORPHANS_CALL_TIMEOUT:-20}"
deadline="${COMPLETE_ORPHANS_DEADLINE_SECS:-180}"
max_pages="${COMPLETE_ORPHANS_MAX_PAGES:-10}"
slept=0

for tool in python3 timeout; do
  command -v "$tool" >/dev/null 2>&1 \
    || give_up "\`${tool}\` is not on PATH, so no check run was read; orphaned check runs on this head will NOT be completed."
done

work="$(mktemp -d -p "${RUNNER_TEMP:-${TMPDIR:-/tmp}}")" || give_up "mktemp failed, so no check run was read; orphaned check runs on this head will NOT be completed."
trap 'rm -rf -- "$work"' EXIT
# A job cancel sends SIGINT, then SIGTERM. Without these, bash defers an
# untrapped SIGINT behind a foreground child that did not die of it and may
# carry on, leaving $work (which holds curl's credential file) behind. exit
# runs the EXIT trap above; the trap fires once the current call returns (at
# most the per-call bound).
trap 'exit 130' INT
trap 'exit 143' TERM

# The API client: gh when it is on PATH; otherwise curl with GH_TOKEN, making
# the same REST calls. With neither, nothing can be read or completed.
api_url="${GITHUB_API_URL:-https://api.github.com}"
if command -v gh >/dev/null 2>&1; then
  api_tool=gh
elif command -v curl >/dev/null 2>&1 && [ -n "${GH_TOKEN:-}" ]; then
  # The token goes into a curl config file as a quoted string, so a token
  # holding a double quote, a backslash or a newline could break out of it.
  # Refuse such a token rather than write it.
  nl='
'
  case "$GH_TOKEN" in
    *'"'* | *\\* | *"$nl"*)
      give_up "GH_TOKEN contains a double quote, a backslash or a newline, so it is not written into curl's config and the curl fallback is not used; no check run was read, and orphaned check runs on this head will NOT be completed."
      ;;
  esac
  api_tool=curl
  echo "::notice title=${TITLE}::\`gh\` is not on PATH; making the same REST calls with curl and GH_TOKEN"
  (
    umask 077
    printf 'header = "Authorization: Bearer %s"\n' "$GH_TOKEN" >"$work/curl-auth"
  ) || give_up "could not write curl's credential file, so no check run was read; orphaned check runs on this head will NOT be completed."
else
  give_up "\`gh\` is not on PATH, and curl with GH_TOKEN is not available either, so no check run was read; orphaned check runs on this head will NOT be completed."
fi

# curl_api <gh-api-style args>: the call gh api would make, over curl. It
# prints the body; on a non-2xx it prints gh's error shape, "...(HTTP <code>)",
# on stderr, so every caller reads both clients the same way. A transport
# failure leaves curl's own message (and no HTTP status) on stderr, plus, for
# the connection-class exits (6, 7, 16 HTTP/2 framing, 28 timeout, 35 SSL
# connect, 52 empty reply, 55/56 send/receive), a line the retry check reads.
# `-L` follows redirects, as gh does.
curl_api() {
  local method=GET path="" input="" code curl_rc
  while [ $# -gt 0 ]; do
    case "$1" in
      -X) method="$2"; shift ;;
      --input) input="$2"; shift ;;
      -H) shift ;;
      *) path="$1" ;;
    esac
    shift
  done
  local args=(-sS -L -K "$work/curl-auth" -X "$method" --max-time "$call_timeout"
    -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28'
    -o "$work/curl-body" -w '%{http_code}')
  [ -z "$input" ] || args+=(-H 'Content-Type: application/json' --data-binary "@${input}")
  : >"$work/curl-body"
  code="$(curl "${args[@]}" "${api_url}/${path}")" || {
    curl_rc=$?
    case "$curl_rc" in
      6 | 7 | 16 | 28 | 35 | 52 | 55 | 56) echo "curl transport failure (curl exit ${curl_rc})" >&2 ;;
    esac
    return 1
  }
  cat -- "$work/curl-body"
  case "$code" in
    2??) return 0 ;;
  esac
  echo "curl: $(python3 -c 'import json, sys; print(json.load(open(sys.argv[1])).get("message", ""))' "$work/curl-body" 2>/dev/null) (HTTP ${code})" >&2
  return 1
}

# backoff <attempt> <what>: sleep before retry number <attempt> (1-based), or
# return 1 when the schedule or the shared sleep budget is spent.
backoff() {
  local n="$1" what="$2" d
  [ "$n" -le "${#delays[@]}" ] || return 1
  d="${delays[$((n - 1))]}"
  [ $((slept + d)) -le "$sleep_budget" ] || return 1
  echo "${what}: retry ${n}/${#delays[@]} in ${d}s"
  "$sleep_cmd" "$d"
  slept=$((slept + d))
  return 0
}

# api_once <out> <gh api args...>: one call. Sets HTTP_CODE ("" when gh printed
# none) and LAST_OUT, and returns gh's own status. The body goes to <out>.
HTTP_CODE=""
LAST_OUT=""
api_once() {
  local out="$1"
  shift
  HTTP_CODE=""
  LAST_OUT="$out"
  local status=0
  # The wall-clock bound: SECONDS counts from the script's start.
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "the ${deadline}s wall-clock bound was reached, so no new API call was started" >"$work/err"
    HTTP_CODE=deadline
    return 1
  fi
  if [ "$api_tool" = gh ]; then
    timeout -k 5 "$call_timeout" gh api -H 'Accept: application/vnd.github+json' "$@" \
      >"$out" 2>"$work/err" || status=$?
  else
    curl_api "$@" >"$out" 2>"$work/err" || status=$?
  fi
  [ "$status" -ne 0 ] || return 0
  if [ "$status" -eq 124 ] || [ "$status" -eq 137 ]; then
    echo "gh api timed out after ${call_timeout}s" >"$work/err"
    return 1
  fi
  HTTP_CODE="$(grep -oE 'HTTP [0-9]{3}' "$work/err" | tail -n 1 | cut -d' ' -f2)"
  return 1
}

# retryable: is the failure api_once just recorded worth another attempt?
retryable() {
  case "$HTTP_CODE" in
    404 | 5[0-9][0-9]) return 0 ;;
    403 | 429)
      cat -- "$work/err" "$LAST_OUT" 2>/dev/null \
        | grep -qiE 'secondary rate limit|retry-after|retry after'
      return
      ;;
    '')
      grep -qiE 'connection reset|connection refused|timed out|timeout|tls handshake|unexpected eof|no such host|temporary failure in name resolution|network is unreachable|broken pipe|could not resolve host|ssl connect error|empty reply from server|http/2 framing|curl transport failure' "$work/err"
      return
      ;;
    *) return 1 ;;
  esac
}

describe_err() {
  local msg
  msg="$(tr '\n' ' ' <"$work/err" | cut -c1-300)"
  msg="${msg% }"
  if [ "$HTTP_CODE" = deadline ]; then
    echo "$msg"
    return
  fi
  echo "HTTP ${HTTP_CODE:-none}: ${msg:-no error text}"
}

# api <what> <out> <gh api args...>: api_once with the retry.
api() {
  local what="$1" out="$2" n=0
  shift 2
  while :; do
    api_once "$out" "$@" && return 0
    n=$((n + 1))
    if retryable && backoff "$n" "${what} failed (HTTP ${HTTP_CODE:-none})"; then
      continue
    fi
    return 1
  done
}

case "${STEP_RC:-}" in
  '') rc_text="unknown (the step recorded no exit status)" ;;
  unknown) rc_text="unknown (the step ended before reviewdog returned an exit status)" ;;
  *[!0-9]*) rc_text="${STEP_RC}" ;;
  *)
    rc_text="exit ${STEP_RC}"
    [ "$STEP_RC" -lt 128 ] || rc_text="exit ${STEP_RC} (killed by signal $((STEP_RC - 128)))"
    ;;
esac
# A run is expected, so an empty listing is worth waiting for (on the same
# budget), when reviewdog said it failed after (or while) creating its run,
# or when it started and then ended without a clean exit status (rc unknown,
# or 128+ for a signal) -- it may have created a run nothing has shown yet.
expect_run=0
[ "$stderr_state" != failed ] || expect_run=1
if [ -n "${REVIEWDOG_STARTED_AT:-}" ]; then
  case "${STEP_RC:-}" in
    unknown) expect_run=1 ;;
    *[!0-9]* | '') ;;
    *) [ "$STEP_RC" -lt 128 ] || expect_run=1 ;;
  esac
fi

server="${GITHUB_SERVER_URL:-https://github.com}"
if [ -n "${GITHUB_RUN_ID:-}" ] && [ -n "${JOB_CHECK_RUN_ID:-}" ]; then
  job_url="${server}/${GITHUB_REPOSITORY:-$repo}/actions/runs/${GITHUB_RUN_ID}/job/${JOB_CHECK_RUN_ID}"
elif [ -n "${GITHUB_RUN_ID:-}" ]; then
  job_url="${server}/${GITHUB_REPOSITORY:-$repo}/actions/runs/${GITHUB_RUN_ID}/attempts/${GITHUB_RUN_ATTEMPT:-1}"
else
  job_url="unknown (not run under GitHub Actions)"
fi

# epoch <RFC 3339>: epoch seconds, or nothing when unparseable.
epoch() {
  python3 - "$1" <<'PY' 2>/dev/null
import datetime
import sys

try:
    when = datetime.datetime.fromisoformat(sys.argv[1].strip().replace("Z", "+00:00"))
except ValueError:
    sys.exit(0)
if when.tzinfo is not None:
    print(int(when.timestamp()))
PY
}
start_epoch="$(epoch "${REVIEWDOG_STARTED_AT:-}")"
end_epoch="$(epoch "${REVIEWDOG_ENDED_AT:-}")"
if [ "${REVIEWDOG_CLOCK:-}" = github ]; then
  skew="$SKEW_GITHUB_SECS" clock_text="GitHub's clock"
else
  skew="$SKEW_RUNNER_SECS" clock_text="the runner's clock ('${REVIEWDOG_CLOCK:-unknown}'), so the skew is widened"
fi

# --- 1. exact ids from reviewdog's own error lines ---------------------------
found_by=""
: >"$work/runs"
if [ -n "${REVIEWDOG_STDERR_LOG:-}" ] && [ -f "$REVIEWDOG_STDERR_LOG" ]; then
  grep -oE 'check-runs/[0-9]+' "$REVIEWDOG_STDERR_LOG" | cut -d/ -f2 | sort -u \
    | sed 's/$/ unknown/' >"$work/runs"
fi
if [ -s "$work/runs" ]; then
  found_by="reviewdog's own error output names this run"
  echo "reviewdog's stderr names check run(s) $(cut -d' ' -f1 "$work/runs" | tr '\n' ' ')- completing only those"
fi

# --- 2. otherwise, the bounded listing ---------------------------------------
enc_name="$(python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "$check_name")" \
  || give_up "could not URL-encode the check name '${check_name}'."

# list_runs: fill $work/runs with "<id> <status>" lines for every candidate run
# named $check_name on the head, and $work/skips with one line per run the
# guard skipped (also printed). Returns 1 (reason in LIST_ERR) when the FIRST
# page could not be read; a later page's failure keeps what was read, warning.
LIST_ERR=""
list_runs() {
  local page=1 seen=0 parsed count total
  : >"$work/runs"
  : >"$work/skips"
  while :; do
    if [ "$page" -gt "$max_pages" ]; then
      warn "stopped listing at the page cap (${max_pages} pages of ${per_page}); runs beyond it were not examined."
      return 0
    fi
    if ! api "listing '${check_name}' page ${page}" "$work/page" -X GET \
      "repos/${repo}/commits/${head_sha}/check-runs?check_name=${enc_name}&filter=all&per_page=${per_page}&page=${page}"; then
      LIST_ERR="listing check runs '${check_name}' on ${head_sha} failed at page ${page} ($(describe_err))."
      if [ "$page" -gt 1 ]; then
        warn "${LIST_ERR} The runs read from the pages before it are still examined; any beyond it were not."
        return 0
      fi
      return 1
    fi
    if ! parsed="$(python3 - "$work/page" "$check_name" "$APP_SLUG" "$start_epoch" "$end_epoch" "$skew" <<'PY'
import datetime
import json
import sys

path, name, app_slug, start, end, skew = sys.argv[1:7]
skew = int(skew)
try:
    with open(path, encoding="utf-8") as fh:
        doc = json.load(fh)
except (OSError, ValueError) as exc:
    print(f"unparseable list body: {exc}")
    sys.exit(3)
runs = doc.get("check_runs") if isinstance(doc, dict) else None
if not isinstance(runs, list):
    print("list body has no check_runs array")
    sys.exit(3)
total = doc.get("total_count")
print(f"COUNT {len(runs)} {total if isinstance(total, int) else -1}")
low = int(start) - skew if start else None
high = int(end) + skew if end else None
for run in runs:
    if not isinstance(run, dict) or run.get("name") != name:
        continue
    rid, status = run.get("id"), run.get("status")
    app = run.get("app") if isinstance(run.get("app"), dict) else {}
    if app.get("slug") != app_slug:
        print(f"SKIP {rid} it belongs to app '{app.get('slug')}', not '{app_slug}'")
        continue
    if status != "completed" and (low is not None or high is not None):
        started = run.get("started_at")
        try:
            at = int(datetime.datetime.fromisoformat(str(started).replace("Z", "+00:00")).timestamp())
        except ValueError:
            print(f"SKIP {rid} its started_at '{started}' is unreadable, so it cannot be placed inside this job's reviewdog window")
            continue
        if low is not None and at < low:
            print(f"SKIP {rid} it started at {started}, before this job's reviewdog started (minus {skew} s), so it is not this job's")
            continue
        if high is not None and at > high:
            print(f"SKIP {rid} it started at {started}, after this job's reviewdog ended (plus {skew} s), so it is not this job's")
            continue
    print(f"RUN {rid} {status}")
PY
    )"; then
      LIST_ERR="the check-runs list for ${head_sha} could not be read: ${parsed}."
      if [ "$page" -gt 1 ]; then
        warn "${LIST_ERR} The runs read from the pages before it are still examined; any beyond it were not."
        return 0
      fi
      return 1
    fi
    read -r _ count total <<<"$(printf '%s\n' "$parsed" | grep '^COUNT ')"
    printf '%s\n' "$parsed" | sed -n 's/^RUN //p' >>"$work/runs"
    printf '%s\n' "$parsed" | sed -n 's/^SKIP \([^ ]*\) /check run \1 skipped: /p' | tee -a "$work/skips"
    seen=$((seen + count))
    if [ "$count" -lt "$per_page" ] || { [ "$total" -ge 0 ] && [ "$seen" -ge "$total" ]; }; then
      return 0
    fi
    page=$((page + 1))
  done
}

if [ -z "$found_by" ]; then
  guard_off=""
  [ -n "$start_epoch" ] || guard_off="the lower bound (when reviewdog started: '${REVIEWDOG_STARTED_AT:-}')"
  if [ -z "$end_epoch" ]; then
    guard_off="${guard_off:+${guard_off} and }the upper bound (when reviewdog ended: '${REVIEWDOG_ENDED_AT:-}')"
  fi
  if [ -z "$guard_off" ]; then
    found_by="listing '${check_name}' runs on this head that started inside this job's reviewdog window (${REVIEWDOG_STARTED_AT} to ${REVIEWDOG_ENDED_AT}, +/- ${skew} s, ${clock_text})"
  else
    found_by="listing '${check_name}' runs on this head WITHOUT ${guard_off}, which is absent or unparseable, so a concurrent run on this head on that side cannot be told apart"
    warn "the time guard is OFF for ${guard_off}, which is absent or unparseable: every non-completed '${check_name}' run from ${APP_SLUG} on ${head_sha} on that side of the window is a candidate, including one a concurrent workflow run may still be posting."
  fi
  absent_n=0
  while :; do
    list_runs || give_up "$LIST_ERR"
    # Only a non-completed candidate ends the wait: a completed one (an
    # earlier attempt's, say) is left alone and is not this job's orphan.
    if awk '$2 != "completed" { found = 1 } END { exit !found }' "$work/runs"; then
      break
    fi
    if [ -s "$work/runs" ]; then
      echo "$(wc -l <"$work/runs" | tr -d ' ') '${check_name}' run(s) on ${head_sha} are already completed and are left as they are"
    fi
    skipped=0
    [ ! -s "$work/skips" ] || skipped="$(wc -l <"$work/skips" | tr -d ' ')"
    if [ "$expect_run" -eq 1 ]; then
      # reviewdog said it failed: this job's run may simply not be visible
      # yet, so keep waiting even when other runs were skipped as not ours.
      absent_n=$((absent_n + 1))
      if backoff "$absent_n" "no candidate '${check_name}' run is visible on ${head_sha} yet (the posting step ended ${rc_text}; ${skipped} run(s) skipped by the guard)"; then
        continue
      fi
      give_up "no candidate check run named '${check_name}' is visible on ${head_sha}, although the posting step ended ${rc_text}; ${skipped} run(s) were skipped by the guard as not this job's (listed above). Either it failed before creating one, or GitHub has not shown it yet."
    fi
    if [ "$skipped" -gt 0 ]; then
      echo "no candidate '${check_name}' run on ${head_sha}: ${skipped} run(s) skipped by the guard (listed above) as not this job's; nothing to complete"
      exit 0
    fi
    echo "no check run named '${check_name}' on ${head_sha}; nothing to complete"
    exit 0
  done
fi

python3 - "$work/body.json" "$check_name" "$reason" "$rc_text" "$job_url" "$found_by" <<'PY' || give_up "could not build the PATCH body."
import json
import sys

out, name, reason, rc_text, job_url, found_by = sys.argv[1:7]
summary = (
    f"This `{name}` check run was left `in_progress` by the step that posts it, "
    f"and nothing is running for it any more.\n\n"
    f"- Why: {reason}\n"
    f"- The posting step (reviewdog): {rc_text}\n"
    f"- Job: {job_url}\n"
    f"- Found by: {found_by}\n\n"
    "It was completed as `neutral` by `.github/scripts/complete-orphaned-check-runs.sh`, "
    "so it no longer reads as still running. "
    "It carries no verdict about this diff; the job itself reports any failure. "
    "Re-run the job, or push a commit, to get this check's annotations."
)
body = {
    "status": "completed",
    "conclusion": "neutral",
    "output": {"title": "reviewdog did not finish this check", "summary": summary},
}
with open(out, "w", encoding="utf-8") as fh:
    json.dump(body, fh)
PY

# complete_run <id>: read the run, and PATCH it only if that read shows it is
# this check's, from this app, on this head, and not completed. Every PATCH
# attempt, the first included, follows a fresh successful read; a failed read
# or PATCH is retried on the shared schedule. Returns 0 when the run ends
# completed or is deliberately left alone, 1 (reason in PATCH_ERR) otherwise.
complete_run() {
  local id="$1" n=0 verdict
  while :; do
    if ! api_once "$work/one" -X GET "repos/${repo}/check-runs/${id}"; then
      n=$((n + 1))
      if retryable && backoff "$n" "re-reading check run ${id} failed (HTTP ${HTTP_CODE:-none})"; then
        continue
      fi
      PATCH_ERR="re-reading check run ${id} failed ($(describe_err)), so it was not PATCHed."
      return 1
    fi
    verdict="$(python3 - "$work/one" "$check_name" "$APP_SLUG" "$head_sha" <<'PY' 2>/dev/null
import json
import sys

path, name, app_slug, head = sys.argv[1:5]
try:
    with open(path, encoding="utf-8") as fh:
        run = json.load(fh)
except (OSError, ValueError):
    print("unreadable")
    sys.exit(0)
if not isinstance(run, dict):
    print("unreadable")
elif run.get("status") == "completed":
    print("completed")
elif run.get("name") != name:
    print(f"foreign its name is '{run.get('name')}'")
elif (run.get("app") or {}).get("slug") != app_slug:
    print(f"foreign it belongs to app '{(run.get('app') or {}).get('slug')}'")
elif run.get("head_sha") != head:
    print(f"foreign it is on head '{run.get('head_sha')}'")
else:
    print("open")
PY
    )"
    case "$verdict" in
      completed)
        echo "check run ${id} is completed; left as it is"
        return 0
        ;;
      foreign*)
        echo "check run ${id} skipped: ${verdict#foreign }, not the expected '${check_name}' from ${APP_SLUG} on ${head_sha}"
        return 0
        ;;
      open) ;;
      *)
        PATCH_ERR="re-reading check run ${id} returned a body that could not be read, so it was not PATCHed."
        return 1
        ;;
    esac
    if api_once "$work/patched" -X PATCH "repos/${repo}/check-runs/${id}" --input "$work/body.json"; then
      echo "completed check run ${id} ('${check_name}') as neutral"
      return 0
    fi
    n=$((n + 1))
    if retryable && backoff "$n" "PATCH of check run ${id} failed (HTTP ${HTTP_CODE:-none})"; then
      continue
    fi
    PATCH_ERR="completing check run ${id} failed ($(describe_err))."
    return 1
  done
}

declare -A done_ids=()
failed=0
while read -r -u 3 id status; do
  [ -n "${id:-}" ] || continue
  if ! [[ "$id" =~ ^[0-9]+$ ]]; then
    warn "skipping a '${check_name}' run with an unreadable id '${id}'."
    continue
  fi
  [ -z "${done_ids[$id]:-}" ] || continue
  done_ids[$id]=1
  if [ "$status" = "completed" ]; then
    echo "check run ${id} is already completed; left as it is"
    continue
  fi
  PATCH_ERR=""
  if ! complete_run "$id"; then
    warn "${PATCH_ERR} This step is advisory: nothing failed, but that run may still read as in progress."
    failed=1
  fi
done 3<"$work/runs"

[ "$failed" -eq 0 ] || echo "one or more '${check_name}' runs were not completed (see the warnings above)"
exit 0
