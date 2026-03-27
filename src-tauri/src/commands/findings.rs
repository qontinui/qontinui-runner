//! Tauri commands for finding operations.
//!
//! Provides commands for querying and managing AI-detected findings
//! stored in the checkpoint database.

use std::sync::Arc;
use tauri::State;
use tracing::info;

use crate::commands::AppState;
use crate::database::StoredTaskKnowledge;
use crate::findings::{Finding, FindingStatus, FindingSummary};

/// Get all findings for a task run.
#[tauri::command]
pub async fn get_task_findings(
    task_run_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<Finding>, String> {
    app_state.pg_db.get_findings_for_task(&task_run_id).await
}

/// Get findings by status for a task run.
#[tauri::command]
pub async fn get_findings_by_status_cmd(
    task_run_id: String,
    status: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<Finding>, String> {
    let status =
        FindingStatus::from_str(&status).ok_or_else(|| format!("Invalid status: {}", status))?;

    app_state
        .checkpoint_db
        .get_findings_by_status(&task_run_id, &status)
}

/// Get a single finding by ID.
#[tauri::command]
pub async fn get_finding_by_id(
    finding_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Option<Finding>, String> {
    app_state.pg_db.get_finding(&finding_id).await
}

/// Update finding status.
#[tauri::command]
pub async fn update_finding(
    finding_id: String,
    status: String,
    resolution: Option<String>,
    session_num: Option<u32>,
    app_state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let status =
        FindingStatus::from_str(&status).ok_or_else(|| format!("Invalid status: {}", status))?;

    app_state.pg_db.update_finding_status(
        &finding_id,
        status.as_str(),
        resolution.as_deref(),
        session_num,
    ).await?;

    info!(
        "Updated finding {} to status {}",
        finding_id,
        status.as_str()
    );
    Ok(())
}

/// Resolve a finding.
#[tauri::command]
pub async fn resolve_finding(
    finding_id: String,
    resolution: String,
    session_num: Option<u32>,
    app_state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    app_state.pg_db.update_finding_status(
        &finding_id,
        "resolved",
        Some(&resolution),
        session_num,
    ).await?;

    info!("Resolved finding {}", finding_id);
    Ok(())
}

/// Provide user response for a finding.
#[tauri::command]
pub async fn provide_finding_response(
    finding_id: String,
    response: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    app_state
        .checkpoint_db
        .set_finding_user_response(&finding_id, &response)?;

    info!("Set user response for finding {}", finding_id);
    Ok(())
}

/// Get summary statistics for a task run.
#[tauri::command]
pub async fn get_findings_summary(
    task_run_id: String,
    app_state: State<'_, Arc<AppState>>,
) -> Result<FindingSummary, String> {
    app_state.pg_db.get_finding_summary(&task_run_id).await
}

/// List knowledge entries for a task run with optional category filter.
#[tauri::command]
pub async fn list_task_knowledge_cmd(
    task_run_id: String,
    category: Option<String>,
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<StoredTaskKnowledge>, String> {
    app_state.pg_db.list_task_knowledge(&task_run_id, category.as_deref(), false).await
}
