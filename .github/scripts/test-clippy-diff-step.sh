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
# says. Why: reviewdog v0.21.2 exits 1 for every error -- a diff finding, a
# check run GitHub answered `404 Not Found` on its PATCH (job 135103311891 of
# run 44389597385, qontinui-runner#3025; coord finding
# 2f3d3b96-7bd6-420e-aff3-f20d29544b74), a failed PR diff fetch, an unparseable
# PR diff -- and before this step classified them, each of the others reded
# this advisory job exactly as a finding does. The diff-fetch and diff-parse
# stub lines are the stderr the pinned v0.21.2 binary printed (plan
# 2026-11-22-reviewdog-diff-and-parse-infra-exits-read-as-lint-findings,
# Phase 1 step 1). Cases:
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
#   4. a clean run: the step exits 0 after one run;
#   5. a failed diff fetch, then success: the step exits 0 after two runs,
#      each from the whole report;
#   6. a diff fetch that fails on every attempt: the step exits 1 after three
#      runs, with a not-a-verdict annotation whose message names diff-fetch;
#   7. a diff that does not parse: the step exits 1 after ONE run (never
#      retried), with a not-a-verdict annotation saying a re-run will not help;
#   8. a findings line in other words (`reviewdog: 3 problems found`): still a
#      finding -- exit 1 after one run, no not-a-verdict annotation;
#   9. the step feeds reviewdog cargo's `--message-format=short` stderr, the
#      `%f:%l:%c: %m` shape `-f=clippy` parses, not the JSON stream (which
#      parses to zero diagnostics);
#   10. cargo runs from src-tauri and reviewdog from the git root, where
#      cargo's workspace-relative paths match the PR diff;
#   11. cargo dies before linting (exit 101, no workspace diagnostic line --
#      only absolute and ../ positions outside the workspace):
#      reviewdog is NOT run, the step exits non-zero with rc
#      not-run-clippy-failed, and a comma-free `(not a verdict on this PR)`
#      title says clippy did not lint -- an empty capture would otherwise
#      read as a clean diff; the completion step, fed those outputs, reads
#      it as "reviewdog never ran" and makes no API call;
#   12. cargo exits non-zero WITH diagnostic lines (deny lints): reviewdog
#      runs as normal from the whole report;
#   13. an error in a crates/* workspace member (cargo stops before
#      src-tauri): a verdict on the PR's code, so reviewdog runs on it and no
#      not-a-verdict annotation appears;
#   14. cargo killed by a signal (exit 137) after printing lines: a partial
#      capture, so reviewdog is not run and the step says clippy did not lint.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKFLOW="${HERE}/../workflows/clippy-tiers.yml"
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
fail=0
unset RSO_RETRY_DELAYS RSO_RETRY_SLEEP
NO_VERDICT='::error title=Clippy diff-scoped (advisory): reviewdog could not deliver a verdict (not a verdict on this PR)::'
NO_LINT='::error title=Clippy diff-scoped (advisory): clippy did not lint (not a verdict on this PR)::'

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
# `cargo clippy`: records its arguments, then a fixed report -- short-format
# lines on STDERR for `--message-format=short` (where cargo writes them), a
# JSON stream on stdout for `--message-format=json`. Paths are relative to the
# WORKSPACE root (the repo root, whose Cargo.toml lists src-tauri as a member),
# which is what real cargo prints even when run from src-tauri.
# CARGO_MODE (set per case): unset or `ok` exits 0; `buildfail` writes what a
# build.rs failure writes -- no workspace `<relative path>:L:C: ` line, only
# out-of-workspace (absolute and ../) positions -- and exits 101; `deny`
# writes the findings and exits 101, as a deny lint does; `memberdeny` writes
# an error in a crates/* member src-tauri path-depends on and exits 101 (cargo
# stops before src-tauri); `killed` writes the findings and exits 137 (SIGKILL).
echo "$*" >>"$CARGO_ARGS"
[ -n "${CARGO_CWD:-}" ] && pwd >"$CARGO_CWD"
if [ "${CARGO_MODE:-ok}" = memberdeny ]; then
  echo 'crates/runner-stats/src/lib.rs:3:5: error: this comparison involving the minimum or maximum element for this type contains a case that is always true or always false' >&2
  exit 101
fi
if [ "${CARGO_MODE:-ok}" = buildfail ]; then
  {
    echo '/home/runner/.cargo/registry/src/index.crates.io-0/somedep-1.0.0/src/lib.rs:4:2: error: cannot find type `X` in this scope'
    echo '../qontinui-schemas/rust/src/lib.rs:9:1: error: unresolved import `y`'
    echo 'error: failed to run custom build command for `qontinui-runner v0.1.0 (/w/src-tauri)`'
    echo ''
    echo 'Caused by:'
    echo "  process didn't exit successfully: \`/w/target/debug/build/qontinui-runner-0/build-script-build\` (exit status: 101)"
    echo '  --- stderr'
    echo "  thread 'main' panicked at build.rs:12:5:"
    echo '  frontend dist not found'
  } >&2
  exit 101
fi
case " $* " in
  *" --message-format=short "*)
    for n in $(seq 1 50); do
      printf 'src-tauri/src/lib.rs:%s:5: warning: this looks like a stub finding\n' "$n" >&2
    done
    ;;
  *) printf '{"reason":"compiler-message","n":%s}\n' $(seq 1 50) ;;
esac
[ "${CARGO_MODE:-ok}" = deny ] && exit 101
[ "${CARGO_MODE:-ok}" = killed ] && exit 137
exit 0
STUB
cat >"$W/stub/reviewdog" <<'STUB'
#!/usr/bin/env bash
n=$(( $(cat "$RD_STATE.n" 2>/dev/null || echo 0) + 1 ))
echo "$n" >"$RD_STATE.n"
echo "$*" >"$RD_STATE.args.$n"
pwd >"$RD_STATE.cwd.$n"
cat >"$RD_STATE.stdin.$n"
what="$(sed -n "${n}p" "$RD_PLAN")"
# What v0.21.2 prints on every run with -fail-on-error (job 135103311891).
echo 'time=2026-11-18T21:33:24.874Z level=WARN msg="reviewdog: -fail-on-error is deprecated. Use -fail-level=any, or -fail-level=error for github-[pr-]check reporter instead. See also https://github.com/reviewdog/reviewdog/blob/master/CHANGELOG.md"' >&2
case "$what" in
  ok) exit 0 ;;
  findings) echo "reviewdog: found at least one issue with severity greater than or equal to the given level: error" >&2; exit 1 ;;
  post\ *) echo "reviewdog: failed to post result: PATCH https://api.github.com/repos/o/r/check-runs/${what#post }: 404 Not Found []" >&2; exit 1 ;;
  # Captured from the pinned v0.21.2 binary (plan Phase 1 step 1).
  difffetch) echo "reviewdog: fail to get diff: exit status 1" >&2; exit 1 ;;
  diffparse) echo "reviewdog: fail to parse diff: invalid hunk range: @@ -x,y +z @@" >&2; exit 1 ;;
  # A findings exit in words the classifier has never seen.
  reworded) echo "reviewdog: 3 problems found" >&2; exit 1 ;;
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
# The report reviewdog must receive: exactly what cargo's short format writes.
CARGO_ARGS=/dev/null "$W/stub/cargo" clippy -q --message-format=short 2>"$W/report.short" >/dev/null

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
  : >"$C/cargo.args"
  GHO="$C/gho"
  : >"$GHO"
  OUT="$(cd "$C/ws" && env PATH="$W/stub:$PATH" GITHUB_OUTPUT="$GHO" RUNNER_TEMP="$C/rt" \
    CARGO_MODE="${CARGO_MODE:-ok}" CARGO_ARGS="$C/cargo.args" CARGO_CWD="$C/cargo.cwd" RD_PLAN="$C/plan" RD_STATE="$C/rd" RSO_RETRY_SLEEP="$W/stub/fake-sleep" SLEEP_LOG="$C/sleeps" \
    bash --noprofile --norc -eo pipefail "$W/step.sh" 2>&1)"
  RC=$?
}
val() { sed -n "s/^$1=//p" "$GHO"; }
attempts() { cat "$C/rd.n" 2>/dev/null || echo 0; }
sleeps() { tr '\n' ' ' <"$C/sleeps" | sed 's/ $//'; }
whole_report() {
  local k
  for k in $(seq 1 "$1"); do
    cmp -s "$C/rd.stdin.$k" "$W/report.short" || return 1
    grep -q -- '-name=clippy-diff' "$C/rd.args.$k" || return 1
  done
}
no_verdict() { printf '%s\n' "$OUT" | grep -qF "$NO_VERDICT"; }
# the not-a-verdict annotation's message (everything after its title)
nv_message() { printf '%s\n' "$OUT" | grep -F "$NO_VERDICT" | head -n 1 | sed 's/^.*(not a verdict on this PR):://'; }
# any ::error carrying the marker, whatever its title
any_no_verdict() { printf '%s\n' "$OUT" | grep '^::error' | grep -qF '(not a verdict on this PR)'; }
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
  && no_verdict && verdict_title_ok && [ "$(grep -oE 'check-runs/[0-9]+' "$(val stderr_log)" | sort -u | wc -l | tr -d ' ')" -eq 3 ] \
  && [[ "$(nv_message)" == *"class post after 3 attempt(s), last exit 1"* ]] && [[ "$(nv_message)" == *"re-run this job"* ]]; then
  ok "a post that keeps failing: the step exits 1 after 3 runs and declares the red not a verdict on this PR"
else bad "persistent post failure" "rc=$RC attempts=$(attempts) sleeps='$(sleeps)'"; fi

# 4. a clean run.
run_step clean ok
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 0 ] && [ "$(attempts)" -eq 1 ] && [ ! -s "$C/sleeps" ] && ! no_verdict; then
  ok "a clean run: the step exits 0 after one run"
else bad "clean" "rc=$RC attempts=$(attempts)"; fi

# 5. a failed diff fetch, then success: retried, green, from the whole report.
run_step difffetch-ok difffetch ok
if [ "$RC" -eq 0 ] && [ "$(val rc)" = 0 ] && [ "$(attempts)" -eq 2 ] && whole_report 2 \
  && [ "$(sleeps)" = 5 ] && ! any_no_verdict; then
  ok "a failed diff fetch then success: the step exits 0 after two runs from the same report"
else bad "diff fetch then ok" "rc=$RC attempts=$(attempts) sleeps='$(sleeps)'"; fi

# 6. a diff fetch that fails on every attempt: red, and says it is not a verdict.
run_step difffetch-persist difffetch difffetch difffetch
if [ "$RC" -eq 1 ] && [ "$(val rc)" = 1 ] && [ "$(attempts)" -eq 3 ] && whole_report 3 \
  && [ "$(sleeps)" = "5 10" ] && no_verdict && verdict_title_ok \
  && [[ "$(nv_message)" == *"class diff-fetch after 3 attempt(s), last exit 1"* ]] \
  && [[ "$(nv_message)" == *"re-run this job"* ]]; then
  ok "a diff fetch that keeps failing: the step exits 1 after 3 runs and declares the red not a verdict, naming diff-fetch"
else bad "persistent diff fetch failure" "rc=$RC attempts=$(attempts) sleeps='$(sleeps)' message='$(nv_message)'"; fi

# 7. a diff that does not parse: never retried, red, not a verdict, and a
#    re-run will not help.
run_step diffparse diffparse
if [ "$RC" -eq 1 ] && [ "$(val rc)" = 1 ] && [ "$(attempts)" -eq 1 ] && [ ! -s "$C/sleeps" ] \
  && no_verdict && verdict_title_ok \
  && [[ "$(nv_message)" == *"class diff-parse after 1 attempt(s), last exit 1"* ]] \
  && [[ "$(nv_message)" == *"A re-run will not help"* ]] && [[ "$(nv_message)" != *"re-run this job"* ]]; then
  ok "a diff that does not parse: the step exits 1 after one run, not retried, declares the red not a verdict and says a re-run will not help"
else bad "diff parse failure" "rc=$RC attempts=$(attempts) sleeps='$(sleeps)' message='$(nv_message)'"; fi

# 8. a findings exit in other words: still the verdict.
run_step reworded reworded
if [ "$RC" -eq 1 ] && [ "$(val rc)" = 1 ] && [ "$(attempts)" -eq 1 ] && [ ! -s "$C/sleeps" ] && ! any_no_verdict; then
  ok "a reworded findings line: the step exits 1 after one run, not retried, no not-a-verdict annotation"
else bad "reworded findings" "rc=$RC attempts=$(attempts) sleeps='$(sleeps)'"; fi

# 9. the step feeds reviewdog the short format, not cargo's JSON stream.
run_step short-feed ok
if grep -q -- '--message-format=short' "$W/step.sh" && ! grep -q -- '--message-format=json' "$W/step.sh" \
  && grep -q '^clippy .*--message-format=short' "$C/cargo.args" && [ "$(wc -l <"$C/cargo.args" | tr -d ' ')" -eq 1 ] \
  && whole_report 1 && grep -qE '^src-tauri/src/lib\.rs:[0-9]+:[0-9]+: warning: ' "$C/rd.stdin.1"; then
  ok "the step runs clippy once with --message-format=short and feeds reviewdog that stderr capture"
else bad "short-format feed" "cargo args='$(tr '\n' '|' <"$C/cargo.args")' stdin bytes=$(wc -c <"$C/rd.stdin.1" 2>/dev/null)"; fi

# 10. reviewdog runs at the git root. cargo prints paths relative to the
# WORKSPACE root (the repo root here, so `src-tauri/src/...`) even when run
# from src-tauri, and reviewdog's github-pr-check reporter prepends its own
# git-relative workdir to every parsed path: run from src-tauri, every path
# would become src-tauri/src-tauri/... and -filter-mode=diff_context would drop
# every finding. cargo itself still runs from src-tauri (it picks the package).
run_step git-root-cwd ok
if [ "$(cat "$C/rd.cwd.1" 2>/dev/null)" = "$(cd "$C/ws" && pwd)" ] \
  && [ "$(cat "$C/cargo.cwd" 2>/dev/null)" = "$(cd "$C/ws/src-tauri" && pwd)" ]; then
  ok "cargo runs from src-tauri and reviewdog from the git root, where cargo's workspace-relative paths match the PR diff"
else bad "reviewdog cwd" "reviewdog cwd='$(cat "$C/rd.cwd.1" 2>/dev/null)' cargo cwd='$(cat "$C/cargo.cwd" 2>/dev/null)' ws='$C/ws'"; fi

# 11. cargo dies before linting src-tauri: no diagnostic line, exit 101.
#     reviewdog would read that capture as zero findings and exit 0 -- green
#     for a diff nothing inspected -- so it must not run at all.
CARGO_MODE=buildfail run_step clippy-buildfail ok
nl_line="$(printf '%s\n' "$OUT" | grep -F "$NO_LINT" | head -n 1)"
nl_title="${nl_line#::error title=}"
nl_title="${nl_title%%::*}"
nl_msg="${nl_line#"$NO_LINT"}"
if [ "$RC" -ne 0 ] && [ "$(attempts)" -eq 0 ] && [ "$(val rc)" = not-run-clippy-failed ] \
  && [ -z "$(val started_at)" ] && [ -n "$nl_line" ] && [[ "$nl_title" != *,* ]] \
  && [[ "$nl_title" == *"(not a verdict on this PR)"* ]] \
  && [ "$(printf '%s\n' "$OUT" | grep -cF "$NO_LINT")" -eq 1 ] && ! no_verdict \
  && [[ "$nl_msg" == *"exited 101"* ]] && [[ "$nl_msg" == *"job's log"* ]] && [[ "$nl_msg" != *%* ]]; then
  ok "cargo dies before linting: reviewdog is not run, rc=not-run-clippy-failed, the step exits non-zero and says clippy did not lint (not a verdict)"
else bad "clippy build failure" "rc=$RC step rc='$(val rc)' attempts=$(attempts) message='${nl_msg}'"; fi

# 11b. ...and the completion step, fed those outputs, treats it as
#      "reviewdog never ran": no API call at all.
: >"$W/gh.log"
COUT="$(env PATH="$W/gh:$W/stub:$PATH" GH_LOG="$W/gh.log" GH_TOKEN=stub RUNNER_TEMP="$C/rt" \
  COMPLETE_ORPHANS_SLEEP="$W/stub/fake-sleep" SLEEP_LOG="$C/completer-sleeps" \
  STEP_RC="$(val rc)" STEP_OUTCOME=failure REVIEWDOG_STARTED_AT="$(val started_at)" \
  REVIEWDOG_ENDED_AT="$(val ended_at)" REVIEWDOG_CLOCK="$(val clock)" REVIEWDOG_STDERR_LOG="$(val stderr_log)" \
  bash "${HERE}/complete-orphaned-check-runs.sh" o/r abc1234 clippy-diff \
  "the reviewdog step ended with outcome 'failure'" 2>&1)"
CRC=$?
if [ "$CRC" -eq 0 ] && [ ! -s "$W/gh.log" ] && [[ "$COUT" == *"reviewdog never ran (not-run-clippy-failed)"* ]]; then
  ok "the completion step reads rc=not-run-clippy-failed as reviewdog never ran and makes no API call"
else
  OUT="$COUT" GHO=/dev/null
  bad "completion after a clippy build failure" "rc=$CRC calls=$(tr '\n' '|' <"$W/gh.log")"
fi

# 12. a deny lint: cargo exits non-zero WITH diagnostic lines. That is a lint
#     result, so reviewdog runs as before, from the whole report.
CARGO_MODE=deny run_step clippy-deny findings
if [ "$RC" -eq 1 ] && [ "$(val rc)" = 1 ] && [ "$(attempts)" -eq 1 ] && whole_report 1 \
  && ! any_no_verdict; then
  ok "cargo exits non-zero with diagnostics (deny lints): reviewdog runs as normal and its finding is the verdict"
else bad "clippy deny lints" "rc=$RC step rc='$(val rc)' attempts=$(attempts)"; fi

# 13. an error in a crates/* workspace member (cargo stops before src-tauri):
#     still the PR's code, so a verdict -- reviewdog runs, no not-a-verdict.
CARGO_MODE=memberdeny run_step clippy-memberdeny findings
if [ "$RC" -eq 1 ] && [ "$(val rc)" = 1 ] && [ "$(attempts)" -eq 1 ] \
  && grep -q '^crates/runner-stats/src/lib\.rs:3:5: error: ' "$C/rd.stdin.1" \
  && ! any_no_verdict && ! printf '%s\n' "$OUT" | grep -qF "$NO_LINT"; then
  ok "an error in a crates/* member is a verdict: reviewdog runs on it and no not-a-verdict annotation appears"
else bad "member crate error" "rc=$RC step rc='$(val rc)' attempts=$(attempts)"; fi

# 14. cargo killed by a signal (exit 137) after printing some lines: a partial
#     capture, so not a verdict even though workspace lines are present.
CARGO_MODE=killed run_step clippy-killed ok
if [ "$RC" -ne 0 ] && [ "$(attempts)" -eq 0 ] && [ "$(val rc)" = not-run-clippy-failed ] \
  && printf '%s\n' "$OUT" | grep -F "$NO_LINT" | grep -qF 'exited 137'; then
  ok "cargo killed by a signal: reviewdog is not run and the step says clippy did not lint"
else bad "clippy killed" "rc=$RC step rc='$(val rc)' attempts=$(attempts)"; fi

if [ "$fail" -ne 0 ]; then
  echo "clippy-diff step tests: FAILED"
  exit 1
fi
echo "clippy-diff step tests: all passed"
