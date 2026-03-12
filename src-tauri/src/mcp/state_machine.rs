//! State Machine handlers for MCP API
//!
//! Provides HTTP handlers for UI Bridge State Machine operations:
//! loading state machine configs, querying status, executing transitions,
//! navigating between states via the Python bridge, and CRUD operations
//! for state machine configs, states, and transitions stored in SQLite.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::state_machine_configs::{
    storage, CreateSmConfigRequest, CreateSmStateRequest, CreateSmTransitionRequest, SmConfig,
    SmConfigFull, SmImportRequest, SmState, SmTransition,
};

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

    // Try Python bridge first for live transition availability
    let bridge_result = {
        let app_state_clone = app_state.clone();
        tokio::task::spawn_blocking(move || {
            with_default_bridge(&app_state_clone, |bridge| {
                if !bridge.is_running() {
                    return Err("Python executor not running".to_string());
                }
                bridge.send_command_and_wait("sm_get_available_transitions", None, timeout)
            })?
        })
        .await
    };

    // If Python bridge succeeds with non-empty transitions, return its result
    if let Ok(Ok(response)) = &bridge_result {
        if response.success {
            // Check if the bridge returned actual transitions (not just an empty list)
            let has_transitions = response
                .data
                .as_ref()
                .and_then(|d| d.get("transitions"))
                .and_then(|t| t.as_array())
                .map(|arr| !arr.is_empty())
                .unwrap_or(false);

            if has_transitions {
                return Ok(Json(ApiResponse::success(
                    response
                        .data
                        .clone()
                        .unwrap_or(serde_json::json!({"transitions": []})),
                )));
            }
            // Bridge succeeded but returned empty transitions (no active state).
            // Fall through to SQLite fallback which returns all defined transitions.
        }
    }

    // Fallback: get transitions from the loaded config's SQLite data
    // This runs when the Python bridge is unavailable OR returned empty transitions
    // (e.g., when no active state is detected because there is no target app).
    info!("State Machine API: Falling back to SQLite for transition data");
    let status_result = {
        let app_state_clone = app_state.clone();
        tokio::task::spawn_blocking(move || {
            with_default_bridge(&app_state_clone, |bridge| {
                if !bridge.is_running() {
                    return Err("Python executor not running".to_string());
                }
                bridge.send_command_and_wait("get_state_machine_status", None, timeout)
            })?
        })
        .await
    };

    // Check if state machine is loaded via status
    if let Ok(Ok(ref status_response)) = status_result {
        if status_response.success {
            if let Some(ref data) = status_response.data {
                let loaded = data
                    .get("loaded")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if loaded {
                    // State machine is loaded — get transitions from the database
                    // Use the most recent config that has transitions
                    let checkpoint_db = app_state.checkpoint_db.clone();
                    let db_transitions = tokio::task::spawn_blocking(move || {
                        let conn = checkpoint_db.get_conn_string().ok()?;
                        let configs = storage::list_configs(&conn).ok()?;
                        for config in configs.iter() {
                            if let Ok(Some(full)) = storage::get_config_full(&conn, &config.id) {
                                if !full.transitions.is_empty() {
                                    let transitions: Vec<serde_json::Value> = full
                                        .transitions
                                        .iter()
                                        .map(|t| {
                                            serde_json::json!({
                                                "id": t.transition_id,
                                                "name": t.name,
                                                "from_states": t.from_states,
                                                "activate_states": t.activate_states,
                                                "exit_states": t.exit_states,
                                            })
                                        })
                                        .collect();
                                    return Some(transitions);
                                }
                            }
                        }
                        None
                    })
                    .await
                    .ok()
                    .flatten();

                    if let Some(transitions) = db_transitions {
                        return Ok(Json(ApiResponse::success(
                            serde_json::json!({"transitions": transitions}),
                        )));
                    }
                }
            }
        }
    }

    // No state machine loaded
    Err((
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(api_error("No state machine loaded".to_string())),
    ))
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
// CRUD Handlers — Config, State, Transition management via HTTP
// ============================================================================

/// Helper to get a DB connection and map errors to HTTP responses.
fn db_conn(
    state: &Arc<ApiState>,
) -> Result<
    r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    state.app_state.checkpoint_db.get_conn().map_err(|e| {
        error!("State Machine CRUD: DB connection error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Database error: {}", e))),
        )
    })
}

/// GET /state-machine/configs — List all state machine configs
pub async fn list_configs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<SmConfig>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let conn = db_conn(&state)?;
    let configs = storage::list_configs(&conn).map_err(|e| {
        error!("State Machine CRUD: list_configs error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;
    Ok(Json(ApiResponse::success(configs)))
}

/// POST /state-machine/configs — Create a new state machine config
pub async fn create_config(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateSmConfigRequest>,
) -> Result<Json<ApiResponse<SmConfig>>, (StatusCode, Json<ApiResponse<()>>)> {
    let conn = db_conn(&state)?;
    let config = storage::insert_config(&conn, &request).map_err(|e| {
        error!("State Machine CRUD: create_config error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;
    info!(
        "State Machine CRUD: Created config '{}' ({})",
        config.name, config.id
    );
    Ok(Json(ApiResponse::success(config)))
}

/// GET /state-machine/configs/:id — Get a config with all states and transitions
pub async fn get_config(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<SmConfigFull>>, (StatusCode, Json<ApiResponse<()>>)> {
    let conn = db_conn(&state)?;
    let config = storage::get_config_full(&conn, &id).map_err(|e| {
        error!("State Machine CRUD: get_config error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;
    match config {
        Some(c) => Ok(Json(ApiResponse::success(c))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Config not found: {}", id))),
        )),
    }
}

/// DELETE /state-machine/configs/:id — Delete a config and its states/transitions
pub async fn delete_config(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let conn = db_conn(&state)?;
    let deleted = storage::delete_config(&conn, &id).map_err(|e| {
        error!("State Machine CRUD: delete_config error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;
    if deleted {
        info!("State Machine CRUD: Deleted config {}", id);
        Ok(Json(ApiResponse::success(
            serde_json::json!({"deleted": true}),
        )))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Config not found: {}", id))),
        ))
    }
}

/// POST /state-machine/configs/:config_id/states — Create a state within a config
pub async fn create_state(
    State(state): State<Arc<ApiState>>,
    Path(config_id): Path<String>,
    Json(request): Json<CreateSmStateRequest>,
) -> Result<Json<ApiResponse<SmState>>, (StatusCode, Json<ApiResponse<()>>)> {
    let conn = db_conn(&state)?;
    let sm_state = storage::insert_state(&conn, &config_id, &request).map_err(|e| {
        error!("State Machine CRUD: create_state error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;
    info!(
        "State Machine CRUD: Created state '{}' in config {}",
        sm_state.name, config_id
    );
    Ok(Json(ApiResponse::success(sm_state)))
}

/// DELETE /state-machine/configs/:config_id/states/:id — Delete a state
pub async fn delete_state(
    State(state): State<Arc<ApiState>>,
    Path((_config_id, id)): Path<(String, String)>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let conn = db_conn(&state)?;
    let deleted = storage::delete_state(&conn, &id).map_err(|e| {
        error!("State Machine CRUD: delete_state error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;
    if deleted {
        Ok(Json(ApiResponse::success(
            serde_json::json!({"deleted": true}),
        )))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("State not found: {}", id))),
        ))
    }
}

/// POST /state-machine/configs/:config_id/transitions — Create a transition within a config
pub async fn create_transition(
    State(state): State<Arc<ApiState>>,
    Path(config_id): Path<String>,
    Json(request): Json<CreateSmTransitionRequest>,
) -> Result<Json<ApiResponse<SmTransition>>, (StatusCode, Json<ApiResponse<()>>)> {
    let conn = db_conn(&state)?;
    let transition = storage::insert_transition(&conn, &config_id, &request).map_err(|e| {
        error!("State Machine CRUD: create_transition error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;
    info!(
        "State Machine CRUD: Created transition '{}' in config {}",
        transition.name, config_id
    );
    Ok(Json(ApiResponse::success(transition)))
}

/// DELETE /state-machine/configs/:config_id/transitions/:id — Delete a transition
pub async fn delete_transition(
    State(state): State<Arc<ApiState>>,
    Path((_config_id, id)): Path<(String, String)>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let conn = db_conn(&state)?;
    let deleted = storage::delete_transition(&conn, &id).map_err(|e| {
        error!("State Machine CRUD: delete_transition error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;
    if deleted {
        Ok(Json(ApiResponse::success(
            serde_json::json!({"deleted": true}),
        )))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Transition not found: {}", id))),
        ))
    }
}

/// POST /state-machine/configs/import — Import a complete config with states and transitions
pub async fn import_config(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SmImportRequest>,
) -> Result<Json<ApiResponse<SmConfigFull>>, (StatusCode, Json<ApiResponse<()>>)> {
    let conn = db_conn(&state)?;
    let result = storage::import_config(&conn, &request).map_err(|e| {
        error!("State Machine CRUD: import_config error: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
    })?;
    info!(
        "State Machine CRUD: Imported config '{}' ({} states, {} transitions)",
        result.config.name,
        result.states.len(),
        result.transitions.len()
    );
    Ok(Json(ApiResponse::success(result)))
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{delete, get, post};

    axum::Router::new()
        // Runtime state machine operations (Python bridge)
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
        // CRUD operations for state machine configs (SQLite)
        .route("/state-machine/configs", get(list_configs))
        .route("/state-machine/configs", post(create_config))
        .route("/state-machine/configs/import", post(import_config))
        .route("/state-machine/configs/:id", get(get_config))
        .route("/state-machine/configs/:id", delete(delete_config))
        .route(
            "/state-machine/configs/:config_id/states",
            post(create_state),
        )
        .route(
            "/state-machine/configs/:config_id/states/:id",
            delete(delete_state),
        )
        .route(
            "/state-machine/configs/:config_id/transitions",
            post(create_transition),
        )
        .route(
            "/state-machine/configs/:config_id/transitions/:id",
            delete(delete_transition),
        )
}
