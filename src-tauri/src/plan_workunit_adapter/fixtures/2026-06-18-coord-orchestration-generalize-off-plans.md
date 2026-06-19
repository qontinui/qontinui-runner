# Generalize coord orchestration off "plan" onto work-unit (P3 of the plan-decoupling program)

> **Status: VETTED 2026-06-19.** Audited every concrete claim against coord/runner/web code.
> Defects found: 4. Auto-fixed: 4. Surfaced for user: 0. Core thesis (decouple gate kinds +
> delivery + web from plan vocabulary) is sound. Key corrections: (1) the Approach-D conductor is
> **already plan-agnostic** — Phase 2 rewritten from "conductor retarget" to "generic gate
> registration surface"; (2) `plan_status` is already caller-supplied-status (only its *registration
> validation* couples to vocab) while `plan_ready` hard-codes `"vetted"` — §1 corrected; (3) plan_*
> must NOT become work-unit shims (would break all existing plan gates during overlap) — replaced
> with a shared-evaluation-core design (robustness); (4) plan is **coord + web only**, not
> coord/web/runner. Depends-On: 2026-06-18-coord-generic-work-unit-primitive.
> **Repo(s):** qontinui-coord (primary), qontinui-web (dashboard surfaces).

## Program context

P3 of 5. Depends on **P1** (`2026-06-18-coord-generic-work-unit-primitive`). Parallel with **P2**.
Should be live before **P4** removes the plan side. Siblings: P1, P2
(`...-harness-markdown-to-workunit-adapter`), P4 (`...-decommission-markdown-plan-ingest`), P5
(`...-workunit-authz-graduated-trust`).

## 1. Problem

The plan concept leaks beyond the registry into coord's orchestration brain and its read surfaces:

- **Gate kinds** encode plan semantics — but the two couple to the vocab _differently_ (vet
  2026-06-19, `gates.rs`):
  - `plan_ready` (`GatePredicate::PlanReady { plan_slug }`, `gates.rs:136`; evaluator
    `PlanReadyEvaluator`, `gates.rs:1990`) **hard-codes** the ready status: `const
    PLAN_READY_STATUS = "vetted"` (`gates.rs:1993`). This is the genuine vocab bake-in.
  - `plan_status` (`GatePredicate::PlanStatus { plan_slug, status }`, `gates.rs:222`; evaluator
    `PlanStatusEvaluator`, `gates.rs:2535`) already takes a **caller-supplied** `status` string in
    the predicate — the evaluator is vocabulary-free. Its only vocab coupling is **registration-time
    validation** against `PLAN_STATUS_VOCAB` (the gate-route upsert at `api/gate_routes.rs:362-378`
    and the MCP/registry path at `plan_registry.rs:790-792`). So `unit_status` is `plan_status`
    minus that registration validation — the predicate logic is reusable as-is.
- **The conductor / Approach-D engine** is _already_ plan-agnostic (vet 2026-06-19 — corrects the
  original premise). The Approach-D conductor (`qontinui-runner/src-tauri/src/orchestration_loop/
  conductor.rs` + `coord_gate.rs`, runner main `836edabb` / #583) orchestrates a **runner-local
  `orchestration.runs/subtasks` ledger** and registers only `ci_green` / `pr_merged` /
  `deploy_healthy` coord gates as subtask checkpoints (`coord_gate.rs` `GatePredicateSpec`) — it has
  **zero** `coord.plans` / `plan_ready` / `plan_status` / `plan_slug` reads. The fleet
  plan-lifecycle orchestrator's in-coord anchor (`plan_lifecycle.rs`,
  `project_fleet_plan_lifecycle_orchestrator` Phase 2) **does not exist yet** (dark/un-built). So
  there is no conductor "plan-anchor" to retarget — see the rewritten Phase 2.
- **The delivery verdict** (`delivery_view.rs`, coord #655) joins plan status ⋈ PR-merged ⋈ deploy.
- **Web surfaces** render "plans": the gate/rollout dashboard (`/admin/coord/gates`) and the
  digital-twin Delivery tab (`project_twin_delivery_verdict_impl`).

All of these must operate on the generic work-unit and a caller-supplied "ready" value, with no
hard-coded vocabulary.

## 2. Prior art (reuse, don't rebuild)

| Piece | Location | Notes |
|---|---|---|
| `plan_ready` predicate + evaluator | `gates.rs:136` (`PlanReady{plan_slug}`) + `gates.rs:1990` (`PlanReadyEvaluator`), `const PLAN_READY_STATUS="vetted"` `gates.rs:1993` | Add `UnitReady{work_unit_id, ready_status, phase_name}`: ready value is a **gate param**, not a const. Counts open sibling gates by `work_unit_id` (P1's third anchor), mirroring how `PlanReadyEvaluator` counts by `plan_id`. |
| `plan_status` predicate + evaluator | `gates.rs:222` (`PlanStatus{plan_slug,status}`) + `gates.rs:2535` (`PlanStatusEvaluator`) | Predicate already vocab-free → `UnitStatus{work_unit_id, status}` reuses the evaluator logic verbatim against `work_units`. The only thing to drop is the **registration-time vocab validation** (next row). |
| Vocab validation at registration | `api/gate_routes.rs:362-378` + `plan_registry.rs:790-792` (`normalize_vocab_status` / `PLAN_STATUS_VOCAB`) | `unit_*` gates must register with **no** vocab validation — the caller's status/ready value is opaque. Do NOT route unit-gate registration through `normalize_vocab_status`. |
| Gate anchoring (from P1) | `gates.rs` gate-anchor enum (`NewGate`, P1 `gates.rs:776-779`) + XOR→tri enforcement (P1 `gates.rs:851-875`); `work_unit_id` column added by P1 Phase 3 | `unit_*` gates anchor on `(work_unit_id + phase_name)`. **Hard dep on P1** — this anchor does not exist until P1 (web#623 + coord#710) lands on prod. |
| Gate metrics / dev overview | `gate_metrics.rs`, `api/dev_overview.rs`, `api/gate_routes.rs` | Generalize labels/series from plan→unit; keep old series during overlap. |
| Conductor / orchestrator | runner `orchestration_loop/conductor.rs` + `coord_gate.rs` (#583); fleet `fleet_commands.rs` (#622) | **Already plan-agnostic** (vet 2026-06-19). Conductor registers only `ci_green`/`pr_merged`/`deploy_healthy` gates over its own `orchestration` ledger — nothing to retarget. The only plan-coupled orchestration is the fleet vet→implement command bundler + its §5.4 `plan_ready` gate, which the Phase-1 shim covers. DAG-aware scheduling stays a follow-on. |
| Delivery verdict | `qontinui-coord/src/delivery_view.rs` (reads `coord.plans.status`, lines 7/67/203; coord #655) + web Delivery tab | Join `work_units` instead of `plans`; `drift_class` semantics unchanged. |
| Web gate dashboard | `qontinui-web/frontend/src/app/(app)/admin/coord/gates/*` + `digital-twin/_components/DeliveryVerdictCard.tsx` + `_hooks/useDeliveryVerdict.ts` + backend `app/api/v1/endpoints/digital_twin.py` | Render generic units (title + opaque status + metadata) instead of plan-specific fields. |

> ~~Vet note: confirm exact `gates.rs` predicate sites + the conductor's plan-anchor reads.~~
> **Resolved (vet 2026-06-19).** Predicate sites pinned above. The conductor has **no** plan-anchor
> reads — it is already plan-agnostic (see §1 + Phase 2). Net effect: this plan is **coord + web
> only**; `qontinui-runner` is NOT in scope, so the §5.4 continuation `repos` stays
> `["qontinui-coord","qontinui-web"]`.

## 3. Target design

- **`unit_ready` gate:** clears when the anchored work-unit's status equals a caller-declared
  ready value (gate param, e.g. `ready_status: "vetted"` for the operator's convention) AND every
  other gate on the same unit is cleared. No coord-side vocabulary.
- **`unit_status` gate:** clears when the unit reaches a caller-supplied status string.
- **plan_ready / plan_status keep reading `coord.plans` directly — they do NOT delegate through a
  work-unit.** ~~become thin shims delegating to the unit gates against a plan's work-unit.~~
  **Resolved (vet 2026-06-19 — robustness).** Delegating `plan_*` through "a plan's work-unit"
  assumes every plan has a `work_units` row, but P1 keeps `plans` and `work_units` as **separate
  tables** and nothing backfills a unit-per-plan until P2's harness adapter runs. A shim that
  resolves plan→unit would therefore make every existing `plan_ready`/`plan_status` gate fail-open
  to `Open` **forever** (no unit found) during the P1→P4 overlap — directly violating the
  acceptance criterion "plan_ready/plan_status still work so nothing breaks pre-P4." Instead:
  **factor the shared evaluation core** (status-equals-ready-value AND siblings-cleared, counting
  siblings by anchor id) into a generic helper parameterized by `(status source table, anchor id,
  ready value)`. `PlanReadyEvaluator`/`PlanStatusEvaluator` call it against `coord.plans` +
  `plan_id`; the new `UnitReady`/`UnitStatus` evaluators call it against `coord.work_units` +
  `work_unit_id`. Both anchors evaluate independently; P4 deletes the plan_* arms cleanly. This
  also satisfies clean-code (one evaluation core, no special cases) without a cross-table bridge.
- **Conductor + delivery + web:** read the work-unit anchor. Status is rendered as an opaque label;
  any "is this ready/shipped" judgment uses caller-supplied values or derived predicates
  (shipped = delivery verdict), never a hard-coded word.

## 4. Phases

**Phase 1 — generic gate kinds.** Extract the shared `plan_ready`/`plan_status` evaluation core
(status-equals-ready-value AND siblings-cleared, sibling count by anchor id) into a generic helper;
add `GatePredicate::UnitReady{work_unit_id, ready_status, phase_name}` and
`UnitStatus{work_unit_id, status}` that call it against `coord.work_units` + `work_unit_id` (P1's
third anchor). `plan_*` keep calling the same core against `coord.plans` + `plan_id` — **not** as
work-unit shims (see §3 robustness resolution). Register `unit_*` gates with **no**
`PLAN_STATUS_VOCAB` validation (do not route through `normalize_vocab_status`, `gate_routes.rs:362`
/ `plan_registry.rs:790`). Lockstep tests mirroring the existing `plan_ready_*` / `plan_status_*`
gate tests in `gates.rs` (e.g. `plan_ready_verdict_non_vetted_names_status`, `gates.rs:6384`).

**Phase 2 — generic gate registration surface (NOT a conductor retarget).** The Approach-D
conductor is already plan-agnostic (§1) — there is nothing to retarget there. The actual work is
making the `unit_*` gates registrable through the same surfaces `plan_*` use: the
`coord_register_gate` MCP tool (`mcp/tools.rs`) and the device-authed `register-gate` HTTP route,
accepting the `(work_unit_id + phase_name)` anchor P1 added. The fleet vet→implement orchestrator
(`fleet_commands.rs` #622) and its §5.4 `plan_ready` gate continue to work via the Phase-1 plan_*
core unchanged; no runner change is required. (DAG-aware scheduling over `metadata.depends_on`
remains a follow-on, out of scope.)

**Phase 3 — delivery verdict.** `delivery_view.rs` joins `work_units`; keep the
`shipped_but_unmerged` / `merged_but_unstamped` drift classes. Regen OpenAPI snapshots if the web
proxy route shape changes.

**Phase 4 — web surfaces.** Gate dashboard + Delivery tab render generic units. ruff/format + OpenAPI
snapshot gates (the `project_twin_delivery_verdict_impl` CI gotchas).

## 5. Acceptance

- A `unit_ready` gate clears on a work-unit with no coord-side vocabulary, parameterized by a
  caller-supplied ready value.
- Conductor drives work off the work-unit anchor; delivery verdict + dashboards render generic units.
- `plan_ready`/`plan_status` still work (as shims) so nothing breaks pre-P4.
- Web CI (ruff format, OpenAPI snapshots) green.

## 6. Risks / out-of-scope

- **Out of scope:** the work-unit primitive (P1), the markdown adapter (P2), deleting plan code (P4),
  authz (P5), and the DAG-scheduling conductor upgrade (follow-on).
- ~~**Risk:** the conductor lives partly in the runner — coordinate the runner-side retarget.~~
  **Retired (vet 2026-06-19):** the conductor needs no retarget (already plan-agnostic), so there is
  no runner-side change to sequence. This plan is coord + web only.
- **Hard dependency on P1:** `unit_*` gates anchor on the `work_unit_id` gate column + `coord.work_units`
  table that P1 (web#623 + coord#710) adds. P1 is IMPLEMENTED but its PRs are not yet on `main` /
  applied to prod RDS. Do not start Phase 1 until P1 has landed and the migrations are confirmed
  applied (P1 carries `coord:blocked` until then). A `migration_at_head`/`pr_merged` gate on P1 is the
  right trigger.
- **Risk:** dashboard/delivery consumers (mobile, `project_mobile_coord_first_ia_shipped`) read plan
  fields — inventory + dual-render during overlap.
