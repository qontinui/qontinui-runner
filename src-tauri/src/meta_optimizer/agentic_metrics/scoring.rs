//! Baseline learning and adaptive scoring for agentic metrics.
//!
//! Computes learned baselines from historical successful runs:
//! - **Step baselines**: P25 of iterations and step_count from successes
//! - **Tool baselines**: Union of tools_used from successful runs
//!
//! Baselines are stored in `agentic_metric_baselines` and loaded at scoring
//! time. They're bootstrapped from the first 20+ successful runs per workflow
//! and recalculated periodically.

use crate::database::Connection;
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
    Err("SQLite removed".to_string())
}

/// Compute and persist tool baselines for a workflow (or global if workflow_id is None).
///
/// Takes the union of all tools_used across successful runs.
pub fn compute_tool_baseline(
    conn: &Connection,
    workflow_id: Option<&str>,
) -> Result<Option<ToolBaseline>, String> {
    Err("SQLite removed".to_string())
}

/// Load a previously persisted step baseline from the DB.
pub fn load_step_baseline(
    conn: &Connection,
    workflow_id: Option<&str>,
) -> Result<Option<StepBaseline>, String> {
    Err("SQLite removed".to_string())
}

/// Load a previously persisted tool baseline from the DB.
pub fn load_tool_baseline(
    conn: &Connection,
    workflow_id: Option<&str>,
) -> Result<Option<ToolBaseline>, String> {
    Err("SQLite removed".to_string())
}

/// Recompute all baselines (global + per-workflow) and persist them.
///
/// Call this periodically (e.g., after every N runs or on a timer).
pub fn recompute_all_baselines(conn: &Connection) -> Result<u32, String> {
    Err("SQLite removed".to_string())
}

// ── Private helpers ─────────────────────────────────────────────────────

/// Query iteration and step count data from successful runs.
fn query_step_data(
    conn: &Connection,
    workflow_id: Option<&str>,
) -> Result<(Vec<f64>, Vec<f64>, i64), String> {
    Err("SQLite removed".to_string())
}

/// Query tools_used from successful runs.
fn query_tool_data(
    conn: &Connection,
    workflow_id: Option<&str>,
) -> Result<(Vec<String>, i64), String> {
    Err("SQLite removed".to_string())
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
    Err("SQLite removed".to_string())
}

/// Load a baseline value from the DB.
fn load_baseline_value(
    conn: &Connection,
    workflow_id: Option<&str>,
    metric_type: &str,
) -> Result<Option<String>, String> {
    Err("SQLite removed".to_string())
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
use crate::database::Connection;

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
