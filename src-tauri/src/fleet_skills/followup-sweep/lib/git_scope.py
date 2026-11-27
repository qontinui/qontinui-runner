"""Shared helper - IMPORT this, do not execute it.

Make a scoped git call actually mean the repository the caller named, for the
Python callers.

-- The defect, and why Python needs its own copy --------------------------
`git -C <path>` is NOT authoritative. `GIT_DIR` skips repository discovery
altogether, so `-C` never applies to it - and neither does `cwd=`, which is the
spelling a `subprocess` caller reaches for first. `GIT_WORK_TREE` and
`GIT_COMMON_DIR` retarget the tree and the common directory the same way. A
process that inherits any of them runs every scoped query against a DIFFERENT
repository, and the answer comes back well-formed: nothing errors, nothing is
UNKNOWN, and the caller reports a confident number about a tree it never
looked at.

`scripts/lib/git-scope.psm1` fixed that for the PowerShell callers (#494, #498)
and `scripts/lib/git-scope.sh` for the bash ones (#509). Neither could fix the
Python ones, and check #30 could not even SEE them: it widened its discovery
from a hand-written `scripts/` roster to `git ls-files` over the whole repo,
but kept an extension filter of `.ps1` / `.psm1` / `.sh`. A directory boundary
drawn by hand was replaced by a LANGUAGE boundary drawn by hand, which is the
same artifact one level over - and Python is where this repo's gating linters
live.

Measured on the tree #509 landed, and the reason this file exists:
`scripts/check-operator-paths.py` - whose `operator-paths` job is a REQUIRED
status check - ran `["git", "-C", root, "diff", ...]` with no strip at all.
Given a diff that really does add an operator-absolute path it exits 1 and
names the line; given the same diff with a decoy `GIT_DIR` inherited it prints
`operator-path guard: clean` and exits 0. A required gate answering "clean"
about a repository it never read is the exact fail-open this module exists to
prevent.

-- Why Python gets TWO shapes where bash gets one -------------------------
`git-scope.sh` deliberately offers only a one-way `git_scope_strip`, because
every bash consumer here is a one-shot script whose process environment is
nobody else's. Two of the three Python consumers are exactly that, and for
them the same one-way strip is the stronger primitive - there is no interval
in which a later-added call can be made outside it.

The third is not, and the difference is real rather than stylistic.
`scripts/lint-doc-paths.py` has a self-test that deliberately WRITES
`GIT_CONFIG_*` into `os.environ` to prove its own strip resists an injected
`remote.origin.url`. A process-wide strip would leave that test unable to
build its own fixture, so it needs the per-call env DICT that `subprocess`
takes natively - which is the shape both existing Python consumers had already
converged on independently. Offering it is therefore not speculative API: it
has a shipped caller, which is the bar `git-scope.sh`'s header sets when it
refuses to ship an unused push/pop pair.

Both shapes are exported because both are USED. Neither is a default: a
one-shot script should strip, and a caller that needs the ambient environment
for anything else should take the dict and pass it.

-- The list is not this file's to invent ----------------------------------
`GIT_SCOPE_ENV_VARS` below must name exactly what `git-scope.psm1` and
`git-scope.sh` name. THREE private copies of one security-relevant list is the
failure mode plan 2026-08-26-stale-shared-checkouts-read-as-defects D5 was
written against, and it is not hypothetical here: before this file existed the
repo carried three Python copies, and one had already drifted -
`breadcrumb-reason-drift.py` was missing `GIT_NAMESPACE`. That is the copy
nobody re-read, and it drifted precisely because check #30 pinned only the two
lists it could see. So the list is not maintained by discipline:
`scripts/lint-git-scope.py` (check #30) parses all THREE files and fails the
run when any pair disagrees, in every direction.

In particular the `GIT_CONFIG_*` family is absent HERE for the reason it is
absent THERE, and that reason is measured rather than stylistic: on this fleet
that family is how `gh auth setup-git` delivers the credential helper, and
stripping it breaks authenticated fetch for anything inside the window. A
caller that genuinely needs it stripped asks for it per-call via
`strip_prefixes` and says why at its own call site - which is what
`lint-doc-paths.py` does, for an arm that reads `remote.origin.url` and so
takes ALL of its answer from config.

Read the long note in `git-scope.psm1` before touching any list.

-- Note on the filename ---------------------------------------------------
The companions are `git-scope.psm1` and `git-scope.sh`; this one is
`git_scope.py`, with an underscore, because a hyphen is not importable. Check
#30 knows both spellings; do not "fix" the inconsistency by renaming.
"""

from __future__ import annotations

import os
import sys
from typing import Iterable, Mapping

#: The variables that redirect git's repository resolution. Ordered exactly as
#: git-scope.psm1 and git-scope.sh order them, so a human diffing the three
#: sees a real difference rather than a reordering.
#: The marker on the next line exists for the byte-identical copies of this file
#: that skills vendor into their own lib/ directory: check #30 exempts only THIS
#: path by identity, and would otherwise read a copy's declaration as a
#: fallback danger probe that disagrees with its floor.
GIT_SCOPE_ENV_VARS = (  # git-scope-ok: the module's own declaration of the list, carried verbatim by vendored copies; not a fallback probe
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_INDEX_FILE",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
    "GIT_LITERAL_PATHSPECS",
)


def git_scope_env_vars() -> tuple[str, ...]:
    """Return the list.

    Exists so a suite can ASSERT the list rather than restate it - a suite
    carrying its own copy is a fourth copy.
    """
    return GIT_SCOPE_ENV_VARS


def git_scope_env_names_in_scope(base: Mapping[str, str] | None = None) -> list[str]:
    """Those names CURRENTLY SET in `base` (default `os.environ`).

    That is what a strip would actually take, and it is MEMBERSHIP rather than
    truthiness: an EMPTY value is still SET, and `GIT_DIR=` is not the same
    thing as no `GIT_DIR` - git treats the empty string as a real (and broken)
    setting rather than as absence. Testing for non-emptiness here would leave
    that case behind, which is the fail-open direction.
    """
    src = os.environ if base is None else base
    return [name for name in GIT_SCOPE_ENV_VARS if name in src]


def git_scope_env(
    base: Mapping[str, str] | None = None,
    strip_prefixes: Iterable[str] = (),
    pins: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """A copy of `base` (default `os.environ`) with every scope-redirecting
    variable removed - the dict to hand to `subprocess(..., env=...)`.

    `strip_prefixes` removes whole families a specific caller needs gone and
    that this module deliberately does not own (`GIT_CONFIG`, whose indexed
    `GIT_CONFIG_KEY_n` / `GIT_CONFIG_VALUE_n` pairs cannot be enumerated, so a
    name list could not express it).

    `pins` is applied AFTER the strip, never instead of it: git rejects
    `literal` combined with an inherited `glob` or `icase` outright - "fatal:
    global 'literal' pathspec setting is incompatible with all other global
    pathspec settings" - so pinning without stripping converts one fatal into a
    different one rather than fixing anything.

    Returns a NEW dict; `os.environ` is not touched. A caller that forgets to
    pass the result to `subprocess` is not isolated, which is why a one-shot
    script should call `git_scope_strip()` instead, and why check #30 requires
    a `.py` file holding a scoped git call to both import this module and
    invoke one of these two entry points.
    """
    src = os.environ if base is None else base
    prefixes = tuple(strip_prefixes)
    env = {
        k: v
        for k, v in src.items()
        if k not in GIT_SCOPE_ENV_VARS
        and not any(k.startswith(p) for p in prefixes)
    }
    if pins:
        env.update(pins)
    return env


def git_scope_strip() -> bool:
    """Remove every scope-redirecting variable from `os.environ`, for the
    remainder of this process and for everything it spawns. Idempotent.

    IT VERIFIES ITS OWN POSTCONDITION rather than assuming it, for the same
    reason `git-scope.sh` does: a strip that silently did not happen leaves the
    caller believing it is isolated when it is not. That is the fail-open
    direction, and the same shape as every other defect this module is about -
    a confident answer about something never actually checked.

    Returns True when nothing on the list remains set. Returns False otherwise,
    having named the survivors on stderr, so a caller that mutates on the
    strength of a scoped git answer can stop, and a caller that must not stop
    can say what it is going to do about it instead of silencing it.

    Deliberately NOT an exception: the bash twin returns a status for exactly
    this reason, and a linter that crashes where it could report is worse than
    one that reports.
    """
    for name in GIT_SCOPE_ENV_VARS:
        try:
            os.environ.pop(name, None)
        except OSError:
            # THE POSTCONDITION BELOW IS ONLY REACHABLE BECAUSE OF THIS.
            # `os.environ.pop` goes through `MutableMapping.pop` -> `__delitem__`
            # -> `unsetenv`, and on failure that RAISES rather than leaving the
            # key set. So without this handler the loop either removed every
            # name -- making `residual` unconditionally empty and `return False`
            # dead code -- or blew up with an uncaught OSError inside a linter.
            # Neither is the contract: the caller was promised a VERDICT, and
            # the bash twin genuinely returns one (its `unset` fails on a
            # readonly variable, which is the case this port copied the SHAPE of
            # without the failure mode that motivates it).
            #
            # Swallowing it here is not losing information: the name is still
            # set, so `git_scope_env_names_in_scope()` sees it and names it on
            # stderr with the rest.
            pass
    residual = git_scope_env_names_in_scope()
    if residual:
        print(
            "git_scope_strip: FAILED to clear "
            + " ".join(residual)
            + " -- scoped git queries in this process are NOT isolated and may"
            " answer about another repository",
            file=sys.stderr,
        )
        return False
    return True
