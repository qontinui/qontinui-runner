//! Parallel Comparison Run Coordinator
//!
//! Launches N copies of the same workflow in isolated worktrees,
//! waits for all to complete, then triggers AI comparison analysis.

use serde::{Deserialize, Serialize};

use crate::database::CheckpointDb;

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
    db: &CheckpointDb,
) -> Vec<(String, String, String)> {
    entries
        .iter()
        .map(|entry| {
            let result_summary = entry
                .result
                .as_ref()
                .map(|r| {
                    format!(
                "Success: {}, Verification: {}, Iterations: {}, Duration: {}ms, Files changed: {}",
                r.success, r.verification_passed, r.iterations, r.duration_ms, r.files_changed
            )
                })
                .unwrap_or_else(|| {
                    // Try to get from database
                    db.get_task_run(&entry.task_run_id)
                        .ok()
                        .flatten()
                        .map(|run| {
                            format!(
                                "Status: {}, Summary: {}",
                                run.status,
                                run.summary.as_deref().unwrap_or("none")
                            )
                        })
                        .unwrap_or_else(|| format!("Status: {:?}", entry.status))
                });

            (entry.branch_name.clone(), String::new(), result_summary)
        })
        .collect()
}
