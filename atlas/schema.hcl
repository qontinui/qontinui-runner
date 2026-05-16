// Atlas-managed schema for runner-Rust-owned PG objects.
//
// Source of truth for the table/index/FK definitions the runner's
// Rust code used to create imperatively via `database/pg/mod.rs::PgDb::new`
// (regression_* tables) and `database/pg/coordinator_shadow_decisions.rs::
// ensure_shadow_decisions_table` (coord.coordinator_shadow_decisions).
//
// Row 3 schema-half pilot per
// `plans/2026-05-14-branch-per-agent-bottlenecks-tracker.md` Row 3 schema-half.
// Atlas Community edition is the target; PG extensions (vector, pgcrypto)
// stay imperatively bootstrapped in mod.rs::PgDb::new — that's the
// idiomatic bootstrap-vs-schema split.
//
// Tables ALSO covered by historical alembic migrations
// (project.regression_* by f9d3e8a4c1b6, coord.coordinator_shadow_decisions
// implicit via earlier coord migrations). The alembic files stay in
// `qontinui-web/backend/alembic/versions/` as frozen history; new alembic
// autogenerate runs exclude these tables via the env.py include_object
// filter so the two systems can't drift.
//
// To apply against live PG:
//   docker run --rm --network host -v "${PWD}:/workspace" -w /workspace \
//     arigaio/atlas:latest schema apply \
//     --url 'postgres://qontinui_user:PASSWORD@localhost:5433/qontinui_db?sslmode=disable' \
//     --to file:///workspace/atlas/schema.hcl \
//     --schema project --schema coord \
//     --dev-url 'docker://postgres/16/dev'
//
// To verify zero-diff:
//   docker run --rm --network host -v "${PWD}:/workspace" -w /workspace \
//     arigaio/atlas:latest schema diff \
//     --from 'postgres://...?sslmode=disable' \
//     --to file:///workspace/atlas/schema.hcl \
//     --schema project --schema coord \
//     --dev-url 'docker://postgres/16/dev'

schema "project" {}
schema "coord" {}

// ---------------------------------------------------------------
// project.regression_* — UI Bridge regression substrate (Section 11 / Phase A2)
// ---------------------------------------------------------------

table "regression_suites" {
  schema = schema.project
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
  schema = schema.project
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
  schema = schema.project
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
  schema = schema.project
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
// project.spec_proposals — Stream E (Flywheel) coverage-growth queue.
// Stores `fullPage` and `patch` proposals discovered by
// `/spec/proposals/scan`; lifecycle is driven by the supervisor cron + the
// `/spec/proposals/{id}/execute` handler.
// ---------------------------------------------------------------

table "spec_proposals" {
  schema = schema.project
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
// project.proposal_events — Plan 06 Step 6 (G.6) flywheel observability.
// Append-only log of state transitions on spec_proposals rows. Written
// alongside the corresponding SpecApiEvent broadcast (Plan 06 Step 2).
// Decouples durable history from broadcast; a subscriber that drops events
// still gets full history from this table.
// ---------------------------------------------------------------

table "proposal_events" {
  schema = schema.project
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
}

// ---------------------------------------------------------------
// coord.coordinator_shadow_decisions — soak comparison of shadow vs live
// scheduler decisions. Created by Rust ensure_shadow_decisions_table; this
// HCL is now source of truth.
// ---------------------------------------------------------------

table "coordinator_shadow_decisions" {
  schema = schema.coord
  column "id" {
    null    = false
    type    = uuid
    default = sql("gen_random_uuid()")
  }
  column "instance_id" {
    null = false
    type = text
  }
  column "iteration" {
    null = false
    type = bigint
  }
  column "observation_hash" {
    null = false
    type = text
  }
  column "rule" {
    null = false
    type = text
  }
  column "action" {
    null = false
    type = text
  }
  column "target_id" {
    null = true
    type = text
  }
  column "reasoning" {
    null = false
    type = text
  }
  column "would_have_acted" {
    null = false
    type = boolean
  }
  column "taken_at" {
    null    = false
    type    = timestamptz
    default = sql("now()")
  }
  primary_key {
    columns = [column.id]
  }
  index "idx_csd_taken_at" {
    on {
      column = column.taken_at
      desc   = true
    }
  }
  index "idx_csd_obs_hash" {
    columns = [column.observation_hash]
  }
  index "idx_csd_instance" {
    on {
      column = column.instance_id
    }
    on {
      column = column.taken_at
      desc   = true
    }
  }
}
