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
) -> Result<String, String> {
    let config: super::types::ResearchConfig = serde_json::from_str(&config_json)
        .map_err(|e| format!("Invalid research config: {}", e))?;

    let mut eng = engine.lock().await;
    eng.start(config, db.inner().clone()).await
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
) -> Result<Vec<CampaignSummary>, String> {
    db.with_conn(|conn| {
        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match &filter {
            Some(f) => (
                "SELECT id, name, status, experiment_count, accepted_count, config_json, created_at \
                 FROM autoresearch_campaigns WHERE name LIKE ?1 ORDER BY created_at DESC"
                    .to_string(),
                vec![Box::new(format!("%{}%", f)) as Box<dyn rusqlite::types::ToSql>],
            ),
            None => (
                "SELECT id, name, status, experiment_count, accepted_count, config_json, created_at \
                 FROM autoresearch_campaigns ORDER BY created_at DESC"
                    .to_string(),
                vec![],
            ),
        };

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(CampaignSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    status: row.get(2)?,
                    experiment_count: row.get(3)?,
                    accepted_count: row.get(4)?,
                    config_json: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query campaigns: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(results)
    })
}

/// Get all experiments for a specific campaign, returning (experiment_number, ExperimentResult).
#[tauri::command]
pub async fn get_autoresearch_campaign_experiments(
    campaign_id: String,
    db: State<'_, Arc<CheckpointDb>>,
) -> Result<Vec<(u32, ExperimentResult)>, String> {
    db.with_conn(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT experiment_number, config_json, trials_json, aggregate_json, accepted, reason, p_value \
                 FROM autoresearch_experiments WHERE campaign_id = ?1 ORDER BY experiment_number ASC",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map([&campaign_id], |row| {
                let experiment_number: u32 = row.get(0)?;
                let config_json: String = row.get(1)?;
                let trials_json: String = row.get(2)?;
                let aggregate_json: String = row.get(3)?;
                let accepted: bool = row.get(4)?;
                let reason: String = row.get::<_, Option<String>>(5)?.unwrap_or_default();
                let p_value: Option<f64> = row.get(6)?;
                Ok((
                    experiment_number,
                    config_json,
                    trials_json,
                    aggregate_json,
                    accepted,
                    reason,
                    p_value,
                ))
            })
            .map_err(|e| format!("Failed to query experiments: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            let (num, config_json, trials_json, aggregate_json, accepted, reason, p_value) =
                row.map_err(|e| format!("Failed to read row: {}", e))?;

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
                num,
                ExperimentResult {
                    config,
                    trials,
                    aggregate,
                    accepted,
                    reason,
                    p_value,
                    ai_recommendation: None,
                },
            ));
        }
        Ok(results)
    })
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
) -> Result<String, String> {
    // Load the original campaign's config
    let config_json: String = db.with_conn({
        let campaign_id = campaign_id.clone();
        move |conn| {
            conn.query_row(
                "SELECT config_json FROM autoresearch_campaigns WHERE id = ?1",
                rusqlite::params![campaign_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Campaign not found: {}", e))
        }
    })?;

    let mut config: super::types::ResearchConfig = serde_json::from_str(&config_json)
        .map_err(|e| format!("Invalid campaign config: {}", e))?;

    config.name = format!("{} (rerun)", config.name);

    let mut eng = engine.lock().await;
    eng.start(config, db.inner().clone()).await
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
) -> Result<CampaignComparison, String> {
    let load_campaign = |id: String| -> Result<
        (CampaignSummary, Vec<(u32, super::types::ExperimentResult)>),
        String,
    > {
        let summary = db.with_conn({
            let id = id.clone();
            move |conn| {
                conn.query_row(
                    "SELECT id, name, status, experiment_count, accepted_count, config_json, created_at \
                     FROM autoresearch_campaigns WHERE id = ?1",
                    rusqlite::params![id],
                    |row| {
                        Ok(CampaignSummary {
                            id: row.get(0)?,
                            name: row.get(1)?,
                            status: row.get(2)?,
                            experiment_count: row.get(3)?,
                            accepted_count: row.get(4)?,
                            config_json: row.get(5)?,
                            created_at: row.get(6)?,
                        })
                    },
                )
                .map_err(|e| format!("Campaign not found: {}", e))
            }
        })?;

        // Load experiments for pass rate calculation
        let experiments = db.with_conn({
            let id = id.clone();
            move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT experiment_number, aggregate_json, accepted FROM autoresearch_experiments \
                         WHERE campaign_id = ?1 ORDER BY experiment_number ASC",
                    )
                    .map_err(|e| format!("Failed to prepare query: {}", e))?;

                let rows = stmt
                    .query_map(rusqlite::params![id], |row| {
                        let num: u32 = row.get(0)?;
                        let agg_json: String = row.get(1)?;
                        let accepted: bool = row.get(2)?;
                        Ok((num, agg_json, accepted))
                    })
                    .map_err(|e| format!("Failed to query experiments: {}", e))?;

                let mut results = Vec::new();
                for row in rows {
                    let (num, agg_json, accepted) = row.map_err(|e| format!("Row error: {}", e))?;
                    let aggregate: super::types::AggregateMetrics = serde_json::from_str(&agg_json)
                        .unwrap_or(super::types::AggregateMetrics {
                            pass_rate: 0.0,
                            mean_iterations: 0.0,
                            mean_duration_ms: 0.0,
                            trial_count: 0,
                        });
                    // We only need the aggregate for comparison; construct a minimal ExperimentResult
                    let config: super::types::ExperimentConfig =
                        serde_json::from_str("{}").unwrap_or_else(|_| serde_json::from_value(serde_json::json!({})).unwrap());
                    results.push((num, super::types::ExperimentResult {
                        config,
                        trials: vec![],
                        aggregate,
                        accepted,
                        reason: String::new(),
                        p_value: None,
                        ai_recommendation: None,
                    }));
                }
                Ok(results)
            }
        })?;

        Ok((summary, experiments))
    };

    let (summary_a, exps_a) = load_campaign(campaign_id_a)?;
    let (summary_b, exps_b) = load_campaign(campaign_id_b)?;

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
