//! State Machine handlers for MCP API
//!
//! Provides HTTP handlers for UI Bridge State Machine operations:
//! loading state machine configs, querying status, executing transitions,
//! navigating between states via the Python bridge, and CRUD operations
//! for state machine configs, states, and transitions stored in PostgreSQL.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::state_machine_configs::{
    CreateSmConfigRequest, CreateSmStateRequest, CreateSmTransitionRequest, SmConfig, SmConfigFull,
    SmImportRequest, SmState, SmTransition,
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
            // Fall through to PG-backed lookup which returns all defined transitions.
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
                    let pg = app_state.pg_db.clone();
                    let db_transitions = async move {
                        let configs = pg.list_sm_configs().await.ok()?;
                        for config in configs.iter() {
                            if let Ok(Some(full)) = pg.get_sm_config_full(&config.id).await {
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
                    }
                    .await;

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

/// Query parameters for trigger introspection endpoints.
///
/// ``active_state_ids`` is a comma-separated list of state IDs. When absent
/// or empty, the current active state set is used.
#[derive(Debug, Deserialize, Default)]
pub struct TriggerIntrospectionQuery {
    #[serde(default)]
    pub active_state_ids: Option<String>,
}

impl TriggerIntrospectionQuery {
    fn as_id_list(&self) -> Vec<String> {
        match &self.active_state_ids {
            Some(s) if !s.is_empty() => s
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// GET /state-machine/permitted-triggers?active_state_ids=a,b,c
/// Return transitions currently permitted from the (hypothetical) active set.
pub async fn get_permitted_triggers(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TriggerIntrospectionQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let active_state_ids = query.as_id_list();
    info!(
        "State Machine API: Getting permitted triggers (active_state_ids={:?})",
        active_state_ids
    );

    let app_state = state.app_state.clone();
    let params = serde_json::json!({ "active_state_ids": active_state_ids });
    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("sm_get_permitted_triggers", Some(params), timeout)
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
                        .unwrap_or(serde_json::json!({"permitted_triggers": []})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get permitted triggers".to_string());
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

/// GET /state-machine/blocked-triggers?active_state_ids=a,b,c
/// Return transitions currently blocked, each with a structured reason.
pub async fn get_blocked_triggers(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TriggerIntrospectionQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let active_state_ids = query.as_id_list();
    info!(
        "State Machine API: Getting blocked triggers (active_state_ids={:?})",
        active_state_ids
    );

    let app_state = state.app_state.clone();
    let params = serde_json::json!({ "active_state_ids": active_state_ids });
    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("sm_get_blocked_triggers", Some(params), timeout)
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
                        .unwrap_or(serde_json::json!({"blocked_triggers": []})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get blocked triggers".to_string());
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

/// GET /state-machine/mermaid-diagram?active_state_ids=a,b,c
/// Return a Mermaid ``stateDiagram-v2`` source for the loaded state machine.
/// When ``active_state_ids`` is absent or empty, the runtime's current active
/// states are highlighted; otherwise the specified hypothetical set is used.
pub async fn get_mermaid_diagram(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TriggerIntrospectionQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let active_state_ids = query.as_id_list();
    info!(
        "State Machine API: Getting Mermaid diagram (active_state_ids={:?})",
        active_state_ids
    );

    let app_state = state.app_state.clone();
    let params = serde_json::json!({ "active_state_ids": active_state_ids });
    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("sm_get_mermaid_diagram", Some(params), timeout)
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
                        .unwrap_or(serde_json::json!({"diagram": ""})),
                )))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get mermaid diagram".to_string());
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
// CRUD Handlers — Config, State, Transition management via HTTP
// ============================================================================

/// Helper to get PG DB reference and map errors to HTTP responses.
fn pg_db(state: &Arc<ApiState>) -> &std::sync::Arc<crate::database::pg::PgDb> {
    &state.app_state.pg_db
}

fn pg_err(e: String) -> (StatusCode, Json<ApiResponse<()>>) {
    error!("State Machine CRUD: PG error: {}", e);
    (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
}

/// GET /state-machine/configs — List all state machine configs
pub async fn list_configs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<SmConfig>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let configs = pg_db(&state).list_sm_configs().await.map_err(pg_err)?;
    Ok(Json(ApiResponse::success(configs)))
}

/// POST /state-machine/configs — Create a new state machine config
pub async fn create_config(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateSmConfigRequest>,
) -> Result<Json<ApiResponse<SmConfig>>, (StatusCode, Json<ApiResponse<()>>)> {
    let config = pg_db(&state)
        .insert_sm_config(&request)
        .await
        .map_err(pg_err)?;
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
    let config = pg_db(&state)
        .get_sm_config_full(&id)
        .await
        .map_err(pg_err)?;
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
    let deleted = pg_db(&state).delete_sm_config(&id).await.map_err(pg_err)?;
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
    let sm_state = pg_db(&state)
        .insert_sm_state(&config_id, &request)
        .await
        .map_err(pg_err)?;
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
    let deleted = pg_db(&state).delete_sm_state(&id).await.map_err(pg_err)?;
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
    let transition = pg_db(&state)
        .insert_sm_transition(&config_id, &request)
        .await
        .map_err(pg_err)?;
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
    let deleted = pg_db(&state)
        .delete_sm_transition(&id)
        .await
        .map_err(pg_err)?;
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
    let result = pg_db(&state)
        .import_sm_config(&request)
        .await
        .map_err(pg_err)?;
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
        .route(
            "/state-machine/permitted-triggers",
            get(get_permitted_triggers),
        )
        .route(
            "/state-machine/blocked-triggers",
            get(get_blocked_triggers),
        )
        .route(
            "/state-machine/mermaid-diagram",
            get(get_mermaid_diagram),
        )
        .route("/state-machine", delete(clear_state_machine))
        // CRUD operations for state machine configs (SQLite)
        .route("/state-machine/configs", get(list_configs))
        .route("/state-machine/configs", post(create_config))
        .route("/state-machine/configs/import", post(import_config))
        .route("/state-machine/configs/{id}", get(get_config))
        .route("/state-machine/configs/{id}", delete(delete_config))
        .route(
            "/state-machine/configs/{config_id}/states",
            post(create_state),
        )
        .route(
            "/state-machine/configs/{config_id}/states/{id}",
            delete(delete_state),
        )
        .route(
            "/state-machine/configs/{config_id}/transitions",
            post(create_transition),
        )
        .route(
            "/state-machine/configs/{config_id}/transitions/{id}",
            delete(delete_transition),
        )
}
