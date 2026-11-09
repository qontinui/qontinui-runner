#!/usr/bin/env bash
#
# Pre-push wrapper for `scripts/check_untimed_subprocess.py` — the local mirror
# of the required CI check `forbid-untimed-subprocess`.
#
# WHY A WRAPPER. The qontinui-claude-config git-hooks shim (which replaces the
# pre-commit framework's own pre-push stage, so pre-commit's stash can no longer
# revert unstaged edits mid-build) runs only `repo: local` + `language: system`
# hooks whose entry is `bash <script>`. A `python3 …` entry is dropped by it, so
# this hook needs a bash entry to keep running under that shim (ccfg#2448).
#
# CONTRACT
#   * repo root: `$QONTINUI_HOOK_REPO_ROOT` when set (the shim exports it), else
#     `git rev-parse --show-toplevel`. Never derived from this script's own
#     location: the shim may run a copy of it from outside the checkout.
#   * interpreter: `python3`, falling back to `python` — the first that actually
#     RUNS and is 3.11+, which the checker requires (`tomllib`). A Windows Store
#     `python3` stub resolves on PATH but cannot run, so it is skipped.
#   * arguments are passed through unchanged. Under pre-commit the hook is
#     `pass_filenames: false`, and under the shim there are no filenames either;
#     the checker takes none — with no arguments it scans the whole discovered
#     scope of the repo, which is what both callers need.
#   * the checker's exit code is this script's exit code.
#
# Run: bash .pre-commit-hooks/untimed-subprocess.sh [checker args...]

set -uo pipefail

if [ -n "${QONTINUI_HOOK_REPO_ROOT:-}" ]; then
    ROOT="$QONTINUI_HOOK_REPO_ROOT"
else
    ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || ROOT=""
fi
if [ -z "$ROOT" ] || [ ! -d "$ROOT" ]; then
    echo "untimed-subprocess: cannot resolve the repo root (QONTINUI_HOOK_REPO_ROOT unset and not inside a git work tree)" >&2
    exit 2
fi
# Absolute, so the checker path stays valid after the `cd` below even when the
# root was given relative.
ROOT="$(cd "$ROOT" && pwd)" || exit 2

CHECKER="$ROOT/scripts/check_untimed_subprocess.py"
if [ ! -f "$CHECKER" ]; then
    echo "untimed-subprocess: checker not found at $CHECKER" >&2
    exit 2
fi

PY=""
for cand in python3 python; do
    if command -v "$cand" >/dev/null 2>&1 && "$cand" -c 'import sys; sys.exit(sys.version_info < (3, 11))' >/dev/null 2>&1; then
        PY="$cand"
        break
    fi
done
if [ -z "$PY" ]; then
    echo "untimed-subprocess: no runnable python3 or python (3.11+) on PATH" >&2
    exit 2
fi

cd "$ROOT" || exit 2
"$PY" "$CHECKER" "$@"
exit $?
