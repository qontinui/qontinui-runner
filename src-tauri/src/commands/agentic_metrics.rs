//! Tauri commands for agentic metric scores and trends.

use crate::auth::AuthManager;
use crate::commands::AppState;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;
use tracing::{error, info, warn};

/// Get all agentic metric scores for a specific task run.
#[tauri::command]
pub async fn get_agentic_scores(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<Vec<crate::database::agentic_metrics_ops::AgenticMetricScoreRow>, String> {
    state.pg_db.get_agentic_scores_for_run(&task_run_id).await
}

/// Get aggregate agentic metric stats over a time period.
#[tauri::command]
pub async fn get_agentic_metric_aggregates(
    state: State<'_, Arc<AppState>>,
    days: Option<i64>,
) -> Result<Vec<crate::database::agentic_metrics_ops::AgenticMetricAggregate>, String> {
    let interval = format!("{} days", days.unwrap_or(30));
    state
        .pg_db
        .get_agentic_metric_aggregates(&interval)
        .await
}

/// Get composite agentic score trend over time, grouped by date.
#[tauri::command]
pub async fn get_composite_score_trend(
    state: State<'_, Arc<AppState>>,
    days: Option<i64>,
) -> Result<Vec<crate::database::agentic_metrics_ops::CompositeScoreTrendPoint>, String> {
    state
        .pg_db
        .get_composite_score_trend(days.unwrap_or(30))
        .await
}

/// Manually trigger baseline recomputation.
#[tauri::command]
pub async fn recompute_agentic_baselines(state: State<'_, Arc<AppState>>) -> Result<u32, String> {
    state.pg_db.recompute_agentic_baselines().await
}

// ============================================================================
// Backend Push Commands
// ============================================================================

/// Get API base URL for qontinui-web backend.
fn get_api_base_url() -> String {
    std::env::var("QONTINUI_API_URL").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "http://127.0.0.1:8000".to_string()
        } else {
            "https://qontinui-prod-py.eba-km2u4s23.eu-central-1.elasticbeanstalk.com".to_string()
        }
    })
}

/// Response from pushing scores to the backend.
#[derive(Debug, Serialize)]
pub struct PushScoresResult {
    pub pushed: u32,
    pub failed: u32,
    pub errors: Vec<String>,
}

/// Push agentic metric scores for a task run to the backend API.
///
/// Reads scores from PostgreSQL when available, falling back to SQLite.
/// POSTs them as a single batch to `/api/v1/feedback-scores/batch` with bearer auth.
#[tauri::command]
pub async fn push_agentic_scores_to_backend(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
    target_id: String,
    target_type: String,
) -> Result<PushScoresResult, String> {
    info!(
        "Pushing agentic scores for task_run_id={} to backend (target={}:{})",
        task_run_id, target_type, target_id
    );

    let scores = state.pg_db.get_agentic_scores_for_run(&task_run_id).await?;

    if scores.is_empty() {
        info!("No agentic scores found for task_run_id={}", task_run_id);
        return Ok(PushScoresResult {
            pushed: 0,
            failed: 0,
            errors: vec![],
        });
    }

    let auth_manager = AuthManager::new();
    if !auth_manager.has_tokens() {
        warn!("Cannot push agentic scores: not authenticated");
        return Err("Not authenticated. Please log in.".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/feedback-scores/batch", get_api_base_url());

    // Collect all scores into a single batch payload.
    let payloads: Vec<serde_json::Value> = scores
        .iter()
        .map(|score| {
            serde_json::json!({
                "target_id": target_id,
                "target_type": target_type,
                "name": score.metric_type,
                "value": score.score,
                "source": "runner_agentic_metrics",
                "metadata": {
                    "confidence": score.confidence,
                    "task_run_id": task_run_id,
                },
            })
        })
        .collect();

    let total = payloads.len() as u32;

    match client
        .post(&url)
        .bearer_auth(&access_token)
        .json(&payloads)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            info!("Pushed {} agentic scores in a single batch", total);
            Ok(PushScoresResult {
                pushed: total,
                failed: 0,
                errors: vec![],
            })
        }
        Ok(resp) => {
            let status = resp.status();
            let err_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            let msg = format!("Batch push failed: {} - {}", status, err_text);
            warn!("{}", msg);
            Ok(PushScoresResult {
                pushed: 0,
                failed: total,
                errors: vec![msg],
            })
        }
        Err(e) => {
            let msg = format!("Network error pushing batch scores: {}", e);
            error!("{}", msg);
            Ok(PushScoresResult {
                pushed: 0,
                failed: total,
                errors: vec![msg],
            })
        }
    }
}

/// Push agentic scores for the most recent task run to the backend.
///
/// Finds the most recent task_run_id that has agentic scores, then
/// delegates to `push_agentic_scores_to_backend`.
#[tauri::command]
pub async fn push_latest_agentic_scores(
    state: State<'_, Arc<AppState>>,
    target_id: String,
    target_type: String,
) -> Result<PushScoresResult, String> {
    info!("Finding most recent task run with agentic scores...");

    let task_run_id: String = state.pg_db.get_latest_scored_task_run_id()
        .await?
        .ok_or_else(|| "No agentic scores found".to_string())?;

    info!("Most recent task_run_id with scores: {}", task_run_id);

    push_agentic_scores_to_backend(state, task_run_id, target_id, target_type).await
}
