//! Tauri IPC commands for the autoresearch module.

use super::engine::ResearchEngine;
use super::types::{CampaignStatus, CampaignSummary, ExperimentResult};
use crate::database::CheckpointDb;
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
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<String, String> {
    let config: super::types::ResearchConfig = serde_json::from_str(&config_json)
        .map_err(|e| format!("Invalid research config: {}", e))?;

    let pg_db = app_state.pg_db.clone();
    let mut eng = engine.lock().await;
    eng.start(config, db.inner().clone(), pg_db).await
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
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<Vec<CampaignSummary>, String> {
    let rows = app_state
        .pg_db
        .get_autoresearch_campaigns(filter.as_deref())
        .await?;

    Ok(rows
        .into_iter()
        .map(|(id, name, status, experiment_count, accepted_count, config_json, created_at)| {
            CampaignSummary {
                id,
                name,
                status,
                experiment_count,
                accepted_count,
                config_json,
                created_at,
            }
        })
        .collect())
}

/// Get all experiments for a specific campaign, returning (experiment_number, ExperimentResult).
#[tauri::command]
pub async fn get_autoresearch_campaign_experiments(
    campaign_id: String,
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<Vec<(u32, ExperimentResult)>, String> {
    let rows = app_state
        .pg_db
        .get_autoresearch_experiments(&campaign_id)
        .await?;

    let mut results = Vec::new();
    for (num, config_json, trials_json, aggregate_json, accepted, reason, p_value) in rows {
        let config: super::types::ExperimentConfig =
            serde_json::from_str(&config_json)
                .map_err(|e| format!("Failed to parse config JSON: {}", e))?;
        let trials: Vec<super::types::TrialResult> =
            serde_json::from_str(&trials_json)
                .map_err(|e| format!("Failed to parse trials JSON: {}", e))?;
        let aggregate: super::types::AggregateMetrics =
            serde_json::from_str(&aggregate_json)
                .map_err(|e| format!("Failed to parse aggregate JSON: {}", e))?;

        results.push((
            num as u32,
            ExperimentResult {
                config,
                trials,
                aggregate,
                accepted,
                reason: reason.unwrap_or_default(),
                p_value,
                ai_recommendation: None,
            },
        ));
    }
    Ok(results)
}

// =============================================================================
// Campaign re-run and comparison
// =============================================================================

/// Re-run a past campaign with the same config.
#[tauri::command]
pub async fn rerun_autoresearch_campaign(
    campaign_id: String,
    engine: State<'_, SharedResearchEngine>,
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<String, String> {
    // Load the original campaign's config from PG
    let config_json: String = app_state.pg_db.get_campaign_config(&campaign_id).await?;

    let mut config: super::types::ResearchConfig = serde_json::from_str(&config_json)
        .map_err(|e| format!("Invalid campaign config: {}", e))?;

    config.name = format!("{} (rerun)", config.name);

    let pg_db = app_state.pg_db.clone();
    let mut eng = engine.lock().await;
    eng.start(config, db.inner().clone(), pg_db).await
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
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<CampaignComparison, String> {
    let pg_db = &app_state.pg_db;

    // Helper: load campaign summary + experiments from PG
    async fn load_campaign_pg(
        pg_db: &crate::database::pg::PgDb,
        id: &str,
    ) -> Result<(CampaignSummary, Vec<(u32, super::types::ExperimentResult)>), String> {
        let (sid, name, status, experiment_count, accepted_count, config_json, created_at) =
            pg_db.get_campaign_summary_by_id(id).await?;
        let summary = CampaignSummary {
            id: sid,
            name,
            status,
            experiment_count,
            accepted_count,
            config_json,
            created_at,
        };

        let exp_rows = pg_db.get_experiments_for_comparison(id).await?;
        let mut experiments = Vec::new();
        for (num, agg_json, accepted) in exp_rows {
            let aggregate: super::types::AggregateMetrics = serde_json::from_str(&agg_json)
                .unwrap_or(super::types::AggregateMetrics {
                    pass_rate: 0.0,
                    mean_iterations: 0.0,
                    mean_duration_ms: 0.0,
                    trial_count: 0,
                    mean_spec_compliance: None,
                    mean_composite_agentic_score: None,
                    mean_agentic_scores: None,
                });
            let config: super::types::ExperimentConfig =
                serde_json::from_str("{}").unwrap_or_else(|_| serde_json::from_value(serde_json::json!({})).unwrap());
            experiments.push((num as u32, super::types::ExperimentResult {
                config,
                trials: vec![],
                aggregate,
                accepted,
                reason: String::new(),
                p_value: None,
                ai_recommendation: None,
            }));
        }
        Ok((summary, experiments))
    }

    let (summary_a, exps_a) = load_campaign_pg(pg_db, &campaign_id_a).await?;
    let (summary_b, exps_b) = load_campaign_pg(pg_db, &campaign_id_b).await?;

    // Compute weighted average pass rate
    let compute_pass_rate = |exps: &[(u32, super::types::ExperimentResult)]| -> f64 {
        let total_trials: u32 = exps.iter().map(|(_, e)| e.aggregate.trial_count).sum();
        if total_trials == 0 {
            return 0.0;
        }
        exps.iter()
            .map(|(_, e)| e.aggregate.pass_rate * e.aggregate.trial_count as f64)
            .sum::<f64>()
            / total_trials as f64
    };

    let compute_duration = |exps: &[(u32, super::types::ExperimentResult)]| -> f64 {
        if exps.is_empty() {
            return 0.0;
        }
        exps.iter()
            .map(|(_, e)| e.aggregate.mean_duration_ms)
            .sum::<f64>()
            / exps.len() as f64
    };

    let compute_accepted_rate = |summary: &CampaignSummary| -> f64 {
        if summary.experiment_count == 0 {
            return 0.0;
        }
        summary.accepted_count as f64 / summary.experiment_count as f64
    };

    let pass_rate_a = compute_pass_rate(&exps_a);
    let pass_rate_b = compute_pass_rate(&exps_b);
    let duration_a = compute_duration(&exps_a);
    let duration_b = compute_duration(&exps_b);

    Ok(CampaignComparison {
        pass_rate_delta: pass_rate_b - pass_rate_a,
        duration_delta: duration_b - duration_a,
        accepted_rate_delta: compute_accepted_rate(&summary_b) - compute_accepted_rate(&summary_a),
        campaign_a: summary_a,
        campaign_b: summary_b,
    })
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
pub async fn list_unified_workflows(
    db: State<'_, Arc<CheckpointDb>>,
) -> Result<Vec<WorkflowListItem>, String> {
    let workflows = db.list_unified_workflows()?;
    Ok(workflows
        .into_iter()
        .map(|w| WorkflowListItem {
            id: w.id,
            name: w.name,
        })
        .collect())
}

// =============================================================================
// Q-routing observability (for QRoutingTab)
// =============================================================================

/// Load Q-table rows from PG.
async fn load_q_rows_pg_first(
    _db: &CheckpointDb,
    app_state: &crate::commands::AppState,
) -> Vec<(String, String, f64, u32)> {
    app_state.pg_db.get_all_q_entries().await.unwrap_or_default()
}

/// Load overrides from PG.
async fn load_overrides_pg_first(
    _db: &CheckpointDb,
    app_state: &crate::commands::AppState,
) -> Vec<(String, String)> {
    app_state.pg_db.get_all_q_overrides().await.unwrap_or_default()
}

/// Get the full Q-routing table as JSON. Prefers PG.
#[tauri::command]
pub async fn get_q_routing_table(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<Vec<super::q_router::QTableRow>, String> {
    let mut q_router = super::q_router::QRouter::new();
    let rows = load_q_rows_pg_first(&db, &app_state).await;
    q_router.load_from_rows(rows);
    Ok(q_router.table_snapshot())
}

/// Get the greedy Q-routing policy (best architecture per state). Prefers PG.
#[tauri::command]
pub async fn get_q_routing_policy(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<Vec<super::q_router::PolicyEntry>, String> {
    let mut q_router = super::q_router::QRouter::new();
    let rows = load_q_rows_pg_first(&db, &app_state).await;
    q_router.load_from_rows(rows);
    Ok(q_router.policy_snapshot())
}

/// Get Q-routing convergence stats. Prefers PG.
#[tauri::command]
pub async fn get_q_routing_stats(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<super::q_router::QRoutingStats, String> {
    let mut q_router = super::q_router::QRouter::new();
    let rows = load_q_rows_pg_first(&db, &app_state).await;
    q_router.load_from_rows(rows);
    Ok(q_router.stats())
}

/// Get all Q-routing overrides. Prefers PG.
#[tauri::command]
pub async fn get_q_routing_overrides(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<Vec<super::q_router::OverrideEntry>, String> {
    let mut q_router = super::q_router::QRouter::new();
    let overrides = load_overrides_pg_first(&db, &app_state).await;
    q_router.load_overrides(overrides);
    Ok(q_router.overrides_snapshot())
}

/// Set a manual override: lock a state to a specific architecture.
/// Writes to both SQLite and PG (PG-first).
#[tauri::command]
pub async fn set_q_routing_override(
    state_key: String,
    forced_action: String,
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<(), String> {
    // Validate state_key parses
    super::q_router::TaskState::from_key(&state_key)
        .ok_or_else(|| format!("Invalid state_key: {}", state_key))?;

    // Validate architecture
    if !["traditional", "agentic_verification", "multi_agent_pipeline"].contains(&forced_action.as_str()) {
        return Err(format!("Invalid architecture: {}", forced_action));
    }

    // PG-first
    app_state.pg_db.upsert_q_override(&state_key, &forced_action).await?;

    // PG is the primary store (upsert_q_override above); no SQLite fallback needed
    Ok(())
}

/// Remove a manual override for a state.
/// Removes from both PG and SQLite.
#[tauri::command]
pub async fn remove_q_routing_override(
    state_key: String,
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<bool, String> {
    // PG-first
    if let Err(e) = app_state.pg_db.delete_q_override(&state_key).await {
        tracing::warn!("Failed to delete PG override for {}: {}", state_key, e);
    }

    // PG is the primary store (delete_q_override above); no SQLite fallback needed
    Ok(true)
}

// (Q-table / overrides now use PG exclusively via load_q_rows_pg_first / load_overrides_pg_first)

/// Reset the Q-routing table (clears all learned Q-values).
/// Clears both PG and SQLite. Overrides are preserved.
#[tauri::command]
pub async fn reset_q_routing_table(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<crate::commands::AppState>>,
) -> Result<serde_json::Value, String> {
    let mut deleted_pg: u64 = 0;

    // PG-first
    deleted_pg = app_state.pg_db.clear_q_table().await.unwrap_or(0);

    // PG is the primary store; no SQLite Q-table cleanup needed
    let deleted_sqlite: u64 = 0;

    tracing::info!(
        "Q-routing table reset: {} PG rows, {} SQLite rows deleted",
        deleted_pg, deleted_sqlite
    );

    Ok(serde_json::json!({
        "deleted_pg": deleted_pg,
        "deleted_sqlite": deleted_sqlite,
    }))
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
pub async fn list_worktree_records(
    db: State<'_, Arc<CheckpointDb>>,
) -> Result<Vec<WorktreeListEntry>, String> {
    let records = db.list_worktrees(None)?;
    Ok(records
        .into_iter()
        .map(|r| WorktreeListEntry {
            branch_name: r.branch_name,
            task_run_id: r.task_run_id,
            worktree_path: r.worktree_path,
            source_branch: r.source_branch,
            repo_path: r.repo_path,
            status: r.status.to_string(),
            workflow_name: r.workflow_name,
            created_at: r.created_at,
        })
        .collect())
}

/// Diff result for the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeDiffResult {
    pub diff: String,
    pub files_changed: usize,
}

/// Get diff for a worktree branch, looking up repo/source from the DB.
#[tauri::command]
pub async fn get_worktree_diff(
    branch_name: String,
    db: State<'_, Arc<CheckpointDb>>,
) -> Result<WorktreeDiffResult, String> {
    let record = find_worktree_by_branch(&db, &branch_name)?;
    let repo_path = Path::new(&record.repo_path);

    let summary =
        worktree::get_worktree_diff_summary(repo_path, &record.branch_name, &record.source_branch)?;

    let full_diff = worktree::get_full_diff(repo_path, &record.branch_name, &record.source_branch)
        .unwrap_or_default();

    let files_changed = full_diff
        .lines()
        .filter(|l| l.starts_with("diff --git"))
        .count();

    let diff_text = if full_diff.is_empty() {
        summary
    } else {
        full_diff
    };

    Ok(WorktreeDiffResult {
        diff: diff_text,
        files_changed,
    })
}

/// Merge a worktree branch back into its source.
#[tauri::command]
pub async fn merge_worktree_branch(
    branch_name: String,
    ai_resolve: bool,
    db: State<'_, Arc<CheckpointDb>>,
) -> Result<serde_json::Value, String> {
    let record = find_worktree_by_branch(&db, &branch_name)?;
    let repo_path = Path::new(&record.repo_path);

    let result = worktree::merge_worktree(repo_path, &record.branch_name, &record.source_branch)?;

    if result.success {
        let _ = db.update_worktree_status(&record.id, &worktree::WorktreeStatus::Merged);
        return Ok(serde_json::json!({
            "success": true,
            "merge_commit": result.merge_commit,
            "summary": result.summary,
        }));
    }

    if !ai_resolve {
        return Ok(serde_json::json!({
            "success": false,
            "conflicts": result.conflicts,
            "summary": result.summary,
        }));
    }

    // AI-assisted merge
    let merge_state = worktree::start_merge_with_conflicts(repo_path, &record.branch_name)?;

    if merge_state.success {
        let _ = db.update_worktree_status(&record.id, &worktree::WorktreeStatus::Merged);
        return Ok(serde_json::json!({
            "success": true,
            "merge_commit": merge_state.merge_commit,
            "summary": "Clean merge completed",
        }));
    }

    Ok(serde_json::json!({
        "success": false,
        "needs_ai_resolution": true,
        "conflicts": merge_state.conflicts,
        "summary": format!("{} conflict(s) need resolution", merge_state.conflicts.len()),
    }))
}

/// Remove a worktree and optionally delete its branch.
#[tauri::command]
pub async fn remove_worktree_branch(
    branch_name: String,
    db: State<'_, Arc<CheckpointDb>>,
) -> Result<serde_json::Value, String> {
    let record = find_worktree_by_branch(&db, &branch_name)?;

    worktree::remove_worktree(
        Path::new(&record.repo_path),
        Path::new(&record.worktree_path),
        &record.branch_name,
        true, // delete branch
    )?;

    let _ = db.update_worktree_status(&record.id, &worktree::WorktreeStatus::Removed);

    Ok(serde_json::json!({
        "success": true,
        "removed": record.branch_name,
    }))
}

/// Compare all active worktree branches.
#[tauri::command]
pub async fn compare_worktree_branches(
    db: State<'_, Arc<CheckpointDb>>,
) -> Result<serde_json::Value, String> {
    let records = db.list_worktrees(Some("active"))?;
    if records.len() < 2 {
        return Err("Need at least 2 active worktrees to compare".to_string());
    }

    let source_branch = &records[0].source_branch;

    let mut branch_data: Vec<(String, String, String)> = Vec::new();
    for r in &records {
        let diff_summary = worktree::get_worktree_diff_summary(
            Path::new(&r.repo_path),
            &r.branch_name,
            &r.source_branch,
        )
        .unwrap_or_else(|e| format!("Failed to get diff: {}", e));

        let result_summary = r.workflow_name.clone().unwrap_or_default();
        branch_data.push((r.branch_name.clone(), diff_summary, result_summary));
    }

    let workflow_name = records[0].workflow_name.as_deref().unwrap_or("Comparison");
    let report = worktree::build_comparison_prompt(&branch_data, workflow_name, source_branch);

    Ok(serde_json::json!({
        "report": report,
        "branch_count": records.len(),
        "branches": records.iter().map(|r| &r.branch_name).collect::<Vec<_>>(),
    }))
}

/// Helper: find a worktree record by branch name.
fn find_worktree_by_branch(
    db: &CheckpointDb,
    branch_name: &str,
) -> Result<worktree::WorktreeRecord, String> {
    let records = db.list_worktrees(None)?;
    records
        .into_iter()
        .find(|r| r.branch_name == branch_name)
        .ok_or_else(|| format!("No worktree found for branch: {}", branch_name))
}
