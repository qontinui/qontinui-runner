//! PostgreSQL activity timeline operations via Clorinde-generated queries.
//!
//! Screenpipe-inspired searchable capture history: stores extracted text from
//! UI Bridge snapshots, OCR, and accessibility trees with full-text search
//! and content-hash deduplication.

use sha2::{Digest, Sha256};

use super::PgDb;
use crate::database::types::*;

/// Compute normalized SHA-256 hash for consecutive-frame deduplication.
fn content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.trim().to_lowercase().as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Map a Clorinde search/range row to our app-level type.
/// Works for SearchTimeline, SearchTimelineFiltered, GetTimelineRange, and GetTimelineByTaskRun
/// since they share the same shape (preview rows).
macro_rules! map_preview_row {
    ($r:expr, $rank:expr) => {
        ActivityTimelineSearchResult {
            id: $r.id,
            text_preview: $r.text_preview,
            source_type: $r.source_type,
            capture_mode: $r.capture_mode,
            app_name: $r.app_name,
            window_title: $r.window_title,
            url: $r.url,
            screenshot_path: $r.screenshot_path,
            element_count: $r.element_count,
            confidence: $r.confidence,
            created_at: $r.created_at.to_rfc3339(),
            rank: $rank,
        }
    };
}

impl PgDb {
    /// Insert a new activity timeline entry with content-hash deduplication.
    ///
    /// If an entry with the same content_hash was inserted within the last 30 seconds,
    /// its duplicate_count is incremented and the existing ID is returned.
    pub async fn insert_activity_entry(
        &self,
        input: &ActivityTimelineInput,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let hash = content_hash(&input.text_content);

        // Check for recent duplicate (30-second window)
        let dup = qontinui_db::queries::activity_timeline::find_recent_duplicate()
            .bind(&conn, &hash.as_str())
            .opt()
            .await
            .map_err(|e| format!("PG find_recent_duplicate: {}", e))?;

        if let Some(existing) = dup {
            qontinui_db::queries::activity_timeline::increment_duplicate_count()
                .bind(&conn, &existing.id)
                .await
                .map_err(|e| format!("PG increment_duplicate_count: {}", e))?;
            return Ok(existing.id);
        }

        // Insert new entry
        let id = qontinui_db::queries::activity_timeline::insert_activity_entry()
            .bind(
                &conn,
                &input.text_content.as_str(),
                &hash.as_str(),
                &input.source_type.as_str(),
                &input.capture_mode.as_str(),
                &input.app_name,
                &input.window_title,
                &input.url,
                &input.task_run_id,
                &input.screenshot_path,
                &input.element_count,
                &input.confidence,
                &input.metadata_json,
            )
            .one()
            .await
            .map_err(|e| format!("PG insert_activity_entry: {}", e))?;

        Ok(id)
    }

    /// Full-text search over the activity timeline. Returns 500-char previews with relevance ranking.
    pub async fn search_timeline(
        &self,
        query: &str,
        max_results: i64,
    ) -> Result<Vec<ActivityTimelineSearchResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::activity_timeline::search_timeline()
            .bind(&conn, &query, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG search_timeline: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| map_preview_row!(r, Some(r.rank)))
            .collect())
    }

    /// Full-text search with optional app_name, source_type, and task_run_id filters.
    pub async fn search_timeline_filtered(
        &self,
        query: &str,
        app_name: Option<&str>,
        source_type: Option<&str>,
        task_run_id: Option<&str>,
        max_results: i64,
    ) -> Result<Vec<ActivityTimelineSearchResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let app = app_name.map(String::from);
        let src = source_type.map(String::from);
        let run = task_run_id.map(String::from);

        let rows = qontinui_db::queries::activity_timeline::search_timeline_filtered()
            .bind(&conn, &query, &app, &src, &run, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG search_timeline_filtered: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| map_preview_row!(r, Some(r.rank)))
            .collect())
    }

    /// Get timeline entries within a time range.
    pub async fn get_timeline_range(
        &self,
        start_time: chrono::DateTime<chrono::FixedOffset>,
        end_time: chrono::DateTime<chrono::FixedOffset>,
        max_results: i64,
    ) -> Result<Vec<ActivityTimelineSearchResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::activity_timeline::get_timeline_range()
            .bind(&conn, &start_time, &end_time, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_timeline_range: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| map_preview_row!(r, None))
            .collect())
    }

    /// Get a single timeline entry by ID (full content).
    pub async fn get_timeline_entry(
        &self,
        id: i64,
    ) -> Result<Option<ActivityTimelineEntry>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = qontinui_db::queries::activity_timeline::get_timeline_entry()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_timeline_entry: {}", e))?;

        Ok(row.map(|r| ActivityTimelineEntry {
            id: r.id,
            text_content: r.text_content,
            content_hash: r.content_hash,
            source_type: r.source_type,
            capture_mode: r.capture_mode,
            app_name: r.app_name,
            window_title: r.window_title,
            url: r.url,
            task_run_id: r.task_run_id,
            screenshot_path: r.screenshot_path,
            element_count: r.element_count,
            confidence: r.confidence,
            metadata_json: r.metadata_json,
            duplicate_count: r.duplicate_count,
            created_at: r.created_at.to_rfc3339(),
        }))
    }

    /// Get all timeline entries for a specific task run, ordered chronologically.
    pub async fn get_timeline_by_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<ActivityTimelineSearchResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::activity_timeline::get_timeline_by_task_run()
            .bind(&conn, &task_run_id)
            .all()
            .await
            .map_err(|e| format!("PG get_timeline_by_task_run: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| map_preview_row!(r, None))
            .collect())
    }

    /// Soft-delete a timeline entry.
    pub async fn delete_timeline_entry(&self, id: i64) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let deleted = qontinui_db::queries::activity_timeline::soft_delete_timeline_entry()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG soft_delete_timeline_entry: {}", e))?;

        Ok(deleted.is_some())
    }

    /// Get capture statistics grouped by source_type and capture_mode.
    pub async fn get_timeline_stats(&self) -> Result<Vec<ActivityTimelineStat>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::activity_timeline::get_timeline_stats()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG get_timeline_stats: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| ActivityTimelineStat {
                source_type: r.source_type,
                capture_mode: r.capture_mode,
                count: r.count,
                latest_capture: r.latest_capture.map(|dt| dt.to_rfc3339()).unwrap_or_default(),
            })
            .collect())
    }

    /// Permanently delete soft-deleted entries older than retention_days.
    pub async fn cleanup_old_timeline_entries(
        &self,
        retention_days: i32,
    ) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let deleted = qontinui_db::queries::activity_timeline::cleanup_old_entries()
            .bind(&conn, &retention_days)
            .await
            .map_err(|e| format!("PG cleanup_old_entries: {}", e))?;

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = content_hash("Hello world");
        let h2 = content_hash("Hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_content_hash_normalized() {
        let h1 = content_hash("  Hello World  ");
        let h2 = content_hash("hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_content_hash_different() {
        let h1 = content_hash("Hello");
        let h2 = content_hash("World");
        assert_ne!(h1, h2);
    }
}
