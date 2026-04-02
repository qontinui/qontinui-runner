//! Parallel Comparison Run Coordinator
//!
//! Launches N copies of the same workflow in isolated worktrees,
//! waits for all to complete, then triggers AI comparison analysis.

use serde::{Deserialize, Serialize};


// =============================================================================
// Types
// =============================================================================

/// Configuration for a comparison run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonConfig {
    /// Workflow ID to run.
    pub workflow_id: String,
    /// Number of parallel runs.
    pub run_count: usize,
    /// What varies between runs.
    pub variation: ComparisonVariation,
    /// Maximum time to wait for all runs (seconds).
    pub timeout_seconds: u64,
}

/// What differs between comparison runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ComparisonVariation {
    /// Identical config — tests implementation variance / non-determinism.
    Same,
    /// One run with multi_agent_mode on, one off.
    MultiAgent,
    /// Different AI models for each run.
    Model { models: Vec<String> },
    /// Different context token limits.
    ContextTokens { limits: Vec<usize> },
    /// Custom per-run overrides.
    Custom { overrides: Vec<serde_json::Value> },
}

/// Tracks the state of a comparison run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonRun {
    /// Unique comparison ID.
    pub id: String,
    /// Source workflow ID.
    pub workflow_id: String,
    /// Workflow name.
    pub workflow_name: String,
    /// Git branch all runs started from.
    pub source_branch: String,
    /// Git commit all runs started from.
    pub source_commit: String,
    /// Individual run entries.
    pub entries: Vec<ComparisonEntry>,
    /// Overall status.
    pub status: ComparisonStatus,
    /// AI comparison report (populated after all runs complete).
    pub comparison_report: Option<String>,
    /// AI recommendation.
    pub recommendation: Option<ComparisonRecommendation>,
    /// Timestamps.
    pub created_at: String,
    pub updated_at: String,
    /// Meta-optimizer recommendation ID (if created from comparison bridge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation_id: Option<String>,
    /// How this comparison was triggered: "manual", "meta_optimizer", "autoresearch".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// One run within a comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonEntry {
    /// Task run ID for this entry.
    pub task_run_id: String,
    /// Branch name in the worktree.
    pub branch_name: String,
    /// Worktree path.
    pub worktree_path: String,
    /// What config overrides were applied for this entry.
    pub config_overrides: serde_json::Value,
    /// Run status.
    pub status: ComparisonEntryStatus,
    /// Results (populated after run completes).
    pub result: Option<ComparisonEntryResult>,
}

/// Status of a comparison entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonEntryStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

/// Status of the overall comparison.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonStatus {
    /// Runs are in progress.
    Running,
    /// All runs done, AI comparison in progress.
    Comparing,
    /// Comparison complete with report.
    Completed,
    /// Something went wrong.
    Failed,
}

/// Results from a single comparison entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonEntryResult {
    pub success: bool,
    pub verification_passed: bool,
    pub iterations: u32,
    pub duration_ms: u64,
    pub files_changed: usize,
}

/// AI recommendation from comparison analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonRecommendation {
    pub branch_name: String,
    pub confidence: f64,
    pub reasoning: String,
}

// =============================================================================
// Structured comparison report (machine-readable metrics alongside prose)
// =============================================================================

/// Structured comparison output with statistical analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredComparisonReport {
    /// Per-metric deltas across all entries.
    pub metric_deltas: Vec<MetricDelta>,
    /// Cost per entry (label, cost_usd).
    pub cost_breakdown: Vec<(String, f64)>,
    /// Winning variant (if any).
    pub winner: Option<String>,
    /// Confidence in the winner (0.0–1.0).
    pub confidence: f64,
    /// P-value for success rate difference (if ≥2 entries with ≥2 runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_value: Option<f64>,
}

/// A single metric compared across variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricDelta {
    /// Metric name (e.g., "success_rate", "iterations", "duration_ms").
    pub name: String,
    /// (variant_label, value) pairs.
    pub values: Vec<(String, f64)>,
    /// Best variant for this metric.
    pub best: String,
    /// Delta between best and worst as a percentage.
    pub delta_pct: f64,
}

/// Build a structured comparison report from completed entries.
pub fn build_structured_report(entries: &[ComparisonEntry]) -> Option<StructuredComparisonReport> {
    let completed: Vec<&ComparisonEntry> = entries.iter().filter(|e| e.result.is_some()).collect();

    if completed.len() < 2 {
        return None;
    }

    let labels: Vec<String> = completed.iter().map(|e| e.branch_name.clone()).collect();
    let results: Vec<&ComparisonEntryResult> = completed
        .iter()
        .map(|e| e.result.as_ref().unwrap())
        .collect();

    // Success rate metric
    let success_values: Vec<(String, f64)> = labels
        .iter()
        .zip(results.iter())
        .map(|(l, r)| (l.clone(), if r.success { 1.0 } else { 0.0 }))
        .collect();
    let best_success = success_values
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(l, _)| l.clone())
        .unwrap_or_default();

    // Iterations metric (lower is better)
    let iter_values: Vec<(String, f64)> = labels
        .iter()
        .zip(results.iter())
        .map(|(l, r)| (l.clone(), r.iterations as f64))
        .collect();
    let best_iter = iter_values
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(l, _)| l.clone())
        .unwrap_or_default();

    // Duration metric (lower is better)
    let dur_values: Vec<(String, f64)> = labels
        .iter()
        .zip(results.iter())
        .map(|(l, r)| (l.clone(), r.duration_ms as f64))
        .collect();
    let best_dur = dur_values
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(l, _)| l.clone())
        .unwrap_or_default();

    // Files changed metric
    let files_values: Vec<(String, f64)> = labels
        .iter()
        .zip(results.iter())
        .map(|(l, r)| (l.clone(), r.files_changed as f64))
        .collect();
    let best_files = files_values
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(l, _)| l.clone())
        .unwrap_or_default();

    let metric_deltas = vec![
        build_metric_delta("success_rate", success_values, &best_success),
        build_metric_delta("iterations", iter_values, &best_iter),
        build_metric_delta("duration_ms", dur_values, &best_dur),
        build_metric_delta("files_changed", files_values, &best_files),
    ];

    // Winner: variant with most "best" wins, tie-broken by success
    let mut win_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for md in &metric_deltas {
        *win_counts.entry(&md.best).or_default() += 1;
    }
    let winner = win_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(label, _)| label.to_string());

    // Confidence: proportion of metrics won by the winner
    let winner_wins = metric_deltas
        .iter()
        .filter(|md| winner.as_deref() == Some(&md.best))
        .count();
    let confidence = winner_wins as f64 / metric_deltas.len() as f64;

    // Cost breakdown (use duration as proxy since we don't have direct cost per entry)
    let cost_breakdown: Vec<(String, f64)> = labels
        .iter()
        .zip(results.iter())
        .map(|(l, r)| (l.clone(), r.duration_ms as f64 / 1000.0)) // duration as cost proxy
        .collect();

    Some(StructuredComparisonReport {
        metric_deltas,
        cost_breakdown,
        winner,
        confidence,
        p_value: None, // Requires multiple trials per variant (future: aggregate from autoresearch)
    })
}

fn build_metric_delta(name: &str, values: Vec<(String, f64)>, best: &str) -> MetricDelta {
    let max_val = values
        .iter()
        .map(|(_, v)| *v)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_val = values.iter().map(|(_, v)| *v).fold(f64::INFINITY, f64::min);
    let delta_pct = if min_val.abs() > f64::EPSILON {
        ((max_val - min_val) / min_val.abs()) * 100.0
    } else if max_val.abs() > f64::EPSILON {
        100.0
    } else {
        0.0
    };

    MetricDelta {
        name: name.to_string(),
        values,
        best: best.to_string(),
        delta_pct,
    }
}

// =============================================================================
// Coordinator
// =============================================================================

/// Build config overrides for each run based on the variation type.
pub fn build_run_overrides(
    variation: &ComparisonVariation,
    run_count: usize,
) -> Vec<serde_json::Value> {
    match variation {
        ComparisonVariation::Same => (0..run_count).map(|_| serde_json::json!({})).collect(),
        ComparisonVariation::MultiAgent => {
            vec![
                serde_json::json!({"multi_agent_mode": true, "label": "multi-agent"}),
                serde_json::json!({"multi_agent_mode": false, "label": "monolithic"}),
            ]
        }
        ComparisonVariation::Model { models } => models
            .iter()
            .map(|m| serde_json::json!({"model": m, "label": m}))
            .collect(),
        ComparisonVariation::ContextTokens { limits } => limits
            .iter()
            .map(
                |l| serde_json::json!({"max_context_tokens": l, "label": format!("{}K", l / 1000)}),
            )
            .collect(),
        ComparisonVariation::Custom { overrides } => overrides.clone(),
    }
}

/// Check if all entries in a comparison are done (completed or failed).
pub fn all_entries_done(entries: &[ComparisonEntry]) -> bool {
    entries.iter().all(|e| {
        e.status == ComparisonEntryStatus::Completed || e.status == ComparisonEntryStatus::Failed
    })
}

/// Build a comparison summary for the AI comparison prompt.
pub fn build_entry_summaries(
    entries: &[ComparisonEntry],
) -> Vec<(String, String, String)> {
    Vec::new()
}
