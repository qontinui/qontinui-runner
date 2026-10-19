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
# Everything CI decides about the COMMITTED schemas tree is decided by
# qontinui-schemas' own module, never re-derived here, so the two sides cannot
# disagree:
#   - ACTIVE: `metadata_verdict` (date, open runner PR, its head registers T);
#   - SHAPE: the HEAD half of `absorb_set`, through the module's own
#     `py_module_for`, `ts_export_line`, `py_all_line` and `lines_of`
#     (Python's `splitlines`, so every line-break rule is CI's): T's `.d.ts`
#     and per-type `.py` are committed, and each export / import / `__all__`
#     line is committed exactly once.
# Every fact is read at ONE commit (the schemas HEAD, resolved once): the
# checkout is shared, and a peer's commit or uncommitted edit must neither mix
# commits nor be honoured — nor have its code run with the borrowed token. The
# toml and the module are copied out of that commit into a scratch dir and
# imported from there.
#
# The ABSORPTION itself is re-implemented, TS-only, because the module's
# `decide` reads a regenerated WORKING TREE of the schemas repo, and this hook
# is forbidden to write one (see the zero-writes invariant in
# gen-events-drift.sh). It removes T's `.d.ts` and its one export line from a
# scratch copy of the on-disk baseline, and only where the regeneration
# deleted both. A type the regeneration DID produce is compared as usual.
#
# FAILS CLOSED. This is a GATING consumer (the absorption is what lets a push
# through), so an UNKNOWN input withholds the tolerance and never grants it
# [policy: unknown-must-not-render-as-a-default]. No python, no module, a module
# error, no GitHub token, an unreadable PR or commit: absorb nothing, and say
# which.

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

# _pending_registration_head_sha <schemas_dir> — the ONE commit every read is
# made at. Refuses a directory that is not itself the top of a git checkout (a
# plain directory nested in another repo would otherwise read THAT repo).
# Prints the sha, or why not and returns 1.
_pending_registration_head_sha() {
    local schemas="$1" prefix sha
    # `--show-prefix` is empty exactly at the top level: no path-string
    # comparison, so a differently cased spelling on Windows is not refused.
    if ! prefix="$(git -C "$schemas" rev-parse --show-prefix 2>/dev/null)"; then
        printf '%s is not a git checkout, so its committed declaration cannot be read' "$schemas"
        return 1
    fi
    if [ -n "$prefix" ]; then
        printf '%s is not the top of a git checkout (it is %s inside one)' "$schemas" "$prefix"
        return 1
    fi
    if ! sha="$(git -C "$schemas" rev-parse --verify --quiet 'HEAD^{commit}' 2>/dev/null)" || [ -z "$sha" ]; then
        printf 'cannot resolve HEAD of %s' "$schemas"
        return 1
    fi
    printf '%s' "$sha"
}

# pending_registration_active_types <schemas_dir> <scratch_dir>
#
# <scratch_dir> is the caller's to create and remove; the toml and module are
# copied into it from the pinned commit and imported from there, so nothing —
# not even `__pycache__` — can land in the schemas checkout.
#
# Sets:
#   PENDING_STATE          none | ok | unknown
#   PENDING_ACTIVE_TYPES   newline-separated type names: ACTIVE and in shape
#   PENDING_ROWS           one "<type>: ACTIVE|INERT (<reason>)" line per entry
#   PENDING_UNKNOWN_REASON why the answer is unknown (unknown only)
#   PENDING_HEAD_SHA       the schemas commit everything was read at
# Always returns 0: every outcome is an answer the caller must read.
pending_registration_active_types() {
    local schemas="$1" head="$2"
    PENDING_STATE="unknown"
    PENDING_ACTIVE_TYPES=""
    PENDING_ROWS=""
    PENDING_UNKNOWN_REASON=""
    PENDING_HEAD_SHA=""

    local sha listing
    if ! sha="$(_pending_registration_head_sha "$schemas")"; then
        PENDING_UNKNOWN_REASON="$sha"
        return 0
    fi
    PENDING_HEAD_SHA="$sha"
    # Absent from the commit is the one confident "nothing declared"; a git
    # that FAILED to answer is unknown, never "none". Every other question
    # about the file (how the table is spelled, whether it is empty) is the
    # module's to answer.
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
    # "Committed" is not "reviewed": the shared checkout's HEAD may be a
    # feature branch or a peer's unpushed commit, and this module runs with the
    # borrowed token. Run it only when the pinned commit is already on the
    # remote default branch (a pinned sibling behind main is the usual case),
    # or its module is byte-identical to the one there: code CI itself runs.
    local default_ref want got
    default_ref="$(git -C "$schemas" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)"
    [ -n "$default_ref" ] || default_ref="origin/main"
    if ! git -C "$schemas" merge-base --is-ancestor "$sha" "$default_ref" >/dev/null 2>&1; then
        want="$(git -C "$schemas" rev-parse --verify --quiet "$default_ref:$PENDING_REGISTRATION_MODULE_REL" 2>/dev/null || true)"
        got="$(git -C "$schemas" rev-parse --verify --quiet "$sha:$PENDING_REGISTRATION_MODULE_REL" 2>/dev/null || true)"
        if [ -z "$want" ]; then
            PENDING_UNKNOWN_REASON="commit $sha is not on $default_ref and $default_ref has no $PENDING_REGISTRATION_MODULE_REL here, so the module is not run"
            return 0
        fi
        if [ "$want" != "$got" ]; then
            PENDING_UNKNOWN_REASON="$PENDING_REGISTRATION_MODULE_REL at $sha is not the one on $default_ref (commit not on it, module differs), so it is not run"
            return 0
        fi
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
    # xtrace off while the token is in a variable: `bash -x` on a hook is a
    # normal way to debug it, and its trace is what people paste into reports.
    local had_xtrace=0
    case $- in *x*) had_xtrace=1; set +x ;; esac
    local token="${GITHUB_TOKEN:-}"
    if [ -z "$token" ] && command -v gh >/dev/null 2>&1; then
        token="$(gh auth token 2>/dev/null || true)"
    fi

    # The token is a prefix ASSIGNMENT, never an argument to `env`, so it never
    # sits on a command line a process listing can read. `env -u
    # PENDING_REGISTRATION_FIXTURE`: the module honours that fixture seam off
    # GitHub Actions, so a value left in a developer's environment could mark
    # any entry ACTIVE without reading GitHub, a path CI can never take.
    # GITHUB_WORKFLOW goes too, so nothing here can impersonate a workflow.
    local out rc=0
    out="$(GITHUB_TOKEN="$token" PYTHONDONTWRITEBYTECODE=1 \
        env -u PENDING_REGISTRATION_FIXTURE -u GITHUB_WORKFLOW "$py" -I -B -c '
import subprocess
import sys
from pathlib import Path

head_dir, schemas, sha = sys.argv[1], sys.argv[2], sys.argv[3]
sys.path.insert(0, str(Path(head_dir) / "scripts"))
try:
    import pending_registration as m
except Exception as exc:  # an import failure is an answer, not a crash
    print("ERROR\timport failed: %s" % exc)
    sys.exit(3)


def at_commit(path):
    """The file at the pinned commit; None when absent; raise when git fails."""
    # ls-tree, not cat-file -e: an empty listing is "absent", while a git that
    # failed raises, so the whole answer goes UNKNOWN (as the CI Trees.head does)
    # instead of one entry reading "not committed" for the wrong reason.
    r = subprocess.run(["git", "-C", schemas, "ls-tree", sha, "--", path],
                       capture_output=True)
    if r.returncode != 0:
        raise RuntimeError("git ls-tree %s -- %s failed" % (sha, path))
    if not r.stdout.strip():
        return None
    r = subprocess.run(["git", "-C", schemas, "show", "%s:%s" % (sha, path)],
                       capture_output=True)
    if r.returncode != 0:
        raise RuntimeError("git show %s:%s failed" % (sha, path))
    return r.stdout.decode("utf-8")


def committed_shape(t, barrels):
    """The HEAD half of the module s own absorb_set, through its own helpers."""
    module = m.py_module_for(t, barrels[m.PY_BARREL])
    if module is None:
        return "no `from .per_type.<m> import %s` line in the committed %s" % (t, m.PY_BARREL)
    for f in ("%s/%s.d.ts" % (m.TS_DIR, t), "%s/per_type/%s.py" % (m.PY_DIR, module)):
        if at_commit(f) is None:
            return "%s is not committed" % f
    want = {
        m.TS_BARREL: [m.ts_export_line(t)],
        m.PY_BARREL: ["from .per_type.%s import %s" % (module, t), m.py_all_line(t)],
    }
    for barrel, lines in want.items():
        committed = m.lines_of(barrels[barrel])
        for line in lines:
            n = committed.count(line)
            if n != 1:
                return "`%s` is in the committed %s %d times, not once" % (line.strip(), barrel, n)
    return ""


try:
    entries = m.load_entries(Path(head_dir))
    if not entries:
        print("NONE")
        sys.exit(0)
    reader = m.make_reader()
    today = m._today()
    barrels = None
    for e in entries:
        ok, why = m.metadata_verdict(e, reader, today)
        if not ok:
            print("INERT\t%s\t%s" % (e.type, why))
            continue
        if barrels is None:
            barrels = {b: at_commit(b) for b in (m.TS_BARREL, m.PY_BARREL)}
        why = committed_shape(e.type, barrels)
        if why:
            print("INERT\t%s\tshape: %s" % (e.type, why))
        else:
            print("ACTIVE\t%s" % e.type)
    print("END")
except Exception as exc:
    print("ERROR\t%s: %s" % (type(exc).__name__, exc))
    sys.exit(3)
' "$head" "$schemas" "$sha" 2>&1)" || rc=$?
    # Only whether there WAS a token outlives the xtrace-off window.
    local had_token=0
    [ -z "$token" ] || had_token=1
    token=""
    [ "$had_xtrace" = 0 ] || set -x
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
        PENDING_UNKNOWN_REASON="$PENDING_REGISTRATION_MODULE_REL failed (exit $rc): ${err:-${out##*$'\n'}}"
        return 0
    fi
    local last="${out##*$'\n'}"
    if [ "$last" = "NONE" ]; then
        PENDING_STATE="none"
        return 0
    fi
    if [ "$last" != "END" ]; then
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
    if [ "$had_token" = 0 ] && [ -z "$PENDING_ACTIVE_TYPES" ]; then
        PENDING_UNKNOWN_REASON="no GitHub token (GITHUB_TOKEN unset and 'gh auth token' gave none), so no declared wait could be confirmed ACTIVE"
        PENDING_ROWS=""
        return 0
    fi
    PENDING_STATE="ok"
    return 0
}

# _pending_registration_count_line <line> — how many lines of stdin equal it,
# ignoring a trailing CR (a CRLF working copy of an LF-committed file).
_pending_registration_count_line() {
    local want="$1" l n=0
    while IFS= read -r l || [ -n "$l" ]; do
        [ "${l%$'\r'}" = "$want" ] && n=$((n + 1))
    done
    printf '%s' "$n"
}

# Copy stdin to stdout without the FIRST line equal to $1 (a trailing CR is
# ignored for the comparison and kept on every line that stays).
_pending_registration_drop_line() {
    local want="$1" l removed=0 nl
    while :; do
        # A last line with no newline is written back without one, so the
        # copy differs from the regeneration only by the dropped line.
        if IFS= read -r l; then nl=$'\n'; elif [ -n "$l" ]; then nl=""; else break; fi
        if [ "$removed" = 0 ] && [ "${l%$'\r'}" = "$want" ]; then
            removed=1
        else
            printf '%s%s' "$l" "$nl"
        fi
        [ -n "$nl" ] || break
    done
}

# pending_registration_absorb <baseline_dir> <regenerated_dir> <out_dir> [<type>...]
#
# The types are ones pending_registration_active_types returned (ACTIVE, and
# in CI's committed shape). This decides only what the WORKING trees allow:
# writes <out_dir> as a copy of <baseline_dir> with each type's `.d.ts` and its
# one `index.ts` export line removed, where the baseline on disk has both and
# the regeneration deleted both. The caller then compares <out_dir> with
# <regenerated_dir> exactly as it compared the baseline.
#
# Sets:
#   PENDING_ABSORBED       newline-separated types absorbed
#   PENDING_NOT_ABSORBED   one "<type>: <reason>" line per type declined
# Returns 0 on success, 1 when <out_dir> could not be written (the caller then
# compares against the untouched baseline: absorbing nothing).
pending_registration_absorb() {
    local base="$1" regen="$2" out="$3"
    shift 3
    PENDING_ABSORBED=""
    PENDING_NOT_ABSORBED=""

    rm -rf -- "$out" 2>/dev/null
    # Copy the CONTENTS into a fresh directory: `cp -R <symlink> <dest>`
    # would copy a symlinked baseline as a link, and the edits below would
    # then write through it into the schemas checkout.
    mkdir -p -- "$out" && cp -R -- "$base/." "$out/" || return 1
    # The baseline may be the read-only SHA-keyed sibling store, and cp -R
    # keeps its modes (finding 51901722).
    chmod -R u+w -- "$out" 2>/dev/null || true

    local t line count tmp
    for t in "$@"; do
        # A type name becomes a file path below; refuse anything that is not
        # a plain identifier rather than trust the module's own validation.
        case "$t" in
            ""|[0-9]*|*[!A-Za-z0-9_]*)
                [ -z "$t" ] || PENDING_NOT_ABSORBED+="$t: not a type name"$'\n'
                continue ;;
        esac
        line="export type { $t } from './$t';"
        if [ ! -f "$base/$t.d.ts" ] || [ ! -f "$base/index.ts" ]; then
            PENDING_NOT_ABSORBED+="$t: not in the baseline on disk"$'\n'; continue
        fi
        if [ -e "$regen/$t.d.ts" ]; then
            PENDING_NOT_ABSORBED+="$t: the regeneration produced it, so it is compared as usual"$'\n'; continue
        fi
        count="$(_pending_registration_count_line "$line" < "$base/index.ts")"
        if [ "$count" != "1" ]; then
            PENDING_NOT_ABSORBED+="$t: its export line is in the on-disk index.ts $count times, not once"$'\n'; continue
        fi
        if [ -f "$regen/index.ts" ] && [ "$(_pending_registration_count_line "$line" < "$regen/index.ts")" != 0 ]; then
            PENDING_NOT_ABSORBED+="$t: the regenerated index.ts still exports it"$'\n'; continue
        fi
        # A bash read loop, not awk: Git Bash's awk opens files in text mode
        # and drops every CR, which would rewrite a CRLF index.ts wholesale.
        # The temp file lives OUTSIDE the copy, so no entry copied from the
        # baseline (a symlink, say) can be written through.
        tmp="$(mktemp "${TMPDIR:-/tmp}/pending-index-XXXXXXXX")" || return 1
        _pending_registration_drop_line "$line" < "$out/index.ts" > "$tmp" \
            && mv -f -- "$tmp" "$out/index.ts" || { rm -f -- "$tmp"; return 1; }
        rm -f -- "$out/$t.d.ts" || return 1
        PENDING_ABSORBED+="$t"$'\n'
    done
    PENDING_ABSORBED="${PENDING_ABSORBED%$'\n'}"
    PENDING_NOT_ABSORBED="${PENDING_NOT_ABSORBED%$'\n'}"
    return 0
}
