#!/usr/bin/env bash
# Hermetic tests for .github/scripts/complete-orphaned-check-runs.sh.
# Run by qontinui-coord's ci.yml (ci-scripts-tests) and, as a byte-identical
# copy, by qontinui-runner's clippy-tiers.yml (clippy-diff). No network: a stub
# `gh` first on PATH serves fixture responses written by each case, and logs
# every call. The backoff sleep is injected (COMPLETE_ORPHANS_SLEEP), so the
# retry cases cost no wall time and the sleeps they asked for are asserted.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="${HERE}/complete-orphaned-check-runs.sh"
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
fail=0
REPO=o/r
SHA=abc1234
NAME='clippy-diff (advisory)'
ENC='clippy-diff%20%28advisory%29'
WARN='::warning title=complete-orphaned-check-runs::'
STARTED=2026-11-15T01:06:20Z
RD_START=2026-11-15T01:06:19Z
ENDED=2026-11-15T01:06:21Z

if [ ! -f "$SCRIPT" ]; then
  echo "FAIL - ${SCRIPT} does not exist"
  exit 1
fi

# --- stub gh -----------------------------------------------------------------
# Each request is keyed by "<METHOD> <path>". The response for the k-th call to
# a key is the file responses/<key>.<k>, else responses/<key>.default, else a
# 599. A response file's first line is the HTTP status, the rest is the body.
# A non-2xx status prints gh's own error shape, "gh: ... (HTTP <code>)", on
# stderr and exits 1, as the real gh does; the status `neterr` prints a
# transport error with no HTTP status at all.
mkdir -p "$W/stub"
cat >"$W/stub/gh" <<'STUB'
#!/usr/bin/env bash
[ "$1" = api ] || { echo "stub gh: unexpected $*" >&2; exit 64; }
shift
method=GET path="" input=""
while [ $# -gt 0 ]; do
  case "$1" in
    -X) method="$2"; shift ;;
    -H) shift ;;
    --input) input="$2"; shift ;;
    -*) echo "stub gh: unexpected flag $1" >&2; exit 64 ;;
    *) path="$1" ;;
  esac
  shift
done
key="$(printf '%s %s' "$method" "$path" | tr -c 'A-Za-z0-9\n' '_')"
n=$(( $(cat "$STUB_DIR/count.$key" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "$STUB_DIR/count.$key"
echo "$method $path" >> "$STUB_DIR/calls.log"
if [ -n "$input" ]; then
  cp "$input" "$STUB_DIR/body.$key.$n"
fi
resp="$STUB_DIR/responses/$key.$n"
[ -f "$resp" ] || resp="$STUB_DIR/responses/$key.default"
if [ ! -f "$resp" ]; then
  echo "gh: stub has no response for $method $path (HTTP 599)" >&2
  exit 1
fi
code="$(head -n 1 "$resp")"
[ "$code" = hang ] && { sleep 3; exit 0; }
tail -n +2 "$resp"
case "$code" in
  2??) exit 0 ;;
  neterr) echo "Get \"https://api.github.com/$path\": read tcp: connection reset by peer" >&2; exit 1 ;;
  eoferr) echo "Get \"https://api.github.com/$path\": unexpected EOF" >&2; exit 1 ;;
  otherr) echo "gh: could not determine the base repository; the eof of this text is not a transport error" >&2; exit 1 ;;
  autherr) echo "gh: To use GitHub CLI in a GitHub Actions workflow, set the GH_TOKEN environment variable." >&2; exit 4 ;;
  *) echo "gh: stubbed failure (HTTP $code)" >&2; exit 1 ;;
esac
STUB
chmod +x "$W/stub/gh"
cat >"$W/stub/fake-sleep" <<'STUB'
#!/usr/bin/env bash
echo "$1" >> "$STUB_DIR/sleeps.log"
STUB
chmod +x "$W/stub/fake-sleep"

# The default posting-step stderr: reviewdog reports a failed post that names
# no run id, so the listing path runs. A case overrides it to test the others.
echo "reviewdog: failed to post result: 502 Bad Gateway []" >"$W/failed-post.err"

keyof() { printf '%s %s' "$1" "$2" | tr -c 'A-Za-z0-9\n' '_'; }
list_path() { echo "repos/${REPO}/commits/${SHA}/check-runs?check_name=${ENC}&filter=all&per_page=${2:-100}&page=$1"; }
run_path() { echo "repos/${REPO}/check-runs/$1"; }

# respond <method> <path> <k|default> <code> <body>
respond() {
  printf '%s\n%s\n' "$4" "$5" >"$STUB_DIR/responses/$(keyof "$1" "$2").$3"
}
# run_json <id> <status> [started_at] [app slug] [name] [head sha]
run_json() {
  echo "{\"id\":$1,\"name\":\"${5:-$NAME}\",\"status\":\"$2\",\"started_at\":\"${3:-$STARTED}\",\"head_sha\":\"${6:-$SHA}\",\"app\":{\"slug\":\"${4:-github-actions}\"}}"
}
# page_body <total> <id,status[,started_at[,slug]]>...
page_body() {
  local total="$1" sep="" runs="" id st started slug
  shift
  for r in "$@"; do
    IFS=, read -r id st started slug <<<"$r"
    runs="${runs}${sep}$(run_json "$id" "$st" "${started:-$STARTED}" "${slug:-github-actions}")"
    sep=","
  done
  echo "{\"total_count\":${total},\"check_runs\":[${runs}]}"
}
# orphan <id>: the run reads back in progress, and its PATCH succeeds.
orphan() {
  respond GET "$(run_path "$1")" default 200 "$(run_json "$1" in_progress)"
  respond PATCH "$(run_path "$1")" default 200 "{\"id\":$1,\"status\":\"completed\"}"
}

# fresh <case>: a new stub state directory
fresh() {
  STUB_DIR="$W/case-$1"
  mkdir -p "$STUB_DIR/responses"
  : >"$STUB_DIR/calls.log"
  : >"$STUB_DIR/sleeps.log"
}

# run [VAR=value...]: run the script against the current stub; sets RC and OUT.
run() {
  OUT="$(env PATH="$W/stub:$PATH" STUB_DIR="$STUB_DIR" \
    COMPLETE_ORPHANS_SLEEP="$W/stub/fake-sleep" \
    GITHUB_SERVER_URL=https://github.example GITHUB_REPOSITORY="$REPO" \
    GITHUB_RUN_ID=777 GITHUB_RUN_ATTEMPT=2 JOB_CHECK_RUN_ID=888 STEP_RC=1 \
    REVIEWDOG_STARTED_AT="$RD_START" REVIEWDOG_ENDED_AT="$ENDED" REVIEWDOG_CLOCK=github \
    REVIEWDOG_STDERR_LOG="$W/failed-post.err" \
    "$@" bash "$SCRIPT" "$REPO" "$SHA" "$NAME" "the reviewdog step ended with outcome 'failure'" 2>&1)"
  RC=$?
}
calls() { grep -c -F -x "$1" "$STUB_DIR/calls.log"; }
npatch() { grep -c '^PATCH ' "$STUB_DIR/calls.log"; }
patched() { [ -f "$STUB_DIR/body.$(keyof PATCH "$(run_path "$1")").${2:-1}" ]; }
summary_of() {
  python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["output"]["summary"])' \
    "$STUB_DIR/body.$(keyof PATCH "$(run_path "$1")").1"
}
sleeps() { tr '\n' ' ' <"$STUB_DIR/sleeps.log" | sed 's/ $//'; }
# before <call A> <call B>: call A was first made before call B was.
before() {
  local a b
  a="$(grep -n -F -x "$1" "$STUB_DIR/calls.log" | head -n 1 | cut -d: -f1)"
  b="$(grep -n -F -x "$2" "$STUB_DIR/calls.log" | head -n 1 | cut -d: -f1)"
  [ -n "$a" ] && [ -n "$b" ] && [ "$a" -lt "$b" ]
}
warned() { printf '%s\n' "$OUT" | grep -qF "${WARN}${1:-}"; }
ok() { echo "ok   - $1"; }
bad() {
  echo "FAIL - $1: $2"
  printf '%s\n' "$OUT" | while IFS= read -r l; do echo "       | $l"; done
  fail=1
}

# 1. an in_progress run is PATCHed to completed/neutral; a completed one is not.
fresh basic
respond GET "$(list_path 1)" default 200 "$(page_body 2 11,in_progress 12,completed)"
orphan 11
run
body="$STUB_DIR/body.$(keyof PATCH "$(run_path 11)").1"
if [ "$RC" -eq 0 ] && patched 11 && ! patched 12 && [ "$(npatch)" -eq 1 ] \
  && [ "$(calls "GET $(run_path 12)")" -eq 0 ] && ! warned \
  && python3 - "$body" "$RD_START" "$ENDED" <<'PY'; then
import json, sys
b = json.load(open(sys.argv[1]))
o = b["output"]
assert b["status"] == "completed" and b["conclusion"] == "neutral", b
assert o["title"] == "reviewdog did not finish this check", o
s = o["summary"]
assert "exit 1" in s, s
assert "https://github.example/o/r/actions/runs/777/job/888" in s, s
assert "outcome 'failure'" in s, s
assert f"reviewdog window ({sys.argv[2]} to {sys.argv[3]}, +/- 5 s, GitHub's clock)" in s, s
PY
  ok "an in_progress run is completed as neutral, naming the rc, the job URL and how it was found; a completed run is untouched"
else bad "basic" "rc=$RC"; fi

# 2. a 5xx on the list is retried on the backoff schedule, then warns and exits 0.
fresh list5xx
respond GET "$(list_path 1)" default 502 '{"message":"Bad Gateway"}'
run
if [ "$RC" -eq 0 ] && [ "$(calls "GET $(list_path 1)")" -eq 4 ] && [ "$(sleeps)" = "2 5 10" ] \
  && warned && [ "$(npatch)" -eq 0 ]; then
  ok "a persistent 5xx list is retried at 2/5/10 s, then warns and exits 0"
else bad "list 5xx" "rc=$RC calls=$(calls "GET $(list_path 1)") sleeps='$(sleeps)'"; fi

# 3. pagination reaches page 2, and the orphan found there is completed.
fresh pages
respond GET "$(list_path 1 2)" default 200 "$(page_body 3 21,completed 22,completed)"
respond GET "$(list_path 2 2)" default 200 "$(page_body 3 23,in_progress)"
orphan 23
run COMPLETE_ORPHANS_PER_PAGE=2
if [ "$RC" -eq 0 ] && [ "$(calls "GET $(list_path 2 2)")" -eq 1 ] && patched 23 \
  && [ "$(npatch)" -eq 1 ]; then
  ok "pagination reaches page 2 and completes the orphan found there"
else bad "pagination" "rc=$RC"; fi

# 4. the #4159 shape on the list: a 404 once, then the run is visible.
fresh list404
respond GET "$(list_path 1)" 1 404 '{"message":"Not Found"}'
respond GET "$(list_path 1)" default 200 "$(page_body 1 31,in_progress)"
orphan 31
run
if [ "$RC" -eq 0 ] && patched 31 && [ "$(sleeps)" = "2" ] && ! warned; then
  ok "a list that 404s once and then answers ends with the orphan PATCHed"
else bad "list 404 once" "rc=$RC sleeps='$(sleeps)'"; fi

# 5. the #4159 shape on the PATCH: a 404 once, the re-read still shows it
#    in progress, and the second PATCH lands. Each PATCH follows a read.
fresh patch404
respond GET "$(list_path 1)" default 200 "$(page_body 1 41,in_progress)"
orphan 41
respond PATCH "$(run_path 41)" 1 404 '{"message":"Not Found"}'
run
if [ "$RC" -eq 0 ] && patched 41 2 && [ "$(calls "GET $(run_path 41)")" -eq 2 ] \
  && [ "$(sleeps)" = "2" ] && ! warned; then
  ok "a PATCH that 404s once is re-read and retried, and lands"
else bad "patch 404 once" "rc=$RC gets=$(calls "GET $(run_path 41)") sleeps='$(sleeps)'"; fi

# 6. never PATCH a completed run: when the re-read before a retry shows the
#    run completed, no second PATCH is sent.
fresh patchraced
respond GET "$(list_path 1)" default 200 "$(page_body 1 51,in_progress)"
respond GET "$(run_path 51)" 1 200 "$(run_json 51 in_progress)"
respond GET "$(run_path 51)" default 200 "$(run_json 51 completed)"
respond PATCH "$(run_path 51)" default 404 '{"message":"Not Found"}'
run
if [ "$RC" -eq 0 ] && [ "$(calls "PATCH $(run_path 51)")" -eq 1 ] \
  && printf '%s\n' "$OUT" | grep -q 'check run 51 is completed; left as it is'; then
  ok "a run that completed between attempts is never PATCHed again"
else bad "patch raced" "rc=$RC patches=$(calls "PATCH $(run_path 51)")"; fi

# 7. a PATCH that keeps 5xx-ing gives up after the schedule: warning, exit 0.
fresh patch5xx
respond GET "$(list_path 1)" default 200 "$(page_body 1 61,queued)"
respond GET "$(run_path 61)" default 200 "$(run_json 61 queued)"
respond PATCH "$(run_path 61)" default 503 '{"message":"Unavailable"}'
run
if [ "$RC" -eq 0 ] && [ "$(calls "PATCH $(run_path 61)")" -eq 4 ] && [ "$(sleeps)" = "2 5 10" ] \
  && warned "completing check run 61 failed"; then
  ok "a persistent 5xx PATCH warns after the last attempt and exits 0"
else bad "patch 5xx" "rc=$RC patches=$(calls "PATCH $(run_path 61)") sleeps='$(sleeps)'"; fi

# 8. a non-retryable PATCH error (403) is not retried: warning, exit 0.
fresh patch403
respond GET "$(list_path 1)" default 200 "$(page_body 1 71,in_progress)"
orphan 71
respond PATCH "$(run_path 71)" default 403 '{"message":"Resource not accessible by integration"}'
run
if [ "$RC" -eq 0 ] && [ "$(calls "PATCH $(run_path 71)")" -eq 1 ] && [ -z "$(sleeps)" ] && warned; then
  ok "a 403 PATCH is not retried, warns and exits 0"
else bad "patch 403" "rc=$RC sleeps='$(sleeps)'"; fi

# 9. an unparseable list body warns and exits 0 without PATCHing.
fresh garbage
respond GET "$(list_path 1)" default 200 '<html>not json</html>'
run
if [ "$RC" -eq 0 ] && warned "the check-runs list" && [ "$(npatch)" -eq 0 ]; then
  ok "an unparseable list body warns and exits 0"
else bad "garbage list" "rc=$RC"; fi

# 10. the posting step failed and no run is visible yet: the listing is retried,
#     and the run that appears is completed.
fresh absent
respond GET "$(list_path 1)" 1 200 "$(page_body 0)"
respond GET "$(list_path 1)" default 200 "$(page_body 1 81,in_progress)"
orphan 81
run
if [ "$RC" -eq 0 ] && patched 81 && [ "$(sleeps)" = "2" ]; then
  ok "after a failed step, an empty listing is retried until the run is visible"
else bad "absent then visible" "rc=$RC sleeps='$(sleeps)'"; fi

# 11. no run of that name, and no evidence the step failed (an empty rc, no
#     stderr): one listing, no retry, no warning.
fresh none
respond GET "$(list_path 1)" default 200 "$(page_body 0)"
run STEP_RC= REVIEWDOG_STDERR_LOG=
if [ "$RC" -eq 0 ] && [ "$(calls "GET $(list_path 1)")" -eq 1 ] && [ -z "$(sleeps)" ] && ! warned; then
  ok "no run and no failure evidence: one listing, nothing to complete"
else bad "none after rc 0" "rc=$RC sleeps='$(sleeps)'"; fi

# 12. the shared sleep budget bounds the total retry time.
fresh budget
respond GET "$(list_path 1)" default 500 '{}'
run COMPLETE_ORPHANS_RETRY_DELAYS="20 20 20"
if [ "$RC" -eq 0 ] && [ "$(sleeps)" = "20" ] && [ "$(calls "GET $(list_path 1)")" -eq 2 ]; then
  ok "retries stop when the next sleep would pass the 30 s budget"
else bad "sleep budget" "rc=$RC sleeps='$(sleeps)'"; fi

# 13. the summary reads an unrecorded rc as unknown, and falls back to the run URL.
fresh norc
respond GET "$(list_path 1)" default 200 "$(page_body 1 91,in_progress)"
orphan 91
run STEP_RC= JOB_CHECK_RUN_ID=
if [ "$RC" -eq 0 ] && patched 91 && s="$(summary_of 91)" \
  && grep -qF "unknown (the step recorded no exit status)" <<<"$s" \
  && grep -qF "https://github.example/o/r/actions/runs/777/attempts/2" <<<"$s"; then
  ok "an unrecorded rc reads as unknown, and the run-attempt URL stands in for the job URL"
else bad "no rc" "rc=$RC"; fi

# 14. wrong arguments are a wiring error: exit 2.
OUT="$(bash "$SCRIPT" o/r abc 2>&1)"
RC=$?
if [ "$RC" -eq 2 ]; then ok "wrong arguments exit 2"; else bad "usage" "rc=$RC"; fi

# 15. reviewdog never ran (STEP_RC not-run-*): no run can be this job's, so
#     no API call is made at all.
fresh rc-not-run
respond GET "$(list_path 1)" default 200 "$(page_body 1 101,in_progress)"
orphan 101
run STEP_RC=not-run-no-clippy-json
if [ "$RC" -eq 0 ] && [ ! -s "$STUB_DIR/calls.log" ] && [ -z "$(sleeps)" ] && ! warned \
  && printf '%s\n' "$OUT" | grep -q 'reviewdog never ran (not-run-no-clippy-json)'; then
  ok "STEP_RC=not-run-no-clippy-json: nothing is read or completed"
else bad "STEP_RC=not-run" "rc=$RC calls=$(wc -l <"$STUB_DIR/calls.log")"; fi

# 16. STEP_RC=unknown after reviewdog started (the step died before it
#     returned): a run may exist that nothing shows yet, so an empty listing
#     is waited on for the whole schedule, then warns; and the summary names
#     that rc.
fresh rc-unknown-empty
respond GET "$(list_path 1)" default 200 "$(page_body 0)"
run STEP_RC=unknown REVIEWDOG_STDERR_LOG=
e_ok=0
if [ "$RC" -eq 0 ] && [ "$(calls "GET $(list_path 1)")" -eq 4 ] && [ "$(sleeps)" = "2 5 10" ] && warned "no candidate check run"; then e_ok=1; fi
fresh rc-unknown-run
respond GET "$(list_path 1)" default 200 "$(page_body 1 102,in_progress)"
orphan 102
run STEP_RC=unknown REVIEWDOG_STDERR_LOG=
if [ "$e_ok" -eq 1 ] && [ "$RC" -eq 0 ] && patched 102 \
  && grep -qF -- "- The posting step's final reviewdog exit: unknown (the step ended before reviewdog returned an exit status)" <<<"$(summary_of 102)"; then
  ok "STEP_RC=unknown: an empty listing is waited on, then warns; the summary names the rc"
else bad "STEP_RC=unknown" "rc=$RC empty_ok=$e_ok"; fi

# 17. a 5xx on the re-read backs off and reads again; the PATCH waits for it.
fresh reread5xx
respond GET "$(list_path 1)" default 200 "$(page_body 1 111,in_progress)"
orphan 111
respond GET "$(run_path 111)" 1 503 '{"message":"Unavailable"}'
run
if [ "$RC" -eq 0 ] && patched 111 && [ "$(calls "GET $(run_path 111)")" -eq 2 ] \
  && [ "$(npatch)" -eq 1 ] && [ "$(sleeps)" = "2" ] && ! warned; then
  ok "a 5xx re-read is retried, and the PATCH follows the successful read"
else bad "re-read 5xx once" "rc=$RC gets=$(calls "GET $(run_path 111)") sleeps='$(sleeps)'"; fi

# 18. a re-read that never succeeds means NO PATCH at all, then a warning.
fresh rereaddead
respond GET "$(list_path 1)" default 200 "$(page_body 1 121,in_progress)"
respond GET "$(run_path 121)" default 502 '{}'
respond PATCH "$(run_path 121)" default 200 '{}'
run
if [ "$RC" -eq 0 ] && [ "$(npatch)" -eq 0 ] && [ "$(calls "GET $(run_path 121)")" -eq 4 ] \
  && warned "re-reading check run 121 failed"; then
  ok "a re-read that keeps failing never PATCHes blind; it warns and exits 0"
else bad "re-read dead" "rc=$RC patches=$(npatch)"; fi

# 19. a transport failure with no HTTP status is retried.
fresh neterr
respond GET "$(list_path 1)" 1 neterr ''
respond GET "$(list_path 1)" default 200 "$(page_body 1 131,in_progress)"
orphan 131
run
if [ "$RC" -eq 0 ] && patched 131 && [ "$(sleeps)" = "2" ] && ! warned; then
  ok "a failure with no HTTP status (connection reset) is retried"
else bad "no HTTP status" "rc=$RC sleeps='$(sleeps)'"; fi

# 20. the page cap stops the listing with a warning; what was read is still used.
fresh pagecap
respond GET "$(list_path 1 1)" default 200 "$(page_body 2 141,in_progress)"
respond GET "$(list_path 2 1)" default 200 "$(page_body 2 142,in_progress)"
orphan 141
orphan 142
run COMPLETE_ORPHANS_PER_PAGE=1 COMPLETE_ORPHANS_MAX_PAGES=1
if [ "$RC" -eq 0 ] && warned "stopped listing at the page cap" && patched 141 && ! patched 142 \
  && [ "$(calls "GET $(list_path 2 1)")" -eq 0 ]; then
  ok "the page cap warns and stops listing; the orphan already read is completed"
else bad "page cap" "rc=$RC"; fi

# 21. the time guard: a run that started after this job's reviewdog ended (plus
#     5 s), or before it started (minus 5 s), is not this job's and is skipped;
#     one inside the skew is ours.
fresh late
respond GET "$(list_path 1)" default 200 \
  "$(page_body 3 151,in_progress,2026-11-15T01:06:30Z 152,in_progress,2026-11-15T01:06:25Z 153,in_progress,2026-11-15T01:06:10Z)"
orphan 151
orphan 152
orphan 153
run
if [ "$RC" -eq 0 ] && ! patched 151 && patched 152 && ! patched 153 \
  && [ "$(calls "GET $(run_path 151)")" -eq 0 ] && [ "$(calls "GET $(run_path 153)")" -eq 0 ] \
  && printf '%s\n' "$OUT" | grep -q 'check run 151 skipped: it started at 2026-11-15T01:06:30Z, after' \
  && printf '%s\n' "$OUT" | grep -q 'check run 153 skipped: it started at 2026-11-15T01:06:10Z, before'; then
  ok "runs outside reviewdog's window (+/-5 s) are skipped; one inside the skew is completed"
else bad "time guard" "rc=$RC"; fi

# 21b. a bound from the runner's clock widens the skew to 30 s.
fresh runnerclock
respond GET "$(list_path 1)" default 200 "$(page_body 1 155,in_progress,2026-11-15T01:06:40Z)"
orphan 155
run REVIEWDOG_CLOCK=runner
if [ "$RC" -eq 0 ] && patched 155 && grep -qF "the runner's clock ('runner'), so the skew is widened" <<<"$(summary_of 155)"; then
  ok "a runner-clock bound widens the skew to 30 s, and the summary says so"
else bad "runner clock" "rc=$RC"; fi

# 22. a run from another app is skipped.
fresh foreignapp
respond GET "$(list_path 1)" default 200 "$(page_body 1 161,in_progress,${STARTED},some-other-app)"
orphan 161
run
if [ "$RC" -eq 0 ] && [ "$(npatch)" -eq 0 ] \
  && printf '%s\n' "$OUT" | grep -q "check run 161 skipped: it belongs to app 'some-other-app'"; then
  ok "a run that belongs to another app is skipped"
else bad "foreign app" "rc=$RC"; fi

# 23. the read before the FIRST PATCH finds the run completed: no PATCH at all.
fresh firstread
respond GET "$(list_path 1)" default 200 "$(page_body 1 171,in_progress)"
respond GET "$(run_path 171)" default 200 "$(run_json 171 completed)"
respond PATCH "$(run_path 171)" default 200 '{}'
run
if [ "$RC" -eq 0 ] && [ "$(npatch)" -eq 0 ] && [ "$(calls "GET $(run_path 171)")" -eq 1 ]; then
  ok "the read before the first PATCH finds the run completed, so it is never PATCHed"
else bad "first read completed" "rc=$RC patches=$(npatch)"; fi

# 24. REVIEWDOG_ENDED_AT unknown: the listing is still used, without the time
#     guard, and both the warning and the summary say so.
fresh noended
respond GET "$(list_path 1)" default 200 "$(page_body 1 181,in_progress,2030-01-01T00:00:00Z)"
orphan 181
run REVIEWDOG_ENDED_AT=not-a-time
if [ "$RC" -eq 0 ] && patched 181 && warned "the time guard is OFF for the upper bound" \
  && grep -qF "WITHOUT the upper bound" <<<"$(summary_of 181)"; then
  ok "an unknown end time keeps the listing, and says the time guard is off"
else bad "no ended_at" "rc=$RC"; fi

# 25. reviewdog's stderr names the failing run: only that id, no listing.
fresh exactid
printf '%s\n' "reviewdog: failed to post result: PATCH https://api.github.com/repos/o/r/check-runs/191: 404 Not Found []" \
  >"$STUB_DIR/reviewdog.err"
respond GET "$(list_path 1)" default 200 "$(page_body 1 192,in_progress)"
orphan 191
orphan 192
run REVIEWDOG_STDERR_LOG="$STUB_DIR/reviewdog.err"
if [ "$RC" -eq 0 ] && patched 191 && ! patched 192 && [ "$(calls "GET $(list_path 1)")" -eq 0 ] \
  && grep -qF "reviewdog's own error output names this run" <<<"$(summary_of 191)"; then
  ok "an id in reviewdog's stderr is completed exactly, with no listing"
else bad "exact id" "rc=$RC"; fi

# 26. an exact id whose re-read is another check is left alone.
fresh exactforeign
printf '%s\n' "PATCH https://api.github.com/repos/o/r/check-runs/201: 404 Not Found" >"$STUB_DIR/reviewdog.err"
respond GET "$(run_path 201)" default 200 "$(run_json 201 in_progress "$STARTED" github-actions other-check)"
respond PATCH "$(run_path 201)" default 200 '{}'
run REVIEWDOG_STDERR_LOG="$STUB_DIR/reviewdog.err"
if [ "$RC" -eq 0 ] && [ "$(npatch)" -eq 0 ] && printf '%s\n' "$OUT" | grep -q "check run 201 skipped: its name is 'other-check'"; then
  ok "an exact id that reads back as a different check is not PATCHed"
else bad "exact id foreign" "rc=$RC"; fi

# 27. stderr with no check-runs id falls back to the listing.
fresh exactnone
printf '%s\n' "reviewdog: failed to create check: POST https://api.github.com/repos/o/r/check-runs: 502 Bad Gateway" >"$STUB_DIR/reviewdog.err"
respond GET "$(list_path 1)" default 200 "$(page_body 1 211,in_progress)"
orphan 211
run REVIEWDOG_STDERR_LOG="$STUB_DIR/reviewdog.err"
if [ "$RC" -eq 0 ] && patched 211 && [ "$(calls "GET $(list_path 1)")" -eq 1 ]; then
  ok "stderr naming no run id falls back to the listing"
else bad "exact none" "rc=$RC"; fi

# 28-29. an exact id whose re-read shows a foreign app, or a foreign head sha,
#        is not PATCHed.
for fc in "app|221|github-actions|some-other-app|$SHA|it belongs to app 'some-other-app'" \
  "head|222|$NAME|github-actions|def456|it is on head 'def456'"; do
  IFS='|' read -r what fid _ fapp fhead fwant <<<"$fc"
  fresh "exact-foreign-$what"
  printf '%s\n' "reviewdog: failed to post result: PATCH https://api.github.com/repos/o/r/check-runs/${fid}: 404 Not Found" >"$STUB_DIR/reviewdog.err"
  respond GET "$(run_path "$fid")" default 200 "$(run_json "$fid" in_progress "$STARTED" "$fapp" "$NAME" "$fhead")"
  respond PATCH "$(run_path "$fid")" default 200 '{}'
  run REVIEWDOG_STDERR_LOG="$STUB_DIR/reviewdog.err"
  if [ "$RC" -eq 0 ] && [ "$(npatch)" -eq 0 ] && printf '%s\n' "$OUT" | grep -qF "check run ${fid} skipped: ${fwant}"; then
    ok "an exact id that reads back with a foreign ${what} is not PATCHed"
  else bad "exact id foreign ${what}" "rc=$RC patches=$(npatch)"; fi
done

# 30. a listed run with an unreadable started_at is skipped.
fresh badstart
respond GET "$(list_path 1)" default 200 "$(page_body 1 231,in_progress,not-a-time)"
orphan 231
run
if [ "$RC" -eq 0 ] && [ "$(npatch)" -eq 0 ] \
  && printf '%s\n' "$OUT" | grep -q "check run 231 skipped: its started_at 'not-a-time' is unreadable"; then
  ok "a run with an unreadable started_at is skipped"
else bad "unreadable started_at" "rc=$RC"; fi

# 31. the list and the re-read share one sleep budget.
fresh sharedbudget
respond GET "$(list_path 1)" 1 503 '{}'
respond GET "$(list_path 1)" default 200 "$(page_body 1 241,in_progress)"
respond GET "$(run_path 241)" default 503 '{}'
respond PATCH "$(run_path 241)" default 200 '{}'
run COMPLETE_ORPHANS_RETRY_DELAYS="20 20 20"
if [ "$RC" -eq 0 ] && [ "$(sleeps)" = "20" ] && [ "$(calls "GET $(run_path 241)")" -eq 1 ] \
  && [ "$(npatch)" -eq 0 ] && warned "re-reading check run 241 failed"; then
  ok "a list retry spends the budget the re-read then cannot exceed"
else bad "shared budget" "rc=$RC sleeps='$(sleeps)' gets=$(calls "GET $(run_path 241)")"; fi

# 32. the same id on two pages is read and PATCHed once.
fresh dedup
respond GET "$(list_path 1 1)" default 200 "$(page_body 2 251,in_progress)"
respond GET "$(list_path 2 1)" default 200 "$(page_body 2 251,in_progress)"
orphan 251
run COMPLETE_ORPHANS_PER_PAGE=1
if [ "$RC" -eq 0 ] && [ "$(calls "PATCH $(run_path 251)")" -eq 1 ] && [ "$(calls "GET $(run_path 251)")" -eq 1 ]; then
  ok "an id listed twice is completed once"
else bad "dedup" "rc=$RC patches=$(calls "PATCH $(run_path 251)")"; fi

# 33. a 422 on the list is not retried: one call, a warning, exit 0.
fresh list422
respond GET "$(list_path 1)" default 422 '{"message":"Validation Failed"}'
run
if [ "$RC" -eq 0 ] && [ "$(calls "GET $(list_path 1)")" -eq 1 ] && [ -z "$(sleeps)" ] && warned "listing check runs"; then
  ok "a 422 list is not retried; it warns and exits 0"
else bad "list 422" "rc=$RC calls=$(calls "GET $(list_path 1)")"; fi

# 34. visible runs all skipped by the guard. When reviewdog reported a
#     failure, this job's run may just not be visible yet, so it keeps
#     waiting and the final warning names the skips; with no failure
#     reported, it says so and stops without waiting.
fresh allskipped
respond GET "$(list_path 1)" default 200 "$(page_body 1 261,in_progress,2026-11-15T01:09:00Z)"
run
w_ok=0
if [ "$RC" -eq 0 ] && [ "$(sleeps)" = "2 5 10" ] && [ "$(calls "GET $(list_path 1)")" -eq 4 ] \
  && warned "no candidate check run named 'clippy-diff (advisory)' is visible on ${SHA}, although the posting step ended exit 1; 1 run(s) were skipped by the guard"; then w_ok=1; fi
fresh allskipped-nofail
respond GET "$(list_path 1)" default 200 "$(page_body 1 262,in_progress,2026-11-15T01:09:00Z)"
run STEP_RC= REVIEWDOG_STDERR_LOG=
if [ "$w_ok" -eq 1 ] && [ "$RC" -eq 0 ] && [ -z "$(sleeps)" ] && [ "$(calls "GET $(list_path 1)")" -eq 1 ] && ! warned \
  && printf '%s\n' "$OUT" | grep -q '1 run(s) skipped by the guard (listed above) as not this job'; then
  ok "all visible runs skipped: it waits when reviewdog reported a failure, and stops at once when it did not"
else bad "all skipped" "rc=$RC wait_ok=$w_ok sleeps='$(sleeps)'"; fi

# 35. a secondary rate limit (403) is retried; a gh auth error with no HTTP
#     status is not.
fresh secondary
respond GET "$(list_path 1)" 1 403 '{"message":"You have exceeded a secondary rate limit."}'
respond GET "$(list_path 1)" default 200 "$(page_body 1 271,in_progress)"
orphan 271
run
s_ok=0
if [ "$RC" -eq 0 ] && patched 271 && [ "$(sleeps)" = "2" ]; then s_ok=1; fi
fresh autherr
respond GET "$(list_path 1)" default autherr ''
run
if [ "$s_ok" -eq 1 ] && [ "$RC" -eq 0 ] && [ "$(calls "GET $(list_path 1)")" -eq 1 ] && [ -z "$(sleeps)" ] && warned; then
  ok "a secondary-rate-limit 403 is retried; a no-status gh auth error is not"
else bad "403 secondary / auth error" "rc=$RC secondary_ok=$s_ok sleeps='$(sleeps)'"; fi

# 36. a later page that fails keeps the candidates already read, with a warning.
fresh page2fail
respond GET "$(list_path 1 1)" default 200 "$(page_body 2 281,in_progress)"
respond GET "$(list_path 2 1)" default 422 '{"message":"Validation Failed"}'
orphan 281
run COMPLETE_ORPHANS_PER_PAGE=1
if [ "$RC" -eq 0 ] && patched 281 && warned "listing check runs 'clippy-diff (advisory)' on ${SHA} failed at page 2"; then
  ok "a failed page 2 keeps page 1's orphan and completes it, with a warning"
else bad "page 2 fails" "rc=$RC"; fi

# 37-38. reviewdog finished its run (a number for rc, and stderr naming no
#        failed create or post): no API call at all, and a notice says so.
#        rc 0 is coord's clean pass; rc 1 is the runner's diff-findings exit.
for rcv in 0 1; do
  fresh "clean-$rcv"
  printf '%s\n' "reviewdog: some findings were reported" >"$STUB_DIR/clean.err"
  respond GET "$(list_path 1)" default 200 "$(page_body 1 291,in_progress)"
  orphan 291
  run STEP_RC="$rcv" REVIEWDOG_STDERR_LOG="$STUB_DIR/clean.err"
  if [ "$RC" -eq 0 ] && [ ! -s "$STUB_DIR/calls.log" ] && ! warned \
    && printf '%s\n' "$OUT" | grep -qF "::notice title=complete-orphaned-check-runs::reviewdog exited ${rcv} and its stderr (clean) reports no failed check create or post"; then
    ok "a clean reviewdog exit ${rcv} makes no API call, with a notice"
  else bad "clean rc ${rcv}" "rc=$RC calls=$(wc -l <"$STUB_DIR/calls.log")"; fi
done

# 39. the posting step was skipped: no API call.
fresh skipped
respond GET "$(list_path 1)" default 200 "$(page_body 1 301,in_progress)"
orphan 301
run STEP_RC= STEP_OUTCOME=skipped
if [ "$RC" -eq 0 ] && [ ! -s "$STUB_DIR/calls.log" ] && printf '%s\n' "$OUT" | grep -q 'posting step was skipped'; then
  ok "a skipped posting step makes no API call"
else bad "skipped step" "rc=$RC calls=$(wc -l <"$STUB_DIR/calls.log")"; fi

# 40. the re-read answers 200 with a body that is not JSON: no PATCH, a warning.
fresh rereadgarbage
respond GET "$(list_path 1)" default 200 "$(page_body 1 311,in_progress)"
respond GET "$(run_path 311)" default 200 '<html>not json</html>'
respond PATCH "$(run_path 311)" default 200 '{}'
run
if [ "$RC" -eq 0 ] && [ "$(npatch)" -eq 0 ] && warned "re-reading check run 311 returned a body that could not be read"; then
  ok "a re-read whose body cannot be read is never PATCHed; it warns"
else bad "re-read garbage" "rc=$RC patches=$(npatch)"; fi

# 41. a gh call that hits the per-call timeout is a retryable no-status failure.
fresh hang
respond GET "$(list_path 1)" 1 hang ''
respond GET "$(list_path 1)" default 200 "$(page_body 1 321,in_progress)"
orphan 321
run COMPLETE_ORPHANS_CALL_TIMEOUT=1
if [ "$RC" -eq 0 ] && patched 321 && [ "$(sleeps)" = "2" ] && ! warned \
  && printf '%s\n' "$OUT" | grep -q 'listing .* failed (HTTP none): retry 1/3'; then
  ok "a gh call past the per-call timeout is retried"
else bad "call timeout" "rc=$RC sleeps='$(sleeps)'"; fi

# 42. an earlier attempt's COMPLETED run is visible from the first listing,
#     and this job's orphan only appears on a later one: the wait goes on past
#     the completed row, and the orphan is PATCHed.
fresh completedfirst
respond GET "$(list_path 1)" 1 200 "$(page_body 1 331,completed)"
respond GET "$(list_path 1)" default 200 "$(page_body 2 331,completed 332,in_progress)"
orphan 332
respond GET "$(run_path 331)" default 200 "$(run_json 331 completed)"
run
if [ "$RC" -eq 0 ] && patched 332 && ! patched 331 && [ "$(sleeps)" = "2" ] && ! warned; then
  ok "a completed run visible first does not end the wait; the later orphan is PATCHed"
else bad "completed first" "rc=$RC sleeps='$(sleeps)'"; fi

# 43. rc unknown with an end time but no start time: the step ended before it
#     reached reviewdog, so no API call.
fresh nostart
respond GET "$(list_path 1)" default 200 "$(page_body 1 341,in_progress)"
orphan 341
run STEP_RC=unknown REVIEWDOG_STARTED_AT=
if [ "$RC" -eq 0 ] && [ ! -s "$STUB_DIR/calls.log" ] \
  && printf '%s\n' "$OUT" | grep -q 'no reviewdog start time, so reviewdog never ran'; then
  ok "rc unknown with no start time makes no API call"
else bad "no start time" "rc=$RC calls=$(wc -l <"$STUB_DIR/calls.log")"; fi

# 44. an out-of-window orphan (a concurrent run's) is skipped on the first
#     listing; this job's own run appears later and is PATCHed.
fresh skipthenours
respond GET "$(list_path 1)" 1 200 "$(page_body 1 351,in_progress,2026-11-15T01:09:00Z)"
respond GET "$(list_path 1)" default 200 "$(page_body 2 351,in_progress,2026-11-15T01:09:00Z 352,in_progress)"
orphan 351
orphan 352
run
if [ "$RC" -eq 0 ] && patched 352 && ! patched 351 && [ "$(sleeps)" = "2" ] && ! warned; then
  ok "a skipped out-of-window run does not end the wait; this job's later run is PATCHed"
else bad "skip then ours" "rc=$RC sleeps='$(sleeps)'"; fi

# 45. reviewdog killed by a signal (rc 143, clean stderr) is treated like
#     unknown: the orphan is looked for and completed, and the summary says so.
fresh signalled
printf '%s\n' "reviewdog: posting annotations" >"$STUB_DIR/clean.err"
respond GET "$(list_path 1)" default 200 "$(page_body 1 361,in_progress)"
orphan 361
run STEP_RC=143 REVIEWDOG_STDERR_LOG="$STUB_DIR/clean.err"
if [ "$RC" -eq 0 ] && patched 361 && ! warned \
  && grep -qF -- "- The posting step's final reviewdog exit: exit 143 (killed by signal 15)" <<<"$(summary_of 361)"; then
  ok "rc 143 (signal-killed reviewdog) is looked for like unknown, and the summary names the signal"
else bad "rc 143" "rc=$RC calls=$(wc -l <"$STUB_DIR/calls.log")"; fi

# 46. retry delays summing exactly to the budget are all slept.
fresh budgetedge
respond GET "$(list_path 1)" default 503 '{}'
run COMPLETE_ORPHANS_RETRY_DELAYS="10 10 10" COMPLETE_ORPHANS_SLEEP_BUDGET=30
if [ "$RC" -eq 0 ] && [ "$(sleeps)" = "10 10 10" ] && [ "$(calls "GET $(list_path 1)")" -eq 4 ]; then
  ok "delays that sum exactly to the budget are all used"
else bad "budget edge" "rc=$RC sleeps='$(sleeps)'"; fi

# 47. an EMPTY rc with an end time but no start time: reviewdog never ran.
fresh emptynostart
respond GET "$(list_path 1)" default 200 "$(page_body 1 371,in_progress)"
orphan 371
run STEP_RC= REVIEWDOG_STARTED_AT=
if [ "$RC" -eq 0 ] && [ ! -s "$STUB_DIR/calls.log" ] \
  && printf '%s\n' "$OUT" | grep -q "with rc '' and no reviewdog start time, so reviewdog never ran"; then
  ok "an empty rc with no start time makes no API call"
else bad "empty rc no start" "rc=$RC calls=$(wc -l <"$STUB_DIR/calls.log")"; fi

# 48. a WIRING error: the posting step ran to its end with outcome success,
#     yet every output this step reads is empty, so it is reading outputs
#     that do not exist. No API call, and a warning names it. An outcome of
#     failure or cancelled with no outputs is NOT that (a SIGKILLed step shell
#     fails and writes none): it is looked for as unknown, guard off, warned.
fresh miswired-success
respond GET "$(list_path 1)" default 200 "$(page_body 1 381,in_progress)"
orphan 381
run STEP_OUTCOME=success STEP_RC= REVIEWDOG_STDERR_LOG= REVIEWDOG_STARTED_AT= REVIEWDOG_ENDED_AT=
if [ "$RC" -eq 0 ] && [ ! -s "$STUB_DIR/calls.log" ] && warned "the posting step ended with outcome 'success', which always writes rc"; then
  ok "outcome success with no outputs at all is a wiring error: a warning, no API call"
else bad "miswired success" "rc=$RC calls=$(wc -l <"$STUB_DIR/calls.log")"; fi
for outcome in failure cancelled; do
  fresh "no-outputs-$outcome"
  respond GET "$(list_path 1)" default 200 "$(page_body 1 382,in_progress)"
  orphan 382
  run STEP_OUTCOME="$outcome" STEP_RC= REVIEWDOG_STDERR_LOG= REVIEWDOG_STARTED_AT= REVIEWDOG_ENDED_AT=
  if [ "$RC" -eq 0 ] && patched 382 && ! warned "which always writes rc" && warned "the time guard is OFF"; then
    ok "outcome ${outcome} with no outputs is looked for as unknown, guard off and warned"
  else bad "no outputs, ${outcome}" "rc=$RC"; fi
done

# 49. rc unknown (or a signal's 128+) after reviewdog started, clean stderr:
#     the first listing is empty, the run appears on the next and is PATCHed.
for rcv in unknown 137; do
  fresh "late-$rcv"
  printf '%s\n' "reviewdog: posting" >"$STUB_DIR/clean.err"
  respond GET "$(list_path 1)" 1 200 "$(page_body 0)"
  respond GET "$(list_path 1)" default 200 "$(page_body 1 391,in_progress)"
  orphan 391
  run STEP_RC="$rcv" REVIEWDOG_STDERR_LOG="$STUB_DIR/clean.err"
  if [ "$RC" -eq 0 ] && patched 391 && [ "$(sleeps)" = "2" ] && ! warned; then
    ok "rc ${rcv} after reviewdog started waits for the run to become visible, then PATCHes it"
  else bad "late run, rc ${rcv}" "rc=$RC sleeps='$(sleeps)'"; fi
done

# 50. the wall-clock bound: once it has passed, no new API call is started;
#     the script warns and exits 0.
fresh deadline
respond GET "$(list_path 1)" default 200 "$(page_body 1 401,in_progress)"
orphan 401
run COMPLETE_ORPHANS_DEADLINE_SECS=0
if [ "$RC" -eq 0 ] && [ ! -s "$STUB_DIR/calls.log" ] && warned \
  && printf '%s\n' "$OUT" | grep -qF "(the 0s wall-clock bound was reached, so no new API call was started)"; then
  ok "past the wall-clock bound no API call is started; it warns and exits 0"
else bad "deadline" "rc=$RC calls=$(wc -l <"$STUB_DIR/calls.log")"; fi

# 51. no-status retries are anchored to connection-class phrases: an
#     "unexpected EOF" is retried; a message that merely contains "eof" is not.
fresh eof
respond GET "$(list_path 1)" 1 eoferr ''
respond GET "$(list_path 1)" default 200 "$(page_body 1 411,in_progress)"
orphan 411
run
e_ok=0
if [ "$RC" -eq 0 ] && patched 411 && [ "$(sleeps)" = "2" ]; then e_ok=1; fi
fresh othereof
respond GET "$(list_path 1)" default otherr ''
run
if [ "$e_ok" -eq 1 ] && [ "$RC" -eq 0 ] && [ "$(calls "GET $(list_path 1)")" -eq 1 ] && [ -z "$(sleeps)" ] && warned; then
  ok "an unexpected EOF is retried; text that only contains 'eof' is not"
else bad "eof anchoring" "rc=$RC eof_ok=$e_ok sleeps='$(sleeps)'"; fi

# --- no gh on PATH: the curl fallback ------------------------------------------
# A PATH holding only what the script needs, without gh, plus a stub curl that
# serves the same responses/<key> files the gh stub does and logs each call in
# the same "METHOD path" form.
mkdir -p "$W/minbin" "$W/curlstub"
# (cp, stat and dirname are the stub's, not the script's.)
for tool in bash env python3 timeout grep cut tr sed awk sort tee wc cat rm mktemp head tail cp stat dirname; do
  ln -sf "$(command -v "$tool")" "$W/minbin/$tool"
done
cat >"$W/curlstub/curl" <<'STUB'
#!/usr/bin/env bash
echo "$*" >>"$STUB_DIR/curl-argv.log"
method=GET out="" input="" url="" auth=""
while [ $# -gt 0 ]; do
  case "$1" in
    -X) method="$2"; shift ;;
    -o) out="$2"; shift ;;
    -K) auth="$2"; shift ;;
    --data-binary) input="${2#@}"; shift ;;
    -H | -w | --max-time) shift ;;
    -*) ;;
    *) url="$1" ;;
  esac
  shift
done
if [ -n "$auth" ]; then
  cp "$auth" "$STUB_DIR/curl-auth.seen"
  stat -c %a "$auth" >>"$STUB_DIR/curl-auth.modes"
  dirname "$auth" >>"$STUB_DIR/curl-auth.dirs"
fi
path="${url#https://api.example/}"
key="$(printf '%s %s' "$method" "$path" | tr -c 'A-Za-z0-9\n' '_')"
n=$(( $(cat "$STUB_DIR/count.$key" 2>/dev/null || echo 0) + 1 ))
echo "$n" >"$STUB_DIR/count.$key"
echo "$method $path" >>"$STUB_DIR/calls.log"
[ -z "$input" ] || cp "$input" "$STUB_DIR/body.$key.$n"
resp="$STUB_DIR/responses/$key.$n"
[ -f "$resp" ] || resp="$STUB_DIR/responses/$key.default"
code="$(head -n 1 "$resp")"
case "$code" in
  neterr) echo "curl: (56) Recv failure: Connection reset by peer" >&2; exit 56 ;;
  curlexit35) echo "curl: (35) OpenSSL SSL_connect: SSL_ERROR_SYSCALL in connection to api.example:443" >&2; exit 35 ;;
  curlexit52) echo "curl: (52) Empty reply from server" >&2; exit 52 ;;
  curlexit16) echo "curl: (16) Error in the HTTP2 framing layer" >&2; exit 16 ;;
  curlexit60) echo "curl: (60) SSL certificate problem: unable to get local issuer certificate" >&2; exit 60 ;;
esac
tail -n +2 "$resp" >"$out"
printf '%s' "$code"
STUB
chmod +x "$W/curlstub/curl"
NOGH_PATH="$W/curlstub:$W/minbin"

# 52. gh absent, curl and GH_TOKEN present: the same calls go over curl, the
#     orphan is PATCHed, the token is in curl's config file and never on argv.
fresh curlpath
respond GET "$(list_path 1)" 1 neterr ''
respond GET "$(list_path 1)" default 200 "$(page_body 1 421,in_progress)"
orphan 421
run PATH="$NOGH_PATH" GITHUB_API_URL=https://api.example GH_TOKEN=tok-secret-421
if [ "$RC" -eq 0 ] && patched 421 && [ "$(sleeps)" = "2" ] && ! warned \
  && printf '%s\n' "$OUT" | grep -qF "::notice title=complete-orphaned-check-runs::\`gh\` is not on PATH; making the same REST calls with curl" \
  && ! grep -q 'tok-secret-421' "$STUB_DIR/curl-argv.log" \
  && grep -qF 'header = "Authorization: Bearer tok-secret-421"' "$STUB_DIR/curl-auth.seen" \
  && python3 -c 'import json, sys; b = json.load(open(sys.argv[1])); assert b["conclusion"] == "neutral"' \
    "$STUB_DIR/body.$(keyof PATCH "$(run_path 421)").1"; then
  ok "without gh, curl makes the same calls (retrying a reset), token in a config file, never on argv"
else bad "curl fallback" "rc=$RC sleeps='$(sleeps)'"; fi

# 53. gh absent and no GH_TOKEN: nothing can be read; the warning says the
#     orphans will NOT be completed.
fresh nogh-notoken
respond GET "$(list_path 1)" default 200 "$(page_body 1 431,in_progress)"
run PATH="$NOGH_PATH" GH_TOKEN=
if [ "$RC" -eq 0 ] && [ ! -s "$STUB_DIR/calls.log" ] \
  && warned "\`gh\` is not on PATH, and curl with GH_TOKEN is not available either, so no check run was read; orphaned check runs on this head will NOT be completed."; then
  ok "without gh or a token, it warns that orphaned runs will NOT be completed"
else bad "no gh, no token" "rc=$RC"; fi

# 54. the runner-clock skew is 30 s, not wider: a run that started 32 s after
#     reviewdog ended is skipped even on the runner's clock.
fresh runnerclock-far
respond GET "$(list_path 1)" default 200 "$(page_body 1 441,in_progress,2026-11-15T01:06:53Z)"
orphan 441
run REVIEWDOG_CLOCK=runner STEP_RC=unknown REVIEWDOG_STDERR_LOG= COMPLETE_ORPHANS_RETRY_DELAYS=
if [ "$RC" -eq 0 ] && ! patched 441 \
  && printf '%s\n' "$OUT" | grep -q 'check run 441 skipped: it started at 2026-11-15T01:06:53Z, after this job.s reviewdog ended (plus 30 s)'; then
  ok "on the runner's clock a run 32 s past the window is still skipped (skew is 30 s)"
else bad "runner clock far" "rc=$RC"; fi

# 55. curl path: a 404 on the first re-read (read-after-write lag), then 200:
#     one 2 s backoff, then the PATCH.
fresh curl-reread404
respond GET "$(list_path 1)" default 200 "$(page_body 1 451,in_progress)"
orphan 451
respond GET "$(run_path 451)" 1 404 '{"message":"Not Found"}'
run PATH="$NOGH_PATH" GITHUB_API_URL=https://api.example GH_TOKEN=tok-455
if [ "$RC" -eq 0 ] && patched 451 && [ "$(sleeps)" = "2" ] && [ "$(calls "GET $(run_path 451)")" -eq 2 ] && ! warned; then
  ok "curl path: a 404 re-read is retried after 2 s, then the run is PATCHed"
else bad "curl re-read 404" "rc=$RC sleeps='$(sleeps)'"; fi

# 56. curl path: a 403 with a plain message on the PATCH is not retried; it
#     warns, and no "completed check run" line is printed.
fresh curl-patch403
respond GET "$(list_path 1)" default 200 "$(page_body 1 461,in_progress)"
orphan 461
respond PATCH "$(run_path 461)" default 403 '{"message":"Resource not accessible by integration"}'
run PATH="$NOGH_PATH" GITHUB_API_URL=https://api.example GH_TOKEN=tok-456
if [ "$RC" -eq 0 ] && [ "$(calls "PATCH $(run_path 461)")" -eq 1 ] && [ -z "$(sleeps)" ] \
  && warned "completing check run 461 failed (HTTP 403: curl: Resource not accessible by integration (HTTP 403))" \
  && ! printf '%s\n' "$OUT" | grep -q 'completed check run 461'; then
  ok "curl path: a plain 403 PATCH warns, is not retried, and claims no completion"
else bad "curl PATCH 403" "rc=$RC patches=$(calls "PATCH $(run_path 461)") sleeps='$(sleeps)'"; fi

# 57. curl path: the token file is mode 0600 while curl reads it, it lives in
#     a work dir under RUNNER_TEMP, and that dir is gone after the script ends.
fresh curl-tokenfile
mkdir -p "$STUB_DIR/rt"
respond GET "$(list_path 1)" default 200 "$(page_body 1 471,in_progress)"
orphan 471
run PATH="$NOGH_PATH" GITHUB_API_URL=https://api.example GH_TOKEN=tok-457 RUNNER_TEMP="$STUB_DIR/rt"
wdir="$(head -n 1 "$STUB_DIR/curl-auth.dirs" 2>/dev/null)"
if [ "$RC" -eq 0 ] && patched 471 && [ "$(sort -u "$STUB_DIR/curl-auth.modes")" = 600 ] \
  && grep -q -- ' -L ' "$STUB_DIR/curl-argv.log" \
  && [ "${wdir#"$STUB_DIR"/rt/}" != "$wdir" ] && [ ! -e "$wdir" ]; then
  ok "curl path: -L is passed; the token file is 0600 in a RUNNER_TEMP work dir that is removed at exit"
else bad "curl token file" "rc=$RC modes=$(tr '\n' ' ' <"$STUB_DIR/curl-auth.modes" 2>/dev/null) dir=$wdir"; fi

# 58. curl path: curl's own transport failures -- 35 (SSL connect), 52 (empty
#     reply), 16 (HTTP/2 framing) -- are retried.
for cx in 35 52 16; do
  fresh "curl-exit$cx"
  respond GET "$(list_path 1)" 1 "curlexit$cx" ''
  respond GET "$(list_path 1)" default 200 "$(page_body 1 481,in_progress)"
  orphan 481
  run PATH="$NOGH_PATH" GITHUB_API_URL=https://api.example GH_TOKEN=tok-458
  if [ "$RC" -eq 0 ] && patched 481 && [ "$(sleeps)" = "2" ] && ! warned; then
    ok "curl path: curl exit ${cx} is retried"
  else bad "curl exit ${cx}" "rc=$RC sleeps='$(sleeps)'"; fi
  # The marker itself: when every attempt fails the same way, the warning must
  # carry the exact "curl transport failure (curl exit N)" line curl_api wrote,
  # with the exit code of THIS failure and a no-status ("HTTP none") reading.
  fresh "curl-exit$cx-marker"
  respond GET "$(list_path 1)" default "curlexit$cx" ''
  run PATH="$NOGH_PATH" GITHUB_API_URL=https://api.example GH_TOKEN=tok-458
  if [ "$RC" -eq 0 ] \
    && warned "listing check runs '${NAME}' on ${SHA} failed at page 1 (HTTP none: curl: (${cx}) " \
    && printf '%s\n' "$OUT" | grep -qF "curl transport failure (curl exit ${cx}))."; then
    ok "curl path: exit ${cx} surfaces the marker 'curl transport failure (curl exit ${cx})' with no HTTP status"
  else bad "curl exit ${cx} marker" "rc=$RC"; fi
done
# A curl exit outside the connection class (60, a certificate problem) writes
# no marker and is not retried.
fresh curl-exit60
respond GET "$(list_path 1)" default curlexit60 ''
run PATH="$NOGH_PATH" GITHUB_API_URL=https://api.example GH_TOKEN=tok-458
if [ "$RC" -eq 0 ] && [ -z "$(sleeps)" ] && warned && ! printf '%s\n' "$OUT" | grep -qF 'curl transport failure'; then
  ok "curl path: exit 60 carries no transport marker and is not retried"
else bad "curl exit 60" "rc=$RC sleeps='$(sleeps)'"; fi

# 59. a GH_TOKEN holding a double quote, a backslash or a newline is not
#     written into curl's config: a warning, and no call.
for tok in 'tok"x' 'tok\x' $'tok\nx'; do
  fresh badtoken
  respond GET "$(list_path 1)" default 200 "$(page_body 1 491,in_progress)"
  run PATH="$NOGH_PATH" GITHUB_API_URL=https://api.example GH_TOKEN="$tok"
  if [ "$RC" -eq 0 ] && [ ! -s "$STUB_DIR/calls.log" ] && warned "GH_TOKEN contains a double quote, a backslash or a newline"; then
    ok "a GH_TOKEN with an unsafe character is refused for the curl config ($(printf '%q' "$tok"))"
  else bad "bad token $(printf '%q' "$tok")" "rc=$RC"; fi
  rm -rf "$STUB_DIR"
done

# 60. the allowlist: a repo, head_sha or check_name outside it is a wiring
#     error (exit 2) before any API call, and nothing is echoed raw.
fresh allowlist
respond GET "$(list_path 1)" default 200 "$(page_body 1 501,in_progress)"
orphan 501
allow_bad() { # <label> <repo> <sha> <name>
  OUT="$(env PATH="$W/stub:$PATH" STUB_DIR="$STUB_DIR" STEP_RC=unknown \
    bash "$SCRIPT" "$2" "$3" "$4" why 2>&1)"
  RC=$?
  if [ "$RC" -eq 2 ] && [ ! -s "$STUB_DIR/calls.log" ] && printf '%s\n' "$OUT" | grep -q "^refusing $1 "; then
    ok "the allowlist refuses a bad $1 (exit 2, no call)"
  else bad "allowlist $1" "rc=$RC"; fi
}
allow_bad repo 'o/r/../x' "$SHA" "$NAME"
allow_bad repo 'o r' "$SHA" "$NAME"
allow_bad repo 'justname' "$SHA" "$NAME"
allow_bad head_sha "$REPO" 'abc12' "$NAME"
allow_bad head_sha "$REPO" 'main?x=1' "$NAME"
allow_bad head_sha "$REPO" 'abcdefg' "$NAME"
allow_bad check_name "$REPO" "$SHA" $'clippy\n::error::x'
allow_bad check_name "$REPO" "$SHA" 'a"b'
allow_bad check_name "$REPO" "$SHA" "$(printf 'x%.0s' $(seq 1 101))"
fresh allowlist-ok
respond GET "$(list_path 1)" default 200 "$(page_body 1 511,in_progress)"
orphan 511
run
if [ "$RC" -eq 0 ] && patched 511 && ! warned; then
  ok "a conforming repo, sha and check name are accepted"
else bad "allowlist accepts" "rc=$RC"; fi

# 61. SIGINT (a job cancel) removes the work dir. The script is launched through
#     a python exec that resets SIGINT to SIG_DFL: a background job started by a
#     non-interactive shell inherits SIGINT as IGNORED, which makes `kill -INT`
#     a no-op and the case vacuous, so the precondition is asserted (SigIgn in
#     /proc, where it exists; elsewhere the case is skipped LOUDLY).
fresh sigint
mkdir -p "$STUB_DIR/rt"
mkdir -p "$W/stub-hang"
cat >"$W/stub-hang/gh" <<'STUB'
#!/usr/bin/env bash
echo called >>"$STUB_DIR/hang-called"
sleep 3
STUB
chmod +x "$W/stub-hang/gh"
if [ ! -r /proc/self/status ]; then
  echo "SKIP - 61 (SIGINT cleanup): no /proc to verify SIGINT is not ignored; NOT TESTED on this platform"
else
  python3 -c 'import os, signal, sys
signal.signal(signal.SIGINT, signal.SIG_DFL)
signal.signal(signal.SIGTERM, signal.SIG_DFL)
os.execvp("env", ["env"] + sys.argv[1:])' \
    PATH="$W/stub-hang:$PATH" STUB_DIR="$STUB_DIR" RUNNER_TEMP="$STUB_DIR/rt" STEP_RC=unknown \
    REVIEWDOG_STARTED_AT="$RD_START" REVIEWDOG_ENDED_AT="$ENDED" REVIEWDOG_CLOCK=github \
    bash "$SCRIPT" "$REPO" "$SHA" "$NAME" why >"$STUB_DIR/sigint.out" 2>&1 &
  sigpid=$!
  for _ in $(seq 1 50); do
    [ -s "$STUB_DIR/hang-called" ] && break
    sleep 0.1
  done
  sigign="$(awk '/^SigIgn:/ {print $2}' "/proc/${sigpid}/status" 2>/dev/null)"
  # SIGINT is signal 2: bit 0x2 of the SigIgn mask.
  if [ -z "$sigign" ] || [ $((16#$sigign & 2)) -ne 0 ]; then
    kill "$sigpid" 2>/dev/null
    wait "$sigpid" 2>/dev/null
    bad "SIGINT precondition" "SIGINT is still ignored in the child (SigIgn='${sigign}') or it was not readable; the case would be vacuous"
  elif [ ! -s "$STUB_DIR/hang-called" ]; then
    kill "$sigpid" 2>/dev/null
    wait "$sigpid" 2>/dev/null
    bad "SIGINT precondition" "the script never reached its first API call"
  else
    leftover_before="$(find "$STUB_DIR/rt" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')"
    kill -INT "$sigpid"
    wait "$sigpid"
    RC=$?
    leftover_after="$(find "$STUB_DIR/rt" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')"
    OUT="$(cat "$STUB_DIR/sigint.out")"
    if [ "$leftover_before" -eq 1 ] && [ "$leftover_after" -eq 0 ] && [ "$RC" -eq 130 ]; then
      ok "SIGINT mid-call exits 130 and removes the work dir (it existed before the signal)"
    else bad "SIGINT cleanup" "rc=$RC dirs before=$leftover_before after=$leftover_after"; fi
  fi
fi

# 62. a posting step that ran reviewdog more than once (rso_run_retrying in
#     reviewdog-step-outputs.sh) appends every attempt's stderr to one log. An
#     earlier attempt's failed post names run 601; the step was then cancelled
#     mid-attempt (rc unknown after a start), and that LATER attempt's run 602
#     is named by no line. Both are completed: the named id FIRST (before any
#     listing, so the listing's wait cannot spend the sleep budget its re-read
#     may need), then the listing as well, which exact-only mode never made.
#     Each run's summary says how that run was found.
fresh union-unknown
printf '%s\n' "reviewdog: failed to post result: PATCH https://api.github.com/repos/o/r/check-runs/601: 404 Not Found []" \
  >"$STUB_DIR/reviewdog.err"
respond GET "$(list_path 1)" default 200 "$(page_body 2 601,in_progress 602,in_progress)"
orphan 601
orphan 602
run STEP_RC=unknown REVIEWDOG_STDERR_LOG="$STUB_DIR/reviewdog.err"
if [ "$RC" -eq 0 ] && patched 601 && patched 602 && [ "$(npatch)" -eq 2 ] \
  && [ "$(calls "GET $(list_path 1)")" -eq 1 ] && [ ! -s "$STUB_DIR/sleeps.log" ] && ! warned \
  && before "PATCH $(run_path 601)" "GET $(list_path 1)" \
  && grep -qF "reviewdog's own error output names this run" <<<"$(summary_of 601)" \
  && grep -qF "listing '${NAME}' runs on this head that started inside this job's reviewdog window" <<<"$(summary_of 602)" \
  && grep -qF "because reviewdog started and did not return an exit status" <<<"$(summary_of 602)"; then
  ok "a named id plus rc unknown: the listing runs too, and the run no line names is completed with the named one"
else bad "union, rc unknown" "rc=$RC patches=$(npatch) lists=$(calls "GET $(list_path 1)")"; fi

# 63. the same, where the later attempt's CREATE failed (its line names no run)
#     after GitHub had made run 612 anyway, and the step then ended rc 1.
fresh union-create
printf '%s\n' "reviewdog: failed to post result: PATCH https://api.github.com/repos/o/r/check-runs/611: 404 Not Found []" \
  "reviewdog: failed to create check: POST https://api.github.com/repos/o/r/check-runs: 502 Bad Gateway []" \
  >"$STUB_DIR/reviewdog.err"
respond GET "$(list_path 1)" default 200 "$(page_body 2 611,in_progress 612,in_progress)"
orphan 611
orphan 612
run REVIEWDOG_STDERR_LOG="$STUB_DIR/reviewdog.err"
if [ "$RC" -eq 0 ] && patched 611 && patched 612 && [ "$(npatch)" -eq 2 ] \
  && [ "$(calls "GET $(list_path 1)")" -eq 1 ] \
  && printf '%s\n' "$OUT" | grep -qF "completing those, and listing as well, because reviewdog's stderr also reports a failed check create"; then
  ok "a named id plus a failed create: the listing runs too, and the unnamed run is completed"
else bad "union, failed create" "rc=$RC patches=$(npatch) lists=$(calls "GET $(list_path 1)")"; fi

# 64. a signal's rc (143) after a named id: the listing runs; the run it shows
#     beside the named one that is already COMPLETED (the earlier attempt's
#     successful run, say) is left alone.
fresh union-signal
printf '%s\n' "reviewdog: failed to post result: PATCH https://api.github.com/repos/o/r/check-runs/621: 404 Not Found []" \
  >"$STUB_DIR/reviewdog.err"
respond GET "$(list_path 1)" default 200 "$(page_body 3 621,in_progress 622,completed 623,in_progress)"
orphan 621
orphan 623
run STEP_RC=143 REVIEWDOG_STDERR_LOG="$STUB_DIR/reviewdog.err"
if [ "$RC" -eq 0 ] && patched 621 && patched 623 && ! patched 622 && [ "$(npatch)" -eq 2 ]; then
  ok "a named id plus rc 143: the listing runs, its open run is completed and a completed one is not"
else bad "union, rc 143" "rc=$RC patches=$(npatch)"; fi

# 65. a named id plus rc unknown, and the listing shows only the named run:
#     the wait for a run beyond it is spent on the shared budget, and then the
#     named run is still completed (no give-up).
fresh union-nothing-more
printf '%s\n' "reviewdog: failed to post result: PATCH https://api.github.com/repos/o/r/check-runs/631: 404 Not Found []" \
  >"$STUB_DIR/reviewdog.err"
respond GET "$(list_path 1)" default 200 "$(page_body 1 631,in_progress)"
orphan 631
run STEP_RC=unknown REVIEWDOG_STDERR_LOG="$STUB_DIR/reviewdog.err"
if [ "$RC" -eq 0 ] && patched 631 && [ "$(npatch)" -eq 1 ] && [ "$(sleeps)" = "2 5 10" ] \
  && [ "$(calls "GET $(list_path 1)")" -eq 4 ] && ! warned \
  && printf '%s\n' "$OUT" | grep -qF "no '${NAME}' run beyond the named one(s) (631) became visible" \
  && ! printf '%s\n' "$OUT" | grep -qF 'are already completed and are left as they are'; then
  ok "a named id plus rc unknown with nothing more listed: the wait is spent, then the named run is completed"
else bad "union, nothing more" "rc=$RC sleeps='$(sleeps)' lists=$(calls "GET $(list_path 1)")"; fi

# 66. a named id plus rc unknown, and the listing FAILS (a 422, not retried):
#     a warning, and the named run is still completed.
fresh union-listfail
printf '%s\n' "reviewdog: failed to post result: PATCH https://api.github.com/repos/o/r/check-runs/641: 404 Not Found []" \
  >"$STUB_DIR/reviewdog.err"
respond GET "$(list_path 1)" default 422 '{"message":"Unprocessable"}'
orphan 641
run STEP_RC=unknown REVIEWDOG_STDERR_LOG="$STUB_DIR/reviewdog.err"
if [ "$RC" -eq 0 ] && patched 641 && [ "$(npatch)" -eq 1 ] \
  && warned "listing check runs '${NAME}' on ${SHA} failed at page 1" \
  && printf '%s\n' "$OUT" | grep -qF "Only the run(s) reviewdog's stderr names (641) were handled, above."; then
  ok "a named id plus rc unknown with a failing listing: a warning, and the named run is still completed"
else bad "union, list fails" "rc=$RC patches=$(npatch)"; fi

# 67. a named id plus rc unknown, and the listing shows the named run beside
#     an UNNAMED run that is already completed: the wait counts that one run
#     (not the named one) as completed and leaves it alone.
fresh union-count
printf '%s\n' "reviewdog: failed to post result: PATCH https://api.github.com/repos/o/r/check-runs/651: 404 Not Found []" \
  >"$STUB_DIR/reviewdog.err"
respond GET "$(list_path 1)" default 200 "$(page_body 2 651,in_progress 652,completed)"
orphan 651
run STEP_RC=unknown REVIEWDOG_STDERR_LOG="$STUB_DIR/reviewdog.err"
if [ "$RC" -eq 0 ] && patched 651 && ! patched 652 && [ "$(npatch)" -eq 1 ] \
  && printf '%s\n' "$OUT" | grep -qxF "1 '${NAME}' run(s) on ${SHA} are already completed and are left as they are"; then
  ok "a named id plus an unnamed completed run: the count names that one run, and it is left alone"
else bad "union, completed count" "rc=$RC patches=$(npatch)"; fi

if [ "$fail" -ne 0 ]; then
  echo "complete-orphaned-check-runs tests: FAILED"
  exit 1
fi
echo "complete-orphaned-check-runs tests: all passed"
