//! Tauri commands for agentic metric scores and trends.

use crate::commands::AppState;
use std::sync::Arc;
use tauri::State;

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
