#!/usr/bin/env bash
# Fixture tests for scripts/ci/resource-report.sh (and a parse check of
# scripts/ci/resource-report-windows.ps1). No network, no cargo.
#
# The report is how coord learns each runner class's capability and each
# job's requirement (plan 2026-10-27-coord-routes-ci-jobs-by-runner-capability,
# Phase 2). Its one hard rule is ABSENT, NEVER ZERO: a fabricated 0 MB of RAM
# reads as an incapable runner, a fabricated 0 oom_kills as an all-clear. So
# every case below pins the exact JSON, including which keys are MISSING.
#
# Fixtures live in scripts/tests/fixtures/resource-report/ (`.txt`, because the
# repo's .gitignore drops every `*.log`). The cgroup tree is built per case in
# a temp dir, because the script reads `<root><path>/memory.{stat,events}` and,
# for phys_mem_mb, `memory.{high,max}` at every level up to the root.
#
# Run locally:
#   bash scripts/tests/test_resource_report.sh

set -euo pipefail

tests_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$tests_dir/../.." && pwd)"
script="$repo_root/scripts/ci/resource-report.sh"
ps1="$repo_root/scripts/ci/resource-report-windows.ps1"
fx="$tests_dir/fixtures/resource-report"

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

PREFIX='::notice title=qontinui-resource-report::'

# A cgroup tree: <dir>/proc_cgroup names /job.slice, whose memory.peak and
# memory.events are the given values/fixture. Echoes the dir.
cgroup_fixture() { # <name> <memory.peak contents> <memory.events fixture or "">
  local d="$work/cg-$1"
  mkdir -p "$d/root/job.slice"
  printf '0::/job.slice\n' >"$d/proc_cgroup"
  printf '%s\n' "$2" >"$d/root/job.slice/memory.peak"
  [ -z "$3" ] || cp "$3" "$d/root/job.slice/memory.events"
  printf '%s' "$d"
}

# Runs the report as Linux with the hosted fixtures unless overridden by the
# caller's own env assignments (env given BEFORE the function name wins).
# Prints stdout; a non-zero exit is printed as `<exit N>`.
run_linux() { # <cgroup dir or ""> <args...>
  local cg="${1:-$work/no-such-cgroup}" out status=0
  shift
  out="$(RESOURCE_REPORT_OS=linux \
    RESOURCE_REPORT_MEMINFO="${MEMINFO:-$fx/meminfo-hosted-16g.txt}" \
    RESOURCE_REPORT_OVERCOMMIT="${OVERCOMMIT:-$fx/overcommit-heuristic.txt}" \
    RESOURCE_REPORT_CGROUP_PROC="$cg/proc_cgroup" \
    RESOURCE_REPORT_CGROUP_ROOT="$cg/root" \
    RESOURCE_REPORT_DF="${DF:-$fx/df-20g-free.txt}" \
    RUNNER_ENVIRONMENT="${RUNNER_ENV:-github-hosted}" \
    bash "$script" "$@")" || status=$?
  [ "$status" -eq 0 ] || { echo "<exit $status>"; return 0; }
  printf '%s' "$out"
}

run_windows() { # <reading fixture> <args...>
  local reading="$1" out status=0
  shift
  out="$(RESOURCE_REPORT_OS=windows \
    RESOURCE_REPORT_WINDOWS_READING="$reading" \
    RESOURCE_REPORT_DF="${DF:-$fx/df-20g-free.txt}" \
    RUNNER_ENVIRONMENT="${RUNNER_ENV:-github-hosted}" \
    bash "$script" "$@")" || status=$?
  [ "$status" -eq 0 ] || { echo "<exit $status>"; return 0; }
  printf '%s' "$out"
}

GIB12=12884901888 # 12288 MiB

echo "(a) a github-hosted Linux job: every Linux quantity, exactly"
# Every cgroup fixture carries a 12 GiB `memory.peak` — the cgroup's whole
# charge, page cache included. It must NEVER surface as peak_rss_mb: the
# anon sampler (section d) is the only source of that key.
cg="$(cgroup_fixture hosted "$GIB12" "$fx/memory-events-oom-kill-0.txt")"
assert_eq "hosted linux, success" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"cgroup_oom_kills\":0,\"disk_free_min_mb\":20464,\"build_outcome\":\"success\"}" \
  "$(run_linux "$cg" --build-outcome success)"

echo "(b) the Linux commit limit is reported only under STRICT overcommit"
assert_eq "strict overcommit adds commit_limit_mb" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"commit_limit_mb\":12285,\"pagefile_alloc_mb\":4095,\"cgroup_oom_kills\":0,\"disk_free_min_mb\":20464,\"build_outcome\":\"failure\"}" \
  "$(OVERCOMMIT="$fx/overcommit-strict.txt" run_linux "$cg" --build-outcome failure)"
assert_eq "unreadable overcommit mode omits it" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"cgroup_oom_kills\":0,\"disk_free_min_mb\":20464}" \
  "$(OVERCOMMIT="$work/none" run_linux "$cg")"

echo "(c) oom_kills: the RISE over a baseline; cumulative only on github-hosted"
cg3="$(cgroup_fixture kills "$GIB12" "$fx/memory-events-oom-kill-3.txt")"
assert_eq "hosted, no baseline: cumulative 3" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"cgroup_oom_kills\":3,\"disk_free_min_mb\":20464}" \
  "$(run_linux "$cg3")"
assert_eq "hosted, baseline 2: rise 1" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"cgroup_oom_kills\":1,\"disk_free_min_mb\":20464}" \
  "$(run_linux "$cg3" --oom-kill-baseline 2)"
assert_eq "self-hosted, baseline 3: rise 0 is a real 0" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"cgroup_oom_kills\":0,\"disk_free_min_mb\":20464}" \
  "$(RUNNER_ENV=self-hosted run_linux "$cg3" --oom-kill-baseline 3)"
assert_eq "self-hosted, no baseline: cumulative is not this job's, omitted" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"disk_free_min_mb\":20464}" \
  "$(RUNNER_ENV=self-hosted run_linux "$cg3")"
assert_eq "count BELOW its baseline (a different cgroup): omitted, not negative" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"disk_free_min_mb\":20464}" \
  "$(run_linux "$cg3" --oom-kill-baseline 7)"
assert_eq "an empty baseline (unreadable before the build) on hosted: cumulative" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"cgroup_oom_kills\":3,\"disk_free_min_mb\":20464}" \
  "$(run_linux "$cg3" --oom-kill-baseline '')"

echo "(d) peak_rss_mb is ANONYMOUS memory from the sampler, never memory.peak"
MIB=1048576
# A state dir as --start-anon-sampler leaves it: <baseline MiB> <peak MiB>.
anon_state() { # <name> <baseline MiB> <peak MiB>
  local d="$work/anon-$1"
  mkdir -p "$d"
  printf '%s\n' "$(( $2 * MIB ))" >"$d/anon-baseline-bytes"
  printf '%s\n' "$(( $3 * MIB ))" >"$d/anon-peak-bytes"
  printf '%s' "$d"
}
# The cgroup's anon right now (the report's own final read).
cg_anon() { # <cgroup dir> <anon MiB>
  printf 'anon %s\nfile 40000000000\nkernel 900000000\n' "$(( $2 * MIB ))" >"$1/root/job.slice/memory.stat"
}
cg_anon "$cg" 100
assert_eq "hosted: the sampled anon peak" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"peak_rss_mb\":6000,\"cgroup_oom_kills\":0,\"disk_free_min_mb\":20464}" \
  "$(run_linux "$cg" --anon-state-dir "$(anon_state h 300 6000)")"
assert_eq "self-hosted, anon rose above the sampler's baseline: reported" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"peak_rss_mb\":6000,\"cgroup_oom_kills\":0,\"disk_free_min_mb\":20464}" \
  "$(RUNNER_ENV=self-hosted run_linux "$cg" --oom-kill-baseline 0 --anon-state-dir "$(anon_state s 300 6000)")"
assert_eq "self-hosted, anon never rose: UNKNOWN, omitted" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"cgroup_oom_kills\":0,\"disk_free_min_mb\":20464}" \
  "$(RUNNER_ENV=self-hosted run_linux "$cg" --oom-kill-baseline 0 --anon-state-dir "$(anon_state flat 300 300)")"
cg_anon "$cg" 9000
assert_eq "the report's own final read raises the peak" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"peak_rss_mb\":9000,\"cgroup_oom_kills\":0,\"disk_free_min_mb\":20464}" \
  "$(run_linux "$cg" --anon-state-dir "$(anon_state final 300 6000)")"
assert_eq "no sampler state dir: no peak, whatever memory.peak says" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"cgroup_oom_kills\":0,\"disk_free_min_mb\":20464}" \
  "$(run_linux "$cg" --anon-state-dir "$work/never-started")"
nobase="$(anon_state nobase 300 6000)"; rm -f "$nobase/anon-baseline-bytes"
assert_eq "a state dir with no baseline (sampler never started): omitted" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"cgroup_oom_kills\":0,\"disk_free_min_mb\":20464}" \
  "$(run_linux "$cg" --anon-state-dir "$nobase")"

echo "(d2) the sampler loop: start, sample, stop, and never outlive its cap"
cgl="$(cgroup_fixture loop "$GIB12" "$fx/memory-events-oom-kill-0.txt")"
cg_anon "$cgl" 500
sd="$work/anon-live"
sampler_env() {
  RESOURCE_REPORT_CGROUP_PROC="$cgl/proc_cgroup" RESOURCE_REPORT_CGROUP_ROOT="$cgl/root" "$@"
}
start_status=0
sampler_env bash "$script" --start-anon-sampler --state-dir "$sd" --interval 0.1 >/dev/null || start_status=$?
assert_eq "start: exit 0" "0" "$start_status"
assert_eq "start: baseline is the anon at start" "$(( 500 * MIB ))" "$(cat "$sd/anon-baseline-bytes" 2>/dev/null)"
loop_pid="$(cat "$sd/anon-sampler.pid" 2>/dev/null || true)"
assert_eq "start: the loop is running" "alive" \
  "$( [ -n "$loop_pid" ] && kill -0 "$loop_pid" 2>/dev/null && echo alive || echo dead)"
cg_anon "$cgl" 4000
i=0
while [ "$i" -lt 50 ] && [ "$(cat "$sd/anon-peak-bytes" 2>/dev/null)" != "$(( 4000 * MIB ))" ]; do sleep 0.1; i=$((i + 1)); done
assert_eq "loop: raises the peak as anon grows" "$(( 4000 * MIB ))" "$(cat "$sd/anon-peak-bytes" 2>/dev/null)"
cg_anon "$cgl" 700
sleep 0.4
assert_eq "loop: a fall in anon never lowers the peak" "$(( 4000 * MIB ))" "$(cat "$sd/anon-peak-bytes" 2>/dev/null)"
assert_eq "report: the peak the loop saw" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"peak_rss_mb\":4000,\"cgroup_oom_kills\":0,\"disk_free_min_mb\":20464}" \
  "$(run_linux "$cgl" --anon-state-dir "$sd")"
i=0
while [ "$i" -lt 30 ] && kill -0 "$loop_pid" 2>/dev/null; do sleep 0.1; i=$((i + 1)); done
assert_eq "report: stops the loop" "dead" "$(kill -0 "$loop_pid" 2>/dev/null && echo alive || echo dead)"
sd2="$work/anon-capped"
sampler_env env RESOURCE_REPORT_SAMPLER_MAX_SECS=1 bash "$script" --start-anon-sampler --state-dir "$sd2" --interval 0.2 >/dev/null
cap_pid="$(cat "$sd2/anon-sampler.pid" 2>/dev/null || true)"
i=0
while [ "$i" -lt 40 ] && kill -0 "$cap_pid" 2>/dev/null; do sleep 0.1; i=$((i + 1)); done
assert_eq "loop: exits on its own at the lifetime cap (no report ever ran)" "dead" \
  "$(kill -0 "$cap_pid" 2>/dev/null && echo alive || echo dead)"
start_status=0
out="$(RESOURCE_REPORT_CGROUP_PROC="$work/no-such-proc" bash "$script" --start-anon-sampler --state-dir "$work/anon-unreadable" 2>&1)" || start_status=$?
assert_eq "start with no readable cgroup: exit 0, no state, no loop" "0 absent" \
  "$start_status $([ -e "$work/anon-unreadable/anon-sampler.pid" ] && echo present || echo absent)"
# A recycled pid is someone else's: the report kills only a pid whose command
# line is still the loop.
decoy_sd="$(anon_state decoy 300 6000)"
sleep 300 &
decoy=$!
printf '%s\n' "$decoy" >"$decoy_sd/anon-sampler.pid"
run_linux "$cg" --anon-state-dir "$decoy_sd" >/dev/null
# The decoy is OUR child, so a killed one lingers as a zombie that `kill -0`
# still answers for: read its state instead (Z or gone = dead).
sleep 0.3
decoy_state="$(awk '{print $3}' "/proc/$decoy/stat" 2>/dev/null || true)"
assert_eq "report: never kills a recycled pid that is not the loop" "alive" \
  "$(case "$decoy_state" in ''|Z|X) echo dead ;; *) echo alive ;; esac)"
kill "$decoy" 2>/dev/null || true
wait "$decoy" 2>/dev/null || true

echo "(d3) build_outcome is the heavy compile+test work's, combined"
for c in "success success:success" "success failure:failure" "failure success:failure" \
         "success skipped:success" "skipped skipped:skipped" "cancelled success:cancelled" \
         "failure cancelled:failure" ":<none>" "bogus skipped:skipped" \
         "success success failure:failure" "failure skipped skipped:failure" \
         "success success cancelled:cancelled" "success success skipped:success"; do
  ins="${c%%:*}"; want="${c##*:}"
  args=()
  for o in $ins; do args+=(--build-outcome "$o"); done
  [ -n "$ins" ] || args=(--build-outcome "")
  got="$(run_linux "" "${args[@]}")"
  case "$got" in
    *'"build_outcome":"'*) got="${got##*\"build_outcome\":\"}"; got="${got%%\"*}" ;;
    *) got="<none>" ;;
  esac
  assert_eq "outcomes [${ins:-empty}] -> $want" "$want" "$got"
done

echo "(e) unreadable and malformed inputs are ABSENT, never zero"
assert_eq "nothing readable: os only" \
  "${PREFIX}{\"os\":\"linux\"}" \
  "$(MEMINFO="$work/none" DF="$work/none" run_linux "")"
cgmax="$(cgroup_fixture max "max" "")"
printf 'MemTotal:       abc kB\nSwapTotal:   kB\n' >"$work/meminfo-junk.txt"
printf 'Filesystem 1048576-blocks Used Available Capacity Mounted on\n/dev/root 74244 53764 - 73%% /\n' >"$work/df-junk.txt"
assert_eq "junk meminfo, peak 'max', no memory.events, junk df: os only" \
  "${PREFIX}{\"os\":\"linux\"}" \
  "$(MEMINFO="$work/meminfo-junk.txt" DF="$work/df-junk.txt" run_linux "$cgmax")"
printf '0::relative/path\n' >"$cgmax/proc_cgroup"
assert_eq "a non-absolute cgroup path reads no cgroup" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"disk_free_min_mb\":20464}" \
  "$(run_linux "$cgmax")"
printf 'MemTotal:       0 kB\n' >"$work/meminfo-zero.txt"
assert_eq "a 0 kB MemTotal is a failed read, not a 0 MB machine" \
  "${PREFIX}{\"os\":\"linux\",\"disk_free_min_mb\":20464}" \
  "$(MEMINFO="$work/meminfo-zero.txt" run_linux "")"
assert_eq "an unrecognised build outcome is omitted" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"disk_free_min_mb\":20464}" \
  "$(run_linux "" --build-outcome 'success"},"x":1')"
assert_eq "a non-numeric oom baseline is ignored (cumulative on hosted)" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"cgroup_oom_kills\":3,\"disk_free_min_mb\":20464}" \
  "$(run_linux "$cg3" --oom-kill-baseline '<unreadable>')"

echo "(e2) phys_mem_mb is the memory available to THIS job, not host RAM"
# The same rule as qontinui-coord's .github/scripts/cgroup-mem-budget-kb.sh:
# MemTotal capped by the tightest finite memory.high / memory.max from the
# job's cgroup up to the root. The fixtures are merytshost's real shape: a
# 377689 MiB host whose runner units are memory.high 48 GiB / memory.max
# 64 GiB under ci-runners.slice (high 203952 MiB, max 226617 MiB).
printf 'MemTotal:       386753564 kB\nSwapTotal:             0 kB\n' >"$work/meminfo-377g.txt"
B_SLICE=/ci.slice/ci-runners.slice
B_UNIT=$B_SLICE/actions.runner.qontinui-qontinui-runner.merytshost-public.service
budget_fixture() { # <name> <proc_cgroup path>; echoes the dir
  local d="$work/bud-$1"
  mkdir -p "$d/root"
  printf '0::%s\n' "$2" >"$d/proc_cgroup"
  printf '%s' "$d"
}
limit() { # <fixture dir> <cgroup path> <file> <value>
  mkdir -p "$1/root$2"
  printf '%s\n' "$4" >"$1/root$2/$3"
}
merytshost_tree() { # <fixture dir>: the slice and ci.slice levels as they are
  limit "$1" "$B_SLICE" memory.high 213860220928
  limit "$1" "$B_SLICE" memory.max 237623050240
  limit "$1" /ci.slice memory.high max
  limit "$1" /ci.slice memory.max max
}
budget_line() { # <phys_mem_mb or ""> -> the expected self-hosted report line
  if [ -n "$1" ]; then
    printf '%s{"os":"linux","phys_mem_mb":%s,"pagefile_alloc_mb":0,"disk_free_min_mb":20464}' "$PREFIX" "$1"
  else
    printf '%s{"os":"linux","pagefile_alloc_mb":0,"disk_free_min_mb":20464}' "$PREFIX"
  fi
}
brun() { # <fixture dir> -> the self-hosted report against the 377 GB host
  MEMINFO="$work/meminfo-377g.txt" RUNNER_ENV=self-hosted run_linux "$1"
}
GIB=1073741824

b="$(budget_fixture unit "$B_UNIT")"; merytshost_tree "$b"
limit "$b" "$B_UNIT" memory.high $(( 48 * GIB ))
limit "$b" "$B_UNIT" memory.max $(( 64 * GIB ))
assert_eq "limited unit: its 48 GiB memory.high, not the 377689 MiB host" \
  "$(budget_line 49152)" "$(brun "$b")"

b="$(budget_fixture unitmax "$B_UNIT")"; merytshost_tree "$b"
limit "$b" "$B_UNIT" memory.high $(( 48 * GIB ))
limit "$b" "$B_UNIT" memory.max $(( 30000 * MIB ))
assert_eq "both files are read: a memory.max tighter than memory.high binds" \
  "$(budget_line 30000)" "$(brun "$b")"

b="$(budget_fixture nested "$B_UNIT")"
limit "$b" "$B_UNIT" memory.high $(( 48 * GIB ))
limit "$b" "$B_UNIT" memory.max $(( 64 * GIB ))
limit "$b" "$B_SLICE" memory.high $(( 18000 * MIB ))
limit "$b" "$B_SLICE" memory.max $(( 24000 * MIB ))
limit "$b" /ci.slice memory.max $(( 100 * GIB ))
assert_eq "nested tighter parent: the slice's 18000 MiB binds, looser levels above and below do not" \
  "$(budget_line 18000)" "$(brun "$b")"

b="$(budget_fixture allmax "$B_UNIT")"
for lvl in "$B_UNIT" "$B_SLICE" /ci.slice; do
  limit "$b" "$lvl" memory.high max
  limit "$b" "$lvl" memory.max max
done
assert_eq "\`max\` at every level: MemTotal" \
  "$(budget_line 377689)" "$(brun "$b")"

b="$(budget_fixture above "$B_UNIT")"
limit "$b" "$B_UNIT" memory.max $(( 64 * GIB ))
assert_eq "a limit ABOVE MemTotal adds no bound (a 16 GiB VM in a 64 GiB unit)" \
  "${PREFIX}{\"os\":\"linux\",\"phys_mem_mb\":16378,\"pagefile_alloc_mb\":4095,\"disk_free_min_mb\":20464}" \
  "$(RUNNER_ENV=self-hosted run_linux "$b")"

assert_eq "unreadable /proc/self/cgroup: MemTotal" \
  "$(budget_line 377689)" "$(brun "$work/no-such-cgroup")"

b="$(budget_fixture notree "$B_UNIT")"; rm -rf "$b/root"
assert_eq "a cgroup path whose tree is unreadable: MemTotal" \
  "$(budget_line 377689)" "$(brun "$b")"

b="$(budget_fixture junk "$B_UNIT")"
limit "$b" "$B_UNIT" memory.high 18446744073709551615
limit "$b" "$B_UNIT" memory.max ''
limit "$b" "$B_SLICE" memory.high abc
limit "$b" "$B_SLICE" memory.max 0
assert_eq "junk, empty, 0 and wrap-sized (20-digit) limits add no bound" \
  "$(budget_line 377689)" "$(brun "$b")"

b="$(budget_fixture nsroot /)"
limit "$b" "" memory.max $(( 8 * GIB ))
assert_eq "a cgroup-namespaced container (0::/): the namespace root's own limit binds" \
  "$(budget_line 8192)" "$(brun "$b")"

b="$(budget_fixture nomem "$B_UNIT")"
limit "$b" "$B_UNIT" memory.high $(( 48 * GIB ))
assert_eq "no MemTotal: omitted, even with a cgroup limit readable" \
  "${PREFIX}{\"os\":\"linux\",\"disk_free_min_mb\":20464}" \
  "$(MEMINFO="$work/none" RUNNER_ENV=self-hosted run_linux "$b")"

b="$(budget_fixture win "$B_UNIT")"
limit "$b" "$B_UNIT" memory.high $(( 4 * GIB ))
assert_eq "windows: phys_mem_mb is the reader's, no cgroup is consulted" \
  "${PREFIX}{\"os\":\"windows\",\"phys_mem_mb\":16378,\"commit_limit_mb\":49146,\"pagefile_alloc_mb\":32768,\"pagefile_peak_mb\":32767,\"peak_commit_mb\":48990,\"disk_free_min_mb\":20464}" \
  "$(RESOURCE_REPORT_CGROUP_PROC="$b/proc_cgroup" RESOURCE_REPORT_CGROUP_ROOT="$b/root" \
      run_windows "$fx/windows-reading-hosted.txt")"

echo "(f) Windows: GetPerformanceInfo wins, CIM is the fallback, CRLF tolerated"
WIN_HOSTED="${PREFIX}{\"os\":\"windows\",\"phys_mem_mb\":16378,\"commit_limit_mb\":49146,\"pagefile_alloc_mb\":32768,\"pagefile_peak_mb\":32767,\"peak_commit_mb\":48990,\"disk_free_min_mb\":20464,\"build_outcome\":\"failure\"}"
assert_eq "hosted windows, failure" "$WIN_HOSTED" \
  "$(run_windows "$fx/windows-reading-hosted.txt" --build-outcome failure)"
# powershell writes CRLF. Made here rather than committed: .gitattributes
# normalises the fixtures to LF on checkout.
sed 's/$/\r/' "$fx/windows-reading-hosted.txt" >"$work/windows-reading-hosted-crlf.txt"
assert_eq "hosted windows, CRLF reading" "$WIN_HOSTED" \
  "$(run_windows "$work/windows-reading-hosted-crlf.txt" --build-outcome failure)"
assert_eq "CIM only, a 0 RAM reading dropped, no pagefile peak" \
  "${PREFIX}{\"os\":\"windows\",\"commit_limit_mb\":49148,\"pagefile_alloc_mb\":32768,\"disk_free_min_mb\":20464,\"build_outcome\":\"cancelled\"}" \
  "$(run_windows "$fx/windows-reading-cim-only.txt" --build-outcome cancelled)"
assert_eq "self-hosted windows: boot-relative peaks (commit, pagefile) omitted" \
  "${PREFIX}{\"os\":\"windows\",\"phys_mem_mb\":16378,\"commit_limit_mb\":49146,\"pagefile_alloc_mb\":32768,\"disk_free_min_mb\":20464,\"build_outcome\":\"failure\"}" \
  "$(RUNNER_ENV=self-hosted run_windows "$fx/windows-reading-hosted.txt" --build-outcome failure)"
assert_eq "no reading at all (powershell failed): os and disk only" \
  "${PREFIX}{\"os\":\"windows\",\"disk_free_min_mb\":20464}" \
  "$(run_windows "$work/none")"
printf 'phys_mem_mb=016378\nnot_a_key=5\npeak_rss_mb=\npeak_commit_mb=1.5\n' >"$work/win-odd.txt"
assert_eq "leading zeros normalised; unknown keys, empty and fractional values dropped" \
  "${PREFIX}{\"os\":\"windows\",\"phys_mem_mb\":16378,\"disk_free_min_mb\":20464}" \
  "$(run_windows "$work/win-odd.txt")"

echo "(g) contract: one line, exit 0, usage error exits 2"
out="$(run_linux "$cg" --build-outcome success)"
assert_eq "exactly one output line" "1" "$(printf '%s\n' "$out" | wc -l | tr -d ' ')"
assert_eq "unknown argument is a usage error" "<exit 2>" "$(run_linux "" --bogus)"
if command -v python3 >/dev/null 2>&1; then
  for f in "$fx/windows-reading-hosted.txt" "$fx/windows-reading-cim-only.txt"; do
    line="$(run_windows "$f" --build-outcome success)"
    assert_eq "JSON parses: $(basename "$f")" "ok" \
      "$(printf '%s' "${line#"$PREFIX"}" | python3 -c 'import json,sys; json.load(sys.stdin); print("ok")' 2>&1)"
  done
  assert_eq "JSON parses: hosted linux" "ok" \
    "$(printf '%s' "${out#"$PREFIX"}" | python3 -c 'import json,sys; json.load(sys.stdin); print("ok")' 2>&1)"
fi

echo "(h) live: this machine's own reading carries its RAM"
# No seams: the real /proc, cgroup, df — or, under Git Bash on the Windows CI
# leg, the real powershell reader. This is the only case that proves the .ps1
# reads anything on a real Windows host.
live_status=0
live_err="$(mktemp)"
live="$(RESOURCE_REPORT_DEBUG=1 bash "$script" --build-outcome success 2>"$live_err")" || live_status=$?
if [ -s "$live_err" ]; then
  echo "  (live reader diagnostics:)"; sed 's/^/    /' "$live_err"
fi
rm -f "$live_err"
assert_eq "live: exit 0" "0" "$live_status"
case "$(uname -s)" in
  Linux*) live_os=linux ;;
  MINGW*|MSYS*|CYGWIN*) live_os=windows ;;
  *) live_os="" ;;
esac
if [ -n "$live_os" ]; then
  assert_eq "live: os is $live_os" "yes" \
    "$(case "$live" in "${PREFIX}{\"os\":\"$live_os\""*) echo yes ;; *) echo "no: $live" ;; esac)"
  assert_eq "live: phys_mem_mb measured" "yes" \
    "$(case "$live" in *'"phys_mem_mb":'[1-9]*) echo yes ;; *) echo "no: $live" ;; esac)"
  if [ "$live_os" = windows ]; then
    assert_eq "live windows: commit_limit_mb measured" "yes" \
      "$(case "$live" in *'"commit_limit_mb":'[1-9]*) echo yes ;; *) echo "no: $live" ;; esac)"
    # The one key only the GetPerformanceInfo P/Invoke yields (the CIM
    # fallback has no commit peak), so a broken Add-Type is caught here. It is
    # gated on a hosted VM, which is where this CI leg runs.
    if [ "${RUNNER_ENVIRONMENT:-}" = github-hosted ]; then
      assert_eq "live hosted windows: peak_commit_mb measured (GetPerformanceInfo works)" "yes" \
        "$(case "$live" in *'"peak_commit_mb":'[1-9]*) echo yes ;; *) echo "no: $live" ;; esac)"
    fi
  fi
fi

echo "(i) the Windows reader parses"
ps_parse='$e=$null; [void][System.Management.Automation.Language.Parser]::ParseFile($args[0],[ref]$null,[ref]$e); if ($e.Count) { $e | ForEach-Object { $_.Message }; exit 1 }; "ok"'
ps_bin=""
if command -v pwsh >/dev/null 2>&1; then ps_bin=pwsh
elif command -v powershell >/dev/null 2>&1; then ps_bin=powershell
fi
if [ -n "$ps_bin" ]; then
  ps_path="$ps1"
  if command -v cygpath >/dev/null 2>&1; then ps_path="$(cygpath -w "$ps1")"; fi
  assert_eq "resource-report-windows.ps1 has no parse errors" "ok" \
    "$("$ps_bin" -NoProfile -Command "& { $ps_parse }" "$ps_path" 2>&1 | tr -d '\r')"
else
  echo "  SKIP  no pwsh/powershell on this host (the windows CI leg runs it)"
fi

echo
echo "$cases cases, $failures failures"
[ "$failures" -eq 0 ]
