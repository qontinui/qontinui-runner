//! Lifecycle Hooks HTTP API handlers
//!
//! Provides HTTP endpoints for managing lifecycle hooks.
//! Wraps the existing database operations from commands/hooks.rs.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post, put},
    Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::commands::hooks::{
    CreateHookRequest, HookResponse, ReorderHooksRequest, TestHookResponse, UpdateHookRequest,
};
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::orchestrator::hooks::{Hook, HookAction, HookCondition, HookContext, HookTrigger};
use rusqlite::params;
use uuid::Uuid;

// ============================================================================
// Internal helpers (reused from commands/hooks.rs logic)
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
        "run_workflow" => {
            let workflow_id = config
                .get("workflow_id")
                .and_then(|v| v.as_str())
                .ok_or("RunWorkflow action requires 'workflow_id' field")?
                .to_string();
            let pass_context = config
                .get("pass_context")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let override_config = config.get("override_config").cloned();

            Ok(HookAction::RunWorkflow {
                workflow_id,
                pass_context,
                override_config,
            })
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
        HookAction::RunWorkflow { .. } => "run_workflow",
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
        HookAction::RunWorkflow {
            workflow_id,
            pass_context,
            override_config,
        } => {
            let mut config = serde_json::json!({
                "type": "run_workflow",
                "workflow_id": workflow_id,
                "pass_context": pass_context,
            });
            if let Some(oc) = override_config {
                config["override_config"] = oc.clone();
            }
            config
        }
    }
}

fn row_to_hook_response(
    id: String,
    name: String,
    description: Option<String>,
    trigger_str: &str,
    action_type: &str,
    action_config_str: &str,
    enabled: bool,
    execution_order: i32,
    continue_on_failure: bool,
    conditions_str: &str,
    task_run_id: Option<String>,
    created_at: String,
    updated_at: String,
) -> Result<HookResponse, String> {
    let trigger = parse_trigger(trigger_str)?;
    let action_config: serde_json::Value = serde_json::from_str(action_config_str)
        .map_err(|e| format!("Failed to parse action config: {}", e))?;
    let action = parse_action(action_type, action_config)?;
    let conditions: Vec<HookCondition> = serde_json::from_str(conditions_str).unwrap_or_default();

    Ok(HookResponse {
        id,
        name,
        description,
        trigger: trigger_to_string(trigger),
        action_type: action_to_type_string(&action),
        action_config: action_to_config(&action),
        enabled,
        execution_order,
        continue_on_failure,
        conditions: conditions
            .iter()
            .map(|c| crate::commands::hooks::HookConditionResponse {
                variable: c.variable.clone(),
                operator: c.operator.clone(),
                value: c.value.clone(),
            })
            .collect(),
        task_run_id,
        created_at,
        updated_at,
    })
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /hooks - List all global hooks
async fn list_hooks(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<HookResponse>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = db
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

            match row_to_hook_response(
                id.clone(),
                name,
                description,
                &trigger_str,
                &action_type,
                &action_config_str,
                enabled,
                execution_order,
                continue_on_failure,
                &conditions_str,
                task_run_id,
                created_at,
                updated_at,
            ) {
                Ok(hook) => hooks.push(hook),
                Err(e) => {
                    error!("Skipping hook {}: {}", id, e);
                    continue;
                }
            }
        }

        Ok::<_, String>(hooks)
    })
    .await
    .map_err(|e| {
        error!("Failed to list hooks: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(hooks) => Ok(Json(ApiResponse::success(hooks))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /hooks/:id - Get a single hook
async fn get_hook(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<HookResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = db
            .connection()
            .map_err(|e| format!("Failed to get connection: {}", e))?;

        let row = conn.query_row(
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

        match row {
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
            )) => row_to_hook_response(
                id,
                name,
                description,
                &trigger_str,
                &action_type,
                &action_config_str,
                enabled,
                execution_order,
                continue_on_failure,
                &conditions_str,
                task_run_id,
                created_at,
                updated_at,
            ),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err("Hook not found".to_string()),
            Err(e) => Err(format!("Failed to query hook: {}", e)),
        }
    })
    .await
    .map_err(|e| {
        error!("Failed to get hook: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(hook) => Ok(Json(ApiResponse::success(hook))),
        Err(e) if e.contains("not found") => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /hooks - Create a new hook
async fn create_hook(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateHookRequest>,
) -> Result<Json<ApiResponse<HookResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        // Validate trigger and action
        let _ = parse_trigger(&request.trigger)?;
        let _ = parse_action(&request.action_type, request.action_config.clone())?;

        let conn = db
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

        row_to_hook_response(
            id,
            request.name,
            request.description,
            &request.trigger,
            &request.action_type,
            &action_config_str,
            request.enabled,
            request.execution_order,
            request.continue_on_failure,
            &conditions_str,
            request.task_run_id,
            now.clone(),
            now,
        )
    })
    .await
    .map_err(|e| {
        error!("Failed to create hook: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(hook) => {
            info!("Created hook: {}", hook.id);
            Ok(Json(ApiResponse::success(hook)))
        }
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

/// PUT /hooks/:id - Update a hook
async fn update_hook(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateHookRequest>,
) -> Result<Json<ApiResponse<HookResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = db
            .connection()
            .map_err(|e| format!("Failed to get connection: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();

        // Get current values
        let current = conn.query_row(
            "SELECT name, description, trigger, action_type, action_config, enabled, execution_order, continue_on_failure, conditions, task_run_id FROM task_hooks WHERE id = ?1",
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
                row.get::<_, Option<String>>(9)?,
            ))
        );

        let (
            current_name, current_description, current_trigger, current_action_type,
            current_action_config, current_enabled, current_execution_order,
            current_continue_on_failure, current_conditions, current_task_run_id,
        ) = match current {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err("Hook not found".to_string());
            }
            Err(e) => return Err(format!("Failed to query hook: {}", e)),
        };

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
        let continue_on_failure = request.continue_on_failure.unwrap_or(current_continue_on_failure);
        let conditions_str = match request.conditions {
            Some(conditions) => serde_json::to_string(&conditions)
                .map_err(|e| format!("Failed to serialize conditions: {}", e))?,
            None => current_conditions,
        };

        // Validate
        let _ = parse_trigger(&trigger)?;
        let action_config: serde_json::Value = serde_json::from_str(&action_config_str)
            .map_err(|e| format!("Failed to parse action config: {}", e))?;
        let _ = parse_action(&action_type, action_config)?;

        conn.execute(
            r#"
            UPDATE task_hooks SET
                name = ?1, description = ?2, trigger = ?3, action_type = ?4,
                action_config = ?5, enabled = ?6, execution_order = ?7,
                continue_on_failure = ?8, conditions = ?9, updated_at = ?10
            WHERE id = ?11
            "#,
            params![name, description, trigger, action_type, action_config_str,
                    enabled, execution_order, continue_on_failure, conditions_str, now, id],
        )
        .map_err(|e| format!("Failed to update hook: {}", e))?;

        // Read back the created_at
        let created_at: String = conn.query_row(
            "SELECT created_at FROM task_hooks WHERE id = ?1",
            params![id],
            |row| row.get(0),
        ).map_err(|e| format!("Failed to read hook: {}", e))?;

        row_to_hook_response(
            id, name, description, &trigger, &action_type,
            &action_config_str, enabled, execution_order, continue_on_failure,
            &conditions_str, current_task_run_id, created_at, now,
        )
    })
    .await
    .map_err(|e| {
        error!("Failed to update hook: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(hook) => {
            info!("Updated hook: {}", hook.id);
            Ok(Json(ApiResponse::success(hook)))
        }
        Err(e) if e.contains("not found") => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

/// DELETE /hooks/:id - Delete a hook
async fn delete_hook_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();
    let id_for_log = id.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = db
            .connection()
            .map_err(|e| format!("Failed to get connection: {}", e))?;

        let affected = conn
            .execute("DELETE FROM task_hooks WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete hook: {}", e))?;

        if affected == 0 {
            return Err("Hook not found".to_string());
        }

        Ok(())
    })
    .await
    .map_err(|e| {
        error!("Failed to delete hook: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => {
            info!("Deleted hook: {}", id_for_log);
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) if e.contains("not found") => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// PUT /hooks/:id/enabled - Enable/disable a hook
#[derive(Debug, Deserialize)]
struct SetEnabledRequest {
    enabled: bool,
}

async fn set_hook_enabled(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<SetEnabledRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = db
            .connection()
            .map_err(|e| format!("Failed to get connection: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();
        let affected = conn
            .execute(
                "UPDATE task_hooks SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
                params![request.enabled, now, id],
            )
            .map_err(|e| format!("Failed to update hook: {}", e))?;

        if affected == 0 {
            return Err("Hook not found".to_string());
        }

        Ok(())
    })
    .await
    .map_err(|e| {
        error!("Failed to set hook enabled: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// PUT /hooks/reorder - Reorder hooks
async fn reorder_hooks(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ReorderHooksRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = db
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

        Ok::<_, String>(())
    })
    .await
    .map_err(|e| {
        error!("Failed to reorder hooks: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /hooks/:id/test - Test a hook
#[derive(Debug, Deserialize, Default)]
struct TestHookContextRequest {
    pub task_run_id: Option<String>,
    pub task_name: Option<String>,
    pub iteration: Option<u32>,
    pub status: Option<String>,
    pub error: Option<String>,
}

async fn test_hook(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(context): Json<Option<TestHookContextRequest>>,
) -> Result<Json<ApiResponse<TestHookResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = db
            .connection()
            .map_err(|e| format!("Failed to get connection: {}", e))?;

        let row = conn.query_row(
            "SELECT action_type, action_config FROM task_hooks WHERE id = ?1",
            params![id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );

        let (action_type, action_config_str) = match row {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err("Hook not found".to_string());
            }
            Err(e) => return Err(format!("Failed to query hook: {}", e)),
        };

        let action_config: serde_json::Value = serde_json::from_str(&action_config_str)
            .map_err(|e| format!("Failed to parse action config: {}", e))?;
        let action = parse_action(&action_type, action_config)?;

        let ctx = context.unwrap_or_default();
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

        use crate::orchestrator::hooks::HookExecutor;

        let hook = Hook {
            id: id.clone(),
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

        let result = results
            .first()
            .map(|r| TestHookResponse {
                success: r.success,
                output: r.output.clone(),
                error: r.error.clone(),
                duration_ms: r.duration_ms,
            })
            .unwrap_or(TestHookResponse {
                success: false,
                output: None,
                error: Some("No result from hook execution".to_string()),
                duration_ms: 0,
            });

        Ok(result)
    })
    .await
    .map_err(|e| {
        error!("Failed to test hook: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Task failed: {}", e))),
        )
    })?;

    match result {
        Ok(response) => Ok(Json(ApiResponse::success(response))),
        Err(e) if e.contains("not found") => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/hooks", get(list_hooks).post(create_hook))
        .route(
            "/hooks/:id",
            get(get_hook).put(update_hook).delete(delete_hook_handler),
        )
        .route("/hooks/:id/enabled", put(set_hook_enabled))
        .route("/hooks/:id/test", post(test_hook))
        .route("/hooks/reorder", put(reorder_hooks))
}
