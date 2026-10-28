#!/usr/bin/env bash
# Fixture tests for scripts/ci/oom-signature.sh. No network, no cargo.
#
# The emitter turns a CI red into a capacity red (coord re-runs it, or on main
# write-skips a build-phase one) instead of a code red (coord dispatches a code
# fixer). Both directions of error are expensive — a missed OOM sends a fixer
# at a diff with nothing to fix, and a false OOM hides a real red — so this
# pins the exact title on the incident log AND silence on every look-alike.
#
# Fixtures live in scripts/tests/fixtures/oom-signature/:
#   job-122032841168-build-rust-tests-tail.txt — the literal tail of the
#     incident job's `Build Rust tests` step (jobs-logs API, timestamps and
#     ANSI kept; two 18 KB rustc command lines cut at 250 characters).
#   job-122032841168-mempeak.txt — that job's MEMPEAK line. Kept to pin that
#     a pagefile reading is NOT evidence: Phase 0 measured hosted Windows at
#     32767 of 32768 MB on jobs that failed in tests, and 32551 on a green one.
#   step-script-echo.txt — GitHub's echo of a step script naming signatures.
#   (`.txt`, not `.log`: the repo's .gitignore drops every `*.log`.)
#
# Plan: 2026-10-27-coord-routes-ci-jobs-by-runner-capability, Phase 1.
#
# Run locally:
#   bash scripts/tests/test_oom_signature.sh

set -euo pipefail

tests_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$tests_dir/../.." && pwd)"
script="$repo_root/scripts/ci/oom-signature.sh"
fx="$tests_dir/fixtures/oom-signature"

bash -n "$script"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0
cases=0
assert_eq() {
  # assert_eq <name> <expected> <actual>
  cases=$((cases + 1))
  if [ "$2" = "$3" ]; then
    printf '  PASS  %s\n' "$1"
  else
    printf '  FAIL  %s\n    expected: %s\n    actual:   %s\n' "$1" "$2" "$3"
    failures=$((failures + 1))
  fi
}

# run_emitter <args...> — stdout of the emitter; a non-zero exit is a failure
# of its contract (only a usage error may exit non-zero).
run_emitter() {
  local out status=0
  out="$(bash "$script" "$@")" || status=$?
  if [ "$status" -ne 0 ]; then
    echo "<exit $status>"
    return 0
  fi
  printf '%s' "$out"
}

# THE CROSS-REPO CONTRACT. These two literals are what qontinui-coord matches:
# `pr_merge/author_feedback.rs` OUT_OF_MEMORY_ANNOTATION_MARKER
# ("OUT OF MEMORY (not a verdict on this PR)") and BUILD_KILLED_ANNOTATION_SUFFIX
# ("killed IN THE BUILD"), classified by `main_baseline_no_verdict.rs`
# `no_verdict_marker_for_title` into MARKER_BUILD_KILLED / MARKER_OUT_OF_MEMORY
# (pinned there by `title_classifier_over_the_out_of_memory_titles`). There is
# no shared source: each repo pins only its own copy, so change both together.
# Section (k) asserts the emitted titles byte-for-byte against these.
BUILD_TITLE='Build Rust tests OUT OF MEMORY (not a verdict on this PR) - killed IN THE BUILD'
RUN_TITLE='Run Rust tests OUT OF MEMORY (not a verdict on this PR)'
incident="$fx/job-122032841168-build-rust-tests-tail.txt"

echo "(a) the incident log emits the build-killed OOM title"
assert_eq "incident, build phase" \
  "::error title=$BUILD_TITLE::rustc-LLVM ERROR: out of memory: rustc-LLVM ERROR: out of memory" \
  "$(run_emitter --step 'Build Rust tests' --log "$incident" --build-phase)"

# The in-job file is written by `tee` on Windows, so it carries CRLF.
sed 's/$/\r/' "$incident" >"$work/incident-crlf.log"
assert_eq "incident with CRLF line endings" \
  "::error title=$BUILD_TITLE::rustc-LLVM ERROR: out of memory: rustc-LLVM ERROR: out of memory" \
  "$(run_emitter --step 'Build Rust tests' --log "$work/incident-crlf.log" --build-phase)"

assert_eq "incident without --build-phase carries no build-killed suffix" \
  "::error title=Build Rust tests OUT OF MEMORY (not a verdict on this PR)::rustc-LLVM ERROR: out of memory: rustc-LLVM ERROR: out of memory" \
  "$(run_emitter --step 'Build Rust tests' --log "$incident")"

echo "(b) a clean compile error emits nothing"
assert_eq "compile error (its snippet quotes os error 1455)" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$fx/compile-error.txt" --build-phase)"

# With the error-code line gone, the `$` anchor keeps it silent: the snippet
# line quotes `os error 1455` MID-line. (The snippet FILTER is pinned by (j),
# whose snippet line ENDS in the signature.)
grep -v 'error\[E0425\]' "$fx/compile-error.txt" >"$work/snippet-only.log"
assert_eq "a source snippet quoting os error 1455 mid-line, no error code" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$work/snippet-only.log" --build-phase)"

echo "(c) an empty, missing or unreadable log emits nothing"
: >"$work/empty.log"
assert_eq "empty log" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$work/empty.log" --build-phase)"
assert_eq "missing log" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$work/does-not-exist.log" --build-phase)"
assert_eq "a directory where the log should be" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$work" --build-phase)"
if [ "$(id -u)" != "0" ]; then
  cp "$incident" "$work/unreadable.log"
  chmod 000 "$work/unreadable.log"
  if [ -r "$work/unreadable.log" ]; then
    echo "  SKIP  unreadable log (this platform cannot revoke read permission)"
  else
    assert_eq "unreadable log" "" \
      "$(run_emitter --step 'Build Rust tests' --log "$work/unreadable.log" --build-phase)"
  fi
fi

echo "(d) a SIGKILL without cgroup oom_kill evidence emits nothing"
sigkill="$fx/sigkill-no-cgroup-evidence.txt"
assert_eq "SIGKILL, no memory.events" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$sigkill" --build-phase)"
assert_eq "SIGKILL, memory.events oom_kill 0" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$sigkill" --build-phase \
       --memory-events "$fx/memory-events-no-oom-kill")"
assert_eq "SIGKILL, unreadable memory.events" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$sigkill" --build-phase \
       --memory-events "$work/no-such-memory.events")"
printf 'oom_kill many\n' >"$work/memory-events-garbage"
assert_eq "SIGKILL, non-integer oom_kill" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$sigkill" --build-phase \
       --memory-events "$work/memory-events-garbage")"
# oom_kill is cumulative per cgroup, so the count must RISE over the baseline
# recorded before the step; a count alone is no evidence.
assert_eq "SIGKILL, oom_kill 2 but no baseline" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$sigkill" --build-phase \
       --memory-events "$fx/memory-events-oom-kill")"
assert_eq "SIGKILL, oom_kill 2 unchanged from baseline 2" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$sigkill" --build-phase \
       --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline 2)"
assert_eq "SIGKILL, empty baseline (non-Linux leg)" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$sigkill" --build-phase \
       --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline '')"
assert_eq "SIGKILL, non-integer baseline" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$sigkill" --build-phase \
       --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline unknown)"
assert_eq "SIGKILL WITH oom_kill increased 0->2 (positive control)" \
  "::error title=$BUILD_TITLE::SIGKILL with cgroup oom_kill 0->2: process didn't exit successfully: \`/home/runner/.rustup/toolchains/1.95.0-x86_64-unknown-linux-gnu/bin/rustc --crate-name qontinui_runner_lib --edition=2021 src/lib.rs\` (signal: 9, SIGKILL: kill)" \
  "$(run_emitter --step 'Build Rust tests' --log "$sigkill" --build-phase \
       --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline 0)"
assert_eq "SIGKILL WITH oom_kill increased 1->2" \
  "::error title=$BUILD_TITLE::SIGKILL with cgroup oom_kill 1->2: process didn't exit successfully: \`/home/runner/.rustup/toolchains/1.95.0-x86_64-unknown-linux-gnu/bin/rustc --crate-name qontinui_runner_lib --edition=2021 src/lib.rs\` (signal: 9, SIGKILL: kill)" \
  "$(run_emitter --step 'Build Rust tests' --log "$sigkill" --build-phase \
       --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline 1)"

echo "(e) quoted signatures in a failing test's captured output emit nothing"
assert_eq "failing test whose stdout quotes every signature" "" \
  "$(run_emitter --step 'Run Rust tests' --log "$fx/run-failing-test-quotes-signatures.txt")"

# With the `... FAILED` line gone, only the captured-output filter stands
# between the quoted signatures and a false claim.
grep -v '\.\.\. FAILED$' "$fx/run-failing-test-quotes-signatures.txt" >"$work/captured-only.log"
assert_eq "captured test output quoting every signature, no FAILED line" "" \
  "$(run_emitter --step 'Run Rust tests' --log "$work/captured-only.log")"

echo "(f) the other positive signatures"
assert_eq "test binary aborts on an allocation failure (run phase, no suffix)" \
  "::error title=$RUN_TITLE::memory allocation failed: memory allocation of 2097152 bytes failed" \
  "$(run_emitter --step 'Run Rust tests' --log "$fx/run-test-binary-alloc-abort.txt")"
printf '%s\n' "error: could not compile \`qontinui-runner\` (lib test)" "" "Caused by:" \
  "  process didn't exit successfully: \`rustc.exe --crate-name qontinui_runner\` (exit code: 0xc0000017, STATUS_NO_MEMORY)" \
  >"$work/status-no-memory.log"
assert_eq "STATUS_NO_MEMORY process exit" \
  "::error title=$BUILD_TITLE::STATUS_NO_MEMORY (0xc0000017): process didn't exit successfully: \`rustc.exe --crate-name qontinui_runner\` (exit code: 0xc0000017, STATUS_NO_MEMORY)" \
  "$(run_emitter --step 'Build Rust tests' --log "$work/status-no-memory.log" --build-phase)"
printf '%s\n' "error: failed to spawn rustc: The paging file is too small for this operation to complete. (os error 1455)" \
  >"$work/commit-limit.log"
assert_eq "commit limit (os error 1455)" \
  "::error title=$BUILD_TITLE::commit limit (os error 1455): error: failed to spawn rustc: The paging file is too small for this operation to complete. (os error 1455)" \
  "$(run_emitter --step 'Build Rust tests' --log "$work/commit-limit.log" --build-phase)"
printf '%s\n' "error: linking with \`link.exe\` failed: exit code: 1102" \
  "  = note: LINK : fatal error LNK1102: out of memory" >"$work/lnk1102.log"
assert_eq "LNK1102" \
  "::error title=$BUILD_TITLE::LNK1102 linker out of memory: = note: LINK : fatal error LNK1102: out of memory" \
  "$(run_emitter --step 'Build Rust tests' --log "$work/lnk1102.log" --build-phase)"

echo "(g) a pagefile reading is not evidence"
# The incident log with its explicit OOM lines removed, plus that job's own
# MEMPEAK line: there is no pagefile signature, so this is silent.
grep -v -e 'rustc-LLVM ERROR' -e 'Allocation failed' "$incident" >"$work/no-llvm-line.log"
cat "$fx/job-122032841168-mempeak.txt" >>"$work/no-llvm-line.log"
assert_eq "MEMPEAK line in the log, no OOM signature" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$work/no-llvm-line.log" --build-phase)"
assert_eq "--mempeak is not an option" "<exit 2>" \
  "$(run_emitter --step x --log "$incident" --mempeak "$fx/job-122032841168-mempeak.txt" 2>/dev/null)"

echo "(i) text that NAMES a signature without being one emits nothing"
# The emitter's own source and the workflow that calls it name every
# signature; neither may read as an OOM, even with the SIGKILL arm armed.
assert_eq "the emitter script's own source as the log" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$script" --build-phase \
       --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline 0)"
assert_eq "ci.yml as the log" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$repo_root/.github/workflows/ci.yml" \
       --build-phase --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline 0)"
assert_eq "this test file as the log" "" \
  "$(run_emitter --step 'Run Rust tests' --log "${BASH_SOURCE[0]}" \
       --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline 0)"
assert_eq "GitHub's echo of a step script, then a clean failure" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$fx/step-script-echo.txt" --build-phase \
       --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline 0)"
assert_eq "passing test names that mention OOM and SIGKILL" "" \
  "$(run_emitter --step 'Run Rust tests' --log "$fx/test-names-mention-oom.txt" \
       --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline 0)"

echo "(j) a code verdict beside real OOM evidence wins; cargo wrappers are not verdicts"
assert_eq "deny-lint \`error: unused variable\` + rustc-LLVM OOM" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$fx/deny-lint-and-llvm-oom.txt" --build-phase)"
assert_eq "syntax error + allocation abort" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$fx/syntax-error-and-oom.txt" --build-phase)"
assert_eq "error[E0308] + rustc-LLVM OOM" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$fx/oom-with-e0308.txt" --build-phase)"
assert_eq "a FAILED test + an allocation abort outside any captured output" "" \
  "$(run_emitter --step 'Run Rust tests' --log "$fx/run-failed-test-then-alloc-abort.txt")"
assert_eq "an OOM whose only error lines are cargo wrappers still emits" \
  "::error title=$BUILD_TITLE::rustc-LLVM ERROR: out of memory: rustc-LLVM ERROR: out of memory" \
  "$(run_emitter --step 'Build Rust tests' --log "$fx/oom-cargo-wrappers-only.txt" --build-phase)"
# A WARNING is not a verdict, so the guard lets it through; only the snippet
# filter stops its source line, which ENDS in `(os error 1455)`, from matching.
assert_eq "a warning's snippet line ending in (os error 1455)" "" \
  "$(run_emitter --step 'Build Rust tests' --log "$fx/warning-snippet-ends-in-os-error-1455.txt" --build-phase)"

echo "(l) CRLF logs: \$-anchored patterns still match after CR stripping"
# On Windows `tee` writes CRLF; a pattern anchored on `$` sees the CR unless it
# is stripped — a missed verdict (FAILED$) or a missed signature (1455)$.
sed 's/$/\r/' "$fx/run-failed-test-then-alloc-abort.txt" >"$work/failed-crlf.log"
assert_eq "CRLF: a FAILED test + allocation abort stays silent" "" \
  "$(run_emitter --step 'Run Rust tests' --log "$work/failed-crlf.log")"
printf '%s\r\n' "error: failed to spawn rustc: The paging file is too small for this operation to complete. (os error 1455)" \
  >"$work/commit-limit-crlf.log"
assert_eq "CRLF: commit limit (os error 1455) still emits" \
  "::error title=$BUILD_TITLE::commit limit (os error 1455): error: failed to spawn rustc: The paging file is too small for this operation to complete. (os error 1455)" \
  "$(run_emitter --step 'Build Rust tests' --log "$work/commit-limit-crlf.log" --build-phase)"

echo "(m) run phase: bash's \`Killed\` report, gated on an oom_kill increase"
killed="$fx/run-phase-bash-killed.txt"
assert_eq "bash Killed + oom_kill increased 0->2" \
  "::error title=$RUN_TITLE::Killed with cgroup oom_kill 0->2: /home/runner/work/_temp/0b5e4c1e-7d3a-4f53-9e3b-1c2d3e4f5a6b.sh: line 72: 23817 Killed                  \"\$exe\"" \
  "$(run_emitter --step 'Run Rust tests' --log "$killed" \
       --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline 0)"
assert_eq "bash Killed, oom_kill unchanged at 2" "" \
  "$(run_emitter --step 'Run Rust tests' --log "$killed" \
       --memory-events "$fx/memory-events-oom-kill" --oom-kill-baseline 2)"
assert_eq "bash Killed, no baseline" "" \
  "$(run_emitter --step 'Run Rust tests' --log "$killed" --memory-events "$fx/memory-events-oom-kill")"
assert_eq "bash Killed, no memory.events" "" \
  "$(run_emitter --step 'Run Rust tests' --log "$killed" --oom-kill-baseline 0)"

echo "(n) an allocation abort: single-threaded form, and bounded by total RAM"
assert_eq "single-threaded libtest: \`test x ... memory allocation of N bytes failed\`" \
  "::error title=$RUN_TITLE::memory allocation failed: test fleet::tests::builds_the_roster ... memory allocation of 2097152 bytes failed" \
  "$(run_emitter --step 'Run Rust tests' --log "$fx/run-single-threaded-alloc-abort.txt" --mem-total-bytes 17179869184)"
alloc_log() { printf '%s\n' "running 1 test" "memory allocation of $1 bytes failed" >"$work/alloc-$1.log"; }
alloc_log 2097152
assert_eq "N within --mem-total-bytes emits" \
  "::error title=$RUN_TITLE::memory allocation failed: memory allocation of 2097152 bytes failed" \
  "$(run_emitter --step 'Run Rust tests' --log "$work/alloc-2097152.log" --mem-total-bytes 2097152)"
assert_eq "N above --mem-total-bytes is a size bug, not capacity" "" \
  "$(run_emitter --step 'Run Rust tests' --log "$work/alloc-2097152.log" --mem-total-bytes 2097151)"
assert_eq "N above MemTotal read from meminfo" "" \
  "$(OOM_SIGNATURE_MEMINFO="$fx/meminfo-1mib.txt" run_emitter --step 'Run Rust tests' --log "$work/alloc-2097152.log")"
alloc_log 1048576
assert_eq "N within MemTotal read from meminfo" \
  "::error title=$RUN_TITLE::memory allocation failed: memory allocation of 1048576 bytes failed" \
  "$(OOM_SIGNATURE_MEMINFO="$fx/meminfo-1mib.txt" run_emitter --step 'Run Rust tests' --log "$work/alloc-1048576.log")"
# No meminfo (macOS; or no /proc): the fixed 64 GiB cap applies.
alloc_log 68719476737
assert_eq "no meminfo: N above the 64 GiB cap" "" \
  "$(OOM_SIGNATURE_MEMINFO="$work/no-meminfo" run_emitter --step 'Run Rust tests' --log "$work/alloc-68719476737.log")"
alloc_log 68719476736
assert_eq "no meminfo: N at the 64 GiB cap emits" \
  "::error title=$RUN_TITLE::memory allocation failed: memory allocation of 68719476736 bytes failed" \
  "$(OOM_SIGNATURE_MEMINFO="$work/no-meminfo" run_emitter --step 'Run Rust tests' --log "$work/alloc-68719476736.log")"
alloc_log 18446744073709551615
assert_eq "N = usize::MAX (an overflowed capacity), default total" "" \
  "$(run_emitter --step 'Run Rust tests' --log "$work/alloc-18446744073709551615.log")"

echo "(k) the cross-repo title contract, byte for byte"
# See the BUILD_TITLE / RUN_TITLE comment: coord's constants are the other half.
title_of() { sed -n 's/^::error title=\([^:]*\)::.*/\1/p'; }
assert_eq "build-phase title is coord's build-killed OOM title" \
  "Build Rust tests OUT OF MEMORY (not a verdict on this PR) - killed IN THE BUILD" \
  "$(run_emitter --step 'Build Rust tests' --log "$incident" --build-phase | title_of)"
assert_eq "run-phase title is coord's out_of_memory title" \
  "Run Rust tests OUT OF MEMORY (not a verdict on this PR)" \
  "$(run_emitter --step 'Run Rust tests' --log "$work/alloc-2097152.log" --mem-total-bytes 17179869184 | title_of)"

echo "(h) usage errors exit 2 and print nothing on stdout"
assert_eq "no arguments" "<exit 2>" "$(run_emitter 2>/dev/null)"
assert_eq "unknown flag" "<exit 2>" "$(run_emitter --step x --log "$incident" --bogus 2>/dev/null)"

echo
if [ "$failures" -ne 0 ]; then
  echo "::error::oom-signature: $failures of $cases case(s) failed"
  exit 1
fi
echo "oom-signature: all $cases cases passed"
