#!/bin/bash
# Shared helper - SOURCE this, do not execute it.
#
# Make `git -C <path>` actually mean <path>, for the rest of this process.
#
# ── The defect, and why bash needs its own copy ──────────────────────────────
# `git -C <path>` is NOT authoritative. `GIT_DIR` skips repository discovery
# altogether, so `-C` never applies to it; `GIT_WORK_TREE` and `GIT_COMMON_DIR`
# retarget the tree and the common directory the same way. A process that
# inherits any of them runs every scoped query against a DIFFERENT repository,
# and the answer comes back well-formed: nothing errors, nothing is UNKNOWN, and
# the caller reports a confident number about a tree it never looked at.
#
# scripts/lib/git-scope.psm1 fixed that for the PowerShell callers (#494, #498).
# It could not fix the bash ones, and the bash ones are where the hazard is most
# REACHABLE - which the .psm1's own header says out loud without being able to
# act on it:
#
#     "git exports GIT_DIR (and GIT_INDEX_FILE, and often GIT_WORK_TREE) into
#      the environment of every hook it runs. This fleet installs per-clone hooks
#      (scripts/install-repo-git-hooks.sh) and a Stop hook that shells git
#      (scripts/wip-custody-record.sh) ..."
#
# Both files named there are bash. `install-repo-git-hooks.sh` decides which
# repository's `$GIT_DIR/hooks` gets written; `wip-custody-record.sh` runs
# `git -C "$top" stash create` and `git -C "$top" update-ref` - it WRITES a ref.
# Under an inherited `GIT_DIR` those two do not misreport, they mutate the wrong
# repository. That is a strictly larger blast radius than any of the read-only
# probes #494 was written for, and it was carried by the argument for why the
# PowerShell fix mattered.
#
# ── Why a STRIP and not a push/pop window ────────────────────────────────────
# The .psm1 exports `Push-GitScopeIsolation` / `Pop-GitScopeIsolation` because it
# is loaded into a LONG-LIVED PowerShell host that may legitimately belong to a
# git hook needing its own `GIT_DIR` back the moment the query is done. Unsetting
# for the remainder of that process would be a second bug with a bigger blast
# radius than the one being fixed.
#
# A bash consumer here is not that. Every caller is an executed script - a hook
# invocation or a one-shot - that runs, does its git work, and exits. Its own
# process environment is nobody else's, and a child it spawns should inherit the
# stripped view too. So the honest bash primitive is a one-way strip at the top
# of the script, which is STRICTLY STRONGER than a window: there is no interval
# in which an unwindowed call can be added later and go unnoticed.
#
# Deliberately NOT offered: a bash `push`/`pop` pair. Nothing here needs one, and
# an exported-but-unused restore path is the "wiring that reads as protection"
# shape scripts/lint-git-scope.py rule (B) exists to catch. Add it when a caller
# genuinely needs the inherited value back - and note that such a caller wants
# the AMBIENT repository, which usually means it should not be calling `git -C`
# at all.
#
# ── The list is not this file's to invent ────────────────────────────────────
# GIT_SCOPE_ENV_VARS below must name exactly what the .psm1 names. Two private
# copies of one security-relevant list is the failure mode plan
# 2026-08-26-stale-shared-checkouts-read-as-defects D5 was written against, and
# the copy that drifts is the one nobody re-reads. So it is not maintained by
# discipline: scripts/lint-git-scope.py (check #30) parses BOTH files and fails
# the run when they disagree, in either direction.
#
# In particular the `GIT_CONFIG_*` family is absent HERE for the reason it is
# absent THERE, and that reason is measured rather than stylistic: on this fleet
# that family is how `gh auth setup-git` delivers the credential helper, and
# stripping it breaks authenticated fetch for anything inside the window. Read
# the long note in git-scope.psm1 before touching either list; making them
# "congruent with lint-doc-paths.py" is the specific edit that is wrong.

# The variables that redirect git's repository resolution. Ordered exactly as
# git-scope.psm1 orders them, so a human diffing the two sees a real difference
# rather than a reordering.
GIT_SCOPE_ENV_VARS=(
    GIT_DIR
    GIT_WORK_TREE
    GIT_COMMON_DIR
    GIT_NAMESPACE
    GIT_OBJECT_DIRECTORY
    GIT_ALTERNATE_OBJECT_DIRECTORIES
    GIT_INDEX_FILE
    GIT_GLOB_PATHSPECS
    GIT_NOGLOB_PATHSPECS
    GIT_ICASE_PATHSPECS
    GIT_LITERAL_PATHSPECS
)

# git_scope_env_vars
#   Print the list, one name per line. Exists so a suite can ASSERT the list
#   rather than restate it - a suite carrying its own copy is a third copy.
git_scope_env_vars() {
    printf '%s\n' "${GIT_SCOPE_ENV_VARS[@]}"
}

# git_scope_env_names_in_scope
#   Print only those names CURRENTLY SET in this process - what a strip would
#   actually take.
#
#   USED BY THE SUITE, and by nothing else in bash today. An earlier version of
#   this comment also claimed "a caller that wants to say in a diagnostic which
#   poison it found"; no such bash caller exists, and a docstring naming a
#   consumer that is not there is the same "reads as wiring that is not there"
#   shape rule (B) is about, one level down. Asserting the list rather than
#   restating it is a real use on its own - a suite carrying its own copy would
#   be a fourth copy - so this stays. The diagnostic caller the sentence
#   imagined DOES exist in the Python twin (`check-operator-paths.py` names the
#   survivors when a strip fails); add one here when a bash caller genuinely
#   needs it, rather than leaving the claim standing in advance.
#
#   Uses `${!name+set}` rather than `-n`/`-z`: an EMPTY value is still SET, and
#   `GIT_DIR=` is not the same thing as no `GIT_DIR` - git treats the empty
#   string as a real (and broken) setting rather than as absence. Testing for
#   non-emptiness here would leave that case behind, which is the fail-open
#   direction.
git_scope_env_names_in_scope() {
    local name
    for name in "${GIT_SCOPE_ENV_VARS[@]}"; do
        if [ -n "${!name+set}" ]; then printf '%s\n' "$name"; fi
    done
}

# git_scope_strip
#   Unset every scope-redirecting variable for the remainder of this process and
#   for everything it spawns. Idempotent; safe under `set -u` (unset on an absent
#   name is not an error).
#
#   IT VERIFIES ITS OWN POSTCONDITION rather than assuming it. `unset` fails on a
#   readonly variable, and a bare `unset ... || true` would swallow exactly the
#   case where the strip did NOT happen - leaving the caller believing it is
#   isolated when it is not. That is the fail-open direction, and it is the same
#   shape as every other defect this module is about: a confident answer about
#   something never actually checked.
#
#   Returns 0 when nothing on the list remains set, 1 otherwise, having named the
#   survivors on stderr. A caller running under `set -e` therefore ABORTS rather
#   than proceeding unisolated, which is the right direction for a script that
#   mutates on the strength of a scoped git answer. A caller that must not abort
#   (a hook) should read the return value explicitly and say what it is going to
#   do about it, not silence it.
#   The survivor list is a STRING, not an array. `${#arr[@]}` on an empty array
#   is an "unbound variable" error under `set -u` before bash 4.4, and this file
#   is sourced by hooks whose bash version is not this repo's to choose - a strip
#   helper that aborts the caller on an OLD SHELL, in the exact branch that
#   normally never runs, is a worse failure than the one it guards.
git_scope_strip() {
    local name residual=""
    unset "${GIT_SCOPE_ENV_VARS[@]}" 2>/dev/null || true
    for name in "${GIT_SCOPE_ENV_VARS[@]}"; do
        if [ -n "${!name+set}" ]; then residual="$residual $name"; fi
    done
    if [ -n "$residual" ]; then
        echo "git_scope_strip: FAILED to clear${residual} (readonly?) -- scoped \`git -C\` queries in this process are NOT isolated and may answer about another repository" >&2
        return 1
    fi
    return 0
}
