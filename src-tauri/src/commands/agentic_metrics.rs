//! Tauri commands for agentic metric scores and trends.

use crate::auth::AuthManager;
use crate::commands::AppState;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;
use tracing::{error, info, warn};

/// Get all agentic metric scores for a specific task run.
#[tauri::command]
pub fn get_agentic_scores(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<Vec<crate::database::agentic_metrics_ops::AgenticMetricScoreRow>, String> {
    state.checkpoint_db.get_agentic_scores_for_run(&task_run_id)
}

/// Get aggregate agentic metric stats over a time period.
#[tauri::command]
pub fn get_agentic_metric_aggregates(
    state: State<'_, Arc<AppState>>,
    days: Option<i64>,
) -> Result<Vec<crate::database::agentic_metrics_ops::AgenticMetricAggregate>, String> {
    state
        .checkpoint_db
        .get_agentic_metric_aggregates(days.unwrap_or(30))
}

/// Get composite agentic score trend over time, grouped by date.
#[tauri::command]
pub fn get_composite_score_trend(
    state: State<'_, Arc<AppState>>,
    days: Option<i64>,
) -> Result<Vec<crate::database::agentic_metrics_ops::CompositeScoreTrendPoint>, String> {
    state
        .checkpoint_db
        .get_composite_score_trend(days.unwrap_or(30))
}

/// Manually trigger baseline recomputation.
#[tauri::command]
pub fn recompute_agentic_baselines(state: State<'_, Arc<AppState>>) -> Result<u32, String> {
    state.checkpoint_db.with_conn(|conn| {
        crate::meta_optimizer::agentic_metrics::scoring::recompute_all_baselines(conn)
    })
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
/// Reads scores from the local SQLite database and POSTs each to
/// `/api/v1/feedback-scores` with bearer auth.
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

    let scores = state
        .checkpoint_db
        .get_agentic_scores_for_run(&task_run_id)?;

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
    let url = format!("{}/api/v1/feedback-scores", get_api_base_url());

    let mut pushed = 0u32;
    let mut failed = 0u32;
    let mut errors = Vec::new();

    for score in &scores {
        let payload = serde_json::json!({
            "target_id": target_id,
            "target_type": target_type,
            "metric_type": score.metric_type,
            "score": score.score,
            "confidence": score.confidence,
            "task_run_id": task_run_id,
            "source": "runner_agentic_metrics",
        });

        match client
            .post(&url)
            .bearer_auth(&access_token)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                pushed += 1;
            }
            Ok(resp) => {
                let status = resp.status();
                let err_text = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string());
                let msg = format!(
                    "Failed to push {} score: {} - {}",
                    score.metric_type, status, err_text
                );
                warn!("{}", msg);
                errors.push(msg);
                failed += 1;
            }
            Err(e) => {
                let msg = format!("Network error pushing {} score: {}", score.metric_type, e);
                error!("{}", msg);
                errors.push(msg);
                failed += 1;
            }
        }
    }

    info!(
        "Pushed agentic scores: {} succeeded, {} failed",
        pushed, failed
    );

    Ok(PushScoresResult {
        pushed,
        failed,
        errors,
    })
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

    let task_run_id: String = state.checkpoint_db.with_conn(|conn| {
        conn.query_row(
            "SELECT task_run_id FROM agentic_metric_scores ORDER BY scored_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("No agentic scores found: {}", e))
    })?;

    info!("Most recent task_run_id with scores: {}", task_run_id);

    push_agentic_scores_to_backend(state, task_run_id, target_id, target_type).await
}
