//! Check generation and CRUD handlers for MCP API
//!
//! Provides HTTP handlers for workspace scanning, AI-assisted check generation,
//! check association repair, and full CRUD for checks and check groups.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::database::{
    Check, CheckGroup, CheckpointDb, CreateCheckGroupInput, CreateCheckInput,
    UpdateCheckGroupInput, UpdateCheckInput,
};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Check Generation HTTP API Handlers
// ============================================================================

/// Scan a workspace for projects and their configuration
pub async fn scan_workspace_handler(
    Json(request): Json<crate::check_generation::ScanWorkspaceRequest>,
) -> Result<
    Json<ApiResponse<crate::check_generation::WorkspaceScanResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    use crate::check_generation::scan_workspace;

    info!(
        "Scanning workspace: {} (max_depth: {})",
        request.base_directory, request.max_depth
    );

    // Run scan in blocking task since it involves file I/O
    let result = tokio::task::spawn_blocking(move || scan_workspace(&request))
        .await
        .map_err(|e| {
            error!("Workspace scan task failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Scan task failed: {}", e))),
            )
        })?;

    match result {
        Ok(scan_result) => {
            info!(
                "Workspace scan complete: {} projects found in {} directories",
                scan_result.projects.len(),
                scan_result.total_directories_scanned
            );
            Ok(Json(ApiResponse::success(scan_result)))
        }
        Err(e) => {
            error!("Workspace scan failed: {}", e);
            Err((StatusCode::BAD_REQUEST, Json(api_error(e))))
        }
    }
}

/// Generate check suggestions using AI based on workspace scan results
pub async fn generate_checks_handler(
    State(api_state): State<Arc<ApiState>>,
    Json(request): Json<crate::check_generation::GenerateChecksRequest>,
) -> Result<
    Json<ApiResponse<crate::check_generation::GenerateChecksResponse>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    use crate::check_generation::generate_checks;

    info!(
        "Generating checks for {} projects",
        request.workspace_scan.projects.len()
    );

    // Clone doctor handle so it can be moved into the blocking closure
    let doctor_handle = api_state.doctor_handle.clone();

    // Run generation in blocking task since it may call AI
    let result =
        tokio::task::spawn_blocking(move || generate_checks(&request, doctor_handle.as_ref()))
            .await
            .map_err(|e| {
                error!("Check generation task failed: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Generation task failed: {}", e))),
                )
            })?;

    info!(
        "Check generation complete: {} suggestions, success={}",
        result.suggested_checks.len(),
        result.success
    );

    Ok(Json(ApiResponse::success(result)))
}

/// Repair check-group associations based on naming convention
///
/// Checks are named with format "{group_name} - {tool_name}" (e.g., "multistate - Ruff Linting").
/// This endpoint finds checks that match groups by this pattern and ensures they are linked.
pub async fn repair_check_associations_handler(
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Repairing check-group associations via MCP API");

    let db = CheckpointDb::new().map_err(|e| {
        error!("Failed to open database: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to open database: {}", e))),
        )
    })?;

    match db.repair_check_group_associations() {
        Ok(count) => {
            let message = if count > 0 {
                format!("Repaired {} check-group associations", count)
            } else {
                "All check-group associations are already correct".to_string()
            };
            info!("{}", message);
            Ok(Json(ApiResponse::success(serde_json::json!({
                "message": message,
                "associations_created": count
            }))))
        }
        Err(e) => {
            error!("Failed to repair associations: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to repair associations: {}", e))),
            ))
        }
    }
}

// ============================================================================
// End Check Generation HTTP API Handlers
// ============================================================================

// ============================================================================
// Check CRUD HTTP API Handlers
// ============================================================================

/// Query parameters for listing checks
#[derive(Debug, Deserialize)]
pub struct ListChecksQuery {
    #[serde(default)]
    pub enabled_only: Option<bool>,
    #[serde(default)]
    pub check_type: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
}

/// List all checks
pub async fn list_checks_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListChecksQuery>,
) -> Result<Json<ApiResponse<Vec<Check>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let enabled_only = query.enabled_only.unwrap_or(false);
    match state.app_state.checkpoint_db.list_checks(
        enabled_only,
        query.check_type.as_deref(),
        query.tool.as_deref(),
    ) {
        Ok(checks) => Ok(Json(ApiResponse::success(checks))),
        Err(e) => {
            error!("Failed to list checks: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list checks: {}", e))),
            ))
        }
    }
}

/// Get a single check by ID
pub async fn get_check_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Check>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.app_state.checkpoint_db.get_check(&id) {
        Ok(Some(check)) => Ok(Json(ApiResponse::success(check))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Check not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to get check: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get check: {}", e))),
            ))
        }
    }
}

/// Create a new check
pub async fn create_check_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateCheckInput>,
) -> Result<Json<ApiResponse<Check>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Creating check: {}", input.name);
    match state.app_state.checkpoint_db.create_check(&input) {
        Ok(check) => {
            info!("Created check: {} ({})", check.name, check.id);
            Ok(Json(ApiResponse::success(check)))
        }
        Err(e) => {
            error!("Failed to create check: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to create check: {}", e))),
            ))
        }
    }
}

/// Update an existing check
pub async fn update_check_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<UpdateCheckInput>,
) -> Result<Json<ApiResponse<Check>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Updating check: {}", id);
    match state.app_state.checkpoint_db.update_check(&id, &input) {
        Ok(check) => {
            info!("Updated check: {} ({})", check.name, check.id);
            Ok(Json(ApiResponse::success(check)))
        }
        Err(e) if e.contains("not found") => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Check not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to update check: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update check: {}", e))),
            ))
        }
    }
}

/// Delete a check by ID
pub async fn delete_check_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Deleting check: {}", id);
    match state.app_state.checkpoint_db.delete_check(&id) {
        Ok(true) => Ok(Json(ApiResponse::success(serde_json::json!({
            "deleted": true,
            "id": id
        })))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Check not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to delete check: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to delete check: {}", e))),
            ))
        }
    }
}

// ============================================================================
// Check Group CRUD HTTP API Handlers
// ============================================================================

/// Query parameters for listing check groups
#[derive(Debug, Deserialize)]
pub struct ListCheckGroupsQuery {
    #[serde(default)]
    pub enabled_only: Option<bool>,
}

/// List all check groups
pub async fn list_check_groups_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListCheckGroupsQuery>,
) -> Result<Json<ApiResponse<Vec<CheckGroup>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let enabled_only = query.enabled_only.unwrap_or(false);
    match state
        .app_state
        .checkpoint_db
        .list_check_groups(enabled_only)
    {
        Ok(groups) => Ok(Json(ApiResponse::success(groups))),
        Err(e) => {
            error!("Failed to list check groups: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list check groups: {}", e))),
            ))
        }
    }
}

/// Get a single check group by ID
pub async fn get_check_group_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<CheckGroup>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.app_state.checkpoint_db.get_check_group(&id) {
        Ok(Some(group)) => Ok(Json(ApiResponse::success(group))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Check group not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to get check group: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get check group: {}", e))),
            ))
        }
    }
}

/// Create a new check group
pub async fn create_check_group_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateCheckGroupInput>,
) -> Result<Json<ApiResponse<CheckGroup>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Creating check group: {}", input.name);
    match state.app_state.checkpoint_db.create_check_group(&input) {
        Ok(group) => {
            info!("Created check group: {} ({})", group.name, group.id);
            Ok(Json(ApiResponse::success(group)))
        }
        Err(e) => {
            error!("Failed to create check group: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to create check group: {}", e))),
            ))
        }
    }
}

/// Update an existing check group
pub async fn update_check_group_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<UpdateCheckGroupInput>,
) -> Result<Json<ApiResponse<CheckGroup>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Updating check group: {}", id);
    match state
        .app_state
        .checkpoint_db
        .update_check_group(&id, &input)
    {
        Ok(group) => {
            info!("Updated check group: {} ({})", group.name, group.id);
            Ok(Json(ApiResponse::success(group)))
        }
        Err(e) if e.contains("not found") => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Check group not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to update check group: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to update check group: {}", e))),
            ))
        }
    }
}

/// Delete a check group by ID
pub async fn delete_check_group_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Deleting check group: {}", id);
    match state.app_state.checkpoint_db.delete_check_group(&id) {
        Ok(true) => Ok(Json(ApiResponse::success(serde_json::json!({
            "deleted": true,
            "id": id
        })))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Check group not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to delete check group: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to delete check group: {}", e))),
            ))
        }
    }
}

// ============================================================================
// End Check CRUD HTTP API Handlers
// ============================================================================

// ============================================================================
// Check Run Handler
// ============================================================================

/// Request body for running a check
#[derive(Debug, Deserialize)]
pub struct RunCheckRequest {
    /// Optional task run ID for audit logging
    #[serde(default)]
    pub task_run_id: Option<String>,
}

/// Run a check by ID.
///
/// Reads the check from the database, converts it to a CheckDefinition,
/// executes it, and optionally stores the result.
pub async fn run_check_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<RunCheckRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("HTTP: Running check: {}", id);

    // Get the check from database
    let check = match state.app_state.checkpoint_db.get_check(&id) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Check not found: {}", id))),
            ));
        }
        Err(e) => {
            error!("HTTP: Failed to get check: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get check: {}", e))),
            ));
        }
    };

    let db = state.app_state.checkpoint_db.clone();
    let task_run_id = request.task_run_id.clone();

    // Execute in spawn_blocking since check execution is synchronous I/O
    let result = tokio::task::spawn_blocking(move || {
        use crate::check_executor::{execute_check, CheckDefinition, CheckTool, CheckType};

        // Convert database Check to CheckDefinition
        let check_def = CheckDefinition {
            id: check.id.clone(),
            name: check.name.clone(),
            check_type: serde_json::from_str(&format!("\"{}\"", check.check_type))
                .unwrap_or(CheckType::Lint),
            tool: serde_json::from_str(&format!("\"{}\"", check.tool)).unwrap_or(CheckTool::Custom),
            command: check.command.clone(),
            working_directory: check.working_directory.clone(),
            config_path: check.config_path.clone(),
            auto_fix: check.auto_fix,
            fail_on_warning: check.fail_on_warning,
            timeout_seconds: check.timeout_seconds,
            is_critical: check.is_critical,
        };

        // Execute the check
        let result = execute_check(&check_def);

        // Store result in database if task_run_id provided
        if let Some(ref run_id) = task_run_id {
            let status_str = serde_json::to_string(&result.status)
                .unwrap_or("\"pending\"".to_string())
                .trim_matches('"')
                .to_string();
            let structured = result
                .structured_output
                .as_ref()
                .map(|o| serde_json::to_string(o).unwrap_or_default());

            if let Err(e) = db.save_check_result(
                &result.check_id,
                &status_str,
                Some(result.started_at.as_str()),
                Some(result.completed_at.as_str()),
                Some(result.duration_ms as i64),
                Some(result.output.as_str()),
                result.error.as_deref(),
                result.issues_found as i32,
                result.issues_fixed as i32,
                result.files_checked as i32,
                structured.as_deref(),
                Some(run_id.as_str()),
            ) {
                tracing::error!("Failed to store check result: {}", e);
            }
        }

        result
    })
    .await
    .map_err(|e| {
        error!("HTTP: spawn_blocking error for run check: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    info!(
        "HTTP: Check executed: status={:?}, duration={}ms",
        result.status, result.duration_ms
    );

    Ok(Json(ApiResponse::success(
        serde_json::to_value(result).unwrap_or_default(),
    )))
}

// ============================================================================
// Check Group Member Management Handlers
// ============================================================================

/// Get checks assigned to a check group
pub async fn get_checks_in_group_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<Check>>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Verify group exists
    match state.app_state.checkpoint_db.get_check_group(&id) {
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Check group not found: {}", id))),
            ));
        }
        Err(e) => {
            error!("Failed to get check group: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get check group: {}", e))),
            ));
        }
        Ok(Some(_)) => {}
    }

    match state.app_state.checkpoint_db.get_checks_in_group(&id) {
        Ok(checks) => Ok(Json(ApiResponse::success(checks))),
        Err(e) => {
            error!("Failed to get checks in group {}: {}", id, e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get checks in group: {}", e))),
            ))
        }
    }
}

/// Request body for setting checks in a group
#[derive(Debug, Deserialize)]
pub struct SetChecksInGroupRequest {
    pub check_ids: Vec<String>,
}

/// Set checks assigned to a check group (replaces all existing assignments)
pub async fn set_checks_in_group_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<SetChecksInGroupRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "Setting {} checks in group: {}",
        request.check_ids.len(),
        id
    );

    // Verify group exists
    match state.app_state.checkpoint_db.get_check_group(&id) {
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Check group not found: {}", id))),
            ));
        }
        Err(e) => {
            error!("Failed to get check group: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get check group: {}", e))),
            ));
        }
        Ok(Some(_)) => {}
    }

    match state
        .app_state
        .checkpoint_db
        .set_checks_in_group(&id, &request.check_ids)
    {
        Ok(()) => {
            info!("Set {} checks in group {}", request.check_ids.len(), id);
            Ok(Json(ApiResponse::success(serde_json::json!({
                "group_id": id,
                "check_count": request.check_ids.len()
            }))))
        }
        Err(e) => {
            error!("Failed to set checks in group {}: {}", id, e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to set checks in group: {}", e))),
            ))
        }
    }
}

/// Run all checks in a check group
pub async fn run_check_group_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("HTTP: Running check group: {}", id);

    // Get the group
    let group = match state.app_state.checkpoint_db.get_check_group(&id) {
        Ok(Some(g)) => g,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Check group not found: {}", id))),
            ));
        }
        Err(e) => {
            error!("Failed to get check group: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get check group: {}", e))),
            ));
        }
    };

    // Get checks in group
    let checks = match state.app_state.checkpoint_db.get_checks_in_group(&id) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to get checks in group: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get checks in group: {}", e))),
            ));
        }
    };

    if checks.is_empty() {
        return Ok(Json(ApiResponse::success(serde_json::json!({
            "group_id": id,
            "group_name": group.name,
            "total": 0,
            "passed": 0,
            "failed": 0,
            "results": []
        }))));
    }

    let _db = state.app_state.checkpoint_db.clone();
    let stop_on_failure = group.stop_on_failure;

    // Execute checks sequentially in a blocking task
    let result = tokio::task::spawn_blocking(move || {
        use crate::check_executor::{execute_check, CheckDefinition, CheckTool, CheckType};

        let mut results = Vec::new();
        let mut passed = 0u32;
        let mut failed = 0u32;

        for check in &checks {
            let check_def = CheckDefinition {
                id: check.id.clone(),
                name: check.name.clone(),
                check_type: serde_json::from_str(&format!("\"{}\"", check.check_type))
                    .unwrap_or(CheckType::Lint),
                tool: serde_json::from_str(&format!("\"{}\"", &check.tool))
                    .unwrap_or(CheckTool::Custom),
                command: check.command.clone(),
                working_directory: check.working_directory.clone(),
                config_path: check.config_path.clone(),
                auto_fix: check.auto_fix,
                fail_on_warning: check.fail_on_warning,
                timeout_seconds: check.timeout_seconds,
                is_critical: check.is_critical,
            };

            let check_result = execute_check(&check_def);
            let is_pass = matches!(
                check_result.status,
                crate::check_executor::CheckStatus::Passed
            );

            if is_pass {
                passed += 1;
            } else {
                failed += 1;
            }

            results.push(serde_json::to_value(&check_result).unwrap_or_default());

            if !is_pass && stop_on_failure {
                break;
            }
        }

        serde_json::json!({
            "group_id": id,
            "group_name": group.name,
            "total": results.len(),
            "passed": passed,
            "failed": failed,
            "results": results
        })
    })
    .await
    .map_err(|e| {
        error!("HTTP: spawn_blocking error for run check group: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(result)))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        // Check generation routes
        .route("/checks/scan-workspace", post(scan_workspace_handler))
        .route("/checks/generate", post(generate_checks_handler))
        .route(
            "/checks/repair-associations",
            post(repair_check_associations_handler),
        )
        // Check CRUD routes
        .route(
            "/checks",
            get(list_checks_handler).post(create_check_handler),
        )
        .route(
            "/checks/{id}",
            get(get_check_handler)
                .put(update_check_handler)
                .delete(delete_check_handler),
        )
        // Check run route
        .route("/checks/{id}/run", post(run_check_handler))
        // Check group CRUD routes
        .route(
            "/check-groups",
            get(list_check_groups_handler).post(create_check_group_handler),
        )
        .route(
            "/check-groups/{id}",
            get(get_check_group_handler)
                .put(update_check_group_handler)
                .delete(delete_check_group_handler),
        )
        // Check group member management routes
        .route(
            "/check-groups/{id}/checks",
            get(get_checks_in_group_handler).put(set_checks_in_group_handler),
        )
        .route("/check-groups/{id}/run", post(run_check_group_handler))
}
