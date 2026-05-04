--- UI Bridge regression suites: persistence shell for the pure `diagnose`
--- function in `ui-bridge-auto`. Stores suite definitions, run summaries,
--- self-diagnosis memos, and a per-assertion exercise log used by the
--- Phase C2 panel to reconstruct case/assertion timelines.
---
--- IMPORTANT: At the time these queries were authored, the regression_*
--- tables had not yet been added to the alembic chain that produces
--- `schema.pg.sql.generated`. Until that migration lands and the schema
--- file is regenerated, Clorinde will reject these definitions. The
--- runner-side wrapper in `src/database/pg/regression.rs` therefore uses
--- raw `tokio_postgres` queries directly (mirroring `ui_bridge_baselines`).
--- These query definitions are kept in source so the Clorinde-generated
--- bindings can be wired in without rewriting the SQL once the schema
--- catches up. Clorinde's codegen step may silently drop this file if it
--- runs before the migration; that is expected.

--! save_regression_suite
INSERT INTO regression_suites (id, ir_doc_id, suite_json)
VALUES (:id::uuid, :ir_doc_id, :suite_json::jsonb)
RETURNING id;

--! record_regression_run
INSERT INTO regression_runs (
    id, suite_id, run_id, passed, failed, started_at, completed_at, run_result_json
) VALUES (
    :id::uuid, :suite_id::uuid, :run_id, :passed, :failed,
    :started_at::timestamptz, :completed_at::timestamptz, :run_result_json::jsonb
)
RETURNING id;

--! record_regression_diagnosis
INSERT INTO regression_diagnoses (id, run_id, diagnosis_json)
VALUES (:id::uuid, :run_id::uuid, :diagnosis_json::jsonb)
RETURNING id;

--! record_assertion_executions_batch
-- Single-batch insert that explodes a JSON array of execution rows into
-- regression_assertion_executions. Each element of `executions_json` must
-- have the keys: id, run_id, case_id, assertion_id, assertion_kind, status,
-- started_at, duration_ms, and the optional failure_kind / failure_evidence_json /
-- error_message.
INSERT INTO regression_assertion_executions (
    id, run_id, case_id, assertion_id, assertion_kind, status,
    started_at, duration_ms, failure_kind, failure_evidence_json, error_message
)
SELECT
    (e->>'id')::uuid,
    (e->>'run_id')::uuid,
    e->>'case_id',
    e->>'assertion_id',
    e->>'assertion_kind',
    e->>'status',
    (e->>'started_at')::timestamptz,
    (e->>'duration_ms')::int,
    e->>'failure_kind',
    e->'failure_evidence_json',
    e->>'error_message'
FROM jsonb_array_elements(:executions_json::jsonb) AS e;

--! get_assertion_executions_for_suite
-- Joins the per-run exercise log back to its parent suite so the panel can
-- reconstruct case/assertion timelines. Returns the shape consumed by the
-- ui-bridge-auto exercise-log view: {case_id, assertion_id, started_at}.
SELECT
    ae.case_id,
    ae.assertion_id,
    ae.started_at::TEXT AS started_at,
    ae.status,
    ae.assertion_kind,
    ae.duration_ms,
    ae.failure_kind,
    ae.error_message
FROM regression_assertion_executions ae
JOIN regression_runs r ON r.id = ae.run_id
WHERE r.suite_id = :suite_id::uuid
ORDER BY ae.started_at ASC;

--! get_recent_diagnoses_for_suite
-- Latest self-diagnosis memos for the runner panel. Joins through
-- regression_runs to filter by suite_id.
SELECT
    d.id,
    d.run_id,
    d.diagnosis_json,
    d.created_at::TEXT AS created_at
FROM regression_diagnoses d
JOIN regression_runs r ON r.id = d.run_id
WHERE r.suite_id = :suite_id::uuid
ORDER BY d.created_at DESC
LIMIT :limit;
