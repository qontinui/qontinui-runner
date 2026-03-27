//! Tauri commands for the activity timeline (screenpipe-inspired capture history).
//!
//! Provides full-text search, time-range queries, and statistics over captured
//! screen text from UI Bridge snapshots, OCR, and accessibility trees.

use std::sync::Arc;
use tauri::State;
use tracing::{debug, info};

use crate::commands::AppState;
use crate::database::types::*;
use crate::recording::content_filter::ContentFilter;

/// Shared content filter instance (lazy-initialized).
fn get_content_filter() -> &'static ContentFilter {
    use std::sync::OnceLock;
    static FILTER: OnceLock<ContentFilter> = OnceLock::new();
    FILTER.get_or_init(ContentFilter::new)
}

/// Insert a new activity timeline entry (with PII scrubbing and deduplication).
///
/// Content is automatically filtered: PII is scrubbed and sensitive app windows
/// are skipped entirely (returns -1 without storing).
#[tauri::command]
pub async fn insert_activity_entry(
    mut input: ActivityTimelineInput,
    state: State<'_, Arc<AppState>>,
) -> Result<i64, String> {
    let pg = state
        .pg_db
        .as_ref()
        .ok_or_else(|| "PostgreSQL database not available".to_string())?;

    // Content filtering: skip sensitive windows, scrub PII
    let filter = get_content_filter();
    let app = input.app_name.as_deref().unwrap_or("");
    let title = input.window_title.as_deref().unwrap_or("");

    if filter.should_skip(app, title) {
        debug!("Activity timeline: skipping sensitive window '{}'", title);
        return Ok(-1);
    }

    let result = filter.scrub(&input.text_content);
    input.text_content = result.text;

    pg.insert_activity_entry(&input).await
}

/// Full-text search over the activity timeline.
#[tauri::command]
pub async fn search_activity_timeline(
    query: String,
    max_results: Option<i64>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ActivityTimelineSearchResult>, String> {
    let pg = state
        .pg_db
        .as_ref()
        .ok_or_else(|| "PostgreSQL database not available".to_string())?;

    pg.search_timeline(&query, max_results.unwrap_or(50)).await
}

/// Full-text search with optional filters (app_name, source_type, task_run_id).
#[tauri::command]
pub async fn search_activity_timeline_filtered(
    query: String,
    app_name: Option<String>,
    source_type: Option<String>,
    task_run_id: Option<String>,
    max_results: Option<i64>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ActivityTimelineSearchResult>, String> {
    let pg = state
        .pg_db
        .as_ref()
        .ok_or_else(|| "PostgreSQL database not available".to_string())?;

    pg.search_timeline_filtered(
        &query,
        app_name.as_deref(),
        source_type.as_deref(),
        task_run_id.as_deref(),
        max_results.unwrap_or(50),
    )
    .await
}

/// Get timeline entries within a time range (ISO 8601 timestamps).
#[tauri::command]
pub async fn get_activity_timeline_range(
    start_time: String,
    end_time: String,
    max_results: Option<i64>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ActivityTimelineSearchResult>, String> {
    let pg = state
        .pg_db
        .as_ref()
        .ok_or_else(|| "PostgreSQL database not available".to_string())?;

    let start = chrono::DateTime::parse_from_rfc3339(&start_time)
        .map_err(|e| format!("Invalid start_time: {}", e))?;
    let end = chrono::DateTime::parse_from_rfc3339(&end_time)
        .map_err(|e| format!("Invalid end_time: {}", e))?;

    pg.get_timeline_range(start, end, max_results.unwrap_or(100))
        .await
}

/// Get a single timeline entry by ID (full content).
#[tauri::command]
pub async fn get_activity_timeline_entry(
    id: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<ActivityTimelineEntry>, String> {
    let pg = state
        .pg_db
        .as_ref()
        .ok_or_else(|| "PostgreSQL database not available".to_string())?;

    pg.get_timeline_entry(id).await
}

/// Get all timeline entries for a specific task run.
#[tauri::command]
pub async fn get_activity_timeline_for_task_run(
    task_run_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ActivityTimelineSearchResult>, String> {
    let pg = state
        .pg_db
        .as_ref()
        .ok_or_else(|| "PostgreSQL database not available".to_string())?;

    pg.get_timeline_by_task_run(&task_run_id).await
}

/// Soft-delete a timeline entry.
#[tauri::command]
pub async fn delete_activity_timeline_entry(
    id: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let pg = state
        .pg_db
        .as_ref()
        .ok_or_else(|| "PostgreSQL database not available".to_string())?;

    let deleted = pg.delete_timeline_entry(id).await?;
    if deleted {
        info!("Soft-deleted activity timeline entry {}", id);
    }
    Ok(deleted)
}

/// Get capture statistics (counts by source_type and capture_mode).
#[tauri::command]
pub async fn get_activity_timeline_stats(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ActivityTimelineStat>, String> {
    let pg = state
        .pg_db
        .as_ref()
        .ok_or_else(|| "PostgreSQL database not available".to_string())?;

    pg.get_timeline_stats().await
}
