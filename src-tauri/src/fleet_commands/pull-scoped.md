# Scoped Pull (Safe — Never Touches WIP)

Pull `main` (or whatever the default branch is) for **only the repos this session is actively working on**, and **never** auto-stash, rebase, or otherwise touch a working tree that has uncommitted changes. Strictly more conservative than `/pull-all`.

Use this when you've finished a chunk of cross-repo work and want to bring local default branches up to date without disturbing parallel agents that may have WIP in the same directories.

## Arguments

- `$ARGUMENTS` — `[repo-name ...]` — zero or more repo names (relative to cwd, e.g. `qontinui-runner` or just `runner`).
  - **Empty** → auto-detect scope from working state (see Phase 1).
  - **Non-empty** → use the listed repos as the scope. Names match by suffix, so `runner` matches `qontinui-runner`.

## Hard rules (non-negotiable)

These differ from `/pull-all` and exist specifically to protect parallel-agent state:

1. **Never `git stash` automatically.** If the working tree has uncommitted changes **to tracked files**, that's another agent's (or your own earlier) WIP. Stash + pop loses information on collision (re-staged hunks, deleted-then-modified files) and is silent. We refuse to touch repos with tracked-file WIP and report them. Untracked files are outside that rule: `git stash` without `-u` would not have carried them anyway, a `--ff-only` pull never touches them, and treating them as WIP is what stops a repo being pulled at all (see Phase 2).
2. **Never operate on a non-default branch's tip.** Feature/PR branches belong to whatever agent checked them out. We may *background-fast-forward the local default ref* (`git fetch origin main:main`) — that's reversible and never touches a checked-out tree — but we never `git pull` while a non-default branch is HEAD.
3. **Never `--force` or `reset --hard`.** Period.
4. **Never `git pull` non-fast-forward.** If the local default branch has commits ahead of origin (rare — usually means a local commit nobody pushed), refuse and report. No auto-rebase.
5. **Confirm branch identity before each action.** Per `feedback_wrong_branch_multi_agent_check.md`: another agent may have switched the working dir's branch since you started. Re-check `git branch --show-current` immediately before any state change.

## Instructions

### Phase 1 — Determine scope

If `$ARGUMENTS` lists repo names, use those. Resolve each by suffix match against directory siblings of `$PWD` that contain a `.git/`. Refuse to act on any name that doesn't resolve uniquely.

If `$ARGUMENTS` is empty, auto-detect:

```bash
BASE="$(pwd)"
SCOPE=()
for dir in "$BASE"/*/; do
    [ -d "$dir/.git" ] || continue
    name=$(basename "$dir")
    cd "$dir"

    # Signal 1: working tree has any uncommitted state (modified, untracked) → active work area
    has_state=$(git status --porcelain 2>/dev/null | head -1)

    # Signal 2: checked out on a non-default branch (probably mid-feature)
    current=$(git branch --show-current 2>/dev/null)
    default=$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's|^origin/||')
    [ -z "$default" ] && {
        if   git rev-parse --verify --quiet origin/main   >/dev/null; then default=main
        elif git rev-parse --verify --quiet origin/master >/dev/null; then default=master
        else default=main
        fi
    }
    on_feature_branch=0
    [ -n "$current" ] && [ "$current" != "$default" ] && on_feature_branch=1

    # Signal 3: a recent local commit exists but is unpushed — means I (or a sibling) was just working here
    unpushed=$(git log --oneline @{u}.. 2>/dev/null | head -1)

    if [ -n "$has_state" ] || [ "$on_feature_branch" = "1" ] || [ -n "$unpushed" ]; then
        SCOPE+=("$name")
    fi
done
cd "$BASE"
```

If auto-detect produces an empty scope, **report that and stop** — don't fall back to "pull everything," that's `/pull-all`'s job.

After resolution, print the scope explicitly (`Scope: A, B, C`) so the user sees exactly what's about to be touched.

### Phase 2 — Per-repo decision

For each repo in scope:

```bash
cd "$BASE/$repo"
default=<as resolved in Phase 1>
current=$(git branch --show-current)

# TWO readings, deliberately — the classification below turns on TRACKED
# changes only. `git status --porcelain` counts untracked files, and an
# untracked machine-local artifact is not WIP: it is a `.claude/` dir, a
# `.mcp.json`, a `.coord-mcp-status`, an `agent-worktrees/`, a stray log. On
# this fleet the runner drops several of those into every managed repo, so a
# bare porcelain read means Case B fires forever and the repo is never pulled
# again — measured as exactly that pin in `dev-start.ps1`, where it held
# qontinui-supervisor 26 commits behind main for weeks (qontinui-supervisor#166,
# fixed for that surface by qontinui-claude-config#741). Chasing the artifacts
# one `.gitignore` / `.git/info/exclude` entry at a time does not converge;
# qontinui-supervisor#167 was the third round in one file.
tracked_wip=$(git status --porcelain --untracked-files=no 2>/dev/null)
untracked_count=$(git ls-files --others --exclude-standard 2>/dev/null | wc -l)

# Always cheap to fetch — never touches working tree
git fetch origin --prune 2>&1
```

`/pull-all` already reads the tracked-only form here, for its own reason (an
untracked-only tree auto-stashes an empty stash and the pop reports a
false-positive conflict). This is the same reading for a different reason, so
the two commands now agree on what "WIP" means.

**Phase 1's scope detection is deliberately NOT changed.** There, an untracked
file is a legitimate signal that a directory is an *active work area* — it is
answering "should I look at this repo at all", not "may I move its HEAD".
Narrowing it would shrink the scope rather than widen a mutation.

Then classify into exactly one of these cases and act accordingly. **`$tracked_wip`
is what splits A from B** — empty means Case A, non-empty means Case B.
`$untracked_count` never decides a case; it is only reported.

#### Case A — Current branch IS default, AND no TRACKED-file WIP

This is the only case where we modify the checked-out tree. Action:

```bash
# Verify no tracked WIP once more right before acting (multi-agent paranoia)
[ -z "$(git status --porcelain --untracked-files=no)" ] \
    || { echo "WIP appeared mid-flight; aborting $repo"; continue; }

# Only ff-only — refuse if local has unpushed commits ahead of origin.
# Untracked files cannot be lost here: git itself REFUSES a pull that would
# overwrite one ("untracked working tree files would be overwritten by
# merge"), leaving HEAD unmoved — which lands in the `else` below.
if git pull --ff-only origin "$default" 2>&1; then
    record PULLED "$repo ($untracked_count untracked file(s) left untouched)"
else
    # NAME the cause; do not file both under DIVERGED. A refused ff has two
    # causes here and they need opposite remedies -- rebase/push for one, move
    # a file for the other -- so a single bucket sends the operator the wrong
    # way and hides a collision inside a section headed "local commits ahead of
    # origin". Identify it the way dev-start.ps1 does: intersect the incoming
    # paths with the untracked ones.
    collisions=$(comm -12 \
        <(git diff --name-only HEAD "origin/$default" 2>/dev/null | sort) \
        <(git ls-files --others --exclude-standard 2>/dev/null | sort))
    if [ -n "$collisions" ]; then
        record BLOCKED-UNTRACKED "$repo (these untracked file(s) would be overwritten by incoming commits; move or remove them, then re-run: $(echo "$collisions" | tr '\n' ' '))"
    else
        record DIVERGED "$repo (local has unpushed commits on $default — manual intervention)"
    fi
fi
```

#### Case B — Current branch IS default, working tree has TRACKED-file WIP

**Refuse.** We will not auto-stash. Untracked-only trees do **not** reach this
case — see the two readings above. Report:

```
SKIPPED-WIP: $repo on $default with N modified tracked file(s); not auto-stashing.
  hint: commit or stash manually, then re-run /pull-scoped $repo
```

#### Case C — Current branch is NOT default

Do **not** touch the feature branch in any way (not rebase, not merge, not pull). But it IS safe to background-fast-forward the *local default ref* without checking it out — this update is purely a ref move, never modifies the working tree:

```bash
# Local default ref → matches origin/default, ONLY if local default has no commits ahead.
# If local default has divergent commits, this silently fails — that's correct behavior.
git fetch origin "$default:$default" 2>/dev/null \
    && record DEFAULT-REF-SYNCED "$repo (local $default fast-forwarded; $current still checked out)" \
    || record DEFAULT-REF-DIVERGED "$repo (local $default has commits not on origin; left alone)"

# Also report drift on the feature branch itself, but DO NOT pull it
behind=$(git rev-list HEAD..origin/$current --count 2>/dev/null)
[ "$behind" -gt 0 ] && record FEATURE-BEHIND "$repo ($current is $behind commit(s) behind origin/$current)"
```

#### Case D — No upstream / no remote / detached HEAD

Skip and report. Don't try to repair.

### Phase 3 — Final report

Print one categorized roll-up. Use these section headers (omit any that are empty):

```
## /pull-scoped result

### Pulled (default branch fast-forwarded, no conflicts)
- repo (default-branch) — N new commits; M untracked file(s) left untouched

### Default-ref synced in background (feature branch checked out, local default updated)
- repo — local <default> fast-forwarded by N commits without checkout; <feature> still HEAD

### Skipped — working-tree WIP
- repo — N modified tracked file(s) on <branch>; left untouched (M untracked file(s) present; those are not why)

### Skipped — feature branch only (safe; default ref also synced or noted)
- repo — <feature> is HEAD; feature is X commit(s) behind origin/<feature> (will resolve at next checkout)

### Diverged (local commits ahead of origin)
- repo — local <default> ahead by N commits not on origin; manual rebase/push needed

### Blocked — an untracked file is in the way
- repo — git refused the fast-forward: <paths> would be overwritten by incoming commits; move or remove them and re-run

### Skipped — no upstream / detached / no remote
- repo — reason

### Errors
- repo — error
```

Do **not** report repos that weren't in scope. Do **not** suggest follow-up actions for SKIPPED-WIP repos beyond the existing hint — leave decisions to the user.

## Differences from `/pull-all`

| Behavior | `/pull-all` | `/pull-scoped` |
| --- | --- | --- |
| Repo set | Every repo with `.git/` in cwd | Args, or auto-detect from WIP / non-default branch / unpushed commit |
| What counts as WIP for the pull decision | Tracked changes only | Tracked changes only (same) |
| Tracked-file WIP on default branch | Auto-stash → pull → pop | **REFUSE; report** |
| Untracked files only, on default branch | Pull (untracked left untouched) | Pull (untracked left untouched) |
| Feature branch with WIP | Skip | Skip (same) |
| Local default ahead of origin | `git pull --rebase` | **REFUSE; report as DIVERGED** |
| Background fast-forward of local default ref | Yes (when on feature branch) | Yes (same) |
| Conflict resolution | Auto-resolve via heuristics | Never reaches conflict; refuses any path that could |

## Rules

- **Never auto-stash.** If a repo has tracked-file WIP, skip it cleanly and tell the user. They'll commit or stash manually and re-run.
- **Untracked files are not WIP for this decision.** They never block a pull and are never touched by one; git's own refusal is the guard for the single case that could lose them (an incoming commit adding the same path). Skipping a repo because of one is how a repo goes months without a pull.
- **Never rebase or merge.** `--ff-only` only. If origin has diverged from local default, refuse.
- **Never modify a feature branch.** Period. Even if it's "just" `git pull --ff-only` — the feature branch belongs to whoever is on it.
- **Re-check branch right before acting.** Multi-agent collisions can switch branches between your `git status` and your `git pull`. The cost of re-checking is one cheap shell call.
- **Print the scope before acting** so the user can interrupt if it's wrong.
- **Be silent about repos out of scope.** This command's whole reason for existing is "do less than `/pull-all`" — don't widen by surfacing them.
