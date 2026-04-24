//! Tauri commands for the activity timeline (screenpipe-inspired capture history).
//!
//! Provides full-text search, time-range queries, and statistics over captured
//! screen text from UI Bridge snapshots, OCR, and accessibility trees.

use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
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
    let pg = &state.pg_db;

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
    let pg = &state.pg_db;

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
    let pg = &state.pg_db;

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
    let pg = &state.pg_db;

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
    let pg = &state.pg_db;

    pg.get_timeline_entry(id).await
}

/// Get all timeline entries for a specific task run.
#[tauri::command]
pub async fn get_activity_timeline_for_task_run(
    task_run_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ActivityTimelineSearchResult>, String> {
    let pg = &state.pg_db;

    pg.get_timeline_by_task_run(&task_run_id).await
}

/// Soft-delete a timeline entry.
#[tauri::command]
pub async fn delete_activity_timeline_entry(
    id: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let pg = &state.pg_db;

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
    let pg = &state.pg_db;

    pg.get_timeline_stats().await
}

// ============================================================================
// Scripted-output aggregates (Phase C of script-emitter-wiring plan)
// ============================================================================

/// Per-run (or global) aggregate counters for `source_type = 'scripted_output'`
/// activity-timeline events.
///
/// Fed by [`get_scripted_output_stats`] and consumed by the
/// `ScriptedOutputPanel` widget on the LLM Analytics page.
///
/// **Event shapes** (Phase A — see `step_output/script_emitter.rs`):
/// - `scripted_output.cache_hit` — metadata `{ "tier": 1 }`
/// - `scripted_output.llm_ok` — metadata `{ "tokens_in", "tokens_out", "model" }`
/// - `scripted_output.fallback` — metadata `{ "reason": <string> }`
///
/// **Event shapes** (Phase B, not yet emitted — we tolerate their absence):
/// - `scripted_output.attempted`
/// - `scripted_output.worker_ok`
/// - `scripted_output.bytes_avoided` — metadata `{ "bytes_avoided": <number> }`
///
/// If Phase B never lands, the TS-side counts just stay 0.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptedOutputStats {
    /// Count of `scripted_output.attempted` events (Phase B — may be 0).
    pub attempted: u64,
    /// Count of `scripted_output.cache_hit` events.
    pub cache_hit: u64,
    /// Count of `scripted_output.llm_ok` events.
    pub llm_ok: u64,
    /// Count of `scripted_output.worker_ok` events (Phase B — may be 0).
    pub worker_ok: u64,
    /// Total `bytes_avoided` summed across both Phase-B
    /// `scripted_output.bytes_avoided` events (if emitted) AND any
    /// `scripted_output.llm_ok` events that carry a `bytes_avoided` field.
    pub bytes_avoided: u64,
    /// Map of `scripted_output.fallback`'s `reason` → count.
    pub fallbacks: std::collections::HashMap<String, u64>,
    /// Total input tokens summed across `scripted_output.llm_ok` events.
    pub total_tokens_in: u64,
    /// Total output tokens summed across `scripted_output.llm_ok` events.
    pub total_tokens_out: u64,
    /// Sum of `cache_creation_tokens` across all `scripted_output.llm_ok` events.
    pub cache_creation_tokens: u64,
    /// Sum of `cache_read_tokens` across all `scripted_output.llm_ok` events.
    pub cache_read_tokens: u64,
}

/// Aggregate scripted-output activity-timeline events into a single
/// stat block. If `task_run_id` is `Some`, scopes to that run; otherwise
/// aggregates across all runs (including the "unassigned" bucket).
///
/// This does one indexed scan of `activity_timeline` and aggregates in
/// memory — cheap for current volumes (expect <1k rows per task_run).
#[tauri::command]
pub async fn get_scripted_output_stats(
    task_run_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<ScriptedOutputStats, String> {
    let pg = &state.pg_db;
    let rows = pg.get_scripted_output_rows(task_run_id.as_deref()).await?;

    let mut stats = ScriptedOutputStats {
        attempted: 0,
        cache_hit: 0,
        llm_ok: 0,
        worker_ok: 0,
        bytes_avoided: 0,
        fallbacks: std::collections::HashMap::new(),
        total_tokens_in: 0,
        total_tokens_out: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };

    for (text_content, metadata_json) in rows {
        // Parse metadata lazily — many events have None or unparseable JSON.
        let meta: Option<serde_json::Value> = metadata_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());

        match text_content.as_str() {
            "scripted_output.attempted" => stats.attempted += 1,
            "scripted_output.cache_hit" => stats.cache_hit += 1,
            "scripted_output.worker_ok" => stats.worker_ok += 1,
            "scripted_output.bytes_avoided" => {
                if let Some(n) = meta.as_ref().and_then(read_bytes_avoided) {
                    stats.bytes_avoided = stats.bytes_avoided.saturating_add(n);
                }
            }
            "scripted_output.llm_ok" => {
                stats.llm_ok += 1;
                if let Some(m) = meta.as_ref() {
                    if let Some(n) = m.get("tokens_in").and_then(|v| v.as_u64()) {
                        stats.total_tokens_in = stats.total_tokens_in.saturating_add(n);
                    }
                    if let Some(n) = m.get("tokens_out").and_then(|v| v.as_u64()) {
                        stats.total_tokens_out = stats.total_tokens_out.saturating_add(n);
                    }
                    if let Some(n) = m.get("cache_creation_tokens").and_then(|v| v.as_u64()) {
                        stats.cache_creation_tokens = stats.cache_creation_tokens.saturating_add(n);
                    }
                    if let Some(n) = m.get("cache_read_tokens").and_then(|v| v.as_u64()) {
                        stats.cache_read_tokens = stats.cache_read_tokens.saturating_add(n);
                    }
                    // Some Phase-B builds may roll `bytes_avoided` into the
                    // llm_ok event. Include it here defensively.
                    if let Some(n) = read_bytes_avoided(m) {
                        stats.bytes_avoided = stats.bytes_avoided.saturating_add(n);
                    }
                }
            }
            "scripted_output.fallback" => {
                let reason = meta
                    .as_ref()
                    .and_then(|m| m.get("reason"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                *stats.fallbacks.entry(reason).or_insert(0) += 1;
            }
            _ => {
                // Ignore unknown `scripted_output.*` events — forward compat.
            }
        }
    }

    Ok(stats)
}

/// Extract a `bytes_avoided` integer from a metadata JSON object, tolerating
/// both integer and floating-point representations.
fn read_bytes_avoided(meta: &serde_json::Value) -> Option<u64> {
    meta.get("bytes_avoided")
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_activity_timeline")
        .invoke_handler(tauri::generate_handler![
            insert_activity_entry,
            search_activity_timeline,
            search_activity_timeline_filtered,
            get_activity_timeline_range,
            get_activity_timeline_entry,
            get_activity_timeline_for_task_run,
            delete_activity_timeline_entry,
            get_activity_timeline_stats,
            get_scripted_output_stats,
        ])
        .build()
}
