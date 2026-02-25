//! Unified workflow handlers for MCP API
//!
//! Provides HTTP handlers for unified workflow CRUD operations,
//! execution (single, inline, plan, sequence), generation, and stats.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Manager;
use tracing::{error, info, warn};

use crate::database::CreateTaskRunInput;
use crate::mcp::misc::default_true;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::step_event_builder::categorize_steps;
use crate::workflow_generation;

#[derive(Debug, Serialize, Deserialize)]
pub struct GenerateWorkflowAsyncResponse {
    pub task_run_id: String,
    pub meta_workflow_id: String,
}

pub fn refetch_unified_workflow_steps(
    task_id: &str,
    cached_steps_json: Option<String>,
    db: &crate::database::CheckpointDb,
) -> Option<String> {
    // Check if this is a unified workflow task
    if !task_id.starts_with("unified-workflow-") {
        return cached_steps_json;
    }

    // Extract workflow ID from task ID (format: unified-workflow-{uuid}-{timestamp})
    let parts: Vec<&str> = task_id.split('-').collect();
    if parts.len() < 7 {
        return cached_steps_json;
    }

    let workflow_id = format!(
        "{}-{}-{}-{}-{}",
        parts[2], parts[3], parts[4], parts[5], parts[6]
    );

    info!(
        "Re-fetching unified workflow steps for task {} from workflow {}",
        task_id, workflow_id
    );

    // Fetch workflow from database
    match db.get_unified_workflow(&workflow_id) {
        Ok(Some(workflow)) => {
            use crate::step_executor::ExecutionStepConfig;
            let mut all_steps: Vec<ExecutionStepConfig> = Vec::new();

            // Helper closure to convert step
            let convert_step = |step: &serde_json::Value| -> Option<ExecutionStepConfig> {
                // Debug: log the raw step JSON
                info!(
                    "refetch_unified_workflow_steps: converting step: {}",
                    serde_json::to_string(step).unwrap_or_else(|_| "ERROR".to_string())
                );

                // Try direct deserialization first
                if let Ok(config) = serde_json::from_value::<ExecutionStepConfig>(step.clone()) {
                    info!(
                        "refetch_unified_workflow_steps: serde succeeded, check_type={:?}",
                        config.check_type
                    );
                    return Some(config);
                }

                // Debug: log that serde failed
                info!("refetch_unified_workflow_steps: serde failed, using manual extraction");

                // Fall back to manual extraction
                let step_type = step.get("type").and_then(|t| t.as_str())?;
                let name = step
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string());

                // Helper to get string from either snake_case or camelCase
                let get_str = |keys: &[&str]| -> Option<String> {
                    keys.iter()
                        .find_map(|k| step.get(*k).and_then(|v| v.as_str()))
                        .map(|s| s.to_string())
                };
                let get_bool = |keys: &[&str]| -> Option<bool> {
                    keys.iter()
                        .find_map(|k| step.get(*k).and_then(|v| v.as_bool()))
                };

                Some(ExecutionStepConfig {
                    step_type: step_type.to_string(),
                    name,
                    check_type: get_str(&["check_type", "checkType"]),
                    check_command: get_str(&["command", "check_command", "checkCommand"]),
                    check_working_directory: get_str(&[
                        "working_directory",
                        "workingDirectory",
                        "check_working_directory",
                        "checkWorkingDirectory",
                    ]),
                    check_auto_fix: get_bool(&[
                        "auto_fix",
                        "autoFix",
                        "check_auto_fix",
                        "checkAutoFix",
                    ]),
                    test_id: get_str(&["test_id", "testId"]),
                    test_type: get_str(&["test_type", "testType"]),
                    test_is_critical: get_bool(&["is_critical", "isCritical"]),
                    shell_command: get_str(&["command", "shell_command", "shellCommand"]),
                    shell_command_working_directory: get_str(&[
                        "working_directory",
                        "workingDirectory",
                        "shell_command_working_directory",
                        "shellCommandWorkingDirectory",
                    ]),
                    shell_command_fail_on_error: get_bool(&[
                        "fail_on_error",
                        "failOnError",
                        "shell_command_fail_on_error",
                        "shellCommandFailOnError",
                    ]),
                    prompt_content: get_str(&["content", "prompt_content", "promptContent"]),
                    // UI Bridge fields
                    ui_bridge_action: get_str(&["ui_bridge_action", "uiBridgeAction"]),
                    ui_bridge_url: get_str(&["ui_bridge_url", "uiBridgeUrl"]),
                    ui_bridge_instruction: get_str(&[
                        "ui_bridge_instruction",
                        "uiBridgeInstruction",
                    ]),
                    ui_bridge_target: get_str(&["ui_bridge_target", "uiBridgeTarget"]),
                    ui_bridge_assert_type: get_str(&[
                        "ui_bridge_assert_type",
                        "uiBridgeAssertType",
                    ]),
                    ui_bridge_expected: get_str(&["ui_bridge_expected", "uiBridgeExpected"]),
                    ui_bridge_timeout_ms: step
                        .get("ui_bridge_timeout_ms")
                        .or_else(|| step.get("uiBridgeTimeoutMs"))
                        .and_then(|v| v.as_u64()),
                    ui_bridge_compare_mode: get_str(&[
                        "ui_bridge_compare_mode",
                        "uiBridgeCompareMode",
                    ]),
                    ui_bridge_reference_snapshot: step
                        .get("ui_bridge_reference_snapshot")
                        .or_else(|| step.get("uiBridgeReferenceSnapshot"))
                        .cloned(),
                    ui_bridge_reference_snapshot_id: get_str(&[
                        "ui_bridge_reference_snapshot_id",
                        "uiBridgeReferenceSnapshotId",
                    ]),
                    ui_bridge_severity_threshold: get_str(&[
                        "ui_bridge_severity_threshold",
                        "uiBridgeSeverityThreshold",
                    ]),
                    ..Default::default()
                })
            };

            // Add setup steps (mark as setup phase)
            for step in &workflow.setup_steps {
                if let Some(mut config) = convert_step(step) {
                    config.phase = Some("setup".to_string());
                    all_steps.push(config);
                }
            }

            // Add verification steps
            for step in &workflow.verification_steps {
                if let Some(mut config) = convert_step(step) {
                    config.phase = Some("verification".to_string());
                    all_steps.push(config);
                }
            }

            // Add agentic steps
            for step in &workflow.agentic_steps {
                if let Some(mut config) = convert_step(step) {
                    config.phase = Some("agentic".to_string());
                    all_steps.push(config);
                }
            }

            // Add completion steps (mark as completion phase)
            for step in &workflow.completion_steps {
                if let Some(mut config) = convert_step(step) {
                    config.phase = Some("completion".to_string());
                    all_steps.push(config);
                }
            }

            info!(
                "Re-fetched {} steps from unified workflow definition",
                all_steps.len()
            );

            // Update the task_run with the correct execution_steps_json
            if let Ok(new_json) = serde_json::to_string(&all_steps) {
                if let Err(e) =
                    db.update_task_run_execution_steps(task_id, Some(new_json.clone()), None)
                {
                    warn!(
                        "Failed to update execution_steps_json for task {}: {}",
                        task_id, e
                    );
                }
                Some(new_json)
            } else {
                cached_steps_json
            }
        }
        Ok(None) => {
            warn!(
                "Unified workflow {} not found, using cached execution_steps_json",
                workflow_id
            );
            cached_steps_json
        }
        Err(e) => {
            warn!(
                "Failed to fetch unified workflow {}: {}, using cached execution_steps_json",
                workflow_id, e
            );
            cached_steps_json
        }
    }
}

// ============================================================================
// Web Backend Sync Helpers
// ============================================================================

/// Push a workflow to the web backend (best-effort).
/// On failure, marks the local workflow as sync_pending.
async fn push_to_backend(
    db: &crate::database::CheckpointDb,
    workflow: &crate::unified_workflows::UnifiedWorkflow,
) {
    let client = crate::mcp::web_backend_workflows::WebBackendWorkflowClient::new();
    match client.save_workflow(workflow).await {
        Ok(_) => {
            info!("Synced workflow '{}' to web backend", workflow.name);
            let _ = db.clear_sync_pending(&workflow.id);
        }
        Err(e) => {
            warn!(
                "Failed to push workflow '{}' to backend (will sync later): {}",
                workflow.name, e
            );
            let _ = db.set_sync_pending(&workflow.id);
        }
    }
}

/// Update a workflow on the web backend (best-effort).
/// On failure, marks the local workflow as sync_pending.
async fn update_on_backend(
    db: &crate::database::CheckpointDb,
    workflow: &crate::unified_workflows::UnifiedWorkflow,
) {
    let client = crate::mcp::web_backend_workflows::WebBackendWorkflowClient::new();
    match client.update_workflow(&workflow.id, workflow).await {
        Ok(_) => {
            info!("Synced workflow update '{}' to web backend", workflow.name);
            let _ = db.clear_sync_pending(&workflow.id);
        }
        Err(e) => {
            warn!(
                "Failed to push workflow update '{}' to backend (will sync later): {}",
                workflow.name, e
            );
            let _ = db.set_sync_pending(&workflow.id);
        }
    }
}

/// Delete a workflow from the web backend (best-effort).
async fn delete_from_backend(id: &str) {
    let client = crate::mcp::web_backend_workflows::WebBackendWorkflowClient::new();
    if let Err(e) = client.delete_workflow(id).await {
        warn!("Failed to delete workflow '{}' from backend: {}", id, e);
    }
}

// ============================================================================
// Unified Workflows HTTP API Handlers
// ============================================================================

/// List all unified workflows
pub async fn list_unified_workflows(
    State(state): State<Arc<ApiState>>,
) -> Result<
    Json<ApiResponse<Vec<crate::unified_workflows::UnifiedWorkflow>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state.app_state.checkpoint_db.list_unified_workflows() {
        Ok(workflows) => Ok(Json(ApiResponse::success(workflows))),
        Err(e) => {
            error!("Failed to list unified workflows: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to list unified workflows: {}",
                    e
                ))),
            ))
        }
    }
}

/// Get a single unified workflow by ID
/// Checks local cache first, falls back to web backend on cache miss.
pub async fn get_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::UnifiedWorkflow>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    // Check local cache first
    match state.app_state.checkpoint_db.get_unified_workflow(&id) {
        Ok(Some(workflow)) => return Ok(Json(ApiResponse::success(workflow))),
        Ok(None) => {
            // Cache miss — try web backend
            info!("Workflow {} not in local cache, trying web backend", id);
        }
        Err(e) => {
            error!("Failed to get unified workflow from cache: {}", e);
            // Still try backend as fallback
        }
    }

    // Try fetching from web backend
    let client = crate::mcp::web_backend_workflows::WebBackendWorkflowClient::new();
    match client.fetch_workflow(&id).await {
        Ok(workflow) => {
            info!(
                "Fetched workflow '{}' from web backend, caching locally",
                workflow.name
            );
            // Cache locally for future access
            let create_req = crate::unified_workflows::CreateUnifiedWorkflowRequest {
                name: workflow.name.clone(),
                description: workflow.description.clone(),
                category: workflow.category.clone(),
                tags: workflow.tags.clone(),
                setup_steps: workflow.setup_steps.clone(),
                verification_steps: workflow.verification_steps.clone(),
                agentic_steps: workflow.agentic_steps.clone(),
                completion_steps: workflow.completion_steps.clone(),
                max_iterations: workflow.max_iterations,
                timeout_seconds: workflow.timeout_seconds,
                provider: workflow.provider.clone(),
                model: workflow.model.clone(),
                skip_ai_summary: workflow.skip_ai_summary,
                log_source_selection: Some(workflow.log_source_selection.clone()),
                context_ids: Some(workflow.context_ids.clone()),
                disabled_context_ids: Some(workflow.disabled_context_ids.clone()),
                auto_include_contexts: Some(workflow.auto_include_contexts),
                prompt_template: workflow.prompt_template.clone(),
                log_watch_enabled: Some(workflow.log_watch_enabled),
                health_check_enabled: Some(workflow.health_check_enabled),
                health_check_urls: Some(workflow.health_check_urls.clone()),
                preflight_check_enabled: Some(workflow.preflight_check_enabled),
                targeted_error_ids: None,
                generated_by_task_run_id: workflow.generated_by_task_run_id.clone(),
                enable_sweep: Some(workflow.enable_sweep),
                max_sweep_iterations: Some(workflow.max_sweep_iterations),
            };
            if let Err(e) = state
                .app_state
                .checkpoint_db
                .create_unified_workflow_with_id(&workflow.id, &create_req)
            {
                warn!("Failed to cache workflow locally: {}", e);
            }
            Ok(Json(ApiResponse::success(workflow)))
        }
        Err(e) => {
            warn!("Workflow {} not found on backend either: {}", id, e);
            Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Unified workflow not found: {}", id))),
            ))
        }
    }
}

/// Create a new unified workflow
pub async fn create_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<crate::unified_workflows::CreateUnifiedWorkflowRequest>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::UnifiedWorkflow>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Creating unified workflow: {}", request.name);
    match state
        .app_state
        .checkpoint_db
        .create_unified_workflow(&request)
    {
        Ok(created) => {
            info!(
                "Created unified workflow: {} ({})",
                created.name, created.id
            );
            // Push to web backend (best-effort, async)
            push_to_backend(&state.app_state.checkpoint_db, &created).await;
            Ok(Json(ApiResponse::success(created)))
        }
        Err(e) => {
            error!("Failed to create unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to create unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Update a unified workflow
pub async fn update_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<crate::unified_workflows::UpdateUnifiedWorkflowRequest>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::UnifiedWorkflow>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Updating unified workflow: {}", id);

    // Check if this workflow was generated by a task run (for feedback tracking)
    let existing_for_feedback = state
        .app_state
        .checkpoint_db
        .get_unified_workflow(&id)
        .ok()
        .flatten();
    if let Some(ref existing) = existing_for_feedback {
        if let Some(ref task_run_id) = existing.generated_by_task_run_id {
            info!(
                "Workflow '{}' (generated by task run '{}') is being updated — \
                 this is a post-generation feedback event indicating the user \
                 refined the generated workflow",
                existing.name, task_run_id
            );

            // Record structured feedback
            let feedback = crate::workflow_generation::feedback::FeedbackType::Edit {
                field: "workflow_update".to_string(),
                old_value: None,
                new_value: None,
            };
            if let Err(e) = state.app_state.checkpoint_db.with_conn(|conn| {
                crate::workflow_generation::feedback::record_workflow_feedback(
                    conn,
                    &id,
                    Some(task_run_id),
                    Some(&existing.category),
                    Some(&existing.description),
                    &feedback,
                )
            }) {
                warn!("Failed to record edit feedback: {}", e);
            }
        }
    }

    match state
        .app_state
        .checkpoint_db
        .update_unified_workflow(&id, &request)
    {
        Ok(updated) => {
            info!(
                "Updated unified workflow: {} ({})",
                updated.name, updated.id
            );
            // Push update to web backend (best-effort, async)
            update_on_backend(&state.app_state.checkpoint_db, &updated).await;
            Ok(Json(ApiResponse::success(updated)))
        }
        Err(e) if e.contains("not found") => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Unified workflow not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to update unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to update unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Delete a unified workflow
pub async fn delete_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Deleting unified workflow: {}", id);

    // Check if this workflow was generated by a task run (for feedback tracking)
    if let Ok(Some(existing)) = state.app_state.checkpoint_db.get_unified_workflow(&id) {
        if let Some(ref task_run_id) = existing.generated_by_task_run_id {
            info!(
                "Workflow '{}' (generated by task run '{}') is being deleted — \
                 this is a post-generation feedback event indicating the user \
                 rejected the generated workflow",
                existing.name, task_run_id
            );

            // Record structured feedback
            let feedback =
                crate::workflow_generation::feedback::FeedbackType::Delete { reason: None };
            if let Err(e) = state.app_state.checkpoint_db.with_conn(|conn| {
                crate::workflow_generation::feedback::record_workflow_feedback(
                    conn,
                    &id,
                    Some(task_run_id),
                    Some(&existing.category),
                    Some(&existing.description),
                    &feedback,
                )
            }) {
                warn!("Failed to record delete feedback: {}", e);
            }
        }
    }

    match state.app_state.checkpoint_db.delete_unified_workflow(&id) {
        Ok(true) => {
            // Delete from web backend (best-effort, async)
            delete_from_backend(&id).await;
            Ok(Json(ApiResponse::success(serde_json::json!({
                "deleted": true,
                "id": id
            }))))
        }
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Unified workflow not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to delete unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to delete unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Search unified workflows
pub async fn search_unified_workflows(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<crate::unified_workflows::SearchUnifiedWorkflowsQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::unified_workflows::UnifiedWorkflow>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .checkpoint_db
        .search_unified_workflows(&query)
    {
        Ok(workflows) => Ok(Json(ApiResponse::success(workflows))),
        Err(e) => {
            error!("Failed to search unified workflows: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to search unified workflows: {}",
                    e
                ))),
            ))
        }
    }
}

/// Duplicate a unified workflow
pub async fn duplicate_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::UnifiedWorkflow>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Duplicating unified workflow: {}", id);
    match state
        .app_state
        .checkpoint_db
        .duplicate_unified_workflow(&id)
    {
        Ok(duplicated) => {
            info!("Duplicated unified workflow: {} -> {}", id, duplicated.id);
            // Push duplicate to web backend (best-effort, async)
            push_to_backend(&state.app_state.checkpoint_db, &duplicated).await;
            Ok(Json(ApiResponse::success(duplicated)))
        }
        Err(e) if e.contains("not found") => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Unified workflow not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to duplicate unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to duplicate unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Export a single unified workflow as a standalone JSON file
pub async fn export_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::WorkflowExport>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Exporting unified workflow: {}", id);

    match state.app_state.checkpoint_db.get_unified_workflow(&id) {
        Ok(Some(workflow)) => {
            let export = crate::unified_workflows::WorkflowExport {
                manifest: crate::unified_workflows::WorkflowExportManifest {
                    version: "1.0.0".to_string(),
                    exported_at: chrono::Utc::now().to_rfc3339(),
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                    content_type: "unified_workflow".to_string(),
                },
                workflow,
            };
            info!("Exported unified workflow: {}", id);
            Ok(Json(ApiResponse::success(export)))
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Unified workflow not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to export unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to export unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Import a unified workflow from an export file
pub async fn import_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<crate::unified_workflows::ImportWorkflowRequest>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::ImportWorkflowResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!(
        "Importing unified workflow: {} (strategy: {})",
        request.workflow.name, request.conflict_strategy
    );

    let mut workflow = request.workflow;
    let original_id = workflow.id.clone();
    let mut overwritten = false;

    // Check if workflow with this ID already exists
    let existing = state
        .app_state
        .checkpoint_db
        .get_unified_workflow(&workflow.id)
        .ok()
        .flatten();

    match request.conflict_strategy.as_str() {
        "keep" => {
            // Try to use the original ID, fail if it exists
            if existing.is_some() {
                return Err((
                    StatusCode::CONFLICT,
                    Json(api_error(format!(
                        "Workflow with ID '{}' already exists. Use 'generate' or 'overwrite' strategy.",
                        workflow.id
                    ))),
                ));
            }
        }
        "overwrite" => {
            // If exists, delete it first
            if existing.is_some() {
                if let Err(e) = state
                    .app_state
                    .checkpoint_db
                    .delete_unified_workflow(&workflow.id)
                {
                    error!("Failed to delete existing workflow for overwrite: {}", e);
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!(
                            "Failed to delete existing workflow: {}",
                            e
                        ))),
                    ));
                }
                overwritten = true;
            }
        }
        _ => {
            // Always generate a new ID
            workflow.id = uuid::Uuid::new_v4().to_string();
        }
    }

    // Update timestamps
    let now = chrono::Utc::now().to_rfc3339();
    workflow.updated_at = now.clone();
    if request.conflict_strategy != "overwrite" || !overwritten {
        workflow.created_at = now;
    }

    // Create the workflow using the existing create function logic
    let create_request = crate::unified_workflows::CreateUnifiedWorkflowRequest {
        name: workflow.name.clone(),
        description: workflow.description.clone(),
        category: workflow.category.clone(),
        tags: workflow.tags.clone(),
        setup_steps: workflow.setup_steps.clone(),
        verification_steps: workflow.verification_steps.clone(),
        agentic_steps: workflow.agentic_steps.clone(),
        completion_steps: workflow.completion_steps.clone(),
        max_iterations: workflow.max_iterations,
        timeout_seconds: workflow.timeout_seconds,
        provider: workflow.provider.clone(),
        model: workflow.model.clone(),
        skip_ai_summary: workflow.skip_ai_summary,
        log_source_selection: Some(workflow.log_source_selection.clone()),
        context_ids: Some(workflow.context_ids.clone()),
        disabled_context_ids: Some(workflow.disabled_context_ids.clone()),
        auto_include_contexts: Some(workflow.auto_include_contexts),
        prompt_template: workflow.prompt_template.clone(),
        log_watch_enabled: Some(workflow.log_watch_enabled),
        health_check_enabled: Some(workflow.health_check_enabled),
        health_check_urls: Some(workflow.health_check_urls.clone()),
        preflight_check_enabled: Some(workflow.preflight_check_enabled),
        targeted_error_ids: None,
        generated_by_task_run_id: workflow.generated_by_task_run_id.clone(),
        enable_sweep: Some(workflow.enable_sweep),
        max_sweep_iterations: Some(workflow.max_sweep_iterations),
    };

    // Use the database's create function but with our custom ID
    match state
        .app_state
        .checkpoint_db
        .create_unified_workflow_with_id(&workflow.id, &create_request)
    {
        Ok(created) => {
            info!(
                "Imported unified workflow: {} ({}) [overwritten: {}]",
                created.name, created.id, overwritten
            );
            // Push imported workflow to web backend (best-effort, async)
            push_to_backend(&state.app_state.checkpoint_db, &created).await;
            Ok(Json(ApiResponse::success(
                crate::unified_workflows::ImportWorkflowResult {
                    workflow: created,
                    overwritten,
                    original_id: if workflow.id != original_id {
                        Some(original_id)
                    } else {
                        None
                    },
                },
            )))
        }
        Err(e) => {
            error!("Failed to import unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to import unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Generate a unified workflow from natural language description using AI
pub async fn generate_unified_workflow_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<workflow_generation::GenerateWorkflowRequest>,
) -> Result<
    Json<ApiResponse<workflow_generation::GenerateWorkflowResponse>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!(
        "Generating unified workflow from description: {}...",
        &request.description[..request.description.len().min(50)]
    );

    // Clone handles so they can be moved into the blocking closure
    let doctor_handle = state.doctor_handle.clone();
    let db = state.app_state.checkpoint_db.clone();

    // Run the generation in a blocking task since it uses sync AI provider
    let result = tokio::task::spawn_blocking(move || {
        // Get DB connection for RAG examples + filtered schema context
        let gen_result = db.with_conn(|conn| {
            Ok(workflow_generation::generate_workflow(
                request,
                doctor_handle.as_ref(),
                Some(conn),
                None, // Embedding computed lazily if embedding API is available
            ))
        });
        match gen_result {
            Ok(response) => response,
            Err(e) => {
                warn!(
                    "DB access failed for workflow generation, falling back to no-DB path: {}",
                    e
                );
                // This shouldn't happen, but if it does, the request was already moved
                // into the closure above, so we can't retry. Return an error response.
                workflow_generation::GenerateWorkflowResponse {
                    workflow: None,
                    validation_errors: vec![],
                    success: false,
                    error: Some(format!("Database error during generation: {}", e)),
                    model_used: None,
                    verification_iterations: vec![],
                    hardening_summary: None,
                    discovery_calls: vec![],
                }
            }
        }
    })
    .await
    .map_err(|e| {
        error!(
            "Failed to spawn blocking task for workflow generation: {}",
            e
        );
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to generate workflow: {}", e))),
        )
    })?;

    if result.success {
        info!(
            "Successfully generated workflow: {}",
            result
                .workflow
                .as_ref()
                .map(|w| w.name.as_str())
                .unwrap_or("unknown")
        );
        Ok(Json(ApiResponse::success(result)))
    } else {
        warn!(
            "Workflow generation failed: {}",
            result.error.as_deref().unwrap_or("unknown error")
        );
        // Still return success HTTP status with the error in the response body
        // This allows the client to show the error message to the user
        Ok(Json(ApiResponse::success(result)))
    }
}

/// Generate a unified workflow asynchronously using a meta-workflow approach.
///
/// Instead of generating synchronously, this endpoint:
/// 1. Resolves contexts from the request
/// 2. Builds a meta-workflow (a UnifiedWorkflow that generates another workflow)
/// 3. Saves the meta-workflow to the database
/// 4. Creates a task run for the meta-workflow execution
/// 5. Returns the task_run_id and meta_workflow_id for frontend polling
pub async fn generate_unified_workflow_async_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<workflow_generation::GenerateWorkflowRequest>,
) -> Result<Json<ApiResponse<GenerateWorkflowAsyncResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::workflow_generation::meta_workflow::{
        build_historical_context, build_meta_workflow_template,
    };

    info!(
        "Generating unified workflow async from description: {}...",
        &request.description[..request.description.len().min(50)]
    );

    // Resolve contexts (same pattern as generator.rs)
    let mut resolved_contexts = String::new();
    if let Some(ref ids) = request.context_ids {
        if !ids.is_empty() {
            let resolved = crate::context::resolve_contexts(ids, false, "", &[], &[]);
            if let Some(formatted) = crate::context::format_contexts_for_prompt(&resolved) {
                resolved_contexts.push_str(&formatted);
            }
        }
    }
    if let Some(ref inline) = request.inline_context {
        if !inline.is_empty() {
            resolved_contexts.push_str(&format!(
                "<context name=\"User-Provided Context\">\n{}\n</context>\n\n",
                inline
            ));
        }
    }

    // Build historical context from database (best-effort, falls back gracefully)
    let historical_context = state
        .app_state
        .checkpoint_db
        .with_conn(|conn| {
            Ok(build_historical_context(
                conn,
                &request.description,
                None, // Embedding computed lazily if embedding API is available
                request.category.as_deref(),
            ))
        })
        .ok()
        .flatten();

    // Build the meta-workflow
    let meta_workflow =
        build_meta_workflow_template(&request, &resolved_contexts, historical_context.as_ref());

    // Save the meta-workflow to database
    let create_request = crate::unified_workflows::CreateUnifiedWorkflowRequest {
        name: meta_workflow.name.clone(),
        description: meta_workflow.description.clone(),
        category: meta_workflow.category.clone(),
        tags: meta_workflow.tags.clone(),
        setup_steps: meta_workflow.setup_steps.clone(),
        verification_steps: meta_workflow.verification_steps.clone(),
        agentic_steps: meta_workflow.agentic_steps.clone(),
        completion_steps: meta_workflow.completion_steps.clone(),
        max_iterations: meta_workflow.max_iterations,
        timeout_seconds: meta_workflow.timeout_seconds,
        provider: meta_workflow.provider.clone(),
        model: meta_workflow.model.clone(),
        skip_ai_summary: meta_workflow.skip_ai_summary,
        log_source_selection: Some(meta_workflow.log_source_selection.clone()),
        log_watch_enabled: Some(meta_workflow.log_watch_enabled),
        health_check_enabled: Some(meta_workflow.health_check_enabled),
        health_check_urls: Some(meta_workflow.health_check_urls.clone()),
        preflight_check_enabled: Some(meta_workflow.preflight_check_enabled),
        context_ids: Some(meta_workflow.context_ids.clone()),
        disabled_context_ids: Some(meta_workflow.disabled_context_ids.clone()),
        auto_include_contexts: Some(meta_workflow.auto_include_contexts),
        prompt_template: meta_workflow.prompt_template.clone(),
        targeted_error_ids: Some(meta_workflow.targeted_error_ids.clone()),
        generated_by_task_run_id: None, // Will be set after task run creation
        enable_sweep: Some(meta_workflow.enable_sweep),
        max_sweep_iterations: Some(meta_workflow.max_sweep_iterations),
    };

    let saved_workflow = match state
        .app_state
        .checkpoint_db
        .create_unified_workflow(&create_request)
    {
        Ok(w) => w,
        Err(e) => {
            error!("Failed to save meta-workflow: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to save meta-workflow: {}", e))),
            ));
        }
    };

    // Create a task run for this workflow
    let task_run_id = uuid::Uuid::new_v4().to_string();
    let task_run_input = CreateTaskRunInput::new(&task_run_id, &saved_workflow.name)
        .with_workflow_type("unified")
        .with_workflow_name(&saved_workflow.name)
        .with_max_sessions(saved_workflow.max_iterations)
        .with_auto_continue(true);

    if let Err(e) = state
        .app_state
        .checkpoint_db
        .create_task_run(&task_run_input)
    {
        error!("Failed to create task run: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create task run: {}", e))),
        ));
    }

    info!(
        "Created meta-workflow '{}' (id={}) with task run {}",
        saved_workflow.name, saved_workflow.id, task_run_id
    );

    // Sync task run creation to web backend (best-effort, non-blocking)
    {
        let db = state.app_state.checkpoint_db.clone();
        let tid = task_run_id.clone();
        tokio::spawn(async move {
            let sync_service = crate::commands::task_sync::AITaskSyncService::new();
            if let Ok(Some(task)) = db.get_task_run(&tid) {
                if let Err(e) = sync_service.sync_task_created(&task, None).await {
                    warn!("Failed to sync task creation to backend: {}", e);
                }
            }
        });
    }

    // Convert workflow steps for LoopController with explicit phase assignment
    {
        use crate::unified_workflow_executor::{
            convert_json_steps_with_phase, extract_prompt_steps_with_phase, LoopConfig,
            LoopController,
        };

        let setup_automation_steps =
            convert_json_steps_with_phase(&saved_workflow.setup_steps, 0, Some("setup"));
        let setup_automation_steps = crate::unified_workflows::prepend_preflight_check_step(
            setup_automation_steps,
            saved_workflow.preflight_check_enabled,
        );
        let setup_prompt_steps =
            extract_prompt_steps_with_phase(&saved_workflow.setup_steps, Some("setup"));
        let verification_steps = convert_json_steps_with_phase(
            &saved_workflow.verification_steps,
            0,
            Some("verification"),
        );
        let verification_steps = crate::unified_workflows::prepend_health_check_steps(
            verification_steps,
            saved_workflow.health_check_enabled,
            &saved_workflow.health_check_urls,
        );
        let verification_steps = crate::unified_workflows::prepend_log_watch_step(
            verification_steps,
            saved_workflow.log_watch_enabled,
        );
        let agentic_steps =
            extract_prompt_steps_with_phase(&saved_workflow.agentic_steps, Some("agentic"));
        let completion_automation_steps =
            convert_json_steps_with_phase(&saved_workflow.completion_steps, 0, Some("completion"));
        let completion_prompt_steps =
            extract_prompt_steps_with_phase(&saved_workflow.completion_steps, Some("completion"));

        // For meta-workflows, run agentic first if there are targeted errors
        let run_agentic_first = !saved_workflow.targeted_error_ids.is_empty();

        let loop_config = LoopConfig {
            max_iterations: saved_workflow.max_iterations,
            base_prompt: String::new(),
            workflow_name: saved_workflow.name.clone(),
            workflow_id: saved_workflow.id.clone(),
            execution_id: task_run_id.clone(),
            targeted_error_ids: saved_workflow.targeted_error_ids.clone(),
            starting_iteration: 0,
            run_agentic_first,
            artifact_dir: None,
            is_dev_mode: cfg!(debug_assertions),
            enable_sweep: saved_workflow.enable_sweep,
            max_sweep_iterations: saved_workflow.max_sweep_iterations,
        };

        // Clone state fields for the background task
        let app_state = state.app_state.clone();
        let config_storage = state.config_storage.clone();
        let app_handle = state.app_handle.clone();
        let pid_tracker = state.current_ai_pids.clone();
        let checkpoint_db = state.app_state.checkpoint_db.clone();
        let workflow_name = saved_workflow.name.clone();
        let execution_id_for_guard = task_run_id.clone();

        // Get session manager for interactive mode
        let session_manager: Arc<crate::claude_session::SessionManager> = state
            .app_handle
            .state::<Arc<crate::claude_session::SessionManager>>()
            .inner()
            .clone();

        // Spawn the workflow executor in the background with panic protection
        let url_lock = Some(state.app_state.url_lock_manager.clone());
        crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
            checkpoint_db,
            execution_id_for_guard,
            workflow_name,
            url_lock,
            async move {
                let mut controller =
                    LoopController::new(app_state, config_storage, app_handle, pid_tracker)
                        .with_session_manager(session_manager);

                controller
                    .run(
                        loop_config,
                        setup_automation_steps,
                        setup_prompt_steps,
                        verification_steps,
                        agentic_steps,
                        completion_automation_steps,
                        completion_prompt_steps,
                    )
                    .await
            },
        );
    }

    Ok(Json(ApiResponse::success(GenerateWorkflowAsyncResponse {
        task_run_id,
        meta_workflow_id: saved_workflow.id,
    })))
}

// =============================================================================
// NOTE: run_unified_workflow_with_verification_loop was removed and replaced
// with the modular unified_workflow_executor module.
// See: src/unified_workflow_executor/
// =============================================================================

/// Request body for running a unified workflow
#[derive(Debug, Deserialize)]
pub struct RunUnifiedWorkflowRequest {
    /// Monitor index to use (defaults to 0)
    #[serde(default)]
    monitor_index: Option<i32>,
    /// Optional task_run_id for resuming an existing execution.
    /// If provided, the workflow will resume from where it left off.
    /// If not provided, the system will check for incomplete task_runs and auto-resume,
    /// or create a new execution if none exist.
    #[serde(default)]
    task_run_id: Option<String>,
    /// Force a fresh start even if there's an incomplete task_run.
    /// When true, creates a new execution_id instead of resuming.
    #[serde(default)]
    force_fresh_start: bool,
}

/// Response body for running a unified workflow (non-blocking)
#[derive(Debug, Serialize)]
pub struct RunUnifiedWorkflowResponse {
    pub task_run_id: String,
}

/// Request body for executing an inline workflow (without saving to database)
/// Used by Quick Fix to run a workflow directly without cluttering the library
#[derive(Debug, Deserialize, Serialize)]
pub struct ExecuteInlineWorkflowRequest {
    /// Workflow name
    name: String,
    /// Description
    #[serde(default)]
    description: String,
    /// Setup phase steps
    #[serde(default)]
    setup_steps: Vec<serde_json::Value>,
    /// Verification phase steps
    #[serde(default)]
    verification_steps: Vec<serde_json::Value>,
    /// Agentic phase steps
    #[serde(default)]
    agentic_steps: Vec<serde_json::Value>,
    /// Completion phase steps
    #[serde(default)]
    completion_steps: Vec<serde_json::Value>,
    /// Maximum iterations for agentic phase
    #[serde(default = "default_max_iterations")]
    max_iterations: u32,
    /// Timeout in seconds
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// Monitor index to use (defaults to 0)
    #[serde(default)]
    monitor_index: Option<i32>,
    /// Error IDs targeted by this workflow
    #[serde(default)]
    targeted_error_ids: Vec<i64>,
    /// Workflow settings (optional, extracted from generated workflow)
    #[serde(default)]
    settings: Option<serde_json::Value>,
}

pub fn default_max_iterations() -> u32 {
    10
}

/// Request body for executing a structured implementation plan.
///
/// Each phase runs as a separate AI session with full context from prior phases.
#[derive(Debug, Deserialize, Serialize)]
pub struct ExecutePlanRequest {
    /// Name of the plan.
    plan_name: String,
    /// Overview/description of the plan.
    plan_overview: String,
    /// Ordered list of phases to execute.
    phases: Vec<PlanPhaseInput>,
    /// Whether to run a "next steps sweep" after all phases (default: true).
    #[serde(default = "default_true")]
    next_steps_sweep: bool,
    /// Maximum number of sweep iterations (default: 5).
    #[serde(default = "default_max_sweep_iterations")]
    max_next_steps_iterations: u32,
}

/// A single phase in a plan execution request.
#[derive(Debug, Deserialize, Serialize)]
pub struct PlanPhaseInput {
    /// Human-readable name for this phase.
    name: String,
    /// The prompt/instructions for this phase.
    prompt: String,
}

fn default_max_sweep_iterations() -> u32 {
    5
}

/// Run a unified workflow by ID
///
/// This endpoint executes a unified workflow by:
/// 1. Fetching the workflow from the database
/// 2. Converting phase steps to executable steps
/// 3. Running setup -> verification -> agentic -> completion phases
pub async fn run_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<RunUnifiedWorkflowRequest>,
) -> Result<Json<ApiResponse<RunUnifiedWorkflowResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Running unified workflow: {}", id);

    // Fetch the workflow
    let mut workflow = match state.app_state.checkpoint_db.get_unified_workflow(&id) {
        Ok(Some(w)) => w,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Unified workflow not found: {}", id))),
            ));
        }
        Err(e) => {
            error!("Failed to get unified workflow: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get unified workflow: {}", e))),
            ));
        }
    };

    // Apply deterministic command sanitization (fixes jq→python, nested retry, etc.)
    {
        use crate::workflow_generation::hardener::sanitize_commands_in_steps;
        let mut fix_count = 0;
        fix_count += sanitize_commands_in_steps(&mut workflow.setup_steps);
        fix_count += sanitize_commands_in_steps(&mut workflow.verification_steps);
        fix_count += sanitize_commands_in_steps(&mut workflow.completion_steps);
        if fix_count > 0 {
            info!(
                "Runtime sanitizer: fixed {} steps with command/retry issues",
                fix_count
            );
        }
    }

    info!(
        "Executing unified workflow '{}' with {} setup, {} verification, {} agentic, {} completion steps",
        workflow.name,
        workflow.setup_steps.len(),
        workflow.verification_steps.len(),
        workflow.agentic_steps.len(),
        workflow.completion_steps.len()
    );

    // Save unified workflow config to .dev-logs for Claude Code debugging access
    // This is separate from GUI automation configs to avoid confusion
    if let Ok(workflow_json) = serde_json::to_value(&workflow) {
        crate::executor::file_logger::save_unified_workflow_config(&workflow_json, &workflow.name);
    }

    let monitor_index = request.monitor_index.unwrap_or(0);

    // Convert JSON steps to ExecutionStepConfig
    // For now, we run phases sequentially: setup -> (verification + agentic) -> completion
    let mut all_steps: Vec<crate::step_executor::ExecutionStepConfig> = Vec::new();

    // Helper to convert Value steps to ExecutionStepConfig
    let convert_step = |step: &serde_json::Value,
                        _monitor: i32|
     -> Option<crate::step_executor::ExecutionStepConfig> {
        // Try to deserialize the step directly
        if let Ok(mut config) =
            serde_json::from_value::<crate::step_executor::ExecutionStepConfig>(step.clone())
        {
            // Fix ambiguous "command" field mapping based on step_type
            // Both shell_command and check_command have alias "command", so serde picks one arbitrarily.
            // We need to ensure the command goes to the right field based on step_type.
            if let Some(command) = step.get("command").and_then(|v| v.as_str()) {
                let cmd = command.to_string();
                match config.step_type.as_str() {
                    "command" | "shell_command" | "check" | "check_group" => {
                        // Route "command" field to the right struct field based on check_type
                        if config.check_type.is_some() {
                            if config.check_command.is_none() {
                                config.check_command = Some(cmd);
                            }
                        } else if config.shell_command.is_none() {
                            config.shell_command = Some(cmd);
                        }
                        // Normalize legacy step types to "command"
                        config.step_type = "command".to_string();
                    }
                    "test" => {
                        // Test steps may also use command field
                        if config.shell_command.is_none() {
                            config.shell_command = Some(cmd);
                        }
                        // Normalize "test" to "command" (test is now a command dispatch mode)
                        config.step_type = "command".to_string();
                    }
                    _ => {}
                }
            } else {
                // Even without a "command" field, normalize legacy step types
                match config.step_type.as_str() {
                    "shell_command" | "check" | "check_group" | "test" => {
                        config.step_type = "command".to_string();
                    }
                    _ => {}
                }
            }

            // Fix prompt content mapping - "content" field maps to prompt_content for prompt steps
            if config.step_type == "prompt" && config.prompt_content.is_none() {
                if let Some(content) = step.get("content").and_then(|v| v.as_str()) {
                    config.prompt_content = Some(content.to_string());
                }
            }

            // Normalize nested retry format: {"retry": {"count": N, "delay_ms": M}}
            if config.retry_count.is_none() {
                if let Some(retry_obj) = step.get("retry") {
                    if let Some(c) = retry_obj.get("count").and_then(|v| v.as_u64()) {
                        config.retry_count = Some(c as u32);
                    }
                    if let Some(d) = retry_obj.get("delay_ms").and_then(|v| v.as_u64()) {
                        config.retry_delay_ms = Some(d);
                    }
                }
            }

            return Some(config);
        }

        // Fall back to extracting type and creating manually
        let step_type = step.get("type").and_then(|t| t.as_str())?;
        let name = step
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string());

        Some(crate::step_executor::ExecutionStepConfig {
            step_type: step_type.to_string(),
            name,
            prompt_content: step
                .get("content")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            timeout_seconds: step.get("timeoutSeconds").and_then(|v| v.as_u64()),
            phase: step
                .get("phase")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            run_on_subsequent_iterations: None,
            test_id: step
                .get("test_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            test_type: step
                .get("test_type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            test_is_critical: step.get("is_critical").and_then(|v| v.as_bool()),
            sub_step_id: step
                .get("subStepId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            // Shell command fields
            shell_command: step
                .get("command")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            shell_command_id: step
                .get("shell_command_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            shell_command_working_directory: step
                .get("working_directory")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            shell_command_fail_on_error: step.get("fail_on_error").and_then(|v| v.as_bool()),
            // Check fields - support both snake_case and camelCase
            check_type: step
                .get("check_type")
                .or_else(|| step.get("checkType"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            check_command: step
                .get("command")
                .or_else(|| step.get("check_command"))
                .or_else(|| step.get("checkCommand"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            check_working_directory: step
                .get("working_directory")
                .or_else(|| step.get("workingDirectory"))
                .or_else(|| step.get("check_working_directory"))
                .or_else(|| step.get("checkWorkingDirectory"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            check_auto_fix: step
                .get("auto_fix")
                .or_else(|| step.get("autoFix"))
                .or_else(|| step.get("check_auto_fix"))
                .or_else(|| step.get("checkAutoFix"))
                .and_then(|v| v.as_bool()),
            check_url: step
                .get("check_url")
                .or_else(|| step.get("checkUrl"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            expected_status: step
                .get("expected_status")
                .or_else(|| step.get("expectedStatus"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u16),
            // Check group fields
            check_group_id: step
                .get("check_group_id")
                .or_else(|| step.get("checkGroupId"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            ..Default::default()
        })
    };

    // Add setup steps (mark as setup phase)
    for step in &workflow.setup_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("setup".to_string());
            all_steps.push(config);
        }
    }

    // Add verification steps
    for step in &workflow.verification_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("verification".to_string());
            all_steps.push(config);
        }
    }

    // Add agentic steps
    for step in &workflow.agentic_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("agentic".to_string());
            all_steps.push(config);
        }
    }

    // Add completion steps (mark as completion phase)
    for step in &workflow.completion_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("completion".to_string());
            all_steps.push(config);
        }
    }

    if all_steps.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("Workflow has no steps to execute".to_string())),
        ));
    }

    // Extract verification-phase steps BEFORE categorize_steps consumes all_steps.
    // This preserves the original step ordering (e.g., prompt checks before gates)
    // which is critical for gate steps that depend on prompt step results.
    let ordered_verification_steps: Vec<_> = all_steps
        .iter()
        .filter(|s| s.phase.as_deref() == Some("verification"))
        .cloned()
        .collect();

    // Separate automation steps from AI steps using the robust categorize_steps helper.
    // This replaces the fragile string-based partition logic.
    let (automation_steps, prompt_steps) = categorize_steps(all_steps, |s| &s.step_type);
    let has_prompt_steps = !prompt_steps.is_empty();

    // Determine execution_id for resume support:
    // 1. If task_run_id is explicitly provided, use it (explicit resume)
    // 2. If force_fresh_start is false, check for incomplete task_run (auto-resume)
    // 3. Otherwise, generate a new execution_id
    let execution_id = if let Some(ref provided_id) = request.task_run_id {
        info!(
            "Using provided task_run_id for explicit resume: {}",
            provided_id
        );
        provided_id.clone()
    } else if !request.force_fresh_start {
        // Check for incomplete task_run to auto-resume
        match state
            .app_state
            .checkpoint_db
            .get_incomplete_task_run_for_workflow(&id)
        {
            Ok(Some(existing_id)) => {
                info!(
                    "Found incomplete task_run {} for workflow {} - auto-resuming",
                    existing_id, id
                );
                existing_id
            }
            Ok(None) => {
                // No incomplete run found, generate new execution_id
                let new_id = format!(
                    "unified-workflow-{}-{}",
                    id,
                    chrono::Utc::now().timestamp_millis()
                );
                info!("No incomplete task_run found, starting fresh: {}", new_id);
                new_id
            }
            Err(e) => {
                warn!(
                    "Failed to check for incomplete task_run: {} - starting fresh",
                    e
                );
                format!(
                    "unified-workflow-{}-{}",
                    id,
                    chrono::Utc::now().timestamp_millis()
                )
            }
        }
    } else {
        // force_fresh_start = true
        let new_id = format!(
            "unified-workflow-{}-{}",
            id,
            chrono::Utc::now().timestamp_millis()
        );
        info!("Force fresh start requested, new execution_id: {}", new_id);
        new_id
    };

    // If workflow has prompt steps, use AI-based execution
    if has_prompt_steps {
        info!(
            "Workflow '{}' has {} prompt steps - using AI-based execution",
            workflow.name,
            prompt_steps.len()
        );

        // Combine all prompt contents into a single prompt
        let combined_prompt = prompt_steps
            .iter()
            .filter_map(|s| s.prompt_content.as_ref())
            .map(|content| content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        if combined_prompt.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "Workflow has prompt steps but no prompt content".to_string(),
                )),
            ));
        }

        // =====================================================================
        // UNIFIED VERIFICATION-AGENTIC LOOP (required for all AI workflows)
        // =====================================================================
        // All AI workflows must have verification steps. The loop:
        // 1. Runs verification FIRST to tell the agentic phase what to work on
        // 2. Builds failure context from verification results
        // 3. Loops: verification -> agentic until pass or max_iterations
        // 4. Cannot be bypassed by AI claiming [TASK_COMPLETE]
        //
        // Verification steps can be:
        // - Automated tests (Playwright, shell commands)
        // - AI-based verification (prompt steps that check work quality)
        //
        // If you want a simple AI task, add a verification step like:
        // "Review the changes and verify the task was completed correctly"
        if workflow.verification_steps.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "AI workflows require at least one verification step. \
                     Verification can be automated (tests) or AI-based (a prompt that checks work quality). \
                     This ensures the AI's work is verified before marking the task complete.".to_string(),
                )),
            ));
        }

        info!(
            "Unified workflow '{}' has {} verification steps - using verification-agentic loop",
            workflow.name,
            workflow.verification_steps.len()
        );

        // Separate steps by phase for the new loop function
        // Setup automation steps (shell commands, workflows, etc.)
        let setup_automation_steps: Vec<_> = automation_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("setup"))
            .cloned()
            .collect();
        // Prepend pre-flight check if enabled (default: true)
        let setup_automation_steps = crate::unified_workflows::prepend_preflight_check_step(
            setup_automation_steps,
            workflow.preflight_check_enabled,
        );
        // Setup prompt steps (AI tasks during setup)
        let setup_prompt_steps: Vec<_> = prompt_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("setup"))
            .cloned()
            .collect();
        // Use ordered_verification_steps (extracted before categorize_steps) to preserve
        // the original step ordering from the workflow definition. This is critical because
        // gate steps must come AFTER the prompt/automation steps they reference.
        // The previous approach of automation-first-then-prompt broke gate evaluation.
        let verification_steps = ordered_verification_steps;

        // Prepend health check steps if enabled and URLs configured
        // Health checks run BEFORE log_watch to catch server down before scanning logs
        let verification_steps = crate::unified_workflows::prepend_health_check_steps(
            verification_steps,
            workflow.health_check_enabled,
            &workflow.health_check_urls,
        );

        // Prepend log_watch step if enabled (default: true)
        let verification_steps = crate::unified_workflows::prepend_log_watch_step(
            verification_steps,
            workflow.log_watch_enabled,
        );

        // Filter prompt_steps by phase - agentic prompts only
        let agentic_steps: Vec<_> = prompt_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("agentic"))
            .cloned()
            .collect();
        // Completion steps: combine non-prompt completion steps with prompt completion steps
        let mut completion_steps: Vec<_> = automation_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("completion"))
            .cloned()
            .collect();
        // Add completion prompts (e.g., AI summary) to completion_steps
        completion_steps.extend(
            prompt_steps
                .iter()
                .filter(|s| s.phase.as_deref() == Some("completion"))
                .cloned(),
        );

        // Create task_run for tracking with workflow_type="unified"
        // This prevents TaskMonitor and legacy session code from modifying status
        let execution_steps_json = serde_json::to_string(&automation_steps).ok();
        let mut input = CreateTaskRunInput::new(&execution_id, &workflow.name)
            .with_prompt(&combined_prompt)
            .with_task_type("ai")
            .with_workflow_name(&workflow.name)
            .with_max_sessions(workflow.max_iterations)
            .with_auto_continue(true)
            .with_workflow_type("unified"); // LoopController is sole authority on status
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Err(e) = state.app_state.checkpoint_db.create_task_run(&input) {
            warn!(
                "Failed to create task_run for unified workflow {}: {}",
                execution_id, e
            );
        }

        // Separate completion steps into automation and prompt steps
        let (completion_automation_steps, completion_prompt_steps) =
            categorize_steps(completion_steps, |s| &s.step_type);

        // For error-fix workflows, run agentic first
        let run_agentic_first = !workflow.targeted_error_ids.is_empty();

        let loop_config = crate::unified_workflow_executor::LoopConfig {
            max_iterations: workflow.max_iterations,
            base_prompt: combined_prompt,
            workflow_name: workflow.name.clone(),
            workflow_id: workflow.id.clone(),
            execution_id: execution_id.clone(),
            targeted_error_ids: workflow.targeted_error_ids.clone(),
            starting_iteration: 0, // Fresh start
            run_agentic_first,
            artifact_dir: None,
            is_dev_mode: cfg!(debug_assertions),
            enable_sweep: workflow.enable_sweep,
            max_sweep_iterations: workflow.max_sweep_iterations,
        };

        // Spawn execution in background (non-blocking) — same pattern as
        // generate_unified_workflow_async_handler
        let checkpoint_db = state.app_state.checkpoint_db.clone();
        let execution_id_for_guard = execution_id.clone();
        let workflow_name_for_guard = workflow.name.clone();
        let app_state = state.app_state.clone();
        let config_storage = state.config_storage.clone();
        let app_handle = state.app_handle.clone();
        let pid_tracker = state.current_ai_pids.clone();
        let url_lock = Some(state.app_state.url_lock_manager.clone());

        crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
            checkpoint_db,
            execution_id_for_guard,
            workflow_name_for_guard,
            url_lock,
            async move {
                let session_manager: Arc<crate::claude_session::SessionManager> = app_handle
                    .state::<Arc<crate::claude_session::SessionManager>>()
                    .inner()
                    .clone();
                let mut controller = crate::unified_workflow_executor::LoopController::new(
                    app_state,
                    config_storage,
                    app_handle,
                    pid_tracker,
                )
                .with_session_manager(session_manager);

                controller
                    .run(
                        loop_config,
                        setup_automation_steps,
                        setup_prompt_steps,
                        verification_steps,
                        agentic_steps,
                        completion_automation_steps,
                        completion_prompt_steps,
                    )
                    .await
            },
        );

        return Ok(Json(ApiResponse::success(RunUnifiedWorkflowResponse {
            task_run_id: execution_id,
        })));
    }

    // No prompt steps - use step_executor for automation-only workflow
    // Create a task_run record so the workflow shows in the Active page
    // Serialize full step configuration so re-execution on resume has all fields
    let execution_steps_json = serde_json::to_string(&automation_steps).ok();

    // Create task_run to track this execution (enables Active page monitoring)
    let mut input = CreateTaskRunInput::new(&execution_id, &workflow.name)
        .with_task_type("automation") // identifies as automation task
        .with_workflow_name(&workflow.name); // helps identify this in the dashboard
    if let Some(esj) = execution_steps_json {
        input = input.with_execution_steps_json(esj);
    }
    if let Err(e) = state.app_state.checkpoint_db.create_task_run(&input) {
        warn!(
            "Failed to create task_run for unified workflow {}: {}",
            execution_id, e
        );
    }

    // Spawn automation execution in background (non-blocking)
    let response_task_run_id = execution_id.clone();
    let checkpoint_db = state.app_state.checkpoint_db.clone();
    let execution_id_for_guard = execution_id.clone();
    let workflow_name_for_guard = workflow.name.clone();
    let app_state = state.app_state.clone();
    let config_storage = state.config_storage.clone();
    let app_handle = state.app_handle.clone();
    let url_lock = Some(state.app_state.url_lock_manager.clone());

    crate::unified_workflow_executor::spawn_sequence_with_panic_guard(
        checkpoint_db,
        execution_id_for_guard,
        workflow_name_for_guard,
        url_lock,
        async move {
            let executor = crate::step_executor::StepExecutor::with_app_handle(
                app_state.clone(),
                config_storage,
                app_handle,
            );

            let result = executor
                .execute_steps_with_log_sources(&automation_steps, &execution_id, &[])
                .await;

            info!(
                "Unified workflow automation completed: {} of {} steps succeeded",
                result.successful_steps, result.total_steps
            );

            // Update task_run status based on result
            if result.success {
                if let Err(e) = app_state.checkpoint_db.complete_task_run(&execution_id) {
                    warn!(
                        "Failed to mark task_run {} as completed: {}",
                        execution_id, e
                    );
                }
            } else {
                let error_msg = result
                    .steps
                    .iter()
                    .find(|s| !s.success)
                    .and_then(|s| s.error.as_ref())
                    .map(|s| s.as_str())
                    .unwrap_or("Unknown error");
                if let Err(e) = app_state
                    .checkpoint_db
                    .fail_task_run(&execution_id, error_msg)
                {
                    warn!("Failed to mark task_run {} as failed: {}", execution_id, e);
                }
            }
        },
    );

    Ok(Json(ApiResponse::success(RunUnifiedWorkflowResponse {
        task_run_id: response_task_run_id,
    })))
}

/// Stores the last inline workflow request for re-execution via "Run Last Workflow".
/// Inline workflows aren't saved to the database, so this provides a way to re-run them.
static LAST_INLINE_WORKFLOW: std::sync::Mutex<Option<serde_json::Value>> =
    std::sync::Mutex::new(None);

/// Get the last inline workflow definition for re-execution.
/// Checks in-memory cache first (fast path), then falls back to SQLite (survives restart).
pub async fn get_last_inline_workflow(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Fast path: check in-memory cache
    {
        let guard = LAST_INLINE_WORKFLOW
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(workflow) = guard.as_ref() {
            return Ok(Json(ApiResponse::success(workflow.clone())));
        }
    }

    // Slow path: check SQLite settings table (survives runner restart)
    match state
        .app_state
        .checkpoint_db
        .get_setting("last_inline_workflow")
    {
        Ok(Some(workflow)) => {
            // Populate in-memory cache for future fast lookups
            let mut guard = LAST_INLINE_WORKFLOW
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *guard = Some(workflow.clone());
            Ok(Json(ApiResponse::success(workflow)))
        }
        _ => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(
                "No inline workflow has been executed yet".to_string(),
            )),
        )),
    }
}

/// Execute an inline workflow without saving to the database
///
/// This endpoint is used by Quick Fix to run a generated workflow directly
/// without cluttering the workflow library. The workflow is executed with
/// a temporary ID and is not persisted.
pub async fn execute_inline_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecuteInlineWorkflowRequest>,
) -> Result<
    Json<ApiResponse<crate::step_executor::ExecutionResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Executing inline workflow: {}", request.name);

    // Check for duplicate running error-fix workflows
    // This prevents multiple Quick Fix workflows from targeting the same errors
    if request.name.contains("Fix") && request.name.contains("Error") {
        if let Ok(Some(existing_id)) = state
            .app_state
            .checkpoint_db
            .has_running_error_fix_workflow()
        {
            warn!(
                "Duplicate error-fix workflow prevented - already running: {}",
                existing_id
            );
            return Err((
                StatusCode::CONFLICT,
                Json(api_error(format!(
                    "An error-fix workflow is already running (task_id: {}). \
                     Please wait for it to complete or stop it before starting a new one.",
                    existing_id
                ))),
            ));
        }
    }

    // Store the request for re-execution via "Run Last Workflow" button
    // Persist to both in-memory cache (fast path) and SQLite (survives restart)
    if let Ok(request_json) = serde_json::to_value(&request) {
        let mut guard = LAST_INLINE_WORKFLOW
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = Some(request_json.clone());

        if let Err(e) = state
            .app_state
            .checkpoint_db
            .set_setting("last_inline_workflow", &request_json)
        {
            warn!("Failed to persist inline workflow to settings: {}", e);
        }
    }

    // Create a temporary workflow object (not saved to DB)
    let execution_id = uuid::Uuid::new_v4().to_string();
    let workflow = crate::unified_workflows::UnifiedWorkflow {
        id: format!("inline-{}", execution_id),
        name: request.name.clone(),
        description: request.description,
        category: "error-fix".to_string(),
        tags: vec!["inline".to_string(), "quick-fix".to_string()],
        setup_steps: request.setup_steps,
        verification_steps: request.verification_steps,
        agentic_steps: request.agentic_steps,
        completion_steps: request.completion_steps,
        max_iterations: request.max_iterations,
        timeout_seconds: request.timeout_seconds,
        provider: None,
        model: None,
        skip_ai_summary: false,
        targeted_error_ids: request.targeted_error_ids,
        log_source_selection: Default::default(),
        context_ids: vec![],
        disabled_context_ids: vec![],
        auto_include_contexts: true,
        prompt_template: None,
        log_watch_enabled: true,
        health_check_enabled: false,
        health_check_urls: vec![],
        preflight_check_enabled: true,
        enable_sweep: false,
        max_sweep_iterations: 5,
        generated_by_task_run_id: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    info!(
        "Inline workflow '{}' has {} setup, {} verification, {} agentic, {} completion steps",
        workflow.name,
        workflow.setup_steps.len(),
        workflow.verification_steps.len(),
        workflow.agentic_steps.len(),
        workflow.completion_steps.len()
    );

    // Save unified workflow config to .dev-logs for debugging (uses inline- prefix)
    if let Ok(workflow_json) = serde_json::to_value(&workflow) {
        crate::executor::file_logger::save_unified_workflow_config(&workflow_json, &workflow.name);
    }

    let monitor_index = request.monitor_index.unwrap_or(0);

    // Convert JSON steps to ExecutionStepConfig
    let mut all_steps: Vec<crate::step_executor::ExecutionStepConfig> = Vec::new();

    // Helper to convert Value steps to ExecutionStepConfig
    let convert_step = |step: &serde_json::Value,
                        _monitor: i32|
     -> Option<crate::step_executor::ExecutionStepConfig> {
        if let Ok(mut config) =
            serde_json::from_value::<crate::step_executor::ExecutionStepConfig>(step.clone())
        {
            if let Some(command) = step.get("command").and_then(|v| v.as_str()) {
                let cmd = command.to_string();
                match config.step_type.as_str() {
                    "command" | "shell_command" | "check" | "check_group" => {
                        if config.check_type.is_some() {
                            if config.check_command.is_none() {
                                config.check_command = Some(cmd);
                            }
                        } else if config.shell_command.is_none() {
                            config.shell_command = Some(cmd);
                        }
                        config.step_type = "command".to_string();
                    }
                    "test" => {
                        if config.shell_command.is_none() {
                            config.shell_command = Some(cmd);
                        }
                        config.step_type = "command".to_string();
                    }
                    _ => {}
                }
            } else {
                match config.step_type.as_str() {
                    "shell_command" | "check" | "check_group" | "test" => {
                        config.step_type = "command".to_string();
                    }
                    _ => {}
                }
            }
            if config.step_type == "prompt" && config.prompt_content.is_none() {
                if let Some(content) = step.get("content").and_then(|v| v.as_str()) {
                    config.prompt_content = Some(content.to_string());
                }
            }
            return Some(config);
        }
        let step_type = step.get("type").and_then(|t| t.as_str())?;
        let name = step
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string());
        Some(crate::step_executor::ExecutionStepConfig {
            step_type: step_type.to_string(),
            name,
            ..Default::default()
        })
    };

    // Convert all phase steps with phase markers
    for step in &workflow.setup_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("setup".to_string());
            all_steps.push(config);
        }
    }
    for step in &workflow.verification_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("verification".to_string());
            all_steps.push(config);
        }
    }
    for step in &workflow.agentic_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("agentic".to_string());
            all_steps.push(config);
        }
    }
    for step in &workflow.completion_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("completion".to_string());
            all_steps.push(config);
        }
    }

    info!(
        "Converted {} total steps for inline workflow",
        all_steps.len()
    );

    // Extract verification-phase steps BEFORE categorize_steps consumes all_steps.
    // This preserves the original step ordering (e.g., prompt checks before gates)
    // which is critical for gate steps that depend on prompt step results.
    let ordered_verification_steps: Vec<_> = all_steps
        .iter()
        .filter(|s| s.phase.as_deref() == Some("verification"))
        .cloned()
        .collect();

    // Separate steps by type
    let (automation_steps, prompt_steps) = categorize_steps(all_steps, |s| &s.step_type);

    // If there are prompt steps, use the verification-agentic loop
    if !prompt_steps.is_empty() {
        let combined_prompt = prompt_steps
            .iter()
            .filter_map(|s| s.prompt_content.as_ref())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        if combined_prompt.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "Workflow has prompt steps but no prompt content".to_string(),
                )),
            ));
        }

        // Separate steps by phase
        let setup_automation_steps: Vec<_> = automation_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("setup"))
            .cloned()
            .collect();
        // Prepend pre-flight check if enabled (default: true)
        let setup_automation_steps = crate::unified_workflows::prepend_preflight_check_step(
            setup_automation_steps,
            workflow.preflight_check_enabled,
        );
        let setup_prompt_steps: Vec<_> = prompt_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("setup"))
            .cloned()
            .collect();
        // Use ordered_verification_steps (extracted before categorize_steps) to preserve
        // the original step ordering from the workflow definition. This is critical because
        // gate steps must come AFTER the prompt/automation steps they reference.
        let verification_steps = ordered_verification_steps;

        // Prepend log_watch step
        let verification_steps = crate::unified_workflows::prepend_log_watch_step(
            verification_steps,
            workflow.log_watch_enabled,
        );

        let agentic_steps: Vec<_> = prompt_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("agentic"))
            .cloned()
            .collect();
        let mut completion_steps: Vec<_> = automation_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("completion"))
            .cloned()
            .collect();
        completion_steps.extend(
            prompt_steps
                .iter()
                .filter(|s| s.phase.as_deref() == Some("completion"))
                .cloned(),
        );

        // Create task_run for tracking (marked as inline/temporary)
        let execution_steps_json = serde_json::to_string(&automation_steps).ok();
        let mut input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
            .with_prompt(&combined_prompt)
            .with_task_type("ai")
            .with_workflow_name(format!("[Inline] {}", workflow.name))
            .with_max_sessions(workflow.max_iterations)
            .with_auto_continue(true)
            .with_workflow_type("unified");
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Err(e) = state.app_state.checkpoint_db.create_task_run(&input) {
            warn!(
                "Failed to create task_run for inline workflow {}: {}",
                execution_id, e
            );
        }

        let (completion_automation_steps, completion_prompt_steps) =
            categorize_steps(completion_steps, |s| &s.step_type);

        // For error-fix workflows (with targeted_error_ids), run agentic first.
        // This ensures the AI attempts to fix errors before verification runs,
        // since log_watch verification may pass immediately if logs are currently clean.
        let run_agentic_first = !workflow.targeted_error_ids.is_empty();

        let loop_config = crate::unified_workflow_executor::LoopConfig {
            max_iterations: workflow.max_iterations,
            base_prompt: combined_prompt,
            workflow_name: workflow.name.clone(),
            workflow_id: workflow.id.clone(),
            execution_id: execution_id.clone(),
            targeted_error_ids: workflow.targeted_error_ids.clone(),
            starting_iteration: 0,
            run_agentic_first,
            artifact_dir: None,
            is_dev_mode: cfg!(debug_assertions),
            enable_sweep: workflow.enable_sweep,
            max_sweep_iterations: workflow.max_sweep_iterations,
        };

        let session_manager: Arc<crate::claude_session::SessionManager> = state
            .app_handle
            .state::<Arc<crate::claude_session::SessionManager>>()
            .inner()
            .clone();
        let mut controller = crate::unified_workflow_executor::LoopController::new(
            state.app_state.clone(),
            state.config_storage.clone(),
            state.app_handle.clone(),
            state.current_ai_pids.clone(),
        )
        .with_session_manager(session_manager);

        let result = controller
            .run(
                loop_config,
                setup_automation_steps,
                setup_prompt_steps,
                verification_steps,
                agentic_steps,
                completion_automation_steps,
                completion_prompt_steps,
            )
            .await;

        return Ok(Json(ApiResponse::success(result.to_execution_result())));
    }

    // No prompt steps - use step_executor for automation-only workflow
    let execution_steps_json = serde_json::to_string(&automation_steps).ok();
    let mut input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
        .with_task_type("automation")
        .with_workflow_name(format!("[Inline] {}", workflow.name));
    if let Some(esj) = execution_steps_json {
        input = input.with_execution_steps_json(esj);
    }
    if let Err(e) = state.app_state.checkpoint_db.create_task_run(&input) {
        warn!(
            "Failed to create task_run for inline workflow {}: {}",
            execution_id, e
        );
    }

    let executor = crate::step_executor::StepExecutor::with_app_handle(
        state.app_state.clone(),
        state.config_storage.clone(),
        state.app_handle.clone(),
    );

    let result = executor
        .execute_steps_with_log_sources(&automation_steps, &execution_id, &[])
        .await;

    info!(
        "Inline workflow '{}' completed: {} of {} steps succeeded",
        workflow.name, result.successful_steps, result.total_steps
    );

    // Update task_run status
    if result.success {
        if let Err(e) = state
            .app_state
            .checkpoint_db
            .complete_task_run(&execution_id)
        {
            warn!(
                "Failed to mark task_run {} as completed: {}",
                execution_id, e
            );
        }
    } else {
        let error_msg = result
            .steps
            .iter()
            .find(|s| !s.success)
            .and_then(|s| s.error.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("Unknown error");
        if let Err(e) = state
            .app_state
            .checkpoint_db
            .fail_task_run(&execution_id, error_msg)
        {
            warn!("Failed to mark task_run {} as failed: {}", execution_id, e);
        }
    }

    Ok(Json(ApiResponse::success(result)))
}

/// Execute a structured implementation plan.
///
/// Each phase runs as a separate AI session. Output from each phase is
/// accumulated so subsequent phases have full context from prior work.
/// Optionally runs a "next steps sweep" to catch overlooked items.
pub async fn execute_plan(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecutePlanRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Validate request
    if request.phases.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::error("Plan must have at least one phase")),
        ));
    }

    let execution_id = format!("plan-{}", chrono::Utc::now().timestamp_millis());
    let plan_name = request.plan_name.clone();

    info!(
        "MCP API: execute_plan '{}' with {} phases (id: {})",
        plan_name,
        request.phases.len(),
        execution_id
    );

    // Create task_run in database
    let input = crate::database::CreateTaskRunInput::new(&execution_id, &plan_name)
        .with_task_type("ai")
        .with_workflow_type("plan")
        .with_workflow_name(format!("[Plan] {}", plan_name))
        .with_max_sessions(
            request.phases.len() as u32
                + if request.next_steps_sweep {
                    request.max_next_steps_iterations
                } else {
                    0
                },
        )
        .with_auto_continue(true)
        .with_prompt(&request.plan_overview);

    if let Err(e) = state.app_state.checkpoint_db.create_task_run(&input) {
        warn!("Failed to create task_run for plan {}: {}", execution_id, e);
    }

    // Build PlanConfig
    let config = crate::plan_executor::PlanConfig {
        plan_name: request.plan_name.clone(),
        plan_overview: request.plan_overview.clone(),
        phases: request
            .phases
            .into_iter()
            .map(|p| crate::plan_executor::PlanPhase {
                name: p.name,
                prompt: p.prompt,
            })
            .collect(),
        next_steps_sweep: request.next_steps_sweep,
        max_next_steps_iterations: request.max_next_steps_iterations,
        execution_id: execution_id.clone(),
    };

    // Get session manager for interactive mode (clone out before async block)
    let session_manager: Option<Arc<crate::claude_session::SessionManager>> = state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .map(|sm| sm.inner().clone());

    let app_state = state.app_state.clone();
    let app_handle = state.app_handle.clone();
    let pid_tracker = state.current_ai_pids.clone();
    let checkpoint_db = state.app_state.checkpoint_db.clone();
    let exec_id = execution_id.clone();
    let name = request.plan_name.clone();

    crate::plan_executor::spawn_plan_with_panic_guard(checkpoint_db, exec_id, name, async move {
        let mut executor =
            crate::plan_executor::PlanExecutor::new(app_state, app_handle, pid_tracker);

        if let Some(sm) = session_manager {
            executor = executor.with_session_manager(sm);
        }

        executor.run(config).await
    });

    Ok(Json(ApiResponse::success(serde_json::json!({
        "success": true,
        "execution_id": execution_id,
        "message": format!("Plan '{}' execution started", plan_name),
    }))))
}

/// Workflow execution statistics
#[derive(Serialize)]
pub struct WorkflowStats {
    #[serde(rename = "totalRuns")]
    total_runs: u32,
    #[serde(rename = "successCount")]
    success_count: u32,
    #[serde(rename = "failureCount")]
    failure_count: u32,
    #[serde(rename = "lastRunAt")]
    last_run_at: Option<String>,
    #[serde(rename = "lastRunStatus")]
    last_run_status: Option<String>,
    #[serde(rename = "avgDurationMs")]
    avg_duration_ms: Option<i64>,
}

/// Get execution statistics for a unified workflow
pub async fn get_unified_workflow_stats(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<WorkflowStats>>, (StatusCode, Json<ApiResponse<()>>)> {
    // First verify the workflow exists
    let workflow = match state.app_state.checkpoint_db.get_unified_workflow(&id) {
        Ok(Some(w)) => w,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Unified workflow not found: {}", id))),
            ));
        }
        Err(e) => {
            error!("Failed to get unified workflow: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get unified workflow: {}", e))),
            ));
        }
    };

    // Query stats from task_runs table by workflow_name
    let conn = match state.app_state.checkpoint_db.connection() {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to get database connection: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get database connection: {}",
                    e
                ))),
            ));
        }
    };

    let stats_result: Result<WorkflowStats, rusqlite::Error> = conn.query_row(
        r#"
        SELECT
            COUNT(*) as total_runs,
            SUM(CASE WHEN status = 'complete' THEN 1 ELSE 0 END) as success_count,
            SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) as failure_count,
            MAX(created_at) as last_run_at,
            (SELECT status FROM task_runs WHERE workflow_name = ?1
             ORDER BY created_at DESC LIMIT 1) as last_run_status,
            AVG(CASE WHEN completed_at IS NOT NULL
                THEN (julianday(completed_at) - julianday(created_at)) * 86400000
                END) as avg_duration_ms
        FROM task_runs
        WHERE workflow_name = ?1
        "#,
        [&workflow.name],
        |row| {
            Ok(WorkflowStats {
                total_runs: row.get::<_, i64>(0)? as u32,
                success_count: row.get::<_, i64>(1)? as u32,
                failure_count: row.get::<_, i64>(2)? as u32,
                last_run_at: row.get(3)?,
                last_run_status: row.get(4)?,
                // AVG() returns a float, so we need to convert it to i64
                avg_duration_ms: row.get::<_, Option<f64>>(5)?.map(|f| f as i64),
            })
        },
    );

    match stats_result {
        Ok(stats) => Ok(Json(ApiResponse::success(stats))),
        Err(e) => {
            // If no rows found, return empty stats
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                Ok(Json(ApiResponse::success(WorkflowStats {
                    total_runs: 0,
                    success_count: 0,
                    failure_count: 0,
                    last_run_at: None,
                    last_run_status: None,
                    avg_duration_ms: None,
                })))
            } else {
                error!("Failed to query workflow stats: {}", e);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to query workflow stats: {}", e))),
                ))
            }
        }
    }
}

/// Request body for running a sequence of workflows
#[derive(Deserialize)]
pub struct RunWorkflowSequenceRequest {
    workflow_ids: Vec<String>,
    #[serde(default = "default_stop_on_failure")]
    stop_on_failure: bool,
}

pub fn default_stop_on_failure() -> bool {
    true
}

/// Response for workflow sequence execution
#[derive(Serialize)]
pub struct WorkflowSequenceResponse {
    task_run_id: String,
    workflow_count: usize,
    workflow_names: Vec<String>,
}

/// Run a sequence of unified workflows
pub async fn run_workflow_sequence(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunWorkflowSequenceRequest>,
) -> Result<Json<ApiResponse<WorkflowSequenceResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    if request.workflow_ids.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("workflow_ids cannot be empty".to_string())),
        ));
    }

    info!(
        "Running workflow sequence: {} workflows, stop_on_failure={}",
        request.workflow_ids.len(),
        request.stop_on_failure
    );

    // Fetch all workflows and validate they exist
    let mut workflows: Vec<crate::unified_workflows::UnifiedWorkflow> = Vec::new();
    for id in &request.workflow_ids {
        match state.app_state.checkpoint_db.get_unified_workflow(id) {
            Ok(Some(w)) => workflows.push(w),
            Ok(None) => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(api_error(format!("Workflow not found: {}", id))),
                ));
            }
            Err(e) => {
                error!("Failed to get workflow {}: {}", id, e);
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to get workflow: {}", e))),
                ));
            }
        }
    }

    let workflow_names: Vec<String> = workflows.iter().map(|w| w.name.clone()).collect();
    let sequence_name = if workflows.len() == 1 {
        workflows[0].name.clone()
    } else {
        format!(
            "Sequence: {} + {} more",
            workflows[0].name,
            workflows.len() - 1
        )
    };

    // Create a combined prompt describing the sequence
    let sequence_description = workflows
        .iter()
        .enumerate()
        .map(|(i, w)| format!("{}. {} - {}", i + 1, w.name, w.description))
        .collect::<Vec<_>>()
        .join("\n");

    let combined_prompt = format!(
        "Execute the following workflow sequence{}:\n\n{}\n\nTotal workflows: {}",
        if request.stop_on_failure {
            " (stopping on first failure)"
        } else {
            ""
        },
        sequence_description,
        workflows.len()
    );

    // Create a single task run for the sequence
    let execution_id = format!(
        "workflow-sequence-{}",
        chrono::Utc::now().timestamp_millis()
    );

    // Build combined steps from all workflows
    let mut all_steps: Vec<crate::step_executor::ExecutionStepConfig> = Vec::new();
    let monitor_index = 0i32;

    for (workflow_idx, workflow) in workflows.iter().enumerate() {
        info!(
            "Adding workflow {}/{}: {} ({} setup, {} verification, {} agentic, {} completion steps)",
            workflow_idx + 1,
            workflows.len(),
            workflow.name,
            workflow.setup_steps.len(),
            workflow.verification_steps.len(),
            workflow.agentic_steps.len(),
            workflow.completion_steps.len()
        );

        // Helper to convert Value steps to ExecutionStepConfig (same as run_unified_workflow)
        let convert_step = |step: &serde_json::Value,
                            _monitor: i32|
         -> Option<crate::step_executor::ExecutionStepConfig> {
            if let Ok(config) =
                serde_json::from_value::<crate::step_executor::ExecutionStepConfig>(step.clone())
            {
                return Some(config);
            }

            let step_type = step.get("type").and_then(|t| t.as_str())?;
            let name = step
                .get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string());

            Some(crate::step_executor::ExecutionStepConfig {
                step_type: step_type.to_string(),
                name,
                ..Default::default()
            })
        };

        // Add steps from each phase
        for step in &workflow.setup_steps {
            if let Some(mut config) = convert_step(step, monitor_index) {
                config.phase = Some("setup".to_string());
                all_steps.push(config);
            }
        }

        for step in &workflow.verification_steps {
            if let Some(mut config) = convert_step(step, monitor_index) {
                config.phase = Some("verification".to_string());
                all_steps.push(config);
            }
        }

        for step in &workflow.agentic_steps {
            if let Some(mut config) = convert_step(step, monitor_index) {
                config.phase = Some("agentic".to_string());
                all_steps.push(config);
            }
        }

        for step in &workflow.completion_steps {
            if let Some(mut config) = convert_step(step, monitor_index) {
                config.phase = Some("completion".to_string());
                all_steps.push(config);
            }
        }
    }

    // Create task run for tracking
    let execution_steps_json = serde_json::to_string(&all_steps).ok();
    let mut input = CreateTaskRunInput::new(&execution_id, &sequence_name)
        .with_prompt(&combined_prompt)
        .with_task_type("ai")
        .with_workflow_name(workflow_names.join(", "))
        .with_auto_continue(true)
        .with_workflow_type("unified");
    if let Some(esj) = execution_steps_json {
        input = input.with_execution_steps_json(esj);
    }
    if let Err(e) = state.app_state.checkpoint_db.create_task_run(&input) {
        warn!("Failed to create task_run for sequence: {}", e);
    }

    // Capture values for response before moving workflows into the async block
    let workflow_count = workflows.len();
    let workflow_names_response = workflow_names.clone();

    // Spawn background task to execute workflow sequence with panic protection
    let state_clone = state.clone();
    let execution_id_clone = execution_id.clone();
    let stop_on_failure = request.stop_on_failure;
    let checkpoint_db_for_guard = state.app_state.checkpoint_db.clone();
    let sequence_name_for_guard = format!("Workflow Sequence ({} workflows)", workflow_count);
    let execution_id_for_guard = execution_id.clone();
    let url_lock_for_sequence = Some(state.app_state.url_lock_manager.clone());

    // Use panic-safe spawning to ensure task is marked as failed if sequence panics
    crate::unified_workflow_executor::spawn_sequence_with_panic_guard(
        checkpoint_db_for_guard,
        execution_id_for_guard,
        sequence_name_for_guard,
        url_lock_for_sequence,
        async move {
            info!(
                "Starting workflow sequence execution: {} workflows",
                workflow_count
            );

            let session_manager: Arc<crate::claude_session::SessionManager> = state_clone
                .app_handle
                .state::<Arc<crate::claude_session::SessionManager>>()
                .inner()
                .clone();
            let mut controller = crate::unified_workflow_executor::LoopController::new(
                state_clone.app_state.clone(),
                state_clone.config_storage.clone(),
                state_clone.app_handle.clone(),
                state_clone.current_ai_pids.clone(),
            )
            .with_session_manager(session_manager);

            let mut all_results: Vec<crate::step_executor::StepExecutionResult> = Vec::new();
            let mut sequence_success = true;
            let mut failed_workflow: Option<String> = None;

            for (idx, workflow) in workflows.iter().enumerate() {
                info!(
                    "=== Executing workflow {}/{}: {} ===",
                    idx + 1,
                    workflow_count,
                    workflow.name
                );

                // Convert workflow steps to ExecutionStepConfig
                let monitor_index = 0i32;
                let convert_step =
                    |step: &serde_json::Value,
                     _monitor: i32|
                     -> Option<crate::step_executor::ExecutionStepConfig> {
                        if let Ok(config) = serde_json::from_value::<
                            crate::step_executor::ExecutionStepConfig,
                        >(step.clone())
                        {
                            return Some(config);
                        }

                        let step_type = step.get("type").and_then(|t| t.as_str())?;
                        let name = step
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string());

                        Some(crate::step_executor::ExecutionStepConfig {
                            step_type: step_type.to_string(),
                            name,
                            ..Default::default()
                        })
                    };

                // Collect all steps with phases
                let mut workflow_steps: Vec<crate::step_executor::ExecutionStepConfig> = Vec::new();

                for step in &workflow.setup_steps {
                    if let Some(mut config) = convert_step(step, monitor_index) {
                        config.phase = Some("setup".to_string());
                        workflow_steps.push(config);
                    }
                }
                for step in &workflow.verification_steps {
                    if let Some(mut config) = convert_step(step, monitor_index) {
                        config.phase = Some("verification".to_string());
                        workflow_steps.push(config);
                    }
                }
                for step in &workflow.agentic_steps {
                    if let Some(mut config) = convert_step(step, monitor_index) {
                        config.phase = Some("agentic".to_string());
                        workflow_steps.push(config);
                    }
                }
                for step in &workflow.completion_steps {
                    if let Some(mut config) = convert_step(step, monitor_index) {
                        config.phase = Some("completion".to_string());
                        workflow_steps.push(config);
                    }
                }

                // Separate automation from prompt steps
                let (automation_steps, prompt_steps) =
                    categorize_steps(workflow_steps, |s| &s.step_type);

                // Check if this is an AI workflow (has prompt steps)
                let has_prompt_steps = !prompt_steps.is_empty();

                if has_prompt_steps {
                    // Separate by phase
                    let setup_automation: Vec<_> = automation_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("setup"))
                        .cloned()
                        .collect();
                    // Prepend pre-flight check if enabled (default: true)
                    let setup_automation = crate::unified_workflows::prepend_preflight_check_step(
                        setup_automation,
                        workflow.preflight_check_enabled,
                    );
                    let setup_prompts: Vec<_> = prompt_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("setup"))
                        .cloned()
                        .collect();
                    let verification: Vec<_> = automation_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("verification"))
                        .cloned()
                        .collect();
                    let agentic: Vec<_> = prompt_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("agentic"))
                        .cloned()
                        .collect();
                    let completion_automation: Vec<_> = automation_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("completion"))
                        .cloned()
                        .collect();
                    let completion_prompts: Vec<_> = prompt_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("completion"))
                        .cloned()
                        .collect();

                    // Build prompt content
                    let prompt_content = prompt_steps
                        .iter()
                        .filter_map(|s| s.prompt_content.as_ref())
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n---\n\n");

                    // Create workflow-specific execution ID for internal tracking
                    // Note: We don't create a separate task_run row for each workflow in a sequence
                    // The parent sequence task_run is sufficient for tracking. Creating child task_runs
                    // caused duplicate entries to appear in the running tasks list.
                    let workflow_exec_id = format!("{}-workflow-{}", execution_id_clone, idx + 1);

                    // For error-fix workflows, run agentic first
                    let run_agentic_first = !workflow.targeted_error_ids.is_empty();

                    let loop_config = crate::unified_workflow_executor::LoopConfig {
                        max_iterations: workflow.max_iterations,
                        base_prompt: prompt_content,
                        workflow_name: workflow.name.clone(),
                        workflow_id: workflow.id.clone(),
                        execution_id: workflow_exec_id,
                        targeted_error_ids: workflow.targeted_error_ids.clone(),
                        starting_iteration: 0, // Fresh start
                        run_agentic_first,
                        artifact_dir: None,
                        is_dev_mode: cfg!(debug_assertions),
                        enable_sweep: workflow.enable_sweep,
                        max_sweep_iterations: workflow.max_sweep_iterations,
                    };

                    let result = controller
                        .run(
                            loop_config,
                            setup_automation,
                            setup_prompts,
                            verification,
                            agentic,
                            completion_automation,
                            completion_prompts,
                        )
                        .await;

                    all_results.extend(result.step_results);

                    if !result.success {
                        sequence_success = false;
                        failed_workflow = Some(workflow.name.clone());
                        error!("Workflow '{}' failed in sequence", workflow.name);

                        if stop_on_failure {
                            info!(
                                "Stopping sequence due to workflow failure (stop_on_failure=true)"
                            );
                            break;
                        }
                    } else {
                        info!("Workflow '{}' completed successfully", workflow.name);
                    }
                } else {
                    // Automation-only workflow - use StepExecutor
                    let executor = crate::step_executor::StepExecutor::with_app_handle(
                        state_clone.app_state.clone(),
                        state_clone.config_storage.clone(),
                        state_clone.app_handle.clone(),
                    );

                    let workflow_exec_id = format!("{}-workflow-{}", execution_id_clone, idx + 1);

                    let result = executor
                        .execute_steps_with_log_sources(&automation_steps, &workflow_exec_id, &[])
                        .await;

                    all_results.extend(result.steps);

                    if !result.success {
                        sequence_success = false;
                        failed_workflow = Some(workflow.name.clone());
                        error!("Automation workflow '{}' failed in sequence", workflow.name);

                        if stop_on_failure {
                            info!(
                                "Stopping sequence due to workflow failure (stop_on_failure=true)"
                            );
                            break;
                        }
                    } else {
                        info!(
                            "Automation workflow '{}' completed successfully",
                            workflow.name
                        );
                    }
                }
            }

            // Update task_run status
            if sequence_success {
                info!("Workflow sequence completed successfully");
                let _ = state_clone
                    .app_state
                    .checkpoint_db
                    .complete_task_run(&execution_id_clone);
            } else {
                let error_msg = match failed_workflow {
                    Some(name) => format!("Workflow '{}' failed", name),
                    None => "Sequence failed".to_string(),
                };
                error!("Workflow sequence failed: {}", error_msg);
                let _ = state_clone
                    .app_state
                    .checkpoint_db
                    .fail_task_run(&execution_id_clone, &error_msg);
            }

            // Fire-and-forget summary generation for the sequence task
            let db = state_clone.app_state.checkpoint_db.clone();
            let exec_id = execution_id_clone.clone();
            let doctor_handle = state_clone.doctor_handle.clone();
            tokio::spawn(async move {
                match crate::summary_generator::generate_task_summary_async(
                    db,
                    exec_id.clone(),
                    doctor_handle,
                )
                .await
                {
                    Ok(_) => info!("Generated summary for sequence task {}", exec_id),
                    Err(e) => warn!(
                        "Failed to generate summary for sequence task {}: {}",
                        exec_id, e
                    ),
                }
            });
        },
    );

    Ok(Json(ApiResponse::success(WorkflowSequenceResponse {
        task_run_id: execution_id,
        workflow_count,
        workflow_names: workflow_names_response,
    })))
}

// ============================================================================
// Example Status API
// ============================================================================

#[derive(Deserialize)]
struct UpdateExampleStatusRequest {
    status: String,
}

async fn update_example_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateExampleStatusRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    use crate::workflow_generation::example_workflows;

    let valid_statuses = ["active", "excluded", "pending"];
    if !valid_statuses.contains(&request.status.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "Invalid status '{}'. Must be one of: active, excluded, pending",
                request.status
            ))),
        ));
    }

    state
        .app_state
        .checkpoint_db
        .with_conn(|conn| match request.status.as_str() {
            "active" => example_workflows::promote_workflow_to_example(conn, &id),
            "excluded" => example_workflows::exclude_workflow_from_examples(conn, &id),
            "pending" => example_workflows::remove_workflow_from_examples(conn, &id),
            _ => unreachable!(),
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update example status: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(())))
}

// ============================================================================
// End Unified Workflows HTTP API Handlers
// ============================================================================

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/unified-workflows",
            get(list_unified_workflows).post(create_unified_workflow),
        )
        // Literal paths must come before :id catch-all
        .route("/unified-workflows/search", get(search_unified_workflows))
        .route("/unified-workflows/import", post(import_unified_workflow))
        .route(
            "/unified-workflows/generate",
            post(generate_unified_workflow_handler),
        )
        .route(
            "/unified-workflows/generate-async",
            post(generate_unified_workflow_async_handler),
        )
        .route(
            "/unified-workflows/execute-inline",
            post(execute_inline_workflow),
        )
        .route(
            "/unified-workflows/last-inline",
            get(get_last_inline_workflow),
        )
        .route(
            "/unified-workflows/run-sequence",
            post(run_workflow_sequence),
        )
        // Parameterized paths after all literal paths
        .route(
            "/unified-workflows/:id",
            get(get_unified_workflow)
                .put(update_unified_workflow)
                .delete(delete_unified_workflow),
        )
        .route(
            "/unified-workflows/:id/duplicate",
            post(duplicate_unified_workflow),
        )
        .route(
            "/unified-workflows/:id/export",
            get(export_unified_workflow),
        )
        .route("/unified-workflows/:id/run", post(run_unified_workflow))
        .route(
            "/unified-workflows/:id/stats",
            get(get_unified_workflow_stats),
        )
        .route(
            "/unified-workflows/:id/example-status",
            axum::routing::put(update_example_status_handler),
        )
        .route("/execute-plan", post(execute_plan))
}
