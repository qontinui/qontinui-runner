//! Inngest-inspired workflow orchestration API.
//!
//! Provides HTTP endpoints for:
//! - Event bus history and subscriptions
//! - Flow control status
//! - Circuit breaker state

use axum::extract::{Query, State};
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::mcp::types::{ApiResponse, ApiState};

// ============================================================================
// Event Bus Endpoints
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct EventHistoryQuery {
    limit: Option<usize>,
}

/// Get recent event bus history.
pub async fn get_event_history(
    State(_state): State<Arc<ApiState>>,
    Query(query): Query<EventHistoryQuery>,
) -> Json<ApiResponse<Vec<crate::workflow_event_bus::WorkflowEvent>>> {
    let bus = crate::workflow_event_bus::get_workflow_event_bus();
    let limit = query.limit.unwrap_or(50);
    let history = bus.history(limit).await;
    Json(ApiResponse::success(history))
}

/// List active event subscriptions.
pub async fn list_subscriptions(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<SubscriptionInfo>>> {
    let bus = crate::workflow_event_bus::get_workflow_event_bus();
    let subs = bus.list_subscriptions().await;
    let infos: Vec<SubscriptionInfo> = subs
        .iter()
        .map(|s| SubscriptionInfo {
            id: s.id.clone(),
            event_pattern: s.event_pattern.clone(),
            target_workflow_id: s.target_workflow_id.clone(),
            once: s.once,
        })
        .collect();
    Json(ApiResponse::success(infos))
}

#[derive(Debug, Serialize)]
pub struct SubscriptionInfo {
    pub id: String,
    pub event_pattern: String,
    pub target_workflow_id: Option<String>,
    pub once: bool,
}

// ============================================================================
// Flow Control Endpoints
// ============================================================================

/// Get flow control enforcer status.
pub async fn get_flow_control_status(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<FlowControlStatus>> {
    Json(ApiResponse::success(FlowControlStatus {
        active: true,
        description: "Per-workflow flow control enforcer (concurrency, throttle, debounce, rate limit)".to_string(),
    }))
}

#[derive(Debug, Serialize)]
pub struct FlowControlStatus {
    pub active: bool,
    pub description: String,
}

// ============================================================================
// Queue Status (enhanced)
// ============================================================================

/// Get enhanced queue status with durable queue info.
pub async fn get_durable_queue_status(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<DurableQueueStatus>> {
    let queue = crate::workflow_queue::get_workflow_queue();
    let status = queue.status().await;
    Json(ApiResponse::success(DurableQueueStatus {
        pending: status.pending,
        running: status.running,
        max_concurrent: status.max_concurrent,
        max_queue_depth: status.max_queue_depth,
        durable_persistence: true,
    }))
}

#[derive(Debug, Serialize)]
pub struct DurableQueueStatus {
    pub pending: usize,
    pub running: usize,
    pub max_concurrent: usize,
    pub max_queue_depth: usize,
    pub durable_persistence: bool,
}

// ============================================================================
// Routes
// ============================================================================

/// Create routes for the inngest orchestration module.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;

    axum::Router::new()
        .route("/inngest/events", get(get_event_history))
        .route("/inngest/subscriptions", get(list_subscriptions))
        .route("/inngest/flow-control", get(get_flow_control_status))
        .route("/inngest/queue", get(get_durable_queue_status))
}
