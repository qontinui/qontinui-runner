# Implement Phase

Implement a single phase from an approved plan, then automatically trigger review and next-steps.

**This skill is invoked by `/implement-plan` for each phase. It can also be used standalone.**

## Arguments
- `$ARGUMENTS` - Description of the phase to implement

## Instructions

### Part 0: Publish activity to `coord.device_status`

Before starting implementation, UPSERT a status row so the operator
dashboard shows this session as actively implementing a phase. This is
the read-side of Phase 1.1 + 1.3 of plan
`2026-05-21-coordination-improvements.md`. When invoked from
`/implement-plan`, `/implement-plan`'s Step 0.6.5 already published the
phase row — this UPSERT refreshes it with the more specific
`implement-phase:` shape; when invoked standalone, this is the only
publication.

The UPSERT is keyed on `device_id`, so each call overwrites the prior
row. Skip-and-warn if `device_id` cannot be resolved — status
publication is observability, not gating.

**Resolution chain:**

1. **`device_id`.** Env `QONTINUI_MACHINE_ID` first. Else read
   `~/.qontinui/machine.json` and parse the `"device_id"` field; fall
   back to `"machine_id"` if present (legacy shape). If neither
   source supplies a UUID, emit a single-line warning and skip the
   UPSERT (proceed to Part 1).
2. **`current_repo`.** The MAIN repo's directory name —
   `basename "$(dirname "$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)")"`.
   NOT `basename` of `git rev-parse --show-toplevel`: from a linked git worktree
   that returns the WORKTREE's own directory name (`ccfg-wt-pr161-followup`,
   `qontinui-claude-config-wt-lna`), so the dashboard tile groups this session
   under a repo that does not exist — and sessions run under
   `QONTINUI_AGENT_WORKTREE_MODE=1`, so that is the common path, not an edge case.
   `--git-common-dir` resolves to the main checkout's `.git` from a worktree and
   from the canonical checkout alike, and `--path-format=absolute` (git >= 2.31)
   keeps it from returning a relative `.git`. **If it prints nothing or `.`** —
   not a git tree — omit `current_repo` rather than sending what the expression
   then evaluates to (the parent directory's name, a wrong-but-plausible repo).
   (Inside a git submodule it yields `modules`; no submodules exist here.)
3. **`current_branch`.** `git symbolic-ref --short HEAD`.
4. **`tenant_id`.** Env `QONTINUI_TENANT_ID` if set; otherwise omit
   the field (coord column is nullable).
5. **Coord HTTP base.** Env `COORD_HTTP_URL` first, else
   `https://coord.qontinui.io`.

**`<plan-stem>`** is the parent plan filename without `.md` extension
(if invoked standalone with no plan context, use `standalone` as the
stem).

**`<n>`** is the phase number being implemented (parse from
`$ARGUMENTS` or the invocation context).

```bash
curl -fsS -X POST "$COORD_HTTP_URL/coord/status" \
  -H "Content-Type: application/json" \
  -d "$(cat <<EOF
{
  "device_id": "<device_id>",
  "current_task": "implement-phase: <plan-stem>: phase <n>",
  "current_repo": "<repo basename>",
  "current_branch": "<branch name>",
  "details": {"phase": "<n>"},
  "tenant_id": "<QONTINUI_TENANT_ID, omit field entirely if unset>"
}
EOF
)"
```

**Failure handling.** Any non-2xx response is logged as a single-line
warning (`⚠️ coord status publish failed: <status> <body>`) and the
skill continues. NEVER abort phase implementation on a status
publication error.

When `/implement-plan` is the parent, IT handles the clearing UPSERT
on Step 6 (SHIPPED) — `implement-phase` does NOT clear on completion
because the next phase will overwrite the row immediately, and a
clearing-then-resetting UPSERT pair would flicker the dashboard tile.
When invoked standalone, no clearing is needed — `prune_stale()` TTLs
the row within an hour.

### Part 1: Implement

Implement the described phase fully:
- No stubs, no partial work, no TODOs
- Use subagents for independent work
- Fix issues as you find them
- **Pre-edit peer hotspot check (best-effort).** Before the first edit
  to any non-trivial file, query coord:
  `curl -s "https://coord.qontinui.io/coord/builds/peers?repo=<repo>&since=30m&file=<file>"`
  and surface a warning if a peer's recent build is red on that path
  (`result == "failure" && error_file == file`). Coord-down or no-peer
  cases are no-ops. See plan
  `coord-tinderbox-build-status-2026-05-09.md` for the surface
  rationale.
- **Edit-effect loop (best-effort).** Run coord's predict→gate→verify
  loop around your edits — see the block below.

> **Edit-effect loop — predict, gate, verify.** Wire this phase into
> coord's edit-effect D3 loop (plan
> `2026-06-05-edit-effect-loop-adoption`). Every call is **best-effort**:
> a failed or unreachable coord NEVER blocks the phase — warn once and
> proceed. `COORD_HTTP_URL` overrides the base (default
> `https://coord.qontinui.io`).
>
> **1. Pre-edit (after worktree allocation, before the first `Edit`):**
> call predict-and-check with `{repo: "<repo basename>", paths: <the
> phase's planned touched files>, head_sha: "<git rev-parse HEAD in the
> worktree>", declared_globs: <plan/work-plan globs when present>,
> declared_intent: "<plan-stem>: <phase title>"}`.
> - **MCP (preferred):** `coord_edit_predict_and_check(<same JSON>)`.
> - **HTTP fallback (universal):** `curl -fsS -X POST
>   "$COORD_HTTP_URL/coord/edits/predict-and-check" -H "Content-Type:
>   application/json" -d '<same JSON>'`.
>
> **2. Branch on the response envelope** (`{predicted_effect, resolution,
> risk_factors}`). Read `resolution.action`: anything that is **not**
> explicitly `escalate` (e.g. `proceed`) → continue silently. On
> `escalate` → list the `risk_factors` strings; proceed only when EVERY
> factor is blast-radius-shaped AND the plan's file inventory explicitly
> scopes that many files. Otherwise STOP and report the factors verbatim
> to the coordinator (or, standalone, in your Part 3 report) — the gate
> creates no `agent_questions` row and you never ask the operator
> directly.
>
> **3. Post-edit verify (after the phase commit):** call verify with
> `{repo, paths: <files actually touched>, head_sha: "<the new commit
> sha>", tests_predicted: <the predict response's `detail.affected_tests`,
> when present>}` — MCP `coord_edit_verify(<JSON>)` or HTTP `curl -fsS -X
> POST "$COORD_HTTP_URL/coord/edits/verify" …`. Record the composed
> outcome (`composed_outcome` + the per-subspace summary) in your Part 3
> report. A `Contradiction`/`Failure` composed outcome is a report **red
> flag**, NOT an automatic revert.

#### Coordinator mode (when the phase scope is too large)

If you would otherwise complain that the phase is too large to implement
directly, **do not stop or hand back to the operator**. Pivot into coordinator
mode and ship the phase by orchestrating subagents. The coordinator never
writes feature code itself — its job is to spawn, review, decide, and unblock.

**Responsibilities**

1. **Spawn.** Decompose the phase into independently-buildable chunks. Launch
   an Agent per chunk with a self-contained prompt — chunk description,
   relevant file paths, surrounding context, and explicit instructions to
   implement fully (no stubs / TODOs), run type checks + lints, fix what it
   finds, and report a structured summary (files changed, decisions made,
   issues hit + how resolved, remaining concerns). Launch independent chunks
   in parallel via multiple Agent tool calls in a single message.
2. **Review.** When each agent returns, read its summary critically. Spot-check
   the actual diff with `git diff` / `Read` — don't trust the summary alone
   (see [[feedback_verify_function_exists_before_trusting_stamp]]). Confirm
   the chunk contract is met, no stubs were left behind, types/lints pass, no
   half-finished abstractions, no backward-compat shims, no dead code, no
   feature flags hiding incomplete work.
3. **Decide autonomously.** When an agent surfaces an ambiguity, conflict, or
   judgment call ("two ways to wire this — should I do A or B?"), DO NOT
   bounce it back to the operator. Resolve it using the decision framework
   below and issue the agent (or a follow-up agent) a concrete instruction.
4. **Fix.** If an agent's output is wrong, incomplete, or violates the
   framework, fix it — either by editing directly in the main context (for
   small mechanical issues) or by spawning a follow-up agent with explicit
   instructions on what to change and why. Never accept "good enough" output
   that the framework would reject.
5. **Integrate.** After each wave of parallel agents, do a cross-chunk
   integration pass in the main context: verify imports/exports line up,
   shared types are consistent across boundaries, no two agents introduced
   conflicting abstractions for the same concept. Reconcile divergences via
   direct edit or a targeted follow-up agent.

**Decision framework**

When weighing options as the coordinator — whether resolving an agent's
ambiguity, choosing between two implementation paths, or deciding whether to
accept an agent's output — optimize against these priorities, in order:

1. **Powerful features.** Prefer the option that unlocks more capability or
   composes better with planned future work. A more powerful primitive beats
   a narrower one even if the narrower one is "enough for now."
2. **Scalability.** Prefer the option that holds up as data volume, user
   count, concurrency, or call-site count grows. Reject choices that look
   fine at current scale but have a known cliff.
3. **Robustness.** Prefer the option that fails predictably, surfaces errors
   clearly, and recovers cleanly. Bias toward explicit invariants, structured
   errors, and verification at boundaries.
4. **Clean code.** Prefer the option that future readers will understand
   without archeology — clear names, focused functions, minimal indirection,
   no dead branches, no comments explaining what the code already says.

**Explicitly NOT factors:** programming effort (your time / token budget /
agent count is not a constraint here — ship the right thing), backward
compatibility (per CLAUDE.md, breaking changes are expected; delete-over-
deprecate; refactor fearlessly).

When two options tie on priorities 1–4, pick the one that leaves less
follow-up work. If you genuinely cannot decide after applying the framework,
pick the option you'd defend in a code review and note the trade-off in the
phase commit message — do not stall asking the operator.

**Implementation priorities (execution)**

The engineering priorities above — with the UX gates for user-facing
surfaces (memory: `ux-priorities-alongside-engineering`) — decide **what**
to build. A third orthogonal set, the **implementation priorities** (memory:
`implementation-priorities`), decides **how and when** the coordinator
executes, in order: (1) **verified throughput** — most work built AND
verified this session, verification tiered by consumer (user-facing → goal
observed on the page; consumer-free infra → green CI + documented checks),
majority of work delegated to subagents; (2) **early risk retirement** —
sequence most-falsifiable-first; (3) **autonomy with checks** — merge,
deploy, migration, new security surfaces, scope growth, and spend proceed
when their documented checks pass (no-live-users era); (4) **momentum
through re-planning** — a falsified assumption never halts the session.

**When to escalate anyway**

The coordinator is autonomous but not unconditional. Per the implementation
priorities, exactly two things justify an `AskUserQuestion`:

- **Operator-resource needs** — something only the operator can physically
  do: start the primary runner, unlock a phone, complete an interactive
  login, add a payment method.
- **Oversize-plan handoff** — a re-planned or combined phase too large even
  for coordinator-style orchestration: author the plan, vet it with a
  subagent, then present it for a fresh session.

Everything else resolves in-session: a falsified premise or goal-changing
finding triggers re-evaluation against the priority sets and automatic
selection of the new correct path (incorporate if it fits; new vetted plan
executed coordinator-style if bigger); deploys/migrations/security surfaces
proceed on their documented checks; questions no priority set breaks are
yours to decide (if that recurs, propose expanding the priority sets).
Destructive git and CLAUDE.md's "executing actions with care" list still
get care — prefer the reversible path — but care means checks, not
questions.

Routine implementation choices — library selection, API shape, file layout,
error-handling strategy, test structure — are NOT escalation triggers. Decide
and move on.

### Part 2: Review + Next Steps (AUTOMATIC — DO NOT SKIP)

**After implementation is complete, invoke `/review-plan-next-steps` using the Skill tool. This single invocation handles both the review AND the next-steps analysis.**

```
Skill: review-plan-next-steps
Args: $ARGUMENTS
```

### Part 2.5: Blocked-exit — offer to register a coord gate

*(canonical spec: `_gate-registration` — keep copies in sync)*

If the phase cannot complete because it is **blocked on an observable condition**
(an upstream PR must merge, a deploy/CI must go green, a metric must cross a
threshold, a time window must elapse, or an operator must approve), offer to
register a coord gate before exiting — so coord watches the condition and can
auto-resume the phase after every session closes. A block with no observable
trigger is NOT a gate; skip those and just report the blocker.

- **Default = explicit offer** via `AskUserQuestion` (header `Register gate?`,
  options Register / Skip), showing the derived anchor + predicate + condition.
  Under opt-in auto mode (env `QONTINUI_AUTO_GATE=1`) register without asking and
  report the gate_id.
- **Anchor (zero user input):** `work_unit_id` (a UUID) from
  `POST $COORD_HTTP_URL/coord/work-units/upsert` with the parent plan stem as
  `slug` (capture the returned `work_unit_id`; or the device-authed
  `GET /coord/agent-work-units/<slug>` — the operator `GET /coord/work-units/<slug>`
  403s a device JWT);
  `phase_name` from this phase's heading. Anchor = (work_unit_id, phase_name). The
  `unit_ready`/`unit_status` predicates carry this UUID, not the slug.
- **Register:** prefer MCP `coord_register_gate` (kinds: `pr_merged`,
  `deploy_healthy`, `claim_terminal`, `operator_approval`, `ci_green`,
  `ref_exists`, `metric_threshold`, `time_elapsed`, `unit_ready`,
  `migration_at_head`, `infra_drift_clear`, `file_exists`, `sql_count`,
  `unit_status`, `gate_cleared`, `commit_live`; plus — **exception cases only,
  see the Continuation bullet below** — an optional typed `continuation` or legacy
  `continuation_prompt` e.g. `run /implement-phase <stem> "<phase>"`). **HTTP
  fallback** when MCP is unavailable — for a plan-anchored gate it is now TWO
  device-authed calls on coord's `require_jwt` sub-router (device/agent/service JWT
  all work): (1) `POST $COORD_HTTP_URL/coord/work-units/upsert {slug, title?, status?}`
  → **capture `work_unit_id`**; (2)
  `POST $COORD_HTTP_URL/coord/work-units/<slug>/register-gate`
  `{predicate, phase_name (required), continuation_spawn?, clearance_audience?, gate_class?}` —
  `register-gate` does NOT upsert (404s `work_unit_not_found` if you skip step 1).
  Reach the two routes over a held device JWT, the `/coord-mcp` proxy (injects a
  device JWT), or the acting-user-service token (`coord-acting-bearer.sh`); MCP
  `coord_register_gate` now works from a device session too. A claim-anchored gate
  (no slug) uses MCP or `POST $COORD_HTTP_URL/coord/gates/register` (default
  `https://coord.qontinui.io`). Tenant derives server-side — never pass it.
- **Continuation:** gate registration is **continuation-less by default** —
  meaning **no `continuation` and no `continuation_prompt` (MCP
  `coord_register_gate`), and no `continuation_spawn` (HTTP `register-gate`)**.
  All three spellings are the SAME knob (coord materializes both MCP fields into
  the DB's `continuation_spawn` and both spawn), and the default is **omission**
  (`continuation_spawn` NULL), *not* the typed `{"action":"notify_only"}`, which
  stores a payload. A redundant continuation queues a duplicate, parallel run of
  work the registering session is still doing itself (charter rule 10, "Finish to
  zero"). **This blocked-exit path is an enumerated exception**, though: the phase
  is *exiting* incomplete, so this session will not be the dispatcher and a
  continuation is the norm here. Attach one — **unless** the blocker is
  **sensitive** (security / credential / billing / strategy → notify-only
  unconditionally), or you are in fact staying alive to monitor it (rule 10 keeps
  a session monitoring for "an observable signal and a short expected wait
  (≲2h: deploy, CI, merge train)"; inside that window finish the phase rather
  than gating it — that window is a reason NOT to gate, never a reason to gate
  without a dispatcher). If you rely on the spawn, delivery is a live defect —
  continuations are being dispatched but never consumed, and coord's 24h pending
  window drops them permanently — so treat it as best-effort and read the gate's
  `continuation_consumed_outcome` (a **null** outcome means never claimed, which
  is worse than a recorded `spawn_failed`).
  (Canonical: `_gate-registration` → "Continuation policy".)
- **`clearance_audience`:** set `agent` for agent-verifiable facts ("/vet-plan
  was run", "crate exists + tests green", "a dual run emitted evidence") so the
  session that completes the work can attest the gate itself; set `operator` for
  business/judgment/strategy or on-page-human-verification gates. Default is
  `operator` if omitted; the sensitive-work rule always forces `operator`.
- **`gate_class`:** classify the gate so coord's per-tenant `gate_clearance`
  matrix can resolve who may clear it. `security-surface` when the deferred work
  this gate guards would itself fire a `security-and-autonomy` glob or content
  trigger (name the trigger in `phase_name` so it stays auditable — a
  CLAIM-anchored gate has no `phase_name`, so name it in the plan or report
  instead);
  `ops-confirm` for deploy/sweep/migration/config confirmations;
  `routine-review` for mechanical follow-ups. **Omit when none applies** —
  omitting is safe and never a loophole, and a guessed class is worse than none.
  ⚠️ Do not ask for the `agent_non_author` authority on this fleet: it is a
  ONE-DEVICE fleet and no gate carries an agent id, so the device floor
  (`reg_dev == cal_dev`) treats every caller as the author and it resolves to
  "nobody may attest". A second paired device would also lift it.
  (Canonical: `_gate-registration` → "`gate_class`".)
- **Predicate choice:** wait-on-PR (non-coord repo) → `pr_merged`; work landing
  on a **coord-orchestrated repo** → `commit_live` `{repo, commit_sha}` with a
  **post-land main SHA** (NEVER a pre-land branch-head SHA — rebase-land rewrites
  SHAs so the gate rots open, gate `c14d103c` 2026-07-11; or anchor `unit_status`
  instead — **NOT `file_exists`, which is broken, see below**); wait-on-deploy →
  `deploy_healthy`; wait-on-CI → `ci_green`; burn-in → `time_elapsed`; metric →
  `metric_threshold` (explicit `labels` — e.g. `coord_ci_runner_count` MUST filter
  `{status:"idle"}`); a vetted plan that is ready, dispatchable work → `unit_ready`
  `{work_unit_id, ready_status}` — transition the unit FIRST and set
  `ready_status` to the status that actually landed (`vetted`, else the Free
  fallback `vetted_unattested`); a hardcoded Attested value on a unit you own
  never clears, since an owner may not attest (canonical: `_gate-registration`).
  (**NOT** `operator_approval` — `operator_approval`
  is for genuine human decisions, not a work queue); schema/alembic-at-head →
  `migration_at_head` `{schema}`; infra drift cleared → `infra_drift_clear`; a repo
  file/workflow existing → ⛔ `file_exists` is **KNOWN BROKEN (2026-08-05): it 403s
  fleet-wide on the contents API and the gate can never clear — use `commit_live`
  (post-land SHA) or `unit_status`**;
  a coord data count crossing a bound → `sql_count` `{query_id,op,n}` (whitelisted
  `query_id`, never raw SQL); an umbrella plan reaching a status → `unit_status`
  `{work_unit_id,status}`; another cross-anchor gate clearing → `gate_cleared`
  `{gate_id}`; needs-human → `operator_approval`. Anything
  **security / credential / billing / strategy-sensitive** → `operator_approval` +
  notify, never an auto-resuming gate, never silently auto-registered.
- **Masked-tool honesty:** if `coord_register_gate` fails as unknown/
  method-not-found (per-agent MCP allow-set masking, coord `mcp/mod.rs`), report
  **"gate NOT registered — coord_register_gate not in this session's tool
  allow-set"** and fall back to HTTP (or surface to the operator). NEVER report a
  gate registered without a returned `gate_id`.
- **Warnings honesty — a `gate_id` is necessary, NOT sufficient**
  [policy: `coordination` `gate-warnings-mean-not-usable`]. A non-empty
  `warnings[]`, or an `initial_verdict_reason` containing **"cannot evaluate"**,
  means **REGISTERED-BUT-NOT-USABLE**: the row was written and the gate can never
  clear. Do NOT report the deferred item gated. Re-check with
  `coord_check_gate_predicate {predicate}` **against a control whose answer you
  already know** (identical output on the control proves the predicate is dead,
  not your anchor), re-register on a predicate coord can evaluate, withdraw the
  unusable one (`coord_withdraw_gate`), and quote the NEW `gate_id`. Canonical:
  `_gate-registration` → "Registration warnings".
- **Dead-transport honesty (the OTHER mask):** a call that returns **`"Command
  failed with no output"`** is a *dead cached transport*, not a masked tool — the
  tool is present and listed, so the fallback above never fires. Presume the
  registration **LOST** (8 of 8 prod-adjudicable "no output" writes were adjudicated
  lost on 2026-07-26, four of them `coord_register_gate`), run **`/coord-revive`**
  for a typed verdict naming the door that is live right now, re-issue there, then
  **verify by read** (`coord_gate_inspect(gate_id)`, or the anchor filter
  `GET .../coord/agent-gates?work_unit_id=<uuid>&phase_name=<name>`). A retry's success is
  never evidence the original landed. The same applies to `coord_attest_gate` below.
  Canonical: `_gate-registration` → "Dead-transport honesty".
- The plan-file `## Gates` block is a **local convenience mirror only** — coord is
  the source of truth; never require it, never read it back as authoritative.
- **Re-registering for the same plan/anchor:** first cancel the prior gate's
  PENDING continuation so the old queued runner-terminal spawn doesn't fire
  alongside the new one — `GET .../coord/agent-gates?work_unit_id=<id>` for rows with
  `continuation_dispatched_at != null ∧ continuation_consumed_at == null ∧
  continuation_cancelled_at == null`, then
  `POST .../coord/gates/:gate_id/agent/continuation-cancel {reason}` — the
  device-authed `/agent/` **infix** twin, so a device session does the whole loop
  itself: `/coord/agent-gates` discovers the pending continuation and this route
  retires it. `cancelled_by` derives from the JWT and is not a body field.
  Best-effort; 404 = nothing pending, 409 `already_consumed` =
  a spawn already happened — report it honestly. (This bullet used to say the
  cancel "stays operator-only" and that a device session had to reach an
  operator door; that was wrong — it named the unprefixed OPERATOR route, which
  answers an agent 401.) (canonical spec:
  `_gate-registration` → "Continuation cancel + refresh".)

**Attest-on-completion (close the loop).** If this phase instead COMPLETES work
that a registered gate was watching (a previously-blocked phase now finishes), it
MUST attest that gate — otherwise an agent-fact gate rots open until a human
clicks it.

- **Find the gate:** by the `gate_id` recorded at registration, or by lookup
  `GET $COORD_HTTP_URL/coord/agent-gates?work_unit_id=<id>&phase_name=<name>` — the OPEN
  gate whose condition the completed work satisfies. That is the **device-authed**
  read door; the operator `GET /coord/gates` is `TenantId`-only and 403s a device
  JWT (a wrong-door 403, not a missing gate).
- **Attest (unchanged — keyed by `gate_id`):** prefer MCP `coord_attest_gate` (pass
  `gate_id` — works from a device session since attest takes no upsert); fall back to
  the device loopback forwarder `POST http://127.0.0.1:{runner_port}/coord-mcp/gates/{gate_id}/attest`
  (header `X-Coord-Mcp-Proxy-Key`, or `Authorization: Bearer <nonce>` on configs
  written after the Phase 2 header move — no body bearer; maskless fallback), then the
  direct device-authed `POST $COORD_HTTP_URL/coord/gates/:gate_id/attest`. Tenant
  derives server-side — never pass it. Legal only on an OPEN `operator_approval`
  gate with `clearance_audience = 'agent'` in the caller's own tenant; coord flips
  it to `cleared` and fires the same fanout as operator approve.
- **Masked-tool honesty:** if `coord_attest_gate` is unknown/METHOD_NOT_FOUND it
  isn't in this session's allow-set → fall back to the HTTP attest route. NEVER
  claim a gate attested without a returned cleared `gate_id`.
  A **`"Command failed with no output"`** attest is the *dead-transport* failure, not
  allow-set masking — the tool was present, so that fallback never fires: presume the attest
  **LOST**, run **`/coord-revive`**, re-issue over the live door, and read the gate
  back to confirm `cleared`. A lost attest is the quiet one — the gate rots open
  while this run reports the item done. Canonical: `_gate-registration` →
  "Dead-transport honesty".
- **Honesty:** NEVER report a deferred item as done without EITHER a cleared
  `gate_id` OR an explicit "gate not found" note.

### Part 3: Report

After Part 2 completes, report:
- What was implemented (files created/modified, line counts)
- What the review found and fixed
- What next-steps found and fixed
- Any gate registered for a blocked condition (gate_id + predicate), or that
  registration was skipped/declined

## Phase Description

$ARGUMENTS
