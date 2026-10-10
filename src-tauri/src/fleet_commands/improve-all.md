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

## Safety Rules

A workspace root is usually shared. It holds other sessions' linked worktrees,
other sessions' uncommitted work in the primary checkouts, and sometimes an
unrelated product's checkout. These rules bound every step that commits, lints,
pushes or opens a PR:

1. **Primary checkouts only.** The repos this run discovers are PRIMARY
   checkouts directly under the workspace root, meaning their `.git` is a
   DIRECTORY (`[ -d "$d/.git" ]`; equivalently `git -C "$d" rev-parse
   --git-dir` equals `--git-common-dir`). A linked worktree has a `.git` FILE
   and belongs to whichever session created it. Worktree containers such as
   `agent-worktrees/`, `*-wt-*` checkouts and nested worktrees are never
   targets. Never loosen the test to `-e` or `-f`. `/improve-all` runs from the
   workspace root itself, so it has no starting checkout of its own. This rule
   therefore bounds every repo it touches.
2. **Push only what this run committed.** Pushes and PRs apply ONLY to repos in
   this run's ledger, `$R/changed-repos` (the run directory `$R` is below). The
   record snippet writes a ledger line immediately before EVERY commit this run
   makes (Step 5, Steps 6-14, and commits made by subagents). "Every repo with
   unpushed commits" is NOT the work list. A repo holding unpushed commits this
   run did not make belongs to someone else. Report it and never touch it. For
   the same reason Step 16 refuses to ship a ledger repo that already held
   unpushed commits when this run first committed there, because shipping it
   would carry a peer's commits into your PR. Step 16 also refuses a repo where
   any commit since the run's first commit there is not this run's. A commit is
   this run's ONLY when it is listed in `$R/own-commits`. The record snippet
   appends it there after verifying that its parent is the HEAD it committed
   on. A `Session-Id:` trailer does not count, because sibling subagents share
   it. Otherwise Step 16 prints `SKIP -- foreign commit since run start`. It
   also refuses a repo whose checkout has moved: the default branch must be
   checked out, and the run's start must be an ancestor of HEAD. Otherwise it
   prints `SKIP -- checkout moved since run start`. In either case it touches
   nothing, since resetting the default branch would drop a peer's commit or
   strand this run's.
3. **Never commit or EDIT a peer's work.** Every path that is uncommitted when
   Step 2 or Step 4 runs is recorded as NOT THIS RUN'S. That work is a peer's.
   Never commit, edit, reformat, stash, move, delete or `.gitignore` it. A repo
   holding any NOT THIS RUN'S path is left off the work list (Step 4), so no
   lint, fix or refactor step runs there. An automated fixer runs only in a
   work-list repo whose tree is clean (the pre-fix guard in Step 5). Stage only
   the paths this run itself changed or created (`git add <path>…`, never
   `git add -A` or `git add .`). The record snippet refuses a commit whose staged
   set holds a NOT THIS RUN'S path.
4. **Never commit to another product's repo.** Step 1 DETECTS other products.
   The reference owner is the GitHub owner of the configuration repository's
   `origin`, or of the repo your workspace's `.claude` resolves into; it is never
   taken from a majority vote. A primary checkout whose `origin` owner differs,
   or whose `origin` is missing, not on GitHub or unparseable, is another product
   (fail closed). Two explicit lists override the detection: `other-products` is
   always excluded, and `same-product` overrides a mismatch. Steps 3-16 skip
   other products, and the record snippet refuses to commit there.

**Shell state.** Environment variables do not persist from one Bash call to the
next, but the working directory DOES, so `$PWD` in a later fence is wherever an
earlier fence left it. Every fence therefore reads its state from the run
directory, never from `$PWD` or from a variable set in another fence:

```bash
# Run preamble: the first lines of every fence below that reads run state.
R="<the RUN DIR printed at init>"   # this run's own directory (Step 1)
{ [ -s "$R/base" ] && BASE=$(cat "$R/base") && [ -d "$BASE" ] && [ ! -e "$BASE/.git" ]; } || { echo "UNKNOWN -- run directory $R is not initialised (Step 1): act on nothing"; exit 1; }
SPECIAL_REPOS=$(tr '\n' ' ' < "$R/special-repos"); OTHER_PRODUCTS=$(tr '\n' ' ' < "$R/other-products")
```

`R` is always the literal `RUN DIR:` path Step 1 printed, written into each
fence the same way the branch fence takes its `SHIP` path. The directory is a
per-run nonce, so a concurrent `/improve-all` never reads or clobbers this
run's ledger. That holds even for a sibling subagent sharing this session id. An UNKNOWN from any
fence is never clean. Resolve it before reporting, and never output
`[TASK_COMPLETE]` after an unresolved UNKNOWN.

**Record snippet.** Stage your paths first. Then run this whole snippet, which
ends with the commit itself, in ONE Bash call from inside the repo. It records
the new commit as this run's only when its parent is the HEAD the snippet read
before committing. Otherwise a peer committed in between: it writes
`UNKNOWN <repo path>`, and Step 16 will not ship that repo:

```bash
R="<the RUN DIR printed at init>"   # this run's own directory (Step 1)
[ -f "$R/changed-repos" ] && [ -f "$R/not-this-runs" ] || { echo "UNKNOWN -- no run ledger at $R (Step 1): do not commit"; exit 1; }
top=$(git rev-parse --show-toplevel) && head=$(git rev-parse HEAD) || { echo "record: UNKNOWN repo/HEAD -- do not commit"; exit 1; }
gd=$(git rev-parse --path-format=absolute --git-dir) && cd_=$(git rev-parse --path-format=absolute --git-common-dir) || { echo "record: UNKNOWN checkout kind -- do not commit"; exit 1; }
[ "$gd" = "$cd_" ] || { echo "REFUSE -- $top is a linked worktree, not a primary checkout (Safety Rule 1)"; exit 1; }
repo=$(basename "$top")
case " $(tr '\n' ' ' < "$R/other-products") " in *" $repo "*) echo "REFUSE -- $repo is another product's repo (Safety Rule 4)"; exit 1 ;; esac
grep -qxF -- "$repo" "$R/classified" || { echo "REFUSE -- $repo was not classified (a repo that appeared after Step 1): run Step 4 first"; exit 1; }
staged=$(mktemp) && git diff --cached --name-only -z > "$staged" || { echo "record: UNKNOWN staged set -- do not commit"; exit 1; }
while IFS= read -r -d '' p; do
  grep -qxF -- "$repo"$'\t'"$p" "$R/not-this-runs" "$R/carved" 2>/dev/null &&
    { echo "REFUSE -- staged path is NOT THIS RUN'S or carved out: $p (unstage it; Safety Rule 3)"; exit 1; }
done < "$staged"
cut -d' ' -f2- "$R/changed-repos" | grep -qxF -- "$top" || printf '%s %s\n' "$head" "$top" >> "$R/changed-repos"   # <HEAD before this run's first commit> <repo path>
git commit -m "<type>: <summary>" || { echo "commit failed (fix the hook rejection; never --no-verify): nothing recorded"; exit 1; }
new=$(git rev-parse HEAD) && [ "$(git rev-parse "$new^" 2>/dev/null)" = "$head" ] && printf '%s\n' "$new" >> "$R/own-commits" ||
  { printf 'UNKNOWN %s\n' "$top" >> "$R/own-commits"; echo "UNKNOWN -- HEAD moved around this commit in $top: this repo will not ship"; }
```

---

## Target Repositories

Every PRIMARY git checkout directly under your workspace root, meaning the
directory you start `/improve-all` from (Step 1 records it as `BASE`). Safety
Rule 1 excludes linked worktrees. Discover the repos; never work from a
remembered list. Identify each one's type from its own manifest
(`pyproject.toml` → Python, `package.json` → TypeScript/JavaScript,
`Cargo.toml` → Rust, a Docusaurus config → docs site; a repo may be several). A
primary checkout that belongs to a different product beside yours is discovered
too. Step 1 names it in `other-products` (Safety Rule 4), and from then on it is
never touched.

```bash
for d in "$(pwd -P)"/*/; do [ -d "$d/.git" ] && basename "$d"; done   # primary checkouts only: a linked worktree's .git is a file
```

**Two kinds of repo are special, if your workspace has them:** a
*configuration repository* (slash commands, agent settings and markdown — for
example the repo your workspace's `.claude` directory comes from) and a
*dev-notes repository* (plans and working notes kept out of the project repos).
Only commit and push changes in these at the very end of the workflow. Do NOT
run analysis or improvements on them. Step 1 records their directory names in
`special-repos`, and every fence reads them from there.

---

## Workflow Steps

### Step 1: Check What Was Done Before

If this is a continuation session, review the previous output provided in your prompt. Identify:
- Which repos were already processed
- What work was completed
- What work remains

A continuation session keeps the run directory. The previous output names it on
its `RUN DIR:` line. Use that path as `R` in every fence and continue where the
previous session stopped.

If starting fresh, `cd` to the workspace root and initialise a NEW run
directory. Its name is a per-run nonce, so neither a concurrent run nor a
sibling subagent sharing this session id can read or clobber it. Init removes
nothing. Fill in the four values
at the top (each may be empty). The fence refuses to start while any of them
still holds a `<placeholder>`. It then classifies every primary checkout as
this product or another product, printing the reason for each (Safety Rule 4):

```bash
CONFIG_REPO="<config-repo-dir>"          # the configuration repository's directory under the workspace root; empty if none
NOTES_REPO="<dev-notes-repo-dir>"        # the dev-notes repository's directory; empty if none
OTHER_EXPLICIT="<other-product-dirs>"    # always another product, whatever its origin
SAME_EXPLICIT="<same-product-dirs>"      # this product even when its origin owner differs
case "$CONFIG_REPO$NOTES_REPO$OTHER_EXPLICIT$SAME_EXPLICIT" in *'<'*|*'>'*) echo "UNKNOWN -- a Step 1 value is still a <placeholder>: fill it in (empty is allowed)"; exit 1 ;; esac
BASE=$(pwd -P)   # the workspace root: the directory that holds your checkouts
[ -e "$BASE/.git" ] && { echo "UNKNOWN -- $BASE is itself a checkout, not the workspace root that holds them: cd there and re-run"; exit 1; }
# GitHub owner of "$top"'s origin, from the configured URL. 1 (reason on stdout) when there is none.
gh_owner() {
  local u
  u=$(git -C "$top" config --get remote.origin.url) || { echo "no origin"; return 1; }
  case "$u" in
    git@github.com:*) u=${u#git@github.com:} ;;
    https://github.com/*|ssh://git@github.com/*) u=${u#*github.com/} ;;
    *) echo "origin is not on GitHub: $u"; return 1 ;;
  esac
  case "$u" in */*) u=${u%%/*} ;; *) echo "unparseable origin"; return 1 ;; esac
  [ -n "$u" ] || { echo "unparseable origin"; return 1; }
  printf '%s\n' "$u" | tr '[:upper:]' '[:lower:]'
}
# The reference owner: the configuration repo's, else the repo the workspace .claude resolves into.
if [ -n "$CONFIG_REPO" ]; then top="$BASE/$CONFIG_REPO"
else top=$(git -C "$(readlink -f "$BASE/.claude")" rev-parse --show-toplevel 2>/dev/null); fi
[ -n "$top" ] && [ -d "$top" ] && REF=$(gh_owner) || { echo "UNKNOWN -- no reference owner (${REF:-no configuration repo}): name CONFIG_REPO"; exit 1; }
# A fresh directory per run (a nonce), never keyed by session: sibling subagents share one
# session id. Init creates it and never removes any other directory.
mkdir -p "$HOME/.qontinui/improve-all-run" && R=$(mktemp -d "$HOME/.qontinui/improve-all-run/XXXXXXXX") || { echo "UNKNOWN -- cannot create a run directory"; exit 1; }
printf '%s\n' "$BASE" > "$R/base"
printf '%s\n' "$CONFIG_REPO $NOTES_REPO" > "$R/special-repos"
printf '%s\n' "$REF" > "$R/ref-owner"; printf '%s\n' "$OTHER_EXPLICIT" > "$R/other-explicit"; printf '%s\n' "$SAME_EXPLICIT" > "$R/same-explicit"
: > "$R/classified"
: > "$R/other-products"; : > "$R/changed-repos"; : > "$R/not-this-runs"; : > "$R/carved"; : > "$R/own-commits"
echo "reference owner: $REF"
for d in "$BASE"/*/; do
  [ -d "$d/.git" ] || continue                     # primary checkouts only (Safety Rule 1)
  top=${d%/}; repo=$(basename "$top"); printf '%s\n' "$repo" >> "$R/classified"
  case " $OTHER_EXPLICIT " in *" $repo "*) echo "$repo: OTHER PRODUCT (listed in other-products)"; printf ' %s' "$repo" >> "$R/other-products"; continue ;; esac
  case " $SAME_EXPLICIT " in *" $repo "*) echo "$repo: THIS PRODUCT (listed in same-product)"; continue ;; esac
  if ! owner=$(gh_owner); then echo "$repo: OTHER PRODUCT ($owner; fail closed)"; printf ' %s' "$repo" >> "$R/other-products"
  elif [ "$owner" != "$REF" ]; then echo "$repo: OTHER PRODUCT (origin owner $owner != $REF)"; printf ' %s' "$repo" >> "$R/other-products"
  else echo "$repo: THIS PRODUCT (origin owner $owner)"; fi
done
echo "RUN DIR: $R"   # every later fence starts with R="<this path>"
```

### Step 2: Record Work That Is Not This Run's

**CRITICAL: Never stash, commit, move or delete uncommitted work you find at the
start. It is a peer's (Safety Rule 3). Stashing has caused lost work.**

This run has changed nothing yet, so every path that is already dirty belongs to
someone else. Record each one as NOT THIS RUN'S. From then on no step commits
it, and every clean check ignores it. `-z` keeps paths with spaces or quotes
intact:

```bash
R="<the RUN DIR printed at init>"   # this run's own directory (Step 1)
{ [ -s "$R/base" ] && BASE=$(cat "$R/base") && [ -d "$BASE" ] && [ ! -e "$BASE/.git" ]; } || { echo "UNKNOWN -- run directory $R is not initialised (Step 1): act on nothing"; exit 1; }
st=$(mktemp)
for d in "$BASE"/*/; do
  [ -d "$d/.git" ] || continue                     # primary checkouts only (Safety Rule 1)
  repo=$(basename "$d")
  git -C "$d" status --porcelain -z --untracked-files=all > "$st" || { echo "$repo: UNKNOWN -- git status failed: never touched this run"; printf ' %s' "$repo" >> "$R/other-products"; continue; }
  while IFS= read -r -d '' e; do
    xy=${e:0:2}; p=${e:3}
    case "$xy" in R*|C*) IFS= read -r -d '' _src ;; esac   # a rename/copy entry is followed by its source path
    printf '%s\t%s\n' "$repo" "$p" >> "$R/not-this-runs"
    echo "$repo: NOT THIS RUN'S -- $p"
  done < "$st"
done
rm -f "$st"
```

List these in the report. Build artifacts that show up here stay as they are:
editing another session's `.gitignore` is changing its work too.

**Dev notes this run creates** (`PLAN*.md`, `TODO*.md`, `NOTES*.md`, temp
scripts) go to the dev-notes repository, not the project repo. If there is no
such repo, carve them out instead: append each one as `<repo-dir><TAB><path>` to
`$R/carved`. It is then never committed, and Step 4's clean check ignores it.
List them in the report.

### Step 3: Pull All Repositories

**Sync default branches in bulk. Leave PR branches and feature branches alone — only report their drift.**

The principle: improve-all does its work on the default branch (`main` or `master` depending on the repo). Pulling a feature branch you're not actively driving creates conflicts you have to re-derive context for; pulling a branch with an open PR rewrites someone's review snapshot. Neither belongs in an autonomous workflow.

For each repo, do all of the following:

1. Detect the default branch (handles `main` vs `master` per repo).
2. Fetch from origin.
3. If the **default branch** is checked out and behind, fast-forward it. No rebase needed: a `--ff-only` pull either fast-forwards or refuses, and it refuses rather than overwrite uncommitted files.
4. If a **non-default branch** is checked out, do NOT pull it. Instead:
   - Check `gh pr list --head $branch --state open` — if a PR exists, mark this branch as "PR-protected; do not touch."
   - Either way, just report whether the branch has drifted vs `origin/$branch` and whether `origin/main`/`master` has new commits the branch could rebase onto when next picked up.
5. **Do NOT push anything.** Pulling readies us for a clean Step 16 push; pushing now could overwrite remote work from other agents.

```bash
R="<the RUN DIR printed at init>"   # this run's own directory (Step 1)
{ [ -s "$R/base" ] && BASE=$(cat "$R/base") && [ -d "$BASE" ] && [ ! -e "$BASE/.git" ]; } || { echo "UNKNOWN -- run directory $R is not initialised (Step 1): act on nothing"; exit 1; }
OTHER_PRODUCTS=$(tr '\n' ' ' < "$R/other-products")
declare -a DRIFTED_FEATURE_BRANCHES=()
declare -a PR_PROTECTED_BRANCHES=()

# Every PRIMARY checkout under the workspace root, special repos included (they are pulled too).
# -d, never -e: a linked worktree's .git is a file, and it is another session's (Safety Rule 1).
for d in "$BASE"/*/; do
  repo=$(basename "$d")
  if [ ! -d "$BASE/$repo/.git" ]; then continue; fi
  case " $OTHER_PRODUCTS " in *" $repo "*) echo "$repo: another product -- untouched (Safety Rule 4)"; continue ;; esac
  cd "$BASE/$repo" || { echo "$repo: UNKNOWN -- cannot enter the checkout"; continue; }
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

# Persist the list in the run directory: later fences read it back from there.
printf '%s\n' "${PR_PROTECTED_BRANCHES[@]}" | sed '/^$/d' > "$R/pr-protected"
```

**Pre-merge sanity for PR branches.** For each entry in `PR_PROTECTED_BRANCHES`, run a quick rename/delete scan against the new `origin/$default` so the user knows whether their open PR is at risk of conflict at merge time:

```bash
R="<the RUN DIR printed at init>"   # this run's own directory (Step 1)
{ [ -s "$R/base" ] && BASE=$(cat "$R/base") && [ -d "$BASE" ] && [ ! -e "$BASE/.git" ]; } || { echo "UNKNOWN -- run directory $R is not initialised (Step 1): act on nothing"; exit 1; }
# Define PR_PROTECTED_BRANCHES here, from the file the Step 3 loop wrote.
[ -f "$R/pr-protected" ] || { echo "UNKNOWN -- $R/pr-protected missing: run the Step 3 loop first"; exit 1; }
mapfile -t PR_PROTECTED_BRANCHES < "$R/pr-protected"
for entry in "${PR_PROTECTED_BRANCHES[@]}"; do
  repo="${entry%%:*}"; rest="${entry#*:}"; branch="${rest%% *}"
  cd "$BASE/$repo" || { echo "$repo: UNKNOWN -- cannot enter the checkout"; continue; }
  default=$(git symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's|^refs/remotes/origin/||')
  default=${default:-main}
  echo "--- $repo:$branch vs origin/$default (renames/deletes only) ---"
  git diff "origin/$default...HEAD" --name-status --diff-filter=D | head -20
done
```

This is informational — improve-all does NOT auto-rebase PR branches. It just surfaces which ones might bite you at merge time.

**If a fast-forward fails** (the default branch has diverged from origin, or the pull would overwrite uncommitted files), report it and skip rebasing. Switching to interactive rebase mid-autonomous-workflow is a bad pattern; better to surface and let the user resolve.

Report what was synced, what was skipped, and any PR-protected branches with rename/delete drift.

### Step 4: Select the Work List

**Commit your own edits before running this step.** Any path still uncommitted
when it runs is recorded as NOT THIS RUN'S, because a peer's work can appear
after Step 2 (Safety Rule 3). Paths this run carved out are the one exception.

Steps 5-14 work only on the **work list**, which this step writes to
`$R/work-list`. A repo is WORK only when it has NO NOT THIS RUN'S path: no
lint, fix or refactor step may touch a peer's work. On that condition, the work
list holds this run's own ledger repos (on a continuation), plus every primary
checkout of this product that:
- is not special;
- is not another product;
- is not PR-protected;
- has its default branch checked out;
- holds no unpushed commits.

A repo holding a peer's uncommitted work, or unpushed commits this run did not
make, is reported `EXCLUDED` or `NOT THIS RUN'S` and left out. The step also
prints how many ledger repos it examined.

```bash
R="<the RUN DIR printed at init>"   # this run's own directory (Step 1)
{ [ -s "$R/base" ] && BASE=$(cat "$R/base") && [ -d "$BASE" ] && [ ! -e "$BASE/.git" ]; } || { echo "UNKNOWN -- run directory $R is not initialised (Step 1): act on nothing"; exit 1; }
SPECIAL_REPOS=$(tr '\n' ' ' < "$R/special-repos"); OTHER_PRODUCTS=$(tr '\n' ' ' < "$R/other-products")
L="$R/changed-repos"
for f in "$L" "$R/not-this-runs" "$R/carved" "$R/pr-protected" "$R/classified" "$R/ref-owner"; do
  [ -f "$f" ] || { echo "UNKNOWN -- $f missing: report nothing clean, and do not output [TASK_COMPLETE]"; exit 1; }
done
in_ledger() { cut -d' ' -f2- "$L" | grep -qxF -- "$top"; }
# GitHub owner of "$top"'s origin, from the configured URL. 1 (reason on stdout) when there is none.
gh_owner() {
  local u
  u=$(git -C "$top" config --get remote.origin.url) || { echo "no origin"; return 1; }
  case "$u" in
    git@github.com:*) u=${u#git@github.com:} ;;
    https://github.com/*|ssh://git@github.com/*) u=${u#*github.com/} ;;
    *) echo "origin is not on GitHub: $u"; return 1 ;;
  esac
  case "$u" in */*) u=${u%%/*} ;; *) echo "unparseable origin"; return 1 ;; esac
  [ -n "$u" ] || { echo "unparseable origin"; return 1; }
  printf '%s\n' "$u" | tr '[:upper:]' '[:lower:]'
}
# A repo that appeared after Step 1 (a mid-run clone) gets the same classification, never a default.
classify_new() {
  local owner
  grep -qxF -- "$repo" "$R/classified" && return 0
  printf '%s\n' "$repo" >> "$R/classified"
  case " $(cat "$R/other-explicit") " in *" $repo "*) echo "$repo: NEW -- OTHER PRODUCT (listed in other-products)"; printf ' %s' "$repo" >> "$R/other-products"; return 0 ;; esac
  case " $(cat "$R/same-explicit") " in *" $repo "*) echo "$repo: NEW -- THIS PRODUCT (listed in same-product)"; return 0 ;; esac
  if ! owner=$(gh_owner); then echo "$repo: NEW -- OTHER PRODUCT ($owner; fail closed)"; printf ' %s' "$repo" >> "$R/other-products"
  elif [ "$owner" != "$(cat "$R/ref-owner")" ]; then echo "$repo: NEW -- OTHER PRODUCT (origin owner $owner != $(cat "$R/ref-owner"))"; printf ' %s' "$repo" >> "$R/other-products"
  else echo "$repo: NEW -- THIS PRODUCT (origin owner $owner)"; fi
}
# Record every uncommitted path in "$top" that is not carved out as NOT THIS RUN'S, and print
# how many there are. -z keeps paths with spaces, quotes or backslashes intact. 1 = git status failed.
peer_paths() {
  local st e xy p n=0
  st=$(mktemp) || return 1
  git -C "$top" status --porcelain -z --untracked-files=all > "$st" || { rm -f "$st"; return 1; }
  while IFS= read -r -d '' e; do
    xy=${e:0:2}; p=${e:3}
    case "$xy" in R*|C*) IFS= read -r -d '' _src ;; esac
    grep -qxF -- "$repo"$'\t'"$p" "$R/carved" && continue
    grep -qxF -- "$repo"$'\t'"$p" "$R/not-this-runs" || printf '%s\t%s\n' "$repo" "$p" >> "$R/not-this-runs"
    n=$((n + 1))
  done < "$st"
  rm -f "$st"; echo "$n"
}
unknown=0; examined=0; : > "$R/work-list"
# 1. This run's ledger repos: on the work list unless a peer's work has appeared there.
while read -r start top; do
  repo=$(basename "$top")
  case " $SPECIAL_REPOS " in *" $repo "*) continue ;; esac   # special repos are handled at the end
  examined=$((examined + 1))
  if ! n=$(peer_paths); then echo "$repo: UNKNOWN -- git status failed; not on the work list"; unknown=1; continue; fi
  if [ "$n" -gt 0 ]; then echo "$repo: EXCLUDED -- $n NOT THIS RUN'S path(s) now in the tree; no lint/fix step runs here"; continue; fi
  printf '%s\n' "$top" >> "$R/work-list"
  if ! unpushed=$(git -C "$top" log --oneline @{u}.. 2>/dev/null); then echo "$repo: UNKNOWN -- no upstream (or git log failed)"; unknown=1
  elif [ -n "$unpushed" ]; then echo "$repo: WORK -- has unpushed commits (this run's)"
  else echo "$repo: WORK -- clean"; fi
done < "$L"
echo "examined $examined ledger repo(s) (special repos excluded)"
# 2. Other primary checkouts of this product.
for d in "$BASE"/*/; do
  [ -d "$d/.git" ] || continue                             # primary checkouts only (Safety Rule 1)
  top=${d%/}; repo=$(basename "$top")
  in_ledger && continue
  case " $SPECIAL_REPOS " in *" $repo "*) continue ;; esac
  classify_new; OTHER_PRODUCTS=$(tr '\n' ' ' < "$R/other-products")
  case " $OTHER_PRODUCTS " in *" $repo "*) echo "$repo: another product -- untouched"; continue ;; esac
  grep -q "^$repo:" "$R/pr-protected" && { echo "$repo: PR-protected -- untouched"; continue; }
  if ! n=$(peer_paths); then echo "$repo: UNKNOWN -- git status failed; untouched"; unknown=1; continue; fi
  [ "$n" -eq 0 ] || { echo "$repo: EXCLUDED -- $n NOT THIS RUN'S path(s); a fixer would rewrite a peer's work"; continue; }
  default=$(git -C "$top" symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's|^refs/remotes/origin/||'); default=${default:-main}
  [ "$(git -C "$top" branch --show-current)" = "$default" ] || { echo "$repo: not on $default -- untouched"; continue; }
  if ! ahead=$(git -C "$top" rev-list "origin/$default..HEAD" 2>/dev/null); then echo "$repo: UNKNOWN -- cannot compare with origin/$default; untouched"; unknown=1
  elif [ -n "$ahead" ]; then echo "$repo: NOT THIS RUN'S -- unpushed commits this run did not make; untouched"
  else echo "$repo: WORK"; printf '%s\n' "$top" >> "$R/work-list"; fi
done
[ "$unknown" = 0 ] || echo "UNKNOWN present: resolve it before reporting; do not output [TASK_COMPLETE]"
```

If the work list is empty and no line reads UNKNOWN, report that there is
nothing to improve and output `[TASK_COMPLETE]`. An `UNKNOWN` line is not clean:
set the repo's upstream (or check it by hand) and re-run this step first.

### Step 5: Linting and Commit

Run linting fixes (NOT formatting — formatting is not a concern), then commit. Apply this step, and Steps 6-14, to the repos in `$R/work-list` only (Step 4):
- **Pre-fix guard.** Run this before every automated fixer (`ruff --fix`, codemods, refactor tools), in this step and in Steps 6-14. A fixer rewrites whatever it finds, so it runs only in a work-list repo whose tree is clean. Run the fixer in the SAME Bash call, after the guard: the guard's `exit 1` protects only its own call:
  ```bash
  R="<the RUN DIR printed at init>"   # this run's own directory (Step 1)
  top="<a path from $R/work-list>"
  grep -qxF -- "$top" "$R/work-list" || { echo "REFUSE -- $top is not on the work list (Step 4)"; exit 1; }
  st=$(git -C "$top" status --porcelain --untracked-files=all) || { echo "UNKNOWN -- git status failed in $top: run no fixer"; exit 1; }
  [ -z "$st" ] || { echo "EXCLUDED -- $top has uncommitted work (a peer's, or yours: commit yours first); run no fixer, report it"; exit 1; }
  cd "$top" && ruff check . --fix   # your fixer, in the same call: it runs only if the guard passed
  ```
- Fix linting issues: `ruff check . --fix` (removes unused imports, fixes code quality issues)
- Organize notes this run created (move them to the dev-notes repository, if your workspace has one; otherwise carve them out as in Step 2). Pre-existing notes are NOT THIS RUN'S: leave them where they are
- Review and commit changes. Stage only the paths this run changed, never a NOT THIS RUN'S or carved-out path (Safety Rule 3). Then run the record snippet, which makes the commit
- **Do NOT push yet** — push happens at the end after all improvements

**Do NOT run black, isort, prettier, or any code formatters** — formatting pre-commit hooks are disabled and code style is not a concern in this project.

### Step 6: Full Audit

Run comprehensive analysis on the work-list repos (`$R/work-list`, Step 4):

```bash
R="<the RUN DIR printed at init>"   # this run's own directory (Step 1)
{ [ -s "$R/base" ] && BASE=$(cat "$R/base") && [ -d "$BASE" ] && [ ! -e "$BASE/.git" ]; } || { echo "UNKNOWN -- run directory $R is not initialised (Step 1): act on nothing"; exit 1; }
cat "$R/work-list"                  # the /path/to/project values below
# Your project's devtools/analysis repo, where it has one. Without one, skip the analysis
# commands below and gather the same facts with the linters and type checkers you already run.
DEVTOOLS="$BASE/<devtools-repo-dir>"
[ -d "$DEVTOOLS" ] || { echo "no devtools/analysis repo at $DEVTOOLS: skip the analysis commands in this fence"; exit 0; }
cd "$DEVTOOLS" || exit 1

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

**Commit**: `fix: address security vulnerabilities` — stage only this run's paths, then run the record snippet, which makes the commit (Safety Rules 2-3)

### Step 8: Architecture Improvements

For each class flagged (high LCOM, many methods):

1. Analyze if refactoring is beneficial
2. Skip classes that are fine (domain models, parsers)
3. Refactor classes that genuinely have too many responsibilities
4. Fix circular dependencies

**Commit**: `refactor: improve code architecture and modularity` — stage only this run's paths, then run the record snippet, which makes the commit (Safety Rules 2-3)

### Step 9: Code Quality

1. Remove dead code (confidence > 0.90)
2. Remove dead imports: `ruff check . --fix`
3. Fix all linting issues

**Commit**: `refactor: remove dead code and fix linting issues` — stage only this run's paths, then run the record snippet, which makes the commit (Safety Rules 2-3)

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

**Commit**: `fix: address React health issues from react-doctor` — stage only this run's paths, then run the record snippet, which makes the commit (Safety Rules 2-3)

### Step 11: Fix Type Errors

**MANDATORY: Fix ALL type errors. Volume is not an excuse to skip.**

1. Run mypy and capture all errors
2. Categorize by type (arg-type, attr-defined, return-value, etc.)
3. Fix each error - trace through code to understand intent
4. Use parallel Task agents for efficiency
5. Verify all errors are fixed

**Commit**: `fix: resolve type errors and improve type coverage` — stage only this run's paths, then run the record snippet, which makes the commit (Safety Rules 2-3)

### Step 12: Implement TODO Items

1. Categorize each TODO (implement, needs user input, stale)
2. Implement clear TODOs
3. Remove stale TODOs
4. Document items needing user input

**Commit**: `feat: implement TODO items` — stage only this run's paths, then run the record snippet, which makes the commit (Safety Rules 2-3)

### Step 13: Incomplete Feature Detection

Find UI elements and API parameters that exist but don't do anything:
- UI controls that set state but never use it
- API parameters that are accepted but ignored
- Feature flags without implementation

Either implement them or remove the dead code.

**Commit**: `fix: implement incomplete features` or `refactor: remove dead feature code` — stage only this run's paths, then run the record snippet, which makes the commit (Safety Rules 2-3)

### Step 14: Dependency Updates

1. Check for outdated dependencies: `poetry show --outdated`
2. Update dependencies (careful with major versions)
3. Run tests after updates

**Commit**: `chore: update dependencies` — stage only this run's paths, then run the record snippet, which makes the commit (Safety Rules 2-3)

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

**Which repos ship.** Only this run's ledger repos ship (Safety Rule 2), and
only when each is a PRIMARY checkout directly under `BASE` (Safety Rule 1). Also
skip any ledger repo whose recorded pre-run HEAD was already ahead of
`origin/<default>`. Those earlier commits are not this run's, and branching from
`HEAD` would carry them into your PR, so report the repo instead. Resolve the set
in-fence. Each `SHIP` line prints the repo's ledger path, and items 2 and 3 act on
exactly that path:

```bash
R="<the RUN DIR printed at init>"   # this run's own directory (Step 1)
{ [ -s "$R/base" ] && BASE=$(cat "$R/base") && [ -d "$BASE" ] && [ ! -e "$BASE/.git" ]; } || { echo "UNKNOWN -- run directory $R is not initialised (Step 1): act on nothing"; exit 1; }
SPECIAL_REPOS=$(tr '\n' ' ' < "$R/special-repos")
L="$R/changed-repos"
[ -f "$L" ] || { echo "UNKNOWN -- ledger $L missing: ship nothing"; exit 1; }
# Define PR_PROTECTED_BRANCHES here, from the file the Step 3 loop wrote.
[ -f "$R/pr-protected" ] || { echo "UNKNOWN -- $R/pr-protected missing: run the Step 3 loop first; ship nothing"; exit 1; }
mapfile -t PR_PROTECTED_BRANCHES < "$R/pr-protected"
# Commits in "$start..HEAD" of "$top" that are not this run's. Ownership is $R/own-commits
# ALONE (a Session-Id trailer is shared by sibling subagents). An "UNKNOWN <path>" line
# there makes the whole repo unshippable. 1 = cannot read.
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
# The checkout must still be on the default branch, with this run's start in its history.
checkout_in_place() {
  [ "$(git -C "$top" symbolic-ref --short HEAD 2>/dev/null)" = "$default" ] &&
    git -C "$top" merge-base --is-ancestor "$start" HEAD
}
echo "ledger: $(wc -l < "$L") repo(s)"
while read -r start top; do                     # ledger line: <pre-run HEAD> <repo path>
  repo=$(basename "$top")
  if [ ! -d "$top/.git" ] || [ "$(dirname "$top")" != "$BASE" ]; then
    echo "$top: SKIP -- not a primary checkout directly under $BASE (Safety Rule 1)"; continue
  fi
  if printf '%s\n' "${PR_PROTECTED_BRANCHES[@]}" | grep -q "^$repo:"; then
    echo "$top: SKIP -- current branch has an open PR (item 1)"; continue
  fi
  default=$(git -C "$top" symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's|^refs/remotes/origin/||'); default=${default:-main}
  if ! checkout_in_place; then
    echo "$top: SKIP -- checkout moved since run start (not on $default, or $start is not in HEAD's history); touching nothing"
  elif ! prior="$(git -C "$top" rev-list "origin/$default..$start" 2>/dev/null)"; then
    echo "$top: SKIP -- UNKNOWN whether it held unpushed commits before this run (rev-list failed); not shipping"
  elif [ -n "$prior" ]; then
    echo "$top: SKIP -- held unpushed commits before this run; not shipping them"
  elif ! foreign=$(foreign_since_start); then
    echo "$top: SKIP -- UNKNOWN whether a foreign commit landed since run start; not shipping"
  elif [ -n "$foreign" ]; then
    echo "$top: SKIP -- foreign commit since run start ($(printf '%s' "$foreign" | head -c 12)…); touching nothing"
  elif case " $SPECIAL_REPOS " in *" $repo "*) true ;; *) false ;; esac; then
    echo "$top: SHIP (special repo: direct push of default, item 3)"
  else
    echo "$top: SHIP (branch + PR, item 2)"
  fi
done < "$L"
```

1. **Skip any repo whose current branch has an open PR.** Pushing into an open PR branch as part of an autonomous improve-all run rewrites the snapshot reviewers are looking at. Use the `PR_PROTECTED_BRANCHES` list captured in Step 3. The resolve fence above reads it from `$R/pr-protected` and prints `SKIP` for those repos.
2. For each non-special repo listed `SHIP` above that has unpushed commits on its default branch (`<default>`):
   - Capture the commits made this run: `git rev-list "origin/<default>..HEAD"`.
   - Create a session branch holding exactly those commits and reset the default branch back to origin so nothing is left staged for a direct default-branch push:
     ```bash
     R="<the RUN DIR printed at init>"   # this run's own directory (Step 1)
     top="<the path the resolve fence printed SHIP>"   # the ledger path itself, never a re-derived one
     [ -d "$top/.git" ] || { echo "UNKNOWN -- $top is not a primary checkout: ship nothing"; exit 1; }
     start=""; while read -r s t; do [ "$t" = "$top" ] && start=$s; done < "$R/changed-repos"
     [ -n "$start" ] || { echo "UNKNOWN -- $top is not in this run's ledger: ship nothing"; exit 1; }
     default=$(git -C "$top" symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's|^refs/remotes/origin/||'); default=${default:-main}
     # Commits in "$start..HEAD" of "$top" that are not this run's. Ownership is $R/own-commits
     # ALONE (a Session-Id trailer is shared by sibling subagents). An "UNKNOWN <path>" line
     # there makes the whole repo unshippable. 1 = cannot read.
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
     # The checkout must still be on the default branch, with this run's start in its history.
     checkout_in_place() {
       [ "$(git -C "$top" symbolic-ref --short HEAD 2>/dev/null)" = "$default" ] &&
         git -C "$top" merge-base --is-ancestor "$start" HEAD
     }
     # Re-check right before branching: a peer may have committed or switched branch since the resolve fence ran.
     checkout_in_place || { echo "SKIP -- checkout moved since run start; touching nothing"; exit 1; }
     foreign=$(foreign_since_start) || { echo "UNKNOWN -- cannot read $start..HEAD: ship nothing"; exit 1; }
     [ -z "$foreign" ] || { echo "SKIP -- foreign commit since run start; touching nothing"; exit 1; }
     branch="loop/improve-all-$(date +%Y%m%d-%H%M%S)-${SESSION_SHORT:-$RANDOM}"
     git -C "$top" branch "$branch"          # branch points at current HEAD (the new commits)
     git -C "$top" checkout "$branch"
     git -C "$top" branch -f "$default" "origin/$default"   # default ref returns to origin; commits live only on the branch
     ```
   - Push the BRANCH (never the default branch), then open a PR naming the loop from inside that checkout — CHAINED, so a refused push opens nothing and the repo is skipped:
     ```bash
     # $top, $branch and $default come from the fence above. In a fresh shell, rebind them first:
     # top = the path the resolve fence printed SHIP, branch=$(git -C "$top" symbolic-ref --short HEAD), default = that repo's default branch.
     : "${top:?unbound: rebind from the branching fence}" "${branch:?unbound: rebind from the branching fence}" "${default:?unbound: rebind from the branching fence}"
     if git -C "$top" push -u origin "$branch"; then
       (cd "$top" && gh pr create --head "$branch" --title "improve-all: $(basename "$top") autonomous improvements" --body "Autonomous /improve-all run. Commits: $(git -C "$top" log --format=%s "origin/$default..HEAD" | paste -sd ';' -)")
     else
       echo "refusing: push of $branch failed; no PR opened" >&2   # report $(basename "$top") as Push failed
     fi
     ```
     A refused push (auth, network, a hook) must not go on to a `gh pr create` whose failure would then be misreported as a PR problem. On that arm skip the read-back below, and report the repo as `Push failed`, naming `$branch`: the default ref was already reset to origin, so this run's commits live ONLY on that local branch.
   - Session-Id + Session-Name trailers come from each repo's PER-CLONE `prepare-commit-msg` hook, not from the commit command — a clone the installer never ran against emits neither, so an untrailered commit is a missing hook, not a missing name. Install: `qontinui-claude-config/scripts/install-guard-hooks.sh` — add `--git-repo "$(git rev-parse --show-toplevel)"` to repair just the clone you are in.
   - **Read the PR back before reporting it** — in `/implement-plan` Step 4.5b's served door order the create may run through any door, and none of their exit statuses or outputs is evidence a PR exists: `gh pr list --repo <owner/repo> --head "$branch" --state all --json number,url,state,headRefOid` must return a row whose `headRefOid` is the head you pushed, and the PR URL in the summary report comes from that read.
   - **Do NOT merge.** Coord is the sole merge authority for `qontinui/*` repos; agents never run `gh pr merge` or `--admin` (CLAUDE.md; coord-served policy `git-operations` `merge-authority`). Opening the PR IS shipping — coord's merge train lands it once checks are green. If checks fail, leave the PR open and surface it in the report.
3. **The special repos listed `SHIP (special repo …)` above** (the configuration and dev-notes repositories; config/notes only): these have no CI gate. For them only, commit (staging only this run's paths, with the record snippet) and push the default branch directly from the printed path: `git -C "<the path the resolve fence printed SHIP>" push origin "<default>"`. A special repo this run never committed to is not listed, so it is not pushed. (They are the carve-out — code repos always go through the branch-first PR flow above.)
4. Generate summary report (see format below) — include each repo's branch name, PR URL, and merge status; a repo whose push was refused reads `Push failed` with the local branch that holds its commits.

---

## Summary Report Format

```markdown
tree: root=<checkout-name> head=<sha> dirty=<digest|clean|unknown> dirty_files=<n|UNKNOWN> measured=<ISO-8601-UTC>
# Improve All - Summary Report
Date: {date}

## Repositories Processed

| Repository | Status | Changes |
|------------|--------|---------|
| qontinui | Processed | {description} |
| {repo} | Push failed | commits only on local branch `{branch}` |
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
