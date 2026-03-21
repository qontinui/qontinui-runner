//! Queue management handlers for MCP API.
//!
//! Provides HTTP endpoints for inspecting and managing the workflow execution queue:
//! - `GET /queue/status` — queue depth, running count, capacity
//! - `GET /queue` — list all pending workflows
//! - `DELETE /queue/:id` — cancel a pending workflow

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use std::sync::Arc;
use tracing::info;

use crate::mcp::types::{ApiResponse, ApiState};
use crate::workflow_queue::{get_workflow_queue, QueueStatus, QueuedWorkflow};

// ============================================================================
// Handlers
// ============================================================================

/// Get the current status of the workflow queue.
///
/// Returns concurrency information: how many workflows are pending,
/// how many are running, and the configured limits.
pub async fn get_queue_status(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<QueueStatus>> {
    let status = get_workflow_queue().status().await;
    Json(ApiResponse::success(status))
}

/// List all workflows currently waiting in the queue.
///
/// Does not include workflows that are currently executing.
pub async fn list_queued_workflows(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<QueuedWorkflow>>> {
    let workflows = get_workflow_queue().list().await;
    Json(ApiResponse::success(workflows))
}

/// Cancel a pending workflow by its queue entry ID.
///
/// Returns success if the workflow was found and removed from the queue.
/// Returns an error if the workflow was not found (may already be executing).
pub async fn cancel_queued_workflow(
    State(_state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<CancelResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Cancelling queued workflow: {}", id);

    let cancelled = get_workflow_queue().cancel(&id).await;

    if cancelled {
        Ok(Json(ApiResponse::success(CancelResponse {
            cancelled: true,
            message: format!("Workflow {} removed from queue", id),
        })))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!(
                "Workflow {} not found in queue (may already be executing or completed)",
                id
            ))),
        ))
    }
}

// ============================================================================
// Response Types
// ============================================================================

/// Response for cancel operations.
#[derive(Debug, serde::Serialize)]
pub struct CancelResponse {
    pub cancelled: bool,
    pub message: String,
}

// ============================================================================
// Routes
// ============================================================================

/// Create routes for the queue management module.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{delete, get};

    axum::Router::new()
        .route("/queue/status", get(get_queue_status))
        .route("/queue", get(list_queued_workflows))
        .route("/queue/{id}", delete(cancel_queued_workflow))
}
