//! Action execution handlers for MCP API
//!
//! Provides handlers for executing individual GUI actions.

use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use super::types::{api_error, ApiResponse, ApiState, ExecuteActionRequest, ExecuteActionResult};
use crate::executor::with_default_bridge;

/// Execute a single action (e.g., click on an image)
///
/// This endpoint allows executing individual GUI actions without running a full workflow.
/// It sends the action to the Python executor and waits for completion.
pub async fn execute_action(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecuteActionRequest>,
) -> Result<Json<ApiResponse<ExecuteActionResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Executing action: {} on image {} (timeout: {}s)",
        request.action_type, request.image_id, request.timeout_seconds
    );

    let app_state = state.app_state.clone();
    let action_type = request.action_type.clone();
    let image_id = request.image_id.clone();
    let monitor_index = request.monitor_index;
    let timeout_duration = Duration::from_secs(request.timeout_seconds);

    // Build action parameters
    let action_params = serde_json::json!({
        "action_type": action_type.to_uppercase(),
        "image_id": image_id,
        "monitor_index": monitor_index.unwrap_or(0)
    });

    // Send command and wait for response using spawn_blocking
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Use send_command_and_wait to get synchronous response
            match bridge.send_command_and_wait(
                "execute_action",
                Some(action_params),
                timeout_duration,
            ) {
                Ok(response) => {
                    if response.success {
                        Ok(ExecuteActionResult {
                            success: true,
                            action_type: request.action_type,
                            image_id: request.image_id,
                            error: None,
                        })
                    } else {
                        Ok(ExecuteActionResult {
                            success: false,
                            action_type: request.action_type,
                            image_id: request.image_id,
                            error: response.error,
                        })
                    }
                }
                Err(e) => Err(format!("Failed to execute action: {}", e)),
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
        Ok(action_result) => {
            if action_result.success {
                info!(
                    "MCP API: Action {} on image {} succeeded",
                    action_result.action_type, action_result.image_id
                );
            } else {
                warn!(
                    "MCP API: Action {} on image {} failed: {:?}",
                    action_result.action_type, action_result.image_id, action_result.error
                );
            }
            Ok(Json(ApiResponse::success(action_result)))
        }
        Err(e) => {
            error!("MCP API: Failed to execute action: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}
