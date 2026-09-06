---
name: tag-session
description: "Set the current Claude Code session's display name so it appears as a `Session-Name: <name>` trailer on every subsequent commit. Persists to `~/.qontinui/session-names/$CLAUDE_CODE_SESSION_ID`, which the per-clone `prepare-commit-msg` hook (qontinui-claude-config/scripts/git-hooks/) reads — the skill checks that hook is installed here, because without it neither trailer appears. Pair with /rename — /rename sets the UI label; /tag-session sets the commit-trailer value."
user-invocable: true
---

# tag-session

Set a human-readable name for the current Claude Code session that
will appear as a `Session-Name: <name>` git trailer on every
subsequent commit in this session (alongside the canonical
`Session-Id: <uuid>` trailer that the same `prepare-commit-msg` hook
injects — **both** trailers depend on that hook being installed in
this clone; see below).

The mechanic: writes the name to
`~/.qontinui/session-names/$CLAUDE_CODE_SESSION_ID`. The hook
(installed by `qontinui-claude-config/scripts/install-guard-hooks.sh`)
reads this file on every commit — *where that hook is installed*. No
restart required — the next `git commit` in a hooked clone picks it
up; in an unhooked one nothing happens, silently.

> **The hook is per-clone — check it is installed before relying on this.**
> Writing the marker file does nothing on a clone whose `.git/hooks/` has no
> `prepare-commit-msg`, and that is the default state of every fresh clone.
> Between 2026-06-07 and 2026-08-06 it was the state of *every* clone, because
> the hook had been deleted from qontinui-dev-notes while its CI gate and its
> three consumers stayed live (see
> `qontinui-dev-notes/plans/2026-08-06-session-id-trailer-hook-delivery.md`).
>
> Run this **from inside the clone you are asking about** — it reports what git
> would do in the current repo, and says so rather than guessing when there is
> no repo to ask about:
>
> ```bash
> # Match the MARKER, not just the filename: a foreign prepare-commit-msg is
> # skipped-and-warned by the installer, never replaced, so mere existence
> # proves nothing. This is the installer's own recognition test.
> hooks=$(git rev-parse --git-path hooks 2>/dev/null)
> if [ -z "$hooks" ]; then echo "UNKNOWN — not inside a git repo"
> elif head -5 "$hooks/prepare-commit-msg" 2>/dev/null \
>        | grep -qF "Qontinui Session-Id trailer auto-injector"; then echo "installed"
> else echo "NOT installed (absent or foreign) — run the installer"; fi
> ```
>
> To install, pass `--git-repo` so the run stays scoped to this clone — without
> it the installer sweeps every repo under the workspace root:
>
> ```bash
> bash <workspace-root>/qontinui-claude-config/scripts/install-guard-hooks.sh --git-repo "$(git rev-parse --show-toplevel)"
> ```
>
> The installer is idempotent. It also sets machine-global
> `git config --global init.templatedir`, which seeds clones created *after* it
> runs — it never reaches a clone that already exists, so those are covered only
> by the walk.
>
> The unit is the CLONE, not the worktree: `git rev-parse --git-path hooks`
> resolves from a linked worktree to its MAIN checkout's hooks dir, so one
> install covers a repo and every worktree off it, and the check above is
> correct to run from inside one. (Same resolution the installer relies on —
> see its own scope note.)

## When to use

- **Right after `/rename <name>`** — `/rename` only changes the
  Claude Code UI label (the title bar / session list); it does NOT
  persist anywhere accessible to git hooks. `/tag-session <same-name>`
  mirrors the label into the commit trailer.
- **At the start of a focused workstream** — gives every commit in the
  session a grep-able label that survives squash-merge and shows up
  in `git log --format="%h %s%n%(trailers:key=Session-Name,valueonly)"`.
- **When picking up a long-running task** — set the name once, then
  every commit you make (including via agents you spawn) carries it.

**Don't use** if you don't actually want a Session-Name trailer —
omitting the marker file is fine; the `Session-Id` UUID trailer
alone is sufficient and is the canonical identifier (it comes from
the same per-clone hook, so "no Session-Name" and "no hook" are
different diagnoses: the second loses `Session-Id` as well).

## Inputs

A single positional argument: the session name.

Conventions (not enforced — the hook accepts any non-empty string):

- Use a slug-like form: lowercase, hyphens, no spaces
  (e.g., `pr-merge-orchestrator`, `coord-readiness-phase-7`).
- Keep it under ~40 chars so it fits on one line of `git log`.
- Reuse the same name you passed to `/rename` so the UI and the
  trailer stay in lockstep.

## What this skill does

1. Confirms `$CLAUDE_CODE_SESSION_ID` is set (errors with a clear
   message if invoked outside Claude Code).
2. Trims whitespace from the input name; rejects empty.
3. `mkdir -p ~/.qontinui/session-names` (idempotent).
4. Writes the name to `~/.qontinui/session-names/$CLAUDE_CODE_SESSION_ID`
   (overwrites if already set).
5. Checks whether *this clone* has the Qontinui `prepare-commit-msg`
   hook — by the installer's own marker, not by filename — and keeps
   five states apart, because each has a different repair: installed;
   absent; present-but-**foreign** (a different hook, which the
   installer will not clobber); present-and-ours but **not
   executable** (git silently skips it, and re-running the installer
   will *not* fix it — it reads as already installed); and
   not-a-git-repo (the check could not run — unknown, never reported
   as absent).
6. Echoes confirmation: the file path written, the name stored, and —
   only when the hook is actually installed — a sample of what the
   next commit's trailers will look like.

## How to run (under Claude Code)

When a user types `/tag-session <name>`, execute the following bash:

```bash
set -e
# Fill in TS_NAME_INPUT with the operator's argument text for /tag-session — ALL
# of it, exactly as typed, single-quoted. A name containing an apostrophe must
# close and reopen the quoting around it, so `josh's session` is written
# 'josh'\''s session'; the old form tolerated a bare apostrophe and this one does
# not, which is the one behaviour this change costs. Leave it empty when they
# invoked /tag-session with no argument: the empty value falls through to the
# usage hint below and exits non-zero, the correct no-argument behaviour.
#
# Do NOT replace this with a shell positional parameter. A dollar sign followed
# by a single digit in a skill body is a HARNESS ARGUMENT PLACEHOLDER, not a
# shell positional: Claude Code substitutes the invocation's argument words into
# this body BEFORE injecting it, indexed from ZERO (the zeroth placeholder is the
# FIRST word), and leaves unfilled positions LITERAL. The name is the whole point
# of this skill and names are routinely multi-word, so under the old positional
# form `/tag-session merge steward aug 12` stored the SECOND word alone — a
# silently wrong session name on every subsequent commit trailer. (This comment
# deliberately spells no such sequence of its own — a literal one here would be
# substituted too, garbling the warning.)
TS_NAME_INPUT=''
if [[ -z "$CLAUDE_CODE_SESSION_ID" ]]; then
  echo "error: \$CLAUDE_CODE_SESSION_ID is not set — are you running inside Claude Code?" >&2
  exit 1
fi
NAME="$(echo -n "$TS_NAME_INPUT" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
if [[ -z "$NAME" ]]; then
  echo "usage: /tag-session <name>" >&2
  echo "  Example: /tag-session pr-merge-orchestrator" >&2
  exit 1
fi
mkdir -p "$HOME/.qontinui/session-names"
echo "$NAME" > "$HOME/.qontinui/session-names/$CLAUDE_CODE_SESSION_ID"
echo "✓ session name set"
echo "  file:  $HOME/.qontinui/session-names/$CLAUDE_CODE_SESSION_ID"
echo "  value: $NAME"
echo ""

# The marker file alone produces nothing: the hook that reads it is per-clone.
# `--git-path hooks` honours core.hooksPath and resolves a linked worktree to
# its MAIN checkout's hooks dir, so this is correct from inside a worktree too.
# `|| true` because `set -e` is on and this is a probe, not a precondition —
# and a failed probe is UNKNOWN, reported as its own state below, never as
# "not installed".
#
# The MARKER test (not the filename) is the installer's own `is_our_hook`,
# verbatim — same string, same `head -5` window — so on the question "is this
# hook ours" the two agree by construction. They are deliberately NOT identical
# on presence: the installer gates on `-f` and tests no mode at all, so a
# correctly-marked but NON-EXECUTABLE hook reads "ours, skipped" to it while git
# silently declines to run it. That gets its own branch below rather than being
# folded into "absent", which would name the wrong repair.
HOOK_MARKER="Qontinui Session-Id trailer auto-injector"
HOOKS_DIR="$(git rev-parse --git-path hooks 2>/dev/null)" || true
# Normalise to absolute — `--git-path` returns a path relative to the CWD in an
# ordinary clone (`.git/hooks`, or `../../.git/hooks` from a subdir). The tests
# below are unaffected (nothing changes directory between here and there), but a
# warning that prints `../../.git/hooks` names no clone. Same case test the
# installer uses for the same reason.
case "$HOOKS_DIR" in
  "" | /* | [A-Za-z]:*) : ;;
  *) HOOKS_DIR="$PWD/$HOOKS_DIR" ;;
esac
HOOK="$HOOKS_DIR/prepare-commit-msg"
# Scope a repair to THIS clone with --git-repo. With no such flag the installer
# walks every repo under the workspace root (46 on this box) — right for
# first-time setup, far too wide for "fix the clone I am in", and actively
# destructive with --force. `--show-toplevel` is also correct from a linked
# worktree: its `.git` is a file, which the installer's `-e` test accepts, and
# the hooks dir resolves to the MAIN checkout's either way.
THIS_REPO="$(git rev-parse --show-toplevel 2>/dev/null)" || true
INSTALLER="<workspace-root>/qontinui-claude-config/scripts/install-guard-hooks.sh"
if [[ -z "$HOOKS_DIR" ]]; then
  echo "? Not inside a git repo here — could not check for the hook."
  echo "  The name is stored regardless; it applies to whichever clone you commit in."
elif [[ ! -f "$HOOK" ]]; then
  echo "WARNING: this clone has NO prepare-commit-msg hook ($HOOKS_DIR)."
  echo "  NEITHER trailer will appear — the name above is being written into a void."
  echo "  Install it here (idempotent; covers this clone and every worktree off it):"
  echo "    bash $INSTALLER --git-repo \"$THIS_REPO\""
  echo "  Omit --git-repo to sweep every repo under the workspace root instead."
  echo "  Either form also sets machine-global git config init.templatedir,"
  echo "  and installs the rest of the guard-hooks component (idempotent)."
elif ! head -5 "$HOOK" 2>/dev/null | grep -qF "$HOOK_MARKER"; then
  echo "WARNING: a FOREIGN prepare-commit-msg hook is installed here ($HOOK)."
  echo "  It is not the Qontinui trailer injector, so NEITHER trailer will appear."
  echo "  The installer skips a foreign hook rather than clobbering it. Inspect it,"
  echo "  then — only if it is safe to REPLACE — re-run scoped to this clone:"
  echo "    bash $INSTALLER --force --git-repo \"$THIS_REPO\""
  echo "  Do NOT drop that flag: bare --force overwrites the foreign hook in every"
  echo "  repo under the workspace root, not just this one."
elif [[ ! -x "$HOOK" ]]; then
  echo "WARNING: the Qontinui hook is present here but NOT EXECUTABLE ($HOOK)."
  echo "  git will not run it, so NEITHER trailer will appear. The installer"
  echo "  considers it already installed and will skip it, so re-running is not"
  echo "  the fix — restore the mode bit:"
  echo "    chmod +x \"$HOOK\""
else
  echo "Next commit in this clone will carry both trailers:"
  echo "  Session-Id:   $CLAUDE_CODE_SESSION_ID"
  echo "  Session-Name: $NAME"
fi
```

Fill in `TS_NAME_INPUT` with the operator's input — the whole argument
string, not just its first word. If they invoked `/tag-session` with no
arg, leave it empty; the block then shows the usage hint and exits 1.

Report the hook state to the operator verbatim — a stored name with no
hook looks identical to a working one until someone greps a commit
and finds nothing.

## Unsetting

To clear the name (Session-Name stops; Session-Id continues wherever
the hook is installed):

```bash
rm "$HOME/.qontinui/session-names/$CLAUDE_CODE_SESSION_ID"
```

No dedicated `/untag-session` skill — it's a one-liner.

## Reading from another session

The name is per-session-UUID. If you switch sessions and want to see
what name a prior session used:

```bash
ls ~/.qontinui/session-names/   # all session-name markers on this machine
cat ~/.qontinui/session-names/<uuid>
```

## Related

- `qontinui-claude-config/scripts/git-hooks/prepare-commit-msg` — the
  source of the hook that reads the marker file. This is the
  template, not an installed hook: it does nothing until the
  installer copies it into a clone. It **moved here from
  `qontinui-dev-notes` on 2026-09-03** (plan
  `2026-09-03-session-id-gate-rejects-the-provenance-the-fleet-actually-writes`,
  Phase 2), because its standalone installer over there was wired into
  no installer script anywhere and therefore reached almost no machine.
- `qontinui-claude-config/scripts/install-guard-hooks.sh` — installs
  the whole guard-hooks component, of which this hook is now the
  `_git_hooks` half: it stamps every repo under the workspace root and
  sets `init.templatedir` so clones made *later* inherit it. Existing
  clones are covered only by the sweep, so re-run it after adding a
  repo. `--git-repo <path>` scopes the sweep to one checkout;
  `--check` reports `absent` / `stale` / `stamped` / `foreign` per
  target; a foreign `prepare-commit-msg` is never clobbered without
  `--force`.
- `qontinui-dev-notes/memory/current_session_id.md` — long-form
  session log; the trailers make it easier to keep up to date.
- `/rename <name>` — Claude Code built-in; sets the UI label.
  `/tag-session <name>` mirrors it into the commit trailers.
