# Review Completed Plan

Review a just-completed implementation plan. Find unfinished features, check for bugs, fix all issues found, and report what was done.

## Arguments
- `$ARGUMENTS` - Optional: specific area or concern to focus on

## Instructions

### Phase 1: Identify the Plan

1. **Find the plan context** — look for:
   - An active plan in the current conversation (most common)
   - Recent git commits on the current branch that represent the implementation
   - PLAN*.md or TODO*.md files in the working directory or qontinui-dev-notes

2. **Determine scope** — identify which repos/directories were touched:
   ```bash
   # Recent commits on current branch
   git log --oneline -20
   ```
   For multi-repo work, check git status across relevant repos.

### Phase 2: Audit Implementation Completeness

For each item in the plan, verify it was actually implemented:

1. **Read the plan items** and create a checklist
2. **For each planned feature/change**:
   - Search the codebase to confirm the code exists
   - Check that it's wired up end-to-end (not just a stub or dead code)
   - Verify imports, exports, and call sites are connected
   - Look for TODO/FIXME/HACK comments left behind
3. **Flag anything that is**:
   - Missing entirely (planned but not implemented)
   - Partially implemented (skeleton exists but logic is incomplete)
   - Implemented but not connected (dead code, unused exports)

### Phase 3: Check for Bugs

1. **Type checking** — run appropriate checkers:
   - Python: `poetry run mypy --package <pkg>` or `ruff check .`
   - TypeScript: `npm run type-check` or `npx tsc --noEmit`
   - Rust: `cargo check` / `cargo clippy`

2. **Lint** — run linters on changed files:
   - Python: `poetry run ruff check .`
   - TypeScript: `npm run lint`
   - Rust: `cargo clippy`

3. **Logical review** — read through the changed code looking for:
   - Off-by-one errors, missing error handling at boundaries
   - Inconsistent state (e.g., field added to struct but not to serialization)
   - Race conditions or missing awaits
   - Hardcoded values that should be configurable

4. **Integration gaps** — check that:
   - Database schema matches model definitions
   - API endpoints match frontend calls
   - Type definitions are consistent across language boundaries (Rust/TS/Python)

### Phase 4: Fix Everything

**Do not just report issues — fix them.** Work through findings by priority:

1. **Critical bugs first** — type errors, runtime crashes, broken integration points
2. **Unfinished features** — complete any partially implemented or missing plan items
3. **Wiring gaps** — connect dead code, add missing imports/exports/call sites
4. **Lint and type errors** — fix all warnings and errors from checkers
5. **TODO/FIXME cleanup** — implement or remove any leftover stubs

**How to fix:**
- Use subagents to parallelize fixes across multiple repos/languages
- After fixing, re-run the relevant checker (type check, lint, cargo check) to confirm the fix
- If a fix in one repo requires a corresponding change in another (e.g., Rust struct + TS type), make both changes
- If a planned feature is too large to complete inline, implement the core functionality and note what remains

### Phase 5: Verify and Report

1. **Re-run all checkers** on affected code to confirm zero errors:
   - Python: `poetry run ruff check . && poetry run mypy --package <pkg>`
   - TypeScript: `npm run type-check && npm run lint`
   - Rust: `cargo check && cargo clippy`

2. **Report what was done:**

```markdown
## Plan Review Complete

### Completed (from original plan)
- [x] Item 1 — verified working
- [x] Item 2 — verified working

### Fixed During Review
- **[Bug]** Description — what was wrong and how it was fixed (file:line)
- **[Incomplete]** Description — what was missing and what was added (file:line)
- **[Wiring]** Description — what was connected (file:line)

### Remaining (if any)
- [ ] Item — why it couldn't be completed now, what's needed

### Checkers
- Type check: PASS/FAIL
- Lint: PASS/FAIL
- Build: PASS/FAIL
```

### Rules

- **Fix, don't just report** — the goal is zero issues remaining, not a list of findings
- **Be specific** — include file paths and line numbers for every change made
- **Be thorough** — check ALL items from the plan, don't skip any
- **Prioritize** — critical bugs and missing features first, polish last
- **Use subagents** to parallelize fixes across multiple repos/languages when the plan spans several areas
- **Never skip tasks due to size** — break large items into steps and complete them all
