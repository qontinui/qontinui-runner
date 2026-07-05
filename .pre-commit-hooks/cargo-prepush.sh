#!/usr/bin/env bash
# Pre-push guard: mirrors CI's Rust gate so a correctness failure is surfaced
# in 60-90s of local cycle time instead of 8-15 min of CI roundtrip per push.
# Tiered per plan 2026-07-05-lint-tiers-and-diff-scoped-gates: formatting
# auto-applies (never blocks), and clippy runs plain (no `-D warnings`) so only
# deny-tier (correctness/suspicious) lints from Cargo.toml [lints.clippy] block.
#
# Skip with `git push --no-verify` if you genuinely want CI to be the
# source of truth (e.g. you're pushing a WIP branch you don't expect to
# pass yet).
#
# Set QONTINUI_PREPUSH_SKIP=1 to bypass without `--no-verify` (useful
# when iterating on docs-only changes that don't touch src-tauri/).
set -euo pipefail

if [[ "${QONTINUI_PREPUSH_SKIP:-}" == "1" ]]; then
  echo "[pre-push] QONTINUI_PREPUSH_SKIP=1 — skipping cargo fmt + clippy"
  exit 0
fi

# If nothing under src-tauri/ changed since the last push to this remote,
# skip the cargo gate. Conservative fallback: when we can't determine the
# upstream merge-base (first push, detached, etc.), run the gate.
if remote_ref=$(git rev-parse --symbolic-full-name @{u} 2>/dev/null); then
  if base=$(git merge-base "$remote_ref" HEAD 2>/dev/null); then
    if ! git diff --name-only "$base"..HEAD -- src-tauri/ | grep -q .; then
      echo "[pre-push] no src-tauri/ changes since $remote_ref — skipping cargo gate"
      exit 0
    fi
  fi
fi

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT/src-tauri"

# A path dependency on a sibling repo (e.g. qontinui-schemas/rust) that sits
# ahead of the version pinned in the committed Cargo.lock makes every cargo
# invocation rewrite Cargo.lock. That churn is environment-local — it depends
# on which ref the sibling checkout is parked at (a feature branch cut before
# a sibling version bump, a sibling that's been pulled ahead, etc.) — and must
# NOT be committed. But pre-commit reports the hook as Failed whenever it
# leaves a tracked file modified, even when the gate itself passed. Snapshot
# Cargo.lock and restore it on exit so the gate is non-mutating regardless of
# local skew. A Cargo.lock change the developer actually intends rides in their
# own staged diff (which pre-commit hands us as the baseline), so this never
# reverts an intended update.
LOCK="$ROOT/Cargo.lock"
LOCK_BAK=""
if [[ -f "$LOCK" ]]; then
  LOCK_BAK="$(mktemp)"
  cp -p "$LOCK" "$LOCK_BAK"
fi
restore_lock() {
  if [[ -n "$LOCK_BAK" ]]; then
    cp -p "$LOCK_BAK" "$LOCK"
    rm -f "$LOCK_BAK"
  fi
}
trap restore_lock EXIT

# T2 (formatting) — auto-apply, never block. Run `cargo fmt` (writes) instead
# of `cargo fmt -- --check` (fails). Re-stage anything it rewrote so the
# formatted result rides in this push rather than failing it. Formatting is
# mechanical; there is no value in bouncing a push over whitespace.
echo "[pre-push] cargo fmt (auto-apply)"
cargo fmt
# Re-stage any src-tauri files fmt rewrote. `git add -u` on the src-tauri tree
# picks up tracked-file modifications only (no new/untracked files). Best-effort:
# if nothing changed this is a no-op.
git -C "$ROOT" add -u -- src-tauri || true

# T1 (correctness) — block. Plain `cargo clippy`: NO `--all-targets` (that
# compiles #[cfg(test)] modules this PR didn't touch — stricter than CI and a
# frequent source of unrelated-lint push blocks) and NO `-D warnings` (that
# would promote every warning to an error and neuter the Cargo.toml tiering).
# Lint levels come solely from `[lints.clippy]` in src-tauri/Cargo.toml: a
# `deny`-tier (correctness/suspicious) lint still exits non-zero and blocks,
# while `warn`-tier (style/complexity/perf) lints are emitted but do not fail
# the push. See plan 2026-07-05-lint-tiers-and-diff-scoped-gates.
echo "[pre-push] cargo clippy (levels from Cargo.toml [lints.clippy]; deny-tier blocks)"
if ! cargo clippy; then
  echo
  echo "[pre-push] FAIL — a deny-tier (correctness/suspicious) clippy lint fired."
  echo "          Fix the lint, or QONTINUI_PREPUSH_SKIP=1 git push to let CI gate."
  exit 1
fi

echo "[pre-push] cargo gate passed."
