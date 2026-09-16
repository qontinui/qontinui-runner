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
# THE SECOND PROBLEM: `HEAD` IS NOT WHAT IS BEING PUSHED
#
# The cascade above answers "what is this branch measured against". It used to
# be the WHOLE answer, and that silently assumed the checked-out branch is the
# one in flight. It very often is not:
#
#     git push origin <sha>:refs/heads/topic     # HEAD irrelevant
#     git push origin some-other-branch          # HEAD is a different branch
#     git push --all                             # several refs at once
#
# A gate scoped `merge-base(base, HEAD)..HEAD` then measures a tree nobody is
# pushing. Measured on a fixture 2026-09-17: with `main` checked out and a
# Rust-changing `feature` branch pushed by name, `cargo-prepush.sh` printed
# "no src-tauri/ changes since origin/main — skipping cargo gate" and the
# unlinted commit went out. The failure direction is a FALSE SKIP, which is the
# expensive one: the gate reports a clean skip and nothing is checked.
#
# git hands a pre-push hook the answer on stdin, one line per ref:
#
#     <local ref> <local sha> <remote ref> <remote sha>
#
# and pre-commit, which consumes that stdin itself, re-exposes it as
# `PRE_COMMIT_FROM_REF` / `PRE_COMMIT_TO_REF`. `push_ranges` below reads
# whichever is available and falls back to the HEAD-based cascade — so a hook
# run by hand, or by its own tests, behaves exactly as it did before.
#
# The earlier "deliberately NOT sourced from PRE_COMMIT_*" note that stood here
# was right about the constraint and wrong about the conclusion: those vars
# cannot REPLACE the cascade, because a hook run directly has neither them nor
# stdin. They belong, as that note itself said, "as a first candidate INSIDE
# this function" — which is what `push_ranges` is.

# Resolve the ref this push is measured against, for the repo at $1.
# $2 — the commit the base must be an ancestry-relation of; defaults to HEAD.
# Prints the ref name and returns 0, or prints nothing and returns 1.
push_base_ref() {
    local repo="$1" tip="${2:-HEAD}" ref
    for ref in \
        "$(git -C "$repo" rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)" \
        "$(git -C "$repo" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)" \
        "origin/main"
    do
        [ -n "$ref" ] || continue
        git -C "$repo" rev-parse --verify --quiet "${ref}^{commit}" >/dev/null 2>&1 || continue
        git -C "$repo" merge-base "$tip" "$ref" >/dev/null 2>&1 || continue
        printf '%s\n' "$ref"
        return 0
    done
    return 1
}

# True when $1 is git's all-zero sha (ref does not exist on the remote yet, or
# is being deleted). Both object formats: 40 hex for sha1, 64 for sha256.
push_range_is_zero_sha() {
    case "$1" in
        *[!0]*) return 1 ;;
        "") return 1 ;;
        *) return 0 ;;
    esac
}

# The (local sha, remote sha) pairs this push carries, one per line.
#
# Sources, in order:
#   1. git's pre-push protocol on stdin — the authoritative answer, and the
#      only one that covers a multi-ref push.
#   2. PRE_COMMIT_TO_REF / PRE_COMMIT_FROM_REF — what pre-commit exposes after
#      consuming that same stdin.
# THREE outcomes, and the caller must tell them apart:
#   0 — a COMPLETE ref list
#   1 — NO ref information at all (run by hand, or by a test) -> fall back to HEAD
#   2 — ref information that is INCOMPLETE (a stalled read, a malformed line)
#
# 1 and 2 used to share a code, and the caller mapped both onto the HEAD
# fallback — which silently converted "I could not read the whole push" into
# "measure HEAD instead", the exact false skip this file exists to abolish.
#
# ⚠️ CONSUMES STDIN. That is safe here only because every caller is handed its
# OWN copy: the managed pre-push shim `cat`s git's stdin to a temp file and
# redirects each hook from it separately, precisely so "a hook that did read
# stdin would not starve every hook after it". A future runner of these hooks
# must preserve that. Never blocks: a terminal stdin is skipped outright and
# the read is time-bounded, so a caller that hands us an idle pipe costs
# seconds, not a hung push.
push_pushed_refs() {
    local local_ref local_sha remote_ref remote_sha rest found=0 read_rc=0 malformed=0

    if [ ! -t 0 ]; then
        # ⚠️ `read` drives the loop from INSIDE the body, not from the `while`
        # condition. After `done`, `$?` is the status of the last command in the
        # BODY — always 0 here — so a condition-driven loop cannot tell a
        # timeout from EOF no matter what it inspects afterwards. Measured: the
        # first spelling of this fix looked right and caught nothing.
        while :; do
            # `|| read_rc=$?`, never `if ! read …; then read_rc=$?`: inside the
            # body of an `if !` the status is the NEGATION's, which is always 0.
            # The same idiom, for the same reason, as `cargo-prepush.sh`'s
            # metadata and clippy blocks.
            read -r -t 2 local_ref local_sha remote_ref remote_sha rest \
                || { read_rc=$?; break; }
            # Only well-formed protocol lines. Anything else is a caller
            # piping us something that is not a ref list, and guessing at it
            # would be worse than falling back.
            [ -n "${local_sha:-}" ] && [ -n "${remote_sha:-}" ] && [ -n "${remote_ref:-}" ] \
                || { malformed=1; continue; }
            [ -z "${rest:-}" ] || { malformed=1; continue; }
            case "$local_sha$remote_sha" in *[!0-9a-fA-F]*) malformed=1; continue ;; esac
            # A deletion pushes no commits; there is nothing to scope.
            push_range_is_zero_sha "$local_sha" && continue
            printf '%s %s\n' "$local_sha" "$remote_sha"
            found=1
        done
        # ⚠️ A TIMEOUT IS NOT EOF. `read -t` returns >128 when it gives up, and
        # the loop ends either way — so without this a ref list cut off
        # mid-stream would report as the COMPLETE push. If the dropped ref is
        # the Rust-bearing one and a surviving ref is TS-only, the verdict over
        # the "union" is a skip: the exact false skip this file exists to stop.
        # Every other "could not tell" here fails open; so does this.
        if [ "$read_rc" -gt 128 ]; then
            return 2
        fi
        # A line that failed the well-formedness screen was DROPPED, not
        # understood. With other lines surviving, the ref list we return is a
        # strict subset of the push — so it is incomplete, not empty. The
        # screen's own comment promises a fallback; this is what delivers it.
        if [ "$malformed" -eq 1 ]; then
            return 2
        fi
    fi
    [ "$found" -eq 0 ] || return 0

    # ⚠️ KNOWN LIMITATION, not an oversight. pre-commit consumes git's stdin
    # itself and re-exposes ONE ref pair, so on a machine where the hooks run
    # through pre-commit rather than the direct shim, a multi-ref push
    # (`git push --all`) is scoped to whichever ref pre-commit picked. Nothing
    # in the environment distinguishes "one ref was pushed" from "one of
    # several", so there is no signal to fail open on, and inventing one would
    # mean running the full gate on every push through pre-commit. Recorded
    # here rather than papered over; the direct shim, which is what this fleet
    # installs, hands us the whole list and does not have this limitation.
    if [ -n "${PRE_COMMIT_TO_REF:-}" ]; then
        push_range_is_zero_sha "$PRE_COMMIT_TO_REF" && return 1
        printf '%s %s\n' "$PRE_COMMIT_TO_REF" "${PRE_COMMIT_FROM_REF:-}"
        return 0
    fi
    return 1
}

# The ranges this push actually carries, as "<base sha> <tip sha>" lines.
#
# $1 — repo root.
# For each pushed ref: the tip is the LOCAL sha in flight, and the base is the
# remote's current sha when the remote already has the ref and we hold that
# object, otherwise `merge-base(<cascade ref>, tip)`. A ref whose base cannot
# be resolved at all is DROPPED and the function returns 1, so the caller can
# fail open rather than scope against a partial answer.
#
# With no pushed-ref information at all it falls back to the historical
# behaviour — `merge-base(<cascade ref>, HEAD)..HEAD` — and returns 0.
push_ranges() {
    local repo="$1" tip remote_sha base base_ref line incomplete=0 emitted=0
    local refs refs_rc=0
    refs="$(push_pushed_refs)" || refs_rc=$?

    # 2 is "I read SOME of the push". Falling back to HEAD here would answer a
    # different question confidently; the caller's only safe move is the gate.
    [ "$refs_rc" -ne 2 ] || return 1

    if [ -z "$refs" ]; then
        base_ref="$(push_base_ref "$repo")" || return 1
        base="$(git -C "$repo" merge-base "$base_ref" HEAD 2>/dev/null)" || return 1
        printf '%s %s\n' "$base" "HEAD"
        return 0
    fi

    while IFS=' ' read -r tip remote_sha; do
        [ -n "$tip" ] || continue
        git -C "$repo" rev-parse --verify --quiet "${tip}^{commit}" >/dev/null 2>&1 \
            || { incomplete=1; continue; }
        base=""
        if [ -n "$remote_sha" ] && ! push_range_is_zero_sha "$remote_sha" \
           && git -C "$repo" rev-parse --verify --quiet "${remote_sha}^{commit}" >/dev/null 2>&1; then
            # The remote already has this ref: what it has is exactly the base.
            base="$remote_sha"
        else
            if base_ref="$(push_base_ref "$repo" "$tip")"; then
                base="$(git -C "$repo" merge-base "$base_ref" "$tip" 2>/dev/null)" || base=""
            fi
        fi
        [ -n "$base" ] || { incomplete=1; continue; }
        printf '%s %s\n' "$base" "$tip"
        emitted=1
    done <<EOF
$refs
EOF

    { [ "$emitted" -eq 1 ] && [ "$incomplete" -eq 0 ]; } || return 1
    return 0
}
