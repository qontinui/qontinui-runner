# Harness markdown→work-unit adapter (P2 of the plan-decoupling program)

> **Status: IN PROGRESS 2026-06-19.** Implementing Phase 1 only (the pure markdown→work-unit
> parser + golden tests in qontinui-runner) — fully unblocked. Phases 2–4 are deferred behind a
> coord gate: P1's work-unit API (coord#710 + web#623) is IMPLEMENTED but NOT on `origin/main`
> (both PRs OPEN; coord#710 `coord:blocked` pending prod-RDS migration apply), so the push client
> and parity proof cannot be end-to-end verified yet. History: Started from VETTED 2026-06-19 (4
> defects auto-fixed). Operator decision 2026-06-19: implement unblocked Phase 1, gate Phases 2–4
> on P1 landing on coord main (`resolve-plan-deps.py` false-positived SHIPPED off a "Flip to
> SHIPPED" sentence; verified not-on-main). Depends-On: 2026-06-18-coord-generic-work-unit-primitive.
> **Repo(s):** qontinui-runner (primary — push mechanism + trigger) and qontinui-claude-config
> (the operator-private markdown parser/convention config).

## Program context

P2 of 5. Depends on **P1** (`2026-06-18-coord-generic-work-unit-primitive`) — the work-unit API
must exist to push to. Runs in parallel with **P3**. Must be **proven** before **P4** deletes
coord's ingest worker (else the operator's own autonomous loop goes dark). Siblings: P1, P3
(`...-orchestration-generalize-off-plans`), P4 (`...-decommission-markdown-plan-ingest`), P5
(`...-workunit-authz-graduated-trust`).

## Discovered prior art (added during /vet-plan)

P1's work-unit API is **already built** (branch `agent/workunit-primitive-p1`, coord#710 +
web#623 — IMPLEMENTED, not yet on main). Phase 2 should target these concrete endpoints, not an
abstract "P1 upsert/transition":

| Piece | Location | Notes |
|---|---|---|
| P1 push target (routes) | `qontinui-coord/src/routes.rs:411-431` (branch `agent/workunit-primitive-p1`) | `POST /coord/work-units/upsert`, `POST /coord/work-units/:slug/transition`, `GET /coord/work-units`, `GET /coord/work-units/:slug/history`. All under `require_jwt` — tenant + identity lifted from the verified device-JWT `AuthContext`, **never** from arguments. |
| Registry impl | `qontinui-coord/src/work_unit_registry.rs` (`post_upsert`/`post_transition`/`get_list`/`get_history`, ~1367 lines) | Slug-keyed; status is **OPAQUE** (stored verbatim, no vocab, no 422); stale-CAS reject; cross-tenant slug-collision = the lone 409; history rows on transition. |
| Parser/edge-trigger/metrics to PORT | `qontinui-coord/src/plan_ingest_worker.rs` | The thing being moved out. Re-use its `decide_status_action` edge-trigger semantics (#564), the `ingested_status` last-applied memory (now client-side), and the `coord_plan_ingest_*` metric names. |
| Vocabulary to DROP | `qontinui-coord/src/plan_registry.rs:182` `PLAN_STATUS_VOCAB` | The old parse normalized status against this fixed vocab. P1 work-unit status is opaque, so the adapter ports the parse but **drops** vocab normalization (free-text status flows straight through). |

**Consequence for sequencing:** Phase 1 (pure parser + golden tests) is fully implementable now
with zero P1 dependency. Phases 2–4 can be *written* against the contract above but cannot be
end-to-end verified until P1 lands on `origin/main` and deploys to prod (or against a local coord
build of the P1 branch). The Phase-4 parity proof in particular needs both adapters live.

## 1. Problem

Today coord auto-discovers plans by scanning `COORD_PLANS_ROOT_DIR` (default `D:/qontinui-root/plans/`)
and a `qontinui-dev-notes` git mirror, parsing each `*.md` into a `coord.plans` row
(`plan_ingest_worker.rs`). This is operator-specific (a dev-box path defaulted in a cloud service)
and **lossy** — it extracts only slug + a `PLAN_STATUS_VOCAB` status, discarding the phase
structure, work-items, and dependencies that are the actual value of a plan.

We are deliberately removing this from coord (P4). To **preserve frictionless ambient authoring**
("the markdown I'd write anyway IS the coordination object"), the parse moves into the operator
harness — where it can be as opinionated and rich as we like without coupling or endangering the
shared plane. This is the same rule as
`feedback_fleet_wide_capabilities_belong_in_runner_not_claude_config`, applied inversely:
operator-specific ingest belongs in the operator harness, NOT coord.

## 2. The source→IR reframe

Markdown plan = **source**; the work-unit (DAG) = **IR**; coord = the **runtime** that executes IR.
coord must not parse source. The adapter is the compiler front-end: operator markdown → structured
work-unit(s) via P1's API. Other users bring other front-ends (Linear/Jira/GitHub-Projects sync, a
UI, an LLM reading a freeform doc, or nothing) — none of which coord needs to know about.

## 3. Target design

A harness component (decision below) that, on a trigger, reads the operator's `plans/` markdown and
**pushes structured work-units to coord's API** (P1):

- **Richer than coord's parse.** Extract not just slug+status but the phase structure → sub-units
  (or `metadata.phases`), declared dependencies / sibling-plan edges (the "Program context" links
  in these very plans) → `metadata.depends_on`, and a back-link to the source file as provenance.
- **Edge-triggered + idempotent.** Re-running yields no spurious transitions (carry the
  last-applied status, mirroring the old `ingested_status` edge-trigger, issue #564 — but now
  client-side). A status change in the file → one `transition` call.
- **Conflict posture.** The file remains the operator's source of truth; a direct API transition
  (e.g. by an agent) that diverges from the file is surfaced LOUDLY (warn/metric) the way the old
  worker did, but resolution policy lives in the harness now.

### Decisions (resolved during /vet-plan)
- **Home — RESOLVED: runner hosts the push mechanism + trigger; the markdown-parsing convention is
  a claude-config-supplied parser/config the runner invokes.** A pure claude-config hook fires only
  on the operator's machine and dies with the session, so it can never push a plan edited while no
  TUI is open — exactly the closed-session delivery gap the session-bus preamble was moved to the
  runner to close (`feedback_fleet_wide_capabilities_belong_in_runner_not_claude_config`; runner
  `fleet_commands.rs` / `provision_agent_definitions` are the established home for this kind of
  fleet infra). Keeping the *mechanism* (scan → diff → push, idempotency memory, metrics) generic
  in the runner and the operator-private *markdown convention* as config preserves the
  mechanism/policy split: the runner stays fleet-portable while operator policy never pollutes
  fleet code. (Decided by **scalability** — survives closed sessions and generalizes to other
  front-ends; clean mechanism/policy separation breaks the tie.)
- **Trigger — RESOLVED: periodic reconcile scan (mirror the existing `plan_ingest_worker` ~60s
  tick), optionally nudged by an explicit `/sync-plans` for instant sync.** A filesystem watch or
  on-edit hook is lower-latency but misses every edit made while the runner is down and needs a
  reconcile scan anyway to be correct; the edge-triggered/idempotent design (last-applied status
  memory) is built precisely so re-scanning is free of phantom transitions. (Decided by
  **robustness** — no missed edits across downtime/closed sessions; it is the model coord's own
  proven ingest already uses.)

## 4. Phases

**Phase 1 — parser (pure, unit-tested).** Port + enrich the markdown parse: slug from filename,
status from the stamp, PLUS phases→sub-units and dependency edges. Pure function, golden-file tests
over the real `plans/` corpus (it's a great fixture set — these 5 plans included).

**Phase 2 — push client.** Call P1's `upsert`/`transition` with a tenant-scoped JWT; idempotent,
edge-triggered, last-applied-status memory; loud conflict surfacing.

**Phase 3 — wire-up + trigger.** Mount in the chosen home with the chosen trigger; metrics
(scanned/transitions/conflicts) mirroring `coord_plan_ingest_*` so the operator keeps the same
observability.

**Phase 4 — parity proof (gates P4).** Run adapter + coord's old ingest **side by side** over the
live `plans/` corpus and assert the resulting work-units match the plans coord would have produced
(slug+status parity at minimum; structure is bonus). This green light is P4's precondition.

## 5. Acceptance

- Editing a plan markdown file results in a corresponding coord work-unit upsert/transition via the
  API, with zero manual steps (ambient authoring preserved).
- Re-sync is idempotent (no phantom transitions on an unchanged file).
- Parity run matches coord's current ingest output for the existing `plans/` corpus → P4 unblocked.
- Parser tests green over the real corpus.

## 6. Risks / out-of-scope

- **Out of scope:** the coord API itself (P1), removing coord's ingest (P4), gate/conductor/web
  generalization (P3).
- **Risk:** cutover gap — if the adapter is buggy when P4 removes coord's ingest, the loop stops.
  Mitigation: the Phase-4 parity proof is a hard gate on P4; keep coord's ingest running until
  parity is green (the two coexist harmlessly — both just upsert).
- **Risk: secret/auth for the push — RESOLVED.** P1's work-unit routes sit behind the SAME
  `require_jwt` device/agent-JWT posture as the device-authed gate-registration path
  (`routes.rs:411-431`; tenant lifted from claims, never args), so the harness's existing device
  JWT is sufficient — no new auth surface. **One concrete wiring gap to note:** the runner's
  loopback write-forwarder currently exposes only `register-plan` + `attest`
  (`2026-06-15-coord-mcp-live-token-write-forwarder`), so reaching `/coord/work-units/*` over the
  proxy needs EITHER a raw device JWT minted in the runner step (it runs in-process, so it can) OR
  a small new forwarder route (`/coord-mcp/work-units/*`) injecting the device JWT. Prefer the
  raw-JWT-in-runner path — the runner step is server-side, not a proxy-shaped agent session, so it
  can hold the JWT directly without the forwarder indirection.
