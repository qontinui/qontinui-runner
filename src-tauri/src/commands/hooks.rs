//! Lifecycle Hooks commands
//!
//! This module handles lifecycle hook CRUD operations:
//! - List all hooks
//! - Create, update, delete hooks
//! - Enable/disable hooks
//! - Test hook execution
//! - Reorder hooks

use crate::commands::{AppState, CommandResponse};
use crate::orchestrator::hooks::{Hook, HookAction, HookCondition, HookContext, HookTrigger};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use tracing::{error, info};
use uuid::Uuid;

// ============================================================================
// Request/Response Types
// ============================================================================

/// Request to create a new hook
#[derive(Debug, Deserialize)]
pub struct CreateHookRequest {
    pub name: String,
    pub description: Option<String>,
    pub trigger: String,
    pub action_type: String,
    pub action_config: serde_json::Value,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub execution_order: i32,
    #[serde(default = "default_continue_on_failure")]
    pub continue_on_failure: bool,
    #[serde(default)]
    pub conditions: Vec<HookCondition>,
    pub task_run_id: Option<String>,
}

fn default_enabled() -> bool {
    true
}

fn default_continue_on_failure() -> bool {
    true
}

/// Request to update an existing hook
#[derive(Debug, Deserialize)]
pub struct UpdateHookRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub trigger: Option<String>,
    pub action_type: Option<String>,
    pub action_config: Option<serde_json::Value>,
    pub enabled: Option<bool>,
    pub execution_order: Option<i32>,
    pub continue_on_failure: Option<bool>,
    pub conditions: Option<Vec<HookCondition>>,
}

/// Request to reorder hooks
#[derive(Debug, Deserialize)]
pub struct ReorderHooksRequest {
    /// List of hook IDs in desired order
    pub hook_ids: Vec<String>,
}

/// Request to test a hook
#[derive(Debug, Deserialize)]
pub struct TestHookRequest {
    pub hook_id: String,
    /// Optional context for testing (uses defaults if not provided)
    pub context: Option<TestHookContext>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TestHookContext {
    pub task_run_id: Option<String>,
    pub task_name: Option<String>,
    pub iteration: Option<u32>,
    pub status: Option<String>,
    pub error: Option<String>,
}

/// Response for a single hook
#[derive(Debug, Serialize)]
pub struct HookResponse {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub trigger: String,
    pub action_type: String,
    pub action_config: serde_json::Value,
    pub enabled: bool,
    pub execution_order: i32,
    pub continue_on_failure: bool,
    pub conditions: Vec<HookConditionResponse>,
    pub task_run_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct HookConditionResponse {
    pub variable: String,
    pub operator: String,
    pub value: serde_json::Value,
}

/// Response for hook test execution
#[derive(Debug, Serialize)]
pub struct TestHookResponse {
    pub success: bool,
    pub output: Option<String>,
    pub error: Option<String>,
    pub duration_ms: u64,
}

// ============================================================================
// Conversion Functions
// ============================================================================

fn parse_trigger(trigger_str: &str) -> Result<HookTrigger, String> {
    HookTrigger::from_str(trigger_str).ok_or_else(|| format!("Invalid trigger: {}", trigger_str))
}

fn trigger_to_string(trigger: HookTrigger) -> String {
    match trigger {
        HookTrigger::PreExecution => "pre_execution",
        HookTrigger::PostExecution => "post_execution",
        HookTrigger::OnError => "on_error",
        HookTrigger::OnVerificationFail => "on_verification_fail",
        HookTrigger::OnComplete => "on_complete",
        HookTrigger::PreIteration => "pre_iteration",
        HookTrigger::PostIteration => "post_iteration",
    }
    .to_string()
}

fn parse_action(action_type: &str, config: serde_json::Value) -> Result<HookAction, String> {
    match action_type {
        "command" => {
            let command = config
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or("Command action requires 'command' field")?
                .to_string();
            let working_dir = config
                .get("working_dir")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let timeout_seconds = config
                .get("timeout_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(30);
            let env = config
                .get("env")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();

            Ok(HookAction::Command {
                command,
                working_dir,
                timeout_seconds,
                env,
            })
        }
        "webhook" => {
            let url = config
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or("Webhook action requires 'url' field")?
                .to_string();
            let method = config
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("POST")
                .to_string();
            let headers = config
                .get("headers")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let body = config
                .get("body")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let timeout_seconds = config
                .get("timeout_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(30);

            Ok(HookAction::Webhook {
                url,
                method,
                headers,
                body,
                timeout_seconds,
            })
        }
        "log" => {
            let level = config
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("info")
                .to_string();
            let message = config
                .get("message")
                .and_then(|v| v.as_str())
                .ok_or("Log action requires 'message' field")?
                .to_string();

            Ok(HookAction::Log { level, message })
        }
        "notification" => {
            let title = config
                .get("title")
                .and_then(|v| v.as_str())
                .ok_or("Notification action requires 'title' field")?
                .to_string();
            let body = config
                .get("body")
                .and_then(|v| v.as_str())
                .ok_or("Notification action requires 'body' field")?
                .to_string();

            Ok(HookAction::Notification { title, body })
        }
        _ => Err(format!("Unknown action type: {}", action_type)),
    }
}

fn action_to_type_string(action: &HookAction) -> String {
    match action {
        HookAction::Command { .. } => "command",
        HookAction::Webhook { .. } => "webhook",
        HookAction::Log { .. } => "log",
        HookAction::Notification { .. } => "notification",
    }
    .to_string()
}

fn action_to_config(action: &HookAction) -> serde_json::Value {
    match action {
        HookAction::Command {
            command,
            working_dir,
            timeout_seconds,
            env,
        } => {
            let mut config = serde_json::json!({
                "type": "command",
                "command": command,
                "timeout_seconds": timeout_seconds,
            });
            if let Some(wd) = working_dir {
                config["working_dir"] = serde_json::json!(wd);
            }
            if !env.is_empty() {
                config["env"] = serde_json::to_value(env).unwrap_or_default();
            }
            config
        }
        HookAction::Webhook {
            url,
            method,
            headers,
            body,
            timeout_seconds,
        } => {
            let mut config = serde_json::json!({
                "type": "webhook",
                "url": url,
                "method": method,
                "timeout_seconds": timeout_seconds,
            });
            if !headers.is_empty() {
                config["headers"] = serde_json::to_value(headers).unwrap_or_default();
            }
            if let Some(b) = body {
                config["body"] = serde_json::json!(b);
            }
            config
        }
        HookAction::Log { level, message } => {
            serde_json::json!({
                "type": "log",
                "level": level,
                "message": message,
            })
        }
        HookAction::Notification { title, body } => {
            serde_json::json!({
                "type": "notification",
                "title": title,
                "body": body,
            })
        }
    }
}

fn hook_to_response(
    id: String,
    name: String,
    description: Option<String>,
    trigger: HookTrigger,
    action: &HookAction,
    enabled: bool,
    execution_order: i32,
    continue_on_failure: bool,
    conditions: &[HookCondition],
    task_run_id: Option<String>,
    created_at: String,
    updated_at: String,
) -> HookResponse {
    HookResponse {
        id,
        name,
        description,
        trigger: trigger_to_string(trigger),
        action_type: action_to_type_string(action),
        action_config: action_to_config(action),
        enabled,
        execution_order,
        continue_on_failure,
        conditions: conditions
            .iter()
            .map(|c| HookConditionResponse {
                variable: c.variable.clone(),
                operator: c.operator.clone(),
                value: c.value.clone(),
            })
            .collect(),
        task_run_id,
        created_at,
        updated_at,
    }
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get all hooks.
///
/// Returns all global hooks from the database, sorted by execution order.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with list of HookResponse
/// * `Err(String)` - Error message if hooks cannot be loaded
#[tauri::command]
pub async fn get_all_hooks(state: State<'_, Arc<AppState>>) -> Result<CommandResponse, String> {
    info!("Getting all hooks");

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, name, description, trigger, action_type, action_config,
                   enabled, execution_order, continue_on_failure, conditions,
                   task_run_id, created_at, updated_at
            FROM task_hooks
            WHERE task_run_id IS NULL
            ORDER BY execution_order ASC, created_at ASC
            "#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let hooks_iter = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,          // id
                row.get::<_, String>(1)?,          // name
                row.get::<_, Option<String>>(2)?,  // description
                row.get::<_, String>(3)?,          // trigger
                row.get::<_, String>(4)?,          // action_type
                row.get::<_, String>(5)?,          // action_config
                row.get::<_, bool>(6)?,            // enabled
                row.get::<_, i32>(7)?,             // execution_order
                row.get::<_, bool>(8)?,            // continue_on_failure
                row.get::<_, String>(9)?,          // conditions
                row.get::<_, Option<String>>(10)?, // task_run_id
                row.get::<_, String>(11)?,         // created_at
                row.get::<_, String>(12)?,         // updated_at
            ))
        })
        .map_err(|e| format!("Failed to query hooks: {}", e))?;

    let mut hooks: Vec<HookResponse> = Vec::new();

    for hook_result in hooks_iter {
        let (
            id,
            name,
            description,
            trigger_str,
            action_type,
            action_config_str,
            enabled,
            execution_order,
            continue_on_failure,
            conditions_str,
            task_run_id,
            created_at,
            updated_at,
        ) = hook_result.map_err(|e| format!("Failed to read hook row: {}", e))?;

        let trigger = match parse_trigger(&trigger_str) {
            Ok(t) => t,
            Err(e) => {
                error!("Invalid trigger for hook {}: {}", id, e);
                continue;
            }
        };

        let action_config: serde_json::Value = serde_json::from_str(&action_config_str)
            .map_err(|e| format!("Failed to parse action config: {}", e))?;

        let action = match parse_action(&action_type, action_config.clone()) {
            Ok(a) => a,
            Err(e) => {
                error!("Invalid action for hook {}: {}", id, e);
                continue;
            }
        };

        let conditions: Vec<HookCondition> =
            serde_json::from_str(&conditions_str).unwrap_or_default();

        hooks.push(hook_to_response(
            id,
            name,
            description,
            trigger,
            &action,
            enabled,
            execution_order,
            continue_on_failure,
            &conditions,
            task_run_id,
            created_at,
            updated_at,
        ));
    }

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Retrieved {} hooks", hooks.len())),
        data: Some(
            serde_json::to_value(&hooks)
                .map_err(|e| format!("Failed to serialize hooks: {}", e))?,
        ),
    })
}

/// Get a single hook by ID.
///
/// # Arguments
/// * `id` - Hook ID to find
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with HookResponse or null if not found
/// * `Err(String)` - Error message if lookup fails
#[tauri::command]
pub async fn get_hook(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Getting hook: {}", id);

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    let result = conn.query_row(
        r#"
        SELECT id, name, description, trigger, action_type, action_config,
               enabled, execution_order, continue_on_failure, conditions,
               task_run_id, created_at, updated_at
        FROM task_hooks
        WHERE id = ?1
        "#,
        params![id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, i32>(7)?,
                row.get::<_, bool>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, String>(12)?,
            ))
        },
    );

    match result {
        Ok((
            id,
            name,
            description,
            trigger_str,
            action_type,
            action_config_str,
            enabled,
            execution_order,
            continue_on_failure,
            conditions_str,
            task_run_id,
            created_at,
            updated_at,
        )) => {
            let trigger = parse_trigger(&trigger_str)?;
            let action_config: serde_json::Value = serde_json::from_str(&action_config_str)
                .map_err(|e| format!("Failed to parse action config: {}", e))?;
            let action = parse_action(&action_type, action_config)?;
            let conditions: Vec<HookCondition> =
                serde_json::from_str(&conditions_str).unwrap_or_default();

            let hook = hook_to_response(
                id,
                name,
                description,
                trigger,
                &action,
                enabled,
                execution_order,
                continue_on_failure,
                &conditions,
                task_run_id,
                created_at,
                updated_at,
            );

            Ok(CommandResponse {
                success: true,
                message: Some("Hook found".to_string()),
                data: Some(
                    serde_json::to_value(&hook)
                        .map_err(|e| format!("Failed to serialize hook: {}", e))?,
                ),
            })
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(CommandResponse {
            success: true,
            message: Some("Hook not found".to_string()),
            data: Some(serde_json::Value::Null),
        }),
        Err(e) => Err(format!("Failed to query hook: {}", e)),
    }
}

/// Create a new hook.
///
/// # Arguments
/// * `request` - CreateHookRequest with hook details
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with created HookResponse
/// * `Err(String)` - Error message if creation fails
#[tauri::command]
pub async fn create_hook(
    request: CreateHookRequest,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Creating hook: {}", request.name);

    // Validate trigger
    let _ = parse_trigger(&request.trigger)?;

    // Validate action
    let _ = parse_action(&request.action_type, request.action_config.clone())?;

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let action_config_str = serde_json::to_string(&request.action_config)
        .map_err(|e| format!("Failed to serialize action config: {}", e))?;

    let conditions_str = serde_json::to_string(&request.conditions)
        .map_err(|e| format!("Failed to serialize conditions: {}", e))?;

    conn.execute(
        r#"
        INSERT INTO task_hooks (
            id, name, description, trigger, action_type, action_config,
            enabled, execution_order, continue_on_failure, conditions,
            task_run_id, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        "#,
        params![
            id,
            request.name,
            request.description,
            request.trigger,
            request.action_type,
            action_config_str,
            request.enabled,
            request.execution_order,
            request.continue_on_failure,
            conditions_str,
            request.task_run_id,
            now,
            now,
        ],
    )
    .map_err(|e| format!("Failed to create hook: {}", e))?;

    drop(conn); // Release connection before calling get_hook

    // Fetch the created hook
    get_hook(id, state).await
}

/// Update an existing hook.
///
/// # Arguments
/// * `id` - Hook ID to update
/// * `request` - UpdateHookRequest with fields to update
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with updated HookResponse
/// * `Err(String)` - Error message if update fails
#[tauri::command]
pub async fn update_hook(
    id: String,
    request: UpdateHookRequest,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Updating hook: {}", id);

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    let now = chrono::Utc::now().to_rfc3339();

    // Get current hook values
    let current = conn.query_row(
        "SELECT name, description, trigger, action_type, action_config, enabled, execution_order, continue_on_failure, conditions FROM task_hooks WHERE id = ?1",
        params![id],
        |row| Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, bool>(5)?,
            row.get::<_, i32>(6)?,
            row.get::<_, bool>(7)?,
            row.get::<_, String>(8)?,
        ))
    );

    let (
        current_name,
        current_description,
        current_trigger,
        current_action_type,
        current_action_config,
        current_enabled,
        current_execution_order,
        current_continue_on_failure,
        current_conditions,
    ) = match current {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Ok(CommandResponse {
                success: false,
                message: Some("Hook not found".to_string()),
                data: None,
            });
        }
        Err(e) => return Err(format!("Failed to query hook: {}", e)),
    };

    // Apply updates
    let name = request.name.unwrap_or(current_name);
    let description = request.description.unwrap_or(current_description);
    let trigger = request.trigger.unwrap_or(current_trigger);
    let action_type = request.action_type.unwrap_or(current_action_type);
    let action_config_str = match request.action_config {
        Some(config) => serde_json::to_string(&config)
            .map_err(|e| format!("Failed to serialize action config: {}", e))?,
        None => current_action_config,
    };
    let enabled = request.enabled.unwrap_or(current_enabled);
    let execution_order = request.execution_order.unwrap_or(current_execution_order);
    let continue_on_failure = request
        .continue_on_failure
        .unwrap_or(current_continue_on_failure);
    let conditions_str = match request.conditions {
        Some(conditions) => serde_json::to_string(&conditions)
            .map_err(|e| format!("Failed to serialize conditions: {}", e))?,
        None => current_conditions,
    };

    // Validate trigger
    let _ = parse_trigger(&trigger)?;

    // Validate action
    let action_config: serde_json::Value = serde_json::from_str(&action_config_str)
        .map_err(|e| format!("Failed to parse action config: {}", e))?;
    let _ = parse_action(&action_type, action_config)?;

    conn.execute(
        r#"
        UPDATE task_hooks SET
            name = ?1,
            description = ?2,
            trigger = ?3,
            action_type = ?4,
            action_config = ?5,
            enabled = ?6,
            execution_order = ?7,
            continue_on_failure = ?8,
            conditions = ?9,
            updated_at = ?10
        WHERE id = ?11
        "#,
        params![
            name,
            description,
            trigger,
            action_type,
            action_config_str,
            enabled,
            execution_order,
            continue_on_failure,
            conditions_str,
            now,
            id,
        ],
    )
    .map_err(|e| format!("Failed to update hook: {}", e))?;

    drop(conn);

    // Fetch the updated hook
    get_hook(id, state).await
}

/// Delete a hook.
///
/// # Arguments
/// * `id` - Hook ID to delete
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if deletion fails
#[tauri::command]
pub async fn delete_hook(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Deleting hook: {}", id);

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    let affected = conn
        .execute("DELETE FROM task_hooks WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete hook: {}", e))?;

    if affected == 0 {
        return Ok(CommandResponse {
            success: false,
            message: Some("Hook not found".to_string()),
            data: None,
        });
    }

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Hook '{}' deleted", id)),
        data: None,
    })
}

/// Enable or disable a hook.
///
/// # Arguments
/// * `id` - Hook ID to update
/// * `enabled` - Whether the hook should be enabled
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if update fails
#[tauri::command]
pub async fn set_hook_enabled(
    id: String,
    enabled: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Setting hook {} enabled={}", id, enabled);

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    let now = chrono::Utc::now().to_rfc3339();

    let affected = conn
        .execute(
            "UPDATE task_hooks SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
            params![enabled, now, id],
        )
        .map_err(|e| format!("Failed to update hook: {}", e))?;

    if affected == 0 {
        return Ok(CommandResponse {
            success: false,
            message: Some("Hook not found".to_string()),
            data: None,
        });
    }

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Hook {} {}",
            id,
            if enabled { "enabled" } else { "disabled" }
        )),
        data: None,
    })
}

/// Reorder hooks.
///
/// Updates execution_order for each hook based on position in the provided list.
///
/// # Arguments
/// * `request` - ReorderHooksRequest with ordered list of hook IDs
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if reordering fails
#[tauri::command]
pub async fn reorder_hooks(
    request: ReorderHooksRequest,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Reordering {} hooks", request.hook_ids.len());

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    let now = chrono::Utc::now().to_rfc3339();

    for (index, hook_id) in request.hook_ids.iter().enumerate() {
        conn.execute(
            "UPDATE task_hooks SET execution_order = ?1, updated_at = ?2 WHERE id = ?3",
            params![index as i32, now, hook_id],
        )
        .map_err(|e| format!("Failed to update hook order: {}", e))?;
    }

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Reordered {} hooks", request.hook_ids.len())),
        data: None,
    })
}

/// Test a hook by executing it with a test context.
///
/// # Arguments
/// * `request` - TestHookRequest with hook ID and optional context
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with TestHookResponse
/// * `Err(String)` - Error message if test fails
#[tauri::command]
pub async fn test_hook(
    request: TestHookRequest,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Testing hook: {}", request.hook_id);

    let conn = state
        .checkpoint_db
        .connection()
        .map_err(|e| format!("Failed to get connection: {}", e))?;

    // Get the hook
    let result = conn.query_row(
        "SELECT action_type, action_config FROM task_hooks WHERE id = ?1",
        params![request.hook_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    );

    let (action_type, action_config_str) = match result {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Ok(CommandResponse {
                success: false,
                message: Some("Hook not found".to_string()),
                data: None,
            });
        }
        Err(e) => return Err(format!("Failed to query hook: {}", e)),
    };

    let action_config: serde_json::Value = serde_json::from_str(&action_config_str)
        .map_err(|e| format!("Failed to parse action config: {}", e))?;

    let action = parse_action(&action_type, action_config)?;

    // Build test context
    let ctx = request.context.unwrap_or_default();
    let hook_context = HookContext::new(
        &ctx.task_run_id.unwrap_or_else(|| "test-run".to_string()),
        &ctx.task_name.unwrap_or_else(|| "Test Task".to_string()),
    )
    .with_iteration(ctx.iteration.unwrap_or(1))
    .with_status(&ctx.status.unwrap_or_else(|| "running".to_string()));

    let hook_context = if let Some(error) = ctx.error {
        hook_context.with_error(&error)
    } else {
        hook_context
    };

    // Create a temporary executor to test the hook
    use crate::orchestrator::hooks::HookExecutor;

    let hook = Hook {
        id: request.hook_id.clone(),
        name: "Test".to_string(),
        trigger: HookTrigger::PreExecution,
        action,
        enabled: true,
        execution_order: 0,
        continue_on_failure: true,
        conditions: vec![],
    };

    let executor = HookExecutor::new(vec![hook]);
    let results = executor.execute_trigger(HookTrigger::PreExecution, &hook_context);

    let result = results.first().map(|r| TestHookResponse {
        success: r.success,
        output: r.output.clone(),
        error: r.error.clone(),
        duration_ms: r.duration_ms,
    });

    Ok(CommandResponse {
        success: true,
        message: Some("Hook test completed".to_string()),
        data: Some(
            serde_json::to_value(&result)
                .map_err(|e| format!("Failed to serialize result: {}", e))?,
        ),
    })
}
