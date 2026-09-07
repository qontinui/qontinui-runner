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
//!
//! ## Schema authority — the runner authors this table
//!
//! Re-homed from `coord.*` to `project.*` by P3 of plan
//! `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration`. The
//! `coord.*` schema is authored SOLELY by qontinui-web's alembic, which
//! never runs on an end-user machine — and on such a machine the runner's
//! bundled per-machine PostgreSQL (`postgresql_embedded`) IS the production
//! database. So the old `coord.`-qualified SQL here either errored against a
//! table that was never provisioned or wrote to a private table no fleet
//! member could read. This table is machine-local operational state the
//! runner reads back itself, so the runner is now its author: the shape is
//! defined by the `CREATE TABLE IF NOT EXISTS` self-heal in
//! `database/pg/mod.rs` (`MACHINE_LOCAL_TABLES_DDL`), not by any alembic
//! revision.

use super::PgDb;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Margin added on top of the widest live reader's own horizon before a row
/// becomes eligible for deletion. Deleting a row a reader can still see is a
/// data-loss bug; keeping one a month too long costs a few thousand rows on a
/// table whose entire census on a heavily-used box is four figures. So the
/// margin errs long deliberately.
const RETENTION_MARGIN_DAYS: u32 = 30;

/// Default retention for `project.session_touched_files`, in days. Override
/// with env var `QONTINUI_SESSION_TOUCHED_FILES_RETENTION_DAYS`.
///
/// ## Which reader set this number
///
/// The window is sized against the WIDEST live reader, not the most obvious
/// one. Every consumer of this table and the horizon it needs:
///
/// | Reader | Horizon |
/// |---|---|
/// | `coordinator::deconflicter` (`RECENT_TOUCH_WINDOW_MINUTES`) | 15 minutes |
/// | `commands::ai_session::recent_session_touched_files` (the heatmap panel) | caller-supplied `window_secs`; the UI default is 30 s and its widest fixed option is 24 h |
/// | [`PgDb::hot_files`] / [`PgDb::hot_sessions`] via `GET /file-activity/heatmap` | caller-supplied, **clamped to 86 400 s** (24 h) by the handler |
/// | `coordinator::observe` | 3600 s for `hot_sessions`; unbounded for [`PgDb::get_files_touched`], but scoped to sessions that are live right now |
/// | Commit-time enumeration ([`PgDb::get_files_touched`] from `mcp::ai_session::commit_session_progress`, `unified_workflow_executor::task_lifecycle::auto_commit_on_success`, `mcp::sessions`, `productivity::review`) | unbounded query, but bounded in practice by one session's lifetime — and each of those call sites calls [`PgDb::clear_files_touched`] on success |
/// | [`PgDb::get_sessions_for_files`] (the worktree-merge and file-registry guards) | unbounded query over currently-dirty files |
/// | **`projects::snapshot::fetch_touched_rows`** (the saved-projects dashboard) | **`SESSION_WINDOW_DAYS` = 90 days** |
///
/// The binding constraint is therefore the project-snapshot scan, not the
/// commit-time enumeration: it is the only reader that deliberately reaches
/// back months, to answer "which saved projects has this machine actually
/// worked in". It filters `recorded_at >= NOW() - 90 days` itself, so a
/// retention window at or above 90 days is invisible to it — and the constant
/// is derived from that reader's own constant rather than re-typed, so the two
/// cannot silently drift apart.
const DEFAULT_RETENTION_DAYS: u32 =
    crate::projects::snapshot::SESSION_WINDOW_DAYS as u32 + RETENTION_MARGIN_DAYS;

/// Default interval between sweeps: once per day. Matches
/// `process_capture::cleanup`, and a table this size needs nothing tighter.
const DEFAULT_INTERVAL_SECS: u64 = 86_400;

fn get_retention_days() -> u32 {
    std::env::var("QONTINUI_SESSION_TOUCHED_FILES_RETENTION_DAYS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

/// Background loop: delete `project.session_touched_files` rows older than the
/// retention period.
///
/// The table is append-only by design — the dispatcher UPSERTs on every
/// Edit/Write so commit-time logic can enumerate a whole session's file set —
/// and [`PgDb::clear_files_touched`] only fires on the paths that reach a
/// successful commit. Every session that dies, is abandoned, or commits by
/// hand leaves its rows behind forever, so without this loop the table only
/// grows. Its sibling `project.process_sessions` has had a retention loop
/// since it shipped; this is the same shape for the same reason.
///
/// Fire-and-forget: an error is warned and the loop keeps its cadence rather
/// than aborting, so a transient PG hiccup does not silently disable retention
/// for the rest of the runner's life.
pub async fn run_session_touched_files_cleanup_loop(pg_db: Arc<PgDb>) {
    let interval_secs = DEFAULT_INTERVAL_SECS;
    let retention_days = get_retention_days();

    info!(
        "session_touched_files_cleanup_loop_started: interval_secs={}, retention_days={}",
        interval_secs, retention_days
    );

    // Run shortly after startup, then periodically. The delay keeps the sweep
    // off the critical path while the runner is still wiring up its pools.
    tokio::time::sleep(Duration::from_secs(30)).await;

    loop {
        match pg_db
            .cleanup_old_session_touched_files(retention_days)
            .await
        {
            Ok(deleted) if deleted > 0 => {
                info!(
                    "session_touched_files_cleanup: deleted {} rows older than {} days",
                    deleted, retention_days
                );
            }
            Ok(_) => {}
            Err(e) => {
                warn!("session_touched_files_cleanup failed: {}", e);
            }
        }

        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
    }
}

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
            r#"INSERT INTO project.session_touched_files
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
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_files_touched(&self, task_run_id: &str) -> Result<Vec<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT file_path
                   FROM project.session_touched_files
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
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
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
                   FROM project.session_touched_files
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
                "DELETE FROM project.session_touched_files WHERE task_run_id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG clear_files_touched: {}", e))?;

        Ok(n)
    }

    /// Delete every row whose `recorded_at` is older than `retention_days`.
    /// Returns the number of rows removed.
    ///
    /// This is the age-based backstop for the per-session
    /// [`clear_files_touched`](PgDb::clear_files_touched): that one only fires
    /// on a successful commit, so rows from abandoned, crashed or
    /// hand-committed sessions accumulate without bound. Driven by
    /// [`run_session_touched_files_cleanup_loop`]; see
    /// [`DEFAULT_RETENTION_DAYS`] for which reader sizes the window.
    ///
    /// Uses `idx_session_touched_files_recorded_at`.
    pub async fn cleanup_old_session_touched_files(
        &self,
        retention_days: u32,
    ) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // `make_interval(days => ...)` rather than the `($1 || ' days')::interval`
        // string concatenation used by `cleanup_old_process_sessions`: it binds
        // an integer instead of round-tripping through text, and it is the
        // spelling every other windowed query in this module already uses.
        let days = i32::try_from(retention_days).unwrap_or(i32::MAX);
        let n = conn
            .execute(
                "DELETE FROM project.session_touched_files \
                 WHERE recorded_at < NOW() - make_interval(days => $1::int)",
                &[&days],
            )
            .await
            .map_err(|e| format!("PG cleanup_old_session_touched_files: {}", e))?;

        Ok(n)
    }

    /// Top files by distinct-toucher count in the last `window_secs`
    /// seconds. Ordered by `distinct_sessions DESC`, then most-recent
    /// `recorded_at` for stable display. Capped at `limit` rows.
    ///
    /// Uses the existing `idx_session_touched_files_recorded_at` index;
    /// `EXPLAIN` confirms a bitmap-index scan for windows ≤ 1 hour on
    /// dev table sizes.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
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
                       FROM project.session_touched_files
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
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
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
                   FROM project.session_touched_files
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
                "SELECT file_path, worktree_id FROM project.session_touched_files \
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
                "SELECT worktree_id FROM project.session_touched_files \
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

    /// The retention default must never fall below the widest live reader's
    /// own horizon — `projects::snapshot`'s 90-day project-attribution scan.
    /// A change to either constant that broke that ordering would silently
    /// delete rows the saved-projects dashboard still queries, which is why
    /// this asserts the relationship rather than a literal.
    #[test]
    fn default_retention_covers_the_widest_reader() {
        let widest_reader_days = crate::projects::snapshot::SESSION_WINDOW_DAYS as u32;
        assert!(
            DEFAULT_RETENTION_DAYS >= widest_reader_days,
            "retention ({} days) must be >= the project-snapshot scan window ({} days)",
            DEFAULT_RETENTION_DAYS,
            widest_reader_days
        );
        assert_eq!(
            DEFAULT_RETENTION_DAYS,
            widest_reader_days + RETENTION_MARGIN_DAYS
        );
    }

    #[test]
    fn retention_env_override_parses_and_falls_back() {
        // Absent → the default. Set to a number → that number. Garbage → the
        // default rather than a panic or a 0-day window that would empty the
        // table on the next sweep.
        //
        // The variable is read only by `get_retention_days` in this module,
        // so no concurrently-running test observes the mutation.
        const VAR: &str = "QONTINUI_SESSION_TOUCHED_FILES_RETENTION_DAYS";

        std::env::remove_var(VAR);
        assert_eq!(get_retention_days(), DEFAULT_RETENTION_DAYS);

        std::env::set_var(VAR, "5");
        assert_eq!(get_retention_days(), 5);

        std::env::set_var(VAR, "not-a-number");
        assert_eq!(
            get_retention_days(),
            DEFAULT_RETENTION_DAYS,
            "an unparseable override must fall back to the default"
        );

        std::env::remove_var(VAR);
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn cleanup_deletes_only_rows_past_the_retention_window() {
        let db = PgDb::new_blocking_for_test();
        let task_run_id = unique_task_run_id("retention");
        let _ = db.clear_files_touched(&task_run_id).await;

        db.record_file_touched(&task_run_id, "/repo/ancient.rs", None)
            .await
            .expect("ancient row");
        db.record_file_touched(&task_run_id, "/repo/fresh.rs", None)
            .await
            .expect("fresh row");

        // Backdate one row well past the window. `record_file_touched` always
        // stamps NOW(), so this is the only way to age a row in a test.
        let conn = db.pool().get().await.expect("pool");
        conn.execute(
            "UPDATE project.session_touched_files \
             SET recorded_at = NOW() - make_interval(days => 200) \
             WHERE task_run_id = $1 AND file_path = $2",
            &[&task_run_id, &"/repo/ancient.rs"],
        )
        .await
        .expect("backdate");

        // 120 days: older than the fresh row, younger than the backdated one.
        db.cleanup_old_session_touched_files(120)
            .await
            .expect("cleanup");

        // Assert on this task_run's own rows, not on the returned count —
        // other sessions sharing this PG instance may have aged rows too.
        let files = db.get_files_touched(&task_run_id).await.expect("read back");
        assert_eq!(
            files,
            vec!["/repo/fresh.rs".to_string()],
            "the 200-day-old row must be gone and the fresh one kept; got {:?}",
            files
        );

        let _ = db.clear_files_touched(&task_run_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn cleanup_keeps_everything_inside_a_wide_window() {
        let db = PgDb::new_blocking_for_test();
        let task_run_id = unique_task_run_id("retention-wide");
        let _ = db.clear_files_touched(&task_run_id).await;

        db.record_file_touched(&task_run_id, "/repo/keep.rs", None)
            .await
            .expect("record");

        let conn = db.pool().get().await.expect("pool");
        conn.execute(
            "UPDATE project.session_touched_files \
             SET recorded_at = NOW() - make_interval(days => 100) \
             WHERE task_run_id = $1",
            &[&task_run_id],
        )
        .await
        .expect("backdate");

        // A 100-day-old row is INSIDE the 90-day project-snapshot scan's reach
        // plus the margin, so the shipped default must not delete it.
        db.cleanup_old_session_touched_files(DEFAULT_RETENTION_DAYS)
            .await
            .expect("cleanup");

        let files = db.get_files_touched(&task_run_id).await.expect("read back");
        assert_eq!(
            files,
            vec!["/repo/keep.rs".to_string()],
            "a row younger than the default retention must survive"
        );

        let _ = db.clear_files_touched(&task_run_id).await;
    }
}
