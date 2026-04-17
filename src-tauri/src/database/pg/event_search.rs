//! PostgreSQL unified event search operations via Clorinde-generated queries.
//!
//! Full-text search across activity_timeline, observations, deferred_questions,
//! and error_events with a source_table discriminator and ts_rank_cd relevance
//! scoring. See queries/event_search.sql for the underlying UNION query.

use super::PgDb;
use qontinui_db::queries::event_search::SearchEventsRow;

impl PgDb {
    /// Unified full-text search across activity_timeline, observations,
    /// deferred_questions, and error_events. Returns raw Clorinde rows; the
    /// command layer is responsible for mapping to the presentation struct.
    pub async fn search_events(
        &self,
        query: &str,
        since_ts: chrono::DateTime<chrono::FixedOffset>,
        max_results: i64,
    ) -> Result<Vec<SearchEventsRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::event_search::search_events()
            .bind(&conn, &query, &since_ts, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG search_events: {}", e))?;

        Ok(rows)
    }
}
