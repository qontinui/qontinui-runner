#!/usr/bin/env bash
# Install qontinui-runner's git hooks. Run from the repo root:
#   ./scripts/git-hooks/install.sh
#
# Copies the bundled pre-commit and commit-msg hooks into .git/hooks/ and
# marks them executable. Safe to re-run; overwrites in place.
#
# Why direct shell hooks instead of pre-commit-framework's Python module?
# The Python `pre_commit` module hangs on import with Python 3.13 on this
# machine — see qontinui-dev-notes/runtime memory `pre_commit_hangs_3_13`.
# These bundled scripts run the same checks as `.pre-commit-config.yaml`'s
# local hooks (cargo clippy, ESLint, Prettier, tsc, attribution check).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel)"
HOOKS_DIR="$REPO_ROOT/.git/hooks"

for hook in pre-commit commit-msg; do
  src="$SCRIPT_DIR/$hook"
  dst="$HOOKS_DIR/$hook"
  if [ ! -f "$src" ]; then
    echo "WARN: $src missing, skipping"
    continue
  fi
  cp "$src" "$dst"
  chmod +x "$dst"
  echo "Installed: $dst"
done

echo "Done. Run a no-op commit to verify hooks fire correctly."
