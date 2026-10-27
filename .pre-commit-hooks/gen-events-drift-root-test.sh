#!/usr/bin/env bash
#
# Tests for how `gen-events-drift.sh` finds the repository root.
#
# Run: bash .pre-commit-hooks/gen-events-drift-root-test.sh
#
# The commit-sourced pre-push shim (qontinui-claude-config
# scripts/install-repo-git-hooks.sh, shim v2) may run this hook from a copy of
# .pre-commit-hooks/ extracted into a private temp dir. A root derived from the
# script's own location ("$SCRIPT_DIR/..") is then the temp dir, so the hook
# looks for ../qontinui-schemas there, reports CANNOT EVALUATE and checks nothing.
#
# Each case stops at the hook's first sibling check (no qontinui-schemas beside
# the fixture), whose message names the path the hook derived — so nothing here
# needs a Rust toolchain or a schemas checkout. What is pinned is WHICH root:
#
#   * extracted copy, QONTINUI_HOOK_REPO_ROOT exported -> that root
#   * extracted copy, no export, cwd at a checkout     -> the cwd's top level
#   * in place, run by path from an unrelated cwd      -> the script's checkout

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOK_DIR="$SCRIPT_DIR"

# Under a hook, GIT_DIR and friends override `git -C`; start from none of them.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX GIT_COMMON_DIR \
      GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES \
      QONTINUI_HOOK_REPO_ROOT QONTINUI_SCHEMAS_DIR QONTINUI_GEN_EVENTS_DRIFT_STRICT \
      PRE_COMMIT PRE_COMMIT_FROM_REF PRE_COMMIT_TO_REF PRE_COMMIT_REMOTE_NAME \
      PRE_COMMIT_REMOTE_URL PRE_COMMIT_REMOTE_BRANCH PRE_COMMIT_LOCAL_BRANCH 2>/dev/null

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

pass=0; fail=0
ok()  { pass=$((pass+1)); echo "  PASS  $1"; }
bad() { fail=$((fail+1)); echo "  FAIL  $1"; [ -n "${2:-}" ] && printf '%s\n' "$2" | sed 's/^/        /'; }

# A fixture "checkout" with no qontinui-schemas sibling. The hook prints the
# lookup as "<root>/../qontinui-schemas", with <root> in pwd form.
mkdir -p "$T/ws/repo" "$T/elsewhere" "$T/other/root"
git -C "$T/ws/repo" init -q
WS="$(cd "$T/ws" && pwd)"
OTHER="$(cd "$T/other" && pwd)"

# The extracted copy: the whole hook directory, outside any git checkout.
mkdir -p "$T/extract"
cp -R "$HOOK_DIR" "$T/extract/hooks"
EXTRACTED="$T/extract/hooks/gen-events-drift.sh"

# The in-place copy: the hook directory inside the fixture checkout.
cp -R "$HOOK_DIR" "$T/ws/repo/.pre-commit-hooks"
INPLACE="$T/ws/repo/.pre-commit-hooks/gen-events-drift.sh"

expect_root() {  # name, expected repo root, output
  case "$3" in
    *"not found at $2/../qontinui-schemas"*) ok "$1" ;;
    *) bad "$1 (expected the sibling lookup beside $2)" "$3" ;;
  esac
}

echo "gen-events-drift root resolution"

out="$(cd "$T/ws/repo" && QONTINUI_HOOK_REPO_ROOT="$OTHER/root" bash "$EXTRACTED" </dev/null 2>&1)"
expect_root "extracted copy uses the exported QONTINUI_HOOK_REPO_ROOT" "$OTHER/root" "$out"

out="$(cd "$T/ws/repo" && bash "$EXTRACTED" </dev/null 2>&1)"
expect_root "extracted copy with no export uses the cwd's checkout" "$WS/repo" "$out"

out="$(cd "$T/elsewhere" && bash "$INPLACE" </dev/null 2>&1)"
expect_root "in-place copy run from an unrelated cwd uses its own checkout" "$WS/repo" "$out"

echo "gen-events-drift-root: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
