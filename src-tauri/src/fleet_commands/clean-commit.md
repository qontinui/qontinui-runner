# Clean, Organize, and Commit (Full Workflow)

Run the complete cleanup workflow: lint, organize notes, review, commit, and push all repos.

## Instructions

This command runs the full workflow. Execute each phase in order.

**IMPORTANT**: All work must be completed. No task is too large. Do not skip any phase.

### Safety Rules

A workspace root is usually shared. It holds other sessions' linked worktrees,
other sessions' uncommitted work in the primary checkouts, and sometimes an
unrelated product's checkout. These rules bound every commit and push this
command makes:

1. **Your starting checkout, plus primary checkouts for everything else.** The
   checkout you start `/clean-commit` in is this run's own, whatever its kind: a
   linked worktree (the usual agent case) counts, and it is always pushable.
   Every OTHER repo this run touches, such as the dev-notes repository, must be a
   PRIMARY checkout directly under the workspace root. That means its `.git` is
   a DIRECTORY (`[ -d "$d/.git" ]`; equivalently `git -C "$d" rev-parse
   --git-dir` equals `--git-common-dir`). Another linked worktree has a `.git`
   FILE and belongs to whichever session created it. Worktree containers,
   `*-wt-*` checkouts and nested worktrees are never in scope for that reason.
2. **Push only the repos this run committed to.** Phase 6 never pushes "every
   repo with commits". It pushes exactly the repos in this run's ledger,
   `$R/changed-repos`. The record snippet in Phases 4 and 5 writes that ledger
   immediately before each commit. Apart from your starting checkout, a repo
   that already held unpushed commits when this run first committed there is
   reported, not pushed. So is one that received a commit this run did not make
   after that point. Such commits are a peer's, or another product's.
3. **Commit only this run's own paths outside your starting checkout.** In your
   starting checkout, Phase 4 may stage everything (`git add -A`). In any other
   repo, stage only the paths this run itself changed or moved there, which
   Phase 2 records in `$R/moved`. Never `git add -A` or `git add .`. List every
   other dirty path there as NOT THIS RUN'S, and leave it alone.
4. **Never commit to another product's repo.** A checkout of a different product
   beside yours is not this run's to change, even when it is dirty.

**Shell state.** Environment variables do not persist from one Bash call to the
next, but the working directory DOES. Every fence therefore reads this run's
state from its run directory, never from `$PWD` or from another fence's
variables. Every fence sets `R` to the literal `RUN DIR:` path printed below. That
path is a per-run nonce, so a concurrent `/clean-commit` never touches this run's
directory. That holds even for a sibling subagent sharing this session id.
Initialise it once, at the start of the run, from inside the checkout you are
cleaning:

```bash
start=$(git rev-parse --show-toplevel) && common=$(git rev-parse --path-format=absolute --git-common-dir) ||
  { echo "UNKNOWN -- start /clean-commit inside the checkout you are cleaning"; exit 1; }
BASE=$(dirname "$(dirname "$common")")   # the workspace root: the parent of this repo's PRIMARY checkout
[ -e "$BASE/.git" ] && { echo "UNKNOWN -- $BASE is itself a checkout, not a workspace root"; exit 1; }
# A fresh directory per run (a nonce), never keyed by session: sibling subagents share one
# session id. This creates it and never removes any other directory.
mkdir -p "$HOME/.qontinui/clean-commit-run" && R=$(mktemp -d "$HOME/.qontinui/clean-commit-run/XXXXXXXX") || { echo "UNKNOWN -- cannot create a run directory"; exit 1; }
printf '%s\n' "$start" > "$R/start"; printf '%s\n' "$BASE" > "$R/base"
: > "$R/changed-repos"; : > "$R/moved"; : > "$R/own-commits"
echo "RUN DIR: $R  START: $start  BASE: $BASE"   # every later fence starts with R="<this RUN DIR>"
```

An UNKNOWN from any fence is never clean. Resolve it, and do not report success
over it.

---

## Special Handling: configuration repositories

**If your workspace includes a configuration repository — one holding slash commands and markdown files (for example the repo your workspace's `.claude` directory comes from) — treat it specially.**

- **DO NOT** run linting/formatting on this repo
- **DO NOT** move any markdown files from this repo
- **ONLY** commit and push changes at the end

This repo is handled separately in Phase 6 (Push All Repos). It is pushed only if
this run committed to it (Safety Rule 2).

---

### Phase 1: Clean Code

**Skip the configuration repository in this phase.**

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

**Skip the configuration repository in this phase.** Do not move any files from the config repo.

Move dev files to your workspace's dev-notes repository — the separate repo
your deployment keeps plans and working notes in. If your workspace has none,
leave the files where they are, skip this phase, and say so in the Final Report.

1. **Find files to move**: `PLAN*.md`, `TODO*.md`, `NOTES*.md`, `IMPLEMENTATION*.md`, temp scripts

2. **Never move**: `README.md`, `CLAUDE.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, `LICENSE.md`

3. **Never touch**: Any files in the configuration repository (all markdown files are intentional)

4. **Move to**: `<dev-notes-repo>/{project}/docs/` (or `/scripts/`, `/tests/`), where `<dev-notes-repo>` is that repository's PRIMARY checkout under your workspace root (Safety Rule 1). Record each destination path, relative to that repository, as one line in `$R/moved`. Phase 5 commits exactly those paths and nothing else there (Safety Rule 3).

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
- Any attribution beyond the harness trailers (no "generated by" lines in the
  subject or body)

**Trailers.** The harness ends every commit message with
`Co-Authored-By: <model> <noreply@anthropic.com>` and `Claude-Session: <url>`;
keep exactly those, and the per-clone `prepare-commit-msg` hook adds
`Session-Id:` and, when a name is set, `Session-Name:`. Aligned 2026-09-09 with
the harness rule; the old "no Co-Authored-By" line was a rule main's own
history had not followed. No repo may reject those trailers: the operator
retired the `no-claude-attribution` `commit-msg` pre-commit hook on 2026-09-25,
and served `git-operations` clause `commit-trailers-are-the-harness's` now says
so (a repo may still reject a "generated by" line in the subject or body). The
hook is being removed from `qontinui-mcp`, `qontinui-hal-mcp`, `qontinui-prm`
and `ui-bridge-mcp`, and `scripts/git-hooks-doctor.sh` flags any reintroduction as
`REJECTS_HARNESS_TRAILERS`.

**Commit format**:
```
<type>: <short summary>

Areas changed:
- Area 1: Description
- Area 2: Description
```

```bash
R="<the RUN DIR printed at the start of the run>"   # this run's own directory
{ [ -s "$R/start" ] && [ -s "$R/base" ] && START=$(cat "$R/start") && BASE=$(cat "$R/base"); } || { echo "UNKNOWN -- run directory $R is not initialised (Start of run): commit and push nothing"; exit 1; }
top=$(git rev-parse --show-toplevel) && head=$(git rev-parse HEAD) || { echo "record: UNKNOWN repo/HEAD -- do not commit"; exit 1; }
# `git add -A` is for your starting checkout only (Safety Rule 3).
[ "$top" = "$START" ] || { echo "REFUSE -- $top is not this run's starting checkout ($START): cd there"; exit 1; }
# Safety Rule 2: record the repo and its pre-commit HEAD in the ledger (first commit only): <sha> <path>.
cut -d' ' -f2- "$R/changed-repos" | grep -qxF -- "$top" || printf '%s %s\n' "$head" "$top" >> "$R/changed-repos"
git add -A
# Capture output so a pre-commit-hook *rejection* can be reported to coord's
# commit predict-verify loop (cooperative abort-report — see note below).
commit_out="$(git commit -m "$(cat <<'EOF'
<type>: <summary>

<body>
EOF
)" 2>&1)"; commit_rc=$?
printf '%s\n' "$commit_out"
# Record the commit as this run's only if its parent is the HEAD read above (else a peer interleaved).
if [ "$commit_rc" -eq 0 ]; then
  new=$(git rev-parse HEAD) && [ "$(git rev-parse "$new^" 2>/dev/null)" = "$head" ] && printf '%s\n' "$new" >> "$R/own-commits" ||
    { printf 'UNKNOWN %s\n' "$top" >> "$R/own-commits"; echo "UNKNOWN -- HEAD moved around this commit in $top"; }
fi
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
> (plan `2026-06-06-commit-abort-wrapper`), the hook itself auto-reports rejections
> when gated on (`~/.qontinui/commit-abort-reporter.enabled` or
> `QONTINUI_COMMIT_ABORT_REPORTER_ENABLED`). The explicit call above is the
> universal fallback — it stays correct everywhere (a double report is
> harmless: same match keys, best-effort oplog), so keep it.

---

### Phase 5: Commit Dev Notes

If files were moved to the dev-notes repository, commit exactly the paths Phase 2
recorded in `$R/moved`. Any other dirty path there is NOT THIS RUN'S (Safety
Rule 3). List it, and leave it staged or unstaged exactly as you found it:
```bash
R="<the RUN DIR printed at the start of the run>"   # this run's own directory
{ [ -s "$R/start" ] && [ -s "$R/base" ] && START=$(cat "$R/start") && BASE=$(cat "$R/base"); } || { echo "UNKNOWN -- run directory $R is not initialised (Start of run): commit and push nothing"; exit 1; }
N="$BASE/<dev-notes-repo-dir>"
[ -d "$N/.git" ] || { echo "UNKNOWN -- $N is not a primary checkout under $BASE (Safety Rule 1): do not commit"; exit 1; }
[ -s "$R/moved" ] || { echo "nothing this run moved -- no dev-notes commit"; exit 0; }
cd "$N" || exit 1
head=$(git rev-parse HEAD) || { echo "record: UNKNOWN HEAD -- do not commit"; exit 1; }
cut -d' ' -f2- "$R/changed-repos" | grep -qxF -- "$N" || printf '%s %s\n' "$head" "$N" >> "$R/changed-repos"   # Safety Rule 2
git add --pathspec-from-file="$R/moved"
# Paths given to commit mean ONLY those are committed, even if a peer staged others.
git commit --pathspec-from-file="$R/moved" -m "docs: archive development notes from {project}" || { echo "commit failed: nothing recorded"; exit 1; }
# Record it as this run's only if its parent is the HEAD read above (else a peer interleaved).
new=$(git rev-parse HEAD) && [ "$(git rev-parse "$new^" 2>/dev/null)" = "$head" ] && printf '%s\n' "$new" >> "$R/own-commits" ||
  { printf 'UNKNOWN %s\n' "$N" >> "$R/own-commits"; echo "UNKNOWN -- HEAD moved around this commit in $N: it will not be pushed"; }
git status --porcelain -z --untracked-files=all | tr '\0' '\n' | sed "s/^/NOT THIS RUN'S (left as found): /"
```

---

### Phase 6: Push All Repos

**Always push every repo this run committed to, and only those (Safety Rule 2)**:

1. **Resolve the repos to push from this run's ledger.** The ledger holds your
   starting checkout, plus the dev-notes and configuration repositories if this
   run committed there. Your starting checkout is always pushable, whatever its
   kind (Safety Rule 1). Any other ledger repo must be a primary checkout
   directly under the workspace root. It is skipped if its pre-run HEAD was
   already ahead of its upstream, because those commits are not this run's. It is
   also skipped if any commit since this run's first commit there is not this
   run's. A commit counts as this run's only when it is listed in
   `$R/own-commits`; a `Session-Id:` trailer does not count, because sibling
   subagents share it. The fence then prints `SKIP -- foreign commit since run
   start`. The repo is also skipped if its checkout has moved, meaning it is off
   its default branch or this run's start is no longer an ancestor of HEAD. That
   prints `SKIP -- checkout moved since run start`:
     ```bash
     R="<the RUN DIR printed at the start of the run>"   # this run's own directory
     { [ -s "$R/start" ] && [ -s "$R/base" ] && START=$(cat "$R/start") && BASE=$(cat "$R/base"); } || { echo "UNKNOWN -- run directory $R is not initialised: push nothing"; exit 1; }
     L="$R/changed-repos"
     [ -f "$L" ] || { echo "UNKNOWN -- ledger $L missing: push nothing"; exit 1; }
     # Commits in "$start..HEAD" of "$top" that are not this run's: ownership is $R/own-commits
     # ALONE, and an "UNKNOWN <path>" line there makes the repo unpushable. 1 = cannot read.
     foreign_since_start() {
       local revs c
       [ -f "$R/own-commits" ] || return 1
       grep -qxF -- "UNKNOWN $top" "$R/own-commits" && { echo "UNKNOWN-commit"; return 0; }
       revs=$(mktemp) && git -C "$top" rev-list "$start..HEAD" > "$revs" || return 1
       while read -r c; do
         grep -qxF -- "$c" "$R/own-commits" || printf '%s\n' "$c"
       done < "$revs"
       rm -f "$revs"
     }
     # Still on the default branch, with this run's start in HEAD's history.
     checkout_in_place() {
       local default
       default=$(git -C "$top" symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's|^refs/remotes/origin/||'); default=${default:-main}
       [ "$(git -C "$top" symbolic-ref --short HEAD 2>/dev/null)" = "$default" ] &&
         git -C "$top" merge-base --is-ancestor "$start" HEAD
     }
     echo "ledger: $(wc -l < "$L") repo(s)"
     while read -r start top; do                   # ledger line: <pre-run HEAD> <repo path>
       if [ "$top" = "$START" ]; then
         echo "$top: PUSH (this run's starting checkout)"; git -C "$top" status -sb | head -1; continue
       fi
       if [ ! -d "$top/.git" ] || [ "$(dirname "$top")" != "$BASE" ]; then
         echo "$top: SKIP -- not a primary checkout directly under $BASE (Safety Rule 1)"; continue
       fi
       if ! checkout_in_place; then
         echo "$top: SKIP -- checkout moved since run start; report, do not push"; continue
       fi
       # No upstream yet (a new branch): measure against the remote default instead.
       if ! prior="$(git -C "$top" rev-list "@{u}..$start" 2>/dev/null)" &&
          ! prior="$(git -C "$top" rev-list "origin/HEAD..$start" 2>/dev/null)"; then
         echo "$top: SKIP -- UNKNOWN whether it held unpushed commits before this run; report, do not push"
       elif [ -n "$prior" ]; then
         echo "$top: SKIP -- held unpushed commits before this run; report, do not push"
       elif ! foreign=$(foreign_since_start); then
         echo "$top: SKIP -- UNKNOWN whether a foreign commit landed since run start; report, do not push"
       elif [ -n "$foreign" ]; then
         echo "$top: SKIP -- foreign commit since run start; report, do not push"
       else
         echo "$top: PUSH"; git -C "$top" status -sb | head -1
       fi
     done < "$L"
     ```
   An empty ledger means this run committed nothing, so there is nothing to push.

2. **Push each repo listed `PUSH`**:
```bash
# For each repo listed PUSH above: the exact path it printed
git -C "<the path printed PUSH>" push origin "<branch>"
```

> **Does a PR still carry this push?** Per repo: if `<branch>` has a PR, coord
> may already have landed it — `CLOSED`, `MERGED`, or still OPEN with its head
> on `origin/main` by content — and the push then succeeds while nothing carries
> it toward `main`. Shared checkouts are routinely parked on exactly such a
> branch. Decide it by
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
