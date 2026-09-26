---
description: Autonomous, checks-gated merge-train steward — runs in a visible, stoppable session, continuously watches coord's merge train + all open PRs fleet-wide, auto-remediates known wedge classes deterministically (Tier 1), and autonomously runs root-cause→author→/vet-imp→land on ANY deficiency it finds (Tier 2) — coord defects, twin retrieval gaps, neighbouring-repo bugs, or flaws in this skill itself — with NO human approval click. Runs under the same fleet policy as every other session (autonomy charter + coord-served policy documents), delegates heavy work to subagents to keep the main session a lean ledger, and escalates ONLY on the fleet's closed list.
argument-hint: "[--mode=autonomous|observe] [--repos=r1,r2] [--interval=5m] [--max-recovery-merges=1] [--threshold=45m] [--respond-to-alert=ID] [--once]"
allowed-tools: Read, Write, Edit, Bash, PowerShell, Grep, Glob, Monitor, Skill, ToolSearch, TaskCreate, TaskUpdate, Agent
---

# Merge-train steward — autonomous, checks-gated, visible

This is `/babysit-prs` **generalized**: from *this session's PRs, event-triggered* to
**the whole train, fleet-wide, continuous**, plus a deterministic Tier-1 reflex table,
fleet-level rate-limits, and deploy-batch coordination. It runs as a **visible, stoppable
Claude session** — a `/loop`, or a coord `continuation_spawn` with
`presentation:"terminal"` on the operator's device — so the operator watches every step
live and can kill it any moment. That continuation deliberately carries **no `hint`
brief**: the spawned session runs this skill, so the skill body IS its brief and every
input is re-read from live doors on the first pass. The `hint` rule
(`_gate-registration` → "The brief — a continuation with no `hint` is a fresh agent with
no context") binds a continuation resuming *particular* work; this one re-arms a standing
watch that starts from scratch by design.

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

The steward is **enabled by default** AND is instantly stoppable:

- **`COORD_MERGE_STEWARD_ENABLED` is an OFF switch, not an ON switch** (operator-directed
  2026-08-31). **Unset means ENABLED** — run the iteration. Only an explicit `0` / `false`
  / `no` / `off` (case-insensitive, trimmed) disables it; on that value do nothing this
  iteration, report `steward disabled (COORD_MERGE_STEWARD_ENABLED=<value>)` and stop.
  Any other value, including `1`/`true` and including garbage, is ENABLED — an off switch
  that fails open is the correct direction here, because the failure it must never have is
  silently declining to watch a wedged train.

  ⚠️ **This inverts a real safety property, deliberately — know what was traded.** The old
  default meant a steward launched by anything other than the runner's own launcher found
  the flag unset and refused to act, so an accidental `/merge-train-steward` was inert. It
  is now armed on invocation, in `--mode=autonomous` (the default), which is Tier-1
  remediation + Tier-2 authoring. The operator's rationale is the same as served policy
  `escalation-bar` `do-reversible-mechanical-work`: the cost of a stopped action exceeds
  the cost of repairing a rare wrong one, and every mutation here is still gated by
  `/vet-plan` + CI + candidate CI + the no-reap gate + per-PR review. If you want the old
  inert-by-default behaviour for a session, pass `--mode=observe`; that is now the brake,
  not the env var.

  **Scope: this flag ONLY.** The sibling stewards keep ON-switch semantics and are
  unchanged — `QONTINUI_CLEANUP_STEWARD_ENABLED` (which gates *destructive* reaping) and
  `COORD_DEVOPS_STEWARD_ENABLED`. Do not generalise this inversion to them; the risk
  profiles differ and only this one was operator-directed.
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
   `--once` (single pass, no continuous loop),
   `--respond-to-alert` (a `coord.alerts` row id — run the ALERT-BOUND pass described
   under "Continuous operation" instead of the fleet scan; see that section for the full
   contract).

   ⚠️ **DERIVE the watch set each pass — the hardcoded default above is a FALLBACK, not the
   set.** A literal list is exactly what goes stale, and silently: measured 2026-09-01 against
   `coord_query_train_activity` (**40** coord-authority repos), four repos holding live work sat
   outside the default — `qontinui-devtools` (1 open PR, plus two stale gating REDs on `main`),
   `qontinui-supervisor` (2 open), `qontinui-prm` (1 open), and `qontinui-dev-notes`
   (**48 open non-draft — the largest backlog on the fleet**). So unless `--repos` was passed
   explicitly, **build the watch set from `coord_query_train_activity`**: it returns one row per
   coord-authority repo *including repos with no proposals at all*, which is precisely the
   population a static list cannot contain and the reason a longer hardcoded list is not the fix.
   Fall back to the default only when that read is unavailable, and **say which of the two you
   used** — a pass run off the fallback has a known blind spot and should report it as one.
   ⚠️ Its counts lag even where its row set is right: the same read reported **41** open for
   `qontinui-dev-notes` against a measured **48**. Take the row set as authoritative for WHICH
   repos to watch, and re-count per repo before acting on any number it gives.

   ⚠️ **The merge-authority set is DERIVED each pass — and it is NOT the same question as
   "who lands this repo". Keep the two apart; conflating them is what produced the defect
   below.**

   - **the merge-authority set, which this file also calls the coord-authority set — one
     object, two spellings** — the repos coord's train covers. **This file already carries
     the correct definition**, in item 3's *"Do NOT probe merge authority with a
     `POST /agents/allocate` 409"* warning: `coord.canonical_repos` (tenant/global rows)
     UNION `coord.tenant_repos` — **40 rows on 2026-09-16**, `qontinui-claude-config` among
     them. What follows is that same definition, DERIVED rather than listed. There is not a
     second one, and this file must never grow a third. **It is also the WATCH set**: both
     come off the same read, so they are the same population and neither is "wider".
   - **the lander** — WHO actually closes a PR here, which is a DIFFERENT question and the
     only thing that selects the watch-only remedy below. **The procedure, stated once
     because the steward acts on OPEN PRs that have no landing commit of their own:** read
     the committer census over the repo's recent default-branch history —
     `git log origin/main -100 --format='%cn'` — and classify each commit by the two shapes
     the two-lander census below spells out. Coord is the PRIMARY lander iff coord-shaped
     commits are the majority. That is a per-REPO verdict derived from per-COMMIT evidence;
     the two phrasings elsewhere in this file mean this one procedure.

   **What went wrong.** A six-repo literal
   (`qontinui-web,qontinui-runner,qontinui-coord,qontinui-schemas,qontinui,ui-bridge`) stood
   here as if it answered both, and everything else was called watch-only — *"coord holds no
   merge authority"*, *"a coord proposal that will never come"*. Measured false 2026-09-16:
   `gh pr list --state merged --json number,mergedBy` shows **`app/qontinui-merge-orchestrator`
   — which IS coord**, the same App backing the "Qontinui merge gate" check on the six —
   landing `qontinui-supervisor` (5/5), `qontinui-stack` (5/5), `qontinui-dev-notes` (8/8),
   `qontinui-prm` (1/1) and `qontinui-devtools` (#7, #5; #4 down to #1 were `jspinak`, a transition).

   ⚠️ **`mergedBy` UNDERCOUNTS coord, so a zero from it never establishes watch-only.** A coord
   rebase-fast-forward land rewrites the sha, so GitHub often does not auto-close the PR — this
   file says so itself under the two-lander census below — and such a land never appears in
   `--state merged` with an orchestrator `mergedBy` at all. Measured on ccfg `origin/main`,
   newest 100 commits: **83 `GitHub` / 13 `qontinui-coord` / 4 human**, and the `mergedBy` query
   above saw **none of the 13**. So `mergedBy` is a one-way instrument — a hit proves coord
   lands here, a miss proves nothing — and a repo coord lands EXCLUSIVELY by ff would read as
   zero and be misclassified watch-only, which is this very defect with a different cause. **The
   two-way instrument is the committer census**, `git log origin/main -100 --format='%cn'`,
   read with the two committer shapes the census below spells out.

   That changed the **REMEDY**, not a label: on those five a steward was sent hunting for a
   land mechanism that does not exist and told to skip the coord-side diagnosis that actually
   explains the hold — on that same pass supervisor#192 `action_required` with zero jobs,
   prm#2 `escalate-path-matched`, and stack#80 waiting on a declared cross-repo dep edge
   `upstream_of qontinui-coord#1836`. Finding `b8e2c2c7-35bb-4f24-be4e-6b3c25f92afe`.
   **Do not fix it by widening the literal** — widening is the failure mode, not the remedy.

   **Derive the coord-authority set on a CHAIN, and NAME the rung that answered.** Either rung
   can time out: `coord_query_train_activity` timed out at 30 s twice in the 2026-09-16 pass
   while `train_health` answered, and in the independent vet of this very change the reverse
   happened. Try in order and report which one carried it.

   ⚠️ **Before demoting a rung for timing out, retry it on the OTHER DOOR — a timeout may be
   about the transport rather than the tool, and on 2026-09-16 it was not even stable within
   one hour.** Three readings that day, same call, same arguments: over the runner loopback
   proxy (`http://127.0.0.1:9876/coord-mcp`) one session saw **4 timeouts out of 4** at 30 s
   while a direct `POST https://coord.qontinui.io/mcp` answered **HTTP 200 in 2.57 s**;
   minutes later a second session got a full 40-row payload over **that same loopback proxy**,
   well inside the budget. **So this is an observation, not a mechanism** — nothing here
   measured WHERE the 30 s budget lives, and a paragraph claiming it did would be the
   cause-for-a-measurement substitution this command polices elsewhere. Practically: retry on
   the other door before falling through, and say which door carried it. The direct door needs
   a device JWT — the credential chain is in item 2 of this step, not repeated here.
   1. **`coord_query_train_activity`** — one row per coord-authority repo, each with an
      `authority` object `{canonical, tenant}`; its own `coverage_note` states the row set as
      `canonical_repos` UNION `tenant_repos`. Measured 2026-09-16: `repo_count: 40`, all five
      corrected repos present. ⚠️ **`authority.canonical` is which TABLE the row came from,
      not a capability tier**. It is wrong in BOTH directions: `qontinui` and `ui-bridge` read
      `canonical: false` and have always been in the six, while `qontinui-supervisor` reads
      `canonical: true` and was never in it. Never filter the set on it.
   2. **`coord_query_train_health` → `ci_runner_inventory.registrar.armed_repos`** — note it
      takes a REQUIRED `repo` argument, which bites precisely when rung 1 (the enumeration)
      failed and you hold no repo: pass any repo you already know. 43 entries
      on 2026-09-16, a strict SUPERSET of rung 1's rows (the extra three being
      `portofino-pizzeria/backend`, `portofino-pizzeria/infra` and
      `stefanbleck/Expenses-App`). ⚠️ It is the CI-runner registrar's roster rather than the
      authority table, and it **reports its own freshness** in the same block — `coverage`,
      `refresh.verdict` and `stale_repos`. On 2026-09-16 two reads seven minutes apart
      disagreed on two of those three (`refresh.verdict` `unobserved` then `stale`;
      `stale_repos` 43 then 26), which is exactly why no value from it is pinned here: **read
      the freshness fields yourself and quote what YOU saw.** Take the roster as a WIDENING
      signal and confirm per repo.
   3. **The retired six-repo literal quoted under "What went wrong" — a declared FALLBACK
      only, and note it is NOT the seven-repo `--repos` default in item 1; they differ by
      exactly `qontinui-claude-config`.** A pass run off it has a known
      blind spot and **must report it as one**.

   ⚠️ **Neither rung is a complete enumeration, and rung 1 is credential-scoped.** Finding
   `76f36279-fe65-4273-9c5d-23f1a828f997` measured `coord_query_train_activity` under a
   `personal-jspinak` credential returning 40 rows that OMITTED `portofino-pizzeria/infra` and
   `backend` — two repos coord had in fact ff-landed — with the practical rule that a
   tenant-scoped steward enumerates via `gh repo list <owner>`, not via this read. A repo
   absent from both rungs is UNKNOWN, never out of scope.

   **Watch-only is a LANDER fact, established PER REPO — not the complement of a list, and
   not "coord has no authority here".** A repo is watch-only when coord is not its PRIMARY
   lander; `qontinui-claude-config` is the only measured instance today, and it is the case
   this file has always got right. There the steward scans for stuck and red PRs exactly as
   elsewhere, but the remedy STOPS at making the PR landable and green (**re-run the failed
   run**, rebase a stale merge ref, fix red CI, or close an already-landed empty diff) —
   closing it is the other mechanism's job. **This is a change of remedy, not a lowering of
   the stuck bar:** non-draft + green + unlanded past the threshold is still a wedge, just a
   wedge in *that repo's own* land mechanism.

   ⚠️ **Watch-only does NOT mean coord is absent — and several rows further down in this file
   were written as though it did.** ccfg is coord-authority, and on 2026-09-16
   `coord_query_train_health` answered for it with a full payload:
   `open_pr_backlog.histogram` over 18 open non-draft PRs, `candidate_ci_p90_secs: 78.8`, `last_land_at 2026-09-14T19:54:54Z`, 37
   `coord.scheduler_ticks` rows and 14 `unproposed_branches`; `coord_pr_status` for ccfg#980
   returned a fully populated card, not a thin one. Coord holds a card, a train and thresholds
   even here.

   **Do not trust a COUNT of the affected rows — this file has already been burned by one.**
   Every passage corrected for this carries the literal marker **`WATCH-ONLY CORRECTION
   2026-09-16`**. Before acting on ANY row that opens "on a watch-only repo …", check that it
   carries that marker; a row that does not carries the pre-correction premise and must be
   re-derived against the rule above. `grep -n 'watch-only' .claude/commands/merge-train-steward.md`
   enumerates them in one command, which is the check to run rather than a number to read here.

   **Honest limit on the correction above.** Recent-merge history is evidence about who HAS
   landed, not a read of coord's authority table, and a repo with no recent merges is
   invisible to that method. The five named are a **FLOOR**, not the complete correction —
   the second reason to derive the set rather than paste a longer one.

   `qontinui-claude-config` is watch-only today: coord is not its PRIMARY lander — the
   committer census over the newest 100 commits of `origin/main` reads **83 `GitHub` / 13
   `qontinui-coord` / 4 human**, so coord lands here but `auto-merge.yml` does the bulk — and
   its checked-in mechanism is
   `.github/workflows/auto-merge.yml`, whose own header says it "is NOT coord-managed" and
   which merges with the default `GITHUB_TOKEN`, crediting those lands to
   `github-actions[bot]`. (Coord ff-lands here too — the two-lander census below — so infer
   the lander from the COMMIT, never from the repo.)
   ⚠️ `auto-merge.yml` is **edge-triggered** — it fires
   only on a `lint-frontmatter` `workflow_run` *completing* with `conclusion == 'success'` and
   `event == 'pull_request'`, and nothing re-fires it on a schedule. A cancelled or failed lint
   run therefore strands the PR indefinitely, and only `rerun_failed_jobs` re-arms it: a fresh
   `gh workflow run --ref` dispatch produces `event == 'workflow_dispatch'`, fails that `if:`,
   and **cannot land the PR** no matter how green it goes.
   Since 2026-09-02 a sibling workflow — `.github/workflows/lint-retry.yml` — performs that
   re-run **automatically**, for a `pull_request` lint run. It does not change the trigger or
   the bar: the retry must go green on its own merits before `auto-merge.yml` will look at it.
   Since #916 (2026-09-13) the bound is **class-dependent** — `failure` gets one retry
   (attempt 1 only); any other non-success conclusion (`cancelled`, `timed_out`, … — `skipped`
   and an empty conclusion stay excluded) is retried while `run_attempt < 4` — and a decide step
   (`scripts/lander-classify.sh retry`) stands the retry down when this attempt's failure
   signature REPRODUCES the previous attempt's, at the ceiling, or when the decision could not
   be read, **with a best-effort comment on the PR** in those three cases. It also stands down,
   with NO comment, when the head is no longer an open PR's head (merged, superseded, branch
   deleted) — so a missing comment on a merged or superseded head is expected, not a fault.
   **So the manual re-run remedy above is now the SECOND-line remedy, not the first** — a ccfg
   PR you find stranded behind a red lint has already had its automatic retry, which makes a
   second failure evidence of a real failure rather than of a flake. And a `lint retry standing
   down: … reproduced the previous attempt` comment means a re-run **cannot** help: a re-run
   re-executes the workflow definition the run was created with, so the remedy is a new head
   (rebase and push), never another re-run. Check `run_attempt` before spending one: an
   attempt-1 red on a still-open head that was never retried and carries no stand-down comment
   most likely means `lint-retry.yml` itself did not fire — read its runs for that head (the
   comment is best-effort, so its absence alone is not proof) and chase THAT.

   ⚠️ **And a GREEN lint with an open PR is a class `rerun_failed_jobs` cannot serve at all —
   read the PR's file list before spending a re-arm on it.** `auto-merge.yml` merges with
   `secrets.GITHUB_TOKEN`, and `workflows` is not a key its `permissions:` block can carry, so
   GitHub refuses its `gh pr merge` with
   `refusing to allow a GitHub App to create or update workflow `.github/workflows/<f>` without
   `workflows` permission` — **but only when the squash would have to SYNTHESIZE a workflow blob
   that no authorized push has introduced.** Measured 2026-09-06: `app/github-actions` landed 8
   of 8 recent workflow-touching ccfg PRs (#788 #786 #777 #774 #772 #742 #726 #724, two of them
   edits to `auto-merge.yml` itself), because the merged blob already existed on the head or on a
   coord `merge-candidate/*` ref; the one refusal, #728, touched `lint-frontmatter.yml` after
   `main` had also changed it. So the discriminator is **the PR touches a `.github/workflows/`
   file that `main` has ALSO changed since the PR's merge-base** — a *stale* workflow-touching
   PR, not a workflow-touching PR — and for that head the refusal is deterministic: re-arming the
   edge (lint already green at its latest attempt) re-fires an auto-merge that refuses again. It
   spent one of #728's three bounded attempts before its log was read. **Remedy: the
   *Green-but-dirty* row's "Rebase it"** (any actor with workflow authority — the author or an
   agent — can push the rebase; `auto-merge.yml` cannot, because update-branch under
   GITHUB_TOKEN synthesizes the same blob), or coord's own lane, which lands the PR without a
   rebase once EVERY check on the head is green — it did not take #728 because
   `skill-bundle-parity` was red (coord card `block_reason_code: ci-not-green`). **It is not an
   operator case**: finding `954275c5` refuted that escalation after it held #772 in draft while
   green. Since plan `2026-09-04-ccfg-auto-merge-cannot-land-a-pr-that-touches-a-workflow-file`
   landed, `auto-merge.yml` posts this class on the PR as a typed `auto-merge declined:` comment
   naming the stale files and the rebase; before it, the reason lived only in the failed
   `auto-merge` run's log, which is not attached to the PR as a check.

   ⚠️ **`auto-merge.yml` is NOT the only thing landing this repo, and this file used to
   say it was.** Measured 2026-09-02T05:46Z via
   `git log origin/main --format='%cI %h committer=%cn %s'`: **two concurrent landers**,
   interleaved inside one hour — `d1796d3` 05:42:16Z and `7875d44` 05:21:25Z with
   `committer=GitHub`, against `f2b7367` 05:13:41Z and `fafe9b3` 05:14:56Z with
   `committer=qontinui-coord <coord@qontinui.dev>`. coord states it itself on PR #615:
   *"Landed on `main` by coord as `f2b73676c` (rebased fast-forward; PR head `ea46bfa69`
   differs, so GitHub did not auto-close) — closing."* The retired claim — that lands here are
   credited to `github-actions[bot]` **never** to `app/qontinui-merge-orchestrator` — was
   simply false, and it misdirected two live steward sessions on 2026-09-02.

   **So do not infer the lander from the REPO — infer it from the COMMIT.** That one
   `git log` line separates the two shapes:
   - `committer=GitHub` **with** a trailing `(#N)` ⇒ `auto-merge.yml`'s squash, landing as
     `github-actions[bot]`; the squash rewrites the committer and stamps the PR number.
   - `committer=qontinui-coord <coord@qontinui.dev>`, **no** `(#N)`, and the **original author
     preserved** (`f2b7367` author `t`; `fafe9b3` author `jspinak`) ⇒ a coord
     **rebase-fast-forward** land. The rebase changes the SHA, so GitHub often does not
     auto-close the PR — a coord land can present as a stuck PR that has in fact already
     shipped, which is the *already-landed empty-diff* class below, not a wedge.

   ⚠️ **The two landers differ in a second way that nothing here used to say: only ONE of them
   produces CI on `main`.** `auto-merge.yml` merges with `secrets.GITHUB_TOKEN`, and GitHub
   creates no new workflow runs for events triggered by that token — so **every `committer=GitHub`
   squash lands with zero `push`-event workflow runs at its sha**, while a `committer=qontinui-coord`
   ff-land (a different credential) triggers them normally. Measured 2026-09-05 over the newest
   100 commits of `origin/main`: 95 auto-merge squashes with **0** push runs; 4 coord ff-lands
   and 1 human push with one each. Consequence for a steward reading this repo: `qontinui CI`
   green on ccfg `main` means *"green as of whenever a coord ff-land or a human last pushed"*,
   which at that land rate is roughly one commit in twenty — it does **not** mean the tip is
   verified, and a broken ccfg `main` would read green. That is case **2s** in the red-main
   remedies table below; the repo now carries a `main CI coverage` workflow that reports the gap
   as a number. Plan: `2026-09-04-auto-merge-lands-trigger-no-main-ci`.

   **Honest limit: whether coord landing this repo is NEW behaviour, or this doc was wrong
   from the day it was written, is NOT established.** The evidence above is one hour of
   `origin/main`, not a history. Say UNKNOWN rather than inventing either story.

   **Consequence — a hypothesis, not a proven cause.** Two independent landers on one
   protected `main` share no lease, and a collision surfaces as `mergeStateStatus: BLOCKED`,
   which `gh pr merge` reports client-side as "the base branch policy prohibits the merge"
   while `mergeable` still reads `MERGEABLE`. That is a **plausible mechanism for the BLOCKED
   refusals seen here, not a demonstrated one** — a prior session asserted causation from
   timing alone and was wrong; do not repeat that. It went unseen because `auto-merge.yml`
   polled only `mergeable` (fixed in #619, landed as `7574c9a`, which now rechecks
   `mergeStateStatus` between attempts). **One BLOCKED cause IS demonstrated, and it is not a
   race:** on #909 (2026-09-12, twice) BLOCKED was the stale-workflow-blob refusal surfacing
   at `mergeStateStatus` against a `main` tip 13 hours old. Since #916, `auto-merge.yml`
   classifies a BLOCKED once from API data (`scripts/lander-classify.sh blocked`) before
   spending its poll window — declining at once, on the PR, for a stale workflow blob or a
   refusing branch rule — and its exhaustion decline now says whether `main`'s tip actually
   moved. A decline reading *"main's tip did NOT move … UNEXPLAINED"* is not the race either;
   read the classifier output it quotes before re-arming.

   None of this loosens the watch-only **remedy** rule above: whichever mechanism lands this
   repo, it is still not the steward's, so your remedy still STOPS at making the PR landable
   and green.

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

   > ⚠️ **Probe a second, independent instance before you name a cause for a dead door.**
   > `/coord-revive` hands back a failure TYPE, and a type is a *diagnosis*: "the key is
   > stale", "the port is dead", "coord is down". Each is a claim about a service you
   > reached from one client, on one box, over one transport. Coord is multi-replica by
   > construction — which is why liveness here is `<base>/health` sampled 4–8× looking for
   > exactly one `is_leader:true` — so a second instance is free: sample again and read
   > WHICH replica answered, then cross-check the public door with
   > `curl -sS -o /dev/null -w '%{http_code}\n' https://coord.qontinui.io/health`. A `401`
   > there still proves coord is **served**, which is exactly the claim a local "Command
   > failed with no output" was about to be used to deny; a masked MCP tool is a fact
   > about this session's transport and no evidence at all about the train. **This rung is
   > unconditional** — no credential, no MCP, one `curl` — and it is cheaper than the
   > `/coord-revive` cascade it precedes, so it runs first rather than instead.
   >
   > **Then ask what a peer already found:** `coord_recent_findings` for the
   > `resource_keys` you are about to touch, or the `topic` when you know the subsystem
   > (`coord-mcp`, `pr-merge`) before you know the files. `coord.findings` is
   > pull-by-relevance — nothing pushes a peer's diagnosis at a steward, and a `/loop`
   > steward re-derives the same wedge every iteration without it. Masked tool or dead
   > transport: `GET /coord/agent-findings?resource_keys=…&topic=…&limit=…` (same
   > `findings::recent` behind both doors). ⚠️ The two filters are **OR'd, not AND'd**
   > — coord's `recent` matches *keys-overlap* **OR** *topic-equals* as one
   > disjunction, not a conjunction (qontinui-coord
   > `crates/coord/src/findings.rs`), so passing both **widens** the read.
   >
   > **Then ask what an ANSWERED QUESTION already established:** read the
   > `diagnostic` corpus for the subsystem you are about to touch, exactly as you
   > just read `coord_recent_findings`. Findings expire in ~14 days and are raw;
   > diagnostics are durable, versioned, and carry a **Refutes** section naming a
   > belief that has already been falsified — which is the one you are most likely
   > to re-derive.
   >
   > ```bash
   > # $HDR is a 0600 tempfile holding one line, `Authorization: Bearer <device JWT>`
   > # — never the token on argv (served policy `security-and-autonomy`, credential hygiene)
   > # topic vocabulary is coord.findings' own: merge-engine, pr-merge, coord-mcp, plan-corpus
   > curl -sS -H @"$HDR" \
   >   'https://api.qontinui.io/api/v1/plan-library?kind=diagnostic&q=<topic>&limit=20'
   > ```
   >
   > Skim the **Refutes** sections first, then **Re-run** — a diagnostic's Measured
   > block is a reading, not a fact, and re-running its named probe is cheaper than
   > re-deriving its conclusion. ⚠️ `?q=` is full-text over title and body and does
   > NOT match the slug; the exact door is `&work_unit_slug=<stem>`. A zero result is
   > **UNKNOWN, not absent** — the corpus is young (3 rows at 2026-09-06T08:35Z) and
   > nothing yet tells you it is empty rather than unwritten. Contract, bar and slug
   > convention: `knowledge-base/qontinui-specific/diagnostic-artifacts.md`.
   >
   > **It gates the CAUSE, never the OBSERVATION.** "`coord_pr_status` returned 'Command
   > failed with no output'" is a measurement and belongs in the ledger verbatim. "coord
   > is down", "the train is stopped" are causes — and this steward *acts* on causes, which
   > is why the distinction is operational rather than stylistic: a leaderless outage is a
   > Tier-3 escalation and a hard stop on Tier-1/2 remediation, while a stale nonce is a
   > `/coord-revive` and no escalation at all. **If the second instance cannot be reached
   > either, that is UNKNOWN** — record UNKNOWN plus the two probes you ran, never the
   > mechanism. Measured 2026-09-01 on this fleet: a session held a write door dead for
   > ~5h45m on one local probe while it was live, with a correctly-keyed finding already
   > six hours old (plan
   > `2026-08-28-probe-first-belongs-in-the-diagnosing-commands`).
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

   ⚠️ **The `aws` CLI is not present on every fleet member — DECLARE which read you
   used.** Measured on the Linux box 2026-09-04 and again 2026-09-06: `command -v aws`
   finds nothing, neither `~/.local/bin/aws` nor `/usr/local/bin/aws` exists, and both
   commands above exit `127 command not found`. An absent `aws` is
   **INOPERATIVE-ON-THIS-MACHINE, never a silently skipped item 9** — and never a
   substitution made without saying so.

   The declared fallback is coord's own unauthenticated health surface:

   ```
   curl -sS https://coord.qontinui.io/health | jq -c '{build_sha, built_at}'
   ```

   `build_sha` and `built_at` are **top-level**, while `is_leader` / `holder_id` sit
   under `leader` — so `build_sha` is per-replica-image and NOT leader-gated, which is
   why it survives a follower read where `/metrics` does not. Read it off the **same
   4–8 samples** Step 0's liveness check already takes (see the leader probe above);
   quote it only once it agrees across all of them. Measured 2026-09-06:
   `5e151e6c13f236aeb6620a0a81274b2c6ed13c5a` @ `2026-09-05T20:25:39Z` on four
   consecutive samples, every one served by a follower (`leader.is_leader: false`).

   *Why `/health` and not `/coord/build-info`* — the endpoint the deploy workflow's own
   provenance probe uses (`qontinui-coord` `.github/workflows/deploy-coord.yml`, step `Build-info provenance probe (warning-only)` — re-resolve with `git grep -n 'name: Build-info provenance probe' origin/main -- .github/workflows/deploy-coord.yml`). It is operator-Bearer-gated:
   measured 2026-09-06, it answers `401 {"error":"missing operator Bearer token"}`.
   `/health` is the only unauthenticated door carrying a build sha.

   ⚠️ **What the fallback cannot tell you — from the workflow's own source, not by
   analogy.** `/health` is coord's **self-report**, answered by whichever replica the
   ALB routes to. The workflow's own step (`qontinui-coord` `.github/workflows/deploy-coord.yml`, step `Build-info provenance probe (warning-only)` — re-resolve with `git grep -n 'name: Build-info provenance probe' origin/main -- .github/workflows/deploy-coord.yml`) makes its own build-info probe
   *warning-only* for exactly this reason: *"during/just after the task flip a
   still-draining old task (or a replica lag) can answer with the previous sha even
   though the rollout took."* So the fallback confirms **which build is currently
   answering** and can never confirm **that the intended task definition rolled** — the
   `describe-services` → `describe-task-definition` chain resolves what the SERVICE is
   pointed at, which survives a mid-flip read. Consequences: where the two disagree the
   ECS read wins, and a single `/health` sample taken within minutes of a deploy is the
   one reading you must not quote. Where `aws` is absent, `/health` IS the honest
   answer — and the ledger says which read produced the number.

## The per-pass CHECKLIST — every iteration, before anything else

Steps 0-4 below say HOW to do the work. This says WHAT must be LOOKED AT, so a pass
cannot quietly skip a surface. **Tick every line explicitly in the iteration ledger.**
An unticked line is **UNKNOWN, never "fine"**, and a read that failed is named as the
read that failed — absence is never health
[policy: `verification-and-evidence` `silent-empty-is-unknown`].

This list exists because a full overnight soak ran ~12 iterations, reported the fleet
accurately on every surface it consulted, and was still blind to four repos with 23 open
PRs — because nothing told it to look. Every item below is a surface some pass has
actually missed.

- [ ] **1. Enablement + kill-switch.** `COORD_MERGE_STEWARD_ENABLED` is an OFF switch —
      confirm it is NOT set to `0`/`false`/`no`/`off` (unset = enabled); mode
      (`autonomous` / `observe`) stated in the ledger.

- [ ] **2. CARRY-OVER: were the LAST pass's findings actually ADDRESSED?**
      For every deficiency the previous iteration named, state its CURRENT disposition —
      not what you intended, what is true now:
      **plan written → vetted → implemented → PR open → landed → SERVING**, or a
      completed Tier-1 remediation, or a registered gate with a returned `gate_id`.
      **Naming a deficiency is not addressing it, and neither is authoring a plan that
      was never implemented.** A finding that reappears in three consecutive ledgers with
      no plan behind it is itself the defect — escalate it as one. Carry the list forward
      verbatim until each item reaches a terminal state; a deficiency that silently stops
      being mentioned is a DROPPED item, which is the exact failure `/unattended` exists
      to catch.

- [ ] **3. THE TRAIN TAB — `https://qontinui.io/admin/coord/pipeline`.**
      **This is not the same read as `/pr-merge/health`, and the difference is the whole
      point of this line.** `GET /pr-merge/health` lists a repo only when it HAS a
      proposal (`slots.repos[]`) or a ready PR (`ready_unmerged`). A repo with open
      non-draft PRs and NO proposal at all appears in **neither**, so it is structurally
      invisible to that read. The Train tab is built from
      `buildRepoTrainRows(proposals, prs, health)` in
      `qontinui-web/frontend/src/components/operations/trainActivity.ts` — note it takes
      the **PR list** as an input, which is precisely why it can show what health cannot.

      Read, per repo: the **activity kind** and dwell, and the **ranked pause reasons**.

      ⚠️ **`kind: "idle"` means ONLY "no proposal in flight". It is NOT a health verdict.**
      The reasons are what separate benign from wedged:
      - idle **with zero open non-draft PRs** → benign (`no-candidates`, severity `info`).
        Do not flag it; a detector that cries wolf here gets ignored.
      - idle **WITH open non-draft PRs** → that is the signal. Ask why nothing is proposed.
      Reason vocabulary to expect: `no-candidates`, `no-ci-runners`, `repo-cap-starved`,
      `slots-saturated`, `main-red`, `suppressed-train`, `orchestrator-stalled`,
      `leader-lease-stale`, `hydration-stale`, `dry-run-freeze`, `already_terminal`,
      `removes-referenced-export`, `unrecognized-status`, `web_error`, `healthy`.
      Note the fleet banner **`suppressed-train`** — "coord is deciding and then not
      executing — the train is suppressed, not idle" — which fires when nothing has landed
      for a long dwell while the predicate is still evaluating and PRs are ready. That
      condition is invisible in the per-repo view.

      **Until a served per-repo activity read exists, compute the census by hand** — it is
      three cheap commands and it is the only thing that catches this class:
      enumerate coord-authority repos, count **open non-draft** PRs per repo, and diff that
      set against `slots.repos[]` plus `ready_unmerged` from `/pr-merge/health`. Any repo
      with open non-draft PRs and no train presence is an idle-with-work repo: name it,
      get its block reason, and dispose of it.

      Measured 2026-08-28T08:32Z, the reason this item exists: only `qontinui-runner` and
      `qontinui-coord` had any train activity, while `qontinui-dev-notes` (14),
      `qontinui-web` (6), `qontinui-supervisor` (2) and `qontinui-schemas` (1) sat idle
      holding **23 non-draft PRs between them**. Two of those repos were not in the default
      `--repos` watch set at all — so **check the watch set against the repos coord
      actually has authority over**, not against the default list.

      (The route was RENAMED from `/admin/coord/fleet` to `/admin/coord/pipeline`; there is
      a `route-rename.test.ts` beside the page. If a pointer anywhere still says `fleet`,
      it is stale.)

      ⚠️ **TRANSPORT REALITY — an agent session CANNOT perform the read this doc mandates.**
      `GET /pr-merge/health` is TENANT-SCOPED and returns **HTTP 403
      `{"error":"tenant_not_resolved"}`** to a device JWT and to an agent JWT alike (measured
      2026-08-28; the gate is coord's `auth.rs`). It answers only for an **operator Cognito
      bearer** minted from SSM — an OPERATOR credential, not one a session holds by default.
      A steward that reports having read it should be able to say which credential carried it;
      if the answer is "a device JWT", the read did not happen and the result is UNKNOWN.
      The agent-reachable substitutes, in order:
      - **`coord_query_train_activity`** — the per-repo activity + reason read built for exactly
        this gap (coord#1682, plan `2026-08-28-coord-train-activity-retrieval-gap`). Prefer it
        once it is SERVING; check that, not just that it landed.
      - **`coord_query_train_health`** — reachable today, **but do not trust its idle arm.**
        Measured 2026-08-28 against `qontinui-dev-notes`: it returned
        `is_making_progress: true` with `why: "nothing to land — queue is empty"` while the
        SAME payload carried **9 stranded PRs**, one conflicted for 7.6 days. It keyed the idle
        verdict on `open_proposals == 0` alone and had no open-PR input at all. For
        `qontinui-supervisor` and `qontinui-schemas` even `stranded_prs` was empty, so **no
        field anywhere named their backlog**. Read `stranded_prs` yourself and cross-check
        against `gh pr list`; a green `is_making_progress` on a repo with open PRs and no
        proposals is exactly the false-healthy this checklist exists to catch.

      ⚠️ **Do NOT probe merge authority with a `POST /agents/allocate` 409.** That checks
      `coord.canonical_repos` ALONE (5 repos as of 2026-09-16), while merge authority is
      `canonical_repos` UNION `tenant_repos` (40 as of the same date — both are live counts
      off a mutable table, so re-read them rather than quoting these). A repo can 409 on allocate and still be a
      full coord-authority repo with a complete train-health payload — `qontinui-claude-config`
      does exactly that. The steward drew the wrong conclusion from this on 2026-08-28. Probe
      with `train-health` itself instead, and note that among the nine repos examined the
      "not a coord-authority repo" class was **empty** — so do not reach for it as an
      explanation before establishing it.

      ⚠️ **The console view has the same blind spot in a different shape.** `trainActivity.ts`
      has NO reason code for "not a coord-authority repo", and for an unhydrated repo it
      renders **no row at all** (the row set is seeded from the proposal/hydration maps). So an
      absent row on the Train tab is UNKNOWN, never health — the same rule as everywhere else
      here, applied to a UI.

- [ ] **4. RED MAIN, every repo in the watch set.** Highest-severity signal; nothing else
      in the scan detects it. Never report a repo green while any `RED(...)` or
      `UNKNOWN@<tip>` line stands. Quote `excluded:` lines alongside a green verdict.
      **Order the sweep, and stop it on a primary exhaustion.** The GitHub API budget is
      per-ACCOUNT, so the repos read LAST are the ones that lose their verdict when it runs
      out — and 2026-08-25 is what that costs: coord and runner were read back to back, the
      account hit 5000/5000, and `qontinui-web` (21 open PRs, the most on the fleet) got no
      verdict at all. So (a) **read the highest-open-PR repos first**, plus any repo whose
      train is already held, so a shortfall costs the cheapest verdict rather than an
      arbitrary one; (b) **on a `red_main` exit of `2` whose line names PRIMARY exhaustion,
      STOP** — every remaining repo will fail identically, and GitHub warns that continuing to
      call while limited risks the account. A SECONDARY throttle is the opposite: back off the
      stated seconds, drop `RED_MAIN_PARALLEL`, continue. **A repo you never read is UNKNOWN
      and must appear in the report as UNKNOWN, naming the reset instant from the throttle
      line** — an omitted repo is the one way this item can read as OK while a red main sits
      unwatched. Full reasoning at "Sweep the repos so the budget survives the sweep".
      **Advisory streaks** (at most every 6 h, not every pass): run the schedule-red-streak
      detector per repo — see "Advisory STREAKS" under the case table in Step 1.

- [ ] **5. LAND-vs-RED CONTRADICTION.** For every repo where item 4 reports a **gating**
      RED, ask what that repo has actually landed. A gating RED asserts *"coord will not
      enqueue for this repo"*; a train that keeps landing asserts the opposite. **Both
      cannot be true, and nothing in this loop compared them until this line existed** —
      which is exactly how a permanent false `RED(cancelled)` survived five iterations,
      producing a frightening line and no consequence on every one of them. `advisory:`
      lines are out of scope by construction: they never claimed to hold anything.

      **The two land reads, and what each cannot see.**
      - `coord_query_train_health` → `last_land_at` / `seconds_since_land`. ⚠️ That field is
        `max(merge_proposals.merged_at) FILTER (WHERE status = 'merged')`
        (`pr_merge/train_health.rs`, `proposal_health_facts`), so it sees **coord proposal
        lands only** — a direct push, a hand merge, or a shadow-mode repo (which never
        writes `merged_at` at all) leaves it NULL or frozen. A null or stale `last_land_at`
        is **UNKNOWN, never "nothing landed"** [policy: `verification-and-evidence`
        `silent-empty-is-unknown`], so this read can CONFIRM a contradiction and can never
        clear one.
      - **Did `main` move at all?** Compare the tip SHA against the one the previous pass
        recorded, and — for a RED you are investigating — read `ahead_by` from the compare
        call below. `red_main` already fetches `repos/<r>/commits/main` for the tip sha, so
        the comparison is free. This read survives coord's sha-rewriting ff-land, which
        `gh pr list --state merged` does not: that land closes the PR as `CLOSED` with
        `mergedAt: null` and never appears there (`coord-ff-lands.md`). What it CANNOT
        separate is a train land from a direct push — that is what the first read is for.

        ⚠️ **Do NOT compute the tip's AGE from `.commit.committer.date`. On a coord-landed
        repo that field is the CANDIDATE REBASE instant, not the land, and it under-states
        the tip's recency by up to a full candidate-CI duration.** This bullet used to
        prescribe exactly that read and call it *"land-shape independent"*; it is the
        opposite — the field's meaning is decided by the land shape, and the two shapes on
        this fleet disagree by tens of minutes to days:

        | Land shape | `committer` | `committer.date` means |
        |---|---|---|
        | `auto-merge.yml` squash (ccfg) | `GitHub` | the LAND. Measured 2026-09-06 on the newest 5 ccfg commits: `committer.date == author.date` exactly, on every one |
        | coord rebase-ff-land (every merge-authority repo) | `qontinui-coord` | the instant coord REBASED the candidate. The land happens after candidate CI — coord's own `candidate_ci_p90_secs` was **2567s (~43m)** at the time of measurement |

        **Two measured consequences, both of which manufacture a FALSE staleness alarm:**

        1. **A whole proposal shares ONE committer date**, so a multi-commit land gives the
           tip the age of the oldest candidate build rather than of the land. Measured
           2026-09-06 on `qontinui-coord`: `d0e1fe7d`, `14a68f17` and `d42ca974` all carry
           `cd=12:46:04Z` while their author dates span `10:45:24Z`–`12:07:01Z`. Same shape
           on `qontinui-runner` (`4d06204f` and `d3ed10be` both `cd=22:57:06Z`), where the
           widest measured author→committer gap was **4 days** (`44fbea4e`: `ad` 2026-09-01
           `21:47:56Z`, `cd` 2026-09-05 `22:57:06Z`).
        2. **The committer date can PRECEDE the instant its own predecessor was still tip** —
           which is the decisive proof that it is not a land clock. Measured 2026-09-06: this
           steward's own `red_main` sweep read `a84b8f9e` as the coord tip at **13:05:54Z**;
           its successor `d42ca974` carries `cd=12:46:04Z`, **19 minutes earlier**. In the
           same window coord's `last_land_at` read `13:10:48Z` against that `12:46:04Z` tip
           date — a ~25-minute gap between the tip's stamp and the land it belongs to.

        So a steward computing "main has not moved in N minutes" from this field will read a
        train that just landed as a stalled one. **The land clock on a coord repo is
        `coord_query_train_health` → `last_land_at`** (with the null/frozen caveat the bullet
        above already states); the *movement* question is answered by the SHA comparison and
        by `ahead_by`, neither of which needs a timestamp at all.

      **Compare against the RED's own sha, not against "now".** A land that PREDATES the red
      proves nothing. The question is how far `main` has advanced *since the sha the RED is
      reported at* — `gh api repos/<r>/compare/<red-sha>...main --jq '.status,.ahead_by'`,
      one call, and only for a RED you are already investigating. ⚠️ Read `status` as well as
      the count: compare is MERGE-BASE relative, so on an abandoned sha that is not an
      ancestor of `main` (exactly the shape a superseded run sits on) `status` reads
      `diverged` and `ahead_by` counts from the merge base, not from that sha. It is still
      the right order-of-magnitude answer to *"has the train moved since?"*, but it is not a
      commit distance from the red sha, and must not be reported as one.

      ⚠️ **One land straight after a red appears is NORMAL — do not report it as a
      contradiction.** coord's `main-red` block is an **ENQUEUE** gate and is not
      re-consulted at land time; the optimism gate in `merge_scheduler.rs` says so in its own
      refusal message (*"the ENQUEUE `main-red` block is NOT re-consulted at land; this is
      the only land-time main-greenness read"*). Proposals already in flight when main went
      red land past it by design. **The contradiction shape is a red that STANDS while lands
      KEEP COMING** — across more than one pass, or with `ahead_by` climbing between passes.

      **When it fires, one of the two is wrong. Investigate BEFORE reporting**, and name
      which it was:
      1. **The verdict class is wrong for that conclusion.** The measured instance:
         `RED(cancelled)@42ea7611` reported as gating on `qontinui-runner` 2026-08-31 while
         the repo landed PRs continuously. `cancelled`/`stale` are SUPERSEDED, not RED — see
         the superseded note in the verdict vocabulary.
      2. **This detector calls a workflow gating that coord's baseline does not — the
         likeliest cause of the NEXT instance.** Its gating test is
         `establishes_main_baseline` alone (the workflow has a `push` run on main). coord
         applies two further filters this detector has no equivalent of:
         `workflow_counts_for_main_red` (drops benign-supersession and one-shot-dynamic
         workflows) and `failing_workflow_is_required` (a REQUIRED-context join fed by
         `required_checks_cached`), both in `ci_baseline.rs`. So a failing **non-required
         advisory** workflow on main is a gating RED here and green to coord, and the train
         keeps running. ⚠️ That second filter is FAIL-CLOSED and only ever NARROWS the red
         set on positive evidence: an unreadable or empty required set, or a
         `failure_pattern.jobs[]` that is absent, empty or names no non-passing job, all
         return `true` and keep the workflow counting. So the divergence exists only where
         coord could actually read a non-empty required set AND name the failing jobs — do
         not reach for this explanation before establishing that.
      3. **`main` moved but the TRAIN did not** — a direct push, not a land. Read
         `last_land_at` before calling it a landing train.
      4. **The red is genuine and the lands predate it** — the comparison was made against
         "now" instead of against the red's sha. Redo it.

      It is a free consistency check over data the pass already holds, and it is what would
      have caught the 2026-08-31 defect on the first iteration instead of the fifth.

- [ ] **6. Per-PR twin cards** for every open non-draft PR in the watch set —
      `coord_pr_status`, plus `changedFiles` for the empty-diff class. Read `confidence`
      and `last_verified_at`; a stale card is stale, not absent.

      ⚠️ **Report non-`none` `rebase_block`s SPLIT BY `rebase_block_disposition`, never
      as one "blocked" bucket.** Three counts in the ledger — `coord_retries`,
      `author_acts`, `terminal` — plus `unknown` named individually with its PR numbers.
      A single bucket cannot be read: it mixes a population coord is actively re-cutting
      by itself with one that will sit forever until somebody pushes or closes, and a
      steward looking at the total cannot see that part of it is SELF-CLEARING. That is
      how *"coord holds `rebase_block: rebase_ci_failed`, so it will not land"* reached an
      operator-facing ledger about a PR coord re-proposed ~17 minutes later
      (`qontinui-runner#1387`, 2026-09-06).

      What each count obliges:
      - **`coord_retries`** → **no steward action, and say so.** Coord re-enqueues these
        unprompted. Do not rebase, do not force-push, do not close, do not open a
        remediation plan for the block itself. It is a legitimate ledger line only as a
        COUNT with a dwell — a `coord_retries` PR whose `rebase_checked_at` has not moved
        across several passes is a *different* finding (the retry itself has stalled, or
        `ContentConflictCap`'s 6h valve is throttling it) and is Tier 2, not an arm-2
        close.
      - **`author_acts`** → the green-but-dirty / stranded-conflict rows above own these.
        ⚠️ **This is the fleet's biggest bucket and this split alone does not triage it.**
        `RebaseBlockDisposition::of` sends every `conflicting_head` PR here — 81 of 174 open
        non-draft PRs on 2026-09-17 — so `author_acts=<n>` is the same one number under a new
        name, on an axis (WHO acts) that cannot separate a 90-minute conflict from a 28-day
        strand. The **CONFLICT ledger line** below is mandatory for exactly this reason: it
        carries the axis this split does not.
      - **`terminal`** → arm 2's population (`already_landed` only; `empty_candidate` and
        `empty_candidate_superseded` are explicitly *not* closeable on coord's say-so, and
        `stranded_on_absent_parent` — also `terminal` in `RebaseBlockDisposition::of`, since
        the PR is built on work that never landed — is a base refusal the stranded rows
        above own, never an arm-2 close). Re-derive the membership by READING the match arms
        of `fn of` under `git grep -n 'impl RebaseBlockDisposition' origin/main --
        crates/coord/src/mcp/tools.rs`, not from this list — and not by grepping for
        `Some(Self::Terminal)`, which prints only the LAST line of an or-pattern and so
        showed two of the four members (dropping `already_landed`) on 2026-09-26.
      - **`unknown`** → Tier 2 diagnosis, never a default disposition for a card that
        simply came from an older coord build. A card carrying NO
        `rebase_block_disposition` key at all is UNKNOWN — the same reading the
        already-landed row's ARM 3 gives an absent `land_stamp`, because an absent
        field satisfies every denylist.
      Cross-check the split against `coord_query_train_health`'s `stranded_prs[].reason`,
      which is the SAME token set from the same shared ladder
      (`outbound_git::classify_conflict_cause`).

- [ ] **7. Fleet metrics + economics.** `/metrics` scraped x10, **max per series** (a
      follower renders every series as `0`). Counters need **two spaced samples** before
      "flat" means anything. Cross-check `pr_state_stale_backlog` against
      `/pr-merge/health`, which is the authoritative cluster-consistent twin.

- [ ] **8. CI CAPACITY.** `online_ci_runners` and `coord_ci_runner_count{status=...}`
      against GitHub's own runner list. The slot cap is dynamic, so an offline runner
      silently lowers `effective_cap` and starves every repo at once — and that shows up
      downstream as repos going **idle**, which is item 3.

      ⚠️ **"a runner went offline" is a CAUSE — probe a second, independent instance
      before you write it.** `online_ci_runners` falling is the observation; *which*
      runner dropped, and whether any did, is settled on the other side of the
      registration by GitHub's own list (`gh api /repos/{owner}/{repo}/actions/runners`,
      or the org route) — which is why this item names that cross-check rather than the
      metric alone. The metric by itself cannot carry the claim: a `/metrics` scrape that
      lands on a FOLLOWER renders every series as `0`, so "zero online runners" is
      indistinguishable from a total capacity outage until a second read says otherwise.
      Where coord answers, run `coord_recent_findings` on the CI topic before authoring a
      Tier-2 fix for a starvation a peer diagnosed an hour ago.

- [ ] **9. LANDED IS NOT SERVING.** For every fix this session claims to have shipped,
      verify the ECS task-definition image tag and test ancestry against it. A green
      "Deploy coord" run is debounced and proves nothing. A defect whose fix has landed but
      is not serving is **still live in production** and stays on the carry-over list at
      item 2. **Where `aws` is absent, tick this line off the declared `/health`
      fallback and SAY SO** (Step 0 item 4) — an absent `aws` is
      INOPERATIVE-ON-THIS-MACHINE, never a skipped item, and a serving sha quoted with
      no named source is the silent substitution this command polices everywhere else.
      **A serving sha standing still is usually the DEBOUNCE, and it is bounded** — read
      the newest `Deploy coord` run's JOBS, not its conclusion, before calling it an
      anomaly (Guardrails, "A landed fix is NOT a serving fix").

- [ ] **10. Gates and findings opened by this session** — every `gate_id` read BACK
      (`has_continuation` and `will_dispatch` read together), every finding with a returned
      `finding_id`. ⚠️ A gate continuation is written at REGISTRATION time and executed
      LATER against a repository that has moved: re-read that any continuation you armed is
      still the correct instruction, and make its first step a measurement rather than an
      action.

- [ ] **11. DARK CAPABILITIES WHOSE CONDITION IS ALREADY MET.** One read:
      `coord_flag_states` (no arguments), then the filter
      `effective_state == "off" && enabling_condition_met == true`, pre-counted for you
      as `summary.off_but_enabling_condition_met`.

      **Each such flag is a capability whose safeguards are all built and which nobody
      has switched on** — served policy `engineering-priorities`
      `capability-ships-enabled`: *"a capability nobody switched on is a capability the
      product does not have."* Report the list, not the count alone; for each, name its
      `dark_arm` (the recorded justification) and say whether that justification still
      holds now that its condition is met. An arm that has been discharged is an
      **arming candidate to surface with a recommendation**, never something this
      session arms itself — arming stays an IaC edit plus an operator blast-radius call.

      ⚠️ **Three ways to misread this line, all of them measured on 2026-09-06:**
      - **`summary.off` is NOT this number and overstates it badly.** 34 flags read
        `off`; 19 of those are numeric override knobs (`kind: "tunable"`) whose `off`
        means *no override in effect*, which is a configured state and not a dark
        feature. Filter on `kind == "capability"` before counting anything.
      - **`enabling_condition_met: "unknown"` is not `false`.** It is the string
        `unknown` and it means a conjunct could not be observed from coord's runtime.
        Treat it as a blind spot to name in the ledger, never as "arming this is
        pointless" [policy: `verification-and-evidence`
        `unknown-must-not-render-as-a-default`].
      - **This read evaluates against the SERVING build, not `origin/main`.** A
        capability enabled on main and dark in the serving binary is a **deploy gap**,
        not a dark capability — it is not one of the clause's arms and must not be
        reported as one. Cross-check `coord_query_health` →
        `surfaces[0].components.build` (`in_sync`, `serving_sha`) before you write the
        row; on 2026-09-06 the serving build was 13 commits behind and
        `Defaults::AUTO_FIX_RED_MAIN` was `true` on main and `false` in the binary
        being queried. This is the same distinction item 9 draws for a fix, applied to
        a flag.

      ⚠️ **The agent principal does NOT reach this read either — so ATTEMPT it, and on the
      refusal tick the item as UNKNOWN with the cascade EXHAUSTED.** This file used to say
      `coord_flag_states` was off the `device` principal's allow-set but ON the **agent**
      principal's, and sent you to the token from `POST /agents/allocate`. Measured false
      2026-09-16: an agent credential used against `POST https://coord.qontinui.io/mcp`
      answered verbatim — *"`coord_flag_states` exists but is NOT on this caller's MCP
      allow-set (principal_kind=agent) — and no HTTP route is registered as serving the same
      core"* — carrying `{"error":"tool_not_available_to_principal",
      "principal_kind":"agent","tool_exists":true}`. **The second clause is what closes the
      cascade**: there is no REST twin to fall back to, so the device refusal and the agent
      refusal are the whole door set an agent session can reach.

      ⚠️ **One residual, stated rather than papered over.** That measurement was taken with a
      credential from the anonymous `POST https://coord.qontinui.io/agents/credential` mint,
      **not** from the `POST /agents/allocate` token the retired sentence named. Both yield an
      agent-principal JWT and coord refused on `principal_kind=agent` — a property of the
      principal, not of the mint — so the refusal is expected to reproduce on the other door,
      but that door was NOT exercised. If you have an `/agents/allocate` token to hand, spend
      it here and record what it answers; until someone does, treat "no agent-holdable
      principal reaches it" as strongly evidenced rather than exhaustively tested.

      **Do not read that as a pinned value** [policy: `memory-and-notes`
      `never-pin-a-mutable-policy-value`] — an allow-set is mutable, and the claim this
      paragraph replaces is the worked example of what a pin costs. **Issue the call.** If
      it answers, tick the item against the real list. If it refuses, tick it **UNKNOWN —
      cascade exhausted**, quoting the refusal text and the `principal_kind` that produced
      it. That IS a ticked line under this checklist's own preamble — *"a read that failed
      is named as the read that failed"* — and it is what separates a NAMED blind spot from
      silence. A `-32601` from the local `/coord-mcp` proxy reports the RUNNING runner
      build's allowlist and is evidence about neither principal.

      ⚠️ **SURFACE the boundary; do not absorb it.** A checklist line this command mandates
      that no agent principal can satisfy is a coord-side gap — either `coord_flag_states`
      belongs on the agent allow-set, or an HTTP route should serve the same core. Carry it
      as a **Tier-2 deficiency** with the measured refusal attached. A steward pass does not
      change coord's allow-set and does not arm anything.

      The standing census this line samples against — 63 dark capabilities, 31 of them
      carrying no justification the clause recognises, and 17 that are not in the flag
      registry at all — is the `diagnostic` artifact
      `2026-09-06-coord-dark-capability-census`. ⚠️ **It is a dated SNAPSHOT of a
      per-serving-build quantity, never a substitute for the read** — its own third trap
      (serving build vs `origin/main`) is what makes an old count unusable as a current one.
      While the refusal above stands a steward cannot refresh it from any door either, so it
      can never tick this item, and a disagreeing count read from it is UNKNOWN rather than
      a correction. Where the read DOES answer, a count that disagrees with the census has
      either moved (say so) or been read wrong (check the three traps above).

- [ ] **12. THE AGENT ALERT QUEUE — `merge_train` domain.** Coord now serves alerts as
      claimable agent work: `coord_alert_queue {domain: "merge_train"}`, or its HTTP twin
      `GET /coord/alerts/queue?domain=merge_train` (plan
      `2026-09-18-notifications-are-agent-actions-and-alerts-are-agent-work` Phase 2).
      Until that plan this command read no alerts at all, so a merge-train condition coord
      had already detected — and paged — reached no steward. The protocol is stated once, in
      `qontinui-claude-config/knowledge-base/qontinui-specific/coord-gates-and-access.md`
      -> "The agent alert work queue — claim before you act". Here:
      - **Claim before acting** (`coord_alert_claim {alert_id}`) — before a Tier-1 reflex
        or a Tier-2 fix touches the condition a row names — and **act only when the
        answer's `status` is `claimed`**. `claimed_by_other` (record `claimed_by` and
        `claim_expires_at`), `alert_resolved`, `not_agent_work` and `not_found` all mean
        leave it, and so does a `renewed: true` on a row not on this session's claimed
        list — a peer sharing your holder label, whose lease you must never release. Full
        table: the KB section named above, "The claim answer — act only on `status: claimed`".
        Record the echoed `claimed_by`; **release** (`coord_alert_release`) when done or
        handed off, or let the lease lapse. An `observe` pass claims nothing.
      - **Claim mechanics** (KB -> "The claim answer — act only on `status: claimed`"): add
        the alert id to this session's claimed list when the claim is SENT, not when it
        answers, so a claim retried after a `5xx` that comes back `claimed, renewed: true`
        reads as yours; a `claimed_by` of `device:<d>:session:<your own session>` is always
        yours. Release over the door you claimed through; a `claimed_by_other` whose
        `claimed_by` equals the label you recorded is your own lease under another label —
        release again over that door, or let it lapse if that door is unavailable, never a
        third door. `tool_not_available_to_principal` (with an `alternate_door`) means use
        the HTTP twin it names; it is not a fallback to `/coord/alerts`.
      - **A row never triggers a Tier-1 reflex by itself.** It names a condition coord
        detected; the per-PR scan re-confirms the wedge class from its own reads before
        any reflex fires.
      - **Claiming never resolves** — coord closes the row when it re-observes the
        condition clear, so tick an acted-on row as *"claimed, fix landed, awaiting
        re-observation"*, never as closed.
      - **An empty queue is not a healthy train.** It is empty only when `total_count`
        reads `0` on page 1: `count` is the page length, `total_count` is `null`
        (UNKNOWN) on a continuation page or a failed count, and a non-null `next_cursor`
        means more pages [policy: `verification-and-evidence`
        `silent-empty-is-unknown`]. The queue also shares `/coord/alerts`' per-principal
        visibility.
      - **Fall back only on the three answers the KB names**: coord refuses the tool as
        unknown, the route answers `404`, or the body or tool error names
        `schema_migration_pending` (KB -> "Before coord serves it — the fallback, and
        what is NOT a fallback"). Then read `GET /coord/alerts` filtered by repeated
        `?kind=` over the merge-train kinds this pass is weighing (`pr_merge_stuck`,
        `coord_lost_land`, …), read `unknown_kinds` on page 1, and **tick the line as
        carried by the fallback**. Nothing is claimable there, so act only on what the
        per-PR scan confirms independently. Any other `5xx` is transient — retry (a retried claim is already on your claimed list — claim mechanics above),
        do not fall back. A `-32601` from the local `/coord-mcp` proxy is the runner's
        allowlist, not coord — try the HTTP twin first.

- [ ] **13. WHY-ISN'T-THIS-LANDING FORENSICS — the two censuses only a GitHub-vs-twin diff
      sees.** Protocol, remedies and measured instances: "Standing duty — why isn't this
      landing" below. Every pass, every repo in the watch set:
      - **HIDDEN-LANDED census.** Diff the non-draft OPEN PRs `gh pr list` returns against
        the membership of `GET /pr-merge/prs`. A PR open on GitHub and ABSENT from coord's
        list is invisible to every open-PR reader coord has — the phantom-open closer
        included — so this diff is the only read that sees the class. Measured 2026-09-23:
        40 runner PRs (`#1589`–`#1684`) open, labelled `coord:landed`, missing from coord's
        list.
      - **STACK census.** Every `base-not-default` PR: resolve its parent by the child's base
        branch (`gh pr list --head <base> --state all`) and read the parent's state and card.
        A CLOSED or coord-landed parent means a stranded child, not a healthy stack.
      - **Any non-draft PR green or `CLEAN` and unlanded past `--threshold`** gets the
        forensics protocol — cause, Tier-1 remedy with proof, and a Tier-2 plan when the
        class can recur. A "why" with no structural plan behind a recurring class is an
        unticked line.
      Ledger both as the `HIDDEN-LANDED` and `STACK` lines (Report section). **A census whose
      read failed is `UNKNOWN`, never `0`** [policy: `verification-and-evidence`
      `silent-empty-is-unknown`].

## Step 1 — Fleet scan (each iteration)

Read coord's own honest view — no new observability:

- ⚠️ **READ THE OPERATOR DASHBOARD FIRST — `GET <base>/pr-merge/health`. This is REQUIRED,
  every iteration, and it is the read this skill spent months not naming.** It is the Train
  tab of `https://qontinui.io/admin/coord/pipeline`, and it carries three things the per-repo
  twin reads DO NOT: `ready_unmerged` (every open PR that passes coord's DISPATCH filter
  but has not landed — **including PRs coord is actively blocking** — each row carrying
  coord's own persisted verdict and **`latest_proposal_error` verbatim**; contract below),
  fleet-wide `slots` (occupancy vs cap, per-repo at-cap flags), and `pr_state_stale_backlog`.

  ⚠️ **`ready_unmerged.count` / `max_age_seconds` are NOT "ready" figures — read the
  predicate-accurate split beside them.** Membership is coord's dispatch filter
  (`looks_ready`), which admits a PR whose CI aggregate is RED whenever its required checks
  are satisfied. On 2026-08-26 six qontinui-web PRs coord had been blocking `ci-not-green`
  for two days were published here as "ready" (plan
  `2026-08-26-coord-pr-merge-health-ready-unmerged-over-reports`). Since coord #1758 the
  object carries the honest split — read THESE, not `count`, for "how much is stuck-ready":
  - `genuinely_ready_count` — rows whose persisted `predicate_eval` verdict PASSES.
  - `blocked_count` — rows coord affirmatively blocks (it also fails closed on a verdict
    value coord does not recognise). `by_block_reason` maps `verdict_block_reason_code` →
    count over blocked rows; a blocked row with no code is bucketed under `"unknown"`.
  - `unevaluated_count` — rows coord has **never evaluated**. This is **UNKNOWN: never
    count it as ready and never count it as blocked.** Report it as its own number. A
    fleet-wide evaluation outage shows up here and nowhere else; folding it into "ready"
    re-creates the phantom-stuck-PR chase, folding it into "blocked" hides the outage.
  - `unblocked_max_age_seconds` — the max age over genuinely-ready rows ONLY: the honest
    stuck-PR signal. It is **`null`, not 0, when no row is genuinely ready** — render
    `null` as "nothing to measure" ONLY when `genuinely_ready_count == 0`, never as
    "0 s, nothing old". `null` beside `genuinely_ready_count > 0` means no ready row
    carried an age: the age is UNKNOWN, not "nothing to measure" — say so.
    `max_age_seconds` is over ALL rows, so a blocked PR can dominate it.

  **Self-check your parse against both invariants every iteration:**
  `genuinely_ready_count + blocked_count + unevaluated_count == count`, and
  `sum(by_block_reason) == blocked_count`. A mismatch means your parse is wrong (or a
  coord build predates the split — then all five keys are absent: say so, and treat
  `count` as dispatch-filter membership, never as ready). Per row, `prs[]` carries
  `readiness` — exactly one of `unevaluated` / `ready_no_proposal` /
  `ready_proposal_open` / `blocked` — plus `verdict_result` / `verdict_block_reason_code`
  / `verdict_as_of`, always serialized, `null` meaning never evaluated. **Render
  `readiness` on every row you list**, so a blocked PR is never presented as stuck-ready.
  `ready_proposal_open` does NOT assert an open proposal — only that a proposal row exists
  at the current head, possibly `merged` / `conflict` / `cancelled` /
  `shadow-landed`; read
  `latest_proposal_status` beside it. Source of truth: coord `pr_merge/ops_routes.rs`
  (`ready_unmerged_counters`, and the `get_health` doc comment). This split exists only
  on `/pr-merge/health` — no agent-reachable tool carries it — and that route answers an
  agent or device JWT `403 tenant_not_resolved` (checklist item 3's **TRANSPORT
  REALITY** note above). So this contract governs the read WHEN an operator credential
  carries it; a session holding only a device JWT has no split to report and says so.

  ⚠️ **DATE THE PAYLOAD BEFORE YOU READ A SINGLE FIELD OF IT. Quote `generated_at` AND
  its age in the ledger, every iteration.** Compute the age against YOUR OWN clock —
  `now - generated_at` — and compare it to the payload's `snapshot_max_age_seconds`
  budget (absent on a coord build predating that key: fall back to 600s and say you
  did). **Any age beyond the budget makes the whole payload UNKNOWN, not fleet state:
  discard it, re-read, and do not act on it.** This costs one subtraction and it is the
  only thing standing between you and a confidently-wrong pass.

  **Why this is REQUIRED and not hygiene.** Measured 2026-08-29T22:33:56Z: this read
  returned HTTP 200 with a fully-populated, well-formed payload describing the fleet as
  it stood **~48 hours earlier** — real PR numbers, real proposal statuses, real
  verbatim error strings, and *nothing* marking it stale. It was caught only because
  the PR numbers looked wrong against a census taken ten minutes before. This is a
  strictly more dangerous shape than the two staleness traps already documented below:
  the `/metrics` follower render returns all zeros, which is at least recognisable as
  broken. This returns **plausible, richly-populated, wrong data**.

  ⚠️ **Every other freshness-looking field in this payload is computed WITHIN the
  payload itself and therefore CANNOT indicate staleness.** `leader.lease_fresh` and
  `leader.heartbeat_age_seconds` are the trap: in the 2026-08-29 payload they read
  `true` and `2.38` — self-consistently *fresh* — for a `holder_id` present in neither
  live replica, on a `fenced_token` **41 terms behind**. Freshness there is measured
  relative to the snapshot's own recorded instant, so it is a tautology inside a stale
  payload rather than an assertion about the present. **`lease_fresh: true` is not
  evidence the view is live, and reading it as such is how a two-day-old fleet gets
  certified as healthy.** Read it against `leader.observed_at` (the DB instant the
  comparison was made at) — and where that key is absent, `lease_fresh` tells you
  nothing at all on its own.

  The root cause of that incident is NOT a stale snapshot inside coord: `assemble_health`
  is a live per-request derivation with no cache or memo in the path, so the bytes were
  genuinely produced at their `generated_at` and REPLAYED two days later. That is why
  the check belongs HERE, on the reader, and cannot be fully delegated to the server —
  a replay copies any `stale: false` the server writes. Plan:
  `2026-08-29-pr-merge-health-served-a-48h-stale-snapshot-as-live`.

  ⚠️ **A replay is only visible from a second instance — probe one before you name the
  cause.** The paragraph above names a mechanism, and it is known ONLY because a census
  taken ten minutes earlier disagreed with the payload. Reproduce that rather than
  inheriting it: read `<base>/pr-merge/health` a **second** time and compare
  `generated_at` — two reads seconds apart carrying an IDENTICAL `generated_at` is a
  replay, not a fleet that stood still — and sample `<base>/health` 4–8× so you know
  which replica answered each read. Coord being multi-replica is what makes a **second,
  independent instance** free here; there is no excuse for a single-read diagnosis. Only
  after that may you write "the payload is stale", "the view is cached", or "the train is
  stopped" — before it, report the fields and their age and nothing more. And where coord
  answers at all, `coord_recent_findings` on the `pr-merge` topic first: a stale-payload
  incident is exactly the finding a peer records and a steward re-derives.

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

- **Per open PR**, across the `--repos` **watch set** (Step 0 — DERIVED from the same read
  as the merge-authority set, so the two are the same population; the hardcoded `--repos`
  default is seven of forty and is a fallback, never the set): **`coord_pr_status {repo,
  number}`** — the
  deployed status card. Read `pr_state`, `head_sha`, `merge_state_status`, `mergeable`,
  **`confidence`** (`fresh|stale|unknown`), **`last_verified_at`**, `merged_at`,
  `merge_commit`, `blockers`, `dep_edges`. Enumerate open PRs with
  `gh pr list --repo <owner/repo> --state open --json number,mergeStateStatus,labels,isDraft`;
  skip drafts. **Also read `changedFiles`** — a `changedFiles=0` PR is **not mergeable work**;
  classify it, do not assume it landed. An empty diff is equally consistent with an
  already-landed PR, an unhydrated or emptied branch, and a self-revert; only the Tier-1
  "Already-landed empty-diff PR" row's P ∧ N ∧ V ∧ A proof tells them apart.
  ⚠️ **WATCH-ONLY CORRECTION 2026-09-16.** This used to read *"on a watch-only repo the twin
  card may be thin or absent, since coord is not landing there"*. **The reason is wrong and
  so is the expectation**: watch-only is a LANDER fact (Step 0), coord still holds authority
  there, and `coord_pr_status` for `qontinui-claude-config#980` came back fully populated.
  A thin or absent card is possible on ANY repo and is a fact you READ, never one you predict
  from the class. When it IS thin, that is UNKNOWN, not health: fall back to `gh` (`pr view`,
  `pr checks`) and judge the PR on its own CI. A repo the twin says nothing about is exactly the repo that goes unwatched
  for a week. ⚠️ **The fallback inherits the same rule one level down: a `gh pr checks` that
  reports NO check rows is UNKNOWN too, not green.** "Nothing is failing" on a head with
  nothing to fail is the zero-check vacuity, and reading it as a pass here launders an
  unverified PR into "judged on its own CI". Use the same two-conjunct form as everywhere else
  — **≥ 1 non-skipped check that PASSED, and no non-skipped check that has not passed** —
  rather than enumerating the non-passing states, which is how `cancelled` slips through: one
  pass plus one cancel satisfies a list that names only `queued` and `in_progress`, and a cancel
  has not passed. Zero rows is a state to diagnose, not a verdict.
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
  ⚠️ **WATCH-ONLY CORRECTION 2026-09-16.** This bullet used to say every read here is
  coord-sourced and therefore empty **BY CONSTRUCTION** on a **watch-only** repo — no
  candidates, no proposals, λ=0, no `suggested_stuck_threshold_secs`. **That is false, and it
  was false for the one repo watch-only actually names.** `coord_query_train_health` answered
  for `qontinui-claude-config` with a full payload: `candidate_ci_p90_secs: 78.8`, `last_land_at 2026-09-14T19:54:54Z`, 37
  `coord.scheduler_ticks` rows (inside `deferral_streak_basis`) and an
  `open_pr_backlog.histogram` over 18 open non-draft PRs — note the spelling: `train_health`
  has no `verdict_histogram`, that is `train_activity`'s row field. Watch-only is a LANDER fact
  (Step 0); coord still holds authority, a train and thresholds there. **So READ these values
  rather than assuming their absence**, and fall back to a plain wall-clock threshold only
  where the payload actually comes back empty. An empty read is UNKNOWN in both directions —
  not a wedge, not health — never a property of the class.

- **The agent alert queue, `merge_train` domain** — `coord_alert_queue {domain:
  "merge_train"}` / `GET /coord/alerts/queue?domain=merge_train`. Every row is a condition
  coord already detected and wants an agent on, paged rows first. Claim before acting and
  act only on `status: claimed`, release when done, never read an empty queue as a healthy
  train, and fall back to `GET /coord/alerts?kind=` (saying so) only on the three answers
  the KB names — all of it per checklist item 12. **A row never fires a Tier-1 reflex by
  itself**: merge each row into the per-PR snapshot below, where the per-PR reads
  re-confirm the class first, so a claimed row and the PR it names are dispositioned once.

- **RED MAIN — check it FIRST, every repo, every iteration. It is the highest-severity fleet
  signal and NOTHING else in this scan detects it.** A red main HOLDS the merge train: coord
  refuses to land while the base branch's CI baseline is failing. Prefer `coord.ci_baselines`
  when you have SQL; the no-SQL equivalent is the per-workflow main-run read below.
  (⚠️ **WATCH-ONLY CORRECTION 2026-09-16.** This used to read *"on a watch-only repo a red main
  holds no train — coord is not landing there — so it is a normal red to fix, not a
  fleet-severity alarm."* **Do not downgrade on that reasoning.** Watch-only is a LANDER fact
  (Step 0), and coord's train runs on a watch-only repo too: ccfg carried 37
  `coord.scheduler_ticks` rows and a land 40h old when this was measured. Downgrade only where
  you have ESTABLISHED that no coord train is in play for the repo, and say how you
  established it. The detector itself is unchanged and repo-agnostic; only the severity you
  attach to its verdict differs.)
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

  **Advisory STREAKS — row 5 prints one verdict per workflow, so run the streak detector too.**
  Row 5's `advisory:` line carries the NEWEST out-of-band verdict only. It cannot see a streak,
  and a nightly that blows a job-level `timeout-minutes` every night reads `RED(cancelled)` or,
  worse, looks like a supersede — GitHub concludes a job-level timeout as `cancelled`, never
  `failure` or `timed_out`. `qontinui-runner/scripts/detect-schedule-red-streaks.sh` is the
  tool built for exactly this class (its selftest workflow, `advisory-red-detector.yml:25-26`,
  names this steward as its caller), and until this paragraph nothing ran it: a census on
  2026-09-13 found no workflow, cron or skill in any repo invoking it (plan
  `2026-09-13-a-timeout-minutes-expiry-renders-as-cancelled-and-three-programs-are-blind-to-it`).
  It counts a `cancelled` scheduled run as failing only when that run carries a **bound-hit
  cancellation** — a job cancelled inside a tight cluster of same-job cancellations that sits
  above every earlier success — and keeps every other cancellation neutral.

  **Cadence: once at Step 0, then at most once per 6 hours** — nightlies change once a day, and
  the detector costs ~2 API calls per workflow plus one per run where a workflow's window holds
  ≥ 3 non-success runs, against the same per-account budget item 4 rations. Skip it (and say so)
  on any pass where item 4 hit a PRIMARY exhaustion. Read the detector from the runner's
  **`origin/main`**, never from a checkout, which may be parked on a branch where the file
  differs — the scope correction in the plan above records exactly that mistake:

  ```bash
  RUNNER=<workspace-root>/qontinui-runner
  git -C "$RUNNER" fetch -q origin +refs/heads/main:refs/remotes/origin/main || echo "streaks: runner fetch failed -- detector read is from a possibly stale origin/main"
  det="$(mktemp -d)"
  # The bound-hit signature file is read beside the detector; list it only when origin/main has it,
  # so the same snippet works on a runner main that predates it (the old detector needs no lib).
  git -C "$RUNNER" archive origin/main scripts/detect-schedule-red-streaks.sh \
    $(git -C "$RUNNER" ls-tree --name-only origin/main scripts/lib/bound-hit-signature.jq) | tar -x -C "$det"
  [ -f "$det/scripts/detect-schedule-red-streaks.sh" ] \
    || echo "streaks: could not extract the detector from runner origin/main -- every repo below is UNKNOWN for that reason"
  for repo in <each repo in the watch set>; do
    rc=0
    bash "$det/scripts/detect-schedule-red-streaks.sh" --repo "qontinui/$repo" --exit-zero || rc=$?
    [ "$rc" -eq 0 ] || echo "streaks: $repo UNKNOWN (detector exit $rc)"
  done
  rm -rf "$det"
  ```

  **Print every `schedule-red-streak:` finding beside the row-5 `advisory:` lines**, verbatim,
  and quote each repo's summary line even when it reports 0 findings — its `cancelled runs:` clause
  is what distinguishes "no bound-hits" from "no job data". Print any `UNDECIDED` line too: it names
  a job that times out in a tight cluster with no earlier success to order it against — a bound
  that never fit — which the detector deliberately does not count as a finding. It shares the
  `schedule-red-streak:` prefix, so never count or post an `UNDECIDED` line as a finding; it goes
  in the report as UNKNOWN about that job's bound, nothing more. A detector exit `2` is **UNKNOWN for
  that repo, never "no findings"**: it means a read failed, and the detector refuses to report
  zero off a failed read on purpose. A finding **never gates** and never holds the train, for
  the same reason row 5 never does. What it asks of you is a decision on the named workflow —
  resize the bound against the censoring point, split the job, or retire the workflow — so
  record each real finding (never an `UNDECIDED` line) as a coord finding (`coord_post_finding`, topic `advisory-red-streak`,
  resource key the workflow path) unless a live one for that workflow already stands.

  **The red-main remedies table — two different remedies clear a red main, and picking the wrong
  one proves nothing.** (That name is load-bearing: `/babysit-prs` Step 3 and
  `.claude/agents/merge-specialist.md` both cite *"the red-main remedies table"* and *"row 1"* by
  name, and until this heading existed the phrase appeared in this file only inside citations —
  so grepping the cited name found no target. Keep the name if you move the table.) The
  `e154036b` note below says a fresh dispatch was the *wrong* move there; `qontinui-types-drift.yml`'s
  own header says a fresh dispatch is the *right* move for it. Both are correct, for different
  cases — the discriminator is **where the failing run sits**, not which tool you like:

  | Case | Correct remedy | Why |
  |---|---|---|
  | A push run went red **at the tip** (case 1) on a flake or an infra kill — `RED(cancelled)`, **or** a `RED(failure)` that classifies Tier 1/2 | `rerun_failed_jobs` on that run | a GitHub re-run **preserves `event: push`**, reuses the run id and increments `run_attempt`, so it re-adjudicates the baseline at the current sha. This is what coord's own `auto_fix_red_main` does. |
  | A push run went red at an **older** sha and the workflow is **path-filtered**, so no later commit can re-trigger it (case 2b) | `gh workflow run <wf> --ref main` | a re-run would re-run *at the stale sha* and prove nothing about the tip. The dispatch is the only way to evaluate the workflow against the tip without a noop commit. |
  | A push run went red at an **older** sha and the workflow is **NOT filtered at all** — the commits since it were landed by a mechanism whose pushes GitHub suppresses (case 2s, "suppressed") | `gh workflow run <wf> --ref main` for evidence, then **escalate the lander's credential** | the dispatch is the same remedy as 2b and buys the same evidence, but the *cause* is opposite and so is the follow-up: 2b's workflow is behaving correctly and needs nothing fixed, while 2s's repo is in a **CI blackout** that no dispatch can end. |

  ⚠️ **2b and 2s render IDENTICALLY — `RED(...)@<older> (not triggered on tip)` — and reading
  2s as 2b is the benign-direction error that hides a repo-wide blackout.** The discriminator is
  the workflow's own `on:` block, which is readable and settles it: **2b has a `paths:` (or
  `paths-ignore:`) key; 2s has none.** A workflow with an unfiltered `push: branches: [main]`
  that has no run at the tip did not decline to match — it was *suppressed*, and something
  landed those commits without producing a push event. Confirm with a committer census —
  `git log origin/main -100 --format='%h|%cn'` cross-referenced against
  `gh api "repos/<repo>/actions/workflows/<wf>/runs?branch=main&event=push"` — and the
  suppressing lander names itself in the committer column.

  The known instance is **`qontinui-claude-config`**, and it is structural rather than
  incidental. `auto-merge.yml` there merges with `secrets.GITHUB_TOKEN`, and GitHub creates no
  new workflow runs for events triggered by that token. Measured 2026-09-05 over the newest 100
  commits of `origin/main`: **95 `committer=GitHub` auto-merge squashes, ZERO with a `push` run
  at their sha**, while all 4 `committer=qontinui-coord` ff-lands and the 1 human push have one.
  Both of that repo's main-push workflows were therefore 15 commits stale, with `qontinui CI`
  reading GREEN at a sha 15 unverified commits below the tip. This is why the lander column of
  the census above is not trivia: **`committer=GitHub` (auto-merge squash, trailing `(#N)`) is
  the shape that suppresses; `committer=qontinui-coord` is the shape that does not.** Plan:
  `2026-09-04-auto-merge-lands-trigger-no-main-ci`; the repo now carries a `main CI coverage`
  workflow that measures the gap directly, and a red line from THAT workflow is this class
  reporting itself rather than a check to re-run.

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
  `RED(<conclusion>)` so the reason travels with the alarm. An empty conclusion on a *completed*
  run is `UNKNOWN(blank conclusion)`, never blank output.

  ⚠️ **`cancelled` and `stale` are SUPERSEDED — a THIRD class, neither GREEN nor
  train-holding RED — and this is coord's rule, not a preference.**
  `ci_baseline.rs` (`is_supersession_conclusion`, quoted): *"A `workflow_run` conclusion that
  means 'superseded / never concluded', not a verdict on main's health. Such a run must not
  overwrite the last CONCLUSIVE baseline (else a concurrency-cancel wedges the merge queue) …
  `ingest_workflow_run` skips the write and the baseline keeps its last conclusive verdict —
  green stays green (no wedge), and a real `failure` is never masked because it was never
  overwritten."* So the run-selection above EXCLUDES them and falls through to the newest
  conclusive run, which is what coord scores. They are still surfaced, as a
  `+N superseded` annotation, never silently dropped.

  Both directions are wrong: do NOT upcast them to GREEN (the older bug), and do NOT report
  them as train-holding RED. Measured 2026-08-31: `qontinui-runner` reported a gating
  `RED(cancelled)@42ea7611` while the repo was landing PRs continuously — a permanent false red
  on an abandoned sha that nothing can ever supersede, which is the immortal-red shape the
  `excluded:` logic exists to kill, arriving by another door. With the fix the same workflow
  reads `GREEN@e6450727`, its last conclusive verdict.

  **The reason it survived five iterations is that nothing compared the two facts in that
  sentence.** A gating red produces a frightening line and no consequence, so nothing
  contradicts it; the only tell is that the train kept landing. Per-pass checklist item 5 is
  that comparison, and it is what should catch the next divergence of this class on the first
  pass rather than the fifth.

  ⚠️ `failure` / `timed_out` / `action_required` are REAL signals and stay RED — coord names
  them so in the same comment. Do NOT widen the superseded set.

  ⚠️ This is the RUN-level rule and does NOT change JOB-level triage: a `cancelled` *job*
  inside an otherwise-red run is a different question, handled by the step-level Tier-1/2/3
  classification. **But do not read the standing memory
  `reference_coord_infra_cancelled_job_reds_main_holds_train` as current at EITHER level.** Its
  title asserts that an infra-cancelled job reds main and holds the train. That is wrong at the
  run level (`is_supersession_conclusion`, quoted above) and — since 2026-07-24 — wrong at the
  job level too: `write_enriched_baseline` (`ci_baseline.rs`) now applies the job-level twin of
  the same skip, dropping a `failure` rollup whenever `all_failing_jobs_cancelled` proves every
  non-passing job in the enriched `failure_pattern.jobs` array was `cancelled`, and keeping the
  prior conclusive baseline. coord's own comment there cites the 2026-07-19 incident that memory
  was written from. What still reds main is what that predicate deliberately cannot prove: a
  **MIXED** failing set (any genuine `failure`/`timed_out`/`action_required` job alongside the
  cancelled ones), and an **UNENRICHED** run whose `jobs` array is empty or missing, which fails
  the predicate closed so a degraded enrichment can never launder a real red. The memory's other
  named defect is closed as well — `main_ci_status` now runs a required-context join
  (`failing_workflow_is_required`, fed by `required_checks_cached`), so a non-required advisory
  workflow failing on main no longer reds the train. Both landed in coord `f5d63aae`; verified
  by content on `origin/main` 2026-09-01.

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
  permanently red. `cancelled` is a different class again — SUPERSEDED, not RED — see the
  superseded note in the verdict vocabulary above.
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
  # ── GitHub API budget: read it from a REAL request's RESPONSE HEADERS ────────────────
  # ⚠️ **NEVER from `gh api rate_limit`.** That endpoint is a well-formed, CONFIDENT WRONG
  # ANSWER here. Measured 2026-08-25 on this account, 26 seconds apart, same token, both
  # responses self-reporting `X-Ratelimit-Resource: core`:
  #   GET /user       22:19:46Z -> 403, Limit 5000, Remaining    0, Used 5000, Reset 22:26:02Z
  #   GET /rate_limit 22:20:12Z -> 200, Limit 5000, Remaining 4841, Used  159, Reset 22:23:34Z
  # Different used, different remaining, and a DIFFERENT RESET INSTANT — so `rate_limit` is
  # not merely *exempt from consumption*, it answers about a bucket that is not the one
  # gating you. A steward that preflights on it is told 96.8% of the budget is intact while
  # every real call is being refused. The only authority is the headers on a request that
  # actually passed through the gate — which is why every tip read below uses `-i`.
  # An ABSENT header is UNKNOWN: never 0, and never "fine". Every consumer treats an empty
  # value as "cannot tell" and declines to gate on it, saying so out loud.
  rm_bud() {
    local f="${RM_BUD_FILE:-}"
    RM_BUD_LIMIT=""; RM_BUD_REMAIN=""; RM_BUD_USED=""; RM_BUD_RESET=""; RM_BUD_RESRC=""; RM_BUD_RETRY=""
    [ -s "$f" ] || return 0
    RM_BUD_LIMIT=$(grep -i  '^x-ratelimit-limit:'     "$f" | head -n 1 | cut -d: -f2 | tr -d ' \r')
    RM_BUD_REMAIN=$(grep -i '^x-ratelimit-remaining:' "$f" | head -n 1 | cut -d: -f2 | tr -d ' \r')
    RM_BUD_USED=$(grep -i   '^x-ratelimit-used:'      "$f" | head -n 1 | cut -d: -f2 | tr -d ' \r')
    RM_BUD_RESET=$(grep -i  '^x-ratelimit-reset:'     "$f" | head -n 1 | cut -d: -f2 | tr -d ' \r')
    RM_BUD_RESRC=$(grep -i  '^x-ratelimit-resource:'  "$f" | head -n 1 | cut -d: -f2 | tr -d ' \r')
    RM_BUD_RETRY=$(grep -i  '^retry-after:'           "$f" | head -n 1 | cut -d: -f2 | tr -d ' \r')
    return 0
  }
  # The server's own `message`, for the arms where we are NOT claiming to know the cause.
  # `gh api -i` sends stderr to the void (the body carries the same text), so without this the
  # non-throttle failure paths would print a candidate LIST and nothing observed — trading one
  # confident-wrong-diagnosis for a vaguer one.
  # ⚠️ It NEVER renders blank, for the same reason `rm_reset_at` does not. Every call site
  # embeds it mid-sentence after "GitHub said:", so an empty return printed
  # `GitHub said:  — candidates are …` — a sentence whose subject silently vanished, which
  # reads as "GitHub said nothing" when the truth may be "nothing was captured to read". The
  # two are different facts and each now names itself.
  # ⚠️ Its FIRST branch is a BACKSTOP, not a live path, and labelling it is the whole point.
  # Once `rm_throttle_report` went three-valued, an absent or empty capture returns 2 and is
  # answered by the `tcls` 2 arm, which does not call this function at all — so **both** call
  # sites below reach here only with a non-empty `RM_BUD_FILE`, guaranteed by the very test
  # that produced the return of 1. The branch stays because this is a FUNCTION contract the
  # next call site inherits, and the suite exercises it directly (cases A8/A8b). It is
  # labelled rather than left to be rediscovered because an unlabelled unreachable branch is
  # precisely what the withdrawn fourth defect was — and the next reader, finding it, would
  # otherwise have to re-derive its reachability from two call sites and a return code.
  rm_err_msg() {
    local m=""
    if [ ! -s "${RM_BUD_FILE:-}" ]; then
      printf 'NOTHING — no response was captured, so the server was not quoted'
      return 0
    fi
    m=$(sed -e '1,/^[[:space:]]*$/d' "$RM_BUD_FILE" | jq -r '.message // empty' 2>/dev/null) || m=""
    m=${m%$(printf '\r')}   # CR strip, same class as query.tsv: this is quoted verbatim to the operator
    if [ -n "$m" ]; then printf '%s' "$m"; else printf 'NOTHING — the captured response body carried no message field'; fi
    return 0
  }
  # An operator needs an INSTANT to wait until, so this never renders blank and never says
  # "soon": it degrades to the raw epoch, and to a named UNKNOWN when the header was absent.
  rm_reset_at() {
    local e="${RM_BUD_RESET:-}" t=""
    if [ -z "$e" ]; then printf 'an UNKNOWN time (no X-Ratelimit-Reset header)'; return 0; fi
    t=$(date -u -d "@$e" +%H:%M:%SZ 2>/dev/null) || t=""
    [ -n "$t" ] || t=$(date -u -r "$e" +%H:%M:%SZ 2>/dev/null) || t=""
    if [ -n "$t" ]; then printf '%s' "$t"; else printf 'epoch %s' "$e"; fi
    return 0
  }
  # Classifies a saved response and prints ONE explicit line naming the throttle class, the
  # measured budget, the remedy, and the instant to retry at — so a throttle never reaches an
  # operator disguised as a bare UNKNOWN that reads like an auth problem.
  #
  # THREE-VALUED, and the third value is the whole point. Returns 0 IFF the response really
  # was a rate-limit rejection; returns 1 when a response WAS captured and was not a throttle,
  # so a non-throttle failure KEEPS ITS OWN DIAGNOSIS instead of being laundered into
  # "throttled"; and returns 2 when NO response was captured at all, which is not evidence of
  # anything.
  # ⚠️ 1 and 2 used to be the same return, and that collapse re-created the exact defect this
  # whole block was written to remove. `mktemp` failing sends the tip read down the
  # uninstrumented plain-`gh` fallback, which saves no response — so this function had nothing
  # to classify, said "not a throttle", and the caller printed
  # `cause: NOT a rate-limit refusal` over a 403 it had never looked at. A confident negative
  # derived from an absent measurement is the same class as the confident `main moved`
  # derived from an absent sha: absence must never become a finding, in EITHER direction.
  #
  # ORDER MATTERS, and it is not the obvious one. The tempting discriminator — "remaining is
  # still high, therefore SECONDARY" — is a heuristic GitHub's own documentation does NOT
  # endorse: its handling ladder explicitly contemplates a secondary refusal WITH
  # `x-ratelimit-remaining: 0` as well as without, so a high remaining is suggestive and a
  # zero remaining proves nothing about the class on its own. The structural discriminator is
  # `documentation_url`, which is what `google/go-github`'s CheckResponse branches on
  # (secondary iff it ends `#abuse-rate-limits` or `secondary-rate-limits`; a primary refusal
  # ends `#rate-limiting` — verified against a live 403 on this account, 2026-08-25). So the
  # body is consulted FIRST and the headers only corroborate.
  #   SECONDARY — burst/concurrency. Remedy: honour Retry-After, else wait >= 60s and back
  #               off exponentially with a BOUNDED retry count; make requests more serially.
  #   PRIMARY   — the hourly bucket is spent. Nothing but time helps, lower parallelism helps
  #               NOTHING, and the budget is ACCOUNT-WIDE, so continuing to poke it starves
  #               coord and every peer session. GitHub is explicit that "continuing to make
  #               requests while you are rate limited may result in the banning of your
  #               integration", which is why this arm says STOP rather than RETRY.
  # Collapsing the two into one "rate limited" line is what makes an operator wait an hour for
  # a 60-second problem, or retry-storm a bucket that will not refill for 45 minutes.
  rm_throttle_report() {
    local what="${RM_WHAT:-the request}"
    [ -s "${RM_BUD_FILE:-}" ] || return 2
    head -n 1 "$RM_BUD_FILE" | grep -qE ' (403|429)([^0-9]|$)' || return 1
    grep -qiE 'rate limit|secondary-rate-limits|abuse-rate-limits' "$RM_BUD_FILE" || return 1
    rm_bud
    if grep -qiE 'secondary-rate-limits|abuse-rate-limits|exceeded a secondary rate limit' "$RM_BUD_FILE"; then
      echo "  throttled: GitHub SECONDARY rate limit while reading $what (identified from the response body, not from the budget headers — the primary '${RM_BUD_RESRC:-unknown}' budget reads ${RM_BUD_REMAIN:-?}/${RM_BUD_LIMIT:-?} remaining, which does NOT settle the class either way). This is burst/concurrency: wait ${RM_BUD_RETRY:-60}s${RM_BUD_RETRY:+ (Retry-After)}, back off exponentially, cap the retries, and lower RED_MAIN_PARALLEL. Verdict withheld."
    elif [ "${RM_BUD_REMAIN:-}" = "0" ]; then
      echo "  throttled: GitHub PRIMARY rate limit EXHAUSTED on resource '${RM_BUD_RESRC:-unknown}' (${RM_BUD_USED:-?}/${RM_BUD_LIMIT:-?} used) while reading $what. This budget is ACCOUNT-WIDE and shared with coord, /babysit-prs, the runner and every peer session — no retry and no lower parallelism succeeds before it resets at $(rm_reset_at). STOP THE SWEEP: every remaining repo will fail identically, and continuing to poke a spent bucket risks the account. Verdict withheld."
    elif [ -n "${RM_BUD_RETRY:-}" ]; then
      echo "  throttled: GitHub refused the read of $what and sent Retry-After ${RM_BUD_RETRY}s without naming the class (primary '${RM_BUD_RESRC:-unknown}' reads ${RM_BUD_REMAIN:-?}/${RM_BUD_LIMIT:-?} remaining). Honour Retry-After — that instruction is class-independent. Verdict withheld."
    else
      echo "  throttled: GitHub refused the read of $what as rate-limited, but the response named no class and carried no usable budget headers (remaining='${RM_BUD_REMAIN:-}', resource='${RM_BUD_RESRC:-}') — PRIMARY vs SECONDARY is UNKNOWN, so neither wait is asserted here. Wait at least 60s before any retry, and cap them. Verdict withheld."
    fi
    return 0
  }
  red_main() {
  local r="${RM_REPO:-}" tip tip2 wf live win d qn qfail unadj adv nb nobase note id state name cls line probes gp gprun rc budnote need budneed tcls rm_prog cmps cmpskip cmpbud newerGp newerLast nsite nfrom nto nev nconc ncache nlab nnote
  local PAR="${RED_MAIN_PARALLEL:-12}" DEPTH="${RED_MAIN_DEPTH:-10}" tnote=""
  # Held back from the fan-out for OTHER consumers of the same account-wide budget — coord's
  # merge train, peer sessions, /babysit-prs. Not a safety margin for this function.
  local RESERVE="${RED_MAIN_BUDGET_RESERVE:-250}"
  # Validated because it is interpolated into an arithmetic expansion below, where a
  # non-integer is a hard shell error rather than a false compare — an operator typo in an
  # env var must not take the fleet's highest-severity detector down. Fails to the default.
  #
  # ALL THREE TUNABLES ARE GUARDED HERE, and for one tick this guard covered only the third.
  # `PAR` and `DEPTH` were declared two lines up and reached a process spawner and a URL with
  # nothing checking them, which is the same unguarded-input shape as `RESERVE` arriving at an
  # arithmetic expansion — but each fails DIFFERENTLY, so the argument above does not carry
  # over and is not what justifies them:
  #
  # `PAR` reaches `xargs -P "$PAR"` twice. MEASURED, GNU findutils 4.10.0:
  #   `xargs -P abc` → `invalid number "abc" for -P option`, exit 1, and ZERO children run.
  #   The `|| true` that follows each fan-out (an errexit backstop, correct on its own terms)
  #   then swallows that exit, so the ENTIRE fan-out is skipped and every workflow falls to
  #   `UNKNOWN@none (per-workflow runs read FAILED for id N — verdict withheld)` — a positive
  #   claim about a read that was never attempted. That is this fence's own defect class
  #   (absence rendering as a finding), reachable from a typo, in the one input left unchecked.
  #   `xargs -P 0` is the WORSE half because it succeeds: it is accepted and runs (measured),
  #   and GNU documents -P 0 as "run as many processes as possible" — that second half is CITED
  #   semantics, not something measured here — so the width goes UNBOUNDED, the opposite of the
  #   remedy
  #   every SECONDARY rate-limit arm in this file prescribes, arrived at by an operator typing
  #   what they read as "off". So non-positive is rejected as well as non-numeric, which is
  #   why this is a character-class test FOLLOWED BY a numeric one and not either alone: after
  #   the first, `$PAR` is all digits and `-gt 0` is safe arithmetic on it, and it also catches
  #   `00` and `000`, which the character class alone does not.
  #   Pinned by `E11` (the class half), `E12`/`E12b` (the numeric half, including the `00` the
  #   class waves through) and `E14` (the anti-vacuity control: a valid non-default width is
  #   NOT rejected). ⚠️ Each of those pairs its header assertion with one read off a RECORDED
  #   `xargs -P`, and the pairing is the point: the header prints `$PAR`, the variable this
  #   guard has just written, so on its own it cannot tell a width that reached `xargs` from
  #   one that only reached a `printf`. That distinction is load-bearing HERE rather than
  #   pedantry: the `abc` half also announces itself through xargs's own `invalid number`, but
  #   the `0` half is measured to error NOWHERE, so for `E12`/`E12b` the recorded `-P` is the
  #   only non-header witness there is — MEASURED, a fence sanitising only what it prints
  #   passed both arms outright.
  #   ⚠️ "TWICE" IS THE FIRST WORD OF THIS PARAGRAPH AND FOR TWO INCREMENTS THE WITNESSES
  #   COVERED ONE OF THE TWO. Every recording named above comes from the FETCH fan-out; the
  #   gating probe below is the other `xargs -P "$PAR"`, and no arm executed it at all —
  #   `SHIM_RUNS` was empty in all 28, so no workflow `.json` was written, `ambig.txt` stayed
  #   empty and `probes` was 0 throughout. MEASURED: deleting the probe fan-out outright left
  #   the whole suite green. `E16` is the arm that reaches it, and it needs TWO assertions no
  #   single-site arm does, because both spawners append a bare width to ONE log: a COUNT (both
  #   ran) and an all-lines width (neither was hardcoded). It runs at `-P 4` for that second
  #   reason — at the default, a probe pinned to `12` is indistinguishable from a guarded one.
  #   ⚠️ The second test rejects one more thing, and the message says the accepted SHAPE rather
  #   than a diagnosis because of it: an all-digit value past the shell's integer range does not
  #   compare as small, it ERRORS (MEASURED, bash 5.2: `[ 99999999999999999999999 -gt 0 ]` →
  #   `integer expression expected`, rc 2), which `2>/dev/null` swallows and `||` routes to the
  #   same fallback. Falling back is right; a message saying that value "is not at least 1"
  #   would not be, so none is asserted — this fence does not print a derived cause it cannot
  #   support, and that rule applies to its own guards.
  #
  # ⚠️ AND THAT REASONING SENT US BACK TO `RESERVE`, which had carried the character class ALONE
  # since it shipped, so the class of value just described walked straight through it. It gets
  # the numeric test too — spelled `-ge 0` rather than `-gt 0`, because zero IS a legal reserve
  # here (discouraged and documented, not rejected) while a value this shell cannot compare is
  # not a reserve at all.
  # ⚠️ THAT IS ONLY HALF OF IT, and saying so here is the point: `-ge 0` closes the values the
  # shell cannot COMPARE, and a value it can compare can still be one it cannot ADD. MEASURED,
  # bash 5.2: `9223372036854775807` (INT64_MAX) is all digits, so the character class waves it
  # through, and `[ 9223372036854775807 -ge 0 ]` is TRUE, so this arm waves it through as well —
  # then `need + RESERVE` wraps to `-9223372036854775796`. The other half is therefore closed at
  # the ONE arithmetic site that consumes it, the pre-flight gate below, where `need` exists to
  # add. A guard that stopped here would have been an honest test of the wrong property.
  #
  # `DEPTH` is interpolated into `per_page=$RM_DEPTH` on every one of the `qn` fan-out URLs.
  #   A value that is not a positive integer cannot be a page size, so the fan-out spends `qn`
  #   calls of the ACCOUNT-WIDE budget the pre-flight gate has just approved on calls that
  #   cannot answer — and the header below then prints `depth=<the typo>` as though that were
  #   what was read. NOTHING is claimed here about what GitHub does with such a value: it is
  #   not measured, and the guard does not need it, because the value is rejected on its own
  #   shape. (For the same reason this does not cap DEPTH at any ceiling — an unmeasured
  #   ceiling asserted as a clamp would be the same overclaim one direction over.)
  #
  # The rejection is NAMED rather than applied silently, and that is the one place these two
  # differ from `RESERVE`, whose fallback is self-revealing (its decline message prints the
  # reserve it actually used). A rejected `PAR` is invisible the moment the default works, and
  # a rejected `DEPTH` is invisible behind a header printing the default — so the operator who
  # set it, the only person who can fix it, would never learn it was ignored, and the tick
  # would read as evidence about the fleet when it is evidence about the config. `RESERVE`
  # joins the same NOTE for consistency: it had the guard but not the sentence.
  #
  # ⚠️ The `""` in each pattern is a CONTRACT BACKSTOP, not a live path, and it is labelled
  # here rather than left for the next reader to re-derive — an unlabelled unreachable branch
  # is exactly what `rm_err_msg` above had to correct. `${VAR:-default}` substitutes the
  # default for an unset AND for an empty value, so all three variables are non-empty by the
  # time they reach these tests; the empty pattern can only fire if a later edit drops a `:-`.
  # It is kept for that, and because the sibling guard has carried it since it shipped — but
  # no assertion claims it, since a fixture claiming it would pass with the pattern deleted.
  # Each arm ASSIGNS THE DEFAULT FIRST and then interpolates it, so the sentence cannot drift
  # from the value: written the other way the literal appears twice per tunable, and the next
  # edit to a default leaves the NOTE confidently naming a number it did not use.
  case "$RESERVE" in
    (*[!0-9]*|"") RESERVE=250; tnote="${tnote}RED_MAIN_BUDGET_RESERVE='${RED_MAIN_BUDGET_RESERVE-}' is not a whole number, using ${RESERVE}; " ;;
    (*) [ "$RESERVE" -ge 0 ] 2>/dev/null || { RESERVE=250; tnote="${tnote}RED_MAIN_BUDGET_RESERVE='${RED_MAIN_BUDGET_RESERVE-}' is not a reserve this shell can add — it must be a whole number the shell can compare, and an over-range one wraps NEGATIVE in the gate below and silently disables the reserve, using ${RESERVE}; "; } ;;
  esac
  case "$PAR" in
    (*[!0-9]*|"") PAR=12; tnote="${tnote}RED_MAIN_PARALLEL='${RED_MAIN_PARALLEL-}' is not a whole number, using ${PAR}; " ;;
    (*) [ "$PAR" -gt 0 ] 2>/dev/null || { PAR=12; tnote="${tnote}RED_MAIN_PARALLEL='${RED_MAIN_PARALLEL-}' is not a usable fan-out width — it must be a whole number of at least 1 that the shell can compare (0 and 00 are UNBOUNDED parallelism to xargs, not off; a value past the shell's integer range fails the comparison), using ${PAR}; "; } ;;
  esac
  case "$DEPTH" in
    (*[!0-9]*|"") DEPTH=10; tnote="${tnote}RED_MAIN_DEPTH='${RED_MAIN_DEPTH-}' is not a whole number, using ${DEPTH}; " ;;
    (*) [ "$DEPTH" -gt 0 ] 2>/dev/null || { DEPTH=10; tnote="${tnote}RED_MAIN_DEPTH='${RED_MAIN_DEPTH-}' is not a usable page size — it must be a whole number of at least 1 that the shell can compare, using ${DEPTH}; "; } ;;
  esac
  # `local` so each repo starts from a clean budget rather than inheriting the previous
  # repo's numbers; `rm_bud` (dynamically scoped into these) also clears them on every call.
  local RM_BUD_FILE="" RM_BUD_LIMIT="" RM_BUD_REMAIN="" RM_BUD_USED="" RM_BUD_RESET="" RM_BUD_RESRC="" RM_BUD_RETRY=""
  # Stated separately from the tip read below so an unset RM_REPO names its OWN cause. Folding
  # it into the tip check would surface a calling-convention mistake as "cannot resolve tip of
  # main (wrong default branch? auth?)" — a confident, wrong diagnosis. The `:-` default above
  # is what keeps this message REACHABLE under a `set -u` caller, which would otherwise abort on
  # the unset read one line earlier and print nothing at all.
  [ -n "$r" ] || { echo "UNKNOWN — red_main reads its repo from the named variable RM_REPO (call it as: RM_REPO=owner/repo red_main); RM_REPO was empty or unset, so NOTHING was read and no verdict is implied"; return 1; }
  # Emitted AFTER the RM_REPO guard so that a calling-convention mistake still renders as the
  # single self-contained line above, and before the first read so it is present even on the
  # ticks that decline this repo at the pre-flight gate and never reach the header.
  # ⚠️ This sentence ends where it does deliberately. An earlier draft closed it with "and none
  # of this changes a verdict", which THIS FILE contradicts twice: the depth section below calls
  # the raw-window spend "the one place depth changes an outcome" and tells the operator to
  # RAISE `RED_MAIN_DEPTH` for a deploy-event-heavy workflow, so a rejected 50 restoring 10 is
  # exactly when a workflow renders `no-baseline` instead of a verdict; and the SECONDARY remedy
  # is to LOWER `RED_MAIN_PARALLEL`, so a rejected low width restoring 12 can re-trigger the
  # throttle that withholds every verdict on the repo. The guard is not itself a verdict
  # decision, but its consequences are not none, and asserting they were would be an unmeasured
  # positive claim in the file that exists to refuse them.
  [ -z "$tnote" ] || echo "$r: NOTE: rejected tunable value(s) — ${tnote}defaults are in force for this tick. Nothing was silently accepted; the defaults may not be the width or depth you asked for, and the tunables section explains what each one changes."
  # BUDGET-INSTRUMENTED. `-i` costs NO extra API call — the budget headers ride the response
  # this function already had to make — and it is the only honest source of the remaining
  # budget (see the rm_bud block above for why `gh api rate_limit` is not). It is also what
  # makes a REFUSAL informative: gh prints the full status line and headers before it branches
  # on the status code, so a 403 still yields its budget headers. The plain-gh fallback below
  # only runs if mktemp fails, and it is deliberately unchanged behaviour, not a second path
  # to maintain.
  RM_BUD_FILE=$(mktemp) || RM_BUD_FILE=""
  if [ -n "$RM_BUD_FILE" ]; then
    gh api -i "repos/$r/commits/main" > "$RM_BUD_FILE" 2>/dev/null || true
    rm_bud
    tip=$(sed -e '1,/^[[:space:]]*$/d' "$RM_BUD_FILE" | jq -r '.sha // empty' 2>/dev/null) || tip=""
  else
    tip=$(gh api "repos/$r/commits/main" --jq .sha) || tip=""
  fi
  # CR strip — see the note on query.tsv, and `rm_bud` above, which already strips `\r` off
  # every header value for the same reason. `$tip` is compared to `$tip2` (the mid-read
  # re-read) and to every `head_sha`; it must be stripped at the SAME layer as they are, or
  # the strip itself manufactures the inequality it was added to prevent.
  tip=${tip%$(printf '\r')}
  # HARD PRECONDITION. An empty $tip makes every head_sha comparison fail, so cases 1
  # and 3 become UNREACHABLE and the repo silently degrades to all-stale verdicts.
  # ⚠️ The cause is now DERIVED, never asserted. This message used to read
  # "(wrong default branch? auth?)" — two guesses printed as if they were the finding, on a
  # line that fires for any failure at all. When the real cause was an exhausted API budget
  # it sent the operator to look at branch configuration and credentials, neither of which
  # was wrong. Same class as everywhere else in this file: a suppressed error must never
  # become a confident value, and that includes a confident *diagnosis*.
  if [ -z "$tip" ]; then
    echo "$r: UNKNOWN — cannot resolve tip of 'main'; verdict withheld"
    # ⚠️ The `if` form is load-bearing, NOT style. `rm_throttle_report; tcls=$?` aborts the
    # whole function under a `set -e` caller on exactly the two returns that matter (1 and 2),
    # so the `cause:` line never prints, `rc` is never assigned, and the temp file leaks —
    # while a real throttle (0) survives. That is failure biased toward the misdiagnosis-prone
    # path, which is this block's whole subject. A command in an `if` condition is exempt from
    # `set -e`; that is what makes the three-valued dispatch safe here.
    if RM_WHAT="the tip of main" rm_throttle_report; then tcls=0; else tcls=$?; fi
    case "$tcls" in
      0) rc=2 ;;
      2) echo "  cause: UNKNOWN — the throttle class could not be established because NO response was captured for this read (the saved response is absent or empty: mktemp may have failed, or gh may have produced no output at all). The cause is DERIVED, so neither is asserted. This is NOT a finding that it was not a throttle: from here a rate-limit refusal and a credentials failure are indistinguishable."
         rc=1 ;;
      *) echo "  cause: NOT a rate-limit refusal. GitHub said: $(rm_err_msg) — candidates are a non-'main' default branch, credentials, or the network. Diagnose this one; do not wait it out."
         rc=1 ;;
    esac
    [ -z "$RM_BUD_FILE" ] || rm -f "$RM_BUD_FILE"
    return "$rc"
  fi

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

  d=$(mktemp -d) || { [ -z "$RM_BUD_FILE" ] || rm -f "$RM_BUD_FILE"; echo "$r: UNKNOWN — mktemp failed; verdict withheld"; return 1; }

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
    || { rm -rf "$d"; [ -z "$RM_BUD_FILE" ] || rm -f "$RM_BUD_FILE"; echo "$r: UNKNOWN — could not build the workflow query set; verdict withheld"; return 1; }
  # ⚠️ CR STRIP — a CORRECTNESS step on Windows, not cosmetics, and the reason every jq
  # value below is stripped too. jq opens stdout in TEXT mode on Windows, so each line it
  # writes ends CRLF; the shell reads the file with IFS=TAB, which is not CR, so the LAST
  # tab-separated field keeps it. Here that field is `name`, and `A<CR>` rendered as a line
  # break mid-verdict on the `guard-roster-windows` leg. The same translation put
  # `success<CR>` into the newer-* conclusion below — which is not `success`, so a GREEN run
  # was labelled `newer-red`: a WRONG verdict word, not a spacing defect. This file already
  # treats Windows as a first-class host (see the 32KB argv note below), and neither `gh`
  # (Go, LF) nor Linux jq emits CR, so the strip is a no-op everywhere else. Applied AFTER
  # the `||` handler so jq's own exit status still reaches it, and `|| :` so a strip that
  # somehow fails cannot abort a `set -e` caller — worst case the CR survives and prints.
  { tr -d '\r' < "$d/query.tsv" > "$d/query.lf" && mv -f "$d/query.lf" "$d/query.tsv"; } || :

  qn=$(wc -l < "$d/query.tsv" | tr -d ' \t')
  [ -n "$qn" ] && [ "$qn" -gt 0 ] 2>/dev/null \
    || { rm -rf "$d"; [ -z "$RM_BUD_FILE" ] || rm -f "$RM_BUD_FILE"; echo "$r: UNKNOWN — no workflows to query (workflow list unusable AND no runs discovered); verdict withheld"; return 1; }

  # ── PRE-FLIGHT BUDGET GATE ────────────────────────────────────────────────────────────
  # About to issue qn per-workflow calls, up to qn gating probes and one tip re-read against
  # a budget that is ACCOUNT-WIDE, not this function's own: coord's merge train, /babysit-prs,
  # the runner and every peer session draw from the same 5000/h. Spending the last of it buys
  # a HALF-READ repo and strands every other consumer behind the same wall — which is exactly
  # what happened on 2026-08-25: the sweep read qontinui-coord and qontinui-runner back to
  # back, the account hit 5000/5000, and the third repo — qontinui-web, the busiest on the
  # fleet with 21 open PRs — got no verdict at all.
  # So a repo we cannot afford to read COMPLETELY is declined CHEAPLY and honestly, before
  # spending anything, instead of half-read expensively. This is never a green: it prints
  # UNKNOWN, withholds the verdict, and returns non-zero like every other abstention here.
  # An UNKNOWN budget does NOT gate — but it does not silently assert affordability either;
  # it says so, and every per-workflow path below still fails closed on a refused call.
  #
  # ⚠️ THE COST IS THE WORST CASE, and it must be, because "completely" is the whole claim.
  # Three calls are already SPENT by the time we get here (the tip read, the workflow list,
  # the discovery window), so what is left to buy is: qn fan-out calls, up to qn gating
  # probes, and one tip re-read. `probes` is NOT KNOWABLE at this point — it is derived from
  # the fan-out responses, which is precisely what we are deciding whether to buy — so its
  # upper bound qn is the only honest figure, giving 2*qn + 1.
  # This gate shipped budgeting `qn + 2` instead, i.e. it under-counted by up to qn - 1 calls
  # (nearly HALF the repo on a probe-heavy one), so on the shortfall it exists to catch it
  # approved a read it could not finish and half-read the repo anyway — the exact outcome it
  # was written to prevent. The number is computed ONCE into `need` and both the test and the
  # message read that variable: the shipped defect was an arithmetic and a sentence free to
  # disagree, and they did.
  # `need` is deliberately a CEILING, so a repo whose probes come in low is sometimes declined
  # when it would just have fitted. That is the cheap error, and the same trade the RESERVE
  # already makes; half-reading the repo and starving coord is the expensive one.
  need=$((2 * qn + 1))
  # ⚠️ THE ONLY ARITHMETIC `RESERVE` REACHES, and so the only place a reserve that PASSED the
  # guard above can still do damage — the guard rejects a reserve this shell cannot compare;
  # this rejects one it cannot add. MEASURED end to end through this fence with a fixture `gh`
  # (`remaining` 10, `qn` 6, so `need` 13) and `RED_MAIN_BUDGET_RESERVE=9223372036854775807`:
  # the sum wrapped NEGATIVE, `remaining -lt <negative>` was false, the gate declined NOTHING,
  # and the fence read the repo on with the reserve effectively at ZERO — printing no decline
  # line and no NOTE. That is exactly the silently-zeroed reserve this file calls the dangerous
  # half, arriving past both halves of the guard, and it is why the reserve is checked in TWO
  # places rather than one.
  # The test is a DETECTION, not an assumption about limits: `RESERVE` is all digits by the time
  # it gets here (the character class guarantees it) so it is non-negative, and `need` is at
  # least 3, so a correct sum is always GREATER than `RESERVE` — a sum below it can only be a
  # wrap. No ceiling is invented and none is asserted; an enormous but non-wrapping reserve is
  # left alone, because "decline every repo" is a thing an operator may legitimately have asked
  # for. Pinned by `E10c` — and the OTHER half of this line, that a valid reserve is actually
  # ADDED, by `E15`. A rejection arm cannot show that half, and until `E15` existed nothing
  # did: MEASURED, deleting `+ RESERVE` from this expression outright left ALL 143 of the
  # suite's assertions GREEN, because every budget arm ran the reserve at 0 — the additive
  # identity — and the only three carrying a non-zero one sat below `need` on its own, so
  # they declined either way. The knob could have stopped working and the suite would have
  # said nothing.
  budneed=$((need + RESERVE))
  if [ "$budneed" -lt "$RESERVE" ]; then
    # ASSIGNED BEFORE IT IS INTERPOLATED, for the reason the guard block above
    # states and this line used to break: written the other way the literal appears
    # twice, and the next edit to the default leaves the NOTE confidently naming a
    # number it did not use. This was the only site in the fence still spelling it
    # the forbidden way, and it shipped in the same commit as the rule.
    RESERVE=250
    echo "$r: NOTE: rejected tunable value(s) — RED_MAIN_BUDGET_RESERVE='${RED_MAIN_BUDGET_RESERVE-}' cannot be added to this repo's cost without overflowing the shell's integer range, which would silently disable the reserve entirely; using ${RESERVE}. Nothing was silently accepted."
    budneed=$((need + RESERVE))
  fi
  # This NOTE names its cause as "the tip read returned no X-Ratelimit-Remaining header", and
  # that is ACCURATE on every path that reaches here — checked, not assumed. The other way to
  # arrive with an empty RM_BUD_REMAIN is an uninstrumented tip read, i.e. `mktemp` having
  # failed above; but `mktemp -d` is called before this point and fails under the same
  # conditions, so that path returns earlier and never reaches this line. A second, derived
  # branch was drafted here and then WITHDRAWN as unreachable: adding a cause the code cannot
  # produce is the same defect as asserting one it cannot support, one direction over.
  if [ -z "${RM_BUD_REMAIN:-}" ]; then
    echo "$r: NOTE: GitHub API budget UNKNOWN this tick (the tip read returned no X-Ratelimit-Remaining header), so the pre-flight affordability gate did NOT run. A mid-sweep refusal is still reported explicitly and still withholds the verdict — it is never a green."
  elif [ "$RM_BUD_REMAIN" -lt "$budneed" ] 2>/dev/null; then
    rm -rf "$d"; [ -z "$RM_BUD_FILE" ] || rm -f "$RM_BUD_FILE"
    echo "$r: UNKNOWN — GitHub API budget too low to read this repo COMPLETELY: ${RM_BUD_REMAIN} left on resource '${RM_BUD_RESRC:-core}', this repo needs up to ${need} more calls ($qn workflow reads, up to $qn gating probes, one tip re-read) plus the ${RESERVE}-call reserve held back for coord and peer sessions. Nothing further was read; verdict withheld, retry after $(rm_reset_at)."
    return 2
  fi

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
  # `|| true`: a non-zero `xargs` would abort a `set -e` caller HERE, before the header prints
  # — a tick that prints nothing, which must never happen. Child failures are already
  # recorded as `.fail` marker files and surface per workflow below.
  # ⚠️ Be exact about WHICH `xargs` exit that is, because this comment used to name the one
  # that cannot happen. `xargs` returns 123 when a child exits 1-125 — and the helper generated
  # one line above ends in `|| : > "...fail"`, so `sh` exits 0 whatever `gh` did. MEASURED with
  # the real generated line and a failing `gh` shim: the helper exits 0, and `xargs` over two
  # such children exits 0, never 123. (Control, same box: `xargs` over children that really do
  # exit non-zero returns 123, so the instrument can see the code it did not find here.)
  # So 123 is unreachable while that helper swallows its own failure, and this guard is a
  # BACKSTOP for the exits that remain reachable rather than for the one it used to cite:
  # a child killed by a signal (MEASURED 125 — an OOM kill of one `-P` child is the realistic
  # instance; the FREQUENCY is not measured, only the exit) and an `env` that xargs cannot run
  # (MEASURED 127 for a missing command; 126 is its found-but-unexecutable twin, not separately
  # measured). Keep the guard; it is cheap and those paths are
  # real. Do NOT rewrite it as a claim about failed children, and note that deleting the
  # helper's `|| : > "...fail"` would put the 123 path back — the two are coupled.
  # This is also exactly why `steward-red-main-throttle-fixtures-test.sh` declares this guard
  # UNCOVERED instead of counting it: no fixture can redden it, so an arm claiming it would
  # pass with the guard deleted.
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
  # The `-eq 0` short-circuit is what stops `xargs` being handed an empty `ambig.txt`; the
  # trailing `|| true` is the same errexit backstop as the fetch fan-out, and it carries the
  # same correction. `probe.sh` above ALSO ends in `|| : > "...pushfail"`, so it too exits 0
  # whatever `gh` did (MEASURED, with the real generated line and a failing `gh` shim: exit 0,
  # and `xargs` over two such children exits 0). A failed probe therefore never reaches this
  # guard either — it is reachable only by a signal-killed child (MEASURED 125) or an `env`
  # xargs cannot run (MEASURED 127). Keep it for those; do not read it as covering probe
  # failure, which is handled by the `.pushfail` marker and the `gating_unknown` route above.
  # ⚠️ This guard stays uncovered, and the reason is now MEASURED rather than predicted. The
  # suite used to list it as uncovered because `probes` was 0 in every arm — the WEAKER reason,
  # which reads as "a runs fixture would reach it". `E16` is that runs fixture: `probes` is 1,
  # this `xargs` runs, and the guard is STILL not reddened, because `probe.sh` exits 0 whatever
  # `gh` did. The structural reason survived the arm that removed the weaker one. An arm built
  # to assert THIS guard would still pass with it deleted.
  # ⚠️ AND THE FAN-OUT ITSELF IS NOW WITNESSED, which it was not for two increments: `E16`
  # asserts that this line spawned and did so at the guarded `$PAR`. That is a different claim
  # from the guard above it, and it is the one the paragraph three up says matters most here —
  # this site fails quieter than the fetch fan-out.
  [ "$probes" -eq 0 ] || xargs -P "$PAR" -I{} env RM_REPO="$r" RM_DIR="$d" RM_WF={} sh "$d/probe.sh" < "$d/ambig.txt" || true

  # Main can advance while we read; then the "tip" we label is stale and runs on the real
  # tip are invisible. This re-read sits AFTER the fan-out deliberately, so it brackets every
  # per-workflow read — re-reading before them would leave the reads unbracketed.
  # Budget-instrumented for the same reason as the first read, and additionally because THIS
  # is the call the 2026-08-25 exhaustion actually landed on — so this is where the budget
  # reported in the footer is measured from, after the whole fan-out has been paid for.
  if [ -n "$RM_BUD_FILE" ]; then
    gh api -i "repos/$r/commits/main" > "$RM_BUD_FILE" 2>/dev/null || true
    rm_bud
    tip2=$(sed -e '1,/^[[:space:]]*$/d' "$RM_BUD_FILE" | jq -r '.sha // empty' 2>/dev/null) || tip2=""
  else
    tip2=$(gh api "repos/$r/commits/main" --jq .sha) || tip2=""
  fi
  # CR strip, same class as query.tsv: `$tip2` is COMPARED to `$tip`, so a `<CR>` on the
  # right-hand side reports a main move that never happened — the exact wrong-diagnosis
  # failure the block below exists to prevent, arriving by a different door.
  tip2=${tip2%$(printf '\r')}
  # ⚠️ **AN EMPTY tip2 IS A FAILED RE-READ, NOT A MAIN MOVE.** Conflating the two shipped a
  # confident wrong diagnosis for as long as this line existed: a failed read set tip2 empty,
  # the equality test below could not distinguish that from a moved branch, and the operator
  # was told main had moved. Measured 2026-08-25 22:12:35Z on qontinui-web — the account had
  # spent its whole 5000/h core budget, this call 403'd, and the detector printed
  # `UNKNOWN - main moved mid-read (bd80272c -> )`, sending the reader to look at a branch
  # that had not moved at all. Fail-closed held (UNKNOWN, non-zero, no verdict) — the
  # *reason* was invented. The tell was in the output the whole time: the arrow's right-hand
  # side rendered BLANK, because there was no second sha to print. The empty case now names
  # its own cause, and the move message is only ever reached with two real shas.
  if [ -z "$tip2" ]; then
    rm -rf "$d"
    echo "$r: UNKNOWN — the tip re-read FAILED, so the fan-out could not be bracketed (this is NOT a main move: ${tip:0:8} -> <read failed>); verdict withheld"
    # `if`, for the reason spelled out at the first tip read: a bare call plus `$?` is a
    # `set -e` abort on rc 1 and rc 2.
    if RM_WHAT="the tip re-read" rm_throttle_report; then tcls=0; else tcls=$?; fi
    case "$tcls" in
      0) rc=2 ;;
      2) echo "  cause: UNKNOWN — the throttle class could not be established because NO response was captured for this read (the saved response is absent or empty). This is NOT a finding that it was not a throttle."
         rc=1 ;;
      *) echo "  cause: NOT a rate-limit refusal. GitHub said: $(rm_err_msg) — candidates are credentials or the network. Diagnose this one; do not wait it out."
         rc=1 ;;
    esac
    [ -z "$RM_BUD_FILE" ] || rm -f "$RM_BUD_FILE"
    return "$rc"
  fi
  [ "$tip" = "$tip2" ] || { rm -rf "$d"; [ -z "$RM_BUD_FILE" ] || rm -f "$RM_BUD_FILE"; echo "$r: UNKNOWN — main moved mid-read (${tip:0:8} -> ${tip2:0:8}); verdict withheld, re-read next tick"; return 1; }
  # The budget VALUES survive in RM_BUD_* for the footer; only the response file is dropped.
  [ -z "$RM_BUD_FILE" ] || rm -f "$RM_BUD_FILE"

  echo "== $r tip=${tip:0:8} — $qn workflow(s), per-workflow authoritative read (depth=$DEPTH, parallel=$PAR)"
  [ -z "$note" ] || echo "$note"
  # This NOTE is a terminal verdict, so it owes the axes it was computed from.
  # It is about GITHUB's workflow list and nothing else -- no coord door was
  # consulted to produce it, and it says nothing about one. Adding the CONFLICT
  # ledger section pushed this file over the cascade-surface threshold, which is
  # what made the roster owed here (lint-reachability-axes check #58 arm B).
  # axes: hosts=api.github.com prefixes=/repos credentials=gh-token unprobed=api.qontinui.io
  [ "$live" != "null" ] || echo "  NOTE: workflow list unavailable or truncated — producer liveness UNKNOWN for every line below, AND the workflow inventory itself fell back to the distrusted run window, so a workflow missing from that window is missing from this report entirely. A RED here may be a deleted workflow's immortal last run; a green repo verdict is NOT supported and this repo returns non-zero."

  qfail=0; unadj=0; adv=0; nb=0; nobase=""; cmps=0; cmpskip=0
  # ===== how many descent compares this repo may spend =======================================
  # THE ANNOTATION YIELDS TO THE BUDGET, and this is D1's lesson applied to a call the gate
  # above does not know about. `need` is `2 * qn + 1` -- the reads that MUST happen for a
  # verdict -- and the newer-* compares sit on top of it. Folding them into `need` would raise
  # the affordability bar by up to 50% for a cost MEASURED at 0-1 calls per repo (ccfg, live,
  # 2026-09-06: exactly 1 for ten workflows), declining whole repos over an annotation; spending
  # them unbudgeted is the shortfall D1 is about. So they are capped instead: whatever remains
  # after the mandatory reads and the reserve, never more than `qn` (one per workflow is the
  # pre-memoisation maximum), and NONE at all when the budget is UNKNOWN -- an annotation is
  # never worth a call that cannot be accounted for. Skips are COUNTED and printed, so the
  # omission is visible rather than silent.
  cmpbud=0
  if [ -n "${RM_BUD_REMAIN:-}" ]; then
    cmpbud=$((RM_BUD_REMAIN - need - RESERVE)) || cmpbud=0
    [ "$cmpbud" -ge 0 ] 2>/dev/null || cmpbud=0
    [ "$cmpbud" -le "$qn" ] || cmpbud="$qn"
  fi
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
      gp=${gp%$(printf '\r')}   # see the CR-strip note on query.tsv: `gating<CR>` matches no case arm
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
    # The jq program is bound to a NAME rather than written inline, because it is now run
    # TWICE per workflow -- once as the newer-* PREPASS, which emits an annotation request, and
    # once to RENDER the line with the resolved note. Two inline copies is precisely the drift
    # the annotation design forbids. It is a string assignment, not a call: no extra process.
    rm_prog='
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
        # The three PASSING conclusions again, as a predicate. Used ONLY to ask whether the
        # newer-* evidence CONTRADICTS the line it would decorate -- never to form a verdict.
        def passing(c): (["success","skipped","neutral"] | index(c // "")) != null;
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
        # SUPERSEDED conclusions are NOT verdicts, and coord says so in code:
        # ci_baseline.rs -- "a `workflow_run` conclusion that means superseded /
        # never concluded, not a verdict on mains health. Such a run must not
        # overwrite the last CONCLUSIVE baseline (else a concurrency-cancel
        # wedges the merge queue) ... `ingest_workflow_run` skips the write and
        # the baseline keeps its last conclusive verdict". `cancelled` is what
        # GitHub stamps when a newer push cancels a still-running job; `stale`
        # is the analogous marker. So they are EXCLUDED from the completed set
        # here, and selection falls through to the newest CONCLUSIVE run --
        # exactly what coord baseline does. Measured 2026-08-31: without this,
        # qontinui-runner reported a gating `RED(cancelled)@42ea7611` while the
        # repo was landing PRs continuously, which is a permanent false red on
        # an abandoned sha that nothing can ever supersede.
        # `failure`/`timed_out`/`action_required` are REAL and stay RED -- coord
        # names them so in the same comment. Do NOT widen this set.
      | ["cancelled","stale"] as $SUPERSEDED
      | ([$attip[] | select(.status == "completed")
                   | select((.conclusion // "") as $c | ($SUPERSEDED | index($c)) == null)]
         | sort_by(.created_at) | last) as $tipDone
      | ([$attip[] | select(.status != "completed")] | length) as $inflight
      | ([$attip[] | select(.status == "completed")
                   | select((.conclusion // "") as $c | ($SUPERSEDED | index($c)) != null)]
         | length) as $tipSuperseded
      | (if $inflight > 0 then " +\($inflight) in flight" else "" end) as $busy
      | (if $tipSuperseded > 0 then " +\($tipSuperseded) superseded" else "" end) as $sup
        # Superseded runs ANYWHERE in the judged set, not only at the tip. $tipSuperseded is
        # at-tip only, so on the fallthrough line it is provably 0 and annotating with it
        # would be dead code -- which is exactly how the MEASURED qontinui-runner case
        # (cancelled off-tip, the conclusive verdict older still) dropped its annotation
        # silently, contradicting the never-silently-dropped claim in this very file.
      | ([$R[] | select(.status == "completed")
                | select((.conclusion // "") as $c | ($SUPERSEDED | index($c)) != null)]
         | length) as $allSuperseded
      | (if $allSuperseded > 0 then " +\($allSuperseded) superseded" else "" end) as $supAll
      | ([$R[]  | select(.status == "completed")
                | select((.conclusion // "") as $c | ($SUPERSEDED | index($c)) == null)]
         | sort_by(.created_at) | last) as $lastDone
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
        # A SUPERSEDED conclusion gets its own tip-superseded label rather than tip-red -- it
        # carries no verdict at all, and calling it red here would contradict the run-level rule.
      | (if $tipAlt == null then "" else
           (($tipAlt.conclusion // "") as $tc
            | (if $tc == "success" then "tip-green"
               elif ($SUPERSEDED | index($tc)) != null then "tip-superseded"
               elif ($tc == "") or ((["skipped","neutral"] | index($tc)) != null) then "tip-other"
               else "tip-red" end) as $tlab
            | " [\($tlab): \($tipAlt.event) \(if $tc == "" then "blank conclusion" else $tc end)@\($tip[0:8]) — observed, not adjudicating]")
         end) as $tipnote
        # The three branch guards below are HOISTED rather than written inline in the chain,
        # because $noteSite has to ask the same questions and a second copy would drift.
      | (($mode == "gates_no_evidence") and (($state != "dead") or (($attip | length) > 0))) as $bGNE
      | (($mode == "gating_unknown") and (($state != "dead") or (($attip | length) > 0))) as $bGU
      | (($state == "dead") and (($attip | length) == 0)) as $bDead
        # newer-* REQUEST. The evidence run: newest completed CONCLUSIVE non-push baseline run
        # that is NOT at the tip -- the tip case is $tipnote and stays there. See the newer-run
        # bullet in the notes for the descent rule, the contradiction gate and the cost.
      | ([$cand[] | select((.event != "push") and (.status == "completed") and (.head_sha != $tip))
                  | select((.conclusion // "") as $c | ($SUPERSEDED | index($c)) == null)]
         | sort_by(.created_at) | last) as $newerAlt
        # WHICH line would render, and with what sha/conclusion. Identical gates to the two
        # $tipnote use sites, expressed once.
      | (if $bGNE and ($gprun != null) and ($gprun.status == "completed")
             and ((($gprun.conclusion // "") as $gc | ($SUPERSEDED | index($gc)) == null))
             and (($gprun.conclusion // "") != "") and (($gprun.head_sha // "") != $tip)
           then {site: "gp", sha: ($gprun.head_sha // ""), conc: ($gprun.conclusion // "")}
         elif (($bGNE | not) and ($bGU | not) and ($n != 0) and ($bDead | not)
               and ($tipDone == null) and ($inflight == 0) and ($lastDone != null)
               and $gates and (($lastDone.conclusion // "") != ""))
           then {site: "last", sha: ($lastDone.head_sha // ""), conc: ($lastDone.conclusion // "")}
         else null end) as $noteSite
        # CONTRADICTION ONLY. Evidence agreeing with the line changes nothing a reader acts on,
        # and the compare call is the cost -- so no request is emitted for it.
      | (if ($noteSite == null) or ($newerAlt == null) then null
         else (($newerAlt.head_sha // "") as $es
               | if ($es == "") or ($noteSite.sha == "") or ($es == $noteSite.sha)
                    or (passing($newerAlt.conclusion) == passing($noteSite.conc))
                 then null
                 else {site: $noteSite.site, from: $noteSite.sha, to: $es,
                       event: ($newerAlt.event // "?"), conc: ($newerAlt.conclusion // "")} end)
         end) as $newerReq
        # ONE program, two modes. The prepass emits the REQUEST and nothing else; the render
        # pass takes the resolved notes back as $newerGp / $newerLast. Running the same program
        # for both is what stops the request gates and the render gates drifting apart.
      | if $prepass then
          (if $newerReq == null then ""
           else "\($newerReq.site)\t\($newerReq.from)\t\($newerReq.to)\t\($newerReq.event)\t\($newerReq.conc)" end)
        else
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
        if $bGNE then
          # The tip-run note rides THIS line too, and that is not decoration. Every dispatch adds a
          # run to the DEPTH window and can evict the last in-window push run, flipping a workflow
          # from push_in_window to gates_no_evidence — so annotating only the case-2 branch would
          # make the annotation vanish precisely for the operator who applied the documented
          # dispatch remedy hardest. Gated on an ADJUDICATED probe verdict (non-blank conclusion)
          # that is NOT already at the tip, so it never decorates an UNKNOWN or a redundant line.
          (if ($gprun != null) and ($gprun.status == "completed")
              and ((($gprun.conclusion // "") as $gc | ($SUPERSEDED | index($gc)) == null)) then
             "\(cls($gprun.conclusion))\t  \($w): \(verdict($gprun.conclusion))@\(($gprun.head_sha // "none")[0:8]) (newest push run on main, older than the \($raw0) examined; from gating probe)\(if (($gprun.conclusion // "") != "") and (($gprun.head_sha // "") != $tip) then $tipnote else "" end)\($newerGp)\($pq)\($depth)"
           elif ($gprun != null) and ($gprun.status == "completed") then
             # SUPERSEDED probe run: per_page=1 gives no older run to fall through to, so there
             # is nothing conclusive to report. It must NOT render as RED -- that is the defect
             # this very commit closes, surviving on the probe path, and the probe reaches PAST
             # window into frozen history, which is where an immortal false red lives longest.
             # UNKNOWN is the honest verdict: no conclusive push run was observed.
             "UNADJ\t  \($w): UNKNOWN@\(($gprun.head_sha // "none")[0:8]) (newest push run on main is \($gprun.conclusion // "?") — superseded, carries no verdict; from gating probe. Raise RED_MAIN_DEPTH to see a conclusive one)\($pq)\($depth)"
           elif ($gprun != null) then
             "UNADJ\t  \($w): UNKNOWN@\(($gprun.head_sha // "none")[0:8]) (newest push run on main is still \($gprun.status // "pending"); from gating probe)\($pq)\($depth)"
           else
             "UNADJ\t  \($w): UNKNOWN@none (gates main — push probe confirms push runs exist — but none in the \($raw0) examined and the probe returned no run; raise RED_MAIN_DEPTH)\($pq)\($depth)" end)
        elif $bGU then
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
        elif $bDead then
          "ADJ\t  excluded:\($w) (no live producer — deleted from repo; "
          + (if $lastDone == null then (if $allSuperseded > 0 then "no CONCLUSIVE run on main (\($allSuperseded) superseded)" else "no completed run on main" end)
             else "last conclusive \(if ($lastDone.conclusion // "") == "" then "blank" else $lastDone.conclusion end)@\(($lastDone.head_sha // "none")[0:8])" end)
          + ")\($depth)"
        # ADV routes every advisory line, whatever its conclusion: an advisory workflow never
        # gates, so it must not reach UNADJ (which would hold the repo unadjudicated) nor ADJ
        # (whose RED holds the train). It is still PRINTED in full — suppressing it is what hid
        # the atlas nightly failure for 12 days.
        elif $tipDone != null then
          "\(if $gates then cls($tipDone.conclusion) else "ADV" end)\t  \($adv)\($w): \(verdict($tipDone.conclusion))@\($tip[0:8])\($busy)\($sup)\($advwhy)\($pq)\($depth)"
        # Gated on $inflight, NOT on ($attip | length): if every completed run at the tip is
        # SUPERSEDED, $tipDone is null while $attip is non-empty, so keying on $attip swallows
        # the fallthrough and renders a PERMANENT UNADJ -- text that says no completed run
        # while a completed run exists. That re-creates the durable false signal this change
        # exists to kill, one class over: an immortal false UNKNOWN instead of an immortal
        # false RED. It is reachable by the dispatch remedy this file documents, since a
        # dispatch on a workflow with cancel-in-progress concurrency cancels the tip push run.
        # Falling through to $lastDone is what coord does -- keep the last CONCLUSIVE baseline.
        elif $inflight > 0 then
          "\(if $gates then "UNADJ" else "ADV" end)\t  \($adv)\($w): UNKNOWN@\($tip[0:8]) (triggered on tip, \($inflight) in flight, no completed run)\($sup)\($advwhy)\($pq)\($depth)"
        elif $lastDone != null then
          "\(if $gates then cls($lastDone.conclusion) else "ADV" end)\t  \($adv)\($w): \(verdict($lastDone.conclusion))@\(($lastDone.head_sha // "none")[0:8]) (\(if ($attip | length) > 0 then "tip run superseded" else "not triggered on tip" end))\($supAll)\(if $gates and (($lastDone.conclusion // "") != "") then $tipnote else "" end)\($newerLast)\($advwhy)\($pq)\($depth)"
        else
          "\(if $gates then "UNADJ" else "ADV" end)\t  \($adv)\($w): UNKNOWN@none (\(if $allSuperseded > 0 then "no CONCLUSIVE run on main in the \($n) examined (\($allSuperseded) superseded)" else "no completed run on main in the \($n) examined" end))\($advwhy)\($pq)\($depth)"
        end end'
    # ===== the newer-* annotation: resolve the request ========================================
    # The prepass emits at most ONE tab-separated request per workflow, and only when the
    # evidence CONTRADICTS the line. jq failing here costs the annotation and nothing else --
    # the render pass below reports its own failure -- so it fails to an empty request file.
    newerGp=""; newerLast=""
    jq -r --arg tip "$tip" --arg w "$name" --arg state "$state" --arg probe "$gp" --argjson gprun "$gprun" \
          --argjson prepass true --arg newerGp "" --arg newerLast "" "$rm_prog" \
       < "$d/$id.json" > "$d/$id.newer" 2>/dev/null || : > "$d/$id.newer"
    # The second CR carrier — see the strip on `query.tsv`. `nconc` is this file's last
    # tab-separated field, and `success<CR>` misses the `success)` arm of the case below, so
    # the CR does not mis-space the note, it RELABELS a green run red.
    { tr -d '\r' < "$d/$id.newer" > "$d/$id.newer.lf" && mv -f "$d/$id.newer.lf" "$d/$id.newer"; } || :
    while IFS=$(printf '\t') read -r nsite nfrom nto nev nconc; do
      [ -n "$nsite" ] && [ -n "$nfrom" ] && [ -n "$nto" ] || continue
      # DESCENT, not recency. `compare` is merge-base relative, so `ahead_by` is non-zero for a
      # DIVERGED sha too; `status` is the only field that answers the ancestry question, and it
      # is tested `= ahead` rather than `!= diverged` so that `behind` and `identical` are both
      # refused. MEMOISED per (from, to) for the tick: workflows on one repo share stale shas.
      # A FAILED call caches `call-failed`, which is not `ahead`, so it annotates nothing and
      # is not retried -- an absent answer is never allowed to fall through into a descent.
      ncache="$d/cmp.$nfrom.$nto"
      if [ ! -f "$ncache" ]; then
        if [ "$cmps" -ge "$cmpbud" ]; then cmpskip=$((cmpskip+1)); continue; fi
        gh api "repos/$r/compare/$nfrom...$nto" --jq .status > "$ncache" 2>/dev/null \
          || printf 'call-failed\n' > "$ncache"
        cmps=$((cmps+1))
      fi
      [ "$(cat "$ncache" 2>/dev/null)" = "ahead" ] || continue
      # SYMMETRIC, exactly as the tip-* note is: the label comes from the evidence run own
      # conclusion, so newer-red prints as loudly as newer-green. The name states the POSITION
      # of the run, never a verdict about the line it decorates.
      case "$nconc" in
        success)             nlab="newer-green" ;;
        ""|skipped|neutral)  nlab="newer-other" ;;
        *)                   nlab="newer-red" ;;
      esac
      [ -n "$nconc" ] || nconc="blank conclusion"
      nnote=" [$nlab: $nev $nconc@${nto:0:8} — newer than this line sha, older than tip; observed, not adjudicating]"
      case "$nsite" in
        gp)   newerGp="$nnote" ;;
        last) newerLast="$nnote" ;;
      esac
    done < "$d/$id.newer"
    line=$(jq -r --arg tip "$tip" --arg w "$name" --arg state "$state" --arg probe "$gp" --argjson gprun "$gprun" \
                 --argjson prepass false --arg newerGp "$newerGp" --arg newerLast "$newerLast" "$rm_prog" \
              < "$d/$id.json") \
      || { qfail=$((qfail+1)); echo "  $name: UNKNOWN@none (jq failed on id $id — verdict withheld)$([ "$state" = unknown ] && printf ' [producer liveness UNKNOWN]')"; continue; }
    # Same strip, command-substitution form: `$( )` eats the trailing newline and leaves the
    # CR, so an un-stripped `line` ends every rendered verdict with one. `gprun` is exempt on
    # purpose — it is spent as `--argjson`, where a trailing CR is legal JSON whitespace.
    line=${line%$(printf '\r')}
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
  # The measured budget rides the footer so its TREND is visible tick over tick — the whole
  # point of instrumenting the tip reads. An UNKNOWN budget prints as UNKNOWN, never as blank
  # and never omitted, so "no budget shown" can never be read as "budget fine".
  if [ -n "${RM_BUD_REMAIN:-}" ]; then
    budnote=", GitHub budget ${RM_BUD_REMAIN}/${RM_BUD_LIMIT:-?} left on '${RM_BUD_RESRC:-core}' after this repo (resets $(rm_reset_at))"
  else
    budnote=", GitHub budget UNKNOWN (no X-Ratelimit-Remaining header on the tip re-read)"
  fi
  echo "  read: $qn workflow(s) queried, $qfail failed, $unadj unadjudicated, $adv advisory (non-gating), $probes gating probe(s), $cmps descent compare(s)$([ "$cmpskip" -eq 0 ] || printf ' (+%s skipped, budget)' "$cmpskip"), $((qn + probes + cmps + 4)) API calls issued$budnote"
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
    shipped rule.** `qontinui-coord/crates/coord/src/ci_baseline.rs` defines
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
    the run id and incrementing `run_attempt` — `crates/coord/src/ci_baseline.rs` says the same, in
    its module doc (`git grep -n 'bumps .run_attempt' origin/main -- crates/coord/src/ci_baseline.rs`):
    *"a re-run reuses its run id and only bumps `run_attempt`, and that re-run is the sanctioned
    red-main remedy"*. So
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
  - **The `newer-*` annotation — because the `tip-*` note above EVAPORATES on any unrelated push.**
    Plan: `2026-09-06-stale-red-tip-green-annotation-evaporates-on-unrelated-push`. `$tipAlt`
    requires `.head_sha == $tip`, so when `main` advances the corroborating evidence stops being
    printed with **no change whatever in the condition it describes** — the line degrades from
    *"RED, but proven green at the tip"* to a bare *"RED"*, and the next reader cannot tell it from
    a live breakage. MEASURED on `qontinui-claude-config` `fleet-skill bundle parity`: the note was
    present at tip `29162d71`, absent at tip `a519c67f` 16 minutes later, and the only intervening
    commit touched a **command** file this workflow does not gate on.

    ⚠️ **Do NOT "fix" this by widening `$tipAlt`.** Relaxing `.head_sha == $tip` to "the newest
    completed non-push run at any sha" is the obvious one-liner and it re-creates the one-way
    ratchet toward optimism rule 2 above forbids: a green run at an ANCIENT sha would then
    decorate a fresh red. The tip scoping is correct on its own terms. What was missing is a
    SECOND, honestly-labelled shape for the case the scoping cannot express.

    So a line carrying an **adjudicated stale** verdict can now also carry
    `[newer-green: schedule success@b2b2b2b2 — newer than this line sha, older than tip; observed, not adjudicating]`,
    under the same five rules as the `tip-*` note (annotation only; symmetric — `newer-red` prints
    as loudly, with `newer-other` for the benign non-passes; the event always named; completed
    CONCLUSIVE runs only) plus **three of its own**:
    1. **Admitted only on proven DESCENT.** `gh api repos/<r>/compare/<line-sha>...<evidence-sha>`
       must read `status == "ahead"` — **exactly**. Not "newer by `created_at`": a timestamp says
       nothing about branch topology and an abandoned or diverged sha must never corroborate.
       Read `status`, not `ahead_by` — compare is merge-base relative, so a diverged sha reports a
       non-zero `ahead_by` that is not descent. And test `== "ahead"`, never `!= "diverged"`:
       `behind` and `identical` are the other two values and both must be refused. A FAILED call
       caches `call-failed`, which is not `ahead`, so an absent answer can never fall through into
       a descent — and it makes no claim in the other direction either.
    2. **The call is issued only when the evidence CONTRADICTS the line**, and **memoised per
       `(from-sha, to-sha)`** for the tick. Both are cost gates, not niceties: the population is
       every adjudicated stale line, which this same file measures as roughly half a repo's
       workflows at any moment. Without the contradiction gate `C` is the same order as `W`.
       **And the annotation YIELDS to the budget.** The pre-flight gate budgets `2 * qn + 1` —
       the reads a VERDICT needs — and these compares sit on top of it, so they are capped by
       whatever remains after the mandatory reads and the reserve, never exceed `qn`, and are
       **not issued at all when the budget is UNKNOWN**. Skips are counted and printed
       (`0 descent compare(s) (+1 skipped, budget)`), because an annotation silently omitted is
       indistinguishable from one that had nothing to say — which is the defect class this note
       exists to close. Folding `C` into `need` instead would raise the affordability bar by up
       to 50% and decline whole repos over a cost measured at 0–1 calls each.
    3. **The gate is the SAME adjudicated-and-stale test `$tipnote` already uses — NOT "the line
       carries a RED".** Restricting it to REDs looks like a cost saving and is an asymmetry: a
       stale `GREEN@<older>` whose workflow has since produced a **failing** run at a descendant
       sha would print nothing, while the mirror case still prints. Evidence that a condition has
       BROKEN suppressed, evidence that it was FIXED shown — the same ratchet, arriving through
       the cost gate instead of through `$tipAlt`. The label names the run's **position**, not the
       verdict it decorates, which is why it is `newer-*` and not `post-red-*`.

    **Severity, narrowed rather than inflated.** The decay is SELF-HEALING wherever the workflow
    has an out-of-band cadence: the same ccfg line got its note back ~46 minutes later with no
    intervention, because the workflow's ~1h `schedule` produced a fresh completed run AT the new
    tip. So for a workflow WITH a cadence this is a legibility defect bounded by that interval —
    real, because a steward iterating every 15 minutes reports the bare RED two or three times per
    restoration, but not a blind spot. The population this is really for is a **path-filtered
    workflow with NO out-of-band cadence at all**, where nothing will ever produce a fresh non-push
    run at the new tip and the annotation, once dropped, never returns. ⚠️ That same self-healing
    is what will make a hand-run verification of this look like a pass with the change reverted:
    re-measure inside the window — after an unrelated push and BEFORE the next scheduled run
    completes — or against a cadence-less workflow.

    **Why not just re-fire the workflow.** `gh workflow run <wf> --ref main` produces a
    `workflow_dispatch` run, which never adjudicates under the push-only baseline rule, so it buys
    one iteration of tip-scoped legibility that decays again on the next unrelated push — for one
    API call plus a CI run, forever.

    **Implementation note, and it is load-bearing rather than incidental.** The annotation needs an
    API call, and jq cannot make one — so the per-workflow jq program is bound to `rm_prog` and run
    **twice**: once with `--argjson prepass true`, which emits the annotation REQUEST and nothing
    else, and once to render the line with the resolved note in `$newerGp` / `$newerLast`. One
    program, so the gates that emit a request and the gates that render a line cannot drift apart.
    The three branch guards `$bGNE` / `$bGU` / `$bDead` are hoisted for the same reason. Fixtures:
    `scripts/steward-red-main-throttle-fixtures-test.sh` arms `N1`–`N8`, covering all four
    `compare` statuses, the failed call, the anti-vacuity control (a stale GREEN with a
    contradicting red MUST annotate), the contradiction cost gate, and the memoisation.
  - **Candidate events are still narrowed first, and `branch=main` does NOT do it for you.**
    `deployment_status` and `dynamic` are never evidence: web's `Verify Frontend Deploy` is
    100/100 `deployment_status` (all `completed/skipped`), and `dynamic` on `qontinui/qontinui` is
    Dependabot `Graph Update` with 2 of 3 recent runs `failure`. Coord excludes `dynamic` at BOTH
    ingest (`is_dynamic_event`, called from the `workflow_run` ingest path —
    `git grep -n 'fn is_dynamic_event' origin/main -- crates/coord/src/ci_baseline.rs`) and read time
    (`is_one_shot_dynamic_workflow`) after a live wedge — **qontinui-web 2026-06-05,
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
  - **`gh api rate_limit` is a CONFIDENT WRONG ANSWER about the budget — the authority is the
    response headers of a real request.** This is the load-bearing measurement behind the
    budget instrumentation, and it reproduces in both the throttled and the healthy state.
    Measured 2026-08-25 on this account, same token, both responses self-reporting
    `X-Ratelimit-Resource: core`:

    | | a real request | `GET /rate_limit` |
    |---|---|---|
    | throttled (22:19:46Z / 22:20:12Z) | 403, remaining **0**, used **5000**, reset 22:26:02Z | 200, remaining **4841**, used **159**, reset 22:23:34Z |
    | recovered (~23:09Z, seconds apart) | remaining **4519**, used **481**, reset 23:26:20Z | remaining **4985**, used **15**, reset 23:23:50Z |

    Different used, different remaining, and a **different reset instant** every time — in the
    recovered sample `/rate_limit` under-reported consumption **32-fold** (15 vs 481). So it is
    not merely *exempt from consumption*: **it answers about a bucket that is not the one
    gating you**, and because it always reads near-pristine it can never warn. A steward that
    preflights on it is told the budget is fine and walks into the wall — a non-empty,
    well-formed value whose provenance cannot carry it, which is exactly the
    unknown-must-not-render-as-a-default class this file polices everywhere else.
    **The cause is NOT established** and this file does not assert one (a pooled or shared
    credential resolving `/rate_limit` to a different identity is a candidate, not a finding);
    what is established is the disagreement, three independent times. GitHub's own guidance
    lands in the same place — *"When possible, you should use the rate limit response headers
    instead of calling the API to check your rate limit"* — and adds two more reasons not to
    poll it: it **does** count against the *secondary* limit, and **"there is not a way to
    check the status of your secondary rate limit"** at all. Hence both tip reads use
    `gh api -i`, which costs **no extra call** (the headers ride a response the function
    already had to fetch) and, because `gh` prints the status line and headers before it
    branches on the status code, is exactly what makes a **refusal** informative.
  - **Lowering `RED_MAIN_PARALLEL` was the obvious fix for 2026-08-25 and it is the WRONG one
    — deliberately rejected.** The refusal was a **primary** limit: `X-Ratelimit-Resource:
    core`, `Used: 5000`, `Remaining: 0`, **no `Retry-After`**, and the primary-form message
    body — and it struck `GET /user`, a plain non-Actions endpoint, so it was not an
    Actions-specific limit either (Actions REST endpoints draw from `core`; the only
    Actions-named bucket, `actions_runner_registration`, is for registering self-hosted
    runners, and the documented per-repo Actions figure applies to `GITHUB_TOKEN` inside a
    workflow, not to a user PAT). A primary bucket is **volume**-sensitive, not
    rate-sensitive: the same fan-out at `-P 4` issues the same number of calls and exhausts
    the same 5000, just slower. Confirmed by the incident itself — the 22:13:30Z retry at
    `RED_MAIN_PARALLEL=4` failed on its **first** call. So parallelism stays at 12; the levers
    that actually apply are the pre-flight budget gate and stopping the sweep. Parallelism
    *is* the right lever for a **secondary** limit (GitHub's advice there is to make requests
    serially, and its documented ceilings are concurrency- and per-minute-shaped: no more than
    100 concurrent requests, no more than 900 points/minute to a single endpoint, where a
    `GET` costs 1) — which is why the throttle line names the class before naming a remedy.
  - **An empty second value is a FAILED READ, not a changed one.** The tip re-read used to
    feed straight into `[ "$tip" = "$tip2" ]`, so a refused re-read (empty `tip2`) rendered as
    `UNKNOWN — main moved mid-read (bd80272c -> )` — a confident wrong diagnosis that sent the
    operator to look at a branch which had not moved. Fail-closed held throughout (UNKNOWN,
    non-zero, verdict withheld), which is the only reason this cost an iteration rather than a
    false green; the *reason* was invented, and the tell was in the output all along — the
    arrow's right-hand side rendered **blank**. The empty case is now its own branch with its
    own cause. Generalise it: this file already forbids a suppressed error becoming a
    confident value, and a **confident diagnosis** is the same defect one level up. The same
    correction applies to the first tip read, whose message asserted `(wrong default branch?
    auth?)` — two guesses printed as a finding on a line that fires for every cause.
  - **`RED_MAIN_PARALLEL` (default 12) and `RED_MAIN_DEPTH` (default 10)** tune the fan-out width
    and how many runs per workflow are examined. Depth 10 is safe because the tip is the newest
    commit on `main`, so any at-tip runs are the newest rows in that workflow's own index —
    **depth can never hide an at-tip run**, it can only shorten the search for the newest
    *completed* one on the stale branch, and that case surfaces explicitly as
    `UNKNOWN@none (no completed run on main in the N examined)` rather than as a green — or, when the
    only completed runs were superseded, `no CONCLUSIVE run on main in the N examined (M superseded)`. The wider
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
    ⚠️ **Both are validated, and both were unvalidated for as long as they existed** — the guard
    that `RED_MAIN_BUDGET_RESERVE` has always had covered only that one of the three. Each of
    these two fails in its own way, and each failure below was reproduced against the SHIPPED
    detector rather than reasoned about — with one boundary worth naming, since this section is
    about not overclaiming: what was measured for `0` is that `xargs` accepts it and runs; that
    `0` *means* unbounded is GNU's documented semantics for `-P`, cited, not measured here.
    `RED_MAIN_PARALLEL=abc` makes `xargs -P` exit 1 with `invalid number "abc"` and run **zero
    children**, which the fan-out's `|| true` errexit backstop then swallows — so the whole
    fan-out is skipped and every workflow renders
    `UNKNOWN@none (per-workflow runs read FAILED for id N)`, **a positive claim about a read
    that was never attempted**, which is the one thing this detector may not print.
    `RED_MAIN_PARALLEL=0` is worse because nothing errors at all: `xargs` accepts it and runs
    (measured), and GNU documents `-P 0` as *run as many processes as possible* — so the
    fan-out's width becomes **unbounded rather than off**, precisely the opposite of what the
    SECONDARY rate-limit remedy above prescribes, reached by typing what reads as "off".
    `RED_MAIN_DEPTH=abc` reaches `per_page=` on all `qn` fan-out URLs, spending an account-wide
    budget the pre-flight gate has just approved on calls that cannot answer, under a header
    printing `depth=abc` as though that were what was read.
    So each is now rejected on **shape** — a whole number, at least 1 — and falls back to its
    default. **The same pass closed the sibling hole it uncovered**, and closed it in two
    places because it turned out to be two holes. `RESERVE` had carried the character class
    alone, so it now carries a numeric test as well — spelled `-ge 0`, because zero is a legal
    (documented, discouraged) reserve while a value the shell cannot compare is not a reserve at
    all. But `9223372036854775807` passes **both**: it is all digits, and it compares fine. Its
    damage is in the one **addition** the reserve reaches, `need + RESERVE`, which wraps
    negative — so `remaining < negative` is false, the pre-flight gate declines nothing, and the
    fence reads the repo on with the reserve effectively at zero, printing neither a decline nor
    a NOTE. Measured through this fence. That half is caught at the **gate**, where `need`
    exists to add, by detecting the wrap itself (a correct sum always exceeds a non-negative
    `RESERVE`) rather than by inventing a ceiling — an enormous but non-wrapping reserve is left
    alone, since "decline every repo" is something an operator may legitimately have asked for. Nothing is clamped to a ceiling: GitHub's own handling of an oversized `per_page`
    is not measured here, and asserting a clamp on an unmeasured ceiling would be the same
    overclaim in the other direction. **The rejection is printed**, naming the variable and the
    value: unlike the reserve's, whose fallback is self-revealing in the decline message, these
    two fall back invisibly behind a header showing the default, and a knob that ignores its
    input in silence makes the tick read as evidence about the fleet when it is evidence about
    the config. Pinned by `steward-red-main-throttle-fixtures-test.sh` cases `E11`–`E14`, of
    which `E14` is the load-bearing one: valid non-default values must reach the header
    unchanged, so a guard that rejected everything — turning a documented knob into a
    decoration — cannot pass. `E13b` pins `DEPTH`'s numeric half specifically (neither
    whole-guard mutation isolates it, because each reddens `E13` for a different reason), and
    `E10c` pins the reserve's over-range door.
  - **`RED_MAIN_BUDGET_RESERVE` (default 250)** is the third tunable, and it was undocumented
    here for as long as it existed — a knob named in an operator-facing message
    (*"plus the 250-call reserve"*) with nothing telling the reader it was theirs to turn. It
    is the number of GitHub API calls the pre-flight gate holds back for OTHER consumers of
    the same account-wide budget — coord's merge train, `/babysit-prs`, the runner, every peer
    session — **not** a safety margin for this function, which budgets its own worst case
    separately. A full fleet tick is ~113 calls across the watch set, so 250 is roughly two
    ticks' headroom left for everyone else; at that setting the steward stops reading at ~5%
    remaining, i.e. it goes blind slightly *earlier* than strictly necessary. That is the
    deliberate direction: declining a repo it could just barely have finished is the cheap
    error, half-reading it and starving coord is the expensive one. **Lower it only to buy one
    more repo in a shortfall you are watching, and put it back** — a `0` makes this steward
    the consumer that spends the fleet's last call. A non-integer value (an operator typo)
    falls back to the default rather than reaching the arithmetic expansion below, where a
    non-integer fails in **two different ways and neither is a clean error** (measured,
    bash 5.2): `12abc` is a hard `value too great for base` error in every shell, aborting
    the fleet's highest-severity detector mid-run; `not-a-number` tokenizes as three bare
    *identifiers*, so under `set -u` it aborts as an unbound variable and **without `set -u`
    it quietly evaluates to `0`** — silently disabling the reserve and handing coord and
    every peer session a spent budget, with nothing printed. That last one is the dangerous
    half and it depends on a shell option this fence does not control. The character-class test
    closes those three, which is why it is not an attempt to parse a number — and a fourth,
    an all-digit value the shell cannot **compare**, is closed by the `-ge 0` arm beside it
    rather than by the class — and a fifth, one it can compare but cannot **add**, is closed at
    the pre-flight gate instead. Both reasons are recorded at their sites.
    Its fallback is now **named** as well, alongside the other two: it was correct from the day
    it shipped and it was also silent, and the sentence that revealed it (*"plus the 250-call
    reserve"*) only ever prints on the path that declines the repo.
    ⚠️ **The RESERVE's three doors were each asserted where they REJECT and nowhere where
    they ACCEPT** — `PAR` and `DEPTH` have had an accept-side witness since `E14` shipped, and the
    reserve, which reaches the header nowhere, had none. That asymmetry is easy to miss precisely
    because the rejection arms look like thorough coverage. Three fixtures close it, each measured
    against a mutation rather than argued:
    `E15` is the reserve's own `E14` — a budget that affords the read on `need` alone and not
    once the reserve is added, so the reserve is the **only** thing that can decide the verdict
    (before it, `+ RESERVE` could be deleted from the gate with all 143 assertions green).
    `E10d` executes the `-ge 0` arm **itself**, which nothing did (`S7` now does too, under
    `set -eu`): `E10a`/`E10b` stop at the
    character class one test earlier, and `E10c`'s `INT64_MAX` **passes** this test and is
    caught two hundred lines later at the gate — so the arm and its sentence were dead to the
    suite on the day they shipped. `S7` drives `DEPTH`'s and `RESERVE`'s numeric arms under
    `set -eu`, which `S6` reaches for `PAR` only.
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
    **`return 2` is a NARROWING of that non-zero, never a new success.** It means *not
    adjudicated, because GitHub refused the read* — a rate-limit throttle, or a pre-flight
    decline because the remaining budget could not cover the repo. Every existing caller that
    treats non-zero as "not fully adjudicated" is correct unchanged; 2 exists so a caller that
    *wants* to can tell "this repo is unreadable right now" from "this repo was read and has
    UNKNOWN lines". **On a `2` whose line says PRIMARY exhaustion, STOP THE SWEEP** — the
    budget is account-wide, so every remaining repo will fail identically, and GitHub warns
    that continuing to call while limited can get the account banned. Report the unread repos
    as UNKNOWN with the reset time; do not emit N identical UNKNOWNs by trying each one.
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
  - **Cost: `4 + W + P + C` API calls per repo.** `W` = workflows in the union set (tip, workflow
    list, discovery window, tip re-read, then one per workflow); `P` = gating probes, which only
    AMBIGUOUS workflows incur; `C` = **descent compares for the `newer-*` annotation**, which only
    a line whose newest off-tip non-push run CONTRADICTS it incurs, memoised per `(from, to)` sha
    pair for the tick. `C` is reported in the footer beside `P` rather than folded into it,
    because the two are incurred by disjoint populations and a merged figure would hide which
    one moved, and it is separately capped so it can never consume the budget the mandatory
    reads need. It is expected to be **single digits fleet-wide and frequently 0** — the
    contradiction gate is what keeps it there, and deleting that gate makes `C` the same order as
    `W`, since roughly half a repo's workflows are adjudicated-and-stale at any moment. The 2026-07-31 pre-probe census was web 32, runner 21, coord 14,
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
    ⚠️ **The "≥9.0% of 5000/h" reading above is a SHARE, and a share is not headroom — do not
    plan against it.** That percentage is this one function's own consumption on its own
    census. The 5000/h is a **per-user budget shared by every tool, agent and session on the
    account**: coord's merge train, `/babysit-prs`, every `gh pr`/`gh run` call any peer
    session makes, and every other steward tick. `red_main` is one consumer among many and
    cannot see the others' spend, so its own 9% says nothing about what is left.
    **Measured 2026-08-25: the account hit `x-ratelimit-used: 5000` of 5000 — 100% — while
    this census predicted 9%.** The number to reason about is therefore the **measured
    remaining headroom at tick start**, which the tip read now reports in the footer, not any
    share computed here. Both this figure and the `4 + W + P + C` formula stay because they are
    still the right way to size *this* function; they were simply never a budget forecast.
    ⚠️ The census figures above **predate `C`** and were never re-measured with it. `C` is
    additive to every one of them, so read them as a floor for that reason as well.
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

  ⚠️ **Sweep the repos so the budget survives the sweep, and STOP the sweep on a primary
  exhaustion.** The GitHub budget is per-account, not per-repo, so the repos read LAST are the
  ones that go unadjudicated when it runs out — and the sweep order is arbitrary, so the repo
  that loses its verdict is arbitrary too. Measured 2026-08-25: coord and runner were read
  back to back, the account hit 5000/5000, and **`qontinui-web` — 21 open PRs, the most on the
  fleet — got no red-main verdict at all**. Two consequences:
  (a) **Read the repos that matter most first** — highest open-PR count, and any repo whose
  train is already held — so a budget shortfall costs the cheapest verdict rather than a
  random one. (b) **On a `red_main` exit of `2` whose line names PRIMARY exhaustion, stop.**
  The budget is account-wide, so every remaining repo will fail identically; calling them
  anyway buys N identical UNKNOWNs, starves coord and every peer session of the same budget,
  and GitHub warns that continuing to call while limited risks the account. Report the unread
  repos as UNKNOWN **naming the reset instant** from the throttle line, and read them next
  tick. A SECONDARY throttle is the opposite case: back off the stated seconds, drop
  `RED_MAIN_PARALLEL`, and continue — that one really is about burst, not budget.
  **Neither is a green, and neither may be quietly omitted from the report:** a repo that was
  never read is UNKNOWN, and an absent line must never read as OK — the same rule the detector
  itself enforces per workflow, applied one level up at the sweep.

  Once red, classify before acting (see `reference_coord_infra_cancelled_job_reds_main_holds_train`):
  a **flake / infra-cancelled job** rolls up to workflow `failure` and self-heals on a CI RE-RUN
  (the unblock is a re-run, NOT a PR); a **genuine regression** needs a fix PR — but neither a
  label nor a recovery-merge is what lands it, so do not reach for either.

  **The `failure`-side discriminator is STEP-LEVEL.** `cancelled` is the minority shape; the
  common infra kill is `conclusion: failure` with every step green, so classifying on the run
  `conclusion` alone drops a dying runner into the “author a fix PR” arm. Read the JOBS of
  the red run — **once, by hand, per investigated RED**. Do NOT fold this into `red_main`: that
  function sweeps every workflow on every repo every tick, and a per-run jobs fetch would
  multiply its API cost for a read it does not need in order to report `RED(failure)` correctly.

  ```bash
  gh api "repos/OWNER/REPO/actions/runs/<run_id>/jobs?per_page=100" --jq '.jobs[] | select(.conclusion == "failure" or .conclusion == "cancelled") | {name, conclusion, steps: [(.steps // [])[] | {name, conclusion}]}'
  ```

  ⚠️ **Run that spelling, not a shorter one — all three of its awkward parts are load-bearing,
  and this file is where the consumers copy it from.** `per_page=100`: the default is **30**, so a
  wide matrix is silently truncated and the job you are looking for may simply not be in the page
  — the fleet-wide steward is the reader most likely to hit that. `(.steps // [])[]`: `steps` is
  OPTIONAL on GitHub's job object, and a bare `.steps[]` **aborts the whole jq program mid-stream**
  (`Cannot iterate over null`, exit 5) on exactly the **Tier 2** job this classification exists to
  catch — printing a partial list that reads like a complete one. That one is guarded executably:
  `scripts/lint-command-frontmatter.py` **check #23** fails CI on the bracket-iteration spellings
  of it under `.claude/`, `scripts/` and `.agents/` — twelve canonical ones (`.steps[]`,
  `.steps | .[]`, `(.steps)[]`, `.["steps"][]`, `."steps"[]`, `.steps.[]`, `(.steps).[]`,
  `.steps[ ]`, `.steps | .[ ]`, `.steps? []`, `.steps?[]`, `.steps?.[]`) plus their whitespace
  variants. ⚠️ **The `?` ones are the trap worth knowing by hand: a `?` placed BEFORE the `[]` does
  not save you** — it suppresses the field access and leaves the null to be iterated, so the filter
  still exits 5 while *looking* null-safe. The `?` protects the iteration only when it lands AFTER
  the brackets — `.steps[]?`, and equally `.steps[] ?` or `.steps | .[]?`, all of which the check
  accepts alongside `(.steps // [])[]`.
  ⚠️ **It guards `[]` iteration and nothing else, so a green CI run is NOT a proof this filter is
  null-safe.** `steps` is just as null under `map`, `sort`, `keys`, `add`, `join`, `flatten`,
  `group_by`, `to_entries`, `any`, `all`, `unique`, `sort_by` and `min_by` — all re-measured to
  abort (jq 1.8.2), none guarded. (`first` and `last` were listed here on first shipping and are
  **exit 0**: jq defines them as `.[0]` / `.[-1]`, and indexing a null yields null. `limit` and
  `isempty` abort only in their real arities, where the array is iterated *inside* the call.)
  The trap in *this* file is `map`: rewriting the projection above as
  `steps: (.steps | map({name, conclusion}))` reads as a tidy-up, aborts identically on the
  Tier-2 job, and CI stays green. Use `(.steps // [])` whenever you change how the array is
  consumed, not only when you type brackets.
  It bounds a hand-edit; it does not prove the property — a dynamically built path
  is invisible to it, `.github/workflows/` is outside its roots, and `per_page` and the projection
  have no spelling narrow enough to guard at all. Those stay a review concern. And `conclusion` is
  projected into the output because Tiers 1 and 2 below are keyed on `failure` while `cancelled`
  is read elsewhere — a filter that selects both and prints neither cannot tell you which of the
  two you got.

  ⚠️ **Empty output is UNKNOWN, not “genuine failure”.** It means this run has no `failure` or
  `cancelled` job at all — the wrong `run_id`, a rollup whose red is somewhere else, or the
  **undispatched** run below — so go back and re-read the run's own conclusion before concluding
  anything about the code.

  ⚠️ **The third cause is the dangerous one, and the re-run reflex DESTROYS it: a run whose
  jobs were NEVER DISPATCHED.** Predicate, all **three** together: the run is **`completed`** —
  of ANY conclusion — and **every** job reads `status: queued` with `conclusion: null` and
  **zero steps**. ⚠️ **The run-level conclusion is NOT one of the conjuncts.** It was until
  2026-09-05, when this predicate read *“all four together: the run is `completed/failure`”*, and
  that fourth conjunct made the class literally unrecognisable on a run that rolled up any other
  way — which is how one sat undiagnosed for four steward ticks. The two conclusions MEASURED so
  far are `failure` (`qontinui-web` runs `33817996523` / `33817996519`, 2026-09-04) and
  **`startup_failure`** (`qontinui-coord` run `32984532063` on #1658's old head
  `9d3c3f80fe23e201de556e7e67ab3e5c2aae56fe`, 2026-08-26 — its one job `Gitleaks Secret
  Detection` read `queued`/`conclusion: null`/zero steps). Those are **instances, not the test**:
  the next variant must be recognised by this predicate as written, without a third plan. That is
  the same **allowlist-vs-denylist** lesson the arm-2 `rebase_block` row carries in the wedge-class
  table below — key on the shape you can state, never on an enumeration of the values you have
  happened to see — applied one predicate over.

  **Why the rollup conclusion was never load-bearing:** the JOB shape carries the whole signal.
  The tier table cannot see this class because all three tiers key on `conclusion == "failure"`
  and these jobs have **no conclusion at all** — a consequence of the jobs never being
  dispatched, and entirely independent of what the run rolled up to. So the filter above returns
  empty while the run-level conclusion reads `failure`, or `startup_failure`, or anything else.
  That combination reads as *“infra killed it, re-run it”* and it is the one case where that is
  wrong.

  **The executable twin — cite it, so prose and code cannot drift apart again.** coord already
  ships this predicate in exactly job-shape-only form: `ci_baseline::all_failing_jobs_undispatched`
  (`qontinui-coord` `crates/coord/src/ci_baseline.rs` — re-resolve with
  `git grep -n 'fn all_failing_jobs_undispatched' origin/main -- crates/coord/src/ci_baseline.rs`),
  which tests, for EVERY job (and at least one job), `job_status == "queued"`,
  `step_count == 0` and an absent-or-`"unknown"` conclusion, and reads **no run-level
  conclusion at all**.

  **Do NOT re-run it. The re-run is one-way.** Measured 2026-09-04 on `qontinui-web`, twice, 45
  minutes apart: runs `33817996523` (04:19Z) and `33817996519` (05:04Z) each went from
  `completed/failure` — re-runnable — to permanently `queued` with `jobs=0` and `run_attempt`
  still `1`; afterwards `POST .../cancel` answers `409` and `POST .../rerun` answers `403`, so
  the run can never reach a conclusion again and its red is now immortal. Three sibling runs on
  the same sha (`33817996512`, `33817996516`, `33817996522`) were deliberately left untouched and
  were still `completed/failure`, still re-runnable, 2.5h later — that contrast is the evidence,
  not the theory. Re-measured 06:47Z on `33817996512`: 6 of 6 jobs `queued`, `conclusion: null`,
  zero steps, and the documented filter returns nothing.

  ⚠️ **Whether a `startup_failure` run behaves the same under re-run is UNMEASURED — and it
  must STAY that way.** Everything measured above was measured on `completed/failure` runs; the
  only way to settle the other rollups is to manufacture a subject and risk making its red
  immortal, and nobody should. So treat the one-way hazard as applying to the **whole undispatched
  class regardless of rollup conclusion**. That is the conservative reading and it costs nothing,
  because the remedy on every arm is the same one anyway — *wait for the head to move*.

  **What to do instead:** classify it as UNDISPATCHED, leave the run alone, and treat the red as
  clearing only when `main` next MOVES — a fresh push dispatches normally, and dispatch failure
  is per-run rather than a capability outage (a diagnostic `workflow_dispatch` of an unrelated
  workflow on the same repo completed `success` in ~30s while this class was live, which is what
  rules the outage reading out). Say UNDISPATCHED in the ledger rather than “infra”, since the
  remedies are opposite. ⚠️ **coord HAS this predicate — this line used to say *“coord has no
  notion of this class either”*, and that is now FALSE.** `all_failing_jobs_undispatched` shipped
  as coord `8f0eb501` and is wired into the main-baseline ingest in `write_enriched_baseline`
  (`git grep -n 'all_failing_jobs_undispatched(&new_pattern)' origin/main -- crates/coord/src/ci_baseline.rs`).
  ⚠️ **The WIRE has landed too — this paragraph used to say it was still missing, and that is now
  FALSE.** The call site was conjoined on `conclusion == Some("failure")`, so a `startup_failure`
  run whose jobs were all undispatched matched neither write-skip and coord wrote it as a
  conclusive red baseline. Since coord `7e1e622b` (landed 2026-09-06 via **qontinui-coord#1981**)
  both write-skips are gated on `conclusion_is_unverdicted_rollup`
  (`git grep -n 'fn conclusion_is_unverdicted_rollup' origin/main -- crates/coord/src/ci_baseline.rs`
  for the predicate;
  `git grep -n 'conclusion_is_unverdicted_rollup(ev.workflow_run' origin/main -- crates/coord/src/ci_baseline.rs`
  returns its TWO call sites, the all-cancelled guard and this one),
  which admits `failure` and `startup_failure` alike, so such a run is skipped and the last
  conclusive verdict is preserved. That widening is the one
  `2026-09-04-coord-undispatched-ci-run-reds-main-permanently` reserved by name for
  `2026-09-04-undispatched-predicate-misses-startup-failure-and-coord-waits-forever` — the plan
  that wrote this correction. The sibling plan's own Phase 2 (the read-side
  `RerunClass::Undispatched`, so the red-main self-heal never re-runs such a run) landed as coord
  `01026dea`; its PR **qontinui-coord#1972** reads CLOSED because the content landed by
  fast-forward, not because it was abandoned.

  ⚠️ **A `cancelled` job the filter surfaces has NO row in the table below, and must not fall
  through it into “otherwise → genuine”.** The three tiers are all keyed on `conclusion ==
  "failure"`. ⚠️ **Where to read it depends on WHOSE run is red, and this table serves both.**
  On a **`main` baseline** — the case this section is written for — `cancelled` is the
  infra-cancelled class, its remedy is the re-run of the red-main remedies table's row 1
  (`RED(cancelled)`), and it stays `RED(cancelled)` in the verdict vocabulary. On a **PR's own
  head** that reading does NOT hold: a cancel there reached no verdict at all, must not be counted
  as a red, and its remedy is normally a rebase — see the **“`cancel` bucket misread as a
  failure”** row in the wedge-class table below, which measured two runner PRs skipped as “has
  failures beyond `security`” when the extras were cancels. Read it in whichever of those two
  places matches the run in your hand, not here.

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
  ⚠️ **The recovery-waiver lane still cannot be relied on — do not wait on it.**
  `is_recovery_candidate` needs `rebased_candidate_green`, whose only producer is
  `pr_merge::engine::head_has_green_speculative_candidate` (a green `coord.speculative_chains` row,
  fail-closed). Speculative candidate CI is ARMED in production since the arm PR of plan
  `2026-07-25-coord-speculative-push-before-gate-churn` §8.4 step 6 (2026-09-03, qontinui-coord#1894): `COORD_SPECULATIVE_DISABLED` is now an
  ordinary default-ON kill switch — `"1"` disables, unset arms, `deploy/taskdef.json` sets `"0"` —
  so that producer CAN produce rows and the waiver is no longer inert BY THAT CAUSE. What remains
  is the bootstrap gap plan `2026-08-20-coord-red-main-recovery-lane-is-inert` records: a
  Tier-4-blocked PR never gets a proposal, so no chain is ever built for it. The
  `fixer_arm_readiness::adjacent_breakages` entries `speculative_candidate_ci_disabled_in_prod` and
  `red_main_recovery_merge_lane_inert` now derive their state from the live flag read rather than
  asserting a prod value.
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
  it as though it were the merge mechanism — and set it over the REST labels route,
  `gh api -X POST repos/<owner>/<repo>/issues/<pr>/labels -f 'labels[]=coord:red-main-fix'`, since
  `pr_merge::labels_routes::validate_label` rejects it (that is the validator working, not another
  broken lane) and `gh pr edit --add-label` cannot set it either — `gh pr edit` prefetches
  `repository.pullRequest.projectCards` over GraphQL, GitHub refuses that under the Projects-classic
  sunset, and it exits 1 **before** the label is applied (gh 2.46.0, reproduced 2026-09-04). Same
  route and same reason as `.claude/skills/coord-pr-label/set-label.sh`.

**The fixtures — run them, do not read them.** `bash scripts/steward-red-main-throttle-fixtures-test.sh`
(this repo; its tracked `.sh` files are mode `100644`, so invoke through `bash`, never as a
program). It extracts the `red_main` fence **out of this file**, located by the `red_main() {`
marker rather than by line number or fence position, so it cannot go green over a copy that has
drifted from what the harness injects — and a missing marker is a **harness failure (exit 2)**, not
a green, because an empty extraction would make every arm below it pass vacuously. Hermetic:
fixture HTTP responses under `mktemp -d` and a counting `gh` shim first on `PATH`; no network, no
`gh` binary, no credentials, no fleet state.

| Arms | What they hold |
|---|---|
| `A*` | `rm_throttle_report`'s classification, including the ordering that makes a 429 carrying `remaining: 0` read **SECONDARY**; and that `rm_err_msg` and `rm_reset_at` **never render blank** |
| `E*` | `red_main` end to end: mostly over a refused read — both tip reads, the pre-flight gate at the boundary that separates the correct `2*qn + 1` from the `qn + 2` this gate shipped with, a garbage `RED_MAIN_BUDGET_RESERVE` — plus two healthy-tip arms (an instrumented 200 parses and hands a real sha forward; an UNKNOWN budget does not gate and does not render blank), and the **tunable guard** on all three of `RED_MAIN_PARALLEL` / `RED_MAIN_DEPTH` / `RED_MAIN_BUDGET_RESERVE` — including `E14`, the anti-vacuity control that fails if a valid non-default value is rejected; `E13b`, the witness that isolates `DEPTH`'s numeric half under `set -u` (`S7` is a second, under `set -eu`); and `E10c`, the reserve value that passes **both** halves of the guard and wraps in the gate's arithmetic; `E10d`, the uncomparable reserve that reaches the `-ge 0` test itself — the only arm that does so under `set -u`, `S7` being the one that does under `set -eu`; and `E15`, the pair pinning the reserve's **accepted** path — the only arms where a valid reserve is what decides the verdict. `E11`/`E12`/`E12b`/`E14` each pair their header assertion with the `-P` an `xargs` shim RECORDED: for the `0` and `00` halves, which are measured to error nowhere, that recording is the only non-header witness there is; and `E16`, the arm that reaches the fence's **second** `xargs -P "$PAR"` — the gating probe, which no arm executed until it, and which needs a fan-out COUNT and an all-invocations width because both spawners share one recorder |
| `N*` | the **`newer-*` annotation**: the four values `compare` can return, tested `= ahead` so `behind` (`N3`) and `identical` (`N4`) are refused as firmly as `diverged` (`N2`) — a `!= diverged` implementation passes `N1`/`N2`/`N5` unchanged; the symmetry (`N6`, a stale GREEN whose newer evidence is RED, the only arm that reddens if the note is re-gated to lines already carrying a RED); the two cost properties (`N7`, contradiction-only, the only arm a fence calling unconditionally fails; `N8`, memoised per `(from, to)`); and the budget skip being REPORTED rather than silent (`N9`). `N10` is the **platform** arm: it runs the whole fence against a `jq` shim that emits CRLF, because jq's stdout is text-mode on Windows and the LAST tab-separated field of each jq-written TSV then reaches the shell with a `\r` — which does not mis-space the note, it makes `success` miss the `success)` label arm and prints `newer-red` over a green run. It shipped green on linux and red on `guard-roster-windows`, so a linux-only leg is not evidence for this class |
| `S*` | the **shell-option contract** — the two instrumented tip reads, the two `if`-form dispatches, the zero-match guard on the workflow-id filter, and the tunable guard's `\|\| { … }` rejection arms — **all three of them**, across `S6` (`PAR`) and `S7` (`DEPTH` and `RESERVE`, whose arm returns rc **2** rather than 1) — driven under `set -eu` in a child shell. Six of the fence's **eight** errexit defences; the two `xargs` guards are declared uncovered there rather than counted. `S4` additionally asserts the fetch fan-out RAN — every other needle it carries is printed at or after the re-read, so all of them survive a fan-out that was skipped entirely, which is precisely what `E11` measures an unguarded `PAR` to cause — and `S1` the complement, that a refused tip read reaches **no** fan-out, so a repo whose tip could not be read costs no share of an account-wide budget. `S6` and `S7` follow the REPLACEMENT value — the one the guard substitutes for a rejected tunable — to the spawner and to the request under `set -eu`; every other needle they carry is something the guard itself printed, which is the shape a fence sanitising only its own output passes outright |

⚠️ **Edit the fence and re-run it; the `S*` arms are why that is not a formality.** All three
defects this suite found on its first run were in *shipped* code that four rounds of review had
read as correct. The `set -e` hardness above was likewise restored by review alone, and **under
`set -u` a reverted dispatch is byte-identical to the correct one** — same exit code, same text —
so it is invisible to every other arm here. Under `set -eu` it loses the `cause:` line while `rc`
stays `1`, and on the no-capture paths it returns **`2`** for a read that was merely unreadable —
the exit this file defines as *"not adjudicated, because GitHub refused the read"*, answering the
one question that exit exists to answer wrongly and silently. (It does **not** trip item 4's
`STOP`, which is conditioned on a `2` *whose line names PRIMARY exhaustion*; the revert prints no
line at all.)

Build the per-PR + fleet snapshot. Then classify each signal against Tier 1; anything Tier 1
cannot classify goes to Tier 2.

## Step 2 — Tier 1: deterministic reflexes (known wedge classes, NO LLM reasoning)

A rule table over the *remaining* wedge taxonomy (post-Phase-1/2). Each rule = detector →
bounded, idempotent remediation (or escalate). **Reconcile this table with `/babysit-prs`
Step 4d** (`.claude/commands/babysit-prs.md` → the step headed **"4d. Classify"**, whose
table keys on `block_reason_code`; cite it by that name, **never by line number** — the two
line-number pointers this paragraph and the *Stale read* row below used to carry both rotted
the moment that file grew, and had come to land inside an unrelated step. Digits are omitted
here deliberately, so this warning is not itself a hit for any future grep against that
pattern) — it is the same taxonomy keyed on the real
`block_reason_code` set; keep ONE source, don't let them drift. In `--mode=observe`, print
the intended remediation and do nothing.

⚠️ **The same rule now governs every citation into `qontinui-coord` in this file, and it had to
be applied there the hard way.** This file used to point at `crates/coord/src/mcp/tools.rs`,
`crates/coord/src/merge_scheduler.rs` and `crates/coord/src/ci_baseline.rs` by absolute line
number. Re-verified against coord `origin/main` (`f8f84494`) on 2026-09-04, **eighteen of the
nineteen had rotted** — `tools.rs` alone is a ~55,600-line file under continuous change, and a
sibling worktree measured that week was 4,025 lines short of `main`, so a pointer into it decays
within days. Only the `leader.rs` TTL pointer still resolved. **A rotted line number does not
fail — it lands silently inside unrelated code and reads as a citation**: one of these had come
to rest on a comment about `coord_skill`, another inside a different function entirely, and a
reader who followed it found prose that neither confirmed nor denied the claim. Worse, the
numbers were *hiding a claim that had gone false*: the `rebase_block` enum had grown a ninth
member while this file still said eight, exactly the falsification its own next sentence
predicted (see the **Already-landed empty-diff** row).

**So: cite coord by SYMBOL, never by line number.** Every coord citation below names a token you
can resolve for yourself from a coord checkout — the form is
`git fetch origin main` then `git grep -n '<symbol>' origin/main -- <path>`, and the citations
carry that command inline so the pointer re-resolves itself. A symbol that stops resolving is a
LOUD failure that tells you the code moved; a line number that stops resolving is a silent one
that tells you nothing. Structural facts that no single token names (the numbered `Phase`
banners inside `recover_orphaned_proposals`, say) are cited by the enclosing function plus the
banner text, on the same principle. If you add a coord citation to this file, add it in that
form — and if a symbol grep returns a count that contradicts the prose (nine variants where the
text says eight), **fix the prose, do not re-point the citation**: that mismatch is the only
alarm this file gets, and it is worth more than the pointer. (No `file:NNNN` example appears in
either of these two warnings, deliberately, so a grep for the pattern does not hit the warnings
against it.)

| Wedge class | Detector | Remediation (bounded, idempotent) |
|---|---|---|
| **Green-but-dirty** PR (behind main / needs rebase) | `mergeable_state=dirty`/`behind` or `freshness_next_action=rebase` + CI green. ⚠️ **A CLEAN `mergeStateStatus` does NOT rule out a coord rebase conflict — for one live PR class it asserts the OPPOSITE of the truth.** GitHub tests a **MERGE** (which trivially takes both sides); coord performs a **REBASE** (which replays commits). For a branch whose content already landed on `main` as a single squashed/verbatim commit, replaying its file-creating commit add/add-conflicts forever while the merge test stays clean. Measured on `qontinui-dev-notes#148` (2026-08-19): `gh pr view` reported `mergeable: MERGEABLE, mergeStateStatus: CLEAN` while coord held a **terminal `conflict`** on the same PR, stuck 30.8h. The decisive test is per-path **blob comparison** between the PR head and `origin/main` (`git rev-parse <head>:<path>` vs `git rev-parse origin/main:<path>`) — **`git cherry` also fails here**, because a squash landing destroys patch-id equivalence while preserving content equivalence: all 8 of #148's commits read `+` while the file was byte-identical at blob `32f17375`. A **second, independent** `git cherry` failure — the *merge-forward* shape, where the landed twins are pulled into the branch and so are excluded from cherry's comparison set — is measured in the **Already-landed empty-diff** row below. Neither is fixable: `git cherry` is not a land proof, in either direction. Anywhere below that triages on `mergeStateStatus`, read it as "GitHub's merge test passed", never as "coord can rebase this". | **CI-DURATION-AWARE — do NOT blind-rebase (that's the eager-churn trap).** (1) **Is it even yours to fix?** If the PR is merely *behind* main and coord's dry-rebase resolves it (no `could not apply`), LEAVE IT — coord auto-rebases the candidate at land; a manual rebase only resets CI to do coord's job. Only a TRUE textual conflict (`CONFLICTING` / `could not apply`, confirm via `git merge-tree`) needs hands. **If this session does not own the branch, those hands are not yours** (`git-operations` `own-artifact-lifecycle`): run `/handoff-stuck-pr <owner/repo#N>` — plan `2026-09-06-fleet-scale-stuck-pr-conflict-handoff-protocol` — instead of resolving it, and let the author session or its coord-dispatched successor do it. ⚠️ **And one shape is neither a hard conflict nor coord's to resolve:** attempts climbing while coord's `last_error` text stays byte-identical is the merge-commit replay shape — see **Stranded conflicts that never converge** below. That one never converges on its own, so waiting is not a remedy. (2) **Gate the timing on the repo's candidate-CI p90** (from `coord_query_merge_economics`): **short-CI (p90 < ~30m) → resolve EAGERLY** (rebase in a worktree → re-verify (build + lint + format-check — see **Silent semantic conflicts** below; absence of `<<<<<<<` markers is NOT re-verified) → `--force-with-lease` (a push to the PR's branch: run `coord-ff-lands.md` → "Pushing to a branch whose PR may already have landed" before and after it, re-testing the PRE-rebase head — a PR coord landed between polls, even one GitHub still shows OPEN, carries nothing, and the rebase then re-proposes landed commits) → let coord land; re-resolution is cheap). **Long-CI (p90 ≥ ~30m; runner ~2h) → resolve JUST-IN-TIME, only when the PR is at/near the FRONT of the land queue** — a rebase resets a full ~2h CI and any sibling land re-dirties it, so resolving deep-in-queue = wasted CI (the churn tonight's audit measured: 82% of candidate CI wasted, 24/24 green). (3) **Overlapping cluster:** when several PRs conflict in the same files, STACK them (`coord:stacked-on=`) or land as a coord batch so they resolve ONCE, not N times. Never `gh pr merge` — coord is the merge authority once clean+green. Rebase mechanics = `/babysit-prs` Step 6 item 2 (rebase in a worktree, `--force-with-lease`). |
| **Red-on-a-stale-base** PR (red ONLY because `main` was red when its run was built — `main` is green now, the PR is not) | ⚠️ **READ THE FAILING JOB'S LOG FIRST — the token `ci-failed-stale-base` does NOT mean "red because `main` moved".** Arm (b) below marks a PR stale by DISTANCE alone, whatever it is red on. Measured 2026-09-23 on six coord PRs carrying it (`#1795`, `#1829`, `#2271`, `#2319`, `#2331`, `#2341`): `main` had moved 132–307 commits and `base_red_workflows: []`, a case `behind_handler::run_stale_ci_base_pass` deliberately skips when the PR is not `BEHIND` and `main` is green (`StaleCiBaseSkip::DistanceArmNotShipped`). The cheap first read is `base_red_workflows` from `coord_reevaluate_dry`: EMPTY means arm (b), distance only — classify from the log and expect no coord freshen. The job logs showed **4 red on the PR's OWN code** (`#1795` a `schema_read_contract` allowlist, `#1829` its own assertion, `#2319` devenv tests, `#2331` clippy `string_slice`) and **2 CI infra** (`#2271` a DB-test step timeout, `#2341` a self-hosted runner that lost communication — both re-run). An update-branch would have fixed **0 of 6** and cost 6 CI runs on a 125-deep queue. **Only a failing assertion that names a path the PR never touched, or that passes at `main`'s tip AND fails at the run's base, is this row's class**; own-code red goes to the author or a Tier-2 fix. Infra red (runner lost communication, a step timeout) → `gh run rerun <run-id> --failed`: the re-run trap in the Remediation cell below applies only to a GENUINE stale base — here the block is the red check itself, not the base, and coord's candidate CI re-tests against current `main` at land. Evidence: coord finding `9d82e73c`, and the measurement appended to plan `2026-09-08-a-red-pr-with-no-recorded-ci-base-can-never-be-measured-stale`. **Since Phase 2 (below) the train reports this class as `ci-failed-stale-base` and the predicate surface as `ci-not-green-stale-base` — filter on THOSE. ⚠️ A histogram or dashboard filter on `ci-failed` EQUALITY, or on `block_reason_code == "ci-not-green"`, returns ZERO rows of the very class this row exists to remediate.** The bare pair is what you see when NEITHER Tier-2 arm fires — the base is not known-red-with-`main`-green (arm a), and `main` has not moved past coord's GLOBAL `ci_stale_base_commits_threshold` (arm b, default `CI_STALE_BASE_COMMITS_THRESHOLD` = 100, `0` disables the arm — env-tier only, `QONTINUI_CI_STALE_BASE_COMMITS_THRESHOLD` on the coord process, so do NOT look for it in tenant merge settings or the admin UI: a per-tenant column is `coord.*` DDL nobody has landed). Do NOT reason from the title of plan `2026-09-08-a-red-pr-with-no-recorded-ci-base-can-never-be-measured-stale`: its Phase 3 REMOVED that limitation, and arm (b) needs no recorded base at all, so a no-base PR 100+ commits behind IS measured stale today. When neither arm fires: the train reports it as **`ci-failed`** — the `classify_merge_status` token on the dashboard cell and in `coord_query_train_activity`'s verdict histogram — while the same PR's `predicate_eval.block_reason_code`, its merge-gate Check Run and `coord_pr_merge_verdict` spell it **`ci-not-green`**, `BlockReason::CiNotGreen`'s `code()`. Two vocabularies at each level, one PR; resolve both from a coord checkout with `git fetch origin main` then `git grep -n -e '"ci-failed"' -e '"ci-not-green"' origin/main -- crates/coord/src/pr_merge/`. What makes it THIS row: the PR's **newest failing run was CREATED before the current `main` tip** — `gh run list -R <owner>/<repo> --branch <head> --json name,event,conclusion,createdAt,headSha --limit 30`, take the newest `pull_request` row with `conclusion: failure`, and compare its `createdAt` against `git log -1 --format=%cI origin/main`; a run older than the tip was built on a merge commit against a `main` that no longer exists, and every `pull_request` run tests the merge of the head with `main` AS IT STOOD WHEN THE RUN WAS CREATED. ⚠️ **That comparison is NECESSARY, NOT SUFFICIENT** — on an active repo nearly every red PR's run predates the tip, and `%cI` is the tip's COMMITTER date rather than when it became the tip, so it is a screen. Confirm with the tip-vs-base test in the Remediation cell before classifying. The stale-base plan's Phase 2 HAS LANDED (plan `2026-09-05-a-red-pr-is-classified-before-coord-looks-at-behind-so-a-stale-base-is-never-refreshed`, coord finding `f7ca10dc`, topic `merge-engine`), so **the tokens are now the detector and the hand derivation above is the fallback** — use it when the token is absent, and as a cross-check when it is present. The predicate surface spells this class `ci-not-green-stale-base` (`BlockReason::CiNotGreenOnStaleBase`, `predicate.rs`) and the histogram spells it `ci-failed-stale-base` (`CI_FAILED_STALE_BASE_STATUS`, `pr_merge/mod.rs`, whose `classify_merge_status` arm sits deliberately ABOVE the first-match-wins `ci-failed` arm that would otherwise swallow it); `git grep -n CI_FAILED_STALE_BASE_STATUS origin/main -- crates/coord/src/pr_merge/` from a coord checkout is the one-line confirmation that the build you are reading has it. Coord itself could NOT see this class before Phase 2, and the reason it could not is why the new token exists rather than a `behind-base` one: the CI tier of `is_simple_green_path` returns `CiNotGreen` before the BEHIND tier is ever consulted, and `classify_merge_status` is first-match-wins with `ci-failed` above `behind-base`, so the histogram is **structurally incapable of saying `behind-base` about a red PR** — measured 2026-09-05 on qontinui-web: `idle_blocked`, `ci-failed 17`, coord's `why` reading "these PRs need their authors", and eight of the 17 were fossils of one 14 h 35 m red window on `main` (2026-09-02 05:44Z to 20:19Z), still red three days after `main` was fixed. This is NOT the `main-red` class (there `main` is red NOW; here it was red for a window and the PR cached the window as its own verdict). | **Classify FROM THE JOB LOG, never from the shared step name — a same-step cluster is not homogeneous.** On 2026-09-05 ten qontinui-web PRs failed the same `Run Tests` step and split three ways once the logs were read: **3 pure stale-base** (#1230/#1223/#1203 — the failing assertion named a workflow file none of them touched, and the same test passes at `main`'s tip); **5 ALSO carrying a forked alembic head** (#989/#1143/#1216/#1218/#1210 — the 16-test cascade off `test_the_real_repo_chain_has_exactly_one_head`; repair by hand-repointing the branch revision's `down_revision`, its `Revises:` docstring and any `_PARENT_REVISION_ID` test pin onto the new head — **never `alembic merge`**, which papers over the fork with a second head instead of re-linearising it); **2 genuine own-defects** unrelated to the window (#1241/#1114) that a rebase would not have cured. The stale-base proof is executing the failing test at `main`'s tip (passes) and at the base the run was built on (fails), or confirming the assertion names a path the PR's diff never touched. ⚠️ **TIER-1 TRAP — a GitHub RE-RUN DOES NOT WORK; do not reach for the `rerun_failed_jobs` reflex.** A re-run acts on the existing run record, whose `pull_request` `head_sha` is the merge commit built when the run was CREATED, so it re-tests the same stale base and returns the same red — after another ~60 min on qontinui-web's `Run Tests`. Coord's own re-run doors confirm it by exclusion: `rerun_failed_jobs` is called only by `maybe_rerun_red_main` (red MAIN — and do NOT quote the old “both arms default-off” gloss onward: `auto_fix_red_main` is default-**ON** since plan `2026-09-06-ci-rework-lane-unreachable-and-red-main-fix-never-consults`, only `auto_fix_red_main_flaky` is default-off. The exclusion rests on the single CALL SITE, not on a dial), `settle_watcher`'s `attempt_rerun` runs only in the post-merge settle window, and `ci_dispatch.rs` ships dark — `git grep -n -e 'fn rerun_failed_jobs' -e 'fn maybe_rerun_red_main' -e 'fn attempt_rerun' origin/main -- crates/coord/src/`. **Only a PUSH creates a fresh merge ref, so the remedy is rebase onto `origin/main` and `--force-with-lease`** (mechanics = `/babysit-prs` **Step 6 item 2**, the same pointer the *Green-but-dirty* row above uses — NOT Step 5 lever 3, which is *fresh hydration* — a no-op label touch or a push carrying a legitimate commit, never an EMPTY commit just to poke coord; a coord-allocated worktree, never a shared checkout), then let coord land. This is the red-CI carve-out from the *Green-but-dirty* row's LEAVE-IT: "coord auto-rebases the candidate at land" is true only for a PR that reaches enqueue, and a red PR never does — red → coord refuses to enqueue → no proposal → no candidate → the base is never refreshed → CI never re-runs → still red; the refresh lives downstream of the gate the stale red holds shut — **that chain is the PRE-Phase-3 history**; the freshen arms below are what broke it, and they run OUTSIDE the enqueue path. The same CI-duration discipline applies (long-CI repo: rebase the cluster together so sibling lands do not re-dirty each; stack with `coord:stacked-on=` where diffs overlap). **The plan's Phase 3 HAS LANDED, so the freshen is coord's — but read WHICH ARM before you believe it will fire.** `engine::behind_candidate_head` does admit the stale-base variant into the durable BEHIND freshen (`behind_handler.rs`: head-keyed backoff `BACKOFF_BASE`/`BACKOFF_MAX`, per-PR `MAX_ATTEMPTS` ceiling escalating via `BEHIND_FRESHEN_CEILING_REASON`, i.e. `behind-freshen-ceiling`) — **behind its `BEHIND` gate, and the sweep's SQL prefilters `BEHIND` too, so on a tenant that never reports `BEHIND` that admission never fires**: measured 2026-09-13, qontinui-coord #2091/#2094/#2096/#2097/#2098 held the stale-base verdict ~29h after `main` went green and a hand `update-branch` was the only remedy. The arm that reaches those PRs is the THIRD one, `behind_handler::run_stale_ci_base_pass` (plan `2026-09-08-a-red-pr-with-no-recorded-ci-base-can-never-be-measured-stale`, Phase 4): it enumerates open NON-`BEHIND` PRs whose newest persisted verdict is `ci-not-green-stale-base`, re-derives it fresh, and shares the attempt state, ceiling and backoff with the other two arms. **Its admission set is NARROW, so do not wait on it blindly**: arm (a) ONLY (the SQL requires a non-empty `red_workflows`; the distance arm is `distance_arm_not_shipped`), `main` green now, the verdict at least `STALE_CI_BASE_MIN_VERDICT_AGE_SECS` (3 h) old, GitHub reading the PR `mergeable: true`, no live `BranchName`/`CiWait` claim, and at most `STALE_CI_BASE_PER_TICK_PUT_LIMIT` (2) PUTs per leader tick. Kill switches, in the order to read them: the pass runs inside the behind-update sweep's leader-gated tick, so **this node not being leader, or `COORD_BEHIND_UPDATE_SWEEP_DISABLED=1`, kills the BEHIND sweep AND this third arm** before `COORD_STALE_CI_BASE_FRESHEN_DISABLED=1` is ever reached — arm 2, the stale-green freshen, is NOT in that tick: it is event-driven from `merge_scheduler::handle_verify_fail` and that env flag does not touch it — and all of them are coord-process env on the LEADER node, not anything on the box you are sitting on. ⚠️ **None of this reaches the bare-pair sub-class above**: arm 3 enumerates only PRs whose newest verdict code IS `ci-not-green-stale-base`, so a PR coord never labelled stale is freshened by nothing and the manual rebase stays its only remedy. For the PRs coord DID label, the steward's job is now **watching the `behind-freshen-ceiling` escalation** for the PRs coord gave up on, the same hands-off posture the *Green-but-dirty* row takes for the merely-behind green population, and the manual rebase above is what you do for the PRs that reach that ceiling; on a **watch-only** repo nothing freshens (see the watch-only carve-outs below), so there the manual rebase stays the remedy. In `--mode=observe`, print the per-PR classification and do nothing. |
| **Already-landed empty-diff PR** (re-proposed forever) | **FOUR ARMS — read all four; each later arm exists because the earlier ones cannot see a land shape the fleet actually produces.** **ARM 1 (`changedFiles==0`):** `changedFiles=0` + non-draft, **or** `coord_pr_status` reports `merged_at`/`merge_commit` while `pr_state=open`. ⚠️ **That second disjunct selects a population conjunct A can NEVER satisfy, and that population is ARM 3's.** `merged_at`/`merge_commit` served while `pr_state=open` is precisely the sha-rewriting land, and GitHub freezes such a PR's diff against its recorded base sha — so `changedFiles` stays NON-zero indefinitely and arm 1's proof can never complete. The disjunct **stays**: it still correctly admits the genuine `changedFiles==0` ff-land where A does hold (web#1033), and arm 3 is reached only when `changedFiles > 0`. When this disjunct admits a PR whose `changedFiles > 0`, do **not** decline it on A — fall through to arm 2's detector, then arm 3's. **ARM 2 (rebase-landed, FROZEN non-zero diff):** non-draft, **open**, `changedFiles > 0`, and **`coord_pr_status` serves EITHER member of a TWO-MEMBER ALLOWLIST — (i) `rebase_landable: false` together with `rebase_block == "already_landed"`, or (ii) `block_reason_code == "already-landed-by-content"`** (member (ii) added 2026-09-06; read the `block_reason_code` warning below before touching it). ⚠️ **Member (ii) does NOT come with `rebase_landable: false`, and requiring it there would make the member inert** — it is the merge PREDICATE's verdict, not the rebase classifier's. `BlockReason::AlreadyLandedByContent` (`git grep -n 'AlreadyLandedByContent' origin/main -- crates/coord/src/pr_merge/predicate.rs`) is raised by Guard 3 of `delivers_no_change_block` off `PrSnapshot::already_landed_proof_at_head`, whose own doc describes arm 2's population in coord's words: the proposal *"parks in `status='conflict'` and never reaches `merged`"* and *"this PR's `changedFiles` is NONZERO against its own base ref"*. Same proposal row as member (i), different code path, so the pair is one fact read twice — and **both members stay EQUALITIES**: the allowlist grew by one NAMED member, which is not the act of relaxing it into a denylist. `empty_candidate` still cannot satisfy either (Guard 3 keeps the detail only when `failure_class` classifies it `AlreadyLanded`), so C16's refusal is untouched. ⚠️ **Member (ii) now has BOTH a measured live card and a fixture pin — this sentence used to deny both, and was doubly wrong.** The suite gained a fourth detector input `MT_BRCODE` (`block_reason_code`) on 2026-09-07, and **C24** routes the C11 shape to arm 2 on member (ii) ALONE, with `rebase_block: none` and `land_stamp: none`, so a member (ii) that is absent, mis-spelled, or wrongly conjoined with `rebase_landable: false` fails there. The live card is `qontinui-web#1230`, from a sweep of all **168** open non-draft PRs on 2026-09-07 (coord finding `aa11904e-d074-4073-a622-39d0987363f0`, topic `pr-merge`; re-read fresh at 16:47Z): `pr_state: open`, `rebase_block: already_landed`, `rebase_block_disposition: terminal`, `rebase_landable: false`, **`block_reason_code: already-landed-by-content`**, `block_reason_repeat_count: 33`, `land_stamp: superseded_head`, `merged_at: null`, `merge_state_status: BEHIND`. In that whole sweep `rebase_block: already_landed` = 1, `already-landed-by-content` = 1 (the same PR), `empty_candidate` = 1, `land_stamp: current_head` = 0. ⚠️ **Both allowlist members are present on that one card, and that CORROBORATES the pair rather than making member (ii) dead weight — do not delete it on this evidence.** It is exactly the *"one fact read twice — same proposal row, different code path"* claim argued above: member (i) comes from `classify_rebase_block`, member (ii) from Guard 3 of `delivers_no_change_block` off `PrSnapshot::already_landed_proof_at_head`. Different code paths, so a card carrying member (ii) ALONE remains possible — and that population is precisely what C24 pins. Member (i) is the card's own expression of a terminally-failed proposal (the doc comment on `PrStatusCard`'s own `rebase_landable` field in `crates/coord/src/mcp/tools.rs` — `git grep -n 'pub rebase_landable' origin/main -- crates/coord/src/mcp/tools.rs` — reads *"`Some(false)` = **the LATEST PROPOSAL failed terminally** (see `rebase_block`)"*) whose cause is the already-landed marker. ⚠️ **Read "terminally" as scoped to the PROPOSAL, never to the PR — that is a THIRD independent reason `rebase_landable: false` alone is not this signal**, on top of the two present-tense arms named below. For `rebase_ci_failed` the proposal really did fail terminally and coord cut a fresh candidate ~17 minutes later unprompted (measured 2026-09-06 on `qontinui/qontinui-runner#1387`: card at ~03:02Z read `rebase_ci_failed` / `rebase_landable: false`, coord cut candidate `merge-candidate/01a074b8…` at 03:17:41Z with no human or agent action between). This steward printed *"coord holds `rebase_block: rebase_ci_failed`, so it will not land"* into an operator-facing ledger on exactly that misreading. **The card now answers it directly: `rebase_block_disposition`** (`git grep -n 'pub enum RebaseBlockDisposition' origin/main -- crates/coord/src/mcp/tools.rs`) — `coord_retries` | `author_acts` | `terminal` | `unknown`, `null` when `rebase_block` is `none`. Arm 2's population is `terminal` (`already_landed`, `empty_candidate`, `empty_candidate_superseded` — on coord `origin/main` since `596745570`, plan `2026-09-05-coord-empty-candidate-unknown-withholds-a-decidable-net-effect` Phase 3 — and the base refusal `stranded_on_absent_parent` are its members, and only the first is admitted here); a `coord_retries` card is coord's own work in progress and is **never** an arm-2 close, whatever its `rebase_landable`. The disposition is a WHO-ACTS column, not a second detector: it does not widen or narrow either member of arm 2's allowlist, both of which stay EQUALITIES. ⚠️ **This conjunct used to read "coord holds the PR in its structured terminal `conflict` status", and that was wrong in two independent ways.** *Unobservable:* `conflict` is `coord.merge_proposals.status`, an INTERNAL column the card projects away — the `PrStatusCard` struct (`git grep -n 'pub struct PrStatusCard' origin/main -- crates/coord/src/mcp/tools.rs`) carries **no proposal-status field at all**, and `classify_rebase_block` CONSUMES the proposal status rather than storing it — it takes it as a bare `status: &str` parameter and returns only the triple `(rebase_landable, rebase_block, rebase_block_detail)` (`git grep -n 'fn classify_rebase_block' origin/main -- crates/coord/src/mcp/tools.rs`). A steward looking for a field equal to `"conflict"` finds none. *Too broad:* several classes are producible inside that single `"conflict"` arm — `merge_resolution_discarded`, `rebase_conflict`, `already_landed`, `empty_candidate`, `rebase_ci_failed`, `infra`, `empty_candidate_superseded`, `unknown` (the `"conflict"` arm of `classify_rebase_block`; re-count rather than trusting this list; resolve with the `git grep` above) — and only `already_landed` is arm 2's population. **This row used to say FIVE, omitting `merge_resolution_discarded`; that was measured false against coord `origin/main` on 2026-09-04.** Measured 2026-09-02: the detector as written matched nothing on four live arm-2 candidates, which were instead declined on conjunct **A** — arm 1's conjunct, structurally unsatisfiable here. ⚠️ **Each member of the test is an EQUALITY, i.e. the pair is an ALLOWLIST, and must stay one.** Member (i) is the `already_landed` equality argued here; member (ii) is the `already-landed-by-content` equality argued above, and the same reasoning governs both. `rebase_block` is a **fourteen**-member enum on coord `origin/main` as of 2026-09-26 (`git grep -n 'pub enum RebaseBlock' origin/main -- crates/coord/src/mcp/tools.rs`): `none`, `base_not_default`, `stacked_on_open_parent`, `stranded_on_landed_parent`, `stranded_on_absent_parent`, `conflicting_head`, `rebase_conflict`, `merge_resolution_discarded`, `rebase_ci_failed`, `already_landed`, `empty_candidate`, `empty_candidate_superseded`, `infra`, `unknown` — the fourteenth, `empty_candidate_superseded`, landed with plan `2026-09-05-coord-empty-candidate-unknown-withholds-a-decidable-net-effect` Phase 3 (coord `596745570`). ⚠️ **This row said EIGHT until 2026-09-04 and NINE until 2026-09-23, predicting its own falsification BOTH times — coord duly added the ninth (`merge_resolution_discarded`), then the TENTH (`infra`), and the ALLOWLIST survived both unchanged.** That is the whole argument for the allowlist, now measured TWICE rather than reasoned: it survived two variants it had never heard of, while a denylist of "the classes to refuse" would have gone stale on each. ⚠️ **`infra` is LIVE, and its disposition is `coord_retries` — so it is never an arm-2 close.** Measured 2026-09-23 on `qontinui-coord` #2378 / #2397 / #2399: all three served `rebase_block: infra` with `rebase_block_disposition: coord_retries` and the detail *"terminally failed by awaiting-ci churn guard: candidate CI never converged before the flip-proof cumulative awaiting-ci deadline"*, while their `rebase_checked_at` had all moved within the preceding 15 minutes — coord's own work in progress, whatever the fleet route's `blocking_summary` calls it (it called all three "orchestrator stalled"). Two of the ten are traps a five-member mental model misses outright: `base_not_default` and `conflicting_head` are **present-tense** arms — Step 1 of `classify_rebase_block`, returning `RebaseBlock::BaseNotDefault` and `RebaseBlock::ConflictingHead` (`git grep -n 'RebaseBlock::BaseNotDefault' origin/main -- crates/coord/src/mcp/tools.rs`) **before** the proposal-derived ladder of Step 2, and outranking it, and **both also set `rebase_landable: Some(false)`** — so `rebase_landable: false` ALONE does **not** entail a terminal proposal and is not this signal. `already_landed` is by contrast reachable **only** from the `"conflict"` arm (`git grep -n 'RebaseBlock::AlreadyLanded' origin/main -- crates/coord/src/mcp/tools.rs` — exactly one CONSTRUCTION outside `#[cfg(test)]`, and ⚠️ **it is `C::AlreadyLanded => RebaseBlock::AlreadyLanded` inside `impl From<crate::outbound_git::ConflictCause> for RebaseBlock`, NOT inside `classify_rebase_block`** — this row asserted the latter until 2026-09-23, measured wrong against coord `origin/main` `037fc1a8f`. The NARROWNESS conclusion survives the correction, because `classify_rebase_block`'s `"conflict"` arm is what calls `RebaseBlock::from(outbound_git::classify_conflict_cause(...))`, so the construction is still reachable only from there; the remaining non-test hits are a MATCH arm in `RebaseBlockDisposition::of` and a doc-comment reference, and the rest are unit tests), so keying on it is strictly NARROWER than "terminal `conflict`", never broader. ⚠️ **`rebase_block: "empty_candidate"` is NOT admitted** — a sibling under that same terminal status whose own detail reads *"Do NOT close this PR on coord's say-so; a human must decide"* (the two `EMPTY_CANDIDATE_MARKER` detail strings in `crates/coord/src/merge_scheduler.rs` — `git grep -n 'EMPTY_CANDIDATE_MARKER' origin/main -- crates/coord/src/merge_scheduler.rs`; measured on `qontinui-coord#1664`, 2026-09-02). ⚠️ **TWO other fields on the same card are not this signal and will mislead you:** `blockers` (reads `["behind main"]` or `[]` across this population) and `merge_state_status` (GitHub's merge test — `BEHIND` or `CLEAN`); both were checked against the four live candidates on 2026-09-02 and neither carries the verdict. ⚠️ **That warning used to name a THIRD field — `block_reason_code` "(reads `none`)" — and the generalisation was FALSIFIED on 2026-09-06. The citation stays; the claim does not.** It did read `none` on those four candidates. On `qontinui-dev-notes#428` (`coord_pr_status`, `last_verified_at 2026-09-06T13:46:01.776242Z`, `confidence: fresh`) it read **`already-landed-at-head`**, so the field is a SECOND EXPRESSION of the already-landed fact rather than noise — and **which expression it is names the ARM**. `BlockReason::code` (`git grep -n 'fn code' origin/main -- crates/coord/src/pr_merge/predicate.rs`) carries three already-landed codes, one per guard of `delivers_no_change_block`, and they map one-to-one onto the three arms here: `empty-diff-already-landed` (Guard 1, `changedFiles` observed zero) → **arm 1**; `already-landed-by-content` (Guard 3, the parked-`conflict` proof, `changedFiles` nonzero) → **arm 2**, and it is member (ii) of the detector above; `already-landed-at-head` (Guard 2, `PrSnapshot::landed_at_current_head`) → **arm 3**. ⚠️ **Do NOT put `already-landed-at-head` in arm 2's allowlist.** Guard 2 is `gates::pr_has_merged_proposal_at_current_head` — a `merged` `coord.merge_proposals` row at the PR's EXACT current head — which is the SAME fact `land_stamp == "current_head"` reports through `merged_proposal_probe`'s `at_current_head`. Admitting it here pulls arm 3's whole population into arm 2 (arm 2's detector runs first) and onto a proof that requires B's VERDICT, which coord#1920 was measured to fail (`NOT-EQUIVALENT paths=5 identical=3`); and **C25** is the fixture that catches it — the live card shape below, carrying `land_stamp: current_head` AND `block_reason_code: already-landed-at-head`, which must route to arm 3; C26 additionally refuses the substring/prefix spelling of member (ii) that would admit this code by accident. The measured card: `pr_state: open`, `changedFiles: 3`, `commits: 1`, `merge_state_status: CLEAN`, `merged_at: 2026-09-06T13:45:57.832211Z`, `merge_commit: e31e7ed9994d64cc6cd5781b4c734c4cec539fe7`, `land_stamp: current_head`, `block_reason_code: already-landed-at-head`, `rebase_block: none`, `rebase_landable: true`, `blockers: []` — **arm 3's shape, on which arm 3's detector DOES fire; it is recorded as arm 3's third measured member below.** Arm 2's six conjuncts happened to pass on it as well (N `NOOP-PROVEN tree=c4d29d49f127e3301066402b1081f25d6e026478`, V `commits = 1`, P6 `IN-TREE 1`, B `EQUIVALENT paths=3 identical=3 base_existing=3`, A' `3 == 3`), **so this detector's miss cost a ROUTED close and never a wrong one — the failure direction was SILENCE**, which is the direction this row exists to police and the reason arm reach is ledgered rather than assumed. As before, **the structured enum only, never the `rebase_block_detail` TEXT** — and here the text is not merely unreliable but actively booby-trapped, since the `archived()` clause interpolates the raw proposal status into it (`git grep -n 'let archived = ' origin/main -- crates/coord/src/mcp/tools.rs`, whose two format strings both embed the status verbatim), so a substring search for `conflict` on the card hits exactly the two present-tense classes that must be refused. Fixtures C11 (admits), C15 / C16 (refuse the other eight) and C17 pin all of this; plan `2026-09-02-steward-arm2-detector-keyed-on-a-status-the-population-does-not-carry`. ⚠️ Arm 2's detector is a **COST FILTER and authorises nothing** — what authorises its close is P ∧ N ∧ V ∧ P6 ∧ B ∧ A' below. In particular, do **not** key it on coord's `[already-landed]` verdict TEXT: this same file records **"Believe coord's error TEXT last"** and two soaks in which the stored message named the wrong subsystem and sent the diagnosis hours astray. The text orders triage; the structured status is the filter. **ARM 3 (coord-landed, GitHub still OPEN — the population BETWEEN arms 1 and 2):** non-draft, **open**, `changedFiles > 0`, and `coord_pr_status` serves **`land_stamp == "current_head"`**. Evaluated **AFTER** arm 2's detector, so arm 2 keeps its population untouched; a PR matching both routes to **arm 2**, whose proof is the stronger one (fixture C22 pins that ordering, and an arm-3 branch inserted ABOVE arm 2's is exactly what it fails on). ⚠️ **Ordering is not EXCLUSIVITY — an arm-2 proof that refuses on conjunct B with `rc=1` FALLS THROUGH to arm 3's full proof when this detector also admits the card.** Arm 2 is still evaluated FIRST and still closes on its own stronger proof wherever that proof holds, so **C22 passes unchanged** — it pins the ORDERING, not exclusivity, and that compatibility is the whole reason the fix is spelled as a fall-through rather than as a precedence flip. **Fixture C53** is the control that keeps it one: an overlap card whose B reads `EQUIVALENT` *and* whose arm-3 conjuncts would all hold must still close as **arm 2**, so a fall-through armed without requiring B's refusal reddens it (and C22 with it). **Why it exists:** the two halves of this row point in opposite directions. B **decays toward REFUSAL** as `main` evolves any path the PR touched, and goes on refusing for the rest of the PR's life; arm 3's M was built precisely NOT to decay (plans `2026-09-13-steward-arm3-noop-proof-decays-when-main-evolves-landed-paths` and `2026-09-19-steward-arm3-m-false-refuses-hot-file-lands`). So ordering alone hands the overlap population to the ONE arm whose proof cannot survive `main` moving, while the arm built to survive it is never reached — and the failure direction is SILENCE, since a declined arm-2 close emits a `declined:` line that reads identically whether the PR is genuinely unprovable or merely routed to the wrong arm. Measured on `qontinui/qontinui-coord#2374` (2026-09-23; open since 2026-09-22T17:27:08Z, `block_reason_repeat_count: 5`): the card served `rebase_block: already_landed` AND `land_stamp: current_head`, B against `origin/main` read `NOT-EQUIVALENT paths=1 ghfiles=1 identical=0 first-differing=crates/coord/src/mcp/tools.rs` **rc=1**, and every arm-3 conjunct held — N, P6, L, L', the first-parent check, A'' and Mn at the stamp (`NOOP-PROVEN`). ⚠️ **SCOPE: `rc=1` ONLY — a PROVEN refusal, never a `2` UNKNOWN.** A B that reads UNKNOWN is a Tier-2 route today and stays one; an UNKNOWN must never collapse into a close on a weaker proof, which is the contract every three-exit-code probe on this path is under. **Fixture C54** is that pin. The fall-through is strictly **ADDITIVE** — it can only convert a `declined:` into a close, and every such close still requires arm 3's COMPLETE conjunct set (L, L', the first-parent check, A'' and A''') — and it **widens no detector**: neither arm's allowlist gains a member, which is the failure mode this row's own history keeps recording. **A fall-through close is an ARM 3 close and is counted as one in the `closed=` ledger line**; it invents no fourth label. **Fixture C52** is the #2374 shape and FAILS against the pre-2026-09-23 row, which is its anti-vacuity property; plan `2026-09-23-steward-arm2-precedence-starves-a-provable-arm3-close`. The overlap population's SIZE is UNKNOWN and must not be reported as small: the fleet route `GET /pr-merge/prs` carries no `rebase_block`, so arm 2's detector cannot be evaluated fleet-wide from it, and only the per-PR `coord_pr_status` card carries the field. ⚠️ **The test is an EQUALITY on `current_head` — an ALLOWLIST over the five-member `LandStampScope` enum** (`git grep -n 'pub enum LandStampScope' origin/main -- crates/coord/src/successor_land.rs`; it carries `serde(rename_all = "snake_case")`, so the served values are `none`, `terminal`, `terminal_uncorroborated`, `current_head`, `superseded_head`) — and **never a denylist** such as `land_stamp != "superseded_head"`, for the same reason arm 2's `already_landed` equality must stay one. ⚠️ **That member count is COMMENTARY, not the contract — resolve it with the `git grep` above rather than trusting this line.** The `rebase_block` count one arm over read EIGHT for weeks after coord had already added a ninth, and the stale citation hid it; the EQUALITY is what survives a member this row has never heard of, which is exactly what happened there. ⚠️ **A card from an older coord that carries NO `land_stamp` field at all is UNKNOWN → Tier 2, never arm 3.** An absent field satisfies every denylist, so the denylist formulation would open this arm to the entire population it exists to exclude. Fixture C20 pins the absent field and C21 sweeps the four non-`current_head` values; together they are the direct analogue of C15/C16 for arm 2. ⚠️ **This is NOT the banned `merged_at`/`merge_commit` ancestry key, and that difference is the whole runner#978 lesson.** `LandStampScope::CurrentHead` has exactly ONE construction site outside `#[cfg(test)]` (`git grep -n 'LandStampScope::CurrentHead' origin/main -- crates/coord/src/mcp/tools.rs`), reachable only when the `at_current_head` field of `MergedProposalProbe` (`git grep -n 'struct MergedProposalProbe' origin/main -- crates/coord/src/successor_land.rs`) is `Some(true)`. `merged_proposal_probe` (`git grep -n 'fn merged_proposal_probe' origin/main -- crates/coord/src/successor_land.rs`) computes that from an `EXISTS` over `coord.merge_proposals` joined to `coord.merge_proposal_repos` with `p.status = 'merged'`, keyed on **its two bound parameters — the repo, and the PR's EXACT CURRENT head sha** (paraphrased on purpose: that query's own bind markers are dollar-digit sequences, which this file may not contain — see the gate under "The no-op probe"). Its own doc comment says the key is *"deliberately NOT on branch or pr_number"*, because a sha identifies a revision uniquely and the key *"survives the branch rename and the pr_number rebind that are exactly the events which break the other two keys."* So `current_head` asserts **coord's scheduler marked a proposal MERGED carrying THIS head** — a head-exact corroboration, not a reachability claim about some recorded commit. Those are different facts. ⚠️ **The fail-closed polarity is coord's, not the steward's.** If the probe cannot be evaluated (a PG error) `at_current_head` is `None`, which falls through to `SupersededHead`; `land_stamp_scope`'s doc block (`git grep -n 'fn land_stamp_scope' origin/main -- crates/coord/src/mcp/tools.rs`) states the intent directly — *"`None` fails to `SupersededHead`, not `CurrentHead`. The harmful direction on THIS surface is a false 'landed' on the PR the caller asked about, so we fail toward suppression."* — pinned by coord's own regression test `land_stamp_scope_corroboration_error_fails_to_superseded`. Arm 3 therefore inherits fail-closed behaviour without implementing it, and the ONE case it must still handle itself is the absent field above. ⚠️ **On this signal the steward was BEHIND COORD'S OWN TWIN, which is why arm 3 needs no coord change and is cheap.** `pr_status_blockers` (`git grep -n 'fn pr_status_blockers' origin/main -- crates/coord/src/mcp/tools.rs`) returns an EMPTY blocker list the moment `land_stamp` is `CurrentHead`, its comment calling that *"the legitimate phantom-open ff-land window … Treat it exactly like the terminal merged/closed arm above."* Coord already classifies this population as landed **by name** and already acts on that classification, while Tier 1 went on triaging with `changedFiles` and `rebase_block` — two fields that answer the question only by inference — and a purpose-built, head-exact field sat unread on the same card. The defect was never a missing signal; it was an **unconsumed** one. ⚠️ `mergeStateStatus` is a **dispatch hint here, not a gate** — an empty-diff PR reading `BLOCKED` or `UNKNOWN` still enters this row, because what authorises the close is the proof below, not GitHub's merge test. (Whether `CLEAN` genuinely holds on the ff-land shape is **UNSETTLED**: web#1033 read `BLOCKED`, but that read was taken *after* the close and a closed PR's `mergeStateStatus` is not a witness of its open-state value; a fleet sweep on 2026-08-24 found **no** open `changedFiles=0` PR in any of the seven repos, so there was nothing live to settle it against. Treat it as UNKNOWN, which is why it is not a gate.) **ARM 4 (successor-carried land, NO land record of its own):** non-draft, **open**, `changedFiles > 0`, and `coord_pr_status` serves **ALL FIVE** of a five-member ALLOWLIST: **`land_stamp == "none"`**; **`superseded_by_status == "recorded"`**; and at least ONE `superseded_by[]` entry carrying **`successor_read == "ok"`**, **`successor_land_stamp == "terminal"`**, **`successor_repo` PRESENT and EQUAL to the repo being polled** — the presence half is not decoration: a bare `=` is satisfied by empty == empty, so a null `successor_repo` compared against a polled-repo string that failed to resolve would ADMIT (found by the independent review, which built that case and watched it route to arm 4; fixture C42's `bothempty` member) — and a `successor_land.merge_commit` that is non-null and 40 lowercase hex characters (C42's `mc` member; nothing pinned this until the same review deleted the conjunct and watched the suite stay green) — call that commit **`SUCC`**. Evaluated **LAST**, after arms 1, 2 and 3, so every earlier arm keeps its population untouched. ⚠️ **This is the population the arm-3 cell already named and put out of scope** — *"Content that reached `main` through a DIFFERENT PR is not M's to judge at all: L refuses it at the detector"* — and which no other arm picked up either, so nothing owned it and it could not even be declined-with-reason. Measured on `qontinui/qontinui-runner#1511` (2026-09-19): `land_stamp: "none"`, `rebase_block: "none"`, `block_reason_code: "ci-not-green"`, `merged_at`/`merge_commit`/`landed_by_patch_id` all null, `changedFiles: 2`, `superseded_by[0]` = `qontinui/qontinui-runner#1564` with `successor_read: "ok"`, `successor_land_stamp: "terminal"`, `successor_land.merge_commit: b4cb0df1815fc2058c0985725a9b9199d6c90d4a`; and `block_reason_repeat_count: 856` re-proposals since `block_reason_first_seen_at: 2026-09-13T12:47:03Z`. Its two commits are on `main` as rebased twins (`dfd0b18df`, `8222d80d3`, patch-id-identical) carried there by the SUCCESSOR's ff-land — `b4cb0df18`'s FIRST PARENT is the second twin — so coord never marked a proposal merged at #1511's own head and arm 3's `current_head` equality has nothing to match. `P ∧ N ∧ V ∧ P6` all held throughout. ⚠️ **`land_stamp == "none"` is an EQUALITY, and on this arm that is unusually load-bearing: `none` is ALSO what every absent-field default reaches for.** `null`, a missing key, `jq '.land_stamp // "none"'`, `${LAND:-none}` and `[ -z "$LAND" ]` all collapse an ABSENT field onto arm 4's admitting value — so establish the field's PRESENCE before comparing it, and never spell this `land_stamp != "current_head"`. Arm 3's C20 only had to survive a reader inventing a literal `current_head`; arm 4 must survive the laziest default in the language. Fixtures **C43** (set-but-empty) and **C43u** (genuinely unset) pin BOTH spellings, and the suite's own sentinel had to move from `none` to `<absent>` to make C43u expressible at all — before that the suite went green over exactly this hole. ⚠️ **`successor_repo` must EQUAL the polled repo.** `parse_supersedes_refs` (`git grep -n 'fn parse_supersedes_refs' origin/main -- crates/coord/src/pr_merge/declared_supersession.rs`) parses `owner/repo#N` as well as `#N`, so cross-repo declarations are a live shape; coord's own closer refuses them by name — *"Cross-repo declarations are never candidates, because a tree comparison across two repositories proves nothing"*. Without the equality this arm would point its proof at a sha from another repository and rely on `git cat-file -e` failing in the polled clone, which is accidental safety. ⚠️ **`superseded_by[].content` — coord's OWN content verdict — is NOT a member and must not become one.** It read **`null`** on the one measured member of this population, which its own doc defines as *"never probed, the head or land moved since, or the probe abstained"* — UNKNOWN, not "not equivalent" — so an arm keyed on it would be born inert, the same way arm 2's member (ii) and arm 3's `block_reason_code` each nearly were. When it IS present with `tree: noop` it is legitimate corroboration for the close COMMENT; it is never a conjunct. ⚠️ **Member counts here are COMMENTARY — resolve the three enums, never trust this line:** `git grep -n 'pub enum LandStampScope' origin/main -- crates/coord/src/successor_land.rs`, `git grep -n 'pub enum SuccessorRead' origin/main -- crates/coord/src/successor_land.rs`, `git grep -n 'pub enum SupersededByStatus' origin/main -- crates/coord/src/mcp/tools.rs`. The `rebase_block` count one arm over read EIGHT for weeks after coord had added a ninth. ⚠️ **Like every other arm's, this detector is a COST FILTER and authorises NOTHING.** It says which commit to point the proof at, cheaply, without searching `main`'s history — it is emphatically not evidence the content landed, and coord says so itself on `SuccessorLand`: *"It is the successor's land, never a proof that THIS PR's code is on the base: a successor may carry corrections."* `declared_in: "title"` means a human wrote `supersedes #N` in a PR title and coord recorded it faithfully, verifying nothing; coord measured 2026-09-13 that **5 of 6** declared predecessors were NOT content-equivalent at their successor's land. ⚠️ **When more than one entry qualifies, try each in turn**: close on the first whose proof holds, and decline only when every qualifying entry refuses. Each entry is independently proof-carrying, so a refusal on one is not a refusal overall — and say in the ledger which entry proved it, or the `closed=` line is unreadable. | **Prove the merge is a NO-OP against the PR's REAL base, then close. Close iff P ∧ N ∧ V ∧ A; any read that errors is UNKNOWN → do not close, route to Tier 2.** ⚠️ **Ancestry is NOT a gate here — it is structurally unsatisfiable on the fleet's most common land shape.** A coord fast-forward land leaves the branch a *descendant* of `main`: it rebases the PR's commits onto a candidate, pushes the candidate tip straight to `main`, and the branch afterwards merges `main` back into itself. So `git merge-base --is-ancestor <head> origin/main` points the wrong way *by construction* and `git log origin/main..<head>` is non-empty *by construction* — not conservative, **unreachable**; no amount of waiting, re-fetching or re-polling will ever make them pass. Measured on `qontinui-web#1033` (2026-08-24): ancestry `exit=1`, `origin/main..head` = 6 commits, `changedFiles=0`, and the merge nonetheless a **proven no-op**. The claim this row used to carry — *"both shapes reach this row, and the three guards below hold for either"* — was **false**, and left open, the row *caused* the very re-cut-forever outcome it exists to prevent. Ancestry now counts as *evidence when it passes*, never as a gate. **P — preconditions, all mandatory.** (P1) Resolve the PR's **real** base — `gh pr view <n> --json baseRefName,headRefOid,isDraft,changedFiles,commits` — and compare against `origin/<baseRefName>`. ⚠️ **The hazard is the LOCAL CLONE, not the PR.** `baseRefName` is a bare branch name that the steward resolves as `origin/<name>` in whatever checkout it happens to be standing in, so a non-default base (or a stale sibling clone) proves a no-op against a same-named branch in the *wrong repo* — that is the D2 failure one level up. Check it where it lives: **`git remote get-url origin` must name the repo you are polling**, exit-status-checked, before any comparison. ⚠️ Do **not** reach for a `baseRepository` JSON field: **it does not exist** — measured 2026-08-25, asking gh for a `baseRepository` field returns `Unknown JSON field: "baseRepository"` and gh exits 1, which would abort this row's FIRST precondition on every PR and silently reproduce the very never-fires defect this rule replaced. The base-side field is `baseRefName` alone; `headRepository` / `headRepositoryOwner` / `isCrossRepository` are the repo-identity fields that do exist, and a PR's base repository is by construction the repo you passed to `-R`, so no PR field could ever disagree. `scripts/steward-empty-diff-fixtures-test.sh`'s **gate2** pins every `--json` field this row names against the real roster. (P2) `git fetch origin <baseRefName>`, **exit status checked** — a stale base is a measured false positive (fixture C5 reads `NOOP-PROVEN` against a stale base for a branch carrying a live deletion). (P3) `git fetch origin refs/pull/<n>/head:refs/tmp/pr<n>`, then `git cat-file -e` on **both** `origin/<baseRefName>^{commit}` and `<head>^{commit}`, both exit-status-checked — a hard prerequisite of N, not a courtesy: `git merge-tree` cannot distinguish an unreadable object from a conflict. (P4) Non-draft. (P6) **The payload must be IN THE TREE** — run the payload guard (inlined under **"The no-op probe"** below, as `scripts/merge-payload-guard.sh` in ccfg; use the **inlined** copy, since a relative path does not resolve from the polled repo's clone) and require exit `0`. N compares *trees*, so a PR whose payload is **not** a tree change is invisible to it and reads no-op while carrying work someone still intends to use. Two such shapes were **measured 2026-08-25, and P ∧ N ∧ V ∧ A all hold on both**: a **history-reconcile / back-merge** PR ("merge `release` into `main`" after the same content landed on `main` under a different sha) reads `commits=2, changedFiles=0, NOOP-PROVEN` — its payload is the **merge edge**, and closing it loses that edge, after which the next merge add/add-conflicts on byte-identical content (`rc=1 UU`), which is the wedge the Green-but-dirty row above documents; and a **marker PR** whose only commit is `git commit --allow-empty` (release marker, CI re-trigger) reads `commits=1, changedFiles=0, NOOP-PROVEN`. The guard refuses exactly two patterns and nothing else — *(a)* the head is a **merge commit whose first parent is already on the base**, and *(b)* **every** commit in `origin/<base>..<head>` is empty — both scoped to heads that are not already ancestors of the base. Measured non-refusals: web#1033 itself (`IN-TREE 6 tree-changing commits`), a single-commit rebase-landed PR (fixture C11), and the disclosed self-revert. A P6 refusal is **abort + escalate to Tier 2**, never a close. (P5) `git --version` ≥ 2.38, **checked in code, not asserted in prose** — an older git is **UNKNOWN → Tier 2**, and must never fall back to tree comparison (measured: that fallback decays within ~1h as `main` advances, and closes live work on any PR whose base is not the default branch). **N — the no-op proof.** `git merge-tree --write-tree origin/<baseRefName> <head>` exits **0** **and** its output equals `git rev-parse 'origin/<baseRefName>^{tree}'`. Both oids validated `^[0-9a-f]{40}` and non-empty **before** the equality test, which is never the first thing evaluated. Three exit codes, deliberately: `0` proven no-op, `1` proven **not** a no-op (conflict included), `2` **UNKNOWN** — and **`2` must never collapse into `1`**; only `0` may reach the close path. Runnable snippet + the exit-code and empty-string traps: **"The no-op probe"** immediately below this table. **V — anti-vacuity.** `gh pr view <n> --json commits` must return **≥ 1**. *Any* head that is an **ancestor** of the base makes N read no-op trivially — merging an ancestor into its descendant changes nothing — and that class holds both the legitimate sha-preserving ff-land **and** every unhydrated, emptied or stale-parked branch. **N cannot tell them apart.** V is what separates them *in the degenerate case*, and its discriminator is the commit count (#1033: `commits=6` with `changedFiles=0`; fixture C3's branch parked exactly at the base tip: `0`). ⚠️ **V is a test for "at least one commit object exists", NOT a hydration test — measured 2026-08-25, and the row used to overstate it.** An unhydrated branch carrying a single `--no-ff` merge-forward and no work at all reads `commits=1, changedFiles=0, NOOP-PROVEN` and sails straight through V (fixture C10); so does an empty-commit marker (C9). **P6 is what stops those**, not V. Keep V — it is still the only thing standing between this reflex and the `commits == 0` population, and it is exactly the conjunct a "simplifying" reviewer will delete — but do not credit it with more than the degenerate shape. **A — cross-source agreement.** GitHub's `changedFiles` must independently read `0`. It is redundant with N when everything is consistent, and that is the point: a different engine on different data. ⚠️ **Scope A's abort to the `changedFiles==0` population only.** GitHub *freezes* a landed PR's diff against its recorded base sha — measured 2026-08-24 on six merged web PRs (#1054/#1051/#1049/#1045/#1041/#1030): every one still reports `changedFiles` of 1..12 and `commits` of 1..3 **while its head IS an ancestor of `origin/main`**, i.e. while N reads no-op. N and `changedFiles` therefore disagree routinely and benignly, with nothing stale; an unscoped "any disagreement escalates" rule would fire on every landed PR on the fleet. **Within** the `changedFiles==0` population, a disagreement is an abort + escalate to Tier 2, never a close. **The close.** Re-read `headRefOid` immediately before `gh pr close` and abort if it moved since P1 — `gh pr close` has no compare-and-swap, and a force-push between the proof and the close is the one way this rule can close work that was live *at the moment of the close*. **NEVER `--delete-branch`**: that is a conjunct of the rule, not manners — it is what keeps a wrong close recoverable (every commit preserved, the PR reopenable) and it is what the asymmetry argument rests on. **The comment must state what was PROVEN, not what was assumed.** Cite base ref + base sha + base tree; head sha + the merged-tree oid from `merge-tree`; `changedFiles`; the commit count; and whatever land evidence was found — or, explicitly, *"none found — closed as a proven no-op, not as a land"*. The rule proves *"merging this PR into `<base>` changes nothing"*; it does **not** by itself prove *"this PR's work landed"*, and the honest generalisation is that **any PR whose payload is not in the tree** reads as a no-op. P6 refuses the two measured members of that class that carry real work when the graph permits (the trunk-side back-merge and the empty-commit marker). **Two survivors remain, and both are deliberately accepted** — do not restate this as "exactly one case", which is the mistake the pre-P6 rule made and which was measured false twice: *(i)* the **self-revert** (`add N.txt` then `git rm N.txt` reads no-op with `commits=2`) genuinely never landed and closes; *(ii)* the **release-side history-reconcile**, which is the same reconcile as the refused one with the merge's parents swapped and is therefore **graph-identical to web#1033 itself**, so no predicate can refuse it without refusing the case this row exists for (fixture C13 pins it). In both, the branch survives, `--delete-branch` is forbidden, and the honest-comment requirement is what tells the author why their PR closed — which is exactly why that requirement is load-bearing and not cosmetic. Never claim "already landed" blindly. **Land evidence is CITED, not REQUIRED — IN ARM 1** (⚠️ **arm 2 promotes one item of this list, per-path tree-entry comparison, to a REQUIRED conjunct — see B below.** The scoping is the reconciliation: in arm 1 conjunct A already supplies the cross-source engine, so the blob check is corroboration; in arm 2 A is unsatisfiable, so the same signal is made to carry weight it does not carry here. Two rules, one scope each — not a contradiction) — under N ∧ V there is provably no content to lose, so it is not what authorises the close: collect `merge-base --is-ancestor` if it passes, per-commit `git patch-id --stable` twins reachable from the base, per-path tree-entry comparison, `coord_explain_pr_close` / `close_cause` **keyed on the current head sha**. ⚠️ **Do NOT use `git cherry` here — measured false negative on this exact shape.** All four #1033 commits read `+` ("not upstream") while their `git patch-id --stable` values provably matched commits already on `main`. Cause: `git cherry <upstream> <head>` marks a commit `-` only if an equivalent patch turns up in **`<head>..<upstream>`** — commits on the upstream that are **not** reachable from the head. A merge-forward pulls `main` *into* the branch, so the landed twins become **ancestors of the head** and are excluded from that comparison set *by construction*; they can never be found there. ⚠️ Do not shorten this to "the set is empty": measured twice on #1033 — on 2026-08-24 with `main` still at the merge-base the set was indeed empty (all four `+` vacuously), and re-measured 2026-08-25 with `main` **6 commits ahead** the set was **non-empty** and all four *still* read `+`. Emptiness was an accident of timing; **twin-exclusion** is the durable mechanism, and it does not decay. This is **independent of** the squash-land limitation noted in the Green-but-dirty row above, and it fails in the dangerous direction: it *under-reports* landedness. Left open, coord re-cuts a candidate identical to main and re-runs full CI forever (observed on web#833/#836, 2026-07-23). Idempotent; not rate-limited. **Ledger it:** report `empty-diff candidates seen / closed / declined-with-reason` each cycle — this defect stayed invisible for weeks precisely because a Tier-1 row that *refuses* logs nothing and produces no wedge signal of its own. ⚠️ **NEVER close on the recorded `merge_commit`'s ancestry, and never on `changedFiles=0` alone.** `merged_at`/`merge_commit` come from `coord.repo_branches` keyed on `(repo, pr_number)` — a stamp about *a head that landed*, not about the head you are looking at — and coord's only invalidation fires on `pr_number IS DISTINCT FROM`, which by construction CANNOT fire when the same PR's head moves after a **partial ff-land**. Measured 2026-08-06 on runner#978: `merged_at=2026-08-05T18:49Z` + `merge_commit=be0d07fb` served against `pr_state=open`, where `be0d07fb` is genuinely an ancestor of main **and of the PR's own current head** — so an ancestry-keyed close PASSED while 2 commits (+277/-5) sat unlanded. That case is why the close is keyed on **N** (which reads NOT-NOOP there: merging those 2 commits changes the base) instead of on ancestry, and why **A** keeps `changedFiles=0` as an independent second engine. An unhydrated PR also reads `changedFiles=0` — that one is **V**'s job. (coord's own destructive sweep `phantom_open_candidates` already joins `rb.head_sha = mpr.head_sha` and is NOT affected — this reflex was the sole exposed consumer.) **ARM 2 — the rebase-landed frozen-diff close. Close iff P ∧ N ∧ V ∧ P6 ∧ B ∧ A'; any read that errors is UNKNOWN → do not close, route to Tier 2.** ⚠️ **Conjunct A is STRUCTURALLY UNSATISFIABLE here** — the same class of unreachability ancestry has on the ff-land shape, one conjunct over. GitHub freezes a PR's diff against its *recorded base sha*, so a PR whose content landed under a rewritten sha keeps reporting its pre-land file count **indefinitely**. This row already knew GitHub freezes landed diffs (the six merged web PRs cited under A) and used the fact only to scope A's abort — it never asked what happens when a frozen-diff PR is still **OPEN**, and the answer is that the reflex cannot see it at all. Measured on `qontinui-schemas#144` (2026-08-29; re-measured 2026-09-01 after `main` advanced six commits, verdicts unchanged): open 106h, coord `conflict` ~22h re-proposing `[already-landed] nothing to land`, `changedFiles=13 commits=1 mergeState=CLEAN`, ancestry `rc=1`, N `NOOP-PROVEN`, P6 `IN-TREE 1`, and all 13 of the PR's own files byte-identical to `origin/main`. Every signal except the frozen `changedFiles` said landed; that one dissenting signal is exactly what A is keyed on. Closed by hand on this evidence, branch deliberately preserved. **P, N, V, P6 — unchanged, but NOT boilerplate.** ⚠️ **P1's `git remote get-url origin` identity check and P2/P3's exit-status-checked fetches are LOAD-BEARING in arm 2**, because B compares against `origin/<baseRefName>` in whatever clone you are standing in and A is not there to catch a wrong or stale one. A stale base is already a measured false positive for N (fixture C5); it is the same false positive for B. **B — per-path tree-entry equivalence (mode, type and blob id), arm 2 only.** Run the inlined `merge-blob-equivalence` snippet (under "The no-op probe") and require exit `0`. It takes the path set from `git merge-base origin/<baseRefName> <head>` — **not** a two-dot diff from `origin/<baseRefName>`, which would also list every path the *base* changed and so refuse every PR on a moving base — and requires every path's tree ENTRY at the head (`git --literal-pathspecs ls-tree <head> -- <path>`: mode, type and blob id) to equal its entry at `origin/<baseRefName>`, the set to be **non-empty** (an empty set is arm 1's `changedFiles==0` population and the snippet routes it there as `NOT-ARM2`), and **at least one compared blob to exist in the base**, so an all-absent read from a wrong clone can never pass as "identical everywhere". ⚠️ **B is not a second SOURCE** — it is git again over the object store N already read — and it **decays toward REFUSAL** as `main` evolves the PR's paths, so a `NOT-EQUIVALENT` is *"arm 2 cannot prove this one"* and never *"the work did not land"*. Both points are argued in full beside the snippet. ⚠️ **And a `NOT-EQUIVALENT` with `rc=1` is no longer the end of the road.** When the same card ALSO satisfies arm 3's detector (`land_stamp == "current_head"`), do not emit `declined:` here — **fall through to arm 3's full proof** and close only if arm 3's own conjuncts all hold, counting the close as **arm 3**. Scoped to `rc=1`; a B reading `2` UNKNOWN stays a Tier-2 route. The clause, its measured shape and its three fixtures (C52/C53/C54) are stated once, in arm 3's detector above. **A' — cross-source cardinality agreement, arm 2's replacement for A.** GitHub's `changedFiles` must be non-zero (the detector already requires it) **and equal the `ghfiles=` count B printed**. ⚠️ **`ghfiles=`, NOT `paths=` — the two are different numbers and reading the wrong one is the defect this conjunct shipped with.** B's path set is derived `--no-renames`, which splits a rename into `D old` + `A new`, while GitHub's `changedFiles` counts a rename as ONE; so `paths=` exceeds `changedFiles` by exactly the rename count and **every renaming PR failed A' and was escalated to Tier 2 instead of closed** (measured 2026-09-20 on ccfg#1035, `changedFiles=7` against `paths=8`, and web#1424, `changedFiles=54` against `paths=66`, both with every other conjunct proven; plan `2026-09-20-steward-cardinality-conjunct-miscounts-renames`). `ghfiles=` is B's second, rename-aware count over the same merge-base, and is the comparable of `changedFiles`. The `--no-renames` set STAYS: it is a conservative superset that also checks the deletion half of a rename landed, and a `-M`-derived comparison set was measured to prove `EQUIVALENT` on a partial land the base had never taken. On a PR containing no rename the two counts coincide, which is why every previously measured instance passed. Three fixtures hold it: **C49** (a fully landed renaming PR, every other conjunct proven, which closes only because A'' reads `ghfiles=`), **C50** (the ANTI-VACUITY control — a `changedFiles` neither count can equal, which must still REFUSE, so "point it at an always-agreeing number" is not a fix) and **C51** (a PARTIAL land where the cardinality AGREES and B's `--no-renames` comparison set is what refuses, which is the pin on not switching that set to `-M`). A mismatch is abort + escalate to Tier 2, never a close — exactly how A's abort is scoped inside the `changedFiles==0` population. Measured on schemas#144: `changedFiles=13`, B `paths=13`. Re-measured 2026-09-01 after six `main` commits: still `13 == 13`. (Those historic readings are left as MEASURED: the run predates `ghfiles=`, and restating them in the new field would be an inference, not a measurement.) Weaker than A (a count, not a count against zero) and deliberately kept anyway: it is the only conjunct in arm 2 computed by a different engine on different data, and it is what makes the P1/P2/P3 promotion above a belt rather than the only strap. **The close is identical to arm 1's** — re-read `headRefOid` immediately before `gh pr close` and abort if it moved; **NEVER `--delete-branch`**; and the comment states what was PROVEN: base ref + base sha, head sha, N's merged-tree oid, B's full `paths=`/`ghfiles=`/`identical=`/`base_existing=` line — **with `ghfiles=` named as the number A' was checked against**, since it is `paths=` that the eye reaches for and they differ on any renaming PR, GitHub's `changedFiles`, the commit count, and — because arm 2 exists precisely where ancestry and `changedFiles` both read against it — an explicit note that **arm 1 does not apply and why**. **ARM 3 — the coord-landed, GitHub-still-open close. Close iff P ∧ (N ∨ (M ∧ A''')) ∧ V ∧ P6 ∧ L ∧ L' ∧ A''; any read that errors is UNKNOWN → do not close, route to Tier 2.** ⚠️ **This is the population BETWEEN arms 1 and 2 — admitted by arm 1's DETECTOR, unprovable by arm 1's PROOF, unadmitted by arm 2's DETECTOR, so NO ROW OWNED IT.** Conjunct A is structurally unsatisfiable here for exactly the reason it is in arm 2 (GitHub freezes a landed PR's diff against its recorded base sha, so this population reports its pre-land file count indefinitely), and that is true of **every** PR arm 1's `merged_at`-while-open disjunct admits, because that disjunct selects precisely the sha-rewriting land. Arm 2 cannot reach it either: arm 2's detector selects on *"coord is STUCK"* — `rebase_block == "already_landed"`, reachable only from a terminally-failed `"conflict"` proposal — and here coord **SUCCEEDED**, recorded the land, and has no failed proposal left to classify. Measured on `qontinui-coord#1920` (2026-09-04): `pr_state="open"`, `merged_at="2026-09-04T15:09:08Z"`, `merge_commit="f8f84494…"` which IS `main`'s own tip, `land_stamp="current_head"`, `rebase_block="none"`, `rebase_landable=true`; N `NOOP-PROVEN`, P6 `IN-TREE 2`, V `commits=2`, ancestry `rc=1` (uninformative, as always on this shape). It was closed **BY HAND** because no arm could classify it, and the `EMPTYDIFF` ledger had to record the close as having happened OUTSIDE the reflex. **A second live instance, `portofino-pizzeria/mobile#4` (measured 2026-09-05T20:42Z)**, also closed by hand as arm 3 before this arm shipped: `land_stamp=current_head`, `merge_commit=0447cdbf…` (GitHub's own compare of that sha against `main` reads `identical` — it IS main's tip), `merged_at=2026-09-05T17:02:09Z`, `rebase_block=none`, `rebase_landable=true`, `block_reason_code=already-landed-at-head`, `changedFiles=26`; N `NOOP-PROVEN`, P6 `IN-TREE 9`, V `commits=9`, ancestry `rc=1`. **A third, `qontinui-dev-notes#428` (measured 2026-09-06T13:46:01.776242Z, `confidence: fresh`)**, also closed by hand: `land_stamp=current_head`, `merge_commit=e31e7ed9994d64cc6cd5781b4c734c4cec539fe7` (on `origin/main`, `committer=qontinui-coord`, 2026-09-06T13:44:31Z), `merged_at=2026-09-06T13:45:57.832211Z`, `rebase_block=none`, `rebase_landable=true`, `block_reason_code=already-landed-at-head`, `changedFiles=3`; N `NOOP-PROVEN`, P6 `IN-TREE 1`, V `commits=1`, B `EQUIVALENT paths=3 identical=3 base_existing=3`. **The two members measured WITH that field carry `block_reason_code=already-landed-at-head`** — `mobile#4` and `dev-notes#428`, both quoted above. ⚠️ **This sentence read "All three members" until 2026-09-07, and the card quoted two paragraphs up refutes it: `coord#1920` (2026-09-04) carries `land_stamp`, `rebase_block` and `rebase_landable` and NO `block_reason_code` at all**, because it predates the field being read on this population. The over-claim then propagated into the C25 fixture and its table row before review caught it — this is the SECOND `block_reason_code` generalisation this row has had to retract, after the `(reads `none`)` one three paragraphs down, and both were reached by generalising from the members that happened to carry the field. Say "the members measured with it", never "all". The code is corroboration for arm 3's `land_stamp` equality, never a substitute for it, and never a member of arm 2's allowlist. **P, N, V, P6 — unchanged, and NOT boilerplate.** ⚠️ **P1's `git remote get-url origin` identity check and P2/P3's exit-status-checked fetches are LOAD-BEARING here for the same reason they are in arm 2** — L' compares against `origin/<baseRefName>` in whatever clone the steward is standing in, and A is not present to catch a wrong or stale one. N remains the proof that merging changes nothing — but in arm 3 it is only ONE disjunct of **(N ∨ M)**, because N decays once `main` rewrites the PR's own lines (see **M** below); V remains the only guard against the `commits == 0` population; P6 remains the only guard against the back-merge and empty-commit-marker shapes. **L — head-exact land corroboration, and THE authorising conjunct.** `coord_pr_status` must serve `land_stamp == "current_head"` as the structured-enum EQUALITY argued in the detector cell, **and** a `merge_commit` that is non-null and is 40 lowercase hex characters. L is what makes this arm sound; every other conjunct only narrows it. **L' — local reachability, the SECOND ENGINE, and NEVER a substitute for L.** In the polled repo's clone, both of these must hold, each **exit-status-checked**: `git cat-file -e <merge_commit>^{commit}`, then `git merge-base --is-ancestor <merge_commit> origin/<baseRefName>`. Three exit codes as everywhere on this path — `0` proven, `1` proven-refused, `2` UNKNOWN — and **`2` must never collapse into `1`**: an unreadable object or an unresolvable base is `2`, never `1` (`git merge-base --is-ancestor` returns 128 on a bad rev, as the payload guard already documents; discriminate BY EXIT CODE, never by "non-zero"). ⚠️ **L' IS the ancestry check this row bans, and it is admissible ONLY downstream of L.** On runner#978 the recorded `be0d07fb` genuinely **was** an ancestor of `main`, so **L' would have PASSED there** while 2 commits (+277/-5) sat unlanded — an arm keyed on L' alone is precisely the rule that closed live work. What refuses runner#978 is **L**, and independently **N**. *At the detector:* its head had moved past the stamp, so the probe's `at_current_head` reads `Some(false)`, `land_stamp_scope` falls to `SupersededHead`, and `merged_at`/`merge_commit` are **nulled out of the top-level card fields** and relocated to `superseded_land` (`git grep -n 'fn apply_land_stamp_scope' origin/main -- crates/coord/src/mcp/tools.rs`); coord's own regression test for that shape is named `land_stamp_scope_open_without_matching_proposal_is_superseded`, and the ADMITTING case is pinned by its neighbour `land_stamp_scope_open_with_proposal_at_current_head_serves_stamp`. *At the proof:* even with the detector wrong, **N reads NOT-NOOP on runner#978** — merging 2 unlanded commits that change +277/-5 changes the base tree. L' is present only because every arm owes one conjunct computed by a different engine on different data — arm 1 has A, arm 2 has A' — and L' is git's object store answering a question coord's Postgres already answered. **It narrows; it never admits.** Fixture C19 pins exactly that: the runner#978 shape, where L' PASSES and the DETECTOR is the only refusal. ⚠️ **L' is PROSE here, deliberately not a fourth inlined snippet.** The three inlined probes each own a script because each has a real surface that can drift — output strings the fixture suite matches IN FULL, an oid equality with an empty-string trap, an exit code that means two different things, a path-set derivation. L' has none of that: two commands whose exit statuses are conjoined, nothing parsed, no string compared — so a fourth `BEGIN`/`END` block would owe a fourth drift check protecting nothing (this file already prices a fourth inlining and declines it under "The no-op probe"). **Priced honestly, that is a small and real weakness** — prose can drift from what the steward actually runs, and no gate catches that — accepted because the fixture suite asserts the L' **PREDICATE** on C18, C19, C23a and C23b, so a wrong predicate fails the suite even though a reworded paragraph would not. **M — per-path tree-entry equivalence AT THE LAND COMMIT: arm 3's second no-op proof, and the disjunct that does not decay.** ⚠️ **N decays in arm 3, and this row did not know it.** N asks *"would replaying this head onto TODAY's `main` change nothing?"*. That is a replay-safety question. It is a landing question only until `main` evolves the PR's own lines. `main` can do three things after the land. *(1)* It can grow the PR's paths **away from its hunks**. The merge then takes `main`'s side and N stays `NOOP-PROVEN`. This is the 11-PR `qontinui-coord` population measured 2026-09-05, where "N does not decay" held. *(2)* It can rewrite **the very lines the PR introduced**. The two sides then collide in one hunk and N reads `NOT-NOOP merge conflicts`. *(3)* It can **revert** the PR. Replaying the head would re-apply it, so N reads `NOT-NOOP merged=…`. In cases 2 and 3 arm 3 declined forever, while coord served `block_reason_code=already-landed-at-head` on every tick. Measured 2026-09-13 on two PRs. `portofino-pizzeria/mobile#11` was a single squash (`merge_commit=e58e1954…`, `block_reason_repeat_count` 19). `#13` was a three-commit rebase train (`merge_commit=23b895a0…`, the train's tip, repeat count 45). On both, N read `NOT-NOOP merge conflicts`. B against `origin/main` read `NOT-EQUIVALENT paths=7 identical=6` and `NOT-EQUIVALENT paths=2 identical=0`. The SAME B snippet run against the recorded `merge_commit` read `EQUIVALENT paths=7 identical=7 base_existing=7` and `EQUIVALENT paths=2 identical=2 base_existing=2`. Both PRs were closed by hand on that evidence. **The rule.** Run the inlined `merge-blob-equivalence` snippet (under "The no-op probe") with `MT_BASE=<merge_commit>` and `MT_HEAD=<head>`. No fourth inlined script is added. With that input, the path set comes from `git merge-base <merge_commit> <head>`, which is the head's own fork-point set. Every path's tree ENTRY at the head (mode, type and blob id, never the blob id alone) must equal its entry at `merge_commit`. The set must be non-empty, and at least one blob must exist at `merge_commit`. **And `merge_commit` must sit on the base's FIRST-PARENT chain.** `git rev-list --first-parent origin/<baseRefName>` must exit `0`, and anything else reads `2`. The stamp, resolved to its full 40-hex with `git rev-parse --verify`, must then appear in that output as a whole line; if it is absent, this check reads `1`. A stamp that does not resolve reads `2`, not `1`. So does a shallow clone: unless `git rev-parse --is-shallow-repository` prints exactly `false`, this check reads `2`, because a truncated chain can omit a stamp that really is on it. L' alone does not establish this. An ancestor can reach the base through a merge whose resolution THREW ITS CONTENT AWAY (`-s ours`, or a bad conflict resolution). Its blobs then match the head while `main`'s tree never carried them (fixture C30). Rebase, squash, fast-forward and merge-commit lands all put the recorded commit on that chain AT LAND TIME (see *(g)* below for how a later fast-forward can move it off), and both measured stamps are on it today. This half is prose, not a fourth inlined snippet: it parses nothing but one whole-line match. The fixture suite pins its predicate (`mt_fp`, C30 and the P7 shallow check) the same way it pins L'. **M's content half has TWO engines, because the blob engine alone false-refuses the fleet's commonest land.** ⚠️ Measured 2026-09-19: of 23 coord-landed, GitHub-open conflicting PRs, the blob check above (call it **Mb**) proved 8 and refused 15. A hunk-level adjudication found **all 15 refusals false**: every PR's change had landed intact, and they were closed by hand. The shape is ordinary. Between the PR's fork point and its land, `main` edited ANOTHER part of one of the PR's own hot files (coord `mcp/tools.rs`, `schema-read-surfaces.tsv`, `gates.rs`, `merge_scheduler.rs`; runner `settings.rs`, `agent_runtime.rs`; ccfg `unattended.md`, `run-guard-tests.sh`). coord rebased the head onto that `main`, so the land commit's blob carries `main`'s edit as well as the PR's, and the head, forked earlier, never saw it. "M does not decay" holds for `main`'s evolution AFTER the land; the blob comparison was never immune to `main`'s evolution BEFORE it. **Mn — the second engine.** Run the inlined `merge-noop-probe` snippet (N) with `MT_BASE=<merge_commit>` and `MT_HEAD=<head>`. It asks: *would merging this head into the land commit change nothing?* A three-way merge from the fork point applies a hunk of the head's only where the land lacks it, so `NOOP-PROVEN` says every hunk the head made is in the land. `main`'s other edits to the same file are ours-side content and prove nothing either way. On the 15 false refusals Mn read `NOOP-PROVEN` 15 of 15. That includes `qontinui-runner#1555`, where the PR moved a test block: a per-path merge under the default (myers) diff conflicts there, while merge-ort diffs with histogram (fixture C37). Mn refuses a dropped hunk, because the merge re-applies it (C36). It refuses a lost mode (C32) and a non-tip stamp (C28, C33), because the merge re-applies the missing change. A hunk `main` carried to a RENAMED path before the land also reads `NOOP-PROVEN`, because merge-ort follows renames; the content did land, so that is correct, but Mb refuses it. **Mn's guard:** Mn counts only when B at the stamp printed a line that compared something. An `UNKNOWN …` line from B compared nothing, so the guard then reads `2`, never Mn's own `0` (P10, found by the pre-PR review: a `GIT_*_PATHSPECS` variable in the environment makes every ls-tree fail). Two more B lines void Mn outright. The all-absent line `NOT-EQUIVALENT paths=<n> ghfiles=<g> but no compared blob exists in the base` is M's refusal *(c)* below; without the guard, Mn would close an all-deletions PR whose N has decayed (C38). That half keeps a false refusal on purpose rather than adding safety: an unreadable stamp already makes Mn read UNKNOWN, so B's stale-clone reason does not apply to Mn. The `NOT-ARM2 empty path set` line means the head is an ANCESTOR of the stamp, and merging an ancestor changes nothing whatever the stamp holds: a `main` that merged the head with `-s ours`, discarding it, reads Mn `NOOP-PROVEN` with first-parent `0` (P9, found by the independent vet). A''' refuses that shape too, since the line carries no `ghfiles=` and reads as `0`; the guard is what makes Mn itself refuse it. **The content half is the three-valued OR of Mb and Mn**: `0` if either proves it, otherwise `2` if either could not tell, otherwise `1`. Mb stays: B at the stamp runs anyway for A''', and dropping it could only lose a close. No new script and no new inlined copy: both engines are existing snippets pointed at the stamp, and gate0 already pins both. ⚠️ **The alternative the adjudication proposed was vetted and NOT adopted.** It would locate the PR's landed commits by `git patch-id`, then compare zero-context diff bodies with the `@@` headers stripped. A squash land has no per-commit patch-id twin, and neither does a commit whose context `main` rewrote (ccfg#844's `2a91f627` needed a content fallback). A body comparison with the line numbers stripped also cannot tell a hunk from the same lines landed at a DIFFERENT place in the file, which the three-way merge refuses. And it would have been a fourth script, so a fourth inlined copy with its own drift gate. Plan `2026-09-19-steward-arm3-m-false-refuses-hot-file-lands`. M is the three-valued AND of this first-parent check and the content half: `1` if either proves a refusal, otherwise `2` if either could not tell, otherwise `0`. **M counts only once L and L' both read `0`.** Running it earlier is harmless, but a close never rests on M without them. M's authority is borrowed from them. L says coord marked a proposal MERGED carrying THIS head. L' says the stamped commit really is on the base. M adds git's own statement that the land commit carried every change this head made. **(N ∨ M) is a three-valued OR.** It reads `0` if either disjunct reads `0`. Otherwise it reads `2` if either reads `2`. Otherwise it reads `1`. **A `2` never collapses into `1`**, and only `0` reaches the close. **A''' — M's own `ghfiles=` must also equal `changedFiles`** whenever M, not N, is the disjunct that proved the close. The count is B-at-the-stamp's `ghfiles=`, the rename-aware one — **not `paths=`**, for the reason spelled out under A' (a `--no-renames` path set counts a rename twice and GitHub counts it once). B prints both on its `NOT-EQUIVALENT` refusing lines as well as on `EQUIVALENT`, so A''' binds an Mn-proved close exactly as it binds an Mb-proved one. A B line with no `ghfiles=` (`NOT-ARM2`, `UNKNOWN`) reads as `0` and refuses. The two merge-bases coincided on both measured PRs. A mismatch means the stamp and the base see different fork points for this head, and that goes to Tier 2, never a close. The usual causes are a mis-recorded stamp, or a head that merged `main` forward through a commit the stamp does not contain (fixture C31). **What M admits that N refused, deliberately:** *(a)* overlapping later evolution, which is the target population. *(a')* A rebase land onto a `main` that had edited the PR's own paths elsewhere since the fork point (Mn only; C35), which is the commonest land shape on this fleet. *(b)* A multi-commit rebase land stamped at the train's TIP. The tip's tree holds the cumulative result of every train commit, so blob equality there on every path means every path landed. *(c)* A **later revert**. The PR DID land, and undoing it was a later decision that a new PR owns. The close comment must show N's refusal beside M's line, so the revert stays visible. **What M still refuses (fail-safe):** *(a)* A non-tip `merge_commit`. Measured on `#13`'s intermediate train commits `e9ca38b` (`identical=0`) and `63b2459` (`identical=1`), and pinned as fixture C28. That includes a train whose LATER commits change only a mode. The non-tip stamp's content matches, and only its mode differs, so a blob-only M would have admitted it (fixture C33). The tip stamp of the same train still closes (fixture C34). *(b)* A land commit missing any hunk the head made, for example a conflict resolved against the head (C36). Both engines refuse it: Mb because the blob differs, Mn because the merge re-applies the hunk. *(c)* An all-deletions payload (`base_existing=0`, arm 2's accepted cost, C17). *(d)* A true merge commit with the head as a parent. The path set is empty, and the snippet prints its `NOT-ARM2 empty path set` routing line with rc `1`, which in arm 3 is simply a refusal. *(e)* A `merge_commit` absent from the clone, which reads `2` (fixture C29). *(f)* A stamp that reached the base only through a merge that discarded its content. Such a stamp is off the first-parent chain (fixture C30). *(g)* A genuine stamp that `main`'s first-parent chain later left behind. For example, a release branch merges `main` and `main` then fast-forwards to that release branch. This is a false refusal, disclosed rather than fixed: such a PR falls back to N, and where N also refuses, (N ∨ M) reads a proven `1`, so the PR is DECLINED and stays open. It is not routed to Tier 2, so the `declined` ledger line is the only place it shows up. Content that reached `main` through a DIFFERENT PR is not M's to judge at all: L refuses it at the detector. ⚠️ **The comparison is by tree ENTRY (mode + type + blob id), and that is measured, not a style choice.** M's first cut compared `git rev-parse <rev>:<path>` blob ids, which carry no mode. The independent vet of ccfg#943 closed a PR with that cut. The head ran `chmod +x run.sh` and edited a line of `g.txt`. The land kept `run.sh` at `100644`, and `main` then rewrote the `g.txt` line. N refused with a conflict, but blob-only M read `EQUIVALENT paths=2`, and first-parent, L', P6 and A''' all passed. So arm 3 CLOSED while `main` never carried the exec bit (fixture C32). A symlink swapped for a regular file with the same bytes is the same hole. It was NOT shared with arm 2, as this row once claimed. Arm 2 also requires N, and N compares tree oids, which include the mode; the hole was new to arm 3 because arm 3 can close on M alone. For the same reason, making B mode-aware changes no arm-2 verdict. ⚠️ **Do NOT drop N in favour of M.** M refuses a land whose conflict resolution differs from the head on some path, and N can still prove subsumption there. The disjunction keeps every close that worked before (C18, C23a) and adds only the population that N's decay drops (C27). **A'' — cross-source cardinality agreement, arm 3's replacement for A.** GitHub's `changedFiles` must be non-zero (the detector already requires it) **and equal the `ghfiles=` count that B prints** — the rename-aware count, **not `paths=`**; see A' for why the two differ and what reading `paths=` here cost. ⚠️ **Arm 3 runs B for its PATH COUNT ONLY and does NOT require B's verdict.** That has to be spelled out, because every reader arriving from arm 2 will assume B must exit `0` — and requiring it would refuse the very case this arm exists for. On coord#1920 a sibling PR (#1919) had landed `crates/coord/src/mcp/tools.rs` first, so `main` had evolved that path out from under B and B read `NOT-EQUIVALENT paths=5 identical=3 first-differing=crates/coord/src/mcp/tools.rs` — the documented decay this row already defines as *"arm 2 cannot prove this one"* and never *"the work did not land"*. **The cardinality agreed anyway:** `changedFiles=5` against B's `paths=5`. The second live instance settles it from the other side: on `portofino-pizzeria/mobile#4` B read **`EQUIVALENT paths=26 identical=26 base_existing=26`** against `changedFiles=26`. **The two measured members of this population DISAGREE on B's verdict and AGREE on its count** — which is the measurement that says A'' must take the count and not the verdict, and why an arm 3 requiring `B` to exit `0` would have been silently inert on half of what this population has already shown. A mismatch is **abort + escalate to Tier 2, never a close**; a `NOT-ARM2 empty path set` from B carries no count at all, which reads as `0` against `changedFiles > 0` and fails A'' correctly (an empty `--no-renames` set implies an empty rename-aware one, so that line is left without a `ghfiles=` field rather than carrying a redundant zero). **The close is identical to arms 1 and 2** — re-read `headRefOid` immediately before `gh pr close` and abort if it moved; **NEVER `--delete-branch`**; and the comment states what was PROVEN: base ref + base sha, head sha, N's merged-tree oid, B's full `paths=`/`ghfiles=` line **with an explicit note that its verdict was NOT required and why, and that A'' was checked against `ghfiles=` rather than `paths=`**, GitHub's `changedFiles`, the commit count, the served `land_stamp` and `merge_commit`, **which disjunct proved the close** — N's merged-tree oid, or M's proving line with the `merge_commit` it ran against (Mb's `EQUIVALENT` line, or Mn's `NOOP-PROVEN` line beside B-at-the-stamp's refusing line) AND N's own refusal line, since an M-proved close is exactly the case where `main` has since rewritten or reverted the PR's lines — and an explicit note that **arms 1 and 2 do not apply and why**. **ARM 4 — the successor-carried land. Close iff P ∧ (N ∨ M4) ∧ V ∧ V' ∧ P6 ∧ S' ∧ A4; any read that errors is UNKNOWN → do not close, route to Tier 2.** ⚠️ **Arm 4 has NO `L` and cannot have one.** coord holds no land record for this head — that is the whole defect — so there is no coord-side fact about this PR to anchor on, and the authority rests ENTIRELY on two git-side conjuncts. **P, V, P6 — unchanged, and NOT boilerplate.** P1's `git remote get-url origin` identity check and P2/P3's exit-status-checked fetches are **load-bearing** here for the same reason they are in arms 2 and 3: S' and M4 compare against `origin/<baseRefName>` and `SUCC` in whatever clone the steward is standing in, and conjunct A is not present to catch a wrong or stale one. **S' — the successor stamp really is on the base. THE AUTHORISING HALF, and the second engine in L''s sense.** In the polled repo's clone, all exit-status-checked: `git cat-file -e SUCC^{commit}`; `git merge-base --is-ancestor SUCC origin/<baseRefName>`; and `SUCC`, resolved to full 40-hex by `git rev-parse --verify`, appearing as a **whole line** in `git rev-list --first-parent origin/<baseRefName>` (which must exit `0`), with `git rev-parse --is-shallow-repository` printing exactly `false` — a truncated chain can omit a stamp that really is on it, and that reads `2`, never `1`. An unreadable object is `2`. `git merge-base --is-ancestor` returns 128 on a bad rev, so discriminate **by exit code**, never by "non-zero". S' is executably IDENTICAL to arm 3's L' plus arm 3's first-parent check, pointed at a different stamp — deliberately, so it needs no new script and inherits every pin C23a/C23b/C28–C38 already carry. **M4 — arm 3's M, pointed at `SUCC`. THE OTHER AUTHORISING HALF.** The three-valued AND of the first-parent check (inside S') and a content half that is the three-valued **OR** of `Mb` (the inlined `merge-blob-equivalence` snippet with `MT_BASE=SUCC`) and `Mn` (the inlined `merge-noop-probe` snippet with `MT_BASE=SUCC`), guarded exactly as arm 3 guards it — `Mn` counts only when `Mb` printed a line that compared something. **No new script and no fourth inlined copy:** both engines are snippets this row already inlines, pointed at `SUCC`. **N** stays the first disjunct, as in arm 3, and decays the same way. **V' — the head must be genuinely AHEAD of the base, and this is a SAFETY conjunct, not a tidiness one.** `git rev-list --count origin/<baseRefName>..<head>` must be **≥ 1**; a `0` is a proven refusal (`1`) and an errored read is `2`. ⚠️ **Do NOT confuse V' with V.** V is `gh pr view --json commits` ≥ 1 — "at least one commit object exists" — and the ancestor shape SATISFIES it. Measured on a `-s ours` repo in which `main` merged the PR's own branch and DISCARDED its content: the detector admits it, S' reads `0`, the first-parent check reads `0` (unlike arm 3's C30 side commit, this merge IS on the chain), N reads `NOOP-PROVEN` because merging an ancestor changes nothing, P6 reads `0` because the payload guard reports the ancestor case IN-TREE, and `Mn` at the stamp reads `NOOP-PROVEN` — **every conjunct but V' passed on a PR whose work provably never reached `main`.** Arm 3 refuses that shape on L; arm 4 has no L, so without V' the only thing left is A4's path count happening to read zero, and `changedFiles > 0` is no help because this row's own measurement is that GitHub FREEZES a landed PR's diff at 1..12 files while its head IS an ancestor of `origin/main`. V' kills the whole ancestor family — `-s ours`, head-ancestor-of-`SUCC`, stale-parked branches — by name rather than by accident. Fixture **C47**. **A4 — cross-source cardinality agreement, arm 4's A''/A'''. Read the next sentence before deleting it.** ⚠️ **In arms 2 and 3 the cardinality check is corroboration; in arm 4 it is a SECOND safety conjunct**, because arm 4 is the one arm with no coord-side proof behind it. This row already warns that V "is exactly the conjunct a 'simplifying' reviewer will delete"; A4 now inherits that status. GitHub's `changedFiles` must be non-zero (the detector requires it) **and equal the `ghfiles=` count B printed against `origin/<baseRefName>`** — the rename-aware count, **not `paths=`** (A' has the measurement and the argument) — asserted WITHOUT requiring B to exit `0`, for arm 3's measured reason: B read `NOT-EQUIVALENT paths=2 identical=1` on #1511 while its count agreed. **And**, whenever M4 rather than N proved the close, `Mb`-at-`SUCC`'s `ghfiles=` must equal `changedFiles` too (arm 3's A'''). On #1511: `changedFiles = 2`, B `paths = 2`, `Mb`-at-`SUCC` `paths = 2` — a PR with no rename, so `ghfiles=` reads `2` on both. A `NOT-ARM2` or `UNKNOWN` line carries no `ghfiles=` and reads as `0`, correctly failing A4 against `changedFiles > 0`. A mismatch is **abort + escalate to Tier 2, never a close** — and in arm 4 that is a TERM OF THE CLOSE, not merely an assertion beside it. ⚠️ **That is where arm 4's bookkeeping differs from arm 3's, and the difference is load-bearing.** Arm 3 can leave A'' out of its close because an arm-3 cardinality mismatch is an unconditional refusal upstream, so the case never reaches the close at all. Arm 4 meets the mismatch on a shape it must still DETECT and DECLINE — the ancestor family, where B's path set is empty against a frozen non-zero `changedFiles` — so the conjunct has to be carried into the close itself or a proof-complete-looking candidate closes on a count that disagrees. Fixture **C48**. **What authorises arm 4's close, stated plainly.** Arm 1's authorising proof is A; arm 2's is B's verdict + A'; arm 3's is L + L' + A'', where L is coord proving a `merged` proposal carried THIS PR's exact head. Arm 4's is a **syllogism in git's own object store, to which coord contributes nothing**: S' says `SUCC` is on `origin/<base>`'s first-parent chain, so `main`'s own tree carried `SUCC`'s tree at that point in its history; M4 says every change this head made is present in `SUCC`; therefore every change this head made reached `main`. The **cross-source second engine** is A4 — GitHub's `changedFiles`, a different engine on different data, exactly as A' serves arm 2. Two things that argument deliberately does NOT lean on. *(i)* **coord's supersession record is not evidence of a land.** A dishonest or mistaken declaration only changes WHICH commit gets tried; M4 then fails and arm 4 declines. That is what makes it a legitimate cost filter rather than a smuggled authority, and it is why arm 4 stays sound on a detector weaker than arm 3's L. *(ii)* **S' alone is the banned ancestry check and must never authorise anything.** On runner#978 the recorded stamp genuinely WAS an ancestor of `main` while 2 commits (+277/-5) sat unlanded — S' would have PASSED. What refuses runner#978 is M4: `Mn` re-applies those hunks and reads `NOT-NOOP`, `Mb` finds the differing blobs, and N refuses independently. **S' narrows; it never admits** — arm 3's posture for L', and fixture **C44** holds it in both directions. ⚠️ **(N ∨ M4) has TWO branches and they are not equally gated — say which one proved the close.** On the M4 branch the syllogism above is the whole argument. On the N branch the close rests on arm 1's pre-existing no-op proof, which this row itself says *"does not by itself prove 'this PR's work landed'"* — and arm 4's detector is the weakest of the four, so that branch is a materially wider close than arms 1–3 gate. V' is what makes it safe; do not remove V' and keep the N branch. ⚠️ **Arm 4 CLOSES a PR whose content was later REVERTED**, inheriting arm 3's accepted refusal *(c)*: the PR DID land, and undoing it was a later decision a new PR owns. Nobody reading arm 4's detector would predict that, which is why it is written here; the close comment must show N's refusal line beside M4's proving line so the revert stays visible. **The close is identical to arms 1, 2 and 3** — re-read `headRefOid` immediately before `gh pr close` and abort if it moved; **NEVER `--delete-branch`**; and the comment states what was PROVEN: base ref + base sha; head sha; **which disjunct proved the close** — N's merged-tree oid, or M4's proving line (`Mb`'s `EQUIVALENT` line, or `Mn`'s `NOOP-PROVEN` line beside B-at-`SUCC`'s refusing line) TOGETHER WITH N's own refusal line; `SUCC` with the successor's `owner/repo#N` and `declared_at`; the first-parent result; V''s count; B-against-base's full `paths=`/`ghfiles=` line **with an explicit note that its verdict was NOT required and why, and that A4 was checked against `ghfiles=` rather than `paths=`**; GitHub's `changedFiles`; the commit count; coord's `superseded_by[].content` verdict when non-null, labelled as corroboration; and an explicit note that **arms 1, 2 and 3 do not apply and why** — specifically that coord holds `land_stamp: "none"` for this PR, so the close rests on the SUCCESSOR's land and on no record of this PR's own. ⚠️ **Do NOT reach arm 4 by widening arm 2's allowlist instead.** Arm 2's proof requires B's VERDICT against `origin/main`, which reads `NOT-EQUIVALENT paths=2 identical=1` on the one measured member of this population — a widened arm 2 would be inert on the very PR it was widened for. Fixture **C41** is what fails. ⚠️ **What arm 4 still cannot see, ledgered rather than assumed.** It covers only the sub-population coord can name cheaply: a predecessor with a RECORDED title declaration whose same-repo successor landed terminally. A PR whose content was rebase-carried onto `main` with **no** `supersedes #N` declaration remains invisible to all four arms, because finding it would mean running the proof against every commit on `main` for every open PR — the cost the detectors exist to avoid. That residue is **UNKNOWN, not zero**, and it must be said in the ledger rather than silently dropped. ⚠️ **Do NOT "simplify" this later by widening arm 2 instead.** Admitting this population into arm 2 means growing its `already_landed` equality to include `none` — the DEFAULT `rebase_block` carried by essentially every healthy open PR on the fleet, and the exact value fixture C15 exists to pin as REFUSED. That does not stretch arm 2, it destroys the cost filter and hands every open PR to the proof conjuncts. ⚠️ **There is now a SECOND route to that same mistake, and it does not look like a denylist at all: adding `block_reason_code == "already-landed-at-head"` to arm 2's allowlist.** It reads as a disciplined one-member widening, and it captures this entire population, because Guard 2 and `land_stamp == "current_head"` are the same merged-proposal-at-this-head fact by two probes. Arm 2 is evaluated first, so the population would arrive at a proof requiring B's verdict — which coord#1920 fails. Refused in arm 2's own cell, with the measurement, and **fixture-pinned since 2026-09-07**: the suite gained a `MT_BRCODE` detector input, and **C25** fails the moment `already-landed-at-head` is admitted to arm 2 (C26 catches the substring spelling that admits it by accident). The two populations also close on **different authorising proofs** (arm 2 requires B's verdict; arm 3 must not), and a single arm whose proof set varies by case makes the ledger's `closed=<n> (arm1=… arm2=… arm3=…)` rule unanswerable — that rule exists precisely so a reader can tell which proof was actually run. **Do NOT relax A for arm 1, do NOT add B to arm 1** (its path set is empty there by construction, so the conjunct would be vacuous — a vacuous conjunct that reads as a passing one is the silent-empty class), and do NOT key any arm's CLOSE on `merged_at`/`merge_commit` ancestry (runner#978 above) — ⚠️ **arm 3's L' IS that very check**, and it is admissible there ONLY as a second engine downstream of L, never as a substitute for it; see arm 3. **Ledger ALL FOUR arms separately:** `arm1-candidates-seen / arm2-candidates-seen / arm3-candidates-seen / arm4-candidates-seen / closed / declined-with-reason` each cycle. A repo showing coord `[already-landed]` verdicts alongside **zero** arm-2 candidates seen is a DETECTOR fault, not a clean repo — and the same rule holds one arm over: **zero arm-3 candidates seen in a repo where any open PR's card serves `land_stamp: "current_head"` is a DETECTOR FAULT too**. And one arm further over: **zero arm-4 candidates seen in a repo where any open non-draft PR's card serves `land_stamp: "none"` beside a `superseded_by[]` entry reading `successor_read: "ok"` and `successor_land_stamp: "terminal"` is a DETECTOR FAULT as well** — and beside that number, say that the undeclared-supersession residue is UNKNOWN rather than letting arm 4's coverage read as the whole population. An undetected population cannot even be declined-with-reason, which is how arm 2's whole population stayed invisible while arm 1 was being hardened twice — and how arm 3's stayed invisible one arm further over. **Fixtures — run them, do not read them:** `bash scripts/steward-empty-diff-fixtures-test.sh` (qontinui-claude-config). Plans: `2026-08-24-steward-empty-diff-reflex-blind-to-ff-lands` (arm 1), `2026-08-29-steward-empty-diff-reflex-blind-to-rebase-landed-frozen-diff` (arm 2), `2026-09-04-steward-empty-diff-reflex-has-a-population-between-its-two-arms` (arm 3), `2026-09-13-steward-arm3-noop-proof-decays-when-main-evolves-landed-paths` (arm 3's M), `2026-09-19-steward-arm3-m-false-refuses-hot-file-lands` (M's Mn engine), `2026-09-19-steward-empty-diff-reflex-blind-to-successor-carried-lands` (arm 4). |
| **Verified-green stuck** PR (train slow) | CLEAN + green + aged past the repo's *data-driven* `suggested_stuck_threshold_secs`, AND a **diagnosed coord defect** blocking autonomy. ⚠️ **PRECONDITION — read `rebase_landable`, because every SUMMARY field lies about this class.** A PR based on a NON-DEFAULT branch is never proposed at all, so it is not in the train population and cannot be *stuck in* it. Measured on qontinui-runner#1560 at 2026-09-17T00:03Z: `block_reason_code: none`, `blockers: []`, `mergeable: true`, `merge_state_status: CLEAN`, all six checks `success`/`skipped`, `confidence: fresh`, `verdict_stale: false` — and SIMULTANEOUSLY `rebase_landable: false`, `rebase_block: base_not_default`, `rebase_block_disposition: author_acts`, `land_stamp: none`, with `block_reason_repeat_count: 6` recording coord reaching `none` six times over. `blockers: []` means *nothing blocks the proposal from being EVALUATED*; on a non-default base there is no proposal to evaluate. `rebase_block_detail` says it outright: *"coord fast-forward-lands only the repo default branch `main`, so it will never be proposed."* So a steward reading the summary fields babysits such a PR to green and then waits forever, with every field agreeing with it. **`rebase_landable` is the field that decides.** When it is `false` with `base_not_default` this row does NOT apply and neither does a wedge diagnosis: the disposition is `author_acts` — retarget the PR at `main` (for a stacked PR, once its parent lands), which no amount of coord-side remediation will do for you. This enum member already appears in the empty-diff reflex row above, as one of the present-tense arms sitting OUTSIDE arm 2's two-member allowlist — an allowlist by deliberate choice, per that row's own argument that a denylist goes stale the moment coord adds a tenth member. It was never connected to THIS row, which is how a structurally unproposable PR could read as a slow train. ⚠️ **`green` here means ≥ 1 non-skipped check that PASSED and no non-skipped check that has not passed — never a head with ZERO checks.** "No check is failing" is **vacuously true** on a head whose CI never fired, so an unqualified reading admits a PR that has been verified by nothing at all into a row whose remedy is an operator hand-off for a merge. This is the same predicate `/babysit-prs` Step 6 states as an explicit precondition and Step 2 as a stamping rule — the definition lives there; keep this row's reading identical to it. A zero-check head is **not** this row: discriminate the **never-fired** class (`ci_check_row_count: 0` **and** `actions/runs?head_sha=<FULL 40>` → `total_count: 0`) from the benign **`no-baseline`** one (every workflow path-filtered off this head — which shows EITHER as zero check rows OR as rows that all concluded `skipped`; both counters read non-zero in the second case, so the two-counter test alone does not sort it — full four-arm table in `/babysit-prs` Step 2's *THREE causes* warning) exactly as the *Conflicting PR gets NO new CI* row below does, and route it there rather than here. | **Recovery-merge, NOT `--admin`.** `--admin` was observed failing 2026-07-04 ("required status checks expected") — a real observation, but the premise once recorded here to explain it ("bypass lists contain only `Integration:3825026`") is FALSE: measured 2026-07-29, the four `main-merge-gates` rulesets (runner, schemas, qontinui, ui-bridge) also carry `OrganizationAdmin` with `bypass_mode: always`; only coord/web/claude-config are App-only. Bypass lists differ per repo — re-read `bypass_actors` rather than restating a table. Whether `--admin` succeeds is untested by design; the steward's path is the deterministic one: rebase onto `origin/main`, required checks green on the up-to-date head, then plain `gh pr merge <n> --rebase`. **⚠️ Since PR #328 an agent CANNOT run that last command** — shared `.claude/settings.json` carries `deny: Bash(gh pr merge:*)`, which holds in every permission mode (`bypassPermissions` included), cannot be lifted by any local settings file or flag, and cannot be approved by a hook. So this row's recovery now ENDS at the hand-off: leave the audit trail (`/babysit-prs` Step 6 item 1), register a gate / escalate to the operator who holds the merge capability, and go to Tier 2 remediation. `--max-recovery-merges` is inert while the deny stands. Never route around it via `gh api .../pulls/N/merge` — `git-guard.sh` mechanically blocks that spelling too (briefly unwired fleet-wide alongside its unrelated destructive git/rm/cargo arms in qontinui-claude-config PR #567, then re-wired the same day narrowed to only this merge-route check; the destructive arms stay removed). |
| **Conflicting PR gets NO new CI — coord parks it in `ci-pending` forever** | `/reevaluate` returns `block_reason_code: "ci-pending"` with **`input_freshness.ci_check_row_count: 0`**, and `GET repos/<r>/actions/runs?head_sha=<FULL 40>` returns `total_count: 0` — i.e. CI never fired even once. | **Check `mergeable` FIRST; this is not a separate defect.** A CONFLICTING PR gets **no new `pull_request` workflow runs at all** — GitHub cannot compute `refs/pull/N/merge`, so nothing is scheduled (not queued, not skipped). No runs ⇒ no check rows ⇒ coord waits for CI that can never arrive, and the PR shows **no FAILING checks**, so any sweep that counts only reds reads it as healthy. Measured 2026-08-24 across `qontinui-dev-notes`: 4 of 9 stuck PRs had `total_count: 0`. **Remedy: resolve the conflict — CI follows.** For a **MERGEABLE** PR whose CI simply never fired, `gh pr close <n> && gh pr reopen <n>` fires `reopened` and schedules it (no content change, no new commit, same head sha) — verified on dev-notes#153, green in ~20s, and it then let coord reach its real verdict (`[already-landed] — close this PR`). On a CONFLICTING PR the same close/reopen is a **no-op**: it succeeds and schedules nothing (verified on #203/#84/#78). Do NOT go hunting for disabled Actions or missing workflow files. ⚠️ Use the **FULL 40-char** sha on that query — a short sha returns a silent 200 with `total_count: 0` and fakes this exact symptom. ⚠️ **`mergeable` is TERNARY, so those two arms are not a partition.** GitHub's GraphQL `MergeableState` enum is exactly `MERGEABLE \| CONFLICTING \| UNKNOWN` — checkable rather than remembered: `gh api graphql -f query='{ __type(name: "MergeableState") { enumValues { name } } }' --jq '.data.__type.enumValues[].name'`. `UNKNOWN` is the *not-yet-computed* state, and a two-armed split silently routes it into whichever arm is written first — here, the `MERGEABLE` arm, spending a close/reopen that schedules nothing and leaving the PR exactly as stuck. **Third arm: `UNKNOWN` → re-read it.** Poll `mergeable` again after a short delay and act only on a settled value; `UNKNOWN` is UNKNOWN, never a quiet synonym for `MERGEABLE`. How OFTEN this class reads `UNKNOWN` is **not measured** — the 9 dev-notes PRs recorded no `mergeable` values — so handle it because the enum has three members, not because it is expected. Also ⚠️ **a `MERGEABLE`/`CLEAN` read is GitHub's merge test passing, never "coord can rebase this"** — see the *Green-but-dirty* row's #148 measurement above; that caveat bears on a coord **rebase** hold, not on an absent workflow run, so it does not weaken the remedy here. |
| **Runs EXIST but none of them can ever conclude — zero CONCLUSIVE runs** | `block_reason_code: "ci-pending"` with **`input_freshness.ci_check_row_count` > 0** (so this is **NOT** the never-fired class in the row above), **`mergeable: MERGEABLE`** (so it is **NOT** the CONFLICTING class either), and `GET repos/<r>/actions/runs?head_sha=<FULL 40>` returning **`total_count > 0` where EVERY run is non-conclusive** — each one `cancelled`, `startup_failure`, or a `completed` run carrying only never-dispatched jobs (the job shape defined in the UNDISPATCHED section above: every job `status: queued`, `conclusion: null`, zero steps). ⚠️ The **FULL 40-char** sha rule from the row above applies here unchanged. Since **`coord@f3942732`** (2026-09-05) the block reason carries a `detail` payload `{pending_checks, pending_required}`, built in one place — `ci_pending_from`, `qontinui-coord` `crates/coord/src/pr_merge/predicate.rs` (`git grep -n 'fn ci_pending_from' origin/main -- crates/coord/src/pr_merge/predicate.rs`) — so the steward can now READ which check coord is waiting on directly off the block reason instead of deriving it (`pending_required: null` is UNKNOWN, `[]` means every pending check is advisory). That is how you confirm this row cheaply. ⚠️ It still does **not** say the check is UNCONCLUDABLE — that judgement is this row's whole job. ⚠️ **`gh pr checks` is blind to this by construction:** a required context that never reported is **absent, not red**, exactly as ccfg **#487** (MERGED 2026-08-30) already teaches in `/babysit-prs`, `/implement-plan` and `/vet-imp` — this row is that same observation one layer down, at the workflow run rather than at the check row, so read it there rather than as a second rule. | **The head must MOVE — and the two reflexes are both wrong, which is why this row names both.** ⚠️ **NOT a re-run.** That is the one-way hazard the UNDISPATCHED section measures: `POST .../rerun` on such a run leaves it permanently `queued` with `jobs=0`, after which `cancel` answers `409`, `rerun` answers `403`, and the red is immortal — and whether `startup_failure` behaves identically is deliberately UNMEASURED, so the class is treated as one. ⚠️ **NOT `gh pr close && gh pr reopen`.** That is the **MERGEABLE never-fired** remedy from the row above, and it schedules nothing useful here because runs ALREADY EXIST for this head; reopening does not re-dispatch a run that already failed to dispatch. **The remedy is a new head:** hand back to the author, or push a fresh commit — a new head gets a fresh dispatch. **Worked example, confirmed by observation:** `qontinui-coord#1658` sat **9.2 days** on `ci-pending` with **`block_reason_repeat_count: 620`** at head `9d3c3f80…` — one `completed/startup_failure` run whose only job `Gitleaks Secret Detection` was `queued`/`conclusion: null`/zero steps, plus one `completed/cancelled` run — and was **deliberately left alone**. On **2026-09-05T16:39:47Z its head moved to `20f63c3045522e5c99e94ed8a033bf4bdcdc44f1`**: both runs came back `completed/success`, the PR reached 9 check rows (8 success + 1 skipped), `mergeable: MERGEABLE`, `mergeStateStatus: CLEAN`, and the card read `Merge gate: passed / Scheduler: eligible`. So the prescribed remedy is **measured, not reasoned**, and NOT re-running was the correct call — no immortal red was created. Plan: `2026-09-04-undispatched-predicate-misses-startup-failure-and-coord-waits-forever`. |
| **`cancel` bucket misread as a failure** (triage error, not a wedge) | a PR's non-passing check is in the **`cancel`** bucket, not `fail` | **`cancelled` is NOT `failed` — it reached NO verdict.** Treating it as a red hides PRs that are actually fixable. Measured 2026-08-22: runner#1062 and #1055 were skipped as "has failures beyond `security`", but those extras were `Clippy diff-scoped (advisory) → cancel` and `test (ubuntu-22.04) → cancel` (the latter cancelled after a **6h** run). Both were genuinely `security`-only, i.e. the stale-base class; after a rebase both went fully green — and #1055's previously-cancelled ubuntu job reached a real verdict in 51m. When triaging, filter on `.bucket == "fail"` and report `cancel` separately. This is the per-PR twin of the main-baseline `cancelled` handling above. |
| **Stale read** (`freshness_next_action=refresh_github`) | `confidence ∈ {stale,unknown}` | Fire the concrete re-eval lever `POST <base>/pr-merge/prs/<owner>/<repo>/<pr>/reevaluate` (`.claude/commands/babysit-prs.md` → Step 5, lever 1 **"Force re-evaluation"** — cures stale snapshots), then wait one poll tick. Phase 2's freshness gate + Phase 1's post-land refresh do the re-read. Idempotent; not rate-limited. |
| **Orphaned-proposal residue** | `coord_proposals_resumed_after_failover_total` climbing without corresponding lands | Verify Phase-1 recovery ran (check the metric moves + lands resume); nudge re-eval on the affected PRs. If Phase-1 recovery regressed (metric climbs, no lands, no recovery), that's a **coord defect → Tier 2**. |
| **Re-eval drift starvation** | `pr_merge_reconcile_reeval_total{reason="stale_eval_backstop"}` flat ACROSS TWO SAMPLES while a stale backlog > 0 | Raise the non-drift reserve knob (the `e86d2026` mitigation is a knob), or alert. ⚠️ **The series name matters:** there is NO `reconcile_reeval_stale_eval_backstop` — that bare name greps zero forever and the detector silently never fires (verified absent in production 2026-08-19T23:52Z; see Step 1). ⚠️ **"Flat" is only measurable across TWO samples.** This is a COUNTER: its absolute value says nothing about whether it is advancing, so a single nonzero scrape is not health and a single scrape is not "flat". Scrape twice, spaced, and compare — measured 2026-08-19, it read `203` on every leader-shaped scrape across ~7 minutes, which is a *finding* only because it was sampled repeatedly. And per Step 1, a follower scrape renders the whole family as `0`, so an apparent drop to zero is a WRONG-REPLICA read, not a reset — counters cannot go backwards without a restart. Backlog side: `pr_merge_reconciler_backlog_stale` (leader-only gauge; its HELP says it converges to 0 in steady state, sustained non-zero = the frozen-row backlog is not draining) and `pr_merge_pr_state_stale_backlog` (leader-only; **cluster-consistent twin is `GET /pr-merge/health` → `pr_state_stale_backlog`, which is authoritative**). NOTE: a flat backstop at **0 with no backlog is HEALTHY** — do not fire on it. |
| **Phantom required context / aged prior** | the documented signatures (a required status context with no producer; a merge-state-unsettled dwell > 2× threshold on a fully-green head) | Arm the existing dark-launch flag / age-out per the shipped fixes; else escalate. |
| **Phantom-kill / un-credited land** (long-CI repos) | main ADVANCED with the PR's content, but the PR is still `OPEN` and coord re-cut a NEW candidate **identical to the main tip** + re-ran full CI; churn-guard terminal error "candidate CI never converged" fired **seconds after** a successful land | ROOT-CAUSED + FIXED 2026-07-18 (`b0fab9c8`/coord#1095): `recover_same_term_stalls` reclaimed a `landing` row mid-push (dequeue-anchored `leased_at` + off-lease CI wait ⇒ every >1h-CI land raced the reclaim). If it RECURS, the fix isn't serving — verify the ECS image (see Honest-bookkeeping); do NOT re-propose a PR whose content is already on main. |
| **`ci_timeout` < real CI livelock** | a GREEN candidate is re-cut ~seconds after its CI completes, forever; nothing lands on a repo whose CI > `COORD_MERGE_CI_TIMEOUT` (1800s default) | FIXED 2026-07-17 (`adb844d6`+`65a462bc`/coord#1070/#1078): `FallThrough`→`check_and_land`. Emergency lever if it recurs: raise `COORD_MERGE_CI_TIMEOUT` above the repo's CI wall-clock (fleet-wide; per-repo timers are the real fix — redesign P1). |
| **Actions-saturation firehose** (NOT a coord defect) | runner train stalls with green PRs queued; `gh run list` is dominated by ONE branch pushing every ~few min; candidate CI is queued-not-started | **Check the COMMITTER of the looping commits** (`gh api repos/<r>/commits`): `github-actions[bot]` ⇒ a self-triggering auto-commit workflow (e.g. nondeterministic codegen re-detecting its own drift — clorinde `pub mod` HashMap order, fixed runner#769 `55b04022`), NOT a looping agent. Fix = deterministic codegen / per-branch CI concurrency-cancel. Escalate to the branch owner; do NOT "stop an agent" that isn't the cause. |
| **Post-deploy proposal loss** (NOT a wedge) | coord went quiet on candidate-cutting right after a deploy / reconciler restart | ⚠️ **The "~80 min to rehydrate" figure was FOLKLORE — corrected 2026-07-20 by source trace.** No such constant exists in coord. **Scheduler recovery after a redeploy is ~17s** (leader TTL 15s — `DEFAULT_TTL_SECS`, `git grep -n 'DEFAULT_TTL_SECS' origin/main -- crates/coord/src/leader.rs` — plus a 2s tick — `COORD_MERGE_TICK_SECS`, `git grep -n 'COORD_MERGE_TICK_SECS' origin/main -- crates/coord/src/merge_scheduler.rs`, which defaults to 2). What can cost ~88 min is a *single proposal* whose in-flight CI is DISCARDED by the **Phase 2** requeue of `recover_orphaned_proposals` (`git grep -n 'fn recover_orphaned_proposals' origin/main -- crates/coord/src/merge_scheduler.rs`, then read its numbered `Phase` banners in order) — and only for `dry-rebasing`, `landing`, batch members, `speculative-ci`, and base-moved `awaiting-ci` with no live CI. A plain `awaiting-ci` singleton on an unmoved base is ADOPTED and costs nothing (**Phase 1** of that same sweep; `PROPOSALS_ADOPTED_AFTER_FAILOVER` is its counter). ~88min is runner's candidate-CI suite length (`git grep -n '88min' origin/main -- crates/coord/src/merge_scheduler.rs`, on the `candidate_ci_hard_cap` / `COORD_MERGE_CANDIDATE_CI_HARD_CAP_SECS` doc comments), not a recovery timer. **So: don't wait 80 minutes, and don't cite a system-wide recovery time — quote the per-proposal work at risk.** Correlate "idle since" with a task-def revision bump; WAIT, do not remediate. |

### Stranded conflicts that never converge — the merge-commit replay shape (Green-but-dirty row)

coord's dry-rebase replays the branch's **raw** commits onto the candidate. A conflict the author
already resolved *inside* a `Merge branch 'main'` commit is **not carried by the commits being
replayed** — a merge commit's resolution lives in the merge, not in its parents — so coord
re-encounters that same conflict on every attempt, and each new `main` tip the author merges in
adds one more. The attempt count therefore climbs **without ever converging**. Measured 2026-09-01
clearing `qontinui-web`'s entire stranded population (6 PRs; one stranded **17.7 days across 13
attempts**, another across 19): every one of the six carried between **2 and 21**
`Merge branch 'main'` commits.

- **The signature:** attempts rising while coord's `last_error` text stays *identical* is this
  shape, not a hard conflict. A hard conflict's error text moves as `main` moves; this one does
  not, because it is the same replayed commit failing the same way.
- **The cheap probe:** `git rev-list --merges --count origin/main..<head>`. A non-zero count on a
  long-stranded PR predicts it, at one command per PR — run it before spending any triage.
- ⚠️ **That probe is NECESSARY, NOT SUFFICIENT — and the discriminator is free, because you already
  hold it.** Measured 2026-09-05 on four `qontinui-web` PRs coord reported with `could not apply`:
  all four passed the merge-count probe (#1132 3 merges of 5 commits, #1137 2 of 8, #1224 1 of 3,
  #1223 1 of 2) and **only one was remediable**. Split them on GitHub's own `mergeable`:
  - `could not apply` + **`CLEAN` / `MERGEABLE`** ⇒ the replay shape; the remedies below apply.
  - `could not apply` + **`DIRTY` / `CONFLICTING`** ⇒ a **genuine content conflict with today's
    `main`**. The replay shape may *also* be present, but it is not the blocker, and removing the
    merge commits does not unstick the PR.
  The reason is stated in the Green-but-dirty row above and simply was not carried across to here:
  the replay shape leaves GitHub's **merge** test CLEAN *by construction*, because GitHub merges
  (taking both sides) while coord rebases (replaying commits). So a `DIRTY` read is positive
  evidence of a DIFFERENT blocker — the one case where `mergeStateStatus` is informative here.
- **The decisive test, when the cheap one is ambiguous: `git merge origin/main` on the UNMODIFIED
  head**, which still contains every merge commit and therefore every resolution the author made.
  If *that* conflicts, the conflict is content `main` gained **after** the author's last merge, not
  a resolution the rebase dropped. It conflicted for all three of the above, on file sets
  byte-identical to the rebase's. **Consequence, and it is the load-bearing one: the tree-equality
  proof is the entire safety argument for BOTH sub-shapes below, and here it cannot be
  CONSTRUCTED** — there is no clean merge to compare a rewritten branch against. So the remedy is
  not merely hard, it is *unprovable*, and an agent that rewrites the branch anyway has no argument
  that it kept the author's resolutions. **Stop and report; do not adjudicate the content.**
- ⚠️ **Two coord counters look like "attempts" and rank the population OPPOSITELY.**
  `coord_query_train_health` → `stranded_prs[].attempts` read `20 / 1 / 1 / 1` for
  #1132 / #1137 / #1224 / #1223, while `coord_pr_status` → `block_reason_repeat_count` read
  `8 / 6 / 5 / 25` for the same four, at the same time. Both are correct — they count different
  things (candidate attempts vs. re-observations of one block reason) — and neither field names its
  own scope. **Quote the surface alongside the number**; a bare "attempts=20" is ambiguous, and it
  was used to rank this work wrongly.
- **Sub-shape (a) — a plain rebase succeeds. PREFER IT:** `git rebase origin/main` drops the merge
  commits and replays only the real work, preserving authorship. Measured on **3 of the 6**, and
  for all three the rebased tree was **byte-identical** to the tree `git merge origin/main`
  produces — compare the two tree oids rather than assuming it, since that equality is what says
  the rebase kept every resolution the merges held.
- **Sub-shape (b) — a plain rebase CANNOT work**, because later commits exist solely to repair an
  earlier merge's resolution: every intermediate state is one those fixups assume away, so the
  rebase re-conflicts commit after commit no matter how many times it is retried. Remedy: take
  the tree from a clean `git merge origin/main`, flatten the branch onto `origin/main` with that
  tree, and **prove tree-equality against that merge** before pushing — the proof is the whole
  safety argument, because a flatten discards the branch's history and nothing else re-checks it.
  Preserve the original authorship (carry `--author` and the author date off the branch's own
  commits); push with `--force-with-lease`.

Either remedy produces a rewritten branch, so re-verify it exactly as the Green-but-dirty row
requires — build + lint + format-check, plus the checks under **Silent semantic conflicts**
below. A flatten changes more tree at once than a rebase does, which makes it the likelier place
for one of those to hide. And either remedy ends in the same `--force-with-lease` push to the
PR's branch as the Green-but-dirty row's, with the same carries-the-push check before and after
it (`coord-ff-lands.md` → "Pushing to a branch whose PR may already have landed"), re-testing the PRE-rebase head.

### Silent semantic conflicts — git's detector cannot see these (Green-but-dirty row)

Git's three-way merge is line-based: two disjoint hunks that are each individually
well-formed but jointly wrong produce **zero conflict markers**. "No `<<<<<<<` remains" is
therefore not "re-verified" — re-verify means **build + lint + format-check**, run AFTER the
rebase, per the Green-but-dirty row above. Nine subclasses, all measured across two soaks —
subclasses 1-5 in the overnight soak of 2026-08-31/09-01 (full write-up: coord finding
`4c4f637e-93cb-4fbc-ad1e-539050ce717c`); subclasses 6-9 in the taxonomy's extension to 9
instances + 2 adjacent shapes (coord finding `f28442a9-a14f-428d-8d79-a1c216862e3c`); the
cross-PR variant below is a separate finding, `69937d2c-f2fb-4e3b-b06b-18b31d1422ec`:

1. **Signature drift, disjoint call sites** — coord#1664 `merge_scheduler.rs`: the branch added
   a 4th param to `tenant_effective_cap`; main added 5 NEW callers of the old 3-arg signature.
   Disjoint hunks, no markers — reached CI red (rust-ci, coord-db-tests, clippy-diff); plain
   `cargo check` alone did not catch it (see the compile-target note below). <!-- lint-cargo-verification-form: ok the bare form is quoted here as the DEFECT, not prescribed — this line is the incident report -->
2. **Adjacent-add duplication** — coord#1735 `agent_registry.rs`: adjacent adds on both sides
   produced a duplicated `const` (compile error) and a duplicated test loop.
3. **Rename shadowing** — ccfg#459 `scripts/analyze-hook-latency.py`: main renamed a loop
   variable `matchers`→`declared`; the branch's code still read `matchers`, which post-rename
   silently resolves to the OUTER dict. No compiler exists to catch this in Python.
4. **Same-element duplicate attribute** — runner#1174 `CommandBar.tsx`: both sides added
   `aria-label` to the same JSX element → TS17001, caught ONLY by `tsc --noEmit`.
5. **Retirement drift, breaks a THIRD site** — runner#1175 `coord_auth_pin.rs`: main retired the
   `session-owed` kind from a validation table after driving its emitters to 0; an in-flight PR
   still emitted it. The breakage lands at the validation table — neither edit site — so grepping
   symbol *definitions* on both sides misses it; grep for values the PR still **emits** that main
   may have **retired**.
6. **Identical counter bumps collapsing** — both sides bump the SAME census literal to the SAME
   value; git takes it once where the intent was cumulative, so the merge is a silent
   **collapse**, not a conflict — nothing marks it, and the count is now one short. Hit TWICE in
   `crates/coord/src/alert_kind.rs`.
7. **Forked dependency/revision GRAPH — the breakage is in a graph, not in any file either side
   edited.** Two Alembic revisions chaining off one `down_revision` is a forked migration head:
   no two commits touch the same line, so git never flags it; qontinui-web#1149 would have failed
   the `alembic-heads-pr` required check had CI reached it in time. Measured again 2026-09-01 on
   qontinui-web **#1071 and #989** — both pointed `down_revision` at `coordtouch_01`, by then
   **three revisions stale** (`coordtouch_01 → grantorig_01 → coord_wusod_01 →
   pmf_scope_cols_01`), and `count_alembic_heads.py` reported `HEAD_COUNT=2`. Neither side edits
   the other's file, so there is **no textual conflict and no marker**, and it is invisible to
   every check in this list: no compiler sees it, mypy does not, `tsc` does not, and a grep for
   symbol *definitions* does not — the broken thing is the **revision graph**, which is neither
   edit site. It is not cheap to leave: **4 of #989's 19 commits existed only to chase this
   token**. **Generalise it beyond alembic:** any content-addressed or parent-pointer graph
   (migration chains, a lockfile carrying a resolved-tree hash, a generated-code manifest) can
   fork with zero textual conflict, for the same reason: the graph is not a file either side
   touched. **Detection:** after any rebase touching `alembic/versions/`, run the repo's own
   `count_alembic_heads.py` (or `alembic heads`) and require a **single** head; where the change
   is non-trivial, confirm with `alembic upgrade head` against a clean database. ⚠️ **Repair by
   HAND.** alembic is the sole author of `coord.*` schema and `alembic revision --autogenerate`
   is never run directly — served policy `production-and-cost` `alembic-sole-authorship` — so a
   fork is repaired by hand-editing `down_revision` **and** the `Revises:` docstring line, never
   by regenerating, and in qontinui-web never by `alembic merge`, which that same gate
   explicitly forbids.
8. **A second copy of already-fixed stale text** — a PR's new hunk carries a SECOND COPY of text
   `main` had already corrected elsewhere, silently reintroducing the bug it fixed. The
   duplicate copy never touches the fixed one's lines, so nothing conflicts (qontinui-web#1200).
9. **A brand-new file consuming a retired API** — `SessionsConsole.tsx`, added whole by the
   branch, called `<RecordRow accent=…>` after `main` had replaced the `accent` prop with
   `attention`. Caught ONLY by `tsc` (TS17001-adjacent, qontinui-web#1142). This one has **no
   shared line, no shared hunk, and no file present on both sides** — the smallest surface any
   subclass here presents, and it is still invisible to git.

**Instance 9 forces a reframe, stated plainly: this was never really about conflicts.** A rebase
produces an UNVERIFIED TREE, and only a build + typecheck + format-check + test run verifies it.
"No `<<<<<<<` markers remain" is a fact about git's diff algorithm, not a fact about the code —
subclass 9 shares no line, no hunk, and no file with the other side, and it is still wrong.

**Re-verify, in increasing cost — run in this order, and treat none of the earlier steps as a
substitute for the last:**

1. Diff both sides against the TRUE merge-base (`git diff <merge-base> <main> -- <path>` and
   `git diff <merge-base> <branch> -- <path>`), and read each commit's own diff — not just the
   post-rebase tree. Surfaces subclasses 1, 3, and 5 before a compiler has to.
2. Grep for duplicated definitions introduced on both sides over the same path (subclass 2),
   AND grep for values the PR still emits that main may have retired (subclass 5) — a
   definition-only grep is blind to the latter.
3. Compile / typecheck / format-check — the load-bearing floor; 1 and 2 are pattern-matches and
   can miss a variant shape.
   - **Rust:** `cargo check --all-targets` (bare `cargo check` is **insufficient** — <!-- lint-cargo-verification-form: ok the bare form is quoted here as the DEFECT, not prescribed — this is the one file that STATES the rule, and it quotes the bad form to name it -->
     coord#1664 passed it and still went red, because clippy and the DB tests must first
     COMPILE THE TEST BINARY, which `--all-targets` forces and plain `cargo check` does not) + <!-- lint-cargo-verification-form: ok the bare form is quoted here as the DEFECT, not prescribed — prose about what --all-targets forces -->
     `cargo clippy --all-targets` + `cargo fmt --check` (runner#1174 passed `cargo check`, <!-- lint-cargo-verification-form: ok the bare form is quoted here as the DEFECT, not prescribed — runner#1174 is cited as the counter-example -->
     `cargo clippy`, AND `tsc --noEmit`, then failed `test (ubuntu-22.04)` and <!-- lint-cargo-verification-form: ok the bare form is quoted here as the DEFECT, not prescribed — same sentence, continued -->
     `test (windows-latest)` on the single step "Format Rust code check" over one unwrapped
     string literal — `cargo fmt --check` is separately load-bearing, not implied by the others).
   - **Python:** the repo's own test suite / type-checker — no compiler exists, so subclass 3 is
     invisible to anything less.
   - **TypeScript:** `tsc --noEmit` — subclass 4 is invisible to `cargo`-shaped checks and to
     eslint rules that don't cross a JSX attribute list.
   - **Parent-pointer graphs (subclass 7):** no compiler reads these, so nothing above can
     catch them. After any rebase touching `alembic/versions/`, run the repo's own
     `count_alembic_heads.py` (or `alembic heads`) and require exactly one head; apply the same
     discipline to any lockfile or manifest carrying a resolved-tree hash.

⚠️ **The fleet's "never `cargo fmt`" rule is qontinui-COORD-ONLY — it does not generalize.** In
qontinui-runner `cargo fmt` is REQUIRED and CI enforces it (`cargo fmt --check` is its own CI
step, per runner#1174 above). Applying coord's rule to a runner PR silently reopens the exact
gap this subsection closes.

### Cross-PR silent conflicts — no pairwise rebase can see these

Everything above assumes ONE branch rebased against ONE base. A distinct failure needs no
rebase at all: three coord PRs (#1759, #1705, #1763) EACH pinned
`FLAT_ALERT_KINDS.len() == 122` against a `main` that read 121, each adding exactly one new
kind. Individually correct; jointly wrong — the second and third landers each falsify their OWN
assertion the moment the first one lands, and no PR in the set was ever rebased against either
of the others. The mechanism that makes it invisible: because all three write the SAME literal,
git's three-way merge takes the numeric assert SILENTLY and conflicts only on the PROSE beside
it — the visible conflict marker is a decoy pointing AWAY from the already-wrong number, not
toward it. (coord finding `69937d2c-f2fb-4e3b-b06b-18b31d1422ec`.)

**Rule:** before pinning any census literal, grep the OPEN PR SET for the same constant, not
just `main` — a rebase against `main` alone cannot see a sibling PR that has not landed yet. And
when a rebase conflicts on prose sitting next to a pinned number, treat the number itself as
suspect even though git did not flag it — a decoy conflict on the prose is exactly what a
jointly-wrong number under this mechanism looks like.

**Two adjacent shapes, worth naming separately because neither is a rebase-detection gap:**

- **The resolver's own tooling corrupts the result.** A naive split on `"======="` matched a
  `// ====…====` comment header and duplicated a whole test module (runner#1252) — found only by
  re-diffing the RESOLVED commit against BOTH parents, which should be its own distinct
  verification step, not folded into "the rebase looked clean."
- **A PR collides with itself.** One commit extracts code into a helper; a later commit,
  authored PRE-extraction and carried into the PR unchanged, edits the now-deleted inline
  version (coord#1705). No other branch or PR is involved — the self-inflicted conflict never
  reaches git's detector because both commits sit on the same side of every rebase.

### The no-op probe — conjunct N of the "Already-landed empty-diff PR" row

> ⚠️ **Dollar-digit booby trap — read this before editing anything in this file.** This file is
> a **slash-command body**. A dollar sign followed by a single digit is a **harness argument
> placeholder**, not a shell positional: Claude Code substitutes the invocation's argument words
> into the body before injecting it, indexed from zero, and leaves unfilled positions literal. On
> 2026-08-13 that silently rewrote the red-main detector's variables to garbage — *"29 queried, 29
> failed, every line UNKNOWN"* on every repo — while the **tracked file stayed correct**, so no
> read, diff, review or `git log -S` could see it. This file must contain **zero** dollar-digit
> sequences: every value goes through a **named** env var (the `RM_*` convention above, `MT_*`
> here), and **no shell function with positional parameters may be added**. The gate is
> `grep -cE '[$][0-9]' .claude/commands/merge-train-steward.md` -> `0`, and
> `scripts/steward-empty-diff-fixtures-test.sh` runs it.

Inputs arrive as **named env vars only**: `MT_BASE` (the PR's real base, `origin/<baseRefName>`,
freshly fetched) and `MT_HEAD` (the PR's current head sha). **Three exit codes, deliberately:**
`0` proven no-op, `1` proven **not** a no-op, `2` **UNKNOWN**. Only `0` may reach the close path,
and **`2` must never be collapsed into `1`** — the Tier-2 escalation path depends on the UNKNOWN
signal surviving. The snippet below is **byte-identical** to `scripts/merge-noop-probe.sh`;
`scripts/steward-empty-diff-fixtures-test.sh` fails if the two drift.

- ⚠️ **`git merge-tree` returns `1` for BOTH "conflict" and "I cannot read that object."**
  Measured in `qontinui-web`: a genuine conflict gives `rc=1`, and
  `git merge-tree --write-tree origin/main deadbeef…` (an absent object) *also* gives `rc=1`, on
  stderr `"not something we can merge"`. The exit code alone therefore **cannot** separate
  NOT-NOOP from UNKNOWN, and a naive `|| { echo NOT-NOOP; exit 1; }` reports "this PR is not a
  no-op" when the truth is "I could not read the head" — fail-safe for closing, but it destroys
  the signal escalation runs on. The `git cat-file -e` pair **must** run first; only then does
  `rc=1` mean conflict. An earlier draft of this very snippet had that bug.
- ⚠️ **Two unreadable objects compare EQUAL.** Measured: `git rev-parse -q --verify
  '<absent>^{tree}'` exits 1 printing nothing, so two different absent shas both yield the empty
  string and an equality test fires on **no evidence at all**. The trap is specific to
  `-q --verify`: plain `git rev-parse '<absent>^{tree}'` echoes the literal input and exits 128,
  so two absent shas compare *unequal* — the safe behaviour arrives **by accident**, and
  "cleaning up" to the `-q --verify` idiom silently removes it. Hence, on this path: every exit
  status checked, **no `2>/dev/null`, no `|| echo`, no `|| true`, no `|| 0`**; both oids validated
  as 40-char lowercase hex **and** non-empty; and the equality test never evaluated first.
- ⚠️ **Old git fails closed, and it must say so out loud.** `--write-tree` needs git >= 2.38 (this
  fleet's Linux box: 2.47.3; Git-Bash on the Windows members is **unverified**). P5 is **code, not
  prose** — an earlier draft had the version check in prose only, so a pre-2.38 git exited `129`
  and the old `|| { echo NOT-NOOP; exit 1; }` arm printed a **proven** verdict for an UNKNOWN.
  On an old-git machine this row simply never closes: that is the status quo, not a regression,
  but nobody may read a quiet steward as a working one. Do **not** substitute
  `head^{tree} == origin/main^{tree}` — measured, that guard decays within ~1h of the land as
  `main` advances, and it closes live work on any PR whose base is not the default branch.
- ⚠️ **Git-Bash path mangling.** `git rev-parse '<rev>:<path>'` is MSYS-mangled (see the field
  lessons further below). This snippet passes `MT_BASE` as a bare ref name with a `^{tree}`
  suffix, never a `rev:path`, which should be unaffected — **verify on a Windows fleet member**,
  and do not reach for `MSYS_NO_PATHCONV=1`, which breaks `git -C` paths.

<!-- BEGIN merge-noop-probe (byte-identical to scripts/merge-noop-probe.sh) -->
```bash
set -u
# Missing inputs are UNKNOWN, not a verdict. `set -u` ALONE exits 1, and under
# the three-exit-code contract below 1 means "proven NOT a no-op" -- measured
# 2026-08-25: `unset MT_BASE; bash merge-noop-probe.sh` -> rc=1. That is the
# same UNKNOWN-collapsed-into-a-proven-verdict defect P5 was written to stop,
# one level up. Check presence explicitly, and exit 2.
if [ -z "${MT_BASE:-}" ] || [ -z "${MT_HEAD:-}" ]; then
  echo "UNKNOWN MT_BASE or MT_HEAD unset or empty"; exit 2
fi
# Inputs arrive as named env vars ONLY: MT_HEAD (40-hex), MT_BASE (e.g. origin/main).
# P5 FIRST -- it is CODE, not prose. Without it a pre-2.38 git fails the
# merge-tree call and the old "|| { echo NOT-NOOP; exit 1; }" arm reported a
# PROVEN verdict for an UNKNOWN. Measured 2026-08-24 against a shimmed old git:
# the pre-vet snippet printed "NOT-NOOP merge conflicts", rc=1. See Vet findings.
MT_GV=$(git --version) || { echo "UNKNOWN git --version failed"; exit 2; }
MT_GN=${MT_GV#git version }
# A version string with no dot at all would otherwise parse as "2.2" and report
# UNKNOWN naming a version that does not exist. Same verdict, honest message.
case "${MT_GN}" in *.*) ;; *) echo "UNKNOWN git version has no minor: ${MT_GV}"; exit 2;; esac
MT_MAJ=${MT_GN%%.*}
MT_REST=${MT_GN#*.}
MT_MIN=${MT_REST%%.*}
# Validate the two components SEPARATELY. Concatenating them first cannot see an
# EMPTY component when the other is numeric -- measured 2026-08-25 against shims
# printing "git version 2." and "git version .38.1": both slipped past the gate
# and reached `[: : integer expression expected` on the next line, on a path this
# row states carries no suppressed shell errors.
case "${MT_MAJ}" in ""|*[!0-9]*) echo "UNKNOWN git major unparseable: ${MT_GV}"; exit 2;; esac
case "${MT_MIN}" in ""|*[!0-9]*) echo "UNKNOWN git minor unparseable: ${MT_GV}"; exit 2;; esac
if [ "${MT_MAJ}" -lt 2 ] || { [ "${MT_MAJ}" -eq 2 ] && [ "${MT_MIN}" -lt 38 ]; }; then
  echo "UNKNOWN git ${MT_MAJ}.${MT_MIN} lacks merge-tree --write-tree"; exit 2
fi
# P3, and it is not optional -- see the exit-code note below.
git cat-file -e "${MT_BASE}^{commit}" || { echo "UNKNOWN base object absent"; exit 2; }
git cat-file -e "${MT_HEAD}^{commit}" || { echo "UNKNOWN head object absent"; exit 2; }
MT_BT=$(git rev-parse "${MT_BASE}^{tree}") \
  || { echo "UNKNOWN base-tree unreadable"; exit 2; }
# Discriminate BY EXIT CODE, never by "non-zero". Only rc==1 is a conflict;
# 128/129 (bad object, unknown option, old git) are UNKNOWN. Measured: conflict
# rc=1, absent object rc=1, unknown option rc=129.
MT_MERGED=$(git merge-tree --write-tree "${MT_BASE}" "${MT_HEAD}"); MT_RC=$?
if [ "${MT_RC}" -eq 1 ]; then echo "NOT-NOOP merge conflicts"; exit 1; fi
if [ "${MT_RC}" -ne 0 ]; then echo "UNKNOWN merge-tree rc=${MT_RC}"; exit 2; fi
case "${MT_MERGED}" in *[!0-9a-f]*) echo "UNKNOWN merged-tree not hex"; exit 2;; esac
case "${MT_BT}"     in *[!0-9a-f]*) echo "UNKNOWN base-tree not hex";   exit 2;; esac
# Length 40 is SHA-1. In a SHA-256 repository this can never prove a no-op and
# always reports UNKNOWN -- fail-closed, and correct for this fleet, which is
# SHA-1 throughout. Widen deliberately if that ever changes; do not drop it.
[ ${#MT_MERGED} -eq 40 ] && [ ${#MT_BT} -eq 40 ] || { echo "UNKNOWN short sha"; exit 2; }
if [ "${MT_MERGED}" = "${MT_BT}" ]; then echo "NOOP-PROVEN tree=${MT_BT}"; exit 0; fi
echo "NOT-NOOP merged=${MT_MERGED} base=${MT_BT}"; exit 1
```
<!-- END merge-noop-probe -->

**Conjunct P6 is inlined the same way, and for the same reason.** ⚠️ It was first specified as
`bash scripts/merge-payload-guard.sh` — a path relative to *this* repo, while P1 requires the
steward to be standing in the **polled repo's** clone, where it resolves to nothing and the shell
exits **127**. That fails safe, but it makes a mandatory conjunct abort on every PR, which is the
never-fires class this whole change exists to close, reached by a third door. **Inlining is what
makes a conjunct runnable from the mandated CWD**; gate0 in the fixture suite pins both inlined
copies against their files. Same input convention (`MT_BASE`/`MT_HEAD`), same three exit codes —
here `0` means the payload IS in the tree (N's verdict is meaningful), `1` means it is **not**
(refuse, escalate to Tier 2), `2` UNKNOWN.

⚠️ **Two limits, both measured, both graph-isomorphism arguments — so neither is a bug a better
predicate could fix, and neither may be "closed" by weakening the guard.** *(1)* **Parent order.**
The same history-reconcile authored on the **release** side (`git checkout rel; git merge trunk`,
PR'd `rel → trunk`) carries the same two commits with the merge's parents **swapped**, so its first
parent is not on the base and it **passes** — and that graph is identical to the merge-forward
shape this row exists to close (web#1033), so nothing over the graph can separate them. It is an
**accepted close**, pinned as fixture C13 so that deleting P6a is visibly a change in what closes
rather than a silent one. *(2)* **One over-refusal.** A legitimately sha-preserving ff-landed PR
that then took a gratuitous `--no-ff` merge-forward is graph-identical to the unhydrated C10 shape
and is **refused** — fail-safe (it escalates to Tier 2, it never closes wrongly), but say so out
loud, because a Tier-2 escalation on a PR that plainly landed otherwise reads as a bug to whoever
picks it up.

<!-- BEGIN merge-payload-guard (byte-identical to scripts/merge-payload-guard.sh) -->
```bash
set -u
# Conjunct P6 of the Tier-1 "Already-landed empty-diff PR" row: is this PR's
# PAYLOAD in the TREE at all? The no-op proof (N, scripts/merge-noop-probe.sh)
# compares trees, so a PR whose payload is NOT a tree change is invisible to it
# and reads NOOP-PROVEN while carrying work a human still intends to use.
# Two such shapes were MEASURED 2026-08-25 (both close under P AND N AND V AND A alone):
#   - a history-reconcile / back-merge PR ("merge release into main" after the
#     same content landed on main under a different sha): commits=2,
#     changedFiles=0, NOOP-PROVEN -- and the payload is the merge EDGE. Closing
#     it loses the edge, and the next merge add/add-conflicts on byte-identical
#     content (rc=1 UU), the wedge the Green-but-dirty row documents.
#   - a marker PR whose only commit is `git commit --allow-empty` (release
#     marker / CI re-trigger): commits=1, changedFiles=0, NOOP-PROVEN.
# Inputs are named env vars ONLY: MT_BASE, MT_HEAD.
# Exit codes match the probe's contract: 0 payload IS in the tree (N's verdict
# is meaningful), 1 payload is NOT in the tree (refuse, escalate to Tier 2),
# 2 UNKNOWN. As everywhere on this path, 2 must never collapse into 1.
#
# TWO LIMITS, both measured, both graph-isomorphism arguments -- so neither is a
# bug to be fixed by a better predicate, and both are stated so nobody "closes"
# them by weakening the guard:
#   - PARENT ORDER. A reconcile authored on the RELEASE side (`git checkout rel;
#     git merge trunk`, PR'd rel -> trunk) has the same two commits with the
#     parents SWAPPED, so its first parent is not on the base and it PASSES.
#     That graph is identical to the merge-forward shape this whole row exists
#     to close (web#1033), so no predicate over the graph can separate them.
#     It is an ACCEPTED close, pinned by fixture C13.
#   - ONE OVER-REFUSAL. A legitimately sha-preserving ff-landed PR that then
#     took a gratuitous `--no-ff` merge-forward is graph-identical to the
#     unhydrated C10 shape and is REFUSED. Fail-safe -- it escalates to Tier 2,
#     it never closes wrongly -- but say so, because a Tier-2 escalation on a
#     landed PR otherwise reads as a bug to whoever picks it up.
if [ -z "${MT_BASE:-}" ] || [ -z "${MT_HEAD:-}" ]; then
  echo "UNKNOWN MT_BASE or MT_HEAD unset or empty"; exit 2
fi
git cat-file -e "${MT_BASE}^{commit}" || { echo "UNKNOWN base object absent"; exit 2; }
git cat-file -e "${MT_HEAD}^{commit}" || { echo "UNKNOWN head object absent"; exit 2; }
# The sha-PRESERVING ff-land: the head is already reachable from the base, so it
# adds neither a commit nor an edge. Nothing for this guard to find; V's commit
# count is what governs there.
# Discriminate BY EXIT CODE, never by "non-zero" -- the same discipline the
# probe states for `merge-tree`. `git merge-base --is-ancestor` returns 128 on a
# bad rev (measured), and treating that as a plain "no" turns an UNKNOWN into a
# verdict. Only 0 and 1 are answers.
git merge-base --is-ancestor "${MT_HEAD}" "${MT_BASE}"; MT_ARC=$?
if [ "${MT_ARC}" -eq 0 ]; then
  echo "IN-TREE head is already an ancestor of the base"; exit 0
fi
if [ "${MT_ARC}" -ne 1 ]; then echo "UNKNOWN is-ancestor rc=${MT_ARC} on the head"; exit 2; fi
# P6a -- history-reconcile: the head is a MERGE commit whose FIRST parent is
# already on the base. Measured discriminator: the merge-forward shape this row
# exists for (web#1033) is also a merge commit, but its first parent is the
# branch's own tip and is NOT on the base, so it passes.
MT_HP=$(git rev-list --parents -n 1 "${MT_HEAD}") || { echo "UNKNOWN cannot read head parents"; exit 2; }
MT_NP=0
for MT_W in ${MT_HP}; do MT_NP=$((MT_NP + 1)); done
MT_NP=$((MT_NP - 1))
if [ "${MT_NP}" -gt 1 ]; then
  git merge-base --is-ancestor "${MT_HEAD}^1" "${MT_BASE}"; MT_PRC=$?
  if [ "${MT_PRC}" -eq 0 ]; then
    echo "PAYLOAD-NOT-IN-TREE merge edge: head is a merge whose first parent is already on the base"
    exit 1
  fi
  if [ "${MT_PRC}" -ne 1 ]; then echo "UNKNOWN is-ancestor rc=${MT_PRC} on the first parent"; exit 2; fi
fi
# P6b -- marker PR: EVERY commit ahead of the base is empty.
MT_LIST=$(git rev-list "${MT_BASE}..${MT_HEAD}") || { echo "UNKNOWN cannot list commits"; exit 2; }
if [ -z "${MT_LIST}" ]; then echo "IN-TREE no commits ahead of the base"; exit 0; fi
MT_LIVE=0
for MT_C in ${MT_LIST}; do
  MT_CP=$(git rev-list --parents -n 1 "${MT_C}") || { echo "UNKNOWN cannot read parents of ${MT_C}"; exit 2; }
  MT_CN=0
  for MT_W in ${MT_CP}; do MT_CN=$((MT_CN + 1)); done
  if [ "${MT_CN}" -lt 2 ]; then MT_LIVE=$((MT_LIVE + 1)); continue; fi
  git diff-tree --quiet "${MT_C}^" "${MT_C}"; MT_DRC=$?
  if [ "${MT_DRC}" -eq 1 ]; then MT_LIVE=$((MT_LIVE + 1)); continue; fi
  if [ "${MT_DRC}" -ne 0 ]; then echo "UNKNOWN diff-tree rc=${MT_DRC} on ${MT_C}"; exit 2; fi
done
if [ "${MT_LIVE}" -eq 0 ]; then
  echo "PAYLOAD-NOT-IN-TREE every commit ahead of the base is empty"; exit 1
fi
echo "IN-TREE ${MT_LIVE} tree-changing commit(s) ahead of the base"; exit 0
```
<!-- END merge-payload-guard -->

**Conjunct B is inlined the same way — its VERDICT is required in ARM 2 only, and it is a THIRD
engine, not a replacement for A.** Arm 3 runs the same snippet twice: against
`origin/<baseRefName>` for its rename-aware `ghfiles=` COUNT only (A''), and against coord's recorded `merge_commit`
as the blob engine (Mb) of conjunct **M**. M's second engine, Mn, is the inlined N snippet against
the same `merge_commit`, so M adds no inlined copy of its own. B reads each side with `git --literal-pathspecs ls-tree --full-tree` and forces
`diff.relative=false`, so it gives the same answer from any subdirectory (fixture P8). A
`GIT_GLOB_PATHSPECS`, `GIT_NOGLOB_PATHSPECS` or `GIT_ICASE_PATHSPECS` in the environment makes
`--literal-pathspecs` fail with rc `128`, and B then reads UNKNOWN on every PR. That fails safe but
silently, so unset those variables before running B. Arm 2 exists because a **rebase-landed** PR's GitHub diff is *frozen* against
its recorded base sha, so `changedFiles` stays NON-zero forever and conjunct **A is structurally
unsatisfiable** — the same shape of unreachability that ancestry has on the ff-land shape, one
conjunct over. Measured on `qontinui-schemas#144` (2026-08-29, re-measured 2026-09-01 after `main`
advanced six commits): `changedFiles=13 commits=1 state=OPEN`, ancestry `rc=1`, N `NOOP-PROVEN`,
P6 `IN-TREE 1`, and every one of the PR's own 13 files byte-identical to `origin/main`.

⚠️ **B is NOT a second SOURCE, and arm 2 must not be read as "A replaced".** A is *cross-source* —
`changedFiles` is computed by GitHub, on GitHub's data, by code this fleet does not run — and its
real job is the failure no amount of local git can see: a **stale or wrong local clone**. B is git
again, over the **same object store N already read, in the same working copy**; a wrong clone makes
N and B agree and both be wrong. So arm 2 carries its cross-source engine as **A'** (below), and
**P1's `git remote get-url origin` identity check and P2/P3's exit-checked fetches are promoted from
hygiene to load-bearing** there. Do not carry them over from arm 1 as unchanged boilerplate: in arm
1 A covers for them, and in arm 2 nothing does.

⚠️ **B decays toward REFUSAL, and a B refusal is NOT evidence that the work did not land.** Once
`main` evolves any path the PR touched, B reads `NOT-EQUIVALENT` and arm 2 declines — the PR stays
open, which is fail-safe. That direction is the whole reason B is safe to make a required conjunct
where the `head^{tree} == origin/main^{tree}` predicate was measured and REJECTED: that one decayed
toward *closing live work*, within an hour. B measured stable at +3 days and +6 `main` commits.
Practical consequence: arm 2 catches the **recently** rebase-landed PR — the re-proposed-forever
population it exists for — and a stale one `main` has evolved past falls out of arm 2 into Tier 2. That same decay is why arm 3
carries **M**: pointed at the land commit instead of the moving base, the identical comparison no
longer decays with `main`'s LATER evolution. It still refuses a land that `main` edited on the same
path BEFORE the land, which is why M also carries Mn (the no-op probe at the stamp; plan
`2026-09-19-steward-arm3-m-false-refuses-hot-file-lands`).

Same input convention (`MT_BASE`/`MT_HEAD`), same three exit codes — `0` EQUIVALENT (proven), `1`
NOT-EQUIVALENT **including the empty-path-set routing refusal** (an empty set IS the
`changedFiles==0` population and belongs to arm 1), `2` UNKNOWN. Only `0` may reach the close path
and `2` must never collapse into `1`.

⚠️ **The `-z` output goes to a FILE, never to a command substitution — and that line is measured,
not stylistic.** bash **discards NUL bytes in command substitution**: on schemas#144's real 13-path
set, `MT_PATHS=$(git diff --name-only --no-renames -z ...)` warns *"ignored null byte in input"* and
concatenates all thirteen paths into one meaningless string, which then reads absent on both sides,
trips the `MT_EXIST` guard and refuses. Arm 2 would have **never fired, on any PR**, while every
line of this prose still read correct — the shipped-implementation-does-not-match-the-prose class
that P5 was written to stop. Reading from a file rather than a pipe also keeps the loop in the
current shell so its counters survive.

<!-- BEGIN merge-blob-equivalence (byte-identical to scripts/merge-blob-equivalence.sh) -->
```bash
set -u
# Conjunct B of the Tier-1 "Already-landed empty-diff PR" row: are this PR's OWN
# files already at the base's content, tree entry for tree entry (mode, type AND
# blob id -- never the blob id alone)? Its VERDICT is required in
# ARM 2. ARM 3 runs it twice: against the base for its ghfiles= COUNT only (A''),
# and with MT_BASE set to coord's recorded merge_commit as the blob engine (Mb)
# of conjunct M, which does not decay when main later evolves the PR's own lines
# (plan 2026-09-13-steward-arm3-noop-proof-decays-when-main-evolves-landed-paths).
# M's second engine is the no-op probe run at the same stamp (plan
# 2026-09-19-steward-arm3-m-false-refuses-hot-file-lands).
#
# N (scripts/merge-noop-probe.sh) answers "merging this PR changes nothing".
# B answers a different question over the same object store, and the row needs
# both because arm 2 cannot use conjunct A: on a rebase-landed PR GitHub freezes
# the diff against the recorded base sha, so changedFiles stays NON-zero and A is
# unsatisfiable. See the row's "arm 2" block and plan
# 2026-08-29-steward-empty-diff-reflex-blind-to-rebase-landed-frozen-diff.
#
# B is NOT a second SOURCE -- it is git again, in the same clone N read. The
# cross-source engine for arm 2 is A' (GitHub's changedFiles == the ghfiles=
# count this script prints -- ghfiles=, NOT paths=; see the rename-aware block
# below), and the clone-identity guarantee rests on P1's
# `git remote get-url origin` check plus P2/P3's exit-checked fetches. Do not
# read B as a replacement for A.
#
# Three exit codes, the same contract as N and P6:
#   0  EQUIVALENT      -- proven: every path matches, set non-empty, base has content
#   1  NOT-EQUIVALENT  -- proven refusal, INCLUDING "empty path set" (that is arm 1's
#                         population, not arm 2's)
#   2  UNKNOWN         -- nothing was proven in either direction; route to Tier 2
# Only 0 may reach the close path, and 2 must NEVER collapse into 1.
#
# Inputs arrive as named env vars ONLY: MT_BASE (e.g. origin/main), MT_HEAD.
# No positional parameters anywhere -- this file is inlined verbatim into a
# slash-command body, where a dollar followed by a digit is a harness argument
# placeholder (fixtures gate 1 greps the whole command file for one).

# Missing inputs are UNKNOWN, not a verdict. `set -u` ALONE exits 1, and under
# the contract above 1 means "proven NOT equivalent" -- the same
# UNKNOWN-collapsed-into-a-proven-verdict defect P5 was written to stop.
if [ -z "${MT_BASE:-}" ] || [ -z "${MT_HEAD:-}" ]; then
  echo "UNKNOWN MT_BASE or MT_HEAD unset or empty"; exit 2
fi

# NO git-version gate here, and that is deliberate rather than forgotten: every
# command below (merge-base, diff --name-only -z, ls-tree with
# --literal-pathspecs, cat-file -e) long predates git 2.0. N carries P5 because `merge-tree
# --write-tree` is 2.38+; nothing in B is. Add a gate here only if B ever grows a
# dependency that actually has a floor. (--literal-pathspecs needs 1.8.5.) A
# GIT_GLOB_PATHSPECS / GIT_NOGLOB_PATHSPECS / GIT_ICASE_PATHSPECS in the
# environment makes --literal-pathspecs fail rc 128, so B reads UNKNOWN on every
# PR -- fail-safe but silent; unset them before running B.

git cat-file -e "${MT_BASE}^{commit}" || { echo "UNKNOWN base object absent"; exit 2; }
git cat-file -e "${MT_HEAD}^{commit}" || { echo "UNKNOWN head object absent"; exit 2; }

# The path set comes from the MERGE-BASE, never from MT_BASE directly. A two-dot
# `git diff MT_BASE MT_HEAD` also lists every path the BASE changed that this head
# never touched -- each differs by construction, so B would refuse every PR whose
# base has moved at all. Merge-base yields the head's OWN path set -- the same
# fork point GitHub computes changedFiles from, which is what makes the two
# comparable at all. (COMPARABLE, not equal: the rename-aware ghfiles= count
# below is the number the cardinality conjuncts use, because this set splits a
# rename into two entries and GitHub counts it as one.)
MT_MB=$(git merge-base "${MT_BASE}" "${MT_HEAD}"); MT_MBRC=$?
# rc 1 is "no merge base" (unrelated histories). Nothing was compared, so nothing
# was proven -- UNKNOWN, never a refusal.
[ "${MT_MBRC}" -eq 0 ] || { echo "UNKNOWN merge-base rc=${MT_MBRC}"; exit 2; }
case "${MT_MB}" in ""|*[!0-9a-f]*) echo "UNKNOWN merge-base not hex"; exit 2;; esac

# -z: NUL-delimited. A path containing a newline would otherwise split into two
# entries, shrinking the set that has to match and passing a PR on fewer files
# than it actually changes.
#
# It goes to a FILE, never to a command substitution. Measured 2026-09-01 on the
# real 13-path schemas#144 set: `MT_PATHS=$(git diff ... -z ...)` makes bash warn
# "ignored null byte in input" and STRIP every NUL, concatenating all 13 paths
# into one meaningless string. B would then compare a single path absent on both
# sides, hit the MT_EXIST guard and refuse -- so arm 2 would never fire, on any
# PR, while every line of the prose still read correct. A redirect preserves the
# NULs, and reading from a file (not a pipe) keeps the loop in THIS shell so the
# counters below survive it.
MT_PF=$(mktemp) || { echo "UNKNOWN cannot mktemp"; exit 2; }
# -c diff.relative=false: with diff.relative=true (git 2.28+) a run from a
# subdirectory would list only that subtree, with the prefix stripped (fixture P8).
git -c diff.relative=false diff --name-only --no-renames -z "${MT_MB}" "${MT_HEAD}" > "${MT_PF}"; MT_DRC=$?
if [ "${MT_DRC}" -ne 0 ]; then
  rm -f "${MT_PF}"; echo "UNKNOWN diff rc=${MT_DRC}"; exit 2
fi

# ---- the SECOND, rename-aware count: `ghfiles=` -----------------------------
# `--no-renames` above is RIGHT for the comparison set and must stay. A rename
# old->new becomes {old, new}: `old` is absent at head AND absent at base
# (equal), `new` is present at both with the same blob (equal). That is a
# conservative SUPERSET which still proves the property -- and the extra element
# is load-bearing, because it is the only thing that checks the DELETION half of
# the rename landed. Measured 2026-09-20 on a partial land (the base took the
# new file and never deleted the old one): a `-M`-derived comparison set sees
# only {new}, reads them equal and proves EQUIVALENT, while merging the PR would
# still delete `old` from the base. `--no-renames` refuses it correctly.
#
# But that same split makes the COUNT wrong for the cardinality conjuncts.
# GitHub's `changedFiles` counts a rename as ONE file; `--no-renames` prints TWO.
# So A' (arm 2), A'' / A''' (arm 3) and A4 (arm 4) -- all of which require
# `changedFiles` to equal a count from this script -- failed on EVERY renaming
# PR, and the arm aborted to Tier 2 instead of closing. Measured 2026-09-20:
# ccfg#1035 `changedFiles=7` against `paths=8` (1 rename); web#1424
# `changedFiles=54` against `paths=66` (12 renames). Both had every other
# conjunct proven. Plan
# `2026-09-20-steward-cardinality-conjunct-miscounts-renames`.
#
# So B prints a SECOND number beside the first, from the same merge-base, and
# the four cardinality conjuncts read THAT one. `paths=` keeps its meaning:
# the size of the set B actually compared, which `identical=` and
# `base_existing=` are counted against.
#
# `--name-only` (not `--name-status`): under `-z`, `--name-only -M` emits
# exactly ONE NUL-terminated record per changed file -- the DESTINATION path for
# a rename -- so the count is just the record count. `--name-status -z` emits a
# rename as THREE fields (`R087\0old\0new\0`) and would need parsing.
#
# `-M` is passed EXPLICITLY, never inherited: measured, a command-line `-M`
# overrides `diff.renames` in both directions (`false` and `copies` both gave
# the same rename-collapsed set). Copy detection cannot skew it either -- a copy
# prints only the new path, which is the one file GitHub counts as added.
#
# Two residuals, both FAIL-SAFE and both strictly better than the status quo
# they replace (which disagreed on every rename): git's `-M` uses a 50%
# similarity default while GitHub runs its own detector, so at the margin the
# two can disagree about a single pair; and `diff.renameLimit` exhaustion makes
# git silently skip rename detection, reverting this count toward the
# `--no-renames` one. Either way the conjunct refuses and the PR stays open.
MT_GPF=$(mktemp) || { rm -f "${MT_PF}"; echo "UNKNOWN cannot mktemp"; exit 2; }
git -c diff.relative=false diff --name-only -M -z "${MT_MB}" "${MT_HEAD}" > "${MT_GPF}"; MT_GDRC=$?
if [ "${MT_GDRC}" -ne 0 ]; then
  rm -f "${MT_PF}" "${MT_GPF}"; echo "UNKNOWN rename-aware diff rc=${MT_GDRC}"; exit 2
fi
MT_GF=0
while IFS= read -r -d '' MT_GP; do
  MT_GF=$((MT_GF + 1))
done < "${MT_GPF}"
rm -f "${MT_GPF}"

# ls-tree separates "<mode> <type> <oid>" from the path with a TAB; a listing
# with a NEWLINE in it matched more than one entry.
MT_TAB=$(printf '\t')
MT_NL=$(printf '\nx'); MT_NL=${MT_NL%x}
MT_N=0
MT_SAME=0
MT_EXIST=0
MT_FIRSTDIFF=""
while IFS= read -r -d '' MT_P; do
  MT_N=$((MT_N + 1))
  # Each side is read as its tree ENTRY -- "<mode> <type> <oid>" -- never as a
  # bare blob id. A blob id carries no mode, so the old `rev-parse <rev>:<path>`
  # compared a lost exec bit, or a symlink swapped for a regular file with the
  # same bytes, as EQUAL. Arm 2 never paid for that (it also requires N, and
  # merge-tree compares tree oids, which include the mode); arm 3 can close on M
  # alone, and the independent vet of ccfg#943 closed a PR with main never
  # carrying its chmod (fixture C32). --literal-pathspecs: a path containing `*`,
  # `?` or `[` must match itself only. Both commits are proven readable, so a
  # non-zero ls-tree is UNKNOWN; an EMPTY listing means the path is absent there.
  # --full-tree: ls-tree resolves a pathspec against the CURRENT DIRECTORY, while
  # diff --name-only prints repo-root paths. Without it, a run from a subdirectory
  # compared the WRONG files and proved EQUIVALENT (fixture P8).
  MT_HL=$(git --literal-pathspecs ls-tree --full-tree "${MT_HEAD}" -- "${MT_P}"); MT_HRC=$?
  MT_BL=$(git --literal-pathspecs ls-tree --full-tree "${MT_BASE}" -- "${MT_P}"); MT_BRC=$?
  if [ "${MT_HRC}" -ne 0 ] || [ "${MT_BRC}" -ne 0 ]; then
    rm -f "${MT_PF}"; echo "UNKNOWN ls-tree rc=${MT_HRC}/${MT_BRC}"; exit 2
  fi
  case "${MT_HL}${MT_BL}" in
    *"${MT_NL}"*) rm -f "${MT_PF}"; echo "UNKNOWN ls-tree listed more than one entry"; exit 2 ;;
  esac
  MT_HB=${MT_HL%%"${MT_TAB}"*}; [ -n "${MT_HB}" ] || MT_HB="absent"
  MT_BB=${MT_BL%%"${MT_TAB}"*}; [ -n "${MT_BB}" ] || MT_BB="absent"
  if [ "${MT_BB}" != "absent" ]; then MT_EXIST=$((MT_EXIST + 1)); fi
  if [ "${MT_HB}" = "${MT_BB}" ]; then
    # Both-absent lands here and is CORRECT at the path level: a deletion this PR
    # made that the base has already taken is equivalent. What stops an all-absent
    # read (a wrong or stale clone -- every path resolves to nothing) from passing
    # is the SET-level MT_EXIST guard below, not this comparison.
    MT_SAME=$((MT_SAME + 1))
  elif [ -z "${MT_FIRSTDIFF}" ]; then
    MT_FIRSTDIFF="${MT_P}"
  fi
done < "${MT_PF}"
rm -f "${MT_PF}"

# An EMPTY path set is not arm 2's population at all -- it is the changedFiles==0
# population, which arm 1 owns under conjunct A. A proven routing refusal, not an
# UNKNOWN.
if [ "${MT_N}" -eq 0 ]; then
  echo "NOT-ARM2 empty path set (changedFiles==0 population -- arm 1 owns it)"; exit 1
fi

if [ -n "${MT_FIRSTDIFF}" ]; then
  echo "NOT-EQUIVALENT paths=${MT_N} ghfiles=${MT_GF} identical=${MT_SAME} first-differing=${MT_FIRSTDIFF}"
  exit 1
fi

# The silent-empty guard, and it is a SET-level test on purpose. Requiring at
# least one path whose blob genuinely exists in the base is what stops "every
# comparison read absent on both sides" -- the shape a wrong or stale clone
# produces -- from reading as "identical everywhere". Accepted cost: a PR whose
# entire payload is deletions that have all landed refuses here. That is
# fail-safe (the PR stays open) and is the correct trade against a close that
# proved nothing.
if [ "${MT_EXIST}" -eq 0 ]; then
  echo "NOT-EQUIVALENT paths=${MT_N} ghfiles=${MT_GF} but no compared blob exists in the base"; exit 1
fi

echo "EQUIVALENT paths=${MT_N} ghfiles=${MT_GF} identical=${MT_SAME} base_existing=${MT_EXIST}"
exit 0
```
<!-- END merge-blob-equivalence -->

**The fixtures — run them, do not read them.** `bash scripts/steward-empty-diff-fixtures-test.sh`
(this repo; its tracked `.sh` files are mode `100644`, so invoke through `bash`, never as a
program — a fresh CI checkout has no exec bit and `Permission denied` rc=126 reads as a
behavioural failure). It builds throwaway git repos, touches no network and no fleet state,
and pins the git shapes that argue for each conjunct, plus FOUR static gates over this
file and the UNKNOWN cases. Each row asserts **N's full output string** (a prefix match cannot tell
`NOT-NOOP merged=<oid>` from `NOT-NOOP merge conflicts`), **B's full output string** (same reason — a
routing `NOT-ARM2` and a proven `NOT-EQUIVALENT` are different facts), **P6's exit code**, the
**commits-ahead count** — which is the empirical basis of the V argument and must be asserted, not
merely printed — and, since 2026-09-03, **which ARM the detector routes the case to**, from the
fixture inputs `MT_CHANGED` (GitHub's `changedFiles`), `MT_COORD` (coord's `rebase_block`),
`MT_LAND` (coord's `land_stamp`) and — since 2026-09-07 — `MT_BRCODE` (coord's `block_reason_code`,
which is what pins arm 2's allowlist member (ii) and refuses the arm-3-code mis-widening).
`MT_EXPECT_ARM=0` means *the detector refuses it before any probe is credited*, which is a different
fact from *the probes refuse it* and was inexpressible while routing read `MT_CHANGED` alone:

| Case | N | P6 | ahead | What it proves |
|---|---|---|---|---|
| C1 the #1033 shape — merge-forward onto already-landed commits | `NOOP-PROVEN` | pass | 2 | the row must **close** this; the retired three-guard rule could not |
| C2 live work on top of the C1 shape | `NOT-NOOP merged=…` | pass | 3 | N refuses on tree content |
| C3 branch parked exactly at the base tip | `NOOP-PROVEN` | pass | **0** | **V is the only stopper** — A reads `0`, P1/P2/P6 all pass |
| C4 live deletion vs the FRESH base | `NOT-NOOP merged=…` | pass | 1 | N refuses |
| C5 the same branch vs a **STALE** base | `NOOP-PROVEN` | pass | 2 | **P2** (fresh fetch) is the stopper |
| C6' PR onto `release`, measured against the DEFAULT base | `NOOP-PROVEN` | pass | 2 | **P1** (real base) is the stopper — this one would close live work |
| C6' the same PR against its REAL base | `NOT-NOOP merged=…` | pass | 1 | N refuses once the base is right |
| C7 true textual conflict | `NOT-NOOP merge conflicts` | pass | 1 | a conflicted PR is never a no-op |
| C8 back-merge / history reconcile | `NOOP-PROVEN` | **REFUSE** | 2 | **P ∧ N ∧ V ∧ A all hold** — P6a is the only stopper; the payload is the merge edge |
| C9 empty-commit marker PR | `NOOP-PROVEN` | **REFUSE** | 1 | V passes on one commit — P6b is the only stopper |
| C10 unhydrated branch + one `--no-ff` merge-forward | `NOOP-PROVEN` | **REFUSE** | 1 | **V is not a hydration test**; P6a is what stops this |
| C11 single-commit, rebase-landed | `NOOP-PROVEN` | pass | 1 | **must still close** — pins P6 against over-refusing |
| C12 self-revert | `NOOP-PROVEN` | pass | 2 | accepted close of never-landed work (survivor 1 of 2) |
| C13 the same reconcile as C8, authored on the **release** side | `NOOP-PROVEN` | pass | 2 | **accepted close**: swapping the merge's parents makes it graph-identical to C1, so no predicate separates them (survivor 2 of 2) |
| C15 the C11 shape, `rebase_block` = each non-`already_landed`, non-`empty_candidate` class (`empty_candidate_superseded` and `infra` included) | `NOOP-PROVEN` | pass | 1 | **every proof conjunct passes** — the DETECTOR is the only refusal. Sweeps the enum so a five-member mental model cannot leave `none` / `base_not_default` / `conflicting_head` admitted |
| C16 the C11 shape, `rebase_block` = `empty_candidate` | `NOOP-PROVEN` | pass | 1 | the `qontinui-coord#1664` **safety pin**: coord's own detail says a human must decide. Fails if anyone re-widens the conjunct to the parent `conflict` status |
| C17 landed **all-deletions** branch (the `web#1185` shape) | `NOOP-PROVEN` | pass | 1 | admitted at the detector, then **B refuses** with `paths=1 ghfiles=1 but no compared blob exists in the base` — pins the `base_existing` guard that stops a wrong or stale clone reading "absent everywhere" as "identical everywhere" |
| C18 the `coord#1920` shape — rebase-ff-landed by coord, left OPEN by GitHub, diff frozen non-zero | `NOOP-PROVEN` | pass | 1 | **arm 3 closes this**, and it is THE fixture the arm exists for: `land_stamp=current_head`, `changedFiles=5`, and B reads `NOT-EQUIVALENT paths=5 ghfiles=5 identical=3` because a sibling PR evolved two of the paths afterwards — A'' takes B's rename-aware `ghfiles=` **count** (5 = 5), never its verdict. Fails if the third ladder branch is deleted, or if anyone requires B to exit 0 inside arm 3 |
| C19 the `runner#978` partial ff-land — coord landed an EARLIER head, the author kept pushing | `NOT-NOOP merged=…` | pass | 3 | **L' PASSES** (the stamped `merge_commit` genuinely is an ancestor of the base) and the PR still carries live unlanded work — coord re-scopes the stamp to `superseded_head`, so **L is the refusal** and the DETECTOR routes it to no arm before A'' is ever reached; N refuses independently. Fails for any arm 3 keyed on L' alone, or on the mere presence of `merged_at` / `merge_commit` |
| C20 the C18 shape as served by an OLDER coord with no `land_stamp` field at all | `NOOP-PROVEN` | pass | 1 | the direct analogue of C15: an **absent field is UNKNOWN → Tier 2**, never arm 3. Fails if L is ever written as the denylist `land_stamp != "superseded_head"`, which an absent field satisfies |
| C21 the C18 shape, `land_stamp` = each of the four non-`current_head` members of `LandStampScope` (`none`, `terminal`, `terminal_uncorroborated`, `superseded_head`) | `NOOP-PROVEN` | pass | 1 | **every proof conjunct passes** — the DETECTOR is the only refusal, the arm-3 counterpart of C15. Sweeps the enum so a five-member mental model cannot leave `terminal` / `terminal_uncorroborated` accidentally admitted |
| C22 the C11 shape carrying BOTH signals — `rebase_block=already_landed` AND `land_stamp=current_head` | `NOOP-PROVEN` | pass | 1 | the **precedence** pin: routes to **arm 2**, where B reads `EQUIVALENT paths=1 ghfiles=1 identical=1 base_existing=1` and the close needs B's *verdict* — arm 2 is evaluated first, so the stronger proof owns the overlap. Fails if the arm-3 branch is inserted above arm 2's, which would silently downgrade arm 2's population to the weaker proof |
| C23a the C18 shape, `merge_commit` = the fork point (exists AND is an ancestor of the base) | `NOOP-PROVEN` | pass | 1 | the **accept** control of the L' anti-vacuity bracket: L' reads `0` and the case closes under arm 3. L' is the one arm-3 conjunct the row keeps as prose, so this pair is its only executable pin — fails if L' is stubbed `exit 1`, which would make arm 3 inert while every other assertion still passed |
| C23b the C18 shape, `merge_commit` = a commit that EXISTS in the clone but is NOT on the base | `NOOP-PROVEN` | pass | 1 | the **refuse** control: L' reads `1` — a *proven* refusal, never an UNKNOWN — and the case must NOT close even though the detector admits it and A'' agrees; **L' is the only stopper**. Fails if L' is stubbed `exit 0`. Pinned here rather than on C19 because L' *passes* on the runner#978 shape, so C19 cannot observe the refusing direction |
| C24 the C11 shape, `block_reason_code` = `already-landed-by-content` with `rebase_block=none` AND `land_stamp=none` | `NOOP-PROVEN` | pass | 1 | the **member (ii) admission** pin: the second member of arm 2's allowlist routes this to **arm 2 on its own**, with neither of the two fields the ladder used to read. Member (ii) is Guard 3 of the merge predicate, not the rebase classifier, so it does **not** come with `rebase_landable: false` — requiring it would make the member inert. Fails against the pre-2026-09-07 detector, which routes this to no arm |
| C25 the C18 arm-3 shape carrying `land_stamp=current_head` AND `block_reason_code=already-landed-at-head` | `NOOP-PROVEN` | pass | 1 | the **mis-widening refusal**, and the shape of the arm-3 cards measured WITH that field — `portofino-pizzeria/mobile#4` (2026-09-05) and `qontinui-dev-notes#428` (2026-09-06). ⚠️ **Two of the three, not all three: `coord#1920`'s card is quoted above with `land_stamp`, `rebase_block` and `rebase_landable` and NO `block_reason_code` at all**, having been measured 2026-09-04 before the field was read on this population — C18 carries `MT_BRCODE=none` for exactly that reason, which is an observation date and not a claim about the field's value here. This row already records one `block_reason_code` generalisation being falsified, so the narrower statement is the one that keeps. `already-landed-at-head` is Guard 2 — the same fact `land_stamp` reports — so it must route to **arm 3**; **fails the moment anyone adds that code to arm 2's allowlist**, which would pull arm 3's whole population onto a proof requiring B's verdict that `coord#1920` was measured to fail (`NOT-EQUIVALENT paths=5 ghfiles=5 identical=3`) |
| C26 the C11 shape, `block_reason_code` = each of `empty-diff-already-landed`, `already-landed-at-head`, `ci-not-green`, `main-red`, `none` and the EMPTY value, with `rebase_block=none` and `land_stamp=none` | `NOOP-PROVEN` | pass | 1 | the **EQUALITY sweep** — every proof conjunct passes on all six (B reads `EQUIVALENT paths=1 ghfiles=1 identical=1`), so the DETECTOR is provably the only refusal, the C15/C21 construction applied to member (ii). `empty-diff-already-landed` is the load-bearing member: it pins that arm 2 keys on **one named code**, not on "any already-landed code", so a prefix/substring spelling of member (ii) fails here. `already-landed-at-head` appears with `land_stamp=none` — the older-coord shape — and must still be arm 0; the EMPTY value is a card carrying no such field at all and satisfies nothing |
| C27 landed as a two-commit rebase train stamped at its TIP, then a LATER commit on `main` rewrites the very line the PR introduced (the `portofino-pizzeria/mobile#11` / `#13` shape) | `NOT-NOOP merge conflicts` | pass | 1 | **arm 3 closes this through M**, and it is THE fixture M exists for. N refuses because `main` rewrote the PR's line, and B against the base reads `NOT-EQUIVALENT paths=2 ghfiles=2 identical=1`. B at the stamped `merge_commit` (the train's tip) reads `EQUIVALENT paths=2 ghfiles=2 identical=2 base_existing=2`, so (N ∨ M) reads `0` and A''' agrees (2 = 2). Fails if M is dropped from the OR, or if M is run against the base instead of the land commit |
| C28 the C27 shape, `merge_commit` = the FIRST train commit (a non-tip, mis-recorded land) | `NOT-NOOP merge conflicts` | pass | 1 | the **refuse** control for M. M reads `NOT-EQUIVALENT paths=2 ghfiles=2 identical=1 first-differing=k2.txt`, so (N ∨ M) reads `1` and the PR must NOT close, even though the detector admits it and L' and A'' both pass. Fails if the OR is stubbed to `0` |
| C29 the C27 shape, `merge_commit` = a well-formed sha ABSENT from the clone | `NOT-NOOP merge conflicts` | pass | 1 | the **UNKNOWN** control. L' reads `2` and M reads `UNKNOWN base object absent` (rc `2`), so (N ∨ M) must read `2`, not `1`: a `2` never collapses into `1`. Fails if the OR's UNKNOWN arm is dropped |
| C30 the head's content reached `main` only through a side commit whose merge DISCARDED it (`-s ours`), and coord stamped that side commit | `NOT-NOOP merged=…` | pass | 1 | the **first-parent** pin. L' passes because the side commit IS an ancestor of the base. B at the stamp reads `EQUIVALENT paths=1 ghfiles=1 identical=1 base_existing=1`, but the stamp is off the base's first-parent chain, so M reads `1`, and so does (N ∨ M). `main`'s tree never carried the content. Fails if the first-parent check is dropped from M |
| C31 the head merged `main` forward through a commit the stamp does not contain, so the stamp and the base see different fork points | `NOT-NOOP merge conflicts` | pass | 2 | the **A'''** pin. B's `ghfiles=1` equals `changedFiles=1`, and M reads `EQUIVALENT paths=2 ghfiles=2`, so (N ∨ M) reads `0`. M's count disagrees with `changedFiles`, though, so the PR must NOT close. Fails if A''' is dropped from the computed close |
| C32 the ccfg#943 vet's counter-example: the head runs `chmod +x run.sh` and edits `g.txt`, the land keeps `run.sh` at `100644`, and `main` later rewrites the `g.txt` line | `NOT-NOOP merge conflicts` | pass | 1 | the **mode** pin. M at the lossy land reads `NOT-EQUIVALENT paths=2 ghfiles=2 identical=1 first-differing=run.sh`, so (N ∨ M) reads `1` and the PR must NOT close, although first-parent, L', P6 and A''' all pass. Fails if B compares blob ids without the mode, which is exactly what closed it in the vet |
| C33 the same head landed as a train, `g.txt` first and then ONLY the mode, with coord's stamp on the non-tip commit | `NOT-NOOP merge conflicts` | pass | 1 | the **mode-only non-tip** pin. The stamp's content matches, and only `run.sh`'s mode differs, so M reads `NOT-EQUIVALENT paths=2 ghfiles=2 identical=1 first-differing=run.sh` and refuses. A blob-only M admitted it. Fails for the same mutation as C32 |
| C34 the C33 train, stamped at its tip | `NOT-NOOP merge conflicts` | pass | 1 | the **accept** control for C32/C33. M reads `EQUIVALENT paths=2 ghfiles=2 identical=2 base_existing=2` (mode and content both landed), so arm 3 closes through M. Fails if mode-awareness over-refuses a correct land |
| C35 the 2026-09-19 hot-file shape: `main` edited another line of one of the PR's paths between the fork point and the land, coord rebased the head onto it, and a LATER commit rewrote the PR's line | `NOT-NOOP merge conflicts` | pass | 1 | **arm 3 closes this through Mn**, and it is THE fixture Mn exists for. Mb reads `NOT-EQUIVALENT paths=2 ghfiles=2 identical=1 first-differing=hot.txt` because the land's blob carries `main`'s edit, and N conflicts, but Mn reads `NOOP-PROVEN`. Fails if Mn is dropped from M's content half, or run against the base instead of the stamp |
| C36 the C35 land with one of the PR's hunks DROPPED (a conflict resolved against the PR) | `NOT-NOOP merge conflicts` | pass | 1 | the **refuse** control for Mn: the merge re-applies the dropped hunk, so Mn reads `NOT-NOOP merged=…` and the PR must NOT close although detector, L', A'' and first-parent all pass. Fails if Mn is stubbed to `0` |
| C37 the `runner#1555` shape, minimised to 14 lines: the PR MOVES a block, `main` edits a nearby body before the land | `NOT-NOOP merge conflicts` | pass | 1 | the **diff-algorithm** pin. The suite first asserts that a per-path `git merge-file` under the default myers diff CONFLICTS here, so the fixture really is a misread shape; Mn (merge-ort, histogram) reads `NOOP-PROVEN` and arm 3 closes |
| C38 an all-deletions PR, landed, whose N has decayed (`main` later re-adds the file) | `NOT-NOOP merge conflicts` | pass | 1 | the **Mn guard** pin. Mn reads `NOOP-PROVEN`, but B at the stamp prints `paths=1 ghfiles=1 but no compared blob exists in the base`, which is M's refusal (c), so the PR must NOT close. Fails if the guard is dropped |
| P9 the head is an ANCESTOR of the stamp: `main` merged it with `-s ours`, discarding it, and coord stamped that merge | — | — | — | the **second Mn-guard** pin. Mn reads `NOOP-PROVEN` and first-parent reads `0`, but B at the stamp reads `NOT-ARM2 empty path set`, so the guard voids Mn. Pinned as a predicate check rather than an arm-3 case because A'' also refuses the shape, which the suite treats as a hard failure. Fails if the guard's `NOT-ARM2` arm is dropped |
| P10 B at the stamp reads `UNKNOWN` (ls-tree fails under `GIT_ICASE_PATHSPECS`) on the C38 shape while Mn reads `NOOP-PROVEN` | — | — | — | the **UNKNOWN guard** pin: the guard reads `2`, so whether Mn is voided stays UNKNOWN rather than Mn's `0` carrying the content half. Fails if the guard's `UNKNOWN` arm is dropped |
| gate0 drift | — | — | — | each of the three inlined snippets (`merge-noop-probe`, `merge-payload-guard`, `merge-blob-equivalence`) is byte-identical to its script |
| gate1 dollar-digit | — | — | — | `0` in this file — **prose included**, which the CI linter's fence-scoped guard #18 does not cover |
| gate2 gh fields | — | — | — | every `gh pr view --json` field this row names actually exists — this gate exists because `baseRepository` **does not**, and it aborted P1 on every PR |
| gate3 EMPTYCAND spec | — | — | — | the `EMPTYCAND` ledger section exists and names every required token — the four counters (`unproven` above all, which has **no live exemplar**: `content-in-main` was 14 of 14 at authoring, so this gate is the only thing pinning it), the DETECTOR-FAULT rule, and `clearance_audience` / `"operator"`. Without that last one coord's `default_authority` resolves the gate to `AgentAny` — *the registrant included* — and the steward could clear its own human-decision gate |
| gate3b no-close + handoff | — | — | — | the row states that **nothing in this line closes a PR** (the prose counterpart of C16's detector pin), and the structured exit handoff actually carries the `EMPTYCAND` line — a per-iteration line dropped from the handoff is invisible to the next steward |
| P0 unset `MT_BASE`/`MT_HEAD` | `UNKNOWN` rc=2 | `UNKNOWN` rc=2 | — | bare `set -u` exits **1**, which this contract reads as a *proven* verdict |
| P7 shallow clone | — | — | — | M's first-parent check reads `2` (UNKNOWN), never `1`, on a repository `git rev-parse --is-shallow-repository` reports as shallow, and reads `0` on the full copy of the same repository. A truncated first-parent chain can omit a stamp that really is on it, and reading `1` there would log a genuine land as a proven decline |
| P8 B from a subdirectory | — | — | — | B reads `NOT-EQUIVALENT paths=2 ghfiles=2 identical=1 first-differing=f` (rc `1`) both from the repo root AND from `sub/`, in a repository that sets `diff.relative=true`. Without `ls-tree --full-tree`, a run from `sub/` compared the wrong files and proved `EQUIVALENT` (rc `0`), a false close. Without `-c diff.relative=false`, the path set shrank to the subtree |
| P4 draft | — | — | — | the row's P1 field list actually requests `isDraft` |
| P5 six malformed / pre-2.38 git versions | `UNKNOWN` rc=2 | — | — | UNKNOWN never collapses into a proven verdict |

**Most of the git-shape fixtures read `NOOP-PROVEN` — so the no-op proof alone is not the rule, and it is not
close.** Each negative has a *different primary* stopper, and that is the entire argument for
keeping the predicate conjunctive. C5 and C6' would each *also* be refused by **A** (GitHub computes
`changedFiles` against the real, fresh base, so both read `1`), which makes P1 and P2
belt-and-braces there — still required, because they are what makes N's own proof *sound* rather
than accidentally-agreeing. The genuinely non-redundant ones are **C3 → V** and **C8/C9/C10 → P6**,
and those four are exactly the rows a "simplifying" reviewer will delete. ⚠️ Note the fixtures'
`git rev-list --count BASE..HEAD` only **approximates** V: V's real discriminator is
`gh pr view --json commits`, which GitHub does *not* recompute to `0` once a head becomes an
ancestor of its base (measured on six merged web PRs). **Do not let the fixture suite stand in for
a live check of the `commits` field.**

Tier 1 is **deterministic, auditable, fast** — the SRE reflexes, no LLM cost.

**Watch-only repos (Step 0).** Every row above whose remediation ends in "coord lands it"
applies wherever coord is the **LANDER**. On a watch-only repo the steward's remedy stops at
**landable and green** (Step 0), and closing the PR is the other mechanism's job. Still never
`gh pr merge`, and still never `--admin`.

⚠️ **WATCH-ONLY CORRECTION 2026-09-16 — establish watch-only PER REPO before applying any
inversion below.** It is a lander fact, not the complement of a list, and the list this block
was written against was measured WRONG for five repos coord does land (Step 0). **Applying an
inversion to a repo coord lands is worse than not applying it at all** — the Green-but-dirty
one turns into the eager-churn trap that row's own step (1) exists to prevent.

Three rows need their default INVERTED on a genuinely watch-only repo, rather than merely
disapplied:

- **Verified-green stuck.** The detector's `AND a diagnosed coord defect` conjunct applies
  **wherever coord is the LANDER** — by construction it can never hold where coord is not,
  which would leave a watch-only repo with NO row that detects a stuck PR at all. The
  aged-past-threshold half stands alone: non-draft + green + unlanded past a plain wall-clock
  threshold IS the wedge. The remedy is not a recovery-merge (there is no coord defect to
  recover from) but **diagnosing that repo's own land mechanism** — for ccfg, `rerun_failed_jobs`
  on the PR's `lint-frontmatter` run, which is the only thing that re-arms its edge-trigger —
  **gated on the lint NOT already being green at its latest attempt.** When it IS green and the
  PR is still open, the edge already fired and the `auto-merge` run it fired declined; read that
  run's log (or, since plan `2026-09-04-ccfg-auto-merge-cannot-land-a-pr-that-touches-a-workflow-file`,
  the `auto-merge declined:` comment it posts on the PR) and the PR's file list before spending
  an attempt. A PR touching a `.github/workflows/` file that `main` has also changed since the
  merge-base is the `workflows`-refusal class named in the land-mechanism note under Step 0:
  route it to the *Green-but-dirty* row's **Rebase it** — never to `rerun_failed_jobs`, which
  re-arms a deterministic refusal, and never to an operator.
  Handle it HERE; do not let it fall through to Tier 2, which would spend a plan and a
  `/vet-imp` run on something one re-run (or one rebase) fixes.

  ⚠️ **Dropping the coord-defect conjunct makes `green` the only substantive one — so the
  zero-check qualifier is LOAD-BEARING here, more than in the row itself.** Read `green` with
  **both** conjuncts the row above states — ≥ 1 non-skipped check that PASSED **and no
  non-skipped check that has not passed** — not the first alone. Keeping only the first admits a
  head with one pass and one still-running check, which is the shortening this whole propagation
  exists to stop, and on a watch-only repo nothing else is left to catch it.

  ⚠️ **And `rerun_failed_jobs` presupposes a run exists.** On a head with **zero** check runs
  there is nothing to re-run: the call has no target, and an agent that reaches for it here
  gets an error it will read as a tooling fault rather than as the diagnosis. That head is a
  different class with a different remedy — split it first, exactly as the *Conflicting PR gets
  NO new CI* row does: **never-fired** (`ci_check_row_count: 0` **and** `total_count: 0` on the
  FULL 40-char sha) → split on `mergeable`, and **on all THREE of its values, not two** —
  `CONFLICTING` → resolve the conflict, CI follows; `MERGEABLE` → `gh pr close && gh pr reopen`
  to schedule it; `UNKNOWN` → **re-read it** after a short delay and act only on a settled value,
  never as a quiet synonym for `MERGEABLE`. (An `if CONFLICTING … else …` here is the same
  two-armed misroute the row above refutes — the else-arm swallows `UNKNOWN` and spends a
  close/reopen that schedules nothing.) **`no-baseline`** (every workflow path-filtered off this
  head — zero check rows, **or** rows that all concluded `skipped`, which the two-counter test
  does not distinguish; see `/babysit-prs` Step 2's *THREE causes* table) →
  coord's `required-checks-missing` question, not a green-ness one. ccfg is not a hypothetical
  witness for this: its `.github/workflows/auto-merge.yml` is edge-triggered on a
  `lint-frontmatter` `workflow_run` *completing* (the land-mechanism note earlier in this file
  spells out the trigger and why a fresh dispatch cannot substitute), so a ccfg PR that produced
  **no such run at all** is precisely the head where the named remedy has nothing to act on.
  That note already covers a lint run that was **cancelled or failed** — a run that never
  existed is the third case, and it is the one `rerun_failed_jobs` cannot serve.

  ⚠️ **And a THIRD arm that split does not have, on either surface: runs EXIST and none of them
  can ever conclude.** `ci_check_row_count` **> 0** and `total_count` **> 0** on the FULL 40-char
  sha, `mergeable: MERGEABLE`, yet **every** run is non-conclusive — `cancelled`,
  `startup_failure`, or a `completed` run carrying only never-dispatched jobs (the job shape the
  UNDISPATCHED section defines). That is neither **never-fired** (runs exist) nor **`no-baseline`**
  (the workflows did fire), so neither arm of the split above reaches it — and **both of their
  remedies are wrong here**: `rerun_failed_jobs` carries the one-way hazard that section measures,
  and `gh pr close && gh pr reopen` schedules nothing when runs already exist for the head.
  **The remedy is that the head must move** — hand back to the author, or push a fresh commit.
  Full detector, the `{pending_checks, pending_required}` block-reason payload that confirms it
  cheaply since `coord@f3942732`, and the `qontinui-coord#1658` worked example in which exactly
  that remedy cleared a 9.2-day wedge: the *Runs EXIST but none of them can ever conclude* row in
  the wedge-class table above.
- **Green-but-dirty.** Step (1)'s "is it even yours to fix? … LEAVE IT — coord auto-rebases the
  candidate at land" holds **wherever coord lands the repo**. Where it genuinely does not,
  nothing auto-rebases, a merely-behind PR stays behind forever, and LEAVE-IT is the wrong
  default: rebase it. ⚠️ **This is the inversion that costs most when misapplied, so establish
  the lander first.** On the five repos the 2026-09-16 measurement corrected, coord DOES
  auto-rebase, and rebasing there resets a full CI to do coord's job — the eager-churn trap
  step (2) is gated against. The CI-duration gating in step (2) is moot only where there
  genuinely is no candidate CI: read `candidate_ci_p90_secs` rather than assuming it absent
  (ccfg reported **78.8 s** on 2026-09-16).
- **Already-landed empty-diff.** This row **DOES apply**, and on a watch-only repo it is the
  more likely of the two — a stranded PR's content often lands out-of-band via a successor PR
  while the original sits open (ccfg #231, superseded by #255). Run its **P ∧ N ∧ V ∧ A no-op
  proof** against the PR's CURRENT head and its **real base** before touching anything else; a PR
  that needs closing must never be re-run and landed as a no-op. Out-of-band landing is precisely
  the shape ancestry cannot see, so do **not** substitute an ancestry check for the proof.
  ⚠️ **WATCH-ONLY CORRECTION 2026-09-16.** This used to add *"on a watch-only repo there is no
  coord land record to fall back on either"*. **False** — watch-only is a LANDER fact (Step 0),
  and coord ff-lands even here: 13 of ccfg `origin/main`'s newest 100 commits carry
  `committer=qontinui-coord`. Look for the land record rather than assuming its absence; where
  none is found, UNKNOWN is UNKNOWN.

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
   user once where plans live, or DISCOVER one: from the workspace root,
   `ls -d plans */plans 2>/dev/null` and use the directory that actually exists. Never
   fall back to a directory you have not confirmed is there; a named fallback fails
   silently on every machine that does not have it. Never assume an
   absolute path from another machine. Pass the resolved absolute path to step 3, not the
   variable.

   **Check for an existing plan on this defect first** — this step writes with no
   existence check at all, and a steward that runs continuously is the likeliest
   author of a twin. Discovery resolves against the plan corpus (`CLAUDE.md` → "Plan
   corpus authority"): `GET <web-origin>/api/v1/plan-library?kind=plan&work_unit_slug=<stem>`
   for a known stem, else page `?kind=plan&limit=200` and match `slug`/title —
   **never `?q=<stem>`**, which matches title and body but not the slug. A
   zero-result read is **UNKNOWN, not "no such plan"** — the corpus is a partial
   mirror of disk by construction — so when it comes back UNKNOWN, reference the
   uncertainty in the plan body rather than recording the defect as newly discovered.
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
   **⚠️ The recovery arm is agent-unexecutable since PR #328** (shared-settings
   `deny` on `Bash(gh pr merge:*)`, unliftable in every permission mode and by any
   hook). When the train is the defect, the steward's land step is: audit comment →
   gate/escalate to the operator → carry on with the remediation PR, which lands
   normally through the train once the defect is fixed. The fix PR itself is
   unaffected — it is an ordinary PR.

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
`AskUserQuestion` is for the interactive-operator case, and only for these three carve-outs:
(1) a **security anomaly** — an apparent credential leak, auth bypass, injection, or an unexpected
privilege/escape; (2) a **coord / deploy / migrate need** — an action requiring a coord deploy, a web
deploy, a DB migration, or another fleet-mutating step; (3) a **genuinely-surprising finding** — a
result contradicting a core assumption such that continuing autonomously would be reckless.
Everything else is `stop_and_report`: emit the handoff and stop. Do not ask the operator to confirm
routine things, restart services, or look at a log.

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
    ⚠️ **`open_proposals` / `open_proposal_list` ALREADY exclude terminal statuses — do NOT
    re-filter them.** Both query sites bind coord's canonical `TERMINAL_PROPOSAL_STATUSES`
    (`merged`, `cancelled`, `conflict`, `shadow-landed`) into a `status <> ALL(...)`
    predicate (the test `terminal_set_counts_shadow_landed_as_terminal` pins the constant's
    contents, not the binding); coord's
    own module doc (`crates/coord/src/pr_merge/economics.rs`, "Terminal statuses are
    ALREADY excluded — do NOT re-filter") names this guardrail as stale. Subtracting
    terminal rows again double-filters and UNDER-reports — it makes a busy queue look
    drained, which is the exact moment a coord fix must NOT land. Take the count as served.
    *History:* this bullet used to say the opposite — "`open_proposals` OVER-COUNTS …
    ignore `shadow-landed`" — and that was TRUE when written: on 2026-07-23 it included 19
    long-dead `shadow-landed` rows (split recorded then as coord 15, web 5 — sums to
    20, quoted as recorded), because the query then used a private
    status list that omitted `shadow-landed`. It stopped being true when the query sites
    were repointed at the canonical constant (plan
    `2026-08-26-coord-pr-merge-health-ready-unmerged-over-reports`, Fix 2).
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
- **`--max-recovery-merges` (default 1/hour) STANDS — but is INERT since PR #328**, which
  denied `Bash(gh pr merge:*)` fleet-wide in shared settings; a budget on an action no
  agent can perform caps nothing. Keep the flag (it re-arms the moment the deny is
  lifted) and do not read a non-zero remaining budget as permission. A recovery-merge
  bypasses coord, the
  merge authority, on a diagnosed-defect argument — that is a genuinely dangerous, genuinely
  rare act and deserves a hard ceiling. Do not conflate it with authoring fixes.
- **Tighten the REVIEW, not the volume.** If autonomous fix quality drops, raise the vet bar /
  require a second adversarial review pass. Per-PR `/code-review` is the gate that has actually
  caught bad code here; a volume cap never caught any of it.
- **Every change goes through the same worktree-isolated, vetted, CI-gated path a human
  session uses.** NEVER `--no-verify`. NEVER bypass a real (non-defect) gate —
  `escalate-path-matched` and red CI are the system *working* (act on the `pending_gate`
  coord's `escalate` block names — `/babysit-prs` 4d; only its override arms go to the operator);
  only a **diagnosed coord defect** or **coord outage** justifies a recovery-merge, with evidence quoted on the PR
  first (`/babysit-prs` Step 6 preconditions; the audit trail is mandatory per
  recovery-merge).
- **Honest bookkeeping** *(the honest-bookkeeping rule — cited by that name from
  `.claude/commands/cleanup-steward.md` and `.claude/commands/unattended.md`; keep the name if
  you move it, and note it is a BULLET, not a step or a heading, so a reader looking for a
  "step" of that name finds nothing)** **— PR state is DOUBLY unreliable; CONTENT on `origin/main` is the only
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
  ancestor. `/coord/build-info` is operator-Bearer-gated → useless from a device session
  (measured 2026-09-06: `401 missing operator Bearer token`). A wedge you "fixed" keeps
  firing until the fix actually SERVES.

  ⚠️ **`aws` is absent on some fleet members — the declared fallback is
  `GET https://coord.qontinui.io/health` → `.build_sha` / `.built_at`**, unauthenticated
  and CLI-free, read off the same 4–8 `/health` samples Step 0 already takes. **Say which
  of the two reads you used.** It is coord's own self-report answered by whichever replica
  the ALB routes to, so it establishes *which build is answering* and never *that the
  intended task definition rolled*; where the two disagree the ECS read wins. An absent
  `aws` is **INOPERATIVE-ON-THIS-MACHINE, never a skipped item 9**. Full statement, with
  the sampling rule and the mid-flip caveat quoted from that same `deploy-coord.yml` step:
  Step 0 item 4 above.

  ⚠️ **A serving sha that is not advancing is usually the DEBOUNCE, not a wedge — and it
  is BOUNDED.** `deploy-coord.yml` skips a **push** deploy while the last run whose
  `deploy` job itself concluded success finished less than `DEPLOY_MIN_SPACING_HOURS`
  (default 4h, `:217-218`) ago; debounced skips and coalesce bow-outs conclude workflow
  SUCCESS and never reset that clock (`:26-52`). The `schedule` lane
  (`cron '17 */4 * * *'`, `:81-82`) is the catch-up, deploying iff main's HEAD differs
  from the last run that actually rolled. The workflow states its own contract: **a
  landed commit deploys within ~`DEPLOY_MIN_SPACING_HOURS` + one cron interval, ≤ ~8h
  worst case** (`:44-45`). So before reporting a static serving sha as an anomaly: read
  the newest `Deploy coord` run's **JOBS, not its conclusion** — a
  `Deploy SKIPPED (spacing gate — no rollout)` job with `Build, push, and roll coord →
  skipped` is the debounce working, and the answer is the next cron tick, not a
  remediation. Compute and report the predicted release instant. **Only past that bound
  is it a defect.** (Measured 2026-09-04: eleven consecutive steward ticks spent on a
  4-commit gap that was inside the designed bound the whole time, with a scheduled
  recovery already due.)

  ⚠️ **Force a real deploy with `gh workflow run deploy-coord.yml --ref main` ONLY when a
  diagnosed fix must serve NOW *and* the merge train is drained** — then re-verify the
  tag. The `workflow_dispatch` lane is never debounced, and that is exactly the problem:
  the drain step is gated
  `if: ${{ (github.event_name == 'push' || github.event_name == 'schedule') && inputs.force_deploy != true }}`
  (`qontinui-coord` `.github/workflows/deploy-coord.yml` — re-resolve with
  `git grep -n 'inputs.force_deploy != true' origin/main -- .github/workflows/deploy-coord.yml`),
  so **the dispatch lane SKIPS the merge-train drain** — and a
  coord restart orphans in-flight proposals, the top self-harm risk named in Step 0
  item 4. Never force a deploy merely to advance a debounced sha; that is the debounce
  working, and the cron catch-up is its recovery lane.

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
- **`--respond-to-alert <id>`** runs an **alert-bound pass**: coord dispatched this session
  because a specific fault fired, and the alert row IS the brief. Today the one kind bound
  to it is `pr_merge_admission_stalled` — the merge train had dispatchable work for a tenant
  and admitted none of it for two consecutive evaluations (plan
  `2026-09-12-merge-train-alerts-page-a-reader-and-act-on-nothing`, Phase 2) — but the
  entrypoint is kind-generic and a second kind needs no new flag. The contract, in order:

  1. **Read the alert.** `GET /coord/alerts` (`fleet_health::get_alerts`, the device-authed
     HTTP route). **There is no MCP twin for it** — that is stated here rather than
     discovered, so nobody spends a pass probing for `coord_query_alerts` and reading its
     absence as coord being down. Select the row by the id you were handed; its `detail`
     carries `cause`, `remedy`, `dominant_code`, `auto_clearing`, `self_blocking`,
     `dispatchable`, `repos` (the per-repo census), `held_since` and `code_histogram`.
  2. **Re-read the census by repo BEFORE acting.** A phantom `dispatchable` fires this
     invariant exactly as a real one does, and acting on a phantom is worse than waiting:
     `coord_query_train_activity` plus `coord_reevaluate_dry` for the repos the row names.
     If the census no longer holds the work the row claims, the finding is the PHANTOM —
     record it and stop. Do not remediate a stall that is not there.
  3. **Run the Tier-1 reflex the attributed cause names** (Step 2's table plus the
     `remedy` text on the row — the row's remedy is the cause-specific instruction and the
     table is the general one; where they disagree the row is fresher). Tier 1 only.
  4. **Run Tier 2 only for an `unclassified:` cause.** A cause the closed `STALL_CAUSES`
     table does not know is by definition a halt class nobody has written a remedy for, and
     that is exactly what Step 3's root-cause → author → `/vet-imp` pipeline is for. A
     KNOWN cause with a remedy does not get a plan written about it.

     ⚠️ **This step is the EDGE OF THE BLAST RADIUS of the whole autodispatch arm, and it
     is named here rather than left to be discovered.** Coord's own security-surface note
     says truthfully that nothing in the arm binds a merge, a deploy, a migrate or a cap
     change — that is a statement about COORD. The session coord spawns is this one, which
     carries `Read, Write, Edit, Bash, Agent` and drives root-cause → author → `/vet-imp` →
     land with no approval click. Every other clause of this contract narrows that (Tier 1
     only for an attributed cause; act-and-notify when `self_blocking`; never resolve your
     own alert). This clause widens it back to full Tier 2 — **so the widest autonomy the
     arm grants is granted exactly where nobody has characterised the fault.** That is the
     deliberate choice, and it is the right one: an uncharacterised halt class is precisely
     what the author-a-fix pipeline exists for, and the alternative is a stalled train that
     nothing is allowed to work. It is also PRE-EXISTING steward authority — coord's
     dispatch does not mint it. But it is where to look first if this arm ever does
     something surprising, and it is the lever to narrow if it does.
  5. **When `self_blocking` is true, act-and-notify.** The tenant owns
     `qontinui/qontinui-coord`, so a coord fix for this stall cannot land through the
     stalled train. Permitted: mechanical, REVERSIBLE unblocks — `coord_cancel_merge` with
     `unblock: true` on a starved candidate, `coord_reevaluate`, a rebase of a conflicted
     head this session owns — plus a notification. **Never a deploy, never a merge, never a
     fix PR that must land on the stalled train.** (Served policy `escalation-bar`
     `do-reversible-mechanical-work` governs, over
     `planning-and-scope` `finish-to-zero-includes-the-defect-underneath`'s "that deadlock
     is the escalation", per `escalation-bar` `clause-precedence`.)
  6. **Escalate only on the fleet's closed list** (Step 4). A stalled train is not itself
     on it.
  7. **Post a finding** under topic `coord-merge-train` naming the alert id, whatever the
     outcome — including "phantom census, nothing to do". A pass that acted on nothing and
     recorded nothing is indistinguishable from one that never ran.
  8. **`--once` semantics apply**: assess → act → report → exit. An alert-bound pass is a
     single pass by nature; coord dispatches a new one if the alert re-fires.

  **It NEVER resolves the alert.** The invariant resolves its own row when admissions
  resume (`resolve_cleared_scoped_one_key`), and that resolution is this pass's success
  signal — the one signal you did not write yourself. A responder that closed its own alert
  would be marking its own homework.

  **This arm is the one place the no-`hint` carve-out in the lede does NOT apply, and the
  difference is exactly the one `_gate-registration` draws.** The lede's carve-out is for a
  continuation that re-arms a STANDING WATCH starting from scratch; this arm resumes
  PARTICULAR work, so it falls back under the `hint` rule the lede excuses itself from. It
  satisfies that rule without a `hint` brief because the alert id IS the brief: the row it
  points at carries the whole fault description, it is re-read live on every pass, and
  freezing a snapshot of it into argv would be strictly worse than the pointer — the row
  keeps moving while a prompt does not. Coord's dispatcher composes exactly
  `run /merge-train-steward --respond-to-alert <id> --once` and nothing else
  (`next_step::build_admission_stall_prompt`), so if you find yourself wanting more context
  in the prompt, the fix is a richer alert `detail`, never a richer argv.

## Standing duty — "why isn't this landing": per-PR forensics against the twin

**TRIGGER: any non-draft PR that is green or `CLEAN` and still unlanded past `--threshold`,
and every operator question of the form "why isn't X landing".** Tier 1's table answers a
PR whose wedge class is already KNOWN. This duty is for the PR nothing in the table has
named — and it is the default for that PR, not an optional deep-dive. It exists because the
2026-09-23 steward session found five fleet-wide classes this command did not prescribe a
look at, each by asking one PR "why" and then counting how many others shared the answer.

**The protocol — GitHub truth against coord's twin, and name the divergence.** Read the PR
on GitHub (`gh pr view --json state,isDraft,baseRefName,mergeStateStatus,labels,headRefOid`),
its `coord_pr_status` card (`pr_state`, `land_stamp`, `merge_commit`, `rebase_block`,
`block_reason_code`, `last_verified_at`), and whether it is a MEMBER of `GET /pr-merge/prs`
at all. Where two of those disagree, the disagreement is the finding; state it as
*"GitHub says A, coord says B"*, each with the read that produced it. Every case ends in
all three of:

- **(a) the cause**, with evidence — never an inference from one source, and never from a
  block token alone: a red PR's cause is in its failing job's LOG (the Red-on-a-stale-base
  row's warning — `ci-failed-stale-base` was the wrong cause on 6 of 6 coord PRs, 2026-09-23);
- **(b) a Tier-1 remediation executed, with its proof** — or the named conjunct that refused
  it, which is a `declined` line, not a quiet skip;
- **(c) when the class can recur, a Tier-2 plan authored, vetted and implemented via
  `/vet-imp`** (Step 3). **Answering "why" without the structural plan is incomplete** —
  it clears one PR and leaves the generator running [policy: `planning-and-scope`
  `finish-to-zero`].

Then **census the class fleet-wide** before closing the case: one PR with the shape is an
instance, and the count is what sizes the plan.

### Class 1 — twin-hidden landed-open (the `HIDDEN-LANDED` census)

**Measured 2026-09-23, `qontinui-runner#1590`.** Coord landed it 2026-09-19T14:46Z as
`9f798e77`, a rebased fast-forward, and posted *"Landed … closes the PR from a later
sweep"*. Its card read `pr_state: merged`, `land_stamp: terminal`. GitHub still showed it
OPEN four days later — and it was **ABSENT from `/pr-merge/prs`**, so every open-PR reader,
the phantom-open closer included, was blind to it. The census below then found **40** runner
PRs (`#1589`–`#1684`) in the same state: open, labelled `coord:landed`, missing from coord's
list. That plan's diagnosis: coord marks its own land `merged` in the twin, and that drops
the row from the only list its closers read. Structural plan:
`2026-09-23-coord-self-marks-its-own-land-merged-and-hides-a-still-open-pr` (IN PROGRESS).

The census — ONE fleet read of `/pr-merge/prs`, then one `gh` read per watch-set repo (bearer
in a header FILE, never on argv — `security-and-autonomy` credential hygiene). Set `repos` to
the watch set, checked against the repos coord actually has authority over (item 3). The
value in the fence is an EXAMPLE to replace, and the ledger names every watch-set repo — a repo
the loop did not reach is `UNKNOWN`, never silently absent:

```bash
repos="qontinui/qontinui-runner qontinui/qontinui-coord"   # EXAMPLE: replace with the WHOLE watch set
limit=1000
hdr=$(mktemp); coord=$(mktemp); raw=$(mktemp); gh_open=$(mktemp); listed=$(mktemp)
trap 'rm -f "$hdr" "$coord" "$raw" "$gh_open" "$listed"' EXIT
chmod 600 "$hdr"
printf 'Authorization: Bearer %s\n' "$(cat ~/.qontinui/coord-device-jwt)" > "$hdr"
if ! curl -sS -f -m 90 -H @"$hdr" "https://coord.qontinui.io/pr-merge/prs" > "$coord" \
   || ! jq -e 'type=="object" and (.prs|type)=="array"' < "$coord" >/dev/null; then
  echo "HIDDEN-LANDED UNKNOWN coord list unreadable"
else
  for repo in $repos; do
    if ! gh pr list -R "$repo" --state open --limit "$limit" --json number,isDraft > "$raw"; then
      echo "HIDDEN-LANDED UNKNOWN repo=$repo gh read failed"; continue
    fi
    if [ "$(jq 'length' < "$raw")" -ge "$limit" ]; then
      echo "HIDDEN-LANDED UNKNOWN repo=$repo gh list truncated at $limit"; continue
    fi
    jq -r '.[] | select(.isDraft|not) | .number' < "$raw" | sort > "$gh_open"
    jq -r --arg r "$repo" '.prs[] | select(.repo==$r) | .pr_number' < "$coord" | sort > "$listed"
    if [ -s "$gh_open" ] && [ ! -s "$listed" ]; then
      echo "HIDDEN-LANDED UNKNOWN repo=$repo coord lists 0 rows (tenant scope? see Class 4)"; continue
    fi
    echo "repo=$repo gh_open=$(wc -l < "$gh_open") coord_listed=$(wc -l < "$listed") missing=$(comm -23 "$gh_open" "$listed" | wc -l)"
    comm -23 "$gh_open" "$listed" | sed "s|^|  $repo#|"   # open on GitHub, ABSENT from coord's list
  done
fi
```

`/pr-merge/prs` is tenant-scoped (`pr_merge::list_prs`), so a wrong-tenant credential answers
200 with a valid `.prs` array and ZERO rows for the repo — which would print every open PR as
missing. That is what the zero-rows guard turns into UNKNOWN.

A missing row is a CANDIDATE, not a verdict. Read each candidate's `coord_pr_status` card. It
is this class only when the card passes **Lt**: the `land_stamp` field is PRESENT and equals
`"terminal"`, and `merge_commit` is non-null and 40 lowercase hex. `terminal_uncorroborated`
(the branch-name-reuse shape coord's `LandStampScope` doc calls the runner#978 output shape),
an absent field, or any other value is Tier 2. `current_head` is arm 3's own Tier-1 population
and goes through the table and `EMPTYDIFF`, not here. A card with no land at all is a different
gap (coord never ingested the PR) and is also Tier 2.

**Remedy — close with arm 3's proof, stated as outside arm 3's DETECTOR.** The Tier-1
"Already-landed empty-diff PR" row's arm-3 detector admits only `land_stamp ==
"current_head"`; this population serves `"terminal"`, so it sits outside Tier 1 by
construction. The steward still closes it, on arm 3's **proof** with **Lt in place of L**,
run in full: `P ∧ (N ∨ (M ∧ A''')) ∧ V ∧ P6 ∧ Lt ∧ L' ∧ A''`. L' is taken against the card's
`merge_commit` and stays admissible only downstream of Lt, exactly as the row requires it
downstream of L. M is Mb or Mn at that `merge_commit` plus the first-parent check, and it is
what survives when N has decayed, as on `#1590`, where `main` re-vendored the same files
afterwards. N and Mn run against the PR's CURRENT `headRefOid`, which is what makes the proof
head-exact on a PR that is still open and could have moved after the land. The close comment
says, in terms, that **the detector did not admit it and which proof was run instead**, so the
ledger never reads it as a Tier-1 close. **Never delete the branch.** Count these under
`HIDDEN-LANDED closed=`, not under `EMPTYDIFF`'s arms.

⚠️ **Do NOT widen the arm-3 detector equality in the table to admit `terminal`.** That row's
equalities are pinned by a fixture suite for a reason recorded at length in the row itself.
If a change to the table is genuinely warranted, it lands with fixtures, and
`bash scripts/steward-empty-diff-fixtures-test.sh` stays green.

### Class 2 — stranded stack child (the `STACK` census)

**Measured 2026-09-20 → 09-23.** `qontinui-runner#1629` and `#1632` are stacked on `#1625` and
`#1626`. Coord landed those parents 2026-09-20 by rebased fast-forward; the parents stayed
OPEN on GitHub, and the children sat `base-not-default` for 2.5 days. The fleet census on
2026-09-23: **6 of the 10** `base-not-default` PRs had a CLOSED or landed parent
(`qontinui-coord#2141`, `#2150`; `qontinui-runner#1534`, `#1560`, `#1629`, `#1632`); **4** were
healthy stacks. Structural plan:
`2026-09-23-coord-stacked-child-of-a-landed-parent-is-stranded-as-base-not-default`.

**Discriminator:** resolve the parent by the child's base branch —
`gh pr list -R <repo> --head <baseRefName> --state all --json number,state` — then read the
parent's state and its `coord_pr_status` card. Parent OPEN and unlanded → healthy stack,
leave it. Parent CLOSED, MERGED, or coord-landed-but-open → stranded. **Zero matches, or more
than one (a reused branch name), is UNKNOWN** — counted as `unresolved`, never guessed.

**A naive retarget FAILS**, and that is measured, not predicted: the child carries the
parent's PRE-rebase commits, which conflict against the rebased copies already on `main`.
The remedy is two steps, in order: (1) dispose of the parent — already MERGED or CLOSED on
GitHub needs no close; a card whose detector the Tier-1 table admits (`land_stamp:
"current_head"`) goes through that row and `EMPTYDIFF`; only an Lt (`terminal`) parent takes
the Class-1 proof. **Never delete its branch**, because GitHub closes every PR based on a
deleted branch;
(2) route the child around — fresh branch off `main`, cherry-pick ONLY the child's own
commits, `supersedes #N` in the title — per served policy `git-operations`
`abandoned-pr-branch-is-adoptable-by-route-around` when its owner is gone (Class 5); a
live owner is told, not adopted from.

### Class 3 — paired / sibling-order red main

**Before fixing a red main, ask whether the fix is a missing PARTNER PR in another repo.**
Measured: `qontinui-schemas` `schema-drift` went red at `a3711db3` because `schemas#180`
landed before its runner half `qontinui-runner#1685` — the workflow resolves the runner
sibling from runner `main` on push. A schemas-side revert is the WRONG remedy; get the
partner PR through the train (unblock or prioritise it — never `gh pr merge`, served policy
`git-operations` `merge-authority`), then re-run per the red-main remedies table. Same shape, standing: ccfg's
`fleet-command bundle parity` goes red on every command change that needs its runner carry
(fix in flight: `qontinui-claude-config#1110`, pending-carry). And note the alert's clearing
rule: coord's `red_main` alert on ccfg clears only on a **push** run, and auto-merge squashes
produce none (plan `2026-09-04-auto-merge-lands-trigger-no-main-ci`) — a green SCHEDULED run
does not clear it, so a still-raised alert over a green schedule is that, not a new red.

### Class 4 — a tool's false "none"

**When a tool says "none" about a condition you can see is live, verify the credential's
TENANT before believing it.** Measured: `/handoff-stuck-pr` reported `alert=none consults=0`
for 12 PRs because it minted a device JWT for tenant `meryts-2-0` instead of `qontinui`; the
fixer lane had in fact engaged and capped out. Fixed in `qontinui-claude-config#1104`. Decode
the JWT payload's tenant claim (base64, no secret needed) and compare it with the repo
owner's tenant before recording any "none" from a tenant-scoped read [policy:
`verification-and-evidence` `silent-empty-is-unknown`].

### Class 5 — orphaned PR, gone owner

**Measure owner liveness; never assume it.** Four reads, each named in the ledger: the
`ListAgents` roster (by the commit's `Session-Name`); the transcript mtime at
`~/.claude*/projects/*/<session-id>.jsonl`; the commit's `Session-Id` trailer (which session
to look for); and the branch's last push time. A live owner gets `/handoff-stuck-pr`; a gone
one gets **route-around** per `git-operations` `abandoned-pr-branch-is-adoptable-by-route-around`
— fresh branch, cherry-pick, a `supersedes #N` title, disclosure in the body, the original
left untouched — and the gates watching the superseded PR re-anchored per the served
policy `git-operations` `an-adoption-must-re-anchor-the-gates-watching-what-it-supersedes`
(read it via `/policy` beside the route-around clause, not from this line). Measured instances:
`qontinui-coord#2382` (detector-7 cap fix, red on one manifest line, author gone) and
`qontinui-coord#2396` (author idle 9h, blocking `qontinui-claude-config#1102`).

### Worktree-cap discipline while doing this work

Observed on 2026-09-23 through the budget door below: this device sat at **17/16**, then
**20/16**, of live work for 15h+ while other sessions kept allocating, and every refused `allocate-worktree.sh` call mints a stray coord row
(dossier `1a3b1250`). The count is not a wall for everyone: coord's worktree cap —
`policies::isolation` rule 3a, `twin.active_worktree_count < max_worktrees()` — gates ONLY
**no-build** allocations. An allocation that declares a build is admitted through the
build-slot rules (`COORD_MAX_CONCURRENT_BUILDS` plus the disk reserve) and is NOT refused by the
worktree count. Reading the cap as universal kept the steward's own compiling agents waiting
on a count that never gated them, for hours that same day. So:

1. **Work that compiles declares it.** Coord or runner work that runs `cargo-verify.sh` or
   `cargo-guard.sh` allocates with `--build-required`, on the `shared` target — the honest
   declaration, per `scripts/allocate-worktree.sh`'s own header ("THE BUILD DECLARATION"),
   which says to leave `--build-target` off because coord's default is already `shared`.
   Declaring no-build for compiling work is the unenforced-declaration exploit coord finding
   `17a091e6` records — and it is NOT reliably caught: the script sends no declared paths, so
   coord only derives `needs_build` when the footprint it infers from `--intent` happens to
   name a `.rs` / `Cargo.toml` / `src-tauri` / `target/` path.
2. **Only genuinely no-build work waits on the worktree cap** — docs, plans, ccfg markdown.
   For that, **poll**
   `GET https://coord.qontinui.io/coord/agent-worktrees/allocation-budget/<device_id>` instead
   of looping on allocate, or use the plumbing path below.
3. **Read a refusal's `blocking=` field before choosing how to wait.** The script prints
   `reason=… blocking=… retry_when=…` on exit 3; `worktree_slot` is the count above, while
   `build_slot`, `disk` and `disk_unknown` are different budgets with different waits, and
   `main_merge`, `alembic_revision` and `phase` are LOCKS — retry, never reclaim. The script
   names the remediation for each (its `memory` arm waits on a coord `BlockingResource`
   variant that does not exist yet).

**Reuse** the steward's own worktrees for sequential fixes and hand each back (`--done`) as its
PR lands. The plumbing path (separate index, pinned `BASE` — see the workspace CLAUDE.md "Plan
corpus authority") is allowed ONLY for ccfg/docs changes or for clean cherry-picks where CI is
the verifier — and the PR body says so. The plumbing commit is pushed to a NEW PR BRANCH only,
never to `main` of a coord-merged repo: that recipe's final `git push … :refs/heads/main`
belongs to `qontinui-dev-notes` plans alone, and anywhere else it bypasses served policy
`git-operations` `merge-authority`.

### An operator "why" is forensics PLUS a plan

When the operator asks "why isn't X landing", the deliverable is four things: the cause with
evidence; the Tier-1 remediation executed; a fleet census of the same class; and, when the
class is structural, a plan authored → vetted → implemented via `/vet-imp`. The answer itself
is a diagnostic artifact — write it per "Answering an operator question" below; this duty
adds the census and the plan that section does not require.

## Answering an operator question — the answer is a diagnostic artifact

**TRIGGER: an operator asks this steward a question about fleet or product state
whose answer required ≥2 measurements.** That answer is not a finding, not a
plan, not a memory and not a policy document. Until 2026-09-06 it had no home and
went into the chat transcript, where the next session re-derived it from scratch.
It has one now: `agent.work_artifacts` with `kind = diagnostic`. **Write it.**

Full body contract, write door, bar, slug convention and the two rules that make
the store worth trusting: `knowledge-base/qontinui-specific/diagnostic-artifacts.md`.
The short form:

1. **Seven sections, in order** — Question (the operator's words, verbatim),
   Measured (every claim with the TOOL that produced it and a UTC stamp; a claim
   with no probe named is prefixed `ASSERTED:` and is not load-bearing), Re-run
   (the commands that re-derive Measured — this is what turns the artifact from
   history into an instrument), Mechanism (cited by SYMBOL, never by line number),
   Refutes, Recommendation (ordered, each step with its OWNER and its OBSERVABLE),
   UNKNOWN (never omitted).
2. **The bar, all three or write nothing:** someone actually asked; ≥1 measurement
   with a named probe; the conclusion would be non-obvious to a competent session
   starting fresh. Restating what the code plainly says is noise, and noise
   dilutes exactly the corpus a future session is meant to trust.
3. **The write door.** Prefer `POST http://127.0.0.1:9876/plan-library/artifacts`;
   where that answers 502 (the runner forwards to a web base this box does not
   run) the live door is `POST https://api.qontinui.io/api/v1/plan-library` with a
   device JWT carrying `user_id`. **Verify by read, never by the 201.** Say which
   door you wrote through.
4. **Slug** `probe-<topic>-<YYYY-MM-DD>-<short>`, `source_repo`
   `qontinui-dev-notes/diagnostics`, `kind_is_heuristic: false`, and `intent_refs`
   citing the served `success_metric/` or `domain_spec/` the answer bears on — so
   its importance is INHERITED from a document the operator authored rather than
   asserted by this steward.

### A corrected claim gets a RETRACTION, not an in-place ⚠️ block

This is the rule that lets this file shrink. When this steward corrects a
belief — its own ledger line, a doc comment it quoted, a `domain_spec` line — the
EVIDENCE goes into a `retraction-<topic>-<YYYY-MM-DD>-<short>` diagnostic in the
same seven-section shape, with Question = *"what did we believe, and what
falsified it?"*. Where the falsified claim is itself a plan-library artifact,
also wire `POST …/plan-library/<retraction-id>/edges` with
`{"relation": "refutes", "to_id": "<the falsified artifact>"}`. `supersedes` is
close and WRONG — it means a newer version of the same thing, and using it makes
a refutation indistinguishable from a revision. Where the falsified claim is a
command file, a code comment or a transcript, the Refutes section carries the
pointer in prose and the edge is **omitted rather than faked**.

An in-place ⚠️ block here is still correct where this command's own BEHAVIOUR
must change. What must stop is storing the evidence here: this file is injected
into every steward session, so each correction is paid for in context on every
tick, forever, and is unreadable to anyone not running the command. Worked
instance, with no edge because its target was a transcript:
`retraction-pr-merge-2026-09-06-rebase-ci-failed-is-not-terminal` — the it=179
ledger line *"coord holds `rebase_block: rebase_ci_failed`, so it will not
land"*, falsified by coord re-cutting a candidate 16m48s later with no human
action.

## Report (each iteration, and on exit)

Per iteration, emit a compact ledger: for each PR/signal touched — `repo#pr | class | next-action
| action-taken | outcome` — plus the counters (`recovery_merges 0/1` — a real cap; and
`fix_prs authored: N` per repo — a TALLY, not a ceiling) and any registered
`gate_id`s. Also list **deficiencies found → what you did about them** (fixed / PR # / plan +
gate_id / unchased-with-reason) — a found deficiency with no disposition is a silent drop.

### The empty-diff ledger line — MANDATORY, and ALL THREE arms, every iteration

The Tier-1 "Already-landed empty-diff PR" row has always asked to be ledgered. That instruction
had **no executable backing and no fixed shape**, so it was satisfied by silence — and silence is
precisely how the row's arm-2 population stayed invisible for weeks while arm 1 was hardened
twice. A Tier-1 row that *refuses* produces no wedge signal of its own; the ledger line IS its
only signal. Emit it every iteration, even when every number is zero:

```
EMPTYDIFF it=<N> <HH:MM:SSZ>  arm1-seen=<n> arm2-seen=<n> arm3-seen=<n> closed=<n> declined=<n>
  declined: <repo#pr conjunct=<P1|P2|P3|P4|P5|P6|N|V|A|B|A'|L|L'|A''|M|A'''> verdict=<the tool's own line>
```

A decline on `M` quotes BOTH engines' lines, Mb's then Mn's, separated by ` | `, plus `fp=<0|1|2>` when the first-parent check is what refused.

Rules that make it a signal rather than a formality:

- **`arm1-seen`, `arm2-seen` and `arm3-seen` are counted SEPARATELY and none may be omitted.**
  They are three disjoint populations selected by three different detectors, so one number cannot
  stand for another, and a single merged count is exactly what hid arm 2 — then hid arm 3 one arm
  further over.
- **Zero arm-2 candidates in a repo where coord is emitting `[already-landed]` verdicts is a
  DETECTOR FAULT, not a clean repo — say so in the ledger and route it to Tier 2.** An undetected
  population cannot be declined-with-reason; it produces no line at all, which reads identical to
  a healthy repo. This is the `silent-empty-is-unknown` class applied to a reflex.
- **Arm 3's analogue of that rule, and it is not optional either: zero `arm3-seen` in a repo where
  ANY open PR's `coord_pr_status` card serves `land_stamp: "current_head"` is a DETECTOR FAULT** —
  say so in the ledger and route it to Tier 2. A second, **coord-independent** statement of the
  same observable, for the repos that have no card to read: `origin/main` carrying commits whose
  committer is `qontinui-coord` with **no `(#N)` suffix** while PRs sit open is the git-side
  signature of this land shape. ⚠️ **WATCH-ONLY CORRECTION 2026-09-16:** this used to call it
  "what a **watch-only** repo has *instead of* a coord card". It is an **additional**,
  coord-independent signal, not a substitute — a watch-only repo still has a coord card
  (Step 0), and this signature is worth reading on every repo.
  Arm 3 was undetected for as long as it was precisely because an undetected population emits no
  line at all — the frequency of this population is still unmeasured, and this counter is what
  will actually answer that question.
- **`declined` needs the REASON, and the reason is the conjunct that refused plus the tool's own
  verdict string** — `NOT-NOOP merged=<oid> base=<oid>`, `NOT-EQUIVALENT paths=… first-differing=…`,
  `NOT-ARM2 empty path set …`, `UNKNOWN …`. A bare count is not a reason, and an `UNKNOWN` decline
  is a Tier-2 route, not a quiet skip.
- **`closed` names the arm** (`closed=3 (arm1=1 arm2=1 arm3=1)`), because the three arms authorise
  a close on different evidence — arm 1 on A, arm 2 on B's VERDICT plus A', arm 3 on L, L' and A''
  (which takes B's rename-aware `ghfiles=` COUNT and explicitly not its verdict) plus N or M, counted separately as
  `arm3=<n> (N=<n> M=<n>)` — and a reader has to be able to tell
  which proof was actually run.
  **A close reached by the arm-2 → arm-3 FALL-THROUGH counts as `arm3`**, because arm 3's proof
  is the one that was run. It gets no label of its own — inventing a fourth would break the
  "which proof was actually run" property this rule exists for — and `arm2=` counts only the
  closes arm 2's own proof carried.
- Every number comes from a command run in THIS iteration — the same freshness rule the rest of
  the ledger is under.

### The empty-CANDIDATE ledger line — MANDATORY, every iteration, and it closes NOTHING

`EMPTYDIFF` above counts the populations the Tier-1 row **acts on**. This line counts the one it
deliberately **refuses**, and the two must never be merged — see "Why this is its own line" below.

coord terminates a proposal `empty_candidate` with the detail *"Do NOT close this PR on coord's
say-so; a human must decide."* (two `EMPTY_CANDIDATE_MARKER` sites —
`git grep -n 'EMPTY_CANDIDATE_MARKER' origin/main -- crates/coord/src/merge_scheduler.rs`). That
disposition is CORRECT and stays: arm 2's detector allowlist is a pair of EQUALITIES
(`rebase_block == already_landed`; `block_reason_code == already-landed-by-content`),
neither of which an `empty_candidate` card satisfies; fixture C16 pins the refusal, and
widening the allowlist to admit that class was already refuted. The defect is narrower and
entirely on this side: **coord names an actor that nothing notifies.** The class is not on the
Train tab (train activity, not terminated proposals), not in `ready_unmerged` (the proposal is
terminal, not ready), and produces no `EMPTYDIFF` line because every arm's detector refuses it.

**The class has TWO tokens, and this line counts BOTH.** Since plan
`2026-09-05-coord-empty-candidate-unknown-withholds-a-decidable-net-effect` Phase 3, coord splits
the superseded verdict — every commit upstream by patch-id, and landing would only REVERT content
the base added after the merge-base — into its own `rebase_block` / `stranded_prs[].reason` token,
`empty_candidate_superseded` (detail lead `[empty-candidate] Landing this now would REVERT
upstream work`, symbol `EMPTY_CANDIDATE_SUPERSEDED_LEAD`). coord's own disposition there is
*"close it, or rebase onto the base tip and re-open"* — actionable, but still **not** an
auto-close (`is_proven_landed()` is false), so it stays in THIS line's population and out of every
arm's detector. Count a member under either token; report the split
(`superseded=<n>` of `seen`) so the operator can see which members already carry a stated
disposition. Keying `seen` on the literal `empty_candidate` alone would silently drop exactly the
members coord has decided.
Measured 2026-09-06: fourteen open non-draft PRs fleet-wide, eleven on `qontinui-coord` and three
on `qontinui-web`, N proven and P6 in-tree on **14 of 14** — every one with its content already in
`main` — a population that grew across five consecutive iterations and never shrank.

Emit it every iteration, even when every number is zero:

```
EMPTYCAND it=<N> <HH:MM:SSZ>  seen=<n> superseded=<n> content-in-main=<n> unproven=<n> surfaced=<n>
  gates: <repo>=<gate_id|none> …
  <repo#pr> rb=<empty_candidate|empty_candidate_superseded> age=<Nd> cf=<n> commits=<n> N=<verdict> P6=<verdict> gate=<gate_id|none>
```

Rules that make it a signal rather than a formality:

- **`content-in-main` counts a member only where N is `NOOP-PROVEN` AND P6 is `IN-TREE` AND
  V >= 1** — the three conjuncts that are sound for this population. `unproven` counts every other
  outcome, **including every `UNKNOWN`**, which routes to Tier 2 as usual. The two must sum to
  `seen`; a member that is neither is a counting bug, not a third category.
- **B is REPORTED but never COUNTED here, and this is not laziness.** Measured across the eleven
  `qontinui-coord` members, B refused 11 of 11 — including two PRs 8.5 hours old — because `main`
  moves on these PRs' own paths, and on `qontinui-coord` those are the repo's hottest files
  (`mcp/tools.rs` in 3 of 11, `pr_merge/engine.rs`, `fleet_health.rs` in 2, `auth.rs`,
  `prompt_documents.rs`, `ci.yml`, `taskdef.json`). A `NOT-EQUIVALENT` here means *"arm 2 cannot
  prove this one"*, never *"the work did not land"*. Counting it would report every member as
  unproven and rebuild the exact silence this line exists to break.
- **Zero `seen` in a repo where ANY open PR's `coord_pr_status` card serves
  `rebase_block: "empty_candidate"` or `"empty_candidate_superseded"` is a DETECTOR FAULT, not a clean repo — say so in the ledger
  and route it to Tier 2.** This is the `silent-empty-is-unknown` class applied to a reflex, and it
  is the same rule arm 2 and arm 3 each carry, for the same reason: an undetected population emits
  no line at all, which reads identical to a healthy repo. A **coord-independent** statement of the
  same observable, wherever no card can be read: an open non-draft PR whose coord
  proposal comment carries the `[empty-candidate]` marker. (⚠️ **WATCH-ONLY CORRECTION
  2026-09-16:** this used to say "for a watch-only repo with no card to read", which reads as
  though watch-only implies no card. It does not — Step 0.)
- **`surfaced` counts members named in a registered gate whose `gate_id` was RETURNED** — never a
  gate you believe you registered. A registration with no returned id did not happen
  [policy: gate-read-back].
- Every number comes from a command run in THIS iteration — the same freshness rule the rest of
  the ledger is under.

**NOTHING IN THIS LINE CLOSES A PR.** The whole point of the class is that the disposition belongs
to a human. Do not close, do not comment a recommendation to close, and do not widen any arm's
detector to admit `empty_candidate` or `empty_candidate_superseded`.

#### Surfacing it — one `operator_approval` gate per repo, refreshed not duplicated

For each repo holding members, register ONE gate whose prompt names every member with its N and P6
verdicts, its age and its `rb=` token — and, for an `empty_candidate_superseded` member, coord's own
stated disposition (*close it, or rebase onto the base tip and re-open*), so the operator deciding
it sees what coord has already concluded rather than re-deriving it. One gate per repo, not one per PR: fourteen individually clearable gates is
board noise for what is one recurring judgement per repo. **Not one gate fleet-wide either** — a
fleet-wide gate can only clear when the population is empty in EVERY repo, so it never clears while
any repo holds a member, and a gate that cannot clear is a known failure shape on this fleet. Per
repo, it clears as that repo is dispositioned, and the count scales as O(repos).

On a later iteration, **update the existing gate rather than registering a second** — a duplicate
gate per iteration would be worse than the silence it replaces. The `gates:` sub-line is what makes
that property readable: the same `gate_id` beside a repo across two iterations IS the assertion.

Four registration facts, each of which has a way to fail silently:

- **`clearance_audience` MUST be `"operator"`.** `gates_authority::default_authority` maps the
  audience to the clearance authority, and `GatePredicate::is_human_decision`'s own doc comment
  records that `"agent"` resolves to `ClearanceAuthority::AgentAny` — *"any agent in the tenant,
  **the registrant included**"*. Registered under the agent audience, the steward could clear its
  own human-decision gate, which nullifies the entire remedy and re-creates by another route the
  thing coord's safety pin exists to prevent. **The steward never attests or clears one of these
  gates.**
- **Only two doors accept `operator_approval` from a session credential.** coord 403s
  `operator_approval_requires_operator_auth` on BOTH work-unit / claim-anchored device-JWT HTTP
  doors (`POST /coord/work-units/<slug>/register-gate` and `POST /coord/gates/register-agent`); the
  asymmetry is intentional and fenced by a coord regression test. The doors that DO accept it are
  MCP `coord_register_gate` and the operator/acting `POST /coord/gates/register`. Register through
  **`/gate`**, which runs the whole cascade and returns the id.
- **A returned `gate_id` is not yet a usable gate — read the verdict, and classify it.** The gate
  is REGISTERED-BUT-NOT-USABLE when `initial_verdict_reason` says the predicate cannot be
  evaluated, or when `initial_verdict` is a terminal state it can never clear from (`misconfigured` /
  `failed`); then withdraw it and re-register on a predicate coord can evaluate. A non-empty
  `warnings[]` is not that signal — read the warnings, do not count them. **Omit `gate_class` on
  this gate**: it feeds coord's per-tenant `gate_clearance` matrix, which decides who may later
  clear a gate, and none of the vocabulary classes fits a human PR decision — a class such as
  `routine-review` could let a future tenant rule admit an agent clearer, which is exactly what
  `clearance_audience: "operator"` above exists to prevent. Omitted, it resolves to the default
  operator-only authority. Canonical: `_gate-registration` → "Registration warnings".
- **Read it back, and record the id in FULL.** `coord_gate_inspect(gate_id)`, or by anchor over
  `GET /coord/agent-gates` — `/coord/gates` is the operator tier and 403s a session credential,
  which is evidence about the route and never about the gate.

#### Why this is its own line and not a fourth `EMPTYDIFF` counter

`EMPTYDIFF`'s counters all resolve to ONE disposition: its outcome fields are `closed=` and
`declined=`, i.e. *close the PR*. This population must **never** be closed. Putting a `closed=`
counter beside a population where closing is forbidden merges two opposite dispositions under one
reading — the same conflation that makes a single coord verdict string useless when it covers two
populations that want opposite actions. The separateness rule above already forbids merging three
counters that are merely **disjoint**; these two are **opposed**, so it applies a fortiori.

The neighbouring refused class, `base_not_default`, gets **no counter here** for the same reason
and has its own remedy: it needs a DISCRIMINATOR (is the parent PR still open, or did it already
land?), because a healthy stack and a stranded child produce the identical verdict and want
opposite actions. Do not fold it into this line.

### The CONFLICT ledger line — MANDATORY, every iteration, and it costs no GitHub budget

`conflicting_head` is the **largest single wedge class on the fleet** and has been for six weeks —
86 of 184 open non-draft PRs (47%) on 2026-09-06, 61 of 107 (57%) on 2026-09-12, **81 of 174 (47%)
on 2026-09-17** — and it was declined in one number, every tick, with no per-PR disposition and no
reason. Item **6 (Per-PR twin cards)** above now requires non-`none` `rebase_block`s be split by
`rebase_block_disposition`, which is a real improvement on a different axis and **does nothing for
this class**: `RebaseBlockDisposition::of` maps `ConflictingHead` → `AuthorActs`, so all 81 collapse
into one `author_acts` count routed to prose. This line adds the axis that split does not carry —
**how long the PR has been unable to merge at its current head.**

Run the classifier every iteration, even when every number is zero:

```bash
# ONE fleet read. The credential goes in a header FILE, never on argv
# (`security-and-autonomy` credential hygiene) -- a bearer on a command line is
# readable from the process table by every other session on the box.
hdr=$(mktemp); printf 'Authorization: Bearer %s\n' "$(cat ~/.qontinui/coord-device-jwt)" > "$hdr"; chmod 600 "$hdr"
curl -sS -m 90 -H @"$hdr" "https://coord.qontinui.io/pr-merge/prs" \
  | bash scripts/conflict-triage.sh --it <N> \
      [--window-days 3] [--nudges-dir <dir>] [--handoffs <file>]
rm -f "$hdr"
```

The payload also comes from `--file <path>` (process substitution works), which is what the
fixtures use. **A non-zero exit is not a quiet skip:** `2` is UNKNOWN — no usable payload, or a row
jq could not classify — and it must be ledgered as UNKNOWN, never as `seen=0`.

```
CONFLICT it=<N> <HH:MM:SSZ>  seen=<n> active=<n> stranded=<n> unreadable=<n> handed_off=<n|unknown> landed_open_excluded=<n>
  stranded: <repo#pr conflict_age=<Nd> last_activity=<Nd> nudged=<y|n|unknown>>
  unreadable: <repo#pr conflict_age=UNKNOWN …>
  landed_open: <repo#pr>
```

**Two classes and an UNKNOWN, not three.**

| Class | Predicate | Disposition |
|---|---|---|
| **ACTIVE** | `conflict_age_secs < --window-days` (default **3d**) | **do not touch, and say why.** The conflict is younger than the window; the author may not have seen it. A contact here is the duplicate-nudge failure `pr_merge::stuck_author_nudge` calls *"the WORST failure mode"* |
| **STRANDED** | `conflict_age_secs ≥` that window | **`/handoff-stuck-pr <owner/repo#N>`** — the Green-but-dirty row's own instruction. **Never a steward rebase of a branch it does not own** (`git-operations` `own-artifact-lifecycle`) |
| **`unreadable`** | conflicting now, but the row carried **no `conflict_age_secs`** | its OWN column, UNKNOWN, Tier 2. **Never folded into ACTIVE** — that is the do-nothing class and a failed read would take it silently |

Rules that make it a signal rather than a formality:

- **`landed_open_excluded` is OUTSIDE `seen`, and it is not a conflict count.** It counts the
  rows that pass the conflicting test (`mergeable: false` or `dirty`) but that coord serves as
  `merge_status: landed-open` (`pr_merge::LANDED_OPEN_STATUS`) — coord LANDED them at their
  current head and GitHub still shows them open, with the DIRTY column frozen from the moment
  before the ff-land. Measured 2026-09-19 (operator report): a reported `seen=96` carried **23** of these, so the
  real conflict population was 73. They are the opposite population with the opposite remedy —
  close them through the already-landed empty-diff reflex (arm 3, keyed on `land_stamp`), **never**
  a rebase and **never** a `/handoff-stuck-pr` — which is why the land question stays with that
  reflex and out of the strand clock. The exclusion reads coord's own verdict, exactly as the
  `base-not-default` one does; the INCLUSION test stays `mergeable`/`dirty`. It is printed so a
  drop in `seen` always has a visible cause. Pinned by fixtures **S1d**/**S1e**/**S1f**.
- **Each excluded row is NAMED on a `landed_open:` line, and that line is a free arm-3 cross-check.**
  A count says how many to close, not which. coord derives `landed-open` from the same
  current-head land stamp arm 3 keys on (`/pr-merge/prs` serves the verdict, not the stamp — the
  stamp itself is on the `coord_pr_status` card), so every repo named there has an open PR arm 3
  should see. A repo with a `landed_open:` line and **zero `arm3-seen`** in the same tick is the
  arm-3 DETECTOR FAULT the empty-diff ledger's rules already name: say so and route it to Tier 2,
  with no extra read. These rows are never credited by `--handoffs` — the line carries no
  `conflict_age=`, which is what the matcher keys on, because their remedy is a close, not an
  author. Pinned by fixtures **S1g**/**E14**.
- **`seen` must equal `active + stranded + unreadable`.** The classifier exits `2` when they do not
  close; a member in none of the three is a counting bug, not a fourth category.
- **The discriminator is `PrRow::conflict_age_secs`, NEVER `updatedAt`.** GitHub bumps `updatedAt`
  on comments, labels, reviews and CI — coord's own bot writes included. Measured over the 81 on
  2026-09-17: `updatedAt`-age minus last-commit-age had median **4.31d**, max **27.57d**, and
  **64 of 81** carried an `updatedAt` inside a 3-day window while their last commit was outside it.
  An `updatedAt`-keyed classifier parks 64 stranded PRs in a bucket labelled *"leave it alone,
  someone is on it"* while looking like a careful safety rule. `conflict_age_secs` is head-scoped,
  so it *"resets exactly when the author acts"* and cannot call an actively-pushing branch stranded;
  its no-evidence fallback (`repo_branches.last_refreshed_at`) UNDER-reports, delaying a handoff
  rather than inventing one. Pinned by fixture **K4**, and the mechanism by gate **G3**.
- **Zero GitHub API calls.** One `GET /pr-merge/prs` returns the whole fleet with `mergeable`,
  `merge_state_status`, `conflict_age_secs` and `last_activity_secs`. The rejected alternative —
  one `pulls/<n>/commits` walk per conflicting PR per tick — was 81 metered calls against the same
  account-wide budget as `red_main`, and needed a cap, a memoisation and a skip-count. **There is
  nothing to cap here; do not add one.**
  ⚠️ **Use the FLEET route. Do not reach for the per-repo sibling
  `GET /pr-merge/repo/<owner%2Fname>/prs` as a scoped pass** — measured 2026-09-17 with a device
  JWT, it answered **`404 {"error": "no PRs visible for repo qontinui/qontinui-coord"}`** for a
  repo the fleet route was serving **39** conflicting PRs on at that moment. The route resolved the
  repo and still said "no PRs visible", so its 404 is a scoping artefact, **not** an empty repo, and
  a steward that read it as one would report the fleet's biggest backlog as clean. The classifier
  fails closed on it (`rc=2 UNKNOWN`, its `.prs`-array guard), which is the correct outcome and
  exactly what that guard is for.
- **⚠️ There is NO `REPLAY` class, and adding one is a regression.** `conflicting_head` is raised
  off `live.conflicting_now` (`mergeable IS FALSE OR merge_state_status = 'dirty'`) — GitHub's
  **merge** test failing. The merge-commit replay shape leaves that test **CLEAN by construction**,
  which **Stranded conflicts that never converge** above already measured (2026-09-05, four
  `qontinui-web` PRs): *"`could not apply` + DIRTY ⇒ a genuine content conflict with today's `main`
  … removing the merge commits does not unstick the PR."* A merge-commit count inside this
  population is a coincidence; routing on it sends ~11% of the class to a remedy that cannot help
  it. Pinned by fixture **K9**.
- **⚠️ STRANDED IS NOT PROOF THAT WORK REMAINS, and the ledger must not imply it does.** A **fully
  landed** PR can be carded `conflicting_head` — measured live on `qontinui-coord#1742` and
  `#1810` (2026-09-06): every commit patch-identical to one on `main`, `git rebase origin/main`
  skipping all of them, and coord's own newest proposal reading `[empty-candidate]` while the card
  said `conflicting_head`. **No arm of the already-landed empty-diff reflex sees that shape** —
  arm 1 needs `changedFiles == 0`, which GitHub freezes non-zero on a landed PR forever; arm 2 is
  an equality allowlist on `already_landed` / `already-landed-by-content`; arm 3 is keyed on
  `land_stamp`. Eight such PRs were closed by hand in one day. **This classifier deliberately has
  no already-landed arm** — the land question belongs to that reflex, not to a strand clock — so
  before handing a STRANDED row to its author, check that reflex's arms, and **never assert to an
  author that work remains**. Say *"this has not merged in N days"*, not *"you have work to do"*.
  ⚠️ **That check is per HANDOFF, not per tick.** It runs on the row you are about to hand over —
  bounded by the handoff rate, which the p90 just-in-time rule below already keeps small — **never
  across the whole stranded set**. Reading it as "probe all 61 every iteration" would put an
  unbounded git cost on a ledger line whose whole argument is that it costs nothing.
  ⚠️ This is **not** a claim that landed PRs are piling up here: a per-commit patch-id sweep of all
  59 then-open `qontinui-coord` PRs on 2026-09-06, calibrated against those eight, found **zero**
  had landed. The population at any moment may be entirely legitimate. The honest claim is that the
  blind spot admits **no automatic disposition when the case arises**, which it did eight times in
  one day.
- **If you ever build a land check here, use per-commit `git patch-id --stable`, never one
  aggregate over the whole branch diff** — coord replays commits individually, so the aggregate
  false-negatives on every multi-commit PR. And a percentage-of-lines-present heuristic lies **high**
  in two measured ways: a stacked PR inherits its parent's land (one read 90.1% while its own commit
  had 0 of 13 lines on `main` — measure against the **declared base**), and a merge-resolution commit
  imports `main`'s content as "added" (one read 94.9% while its substantive commit had 17 of 18 new
  symbols absent).
- **⚠️ `handed_off` is PER-RUN, and there is no fleet-durable store behind it — say so rather than
  reading it as a fleet fact.** The `--handoffs` file is the steward's OWN running record: append
  `<owner/repo>#<n>` as each `/handoff-stuck-pr` returns exit 0, and pass it back on the next tick,
  so the actionable set visibly shrinks within a run instead of re-reporting the same 61 forever.
  **On the first tick of a run it reads `unknown`, and that is correct, not a placeholder.** What it
  cannot do is survive the run: `/handoff-stuck-pr` anchors its gate on the AUTHOR'S SESSION claim
  (`claim_terminal`, `claim_kind: session`) and explicitly **never** on a `repo_branch`, so no coord
  read answers *"which PRs have been handed off"* by key — only the gate HINT carries a `PR:` line.
  A later run therefore starts from `unknown` again. That is a known limitation of the gate's
  anchoring, not of this ledger, and it is why the column is `unknown` rather than `0`.
- **`handed_off` is `unknown` when nothing was supplied to compute it from, never `0`** — and
  `nudged` is `unknown` on a missing, unparseable or `{"error": …}` nudge body. `GET
  /pr-merge/<repo>/stuck-nudges` answered **404 `no stuck-nudge ledger`** for both `qontinui-coord`
  and `qontinui-runner` on 2026-09-17, and `pr_merge::stuck_author_nudge` is DARK by default
  (`COORD_PR_STUCK_AUTHOR_NUDGE_ENABLED`) — so coord is **not** currently contacting these authors
  and the two-actors-one-branch collision is **latent, not live**. ⚠️ A 404 there is UNKNOWN about
  whether the sweep is armed, never proof that it is off; the flag is an env var and can be flipped
  without this file changing. Read the ledger before contacting an author anyway — it is one cheap
  call and the failure it prevents is the worst one this population has.
- **A failed read and an empty fleet must never look the same.** The classifier exits `2` on a body
  with no `.prs` array — a 401/404/500 parses as JSON too, and `.prs // []` would render every one
  of them as *"no conflicting PRs"* (`verification-and-evidence` `silent-empty-is-unknown`).
  Pinned by fixtures **E1**/**E3**.
- **Work the STRANDED set in the Green-but-dirty row's order, not top-down**: short-CI repos
  (candidate-CI p90 < ~30m, from `coord_query_merge_economics`) first; long-CI repos only at or near
  the front of the land queue. A handoff mints no new head, so it triggers no CI — but the work it
  hands over does, and that row measured 82% of candidate CI wasted by resolving deep in the queue.
- **Honour `/handoff-stuck-pr`'s typed exits.** `3` no author session, `4` author already closed,
  `5` gate registered but the message unaddressed. A `3` or a `5` is a ledger line and a Tier-2
  route, **not a retry**, and the row stays STRANDED — which is a correct outcome, not a failure.
- **The per-PR waste this measures is unbounded, and that is the better scale argument.**
  `block_reason_repeat_count` on 2026-09-06: `coord#1664` **212** since 09-04, `#1715` **251**,
  `#1599` **179**, `#1763` **168**, `#1795` **166**, `#1716` **156**. Coord re-deciding the same PR
  every few minutes for days with no path to a terminal state. A population count says how many;
  this says how much, per PR, growing without bound until something dispositions them.
- Every number comes from a command run in THIS iteration — the same freshness rule the rest of the
  ledger is under.

**The fixtures — run them, do not read them.** `bash scripts/steward-conflict-triage-fixtures-test.sh`
executes `scripts/conflict-triage.sh` against synthetic `/pr-merge/prs` payloads: no network, and
nothing modelled as an unobservable INPUT, because a payload is exactly what a fixture can be. Its
three gates are drift checks on THIS section — **G1** that the section names the classifier by path,
**G2** that this template carries every column the classifier emits, **G3** that the classifier never
reads `updatedAt`. The load-bearing fixture is **K4**: a row whose `updatedAt` is inside the window
while its `conflict_age_secs` is far outside it, which MUST classify STRANDED. It is the live shape
on 64 of 81 PRs, it is the mistake this plan's own first draft made, and it is the **only** arm that
fails a classifier which returns ACTIVE for everything.

### The HIDDEN-LANDED and STACK ledger lines — MANDATORY, every iteration

The two censuses of checklist item 13. Emit both every iteration, even when every number is
zero, one line per repo or one fleet line with the repos named:

```
HIDDEN-LANDED it=<N> <HH:MM:SSZ>  gh_open=<n> coord_listed=<n> missing=<n> closed=<n> declined=<n>
  declined: <repo#pr conjunct=<P1..P6|N|V|M|A''|A'''|Lt|L'> verdict=<the tool's own line>>
STACK it=<N> <HH:MM:SSZ>  base_not_default=<n> parent_landed=<n> healthy=<n> unresolved=<n>
  stranded: <repo#child parent=repo#pr parent_state=<closed|merged|landed-open> action=<closed-parent|routed-around|handed-off>>
```

- **A census whose read failed prints `UNKNOWN` in place of the number, never `0`** — a 401 on
  `/pr-merge/prs` makes `coord_listed=0` and `missing=gh_open`, which reads as a coord outage
  when it is a credential fault, and the reverse error hides the class entirely.
- **`missing` is candidates, `closed` is proofs.** A `missing` row whose card does not show a
  land is not a `declined` — it is a Tier-2 ingestion gap and is named as one.
- **`parent_landed + healthy + unresolved` must equal `base_not_default`**; each `unresolved`
  child is named individually as UNKNOWN (zero or several parent matches).

On exit (stop / `--once` / cap), emit the structured handoff — assembled **mechanically from the
ledger, not re-derived**, so whoever picks this up sees exactly what was attempted and the one
decision that is blocked:

```
## merge-train-steward escalation — <reason: stop | --once | iteration cap | blocked on an observable condition>

- Iterations run: <N>
- Termination reason: <stop | --once | cap | blocked>
- Registered gates: <gate_id(s), or "none">
- Per-wedge ledger: <repo#pr | class | next-action | action-taken | outcome, one line each>
- Empty-diff ledger: <the EMPTYDIFF line from the final iteration, all three arms>
- Empty-candidate ledger: <the EMPTYCAND line from the final iteration, plus its `gates:` sub-line>
- Hidden-landed / stack ledger: <the HIDDEN-LANDED and STACK lines from the final iteration>
- Tier-3 escalations: <the specific decision needed, WITH your recommendation>
- Deferred fix-lands: <each, with its reason — deploy-batch defer / rate-limit defer>
```

**Emit-on-block.** If the steward stops because it is waiting on an **observable** condition — a
deploy going healthy, a CI run going green, a rate-limit window resetting, a coord leader-lease row
being cleared — invoke `/blocked` to register the typed coord gate **BEFORE** stopping, and name the
`gate_id` above. That turns the blocker into a watched gate instead of a report that dies with the
session. If the blocker has no observable trigger, say so — that case is NOT a gate.

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
  evidence; only a failing one is uninformative. Never read a fail as "unlanded" — and never read
  one as "not a no-op" either. **A FAIL proves nothing in either direction**, which is the whole
  reason the question *"does merging this PR change its base?"* has to be asked directly rather
  than inferred from ancestry: `git merge-tree --write-tree`, not `merge-base --is-ancestor`.
  ⚠️ A pass proves only that *the head is reachable from `main`* — NOT that this PR's
  work landed: an unhydrated or empty branch whose head is an old `main` commit passes
  vacuously. **Ancestry alone never authorises a close.** Since 2026-08-24 the Tier-1 close row
  uses ancestry as *evidence when it passes* and as a gate **never**, and the rationale this
  paragraph used to carry — *"that is precisely why the row keeps guards (b) `changedFiles=0` and
  (c) empty `git log origin/main..<head>`"* — is **retired as measured-wrong on both halves**:
  (c) is *structurally unsatisfiable* on the merge-forward ff-land shape (web#1033: ancestry
  `exit=1`, `origin/main..head` = 6 commits, merge a proven no-op), and (b)+(c) never did stop the
  vacuous case they were credited with — a branch parked at the *current* base tip satisfies both.
  What authorises the close now is `git merge-tree --write-tree` against the PR's **real** base
  (N), fenced by an anti-vacuity commit-count check (V) and by `changedFiles=0` as an independent
  second engine (A). See the Tier-1 "Already-landed empty-diff PR" row and "The no-op probe".
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
  - **Brief every worktree-holding or PR-opening subagent with the checkpoint contract,
    and read its checkpoint on EVERY notification — `failed` AND `completed` — before
    deciding anything.** A provider limit (`429`, "You've reached your Fable limit") destroys
    a subagent's transcript and hands you a `failed` notification with no state; a
    subagent that ended on a progress note hands you a `completed` one that is not a
    report. The checkpoint (`scripts/agent-checkpoint.sh write --label <label>`, rewritten
    after each material step) is the only thing that survives either, and it is what you
    pass back verbatim on re-spawn instead of doing repo archaeology. The label is a
    FILENAME on every platform, so derive it from the wedge or PR fingerprint rather than
    using the fingerprint itself — `qontinui-web#12` is a label, `qontinui/qontinui-web#12`
    is not. When the kill message states a reset time, do not re-spawn into the limit:
    register a coord `time_elapsed` gate for it via `/gate` (converted against the zone the
    message names, never the box's) and `annotate --key resume_gate_id` on the checkpoint so
    a second kill does not double-book; the parser that automates the conversion is the
    plan's Phase 2 and is not landed at the time of writing. Fields and exit codes:
    `knowledge-base/qontinui-specific/agent-checkpoints.md`.
  - **Allocate every worktree through coord — never a raw `git worktree add`.** An
    unregistered worktree has no `coord.agent_worktrees` row, and a cleanup pass REAPED one
    mid-rebase on this fleet, costing ~50 minutes — recovered only because the resolved commits
    still lived in the shared `.git/objects` store; a raw worktree gets that same object-store
    safety net, but registers nowhere, so nothing else protects it and nothing attributes the
    reap to the session that owned it. Allocate with the fleet's one command,
    `bash <workspace-root>/qontinui-claude-config/scripts/allocate-worktree.sh --repo <repo> --intent "<text>"`
    (it POSTs the literal, anonymous `https://coord.qontinui.io/agents/allocate` — no credential,
    no `$COORD_HTTP_URL` — and materialises the result; a raw call sends this):
    ```json
    {"device_id": "<this box's id from ~/.qontinui/machine.json>",
     "repos": [{"repo": "<owner/name>", "worktree_path": "<relative-slug>"}],
     "purpose": "<text>"}
    ```
    Three round-trips through typed 422s were spent recovering this shape: `repos` is an ARRAY
    of `AllocateRepoSpec` STRUCTS — never `repo`, never bare strings; the returned
    `worktree_path` is RELATIVE to the workspace root; and the reserved branch the response also
    carries is for NEW work only — a steward rebasing an existing PR still pushes to that PR's
    OWN branch and wants only the registered path, not the branch. **Standing mitigation:**
    commit the resolution BEFORE any long verification run, so a reap costs a re-run, never the
    work itself. (coord finding `74f887ae-194f-4432-9516-ff1ba5cefbef`.)
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
  Never `--admin`, never `--no-verify`. **And since PR #328 the plain form is off the
  table too — mechanically:** shared `.claude/settings.json` denies
  `Bash(gh pr merge)` / `Bash(gh pr merge:*)`, a rule no permission mode, local
  settings file, CLI flag or `PreToolUse` hook can override. A steward that reaches a
  recovery-merge hands the PR to the operator (audit comment + gate/escalation) and
  moves on; it does not attempt the merge, and it does not reach for
  `gh api .../pulls/N/merge`, which `git-guard.sh` blocks as the same act
  — that hook was briefly unwired fleet-wide (qontinui-claude-config PR #567)
  along with its unrelated destructive git/rm/cargo arms (those stay removed,
  at the repo owner's explicit request), then re-wired the same day narrowed
  to ONLY this merge-route check, so this is mechanically enforced again.
  (The four `main-merge-gates` rulesets DO list
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

## A back-merge on a candidate branch is a diagnosis, not a stall

⚠️ **Pointer only — deliberately NOT an inlined snippet.** This file inlines
three probes byte-identically with `<!-- BEGIN … -->` markers, and each
inlining is gated by its own drift check inside
`scripts/steward-empty-diff-fixtures-test.sh`. A fourth inlined copy would owe
a fourth drift check, which is a real cost with no payoff here — the steward is
not this probe's primary consumer.

coord lands by **rebase**, and a rebase **discards merge commits**. When a
candidate branch carries a back-merge (`git rev-list --merges
origin/main..HEAD` non-empty), any conflict resolution inside it is not in what
lands — either the replay re-conflicts and the PR parks while GitHub still
reports it CLEAN, or it re-merges silently and the pre-merge text ships with no
error at all.

Prove it rather than guessing:
`bash qontinui-claude-config/scripts/rebase-oracle-check.sh` (rc 0
`EQUIVALENT`/`NO-MERGES`, rc 1 `DIVERGENT-CONFLICT`/`DIVERGENT-SILENT`, rc 2
UNKNOWN). Full detail, the two faces and the remedy:
`knowledge-base/qontinui-specific/coord-merge-train.md` → "Why a back-merge is
not a fix". The authoring-time warn arm that fires when such a merge is
CREATED is in `knowledge-base/qontinui-specific/guard-hooks.md`.
