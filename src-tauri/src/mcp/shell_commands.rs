//! Shell command CRUD handlers for MCP API
//!
//! Provides HTTP handlers for managing shell commands:
//! list, get, create, update, delete, run.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::database::{CreateShellCommandInput, ShellCommand, UpdateShellCommandInput};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Query Types
// ============================================================================

/// Query parameters for listing shell commands
#[derive(Debug, Deserialize)]
pub struct ListShellCommandsQuery {
    #[serde(default)]
    pub enabled_only: Option<bool>,
    #[serde(default)]
    pub category: Option<String>,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all shell commands
pub async fn list_shell_commands_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListShellCommandsQuery>,
) -> Result<Json<ApiResponse<Vec<ShellCommand>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let enabled_only = query.enabled_only.unwrap_or(false);
    match state
        .app_state
        .checkpoint_db
        .list_shell_commands(enabled_only, query.category.as_deref())
    {
        Ok(commands) => Ok(Json(ApiResponse::success(commands))),
        Err(e) => {
            error!("Failed to list shell commands: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list shell commands: {}", e))),
            ))
        }
    }
}

/// Get a single shell command by ID
pub async fn get_shell_command_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<ShellCommand>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.app_state.checkpoint_db.get_shell_command(&id) {
        Ok(Some(command)) => Ok(Json(ApiResponse::success(command))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Shell command not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to get shell command: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get shell command: {}", e))),
            ))
        }
    }
}

/// Create a new shell command
pub async fn create_shell_command_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateShellCommandInput>,
) -> Result<Json<ApiResponse<ShellCommand>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Creating shell command: {}", input.name);
    match state.app_state.checkpoint_db.create_shell_command(&input) {
        Ok(command) => {
            info!("Created shell command: {} ({})", command.name, command.id);
            Ok(Json(ApiResponse::success(command)))
        }
        Err(e) => {
            error!("Failed to create shell command: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to create shell command: {}", e))),
            ))
        }
    }
}

/// Update an existing shell command
pub async fn update_shell_command_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<UpdateShellCommandInput>,
) -> Result<Json<ApiResponse<ShellCommand>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Updating shell command: {}", id);
    match state
        .app_state
        .checkpoint_db
        .update_shell_command(&id, &input)
    {
        Ok(command) => {
            info!("Updated shell command: {} ({})", command.name, command.id);
            Ok(Json(ApiResponse::success(command)))
        }
        Err(e) if e.contains("not found") => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Shell command not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to update shell command: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update shell command: {}", e))),
            ))
        }
    }
}

/// Delete a shell command by ID
pub async fn delete_shell_command_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Deleting shell command: {}", id);
    match state.app_state.checkpoint_db.delete_shell_command(&id) {
        Ok(true) => Ok(Json(ApiResponse::success(serde_json::json!({
            "deleted": true,
            "id": id
        })))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Shell command not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to delete shell command: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to delete shell command: {}", e))),
            ))
        }
    }
}

// ============================================================================
// Run Handler
// ============================================================================

/// Request body for running a shell command
#[derive(Debug, Deserialize)]
pub struct RunShellCommandRequest {
    /// Optional task run ID for audit logging
    #[serde(default)]
    pub task_run_id: Option<String>,
}

/// Run a shell command by ID.
///
/// Reads the shell command from the database and executes it synchronously,
/// storing the result for audit logging.
pub async fn run_shell_command_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<RunShellCommandRequest>,
) -> Result<
    Json<ApiResponse<crate::database::ShellCommandResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("HTTP: Running shell command: {}", id);

    let db = state.app_state.checkpoint_db.clone();
    let task_run_id = request.task_run_id.clone();

    // Execute in spawn_blocking since this does synchronous I/O and process execution
    let result =
        tokio::task::spawn_blocking(move || db.execute_shell_command(&id, task_run_id.as_deref()))
            .await
            .map_err(|e| {
                error!("HTTP: spawn_blocking error for run shell command: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Internal error: {}", e))),
                )
            })?;

    match result {
        Ok(cmd_result) => {
            info!(
                "HTTP: Shell command executed: status={}, duration={}ms",
                cmd_result.status,
                cmd_result.duration_ms.unwrap_or(0)
            );
            Ok(Json(ApiResponse::success(cmd_result)))
        }
        Err(e) => {
            error!("HTTP: Failed to run shell command: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to run shell command: {}", e))),
            ))
        }
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/shell-commands",
            get(list_shell_commands_handler).post(create_shell_command_handler),
        )
        .route(
            "/shell-commands/:id",
            get(get_shell_command_handler)
                .put(update_shell_command_handler)
                .delete(delete_shell_command_handler),
        )
        .route("/shell-commands/:id/run", post(run_shell_command_handler))
}
