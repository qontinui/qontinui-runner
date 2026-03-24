//! Token usage analytics HTTP API handlers.
//!
//! Provides HTTP endpoints for the LLM Observability dashboard,
//! exposing aggregated cost, token, and latency analytics from the
//! `phase_token_usage` table.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::error;

use crate::mcp::types::ApiState;

// ============================================================================
// Query parameters
// ============================================================================

/// Shared query parameters for analytics endpoints.
#[derive(Debug, Deserialize)]
pub struct TimeRangeParams {
    /// Number of days to look back (default: 7).
    pub days: Option<u32>,
    /// Maximum number of results to return (default: 50, used by task_run_costs).
    pub limit: Option<u32>,
}

// ============================================================================
// Handler functions
// ============================================================================

/// GET /analytics/token-usage/summary
///
/// Returns an aggregate summary of token usage for the given time range.
pub async fn get_token_usage_summary(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let summary = state
        .app_state
        .checkpoint_db
        .get_token_usage_summary(days)
        .map_err(|e| {
            error!("Failed to get token usage summary: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e)
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": summary,
        "days": days,
    })))
}

/// GET /analytics/token-usage/daily
///
/// Returns daily cost breakdown for the given time range.
pub async fn get_daily_cost(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let rows = state
        .app_state
        .checkpoint_db
        .get_daily_cost(days)
        .map_err(|e| {
            error!("Failed to get daily cost: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e)
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": rows,
        "days": days,
        "count": rows.len(),
    })))
}

/// GET /analytics/token-usage/by-model
///
/// Returns cost breakdown by AI model for the given time range.
pub async fn get_cost_by_model(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let rows = state
        .app_state
        .checkpoint_db
        .get_cost_by_model(days)
        .map_err(|e| {
            error!("Failed to get cost by model: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e)
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": rows,
        "days": days,
        "count": rows.len(),
    })))
}

/// GET /analytics/token-usage/by-phase
///
/// Returns cost breakdown by workflow phase for the given time range.
pub async fn get_cost_by_phase(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let rows = state
        .app_state
        .checkpoint_db
        .get_cost_by_phase(days)
        .map_err(|e| {
            error!("Failed to get cost by phase: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e)
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": rows,
        "days": days,
        "count": rows.len(),
    })))
}

/// GET /analytics/token-usage/by-provider
///
/// Returns latency statistics by AI provider for the given time range.
pub async fn get_provider_latency(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let rows = state
        .app_state
        .checkpoint_db
        .get_provider_latency(days)
        .map_err(|e| {
            error!("Failed to get provider latency: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e)
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": rows,
        "days": days,
        "count": rows.len(),
    })))
}

/// GET /analytics/token-usage/task-runs
///
/// Returns per-task-run cost breakdown, ordered by total cost descending.
pub async fn get_task_run_costs(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);
    let limit = params.limit.unwrap_or(50);

    let rows = state
        .app_state
        .checkpoint_db
        .get_task_run_costs(days, limit)
        .map_err(|e| {
            error!("Failed to get task run costs: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e)
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": rows,
        "days": days,
        "limit": limit,
        "count": rows.len(),
    })))
}

// ============================================================================
// Route registration
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/analytics/token-usage/summary",
            get(get_token_usage_summary),
        )
        .route("/analytics/token-usage/daily", get(get_daily_cost))
        .route("/analytics/token-usage/by-model", get(get_cost_by_model))
        .route("/analytics/token-usage/by-phase", get(get_cost_by_phase))
        .route(
            "/analytics/token-usage/by-provider",
            get(get_provider_latency),
        )
        .route(
            "/analytics/token-usage/task-runs",
            get(get_task_run_costs),
        )
}
