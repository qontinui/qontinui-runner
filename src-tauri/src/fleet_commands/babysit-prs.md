---
description: "Shepherd this session's PRs to landed — watch CI, fix red in-session, diagnose green-but-stuck PRs via coord's pr-merge events, hand a diagnosed coord defect to the operator (an agent does not merge — the recovery merge is denied to agents fleet-wide since PR #328), then write a remediation plan and run /vet-imp on it so coord itself gets fixed."
argument-hint: "[repo#123 ...] [--threshold=45m] [--no-merge] [--no-remediate] [--once]"
allowed-tools: Read, Write, Edit, Bash, PowerShell, Grep, Glob, Monitor, Skill, ToolSearch, TaskCreate, TaskUpdate
---

# Babysit PRs — shepherd session PRs to landed

Automates the previously-manual loop: *review the stuck PRs → diagnose why coord
didn't land them → recover → write a remediation plan → vet-imp it*. The core
invariant: **admin-merge is recovery, not routine** — it is reached only when
the diagnosis shows a coord defect (not a legitimate hold), and every such
diagnosis MUST produce a remediation plan so the defect gets fixed. This
keeps CLAUDE.md's "coord is the sole merge authority" rule intact: coord is
bypassed only when coord is provably the thing that's broken, and the bypass is
always paid for with a fix.

⚠️ **Since 2026-08-21 (PR #328) the bypass is not YOURS to perform.** The
shared `.claude/settings.json` denies `Bash(gh pr merge)` / `Bash(gh pr
merge:*)` fleet-wide. `git-guard.sh` additionally blocks the `gh api`
REST and GraphQL spellings of the same act (#353) — briefly unwired
fleet-wide (qontinui-claude-config PR #567) along with its unrelated
destructive git/rm/cargo arms (those stay removed, at the repo owner's
explicit request), then re-wired the same day narrowed to ONLY this
merge-route check. So for an agent this command's terminal state
on a diagnosed defect is **hand-off, not merge**: post the audit comment, hand
the PR to the operator with a gate or an escalation, and go write the
remediation plan. Every "admin-merge" below describes the operator's step, not
yours — Step 6 states this in full. Do not read any of it as latitude.

## Arguments

- `$ARGUMENTS` — `[pr-refs...] [flags]`, all optional:
  - **pr-refs** — explicit PRs as `owner/repo#N` or `repo#N` (owner defaults
    to `qontinui`). When omitted, auto-detect the session's PRs (Step 1).
  - `--threshold=<dur>` — how long a fully-green PR may sit unmerged before
    the diagnosis ladder fires. Default `45m`. Accepts `30m`, `2h`.
  - `--no-merge` — never admin-merge; diagnose, remediate the cause, and
    report only. Use when you want the investigation without the recovery.
    **Its meaning shifted with PR #328, it did not go inert.** The deny already
    makes the merge unreachable to you, so the flag no longer gates *your*
    merge — nothing does, because you have none. What it still gates is the
    **hand-off REQUEST**: with `--no-merge` you diagnose, remediate and report,
    and you do **not** ask the operator to merge. Suppressing that request is
    what the flag now buys. It re-arms against an agent's own merge the moment
    the deny is lifted, so do **not** read its absence as permission — the same
    reading `--max-recovery-merges` gets in `/merge-train-steward`. The audit
    comment is owed either way; under `--no-merge` write it as a diagnosis-only
    record.
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

⚠️ **Do not hand-write the poll. Invoke `scripts/watch-until.sh`.** Two of the
three watcher failures of 2026-09-06 (coord finding `952fafad`) were loops
written against the instruction that used to start this step, and the ~90 lines
of trap prose below are now that skeleton's RATIONALE rather than a checklist
you have to satisfy by hand:

```bash
bash <config-repo>/scripts/watch-until.sh --pr <owner/repo>#<n>
# and, once, before you trust its silence:
bash <config-repo>/scripts/watch-until.sh --pr <owner/repo>#<n> --prove
```

It reads coord's check-run API through `scripts/lib/pr-checks.sh` rather than
the `gh pr checks` state column (which renders a merge gate HOLDING the PR as
`skipping`), it prints every `EMPTY`/`UNMARKED`/`TOOL_ERROR` poll instead of
reading it as calm, and its default condition requires `rows=[1-9][0-9]*`, so a
head with zero checks cannot satisfy it — the vacuous green this step warns
about below. Background and the three transcripts:
`knowledge-base/qontinui-specific/watcher-honesty.md`.

**It is the default route, not the only one.** The skeleton is single-probe by
design, so a multi-PR fan-out still goes through the Monitor tool — one
`watch-until.sh --pr` per PR, or the hand-rolled loop below if you genuinely
need one table across PRs. Whichever you use, the traps below still apply.

Use the Monitor tool (persistent) with a poll script that emits one line per
state transition per PR: each check flip (`pass`/`fail`), `MERGED`, `CLOSED`.
Poll every 60s; cover ALL terminal states (a filter that only matches success
signals is a silent-failure bug). Between events, do nothing — the monitor
re-invokes you. With `--once`, skip the monitor and evaluate immediately.

⚠️ **A transition-only filter cannot deliver the two stationary classes this step
tracks, so the loop needs a TIME tick as well as an event one.**
`pass`/`fail`/`MERGED`/`CLOSED` are
*transitions*; a head whose CI **never fired** emits none of them, ever, and
neither does a head whose only non-passing check is a **`cancel`** once that
cancel has settled. Both are stationary states, not events. With transitions as
the only wake-up, nothing about such a PR ever re-invokes the session, so
`no_ci_since` is never re-examined after the poll that stamped it and **Step 4's
entry clauses fire only by accident** — when some *other* PR in the same watch
happens to transition and drags the whole table into a re-evaluation. On a
single-PR watch, or a night when the others are quiet, they never fire at all.
Either way the clock exists and nothing advances it: a threshold that is only
reached when an unrelated PR moves is not a clock.

⚠️ **This starves EVERY Step 4 entry clause, not just the `no-ci` one** — do not
read it as a `no-ci`-only defect. Each clause ages a stamp against a threshold,
and a transition can only ever deliver the *stamping* moment, never the elapsing
one: the flip to green wakes you at `first_fully_green_at` itself, when the clock
is by definition zero, and nothing wakes you again; a settled `cancel` is
stationary by the argument above. Green-then-stuck is starved exactly as
never-fired is.

So: emit a line on each transition **and** on a fixed time tick while any PR is
in a non-terminal state, whatever its checks are doing. ⚠️ **State that tick's
bound in SECONDS, not in polls.** "Every Nth poll" and "Step 4's threshold" are
different units, and an agent that sets `N` to the threshold's *number* produces
a tick 60× too slow at a 60 s poll while believing it satisfied the bound. The
bound is `N × poll_interval ≤ Step 4's threshold`. This is the
"filter that only matches success signals" warning above applied one level up —
a filter that only matches *changes* is silent about a PR that has stopped
changing, which is precisely the stuck PR this command exists to find.

Track per-PR: `first_fully_green_at` — **≥ 1 non-skipped check that PASSED, and
no non-skipped check that has not passed, on the CURRENT head**. ⚠️ Do not
shorten that to *"all non-skipped checks pass"*: that phrasing is **vacuously
true on a head with zero checks**, which is the defect the *"ZERO checks is not
green"* warning below exists to close, and shortening it here reintroduces that
defect at the point of definition — where a reader who skims to the parenthetical
and stops will find it. The diagnosis clock (Step 4) starts there and RESETS
whenever the head changes or a check flips red. Track **`no_ci_since`** beside it — the
first poll at which the CURRENT head was **successfully read** and carried
**zero non-skipped** checks. ⚠️ **"Zero non-skipped", not "zero" — otherwise
tightening the green predicate opens a fresh hole.** A head whose checks all
conclude `skipped` has *check rows*, so a literal zero-rows clock never stamps;
it also has no non-skipped check that passed, so `first_fully_green_at` never
stamps; nothing failed, so Step 3 is out; and nothing is in the `cancel` bucket.
Under the old vacuous wording that head was (wrongly) stamped green and at least
went somewhere; a rule that only fixes the green side strands it in silence
instead. Neither is acceptable — and note the old behaviour was not merely
"different": that false green also fed Step 6's admin-merge precondition, which
is why silence is the lesser of the two only now that Step 6 carries its own
independent bar. Counting *non-skipped* checks puts this head in the same class
as the zero-row one. ⚠️ **The two-arm discriminator below does NOT sort it as
written** — skipped checks are rows, so both of that signature's counters are
non-zero; see the *THREE causes* warning below, whose third and fourth **rows**
are what actually sort it. Do not read this sentence as "and then it is handled".
⚠️ **Rows, not arms.** That warning has three arms because it names three
*causes*; its table needs a fourth *row* only because the third cause splits on
whether the skip can be TRACED, and row 4 returns `UNKNOWN` — a verdict about
the trace, not a fourth cause. Step 4d says "rows 3 and 4" for the same pair.
Step 4's second entry clause ages against it; without it that
clause would name a threshold with no origin to measure from, and would never
fire. ⚠️ **A poll that errored, was rate-limited, or could not be completed is
UNKNOWN — do not stamp from it.** "Observed zero" and "failed to observe" are
different facts, and only the first may set this clock; suppressing the error
into a value is how a healthy PR acquires a `no-ci` stamp it never earned.

⚠️ **`no_ci_since` MUST clear the moment the head carries ≥ 1 non-skipped check**
— not only
on a head change. **Every** PR passes through a zero-check window: workflows take
seconds to minutes to be scheduled after a push, so a poll landing in that window
stamps `no_ci_since` on a perfectly healthy PR. With head-change as the only
reset, that stamp then stands as a false statement about the current head while
CI runs, and the threshold can elapse *while checks are green-in-progress*,
dragging a well-behaved PR into Step 4. Two resets, then: **≥ 1 non-skipped check
appears**, or the head changes. Relatedly, a head younger than the repo's own scheduling
latency is **not yet** `no-ci` — it is simply new.

⚠️ **ZERO checks is not green — "all non-skipped checks pass" is VACUOUSLY TRUE
on a head that has none.** A PR whose CI never fired has no failing check *and*
no pending check, so a predicate that only counts reds stamps
`first_fully_green_at` immediately, the diagnosis clock starts on a lie, and the
PR goes on to satisfy Step 6's *"CI is fully green on the CURRENT head"*
precondition without a single job having run. **Require ≥ 1 non-skipped check
before stamping it**, and record a head with zero as `no-ci` — which enters
Step 4 in its own right (see that step's entry clause), never a green and never
an admin-merge precondition. ⚠️ **Read that requirement as "≥ 1 non-skipped check
that PASSED", not "≥ 1 non-skipped check EXISTS"** — the shorter form is satisfied
by a single check that is still `pending`, which is not green either. The
definition above is the binding one; this sentence is its summary, not a weaker
alternative. **Measured** 2026-08-24 across `qontinui-dev-notes`:
**4 of 9** stuck PRs had zero CI runs, so the head shape this predicate misreads
is common. The misread itself is **derived from the predicate's wording, not
observed** — do not cite the 4-of-9 figure as evidence that a session actually
stamped a false green.

⚠️ **A head with no passing check, and no FAILED and no CANCELLED check either,
has THREE causes and only one of them is a defect — do not collapse them.**
(The scope matters: a head whose check **failed** has no passing check but belongs
to Step 3, and a head whose checks were **all cancelled** belongs to Step 4's third
entry clause, whose remedy is a rebase. Neither is in the table below, and tracing
`on:` blocks will never explain either — this warning is about heads where nothing
ran, or nothing was required.) A PR whose workflows are all filtered out by `on: paths` (a
docs-only PR in a path-filtered repo) produces no check runs at all, from
an entirely benign cause, and a rule that parks every such head as `no-ci`
would bar that PR from Step 6 **forever**. The **never-fired** class is the
defect one: zero checks **plus** `input_freshness.ci_check_row_count: 0` **plus**
`actions/runs?head_sha=<FULL 40-char sha>` → `total_count: 0`. The
**no-baseline** class is coord's `required-checks-missing` question,
not this one, and `merge-train-steward.md` handles it under `no-baseline:<workflow>`.

⚠️ **The third cause is the ALL-SKIPPED head, and the 3-part signature above does
NOT sort it** — this is one warning with three arms, not a two-arm rule with a
correction bolted on. `no_ci_since` stamps on zero *non-skipped*
checks (Step 2), so an all-skipped head reaches this discriminator — and then
matches neither of the first two arms, because skipped checks **are** rows:
`ci_check_row_count` is non-zero and `total_count` is non-zero (the runs exist;
they skipped). Taken as two arms only, this moves the stall from Step 2 to 4d
rather than closing it. The full table, four rows because the untraceable case is
its own answer:

| Head shape | `ci_check_row_count` / `total_count` | Class |
|---|---|---|
| No check rows and no runs at all | both `0` | **never-fired** |
| No check rows because every workflow is path-filtered off | both `0` | **no-baseline** |
| Rows exist but **every** one concluded `skipped`, and each skip is explained by its workflow's own `on:` (branch/paths/event) | both **non-zero** | **no-baseline** — the same question, reached by a different route |
| Rows exist, all `skipped`, and you **cannot** establish why | both **non-zero** | **UNKNOWN** — do not sort it; say so |

The third row is `no-baseline` only when the skip is traced to the workflow
declining this commit — which is what path-filtering looks like once a workflow is
triggered rather than filtered out. **The trace is the test, and it is required:**
open each skipped check's producing workflow and confirm its `on:` block (event,
`branches`, `paths` — all three, not `paths` alone) excludes this head. ⚠️ A
`skipped` also comes from a job-level `if:` and from a `needs:` whose dependency
skipped; neither is `no-baseline`, and neither is measured here. If the trace does
not close, row 4 is the answer — **UNKNOWN is a verdict this table can return**,
and returning it is correct. Never default an untraced skip into the benign arm.

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

  ⚠️ **`(.steps // [])` is load-bearing, and CI guards the bracket
  spelling of it and nothing else.**
  `scripts/lint-command-frontmatter.py` **check #23** fails on the
  bracket-iteration spellings of the bug — including the trap family, where a
  `?` placed BEFORE the `[]` suppresses only the field access, leaves the null
  to be iterated, and still exits 5 while *looking* null-safe (`.steps?[]`,
  `.steps? []`, `.steps?.[]`). The `?` protects the iteration only when it lands
  AFTER the brackets. But the check guards `[]` iteration and **nothing else**:
  `steps` is just as null under `map`, `sort`, `keys`, `add`, `join`,
  `to_entries`, `any`, `all`, `flatten` and `group_by` — all re-measured to
  abort (jq 1.8.2), none guarded. Rewriting the projection above as
  `steps: (.steps | map({name, conclusion}))` reads as a tidy-up, aborts
  identically on the Tier-2 job, and **CI stays green** (measured). So reach for
  `(.steps // [])` whenever you change how the array is *consumed*, not only
  when you type brackets — a green run is **not** a proof this filter is
  null-safe. Full scope: check #23's docstring.

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
  | The **PR's own head** | no verdict reached; **both measured cases** (#1062, #1055) were the stale-base class | do not count as a red — rebase onto current `origin/main`, which you *can* do to a PR branch — run `landed-since.sh check` first (Step 4a.5). The rebased head is a push to that PR's branch, so the ⚠️ carries-the-push check under **Real failure** below runs before and after it, re-testing the PRE-rebase head |
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

  ⚠️ **Does that PR still carry the push?** A PR you are watching can land
  under you between one poll and the next, and coord's ff-land normally leaves
  the branch in place — so the push succeeds and nothing carries the fix
  toward `main`. That includes the shape a watch loop most easily misreads: an
  ff-land that leaves the PR **OPEN** (phantom-open) and moves its head to
  your new commit, which passes a naive re-check. Decide it by
  `knowledge-base/qontinui-specific/coord-ff-lands.md` → "Pushing to a branch
  whose PR may already have landed" — check before the push and again after
  it, re-testing the head you checked BEFORE pushing, and take that section's
  fresh-branch path when the PR carries nothing. A fix pushed onto a landed
  PR's branch consumes one of the ~3 rounds and changes nothing CI sees —
  which is why the check runs before the push, not after it.
- **Stale-green trap**: a PR can be green while main moved under it (e.g.
  alembic sibling heads — two migrations sharing one `down_revision`). When
  main has advanced since the PR's checks ran, re-verify the union: for
  migration-bearing PRs check `alembic heads` against PR ∪ main; a conflict
  here is a REAL failure to fix now (re-parent onto the new chain tip),
  because coord's speculative CI will hit it even though the PR looks green.

## Step 4 — Green-or-no-CI past threshold → diagnose via coord

⚠️ **Before concluding a PR is merely stuck, ask whether a REBASE-LAND would
even reproduce it.** If `git rev-list --merges origin/main..HEAD` is non-empty,
the branch carries a back-merge and coord's rebase land discards it — so the
resolution inside it is not in what lands. Run the oracle:

```sh
RO_BASE=origin/main RO_HEAD=<pr head> bash qontinui-claude-config/scripts/rebase-oracle-check.sh
```

`DIVERGENT-CONFLICT` (rc 1) says coord will park the PR **while GitHub reports
it CLEAN** — GitHub tests a merge, coord performs a rebase — which is a
diagnosis, not a stall. `DIVERGENT-SILENT` (rc 1) is worse and is why this
check belongs here: nothing anywhere reports it, the land looks clean, and the
pre-merge text ships. `NO-MERGES` or `EQUIVALENT` (rc 0) clears the question
and Step 4 proceeds normally; rc 2 is UNKNOWN and clears nothing. The remedy in
either divergent case is `git rebase origin/main` plus hand resolution, never
another back-merge — run `landed-since.sh check` before it (Step 4a.5). Detail:
`knowledge-base/qontinui-specific/coord-merge-train.md` → "Why a back-merge is
not a fix".

**Entry — at least two ways in, not one.** Enter on `first_fully_green_at` aged
past the threshold, **or** on a **`no-ci`** head whose `no_ci_since` (Step 2) is
aged past the same threshold. The
second clause is load-bearing: Step 2 forbids stamping `first_fully_green_at` on
a zero-check head, and Step 3's entry is "a check **fails**" — so without it a
never-fired PR satisfies neither step's entry and falls into a gap between them,
which is the exact silent stall 4d's `ci-pending` note exists to catch.

**Third clause — a `cancel`-only head, and it is TERMINAL.** Enter also on a head
whose only non-passing check is in the **`cancel`** bucket, aged past the same
threshold. It matches neither of the two clauses above: it is **not green** (a
cancelled check is neither skipped nor passed, so `first_fully_green_at` never
stamps) and it is **not `no-ci`** (non-skipped check rows exist, so `no_ci_since`
never stamps); and Step 3's *"when a check **fails**"* entry does not admit it,
because Step 3 itself forbids reading a cancel as a failure.

⚠️ **This clause is an ENUMERATION, not the only door — do not justify it as
"otherwise the PR is invisible".** The catch-all below already admits this head
under its own predicate: a cancel is, in Step 3's words, *"no verdict reached"*,
and the catch-all says to enter Step 4 on any such shape. What this clause adds
is the **remedy** — the catch-all's advice is shaped for a check still pending,
and a settled cancel needs a rebase, not a wait. Where both read on the same
head, **this clause wins**: it is the more specific, and its remedy is the
actionable one.

⚠️ **Do not fold it into the `queued` shape below — the difference is what you
DO.** A `queued`-forever check has not settled, so "wait and re-read" is at least
coherent; a settled `cancel` has reached its final state and **waiting is
unbounded by construction**. It is also not hypothetical: it is the class
measured 2026-08-22 on `qontinui-runner#1062` and `#1055` (Step 3's `cancelled`
row), both of which went fully green after a rebase — so the PRs that reach this
clause are the *fixable* ones, which is exactly why leaving them undetected
costs the most. Diagnose at 4d as usual, but the remedy is normally the Step 3
`cancelled` row's — rebase onto current `origin/main` — not a re-run.

⚠️ **"At least" is deliberate — this list is NOT known to be exhaustive.** A head
carrying **any check that never reaches a verdict** — the clearest case being a
`queued` check run for a self-hosted label with **no runners** — is not green (a
check that never reaches a verdict means "all non-skipped checks pass" is never
satisfied, and **other checks passing does not help** — 4 green + 1 queued-forever
is still this class, so do not read the green ones as a reason the class does not
match), not `no-ci` (**non-skipped** check rows exist, so the 3-part signature
fails and `no_ci_since` never stamps — say *non-skipped*, since an all-skipped
head has rows too and IS routed by `no_ci_since`), and does not meet Step 3's "a check **fails**" entry.
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

⚠️ **Before running 4a–4d: "no red on `gh pr checks`" is not "this PR can
merge."** `gh pr checks` enumerates checks that EXIST on the head — a required
status-check context that never produced a check run at all (its workflow
died at `startup_failure`, or the workflow simply never triggered on this
branch) contributes NO ROW, not a red one, so the command's output is
systematically biased toward looking green: the failure mode **deletes**
evidence instead of adding it. `gh pr view --json mergeable` does not close
the gap either — `mergeable: MERGEABLE` reports the absence of a textual
conflict, never the satisfaction of a required-check ruleset. Treat "required
context missing/never reported" as its own stuck class here, distinct from
both **red CI** (Step 3) and **green but stuck for some other reason** (queue,
stacked dependency, coord itself wedged — the rest of 4d's table): it is 4d's
`required-checks-missing` row, and it is a **Legitimate hold**, never a lesser
severity than a visible red — a PR can carry zero failing checks and still be
unmergeable. Where coord's PR-status surface (`coord_pr_status` / `/pr-status`)
is available, a `blockers` entry naming an unestablished required-check state
is the actionable signal here (a companion coord change is adding this
wording); treat it with the same urgency as a red check, not lower, since
`gh pr checks` alone will never surface it. Once the missing context is
identified, `startup_failure` (the workflow started and died) and "never
triggered" (the trigger/path filter excludes this branch) are different,
actionable causes — re-run the workflow for the former, fix the
trigger/path filter for the latter — name which one you found rather than
treating them as interchangeable.

**4a. GitHub-side sanity** — `gh pr view --json isDraft,mergeable,mergeStateStatus,labels`:
draft, DIRTY (textual conflict → rebase it yourself), or a `coord:blocked` /
dependency label pointing at a still-open parent all explain the hold.
**Record `mergeable` as well as `mergeStateStatus`** — they are different fields
with different vocabularies (`DIRTY` is a `mergeStateStatus` value;
`MERGEABLE`/`CONFLICTING`/`UNKNOWN` are `mergeable` values), and 4d's `ci-pending`
note needs the `mergeable` one.

**4a.5. Before ANY rebase this command sends you to** — this DIRTY one, Step 3's
stale-base / `cancelled` remedy, the rebase-oracle's divergent case, and the
Step 6 recovery alike — ask whether a peer already LANDED the work *(plan
`2026-09-05-a-verification-report-never-states-the-tree-it-read` Phase 5)*:
`bash <workspace-root>/qontinui-claude-config/scripts/landed-since.sh check`.
Exit `1` names the commits, files and PR numbers that landed into the recorded
paths since the baseline — read them first, because the conflict may be a peer's
shipped copy of this PR's work, and resolving it re-proposes a duplicate. Exit
`3` is INCOMPLETE and never an all-clear. The baseline is keyed on the session
id (`$QONTINUI_AGENT_SESSION_ID`, else `$CLAUDE_CODE_SESSION_ID`) of whoever ran
`record` at `/preflight` step 4b or `/implement-plan` Step 0.45, so a
continuation or babysitting session reads no baseline and gets `3` (or `2` with
no id at all) — pass `--baseline ~/.qontinui/landed-since/<recording-session-id>.json`
when you know that id, and otherwise report the probe as UNKNOWN rather than
reading it as clean.

**4b. Coord liveness** — sample `https://coord.qontinui.io/health` 4–8×:
exactly one replica must report `is_leader: true`. No leader = coord outage →
skip to Step 6 with class `coord-down`. **This is no longer a "CLAUDE.md
hand-merge exception"** — that phrasing cited text CLAUDE.md no longer carries.
CLAUDE.md's `merge-authority` bullet now says the #328 deny blocks this very
recovery and that "**that path now ends in an operator hand-off**". A
`coord-down` PR still enters Step 6; what it reaches there is the hand-off, not
a merge you perform.

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
| **Legitimate hold — fix the cause, NEVER bypass** | `ci-not-green`, `main-red`, `main-status-unknown`, `not-open`, `required-checks-missing`, `behind-main-or-unstable` (DIRTY), `auto-merge-disabled`, `dry-run-mode`, `escalate-path-matched`, `has-cross-repo-dependency` via `stacked-on` with parent still open | Fix in-session (rebase, fix CI). For `escalate-path-matched`, read what coord is actually waiting on and fix THAT — see the note under this table; never route around it (no recovery merge, no override you arrange yourself). ⚠️ For `ci-not-green` **and `main-red` alike, run Step 3's step-level classifier on the failed job FIRST**: a Tier-1/2 kill is not a cause to fix, it is a re-run — of main's own run in the `main-red` case — and it will **never** self-heal on its own. A `main-red` hold is a legitimate hold either way, but the remedy is not the same one. |
| **Coord defect — recover + remediate** | `has-cross-repo-dependency` where the labeled PR is the UPSTREAM of the edge (`coord:upstream-of=` deadlock — engine parent-resolution inverted); `unlandable_cycle` spinning (cycles > ~5) with all members green; `merge-state-unsettled` dwell > 2× threshold on a fully-green head (phantom required-context wedge, coord#638); latest hydration `head_sha` ≠ current head for > 1h (stale ingest); predicate `result: pass` with no landing and no queue entry for > 1h; `has-blocking-label` on a live PR (retired code — should be extinct) | Step 5 → 6. |
| **Coord down** | no leader across 4–8 health samples, **and** `bash .claude/skills/coord-revive/coord-revive.sh --floor-claim` printed a `FLOOR-CLAIM:` block reading `verdict=FLOOR` — paste it into the report. `verdict=UNKNOWN` (exit 5: sampled under this box's own load) is NOT this class; wait for the builds to finish and sample again | Step 6 directly — which for an agent ends in the operator hand-off, not a merge (#328). |

**`escalate-path-matched` is a hold with a named pending gate — read it before
deciding who acts.** `coord_pr_merge_verdict {repo, pr_number}` returns an
`escalate` block for exactly this code — `category`, `disposition`,
`pending_gate` and a precise `reason` — and `pending_gate` decides the next
step. It is never bypassed, whatever it says: no recovery merge (Step 6 does
not apply to this code), no override arranged by the agent.

| `pending_gate` | What coord is waiting on | Action | Exit state |
|---|---|---|---|
| `migration_classifier` | the migration failed coord's additive-safety classifier (`qontinui-coord` `crates/coord/src/pr_merge/migration_classifier.rs`); `reason` names the op | Fix the named op in the migration and push — `auto_if_provably_safe` then lands it with no human. If the op is fine and the CLASSIFIER is wrong, fix the classifier in qontinui-coord (plan → `/vet-imp`) and file a finding; the PR waits on that fix | `fixed-and-waiting` (migration fixed) or `blocked-legitimate` (classifier fix in flight) |
| `reversal_gate` | the repo's `Migration Reversal Gate` check at the head is absent or not green | Fix that check | `fixed-and-waiting` |
| `migration_disposition` | coord could not EVALUATE the migration (fetch failure, no GitHub App client, file missing at head) — not a check | Fix the cause `reason` names; if transient, re-evaluate (`coord_reevaluate`) | `fixed-and-waiting` |
| `secret_scan` | the secret scan must pass, and THEN an operator override is still required | Get the scan green, then ask the operator — secrets is closed-list item 1 | `operator-escalated` |
| `awaiting_operator_override` | every other category (`infra`, `dependencies`, `other`) and dispositions `block_hard` / `block_soft` | Ask the operator to review and override or reject — closed-list item 2 (the `strategy_admin` override is a resource no agent can obtain) | `operator-escalated` |

**The override is a human decision by design, not a mechanical act.**
`coord_attest_escalate_override` requires the `strategy_admin` scope precisely
so that no device or agent token can self-clear its own escalation
(`crates/coord/src/mcp/tools.rs`, the override tool's authorization comment).
Where the table says ask, the ask is the review decision itself — "review this
change, then override or reject" — never a request to press a button, and
never a reason to build an agent-clearable path around it.

**When an ask is owed is served policy, not this file** — read it fresh
(`/policy`) rather than from a copy here: [policy: `escalation-bar`
`escalation-closed-list`] (whose anti-triggers include "high blast radius
alone, when a verification gate exists") and, for migrations, [policy:
`production-and-cost` `pipeline-deploys-are-not-adhoc-mutation`]. Ask through
`/ask-operator`, with a recommendation, naming the closed-list item.

This note replaced an unconditional "surface to the operator via
`/ask-operator`". On 2026-09-18 that sent an additive migration PR
(qontinui-web#1393) to the operator when coord's `pending_gate` was
`migration_classifier` — a rejection the author could fix (finding `bff69ae4`).

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

⚠️ **Both counters non-zero does NOT settle it — that is a third answer, not a
"no".** An **all-skipped** head has rows and runs (they skipped), so this two-part
proof returns neither `never-fired` nor its negation, and an agent instructed to
"prove which one you have" is left with a test that cannot produce an answer for
the head Step 2 just routed here. Read the counters as: **both zero** → the
never-fired proof holds, continue below; **both non-zero with ≥ 1 non-skipped
check** → CI did fire, this note does not apply; **both non-zero with EVERY check
`skipped`** → go back to Step 2's *THREE causes* table, whose rows 3 and 4 sort it
(`no-baseline` when each skip traces to its workflow's own `on:`, UNKNOWN when it
does not). Do not force that head into either arm here.

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
   the `coord:upstream-of` deadlock is broken by removing that label from the
   upstream PR (ordering usually survives via the dep edge until the next label
   sync, and the upstream lands first anyway). Remove it over the REST labels
   route, **not with `gh pr edit --remove-label`**, which cannot run here at
   all: `gh pr edit` prefetches `repository.pullRequest.projectCards` over
   GraphQL, GitHub refuses that under the Projects-classic sunset, and it exits
   1 **before** the label is touched — reproduced on gh 2.46.0 2026-09-04, the
   same cause `.claude/skills/coord-pr-label/set-label.sh` documents for the
   add side.

   ```bash
   LABEL='coord:upstream-of=qontinui/qontinui-runner#1229'
   gh api -X DELETE \
     "repos/<owner>/<repo>/issues/<pr>/labels/$(jq -rn --arg l "$LABEL" '$l|@uri')"
   ```

   ⚠️ **The `@uri` encode is load-bearing, not tidiness.** The dep-label grammar
   is `[<owner>/]<repo>#<n>`, so a real label carries `/` and `#`, and neither
   survives an unencoded path segment. **Measured both ways 2026-09-04 on a live
   PR**: unencoded, `gh api -X DELETE .../labels/zz-probe2/a#1` answers
   `404 {"message":"Label does not exist"}` and the label is **still on the PR**
   — a failure that exits non-zero but looks like "already gone" if you do not
   read it; `@uri`-encoded (`zz-probe2%2Fa%231`) the same call succeeds and the
   label is removed. And do **not** reach
   for `PATCH .../issues/<n>` with a `labels` array instead — that route
   **replaces the whole label set**, stripping every other `coord:*` edge on
   the PR, which is the same clobber hazard `-f body=` carries on the body.
3. **Fresh hydration**: a no-op label touch, or a new head push if you have a
   legitimate commit to add (never an empty commit just to poke coord). A head
   push here is subject to the same carries-the-push check as Step 3's —
   `coord-ff-lands.md` → "Pushing to a branch whose PR may already have
   landed". Hydrating a PR that already landed strands the commit and moves
   nothing.

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
> Item 2 below is therefore **denied to every agent in this fleet**. It is
> left in place because it is still the correct procedure — it is just the
> operator's to run now, not yours. ("Denied", not "impossible": the deny and
> the guard cover the spellings an agent actually reaches for, not every
> conceivable client — see "Do not route around the deny" below. The boundary
> is the policy; the mechanism is a backstop.)
>
> **So: work Steps 1–5 fully, and when a genuine coord-defect diagnosis reaches
> the merge, hand it over instead of attempting it.** Register the gate
> (`/gate`) or escalation FIRST — naming the PR, the diagnosis class and the
> `block_reason_code` evidence — then post the Step 6 item 1 audit comment
> quoting that ref, so the PR is watched rather than silently dropped by a
> session that just stopped. (This is the one place the "audit trail first"
> ordering inverts, and it inverts because the hazard it guarded against is
> gone: there is no merge left for you to perform ahead of the record. What the
> comment now has to carry is the ref, and you cannot quote a ref you have not
> yet obtained.) Then continue to Step 7 — the remediation plan is unaffected
> and is the part that kills the defect class.
>
> **Do not route around the deny.** `gh pr merge` wraps
> `PUT /repos/{owner}/{repo}/pulls/{n}/merge`; reaching that endpoint via
> `gh api` is the same act, and is blocked by `git-guard.sh` with a
> typed reason — that hook was briefly unwired fleet-wide (qontinui-claude-config
> PR #567) along with its unrelated destructive git/rm/cargo arms (those stay
> removed), then re-wired the same day narrowed to ONLY this merge-route
> check, so it mechanically stops this spelling again.
>
> Background, and why this diverges from served policy `git-operations`
> `merge-authority` @8 (whose second sentence sanctions this recovery):
> `qontinui-claude-config/knowledge-base/qontinui-specific/coord-merge-train.md`
> → "That last step is MECHANICALLY DENIED to agents". Whether the deny stays is
> the operator's call; it is recorded as a policy gap, not resolved here.

Preconditions — ALL must hold:
- Diagnosis class is **coord defect** or **coord down** (never a legitimate
  hold, never transient) — and **coord down** is stated only as the
  `FLOOR-CLAIM: verdict=FLOOR` block from `bash .claude/skills/coord-revive/coord-revive.sh --floor-claim`, pasted verbatim.
- Step 5 levers tried and did not clear it (skip levers when coord is down).
- `/merge/queue` shows no in-flight proposal for this PR (do not race coord).
- CI is fully green on the CURRENT head, including the stale-green re-check
  from Step 3 — and **green means ≥ 1 non-skipped check that PASSED**, never a
  head with zero checks (Step 2). A never-fired head satisfies "no check is
  failing" vacuously; that is a `no-ci` diagnosis, not a green one.
  ⚠️ **This bars the never-fired class, NOT the `no-baseline` one** — see Step 2's
  *THREE causes* warning. A head with **no passing check** because every workflow
  is path-filtered off it — whether that shows as zero rows or as rows that all
  concluded `skipped`, which are the same question by two routes —
  can never satisfy a "≥ 1 check passed" precondition *at all*, so
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
1. **Leave the audit trail** — comment on the PR: diagnosis class, the
   `block_reason_code` evidence (quote the latest `predicate_eval` payload),
   levers tried, and the disposition.

   **As an agent, register the gate or escalation BEFORE commenting** (the
   blockquote above says why the "first" in this item's old heading had to
   move): the comment's job now includes carrying a watchable ref, and you
   cannot quote one you have not obtained. If the gate registration itself
   fails, say so **in the comment** — *"gate registration failed: `<reason>`;
   this PR is UNWATCHED"* — rather than dropping the field. An absent ref reads
   as an oversight; a stated failure reads as a known gap someone can pick up.

   ⚠️ **Write the disposition you actually performed.** This step used to
   dictate the closing line *"admin-merged as coord-defect recovery; remediation
   plan follows"* — and since #328 an agent reaching here has **not** merged,
   so that line publishes a false claim on a public PR and mislabels the
   PR's state for coord, for the operator, and for the next session that reads
   the thread. Item 1 is still yours; its wording had to follow item 2 into the
   hand-off. Close the comment with, as applicable:

   - **agent** — *"diagnosed coord-defect recovery; the recovery merge is denied
     to agents fleet-wide (PR #328), so this PR is handed to the operator, who
     holds the merge capability. Gate/escalation: `<ref>`. Remediation plan
     follows."*
   - **agent, under `--no-merge`** — the same record **without** the hand-off
     request: state the diagnosis and the evidence, and do not ask the operator
     to merge. That suppression is what the flag now buys (see Arguments).
   - **operator**, having run item 2 — *"admin-merged as coord-defect recovery;
     remediation plan follows."*

   Never claim a merge you did not perform, and never claim one is coming when
   what you filed is a hand-off.
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
   not a path to reach for. **Own the branch first** (`git-operations`
   `own-artifact-lifecycle`): if this session does not, **re-read served
   policy `git-operations` NOW** (`/policy get policy git-operations`) —
   the clause that decides what a non-owner may do,
   `abandoned-pr-branch-is-adoptable-by-route-around`, was authored on
   2026-09-08 and a session working from a five-day-old read proposed the
   exact anti-pattern it names (plan
   `2026-09-10-the-fixer-contract-authority-context-and-provenance-on-a-pr-you-did-not-author`,
   §2a). Then branch on what it says: a **live** owner gets
   `/handoff-stuck-pr <owner/repo#N>` (plan
   `2026-09-06-fleet-scale-stuck-pr-conflict-handoff-protocol`) and does the
   rebase itself or through its coord-dispatched successor; an **absent**
   owner (`/handoff-stuck-pr` exit 3, or exit 4 with no successor) is NOT
   handed off — the clause names that shape an escalation dressed as a
   hand-off (read its wording from the served document, not from here) — it
   is **adopted by route-around**, per "Adopting a foreign branch" below. The working
   recovery: run `landed-since.sh check` (Step 4a.5), then rebase the branch onto current `origin/main` (in a worktree,
   `--force-with-lease` push — a push to the PR's branch, so Step 3's
   carries-the-push check runs before and after it, re-testing the PRE-rebase
   head; a PR that landed under you while it read stuck needs no recovery, and
   the rebase would re-propose its landed commits), wait for the required
   checks to go green on the up-to-date head, then plain
   `gh pr merge <n> --rebase`. Strict rulesets (qontinui-web) need
   the up-to-date head; non-strict ones (qontinui-coord) merge as soon as the
   required checks are green on the current head.
   **Adopting a foreign branch (route-around) — the only shape it may take.**
   The clause's bound is the original branch: never rebase it, never
   force-push it, never push to it at all. Take its content onto a fresh
   branch off current `origin/main` (cherry-pick or re-apply in a worktree —
   once qontinui-claude-config#901 lands, the trailer hook records you as
   `Rebased-By:` and never as the author; until then a replay stamps YOUR
   `Session-Id:` onto the author's commits, which is the corruption that PR
   closes, so check its state before trusting the trailers you produced),
   open a NEW PR from that branch, and leave the
   original PR to be superseded. Every adoption discloses, in the new PR's
   body, all four of these lines — they are what coord#2034 wrote in prose on
   2026-09-08 and what `scripts/adoption-disclosure-check.sh` checks:

   ```
   Adoption-Supersedes: <owner/repo#N> at <original head sha, 40 hex>
   Adoption-Ownership-Evidence: <what proved the owner absent — the /handoff-stuck-pr exit code and its DOOR/RESOLVER lines, or the PARKED probe>
   Adoption-Original-Untouched: <the original branch's sha as it stands now, 40 hex — verified AFTER your work, never before>
   Adoption-Credit: <who authored the content — the Session-Id/Claude-Session trailers or `unknown`>
   ```

   Run `bash <workspace-root>/qontinui-claude-config/scripts/adoption-disclosure-check.sh --body-file <the body>`
   before `gh pr create`; it exits 1 naming the missing line. And **verify
   the `Untouched` sha against the ORIGINAL branch's provenance, not against
   your own** — coord#2034 certified a branch as untouched at a sha that was
   the previous adopter's rebase commit, a correct check on an already
   rewritten record (the plan's §2c). Trailerless commits you re-applied need
   a `Session-Id-Inherited: <sha> <origin|unknown>` line per commit in the
   same body — the inherited arm of the Session-Id trailer gate, where the
   target repo runs one (`qontinui-dev-notes`'s
   `.github/workflows/require-session-id-trailer.yml` documents the grammar;
   read the target repo's own workflow rather than assuming); do not
   rebase-to-stamp them.

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
   runner do not get it injected — ask the user once where plans live, or DISCOVER
   one: from the workspace root, `ls -d plans */plans 2>/dev/null` and use the
   directory that actually exists. Never fall back to a directory you have not
   confirmed is there — a named fallback fails silently on every machine that does
   not have it — and never assume an absolute path from another machine):
   - **Symptom**: the PRs, timeline, what the operator saw.
   - **Evidence**: the `predicate_eval` / `unlandable_cycle` payloads, graph
     output, health samples — verbatim excerpts, not paraphrase.
   - **Root cause**: the specific coord code path (file:line where known).
   - **Fix design**: the code change, plus the detection gap (e.g. should
     `stuck_pr_watcher` / `coord.alerts` have caught this class? If yes, the
     plan includes the alert).
   - **Recovery taken**: labels removed, and — for each PR that reached the
     merge step — the **hand-off** filed (gate id or escalation ref, and the
     operator it went to), with links. Record `admin-merges performed` only when
     an operator actually performed one; an agent's row is a hand-off, and
     writing it as a merge puts a false recovery into the plan that the fix is
     then designed against.
2. Invoke `Skill: vet-imp` with that plan path.
3. If the session must end before the fix lands, register a coord gate via
   `/gate` (or `/blocked`) on the fix PR instead of leaving a silent stall.

## Rules

- **Never bypass a legitimate hold.** `escalate-path-matched` and red CI are
  the system working. The command's value is telling these apart from
  defects, with evidence. For `escalate-path-matched`, "never bypass" is not
  "always escalate": act on the `pending_gate` coord names (4d's
  `escalate-path-matched` table) — most arms are agent work, and the override
  arms are an operator review decision, never something an agent routes
  around.
- **Evidence before action.** No admin-merge — and no hand-off asking the
  operator to perform one — without a quoted `block_reason_code` diagnosis on
  the PR. "It's been a while" is not a diagnosis. The bar does not drop just
  because the act moved to the operator; it rises, because they are acting on
  your evidence rather than their own.
- **Never report a merge you did not perform.** Since #328 an agent's terminal
  state on a diagnosed defect is `handed-to-operator`. Both the PR audit comment
  (Step 6 item 1) and the exit report must say so.
- **Bounded loops.** Max 3 CI-fix rounds per PR, 2 infra reruns per head,
  one remediation plan per defect class per run (dedupe: if `$QONTINUI_PLANS_DIR`
  already has a plan for this defect class, reference it instead of writing a
  twin; check before writing). **The directory is not the surface of record** — discovery
  resolves against the plan corpus (`CLAUDE.md` → "Plan corpus authority"), so also
  page `GET <web-origin>/api/v1/plan-library?kind=plan&limit=200` and match
  `slug`/title, or use `?kind=plan&work_unit_slug=<stem>` when you already have a
  stem; **never `?q=<stem>`**, which matches title and body but not the slug. Both
  checks fail open in the same direction: an unset `$QONTINUI_PLANS_DIR` (common —
  see above) skips one, and a zero-result corpus read is **UNKNOWN, not "no such
  plan"** — the corpus is a partial mirror of disk by construction. A twin
  written under an
  UNKNOWN is the expected failure here, so **say which check actually ran** in the
  exit report rather than recording the dedupe as passed.
- **Don't race coord.** An in-flight queue proposal always wins; wait.
- **Report on exit**: per-PR final state (merged-by-coord / **handed-to-operator**
  / admin-merged / fixed-and-waiting / blocked-legitimate / operator-escalated),
  defects found, plan path + vet-imp outcome. `handed-to-operator` is the state
  a diagnosed defect ends in when an agent ran this command, and it carries the
  gate id or escalation ref that makes the PR watched; `admin-merged` is
  reachable only on an operator-run pass. Distinguish it from
  `operator-escalated`, which is the closed-list `/ask-operator` question — a
  hand-off asks the operator to *act on a finished diagnosis*, not to answer
  one.
