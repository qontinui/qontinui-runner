//! Security audit HTTP endpoints for MCP API.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::mcp::types::ApiState;

#[derive(Debug, Deserialize)]
struct AuditEventsQuery {
    task_run_id: Option<String>,
    event_type: Option<String>,
    decision: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AuditSummaryQuery {
    task_run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuditCleanupQuery {
    older_than_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct AuditEventResponse {
    id: String,
    timestamp: String,
    task_run_id: Option<String>,
    step_name: Option<String>,
    event_type: String,
    action: String,
    decision: String,
    reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct AuditSummaryResponse {
    total: i64,
    allowed: i64,
    denied: i64,
    warnings: i64,
}

/// Create routes for security audit endpoints.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/security/audit/events", get(get_audit_events))
        .route("/security/audit/summary", get(get_audit_summary))
        .route("/security/audit/cleanup", delete(cleanup_audit_events))
}

async fn get_audit_events(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<AuditEventsQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    let limit = params.limit.unwrap_or(100).min(1000);

    match state
        .app_state
        .pg_db
        .query_security_audit_events(
            params.task_run_id.as_deref(),
            params.event_type.as_deref(),
            params.decision.as_deref(),
            limit,
        )
        .await
    {
        Ok(rows) => {
            let events: Vec<AuditEventResponse> = rows
                .into_iter()
                .map(|r| AuditEventResponse {
                    id: r.id,
                    timestamp: r.timestamp,
                    task_run_id: r.task_run_id,
                    step_name: r.step_name,
                    event_type: r.event_type,
                    action: r.action,
                    decision: r.decision,
                    reason: r.reason,
                })
                .collect();
            (
                StatusCode::OK,
                Json(serde_json::json!({ "success": true, "data": events })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                serde_json::json!({ "success": false, "error": format!("Failed to query audit events: {}", e) }),
            ),
        ),
    }
}

async fn get_audit_summary(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<AuditSummaryQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    match state
        .app_state
        .pg_db
        .security_audit_summary(params.task_run_id.as_deref())
        .await
    {
        Ok(row) => {
            let summary = AuditSummaryResponse {
                total: row.total,
                allowed: row.allowed,
                denied: row.denied,
                warnings: row.warnings,
            };
            (
                StatusCode::OK,
                Json(serde_json::json!({ "success": true, "data": summary })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                serde_json::json!({ "success": false, "error": format!("Failed to get audit summary: {}", e) }),
            ),
        ),
    }
}

async fn cleanup_audit_events(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<AuditCleanupQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    let days = params.older_than_days.unwrap_or(30);

    match state.app_state.pg_db.cleanup_old_audit_events(days).await {
        Ok(deleted) => (
            StatusCode::OK,
            Json(serde_json::json!({ "success": true, "deleted": deleted })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                serde_json::json!({ "success": false, "error": format!("Failed to cleanup: {}", e) }),
            ),
        ),
    }
}
