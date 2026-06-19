# coord generic work-unit primitive + API (P1 of the plan-decoupling program)

> **Status: IMPLEMENTED 2026-06-19 — PRs open, not yet on main.** All 4 phases built +
> tested green on branch `agent/workunit-primitive-p1`. **qontinui-web#623** (migrations
> `coord_workunits_01_work_units` + `coord_workunits_02_gate_anchor`, commit `6a4ec551`) and
> **qontinui-coord#710** (code, commit `9e13860`). Coord #710 carries
> `coord:downstream-of=qontinui/qontinui-web#623` (merge web first) **+ `coord:blocked`** held
> until the migrations are confirmed APPLIED to prod RDS (downstream-of orders merge, not
> migration-apply — without the block coord crash-loops on the `require_table` boot gate).
> Coord tests: opaque-status/no-422, history emission, stale-CAS reject, cross-tenant 409,
> work-unit gate round-trip, both-anchor + partial-anchor rejection (16 work_unit + 3 anchor +
> 139 mcp, all green). **Flip to SHIPPED + archive to dev-notes once both land on origin/main.**
> Started from VETTED 2026-06-19 (5 defects auto-fixed; core thesis — generalize
> `plan_registry.rs` → `work_unit_registry.rs` — confirmed sound).
> Authored from the strategic decision that a "plan" is operator-private and
> coord must NOT encode it (see memory
> `project_plans_operator_private_coord_generic_workunit` /
> `project_coord_generic_ir_execution_engine`). Foundation plan — P2/P3/P4/P5 depend on this.
> **Repo(s):** qontinui-coord (code) **+ qontinui-web** (alembic migrations — coord
> schema is alembic-authored there, NOT in coord; see §2 + Phase 1).

## Program context

This is **P1 of 5** in the "coord stops knowing about plans" migration. The program:

1. **P1 (this) — generic work-unit primitive + API.** The additive foundation: a
   `coord.work_units` anchor coord does NOT semantically interpret, plus an authz'd
   upsert/transition/list API and gate-anchoring on units. Ships first, breaks nothing.
2. **P2 — harness markdown→work-unit adapter** (`2026-06-18-harness-markdown-to-workunit-adapter`):
   move the markdown parse OUT of coord INTO the operator harness, richer than today.
3. **P3 — generalize orchestration off "plan"** (`2026-06-18-coord-orchestration-generalize-off-plans`):
   `plan_ready`/`plan_status` gate kinds, conductor, delivery verdict, web surfaces → work-unit.
4. **P4 — decommission coord markdown ingest** (`2026-06-18-coord-decommission-markdown-plan-ingest`):
   delete `plan_ingest_worker`, `PLAN_STATUS_VOCAB`, `COORD_PLANS_ROOT_DIR`/archive/dev-notes mirror.
   Gated on P1+P2+P3 live and P2 proven.
5. **P5 — work-unit authz / graduated trust** (`2026-06-18-coord-workunit-authz-graduated-trust`):
   retire the operator write-wall; evidence-based + separation-of-duties authz. Independent enhancement on P1's API.

Sequencing: **P1 → (P2 ∥ P3) → P4**; **P5** any time after P1.

## 1. Problem

coord's shared, open-core coordinator hard-codes an operator-private concept. `coord.plans`
(`plan_registry.rs`; the table was created by alembic revision `consolidation_phase2_v_28`
and the markdown columns `markdown_path`/`version_hash`/`status`/`ingested_status` were added
by revision `coord_plans` — see plan_registry.rs:14-23) is a **mirror of operator markdown** — a thin, lossy projection
(slug + a status string from `PLAN_STATUS_VOCAB`) of a rich document. Other qontinui users
will create/run work their own way or not have "plans" at all, so the coordinator cannot be
plan-shaped. We need a **generic anchor** coord coordinates over without interpreting, populated
by *any* front-end via API — never by reading a disk or a git repo.

## 2. Prior art (reuse, don't rebuild)

| Piece | Location | Notes |
|---|---|---|
| Existing plan model + routes | `qontinui-coord/src/plan_registry.rs` (`PlanRow`:73, `UpsertRequest`:122, `TransitionRequest`:157, `/coord/plans*` routes) | The new API is a **generalization** of this exact shape — keep the upsert/transition/history/list verbs, drop the markdown/vocab specifics. |
| Status history table | `coord.plan_status_history` (+ `StatusHistoryRow`:89) | Generalize to `work_unit_status_history`; the transition-record mechanism is reused verbatim. |
| Gate anchoring | `gates.rs` — gates anchor on `(plan_id + phase_name)` OR `(claim_kind + resource_key)` | Add `(work_unit_id + phase_name)` as a third anchor; do NOT rip out plan anchoring yet (P3/P4 retire it). |
| Tenant resolution + JWT sub-router | the `require_jwt` sub-router used by the device-authed gate registration (coord #650) | The work-unit write API mounts here; tenant from session claims, not args. Minimal authz is conservative tenant-scoped fail-closed (P5 evolves it). |
| Cross-tenant slug guard | the `coord.plans.slug` global-uniqueness 409 guard | `work_units.slug` carries the same global-unique + cross-tenant 409 guard. Pattern at `plan_registry.rs:256` (`idx_plans_slug` partial UNIQUE) + `post_upsert` cross-tenant reject at `plan_registry.rs:747-758`; test at `:1610` (`post_upsert_rejects_cross_tenant_slug_with_409`). |
| **Alembic schema authoring** | `qontinui-web/backend/alembic/versions/` (e.g. `coord_plans.py`) | **coord does NOT author its own schema** — alembic in qontinui-web is the sole author; Rust no longer self-heals (`plan_registry.rs:14-23,37-42`). The new `coord.work_units` migration MUST live in qontinui-web, making P1 a **two-repo change**. |
| **Boot `require_table` gate** | `main.rs:809` (`ALEMBIC_OWNED_TABLES`, list at `:733-807`) | Every alembic-owned coord table coord DMLs against is boot-asserted; if work_units joins it, the HARD deploy-order rule binds (web migration applied to prod RDS **before** the coord image — see Phase 1). |
| `coord.work_plans` (NOT the same thing) | `work_plans.rs` (alembic `coord_singleauthored_08_work_plans.py`) | **Naming proximity only** — `work_plans` is the PROPOSE→APPROVE intent→multi-repo-session orchestration backend, *not* a generic status anchor (`work_plans.rs:1-12`). Does not duplicate `work_units`. Its `ensure_work_plans_table` test fixture (`work_plans.rs:49-55`) is a **second template** (alongside `plan_registry.rs::create_plans_for_test`:1169) for the Phase 1 self-provision step. |

> ~~Vet note: confirm exact line numbers + the migration revision name during /vet-plan.~~
> **Resolved (vet 2026-06-19).** All four §2 line citations confirmed exact at coord HEAD
> (`PlanRow`:73, `StatusHistoryRow`:89, `UpsertRequest`:122, `TransitionRequest`:157; gate
> anchor enum gates.rs:776-779 + XOR enforcement gates.rs:851-875). The coord.plans migration
> revision is **`consolidation_phase2_v_28`** (created the table) **+ `coord_plans`** (added the
> markdown/status columns). Naming-collision note: `work_unit_id` already appears in
> `coord_report_status` (mcp/tools.rs:655) as a free-text label — unrelated to this table.

## 3. Target design

A `coord.work_units` row: `id` (uuid) / `slug` (globally unique, tenant-scoped 409) / `tenant_id`
/ `status` (**opaque caller-supplied string — coord does NOT validate against a vocab**) /
`title` (human label, optional) / `metadata` (jsonb — arbitrary, e.g. a link back to the source
artifact, phase list, dependency hints) / `created_at` / `updated_at`. No `markdown_path`, no
`version_hash`, no `ingested_status`.

API (on the `require_jwt` sub-router; tenant from claims):
- `POST /coord/work-units/upsert` — create/update by slug; emits a history row on status change.
- `POST /coord/work-units/:slug/transition` — guarded status transition (optional `from_status` CAS).
- `GET  /coord/work-units` — list with slug/status/tenant filters.
- `GET  /coord/work-units/:slug/history` — status history.

Gate anchoring: `coord_register_gate` accepts `work_unit_id + phase_name`. Generic `unit_ready`
gate (clears when status == a caller-declared ready value AND all sibling gates cleared) is added
in P3; P1 only needs the anchor + plain gate attachment.

`status` is **uninterpreted** by coord: coord stores it, history-tracks it, and lets gates compare
it to caller-supplied values. No `draft`/`vetted`/`shipped` knowledge anywhere.

## 4. Phases

**Phase 1 — schema (additive). This is a two-repo change.** The migration is authored in
**`qontinui-web/backend/alembic/versions/`** (alembic is the SOLE author of `coord.*` schema;
coord does not self-heal — `plan_registry.rs:14-23,37-42`). New revision creates `coord.work_units`
+ `coord.work_unit_status_history` (+ indexes; partial-UNIQUE `slug` index mirroring
`idx_plans_slug`). Leave `coord.plans` untouched (P4 removes it).
- **Boot gate (decided — robustness).** Add `"work_units"` + `"work_unit_status_history"` to the
  `ALEMBIC_OWNED_TABLES` boot list (`main.rs:733-807`, asserted at `:809`) so coord fail-fasts at
  boot if the migration hasn't run, rather than NPE-ing on the first DML — the same posture as every
  sibling alembic-owned table. **This makes the deploy-order HARD RULE binding:** the web migration
  MUST be confirmed applied to prod RDS **before** the coord image carrying P1 deploys, or coord
  crash-loops at boot (cf. `migration_reservations` / `release_observations` comments at `main.rs:740-745`).
  Sequence the merge with the `coord:downstream-of` label on the coord PR + a `migration_at_head` gate —
  the label alone is insufficient (`reference_coord_downstream_label_insufficient_for_schema_deploy_order`;
  first-party migration-drift gate machinery is live per `project_first_party_migration_drift_shipped`).
- **Test fixture.** Self-provision both tables in an in-crate `#[cfg(test)]` DB helper
  (`CREATE TABLE/INDEX IF NOT EXISTS`, race-tolerant), mirroring `plan_registry.rs::create_plans_for_test`:1169
  and `work_plans.rs::ensure_work_plans_table`:49-55 — NOT bumping the migrator digest (the
  migrator-digest-lag gotcha, `reference_coord_db_tests_migrator_digest_lags_web_migrations`).

**Phase 2 — model + API.** `work_unit_registry.rs` mirroring `plan_registry.rs` minus vocab/markdown:
`WorkUnitRow`, `UpsertRequest`, `TransitionRequest`, the 4 routes, conservative tenant-scoped authz,
the global-unique-slug 409 guard. In-crate `#[cfg(test)]` DB tests (coord is binary-only → `--bins`).

**Phase 3 — gate anchor. Also a two-repo change (new gates column).** Today a gate carries
**exactly one of two** anchors — `(claim_kind+resource_key)` XOR `(plan_id+phase_name)` — enforced
at `gates.rs:851-875`. Adding `(work_unit_id+phase_name)` as a third option requires:
- **Schema:** a new nullable `work_unit_id uuid` column on `coord.gates` → **another alembic migration
  in qontinui-web** (Phase 3 originally omitted this). `coord.gates` is already boot-gated
  (`main.rs:681`), so the same deploy-order rule from Phase 1 applies.
- **Code:** extend `NewGate` (`gates.rs:776-779`) with `work_unit_id`; widen the XOR enforcement
  (`gates.rs:851-875`) from "exactly one of TWO" to "exactly one of THREE" (this is the §6 "anchoring
  to both" guard, made concrete); add the column to the INSERT/SELECT column lists
  (`gates.rs:900,989,1081,1115`).
- **Surface:** `coord_register_gate` MCP tool (`mcp/tools.rs:785`) + the HTTP register-gate route accept
  the new anchor; gates fire/clear against it exactly like the plan anchor.
- Lockstep test mirroring the plan-anchor tests.

**Phase 4 — MCP surface.** Add `coord_*` MCP tools for work-unit upsert/transition/list (the agent
+ harness write path). Tenant from session.

## 5. Acceptance

- A work-unit can be created/transitioned/listed via API + MCP with a tenant-scoped JWT; status is
  any string (no 422 on an unknown vocab word).
- A gate anchors to a work-unit and clears identically to a plan-anchored gate.
- Zero behavior change to `coord.plans` / ingest / existing gates (additive only) — proven by the
  existing plan suite staying green.
- New in-crate DB tests green on CI (self-provisioned schema).

## 6. Risks / out-of-scope

- **Out of scope:** the markdown parser (P2), gate-kind renames + conductor/delivery/web (P3),
  removing `coord.plans`/ingest (P4), the graduated-trust authz model (P5). P1 ships with minimal
  conservative authz only.
- **Risk:** double anchor (plan_id ∥ work_unit_id) transiently coexist — fine, P4 removes the plan
  side after the harness cuts over. Guard against a gate accidentally anchoring to both.
- ~~**Risk:** slug global-uniqueness collision with existing `coord.plans` slugs during the overlap —
  decide namespacing during vet.~~ **Resolved (vet 2026-06-19 — clean code).** `work_units` is a
  separate table from `plans`, so `work_units.slug`'s UNIQUE index is independent of
  `plans.slug`'s — they do NOT share a slug space and there is no shared join. A work-unit and a plan
  may carry the same slug with zero collision. **No namespacing prefix needed**; adding one would be a
  special-case for a problem the separate-table design already eliminates. (The per-table cross-tenant
  409 guard still applies within `work_units` itself.)
