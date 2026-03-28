//! Lifecycle Hooks HTTP API handlers
//!
//! Provides HTTP endpoints for managing lifecycle hooks.
//! Backed by PostgreSQL via `pg_db.*` methods in `database/pg/hooks.rs`.

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

fn json_to_hook_response(val: serde_json::Value) -> Result<HookResponse, String> {
    let id = val["id"].as_str().unwrap_or_default().to_string();
    let name = val["name"].as_str().unwrap_or_default().to_string();
    let description = val["description"].as_str().map(|s| s.to_string());
    let trigger_str = val["trigger"].as_str().unwrap_or_default();
    let action_type_str = val["action_type"].as_str().unwrap_or_default();
    let action_config_str = val["action_config"].as_str().unwrap_or("{}");
    let enabled = val["enabled"].as_bool().unwrap_or(false);
    let execution_order = val["execution_order"].as_i64().unwrap_or(0) as i32;
    let continue_on_failure = val["continue_on_failure"].as_bool().unwrap_or(false);
    let conditions_str = val["conditions"].as_str().unwrap_or("[]");
    let task_run_id = val["task_run_id"].as_str().map(|s| s.to_string());
    let created_at = val["created_at"].as_str().unwrap_or_default().to_string();
    let updated_at = val["updated_at"].as_str().unwrap_or_default().to_string();

    let trigger = parse_trigger(trigger_str)?;
    let action_config: serde_json::Value = serde_json::from_str(action_config_str)
        .map_err(|e| format!("Failed to parse action config: {}", e))?;
    let action = parse_action(action_type_str, action_config)?;
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
    let rows = state
        .app_state
        .pg_db
        .list_hooks()
        .await
        .map_err(|e| {
            error!("Failed to list hooks: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list hooks: {}", e))),
            )
        })?;

    let mut hooks = Vec::new();
    for val in rows {
        let id = val["id"].as_str().unwrap_or("?").to_string();
        match json_to_hook_response(val) {
            Ok(hook) => hooks.push(hook),
            Err(e) => {
                error!("Skipping hook {}: {}", id, e);
                continue;
            }
        }
    }

    Ok(Json(ApiResponse::success(hooks)))
}

/// GET /hooks/:id - Get a single hook
async fn get_hook(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<HookResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let row = state
        .app_state
        .pg_db
        .get_hook(&id)
        .await
        .map_err(|e| {
            error!("Failed to get hook: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get hook: {}", e))),
            )
        })?;

    match row {
        Some(val) => {
            let hook = json_to_hook_response(val)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
            Ok(Json(ApiResponse::success(hook)))
        }
        None => Err((StatusCode::NOT_FOUND, Json(api_error("Hook not found".to_string())))),
    }
}

/// POST /hooks - Create a new hook
async fn create_hook(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateHookRequest>,
) -> Result<Json<ApiResponse<HookResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Validate trigger and action
    let _ = parse_trigger(&request.trigger)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;
    let _ = parse_action(&request.action_type, request.action_config.clone())
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;

    let id = Uuid::new_v4().to_string();

    let action_config_str = serde_json::to_string(&request.action_config)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(format!("Failed to serialize action config: {}", e)))))?;

    let conditions_str = serde_json::to_string(&request.conditions)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(format!("Failed to serialize conditions: {}", e)))))?;

    state
        .app_state
        .pg_db
        .create_hook(
            &id,
            &request.name,
            request.description.as_deref(),
            &request.trigger,
            &request.action_type,
            &action_config_str,
            request.enabled,
            request.execution_order,
            request.continue_on_failure,
            &conditions_str,
            request.task_run_id.as_deref(),
        )
        .await
        .map_err(|e| {
            error!("Failed to create hook: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
        })?;

    // Build response from input (we just inserted, so we know the values)
    let now = chrono::Utc::now().to_rfc3339();
    let trigger = parse_trigger(&request.trigger).unwrap();
    let action_config_val: serde_json::Value = serde_json::from_str(&action_config_str).unwrap_or_default();
    let action = parse_action(&request.action_type, action_config_val).unwrap();
    let conditions: Vec<HookCondition> = serde_json::from_str(&conditions_str).unwrap_or_default();

    let hook = HookResponse {
        id: id.clone(),
        name: request.name,
        description: request.description,
        trigger: trigger_to_string(trigger),
        action_type: action_to_type_string(&action),
        action_config: action_to_config(&action),
        enabled: request.enabled,
        execution_order: request.execution_order,
        continue_on_failure: request.continue_on_failure,
        conditions: conditions
            .iter()
            .map(|c| crate::commands::hooks::HookConditionResponse {
                variable: c.variable.clone(),
                operator: c.operator.clone(),
                value: c.value.clone(),
            })
            .collect(),
        task_run_id: request.task_run_id,
        created_at: now.clone(),
        updated_at: now,
    };

    info!("Created hook: {}", id);
    Ok(Json(ApiResponse::success(hook)))
}

/// PUT /hooks/:id - Update a hook
async fn update_hook(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateHookRequest>,
) -> Result<Json<ApiResponse<HookResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Get current values from PG
    let current_val = state
        .app_state
        .pg_db
        .get_hook(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(api_error("Hook not found".to_string()))))?;

    let name = request.name.unwrap_or_else(|| current_val["name"].as_str().unwrap_or_default().to_string());
    let description: Option<String> = match request.description {
        Some(d) => d, // User explicitly set it (Some(Some(str)) or Some(None))
        None => current_val["description"].as_str().map(|s| s.to_string()),
    };
    let trigger = request.trigger.unwrap_or_else(|| current_val["trigger"].as_str().unwrap_or_default().to_string());
    let action_type = request.action_type.unwrap_or_else(|| current_val["action_type"].as_str().unwrap_or_default().to_string());
    let action_config_str = match request.action_config {
        Some(config) => serde_json::to_string(&config)
            .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(format!("Failed to serialize action config: {}", e)))))?,
        None => current_val["action_config"].as_str().unwrap_or("{}").to_string(),
    };
    let enabled = request.enabled.unwrap_or_else(|| current_val["enabled"].as_bool().unwrap_or(false));
    let execution_order = request.execution_order.unwrap_or_else(|| current_val["execution_order"].as_i64().unwrap_or(0) as i32);
    let continue_on_failure = request.continue_on_failure.unwrap_or_else(|| current_val["continue_on_failure"].as_bool().unwrap_or(false));
    let conditions_str = match request.conditions {
        Some(conditions) => serde_json::to_string(&conditions)
            .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(format!("Failed to serialize conditions: {}", e)))))?,
        None => current_val["conditions"].as_str().unwrap_or("[]").to_string(),
    };

    // Validate
    let _ = parse_trigger(&trigger)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;
    let action_config_val: serde_json::Value = serde_json::from_str(&action_config_str)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(format!("Failed to parse action config: {}", e)))))?;
    let _ = parse_action(&action_type, action_config_val)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;

    state
        .app_state
        .pg_db
        .update_hook(
            &id,
            &name,
            description.as_deref(),
            &trigger,
            &action_type,
            &action_config_str,
            enabled,
            execution_order,
            continue_on_failure,
            &conditions_str,
        )
        .await
        .map_err(|e| {
            error!("Failed to update hook: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
        })?;

    // Re-fetch to get updated_at from PG
    let updated_val = state
        .app_state
        .pg_db
        .get_hook(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(api_error("Hook not found after update".to_string()))))?;

    let hook = json_to_hook_response(updated_val)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    info!("Updated hook: {}", hook.id);
    Ok(Json(ApiResponse::success(hook)))
}

/// DELETE /hooks/:id - Delete a hook
async fn delete_hook_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let deleted = state
        .app_state
        .pg_db
        .delete_hook(&id)
        .await
        .map_err(|e| {
            error!("Failed to delete hook: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
        })?;

    if !deleted {
        return Err((StatusCode::NOT_FOUND, Json(api_error("Hook not found".to_string()))));
    }

    info!("Deleted hook: {}", id);
    Ok(Json(ApiResponse::success(())))
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
    let updated = state
        .app_state
        .pg_db
        .set_hook_enabled(&id, request.enabled)
        .await
        .map_err(|e| {
            error!("Failed to set hook enabled: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
        })?;

    if !updated {
        return Err((StatusCode::NOT_FOUND, Json(api_error("Hook not found".to_string()))));
    }

    Ok(Json(ApiResponse::success(())))
}

/// PUT /hooks/reorder - Reorder hooks
async fn reorder_hooks(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ReorderHooksRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    state
        .app_state
        .pg_db
        .reorder_hooks(&request.hook_ids)
        .await
        .map_err(|e| {
            error!("Failed to reorder hooks: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
        })?;

    Ok(Json(ApiResponse::success(())))
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
    let row = state
        .app_state
        .pg_db
        .get_hook(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(api_error("Hook not found".to_string()))))?;

    let action_type_str = row["action_type"].as_str().unwrap_or_default();
    let action_config_str = row["action_config"].as_str().unwrap_or("{}");
    let action_config: serde_json::Value = serde_json::from_str(action_config_str)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(format!("Failed to parse action config: {}", e)))))?;
    let action = parse_action(action_type_str, action_config)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

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

    // HookExecutor::execute_trigger is sync, run in blocking task
    let result = tokio::task::spawn_blocking(move || {
        let executor = HookExecutor::new(vec![hook]);
        let results = executor.execute_trigger(HookTrigger::PreExecution, &hook_context);

        results
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
            })
    })
    .await
    .map_err(|e| {
        error!("Failed to test hook: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(format!("Task failed: {}", e))))
    })?;

    Ok(Json(ApiResponse::success(result)))
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/hooks", get(list_hooks).post(create_hook))
        .route(
            "/hooks/{id}",
            get(get_hook).put(update_hook).delete(delete_hook_handler),
        )
        .route("/hooks/{id}/enabled", put(set_hook_enabled))
        .route("/hooks/{id}/test", post(test_hook))
        .route("/hooks/reorder", put(reorder_hooks))
}
