//! Tauri command behind the knowledge browser (`src/components/knowledge/`,
//! Ctrl+Shift+E). Reads `productivity_knowledge` through the PG FTS path.
//!
//! Rehomed from `commands::productivity` by Phase 3 of
//! `2026-09-12-consolidate-local-orchestration-onto-conductor`; the command
//! name `search_knowledge` is unchanged so the frontend `knowledgeApi.ts`
//! wrapper needs no edit.

use crate::commands::require_app_state;
use crate::database::pg::productivity_knowledge::KnowledgeHit;

/// FTS search over `productivity_knowledge`. Used by the knowledge-browser
/// modal (Ctrl+Shift+E and the Productivity tab's Knowledge sub-view).
/// Vector search remains exposed via the PG layer for advanced callers
/// but is not surfaced through this Tauri command in v1.
///
/// `area_filter` is an exact-match on the row's `area`. `top_k` is
/// clamped server-side to a sane range (see `search_knowledge_fts`).
#[tauri::command]
pub async fn search_knowledge(
    app_handle: tauri::AppHandle,
    query: String,
    area_filter: Option<String>,
    top_k: i32,
) -> Result<Vec<KnowledgeHit>, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state
        .pg_db
        .search_knowledge_fts(&query, area_filter.as_deref(), top_k)
        .await
}
