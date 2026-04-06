//! HITL (Human-in-the-Loop) question endpoints.
//!
//! Exposes deferred questions from overnight/unattended runs so a human
//! reviewer can approve or reject them via the API.

use axum::{extract::State, http::StatusCode, response::Json, routing::{get, post}, Router};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/hitl/pending", get(get_pending_handler))
        .route("/hitl/{id}/respond", post(respond_handler))
}

// ============================================================================
// Request types
// ============================================================================

#[derive(Debug, Deserialize)]
struct RespondRequest {
    /// Must be "approved" or "rejected".
    status: String,
    /// Optional reviewer comment.
    comment: Option<String>,
}

// ============================================================================
// GET /hitl/pending
// ============================================================================

async fn get_pending_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let questions = state
        .app_state
        .pg_db
        .get_all_pending_questions()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e)),
            )
        })?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "questions": questions }),
    )))
}

// ============================================================================
// POST /hitl/:id/respond
// ============================================================================

async fn respond_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<RespondRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Validate status value
    if body.status != "approved" && body.status != "rejected" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("status must be 'approved' or 'rejected'")),
        ));
    }

    state
        .app_state
        .pg_db
        .review_deferred_question(&id, &body.status, body.comment.as_deref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e)),
            )
        })?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "id": id, "status": body.status }),
    )))
}
