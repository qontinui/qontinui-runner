//! Unified search result type.
//!
//! The actual search implementation lives on `PgDb::unified_search` in
//! `database/pg/graph_ops.rs`. This module only retains the shared result
//! type consumed by the MCP graph API.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct UnifiedSearchResult {
    pub entity_type: String,
    pub entity_id: String,
    pub title: String,
    pub snippet: String,
    pub relevance_score: f64,
    pub source_table: String,
}
