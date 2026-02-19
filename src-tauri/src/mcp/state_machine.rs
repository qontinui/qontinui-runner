//! State Machine handlers for MCP API
//!
//! Provides HTTP handlers for UI Bridge State Machine operations:
//! loading state machine configs, querying status, executing transitions,
//! and navigating between states via the Python bridge.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct LoadStateMachineRequest {
    pub config: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteTransitionRequest {
    pub transition_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigateRequest {
    pub target_states: Vec<String>,
}

// ============================================================================
// Handlers
// ============================================================================

/// POST /state-machine/load — Load a state machine configuration
pub async fn load_state_machine(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<LoadStateMachineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("State Machine API: Loading state machine config");

    let app_state = state.app_state.clone();
    let params = serde_json::json!({ "config": request.config });
    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("load_state_machine", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("State Machine API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("State Machine API: Config loaded successfully");
                Ok(Json(ApiResponse::success(
                    response
                        .data
                        .unwrap_or(serde_json::json!({"success": true})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to load state machine".to_string());
                error!("State Machine API: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("State Machine API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// GET /state-machine/status — Get loaded state machine status
pub async fn get_state_machine_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("State Machine API: Getting status");

    let app_state = state.app_state.clone();
    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("get_state_machine_status", None, timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("State Machine API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                Ok(Json(ApiResponse::success(
                    response
                        .data
                        .unwrap_or(serde_json::json!({"loaded": false})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get status".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("State Machine API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// GET /state-machine/active-states — Get currently active states
pub async fn get_active_states(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("State Machine API: Getting active states");

    let app_state = state.app_state.clone();
    let timeout = std::time::Duration::from_secs(15);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("sm_get_active_states", None, timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("State Machine API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                Ok(Json(ApiResponse::success(
                    response
                        .data
                        .unwrap_or(serde_json::json!({"active_states": []})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get active states".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("State Machine API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// POST /state-machine/execute-transition — Execute a transition by ID
pub async fn execute_transition(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecuteTransitionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "State Machine API: Executing transition {}",
        request.transition_id
    );

    let app_state = state.app_state.clone();
    let params = serde_json::json!({ "transition_id": request.transition_id });
    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("sm_execute_transition", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("State Machine API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("State Machine API: Transition executed successfully");
                Ok(Json(ApiResponse::success(
                    response
                        .data
                        .unwrap_or(serde_json::json!({"success": true})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to execute transition".to_string());
                error!("State Machine API: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("State Machine API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// POST /state-machine/navigate — Navigate to target states using pathfinding
pub async fn navigate_to_states(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<NavigateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "State Machine API: Navigating to states {:?}",
        request.target_states
    );

    let app_state = state.app_state.clone();
    let params = serde_json::json!({ "target_states": request.target_states });
    let timeout = std::time::Duration::from_secs(60);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("sm_navigate_to_states", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("State Machine API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("State Machine API: Navigation completed");
                Ok(Json(ApiResponse::success(
                    response
                        .data
                        .unwrap_or(serde_json::json!({"success": true})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Navigation failed".to_string());
                error!("State Machine API: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("State Machine API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// GET /state-machine/available-transitions — Get transitions available from current states
pub async fn get_available_transitions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("State Machine API: Getting available transitions");

    let app_state = state.app_state.clone();
    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("sm_get_available_transitions", None, timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("State Machine API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                Ok(Json(ApiResponse::success(
                    response
                        .data
                        .unwrap_or(serde_json::json!({"transitions": []})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get available transitions".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("State Machine API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// DELETE /state-machine — Clear loaded state machine
pub async fn clear_state_machine(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("State Machine API: Clearing state machine");

    let app_state = state.app_state.clone();
    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("clear_state_machine", None, timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("State Machine API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("State Machine API: State machine cleared");
                Ok(Json(ApiResponse::success(
                    serde_json::json!({"success": true}),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to clear state machine".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("State Machine API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{delete, get, post};

    axum::Router::new()
        .route("/state-machine/load", post(load_state_machine))
        .route("/state-machine/status", get(get_state_machine_status))
        .route("/state-machine/active-states", get(get_active_states))
        .route(
            "/state-machine/execute-transition",
            post(execute_transition),
        )
        .route("/state-machine/navigate", post(navigate_to_states))
        .route(
            "/state-machine/available-transitions",
            get(get_available_transitions),
        )
        .route("/state-machine", delete(clear_state_machine))
}
