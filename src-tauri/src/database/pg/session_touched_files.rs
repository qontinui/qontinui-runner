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
            r#"INSERT INTO session_touched_files
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
                   FROM session_touched_files
                   WHERE task_run_id = $1
                   ORDER BY recorded_at ASC, file_path ASC"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_files_touched: {}", e))?;

        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
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
                "DELETE FROM session_touched_files WHERE task_run_id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG clear_files_touched: {}", e))?;

        Ok(n)
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
                "SELECT file_path, worktree_id FROM session_touched_files \
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
                "SELECT worktree_id FROM session_touched_files \
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
