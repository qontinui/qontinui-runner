//! State machine navigation handlers for MCP API
//!
//! Provides handlers for state-based navigation using pathfinding.

use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use super::types::{api_error, ApiResponse, ApiState, GoToStateRequest, GoToStateResult};
use crate::executor::with_default_bridge;

/// Navigate to a target state using pathfinding
///
/// This endpoint uses the state machine to find and execute the path
/// from the current state to the target state.
pub async fn go_to_state(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GoToStateRequest>,
) -> Result<Json<ApiResponse<GoToStateResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Navigating to state: {} (timeout: {}s)",
        request.state_id, request.timeout_seconds
    );

    let app_state = state.app_state.clone();
    let state_id = request.state_id.clone();
    let monitor_index = request.monitor_index;
    let timeout_duration = Duration::from_secs(request.timeout_seconds);

    // Build navigation parameters
    let nav_params = serde_json::json!({
        "state_id": state_id,
        "monitor_index": monitor_index.unwrap_or(0)
    });

    // Send command and wait for response using spawn_blocking
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Use send_command_and_wait to get synchronous response
            match bridge.send_command_and_wait("go_to_state", Some(nav_params), timeout_duration) {
                Ok(response) => {
                    if response.success {
                        Ok(GoToStateResult {
                            success: true,
                            state_id: request.state_id,
                            error: None,
                        })
                    } else {
                        Ok(GoToStateResult {
                            success: false,
                            state_id: request.state_id,
                            error: response.error,
                        })
                    }
                }
                Err(e) => Err(format!("Failed to navigate to state: {}", e)),
            }
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
        Ok(nav_result) => {
            if nav_result.success {
                info!(
                    "MCP API: Successfully navigated to state {}",
                    nav_result.state_id
                );
            } else {
                warn!(
                    "MCP API: Failed to navigate to state {}: {:?}",
                    nav_result.state_id, nav_result.error
                );
            }
            Ok(Json(ApiResponse::success(nav_result)))
        }
        Err(e) => {
            error!("MCP API: Failed to navigate to state: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}
