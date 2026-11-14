#!/usr/bin/env bash
# ci-cache-slot.sh -- a bounded, flock-guarded slot pool for a self-hosted CI
# job's persistent cargo state (its target, and for some repos its rustup).
#
# BYTE-IDENTICAL COPIES. This file exists, unchanged, at
# .github/scripts/ci-cache-slot.sh in three repos:
#   qontinui/qontinui-supervisor   (ci.yml jobs rust-check and compose-parity)
#   qontinui/qontinui-runner       (ci.yml job test)
#   qontinui/qontinui-schemas      (rust-ci.yml job rust-ci)
# Edit all three together; nothing repo-specific lives in here. Each workflow
# passes its own root and bounds. Its test is beside it under each repo's own
# test convention, and is byte-identical too.
#
# WHY. The persistent state used to be keyed by $RUNNER_NAME, one dir per
# registration that ever ran the job. An org-wide pool has dozens of
# registrations per host, so the entry count grew with the pool, without bound.
# On nawa, 2026-11-04, it was 104G in one cache root and the disk lost ~14G an
# hour (plan 2026-11-04-devops-ci-persistent-target-caches-keyed-per-
# registration-grow-without-bound-on-the-org-pool). This bounds BOTH the
# entry count (K slots) and the root's total (a budget B).
#
# USAGE (Linux only; the self-hosted pool is Linux). Source it, then call:
#
#   . .github/scripts/ci-cache-slot.sh
#   ci_cache_slot_select --root "$HOME/.cache/<repo>-ci" \
#       --budget-gib 32 --entry-bound-gib 40 [--floor-gib 60] [--entry target]
#
# Overrides, read from the environment (empty = the default; a non-integer is
# refused with a warning and the default used):
#   CI_CACHE_SLOTS       K, the slot count               (default 4)
#   CI_CACHE_BUDGET_GIB  B, overrides --budget-gib
#
# It sets, in the caller's shell, and appends to $GITHUB_ENV when that is set:
#   CI_CACHE_STATE_DIR   the chosen state dir: $root/slot-N, or, when every
#                        slot is locked, a cold dir under $RUNNER_TEMP (which
#                        the runner empties per job)
#   CI_CACHE_SLOT        slot-N, or "cold"
# and, in the caller's shell only:
#   CI_CACHE_ENTRY_DIR   $CI_CACHE_STATE_DIR/<entry>, created, within bounds
#   CI_CACHE_ENTRY_USED_KB  its size after the bound checks (0 when reset)
#
# WHAT IT DOES, in order:
#   1. TAKE A SLOT. The first slot-n, n in 0..K-1, whose lock it can take with
#      `exec 9>"$root/slot-$n.lock"; flock -n 9`. Lock files live BESIDE the
#      slot dirs, never inside them, so no reset can delete a held lock. No
#      free slot: a cold dir under $RUNNER_TEMP.
#   2. HOLD IT FOR THE JOB. Every `run:` step is its own shell, so a lock held
#      by the setup step alone would end with that step. fd 9 is handed to a
#      detached holder, `nohup bash -c 'while kill -0 "$1" ...' _ "$WORKER"
#      9>&9 </dev/null >/dev/null 2>&1 &` (the nohup pattern of
#      qontinui-runner scripts/ci/resource-report.sh), and then closed here,
#      so only the holder keeps the lock. WORKER is the pid of the ancestor
#      named Runner.Worker, found by walking /proc/<pid>/stat ppid up from $$.
#      The holder is EXPECTED, not yet verified on nawa, to end on a normal
#      job end through the runner's orphan-process cleanup (it inherits
#      RUNNER_TRACKING_ID). A killed runner service is expected to leave its
#      children alive (KillMode=process, also to be verified on nawa); either
#      way the kill -0 backstop ends the holder within 15 s of the worker's
#      exit. FALLBACKS, chosen so a lock is never held by a
#      process that cannot end: no Runner.Worker in the chain -> the ancestor
#      directly below Runner.Listener (in a runner's tree that IS the job's
#      worker under another name); no Listener either -> no pid is trusted and
#      the holder releases after at most 6 hours, the default job ceiling.
#   3. REAP LEGACY DIRS (the old $RUNNER_NAME keying). In the root, a dir that
#      is not slot-* and not *.lock:
#        (a) named $RUNNER_NAME is removed unconditionally: one registration
#            runs one job at a time, and this job is that registration's;
#        (b) any other is removed only when nothing in its top 3 levels is
#            newer than 6 hours (find -newermt) AND no live process on the box
#            has its cwd or an open fd under it (readlink over /proc/<pid>/cwd
#            and /proc/<pid>/fd/*; entries it cannot read are ignored): a job
#            on a pre-change workflow commit may still be building in it, and
#            a legacy dir has no lock. A /proc scan that cannot be done at all
#            (no /proc, or not one entry readable) KEEPS the dir;
#        (c) slot-* is never touched here, only by step 4.
#   4. EVICT ACROSS SLOTS when the slot-* total exceeds B, or free space on the
#      root's filesystem is under the floor: OTHER unlocked slots first,
#      largest first, each removed only while this script holds its lock
#      (flock -n on a fresh fd), and only then this job's own entry.
#   5. BOUND THIS JOB'S ENTRY: unreadable, or over min(entry bound, B/K), it
#      starts cold. Free space that cannot be read starts it cold too, without
#      evicting anyone else on a reading that does not exist.
#
# A chosen slot dir that is a symlink is unlinked (never followed) and made a
# real dir before anything is built or removed in it. A lock file that is a
# symlink is unlinked before it is opened, so the open never truncates a file
# outside the root.
#
# Every removal goes through one guard that refuses an empty path, "/", a path
# with a ".." component, anything not strictly under the root, and a path with
# a symlink among its components below the root (a symlink as the LAST
# component is unlinked, never followed), so no removal reaches outside it.
#
# TEST SEAMS (the test sets these; a workflow never does):
#   CI_CACHE_SLOT_NO_HOLDER=1     no holder is spawned and fd 9 stays open in
#                                 the caller, which then holds the lock itself
#   CI_CACHE_SLOT_WORKER_PID=<p>  skip the /proc walk, use <p>
#   CI_CACHE_SLOT_PROC_ROOT=<d>   the /proc the walk and the in-use scan read
#                                 (default /proc)
#   CI_CACHE_SLOT_START_PID=<p>   where the walk starts (default $$)
#   CI_CACHE_SLOT_FREE_KB=<n>     the free-space reading, instead of df
#   CI_CACHE_SLOT_BUDGET_KB=<n>   B in KiB, so a fixture need not hold GiBs
#   CI_CACHE_SLOT_HOLDER_POLL_SECS=<n>  the holder's poll (default 15)
#   CI_CACHE_SLOT_SCAN=loop       the in-use scan's readlink loop, not GNU find
#
# Safe under `set -euo pipefail`. Returns non-zero only on a usage error, a
# missing flock, an unsafe root, or a failed reset of this job's own entry.

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  echo "ci-cache-slot.sh is sourced, not run: . ${0}; ci_cache_slot_select --root ..." >&2
  exit 2
fi

_ccs_log() { printf 'ci-cache-slot: %s\n' "$*"; }

_ccs_int_or_default() { # name value default
  case "$2" in
    '') printf '%s' "$3"; return 0 ;;
    *[!0-9]*) ;;
    *) if [ "$((10#$2))" -gt 0 ]; then printf '%s' "$((10#$2))"; return 0; fi ;;
  esac
  echo "::warning title=ci-cache-slot override ignored::$1='$2' is not a positive integer; using $3" >&2
  printf '%s' "$3"
}

# kb of a tree, or nothing when du cannot read all of it.
_ccs_kb() {
  local out
  if out=$(du -sk -- "$1" 2>/dev/null | cut -f1) && [ -n "$out" ]; then
    printf '%s' "$out"
  fi
}

_ccs_free_kb() {
  local out
  if [ -n "${CI_CACHE_SLOT_FREE_KB:-}" ]; then
    out=$CI_CACHE_SLOT_FREE_KB
  else
    out=$(df -Pk -- "$1" 2>/dev/null | awk 'NR == 2 {print $4}') || out=""
  fi
  case "$out" in ''|*[!0-9]*) out="" ;; esac
  printf '%s' "$out"
}

# The one removal door. Refuses anything not strictly under the root.
_ccs_rm() { # root path
  local root="${1:?ci-cache-slot: empty root}" p="${2:?ci-cache-slot: empty path}"
  case "$p" in
    /|"$root"|"$root"/) echo "ci-cache-slot: REFUSED to remove '$p' (the root or /)" >&2; return 1 ;;
    */../*|*/..|../*|..) echo "ci-cache-slot: REFUSED to remove '$p' (a .. component)" >&2; return 1 ;;
    "$root"/?*) ;;
    *) echo "ci-cache-slot: REFUSED to remove '$p' (not under $root)" >&2; return 1 ;;
  esac
  # A symlinked component above the last would carry rm -rf outside the root.
  local rest="${p#"$root"/}" cur="$root" part
  while [ "${rest#*/}" != "$rest" ]; do
    part=${rest%%/*}
    rest=${rest#*/}
    cur="$cur/$part"
    if [ -L "$cur" ]; then
      echo "ci-cache-slot: REFUSED to remove '$p' ('$cur' is a symlink)" >&2
      return 1
    fi
  done
  if [ -L "$p" ]; then
    rm -f -- "${p:?}"
    return
  fi
  chmod -R u+rwX -- "${p:?}" 2>/dev/null || true
  rm -rf -- "${p:?}"
}

# Reads, once per select, every live process's cwd and open-fd targets into a
# temp file under ${RUNNER_TEMP:-${TMPDIR:-/tmp}} named by _CCS_PROC_TARGETS_FILE, one
# "<proc>/<pid>[/cwd|/fd]<TAB><target>" line each; _ccs_scan_done removes it.
# _CCS_PROC_SCAN is "ok", or the reason the scan could not be done at all.
# GNU find reads every link in one process; without it, one readlink per link.
_ccs_scan_proc() {
  local proc="${CI_CACHE_SLOT_PROC_ROOT:-/proc}" f raw p l
  _CCS_PROC_TARGETS_FILE=""
  if [ ! -d "$proc" ]; then
    _CCS_PROC_SCAN="$proc is not a directory"
    return 0
  fi
  # Under RUNNER_TEMP (emptied per job) so an aborted select leaks nothing.
  local tmpd="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
  if ! f=$(mktemp "$tmpd/ci-cache-slot-proc.XXXXXX" 2>/dev/null); then
    _CCS_PROC_SCAN="mktemp under $tmpd failed"
    return 0
  fi
  _CCS_PROC_TARGETS_FILE=$f
  if [ "${CI_CACHE_SLOT_SCAN:-}" != loop ] && find "$proc" -maxdepth 0 -printf '' >/dev/null 2>&1; then
    # A link it cannot read prints an empty target, which the awk drops.
    { find "$proc"/[0-9]*/cwd "$proc"/[0-9]*/fd -maxdepth 1 \( -name cwd -o -path '*/fd/*' \) \
        -printf '%H\t%l\n' 2>/dev/null || :; } | awk '!/\t$/' > "$f" || :
  else
    raw="$f.raw"
    for p in "$proc"/[0-9]*; do
      [ -d "$p" ] || continue
      printf '\t%s\n' "$p"
      # One operand per call: BusyBox readlink takes a single operand.
      for l in "$p/cwd" "$p"/fd/*; do readlink -- "$l" 2>/dev/null || :; done
    done > "$raw"
    awk '/^\t/ { cur = substr($0, 2); next } $0 != "" { print cur "\t" $0 }' "$raw" > "$f" || :
    rm -f -- "$raw"
  fi
  if [ -s "$f" ]; then
    _CCS_PROC_SCAN=ok
  else
    _CCS_PROC_SCAN="not one cwd or fd under $proc could be read"
  fi
}

_ccs_scan_done() {
  if [ -n "${_CCS_PROC_TARGETS_FILE:-}" ]; then rm -f -- "$_CCS_PROC_TARGETS_FILE"; fi
  _CCS_PROC_TARGETS_FILE=""
  _CCS_PROC_SCAN=""
}

# 0 when a live process has its cwd or an open fd at or under $1, OR when that
# cannot be known (fail closed); 1 only when the scan ran and found none. The
# reason goes to _CCS_IN_USE_WHY.
_ccs_in_use() { # dir
  [ -n "${_CCS_PROC_SCAN:-}" ] || _ccs_scan_proc
  if [ "$_CCS_PROC_SCAN" != ok ]; then
    _CCS_IN_USE_WHY="its use is UNKNOWN: $_CCS_PROC_SCAN"
    return 0
  fi
  local real hit rc=0
  real=$(cd -P -- "$1" 2>/dev/null && pwd) || real=""
  # One pass: a target equal to the dir, or under "dir/", for either spelling.
  # ENVIRON, not -v, so a backslash in a path is not read as an escape.
  hit=$(_CCS_A="$1" _CCS_B="$real" awk '
    BEGIN { a = ENVIRON["_CCS_A"]; b = ENVIRON["_CCS_B"] }
    {
      i = index($0, "\t"); t = substr($0, i + 1); src = substr($0, 1, i - 1)
      if (t == a || index(t, a "/") == 1 || (b != "" && (t == b || index(t, b "/") == 1))) {
        sub(/\/(cwd|fd)$/, "", src); sub(/.*\//, "", src)
        print src "\t" t; found = 1; exit
      }
    }
    END { exit found ? 0 : 1 }' "$_CCS_PROC_TARGETS_FILE") || rc=$?
  if [ "$rc" -eq 0 ] && [ -n "$hit" ]; then
    _CCS_IN_USE_WHY="in use: pid ${hit%%$'\t'*} has ${hit#*$'\t'} open"
    return 0
  fi
  if [ "$rc" -gt 1 ]; then
    _CCS_IN_USE_WHY="its use is UNKNOWN: awk exited $rc over the /proc scan"
    return 0
  fi
  return 1
}

# /proc/<pid>/stat -> "<comm>\t<ppid>". comm may hold spaces and parens, so it
# is everything between the first "(" and the LAST ")".
_ccs_stat() { # proc pid
  local s rest comm
  s=$(cat -- "$1/$2/stat" 2>/dev/null) || return 1
  comm=${s#*(}
  comm=${comm%)*}
  rest=${s##*) }
  # shellcheck disable=SC2086  # split on purpose: "<state> <ppid> ..."
  set -- $rest
  [ "$#" -ge 2 ] || return 1
  printf '%s\t%s' "$comm" "$2"
}

# Prints "<pid> <how>", or nothing when no ancestor can be trusted.
_ccs_find_worker() {
  if [ -n "${CI_CACHE_SLOT_WORKER_PID:-}" ]; then
    printf '%s injected' "$CI_CACHE_SLOT_WORKER_PID"
    return 0
  fi
  local proc="${CI_CACHE_SLOT_PROC_ROOT:-/proc}" pid="${CI_CACHE_SLOT_START_PID:-$$}"
  local prev="" line comm ppid
  for _ in $(seq 1 64); do
    line=$(_ccs_stat "$proc" "$pid") || break
    comm=${line%%$'\t'*}
    ppid=${line#*$'\t'}
    if [ "$comm" = Runner.Worker ]; then
      printf '%s Runner.Worker' "$pid"
      return 0
    fi
    if [ "$comm" = Runner.Listener ]; then
      [ -n "$prev" ] && printf '%s below-Runner.Listener' "$prev"
      return 0
    fi
    case "$ppid" in ''|*[!0-9]*|0|1) break ;; esac
    prev=$pid
    pid=$ppid
  done
  return 0
}

_ccs_spawn_holder() { # worker-pid-or-empty max-secs-or-0
  # shellcheck disable=SC2016  # the body is the child's script, expanded there
  nohup bash -c 'cd / || exit 0
    end=0; [ "$2" -gt 0 ] && end=$(( $(date +%s) + $2 ))
    while :; do
      if [ -n "$1" ]; then kill -0 "$1" 2>/dev/null || exit 0; fi
      if [ "$end" -gt 0 ] && [ "$(date +%s)" -ge "$end" ]; then exit 0; fi
      sleep "$3"
    done' _ "$1" "$2" "${CI_CACHE_SLOT_HOLDER_POLL_SECS:-15}" 9>&9 </dev/null >/dev/null 2>&1 &
  _CCS_HOLDER_PID=$!
}

ci_cache_slot_select() {
  local root="" budget_gib="" entry_bound_gib="" floor_gib=60 entry=target
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --root) root="${2:-}"; shift 2 ;;
      --budget-gib) budget_gib="${2:-}"; shift 2 ;;
      --entry-bound-gib) entry_bound_gib="${2:-}"; shift 2 ;;
      --floor-gib) floor_gib="${2:-}"; shift 2 ;;
      --entry) entry="${2:-}"; shift 2 ;;
      *) echo "ci-cache-slot: unknown argument '$1'" >&2; return 2 ;;
    esac
  done
  case "$root" in
    /?*) ;;
    *) echo "::error title=ci-cache-slot::--root must be an absolute path other than /, got '$root'" >&2; return 2 ;;
  esac
  root=${root%/}
  case "$root" in */..|*/../*|*/.|*/./*) echo "::error title=ci-cache-slot::--root '$root' has a . or .. component" >&2; return 2 ;; esac
  case "$entry" in ''|*/*|.|..) echo "::error title=ci-cache-slot::--entry must be one path component, got '$entry'" >&2; return 2 ;; esac
  case "$budget_gib$entry_bound_gib$floor_gib" in *[!0-9]*) echo "::error title=ci-cache-slot::--budget-gib, --entry-bound-gib and --floor-gib take integers" >&2; return 2 ;; esac
  [ -n "$budget_gib" ] && [ -n "$entry_bound_gib" ] || { echo "::error title=ci-cache-slot::--budget-gib and --entry-bound-gib are required" >&2; return 2; }
  : "${RUNNER_TEMP:?ci-cache-slot: RUNNER_TEMP is unset}"
  command -v flock >/dev/null || { echo "::error title=ci-cache-slot::flock (util-linux) is not on PATH; the host is not provisioned" >&2; return 1; }

  local k b_gib
  k=$(_ccs_int_or_default CI_CACHE_SLOTS "${CI_CACHE_SLOTS:-}" 4)
  b_gib=$(_ccs_int_or_default CI_CACHE_BUDGET_GIB "${CI_CACHE_BUDGET_GIB:-}" "$budget_gib")
  local b_kb=$((b_gib * 1048576)) floor_kb=$((floor_gib * 1048576))
  if [ -n "${CI_CACHE_SLOT_BUDGET_KB:-}" ]; then b_kb=$CI_CACHE_SLOT_BUDGET_KB; fi
  local entry_bound_kb=$((entry_bound_gib * 1048576))
  [ $((b_kb / k)) -ge "$entry_bound_kb" ] || entry_bound_kb=$((b_kb / k))

  mkdir -p -- "$root"

  # 1. Take a slot.
  local n slot=""
  local lockf
  for ((n = 0; n < k; n++)); do
    # A symlinked lock would make the open below truncate its target: unlink it.
    lockf="$root/slot-$n.lock"
    if [ -L "$lockf" ]; then rm -f -- "$lockf"; fi
    exec 9>"$lockf"
    # Lock-and-recheck: a peer's unlink of a symlinked lock (above) can
    # remove the file between our open and our flock, so a lock is only
    # ours while the path still names the inode we hold.
    if flock -n 9 && [ ! -L "$lockf" ] && [ "$lockf" -ef /proc/self/fd/9 ]; then
      slot="slot-$n"
      break
    fi
    exec 9>&-
  done

  local state
  if [ -n "$slot" ]; then
    state="$root/$slot"
    # A symlinked slot would put this job's builds, and its resets, outside
    # the root: unlink it (never its target) and make a real dir.
    if [ -L "$state" ]; then
      rm -f -- "$state"
      _ccs_log "$slot was a symlink: unlinked it (its target untouched) and made a real dir"
    fi
    mkdir -p -- "$state"
    # 2. Hold it for the job.
    if [ "${CI_CACHE_SLOT_NO_HOLDER:-}" = 1 ]; then
      _ccs_log "took $slot of $k under $root (no holder: test seam, the caller keeps fd 9)"
    else
      local w worker="" how="none" max=0
      w=$(_ccs_find_worker)
      if [ -n "$w" ]; then worker=${w%% *}; how=${w#* }; else max=21600; fi
      _ccs_spawn_holder "$worker" "$max"
      exec 9>&-
      if [ -n "$worker" ]; then
        _ccs_log "took $slot of $k under $root; lock held by pid $_CCS_HOLDER_PID until $how pid $worker exits"
      else
        _ccs_log "took $slot of $k under $root; no Runner.Worker or Runner.Listener ancestor, lock held by pid $_CCS_HOLDER_PID for at most 6 h"
      fi
    fi
  else
    state="$RUNNER_TEMP/ci-cache-slot-cold"
    mkdir -p -- "$state"
    _ccs_log "all $k slots under $root are locked: building cold in $state"
  fi

  # 3. Reap legacy dirs.
  local d name kb rc newer
  _CCS_PROC_SCAN=""
  for d in "$root"/*; do
    [ -e "$d" ] || [ -L "$d" ] || continue
    name=${d##*/}
    case "$name" in slot-*|*.lock) continue ;; esac
    if [ -L "$d" ] || [ ! -d "$d" ]; then
      _ccs_log "legacy: kept $name (not a directory)"
      continue
    fi
    kb=$(_ccs_kb "$d")
    if [ -n "${RUNNER_NAME:-}" ] && [ "$name" = "$RUNNER_NAME" ]; then
      if _ccs_rm "$root" "$d"; then
        _ccs_log "legacy: removed $name, this registration's own dir (${kb:-?} KiB)"
      else
        _ccs_log "legacy: could not remove $name"
      fi
      continue
    fi
    rc=0
    newer=$(find "$d" -maxdepth 3 -newermt '-6 hours' -print -quit 2>/dev/null) || rc=$?
    if [ "$rc" -ne 0 ]; then
      _ccs_log "legacy: kept $name (find exited $rc, its age is UNKNOWN)"
    elif [ -n "$newer" ]; then
      _ccs_log "legacy: kept $name (written within 6 h, a pre-change job may be using it; ${kb:-?} KiB)"
    elif _ccs_in_use "$d"; then
      _ccs_log "legacy: kept-in-use $name ($_CCS_IN_USE_WHY; ${kb:-?} KiB)"
    elif _ccs_rm "$root" "$d"; then
      _ccs_log "legacy: removed $name (nothing newer than 6 h; ${kb:-?} KiB)"
    else
      _ccs_log "legacy: could not remove $name"
    fi
  done
  _ccs_scan_done

  local entry_dir="$state/$entry"
  mkdir -p -- "$entry_dir"

  # 4. Evict across slots.
  local free_kb total_kb=0 sizes="" s
  free_kb=$(_ccs_free_kb "$root")
  for d in "$root"/slot-*; do
    [ -d "$d" ] && [ ! -L "$d" ] || continue
    s=$(_ccs_kb "$d")
    # Unreadable sorts first: it cannot be measured, so it cannot be trusted.
    sizes+="${s:-999999999999} ${d##*/}"$'\n'
    total_kb=$((total_kb + ${s:-0}))
  done
  _ccs_log "slots total $((total_kb / 1048576)) GiB of budget $b_gib GiB; free ${free_kb:-UNKNOWN}${free_kb:+ KiB} (floor $floor_gib GiB); entry bound $((entry_bound_kb / 1048576)) GiB"

  local own_reset=""
  if [ -z "$free_kb" ]; then
    _ccs_log "free space is UNKNOWN: evicting no other slot on a reading that does not exist; this job's entry starts cold"
    own_reset="free space unreadable"
  elif [ "$slot" != "" ] && { [ "$total_kb" -gt "$b_kb" ] || [ "$free_kb" -lt "$floor_kb" ]; }; then
    local efd prev_free
    while read -r s name; do
      [ -n "$name" ] || continue
      [ "$name" != "$slot" ] || continue
      [ "$total_kb" -gt "$b_kb" ] || [ "$free_kb" -lt "$floor_kb" ] || break
      lockf="$root/$name.lock"
      if [ -L "$lockf" ]; then rm -f -- "$lockf"; fi
      exec {efd}>"$lockf"
      # Lock-and-recheck, as for the take path above.
      if flock -n "$efd" && [ ! -L "$lockf" ] && [ "$lockf" -ef "/proc/self/fd/$efd" ]; then
        if _ccs_rm "$root" "$root/$name"; then
          [ "$s" = 999999999999 ] && s=0
          total_kb=$((total_kb - s))
          prev_free=$free_kb
          free_kb=$(_ccs_free_kb "$root")
          # An unreadable re-read must not look like a fuller disk.
          [ -n "$free_kb" ] || free_kb=$((prev_free + s))
          _ccs_log "evicted $name ($((s / 1048576)) GiB): slots total now $((total_kb / 1048576)) GiB, free $((free_kb / 1048576)) GiB"
        else
          _ccs_log "could not evict $name"
        fi
      else
        _ccs_log "kept $name: locked by a running job"
      fi
      exec {efd}>&-
    done < <(printf '%s' "$sizes" | sort -rn)
    if [ "$total_kb" -gt "$b_kb" ] || [ "$free_kb" -lt "$floor_kb" ]; then
      own_reset="slots total $((total_kb / 1048576)) GiB (budget $b_gib), $((free_kb / 1048576)) GiB free (floor $floor_gib) after evicting every unlocked other slot"
    fi
  elif [ "$slot" = "" ] && [ "$free_kb" -lt "$floor_kb" ]; then
    _ccs_log "under the floor with every slot locked: nothing to evict; the cold dir is already empty"
  fi

  # 5. Bound this job's entry.
  local used_kb
  used_kb=$(_ccs_kb "$entry_dir")
  if [ -z "$own_reset" ] && [ -z "$used_kb" ]; then
    own_reset="du could not read $entry_dir"
  elif [ -z "$own_reset" ] && [ "$used_kb" -gt "$entry_bound_kb" ]; then
    own_reset="$((used_kb / 1048576)) GiB used, over its bound of $((entry_bound_kb / 1048576)) GiB"
  fi
  if [ -n "$own_reset" ]; then
    if [ "$state" = "$root/$slot" ]; then
      _ccs_rm "$root" "$entry_dir" || return 1
    else
      chmod -R u+rwX -- "${entry_dir:?}" 2>/dev/null || true
      rm -rf -- "${entry_dir:?}"
    fi
    mkdir -p -- "$entry_dir"
    used_kb=0
    echo "::notice title=persistent target reset::$own_reset: $entry_dir starts cold"
  fi

  # shellcheck disable=SC2034  # outputs read by the sourcing step
  CI_CACHE_STATE_DIR=$state CI_CACHE_SLOT=${slot:-cold} CI_CACHE_ENTRY_DIR=$entry_dir CI_CACHE_ENTRY_USED_KB=${used_kb:-0}
  if [ -n "${GITHUB_ENV:-}" ]; then
    {
      echo "CI_CACHE_STATE_DIR=$CI_CACHE_STATE_DIR"
      echo "CI_CACHE_SLOT=$CI_CACHE_SLOT"
    } >> "$GITHUB_ENV"
  fi
  _ccs_log "state $CI_CACHE_STATE_DIR ($CI_CACHE_SLOT); $entry $((CI_CACHE_ENTRY_USED_KB / 1048576)) GiB already built"
  return 0
}
