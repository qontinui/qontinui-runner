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
# behaviour and says so on one line. Nothing in here can block a push.

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
  for candidate in \
    "${QONTINUI_PREPUSH_CARGO_GUARD:-}" \
    "${QONTINUI_ROOT:+$QONTINUI_ROOT/qontinui-claude-config/scripts/cargo-guard.sh}" \
    "$(dirname "$primary")/qontinui-claude-config/scripts/cargo-guard.sh"
  do
    if [ -n "$candidate" ] && [ -f "$candidate" ] && [ -r "$candidate" ]; then
      guard="$candidate"
      break
    fi
  done
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
