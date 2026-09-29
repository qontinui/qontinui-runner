// Atlas-managed schema for runner-Rust-owned PG objects.
//
// OWNERSHIP MODEL (plan `2026-05-14-atlas-wave-6-triage`, Phases 1-3):
// Atlas owns exactly two schemas, WHOLLY:
//
//   atlas_managed  -- the UI Bridge regression substrate (regression_*) and
//                     the flywheel queue (spec_proposals, proposal_events).
//   orchestration  -- the Approach-D conductor ledger (runs, subtasks).
//
// Every object in those two schemas must be declared in this file, and
// nothing else in them may exist. There is no exclude list: `coord`,
// `project` and every other schema are simply absent from `schemas` in
// atlas.hcl, and a schema outside `schemas` is invisible to both
// `schema diff` and `schema apply` (spike Q1-1, 2026-09-05), so no object in
// it can ever become a DROP candidate. alembic (qontinui-web) is the sole DDL
// author of `coord.*` and `project.*`, including `project.apps`; its env.py
// skips `atlas_managed` and `orchestration` at schema level so the two
// systems cannot drift into each other.
//
// Invariant, enforced by .github/workflows/atlas-schema-check.yml against a
// Postgres with the alembic chain applied: `schema apply --env runner_pilot`
// plans no DROP, a second `schema diff` plans nothing, and every table
// declared here exists afterwards. A planned DROP means a foreign object
// landed in an Atlas-owned schema; a planned CREATE on the second pass means
// a declaration Atlas cannot converge.
//
// PG extensions (vector, pgcrypto) stay imperatively bootstrapped in
// `database/pg/mod.rs` -- the idiomatic bootstrap-vs-schema split. The six
// atlas_managed tables used to live in `project`; the runner's
// `verify_and_provision` moves (or merges) any leftover `project.<t>` copy
// into `atlas_managed` on boot. No alembic revision moves them: an
// alembic-only database still carries the four legacy project.regression_*
// tables (frozen revision f9d3e8a4c1b6), so every codegen pipeline runs
// atlas/scripts/apply_to.sh after `alembic upgrade head`.
//
// Use `--env runner_pilot` (atlas.hcl) so the two schemas stay the scope.
// The dev database must have NO `public` schema, or `schema apply` aborts
// its post-apply verification after a clean diff.
//   docker run --rm --network host -v "${PWD}/atlas:/work" -w /work \
//     -e ATLAS_LIVE_URL -e ATLAS_DEV_URL \
//     arigaio/atlas:1.3.3-community schema apply --env runner_pilot --dry-run
//   docker run --rm --network host -v "${PWD}/atlas:/work" -w /work \
//     -e ATLAS_LIVE_URL -e ATLAS_DEV_URL \
//     arigaio/atlas:1.3.3-community schema apply --env runner_pilot

// UI Bridge regression substrate + flywheel queue, owned wholly by Atlas.
schema "atlas_managed" {}
// Runner-owned orchestration ledger (Approach-D Conductor/Engine, Phase 1).
// Self-healed imperatively in `database/pg/mod.rs::verify_and_provision` as
// well, so a fresh PG without Atlas applied still boots the conductor loop.
schema "orchestration" {}

// ---------------------------------------------------------------
// atlas_managed.regression_* — UI Bridge regression substrate (Section 11 / Phase A2)
// ---------------------------------------------------------------

table "regression_suites" {
  schema = schema.atlas_managed
  column "id" {
    null = false
    type = uuid
  }
  column "ir_doc_id" {
    null = false
    type = text
  }
  column "suite_json" {
    null = false
    type = jsonb
  }
  column "created_at" {
    null    = false
    type    = timestamptz
    default = sql("now()")
  }
  primary_key {
    columns = [column.id]
  }
  index "regression_suites_ir_doc_id_idx" {
    columns = [column.ir_doc_id]
  }
}

table "regression_runs" {
  schema = schema.atlas_managed
  column "id" {
    null = false
    type = uuid
  }
  column "suite_id" {
    null = false
    type = uuid
  }
  column "run_id" {
    null = false
    type = text
  }
  column "passed" {
    null = false
    type = integer
  }
  column "failed" {
    null = false
    type = integer
  }
  column "started_at" {
    null = false
    type = timestamptz
  }
  column "completed_at" {
    null = false
    type = timestamptz
  }
  column "run_result_json" {
    null = false
    type = jsonb
  }
  column "drift_report_json" {
    null = true
    type = jsonb
  }
  primary_key {
    columns = [column.id]
  }
  foreign_key "regression_runs_suite_id_fkey" {
    columns     = [column.suite_id]
    ref_columns = [table.regression_suites.column.id]
    on_delete   = CASCADE
  }
  index "regression_runs_suite_id_idx" {
    columns = [column.suite_id]
  }
  index "regression_runs_run_id_idx" {
    columns = [column.run_id]
  }
}

table "regression_diagnoses" {
  schema = schema.atlas_managed
  column "id" {
    null = false
    type = uuid
  }
  column "run_id" {
    null = false
    type = uuid
  }
  column "diagnosis_json" {
    null = false
    type = jsonb
  }
  column "created_at" {
    null    = false
    type    = timestamptz
    default = sql("now()")
  }
  primary_key {
    columns = [column.id]
  }
  foreign_key "regression_diagnoses_run_id_fkey" {
    columns     = [column.run_id]
    ref_columns = [table.regression_runs.column.id]
    on_delete   = CASCADE
  }
  index "regression_diagnoses_run_id_idx" {
    columns = [column.run_id]
  }
}

table "regression_assertion_executions" {
  schema = schema.atlas_managed
  column "id" {
    null = false
    type = uuid
  }
  column "run_id" {
    null = false
    type = uuid
  }
  column "case_id" {
    null = false
    type = text
  }
  column "assertion_id" {
    null = false
    type = text
  }
  column "assertion_kind" {
    null = false
    type = text
  }
  column "status" {
    null = false
    type = text
  }
  column "started_at" {
    null = false
    type = timestamptz
  }
  column "duration_ms" {
    null = false
    type = integer
  }
  column "failure_kind" {
    null = true
    type = text
  }
  column "failure_evidence_json" {
    null = true
    type = jsonb
  }
  column "error_message" {
    null = true
    type = text
  }
  primary_key {
    columns = [column.id]
  }
  foreign_key "regression_assertion_executions_run_id_fkey" {
    columns     = [column.run_id]
    ref_columns = [table.regression_runs.column.id]
    on_delete   = CASCADE
  }
  index "regression_assertion_executions_run_id_idx" {
    columns = [column.run_id]
  }
  index "regression_assertion_executions_case_assertion_idx" {
    on {
      column = column.case_id
    }
    on {
      column = column.assertion_id
    }
    on {
      column = column.started_at
      desc   = true
    }
  }
  index "regression_assertion_executions_kind_status_idx" {
    columns = [column.assertion_kind, column.status]
  }
  index "regression_assertion_executions_failures_idx" {
    columns = [column.case_id, column.assertion_id]
    where   = "status = 'fail'::text"
  }
}

// ---------------------------------------------------------------
// atlas_managed.spec_proposals — Stream E (Flywheel) coverage-growth queue.
// Stores `fullPage` and `patch` proposals discovered by
// `/spec/proposals/scan`; lifecycle is driven by the supervisor cron + the
// `/spec/proposals/{id}/execute` handler.
// ---------------------------------------------------------------

table "spec_proposals" {
  schema = schema.atlas_managed
  column "id" {
    null = false
    type = text
  }
  column "kind" {
    null = false
    type = text
  }
  column "pathname" {
    null = true
    type = text
  }
  column "spec_id" {
    null = true
    type = text
  }
  column "status" {
    null = false
    type = text
  }
  column "created_at" {
    null    = false
    type    = timestamptz
    default = sql("now()")
  }
  column "last_attempt_at" {
    null = true
    type = timestamptz
  }
  column "consecutive_greens" {
    null    = false
    type    = integer
    default = 0
  }
  column "last_error" {
    null = true
    type = text
  }
  column "candidate_ir" {
    null = true
    type = jsonb
  }
  column "metadata" {
    null    = false
    type    = jsonb
    default = sql("'{}'::jsonb")
  }
  primary_key {
    columns = [column.id]
  }
  check "spec_proposals_kind_chk" {
    expr = "kind IN ('fullPage', 'patch')"
  }
  // Dedup: a queued/in-flight proposal for a given target identity. Uses
  // a functional unique index over (kind, COALESCE(pathname, spec_id)) so
  // the same pathname (kind='fullPage') or spec_id (kind='patch') cannot
  // be queued twice. Insertions use ON CONFLICT DO NOTHING.
  index "spec_proposals_kind_target_uniq" {
    unique = true
    on {
      column = column.kind
    }
    on {
      expr = "COALESCE(pathname, spec_id)"
    }
  }
  index "spec_proposals_status_idx" {
    columns = [column.status]
  }
}

// ---------------------------------------------------------------
// atlas_managed.proposal_events — Plan 06 Step 6 (G.6) flywheel observability.
// Append-only log of state transitions on spec_proposals rows. Written
// alongside the corresponding SpecApiEvent broadcast (Plan 06 Step 2).
// Decouples durable history from broadcast; a subscriber that drops events
// still gets full history from this table.
// ---------------------------------------------------------------

table "proposal_events" {
  schema = schema.atlas_managed
  column "id" {
    null = false
    type = text
  }
  column "proposal_id" {
    null = false
    type = text
  }
  column "event_type" {
    null = false
    type = text
  }
  column "snapshot_id" {
    null = true
    type = text
  }
  column "failing_assertion_id" {
    null = true
    type = text
  }
  column "at" {
    null    = false
    type    = timestamptz
    default = sql("now()")
  }
  // spec-multi-app Stream E.1 — every event is scoped to a registered app.
  // The runtime self-heal in `database/pg/mod.rs` backfills legacy rows
  // under `qontinui-runner` before flipping NOT NULL.
  column "app_id" {
    null = false
    type = text
  }
  primary_key {
    columns = [column.id]
  }
  check "proposal_events_type_chk" {
    expr = "event_type IN ('scanned','executed','promoted','demoted','failed')"
  }
  index "proposal_events_proposal_at_idx" {
    columns = [column.proposal_id, column.at]
  }
  index "proposal_events_type_at_idx" {
    columns = [column.event_type, column.at]
  }
  index "idx_proposal_events_app_id" {
    columns = [column.app_id]
  }
  index "idx_proposal_events_app_id_at_ms" {
    on {
      column = column.app_id
    }
    on {
      column = column.at
      desc   = true
    }
  }
}

// ---------------------------------------------------------------
// orchestration.runs / orchestration.subtasks — Approach-D Conductor/Engine
// Phase 1 durable ledger.
//
// `runs` is one row per `/orchestrate` invocation; `subtasks` is the growing
// subtask DAG for that run. A later (Phase 3) reconciler is stateless over
// these two tables, so every transition the conductor makes must be durable
// here first.
//
// `subtasks.artifact` is a serde-serialized `CompletionReport` (see
// `database/pg/completion_reports.rs`). `subtasks.produced_by` is the
// elaborating parent task_id (null for DESIGN-origin rows) and is the
// idempotent splice key used by progressive elaboration (Phase 4).
//
// Runner-owned: ALSO self-healed imperatively in
// `database/pg/mod.rs::verify_and_provision`. No alembic migration — this is
// not a coord.* table.
// ---------------------------------------------------------------

table "runs" {
  schema = schema.orchestration
  column "run_id" {
    null = false
    type = uuid
  }
  column "goal" {
    null = false
    type = text
  }
  column "recipe" {
    null = true
    type = text
  }
  column "phases" {
    null    = false
    type    = sql("text[]")
    default = sql("'{}'")
  }
  column "status" {
    null = false
    type = text
  }
  // Why the run left `running`: the fatal error (DAG cycle, DESIGN failure),
  // the stall pattern, or the stop request. Null while `running` and for a
  // `complete` run. Written together with `status` by every terminal exit of
  // the conductor (`PgDb::set_run_status`).
  column "status_reason" {
    null = true
    type = text
  }
  // The run's `OrchestrationRunConfig`, serialized at create. The boot sweep
  // relaunches a `running` run with these knobs rather than the defaults.
  // Null for a row that predates the column (relaunched at defaults).
  column "config" {
    null = true
    type = jsonb
  }
  // The runner instance that started this run: `QONTINUI_INSTANCE_NAME`, or
  // `primary` when unset. A temp runner and the primary share one embedded PG
  // cluster, so the boot sweep relaunches only rows it owns. A NULL owner
  // predates the column and is logged and left alone, never adopted.
  column "owner_instance" {
    null = true
    type = text
  }
  column "created_at" {
    null    = false
    type    = timestamptz
    default = sql("now()")
  }
  column "updated_at" {
    null    = false
    type    = timestamptz
    default = sql("now()")
  }
  primary_key {
    columns = [column.run_id]
  }
}

table "subtasks" {
  schema = schema.orchestration
  column "task_id" {
    null = false
    type = text
  }
  column "run_id" {
    null = false
    type = uuid
  }
  column "idx" {
    null = false
    type = integer
  }
  column "title" {
    null = false
    type = text
  }
  column "brief" {
    null = false
    type = text
  }
  column "phase" {
    null = false
    type = text
  }
  column "repo" {
    null = true
    type = text
  }
  column "depends_on" {
    null    = false
    type    = sql("text[]")
    default = sql("'{}'")
  }
  column "expected_output" {
    null = false
    type = text
  }
  column "emits_subtasks" {
    null    = false
    type    = boolean
    default = false
  }
  column "state" {
    null = false
    type = text
  }
  column "task_run_id" {
    null = true
    type = uuid
  }
  column "artifact" {
    null = true
    type = jsonb
  }
  column "produced_by" {
    null = true
    type = text
  }
  // Phase 6 coord-gate association. `gate_id` is the coord gate that gates
  // this subtask's dispatch (CI-green / PR-merged / deploy-healthy); `gate_status`
  // mirrors the last-polled coord verdict (open|cleared|failed) OR the
  // runner-side typed block when the call produced no verdict at all
  // (coord_unreachable = could not ask; coord_error = coord answered and
  // refused the call). Both nullable —
  // a subtask without an observable external pre-condition carries neither. The
  // gate row is the DURABLE record a restart re-attaches to (no re-registration).
  column "gate_id" {
    null = true
    type = text
  }
  column "gate_status" {
    null = true
    type = text
  }
  // Times the boot sweep returned this row from `working` to `submitted`
  // because its worker died with the previous runner process. A row that
  // would exceed 2 is failed instead ("worker lost across 2 restarts").
  column "restart_resets" {
    null    = false
    type    = integer
    default = 0
  }
  // Why the row reached `failed`: `dependency <id> failed`, a spawn or
  // isolation refusal, a lost or silent worker. Null for a row that did not
  // fail, and for one that failed before the column existed. A failed run's
  // `status_reason` names its failed rows with these.
  column "state_reason" {
    null = true
    type = text
  }
  column "created_at" {
    null    = false
    type    = timestamptz
    default = sql("now()")
  }
  column "updated_at" {
    null    = false
    type    = timestamptz
    default = sql("now()")
  }
  primary_key {
    columns = [column.run_id, column.task_id]
  }
  foreign_key "subtasks_run_id_fkey" {
    columns     = [column.run_id]
    ref_columns = [table.runs.column.run_id]
    on_delete   = CASCADE
  }
  index "idx_orchestration_subtasks_run" {
    on {
      column = column.run_id
    }
    on {
      column = column.idx
    }
  }
  index "idx_orchestration_subtasks_produced_by" {
    columns = [column.run_id, column.produced_by]
  }
}
