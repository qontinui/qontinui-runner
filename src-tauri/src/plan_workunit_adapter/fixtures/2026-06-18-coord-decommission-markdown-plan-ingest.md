# Decommission coord's markdown plan ingest + plan-specific code (P4 of the plan-decoupling program)

> **Status: DRAFT 2026-06-18.** The REMOVAL plan — deletes coord's knowledge of "plans" once the
> generic work-unit (P1), the harness adapter (P2, proven), and the orchestration generalization
> (P3) are live. Hard-gated on those. See memory `project_coord_generic_ir_execution_engine`.
> **Repo(s):** qontinui-coord (primary).

## Program context

P4 of 5 — runs **LAST**. Hard preconditions: **P1** live (work-unit API), **P2** parity-proven (the
harness adapter reproduces coord's current ingest output — else the operator's own loop goes dark),
**P3** live (gates/conductor/delivery/web no longer need plan_* ). Siblings: P1
(`...-generic-work-unit-primitive`), P2 (`...-harness-markdown-to-workunit-adapter`), P3
(`...-orchestration-generalize-off-plans`), P5 (`...-workunit-authz-graduated-trust`).

## 1. Problem

These coord components encode the operator-private plan convention and must be removed so the
shared/open-core coordinator carries no plan knowledge:

- `plan_ingest_worker.rs` — the `*.md` scanner (FS root + `qontinui-dev-notes` git mirror).
- `COORD_PLANS_ROOT_DIR` (default `D:/qontinui-root/plans/`), `COORD_PLANS_ARCHIVE_DIR`,
  `COORD_PLANS_ARCHIVE_GIT_REPO`/`_SUBDIR` env knobs.
- `plan_registry.rs`'s `PLAN_STATUS_VOCAB` + markdown-status parsing (`match_known_status`,
  `normalize_vocab_status`) + `markdown_path`/`version_hash`/`ingested_status` columns.
- `coord.plans` / `coord.plan_status_history` tables (data migrated to `work_units` or archived).
- `plan_ingest_*` metrics and the "inert worker" alert.
- The `plan_ready`/`plan_status` shims left by P3.

## 2. Prior art / inventory

| Piece | Location | Disposition |
|---|---|---|
| Ingest worker | `qontinui-coord/src/plan_ingest_worker.rs` | DELETE (whole module + its `tokio::spawn` wiring in `main.rs`). |
| Status vocab + md parse | `plan_registry.rs` (`PLAN_STATUS_VOCAB`:182, `normalize_vocab_status`, `match_known_status`) | DELETE; `work_unit` status is opaque (P1). |
| Plan routes/MCP | `/coord/plans*` routes, `coord_*` plan MCP tools, `routes.rs`/`mcp/tools.rs` refs | DELETE or 410 (decide at vet; prefer remove since P1 supplies the generic verbs). |
| Tables | `coord.plans`, `coord.plan_status_history` (+ Phase-2 columns) | Backfill into `work_units` if any live rows matter, then DROP via migration. |
| Metrics/alert | `plan_ingest_*` series, inert-worker alert, `gate_metrics.rs` plan labels | Remove; ensure no dashboard references a dropped series (coordinate w/ P3 web work). |

> Vet note: produce the EXACT deletion inventory (grep `plan` across `qontinui-coord/src`) at /vet-plan; the 9 files touching `plan_ready`/`plan_status` were enumerated 2026-06-18 (routes, plan_registry, mcp/tools, main, gates, api/dev_overview, api/gate_routes, plan_ingest_worker, gate_metrics).

## 3. Phases

**Phase 1 — precondition gate.** Verify P1 live, P2 parity green, P3 live. Register a coord gate
chain so this plan cannot start until those hold (P2's parity proof is the load-bearing one).

**Phase 2 — stop ingestion.** Remove the `plan_ingest_worker` spawn + the env knobs + the
`qontinui-dev-notes` mirror path. Confirm the operator's loop still runs (now fed by the P2 adapter).

**Phase 3 — remove the model.** Delete `PLAN_STATUS_VOCAB`/md-parse, the plan routes/MCP tools, the
plan-specific columns; drop the plan_* gate shims from P3.

**Phase 4 — data + schema.** Backfill any live `coord.plans` rows into `work_units` (if needed),
then DROP the plan tables via migration. Self-provision the test harness for any DB tests touched
(`reference_coord_db_tests_migrator_digest_lags_web_migrations`).

**Phase 5 — sweep.** `grep -ri plan` across `qontinui-coord/src` returns only incidental matches
(comments/unrelated). No dangling env knob, metric, route, or doc references coord-side plans.

## 4. Acceptance

- coord builds + all tests green with ZERO plan-specific code/config/tables/metrics/routes.
- The operator's autonomous loop is unaffected (fed entirely by the P2 adapter via the P1 API),
  verified live for at least one full vet→implement→gate cycle.
- A fresh coord deploy with no `COORD_PLANS_*` env set behaves identically (the dev-box path default
  is gone).
- `grep -ri "plan" qontinui-coord/src` shows no load-bearing plan concept.

## 5. Risks / out-of-scope

- **Out of scope:** everything additive (P1/P2/P3) and authz (P5). This plan only removes.
- **Risk (highest in the program):** removing ingest before the adapter is solid = the operator's
  loop dies. Mitigation: the Phase-1 precondition gate hard-blocks on P2 parity; keep both running
  until then; this plan is irreversible-ish (data drop) so stage the table DROP last, behind a
  backup/backfill.
- **Risk:** an external consumer still calls `/coord/plans*`. Inventory callers (web/mobile/agents)
  before removing routes; 410 with a pointer to `/coord/work-units` for a deprecation window if any
  remain.
