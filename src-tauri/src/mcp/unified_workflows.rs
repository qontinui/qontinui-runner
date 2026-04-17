//! Unified workflow handlers for MCP API
//!
//! Provides HTTP handlers for unified workflow CRUD operations,
//! execution (single, inline, composed multi-stage), generation, and stats.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tracing::{debug, error, info, warn};

use crate::database::CreateTaskRunInput;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::unified_workflows::UnifiedWorkflowExt;
use crate::workflow_generation;

#[derive(Debug, Serialize, Deserialize)]
pub struct GenerateWorkflowAsyncResponse {
    pub task_run_id: String,
    pub meta_workflow_id: String,
}

pub fn refetch_unified_workflow_steps(
    task_id: &str,
    cached_steps_json: Option<String>,
    db: &crate::database::pg::PgDb,
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

    // Fetch workflow from database (async call from sync context)
    let workflow_result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(db.get_unified_workflow(&workflow_id))
    });
    match workflow_result {
        Ok(Some(workflow)) => {
            use crate::step_executor::ExecutionStepConfig;
            let mut all_steps: Vec<ExecutionStepConfig> = Vec::new();

            // Helper closure to convert step
            let convert_step = |step: &serde_json::Value| -> Option<ExecutionStepConfig> {
                debug!(
                    "refetch_unified_workflow_steps: converting step: {}",
                    serde_json::to_string(step).unwrap_or_else(|_| "ERROR".to_string())
                );

                // Try direct deserialization first
                if let Ok(config) = serde_json::from_value::<ExecutionStepConfig>(step.clone()) {
                    debug!(
                        "refetch_unified_workflow_steps: serde succeeded, check_type={:?}",
                        config.check_type
                    );
                    return Some(config);
                }

                debug!("refetch_unified_workflow_steps: serde failed, using manual extraction");

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

            // Normalize to stages to handle both flat and multi-stage workflows
            let normalized_stages = workflow.normalize_to_stages();
            for stage in &normalized_stages {
                for step in &stage.setup_steps {
                    if let Some(mut config) = convert_step(step) {
                        config.phase = Some("setup".to_string());
                        all_steps.push(config);
                    }
                }
                for step in &stage.verification_steps {
                    if let Some(mut config) = convert_step(step) {
                        config.phase = Some("verification".to_string());
                        all_steps.push(config);
                    }
                }
                for step in &stage.agentic_steps {
                    if let Some(mut config) = convert_step(step) {
                        config.phase = Some("agentic".to_string());
                        all_steps.push(config);
                    }
                }
                for step in &stage.completion_steps {
                    if let Some(mut config) = convert_step(step) {
                        config.phase = Some("completion".to_string());
                        all_steps.push(config);
                    }
                }
            }

            info!(
                "Re-fetched {} steps from unified workflow definition",
                all_steps.len()
            );

            // Update the task_run with the correct execution_steps_json
            if let Ok(new_json) = serde_json::to_string(&all_steps) {
                let update_result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(db.update_task_run_execution_steps(task_id, &new_json))
                });
                if let Err(e) = update_result {
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
async fn push_to_backend(workflow: crate::unified_workflows::UnifiedWorkflow) {
    let client = crate::mcp::web_backend_workflows::WebBackendWorkflowClient::new();
    match client.save_workflow(&workflow).await {
        Ok(_) => {
            info!("Synced workflow '{}' to web backend", workflow.name);
        }
        Err(e) => {
            warn!(
                "Failed to push workflow '{}' to backend (will sync later): {}",
                workflow.name, e
            );
        }
    }
}

/// Update a workflow on the web backend (best-effort).
/// On failure, marks the local workflow as sync_pending.
async fn update_on_backend(workflow: crate::unified_workflows::UnifiedWorkflow) {
    let client = crate::mcp::web_backend_workflows::WebBackendWorkflowClient::new();
    match client.update_workflow(&workflow.id, &workflow).await {
        Ok(_) => {
            info!("Synced workflow update '{}' to web backend", workflow.name);
        }
        Err(e) => {
            warn!(
                "Failed to push workflow update '{}' to backend (will sync later): {}",
                workflow.name, e
            );
        }
    }
}

/// Delete a workflow from the web backend (best-effort).
async fn delete_from_backend(id: String) {
    let client = crate::mcp::web_backend_workflows::WebBackendWorkflowClient::new();
    if let Err(e) = client.delete_workflow(&id).await {
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
    match state.app_state.pg_db.list_unified_workflows().await {
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
    match state.app_state.pg_db.get_unified_workflow(&id).await {
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
            let create_req =
                crate::unified_workflows::unified_workflow_to_create_request(&workflow);
            if let Err(e) = state
                .app_state
                .pg_db
                .create_unified_workflow_with_id(&workflow.id, &create_req)
                .await
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
        .pg_db
        .create_unified_workflow(&request)
        .await
    {
        Ok(created) => {
            info!(
                "Created unified workflow: {} ({})",
                created.name, created.id
            );
            // Push to web backend (best-effort, fire-and-forget)
            tokio::spawn(push_to_backend(created.clone()));
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
        .pg_db
        .get_unified_workflow(&id)
        .await
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

            // Record structured feedback via PG
            let pg = state.app_state.pg_db.clone();
            let wf_id = id.clone();
            let trid = task_run_id.clone();
            let cat = existing.category.clone();
            let desc = existing.description.clone();
            tokio::spawn(async move {
                if let Err(e) = pg
                    .record_generation_feedback(
                        &wf_id,
                        Some(&trid),
                        "edit",
                        Some("workflow_update"),
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(&cat),
                        Some(&desc),
                    )
                    .await
                {
                    tracing::warn!("Failed to record edit feedback: {}", e);
                }
            });
        }
    }

    match state
        .app_state
        .pg_db
        .update_unified_workflow(&id, &request)
        .await
    {
        Ok(updated) => {
            info!(
                "Updated unified workflow: {} ({})",
                updated.name, updated.id
            );
            // Push update to web backend (best-effort, fire-and-forget)
            tokio::spawn(update_on_backend(updated.clone()));
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
    let existing_for_del = state
        .app_state
        .pg_db
        .get_unified_workflow(&id)
        .await
        .ok()
        .flatten();
    if let Some(existing) = existing_for_del {
        if let Some(ref task_run_id) = existing.generated_by_task_run_id {
            info!(
                "Workflow '{}' (generated by task run '{}') is being deleted — \
                 this is a post-generation feedback event indicating the user \
                 rejected the generated workflow",
                existing.name, task_run_id
            );

            // Record structured feedback via PG
            let pg = state.app_state.pg_db.clone();
            let wf_id = id.clone();
            let trid = task_run_id.clone();
            let cat = existing.category.clone();
            let desc = existing.description.clone();
            tokio::spawn(async move {
                if let Err(e) = pg
                    .record_generation_feedback(
                        &wf_id,
                        Some(&trid),
                        "delete",
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(&cat),
                        Some(&desc),
                    )
                    .await
                {
                    tracing::warn!("Failed to record delete feedback: {}", e);
                }
            });
        }
    }

    match state.app_state.pg_db.delete_unified_workflow(&id).await {
        Ok(true) => {
            // Delete from web backend (best-effort, fire-and-forget)
            tokio::spawn(delete_from_backend(id.clone()));
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
    let q = query.q.as_deref().unwrap_or("");
    match state.app_state.pg_db.search_unified_workflows(q).await {
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

    // Implement duplicate via get + create with new ID
    let original = match state.app_state.pg_db.get_unified_workflow(&id).await {
        Ok(Some(w)) => w,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Unified workflow not found: {}", id))),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get workflow: {}", e))),
            ));
        }
    };

    let mut create_req = crate::unified_workflows::unified_workflow_to_create_request(&original);
    create_req.name = format!("{} (Copy)", original.name);

    match state
        .app_state
        .pg_db
        .create_unified_workflow(&create_req)
        .await
    {
        Ok(duplicated) => {
            info!("Duplicated unified workflow: {} -> {}", id, duplicated.id);
            tokio::spawn(push_to_backend(duplicated.clone()));
            Ok(Json(ApiResponse::success(duplicated)))
        }
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

    match state.app_state.pg_db.get_unified_workflow(&id).await {
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
        .pg_db
        .get_unified_workflow(&workflow.id)
        .await
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
                    .pg_db
                    .delete_unified_workflow(&workflow.id)
                    .await
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
    let create_request = crate::unified_workflows::unified_workflow_to_create_request(&workflow);

    // Use the database's create function but with our custom ID
    match state
        .app_state
        .pg_db
        .create_unified_workflow_with_id(&workflow.id, &create_request)
        .await
    {
        Ok(created) => {
            info!(
                "Imported unified workflow: {} ({}) [overwritten: {}]",
                created.name, created.id, overwritten
            );
            // Push imported workflow to web backend (best-effort, fire-and-forget)
            tokio::spawn(push_to_backend(created.clone()));
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

    // Inject observation memory into inline_context (async, before blocking spawn)
    let mut request = request;
    if let Some(memory) = crate::context::format_observation_memory_for_prompt(
        &state.app_state.pg_db,
        None,
        Some(&request.description),
    )
    .await
    {
        let existing = request.inline_context.unwrap_or_default();
        request.inline_context = Some(format!("{}{}", memory, existing));
    }

    // Clone handles so they can be moved into the blocking closure
    let doctor_handle = state.doctor_handle.clone();
    let pg_db = state.app_state.pg_db.clone();
    let pg_clone = pg_db.clone();

    let (result, artifact) = tokio::task::spawn_blocking(move || {
        let (response, artifact) = workflow_generation::generate_workflow(
            request,
            doctor_handle.as_ref(),
            Some(&pg_clone),
            None,
        );
        (response, artifact)
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

    // Save pipeline artifact to PG (async)
    if let Err(e) = pg_db.save_generation_artifact(&artifact).await {
        tracing::warn!("Failed to save pipeline artifact to PG: {}", e);
    }

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
    use crate::workflow_generation::meta_workflow::build_meta_workflow_template;

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

    // Build historical context from PG (self-improvement patterns, similar workflows, GT refs)
    let historical_context =
        crate::workflow_generation::meta_workflow::build_historical_context_pg(
            &state.app_state.pg_db,
            &request.description,
            request.category.as_deref(),
        )
        .await;

    // Build the meta-workflow
    let meta_workflow =
        build_meta_workflow_template(&request, &resolved_contexts, historical_context.as_ref());

    // Save the meta-workflow to database (SQLite + PG)
    let mut create_request =
        crate::unified_workflows::unified_workflow_to_create_request(&meta_workflow);
    create_request.generated_by_task_run_id = None; // Will be set after task run creation

    let saved_workflow = match state
        .app_state
        .pg_db
        .create_unified_workflow(&create_request)
        .await
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
    let simple_prompt: String = saved_workflow
        .agentic_steps
        .iter()
        .filter_map(|step| step.get("content").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    let port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    let task_run_input = CreateTaskRunInput::new(&task_run_id, &saved_workflow.name)
        .with_task_type("ai")
        .with_prompt(&simple_prompt)
        .with_workflow_type("unified")
        .with_workflow_name(&saved_workflow.name)
        .with_workflow_id(&saved_workflow.id)
        .with_max_sessions(saved_workflow.iter_cap())
        .with_auto_continue(true)
        .with_runner_port(port);

    if let Err(e) = state.app_state.pg_db.create_task_run(&task_run_input).await {
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
        let db = state.app_state.pg_db.clone();
        let tid = task_run_id.clone();
        tokio::spawn(async move {
            let sync_service = crate::commands::task_sync::AITaskSyncService::new();
            if let Ok(Some(task)) = db.get_task_run(&tid).await {
                if let Err(e) = sync_service.sync_task_created(&task, None).await {
                    warn!("Failed to sync task creation to backend: {}", e);
                }
            }
        });
    }

    // Check for DAG workflow routing — if source_yaml exists, use DAG driver
    if let Some(dag_def) =
        crate::workflow::dag_routing::check_dag_routing(&saved_workflow.id, &state.app_state.pg_db)
            .await
    {
        let dag_deps = crate::workflow::dag_driver::DagDriverDeps {
            app_state: state.app_state.clone(),
            config_storage: state.config_storage.clone(),
            app_handle: Some(state.app_handle.clone()),
            execution_id: task_run_id.clone(),
            task_run_id: Some(task_run_id.clone()),
        };

        let exec_id = task_run_id.clone();
        let pg = state.app_state.pg_db.clone();
        tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result = crate::workflow::dag_driver::execute_dag_workflow(dag_def, dag_deps).await;
            let duration = start.elapsed().as_millis() as u64;
            let status = match &result {
                Ok(r) if r.success => "completed",
                Ok(_) => "failed",
                Err(_) => "failed",
            };
            let _ = pg.update_task_run_status(&exec_id, status).await;
            if let Ok(ref r) = result {
                if let Ok(metrics_json) = serde_json::to_string(&r.node_metrics) {
                    let _ = pg
                        .set_task_run_result_data(
                            &exec_id,
                            &serde_json::json!({ "dag_node_metrics": metrics_json }).to_string(),
                        )
                        .await;
                }
            }
            tracing::info!(execution_id = %exec_id, ?duration, ?status, "DAG workflow finished");
        });

        return Ok(Json(ApiResponse::success(GenerateWorkflowAsyncResponse {
            task_run_id,
            meta_workflow_id: saved_workflow.id.clone(),
        })));
    }

    // Normalize to stages — all workflows are now multi-stage
    {
        use crate::unified_workflow_executor::{LoopConfig, LoopController};

        let normalized_stages = saved_workflow.normalize_to_stages();
        let total_stages = normalized_stages.len();

        // Convert each stage to StageConfig
        let stages: Vec<crate::unified_workflow_executor::StageConfig> = normalized_stages
            .iter()
            .enumerate()
            .map(|(idx, stage)| {
                crate::unified_workflows::stage_to_stage_config(
                    stage,
                    idx,
                    total_stages,
                    saved_workflow.preflight_check_enabled,
                    saved_workflow.log_watch_enabled,
                    saved_workflow.health_check_enabled,
                    &saved_workflow.health_check_urls,
                )
            })
            .collect();

        // Build combined prompt from all agentic steps across all stages
        let combined_prompt = normalized_stages
            .iter()
            .flat_map(|stage| stage.agentic_steps.iter())
            .filter_map(|step| step.get("content").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        // For meta-workflows, run agentic first if there are targeted errors
        let run_agentic_first = !saved_workflow.targeted_error_ids.is_empty();

        let loop_config = LoopConfig::from_workflow(
            &saved_workflow,
            task_run_id.clone(),
            combined_prompt,
            stages,
            run_agentic_first,
            request.auto_run.unwrap_or(false),
        );

        // Clone state fields for the background task
        let app_state = state.app_state.clone();
        let config_storage = state.config_storage.clone();
        let app_handle = state.app_handle.clone();
        let pid_tracker = state.current_ai_pids.clone();
        let workflow_name = saved_workflow.name.clone();
        let execution_id_for_guard = task_run_id.clone();

        // Get session manager for interactive mode
        let session_manager: Arc<crate::claude_session::SessionManager> = state
            .app_handle
            .state::<Arc<crate::claude_session::SessionManager>>()
            .inner()
            .clone();

        // Check if Restate durable execution should be used
        let mut use_legacy = true;
        let restate_settings = crate::settings::load_settings().restate;
        if crate::restate::launch::should_use_restate(&restate_settings).await {
            match crate::restate::launch::build_workflow_input_from_loop_config(&loop_config) {
                Ok(input) => {
                    if let Err(e) = state
                        .app_state
                        .pg_db
                        .save_restate_workflow_execution(
                            &execution_id_for_guard,
                            &execution_id_for_guard,
                            None,
                        )
                        .await
                    {
                        tracing::error!("Failed to record Restate workflow: {}", e);
                    }

                    if let Err(e) = crate::restate::launch::launch_workflow_via_restate(
                        &execution_id_for_guard,
                        &input,
                        &restate_settings.ingress_url(),
                    )
                    .await
                    {
                        tracing::error!("Restate launch failed, falling back to legacy: {}", e);
                    } else {
                        use_legacy = false;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to build WorkflowInput for Restate: {}", e);
                }
            }
        }

        // Spawn the workflow executor in the background with panic protection
        if use_legacy {
            let url_lock = Some(state.app_state.url_lock_manager.clone());
            let file_registry = Some(state.app_state.file_registry_manager.clone());
            let file_lock = Some(state.app_state.file_lock_manager.clone());
            crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
                execution_id_for_guard,
                workflow_name,
                url_lock,
                file_registry,
                file_lock,
                state.app_state.pg_db.clone(),
                async move {
                    let mut controller =
                        LoopController::new(app_state, config_storage, app_handle, pid_tracker)
                            .with_session_manager(session_manager);

                    controller
                        .run(
                            loop_config,
                            Vec::new(), // No top-level steps — all in stages
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        )
                        .await
                },
            );
        }
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
    /// Optional runtime config overrides applied to the LoopConfig before execution:
    /// model, max_iterations, multi_agent_mode, max_context_tokens,
    /// workflow_architecture, agentic_verification_config, multi_agent_pipeline_config.
    #[serde(default)]
    overrides: Option<serde_json::Value>,
    /// Arguments to substitute for $ARGUMENTS in slash command workflows.
    #[serde(default)]
    pub arguments: Option<String>,
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
    /// Maximum iterations for agentic phase.
    /// `None` (omitted) means no cap — loop until success or explicit stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_iterations: Option<u32>,
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
    /// Optional stages for multi-stage workflows
    #[serde(default)]
    stages: Vec<crate::unified_workflows::WorkflowStage>,
    /// Category of the workflow (e.g. "plan-generated", "spec-generated", "error-fix")
    #[serde(default)]
    category: Option<String>,
    /// Optional config overrides (workflow_architecture, use_worktree, etc.)
    #[serde(default)]
    overrides: Option<serde_json::Value>,
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
    let mut workflow = match state.app_state.pg_db.get_unified_workflow(&id).await {
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

    // Substitute $ARGUMENTS in agentic step content if arguments are provided
    if let Some(ref args) = request.arguments {
        for step in workflow.agentic_steps.iter_mut() {
            if let Some(content) = step
                .get("content")
                .and_then(|c| c.as_str())
                .map(String::from)
            {
                if content.contains("$ARGUMENTS") {
                    step["content"] =
                        serde_json::Value::String(content.replace("$ARGUMENTS", args));
                    info!("Substituted $ARGUMENTS in agentic step");
                }
            }
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

    // Check for DAG workflow routing — if source_yaml exists, use DAG driver
    if let Some(dag_def) =
        crate::workflow::dag_routing::check_dag_routing(&id, &state.app_state.pg_db).await
    {
        let execution_id = format!(
            "unified-workflow-{}-{}",
            id,
            chrono::Utc::now().timestamp_millis()
        );

        let input = CreateTaskRunInput::new(&execution_id, &workflow.name)
            .with_prompt(format!("DAG workflow: {}", dag_def.name))
            .with_task_type("ai")
            .with_workflow_name(&workflow.name)
            .with_workflow_id(&workflow.id)
            .with_workflow_type("dag");
        let _ = state.app_state.pg_db.create_task_run(&input).await;

        let dag_deps = crate::workflow::dag_driver::DagDriverDeps {
            app_state: state.app_state.clone(),
            config_storage: state.config_storage.clone(),
            app_handle: Some(state.app_handle.clone()),
            execution_id: execution_id.clone(),
            task_run_id: Some(execution_id.clone()),
        };

        let exec_id = execution_id.clone();
        let pg = state.app_state.pg_db.clone();
        tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result = crate::workflow::dag_driver::execute_dag_workflow(dag_def, dag_deps).await;
            let duration = start.elapsed().as_millis() as u64;
            let status = match &result {
                Ok(r) if r.success => "completed",
                Ok(_) => "failed",
                Err(_) => "failed",
            };
            let _ = pg.update_task_run_status(&exec_id, status).await;
            if let Ok(ref r) = result {
                if let Ok(metrics_json) = serde_json::to_string(&r.node_metrics) {
                    let _ = pg
                        .set_task_run_result_data(
                            &exec_id,
                            &serde_json::json!({ "dag_node_metrics": metrics_json }).to_string(),
                        )
                        .await;
                }
            }
            tracing::info!(execution_id = %exec_id, ?duration, ?status, "DAG workflow finished");
        });

        return Ok(Json(ApiResponse::success(RunUnifiedWorkflowResponse {
            task_run_id: execution_id,
        })));
    }

    // Normalize to stages — all workflows are now multi-stage
    let normalized_stages = workflow.normalize_to_stages();
    let total_stages = normalized_stages.len();

    // Convert each stage to StageConfig
    let stages: Vec<crate::unified_workflow_executor::StageConfig> = normalized_stages
        .iter()
        .enumerate()
        .map(|(idx, stage)| {
            crate::unified_workflows::stage_to_stage_config(
                stage,
                idx,
                total_stages,
                workflow.preflight_check_enabled,
                workflow.log_watch_enabled,
                workflow.health_check_enabled,
                &workflow.health_check_urls,
            )
        })
        .collect();

    // Build combined prompt from all agentic steps across all stages
    let combined_prompt = normalized_stages
        .iter()
        .flat_map(|stage| stage.agentic_steps.iter())
        .filter_map(|step| step.get("content").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    // Check if any stage has prompt-based steps
    let has_prompt_steps = normalized_stages.iter().any(|stage| {
        stage
            .agentic_steps
            .iter()
            .any(|s| s.get("type").and_then(|t| t.as_str()) == Some("prompt"))
            || stage
                .setup_steps
                .iter()
                .any(|s| s.get("type").and_then(|t| t.as_str()) == Some("prompt"))
            || stage
                .verification_steps
                .iter()
                .any(|s| s.get("type").and_then(|t| t.as_str()) == Some("prompt"))
            || stage
                .completion_steps
                .iter()
                .any(|s| s.get("type").and_then(|t| t.as_str()) == Some("prompt"))
    });

    // Determine execution_id for resume support
    let execution_id = if let Some(ref provided_id) = request.task_run_id {
        info!(
            "Using provided task_run_id for explicit resume: {}",
            provided_id
        );
        provided_id.clone()
    } else if !request.force_fresh_start {
        // get_incomplete_task_run_for_workflow not on PgDb — always start fresh
        {
            let new_id = format!(
                "unified-workflow-{}-{}",
                id,
                chrono::Utc::now().timestamp_millis()
            );
            info!("Starting fresh execution: {}", new_id);
            new_id
        }
    } else {
        let new_id = format!(
            "unified-workflow-{}-{}",
            id,
            chrono::Utc::now().timestamp_millis()
        );
        info!("Force fresh start requested, new execution_id: {}", new_id);
        new_id
    };

    if has_prompt_steps {
        // AI-based execution with verification-agentic loop
        info!(
            "Workflow '{}' has prompt steps - using AI-based multi-stage execution ({} stages)",
            workflow.name, total_stages
        );

        // Clear canvas state from previous run
        {
            let mut canvas = state.app_state.canvas_state.write().await;
            canvas.clear();
        }
        let clear_event = crate::event_system::AppEvent::CanvasUpdate {
            action: "clear".to_string(),
            panel_id: String::new(),
            panel: None,
            task_run_id: Some(execution_id.clone()),
        };
        if let Err(e) = state
            .app_handle
            .emit(clear_event.event_name(), &clear_event)
        {
            warn!("Failed to emit canvas-clear event: {}", e);
        }

        // Create task_run for tracking
        let input = CreateTaskRunInput::new(&execution_id, &workflow.name)
            .with_prompt(&combined_prompt)
            .with_task_type("ai")
            .with_workflow_name(&workflow.name)
            .with_workflow_id(&workflow.id)
            .with_max_sessions(workflow.iter_cap())
            .with_auto_continue(true)
            .with_workflow_type("unified");
        let create_err = state.app_state.pg_db.create_task_run(&input).await.err();
        if let Some(e) = create_err {
            warn!(
                "Failed to create task_run for unified workflow {}: {}",
                execution_id, e
            );
        }

        // Run agentic first when: error-fix workflows, or workflows with only agentic steps (no verification)
        let has_verification = !workflow.verification_steps.is_empty();
        let has_agentic = !workflow.agentic_steps.is_empty();
        let run_agentic_first =
            !workflow.targeted_error_ids.is_empty() || (has_agentic && !has_verification);

        let mut loop_config = crate::unified_workflow_executor::LoopConfig::from_workflow(
            &workflow,
            execution_id.clone(),
            combined_prompt,
            stages,
            run_agentic_first,
            false,
        );

        // Apply runtime config overrides if present
        if let Some(ref overrides) = request.overrides {
            if let Some(model) = overrides.get("model").and_then(|v| v.as_str()) {
                loop_config.model_override = Some(model.to_string());
            }
            if let Some(max_iter) = overrides.get("max_iterations").and_then(|v| v.as_u64()) {
                loop_config.max_iterations = max_iter as u32;
            }
            if let Some(multi) = overrides.get("multi_agent_mode").and_then(|v| v.as_bool()) {
                loop_config.multi_agent_mode = multi;
            }
            if let Some(tokens) = overrides.get("max_context_tokens").and_then(|v| v.as_u64()) {
                loop_config.max_context_tokens = tokens as usize;
            }
            if let Some(arch) = overrides.get("workflow_architecture") {
                if let Ok(parsed) = serde_json::from_value::<
                    crate::agentic_verification::WorkflowArchitecture,
                >(arch.clone())
                {
                    loop_config.workflow_architecture = Some(parsed);
                }
            }
            if let Some(av_config) = overrides.get("agentic_verification_config") {
                if let Ok(parsed) = serde_json::from_value::<
                    crate::agentic_verification::AgenticVerificationConfig,
                >(av_config.clone())
                {
                    loop_config.agentic_verification_config = Some(parsed);
                }
            }
            if let Some(map_config) = overrides.get("multi_agent_pipeline_config") {
                if let Ok(parsed) = serde_json::from_value::<
                    crate::agentic_verification::MultiAgentPipelineConfig,
                >(map_config.clone())
                {
                    loop_config.multi_agent_pipeline_config = Some(parsed);
                }
            }
            if let Some(wt) = overrides.get("use_worktree").and_then(|v| v.as_bool()) {
                loop_config.use_worktree = wt;
            }
            if let Some(raf) = overrides.get("run_agentic_first").and_then(|v| v.as_bool()) {
                loop_config.run_agentic_first = raf;
            }
            info!(
                "Applied runtime config overrides to loop_config: {}",
                overrides
            );
        }

        let execution_id_for_guard = execution_id.clone();
        let workflow_name_for_guard = workflow.name.clone();
        let app_state = state.app_state.clone();
        let config_storage = state.config_storage.clone();
        let app_handle = state.app_handle.clone();
        let pid_tracker = state.current_ai_pids.clone();
        let url_lock = Some(state.app_state.url_lock_manager.clone());
        let file_registry = Some(state.app_state.file_registry_manager.clone());
        let file_lock = Some(state.app_state.file_lock_manager.clone());

        // Check if Restate durable execution should be used
        let mut use_legacy = true;
        let restate_settings = crate::settings::load_settings().restate;
        if crate::restate::launch::should_use_restate(&restate_settings).await {
            match crate::restate::launch::build_workflow_input_from_loop_config(&loop_config) {
                Ok(input) => {
                    if let Err(e) = state
                        .app_state
                        .pg_db
                        .save_restate_workflow_execution(
                            &execution_id_for_guard,
                            &execution_id_for_guard,
                            None,
                        )
                        .await
                    {
                        tracing::error!("Failed to record Restate workflow: {}", e);
                    }

                    if let Err(e) = crate::restate::launch::launch_workflow_via_restate(
                        &execution_id_for_guard,
                        &input,
                        &restate_settings.ingress_url(),
                    )
                    .await
                    {
                        tracing::error!("Restate launch failed, falling back to legacy: {}", e);
                    } else {
                        use_legacy = false;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to build WorkflowInput for Restate: {}", e);
                }
            }
        }

        if use_legacy {
            crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
                execution_id_for_guard,
                workflow_name_for_guard,
                url_lock,
                file_registry,
                file_lock,
                app_state.pg_db.clone(),
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
                            Vec::new(), // No top-level steps — all in stages
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        )
                        .await
                },
            );
        }

        return Ok(Json(ApiResponse::success(RunUnifiedWorkflowResponse {
            task_run_id: execution_id,
        })));
    }

    // No prompt steps - automation-only workflow
    // Still normalize to stages, but use step executor for simple execution
    let execution_steps_json = {
        let all_auto_steps: Vec<_> = stages
            .iter()
            .flat_map(|s| {
                s.setup_automation_steps
                    .iter()
                    .chain(s.verification_steps.iter())
                    .chain(s.completion_automation_steps.iter())
            })
            .cloned()
            .collect();
        serde_json::to_string(&all_auto_steps).ok()
    };

    // Clear canvas state from previous run
    {
        let mut canvas = state.app_state.canvas_state.write().await;
        canvas.clear();
    }
    let clear_event = crate::event_system::AppEvent::CanvasUpdate {
        action: "clear".to_string(),
        panel_id: String::new(),
        panel: None,
        task_run_id: Some(execution_id.clone()),
    };
    if let Err(e) = state
        .app_handle
        .emit(clear_event.event_name(), &clear_event)
    {
        warn!("Failed to emit canvas-clear event: {}", e);
    }

    let mut input = CreateTaskRunInput::new(&execution_id, &workflow.name)
        .with_task_type("automation")
        .with_workflow_name(&workflow.name)
        .with_workflow_id(&workflow.id);
    if let Some(esj) = execution_steps_json {
        input = input.with_execution_steps_json(esj);
    }
    let cr_err = state.app_state.pg_db.create_task_run(&input).await.err();
    if let Some(e) = cr_err {
        warn!(
            "Failed to create task_run for unified workflow {}: {}",
            execution_id, e
        );
    }

    let response_task_run_id = execution_id.clone();
    let execution_id_for_guard = execution_id.clone();
    let workflow_name_for_guard = workflow.name.clone();
    let app_state = state.app_state.clone();
    let config_storage = state.config_storage.clone();
    let app_handle = state.app_handle.clone();
    let url_lock = Some(state.app_state.url_lock_manager.clone());
    let file_registry = Some(state.app_state.file_registry_manager.clone());
    let file_lock = Some(state.app_state.file_lock_manager.clone());

    crate::unified_workflow_executor::spawn_sequence_with_panic_guard(
        execution_id_for_guard,
        workflow_name_for_guard,
        url_lock,
        file_registry,
        file_lock,
        app_state.pg_db.clone(),
        async move {
            let executor = crate::step_executor::StepExecutor::with_app_handle(
                app_state.clone(),
                config_storage,
                app_handle,
            );

            // Collect all automation steps from stages
            let automation_steps: Vec<_> = stages
                .into_iter()
                .flat_map(|s| {
                    s.setup_automation_steps
                        .into_iter()
                        .chain(s.verification_steps.into_iter())
                        .chain(s.completion_automation_steps.into_iter())
                })
                .collect();

            let result = executor
                .execute_steps_with_log_sources(&automation_steps, &execution_id, &[])
                .await;

            info!(
                "Unified workflow automation completed: {} of {} steps succeeded",
                result.successful_steps, result.total_steps
            );

            if result.success {
                let complete_err = app_state.pg_db.complete_task_run(&execution_id).await.err();
                if let Some(e) = complete_err {
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
                    .pg_db
                    .fail_task_run(&execution_id, error_msg)
                    .await
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

    // Slow path: check DB settings table (survives runner restart)
    match state
        .app_state
        .pg_db
        .get_setting("last_inline_workflow")
        .await
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
        // has_running_error_fix_workflow not on PgDb — skip duplicate check
        // (could be added later using get_running_task_runs + name filter)
    }

    // Store the request for re-execution via "Run Last Workflow" button
    // Persist to both in-memory cache (fast path) and SQLite (survives restart)
    if let Ok(request_json) = serde_json::to_value(&request) {
        {
            let mut guard = LAST_INLINE_WORKFLOW
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *guard = Some(request_json.clone());
        } // drop MutexGuard before .await

        if let Err(e) = state
            .app_state
            .pg_db
            .set_setting("last_inline_workflow", &request_json)
            .await
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
        category: request
            .category
            .clone()
            .unwrap_or_else(|| "error-fix".to_string()),
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
        stages: request.stages,
        stop_on_failure: false,
        constraint_overrides: std::collections::HashMap::new(),
        approval_gate: false,
        reflection_mode: false,
        completion_prompts_first: false,
        is_favorite: false,
        dependency_graph: None,
        cost_annotations: None,
        quality_report: None,
        acceptance_criteria: None,
        ai_reviewed: true,
        multi_agent_mode: true,
        strict_cwd: false,
        tool_tags: Vec::new(),
        use_worktree: false,
        model_overrides: std::collections::HashMap::new(),
        enforce_token_budget: false,
        rollback_policy: None,
        workflow_architecture: None,
        flow_control_json: None,
        phase_timeouts_json: None,
        security_profile: None,
        max_fix_attempts: 3,
        max_ci_auto_resumes: 10,
        htn_enabled: false,
        htn_ui_bridge_url: None,
        htn_state_machine_path: None,
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

    // Normalize to stages — consistent with run_unified_workflow
    let normalized_stages = workflow.normalize_to_stages();
    let total_stages = normalized_stages.len();

    // Convert each stage to StageConfig
    let stages: Vec<crate::unified_workflow_executor::StageConfig> = normalized_stages
        .iter()
        .enumerate()
        .map(|(idx, stage)| {
            crate::unified_workflows::stage_to_stage_config(
                stage,
                idx,
                total_stages,
                workflow.preflight_check_enabled,
                workflow.log_watch_enabled,
                workflow.health_check_enabled,
                &workflow.health_check_urls,
            )
        })
        .collect();

    // Build combined prompt from all agentic steps across all stages
    let combined_prompt = normalized_stages
        .iter()
        .flat_map(|stage| stage.agentic_steps.iter())
        .filter_map(|step| step.get("content").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    // Check if any stage has prompt-based steps
    let has_prompt_steps = normalized_stages.iter().any(|stage| {
        stage
            .agentic_steps
            .iter()
            .any(|s| s.get("type").and_then(|t| t.as_str()) == Some("prompt"))
            || stage
                .setup_steps
                .iter()
                .any(|s| s.get("type").and_then(|t| t.as_str()) == Some("prompt"))
            || stage
                .verification_steps
                .iter()
                .any(|s| s.get("type").and_then(|t| t.as_str()) == Some("prompt"))
            || stage
                .completion_steps
                .iter()
                .any(|s| s.get("type").and_then(|t| t.as_str()) == Some("prompt"))
    });

    info!(
        "Inline workflow '{}' normalized to {} stages, has_prompt={}",
        workflow.name, total_stages, has_prompt_steps
    );

    if has_prompt_steps {
        // AI-based execution with verification-agentic loop
        let wf_type = if workflow.category == "plan-generated"
            || workflow.category == "plan-implementation"
        {
            "plan"
        } else {
            "unified"
        };
        let input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
            .with_prompt(&combined_prompt)
            .with_task_type("ai")
            .with_workflow_name(format!("[Inline] {}", workflow.name))
            .with_workflow_id(&workflow.id)
            .with_max_sessions(workflow.iter_cap())
            .with_auto_continue(true)
            .with_workflow_type(wf_type);
        let cr_err = state.app_state.pg_db.create_task_run(&input).await.err();
        if let Some(e) = cr_err {
            warn!(
                "Failed to create task_run for inline workflow {}: {}",
                execution_id, e
            );
        }

        // For error-fix workflows (with targeted_error_ids), run agentic first.
        let run_agentic_first = !workflow.targeted_error_ids.is_empty();

        let mut loop_config = crate::unified_workflow_executor::LoopConfig {
            max_iterations: workflow.iter_cap(),
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
            stages,
            stop_on_failure: workflow.stop_on_failure,
            constraint_overrides: workflow.constraint_overrides.clone(),
            reflection_mode: workflow.reflection_mode,
            provider_override: None,
            model_override: None,
            model_overrides: workflow.model_overrides.clone(),
            stage_index: None,
            max_sessions: workflow.max_iterations,
            auto_run_generated: false,
            multi_agent_mode: workflow.multi_agent_mode,
            strict_cwd: workflow.strict_cwd,
            tool_tags: workflow.tool_tags.clone(),
            use_worktree: workflow.use_worktree,
            worktree_path: None,
            worktree_branch: None,
            workflow_architecture: workflow.workflow_architecture.clone(),
            agentic_verification_config: None,
            multi_agent_pipeline_config: None,
            approval_gate: workflow.approval_gate,
            blocking_approval: false,
            confidence_threshold: 0.85,
            max_context_tokens: 100_000,
            enforce_token_budget: workflow.enforce_token_budget,
            cross_workflow_learning: true,
            verification_history: std::collections::HashMap::new(),
            routing_context: Default::default(),
            project_path: crate::mcp::shared::current_project_path(),
            acceptance_criteria: workflow.acceptance_criteria.clone(),
            rollback_policy: crate::unified_workflow_executor::RollbackPolicy::None,
            escalation_policy: crate::unified_workflow_executor::blame::EscalationPolicy::default(),
            iteration_diffs: Vec::new(),
            active_canary: None,
            is_canary_run: false,
            phase_timeout_ms: None,
            max_fix_attempts: workflow.max_fix_attempts,
            max_ci_auto_resumes: workflow.max_ci_auto_resumes,
            ci_failure_context: None,
            htn_config: crate::planning_bridge::HtnConfig::default(),
        };

        // Apply overrides if present (same logic as run_unified_workflow)
        if let Some(ref overrides) = request.overrides {
            if let Some(model) = overrides.get("model").and_then(|v| v.as_str()) {
                loop_config.model_override = Some(model.to_string());
            }
            if let Some(max_iter) = overrides.get("max_iterations").and_then(|v| v.as_u64()) {
                loop_config.max_iterations = max_iter as u32;
            }
            if let Some(multi) = overrides.get("multi_agent_mode").and_then(|v| v.as_bool()) {
                loop_config.multi_agent_mode = multi;
            }
            if let Some(tokens) = overrides.get("max_context_tokens").and_then(|v| v.as_u64()) {
                loop_config.max_context_tokens = tokens as usize;
            }
            if let Some(arch) = overrides.get("workflow_architecture") {
                if let Ok(parsed) = serde_json::from_value::<
                    crate::agentic_verification::WorkflowArchitecture,
                >(arch.clone())
                {
                    loop_config.workflow_architecture = Some(parsed);
                }
            }
            if let Some(av_config) = overrides.get("agentic_verification_config") {
                if let Ok(parsed) = serde_json::from_value::<
                    crate::agentic_verification::AgenticVerificationConfig,
                >(av_config.clone())
                {
                    loop_config.agentic_verification_config = Some(parsed);
                }
            }
            if let Some(map_config) = overrides.get("multi_agent_pipeline_config") {
                if let Ok(parsed) = serde_json::from_value::<
                    crate::agentic_verification::MultiAgentPipelineConfig,
                >(map_config.clone())
                {
                    loop_config.multi_agent_pipeline_config = Some(parsed);
                }
            }
            if let Some(wt) = overrides.get("use_worktree").and_then(|v| v.as_bool()) {
                loop_config.use_worktree = wt;
            }
            if let Some(raf) = overrides.get("run_agentic_first").and_then(|v| v.as_bool()) {
                loop_config.run_agentic_first = raf;
            }
        }

        // Spawn in background (non-blocking) — same pattern as run_unified_workflow
        let execution_id_for_guard = execution_id.clone();
        let workflow_name_for_guard = workflow.name.clone();
        let app_state = state.app_state.clone();
        let config_storage = state.config_storage.clone();
        let app_handle = state.app_handle.clone();
        let pid_tracker = state.current_ai_pids.clone();

        // Check if Restate durable execution should be used
        let mut use_legacy = true;
        let restate_settings = crate::settings::load_settings().restate;
        if crate::restate::launch::should_use_restate(&restate_settings).await {
            match crate::restate::launch::build_workflow_input_from_loop_config(&loop_config) {
                Ok(input) => {
                    if let Err(e) = state
                        .app_state
                        .pg_db
                        .save_restate_workflow_execution(
                            &execution_id_for_guard,
                            &execution_id_for_guard,
                            None,
                        )
                        .await
                    {
                        tracing::error!("Failed to record Restate workflow: {}", e);
                    }

                    if let Err(e) = crate::restate::launch::launch_workflow_via_restate(
                        &execution_id_for_guard,
                        &input,
                        &restate_settings.ingress_url(),
                    )
                    .await
                    {
                        tracing::error!("Restate launch failed, falling back to legacy: {}", e);
                    } else {
                        use_legacy = false;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to build WorkflowInput for Restate: {}", e);
                }
            }
        }

        if use_legacy {
            crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
                execution_id_for_guard,
                workflow_name_for_guard,
                None,
                None,
                None,
                app_state.pg_db.clone(),
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
                            Vec::new(), // No top-level steps — all in stages
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        )
                        .await
                },
            );
        }

        return Ok(Json(ApiResponse::success(
            crate::step_executor::ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: vec![],
                captured_logs: None,
                captured_runner_logs: None,
                verification_passed: None,
                loop_result: None,
                task_summary: None,
            },
        )));
    }

    // No prompt steps - automation-only workflow
    let all_auto_steps: Vec<_> = stages
        .iter()
        .flat_map(|s| {
            s.setup_automation_steps
                .iter()
                .chain(s.verification_steps.iter())
                .chain(s.completion_automation_steps.iter())
        })
        .cloned()
        .collect();

    let execution_steps_json = serde_json::to_string(&all_auto_steps).ok();
    let mut input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
        .with_task_type("automation")
        .with_workflow_name(format!("[Inline] {}", workflow.name))
        .with_workflow_id(&workflow.id);
    if let Some(esj) = execution_steps_json {
        input = input.with_execution_steps_json(esj);
    }
    let cr_err = state.app_state.pg_db.create_task_run(&input).await.err();
    if let Some(e) = cr_err {
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
        .execute_steps_with_log_sources(&all_auto_steps, &execution_id, &[])
        .await;

    info!(
        "Inline workflow '{}' completed: {} of {} steps succeeded",
        workflow.name, result.successful_steps, result.total_steps
    );

    // Update task_run status
    if result.success {
        if let Err(e) = state.app_state.pg_db.complete_task_run(&execution_id).await {
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
            .pg_db
            .fail_task_run(&execution_id, error_msg)
            .await
        {
            warn!("Failed to mark task_run {} as failed: {}", execution_id, e);
        }
    }

    Ok(Json(ApiResponse::success(result)))
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
    let workflow = match state.app_state.pg_db.get_unified_workflow(&id).await {
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
    match state
        .app_state
        .pg_db
        .get_workflow_stats(&workflow.name)
        .await
    {
        Ok((total, success, failure, last_at, last_status, avg_dur)) => {
            Ok(Json(ApiResponse::success(WorkflowStats {
                total_runs: total,
                success_count: success,
                failure_count: failure,
                last_run_at: last_at,
                last_run_status: last_status,
                avg_duration_ms: avg_dur,
            })))
        }
        Err(e) => {
            error!("Failed to query workflow stats: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to query workflow stats: {}", e))),
            ))
        }
    }
}

/// Request body for running a composed multi-stage workflow from multiple workflows
#[derive(Deserialize)]
pub struct RunComposedWorkflowRequest {
    workflow_ids: Vec<String>,
    #[serde(default = "default_stop_on_failure")]
    stop_on_failure: bool,
}

pub fn default_stop_on_failure() -> bool {
    true
}

/// Response for composed workflow execution
#[derive(Serialize)]
pub struct ComposedWorkflowResponse {
    task_run_id: String,
    workflow_count: usize,
    workflow_names: Vec<String>,
}

/// Run a composed multi-stage workflow from multiple workflows.
///
/// Each workflow becomes a stage with its own verification-agentic loop.
/// Stages execute sequentially with full output context accumulation.
pub async fn run_composed_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunComposedWorkflowRequest>,
) -> Result<Json<ApiResponse<ComposedWorkflowResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    if request.workflow_ids.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("workflow_ids cannot be empty".to_string())),
        ));
    }

    info!(
        "Running composed workflow: {} workflows as stages, stop_on_failure={}",
        request.workflow_ids.len(),
        request.stop_on_failure
    );

    // Fetch all workflows and validate they exist
    let mut workflows: Vec<crate::unified_workflows::UnifiedWorkflow> = Vec::new();
    for id in &request.workflow_ids {
        match state.app_state.pg_db.get_unified_workflow(id).await {
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
    let composed_name = if workflows.len() == 1 {
        workflows[0].name.clone()
    } else {
        format!("{} + {} more", workflows[0].name, workflows.len() - 1)
    };

    // Create a combined prompt describing the composed workflow
    let stage_descriptions = workflows
        .iter()
        .enumerate()
        .map(|(i, w)| format!("{}. {} - {}", i + 1, w.name, w.description))
        .collect::<Vec<_>>()
        .join("\n");

    let combined_prompt = format!(
        "Execute the following multi-stage workflow{}:\n\n{}\n\nTotal stages: {}",
        if request.stop_on_failure {
            " (stopping on first failure)"
        } else {
            ""
        },
        stage_descriptions,
        workflows.len()
    );

    // Create a single task run for the composed workflow
    let execution_id = format!("composed-run-{}", chrono::Utc::now().timestamp_millis());

    // Build StageConfig entries — normalize each workflow to its stages first,
    // then flatten. A 2-stage workflow + a 3-stage workflow = 5 stages total.
    let all_normalized_stages: Vec<(
        crate::unified_workflows::WorkflowStage,
        &crate::unified_workflows::UnifiedWorkflow,
    )> = workflows
        .iter()
        .flat_map(|w| w.normalize_to_stages().into_iter().map(move |s| (s, w)))
        .collect();
    let total = all_normalized_stages.len();
    let stages: Vec<crate::unified_workflow_executor::StageConfig> = all_normalized_stages
        .iter()
        .enumerate()
        .map(|(idx, (stage, workflow))| {
            crate::unified_workflows::stage_to_stage_config(
                stage,
                idx,
                total,
                workflow.preflight_check_enabled,
                workflow.log_watch_enabled,
                workflow.health_check_enabled,
                &workflow.health_check_urls,
            )
        })
        .collect();

    // Create task run for tracking
    let input = CreateTaskRunInput::new(&execution_id, &composed_name)
        .with_prompt(&combined_prompt)
        .with_task_type("ai")
        .with_workflow_name(workflow_names.join(", "))
        .with_auto_continue(true)
        .with_workflow_type("unified");
    if let Some(e) = state.app_state.pg_db.create_task_run(&input).await.err() {
        warn!("Failed to create task_run for composed workflow: {}", e);
    }

    // Capture values for response before moving workflows into the async block
    let workflow_count = workflows.len();
    let workflow_names_response = workflow_names.clone();

    // Spawn background task to execute composed workflow with panic protection
    let state_clone = state.clone();
    let execution_id_clone = execution_id.clone();
    let stop_on_failure = request.stop_on_failure;
    let sequence_name_for_guard = format!("Composed Workflow ({} stages)", workflow_count);
    let execution_id_for_guard = execution_id.clone();
    let url_lock_for_composed = Some(state.app_state.url_lock_manager.clone());
    let file_registry_for_composed = Some(state.app_state.file_registry_manager.clone());
    let file_lock_for_composed = Some(state.app_state.file_lock_manager.clone());

    // Use panic-safe spawning to ensure task is marked as failed on panic
    crate::unified_workflow_executor::spawn_sequence_with_panic_guard(
        execution_id_for_guard,
        sequence_name_for_guard,
        url_lock_for_composed,
        file_registry_for_composed,
        file_lock_for_composed,
        state.app_state.pg_db.clone(),
        async move {
            info!(
                "Starting composed workflow execution: {} stages",
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

            // Build a single LoopConfig with stages populated from the input workflows
            let loop_config = crate::unified_workflow_executor::LoopConfig {
                max_iterations: 10, // Default; stages have per-stage max_iterations
                base_prompt: combined_prompt,
                workflow_name: composed_name.clone(),
                workflow_id: format!("composed-{}", chrono::Utc::now().timestamp_millis()),
                execution_id: execution_id_clone.clone(),
                targeted_error_ids: Vec::new(),
                starting_iteration: 0,
                run_agentic_first: false,
                artifact_dir: None,
                is_dev_mode: cfg!(debug_assertions),
                enable_sweep: false,
                max_sweep_iterations: 0,
                stages,
                stop_on_failure,
                constraint_overrides: std::collections::HashMap::new(),
                reflection_mode: false,
                provider_override: None,
                model_override: None,
                model_overrides: std::collections::HashMap::new(),
                stage_index: None,
                max_sessions: Some(10), // Default budget for composed workflows
                multi_agent_mode: true,
                strict_cwd: false,
                tool_tags: Vec::new(),
                use_worktree: false,
                worktree_path: None,
                worktree_branch: None,
                workflow_architecture: None,
                agentic_verification_config: None,
                multi_agent_pipeline_config: None,
                auto_run_generated: false,
                approval_gate: false,
                blocking_approval: false,
                confidence_threshold: 0.85,
                max_context_tokens: 100_000,
                enforce_token_budget: false,
                cross_workflow_learning: true,
                verification_history: std::collections::HashMap::new(),
                routing_context: Default::default(),
                project_path: crate::mcp::shared::current_project_path(),
                acceptance_criteria: None,
                rollback_policy: crate::unified_workflow_executor::RollbackPolicy::None,
                escalation_policy:
                    crate::unified_workflow_executor::blame::EscalationPolicy::default(),
                iteration_diffs: Vec::new(),
                active_canary: None,
                is_canary_run: false,
                phase_timeout_ms: None,
                max_fix_attempts: 3,
                max_ci_auto_resumes: 10,
                ci_failure_context: None,
                htn_config: crate::planning_bridge::HtnConfig::default(),
            };

            // Run the LoopController once with all stages
            let result = controller
                .run(
                    loop_config,
                    Vec::new(), // No top-level setup (all in stages)
                    Vec::new(), // No top-level setup prompts
                    Vec::new(), // No top-level verification
                    Vec::new(), // No top-level agentic
                    Vec::new(), // No top-level completion
                    Vec::new(), // No top-level completion prompts
                )
                .await;

            // Update task_run status based on result
            if result.success {
                info!("Composed workflow completed successfully");
                let _ = state_clone
                    .app_state
                    .pg_db
                    .complete_task_run(&execution_id_clone)
                    .await;
            } else {
                let error_msg = "Composed workflow failed".to_string();
                error!("{}", error_msg);
                let _ = state_clone
                    .app_state
                    .pg_db
                    .fail_task_run(&execution_id_clone, &error_msg)
                    .await;
            }

            // Generate task summary (fire-and-forget)
            {
                let exec_id = execution_id_clone.clone();
                tokio::spawn(async move {
                    match crate::summary_generator::generate_task_summary_async(
                        exec_id.clone(),
                        None,
                        None,
                        None,
                    )
                    .await
                    {
                        Ok(result) => tracing::info!(
                            "Summary generated for composed workflow {}: goal_achieved={}",
                            exec_id,
                            result.goal_achieved
                        ),
                        Err(e) => tracing::warn!(
                            "Summary generation failed for composed workflow {}: {}",
                            exec_id,
                            e
                        ),
                    }
                });
            }
        },
    );

    Ok(Json(ApiResponse::success(ComposedWorkflowResponse {
        task_run_id: execution_id,
        workflow_count,
        workflow_names: workflow_names_response,
    })))
}

// ============================================================================
// Favorite Toggle API
// ============================================================================

async fn toggle_favorite_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let new_state = state
        .app_state
        .pg_db
        .toggle_favorite(&id)
        .await
        .map_err(|e| {
            let status = if e.contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(api_error(e)))
        })?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "is_favorite": new_state,
    }))))
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
        .pg_db
        .update_example_status(&id, &request.status)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update example status: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(())))
}

// ============================================================================
// Slash Command Sync
// ============================================================================

/// POST /slash-commands/sync — re-sync slash commands from disk
async fn sync_slash_commands_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<crate::slash_commands::SyncResult>>, (StatusCode, Json<ApiResponse<()>>)>
{
    info!("Manual slash command sync requested");

    match crate::slash_commands::sync_slash_commands(&state.app_state.pg_db).await {
        Ok(result) => {
            info!(
                "Slash command sync complete: {} created, {} updated, {} deleted, {} unchanged",
                result.created, result.updated, result.deleted, result.unchanged
            );

            // Emit event to refresh frontend
            let _ = state.app_handle.emit("slash-commands-synced", &result);

            Ok(Json(ApiResponse {
                success: true,
                data: Some(result),
                error: None,
                error_detail: None,
            }))
        }
        Err(e) => {
            error!("Slash command sync failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Slash command sync failed: {}", e))),
            ))
        }
    }
}

// ============================================================================
// End Unified Workflows HTTP API Handlers
// ============================================================================

// ============================================================================
// Exploration Pipeline Handlers
// ============================================================================

/// Query parameters for the templates list endpoint.
#[derive(Debug, Deserialize)]
struct TemplatesQuery {
    domain: Option<String>,
}

/// List step templates, optionally filtered by domain.
async fn list_templates_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TemplatesQuery>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let domain_filter = params.domain;

    let templates = state
        .app_state
        .pg_db
        .list_templates(domain_filter.as_deref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to load templates: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(templates)))
}

/// Get exploration stats for a workflow.
async fn get_exploration_stats_handler(
    State(state): State<Arc<ApiState>>,
    Path(workflow_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state
        .app_state
        .pg_db
        .get_exploration_stats(&workflow_id)
        .await
    {
        Ok(Some(stats)) => Ok(Json(ApiResponse::success(stats))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "No exploration stats found for {}",
                workflow_id
            ))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get exploration stats: {}", e))),
        )),
    }
}

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
            "/unified-workflows/run-composed",
            post(run_composed_workflow),
        )
        .route("/slash-commands/sync", post(sync_slash_commands_handler))
        // Exploration pipeline endpoints
        .route("/generation/templates", get(list_templates_handler))
        .route(
            "/generation/exploration-stats/{workflow_id}",
            get(get_exploration_stats_handler),
        )
        // Parameterized paths after all literal paths
        .route(
            "/unified-workflows/{id}",
            get(get_unified_workflow)
                .put(update_unified_workflow)
                .delete(delete_unified_workflow),
        )
        .route(
            "/unified-workflows/{id}/duplicate",
            post(duplicate_unified_workflow),
        )
        .route(
            "/unified-workflows/{id}/export",
            get(export_unified_workflow),
        )
        .route("/unified-workflows/{id}/run", post(run_unified_workflow))
        .route(
            "/unified-workflows/{id}/stats",
            get(get_unified_workflow_stats),
        )
        .route(
            "/unified-workflows/{id}/example-status",
            axum::routing::put(update_example_status_handler),
        )
        .route(
            "/unified-workflows/{id}/favorite",
            axum::routing::post(toggle_favorite_handler),
        )
}
