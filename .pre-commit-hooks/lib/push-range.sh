#!/usr/bin/env bash
#
# push-range.sh — "which ref is this push measured against?"
#
# Sourced by `.pre-commit-hooks/cargo-prepush.sh` and by
# `.pre-commit-hooks/lib/gen-events-attribution.sh`. Split into its own neutral
# file because BOTH pre-push hooks need the same question answered and neither
# owns the answer: it used to live inside the attribution library, where the
# cargo gate could not reach it without dragging in codegen-attribution state
# it has nothing to do with.
#
# THE PROBLEM THIS SOLVES
#
# A pre-push hook that wants to scope itself to "what this push actually adds"
# needs a base ref. The obvious one — `@{upstream}` — is ABSENT on exactly the
# branches this fleet pushes most: an allocated agent worktree reserves a fresh
# branch, so its first push has no upstream at all and `git rev-parse
# --symbolic-full-name @{u}` fails outright. A hook keyed on `@{u}` alone
# therefore never takes its scoped path on any worktree.
#
# So the resolution is a CASCADE, each candidate validated before it is
# returned: the branch's own upstream, then the remote's published default
# branch, then the literal `origin/main`.
#
# THE DELIBERATE OMISSION
#
# `push_base_ref` takes NO position on what to do when every candidate fails.
# It prints a ref and returns 0, or prints nothing and returns non-zero. That
# is not indecision — its two callers want OPPOSITE defaults:
#
#   * gen-events attribution fails CLOSED (unknown base -> never clear the
#     pusher, because clearing a guilty push loses the signal)
#   * the cargo pre-push gate fails OPEN (unknown base -> run the full gate,
#     because skipping it would let an unlinted push through)
#
# Baking either default in here would make the function unusable by the other
# caller. Keep it neutral.
#
# Deliberately NOT sourced from pre-commit's `PRE_COMMIT_FROM_REF` /
# `PRE_COMMIT_TO_REF`: those exist only when the hook is invoked THROUGH
# pre-commit, and both callers are plain bash scripts that are also run
# directly (by their own tests, and by anyone debugging them). The git-native
# cascade works under both. If the range vars are ever wanted they belong as a
# first candidate INSIDE this function, not as a replacement for it.

# Resolve the ref this push is measured against, for the repo at $1.
# Prints the ref name and returns 0, or prints nothing and returns 1.
push_base_ref() {
    local repo="$1" ref
    for ref in \
        "$(git -C "$repo" rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)" \
        "$(git -C "$repo" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)" \
        "origin/main"
    do
        [ -n "$ref" ] || continue
        git -C "$repo" rev-parse --verify --quiet "${ref}^{commit}" >/dev/null 2>&1 || continue
        git -C "$repo" merge-base HEAD "$ref" >/dev/null 2>&1 || continue
        printf '%s\n' "$ref"
        return 0
    done
    return 1
}
