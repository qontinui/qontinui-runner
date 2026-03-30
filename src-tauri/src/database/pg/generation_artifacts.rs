//! PostgreSQL generation pipeline artifacts operations.
//!
//! Provides save/query for the `generation_pipeline_artifacts` table using raw SQL.

use super::PgDb;
use crate::workflow_generation::pipeline_artifacts::PipelineArtifact;

impl PgDb {
    // ========================================================================
    // Generation Pipeline Artifacts
    // ========================================================================

    /// Save a generation pipeline artifact.
    pub async fn save_generation_artifact(
        &self,
        artifact: &PipelineArtifact,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

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
        let spec_criteria = artifact.specification_criteria.as_ref().map(|v| v.to_string());
        let verification_prompts = artifact.verification_prompts.as_ref().map(|v| v.to_string());
        let discovery_calls = artifact.discovery_calls.as_ref().map(|v| v.to_string());
        let builder_parsed = artifact.builder_parsed_json.as_ref().map(|v| v.to_string());
        let autofix_diff = artifact.autofix_diff.as_ref().map(|v| v.to_string());
        let verification_iter = artifact.verification_iterations.as_ref().map(|v| v.to_string());
        let fixer_snaps = artifact.fixer_snapshots.as_ref()
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

    /// Get generation pipeline artifacts for a specific task run.
    pub async fn get_generation_artifacts_for_task(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

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
