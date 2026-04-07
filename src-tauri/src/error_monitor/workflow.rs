//! Error fix workflow commands.
//!
//! The legacy SQLite-backed `ErrorFixWorkflowGenerator` has been replaced by
//! `database::pg::tiered_info::generate_error_fix_workflow`, which is invoked
//! from the MCP error_monitor handlers. This module now only exposes the
//! shared result type and the `check_fixable_errors` Tauri command.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Result of workflow generation, produced by the PG-backed generator and
/// returned through the MCP error_monitor endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedWorkflow {
    /// The unified workflow configuration as JSON
    pub workflow_json: Value,
    /// Human-readable description of what the workflow will do
    pub description: String,
    /// Number of errors targeted
    pub error_count: u32,
    /// Whether there are critical errors
    pub has_critical: bool,
}

/// Check if there are fixable errors and return a summary.
#[tauri::command]
pub async fn check_fixable_errors(
    app_state: tauri::State<'_, std::sync::Arc<crate::commands::AppState>>,
    task_run_id: Option<String>,
) -> Result<FixableErrorsSummary, String> {
    let statuses = ["new", "acknowledged", "in_progress", "promoted"];

    let pg_rows = app_state
        .pg_db
        .query_error_events(
            task_run_id.as_deref(),
            Some(&statuses),
            None,
            None,
            None,
            None,
        )
        .await?;

    let errors: Vec<crate::error_monitor::types::StoredErrorEvent> = pg_rows
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();

    let critical_count = errors
        .iter()
        .filter(|e| e.severity == crate::error_monitor::types::ErrorSeverity::Critical)
        .count();

    let error_count = errors
        .iter()
        .filter(|e| e.severity == crate::error_monitor::types::ErrorSeverity::Error)
        .count();

    let warning_count = errors
        .iter()
        .filter(|e| e.severity == crate::error_monitor::types::ErrorSeverity::Warning)
        .count();

    Ok(FixableErrorsSummary {
        total: errors.len() as u32,
        critical_count: critical_count as u32,
        error_count: error_count as u32,
        warning_count: warning_count as u32,
        can_generate_workflow: !errors.is_empty(),
        recommended_action: if critical_count > 0 {
            "Immediate fix recommended - critical errors present".to_string()
        } else if error_count > 0 {
            "Fix recommended - errors present".to_string()
        } else if warning_count > 0 {
            "Optional fix - only warnings present".to_string()
        } else {
            "No action needed".to_string()
        },
    })
}

/// Summary of fixable errors.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixableErrorsSummary {
    /// Total number of fixable errors
    pub total: u32,
    /// Number of critical errors
    pub critical_count: u32,
    /// Number of regular errors
    pub error_count: u32,
    /// Number of warnings
    pub warning_count: u32,
    /// Whether a fix workflow can be generated
    pub can_generate_workflow: bool,
    /// Recommended action
    pub recommended_action: String,
}
