---
description: Suggest a session name of the form `<pr-numbers> <descriptive words>` — the open PRs for this session's work plus a short description — and emit it as a ready-to-run /rename line.
---

Allowed tools: Read, Bash, Grep, Glob

# Name (Suggest Session Name)

Suggest a session name of the form `<pr-numbers> <descriptive words>` —
the comma-joined numbers of the **open (un-merged) PRs for the work this
session is doing**, followed by a short human-readable description of that
work. Emit it as a ready-to-run `/rename <name>` line.

Example output:

```
/rename 614,615 headless agent provisioning
```

## Arguments

- `$ARGUMENTS` (optional) — extra description words to prefer, or explicit
  PR numbers to use instead of auto-detecting (e.g. `/name 620 fleet
  auto-response`). When omitted, detect everything from context.

## Instructions

### 1. Determine the repo / worktree

```bash
git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || pwd
git -C "$PWD" branch --show-current 2>/dev/null
```

The session's cwd is inside one of the qontinui worktrees. Use the toplevel
as the repo and note the current branch.

### 2. Collect the "uncommitted" (open) PR numbers for current work

"Uncommitted PR" = an open PR not yet landed to `main`. Scope to the work
**this** session is touching — i.e. open PRs whose head branch matches a
branch you have locally or are checked out on — not every open PR in the repo.

```bash
# Current branch's PR (if one is open)
git -C "$REPO" branch --show-current
gh -R <owner/repo> pr list --state open --head "$CURRENT_BRANCH" --json number,title,headRefName

# Local branches that are ahead of main and have an open PR
git -C "$REPO" for-each-ref --format='%(refname:short)' refs/heads
gh -R <owner/repo> pr list --state open --author @me --json number,title,headRefName,updatedAt
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

### 4. Output

Print exactly one fenced, ready-to-run line:

```
/rename <pr-numbers> <descriptive words>
```

Then a one-line note of how you derived it (which PRs, which branch). If
useful, mention the user can mirror it to the commit trailer with
`/tag-session <same-name>`.

Do **not** run `/rename` yourself — just suggest the line for the user to run.
