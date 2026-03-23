//! Baseline learning and adaptive scoring for agentic metrics.
//!
//! Computes learned baselines from historical successful runs:
//! - **Step baselines**: P25 of iterations and step_count from successes
//! - **Tool baselines**: Union of tools_used from successful runs
//!
//! Baselines are stored in `agentic_metric_baselines` and loaded at scoring
//! time. They're bootstrapped from the first 20+ successful runs per workflow
//! and recalculated periodically.

use rusqlite::{params, Connection};
use tracing::{debug, info};

use super::step_efficiency::StepBaseline;
use super::tool_correctness::ToolBaseline;

/// Minimum successful runs required before computing a baseline.
const MIN_SAMPLES_FOR_BASELINE: i64 = 20;

/// Compute and persist step baselines for a workflow (or global if workflow_id is None).
///
/// Uses P25 (25th percentile) of iterations and step_count from successful runs.
/// P25 is conservative — it sets the baseline at the efficient end, so runs
/// taking more than this are penalized by step_efficiency scoring.
pub fn compute_step_baseline(
    conn: &Connection,
    workflow_id: Option<&str>,
) -> Result<Option<StepBaseline>, String> {
    let (iterations, steps, sample_count) = if let Some(wf_id) = workflow_id {
        query_step_data(conn, Some(wf_id))?
    } else {
        query_step_data(conn, None)?
    };

    if sample_count < MIN_SAMPLES_FOR_BASELINE {
        debug!(
            "Not enough samples for step baseline (have {}, need {})",
            sample_count, MIN_SAMPLES_FOR_BASELINE
        );
        return Ok(None);
    }

    let expected_iterations = percentile_25(&iterations);
    let expected_steps = percentile_25(&steps);

    let baseline = StepBaseline {
        expected_iterations,
        expected_steps,
    };

    // Persist to DB
    let baseline_json = serde_json::json!({
        "expected_iterations": expected_iterations,
        "expected_steps": expected_steps,
    });
    persist_baseline(
        conn,
        workflow_id,
        "min_steps",
        &baseline_json.to_string(),
        sample_count,
    )?;

    info!(
        "Computed step baseline (workflow={:?}): iterations={:.1}, steps={:.1} (n={})",
        workflow_id, expected_iterations, expected_steps, sample_count
    );

    Ok(Some(baseline))
}

/// Compute and persist tool baselines for a workflow (or global if workflow_id is None).
///
/// Takes the union of all tools_used across successful runs.
pub fn compute_tool_baseline(
    conn: &Connection,
    workflow_id: Option<&str>,
) -> Result<Option<ToolBaseline>, String> {
    let (tools, sample_count) = query_tool_data(conn, workflow_id)?;

    if sample_count < MIN_SAMPLES_FOR_BASELINE {
        debug!(
            "Not enough samples for tool baseline (have {}, need {})",
            sample_count, MIN_SAMPLES_FOR_BASELINE
        );
        return Ok(None);
    }

    let baseline = ToolBaseline {
        expected_tools: tools.clone(),
    };

    // Persist to DB
    let baseline_json = serde_json::json!({ "tools": tools });
    persist_baseline(
        conn,
        workflow_id,
        "expected_tools",
        &baseline_json.to_string(),
        sample_count,
    )?;

    info!(
        "Computed tool baseline (workflow={:?}): {} tools (n={})",
        workflow_id,
        tools.len(),
        sample_count
    );

    Ok(Some(baseline))
}

/// Load a previously persisted step baseline from the DB.
pub fn load_step_baseline(
    conn: &Connection,
    workflow_id: Option<&str>,
) -> Result<Option<StepBaseline>, String> {
    let json = load_baseline_value(conn, workflow_id, "min_steps")?;
    match json {
        Some(val) => {
            let parsed: serde_json::Value = serde_json::from_str(&val)
                .map_err(|e| format!("Failed to parse step baseline: {}", e))?;
            Ok(Some(StepBaseline {
                expected_iterations: parsed["expected_iterations"].as_f64().unwrap_or(2.0),
                expected_steps: parsed["expected_steps"].as_f64().unwrap_or(8.0),
            }))
        }
        None => Ok(None),
    }
}

/// Load a previously persisted tool baseline from the DB.
pub fn load_tool_baseline(
    conn: &Connection,
    workflow_id: Option<&str>,
) -> Result<Option<ToolBaseline>, String> {
    let json = load_baseline_value(conn, workflow_id, "expected_tools")?;
    match json {
        Some(val) => {
            let parsed: serde_json::Value = serde_json::from_str(&val)
                .map_err(|e| format!("Failed to parse tool baseline: {}", e))?;
            let tools = parsed["tools"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Ok(Some(ToolBaseline {
                expected_tools: tools,
            }))
        }
        None => Ok(None),
    }
}

/// Recompute all baselines (global + per-workflow) and persist them.
///
/// Call this periodically (e.g., after every N runs or on a timer).
pub fn recompute_all_baselines(conn: &Connection) -> Result<u32, String> {
    let mut updated = 0u32;

    // Global baselines
    if compute_step_baseline(conn, None)?.is_some() {
        updated += 1;
    }
    if compute_tool_baseline(conn, None)?.is_some() {
        updated += 1;
    }

    // Per-workflow baselines: find workflows with enough data
    let workflow_ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                r#"SELECT DISTINCT w.id
                   FROM unified_workflows w
                   JOIN task_runs tr ON tr.workflow_id = w.id
                   JOIN learning_outcomes lo ON lo.task_id = tr.id
                   WHERE lo.status = 'success'
                     AND (lo.iterations IS NULL OR lo.iterations > 0)
                   GROUP BY w.id
                   HAVING COUNT(*) >= ?1"#,
            )
            .map_err(|e| format!("Failed to query workflows for baselines: {}", e))?;
        let rows = stmt
            .query_map(params![MIN_SAMPLES_FOR_BASELINE], |row| row.get(0))
            .map_err(|e| format!("Failed to read workflow IDs: {}", e))?;
        let ids: Vec<String> = rows.filter_map(|r| r.ok()).collect();
        ids
    };

    for wf_id in &workflow_ids {
        if compute_step_baseline(conn, Some(wf_id))?.is_some() {
            updated += 1;
        }
        if compute_tool_baseline(conn, Some(wf_id))?.is_some() {
            updated += 1;
        }
    }

    info!(
        "Recomputed {} baselines ({} workflows + global)",
        updated,
        workflow_ids.len()
    );
    Ok(updated)
}

// ── Private helpers ─────────────────────────────────────────────────────

/// Query iteration and step count data from successful runs.
fn query_step_data(
    conn: &Connection,
    workflow_id: Option<&str>,
) -> Result<(Vec<f64>, Vec<f64>, i64), String> {
    let (query, wf_param);
    if let Some(wf_id) = workflow_id {
        wf_param = wf_id.to_string();
        query = r#"
            SELECT lo.iterations, lo.step_count
            FROM learning_outcomes lo
            JOIN task_runs tr ON tr.id = lo.task_id
            WHERE lo.status = 'success'
              AND (lo.iterations IS NULL OR lo.iterations > 0)
              AND tr.workflow_id = ?1
            ORDER BY lo.created_at DESC
            LIMIT 200
        "#;
    } else {
        wf_param = String::new();
        query = r#"
            SELECT lo.iterations, lo.step_count
            FROM learning_outcomes lo
            WHERE lo.status = 'success'
              AND (lo.iterations IS NULL OR lo.iterations > 0)
            ORDER BY lo.created_at DESC
            LIMIT 200
        "#;
    }

    let mut stmt = conn
        .prepare(query)
        .map_err(|e| format!("Failed to prepare step query: {}", e))?;

    let params_vec: Vec<&dyn rusqlite::types::ToSql> = if workflow_id.is_some() {
        vec![&wf_param as &dyn rusqlite::types::ToSql]
    } else {
        vec![]
    };

    let rows: Vec<(f64, f64)> = stmt
        .query_map(params_vec.as_slice(), |row| {
            let iterations: Option<i64> = row.get(0)?;
            let steps: Option<i64> = row.get(1)?;
            Ok((iterations.unwrap_or(1) as f64, steps.unwrap_or(5) as f64))
        })
        .map_err(|e| format!("Failed to query step data: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    let count = rows.len() as i64;
    let iterations: Vec<f64> = rows.iter().map(|(i, _)| *i).collect();
    let steps: Vec<f64> = rows.iter().map(|(_, s)| *s).collect();

    Ok((iterations, steps, count))
}

/// Query tools_used from successful runs.
fn query_tool_data(
    conn: &Connection,
    workflow_id: Option<&str>,
) -> Result<(Vec<String>, i64), String> {
    let (query, wf_param);
    if let Some(wf_id) = workflow_id {
        wf_param = wf_id.to_string();
        query = r#"
            SELECT lo.tools_used
            FROM learning_outcomes lo
            JOIN task_runs tr ON tr.id = lo.task_id
            WHERE lo.status = 'success'
              AND (lo.iterations IS NULL OR lo.iterations > 0)
              AND lo.tools_used IS NOT NULL AND lo.tools_used != '[]'
              AND tr.workflow_id = ?1
            ORDER BY lo.created_at DESC
            LIMIT 200
        "#;
    } else {
        wf_param = String::new();
        query = r#"
            SELECT lo.tools_used
            FROM learning_outcomes lo
            WHERE lo.status = 'success'
              AND (lo.iterations IS NULL OR lo.iterations > 0)
              AND lo.tools_used IS NOT NULL AND lo.tools_used != '[]'
            ORDER BY lo.created_at DESC
            LIMIT 200
        "#;
    }

    let mut stmt = conn
        .prepare(query)
        .map_err(|e| format!("Failed to prepare tool query: {}", e))?;

    let params_vec: Vec<&dyn rusqlite::types::ToSql> = if workflow_id.is_some() {
        vec![&wf_param as &dyn rusqlite::types::ToSql]
    } else {
        vec![]
    };

    let mut all_tools = std::collections::BTreeSet::new();
    let mut sample_count = 0i64;

    let rows = stmt
        .query_map(params_vec.as_slice(), |row| row.get::<_, String>(0))
        .map_err(|e| format!("Failed to query tool data: {}", e))?;

    for tools_json in rows.flatten() {
        if let Ok(tools) = serde_json::from_str::<Vec<String>>(&tools_json) {
            for tool in tools {
                all_tools.insert(tool);
            }
            sample_count += 1;
        }
    }

    Ok((all_tools.into_iter().collect(), sample_count))
}

/// Sentinel value for global baselines (not tied to a specific workflow).
/// SQLite treats NULL as distinct in UNIQUE indexes, so we use a sentinel string
/// to ensure the ON CONFLICT upsert works correctly for global baselines.
const GLOBAL_BASELINE_SENTINEL: &str = "__global__";

/// Persist a baseline value to the agentic_metric_baselines table.
fn persist_baseline(
    conn: &Connection,
    workflow_id: Option<&str>,
    metric_type: &str,
    baseline_value: &str,
    sample_count: i64,
) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    let wf_id = workflow_id.unwrap_or(GLOBAL_BASELINE_SENTINEL);

    // Use UPSERT — sentinel value ensures ON CONFLICT fires for global baselines
    conn.execute(
        r#"INSERT INTO agentic_metric_baselines (id, workflow_id, metric_type, baseline_value, sample_count, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6)
           ON CONFLICT(workflow_id, metric_type) DO UPDATE SET
               baseline_value = excluded.baseline_value,
               sample_count = excluded.sample_count,
               updated_at = excluded.updated_at"#,
        params![
            format!("amb-{}", uuid::Uuid::new_v4()),
            wf_id,
            metric_type,
            baseline_value,
            sample_count,
            now,
        ],
    )
    .map_err(|e| format!("Failed to persist baseline: {}", e))?;

    Ok(())
}

/// Load a baseline value from the DB.
fn load_baseline_value(
    conn: &Connection,
    workflow_id: Option<&str>,
    metric_type: &str,
) -> Result<Option<String>, String> {
    // Try workflow-specific first, fall back to global (using sentinel for NULL)
    let result: Option<String> = if let Some(wf_id) = workflow_id {
        conn.query_row(
            "SELECT baseline_value FROM agentic_metric_baselines WHERE workflow_id = ?1 AND metric_type = ?2",
            params![wf_id, metric_type],
            |row| row.get(0),
        )
        .ok()
        .or_else(|| {
            // Fall back to global baseline
            conn.query_row(
                "SELECT baseline_value FROM agentic_metric_baselines WHERE workflow_id = ?1 AND metric_type = ?2",
                params![GLOBAL_BASELINE_SENTINEL, metric_type],
                |row| row.get(0),
            )
            .ok()
        })
    } else {
        conn.query_row(
            "SELECT baseline_value FROM agentic_metric_baselines WHERE workflow_id = ?1 AND metric_type = ?2",
            params![GLOBAL_BASELINE_SENTINEL, metric_type],
            |row| row.get(0),
        )
        .ok()
    };

    Ok(result)
}

/// Compute the 25th percentile of a sorted list.
fn percentile_25(data: &[f64]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut sorted = data.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let idx = (sorted.len() as f64 * 0.25).floor() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percentile_25_empty() {
        assert_eq!(percentile_25(&[]), 0.0);
    }

    #[test]
    fn test_percentile_25_single() {
        assert_eq!(percentile_25(&[5.0]), 5.0);
    }

    #[test]
    fn test_percentile_25_four_elements() {
        // Sorted: [1, 2, 3, 4]. P25 index = floor(4 * 0.25) = 1 → value = 2.0
        assert_eq!(percentile_25(&[3.0, 1.0, 4.0, 2.0]), 2.0);
    }

    #[test]
    fn test_percentile_25_twenty_elements() {
        // 1..=20. P25 index = floor(20 * 0.25) = 5 → value = 6.0
        let data: Vec<f64> = (1..=20).map(|i| i as f64).collect();
        assert_eq!(percentile_25(&data), 6.0);
    }
}
