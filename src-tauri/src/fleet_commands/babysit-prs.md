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
the head changes or a check flips red.

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

## Step 4 — Green but unmerged past threshold → diagnose via coord

Never conclude "coord is thinking." Diagnose in this order; stop at the first
explaining cause.

**4a. GitHub-side sanity** — `gh pr view --json isDraft,mergeable,mergeStateStatus,labels`:
draft, DIRTY (textual conflict → rebase it yourself), or a `coord:blocked` /
dependency label pointing at a still-open parent all explain the hold.

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
| **Transient — wait** | `ci-pending`, `below-green-dwell`, `merge-state-unsettled` (young), in-flight proposal in `/merge/queue` | Nothing. Reset no clocks; check again next poll. |
| **Legitimate hold — fix the cause, NEVER bypass** | `ci-not-green`, `main-red`, `main-status-unknown`, `not-open`, `required-checks-missing`, `behind-main-or-unstable` (DIRTY), `auto-merge-disabled`, `dry-run-mode`, `escalate-path-matched`, `has-cross-repo-dependency` via `stacked-on` with parent still open | Fix in-session (rebase, fix CI) or, for `escalate-path-matched`, surface to the operator via `/ask-operator` — that gate exists to force human review of secrets/migrations/infra; bypassing it defeats its purpose. ⚠️ For `ci-not-green` **and `main-red` alike, run Step 3's step-level classifier on the failed job FIRST**: a Tier-1/2 kill is not a cause to fix, it is a re-run — of main's own run in the `main-red` case — and it will **never** self-heal on its own. A `main-red` hold is a legitimate hold either way, but the remedy is not the same one. |
| **Coord defect — recover + remediate** | `has-cross-repo-dependency` where the labeled PR is the UPSTREAM of the edge (`coord:upstream-of=` deadlock — engine parent-resolution inverted); `unlandable_cycle` spinning (cycles > ~5) with all members green; `merge-state-unsettled` dwell > 2× threshold on a fully-green head (phantom required-context wedge, coord#638); latest hydration `head_sha` ≠ current head for > 1h (stale ingest); predicate `result: pass` with no landing and no queue entry for > 1h; `has-blocking-label` on a live PR (retired code — should be extinct) | Step 5 → 6. |
| **Coord down** | no leader across 4–8 health samples | Step 6 directly (the sanctioned exception). |

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

Preconditions — ALL must hold:
- Diagnosis class is **coord defect** or **coord down** (never a legitimate
  hold, never transient).
- Step 5 levers tried and did not clear it (skip levers when coord is down).
- `--no-merge` not set.
- `/merge/queue` shows no in-flight proposal for this PR (do not race coord).
- CI is fully green on the CURRENT head, including the stale-green re-check
  from Step 3.

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
