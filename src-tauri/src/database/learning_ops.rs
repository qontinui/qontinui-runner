//! Learning system operations: outcomes, patterns, stats, and dashboard queries.
//!
//! Contains all CheckpointDb methods related to the learning system.

use chrono::Utc;
use rusqlite::params;

use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Learning System Operations
    // ========================================================================

    /// Record a task outcome for learning
    pub fn record_learning_outcome(
        &self,
        task_id: &str,
        status: &str,
        duration_secs: Option<f64>,
        iterations: Option<u32>,
        strategy: Option<&str>,
        tools_used: Option<&[String]>,
        files_modified: Option<&[String]>,
        error_type: Option<&str>,
        error_message: Option<&str>,
        feedback: Option<&serde_json::Value>,
        workflow_architecture: Option<&str>,
        step_count: Option<i64>,
        verification_step_count: Option<i64>,
        agentic_step_count: Option<i64>,
        has_ui_bridge: bool,
        total_tokens: Option<u64>,
        total_cost_usd: Option<f64>,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = format!("lo-{}", uuid::Uuid::new_v4());
        let now = Utc::now().to_rfc3339();

        let tools_json = tools_used.map(|t| serde_json::to_string(t).unwrap_or_default());
        let files_json = files_modified.map(|f| serde_json::to_string(f).unwrap_or_default());
        let feedback_json = feedback.map(|f| f.to_string());

        // Auto-enrich error_type from error_message if not provided
        let enriched_error_type = error_type.map(|s| s.to_string()).or_else(|| {
            error_message
                .filter(|msg| !msg.is_empty())
                .map(crate::orchestrator::learning_recorder::categorize_error)
        });

        // Ensure workflow_architecture is never NULL
        let architecture = workflow_architecture.unwrap_or("traditional");

        // Auto-enrich technology and domain tags from files_modified
        let files_vec: Vec<String> = files_modified
            .map(|f| f.to_vec())
            .unwrap_or_default();
        let technology_tags = crate::orchestrator::learning_recorder::infer_technology_tags(&files_vec);
        let technology_tags_json = serde_json::to_string(&technology_tags).unwrap_or_else(|_| "[]".into());

        let domain_tags = crate::orchestrator::learning_recorder::infer_domain_tags(
            &files_vec,
            strategy.unwrap_or(""),
            "", // no category available in this path
            has_ui_bridge,
        );
        let domain_tags_json = serde_json::to_string(&domain_tags).unwrap_or_else(|_| "[]".into());

        // Compute complexity tier
        let complexity_tier = crate::orchestrator::learning_recorder::compute_complexity_tier(
            step_count,
            iterations.unwrap_or(0),
            duration_secs.unwrap_or(0.0),
            agentic_step_count,
        );

        conn.execute(
            r#"
            INSERT INTO learning_outcomes (
                id, task_id, status, duration_secs, iterations, strategy,
                tools_used, files_modified, error_type, error_message, feedback,
                workflow_architecture, step_count, verification_step_count,
                agentic_step_count, has_ui_bridge, total_tokens, total_cost_usd,
                technology_tags, domain_tags, complexity_tier, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
            "#,
            params![
                id,
                task_id,
                status,
                duration_secs,
                iterations,
                strategy,
                tools_json,
                files_json,
                enriched_error_type,
                error_message,
                feedback_json,
                architecture,
                step_count,
                verification_step_count,
                agentic_step_count,
                has_ui_bridge as i32,
                total_tokens.map(|t| t as i64),
                total_cost_usd,
                technology_tags_json,
                domain_tags_json,
                complexity_tier,
                now
            ],
        )
        .map_err(|e| format!("Failed to record learning outcome: {}", e))?;

        Ok(id)
    }

    /// Get learning outcomes for analysis
    pub fn get_learning_outcomes(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;
        let limit_val = limit.unwrap_or(100) as i64;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_id, status, duration_secs, iterations, strategy,
                       tools_used, files_modified, error_type, error_message, feedback, created_at,
                       workflow_architecture, step_count, verification_step_count,
                       agentic_step_count, has_ui_bridge,
                       total_tokens, total_cost_usd, composite_agentic_score,
                       technology_tags, domain_tags, complexity_tier
                FROM learning_outcomes
                ORDER BY created_at DESC
                LIMIT ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![limit_val], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "status": row.get::<_, String>(2)?,
                    "duration_secs": row.get::<_, Option<f64>>(3)?,
                    "iterations": row.get::<_, Option<i64>>(4)?,
                    "strategy": row.get::<_, Option<String>>(5)?,
                    "tools_used": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "files_modified": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error_type": row.get::<_, Option<String>>(8)?,
                    "error_message": row.get::<_, Option<String>>(9)?,
                    "feedback": row.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "created_at": row.get::<_, String>(11)?,
                    "workflow_architecture": row.get::<_, Option<String>>(12)?,
                    "step_count": row.get::<_, Option<i64>>(13)?,
                    "verification_step_count": row.get::<_, Option<i64>>(14)?,
                    "agentic_step_count": row.get::<_, Option<i64>>(15)?,
                    "has_ui_bridge": row.get::<_, Option<i32>>(16)?.map(|v| v != 0),
                    "total_tokens": row.get::<_, Option<i64>>(17)?,
                    "total_cost_usd": row.get::<_, Option<f64>>(18)?,
                    "composite_agentic_score": row.get::<_, Option<f64>>(19)?,
                    "technology_tags": row.get::<_, Option<String>>(20)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "domain_tags": row.get::<_, Option<String>>(21)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "complexity_tier": row.get::<_, Option<String>>(22)?,
                }))
            })
            .map_err(|e| format!("Failed to get learning outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Save or update a learning pattern
    pub fn save_learning_pattern(
        &self,
        id: &str,
        pattern_type: &str,
        description: &str,
        confidence: f64,
        occurrences: u32,
        context: Option<&serde_json::Value>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let context_json = context.map(|c| c.to_string());

        conn.execute(
            r#"
            INSERT INTO learning_patterns (id, pattern_type, description, confidence, occurrences, context, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            ON CONFLICT(id) DO UPDATE SET
                description = ?3,
                confidence = ?4,
                occurrences = ?5,
                context = ?6,
                updated_at = ?7
            "#,
            params![id, pattern_type, description, confidence, occurrences, context_json, now],
        )
        .map_err(|e| format!("Failed to save learning pattern: {}", e))?;

        Ok(())
    }

    /// Get all learning patterns
    pub fn get_learning_patterns(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, pattern_type, description, confidence, occurrences, context, created_at, updated_at
                FROM learning_patterns
                ORDER BY confidence DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "pattern_type": row.get::<_, String>(1)?,
                    "description": row.get::<_, String>(2)?,
                    "confidence": row.get::<_, f64>(3)?,
                    "occurrences": row.get::<_, i64>(4)?,
                    "context": row.get::<_, Option<String>>(5)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "created_at": row.get::<_, String>(6)?,
                    "updated_at": row.get::<_, String>(7)?,
                }))
            })
            .map_err(|e| format!("Failed to get learning patterns: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Enhanced Learning Queries (Filtering, Pagination, Date Ranges)
    // ========================================================================

    /// Get learning outcomes with optional filtering by status, strategy, and date.
    pub fn get_learning_outcomes_filtered(
        &self,
        status: Option<&str>,
        strategy: Option<&str>,
        since: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;
        let limit_val = limit.unwrap_or(100);

        // Build dynamic WHERE clause
        let mut conditions = Vec::new();
        if status.is_some() {
            conditions.push("status = ?1");
        }
        if strategy.is_some() {
            conditions.push("strategy = ?2");
        }
        if since.is_some() {
            conditions.push("created_at >= ?3");
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
            LIMIT ?4
            "#,
            where_clause
        );

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(
                params![
                    status.unwrap_or(""),
                    strategy.unwrap_or(""),
                    since.unwrap_or(""),
                    limit_val
                ],
                |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "task_id": row.get::<_, String>(1)?,
                        "status": row.get::<_, String>(2)?,
                        "duration_secs": row.get::<_, Option<f64>>(3)?,
                        "iterations": row.get::<_, Option<i64>>(4)?,
                        "strategy": row.get::<_, Option<String>>(5)?,
                        "tools_used": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                        "files_modified": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                        "error_type": row.get::<_, Option<String>>(8)?,
                        "error_message": row.get::<_, Option<String>>(9)?,
                        "feedback": row.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                        "created_at": row.get::<_, String>(11)?,
                        "workflow_architecture": row.get::<_, Option<String>>(12)?,
                        "step_count": row.get::<_, Option<i64>>(13)?,
                        "verification_step_count": row.get::<_, Option<i64>>(14)?,
                        "agentic_step_count": row.get::<_, Option<i64>>(15)?,
                        "has_ui_bridge": row.get::<_, Option<i32>>(16)?.map(|v| v != 0),
                        "total_tokens": row.get::<_, Option<i64>>(17)?,
                        "total_cost_usd": row.get::<_, Option<f64>>(18)?,
                        "composite_agentic_score": row.get::<_, Option<f64>>(19)?,
                        "technology_tags": row.get::<_, Option<String>>(20)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                        "domain_tags": row.get::<_, Option<String>>(21)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                        "complexity_tier": row.get::<_, Option<String>>(22)?,
                    }))
                },
            )
            .map_err(|e| format!("Failed to get learning outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get learning outcomes with pagination support.
    pub fn get_learning_outcomes_paginated(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_id, status, duration_secs, iterations, strategy,
                       tools_used, files_modified, error_type, error_message, feedback, created_at,
                       workflow_architecture, step_count, verification_step_count,
                       agentic_step_count, has_ui_bridge,
                       total_tokens, total_cost_usd, composite_agentic_score,
                       technology_tags, domain_tags, complexity_tier
                FROM learning_outcomes
                ORDER BY created_at DESC
                LIMIT ?1 OFFSET ?2
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![limit, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "status": row.get::<_, String>(2)?,
                    "duration_secs": row.get::<_, Option<f64>>(3)?,
                    "iterations": row.get::<_, Option<i64>>(4)?,
                    "strategy": row.get::<_, Option<String>>(5)?,
                    "tools_used": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "files_modified": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error_type": row.get::<_, Option<String>>(8)?,
                    "error_message": row.get::<_, Option<String>>(9)?,
                    "feedback": row.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "created_at": row.get::<_, String>(11)?,
                    "workflow_architecture": row.get::<_, Option<String>>(12)?,
                    "step_count": row.get::<_, Option<i64>>(13)?,
                    "verification_step_count": row.get::<_, Option<i64>>(14)?,
                    "agentic_step_count": row.get::<_, Option<i64>>(15)?,
                    "has_ui_bridge": row.get::<_, Option<i32>>(16)?.map(|v| v != 0),
                    "total_tokens": row.get::<_, Option<i64>>(17)?,
                    "total_cost_usd": row.get::<_, Option<f64>>(18)?,
                    "composite_agentic_score": row.get::<_, Option<f64>>(19)?,
                    "technology_tags": row.get::<_, Option<String>>(20)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "domain_tags": row.get::<_, Option<String>>(21)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "complexity_tier": row.get::<_, Option<String>>(22)?,
                }))
            })
            .map_err(|e| format!("Failed to get learning outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get learning statistics for a date range.
    pub fn get_learning_stats_by_date_range(
        &self,
        start: &str,
        end: &str,
    ) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;

        // Get counts by status
        let mut status_stmt = conn
            .prepare(
                r#"
                SELECT status, COUNT(*) as count
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2
                GROUP BY status
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let status_counts: Vec<(String, i64)> = status_stmt
            .query_map(params![start, end], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("Failed to get status counts: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Get counts by strategy
        let mut strategy_stmt = conn
            .prepare(
                r#"
                SELECT strategy, COUNT(*) as count
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2 AND strategy IS NOT NULL
                GROUP BY strategy
                ORDER BY count DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let strategy_counts: Vec<(String, i64)> = strategy_stmt
            .query_map(params![start, end], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("Failed to get strategy counts: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Get average duration and iterations
        let avg_stats: (Option<f64>, Option<f64>) = conn
            .query_row(
                r#"
                SELECT AVG(duration_secs), AVG(iterations)
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2
                "#,
                params![start, end],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((None, None));

        // Get total count
        let total: i64 = conn
            .query_row(
                r#"
                SELECT COUNT(*)
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2
                "#,
                params![start, end],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Convert to JSON
        let mut status_map = serde_json::Map::new();
        for (status, count) in status_counts {
            status_map.insert(status, serde_json::json!(count));
        }

        let mut strategy_map = serde_json::Map::new();
        for (strategy, count) in strategy_counts {
            strategy_map.insert(strategy, serde_json::json!(count));
        }

        Ok(serde_json::json!({
            "total": total,
            "by_status": status_map,
            "by_strategy": strategy_map,
            "avg_duration_secs": avg_stats.0,
            "avg_iterations": avg_stats.1,
            "date_range": {
                "start": start,
                "end": end
            }
        }))
    }

    /// Get total count of learning outcomes (for pagination).
    pub fn get_learning_outcomes_count(&self) -> Result<i64, String> {
        let conn = self.get_conn()?;
        conn.query_row("SELECT COUNT(*) FROM learning_outcomes", [], |row| {
            row.get(0)
        })
        .map_err(|e| format!("Failed to get count: {}", e))
    }

    // ========================================================================
    // Task Run with Learning Outcome Queries (for Dashboard Integration)
    // ========================================================================

    /// Get recent task runs with their learning outcomes joined.
    /// Returns task runs along with any associated learning outcome data.
    pub fn get_recent_task_runs_with_outcomes(
        &self,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    t.id, t.task_name, t.prompt, t.task_type, t.status,
                    t.sessions_count, t.max_sessions, t.error_message,
                    COALESCE(t.summary, t.ai_summary) as summary,
                    t.goal_achieved, t.remaining_work,
                    t.created_at, t.updated_at, t.completed_at,
                    l.id as outcome_id, l.status as outcome_status,
                    l.duration_secs, l.iterations, l.strategy,
                    l.tools_used, l.files_modified, l.error_type, l.error_message as outcome_error,
                    l.feedback, l.created_at as outcome_created_at
                FROM task_runs t
                LEFT JOIN learning_outcomes l ON t.id = l.task_id
                ORDER BY t.updated_at DESC
                LIMIT ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results = stmt
            .query_map(params![limit], |row| {
                // Parse tools_used JSON if present
                let tools_used: Option<serde_json::Value> = row
                    .get::<_, Option<String>>(19)?
                    .and_then(|s| serde_json::from_str(&s).ok());

                // Parse files_modified JSON if present
                let files_modified: Option<serde_json::Value> = row
                    .get::<_, Option<String>>(20)?
                    .and_then(|s| serde_json::from_str(&s).ok());

                // Parse feedback JSON if present
                let feedback: Option<serde_json::Value> = row
                    .get::<_, Option<String>>(23)?
                    .and_then(|s| serde_json::from_str(&s).ok());

                Ok(serde_json::json!({
                    "task": {
                        "id": row.get::<_, String>(0)?,
                        "task_name": row.get::<_, String>(1)?,
                        "prompt": row.get::<_, Option<String>>(2)?,
                        "task_type": row.get::<_, Option<String>>(3)?,
                        "status": row.get::<_, String>(4)?,
                        "sessions_count": row.get::<_, i64>(5)?,
                        "max_sessions": row.get::<_, Option<i64>>(6)?,
                        "error_message": row.get::<_, Option<String>>(7)?,
                        "summary": row.get::<_, Option<String>>(8)?,
                        "goal_achieved": row.get::<_, Option<i32>>(9)?.map(|v| v != 0),
                        "remaining_work": row.get::<_, Option<String>>(10)?,
                        "created_at": row.get::<_, String>(11)?,
                        "updated_at": row.get::<_, String>(12)?,
                        "completed_at": row.get::<_, Option<String>>(13)?,
                    },
                    "learning_outcome": if row.get::<_, Option<String>>(14)?.is_some() {
                        Some(serde_json::json!({
                            "id": row.get::<_, Option<String>>(14)?,
                            "status": row.get::<_, Option<String>>(15)?,
                            "duration_secs": row.get::<_, Option<f64>>(16)?,
                            "iterations": row.get::<_, Option<i64>>(17)?,
                            "strategy": row.get::<_, Option<String>>(18)?,
                            "tools_used": tools_used,
                            "files_modified": files_modified,
                            "error_type": row.get::<_, Option<String>>(21)?,
                            "error_message": row.get::<_, Option<String>>(22)?,
                            "feedback": feedback,
                            "created_at": row.get::<_, Option<String>>(24)?,
                        }))
                    } else {
                        None
                    }
                }))
            })
            .map_err(|e| format!("Failed to query task runs with outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get the most recent task run with checkpoints (for auto-selection in checkpoint browser).
    pub fn get_most_recent_task_with_checkpoints(&self) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;

        let result: Result<String, _> = conn.query_row(
            r#"
            SELECT DISTINCT t.id
            FROM task_runs t
            INNER JOIN orchestrator_checkpoints c ON t.id = c.task_id
            ORDER BY t.updated_at DESC
            LIMIT 1
            "#,
            [],
            |row| row.get(0),
        );

        match result {
            Ok(task_id) => Ok(Some(task_id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!(
                "Failed to get most recent task with checkpoints: {}",
                e
            )),
        }
    }

    /// Get learning statistics summary (for dashboard cards).
    pub fn get_learning_stats_summary(&self) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;

        // Get counts by status
        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    COUNT(*) as total,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as success_count,
                    SUM(CASE WHEN status = 'failure' THEN 1 ELSE 0 END) as failure_count,
                    SUM(CASE WHEN status = 'partial' THEN 1 ELSE 0 END) as partial_count,
                    AVG(duration_secs) as avg_duration,
                    AVG(iterations) as avg_iterations
                FROM learning_outcomes
                "#,
            )
            .map_err(|e| format!("Failed to prepare stats query: {}", e))?;

        let stats = stmt
            .query_row([], |row| {
                let total: i64 = row.get(0)?;
                let success: i64 = row.get(1)?;
                let failure: i64 = row.get(2)?;
                let partial: i64 = row.get(3)?;
                let avg_duration: Option<f64> = row.get(4)?;
                let avg_iterations: Option<f64> = row.get(5)?;

                let success_rate = if total > 0 {
                    (success as f64 / total as f64) * 100.0
                } else {
                    0.0
                };

                Ok(serde_json::json!({
                    "total_tasks": total,
                    "success_count": success,
                    "failure_count": failure,
                    "partial_count": partial,
                    "success_rate": success_rate,
                    "avg_duration_secs": avg_duration,
                    "avg_iterations": avg_iterations,
                }))
            })
            .map_err(|e| format!("Failed to get learning stats: {}", e))?;

        Ok(stats)
    }
}
