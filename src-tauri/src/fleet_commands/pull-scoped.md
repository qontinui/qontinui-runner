# Scoped Pull (Safe — Never Touches WIP)

Pull `main` (or whatever the default branch is) for **only the repos this session is actively working on**, and **never** auto-stash, rebase, or otherwise touch a working tree that has uncommitted changes. Strictly more conservative than `/pull-all`.

Use this when you've finished a chunk of cross-repo work and want to bring local default branches up to date without disturbing parallel agents that may have WIP in the same directories.

## Arguments

- `$ARGUMENTS` — `[repo-name ...]` — zero or more repo names (relative to cwd, e.g. `qontinui-runner` or just `runner`).
  - **Empty** → auto-detect scope from working state (see Phase 1).
  - **Non-empty** → use the listed repos as the scope. Names match by suffix, so `runner` matches `qontinui-runner`.

## Hard rules (non-negotiable)

These differ from `/pull-all` and exist specifically to protect parallel-agent state:

1. **Never `git stash` automatically.** If the working tree has uncommitted changes, that's another agent's (or your own earlier) WIP. Stash + pop loses information on collision (re-staged hunks, deleted-then-modified files) and is silent. We refuse to touch repos with WIP and report them.
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
status=$(git status --porcelain 2>/dev/null)

# Always cheap to fetch — never touches working tree
git fetch origin --prune 2>&1
```

Then classify into exactly one of these cases and act accordingly:

#### Case A — Current branch IS default, AND working tree is CLEAN

This is the only case where we modify the checked-out tree. Action:

```bash
# Verify clean once more right before acting (multi-agent paranoia)
[ -z "$(git status --porcelain)" ] || { echo "WIP appeared mid-flight; aborting $repo"; continue; }

# Only ff-only — refuse if local has unpushed commits ahead of origin
if git pull --ff-only origin "$default" 2>&1; then
    record PULLED "$repo"
else
    record DIVERGED "$repo (local has unpushed commits on $default — manual intervention)"
fi
```

#### Case B — Current branch IS default, working tree has WIP

**Refuse.** We will not auto-stash. Report:

```
SKIPPED-WIP: $repo on $default with N modified file(s); not auto-stashing.
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
- repo (default-branch) — N new commits

### Default-ref synced in background (feature branch checked out, local default updated)
- repo — local <default> fast-forwarded by N commits without checkout; <feature> still HEAD

### Skipped — working-tree WIP
- repo — N modified, M untracked file(s) on <branch>; left untouched

### Skipped — feature branch only (safe; default ref also synced or noted)
- repo — <feature> is HEAD; feature is X commit(s) behind origin/<feature> (will resolve at next checkout)

### Diverged (local commits ahead of origin)
- repo — local <default> ahead by N commits not on origin; manual rebase/push needed

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
| WIP on default branch | Auto-stash → pull → pop | **REFUSE; report** |
| Feature branch with WIP | Skip | Skip (same) |
| Local default ahead of origin | `git pull --rebase` | **REFUSE; report as DIVERGED** |
| Background fast-forward of local default ref | Yes (when on feature branch) | Yes (same) |
| Conflict resolution | Auto-resolve via heuristics | Never reaches conflict; refuses any path that could |

## Rules

- **Never auto-stash.** If a repo has WIP, skip it cleanly and tell the user. They'll commit or stash manually and re-run.
- **Never rebase or merge.** `--ff-only` only. If origin has diverged from local default, refuse.
- **Never modify a feature branch.** Period. Even if it's "just" `git pull --ff-only` — the feature branch belongs to whoever is on it.
- **Re-check branch right before acting.** Multi-agent collisions can switch branches between your `git status` and your `git pull`. The cost of re-checking is one cheap shell call.
- **Print the scope before acting** so the user can interrupt if it's wrong.
- **Be silent about repos out of scope.** This command's whole reason for existing is "do less than `/pull-all`" — don't widen by surfacing them.
