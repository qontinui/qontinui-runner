#!/usr/bin/env bash
#
# pending-registration.sh — give the LOCAL drift hook the same tolerance for
# declared, bounded "bindings ahead of runner main" waits that CI already has.
#
# Sourced by `.pre-commit-hooks/gen-events-drift.sh`; tested by
# `pending-registration-test.sh`.
#
# THE FAILURE THIS FIXES (dossier runner-prepush-gate-misscoped-and-cold-target,
# coord findings 2fada332 and 72b07886, 2026-10-18)
#
# qontinui-schemas checks in bindings for a type BEFORE the runner PR that
# registers it lands (the runner's `add!` cannot compile until the type exists
# on its sibling). It declares each such wait in
# `codegen/pending-registration.toml`, and its checker
# (`scripts/check-generated-drift.sh`, via `scripts/pending_registration.py
# decide`) lets a declared wait absorb exactly its own `.d.ts` and barrel line
# while the wait is ACTIVE: dated within 14 days, naming a runner PR that is
# still OPEN, whose head registers the type. That is the authority CI applies
# on both sides of the contract.
#
# This hook never applied it. Regenerating against runner main deletes every
# pending binding, so on 2026-10-18 every runner push that touched
# `src-tauri/src/` was refused over CapabilityState, CliProfile and
# SessionFailure — three bindings qontinui-schemas declares as waiting on
# runner#1990 — and every session skipped the hook by name. A guard whose red
# is routinely skipped has stopped gating anything.
#
# WHAT IS REUSED, AND WHAT IS NOT
#
# The ACTIVE decision is made by qontinui-schemas' own module, imported from
# the schemas checkout this hook already compares against — never re-derived
# here, so the two sides cannot disagree about which waits are legitimate.
#
# The ABSORPTION is re-implemented, TS-only, because the module's `decide`
# reads a regenerated WORKING TREE of the schemas repo, and this hook is
# forbidden to write one (see the zero-writes invariant in gen-events-drift.sh).
# It absorbs strictly less than `decide` does, never more:
#   files  ts/src/generated/<T>.d.ts       (present in the baseline, absent
#                                           from the regeneration)
#   lines  `export type { T } from './T';`  (exactly once in the baseline
#                                           index.ts, absent from the
#                                           regenerated one)
# and only where the Python half of CI's shape check also holds in the schemas
# checkout (one per-type import and one `__all__` entry in the Python barrel,
# the module file present) — CI would call the entry INERT otherwise. A type
# the regeneration DID produce is not absorbed: whatever it produced is
# compared as usual.
#
# FAILS CLOSED. This is a GATING consumer (the absorption is what lets a push
# through), so an UNKNOWN input withholds the tolerance and never grants it
# [policy: unknown-must-not-render-as-a-default]. No python, no module, a module
# error, no GitHub token, an unreadable PR: absorb nothing, and say which.
#
# READ-ONLY with respect to the schemas checkout, like everything this hook
# does: the module is imported with `-B` (no `__pycache__` written beside it)
# and `-I` (no ambient PYTHONPATH or user site can shadow it).

PENDING_REGISTRATION_TOML_REL="codegen/pending-registration.toml"
PENDING_REGISTRATION_MODULE_REL="scripts/pending_registration.py"

# pending_registration_python — the interpreter, or nothing.
pending_registration_python() {
    local py
    for py in python3 python; do
        if command -v "$py" >/dev/null 2>&1 \
           && "$py" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' >/dev/null 2>&1; then
            printf '%s' "$py"
            return 0
        fi
    done
    return 1
}

# pending_registration_active_types <schemas_dir> <scratch_dir>
#
# Reads the declaration and the module from the schemas checkout's HEAD
# COMMIT, as CI does, never from its working tree: that checkout is shared, and
# a peer's uncommitted toml row or module edit must not be honoured here (nor
# run with the borrowed token). Both are copied out with `git show` into
# <scratch_dir>, which the caller owns and removes; the module is imported
# from there, so nothing — not even `__pycache__` — can land in the checkout.
#
# Sets:
#   PENDING_STATE          none | ok | unknown
#   PENDING_ACTIVE_TYPES   newline-separated type names (ok only)
#   PENDING_ROWS           one "<type>: ACTIVE|INERT (<reason>)" line per entry
#   PENDING_UNKNOWN_REASON why the answer is unknown (unknown only)
# Always returns 0: every outcome is an answer the caller must read.
pending_registration_active_types() {
    local schemas="$1" head="$2"
    PENDING_STATE="unknown"
    PENDING_ACTIVE_TYPES=""
    PENDING_ROWS=""
    PENDING_UNKNOWN_REASON=""
    PENDING_HEAD_SHA=""
    PENDING_HEAD_SCHEMAS=""

    local sha listing
    if ! sha="$(_pending_registration_head_sha "$schemas")"; then
        PENDING_UNKNOWN_REASON="$sha"
        return 0
    fi
    PENDING_HEAD_SHA="$sha"
    PENDING_HEAD_SCHEMAS="$schemas"
    # Absent from the commit is the one confident "nothing declared"; a git
    # that FAILED to answer is unknown, never "none". Any other shape question
    # (how the table is spelled, whether it is empty) is the module's to
    # answer, so a valid file is never dismissed by a grep here.
    if ! listing="$(git -C "$schemas" ls-tree "$sha" -- "$PENDING_REGISTRATION_TOML_REL" 2>/dev/null)"; then
        PENDING_UNKNOWN_REASON="git ls-tree $sha failed in $schemas"
        return 0
    fi
    if [ -z "$listing" ]; then
        PENDING_STATE="none"
        return 0
    fi
    mkdir -p "$head/codegen" "$head/scripts" || {
        PENDING_UNKNOWN_REASON="cannot create $head"; return 0; }
    if ! git -C "$schemas" show "$sha:$PENDING_REGISTRATION_TOML_REL" > "$head/$PENDING_REGISTRATION_TOML_REL" 2>/dev/null; then
        PENDING_UNKNOWN_REASON="cannot read $sha:$PENDING_REGISTRATION_TOML_REL"
        return 0
    fi
    if ! git -C "$schemas" show "$sha:$PENDING_REGISTRATION_MODULE_REL" > "$head/$PENDING_REGISTRATION_MODULE_REL" 2>/dev/null; then
        PENDING_UNKNOWN_REASON="$PENDING_REGISTRATION_TOML_REL declares waits but commit $sha has no $PENDING_REGISTRATION_MODULE_REL"
        return 0
    fi
    local py
    if ! py="$(pending_registration_python)"; then
        PENDING_UNKNOWN_REASON="no Python 3.11+ (python3/python) to run $PENDING_REGISTRATION_MODULE_REL"
        return 0
    fi

    # The module reads GitHub with GITHUB_TOKEN only and makes NO
    # unauthenticated call without one. A developer box usually has `gh`
    # signed in rather than the variable exported, so borrow its token for
    # this one child — never exported to the rest of the hook.
    local token="${GITHUB_TOKEN:-}"
    if [ -z "$token" ] && command -v gh >/dev/null 2>&1; then
        token="$(gh auth token 2>/dev/null || true)"
    fi

    local out rc=0
    # `env -u PENDING_REGISTRATION_FIXTURE`: the module honours that fixture
    # seam everywhere off GitHub Actions, so a value left in a developer's
    # environment (from running the schemas tests) could mark any entry ACTIVE
    # without reading GitHub, a path CI can never take. GITHUB_WORKFLOW goes
    # too, so nothing here can impersonate a workflow context.
    # The token is a prefix ASSIGNMENT, never an argument to `env`, so it
    # travels in the environment and never on a command line a process
    # listing can read.
    out="$(GITHUB_TOKEN="$token" PYTHONDONTWRITEBYTECODE=1 \
        env -u PENDING_REGISTRATION_FIXTURE -u GITHUB_WORKFLOW "$py" -I -B -c '
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
try:
    import pending_registration as m
except Exception as exc:  # an import failure is an answer, not a crash
    print("ERROR\timport failed: %s" % exc)
    sys.exit(3)
try:
    entries = m.load_entries(Path(sys.argv[2]))
    if not entries:
        print("NONE")
        sys.exit(0)
    reader = m.make_reader()
    today = m._today()
    for e in entries:
        ok, why = m.metadata_verdict(e, reader, today)
        if ok:
            print("ACTIVE\t%s" % e.type)
        else:
            print("INERT\t%s\t%s" % (e.type, why))
    print("END")
except Exception as exc:
    print("ERROR\t%s: %s" % (type(exc).__name__, exc))
    sys.exit(3)
' "$head/scripts" "$head" 2>&1)" || rc=$?
    # A Windows Python ends its lines with CRLF; a CR left on a type name would
    # make it match no file and silently absorb nothing.
    out="$(printf '%s' "$out" | tr -d '\r')"

    if [ "$rc" -ne 0 ]; then
        # A read loop over a here-string, not a pipe into `head`/`sed q`: an
        # early-exiting reader can fail a pipeline under the hook's pipefail
        # and abort it under set -e.
        local err="" l
        while IFS= read -r l; do
            case "$l" in "ERROR"$'\t'*) err="${l#ERROR$'\t'}"; break ;; esac
        done <<< "$out"
        PENDING_UNKNOWN_REASON="$PENDING_REGISTRATION_MODULE_REL failed (exit $rc): ${err:-$(printf '%s\n' "$out" | tail -n1)}"
        return 0
    fi
    if [ "$(printf '%s\n' "$out" | tail -n1)" = "NONE" ]; then
        PENDING_STATE="none"
        return 0
    fi
    if [ "$(printf '%s\n' "$out" | tail -n1)" != "END" ]; then
        PENDING_UNKNOWN_REASON="$PENDING_REGISTRATION_MODULE_REL gave an incomplete answer"
        return 0
    fi

    local kind t why
    while IFS=$'\t' read -r kind t why; do
        case "$kind" in
            ACTIVE)
                PENDING_ACTIVE_TYPES+="$t"$'\n'
                PENDING_ROWS+="$t: ACTIVE"$'\n' ;;
            INERT)
                PENDING_ROWS+="$t: INERT ($why)"$'\n' ;;
        esac
    done <<< "$out"
    PENDING_ACTIVE_TYPES="${PENDING_ACTIVE_TYPES%$'\n'}"
    PENDING_ROWS="${PENDING_ROWS%$'\n'}"
    # A token-less reader answers unknown(no token) for every entry. Name that
    # case: it is the common local one, and its remedy is a sign-in, not a fix.
    if [ -z "$token" ] && [ -z "$PENDING_ACTIVE_TYPES" ]; then
        PENDING_UNKNOWN_REASON="no GitHub token (GITHUB_TOKEN unset and 'gh auth token' gave none), so no declared wait could be confirmed ACTIVE"
        PENDING_ROWS=""
        return 0
    fi
    PENDING_STATE="ok"
    return 0
}

# Copy stdin to stdout without the FIRST line equal to $1 (a trailing CR is
# ignored for the comparison and kept on every line that stays).
_pending_registration_drop_line() {
    local want="$1" l removed=0
    while IFS= read -r l || [ -n "$l" ]; do
        if [ "$removed" = 0 ] && [ "${l%$'\r'}" = "$want" ]; then
            removed=1
            continue
        fi
        printf '%s\n' "$l"
    done
}

# _pending_registration_py_shape <schemas_dir> <type> <sha>
#
# The Python half of CI's shape check (`absorb_set` in the schemas module):
# exactly one `from .per_type.<m> import T` line and exactly one `    "T",`
# `__all__` entry in the Python barrel, and `per_type/<m>.py` present. Where
# CI would call the entry INERT, this side must not absorb either.
#
# Read from the schemas checkout's HEAD COMMIT, as CI's module reads it, never
# from its working tree: a dirty or CRLF-converted working copy must not pass a
# shape the committed barrel fails. No CR is stripped, for the same reason — a
# line ending in CR does not match CI's `^...$` either. Writes nothing. Exit 0
# when the shape holds, else prints why and exits 1.
PENDING_REGISTRATION_PY_DIR_REL="src/qontinui_schemas/generated"

# _pending_registration_head_sha <schemas_dir> — the ONE commit every read is
# made at. Resolved once and reused, because the checkout is shared: a peer's
# commit or checkout between two `HEAD:` reads would otherwise mix commits,
# where CI reads everything at one. Refuses a directory that is not itself the
# top of a git checkout (a plain directory nested in some other repo would
# otherwise read THAT repo's HEAD). Prints the sha, or why not and exits 1.
_pending_registration_head_sha() {
    local schemas="$1" top want sha
    if ! top="$(git -C "$schemas" rev-parse --show-toplevel 2>/dev/null)" || [ -z "$top" ]; then
        printf '%s is not a git checkout, so its committed declaration cannot be read' "$schemas"
        return 1
    fi
    want="$(cd "$schemas" 2>/dev/null && pwd -P)"
    top="$(cd "$top" 2>/dev/null && pwd -P)"
    if [ -z "$want" ] || [ "$want" != "$top" ]; then
        printf '%s is not the top of a git checkout (git resolves %s)' "$schemas" "${top:-nothing}"
        return 1
    fi
    if ! sha="$(git -C "$schemas" rev-parse --verify --quiet 'HEAD^{commit}' 2>/dev/null)" || [ -z "$sha" ]; then
        printf 'cannot resolve HEAD of %s' "$schemas"
        return 1
    fi
    printf '%s' "$sha"
}

# _pending_registration_count_line <line> — how many lines of stdin equal it,
# ignoring only a TRAILING CR. Not `tr -d '\r'`: that deletes a CR anywhere,
# where Python's splitlines (CI's count) breaks the line there instead.
_pending_registration_count_line() {
    local want="$1" l n=0
    while IFS= read -r l || [ -n "$l" ]; do
        [ "${l%$'\r'}" = "$want" ] && n=$((n + 1))
    done
    printf '%s' "$n"
}
PENDING_REGISTRATION_TS_DIR_REL="ts/src/generated"
_pending_registration_py_shape() {
    local schemas="$1" t="$2" sha="$3" barrel l module="" from_count=0 all_count=0
    if ! barrel="$(git -C "$schemas" show "$sha:$PENDING_REGISTRATION_PY_DIR_REL/__init__.py" 2>/dev/null)"; then
        printf 'cannot read %s:%s/__init__.py in %s' "$sha" "$PENDING_REGISTRATION_PY_DIR_REL" "$schemas"
        return 1
    fi
    while IFS= read -r l; do
        case "$l" in
            "from .per_type."*" import $t")
                module="${l#from .per_type.}"
                module="${module% import "$t"}"
                case "$module" in
                    ""|*[!A-Za-z0-9_]*) ;;
                    *) from_count=$((from_count + 1)) ;;
                esac ;;
            "    \"$t\",")
                all_count=$((all_count + 1)) ;;
        esac
    done <<< "$barrel"
    if [ "$from_count" != 1 ]; then
        printf 'the committed Python barrel imports %s from a per_type module %s times, not once' "$t" "$from_count"
        return 1
    fi
    if [ "$all_count" != 1 ]; then
        printf 'the committed Python barrel lists "%s" in __all__ %s times, not once' "$t" "$all_count"
        return 1
    fi
    if ! git -C "$schemas" cat-file -e "$sha:$PENDING_REGISTRATION_PY_DIR_REL/per_type/$module.py" 2>/dev/null; then
        printf 'per_type/%s.py is not in HEAD' "$module"
        return 1
    fi
    return 0
}

# pending_registration_absorb <baseline_dir> <regenerated_dir> <out_dir> <schemas_dir> [<type>...]
#
# Writes <out_dir> as a copy of <baseline_dir> with each absorbable type's
# `.d.ts` removed and its export line removed from `index.ts`. The caller then
# compares <out_dir> with <regenerated_dir> exactly as it compared the baseline.
# <schemas_dir> is the schemas checkout, whose HEAD commit is read for the
# Python half of CI's shape check and never written.
#
# Sets:
#   PENDING_ABSORBED       newline-separated types absorbed
#   PENDING_NOT_ABSORBED   one "<type>: <reason>" line per type declined
# Returns 0 on success, 1 when <out_dir> could not be written (the caller then
# compares against the untouched baseline: absorbing nothing).
pending_registration_absorb() {
    local base="$1" regen="$2" out="$3" schemas="$4"
    shift 4
    PENDING_ABSORBED=""
    PENDING_NOT_ABSORBED=""

    # The same commit the ACTIVE decision read, when it read THIS checkout.
    local sha=""
    [ "${PENDING_HEAD_SCHEMAS:-}" != "$schemas" ] || sha="${PENDING_HEAD_SHA:-}"
    if [ -z "$sha" ] && ! sha="$(_pending_registration_head_sha "$schemas")"; then
        local t
        for t in "$@"; do
            [ -z "$t" ] || PENDING_NOT_ABSORBED+="$t: shape: $sha"$'\n'
        done
        PENDING_NOT_ABSORBED="${PENDING_NOT_ABSORBED%$'\n'}"
        return 0
    fi

    rm -rf -- "$out" 2>/dev/null
    # Copy the CONTENTS into a fresh directory: `cp -R <symlink> <dest>`
    # would copy a symlinked baseline as a link, and the edits below would
    # then write through it into the schemas checkout.
    mkdir -p -- "$out" && cp -R -- "$base/." "$out/" || return 1
    # The baseline may be the read-only SHA-keyed sibling store, and cp -R
    # keeps its modes (finding 51901722).
    chmod -R u+w -- "$out" 2>/dev/null || true

    local t line count tmp why
    for t in "$@"; do
        # A type name becomes a file path below; refuse anything that is not
        # a plain identifier rather than trust the module's own validation.
        case "$t" in
            ""|[0-9]*|*[!A-Za-z0-9_]*)
                [ -z "$t" ] || PENDING_NOT_ABSORBED+="$t: not a type name"$'\n'
                continue ;;
        esac
        line="export type { $t } from './$t';"
        if [ ! -f "$base/$t.d.ts" ]; then
            PENDING_NOT_ABSORBED+="$t: not in the baseline"$'\n'; continue
        fi
        # CI's absorb_set asks these of HEAD, not of the working tree: the
        # file is committed, and its export line is committed exactly once.
        # (Python's splitlines, which CI uses for that count, drops a CR.)
        if ! git -C "$schemas" cat-file -e "$sha:$PENDING_REGISTRATION_TS_DIR_REL/$t.d.ts" 2>/dev/null; then
            PENDING_NOT_ABSORBED+="$t: shape: $t.d.ts is not in HEAD"$'\n'; continue
        fi
        count="$(git -C "$schemas" show "$sha:$PENDING_REGISTRATION_TS_DIR_REL/index.ts" 2>/dev/null \
            | _pending_registration_count_line "$line" || true)"
        if [ "$count" != "1" ]; then
            PENDING_NOT_ABSORBED+="$t: shape: its export line is in the committed index.ts ${count:-0} times, not once"$'\n'; continue
        fi
        if ! why="$(_pending_registration_py_shape "$schemas" "$t" "$sha")"; then
            PENDING_NOT_ABSORBED+="$t: shape: $why"$'\n'; continue
        fi
        if [ -e "$regen/$t.d.ts" ]; then
            PENDING_NOT_ABSORBED+="$t: the regeneration produced it, so it is compared as usual"$'\n'; continue
        fi
        # Compared with any CR stripped, so a CRLF checkout matches; removed
        # whole, so the bytes that remain are the baseline's own.
        count="$(_pending_registration_count_line "$line" < "$base/index.ts")"
        if [ "$count" != "1" ]; then
            PENDING_NOT_ABSORBED+="$t: its export line is in the baseline index.ts $count times, not once"$'\n'; continue
        fi
        if [ -f "$regen/index.ts" ] && [ "$(_pending_registration_count_line "$line" < "$regen/index.ts")" != 0 ]; then
            PENDING_NOT_ABSORBED+="$t: the regenerated index.ts still exports it"$'\n'; continue
        fi
        # A bash read loop, not awk: Git Bash's awk opens files in text mode
        # and drops every CR, which would rewrite a CRLF index.ts wholesale.
        tmp="$out/index.ts.pending-tmp"
        _pending_registration_drop_line "$line" < "$out/index.ts" > "$tmp" \
            && mv -f -- "$tmp" "$out/index.ts" || return 1
        rm -f -- "$out/$t.d.ts" || return 1
        PENDING_ABSORBED+="$t"$'\n'
    done
    PENDING_ABSORBED="${PENDING_ABSORBED%$'\n'}"
    PENDING_NOT_ABSORBED="${PENDING_NOT_ABSORBED%$'\n'}"
    return 0
}
