#!/usr/bin/env bash
#
# sync-setup-mcp.sh — refresh the vendored qontinui-setup-mcp package source.
#
# The setup wizard's folder scan runs `python -m qontinui_setup_mcp.cli` from a
# bundled copy of the (pure-stdlib) qontinui-setup-mcp package, so an end-user
# install needs no pip install and no sibling checkout. The single source of
# truth is the upstream repo; this script re-vendors its package source into
# `src-tauri/resources/qontinui-setup-mcp/src/` (which tauri.conf.json bundles).
#
# Run this whenever upstream changes. CI should run it and fail if it produces
# a diff (i.e. the vendored copy drifted from the pinned upstream).
#
#   ./scripts/sync-setup-mcp.sh                 # use sibling ../qontinui-setup-mcp if present, else clone
#   ./scripts/sync-setup-mcp.sh <git-ref>       # vendor a specific ref/branch/sha
#
set -euo pipefail

REPO_URL="https://github.com/qontinui/qontinui-setup-mcp"
RUNNER_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
DEST="$RUNNER_ROOT/src-tauri/resources/qontinui-setup-mcp/src"
REF="${1:-}"

# Resolve a source checkout: prefer a sibling clone, else shallow-clone to tmp.
SIBLING="$RUNNER_ROOT/../qontinui-setup-mcp"
CLEANUP=""
if [ -d "$SIBLING/.git" ] && [ -z "$REF" ]; then
  SRC_REPO="$SIBLING"
else
  TMP="$(mktemp -d)"; CLEANUP="$TMP"
  git clone --quiet "$REPO_URL" "$TMP"
  [ -n "$REF" ] && git -C "$TMP" checkout --quiet "$REF"
  SRC_REPO="$TMP"
fi

SHA="$(git -C "$SRC_REPO" rev-parse HEAD)"

# Re-vendor: replace the package dir wholesale, strip caches.
rm -rf "$DEST"
mkdir -p "$DEST"
cp -R "$SRC_REPO/src/qontinui_setup_mcp" "$DEST/qontinui_setup_mcp"
find "$DEST" -name '__pycache__' -type d -prune -exec rm -rf {} + 2>/dev/null || true

# Record provenance.
cat > "$RUNNER_ROOT/src-tauri/resources/qontinui-setup-mcp/VENDOR.md" <<EOF
# Vendored: qontinui-setup-mcp

AUTO-VENDORED — do not edit these files by hand. They are a copy of the
\`qontinui_setup_mcp\` package from $REPO_URL, bundled into the app so the
setup-wizard folder scan works out of the box (no pip install, no sibling
checkout). The scan CLI is pure-stdlib; \`mcp\`/\`httpx\` are server-only and
are intentionally NOT vendored.

- Upstream: $REPO_URL
- Pinned commit: $SHA
- Refresh: \`./scripts/sync-setup-mcp.sh\` (from the runner repo root)
EOF

[ -n "$CLEANUP" ] && rm -rf "$CLEANUP"
echo "Vendored qontinui-setup-mcp @ $SHA -> $DEST"
