//! HTTP surface for the productivity-stack `reviews` table — Phase 3 of the
//! productivity stack.
//!
//! Three endpoints, mounted at `mcp::reviews::routes()`:
//!
//! - `POST /reviews` — inserts a row and emits a `review-completed` Tauri
//!   event so the dashboard updates and `/coordinate` Rule D sees it.
//!   Rejects self-review (reviewer == reviewed) with HTTP 409.
//! - `GET /reviews/recent?withinSeconds=N&limit=N` — recent reviews used by
//!   Rule D to scan a single iteration's worth of verdicts.
//! - `GET /sessions/<id>/latest-review` — the worker session's most recent
//!   verdict, used by `ReviewBadge`'s 30-second poll fallback.
//!
//! The `/sessions/<id>/latest-review` route is intentionally rooted under
//! `/sessions` rather than `/reviews` to match the badge's mental model
//! (it asks "what's this session's review state?" not "give me the review
//! row").

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Emitter;
use tracing::warn;

use crate::database::pg::reviews::{InsertReviewError, InsertReviewInput, ReviewRow};
use crate::mcp::types::ApiState;

// =============================================================================
// POST /reviews
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct InsertReviewRequest {
    pub task_id: String,
    pub reviewer_session_id: String,
    pub reviewed_session_id: String,
    pub verdict: String,
    pub confidence: f64,
    pub reasoning: String,
    #[serde(default)]
    pub diff_summary: Option<serde_json::Value>,
    #[serde(default)]
    pub test_results: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertReviewResponse {
    pub review: ReviewRow,
}

async fn post_review(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<InsertReviewRequest>,
) -> Result<(StatusCode, Json<InsertReviewResponse>), (StatusCode, String)> {
    if req.task_id.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "task_id is required".to_string()));
    }
    if req.reviewer_session_id.trim().is_empty() || req.reviewed_session_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "reviewer_session_id and reviewed_session_id are required".to_string(),
        ));
    }
    if req.reasoning.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "reasoning is required".to_string()));
    }

    let input = InsertReviewInput {
        task_id: &req.task_id,
        reviewer_session_id: &req.reviewer_session_id,
        reviewed_session_id: &req.reviewed_session_id,
        verdict: &req.verdict,
        confidence: req.confidence,
        reasoning: &req.reasoning,
        diff_summary: req.diff_summary.clone(),
        test_results: req.test_results.clone(),
    };

    let row = match state.app_state.pg_db.insert_review(&input).await {
        Ok(r) => r,
        Err(InsertReviewError::SelfReview) => {
            return Err((
                StatusCode::CONFLICT,
                "reviewer_session_id == reviewed_session_id (no self-review)".to_string(),
            ));
        }
        Err(InsertReviewError::InvalidVerdict(v)) => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "invalid verdict '{}' (expected approved|needs_fix|escalate)",
                    v
                ),
            ));
        }
        Err(InsertReviewError::InvalidConfidence(c)) => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("invalid confidence {} (must be in [0.0, 1.0])", c),
            ));
        }
        Err(InsertReviewError::Db(e)) => {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, e));
        }
    };

    // Emit the `review-completed` event so:
    //   - `ReviewBadge` updates the affected tab without a poll.
    //   - `/coordinate` Rule D can pick it up on its next observe iteration
    //     (or immediately if it subscribes).
    let payload = serde_json::json!({
        "id": row.id,
        "taskId": row.task_id,
        "reviewerSessionId": row.reviewer_session_id,
        "reviewedSessionId": row.reviewed_session_id,
        "verdict": row.verdict,
        "confidence": row.confidence,
        "createdAt": row.created_at,
    });
    if let Err(e) = state.app_handle.emit("review-completed", &payload) {
        warn!("Failed to emit review-completed event: {}", e);
    }

    Ok((
        StatusCode::CREATED,
        Json(InsertReviewResponse { review: row }),
    ))
}

// =============================================================================
// GET /reviews/recent
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentReviewsQuery {
    /// Lookback window in seconds. Defaults to 60 (one Coordinator
    /// iteration plus headroom).
    #[serde(default = "default_within_seconds")]
    pub within_seconds: i64,
    /// Cap on rows returned. Defaults to 50.
    #[serde(default = "default_recent_limit")]
    pub limit: i64,
}

fn default_within_seconds() -> i64 {
    60
}

fn default_recent_limit() -> i64 {
    50
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentReviewsResponse {
    pub reviews: Vec<ReviewRow>,
}

async fn get_recent_reviews(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<RecentReviewsQuery>,
) -> Result<Json<RecentReviewsResponse>, (StatusCode, String)> {
    let within = query.within_seconds.max(1);
    let limit = query.limit.clamp(1, 500);

    let reviews = state
        .app_state
        .pg_db
        .list_recent_reviews(within, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(RecentReviewsResponse { reviews }))
}

// =============================================================================
// GET /sessions/<id>/latest-review
// =============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestReviewResponse {
    pub task_run_id: String,
    pub review: Option<ReviewRow>,
}

async fn get_latest_review_for_session(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<LatestReviewResponse>, (StatusCode, String)> {
    let review = state
        .app_state
        .pg_db
        .get_latest_review_for_session(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(LatestReviewResponse {
        task_run_id: id,
        review,
    }))
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/reviews", post(post_review))
        .route("/reviews/recent", get(get_recent_reviews))
        .route(
            "/sessions/{id}/latest-review",
            get(get_latest_review_for_session),
        )
}
