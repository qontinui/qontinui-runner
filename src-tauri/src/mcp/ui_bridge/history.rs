//! Element interaction history endpoints (persisted cross-run data).
//!
//! Thin wrappers over `crate::database::ui_bridge_ops` queries that expose
//! per-element reliability and flakiness analytics to the UI.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::error;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// Query parameters for element interaction history.
#[derive(Debug, Deserialize)]
pub struct HistoryElementsQuery {
    pub task_run_id: i64,
}

/// Get all UI Bridge events for a specific task run.
pub async fn ui_bridge_history_elements_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<HistoryElementsQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::UiBridgeEvent>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_element_interactions(query.task_run_id)
        .await
    {
        Ok(events) => Ok(Json(ApiResponse::success(events))),
        Err(e) => {
            error!("UI Bridge history: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Query parameters for single element history.
#[derive(Debug, Deserialize)]
pub struct HistoryElementQuery {
    #[serde(default = "default_history_limit")]
    pub limit: i64,
}

fn default_history_limit() -> i64 {
    50
}

/// Get cross-run interaction history for a single element.
pub async fn ui_bridge_history_element_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<HistoryElementQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::UiBridgeEvent>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_element_history(&id, query.limit)
        .await
    {
        Ok(events) => Ok(Json(ApiResponse::success(events))),
        Err(e) => {
            error!("UI Bridge element history: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Query parameters for flaky element detection.
#[derive(Debug, Deserialize)]
pub struct FlakyElementsQuery {
    #[serde(default = "default_min_interactions")]
    pub min_interactions: i64,
    #[serde(default = "default_max_success_rate")]
    pub max_success_rate: f64,
}

fn default_min_interactions() -> i64 {
    10
}

fn default_max_success_rate() -> f64 {
    0.8
}

/// Get elements with high failure rates across runs.
pub async fn ui_bridge_history_flaky_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<FlakyElementsQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::ElementReliability>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_flaky_elements(query.min_interactions, query.max_success_rate)
        .await
    {
        Ok(elements) => Ok(Json(ApiResponse::success(elements))),
        Err(e) => {
            error!("UI Bridge flaky elements: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Query parameters for element reliability.
#[derive(Debug, Deserialize)]
pub struct ElementReliabilityQuery {
    pub element_id: String,
}

/// Get reliability data for a single element across all runs.
pub async fn ui_bridge_element_reliability_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ElementReliabilityQuery>,
) -> Result<
    Json<ApiResponse<Option<crate::database::ui_bridge_ops::ElementReliability>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_element_reliability(&query.element_id)
        .await
    {
        Ok(reliability) => Ok(Json(ApiResponse::success(reliability))),
        Err(e) => {
            error!("UI Bridge element reliability: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/ui-bridge/history/elements",
            get(ui_bridge_history_elements_handler),
        )
        .route(
            "/ui-bridge/history/element/{id}",
            get(ui_bridge_history_element_handler),
        )
        .route(
            "/ui-bridge/history/flaky",
            get(ui_bridge_history_flaky_handler),
        )
        .route(
            "/ui-bridge/graph/element-reliability",
            get(ui_bridge_element_reliability_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/history/elements"),
        ("GET", "/ui-bridge/history/element/{id}"),
        ("GET", "/ui-bridge/history/flaky"),
        ("GET", "/ui-bridge/graph/element-reliability"),
    ]
}
