# Clean, Organize, and Commit (Full Workflow)

Run the complete cleanup workflow: lint, organize notes, review, commit, and push all repos.

## Instructions

This command runs the full workflow. Execute each phase in order.

**IMPORTANT**: All work must be completed. No task is too large. Do not skip any phase.

---

## Special Handling: qontinui-claude-config

**qontinui-claude-config is a configuration repository containing slash commands and markdown files.**

- **DO NOT** run linting/formatting on this repo
- **DO NOT** move any markdown files from this repo
- **ONLY** commit and push changes at the end

This repo is handled separately in Phase 6 (Push All Repos).

---

### Phase 1: Clean Code

**Skip qontinui-claude-config in this phase.**

**NOTE: Code formatting is NOT a concern.** Do NOT run black, isort, prettier, or any formatters.

Run the linting pipeline:

1. **Detect project type** from `pyproject.toml` or `package.json`

2. **Python projects**:
   ```bash
   poetry run ruff check . --fix
   poetry run mypy --package app  # Adjust package name
   ```

3. **Mypy iterative fixing**:
   - Run mypy, capture errors
   - Use parallel Task agents to fix in batches
   - Repeat until 0 errors
   - Common fixes:
     - `# type: ignore[arg-type]` - SQLAlchemy filters
     - `# type: ignore[assignment]` - Column types
     - `# type: ignore[unreachable]` - Runtime null checks
     - `param: str | None = None` - Explicit Optional

4. **JS/TS projects**:
   ```bash
   npm run lint:fix && npm run typecheck
   ```

---

### Phase 2: Organize Notes

**Skip qontinui-claude-config in this phase.** Do not move any files from the config repo.

Move dev files to qontinui-dev-notes:

1. **Find files to move**: `PLAN*.md`, `TODO*.md`, `NOTES*.md`, `IMPLEMENTATION*.md`, temp scripts

2. **Never move**: `README.md`, `CLAUDE.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, `LICENSE.md`

3. **Never touch**: Any files in qontinui-claude-config (all markdown files are intentional)

4. **Move to**: `$PWD/qontinui-dev-notes/{project}/docs/` (or `/scripts/`, `/tests/`)

5. **Add date prefix** if not present: `YYYY-MM-DD-filename.md`

---

### Phase 3: Review Changes

1. **Run**: `git status` and `git diff --stat`

2. **Verify NOT staged**:
   - `CLAUDE.md`
   - `.env` files
   - Credentials/secrets

3. **Identify files that should NOT be in git**:
   - Large binary files (>.exe, .pkg, .pyz, .zip, .tar.gz over 50MB)
   - Build artifacts (build/, dist/, *.pyc, __pycache__/)
   - IDE/editor files (.idea/, .vscode/, *.swp)
   - OS files (.DS_Store, Thumbs.db)
   - Dependencies (node_modules/, .venv/, venv/)
   - Compiled outputs (*.o, *.so, *.dll)

   **For any such files found**:
   - Add patterns to the repo's `.gitignore` BEFORE committing
   - Use `git reset HEAD <file>` to unstage if already staged
   - Files should remain in the working directory but not be tracked

4. **Categorize changes** by type (feat/fix/refactor/style/docs/test/chore)

---

### Phase 4: Commit

**CRITICAL - NEVER INCLUDE**:
- "Generated with Claude"
- "Co-Authored-By: Claude"
- Any AI attribution

**Commit format**:
```
<type>: <short summary>

Areas changed:
- Area 1: Description
- Area 2: Description
```

```bash
git add -A
# Capture output so a pre-commit-hook *rejection* can be reported to coord's
# commit predict-verify loop (cooperative abort-report — see note below).
commit_out="$(git commit -m "$(cat <<'EOF'
<type>: <summary>

<body>
EOF
)" 2>&1)"; commit_rc=$?
printf '%s\n' "$commit_out"
if [ "$commit_rc" -ne 0 ]; then
  # Forward WHY the commit was rejected to coord (best-effort, fail-open —
  # never blocks, never edits git). Then FIX the hook rejection and retry.
  # NEVER `--no-verify`: that bypasses both the hook AND the supervision signal.
  bash <workspace-root>/.claude/scripts/report-commit-abort.sh "$commit_out"
fi
```

> **Cooperative abort-report (commit-action effect signatures §6.2).** A
> pre-commit hook can reject a commit (non-zero exit, no ref change), which the
> coord filesystem observer can't see — it would only infer a reasonless
> Failure after a settle-timeout. The `report-commit-abort.sh` call above is the
> committer cooperatively reporting its *own* rejection to
> `POST /coord/commits/abort`, so the declared commit signature resolves to a
> Failure-with-reason and a per-(repo,branch) oplog of hook rejections accrues.
> It is strictly best-effort and fail-open: it never changes your commit, never
> retries, and never affects exit status. It is **not** a substitute for fixing
> the rejection — investigate the hook output and retry; do not `--no-verify`.
>
> On machines with the **commit-abort wrapper** installed
> (operator-local installer `qontinui-dev-notes/scripts/install-commit-abort-hook.sh`, plan
> `2026-06-06-commit-abort-wrapper`), the hook itself auto-reports rejections
> when gated on (`~/.qontinui/commit-abort-reporter.enabled` or
> `QONTINUI_COMMIT_ABORT_REPORTER_ENABLED`). The explicit call above is the
> universal fallback — it stays correct everywhere (a double report is
> harmless: same match keys, best-effort oplog), so keep it.

---

### Phase 5: Commit Dev Notes

If files were moved to qontinui-dev-notes:
```bash
cd $PWD/qontinui-dev-notes
git add .
git commit -m "docs: archive development notes from {project}"
```

---

### Phase 6: Push All Repos

**Always push all repos that have commits**:

1. **Identify repos with unpushed commits**:
   - Check each repo in qontinui-root:
     - qontinui (core library)
     - qontinui-web
     - qontinui-runner
     - qontinui-devtools
     - qontinui-dev-notes
     - qontinui-train
     - qontinui-finetune
     - qontinui-docs
     - qontinui-gym
     - qontinui-mcp
     - qontinui-schemas
     - qontinui-web-mcp
     - qontinui-claude-config
     - multistate
   - Use `git status` to check if ahead of origin

2. **Push each repo**:
```bash
# For each repo with commits
git push origin <branch>
```

3. **Handle push failures**:
   - If large files block push, add them to .gitignore and recommit
   - Never skip pushing - resolve issues and retry

---

### Final Report

Summarize:
- Linting results (before/after error counts)
- Files moved to dev-notes (with count)
- Commit details for each repo (hash, message summary)
- Push status for each repo (success/failed with reason)
