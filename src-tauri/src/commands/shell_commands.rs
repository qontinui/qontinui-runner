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
use tracing::{error, info};
use uuid::Uuid;

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

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    // Ensure tables exist
    ensure_shell_commands_tables(&conn)?;

    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let tags_json = serde_json::to_string(&input.tags.unwrap_or_default())
        .map_err(|e| format!("Failed to serialize tags: {}", e))?;

    let timeout_seconds = input
        .timeout_seconds
        .unwrap_or_else(default_timeout_seconds);
    let fail_on_error = input.fail_on_error.unwrap_or_else(default_fail_on_error);

    conn.execute(
        r#"
        INSERT INTO shell_commands (
            id, name, description, command, working_directory,
            timeout_seconds, fail_on_error, category, tags,
            enabled, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        "#,
        params![
            id,
            input.name,
            input.description,
            input.command,
            input.working_directory,
            timeout_seconds,
            fail_on_error,
            input.category,
            tags_json,
            true, // enabled by default
            now,
            now,
        ],
    )
    .map_err(|e| format!("Failed to create shell command: {}", e))?;

    info!("Created shell command: {} ({})", input.name, id);

    // Fetch the created command
    drop(conn);
    get_shell_command(id, state).await
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

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    // Ensure tables exist
    ensure_shell_commands_tables(&conn)?;

    let result = conn.query_row(
        r#"
        SELECT id, name, description, command, working_directory,
               timeout_seconds, fail_on_error, category, tags,
               enabled, created_at, updated_at
        FROM shell_commands
        WHERE id = ?1
        "#,
        params![id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,         // id
                row.get::<_, String>(1)?,         // name
                row.get::<_, Option<String>>(2)?, // description
                row.get::<_, String>(3)?,         // command
                row.get::<_, Option<String>>(4)?, // working_directory
                row.get::<_, i32>(5)?,            // timeout_seconds
                row.get::<_, bool>(6)?,           // fail_on_error
                row.get::<_, Option<String>>(7)?, // category
                row.get::<_, String>(8)?,         // tags
                row.get::<_, bool>(9)?,           // enabled
                row.get::<_, String>(10)?,        // created_at
                row.get::<_, String>(11)?,        // updated_at
            ))
        },
    );

    match result {
        Ok((
            id,
            name,
            description,
            command,
            working_directory,
            timeout_seconds,
            fail_on_error,
            category,
            tags_str,
            enabled,
            created_at,
            updated_at,
        )) => {
            let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();

            let shell_command = ShellCommand {
                id,
                name,
                description,
                command,
                working_directory,
                timeout_seconds,
                fail_on_error,
                category,
                tags,
                enabled,
                created_at,
                updated_at,
            };

            Ok(CommandResponse {
                success: true,
                message: Some("Shell command found".to_string()),
                data: Some(
                    serde_json::to_value(&shell_command)
                        .map_err(|e| format!("Failed to serialize shell command: {}", e))?,
                ),
            })
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Shell command not found: {}", id)),
            data: None,
        }),
        Err(e) => Err(format!("Failed to query shell command: {}", e)),
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

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    // Ensure tables exist
    ensure_shell_commands_tables(&conn)?;

    // Build query based on filters
    let mut query = String::from(
        r#"
        SELECT id, name, description, command, working_directory,
               timeout_seconds, fail_on_error, category, tags,
               enabled, created_at, updated_at
        FROM shell_commands
        WHERE 1=1
        "#,
    );

    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if enabled_only.unwrap_or(false) {
        query.push_str(" AND enabled = 1");
    }

    if let Some(ref cat) = category {
        query.push_str(" AND category = ?");
        params_vec.push(Box::new(cat.clone()));
    }

    query.push_str(" ORDER BY name ASC");

    let mut stmt = conn
        .prepare(&query)
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

    let commands_iter = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,         // id
                row.get::<_, String>(1)?,         // name
                row.get::<_, Option<String>>(2)?, // description
                row.get::<_, String>(3)?,         // command
                row.get::<_, Option<String>>(4)?, // working_directory
                row.get::<_, i32>(5)?,            // timeout_seconds
                row.get::<_, bool>(6)?,           // fail_on_error
                row.get::<_, Option<String>>(7)?, // category
                row.get::<_, String>(8)?,         // tags
                row.get::<_, bool>(9)?,           // enabled
                row.get::<_, String>(10)?,        // created_at
                row.get::<_, String>(11)?,        // updated_at
            ))
        })
        .map_err(|e| format!("Failed to query shell commands: {}", e))?;

    let mut commands: Vec<ShellCommand> = Vec::new();

    for cmd_result in commands_iter {
        let (
            id,
            name,
            description,
            command,
            working_directory,
            timeout_seconds,
            fail_on_error,
            category,
            tags_str,
            enabled,
            created_at,
            updated_at,
        ) = cmd_result.map_err(|e| format!("Failed to read shell command row: {}", e))?;

        let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();

        commands.push(ShellCommand {
            id,
            name,
            description,
            command,
            working_directory,
            timeout_seconds,
            fail_on_error,
            category,
            tags,
            enabled,
            created_at,
            updated_at,
        });
    }

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

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    // Ensure tables exist
    ensure_shell_commands_tables(&conn)?;

    let now = Utc::now().to_rfc3339();

    // Get current values
    let current = conn.query_row(
        r#"
        SELECT name, description, command, working_directory,
               timeout_seconds, fail_on_error, category, tags, enabled
        FROM shell_commands
        WHERE id = ?1
        "#,
        params![id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,         // name
                row.get::<_, Option<String>>(1)?, // description
                row.get::<_, String>(2)?,         // command
                row.get::<_, Option<String>>(3)?, // working_directory
                row.get::<_, i32>(4)?,            // timeout_seconds
                row.get::<_, bool>(5)?,           // fail_on_error
                row.get::<_, Option<String>>(6)?, // category
                row.get::<_, String>(7)?,         // tags
                row.get::<_, bool>(8)?,           // enabled
            ))
        },
    );

    let (
        current_name,
        current_description,
        current_command,
        current_working_directory,
        current_timeout_seconds,
        current_fail_on_error,
        current_category,
        current_tags_str,
        current_enabled,
    ) = match current {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Shell command not found: {}", id)),
                data: None,
            });
        }
        Err(e) => return Err(format!("Failed to query shell command: {}", e)),
    };

    // Apply updates
    let name = input.name.unwrap_or(current_name);
    let description = input.description.or(current_description);
    let command = input.command.unwrap_or(current_command);
    let working_directory = input.working_directory.or(current_working_directory);
    let timeout_seconds = input.timeout_seconds.unwrap_or(current_timeout_seconds);
    let fail_on_error = input.fail_on_error.unwrap_or(current_fail_on_error);
    let category = input.category.or(current_category);
    let tags_str = match input.tags {
        Some(tags) => {
            serde_json::to_string(&tags).map_err(|e| format!("Failed to serialize tags: {}", e))?
        }
        None => current_tags_str,
    };
    let enabled = input.enabled.unwrap_or(current_enabled);

    conn.execute(
        r#"
        UPDATE shell_commands SET
            name = ?1,
            description = ?2,
            command = ?3,
            working_directory = ?4,
            timeout_seconds = ?5,
            fail_on_error = ?6,
            category = ?7,
            tags = ?8,
            enabled = ?9,
            updated_at = ?10
        WHERE id = ?11
        "#,
        params![
            name,
            description,
            command,
            working_directory,
            timeout_seconds,
            fail_on_error,
            category,
            tags_str,
            enabled,
            now,
            id,
        ],
    )
    .map_err(|e| format!("Failed to update shell command: {}", e))?;

    info!("Updated shell command: {}", id);

    drop(conn);
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

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    // Ensure tables exist
    ensure_shell_commands_tables(&conn)?;

    let affected = conn
        .execute("DELETE FROM shell_commands WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete shell command: {}", e))?;

    if affected == 0 {
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

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    // Ensure tables exist
    ensure_shell_commands_tables(&conn)?;

    // Get the shell command
    let result = conn.query_row(
        r#"
        SELECT id, name, command, working_directory, timeout_seconds, fail_on_error, enabled
        FROM shell_commands
        WHERE id = ?1
        "#,
        params![id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,         // id
                row.get::<_, String>(1)?,         // name
                row.get::<_, String>(2)?,         // command
                row.get::<_, Option<String>>(3)?, // working_directory
                row.get::<_, i32>(4)?,            // timeout_seconds
                row.get::<_, bool>(5)?,           // fail_on_error
                row.get::<_, bool>(6)?,           // enabled
            ))
        },
    );

    let (cmd_id, name, command, working_directory, timeout_seconds, fail_on_error, enabled) =
        match result {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Ok(CommandResponse {
                    success: false,
                    message: Some(format!("Shell command not found: {}", id)),
                    data: None,
                });
            }
            Err(e) => return Err(format!("Failed to query shell command: {}", e)),
        };

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

    // Build the command
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = crate::process_helpers::tokio_cmd_no_window();
        c.args(["/C", &command]);
        c
    } else {
        let mut c = crate::process_helpers::tokio_no_window("sh");
        c.args(["-c", &command]);
        c
    };

    // Set working directory if specified
    if let Some(ref wd) = working_directory {
        cmd.current_dir(wd);
    }

    // Capture stdout and stderr
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Execute with timeout
    let start = Instant::now();
    let timeout_duration = Duration::from_secs(timeout_seconds as u64);

    let output_result = timeout(timeout_duration, cmd.output()).await;

    let duration_ms = start.elapsed().as_millis() as u64;

    // Process the result
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

    // Store the result in the database
    let result_id = Uuid::new_v4().to_string();
    let executed_at = Utc::now().to_rfc3339();

    if let Err(e) = conn.execute(
        r#"
        INSERT INTO shell_command_results (
            id, shell_command_id, task_run_id, success, exit_code,
            stdout, stderr, duration_ms, executed_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        "#,
        params![
            result_id,
            cmd_id,
            task_run_id,
            success,
            exit_code,
            stdout,
            stderr,
            duration_ms as i64,
            executed_at,
        ],
    ) {
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

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    // Ensure tables exist
    ensure_shell_commands_tables(&conn)?;

    let limit = limit.unwrap_or(10);

    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, shell_command_id, task_run_id, success, exit_code,
                   stdout, stderr, duration_ms, executed_at
            FROM shell_command_results
            WHERE shell_command_id = ?1
            ORDER BY executed_at DESC
            LIMIT ?2
            "#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let results_iter = stmt
        .query_map(params![shell_command_id, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,         // id
                row.get::<_, String>(1)?,         // shell_command_id
                row.get::<_, Option<String>>(2)?, // task_run_id
                row.get::<_, bool>(3)?,           // success
                row.get::<_, Option<i32>>(4)?,    // exit_code
                row.get::<_, String>(5)?,         // stdout
                row.get::<_, String>(6)?,         // stderr
                row.get::<_, i64>(7)?,            // duration_ms
                row.get::<_, String>(8)?,         // executed_at
            ))
        })
        .map_err(|e| format!("Failed to query shell command results: {}", e))?;

    let mut results: Vec<ShellCommandResult> = Vec::new();

    for result in results_iter {
        let (
            id,
            shell_command_id,
            task_run_id,
            success,
            exit_code,
            stdout,
            stderr,
            duration_ms,
            executed_at,
        ) = result.map_err(|e| format!("Failed to read result row: {}", e))?;

        results.push(ShellCommandResult {
            id,
            shell_command_id,
            task_run_id,
            success,
            exit_code,
            stdout,
            stderr,
            duration_ms,
            executed_at,
        });
    }

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} results", results.len())),
        data: Some(
            serde_json::to_value(&results)
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

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    // Ensure tables exist
    ensure_shell_commands_tables(&conn)?;

    let mut stmt = conn
        .prepare(
            r#"
            SELECT DISTINCT category
            FROM shell_commands
            WHERE category IS NOT NULL AND category != ''
            ORDER BY category ASC
            "#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let categories_iter = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("Failed to query categories: {}", e))?;

    let mut categories: Vec<String> = Vec::new();

    for cat_result in categories_iter {
        categories.push(cat_result.map_err(|e| format!("Failed to read category: {}", e))?);
    }

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

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    // Ensure tables exist
    ensure_shell_commands_tables(&conn)?;

    let now = Utc::now().to_rfc3339();

    let affected = conn
        .execute(
            "UPDATE shell_commands SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
            params![enabled, now, id],
        )
        .map_err(|e| format!("Failed to update shell command: {}", e))?;

    if affected == 0 {
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
