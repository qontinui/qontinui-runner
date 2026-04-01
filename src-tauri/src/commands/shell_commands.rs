//! Shell Commands Module
//!
//! Provides Tauri commands for managing and executing custom shell commands.
//! Shell commands allow users to define reusable command-line operations
//! that can be executed during automation workflows or manually.
//!
//! # Features
//! - CRUD operations for shell commands
//! - Execution with configurable timeout and working directory
//! - Category and tag-based organization
//! - Integration with task runs for audit logging

use crate::commands::{AppState, CommandResponse};
use crate::executor::with_default_bridge;
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tauri::State;
use tokio::time::{timeout, Duration};
use tracing::{debug, error, info, warn};

// ============================================================================
// Request/Response Types
// ============================================================================

/// Input for creating a new shell command
#[derive(Debug, Deserialize)]
pub struct CreateShellCommandInput {
    pub name: String,
    pub description: Option<String>,
    pub command: String,
    pub working_directory: Option<String>,
    pub timeout_seconds: Option<i32>,
    pub fail_on_error: Option<bool>,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
}

/// Input for updating an existing shell command
#[derive(Debug, Deserialize)]
pub struct UpdateShellCommandInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub command: Option<String>,
    pub working_directory: Option<String>,
    pub timeout_seconds: Option<i32>,
    pub fail_on_error: Option<bool>,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

/// Response from executing a shell command
#[derive(Debug, Serialize)]
pub struct ExecuteShellCommandResponse {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

/// A shell command stored in the database
#[derive(Debug, Serialize, Deserialize)]
pub struct ShellCommand {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    pub timeout_seconds: i32,
    pub fail_on_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Result of a shell command execution stored in the database
#[derive(Debug, Serialize, Deserialize)]
pub struct ShellCommandResult {
    pub id: String,
    pub shell_command_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: i64,
    pub executed_at: String,
}

// ============================================================================
// Conversion Helpers
// ============================================================================

/// Convert a PG ShellCommand to the local ShellCommand type.
fn pg_shell_command_to_local(pg: &crate::database::types::ShellCommand) -> ShellCommand {
    ShellCommand {
        id: pg.id.clone(),
        name: pg.name.clone(),
        description: pg.description.clone(),
        command: pg.command.clone(),
        working_directory: pg.working_directory.clone(),
        timeout_seconds: pg.timeout_seconds,
        fail_on_error: pg.fail_on_error,
        category: Some(pg.category.clone()),
        tags: pg.tags.clone(),
        enabled: pg.enabled,
        created_at: pg.created_at.clone(),
        updated_at: pg.updated_at.clone(),
    }
}

// ============================================================================
// Default Values
// ============================================================================

/// Default timeout for shell commands.
/// Returns 0 to indicate no timeout (run until completion).
fn default_timeout_seconds() -> i32 {
    0 // No timeout - run until completion
}

fn default_fail_on_error() -> bool {
    true
}

// ============================================================================
// Database Operations
// ============================================================================

/// Ensures the shell_commands and shell_command_results tables exist
fn ensure_shell_commands_tables(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS shell_commands (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            command TEXT NOT NULL,
            working_directory TEXT,
            timeout_seconds INTEGER,  -- NULL = no timeout (default)
            fail_on_error BOOLEAN NOT NULL DEFAULT 1,
            category TEXT,
            tags TEXT DEFAULT '[]',
            enabled BOOLEAN NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_shell_commands_category ON shell_commands(category);
        CREATE INDEX IF NOT EXISTS idx_shell_commands_enabled ON shell_commands(enabled);

        CREATE TABLE IF NOT EXISTS shell_command_results (
            id TEXT PRIMARY KEY,
            shell_command_id TEXT NOT NULL,
            task_run_id TEXT,
            success BOOLEAN NOT NULL,
            exit_code INTEGER,
            stdout TEXT NOT NULL DEFAULT '',
            stderr TEXT NOT NULL DEFAULT '',
            duration_ms INTEGER NOT NULL,
            executed_at TEXT NOT NULL,
            FOREIGN KEY (shell_command_id) REFERENCES shell_commands(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_shell_command_results_shell_command_id ON shell_command_results(shell_command_id);
        CREATE INDEX IF NOT EXISTS idx_shell_command_results_task_run_id ON shell_command_results(task_run_id);
        CREATE INDEX IF NOT EXISTS idx_shell_command_results_executed_at ON shell_command_results(executed_at);
        "#,
    )
    .map_err(|e| format!("Failed to create shell_commands tables: {}", e))?;

    Ok(())
}

// ============================================================================
// CRUD Commands
// ============================================================================

/// Create a new shell command.
///
/// # Arguments
/// * `input` - CreateShellCommandInput with command details
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with created ShellCommand
/// * `Err(String)` - Error message if creation fails
#[tauri::command]
pub async fn create_shell_command(
    input: CreateShellCommandInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Creating shell command: {}", input.name);

    let timeout_seconds = input
        .timeout_seconds
        .unwrap_or_else(default_timeout_seconds);
    let fail_on_error = input.fail_on_error.unwrap_or_else(default_fail_on_error);

    let pg_input = crate::database::types::CreateShellCommandInput {
        name: input.name.clone(),
        description: input.description,
        command: input.command,
        working_directory: input.working_directory,
        timeout_seconds,
        fail_on_error,
        category: input.category.unwrap_or_else(|| "general".to_string()),
        tags: input.tags.unwrap_or_default(),
        enabled: true,
    };

    let pg_cmd = state.pg_db.create_shell_command(&pg_input).await?;

    info!("Created shell command: {} ({})", pg_cmd.name, pg_cmd.id);

    let shell_command = pg_shell_command_to_local(&pg_cmd);

    Ok(CommandResponse {
        success: true,
        message: Some("Shell command found".to_string()),
        data: Some(
            serde_json::to_value(&shell_command)
                .map_err(|e| format!("Failed to serialize shell command: {}", e))?,
        ),
    })
}

/// Get a single shell command by ID.
///
/// # Arguments
/// * `id` - Shell command ID
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with ShellCommand or null if not found
/// * `Err(String)` - Error message if lookup fails
#[tauri::command]
pub async fn get_shell_command(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Getting shell command: {}", id);

    match state.pg_db.get_shell_command(&id).await? {
        Some(pg_cmd) => {
            let shell_command = pg_shell_command_to_local(&pg_cmd);

            Ok(CommandResponse {
                success: true,
                message: Some("Shell command found".to_string()),
                data: Some(
                    serde_json::to_value(&shell_command)
                        .map_err(|e| format!("Failed to serialize shell command: {}", e))?,
                ),
            })
        }
        None => Ok(CommandResponse {
            success: false,
            message: Some(format!("Shell command not found: {}", id)),
            data: None,
        }),
    }
}

/// List all shell commands.
///
/// # Arguments
/// * `enabled_only` - If true, only return enabled commands
/// * `category` - Optional category filter
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with list of ShellCommand
/// * `Err(String)` - Error message if listing fails
#[tauri::command]
pub async fn list_shell_commands(
    enabled_only: Option<bool>,
    category: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Listing shell commands (enabled_only: {:?}, category: {:?})",
        enabled_only, category
    );

    let pg_cmds = state
        .pg_db
        .list_shell_commands_filtered(enabled_only.unwrap_or(false), category.as_deref())
        .await?;

    let commands: Vec<ShellCommand> = pg_cmds
        .iter()
        .map(|row| {
            let tags: Vec<String> = row["tags"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            ShellCommand {
                id: row["id"].as_str().unwrap_or("").to_string(),
                name: row["name"].as_str().unwrap_or("").to_string(),
                description: row["description"].as_str().map(|s| s.to_string()),
                command: row["command"].as_str().unwrap_or("").to_string(),
                working_directory: row["working_directory"].as_str().map(|s| s.to_string()),
                timeout_seconds: row["timeout_seconds"].as_i64().unwrap_or(0) as i32,
                fail_on_error: row["fail_on_error"].as_bool().unwrap_or(true),
                category: row["category"].as_str().map(|s| s.to_string()),
                tags,
                enabled: row["enabled"].as_bool().unwrap_or(true),
                created_at: row["created_at"].as_str().unwrap_or("").to_string(),
                updated_at: row["updated_at"].as_str().unwrap_or("").to_string(),
            }
        })
        .collect();

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} shell commands", commands.len())),
        data: Some(
            serde_json::to_value(&commands)
                .map_err(|e| format!("Failed to serialize shell commands: {}", e))?,
        ),
    })
}

/// Update an existing shell command.
///
/// # Arguments
/// * `id` - Shell command ID to update
/// * `input` - UpdateShellCommandInput with fields to update
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with updated ShellCommand
/// * `Err(String)` - Error message if update fails
#[tauri::command]
pub async fn update_shell_command(
    id: String,
    input: UpdateShellCommandInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Updating shell command: {}", id);

    // Get current values from PG
    let current = match state.pg_db.get_shell_command(&id).await? {
        Some(v) => v,
        None => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Shell command not found: {}", id)),
                data: None,
            });
        }
    };

    // Apply updates
    let name = input.name.unwrap_or_else(|| current.name.clone());
    let description = input.description.or_else(|| current.description.clone());
    let command = input.command.unwrap_or_else(|| current.command.clone());
    let working_directory = input.working_directory.or_else(|| current.working_directory.clone());
    let timeout_seconds = input.timeout_seconds.unwrap_or(current.timeout_seconds);
    let fail_on_error = input.fail_on_error.unwrap_or(current.fail_on_error);
    let category = input.category.or_else(|| Some(current.category.clone()));
    let tags_str = match input.tags {
        Some(tags) => {
            serde_json::to_string(&tags).map_err(|e| format!("Failed to serialize tags: {}", e))?
        }
        None => serde_json::to_string(&current.tags).unwrap_or_else(|_| "[]".to_string()),
    };
    let enabled = input.enabled.unwrap_or(current.enabled);

    state
        .pg_db
        .update_shell_command_full(
            &id,
            &name,
            description.as_deref(),
            &command,
            working_directory.as_deref(),
            timeout_seconds,
            fail_on_error,
            category.as_deref(),
            &tags_str,
            enabled,
        )
        .await?;

    info!("Updated shell command: {}", id);

    get_shell_command(id, state).await
}

/// Delete a shell command.
///
/// # Arguments
/// * `id` - Shell command ID to delete
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if deletion fails
#[tauri::command]
pub async fn delete_shell_command(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Deleting shell command: {}", id);

    let deleted = state.pg_db.delete_shell_command(&id).await?;

    if !deleted {
        return Ok(CommandResponse {
            success: false,
            message: Some(format!("Shell command not found: {}", id)),
            data: None,
        });
    }

    info!("Deleted shell command: {}", id);

    Ok(CommandResponse {
        success: true,
        message: Some("Shell command deleted".to_string()),
        data: None,
    })
}

// ============================================================================
// Execution Command
// ============================================================================

/// Execute a shell command by ID.
///
/// # Arguments
/// * `id` - Shell command ID to execute
/// * `task_run_id` - Optional task run ID for audit logging
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with ExecuteShellCommandResponse
/// * `Err(String)` - Error message if execution fails
#[tauri::command]
pub async fn execute_shell_command(
    id: String,
    task_run_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Executing shell command: {}", id);

    // Get the shell command from PG
    let pg_cmd = match state.pg_db.get_shell_command(&id).await? {
        Some(cmd) => cmd,
        None => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Shell command not found: {}", id)),
                data: None,
            });
        }
    };

    let cmd_id = pg_cmd.id.clone();
    let name = pg_cmd.name.clone();
    let command = pg_cmd.command.clone();
    let working_directory = pg_cmd.working_directory.clone();
    let timeout_seconds = pg_cmd.timeout_seconds;
    let fail_on_error = pg_cmd.fail_on_error;
    let enabled = pg_cmd.enabled;

    // Check if enabled
    if !enabled {
        return Ok(CommandResponse {
            success: false,
            message: Some(format!("Shell command '{}' is disabled", name)),
            data: None,
        });
    }

    info!(
        "Executing command '{}': {} (timeout: {}s, working_dir: {:?})",
        name, command, timeout_seconds, working_directory
    );

    // ── Container isolation path (with security policy) ────────────
    // Resolve security policy and enforce command restrictions
    let security_settings = crate::settings::get_security_settings();
    let security_policy = crate::security::PolicyEngine::resolve(None, &security_settings);

    if let Err(denial) = crate::security::PolicyEngine::evaluate_command(&security_policy, &command) {
        warn!("Shell command '{}' blocked by security policy: {}", name, denial);
        return Err(format!("Security policy violation: {}", denial.reason));
    }

    let container_result = {
        let executor_guard = state.container_executor.lock().await;
        if let Some(ref executor) = *executor_guard {
            debug!("Container executor present, attempting container execution for '{}'", name);
            match executor
                .try_execute_with_policy(&command, working_directory.as_deref(), Some(&security_policy))
                .await
            {
                Ok(Some(cr)) => {
                    info!(
                        "Shell command '{}' executed in container {} (exit_code={:?}, duration={}ms)",
                        name, cr.container_id, cr.exit_code, cr.duration_ms
                    );
                    let exit_code_i32 = cr.exit_code.map(|c| c as i32);
                    Some((
                        cr.exit_code == Some(0),
                        exit_code_i32,
                        cr.stdout,
                        cr.stderr,
                        cr.duration_ms,
                    ))
                }
                Ok(None) => {
                    debug!(
                        "Container isolation not available for '{}', falling back to host execution",
                        name
                    );
                    None
                }
                Err(e) => {
                    warn!(
                        "Container execution error for '{}': {}, falling back to host execution",
                        name, e
                    );
                    None
                }
            }
        } else {
            None
        }
    };

    let (success, exit_code, stdout, stderr, duration_ms) = if let Some(cr) = container_result {
        cr
    } else {
        // ── Host execution path ───────────────────────────────────────
        debug!("Executing command '{}' on host", name);

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = crate::process_helpers::tokio_cmd_no_window();
            c.args(["/C", &command]);
            c
        } else {
            let mut c = crate::process_helpers::tokio_no_window("sh");
            c.args(["-c", &command]);
            c
        };

        if let Some(ref wd) = working_directory {
            cmd.current_dir(wd);
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let start = Instant::now();
        let timeout_duration = Duration::from_secs(timeout_seconds as u64);

        let output_result = timeout(timeout_duration, cmd.output()).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        let (success, exit_code, stdout, stderr) = match output_result {
            Ok(Ok(output)) => {
                let exit_code = output.status.code();
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let success = output.status.success();
                (success, exit_code, stdout, stderr)
            }
            Ok(Err(e)) => {
                error!("Failed to execute command '{}': {}", name, e);
                (
                    false,
                    None,
                    String::new(),
                    format!("Failed to execute command: {}", e),
                )
            }
            Err(_) => {
                error!("Command '{}' timed out after {}s", name, timeout_seconds);
                (
                    false,
                    None,
                    String::new(),
                    format!("Command timed out after {} seconds", timeout_seconds),
                )
            }
        };

        (success, exit_code, stdout, stderr, duration_ms)
    };

    // Store the result in PG
    let status_str = if success { "success" } else { "failed" };
    let started_at_str = Utc::now().to_rfc3339();

    if let Err(e) = state
        .pg_db
        .save_shell_command_result(
            &cmd_id,
            status_str,
            exit_code,
            Some(&stdout),
            Some(&stderr),
            Some(duration_ms as i64),
            Some(&started_at_str),
            Some(&started_at_str),
            task_run_id.as_deref(),
        )
        .await
    {
        error!("Failed to store shell command result: {}", e);
    }

    let response = ExecuteShellCommandResponse {
        success,
        exit_code,
        stdout: stdout.clone(),
        stderr: stderr.clone(),
        duration_ms,
    };

    info!(
        "Shell command '{}' completed: success={}, exit_code={:?}, duration={}ms",
        name, success, exit_code, duration_ms
    );

    // Determine overall success based on fail_on_error setting
    let overall_success = if fail_on_error { success } else { true };

    Ok(CommandResponse {
        success: overall_success,
        message: Some(format!(
            "Shell command '{}' executed: {}",
            name,
            if success { "success" } else { "failed" }
        )),
        data: Some(
            serde_json::to_value(&response)
                .map_err(|e| format!("Failed to serialize response: {}", e))?,
        ),
    })
}

// ============================================================================
// Additional Utility Commands
// ============================================================================

/// Get execution results for a shell command.
///
/// # Arguments
/// * `shell_command_id` - Shell command ID
/// * `limit` - Maximum number of results to return (default: 10)
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with list of ShellCommandResult
/// * `Err(String)` - Error message if query fails
#[tauri::command]
pub async fn get_shell_command_results(
    shell_command_id: String,
    limit: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Getting results for shell command: {}", shell_command_id);

    let limit = limit.unwrap_or(10);
    let pg_results = state
        .pg_db
        .get_shell_command_results(&shell_command_id, limit)
        .await?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} results", pg_results.len())),
        data: Some(
            serde_json::to_value(&pg_results)
                .map_err(|e| format!("Failed to serialize results: {}", e))?,
        ),
    })
}

/// Get all unique categories for shell commands.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with list of category strings
/// * `Err(String)` - Error message if query fails
#[tauri::command]
pub async fn get_shell_command_categories(
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Getting shell command categories");

    let categories = state.pg_db.get_shell_command_categories().await?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} categories", categories.len())),
        data: Some(
            serde_json::to_value(&categories)
                .map_err(|e| format!("Failed to serialize categories: {}", e))?,
        ),
    })
}

/// Set the enabled status of a shell command.
///
/// # Arguments
/// * `id` - Shell command ID
/// * `enabled` - Whether the command should be enabled
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if update fails
#[tauri::command]
pub async fn set_shell_command_enabled(
    id: String,
    enabled: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Setting shell command {} enabled={}", id, enabled);

    let updated = state.pg_db.set_shell_command_enabled(&id, enabled).await?;

    if !updated {
        return Ok(CommandResponse {
            success: false,
            message: Some(format!("Shell command not found: {}", id)),
            data: None,
        });
    }

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Shell command {} {}",
            id,
            if enabled { "enabled" } else { "disabled" }
        )),
        data: None,
    })
}

// ============================================================================
// AI Shell Command Generation
// ============================================================================

/// Input for generating a shell command with AI
#[derive(Debug, Deserialize)]
pub struct GenerateShellCommandInput {
    pub user_prompt: String,
    pub target_os: Option<String>,
    pub category: Option<String>,
}

/// Response from AI shell command generation
#[derive(Debug, Serialize)]
pub struct GenerateShellCommandResponse {
    pub command: String,
    pub description: String,
}

/// Generate a shell command using AI based on natural language description.
///
/// # Arguments
/// * `input` - The generation input containing user prompt and target OS
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with generated command
/// * `Err(String)` - Error message if generation fails
#[tauri::command]
pub async fn generate_shell_command_with_ai(
    input: GenerateShellCommandInput,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    use crate::settings;

    info!(
        "Generating shell command with AI: {}",
        input.user_prompt.chars().take(50).collect::<String>()
    );

    // Get AI settings
    let ai_settings = settings::get_ai_settings();

    // Map provider to Python format
    let ai_provider = match ai_settings.provider {
        settings::AiProvider::ClaudeCli => "claude_cli",
        settings::AiProvider::ClaudeApi => "claude_api",
        settings::AiProvider::GeminiCli => "gemini_cli",
        settings::AiProvider::GeminiApi => "gemini_api",
    };

    // Build provider-specific settings
    let provider_settings = match ai_settings.provider {
        settings::AiProvider::ClaudeCli => {
            let execution_mode = match ai_settings.claude_cli.execution_mode {
                settings::CliExecutionMode::Auto => "auto",
                settings::CliExecutionMode::WindowsNative => "native",
                settings::CliExecutionMode::Wsl => "wsl",
                settings::CliExecutionMode::Native => "native",
            };
            let effective_config_dir =
                crate::ai_provider::get_effective_config_dir(&ai_settings.claude_cli);
            serde_json::json!({
                "execution_mode": execution_mode,
                "custom_path": ai_settings.claude_cli.custom_path,
                "timeout_seconds": ai_settings.claude_cli.timeout_seconds,
                "config_dir": effective_config_dir,
            })
        }
        settings::AiProvider::ClaudeApi => {
            serde_json::json!({
                "model": ai_settings.claude_api.model,
                "max_tokens": ai_settings.claude_api.max_tokens,
            })
        }
        settings::AiProvider::GeminiCli => {
            let execution_mode = match ai_settings.gemini_cli.execution_mode {
                settings::CliExecutionMode::Auto => "auto",
                settings::CliExecutionMode::WindowsNative => "native",
                settings::CliExecutionMode::Wsl => "wsl",
                settings::CliExecutionMode::Native => "native",
            };
            let auth_method = match ai_settings.gemini_cli.auth_method {
                settings::GeminiAuthMethod::OAuth => "oauth",
                settings::GeminiAuthMethod::ApiKey => "api_key",
            };
            serde_json::json!({
                "execution_mode": execution_mode,
                "custom_path": ai_settings.gemini_cli.custom_path,
                "timeout_seconds": ai_settings.gemini_cli.timeout_seconds,
                "auth_method": auth_method,
                "model": ai_settings.gemini_cli.model,
            })
        }
        settings::AiProvider::GeminiApi => {
            serde_json::json!({
                "model": ai_settings.gemini_api.model,
                "max_output_tokens": ai_settings.gemini_api.max_output_tokens,
                "temperature": ai_settings.gemini_api.temperature,
            })
        }
    };

    // Clone state for use in spawn_blocking
    let app_state = state.inner().clone();

    let target_os = input.target_os.unwrap_or_else(|| "windows".to_string());
    let category = input.category.unwrap_or_else(|| "general".to_string());
    let user_prompt = input.user_prompt;

    // Execute via spawn_blocking since with_default_bridge uses block_on internally
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            let params = serde_json::json!({
                "user_prompt": user_prompt,
                "target_os": target_os,
                "category": category,
                "ai_provider": ai_provider,
                "ai_settings": provider_settings,
            });

            // Use longer timeout for AI generation
            match bridge.send_command_and_wait(
                "generate_shell_command_with_ai",
                Some(params),
                std::time::Duration::from_secs(120),
            ) {
                Ok(response) => {
                    if response.success {
                        CommandResponse {
                            success: true,
                            message: Some("Shell command generated successfully".to_string()),
                            data: response.data,
                        }
                    } else {
                        CommandResponse {
                            success: false,
                            message: Some(
                                response
                                    .error
                                    .unwrap_or_else(|| "Generation failed".to_string()),
                            ),
                            data: None,
                        }
                    }
                }
                Err(e) => CommandResponse {
                    success: false,
                    message: Some(format!("Command failed: {}", e)),
                    data: None,
                },
            }
        })
        .unwrap_or_else(|e| CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        })
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    Ok(result)
}
