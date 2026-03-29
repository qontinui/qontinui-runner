//! Findings HTTP endpoints for MCP API
//!
//! Provides HTTP handlers for querying and managing AI-detected findings
//! stored in the checkpoint database. These endpoints expose the same
//! underlying logic as the Tauri commands in `commands/findings.rs`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::findings::{Finding, FindingStatus};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// Request body for updating finding status
#[derive(Debug, Deserialize)]
pub struct UpdateFindingStatusRequest {
    /// New status: "detected", "in_progress", "needs_input", "resolved", "wont_fix", "deferred"
    pub status: String,
    /// Optional resolution text
    pub resolution: Option<String>,
}

/// Request body for resolving a finding
#[derive(Debug, Deserialize)]
pub struct ResolveFindingRequest {
    /// Resolution text describing how the finding was resolved
    pub resolution: String,
}

/// Request body for providing user response to a finding
#[derive(Debug, Deserialize)]
pub struct UserResponseRequest {
    /// User's response text
    pub response: String,
}

/// Response for mutation operations
#[derive(Debug, Serialize)]
pub struct FindingMutationResponse {
    pub finding_id: String,
    pub status: String,
}

/// GET /findings/task/:task_run_id
///
/// Returns all findings for a specific task run.
pub async fn get_task_findings_handler(
    State(state): State<Arc<ApiState>>,
    Path(task_run_id): Path<String>,
) -> Result<Json<ApiResponse<Vec<Finding>>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state
        .app_state
        .checkpoint_db
        .get_findings_for_task(&task_run_id)
    {
        Ok(findings) => Ok(Json(ApiResponse::success(findings))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get findings: {}", e))),
        )),
    }
}

/// PUT /findings/:finding_id/status
///
/// Update the status of a finding.
/// Supported statuses: "detected", "in_progress", "needs_input", "resolved", "wont_fix", "deferred"
pub async fn update_finding_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(finding_id): Path<String>,
    Json(req): Json<UpdateFindingStatusRequest>,
) -> Result<Json<ApiResponse<FindingMutationResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let status = FindingStatus::from_str(&req.status).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid status: {}", req.status))),
        )
    })?;

    state.app_state.pg_db.update_finding_status(&finding_id, status.as_str(), req.resolution.as_deref(), None).await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to update finding status: {}", e))),
        )
    })?;

    // Invalidate graph cache and emit event so the knowledge graph rebuilds on next access
    crate::mcp::graph_api::invalidate_graph_cache(&state, "finding_mutation").await;

    info!(
        "HTTP: Updated finding {} to status {}",
        finding_id,
        status.as_str()
    );

    Ok(Json(ApiResponse::success(FindingMutationResponse {
        finding_id,
        status: req.status,
    })))
}

/// POST /findings/:finding_id/resolve
///
/// Mark a finding as resolved with the given resolution text.
/// Sets resolved_at timestamp automatically.
pub async fn resolve_finding_handler(
    State(state): State<Arc<ApiState>>,
    Path(finding_id): Path<String>,
    Json(req): Json<ResolveFindingRequest>,
) -> Result<Json<ApiResponse<FindingMutationResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    state.app_state.pg_db.update_finding_status(&finding_id, "resolved", Some(&req.resolution), None).await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to resolve finding: {}", e))),
        )
    })?;

    // Invalidate graph cache and emit event so the knowledge graph rebuilds on next access
    crate::mcp::graph_api::invalidate_graph_cache(&state, "finding_mutation").await;

    info!("HTTP: Resolved finding {}", finding_id);

    Ok(Json(ApiResponse::success(FindingMutationResponse {
        finding_id,
        status: "resolved".to_string(),
    })))
}

/// POST /findings/:finding_id/user-response
///
/// Set the user_response field on a finding (for findings that need user input).
/// This also transitions the finding status from "needs_input" to "in_progress".
pub async fn user_response_handler(
    State(state): State<Arc<ApiState>>,
    Path(finding_id): Path<String>,
    Json(req): Json<UserResponseRequest>,
) -> Result<Json<ApiResponse<FindingMutationResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    state
        .app_state
        .checkpoint_db
        .set_finding_user_response(&finding_id, &req.response)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to set user response: {}", e))),
            )
        })?;

    info!("HTTP: Set user response for finding {}", finding_id);

    Ok(Json(ApiResponse::success(FindingMutationResponse {
        finding_id,
        status: "in_progress".to_string(),
    })))
}

/// Response for clear all findings
#[derive(Debug, Serialize)]
pub struct ClearFindingsResponse {
    pub task_run_id: String,
    pub cleared_count: usize,
}

/// POST /findings/task/:task_run_id/clear-all
///
/// Resolve all findings for the given task run with status "resolved"
/// and resolution "Cleared by user".
pub async fn clear_all_findings_handler(
    State(state): State<Arc<ApiState>>,
    Path(task_run_id): Path<String>,
) -> Result<Json<ApiResponse<ClearFindingsResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Get all findings for the task
    let findings = state
        .app_state
        .checkpoint_db
        .get_findings_for_task(&task_run_id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get findings: {}", e))),
            )
        })?;

    let mut cleared_count = 0;

    // Resolve each non-terminal finding
    for finding in &findings {
        if !finding.status.is_terminal() {
            if let Err(e) = state.app_state.pg_db.update_finding_status(&finding.id, "resolved", Some("Cleared by user"), None).await {
                info!("HTTP: Failed to clear finding {}: {}", finding.id, e);
                continue;
            }
            cleared_count += 1;
        }
    }

    if cleared_count > 0 {
        crate::mcp::graph_api::invalidate_graph_cache(&state, "clear_all_findings").await;
    }

    info!(
        "HTTP: Cleared {} findings for task run {}",
        cleared_count, task_run_id
    );

    Ok(Json(ApiResponse::success(ClearFindingsResponse {
        task_run_id,
        cleared_count,
    })))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post, put};
    axum::Router::new()
        .route(
            "/findings/task/{task_run_id}",
            get(get_task_findings_handler),
        )
        .route(
            "/findings/{finding_id}/status",
            put(update_finding_status_handler),
        )
        .route(
            "/findings/{finding_id}/resolve",
            post(resolve_finding_handler),
        )
        .route(
            "/findings/{finding_id}/user-response",
            post(user_response_handler),
        )
        .route(
            "/findings/task/{task_run_id}/clear-all",
            post(clear_all_findings_handler),
        )
}
