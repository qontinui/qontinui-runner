//! Integration testing handlers for MCP API
//!
//! Provides HTTP handlers for the integration testing framework:
//! test runs, mock actions, state/transition queries, path finding, assertions.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Types
// ============================================================================

/// Request to start an integration test run
#[derive(Debug, Deserialize)]
pub struct StartIntegrationTestRequest {
    /// Name of the test run
    pub name: String,
    /// Configuration path being tested (optional)
    #[serde(default)]
    pub config_path: Option<String>,
    /// Test cases to execute
    #[serde(default)]
    pub test_cases: Vec<IntegrationTestCase>,
    /// Additional metadata
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// A test case for integration testing
#[derive(Debug, Deserialize, Serialize)]
pub struct IntegrationTestCase {
    /// Test ID
    #[serde(default)]
    pub test_id: Option<String>,
    /// Test name
    pub name: String,
    /// Test description
    #[serde(default)]
    pub description: Option<String>,
    /// Assertions to run
    #[serde(default)]
    pub assertions: Vec<IntegrationTestAssertion>,
    /// Setup actions
    #[serde(default)]
    pub setup_actions: Vec<serde_json::Value>,
    /// Teardown actions
    #[serde(default)]
    pub teardown_actions: Vec<serde_json::Value>,
}

/// An assertion for integration testing
#[derive(Debug, Deserialize, Serialize)]
pub struct IntegrationTestAssertion {
    /// Assertion type: state_reached, element_found, action_performed, etc.
    #[serde(rename = "type")]
    pub assertion_type: String,
    /// Target to verify
    pub target: String,
    /// Expected value (optional)
    #[serde(default)]
    pub expected: Option<serde_json::Value>,
    /// Timeout in seconds
    #[serde(default = "default_assertion_timeout")]
    pub timeout_seconds: f64,
}

fn default_assertion_timeout() -> f64 {
    30.0
}

/// Request to mock a GUI action
#[derive(Debug, Deserialize)]
pub struct MockGuiActionRequest {
    /// Action type: click, type, screenshot
    pub action_type: String,
    /// X coordinate (for click)
    #[serde(default)]
    pub x: Option<i32>,
    /// Y coordinate (for click)
    #[serde(default)]
    pub y: Option<i32>,
    /// Mouse button (for click)
    #[serde(default)]
    pub button: Option<String>,
    /// Click count (for click)
    #[serde(default)]
    pub clicks: Option<i32>,
    /// Text to type (for type action)
    #[serde(default)]
    pub text: Option<String>,
    /// Delay between keystrokes in ms (for type action)
    #[serde(default)]
    pub delay_ms: Option<i32>,
    /// Monitor index (for screenshot)
    #[serde(default)]
    pub monitor_index: Option<i32>,
}

/// Request to find path between states
#[derive(Debug, Deserialize)]
pub struct FindPathRequest {
    /// Source state name or ID
    pub from_state: String,
    /// Target state name or ID
    pub to_state: String,
}

/// Request to traverse to a state
#[derive(Debug, Deserialize)]
pub struct TraverseToStateRequest {
    /// Target state name or ID
    pub target_state: String,
    /// Whether to execute the traversal (false for dry run)
    #[serde(default = "default_execute")]
    pub execute: bool,
}

fn default_execute() -> bool {
    true
}

/// Request to set mock mode
#[derive(Debug, Deserialize)]
pub struct SetMockModeRequest {
    /// Mode: disabled, record, playback
    pub mode: String,
}

/// Request to run an assertion
#[derive(Debug, Deserialize)]
pub struct RunAssertionRequest {
    /// Assertion type
    pub assertion_type: String,
    /// Target to verify
    pub target: String,
    /// Expected value
    #[serde(default)]
    pub expected: Option<serde_json::Value>,
    /// Timeout in seconds
    #[serde(default = "default_assertion_timeout")]
    pub timeout_seconds: f64,
}

// ============================================================================
// Handlers
// ============================================================================

/// Start an integration test run
pub async fn start_integration_test(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartIntegrationTestRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Starting integration test run: {}", request.name);

    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "name": request.name,
        "config_path": request.config_path,
        "metadata": request.metadata,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("testing_start_run", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: Integration test run started successfully");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to start test run".to_string());
                error!("MCP API: Failed to start test run: {}", error_msg);
                Err((StatusCode::BAD_REQUEST, Json(api_error(error_msg))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to start integration test: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get test run status
pub async fn get_test_run_status(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "run_id": run_id,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_get_status", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "status": "unknown"
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get test results
pub async fn get_integration_test_results(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "run_id": run_id,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("testing_get_results", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "results": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// List test runs
pub async fn list_integration_test_runs(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();
    let limit: i32 = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);

    let params = serde_json::json!({
        "limit": limit,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_list_runs", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "runs": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Mock a GUI action
pub async fn mock_gui_action(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<MockGuiActionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let (command, params) = match request.action_type.as_str() {
        "click" => {
            let params = serde_json::json!({
                "x": request.x.unwrap_or(0),
                "y": request.y.unwrap_or(0),
                "button": request.button.unwrap_or_else(|| "left".to_string()),
                "clicks": request.clicks.unwrap_or(1),
            });
            ("testing_mock_click", params)
        }
        "type" => {
            let params = serde_json::json!({
                "text": request.text.unwrap_or_default(),
                "delay_ms": request.delay_ms.unwrap_or(50),
            });
            ("testing_mock_type", params)
        }
        "screenshot" => {
            let params = serde_json::json!({
                "monitor_index": request.monitor_index,
            });
            ("testing_mock_screenshot", params)
        }
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Unknown action type: {}",
                    request.action_type
                ))),
            ));
        }
    };

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait(command, Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": true,
                    "mocked": true
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get all states for testing
pub async fn get_testing_states(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_get_states", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "states": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get all transitions for testing
pub async fn get_testing_transitions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_get_transitions", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "transitions": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Find path between states
pub async fn find_testing_path(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<FindPathRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "from_state": request.from_state,
        "to_state": request.to_state,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("testing_find_path", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": "No path found"
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Traverse to a state
pub async fn traverse_to_state(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<TraverseToStateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "target_state": request.target_state,
        "execute": request.execute,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(120);
            bridge.send_command_and_wait(
                "testing_traverse_to_state",
                Some(params),
                timeout_duration,
            )
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get active states
pub async fn get_testing_active_states(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_get_active_states", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "active_states": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Set mock mode
pub async fn set_testing_mock_mode(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SetMockModeRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "mode": request.mode,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_set_mock_mode", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": true
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get mocked actions
pub async fn get_mocked_actions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_get_mocked_actions", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "actions": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Clear mocked actions
pub async fn clear_mocked_actions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_clear_mocked_actions", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": true
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Run an assertion
pub async fn run_testing_assertion(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunAssertionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "assertion_type": request.assertion_type,
        "target": request.target,
        "expected": request.expected,
        "timeout_seconds": request.timeout_seconds,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(60);
            bridge.send_command_and_wait("testing_run_assertion", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// End an integration test run
pub async fn end_integration_test(
    State(state): State<Arc<ApiState>>,
    Path(_run_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("testing_end_run", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": true
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/testing/start", post(start_integration_test))
        .route("/testing/status/{id}", get(get_test_run_status))
        .route("/testing/results/{id}", get(get_integration_test_results))
        .route("/testing/runs", get(list_integration_test_runs))
        .route("/testing/mock-action", post(mock_gui_action))
        .route("/testing/states", get(get_testing_states))
        .route("/testing/transitions", get(get_testing_transitions))
        .route("/testing/find-path", post(find_testing_path))
        .route("/testing/traverse", post(traverse_to_state))
        .route("/testing/active-states", get(get_testing_active_states))
        .route("/testing/mock-mode", post(set_testing_mock_mode))
        .route("/testing/mocked-actions", get(get_mocked_actions))
        .route("/testing/clear-mocked-actions", post(clear_mocked_actions))
        .route("/testing/assertion", post(run_testing_assertion))
        .route("/testing/end/{id}", post(end_integration_test))
}
