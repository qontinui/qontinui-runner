//! Database operations for durable workflow queue persistence.
//!
//! Implements Inngest-inspired durable queue: queued workflows are persisted to
//! SQLite so they survive crashes and restarts. On startup, pending/running
//! entries are reloaded into the in-memory queue.

use rusqlite::params;
use tracing::{debug, info, warn};

use super::CheckpointDb;

/// Status of a queued workflow in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueEntryStatus {
    /// Waiting in queue to be executed.
    Pending,
    /// Currently executing (semaphore permit held).
    Running,
    /// Completed successfully.
    Completed,
    /// Failed during execution.
    Failed,
    /// Cancelled by user before execution started.
    Cancelled,
}

impl QueueEntryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            other => {
                tracing::warn!("Unknown queue entry status '{}', defaulting to Pending", other);
                Self::Pending
            }
        }
    }
}

/// A persisted queue entry stored in the database.
#[derive(Debug, Clone)]
pub struct PersistedQueueEntry {
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub queued_at: String,
    pub priority: u8,
    pub status: QueueEntryStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error_message: Option<String>,
    pub task_run_id: Option<String>,
    pub retry_count: i32,
    pub max_retries: i32,
}

impl CheckpointDb {
    // ========================================================================
    // Queue Entry Operations
    // ========================================================================

    /// Persist a new queue entry with status 'pending'.
    pub fn queue_insert(&self, entry: &PersistedQueueEntry) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO queued_workflows (
                id, workflow_id, workflow_name, queued_at, priority, status,
                started_at, completed_at, error_message, task_run_id,
                retry_count, max_retries
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                entry.id,
                entry.workflow_id,
                entry.workflow_name,
                entry.queued_at,
                entry.priority,
                entry.status.as_str(),
                entry.started_at,
                entry.completed_at,
                entry.error_message,
                entry.task_run_id,
                entry.retry_count,
                entry.max_retries,
            ],
        )
        .map_err(|e| format!("Failed to insert queue entry: {}", e))?;

        debug!("Persisted queue entry '{}' ({})", entry.workflow_name, entry.id);
        Ok(())
    }

    /// Update the status of a queue entry.
    pub fn queue_update_status(
        &self,
        id: &str,
        status: &QueueEntryStatus,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        let (started_at_update, completed_at_update) = match status {
            QueueEntryStatus::Running => (Some(&now as &str), None),
            QueueEntryStatus::Completed | QueueEntryStatus::Failed | QueueEntryStatus::Cancelled => {
                (None, Some(&now as &str))
            }
            _ => (None, None),
        };

        // Always update status and timestamp
        conn.execute(
            "UPDATE queued_workflows SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), now, id],
        )
        .map_err(|e| format!("Failed to update queue entry status: {}", e))?;

        // Set started_at when transitioning to Running
        if let Some(started) = started_at_update {
            conn.execute(
                "UPDATE queued_workflows SET started_at = ?1 WHERE id = ?2 AND started_at IS NULL",
                params![started, id],
            )
            .map_err(|e| format!("Failed to set started_at: {}", e))?;
        }

        // Set completed_at when reaching a terminal status
        if let Some(completed) = completed_at_update {
            conn.execute(
                "UPDATE queued_workflows SET completed_at = ?1 WHERE id = ?2",
                params![completed, id],
            )
            .map_err(|e| format!("Failed to set completed_at: {}", e))?;
        }

        debug!("Updated queue entry '{}' → {}", id, status.as_str());
        Ok(())
    }

    /// Set the error message on a failed queue entry.
    pub fn queue_set_error(&self, id: &str, error: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE queued_workflows SET error_message = ?1 WHERE id = ?2",
            params![error, id],
        )
        .map_err(|e| format!("Failed to set queue error: {}", e))?;
        Ok(())
    }

    /// Link a queue entry to a task_run_id.
    pub fn queue_set_task_run_id(&self, id: &str, task_run_id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE queued_workflows SET task_run_id = ?1 WHERE id = ?2",
            params![task_run_id, id],
        )
        .map_err(|e| format!("Failed to set task_run_id on queue entry: {}", e))?;
        Ok(())
    }

    /// Increment retry count for a queue entry.
    pub fn queue_increment_retry(&self, id: &str) -> Result<i32, String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE queued_workflows SET retry_count = retry_count + 1, status = 'pending', error_message = NULL WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to increment retry: {}", e))?;

        let count: i32 = conn
            .query_row(
                "SELECT retry_count FROM queued_workflows WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to read retry count: {}", e))?;

        info!("Queue entry '{}' retry count → {}", id, count);
        Ok(count)
    }

    /// Load all pending queue entries (for reload on startup).
    pub fn queue_load_pending(&self) -> Result<Vec<PersistedQueueEntry>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, workflow_id, workflow_name, queued_at, priority, status,
                        started_at, completed_at, error_message, task_run_id,
                        retry_count, max_retries
                 FROM queued_workflows
                 WHERE status IN ('pending', 'running')
                 ORDER BY priority DESC, queued_at ASC",
            )
            .map_err(|e| format!("Failed to prepare queue load query: {}", e))?;

        let entries = stmt
            .query_map([], |row| {
                Ok(PersistedQueueEntry {
                    id: row.get(0)?,
                    workflow_id: row.get(1)?,
                    workflow_name: row.get(2)?,
                    queued_at: row.get(3)?,
                    priority: row.get::<_, u8>(4)?,
                    status: QueueEntryStatus::from_str(
                        &row.get::<_, String>(5).unwrap_or_default(),
                    ),
                    started_at: row.get(6)?,
                    completed_at: row.get(7)?,
                    error_message: row.get(8)?,
                    task_run_id: row.get(9)?,
                    retry_count: row.get(10)?,
                    max_retries: row.get(11)?,
                })
            })
            .map_err(|e| format!("Failed to query pending queue entries: {}", e))?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();

        info!(
            "Loaded {} pending/running queue entries from database",
            entries.len()
        );
        Ok(entries)
    }

    /// Mark any entries that were 'running' when the app crashed as 'pending'
    /// so they get re-queued on startup.
    pub fn queue_recover_crashed(&self) -> Result<usize, String> {
        let conn = self.get_conn()?;
        let count = conn
            .execute(
                "UPDATE queued_workflows SET status = 'pending', started_at = NULL
                 WHERE status = 'running'",
                [],
            )
            .map_err(|e| format!("Failed to recover crashed queue entries: {}", e))?;

        if count > 0 {
            warn!(
                "Recovered {} queue entries that were running when app crashed",
                count
            );
        }
        Ok(count)
    }

    /// Clean up old completed/failed/cancelled entries older than the given
    /// number of days.
    pub fn queue_cleanup(&self, older_than_days: i64) -> Result<usize, String> {
        let conn = self.get_conn()?;
        let cutoff = chrono::Utc::now()
            - chrono::Duration::days(older_than_days);
        let cutoff_str = cutoff.to_rfc3339();

        let count = conn
            .execute(
                "DELETE FROM queued_workflows
                 WHERE status IN ('completed', 'failed', 'cancelled')
                   AND completed_at < ?1",
                params![cutoff_str],
            )
            .map_err(|e| format!("Failed to clean up queue entries: {}", e))?;

        if count > 0 {
            info!(
                "Cleaned up {} old queue entries (older than {} days)",
                count, older_than_days
            );
        }
        Ok(count)
    }

    /// Get a single queue entry by ID.
    pub fn queue_get(&self, id: &str) -> Result<Option<PersistedQueueEntry>, String> {
        let conn = self.get_conn()?;
        let result = conn
            .query_row(
                "SELECT id, workflow_id, workflow_name, queued_at, priority, status,
                        started_at, completed_at, error_message, task_run_id,
                        retry_count, max_retries
                 FROM queued_workflows WHERE id = ?1",
                params![id],
                |row| {
                    Ok(PersistedQueueEntry {
                        id: row.get(0)?,
                        workflow_id: row.get(1)?,
                        workflow_name: row.get(2)?,
                        queued_at: row.get(3)?,
                        priority: row.get::<_, u8>(4)?,
                        status: QueueEntryStatus::from_str(
                            &row.get::<_, String>(5).unwrap_or_default(),
                        ),
                        started_at: row.get(6)?,
                        completed_at: row.get(7)?,
                        error_message: row.get(8)?,
                        task_run_id: row.get(9)?,
                        retry_count: row.get(10)?,
                        max_retries: row.get(11)?,
                    })
                },
            );

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get queue entry: {}", e)),
        }
    }
}
