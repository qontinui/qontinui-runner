//! PostgreSQL operations for breakpoint snapshots.

use super::PgDb;
use crate::step_executor::breakpoint::BreakpointSnapshot;
use chrono::{DateTime, Utc};

/// Lightweight row returned when listing breakpoint snapshots.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BreakpointSnapshotRow {
    pub id: String,
    pub execution_id: String,
    pub step_index: i32,
    pub step_name: Option<String>,
    pub phase: Option<String>,
    pub iteration: Option<i32>,
    pub variables_json: String,
    pub last_screenshot_ref: Option<String>,
    pub pending_steps_json: String,
    pub freshness_ts: String,
    pub status: String,
    pub resumed_at: Option<String>,
    pub created_at: String,
}

impl PgDb {
    /// Save a breakpoint snapshot.
    pub async fn save_breakpoint_snapshot(
        &self,
        snapshot: &BreakpointSnapshot,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO breakpoint_snapshots
                (id, execution_id, step_index, step_name, phase, iteration,
                 variables_json, last_screenshot_ref, pending_steps_json,
                 freshness_ts, status, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (id) DO UPDATE SET
                status = EXCLUDED.status,
                variables_json = EXCLUDED.variables_json,
                pending_steps_json = EXCLUDED.pending_steps_json
            "#,
            &[
                &snapshot.id,
                &snapshot.execution_id,
                &(snapshot.step_index as i32),
                &snapshot.step_name,
                &snapshot.phase,
                &snapshot.iteration.map(|i| i as i32),
                &snapshot.variables_json,
                &snapshot.last_screenshot_ref,
                &snapshot.pending_steps_json,
                &snapshot.freshness_ts,
                &snapshot.status.to_string(),
                &snapshot.created_at,
            ],
        )
        .await
        .map_err(|e| format!("Failed to save breakpoint snapshot: {}", e))?;

        Ok(())
    }

    /// Get a breakpoint snapshot by ID.
    pub async fn get_breakpoint_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<Option<BreakpointSnapshotRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, execution_id, step_index, step_name, phase, iteration,
                       variables_json, last_screenshot_ref, pending_steps_json,
                       freshness_ts::TEXT, status, resumed_at::TEXT, created_at::TEXT
                FROM breakpoint_snapshots
                WHERE id = $1
                "#,
                &[&snapshot_id],
            )
            .await
            .map_err(|e| format!("Failed to get breakpoint snapshot: {}", e))?;

        Ok(row.map(|r| BreakpointSnapshotRow {
            id: r.get(0),
            execution_id: r.get(1),
            step_index: r.get(2),
            step_name: r.get(3),
            phase: r.get(4),
            iteration: r.get(5),
            variables_json: r.get(6),
            last_screenshot_ref: r.get(7),
            pending_steps_json: r.get(8),
            freshness_ts: r.get(9),
            status: r.get(10),
            resumed_at: r.get(11),
            created_at: r.get(12),
        }))
    }

    /// List breakpoint snapshots for a task run.
    pub async fn list_breakpoint_snapshots(
        &self,
        execution_id: &str,
    ) -> Result<Vec<BreakpointSnapshotRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, execution_id, step_index, step_name, phase, iteration,
                       variables_json, last_screenshot_ref, pending_steps_json,
                       freshness_ts::TEXT, status, resumed_at::TEXT, created_at::TEXT
                FROM breakpoint_snapshots
                WHERE execution_id = $1
                ORDER BY created_at DESC
                "#,
                &[&execution_id],
            )
            .await
            .map_err(|e| format!("Failed to list breakpoint snapshots: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| BreakpointSnapshotRow {
                id: r.get(0),
                execution_id: r.get(1),
                step_index: r.get(2),
                step_name: r.get(3),
                phase: r.get(4),
                iteration: r.get(5),
                variables_json: r.get(6),
                last_screenshot_ref: r.get(7),
                pending_steps_json: r.get(8),
                freshness_ts: r.get(9),
                status: r.get(10),
                resumed_at: r.get(11),
                created_at: r.get(12),
            })
            .collect())
    }

    /// Resume a breakpoint snapshot (set status to 'resumed').
    pub async fn resume_breakpoint_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let now: DateTime<Utc> = Utc::now();
        let rows_affected = conn
            .execute(
                r#"
                UPDATE breakpoint_snapshots
                SET status = 'resumed', resumed_at = $2
                WHERE id = $1 AND status = 'waiting'
                "#,
                &[&snapshot_id, &now],
            )
            .await
            .map_err(|e| format!("Failed to resume breakpoint snapshot: {}", e))?;

        Ok(rows_affected > 0)
    }

    /// Expire all waiting snapshots for a task run (e.g., on task cancellation).
    pub async fn expire_breakpoint_snapshots(
        &self,
        execution_id: &str,
    ) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows_affected = conn
            .execute(
                r#"
                UPDATE breakpoint_snapshots
                SET status = 'expired'
                WHERE execution_id = $1 AND status = 'waiting'
                "#,
                &[&execution_id],
            )
            .await
            .map_err(|e| format!("Failed to expire breakpoint snapshots: {}", e))?;

        Ok(rows_affected)
    }
}
