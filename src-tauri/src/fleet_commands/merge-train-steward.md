---
description: Autonomous, checks-gated merge-train steward — runs in a visible, stoppable session, continuously watches coord's merge train + all open PRs fleet-wide, auto-remediates known wedge classes deterministically (Tier 1), and autonomously runs root-cause→author→/vet-imp→land on ANY deficiency it finds (Tier 2) — coord defects, twin retrieval gaps, neighbouring-repo bugs, or flaws in this skill itself — with NO human approval click. Runs under the same fleet policy as every other session (autonomy charter + coord-served policy documents), delegates heavy work to subagents to keep the main session a lean ledger, and escalates ONLY on the fleet's closed list.
argument-hint: "[--mode=autonomous|observe] [--repos=r1,r2] [--interval=5m] [--max-recovery-merges=1] [--threshold=45m] [--once]"
allowed-tools: Read, Write, Edit, Bash, PowerShell, Grep, Glob, Monitor, Skill, ToolSearch, TaskCreate, TaskUpdate, Agent
---

# Merge-train steward — autonomous, checks-gated, visible

This is `/babysit-prs` **generalized**: from *this session's PRs, event-triggered* to
**the whole train, fleet-wide, continuous**, plus a deterministic Tier-1 reflex table,
fleet-level rate-limits, and deploy-batch coordination. It runs as a **visible, stoppable
Claude session** — a `/loop`, or a coord `continuation_spawn` with
`presentation:"terminal"` on the operator's device — so the operator watches every step
live and can kill it any moment.

**The autonomy model is checks, not permission.** Roadmap Phase 3
(`2026-07-04-coord-merge-robustness-roadmap`) made the steward responsible not by adding
an approval click but by gating every change on **correctness checks a bad fix cannot
pass**: `/vet-plan`, `cargo`/CI, coord's candidate/speculative CI, and the no-reap gate.
A reasoning error fails those gates and never lands. If a soak shows low-quality
autonomous fixes, **tighten the gates** (raise the vet bar, lower the rate limit) — do not
remove autonomy.

**Do NOT re-derive coord state.** Phase 2 already built the honest, freshness-aware
per-PR view and the fleet metrics. Consume them; never rebuild observability. Do NOT
re-implement `/babysit-prs`'s per-PR diagnosis — **call or fork it** (this skill is that
loop, fleet-wide).

⚠️ **Verify the read tools exist before keying logic on them — and distinguish ABSENT from
UNREACHABLE, they have different fixes.** As of 2026-07-23 the live coord-mcp registry
(45 tools) has **no `coord_pr_merge_verdict` and no `coord_is_merge_safe`**; both are named
throughout this doc. Re-measured 2026-08-06, the two are NOT the same case:
`coord_is_merge_safe` is genuinely absent, but **`coord_pr_merge_verdict` EXISTS and its
schema resolves — it is simply not on the `/coord-mcp` proxy allowlist for device/agent
sessions**, so calling it returns `-32601 COORD_MCP_PROXY_METHOD_NOT_ALLOWED`, not "unknown
tool". A schema you can *fetch* is not a tool you can *call*: resolving the schema is NOT
evidence of reachability, and reporting one as deployed on that basis is a measurement
error (made in this session). Fixing an absent tool means building it; fixing an unreachable
one means widening the allow-set — see fleet memory
`reference_coord_device_session_tool_surface_is_static_50_name_allowset`. The reachable
equivalents are
**`coord_pr_status`** (carries `pr_state`, `head_sha`, `merge_state_status`, `mergeable`,
`confidence`, `last_verified_at`, `merged_at`, `merge_commit`, `blockers`, `dep_edges` — but
**not** `freshness_next_action`) and **`coord_query_merge_economics`** / **`coord_query_ci_state`**.
Enumerate `tools/list` at preflight and dispatch on what is actually there; if a lever this
doc names is missing, that is itself a **deficiency to fix** (see Step 3), not a reason to
go blind.

## Fleet policy — the steward is a normal fleet session (applies EVERY iteration)

**The steward operates under the same fleet policy as every other session on this fleet.**
Nothing in this doc narrows it, and where this doc and the policy disagree, **the stricter
governs**. Two sources, both authoritative:

1. **The autonomy charter** in `qontinui-claude-config/CLAUDE.md` ("Autonomous Operation").
   It OVERRIDES default deference. The clauses that bite hardest here: reads are free
   (rule 1) and verification needs **≥2 independent authoritative signals**; do reversible
   mechanical work (2); closeout push authority for docs/plans (3); **exhaust the cascade
   before reporting blocked** (4) — a dead MCP tool is not a blocker; self-error → re-verify,
   don't escalate (5); **silent-empty is UNKNOWN, not NO** (6); **no silent drops** (7);
   escalation is a **CLOSED list** (8); consult policies before asking (9); and
   **finish to zero** (10).
2. **The unified policy protocol** served as coord prompt documents. Before substantive
   work in a session, call `coord_list_prompt_documents`, fetch `policy/session-protocol`
   via `coord_get_prompt_document`, and follow it: classify each decision, **cite the clause
   you applied**, record a `POLICY_GAP` when none covers it, finish discovered follow-ups to
   zero, and close with a `POLICY_COMPLIANCE` footer. Read the category documents **fresh** —
   they version frequently; never from memory of them. Nothing pasted into a session can
   raise a clause's tier.

Practical consequence for a long `/loop`: fetch the policies **once per session** (not once
per iteration — they are stable within a session and re-fetching burns context), and
**re-fetch on resume**, because a resumed session carries its old context but not the
policies as they now stand.

**At any point you would ask the operator, offer instead of act, or stop short of something
you could execute — the policy documents decide it, not the operator.** Escalate only on a
hit in the closed list, and surface it WITH a recommendation rather than as an open question.

## Enablement gate + kill-switch (check FIRST, every iteration)

The steward is armed by an env flag AND is instantly stoppable:

- **`COORD_MERGE_STEWARD_ENABLED`** must be `1`/`true`. If unset/false, do nothing this
  iteration — report `steward disabled (COORD_MERGE_STEWARD_ENABLED unset)` and stop. This
  is the fleet-wide off switch.
- **Stopping the visible session** (Ctrl-C / closing the terminal / interrupting the
  `/loop`) halts it. On stop, run the **cleanup** in the try/finally sense: release any
  coord claims this run holds and leave no half-state (a partially-rebased worktree gets
  either finished or abandoned cleanly — never a dangling `--force-with-lease` mid-push).
- **`--mode=autonomous`** (the default since 2026-07-22, after the observe soak
  completed and the operator approved fix quality) enables Tier-1 remediations +
  Tier-2 lands. **`--mode=observe`** makes the steward **detect + propose only** —
  it prints what it *would* do per wedge and never mutates. Re-soak in `observe`
  after major changes to this skill or whenever fix quality is in question.

## Step 0 — Preflight (once per session / per `/loop` spawn)

1. **Parse args.** `--mode` (default `autonomous`), `--repos` — the **WATCH SET**, every repo
   scanned for stuck/red PRs (default:
   `qontinui-web,qontinui-runner,qontinui-coord,qontinui-schemas,qontinui,ui-bridge,qontinui-claude-config`),
   `--interval` (default `5m`, the poll cadence for the continuous loop), `--max-fix-prs`
   (**default: unlimited for non-coord repos**; coord fix-lands are drain-gated instead of
   rate-capped — see Guardrails), `--max-recovery-merges` (default **1**/hour), `--threshold`
   (default `45m` — how long a fully-green PR may sit before the stuck-PR reflex fires;
   **⚠️ MUST be per-repo, derived from measured candidate-CI duration — a fixed 45m
   FALSE-FIRES the stuck reflex on long-CI repos.** qontinui-runner candidate CI runs
   **~1h45m–2h** and lands serialize FIFO (no-reap gate) → effective throughput **~1 land / 2h**,
   so a green runner PR legitimately sits **2h+**: NORMAL, not a wedge. Use
   `max(45m, ~2×p90 candidate-CI for the repo)`; treat runner as ≥`3h`. Read the duration from
   `gh run list --branch 'merge-candidate/*'` (coord measures no CI duration today). See
   the `2026-07-17-merge-train-long-ci-redesign` plan),
   `--once` (single pass, no continuous loop).

   ⚠️ **The watch set is a SUPERSET of the merge-authority set — do not conflate them.** The
   **merge-authority set** is the repos coord lands:
   `qontinui-web,qontinui-runner,qontinui-coord,qontinui-schemas,qontinui,ui-bridge` (unchanged).
   Only there do the land-related Tier-1 reflexes apply — recovery-merge, land-queue timing,
   the coord drain gate, candidate-CI thresholds. Everything else in the watch set is
   **watch-only**: the steward scans it for stuck and red PRs exactly as it does the rest, but
   its remedy STOPS at making the PR landable and green (**re-run the failed run**, rebase a
   stale merge ref, fix red CI, or close an already-landed empty diff) — the land itself is
   someone else's mechanism. **This is a change of remedy, not a lowering of the stuck bar:**
   non-draft + green + unlanded past the threshold is still a wedge, it is just a wedge in
   *that repo's own* land mechanism rather than in coord's, so diagnose the mechanism instead
   of waiting for a coord proposal that will never come.
   `qontinui-claude-config` is watch-only today, and its mechanism is checked in —
   `.github/workflows/auto-merge.yml`, whose own header says it "is NOT coord-managed" (coord's
   authority covers the app repos with `strict_required_status_checks_policy` branches;
   **this repo has none**). It merges with the default `GITHUB_TOKEN`, so its lands are
   credited to `github-actions[bot]`, never to `app/qontinui-merge-orchestrator`.
   ⚠️ It is **edge-triggered** — it fires
   only on a `lint-frontmatter` `workflow_run` *completing* with `conclusion == 'success'` and
   `event == 'pull_request'`, and nothing re-fires it on a schedule. A cancelled or failed lint
   run therefore strands the PR indefinitely, and only `rerun_failed_jobs` re-arms it: a fresh
   `gh workflow run --ref` dispatch produces `event == 'workflow_dispatch'`, fails that `if:`,
   and **cannot land the PR** no matter how green it goes.

   **Why it is in the watch set at all: a steward that excludes the repo holding its own
   tooling cannot see fixes to itself.** `qontinui-claude-config` holds this command,
   `/coord-revive`, and the lint guards. While it sat outside the default set, a full steward
   session scanned the fleet repeatedly and never looked at it — #231 and #233 sat non-draft
   for **7 days** and were found only incidentally. They stalled for **DIFFERENT** reasons,
   which is the lesson: #231's sole CI job ended `cancelled` (2026-08-06, rolling its run up to
   `failure`), stranding the edge-trigger until a re-run cleared it; **#233 was a live fix to
   THIS file**, held by a genuine `lint-frontmatter` failure on a week-old merge ref and needing
   a rebase onto a main that had since fixed the violation. And by the time #231 landed, **its
   diff was empty** — its content had shipped three days earlier as #255 (`a5f94b6`), making it
   the *already-landed empty-diff* class below, whose remedy is prove-and-close, not
   re-run-and-land. Re-derive every PR's disposition from its CURRENT head: a shared symptom
   (both stuck, same repo, same week) implied neither a shared cause nor a shared remedy.
2. **Coord access.** Resolve the coord HTTP base (`$COORD_HTTP_URL`, else
   `https://coord.qontinui.io`) and confirm liveness + a single leader — sample
   `<base>/health` 4–8×; **exactly one** replica must report `is_leader:true`. No leader =
   coord outage: the whole train is down, which is the `/babysit-prs` `coord-down` class —
   escalate per Tier 3 (an outage is an operator-resource need if it needs a VPC action)
   and do NOT attempt Tier-1/2 remediations against a leaderless coord.
   **⚠️ coord-mcp is frequently DEAD from the operator box** (root `.mcp.json` points at a stale
   port and/or the on-disk nonce is evicted → every `coord_*` MCP tool returns "Command failed
   with no output"). Do NOT let this blind the steward: run **`/coord-revive`** FIRST — it is the
   cheap door-cascade triage (re-read own key → sibling-key sweep → acting-bearer) and usually
   returns a LIVE loopback door in seconds, no JWT minting needed; it also names the failure
   TYPE, so you learn whether the key is stale, the port is dead, or the credential is
   refreshing. **Any steward WRITE that returned "no output" is presumed LOST** — re-issue it
   over the reported door and verify by read (`_gate-registration` → "Dead-transport honesty").
   Only if `/coord-revive` reports DEAD, fall back to (i) coord REST/MCP-over-HTTP
   with a **minted device JWT** (`admin_secret → service-token → pair-cli(+tenant_id) → 4h JWT`)
   against `https://coord.qontinui.io/mcp` and the `/pr-merge/.../verdict` endpoints, and (ii)
   prod SQL via ECS exec (`apt-get install -y -qq postgresql-client` in the coord container, then
   base64-encoded SQL). Some operator REST levers (`/reevaluate`, merge-order) are **operator-SSO
   gated** and reject a device JWT (`401 operator context missing`) — those are Tier-3
   operator-resource actions, not steward-actionable.
3. **Rate-limit ledger.** Track `recovery_merges_this_hour = 0` on a rolling 60-minute window
   (timestamp each action; evict entries older than 60m before each check) — that cap is real
   and hard. Track `fix_prs` as a COUNT FOR THE LEDGER, not a ceiling: report how many fixes
   you authored, per repo, so the operator can see the volume without it throttling the work.
   Blast radius is bounded by per-PR review + CI + the coord drain gate, not by a quota.
4. **Deploy-cadence tracker (coord only).** Record the last coord deploy/land time and the
   in-flight proposal count. A coord restart orphans in-flight proposals, so a coord fix-land
   waits for a DRAINED queue — never ship N coord fixes into N restarts and orphan the very
   train you are fixing (Phase 1's bug; the top self-harm risk). Note this is a **land** gate,
   not an authoring gate and NOT a reason to batch fixes into one PR: coord's push-deploy
   debounce already collapses several lands into fewer deploys, at the deploy layer, without
   coupling unrelated changes. Non-coord repos have no restart cost and need no tracking.

   ⚠️ **`Deploy coord: success` is NOT evidence a change is SERVING.** The workflow has a
   spacing gate that debounces a rollout while still reporting the run green. Measured
   2026-08-24 on coord `b0a6a114`: the run's conclusion was `success`, and inside it
   `Deploy spacing gate → success ("Decide deploy vs debounce")`, a job literally named
   **`Deploy SKIPPED (spacing gate — no rollout) → success`**, and `Build, push, and roll
   coord → skipped`. Two landed commits were NOT in the serving build. **Verify by reading
   the ECS task definition's image tag, not the workflow run:**

   ```
   aws ecs describe-services --cluster qontinui-staging --services coord \
     --region us-east-1 --query 'services[0].taskDefinition' --output text
   aws ecs describe-task-definition --task-definition <that> --region us-east-1 \
     --query 'taskDefinition.containerDefinitions[*].image' --output text
   ```

   The tag is the serving sha — compare it to `origin/main`. ⚠️ The cluster is in
   **us-east-1**, even though the SSM params above are in **eu-central-1**; querying
   eu-central-1 returns `ClusterNotFoundException` and reads like an outage. This is the
   landed-vs-serving distinction the honest-bookkeeping section depends on.

## Step 1 — Fleet scan (each iteration)

Read coord's own honest view — no new observability:

- ⚠️ **READ THE OPERATOR DASHBOARD FIRST — `GET <base>/pr-merge/health`. This is REQUIRED,
  every iteration, and it is the read this skill spent months not naming.** It is the Train
  tab of `https://qontinui.io/admin/coord/fleet`, and it carries three things the per-repo
  twin reads DO NOT: `ready_unmerged` (every PR coord considers ready but has not landed,
  **with `latest_proposal_error` verbatim**), fleet-wide `slots` (occupancy vs cap, per-repo
  at-cap flags), and `pr_state_stale_backlog`.

  ⚠️ **`ready_unmerged` is an OBJECT, not an array — and misparsing it produces a
  confident FALSE "queue empty".** The shape is `{count, max_age_seconds, prs: [{repo,
  pr_number, latest_proposal_status, age_seconds, ready_since, latest_proposal_error?},
  …]}`. Iterating the WRAPPER (`foreach ($p in $r.ready_unmerged)`) walks ONE element and
  prints empty strings for every field, rendering as a single blank row that reads as
  "nothing stuck". Measured 2026-08-22: a steward reported "ready_unmerged: empty — zero
  stuck PRs" repeatedly while **11 PRs were queued, one wedged for 72 hours**. Read
  `.ready_unmerged.prs`, and **cross-check the rendered row count against
  `.ready_unmerged.count`** — if you printed fewer rows than `count`, your parse is wrong,
  not the queue. This is worse than the usual silent-empty case: a bad parse here yields a
  false NEGATIVE rather than a visible error, defeating the exact read this bullet calls
  REQUIRED.

  **Measured 2026-08-05: `coord_query_train_health` reported qontinui-web
  `is_making_progress: true` while coord had requeued a healthy PR ten times and TERMINALLY
  FAILED it (`status set to conflict + paged`).** The per-repo read cannot see that; this one
  puts it on the first screen. The operator had to point at the dashboard — the same way the
  2026-07-20 soak ended. Two consecutive stewards have now missed a terminal failure by
  reading only the per-repo surface. **Do not be the third.**

  **Transport (it is not the obvious one).** `/pr-merge/health` is TENANT-SCOPED: a device
  JWT gets `403 tenant_not_resolved`, and so does the loopback proxy. It needs a **Cognito
  operator bearer**, minted non-interactively — SSM params
  `/qontinui/cognito/coord-headless-client-id`, `/qontinui/operator/email`,
  `/qontinui/operator/password` live in **eu-central-1** (us-east-1 is empty by design), then
  `aws cognito-idp admin-initiate-auth --auth-flow ADMIN_USER_PASSWORD_AUTH` against pool
  `us-east-1_rgTB9dbZ1` in **us-east-1**; send the **IdToken** as the bearer. Keep it
  in-process (PowerShell `Invoke-RestMethod`) — never on argv, never written to disk. The
  local `:8000` proxy route the dashboard itself uses (`/api/v1/operations/pr-merge/health`)
  401s without an operator session, so it is not a shortcut.

- **Per open PR**, across the `--repos` **watch set** (Step 0 — wider than the merge-authority
  set, and deliberately so): **`coord_pr_status {repo, number}`** — the
  deployed status card. Read `pr_state`, `head_sha`, `merge_state_status`, `mergeable`,
  **`confidence`** (`fresh|stale|unknown`), **`last_verified_at`**, `merged_at`,
  `merge_commit`, `blockers`, `dep_edges`. Enumerate open PRs with
  `gh pr list --repo <owner/repo> --state open --json number,mergeStateStatus,labels,isDraft`;
  skip drafts. **Also read `changedFiles`** — a `changedFiles=0` PR is already landed
  (empty diff) and must not be treated as mergeable work.
  ⚠️ On a **watch-only** repo the twin card may be thin or absent, since coord is not landing
  there. That is UNKNOWN, not health: fall back to `gh` (`pr view`, `pr checks`) and judge the
  PR on its own CI. A repo the twin says nothing about is exactly the repo that goes unwatched
  for a week.
  ⚠️ **Dispatch note.** Older revisions of this doc keyed Tier 1 on
  `freshness_next_action` from `coord_pr_merge_verdict`. **Neither is deployed** (verified
  against the live 45-tool registry, 2026-07-23). Until a typed next-action is served, key
  Tier 1 on `merge_state_status` + `confidence` + `coord_query_merge_economics` +
  git ancestry. If you see a `next_action` field anywhere, do NOT dispatch on it — coord
  fills it with free-text prose, never a typed enum, so a detector keyed on it never fires.
  **Restoring a typed, honest per-PR next-action is itself Tier-2 work** (Step 3).
- **Fleet-level:** the CLEAN-queue depth + land cadence (`gh pr list` /
  `git log origin/main`), **`coord_query_merge_economics {repo}`** (land rate λ, candidate-CI
  p50/p90, `pressure`, CI-min per land, open proposals, `suggested_stuck_threshold_secs`),
  **`coord_query_ci_state {repo, ref|pr_number}`**, and the merge-train metrics on
  `<base>/metrics` (scrape with `--max-time 95`; scrape ×10 and take the **MAX per series**).
  ⚠️ **The reason for ×10 is CORRECTNESS, not latency.** `/metrics` is **leader-only
  rendered** and the ALB round-robins TWO replicas, so a scrape that lands on the follower
  returns **every series as `0`** — a well-formed HTTP 200 with a complete-looking body,
  indistinguishable from a real zero by shape alone. Measured 2026-08-19T23:59Z: one
  10-scrape run split **4 leader-shaped / 6 all-zero follower**; an independent 10-scrape run
  two minutes earlier split 5/5 — a coin flip, and each scrape returned promptly, so a reader who believes the
  ×10 is a *stall* mitigation drops it exactly when it matters. (The stall is also real —
  the leader render has been observed at 40–95s — but it is the SECOND reason.) Take the max
  per series: a follower's `0` can never win a max. **Never take the first successful
  response.** An all-zero `/metrics` read is **UNKNOWN, never zero** — same rule as
  `verification-and-evidence` `silent-empty-is-unknown`, and the same hazard as fleet memory
  `reference_coord_query_metric_follower_zero_is_vacuous_for_leader_gated_counters` (that
  one covers the `coord_query_metric` MCP door; this is the HTTP `/metrics` door). Cheapest
  tell on a single body: `coord_active_leaders 0` alongside an all-zero `pr_merge_*` family
  is a follower render. ⚠️ The no-reap gate `coord_is_merge_safe` named below is **not
  deployed**; infer serialization pressure from `coord_query_merge_economics.pressure`
  (≈1 ⇒ every candidate expects a mid-CI base move) until a real gate read exists.
  ⚠️ `/metrics` is **token-gated at the ALB** (HTTP 403 anonymously) —
  supply the Secrets-Manager token `qontinui/staging/coord/metrics_token` as a bearer, or the
  metrics arm silently yields nothing (the per-PR card + economics arms need no
  token). Series: `coord_proposals_resumed_after_failover_total`,
  `coord_merge_acted_on_stale_state_total`, `coord_merge_freshness_deferred_total`,
  `coord_merge_absent_acted_then_not_open_total`,
  `pr_merge_reconcile_reeval_total{reason="stale_eval_backstop"}`.
  ⚠️ That last one is a **label on a counter, not a series of its own** — there is no
  `reconcile_reeval_stale_eval_backstop` series, and a detector keyed on that bare name
  greps ZERO and silently never fires (absence reading as OK, the exact failure this
  document warns about). Verified 2026-08-19T23:52Z against production: the render carries
  `pr_merge_reconcile_reeval_total{reason=...}` over `drift` / `ready_unevaled` / `ttl` /
  `stale_cross_repo_dep` / `stale_eval_backstop`, plus the gauges
  `pr_merge_reconciler_backlog_stale` and `pr_merge_pr_state_stale_backlog`; the other four
  series above all exist as written. ⚠️ **`pr_merge_pr_state_stale_backlog` on `/metrics` is
  leader-only and will NOT match `GET /pr-merge/health`.** Measured 2026-08-20T00:00Z, same
  minute: `/metrics` (leader-shaped scrape) reported `39`, `/pr-merge/health` reported
  `pr_state_stale_backlog 18`. Both are correct — the metric's own HELP names
  `/pr-merge/health` as the cluster-consistent view, so **`/pr-merge/health` is
  authoritative** and the gap is not a defect in either. (`/pr-merge/health` needs a
  tenant-resolving principal; a device JWT gets `tenant_not_resolved` 403.)
  ⚠️ Every read in this bullet is coord-sourced, so on a **watch-only** repo it is empty BY
  CONSTRUCTION — no candidates, no proposals, λ=0, no `suggested_stuck_threshold_secs`. That is
  UNKNOWN in both directions: not a wedge, and not health. Judge a watch-only repo on its own CI
  and PR ages, with a plain wall-clock threshold — `--threshold`'s "derive it from measured
  candidate-CI duration" rule has no referent where no candidate CI exists.

- **RED MAIN — check it FIRST, every repo, every iteration. It is the highest-severity fleet
  signal and NOTHING else in this scan detects it.** A red main HOLDS the merge train: coord
  refuses to land while the base branch's CI baseline is failing. Prefer `coord.ci_baselines`
  when you have SQL; the no-SQL equivalent is the per-workflow main-run read below.
  (On a **watch-only** repo a red main holds no train — coord is not landing there — so it is
  a normal red to fix, not a fleet-severity alarm. The detector is unchanged and repo-agnostic;
  only the severity you attach to its verdict differs.)
  ⚠️ **A verdict is only meaningful next to the SHA it came from, and next to the count of runs
  it was drawn from.** Three errors live here, and all three are invisible without that
  provenance:
  - Take the **newest run** and a still-executing run (`gh` renders its `conclusion` as the
    empty string `""`, not `null` — that distinction matters below) reads as GREEN the moment a
    re-check starts — silently converting a red main into a healthy one. (Shipped and
    self-inflicted 2026-07-20: this skill's first detector had exactly that bug and cleared a
    red runner main on its first run.)
  - Take the **newest COMPLETED run** and you fail the mirror-image way: a run being **re-run**
    leaves the completed set, so the fallback lands on the PREVIOUS run — *a different, older
    sha* — which is usually green. A red main then reads green precisely while you are re-running
    it, i.e. exactly when you are watching. Hit live twice: `qontinui-coord` on 2026-07-26 (run
    `30182864642` on `1ed9e166` sat `completed/failure` while the steward printed green), and
    `qontinui-web` on 2026-07-27 at ~16:25Z, where re-running the failed E2E run on tip
    `2b4d49e3` put the same run id back `in_progress` and left the newest completed E2E verdict
    on `0dbe8270` — an older sha. It resolved green, but by luck, not by the detector working.
  - Take the runs from a **shared, windowed `gh run list`** and you fail a third way, which is
    the worst of the three because it has **no false-alarm direction at all**.
    `gh run list --limit 100` does NOT reliably return the newest 100 runs — **the window
    content is unstable between calls**. Measured 2026-07-31 on `qontinui-web`: at 08:42Z the
    detector read 7 of 10 workflows as `GREEN@1f4dfa3f` (the tip); at 10:02Z, **same tip, no
    pushes in between**, it read ALL 10 as `GREEN@8ac8b8c7 (not triggered on tip)` — and
    `8ac8b8c7` is dated **2026-07-17, two weeks earlier**. A "newest completed run" cannot move
    backwards in time. A third read minutes later returned the correct answer again
    (`Deploy web` id `283822389`, newest completed `2026-07-31T04:36:07Z 1f4dfa3f success`).
    When runs at the tip drop out of the slice, the workflow silently downgrades from **case 1
    to case 2a** — the one row this table marks *normal, do not flag*. A red main then reads as
    a benign path-filtered green, with no NOTE, no `UNKNOWN` tag, and a zero exit status.
    **The fix is the data source, not the limit:** query each workflow's OWN runs index
    (`repos/{r}/actions/workflows/{id}/runs?branch=main`), which is scoped to one
    workflow and ordered newest-first, so there is no cross-workflow window to be unstable.
    (Already a known fleet trap — memory `gh run list --limit N ≠ newest N`; the first version
    of this snippet did not honour it.)

  **The fix is not "flag every stale sha" — that cries wolf.** Workflows here are
  **path-filtered**, so a workflow legitimately having no run on the tip is the system working:
  `2b4d49e3` touched only `src/components/operations/*`, so web's backend/migration/deploy
  workflows correctly never triggered and their newest verdicts sit on an earlier commit.
  Measured 2026-07-27: **5 of web's 10 main workflows are in that state at any given moment.**
  A detector that alarms on those fires ~5 false UNKNOWNs per repo per tick and gets ignored —
  worse than the silent fallback it replaced.

  **The cases are separable from each workflow's OWN run history** — for each workflow, does a
  run exist at the tip sha, and is one of them completed? ⚠️ **Read that history per workflow
  id, NOT from a shared `gh run list` window** — the window is unstable and silently
  manufactures case 2 out of case 1; see the third failure mode above.

  **Two narrowings happen before any row below is evaluated.** First, only `push`,
  `workflow_dispatch` and `schedule` runs are ever evidence — `pull_request`, `deployment_status`,
  `dynamic` and the rest are dropped outright. Second, a workflow only **gates** main if it has a
  `push` run on main; that is coord's own `establishes_main_baseline` rule. A workflow without one
  is reported on an `advisory:` line (row 5) and can never hold the train — but that is asserted
  only on proof, never on a bounded window that merely failed to show one (rows 6-7). So rows 1-3
  describe **push runs of a gating workflow**, and "RED holds the train" means what coord means.

  | Case | Condition | Verdict | Act? |
  |---|---|---|---|
  | 0 | the workflow has **no baseline run on main** — none ever (`total_count == 0`), or none in the examined window (non-baseline events only) | `no-baseline:<workflow>` — PR-only gate, publish-on-tag, or a deploy-event-only workflow; the collapsed line labels which of the three it is | **Never.** Nothing to judge |
  | 1 | a **completed** run at the tip | `GREEN@<tip>` / `SKIPPED@<tip>` / `NEUTRAL@<tip>` / `RED(<conclusion>)@<tip>` (`+N in flight` if a newer run is running) | RED **holds the train** |
  | 2a | **no run** at the tip, last completed was green | `GREEN@<older> (not triggered on tip)` — path filters excluded it. May carry a secondary tip-run note | **Normal. Do not flag** — but read the note if one is present |
  | 2b | **no run** at the tip, last completed was **red** | `RED(<conclusion>)@<older> (not triggered on tip)`. May carry a secondary tip-run note | **A stale red still holds the train** — see below |
  | 2c | **no run** at the tip, last completed was `skipped`/`neutral` | `SKIPPED@<older>` / `NEUTRAL@<older>` `(not triggered on tip)`. May carry a secondary tip-run note | **Normal. Do not flag.** Benign, but printed |
  | 3 | a run at the tip, **none completed** | `UNKNOWN@<tip> (N in flight)` — queued, executing, or mid-re-run | **Alarm: you do not know yet** |
  | 4 | the workflow **no longer exists in the repo** | `excluded:<workflow> (no live producer — deleted from repo; last <conclusion>@<sha>)` | **Never.** Reported, never a verdict |
  | 5 | the workflow **provably has no `push` run on main** — exhaustive window, or the gating probe returned 0 | `advisory: <workflow>: <verdict>@<sha> (not main-push-triggered; …)` | **Never gates.** Printed in full and counted, because suppressing it hid a 12-day nightly failure |
  | 6 | the workflow **does** have push runs but none in the window | verdict rendered from the probe's newest push run: `<verdict>@<sha> (newest push run on main, older than the N examined; from gating probe)` | Judged normally — **RED still holds the train** |
  | 7 | the gating probe **failed or was inconclusive** | `UNKNOWN@none (cannot establish whether this workflow gates main …) [gating UNKNOWN]` | **Alarm.** Repo cannot read green |

  **Case 0 is disjoint from every other case, not a weaker case 2.** It is reachable only when the
  workflow has **no baseline run to judge** — either the runs endpoint reports `total_count == 0`,
  or every run in the examined window is a non-baseline event. In both there is no baseline run to
  be green, red, or stale, so it can never hide a *baseline* verdict; the two are labelled apart on
  the collapsed line so the second is never mistaken for the first. It exists because enumerating from the
  **workflow list** (rather than from whatever runs a window happened to return) surfaces every
  workflow, and roughly **half the fleet's 82 workflows** have no baseline run on `main` (the
  2026-07-31 census counted 50 — web 15, runner 12, coord 7, schemas 7, ui-bridge 5, qontinui 4 —
  while the read still filtered `event=push`). The baseline allow-list moves membership in **both**
  directions, so treat 50 as approximate rather than a bound: `schedule`/`workflow_dispatch`-only
  workflows leave (they now carry verdicts), while deploy-event-only ones join. Re-measured
  2026-08-04: runner 12 → 7, web 15 → 14, coord 7 → 6.
  Printing ~50 individual lines per tick would bury the ~10 that carry verdicts, so it collapses
  them onto ONE line that still names every one of them — **collapsed, never dropped**: it must
  stay visible that they were queried, or "no line" becomes indistinguishable from "not read".

  **2a and 2b are the same shape and opposite conclusions — do not collapse them.** "Not
  triggered on tip" describes the *provenance*, not the verdict. A workflow that is still in the
  repo but path-filtered off the tip can go red tomorrow, so its stale red is a **decision**
  (fix it, or retire the workflow), not something to scroll past. Equally, never collapse case
  3 into case 2 — that is the sha-fallback bug above.

  **Two different remedies clear a red main, and picking the wrong one proves nothing.** The
  `e154036b` note below says a fresh dispatch was the *wrong* move there; `qontinui-types-drift.yml`'s
  own header says a fresh dispatch is the *right* move for it. Both are correct, for different
  cases — the discriminator is **where the failing run sits**, not which tool you like:

  | Case | Correct remedy | Why |
  |---|---|---|
  | A push run went red **at the tip** (case 1) on a flake or an infra kill — `RED(cancelled)`, **or** a `RED(failure)` that classifies Tier 1/2 | `rerun_failed_jobs` on that run | a GitHub re-run **preserves `event: push`**, reuses the run id and increments `run_attempt`, so it re-adjudicates the baseline at the current sha. This is what coord's own `auto_fix_red_main` does. |
  | A push run went red at an **older** sha and the workflow is **path-filtered**, so no later commit can re-trigger it (case 2b) | `gh workflow run <wf> --ref main` | a re-run would re-run *at the stale sha* and prove nothing about the tip. The dispatch is the only way to evaluate the workflow against the tip without a noop commit. |

  ⚠️ **Row 1 is NOT reached by reading the run `conclusion`.** `cancelled` is the minority
  infra shape; most infra kills report `RED(failure)`, which at the RUN level is
  indistinguishable from a regression. What tells you row 1 applies rather than “author a fix
  PR” is the **step-level** classification below — see “The `failure`-side discriminator is
  STEP-LEVEL” under “Once red, classify before acting”. Two directions to get wrong, and row 1
  covers both: do not skip it because the token is not `cancelled`, and do not fire
  `rerun_failed_jobs` on a `RED(failure)` you have not classified at all. Tier 1/2 makes it an
  infra kill and the re-run is the remedy; a genuine failed step is re-runnable only as a
  BOUNDED flake test, and a second failure at that same step settles it as real.

  ⚠️ **The dispatch remedy does NOT clear the verdict, and that is by design.** A
  `workflow_dispatch` run never adjudicates (push-only baseline, below), so case 2b's line keeps
  its `RED(...)` token. What the dispatch buys you is *evidence*, surfaced as the secondary
  `[tip-green: …]` / `[tip-red: …]` / `[tip-other: …]` annotation described below — a human then
  adjudicates the tradeoff with both facts visible.

  **If you dispatch and no annotation appears, read the line you got before concluding anything.**
  There are four causes and only the first three mean "nothing happened":
  the dispatch did not run; it is still in flight (only **completed** runs are annotated); it did
  not land on the tip; **or the line you are looking at is not one that carries the note.** The
  annotation is suppressed on any line whose own verdict is not an adjudicated stale one — an
  `UNKNOWN` line never carries it, and neither does a line already judged at the tip. It DOES ride
  the gating-probe line (`… from gating probe`) as well as case 2, which matters because each
  dispatch you fire adds a run to the `RED_MAIN_DEPTH` window and can evict the last in-window
  push run, flipping the workflow from the case-2 path onto the probe path. Annotating only case 2
  would have made the note disappear for exactly the operator who followed this advice hardest.

  **Case 4 is why 2b needs a liveness test: a DELETED workflow's last run is immortal.** GitHub
  keeps every run of a workflow whose file has been removed, and nothing can ever supersede it —
  there is no producer left to emit a newer one. So a workflow that failed once and was then
  deleted pins the repo to `RED` **forever**. Live instance: `qontinui` printed
  `Quality Checks: RED(failure)@91db96e1 (not triggered on tip)` on **every tick for two months**
  while `gh api repos/qontinui/qontinui/actions/workflows` did not list `Quality Checks` at all —
  its file was gone. Confirmed a detector defect from the other side: coord read that same repo
  `is_making_progress: true`, `queue_depth: 0`, `last_land_at: 2026-07-23` — it **landed a PR
  there two months after the failure**, i.e. coord ignored the dead workflow entirely and nothing
  was ever blocked. Only this detector saw a red. A permanent false red is worse than no
  detector: it trains you to scroll past exactly the line that is supposed to stop the fleet.

  **Exclusion is a claim about the PRODUCER, and it is the over-reach hazard in this section —
  three rules keep it narrow:**
  - **Deleted ≠ disabled.** `repos/{r}/actions/workflows` returns everything that still exists
    and omits only `state:"deleted"` (visible if you fetch the id directly). **Presence in that
    list — in ANY state — keeps the verdict**, because every other state can produce a run again:
    `active` obviously; `disabled_manually` is one click / one API call from running and is
    routinely how a broken workflow is parked, so its red is precisely the decision you must not
    lose; `disabled_inactivity` is GitHub auto-disabling a scheduled workflow after 60 days of
    repo inactivity and self-reverses on the next push; `disabled_fork` is not disabled at all on
    the upstream repo. Only **absence** — the file is gone from the default branch — is
    unrecoverable without someone re-adding a file, and only that excludes.
  - **Key EVERYTHING on workflow ID, NEVER on name — grouping and enumeration, not just this
    exclusion filter.** A run's `.name` is the workflow's `name:` field *at the time the run
    executed*, so a workflow renamed in its YAML shows old runs under the old name and appears in
    the API under the new one. That breaks name-keyed logic in **both** directions:
    - *Split.* Name-matching an exclusion reads the rename as "deleted" and silently drops a live
      workflow's red. Not hypothetical: `qontinui-web` workflow **`283822389` ran as `Deploy web`
      and is listed as `Deploy Web Backend`** (verified 2026-07-27) — it deploys the backend on
      every push and name-matching would have excluded it. Name-*grouping* fails the same way one
      layer down: the renamed workflow lands in two groups, the pre-rename one has no run at the
      tip forever, and if its last pre-rename run failed that is an **IMMORTAL RED** — the exact
      class this exclusion logic exists to kill, re-created by the grouping that feeds it.
    - *Merge.* `.name` is not unique either — `qontinui-coord` has two distinct workflows both
      named `Secret Scan` (ids `302987119` `secret-scan.yml` and `303192306`
      `secret-scan-caller.yml`, verified 2026-07-31) — so a name group holds several ids and one
      workflow's green can mask the other's red inside it.

    So: enumerate from `.workflows[].id`, query per id, group by id, and carry the name for
    humans only. (The old note here said "keep the group if **any** id is still live, which is
    the safe direction" — that was the least-bad patch available while grouping was name-keyed.
    With id-keyed groups there are no multi-id groups left to be safe about.)
    Residual, now much smaller: renaming the workflow **file** (not the `name:` field) mints a NEW
    id and deletes the old one. The old id is genuinely dead and is correctly `excluded:` — but
    the NEW id is in the workflow list from the moment the file lands, so it is enumerated and
    printed (as `no-baseline` until its first run on main) rather than being invisible until a run
    happens to appear in a window. If a red disappears right after a `.github/workflows/` file
    move, that is why; check the new path's first run.
  - **A failed liveness read is UNKNOWN, never an exclusion and never an all-clear.** If the
    workflow list can't be fetched (or comes back truncated), the snippet prints a NOTE, tags
    every line `[producer liveness UNKNOWN]`, and **leaves every verdict standing** — a red still
    reads red. It must not assume everything is live (that restores this false positive) nor that
    anything is dead (that buries a real red). Same standing rule as everywhere else here: a
    suppressed error must never become a confident value.

  **Verdict vocabulary is explicit because `conclusion` is not binary.** Real values seen on
  main across the fleet: `success`, `failure`, `cancelled`, `skipped`, `""`. `success` is GREEN;
  `skipped` is its own benign class `SKIPPED` (below); everything else non-empty is
  `RED(<conclusion>)` so the reason travels with the alarm — `cancelled` in particular is the
  infra-cancelled class that rolls up to a workflow failure and self-heals on a re-run
  (`reference_coord_infra_cancelled_job_reds_main_holds_train`), and it
  must not be silently upcast to GREEN. An empty conclusion on a *completed* run is
  `UNKNOWN(blank conclusion)`, never blank output.

  ⚠️ **`cancelled` is NOT the only infrastructure class — most infra kills arrive as
  `failure`.** A CI job killed by a dying self-hosted runner reports `conclusion: failure`,
  indistinguishable at the RUN level from a genuine regression — so this vocabulary, which is
  derived from the run `conclusion` alone, cannot separate the two and must not be read as if it
  could. Measured 2026-08-09..2026-08-20 fleet-wide: **74 infrastructure-killed jobs across 62
  workflow runs** (63 self-hosted, 11 GitHub-hosted); **14** were `CI` on `qontinui-coord` `main`,
  i.e. train-holding; **20 have since re-run to success with ZERO bytes changed**. The detector
  below is correct as written and stays that way — it reports `RED(failure)`, which is true.
  What this adds is a **classification step the steward applies AFTER the detector reports a
  RED**, before choosing a remedy: see “Once red, classify before acting”.

  ⚠️ **`skipped` is NOT red, and it is NOT green either.** A workflow-level `skipped` means every
  job was skipped by a conditional — the workflow working as designed. It prints as
  `SKIPPED@<sha>`, is adjudicated (so it never withholds a verdict or forces a re-read), and does
  **not hold the train**. Do not fold it into GREEN: it did not pass, it did not run, and
  conflating the two is the same category error as calling it red, one direction over. Measured
  2026-08-04 on `qontinui-web` `Verify Frontend Deploy` (id `285385598`): **100 of its newest 100
  runs on `main` are `completed/skipped`** — treating that as `RED(skipped)` pins the repo
  permanently red. `cancelled` is unaffected and stays RED.
  ```bash
  # Call once per repo, handing it the repo in the NAMED variable RM_REPO:
  #   RM_REPO=qontinui/qontinui-web red_main
  # NOT as a positional parameter, and never convert it back into one. In a slash-command
  # markdown body a dollar sign followed by a single digit is a HARNESS ARGUMENT PLACEHOLDER,
  # not a shell positional: Claude Code substitutes the invocation's argument words into this
  # body BEFORE injecting it into the session, indexed from ZERO (the zeroth placeholder is the
  # FIRST word), and leaves unfilled positions LITERAL. Measured 2026-08-13: invoking
  # `/merge-train-steward continuous. fix red CI when appropriate. ...` rewrote this header to
  # `local r="fix"` and every per-workflow URL to garbage, and the detector reported
  # 29 queried / 29 failed / every line UNKNOWN on every repo. Named variables are not
  # substituted. (This comment deliberately spells no dollar-digit of its own — a literal one
  # here would be substituted too, garbling the warning.)
  red_main() {
  local r="${RM_REPO:-}" tip tip2 wf live win d qn qfail unadj adv nb nobase note id state name cls line probes gp gprun
  local PAR="${RED_MAIN_PARALLEL:-12}" DEPTH="${RED_MAIN_DEPTH:-10}"
  # Stated separately from the tip read below so an unset RM_REPO names its OWN cause. Folding
  # it into the tip check would surface a calling-convention mistake as "cannot resolve tip of
  # main (wrong default branch? auth?)" — a confident, wrong diagnosis. The `:-` default above
  # is what keeps this message REACHABLE under a `set -u` caller, which would otherwise abort on
  # the unset read one line earlier and print nothing at all.
  [ -n "$r" ] || { echo "UNKNOWN — red_main reads its repo from the named variable RM_REPO (call it as: RM_REPO=owner/repo red_main); RM_REPO was empty or unset, so NOTHING was read and no verdict is implied"; return 1; }
  tip=$(gh api "repos/$r/commits/main" --jq .sha) || tip=""
  # HARD PRECONDITION. An empty $tip makes every head_sha comparison fail, so cases 1
  # and 3 become UNREACHABLE and the repo silently degrades to all-stale verdicts.
  [ -n "$tip" ] || { echo "$r: UNKNOWN — cannot resolve tip of 'main' (wrong default branch? auth?); verdict withheld"; return 1; }

  # LIVE PRODUCERS — and now also the ENUMERATION source, so a workflow is judged even
  # when it has NO runs in any window. A workflow DELETED from the repo keeps its last run
  # forever and nothing can ever supersede it, so a stale red is IMMORTAL. This endpoint
  # omits deleted workflows and returns every one that still exists, INCLUDING every
  # disabled_* state — those keep their verdict (see the state rules above).
  # `null` here means "cannot tell", and must never collapse into an exclusion.
  wf=$(gh api "repos/$r/actions/workflows?per_page=100") || wf=""
  if [ -n "$wf" ]; then
    # An EMPTY list is the trap: {"total_count":0,"workflows":[]} is a legitimate 200 body,
    # it passes the truncation check, and `[]` then reads as "every workflow is deleted" —
    # burying every red on the repo with no NOTE and no UNKNOWN tag. It goes to null because
    # an empty list is INDISTINGUISHABLE from a degenerate read, so it must fail toward
    # UNKNOWN. (Not because such a repo has nothing to judge — a repo whose workflows were
    # all deleted has an empty list AND runs to account for. Do not relax this on that reading.)
    live=$(printf '%s' "$wf" | jq -c '
      if (.total_count == null) or ((.workflows|length) == 0)
         or ((.workflows|length) < .total_count)
      then null else [.workflows[] | {id, name}] end') || live=null
  else live=null; fi
  [ -n "$live" ] || live=null

  # DISCOVERY ONLY — never a verdict source WHEN THE WORKFLOW LIST IS USABLE. The shared
  # cross-workflow run window is UNSTABLE between calls: measured 2026-07-31, two reads of
  # the same repo at the SAME tip 80 min apart returned disjoint slices, the second dated two
  # weeks earlier, and a third read minutes later returned the correct newest runs again. So
  # while `$live != null` this call does exactly one job — learning the ids of workflows that
  # HAVE runs but are ABSENT from the workflow list, i.e. deleted producers that still own an
  # immortal last run — and a degraded read can then only drop an `excluded:` accounting line,
  # which carries no verdict. ⚠️ When `$live == null` the fallback below enumerates from this
  # window ALONE, so the printed set is a DEGRADED INVENTORY drawn from the distrusted source,
  # not an account of the repo: a workflow missing from the window is missing from the output
  # entirely, with no line and no count. That path is why `$live == null` prints the NOTE,
  # tags every line, and returns non-zero — it is never an all-clear.
  # No `--event push` here either, deliberately matched to the authoritative read below: a
  # deleted producer whose only main runs were workflow_dispatch or schedule would otherwise be
  # invisible to discovery and never get its `excluded:` accounting line — and on the
  # `$live == null` path, where this window IS the enumeration, a narrower filter narrows the
  # already-degraded inventory further. A non-baseline run leaking in here is harmless: this call
  # only harvests workflow IDs, which are the repo's own either way, and every ID is then judged
  # by the per-id read above, which applies the baseline allow-list. Keeping this call UNfiltered
  # is what makes it a superset of the ids the allow-list will judge.
  win=$(gh run list --repo "$r" --branch main --limit 100 \
          --json workflowDatabaseId,name) || win=""
  note=""
  [ -n "$win" ] || note="  NOTE: deleted-workflow discovery read failed — an already-deleted workflow may be missing from the excluded: lines below; no verdict is affected while producer liveness is known"

  d=$(mktemp -d) || { echo "$r: UNKNOWN — mktemp failed; verdict withheld"; return 1; }

  # Query set = live ids UNION ids seen in the discovery window. ONE enumeration, ONE read
  # path. `state` per id: live / dead / unknown (workflow list unusable). Grouping is by ID,
  # never by name: `.name` is the workflow's `name:` AT RUN TIME, so a renamed workflow
  # splits into two name-groups (the pre-rename one is then permanently stale and, if its
  # last run failed, an IMMORTAL RED), and `.name` is not unique either (coord has two
  # distinct workflows both named `Secret Scan`), so name-grouping also MERGES distinct
  # workflows. Both directions are wrong; the id is wrong in neither.
  jq -r -n --argjson live "$live" --argjson win "${win:-[]}" '
      (if $live == null then null else ($live | map(.id)) end) as $liveids
    | ((($live // []) | map({id, name, src:"live"}))
       + ($win | map({id: .workflowDatabaseId, name, src:"win"})))
    | map(select(.id != null and .id != 0))
    | group_by(.id)
    | map(. as $g
          | { id:    $g[0].id,
              name:  ((($g | map(select(.src == "live")) | first) // $g[0]).name // "?"),
              state: (if $liveids == null then "unknown"
                      elif ($liveids | index($g[0].id)) != null then "live"
                      else "dead" end) })
    | sort_by(.name | ascii_downcase)
    | .[] | "\(.id)\t\(.state)\t\(.name)"' > "$d/query.tsv" \
    || { rm -rf "$d"; echo "$r: UNKNOWN — could not build the workflow query set; verdict withheld"; return 1; }

  qn=$(wc -l < "$d/query.tsv" | tr -d ' \t')
  [ -n "$qn" ] && [ "$qn" -gt 0 ] 2>/dev/null \
    || { rm -rf "$d"; echo "$r: UNKNOWN — no workflows to query (workflow list unusable AND no runs discovered); verdict withheld"; return 1; }

  # AUTHORITATIVE READ — one call per workflow, against that workflow's OWN runs index,
  # newest-first. This is the whole point of the rewrite: `gh run list` slices a shared,
  # cross-workflow window that is unstable between calls, so runs AT THE TIP can drop out of
  # the slice and the workflow then renders as case 2a — `GREEN@<older sha> (not triggered on
  # tip)`, the ONE branch documented above as normal-do-not-flag. A red main reads as a benign
  # path-filtered green, silently. Scoping each query to a single workflow removes the window.
  # The helper takes the repo, depth, temp dir and workflow id through NAMED ENVIRONMENT
  # VARIABLES. It must NEVER take them as positional parameters: a dollar sign followed by a
  # single digit anywhere in this file is a harness argument placeholder that is substituted at
  # injection time (see the note on the function header above), and the positional form this
  # helper used to carry is exactly what made the generated script read a garbage repo, depth
  # and output path. Passing the id through `env` rather than interpolating it into the
  # generated shell string additionally keeps it out of any shell parse.
  # Do NOT rewrite this as a heredoc: this snippet lives indented inside a markdown fence, and
  # a heredoc terminator must sit at column 0 — an indented terminator swallows the rest of the
  # function and the whole detector fails to parse. (Unchanged, and independent of the above:
  # the `printf` form is what keeps the heredoc hazard out.)
  # NO `event=` filter on the URL — deliberately. `event=push` hid a re-run DISPATCHED at the
  # same sha (the NEWER, authoritative verdict) and manufactured a FALSE RED. But the URL cannot
  # express the RIGHT filter either: the runs endpoint takes exactly ONE `event=` value, and the
  # baseline set is three (`push`, `workflow_dispatch`, `schedule`). So the URL stays open and the
  # allow-list is applied CLIENT-SIDE in the jq below. Do not "tidy" it back into the URL.
  # `branch=main` still does real work here — it excludes merge-candidate runs (head_branch is
  # merge-candidate/*) — but it is NOT sufficient alone. See the notes after the snippet.
  printf '%s\n' 'gh api "repos/$RM_REPO/actions/workflows/$RM_WF/runs?branch=main&per_page=$RM_DEPTH" > "$RM_DIR/$RM_WF.json" 2>/dev/null || : > "$RM_DIR/$RM_WF.fail"' > "$d/fetch.sh"
  # Workflow ids come from the GitHub API and are integers. That is VALIDATED here, once, before
  # either fan-out, because the id is interpolated into a URL and into a temp filename and a
  # non-integer would corrupt both. A rejected id is NOT dropped from the report: it simply
  # never gets a `.json`, so it lands on the per-workflow read-FAILED path below and renders
  # UNKNOWN with a withheld verdict. Fail-closed, which is the only acceptable direction here.
  # `|| true` for the same reason as the fan-out below: `grep` exits 1 when it matches nothing,
  # and a `set -e` caller would abort HERE, before the header prints. The redirect still creates
  # an empty `ids.txt`, so a zero-match read fans out over nothing and every workflow lands on
  # the read-FAILED path — an all-UNKNOWN repo returning non-zero, never a silent green.
  cut -f1 "$d/query.tsv" | grep -E '^[0-9]+$' > "$d/ids.txt" || true
  # `|| true`: xargs exits 123 if any child failed, and a `set -e` caller would abort HERE,
  # before the header prints — a tick that prints nothing, which must never happen. Child
  # failures are already recorded as `.fail` marker files and surface per workflow below.
  # `-I{}` consumes one id per invocation (implying the one-line-per-command behaviour the old
  # `-n1` gave) and `-P` still fans out. The replacement lands in an `env` ASSIGNMENT, never in
  # a shell string, so no id is ever parsed by a shell — `env` is exec'd directly, so a hostile
  # id could not reach a shell even if the numeric validation above were removed. Two changes
  # from `-n1` worth naming: `-I` also implies no-run-if-empty, so an empty `ids.txt` now runs
  # NOTHING where `-n1` ran the helper once with no id and dropped a spurious `.fail`; and the
  # replacement applies to EVERY initial argument, so never let a brace pair reach `$r` or `$d`.
  xargs -P "$PAR" -I{} env RM_REPO="$r" RM_DEPTH="$DEPTH" RM_DIR="$d" RM_WF={} sh "$d/fetch.sh" < "$d/ids.txt" || true

  # GATING PROBE — second, much smaller fan-out. Whether a workflow gates main is "does it have
  # a push run on main", and the bounded window answers that definitively ONLY when it covered
  # the whole branch=main history. When it did not, the push runs may simply be older than
  # DEPTH: web Backend CI has 26 push runs of 86 with the first at index 59, past any sane
  # depth. Calling those advisory would silently downgrade a workflow that really does gate —
  # absence reading as OK, the one thing this section forbids. So each AMBIGUOUS workflow (zero
  # push runs in the window AND a non-exhaustive window) gets ONE authoritative call whose
  # total_count settles it. Only ambiguous ids pay: ~15 fleet-wide against a ~106-call baseline.
  # No process substitution here on purpose: a plain file keeps this readable under `sh` too.
  : > "$d/ambig.txt"
  while IFS= read -r id; do
    [ -n "$id" ] && [ -s "$d/$id.json" ] || continue
    jq -e '((.workflow_runs // []) | map(select(.event == "push")) | length) == 0
           and ((.total_count == null) or (((.workflow_runs // []) | length) < .total_count))' \
       < "$d/$id.json" >/dev/null 2>&1 && printf '%s\n' "$id" >> "$d/ambig.txt"
  done < "$d/ids.txt"
  probes=$(wc -l < "$d/ambig.txt" | tr -d ' \t'); probes=${probes:-0}
  # per_page=1 — we want total_count, not the runs. Same NAMED-VARIABLE discipline and the same
  # no-heredoc rule as the fetch helper above: no positional parameters here either, because a
  # dollar sign followed by a single digit in this file is harness-substituted at injection
  # time. This site fails QUIETER than the fetch fan-out and so is the one most likely to be
  # left broken — a broken probe only writes `.pushfail`, which routes its ids to
  # `gating_unknown` and then UNKNOWN, degrading a minority of workflows in a way no footer
  # count makes obvious. The ids here are the already-validated integers from `ids.txt`.
  printf '%s\n' 'gh api "repos/$RM_REPO/actions/workflows/$RM_WF/runs?branch=main&event=push&per_page=1" > "$RM_DIR/$RM_WF.push.json" 2>/dev/null || : > "$RM_DIR/$RM_WF.pushfail"' > "$d/probe.sh"
  [ "$probes" -eq 0 ] || xargs -P "$PAR" -I{} env RM_REPO="$r" RM_DIR="$d" RM_WF={} sh "$d/probe.sh" < "$d/ambig.txt" || true

  # Main can advance while we read; then the "tip" we label is stale and runs on the real
  # tip are invisible. This re-read sits AFTER the fan-out deliberately, so it brackets every
  # per-workflow read — re-reading before them would leave the reads unbracketed.
  tip2=$(gh api "repos/$r/commits/main" --jq .sha) || tip2=""
  [ "$tip" = "$tip2" ] || { rm -rf "$d"; echo "$r: UNKNOWN — main moved mid-read (${tip:0:8} -> ${tip2:0:8}); verdict withheld, re-read next tick"; return 1; }

  echo "== $r tip=${tip:0:8} — $qn workflow(s), per-workflow authoritative read (depth=$DEPTH, parallel=$PAR)"
  [ -z "$note" ] || echo "$note"
  [ "$live" != "null" ] || echo "  NOTE: workflow list unavailable or truncated — producer liveness UNKNOWN for every line below, AND the workflow inventory itself fell back to the distrusted run window, so a workflow missing from that window is missing from this report entirely. A RED here may be a deleted workflow's immortal last run; a green repo verdict is NOT supported and this repo returns non-zero."

  qfail=0; unadj=0; adv=0; nb=0; nobase=""
  while IFS=$(printf '\t') read -r id state name; do
    [ -n "$id" ] || continue
    if [ -f "$d/$id.fail" ] || [ ! -s "$d/$id.json" ]; then
      qfail=$((qfail+1))
      echo "  $name: UNKNOWN@none (per-workflow runs read FAILED for id $id — verdict withheld)$([ "$state" = unknown ] && printf ' [producer liveness UNKNOWN]')"
      continue
    fi
    # jq emits `<class>\t<text>`, never a formatted line the shell has to re-parse. The class
    # is OUT OF BAND on purpose: classifying by matching the rendered text would let a
    # workflow NAMED `no-baseline` route its own RED into the collapsed nothing-to-judge line.
    # ADJ = a definite verdict (green, red, or benign SKIPPED/NEUTRAL). UNADJ = we do not know.
    # NB = no baseline-event run on main to judge. ADV = out-of-band only, never gates.
    # `[read N/total]` on every judged line is the REGRESSION GUARD for this defect: it states
    # how many runs the verdict rests on, so a truncated or degenerate read is visible in the
    # output instead of silently narrowing the evidence.
    #
    # $gp is the gating probe verdict for this id. `absent` = not ambiguous, so the window
    # already settles it. A probe that failed or returned nonsense is `unknown` and must NOT
    # resolve to advisory — that is the silent-downgrade this probe exists to prevent.
    # `per_page=1` does not only answer "does it gate" — it RETURNS that newest push run, so
    # when the window held no push runs we still get an authoritative baseline verdict out of
    # the same call instead of a permanent UNKNOWN. $gprun is that run, or null.
    gp=absent; gprun=null
    if [ -f "$d/$id.pushfail" ]; then gp=unknown
    elif [ -s "$d/$id.push.json" ]; then
      gp=$(jq -r 'if (.total_count // -1) < 0 then "unknown"
                  elif .total_count > 0 then "gating" else "nongating" end' < "$d/$id.push.json" 2>/dev/null) || gp=unknown
      [ -n "$gp" ] || gp=unknown
      # PROJECT to the three fields the program below actually reads, never the whole run object.
      # This value is passed as --argjson on the jq COMMAND LINE, and on Windows the whole argv is
      # capped near 32KB — shared with the jq program itself. A raw GitHub run object is ~16KB, so
      # a full one spent HALF the budget: measured 2026-08-13, web `Cross-browser Survey` had a
      # 16712-byte gprun against a 12178-byte program (~88% of the cap), and adding ~3KB of jq
      # comments tipped it to `Argument list too long` — the workflow silently degraded from an
      # adjudicated GREEN to `UNKNOWN@none (jq failed …)`. It fails CLOSED, but it is triggered by
      # an UNRELATED edit and by run-object size, so it reads as a random regression. Projecting
      # makes it ~100 bytes and is behaviour-identical: status, conclusion, head_sha and the
      # null-ness are the only things read (see the gates_no_evidence branch).
      gprun=$(jq -c '((.workflow_runs // [])[0] // null) | if . == null then null else {status,conclusion,head_sha} end' < "$d/$id.push.json" 2>/dev/null) || gprun=null
      [ -n "$gprun" ] || gprun=null
    fi
    line=$(jq -r --arg tip "$tip" --arg w "$name" --arg state "$state" --arg probe "$gp" --argjson gprun "$gprun" '
        # success | neutral | skipped are the three PASSING conclusions, matching coord
        # is_passing_conclusion (ci_baseline.rs). neutral and skipped keep their own labels
        # rather than being upcast to GREEN — they did not pass, they declined to run — but
        # neither reds the train, because coord will merge straight through both.
        def verdict(c):
          if c == "success" then "GREEN"
          elif c == "skipped" then "SKIPPED"
          elif c == "neutral" then "NEUTRAL"
          elif (c // "") == "" then "UNKNOWN(blank conclusion)"
          else "RED(\(c))" end;
        def cls(c): if (c // "") == "" then "UNADJ" else "ADJ" end;
        # NOTE: this jq program is inside a SINGLE-QUOTED shell string — no apostrophes below.
        # TWO decisions here, and they are deliberately separate. The URL cannot make either:
        # the runs endpoint accepts exactly ONE event= value, so both are client-side.
        #
        # (1) CANDIDATE EVENTS. Only push/workflow_dispatch/schedule are ever evidence.
        #     deployment_status and dynamic are NOT per-commit verdicts (web Verify Frontend
        #     Deploy is 100/100 deployment_status; qontinui Graph Update is dynamic and often
        #     failure), and branch=main does NOT exclude fork PRs (it matches head_branch).
        #
        # (2) DOES THIS WORKFLOW GATE MAIN AT ALL? A workflow can only hold main if it actually
        #     runs on pushes to main. If it has >=1 push run here it is MAIN-TRIGGERED and its
        #     verdict comes from PUSH RUNS ONLY. Otherwise it is ADVISORY: reported in full,
        #     never gating.
        #
        # Why push-only for the verdict, rather than newest-at-tip regardless of event: a
        # dispatch/schedule run of the SAME workflow does not run the same jobs. coord ci.yml
        # gates clippy-nightly-unscoped on schedule/dispatch and deliberately omits -D warnings
        # ("a false-red here costs nothing"); at tip 40172d56 the push run passed every gating
        # job while the dispatch run failed on exactly that one. Letting it adjudicate turns a
        # deliberately non-gating job into a train-holder. It launders the other way too:
        # deploy-web rollback dispatches skip build+test and still conclude success, and coord
        # deploy has a canary input documented as EXPECTED to end RED. Push is the only
        # un-parameterised baseline, so it is the only authoritative one.
        ["push","workflow_dispatch","schedule"] as $BASELINE
      | (.workflow_runs // []) as $all
      | ($all | map(select(.event as $e | $BASELINE | index($e)))) as $cand
      | ($cand | map(select(.event == "push"))) as $pushes
      | ($all | length) as $raw0
      | (.total_count // -1) as $tot0
        # A MISSING total_count is not evidence of exhaustion. Defaulting it to -1 would make
        # raw >= tot trivially true and assert advisory — proven-non-gating — off a degenerate
        # body. It has to fail toward the probe instead, which is why the shell ambiguity
        # predicate above also treats a null total_count as ambiguous. The two must agree.
      | (($tot0 >= 0) and ($raw0 >= $tot0)) as $exhaustive
        # FIVE outcomes, and the two "we cannot tell" ones are kept apart from the two we can.
        #   push_in_window     — has push runs here; judge them. The only gating verdict path.
        #   gates_no_evidence  — the probe proved push runs EXIST but all are older than DEPTH.
        #                        It gates, and we have no in-window push evidence: UNKNOWN.
        #   gating_unknown     — the probe failed. Never resolves to advisory.
        #   advisory           — proven non-push-triggered (exhaustive window, or probe said 0).
      | (if ($pushes | length) > 0 then "push_in_window"
         elif $probe == "gating" then "gates_no_evidence"
         elif $probe == "unknown" then "gating_unknown"
         elif $probe == "nongating" then "advisory"
         elif $exhaustive then "advisory"
         else "gating_unknown" end) as $mode
      | ($mode == "push_in_window") as $gates
      | (if $gates then $pushes else $cand end) as $R
      | (if $mode == "advisory" then "advisory: " else "" end) as $adv
        # Advisory is only ever asserted on PROOF, never on absence of evidence. Either the
        # window was exhaustive (we saw every branch=main run and none was a push), or the
        # gating probe returned total_count == 0 for event=push. A truncated window on its own
        # NEVER lands here — that path goes to the probe instead.
      | (if $mode == "advisory"
         then (if $exhaustive then " (not main-push-triggered; out-of-band events only)"
               else " (not main-push-triggered; confirmed by push probe: 0 push runs on main)" end)
         else "" end) as $advwhy
      | ($R | length) as $n
      | ($all | length) as $raw
      | (.total_count // -1) as $tot
        # "on main" is NOT decoration. $tot is total_count for the branch=main QUERY, not the
        # workflow lifetime, and reading it as lifetime produced a wrong hypothesis on
        # 2026-08-04: runner Release printed [read 2/2] and schema.pg.sql.generated freshness
        # [read 1/1], which read as "may have NEVER succeeded" when the real histories are 24
        # and 3382 runs. The scope has to travel with the number.
      | " [read \($n)/\($tot) on main\(if $raw > $n then ", +\($raw - $n) non-baseline dropped" else "" end)]" as $depth
      | (if $state == "unknown" then " [producer liveness UNKNOWN]" else "" end) as $pq
      | [$R[] | select(.head_sha == $tip)] as $attip
      | ([$attip[] | select(.status == "completed")] | sort_by(.created_at) | last) as $tipDone
      | ([$attip[] | select(.status != "completed")] | length) as $inflight
      | (if $inflight > 0 then " +\($inflight) in flight" else "" end) as $busy
      | ([$R[]  | select(.status == "completed")] | sort_by(.created_at) | last) as $lastDone
        # OBSERVATION ONLY, never a verdict — the tip-run annotation. A completed NON-push
        # baseline run at the tip is read, admitted to $cand, then dropped from $R by the
        # push-only rule; the drop is CORRECT and stays, the SILENCE about it was the defect.
        # Rules, rationale and the measured instance: see the tip-run bullet in the notes below.
        # Completed-only and sort_by(.created_at)|last, so an in-flight or re-running dispatch is
        # never read as a conclusion. Built unconditionally here and GATED AT EACH USE SITE, so
        # it can only ever decorate a line whose own verdict is ADJUDICATED and stale — never an
        # UNKNOWN, never a line already judged at the tip.
      | ([$cand[] | select((.event != "push") and (.head_sha == $tip) and (.status == "completed"))]
         | sort_by(.created_at) | last) as $tipAlt
        # SYMMETRIC BY CONSTRUCTION: the label comes from the run conclusion, so tip-red prints as
        # loudly as tip-green. Lower-cased and bracketed so it cannot be mistaken for the verdict.
      | (if $tipAlt == null then "" else
           (($tipAlt.conclusion // "") as $tc
            | (if $tc == "success" then "tip-green"
               elif ($tc == "") or ((["skipped","neutral"] | index($tc)) != null) then "tip-other"
               else "tip-red" end) as $tlab
            | " [\($tlab): \($tipAlt.event) \(if $tc == "" then "blank conclusion" else $tc end)@\($tip[0:8]) — observed, not adjudicating]")
         end) as $tipnote
        # $tot is the API total_count and is UNFILTERED, so it cannot distinguish "no baseline
        # run ever" from "no baseline run in this window". Both are NB: nothing to judge, never
        # act. They are LABELLED apart rather than merged, so the collapsed line still says which
        # is which. Routing the second to UNADJ instead would pin any repo owning a
        # deployment_status-only workflow to a permanent UNKNOWN — web has one (Verify Frontend
        # Deploy, 100/100 deployment_status), so that repo could never report green again. The
        # residual is stated in the notes: a baseline run older than $raw non-baseline runs is
        # not examined, so an ancient stale red behind them is not surfaced.
        # These two come FIRST because they are statements about whether the workflow gates at
        # all, which outranks any verdict computed from the runs we happen to hold. Both are
        # UNADJ, so the repo cannot read green while either is present.
        # The probe RETURNED the newest push run, so prefer a real verdict over an UNKNOWN. It
        # is authoritative (newest push run on main, straight from the API). It is USUALLY older
        # than the window, but not necessarily: the window is the newest DEPTH runs of ANY event,
        # so a push run at the tip can be crowded out by newer non-push runs.
        # Only when the probe gave no usable run does this stay UNKNOWN.
        #
        # BOTH gating branches carry the dead-producer guard, and it is NOT optional. A workflow
        # DELETED from the repo keeps its last run forever, so its red is IMMORTAL — that is the
        # entire reason the excluded: branch below exists. The probe reaches PAST the window
        # straight into that frozen history, which makes it the most effective possible way to
        # resurrect such a red. Without this guard a deleted workflow with an ambiguous window
        # renders RED(...) and HOLDS THE MERGE TRAIN on a workflow that can never run again
        # (shipped in #211, caught in review; case-table row 4 says a deleted workflow is
        # reported, never a verdict). The condition is the exact negation of the excluded: guard
        # below so the two cannot drift apart: a run AT THE TIP still proves a producer existed
        # at the tip and keeps its verdict.
      | if ($mode == "gates_no_evidence") and (($state != "dead") or (($attip | length) > 0)) then
          # The tip-run note rides THIS line too, and that is not decoration. Every dispatch adds a
          # run to the DEPTH window and can evict the last in-window push run, flipping a workflow
          # from push_in_window to gates_no_evidence — so annotating only the case-2 branch would
          # make the annotation vanish precisely for the operator who applied the documented
          # dispatch remedy hardest. Gated on an ADJUDICATED probe verdict (non-blank conclusion)
          # that is NOT already at the tip, so it never decorates an UNKNOWN or a redundant line.
          (if ($gprun != null) and ($gprun.status == "completed") then
             "\(cls($gprun.conclusion))\t  \($w): \(verdict($gprun.conclusion))@\(($gprun.head_sha // "none")[0:8]) (newest push run on main, older than the \($raw0) examined; from gating probe)\(if (($gprun.conclusion // "") != "") and (($gprun.head_sha // "") != $tip) then $tipnote else "" end)\($pq)\($depth)"
           elif ($gprun != null) then
             "UNADJ\t  \($w): UNKNOWN@\(($gprun.head_sha // "none")[0:8]) (newest push run on main is still \($gprun.status // "pending"); from gating probe)\($pq)\($depth)"
           else
             "UNADJ\t  \($w): UNKNOWN@none (gates main — push probe confirms push runs exist — but none in the \($raw0) examined and the probe returned no run; raise RED_MAIN_DEPTH)\($pq)\($depth)" end)
        elif ($mode == "gating_unknown") and (($state != "dead") or (($attip | length) > 0)) then
          "UNADJ\t  \($w): UNKNOWN@none (cannot establish whether this workflow gates main — push probe failed or was inconclusive; verdict withheld)\($pq)\($depth) [gating UNKNOWN]"
        elif $n == 0 then
          (if $tot == 0 then "NB\t\($w)"
           # $raw == 0 with $tot > 0 is a DEGENERATE READ, not a benign absence: the API
           # reported runs exist and returned none. It must stay UNADJ — collapsing it into NB
           # would turn a suppressed error into a confident "nothing to judge", and the NB label
           # would additionally assert the events were non-baseline when zero runs were seen.
           elif $raw == 0 then "UNADJ\t  \($w): UNKNOWN@none (total_count \($tot) but 0 runs returned — inconsistent read)\($pq)\($depth)"
           elif $raw >= $tot then "NB\t\($w) [all \($tot) run(s) on main are non-baseline events]"
           elif $probe == "nongating" then "NB\t\($w) [no baseline run in the \($raw) newest of \($tot); push probe confirms 0 push runs on main]"
           else "NB\t\($w) [no baseline run in the \($raw) newest of \($tot); bounded window, not exhaustive]" end)
        # Exclude ONLY a stale verdict (case 2). A run AT THE TIP proves a producer existed
        # at the tip, so cases 1 and 3 keep their verdict whatever the workflow list says —
        # otherwise a momentarily incomplete list drops a live at-tip RED. Blank conclusion
        # and absent head_sha are spelled out so neither side of the `@` can render blank.
        elif ($state == "dead") and (($attip | length) == 0) then
          "ADJ\t  excluded:\($w) (no live producer — deleted from repo; "
          + (if $lastDone == null then "no completed run on main"
             else "last \(if ($lastDone.conclusion // "") == "" then "blank" else $lastDone.conclusion end)@\(($lastDone.head_sha // "none")[0:8])" end)
          + ")\($depth)"
        # ADV routes every advisory line, whatever its conclusion: an advisory workflow never
        # gates, so it must not reach UNADJ (which would hold the repo unadjudicated) nor ADJ
        # (whose RED holds the train). It is still PRINTED in full — suppressing it is what hid
        # the atlas nightly failure for 12 days.
        elif $tipDone != null then
          "\(if $gates then cls($tipDone.conclusion) else "ADV" end)\t  \($adv)\($w): \(verdict($tipDone.conclusion))@\($tip[0:8])\($busy)\($advwhy)\($pq)\($depth)"
        elif ($attip | length) > 0 then
          "\(if $gates then "UNADJ" else "ADV" end)\t  \($adv)\($w): UNKNOWN@\($tip[0:8]) (triggered on tip, \($inflight) in flight, no completed run)\($advwhy)\($pq)\($depth)"
        elif $lastDone != null then
          "\(if $gates then cls($lastDone.conclusion) else "ADV" end)\t  \($adv)\($w): \(verdict($lastDone.conclusion))@\(($lastDone.head_sha // "none")[0:8]) (not triggered on tip)\(if $gates and (($lastDone.conclusion // "") != "") then $tipnote else "" end)\($advwhy)\($pq)\($depth)"
        else
          "\(if $gates then "UNADJ" else "ADV" end)\t  \($adv)\($w): UNKNOWN@none (no completed run on main in the \($n) examined)\($advwhy)\($pq)\($depth)"
        end' < "$d/$id.json") \
      || { qfail=$((qfail+1)); echo "  $name: UNKNOWN@none (jq failed on id $id — verdict withheld)$([ "$state" = unknown ] && printf ' [producer liveness UNKNOWN]')"; continue; }
    cls=${line%%$(printf '\t')*}; line=${line#*$(printf '\t')}
    case "$cls" in
      # A workflow with ZERO BASELINE runs on main has no verdict to hide, so it is accounted for
      # on one collapsed line rather than N noisy ones — up to ~50 of the fleet's 82 queried
      # workflows are in this state. Collapsed, never dropped: it must stay visible they were read.
      NB)    nb=$((nb+1)); nobase="$nobase, $line" ;;
      # ADV prints but never gates and never counts as unadjudicated — see the jq note above.
      ADV)   adv=$((adv+1)); echo "$line" ;;
      UNADJ) unadj=$((unadj+1)); echo "$line" ;;
      *)     echo "$line" ;;
    esac
  done < "$d/query.tsv"
  [ "$nb" -eq 0 ] || echo "  no-baseline ($nb — no baseline run on main to judge): ${nobase#, }"

  rm -rf "$d"
  echo "  read: $qn workflow(s) queried, $qfail failed, $unadj unadjudicated, $adv advisory (non-gating), $probes gating probe(s), $((qn + probes + 4)) API calls issued"
  # Non-zero whenever this repo is NOT fully adjudicated: any UNKNOWN line, any failed
  # per-workflow read, or a producer-liveness filter that did not run — matching the rule
  # below that a repo is never green while any line is UNKNOWN. Every withheld-verdict path
  # above returns 1 too. A RED is ADJUDICATED: it returns 0, and the RED line itself is the
  # signal — do not read exit 0 as "green", read it as "this repo was fully read".
  [ "$qfail" -eq 0 ] || return 1
  [ "$unadj" -eq 0 ] || return 1
  [ "$live" != "null" ] || return 1
  return 0
  }
  ```
  Notes on that snippet, each load-bearing — **every one of these exists because its absence
  produced a silently wrong verdict, not because it is tidy:**
  - **Absence must never read as OK.** Every door to "no output" is closed explicitly: a failed
    tip read, an unusable workflow list, an empty query set, a failed per-workflow read (its own
    `UNKNOWN@none … verdict withheld` line), and a failed `jq` (the guard is the `||`, since jq
    exits **0** on empty stdin and prints nothing, and there is no `pipefail` here). A tick that
    prints nothing must mean the detector did not run, never "the repo is fine".
  - **Every verdict comes from the workflow's OWN runs index — one call per workflow.** This is
    the load-bearing change: `gh run list` slices a shared cross-workflow window whose content is
    unstable between calls, so an at-tip run can vanish from the slice and the workflow silently
    renders as case 2a (`GREEN@<older sha> (not triggered on tip)`), the branch this section
    documents as normal-do-not-flag. Scoping the query to one workflow removes the window.
    `--limit 100` is not a fix and never was: the corrupt read had 100 slots and spent them on
    two-week-old runs.
  - **Only `push` establishes a main baseline, and that is not this skill's opinion — it is coord's
    shipped rule.** `qontinui-coord/src/ci_baseline.rs` defines
    `fn establishes_main_baseline(event) -> bool { event == Some("push") }` with the comment:
    *"ONLY a `push` to `main` is per-commit main CI. Out-of-band runs — `workflow_dispatch`
    (manual diagnostics/runbook tools), `schedule` (maintenance), and `dynamic` (Dependabot) —
    must NEVER red the merge train: a single failed manual probe would wedge it for everyone."*
    It cites the live incident: the `workflow_dispatch`-only **Coord HA git-replica probe held ALL
    coord merges `main-red` 2026-06-26→28**. The Option-C refinement (operator-approved
    2026-07-21, after a 2026-07-19 mid-incident regression) adds that an out-of-band run neither
    writes NOR prunes the baseline — **a red baseline may be cleared ONLY by a real `push`
    verdict.** Line 173 bills this snippet as "the no-SQL equivalent of `coord.ci_baselines`", so
    it must match that predicate or it is lying about what holds the train. Hence: the verdict for
    a gating workflow comes from **push runs only**.
  - **Out-of-band runs are REPORTED but never gate — the `advisory:` class.** Dropping them from
    the *read* (the old `&event=push` URL filter) hid real defects: on `qontinui-runner`,
    `atlas/exclude.txt freshness` had been failing on **12 consecutive nightly `schedule` runs**
    with nobody looking, and `Release` and `schema.pg.sql.generated freshness` carried
    `workflow_dispatch` failures. All three read as `no-baseline` — invisible — for as long as the
    filter existed. So the fix is to WIDEN the read and NARROW the disposition: fetch everything
    on `?branch=main`, judge gating verdicts from `push` alone, and print out-of-band outcomes on
    their own `advisory:` line that is adjudicated, non-train-holding, and counted separately in
    the footer. Suppressing that line is what hid the atlas defect; letting it gate is what wedged
    coord for two days. Both failure modes are real and they point in opposite directions.
  - **Two of those three "reds" were themselves spurious — which is why advisory must never
    gate.** `Release` is triggered by a TAG push (`push: tags: v*`); tag runs are not on
    `branch=main`, so its only main-branch runs are dispatches that are *structurally incapable of
    passing* (`release.yml:57` does `${GITHUB_REF#refs/tags/v}`, which on a dispatch leaves
    `refs/heads/main` unstripped) — while the real release path is healthy (last success v1.0.6,
    2026-07-18). `schema.pg.sql.generated freshness` is a `pull_request` gate whose ~3350 PR runs
    all carry the PR branch as `head_branch`; its only main run is a dispatch that failed
    **2026-05-06** and was fixed the same day. Only `atlas` was a true defect.
  - **The `e154036b` incident was a fresh `workflow_dispatch`, NOT a re-run — do not describe it
    as one.** Both runs are `run_attempt=1` (push `30878053349` `failure` 04:34:00Z; dispatch
    `30878692574` `success` 04:47:15Z), and **a GitHub re-run preserves `event: push`**, reusing
    the run id and incrementing `run_attempt` — `ci_baseline.rs:1097` says the same. So
    `?branch=main&event=push` never hid a re-run, and the "event filter hides the newer
    authoritative verdict" story is wrong. What actually happened is that the push run failed on a
    flaky `coord-db-tests` and someone fired a fresh dispatch instead of re-running it. By coord's
    rule main WAS red there, and the correct remedy was `rerun_failed_jobs` on `30878053349` —
    which coord's own `auto_fix_red_main` already does. Letting the dispatch launder it would have
    reported the repo healthy while the train stayed held.
  - **Newest-at-tip regardless of event is unsafe in BOTH directions** — the measurement that
    settles it. At coord tip `40172d56` the push run passed every gating job while the newer
    dispatch run failed on exactly one: `clippy-nightly-unscoped`, which `ci.yml` gates to
    `schedule || workflow_dispatch`, deliberately omits `-D warnings` from, and documents as
    *"Non-gating, so a false-red here costs nothing."* Admitting it makes that job hold the train.
    It launders the other way too: `deploy-web.yml` rollback dispatches skip build+test and still
    conclude `success`, and coord's `deploy-coord.yml` has a canary input documented as *EXPECTED
    to end RED*. A dispatch run of a workflow does not run the same jobs as its push run, so its
    conclusion is not a substitute. `push` is the only un-parameterised baseline.
  - **…but a non-push run ON THE TIP is ANNOTATED, never hidden — and never promoted.** The
    push-only rule above decides the verdict; it used to also decide what you were allowed to
    *know*. A completed `workflow_dispatch`/`schedule` run at the tip was read, admitted to
    `$cand`, dropped from the judged set, and then never mentioned — so a case-2b line looked
    identical whether or not fresher evidence existed. That silence made the fleet's own
    documented remedy unobservable (see the two-row remedy table above), which is worse than
    useless: an operator who dispatches and sees no change cannot distinguish "it ran green" from
    "it never ran". It also put two pieces of fleet tooling in direct contradiction:
    `qontinui-runner/.github/workflows/qontinui-types-drift.yml`'s own header prescribes
    `gh workflow run <wf> --ref main` as THE remedy for its frozen path-filtered verdict and
    argues it *safe by construction* for a post-land status refresh (citing the 2026-07-30
    runner #905 / schemas #112 precedent) — while this detector could not see that anyone had
    done it. Measured 2026-08-13 on `qontinui-runner` workflow `272919722` *qontinui-types drift*:
    newest push `failure@5e46988e` plus a `workflow_dispatch success` sitting on the then-tip
    `104315ee`, rendered as a bare `RED(failure)@5e46988e (not triggered on tip)`. So case 2 now
    appends a **secondary** note —
    `RED(failure)@5e46988e (not triggered on tip) [tip-green: workflow_dispatch success@104315ee — observed, not adjudicating]`
    — under five rules, each of which is load-bearing:
    1. **Annotation only.** The verdict token, the `ADJ`/`UNADJ`/`NB` class, the out-of-band
       routing and the exit status are byte-identical with and without it. A RED with a tip-green
       note is still a RED that holds the train and still returns 0. It is gated **at each use
       site** on the line's own verdict being an **adjudicated** stale one, so no `UNKNOWN` line
       ever carries it — including the easy-to-miss `UNKNOWN(blank conclusion)`, which reaches the
       same case-2 branch as a real verdict and would otherwise have worn a tip-green note while
       counting as unadjudicated.
    2. **Symmetric.** The label comes from the run's own conclusion, so `[tip-red: …]` prints just
       as loudly as `[tip-green: …]`. A one-way ratchet toward optimism is exactly how this
       becomes the next false green. There are **three** labels, not two: `tip-green` for
       `success`, `tip-red` for every red conclusion (`cancelled` included, matching the verdict
       vocabulary above), and `tip-other` for the benign non-passes — `skipped`, `neutral`, and a
       completed run with a blank conclusion (rendered `blank conclusion`, never blank).
    3. **The event is always named**, because the event is the entire reason the run does not
       adjudicate. Never render a bare "green at tip".
    4. **Completed runs only**, newest-by `created_at`, matching `$tipDone`'s discipline — a queued
       or in-flight dispatch is not a conclusion and is not annotated.
    5. **Free.** It reads `$cand`, which is already in hand. No extra API call, so the per-repo
       call count in the footer is unchanged.

    It rides **two** line shapes: case 2 (`(not triggered on tip)`) and the gating-probe line
    (`… from gating probe`, case 6). The second is not optional — every dispatch adds a run to the
    `RED_MAIN_DEPTH` window and can evict the last in-window push run, moving the workflow from the
    first shape to the second, so a case-2-only annotation would vanish for exactly the operator
    who applied the dispatch remedy hardest.

    ⚠️ **Do not read `+N non-baseline dropped` in the provenance suffix as contradicting the
    note.** That counter is `$raw - $n`, so it also counts baseline `workflow_dispatch`/`schedule`
    runs dropped by the **push-only** rule, not just genuinely non-baseline events — which is why
    one line can now say `[tip-green: workflow_dispatch …]` and `+1 non-baseline dropped` about the
    same run. The wording predates this annotation; it is a labelling bug in the counter, not a
    disagreement about the facts.

    The honest tradeoff, stated rather than resolved: a green run **on the tip** is stronger
    evidence *about the tip* than a red push run on an older sha — but the events are **not
    interchangeable**, because a dispatch can run different jobs on different inputs (the three
    measured examples above). The annotation exists so a human can adjudicate that tradeoff with
    the facts in front of them. The tool refuses to adjudicate it for them.
  - **Candidate events are still narrowed first, and `branch=main` does NOT do it for you.**
    `deployment_status` and `dynamic` are never evidence: web's `Verify Frontend Deploy` is
    100/100 `deployment_status` (all `completed/skipped`), and `dynamic` on `qontinui/qontinui` is
    Dependabot `Graph Update` with 2 of 3 recent runs `failure`. Coord excludes `dynamic` at BOTH
    ingest (`ci_baseline.rs:1074`) and read time after a live wedge — **qontinui-web 2026-06-05,
    every green PR blocked until web #571 had to be admin-merged**. And `branch=main` matches
    `head_branch`, so it does not exclude a **fork PR opened from a branch named `main`**:
    `pytorch/pytorch` `Lint` (id `1316`) returns 92 `push` + 8 `pull_request` from forks, all
    `completed/action_required`. Latent on this fleet (0 such runs across 8 repos, 0 of 761 PRs
    with `head.ref == "main"`, 1 unused fork) — kept because a skill gets copied.
  - **`[read N/M on main]` says "on main" because omitting it caused a wrong diagnosis.** `M` is
    `total_count` for the `branch=main` QUERY, not the workflow lifetime. On 2026-08-04 the bare
    form printed `Release [read 2/2]` and `schema.pg.sql.generated freshness [read 1/1]`, which
    read as "may have NEVER succeeded" and drove a wrong hypothesis; the real histories are **24**
    and **3382** runs. A scoped count presented as a lifetime count defeats the whole point of
    stating the evidence.
  - **The GATING PROBE — why "no push run in the window" is never allowed to mean "advisory".**
    Whether a workflow gates is "does it have a `push` run on main", and a bounded window answers
    that only when it covered the whole `branch=main` history. When it did not, the push runs may
    simply be older than `RED_MAIN_DEPTH`: web's `Backend CI` has **26 push runs of 86 with the
    first at index 59**, past any sane depth. Downgrading it to `advisory:` on that basis would
    silently un-gate a workflow that really does gate main — absence reading as OK, which is the
    one thing this whole section forbids. So every **ambiguous** workflow (zero push runs in the
    window AND a non-exhaustive window) gets ONE extra call,
    `?branch=main&event=push&per_page=1`, and its `total_count` settles the question outright.
    **Only ambiguous ids pay**, which is the difference between affordable and not: measured
    2026-08-04 the probe count was **web 4, runner 3, coord 0** — coord needed none at all, so its
    per-tick cost is unchanged at 14 calls.
    **The same call also returns the run**, so this is not merely a classifier: when the window
    holds no push runs, `workflow_runs[0]` IS the newest push run on main, and the line renders a
    real verdict from it rather than an `UNKNOWN` —
    `Backend CI: GREEN@24e768ae (newest push run on main, older than the 10 examined; from gating
    probe)`. That matters because the failure this closes is a RED hiding out of window: it now
    prints as `RED(...)`, not as a benign advisory and not as perpetual noise. It is *usually*
    older than the window but not necessarily — the window is the newest `DEPTH` runs of ANY
    event, so a push run at the tip can be crowded out by newer non-push runs (web
    `Verify Frontend Deploy` is 100/100 `deployment_status`). The verdict stays correct either
    way; only the "older than the N examined" parenthetical can overstate it.
    ⚠️ **Both gating branches carry a dead-producer guard** — see the snippet. A workflow deleted
    from the repo keeps its last run forever, and the probe reads PAST the window into exactly
    that frozen history, so without the guard it resurrects an immortal red and holds the train
    on a workflow that can never run again. Shipped that way in #211 and caught in review.
    **Failure fails toward gating, never toward advisory.** A probe that errors, or whose body is
    unusable, yields `UNKNOWN@none … [gating UNKNOWN]` (UNADJ), so the repo cannot read green
    while it stands. Advisory is asserted only on proof: an exhaustive window, or a probe that
    positively reported zero push runs — which is why those lines now say *confirmed by push
    probe* instead of merely asserting it.
  - **`gh run list` survives, demoted to DISCOVERY ONLY — *while producer liveness is known*.**
    Its one remaining job is to learn ids of workflows that have runs but are *absent* from the
    workflow list — deleted producers that still own an immortal last run (case 4). On that path
    it contributes no verdict, so a degraded discovery read can only drop an `excluded:`
    accounting line, and that loss prints its own NOTE. **It carries no `--event` filter either,
    matched deliberately to the authoritative read above** — a deleted producer whose only main
    runs were `workflow_dispatch` or `schedule` would otherwise never be discovered and never get
    its `excluded:` line, and on the `live == null` path (where this window IS the enumeration) a
    narrower filter narrows an already-degraded inventory further. Leaving it fully open makes it a
    **superset** of the ids the allow-list will judge, which is the property that matters here — a
    non-baseline run leaking in costs nothing, because this call harvests only workflow ids and
    every id is then judged by the per-id read, which applies the allow-list. Under-discovery is
    the only harmful direction, which is what makes "just add `--event push` back here" look
    harmless when it is not. The query set is the **union** of live ids and discovered ids, and
    every member is read through the same per-id path — so an
    `excluded:` line's conclusion and sha are authoritative even when the discovery window is
    flaky. ⚠️ **The exception is `live == null`**: with no usable workflow list, enumeration
    falls back to this window ALONE, and a workflow missing from the window is then missing from
    the report **entirely** — no line, no count, and `[read N/total]` is per-workflow so it
    structurally cannot see an absent one. That is why that path prints a NOTE saying the
    inventory itself is degraded, tags every line, and returns non-zero. It is a degraded
    inventory, not an account of the repo.
  - **`[read N/total]` on every judged line — the regression guard for this defect.** It states
    how many runs the verdict actually rests on and how many exist, so a truncated, empty, or
    degenerate read is visible in the output rather than silently narrowing the evidence. The
    per-repo footer (`N workflow(s) queried, M failed, K API calls`) does the same at repo scale.
    This is not garnish: the 2026-07-31 miss was only findable by diffing two runs of the
    detector, because a wrong verdict and a right one looked identical on the page. For a
    detector whose failure mode is silence, stating the evidence count IS the self-check. `N` is
    the count AFTER the baseline-event allow-list, `total` is the API's UNFILTERED `total_count`,
    and whenever the two describe different things the line says so explicitly —
    `[read 7/16851 on main, +3 non-baseline dropped]` — so the filter can never quietly shrink the
    evidence behind a verdict.
  - **`skipped` is a benign class of its own, neither RED nor GREEN.** A workflow-level `skipped`
    means every job was skipped by a conditional — the workflow working as designed — so it prints
    as `SKIPPED@<sha>`, counts as adjudicated (it never withholds a verdict or forces a re-read),
    and does **not hold the train**. It is not folded into GREEN: it did not pass and it did not
    run, and conflating those is the same category error as calling it red, one direction over.
    This is not hypothetical tidiness — `qontinui-web`'s `Verify Frontend Deploy` is
    `completed/skipped` on **100 of its newest 100 runs** (measured 2026-08-04), so classifying
    `skipped` as `RED(skipped)` pins that repo permanently red, which is precisely the
    trains-you-to-scroll-past failure this section opens with. **`cancelled` is NOT in this
    class** and stays `RED(cancelled)`: it is the infra-cancelled class that rolls up to a workflow
    failure and self-heals on a re-run, and upcasting it to benign is the older bug this file
    already carries a reference for. ⚠️ **Nor is `cancelled` the WHOLE infra class** — most
    infra kills report `RED(failure)`, and no token in this vocabulary separates those from a
    genuine regression, because the vocabulary is derived from the run `conclusion` alone. Both
    stay RED here and both hold the train; the difference is only in the REMEDY, and it is
    settled at the step level — see “The `failure`-side discriminator is STEP-LEVEL”.
  - **`RED_MAIN_PARALLEL` (default 12) and `RED_MAIN_DEPTH` (default 10)** tune the fan-out width
    and how many runs per workflow are examined. Depth 10 is safe because the tip is the newest
    commit on `main`, so any at-tip runs are the newest rows in that workflow's own index —
    **depth can never hide an at-tip run**, it can only shorten the search for the newest
    *completed* one on the stale branch, and that case surfaces explicitly as
    `UNKNOWN@none (no completed run on main in the N examined)` rather than as a green. The wider
    baseline event set does not weaken that: `main` only moves forward, so any run created after
    the tip commit — `push`, `workflow_dispatch` or `schedule` alike — carries the tip as its
    `head_sha` and is still one of the newest rows. (Confirmed 2026-08-04 on web's `Backend CI`,
    whose newest 12 main runs are all `schedule` and whose head shas track the tip forward.)
    ⚠️ **Depth is spent on the RAW window, before the allow-list**, so non-baseline events consume
    slots: web's `Verify Frontend Deploy` fills all 10 with `deployment_status` and yields zero
    baseline runs. That is the one place depth changes an outcome — the workflow reports
    `no-baseline` (labelled `[no baseline run in the N newest of <total>]`) instead of a verdict.
    Raise `RED_MAIN_DEPTH` if you need to see past a deploy-event-heavy workflow; the trade is a
    proportionally larger response per call, not more calls.
  - **The class the shell routes on is OUT OF BAND.** `jq` emits `<class>\t<text>`
    (`ADJ` / `UNADJ` / `NB`), never a rendered line the shell has to re-parse. Classifying by
    matching the printed text would let a workflow *named* `no-baseline` route its own RED into
    the collapsed nothing-to-judge line — an operator-controlled string deciding whether a red is
    printed. Same principle as everywhere else here: never re-derive a decision from a display
    string you already had the structured value for.
  - **Exit status means ADJUDICATED, not GREEN.** Non-zero on: any `UNKNOWN` line (case 3, a
    blank conclusion, an inconsistent read, no completed run in the examined window), any failed
    per-workflow read, a producer-liveness filter that did not run, and every withheld-verdict
    path above. A **RED returns 0** — it is a definite verdict, and the RED line itself is the
    signal. So a caller must never wire `if RM_REPO=R red_main; then report_green` — exit 0 means "this
    repo was fully read", and the lines are what say whether it is green.
  - **The per-workflow `jq` program shares a HARD ~32KB argv budget with `--argjson gprun`, and
    overrunning it looks like a random regression somewhere else.** Both the program text and the
    probe run are passed on the jq command line; Windows caps the whole argv near 32768 bytes. A
    raw GitHub run object is ~16KB, so passing one whole spent half the budget. Measured
    2026-08-13: `qontinui-web` `Cross-browser Survey` carried a **16712-byte** `gprun` against a
    **12178-byte** program — ~88% of the cap with no margin — and adding ~3KB of *comments* to the
    jq program tipped it to `Argument list too long`, silently degrading that workflow from an
    adjudicated `GREEN@af80876c` to `UNKNOWN@none (jq failed on id … — verdict withheld)`. It
    fails **closed**, which is why it costs a verdict rather than manufacturing one, but the
    trigger is an unrelated edit plus run-object size, so nothing points at the cause. Hence
    `gprun` is **projected to `{status,conclusion,head_sha}`** at the shell — the only fields the
    program reads — taking it from ~16KB to ~100 bytes. **Never pass a whole API object as
    `--argjson`; project it first.** If this program ever needs to grow past its budget again, move
    it to a file and use `jq -f` rather than trimming the comments that explain it.
  - **Never rewrite the fetch helper as a heredoc.** This snippet lives indented inside a
    markdown fence, and a heredoc terminator must sit at column 0 — an indented terminator
    swallows the rest of the function, `red_main` is never defined, and the fleet's
    highest-severity signal silently does not run for that tick. (`<<-` does not help: it strips
    tabs, not spaces.) The single-quoted `printf` form is deliberate for that reason, and that
    reason alone. This rule is unchanged and is independent of the one below.
  - **Never use a shell positional parameter — a dollar sign followed by a single digit —
    anywhere in this file.** ⚠️ **This bullet used to say the opposite**, blessing "the `printf`
    + positional-argument form" as deliberate; that blessing was wrong, and it pointed four
    rounds of reviewers away from the failure. In a slash-command markdown body those sequences
    are **harness argument placeholders**, not shell positionals: Claude Code substitutes the
    invocation's argument words into the body **before** injecting it into the session, indexed
    from **zero** (the zeroth placeholder is the *first* word), and leaves unfilled positions
    **literal**. Measured 2026-08-13: `/merge-train-steward continuous. fix red CI when
    appropriate. …` rewrote the function header to `local r="fix"` and every per-workflow read to
    `repos/fix/actions/workflows/when/runs?…per_page=red` writing into a nonexistent directory,
    and every repo reported **29 queried, 29 failed, every line UNKNOWN**. It failed CLOSED —
    never a false green — which is the only reason this was an inconvenience rather than a
    fleet-wide blind spot.
    **The tracked file was never corrupted**, which is why nothing caught it: reading, reviewing
    or diffing it shows correct code, and `git log -S` on the corrupted string finds no commit —
    the corruption exists only in the injected copy. It also only fires on **argument-bearing**
    invocations; a bare `/merge-train-steward` leaves the placeholders literal and the detector
    worked, which is precisely the mode everyone tested.
    So `red_main` reads the named `RM_REPO`, and both generated helpers read named
    `RM_REPO` / `RM_DEPTH` / `RM_DIR` / `RM_WF` from the environment — named variables are not
    substituted, and this file must contain **zero** dollar-digit sequences. Every warning about
    this class, here and in the fence above, deliberately spells no dollar-digit of its own: a
    literal one would be substituted too and garble the warning that was supposed to prevent it.
  - **Neither side of the `@` can render blank.** Right side: an 8-char sha or the literal
    `none`. Left side: `verdict()` maps an empty/absent conclusion to `UNKNOWN(blank conclusion)`
    rather than emitting nothing — a completed run with an empty conclusion otherwise prints
    `<workflow>: @<sha>`, the same silent-unknown class one field over. `verdict()` also never
    calls a string builtin on a possibly-`null` conclusion: `ascii_upcase` on `null` raises
    `explode input must be a string`, and because `map()` materialises the whole array first,
    **that one bad row would destroy the entire repo's output**. This is **no longer latent**:
    the old note said "today `gh`'s Go struct coerces a JSON null to `""` … but swap `gh run
    list` for `gh api .../actions/runs`, which really does return null, and a red main becomes
    zero output." That swap has now happened — `conclusion: null` arrives verbatim from the API,
    and `verdict()`'s null-safety is what keeps it from doing exactly that.
  - **In-flight count is reported on every branch, including case 1.** A green tip verdict with
    a newer run already executing (`GREEN@<tip> +1 in flight`) is about to change; suppressing
    that on the completed branch reintroduces exactly the blind spot this section exists to
    close, for the variant where GitHub creates a *new* run id instead of resetting the old one.
  - `main` is hardcoded in **four** places (both tip reads, the discovery `--branch main`, and
    `branch=main` in the per-workflow URL). Fine for the repos at Step 0, all of which default to
    `main` — but a master-default repo fails loudly through the precondition rather than
    reporting green (cf. `reference_coord_ci_baseline_hardcodes_main_stranding_master_repos`).
  - **The exclusion is REPORTED, never silent.** A dropped workflow still prints its own
    `excluded:` line carrying the conclusion and sha it was dropped with, so the operator can see
    *why* a red vanished and can challenge it. A silent exclusion is how a real red gets buried —
    it would make this fix strictly more dangerous than the false positive it removes. Green
    exclusions print too (`qontinui` also drops a deleted `Deploy Documentation`): the point is a
    complete account of what was and was not judged, not a shorter list.
  - **The workflow list is fetched `?per_page=100`, no `--paginate`**, and `total_count` is
    compared against the returned length so a >100-workflow repo degrades to
    `[producer liveness UNKNOWN]` instead of silently reading its overflow workflows as deleted —
    over-exclusion is the failure mode that buries a real red, so it fails the other way. Fleet
    max today is web at 28.
  - **Cost: `4 + W + P` API calls per repo.** `W` = workflows in the union set (tip, workflow
    list, discovery window, tip re-read, then one per workflow); `P` = gating probes, which only
    AMBIGUOUS workflows incur. The 2026-07-31 pre-probe census was web 32, runner 21, coord 14,
    schemas 14, ui-bridge 14, qontinui 11 = 106 per fleet tick. Re-measured 2026-08-04 with the
    probe: **web 36 (P=4), runner 24 (P=3), coord 14 (P=0)** — coord pays nothing at all. Holding
    the three unmeasured repos at their old figures, that is **≥113 per fleet tick, ≥452/h against
    a 5000/h limit (≥9.0%)**, up from 8.5%. Treat it as a floor, not a total: schemas, ui-bridge
    and qontinui have not been re-measured with probes, and this census predates
    `qontinui-claude-config` joining the watch set (3 workflow files, so single-digit calls per
    tick — small, but the 5000/h ceiling is shared fleet-wide and really is hit). Wall clock
    with `-P 12`: a single per-id call is ~2.6s serial; web's 28-workflow fan-out measured
    **14.4s / 44.2s / 47.5s**
    across three runs (P=1: 74.4s, P=6: 26.5s, P=20: 18.7s) — GitHub-side latency variance
    dominates, so treat the fan-out as tens of seconds per repo, not a fixed cost.
  - **Why not a hybrid** (cheap windowed list as fast path, per-id verify only where the verdict
    would read stale)? Because the fast path can only skip a per-id call for a workflow that has
    a run **at the tip**, and measured across the fleet that is **18 of 82** workflows (web 7,
    runner 3, ui-bridge 3, coord 3, schemas 1, qontinui 1) — path filters and PR-only workflows
    mean most workflows are legitimately not at the tip on any given tick. So a hybrid saves
    ~22% of the calls, degrades to 100% of the cost **exactly when the window bug fires** (the
    corrupt read had all 10 web workflows reading not-at-tip), and pays for it with a second code
    path sitting in the one branch where the bug hides. Not worth it. Parallelism, not a second
    data source, is the lever. A `head_sha=<tip>`-scoped runs query was also considered: it
    resolves at-tip membership in one call, but every not-at-tip workflow (the majority) still
    needs a per-id read for its last completed run — and the corrupt window returned a stale
    *last completed* run too, so it would buy one call's correctness for a third data source.

  **Never report a repo green while any of its workflows is `UNKNOWN@<tip>`, and never report a
  repo green while any `RED(...)` line is present — including a stale `(not triggered on tip)`
  one.** Say `UNKNOWN (tip <sha> has no completed run for <workflow>; N in flight)` and re-read
  next tick. `excluded:` and `no-baseline` lines are the two exceptions — they carry no verdict
  and do not hold the train — but `excluded:` must be **quoted in the report alongside the green
  verdict**
  (`qontinui: GREEN (excluded: Quality Checks — no live producer, last failure@91db96e1)`), never
  dropped. And a `[producer liveness UNKNOWN]` tag is NOT a green: it means the exclusion filter
  did not run, so any red on that repo is unadjudicated — report the repo as UNKNOWN and re-read.
  ⚠️ **Tips, queue membership and candidates-in-flight are all CONSISTENT with a red main and
  cannot distinguish it.** Candidates keep being cut and keep running while main is red — they
  simply never land. In the 2026-07-20 soak the steward reported qontinui-runner "healthy —
  landed + working, 2 candidates in flight" for **two consecutive iterations** while runner main
  had been red for 2.5h with PRs blocked; a land even occurred mid-red, which made the false
  verdict look confirmed. The operator had to point at the dashboard banner. **A "fleet healthy"
  claim without a per-repo red-main read is unsupported — do not make it.**
  Once red, classify before acting (see `reference_coord_infra_cancelled_job_reds_main_holds_train`):
  a **flake / infra-cancelled job** rolls up to workflow `failure` and self-heals on a CI RE-RUN
  (the unblock is a re-run, NOT a PR); a **genuine regression** needs a fix PR — but neither a
  label nor a recovery-merge is what lands it, so do not reach for either.

  **The `failure`-side discriminator is STEP-LEVEL.** `cancelled` is the minority shape; the
  common infra kill is `conclusion: failure` with every step green, so classifying on the run
  `conclusion` alone drops a dying runner into the “author a fix PR” arm. Read the JOBS of
  the red run — `gh api repos/OWNER/REPO/actions/runs/RUN_ID/jobs` — **once, by hand, per
  investigated RED**. Do NOT fold this into `red_main`: that function sweeps every workflow on
  every repo every tick, and a per-run jobs fetch would multiply its API cost for a read it does
  not need in order to report `RED(failure)` correctly.

  | Tier | Predicate on the failed job | Reading | Remedy |
  |---|---|---|---|
  | **1 (primary)** | `conclusion == "failure"` ∧ `steps` non-empty ∧ **NO step has `conclusion == "failure"`** | infrastructure kill | **re-run it** |
  | **2** | `conclusion == "failure"` ∧ `steps` is **empty** | infra-unknown | re-run it |
  | **3 (confirmatory only)** | job log contains `The runner has received a shutdown signal` or `lost communication with the server` | corroborates Tier 1/2 | **never sufficient alone** |

  Validated 2026-08-20 over that window: Tier 1 caught **35/35** shape-A kills and **18/18**
  shape-B, with **0 false positives across 118 genuine failures**; Tier 2 covers **5** further
  cases. Tier 3 is ranked last deliberately — it costs a full log download per job, the
  GitHub-hosted OOM emits the **identical** string, and it is **blind to one of the two death
  shapes outright**. A log grep is never the test. ⚠️ **Only the first Tier-3 string is
  attested**: the 2026-08-20 sweep observed `The runner has received a shutdown signal` (with
  `The operation was canceled.`); `lost communication with the server` is inherited from the
  original write-up and was NOT observed. Since Tier 3 only corroborates, an unmatched string
  under-corroborates and never misclassifies — but do not cite it as evidence it has earned.

  **Two death shapes, and only the steps API sees both.** *Shape A*: steps green up to the
  running one, which is frozen. *Shape B* (**23 jobs**): the logs **404 entirely** — the runner
  vanished before flushing them — with setup steps green and the running step frozen at
  `completed_at: null`. A log-based detector cannot see shape B at all.

  ⚠️ **A flat job duration of ~600s / ~601s or ~902s with a `null` step is GitHub's
  abandoned-job reaper, not a `timeout-minutes` expiry.** Do not read that round number as a
  configured timeout, and do not “fix” it by raising one.

  ⚠️ **Zero failed steps vs. a NONZERO count separates the two CAUSES, and they need opposite
  remedies.** The self-hosted kill has **zero** failed steps and a re-run is legitimate. The
  GitHub-hosted `Deploy coord` OOM is the nonzero case — exactly one failed step, the build,
  exit 143 — so Tier 1 correctly does NOT fire on it, and re-running it does not help: an OOM
  needs a resource fix, not another attempt. (**10** events of that class in the sweep window.
  The “proven futile 6 times” this paragraph used to carry overstated it — the source report
  cited 6 instances; it did not establish 6 fired-and-failed re-runs.)

  ⚠️ **What a repeat death does and does NOT tell you.** If the re-run comes back with a
  genuinely **failed step**, the Tier-1/2 classification was wrong — stop re-running and treat
  it as real. But a repeat **zero-failed-step** death is *still* infra and must not be upcast to
  a regression: measured 2026-08-20, `qontinui-coord` run `32336379112` died the same way on
  attempt 2 (jobs `96332139114`, `96332139435`, both `msi-wsl`), so a re-run is not a guaranteed
  escape while the host is in that state. Keep the re-run bound; when it is spent, report
  blocked-on-infra, not a code failure.
  ⚠️ **`coord:red-main-fix` is intent/convenience only and is NOT an input to the predicate.** The
  gate is `pr_merge::predicate::is_simple_green_path` **Tier 4**, whose `Red` arm reads only
  `MainCiStatus` — the repo's DEFAULT-BRANCH baseline. No code path in `crates/` reads that label
  at all; the in-predicate escape is a `BlockOverride` waived via
  `policies::evaluator::is_recovery_candidate`, whose doc-comment says the label "is NOT an input
  here". **Do NOT infer the PR's own CI was green from a `main-red` verdict** — it implies only
  that the PR's **required** checks were satisfied, since Tier 2 also admits an advisory-red rollup
  and a zero-CI repo. Coord's engine deliberately recomputes `head_ci_green`, "never inferred from
  tier ordering"; a steward must not make the inference the source itself declines.
  ⚠️ **The recovery-waiver lane is INERT in prod — seeded ON and it never fires**, so never wait on
  it: `is_recovery_candidate` needs `rebased_candidate_green`, whose only producer is
  `pr_merge::engine::head_has_green_speculative_candidate` (a green `coord.speculative_chains` row,
  fail-closed), and speculative candidate CI is OFF — `deploy/taskdef.json` sets
  `COORD_SPECULATIVE_DISABLED="1"` against an inverted-sense read site (`!= Ok("0")`), so only the
  literal `"0"` arms it. Coord says so itself: `fixer_arm_readiness::adjacent_breakages`, entry
  `red_main_recovery_merge_lane_inert`.
  ⚠️ **Yet a red main does NOT permanently deadlock its own fix, because main-red is checked ONLY
  in the predicate at ENQUEUE time and is never re-consulted at land.** The scheduler's land path
  carries no main-red gate — its one such read, `merge_scheduler::no_reap_land_precondition`,
  merely declines the bounded-optimism shortcut and defers. So a proposal enqueued while main was
  still green keeps going and lands under the red; two further paths (`POST /merge/propose`, and
  `engine::enqueue_merge_proposal_for_pr` — "performs no merge-safety evaluation") enqueue without
  the predicate at all. Measured 2026-08-20: `qontinui-runner#1076` was green and `CLEAN`, a
  `/reevaluate` returned `block_reason_code: "main-red"`, and it **landed anyway at 08:41:31Z**
  with `mergedBy = app/qontinui-merge-orchestrator` — coord itself, no human, no `--admin`; which
  route had enqueued it was NOT established. **So check for an in-flight proposal before reaching
  for a recovery-merge**: `main-red` alongside a live proposal is not a deadlock, and a
  recovery-merge there bypasses a merge authority that is about to land it correctly. Note too that
  `coord_reevaluate_dry` reports the RAW predicate by design, so a dry `main-red` tells you nothing
  about whether a land is proceeding.
  Keep applying the label as human/agent signalling (coord's own fixer-dispatch prompt
  `next_step::build_red_main_fix_prompt` tells a spawned fix agent to apply it), but never "verify"
  it as though it were the merge mechanism — and set it with `gh pr edit --add-label`, since
  `pr_merge::labels_routes::validate_label` rejects it (that is the validator working, not another
  broken lane).

Build the per-PR + fleet snapshot. Then classify each signal against Tier 1; anything Tier 1
cannot classify goes to Tier 2.

## Step 2 — Tier 1: deterministic reflexes (known wedge classes, NO LLM reasoning)

A rule table over the *remaining* wedge taxonomy (post-Phase-1/2). Each rule = detector →
bounded, idempotent remediation (or escalate). **Reconcile this table with `/babysit-prs`
Step 4d** (`babysit-prs.md:109-116`) — it is the same taxonomy keyed on the real
`block_reason_code` set; keep ONE source, don't let them drift. In `--mode=observe`, print
the intended remediation and do nothing.

| Wedge class | Detector | Remediation (bounded, idempotent) |
|---|---|---|
| **Green-but-dirty** PR (behind main / needs rebase) | `mergeable_state=dirty`/`behind` or `freshness_next_action=rebase` + CI green. ⚠️ **A CLEAN `mergeStateStatus` does NOT rule out a coord rebase conflict — for one live PR class it asserts the OPPOSITE of the truth.** GitHub tests a **MERGE** (which trivially takes both sides); coord performs a **REBASE** (which replays commits). For a branch whose content already landed on `main` as a single squashed/verbatim commit, replaying its file-creating commit add/add-conflicts forever while the merge test stays clean. Measured on `qontinui-dev-notes#148` (2026-08-19): `gh pr view` reported `mergeable: MERGEABLE, mergeStateStatus: CLEAN` while coord held a **terminal `conflict`** on the same PR, stuck 30.8h. The decisive test is per-path **blob comparison** between the PR head and `origin/main` (`git rev-parse <head>:<path>` vs `git rev-parse origin/main:<path>`) — **`git cherry` also fails here**, because a squash landing destroys patch-id equivalence while preserving content equivalence: all 8 of #148's commits read `+` while the file was byte-identical at blob `32f17375`. Anywhere below that triages on `mergeStateStatus`, read it as "GitHub's merge test passed", never as "coord can rebase this". | **CI-DURATION-AWARE — do NOT blind-rebase (that's the eager-churn trap).** (1) **Is it even yours to fix?** If the PR is merely *behind* main and coord's dry-rebase resolves it (no `could not apply`), LEAVE IT — coord auto-rebases the candidate at land; a manual rebase only resets CI to do coord's job. Only a TRUE textual conflict (`CONFLICTING` / `could not apply`, confirm via `git merge-tree`) needs hands. (2) **Gate the timing on the repo's candidate-CI p90** (from `coord_query_merge_economics`): **short-CI (p90 < ~30m) → resolve EAGERLY** (rebase in a worktree → re-verify → `--force-with-lease` → let coord land; re-resolution is cheap). **Long-CI (p90 ≥ ~30m; runner ~2h) → resolve JUST-IN-TIME, only when the PR is at/near the FRONT of the land queue** — a rebase resets a full ~2h CI and any sibling land re-dirties it, so resolving deep-in-queue = wasted CI (the churn tonight's audit measured: 82% of candidate CI wasted, 24/24 green). (3) **Overlapping cluster:** when several PRs conflict in the same files, STACK them (`coord:stacked-on=`) or land as a coord batch so they resolve ONCE, not N times. Never `gh pr merge` — coord is the merge authority once clean+green. Rebase mechanics = `/babysit-prs` Step 5 lever 3. |
| **Already-landed empty-diff PR** (re-proposed forever) | `changedFiles=0` + non-draft + CLEAN, **or** `coord_pr_status` reports `merged_at`/`merge_commit` while `pr_state=open` | **Verify against the PR's CURRENT HEAD, then close.** A coord rebase-land that REWRITES the sha leaves the PR `OPEN` with its original commits NOT ancestors of main (when the rebase was a no-op the tip is preserved instead — both shapes reach this row, and the three guards below hold for either). Prove the land by ALL THREE of (a) `git merge-base --is-ancestor <PR head_sha> origin/main` — **the head itself is on main**, (b) `changedFiles=0`, and (c) `git log origin/main..<PR head_sha>` is EMPTY. Only then `gh pr close`, citing all three. Left open, coord re-cuts a candidate identical to main and re-runs full CI forever (observed on web#833/#836, 2026-07-23). Idempotent; not rate-limited. ⚠️ **NEVER close on the recorded `merge_commit`'s ancestry, and never on `changedFiles=0` alone.** `merged_at`/`merge_commit` come from `coord.repo_branches` keyed on `(repo, pr_number)` — a stamp about *a head that landed*, not about the head you are looking at — and coord's only invalidation fires on `pr_number IS DISTINCT FROM`, which by construction CANNOT fire when the same PR's head moves after a **partial ff-land**. Measured 2026-08-06 on runner#978: `merged_at=2026-08-05T18:49Z` + `merge_commit=be0d07fb` served against `pr_state=open`, where `be0d07fb` is genuinely an ancestor of main **and of the PR's own current head** — so guard (a)-as-written PASSED while 2 commits (+277/-5) sat unlanded. Only `changedFiles=0` stopped this reflex closing live work; that was a single point of failure, and (a)+(c) remove it. An unhydrated PR also reads `changedFiles=0`. (coord's own destructive sweep `phantom_open_candidates` already joins `rb.head_sha = mpr.head_sha` and is NOT affected — this reflex was the sole exposed consumer.) |
| **Verified-green stuck** PR (train slow) | CLEAN + green + aged past the repo's *data-driven* `suggested_stuck_threshold_secs`, AND a **diagnosed coord defect** blocking autonomy | **Recovery-merge, NOT `--admin`.** `--admin` was observed failing 2026-07-04 ("required status checks expected") — a real observation, but the premise once recorded here to explain it ("bypass lists contain only `Integration:3825026`") is FALSE: measured 2026-07-29, the four `main-merge-gates` rulesets (runner, schemas, qontinui, ui-bridge) also carry `OrganizationAdmin` with `bypass_mode: always`; only coord/web/claude-config are App-only. Bypass lists differ per repo — re-read `bypass_actors` rather than restating a table. Whether `--admin` succeeds is untested by design; the steward's path is the deterministic one: rebase onto `origin/main`, required checks green on the up-to-date head, then plain `gh pr merge <n> --rebase`. Counts against `--max-recovery-merges`. Leave the audit trail first (`/babysit-prs` Step 6). |
| **Conflicting PR gets NO new CI — coord parks it in `ci-pending` forever** | `/reevaluate` returns `block_reason_code: "ci-pending"` with **`input_freshness.ci_check_row_count: 0`**, and `GET repos/<r>/actions/runs?head_sha=<FULL 40>` returns `total_count: 0` — i.e. CI never fired even once. | **Check `mergeable` FIRST; this is not a separate defect.** A CONFLICTING PR gets **no new `pull_request` workflow runs at all** — GitHub cannot compute `refs/pull/N/merge`, so nothing is scheduled (not queued, not skipped). No runs ⇒ no check rows ⇒ coord waits for CI that can never arrive, and the PR shows **no FAILING checks**, so any sweep that counts only reds reads it as healthy. Measured 2026-08-24 across `qontinui-dev-notes`: 4 of 9 stuck PRs had `total_count: 0`. **Remedy: resolve the conflict — CI follows.** For a **MERGEABLE** PR whose CI simply never fired, `gh pr close <n> && gh pr reopen <n>` fires `reopened` and schedules it (no content change, no new commit, same head sha) — verified on dev-notes#153, green in ~20s, and it then let coord reach its real verdict (`[already-landed] — close this PR`). On a CONFLICTING PR the same close/reopen is a **no-op**: it succeeds and schedules nothing (verified on #203/#84/#78). Do NOT go hunting for disabled Actions or missing workflow files. ⚠️ Use the **FULL 40-char** sha on that query — a short sha returns a silent 200 with `total_count: 0` and fakes this exact symptom. |
| **`cancel` bucket misread as a failure** (triage error, not a wedge) | a PR's non-passing check is in the **`cancel`** bucket, not `fail` | **`cancelled` is NOT `failed` — it reached NO verdict.** Treating it as a red hides PRs that are actually fixable. Measured 2026-08-22: runner#1062 and #1055 were skipped as "has failures beyond `security`", but those extras were `Clippy diff-scoped (advisory) → cancel` and `test (ubuntu-22.04) → cancel` (the latter cancelled after a **6h** run). Both were genuinely `security`-only, i.e. the stale-base class; after a rebase both went fully green — and #1055's previously-cancelled ubuntu job reached a real verdict in 51m. When triaging, filter on `.bucket == "fail"` and report `cancel` separately. This is the per-PR twin of the main-baseline `cancelled` handling above. |
| **Stale read** (`freshness_next_action=refresh_github`) | `confidence ∈ {stale,unknown}` | Fire the concrete re-eval lever `POST <base>/pr-merge/prs/<owner>/<repo>/<pr>/reevaluate` (`babysit-prs.md:122` — cures stale snapshots), then wait one poll tick. Phase 2's freshness gate + Phase 1's post-land refresh do the re-read. Idempotent; not rate-limited. |
| **Orphaned-proposal residue** | `coord_proposals_resumed_after_failover_total` climbing without corresponding lands | Verify Phase-1 recovery ran (check the metric moves + lands resume); nudge re-eval on the affected PRs. If Phase-1 recovery regressed (metric climbs, no lands, no recovery), that's a **coord defect → Tier 2**. |
| **Re-eval drift starvation** | `pr_merge_reconcile_reeval_total{reason="stale_eval_backstop"}` flat ACROSS TWO SAMPLES while a stale backlog > 0 | Raise the non-drift reserve knob (the `e86d2026` mitigation is a knob), or alert. ⚠️ **The series name matters:** there is NO `reconcile_reeval_stale_eval_backstop` — that bare name greps zero forever and the detector silently never fires (verified absent in production 2026-08-19T23:52Z; see Step 1). ⚠️ **"Flat" is only measurable across TWO samples.** This is a COUNTER: its absolute value says nothing about whether it is advancing, so a single nonzero scrape is not health and a single scrape is not "flat". Scrape twice, spaced, and compare — measured 2026-08-19, it read `203` on every leader-shaped scrape across ~7 minutes, which is a *finding* only because it was sampled repeatedly. And per Step 1, a follower scrape renders the whole family as `0`, so an apparent drop to zero is a WRONG-REPLICA read, not a reset — counters cannot go backwards without a restart. Backlog side: `pr_merge_reconciler_backlog_stale` (leader-only gauge; its HELP says it converges to 0 in steady state, sustained non-zero = the frozen-row backlog is not draining) and `pr_merge_pr_state_stale_backlog` (leader-only; **cluster-consistent twin is `GET /pr-merge/health` → `pr_state_stale_backlog`, which is authoritative**). NOTE: a flat backstop at **0 with no backlog is HEALTHY** — do not fire on it. |
| **Phantom required context / aged prior** | the documented signatures (a required status context with no producer; a merge-state-unsettled dwell > 2× threshold on a fully-green head) | Arm the existing dark-launch flag / age-out per the shipped fixes; else escalate. |
| **Phantom-kill / un-credited land** (long-CI repos) | main ADVANCED with the PR's content, but the PR is still `OPEN` and coord re-cut a NEW candidate **identical to the main tip** + re-ran full CI; churn-guard terminal error "candidate CI never converged" fired **seconds after** a successful land | ROOT-CAUSED + FIXED 2026-07-18 (`b0fab9c8`/coord#1095): `recover_same_term_stalls` reclaimed a `landing` row mid-push (dequeue-anchored `leased_at` + off-lease CI wait ⇒ every >1h-CI land raced the reclaim). If it RECURS, the fix isn't serving — verify the ECS image (see Honest-bookkeeping); do NOT re-propose a PR whose content is already on main. |
| **`ci_timeout` < real CI livelock** | a GREEN candidate is re-cut ~seconds after its CI completes, forever; nothing lands on a repo whose CI > `COORD_MERGE_CI_TIMEOUT` (1800s default) | FIXED 2026-07-17 (`adb844d6`+`65a462bc`/coord#1070/#1078): `FallThrough`→`check_and_land`. Emergency lever if it recurs: raise `COORD_MERGE_CI_TIMEOUT` above the repo's CI wall-clock (fleet-wide; per-repo timers are the real fix — redesign P1). |
| **Actions-saturation firehose** (NOT a coord defect) | runner train stalls with green PRs queued; `gh run list` is dominated by ONE branch pushing every ~few min; candidate CI is queued-not-started | **Check the COMMITTER of the looping commits** (`gh api repos/<r>/commits`): `github-actions[bot]` ⇒ a self-triggering auto-commit workflow (e.g. nondeterministic codegen re-detecting its own drift — clorinde `pub mod` HashMap order, fixed runner#769 `55b04022`), NOT a looping agent. Fix = deterministic codegen / per-branch CI concurrency-cancel. Escalate to the branch owner; do NOT "stop an agent" that isn't the cause. |
| **Post-deploy proposal loss** (NOT a wedge) | coord went quiet on candidate-cutting right after a deploy / reconciler restart | ⚠️ **The "~80 min to rehydrate" figure was FOLKLORE — corrected 2026-07-20 by source trace.** No such constant exists in coord. **Scheduler recovery after a redeploy is ~17s** (leader TTL 15s, `leader.rs:55-57`, + a 2s tick, `merge_scheduler.rs:1843-1847`). What can cost ~88 min is a *single proposal* whose in-flight CI is DISCARDED by the Phase-2 requeue (`merge_scheduler.rs:8692-8704`) — and only for `dry-rebasing`, `landing`, batch members, `speculative-ci`, and base-moved `awaiting-ci` with no live CI. A plain `awaiting-ci` singleton on an unmoved base is ADOPTED and costs nothing (Phase 1, `:8455-8506`). ~88min is runner's candidate-CI suite length (`merge_scheduler.rs:1210`, `:1246`), not a recovery timer. **So: don't wait 80 minutes, and don't cite a system-wide recovery time — quote the per-proposal work at risk.** Correlate "idle since" with a task-def revision bump; WAIT, do not remediate. |

Tier 1 is **deterministic, auditable, fast** — the SRE reflexes, no LLM cost.

**Watch-only repos (Step 0).** Every row above whose remediation ends in "coord lands it"
applies to the **merge-authority set** only. On a watch-only repo the steward's remedy stops at
**landable and green** (Step 0), and the land is someone else's mechanism. Still never
`gh pr merge`, and still never `--admin`.

Three rows need their default INVERTED here rather than merely disapplied:

- **Verified-green stuck.** The detector's `AND a diagnosed coord defect` conjunct is
  **merge-authority-only** — by construction it can never hold where coord is not the lander,
  which would leave a watch-only repo with NO row that detects a stuck PR at all. The
  aged-past-threshold half stands alone: non-draft + green + unlanded past a plain wall-clock
  threshold IS the wedge. The remedy is not a recovery-merge (there is no coord defect to
  recover from) but **diagnosing that repo's own land mechanism** — for ccfg, `rerun_failed_jobs`
  on the PR's `lint-frontmatter` run, which is the only thing that re-arms its edge-trigger.
  Handle it HERE; do not let it fall through to Tier 2, which would spend a plan and a
  `/vet-imp` run on something one re-run fixes.
- **Green-but-dirty.** Step (1)'s "is it even yours to fix? … LEAVE IT — coord auto-rebases the
  candidate at land" is **merge-authority-only**. Nothing auto-rebases on a watch-only repo, so
  a merely-behind PR stays behind forever and LEAVE-IT is exactly the wrong default. Rebase it.
  The CI-duration gating in step (2) is likewise moot where there is no candidate CI.
- **Already-landed empty-diff.** This row **DOES apply**, and on a watch-only repo it is the
  more likely of the two — a stranded PR's content often lands out-of-band via a successor PR
  while the original sits open (ccfg #231, superseded by #255). Run its three-part proof
  against the PR's CURRENT head before touching anything else; a PR that needs closing must
  never be re-run and landed as a no-op.

**Bounded-remediation discipline (inlined loop control).** Each Tier-1 remediation is a
bounded attempt, not an open loop:

- **Attempt cap.** Give each lever ~2 poll cycles to take effect (cheapest-first, like
  `/babysit-prs` Step 5). A single wedge gets at most **3 remediation attempts** before it
  is treated as novel and escalated to Tier 2 (or, if Tier 2 also cannot resolve it, to
  Tier 3).
- **Stall detection (PRIMARY).** Fingerprint each attempt as
  `sha256(sorted(touched)+block_reason_code)`; if two consecutive attempts on the same wedge
  produce the same fingerprint (same action, same block reason) → **STALL** → stop
  remediating that wedge and escalate. No-progress is the primary stop; the attempt cap is
  the backstop.
- **Emit-on-block.** If a wedge is **blocked on an observable condition** the steward cannot
  clear now (an upstream PR must merge, a deploy must go healthy, CI must go green, a metric
  must cross a threshold, a time window must elapse), register a typed coord gate via
  `/gate` (or `/blocked`) BEFORE moving on — turn the blocker into a watched gate, not a
  silent skip. Note the returned `gate_id`.

## Step 3 — Tier 2: autonomous handling of ANY deficiency found (the fully-autonomous pipeline)

**Scope: not just wedges.** Tier 2 fires on *any deficiency the steward finds in the course
of its work* — in the coord layer or anywhere else it touches. Per charter rule 10
(**finish to zero**), your assignment includes its follow-ups: defects, gaps, and adjacent
issues discovered during the work get plans — written, vetted, and implemented — **even when
not core to the session's topic**. Choosing among discovered follow-ups is **not** an
escalation: do them all, ordered by the priority documents. Concretely, all of these are
Tier-2 work, not "observations to report":

- an unclassifiable merge signal (the original novel-wedge case);
- **a coord defect of any kind** you trip over — a scheduler bug, a starved sweep, a guard
  that should have fired and didn't, a metric that lies;
- **a retrieval gap in the twin** — any merge fact you had to re-derive by hand (see the
  standing responsibility in Rules);
- **a tooling/doc deficiency**, including *in this skill itself* — a lever this doc names
  that isn't deployed, a threshold that false-fires, a detector with a logic bug;
- a defect in a neighbouring repo (web, runner, schemas, ui-bridge, claude-config) that the
  merge train surfaced.

**Decide with the policies, not by asking.** When a discovered deficiency raises a judgement
call — priority, blast radius, whether to fix now or gate it, whether it's even in scope —
resolve it against the policy documents and **cite the clause you applied**. If no clause
covers it, record a `POLICY_GAP` and proceed on your best judgement. Only a hit on the
closed escalation list (Step 4) goes to the operator.

**Register a gate for anything you must defer**, with a returned `gate_id` — a deferred item
without a gate is a silent drop (charter rule 7). Name every unchased anomaly in the ledger:
what, why not chased, where to look.

The pipeline, for each deficiency:

1. **Root-causes** it — spawn a **read-only `Explore`** trace of the relevant subsystem
   (`Agent` with `subagent_type: Explore`); gather the `predicate_eval` /
   `unlandable_cycle` payloads, the merge-scheduler path, file:line evidence.
2. **Authors a fix plan** — write `$QONTINUI_PLANS_DIR/YYYY-MM-DD-coord-<defect-slug>.md`
   in the `/babysit-prs` Step 7 shape (Symptom / Evidence-verbatim / Root cause file:line /
   Fix design + detection-gap / Recovery taken). `$QONTINUI_PLANS_DIR` is the directory
   plans live in, injected by the qontinui runner from its `paths.plans_dir` setting;
   **if it is unset** — a session launched outside the runner will not have it — ask the
   user once where plans live, or fall back to `<workspace-root>/plans`. Never assume an
   absolute path from another machine. Pass the resolved absolute path to step 3, not the
   variable.
3. **Runs `Skill: vet-imp`** on that plan — `/vet-plan` audits it, then `/implement-plan`
   builds it worktree-isolated, runs CI, opens the PR. The vet pass + CI are the correctness
   gates; a bad fix fails them and never lands.
4. **Lands it** — via the train (preferred), or — when the train *itself* is the diagnosed
   defect — via the `/babysit-prs` recovery path (rebase → required checks green on the
   up-to-date head → plain `gh pr merge --rebase`; **NOT `--admin`**, which is not a
   sanctioned steward path and whose behaviour on these rulesets is untested — see the
   `--admin` section of `qontinui-claude-config/knowledge-base/qontinui-specific/coord-merge-train.md`).
   A coord fix-land waits for a DRAINED queue (Guardrails); a recovery-merge counts
   against `--max-recovery-merges`.

**with NO human approval click** — this is the operator's explicit directive, made
responsible by gating on checks, not permission.

**Don't let Tier 2 fire on a Tier-1-known wedge** (wasted LLM + a redundant PR) — Tier 1
must be tried first and its taxonomy kept current. A wedge Phase 2's `enforce` retires must
be *removed* from the Tier-1 table, not left to double-fire.

## Step 4 — Tier 3: escalate ONLY on the fleet's CLOSED escalation list

The steward uses **the same closed list as every other fleet session** (charter rule 8) — it
does not keep a private, narrower one. Escalate only on:

(a) a **security / credential / billing / data-loss class** decision;
(b) a **genuine priority tie** you cannot break from the priority documents;
(c) **no verification gate exists** for the change (nothing — tests, CI, candidate CI,
    no-reap — could catch a bad version of it); or
(d) a **true capability floor** — one diagnosed missing credential or an operator-held
    resource: an interactive login, a payment method, a VPC/console action, a physical
    action, or a coord outage needing a human to clear a stale leader-lease row.

**High blast radius alone is NOT a trigger** when a verification gate exists — and for merge
work one always does (vet + CI + candidate CI + the no-reap gate + per-PR review). Neither is
an oversize plan: decompose it and orchestrate it with subagents; escalate only if it *also*
hits (a)–(d).

Surface an escalation **with a recommendation**, not as an open question, and use
`coord_ask_question` (then status `waiting_human`) for anything only a human can answer.
`AskUserQuestion` is for the interactive-operator case per the `_loop-control.md` carve-outs.

Everything else the steward decides + executes. A per-PR `needs_human` / `operator_merge`
state is **surfaced (logged), not auto-actioned, and is NOT a loop-halt** — it's one PR's
state; keep watching the rest of the fleet.

## Guardrails (hold every iteration)

- **Fix-PR policy — PER-REPO, and gated on STATE not a clock.** There is **no fleet-wide
  fix-PR cap**. The harm the old `--max-fix-prs 2/hour` was proxying for is *one specific
  thing*: **a coord deploy orphans in-flight merge proposals.** Only `qontinui-coord` lands
  cause that — web lands go to Vercel, runner is a desktop app, schemas/ui-bridge are npm
  packages, and **none of them restart the orchestrator**. A uniform cap therefore throttled
  the changes that were free while barely constraining the one that isn't.
  - **Non-coord repos: UNCAPPED.** Author and land every fix a discovered weakness warrants.
  - **qontinui-coord: land when the queue is DRAINED, not on a timer.** Before landing a
    coord fix, read the in-flight proposal state (`coord_query_merge_economics` → `open_proposals`
    / `open_proposal_list`, the queue depth /
    `awaiting-ci` count). Land into a quiet queue; hold while proposals are mid-CI. Two coord
    fixes landing back-to-back into a drained queue are SAFER than one landing mid-flight —
    which is exactly what a per-hour cap cannot express.
    ⚠️ **`open_proposals` OVER-COUNTS — filter before trusting the drain signal.** It includes
    long-dead `shadow-landed` rows (19 of them aged ~40 days on 2026-07-23: coord 15, web 5),
    which make a quiet queue look busy and can defer a coord fix indefinitely. Count only
    genuinely in-flight statuses (`queued`, `awaiting-ci`, `dry-rebasing`, `landing`,
    `speculative-ci`) and ignore `shadow-landed`. Cleaning up that residue is Tier-2 work.
  - **Do NOT batch several fixes into one PR to reduce deploys.** It coarsens the revert unit
    (one bad fix contaminates the rest), degrades the review that is actually catching defects
    (11 real defects were found across two *small* single-purpose PRs on 2026-07-20, including
    a build break and a clock-skew inversion), and worsens file-set overlap so the fat PR
    blocks more siblings. It also does not control the fleet deploy rate — peer sessions land
    coord PRs continuously. **coord's push-deploy DEBOUNCE already batches at the deploy
    layer**, which is the correct layer: N lands collapse to fewer deploys without coupling
    unrelated code.
  - **Batch the PLAN, not the PR.** Accumulate discovered weaknesses into one running plan and
    `/vet-plan` them as a set — the vet pass sees interactions per-PR review cannot — then
    implement as SEPARATE, independently revertible PRs.
  - **The real fix is upstream:** a coord restart should not orphan proposals at all. Until
    that lands, the drain gate above is a workaround, not the design.
- **`--max-recovery-merges` (default 1/hour) STANDS.** A recovery-merge bypasses coord, the
  merge authority, on a diagnosed-defect argument — that is a genuinely dangerous, genuinely
  rare act and deserves a hard ceiling. Do not conflate it with authoring fixes.
- **Tighten the REVIEW, not the volume.** If autonomous fix quality drops, raise the vet bar /
  require a second adversarial review pass. Per-PR `/code-review` is the gate that has actually
  caught bad code here; a volume cap never caught any of it.
- **Every change goes through the same worktree-isolated, vetted, CI-gated path a human
  session uses.** NEVER `--no-verify`. NEVER bypass a real (non-defect) gate —
  `escalate-path-matched` and red CI are the system *working*; only a **diagnosed coord
  defect** or **coord outage** justifies a recovery-merge, with evidence quoted on the PR
  first (`/babysit-prs` Step 6 preconditions; the audit trail is mandatory per
  recovery-merge).
- **Honest bookkeeping — PR state is DOUBLY unreliable; CONTENT on `origin/main` is the only
  proof.** A coord ff/rebase-land leaves the PR `CLOSED, merged=false` (closed ≠ unmerged) —
  but ONLY when the rebase REWROTE the sha; on a TRUE fast-forward the pushed tip is
  byte-identical to the PR head, so GitHub marks it **`MERGED`** with `merge_commit_sha ==
  head sha` (measured 2026-08-20, `qontinui-runner#1076`). BOTH shapes are coord lands, so
  neither `merged` value proves anything on its own. AND
  a landed proposal can be left **`OPEN`** by the phantom-kill bug (open ≠ unlanded — see
  Tier-1 table). So NEVER judge landed/not-landed by PR `state` OR the `merged` bool: grep the
  distinctive content on `origin/main`, or `git merge-base --is-ancestor <candidate-tip>
  origin/main` (note coord rebase-lands REWRITE the sha, so ancestry of the *original* PR head
  sha is not proof — use content, or the rewritten landed sha). NEVER stamp a plan SHIPPED, nor
  report a wedge cleared / gate attested, on anything weaker than that ground truth + a returned
  `gate_id`.
- **A landed fix is NOT a serving fix — verify by ECS task-def image, never a green deploy run.**
  coord push-deploys DEBOUNCE and silently no-op; a green `Deploy coord` workflow does NOT mean
  the new image is serving (this trap hid a landed fix TWICE in one soak). Confirm the serving
  sha via `aws ecs describe-task-definition` (image tag == git sha; `AWS_PAGER="" MSYS_NO_PATHCONV=1`,
  us-east-1, cluster `qontinui-staging`, service `coord`) and check the fix commit is its
  ancestor. Force a real deploy with `gh workflow run deploy-coord.yml --ref main` (the
  never-debounced `workflow_dispatch` lane), then re-verify the tag. `/coord/build-info` is
  operator-Bearer-gated → useless from a device session. A wedge you "fixed" keeps firing until
  the fix actually SERVES.

## Continuous operation

- **`/loop` (default).** Invoke as `/loop <interval> /merge-train-steward <args>` for a
  self-pacing continuous watch, or omit the interval to let the model pace itself. Each
  iteration runs Steps 0-4 once. Between iterations, do nothing but wait — the loop
  re-invokes.
- **`presentation:"terminal"` continuation.** Alternatively a coord `continuation_spawn`
  with `presentation:"terminal"` on the operator's device opens a visible terminal running
  this skill — the operator sees it and can interrupt.
- **`--once`** runs a single pass (assess → act → report → exit) — for a manual spot-check
  or CI dry-run.

## Report (each iteration, and on exit)

Per iteration, emit a compact ledger: for each PR/signal touched — `repo#pr | class | next-action
| action-taken | outcome` — plus the counters (`recovery_merges 0/1` — a real cap; and
`fix_prs authored: N` per repo — a TALLY, not a ceiling) and any registered
`gate_id`s. Also list **deficiencies found → what you did about them** (fixed / PR # / plan +
gate_id / unchased-with-reason) — a found deficiency with no disposition is a silent drop.
On exit (stop / `--once` / cap), emit
the structured handoff (`_loop-control.md` Element 3): iterations run, terminations, per-wedge
ledger, any Tier-3 escalations with the specific decision needed **and your recommendation**,
and any deferred fix-lands with their reason (deploy-batch defer / rate-limit defer).

Close the session's final report with a **`POLICY_COMPLIANCE` footer** per the unified policy
protocol, listing the clauses you applied and any `POLICY_GAP` you recorded.

**Stamp the ledger with an observation time** (`STEWARD it=N HH:MM:SSZ`) and, for any value you
report as *unchanged*, re-read it this iteration before writing it down. Every number in the
ledger must come from a command run in THIS iteration — if you cannot point at the call that
produced it, it does not go in the ledger. This is the enforcement mechanism for the
re-measure-every-iteration rule above: the format makes a carried-forward value impossible to
state without noticing you are stating it.

## Field-tested operating lessons (from the 2026-08-04/05 live soak)

*A ~15h soak that began with the whole fleet wedged behind one unappliable alembic revision.
These are the lessons that cost the most time; the 2026-07-17/18 set below still holds.*

- **A silent-empty read is the single most dangerous thing on this fleet, and it will get
  you through a door you did not know was open.** Concrete, measured: `merge_scheduler.rs`
  is **2.74 MB**, past GitHub's contents-API blob limit, so
  `gh api repos/.../contents/src/merge_scheduler.rs --jq .content` returns
  `content_len: 0, encoding: "none"` **with a 200**. Every `grep` against that empty string
  succeeds and returns `0`. At 20:10Z this produced two *opposite* false conclusions inside
  five minutes — first "this PR's content is absent from main" (it had landed), then "the
  defect is fixed on main" (it was not) — and nearly caused a landed fix to be re-gated as
  an open defect. **For any file over ~1 MB, use `git grep <pat> origin/main -- <path>`.**
  Check `.size` on the contents response before trusting a zero from it, and treat an empty
  read as UNKNOWN — never as NO (`verification-and-evidence` `silent-empty-is-unknown`).
- **`git show <rev>:<path>` is MSYS-mangled in Git-Bash** (`origin/main:.claude/…` becomes
  `origin\main;.claude\…` → `fatal: ambiguous argument`). It looks exactly like a missing
  ref or a wrong path. Use `git grep <pat> <rev> -- <path>`, or read from a worktree. Do NOT
  reach for `MSYS_NO_PATHCONV=1` to fix it — that is for `gh`/`aws` only and breaks
  `git -C /d/...` paths.
- **A coord ff-land may REWRITE the sha, so `git merge-base --is-ancestor <pr-head>
  origin/main` is a ONE-WAY signal — a FAIL proves nothing.** A **rebase**-landed PR (sha
  rewritten) reads `CLOSED` with `mergedAt: null`, no merge commit, and a head sha that is
  not an ancestor — indistinguishable from closed-unmerged by state alone. But when the
  rebase was a no-op the tip is preserved, the head IS an ancestor, and GitHub reads
  `MERGED` (`qontinui-runner#1076`) — so a **passing** ancestry check is genuine positive
  evidence; only a failing one is uninformative. Never read a fail as "unlanded".
  ⚠️ A pass proves only that *the head is reachable from `main`* — NOT that this PR's
  work landed: an unhydrated or empty branch whose head is an old `main` commit passes
  vacuously. That is precisely why the Tier-1 close row keeps guards (b) `changedFiles=0`
  and (c) empty `git log origin/main..<head>`. **Ancestry alone never authorises a close.**
  **Distinctive CONTENT on `origin/main` settles it either way** (and see the silent-empty
  trap above, which is exactly how that content check fails open). Full two-shape model:
  `knowledge-base/qontinui-specific/coord-ff-lands.md`.
- **Believe coord's error TEXT last.** Twice in one soak the stored message named the wrong
  subsystem and sent the diagnosis hours in the wrong direction:
  - `"land task died/dropped/lost-to-teardown"` — the task had not died. It ran to
    completion in 2.6–16.1s and returned `Deferred`. The first land heartbeat is only
    written at t=60s (`tick.tick().await` consumes the immediate tick), so a sub-60s
    *deliberate* defer never heartbeats, and the 900s stall sweep reads it as a corpse.
  - `"push rejected (deterministic — will not succeed on retry)"` — it was a lost 8ms land
    race, retryable by definition, and a sibling PR with the same verdict later landed.
    coord's classifier had substring-matched `"rule violations"` inside GitHub's
    `remote: Bypassed rule violations for refs/heads/main:` — the banner announcing a bypass
    actor's push was **let through**.

    Prefer structured evidence over prose: `coord_query_scheduler_trace`'s
    `decision_code` + `no_reap_verdict`, the proposal's real duration, and the check-runs on
    the candidate head. All three were unambiguous while the text lied.
- **`coord_query_train_health` excludes `deferred:no_reap` from its wedge allow-list
  wholesale**, which is right for the bounded CI-reap arm (`defer:unsafe`, clears with the
  run) and **wrong for `defer:migrate_unsafe`, which is bounded by nothing.** If a repo shows
  a `deferred:no_reap` histogram dominating while nothing lands, read `no_reap_verdict`
  before believing `is_making_progress: true`. The control case that proves the
  discrimination is real: same decision code on qontinui-runner resolves into `safe:gate` +
  `landed`.
- **A stale red "not triggered on tip" can still be the ROOT CAUSE.** The 2026-08-04 fleet
  wedge was an `Apply Migrations to RDS` red on an older sha — easy to file as cosmetic
  under the case-2a rule. It was not: prod's alembic `applied_head` fell behind `chain_head`,
  coord's migrate gate deferred **every** PR touching `alembic/versions/`, and the whole
  fleet's slot semaphore saturated behind it. Case 2b in the red-main table exists for
  exactly this. Do not deprioritise a stale red because a newer green sits next to it.
- **Fixes to silent-failure bugs reintroduce silent failure at a high rate.** Four times in
  one soak a *fix* re-created the class it was fixing, and every one was caught by review
  rather than by the author: a drift-gate guard that could not tell a real migration from an
  entire schema reading empty (would have auto-authored a PR dropping every exclusion); new
  detector branches inserted ABOVE the deleted-workflow guard (resurrecting immortal reds);
  an over-strip guard whose false positive terminated the block and leaked every remaining
  line; and a porcelain-first reroute that would have made deterministic push classification
  unreachable entirely. **When fixing a silent failure, test the NEGATIVE path harder than
  the positive one** — the author is reasoning about the happy path while the danger stays on
  the other side.
- **Nested `code-reviewer` verdicts routinely misroute to the COORDINATOR instead of the
  spawning agent — nine times in this soak.** Every one carried findings the author would
  otherwise have shipped without, including the four above. **Coordinator: watch for a review
  verdict arriving in your own notifications and RELAY it in full** — the spawning agent
  cannot see it and will report "no verdict reached me". **Subagent: if your reviewer's
  verdict never arrives, SAY SO explicitly rather than assuming approval.**
- **"Ready for review" is not a hold — on a coord-landed repo, undrafting IS merging.**
  Sequencing "run the review" and "undraft" in one instruction put a train-holding regression
  on `main` for ~1h: the reviewer stalled, the agent disclosed a degraded review and
  undrafted as told, coord merged within minutes, and the reviewer then returned CRITICAL.
  **Draft is the only real brake.** Sequence it: review returns → findings addressed → then
  undraft.
- **Verify a subagent's load-bearing claims, and expect your own briefs to be wrong.** In
  this soak the coordinator's briefs contained: a false root cause whose recommended fix
  would not have worked (`sa.text()` silently truncates a bind butted against `::` — the
  file never mixed parameter styles); two mutually-defeating instructions (strip only the
  banner AND retain full stderr — the block body re-states the rules and re-convicts); a
  stale sibling version read from a local checkout 4 commits behind; a "lower-risk
  alternative" that was a non-fix; and a sha that did not exist. Agents that pushed back with
  evidence were right every time. **Brief them to trust the source over the brief, and to
  report the disagreement.**

## Field-tested operating lessons (from the 2026-07-17/18 live soak)

- **Run as a lean COORDINATOR; delegate heavy work to subagents. This is a RULE, not a
  preference — context is the binding constraint on a continuous steward.** Over a multi-hour
  session the main context must stay a thin ledger (iteration #, wedge fingerprints, one-line
  outcomes). Reading source / running curl/SQL probes from the main context is drift — that's
  what burns the window.
  - **DELEGATE:** every deep investigation, root-cause trace, code fix, rebase/conflict
    resolution, `/vet-imp` run, per-repo deep-dive, and pre-PR `code-reviewer` pass. One repo
    (or one wedge) per subagent; launch independent ones **in parallel in a single message**.
  - **KEEP INLINE:** the fleet scan, the Tier-1 reflex dispatch, the ledger, and the
    act/no-act decision. Those are cheap and they are the steward's actual judgement.
  - **Brief each subagent with the constraints it cannot infer** — the repo's landmines
    (never `cargo fmt` in coord; zero `coord.*` DDL; **never `gh pr merge`**, coord is the
    merge authority; worktrees must be direct children of `<workspace-root>/`; never touch a
    peer's WIP), what "done" means, and **that it must report what it could NOT do**. A
    subagent that inherits none of your context will confidently do the wrong safe-looking
    thing.
  - **Consume only the compact report, and do not take it at face value.** Spot-check the
    load-bearing claims yourself (PR exists + state, commit on `origin/main`, CI conclusions)
    — charter rule 1 wants ≥2 independent signals, and a subagent's self-report is one.
  - **Note the capability floor:** a subagent cannot always spawn its own subagent. If the
    pre-PR `code-reviewer` pass is mandatory (it is), either spawn it from the MAIN session
    against the worktree diff, or treat a self-review as a **gap to report**, not as the
    review having happened.
- **Adversarially verify YOUR OWN fix — a landed fix that "should work" is a hypothesis, not a
  result.** In this soak a fix (the FallThrough re-cut backstop) LANDED green and introduced a NEW
  failure mode that re-cut green candidates; it was caught ONLY because a fresh `debugging-specialist`
  was pointed at "is my own fix causing this?" instead of declaring victory on merge. After shipping
  any scheduler change, watch the BEHAVIOR it was meant to fix (does a green candidate now actually
  LAND?), and treat a plausible-but-unverified fix as suspect. Prefer a falsifiable prediction ("if
  my backstop misfires, candidate X dies ~45m in") over "it landed, done."
- **Distinguish slow-but-healthy from wedged before remediating.** On runner the whole train is
  `~1 land / 2h` FIFO by physics (2h CI + no-reap serialization). A green PR sitting 2h, a proposal
  `queued` with no candidate for an hour, or coord quiet right after a deploy are all NORMAL. Confirm
  a real wedge (proposal terminal/errored, a stuck lease, a stale `main-red`, a churn loop) before
  spending a remediation attempt — a needless nudge re-cuts a candidate and restarts the ~2h clock.
- **A green candidate CI that keeps re-cutting is the tell for a scheduler defect, not slow CI.**
  Go to the actual runs on the candidate ref (`gh run list --branch merge-candidate/<proposal-uuid>`):
  `conclusion=success` on a sha followed by a NEW sha seconds later = a real bug (livelock /
  phantom-kill / churn), not saturation. `24/24 completed candidate runs GREEN` yet nothing lands is
  a scheduling problem, full stop.
- **Robust polling.** These watches run in Git-Bash: a literal `/` in a `gh -q` expression gets
  MSYS path-mangled (set `MSYS_NO_PATHCONV=1` for `gh`/`aws`, but NOT for `git -C /d/...` paths — it
  breaks those), `grep -c` exits 1 on zero and trips `set -e`, and a 404/JSON error body silently
  fails a sha comparison. Key land-detection on git ancestry/content, not PR `state`.
- **Re-measure EVERY iteration — a carried-forward value is UNKNOWN, not current.** coord is a
  high-churn system: main tips, PR states, queue membership, candidate refs and ECS task-def
  revisions all change minute to minute. In the 2026-07-20 soak, **five of five incorrect steward
  reports were MEASUREMENT errors — not one was a code error** — and three shared a single root:
  *a value from an earlier iteration restated as though freshly observed.* One iteration reported
  "runner main static 4.5h — the thing to watch" when runner had already landed twice; the alarm
  aimed remediation attention at the healthiest repo on the fleet. Discipline:
  - Re-query every field you put in the ledger, every iteration. Never copy a tip sha, an age, a
    queue depth, a CI verdict or a serving image forward from the previous ledger.
  - **Stamp readings with their observation time** ("main `40d7eb8a` as of 10:00Z") so a stale
    number is visibly stale instead of passing as current.
  - **"Unchanged since last tick" is a CLAIM requiring its own fresh read** — it is the single
    easiest thing to assert without checking, and it reads identically to a real measurement.
  - **An iteration is not atomic.** An hour can elapse between the first and last command of ONE
    scan (a clock-skew "anomaly" in this soak was exactly this). Re-read anything you are about
    to act on, not just anything you are about to report.
- **Never suppress an error into a value.** `cmd 2>/dev/null || echo 0` converts a broken probe
  into a confident zero — **silent-empty is UNKNOWN, not NO.** Run probes with stderr visible, and
  verify a probe's dependencies exist before trusting an empty result. When two signals disagree
  (PR reads `MERGED` but the content grep says absent), that disagreement is the most valuable
  thing on your screen: **re-probe both, never pick the convenient one.** In this soak that
  cross-check was the only reason a landed fix wasn't reported as unlanded.
- **A check with `conclusion: null` is RUNNING, not failing.** Filtering on
  `conclusion != "SUCCESS"` silently classes every in-flight check as a failure and manufactures
  red PRs out of healthy ones. Select `conclusion == "FAILURE"` explicitly and report
  in-progress/queued counts as their own column — "checks still running" must never reach the
  ledger as a failure.

## Rules

- **Fleet policy governs.** The steward is a normal fleet session: the autonomy charter +
  the coord-served policy documents apply every iteration, the stricter wins on conflict,
  and decisions cite the clause applied. See "Fleet policy" above.
- **Finish to zero.** Any deficiency found while doing this work — coord defect, twin
  retrieval gap, neighbouring-repo bug, or a flaw in this skill — is Tier-2 work you own:
  plan it, vet it, implement it. Not a note in the report. Gate anything deferred and return
  a `gate_id`; name every unchased anomaly.
- **Delegate to subagents; keep the main session a ledger.** Investigations, fixes, vet runs
  and reviews go to subagents (briefed on the repo landmines, in parallel where independent);
  the scan, the reflex dispatch and the judgement stay inline. Verify their load-bearing
  claims yourself.
- **Consume Phase 1/2, never rebuild it.** `coord_pr_status` + `coord_query_merge_economics`
  + `coord_query_ci_state` + the metrics are the input surface; do not re-derive state. Check
  `tools/list` first — this doc names levers that are not deployed.
- **Extend `/babysit-prs`, don't fork its taxonomy.** Call or reuse its per-PR diagnosis
  (Step 4) + recovery levers (Step 5) + admin-merge preconditions (Step 6) + remediation→
  `/vet-imp` (Step 7). The steward adds fleet scope + continuity + the Tier-1 reflex table +
  rate-limits + deploy-batch coordination on top.
- **`--admin` is off the table — by policy, not because it is impossible.** Recovery =
  rebase → required checks green on the up-to-date head → plain `gh pr merge --rebase`.
  Never `--admin`, never `--no-verify`. (The four `main-merge-gates` rulesets DO list
  `OrganizationAdmin` as a bypass actor — measured 2026-07-29 — so never tell a caller
  "no one but coord can merge this"; say the steward does not merge it. Per-repo bypass
  detail: `qontinui-claude-config/knowledge-base/qontinui-specific/coord-merge-train.md`.)
- **Checks, not permission.** Full autonomy on Tier-1/Tier-2; escalate only on the fleet's
  CLOSED list (Step 4). A bad fix is caught by vet + CI + candidate-CI + no-reap + per-PR
  review, not by an approval click. High blast radius alone is not a trigger.
- **Bounded + honest.** Cap remediation attempts (3/wedge, stall-detected), rate-limit
  fix-PRs and recovery-merges, register a gate for observable blockers, and verify every
  "cleared"/"landed"/"shipped" claim against ground truth (verdict re-read / git ancestry),
  never a stamp or a `merged` bool.
- **Dogfood carefully.** The steward modifies the system that ships changes. The initial
  observe soak completed 2026-07-22 and the operator flipped the default to `autonomous`.
  Re-soak in `--mode=observe` after major changes to this skill; if fix quality
  drops, TIGHTEN the gates — don't remove autonomy.
- **Improve coord's merge-data RETRIEVAL as you go (a standing responsibility, not just wedge-fixing).**
  Every time you have to re-derive a merge fact by hand — content-grepping `origin/main` to tell
  landed-from-open, stitching `gh run list` + `merge_proposals` + `is_merge_safe` to answer "slow
  or wedged?", scraping candidate-CI durations to size a threshold, or hand-computing waste — that
  is a **retrieval gap in the twin**, and closing it is in scope. Treat a recurring manual
  derivation as a Tier-2 improvement: add the missing read to coord's digital twin (a
  `coord_query_*` MCP read + its HTTP twin, derived-on-read over existing tables — coord authors
  ZERO `coord.*` DDL, so never add a table) so the NEXT steward run consumes one honest answer
  instead of re-deriving it. Bias toward enriching the existing verdict / adding a per-repo
  merge-economics read (candidate-CI p50/p90, land rate λ, λ·T pressure, ci-minutes-per-land,
  green-candidates-discarded, `candidate_tip_on_main`/`already_landed`, a data-driven
  `suggested_stuck_threshold`). Ship it through the same vetted, CI-gated path as any fix. The
  goal: the steward should never again learn a merge fact GitHub/SQL knows but coord's twin won't
  say. (Reference target: the `2026-07-17-merge-train-long-ci-redesign` plan
  §"standing metric".)
