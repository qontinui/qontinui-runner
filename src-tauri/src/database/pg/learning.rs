//! PostgreSQL learning system operations via Clorinde-generated queries.
//!
//! Learning outcomes and patterns for the meta-optimizer.

use super::PgDb;

fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn json_opt(s: &str) -> Option<serde_json::Value> {
    if s.is_empty() {
        None
    } else {
        serde_json::from_str(s).ok()
    }
}

impl PgDb {
    /// Record a task outcome for learning.
    pub async fn record_learning_outcome(
        &self,
        id: &str,
        task_id: &str,
        status: &str,
        duration_secs: Option<f64>,
        iterations: Option<i32>,
        strategy: Option<&str>,
        tools_used: Option<&str>,
        files_modified: Option<&str>,
        error_type: Option<&str>,
        error_message: Option<&str>,
        feedback: Option<&str>,
        workflow_architecture: Option<&str>,
        step_count: Option<i64>,
        verification_step_count: Option<i64>,
        agentic_step_count: Option<i64>,
        has_ui_bridge: bool,
        total_tokens: Option<i64>,
        total_cost_usd: Option<f64>,
        technology_tags: Option<&str>,
        domain_tags: Option<&str>,
        complexity_tier: Option<&str>,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let result_id = qontinui_db::queries::learning::record_learning_outcome()
            .bind(
                &conn,
                &id,
                &task_id,
                &status,
                &duration_secs,
                &iterations,
                &strategy,
                &tools_used,
                &files_modified,
                &error_type,
                &error_message,
                &feedback,
                &workflow_architecture,
                &step_count,
                &verification_step_count,
                &agentic_step_count,
                &has_ui_bridge,
                &total_tokens,
                &total_cost_usd,
                &technology_tags,
                &domain_tags,
                &complexity_tier,
            )
            .one()
            .await
            .map_err(|e| format!("PG record_learning_outcome: {}", e))?;
        Ok(result_id)
    }

    /// Update the model_used field for a learning outcome.
    /// Called after recording to backfill model tracking data.
    pub async fn update_learning_outcome_model(
        &self,
        task_id: &str,
        model_used: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE learning_outcomes SET model_used = $2 WHERE task_id = $1 AND model_used IS NULL",
            &[&task_id, &model_used],
        )
        .await
        .map_err(|e| format!("PG update_learning_outcome_model: {}", e))?;
        Ok(())
    }

    /// Get learning outcomes for analysis.
    pub async fn get_learning_outcomes(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let max_results = limit.unwrap_or(100) as i64;

        let rows = qontinui_db::queries::learning::get_learning_outcomes()
            .bind(&conn, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_learning_outcomes: {}", e))?;

        Ok(rows.into_iter().map(|r| {
            serde_json::json!({
                "id": r.id,
                "task_id": r.task_id,
                "status": r.status,
                "duration_secs": if r.duration_secs == 0.0 { None } else { Some(r.duration_secs) },
                "iterations": if r.iterations == 0 { None } else { Some(r.iterations) },
                "strategy": non_empty(&r.strategy),
                "tools_used": json_opt(&r.tools_used),
                "files_modified": json_opt(&r.files_modified),
                "error_type": non_empty(&r.error_type),
                "error_message": non_empty(&r.error_message),
                "feedback": json_opt(&r.feedback),
                "created_at": r.created_at.to_rfc3339(),
                "workflow_architecture": non_empty(&r.workflow_architecture),
                "step_count": if r.step_count == 0 { None } else { Some(r.step_count) },
                "verification_step_count": if r.verification_step_count == 0 { None } else { Some(r.verification_step_count) },
                "agentic_step_count": if r.agentic_step_count == 0 { None } else { Some(r.agentic_step_count) },
                "has_ui_bridge": r.has_ui_bridge,
                "total_tokens": if r.total_tokens == 0 { None } else { Some(r.total_tokens) },
                "total_cost_usd": if r.total_cost_usd == 0.0 { None } else { Some(r.total_cost_usd) },
                "composite_agentic_score": if r.composite_agentic_score == 0.0 { None } else { Some(r.composite_agentic_score) },
                "technology_tags": json_opt(&r.technology_tags),
                "domain_tags": json_opt(&r.domain_tags),
                "complexity_tier": non_empty(&r.complexity_tier),
            })
        }).collect())
    }

    /// Save or update a learning pattern.
    pub async fn save_learning_pattern(
        &self,
        id: &str,
        pattern_type: &str,
        description: &str,
        confidence: f64,
        occurrences: i32,
        context: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::learning::save_learning_pattern()
            .bind(
                &conn,
                &id,
                &pattern_type,
                &description,
                &confidence,
                &occurrences,
                &context,
            )
            .one()
            .await
            .map_err(|e| format!("PG save_learning_pattern: {}", e))?;
        Ok(())
    }

    /// Get all learning patterns.
    pub async fn get_learning_patterns(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::learning::get_learning_patterns()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG get_learning_patterns: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "pattern_type": r.pattern_type,
                    "description": r.description,
                    "confidence": r.confidence,
                    "occurrences": r.occurrences,
                    "context": json_opt(&r.context),
                    "created_at": r.created_at.to_rfc3339(),
                    "updated_at": r.updated_at.to_rfc3339(),
                })
            })
            .collect())
    }

    /// Get learning outcomes count.
    pub async fn get_learning_outcomes_count(&self) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let count = qontinui_db::queries::learning::get_learning_outcomes_count()
            .bind(&conn)
            .one()
            .await
            .map_err(|e| format!("PG get_learning_outcomes_count: {}", e))?;
        Ok(count)
    }

    /// Get learning outcomes with optional filtering by status, strategy, and date.
    pub async fn get_learning_outcomes_filtered(
        &self,
        status: Option<&str>,
        strategy: Option<&str>,
        since: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let limit_val = limit.unwrap_or(100);

        // Build dynamic WHERE clause
        let mut conditions = Vec::new();
        let mut param_idx = 1u32;

        if status.is_some() {
            conditions.push(format!("status = ${}", param_idx));
            param_idx += 1;
        }
        if strategy.is_some() {
            conditions.push(format!("strategy = ${}", param_idx));
            param_idx += 1;
        }
        if since.is_some() {
            conditions.push(format!("created_at >= ${}::timestamptz", param_idx));
            param_idx += 1;
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let query = format!(
            r#"
            SELECT id, task_id, status, duration_secs, iterations, strategy,
                   tools_used, files_modified, error_type, error_message, feedback, created_at,
                   workflow_architecture, step_count, verification_step_count,
                   agentic_step_count, has_ui_bridge,
                   total_tokens, total_cost_usd, composite_agentic_score,
                   technology_tags, domain_tags, complexity_tier
            FROM learning_outcomes
            {}
            ORDER BY created_at DESC
            LIMIT ${}
            "#,
            where_clause, param_idx
        );

        // Build params dynamically
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(s) = status {
            params.push(Box::new(s.to_string()));
        }
        if let Some(s) = strategy {
            params.push(Box::new(s.to_string()));
        }
        if let Some(s) = since {
            params.push(Box::new(s.to_string()));
        }
        params.push(Box::new(limit_val));

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&query, &param_refs)
            .await
            .map_err(|e| format!("PG get_learning_outcomes_filtered: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let tools: Option<String> = r.get(6);
                let files: Option<String> = r.get(7);
                let feedback_str: Option<String> = r.get(10);
                let created: chrono::DateTime<chrono::Utc> = r.get(11);
                let tech_tags: Option<String> = r.get(20);
                let domain_tags: Option<String> = r.get(21);

                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "task_id": r.get::<_, String>(1),
                    "status": r.get::<_, String>(2),
                    "duration_secs": r.get::<_, Option<f64>>(3),
                    "iterations": r.get::<_, Option<i32>>(4),
                    "strategy": r.get::<_, Option<String>>(5),
                    "tools_used": tools.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "files_modified": files.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error_type": r.get::<_, Option<String>>(8),
                    "error_message": r.get::<_, Option<String>>(9),
                    "feedback": feedback_str.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "created_at": created.to_rfc3339(),
                    "workflow_architecture": r.get::<_, Option<String>>(12),
                    "step_count": r.get::<_, Option<i64>>(13),
                    "verification_step_count": r.get::<_, Option<i64>>(14),
                    "agentic_step_count": r.get::<_, Option<i64>>(15),
                    "has_ui_bridge": r.get::<_, Option<bool>>(16),
                    "total_tokens": r.get::<_, Option<i64>>(17),
                    "total_cost_usd": r.get::<_, Option<f64>>(18),
                    "composite_agentic_score": r.get::<_, Option<f64>>(19),
                    "technology_tags": tech_tags.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "domain_tags": domain_tags.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "complexity_tier": r.get::<_, Option<String>>(22),
                })
            })
            .collect())
    }

    /// Get learning stats summary.
    pub async fn get_learning_stats_summary(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::learning::get_learning_stats_summary()
            .bind(&conn)
            .one()
            .await
            .map_err(|e| format!("PG get_learning_stats_summary: {}", e))?;

        Ok(serde_json::json!({
            "total_tasks": row.total,
            "success_count": row.successes,
            "failure_count": row.failures,
            "partial_count": row.partials,
            "success_rate": if row.total > 0 {
                row.successes as f64 / row.total as f64 * 100.0
            } else { 0.0 },
            "avg_duration_secs": row.avg_duration,
            "avg_iterations": row.avg_iterations,
            "avg_cost_usd": row.avg_cost,
        }))
    }

    /// Get learning outcomes with pagination.
    pub async fn get_learning_outcomes_paginated(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, task_id, status, duration_secs, iterations, strategy,
                       tools_used, files_modified, error_type, error_message, feedback, created_at,
                       workflow_architecture, step_count, verification_step_count,
                       agentic_step_count, has_ui_bridge,
                       total_tokens, total_cost_usd, composite_agentic_score,
                       technology_tags, domain_tags, complexity_tier
                FROM learning_outcomes
                ORDER BY created_at DESC
                LIMIT $1 OFFSET $2"#,
                &[&limit, &offset],
            )
            .await
            .map_err(|e| format!("PG get_learning_outcomes_paginated: {}", e))?;

        let results = rows
            .iter()
            .map(|r| {
                let created: chrono::DateTime<chrono::Utc> = r.get(11);
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "task_id": r.get::<_, String>(1),
                    "status": r.get::<_, String>(2),
                    "duration_secs": r.get::<_, Option<f64>>(3),
                    "iterations": r.get::<_, Option<i32>>(4),
                    "strategy": r.get::<_, Option<String>>(5),
                    "tools_used": r.get::<_, Option<String>>(6),
                    "files_modified": r.get::<_, Option<String>>(7),
                    "error_type": r.get::<_, Option<String>>(8),
                    "error_message": r.get::<_, Option<String>>(9),
                    "feedback": r.get::<_, Option<String>>(10),
                    "created_at": created.to_rfc3339(),
                    "workflow_architecture": r.get::<_, Option<String>>(12),
                    "step_count": r.get::<_, Option<i64>>(13),
                    "verification_step_count": r.get::<_, Option<i64>>(14),
                    "agentic_step_count": r.get::<_, Option<i64>>(15),
                    "has_ui_bridge": r.get::<_, Option<bool>>(16),
                    "total_tokens": r.get::<_, Option<i64>>(17),
                    "total_cost_usd": r.get::<_, Option<f64>>(18),
                    "composite_agentic_score": r.get::<_, Option<f64>>(19),
                    "technology_tags": r.get::<_, Option<String>>(20),
                    "domain_tags": r.get::<_, Option<String>>(21),
                    "complexity_tier": r.get::<_, Option<String>>(22),
                })
            })
            .collect();

        Ok(results)
    }

    /// Get learning statistics for a date range.
    pub async fn get_learning_stats_by_date_range(
        &self,
        start: &str,
        end: &str,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Parse the date strings into timestamps for PG comparison
        let start_ts = start.to_string();
        let end_ts = end.to_string();

        // Get counts by status
        let status_rows = conn
            .query(
                r#"SELECT status, COUNT(*) as count
                FROM learning_outcomes
                WHERE created_at >= $1::timestamptz AND created_at <= $2::timestamptz
                GROUP BY status"#,
                &[&start_ts, &end_ts],
            )
            .await
            .map_err(|e| format!("PG get_learning_stats_by_date_range (status): {}", e))?;

        let mut status_map = serde_json::Map::new();
        for row in &status_rows {
            let status: String = row.get(0);
            let count: i64 = row.get(1);
            status_map.insert(status, serde_json::json!(count));
        }

        // Get counts by strategy
        let strategy_rows = conn
            .query(
                r#"SELECT strategy, COUNT(*) as count
                FROM learning_outcomes
                WHERE created_at >= $1::timestamptz AND created_at <= $2::timestamptz AND strategy IS NOT NULL
                GROUP BY strategy
                ORDER BY count DESC"#,
                &[&start_ts, &end_ts],
            )
            .await
            .map_err(|e| format!("PG get_learning_stats_by_date_range (strategy): {}", e))?;

        let mut strategy_map = serde_json::Map::new();
        for row in &strategy_rows {
            let strategy: String = row.get(0);
            let count: i64 = row.get(1);
            strategy_map.insert(strategy, serde_json::json!(count));
        }

        // Get average stats
        let avg_row = conn
            .query_one(
                r#"SELECT AVG(duration_secs), AVG(iterations)::double precision, COUNT(*)
                FROM learning_outcomes
                WHERE created_at >= $1::timestamptz AND created_at <= $2::timestamptz"#,
                &[&start_ts, &end_ts],
            )
            .await
            .map_err(|e| format!("PG get_learning_stats_by_date_range (avg): {}", e))?;

        let avg_duration: Option<f64> = avg_row.try_get(0).unwrap_or_else(|e| {
            tracing::warn!("get_learning_stats_by_date_range: col 0 (avg_duration) decode error: {}", e);
            None
        });
        // AVG over integer iterations returns numeric in PG; cast ::double precision in SQL,
        // but still use try_get to avoid panic on unexpected type.
        let avg_iterations: Option<f64> = avg_row.try_get(1).unwrap_or_else(|e| {
            tracing::warn!("get_learning_stats_by_date_range: col 1 (avg_iterations) decode error: {}", e);
            None
        });
        let total: i64 = avg_row.get(2);

        Ok(serde_json::json!({
            "total": total,
            "by_status": status_map,
            "by_strategy": strategy_map,
            "avg_duration_secs": avg_duration,
            "avg_iterations": avg_iterations,
            "date_range": {
                "start": start,
                "end": end
            }
        }))
    }

    /// Get composite agentic score for a task run.
    pub async fn get_composite_agentic_score(&self, task_id: &str) -> Result<f64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT COALESCE(composite_agentic_score, 0.0) FROM learning_outcomes WHERE task_id = $1",
                &[&task_id],
            )
            .await
            .map_err(|e| format!("PG get_composite_agentic_score: {}", e))?;

        Ok(row.map(|r| r.get::<_, f64>(0)).unwrap_or(0.0))
    }
}
