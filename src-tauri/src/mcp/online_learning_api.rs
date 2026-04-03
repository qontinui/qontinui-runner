//! MCP API endpoints for the online learning system.
//!
//! Read-only query endpoints for diagnostics and the supervisor dashboard:
//! - Model routing Q-table state + policy snapshot
//! - Active drift signals
//! - Step-type scorecards (credit attribution aggregates)
//! - Strategy bank contents
//! - Experience summaries

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// =============================================================================
// Query parameters
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct DomainQuery {
    pub domain: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct TaskRunQuery {
    pub task_run_id: String,
}

// =============================================================================
// Response types
// =============================================================================

#[derive(Debug, Serialize)]
pub struct ModelRoutingState {
    pub q_table_size: usize,
    pub policy: Vec<crate::online_learning::bandit::PolicyEntry>,
    pub available_models: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DriftMonitorState {
    pub detector_count: usize,
    pub active_detectors: Vec<String>,
}

// =============================================================================
// Handlers
// =============================================================================

/// GET /online-learning/model-routing
///
/// Returns the current model routing bandit state: Q-table size, policy snapshot,
/// and available models.
pub async fn get_model_routing_state(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<ModelRoutingState>>, (StatusCode, Json<ApiResponse<()>>)> {
    let router_lock = crate::online_learning::model_router();
    match router_lock {
        Some(mtx) => {
            let router = mtx.lock().map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Lock poisoned: {}", e))),
                )
            })?;
            let state = ModelRoutingState {
                q_table_size: router.table_size(),
                policy: router.policy_snapshot(),
                available_models: router.available_models().to_vec(),
            };
            Ok(Json(ApiResponse::success(state)))
        }
        None => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Online learning not initialized")),
        )),
    }
}

/// GET /online-learning/model-routing/table
///
/// Returns the full model routing Q-table for detailed analysis.
pub async fn get_model_routing_table(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let rows = state
        .app_state
        .pg_db
        .load_model_routing_table()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to load routing table: {}", e))),
            )
        })?;

    let result: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(ctx, model, q, visits, sos)| {
            serde_json::json!({
                "context_key": ctx,
                "model_id": model,
                "q_value": q,
                "visit_count": visits,
                "sum_of_squares": sos,
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(result)))
}

/// GET /online-learning/drift-signals
///
/// Returns unacknowledged drift signals.
pub async fn get_drift_signals(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let signals = state
        .app_state
        .pg_db
        .get_unacknowledged_drift_signals()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get drift signals: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(signals)))
}

/// GET /online-learning/drift-monitor
///
/// Returns the current drift monitor state (detector count, active detectors).
pub async fn get_drift_monitor_state(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<DriftMonitorState>>, (StatusCode, Json<ApiResponse<()>>)> {
    let drift_lock = crate::online_learning::drift_monitor();
    match drift_lock {
        Some(mtx) => {
            let monitor = mtx.lock().map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Lock poisoned: {}", e))),
                )
            })?;
            let state = DriftMonitorState {
                detector_count: monitor.detector_count(),
                active_detectors: monitor.active_detector_keys(),
            };
            Ok(Json(ApiResponse::success(state)))
        }
        None => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("Online learning not initialized")),
        )),
    }
}

/// GET /online-learning/step-credits?task_run_id=...
///
/// Returns step credit assignments for a specific task run.
pub async fn get_step_credits(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TaskRunQuery>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let credits = state
        .app_state
        .pg_db
        .get_step_credits(&query.task_run_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get step credits: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(credits)))
}

/// GET /online-learning/step-scorecard?limit=20
///
/// Returns aggregated step-type scorecard across all runs.
pub async fn get_step_scorecard(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let scorecard = state
        .app_state
        .pg_db
        .get_step_type_scorecard(query.limit.unwrap_or(20))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get scorecard: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(scorecard)))
}

/// GET /online-learning/strategies
///
/// Returns active and candidate strategies from the strategy bank.
pub async fn get_strategies(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let strategies = state
        .app_state
        .pg_db
        .get_active_strategies()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get strategies: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(strategies)))
}

/// GET /online-learning/experiences?domain=backend&limit=20
///
/// Returns recent experience summaries, optionally filtered by domain.
pub async fn get_experiences(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DomainQuery>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let experiences = state
        .app_state
        .pg_db
        .get_experience_summaries(query.domain.as_deref(), query.limit.unwrap_or(20))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get experiences: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(experiences)))
}

// =============================================================================
// Route registration
// =============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;

    axum::Router::new()
        .route(
            "/online-learning/model-routing",
            get(get_model_routing_state),
        )
        .route(
            "/online-learning/model-routing/table",
            get(get_model_routing_table),
        )
        .route("/online-learning/drift-signals", get(get_drift_signals))
        .route(
            "/online-learning/drift-monitor",
            get(get_drift_monitor_state),
        )
        .route("/online-learning/step-credits", get(get_step_credits))
        .route("/online-learning/step-scorecard", get(get_step_scorecard))
        .route("/online-learning/strategies", get(get_strategies))
        .route("/online-learning/experiences", get(get_experiences))
}
