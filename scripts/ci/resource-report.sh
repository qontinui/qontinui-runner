#!/usr/bin/env bash
# resource-report.sh — one machine-readable line per heavy CI job saying what
# the runner it ran on HAD and what the job USED, so coord can learn each
# runner class's capability and each job's requirement from measurement rather
# than from a hand-typed table.
#
# Plan 2026-10-27-coord-routes-ci-jobs-by-runner-capability, Phase 2 (D1, D2).
# Coord reads the line back through the check-run annotations API (Phase 3,
# `ci_baseline.rs` reader) — an annotation, because a job summary is exposed by
# no API and a log line costs coord a log fetch per job.
#
# OUTPUT. Exactly one line on stdout, always:
#   ::notice title=qontinui-resource-report::{json}
# with the keys, in this order, each present ONLY when measured:
#   os                 "linux" | "windows" (always present)
#   phys_mem_mb        the memory AVAILABLE TO THIS JOB.
#                        linux: MemTotal, capped by the tightest finite cgroup
#                        v2 `memory.high` or `memory.max` found walking from
#                        this process's cgroup (/proc/self/cgroup) up to the
#                        root. A level reading `max` (unlimited), or missing or
#                        unreadable, adds no bound; with no cgroup readable at
#                        all it is MemTotal. A self-hosted runner unit is
#                        capped well below its host (every merytshost runner
#                        unit: memory.high 48 GiB, memory.max 64 GiB, on a
#                        377 GB box), and coord routes on this key, so
#                        MemTotal alone would describe the HOST, not the job.
#                        memory.high counts, not only memory.max: past it the
#                        kernel throttles every allocation, so it is the
#                        budget a job can actually run in. The algorithm is
#                        qontinui-coord's .github/scripts/cgroup-mem-budget-kb.sh
#                        (min in KiB, then / 1024), reimplemented here as
#                        job_mem_budget_kb because this repo cannot call
#                        coord's script; keep the two IDENTICAL.
#                        windows: GetPerformanceInfo PhysicalTotal, else
#                        Win32_OperatingSystem TotalVisibleMemorySize (no
#                        cgroup; unchanged).
#   commit_limit_mb    the commit limit the OS ENFORCES.
#                        windows: GetPerformanceInfo CommitLimit, else
#                        TotalVirtualMemorySize. linux: CommitLimit, but only
#                        under strict overcommit (vm.overcommit_memory = 2);
#                        under the default heuristic mode the kernel does not
#                        enforce it (RAM/2 + swap would UNDER-state a 16 GB
#                        box as 8 GB), so it is omitted rather than misread.
#   pagefile_alloc_mb  backing store for the commit charge.
#                        windows: sum of Win32_PageFileUsage AllocatedBaseSize.
#                        linux: SwapTotal (swap is Linux's pagefile analogue).
#                        Read at report time, so this is the capacity the job
#                        RAN WITH, including swap or pagefile it added itself
#                        (`Add swapfile` / `Configure pagefile` run earlier in
#                        the same job), not the runner class's bare capacity.
#   pagefile_peak_mb   windows only, github-hosted only: PeakUsage, and only
#                        when there is exactly ONE pagefile — the sum of
#                        per-file peaks is not a peak. It counts from BOOT, so
#                        only a hosted VM (booted for this job) makes it the
#                        job's; a self-hosted box's is omitted.
#   peak_commit_mb     windows only, github-hosted only: GetPerformanceInfo
#                        CommitPeak, which also counts from boot (same gate).
#                        Linux keeps no commit peak.
#   peak_rss_mb        linux only: ANONYMOUS memory, page cache EXCLUDED — the
#                        largest `memory.stat` `anon` of the job cgroup that
#                        the --start-anon-sampler loop (every 2 s, from the
#                        pre-build baseline step to this report) or this
#                        report's own final read saw. A polled lower bound on
#                        the true peak, covering every step in between (in
#                        `test`: the Rust test build and run AND the Tauri
#                        debug build, since the report is the job's last step).
#                        NOT `memory.peak`: that is the cgroup's whole charge,
#                        page cache included, which a cargo build's target/
#                        writes fill up to the cgroup's own limit (measured on
#                        a coord runner cgroup: 49158M peak, its 48 GiB
#                        memory.high, against 857M current, ~76% file pages).
#                        Coord reads this key as a job's memory REQUIREMENT,
#                        so it must not carry cache.
#                        SATURATION: on a throttled runner this peak levels
#                        off at `memory.high`, because past it the kernel
#                        reclaims and stalls the job rather than letting anon
#                        grow. A peak at ~phys_mem_mb therefore means the job
#                        SATURATED its budget: its true requirement is at
#                        least that, not equal to it.
#                        `kernel` is excluded too: it holds reclaimable slab
#                        (the dentry/inode caches those writes grow). On a
#                        github-hosted runner the VM — and so the cgroup — is
#                        the job's own. On a self-hosted runner the cgroup is a
#                        long-lived service, so the peak counts only when it
#                        ROSE above the sampler's first (baseline) reading;
#                        otherwise UNKNOWN, omitted. No sampler state (it never
#                        started, or this is Windows): omitted.
#   cgroup_oom_kills   linux only: `memory.events` `oom_kill`. With
#                        --oom-kill-baseline, the RISE over it (kills since the
#                        baseline step); else, on github-hosted only, the
#                        cumulative count. The counter is cumulative per cgroup,
#                        so a self-hosted reading with no baseline is omitted.
#   disk_free_min_mb   free MB on the volume holding --disk-path (default .),
#                        read now: the report step is its job's LAST step,
#                        after every heavy compile (in `test`: both Rust
#                        halves and the Tauri debug build), when target/ is
#                        at its largest — so this is the job's observed
#                        minimum (one reading, not a series).
#   build_outcome      the outcome of the job's heavy compile+test work —
#                        the one definition qontinui-coord's report uses too.
#                        Pass --build-outcome once per heavy step (the `test`
#                        job passes both Rust halves and the Tauri debug
#                        build; clippy-windows its clippy step); they
#                        combine as: any `failure` -> failure; else any
#                        `cancelled` -> cancelled; else any `success` ->
#                        success (a `skipped` step did not run); else all
#                        `skipped` -> skipped. Empty or unknown values are
#                        ignored; none left -> omitted.
#
# ABSENT, NEVER ZERO. A reading that fails, is empty, or is not a bare
# non-negative integer is OMITTED. A 0 would read as "measured, and nothing" —
# for disk or RAM that is a fabricated incapable runner; for oom_kills a
# fabricated all-clear.
#
# NEVER FAILS THE JOB. Exit status is 0 in every case, including unreadable
# inputs and a powershell that errors; only a usage error exits 2. The step
# calling this also carries `continue-on-error: true`.
#
# THE ANON SAMPLER. `--start-anon-sampler --state-dir DIR` (Linux; a no-op
# elsewhere) records the cgroup's current `anon` as DIR/anon-baseline-bytes
# and DIR/anon-peak-bytes, then starts `--anon-sampler-loop` fully detached
# (nohup, stdio to /dev/null, so it cannot hold a step's output open) and
# writes its pid to DIR/anon-sampler.pid. The loop raises
# DIR/anon-peak-bytes whenever `anon` exceeds it, every --interval seconds
# (default 2), and exits when DIR/anon-sampler.stop appears, when DIR
# disappears, or after RESOURCE_REPORT_SAMPLER_MAX_SECS (default 21600), so a
# self-hosted runner never keeps one. A report given --anon-state-dir DIR
# stops it (the stop file, plus a kill of the recorded pid only when that
# pid's command line is still this loop), takes one last reading itself, and
# reports the peak. Every failure along the way is an omitted key.
#
# The Windows readings come from scripts/ci/resource-report-windows.ps1, which
# prints `key=value` lines; the first well-formed value per key wins, so a
# GetPerformanceInfo reading takes precedence over the CIM fallback.
#
# TEST SEAMS (all default to the live system; set only by
# scripts/tests/test_resource_report.sh):
#   RESOURCE_REPORT_OS               linux | windows (default: from RUNNER_OS / uname)
#   RESOURCE_REPORT_MEMINFO          default /proc/meminfo
#   RESOURCE_REPORT_OVERCOMMIT       default /proc/sys/vm/overcommit_memory
#   RESOURCE_REPORT_CGROUP_PROC      default /proc/self/cgroup
#   RESOURCE_REPORT_CGROUP_ROOT      default /sys/fs/cgroup
#   RESOURCE_REPORT_WINDOWS_READING  a file of key=value lines used INSTEAD of
#                                    running the powershell reader
#   RESOURCE_REPORT_DF               a file of `df -Pm` output used instead of df
#   RESOURCE_REPORT_SAMPLER_MAX_SECS the anon loop's lifetime cap (default 21600)
#   RUNNER_ENVIRONMENT               github-hosted | self-hosted (set by Actions)
#
# Run the tests: bash scripts/tests/test_resource_report.sh

set -uo pipefail

outcomes=""
oom_kill_baseline=""
disk_path="."
anon_state_dir=""
mode=report
interval=2

while [ $# -gt 0 ]; do
  case "$1" in
    --build-outcome) outcomes="$outcomes ${2-}"; shift 2 || shift ;;
    --oom-kill-baseline) oom_kill_baseline="${2-}"; shift 2 || shift ;;
    --disk-path) disk_path="${2-}"; shift 2 || shift ;;
    --anon-state-dir|--state-dir) anon_state_dir="${2-}"; shift 2 || shift ;;
    --interval) interval="${2-}"; shift 2 || shift ;;
    --start-anon-sampler) mode=start; shift ;;
    --anon-sampler-loop) mode=loop; shift ;;
    -h|--help)
      sed -n '2,/^# Run the tests:/p' "$0"
      exit 0
      ;;
    *)
      echo "resource-report.sh: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

# A bare non-negative integer of a size bash arithmetic cannot wrap.
is_count() {
  case "${1:-}" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "${#1}" -le 18 ]
}

# Normalised decimal (no leading zeros, which JSON forbids and bash would read
# as octal).
norm() { printf '%d' "$((10#$1))"; }

# The memory available to THIS job, in KiB (header: phys_mem_mb), or nothing
# when MemTotal is unreadable. MemTotal, lowered by every finite `memory.max`
# and `memory.high` from this process's cgroup up to the root.
#
# IDENTICAL ALGORITHM to qontinui-coord's .github/scripts/cgroup-mem-budget-kb.sh
# — same files, same order, same per-level min, same rejection of `max`, empty,
# non-digit and over-18-digit values, same KiB truncation before the compare —
# so one runner unit reports the same phys_mem_mb from either repo. That
# script cannot be called from here; change both together.
job_mem_budget_kb() { # <meminfo>
  local mem_kb rel cg f lim root
  mem_kb="$(awk '$1 == "MemTotal:" { print $2; exit }' "$1" 2>/dev/null)"
  is_count "$mem_kb" || return 0
  mem_kb="$((10#$mem_kb))"
  [ "$mem_kb" -ge 1 ] || return 0
  root="${RESOURCE_REPORT_CGROUP_ROOT:-/sys/fs/cgroup}"
  # cgroup v2 only: the `0::` line. None (v1-only host, unreadable file) or a
  # non-absolute path walks nothing, and MemTotal stands.
  rel="$(sed -n 's/^0:://p' "${RESOURCE_REPORT_CGROUP_PROC:-/proc/self/cgroup}" 2>/dev/null | head -n 1)"
  case "$rel" in /*) ;; *) rel="" ;; esac
  if [ -n "$rel" ]; then
    cg="$rel"
    while :; do
      # A parent's limit binds all its children, so EVERY level is read.
      for f in memory.max memory.high; do
        lim="$(cat "$root$cg/$f" 2>/dev/null || true)"
        is_count "$lim" || continue # `max`, empty, junk or wrap-sized: no bound
        lim="$((10#$lim))"
        if [ "$lim" -gt 0 ] && [ "$((lim / 1024))" -lt "$mem_kb" ]; then
          mem_kb="$((lim / 1024))"
        fi
      done
      [ "$cg" = / ] && break
      cg="$(dirname "$cg")"
    done
  fi
  printf '%s' "$mem_kb"
}

# This process's cgroup v2 directory, or nothing.
cgroup_dir() {
  local cg
  cg="$(sed -n 's/^0:://p' "${RESOURCE_REPORT_CGROUP_PROC:-/proc/self/cgroup}" 2>/dev/null | head -n 1)"
  case "$cg" in /*) printf '%s' "${RESOURCE_REPORT_CGROUP_ROOT:-/sys/fs/cgroup}$cg" ;; esac
  return 0
}

# The cgroup's `memory.stat` `anon` in bytes, or nothing.
anon_bytes() {
  local dir v
  dir="$(cgroup_dir)"
  [ -n "$dir" ] || return 0
  v="$(awk '$1 == "anon" { print $2; exit }' "$dir/memory.stat" 2>/dev/null || true)"
  is_count "$v" && printf '%d' "$((10#$v))"
  return 0
}

# Raise DIR/anon-peak-bytes to <bytes> if larger (or absent). Atomic rename,
# so a reader never sees a half-written number.
raise_anon_peak() { # dir bytes
  local cur
  is_count "${2:-}" || return 0
  cur="$(cat "$1/anon-peak-bytes" 2>/dev/null || true)"
  if ! is_count "$cur" || [ "$((10#$2))" -gt "$((10#$cur))" ]; then
    printf '%s\n' "$2" > "$1/anon-peak-bytes.tmp.$$" 2>/dev/null \
      && mv -f "$1/anon-peak-bytes.tmp.$$" "$1/anon-peak-bytes" 2>/dev/null \
      || rm -f "$1/anon-peak-bytes.tmp.$$" 2>/dev/null
  fi
  return 0
}

case "$interval" in ''|*[!0-9.]*) interval=2 ;; esac

if [ "$mode" = loop ]; then
  [ -n "$anon_state_dir" ] || exit 0
  max_secs="${RESOURCE_REPORT_SAMPLER_MAX_SECS:-21600}"
  is_count "$max_secs" || max_secs=21600
  started="$(date +%s 2>/dev/null || echo 0)"
  while [ -d "$anon_state_dir" ] && [ ! -e "$anon_state_dir/anon-sampler.stop" ]; do
    raise_anon_peak "$anon_state_dir" "$(anon_bytes)"
    now="$(date +%s 2>/dev/null || echo 0)"
    [ "$((now - started))" -lt "$((10#$max_secs))" ] || break
    sleep "$interval" || break
  done
  exit 0
fi

if [ "$mode" = start ]; then
  [ -n "$anon_state_dir" ] || exit 0
  [ "$(uname -s 2>/dev/null)" = Linux ] || [ -n "${RESOURCE_REPORT_CGROUP_PROC:-}" ] || exit 0
  base="$(anon_bytes)"
  is_count "$base" || exit 0
  mkdir -p "$anon_state_dir" 2>/dev/null || exit 0
  rm -f "$anon_state_dir/anon-sampler.stop" 2>/dev/null
  printf '%s\n' "$base" > "$anon_state_dir/anon-baseline-bytes" 2>/dev/null || exit 0
  raise_anon_peak "$anon_state_dir" "$base"
  nohup bash "${BASH_SOURCE[0]}" --anon-sampler-loop --state-dir "$anon_state_dir" \
    --interval "$interval" </dev/null >/dev/null 2>&1 &
  printf '%s\n' "$!" > "$anon_state_dir/anon-sampler.pid" 2>/dev/null
  echo "anon sampler started (pid $!, baseline anon $((base / 1048576)) MiB)"
  exit 0
fi

# Stop the loop started by --start-anon-sampler. The pid is killed only when
# its command line is still the loop (a recycled pid is someone else's).
stop_anon_sampler() { # dir
  local pid
  touch "$1/anon-sampler.stop" 2>/dev/null || true
  pid="$(cat "$1/anon-sampler.pid" 2>/dev/null || true)"
  is_count "$pid" || return 0
  if tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -qF -- "--anon-sampler-loop"; then
    kill "$pid" 2>/dev/null || true
  fi
  return 0
}

# Ordered report fields. `set_field key value` keeps the FIRST well-formed
# value per key and silently drops the rest.
KEYS="phys_mem_mb commit_limit_mb pagefile_alloc_mb pagefile_peak_mb peak_commit_mb peak_rss_mb cgroup_oom_kills disk_free_min_mb"
for k in $KEYS; do eval "F_$k=''"; done
set_field() {
  local key="$1" value="${2:-}" cur
  case " $KEYS " in *" $key "*) ;; *) return 0 ;; esac
  value="${value%$'\r'}"
  is_count "$value" || return 0
  # No machine has 0 MB of RAM or a 0 MB commit limit: that is a failed read.
  # (0 IS a real reading for swap, a pagefile peak, oom_kills and a full disk.)
  case "$key" in
    phys_mem_mb|commit_limit_mb) [ "$((10#$value))" -gt 0 ] || return 0 ;;
  esac
  eval "cur=\${F_$key}"
  [ -z "$cur" ] || return 0
  eval "F_$key=\$(norm \"\$value\")"
}

os="${RESOURCE_REPORT_OS:-}"
if [ -z "$os" ]; then
  case "${RUNNER_OS:-$(uname -s 2>/dev/null)}" in
    Linux|linux) os=linux ;;
    Windows|windows|MINGW*|MSYS*|CYGWIN*) os=windows ;;
    macOS|Darwin) os=macos ;;
    *) os=unknown ;;
  esac
fi
hosted=0
[ "${RUNNER_ENVIRONMENT:-}" = "github-hosted" ] && hosted=1

kib_to_mb() { is_count "${1:-}" && printf '%d' "$(( 10#$1 / 1024 ))"; }
bytes_to_mb() { is_count "${1:-}" && printf '%d' "$(( 10#$1 / 1048576 ))"; }

# ── linux ───────────────────────────────────────────────────────────────
if [ "$os" = linux ]; then
  meminfo="${RESOURCE_REPORT_MEMINFO:-/proc/meminfo}"
  if [ -r "$meminfo" ]; then
    # The job's memory budget, not host RAM (header).
    set_field phys_mem_mb "$(kib_to_mb "$(job_mem_budget_kb "$meminfo")")"
    set_field pagefile_alloc_mb "$(kib_to_mb "$(awk '$1 == "SwapTotal:" { print $2; exit }' "$meminfo" 2>/dev/null)")"
    overcommit="$(cat "${RESOURCE_REPORT_OVERCOMMIT:-/proc/sys/vm/overcommit_memory}" 2>/dev/null || true)"
    if [ "${overcommit%$'\r'}" = "2" ]; then
      set_field commit_limit_mb "$(kib_to_mb "$(awk '$1 == "CommitLimit:" { print $2; exit }' "$meminfo" 2>/dev/null)")"
    fi
  fi

  # Anonymous-memory peak (header) — never memory.peak, which carries page
  # cache. Only from sampler state: no baseline, no key.
  if [ -n "$anon_state_dir" ] && [ -d "$anon_state_dir" ]; then
    stop_anon_sampler "$anon_state_dir"
    anon_base="$(cat "$anon_state_dir/anon-baseline-bytes" 2>/dev/null || true)"
    if is_count "$anon_base"; then
      raise_anon_peak "$anon_state_dir" "$(anon_bytes)"
      anon_peak="$(cat "$anon_state_dir/anon-peak-bytes" 2>/dev/null || true)"
      if is_count "$anon_peak"; then
        if [ "$hosted" = 1 ] || [ "$((10#$anon_peak))" -gt "$((10#$anon_base))" ]; then
          set_field peak_rss_mb "$(bytes_to_mb "$anon_peak")"
        fi
      fi
    fi
  fi

  cgdir="$(cgroup_dir)"
  if [ -n "$cgdir" ]; then
    kills="$(awk '$1 == "oom_kill" { print $2; exit }' "$cgdir/memory.events" 2>/dev/null || true)"
    if is_count "$kills"; then
      if is_count "$oom_kill_baseline"; then
        if [ "$((10#$kills))" -ge "$((10#$oom_kill_baseline))" ]; then
          set_field cgroup_oom_kills "$(( 10#$kills - 10#$oom_kill_baseline ))"
        fi
      elif [ "$hosted" = 1 ]; then
        set_field cgroup_oom_kills "$kills"
      fi
    fi
  fi
fi

# ── windows ─────────────────────────────────────────────────────────────
if [ "$os" = windows ]; then
  if [ -n "${RESOURCE_REPORT_WINDOWS_READING:-}" ]; then
    reading="$(cat "$RESOURCE_REPORT_WINDOWS_READING" 2>/dev/null || true)"
  else
    ps1="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd)/resource-report-windows.ps1"
    if command -v cygpath >/dev/null 2>&1; then ps1="$(cygpath -w "$ps1")"; fi
    if [ -n "${RESOURCE_REPORT_DEBUG:-}" ]; then
      # Debug: let the reader's diagnostics through to this script's stderr.
      reading="$(powershell -NoProfile -ExecutionPolicy Bypass -File "$ps1" | tr -d '\r' || true)"
    else
      reading="$(powershell -NoProfile -ExecutionPolicy Bypass -File "$ps1" 2>/dev/null | tr -d '\r' || true)"
    fi
  fi
  while IFS='=' read -r key value; do
    [ -n "$key" ] || continue
    # CommitPeak and PeakUsage count from BOOT: the job's only on a hosted VM.
    case "$key" in
      peak_commit_mb|pagefile_peak_mb) [ "$hosted" = 1 ] || continue ;;
    esac
    set_field "$key" "$value"
  done <<EOF_READING
$reading
EOF_READING
fi

# ── disk (both) ─────────────────────────────────────────────────────────
if [ -n "${RESOURCE_REPORT_DF:-}" ]; then
  df_out="$(cat "$RESOURCE_REPORT_DF" 2>/dev/null || true)"
else
  df_out="$(df -Pm "$disk_path" 2>/dev/null || true)"
fi
set_field disk_free_min_mb "$(printf '%s\n' "$df_out" | awk 'NR == 2 { print $4; exit }')"

# ── emit ────────────────────────────────────────────────────────────────
json="{\"os\":\"$os\""
for k in $KEYS; do
  eval "v=\${F_$k}"
  [ -z "$v" ] || json="$json,\"$k\":$v"
done
# The heavy work's outcome (header): failure > cancelled > success > skipped.
build_outcome=""
for o in $outcomes; do
  case "$o" in
    failure) build_outcome=failure ;;
    cancelled) [ "$build_outcome" = failure ] || build_outcome=cancelled ;;
    success) case "$build_outcome" in failure|cancelled) ;; *) build_outcome=success ;; esac ;;
    skipped) [ -n "$build_outcome" ] || build_outcome=skipped ;;
  esac
done
[ -z "$build_outcome" ] || json="$json,\"build_outcome\":\"$build_outcome\""
json="$json}"
printf '::notice title=qontinui-resource-report::%s\n' "$json"
exit 0
