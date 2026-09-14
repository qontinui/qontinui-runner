# Review and Commit Changes

Review all changes and create an organized commit. Optionally push to GitHub.

## Arguments
- `$1` - Optional: "push" to also push after committing

## Instructions

### Phase 1: Review Changes

1. **Show current status**:
   ```bash
   git status
   git diff --stat
   ```

2. **Check for files that should NOT be committed**:
   - `CLAUDE.md` - NEVER commit this
   - `.env` files, credentials, secrets
   - Temporary debug files
   - Large binary files

3. **Check for files to move to dev-notes**:
   - Planning documents (PLAN*.md, TODO*.md)
   - If found, suggest running `/organize-notes` first

4. **Review actual changes**:
   - `git diff` for modifications
   - Summarize what changed in each file

### Phase 2: Categorize Changes

Group changes by type:
- **feat**: New features
- **fix**: Bug fixes
- **refactor**: Code restructuring (like mypy fixes)
- **style**: Formatting, linting
- **docs**: Documentation
- **test**: Test additions/changes
- **chore**: Maintenance tasks

### Phase 3: Create Commit

**CRITICAL RULES**:
- NEVER include "Generated with Claude" or similar
- NEVER include "Co-Authored-By: Claude"
- NEVER include any AI attribution
- NEVER commit `CLAUDE.md`

**Commit message format**:
```
<type>: <short summary>

<detailed description organized by area>

Areas changed:
- Area 1: Description
- Area 2: Description
```

**Create the commit**:
```bash
git add -A  # Or selectively add
# Capture output so a pre-commit-hook *rejection* can be reported to coord's
# commit predict-verify loop (cooperative abort-report).
commit_out="$(git commit -m "$(cat <<'EOF'
<type>: <summary>

<body>
EOF
)" 2>&1)"; commit_rc=$?
printf '%s\n' "$commit_out"
if [ "$commit_rc" -ne 0 ]; then
  # Forward WHY the commit was rejected to coord (best-effort, fail-open).
  # Then FIX the hook rejection and retry — NEVER `--no-verify`.
  bash <workspace-root>/.claude/scripts/report-commit-abort.sh "$commit_out"
fi
```

> **Cooperative abort-report (commit-action effect signatures §6.2).** If a
> pre-commit hook rejects the commit, `report-commit-abort.sh` forwards the
> reason to `POST /coord/commits/abort` (best-effort, fail-open — never blocks
> or edits git) so coord's predict-verify loop records a Failure-with-reason
> instead of a reasonless settle-timeout. Not a substitute for fixing the
> rejection; never `--no-verify`. On machines with the commit-abort wrapper
> installed (operator-local installer `qontinui-dev-notes/scripts/install-commit-abort-hook.sh`, plan
> `2026-06-06-commit-abort-wrapper`) the hook auto-reports when gated on; this
> explicit call is the universal fallback and a harmless double-report there.

### Phase 4: Push (if requested)

If `$1` is "push":
```bash
git push origin <current-branch>
```

### Final Report

Show:
- Files committed
- Commit hash and message summary
- Push status (if applicable)
