//! HTTP API handlers for the trigger system.
//!
//! CRUD operations, enable/disable, test, history, and webhook ingestion.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

/// Query parameters for history filtering.
#[derive(Debug, serde::Deserialize)]
pub struct HistoryQuery {
    #[serde(default = "default_history_limit")]
    pub limit: u32,
    pub action: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}

fn default_history_limit() -> u32 {
    100
}

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::trigger_system::storage;
use crate::trigger_system::types::*;

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post, put};

    axum::Router::new()
        .route("/triggers", get(list_triggers).post(create_trigger))
        .route(
            "/triggers/:id",
            get(get_trigger).put(update_trigger).delete(delete_trigger),
        )
        .route("/triggers/:id/enabled", put(set_enabled))
        .route("/triggers/:id/test", post(test_trigger))
        .route("/triggers/:id/history", get(get_history))
        .route("/triggers/webhook/:id", post(webhook_ingestion))
        .route("/triggers/status", get(get_status))
}

// ============================================================================
// CRUD Handlers
// ============================================================================

/// List all triggers.
async fn list_triggers(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<WorkflowTrigger>>> {
    let db = &state.app_state.checkpoint_db;

    match storage::get_all_triggers(db) {
        Ok(triggers) => Json(ApiResponse::success(triggers)),
        Err(e) => Json(ApiResponse::error(format!(
            "Failed to list triggers: {}",
            e
        ))),
    }
}

/// Create a new trigger.
async fn create_trigger(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateTriggerRequest>,
) -> Result<(StatusCode, Json<ApiResponse<WorkflowTrigger>>), (StatusCode, Json<ApiResponse<()>>)> {
    let db = &state.app_state.checkpoint_db;

    // Derive trigger_type from config
    let trigger_type = match &request.trigger_config {
        TriggerConfig::Webhook { .. } => "webhook",
        TriggerConfig::FileWatch { .. } => "file_watch",
        TriggerConfig::WorkflowChain { .. } => "workflow_chain",
        TriggerConfig::GitEvent { .. } => "git_event",
        TriggerConfig::HealthCheck { .. } => "health_check",
        TriggerConfig::Schedule { .. } => "schedule",
    };

    let now = chrono::Utc::now().to_rfc3339();
    let trigger = WorkflowTrigger {
        id: uuid::Uuid::new_v4().to_string(),
        name: request.name,
        description: request.description,
        trigger_type: trigger_type.to_string(),
        trigger_config: request.trigger_config,
        workflow_id: request.workflow_id,
        workflow_overrides: request.workflow_overrides,
        conditions: request.conditions,
        debounce_ms: request.debounce_ms,
        cooldown_seconds: request.cooldown_seconds,
        max_concurrent: request.max_concurrent,
        retry_count: request.retry_count,
        retry_delay_seconds: request.retry_delay_seconds,
        enabled: true,
        last_triggered_at: None,
        last_execution_id: None,
        trigger_count: 0,
        created_at: now.clone(),
        updated_at: now,
    };

    storage::create_trigger(db, &trigger).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create trigger: {}", e))),
        )
    })?;

    // Register watcher if service is running
    if let Some(service) = crate::trigger_system::get_trigger_service().await {
        if let Err(e) = service.register_watcher(&trigger).await {
            warn!("Failed to register watcher for new trigger: {}", e);
        }
    }

    Ok((StatusCode::CREATED, Json(ApiResponse::success(trigger))))
}

/// Get a single trigger.
async fn get_trigger(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<WorkflowTrigger>> {
    let db = &state.app_state.checkpoint_db;

    match storage::get_trigger(db, &id) {
        Ok(Some(trigger)) => Json(ApiResponse::success(trigger)),
        Ok(None) => Json(ApiResponse::error(format!("Trigger not found: {}", id))),
        Err(e) => Json(ApiResponse::error(format!("Failed to get trigger: {}", e))),
    }
}

/// Update a trigger.
async fn update_trigger(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateTriggerRequest>,
) -> Json<ApiResponse<WorkflowTrigger>> {
    let db = &state.app_state.checkpoint_db;

    let mut trigger = match storage::get_trigger(db, &id) {
        Ok(Some(t)) => t,
        Ok(None) => return Json(ApiResponse::error(format!("Trigger not found: {}", id))),
        Err(e) => return Json(ApiResponse::error(format!("Failed to get trigger: {}", e))),
    };

    // Apply updates
    if let Some(name) = request.name {
        trigger.name = name;
    }
    if let Some(desc) = request.description {
        trigger.description = desc;
    }
    if let Some(config) = request.trigger_config {
        trigger.trigger_type = match &config {
            TriggerConfig::Webhook { .. } => "webhook".to_string(),
            TriggerConfig::FileWatch { .. } => "file_watch".to_string(),
            TriggerConfig::WorkflowChain { .. } => "workflow_chain".to_string(),
            TriggerConfig::GitEvent { .. } => "git_event".to_string(),
            TriggerConfig::HealthCheck { .. } => "health_check".to_string(),
            TriggerConfig::Schedule { .. } => "schedule".to_string(),
        };
        trigger.trigger_config = config;
    }
    if let Some(wf_id) = request.workflow_id {
        trigger.workflow_id = wf_id;
    }
    if let Some(overrides) = request.workflow_overrides {
        trigger.workflow_overrides = overrides;
    }
    if let Some(conditions) = request.conditions {
        trigger.conditions = conditions;
    }
    if let Some(debounce) = request.debounce_ms {
        trigger.debounce_ms = debounce;
    }
    if let Some(cooldown) = request.cooldown_seconds {
        trigger.cooldown_seconds = cooldown;
    }
    if let Some(max_concurrent) = request.max_concurrent {
        trigger.max_concurrent = max_concurrent;
    }
    if let Some(retry_count) = request.retry_count {
        trigger.retry_count = retry_count;
    }
    if let Some(retry_delay_seconds) = request.retry_delay_seconds {
        trigger.retry_delay_seconds = retry_delay_seconds;
    }

    trigger.updated_at = chrono::Utc::now().to_rfc3339();

    match storage::update_trigger(db, &trigger) {
        Ok(()) => {
            // Re-register watcher if trigger is enabled (config may have changed)
            if trigger.enabled {
                if let Some(service) = crate::trigger_system::get_trigger_service().await {
                    if let Err(e) = service.register_watcher(&trigger).await {
                        warn!("Failed to re-register watcher after update: {}", e);
                    }
                }
            }
            Json(ApiResponse::success(trigger))
        }
        Err(e) => Json(ApiResponse::error(format!(
            "Failed to update trigger: {}",
            e
        ))),
    }
}

/// Delete a trigger.
async fn delete_trigger(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    let db = &state.app_state.checkpoint_db;

    // Unregister the watcher first
    if let Some(service) = crate::trigger_system::get_trigger_service().await {
        service.unregister_watcher(&id).await;
    }

    match storage::delete_trigger(db, &id) {
        Ok(()) => Json(ApiResponse::success(())),
        Err(e) => Json(ApiResponse::error(format!(
            "Failed to delete trigger: {}",
            e
        ))),
    }
}

/// Enable or disable a trigger.
async fn set_enabled(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<SetEnabledRequest>,
) -> Json<ApiResponse<()>> {
    let db = &state.app_state.checkpoint_db;

    match storage::set_trigger_enabled(db, &id, request.enabled) {
        Ok(()) => {
            info!(
                "Trigger {} {}",
                id,
                if request.enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            );

            // Register or unregister watcher based on enabled state
            if let Some(service) = crate::trigger_system::get_trigger_service().await {
                if request.enabled {
                    // Re-register the watcher
                    if let Ok(Some(trigger)) = storage::get_trigger(db, &id) {
                        if let Err(e) = service.register_watcher(&trigger).await {
                            warn!("Failed to register watcher after enable: {}", e);
                        }
                    }
                } else {
                    // Unregister the watcher
                    service.unregister_watcher(&id).await;
                }
            }

            Json(ApiResponse::success(()))
        }
        Err(e) => Json(ApiResponse::error(format!(
            "Failed to set trigger enabled: {}",
            e
        ))),
    }
}

/// Dry-run: evaluate conditions without spawning a workflow.
async fn test_trigger(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<serde_json::Value>> {
    let db = &state.app_state.checkpoint_db;

    let trigger = match storage::get_trigger(db, &id) {
        Ok(Some(t)) => t,
        Ok(None) => return Json(ApiResponse::error(format!("Trigger not found: {}", id))),
        Err(e) => return Json(ApiResponse::error(e)),
    };

    // Create a test event
    let event = crate::trigger_system::types::TriggerEvent {
        trigger_id: id.clone(),
        event_type: "test".to_string(),
        event_data: serde_json::json!({"test": true}),
        variables: HashMap::new(),
        chain_depth: 0,
    };

    // Evaluate without executing
    let service = crate::trigger_system::get_trigger_service().await;
    let result = if let Some(service) = service {
        let eval = service.evaluator().evaluate(&trigger, &event).await;
        serde_json::json!({
            "would_execute": eval.should_execute(),
            "action": eval.action_name(),
            "trigger_name": trigger.name,
            "workflow_id": trigger.workflow_id,
        })
    } else {
        serde_json::json!({
            "error": "Trigger service not running",
            "trigger_name": trigger.name,
        })
    };

    Json(ApiResponse::success(result))
}

/// Get trigger execution history with optional filtering.
async fn get_history(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Json<ApiResponse<Vec<TriggerHistoryEntry>>> {
    let db = &state.app_state.checkpoint_db;

    match storage::get_trigger_history_filtered(
        db,
        &id,
        query.limit,
        query.action.as_deref(),
        query.since.as_deref(),
        query.until.as_deref(),
    ) {
        Ok(entries) => Json(ApiResponse::success(entries)),
        Err(e) => Json(ApiResponse::error(format!("Failed to get history: {}", e))),
    }
}

/// Webhook ingestion endpoint.
///
/// External services POST to `/triggers/webhook/{trigger_id}` to fire webhook triggers.
async fn webhook_ingestion(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    let db = &state.app_state.checkpoint_db;

    // Load the trigger
    let trigger = match storage::get_trigger(db, &id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::error(format!("Trigger not found: {}", id))),
            )
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::error(e)),
            )
        }
    };

    // Verify it's a webhook trigger
    let (secret, payload_filter, variable_mapping) = match &trigger.trigger_config {
        TriggerConfig::Webhook {
            secret,
            payload_filter,
            variable_mapping,
        } => (
            secret.clone(),
            payload_filter.clone(),
            variable_mapping.clone(),
        ),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::error("Trigger is not a webhook type")),
            )
        }
    };

    // Verify HMAC signature if secret is configured
    if let Some(ref secret) = secret {
        // HTTP headers are case-insensitive per RFC 7230
        let signature = headers
            .iter()
            .find(|(name, _)| name.as_str().eq_ignore_ascii_case("x-hub-signature-256"))
            .and_then(|(_, value)| value.to_str().ok());

        match signature {
            Some(sig) => {
                if !crate::trigger_system::watchers::webhook::verify_hmac_signature(
                    secret, &body, sig,
                ) {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(ApiResponse::error("Invalid webhook signature")),
                    );
                }
            }
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(ApiResponse::error(
                        "Missing X-Hub-Signature-256 header (secret is configured)",
                    )),
                )
            }
        }
    }

    // Parse body as JSON
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::error(format!("Invalid JSON body: {}", e))),
            )
        }
    };

    // Apply payload filter
    if let Some(ref filter) = payload_filter {
        if !crate::trigger_system::watchers::webhook::matches_payload_filter(&payload, filter) {
            info!(
                "Webhook for trigger '{}' filtered out by payload_filter",
                trigger.name
            );
            return (
                StatusCode::OK,
                Json(ApiResponse::success(serde_json::json!({
                    "action": "filtered",
                    "message": "Payload did not match filter"
                }))),
            );
        }
    }

    // Extract variables from payload
    let variables =
        crate::trigger_system::watchers::webhook::extract_variables(&payload, &variable_mapping);

    // Send event to trigger service
    let event = crate::trigger_system::types::TriggerEvent {
        trigger_id: id.clone(),
        event_type: "webhook_received".to_string(),
        event_data: payload,
        variables,
        chain_depth: 0,
    };

    match crate::trigger_system::service::send_trigger_event(event).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(ApiResponse::success(serde_json::json!({
                "action": "accepted",
                "trigger_id": id,
            }))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error(format!(
                "Failed to process webhook: {}",
                e
            ))),
        ),
    }
}

/// Get overall trigger system status.
async fn get_status(State(_state): State<Arc<ApiState>>) -> Json<ApiResponse<TriggerSystemStatus>> {
    match crate::trigger_system::get_trigger_service().await {
        Some(service) => {
            let status = service.get_status().await;
            Json(ApiResponse::success(status))
        }
        None => Json(ApiResponse::success(TriggerSystemStatus {
            running: false,
            total_triggers: 0,
            enabled_triggers: 0,
            active_watchers: 0,
            active_executions: 0,
            dropped_events: 0,
        })),
    }
}
