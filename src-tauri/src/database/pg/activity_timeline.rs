//! PostgreSQL activity timeline operations via Clorinde-generated queries.
//!
//! Screenpipe-inspired searchable capture history: stores extracted text from
//! UI Bridge snapshots, OCR, and accessibility trees with full-text search
//! and content-hash deduplication.

use sha2::{Digest, Sha256};

use super::PgDb;
use crate::database::types::*;

/// Compute normalized SHA-256 hash for consecutive-frame deduplication.
///
/// Hashes `(text_content, metadata_json)` together so events that share the
/// same `text_content` but carry distinct metadata are NOT deduped.
/// Screenshot-style callers (BackgroundObserver, Python bridge) pass
/// `metadata_json: None` and so retain their original dedup behavior — two
/// identical text captures within 30 s still collapse into one row.
/// Event-stream callers (e.g. `scripted_output.llm_ok` from
/// [`crate::step_output::script_emitter::spawn_activity_event`]) carry the
/// real signal — token counts, cache stats — in `metadata_json`; including
/// it here means three back-to-back emit events with different cache_read
/// totals are stored as three distinct rows instead of being silently
/// coalesced into one with an incremented `duplicate_count`.
///
/// `text_content` is normalized (trim + lowercase) to match the original
/// screenshot dedup intent. `metadata_json` is hashed verbatim — JSON is
/// case-sensitive and a re-emit produced by the same code path will be
/// byte-identical (serde_json::to_string is deterministic for stable maps).
fn content_hash(text: &str, metadata_json: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.trim().to_lowercase().as_bytes());
    // Separator that can't appear in normalized text or in the metadata
    // body — any 0x00 byte is invalid in JSON and the lowercase
    // text_content is unicode-printable.
    hasher.update(b"\0meta:");
    hasher.update(metadata_json.unwrap_or("").as_bytes());
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

        let hash = content_hash(&input.text_content, input.metadata_json.as_deref());

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
                latest_capture: r
                    .latest_capture
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
            })
            .collect())
    }

    /// Lightweight FK pre-check used by the scripted-output telemetry path.
    ///
    /// `activity_timeline.task_run_id` has an FK constraint to `task_runs(id)`,
    /// so an insert that names a non-existent task_run is rejected outright
    /// and the event is lost. The emitter calls this before inserting to
    /// null out the FK column rather than drop the event when the caller's
    /// id doesn't resolve (e.g. ad-hoc direct invocations of
    /// `emit_extraction_script` outside a real workflow run).
    pub async fn task_run_exists(&self, task_run_id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                "SELECT 1 FROM task_runs WHERE id = $1 LIMIT 1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG task_run_exists: {}", e))?;
        Ok(row.is_some())
    }

    /// Return every `scripted_output` activity-timeline row's event name
    /// (`text_content`), `metadata_json`, and `duplicate_count`, optionally
    /// scoped to a single `task_run_id`. Returns tuples
    /// `(text_content, metadata_json, occurrence_count)` where
    /// `occurrence_count = 1 + duplicate_count` represents how many times the
    /// row's event was emitted (one original + N suppressed dupes).
    ///
    /// `occurrence_count` is critical because [`insert_activity_entry`]
    /// content-hash-dedups entries within a 30-s window: two identical
    /// `(text, metadata)` events arrive as one row with `duplicate_count = 1`,
    /// not as two separate rows. The aggregator must multiply per-row signals
    /// by `occurrence_count` to recover accurate counts; otherwise rapid
    /// emit calls with identical metadata (e.g. several
    /// `scripted_output.llm_ok` events from the warm provider's cache-read
    /// path) silently under-report.
    ///
    /// Used by the LLM-analytics scripted-output widget to aggregate per-run
    /// counters. Single indexed scan on `source_type`; the caller aggregates
    /// in memory.
    pub async fn get_scripted_output_rows(
        &self,
        task_run_id: Option<&str>,
    ) -> Result<Vec<(String, Option<String>, u64)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if let Some(tr) = task_run_id {
            conn.query(
                "SELECT text_content, metadata_json, duplicate_count \
                 FROM activity_timeline \
                 WHERE source_type = 'scripted_output' \
                   AND NOT is_deleted \
                   AND task_run_id = $1",
                &[&tr],
            )
            .await
            .map_err(|e| format!("PG get_scripted_output_rows(task_run): {}", e))?
        } else {
            conn.query(
                "SELECT text_content, metadata_json, duplicate_count \
                 FROM activity_timeline \
                 WHERE source_type = 'scripted_output' \
                   AND NOT is_deleted",
                &[],
            )
            .await
            .map_err(|e| format!("PG get_scripted_output_rows(all): {}", e))?
        };

        Ok(rows
            .into_iter()
            .map(|row| {
                let text: String = row.get(0);
                let meta: Option<String> = row.get(1);
                let dup: i32 = row.get(2);
                // occurrence_count = 1 (the original) + N suppressed dupes.
                // Saturate at i32::MAX cast — a row representing >2B emits
                // is implausible but we don't want to panic on overflow.
                let occurrences = 1u64.saturating_add(dup.max(0) as u64);
                (text, meta, occurrences)
            })
            .collect())
    }

    /// Permanently delete soft-deleted entries older than retention_days.
    pub async fn cleanup_old_timeline_entries(&self, retention_days: i32) -> Result<u64, String> {
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
        let h1 = content_hash("Hello world", None);
        let h2 = content_hash("Hello world", None);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_content_hash_normalized() {
        let h1 = content_hash("  Hello World  ", None);
        let h2 = content_hash("hello world", None);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_content_hash_different() {
        let h1 = content_hash("Hello", None);
        let h2 = content_hash("World", None);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_content_hash_metadata_distinguishes_same_text() {
        // The bug this fixes: three rapid `scripted_output.llm_ok` events
        // share text_content but carry distinct cache_read_tokens in
        // metadata_json. Pre-fix they collided on the 30-s dedup and
        // calls 2..N were silently coalesced; post-fix they produce
        // distinct hashes.
        let text = "scripted_output.llm_ok";
        let h_creation = content_hash(
            text,
            Some(r#"{"cache_creation_tokens":6697,"cache_read_tokens":0}"#),
        );
        let h_read = content_hash(
            text,
            Some(r#"{"cache_creation_tokens":0,"cache_read_tokens":6697}"#),
        );
        assert_ne!(
            h_creation, h_read,
            "events with same text but different metadata must NOT dedup"
        );
    }

    #[test]
    fn test_content_hash_identical_metadata_still_dedups() {
        // Truly identical re-emits (same text, same metadata) still
        // collapse — preserves the original 30-s screenshot-dedup intent.
        let h1 = content_hash("scripted_output.cache_hit", Some(r#"{"tier":1}"#));
        let h2 = content_hash("scripted_output.cache_hit", Some(r#"{"tier":1}"#));
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_content_hash_none_metadata_preserves_screenshot_dedup() {
        // BackgroundObserver and the Python bridge both pass
        // metadata_json: None — two identical text captures within 30 s
        // must still produce the same hash so they dedup.
        let h1 = content_hash("captured screen text", None);
        let h2 = content_hash("captured screen text", None);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_content_hash_none_vs_empty_metadata_are_equivalent() {
        // None and Some("") both represent "no metadata"; they must hash
        // identically so a caller migrating from one to the other
        // doesn't accidentally suppress dedup.
        let h_none = content_hash("event", None);
        let h_empty = content_hash("event", Some(""));
        assert_eq!(h_none, h_empty);
    }

    #[test]
    fn test_content_hash_separator_prevents_field_smushing() {
        // Without a delimiter, ("ab", None) and ("a", Some("b"))
        // could collide. The "\0meta:" separator prevents that.
        let h_smushed = content_hash("ab", None);
        let h_split = content_hash("a", Some("b"));
        assert_ne!(h_smushed, h_split);
    }
}
