//! Tauri commands for LLM observability / token usage analytics.

use crate::commands::AppState;
use crate::database::token_analytics::*;
use std::sync::Arc;
use tauri::State;

/// Get daily cost breakdown for the last N days (default: 7).
#[tauri::command]
pub async fn get_daily_cost(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<Vec<DailyCostRow>, String> {
    let d = days.unwrap_or(7);
    if let Some(pg) = &state.pg_db {
        pg.get_daily_cost(d).await
    } else {
        state.checkpoint_db.get_daily_cost(d)
    }
}

/// Get cost breakdown by AI model for the last N days (default: 7).
#[tauri::command]
pub async fn get_cost_by_model(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<Vec<ModelCostRow>, String> {
    let d = days.unwrap_or(7);
    if let Some(pg) = &state.pg_db {
        pg.get_cost_by_model(d).await
    } else {
        state.checkpoint_db.get_cost_by_model(d)
    }
}

/// Get cost breakdown by workflow phase for the last N days (default: 7).
#[tauri::command]
pub async fn get_cost_by_phase(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<Vec<PhaseCostRow>, String> {
    let d = days.unwrap_or(7);
    if let Some(pg) = &state.pg_db {
        pg.get_cost_by_phase(d).await
    } else {
        state.checkpoint_db.get_cost_by_phase(d)
    }
}

/// Get latency stats by AI provider for the last N days (default: 7).
#[tauri::command]
pub async fn get_provider_latency(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<Vec<ProviderLatencyRow>, String> {
    let d = days.unwrap_or(7);
    if let Some(pg) = &state.pg_db {
        pg.get_provider_latency(d).await
    } else {
        state.checkpoint_db.get_provider_latency(d)
    }
}

/// Get per-task-run cost breakdown for the last N days, limited to top N runs.
#[tauri::command]
pub async fn get_task_run_costs(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
    limit: Option<u32>,
) -> Result<Vec<TaskRunCostRow>, String> {
    let d = days.unwrap_or(7);
    let l = limit.unwrap_or(50);
    if let Some(pg) = &state.pg_db {
        pg.get_task_run_costs(d, l).await
    } else {
        state.checkpoint_db.get_task_run_costs(d, l)
    }
}

/// Get cost breakdown by target application for the last N days (default: 7).
#[tauri::command]
pub async fn get_cost_by_target_app(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<Vec<TargetAppCostRow>, String> {
    let d = days.unwrap_or(7);
    if let Some(pg) = &state.pg_db {
        pg.get_cost_by_target_app(d).await
    } else {
        state.checkpoint_db.get_cost_by_target_app(d)
    }
}

/// Get aggregate token usage summary for the last N days (default: 7).
#[tauri::command]
pub async fn get_token_usage_summary(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<TokenUsageSummary, String> {
    let d = days.unwrap_or(7);
    if let Some(pg) = &state.pg_db {
        pg.get_token_usage_summary(d).await
    } else {
        state.checkpoint_db.get_token_usage_summary(d)
    }
}
