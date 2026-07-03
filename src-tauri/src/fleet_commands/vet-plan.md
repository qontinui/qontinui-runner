# Vet Plan

Read a plan, research the codebase to validate its claims, and edit the plan in place to fix anything wrong, missing, or out-of-date. The plan is the deliverable — leave it stronger than you found it.

## Decision priorities

When the plan's choices need to be evaluated — pattern selection, abstraction boundaries, scope, sequencing, migration approach — judge them against the project's priorities, in this order:

1. **Powerful features** — does the design unlock real capability, or settle for a thin shim?
2. **Scalability** — does it hold up as data, concurrency, or surface area grows?
3. **Robustness** — does it fail safely, handle edge cases, and stay correct under adversarial conditions?
4. **Clean code** — is the structure clear, small, and consistent with how the codebase already solves similar problems?

These four are the **engineering priorities** — they decide *what* gets built. Two sibling sets bind alongside them:

- **UX priorities** (ordered: predictability → discoverability without clutter → no-surprise reversibility → honesty about uncertainty) — gates for any user-facing surface: an option that fails one is rejected even if it wins on power. Engineering breaks ties within UX. (Memory: `ux-priorities-alongside-engineering`.)
- **Implementation priorities** (ordered: verified throughput → early risk retirement → autonomy with checks → momentum through re-planning) — govern *how and when* work executes, orthogonal to the other two sets. (Memory: `implementation-priorities`.) For vetting they bind in three places: **sequence phases most-falsifiable-first** (assumption-killing probes before the builds that depend on them); **size the plan against `/implement-plan` coordinator orchestration** — a plan too large even for subagent-delegated execution must be split or explicitly flagged for a fresh-session handoff; and the **escalation rule** in the Decision policy below.

**Programming effort and backward compatibility are NOT factors.** Do not preserve a worse design because rewriting it would be more work, and do not flag a defect just because the fix is invasive. Do not endorse an awkward shape because it avoids breaking existing callers — breaking changes are expected in this project. If a plan's design is justified primarily by "less work" or "doesn't break X," that is itself a defect worth flagging.

## Arguments

- `$ARGUMENTS` — Path to the plan file (e.g. `C:/tmp/some-plan.md` or relative). If omitted, look for the most recently modified `*plan*.md` file under `C:/tmp` and the working tree root, and ask the user to confirm before editing.

## Decision policy (binding)

When vetting surfaces a question, a fork in the road, or an unresolved trade-off, **answer it yourself** using these priorities, in order:

1. **Powerful features** — does this unlock capability the alternative cannot?
2. **Scalability** — does this hold up across many runners, many users, many specs, many tenants?
3. **Robustness** — fewer failure modes, clearer invariants, less silent drift?
4. **Clean code** — simpler model, fewer special cases, better separation of concerns?

**Explicitly NOT factors:**
- Programming effort, implementation time, lines of code, refactor cost
- Backward compatibility, migration burden, "would break existing callers"
- "More conservative" or "smaller blast radius" as a tiebreaker

If two options tie on the priorities above, prefer the one that **deletes more code** (delete over deprecate) and the one that **avoids new abstractions** when an existing one fits.

Do not escalate questions to the user. If the priority sets (engineering, plus the UX gates for user-facing surfaces) genuinely don't resolve a question, it is by definition not important enough for the operator to have an opinion on — pick the option you'd defend in review and justify it in the plan edit. If priority-unresolvable questions recur, note in the vet summary that the priority sets should be expanded. The only things worth surfacing to the operator are **operator-resource needs** (something only they can physically do) and a plan **too large even for coordinator-style orchestration** (per the implementation priorities).

When you resolve a question this way, write the decision into the plan with one sentence of rationale that names the priority that decided it (e.g. "**Resolved.** Use the registry-backed lookup — scales to N tenants without per-app caches.").

## Instructions

### 0. Publish activity to `coord.device_status`

Before reading the plan, UPSERT a status row so the operator dashboard
shows this session as vetting. This is the read-side of Phase 1.1 + 1.3
of plan `2026-05-21-coordination-improvements.md` — Phase 1.1 added the
`tenant_id` column on `coord.device_status`; Phase 1.3 wires the
dashboard's `MachineCard` to render it. This step fills the row.

The UPSERT is keyed on `device_id`, so each call overwrites the prior
row for this machine. Skip-and-warn if `device_id` cannot be resolved —
status publication is observability, not gating.

**Resolution chain:**

1. **`device_id`.** Env `QONTINUI_MACHINE_ID` first. Else read
   `~/.qontinui/machine.json` and parse the `"device_id"` field; fall
   back to `"machine_id"` if present (legacy shape). If neither
   source supplies a UUID, emit a single-line warning and skip the
   UPSERT (proceed to Step 1).
2. **`current_repo`.** `basename "$(git rev-parse --show-toplevel)"`.
3. **`current_branch`.** `git symbolic-ref --short HEAD`.
4. **`tenant_id`.** Env `QONTINUI_TENANT_ID` if set; otherwise omit
   the field (coord column is nullable).
5. **Coord HTTP base.** Env `COORD_HTTP_URL` first, else
   `https://coord.qontinui.io`.

**`<plan-stem>`** is the plan filename without `.md` extension or
directory prefix (e.g. `2026-05-21-coordination-improvements`).

**Initial UPSERT** (fire before reading the plan):

```bash
curl -fsS -X POST "$COORD_HTTP_URL/coord/status" \
  -H "Content-Type: application/json" \
  -d "$(cat <<EOF
{
  "device_id": "<device_id>",
  "current_task": "vet-plan: <plan-stem>",
  "current_repo": "<repo basename>",
  "current_branch": "<branch name>",
  "details": {},
  "tenant_id": "<QONTINUI_TENANT_ID, omit field entirely if unset>"
}
EOF
)"
```

**Failure handling.** Any non-2xx response is logged as a single-line
warning (`⚠️ coord status publish failed: <status> <body>`) and the
skill continues. NEVER abort vetting on a status publication error.

**Clear on completion (Step 6).** After Step 5 stamps the plan as
VETTED, fire a clearing UPSERT with `current_task: null` so the
dashboard tile stops showing this vet session as in-flight:

```bash
curl -fsS -X POST "$COORD_HTTP_URL/coord/status" \
  -H "Content-Type: application/json" \
  -d "$(cat <<EOF
{
  "device_id": "<device_id>",
  "current_task": null,
  "current_repo": "<repo basename>",
  "current_branch": "<branch name>",
  "details": {}
}
EOF
)"
```

If the skill aborts before Step 5 (e.g. the plan was already SHIPPED
and the user declined to overwrite), fire the clearing POST
best-effort. If that also fails, `prune_stale()` TTLs the row within
an hour.

### 0.9. Name this session + terminal after the plan

Derive a human name from the plan filename and use it to title the session
and the terminal window. The rule: take the plan **stem** (filename without
`.md` or directory), strip a leading `YYYY-MM-DD-` date prefix, and strip a
trailing ` plan` / `-plan` / `_plan` word if present.
(e.g. `2026-05-21-coordination-improvements` → `coordination-improvements`;
`2026-06-02-fleet-auth-plan` → `fleet-auth`.)

Run this once, substituting `<plan-stem>` (the same stem Step 0 computed):

```bash
DISPLAY=$(printf '%s' "<plan-stem>" \
  | sed -E 's/^[0-9]{4}-[0-9]{2}-[0-9]{2}-//; s/[-_ ][Pp][Ll][Aa][Nn]$//')
mkdir -p "$HOME/.qontinui/session-titles"
printf '%s\n' "$DISPLAY" > "$HOME/.qontinui/session-titles/$CLAUDE_CODE_SESSION_ID"
echo "$DISPLAY"
```

**Terminal title:** automatic. Writing the title-hint file is all you need —
the `set-terminal-title.sh` Stop hook reads it and titles the terminal when
this turn ends. Do NOT try to `echo` an escape sequence yourself; the harness
strips tool-stdout control bytes, so it would silently no-op.

**Session label (`/rename`):** this is the one thing a command cannot set on
its own — Claude Code only renames the session when the operator types
`/rename`, never programmatically mid-turn. So surface the derived name to the
operator as a ready-to-paste line, e.g.:

> Session named `coordination-improvements`. To set the Claude session label,
> paste: `/rename coordination-improvements`

The paste only affects the in-app session label; the terminal title is already
handled by the title-hint file above.

This step is best-effort: if `$CLAUDE_CODE_SESSION_ID` is unset or the write
fails, skip it silently and continue — naming never blocks vetting.

### 1. Read the plan in full

Use `Read` on the path. Don't skim — note every concrete claim:
- File paths, function names, line numbers
- "There is no X" / "we'll need to add Y" assertions
- Architectural decisions presented as locked
- Estimates and phase breakdowns

A plan written by someone else (or by past-you) usually has 2–4 things that look right but aren't. Your job is to find them.

### 2. Verify every concrete claim

For each claim in the plan, validate against the current codebase. Run these in parallel where possible:
- **File / module exists** → `Glob` or `Read`
- **Function or symbol exists** → `Grep` for the exact name
- **"No prior art for this" assertions** → `Grep` aggressively for related patterns; the prior art is usually one rename away (e.g. plan says "wrappers need a proxy" — search for `*proxy*.rs` across the repo)
- **Architectural assumptions** → spot-check 1–2 representative files
- **Memory references** → cross-check against `MEMORY.md` entries (memories can be stale; verify before quoting)

When the plan proposes a NEW abstraction, always check whether an existing one already covers it. The single most common defect in plans is "let's add helper X" when helper X already exists under a different name.

If the plan touches any of the SDK files below, also verify the runner-side parallel implementations exist — a plan that only edits the SDK side without naming the runner-side wireups is incomplete and should be flagged as a defect.

> **UI Bridge wire-through reminder.** If your changes touch any of:
> - `ui-bridge/packages/ui-bridge/src/server/handlers.ts`
> - `ui-bridge/packages/ui-bridge/src/server/types.ts` (especially `UI_BRIDGE_ROUTES`)
> - `ui-bridge/packages/ui-bridge/src/server/relay-handlers.ts`
> - `ui-bridge/packages/ui-bridge/src/react/commandHandlers.ts`
>
> …the change MUST also be wired through the runner's three parallel layers, or it will silently 404 / drop fields against a live runner:
> 1. **Direct HTTP handlers** in `qontinui-runner/src-tauri/src/mcp/ui_bridge/<family>.rs`. Register in the family's `routes()` AND `route_entries()` (per-family static manifest concatenated by `mod.rs::route_manifest()`).
> 2. **WS-transport outer wrappers** in `qontinui-runner/src-tauri/src/mcp/sdk_client.rs` for browser-based consumers under `/ui-bridge/sdk/*`.
> 3. **Frontend IPC bridge** in `qontinui-runner/src/hooks/ui-bridge-events/utils.ts` (and the family-specific `use*Events.ts` siblings) when the route depends on data only the React frontend has.
>
> Verify with: `cargo test manifest_matches_route_calls` (internal drift) AND `cargo test sdk_manifest_routes_are_exposed_by_runner` (SDK↔runner diff — Phase 2a). See `qontinui-runner/src-tauri/src/mcp/ui_bridge/CONTRACT.md` for the full per-route classification.
>
> If the plan ALSO touches response shape, query-param parsing, or status-code mapping (not just route registration), flag the absence of a `qontinui-runner/scripts/contract-smoke.ps1` run as a vet defect — the manifest diff catches missing routes but not field-stripping, query drops, or status-code drift.

### 3. Identify defects

Categorize what you find:
- **Wrong** — claim contradicts the code (path, signature, behavior)
- **Redundant** — proposes new code that duplicates existing infrastructure
- **Missing** — overlooks a concrete consumer, edge case, or coupled subsystem
- **Misaligned** — proposes a pattern inconsistent with how the codebase already solves the same problem
- **Stale** — built on a prior assumption that has since changed

Don't flag style preferences or hypothetical concerns — only material defects.

### 4. Edit the plan in place

Use `Edit` (not `Write`) to surgically fix the plan, preserving the author's voice and structure. Specifically:
- **Add a "Discovered prior art" section** near the top if the plan missed existing infrastructure. Include a small table with `Piece | Location | Notes`.
- **Rewrite speculative sections** ("search for X, it might be in Y") to point at the actual file:line.
- **Replace proposed abstractions** that duplicate existing ones — show the existing pattern as the template.
- **Update file inventories** to match what will actually be created/modified.
- **Resolve open questions** inline. Two ways a question gets resolved:
  - *Research answered it* — strike through and explain (`~~...~~ **Resolved.** ...`).
  - *No new evidence, but the question is about an engineering trade-off* — apply the **Decision policy** above. Pick the option that wins on power → scalability → robustness → clean code, and write the decision into the plan with one sentence of rationale naming the deciding priority. Do NOT punt the question back to the user just because it requires judgment.
- **Add concrete file:line citations** where the plan was vague.
- **Verify `Depends-On:` targets exist** — see [Depends-On verification](#depends-on-verification) below.
- **Don't gut sections that are already correct** — the goal is targeted fixes, not a rewrite.

If the plan's overall direction is sound but an entire phase is wrong, rewrite that phase. If the overall direction is wrong, stop and surface that to the user before editing — don't silently rewrite the architectural premise.

#### Depends-On verification

A plan MAY declare upstream dependencies inline in its status blockquote
using a `Depends-On:` suffix:

```markdown
> **Status: VETTED 2026-05-21.** <summary>. Depends-On: 2026-05-20-default-tenant-propagation, 2026-05-19-some-other-plan.
```

Parser rule (same as `/verify-plan-status` and `/implement-plan` — keep
these three skills aligned):

1. Look at the status blockquote (the first `> **Status:` block under the
   H1) and find EVERY case-sensitive `Depends-On:` occurrence — a block
   often carries one in the headline sentence and another in a trailing
   `History:` / re-vet line.
2. For each occurrence, consider only the remainder of that PHYSICAL line
   — never the following blockquote lines or paragraphs, which may name
   unrelated plans in prose.
3. Within that line, keep only date-prefixed plan-stem-shaped tokens
   (`YYYY-MM-DD-<kebab-slug>`, e.g. `2026-06-02-some-plan`). Prose, bare
   dates (`2026-05-21.`), and trailing punctuation never produce tokens
   — a stem requires at least one `-word` segment after the date. Each
   token is a bare plan **stem** — no `.md` extension, no path.
4. Union the stems across all occurrences, deduped, order-preserving.

   (A naive first-occurrence + split-on-commas parse mis-handled real
   status blocks whose prose contained a second `Depends-On:` or commas —
   it produced phantom missing-dep aborts. Fixed in the canonical resolver
   2026-06-04; this inline fallback mirrors it.)

For each dep stem, resolve to a plan file using the lookup chain:

1. Try `D:/qontinui-root/plans/<stem>.md` first (in-progress location).
2. If that doesn't exist, try `D:/qontinui-root/qontinui-dev-notes/plans/<stem>.md`
   (shipped archive).
3. If neither exists, the dep is **missing** — flag it as a vet defect.

Use `Read` (a failure is the not-found signal) or `Glob` to check both
locations. A missing dep is a `Wrong` or `Stale` defect per Step 3's
categories — it means the plan references upstream work that either was
renamed, never written, or already archived under a different stem.

When you find missing-dep defects:

- Add them to the report under "Missing dependencies" with the exact
  unresolved stem.
- Include them in the auto-fix count if you correct the stem in the plan
  (the plan author may have typo'd the stem). If you can't determine the
  correct stem, leave the typo'd value in the plan but flag it for the
  user — do NOT silently delete the Depends-On entry.
- A plan with one or more missing deps is still vettable; the missing-dep
  finding does not block stamping `VETTED`. It just gets surfaced in the
  report.

A plan with no `Depends-On:` field is the common case — no verification
needed.

### 5. Stamp the plan as VETTED

After surgical fixes are written, add a status block immediately below the
H1 title. Use the same format `/verify-plan-status` and `/implement-plan`
use, so the lifecycle reads cleanly:

```markdown
# <Plan Title>

> **Status: VETTED <YYYY-MM-DD>.** <one-line summary of what was checked
> and what survived the audit>. Defects found: <count>. Auto-fixed: <count>.
> Surfaced for user: <count>. <Optional: pointer to follow-up plan if you
> created one in the report.>
```

#### Single-stamp invariant — read before stamping

A plan must have **exactly one** `> **Status:` blockquote between the H1
and the body. Before writing your stamp:

1. Read the top of the plan. Identify EVERY top-of-file blockquote that
   asserts a status, lifecycle state, or verification date — lines
   starting with `> **Status:`, `> **Edit YYYY-MM-DD —`, or `> **Update:`
   all count. Don't be picky about format; if it reads like a status
   declaration, it counts.
2. Use `Edit` to **delete every existing status-adjacent blockquote** —
   even if it came from a different skill (`/verify-plan-status` writes
   `NOT STARTED` / `PARTIAL`; `/implement-plan` writes `IN PROGRESS` /
   `SHIPPED`; older `/vet-plan` runs may have left `Status: VETTED ...`).
   Yours replaces all of them.
3. Then `Edit` again to insert your single new `> **Status: VETTED …`
   block.
4. If folding in history is useful (e.g., the plan was previously
   `NOT STARTED` per a verify pass, or had earlier verifier findings
   in a separate `> **Edit:` block), include the salient info in
   **one trailing line inside your new block**, prefixed `History:` or
   `Previously:`. Never as a sibling blockquote.

When `/vet-plan` and `/verify-plan-status` disagree on what the status
should be (the plan is both VETTED *and* NOT STARTED — a vetted plan
that nobody has implemented yet), consolidate: write the lifecycle
stage in the heading (`Status: VETTED <date> — implementation not
started.`) and put the verifier's findings in the body. The two
skills do NOT both stamp side-by-side.

#### Lifecycle states

| State | Owner skill | Means |
|---|---|---|
| DRAFT | author | plan written, not yet vetted |
| VETTED | `/vet-plan` | claims audited; ready to implement |
| IN PROGRESS | `/implement-plan` | implementation underway |
| SHIPPED | `/implement-plan` or `/verify-plan-status` | all phases live in code |
| PARTIAL | `/verify-plan-status` | some phases live, some open |
| NOT STARTED | `/verify-plan-status` | no implementation evidence |
| SUPERSEDED / OBSOLETE | any | terminal states |

`/vet-plan` writes only `VETTED`. If the existing block is `SHIPPED`,
`SUPERSEDED`, or `OBSOLETE`, do NOT overwrite — surface to the user
that they're vetting a closed plan and confirm they actually want this
rewrite. (`PARTIAL` and `NOT STARTED` are fine to overwrite — your vet
pass produces fresher information.)

This stamp is mandatory. A vetted plan without the stamp is
indistinguishable from a draft, and `/implement-plan` will treat it as
still-aspirational.

After stamping, fire the clearing `POST /coord/status` documented in
Step 0 with `current_task: null` so the dashboard tile stops showing
this vet session as in-flight.

### 5.4. Register a `unit_ready` gate for the vetted plan (dispatchable-work queue)

*(canonical spec: `_gate-registration` — keep copies in sync)*

A VETTED plan is **ready, dispatchable work** — not a human decision. When you
stamp VETTED, register (or **refresh** an existing) `unit_ready` gate (coord
tracks the plan as a generic **work unit**) so coord turns the ready plan into a
queued continuation that dispatches into a session the operator can **see**,
instead of leaving the plan to rot until someone clicks. This **replaces** the old
`operator_approval`-bootstrap gate that used to queue ready work: a work queue is
`unit_ready`, NOT `operator_approval` (`operator_approval` is for genuine human
decisions only — see the predicate guidance in `_gate-registration`).

Register exactly once per VETTED stamp (refresh, don't duplicate):

> **Agent sessions: use the device-authed work-unit door.** A vetting agent holds
> a coord **device JWT** (carrying `tenant_id`) but **no `OperatorContext`**. The
> `/coord/work-units/*` routes live on coord's `require_jwt` sub-router, so a
> device JWT resolves `tenant_id` server-side and CAN upsert + register — the old
> operator-only `/coord/plans/*` wall (which 403'd a vetting agent and silently
> broke §5.4 auto-dispatch) is gone. Registration is now **TWO calls** — upsert
> then register-gate — because `register-gate` does NOT upsert (it 404s
> `work_unit_not_found` if the slug is absent). Prefer, in order: (1) MCP
> `coord_register_gate` (now usable from a device session — it can upsert the work
> unit then register, both over the `/coord-mcp` proxy); (2) the **direct
> device-authed HTTP** routes when a raw device JWT is held:
> `POST $COORD_HTTP_URL/coord/work-units/upsert {slug,title?}` → capture
> `work_unit_id`, then
> `POST $COORD_HTTP_URL/coord/work-units/<plan stem>/register-gate`; (3) the
> acting-user-service token (`coord-acting-bearer.sh`) on the same two routes if no
> device identity is available. The legacy operator-only `/coord/plans/upsert` +
> `/coord/gates/register` are removed (coord P4) — do not use them.
> (`2026-06-15-coord-device-authed-plan-ready-gate-registration`, generalized to
> work units.)

1. **Upsert the work unit and capture `work_unit_id`.**
   `POST $COORD_HTTP_URL/coord/work-units/upsert` with
   `{ "slug": "<plan stem>", "title": "<plan H1>" }` (idempotent on slug; the stem
   is the filename without `.md`/path, the same `<plan-stem>` Step 0 used) →
   returns `{ "work_unit_id": "<uuid>", … }`. This is the **mandatory FIRST call**:
   the device-authed `register-gate` endpoint in step 4 does NOT upsert (it 404s if
   the slug is absent). The captured `work_unit_id` UUID anchors the gate AND is
   what the `unit_ready` predicate carries.
2. **Resolve the operator's `device_id` DYNAMICALLY** (never hardcode a UUID):
   env `QONTINUI_MACHINE_ID` first, else read `~/.qontinui/machine.json` and parse
   `"device_id"` (fall back to `"machine_id"` if present). If neither yields a
   UUID, skip the continuation spawn but still register the gate (notify-only) and
   note it in the report.
3. **Check for an existing gate** anchored to this work unit
   (`GET $COORD_HTTP_URL/coord/gates?work_unit_id=<id>` — find an OPEN `unit_ready`
   gate for this plan). If one exists, **refresh** it (re-register / update the
   continuation) rather than creating a duplicate. **Before refreshing, cancel
   the prior gate's PENDING continuation** so the old queued runner-terminal spawn
   does not fire alongside the new one: for any row in that GET with
   `continuation_dispatched_at != null ∧ continuation_consumed_at == null ∧
   continuation_cancelled_at == null`, fire
   `POST $COORD_HTTP_URL/coord/gates/:gate_id/continuation-cancel`
   `{cancelled_by:"<this session>", reason:"refreshed — superseded by re-registration"}`
   (`TenantId` auth, tenant derives server-side — an operator/`TenantId`-only
   route). Best-effort: a 404 = nothing pending; a 409 `already_consumed` = a spawn
   already happened (report it, don't pretend the cancel landed). (canonical spec:
   `_gate-registration` → "Continuation cancel + refresh".)
4. **Register** via the transport cascade in the blockquote above (the work unit
   was already upserted in step 1, so `register-gate` will find it): prefer MCP
   `coord_register_gate`; when a raw device JWT is held, the direct device-authed
   `POST $COORD_HTTP_URL/coord/work-units/<plan stem>/register-gate` (resolves
   tenant from the device JWT; body `{predicate, phase_name (required),
   continuation_spawn?, clearance_audience?}` — `slug` comes from the path, the
   predicate carries the `work_unit_id` UUID from step 1). `register-gate` does NOT
   upsert — if step 1 was skipped it 404s `work_unit_not_found`. The acting-user
   token works on the same route when no device identity is held. The legacy
   operator-only `/coord/plans/upsert` + `/coord/gates/register` are removed (coord
   P4) — do not fall back to them.
   - **Predicate:** `{"kind": "unit_ready", "work_unit_id": "<uuid from step 1>", "ready_status": "vetted"}`
     — coord auto-clears it when the work unit reaches `ready_status` (`vetted`) AND
     every other gate anchored to this unit is cleared. (The `register-gate` endpoint
     accepts any **predicate-cleared** kind; only `operator_approval` — a human
     decision — is rejected 403, so it can never become a work-queue-as-decision
     fallback.)
   - **Anchor:** the plan (work unit). The `work_unit_id` comes from the step-1
     upsert; pass `phase_name` (the plan title, or a synthetic label like
     `"vet→implement handoff"` for a whole-plan gate — `phase_name` is **required**,
     since coord's anchor is `work_unit_id` + `phase_name` together).
   - **`continuation_spawn`:** target the operator's device with a **visible**
     session —
     ```json
     {
       "target_device_id": "<device_id from step 2>",
       "presentation": "terminal",
       "initial_prompt": "run /implement-plan plans/<plan stem>.md",
       "continuation_prompt": "run /implement-plan plans/<plan stem>.md",
       "repos": ["<the plan's repo slug(s) — see below>"]
     }
     ```
     `presentation: "terminal"` (coord PR #356) opens a VISIBLE terminal window on
     the target device running the `claude` CLI with the prompt as argv — the
     operator sees it and can interrupt (that is the point of a visible
     continuation, per the visible-gate-continuations plan).
     **Populate `repos` with the plan's declared repo(s)** (from the plan's
     `**Repo(s):**` line, or the repos the plan touches) — do NOT leave it `[]`.
     An empty `repos` makes `acquire_continuation_workdir` skip worktree
     allocation and drop the continuation's terminal cwd onto the SHARED ROOT
     (`D:/qontinui-root`) uncoordinated — the exact concurrent-WIP clobber the
     coordination layer exists to prevent. With `repos` set, the runner
     provisions an isolated `.agent-worktrees/<id>/<repo>` worktree (the first
     repo) as the cwd, so the session edits a per-session worktree under a
     `kind=worktree` claim from the first tick. If the plan genuinely touches no
     repo (a pure-investigation plan), `[]` is acceptable.
   - **`clearance_audience`:** `unit_ready` auto-clears by predicate, so audience
     is moot for *clearing*; register consistently with the model (default
     `operator`) — the typed predicate, not a human, is what clears it.
5. **Masked-tool honesty + verification:** if `coord_register_gate` reads as
   unknown/method-not-found (per-agent allow-set masking) fall back to the
   device-authed HTTP routes (upsert → register-gate), and NEVER report a gate
   registered/refreshed without a returned `gate_id`.

(If coord doesn't yet accept `unit_ready` — e.g. the deploy that ships the
work-unit surface hasn't landed — report the gate as NOT registered with the
reason, rather than silently registering an `operator_approval` fallback that would
re-create the work-queue-as-decision antipattern.)

**Set the work unit's VETTED status directly.** When you stamp VETTED, transition
the coord work-unit registry so the `unit_ready` predicate can see it:
`POST $COORD_HTTP_URL/coord/work-units/<plan stem>/transition {to_status:"vetted", by_actor:"<this session>"}`
(or the step-1 upsert carrying `status:"vetted"`). The registry is directly
writable — there is no longer a plan-ingest worker mirroring `qontinui-dev-notes/plans/`,
so this explicit transition is what marks the unit vetted (the plan `.md` VETTED
stamp + its commit/push remain the operator-private artifact record). (claude-config
is NOT a coord sole-authority repo — its PRs land via normal GitHub flow.)

### 5.5. Offer to register a coord gate for a flagged-but-not-fixed item

*(canonical spec: `_gate-registration` — keep copies in sync)*

If vetting surfaces an item you flagged for the user that **cannot be resolved
now because it gates on an observable condition** (an upstream PR must merge, a
deploy/CI must go green, a metric must cross a threshold, a time window must
elapse, or an operator must approve), offer to register a coord gate for it — so
coord watches the condition rather than the finding rotting in the report. A
flagged item with no observable trigger (a product/scope call) is NOT a gate;
leave it in the report.

- **Default = explicit offer** via `AskUserQuestion` (header `Register gate?`,
  options Register / Skip), showing the derived anchor + predicate + condition.
  Under opt-in auto mode (env `QONTINUI_AUTO_GATE=1`) register without asking and
  note the gate_id in the report.
- **Anchor (zero user input):** `work_unit_id` (a UUID) from
  `POST $COORD_HTTP_URL/coord/work-units/upsert` with the plan stem as `slug`
  (capture the returned `work_unit_id`; or `GET /coord/work-units/<slug>`);
  `phase_name` from the relevant phase/section heading. Anchor = (work_unit_id,
  phase_name).
- **Register:** prefer MCP `coord_register_gate` (kinds: `pr_merged`,
  `deploy_healthy`, `claim_terminal`, `operator_approval`, `ci_green`,
  `ref_exists`, `metric_threshold`, `time_elapsed`, `unit_ready`,
  `migration_at_head`, `infra_drift_clear`, `file_exists`, `sql_count`,
  `unit_status`, `gate_cleared`; optional
  `continuation_prompt`). **HTTP fallback** when MCP is unavailable — for a
  plan-anchored gate it is now TWO device-authed calls on coord's `require_jwt`
  sub-router (device/agent/service JWT all work): (1)
  `POST $COORD_HTTP_URL/coord/work-units/upsert {slug, title?, status?}` →
  **capture `work_unit_id`**; (2)
  `POST $COORD_HTTP_URL/coord/work-units/<slug>/register-gate`
  `{predicate, phase_name (required), continuation_spawn?, clearance_audience?}` —
  `register-gate` does NOT upsert (404s `work_unit_not_found` if you skip step 1).
  Reach the two routes over a held device JWT, the `/coord-mcp` proxy (injects a
  device JWT), or the acting-user-service token; MCP `coord_register_gate` now works
  from a device session too. A claim-anchored gate (no slug) uses MCP or
  `POST $COORD_HTTP_URL/coord/gates/register` (default `https://coord.qontinui.io`).
  Tenant derives server-side — never pass it.
- **`clearance_audience`:** set `agent` for agent-verifiable facts ("/vet-plan
  was run", "crate exists + tests green", "a dual run emitted evidence") so the
  session that completes the work can attest the gate itself; set `operator` for
  business/judgment/strategy or on-page-human-verification gates. Default is
  `operator` if omitted; the sensitive-work rule always forces `operator`.
- **Predicate choice:** wait-on-PR → `pr_merged`; wait-on-deploy →
  `deploy_healthy`; wait-on-CI → `ci_green`; burn-in → `time_elapsed`; metric →
  `metric_threshold` (explicit `labels` — e.g. `coord_ci_runner_count` MUST filter
  `{status:"idle"}`); a vetted plan that is ready, dispatchable work → `unit_ready`
  `{work_unit_id, ready_status}` (**NOT** `operator_approval` — `operator_approval`
  is for genuine human decisions, not a work queue); schema/alembic-at-head →
  `migration_at_head` `{schema}`; infra drift cleared → `infra_drift_clear`; a repo
  file/workflow existing → `file_exists` `{repo,path,on_ref?}` (contents, not refs);
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
- The plan-file `## Gates` block is a **local convenience mirror only** — coord is
  the source of truth; never require it, never read it back as authoritative.
- **Re-registering for the same plan/anchor:** first cancel the prior gate's
  PENDING continuation (the §5.4 refresh-cancel rule applies to any
  continuation-carrying gate too) — `GET .../coord/gates?work_unit_id=<id>` for rows
  with `continuation_dispatched_at != null ∧ continuation_consumed_at == null ∧
  continuation_cancelled_at == null`, then
  `POST .../coord/gates/:gate_id/continuation-cancel {cancelled_by, reason}`
  (`TenantId` auth, best-effort). (canonical spec: `_gate-registration` →
  "Continuation cancel + refresh".)

**Attest-on-completion (close the loop).** Vetting normally registers gates
rather than completing gated work — but if this run also COMPLETES work that a
registered gate was watching, it MUST attest that gate (otherwise an agent-fact
gate rots open until a human clicks it).

- **Find the gate:** by the `gate_id` recorded at registration, or by lookup
  `GET $COORD_HTTP_URL/coord/gates?work_unit_id=<id>&phase_name=<name>` — the OPEN
  gate whose condition the completed work satisfies.
- **Attest (unchanged — keyed by `gate_id`):** prefer MCP `coord_attest_gate` (pass
  `gate_id` — works from a device session since attest takes no upsert); fall back to
  the device loopback forwarder `POST http://127.0.0.1:{runner_port}/coord-mcp/gates/{gate_id}/attest`
  (header `X-Coord-Mcp-Proxy-Key`, no body bearer — maskless fallback), then the
  direct device-authed `POST $COORD_HTTP_URL/coord/gates/:gate_id/attest`. Tenant
  derives server-side — never pass it. Legal only on an OPEN `operator_approval`
  gate with `clearance_audience = 'agent'` in the caller's own tenant; coord flips
  it to `cleared` and fires the same fanout as operator approve.
- **Masked-tool honesty:** if `coord_attest_gate` is unknown/METHOD_NOT_FOUND it
  isn't in this session's allow-set → fall back to the HTTP attest route. NEVER
  claim a gate attested without a returned cleared `gate_id`.
- **Honesty:** NEVER report a deferred item as done without EITHER a cleared
  `gate_id` OR an explicit "gate not found" note.

### 6. Report

Brief — under 150 words. State:
- What the plan was about (one sentence)
- The 2–5 material defects found, each with the section they were in
- What you changed (referenced by section name, not full diff)
- The status stamp you added (`Status: VETTED <date>`)
- Open questions you **resolved using the Decision policy**, with the deciding priority in parentheses (e.g. "picked registry-backed lookup (scalability)")
- Anything you flagged for the user that you did NOT auto-fix — limit this to product/scope/stakeholder calls the Decision policy can't decide; engineering trade-offs should already be resolved in the plan

End-of-turn summary: one or two sentences.

## Rules

- **Edit the plan, don't rewrite it.** Surgical changes only. Preserve the author's structure unless an entire section is wrong.
- **Cite the code.** Every claim added to the plan should reference a file:line. "Almost certainly exists" is not good enough.
- **Verify memories before quoting them.** `MEMORY.md` entries are point-in-time observations; check the current code before treating a memory as fact.
- **Don't add new phases or scope.** If you find work the plan missed, note it in the report — adding a new phase is a decision for the user.
- **Parallelize research.** Use Glob, Grep, and Read in parallel; spawn `Explore` for broad surveys. When you spawn agents, never end a turn purely "awaiting notification" on their results — completion notifications are an optimization, never a guarantee (the wake-up channel is at-most-once); re-check for finished results on a bounded timer and proceed on evidence. On any nudge / system-reminder wake, FIRST re-check whether the awaited research already finished (evidence over memory), collect it, and continue — never re-spawn completed research.
- **No new files.** The plan file is the only thing you should be writing to.
- **Decide; don't escalate.** Engineering trade-offs surfaced during vetting must be resolved in the plan using the Decision policy. Effort and backward compatibility are not factors. Only kick a question up to the user when it's genuinely a product/scope/stakeholder call.
