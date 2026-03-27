//! PostgreSQL graph operations via raw SQL.
//!
//! Covers: workflow_versions, step_finding_links, step_provenance,
//! generation_pipeline_events, rule_influence_log, cross_run_patterns.

use super::PgDb;
use crate::database::cross_run_ops::CrossRunPattern;
use crate::database::graph_ops::{PipelineEvent, WorkflowVersion};

impl PgDb {
    // ========================================================================
    // Workflow Versions
    // ========================================================================

    pub async fn insert_workflow_version(
        &self,
        workflow_id: &str,
        version_number: i32,
        parent_version_id: Option<&str>,
        generation_task_run_id: Option<&str>,
        workflow_json: &str,
        diff_summary: Option<&str>,
        diff_json: Option<&str>,
        trigger: &str,
    ) -> Result<String, String> {
        let id = format!("wv-{}", uuid::Uuid::new_v4());
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"INSERT INTO workflow_versions
               (id, workflow_id, version_number, parent_version_id,
                generation_task_run_id, workflow_json, diff_summary,
                diff_json, trigger)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
            &[
                &id.as_str(),
                &workflow_id,
                &version_number,
                &parent_version_id,
                &generation_task_run_id,
                &workflow_json,
                &diff_summary,
                &diff_json,
                &trigger,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_workflow_version: {}", e))?;
        Ok(id)
    }

    pub async fn get_workflow_versions(
        &self,
        workflow_id: &str,
    ) -> Result<Vec<WorkflowVersion>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, workflow_id, version_number, parent_version_id,
                          generation_task_run_id, workflow_json, diff_summary,
                          diff_json, trigger, created_at::TEXT
                   FROM workflow_versions
                   WHERE workflow_id = $1
                   ORDER BY version_number DESC"#,
                &[&workflow_id],
            )
            .await
            .map_err(|e| format!("PG get_workflow_versions: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| WorkflowVersion {
                id: r.get(0),
                workflow_id: r.get(1),
                version_number: r.get(2),
                parent_version_id: r.get(3),
                generation_task_run_id: r.get(4),
                workflow_json: r.get(5),
                diff_summary: r.get(6),
                diff_json: r.get(7),
                trigger: r.get(8),
                created_at: r.get(9),
            })
            .collect())
    }

    // ========================================================================
    // Step Finding Links
    // ========================================================================

    pub async fn insert_step_finding_link(
        &self,
        task_run_id: &str,
        step_name: &str,
        step_index: i32,
        finding_id: &str,
        link_type: &str,
        confidence: f64,
    ) -> Result<String, String> {
        let id = format!("sfl-{}", uuid::Uuid::new_v4());
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let confidence_f32 = confidence as f32;
        conn.execute(
            r#"INSERT INTO step_finding_links
               (id, task_run_id, step_name, step_index, finding_id, link_type, confidence)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
            &[
                &id.as_str(),
                &task_run_id,
                &step_name,
                &step_index,
                &finding_id,
                &link_type,
                &confidence_f32,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_step_finding_link: {}", e))?;
        Ok(id)
    }

    // ========================================================================
    // Step Provenance
    // ========================================================================

    pub async fn insert_step_provenance(
        &self,
        workflow_id: &str,
        workflow_version_id: Option<&str>,
        step_name: &str,
        step_index: i32,
        phase: &str,
        generating_agent: &str,
        generation_iteration: Option<i32>,
        original_step_json: Option<&str>,
        final_step_json: Option<&str>,
        ui_bridge_event_ids: Option<&str>,
    ) -> Result<String, String> {
        let id = format!("sp-{}", uuid::Uuid::new_v4());
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"INSERT INTO step_provenance
               (id, workflow_id, workflow_version_id, step_name, step_index,
                phase, generating_agent, generation_iteration,
                original_step_json, final_step_json, ui_bridge_event_ids)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
            &[
                &id.as_str(),
                &workflow_id,
                &workflow_version_id,
                &step_name,
                &step_index,
                &phase,
                &generating_agent,
                &generation_iteration,
                &original_step_json,
                &final_step_json,
                &ui_bridge_event_ids,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_step_provenance: {}", e))?;
        Ok(id)
    }

    // ========================================================================
    // Pipeline Events
    // ========================================================================

    pub async fn insert_pipeline_event(
        &self,
        task_run_id: &str,
        workflow_id: Option<&str>,
        event_type: &str,
        phase: Option<&str>,
        iteration: Option<i32>,
        payload: Option<&str>,
        duration_ms: Option<i64>,
        token_count: Option<i64>,
        validation_errors_before: Option<i32>,
        validation_errors_after: Option<i32>,
    ) -> Result<String, String> {
        let id = format!("gpe-{}", uuid::Uuid::new_v4());
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"INSERT INTO generation_pipeline_events
               (id, task_run_id, workflow_id, event_type, phase, iteration,
                payload, duration_ms, token_count,
                validation_errors_before, validation_errors_after)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
            &[
                &id.as_str(),
                &task_run_id,
                &workflow_id,
                &event_type,
                &phase,
                &iteration,
                &payload,
                &duration_ms,
                &token_count,
                &validation_errors_before,
                &validation_errors_after,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_pipeline_event: {}", e))?;
        Ok(id)
    }

    pub async fn get_pipeline_events(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<PipelineEvent>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, task_run_id, workflow_id, event_type, phase,
                          iteration, payload, duration_ms, token_count,
                          validation_errors_before, validation_errors_after,
                          created_at::TEXT
                   FROM generation_pipeline_events
                   WHERE task_run_id = $1
                   ORDER BY created_at ASC"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_pipeline_events: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| PipelineEvent {
                id: r.get(0),
                task_run_id: r.get(1),
                workflow_id: r.get(2),
                event_type: r.get(3),
                phase: r.get(4),
                iteration: r.get(5),
                payload: r.get(6),
                duration_ms: r.get(7),
                token_count: r.get(8),
                validation_errors_before: r.get(9),
                validation_errors_after: r.get(10),
                created_at: r.get(11),
            })
            .collect())
    }

    // ========================================================================
    // Rule Influence
    // ========================================================================

    pub async fn insert_rule_influence(
        &self,
        rule_id: &str,
        task_run_id: &str,
        workflow_id: Option<&str>,
        influence_type: &str,
        evidence: Option<&str>,
        phase: Option<&str>,
    ) -> Result<String, String> {
        let id = format!("ri-{}", uuid::Uuid::new_v4());
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"INSERT INTO rule_influence_log
               (id, rule_id, task_run_id, workflow_id, influence_type, evidence, phase)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
            &[
                &id.as_str(),
                &rule_id,
                &task_run_id,
                &workflow_id,
                &influence_type,
                &evidence,
                &phase,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_rule_influence: {}", e))?;
        Ok(id)
    }

    // ========================================================================
    // Cross-Run Patterns
    // ========================================================================

    pub async fn get_active_patterns(
        &self,
        workflow_name: Option<&str>,
    ) -> Result<Vec<CrossRunPattern>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if let Some(wn) = workflow_name {
            conn.query(
                r#"SELECT id, pattern_type, signature_hash, workflow_name,
                          occurrence_count, first_seen_task_run_id, last_seen_task_run_id,
                          first_seen_at::TEXT, last_seen_at::TEXT,
                          affected_components, pattern_data, status,
                          resolved_by_fix_id, created_at::TEXT, updated_at::TEXT
                   FROM cross_run_patterns
                   WHERE status = 'active'
                     AND (workflow_name IS NULL OR workflow_name = $1)
                   ORDER BY occurrence_count DESC"#,
                &[&wn],
            )
            .await
            .map_err(|e| format!("PG get_active_patterns: {}", e))?
        } else {
            conn.query(
                r#"SELECT id, pattern_type, signature_hash, workflow_name,
                          occurrence_count, first_seen_task_run_id, last_seen_task_run_id,
                          first_seen_at::TEXT, last_seen_at::TEXT,
                          affected_components, pattern_data, status,
                          resolved_by_fix_id, created_at::TEXT, updated_at::TEXT
                   FROM cross_run_patterns
                   WHERE status = 'active'
                   ORDER BY occurrence_count DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_active_patterns: {}", e))?
        };

        Ok(rows
            .iter()
            .map(|r| CrossRunPattern {
                id: r.get(0),
                pattern_type: r.get(1),
                signature_hash: r.get(2),
                workflow_name: r.get(3),
                occurrence_count: r.get(4),
                first_seen_task_run_id: r.get(5),
                last_seen_task_run_id: r.get(6),
                first_seen_at: r.get(7),
                last_seen_at: r.get(8),
                affected_components: r.get(9),
                pattern_data: r.get(10),
                status: r.get(11),
                resolved_by_fix_id: r.get(12),
                created_at: r.get(13),
                updated_at: r.get(14),
            })
            .collect())
    }
}
