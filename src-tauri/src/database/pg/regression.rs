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

/// Lightweight suite catalog row: identity + when last touched. Used by the
/// CoveragePanel suite picker and other catalog queries that don't need the
/// full `suite_json` blob.
#[derive(Debug, Serialize, Deserialize)]
pub struct SuiteRow {
    pub id: String,
    pub ir_doc_id: String,
    pub created_at: String,
    /// Number of recorded runs against this suite. Computed via a left-join
    /// count so suites with zero runs still appear in the catalog.
    pub run_count: i64,
    /// `started_at` of the most recent run, or `None` if the suite has never
    /// been executed.
    pub last_run_at: Option<String>,
}

/// Full suite row including the JSON blob (the panel needs the deserialized
/// `RegressionSuite` to feed `coverageDiff` / `coverageOf`).
#[derive(Debug, Serialize, Deserialize)]
pub struct SuiteFullRow {
    pub id: String,
    pub ir_doc_id: String,
    pub suite_json: serde_json::Value,
    pub created_at: String,
}

/// Lightweight run summary for picker UIs.
#[derive(Debug, Serialize, Deserialize)]
pub struct RunSummaryRow {
    pub id: String,
    pub run_id: String,
    pub passed: i32,
    pub failed: i32,
    pub started_at: String,
    pub completed_at: String,
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
    /// form. `drift_report_json` is optional — when present, the executor
    /// passes through the combined `DriftReport` (specDrift + visualDrift
    /// merged) used to build `DriftContext` for the diagnose call. The
    /// `/runs/:run_id/drift` HTTP endpoint reads it back unmodified.
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
        drift_report_json: Option<&serde_json::Value>,
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
                started_at, completed_at, run_result_json, drift_report_json
            ) VALUES (
                $1, $2, $3, $4, $5,
                $6::timestamptz, $7::timestamptz, $8::jsonb, $9::jsonb
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
                &drift_report_json,
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

    /// Catalog of every persisted regression suite, newest first. Includes a
    /// run-count and the most recent run timestamp via a left-join so suites
    /// without runs still surface in the picker.
    pub async fn list_regression_suites(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<SuiteRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let limit_i: i64 = limit.unwrap_or(50).min(1000) as i64;
        let rows = conn
            .query(
                r#"
                SELECT
                    s.id::TEXT,
                    s.ir_doc_id,
                    s.created_at::TEXT,
                    COALESCE(stats.run_count, 0)::BIGINT AS run_count,
                    stats.last_run_at::TEXT
                FROM regression_suites s
                LEFT JOIN LATERAL (
                    SELECT
                        COUNT(*) AS run_count,
                        MAX(started_at) AS last_run_at
                    FROM regression_runs
                    WHERE suite_id = s.id
                ) stats ON TRUE
                ORDER BY s.created_at DESC
                LIMIT $1
                "#,
                &[&limit_i],
            )
            .await
            .map_err(|e| format!("PG list_regression_suites: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| SuiteRow {
                id: r.get(0),
                ir_doc_id: r.get(1),
                created_at: r.get(2),
                run_count: r.get(3),
                last_run_at: r.get(4),
            })
            .collect())
    }

    /// Fetch a single suite by id. Returns `None` if missing.
    pub async fn get_regression_suite_by_id(
        &self,
        suite_id: Uuid,
    ) -> Result<Option<SuiteFullRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT
                    s.id::TEXT,
                    s.ir_doc_id,
                    s.suite_json,
                    s.created_at::TEXT
                FROM regression_suites s
                WHERE s.id = $1
                "#,
                &[&suite_id],
            )
            .await
            .map_err(|e| format!("PG get_regression_suite_by_id: {}", e))?;

        Ok(rows.first().map(|r| SuiteFullRow {
            id: r.get(0),
            ir_doc_id: r.get(1),
            suite_json: r.get(2),
            created_at: r.get(3),
        }))
    }

    /// Fetch the most recent persisted `drift_report_json` for a logical
    /// `run_id` (the caller-supplied id, not the row UUID).
    ///
    /// Returns `None` when no run with that id exists, when the run exists
    /// but has no drift report, OR when the report is JSON `null`.
    /// Used by the `/runs/:run_id/drift` HTTP endpoint and the qontinui-web
    /// drift dashboard proxy.
    pub async fn get_drift_report_for_logical_run_id(
        &self,
        run_id_text: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT drift_report_json
                FROM regression_runs
                WHERE run_id = $1 AND drift_report_json IS NOT NULL
                ORDER BY started_at DESC
                LIMIT 1
                "#,
                &[&run_id_text],
            )
            .await
            .map_err(|e| format!("PG get_drift_report_for_logical_run_id: {}", e))?;

        Ok(rows.first().and_then(|r| {
            let v: Option<serde_json::Value> = r.get(0);
            v.filter(|v| !v.is_null())
        }))
    }

    /// Recent runs for a suite, newest first, lightweight summary shape.
    pub async fn list_regression_runs_for_suite(
        &self,
        suite_id: Uuid,
        limit: Option<u32>,
    ) -> Result<Vec<RunSummaryRow>, String> {
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
                    r.id::TEXT,
                    r.run_id,
                    r.passed,
                    r.failed,
                    r.started_at::TEXT,
                    r.completed_at::TEXT
                FROM regression_runs r
                WHERE r.suite_id = $1
                ORDER BY r.started_at DESC
                LIMIT $2
                "#,
                &[&suite_id, &limit_i],
            )
            .await
            .map_err(|e| format!("PG list_regression_runs_for_suite: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| RunSummaryRow {
                id: r.get(0),
                run_id: r.get(1),
                passed: r.get(2),
                failed: r.get(3),
                started_at: r.get(4),
                completed_at: r.get(5),
            })
            .collect())
    }
}
