//! PostgreSQL CRUD for the UI Bridge regression subsystem.
//!
//! Persistence shell for the pure `diagnose` function in `ui-bridge-auto`.
//! Stores regression suite definitions, run summaries, self-diagnosis memos,
//! and a per-assertion exercise log keyed for forensic queries.
//!
//! Tables are created on first connect by the bootstrap in
//! [`super::PgDb::new`]. They live in the active search_path
//! (`project, public`).
//!
//! Companion query file: `src-tauri/queries/regression.sql` (Clorinde-style
//! definitions kept for the day the alembic chain catches up; until then we
//! issue the queries directly via `tokio_postgres`, mirroring the
//! `ui_bridge_baselines` pattern).

use super::PgDb;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// =============================================================================
// Row types returned to the command layer
// =============================================================================

/// Per-assertion exercise log row, denormalized with failure detail.
#[derive(Debug, Serialize, Deserialize)]
pub struct AssertionExecutionRow {
    pub case_id: String,
    pub assertion_id: String,
    pub started_at: String,
    pub status: String,
    pub assertion_kind: String,
    pub duration_ms: i32,
    pub failure_kind: Option<String>,
    pub error_message: Option<String>,
}

/// Recent diagnosis memo row for the runner panel.
#[derive(Debug, Serialize, Deserialize)]
pub struct DiagnosisRow {
    pub id: String,
    pub run_id: String,
    pub diagnosis_json: serde_json::Value,
    pub created_at: String,
}

impl PgDb {
    // -------------------------------------------------------------------------
    // Writes
    // -------------------------------------------------------------------------

    /// Save a regression suite definition keyed to an IR doc.
    /// Returns the suite UUID on success.
    pub async fn save_regression_suite(
        &self,
        ir_doc_id: &str,
        suite_json: &serde_json::Value,
    ) -> Result<Uuid, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = Uuid::new_v4();
        conn.execute(
            r#"
            INSERT INTO regression_suites (id, ir_doc_id, suite_json)
            VALUES ($1, $2, $3::jsonb)
            "#,
            &[&id, &ir_doc_id, suite_json],
        )
        .await
        .map_err(|e| format!("PG save_regression_suite: {}", e))?;

        Ok(id)
    }

    /// Record a regression-run summary. Stores the full RegressionRunResult
    /// blob for ad-hoc inspection; the per-assertion table is the queryable
    /// form.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_regression_run(
        &self,
        suite_id: Uuid,
        run_id: &str,
        passed: i32,
        failed: i32,
        started_at: &str,
        completed_at: &str,
        run_result_json: &serde_json::Value,
    ) -> Result<Uuid, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = Uuid::new_v4();
        conn.execute(
            r#"
            INSERT INTO regression_runs (
                id, suite_id, run_id, passed, failed,
                started_at, completed_at, run_result_json
            ) VALUES (
                $1, $2, $3, $4, $5,
                $6::timestamptz, $7::timestamptz, $8::jsonb
            )
            "#,
            &[
                &id,
                &suite_id,
                &run_id,
                &passed,
                &failed,
                &started_at,
                &completed_at,
                run_result_json,
            ],
        )
        .await
        .map_err(|e| format!("PG record_regression_run: {}", e))?;

        Ok(id)
    }

    /// Record a self-diagnosis memo for a run.
    pub async fn record_regression_diagnosis(
        &self,
        run_id: Uuid,
        diagnosis_json: &serde_json::Value,
    ) -> Result<Uuid, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = Uuid::new_v4();
        conn.execute(
            r#"
            INSERT INTO regression_diagnoses (id, run_id, diagnosis_json)
            VALUES ($1, $2, $3::jsonb)
            "#,
            &[&id, &run_id, diagnosis_json],
        )
        .await
        .map_err(|e| format!("PG record_regression_diagnosis: {}", e))?;

        Ok(id)
    }

    /// Batch-insert per-assertion execution rows from a JSON array.
    ///
    /// Each element must have keys: `id`, `run_id`, `case_id`, `assertion_id`,
    /// `assertion_kind`, `status`, `started_at`, `duration_ms`. Optional keys:
    /// `failure_kind`, `failure_evidence_json`, `error_message`. Missing
    /// `id`s are auto-generated; missing `run_id`s cause the row to be
    /// rejected by the FK constraint.
    ///
    /// Returns the number of rows inserted.
    pub async fn record_assertion_executions_batch(
        &self,
        executions_json: &serde_json::Value,
    ) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows_affected = conn
            .execute(
                r#"
                INSERT INTO regression_assertion_executions (
                    id, run_id, case_id, assertion_id, assertion_kind, status,
                    started_at, duration_ms, failure_kind,
                    failure_evidence_json, error_message
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
                FROM jsonb_array_elements($1::jsonb) AS e
                "#,
                &[executions_json],
            )
            .await
            .map_err(|e| format!("PG record_assertion_executions_batch: {}", e))?;

        Ok(rows_affected)
    }

    // -------------------------------------------------------------------------
    // Reads
    // -------------------------------------------------------------------------

    /// Fetch every assertion execution for a suite, joined through
    /// regression_runs. Returns rows ordered by `started_at ASC` so the
    /// caller can reconstruct case/assertion timelines.
    pub async fn get_assertion_executions_for_suite(
        &self,
        suite_id: Uuid,
    ) -> Result<Vec<AssertionExecutionRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
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
                WHERE r.suite_id = $1
                ORDER BY ae.started_at ASC
                "#,
                &[&suite_id],
            )
            .await
            .map_err(|e| format!("PG get_assertion_executions_for_suite: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| AssertionExecutionRow {
                case_id: r.get(0),
                assertion_id: r.get(1),
                started_at: r.get(2),
                status: r.get(3),
                assertion_kind: r.get(4),
                duration_ms: r.get(5),
                failure_kind: r.get(6),
                error_message: r.get(7),
            })
            .collect())
    }

    /// Most recent diagnosis memos for a suite, in reverse-chronological
    /// order. The `limit` param caps the returned rows; defaults to 25 if
    /// callers pass `None`.
    pub async fn get_recent_diagnoses_for_suite(
        &self,
        suite_id: Uuid,
        limit: Option<u32>,
    ) -> Result<Vec<DiagnosisRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let limit_i: i64 = limit.unwrap_or(25).min(1000) as i64;
        let rows = conn
            .query(
                r#"
                SELECT
                    d.id::TEXT,
                    d.run_id::TEXT,
                    d.diagnosis_json,
                    d.created_at::TEXT
                FROM regression_diagnoses d
                JOIN regression_runs r ON r.id = d.run_id
                WHERE r.suite_id = $1
                ORDER BY d.created_at DESC
                LIMIT $2
                "#,
                &[&suite_id, &limit_i],
            )
            .await
            .map_err(|e| format!("PG get_recent_diagnoses_for_suite: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| DiagnosisRow {
                id: r.get(0),
                run_id: r.get(1),
                diagnosis_json: r.get(2),
                created_at: r.get(3),
            })
            .collect())
    }
}
