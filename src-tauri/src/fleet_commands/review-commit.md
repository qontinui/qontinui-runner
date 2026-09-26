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
- Commit trailers follow the harness attribution rule: keep the `Co-Authored-By: <model>` and `Claude-Session:` lines the harness supplies, add no other attribution (aligned 2026-09-09)
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
> installed (plan `2026-06-06-commit-abort-wrapper`) the hook auto-reports when gated on; this
> explicit call is the universal fallback and a harmless double-report there.

### Phase 4: Push (if requested)

If `$1` is "push":
```bash
git push origin <current-branch>
```

> **Does a PR still carry this push?** If the current branch has a PR, coord may
> already have landed it — `CLOSED`, `MERGED`, or still OPEN with its head on
> `origin/main` by content — and the push then succeeds while nothing carries it
> toward `main`. A shared checkout parked on a concluded PR's branch is the
> common shape. Decide it by
> `knowledge-base/qontinui-specific/coord-ff-lands.md` → "Pushing to a branch
> whose PR may already have landed": check before the push and again after it,
> and take that section's fresh-branch path when the PR carries nothing.

> **Fail-fast push.** A push that prints NOTHING for 30 s is a credential prompt
> you cannot see, never a slow network — do not wait, do not retry the bare
> command. Inside a runner session the runner already sets the full
> non-interactive posture, so a silent hang there is a BUG: kill it and report it
> (a coord finding, plus the dossier `git-push-hang-credential-helper`). Outside
> one, push with
> `GIT_TERMINAL_PROMPT=0 GIT_ASKPASS=/qontinui-runner/askpass-disabled git -c credential.helper= -c credential.helper='!gh auth git-credential' push …`
> — the empty helper MUST precede the gh helper, because git APPENDS credential
> helpers rather than replacing them. Detail:
> `knowledge-base/qontinui-specific/git-push-non-interactive.md`.

### Final Report

Show:
- Files committed
- Commit hash and message summary
- Push status (if applicable)
