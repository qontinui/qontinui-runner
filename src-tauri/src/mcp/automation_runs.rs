//! Automation run handlers for MCP API
//!
//! Provides HTTP handlers for querying automation execution history.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::ApiState;
use crate::tiered_info::{self, RunDetails};

// ============================================================================
// Automation Run HTTP API Handlers (for MCP/AI access)
// ============================================================================

/// Query params for listing automation runs.
#[derive(Debug, Deserialize)]
pub struct ListAutomationRunsQuery {
    /// Config ID to filter by (optional)
    config_id: Option<String>,
    /// Maximum number of runs to return (default: 20)
    limit: Option<u32>,
}

/// List recent automation runs.
pub async fn list_automation_runs(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<ListAutomationRunsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(20);
    let config_id = query.config_id.clone();

    // PG-first: use the tiered_info PG module
    let pg_db = &state.app_state.pg_db;
    match pg_db.get_recent_runs(config_id.as_deref(), limit).await {
        Ok(runs) => Ok(Json(serde_json::json!({
            "success": true,
            "data": runs
        }))),
        Err(e) => Ok(Json(serde_json::json!({
            "success": false,
            "error": e
        }))),
    }
}

/// Helper to get recent runs across all configs from task_run_automation.
pub fn get_all_recent_runs(limit: u32) -> Result<Vec<RunDetails>, String> {
    Err("SQLite removed".to_string())
}

// row_to_run_details_from_automation removed (SQLite dead code)

/// Get a specific automation run by ID from task_run_automation.
pub async fn get_automation_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // PG-first: use task_run_automation_by_id
    let pg_db = &state.app_state.pg_db;
    match pg_db.get_task_run_automation_by_id(&id).await {
        Ok(Some(automation)) => {
            // Convert to RunDetails-like JSON
            Ok(Json(serde_json::json!({
                "success": true,
                "data": automation
            })))
        }
        Ok(None) => Ok(Json(serde_json::json!({
            "success": false,
            "error": format!("Run not found: {}", id)
        }))),
        Err(e) => Ok(Json(serde_json::json!({
            "success": false,
            "error": e
        }))),
    }
}

// ============================================================================
// End Automation Run HTTP API Handlers
// ============================================================================

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route("/runs", get(list_automation_runs))
        .route("/runs/{id}", get(get_automation_run))
}
