//! PostgreSQL generation pipeline artifacts operations.
//!
//! Provides save/query for the `generation_pipeline_artifacts` table using raw SQL.

use super::PgDb;
use crate::workflow_generation::pipeline_artifacts::PipelineArtifact;
use tokio_postgres::Row;

/// Parse a TEXT column that stores JSON into a `serde_json::Value`, or null.
fn parse_json_text(row: &Row, idx: usize) -> serde_json::Value {
    row.get::<_, Option<String>>(idx)
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// Convert a full-detail artifact row into the JSON shape the frontend expects.
fn row_to_full_artifact(r: &Row) -> serde_json::Value {
    serde_json::json!({
        "id": r.get::<_, String>(0),
        "workflow_id": r.get::<_, Option<String>>(1),
        "task_run_id": r.get::<_, Option<String>>(2),
        "description": r.get::<_, String>(3),
        "category": r.get::<_, Option<String>>(4),
        "created_at": r.get::<_, String>(5),
        "discovery_duration_ms": r.get::<_, Option<i32>>(6),
        "builder_duration_ms": r.get::<_, Option<i32>>(7),
        "autofix_duration_ms": r.get::<_, Option<i32>>(8),
        "verification_duration_ms": r.get::<_, Option<i32>>(9),
        "hardener_duration_ms": r.get::<_, Option<i32>>(10),
        "total_duration_ms": r.get::<_, Option<i32>>(11),
        "discovery_calls": parse_json_text(r, 12),
        "builder_raw_output": r.get::<_, Option<String>>(13),
        "builder_parsed_json": parse_json_text(r, 14),
        "autofix_diff": parse_json_text(r, 15),
        "verification_iterations": parse_json_text(r, 16),
        "fixer_snapshots": parse_json_text(r, 17),
        "hardening_summary": parse_json_text(r, 18),
        "hardened_json": parse_json_text(r, 19),
        "final_json": parse_json_text(r, 20),
        "validation_errors": parse_json_text(r, 21),
        "success": r.get::<_, bool>(22),
        "error_message": r.get::<_, Option<String>>(23),
        "model_used": r.get::<_, Option<String>>(24),
    })
}

impl PgDb {
    // ========================================================================
    // Generation Pipeline Artifacts
    // ========================================================================

    /// Save a generation pipeline artifact.
    pub async fn save_generation_artifact(
        &self,
        artifact: &PipelineArtifact,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let investigation_ms = artifact.investigation_duration_ms.map(|v| v as i64);
        let specification_ms = artifact.specification_duration_ms.map(|v| v as i64);
        let discovery_ms = artifact.discovery_duration_ms.map(|v| v as i64);
        let builder_ms = artifact.builder_duration_ms.map(|v| v as i64);
        let autofix_ms = artifact.autofix_duration_ms.map(|v| v as i64);
        let verification_ms = artifact.verification_duration_ms.map(|v| v as i64);
        let hardener_ms = artifact.hardener_duration_ms.map(|v| v as i64);
        let total_ms = artifact.total_duration_ms.map(|v| v as i64);
        let revision_ms = artifact.revision_duration_ms.map(|v| v as i64);
        let revision_cycles = artifact.revision_cycles.map(|v| v as i32);
        let confidence_score = artifact.confidence_score.map(|v| v as f64);
        let spec_criteria = artifact
            .specification_criteria
            .as_ref()
            .map(|v| v.to_string());
        let verification_prompts = artifact
            .verification_prompts
            .as_ref()
            .map(|v| v.to_string());
        let discovery_calls = artifact.discovery_calls.as_ref().map(|v| v.to_string());
        let builder_parsed = artifact.builder_parsed_json.as_ref().map(|v| v.to_string());
        let autofix_diff = artifact.autofix_diff.as_ref().map(|v| v.to_string());
        let verification_iter = artifact
            .verification_iterations
            .as_ref()
            .map(|v| v.to_string());
        let fixer_snaps = artifact
            .fixer_snapshots
            .as_ref()
            .and_then(|v| serde_json::to_string(v).ok());
        let hardening_summary = artifact.hardening_summary.as_ref().map(|v| v.to_string());
        let hardened_json = artifact.hardened_json.as_ref().map(|v| v.to_string());
        let final_json = artifact.final_json.as_ref().map(|v| v.to_string());
        let validation_errors = artifact.validation_errors.as_ref().map(|v| v.to_string());
        let quality_report = artifact.quality_report.as_ref().map(|v| v.to_string());

        conn.execute(
            r#"INSERT INTO generation_pipeline_artifacts
                (id, workflow_id, task_run_id, description, category, created_at,
                 investigation_duration_ms, investigation_enriched_description,
                 specification_duration_ms, specification_criteria,
                 specification_prompt, builder_prompt, verification_prompts, hardener_prompt,
                 discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                 verification_duration_ms, hardener_duration_ms, total_duration_ms,
                 discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                 verification_iterations, fixer_snapshots, hardening_summary,
                 hardened_json, final_json, validation_errors,
                 success, error_message, model_used,
                 revision_duration_ms, quality_report, revision_cycles,
                 confidence_score)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                    $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27,
                    $28, $29, $30, $31, $32, $33, $34, $35, $36, $37)"#,
            &[
                &artifact.id as &(dyn tokio_postgres::types::ToSql + Sync),
                &artifact.workflow_id,
                &artifact.task_run_id,
                &artifact.description,
                &artifact.category,
                &artifact.created_at,
                &investigation_ms,
                &artifact.investigation_enriched_description,
                &specification_ms,
                &spec_criteria,
                &artifact.specification_prompt,
                &artifact.builder_prompt,
                &verification_prompts,
                &artifact.hardener_prompt,
                &discovery_ms,
                &builder_ms,
                &autofix_ms,
                &verification_ms,
                &hardener_ms,
                &total_ms,
                &discovery_calls,
                &artifact.builder_raw_output,
                &builder_parsed,
                &autofix_diff,
                &verification_iter,
                &fixer_snaps,
                &hardening_summary,
                &hardened_json,
                &final_json,
                &validation_errors,
                &artifact.success,
                &artifact.error_message,
                &artifact.model_used,
                &revision_ms,
                &quality_report,
                &revision_cycles,
                &confidence_score,
            ],
        )
        .await
        .map_err(|e| format!("PG save_generation_artifact: {}", e))?;

        Ok(())
    }

    /// Aggregate dashboard metrics for the Generator Evaluation page.
    ///
    /// Joins `generation_pipeline_artifacts` with `workflow_generation_feedback`
    /// to produce the 12-field `DashboardMetrics` shape the frontend expects.
    pub async fn get_generator_dashboard_metrics(
        &self,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Single query over artifacts for generation stats.
        // `verification_iterations` is TEXT holding JSON, so we avoid aggregating
        // over it at query time and leave the iteration average as null here.
        let artifact_row = conn
            .query_one(
                r#"SELECT
                    COUNT(*)::BIGINT AS total,
                    COUNT(*) FILTER (WHERE success)::BIGINT AS successful,
                    AVG(total_duration_ms)::FLOAT8 AS avg_duration
                   FROM generation_pipeline_artifacts"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_generator_dashboard_metrics (artifacts): {}", e))?;

        let total: i64 = artifact_row.get(0);
        let successful: i64 = artifact_row.get(1);
        let avg_total_duration_ms: Option<f64> = artifact_row.get(2);
        let avg_verification_iterations: Option<f64> = None;
        let success_rate = if total > 0 {
            successful as f64 / total as f64
        } else {
            0.0
        };

        // Feedback aggregations
        let feedback_row = conn
            .query_one(
                r#"SELECT
                    COUNT(*) FILTER (WHERE feedback_type = 'edit')::BIGINT AS edits,
                    COUNT(*) FILTER (WHERE feedback_type = 'delete')::BIGINT AS deletes,
                    COUNT(*) FILTER (WHERE rating IS NOT NULL)::BIGINT AS ratings,
                    AVG(rating)::FLOAT8 AS avg_rating
                   FROM workflow_generation_feedback"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_generator_dashboard_metrics (feedback): {}", e))?;

        let total_edits: i64 = feedback_row.get(0);
        let total_deletes: i64 = feedback_row.get(1);
        let total_ratings: i64 = feedback_row.get(2);
        let avg_rating: Option<f64> = feedback_row.get(3);

        Ok(serde_json::json!({
            "total_generations": total,
            "successful_generations": successful,
            "success_rate": success_rate,
            "avg_total_duration_ms": avg_total_duration_ms,
            "avg_verification_iterations": avg_verification_iterations,
            "first_pass_rate": serde_json::Value::Null,
            "hardener_total_processed": 0,
            "hardener_total_converted": 0,
            "total_edits": total_edits,
            "total_deletes": total_deletes,
            "total_ratings": total_ratings,
            "avg_rating": avg_rating,
        }))
    }

    /// Daily time-series of generator activity over the last `days` days.
    pub async fn get_generator_trends(
        &self,
        days: i32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Inline the interval as an integer literal in the SQL rather than
        // binding it as $1::interval. tokio-postgres infers unknown-typed
        // parameters as TEXT, and Postgres can't always cast a text-typed
        // prepared-statement parameter directly to `interval` — the resolver
        // errors out with "could not determine data type of parameter $1".
        // `days` is a validated i32 (clamped to >= 0), so there is no
        // injection risk in interpolating it directly.
        let days = days.max(0);
        // `created_at` is declared TIMESTAMPTZ in the canonical schema but
        // some existing databases (surviving SQLite-era migrations) still
        // hold it as TEXT. Cast explicitly so both shapes work and so
        // `date_trunc`/interval comparison don't trip on type inference.
        let sql = format!(
            r#"SELECT
                to_char(date_trunc('day', created_at::timestamptz), 'YYYY-MM-DD') AS day,
                COUNT(*)::BIGINT AS total,
                COUNT(*) FILTER (WHERE success)::BIGINT AS successful,
                AVG(total_duration_ms)::FLOAT8 AS avg_duration_ms
               FROM generation_pipeline_artifacts
               WHERE created_at::timestamptz >= NOW() - INTERVAL '{} days'
               GROUP BY day
               ORDER BY day"#,
            days
        );
        let rows = conn
            .query(sql.as_str(), &[])
            .await
            .map_err(|e| {
                // Include the error source chain — tokio_postgres::Error's
                // top-level Display can collapse to "db error" and hide the
                // actual SQLSTATE/column/message from Postgres.
                let mut detail = format!("{}", e);
                let mut src: Option<&(dyn std::error::Error + 'static)> =
                    std::error::Error::source(&e);
                while let Some(s) = src {
                    detail.push_str(" | ");
                    detail.push_str(&s.to_string());
                    src = std::error::Error::source(s);
                }
                format!("PG get_generator_trends: {}", detail)
            })?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "date": r.get::<_, String>(0),
                    "total_generations": r.get::<_, i64>(1),
                    "successful_generations": r.get::<_, i64>(2),
                    "avg_duration_ms": r.get::<_, Option<f64>>(3),
                    "avg_verification_iterations": serde_json::Value::Null,
                })
            })
            .collect())
    }

    /// List generation pipeline artifacts, newest first.
    ///
    /// Returns summary rows matching the frontend `PipelineArtifactSummary` shape.
    /// `verification_iteration_count` and `hardener_converted_count` are derived
    /// from the stored JSON blobs.
    pub async fn list_generation_artifacts(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, workflow_id, description, category, created_at::TEXT,
                          total_duration_ms, success, model_used,
                          verification_iterations, hardening_summary
                   FROM generation_pipeline_artifacts
                   ORDER BY created_at DESC
                   LIMIT $1 OFFSET $2"#,
                &[&limit, &offset],
            )
            .await
            .map_err(|e| format!("PG list_generation_artifacts: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let verification_iterations: Option<String> = r.get(8);
                let hardening_summary: Option<String> = r.get(9);

                // Count iterations from the verification JSON (array length)
                let verification_iteration_count = verification_iterations
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .and_then(|v| v.as_array().map(|a| a.len() as u32))
                    .unwrap_or(0);

                // Count converted items from hardening summary JSON
                let hardener_converted_count = hardening_summary
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .and_then(|v| v.get("converted_count").and_then(|c| c.as_u64()))
                    .map(|n| n as u32)
                    .unwrap_or(0);

                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "workflow_id": r.get::<_, Option<String>>(1),
                    "description": r.get::<_, String>(2),
                    "category": r.get::<_, Option<String>>(3),
                    "created_at": r.get::<_, String>(4),
                    "total_duration_ms": r.get::<_, Option<i32>>(5),
                    "success": r.get::<_, bool>(6),
                    "model_used": r.get::<_, Option<String>>(7),
                    "verification_iteration_count": verification_iteration_count,
                    "hardener_converted_count": hardener_converted_count,
                })
            })
            .collect())
    }

    /// Get a single generation pipeline artifact by id with full stage detail.
    pub async fn get_generation_artifact_by_id(
        &self,
        id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, workflow_id, task_run_id, description, category, created_at::TEXT,
                          discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                          verification_duration_ms, hardener_duration_ms, total_duration_ms,
                          discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                          verification_iterations, fixer_snapshots, hardening_summary,
                          hardened_json, final_json, validation_errors,
                          success, error_message, model_used
                   FROM generation_pipeline_artifacts
                   WHERE id = $1"#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_generation_artifact_by_id: {}", e))?;

        Ok(rows.first().map(row_to_full_artifact))
    }

    /// Get the most recent generation pipeline artifact for a workflow.
    pub async fn get_generation_artifact_by_workflow(
        &self,
        workflow_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, workflow_id, task_run_id, description, category, created_at::TEXT,
                          discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                          verification_duration_ms, hardener_duration_ms, total_duration_ms,
                          discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                          verification_iterations, fixer_snapshots, hardening_summary,
                          hardened_json, final_json, validation_errors,
                          success, error_message, model_used
                   FROM generation_pipeline_artifacts
                   WHERE workflow_id = $1
                   ORDER BY created_at DESC
                   LIMIT 1"#,
                &[&workflow_id],
            )
            .await
            .map_err(|e| format!("PG get_generation_artifact_by_workflow: {}", e))?;

        Ok(rows.first().map(row_to_full_artifact))
    }

    /// Get generation pipeline artifacts for a specific task run.
    pub async fn get_generation_artifacts_for_task(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, workflow_id, task_run_id, description, category, created_at::TEXT,
                          total_duration_ms, success, error_message, model_used,
                          confidence_score
                   FROM generation_pipeline_artifacts
                   WHERE task_run_id = $1
                   ORDER BY created_at DESC"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_generation_artifacts_for_task: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "workflow_id": r.get::<_, Option<String>>(1),
                    "task_run_id": r.get::<_, Option<String>>(2),
                    "description": r.get::<_, String>(3),
                    "category": r.get::<_, Option<String>>(4),
                    "created_at": r.get::<_, String>(5),
                    "total_duration_ms": r.get::<_, Option<i64>>(6),
                    "success": r.get::<_, bool>(7),
                    "error_message": r.get::<_, Option<String>>(8),
                    "model_used": r.get::<_, Option<String>>(9),
                    "confidence_score": r.get::<_, Option<f64>>(10),
                })
            })
            .collect())
    }
}
