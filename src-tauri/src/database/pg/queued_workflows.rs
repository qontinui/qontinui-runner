//! PostgreSQL durable workflow queue operations via Clorinde-generated queries.
//!
//! PG-primary implementation of the Inngest-inspired durable workflow queue.
//! Mirrors the SQLite `queue_ops.rs` API for dual-write compatibility.

use super::PgDb;
use crate::database::queue_ops::{PersistedQueueEntry, QueueEntryStatus};
use tracing::{debug, info, warn};

/// Convert Clorinde empty string (from NULL) to Option::None.
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() { None } else { Some(s.to_string()) }
}

/// Convert Clorinde DateTime to Option<String> (empty = NULL).
fn ts_to_opt(ts: &chrono::DateTime<chrono::FixedOffset>) -> Option<String> {
    let s = ts.to_rfc3339();
    // Clorinde uses epoch-zero for NULL timestamps
    if s.starts_with("1970-01-01") { None } else { Some(s) }
}

impl PgDb {
    // ========================================================================
    // Queue Entry Operations
    // ========================================================================

    /// Persist a new queue entry with status 'pending'.
    pub async fn pg_queue_insert(&self, entry: &PersistedQueueEntry) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::queued_workflows::queue_insert()
            .bind(
                &conn,
                &entry.id.as_str(),
                &entry.workflow_id.as_str(),
                &entry.workflow_name.as_str(),
                &(entry.priority as i32),
                &entry.error_message.as_deref(),
                &entry.task_run_id.as_deref(),
                &entry.max_retries,
            )
            .one()
            .await
            .map_err(|e| format!("PG queue_insert: {}", e))?;

        debug!("PG: Persisted queue entry '{}' ({})", entry.workflow_name, entry.id);
        Ok(())
    }

    /// Update the status of a queue entry.
    pub async fn pg_queue_update_status(
        &self,
        id: &str,
        status: &QueueEntryStatus,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        // Update status
        qontinui_db::queries::queued_workflows::queue_update_status()
            .bind(&conn, &status.as_str(), &id)
            .opt()
            .await
            .map_err(|e| format!("PG queue_update_status: {}", e))?;

        // Set started_at when transitioning to Running
        if matches!(status, QueueEntryStatus::Running) {
            qontinui_db::queries::queued_workflows::queue_set_started()
                .bind(&conn, &id)
                .opt()
                .await
                .map_err(|e| format!("PG queue_set_started: {}", e))?;
        }

        // Set completed_at when reaching a terminal status
        if matches!(
            status,
            QueueEntryStatus::Completed | QueueEntryStatus::Failed | QueueEntryStatus::Cancelled
        ) {
            qontinui_db::queries::queued_workflows::queue_set_completed()
                .bind(&conn, &status.as_str(), &id)
                .opt()
                .await
                .map_err(|e| format!("PG queue_set_completed: {}", e))?;
        }

        debug!("PG: Updated queue entry '{}' → {}", id, status.as_str());
        Ok(())
    }

    /// Set the error message on a queue entry.
    pub async fn pg_queue_set_error(&self, id: &str, error: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::queued_workflows::queue_set_error()
            .bind(&conn, &error, &id)
            .opt()
            .await
            .map_err(|e| format!("PG queue_set_error: {}", e))?;
        Ok(())
    }

    /// Link a queue entry to a task_run_id.
    pub async fn pg_queue_set_task_run_id(
        &self,
        id: &str,
        task_run_id: &str,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::queued_workflows::queue_set_task_run_id()
            .bind(&conn, &task_run_id, &id)
            .opt()
            .await
            .map_err(|e| format!("PG queue_set_task_run_id: {}", e))?;
        Ok(())
    }

    /// Increment retry count and reset to pending.
    pub async fn pg_queue_increment_retry(&self, id: &str) -> Result<i32, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let count = qontinui_db::queries::queued_workflows::queue_increment_retry()
            .bind(&conn, &id)
            .one()
            .await
            .map_err(|e| format!("PG queue_increment_retry: {}", e))?;

        info!("PG: Queue entry '{}' retry count → {}", id, count);
        Ok(count)
    }

    /// Load all pending/running queue entries for startup recovery.
    pub async fn pg_queue_load_pending(&self) -> Result<Vec<PersistedQueueEntry>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::queued_workflows::queue_load_pending()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG queue_load_pending: {}", e))?;

        let entries: Vec<PersistedQueueEntry> = rows
            .into_iter()
            .map(|r| PersistedQueueEntry {
                id: r.id,
                workflow_id: r.workflow_id,
                workflow_name: r.workflow_name,
                queued_at: r.queued_at.to_rfc3339(),
                priority: r.priority as u8,
                status: QueueEntryStatus::from_str(&r.status),
                started_at: ts_to_opt(&r.started_at),
                completed_at: ts_to_opt(&r.completed_at),
                error_message: non_empty(&r.error_message),
                task_run_id: non_empty(&r.task_run_id),
                retry_count: r.retry_count,
                max_retries: r.max_retries,
            })
            .collect();

        info!("PG: Loaded {} pending/running queue entries", entries.len());
        Ok(entries)
    }

    /// Mark any 'running' entries as 'pending' for crash recovery.
    pub async fn pg_queue_recover_crashed(&self) -> Result<usize, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let recovered = qontinui_db::queries::queued_workflows::queue_recover_crashed()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG queue_recover_crashed: {}", e))?;

        let count = recovered.len();
        if count > 0 {
            warn!("PG: Recovered {} queue entries that were running when app crashed", count);
        }
        Ok(count)
    }

    /// Clean up old terminal entries older than the given number of days.
    pub async fn pg_queue_cleanup(&self, older_than_days: i64) -> Result<usize, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(older_than_days))
            .fixed_offset();
        let deleted = qontinui_db::queries::queued_workflows::queue_cleanup()
            .bind(&conn, &cutoff)
            .all()
            .await
            .map_err(|e| format!("PG queue_cleanup: {}", e))?;

        let count = deleted.len();
        if count > 0 {
            info!("PG: Cleaned up {} old queue entries", count);
        }
        Ok(count)
    }
}
