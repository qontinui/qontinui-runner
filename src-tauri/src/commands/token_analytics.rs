//! Tauri commands for LLM observability / token usage analytics.

use crate::commands::compartments::StorageCompartment;
use crate::database::token_analytics::*;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;

/// Get daily cost breakdown for the last N days (default: 7).
#[tauri::command]
pub async fn get_daily_cost(
    state: State<'_, StorageCompartment>,
    days: Option<u32>,
) -> Result<Vec<DailyCostRow>, String> {
    let d = days.unwrap_or(7);
    state.pg_db().get_daily_cost(d).await
}

/// Get cost breakdown by AI model for the last N days (default: 7).
#[tauri::command]
pub async fn get_cost_by_model(
    state: State<'_, StorageCompartment>,
    days: Option<u32>,
) -> Result<Vec<ModelCostRow>, String> {
    let d = days.unwrap_or(7);
    state.pg_db().get_cost_by_model(d).await
}

/// Get cost breakdown by workflow phase for the last N days (default: 7).
#[tauri::command]
pub async fn get_cost_by_phase(
    state: State<'_, StorageCompartment>,
    days: Option<u32>,
) -> Result<Vec<PhaseCostRow>, String> {
    let d = days.unwrap_or(7);
    state.pg_db().get_cost_by_phase(d).await
}

/// Get latency stats by AI provider for the last N days (default: 7).
#[tauri::command]
pub async fn get_provider_latency(
    state: State<'_, StorageCompartment>,
    days: Option<u32>,
) -> Result<Vec<ProviderLatencyRow>, String> {
    let d = days.unwrap_or(7);
    state.pg_db().get_provider_latency(d).await
}

/// Get per-task-run cost breakdown for the last N days, limited to top N runs.
#[tauri::command]
pub async fn get_task_run_costs(
    state: State<'_, StorageCompartment>,
    days: Option<u32>,
    limit: Option<u32>,
) -> Result<Vec<TaskRunCostRow>, String> {
    let d = days.unwrap_or(7);
    let l = limit.unwrap_or(50);
    state.pg_db().get_task_run_costs(d, l).await
}

/// Get cost breakdown by target application for the last N days (default: 7).
#[tauri::command]
pub async fn get_cost_by_target_app(
    state: State<'_, StorageCompartment>,
    days: Option<u32>,
) -> Result<Vec<TargetAppCostRow>, String> {
    let d = days.unwrap_or(7);
    state.pg_db().get_cost_by_target_app(d).await
}

/// Get aggregate token usage summary for the last N days (default: 7).
#[tauri::command]
pub async fn get_token_usage_summary(
    state: State<'_, StorageCompartment>,
    days: Option<u32>,
) -> Result<TokenUsageSummary, String> {
    let d = days.unwrap_or(7);
    state.pg_db().get_token_usage_summary(d).await
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_token_analytics")
        .invoke_handler(tauri::generate_handler![
            get_token_usage_summary,
            get_daily_cost,
            get_cost_by_model,
            get_cost_by_phase,
            get_provider_latency,
            get_task_run_costs,
            get_cost_by_target_app,
        ])
        .build()
}
