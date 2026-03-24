//! Tauri commands for LLM observability / token usage analytics.

use crate::commands::AppState;
use crate::database::token_analytics::*;
use std::sync::Arc;
use tauri::State;

/// Get daily cost breakdown for the last N days (default: 7).
#[tauri::command]
pub fn get_daily_cost(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<Vec<DailyCostRow>, String> {
    state.checkpoint_db.get_daily_cost(days.unwrap_or(7))
}

/// Get cost breakdown by AI model for the last N days (default: 7).
#[tauri::command]
pub fn get_cost_by_model(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<Vec<ModelCostRow>, String> {
    state.checkpoint_db.get_cost_by_model(days.unwrap_or(7))
}

/// Get cost breakdown by workflow phase for the last N days (default: 7).
#[tauri::command]
pub fn get_cost_by_phase(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<Vec<PhaseCostRow>, String> {
    state.checkpoint_db.get_cost_by_phase(days.unwrap_or(7))
}

/// Get latency stats by AI provider for the last N days (default: 7).
#[tauri::command]
pub fn get_provider_latency(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<Vec<ProviderLatencyRow>, String> {
    state.checkpoint_db.get_provider_latency(days.unwrap_or(7))
}

/// Get per-task-run cost breakdown for the last N days, limited to top N runs.
#[tauri::command]
pub fn get_task_run_costs(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
    limit: Option<u32>,
) -> Result<Vec<TaskRunCostRow>, String> {
    state
        .checkpoint_db
        .get_task_run_costs(days.unwrap_or(7), limit.unwrap_or(50))
}

/// Get aggregate token usage summary for the last N days (default: 7).
#[tauri::command]
pub fn get_token_usage_summary(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<TokenUsageSummary, String> {
    state
        .checkpoint_db
        .get_token_usage_summary(days.unwrap_or(7))
}
