#!/usr/bin/env bash
#
# scratch-cleanup.sh — remove a hook's scratch tree even when parts of it are
# read-only.
#
# Sourced by `.pre-commit-hooks/gen-events-drift.sh`; tested by
# `scratch-cleanup-test.sh`.
#
# THE FAILURE THIS FIXES (coord finding 51901722, 2026-10-06)
#
# gen-events-drift snapshots the schemas baseline with `cp -R`, which keeps the
# source's modes. When `qontinui-schemas` is the SHA-keyed sibling store
# (`agent-worktrees/.siblings/qontinui-schemas@<sha>`, read-only by design),
# the snapshot's directories come out read-only, so `rm -rf` cannot empty them.
# The hook's EXIT trap ran that `rm -rf` as its LAST command, so its failure
# became the hook's exit status: a run that had just printed
# "OK — regenerated bindings match" aborted the push.
#
# scratch_dir_remove makes the tree writable first, and never fails the caller:
# cleaning a temp dir is housekeeping, not a verdict. A tree it still cannot
# remove is reported on stderr and left behind.

# scratch_dir_remove <dir> — always returns 0.
scratch_dir_remove() {
    local dir="${1:-}"
    [ -n "$dir" ] && [ -e "$dir" ] || return 0
    chmod -R u+w -- "$dir" 2>/dev/null || true
    if ! rm -rf -- "$dir" 2>/dev/null; then
        echo "[scratch-cleanup] could not remove $dir (left behind; not a hook failure)" >&2
    fi
    return 0
}
