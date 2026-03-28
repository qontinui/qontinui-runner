//! Tauri commands for error monitoring.
//!
//! This module provides IPC commands for the frontend to:
//! - Query error events
//! - Update error status
//! - Get error summaries and statistics
//!
//! Log source management is handled through global log source settings
//! (Settings > Log Sources). See `commands::global_log_sources`.

use std::sync::Arc;

use tauri::State;

use crate::commands::AppState;
use crate::error_monitor::types::{ErrorStatus, ErrorSummary, StoredErrorEvent};

// =============================================================================
// Error Event Commands
// =============================================================================

/// Query error events with filters.
#[tauri::command]
pub async fn query_error_events(
    app_state: State<'_, Arc<AppState>>,
    query: crate::error_monitor::types::ErrorQuery,
) -> Result<Vec<StoredErrorEvent>, String> {
    tracing::info!(
        "[ERROR_MONITOR] query_error_events called with query: {:?}",
        query
    );

    let status_strs: Option<Vec<String>> = query.status.as_ref().map(|statuses| {
        statuses.iter().map(|s| s.as_str().to_string()).collect()
    });
    let status_refs: Option<Vec<&str>> = status_strs.as_ref().map(|v| v.iter().map(|s| s.as_str()).collect());

    let severity_strs: Option<Vec<String>> = query.severity.as_ref().map(|sevs| {
        sevs.iter().map(|s| s.as_str().to_string()).collect()
    });
    let severity_refs: Option<Vec<&str>> = severity_strs.as_ref().map(|v| v.iter().map(|s| s.as_str()).collect());

    let pg_rows = app_state
        .pg_db
        .query_error_events(
            query.task_run_id.as_deref(),
            status_refs.as_deref(),
            severity_refs.as_deref(),
            query.log_source_name.as_deref(),
            query.captured_after.as_deref(),
            query.limit,
        )
        .await?;

    let events: Vec<StoredErrorEvent> = pg_rows
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();

    tracing::info!(
        "[ERROR_MONITOR] query_error_events returning {} events",
        events.len()
    );

    Ok(events)
}

/// Get a single error event by ID.
#[tauri::command]
pub async fn get_error_event(
    app_state: State<'_, Arc<AppState>>,
    id: i64,
) -> Result<Option<StoredErrorEvent>, String> {
    match app_state.pg_db.get_error_event_by_id(id).await? {
        Some(pg_event) => {
            let event: StoredErrorEvent = serde_json::from_value(pg_event)
                .map_err(|e| format!("Failed to deserialize error event: {}", e))?;
            Ok(Some(event))
        }
        None => Ok(None),
    }
}

/// Get unresolved error events (for debug agent).
#[tauri::command]
pub async fn get_unresolved_errors(
    app_state: State<'_, Arc<AppState>>,
    task_run_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<StoredErrorEvent>, String> {
    let limit = limit.unwrap_or(100);

    let pg_rows = app_state
        .pg_db
        .get_unresolved_errors(task_run_id.as_deref(), limit)
        .await?;

    let events: Vec<StoredErrorEvent> = pg_rows
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();

    Ok(events)
}

/// Update the status of an error event.
#[tauri::command]
pub async fn update_error_status(
    app_state: State<'_, Arc<AppState>>,
    id: i64,
    status: String,
    resolution_notes: Option<String>,
) -> Result<(), String> {
    let _status_enum =
        ErrorStatus::from_str(&status).ok_or_else(|| format!("Invalid status: {}", status))?;

    app_state
        .pg_db
        .update_error_status(id, &status, resolution_notes.as_deref())
        .await
}

/// Acknowledge an error (mark as seen).
#[tauri::command]
pub async fn acknowledge_error(
    app_state: State<'_, Arc<AppState>>,
    id: i64,
) -> Result<(), String> {
    app_state
        .pg_db
        .update_error_status(id, "acknowledged", None)
        .await
}

/// Mark an error as resolved.
#[tauri::command]
pub async fn resolve_error(
    app_state: State<'_, Arc<AppState>>,
    id: i64,
    resolution_notes: Option<String>,
    resolved_by_task_run_id: Option<String>,
) -> Result<(), String> {
    if let Some(ref task_run_id) = resolved_by_task_run_id {
        app_state
            .pg_db
            .mark_resolved_by_task(id, task_run_id, resolution_notes.as_deref())
            .await
    } else {
        app_state
            .pg_db
            .update_error_status(id, "resolved", resolution_notes.as_deref())
            .await
    }
}

/// Ignore an error (don't track for resolution).
#[tauri::command]
pub async fn ignore_error(
    app_state: State<'_, Arc<AppState>>,
    id: i64,
    reason: Option<String>,
) -> Result<(), String> {
    app_state
        .pg_db
        .update_error_status(id, "ignored", reason.as_deref())
        .await
}

/// Link an error to a finding (promote to task_run_finding).
#[tauri::command]
pub async fn link_error_to_finding(
    app_state: State<'_, Arc<AppState>>,
    error_id: i64,
    finding_id: i64,
) -> Result<(), String> {
    app_state
        .pg_db
        .link_error_to_finding(error_id, finding_id)
        .await
}

// =============================================================================
// Summary and Statistics Commands
// =============================================================================

/// Get error summary statistics.
#[tauri::command]
pub async fn get_error_summary(
    app_state: State<'_, Arc<AppState>>,
    task_run_id: Option<String>,
) -> Result<ErrorSummary, String> {
    let pg_summary = app_state
        .pg_db
        .get_error_summary(task_run_id.as_deref())
        .await?;

    serde_json::from_value::<ErrorSummary>(pg_summary)
        .map_err(|e| format!("Failed to deserialize error summary: {}", e))
}

/// Search error events by message content.
#[tauri::command]
pub async fn search_errors(
    app_state: State<'_, Arc<AppState>>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<StoredErrorEvent>, String> {
    let limit = limit.unwrap_or(50);

    let pg_rows = app_state.pg_db.search_errors(&query, limit).await?;

    let events: Vec<StoredErrorEvent> = pg_rows
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();

    Ok(events)
}

/// Check if there are actionable errors (critical or error severity, unresolved).
#[tauri::command]
pub async fn has_actionable_errors(
    app_state: State<'_, Arc<AppState>>,
    task_run_id: Option<String>,
) -> Result<bool, String> {
    let pg_summary = app_state
        .pg_db
        .get_error_summary(task_run_id.as_deref())
        .await?;

    Ok(pg_summary
        .get("hasActionableErrors")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

/// Get recent errors (for notification badge).
#[tauri::command]
pub async fn get_recent_errors(
    app_state: State<'_, Arc<AppState>>,
    hours: Option<u32>,
    limit: Option<u32>,
) -> Result<Vec<StoredErrorEvent>, String> {
    let hours = hours.unwrap_or(24);
    let limit = limit.unwrap_or(10);

    let captured_after = chrono::Utc::now() - chrono::Duration::hours(hours as i64);
    let statuses = ["new", "acknowledged", "in_progress"];

    let pg_rows = app_state
        .pg_db
        .query_error_events(
            None,
            Some(&statuses),
            None,
            None,
            Some(&captured_after.to_rfc3339()),
            Some(limit),
        )
        .await?;

    let events: Vec<StoredErrorEvent> = pg_rows
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();

    Ok(events)
}

/// Bulk acknowledge all new errors.
#[tauri::command]
pub async fn acknowledge_all_errors(
    app_state: State<'_, Arc<AppState>>,
    task_run_id: Option<String>,
) -> Result<u32, String> {
    app_state
        .pg_db
        .acknowledge_all_errors(task_run_id.as_deref())
        .await
}

// =============================================================================
// Debug Context Curator Commands
// =============================================================================

use crate::error_monitor::curator::{CuratorConfig, DebugContext, DebugContextCurator};

/// Get curated debug context for the AI debug agent.
///
/// Note: The curator uses SQLite connection internally for complex cross-table queries.
/// This is retained because the curator logic is deeply coupled to SQLite.
#[tauri::command]
pub async fn get_debug_context(
    db: State<'_, Arc<crate::database::CheckpointDb>>,
    task_run_id: Option<String>,
    max_errors: Option<usize>,
) -> Result<DebugContext, String> {
    let db = db.inner().clone();
    let config = CuratorConfig {
        max_errors: max_errors.unwrap_or(50),
        ..Default::default()
    };

    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        let curator = DebugContextCurator::with_config(config);
        curator.build_context(&conn, task_run_id.as_deref())
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Get debug context formatted as a string for AI consumption.
///
/// Note: The curator uses SQLite connection internally for complex cross-table queries.
/// This is retained because the curator logic is deeply coupled to SQLite.
#[tauri::command]
pub async fn get_debug_context_for_ai(
    db: State<'_, Arc<crate::database::CheckpointDb>>,
    task_run_id: Option<String>,
) -> Result<String, String> {
    let db = db.inner().clone();

    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        let curator = DebugContextCurator::new();
        let context = curator.build_context(&conn, task_run_id.as_deref())?;
        Ok(curator.format_for_ai(&context))
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Open a file in the user's editor at a specific line.
/// Tries VS Code first, then falls back to the system default.
#[tauri::command]
pub async fn open_error_in_editor(
    file_path: String,
    line_number: Option<u32>,
    column_number: Option<u32>,
) -> Result<(), String> {
    let line = line_number.unwrap_or(1);
    let col = column_number.unwrap_or(1);

    // Try VS Code first (most common for developers)
    let vscode_arg = format!("{}:{}:{}", file_path, line, col);
    let vscode_result = std::process::Command::new("code")
        .args(["--goto", &vscode_arg])
        .spawn();

    if vscode_result.is_ok() {
        return Ok(());
    }

    // Fallback: try to open just the file with the default handler
    open::that(&file_path).map_err(|e| format!("Failed to open file: {}", e))?;
    Ok(())
}

/// Get all commands that should be registered with Tauri.
///
/// Use this in your main.rs to register all error monitoring commands:
/// ```ignore
/// .invoke_handler(tauri::generate_handler![
///     ...error_monitor::commands::all_commands(),
///     ...other_commands,
/// ])
/// ```
#[macro_export]
macro_rules! error_monitor_commands {
    () => {
        [
            $crate::error_monitor::commands::list_log_sources,
            $crate::error_monitor::commands::get_log_source,
            $crate::error_monitor::commands::add_log_source,
            $crate::error_monitor::commands::update_log_source,
            $crate::error_monitor::commands::delete_log_source,
            $crate::error_monitor::commands::toggle_log_source,
            $crate::error_monitor::commands::query_error_events,
            $crate::error_monitor::commands::get_error_event,
            $crate::error_monitor::commands::get_unresolved_errors,
            $crate::error_monitor::commands::update_error_status,
            $crate::error_monitor::commands::acknowledge_error,
            $crate::error_monitor::commands::resolve_error,
            $crate::error_monitor::commands::ignore_error,
            $crate::error_monitor::commands::link_error_to_finding,
            $crate::error_monitor::commands::get_error_summary,
            $crate::error_monitor::commands::search_errors,
            $crate::error_monitor::commands::has_actionable_errors,
            $crate::error_monitor::commands::get_recent_errors,
            $crate::error_monitor::commands::acknowledge_all_errors,
            $crate::error_monitor::commands::get_debug_context,
            $crate::error_monitor::commands::get_debug_context_for_ai,
            $crate::error_monitor::commands::open_error_in_editor,
            $crate::error_monitor::workflow::generate_error_fix_workflow,
            $crate::error_monitor::workflow::generate_single_error_fix_workflow,
            $crate::error_monitor::workflow::check_fixable_errors,
        ]
    };
}
