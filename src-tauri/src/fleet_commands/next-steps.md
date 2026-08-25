# Next Steps

Analyze a just-completed implementation for missing wiring, polish, and follow-up items, then implement them.

**This skill is invoked automatically by `/implement-phase` after `/review-plan` passes.**

## Arguments
- `$ARGUMENTS` - Description of what was just implemented

## Instructions

### Part 1: Analyze

Ask yourself: **"What are the next steps?"** Look for:

1. **Missing wiring** — dead code, unconnected imports/exports, unused endpoints, interfaces defined but not consumed
2. **Unhandled fields** — type fields or interface properties defined but never checked/used in the implementation
3. **Polish** — error messages that could be clearer, edge cases not handled, inconsistent naming
4. **Integration gaps** — new modules not exported from index.ts, new types not used by existing code, new functions not called anywhere
5. **Follow-up features** — small, natural extensions that complete the feature (e.g., if you added a query engine but forgot query validation)

### Part 2: Implement

For each item found:
1. Implement the fix or addition
2. Verify it compiles (`npx tsc --noEmit` or equivalent)
3. Add tests if the change is non-trivial

If no items are found, report "No next steps identified" and finish.

### Part 2.5: Offer to register a coord gate for a deferred follow-up

*(canonical spec: `_gate-registration` — keep copies in sync)*

If a next-step item **cannot be done now because it waits on an observable
condition** (an upstream PR must merge, a deploy/CI must go green, a metric must
cross a threshold, a time window must elapse, or an operator must approve) rather
than being implementable immediately, offer to register a coord gate for it — so
coord watches the condition and can auto-resume the follow-up after every session
closes. A follow-up with no observable trigger (an open-ended idea) is NOT a
gate; just list it.

- **Default = explicit offer** via `AskUserQuestion` (header `Register gate?`,
  options Register / Skip), showing the derived anchor + predicate + condition.
  Under opt-in auto mode (env `QONTINUI_AUTO_GATE=1`) register without asking and
  report the gate_id.
- **Anchor (zero user input):** `work_unit_id` (a UUID) from
  `POST $COORD_HTTP_URL/coord/work-units/upsert` with the parent plan stem as
  `slug` (capture the returned `work_unit_id`; or the device-authed
  `GET /coord/agent-work-units/<slug>` — the operator `GET /coord/work-units/<slug>`
  403s a device JWT);
  `phase_name` from the relevant phase/section heading. Anchor = (work_unit_id,
  phase_name). The `unit_ready`/`unit_status` predicates carry this UUID, not the
  slug.
- **Register:** prefer MCP `coord_register_gate` (kinds: `pr_merged`,
  `deploy_healthy`, `claim_terminal`, `operator_approval`, `ci_green`,
  `ref_exists`, `metric_threshold`, `time_elapsed`, `unit_ready`,
  `migration_at_head`, `infra_drift_clear`, `file_exists`, `sql_count`,
  `unit_status`, `gate_cleared`, `commit_live`; plus — **exception cases only,
  see the Continuation bullet below** — an optional typed `continuation` or legacy
  `continuation_prompt`). **HTTP fallback** when MCP is unavailable — for a
  plan-anchored gate it is now TWO device-authed calls on coord's `require_jwt`
  sub-router (device/agent/service JWT all work): (1)
  `POST $COORD_HTTP_URL/coord/work-units/upsert {slug, title?, status?}` →
  **capture `work_unit_id`**; (2)
  `POST $COORD_HTTP_URL/coord/work-units/<slug>/register-gate`
  `{predicate, phase_name (required), continuation_spawn?, clearance_audience?, gate_class?}` —
  `register-gate` does NOT upsert (404s `work_unit_not_found` if you skip step 1).
  Reach the two routes over a held device JWT, the `/coord-mcp` proxy (injects a
  device JWT), or the acting-user-service token (`coord-acting-bearer.sh`); MCP
  `coord_register_gate` now works from a device session too. A claim-anchored gate
  (no slug) uses MCP or `POST $COORD_HTTP_URL/coord/gates/register` (default
  `https://coord.qontinui.io`). Tenant derives server-side — never pass it.
  (Canonical: `_gate-registration`.)
- **Continuation = OFF by default.** Register the gate, but **omit the
  continuation ENTIRELY — no `continuation` and no `continuation_prompt` (MCP
  `coord_register_gate`), and no `continuation_spawn` (HTTP `register-gate`)**.
  All three spellings are the SAME knob: coord materializes both MCP fields into
  the DB's `continuation_spawn` column and both spawn, so passing
  `continuation_prompt` while faithfully "omitting `continuation_spawn`" still
  produces the duplicate run. The default is **omission** (`continuation_spawn`
  NULL) — *not* the typed `{"action":"notify_only"}`, which STORES a payload and
  is a different DB state (use that only for a deliberate typed no-op). Under
  charter rule 10 ("Finish to zero") this session finishes its own follow-ups, so
  a redundant continuation queues a duplicate, parallel run of the same work (the
  concurrent-WIP clobber the coordination layer exists to prevent). Attach one
  only when the follow-up will **outlive** this session: a wait longer than rule
  10's ≲2h monitor window; this session ending WITHOUT dispatching the follow-up
  itself; an `operator_approval` / human-decision gate (unbounded in time — but
  sensitive work stays notify-only unconditionally); or a cross-session chain
  owned by a different work unit or device (**out-of-graph only** — a purely
  in-graph dependency on another work unit is a DAG edge + `metadata.dispatch`,
  not a gate). Sessions also die exogenously (usage limit, crash, reboot) — if
  you are *stopping* incomplete-because-WAITING, that is `/blocked`'s
  session-close protocol and it DOES take a continuation. **Clearance stays
  record-only:** a continuation-less gate produces no dispatch and no
  `coord.alerts` row on clear — the gate row + dashboard + `all_cleared` event
  are the clear-time signal. **Failure now alerts regardless of continuation:**
  a gate going `failed`/`misconfigured` raises a `gate_unclearable_terminal`
  alert (`misconfigured` pages critical immediately; `failed` pages warning
  after a 15-min grace), and a gate rotting open past ~7 days surfaces via the
  gate doctor / info-level non-paging alerts. And if you DO
  rely on a spawn, delivery is a live defect — continuations are being dispatched
  but never consumed, and coord's 24h pending window drops them permanently — so
  treat it as best-effort and read the gate's `continuation_consumed_outcome`
  (a **null** outcome means never claimed, which is worse than a recorded
  `spawn_failed`). (Canonical: `_gate-registration` → "Continuation policy".)
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

**Attest-on-completion (close the loop).** If a next-step item instead COMPLETES
work that a registered gate was watching (a previously-deferred follow-up now
finishes), it MUST attest that gate — otherwise an agent-fact gate rots open
until a human clicks it.

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

### Part 3: Verify

After implementing all items, run the full test suite to ensure nothing broke:

```bash
npx tsc --noEmit && npx vitest run
```

Report what was found and fixed.

## Context

$ARGUMENTS
