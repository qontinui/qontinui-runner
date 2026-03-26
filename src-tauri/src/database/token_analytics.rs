//! Token usage analytics queries.
//!
//! Provides aggregated analytics on AI token usage and costs from the
//! `phase_token_usage` table. Used by the LLM Observability dashboard.

use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::CheckpointDb;

// ============================================================================
// Analytics row types
// ============================================================================

/// Daily cost aggregation row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyCostRow {
    pub date: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub call_count: u64,
}

/// Cost aggregation by model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCostRow {
    pub model_used: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub call_count: u64,
    pub avg_duration_ms: Option<u64>,
}

/// Cost aggregation by workflow phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseCostRow {
    pub phase: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

/// Latency stats by provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderLatencyRow {
    pub provider_used: String,
    pub avg_duration_ms: u64,
    pub min_duration_ms: u64,
    pub max_duration_ms: u64,
    pub call_count: u64,
}

/// Per-task-run cost aggregation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunCostRow {
    pub task_run_id: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub call_count: u64,
    pub started_at: String,
}

/// Cost aggregation by target app (UI Bridge automation target).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetAppCostRow {
    pub target_app: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub call_count: u64,
    pub avg_duration_ms: Option<u64>,
}

/// Cost aggregation by target page URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetPageCostRow {
    pub target_page_url: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub call_count: u64,
}

/// Cost per successful UI Bridge interaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostPerInteractionRow {
    pub task_run_id: String,
    pub total_cost_cents: u64,
    pub successful_interactions: u64,
    pub cost_per_interaction_cents: f64,
}

/// Page complexity: cost breakdown by page URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageComplexityRow {
    pub target_page_url: String,
    pub call_count: u64,
    pub total_cost_cents: u64,
    pub avg_cost_per_call_cents: f64,
}

/// Model × action type success rate and cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelActionRow {
    pub model: String,
    pub action: String,
    pub total: u64,
    pub success_rate: f64,
    pub avg_cost_cents: f64,
}

/// Overall token usage summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageSummary {
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_calls: u64,
    pub unique_models: u64,
    pub unique_providers: u64,
    pub avg_cost_per_call_cents: f64,
    pub avg_duration_ms: Option<f64>,
}

// ============================================================================
// CheckpointDb analytics methods
// ============================================================================

impl CheckpointDb {
    /// Get daily cost breakdown for the last N days.
    pub fn get_daily_cost(&self, days: u32) -> Result<Vec<DailyCostRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT date(created_at) as day,
                       COALESCE(SUM(cost_cents), 0),
                       COALESCE(SUM(input_tokens), 0),
                       COALESCE(SUM(output_tokens), 0),
                       COUNT(*)
                FROM phase_token_usage
                WHERE created_at >= datetime('now', ?1)
                GROUP BY date(created_at)
                ORDER BY day ASC"#,
            )
            .map_err(|e| format!("Failed to prepare daily cost query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_param], |row| {
                Ok(DailyCostRow {
                    date: row.get(0)?,
                    total_cost_cents: row.get::<_, i64>(1)? as u64,
                    total_input_tokens: row.get::<_, i64>(2)? as u64,
                    total_output_tokens: row.get::<_, i64>(3)? as u64,
                    call_count: row.get::<_, i64>(4)? as u64,
                })
            })
            .map_err(|e| format!("Failed to query daily cost: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect daily cost rows: {}", e))
    }

    /// Get cost breakdown by model for the last N days.
    pub fn get_cost_by_model(&self, days: u32) -> Result<Vec<ModelCostRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT model_used,
                       COALESCE(SUM(cost_cents), 0),
                       COALESCE(SUM(input_tokens), 0),
                       COALESCE(SUM(output_tokens), 0),
                       COUNT(*),
                       AVG(duration_ms)
                FROM phase_token_usage
                WHERE created_at >= datetime('now', ?1)
                  AND model_used IS NOT NULL
                GROUP BY model_used
                ORDER BY SUM(cost_cents) DESC"#,
            )
            .map_err(|e| format!("Failed to prepare cost by model query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_param], |row| {
                Ok(ModelCostRow {
                    model_used: row.get(0)?,
                    total_cost_cents: row.get::<_, i64>(1)? as u64,
                    total_input_tokens: row.get::<_, i64>(2)? as u64,
                    total_output_tokens: row.get::<_, i64>(3)? as u64,
                    call_count: row.get::<_, i64>(4)? as u64,
                    avg_duration_ms: row.get::<_, Option<f64>>(5)?.map(|v| v as u64),
                })
            })
            .map_err(|e| format!("Failed to query cost by model: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect model cost rows: {}", e))
    }

    /// Get cost breakdown by workflow phase for the last N days.
    pub fn get_cost_by_phase(&self, days: u32) -> Result<Vec<PhaseCostRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT phase,
                       COALESCE(SUM(cost_cents), 0),
                       COALESCE(SUM(input_tokens), 0),
                       COALESCE(SUM(output_tokens), 0)
                FROM phase_token_usage
                WHERE created_at >= datetime('now', ?1)
                GROUP BY phase
                ORDER BY SUM(cost_cents) DESC"#,
            )
            .map_err(|e| format!("Failed to prepare cost by phase query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_param], |row| {
                Ok(PhaseCostRow {
                    phase: row.get(0)?,
                    total_cost_cents: row.get::<_, i64>(1)? as u64,
                    total_input_tokens: row.get::<_, i64>(2)? as u64,
                    total_output_tokens: row.get::<_, i64>(3)? as u64,
                })
            })
            .map_err(|e| format!("Failed to query cost by phase: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect phase cost rows: {}", e))
    }

    /// Get latency stats by provider for the last N days.
    pub fn get_provider_latency(&self, days: u32) -> Result<Vec<ProviderLatencyRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT provider_used,
                       CAST(AVG(duration_ms) AS INTEGER),
                       MIN(duration_ms),
                       MAX(duration_ms),
                       COUNT(*)
                FROM phase_token_usage
                WHERE created_at >= datetime('now', ?1)
                  AND provider_used IS NOT NULL
                  AND duration_ms IS NOT NULL
                GROUP BY provider_used"#,
            )
            .map_err(|e| format!("Failed to prepare provider latency query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_param], |row| {
                Ok(ProviderLatencyRow {
                    provider_used: row.get(0)?,
                    avg_duration_ms: row.get::<_, i64>(1)? as u64,
                    min_duration_ms: row.get::<_, i64>(2)? as u64,
                    max_duration_ms: row.get::<_, i64>(3)? as u64,
                    call_count: row.get::<_, i64>(4)? as u64,
                })
            })
            .map_err(|e| format!("Failed to query provider latency: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect provider latency rows: {}", e))
    }

    /// Get per-task-run cost breakdown for the last N days, limited to top N runs.
    pub fn get_task_run_costs(
        &self,
        days: u32,
        limit: u32,
    ) -> Result<Vec<TaskRunCostRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT task_run_id,
                       COALESCE(SUM(cost_cents), 0),
                       COALESCE(SUM(input_tokens), 0),
                       COALESCE(SUM(output_tokens), 0),
                       COUNT(*),
                       MIN(created_at)
                FROM phase_token_usage
                WHERE created_at >= datetime('now', ?1)
                GROUP BY task_run_id
                ORDER BY SUM(cost_cents) DESC
                LIMIT ?2"#,
            )
            .map_err(|e| format!("Failed to prepare task run costs query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_param, limit as i64], |row| {
                Ok(TaskRunCostRow {
                    task_run_id: row.get(0)?,
                    total_cost_cents: row.get::<_, i64>(1)? as u64,
                    total_input_tokens: row.get::<_, i64>(2)? as u64,
                    total_output_tokens: row.get::<_, i64>(3)? as u64,
                    call_count: row.get::<_, i64>(4)? as u64,
                    started_at: row.get(5)?,
                })
            })
            .map_err(|e| format!("Failed to query task run costs: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect task run cost rows: {}", e))
    }

    /// Get an aggregate summary of token usage for the last N days.
    pub fn get_token_usage_summary(&self, days: u32) -> Result<TokenUsageSummary, String> {
        let conn = self.get_conn()?;
        let days_param = format!("-{} days", days);

        let (total_cost, total_input, total_output, total_calls, avg_duration) = conn
            .query_row(
                r#"SELECT COALESCE(SUM(cost_cents), 0),
                       COALESCE(SUM(input_tokens), 0),
                       COALESCE(SUM(output_tokens), 0),
                       COUNT(*),
                       AVG(duration_ms)
                FROM phase_token_usage
                WHERE created_at >= datetime('now', ?1)"#,
                params![days_param],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as u64,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, i64>(2)? as u64,
                        row.get::<_, i64>(3)? as u64,
                        row.get::<_, Option<f64>>(4)?,
                    ))
                },
            )
            .map_err(|e| format!("Failed to query token usage summary: {}", e))?;

        let unique_models: u64 = conn
            .query_row(
                r#"SELECT COUNT(DISTINCT model_used)
                FROM phase_token_usage
                WHERE created_at >= datetime('now', ?1)
                  AND model_used IS NOT NULL"#,
                params![days_param],
                |row| Ok(row.get::<_, i64>(0)? as u64),
            )
            .map_err(|e| format!("Failed to query unique models: {}", e))?;

        let unique_providers: u64 = conn
            .query_row(
                r#"SELECT COUNT(DISTINCT provider_used)
                FROM phase_token_usage
                WHERE created_at >= datetime('now', ?1)
                  AND provider_used IS NOT NULL"#,
                params![days_param],
                |row| Ok(row.get::<_, i64>(0)? as u64),
            )
            .map_err(|e| format!("Failed to query unique providers: {}", e))?;

        let avg_cost_per_call = if total_calls > 0 {
            total_cost as f64 / total_calls as f64
        } else {
            0.0
        };

        Ok(TokenUsageSummary {
            total_cost_cents: total_cost,
            total_input_tokens: total_input,
            total_output_tokens: total_output,
            total_calls,
            unique_models,
            unique_providers,
            avg_cost_per_call_cents: avg_cost_per_call,
            avg_duration_ms: avg_duration,
        })
    }

    /// Get cost breakdown by target app (UI Bridge automation target).
    pub fn get_cost_by_target_app(&self, days: u32) -> Result<Vec<TargetAppCostRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT COALESCE(target_app, '(no app)'),
                       COALESCE(SUM(cost_cents), 0),
                       COALESCE(SUM(input_tokens), 0),
                       COALESCE(SUM(output_tokens), 0),
                       COUNT(*),
                       AVG(duration_ms)
                FROM phase_token_usage
                WHERE created_at >= datetime('now', ?1)
                  AND target_app IS NOT NULL
                GROUP BY target_app
                ORDER BY SUM(cost_cents) DESC"#,
            )
            .map_err(|e| format!("Failed to prepare cost by target app query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_param], |row| {
                Ok(TargetAppCostRow {
                    target_app: row.get(0)?,
                    total_cost_cents: row.get::<_, i64>(1)? as u64,
                    total_input_tokens: row.get::<_, i64>(2)? as u64,
                    total_output_tokens: row.get::<_, i64>(3)? as u64,
                    call_count: row.get::<_, i64>(4)? as u64,
                    avg_duration_ms: row.get::<_, Option<f64>>(5)?.map(|v| v as u64),
                })
            })
            .map_err(|e| format!("Failed to query cost by target app: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect target app cost rows: {}", e))
    }

    /// Get cost breakdown by target page URL.
    pub fn get_cost_by_target_page(&self, days: u32) -> Result<Vec<TargetPageCostRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT COALESCE(target_page_url, '(no page)'),
                       COALESCE(SUM(cost_cents), 0),
                       COALESCE(SUM(input_tokens), 0),
                       COALESCE(SUM(output_tokens), 0),
                       COUNT(*)
                FROM phase_token_usage
                WHERE created_at >= datetime('now', ?1)
                  AND target_page_url IS NOT NULL
                GROUP BY target_page_url
                ORDER BY SUM(cost_cents) DESC
                LIMIT 50"#,
            )
            .map_err(|e| format!("Failed to prepare cost by target page query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_param], |row| {
                Ok(TargetPageCostRow {
                    target_page_url: row.get(0)?,
                    total_cost_cents: row.get::<_, i64>(1)? as u64,
                    total_input_tokens: row.get::<_, i64>(2)? as u64,
                    total_output_tokens: row.get::<_, i64>(3)? as u64,
                    call_count: row.get::<_, i64>(4)? as u64,
                })
            })
            .map_err(|e| format!("Failed to query cost by target page: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect target page cost rows: {}", e))
    }

    /// Cost per successful UI Bridge interaction per task run.
    pub fn get_cost_per_interaction(&self, days: u32) -> Result<Vec<CostPerInteractionRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT p.task_run_id,
                       COALESCE(SUM(p.cost_cents), 0) AS total_cost,
                       (SELECT COUNT(*) FROM ui_bridge_events e
                        WHERE CAST(e.task_run_id AS TEXT) = p.task_run_id
                          AND e.success = 1
                          AND e.event_type = 'action_executed') AS successes,
                       CASE WHEN (SELECT COUNT(*) FROM ui_bridge_events e
                                  WHERE CAST(e.task_run_id AS TEXT) = p.task_run_id
                                    AND e.success = 1
                                    AND e.event_type = 'action_executed') > 0
                            THEN CAST(COALESCE(SUM(p.cost_cents), 0) AS REAL) /
                                 (SELECT COUNT(*) FROM ui_bridge_events e
                                  WHERE CAST(e.task_run_id AS TEXT) = p.task_run_id
                                    AND e.success = 1
                                    AND e.event_type = 'action_executed')
                            ELSE 0 END AS cost_per_success
                FROM phase_token_usage p
                WHERE p.created_at >= datetime('now', ?1)
                GROUP BY p.task_run_id
                HAVING successes > 0
                ORDER BY cost_per_success DESC
                LIMIT 50"#,
            )
            .map_err(|e| format!("Failed to prepare cost per interaction query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_param], |row| {
                Ok(CostPerInteractionRow {
                    task_run_id: row.get(0)?,
                    total_cost_cents: row.get::<_, i64>(1)? as u64,
                    successful_interactions: row.get::<_, i64>(2)? as u64,
                    cost_per_interaction_cents: row.get::<_, f64>(3)?,
                })
            })
            .map_err(|e| format!("Failed to query cost per interaction: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect cost per interaction rows: {}", e))
    }

    /// Page complexity: average cost-per-action grouped by target page URL.
    pub fn get_page_complexity_scores(&self, days: u32) -> Result<Vec<PageComplexityRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT target_page_url,
                       COUNT(*) AS call_count,
                       COALESCE(SUM(cost_cents), 0) AS total_cost,
                       COALESCE(AVG(cost_cents), 0) AS avg_cost_per_call
                FROM phase_token_usage
                WHERE created_at >= datetime('now', ?1)
                  AND target_page_url IS NOT NULL
                GROUP BY target_page_url
                ORDER BY avg_cost_per_call DESC
                LIMIT 50"#,
            )
            .map_err(|e| format!("Failed to prepare page complexity query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_param], |row| {
                Ok(PageComplexityRow {
                    target_page_url: row.get(0)?,
                    call_count: row.get::<_, i64>(1)? as u64,
                    total_cost_cents: row.get::<_, i64>(2)? as u64,
                    avg_cost_per_call_cents: row.get::<_, f64>(3)?,
                })
            })
            .map_err(|e| format!("Failed to query page complexity: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect page complexity rows: {}", e))
    }

    /// Model × action success rate matrix.
    pub fn get_model_action_success_rates(&self, days: u32) -> Result<Vec<ModelActionRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT p.model_used, e.action,
                       COUNT(*) AS total,
                       CAST(SUM(CASE WHEN e.success = 1 THEN 1 ELSE 0 END) AS REAL) / COUNT(*) AS success_rate,
                       COALESCE(AVG(p.cost_cents), 0) AS avg_cost
                FROM phase_token_usage p
                JOIN ui_bridge_events e ON CAST(e.task_run_id AS TEXT) = p.task_run_id
                WHERE p.created_at >= datetime('now', ?1)
                  AND p.model_used IS NOT NULL
                  AND e.event_type = 'action_executed'
                  AND e.action IS NOT NULL
                GROUP BY p.model_used, e.action
                HAVING total >= 3
                ORDER BY p.model_used, e.action
                LIMIT 100"#,
            )
            .map_err(|e| format!("Failed to prepare model-action matrix query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_param], |row| {
                Ok(ModelActionRow {
                    model: row.get(0)?,
                    action: row.get(1)?,
                    total: row.get::<_, i64>(2)? as u64,
                    success_rate: row.get(3)?,
                    avg_cost_cents: row.get::<_, f64>(4)?,
                })
            })
            .map_err(|e| format!("Failed to query model-action matrix: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect model-action rows: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_db() -> CheckpointDb {
        let db = CheckpointDb::new_in_memory().expect("Failed to create in-memory DB");
        seed_test_data(&db);
        db
    }

    fn seed_test_data(db: &CheckpointDb) {
        let conn = db.get_conn_string().expect("Failed to get connection");

        // Insert a task_run (needed for FK constraint)
        conn.execute(
            "INSERT INTO task_runs (id, task_name, prompt, status, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, datetime('now'), datetime('now'))",
            rusqlite::params!["run_1", "test-task", "test prompt", "completed"],
        ).expect("Failed to insert task_run");

        conn.execute(
            "INSERT INTO task_runs (id, task_name, prompt, status, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, datetime('now'), datetime('now'))",
            rusqlite::params!["run_2", "test-task-2", "test prompt 2", "completed"],
        ).expect("Failed to insert task_run 2");

        // Insert phase_token_usage rows
        let insert_sql = r#"
            INSERT INTO phase_token_usage
                (task_run_id, phase, model_used, provider_used, input_tokens, output_tokens, cost_cents, duration_ms, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))
        "#;

        conn.execute(insert_sql, rusqlite::params![
            "run_1", "reflection", "gpt-4", "openai", 1000, 500, 10, 200
        ]).unwrap();
        conn.execute(insert_sql, rusqlite::params![
            "run_1", "execution", "gpt-4", "openai", 2000, 1000, 20, 300
        ]).unwrap();
        conn.execute(insert_sql, rusqlite::params![
            "run_2", "reflection", "claude-3", "anthropic", 1500, 800, 15, 150
        ]).unwrap();
    }

    #[test]
    fn get_daily_cost_returns_rows() {
        let db = create_test_db();
        let result = db.get_daily_cost(30).unwrap();
        assert!(!result.is_empty());
        // Should have 1 day (today)
        assert_eq!(result.len(), 1);
        let row = &result[0];
        assert_eq!(row.total_cost_cents, 45); // 10 + 20 + 15
        assert_eq!(row.total_input_tokens, 4500); // 1000 + 2000 + 1500
        assert_eq!(row.total_output_tokens, 2300); // 500 + 1000 + 800
        assert_eq!(row.call_count, 3);
    }

    #[test]
    fn get_cost_by_model_groups_correctly() {
        let db = create_test_db();
        let result = db.get_cost_by_model(30).unwrap();
        assert_eq!(result.len(), 2); // gpt-4 and claude-3
        // Sorted by cost DESC: gpt-4 (30 cents) then claude-3 (15 cents)
        assert_eq!(result[0].model_used, "gpt-4");
        assert_eq!(result[0].total_cost_cents, 30);
        assert_eq!(result[0].call_count, 2);
        assert_eq!(result[1].model_used, "claude-3");
        assert_eq!(result[1].total_cost_cents, 15);
    }

    #[test]
    fn get_cost_by_phase_groups_correctly() {
        let db = create_test_db();
        let result = db.get_cost_by_phase(30).unwrap();
        assert_eq!(result.len(), 2); // reflection and execution
        // reflection: 10 + 15 = 25, execution: 20
        // Sorted by cost DESC
        assert_eq!(result[0].phase, "reflection");
        assert_eq!(result[0].total_cost_cents, 25);
        assert_eq!(result[1].phase, "execution");
        assert_eq!(result[1].total_cost_cents, 20);
    }

    #[test]
    fn get_provider_latency_returns_stats() {
        let db = create_test_db();
        let result = db.get_provider_latency(30).unwrap();
        assert_eq!(result.len(), 2); // openai and anthropic
        for row in &result {
            assert!(row.avg_duration_ms > 0);
            assert!(row.min_duration_ms <= row.max_duration_ms);
        }
    }

    #[test]
    fn get_task_run_costs_returns_per_run_breakdown() {
        let db = create_test_db();
        let result = db.get_task_run_costs(30, 10).unwrap();
        assert_eq!(result.len(), 2);
        // run_1 has higher cost (30) so comes first
        assert_eq!(result[0].task_run_id, "run_1");
        assert_eq!(result[0].total_cost_cents, 30);
        assert_eq!(result[0].call_count, 2);
        assert_eq!(result[1].task_run_id, "run_2");
        assert_eq!(result[1].total_cost_cents, 15);
    }

    #[test]
    fn get_task_run_costs_respects_limit() {
        let db = create_test_db();
        let result = db.get_task_run_costs(30, 1).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].task_run_id, "run_1"); // highest cost
    }

    #[test]
    fn get_token_usage_summary_aggregates_all() {
        let db = create_test_db();
        let summary = db.get_token_usage_summary(30).unwrap();
        assert_eq!(summary.total_cost_cents, 45);
        assert_eq!(summary.total_input_tokens, 4500);
        assert_eq!(summary.total_output_tokens, 2300);
        assert_eq!(summary.total_calls, 3);
        assert_eq!(summary.unique_models, 2);
        assert_eq!(summary.unique_providers, 2);
        assert!((summary.avg_cost_per_call_cents - 15.0).abs() < 0.01);
        assert!(summary.avg_duration_ms.is_some());
    }

    #[test]
    fn empty_db_returns_zero_summary() {
        let db = CheckpointDb::new_in_memory().expect("Failed to create in-memory DB");
        let summary = db.get_token_usage_summary(30).unwrap();
        assert_eq!(summary.total_cost_cents, 0);
        assert_eq!(summary.total_calls, 0);
        assert_eq!(summary.avg_cost_per_call_cents, 0.0);
    }

    #[test]
    fn get_daily_cost_empty_db_returns_empty_vec() {
        let db = CheckpointDb::new_in_memory().expect("Failed to create in-memory DB");
        let result = db.get_daily_cost(30).unwrap();
        assert!(result.is_empty());
    }
}
