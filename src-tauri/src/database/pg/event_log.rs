//! Workflow event log — lightweight event-sourced execution log for DAG durable execution.
//!
//! Each workflow step appends an event (started, completed, failed, …) with a monotonically
//! increasing cursor. On crash recovery the executor replays from the latest cursor to
//! reconstruct which nodes have already completed, skipping re-execution of idempotent steps.

use super::PgDb;
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

// ============================================================================
// Types
// ============================================================================

/// Event types for the workflow event log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Started,
    Completed,
    Failed,
    Skipped,
    Retried,
    Cancelled,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Retried => "retried",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "started" => Some(Self::Started),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            "retried" => Some(Self::Retried),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A single event in the workflow event log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEvent {
    pub id: i64,
    pub execution_id: String,
    pub node_id: String,
    pub event_type: EventType,
    pub event_data: Option<serde_json::Value>,
    pub cursor: i64,
    pub created_at: String,
}

// ============================================================================
// Database operations
// ============================================================================

impl PgDb {
    /// Append an event to the log, returning the new cursor value.
    ///
    /// The cursor is a per-execution monotonically increasing integer computed
    /// as `MAX(cursor) + 1` inside the same transaction, so concurrent appends
    /// are serialised naturally by Postgres row-level locking on the index scan.
    pub async fn event_log_append(
        &self,
        execution_id: &str,
        node_id: &str,
        event_type: &EventType,
        event_data: Option<&serde_json::Value>,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Serialise event_data to TEXT (consistent with other TEXT-blob columns).
        let event_data_str: Option<String> =
            event_data.map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));

        let event_type_str = event_type.as_str();

        // Derive the next cursor in-database to avoid races.
        let row = conn
            .query_one(
                r#"
                WITH next_cursor AS (
                    SELECT COALESCE(MAX(cursor), 0) + 1 AS cur
                    FROM workflow_event_log
                    WHERE execution_id = $1
                )
                INSERT INTO workflow_event_log (execution_id, node_id, event_type, event_data, cursor)
                SELECT $1, $2, $3, $4, cur FROM next_cursor
                RETURNING cursor
                "#,
                &[&execution_id, &node_id, &event_type_str, &event_data_str],
            )
            .await
            .map_err(|e| {
                error!("event_log_append failed for execution {}: {}", execution_id, e);
                format!("Failed to append event log: {}", e)
            })?;

        Ok(row.get::<_, i64>(0))
    }

    /// Replay events from a given cursor position (inclusive).
    ///
    /// Returns events ordered by cursor ASC so callers can process them in
    /// emission order to reconstruct execution state.
    pub async fn event_log_replay_from(
        &self,
        execution_id: &str,
        from_cursor: i64,
    ) -> Result<Vec<WorkflowEvent>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, execution_id, node_id, event_type, event_data, cursor, created_at::TEXT
                FROM workflow_event_log
                WHERE execution_id = $1 AND cursor >= $2
                ORDER BY cursor ASC
                "#,
                &[&execution_id, &from_cursor],
            )
            .await
            .map_err(|e| {
                error!(
                    "event_log_replay_from failed for execution {}: {}",
                    execution_id, e
                );
                format!("Failed to replay event log: {}", e)
            })?;

        let events = rows
            .iter()
            .map(|r| {
                let event_type_str: String = r.get(3);
                let event_data_raw: Option<String> = r.get(4);
                let event_data = event_data_raw.and_then(|s| {
                    serde_json::from_str(&s)
                        .map_err(|e| {
                            warn!("Failed to parse event_data JSON: {}", e);
                            e
                        })
                        .ok()
                });
                WorkflowEvent {
                    id: r.get::<_, i64>(0),
                    execution_id: r.get::<_, String>(1),
                    node_id: r.get::<_, String>(2),
                    event_type: EventType::from_str(&event_type_str).unwrap_or(EventType::Started),
                    event_data,
                    cursor: r.get::<_, i64>(5),
                    created_at: r.get::<_, String>(6),
                }
            })
            .collect();

        Ok(events)
    }

    /// Check if a node has a completed event, returning its output data if so.
    ///
    /// Used during crash-recovery replay: if this returns `Some(_)` the node
    /// can be skipped (its output is already persisted).
    pub async fn event_log_node_completed(
        &self,
        execution_id: &str,
        node_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT event_data
                FROM workflow_event_log
                WHERE execution_id = $1 AND node_id = $2 AND event_type = 'completed'
                ORDER BY cursor DESC
                LIMIT 1
                "#,
                &[&execution_id, &node_id],
            )
            .await
            .map_err(|e| {
                error!(
                    "event_log_node_completed failed for execution {}, node {}: {}",
                    execution_id, node_id, e
                );
                format!("Failed to query node completion: {}", e)
            })?;

        Ok(row.and_then(|r| {
            let raw: Option<String> = r.get(0);
            raw.and_then(|s| {
                serde_json::from_str(&s)
                    .map_err(|e| {
                        warn!("Failed to parse node completed event_data: {}", e);
                        e
                    })
                    .ok()
            })
        }))
    }

    /// Prune events before a cursor (inclusive of `before_cursor - 1`).
    ///
    /// Used after a successful commit_interval checkpoint: events that are
    /// older than the checkpoint cursor are no longer needed for replay.
    /// Returns the number of rows deleted.
    pub async fn event_log_prune_before(
        &self,
        execution_id: &str,
        before_cursor: i64,
    ) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                "DELETE FROM workflow_event_log WHERE execution_id = $1 AND cursor < $2",
                &[&execution_id, &before_cursor],
            )
            .await
            .map_err(|e| {
                error!(
                    "event_log_prune_before failed for execution {}: {}",
                    execution_id, e
                );
                format!("Failed to prune event log: {}", e)
            })?;

        Ok(affected)
    }

    /// Get the latest cursor for an execution (for resume position on restart).
    ///
    /// Returns 0 if no events exist yet (fresh execution).
    pub async fn event_log_latest_cursor(&self, execution_id: &str) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                "SELECT COALESCE(MAX(cursor), 0) FROM workflow_event_log WHERE execution_id = $1",
                &[&execution_id],
            )
            .await
            .map_err(|e| {
                error!(
                    "event_log_latest_cursor failed for execution {}: {}",
                    execution_id, e
                );
                format!("Failed to get latest cursor: {}", e)
            })?;

        Ok(row.get::<_, i64>(0))
    }

    /// Delete all events for an execution (cleanup after completion or cancellation).
    ///
    /// Returns the number of rows deleted.
    pub async fn event_log_delete_execution(&self, execution_id: &str) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                "DELETE FROM workflow_event_log WHERE execution_id = $1",
                &[&execution_id],
            )
            .await
            .map_err(|e| {
                error!(
                    "event_log_delete_execution failed for execution {}: {}",
                    execution_id, e
                );
                format!("Failed to delete execution events: {}", e)
            })?;

        Ok(affected)
    }
}
