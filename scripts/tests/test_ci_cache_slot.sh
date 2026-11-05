#!/usr/bin/env bash
# Tests for .github/scripts/ci-cache-slot.sh, over a fixture tree in a temp dir.
# No network, no cargo, nothing outside the temp dir is read or written except
# the repo's own .github/workflows/*.yml, which are only grepped.
#
# BYTE-IDENTICAL COPIES, beside the helper under each repo's test convention:
#   qontinui/qontinui-supervisor   .github/scripts/test-ci-cache-slot.sh
#   qontinui/qontinui-runner       scripts/tests/test_ci_cache_slot.sh
#   qontinui/qontinui-schemas      .github/scripts/ci-cache-slot-test.sh
# The helper is found from the repo root, so the same file works in all three.
#
# Linux only, like the helper: it needs flock (util-linux) and GNU find. Where
# flock is missing it SKIPS LOUDLY, and under CI (CI=true) a skip is a failure,
# so a CI run can never pass on a test that did not run.
#
# Run: bash <this file>
# Mutation runs: CI_CACHE_SLOT_SUT=<a mutated copy> bash <this file>
# check() evals its single-quoted condition later, so the variables it names are
# read there: SC2016/SC2034 do not apply. Subshells scope the seams on purpose.
# shellcheck disable=SC2016,SC2030,SC2031,SC2034,SC1090,SC2329
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null || (cd "$here/../.." && pwd))"
SUT="${CI_CACHE_SLOT_SUT:-$repo/.github/scripts/ci-cache-slot.sh}"

if ! command -v flock >/dev/null 2>&1; then
  echo "SKIPPED: flock is not on PATH here ($(uname -s)). ci-cache-slot.sh is Linux-only;"
  echo "SKIPPED: NONE of its tests ran. The ubuntu CI job is the real run."
  if [ -n "${CI:-}" ]; then echo "FAIL - flock missing under CI: refusing to pass a test that did not run"; exit 1; fi
  exit 0
fi

fails=0
cases=0
pass() { cases=$((cases + 1)); printf 'ok   - %s\n' "$1"; }
fail() { cases=$((cases + 1)); printf 'FAIL - %s\n' "$1"; fails=$((fails + 1)); }
check() { if eval "$2"; then pass "$1"; else fail "$1"; fi; }

T="$(mktemp -d)"
holders=()
cleanup() {
  local p
  for p in "${holders[@]}"; do kill "$p" 2>/dev/null; done
  rm -rf "$T"
}
trap cleanup EXIT

check "helper parses (bash -n)" 'bash -n "$SUT"'

# Hold a lock file from another process, as a running job's holder would.
hold() { # lockfile
  # The subshell execs into sleep holding fd 8, so killing that one pid frees
  # the lock; and no stdio is inherited, so a `| tail` reader is not held open.
  ( exec 8>"$1"; flock 8; exec sleep 600 ) </dev/null >/dev/null 2>&1 &
  holders+=("$!")
  local i
  for _ in $(seq 1 50); do
    if ! flock -n "$1" true 2>/dev/null; then return 0; fi
    sleep 0.1
  done
  echo "could not establish a holder on $1" >&2
  return 1
}
blob() { mkdir -p "$(dirname "$1")"; head -c "$(($2 * 1024))" /dev/zero > "$1"; }

# Runs the helper in a subshell with test seams; prints its log, then the
# outputs as KEY=VALUE lines.
run_select() { # root [extra select args...]
  local root="$1"; shift
  (
    set -euo pipefail
    export RUNNER_TEMP="$T/runner-temp" CI_CACHE_SLOT_NO_HOLDER=1
    export CI_CACHE_SLOT_FREE_KB="${FREE_KB:-999999999}"
    unset GITHUB_ENV
    mkdir -p "$RUNNER_TEMP"
    . "$SUT"
    # Bare, never on the left of || or &&: either would switch errexit off
    # inside the whole function, and the helper would run without -e.
    ci_cache_slot_select --root "$root" --budget-gib 32 --entry-bound-gib 40 "$@"
    echo "OUT_SLOT=$CI_CACHE_SLOT"
    echo "OUT_STATE=$CI_CACHE_STATE_DIR"
    echo "OUT_ENTRY=$CI_CACHE_ENTRY_DIR"
  )
}
out_of() { printf '%s\n' "$1" | sed -n "s/^$2=//p"; }

# --- (i) no persistent cache path in the workflows is keyed by RUNNER_NAME ---
bad=$(grep -nE '/\$\{?RUNNER_NAME|\$\{?RUNNER_NAME\}?/|\.cache[^ ]*RUNNER_NAME' "$repo"/.github/workflows/*.yml 2>/dev/null || true)
check "(i) no workflow path contains \$RUNNER_NAME${bad:+: $bad}" '[ -z "$bad" ]'
uses=$(grep -lE 'ci-cache-slot\.sh' "$repo"/.github/workflows/*.yml 2>/dev/null | wc -l)
check "(i) at least one workflow sources ci-cache-slot.sh (found $uses)" '[ "$uses" -ge 1 ]'
# The grep itself must catch the old spelling, or (i) passes vacuously.
printf '%s\n' '          state="$pool_home/.cache/qontinui-supervisor-ci/$RUNNER_NAME"' \
  '          cache="$CI_POOL_HOME/.cache/qontinui-runner-ci/$RUNNER_NAME/target"' > "$T/old.yml"
n=$(grep -cE '/\$\{?RUNNER_NAME|\$\{?RUNNER_NAME\}?/|\.cache[^ ]*RUNNER_NAME' "$T/old.yml")
check "(i) the grep catches both pre-change spellings (matched $n of 2)" '[ "$n" = 2 ]'

# --- (ii) a free slot is taken, a locked one skipped ---
R="$T/ii"; mkdir -p "$R"
hold "$R/slot-0.lock"
o=$(run_select "$R"); rc=$?
check "(ii) select exits 0 (rc=$rc)" '[ "$rc" = 0 ]'
check "(ii) skips locked slot-0 and takes slot-1 (got $(out_of "$o" OUT_SLOT))" '[ "$(out_of "$o" OUT_SLOT)" = slot-1 ]'
check "(ii) state is \$root/slot-1" '[ "$(out_of "$o" OUT_STATE)" = "$R/slot-1" ]'
check "(ii) the entry dir exists under the slot" '[ -d "$R/slot-1/target" ]'
check "(ii) lock files live beside the slots, not inside" '[ -f "$R/slot-1.lock" ] && [ ! -e "$R/slot-1/slot-1.lock" ]'
o=$(run_select "$R")
check "(ii) a slot released by its job is free again (got $(out_of "$o" OUT_SLOT))" '[ "$(out_of "$o" OUT_SLOT)" = slot-1 ]'

# --- (iii) every slot locked: a cold dir under RUNNER_TEMP ---
R="$T/iii"; mkdir -p "$R"
hold "$R/slot-0.lock"; hold "$R/slot-1.lock"
o=$(CI_CACHE_SLOTS=2 run_select "$R")
check "(iii) all slots locked -> cold (got $(out_of "$o" OUT_SLOT))" '[ "$(out_of "$o" OUT_SLOT)" = cold ]'
case "$(out_of "$o" OUT_STATE)" in "$T/runner-temp/"*) st=yes ;; *) st=no ;; esac
check "(iii) cold state is under RUNNER_TEMP ($(out_of "$o" OUT_STATE))" '[ "$st" = yes ]'
check "(iii) no slot-2 was created past K=2" '[ ! -e "$R/slot-2" ]'

# --- (iv) eviction: unlocked others first, largest first, never a locked one ---
R="$T/iv"; mkdir -p "$R"
blob "$R/slot-0/target/a" 300    # locked by another job
blob "$R/slot-1/rustup/a" 200    # this job's slot (first free); rustup, not the entry
blob "$R/slot-2/target/a" 500    # unlocked, largest
blob "$R/slot-3/target/a" 100    # unlocked, smallest
hold "$R/slot-0.lock"
o=$(CI_CACHE_SLOT_BUDGET_KB=750 run_select "$R")
check "(iv) own slot is slot-1 (got $(out_of "$o" OUT_SLOT))" '[ "$(out_of "$o" OUT_SLOT)" = slot-1 ]'
check "(iv) over budget: the largest unlocked other (slot-2) is evicted" '[ ! -e "$R/slot-2" ]'
check "(iv) eviction stops once under budget: slot-3 kept" '[ -f "$R/slot-3/target/a" ]'
check "(iv) the locked slot-0 is never evicted" '[ -f "$R/slot-0/target/a" ]'
check "(iv) own slot untouched when others sufficed" '[ -f "$R/slot-1/rustup/a" ]'
check "(iv) slot-2's lock file survives its eviction" '[ -f "$R/slot-2.lock" ]'

R="$T/iv2"; mkdir -p "$R"
blob "$R/slot-0/target/a" 300
blob "$R/slot-1/target/a" 20     # own entry
blob "$R/slot-2/target/a" 500
blob "$R/slot-3/target/a" 100
hold "$R/slot-0.lock"
o=$(CI_CACHE_SLOT_BUDGET_KB=50 run_select "$R")
check "(iv) far over budget: every unlocked other evicted" '[ ! -e "$R/slot-2" ] && [ ! -e "$R/slot-3" ]'
check "(iv) ...but never the locked one" '[ -f "$R/slot-0/target/a" ]'
check "(iv) then its own entry is reset" '[ -d "$R/slot-1/target" ] && [ ! -e "$R/slot-1/target/a" ]'
l3=$(printf '%s\n' "$o" | grep -n 'evicted slot-3' | cut -d: -f1)
lo=$(printf '%s\n' "$o" | grep -n 'persistent target reset' | cut -d: -f1)
check "(iv) others are evicted BEFORE its own reset (lines ${l3:-none} < ${lo:-none})" '[ -n "$l3" ] && [ -n "$lo" ] && [ "$l3" -lt "$lo" ]'
check "(iv) the locked slot is logged as kept" 'printf "%s\n" "$o" | grep -q "kept slot-0: locked"'

R="$T/iv3"; mkdir -p "$R"
blob "$R/slot-1/target/a" 100
o=$(FREE_KB=10 run_select "$R")
check "(iv) under the free-space floor: an unlocked other is evicted" '[ ! -e "$R/slot-1" ]'

R="$T/iv4"; mkdir -p "$R"
blob "$R/slot-1/target/a" 100
blob "$R/slot-0/target/a" 100
o=$(FREE_KB=notanumber run_select "$R")
check "(iv) free space UNKNOWN: no other slot is evicted" '[ -f "$R/slot-1/target/a" ]'
check "(iv) free space UNKNOWN: its own entry starts cold" '[ ! -e "$R/slot-0/target/a" ]'

R="$T/iv5"; mkdir -p "$R"
blob "$R/slot-0/target/a" 300
o=$(CI_CACHE_SLOTS=4 CI_CACHE_SLOT_BUDGET_KB=1000 run_select "$R")
check "(iv) entry bound is min(entry bound, B/K): 300K > 1000K/4 resets it" '[ ! -e "$R/slot-0/target/a" ]'

# --- (v) legacy reap ---
R="$T/v"; mkdir -p "$R"
blob "$R/nawa-org-1/target/a" 10            # this registration's own: fresh, still removed
blob "$R/nawa-org-2/target/debug/a" 10      # another, old
blob "$R/nawa-org-3/target/a" 10            # another, fresh
blob "$R/slot-7/target/a" 10                # a slot outside 0..K-1, old
blob "$T/outside/keep" 1
ln -s "$T/outside" "$R/a-link"
: > "$R/stray.lock"
find "$R/nawa-org-2" "$R/slot-7" -depth -exec touch -d '2 days ago' {} +
# Only the TOP of nawa-org-3 is old; a file 2 levels down is fresh.
touch -d '2 days ago' "$R/nawa-org-3" "$R/nawa-org-3/target"
o=$(RUNNER_NAME=nawa-org-1 run_select "$R")
check "(v) own-name legacy dir removed (even though fresh)" '[ ! -e "$R/nawa-org-1" ]'
check "(v) old legacy dir removed" '[ ! -e "$R/nawa-org-2" ]'
check "(v) fresh legacy dir kept" '[ -f "$R/nawa-org-3/target/a" ]'
check "(v) slot-* never reaped as legacy" '[ -f "$R/slot-7/target/a" ]'
check "(v) a *.lock file is kept" '[ -f "$R/stray.lock" ]'
check "(v) a symlink is kept and its target untouched" '[ -L "$R/a-link" ] && [ -f "$T/outside/keep" ]'
check "(v) each reap is logged" 'printf "%s\n" "$o" | grep -q "legacy: removed nawa-org-1" && printf "%s\n" "$o" | grep -q "legacy: removed nawa-org-2" && printf "%s\n" "$o" | grep -q "legacy: kept nawa-org-3"'

# An old legacy dir that a live process is using (cwd or an open fd) is kept,
# by both scans: GNU find (the default) and the readlink loop (no GNU find).
for mode in find loop; do
  R="$T/v-use-$mode"; mkdir -p "$R"
  blob "$R/nawa-org-4/target/a" 10            # old, but a process's cwd is inside
  blob "$R/nawa-org-5/target/a" 10            # old, an open fd inside
  blob "$R/nawa-org-6/target/a" 10            # old, unused: the control
  ( cd "$R/nawa-org-4/target" && exec sleep 600 ) </dev/null >/dev/null 2>&1 &
  holders+=("$!")
  ( exec sleep 600 ) <"$R/nawa-org-5/target/a" >/dev/null 2>&1 &
  holders+=("$!")
  sleep 0.3
  find "$R" -mindepth 1 -depth -exec touch -d '2 days ago' {} +
  scan=""; [ "$mode" = loop ] && scan=loop
  left_before=$(find "${TMPDIR:-/tmp}" -maxdepth 1 -name 'ci-cache-slot-proc.*' 2>/dev/null | wc -l)
  o=$(CI_CACHE_SLOT_SCAN=$scan run_select "$R"); rc=$?
  left_after=$(find "${TMPDIR:-/tmp}" -maxdepth 1 -name 'ci-cache-slot-proc.*' 2>/dev/null | wc -l)
  check "(v/$mode) select exits 0 (rc=$rc)" '[ "$rc" = 0 ]'
  check "(v/$mode) an old legacy dir that is some process's cwd is kept" '[ -f "$R/nawa-org-4/target/a" ]'
  check "(v/$mode) an old legacy dir holding a process's open fd is kept" '[ -f "$R/nawa-org-5/target/a" ]'
  check "(v/$mode) ...while an old unused one beside them is removed" '[ ! -e "$R/nawa-org-6" ]'
  check "(v/$mode) the in-use keep is logged with its pid" 'printf "%s\n" "$o" | grep -qE "legacy: kept-in-use nawa-org-4 \(in use: pid [0-9]+ has "'
  check "(v/$mode) the scan's temp file is removed ($left_before before, $left_after after)" '[ "$left_after" -le "$left_before" ]'
done

# A /proc that cannot be scanned at all keeps every legacy dir (fail closed).
R="$T/v-noproc"; mkdir -p "$R"
blob "$R/nawa-org-7/target/a" 10
find "$R" -mindepth 1 -depth -exec touch -d '2 days ago' {} +
o=$(CI_CACHE_SLOT_PROC_ROOT="$T/no-such-proc" run_select "$R")
check "(v) an unscannable /proc keeps an old legacy dir" '[ -f "$R/nawa-org-7/target/a" ]'
check "(v) ...and says its use is UNKNOWN" 'printf "%s\n" "$o" | grep -q "legacy: kept-in-use nawa-org-7 (its use is UNKNOWN"'

# --- a symlinked slot never carries a removal outside the root ---
R="$T/s3"; mkdir -p "$R"
blob "$T/s3-out/target/a" 10
ln -s "$T/s3-out" "$R/slot-0"
o=$(CI_CACHE_SLOT_BUDGET_KB=1 run_select "$R"); rc=$?
check "(symlink) select exits 0 over a symlinked slot (rc=$rc)" '[ "$rc" = 0 ]'
check "(symlink) the slot is now a real dir, not a link" '[ -d "$R/slot-0" ] && [ ! -L "$R/slot-0" ]'
check "(symlink) the old link's target is untouched by its own-entry reset" '[ -f "$T/s3-out/target/a" ]'

# --- a symlinked lock file never truncates its target ---
R="$T/sl"; mkdir -p "$R" "$T/sl-out"
printf 'precious\n' > "$T/sl-out/take"
printf 'precious\n' > "$T/sl-out/evict"
ln -s "$T/sl-out/take" "$R/slot-0.lock"
blob "$R/slot-1/target/a" 100
ln -s "$T/sl-out/evict" "$R/slot-1.lock"
o=$(CI_CACHE_SLOT_BUDGET_KB=50 run_select "$R"); rc=$?
check "(lock-symlink) select exits 0 over symlinked locks (rc=$rc)" '[ "$rc" = 0 ]'
check "(lock-symlink) the take path left the outside file's contents untouched" '[ "$(cat "$T/sl-out/take")" = precious ]'
check "(lock-symlink) the evict path left the outside file's contents untouched" '[ "$(cat "$T/sl-out/evict")" = precious ]'
check "(lock-symlink) both locks are now real files" '[ -f "$R/slot-0.lock" ] && [ ! -L "$R/slot-0.lock" ] && [ -f "$R/slot-1.lock" ] && [ ! -L "$R/slot-1.lock" ]'
check "(lock-symlink) the other slot was still evicted" '[ ! -e "$R/slot-1" ]'

# --- removal guard ---
R="$T/g"; mkdir -p "$R/x" "$T/g-out"
g=$(
  . "$SUT"
  for p in "$R" "$R/" / "$R/../g-out" "$T/g-out" ""; do
    if _ccs_rm "$R" "$p" 2>/dev/null; then echo "REMOVED:$p"; fi
  done
)
check "(guard) refuses the root, /, a .. path, an outside path and empty${g:+ (removed: $g)}" '[ -z "$g" ] && [ -d "$R" ] && [ -d "$T/g-out" ]'
R="$T/g2"; mkdir -p "$R"
blob "$T/g2-out/target/a" 1
ln -s "$T/g2-out" "$R/link"
g=$( . "$SUT"; if _ccs_rm "$R" "$R/link/target" 2>/dev/null; then echo REMOVED; fi )
check "(guard) refuses a path through a symlinked component${g:+ ($g)}" '[ -z "$g" ] && [ -f "$T/g2-out/target/a" ]'
( . "$SUT"; _ccs_rm "$R" "$R/link" ) 2>/dev/null
check "(guard) a symlink as the last component is unlinked, its target kept" '[ ! -L "$R/link" ] && [ -f "$T/g2-out/target/a" ]'
check "(guard) a bad --root is refused" '! (RUNNER_TEMP=$T . "$SUT"; ci_cache_slot_select --root / --budget-gib 1 --entry-bound-gib 1) >/dev/null 2>&1 && ! (RUNNER_TEMP=$T . "$SUT"; ci_cache_slot_select --root rel/path --budget-gib 1 --entry-bound-gib 1) >/dev/null 2>&1'

# --- the Runner.Worker walk ---
P="$T/proc"
mkstat() { mkdir -p "$P/$1"; printf '%s (%s) S %s 1 1 0 -1\n' "$1" "$2" "$3" > "$P/$1/stat"; }
mkstat 100 bash 90; mkstat 90 'Runner.Worker' 80; mkstat 80 'Runner.Listener' 1
mkstat 200 bash 185; mkstat 185 'odd) name (x' 180; mkstat 180 'Runner.Listener' 1
mkstat 300 bash 290; mkstat 290 sshd 1
walk() { ( . "$SUT"; CI_CACHE_SLOT_PROC_ROOT=$P CI_CACHE_SLOT_START_PID=$1 _ccs_find_worker ); }
check "(worker) finds the Runner.Worker ancestor (got '$(walk 100)')" '[ "$(walk 100)" = "90 Runner.Worker" ]'
check "(worker) else the ancestor below Runner.Listener, parens in comm (got '$(walk 200)')" '[ "$(walk 200)" = "185 below-Runner.Listener" ]'
check "(worker) else nothing (got '$(walk 300)')" '[ -z "$(walk 300)" ]'

# --- the holder keeps the lock for the job, and releases it when the worker ends ---
R="$T/h"; mkdir -p "$R"
( exec sleep 600 ) </dev/null >/dev/null 2>&1 & worker=$!; holders+=("$worker")
o=$(
  set -euo pipefail
  export RUNNER_TEMP="$T/runner-temp" CI_CACHE_SLOT_FREE_KB=999999999 \
    CI_CACHE_SLOT_WORKER_PID=$worker CI_CACHE_SLOT_HOLDER_POLL_SECS=1
  unset GITHUB_ENV
  . "$SUT"
  ci_cache_slot_select --root "$R" --budget-gib 32 --entry-bound-gib 40
  if { true >&9; } 2>/dev/null; then echo "FD9_STILL_OPEN"; fi
)
check "(holder) the step shell closes fd 9 after handing it off" '! printf "%s\n" "$o" | grep -q FD9_STILL_OPEN'
if flock -n "$R/slot-0.lock" true; then held=no; else held=yes; fi
check "(holder) slot-0 stays locked after the step shell exits" '[ "$held" = yes ]'
kill "$worker"; wait "$worker" 2>/dev/null
freed=no
for _ in $(seq 1 50); do
  if flock -n "$R/slot-0.lock" true; then freed=yes; break; fi
  sleep 0.2
done
check "(holder) the lock is released once the worker exits" '[ "$freed" = yes ]'

echo
echo "$((cases - fails))/$cases passed"
# An explicit exit: falling off the end would let the EXIT trap's last command
# decide the status, and a red run would exit 0.
if [ "$fails" -eq 0 ]; then exit 0; fi
exit 1
