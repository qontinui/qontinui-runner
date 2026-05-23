//! Findings HTTP endpoints for MCP API
//!
//! Provides HTTP handlers for querying and managing AI-detected findings
//! stored in the checkpoint database. These endpoints expose the same
//! underlying logic as the Tauri commands in `commands/findings.rs`.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::findings::{Finding, FindingStatus, FindingStatusExt};
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

/// Query string for `GET /findings`.
#[derive(Debug, Deserialize, Default)]
pub struct ListFindingsQuery {
    /// Optional status filter (e.g. "needs_input", "detected").
    pub status: Option<String>,
    /// Optional category filter (e.g. "code_bug", "test_failure").
    pub category: Option<String>,
    /// Page size (clamped to 1..=200; defaults to 50).
    pub limit: Option<i64>,
    /// 1-based page index (defaults to 1).
    pub page: Option<i64>,
}

/// Envelope for paginated finding listings.
#[derive(Debug, Serialize)]
pub struct ListFindingsResponse {
    pub items: Vec<Finding>,
    pub page: i64,
    pub limit: i64,
    pub count: usize,
}

/// GET /findings/:finding_id
///
/// Returns a single finding by id, or 404 if not found.
pub async fn get_finding_handler(
    State(state): State<Arc<ApiState>>,
    Path(finding_id): Path<String>,
) -> Result<Json<ApiResponse<Finding>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.app_state.pg_db.get_finding(&finding_id).await {
        Ok(Some(finding)) => Ok(Json(ApiResponse::success(finding))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Finding not found: {}", finding_id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get finding: {}", e))),
        )),
    }
}

/// GET /findings
///
/// Paginated cross-task-run listing. Supports `status`, `category`, `limit`,
/// `page` query parameters. Results are ordered by `detected_at` DESC.
pub async fn list_findings_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<ListFindingsQuery>,
) -> Result<Json<ApiResponse<ListFindingsResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let page = q.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;

    match state
        .app_state
        .pg_db
        .list_findings(q.status.as_deref(), q.category.as_deref(), limit, offset)
        .await
    {
        Ok(items) => {
            let count = items.len();
            Ok(Json(ApiResponse::success(ListFindingsResponse {
                items,
                page,
                limit,
                count,
            })))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list findings: {}", e))),
        )),
    }
}

/// GET /findings/by-status/:status
///
/// Shortcut for `GET /findings?status=<status>` with default pagination.
pub async fn findings_by_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(status): Path<String>,
    Query(q): Query<ListFindingsQuery>,
) -> Result<Json<ApiResponse<ListFindingsResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Validate the status value up front so callers get a 400 rather than an
    // empty list when they typo the variant.
    if FindingStatus::from_str(&status).is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid status: {}", status))),
        ));
    }

    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let page = q.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;

    match state
        .app_state
        .pg_db
        .list_findings(Some(&status), q.category.as_deref(), limit, offset)
        .await
    {
        Ok(items) => {
            let count = items.len();
            Ok(Json(ApiResponse::success(ListFindingsResponse {
                items,
                page,
                limit,
                count,
            })))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to list findings by status: {}",
                e
            ))),
        )),
    }
}

/// GET /findings/task/:task_run_id
///
/// Returns all findings for a specific task run.
pub async fn get_task_findings_handler(
    State(state): State<Arc<ApiState>>,
    Path(task_run_id): Path<String>,
) -> Result<Json<ApiResponse<Vec<Finding>>>, (StatusCode, Json<ApiResponse<()>>)> {
    // checkpoint_db removed — use PG for findings
    match state
        .app_state
        .pg_db
        .get_findings_for_task(&task_run_id)
        .await
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

    state
        .app_state
        .pg_db
        .update_finding_status(
            &finding_id,
            status.as_str(),
            req.resolution.as_deref(),
            None,
        )
        .await
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
    state
        .app_state
        .pg_db
        .update_finding_status(&finding_id, "resolved", Some(&req.resolution), None)
        .await
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
        .pg_db
        .set_finding_user_response(&finding_id, &req.response)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to set user response: {}", e))),
            )
        })?;

    // Transition status `needs_input` -> `in_progress` so downstream consumers
    // (and the mutation response below) reflect what the docstring promises.
    state
        .app_state
        .pg_db
        .update_finding_status(&finding_id, "in_progress", None, None)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to transition finding to in_progress: {}",
                    e
                ))),
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
    // Get all findings for the task (PG)
    let findings = state
        .app_state
        .pg_db
        .get_findings_for_task(&task_run_id)
        .await
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
            if let Err(e) = state
                .app_state
                .pg_db
                .update_finding_status(&finding.id, "resolved", Some("Cleared by user"), None)
                .await
            {
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
        .route("/findings", get(list_findings_handler))
        .route(
            "/findings/by-status/{status}",
            get(findings_by_status_handler),
        )
        .route(
            "/findings/task/{task_run_id}",
            get(get_task_findings_handler),
        )
        .route(
            "/findings/task/{task_run_id}/clear-all",
            post(clear_all_findings_handler),
        )
        // Capture routes go last so the literal `/findings/task/...` and
        // `/findings/by-status/...` patterns above take priority in axum 0.8's
        // matchit router.
        .route("/findings/{finding_id}", get(get_finding_handler))
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
}
