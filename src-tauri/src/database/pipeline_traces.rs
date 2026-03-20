//! Persistence layer for pipeline agent traces.
//!
//! Saves `PipelineAgentTrace` data from multi-agent pipeline runs into
//! the `pipeline_agent_traces` table for later analysis by the meta-optimizer.

use rusqlite::{params, Connection};
use tracing::debug;

use crate::autoresearch::agentic_verification::PipelineAgentTrace;
use crate::database::CheckpointDb;

/// Save a batch of pipeline agent traces from a completed pipeline run.
pub fn save_pipeline_agent_traces(
    db: &CheckpointDb,
    task_run_id: &str,
    traces: &[PipelineAgentTrace],
) -> Result<usize, String> {
    if traces.is_empty() {
        return Ok(0);
    }

    let task_run_id = task_run_id.to_string();
    let traces = traces.to_vec();

    db.with_conn(move |conn| {
        let count = insert_traces(conn, &task_run_id, &traces)?;
        debug!(
            "Saved {} pipeline agent traces for task run {}",
            count, task_run_id
        );
        Ok(count)
    })
}

fn insert_traces(
    conn: &Connection,
    task_run_id: &str,
    traces: &[PipelineAgentTrace],
) -> Result<usize, String> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut count = 0;

    for trace in traces {
        let id = format!("pat-{}", uuid::Uuid::new_v4());
        let config_json = serde_json::to_string(&trace.config).unwrap_or_default();
        let input_json = trace.input_snapshot.to_string();
        let output_json = trace.output_snapshot.to_string();

        conn.execute(
            r#"INSERT INTO pipeline_agent_traces
               (id, task_run_id, agent_type, agent_id, run_id,
                input_snapshot, output_snapshot, config_json,
                duration_ms, tokens_in, tokens_out, cost_usd,
                downstream_success, output_quality_score, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"#,
            params![
                id,
                task_run_id,
                trace.agent_type,
                trace.agent_id,
                trace.run_id,
                input_json,
                output_json,
                config_json,
                trace.duration_ms as i64,
                trace.tokens_in as i64,
                trace.tokens_out as i64,
                trace.cost_usd,
                trace.downstream_success.map(|b| b as i32),
                trace.output_quality_score,
                now,
            ],
        )
        .map_err(|e| format!("Failed to insert pipeline agent trace: {}", e))?;

        count += 1;
    }

    Ok(count)
}

/// Get aggregate metrics for pipeline agent traces, grouped by agent type.
/// Used by the meta-optimizer to analyze agent performance.
pub fn get_agent_trace_aggregates(
    db: &CheckpointDb,
    limit: u32,
) -> Result<Vec<AgentTraceAggregate>, String> {
    db.with_conn(move |conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT
                    agent_type,
                    COUNT(*) as run_count,
                    AVG(duration_ms) as avg_duration_ms,
                    AVG(cost_usd) as avg_cost_usd,
                    SUM(CASE WHEN downstream_success = 1 THEN 1 ELSE 0 END) as success_count,
                    SUM(CASE WHEN downstream_success = 0 THEN 1 ELSE 0 END) as failure_count,
                    AVG(tokens_in) as avg_tokens_in,
                    AVG(tokens_out) as avg_tokens_out
                FROM pipeline_agent_traces
                WHERE id IN (
                    SELECT id FROM pipeline_agent_traces
                    ORDER BY created_at DESC
                    LIMIT ?1
                )
                GROUP BY agent_type
                ORDER BY agent_type"#,
            )
            .map_err(|e| format!("Failed to prepare aggregate query: {}", e))?;

        let results = stmt
            .query_map(params![limit as i64], |row| {
                Ok(AgentTraceAggregate {
                    agent_type: row.get(0)?,
                    run_count: row.get(1)?,
                    avg_duration_ms: row.get(2)?,
                    avg_cost_usd: row.get(3)?,
                    success_count: row.get(4)?,
                    failure_count: row.get(5)?,
                    avg_tokens_in: row.get(6)?,
                    avg_tokens_out: row.get(7)?,
                })
            })
            .map_err(|e| format!("Failed to query aggregates: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    })
}

/// Retrieve all pipeline agent traces for a given task run ID.
/// Used to attach traces to ExperimentResult.extra for the autoresearch UI.
pub fn get_traces_for_task_run(
    db: &CheckpointDb,
    task_run_id: &str,
) -> Result<Vec<PipelineAgentTrace>, String> {
    let task_run_id = task_run_id.to_string();

    db.with_conn(move |conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT agent_type, agent_id, run_id,
                          input_snapshot, output_snapshot, config_json,
                          duration_ms, tokens_in, tokens_out, cost_usd,
                          downstream_success, output_quality_score
                   FROM pipeline_agent_traces
                   WHERE task_run_id = ?1
                   ORDER BY created_at ASC"#,
            )
            .map_err(|e| format!("Failed to prepare trace query: {}", e))?;

        let results = stmt
            .query_map(params![task_run_id], |row| {
                let config_json: String = row.get(5)?;
                let input_json: String = row.get(3)?;
                let output_json: String = row.get(4)?;
                let downstream_success: Option<i32> = row.get(10)?;

                Ok(PipelineAgentTrace {
                    agent_type: row.get(0)?,
                    agent_id: row.get(1)?,
                    run_id: row.get(2)?,
                    input_snapshot: serde_json::from_str(&input_json).unwrap_or_default(),
                    output_snapshot: serde_json::from_str(&output_json).unwrap_or_default(),
                    config: serde_json::from_str(&config_json).unwrap_or_default(),
                    duration_ms: row.get::<_, i64>(6)? as u64,
                    tokens_in: row.get::<_, i64>(7)? as u32,
                    tokens_out: row.get::<_, i64>(8)? as u32,
                    cost_usd: row.get(9)?,
                    downstream_success: downstream_success.map(|v| v != 0),
                    output_quality_score: row.get(11)?,
                })
            })
            .map_err(|e| format!("Failed to query traces: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentTraceAggregate {
    pub agent_type: String,
    pub run_count: i64,
    pub avg_duration_ms: f64,
    pub avg_cost_usd: f64,
    pub success_count: i64,
    pub failure_count: i64,
    pub avg_tokens_in: f64,
    pub avg_tokens_out: f64,
}

/// Get aggregate metrics for a specific agent type within a time window.
/// Returns None if no traces exist in the window.
pub fn get_agent_aggregates_for_period(
    db: &CheckpointDb,
    agent_type: &str,
    start: &str,
    end: &str,
) -> Result<Option<AgentTraceAggregate>, String> {
    let agent_type = agent_type.to_string();
    let start = start.to_string();
    let end = end.to_string();

    db.with_conn(move |conn| {
        let result = conn.query_row(
            r#"SELECT
                agent_type,
                COUNT(*) as run_count,
                AVG(duration_ms) as avg_duration_ms,
                AVG(cost_usd) as avg_cost_usd,
                SUM(CASE WHEN downstream_success = 1 THEN 1 ELSE 0 END) as success_count,
                SUM(CASE WHEN downstream_success = 0 THEN 1 ELSE 0 END) as failure_count,
                AVG(tokens_in) as avg_tokens_in,
                AVG(tokens_out) as avg_tokens_out
            FROM pipeline_agent_traces
            WHERE agent_type = ?1 AND created_at >= ?2 AND created_at <= ?3"#,
            params![agent_type, start, end],
            |row| {
                let run_count: i64 = row.get(1)?;
                if run_count == 0 {
                    return Ok(None);
                }
                Ok(Some(AgentTraceAggregate {
                    agent_type: row.get(0)?,
                    run_count,
                    avg_duration_ms: row.get(2)?,
                    avg_cost_usd: row.get(3)?,
                    success_count: row.get(4)?,
                    failure_count: row.get(5)?,
                    avg_tokens_in: row.get(6)?,
                    avg_tokens_out: row.get(7)?,
                }))
            },
        );

        match result {
            Ok(agg) => Ok(agg),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!(
                "Failed to query agent aggregates for period: {}",
                e
            )),
        }
    })
}

/// Cost-effectiveness data point for a specific agent and time period.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CostEffectivenessPoint {
    pub agent_type: String,
    pub period_label: String,
    pub avg_cost_usd: f64,
    pub success_rate: f64,
    pub run_count: i64,
}

/// Get cost-effectiveness data points grouped by agent type and ISO week.
pub fn get_agent_cost_effectiveness(
    db: &CheckpointDb,
    agent_type: Option<&str>,
    days: i64,
) -> Result<Vec<CostEffectivenessPoint>, String> {
    let agent_type = agent_type.map(|s| s.to_string());
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();

    db.with_conn(move |conn| {
        let (sql, has_agent_filter) = if agent_type.is_some() {
            (
                r#"SELECT
                    agent_type,
                    strftime('%Y-W%W', created_at) as period_label,
                    AVG(cost_usd) as avg_cost_usd,
                    CAST(SUM(CASE WHEN downstream_success = 1 THEN 1 ELSE 0 END) AS REAL) /
                        CAST(COUNT(*) AS REAL) * 100.0 as success_rate,
                    COUNT(*) as run_count
                FROM pipeline_agent_traces
                WHERE created_at >= ?1 AND agent_type = ?2
                GROUP BY agent_type, period_label
                ORDER BY agent_type, period_label"#,
                true,
            )
        } else {
            (
                r#"SELECT
                    agent_type,
                    strftime('%Y-W%W', created_at) as period_label,
                    AVG(cost_usd) as avg_cost_usd,
                    CAST(SUM(CASE WHEN downstream_success = 1 THEN 1 ELSE 0 END) AS REAL) /
                        CAST(COUNT(*) AS REAL) * 100.0 as success_rate,
                    COUNT(*) as run_count
                FROM pipeline_agent_traces
                WHERE created_at >= ?1
                GROUP BY agent_type, period_label
                ORDER BY agent_type, period_label"#,
                false,
            )
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare cost effectiveness query: {}", e))?;

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<CostEffectivenessPoint> {
            Ok(CostEffectivenessPoint {
                agent_type: row.get(0)?,
                period_label: row.get(1)?,
                avg_cost_usd: row.get(2)?,
                success_rate: row.get(3)?,
                run_count: row.get(4)?,
            })
        };

        let results: Vec<CostEffectivenessPoint> = if has_agent_filter {
            stmt.query_map(params![cutoff, agent_type.unwrap()], map_row)
                .map_err(|e| format!("Failed to query cost effectiveness: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map(params![cutoff], map_row)
                .map_err(|e| format!("Failed to query cost effectiveness: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        Ok(results)
    })
}

/// Interaction between two agent types based on correlation of quality/success.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentInteraction {
    pub source_agent: String,
    pub target_agent: String,
    pub correlation: f64,
    pub sample_size: i64,
}

/// Get the interaction matrix showing correlations between agent pairs.
/// Computes Pearson correlation between source output_quality_score and target downstream_success.
pub fn get_agent_interaction_matrix(
    db: &CheckpointDb,
    days: i64,
) -> Result<Vec<AgentInteraction>, String> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();

    db.with_conn(move |conn| {
        // Self-join on run_id to get pairs of agents in the same pipeline run.
        // Compute Pearson correlation between source's output_quality_score
        // and target's downstream_success.
        let mut stmt = conn
            .prepare(
                r#"WITH recent AS (
                    SELECT id, agent_type, run_id, output_quality_score, downstream_success
                    FROM pipeline_agent_traces
                    WHERE created_at >= ?1
                    ORDER BY created_at DESC
                    LIMIT 10000
                )
                SELECT
                    s.agent_type as source_agent,
                    t.agent_type as target_agent,
                    COUNT(*) as sample_size,
                    (COUNT(*) * SUM(COALESCE(s.output_quality_score, 0.5) * CASE WHEN t.downstream_success = 1 THEN 1.0 ELSE 0.0 END)
                     - SUM(COALESCE(s.output_quality_score, 0.5)) * SUM(CASE WHEN t.downstream_success = 1 THEN 1.0 ELSE 0.0 END))
                    /
                    NULLIF(
                        SQRT(
                            (COUNT(*) * SUM(COALESCE(s.output_quality_score, 0.5) * COALESCE(s.output_quality_score, 0.5))
                             - SUM(COALESCE(s.output_quality_score, 0.5)) * SUM(COALESCE(s.output_quality_score, 0.5)))
                            *
                            (COUNT(*) * SUM(CASE WHEN t.downstream_success = 1 THEN 1.0 ELSE 0.0 END * CASE WHEN t.downstream_success = 1 THEN 1.0 ELSE 0.0 END)
                             - SUM(CASE WHEN t.downstream_success = 1 THEN 1.0 ELSE 0.0 END) * SUM(CASE WHEN t.downstream_success = 1 THEN 1.0 ELSE 0.0 END))
                        ), 0.0
                    ) as correlation
                FROM recent s
                JOIN recent t ON s.run_id = t.run_id AND s.agent_type != t.agent_type
                GROUP BY s.agent_type, t.agent_type
                HAVING COUNT(*) >= 3"#,
            )
            .map_err(|e| format!("Failed to prepare interaction matrix query: {}", e))?;

        let results = stmt
            .query_map(params![cutoff], |row| {
                Ok(AgentInteraction {
                    source_agent: row.get(0)?,
                    target_agent: row.get(1)?,
                    sample_size: row.get(2)?,
                    correlation: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                })
            })
            .map_err(|e| format!("Failed to query interaction matrix: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    })
}

/// Effect of a recommendation on a downstream agent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CascadeEffect {
    pub affected_agent: String,
    pub before_success_rate: f64,
    pub after_success_rate: f64,
    pub delta: f64,
    pub sample_size_before: i64,
    pub sample_size_after: i64,
}

/// Get cascade effects: how an applied recommendation on one agent affected others.
pub fn get_agent_cascade_effect(
    db: &CheckpointDb,
    recommendation_id: &str,
) -> Result<Vec<CascadeEffect>, String> {
    let rec_id = recommendation_id.to_string();

    db.with_conn(move |conn| {
        // Get the recommendation's target_agent and applied_at
        let (target_agent, applied_at): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT target_agent, applied_at FROM meta_optimizer_recommendations WHERE id = ?1",
                params![rec_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| format!("Recommendation not found: {}", e))?;

        let target_agent = match target_agent {
            Some(t) => t,
            None => return Ok(vec![]),
        };
        let applied_at = match applied_at {
            Some(a) => a,
            None => return Ok(vec![]),
        };

        // Parse applied_at to compute 7-day windows
        let applied = chrono::DateTime::parse_from_rfc3339(&applied_at)
            .map_err(|e| format!("Invalid applied_at date: {}", e))?;
        let before_start = (applied - chrono::Duration::days(7)).to_rfc3339();
        let after_end = (applied + chrono::Duration::days(7)).to_rfc3339();

        // For each agent that's not the target, compare success rates before/after
        let mut stmt = conn
            .prepare(
                r#"SELECT
                    agent_type,
                    SUM(CASE WHEN created_at < ?3 THEN 1 ELSE 0 END) as before_total,
                    SUM(CASE WHEN created_at < ?3 AND downstream_success = 1 THEN 1 ELSE 0 END) as before_success,
                    SUM(CASE WHEN created_at >= ?3 THEN 1 ELSE 0 END) as after_total,
                    SUM(CASE WHEN created_at >= ?3 AND downstream_success = 1 THEN 1 ELSE 0 END) as after_success
                FROM pipeline_agent_traces
                WHERE agent_type != ?1
                    AND created_at >= ?2
                    AND created_at <= ?4
                    AND run_id IN (
                        SELECT DISTINCT run_id FROM pipeline_agent_traces
                        WHERE agent_type = ?1 AND created_at >= ?2 AND created_at <= ?4
                    )
                GROUP BY agent_type
                HAVING before_total > 0 AND after_total > 0"#,
            )
            .map_err(|e| format!("Failed to prepare cascade query: {}", e))?;

        let results = stmt
            .query_map(
                params![target_agent, before_start, applied_at, after_end],
                |row| {
                    let agent: String = row.get(0)?;
                    let before_total: i64 = row.get(1)?;
                    let before_success: i64 = row.get(2)?;
                    let after_total: i64 = row.get(3)?;
                    let after_success: i64 = row.get(4)?;

                    let before_rate = if before_total > 0 {
                        before_success as f64 / before_total as f64 * 100.0
                    } else {
                        0.0
                    };
                    let after_rate = if after_total > 0 {
                        after_success as f64 / after_total as f64 * 100.0
                    } else {
                        0.0
                    };

                    Ok(CascadeEffect {
                        affected_agent: agent,
                        before_success_rate: before_rate,
                        after_success_rate: after_rate,
                        delta: after_rate - before_rate,
                        sample_size_before: before_total,
                        sample_size_after: after_total,
                    })
                },
            )
            .map_err(|e| format!("Failed to query cascade effects: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    })
}
