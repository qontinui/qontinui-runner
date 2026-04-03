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
use tracing::{debug, error, info, warn};

use crate::database::{
    CreateShellCommandInput, ShellCommand, ShellCommandResult, UpdateShellCommandInput,
};
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
    // checkpoint_db removed — list not yet fully migrated to PG
    let _ = query;
    Ok(Json(ApiResponse::success(vec![])))
}

/// Get a single shell command by ID
pub async fn get_shell_command_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<ShellCommand>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.app_state.pg_db.get_shell_command(&id).await {
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
    match state.app_state.pg_db.create_shell_command(&input).await {
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
    // checkpoint_db removed — update not yet migrated to PG
    let _ = (id, input);
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(api_error(
            "Update shell command: SQLite removed, not yet migrated to PG".to_string(),
        )),
    ))
}

/// Delete a shell command by ID
pub async fn delete_shell_command_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Deleting shell command: {}", id);
    match state.app_state.pg_db.delete_shell_command(&id).await {
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
/// Reads the shell command from the database and executes it. When container
/// isolation is enabled and Docker is available, runs inside an isolated
/// container; otherwise falls back to host execution.
pub async fn run_shell_command_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<RunShellCommandRequest>,
) -> Result<Json<ApiResponse<ShellCommandResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("HTTP: Running shell command: {}", id);

    let task_run_id = request.task_run_id.clone();

    // Fetch the shell command metadata from PG
    let shell_cmd = state
        .app_state
        .pg_db
        .get_shell_command(&id)
        .await
        .map_err(|e| {
            error!("HTTP: error fetching shell command: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Internal error: {}", e))),
            )
        })?;

    let shell_cmd = match shell_cmd {
        Some(cmd) => cmd,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Shell command not found: {}", id))),
            ));
        }
    };

    if !shell_cmd.enabled {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "Shell command '{}' is disabled",
                shell_cmd.name
            ))),
        ));
    }

    // ── Container isolation path (with security policy) ────────────
    let security_settings = crate::settings::get_security_settings();
    let security_policy = crate::security::PolicyEngine::resolve(None, &security_settings);

    // Enforce command policy before execution
    if let Err(denial) =
        crate::security::PolicyEngine::evaluate_command(&security_policy, &shell_cmd.command)
    {
        tracing::warn!(
            "MCP shell command '{}' blocked by security policy: {}",
            shell_cmd.name,
            denial
        );
        return Err((
            StatusCode::FORBIDDEN,
            Json(api_error(format!(
                "Security policy violation: {}",
                denial.reason
            ))),
        ));
    }

    // Build extra container env vars from security settings
    let extra_env =
        crate::security::build_container_security_env(&security_policy, &security_settings);

    let container_result = {
        let executor_guard = state.app_state.container_executor.lock().await;
        if let Some(ref executor) = *executor_guard {
            debug!(
                "HTTP: Container executor present, attempting container execution for '{}'",
                shell_cmd.name
            );
            match executor
                .try_execute_with_policy(
                    &shell_cmd.command,
                    shell_cmd.working_directory.as_deref(),
                    Some(&security_policy),
                    &extra_env,
                )
                .await
            {
                Ok(Some(cr)) => {
                    info!(
                        "HTTP: Shell command '{}' executed in container {} (exit_code={:?}, duration={}ms)",
                        shell_cmd.name, cr.container_id, cr.exit_code, cr.duration_ms
                    );
                    let exit_code_i32 = cr.exit_code.map(|c| c as i32);
                    let status = if cr.exit_code == Some(0) {
                        "success"
                    } else {
                        "failed"
                    };
                    let started_at = chrono::Utc::now().to_rfc3339();
                    let completed_at = chrono::Utc::now().to_rfc3339();
                    let duration_ms = cr.duration_ms as i64;

                    // checkpoint_db removed — result saving not yet migrated to PG
                    let result_id = uuid::Uuid::new_v4().to_string();

                    Some(ShellCommandResult {
                        id: result_id,
                        shell_command_id: id.clone(),
                        task_run_id: task_run_id.clone(),
                        status: status.to_string(),
                        exit_code: exit_code_i32,
                        stdout: Some(cr.stdout),
                        stderr: Some(cr.stderr),
                        duration_ms: Some(duration_ms),
                        started_at: Some(started_at),
                        completed_at: Some(completed_at),
                        created_at: chrono::Utc::now().to_rfc3339(),
                    })
                }
                Ok(None) => {
                    debug!(
                        "HTTP: Container isolation not available for '{}', falling back to host",
                        shell_cmd.name
                    );
                    None
                }
                Err(e) => {
                    warn!(
                        "HTTP: Container execution error for '{}': {}, falling back to host",
                        shell_cmd.name, e
                    );
                    None
                }
            }
        } else {
            None
        }
    };

    if let Some(cmd_result) = container_result {
        info!(
            "HTTP: Shell command executed (container): status={}, duration={}ms",
            cmd_result.status,
            cmd_result.duration_ms.unwrap_or(0)
        );
        return Ok(Json(ApiResponse::success(cmd_result)));
    }

    // ── Host execution fallback ───────────────────────────────────────
    // checkpoint_db removed — host execution (execute_shell_command) not yet migrated to PG
    debug!(
        "HTTP: Executing shell command '{}' on host (not available, SQLite removed)",
        id
    );
    let _ = task_run_id;
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(api_error(
            "Shell command host execution: SQLite removed, not yet migrated to PG".to_string(),
        )),
    ))
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
            "/shell-commands/{id}",
            get(get_shell_command_handler)
                .put(update_shell_command_handler)
                .delete(delete_shell_command_handler),
        )
        .route("/shell-commands/{id}/run", post(run_shell_command_handler))
}
