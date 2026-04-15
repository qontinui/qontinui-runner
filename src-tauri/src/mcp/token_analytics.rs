//! Token usage analytics HTTP API handlers.
//!
//! Provides HTTP endpoints for the LLM Observability dashboard,
//! exposing aggregated cost, token, and latency analytics from the
//! `phase_token_usage` table. Uses PostgreSQL when available, falls back to SQLite.

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
// Handler functions (PostgreSQL via spawn_blocking)
// ============================================================================

/// GET /analytics/token-usage/summary
pub async fn get_token_usage_summary(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let summary = state
        .app_state
        .pg_db
        .get_token_usage_summary(days)
        .await
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
pub async fn get_daily_cost(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let rows = state
        .app_state
        .pg_db
        .get_daily_cost(days)
        .await
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
pub async fn get_cost_by_model(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let rows = state
        .app_state
        .pg_db
        .get_cost_by_model(days)
        .await
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
pub async fn get_cost_by_phase(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let rows = state
        .app_state
        .pg_db
        .get_cost_by_phase(days)
        .await
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
pub async fn get_provider_latency(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let rows = state
        .app_state
        .pg_db
        .get_provider_latency(days)
        .await
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
pub async fn get_task_run_costs(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);
    let limit = params.limit.unwrap_or(50);

    let rows = state
        .app_state
        .pg_db
        .get_task_run_costs(days, limit)
        .await
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

/// GET /analytics/token-usage/by-target-app
pub async fn get_cost_by_target_app(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let rows = state
        .app_state
        .pg_db
        .get_cost_by_target_app(days)
        .await
        .map_err(|e| {
            error!("Failed to get cost by target app: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e)
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": rows,
        "days": days,
        "count": rows.len(),
    })))
}

/// GET /analytics/token-usage/by-page
pub async fn get_cost_by_target_page(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let days = params.days.unwrap_or(7);

    let rows = state
        .app_state
        .pg_db
        .get_cost_by_target_page(days)
        .await
        .map_err(|e| {
            error!("Failed to get cost by target page: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e)
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": rows,
        "days": days,
        "count": rows.len(),
    })))
}

/// GET /analytics/token-usage/cost-per-interaction
pub async fn get_cost_per_interaction(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // checkpoint_db removed — token analytics not yet migrated to PG
    let days = params.days.unwrap_or(7);
    let rows: Vec<serde_json::Value> = vec![];
    Ok(Json(
        serde_json::json!({ "success": true, "data": rows, "days": days, "count": 0 }),
    ))
}

/// GET /analytics/token-usage/page-complexity
pub async fn get_page_complexity(
    State(_state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // checkpoint_db removed — token analytics not yet migrated to PG
    let days = params.days.unwrap_or(7);
    let rows: Vec<serde_json::Value> = vec![];
    Ok(Json(
        serde_json::json!({ "success": true, "data": rows, "days": days, "count": 0 }),
    ))
}

/// GET /analytics/token-usage/model-action-matrix
pub async fn get_model_action_matrix(
    State(_state): State<Arc<ApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // checkpoint_db removed — token analytics not yet migrated to PG
    let days = params.days.unwrap_or(7);
    let rows: Vec<serde_json::Value> = vec![];
    Ok(Json(
        serde_json::json!({ "success": true, "data": rows, "days": days, "count": 0 }),
    ))
}

/// GET /analytics/account-usage
///
/// Probes all configured Claude accounts for their 7-day utilization and
/// returns each account's actual usage, expected usage at this point in the
/// billing period, and the delta between them.
pub async fn get_account_usage(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let config_dirs = crate::settings::get_claude_config_dirs();
    if config_dirs.is_empty() {
        return Ok(Json(serde_json::json!({
            "success": true,
            "data": [],
            "count": 0,
        })));
    }

    let futures: Vec<_> = config_dirs
        .into_iter()
        .map(|dir| crate::commands::ai_settings::probe_account_usage(dir))
        .collect();

    let results = futures::future::join_all(futures).await;
    let count = results.len();

    Ok(Json(serde_json::json!({
        "success": true,
        "data": results,
        "count": count,
    })))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/analytics/account-usage", get(get_account_usage))
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
        .route("/analytics/token-usage/task-runs", get(get_task_run_costs))
        .route(
            "/analytics/token-usage/by-target-app",
            get(get_cost_by_target_app),
        )
        .route(
            "/analytics/token-usage/by-page",
            get(get_cost_by_target_page),
        )
        .route(
            "/analytics/token-usage/cost-per-interaction",
            get(get_cost_per_interaction),
        )
        .route(
            "/analytics/token-usage/page-complexity",
            get(get_page_complexity),
        )
        .route(
            "/analytics/token-usage/model-action-matrix",
            get(get_model_action_matrix),
        )
}
