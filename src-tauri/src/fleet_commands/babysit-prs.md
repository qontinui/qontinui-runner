---
description: Shepherd this session's PRs to landed — watch CI, fix red in-session, diagnose green-but-stuck PRs via coord's pr-merge events, admin-merge ONLY diagnosed coord defects, then write a remediation plan and run /vet-imp on it so coord itself gets fixed.
argument-hint: "[repo#123 ...] [--threshold=45m] [--no-merge] [--no-remediate] [--once]"
allowed-tools: Read, Write, Edit, Bash, PowerShell, Grep, Glob, Monitor, Skill, ToolSearch, TaskCreate, TaskUpdate
---

# Babysit PRs — shepherd session PRs to landed

Automates the previously-manual loop: *review the stuck PRs → diagnose why coord
didn't land them → recover → write a remediation plan → vet-imp it*. The core
invariant: **admin-merge is recovery, not routine** — it is allowed only when
the diagnosis shows a coord defect (not a legitimate hold), and every
admin-merge MUST produce a remediation plan so the defect gets fixed. This
keeps CLAUDE.md's "coord is the sole merge authority" rule intact: we bypass
coord only when coord is provably the thing that's broken, and we always pay
for the bypass with a fix.

## Arguments

- `$ARGUMENTS` — `[pr-refs...] [flags]`, all optional:
  - **pr-refs** — explicit PRs as `owner/repo#N` or `repo#N` (owner defaults
    to `qontinui`). When omitted, auto-detect the session's PRs (Step 1).
  - `--threshold=<dur>` — how long a fully-green PR may sit unmerged before
    the diagnosis ladder fires. Default `45m`. Accepts `30m`, `2h`.
  - `--no-merge` — never admin-merge; diagnose, remediate the cause, and
    report only. Use when you want the investigation without the recovery.
  - `--no-remediate` — skip the remediation-plan + `/vet-imp` step (recovery
    only). The defect evidence is still written to the PR comment.
  - `--once` — single pass (no watch loop): assess, act, report, exit.

## Step 1 — Collect the session's PRs

Same detection as `/name`: for each git repo this session has touched (current
worktrees, recent commits), intersect local branches with open PRs:

```bash
gh pr list --repo <owner/repo> --state open --head "$BRANCH" --json number,title,headRefName,labels,isDraft
```

Include PRs the session opened via stacked worktrees. Build a table:
`repo | PR | branch | head_sha | labels | deps` — record `coord:stacked-on=` /
`coord:upstream-of=` labels as dependency edges (parents land before children).
Skip drafts. If zero PRs found and none given, report and exit.

## Step 2 — Watch loop

Use the Monitor tool (persistent) with a poll script that emits one line per
state transition per PR: each check flip (`pass`/`fail`), `MERGED`, `CLOSED`.
Poll every 60s; cover ALL terminal states (a filter that only matches success
signals is a silent-failure bug). Between events, do nothing — the monitor
re-invokes you. With `--once`, skip the monitor and evaluate immediately.

Track per-PR: `first_fully_green_at` (all non-skipped checks pass on the
CURRENT head). The diagnosis clock (Step 4) starts there and RESETS whenever
the head changes or a check flips red. Track **`no_ci_since`** beside it — the
first poll at which the CURRENT head was **successfully read** and carried
**zero** checks. Step 4's second entry clause ages against it; without it that
clause would name a threshold with no origin to measure from, and would never
fire. ⚠️ **A poll that errored, was rate-limited, or could not be completed is
UNKNOWN — do not stamp from it.** "Observed zero" and "failed to observe" are
different facts, and only the first may set this clock; suppressing the error
into a value is how a healthy PR acquires a `no-ci` stamp it never earned.

⚠️ **`no_ci_since` MUST clear the moment the head carries ≥ 1 check** — not only
on a head change. **Every** PR passes through a zero-check window: workflows take
seconds to minutes to be scheduled after a push, so a poll landing in that window
stamps `no_ci_since` on a perfectly healthy PR. With head-change as the only
reset, that stamp then stands as a false statement about the current head while
CI runs, and the threshold can elapse *while checks are green-in-progress*,
dragging a well-behaved PR into Step 4. Two resets, then: **≥ 1 check appears**,
or the head changes. Relatedly, a head younger than the repo's own scheduling
latency is **not yet** `no-ci` — it is simply new.

⚠️ **ZERO checks is not green — "all non-skipped checks pass" is VACUOUSLY TRUE
on a head that has none.** A PR whose CI never fired has no failing check *and*
no pending check, so a predicate that only counts reds stamps
`first_fully_green_at` immediately, the diagnosis clock starts on a lie, and the
PR goes on to satisfy Step 6's *"CI is fully green on the CURRENT head"*
precondition without a single job having run. **Require ≥ 1 non-skipped check
before stamping it**, and record a head with zero as `no-ci` — which enters
Step 4 in its own right (see that step's entry clause), never a green and never
an admin-merge precondition. **Measured** 2026-08-24 across `qontinui-dev-notes`:
**4 of 9** stuck PRs had zero CI runs, so the head shape this predicate misreads
is common. The misread itself is **derived from the predicate's wording, not
observed** — do not cite the 4-of-9 figure as evidence that a session actually
stamped a false green.

⚠️ **Zero checks has TWO causes and only one of them is a defect — do not
collapse them.** A PR whose workflows are all filtered out by `on: paths` (a
docs-only PR in a path-filtered repo) also produces no check runs at all, from
an entirely benign cause, and a rule that parks every zero-check head as `no-ci`
would bar such a PR from Step 6 **forever**. Discriminate exactly as 4d does:
zero checks **plus** `input_freshness.ci_check_row_count: 0` **plus**
`actions/runs?head_sha=<FULL 40-char sha>` → `total_count: 0` is the
**never-fired** class. Zero checks because every workflow is path-filtered is the
**no-baseline** class — that one is coord's `required-checks-missing` question,
not this one, and `merge-train-steward.md` handles it under `no-baseline:<workflow>`.

## Step 3 — CI red → fix in-session

When a check fails on a PR this session authored, fix it here (do not spawn).
**Classify from the failed job's STEPS before you read anything else** — the
first bullet below is that test, and the log is the second one, not the first.
Only once the classification says “real” do you read the failing run log
(`gh run view <id> --log-failed`), and then only for what it sends you to fix:

- **Transient infra** (rate-limit fetching a tool version, runner eviction,
  network, a self-hosted runner losing its GitHub session mid-job):
  `gh run rerun <id> --failed`. At most 2 reruns per head before treating as
  real.

  ⚠️ **Identify this class from the job's STEPS — `cancelled` is the minority
  shape.** Most infra kills on this fleet arrive as `conclusion: failure`, which
  is indistinguishable from a genuine regression at the RUN level, so the run
  conclusion cannot make this call and neither can a log grep. Fetch the jobs of
  the red run, once. `steps` is OPTIONAL on GitHub's job object, so the `// []`
  matters: a bare `.steps[]` aborts the whole filter mid-stream on exactly the
  job Tier 2 exists to catch, printing a partial list that reads like a complete
  one. `per_page=100` matters for the same reason — the default 30 silently
  truncates a wide matrix. **Empty output is UNKNOWN, not “real failure”**: it
  means no `failure`/`cancelled` job on this run, so go back and re-read the
  run's own conclusion before concluding anything.

  ```bash
  gh api "repos/OWNER/REPO/actions/runs/<run_id>/jobs?per_page=100" --jq '.jobs[] | select(.conclusion == "failure" or .conclusion == "cancelled") | {name, conclusion, steps: [(.steps // [])[] | {name, conclusion}]}'
  ```

  | Tier | Predicate on the failed job | Reading | Remedy |
  |---|---|---|---|
  | **1 (primary)** | `conclusion == "failure"` ∧ `steps` non-empty ∧ **NO step has `conclusion == "failure"`** | infrastructure kill | **re-run it** |
  | **2** | `conclusion == "failure"` ∧ `steps` is **empty** | infra-unknown | re-run it |
  | **3 (confirmatory only)** | log contains `The runner has received a shutdown signal` or `lost communication with the server` | corroborates Tier 1/2 | **never sufficient alone** |
  | **none — deliberately untiered** | `conclusion == "cancelled"` (the jq above selects it, so it *will* appear in this output) | **no verdict reached** — the tiers are all keyed on `conclusion == "failure"`, and a cancel must not fall through them into "otherwise → genuine" | **Scope-dependent — read the note below before acting.** On a **PR head**: do not count it as a red. On a **main baseline**: the infra-cancelled class — apply the *matching* row of the steward's red-main remedies table (two remedies, each conditioned) |

  ⚠️ **`cancelled` is NOT `failed` — it reached NO verdict, and that is why it
  gets no tier.** The jq selects `cancelled` so you can *see* it; treating what
  you see as a red is the error. A cancel is a **stop**, not a verdict — the stop
  can come from a concurrency-group supersede, a `fail-fast` sibling, a manual
  stop, or the infra kill named above, and **none of those is a statement about
  your code** (mechanism, not a fleet measurement — do not cite this list as
  observed causes). Reading a cancel as a failure **hides PRs that are actually
  fixable**. Measured 2026-08-22:
  `qontinui-runner#1062` and `#1055` were both passed over as *"has failures
  beyond `security`"*, but those extras were `Clippy diff-scoped (advisory) →
  cancel` and `test (ubuntu-22.04) → cancel` — the latter cancelled after a
  **6 h** run. Both PRs were genuinely `security`-only (the stale-base class) and
  both went **fully green after a rebase**, #1055's previously-cancelled ubuntu
  job reaching a real verdict in **51 m**. So: when deciding whether a PR "has
  failures", filter on the **`fail`** bucket and report `cancel` **separately**;
  the remedy for such a cancel is normally a rebase onto current `origin/main`,
  not `gh run rerun`.

  ⚠️ **SCOPE — the paragraph above is about a PR's OWN head. On a MAIN
  baseline the remedy is the opposite, and this step serves both.** 4d's
  Legitimate-hold row routes `main-red` *into this classifier*, and there a
  `cancelled` job **is** the infra-cancelled class — it stays `RED(cancelled)` in
  the verdict vocabulary, and it never self-heals by waiting. **Which**
  remediation applies is conditioned; see the caveat below, and do not infer it
  from this paragraph. Coord's `auto_fix_red_main` is what the steward cites for
  the **case-1** shape; whether it also covers case 2b is **not stated there**, so
  do not treat it as a general answer for this class either. You
  cannot rebase `main`; looking for a rebase target there is looking for something
  that does not exist, and the red never clears. **The cancel is the same event on either ref — what differs is what you
  can DO to that ref, so the discriminator is WHOSE run is red, not the
  `cancelled` token:**

  | Where the cancelled job sits | Reading | Remedy |
  |---|---|---|
  | The **PR's own head** | no verdict reached; **both measured cases** (#1062, #1055) were the stale-base class | do not count as a red — rebase onto current `origin/main`, which you *can* do to a PR branch |
  | **`main`'s baseline** (you got here from 4d's `main-red`) | `RED(cancelled)`, the infra-cancelled class | **you cannot rebase `main`** — go read the steward's red-main remedies table and apply the row that matches, **do not re-run reflexively** |

  ⚠️ **Do not shorten the main-baseline arm to "re-run it".** That table opens by
  warning that **two** remedies clear a red main and picking the wrong one proves
  nothing: `rerun_failed_jobs` is right only for a run that went red **at the tip**
  (case 1), while a run that went red at an **older** sha on a **path-filtered**
  workflow (case 2b) needs `gh workflow run <wf> --ref main` — a re-run there
  re-adjudicates the *stale* sha and tells you nothing about the
  tip. Row 1 is also **not reached by reading the run `conclusion`**. The condition
  travels with the citation, so cite the table rather than copying a remedy out of it.

  Both halves live in `.claude/commands/merge-train-steward.md`: the PR-head half
  is the **"`cancel` bucket misread as a failure"** row; the main-baseline half is
  **row 1 of the red-main remedies table** (`RED(cancelled)`) and the verdict
  vocabulary beside it.

  ⚠️ **A flat job duration of ~600s / ~601s or ~902s with a step frozen at
  `completed_at: null` is GitHub's abandoned-job reaper, not a `timeout-minutes`
  expiry.** Do not read that round number as a configured timeout, and do not
  “fix” it by raising one.

  **The log grep is ranked last deliberately, and must never be the test.** It
  costs a full download per job; the GitHub-hosted OOM emits the **identical**
  string; and it is **blind to one of the two death shapes outright** — when the
  runner vanishes before flushing, `GET /actions/jobs/<id>/logs` **404s** and the
  frozen step is visible ONLY in the `steps` array. So `--log-failed` returning
  nothing useful is not evidence of a real failure; it is the signature of the
  shape you cannot see that way.

  **ANY failed step means a genuine failure of that step** — Tier 1 requires
  ZERO, so the count that separates the two causes is zero-vs-nonzero. The
  GitHub-hosted OOM is the nonzero case with exactly one (the build step, exit
  143), and it is **futile to re-run**: it needs a resource fix, not another
  attempt.

  If the re-run comes back with a genuinely **failed step**, the Tier-1/2
  classification was wrong: stop re-running and treat it as real. But a repeat
  **zero-failed-step** death is *still* infra — `qontinui-coord` run
  `32336379112` died the same way on attempt 2 (2026-08-20, both jobs on
  `msi-wsl`), so a re-run is not a guaranteed escape while the host is in that
  state. It just spends the second of your 2 permitted reruns, after which
  report the PR as **blocked-on-infra**, not as a code failure. Full derivation,
  counts and validation live in `.claude/commands/merge-train-steward.md` →
  “The `failure`-side discriminator is STEP-LEVEL”.
- **Real failure**: fix in the PR's worktree, commit, push. Bounded to ~3
  fix rounds per PR (per `feedback_autonomous_commit_ship`); if still red,
  surface to the operator with the evidence and stop touching that PR.
- **Stale-green trap**: a PR can be green while main moved under it (e.g.
  alembic sibling heads — two migrations sharing one `down_revision`). When
  main has advanced since the PR's checks ran, re-verify the union: for
  migration-bearing PRs check `alembic heads` against PR ∪ main; a conflict
  here is a REAL failure to fix now (re-parent onto the new chain tip),
  because coord's speculative CI will hit it even though the PR looks green.

## Step 4 — Green-or-no-CI past threshold → diagnose via coord

**Entry — at least two ways in, not one.** Enter on `first_fully_green_at` aged
past the threshold, **or** on a **`no-ci`** head whose `no_ci_since` (Step 2) is
aged past the same threshold. The
second clause is load-bearing: Step 2 forbids stamping `first_fully_green_at` on
a zero-check head, and Step 3's entry is "a check **fails**" — so without it a
never-fired PR satisfies neither step's entry and falls into a gap between them,
which is the exact silent stall 4d's `ci-pending` note exists to catch.

⚠️ **"At least" is deliberate — this list is NOT known to be exhaustive.** A head
carrying **any check that never reaches a verdict** — the clearest case being a
`queued` check run for a self-hosted label with **no runners** — is not green (a
check that never reaches a verdict means "all non-skipped checks pass" is never
satisfied, and **other checks passing does not help** — 4 green + 1 queued-forever
is still this class, so do not read the green ones as a reason the class does not
match), not `no-ci` (check rows exist, so the 3-part signature fails and
`no_ci_since` never stamps), and does not meet Step 3's "a check **fails**" entry.
It sits in the same gap, one class over. Coord's name for that shape is a
`ci-pending` hold with a **non-zero** `ci_check_row_count` — note this is a
*different* block reason code from `merge-state-unsettled` (checks incomplete vs.
GitHub's merge state not settled); do not treat the two as interchangeable names.
⚠️ **The adjacent "required context never reported" shape is NOT unenumerated —
do not double-count it.** 4d's **Coord defect** row already owns it as the
*phantom required-context wedge* (`merge-state-unsettled` dwell > 2× threshold on
a fully-green head, **coord#638**); route that one there, not here.
Enter Step 4 on the unenumerated shape too, aged past the threshold, and diagnose
at 4d. Named here rather than left silent: an unenumerated stall is the failure
mode this whole
step exists to prevent.

Never conclude "coord is thinking." Diagnose in this order; stop at the first
explaining cause.

**4a. GitHub-side sanity** — `gh pr view --json isDraft,mergeable,mergeStateStatus,labels`:
draft, DIRTY (textual conflict → rebase it yourself), or a `coord:blocked` /
dependency label pointing at a still-open parent all explain the hold.
**Record `mergeable` as well as `mergeStateStatus`** — they are different fields
with different vocabularies (`DIRTY` is a `mergeStateStatus` value;
`MERGEABLE`/`CONFLICTING`/`UNKNOWN` are `mergeable` values), and 4d's `ci-pending`
note needs the `mergeable` one.

**4b. Coord liveness** — sample `https://coord.qontinui.io/health` 4–8×:
exactly one replica must report `is_leader: true`. No leader = coord outage →
this is the CLAUDE.md hand-merge exception; skip to Step 6 with class
`coord-down`.

**4c. Coord's verdict** — mint a headless operator token and read the event
stream (never echo the token):

```powershell
$cid = aws ssm get-parameter --name /qontinui/cognito/coord-headless-client-id --region eu-central-1 --query Parameter.Value --output text
$em  = aws ssm get-parameter --name /qontinui/operator/email --with-decryption --region eu-central-1 --query Parameter.Value --output text
$pw  = aws ssm get-parameter --name /qontinui/operator/password --with-decryption --region eu-central-1 --query Parameter.Value --output text
# The operator PASSWORD must not reach the `aws` process's argv: an inline
# `--auth-parameters` pair puts it in a cmdline that any peer session on this
# machine can read (`Get-CimInstance Win32_Process`). Pass the whole request
# through `--cli-input-json file://…` instead — the same off-argv door that
# `curl --data-binary @file` provides in the bash runbooks.
# WriteAllText with a BOM-less UTF8Encoding, NOT Set-Content -Encoding utf8:
# 5.1 writes a BOM there and the AWS CLI's file:// JSON parser rejects it.
$bodyFile = [System.IO.Path]::GetTempFileName()
try {
  $body = @{
    UserPoolId     = 'us-east-1_rgTB9dbZ1'
    ClientId       = $cid
    AuthFlow       = 'ADMIN_USER_PASSWORD_AUTH'
    AuthParameters = @{ USERNAME = $em; PASSWORD = $pw }
  } | ConvertTo-Json -Depth 4 -Compress
  [System.IO.File]::WriteAllText($bodyFile, $body, (New-Object System.Text.UTF8Encoding($false)))
  $tok = (aws cognito-idp admin-initiate-auth --cli-input-json "file://$bodyFile" --region us-east-1 --output json | ConvertFrom-Json).AuthenticationResult.IdToken
} finally { Remove-Item $bodyFile -Force -ErrorAction SilentlyContinue }
# The header goes in an in-process hashtable, never on a cmdline.
$h = @{ Authorization = "Bearer $tok" }
Invoke-RestMethod -Uri "https://coord.qontinui.io/pr-merge/events/<owner>/<repo>/<pr>" -Headers $h
```

From the newest events read: the latest `predicate_eval`'s
`block_reason_code` + `detail`, any `unlandable_cycle` rows (and their
`cycles` counter), and whether the newest `hydration.head_sha` matches the
PR's CURRENT head (mismatch = stale ingest). Also useful:
`GET /pr-merge/graph?repo=<repo>&pr=<n>` (`cycle_detected`, `cycle_members`)
and `GET /merge/queue` (an in-flight proposal means coord is actively landing
it — WAIT, do not race it).

**4d. Classify** the `block_reason_code`:

| Class | Codes / signals | Action |
|---|---|---|
| **Transient — wait** | `ci-pending` (**only once you have PROVEN CI actually fired — see the note under this table**), `below-green-dwell`, `merge-state-unsettled` (young), in-flight proposal in `/merge/queue` | Nothing. Reset no clocks; check again next poll. |
| **Legitimate hold — fix the cause, NEVER bypass** | `ci-not-green`, `main-red`, `main-status-unknown`, `not-open`, `required-checks-missing`, `behind-main-or-unstable` (DIRTY), `auto-merge-disabled`, `dry-run-mode`, `escalate-path-matched`, `has-cross-repo-dependency` via `stacked-on` with parent still open | Fix in-session (rebase, fix CI) or, for `escalate-path-matched`, surface to the operator via `/ask-operator` — that gate exists to force human review of secrets/migrations/infra; bypassing it defeats its purpose. ⚠️ For `ci-not-green` **and `main-red` alike, run Step 3's step-level classifier on the failed job FIRST**: a Tier-1/2 kill is not a cause to fix, it is a re-run — of main's own run in the `main-red` case — and it will **never** self-heal on its own. A `main-red` hold is a legitimate hold either way, but the remedy is not the same one. |
| **Coord defect — recover + remediate** | `has-cross-repo-dependency` where the labeled PR is the UPSTREAM of the edge (`coord:upstream-of=` deadlock — engine parent-resolution inverted); `unlandable_cycle` spinning (cycles > ~5) with all members green; `merge-state-unsettled` dwell > 2× threshold on a fully-green head (phantom required-context wedge, coord#638); latest hydration `head_sha` ≠ current head for > 1h (stale ingest); predicate `result: pass` with no landing and no queue entry for > 1h; `has-blocking-label` on a live PR (retired code — should be extinct) | Step 5 → 6. |
| **Coord down** | no leader across 4–8 health samples | Step 6 directly (the sanctioned exception). |

⚠️ **`ci-pending` is transient only if CI actually FIRED. When it never fired
it is PERMANENT, and waiting on it is an unbounded wait on a state that cannot
change.** A **CONFLICTING** PR gets no new `pull_request` workflow runs *at all*
— GitHub cannot compute `refs/pull/N/merge`, so it schedules nothing: not
queued, not skipped. No runs ⇒ no check rows ⇒ coord parks the PR in
`ci-pending` forever. The trap is that such a PR has **no FAILING checks**, so
any sweep that counts reds reports it healthy. 4a's `DIRTY` triage does catch the
**CONFLICTING** half — but the **never-fired-yet-not-conflicting** half passes 4a
clean and arrives *here*, where this table would have you wait on it forever.
Prove which one you have *before* returning to the poll loop:

- `/reevaluate` reports `input_freshness.ci_check_row_count: 0`, **and**
- `gh api "repos/<owner>/<repo>/actions/runs?head_sha=<FULL 40-char sha>"`
  returns `total_count: 0`. ⚠️ Use the **full 40-char** sha — a short sha
  returns a silent `200` with `total_count: 0` and fakes this exact symptom.

Then split on the `mergeable` value 4a recorded. ⚠️ **That field is TERNARY.**
GitHub's GraphQL `MergeableState` enum is exactly `MERGEABLE | CONFLICTING |
UNKNOWN` — verified 2026-08-25 by reading the live schema, so this is checkable
rather than remembered:

```bash
gh api graphql -f query='{ __type(name: "MergeableState") { enumValues { name } } }' \
  --jq '.data.__type.enumValues[].name'
```

`UNKNOWN` is the not-yet-computed state, and **a two-armed split silently routes
it into whichever arm you wrote first.** How OFTEN this class reads `UNKNOWN` is
**not measured** — #347 recorded no `mergeable` values for the 9 dev-notes PRs —
so do not reach for the third arm expecting it; handle it because the enum has
three values and a missing arm is a silent misroute:

- **`CONFLICTING`** → **resolve the conflict; CI follows.** `gh pr close && gh pr
  reopen` is a **no-op** here — it succeeds and schedules nothing (verified on
  `qontinui-dev-notes#203`, `#84`, `#78`). Do not go hunting for disabled
  Actions or a missing workflow file; that is the wrong diagnosis.
- **`UNKNOWN`** → **not an arm — re-read it.** Poll `mergeable` again after a
  short delay and act only on a settled value. `UNKNOWN` is UNKNOWN, never a
  quiet synonym for `MERGEABLE`; guessing costs you a close/reopen that does
  nothing and leaves the PR exactly as stuck.
- **`MERGEABLE`** with CI simply never fired → `gh pr close <n> && gh pr reopen
  <n>` fires `reopened` and schedules it, with no content change, no new commit
  and the same head sha (verified on `qontinui-dev-notes#153`: green in ~20 s,
  which then let coord reach its real verdict, `[already-landed] — close this
  PR`). ⚠️ A `MERGEABLE`/`CLEAN` read is **GitHub's merge test passing**, never
  "coord can rebase this" — measured 2026-08-19, `qontinui-dev-notes#148` read
  `mergeable: MERGEABLE, mergeStateStatus: CLEAN` while coord held a **terminal
  `conflict`**, stuck 30.8h (steward, **Green-but-dirty** row). That caveat bears
  on a coord *rebase* hold, **not** on an absent workflow run — GitHub's merge ref
  computes fine in the #148 class, so CI is scheduled there normally. If
  close/reopen schedules nothing, re-check the two things that actually cause
  that — a short sha on the runs query, and the workflow's own `on:` triggers.

Either way this is a **legitimate hold you can clear** — not a coord defect, and
not something to wait out. Measured 2026-08-24: **4 of 9** stuck
`qontinui-dev-notes` PRs were in this state. Full row:
`.claude/commands/merge-train-steward.md` → **"Conflicting PR gets NO new CI —
coord parks it in `ci-pending` forever"**.

## Step 5 — Sanctioned recovery levers (before any admin-merge)

Try cheapest-first; give each one ~2 poll cycles to take effect:

1. **Force re-evaluation**: `POST /pr-merge/prs/<owner>/<repo>/<pr>/reevaluate`
   (same bearer) — runs one predicate eval immediately. Cures stale snapshots.
2. **Remove the defective input** when the diagnosis identifies one — e.g.
   the `coord:upstream-of` deadlock is broken by
   `gh pr edit <pr> --remove-label "coord:upstream-of=..."` on the upstream PR
   (ordering usually survives via the dep edge until the next label sync, and
   the upstream lands first anyway).
3. **Fresh hydration**: a no-op label touch, or a new head push if you have a
   legitimate commit to add (never an empty commit just to poke coord).

If a lever clears the block (predicate flips to `pass`/`ci-pending`), return
to the watch loop — coord lands it, no admin-merge.

## Step 6 — Admin-merge (recovery path only)

> "Admin-merge" here is shorthand for **hand-merging outside coord**. It does
> NOT mean `gh pr merge --admin` — that flag is not the mechanism (see item 2
> of the procedure below).

> ### ⚠️ An agent CANNOT complete this step (since 2026-08-21, PR #328)
>
> Read this before working the preconditions, so you do not spend a rebase and
> a full CI cycle reaching a wall. PR #328 added
> `deny: ["Bash(gh pr merge)", "Bash(gh pr merge:*)"]` to the shared
> `.claude/settings.json` — which, via the workspace-root `.claude` symlink, is
> this session's settings. A `deny` is evaluated before `ask` and `allow`, holds
> in **every** permission mode including `bypassPermissions`, cannot be
> overridden by `settings.local.json` / user settings / `--allowedTools`, and
> **cannot be approved by a `PreToolUse` hook**. Compound and wrapped spellings
> (`cd x && …`, `;`, `bash -c "…"`, a leading `VAR=…`) are split and matched
> independently, so none of them evade it.
>
> Item 2 below is therefore **unexecutable by any agent in this fleet**. It is
> left in place because it is still the correct procedure — it is just the
> operator's to run now, not yours.
>
> **So: work Steps 1–5 fully, and when a genuine coord-defect diagnosis reaches
> the merge, hand it over instead of attempting it.** Post the Step 6 item 1
> audit comment (that is yours and still required), then register a gate
> (`/gate`) or escalate naming the PR, the diagnosis class and the
> `block_reason_code` evidence, so the PR is watched rather than silently
> dropped by a session that just stopped. Then continue to Step 7 — the
> remediation plan is unaffected and is the part that kills the defect class.
>
> **Do not route around the deny.** `gh pr merge` wraps
> `PUT /repos/{owner}/{repo}/pulls/{n}/merge`; reaching that endpoint via
> `gh api` is the same act and is blocked by `git-guard.sh` with a typed reason.
> Hiding either spelling in a shell variable or a script defeats the hook, not
> the policy.
>
> Background, and why this diverges from served policy `git-operations`
> `merge-authority` @8 (whose second sentence sanctions this recovery):
> `qontinui-claude-config/knowledge-base/qontinui-specific/coord-merge-train.md`
> → "That last step is MECHANICALLY DENIED to agents". Whether the deny stays is
> the operator's call; it is recorded as a policy gap, not resolved here.

Preconditions — ALL must hold:
- Diagnosis class is **coord defect** or **coord down** (never a legitimate
  hold, never transient).
- Step 5 levers tried and did not clear it (skip levers when coord is down).
- `--no-merge` not set.
- `/merge/queue` shows no in-flight proposal for this PR (do not race coord).
- CI is fully green on the CURRENT head, including the stale-green re-check
  from Step 3 — and **green means ≥ 1 non-skipped check that PASSED**, never a
  head with zero checks (Step 2). A never-fired head satisfies "no check is
  failing" vacuously; that is a `no-ci` diagnosis, not a green one.
  ⚠️ **This bars the never-fired class, NOT the `no-baseline` one** — see Step 2's
  two-causes warning. A head with zero checks because every workflow is
  path-filtered can never satisfy a "≥ 1 check passed" precondition *at all*, so
  reading this bullet as covering both classes bars a benign docs-only PR from
  recovery permanently. A `no-baseline` head is coord's `required-checks-missing`
  question, not a green-ness one: it is **out of scope for this step** — do not
  hand-merge it on the strength of having no checks, and do not park it either.
  Note it never reaches this bullet in the first place on a correct reading: a
  `no-baseline` head is neither a **coord defect** nor **coord down**, so it fails
  this step's FIRST precondition already. Its home is 4d's **Legitimate hold**
  row, where coord's `required-checks-missing` already lives.
  ⚠️ **Work the loop before escalating it.** There is a cheap read that usually
  dissolves the decision outright: does the repo's branch ruleset actually
  *require* a context this PR's path filters exclude? If nothing is required,
  `required-checks-missing` cannot fire and there is no configuration decision to
  make. ⚠️ **It takes TWO calls — the LIST endpoint carries no `rules` field at
  all**, so querying required checks from the list silently finds none and fakes
  a "nothing is required" answer. Measured 2026-08-25: `.[0] | keys` on the list
  returns `_links, created_at, enforcement, id, name, node_id, source,
  source_type, target, updated_at` — no `rules`. Get the id, then read the
  ruleset:

  ```bash
  gh api repos/<owner>/<repo>/rulesets --jq '.[] | "\(.id) \(.name)"'
  gh api repos/<owner>/<repo>/rulesets/<id> \
    --jq '.rules[] | select(.type=="required_status_checks")
                   | .parameters.required_status_checks[].context'
  ```

  ⚠️ **Read EVERY ruleset whose target covers this branch, not just the first id.**
  Both repos measured here happen to have exactly one, so a single read looks
  sufficient and is not: on a repo with two rulesets targeting `main`, reading one
  under-reports the required contexts and fails **open** — the same direction as
  the list-endpoint trap above.

  ⚠️ **An empty result there is ambiguous — check `[.rules[].type]` before reading
  it as "nothing is required".** Empty means either the ruleset genuinely has no
  `required_status_checks` rule or your read did not land. Measured 2026-08-25:
  `qontinui-claude-config` returns `["deletion","non_fast_forward"]` (genuinely
  no required checks) while `qontinui-runner` returns `["non_fast_forward",
  "deletion","required_status_checks","pull_request"]` and lists 10 contexts —
  so the same empty output distinguishes the two only via the rule-type read.
  ⚠️ There is a **third** state the rule-type read alone still misreports: a
  `required_status_checks` rule that exists with an **empty contexts array** shows
  the type as present while requiring nothing. That one errs toward escalating
  rather than skipping, so it is the safe direction — but do not report it as
  "something is required" without looking at the contexts. A
  read you could not complete is **UNKNOWN, not "nothing is required"**, and since
  this read GATES the escalation, treating UNKNOWN as "nothing" converts the gate
  into an unconditional skip. Only a demonstrated conflict —
  a required context that no workflow on this path can ever report — is worth
  `/ask-operator`, and then send the discriminator output with it (zero checks
  *without* the 3-part never-fired signature). Escalation here is a closed-list
  judgement under served policy `escalation-bar` `escalation-closed-list`, not a
  default. That clause's anti-trigger list names, verbatim, *"high blast radius
  alone, **when a verification gate (tests + CI + coord merge train) exists**"* —
  quote the qualifier, because blast radius with NO verification gate is a
  different case the policy may well admit. That a question you have not yet
  worked is also not a trigger **follows from** the closed list rather than being
  listed in it; read the served clause yourself (`/policy get policy
  escalation-bar`) before leaning on either.

Then:
1. **Leave the audit trail first** — comment on the PR: diagnosis class, the
   `block_reason_code` evidence (quote the latest `predicate_eval` payload),
   levers tried, and "admin-merged as coord-defect recovery; remediation plan
   follows."
2. **Merge in dependency order** — parents before children
   (`coord:upstream-of` source first, `stacked-on` parents first). After each
   parent lands, rebase the child onto the new main and let its CI settle
   before merging it.
   **How to actually merge — use the rebase path, not `--admin`.** On
   2026-07-04 `gh pr merge --admin` was observed failing with "required status
   checks expected"; that observation is real, but the premise once recorded
   here to explain it — "the bypass lists contain only the coord GitHub App
   (`Integration:3825026`)" — is FALSE, and so is the conclusion built on it
   ("therefore only the App can ever bypass"). Measured 2026-07-29: the four
   `main-merge-gates` rulesets (runner, schemas, qontinui, ui-bridge) also carry
   `OrganizationAdmin` with `bypass_mode: always`; only the three
   `*-protect-main` rulesets (coord, web, claude-config) are App-only. Bypass
   lists differ **per repo** — re-read `bypass_actors` for the repo in hand
   rather than restating any table — see the `--admin` section of
   `qontinui-claude-config/knowledge-base/qontinui-specific/coord-merge-train.md`.
   Whether `--admin` actually succeeds anywhere is untested by design, so it is
   not a path to reach for. The working recovery: rebase the
   branch onto current `origin/main` (in a worktree, `--force-with-lease`
   push), wait for the required checks to go green on the up-to-date head,
   then plain `gh pr merge <n> --rebase`. Strict rulesets (qontinui-web) need
   the up-to-date head; non-strict ones (qontinui-coord) merge as soon as the
   required checks are green on the current head.
3. **Verify coord ingested the land** — within a few minutes the PR's
   `pr_state` should read `merged` (coord's phantom-open sweep and land-cause
   precedence handle the rest). If a dependent PR still carries a
   dependency label on the now-merged parent, coord's Phase-6 resolver strips
   it on next eval; only intervene if it doesn't.

## Step 7 — Remediation plan + /vet-imp

Skip only if `--no-remediate`. Every coord-defect diagnosis — whether or not
an admin-merge happened — produces a plan so the defect class dies:

1. Write `$QONTINUI_PLANS_DIR/YYYY-MM-DD-coord-<defect-slug>.md` containing
   (**if `$QONTINUI_PLANS_DIR` is unset** — sessions launched outside the qontinui
   runner do not get it injected — ask the user once where plans live, or fall back
   to `<workspace-root>/plans`; never assume an absolute path from another machine):
   - **Symptom**: the PRs, timeline, what the operator saw.
   - **Evidence**: the `predicate_eval` / `unlandable_cycle` payloads, graph
     output, health samples — verbatim excerpts, not paraphrase.
   - **Root cause**: the specific coord code path (file:line where known).
   - **Fix design**: the code change, plus the detection gap (e.g. should
     `stuck_pr_watcher` / `coord.alerts` have caught this class? If yes, the
     plan includes the alert).
   - **Recovery taken**: labels removed, admin-merges performed, with links.
2. Invoke `Skill: vet-imp` with that plan path.
3. If the session must end before the fix lands, register a coord gate via
   `/gate` (or `/blocked`) on the fix PR instead of leaving a silent stall.

## Rules

- **Never bypass a legitimate hold.** `escalate-path-matched` and red CI are
  the system working. The command's value is telling these apart from
  defects, with evidence.
- **Evidence before action.** No admin-merge without a quoted
  `block_reason_code` diagnosis on the PR. "It's been a while" is not a
  diagnosis.
- **Bounded loops.** Max 3 CI-fix rounds per PR, 2 infra reruns per head,
  one remediation plan per defect class per run (dedupe: if `$QONTINUI_PLANS_DIR`
  — or `$QONTINUI_PLANS_ARCHIVE_DIR`, when that is set and different — already has
  a plan for this defect class, reference it instead of writing a twin; check
  before writing).
- **Don't race coord.** An in-flight queue proposal always wins; wait.
- **Report on exit**: per-PR final state (merged-by-coord / admin-merged /
  fixed-and-waiting / blocked-legitimate / operator-escalated), defects found,
  plan path + vet-imp outcome.
