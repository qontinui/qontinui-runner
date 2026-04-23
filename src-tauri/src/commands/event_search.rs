//! Tauri command for unified event search across four event tables.
//!
//! Exposes the `event_search` Clorinde query (UNION ALL over activity_timeline,
//! observations, deferred_questions, error_events with ts_rank_cd scoring) to
//! the frontend. See queries/event_search.sql for the underlying query and
//! database/pg/event_search.rs for the PgDb wrapper.

use std::sync::Arc;

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;

use crate::commands::AppState;

/// Presentation-layer row returned to the frontend.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SearchEventResult {
    /// One of: "activity_timeline", "observations", "deferred_questions", "error_events".
    pub source_table: String,
    /// Record id (stringified — deferred_questions.id is TEXT, others are BIGSERIAL cast to TEXT).
    pub record_id: String,
    /// First 400 characters of the matching text.
    pub snippet: String,
    /// Event timestamp, serialized as RFC 3339.
    pub ts: String,
    /// ts_rank_cd relevance score (higher = more relevant).
    pub score: f64,
}

/// Unified full-text search across activity_timeline, observations,
/// deferred_questions, and error_events.
///
/// - `query`: required, non-empty plainto_tsquery input.
/// - `since_iso`: optional RFC 3339 timestamp. Defaults to now - 7 days.
/// - `limit`: optional, defaults to 100, capped at 500.
#[tauri::command]
pub async fn search_events(
    state: State<'_, Arc<AppState>>,
    query: String,
    since_iso: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<SearchEventResult>, String> {
    if query.trim().is_empty() {
        return Err("query must be non-empty".to_string());
    }

    let since_ts = match since_iso {
        Some(s) => chrono::DateTime::parse_from_rfc3339(&s)
            .map_err(|e| format!("Invalid since_iso: {}", e))?,
        None => (Utc::now() - Duration::days(7)).into(),
    };

    let max_results = limit.unwrap_or(100).clamp(1, 500);

    let rows = state
        .pg_db
        .search_events(&query, since_ts, max_results)
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| SearchEventResult {
            source_table: r.source_table,
            record_id: r.record_id,
            snippet: r.snippet,
            ts: r.event_ts.to_rfc3339(),
            score: r.score as f64,
        })
        .collect())
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// See `commands/mod.rs` for the migration guide explaining the plugin pattern.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_event_search")
        .invoke_handler(tauri::generate_handler![search_events,])
        .build()
}
