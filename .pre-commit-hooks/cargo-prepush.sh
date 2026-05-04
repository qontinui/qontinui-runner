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

cd "$(git rev-parse --show-toplevel)/src-tauri"

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
