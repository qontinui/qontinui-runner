#!/usr/bin/env bash
# Hermetic tests for .github/scripts/reviewdog-step-outputs.sh.
# Run by qontinui-coord's ci.yml (ci-scripts-tests) and, as a byte-identical
# copy, by qontinui-runner's clippy-tiers.yml (clippy-diff). No network: a stub
# `curl` first on PATH serves a Date header, or fails. Each case runs the
# helper the way a workflow step does (`bash -eo pipefail`) and reads back what
# it appended to GITHUB_OUTPUT. The last case checks the wiring: every
# `steps.<id>.outputs.*` a workflow in this repo reads from a step that sources
# the helper must be an output the helper writes.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="${HERE}/reviewdog-step-outputs.sh"
WORKFLOWS="${HERE}/../workflows"
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
fail=0
# Hermetic: an inherited retry setting must not change the cases below.
unset RSO_RETRY_DELAYS RSO_RETRY_SLEEP

if [ ! -f "$SCRIPT" ]; then
  echo "FAIL - ${SCRIPT} does not exist"
  exit 1
fi

mkdir -p "$W/stub"
cat >"$W/stub/curl" <<'STUB'
#!/usr/bin/env bash
echo "$*" >>"${STUB_STATE}.args"
mode="${STUB_CURL:-date}"
if [ "$mode" = failfirst ]; then
  # Fail the first call (the start mark), answer every later one.
  if [ -e "$STUB_STATE" ]; then mode=date; else : >"$STUB_STATE"; mode=fail; fi
fi
if [ "$mode" = slowend ]; then
  # Answer the first call (the start mark) at once; a later one (the EXIT
  # trap's end-time read) marks that it started, then answers a second later.
  if [ -e "$STUB_STATE" ]; then : >"${STUB_STATE}.slow"; sleep 1; else : >"$STUB_STATE"; fi
  mode=date
fi
case "$mode" in
  date) printf 'HTTP/2 401\r\ndate: Mon, 16 Nov 2026 05:00:00 GMT\r\ncontent-type: application/json\r\n\r\n' ;;
  noheader) printf 'HTTP/2 200\r\ncontent-type: application/json\r\n\r\n' ;;
  *) echo "curl: (6) Could not resolve host: api.github.com" >&2; exit 6 ;;
esac
STUB
chmod +x "$W/stub/curl"

# step <case> <curl mode> <body>: run <body> after rso_begin, as a step does.
step() {
  GHO="$W/gho-$1"
  : >"$GHO"
  # shellcheck disable=SC2016  # $1/$2 expand in the child bash, not here
  OUT="$(env PATH="$W/stub:$PATH" STUB_CURL="$2" STUB_STATE="$W/$1.curlstate" GITHUB_OUTPUT="$GHO" \
    bash -eo pipefail -c 'source "$1"; rso_begin "$2"; '"$3" _ "$SCRIPT" "$W/$1.stderr" 2>&1)"
  RC=$?
}
val() { sed -n "s/^$1=//p" "$GHO"; }
once() {
  local k
  for k in rc started_at ended_at clock stderr_log; do
    [ "$(grep -c "^${k}=" "$GHO")" -eq 1 ] || return 1
  done
  [ "$(wc -l <"$GHO" | tr -d ' ')" -eq 5 ]
}
ok() { echo "ok   - $1"; }
bad() {
  echo "FAIL - $1: $2"
  sed 's/^/       | /' "$GHO"
  printf '%s\n' "$OUT" | sed 's/^/       > /'
  fail=1
}
T=2026-11-16T05:00:00Z

# 1. an early exit before reviewdog runs: rc names it, no start time.
step early date 'rc=not-run-no-clippy-json; exit 0'
if [ "$RC" -eq 0 ] && once && [ "$(val rc)" = not-run-no-clippy-json ] && [ -z "$(val started_at)" ] \
  && [ "$(val ended_at)" = "$T" ] && [ "$(val clock)" = github ] && [ "$(val stderr_log)" = "$W/early.stderr" ]; then
  ok "an early exit writes rc=not-run-*, no started_at, and the end time from GitHub's Date header"
else bad "early exit" "rc=$RC"; fi

# 2. reviewdog ran and failed: its rc and both bounds, from GitHub's clock;
#    its stderr is captured to the log and replayed into the step's output.
step ran date 'rso_run sh -c "echo reviewdog: failed to post result >&2; exit 3"; exit 0'
if [ "$RC" -eq 0 ] && once && [ "$(val rc)" = 3 ] && [ "$(val started_at)" = "$T" ] \
  && [ "$(val ended_at)" = "$T" ] && [ "$(val clock)" = github ] \
  && grep -q 'failed to post result' "$W/ran.stderr" && printf '%s\n' "$OUT" | grep -q 'failed to post result'; then
  ok "rso_run records the rc, both bounds on GitHub's clock, and captures and replays stderr"
else bad "ran" "rc=$RC"; fi

# 2a. the start time's Date-header read may take 10 s; the EXIT trap's is
#     capped at 2 s, since it runs while the step is ending (the runner kills a
#     step 2.5 s after its SIGTERM).
if [ "$(grep -o -- '--max-time [0-9]*' "$W/ran.curlstate.args" | tr '\n' ' ')" = "--max-time 10 --max-time 2 " ]; then
  ok "the start-time read is bounded at 10 s and the EXIT trap's at 2 s"
else bad "curl bounds" "args=$(tr '\n' '|' <"$W/ran.curlstate.args" 2>/dev/null)"; fi

# 2b. a clean run records rc 0.
step clean date 'rso_run true'
if [ "$RC" -eq 0 ] && once && [ "$(val rc)" = 0 ]; then
  ok "rso_run records rc 0 for a command that succeeds"
else bad "clean" "rc=$RC"; fi

# 2c. a SIGTERM while reviewdog runs (a cancelled job): rc stays unknown, not
#     a pass, and the outputs are still written. The stub `reviewdog` marks
#     that it started, then sleeps; the step is signalled while it sleeps.
cat >"$W/stub/reviewdog" <<'STUB'
#!/usr/bin/env bash
: >"$RD_MARK"
sleep 2
STUB
chmod +x "$W/stub/reviewdog"
GHO="$W/gho-signal"
: >"$GHO"
# shellcheck disable=SC2016  # $1/$2 expand in the child bash, not here
env PATH="$W/stub:$PATH" STUB_CURL=date STUB_STATE="$W/signal.curlstate" GITHUB_OUTPUT="$GHO" \
  RD_MARK="$W/signal.mark" \
  bash -eo pipefail -c 'source "$1"; rso_begin "$2"; rso_run reviewdog -f=clippy' _ "$SCRIPT" "$W/signal.stderr" \
  >"$W/signal.out" 2>&1 &
pid=$!
for _ in $(seq 1 100); do [ -e "$W/signal.mark" ] && break; sleep 0.05; done
kill -TERM "$pid" 2>/dev/null
wait "$pid"
RC=$?
OUT="$(cat "$W/signal.out")"
if [ -e "$W/signal.mark" ] && [ "$RC" -ne 0 ] && once && [ "$(val rc)" = unknown ] && [ "$(val started_at)" = "$T" ]; then
  ok "a SIGTERM mid-reviewdog records rc=unknown, never rc=0"
else bad "signal" "rc=$RC mark=$([ -e "$W/signal.mark" ] && echo yes || echo no)"; fi

# 2d. a SIGINT to the step's process group while reviewdog runs (how a cancel
#     reaches it): rc stays unknown and the outputs are written. Launched from
#     python3 in its own session, because a background job of this
#     non-interactive shell would inherit SIGINT as ignored.
GHO="$W/gho-sigint"
: >"$GHO"
rm -f "$W/sigint.mark"
# shellcheck disable=SC2016  # $1/$2 expand in the child bash, not here
RC="$(env PATH="$W/stub:$PATH" STUB_CURL=date STUB_STATE="$W/sigint.curlstate" GITHUB_OUTPUT="$GHO" \
  RD_MARK="$W/sigint.mark" python3 - "$SCRIPT" "$W/sigint.stderr" "$W/sigint.mark" <<'PY'
import os
import signal
import subprocess
import sys
import time

script, stderr_log, mark = sys.argv[1:4]
child = subprocess.Popen(
    ["bash", "-eo", "pipefail", "-c", 'source "$1"; rso_begin "$2"; rso_run reviewdog -f=clippy', "_", script, stderr_log],
    start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
for _ in range(100):
    if os.path.exists(mark):
        break
    time.sleep(0.05)
os.killpg(child.pid, signal.SIGINT)
print(child.wait(timeout=20))
PY
)"
OUT=""
if [ -e "$W/sigint.mark" ] && [ "$RC" != 0 ] && once && [ "$(val rc)" = unknown ] && [ "$(val started_at)" = "$T" ]; then
  ok "a SIGINT to the step's process group mid-reviewdog records rc=unknown, never rc=0"
else bad "sigint" "rc=$RC mark=$([ -e "$W/sigint.mark" ] && echo yes || echo no)"; fi

# 2e. a SIGINT to the step's shell ALONE while reviewdog runs, which is how the
#     runner's cancel sends it: reviewdog is not signalled and finishes, and
#     rc stays unknown all the same (rso_begin traps SIGINT; untrapped, bash
#     would drop it once reviewdog exited 0, and record rc 0).
GHO="$W/gho-sigintpid"
: >"$GHO"
rm -f "$W/sigintpid.mark"
# shellcheck disable=SC2016  # $1/$2 expand in the child bash, not here
RC="$(env PATH="$W/stub:$PATH" STUB_CURL=date STUB_STATE="$W/sigintpid.curlstate" GITHUB_OUTPUT="$GHO" \
  RD_MARK="$W/sigintpid.mark" python3 - "$SCRIPT" "$W/sigintpid.stderr" "$W/sigintpid.mark" <<'PY'
import os
import signal
import subprocess
import sys
import time

script, stderr_log, mark = sys.argv[1:4]
child = subprocess.Popen(
    ["bash", "-eo", "pipefail", "-c", 'source "$1"; rso_begin "$2"; rso_run reviewdog -f=clippy', "_", script, stderr_log],
    start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
for _ in range(100):
    if os.path.exists(mark):
        break
    time.sleep(0.05)
os.kill(child.pid, signal.SIGINT)
print(child.wait(timeout=20))
PY
)"
OUT=""
if [ -e "$W/sigintpid.mark" ] && [ "$RC" != 0 ] && once && [ "$(val rc)" = unknown ] && [ "$(val started_at)" = "$T" ]; then
  ok "a SIGINT to the step's shell alone mid-reviewdog records rc=unknown, never rc=0"
else bad "sigint to the shell" "rc=$RC mark=$([ -e "$W/sigintpid.mark" ] && echo yes || echo no)"; fi

# 2f. a SIGINT or a SIGTERM that reaches the step's shell while the EXIT trap
#     itself runs (reading the end time) does not cut it short: every output
#     is still written once.
for sig in INT TERM; do
  GHO="$W/gho-finish-${sig}"
  : >"$GHO"
  rm -f "$W/finish-${sig}.curlstate" "$W/finish-${sig}.curlstate.slow"
  # shellcheck disable=SC2016  # $1/$2 expand in the child bash, not here
  RC="$(env PATH="$W/stub:$PATH" STUB_CURL=slowend STUB_STATE="$W/finish-${sig}.curlstate" GITHUB_OUTPUT="$GHO" \
    python3 - "$SCRIPT" "$W/finish-${sig}.stderr" "$W/finish-${sig}.curlstate.slow" "$sig" <<'PY'
import os
import signal
import subprocess
import sys
import time

script, stderr_log, mark, sig = sys.argv[1:5]
child = subprocess.Popen(
    ["bash", "-eo", "pipefail", "-c", 'source "$1"; rso_begin "$2"; rso_run true', "_", script, stderr_log],
    start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
for _ in range(100):
    if os.path.exists(mark):
        break
    time.sleep(0.05)
os.kill(child.pid, signal.SIGINT if sig == "INT" else signal.SIGTERM)
print(child.wait(timeout=20))
PY
)"
  OUT=""
  if [ -e "$W/finish-${sig}.curlstate.slow" ] && once && [ "$(val rc)" = 0 ] && [ "$(val ended_at)" = "$T" ]; then
    ok "a SIG${sig} while the EXIT trap writes the outputs does not cut it short"
  else bad "SIG${sig} during the EXIT trap" "rc=$RC slow=$([ -e "$W/finish-${sig}.curlstate.slow" ] && echo yes || echo no)"; fi
done

# 3. a death under set -e after the start mark: rc stays unknown.
step died date 'rso_mark_start; false; rc=0'
if [ "$RC" -ne 0 ] && once && [ "$(val rc)" = unknown ] && [ "$(val started_at)" = "$T" ]; then
  ok "a death under set -e still writes every output once, with rc=unknown"
else bad "died" "rc=$RC"; fi

# 4. no Date header (curl fails, or answers without one): the runner's clock,
#    and clock=runner says so.
for mode in fail noheader; do
  step "runner-$mode" "$mode" 'rso_mark_start; rc=0; exit 0'
  if [ "$RC" -eq 0 ] && once && [ "$(val clock)" = runner ] \
    && [[ "$(val started_at)" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] \
    && [[ "$(val ended_at)" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T ]]; then
    ok "curl ${mode}: both bounds fall back to the runner's clock, and clock=runner"
  else bad "runner clock (${mode})" "rc=$RC"; fi
done

# 4b. only the START came from the runner's clock: clock=runner all the same,
#     since one bound on an unknown offset is enough to widen the skew.
step mixed failfirst 'rso_mark_start; rc=0; exit 0'
if [ "$RC" -eq 0 ] && once && [ "$(val clock)" = runner ] && [ "$(val ended_at)" = "$T" ]; then
  ok "a start bound from the runner's clock alone still records clock=runner"
else bad "mixed clock" "rc=$RC"; fi

# 5. a stale stderr log from an earlier job is removed by rso_begin.
echo "check-runs/999 stale" >"$W/stale.stderr"
step stale date 'rc=0; exit 0'
if [ "$RC" -eq 0 ] && [ ! -e "$W/stale.stderr" ]; then
  ok "rso_begin removes a stale stderr log"
else bad "stale log" "rc=$RC"; fi

# 5a-5s. rso_run_retrying: a stub `reviewdog` whose Nth attempt does what line
#     N of its plan says. `post <id>` is the race of coord findings c58cd00f
#     and 2f3d3b96: GitHub answers the PATCH of a check run reviewdog created a
#     second earlier with 404, and reviewdog exits 1, the same status a diff
#     finding exits with. `difffetch` and `diffparse` print, verbatim, the
#     stderr line the pinned reviewdog v0.21.2 release binary (Windows x86_64,
#     sha256-checked against the release's checksums.txt) printed on
#     2026-11-22 under `-f=clippy -reporter=local` with one short-format clippy
#     diagnostic on stdin: `-diff="false"` for the fetch, and a `-diff` printing
#     the hunk header `@@ -x,y +z @@` for the parse. Each exited 1. Under
#     `-reporter=github-pr-check` the text after the marker is the GitHub or
#     git error instead; the marker is the same `reviewdog: <msg>: ` prefix
#     (reviewdog.go Run, printed by cmd/reviewdog/main.go). Each attempt
#     records how many bytes of the input it read; the retry sleep is injected
#     and logged.
cat >"$W/stub/rd-scripted" <<'STUB'
#!/usr/bin/env bash
n=$(( $(cat "$RD_STATE.n" 2>/dev/null || echo 0) + 1 ))
echo "$n" >"$RD_STATE.n"
wc -c | tr -d ' ' >"$RD_STATE.stdin.$n"
what="$(sed -n "${n}p" "$RD_PLAN")"
# What v0.21.2 prints on every run with -fail-on-error (job 135103311891).
echo 'time=2026-11-18T21:33:24.874Z level=WARN msg="reviewdog: -fail-on-error is deprecated. Use -fail-level=any, or -fail-level=error for github-[pr-]check reporter instead. See also https://github.com/reviewdog/reviewdog/blob/master/CHANGELOG.md"' >&2
case "$what" in
  ok) exit 0 ;;
  findings) echo "reviewdog: found at least one issue with severity greater than or equal to the given level: error" >&2; exit 1 ;;
  post\ *) echo "reviewdog: failed to post result: PATCH https://api.github.com/repos/o/r/check-runs/${what#post }: 404 Not Found []" >&2; exit 1 ;;
  create) echo "reviewdog: failed to create check: POST https://api.github.com/repos/o/r/check-runs: 502 Bad Gateway []" >&2; exit 1 ;;
  difffetch) echo "reviewdog: fail to get diff: exit status 1" >&2; exit 1 ;;
  diffparse) echo "reviewdog: fail to parse diff: invalid hunk range: @@ -x,y +z @@" >&2; exit 1 ;;
  # Unreachable under -f=clippy in v0.21.2 (no errorformat parser returns it),
  # so it has no class on purpose: a finding.
  parseerr) echo "reviewdog: parse error: x" >&2; exit 1 ;;
  # A findings exit whose wording is not the one above: still a finding.
  reworded) echo "reviewdog: 3 problems found" >&2; exit 1 ;;
  # A slog line QUOTING a diff-fetch marker, then a findings line: a finding.
  slog) echo 'time=2026-11-22T00:00:00.000Z level=ERROR msg="reviewdog: fail to get diff: x"' >&2
        echo "reviewdog: found at least one issue with severity greater than or equal to the given level: error" >&2; exit 1 ;;
  # Every marker, on an attempt that exits 0: a success is never classified.
  okmarked) echo "reviewdog: fail to get diff: x" >&2; echo "reviewdog: fail to parse diff: x" >&2
            echo "reviewdog: failed to post result: check-runs/1: 404" >&2; exit 0 ;;
  hang) : >"$RD_MARK"; sleep 2; exit 0 ;;
  *) echo "rd-scripted: no plan line ${n}" >&2; exit 99 ;;
esac
STUB
cat >"$W/stub/rso-sleep" <<'STUB'
#!/usr/bin/env bash
echo "$1" >>"$RSO_SLEEP_LOG"
STUB
chmod +x "$W/stub/rd-scripted" "$W/stub/rso-sleep"
seq 1 400 >"$W/report.json"
INPUT_BYTES="$(wc -c <"$W/report.json" | tr -d ' ')"

# rstep <case> <plan line>...: run rso_run_retrying the way a step does; sets
# OUT, RC, GHO, and ATTEMPTS / NO_VERDICT from the shell variables it sets.
rstep() {
  local name="$1"
  shift
  GHO="$W/gho-$name"
  : >"$GHO"
  printf '%s\n' "$@" >"$W/$name.plan"
  : >"$W/$name.sleeps"
  # shellcheck disable=SC2016  # $1..$3 expand in the child bash, not here
  OUT="$(env PATH="$W/stub:$PATH" STUB_CURL=date STUB_STATE="$W/$name.curlstate" GITHUB_OUTPUT="$GHO" \
    RD_PLAN="$W/$name.plan" RD_STATE="$W/$name.rd" RD_MARK="$W/$name.mark" \
    RSO_RETRY_SLEEP="$W/stub/rso-sleep" RSO_SLEEP_LOG="$W/$name.sleeps" \
    bash -eo pipefail -c 'source "$1"; rso_begin "$2"; rso_run_retrying "$3" rd-scripted -f=clippy; echo "attempts=$RSO_ATTEMPTS no_verdict=$RSO_NO_VERDICT"' \
    _ "$SCRIPT" "$W/$name.stderr" "${RSTEP_INPUT:-$W/report.json}" 2>&1)"
  RC=$?
  ATTEMPTS="$(printf '%s\n' "$OUT" | sed -n 's/^attempts=\([0-9]*\) .*/\1/p')"
  NO_VERDICT="$(printf '%s\n' "$OUT" | sed -n 's/^attempts=[0-9]* no_verdict=\([a-z-]*\)$/\1/p')"
}
rsleeps() { tr '\n' ' ' <"$W/$1.sleeps" | sed 's/ $//'; }
# every attempt read the whole input
read_whole() {
  local k
  for k in $(seq 1 "$2"); do
    [ "$(cat "$W/$1.rd.stdin.$k" 2>/dev/null)" = "$INPUT_BYTES" ] || return 1
  done
}

# 5a. a failed post, then success: rc 0 from a retry of the same input; the
#     window's start is marked once, before the first attempt; both attempts'
#     stderr is in the log (the completion step reads the first's id there)
#     and in the job log; nothing reports a no-verdict class at the end.
rstep retry-ok "post 111" ok
if [ "$RC" -eq 0 ] && once && [ "$(val rc)" = 0 ] && [ "$ATTEMPTS" = 2 ] && [ -z "$NO_VERDICT" ] \
  && [ "$(val started_at)" = "$T" ] && [ "$(grep -c -- '--max-time 10' "$W/retry-ok.curlstate.args")" -eq 1 ] \
  && [ "$(rsleeps retry-ok)" = 5 ] && read_whole retry-ok 2 \
  && grep -q 'check-runs/111: 404' "$W/retry-ok.stderr" && [ "$(grep -c 'level=WARN' "$W/retry-ok.stderr")" -eq 2 ] \
  && printf '%s\n' "$OUT" | grep -q 'check-runs/111: 404' \
  && printf '%s\n' "$OUT" | grep -qF 'reviewdog attempt 1/3 exited 1 with no verdict, class post (it did not finish its check run' \
  && printf '%s\n' "$OUT" | grep -qxF '::notice title=reviewdog-step-outputs::retry outcome: reviewdog reached a verdict on attempt 2/3 (exit 0), after 1 attempt(s) with no verdict (post)'; then
  ok "a failed post (404) then success: rc 0 after one retry, one start mark, every attempt's stderr kept, a retry-outcome notice"
else bad "retry then ok" "rc=$RC attempts=$ATTEMPTS sleeps='$(rsleeps retry-ok)'"; fi

# 5b. a findings exit (rc 1, no failed create or post in stderr) is the lint
#     verdict: returned at once, never retried.
rstep findings findings
if [ "$RC" -eq 0 ] && once && [ "$(val rc)" = 1 ] && [ "$ATTEMPTS" = 1 ] && [ -z "$NO_VERDICT" ] \
  && [ ! -s "$W/findings.sleeps" ] && ! printf '%s\n' "$OUT" | grep -qE 'retrying|retry outcome' \
  && grep -qF 'found at least one issue with severity' "$W/findings.stderr"; then
  ok "a findings exit (rc 1, clean stderr) is not retried and keeps rc 1"
else bad "findings" "rc=$RC attempts=$ATTEMPTS"; fi

# 5c. a failed post on every attempt: the retries are spent (default 5 10 s),
#     rc is the last attempt's, RSO_NO_VERDICT=post says so, and all three ids
#     are in the log.
rstep persist "post 121" "post 122" "post 123"
if [ "$RC" -eq 0 ] && once && [ "$(val rc)" = 1 ] && [ "$ATTEMPTS" = 3 ] && [ "$NO_VERDICT" = post ] \
  && [ "$(rsleeps persist)" = "5 10" ] && read_whole persist 3 \
  && [ "$(grep -oE 'check-runs/[0-9]+' "$W/persist.stderr" | tr '\n' ' ')" = "check-runs/121 check-runs/122 check-runs/123 " ] \
  && printf '%s\n' "$OUT" | grep -qxF '::notice title=reviewdog-step-outputs::retry outcome: reviewdog reached no verdict on any of 3 attempt(s) (post,post,post; last exit 1)'; then
  ok "a failed post on every attempt: 3 attempts at 5/10 s, rc 1, RSO_NO_VERDICT=post, every id in the log"
else bad "persistent post failure" "rc=$RC attempts=$ATTEMPTS no_verdict=$NO_VERDICT sleeps='$(rsleeps persist)'"; fi

# 5d. a failed create (its line names no run), then success: retried.
rstep create-ok create ok
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 0 ] && [ "$ATTEMPTS" = 2 ] && [ -z "$NO_VERDICT" ] \
  && grep -q 'failed to create check' "$W/create-ok.stderr"; then
  ok "a failed create then success: retried, rc 0, the create failure kept in the log"
else bad "create then ok" "rc=$RC attempts=$ATTEMPTS"; fi

# 5e. a failed post, then a findings exit: the findings are the verdict (rc 1,
#     not a failed post), and the first attempt's id stays in the log.
rstep post-findings "post 131" findings
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 1 ] && [ "$ATTEMPTS" = 2 ] && [ -z "$NO_VERDICT" ] \
  && grep -q 'check-runs/131' "$W/post-findings.stderr"; then
  ok "a failed post then findings: rc 1 from the findings, RSO_NO_VERDICT empty, the first id kept"
else bad "post then findings" "rc=$RC attempts=$ATTEMPTS no_verdict=$NO_VERDICT"; fi

# 5f. RSO_RETRY_DELAYS empty: no retry at all.
RSO_RETRY_DELAYS='' rstep noretry "post 141" ok
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 1 ] && [ "$ATTEMPTS" = 1 ] && [ "$NO_VERDICT" = post ] && [ ! -s "$W/noretry.sleeps" ]; then
  ok "RSO_RETRY_DELAYS empty: one attempt, no sleep"
else bad "no retry" "rc=$RC attempts=$ATTEMPTS"; fi

# 5g. a SIGTERM during the RETRY (a cancelled job): rc stays unknown, never
#     the first attempt's status and never a pass, and the first attempt's id
#     is still in the log for the completion step.
GHO="$W/gho-retry-signal"
: >"$GHO"
printf '%s\n' "post 151" hang >"$W/retry-signal.plan"
rm -f "$W/retry-signal.mark"
# shellcheck disable=SC2016  # $1..$3 expand in the child bash, not here
env PATH="$W/stub:$PATH" STUB_CURL=date STUB_STATE="$W/retry-signal.curlstate" GITHUB_OUTPUT="$GHO" \
  RD_PLAN="$W/retry-signal.plan" RD_STATE="$W/retry-signal.rd" RD_MARK="$W/retry-signal.mark" \
  RSO_RETRY_SLEEP="$W/stub/rso-sleep" RSO_SLEEP_LOG="$W/retry-signal.sleeps" \
  bash -eo pipefail -c 'source "$1"; rso_begin "$2"; rso_run_retrying "$3" rd-scripted -f=clippy' \
  _ "$SCRIPT" "$W/retry-signal.stderr" "$W/report.json" >"$W/retry-signal.out" 2>&1 &
pid=$!
for _ in $(seq 1 100); do [ -e "$W/retry-signal.mark" ] && break; sleep 0.05; done
kill -TERM "$pid" 2>/dev/null
wait "$pid"
RC=$?
OUT="$(cat "$W/retry-signal.out")"
if [ -e "$W/retry-signal.mark" ] && [ "$RC" -ne 0 ] && once && [ "$(val rc)" = unknown ] \
  && [ "$(val started_at)" = "$T" ] && grep -q 'check-runs/151' "$W/retry-signal.stderr"; then
  ok "a SIGTERM during the retry records rc=unknown and keeps the first attempt's id in the log"
else bad "signal during retry" "rc=$RC mark=$([ -e "$W/retry-signal.mark" ] && echo yes || echo no)"; fi

# 5g2. a SIGTERM to the step's shell, a SIGINT to its process group, and a
#     SIGINT to the shell ALONE (how the runner's cancel sends it), during the
#     retry SLEEP: rc stays unknown, the first attempt's id is in the log, the
#     outputs are written once, and no second attempt runs. The sleep stub
#     marks that it started, then really sleeps. An untrapped SIGINT to the
#     shell alone is deferred behind the sleep and then dropped, so without
#     rso_begin's INT trap the second attempt would run and record its rc.
cat >"$W/stub/rso-sleep-real" <<'STUB'
#!/usr/bin/env bash
echo "$1" >>"$RSO_SLEEP_LOG"
: >"$RSO_SLEEP_MARK"
sleep 2
STUB
chmod +x "$W/stub/rso-sleep-real"
for sig in TERM INT INTPID; do
  name="sleep-sig${sig}"
  case "$sig" in TERM) id=161 ;; INT) id=162 ;; *) id=163 ;; esac
  GHO="$W/gho-${name}"
  : >"$GHO"
  : >"$W/${name}.sleeps"
  printf '%s\n' "post ${id}" ok >"$W/${name}.plan"
  # shellcheck disable=SC2016  # $1..$3 expand in the child bash, not here
  RC="$(env PATH="$W/stub:$PATH" STUB_CURL=date STUB_STATE="$W/${name}.curlstate" GITHUB_OUTPUT="$GHO" \
    RD_PLAN="$W/${name}.plan" RD_STATE="$W/${name}.rd" RD_MARK="$W/${name}.mark" \
    RSO_RETRY_SLEEP="$W/stub/rso-sleep-real" RSO_SLEEP_LOG="$W/${name}.sleeps" RSO_SLEEP_MARK="$W/${name}.slept" \
    python3 - "$SCRIPT" "$W/${name}.stderr" "$W/report.json" "$W/${name}.slept" "$sig" <<'PY'
import os
import signal
import subprocess
import sys
import time

script, stderr_log, report, mark, sig = sys.argv[1:6]
child = subprocess.Popen(
    ["bash", "-eo", "pipefail", "-c",
     'source "$1"; rso_begin "$2"; rso_run_retrying "$3" rd-scripted -f=clippy', "_", script, stderr_log, report],
    start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
for _ in range(100):
    if os.path.exists(mark):
        break
    time.sleep(0.05)
if sig == "TERM":
    os.kill(child.pid, signal.SIGTERM)
elif sig == "INT":
    os.killpg(child.pid, signal.SIGINT)
else:
    os.kill(child.pid, signal.SIGINT)
print(child.wait(timeout=20))
PY
)"
  OUT=""
  if [ -e "$W/${name}.slept" ] && [ "$RC" != 0 ] && once && [ "$(val rc)" = unknown ] \
    && [ "$(val started_at)" = "$T" ] && grep -q "check-runs/${id}" "$W/${name}.stderr" \
    && [ "$(cat "$W/${name}.rd.n" 2>/dev/null)" = 1 ]; then
    ok "a ${sig} (TERM: to the shell, INT: to the group, INTPID: SIGINT to the shell alone) during the retry sleep records rc=unknown, keeps the first id, and runs no second attempt"
  else bad "${sig} during the retry sleep" "rc=$RC slept=$([ -e "$W/${name}.slept" ] && echo yes || echo no) attempts=$(cat "$W/${name}.rd.n" 2>/dev/null)"; fi
done

# 5h. an unreadable input runs nothing: rc=not-run-input-unreadable, no start
#     time, and the step fails.
RSTEP_INPUT="$W/no-such-report.json" rstep noinput ok
if [ "$RC" -ne 0 ] && once && [ "$(val rc)" = not-run-input-unreadable ] && [ -z "$(val started_at)" ] \
  && [ ! -e "$W/noinput.rd.n" ]; then
  ok "an unreadable input: reviewdog never runs, rc=not-run-input-unreadable, no started_at"
else bad "unreadable input" "rc=$RC"; fi

# 5i. the retry and the completion step decide "reviewdog did not finish its
#     run" with the same pattern: the helper's RSO_UNFINISHED_RUN_RE is the
#     regex complete-orphaned-check-runs.sh greps the stderr log with.
# shellcheck disable=SC2016  # $1 expands in the child bash, not here
RE="$(bash -c 'source "$1"; printf "%s" "$RSO_UNFINISHED_RUN_RE"' _ "$SCRIPT")"
if [ -n "$RE" ] && grep -qF -- "grep -qE '${RE}' \"\$REVIEWDOG_STDERR_LOG\"" "${HERE}/complete-orphaned-check-runs.sh"; then
  ok "RSO_UNFINISHED_RUN_RE is the completion script's own stderr pattern"
else
  OUT="RSO_UNFINISHED_RUN_RE='${RE}'" GHO=/dev/null
  bad "pattern parity" "the completion script does not grep the stderr log with '${RE}'"
fi

# 5j-5s. the diff classes (plan 2026-11-22-reviewdog-diff-and-parse-infra-
#     exits-read-as-lint-findings). A class is named only by a POSITIVE,
#     line-anchored marker on an attempt that exited non-zero; diff-fetch is
#     retried like a post, diff-parse never is, and anything else is a verdict.
#     5p-5s pass on an unfixed helper by construction: they guard against a
#     classifier that widens "no verdict" (an unanchored marker, an
#     absence-of-findings rule, a classified success).

# 5j. a failed diff fetch, then success: retried once, the verdict stands.
rstep fetch-ok difffetch ok
if [ "$RC" -eq 0 ] && once && [ "$(val rc)" = 0 ] && [ "$ATTEMPTS" = 2 ] && [ -z "$NO_VERDICT" ] \
  && [ "$(rsleeps fetch-ok)" = 5 ] && read_whole fetch-ok 2 \
  && grep -qxF 'reviewdog: fail to get diff: exit status 1' "$W/fetch-ok.stderr" \
  && printf '%s\n' "$OUT" | grep -qF 'reviewdog attempt 1/3 exited 1 with no verdict, class diff-fetch (it could not fetch the PR diff' \
  && printf '%s\n' "$OUT" | grep -qxF '::notice title=reviewdog-step-outputs::retry outcome: reviewdog reached a verdict on attempt 2/3 (exit 0), after 1 attempt(s) with no verdict (diff-fetch)'; then
  ok "a failed diff fetch then success: one retry at 5 s, rc 0, RSO_NO_VERDICT empty, the notice names diff-fetch"
else bad "diff-fetch then ok" "rc=$RC attempts=$ATTEMPTS no_verdict=$NO_VERDICT sleeps='$(rsleeps fetch-ok)'"; fi

# 5k. a failed diff fetch on every attempt: the retries are spent.
rstep fetch-persist difffetch difffetch difffetch
if [ "$RC" -eq 0 ] && once && [ "$(val rc)" = 1 ] && [ "$ATTEMPTS" = 3 ] && [ "$NO_VERDICT" = diff-fetch ] \
  && [ "$(rsleeps fetch-persist)" = "5 10" ] && read_whole fetch-persist 3 \
  && printf '%s\n' "$OUT" | grep -qxF '::notice title=reviewdog-step-outputs::retry outcome: reviewdog reached no verdict on any of 3 attempt(s) (diff-fetch,diff-fetch,diff-fetch; last exit 1)'; then
  ok "a failed diff fetch on every attempt: 3 attempts at 5/10 s, rc 1, RSO_NO_VERDICT=diff-fetch"
else bad "persistent diff-fetch" "rc=$RC attempts=$ATTEMPTS no_verdict=$NO_VERDICT sleeps='$(rsleeps fetch-persist)'"; fi

# 5l. a failed diff parse: no verdict, and never retried (the same bytes
#     parse the same way again).
rstep parse diffparse ok
if [ "$RC" -eq 0 ] && once && [ "$(val rc)" = 1 ] && [ "$ATTEMPTS" = 1 ] && [ "$NO_VERDICT" = diff-parse ] \
  && [ ! -s "$W/parse.sleeps" ] && ! printf '%s\n' "$OUT" | grep -qE 'retrying|retry outcome' \
  && grep -qxF 'reviewdog: fail to parse diff: invalid hunk range: @@ -x,y +z @@' "$W/parse.stderr"; then
  ok "a failed diff parse: one attempt, no sleep, rc 1, RSO_NO_VERDICT=diff-parse"
else bad "diff-parse" "rc=$RC attempts=$ATTEMPTS no_verdict=$NO_VERDICT sleeps='$(rsleeps parse)'"; fi

# 5m. a failed post, then a failed diff parse: the last attempt decides, and
#     the first attempt's id stays in the log for the completion step.
rstep post-parse "post 171" diffparse ok
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 1 ] && [ "$ATTEMPTS" = 2 ] && [ "$NO_VERDICT" = diff-parse ] \
  && [ "$(rsleeps post-parse)" = 5 ] && grep -q 'check-runs/171' "$W/post-parse.stderr" \
  && printf '%s\n' "$OUT" | grep -qxF '::notice title=reviewdog-step-outputs::retry outcome: reviewdog ended with no verdict (diff-parse, not retried) on attempt 2/3 (exit 1), after 1 attempt(s) with no verdict (post)'; then
  ok "a failed post then a failed diff parse: 2 attempts, rc 1, RSO_NO_VERDICT=diff-parse, the first id kept"
else bad "post then diff-parse" "rc=$RC attempts=$ATTEMPTS no_verdict=$NO_VERDICT"; fi

# 5n. `reviewdog: parse error:` has no class (unreachable under -f=clippy): a
#     finding, returned at once.
rstep parseerr parseerr ok
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 1 ] && [ "$ATTEMPTS" = 1 ] && [ -z "$NO_VERDICT" ] && [ ! -s "$W/parseerr.sleeps" ]; then
  ok "reviewdog: parse error: is unclassified by design: one attempt, rc 1, RSO_NO_VERDICT empty"
else bad "parse error" "rc=$RC attempts=$ATTEMPTS no_verdict=$NO_VERDICT"; fi

# 5o. a failed diff fetch, then a findings exit: the findings are the verdict.
rstep fetch-findings difffetch findings
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 1 ] && [ "$ATTEMPTS" = 2 ] && [ -z "$NO_VERDICT" ]; then
  ok "a failed diff fetch then findings: 2 attempts, rc 1 from the findings, RSO_NO_VERDICT empty"
else bad "diff-fetch then findings" "rc=$RC attempts=$ATTEMPTS no_verdict=$NO_VERDICT"; fi

# 5p. a findings exit in other words is still a finding: the classifier is
#     positive, never "no findings line, so infra".
rstep reworded reworded ok
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 1 ] && [ "$ATTEMPTS" = 1 ] && [ -z "$NO_VERDICT" ] \
  && ! printf '%s\n' "$OUT" | grep -qE 'retrying|retry outcome'; then
  ok "a reworded findings exit: one attempt, rc 1, RSO_NO_VERDICT empty, no annotation"
else bad "reworded findings" "rc=$RC attempts=$ATTEMPTS no_verdict=$NO_VERDICT"; fi

# 5q. a slog line quoting a diff marker mid-line does not classify: the
#     markers are anchored at a line's start.
rstep slog slog ok
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 1 ] && [ "$ATTEMPTS" = 1 ] && [ -z "$NO_VERDICT" ] \
  && ! printf '%s\n' "$OUT" | grep -qE 'retrying|retry outcome'; then
  ok "a slog line quoting 'reviewdog: fail to get diff:' plus findings: one attempt, rc 1, RSO_NO_VERDICT empty"
else bad "slog line" "rc=$RC attempts=$ATTEMPTS no_verdict=$NO_VERDICT"; fi

# 5r. every marker on an attempt that exits 0: a success is never classified.
rstep okmarked okmarked ok
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 0 ] && [ "$ATTEMPTS" = 1 ] && [ -z "$NO_VERDICT" ] \
  && ! printf '%s\n' "$OUT" | grep -qE 'retrying|retry outcome'; then
  ok "markers on an exit-0 attempt: one attempt, rc 0, not classified"
else bad "marked success" "rc=$RC attempts=$ATTEMPTS no_verdict=$NO_VERDICT"; fi

# 5s. the diff markers are separate from the completion step's pattern: a
#     diff failure leaves no check run, so the completer must not read one.
# shellcheck disable=SC2016  # $1 expands in the child bash, not here
CLS="$(bash -c 'source "$1"; printf "%s|%s|%s" "$RSO_DIFF_FETCH_RE" "$RSO_DIFF_PARSE_RE" "$RSO_RETRIED_CLASSES"' _ "$SCRIPT")"
if [ "$CLS" = "^reviewdog: fail to get diff: |^reviewdog: fail to parse diff: |post diff-fetch" ] \
  && ! grep -qF 'fail to get diff' "${HERE}/complete-orphaned-check-runs.sh" \
  && ! grep -qF 'fail to parse diff' "${HERE}/complete-orphaned-check-runs.sh"; then
  ok "the diff markers are anchored, only post and diff-fetch are retried, and the completer greps neither"
else
  OUT="classes='${CLS}'" GHO=/dev/null
  bad "diff class table" "unexpected class table or a diff marker in the completer"
fi

# 6. wiring, both ways, in every workflow of this repo with a step that
#    sources the helper: (a) every steps.<id>.outputs.* read from that step is
#    an output the helper writes; (b) the completion step (the one running
#    complete-orphaned-check-runs.sh) reads EVERY output the helper writes,
#    and that step's `outcome` (STEP_OUTCOME), all from that step's id and no
#    other; its `if:` contains always(); it sets GH_TOKEN; and the check name
#    it passes is the sourcing step's reviewdog `-name=`. A mistyped or
#    renamed step id, a forgotten output, outcome or token, a dropped
#    always(), or a check name that drifted from reviewdog's, fails here.
if WIRING="$(python3 - "$WORKFLOWS" "$(bash -c 'source "$1"; echo "$RSO_OUTPUT_NAMES"' _ "$SCRIPT")" <<'PY'
import pathlib
import re
import sys

root, names = pathlib.Path(sys.argv[1]), set(sys.argv[2].split())


def step_bounds(lines, i):
    """The [start, end) line range of the step that contains line i."""
    start = next(j for j in range(i, -1, -1) if re.match(r"\s*- name:", lines[j]))
    indent = len(lines[start]) - len(lines[start].lstrip())
    end = len(lines)
    for j in range(start + 1, len(lines)):
        line = lines[j]
        if line.strip() and len(line) - len(line.lstrip()) <= indent:
            end = j
            break
    return start, end


found, errors = 0, []
for wf in sorted(root.glob("*.y*ml")):
    lines = wf.read_text(encoding="utf-8").splitlines()
    text = "\n".join(lines)
    sourcing = [i for i, l in enumerate(lines)
                if "reviewdog-step-outputs.sh" in l and re.search(r"\bsource\b", l)]
    for i in sourcing:
        start, end = step_bounds(lines, i)
        ids = [m.group(1) for l in lines[start:end] for m in [re.match(r"\s*id:\s*(\S+)", l)] if m]
        if not ids:
            errors.append(f"{wf.name}:{i + 1}: the step sourcing the helper has no id")
            continue
        step_id = ids[0]
        found += 1
        rd_names = re.findall(r'-name="([^"]+)"', "\n".join(lines[start:end]))
        for used in sorted(set(re.findall(rf"steps\.{re.escape(step_id)}\.outputs\.(\w+)", text))):
            if used not in names:
                errors.append(f"{wf.name}: steps.{step_id}.outputs.{used} is not written by the helper")
        completers = [j for j, l in enumerate(lines)
                      if re.search(r"bash \.github/scripts/complete-orphaned-check-runs\.sh", l)]
        if not completers:
            errors.append(f"{wf.name}: no step runs complete-orphaned-check-runs.sh")
            continue
        for j in completers:
            c_start, c_end = step_bounds(lines, j)
            block = "\n".join(lines[c_start:c_end])
            reads = re.findall(r"steps\.([\w-]+)\.outputs\.(\w+)", block)
            for rid, name in reads:
                if rid != step_id:
                    errors.append(f"{wf.name}:{c_start + 1}: the completion step reads steps.{rid}.outputs.{name}, not steps.{step_id}")
            missing = sorted(names - {name for rid, name in reads if rid == step_id})
            if missing:
                errors.append(f"{wf.name}:{c_start + 1}: the completion step does not read steps.{step_id}.outputs.{{{','.join(missing)}}}")
            # STEP_OUTCOME must be the sourcing step's outcome. (Other
            # outcomes the step may read, such as a checkout gate in its
            # `if:`, are not this.)
            outcomes = re.findall(r"STEP_OUTCOME:\s*\$\{\{\s*steps\.([\w-]+)\.outcome\s*\}\}", block)
            if not outcomes:
                errors.append(f"{wf.name}:{c_start + 1}: the completion step does not set STEP_OUTCOME from steps.{step_id}.outcome")
            for rid in outcomes:
                if rid != step_id:
                    errors.append(f"{wf.name}:{c_start + 1}: the completion step sets STEP_OUTCOME from steps.{rid}.outcome, not steps.{step_id}.outcome")
            ifs = [l for l in lines[c_start:c_end] if re.match(r"\s*if:", l)]
            if not ifs or "always()" not in ifs[0]:
                errors.append(f"{wf.name}:{c_start + 1}: the completion step's `if:` does not contain always(), so it would not run after a failed step")
            if not re.search(r"^\s*GH_TOKEN:\s*\S", block, re.M):
                errors.append(f"{wf.name}:{c_start + 1}: the completion step does not set GH_TOKEN")
            m = re.search(r'complete-orphaned-check-runs\.sh\s*\\?\s*"[^"]*"\s+"[^"]*"\s+"([^"]+)"', block)
            passed = m.group(1) if m else None
            if len(rd_names) != 1:
                errors.append(f"{wf.name}:{start + 1}: expected one reviewdog -name= in the sourcing step, found {rd_names}")
            elif passed != rd_names[0]:
                errors.append(f"{wf.name}:{c_start + 1}: the completion step passes check name {passed!r}, but reviewdog posts {rd_names[0]!r}")
if found == 0:
    errors.append("no workflow step sources reviewdog-step-outputs.sh")
print("\n".join(errors) if errors else f"{found} sourcing step(s) and their completion steps checked")
sys.exit(1 if errors else 0)
PY
)"; then
  ok "wiring: ${WIRING}; every output is written, read, and read from the right step"
else
  OUT="$WIRING" GHO=/dev/null
  bad "wiring" "$WIRING"
fi

if [ "$fail" -ne 0 ]; then
  echo "reviewdog-step-outputs tests: FAILED"
  exit 1
fi
echo "reviewdog-step-outputs tests: all passed"
