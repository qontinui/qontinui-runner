---
description: Name this session `<pr-numbers> <descriptive words>` — the open PRs for this session's work plus a short description — and set it. In a runner tab it queues `/rename <name>` into the session's own pane, so the name applies when the turn ends; elsewhere it prints the /rename line to run.
allowed-tools: Read, Grep, Glob, Bash(git rev-parse:*), Bash(git branch:*), Bash(git for-each-ref:*), Bash(gh pr list:*)
---

# Name (Set the Session Name)

Name this session `<pr-numbers> <descriptive words>` — the comma-joined
numbers of the **open (un-merged) PRs for the work this session is doing**,
followed by a short human-readable description of that work — and set it.

Example name: `614,615 headless agent provisioning`.

Claude cannot run the built-in `/rename` itself, and `claude --name` only names
a session at launch (that is how the runner names the sessions it spawns). A
session in a runner tab can still name itself: the runner types
`/rename <name>` into the session's own pane, and Claude Code runs it as soon
as this turn ends.

## Arguments

- `$ARGUMENTS` (optional) — extra description words to prefer, or explicit
  PR numbers to use instead of auto-detecting (e.g. `/name 620 fleet
  auto-response`). When omitted, detect everything from context.

## Instructions

### 1. Determine the repo / worktree

```bash
git rev-parse --show-toplevel
git branch --show-current
```

The session's cwd is inside one of the qontinui worktrees. Use the toplevel
as the repo and note the current branch. Run every command as its own Bash
call, from the cwd: no `;`, `&&`, `|` or `$(…)` joining them (Claude Code
cannot match a compound command against `allowed-tools` and stops to ask), and
keep the subcommand first (`git branch …`, `gh pr list …`, never `git -C …`
or `gh -R …`; `gh pr list` finds the repo from the checkout's remote):
`/triage-sessions` types `/name` into
sessions it does not own, and these spellings are the ones this command's
`allowed-tools` approves, so the read steps never stop at a permission prompt.
Step 4 runs the helper as one plain `python3 <path> …` command, which the
shared settings baseline already approves (`Bash(python3:*)`).

### 2. Collect the "uncommitted" (open) PR numbers for current work

"Uncommitted PR" = an open PR not yet landed to `main`. Scope to the work
**this** session is touching — i.e. open PRs whose head branch matches a
branch you have locally or are checked out on — not every open PR in the repo.

```bash
# Current branch's PR (if one is open)
git branch --show-current
gh pr list --state open --head "$CURRENT_BRANCH" --json number,title,headRefName

# Local branch names, to match against the head branches of open PRs
git for-each-ref --format='%(refname:short)' refs/heads
gh pr list --state open --author @me --json number,title,headRefName,updatedAt
```

Build the candidate set by intersecting open PRs with branches that exist
locally (or are the current branch). Prefer the current branch's PR first.
If `gh` is unavailable or returns nothing, fall back to whatever PR numbers
are evident from the conversation / `$ARGUMENTS`.

- If the user passed explicit numbers in `$ARGUMENTS`, use those verbatim.
- Sort numbers ascending and join with commas, no spaces: `614,615`.
- If exactly zero open PRs are found, omit the number prefix entirely and
  just produce the descriptive name (and say so).

### 3. Derive the descriptive words

Summarise what the session/PRs are about in **2–5 lowercase words** — a
human-readable phrase, not a slug (spaces are fine, matching the example).
Draw from, in priority order:

1. Explicit words in `$ARGUMENTS`.
2. The open PR titles you collected (strip ticket prefixes / boilerplate).
3. The current branch name (de-slugged: `headless-agent-prov` → `headless
   agent provisioning`).
4. The recent conversation topic / files being edited.

Keep the whole name under ~50 chars so it fits one line of `git log`.

### 4. Set the name

The name must be one line, contain no `<`, and stay under ~50 chars. The
helper `name-session.py` is carried by the `command-scripts` skill. Find it
with the Glob tool, not a shell loop, so the lookup needs no Bash approval:
`.claude/skills/command-scripts/_scripts/name-session.py` under the cwd or
one of its parents, else under `$HOME`. Then run it as one plain command with
its absolute path:

```bash
python3 <absolute path to name-session.py> <pr-numbers> <descriptive words>
```

It checks that the pane it would type into is this session's own, and checks
the input prompt immediately before the send, typing nothing when it holds
text. Never run it from a subagent: a subagent shares its parent's pane and
session id, so the check passes and the parent would be renamed. Read its first line and exit code:

| Exit | Line | Meaning |
|---|---|---|
| 0 | `QUEUED: /rename <name>` | The runner typed it into this session's pane. Claude Code runs it when this turn ends. |
| 2 | `NOT-IN-RUNNER` / `NOT-SET` | No runner pane (`$QONTINUI_TERMINAL_ID` unset), or the name is unusable. Nothing was sent. |
| 3 | `UNKNOWN` | The runner could not be read, the pane could not be proven this session's own, or the submit was not confirmed. |
| 4 | `HELD` | The input prompt holds text. Nothing was sent. |

If the helper is not provisioned, or it exits non-zero, print the line for the
user to run instead, fenced:

```
/rename <pr-numbers> <descriptive words>
```

### 5. Report

Say the name, and whether it was queued (exit 0) or needs the user to run the
printed line. A queued name applies after this reply, so never report it as
already set. Add a one-line note of how you derived it (which PRs, which
branch). If useful, mention the user can mirror it to the commit trailer with
`/tag-session <same-name>`.

End the reply after the report. A queued `/rename` runs when the turn ends, so
the turn should end there.
