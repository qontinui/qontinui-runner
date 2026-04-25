//! Auto-continue settings handlers for MCP API
//!
//! Manages the auto-continue AI workflow setting at both global
//! and per-active-workflow levels, plus supervisor availability checks.

use axum::response::Json;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::mcp::types::ApiResponse;
use crate::settings;

/// Response for auto-continue setting
#[derive(Debug, Serialize)]
pub struct AutoContinueSettingResponse {
    enabled: bool,
}

/// Request body for setting auto-continue
#[derive(Debug, Deserialize)]
pub struct SetAutoContinueRequest {
    enabled: bool,
}

/// Response for per-workflow auto-continue setting
#[derive(Debug, Serialize)]
pub struct WorkflowAutoContinueResponse {
    enabled: bool,
    workflow_name: Option<String>,
}

/// Get the auto-continue AI workflow setting
pub async fn get_auto_continue_setting() -> Json<ApiResponse<AutoContinueSettingResponse>> {
    let enabled = settings::get_auto_continue_ai_workflow();
    Json(ApiResponse::success(AutoContinueSettingResponse {
        enabled,
    }))
}

/// Set the auto-continue AI workflow setting
pub async fn set_auto_continue_setting(
    Json(body): Json<SetAutoContinueRequest>,
) -> Json<ApiResponse<AutoContinueSettingResponse>> {
    match settings::save_auto_continue_ai_workflow(body.enabled) {
        Ok(_) => {
            info!(
                "Auto-continue AI workflow setting updated to: {}",
                body.enabled
            );
            Json(ApiResponse::success(AutoContinueSettingResponse {
                enabled: body.enabled,
            }))
        }
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Failed to save setting: {}", e)),
            error_detail: None,
            hint: None,
        }),
    }
}

/// Get the auto-continue setting for the active workflow.
/// Uses global setting and checks for running tasks in database.
pub async fn get_workflow_auto_continue() -> Json<ApiResponse<WorkflowAutoContinueResponse>> {
    let enabled = settings::get_auto_continue_ai_workflow();

    // Check if there are any running tasks (PG)
    let workflow_name = if let Some(pg) = crate::database::pg::PgDb::try_global() {
        pg.get_running_task_runs(None)
            .await
            .ok()
            .and_then(|tasks| tasks.first().map(|t| t.task_name.clone()))
    } else {
        None
    };

    Json(ApiResponse::success(WorkflowAutoContinueResponse {
        enabled,
        workflow_name,
    }))
}

/// Set the auto-continue setting for the active workflow.
/// Updates the global setting.
pub async fn set_workflow_auto_continue(
    Json(body): Json<SetAutoContinueRequest>,
) -> Json<ApiResponse<WorkflowAutoContinueResponse>> {
    // Update the global setting
    match settings::save_auto_continue_ai_workflow(body.enabled) {
        Ok(_) => {
            info!("Auto-continue setting updated to: {}", body.enabled);

            // Get the active workflow name if any (PG)
            let workflow_name = if let Some(pg) = crate::database::pg::PgDb::try_global() {
                pg.get_running_task_runs(None)
                    .await
                    .ok()
                    .and_then(|tasks| tasks.first().map(|t| t.task_name.clone()))
            } else {
                None
            };

            Json(ApiResponse::success(WorkflowAutoContinueResponse {
                enabled: body.enabled,
                workflow_name,
            }))
        }
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Failed to update auto-continue setting: {}", e)),
            error_detail: None,
            hint: None,
        }),
    }
}

/// Check if the supervisor is available on its configured port (default 9875).
/// Used to determine what restart instructions to give AI sessions.
pub fn check_supervisor_available() -> bool {
    use std::net::TcpStream;
    use std::time::Duration;

    // Try to connect to supervisor health endpoint
    let addr = crate::api_config::get_supervisor_socket_addr();
    let socket_addr = match addr.parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    TcpStream::connect_timeout(&socket_addr, Duration::from_millis(500)).is_ok()
}

/// Create routes for auto-continue settings.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/workflow/auto-continue",
            get(get_auto_continue_setting).post(set_auto_continue_setting),
        )
        .route(
            "/workflow/active/auto-continue",
            get(get_workflow_auto_continue).post(set_workflow_auto_continue),
        )
}
