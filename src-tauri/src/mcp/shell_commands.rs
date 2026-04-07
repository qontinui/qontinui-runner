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
use std::process::Stdio;
use std::time::Instant;
use tokio::time::{timeout, Duration};

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
    let rows = state
        .app_state
        .pg_db
        .list_shell_commands_filtered(
            query.enabled_only.unwrap_or(false),
            query.category.as_deref(),
        )
        .await
        .map_err(|e| {
            error!("Failed to list shell commands: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list shell commands: {}", e))),
            )
        })?;

    let commands: Vec<ShellCommand> = rows
        .iter()
        .map(|row| {
            let tags: Vec<String> = row["tags"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            ShellCommand {
                id: row["id"].as_str().unwrap_or("").to_string(),
                name: row["name"].as_str().unwrap_or("").to_string(),
                description: row["description"].as_str().map(|s| s.to_string()),
                command: row["command"].as_str().unwrap_or("").to_string(),
                working_directory: row["working_directory"].as_str().map(|s| s.to_string()),
                timeout_seconds: row["timeout_seconds"].as_i64().unwrap_or(0) as i32,
                fail_on_error: row["fail_on_error"].as_bool().unwrap_or(true),
                category: row["category"]
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "general".to_string()),
                tags,
                enabled: row["enabled"].as_bool().unwrap_or(true),
                created_at: row["created_at"].as_str().unwrap_or("").to_string(),
                updated_at: row["updated_at"].as_str().unwrap_or("").to_string(),
            }
        })
        .collect();

    Ok(Json(ApiResponse::success(commands)))
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

    // Fetch current values so unspecified fields retain their current values
    let current = match state.app_state.pg_db.get_shell_command(&id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Shell command not found: {}", id))),
            ));
        }
        Err(e) => {
            error!("Failed to fetch shell command for update: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to fetch shell command: {}", e))),
            ));
        }
    };

    let name = input.name.unwrap_or(current.name);
    let description = input.description.or(current.description);
    let command = input.command.unwrap_or(current.command);
    let working_directory = input.working_directory.or(current.working_directory);
    let timeout_seconds = input.timeout_seconds.unwrap_or(current.timeout_seconds);
    let fail_on_error = input.fail_on_error.unwrap_or(current.fail_on_error);
    let category = input.category.unwrap_or(current.category);
    let tags = input.tags.unwrap_or(current.tags);
    let enabled = input.enabled.unwrap_or(current.enabled);

    let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string());

    match state
        .app_state
        .pg_db
        .update_shell_command_full(
            &id,
            &name,
            description.as_deref(),
            &command,
            working_directory.as_deref(),
            timeout_seconds,
            fail_on_error,
            Some(category.as_str()),
            &tags_json,
            enabled,
        )
        .await
    {
        Ok(true) => match state.app_state.pg_db.get_shell_command(&id).await {
            Ok(Some(updated)) => Ok(Json(ApiResponse::success(updated))),
            Ok(None) => Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!(
                    "Shell command disappeared after update: {}",
                    id
                ))),
            )),
            Err(e) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to re-fetch updated shell command: {}",
                    e
                ))),
            )),
        },
        Ok(false) => Err((
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

                    let result_id = match state
                        .app_state
                        .pg_db
                        .save_shell_command_result(
                            &id,
                            status,
                            exit_code_i32,
                            Some(cr.stdout.as_str()),
                            Some(cr.stderr.as_str()),
                            Some(duration_ms),
                            Some(started_at.as_str()),
                            Some(completed_at.as_str()),
                            task_run_id.as_deref(),
                        )
                        .await
                    {
                        Ok(rid) => rid,
                        Err(e) => {
                            warn!("Failed to save shell command result: {}", e);
                            uuid::Uuid::new_v4().to_string()
                        }
                    };

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
    debug!("HTTP: Executing shell command '{}' on host", shell_cmd.name);

    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = crate::process_helpers::tokio_cmd_no_window();
        c.args(["/C", &shell_cmd.command]);
        c
    } else {
        let mut c = crate::process_helpers::tokio_no_window("sh");
        c.args(["-c", &shell_cmd.command]);
        c
    };

    if let Some(ref wd) = shell_cmd.working_directory {
        cmd.current_dir(wd);
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let start = Instant::now();
    let timeout_duration = Duration::from_secs(shell_cmd.timeout_seconds as u64);
    let started_at = chrono::Utc::now().to_rfc3339();

    let output_result = timeout(timeout_duration, cmd.output()).await;
    let duration_ms = start.elapsed().as_millis() as i64;

    let (success, exit_code, stdout, stderr) = match output_result {
        Ok(Ok(output)) => {
            let exit_code = output.status.code();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            (output.status.success(), exit_code, stdout, stderr)
        }
        Ok(Err(e)) => {
            error!(
                "HTTP: Failed to execute shell command '{}': {}",
                shell_cmd.name, e
            );
            (
                false,
                None,
                String::new(),
                format!("Failed to execute command: {}", e),
            )
        }
        Err(_) => {
            error!(
                "HTTP: Shell command '{}' timed out after {}s",
                shell_cmd.name, shell_cmd.timeout_seconds
            );
            (
                false,
                None,
                String::new(),
                format!(
                    "Command timed out after {} seconds",
                    shell_cmd.timeout_seconds
                ),
            )
        }
    };

    let status = if success { "success" } else { "failed" };
    let completed_at = chrono::Utc::now().to_rfc3339();

    let result_id = match state
        .app_state
        .pg_db
        .save_shell_command_result(
            &id,
            status,
            exit_code,
            Some(stdout.as_str()),
            Some(stderr.as_str()),
            Some(duration_ms),
            Some(started_at.as_str()),
            Some(completed_at.as_str()),
            task_run_id.as_deref(),
        )
        .await
    {
        Ok(rid) => rid,
        Err(e) => {
            warn!("HTTP: Failed to save shell command result: {}", e);
            uuid::Uuid::new_v4().to_string()
        }
    };

    let cmd_result = ShellCommandResult {
        id: result_id,
        shell_command_id: id.clone(),
        task_run_id: task_run_id.clone(),
        status: status.to_string(),
        exit_code,
        stdout: Some(stdout),
        stderr: Some(stderr),
        duration_ms: Some(duration_ms),
        started_at: Some(started_at),
        completed_at: Some(completed_at),
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    info!(
        "HTTP: Shell command '{}' executed (host): status={}, duration={}ms",
        shell_cmd.name, cmd_result.status, duration_ms
    );

    Ok(Json(ApiResponse::success(cmd_result)))
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
