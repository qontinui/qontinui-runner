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
use crate::database::CheckpointDb;
use crate::error_monitor::storage::ErrorEventStorage;
use crate::error_monitor::types::{ErrorQuery, ErrorStatus, ErrorSummary, StoredErrorEvent};

// =============================================================================
// Error Event Commands
// =============================================================================

/// Query error events with filters.
#[tauri::command]
pub async fn query_error_events(
    db: State<'_, Arc<CheckpointDb>>,
    query: ErrorQuery,
) -> Result<Vec<StoredErrorEvent>, String> {
    tracing::info!(
        "[ERROR_MONITOR] query_error_events called with query: {:?}",
        query
    );
    let db = db.inner().clone();
    let result = tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        ErrorEventStorage::query(&conn, &query)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?;

    match &result {
        Ok(events) => tracing::info!(
            "[ERROR_MONITOR] query_error_events returning {} events",
            events.len()
        ),
        Err(e) => tracing::error!("[ERROR_MONITOR] query_error_events error: {}", e),
    }
    result
}

/// Get a single error event by ID.
#[tauri::command]
pub async fn get_error_event(
    db: State<'_, Arc<CheckpointDb>>,
    id: i64,
) -> Result<Option<StoredErrorEvent>, String> {
    let db = db.inner().clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        ErrorEventStorage::get_by_id(&conn, id)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Get unresolved error events (for debug agent).
#[tauri::command]
pub async fn get_unresolved_errors(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<AppState>>,
    task_run_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<StoredErrorEvent>, String> {
    let limit = limit.unwrap_or(100);

    // Try PG first
    match app_state
        .pg_db
        .get_unresolved_errors(task_run_id.as_deref(), limit)
        .await
    {
        Ok(pg_rows) if !pg_rows.is_empty() => {
            // Convert serde_json::Value rows to StoredErrorEvent
            let events: Vec<StoredErrorEvent> = pg_rows
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            if !events.is_empty() {
                return Ok(events);
            }
        }
        _ => {}
    }

    // Fallback to SQLite
    let db = db.inner().clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        ErrorEventStorage::get_unresolved(&conn, task_run_id.as_deref(), limit)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Update the status of an error event.
#[tauri::command]
pub async fn update_error_status(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<AppState>>,
    id: i64,
    status: String,
    resolution_notes: Option<String>,
) -> Result<(), String> {
    let status_enum =
        ErrorStatus::from_str(&status).ok_or_else(|| format!("Invalid status: {}", status))?;

    // Fire-and-forget PG dual-write
    let pg = app_state.pg_db.clone();
    let status_str = status.clone();
    let notes = resolution_notes.clone();
    tokio::spawn(async move {
        if let Err(e) = pg.update_error_status(id, &status_str, notes.as_deref()).await {
            tracing::warn!("PG update_error_status failed: {}", e);
        }
    });

    let db = db.inner().clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        ErrorEventStorage::update_status(&conn, id, status_enum, resolution_notes.as_deref())
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Acknowledge an error (mark as seen).
#[tauri::command]
pub async fn acknowledge_error(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<AppState>>,
    id: i64,
) -> Result<(), String> {
    // Fire-and-forget PG dual-write
    let pg = app_state.pg_db.clone();
    tokio::spawn(async move {
        if let Err(e) = pg.update_error_status(id, "acknowledged", None).await {
            tracing::warn!("PG acknowledge_error failed: {}", e);
        }
    });

    let db = db.inner().clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        ErrorEventStorage::update_status(&conn, id, ErrorStatus::Acknowledged, None)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Mark an error as resolved.
#[tauri::command]
pub async fn resolve_error(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<AppState>>,
    id: i64,
    resolution_notes: Option<String>,
    resolved_by_task_run_id: Option<String>,
) -> Result<(), String> {
    // Fire-and-forget PG dual-write
    let pg = app_state.pg_db.clone();
    let notes = resolution_notes.clone();
    let task_id = resolved_by_task_run_id.clone();
    tokio::spawn(async move {
        let result = if let Some(ref tid) = task_id {
            pg.mark_resolved_by_task(id, tid, notes.as_deref()).await
        } else {
            pg.update_error_status(id, "resolved", notes.as_deref()).await
        };
        if let Err(e) = result {
            tracing::warn!("PG resolve_error failed: {}", e);
        }
    });

    let db = db.inner().clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        if let Some(ref task_run_id) = resolved_by_task_run_id {
            ErrorEventStorage::mark_resolved_by_task(
                &conn,
                id,
                task_run_id,
                resolution_notes.as_deref(),
            )
        } else {
            ErrorEventStorage::update_status(
                &conn,
                id,
                ErrorStatus::Resolved,
                resolution_notes.as_deref(),
            )
        }
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Ignore an error (don't track for resolution).
#[tauri::command]
pub async fn ignore_error(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<AppState>>,
    id: i64,
    reason: Option<String>,
) -> Result<(), String> {
    // Fire-and-forget PG dual-write
    let pg = app_state.pg_db.clone();
    let notes = reason.clone();
    tokio::spawn(async move {
        if let Err(e) = pg.update_error_status(id, "ignored", notes.as_deref()).await {
            tracing::warn!("PG ignore_error failed: {}", e);
        }
    });

    let db = db.inner().clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        ErrorEventStorage::update_status(&conn, id, ErrorStatus::Ignored, reason.as_deref())
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Link an error to a finding (promote to task_run_finding).
#[tauri::command]
pub async fn link_error_to_finding(
    db: State<'_, Arc<CheckpointDb>>,
    error_id: i64,
    finding_id: i64,
) -> Result<(), String> {
    let db = db.inner().clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        ErrorEventStorage::link_to_finding(&conn, error_id, finding_id)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

// =============================================================================
// Summary and Statistics Commands
// =============================================================================

/// Get error summary statistics.
#[tauri::command]
pub async fn get_error_summary(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<AppState>>,
    task_run_id: Option<String>,
) -> Result<ErrorSummary, String> {
    // Try PG first
    if let Ok(pg_summary) = app_state.pg_db.get_error_summary(task_run_id.as_deref()).await {
        if let Ok(summary) = serde_json::from_value::<ErrorSummary>(pg_summary) {
            return Ok(summary);
        }
    }

    // Fallback to SQLite
    let db = db.inner().clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        ErrorEventStorage::get_summary(&conn, task_run_id.as_deref())
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Search error events by message content.
#[tauri::command]
pub async fn search_errors(
    db: State<'_, Arc<CheckpointDb>>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<StoredErrorEvent>, String> {
    let db = db.inner().clone();
    let limit = limit.unwrap_or(50);
    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        ErrorEventStorage::search(&conn, &query, limit)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Check if there are actionable errors (critical or error severity, unresolved).
#[tauri::command]
pub async fn has_actionable_errors(
    db: State<'_, Arc<CheckpointDb>>,
    app_state: State<'_, Arc<AppState>>,
    task_run_id: Option<String>,
) -> Result<bool, String> {
    // Try PG first
    if let Ok(pg_summary) = app_state.pg_db.get_error_summary(task_run_id.as_deref()).await {
        if let Some(has) = pg_summary.get("hasActionableErrors").and_then(|v| v.as_bool()) {
            return Ok(has);
        }
    }

    // Fallback to SQLite
    let db = db.inner().clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        let summary = ErrorEventStorage::get_summary(&conn, task_run_id.as_deref())?;
        Ok(summary.has_actionable_errors)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Get recent errors (for notification badge).
#[tauri::command]
pub async fn get_recent_errors(
    db: State<'_, Arc<CheckpointDb>>,
    hours: Option<u32>,
    limit: Option<u32>,
) -> Result<Vec<StoredErrorEvent>, String> {
    let db = db.inner().clone();
    let hours = hours.unwrap_or(24);
    let limit = limit.unwrap_or(10);

    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        let captured_after = chrono::Utc::now() - chrono::Duration::hours(hours as i64);

        let query = ErrorQuery {
            captured_after: Some(captured_after.to_rfc3339()),
            status: Some(vec![
                ErrorStatus::New,
                ErrorStatus::Acknowledged,
                ErrorStatus::InProgress,
            ]),
            limit: Some(limit),
            ..Default::default()
        };

        ErrorEventStorage::query(&conn, &query)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Bulk acknowledge all new errors.
#[tauri::command]
pub async fn acknowledge_all_errors(
    db: State<'_, Arc<CheckpointDb>>,
    task_run_id: Option<String>,
) -> Result<u32, String> {
    let db = db.inner().clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;

        // Get all new errors
        let query = ErrorQuery {
            task_run_id,
            status: Some(vec![ErrorStatus::New]),
            ..Default::default()
        };
        let errors = ErrorEventStorage::query(&conn, &query)?;

        // Acknowledge each
        let mut count = 0;
        for error in errors {
            if ErrorEventStorage::update_status(&conn, error.id, ErrorStatus::Acknowledged, None)
                .is_ok()
            {
                count += 1;
            }
        }

        Ok(count)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

// =============================================================================
// Debug Context Curator Commands
// =============================================================================

use crate::error_monitor::curator::{CuratorConfig, DebugContext, DebugContextCurator};

/// Get curated debug context for the AI debug agent.
#[tauri::command]
pub async fn get_debug_context(
    db: State<'_, Arc<CheckpointDb>>,
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
#[tauri::command]
pub async fn get_debug_context_for_ai(
    db: State<'_, Arc<CheckpointDb>>,
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
            $crate::error_monitor::workflow::generate_error_fix_workflow,
            $crate::error_monitor::workflow::generate_single_error_fix_workflow,
            $crate::error_monitor::workflow::check_fixable_errors,
        ]
    };
}
