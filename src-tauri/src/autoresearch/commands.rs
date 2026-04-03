//! Tauri IPC commands for the autoresearch module.

use super::engine::ResearchEngine;
use super::types::{CampaignStatus, CampaignSummary, ExperimentResult};
use crate::worktree;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;

/// Shared autoresearch engine state, managed by Tauri.
pub type SharedResearchEngine = Arc<Mutex<ResearchEngine>>;

/// Start a new autoresearch campaign.
#[tauri::command]
pub async fn start_autoresearch(
    config_json: String,
    engine: State<'_, SharedResearchEngine>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<String, String> {
    Err("SQLite removed".to_string())
}

/// Stop the currently running autoresearch campaign.
#[tauri::command]
pub async fn stop_autoresearch(engine: State<'_, SharedResearchEngine>) -> Result<(), String> {
    let mut eng = engine.lock().await;
    eng.stop()
}

/// Get the status of the current/last autoresearch campaign.
#[tauri::command]
pub async fn get_autoresearch_status(
    engine: State<'_, SharedResearchEngine>,
) -> Result<CampaignStatus, String> {
    let eng = engine.lock().await;
    Ok(eng.status().await)
}

/// Get all experiment results for the current campaign.
#[tauri::command]
pub async fn get_autoresearch_results(
    engine: State<'_, SharedResearchEngine>,
) -> Result<Vec<(u32, ExperimentResult)>, String> {
    let eng = engine.lock().await;
    Ok(eng.results().await)
}

/// Get experiment results as a TSV table string.
#[tauri::command]
pub async fn get_autoresearch_results_tsv(
    engine: State<'_, SharedResearchEngine>,
) -> Result<String, String> {
    let eng = engine.lock().await;
    Ok(eng.results_tsv().await)
}

/// Get historical campaign summaries, optionally filtered by name prefix.
#[tauri::command]
pub async fn get_autoresearch_campaign_history(
    filter: Option<String>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<Vec<CampaignSummary>, String> {
    Err("SQLite removed".to_string())
}

/// Get all experiments for a specific campaign, returning (experiment_number, ExperimentResult).
#[tauri::command]
pub async fn get_autoresearch_campaign_experiments(
    campaign_id: String,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<Vec<(u32, ExperimentResult)>, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Campaign re-run and comparison
// =============================================================================

/// Re-run a past campaign with the same config.
#[tauri::command]
pub async fn rerun_autoresearch_campaign(
    campaign_id: String,
    engine: State<'_, SharedResearchEngine>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<String, String> {
    Err("SQLite removed".to_string())
}

/// Comparison between two campaigns.
#[derive(Debug, Clone, Serialize)]
pub struct CampaignComparison {
    pub campaign_a: CampaignSummary,
    pub campaign_b: CampaignSummary,
    pub pass_rate_delta: f64,
    pub duration_delta: f64,
    pub accepted_rate_delta: f64,
}

/// Compare two autoresearch campaigns side-by-side.
#[tauri::command]
pub async fn compare_autoresearch_campaigns(
    campaign_id_a: String,
    campaign_id_b: String,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<CampaignComparison, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Workflow listing (for CampaignTab workflow selector)
// =============================================================================

/// Lightweight workflow entry for dropdowns.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowListItem {
    pub id: String,
    pub name: String,
}

/// List unified workflows (id + name only) for campaign configuration.
#[tauri::command]
pub async fn list_unified_workflows() -> Result<Vec<WorkflowListItem>, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Q-routing observability (for QRoutingTab)
// =============================================================================

/// Load Q-table rows from PG.
async fn load_q_rows_pg_first(
    app_state: &crate::commands::AppState,
) -> Vec<(String, String, f64, u32)> {
    Vec::new()
}

/// Load overrides from PG.
async fn load_overrides_pg_first(app_state: &crate::commands::AppState) -> Vec<(String, String)> {
    Vec::new()
}

/// Get the full Q-routing table as JSON. Prefers PG.
#[tauri::command]
pub async fn get_q_routing_table(
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<Vec<super::q_router::QTableRow>, String> {
    Err("SQLite removed".to_string())
}

/// Get the greedy Q-routing policy (best architecture per state). Prefers PG.
#[tauri::command]
pub async fn get_q_routing_policy(
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<Vec<super::q_router::PolicyEntry>, String> {
    Err("SQLite removed".to_string())
}

/// Get Q-routing convergence stats. Prefers PG.
#[tauri::command]
pub async fn get_q_routing_stats(
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<super::q_router::QRoutingStats, String> {
    Err("SQLite removed".to_string())
}

/// Get all Q-routing overrides. Prefers PG.
#[tauri::command]
pub async fn get_q_routing_overrides(
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<Vec<super::q_router::OverrideEntry>, String> {
    Err("SQLite removed".to_string())
}

/// Set a manual override: lock a state to a specific architecture.
/// Writes to both SQLite and PG (PG-first).
#[tauri::command]
pub async fn set_q_routing_override(
    state_key: String,
    forced_action: String,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Remove a manual override for a state.
/// Removes from both PG and SQLite.
#[tauri::command]
pub async fn remove_q_routing_override(
    state_key: String,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

// (Q-table / overrides now use PG exclusively via load_q_rows_pg_first / load_overrides_pg_first)

/// Reset the Q-routing table (clears all learned Q-values).
/// Clears both PG and SQLite. Overrides are preserved.
#[tauri::command]
pub async fn reset_q_routing_table(
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<serde_json::Value, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Worktree management (for WorktreeComparisonTab)
// =============================================================================

/// Worktree entry for the frontend list view.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeListEntry {
    pub branch_name: String,
    pub task_run_id: Option<String>,
    pub worktree_path: String,
    pub source_branch: String,
    pub repo_path: String,
    pub status: String,
    pub workflow_name: Option<String>,
    pub created_at: String,
}

/// List all managed worktrees from the database.
#[tauri::command]
pub async fn list_worktree_records() -> Result<Vec<WorktreeListEntry>, String> {
    Err("SQLite removed".to_string())
}

/// Diff result for the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeDiffResult {
    pub diff: String,
    pub files_changed: usize,
}

/// Get diff for a worktree branch, looking up repo/source from the DB.
#[tauri::command]
pub async fn get_worktree_diff(branch_name: String) -> Result<WorktreeDiffResult, String> {
    Err("SQLite removed".to_string())
}

/// Merge a worktree branch back into its source.
#[tauri::command]
pub async fn merge_worktree_branch(
    branch_name: String,
    ai_resolve: bool,
) -> Result<serde_json::Value, String> {
    Err("SQLite removed".to_string())
}

/// Remove a worktree and optionally delete its branch.
#[tauri::command]
pub async fn remove_worktree_branch(branch_name: String) -> Result<serde_json::Value, String> {
    Err("SQLite removed".to_string())
}

/// Compare all active worktree branches.
#[tauri::command]
pub async fn compare_worktree_branches() -> Result<serde_json::Value, String> {
    Err("SQLite removed".to_string())
}

/// Helper: find a worktree record by branch name.
fn find_worktree_by_branch(branch_name: &str) -> Result<worktree::WorktreeRecord, String> {
    Err("SQLite removed".to_string())
}
