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

    /// Link findings to steps by matching creation timestamps within the same task run.
    /// Finds all findings detected during the step's execution window and creates links.
    pub async fn link_findings_to_steps_by_timestamp(
        &self,
        task_run_id: &str,
        step_name: &str,
        step_index: i32,
        step_started_at: &str,
        step_completed_at: &str,
    ) -> Result<u32, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id FROM task_run_findings
                   WHERE task_run_id = $1
                   AND created_at >= $2
                   AND created_at <= $3
                   AND id NOT IN (SELECT finding_id FROM step_finding_links WHERE task_run_id = $1)"#,
                &[&task_run_id, &step_started_at, &step_completed_at],
            )
            .await
            .map_err(|e| format!("PG find unlinked findings: {}", e))?;

        let mut count = 0u32;
        for row in &rows {
            let finding_id: String = row.get(0);
            let id = format!("sfl-{}", uuid::Uuid::new_v4());
            let confidence: f32 = 0.8;
            if conn
                .execute(
                    r#"INSERT INTO step_finding_links
                       (id, task_run_id, step_name, step_index, finding_id, link_type, confidence)
                       VALUES ($1, $2, $3, $4, $5, 'temporal', $6)"#,
                    &[
                        &id.as_str(),
                        &task_run_id,
                        &step_name,
                        &step_index,
                        &finding_id.as_str(),
                        &confidence,
                    ],
                )
                .await
                .is_ok()
            {
                count += 1;
            }
        }

        Ok(count)
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

    /// Get step provenance records for a workflow.
    pub async fn get_provenance_for_workflow(
        &self,
        workflow_id: &str,
    ) -> Result<Vec<crate::database::graph_ops::StepProvenance>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, workflow_id, workflow_version_id, step_name, step_index,
                          phase, generating_agent, generation_iteration,
                          original_step_json, final_step_json, ui_bridge_event_ids, created_at
                   FROM step_provenance
                   WHERE workflow_id = $1
                   ORDER BY step_index ASC, created_at ASC"#,
                &[&workflow_id],
            )
            .await
            .map_err(|e| format!("PG get_provenance_for_workflow: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::database::graph_ops::StepProvenance {
                id: r.get(0),
                workflow_id: r.get(1),
                workflow_version_id: r.get(2),
                step_name: r.get(3),
                step_index: r.get(4),
                phase: r.get(5),
                generating_agent: r.get(6),
                generation_iteration: r.get(7),
                original_step_json: r.get(8),
                final_step_json: r.get(9),
                ui_bridge_event_ids: r.get(10),
                created_at: r.get(11),
            })
            .collect())
    }

    /// Get rule influences for a rule.
    pub async fn get_rule_influences(
        &self,
        rule_id: &str,
    ) -> Result<Vec<crate::database::graph_ops::RuleInfluence>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, rule_id, task_run_id, workflow_id, influence_type,
                          evidence, phase, created_at
                   FROM rule_influence_log
                   WHERE rule_id = $1
                   ORDER BY created_at DESC"#,
                &[&rule_id],
            )
            .await
            .map_err(|e| format!("PG get_rule_influences: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::database::graph_ops::RuleInfluence {
                id: r.get(0),
                rule_id: r.get(1),
                task_run_id: r.get(2),
                workflow_id: r.get(3),
                influence_type: r.get(4),
                evidence: r.get(5),
                phase: r.get(6),
                created_at: r.get(7),
            })
            .collect())
    }

    /// Get rules that appear ineffective.
    pub async fn get_ineffective_rules(
        &self,
        min_no_effect_count: i64,
    ) -> Result<Vec<crate::database::graph_ops::IneffectiveRule>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT
                       rule_id,
                       SUM(CASE WHEN influence_type = 'no_effect' THEN 1 ELSE 0 END) as no_effect_count,
                       SUM(CASE WHEN influence_type = 'prevented_error' THEN 1 ELSE 0 END) as prevented_error_count,
                       COUNT(*) as total_loaded_count
                   FROM rule_influence_log
                   GROUP BY rule_id
                   HAVING SUM(CASE WHEN influence_type = 'no_effect' THEN 1 ELSE 0 END) >= $1
                      AND SUM(CASE WHEN influence_type = 'prevented_error' THEN 1 ELSE 0 END) = 0
                   ORDER BY no_effect_count DESC"#,
                &[&min_no_effect_count],
            )
            .await
            .map_err(|e| format!("PG get_ineffective_rules: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::database::graph_ops::IneffectiveRule {
                rule_id: r.get(0),
                no_effect_count: r.get(1),
                prevented_error_count: r.get(2),
                total_loaded_count: r.get(3),
            })
            .collect())
    }

    /// Get phase stats for a workflow.
    pub async fn get_phase_stats(
        &self,
        workflow_id: &str,
    ) -> Result<Vec<crate::database::graph_ops::PhaseStats>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT
                       phase,
                       COUNT(*) as event_count,
                       AVG(duration_ms) as avg_duration_ms,
                       COALESCE(SUM(token_count), 0) as total_token_count,
                       AVG(validation_errors_before) as avg_errors_before,
                       AVG(validation_errors_after) as avg_errors_after
                   FROM generation_pipeline_events
                   WHERE workflow_id = $1 AND phase IS NOT NULL
                   GROUP BY phase
                   ORDER BY phase"#,
                &[&workflow_id],
            )
            .await
            .map_err(|e| format!("PG get_phase_stats: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::database::graph_ops::PhaseStats {
                phase: r.get(0),
                event_count: r.get(1),
                avg_duration_ms: r.get(2),
                total_token_count: r.get(3),
                avg_errors_before: r.get(4),
                avg_errors_after: r.get(5),
            })
            .collect())
    }

    /// Get fix effectiveness scores for the ui-fix-effectiveness endpoint.
    pub async fn get_fix_effectiveness_scores(
        &self,
        limit: i64,
    ) -> Result<Vec<crate::database::cross_run_ops::FixEffectivenessScore>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT p.resolved_by_fix_id, p.id, p.pattern_type,
                          p.updated_at, p.status, p.occurrence_count::bigint
                   FROM cross_run_patterns p
                   WHERE p.resolved_by_fix_id IS NOT NULL
                   ORDER BY p.updated_at DESC
                   LIMIT $1"#,
                &[&limit],
            )
            .await
            .map_err(|e| format!("PG get_fix_effectiveness_scores: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let status: String = r.get(3);
                let resolved_at: Option<String> = r.get(3);
                let re_appeared = status == "active"; // pattern re-appeared if active again
                crate::database::cross_run_ops::FixEffectivenessScore {
                    fix_id: r.get(0),
                    pattern_id: r.get(1),
                    pattern_type: r.get(2),
                    resolved_at,
                    re_appeared,
                    occurrence_count_at_resolution: r.get(5),
                }
            })
            .collect())
    }

    /// Unified search across all data stores (PG version of unified_search).
    pub async fn unified_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::reflection::unified_search::UnifiedSearchResult>, String> {
        use crate::reflection::unified_search::UnifiedSearchResult;

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let pattern = format!("%{}%", query);
        let query_lower = query.to_lowercase();
        let mut results = Vec::new();

        fn truncate_str(s: &str, max_len: usize) -> String {
            if s.len() <= max_len {
                s.to_string()
            } else {
                let truncated: String = s.chars().take(max_len).collect();
                format!("{}...", truncated)
            }
        }

        // 1. Search task_run_findings
        if let Ok(rows) = conn
            .query(
                r#"SELECT id, title, description, category, severity
               FROM task_run_findings
               WHERE title ILIKE $1 OR description ILIKE $1
               ORDER BY detected_at DESC LIMIT 20"#,
                &[&pattern],
            )
            .await
        {
            for r in &rows {
                let id: String = r.get(0);
                let title: String = r.get(1);
                let description: String = r.get(2);
                let category: String = r.get(3);
                let severity: String = r.get(4);
                let score = if title.to_lowercase().contains(&query_lower) {
                    1.0
                } else {
                    0.7
                };
                results.push(UnifiedSearchResult {
                    entity_type: "finding".to_string(),
                    entity_id: id,
                    title,
                    snippet: format!(
                        "[{}][{}] {}",
                        category,
                        severity,
                        truncate_str(&description, 120)
                    ),
                    relevance_score: score,
                    source_table: "task_run_findings".to_string(),
                });
            }
        }

        // 2. Search reflection_fixes
        if let Ok(rows) = conn
            .query(
                r#"SELECT id, fix_type, fix_description, COALESCE(reflection_scope, ''), status
               FROM reflection_fixes
               WHERE fix_description ILIKE $1 OR fix_type ILIKE $1
               ORDER BY created_at DESC LIMIT 20"#,
                &[&pattern],
            )
            .await
        {
            for r in &rows {
                let id: String = r.get(0);
                let fix_type: String = r.get(1);
                let fix_description: String = r.get(2);
                let scope: String = r.get(3);
                let status: String = r.get(4);
                let score = if fix_type.to_lowercase().contains(&query_lower) {
                    1.0
                } else {
                    0.7
                };
                results.push(UnifiedSearchResult {
                    entity_type: "fix".to_string(),
                    entity_id: id,
                    title: fix_type,
                    snippet: format!(
                        "[{}][{}] {}",
                        scope,
                        status,
                        truncate_str(&fix_description, 120)
                    ),
                    relevance_score: score,
                    source_table: "reflection_fixes".to_string(),
                });
            }
        }

        // 3. Search task_knowledge
        if let Ok(rows) = conn
            .query(
                r#"SELECT id, category, content, confidence
               FROM task_knowledge
               WHERE content ILIKE $1 OR category ILIKE $1
               ORDER BY created_at DESC LIMIT 20"#,
                &[&pattern],
            )
            .await
        {
            for r in &rows {
                let id: String = r.get(0);
                let category: String = r.get(1);
                let content: String = r.get(2);
                let confidence: String = r.get(3);
                let score = if category.to_lowercase().contains(&query_lower) {
                    1.0
                } else {
                    0.7
                };
                results.push(UnifiedSearchResult {
                    entity_type: "knowledge".to_string(),
                    entity_id: id,
                    title: format!("{} knowledge", category),
                    snippet: format!(
                        "[{}][conf:{}] {}",
                        category,
                        confidence,
                        truncate_str(&content, 120)
                    ),
                    relevance_score: score,
                    source_table: "task_knowledge".to_string(),
                });
            }
        }

        // 4. Search error_events
        if let Ok(rows) = conn
            .query(
                r#"SELECT id, log_source_name, severity, message, error_type
               FROM error_events
               WHERE message ILIKE $1 OR error_type ILIKE $1
               ORDER BY last_seen_at DESC LIMIT 20"#,
                &[&pattern],
            )
            .await
        {
            for r in &rows {
                let id: i64 = r.get(0);
                let source: String = r.get(1);
                let severity: String = r.get(2);
                let message: String = r.get(3);
                let error_type: Option<String> = r.get(4);
                let score = if error_type
                    .as_deref()
                    .is_some_and(|t| t.to_lowercase().contains(&query_lower))
                {
                    1.0
                } else {
                    0.7
                };
                results.push(UnifiedSearchResult {
                    entity_type: "error".to_string(),
                    entity_id: id.to_string(),
                    title: error_type.unwrap_or_else(|| format!("{} error", source)),
                    snippet: format!("[{}][{}] {}", source, severity, truncate_str(&message, 120)),
                    relevance_score: score,
                    source_table: "error_events".to_string(),
                });
            }
        }

        // 5. Search generation_rules
        if let Ok(rows) = conn
            .query(
                r#"SELECT id, agent, title, content, severity, status
               FROM generation_rules
               WHERE title ILIKE $1 OR content ILIKE $1
               ORDER BY created_at DESC LIMIT 20"#,
                &[&pattern],
            )
            .await
        {
            for r in &rows {
                let id: String = r.get(0);
                let agent: String = r.get(1);
                let title: String = r.get(2);
                let content: String = r.get(3);
                let severity: String = r.get(4);
                let status: String = r.get(5);
                let score = if title.to_lowercase().contains(&query_lower) {
                    1.0
                } else {
                    0.7
                };
                results.push(UnifiedSearchResult {
                    entity_type: "rule".to_string(),
                    entity_id: id,
                    title: format!("{} ({})", title, agent),
                    snippet: format!(
                        "[{}][{}][{}] {}",
                        agent,
                        severity,
                        status,
                        truncate_str(&content, 120)
                    ),
                    relevance_score: score,
                    source_table: "generation_rules".to_string(),
                });
            }
        }

        // 6. Search unified_workflows
        if let Ok(rows) = conn
            .query(
                r#"SELECT id, name, description, category
               FROM unified_workflows
               WHERE name ILIKE $1 OR description ILIKE $1
               ORDER BY updated_at DESC LIMIT 20"#,
                &[&pattern],
            )
            .await
        {
            for r in &rows {
                let id: String = r.get(0);
                let name: String = r.get(1);
                let description: String = r.get(2);
                let category: String = r.get(3);
                let score = if name.to_lowercase().contains(&query_lower) {
                    1.0
                } else {
                    0.7
                };
                results.push(UnifiedSearchResult {
                    entity_type: "workflow".to_string(),
                    entity_id: id,
                    title: name,
                    snippet: format!("[{}] {}", category, truncate_str(&description, 120)),
                    relevance_score: score,
                    source_table: "unified_workflows".to_string(),
                });
            }
        }

        // 7. Search ui_bridge_elements
        if let Ok(rows) = conn
            .query(
                r#"SELECT DISTINCT element_id, element_type, label, text_content
               FROM ui_bridge_elements
               WHERE element_id ILIKE $1 OR label ILIKE $1 OR text_content ILIKE $1
               LIMIT 20"#,
                &[&pattern],
            )
            .await
        {
            for r in &rows {
                let element_id: String = r.get(0);
                let element_type: Option<String> = r.get(1);
                let label: Option<String> = r.get(2);
                let text_content: Option<String> = r.get(3);
                let id_matches = element_id.to_lowercase().contains(&query_lower);
                let label_matches = label
                    .as_deref()
                    .is_some_and(|l| l.to_lowercase().contains(&query_lower));
                let score = if id_matches {
                    1.0
                } else if label_matches {
                    0.8
                } else {
                    0.6
                };
                results.push(UnifiedSearchResult {
                    entity_type: "ui_element".to_string(),
                    entity_id: element_id.clone(),
                    title: label.unwrap_or(element_id),
                    snippet: format!(
                        "[{}] {}",
                        element_type.as_deref().unwrap_or("unknown"),
                        text_content
                            .as_deref()
                            .map(|t| truncate_str(t, 120))
                            .unwrap_or_default()
                    ),
                    relevance_score: score,
                    source_table: "ui_bridge_elements".to_string(),
                });
            }
        }

        // Sort by relevance DESC, truncate
        results.sort_by(|a, b| {
            b.relevance_score
                .partial_cmp(&a.relevance_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        Ok(results)
    }

    /// Get skill metrics for the /graph/skill-metrics endpoint.
    pub async fn get_skill_metrics(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Status counts
        let status_rows = conn
            .query(
                r#"SELECT COALESCE(approval_status, 'pending') as status, COUNT(*)::bigint
                   FROM user_skills WHERE source = 'auto'
                   GROUP BY COALESCE(approval_status, 'pending') ORDER BY COUNT(*) DESC"#,
                &[],
            )
            .await
            .unwrap_or_default();

        let mut pending = 0i64;
        let mut approved = 0i64;
        let mut rejected = 0i64;
        for row in &status_rows {
            let status: String = row.get(0);
            let count: i64 = row.get(1);
            match status.as_str() {
                "pending" => pending = count,
                "approved" => approved = count,
                "rejected" => rejected = count,
                _ => {}
            }
        }

        // Total usage
        let total_usage: i64 = conn
            .query_one(
                r#"SELECT COALESCE(SUM(usage_count), 0)::bigint FROM user_skills
                   WHERE source = 'auto' AND approval_status = 'approved'"#,
                &[],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);

        // Failure events
        let failure_events: i64 = conn
            .query_one(
                "SELECT COUNT(*)::bigint FROM task_run_events WHERE event_type = 'skill_failure'",
                &[],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);

        // Auto-disabled count
        let auto_disabled: i64 = conn
            .query_one(
                r#"SELECT COUNT(DISTINCT event_subtype)::bigint FROM task_run_events
                   WHERE event_type = 'skill_failure'"#,
                &[],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);

        // Top skills
        let top_rows = conn
            .query(
                r#"SELECT slug, category, usage_count::bigint FROM user_skills
                   WHERE source = 'auto' AND approval_status = 'approved'
                   ORDER BY usage_count DESC LIMIT 5"#,
                &[],
            )
            .await
            .unwrap_or_default();

        let top: Vec<serde_json::Value> = top_rows
            .iter()
            .map(|r| {
                let slug: String = r.get(0);
                let cat: String = r.get(1);
                let usage: i64 = r.get(2);
                serde_json::json!({"slug": slug, "category": cat, "usage_count": usage})
            })
            .collect();

        Ok(serde_json::json!({
            "total_auto_skills": pending + approved + rejected,
            "pending": pending,
            "approved": approved,
            "rejected": rejected,
            "total_usage": total_usage,
            "failure_events": failure_events,
            "unique_skills_failed": auto_disabled,
            "top_skills": top,
        }))
    }
}
