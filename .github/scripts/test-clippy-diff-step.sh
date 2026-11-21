#!/usr/bin/env bash
# Hermetic test of the `clippy` step ("Clippy diff-scoped via reviewdog") of
# .github/workflows/clippy-tiers.yml, job `clippy-diff`. This repo's own file:
# it is NOT vendored from qontinui-coord and NOT in
# complete-orphaned-check-runs.sha256.
#
# It extracts that step's `run:` block from the workflow and runs it the way
# GitHub runs `shell: bash` (`bash --noprofile --norc -eo pipefail`), in a
# scratch workspace holding this repo's .github/scripts/reviewdog-step-outputs.sh,
# with stub `cargo`, `reviewdog` and `curl` first on PATH and the retry sleep
# injected. The stub reviewdog does, on its Nth run, what line N of a plan
# says. Why: reviewdog exits 1 both for a diff finding and when GitHub answers
# the PATCH of its check run `404 Not Found` (job 135103311891 of run
# 44389597385, qontinui-runner#3025; coord finding
# 2f3d3b96-7bd6-420e-aff3-f20d29544b74), and before this step retried such a
# post the second reded this advisory job exactly as the first does. Cases:
#   1. a failed post, then success: the step exits 0, from the same report;
#      its outputs name the first attempt's check run, and the real
#      complete-orphaned-check-runs.sh, fed those outputs, completes that run;
#   2. a diff finding: the step exits 1 after ONE run, with no not-a-verdict
#      annotation;
#   2b. a failed post, then a diff finding: the finding is the verdict -- the
#      step exits 1 after two runs, with no not-a-verdict annotation;
#   3. a post that fails on every attempt: the step exits 1 and says, with the
#      `(not a verdict on this PR)` phrase coord's repair classifier matches,
#      that the red is not the diff's, in an annotation title holding no comma
#      (the workflow-command parser ends a property at a comma);
#   4. a clean run: the step exits 0 after one run.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKFLOW="${HERE}/../workflows/clippy-tiers.yml"
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
fail=0
unset RSO_RETRY_DELAYS RSO_RETRY_SLEEP
NO_VERDICT='::error title=Clippy diff-scoped (advisory): reviewdog could not post its check run (not a verdict on this PR)::'

for f in "$WORKFLOW" "${HERE}/reviewdog-step-outputs.sh" "${HERE}/complete-orphaned-check-runs.sh"; do
  if [ ! -f "$f" ]; then
    echo "FAIL - ${f} does not exist"
    exit 1
  fi
done

# The `run:` block of the step whose id is `clippy`, dedented.
if ! python3 - "$WORKFLOW" "$W/step.sh" <<'PY'; then
import re
import sys

lines = open(sys.argv[1], encoding="utf-8").read().splitlines()
ids = [i for i, l in enumerate(lines) if re.fullmatch(r"\s*id:\s*clippy\s*", l)]
if len(ids) != 1:
    sys.exit(f"expected one step with `id: clippy`, found {len(ids)}")
start = next(j for j in range(ids[0], -1, -1) if re.match(r"\s*- name:", lines[j]))
indent = len(lines[start]) - len(lines[start].lstrip())
end = next((j for j in range(start + 1, len(lines))
            if lines[j].strip() and len(lines[j]) - len(lines[j].lstrip()) <= indent), len(lines))
runs = [j for j in range(start, end) if re.fullmatch(r"\s*run:\s*\|\s*", lines[j])]
if len(runs) != 1:
    sys.exit(f"expected one `run: |` in the clippy step, found {len(runs)}")
r = runs[0]
r_indent = len(lines[r]) - len(lines[r].lstrip())
body = []
for j in range(r + 1, end):
    l = lines[j]
    if l.strip() and len(l) - len(l.lstrip()) <= r_indent:
        break
    body.append(l)
nonblank = [l for l in body if l.strip()]
if not nonblank:
    sys.exit("the clippy step's run block is empty")
cut = min(len(l) - len(l.lstrip()) for l in nonblank)
open(sys.argv[2], "w", encoding="utf-8").write("\n".join(l[cut:] for l in body) + "\n")
PY
  echo "FAIL - could not extract the clippy step's run block from ${WORKFLOW}"
  exit 1
fi

mkdir -p "$W/stub"
cat >"$W/stub/cargo" <<'STUB'
#!/usr/bin/env bash
# `cargo clippy --message-format=json`: a fixed report on stdout.
printf '{"reason":"compiler-message","n":%s}\n' $(seq 1 50)
STUB
cat >"$W/stub/reviewdog" <<'STUB'
#!/usr/bin/env bash
n=$(( $(cat "$RD_STATE.n" 2>/dev/null || echo 0) + 1 ))
echo "$n" >"$RD_STATE.n"
echo "$*" >"$RD_STATE.args.$n"
wc -c | tr -d ' ' >"$RD_STATE.stdin.$n"
what="$(sed -n "${n}p" "$RD_PLAN")"
# What v0.21.2 prints on every run with -fail-on-error (job 135103311891).
echo 'time=2026-11-18T21:33:24.874Z level=WARN msg="reviewdog: -fail-on-error is deprecated. Use -fail-level=any, or -fail-level=error for github-[pr-]check reporter instead. See also https://github.com/reviewdog/reviewdog/blob/master/CHANGELOG.md"' >&2
case "$what" in
  ok) exit 0 ;;
  findings) echo "reviewdog: found at least one issue with severity greater than or equal to the given level: error" >&2; exit 1 ;;
  post\ *) echo "reviewdog: failed to post result: PATCH https://api.github.com/repos/o/r/check-runs/${what#post }: 404 Not Found []" >&2; exit 1 ;;
  *) echo "stub reviewdog: no plan line ${n}" >&2; exit 99 ;;
esac
STUB
cat >"$W/stub/curl" <<'STUB'
#!/usr/bin/env bash
printf 'HTTP/2 401\r\ndate: Mon, 16 Nov 2026 05:00:00 GMT\r\n\r\n'
STUB
cat >"$W/stub/fake-sleep" <<'STUB'
#!/usr/bin/env bash
echo "$1" >>"$SLEEP_LOG"
STUB
chmod +x "$W/stub/cargo" "$W/stub/reviewdog" "$W/stub/curl" "$W/stub/fake-sleep"
REPORT_BYTES="$("$W/stub/cargo" | wc -c | tr -d ' ')"

# run_step <case> <plan line>...: run the extracted block as the step; sets
# RC, OUT, GHO and C (the case's state prefix).
run_step() {
  local name="$1"
  shift
  C="$W/$name"
  mkdir -p "$C/ws/.github/scripts" "$C/ws/src-tauri" "$C/rt"
  cp "${HERE}/reviewdog-step-outputs.sh" "$C/ws/.github/scripts/"
  printf '%s\n' "$@" >"$C/plan"
  : >"$C/sleeps"
  GHO="$C/gho"
  : >"$GHO"
  OUT="$(cd "$C/ws" && env PATH="$W/stub:$PATH" GITHUB_OUTPUT="$GHO" RUNNER_TEMP="$C/rt" \
    RD_PLAN="$C/plan" RD_STATE="$C/rd" RSO_RETRY_SLEEP="$W/stub/fake-sleep" SLEEP_LOG="$C/sleeps" \
    bash --noprofile --norc -eo pipefail "$W/step.sh" 2>&1)"
  RC=$?
}
val() { sed -n "s/^$1=//p" "$GHO"; }
attempts() { cat "$C/rd.n" 2>/dev/null || echo 0; }
sleeps() { tr '\n' ' ' <"$C/sleeps" | sed 's/ $//'; }
whole_report() {
  local k
  for k in $(seq 1 "$1"); do
    [ "$(cat "$C/rd.stdin.$k" 2>/dev/null)" = "$REPORT_BYTES" ] || return 1
    grep -q -- '-name=clippy-diff' "$C/rd.args.$k" || return 1
  done
}
no_verdict() { printf '%s\n' "$OUT" | grep -qF "$NO_VERDICT"; }
# the not-a-verdict annotation's title= property holds the marker and no comma
verdict_title_ok() {
  local line t
  line="$(printf '%s\n' "$OUT" | grep '^::error title=' | grep -F '(not a verdict on this PR)' | head -n 1)"
  t="${line#::error title=}"
  t="${t%%::*}"
  [ -n "$line" ] && [ "$t" != "${line#::error title=}" ] && [[ "$t" != *,* ]] \
    && [[ "$t" == *"(not a verdict on this PR)"* ]]
}
ok() { echo "ok   - $1"; }
bad() {
  echo "FAIL - $1: $2"
  sed 's/^/       | /' "$GHO" 2>/dev/null
  printf '%s\n' "$OUT" | sed 's/^/       > /'
  fail=1
}

# 1. the #3025 shape: a failed post (404), then success.
run_step retry "post 135149621826" ok
log="$(val stderr_log)"
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 0 ] && [ "$(attempts)" -eq 2 ] && whole_report 2 \
  && [ "$(sleeps)" = 5 ] && [ "$(val started_at)" = 2026-11-16T05:00:00Z ] \
  && [ -n "$log" ] && grep -q 'check-runs/135149621826: 404' "$log" && ! no_verdict; then
  ok "a failed post (404) then success: the step exits 0 from the same report, and its outputs name the first run"
else bad "retry then ok" "rc=$RC attempts=$(attempts) sleeps='$(sleeps)'"; fi

# 1b. ...and the completion step, fed exactly those outputs, completes the run
#     the failed attempt left in progress although the step went green.
mkdir -p "$W/gh"
cat >"$W/gh/gh" <<'STUB'
#!/usr/bin/env bash
[ "$1" = api ] || exit 64
shift
method=GET path=""
while [ $# -gt 0 ]; do
  case "$1" in
    -X) method="$2"; shift ;;
    -H | --input) shift ;;
    *) path="$1" ;;
  esac
  shift
done
echo "$method $path" >>"$GH_LOG"
case "$method $path" in
  "GET repos/o/r/check-runs/135149621826")
    echo '{"id":135149621826,"name":"clippy-diff","status":"in_progress","head_sha":"abc1234","app":{"slug":"github-actions"}}' ;;
  "PATCH repos/o/r/check-runs/135149621826") echo '{"id":135149621826,"status":"completed"}' ;;
  *) echo "gh: stub has no response (HTTP 599)" >&2; exit 1 ;;
esac
STUB
chmod +x "$W/gh/gh"
: >"$W/gh.log"
COUT="$(env PATH="$W/gh:$W/stub:$PATH" GH_LOG="$W/gh.log" GH_TOKEN=stub RUNNER_TEMP="$C/rt" \
  COMPLETE_ORPHANS_SLEEP="$W/stub/fake-sleep" SLEEP_LOG="$C/completer-sleeps" \
  STEP_RC="$(val rc)" STEP_OUTCOME=success REVIEWDOG_STARTED_AT="$(val started_at)" \
  REVIEWDOG_ENDED_AT="$(val ended_at)" REVIEWDOG_CLOCK="$(val clock)" REVIEWDOG_STDERR_LOG="$log" \
  bash "${HERE}/complete-orphaned-check-runs.sh" o/r abc1234 clippy-diff \
  "the reviewdog step ended with outcome 'success'" 2>&1)"
if grep -qx 'PATCH repos/o/r/check-runs/135149621826' "$W/gh.log" \
  && [ "$(grep -c '^PATCH ' "$W/gh.log")" -eq 1 ]; then
  ok "the completion step, fed the green step's outputs, completes the run the failed attempt left in progress"
else
  OUT="$COUT" GHO=/dev/null
  bad "completion after a green retry" "calls=$(tr '\n' '|' <"$W/gh.log")"
fi

# 2. a diff finding: the lint verdict, red after one run, never retried.
run_step findings findings
if [ "$RC" -eq 1 ] && [ "$(val rc)" = 1 ] && [ "$(attempts)" -eq 1 ] && [ ! -s "$C/sleeps" ] && ! no_verdict; then
  ok "a diff finding: the step exits 1 after one run, not retried, no not-a-verdict annotation"
else bad "findings" "rc=$RC attempts=$(attempts)"; fi

# 2b. a failed post, then a diff finding: the finding is the verdict.
run_step post-findings "post 21" findings
if [ "$RC" -eq 1 ] && [ "$(val rc)" = 1 ] && [ "$(attempts)" -eq 2 ] && whole_report 2 \
  && [ "$(sleeps)" = 5 ] && ! no_verdict; then
  ok "a failed post then a diff finding: the step exits 1 after two runs, with no not-a-verdict annotation"
else bad "post then findings" "rc=$RC attempts=$(attempts) sleeps='$(sleeps)'"; fi

# 3. a post that fails on every attempt: red, and says it is not a verdict.
run_step persist "post 11" "post 12" "post 13"
if [ "$RC" -eq 1 ] && [ "$(val rc)" = 1 ] && [ "$(attempts)" -eq 3 ] && [ "$(sleeps)" = "5 10" ] \
  && no_verdict && verdict_title_ok && [ "$(grep -oE 'check-runs/[0-9]+' "$(val stderr_log)" | sort -u | wc -l | tr -d ' ')" -eq 3 ]; then
  ok "a post that keeps failing: the step exits 1 after 3 runs and declares the red not a verdict on this PR"
else bad "persistent post failure" "rc=$RC attempts=$(attempts) sleeps='$(sleeps)'"; fi

# 4. a clean run.
run_step clean ok
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 0 ] && [ "$(attempts)" -eq 1 ] && [ ! -s "$C/sleeps" ] && ! no_verdict; then
  ok "a clean run: the step exits 0 after one run"
else bad "clean" "rc=$RC attempts=$(attempts)"; fi

if [ "$fail" -ne 0 ]; then
  echo "clippy-diff step tests: FAILED"
  exit 1
fi
echo "clippy-diff step tests: all passed"
