#!/bin/bash
# Shared helper - SOURCE this, do not execute it.
#
# ONE owner for the MSYS POSIX -> Windows path boundary.
#
# ── The boundary, and why it is silent ───────────────────────────────────────
# Under MSYS/Git-for-Windows bash a path is spelled `/d/<workspace-root>/...`.
# That spelling is understood by bash's own builtins and by MSYS-linked binaries,
# and by NOTHING ELSE. The moment a path crosses into a NATIVE Windows binary
# that OPENS the path itself - python.exe, node.exe, git.exe - it must be spelled
# `D:/<workspace-root>/...` or the open fails on a file that EXISTS.
#
# MSYS normally converts arguments on the way across. It is not something a
# script here may rely on: several commands and runbooks in this fleet export
# MSYS_NO_PATHCONV=1, and it is INHERITED, so the conversion silently stops
# happening in a child that never asked. Measured:
# `MSYS_NO_PATHCONV=1 python3 /d/.../symbol-claims-by-machine.py` ->
# "can't open file 'D:\\d\\qontinui-root\\...'".
#
# The failure is silent in BOTH directions, which is why this is a library and
# not a comment:
#   * `[ -f "$p" ]` passes under MSYS bash on the POSIX spelling, so a caller's
#     "is it installed?" probe says yes and the native invocation then fails -
#     straight into the caller's "not installed" branch, which swallows it.
#   * `git -C ""` (what a FAILED conversion yields) is a documented NO-OP: git
#     stays in the CURRENT directory and exits 0. A scanner then reports its own
#     tree under every candidate's name - a blanket false all-clear.
#
# ── Why the two SPELLINGS exist, and why they are NOT interchangeable ───────
# `cygpath -m` gives `D:/x/y` (forward slashes, "mixed"). `cygpath -w` gives
# `D:\x\y` (backslashes, "windows"). Collapsing the two onto one spelling is a
# BUG, so they are two NAMED functions rather than one function with a flag: a
# caller cannot get the wrong spelling by forgetting an argument.
#
#   native_path_m - for a value that is COMPOSED WITH and COMPARED AGAINST other
#     paths after conversion (a workspace root that gets `/qontinui-web/...`
#     appended, a value re-fed to bash). `-m` is chosen because it is IDEMPOTENT
#     on an already-native path and NORMALIZES backslashes, so a root that is
#     already `D:/...`, or is `D:\...`, comes back the same either way and the
#     result can be converted again harmlessly. A `-w` result cannot: the
#     backslashes it introduces are bash escape characters downstream.
#
#   native_path_w - for a value handed STRAIGHT to a native binary as one
#     argument and never touched again (`git -C <path>`). Backslashes are the
#     spelling that binary's own APIs emit, so this is the form that survives an
#     unhelpful quoting layer between here and there.
#
# ── Why capture-then-test, not `cygpath ... && return 0` ─────────────────────
# The tempting spelling
#     command -v cygpath >/dev/null && cygpath -m "$1" && return 0
#     printf '%s\n' "$1"
# is WRONG in the failure case, not merely inelegant: cygpath has ALREADY
# WRITTEN whatever it produced to stdout before its non-zero status is seen, and
# the fall-through printf then appends the input. The caller's `$( )` captures a
# TWO-LINE string, and every path composed from it is garbage that still looks
# like a path. Capturing into `_n` and testing both the status and the emptiness
# means a failed conversion emits the input and nothing else. No failing case is
# reachable today; the SHAPE is the point, because the recovery from the other
# shape is silent corruption rather than an error.
#
# ── The probe is hoisted ONCE ────────────────────────────────────────────────
# `command -v cygpath` is a process-visible cost on a hot path (the WIP scanner
# used to pay it three times per examined checkout, and hooks run on every tool
# call), so it is resolved at SOURCE time into one module-level variable that
# both functions read. Nothing on this fleet installs or removes cygpath inside
# the lifetime of one hook invocation, so a source-time answer is a
# process-lifetime answer.
#
# ── Sourcing this is a NO-OP on Linux / CI ───────────────────────────────────
# With no cygpath both functions return their input unchanged (native_path_w
# also runs its pure-bash fallback, which matches nothing on a Linux path - see
# below). So a consumer can source and call unconditionally; there is no
# platform branch for a caller to get wrong, and no case where a caller must
# decide whether conversion "applies".
#
# ── Caching is the CALLER's, deliberately ────────────────────────────────────
# No memo table is exported here. A general one needs an associative array
# (bash 4), and this file is sourced by hooks whose bash version is not this
# repo's to choose - the same constraint scripts/lib/git-scope.sh records about
# `${#arr[@]}` under `set -u` on bash < 4.4. A helper that ABORTS the caller on
# an old shell, in a branch that exists only to save a fork, is a worse failure
# than the fork.
#
# What the one caller that needs caching actually needs is not a memo table
# anyway: scripts/scan-worktree-wip.sh converts hundreds of paths that all share
# ONE prefix (the workspace root), so it converts the root once and does string
# surgery on the rest - zero forks for every path after the first, which no
# generic memo could match. It keeps that four-line prefix cache at its own call
# site, built on `native_path_w`. That is the supported pattern: cache the
# PREFIX, call the library for the prefix itself.

# 1 when a usable cygpath was on PATH at source time, 0 otherwise. Read by both
# functions; not part of the exported API - a caller wanting to know should ask
# `command -v cygpath` itself rather than depend on this spelling.
# $QONTINUI_NATIVE_PATH_FORCE_NO_CYGPATH is a TEST SEAM, and it exists because
# the obvious way to exercise the no-cygpath arm is unsafe on the one platform
# that matters. A suite cannot simulate "cygpath is absent" by removing it from
# PATH: under Git Bash `cygpath` and `bash` live in the SAME directory, so
# stripping every PATH entry that holds cygpath also strips the one that holds
# bash -- and the child then resolves to `C:\Windows\System32\bash.exe`, the
# WSL launcher, which answers "Windows Subsystem for Linux has no installed
# distributions." MEASURED 2026-09-10 on windows-latest: all 15 failures of
# scripts/lib/native-path-test.sh were that one string, from this repo's own
# fixture. The suite broke on exactly the platform it exists to measure, which
# is this library's own defect class one level up.
#
# Named in the refusal-free spirit of run-guard-tests.sh's RGT_HOST_PLATFORM:
# a host-keyed predicate needs a seam, and the seam belongs beside the
# predicate rather than reimplemented by every caller. Nothing in production
# sets it; the workflow's own invocations are the audit trail for that.
if [ "${QONTINUI_NATIVE_PATH_FORCE_NO_CYGPATH:-0}" = "1" ]; then
    QONTINUI_NATIVE_PATH_HAVE_CYGPATH=0
elif command -v cygpath >/dev/null 2>&1; then
    QONTINUI_NATIVE_PATH_HAVE_CYGPATH=1
else
    QONTINUI_NATIVE_PATH_HAVE_CYGPATH=0
fi

# native_path_m <path>
#   Print <path> in the FORWARD-SLASH native spelling (`D:/x/y`). Idempotent on
#   an already-native path; normalizes backslashes. Prints <path> unchanged when
#   cygpath is absent or fails. Always returns 0 - a caller that could not
#   convert is not in an error state, it is on Linux.
native_path_m() {
    local _n
    if [ "$QONTINUI_NATIVE_PATH_HAVE_CYGPATH" = 1 ] && _n="$(cygpath -m "$1" 2>/dev/null)" && [ -n "$_n" ]; then
        printf '%s\n' "$_n"
        return 0
    fi
    printf '%s\n' "$1"
}

# native_path_w <path>
#   Print <path> in the BACKSLASH native spelling (`D:\x\y`), for a value handed
#   straight to a native binary and never composed with again.
#
#   The pure-bash fallback is NOT dead code and NOT a duplicate of the `-m`
#   branch. It exists for the shape `/d/foo` -> `D:/foo`, which is the ONE
#   conversion that can be done correctly without cygpath, and it fires on a box
#   that has MSYS-spelled paths but no cygpath on PATH. It emits a FORWARD slash
#   after the drive letter on purpose: `D:/foo` is accepted by every native
#   consumer here, and hand-rolling the backslash form would mean escaping
#   analysis this fallback has no business doing. On a Linux path (`/home/<user>`)
#   the pattern does not match - `/h` is followed by `o`, not `/` - so the
#   fallback is the identity there, which is why sourcing this on CI is a no-op.
#
#   Capture-then-test for the same reason native_path_m does it, and here the
#   consequence is worse: the historic spelling was a bare `cygpath -w "$p"`,
#   which on a cygpath FAILURE emitted nothing at all and handed `git -C ""` to
#   the scanner - the documented no-op that produces a blanket false all-clear.
#   Now a failure falls through to the fallback, and an unconvertible path comes
#   back unchanged rather than empty.
native_path_w() {
    local _p="$1" _n
    if [ "$QONTINUI_NATIVE_PATH_HAVE_CYGPATH" = 1 ] && _n="$(cygpath -w "$_p" 2>/dev/null)" && [ -n "$_n" ]; then
        printf '%s\n' "$_n"
        return 0
    fi
    case "$_p" in
        /[A-Za-z]/*) printf '%s:/%s\n' "$(printf '%s' "${_p:1:1}" | tr 'a-z' 'A-Z')" "${_p:3}" ;;
        *) printf '%s\n' "$_p" ;;
    esac
}

# ── THE OTHER DIRECTION, and it is not the same problem inverted ─────────────
# Everything above converts a path that MUST cross into a native binary. This
# converts nothing: it stops MSYS from converting an argument that only LOOKS
# like a path and is not one.
#
# MSYS rewrites a POSIX-looking argv word on its way to a child. For a real
# path that is the behaviour the functions above exist to make reliable. For a
# NON-path -- a URL route fragment, a regex, a git refspec, an option value
# whose grammar happens to start with `/` -- it is silent corruption: MSYS
# resolves the leading `/` against its own installation root, so a fragment
# `/memory` arrives at the child as `C:/Program Files/Git/memory`.
#
# MEASURED, 2026-09-03, `guard-roster-windows` on windows-latest, the first
# Windows run this roster ever had (plan
# 2026-09-02-msys-native-path-boundary-recurs-past-check-9-in-git-c, Phase 4):
# `scripts/coord-route-census-test.sh` passes the route fragment `/memory` and
# the census reported `fragment=C:/Program Files/Git/memory`, failing four
# assertions. Nothing in this library covered it, because every prior
# occurrence of this dossier's class was the CONVERT direction and the fix for
# that direction is `cygpath`. The fix for this one is the opposite of a
# conversion, which is why it needed a name of its own rather than a flag on
# `native_path_m`.
#
# WHY THIS IS A FUNCTION AND NOT A DOCUMENTED ENV VAR. The variable must be set
# for ONE invocation and never exported. This file's own header records the
# reason from the other side: an EXPORTED `MSYS_NO_PATHCONV=1` is INHERITED, so
# conversion silently stops in a grandchild that never asked -- the exact
# failure `native_path_m` exists to prevent. So the safe spelling and the
# dangerous one differ by a single keyword, in a direction no reviewer reliably
# catches. A prefix assignment on the command scopes it to that child alone.
#
# BOTH SPELLINGS are set because the fleet runs both runtimes: `MSYS_NO_PATHCONV`
# is Git-for-Windows / MSYS1, `MSYS2_ARG_CONV_EXCL=*` is MSYS2. Setting the wrong
# one alone is a silent no-op on the other, which is the same class of
# unobservable failure as the rest of this file.
#
# On Linux both variables are inert, so this is the identity: it runs the command
# exactly as given, which is why sourcing and using it on CI changes nothing.
#
# run_unmangled <cmd> [args...]
#   Run <cmd> with MSYS argv path-conversion suppressed for THAT INVOCATION ONLY.
#   Returns the command's own exit status unchanged.
run_unmangled() {
    MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' "$@"
}

# native_pythonpath <python-cmd> <dir> [existing]
#   Print a PYTHONPATH value THAT INTERPRETER can actually read: <dir> spelled
#   the way it needs, joined to [existing] with the separator IT splits on.
#
# TWO bugs live at this one boundary and a fix that closes only one still fails.
# MEASURED 2026-09-09 in `guard-roster-windows` on windows-latest
# (lint-frontmatter run #34324446074, the probe on qontinui-claude-config#855):
# `scripts/coord-route-census.sh` ran
#   PYTHONPATH="$SCRIPT_DIR:..." exec python3 -m detector_reach.route_census
# against a native `C:\hostedtoolcache\...\python3.exe`, so:
#
#   1. `$SCRIPT_DIR` is POSIX (`cd … && pwd`) and native python cannot read it;
#   2. the joiner `:` is not what that interpreter splits on -- it is `;` there,
#      and a `:` inside `C:/…` is itself a drive letter, so the wrong separator
#      does not merely fail to split, it CORRUPTS the first entry.
#
# The observed failure was `ModuleNotFoundError: detector_reach.route_census` --
# which reads as a packaging problem and is a path-spelling one. That is this
# dossier's whole subject: the failure names the wrong layer.
#
# ── WHY THIS ASKS THE INTERPRETER RATHER THAN THE SHELL ──────────────────────
# The first version keyed on "is cygpath on PATH". That is the wrong question,
# and wrong in the one place it would look fixed: it asks whether this is an
# MSYS/Cygwin SHELL, not whether the INTERPRETER is native. Under Cygwin, or
# with an MSYS2 pacman `python3`, cygpath is present and that interpreter's
# `os.pathsep` is `:` -- so a cygpath-keyed composer emits `;`, the whole value
# becomes one unsplittable entry, and the same ModuleNotFoundError returns in
# an environment where the fix appears to be in place. The caller chooses the
# interpreter (`${PYTHON:-python3}`), so nothing makes them agree.
#
# So ASK IT: `os.pathsep` is `;` iff the interpreter is a native Windows build,
# which is exactly the same condition that decides whether it needs a native
# path spelling. ONE probe answers both halves and they cannot disagree.
#
# The probe costs one subprocess and is skipped entirely off MSYS (no cygpath
# means no conversion to make and `:` either way). If the probe FAILS -- an
# interpreter that will not run is one the caller is about to fail on anyway --
# it falls back to the cygpath heuristic rather than guessing silently, which
# is the old behaviour and is stated rather than hidden.
#
# LIMIT, stated rather than discovered later: only <dir> is converted. Entries
# already in [existing] are passed through untouched, because a PYTHONPATH
# arriving from the environment may already be native, already be POSIX, or mix
# both, and there is no way to tell which separator it was built with -- a `:`
# is ambiguous between a joiner and a drive letter. Converting it would guess.
# The caller owns <dir>; whoever exported [existing] owns its spelling.
#
# An EMPTY <dir> is refused rather than composed. A leading empty entry is the
# CURRENT DIRECTORY to Python, so silently emitting one turns a path-composition
# helper into an import-shadowing hazard -- the same class of silent wrong
# answer this whole file exists to remove.
#
# On Linux this is `<dir>:<existing>` with no conversion and no probe -- the
# identity, which is why every caller can use it unconditionally.
native_pythonpath() {
    local _py="$1" _dir="$2" _rest="${3-}" _sep=':' _n _probe
    if [ -z "$_dir" ]; then
        printf '%s' "$_rest"
        return 0
    fi
    _n="$(native_path_m "$_dir")"
    if [ "$QONTINUI_NATIVE_PATH_HAVE_CYGPATH" = 1 ]; then
        # Ask the interpreter, and only fall back to the shell-shaped guess when
        # it cannot answer.
        if _probe="$("$_py" -c 'import os;print(os.pathsep)' 2>/dev/null)" && [ -n "$_probe" ]; then
            _sep="$_probe"
            # A POSIX-separator interpreter under MSYS wants the POSIX spelling
            # too -- the same answer decides both, which is the point.
            [ "$_sep" = ':' ] && _n="$_dir"
        else
            _sep=';'
        fi
    fi
    if [ -n "$_rest" ]; then
        printf '%s%s%s\n' "$_n" "$_sep" "$_rest"
    else
        printf '%s\n' "$_n"
    fi
}
