# Improve All - Comprehensive Code Improvement

Run the complete cleanup workflow followed by all analysis and improvement tasks. This is a LONG-RUNNING, AUTONOMOUS command that makes extensive changes without supervision.

## How Continuation Works

**This is simple - no checkpoints needed!**

1. You work on the task until complete
2. When done, output `[TASK_COMPLETE]` and the runner stops
3. If your session ends before completion (timeout, context limit, etc.):
   - The runner saves your output to the database
   - When resumed, you get your previous output as context
   - You review what was done and continue from there

**Why this works:**
- The runner tracks task state in the database (`task_runs` table)
- `sessions_count` tracks how many sessions have run
- `output_log` contains cumulative output from all sessions
- On continuation, you see what the previous session accomplished

**Your job:** Work on the task. Output `[TASK_COMPLETE]` when ALL work is done.

---

## Core Principles

**IMPORTANT: Complete ALL beneficial work. The following are NOT valid reasons to skip tasks:**
- Task complexity or size
- Amount of code to write
- Need for "careful analysis" (analysis is expected, not an obstacle)
- Task being a "feature" rather than a "fix"
- Context limits (if you run out, the runner will continue)

**The ONLY valid reason to defer a task is:**
- Design decisions that genuinely require user input (not obvious best choices)

**These are NOT valid reasons to defer — always fix them:**
- **Code modularity**: Duplicate code should always be extracted into shared helpers. Clean code and DRY are not optional.
- **Implementation completeness**: If types, bindings, or interfaces exist in one language/layer, add the corresponding definitions in all other languages/layers that consume them.
- **Security improvements**: Use proper randomness (not hash-based pseudo-random), proper encoding, proper validation. Better security is never "optional" or "nice to have."
- **Development effort**: The amount of work required is never a reason to skip or defer. If it's the right thing to do, do it.

**For tasks requiring user input:**
- Still analyze the task thoroughly
- Present the analysis and options in the final report
- Explain why user input is specifically needed

**Use subagents liberally** to parallelize work and maximize efficiency.

**NEVER tell the user to run commands manually.** The runner handles continuation automatically.

---

## Structured Finding Output Format

**IMPORTANT: Use structured finding markers to categorize all issues, improvements, and observations.**

When you discover issues, complete work, or identify items needing attention, output them using this format:

```
[FINDING:category_id:severity]
Title: Brief title
Description: What was found or done
File: path/to/file (if applicable)
Line: line number (if applicable)
Resolution: What you fixed (if you fixed it)
[/FINDING]
```

### Categories

**Auto-fixable (fix these during the workflow):**
- `code_bug` - Code bugs → Fix and report with Resolution
- `security` - Security issues → Fix and report with Resolution
- `test_issue` - Test problems → Fix and report with Resolution
- `documentation` - Doc issues → Fix and report with Resolution

**Informational (no action needed):**
- `already_fixed` - Fixed in this or previous session
- `expected_behavior` - By design, not a bug
- `warning` - Something to be aware of

**Manual intervention required:**
- `config_issue` - Configuration/environment problem (user handles)
- `runtime_issue` - Operational issue, not a code bug
- `data_migration` - Requires database admin intervention

**Needs user input (use `:needs_input` modifier):**
- `enhancement` - Improvement that needs user decision
- `todo` - TODO that needs user decision
- `performance` - Performance trade-off needing user input

### When User Input is Needed

Add `:needs_input` and include Question/Options fields:

```
[FINDING:enhancement:medium:needs_input]
Title: Caching strategy decision
Description: Multiple caching approaches are possible
Question: Which caching strategy should we use?
Options: Redis (distributed) | In-memory (simple) | Both (hybrid)
[/FINDING]
```

### Severity Levels

- `critical` - System-breaking, security vulnerabilities, data loss
- `high` - Major functionality broken
- `medium` - Should be addressed soon
- `low` - Minor issues
- `info` - Informational only

**Output findings as you work.** The qontinui-runner will parse these markers and display a categorized report in the Monitor tab.

---

## Target Repositories

All repositories in qontinui-root:

| Repository | Type | Notes |
|------------|------|-------|
| qontinui | Python | Core library |
| qontinui-web | Python + TypeScript | Web app (backend + frontend) |
| qontinui-runner | TypeScript + Rust | Tauri desktop app |
| qontinui-devtools | Python | Development utilities |
| qontinui-train | Python | ML training pipelines |
| qontinui-finetune | Python | Model fine-tuning |
| qontinui-prm | Python | Process Reward Model training pipeline |
| qontinui-docs | Docusaurus | Documentation site |
| qontinui-gym | Python | RL training environment |
| qontinui-mcp | Python | MCP server for runner |
| qontinui-hal-mcp | Python | HAL-as-MCP server (GhostDesk-compatible screen+input surface) |
| qontinui-schemas | Python/JSON | Shared schema definitions |
| qontinui-web-mcp | Python | MCP server for web |
| multistate | Python + Docusaurus | State machine library |
| qontinui-design-tokens | CSS + TypeScript | Shared design tokens package |
| qontinui-lib-mcp | Python | MCP library |
| qontinui-mobile | TypeScript | Mobile app |
| qontinui-navigation | TypeScript | Navigation module |
| ui-bridge | TypeScript | UI Bridge SDK monorepo |
| ui-bridge-auto | TypeScript | UI Bridge automation library |
| ui-bridge-mcp | Python | UI Bridge MCP server |
| qontinui-wrappers | TypeScript | Reference UI Bridge wrapper apps (gmail, v0) — pnpm + turborepo monorepo |
| qontinui-supervisor | Rust | Dev supervisor for runner process management |
| qontinui-inspect | Rust + TypeScript | Tauri native accessibility inspector (sibling path dep on qontinui-runner) |
| qontinui-workflow-ui | TypeScript | Shared workflow builder UI components |
| qontinui-workflow-utils | TypeScript | Shared workflow builder utilities |
| qontinui-setup-mcp | Python | MCP server for setup/installation |
| qontinui-claude-config | Markdown | **Config only - commit/push at end, no analysis** |
| qontinui-dev-notes | Markdown | **Notes only - commit/push at end, no analysis** |

**IMPORTANT: qontinui-claude-config and qontinui-dev-notes are special** - only commit and push changes at the very end of the workflow. Do NOT run analysis or improvements on these repos.

---

## Workflow Steps

### Step 1: Check What Was Done Before

If this is a continuation session, review the previous output provided in your prompt. Identify:
- Which repos were already processed
- What work was completed
- What work remains

If starting fresh, proceed to Step 2.

### Step 2: Review and Commit Dirty Repos

**CRITICAL: Before pulling, review and commit all uncommitted work. Never stash — stashing has caused lost work.**

For each repo with uncommitted changes:

1. List dirty files with `git status --porcelain`
2. **Review what should be committed:**
   - Dev notes (`PLAN*.md`, `TODO*.md`, `NOTES*.md`, temp scripts) → move to `qontinui-dev-notes` repo, not the project repo
   - Build artifacts (`target/`, `dist/`, `node_modules/`, `*.dll`, `*.pdb`) → do NOT commit, add to `.gitignore` if missing
   - Actual code changes → commit with a descriptive message based on `git diff --stat`
3. Stage only the appropriate files (use `git add <specific-files>`, not `git add -A` blindly)
4. Commit with a message describing the changes
5. **Do NOT push yet** — just commit locally

After committing, verify every repo has a clean working tree. If any repo still shows dirty files (build artifacts, etc.), ensure they are gitignored, not committed.

### Step 3: Pull All Repositories

**Sync default branches in bulk. Leave PR branches and feature branches alone — only report their drift.**

The principle: improve-all does its work on the default branch (`main` or `master` depending on the repo). Pulling a feature branch you're not actively driving creates conflicts you have to re-derive context for; pulling a branch with an open PR rewrites someone's review snapshot. Neither belongs in an autonomous workflow.

For each repo, do all of the following:

1. Detect the default branch (handles `main` vs `master` per repo).
2. Fetch from origin.
3. If the **default branch** is checked out and behind, fast-forward it. No rebase needed — pulls into clean default branches are always fast-forwards (we never commit to default before pulling).
4. If a **non-default branch** is checked out, do NOT pull it. Instead:
   - Check `gh pr list --head $branch --state open` — if a PR exists, mark this branch as "PR-protected; do not touch."
   - Either way, just report whether the branch has drifted vs `origin/$branch` and whether `origin/main`/`master` has new commits the branch could rebase onto when next picked up.
5. **Do NOT push anything.** Pulling readies us for a clean Step 16 push; pushing now could overwrite remote work from other agents.

```bash
BASE="$PWD"
declare -a DRIFTED_FEATURE_BRANCHES=()
declare -a PR_PROTECTED_BRANCHES=()

for repo in qontinui qontinui-web qontinui-runner qontinui-devtools \
            qontinui-train qontinui-finetune qontinui-prm qontinui-docs qontinui-gym qontinui-mcp \
            qontinui-hal-mcp qontinui-schemas qontinui-web-mcp multistate qontinui-design-tokens \
            qontinui-lib-mcp qontinui-mobile qontinui-navigation \
            ui-bridge ui-bridge-auto ui-bridge-mcp qontinui-wrappers qontinui-supervisor \
            qontinui-inspect qontinui-workflow-ui qontinui-workflow-utils qontinui-setup-mcp \
            qontinui-claude-config qontinui-dev-notes; do
  if [ ! -d "$BASE/$repo/.git" ]; then continue; fi
  cd "$BASE/$repo"

  # Detect default branch (main or master)
  default=$(git symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's|^refs/remotes/origin/||')
  if [ -z "$default" ]; then default=$(git rev-parse --abbrev-ref origin/HEAD 2>/dev/null | sed 's|^origin/||'); fi
  if [ -z "$default" ]; then default=main; fi

  current=$(git branch --show-current)
  git fetch origin 2>/dev/null

  if [ "$current" = "$default" ]; then
    # Safe to fast-forward sync default branch in place.
    behind=$(git rev-list HEAD..origin/$default --count 2>/dev/null)
    if [ "$behind" -gt 0 ] 2>/dev/null; then
      echo "$repo ($default): $behind commits behind — fast-forward"
      git pull --ff-only origin "$default"
    else
      echo "$repo ($default): up to date"
    fi
  else
    # Non-default branch — DO NOT touch. Just report drift.
    pr_exists=$(gh pr list --head "$current" --state open --json number -q '.[0].number' 2>/dev/null)
    default_ahead=$(git rev-list HEAD..origin/$default --count 2>/dev/null)
    branch_drift=$(git rev-list HEAD..origin/$current --count 2>/dev/null)

    if [ -n "$pr_exists" ]; then
      PR_PROTECTED_BRANCHES+=("$repo:$current (PR #$pr_exists)")
      echo "$repo ($current): PR #$pr_exists open — skipping pull. (default $default has $default_ahead new commits)"
    else
      DRIFTED_FEATURE_BRANCHES+=("$repo:$current")
      echo "$repo ($current): non-default branch — skipping pull. ($branch_drift commits behind origin; default $default has $default_ahead new commits)"
    fi

    # Also fast-forward the default branch *ref* without checking it out, so
    # the next time the user switches to it they're already up to date.
    git fetch origin "$default:$default" 2>/dev/null || true
  fi
done

# Print summary at end so it's visible in autonomous-run logs.
echo ""
echo "=== Step 3 summary ==="
[ ${#PR_PROTECTED_BRANCHES[@]} -gt 0 ] && printf "PR-protected (do not touch): %s\n" "${PR_PROTECTED_BRANCHES[@]}"
[ ${#DRIFTED_FEATURE_BRANCHES[@]} -gt 0 ] && printf "Drifted feature branches (rebase on demand): %s\n" "${DRIFTED_FEATURE_BRANCHES[@]}"
[ ${#PR_PROTECTED_BRANCHES[@]} -eq 0 ] && [ ${#DRIFTED_FEATURE_BRANCHES[@]} -eq 0 ] && echo "All repos on default branch and synced."
```

**Pre-merge sanity for PR branches.** For each entry in `PR_PROTECTED_BRANCHES`, run a quick rename/delete scan against the new `origin/$default` so the user knows whether their open PR is at risk of conflict at merge time:

```bash
for entry in "${PR_PROTECTED_BRANCHES[@]}"; do
  repo="${entry%%:*}"; rest="${entry#*:}"; branch="${rest%% *}"
  cd "$BASE/$repo"
  default=$(git symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's|^refs/remotes/origin/||')
  default=${default:-main}
  echo "--- $repo:$branch vs origin/$default (renames/deletes only) ---"
  git diff "origin/$default...HEAD" --name-status --diff-filter=D | head -20
done
```

This is informational — improve-all does NOT auto-rebase PR branches. It just surfaces which ones might bite you at merge time.

**If a fast-forward fails** (default branch has diverged from origin — should be rare since we don't commit to default before this step), report it and skip rebasing. Switching to interactive rebase mid-autonomous-workflow is a bad pattern; better to surface and let the user resolve.

Report what was synced, what was skipped, and any PR-protected branches with rename/delete drift.

### Step 4: Detect Changed Repositories

**Only process repositories with unpushed commits (all repos should have clean working trees after Step 2).**

```bash
BASE="$PWD"
for repo in qontinui qontinui-web qontinui-runner qontinui-devtools \
            qontinui-train qontinui-finetune qontinui-docs qontinui-gym qontinui-mcp \
            qontinui-hal-mcp qontinui-schemas qontinui-web-mcp multistate qontinui-design-tokens \
            qontinui-lib-mcp qontinui-mobile qontinui-navigation \
            ui-bridge ui-bridge-auto ui-bridge-mcp qontinui-wrappers qontinui-supervisor \
            qontinui-inspect qontinui-workflow-ui qontinui-workflow-utils qontinui-setup-mcp; do
  if [ ! -d "$BASE/$repo/.git" ]; then continue; fi
  cd "$BASE/$repo"
  if [ -n "$(git log @{u}.. 2>/dev/null)" ]; then
    echo "$repo: HAS UNPUSHED COMMITS"
  elif [ -n "$(git status --porcelain)" ]; then
    echo "$repo: HAS UNCOMMITTED CHANGES (should have been committed in Step 2!)"
  else
    echo "$repo: clean (skip)"
  fi
done
```

If no repos have changes, report that all repos are clean and output `[TASK_COMPLETE]`.

### Step 5: Linting and Commit

Run linting fixes (NOT formatting — formatting is not a concern), then commit:
- Fix linting issues: `ruff check . --fix` (removes unused imports, fixes code quality issues)
- Organize notes (move dev files to qontinui-dev-notes)
- Review and commit changes
- **Do NOT push yet** — push happens at the end after all improvements

**Do NOT run black, isort, prettier, or any code formatters** — formatting pre-commit hooks are disabled and code style is not a concern in this project.

### Step 6: Full Audit

Run comprehensive analysis on repos with changes:

```bash
cd $PWD/qontinui-devtools

# Run all analysis tools
poetry run qontinui-devtools analyze /path/to/project --report /tmp/full_audit.html
poetry run qontinui-devtools import check /path/to/project
poetry run qontinui-devtools architecture god-classes /path/to/project
poetry run qontinui-devtools quality dead-code /path/to/project
poetry run qontinui-devtools security scan /path/to/project
poetry run qontinui-devtools types coverage /path/to/project

# Cross-language ID type consistency
poetry run qontinui-devtools cross-lang id-types /path/to/qontinui-runner /path/to/qontinui-web
```

**React health analysis:** For each changed repo that has React as a dependency, run React Doctor to get a health score and diagnostics. This is mandatory for any changed repo containing `"react"` in its `package.json`:

```bash
# Run react-doctor on each changed React repo
# Replace /path/to/changed/repo with each repo that has changes AND contains "react" in package.json
repo_path="/path/to/changed/repo"
if [ -f "$repo_path/package.json" ] && grep -q '"react"' "$repo_path/package.json" 2>/dev/null; then
  npx -y react-doctor@latest "$repo_path" --verbose --yes
fi
```

React repos to check: `qontinui-web/frontend`, `qontinui-runner`, `qontinui-mobile`, `ui-bridge`, `multistate/docs-site`, `qontinui-docs`. Include React Doctor findings in the audit results alongside qontinui-devtools output.

Also gather:
- All TODO/FIXME/HACK comments
- Mypy error count and types
- Test coverage gaps

### Step 7: Security Fixes

**Distinguish real vulnerabilities from false positives:**

1. Analyze each security finding
2. Fix real vulnerabilities (hardcoded secrets, injection, etc.)
3. Document false positives

**Commit**: `fix: address security vulnerabilities`

### Step 8: Architecture Improvements

For each class flagged (high LCOM, many methods):

1. Analyze if refactoring is beneficial
2. Skip classes that are fine (domain models, parsers)
3. Refactor classes that genuinely have too many responsibilities
4. Fix circular dependencies

**Commit**: `refactor: improve code architecture and modularity`

### Step 9: Code Quality

1. Remove dead code (confidence > 0.90)
2. Remove dead imports: `ruff check . --fix`
3. Fix all linting issues

**Commit**: `refactor: remove dead code and fix linting issues`

### Step 10: Fix React Health Issues

**MANDATORY for changed React repos. Do not skip.**

For each changed repo that has `"react"` in its `package.json`, review the React Doctor output from Step 6 and fix all findings by priority:

1. **Critical (fix immediately):** Security vulnerabilities, correctness bugs (e.g., stale closures, missing deps in useEffect)
2. **High (fix):** Performance anti-patterns (unnecessary re-renders, missing memoization, large bundle imports), architecture issues (prop drilling, god components)
3. **Medium (fix if straightforward):** State/effects anti-patterns (derived state stored in useState, effects that should be event handlers), accessibility gaps (missing ARIA attributes, keyboard navigation)
4. **Low (skip):** Style and convention issues — these are cosmetic and not worth the churn

Re-run React Doctor after fixes to verify the score improved:

```bash
npx -y react-doctor@latest /path/to/changed/react/repo --score --yes
```

Use parallel Task agents — one per repo — when multiple React repos have findings.

**Commit**: `fix: address React health issues from react-doctor`

### Step 11: Fix Type Errors

**MANDATORY: Fix ALL type errors. Volume is not an excuse to skip.**

1. Run mypy and capture all errors
2. Categorize by type (arg-type, attr-defined, return-value, etc.)
3. Fix each error - trace through code to understand intent
4. Use parallel Task agents for efficiency
5. Verify all errors are fixed

**Commit**: `fix: resolve type errors and improve type coverage`

### Step 12: Implement TODO Items

1. Categorize each TODO (implement, needs user input, stale)
2. Implement clear TODOs
3. Remove stale TODOs
4. Document items needing user input

**Commit**: `feat: implement TODO items`

### Step 13: Incomplete Feature Detection

Find UI elements and API parameters that exist but don't do anything:
- UI controls that set state but never use it
- API parameters that are accepted but ignored
- Feature flags without implementation

Either implement them or remove the dead code.

**Commit**: `fix: implement incomplete features` or `refactor: remove dead feature code`

### Step 14: Dependency Updates

1. Check for outdated dependencies: `poetry show --outdated`
2. Update dependencies (careful with major versions)
3. Run tests after updates

**Commit**: `chore: update dependencies`

### Step 15: Final Verification

```bash
# Linting passes (no formatting checks)
poetry run ruff check .
poetry run mypy --package <package>

# All tests pass
poetry run pytest
```

### Step 16: Branch, PR, Merge-on-Green, and Generate Report

**NEVER push improve-all's commits directly to a default branch.** A loop committing + pushing on `main` is exactly what caused a fleet-wide CI-red incident on 2026-06-07 (an untrailered, PR-less commit reached `main` via the operator's admin bypass) — see memory [[feedback_no_direct_pushes_to_main_loops_use_branches]]. Even though Steps 5-14 made their commits on the default branch in-place, those commits must NOT be pushed to the default branch. Instead, for each repo with new (unpushed) commits, move them onto a session branch and ship via PR:

1. **Skip any repo whose current branch has an open PR.** Pushing into an open PR branch as part of an autonomous improve-all run rewrites the snapshot reviewers are looking at. Use the `PR_PROTECTED_BRANCHES` list captured in Step 3.
2. For each repo that has unpushed commits on its default branch (`<default>`):
   - Capture the commits made this run: `git rev-list origin/<default>..HEAD`.
   - Create a session branch holding exactly those commits and reset the default branch back to origin so nothing is left staged for a direct default-branch push:
     ```bash
     branch="loop/improve-all-$(date +%Y%m%d-%H%M%S)-${SESSION_SHORT:-$RANDOM}"
     git -C "$BASE/$repo" branch "$branch"          # branch points at current HEAD (the new commits)
     git -C "$BASE/$repo" checkout "$branch"
     git -C "$BASE/$repo" branch -f "<default>" "origin/<default>"   # default ref returns to origin; commits live only on the branch
     ```
   - Push the BRANCH (never the default branch): `git -C "$BASE/$repo" push -u origin "$branch"`.
   - Open a PR naming the loop: `gh pr create --title "improve-all: <repo> autonomous improvements" --body "Autonomous /improve-all run. Commits: <subjects>."` (Session-Id + Session-Name trailers come from each repo's PER-CLONE `prepare-commit-msg` hook, not from the commit command — a clone the installer never ran against emits neither, so an untrailered commit is a missing hook, not a missing name. Install: `qontinui-dev-notes/scripts/install-session-id-hook.sh`.)
   - **Do NOT merge.** Coord is the sole merge authority for `qontinui/*` repos; agents never run `gh pr merge` or `--admin` (CLAUDE.md; coord-served policy `git-operations` `merge-authority`). Opening the PR IS shipping — coord's merge train lands it once checks are green. If checks fail, leave the PR open and surface it in the report.
3. **`qontinui-claude-config` / `qontinui-dev-notes`** (config/notes only): these have no CI gate; for them only, commit + push the default branch directly per the special-repos rule. (They are the carve-out — code repos always go through the branch-first PR flow above.)
4. Generate summary report (see format below) — include each repo's branch name, PR URL, and merge status.

---

## Summary Report Format

```markdown
# Improve All - Summary Report
Date: {date}

## Repositories Processed

| Repository | Status | Changes |
|------------|--------|---------|
| qontinui | Processed | {description} |
| ... | ... | ... |

## Commits Made

1. `abc1234` (qontinui) - fix: address security vulnerabilities
2. `def5678` (qontinui-web) - refactor: improve code architecture
...

## Work Completed

### Security
- Fixed {count} vulnerabilities
- {list of fixes}

### Architecture
- Refactored {count} classes
- Resolved {count} circular dependencies

### Code Quality
- Removed {count} lines of dead code
- Fixed {count} linting issues

### React Health
- Repos analyzed: {list}
- Score before/after: {repo}: {before} -> {after}
- Fixed {count} findings (critical: {n}, high: {n}, medium: {n})

### Type Safety
- Fixed {count} type errors
- Type coverage: {before}% -> {after}%

### TODO Items
- Implemented {count} TODOs
- Removed {count} stale TODOs

### Incomplete Features
- Found and fixed {count} incomplete features

### Dependencies
- Updated {count} packages

## Items Requiring User Input

### 1. {Item Title}
**Context**: {description}
**Analysis**: {what you learned}
**Options**:
- Option A: {description} - Pros/Cons
- Option B: {description} - Pros/Cons
**Recommendation**: {your recommendation}
**Decision needed**: {specific question}

## Before/After Metrics

| Metric | Before | After | Change |
|--------|--------|-------|--------|
| Security issues | {n} | {n} | -{n} |
| React health score | {n}/100 | {n}/100 | +{n} |
| Type errors | {n} | {n} | -{n} |
| TODO items | {n} | {n} | -{n} |
```

---

## Parallel Processing Strategy

Use Task agents liberally:
- Architecture: One agent per class being refactored
- Types: One agent per module with type errors
- TODOs: One agent per TODO or group of related TODOs
- Features: One agent per feature area

Always verify changes compile and tests pass after merging parallel work.

---

## When Complete

After all work is done and the summary report is generated, output:

```
[TASK_COMPLETE]
```

The runner will see this and stop the workflow. If you run out of context before completing, just stop - the runner will continue with a new session that has your previous output as context.
