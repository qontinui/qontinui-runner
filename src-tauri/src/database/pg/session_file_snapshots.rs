//! PostgreSQL CRUD for the `project.session_file_snapshots` table.
//!
//! See §2 of `qontinui-dev-notes/plans/productivity-stack.md`. Each row
//! represents a content-addressed copy of a file taken at the moment a
//! session first edited it, used by `/rewind-session` to roll back a
//! failed session's edits.
//!
//! Per `feedback_memory_pressure.md` the blob bytes themselves live on
//! disk (under `<runner_data_dir>/session_snapshots/<session_id>/...`);
//! PG only stores metadata so the table stays slim.
//!
//! The pruner (`prune_older_than`) is invoked on a 24h interval from
//! `database/pg/scheduler.rs::start_session_snapshot_pruner` — see Phase
//! 4 §8 for the cadence rationale.
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
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tracing::{info, warn};

/// A row from the `session_file_snapshots` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRow {
    pub id: String,
    pub session_id: String,
    pub file_path: String,
    pub snapshot_blob_path: String,
    pub blob_sha256: String,
    pub captured_before: bool,
    pub taken_at: String,
}

const SELECT_COLUMNS: &str = r#"
    id::text, session_id, file_path, snapshot_blob_path, blob_sha256,
    captured_before, taken_at::text
"#;

#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
fn row_from_pg(r: &tokio_postgres::Row) -> SnapshotRow {
    SnapshotRow {
        id: r.get(0),
        session_id: r.get(1),
        file_path: r.get(2),
        snapshot_blob_path: r.get(3),
        blob_sha256: r.get(4),
        captured_before: r.get(5),
        taken_at: r.get(6),
    }
}

impl PgDb {
    /// Insert a snapshot row. The blob itself must already be written to
    /// disk; this only records the metadata. `captured_before=true`
    /// indicates a pre-edit snapshot (the rollback target); `false` is
    /// informational (post-edit, e.g. for forensic comparison) and Phase
    /// 4 doesn't write any of those, but the column is left in for
    /// future use.
    pub async fn insert_snapshot(
        &self,
        session_id: &str,
        file_path: &str,
        blob_path: &str,
        blob_sha256: &str,
        captured_before: bool,
    ) -> Result<SnapshotRow, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                &format!(
                    r#"
                    INSERT INTO project.session_file_snapshots
                        (session_id, file_path, snapshot_blob_path, blob_sha256,
                         captured_before)
                    VALUES ($1, $2, $3, $4, $5)
                    RETURNING {}
                    "#,
                    SELECT_COLUMNS
                ),
                &[
                    &session_id,
                    &file_path,
                    &blob_path,
                    &blob_sha256,
                    &captured_before,
                ],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("Failed to insert session_file_snapshot", &e))?;

        Ok(row_from_pg(&row))
    }

    /// All snapshot rows for a session, ordered by `taken_at` ascending.
    /// Used by `/rewind-session` to walk the rollback set.
    pub async fn get_snapshots_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<SnapshotRow>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM project.session_file_snapshots
                    WHERE session_id = $1
                    ORDER BY taken_at ASC
                    "#,
                    SELECT_COLUMNS
                ),
                &[&session_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("Failed to list session_file_snapshots", &e))?;

        Ok(rows.iter().map(row_from_pg).collect())
    }

    /// Cheap pre-check the dispatcher uses to skip duplicate snapshots
    /// when the same session edits the same file more than once. Only
    /// the pre-first-edit snapshot is what /rewind-session wants to
    /// restore from — subsequent edits would overwrite the rollback
    /// target and defeat the purpose.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn has_pre_edit_snapshot(
        &self,
        session_id: &str,
        file_path: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM project.session_file_snapshots
                    WHERE session_id = $1
                      AND file_path = $2
                      AND captured_before = TRUE
                )
                "#,
                &[&session_id, &file_path],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("Failed to check pre-edit snapshot", &e))?;

        Ok(row.get(0))
    }

    /// Delete snapshots older than `days`, returning a count of (rows,
    /// blobs) pruned. Errors deleting a single blob file are logged but
    /// do not abort — the PG row delete succeeds either way to prevent
    /// the table from accumulating dangling rows. The on-disk blob is
    /// best-effort cleanup.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn prune_snapshots_older_than(&self, days: i32) -> Result<(usize, usize), String> {
        if days <= 0 {
            return Err("prune_snapshots_older_than: days must be positive".to_string());
        }
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Two-step: first SELECT the rows we'll prune so we know which
        // blobs to delete, then DELETE. We can't use RETURNING here
        // because we want to delete the blobs even if the txn rolls
        // back later (and we'd rather the rows go than hold them
        // hostage to one missing blob).
        let rows = conn
            .query(
                r#"
                SELECT id::text, snapshot_blob_path
                FROM project.session_file_snapshots
                WHERE taken_at < NOW() - ($1 || ' days')::interval
                "#,
                &[&days.to_string()],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("Failed to scan for prunable snapshots", &e))?;

        let mut blobs_deleted = 0usize;
        for r in &rows {
            let blob_path: String = r.get(1);
            match std::fs::remove_file(Path::new(&blob_path)) {
                Ok(()) => {
                    blobs_deleted += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Already gone — not an error.
                }
                Err(e) => {
                    warn!(
                        "prune_snapshots_older_than: failed to remove '{}': {}",
                        blob_path, e
                    );
                }
            }
        }

        let deleted =
            conn.execute(
                r#"
                DELETE FROM project.session_file_snapshots
                WHERE taken_at < NOW() - ($1 || ' days')::interval
                "#,
                &[&days.to_string()],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("Failed to delete old snapshots", &e))? as usize;

        if deleted > 0 || blobs_deleted > 0 {
            info!(
                "prune_snapshots_older_than({} days): deleted {} rows, {} blobs",
                days, deleted, blobs_deleted
            );
        }

        Ok((deleted, blobs_deleted))
    }
}

// =============================================================================
// Background pruner
// =============================================================================

/// How long snapshots are retained before the daily pruner deletes them.
/// Per `feedback_memory_pressure.md` and Phase 4 §8, 7 days strikes a balance
/// between giving the user time to /rewind-session a long-running task and
/// keeping disk usage bounded.
pub const SNAPSHOT_RETENTION_DAYS: i32 = 7;

/// Spawn a background task that prunes old snapshot rows + blobs once every
/// 24 hours. The task survives until the runner exits — there's no
/// shutdown signalling because the operation is short and idempotent.
///
/// Patterned after `memory::scheduler::start_memory_scheduler`, with an
/// initial 5-minute delay to keep startup tidy.
pub fn start_session_snapshot_pruner(pg: Arc<PgDb>) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        // Let the runner finish its other startup work before we hammer
        // disk + PG with prune scans.
        tokio::time::sleep(Duration::from_secs(300)).await;
        info!(
            "session_file_snapshots pruner started (retention={} days, interval=24h)",
            SNAPSHOT_RETENTION_DAYS
        );

        let mut tick = interval(Duration::from_secs(86_400));
        loop {
            tick.tick().await;
            match pg.prune_snapshots_older_than(SNAPSHOT_RETENTION_DAYS).await {
                Ok((rows, blobs)) => {
                    if rows > 0 || blobs > 0 {
                        info!(
                            "session_file_snapshots pruner: pruned {} rows, {} blobs",
                            rows, blobs
                        );
                    }
                }
                Err(e) => {
                    warn!("session_file_snapshots pruner: prune failed: {}", e);
                }
            }
        }
    })
}
