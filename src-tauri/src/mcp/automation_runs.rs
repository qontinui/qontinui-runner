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
    let conn = state
        .app_state
        .checkpoint_db
        .connection()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let limit = query.limit.unwrap_or(20);

    // If config_id is provided, filter by it. Otherwise get all recent runs.
    let runs = if let Some(config_id) = &query.config_id {
        tiered_info::get_all_recent_runs(&conn, config_id, limit)
    } else {
        // Get runs across all configs - use a broader query
        get_all_recent_runs(&conn, limit)
    };

    match runs {
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
pub fn get_all_recent_runs(
    conn: &rusqlite::Connection,
    limit: u32,
) -> Result<Vec<RunDetails>, String> {
    use rusqlite::params;

    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                tra.id, tr.config_id, tra.workflow_name, tra.started_at, tra.ended_at, tra.duration_ms,
                tra.automation_status, tra.success, tra.error_type, tra.error_message,
                tra.actions_summary, tra.states_visited, tra.transitions_executed,
                tra.template_matches, tra.anomalies
            FROM task_run_automation tra
            INNER JOIN task_runs tr ON tra.task_run_id = tr.id
            ORDER BY tra.started_at DESC
            LIMIT ?1
            "#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let runs = stmt
        .query_map(params![limit], row_to_run_details_from_automation)
        .map_err(|e| format!("Failed to query runs: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(runs)
}

/// Convert a row from task_run_automation to RunDetails.
pub fn row_to_run_details_from_automation(row: &rusqlite::Row) -> rusqlite::Result<RunDetails> {
    let status_str: String = row.get(6)?;
    let actions_json: Option<String> = row.get(10)?;
    let states_json: Option<String> = row.get(11)?;
    let transitions_json: Option<String> = row.get(12)?;
    let templates_json: Option<String> = row.get(13)?;
    let anomalies_json: Option<String> = row.get(14)?;

    Ok(RunDetails {
        id: row.get(0)?,
        config_id: row.get(1)?,
        workflow_name: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        duration_ms: row.get(5)?,
        status: tiered_info::RunStatus::from_str(&status_str)
            .unwrap_or(tiered_info::RunStatus::Running),
        success: row.get(7)?,
        error_type: row.get(8)?,
        error_message: row.get(9)?,
        actions_summary: actions_json.and_then(|j| serde_json::from_str(&j).ok()),
        states_visited: states_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        transitions_executed: transitions_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        template_matches: templates_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        anomalies: anomalies_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        screenshots: Vec::new(),
    })
}

/// Get a specific automation run by ID from task_run_automation.
pub async fn get_automation_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use rusqlite::{params, OptionalExtension};

    let conn = state
        .app_state
        .checkpoint_db
        .connection()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let result: Result<Option<RunDetails>, String> = conn
        .query_row(
            r#"
            SELECT
                tra.id, tr.config_id, tra.workflow_name, tra.started_at, tra.ended_at, tra.duration_ms,
                tra.automation_status, tra.success, tra.error_type, tra.error_message,
                tra.actions_summary, tra.states_visited, tra.transitions_executed,
                tra.template_matches, tra.anomalies
            FROM task_run_automation tra
            INNER JOIN task_runs tr ON tra.task_run_id = tr.id
            WHERE tra.id = ?1
            "#,
            params![id],
            row_to_run_details_from_automation,
        )
        .optional()
        .map_err(|e| format!("Failed to get automation run: {}", e));

    match result {
        Ok(Some(run)) => Ok(Json(serde_json::json!({
            "success": true,
            "data": run
        }))),
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
        .route("/runs/:id", get(get_automation_run))
}
