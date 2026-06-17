#!/usr/bin/env bash
# Pre-push guard: mirrors CI's Rust gate (cargo fmt --check + clippy -D
# warnings) so the failure is surfaced in 60-90s of local cycle time
# instead of 8-15 min of CI roundtrip per push.
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

echo "[pre-push] cargo fmt -- --check"
if ! cargo fmt -- --check; then
  echo
  echo "[pre-push] FAIL — cargo fmt found unformatted code."
  echo "          Run 'cd src-tauri && cargo fmt' to fix, then re-push."
  exit 1
fi

echo "[pre-push] cargo clippy --all-targets -- -D warnings"
if ! cargo clippy --all-targets -- -D warnings; then
  echo
  echo "[pre-push] FAIL — clippy lints. Either fix the lints or"
  echo "          QONTINUI_PREPUSH_SKIP=1 git push   if you want CI to gate."
  exit 1
fi

echo "[pre-push] cargo gate passed."
