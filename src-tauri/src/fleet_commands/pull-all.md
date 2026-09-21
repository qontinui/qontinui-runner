# Pull All Repos and Auto-Resolve Merge Conflicts

Pull the latest changes from all git repositories in the parent directory. Automatically resolve any merge conflicts by analyzing commit history and understanding the evolution of each repository.

## Instructions

**Work completely autonomously. Do NOT ask the user for input on conflict resolution.**

---

### Phase 1: Discover Repositories

Find all git repos in the parent directory:

```bash
BASE="$(pwd)"
for dir in "$BASE"/*/; do
  if [ -d "$dir/.git" ]; then
    echo "$(basename "$dir")"
  fi
done
```

Record the list of all repos found.

---

### Phase 2: Pull Each Repository

**Sync default branches in bulk. Leave PR branches and feature branches alone — pulling them rewrites snapshots reviewers are looking at and creates conflicts you'd rather resolve with full context the next time you sit down to work on them. Report drift; don't act on it.**

For each repository:

1. **Detect the default branch** (handles `main` vs `master` per repo):
   ```bash
   default=$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's|^origin/||')
   if [ -z "$default" ]; then
     # origin/HEAD isn't set on this clone. Sticky-fix it via `git remote
     # set-head origin --auto` (asks the remote what its default is and
     # caches the answer in the local .git/refs/remotes/origin/HEAD), then
     # re-read. This one-shot heals the situation permanently — subsequent
     # /pull-all runs hit the fast path without needing the fallback.
     #
     # DO NOT use `git rev-parse --abbrev-ref origin/HEAD` here: when the
     # symref is missing, that command echoes the literal string
     # "origin/HEAD" to stdout with exit code 128. The sed pipe then strips
     # "origin/" and silently produces default=HEAD, which makes the
     # current-vs-default comparison fail for every repo without an
     # explicit symref and misclassifies them as feature branches.
     git remote set-head origin --auto >/dev/null 2>&1
     default=$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's|^origin/||')
   fi
   if [ -z "$default" ]; then
     # set-head failed (offline, weird remote). Fall back to whichever of
     # main/master actually exists locally.
     if git rev-parse --verify --quiet origin/main >/dev/null; then default=main
     elif git rev-parse --verify --quiet origin/master >/dev/null; then default=master
     else default=main  # last-resort fallback; downstream pull will surface the error
     fi
   fi
   ```

2. **Check current branch**:
   ```bash
   current=$(git branch --show-current)
   ```

3. **Always fetch first** with `--prune` (cheap; no working-tree impact). Pruning is essential — it's what makes a deleted remote branch surface as `[upstream: gone]` in step 4's Case D detection. Without it, an orphan checkout looks identical to a merely-drifted feature branch and Case D never fires:
   ```bash
   git fetch --prune origin
   ```

4. **Branch the workflow on what's checked out:**

   **Case A — current branch IS the default branch.** Safe to sync in place.
   - Check `git status --porcelain --untracked-files=no` for **tracked** modifications. If there are tracked changes, stash them with `git stash push -m "auto-stash before pull-all"` and record the stash so it can be popped. **Do not include untracked files in the check** — `git stash push` without `-u` only stashes tracked changes, so an untracked-only worktree would auto-stash an empty stash and then `stash pop` would surface a false-positive "No stash entries found" / `STASH CONFLICTS` failure. Untracked files (scratch dirs, generated stubs, build artifacts) are intentionally outside version control; the pull will not touch them unless an incoming commit adds a file at the same path (which git handles with its own explicit "untracked working tree files would be overwritten" error — distinct from a merge conflict).
   - Pull with `git pull --ff-only origin "$default"` if possible (the common case: you have no local commits on default, just need to catch up). Fast-forward is reversible and conflict-free.
   - If `--ff-only` fails because you have local commits on default ahead of origin, fall back to `git pull --rebase origin "$default"` and proceed to Phase 3 if conflicts arise. (This is the original behavior; only triggered when the cheaper path can't apply.)
   - Pop the stash. If pop conflicts, resolve via Phase 3. **Capture the pop's exit code explicitly — see "Stash-pop exit-code pitfall" below.**

   **Case D — current branch's upstream is GONE (deleted on remote).** Typically because the branch's PR was merged and the remote branch was auto-deleted (`gh pr merge --delete-branch`). The local checkout is **orphaned**: continuing to sit on it makes Phase 2 effectively a no-op forever, because there's no upstream to pull from. Switch to default before pulling. (Check D *before* B because both have `current != default`, but D is the more specific condition — the upstream IS gone, not just diverged.)

   - **Detect** with:
     ```bash
     track=$(git for-each-ref --format='%(upstream:track,nobracket)' "refs/heads/$current")
     # Literal "gone" is git's standard marker for "had an upstream; now deleted on remote".
     # Empty means the branch never had an upstream (falls through to Case B/C).
     ```
   - If `$track = "gone"`:
     1. Stash any **tracked** modifications with `git stash push -m "auto-stash before orphan-branch switch"` (gate on `git status --porcelain --untracked-files=no` for the same reason as Case A — don't auto-stash an empty stash when only untracked files are present, which would produce a false-positive `STASH CONFLICTS` from the post-pop `git stash pop` call). Record the stash so it can be popped on default afterward. Auto-stash is justified here for the same reason as Case A: the working tree is unrecoverable from upstream (there is no upstream), so swapping branches without stashing would lose data, but the user clearly doesn't want to stay on a dead branch either.
     2. Checkout default and pull (same shape as Case A): `git checkout "$default" && git pull --ff-only origin "$default"`, falling through to `--rebase` + Phase 3 on conflict.
     3. Pop the stash. If pop conflicts on default, resolve via Phase 3. **Capture the pop's exit code explicitly — see "Stash-pop exit-code pitfall" below.**
     4. **Do NOT auto-delete the orphan branch.** Squash-merged PRs (the common case in this repo) produce orphan branches whose commits are *not* SHA-reachable from default, so a naive "fully merged?" check would refuse the delete; and a force-deleted-without-merge branch may carry real WIP. Instead, report it in Phase 4's "Orphan branches (auto-switched off; review for local-branch deletion)" with the exact commands the user can run to verify and delete:
        ```bash
        gh pr list --head <orphan> --state merged   # was a PR merged?
        git log --oneline origin/$default..<orphan> # commits NOT in default (often non-empty even if merged via squash)
        git branch -d <orphan>                      # safe-delete (refuses if commits would be lost)
        git branch -D <orphan>                      # force-delete (use only after verifying PR merged via squash)
        ```

   **Case B — current branch is NOT the default branch (and not Case D).** Do NOT pull this branch.
   - Check `gh pr list --head "$current" --state open --json number -q '.[0].number'`. If a PR exists, mark this entry **PR-protected** — pulling/rebasing would rewrite the snapshot reviewers are looking at.
   - If no PR, mark it **drifted feature branch** — pulling could create conflicts the user will want to resolve with full context the next time they pick up that branch.
   - Either way: skip the pull. Just compute drift counts:
     ```bash
     branch_drift=$(git rev-list HEAD..origin/$current --count 2>/dev/null)
     default_ahead=$(git rev-list HEAD..origin/$default --count 2>/dev/null)
     ```
   - Optionally fast-forward the default branch *ref* without checking it out, so the user's next switch-to-default is already up to date:
     ```bash
     git fetch origin "$default:$default" 2>/dev/null || true
     ```
     (This silently fails if `$default` is the current branch or has uncommitted changes pointing at it — both safe no-ops.)
   - Do NOT auto-stash the user's working tree on a non-default branch. If they have WIP, it's WIP for that branch — pull-all has no business touching it.

   **Case C — repo has no remote tracking branch, or no remote at all.** Skip and note in the report.

   **Detached HEAD** is a distinct skip case (the repo has a remote, but no branch is checked out — e.g. mid-rebase, mid-bisect, or post-`git checkout <sha>`). Use a dedicated `DETACHED_HEAD` bucket and report it in Phase 4 with the SHA and a hint to either `git checkout <branch>` or, if mid-rebase/bisect, finish the in-flight operation. Do NOT lump it into `NO_REMOTE` — the operator response differs.

5. **Stale-orphan scan (after handling the current branch).** Case D fires only when the orphan IS currently checked out. But typical operator flow is *merge PR → branch auto-deletes on remote → operator stays on `main`* — leaving an undetected stale orphan locally. Sweep them up via:

   **Scan TWO classes, because `gone` alone is a blind spot.** `%(upstream:track)` reads **empty**, not `gone`, for a branch that never had tracking configured — no `-u` on the push, or a still-live remote branch the local config never pointed at — so a `gone`-only test never emits those branches at all. Measured 2026-09-16/17: **~19 of the 49** branches removed by hand that session sat in exactly that blind spot, several with a **still-existing** remote counterpart the scan never saw either. The scan below **widens**; it does not swap one blind spot for another. Every `gone` row still comes out, and the never-tracked population comes out beside it as a **separate, labelled class**, so the operator can tell *“upstream deleted”* from *“never tracked”* and pick the right next step for each. The currently-checked-out branch is still excluded (Case D's territory), and so is the repo default — an untracked `main` is not a cleanup candidate.

   ```bash
   # The three field numbers are passed in as awk VARIABLES (`bcol`, `tcol`, `ucol`) and the
   # program references them as `$bcol` / `$tcol` / `$ucol`. Do NOT "simplify" those back to
   # a dollar sign followed by a literal digit: in a slash-command markdown body such a
   # sequence is a HARNESS ARGUMENT PLACEHOLDER, not an awk field reference. Claude Code
   # substitutes the invocation's argument words into this body BEFORE injecting it, indexed
   # from ZERO (the zeroth placeholder is the FIRST word), and the substitution is TEXTUAL —
   # it does not know awk from shell. It would rewrite these field references into bare awk
   # variable names, which awk reads as EMPTY uninitialised variables. Measured both shapes:
   # with two argument words the scan prints one BLANK LINE per branch whose upstream is gone
   # — the current-branch exclusion is defeated at the same time, so that one is emitted too;
   # with three or more argument words it matches nothing and reports ZERO stale orphans.
   # Both exit 0. Neither is an error — it is a silent wrong answer. (This comment spells no
   # such sequence of its own on purpose: it would be substituted too, garbling the warning.)
   #
   # The separator is a TAB (`%09`), not a space: a refname can contain neither a tab nor any
   # other ASCII control character, so the split is unambiguous even when the track field is
   # empty — which is exactly the never-tracked case this scan exists to stop missing.
   # `%(upstream)` is the third field: empty means NO upstream is configured at all, which is
   # a different fact from `gone` (upstream configured, remote ref deleted).
   git for-each-ref refs/heads/ \
       --format='%(refname:short)%09%(upstream:track,nobracket)%09%(upstream)' \
     | awk -F'\t' -v cur="$current" -v def="$default" -v bcol=1 -v tcol=2 -v ucol=3 '
         $bcol == cur                 { next }
         $tcol == "gone"              { print "gone" FS $bcol; next }
         $ucol == "" && $bcol != def  { print "untracked" FS $bcol }'
   ```

   **For each hit, annotate with its merge status before recording it** — the operator needs that to decide between safe-delete (`git branch -d`) and force-delete (`git branch -D`). Without the annotation, the operator has to run `gh pr list --head <name> --state merged` once per stale branch by hand; with it, the report is directly actionable.

   **ONE `gh` call per repo, not one per branch.** The per-branch form was already
   mis-sized before this widening — measured on `qontinui-claude-config` 2026-09-20 the
   scan yields **355** hits (251 `gone` + 104 never-tracked), so the old shape meant 355
   sequential API calls for ONE repo, well into GitHub's secondary rate limit, times ~14
   repos. Build the head→PR map once and look each branch up locally:

   ```bash
   # One call per repo, and THREE failure modes that must not read as "no merged PR":
   #  (a) the query fails with healthy auth — rate limit, 5xx, or a repo whose remote is
   #      not a GitHub host at all (Phase 1 walks every git repo it finds, so this is
   #      routine). `gh auth status` still SUCCEEDS, so step 6's check does not fire and
   #      cannot be relied on here — gate the wording on `prmap_ok` instead.
   #  (b) the map is TRUNCATED. `gh pr list` returns newest-first, so a cap drops the
   #      OLDEST merged PRs — exactly the ones the oldest stale branches point at. This
   #      repo had 989 merged PRs on 2026-09-20, eleven below a 1000 cap. A cap you
   #      cannot detect is the silent-wrong-answer class this file exists to forbid, so
   #      assert it was not hit rather than choosing a number and hoping.
   #  (c) `mktemp` fails. An empty filename makes mawk read STDIN — which here is the
   #      loop's own branch list — so the first iteration would swallow the scan and the
   #      loop would end, exit 0. /dev/null closes that hole, but it does NOT by itself
   #      fail closed: the verdict does. `prmap_ok` is forced to 0 on that path BEFORE
   #      the query can run, which is what makes the annotation honest — see the branch
   #      order below.
   prmap=$(mktemp) || prmap=/dev/null
   # NOTE the guard: `rm -f /dev/null` is a permission error as a normal user and, as
   # root (CI containers routinely are), it UNLINKS THE DEVICE NODE for the life of the
   # container. Guard both removals, not just this one.
   trap '[ "$prmap" = /dev/null ] || rm -f "$prmap"' EXIT   # already trapping EXIT? CHAIN it
   prmap_cap=5000
   if [ "$prmap" = /dev/null ]; then
     # No map file, so merge status cannot be established for ANY branch in this repo.
     # This arm must come FIRST: `gh ... > /dev/null` SUCCEEDS and `wc -l < /dev/null`
     # is 0, so letting the query run into /dev/null would satisfy both the success test
     # and the truncation test and set prmap_ok=1 — "map is known good" — annotating
     # every branch "no merged PR" with full confidence. That is the silent-wrong-answer
     # class this whole block exists to prevent, reached through its own fallback.
     prmap_ok=0
   elif gh pr list --state merged --limit "$prmap_cap" --json number,headRefName \
          -q '.[] | "\(.headRefName)\t\(.number)"' > "$prmap" 2>/dev/null; then
     if [ "$(wc -l < "$prmap")" -ge "$prmap_cap" ]; then prmap_ok=truncated; else prmap_ok=1; fi
   else
     prmap_ok=0
   fi

   while IFS=$'\t' read -r cls br; do
     [ -z "$br" ] && continue
     # Local lookup against the map; no network inside the loop. `</dev/null` so a
     # degenerate $prmap can never consume the loop's stdin (see (c) above).
     pr=$(awk -F'\t' -v b="$br" -v bcol=1 -v ncol=2 '$bcol == b { print $ncol; exit }' "$prmap" </dev/null)
     # An absent $pr means "no merged PR" ONLY when the map is known good. Otherwise the
     # honest annotation is that merge status was not established.
     case "$prmap_ok" in
       1)         unverified="" ;;
       truncated) unverified="merge status unverified — merged-PR map truncated at $prmap_cap; older PRs missing" ;;
       *)         unverified="merge status unverified — gh query failed" ;;
     esac
     case "$cls" in
       gone)
         if [ -n "$pr" ]; then
           STALE_ORPHANS+=("$repo: $br (upstream gone; PR #$pr merged — safe to force-delete)")
         else
           STALE_ORPHANS+=("$repo: $br (upstream gone; ${unverified:-no merged PR — investigate; may hold WIP})")
         fi
         ;;
       untracked)
         # Local-only ref lookup, no network: a never-tracked branch may still have a live
         # remote counterpart nobody ever pointed the local config at.
         # Resolve the remote rather than hardcoding `origin`: this line makes a POSITIVE
         # claim the operator acts on, so in a repo whose remote is named otherwise a
         # hardcoded `origin` would assert "no remote counterpart" about a branch that has
         # one. (Elsewhere in this file `origin` is hardcoded only for FETCHES, which fail
         # visibly; an assertion must not.)
         # `branch.<br>.remote` CAN be set while %(upstream) is empty (remote set, merge
         # unset), so this lookup is not dead code for the untracked class. When it is
         # empty, prefer `origin` — every other remote reference in this file hardcodes it
         # — and fall back to the first remote only if there is no `origin`, SAYING SO,
         # because `git remote | head -1` is alphabetical: in a repo with `aaa-fork` and
         # `origin` it picks `aaa-fork` and would assert "no remote counterpart" about a
         # branch that exists on origin. That is the false positive this block prevents.
         rmt=$(git config --get "branch.$br.remote" 2>/dev/null)
         assumed=""
         if [ -z "$rmt" ]; then
           if git remote | grep -qx origin; then rmt=origin
           else rmt=$(git remote | head -1); assumed=", remote assumed"; fi
         fi
         if [ -z "$rmt" ]; then
           rem="no remote configured in this repo"
         elif git show-ref --verify --quiet "refs/remotes/$rmt/$br"; then
           rem="remote counterpart still exists on '$rmt'$assumed"
         else
           rem="no remote counterpart on '$rmt'$assumed"
         fi
         if [ -n "$pr" ]; then
           UNTRACKED_ORPHANS+=("$repo: $br (never tracked; $rem; PR #$pr merged)")
         else
           UNTRACKED_ORPHANS+=("$repo: $br (never tracked; $rem; ${unverified:-no merged PR — investigate; may hold WIP})")
         fi
         ;;
     esac
   # Same awk-variable field references as the scan above, for the same reason — see the
   # comment there before touching them.
   done < <(git for-each-ref refs/heads/ \
              --format='%(refname:short)%09%(upstream:track,nobracket)%09%(upstream)' \
            | awk -F'\t' -v cur="$current" -v def="$default" -v bcol=1 -v tcol=2 -v ucol=3 '
                $bcol == cur                 { next }
                $tcol == "gone"              { print "gone" FS $bcol; next }
                $ucol == "" && $bcol != def  { print "untracked" FS $bcol }')
   [ "$prmap" = /dev/null ] || rm -f "$prmap"
   ```

   **Three ways merge status can be UNESTABLISHED, and each gets its own wording** — the code above emits these; do not collapse them, and do not annotate any of them "no merged PR":
   - the map query failed (rate limit, 5xx, a non-GitHub remote, or `gh` unavailable — note `gh auth status` succeeding does NOT rule the first three out): `merge status unverified — gh query failed`;
   - the map was truncated at the cap, so the OLDEST merged PRs are missing: `merge status unverified — merged-PR map truncated at <cap>; older PRs missing`;
   - `mktemp` failed, so there is no map at all: same `gh query failed` wording, reached through `prmap_ok=0`.

   In every one of them the operator still needs to see the branch exists, so the hit is reported — with the status unestablished rather than asserted absent.

   **Two properties of this section are DELIBERATE, and a widening must preserve both.**
   (1) The scan **already** annotated every hit with its merge status — now via the repo's merged-PR map (`gh pr list --state merged`, built once above; the per-branch `--head` form it replaced is gone from the executable path) — so what was missing was *detection coverage*, not annotation — the never-tracked class is fed through the same annotation path and no annotation logic changed.
   (2) `pull-all` **does NOT auto-delete**, by the explicit standing decision restated immediately below. The never-tracked class arms nothing: it is surfaced for the operator exactly as the `gone` class is, and adding it must never be read as licence to delete either one.

   **Do NOT auto-delete** — same reasoning as Case D's "Do NOT auto-delete" rule, and it covers **both** classes. Even when a PR is confirmed merged, the operator is the one who decides whether to act on it (a stale orphan might be deliberately kept for archaeology, or might be the source of comments still referenced in the PR). The annotation removes the *verification* step; the delete step is theirs.

   Exclude the currently-checked-out branch from this scan; that's Case D's territory, and the report should not double-count.

   Cost: **one** `gh pr list` per REPO (not per branch — see the map above), plus one local `awk` lookup and, for never-tracked hits, one local `git show-ref` per hit. No network inside the loop. ⚠️ **N is not 0–5 on this fleet and has not been for a long time** — measured on `qontinui-claude-config` alone on 2026-09-20: **355** hits (251 `gone` + 104 never-tracked) out of 387 local branches. The old "typical N is 0–5" was already wrong for the `gone` class before this widening; the never-tracked class simply makes the true size visible, because it was never being counted at all. A four-figure fleet total is the signal to run `/cleanup-steward`, not to run `/pull-all` more often.

6. **If `gh` is unavailable or unauthenticated** (`gh auth status` fails): fall back to treating ALL non-default branches as "drifted feature branch" rather than "PR-protected." Less protective (a real open PR might get downgraded to feature-branch handling) but still correct: the answer is the same — don't pull it.

#### Stash-pop exit-code pitfall

`git stash pop` writes its conflict message to **stderr**, not stdout, and pipes mask the producer's exit code. So this looks right but is **wrong**:

```bash
# WRONG — pipe's exit code is sed's success → conflicted pop is silently reported as OK
if git stash pop 2>&1 | sed 's/^/  /'; then
  STASH_OK+=("$repo")
else
  STASH_CONFLICTS+=("$repo")
fi
```

Use `PIPESTATUS[0]` to capture the pop's actual exit code (or `set -o pipefail` at script top):

```bash
# RIGHT — capture pop's exit code via PIPESTATUS
git stash pop 2>&1 | sed 's/^/  /'
pop_rc=${PIPESTATUS[0]}
if [ "$pop_rc" -eq 0 ]; then
  STASH_OK+=("$repo")
else
  STASH_CONFLICTS+=("$repo")
fi
```

Same hazard applies to any `git <cmd> 2>&1 | sed ...; if ...` pattern in the script — `git pull`, `git checkout`, `git rebase --continue`, etc. Either route through `PIPESTATUS[0]` or run the git command bare and re-format afterward.

#### Phase 2 summary

After processing all repos, print a categorized roll-up so it's visible in autonomous-run logs:

Wrap each section in an `if`/`fi` (not the `[ ] && printf` shorthand) so the script's final exit code is the loop's success/failure, not the empty-array test that happens to come last. The shorthand returns exit 1 when the array is empty, leaving operators staring at a `(Exit code 1)` after a totally clean run.

```bash
echo ""
echo "=== Pull-all summary ==="
if [ ${#PULLED[@]}              -gt 0 ]; then printf "Pulled (default branch): %s\n" "${PULLED[@]}";          fi
if [ ${#ORPHAN_SWITCHED[@]}     -gt 0 ]; then printf "Orphan-switched (Case D, was on a deleted-upstream branch; now on default): %s\n" "${ORPHAN_SWITCHED[@]}"; fi
if [ ${#STALE_ORPHANS[@]}       -gt 0 ]; then printf "Stale orphan (not checked out; upstream gone — cleanup candidate): %s\n" "${STALE_ORPHANS[@]}";       fi
if [ ${#UNTRACKED_ORPHANS[@]}   -gt 0 ]; then printf "Never-tracked local branch (not checked out; no upstream ever configured — cleanup candidate): %s\n" "${UNTRACKED_ORPHANS[@]}"; fi
if [ ${#PR_PROTECTED[@]}        -gt 0 ]; then printf "PR-protected (skipped):  %s\n" "${PR_PROTECTED[@]}";    fi
if [ ${#DRIFTED_FEATURE[@]}     -gt 0 ]; then printf "Feature branch drifted (skipped): %s\n" "${DRIFTED_FEATURE[@]}"; fi
if [ ${#DETACHED_HEAD[@]}       -gt 0 ]; then printf "Detached HEAD (skipped — finish in-flight op or checkout a branch): %s\n" "${DETACHED_HEAD[@]}";  fi
if [ ${#NO_REMOTE[@]}           -gt 0 ]; then printf "No remote (skipped):     %s\n" "${NO_REMOTE[@]}";       fi
```

---

### Phase 3: Auto-Resolve Merge Conflicts

When conflicts arise during pull/rebase, resolve them autonomously using this strategy:

#### Step 1: Understand the Context

For each conflicted file, gather intelligence:

1. **Read the conflict markers** in the file to see both versions

2. **Read recent commit messages** for this file on both sides:
   ```bash
   # Local commits touching this file
   git log --oneline -10 -- <conflicted-file>

   # Incoming commits (during rebase, check ORIG_HEAD)
   git log --oneline -10 ORIG_HEAD..FETCH_HEAD -- <conflicted-file> 2>/dev/null
   ```

3. **Read the full diff** to understand what each side changed:
   ```bash
   git diff -- <conflicted-file>
   ```

4. **Read the file's recent history** to understand its evolution:
   ```bash
   git log --oneline -20 -- <conflicted-file>
   ```

#### Step 2: Decide Which Code to Keep

Apply these resolution rules **in priority order**:

1. **Refactors and rewrites win over patches**: If one side refactored/rewrote a section and the other side made a small fix to the old code, keep the refactored version. The fix was likely addressing something the refactor already handles.

2. **Feature additions are merged**: If both sides added new code (new functions, new imports, new config entries) that doesn't overlap, keep both additions. This is the most common non-conflicting conflict.

3. **Deletions win over modifications to deleted code**: If one side deleted code and the other modified it, the deletion likely reflects an intentional cleanup. Keep the deletion unless commit messages indicate the modification is critical new functionality.

4. **Newer architectural direction wins**: Read the commit messages to identify which side represents the forward direction of the project. Commits with messages like "refactor:", "feat:", or "migrate:" represent intentional evolution. Keep the version aligned with the newer architecture.

5. **Remote (incoming) wins by default**: If the intent is unclear and both changes seem equivalent, prefer the remote/incoming version since it represents the most recently pushed state.

#### Step 3: Apply the Resolution

1. **Read the conflicted file** and understand all conflict blocks (`<<<<<<<`, `=======`, `>>>>>>>`)
2. **Edit the file** to resolve each conflict block based on the decisions above
3. **Ensure the resolved file is syntactically valid**:
   - For Python: check indentation and imports
   - For TypeScript/JavaScript: check brackets, imports, and types
   - For Rust: check braces and use statements
   - For JSON/YAML: validate structure
4. **Stage the resolved file**: `git add <file>`
5. **Continue the rebase**: `git rebase --continue`
   - If more conflicts appear, repeat this phase
   - Use the commit message from the original commit (do not modify rebase commit messages)

---

### Phase 4: Final Report

After processing all repositories, provide a summary:

```
## Pull All Results

### Clean Pulls (default branch fast-forwarded, no conflicts)
- repo-name (default-branch) — X new commits

### Conflicts Resolved (default branch needed --rebase)
- repo-name (default-branch)
  - file.py: kept remote refactor (commit abc1234: "refactor: restructure module")
  - config.ts: merged both additions

### Orphan Branches (auto-switched off; review for local-branch deletion)
- repo-name (was: <orphan-branch> → <default>) — upstream gone; switched + pulled <default> (X new commits); orphan branch retained locally pending review.
  - `gh pr list --head <orphan-branch> --state merged` → was a PR merged? If yes, branch content is on default (likely via squash) and the local branch is safe to delete with `git branch -D <orphan-branch>`.
  - `git log --oneline origin/<default>..<orphan-branch>` → commits NOT directly in default (expected non-empty for squash-merged PRs even though content is merged).

### Stale Orphan Branches (upstream GONE, not checked out — local cleanup candidates)
Local branches whose upstream is gone but that aren't the currently-checked-out branch — typically the residue of "merge PR → branch auto-deletes on remote → operator stays on `main`". Pre-annotated with PR merge status so the operator can decide immediately; never auto-deleted.
- repo-name: `<branch>` (upstream gone; PR #N merged — safe to force-delete)
  - `git branch -D <branch>`
- repo-name: `<branch>` (upstream gone; no merged PR — investigate; may hold WIP)
  - `git log --oneline origin/<default>..<branch>` — see what's there
  - then `git branch -d <branch>` (safe-delete; refuses if commits would be lost) or `git branch -D <branch>` (force-delete; only after manual review)
- repo-name: `<branch>` (upstream gone; merge status unverified — gh query failed)
  - Run `gh auth status` and retry. Note a healthy `gh auth status` does not clear this: the map query also fails on a rate limit, a 5xx, or a repo whose remote is not a GitHub host at all. Check the PR manually before deleting
- repo-name: `<branch>` (upstream gone; merge status unverified — merged-PR map truncated at N; older PRs missing)
  - The repo has more merged PRs than the map cap, and the map drops the OLDEST — which is where the oldest stale branches point. Raise the cap and re-run, or check this PR by hand

### Never-Tracked Local Branches (NO upstream ever configured — local cleanup candidates)
A **distinct class**, rendered separately on purpose: `%(upstream:track)` is *empty*, not `gone`, for these, so the `gone`-only scan never emitted them — ~19 of the 49 branches hand-removed on 2026-09-16/17 were in this blind spot. Empty means no upstream was ever configured (no `-u` on the push, or a live remote branch the local config never pointed at), which is **not** the same fact as "the remote branch was deleted" — hence the separate section. Same annotation path, same rule: **never auto-deleted**.
- repo-name: `<branch>` (never tracked; remote counterpart still exists on '<remote>'; PR #N merged)
  - The remote branch is still there — deleting the local ref loses nothing, but confirm the PR is the one you think it is first
- repo-name: `<branch>` (never tracked; no remote counterpart on '<remote>'; PR #N merged)
  - Content landed (likely via squash); `git branch -D <branch>` after review
- repo-name: `<branch>` (never tracked; remote counterpart still exists on '<remote>'; no merged PR — investigate; may hold WIP)
  - **The most interesting row in this class**: a branch with a LIVE remote counterpart and no merged PR — pushed at some point, never proposed, and never tracked locally. `git log --oneline origin/<default>..<branch>` to see what is there, and check whether the remote branch is someone else's live work before touching either ref
- repo-name: `<branch>` (never tracked; no remote counterpart on '<remote>'; no merged PR — investigate; may hold WIP)
  - `git log --oneline origin/<default>..<branch>` — see what's there; this is the class most likely to hold unpushed work, since nothing ever pushed it
- repo-name: `<branch>` (never tracked; <remote status>; merge status unverified — gh query failed)
  - As above — a healthy `gh auth status` does not clear it
- repo-name: `<branch>` (never tracked; <remote status>; merge status unverified — merged-PR map truncated at N; older PRs missing)
  - As above — raise the cap and re-run

### PR-Protected (skipped — has open PR)
- repo-name (branch) — PR #N open; default has X new commits; branch X commits behind origin

### Drifted Feature Branches (skipped — non-default, no PR)
- repo-name (branch) — branch X commits behind origin; default has Y new commits available for next checkout

### Default-Ref Synced (background fast-forward; non-default branch is checked out)
- repo-name (default) — fast-forwarded the local default ref by X commits without checking it out

### Detached HEAD (skipped)
- repo-name — at `<sha>`; checkout a branch (`git checkout main`) or finish the in-flight op (`git rebase --continue`, `git bisect reset`) before re-running.

### No Remote / No Tracking
- repo-name — reason (no remote, no tracking branch, etc.)

### Stash Status (default-branch only — non-default branches are NEVER auto-stashed)
- repo-name — stash popped successfully / stash pop had conflicts (resolved)

### Errors
- repo-name — error description
```

---

## Rules

- **Never ask the user** which version to keep — decide autonomously
- **Always read commit messages** before resolving conflicts to understand intent
- **Prefer the direction of evolution** — newer architectural changes win
- **Keep the codebase buildable** — verify resolved files are syntactically valid
- **If a rebase becomes too complex** (>10 conflict rounds), abort with `git rebase --abort` and fall back to `git pull --no-rebase` with merge conflict resolution instead
- **Never force push** — this command only pulls
- **Never rebase a branch with an open PR** — rewriting committed SHAs breaks the PR's review history. Always treat PR-protected branches as skipped (Phase 2, Case B).
- **Never auto-stash on a non-default branch** — WIP on a feature branch belongs to that feature; pull-all has no business touching it. Auto-stashing is reserved for two paths only: (1) the default-branch fast-forward path (Case A); (2) the orphan-branch switch path (Case D), where the upstream is `gone` and staying on the branch is itself the worse option than stashing-then-switching.
- **Never auto-delete an orphan branch** — even after Case D switches to default, the orphan local ref is retained. Squash-merged PRs leave their orphans with commits not directly in default by SHA, so a naive merge-check is wrong; force-deleted-without-merge orphans may hold real WIP. The Phase 4 report gives the user the exact commands; the decision is theirs. **Same rule applies to BOTH classes of the Phase-2 step-5 stale-orphan scan** — orphans whose upstream is `gone` but that aren't currently checked out, and branches that never had an upstream configured at all (empty, not `gone`). Surface the first in the "Stale Orphan Branches" report section and the second in "Never-Tracked Local Branches"; never auto-delete either. Widening what the scan *detects* changes nothing about what `pull-all` is allowed to *do*.
- **Background-sync default refs only when safe** — `git fetch origin "$default:$default"` updates the local default ref without checking it out, but it silently no-ops if you're already on default or have local commits ahead. Both no-ops are correct; do not work around them.
- **Log every decision** so the user can review what was resolved and why
