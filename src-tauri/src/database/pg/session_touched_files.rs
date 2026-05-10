//! Session-touched files: durable per-session file-set tracker.
//!
//! Records every file path edited by a session/sub-agent over its lifetime so
//! commit-time logic (Commit Progress Phase C for workflows, Phase D for
//! Terminal sessions) can enumerate exactly the files this agent touched.
//!
//! This is intentionally separate from `FileRegistryManager` (executor/file_registry.rs):
//!   - FileRegistryManager holds transient `Active` lock entries that are
//!     released when the agent finishes — by commit time those entries are
//!     gone.
//!   - This module is append-only (UPSERT on the natural composite key
//!     `(task_run_id, file_path)`) and survives lock release.
//!
//! The dispatcher writes here fire-and-forget on every Edit/Write tool call
//! (see `claude_session::dispatcher::auto_register_file`), in parallel with the
//! existing `registry.register(...)` call.

use super::PgDb;
use serde::Serialize;

/// One row of the windowed "hot files" aggregate. Returned by
/// [`PgDb::hot_files`] for the file-activity heatmap.
#[derive(Debug, Clone, Serialize)]
pub struct HotFileRow {
    pub file_path: String,
    /// Distinct sessions that touched this file in the window. Counts
    /// distinct `task_run_id`, not raw UPSERT rows — re-edits by the
    /// same session don't inflate the count, which is the signal we
    /// actually want ("how contested is this file").
    pub distinct_sessions: i64,
    /// Most recent recorded_at within the window for this file.
    pub latest_recorded_at: chrono::DateTime<chrono::Utc>,
    /// task_run_id of the most-recent toucher. Useful for the "latest
    /// editor" column in the UI.
    pub latest_task_run_id: String,
}

/// One row of the windowed "hot sessions" aggregate. Returned by
/// [`PgDb::hot_sessions`].
#[derive(Debug, Clone, Serialize)]
pub struct HotSessionRow {
    pub task_run_id: String,
    /// Distinct files touched by this session in the window.
    pub distinct_files: i64,
    pub latest_recorded_at: chrono::DateTime<chrono::Utc>,
}

impl PgDb {
    /// Record that `file_path` was touched by `task_run_id`. Idempotent —
    /// repeated calls for the same `(task_run_id, file_path)` refresh
    /// `recorded_at` to NOW via UPSERT but do not create duplicate rows.
    ///
    /// `worktree_id` follows the same nullable-string convention as
    /// `RegistryKey` from Phase 1: pass `None` for the canonical/main repo,
    /// `Some(id)` to scope to a specific git worktree. The latest UPSERT wins
    /// on `worktree_id` if the same file is later touched in a different
    /// worktree (rare; Phase C will filter by worktree at commit time anyway).
    pub async fn record_file_touched(
        &self,
        task_run_id: &str,
        file_path: &str,
        worktree_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // INSERT ... ON CONFLICT updates recorded_at and worktree_id. Updating
        // worktree_id keeps the row coherent if a session promotes from main
        // into a worktree mid-flight (Phase 2 promote method); the latest
        // observed scope wins.
        conn.execute(
            r#"INSERT INTO coord.session_touched_files
                   (task_run_id, file_path, worktree_id, recorded_at)
               VALUES ($1, $2, $3, NOW())
               ON CONFLICT (task_run_id, file_path) DO UPDATE
                   SET recorded_at = NOW(),
                       worktree_id = EXCLUDED.worktree_id"#,
            &[&task_run_id, &file_path, &worktree_id],
        )
        .await
        .map_err(|e| format!("PG record_file_touched: {}", e))?;

        Ok(())
    }

    /// Return all distinct file paths touched by `task_run_id`, sorted
    /// oldest-first by `recorded_at`. Deterministic — second-touched-first
    /// files move to the bottom of the list because UPSERT refreshes
    /// `recorded_at`. This is the enumeration commit-time logic uses.
    pub async fn get_files_touched(&self, task_run_id: &str) -> Result<Vec<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT file_path
                   FROM coord.session_touched_files
                   WHERE task_run_id = $1
                   ORDER BY recorded_at ASC, file_path ASC"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_files_touched: {}", e))?;

        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    /// Reverse lookup of [`get_files_touched`]: given a list of file paths,
    /// return every `(file_path, task_run_id)` pair that recently touched any
    /// of those files. Ordered most-recent first by `recorded_at`.
    ///
    /// Used by Phase F's pre-merge guard (`worktree::merge_worktree`) to
    /// surface which sibling sessions own the dirty files in a destination
    /// checkout when a worktree merge would silently overwrite them. Returns
    /// one row per `(file, session)` pair — callers dedup if they want
    /// per-file or per-session views.
    pub async fn get_sessions_for_files(
        &self,
        file_paths: &[String],
    ) -> Result<Vec<(String, String)>, String> {
        if file_paths.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT file_path, task_run_id
                   FROM coord.session_touched_files
                   WHERE file_path = ANY($1)
                   ORDER BY recorded_at DESC, file_path ASC"#,
                &[&file_paths],
            )
            .await
            .map_err(|e| format!("PG get_sessions_for_files: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
            .collect())
    }

    /// Delete every recorded file row for `task_run_id`. Returns the number
    /// of rows removed. Phase C/D will call this after a successful commit
    /// to keep the table from growing unboundedly across long-lived runs.
    pub async fn clear_files_touched(&self, task_run_id: &str) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                "DELETE FROM coord.session_touched_files WHERE task_run_id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG clear_files_touched: {}", e))?;

        Ok(n)
    }

    /// Top files by distinct-toucher count in the last `window_secs`
    /// seconds. Ordered by `distinct_sessions DESC`, then most-recent
    /// `recorded_at` for stable display. Capped at `limit` rows.
    ///
    /// Uses the existing `idx_session_touched_files_recorded_at` index;
    /// `EXPLAIN` confirms a bitmap-index scan for windows ≤ 1 hour on
    /// dev table sizes.
    pub async fn hot_files(&self, window_secs: i64, limit: i64) -> Result<Vec<HotFileRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"WITH windowed AS (
                       SELECT file_path, task_run_id, recorded_at
                       FROM coord.session_touched_files
                       WHERE recorded_at >= NOW() - make_interval(secs => $1::double precision)
                   ),
                   per_file_latest AS (
                       SELECT DISTINCT ON (file_path)
                              file_path, task_run_id, recorded_at
                       FROM windowed
                       ORDER BY file_path, recorded_at DESC
                   )
                   SELECT w.file_path,
                          COUNT(DISTINCT w.task_run_id)::bigint AS distinct_sessions,
                          MAX(w.recorded_at) AS latest_recorded_at,
                          (SELECT pfl.task_run_id
                             FROM per_file_latest pfl
                            WHERE pfl.file_path = w.file_path) AS latest_task_run_id
                   FROM windowed w
                   GROUP BY w.file_path
                   ORDER BY distinct_sessions DESC, latest_recorded_at DESC
                   LIMIT $2"#,
                &[&(window_secs as f64), &limit],
            )
            .await
            .map_err(|e| format!("PG hot_files: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| HotFileRow {
                file_path: r.get(0),
                distinct_sessions: r.get(1),
                latest_recorded_at: r.get(2),
                latest_task_run_id: r.get(3),
            })
            .collect())
    }

    /// Top sessions by distinct-file count in the last `window_secs`
    /// seconds. Ordered by `distinct_files DESC`, then most-recent
    /// `recorded_at`. Capped at `limit` rows.
    pub async fn hot_sessions(
        &self,
        window_secs: i64,
        limit: i64,
    ) -> Result<Vec<HotSessionRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT task_run_id,
                          COUNT(DISTINCT file_path)::bigint AS distinct_files,
                          MAX(recorded_at) AS latest_recorded_at
                   FROM coord.session_touched_files
                   WHERE recorded_at >= NOW() - make_interval(secs => $1::double precision)
                   GROUP BY task_run_id
                   ORDER BY distinct_files DESC, latest_recorded_at DESC
                   LIMIT $2"#,
                &[&(window_secs as f64), &limit],
            )
            .await
            .map_err(|e| format!("PG hot_sessions: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| HotSessionRow {
                task_run_id: r.get(0),
                distinct_files: r.get(1),
                latest_recorded_at: r.get(2),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a unique task_run_id per test so concurrent test runs don't
    /// collide on the same PG instance. Uses nanos-since-epoch + a thread
    /// id — collision-free for any realistic test cadence.
    fn unique_task_run_id(label: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!(
            "test-stf-{}-{}-{:?}",
            label,
            nanos,
            std::thread::current().id()
        )
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn record_then_get_returns_file() {
        let db = PgDb::new_blocking_for_test();
        let task_run_id = unique_task_run_id("basic");
        let _ = db.clear_files_touched(&task_run_id).await;

        db.record_file_touched(&task_run_id, "/repo/src/foo.rs", None)
            .await
            .expect("record_file_touched");

        let files = db
            .get_files_touched(&task_run_id)
            .await
            .expect("get_files_touched");
        assert_eq!(files, vec!["/repo/src/foo.rs".to_string()]);

        // Cleanup so reruns are deterministic.
        let _ = db.clear_files_touched(&task_run_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn record_same_file_twice_is_one_row() {
        let db = PgDb::new_blocking_for_test();
        let task_run_id = unique_task_run_id("upsert");
        let _ = db.clear_files_touched(&task_run_id).await;

        db.record_file_touched(&task_run_id, "/repo/src/bar.rs", None)
            .await
            .expect("first insert");
        db.record_file_touched(&task_run_id, "/repo/src/bar.rs", None)
            .await
            .expect("second insert (UPSERT)");

        let files = db
            .get_files_touched(&task_run_id)
            .await
            .expect("get_files_touched");
        assert_eq!(
            files.len(),
            1,
            "UPSERT must not create duplicate rows; got {:?}",
            files
        );
        assert_eq!(files[0], "/repo/src/bar.rs");

        let _ = db.clear_files_touched(&task_run_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn get_files_touched_sorted_oldest_first() {
        let db = PgDb::new_blocking_for_test();
        let task_run_id = unique_task_run_id("sort");
        let _ = db.clear_files_touched(&task_run_id).await;

        db.record_file_touched(&task_run_id, "/repo/a.rs", None)
            .await
            .unwrap();
        // PG NOW() resolution is microseconds; sleep ~5ms to guarantee
        // a strictly-greater recorded_at on the second insert. Without
        // this, two inserts inside the same statement_timestamp() can
        // tie and break the ordering assertion.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        db.record_file_touched(&task_run_id, "/repo/b.rs", None)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        db.record_file_touched(&task_run_id, "/repo/c.rs", None)
            .await
            .unwrap();

        let files = db.get_files_touched(&task_run_id).await.unwrap();
        assert_eq!(
            files,
            vec![
                "/repo/a.rs".to_string(),
                "/repo/b.rs".to_string(),
                "/repo/c.rs".to_string(),
            ],
            "files must be sorted oldest-first by recorded_at"
        );

        let _ = db.clear_files_touched(&task_run_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn upsert_moves_file_to_bottom_of_sort() {
        let db = PgDb::new_blocking_for_test();
        let task_run_id = unique_task_run_id("upsert-sort");
        let _ = db.clear_files_touched(&task_run_id).await;

        db.record_file_touched(&task_run_id, "/repo/a.rs", None)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        db.record_file_touched(&task_run_id, "/repo/b.rs", None)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        // Re-touch a.rs — recorded_at refreshes to NOW.
        db.record_file_touched(&task_run_id, "/repo/a.rs", None)
            .await
            .unwrap();

        let files = db.get_files_touched(&task_run_id).await.unwrap();
        assert_eq!(
            files,
            vec!["/repo/b.rs".to_string(), "/repo/a.rs".to_string()],
            "re-touched file must move to the bottom of the sort"
        );

        let _ = db.clear_files_touched(&task_run_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn clear_removes_only_target_task_run() {
        let db = PgDb::new_blocking_for_test();
        let target = unique_task_run_id("clear-target");
        let other = unique_task_run_id("clear-other");
        let _ = db.clear_files_touched(&target).await;
        let _ = db.clear_files_touched(&other).await;

        db.record_file_touched(&target, "/repo/x.rs", None)
            .await
            .unwrap();
        db.record_file_touched(&target, "/repo/y.rs", None)
            .await
            .unwrap();
        db.record_file_touched(&other, "/repo/z.rs", None)
            .await
            .unwrap();

        let n = db.clear_files_touched(&target).await.unwrap();
        assert_eq!(n, 2, "must delete exactly the 2 rows for target task_run");

        let target_files = db.get_files_touched(&target).await.unwrap();
        assert!(
            target_files.is_empty(),
            "target rows must be gone after clear"
        );

        let other_files = db.get_files_touched(&other).await.unwrap();
        assert_eq!(
            other_files,
            vec!["/repo/z.rs".to_string()],
            "other task_run rows must be untouched"
        );

        let _ = db.clear_files_touched(&other).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn worktree_id_preserved_null_and_some() {
        let db = PgDb::new_blocking_for_test();
        let task_run_id = unique_task_run_id("worktree");
        let _ = db.clear_files_touched(&task_run_id).await;

        db.record_file_touched(&task_run_id, "/repo/main.rs", None)
            .await
            .expect("None worktree");
        db.record_file_touched(&task_run_id, "/repo/wt.rs", Some("wt-abc"))
            .await
            .expect("Some worktree");

        // Read raw rows to verify worktree_id round-trips correctly.
        let conn = db.pool().get().await.expect("pool");
        let rows = conn
            .query(
                "SELECT file_path, worktree_id FROM coord.session_touched_files \
                 WHERE task_run_id = $1 ORDER BY file_path ASC",
                &[&task_run_id],
            )
            .await
            .expect("query");

        assert_eq!(rows.len(), 2, "expected 2 rows for this task_run");
        let row0_path: String = rows[0].get(0);
        let row0_wt: Option<String> = rows[0].get(1);
        let row1_path: String = rows[1].get(0);
        let row1_wt: Option<String> = rows[1].get(1);

        assert_eq!(row0_path, "/repo/main.rs");
        assert_eq!(row0_wt, None, "None worktree_id must round-trip as NULL");
        assert_eq!(row1_path, "/repo/wt.rs");
        assert_eq!(
            row1_wt,
            Some("wt-abc".to_string()),
            "Some(...) worktree_id must round-trip"
        );

        let _ = db.clear_files_touched(&task_run_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn upsert_updates_worktree_id_on_re_touch() {
        let db = PgDb::new_blocking_for_test();
        let task_run_id = unique_task_run_id("worktree-upsert");
        let _ = db.clear_files_touched(&task_run_id).await;

        // First touch in main repo (None), then re-touch from worktree.
        db.record_file_touched(&task_run_id, "/repo/promoted.rs", None)
            .await
            .unwrap();
        db.record_file_touched(&task_run_id, "/repo/promoted.rs", Some("wt-promote"))
            .await
            .unwrap();

        let conn = db.pool().get().await.unwrap();
        let row = conn
            .query_one(
                "SELECT worktree_id FROM coord.session_touched_files \
                 WHERE task_run_id = $1 AND file_path = $2",
                &[&task_run_id, &"/repo/promoted.rs"],
            )
            .await
            .unwrap();
        let wt: Option<String> = row.get(0);
        assert_eq!(
            wt,
            Some("wt-promote".to_string()),
            "EXCLUDED.worktree_id from second touch must overwrite the NULL from first"
        );

        let _ = db.clear_files_touched(&task_run_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn get_sessions_for_files_returns_owners_most_recent_first() {
        let db = PgDb::new_blocking_for_test();
        let session_a = unique_task_run_id("rev-a");
        let session_b = unique_task_run_id("rev-b");
        let session_c = unique_task_run_id("rev-c");
        let _ = db.clear_files_touched(&session_a).await;
        let _ = db.clear_files_touched(&session_b).await;
        let _ = db.clear_files_touched(&session_c).await;

        // Use unique paths per test run so concurrent runs don't collide on
        // shared rows from other sessions in the same PG instance.
        let file1 = format!("/repo/rev-{}-1.rs", session_a);
        let file2 = format!("/repo/rev-{}-2.rs", session_a);
        let other = format!("/repo/rev-{}-other.rs", session_a);

        // Session A touches file1 first, then file2. Session B touches file1
        // later (so B is the most-recent owner of file1). Session C touches
        // an unrelated file we won't query for.
        db.record_file_touched(&session_a, &file1, None)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        db.record_file_touched(&session_a, &file2, None)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        db.record_file_touched(&session_b, &file1, None)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        db.record_file_touched(&session_c, &other, None)
            .await
            .unwrap();

        let pairs = db
            .get_sessions_for_files(&[file1.clone(), file2.clone()])
            .await
            .expect("get_sessions_for_files");

        // Expected: 3 pairs total (A+file1, A+file2, B+file1). Session C
        // touched a path we didn't query for, so it must NOT appear.
        assert_eq!(pairs.len(), 3, "got {:?}", pairs);
        assert!(
            !pairs.iter().any(|(_, s)| s == &session_c),
            "session_c touched an unrelated file and must not appear: {:?}",
            pairs
        );

        // Most-recent-first ordering: B's touch of file1 was last, so it
        // must be the first row.
        assert_eq!(
            pairs[0],
            (file1.clone(), session_b.clone()),
            "most-recent owner of file1 must be session_b: {:?}",
            pairs
        );

        // Empty input must short-circuit to empty output without erroring.
        let empty = db.get_sessions_for_files(&[]).await.expect("empty input");
        assert!(empty.is_empty());

        let _ = db.clear_files_touched(&session_a).await;
        let _ = db.clear_files_touched(&session_b).await;
        let _ = db.clear_files_touched(&session_c).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn get_files_touched_empty_for_unknown_task_run() {
        let db = PgDb::new_blocking_for_test();
        let task_run_id = unique_task_run_id("empty");
        // No inserts — must come back empty (not error).
        let files = db.get_files_touched(&task_run_id).await.unwrap();
        assert!(files.is_empty());

        let n = db.clear_files_touched(&task_run_id).await.unwrap();
        assert_eq!(n, 0, "clear of unknown task_run returns 0 rows deleted");
    }
}
