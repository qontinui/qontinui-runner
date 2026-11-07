#!/usr/bin/env bash
# oom-signature.sh — say, from POSITIVE evidence only, that a failed CI step
# ran out of memory, in the one annotation title coord already reads as
# "not a verdict on this PR".
#
# WHY THIS EXISTS. Job 122032841168 (`test (windows-latest)`, 2026-10-27) died
# in `Build Rust tests` with `rustc-LLVM ERROR: out of memory` — a capacity
# failure of the runner it was placed on, not a defect in the PR. Without an
# annotation coord read that red exactly like a compile error: main went red
# and the red-main path dispatched a CODE fixer at a diff that had nothing to
# fix. Coord already understands the CI declaring a red inconclusive through an
# `::error title=…(not a verdict on this PR)…` annotation
# (qontinui-coord `pr_merge/author_feedback.rs` `NOT_A_VERDICT_ANNOTATION_MARKER`,
# `main_baseline_no_verdict.rs` `no_verdict_marker_for_title`), so the fix is
# to emit that title when — and only when — the log proves an OOM. Plan
# 2026-10-27-coord-routes-ci-jobs-by-runner-capability, D4 and Phase 1.
#
# DESIGN RISK (accepted, same class as a build timeout). A PR that
# DETERMINISTICALLY runs the compiler out of memory (a pathological generic or
# macro expansion) gets the build-killed title on every attempt. On main coord
# write-skips that as `build_killed`, so such a commit would sit write-skipped
# — neither green nor a code red — indefinitely, exactly as a build that
# deterministically times out does. Coord cannot tell the two apart there (see
# qontinui-coord `main_baseline_no_verdict.rs`); the bound on it is coord's,
# not this script's.
#
# OUTPUT. On a match, exactly one line on stdout:
#   ::error title=<step> OUT OF MEMORY (not a verdict on this PR)::<signature>: <evidence>
# With --build-phase (the step only compiles, so NO test ran) the title also
# carries coord's build-killed suffix, ` - killed IN THE BUILD`, which coord
# write-skips on main as `build_killed` instead of keeping it red. Coord's
# coord-db-tests timeout title uses the same ` - killed IN THE BUILD` spelling.
#
# NOTHING IS EMITTED — and the job stays an ordinary red — when no signature
# matches, when the log is missing, unreadable or empty, or when the only
# evidence is ambiguous. A false OOM claim would turn a real red into a re-run,
# so every rule here prefers silence to a guess. Exit status is 0 in all of
# those cases; only a usage error exits non-zero (2).
#
# POSITIVE SIGNATURES, checked in this order (the first match is reported).
# Each is anchored on the SHAPE of the real error line, never a bare
# substring, because the words also occur in script text and test names:
#   1. `rustc-LLVM ERROR: out of memory`        at line start (rustc/LLVM).
#      The one signal Phase 0 confirmed on hosted runners: the incident plus
#      3 further hits.
#   2. `memory allocation of N bytes failed`     at line start (Rust's abort),
#      or after libtest's `test <name> ... ` on the same line (a single-
#      threaded test binary prints the abort where the result would go). Only
#      when N is no more than the machine's total RAM: a request larger than
#      the whole machine is a size bug (an overflowed capacity), not capacity.
#      Total RAM is --mem-total-bytes when given, else MemTotal from
#      /proc/meminfo (Linux; Git Bash on Windows emulates it), else a fixed
#      64 GiB cap — larger than any hosted runner, so the cap only ever
#      SILENCES an absurd request and never invents a match.
#   3. cargo's `process didn't exit successfully: … (exit code: 0xc0000017[,
#      STATUS_NO_MEMORY])` ending the line (a Windows process out of memory).
#   4. `(os error 1455)` at line end             (Windows commit limit).
#   5. `LINK : fatal error LNK1102: …`, optionally after cargo's `= note: `
#      (MSVC linker out of memory).
#   6. `(signal: 9, SIGKILL: kill)` at line end (cargo reporting a killed
#      rustc or test binary), and
#   7. bash's own `<script>: line N: <pid> Killed  <cmd>` — the run phase
#      execs each test binary from the step's bash script, not through cargo,
#      so an OOM-killed test binary is reported this way (the step exits 137).
#   6 and 7 count ONLY together with cgroup evidence: the `oom_kill` count in
#   --memory-events must EXCEED --oom-kill-baseline, the count the workflow
#   recorded before the step ran. `oom_kill` is cumulative over the cgroup's
#   lifetime (on a hosted runner, the runner service's), so a non-zero count
#   alone may be an earlier step's or an earlier job's. A missing, unreadable
#   or non-integer baseline or count is NO evidence. A SIGKILL alone is not
#   evidence either: job 111779867816's rustc was SIGKILLed because the runner
#   received a shutdown signal, and a timeout or a cancel delivers one too.
#
# NOT a signature: the Windows pagefile peak. Phase 0 measured hosted Windows
# at 32767 of 32768 MB in 8 of 21 readings, 5 of them jobs that failed in
# TESTS, and a GREEN job at 32551 — saturation is normal there, not OOM.
#
# A CODE VERDICT IN THE SAME LOG WINS. When the log also carries a verdict on
# the code — any rustc error diagnostic, with or without a code
# (`error[E0425]: …`, a deny-lint `error: unused variable: …`, a syntax
# `error: expected …`), or a failed test (`... FAILED` ending a line, or a
# bare `FAILED` line) — nothing
# is emitted, whatever memory evidence sits beside it. The ONLY `error: ` lines
# that are not a verdict are cargo's wrappers around a failure reported
# elsewhere: `could not compile`, `linking with`, `failed to spawn`,
# `build failed`, `test failed`, `failed to run custom build command` and
# `process didn't exit successfully` — each names WHICH process failed, never
# why, and every OOM log carries one of them. An `error: ` line that itself
# ends in `(os error 1455)` is not a verdict either: it is rustc's own report
# of the commit limit (signature 4). On main a build-phase OOM title is WRITE-SKIPPED
# (the prior verdict stands), so annotating a run that also failed on its
# merits would hide a real red; a pagefile that peaked during a genuine compile
# error is not why that compile failed.
#
# WHAT A MATCH MAY NOT COME FROM. Lines that merely QUOTE a signature are not
# evidence, so three kinds of line are dropped before matching:
#   * GitHub's echo of a step's own script (`##[group]Run …` to
#     `##[endgroup]`) in a log downloaded through the jobs-logs API — this
#     script's source and the workflow text that calls it name every signature;
#   * rustc source snippets (`188 | …`, `    | …`) — a WARNING (which is not a
#     verdict, so the guard above lets it through) that points at a source
#     line ending in `(os error 1455)` must not read as an OOM; and
#   * libtest's captured output of a failing test (from `---- <name> stdout ----`
#     to the `failures:` / `test result:` / next binary's `Running` line that
#     ends it), where a test's own assertion text about these errors is printed.
#
# INPUT is the tee'd step output (`cargo-test-build.log`,
# `cargo-test-output.log`). ANSI colour escapes, CRs and a leading GitHub log
# timestamp are stripped first, so a log downloaded through the jobs-logs API
# matches the same way the in-job file does.
#
# Usage:
#   oom-signature.sh --step <step name> --log <file> [--build-phase]
#                    [--memory-events <file> --oom-kill-baseline <count>]
#                    [--mem-total-bytes <bytes>]
#
# Tested by scripts/tests/test_oom_signature.sh (fixtures, no network).

set -uo pipefail

usage() {
  echo "usage: $0 --step <name> --log <file> [--build-phase]" \
    "[--memory-events <file> --oom-kill-baseline <count>] [--mem-total-bytes <bytes>]" >&2
  exit 2
}

step=""
log=""
build_phase=0
memory_events=""
oom_kill_baseline=""
mem_total_bytes=""
while [ $# -gt 0 ]; do
  case "$1" in
    --step) [ $# -ge 2 ] || usage; step="$2"; shift 2 ;;
    --log) [ $# -ge 2 ] || usage; log="$2"; shift 2 ;;
    --build-phase) build_phase=1; shift ;;
    --memory-events) [ $# -ge 2 ] || usage; memory_events="$2"; shift 2 ;;
    --oom-kill-baseline) [ $# -ge 2 ] || usage; oom_kill_baseline="$2"; shift 2 ;;
    --mem-total-bytes) [ $# -ge 2 ] || usage; mem_total_bytes="$2"; shift 2 ;;
    *) usage ;;
  esac
done
if [ -z "$step" ] || [ -z "$log" ]; then usage; fi

# An unreadable or empty log proves nothing: emit nothing.
if [ ! -f "$log" ] || [ ! -r "$log" ] || [ ! -s "$log" ]; then
  exit 0
fi

export LC_ALL=C
ESC=$(printf '\033')
CR=$(printf '\r')

# The log as evidence: normalised, with snippet lines and captured test output
# removed (see the header). One line per input line, so a match is a whole line.
# Written once to a temp file so every check below greps a file rather than a
# pipe (an early-exiting grep under pipefail would read as "no match").
evidence="$(mktemp 2>/dev/null)" || exit 0
trap 'rm -f "$evidence"' EXIT
sed -e "s/${ESC}\[[0-9;]*[A-Za-z]//g" \
    -e "s/${CR}//g" \
    -e 's/^[0-9]\{4\}-[0-9][0-9]-[0-9][0-9]T[0-9:.]*Z //' \
    "$log" 2>/dev/null \
  | awk '
      /^##\[group\]/ { echoed = 1; next }
      /^##\[endgroup\]/ { echoed = 0; next }
      echoed { next }
      /^---- .* std(out|err) ----$/ { captured = 1; next }
      /^failures:$/ || /^test result:/ || /^ *Running / { captured = 0 }
      captured { next }
      /^ *[0-9]* *[|]/ { next }
      { print }
    ' >"$evidence" || exit 0

# first_line <grep flags> <pattern> — the first evidence line matching.
first_line() {
  grep -m1 "$1" -- "$2" "$evidence" || true
}

# A verdict on the code in the same log: stay silent (see the header). Every
# rustc `error: ` / `error[E…]: ` line is a verdict except cargo's wrappers.
CARGO_WRAPPER='^error: (could not compile|linking with|failed to spawn|build failed|test failed|failed to run custom build command|process didn.t exit successfully)'
# One awk, not `grep | grep -q`: under pipefail an early-exiting `grep -q`
# SIGPIPEs the first grep and the pipeline would read as "no verdict".
# An `error: ` line that itself ENDS in `(os error 1455)` is rustc reporting
# the commit limit (`could not exec the linker …`, `failed to write …rlib`), so
# it is signature 4, not a verdict — but any OTHER verdict line still wins.
if awk -v w="$CARGO_WRAPPER" '/^error(\[E[0-9]+\])?: / && $0 !~ w && $0 !~ /\(os error 1455\)$/ { found = 1 }
                             END { exit !found }' "$evidence"; then
  exit 0
fi
# A failed test: libtest's `… ... FAILED` (anywhere on the line — a test's own
# output can share the line with the result) or a bare `FAILED`.
if grep -qE '\.\.\. FAILED$|^FAILED$' "$evidence"; then
  exit 0
fi

# The machine's total RAM in bytes (see signature 2 in the header).
is_uint() { case "$1" in '' | *[!0-9]*) return 1 ;; *) return 0 ;; esac; }
MEM_TOTAL_CAP=68719476736 # 64 GiB
if ! is_uint "$mem_total_bytes"; then
  mem_total_bytes="$(awk '$1 == "MemTotal:" && $2 ~ /^[0-9]+$/ { printf "%.0f", $2 * 1024; exit }' \
    "${OOM_SIGNATURE_MEMINFO:-/proc/meminfo}" 2>/dev/null)"
  is_uint "$mem_total_bytes" || mem_total_bytes=$MEM_TOTAL_CAP
fi

signature=""
line=""

match() {
  # match <signature label> <grep flags> <pattern>
  [ -n "$signature" ] && return 0
  local hit
  hit="$(first_line "$2" "$3")"
  if [ -n "$hit" ]; then
    signature="$1"
    line="$hit"
  fi
}

match "rustc-LLVM ERROR: out of memory" -E '^rustc-LLVM ERROR: out of memory'
# 2. Rust's allocation abort, bounded by the machine's RAM.
alloc_hit="$(first_line -E '^(test .* \.\.\. )?memory allocation of [0-9]+ bytes failed')"
if [ -n "$alloc_hit" ]; then
  alloc_bytes="$(printf '%s' "$alloc_hit" | sed -n 's/.*memory allocation of \([0-9]*\) bytes failed.*/\1/p')"
  # awk compares as doubles, so a 25-digit N still compares correctly.
  if awk -v n="$alloc_bytes" -v t="$mem_total_bytes" 'BEGIN { exit !(n != "" && n + 0 <= t + 0) }'; then
    match "memory allocation failed" -E '^(test .* \.\.\. )?memory allocation of [0-9]+ bytes failed'
  fi
fi
match "STATUS_NO_MEMORY (0xc0000017)" -iE \
  "^[[:space:]]*process didn't exit successfully: .*\\(exit code: 0xc0000017(, STATUS_NO_MEMORY)?\\)$"
match "commit limit (os error 1455)" -E '\(os error 1455\)$'
match "LNK1102 linker out of memory" -E '^[[:space:]]*(= note: )?LINK : fatal error LNK1102'

# 6/7. A kill, with the cgroup's own record of an OOM kill DURING the step:
# the count must have risen above the baseline recorded before it.
if [ -z "$signature" ] && [ -n "$memory_events" ] && [ -r "$memory_events" ] \
   && is_uint "$oom_kill_baseline"; then
  oom_kills="$(awk '$1 == "oom_kill" { print $2; exit }' "$memory_events" 2>/dev/null)"
  if is_uint "$oom_kills" \
     && awk -v n="$oom_kills" -v b="$oom_kill_baseline" 'BEGIN { exit !(n + 0 > b + 0) }'; then
    match "SIGKILL with cgroup oom_kill $oom_kill_baseline->$oom_kills" -E '\(signal: 9, SIGKILL: kill\)$'
    match "Killed with cgroup oom_kill $oom_kill_baseline->$oom_kills" -E \
      '^[^[:space:]].*: line [0-9]+: +[0-9]+ Killed( .*)?$'
  fi
fi

[ -n "$signature" ] || exit 0

# Workflow-command escaping: `%`, CR and LF in data; additionally `:` and `,`
# in a property value such as `title`.
escape_data() {
  local s="$1"
  s="${s//%/%25}"
  s="${s//$'\r'/%0D}"
  s="${s//$'\n'/%0A}"
  printf '%s' "$s"
}
escape_property() {
  local s
  s="$(escape_data "$1")"
  s="${s//:/%3A}"
  s="${s//,/%2C}"
  printf '%s' "$s"
}

title="$step OUT OF MEMORY (not a verdict on this PR)"
if [ "$build_phase" -eq 1 ]; then
  title="$title - killed IN THE BUILD"
fi
# Trim the evidence line and bound its length; it is context, not the claim.
line="$(printf '%s' "$line" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' | cut -c1-300)"

printf '::error title=%s::%s\n' "$(escape_property "$title")" "$(escape_data "$signature: $line")"
exit 0
