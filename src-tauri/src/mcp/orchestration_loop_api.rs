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
use crate::orchestration_loop::loop_engine::LoopStartError;
use crate::orchestration_loop::restart_path::{self, LoopHost};
use crate::orchestration_loop::{loop_engine, types::*};

/// The restart host for a loop started over HTTP: the runner's instance
/// manager, its AppHandle (so a relaunched slot keeps its `spawn_placement`)
/// and the bound API port.
fn loop_host(state: &ApiState) -> LoopHost {
    LoopHost::new(
        state.instance_manager.clone(),
        Some(state.app_handle.clone()),
        state
            .app_state
            .api_port
            .load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// The 409 for a refused start. An unsupported restart mode carries its typed
/// code in `code` (the `RestartUnsupportedCode` wire token, e.g.
/// `target_is_orchestrator`) and its message begins `unsupported here: `.
fn start_refusal(context: &str, e: LoopStartError) -> (StatusCode, Json<ApiResponse<()>>) {
    let mut body = match &e {
        LoopStartError::Unsupported { .. } => api_error(e.to_string()),
        LoopStartError::Refused(_) => api_error(format!("{context}: {e}")),
    };
    body.code = e
        .code()
        .map(|code| restart_path::code_token(code).to_string());
    (StatusCode::CONFLICT, Json(body))
}

// --- Backwards-compatible single-loop endpoints (use default loop ID) ---

/// POST /orchestration-loop/start
async fn start(
    State(state): State<Arc<ApiState>>,
    Json(config): Json<OrchestrationLoopConfig>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let states = state.app_state.orchestration_loops.clone();
    let host = loop_host(&state);

    loop_engine::start_loop_compat(states, config, &host)
        .await
        .map_err(|e| start_refusal("Failed to start loop", e))?;

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

/// POST /orchestration-loop/restart-capability
///
/// Read-only preflight: resolves how the posted config's between-iterations
/// mode would restart its target, without starting anything. Always 200 — an
/// unsupported mode is a verdict (`supported: false` + `code` + `reason`), not
/// an error.
async fn restart_capability(
    State(state): State<Arc<ApiState>>,
    Json(config): Json<OrchestrationLoopConfig>,
) -> Json<ApiResponse<RestartCapability>> {
    let host = loop_host(&state);
    Json(ApiResponse::success(
        restart_path::restart_capability(&config, &host).await,
    ))
}

// --- Multi-loop endpoints ---

/// POST /orchestration-loop/start-multi
async fn start_multi(
    State(state): State<Arc<ApiState>>,
    Json(config): Json<MultiLoopConfig>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let states = state.app_state.orchestration_loops.clone();
    let host = loop_host(&state);

    loop_engine::start_multi_loop(states, config, &host)
        .await
        .map_err(|e| start_refusal("Failed to start multi-loop", e))?;

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
                Json(api_error(format!(
                    "Failed to stop loop '{}': {}",
                    loop_id, e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(format!(
        "loop '{}' stopped",
        loop_id
    ))))
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
async fn status_all(State(state): State<Arc<ApiState>>) -> Json<ApiResponse<MultiLoopStatus>> {
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
        .route(
            "/orchestration-loop/restart-capability",
            post(restart_capability),
        )
        // Multi-loop routes
        .route("/orchestration-loop/start-multi", post(start_multi))
        .route("/orchestration-loop/stop-all", post(stop_all))
        .route("/orchestration-loop/status-all", get(status_all))
        .route("/orchestration-loop/{loop_id}/stop", post(stop_by_id))
        .route(
            "/orchestration-loop/{loop_id}/signal-restart",
            post(signal_restart_by_id),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration_loop::restart_path::RestartUnsupported;

    #[test]
    fn an_unsupported_start_is_a_409_carrying_the_typed_code() {
        let (status, Json(body)) = start_refusal(
            "Failed to start loop",
            LoopStartError::Unsupported {
                loop_id: None,
                unsupported: RestartUnsupported {
                    code: RestartUnsupportedCode::TargetIsOrchestrator,
                    reason: "restarting it would end the loop".into(),
                },
            },
        );
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body.code.as_deref(), Some("target_is_orchestrator"));
        let msg = body.error.unwrap();
        assert!(
            msg.starts_with("unsupported here: target_is_orchestrator: "),
            "{msg}"
        );
    }

    #[test]
    fn any_other_refusal_keeps_its_context_and_carries_no_code() {
        let (status, Json(body)) = start_refusal(
            "Failed to start multi-loop",
            LoopStartError::Refused("No loops configured".into()),
        );
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body.code, None);
        assert_eq!(
            body.error.as_deref(),
            Some("Failed to start multi-loop: No loops configured")
        );
    }
}
