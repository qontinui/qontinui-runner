#!/usr/bin/env bash
#
# shared-target.sh — "which target dir should a pre-push build write into?"
#
# Sourced by `.pre-commit-hooks/cargo-prepush.sh` and by
# `.pre-commit-hooks/gen-events-drift.sh`. Split into its own neutral file for
# the same reason `lib/push-range.sh` was: BOTH pre-push hooks build Rust, and
# an answer that lives inside one of them does not reach the other.
#
# THE PROBLEM THIS SOLVES
#
# With no `CARGO_TARGET_DIR`, cargo builds into `<checkout>/target`. In the
# primary checkout that is the warm target every build uses. In a linked
# worktree (every coord-allocated agent worktree) it is a brand-new directory:
# a cold build of the whole dependency tree, measured at 4.9-5.2 GB per push
# (findings 33d6f2d8, f896b13f), and slow enough that sessions read the silent
# hook as a credential hang and skip it.
#
# WHY IT IS A LIBRARY AND NOT A LINE IN ONE HOOK
#
# plan 2026-09-15-runner-prepush-cargo-gate-fires-on-markdown-only-diffs-and-
# builds-a-cold-per-worktree-target (qontinui-runner#1556) put this resolution
# INSIDE `cargo-prepush.sh`, which exports the variable into that script's own
# process only. `gen-events-drift.sh` is a SEPARATE hook process — the pre-push
# shim runs it with its own `bash` — so it never saw the export and kept
# building cold. Its own chain is:
#
#   gen-events-drift.sh -> src-tauri/scripts/generate_types.sh
#                       -> `cargo build --bin export_schemas --release`
#
# and `generate_types.sh:71` resolves its output dir from an INHERITED
# `CARGO_TARGET_DIR` (then `CARGO_BUILD_TARGET_DIR`, then `<crate>/target`) and
# never resolves one itself. That is the right contract for that script — it is
# also run by CI and by hand, where `cargo-guard.sh` does not exist — so the
# fix belongs in its CALLER, which is this library's second consumer.
#
# THE DELIBERATE OMISSION, mirroring `push-range.sh`'s
#
# `resolve_shared_target` takes no position on what a caller prints. It accepts
# the caller's log prefix as `$2` so `[pre-push]` and `[gen-events-drift]`
# framing both come out right, and it NEVER fails: every miss keeps today's
# behaviour and says so on one line. Nothing in `resolve_shared_target` can
# block a push. (`guard_shared_target`, below, can make a caller DECLINE its
# build; whether that blocks is the caller's QONTINUI_*_STRICT switch.)

# _shared_target_ccfg_file <primary checkout> <path under scripts/>
#
# Print the first readable `qontinui-claude-config/scripts/<path>`, looked for
# under `$QONTINUI_ROOT` and then beside the primary checkout, or return 1.
#
# The primary is derived from the git COMMON dir, never from this file's own
# location: a hook may run from a copy extracted out of the pushed commit
# (plan 2026-10-18-repo-pre-push-hooks-resolve-from-the-pushed-commit-not-the-
# working-tree), where `SCRIPT_DIR` is a temp directory with no siblings.
_shared_target_ccfg_file() {
  local primary="$1" rel="$2" base
  for base in \
    "${QONTINUI_ROOT:+$QONTINUI_ROOT/qontinui-claude-config/scripts}" \
    "$(dirname "$primary")/qontinui-claude-config/scripts"
  do
    if [ -n "$base" ] && [ -f "$base/$rel" ] && [ -r "$base/$rel" ]; then
      printf '%s\n' "$base/$rel"
      return 0
    fi
  done
  return 1
}

# Resolve the shared cargo target for the repo at $1 and export CARGO_TARGET_DIR.
#
# $1 — repo root (the worktree being pushed from)
# $2 — log prefix, e.g. "[pre-push]" (defaults to "[pre-push]")
#
# Always returns 0. Exports CARGO_TARGET_DIR only when it resolved one, and is
# a silent no-op in the primary checkout, which already builds into its own
# warm target.
#
# ⚠️ BORROW THE TARGET DIR; DO NOT RUN CARGO THROUGH THE GUARD. The guard
# injects `--all-targets` into a `clippy` that names no target, which compiles
# #[cfg(test)] code — exactly what `cargo-prepush.sh`'s T1 comment rejects for
# that gate. Concurrency is still safe: cargo takes its own file lock on the
# target directory and prints "Blocking waiting for file lock" while it waits,
# which is visible output rather than silence.
#
# `cargo-guard.sh` lives in qontinui-claude-config, not in this repo, so an
# open-source checkout or CI has none. A caller-set CARGO_TARGET_DIR always
# wins, and the primary checkout is never touched.
resolve_shared_target() {
  local root="$1" tag="${2:-[pre-push]}"
  local git_dir common_dir primary guard candidate out target line

  if ! git_dir="$(git -C "$root" rev-parse --path-format=absolute --git-dir 2>/dev/null)" \
     || ! common_dir="$(git -C "$root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" \
     || [ -z "$git_dir" ] || [ -z "$common_dir" ]; then
    echo "$tag could not tell whether this is a linked worktree (git >= 2.31 needed) — building into this checkout's own target/"
    return 0
  fi
  # The primary checkout already builds into its own warm target: nothing to say.
  [ "$git_dir" != "$common_dir" ] || return 0

  if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    echo "$tag linked worktree — using the caller's CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
    return 0
  fi

  primary="$(dirname "$common_dir")"
  guard=""
  candidate="${QONTINUI_PREPUSH_CARGO_GUARD:-}"
  if [ -n "$candidate" ] && [ -f "$candidate" ] && [ -r "$candidate" ]; then
    guard="$candidate"
  else
    guard="$(_shared_target_ccfg_file "$primary" cargo-guard.sh)" || guard=""
  fi
  if [ -z "$guard" ]; then
    echo "$tag linked worktree, but no cargo-guard.sh to resolve the shared target — building into this worktree's own target/"
    return 0
  fi

  if ! out="$(cd "$root" && CARGO_GUARD_RESOLVE_ONLY=1 bash "$guard" check 2>/dev/null)"; then
    echo "$tag linked worktree, but $guard could not resolve a target — building into this worktree's own target/"
    return 0
  fi
  # A plain loop, not `… | sed | head`. Both callers run under `set -o pipefail`,
  # so `head` exiting early would SIGPIPE `sed`, make the substitution non-zero,
  # and the `|| target=""` would then discard a value that was read correctly —
  # silently degrading to a cold build. No pipeline, no hazard.
  target=""
  while IFS= read -r line; do
    case "$line" in
      TARGET_DIR=*) target="${line#TARGET_DIR=}"; break ;;
    esac
  done <<EOF
$out
EOF
  if [ -z "$target" ]; then
    echo "$tag linked worktree, but $guard printed no TARGET_DIR — building into this worktree's own target/"
    return 0
  fi
  export CARGO_TARGET_DIR="$target"
  echo "$tag linked worktree — reusing the shared target CARGO_TARGET_DIR=$target"
}

# ── Guarding the build: the tree lock and the freshness ledger ───────────────
#
# THE DEFECT (coord finding 7b603cde; plan 2026-10-29-runner-prepush-gate-
# builds-the-shared-target-without-the-tree-lock). A path package's cargo
# fingerprint is mtime-based and every worktree of this repo builds the SAME
# unit names, so a target last built from a PEER worktree serves that peer's
# units as `Fresh` whenever this tree's files are older than the peer's build.
# On 2026-10-29 that made this hook's clippy report
# `error[E0432]: unresolved import qontinui_runner_win32::process_tree` for a
# module that existed in the pushed tree, and the push went out under
# QONTINUI_PREPUSH_SKIP=1.
#
# Sharing the target is not the bug and resolving it differently does not fix
# it: `target-agent/` is shared by every linked worktree, and a caller-set
# CARGO_TARGET_DIR may name anything. What fixes it is GUARDING whatever target
# the build uses, with the same library `cargo-verify.sh` uses for the same
# defect: `qontinui-claude-config/scripts/lib/cargo-tree-freshness.sh`, a
# per-target tree lock plus a ledger that touches this tree's files whenever
# the target was last built from another checkout. Reused, not re-written.
#
# ⚠️ WHAT IT DOES NOT COVER. The ledger only knows builds that take the lock.
# A build that does not — `cargo-guard.sh` until qontinui-claude-config#1823
# wires the same library into it, rust-analyzer, a raw `cargo` — can rebuild
# a peer's units into the target without moving the ledger. A later guarded
# run from the checkout the ledger already names then takes the library's
# same-checkout arm, touches nothing, and can still be served those units.
# So a guarded run narrows the false verdict to that residual; it does not
# remove it, and the hooks' compile-error message says so.
#
# Return codes of guard_shared_target:
#   0   guarded — the lock is held and this tree's units will be re-judged
#   1   the lock wait timed out (QONTINUI_PREPUSH_TREE_LOCK_MAX_WAIT, 300 s —
#       a push is interactive; declining costs latency, never coverage)
#   2   the lock could not be taken, the library failed to load, or cargo
#       could not say which target it will use
#   3   this environment claims the lock but no holder proves it (a PEER's lock)
#   4   a touch was needed and failed (the lock is released before returning)
#   10  unguarded — no tree-lock library on this machine; the caller builds as
#       before, and a NOTE says what that costs
# On 1-4 the caller must NOT build unguarded: that build is exactly the false
# verdict this exists to remove. SHARED_TARGET_GUARD_REASON names the cause.
#
# The library is found through QONTINUI_PREPUSH_TREE_GUARD_LIB_DIR (a directory
# holding cargo-tree-freshness.sh and tree-identity.sh; the tests use it), else
# beside cargo-guard.sh in qontinui-claude-config. An open-source checkout or
# CI has none, and has no peer sharing its target either, so rc 10 there costs
# nothing a machine actually has.
#
# Sourcing tree-identity.sh strips GIT_DIR / GIT_WORK_TREE / GIT_COMMON_DIR
# process-wide. Both callers already run with an inherited git scope cleared or
# irrelevant (gen-events-drift.sh clears it before its first git call;
# cargo-prepush.sh guards after its last scope-sensitive one).
SHARED_TARGET_GUARD_REASON=""
_SHARED_TARGET_CTF_LOADED=0

# Print the target directory cargo will build into when run from $1 — the
# directory the CALLER's build runs in, because cargo reads `.cargo/config.toml`
# upward from there, so asking from anywhere else could name another target —
# as cargo itself resolves it (CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR,
# .cargo/config, then <workspace>/target): no second resolver here to drift
# from cargo's. Returns 1 when cargo cannot say, with cargo's first `error`
# line (else its first line) in _SHARED_TARGET_META_ERR. Called directly
# (never in `$( )`) so that variable survives; the answer is in
# _SHARED_TARGET_DIR.
_SHARED_TARGET_META_ERR=""
_SHARED_TARGET_DIR=""
_shared_target_dir_of() {
  local dir_from="$1" meta dir errf bs='\'
  local re='"target_directory":"([^"]*)"'
  _SHARED_TARGET_META_ERR=""
  _SHARED_TARGET_DIR=""
  errf="$(mktemp)" || return 1
  if ! meta="$(cd "$dir_from" && cargo metadata --color never --no-deps --format-version 1 2>"$errf")"; then
    # The first `error` line, not the first line: cargo can warn first.
    _SHARED_TARGET_META_ERR="$({ grep -m1 '^error' "$errf" || grep -m1 . "$errf"; } 2>/dev/null || true)"
    rm -f "$errf"
    return 1
  fi
  rm -f "$errf"
  [[ $meta =~ $re ]] || return 1
  dir="${BASH_REMATCH[1]}"
  # JSON escapes a Windows path's backslashes. Pattern and replacement are
  # QUOTED: unquoted, `$bs$bs` is a glob matching ONE backslash, so the
  # substitution would replace each backslash with itself.
  dir="${dir//"$bs$bs"/"$bs"}"
  [ -n "$dir" ] || return 1
  if command -v cygpath >/dev/null 2>&1; then
    dir="$(cygpath -u "$dir" 2>/dev/null)" || return 1
  fi
  _SHARED_TARGET_DIR="$dir"
}

# guard_shared_target <repo-root> <log prefix> [<dir cargo runs in>] — see the
# block above. The third argument defaults to the repo root; both hooks build
# from `src-tauri/` and pass that.
guard_shared_target() {
  local root="$1" tag="${2:-[pre-push]}" cargo_dir="${3:-$1}"
  local max_wait="${QONTINUI_PREPUSH_TREE_LOCK_MAX_WAIT:-300}"
  local common_dir libdir found target fn rc=0
  SHARED_TARGET_GUARD_REASON=""

  libdir="${QONTINUI_PREPUSH_TREE_GUARD_LIB_DIR:-}"
  if [ -z "$libdir" ]; then
    common_dir="$(git -C "$root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || common_dir=""
    if [ -n "$common_dir" ] \
       && found="$(_shared_target_ccfg_file "$(dirname "$common_dir")" lib/cargo-tree-freshness.sh)"; then
      libdir="$(dirname "$found")"
    fi
  fi
  if [ -z "$libdir" ] || [ ! -r "$libdir/cargo-tree-freshness.sh" ] || [ ! -r "$libdir/tree-identity.sh" ]; then
    echo "$tag NOTE — no qontinui-claude-config tree-lock library (lib/cargo-tree-freshness.sh) on this machine, so this build is UNGUARDED."
    echo "$tag        If another checkout shares this target, cargo can replay ITS build as Fresh: an error naming code that exists in this tree may not be yours."
    return 10
  fi

  # `|| rc=$?`: both libraries were written for a caller WITHOUT `set -e`
  # (cargo-verify.sh), and tree-identity.sh refuses with `return 2`, which
  # would otherwise abort the calling hook outright.
  # shellcheck source=/dev/null
  . "$libdir/tree-identity.sh" || rc=$?
  if [ "$rc" -eq 0 ]; then
    # shellcheck source=/dev/null
    . "$libdir/cargo-tree-freshness.sh" || rc=$?
  fi
  if [ "$rc" -ne 0 ]; then
    SHARED_TARGET_GUARD_REASON="the tree-lock library in $libdir failed to load (rc $rc)"
    return 2
  fi
  for fn in ctf_lock_acquire ctf_lock_release ctf_refresh tree_identity_line; do
    if ! declare -F "$fn" >/dev/null 2>&1; then
      echo "$tag NOTE — $libdir defines no $fn (a stale or partial qontinui-claude-config), so this build is UNGUARDED."
      echo "$tag        If another checkout shares this target, cargo can replay ITS build as Fresh: an error naming code that exists in this tree may not be yours."
      return 10
    fi
  done
  _SHARED_TARGET_CTF_LOADED=1

  if ! _shared_target_dir_of "$cargo_dir"; then
    SHARED_TARGET_GUARD_REASON="'cargo metadata' could not say which target directory this build uses${_SHARED_TARGET_META_ERR:+ ($_SHARED_TARGET_META_ERR)}"
    return 2
  fi
  target="$_SHARED_TARGET_DIR"

  # A signal must still run the caller's EXIT trap, which releases the lock.
  trap 'exit 130' INT
  trap 'exit 143' TERM
  CTF_HOLDER_ROOT="$root"
  # pre-commit shows a hook's output only once the hook ends, so the library's
  # "waiting for the tree lock" line is invisible WHILE it waits — and a silent
  # push is what gets skipped as a hang. Say it on the terminal as well — only
  # when stderr is NOT already the terminal (i.e. something is capturing it),
  # and never under QONTINUI_PREPUSH_NO_TTY=1 (the test suites set it), from
  # Claude Code, or from any runner-spawned pane (QONTINUI_RUNNER_CONTEXT is
  # set there for a human's shell too): there the waiter is usually an agent,
  # which would never see the line, while the terminal is the operator's.
  # A headless push has no terminal and loses nothing.
  if [ "${QONTINUI_PREPUSH_NO_TTY:-0}" != 1 ] && [ -z "${QONTINUI_RUNNER_CONTEXT:-}" ] && [ "${CLAUDECODE:-}" != 1 ] && [ ! -t 2 ]; then
    { printf '%s taking the tree lock on %s (waits up to %ss if a peer build holds it)\n' \
        "$tag" "$target" "$max_wait" > /dev/tty; } 2>/dev/null || :
  fi
  rc=0
  CARGO_VERIFY_TREE_LOCK_MAX_WAIT="$max_wait" ctf_lock_acquire "$target" "$tag" || rc=$?
  case "$rc" in
    0) ;;
    1) SHARED_TARGET_GUARD_REASON="timed out after ${max_wait}s waiting for the tree lock on $target"; return 1 ;;
    3) SHARED_TARGET_GUARD_REASON="this environment claims the tree lock on $target, but the lock is held by a holder the claim does not prove"; return 3 ;;
    *) SHARED_TARGET_GUARD_REASON="the tree lock on $target could not be taken (rc $rc)"; return 2 ;;
  esac
  echo "$tag building under the tree lock on $target"

  rc=0
  ctf_refresh "$root" "$target" "$tag" || rc=$?
  if [ "$rc" -ne 0 ]; then
    release_shared_target
    SHARED_TARGET_GUARD_REASON="this tree's files could not be touched, so cargo may still judge them against another checkout's build"
    return 4
  fi
  return 0
}

# release_shared_target — idempotent, always returns 0, safe from an EXIT trap
# whether or not guard_shared_target ever ran or got as far as the lock (an
# interrupted wait leaves a waiter or a prep dir that the release cleans).
release_shared_target() {
  if [ "$_SHARED_TARGET_CTF_LOADED" = "1" ] && declare -F ctf_lock_release >/dev/null 2>&1; then
    ctf_lock_release || :
  fi
  return 0
}
