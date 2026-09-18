# Publish Runner

Cut and publish a new **qontinui-runner** desktop release (Tauri v2, Windows NSIS installer) with a working auto-updater. Bumps the version everywhere, tags a CI-green commit, lets the release workflow build + sign + auto-publish, then verifies the release, the updater endpoint, and the web download actually flipped.

Argument: the target version, e.g. `/publish-runner 1.0.5`. If omitted, read the current version (Phase 1) and propose the next patch bump for confirmation.

Repo: `qontinui/qontinui-runner`. Working tree: `<workspace-root>/qontinui-runner` (path-dep on sibling `../../qontinui-schemas` — never clone the runner elsewhere or the build won't resolve).

---

## How the release actually ships (mental model — read once)

- **Everything is done in GitHub — you never need a private key locally.** The build, the minisign signing, and the publish all happen inside GitHub Actions on the tag push. Running this command is: bump the version, land it, push a tag. That's it. The signing key exists **only as the `TAURI_SIGNING_PRIVATE_KEY` repo secret** (already configured on `qontinui/qontinui-runner`) — CI reads it to sign; nobody running `/publish-runner` holds, sets, or supplies a private key on their machine. Phase 0's secret check is a **read-only confirmation that the repo secret still exists**, not a step where you provide a key.
- **Trigger is a tag push**, not a branch push. `.github/workflows/release.yml` fires on `push: tags: v*`. Pushing a `vX.Y.Z` tag is what starts a release; nothing else does.
- The workflow **creates a DRAFT release, builds every platform, and auto-un-drafts** to `--latest` **only after the Windows `-setup.exe` uploads** (`publish-update-json` job, hard-gated on the Windows asset via `always() && create-release==success`). macOS/Linux legs are `continue-on-error` — a red mac/linux leg does NOT block publish.
- **The macOS-arm64 leg is slow** (~much longer than Windows). Auto-publish waits on the whole matrix. If Windows is green and its assets are up but the release is still a draft because mac-arm64 is dragging, you can **publish by hand immediately** (Phase 5 fallback) — the Windows asset gate is the only thing that actually matters.
- **Auto-update is real as of v1.0.3.** The Windows leg signs each bundle (minisign, `createUpdaterArtifacts: true`) using repo secrets `TAURI_SIGNING_PRIVATE_KEY` + `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, emits `<installer>.sig`, and assembles a **real-signed `latest.json`**. The updater endpoint is `releases/latest/download/latest.json`. The public key is embedded in `src-tauri/tauri.conf.json` (`plugins.updater.pubkey`). **DO NOT change the pubkey** — rotating it breaks auto-update for every existing install and forces everyone to reinstall.

---

## Phase 0: Preconditions (fail fast)

```bash
cd <workspace-root>/qontinui-runner
# 1. Confirm the signing secrets still exist ON THE REPO (GitHub-side; CI reads
#    them to sign). This is a read-only presence check — you do NOT supply a key.
#    NOTE: the row for the private key is tab-delimited, so `grep " "` can miss it;
#    just eyeball the full list if the grep looks short.
gh secret list -R qontinui/qontinui-runner | grep -E "TAURI_SIGNING_PRIVATE_KEY( |	|_PASSWORD)"
# 2. main must be GREEN — a red required check (usually a fresh RUSTSEC advisory
#    on the `security` cargo-audit job) blocks the merge train fleet-wide AND
#    means you'd be tagging on top of red.
gh run list -R qontinui/qontinui-runner --branch main --limit 3 --json workflowName,conclusion,headSha
```

⚠️ **`gh run list` cannot answer this question — it is the wrong instrument, not
just a loosely-worded one.** It enumerates **workflow runs**; what you need to
judge is **check contexts**, and the two are not the same objects. A run can
conclude `success` with a context inside it skipped, and a context can be missing
entirely with no run to represent it — **neither is visible to any refinement of
`run list`**, however you bind or widen it. Scanning its output for `failure`
answers *"is anything RED?"*; a tag needs *"was this commit actually verified?"*,
which takes the two reads below and is **not** a single "are all ten green at the
tip" test — see the framing note after the block, before you act on anything here.

```bash
TIP=$(gh api repos/qontinui/qontinui-runner/commits/main --jq .sha)
# what merging INTO main requires -- a PR-HEAD gate, NOT a predicate on a main
# commit; several of these contexts can never report on a push (see below)
gh api repos/qontinui/qontinui-runner/rules/branches/main \
  --jq '.[] | select(.type=="required_status_checks")
            | .parameters.required_status_checks[].context'
# READ 1 -- what the tip actually reports. DEDUPED: one row per context name
gh api "repos/qontinui/qontinui-runner/commits/$TIP/check-runs?per_page=100" \
  --jq '[.check_runs[]] | group_by(.name) | map(max_by(.started_at))
        | .[] | "\(.name) | \(.status) | \(.conclusion)"'

# READ 2 -- the PR that produced $TIP, and its head's contexts. MANDATORY, not a
# confirmation step: branch protection here is bypassable (see below).
REPO=qontinui/qontinui-runner
SUBJ=$(gh api "repos/$REPO/commits/$TIP" --jq '.commit.message' | head -1)
# Branch on the EXTRACTION, not on a separate glob: one test, one truth. A glob
# broad enough to match `(#2) support (WIP)` would claim a subject this sed cannot
# serve, and there would be no fall-through to the route that can.
PRNUM=$(printf '%s' "$SUBJ" | sed -n 's/.*(#\([0-9]\{1,\}\))$/\1/p')
if [ -n "$PRNUM" ]; then       # squash land: the PR is named in the subject
  PRHEAD=$(gh pr view "$PRNUM" --repo "$REPO" --json headRefOid --jq .headRefOid)
else                           # rebase land: match PR title to subject over a LISTING.
  # NOT --search: it does not find a just-landed PR at all (see the traps below).
  # sort=updated, because a land is an UPDATE -- a number-ordered window drops the
  # old-branch-landed-today PR, which is exactly the one you are tagging.
  # Require EXACTLY ONE match. Real `jq`, not gh's --jq (rejects --arg).
  PRHEAD=$(gh api "repos/$REPO/pulls?state=all&sort=updated&direction=desc&per_page=100" \
             --jq '[.[] | {title, head: .head.sha}]' \
           | jq -r --arg s "$SUBJ" \
               '[.[] | select(.title == $s)]
                | if length == 1 then .[0].head else empty end')
fi
# GUARD THE CALL, NOT THE VARIABLE -- an empty $PRHEAD would otherwise build
# `commits//check-runs`, which returns HTTP 422 and still exits 0.
if [ -n "$PRHEAD" ]; then
  # the SAME deduped call -- and here comparing against all ten required
  # contexts IS correct:
  gh api "repos/$REPO/commits/$PRHEAD/check-runs?per_page=100" \
    --jq '[.check_runs[]] | group_by(.name) | map(max_by(.started_at))
          | .[] | "\(.name) | \(.status) | \(.conclusion)"'
else
  echo "UNKNOWN: could not resolve the PR for $TIP -- DO NOT TAG"
fi
```

⚠️ **The dedup is mandatory, and `?filter=latest` does NOT do it.** Names repeat
across check *suites*, not just attempts within one, so the plain call returns
several rows per context and the obvious knob does not collapse them — measured
2026-08-29 at `9ed902f4`: **14 rows for 8 distinct names** (six contexts twice
over) both with and without `filter=latest`, while the same read at `7a478d22`
returned 8 rows with no duplicates at all. So the shape is intermittent, and a
recipe that works today silently hands you a doubled list tomorrow. **When two
rows share a name and disagree, the latest `started_at` wins** — that is what the
`group_by`/`max_by` above encodes; verified to reduce those 14 rows to the 8
distinct contexts. Also note `per_page=100` is unpaginated: truncation would
present as a *missing* required context, which fails safe into the UNKNOWN arm
below rather than into a false green, but read a surprising absence as possible
truncation before treating it as a finding.

⚠️ **The required-context list is the PR-HEAD merge gate, not a main-commit
predicate — do not compare a main tip against all ten.** Branch protection
evaluates those contexts on a pull request's head, and several of the producing
workflows are `pull_request`-only *by construction*, so they can never report on a
push commit. Measured 2026-08-29 on **both** tips (`9ed902f4` and `7a478d22`):
`clorinde-fresh`, `schema-fresh` and `Gitleaks Secret Detection` have no check run
at all — and `.github/workflows/secret-scan.yml` is literally `on: pull_request:`
with the job named `Gitleaks Secret Detection`. **Nothing is wrong there.** A gate
demanding all ten at a main tip is unsatisfiable on this repo, and a gate nobody
can pass is one every agent learns to ignore.

So the honest question for a tag is asked in two halves:

- **At the tip** — every context that *did* report, deduped, is `completed` +
  `success`, with no `cancelled` (see below) and nothing still unconcluded.
  ⚠️ **This predicate is an ALLOW-LIST — only `success` passes** — which is
  deliberate and worth naming, because every other conclusion rule in this family
  is a deny-list (`cancelled` is not a red, `skipped` is not a pass, `""` is not
  green) and a reader in that habit will start enumerating. Do not: an allow-list
  fails closed on conclusions nobody enumerated — `neutral`, `timed_out`,
  `action_required`, `stale` — without needing to know they exist. (`neutral` is
  live here: `Qontinui merge gate` returned it, observed 2026-08-29.)
- **On the PR that produced it** — the ten required contexts passed on that PR's
  head. That PR head is the one place comparing against all ten **is** correct:
  same deduped `check-runs` call, pointed at the PR's head sha instead of `$TIP`.

⚠️ **Do NOT substitute "branch protection already enforced it" for that second
read.** Measured 2026-08-29, `qontinui-runner`'s `main-merge-gates` ruleset carries
**two `always` bypass actors** — `OrganizationAdmin`, and `Integration:3825026`,
which is the actor coord lands through here (that coord is the sole merge authority
for `qontinui/*` is fleet policy — `git-operations` `merge-authority`; what was
measured is that this actor *may* bypass, not its share of lands). A merge
performed under a bypass does not prove the required contexts were satisfied, so
the assumption is worth nothing on the path that does the landing.
(`.claude/commands/merge-train-steward.md`'s Verified-green-stuck row records the
same bypass measurement and its lesson — re-read `bypass_actors`, never restate a
table. Bypass lists differ per repo.) The deliverable here is a **signed release
tag**; the read is mandatory, not a confirmation step you may skip.

⚠️ **The two OBVIOUS routes from a main sha to its PR both fail on a coord land —
use the subject search instead.** Measured 2026-08-29 on tip `9ed902f4`:

- `gh api repos/.../commits/$TIP/pulls` → **empty**. Not because the commit was
  pushed directly: a coord land rewrites the sha, so the pushed main tip is not any
  PR's head and GitHub's commit→PR association has nothing to match. The land
  shapes that make this so are the fleet's documented ones —
  `qontinui-claude-config/knowledge-base/qontinui-specific/coord-ff-lands.md`.
- the subject carries **no `(#NNNN)` marker**, so the squash-merge fallback is
  absent too. That is the repo's normal shape, not one commit's quirk: **0 of the
  last 30** `qontinui-runner` `main` subjects carry the marker (measured
  2026-08-29). ⚠️ **Per-repo, though — do not generalise it across the fleet.**
  `qontinui-claude-config`'s `main` is almost entirely `(#NNNN)`-suffixed over the
  same window, because it squash-lands. Both shapes are live, which is why the
  block above branches on the subject rather than assuming either: a `(#NNNN)`
  subject names its PR outright and needs no search at all.

Neither absence is a finding, and **an empty result is UNKNOWN — never "nothing
gated it"**. What works is matching the PR **title** against the commit subject,
which a rebase-land preserves — over a **listing**, filtered to an exact match and
required to be unique. ⚠️ **Do NOT reach for `--search` here.** It is the obvious
choice and it fails in two independent ways, both measured 2026-08-29:

- **It is tokenized full-text, not exact-title, so it returns unrelated PRs.** On
  the subject of tip `9ed902f4`, `--state all --search` returned **two** rows: PR
  **#1182** (`CLOSED`, the real one) and PR **#2** (`MERGED`, `fix(pg): v32 — add
  missing ENUM columns…`, entirely unrelated). ⚠️ **The intuitive tie-break picks
  the wrong one:** "the commit is on `main`, so take the `MERGED` row" selects #2,
  and READ 2 then validates a *different commit's* contexts and signs off on a
  green that was never about this release. A false positive carrying a real head
  sha is far more dangerous than a null.
- **It does not find a landed PR that a listing finds — and it does not recover.**
  Tip `09b1d574`'s subject searched to **`[]`**, while `gh pr view 1193` returned
  that same title, `state: "CLOSED"`, `closedAt: 2026-08-29T09:07:27Z`, head
  `84bd4503`. Re-run hours later: still `[]`. ⚠️ **The cause is NOT established** —
  indexing, or tokenization of a title carrying `(terminal):` and a long clause, or
  something else; do not repeat "the index lags", which would imply it self-heals,
  and it did not. What is measured is that the PR exists, a listing finds it, and
  `--search` does not. That is a stronger reason to avoid the tool, not a weaker one.

**The listing route has neither defect** — it does not consult the search index,
and the client-side `.title == $s` is exact. Verified on that same tip: exactly
**one** row, PR **#1193**, head `84bd4503`. ⚠️ **Order it by `updated`, not by PR
number.** `gh pr list` returns newest-**created** first, so its window is a
*creation* window — measured, `--limit 100` spanned #1104–#1203 contiguously — and
the PR it drops is precisely the one you are most likely to be tagging: opened
weeks ago on a long-lived branch, landed today. `sort=updated` spans #1038–#1203
for the same page size (a land *is* an update) and still resolves #1193. Its one
limit is the page: a PR untouched for longer than 100 others falls outside it,
which is UNKNOWN — widen `per_page`/paginate — not absence.

⚠️ **`--state merged` is wrong on either route**, for a reason of its own: a coord
ff/rebase land leaves the PR **`state: "CLOSED"` with `mergedAt: null`** (measured
on #1182), so a merged-only filter excludes the correct row outright
(`coord-ff-lands.md`: both `Closed` *and* `Merged` are lands. ⚠️ Do not reach for a
`merged` field to test this — `gh pr view --json merged` errors with
`Unknown JSON field: "merged"`; the observables are `state` and `mergedAt`.)

The **cardinality check is the load-bearing half** — without it the filter is just
a different way to guess. **Zero or more than one → UNKNOWN, do not tag.** ⚠️ It
must be a real `jq` pipe: `gh`'s built-in `--jq` **rejects `--arg`**
(`unknown arguments`), so the subject cannot be passed into it safely.

End-to-end, on tip `9ed902f4`: the filter resolved **PR #1182, `CLOSED`**, head
`a7df6a6f`, and the deduped read at that head showed **all ten** required contexts
`completed`+`success` — including the three absent at the tip. That is the whole
framing confirmed on one commit: absent at the tip, green on the PR head.

If the subject search comes up empty or ambiguous, resolve the PR through coord's
own record — it performed the land — and if you still cannot establish which PR
produced the commit, **say so and do not tag on READ 1 alone**: half the evidence
is missing, which is UNKNOWN, not a pass.

⚠️ **When a context is missing at the tip, the test is the producing workflow's
`on:` block AS A WHOLE — event, `branches`, and `paths` — never `paths` alone.**
`secret-scan.yml` has no `paths:` at any level; an agent checking only `paths`
finds nothing to explain the absence, wrongly concludes "should have reported and
did not", and withholds the tag forever. Resolve each absence to one of:

- **structurally cannot report here** (the workflow does not trigger on `push` to
  `main`, or its `branches`/`paths` exclude this head) → benign; not a reason to
  withhold the tag;
- **should have reported and did not** → a real hold; do not tag.

Three ways the old `run list` read said green when it was not:

- **An unconcluded run has an EMPTY `conclusion`, not a red one.** On tip
  `7a478d22` the three rows were `CI → ""`, `forbid runner.* schema regressions →
  "success"`, `page-spec paths producer → "success"` — nothing to find by scanning
  for `failure`, while `CI` was still running. ⚠️ **This is a property of the
  MOMENT, not of that commit**: re-read minutes later, `7a478d22` was all
  `success`, and the *next* tip `9ed902f4` was showing `CI → "" / in_progress`
  in its turn. Every push passes through this window, so re-running the command
  and seeing three greens does not retire the warning. Treat `""` /
  `in_progress` / `queued` as **UNKNOWN, never green**.
- **Absent contexts are invisible to `run list` entirely.** On tip `9ed902f4`,
  main required **ten** contexts (`test (ubuntu-22.04)`, `test (windows-latest)`,
  `Frontend unit tests (vitest)`, `seam-gate`, `security`, `forbid-runner-schema`,
  `clorinde-fresh`, `schema-fresh`, `Gitleaks Secret Detection`,
  `Clippy (windows)`) — and three of them had **no row at all** at the tip.
  `run list` could not have shown you that in either direction: it enumerates
  runs, so a context with no run is simply not in its output, and a context
  *inside* a green run is not in its output either. **What those three absences
  MEAN is settled by the framing note above — do not re-derive a disposition
  here.** Note also that none of the ten is named `CI`: the `CI` *workflow*
  produces several of them, which is why a workflow-level read cannot be mapped
  onto this list at all.
- **ZERO rows is not green either** — "nothing is failing" is *vacuously true* on a
  head with nothing to fail, the same vacuity `.claude/commands/babysit-prs.md`
  Step 2 and Step 6 close for a PR head. Absence is UNKNOWN until resolved by the
  `on:`-block test above; it is neither a pass nor, on its own, a refusal.

  ⚠️ **Resolve an absence by finding the PRODUCER, and do not look for it by
  name.** A required *context* is a **job** name; `actions/workflows` lists
  **workflow display names**, and the two are different strings — matching one
  against the other is a test that cannot succeed, so a null result from it is
  evidence of nothing. Measured 2026-08-29, all three "missing" producers exist
  and are found by reading the workflow files, not by name-matching the list:
  `clorinde-fresh` ← `clorinde-bindings-fresh.yml` ("clorinde bindings
  freshness"), `schema-fresh` ← `schema-pg-sql-fresh.yml`
  ("schema.pg.sql.generated freshness"), `Gitleaks Secret Detection` ←
  `secret-scan.yml` ("Secret Scan"), whose `jobs.gitleaks.name` **is** that
  context. None of the three is a phantom required context, and none of them
  belongs anywhere near that diagnosis.

⚠️ **And a `cancelled` is NOT a red — but on `main` it is not nothing, either.**
This applies to READ 1: a row from the deduped `check-runs` call whose `conclusion`
is `cancelled`. READ 1 resolves `$TIP` from `commits/main`, so it is unambiguously
a **main baseline** and the scope rule applies: a cancel on a PR head reaches no
verdict and is rebased away,
but a cancel on `main` is the **infra-cancelled** class, `RED(cancelled)`, and it
**never self-heals**. Do not tag on top of it and do not reflexively re-run it —
apply the matching row of `.claude/commands/merge-train-steward.md` → **the
red-main remedies table** (`rerun_failed_jobs` only for a run red *at the tip*; a
path-filtered workflow red at an *older* sha needs `gh workflow run <wf> --ref main`
instead, because a re-run there re-adjudicates the stale sha and tells you nothing
about the commit you are about to tag). ⚠️ **And in that second case the dispatch
does not clear the verdict** — a `workflow_dispatch` run does not establish a
push-keyed baseline, so the red survives its own remedy. Do not read "I ran the
dispatch and it went green" as authorisation to tag; that is the same file's
warning about the ccfg edge-trigger in a different costume.

Both secrets MUST be present **on the repo** (that's all this checks — GitHub-side presence). They were set once and persist; signing happens in CI, so there is nothing to supply locally when publishing. If one has genuinely gone missing, STOP — this is the rare case where the operator (who holds the key material out-of-band) must re-set it: PowerShell `gh secret set NAME -R qontinui/qontinui-runner --body (Get-Content path -Raw).Trim()` — NOT `< file`, PowerShell doesn't support `<`. Do NOT block a normal publish on generating or having a private key yourself; you never need one.

**If `security` (cargo-audit) is red on main**, it HOLDS the merge train — but it does not deadlock every merge. `main-red` is checked ONLY at ENQUEUE (Tier 4 of `pr_merge::predicate::is_simple_green_path`) and is never re-consulted at land; two further routes (`POST /merge/propose`, `engine::enqueue_merge_proposal_for_pr`) enqueue without the predicate at all. So a PR can still land under a red main: measured 2026-08-20, `qontinui-runner#1076` got `block_reason_code: "main-red"` from a live `/reevaluate` and **landed anyway** at 08:41:31Z with `mergedBy = app/qontinui-merge-orchestrator` — coord itself, no human, no `--admin`; which route had enqueued it was NOT established. ⚠️ **Before assuming a cause at all, check that the job actually failed.** `gh run list --json conclusion` reads the RUN level, where an infrastructure kill is indistinguishable from a real red: fetch the failed job's steps (`gh api repos/qontinui/qontinui-runner/actions/runs/<run_id>/jobs?per_page=100`) and if it has **no step whose `conclusion` is `"failure"`** (or no steps at all), it is an infra kill — re-run it and publish, do not go hunting an advisory that does not exist. Scope this honestly — and **re-measure it, because the figure this sentence used to assert has changed**: `qontinui-runner` was written up as having **zero self-hosted runners** (`GET /repos/qontinui/qontinui-runner/actions/runners` → `total_count: 0`), from which it concluded the self-hosted shape *cannot occur here*. **Measured 2026-09-02 that same call returns `total_count: 1` — `merytshost`, `status: online`, labels `[self-hosted, Linux, X64, qontinui]`.** So the self-hosted shape IS possible in this repo now and the GitHub-hosted one is no longer the only exposure. Run the call rather than quoting either number; a runner inventory is live state, and the conclusion drawn from it inverts when the count crosses zero. ⚠️ **That “one” is a miscount and is corrected here:** the 2026-08-20 sweep counted **11** GitHub-hosted infrastructure kills, not one — §2.3's table has **10** (`Deploy coord` in `qontinui-coord`) plus **1** (`Spec CI` in `qontinui-web`), and reading only the second row understates the class eleven-fold in the very sentence that calls it rare. What the plan writes for this repo is **zero self-hosted kills** (§2.3); that no `qontinui-runner` row appears anywhere in §2.3's enumeration of the 74 is a fair read of a complete table over a swept repo (§2.2: 2026-08-08 → 08-20, 1000-row cap), but it is DERIVED, not written — so treat it as *no kill of any shape was observed here*, not as a measured zero. Either way the hosted shape is **possible but unobserved** in this repo, not “seen once”, and it stays cheap to rule out. Predicate and derivation: `.claude/commands/babysit-prs.md` Step 3 / `.claude/commands/merge-train-steward.md` → “The `failure`-side discriminator is STEP-LEVEL”. ⚠️ **The hosted exposure is TWO shapes, and the zero-failed-step test sees only one of them — so a clean test result here is not the end of the check.** §2.3 splits the 11 hosted kills into **10** `Deploy coord` OOMs and **1** `qontinui-web` `Spec CI` job. The `Spec CI` one is shape A — zero failed steps (§7, run `32289431023`) — so the test above **does** catch it and a re-run is the right remedy. The OOM is the other shape: **exactly one** failed step (the build, exit 143, on a 7 GB hosted runner), so it sails past the test into “genuinely red” and then into the two causes below, **neither of which it is**. So before attributing a failed step to either, **read that step's exit code and the runner it ran on**: a `143` on a `GitHub Actions`-labelled runner is a resource kill, not an advisory and not MSRV drift — re-running it is futile, it needs a resource fix, and there is no `cargo audit` finding to go looking for. Scoped the same way: all **10** measured events of this OOM sub-class were coord's `Deploy coord` build — none was `security`, and none was in `qontinui-runner` at all — so treat it as the third cause to *rule out cheaply*, never as one seen here. Derivation: plan `2026-08-20-self-hosted-runner-shutdowns-produce-spurious-ci-failures` §2.9. Once the job is confirmed genuinely red — a failed step that is not a resource kill — two distinct causes:
- **A new RustSec advisory** against a runner dependency: bump the flagged crate (`cargo update -p <crate> --precise <patched>`), verify `cargo audit` exits 0. ⚠️ **Do NOT route this "via the `coord:red-main-fix` recovery lane" — there is no such lane to route through, and for a security-class fix least of all.** Three independent reasons: (a) the label is **convenience/intent only and is NOT an input to the predicate** — `policies::evaluator::is_recovery_candidate` says so verbatim ("a mislabeled (or unlabeled) PR is judged purely on these facts"), which is deliberate — a mislabeled PR "can never force-land unless it truly fixes main", and labelling yours changes nothing; (b) the in-predicate waiver it names **still cannot be relied on** — `is_recovery_candidate` requires `rebased_candidate_green`, whose only producer is `pr_merge::engine::head_has_green_speculative_candidate` (a green, non-invalidated `coord.speculative_chains` row); speculative candidate CI is ARMED in production since the arm PR of plan `2026-07-25-coord-speculative-push-before-gate-churn` §8.4 step 6 (2026-09-03, qontinui-coord#1894) — `COORD_SPECULATIVE_DISABLED` is now an ordinary default-ON kill switch, `"1"` disables and unset arms, with `deploy/taskdef.json` at `"0"` — so that producer CAN produce rows and the waiver is no longer inert BY THAT CAUSE, but the bootstrap gap plan `2026-08-20-coord-red-main-recovery-lane-is-inert` records remains (a Tier-4-blocked PR never gets a proposal, so no chain is built for it); coord's own `fixer_arm_readiness::adjacent_breakages` entry `red_main_recovery_merge_lane_inert` now derives its state from the live flag read rather than asserting a prod value; (c) even if it fired, the predicate requires `!security_class_touched`, so it refuses security-class PRs **by design** — a cargo-audit bump is exactly that. **So never wait for the waiver to fire; that wait never ends.** Open the fix PR green and non-draft and let coord's ordinary merge path land it; never `--admin`. Applying the label as intent signalling is still fine, but set it over the REST labels route — `gh api -X POST repos/<owner>/<repo>/issues/<pr>/labels -f 'labels[]=coord:red-main-fix'` — because BOTH of the other doors refuse it: `pr_merge::labels_routes::validate_label` **rejects** `coord:red-main-fix`, so `/coord-pr-label` cannot; and `gh pr edit --add-label` cannot either, since its `repository.pullRequest.projectCards` GraphQL prefetch is refused under the Projects-classic sunset and it exits 1 **before** applying the label (gh 2.46.0, reproduced 2026-09-04). The REST route needs no GraphQL and creates a missing label as a side effect; rationale in `.claude/skills/coord-pr-label/set-label.sh`. Full derivation of all three points is in `.claude/commands/merge-train-steward.md`. See `reference_runner_rustsec_redmain_cargoaudit_coord_recovery`.
- **`cargo install cargo-audit` fails to COMPILE** (exit 101, ~20s fast-fail, log says e.g. "kstring@X requires rustc 1.96.0 / Try --locked"): this is upstream *tooling* MSRV drift, NOT an advisory and NOT a runner vuln. CI pins rustc 1.95.0 (`rust-toolchain.toml` + ci.yml dtolnay); the unpinned install pulled a too-new transitive dep. **FIX = add `--locked` to `cargo install cargo-audit` in `.github/workflows/ci.yml`** (installs cargo-audit against its own tested lockfile). Do NOT bump the toolchain — huge blast radius and it still edits a gated workflow. This happened on the v1.0.5 cut (2026-07-14, kstring 2.0.3).

**Editing `ci.yml` (or any gating workflow) trips the `ci-integrity.yml` guard** ("Guard gating workflows from self-edits") which reds CI by design. BUT that guard is NOT a required context in the ruleset — so an armed auto-merge STILL lands the PR once the actually-required checks (security + test ubuntu/windows) pass. It's an advisory red, not a hard block; don't assume you need `--admin` for a workflow-edit PR — check `mergeStateStatus` / whether it merges on its own first. (On v1.0.5, folding the `--locked` fix into the release-bump PR #772 auto-merged with no admin override.)

## Phase 1: Read current version + decide target

```bash
cd <workspace-root>/qontinui-runner
grep -m1 '"version"' src-tauri/tauri.conf.json
gh release list -R qontinui/qontinui-runner --limit 3
```

The published GitHub "latest" is the real production version — trust it over local files (local may be mid-bump on a feature branch). Target = next version per the user's arg, or propose `latest + patch`.

## Phase 2: Bump the version in ALL FOUR places

The version lives in four files and they MUST match, or the build is inconsistent (Cargo.lock mismatch fails `--locked`; a stale tauri.conf serves the wrong updater version):

1. `src-tauri/tauri.conf.json` → `"version"` (top-level)
2. `src-tauri/Cargo.toml` → `version = "…"` (package, near line 3)
3. `package.json` → `"version"`
4. `Cargo.lock` → the `[[package]] name = "qontinui-runner"` entry's `version`

Edit files 1–3 with Edit. For `Cargo.lock`, prefer `cargo update -p qontinui-runner --precise <version>` (or edit the one entry by hand). Do NOT run a bare `cargo update` — it churns unrelated crates and can pull in a fresh RustSec advisory that reds `security`.

**Do these bumps on a normal feature branch and land them through the merge train** — the tag must point at a commit that already exists and is CI-green on main. Do not tag an unmerged local commit.

## Phase 3: Land the bump, then tag a green commit

1. Open a PR with the four version bumps (plus any release-note edits) and let coord's merge train land it once green. **Do not run `gh pr merge` or `--admin`** — coord is the sole merge authority for `qontinui/*` repos (CLAUDE.md; coord-served policy `git-operations` `merge-authority`). The tag in Phase 3 must point at a commit that is already on `main` and green, so waiting for coord is a real dependency here, not a formality. **Gotcha:** if you re-point a PR's base, `pull_request` CI does NOT re-fire — `gh pr close <n> && gh pr reopen <n>` to force a `reopened` event.
2. Confirm the merge commit is green on main, then tag **that exact SHA**:

```bash
cd <workspace-root>/qontinui-runner
git fetch origin main
SHA=$(git rev-parse origin/main)          # or the specific merge SHA you verified green
git tag v<version> $SHA
git push origin v<version>                 # <-- this is what launches the release
```

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

The tag push is the release trigger. Watch it:

```bash
gh run list -R qontinui/qontinui-runner --workflow=release.yml --limit 3
gh run watch -R qontinui/qontinui-runner <run-id>
```

**Local-push clippy gotcha:** `cargo-prepush` runs `clippy --all-targets` (stricter than CI) and pre-existing test-file lints can block an unrelated push. Bypass with `QONTINUI_PREPUSH_SKIP=1 git push …`. (Tag pushes usually don't trip this, but branch pushes for the bump PR can.)

## Phase 4: Let it build + auto-publish

The workflow: creates the draft → builds the matrix → Windows leg signs + writes `latest.json` → `publish-update-json` verifies the Windows `-setup.exe` is present → un-drafts to `--latest`.

Green Windows leg + assets uploaded ⇒ it auto-publishes. If you're watching and Windows is done but the release is still a draft because mac-arm64 is slow, go to Phase 5's manual publish — don't wait it out.

**Do NOT re-diagnose an `os error 2` NSIS failure as the resources glob or the NSIS toolchain** — that was resolved (real cause was the `--target` flag, PR #684, long merged). If the Windows leg genuinely fails now, read the actual error; it's something new. See memory `reference_runner_release_nsis_os_error2_not_resources_glob`.

## Phase 5: Publish manually (fallback — only if auto-publish is gated)

If Windows is green + its assets are up but the release is still a draft (slow/failed non-blocking leg):

```bash
# Confirm the Windows installer + signed manifest are actually on the release first:
gh release view v<version> -R qontinui/qontinui-runner --json assets --jq '.assets[].name'
# Expect: Qontinui.Runner_<version>_x64-setup.exe, .exe.sig, latest.json, checksums-windows-x64.txt
gh release edit v<version> -R qontinui/qontinui-runner --draft=false --prerelease=false --latest
```

Never publish a release missing the `-setup.exe` or `latest.json` — an assetless "latest" breaks both the web download and the updater.

## Phase 6: Verify the release is genuinely live (do NOT skip)

Every pre-v1.0.3 release *looked* configured but shipped empty signatures. Verify for real:

```bash
# 1. GitHub "latest" is the new version
gh release list -R qontinui/qontinui-runner --limit 3   # top row should show "Latest" on v<version>

# 2. Updater endpoint serves the new version with a REAL (non-empty) signature
curl -sL https://github.com/qontinui/qontinui-runner/releases/latest/download/latest.json | head -c 500
#    -> "version":"v<version>" and a long base64 "signature" that decodes to
#       "signature from tauri secret key" (NOT empty, NOT the pubkey).

# 3. The manifest signature matches the uploaded .sig byte-for-byte
#    (download both; the "signature" field in latest.json must equal the .sig file contents)

# 4. Web download flips to the new version (may lag GitHub by a bit / needs auth on the page route)
curl -sI https://github.com/qontinui/qontinui-runner/releases/latest/download/Qontinui.Runner_<version>_x64-setup.exe | grep -i location
```

All four must pass. Only then is the release real.

## Phase 7: Report + record

Tell the user:
- New version published + verified (GitHub latest, updater endpoint, signature match, web download).
- **Whether existing users must reinstall or auto-update carries them.** Auto-update works v1.0.3 → onward. Any user on a release that shipped *without* a working updater (≤ v1.0.2) must reinstall **once**; from a working-updater version they auto-update.
- If anything in Phase 6 failed, say exactly which check and stop — do not claim success.

Update memory `reference_runner_device_autoinit_and_autoupdate_v103` (or a successor) with the new version + any new gotcha.

---

## Notes / invariants

- **Four version files, always in lockstep:** tauri.conf.json, src-tauri/Cargo.toml, package.json, Cargo.lock.
- **Never touch `plugins.updater.pubkey`** in tauri.conf.json. Signing-key custody is load-bearing and irreversible-ish — losing/rotating the private key forces a fleet-wide manual reinstall.
- **Tag = release trigger.** No tag, no build. Tag only a CI-green commit that's already on main.
- **Windows is the sole hard gate;** mac/linux legs are non-blocking (`continue-on-error`). If mac-arm64 stalls auto-publish, publish by hand once Windows assets are up.
- **The web download endpoints query GitHub's "latest"** — that's why the un-draft step passes `--latest`. If a release isn't marked latest, the website keeps serving the old one.
- **`os error 2` is NOT the NSIS glob/toolchain** (resolved via #684 `--target`). Read the real error before theorizing.
- **cargo-audit / RUSTSEC** can red `security` on main from an upstream advisory, holding new proposals (incl. the release bump) at ENQUEUE — fix the crate first, then open the fix PR normally and let coord land it. **There is no working `coord:red-main-fix` recovery lane**: the label is not a predicate input, the waiver it names is inert in prod, and the predicate excludes security-class changes anyway (`!security_class_touched`). Never `--admin`; detail in the `security`-red section above.
- **Base re-point doesn't re-fire CI** — `gh pr close && gh pr reopen`.
- **PowerShell has no `<`** — `gh secret set … --body (Get-Content path -Raw).Trim()`.
- **prepush clippy is stricter than CI** — `QONTINUI_PREPUSH_SKIP=1 git push` to bypass pre-existing unrelated lints.
