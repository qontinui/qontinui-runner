//! HTTP API endpoints for the orchestration loop.

use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::orchestration_loop::{loop_engine, types::*};

/// POST /orchestration-loop/start
async fn start(
    State(state): State<Arc<ApiState>>,
    Json(config): Json<OrchestrationLoopConfig>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let loop_state = state.app_state.orchestration_loop.clone();

    loop_engine::start_loop(loop_state, config)
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
    let loop_state = state.app_state.orchestration_loop.clone();

    loop_engine::stop_loop(loop_state).await.map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(api_error(format!("Failed to stop loop: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success("stopped".to_string())))
}

/// GET /orchestration-loop/status
async fn status(State(state): State<Arc<ApiState>>) -> Json<ApiResponse<OrchestrationLoopStatus>> {
    let loop_state = state.app_state.orchestration_loop.clone();
    let status = loop_engine::get_status(loop_state).await;
    Json(ApiResponse::success(status))
}

/// POST /orchestration-loop/signal-restart
async fn signal_restart(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let loop_state = state.app_state.orchestration_loop.clone();

    loop_engine::signal_restart(loop_state).await.map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(api_error(format!("Failed to signal restart: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success("restart signaled".to_string())))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/orchestration-loop/start", post(start))
        .route("/orchestration-loop/stop", post(stop))
        .route("/orchestration-loop/status", get(status))
        .route("/orchestration-loop/signal-restart", post(signal_restart))
}
