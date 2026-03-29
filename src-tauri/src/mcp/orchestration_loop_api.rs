//! HTTP API endpoints for the orchestration loop.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::orchestration_loop::{loop_engine, types::*};

// --- Backwards-compatible single-loop endpoints (use default loop ID) ---

/// POST /orchestration-loop/start
async fn start(
    State(state): State<Arc<ApiState>>,
    Json(config): Json<OrchestrationLoopConfig>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let states = state.app_state.orchestration_loops.clone();

    loop_engine::start_loop_compat(states, config)
        .await
        .map_err(|e| {
            (
                StatusCode::CONFLICT,
                Json(api_error(format!("Failed to start loop: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success("started".to_string())))
}

/// POST /orchestration-loop/stop
async fn stop(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let states = state.app_state.orchestration_loops.clone();

    loop_engine::stop_loop_compat(states).await.map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(api_error(format!("Failed to stop loop: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success("stopped".to_string())))
}

/// GET /orchestration-loop/status
async fn status(State(state): State<Arc<ApiState>>) -> Json<ApiResponse<OrchestrationLoopStatus>> {
    let states = state.app_state.orchestration_loops.clone();
    let status = loop_engine::get_status_compat(states).await;
    Json(ApiResponse::success(status))
}

/// POST /orchestration-loop/signal-restart
async fn signal_restart(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let states = state.app_state.orchestration_loops.clone();

    loop_engine::signal_restart_compat(states)
        .await
        .map_err(|e| {
            (
                StatusCode::CONFLICT,
                Json(api_error(format!("Failed to signal restart: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success("restart signaled".to_string())))
}

// --- Multi-loop endpoints ---

/// POST /orchestration-loop/start-multi
async fn start_multi(
    State(state): State<Arc<ApiState>>,
    Json(config): Json<MultiLoopConfig>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let states = state.app_state.orchestration_loops.clone();

    loop_engine::start_multi_loop(states, config)
        .await
        .map_err(|e| {
            (
                StatusCode::CONFLICT,
                Json(api_error(format!("Failed to start multi-loop: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success("multi-loop started".to_string())))
}

/// POST /orchestration-loop/{loop_id}/stop
async fn stop_by_id(
    State(state): State<Arc<ApiState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let states = state.app_state.orchestration_loops.clone();

    loop_engine::stop_loop_by_id(states, &loop_id)
        .await
        .map_err(|e| {
            (
                StatusCode::CONFLICT,
                Json(api_error(format!("Failed to stop loop '{}': {}", loop_id, e))),
            )
        })?;

    Ok(Json(ApiResponse::success(format!("loop '{}' stopped", loop_id))))
}

/// POST /orchestration-loop/stop-all
async fn stop_all(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let states = state.app_state.orchestration_loops.clone();

    loop_engine::stop_all_loops(states).await.map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(api_error(format!("Failed to stop all loops: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success("all loops stopped".to_string())))
}

/// GET /orchestration-loop/status-all
async fn status_all(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<MultiLoopStatus>> {
    let states = state.app_state.orchestration_loops.clone();
    let status = loop_engine::get_multi_status(states).await;
    Json(ApiResponse::success(status))
}

/// POST /orchestration-loop/{loop_id}/signal-restart
async fn signal_restart_by_id(
    State(state): State<Arc<ApiState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let states = state.app_state.orchestration_loops.clone();

    loop_engine::signal_restart_by_id(states, &loop_id)
        .await
        .map_err(|e| {
            (
                StatusCode::CONFLICT,
                Json(api_error(format!(
                    "Failed to signal restart for loop '{}': {}",
                    loop_id, e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(format!(
        "restart signaled for loop '{}'",
        loop_id
    ))))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // Backwards-compatible single-loop routes
        .route("/orchestration-loop/start", post(start))
        .route("/orchestration-loop/stop", post(stop))
        .route("/orchestration-loop/status", get(status))
        .route("/orchestration-loop/signal-restart", post(signal_restart))
        // Multi-loop routes
        .route("/orchestration-loop/start-multi", post(start_multi))
        .route("/orchestration-loop/stop-all", post(stop_all))
        .route("/orchestration-loop/status-all", get(status_all))
        .route(
            "/orchestration-loop/{loop_id}/stop",
            post(stop_by_id),
        )
        .route(
            "/orchestration-loop/{loop_id}/signal-restart",
            post(signal_restart_by_id),
        )
}
