//! PostgreSQL CRUD operations for Restate durable execution tables.

use super::PgDb;
use tracing::{error, info};

// ---------------------------------------------------------------------------
// Return types
// ---------------------------------------------------------------------------

/// A row from the `restate_workflow_executions` table.
pub struct RestateWorkflowExecution {
    pub execution_id: String,
    pub restate_workflow_id: String,
    pub restate_invocation_id: Option<String>,
    pub status: String,
    pub launched_via_restate: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// A row from the `restate_awakeables` table.
pub struct RestateAwakeable {
    pub awakeable_id: String,
    pub execution_id: String,
    pub awakeable_type: String,
    pub type_data: Option<String>,
    pub status: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

// ---------------------------------------------------------------------------
// impl PgDb — restate_workflow_executions
// ---------------------------------------------------------------------------

impl PgDb {
    /// Insert a new Restate workflow execution mapping.
    pub async fn save_restate_workflow_execution(
        &self,
        execution_id: &str,
        restate_workflow_id: &str,
        restate_invocation_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"INSERT INTO restate_workflow_executions
               (execution_id, restate_workflow_id, restate_invocation_id)
               VALUES ($1, $2, $3)
               ON CONFLICT (execution_id) DO UPDATE
               SET restate_workflow_id   = EXCLUDED.restate_workflow_id,
                   restate_invocation_id = EXCLUDED.restate_invocation_id,
                   updated_at            = NOW()"#,
            &[
                &execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &restate_workflow_id,
                &restate_invocation_id,
            ],
        )
        .await
        .map_err(|e| {
            error!("save_restate_workflow_execution failed: {}", e);
            format!("PG save_restate_workflow_execution: {}", e)
        })?;

        info!(
            "Saved Restate workflow execution mapping: execution_id={}, restate_workflow_id={}",
            execution_id, restate_workflow_id
        );
        Ok(())
    }

    /// Fetch a Restate workflow execution by its execution_id.
    pub async fn get_restate_workflow_execution(
        &self,
        execution_id: &str,
    ) -> Result<Option<RestateWorkflowExecution>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT execution_id,
                          restate_workflow_id,
                          restate_invocation_id,
                          status,
                          launched_via_restate,
                          created_at::TEXT,
                          updated_at::TEXT
                   FROM restate_workflow_executions
                   WHERE execution_id = $1"#,
                &[&execution_id as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| format!("PG get_restate_workflow_execution: {}", e))?;

        Ok(row.map(|r| RestateWorkflowExecution {
            execution_id: r.get(0),
            restate_workflow_id: r.get(1),
            restate_invocation_id: r.get(2),
            status: r.get(3),
            launched_via_restate: r.get(4),
            created_at: r.get(5),
            updated_at: r.get(6),
        }))
    }

    /// Update the status of a Restate workflow execution.
    pub async fn update_restate_workflow_status(
        &self,
        execution_id: &str,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows_modified = conn
            .execute(
                r#"UPDATE restate_workflow_executions
                   SET status = $2, updated_at = NOW()
                   WHERE execution_id = $1"#,
                &[
                    &execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &status,
                ],
            )
            .await
            .map_err(|e| {
                error!("update_restate_workflow_status failed: {}", e);
                format!("PG update_restate_workflow_status: {}", e)
            })?;

        if rows_modified == 0 {
            return Err(format!(
                "No restate_workflow_execution found for execution_id={}",
                execution_id
            ));
        }

        info!(
            "Updated Restate workflow status: execution_id={}, status={}",
            execution_id, status
        );
        Ok(())
    }

    /// Check whether an execution_id corresponds to a Restate-managed workflow.
    pub async fn is_restate_workflow(&self, execution_id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT 1 FROM restate_workflow_executions
                   WHERE execution_id = $1 AND launched_via_restate = TRUE"#,
                &[&execution_id as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| format!("PG is_restate_workflow: {}", e))?;

        Ok(row.is_some())
    }

    // -----------------------------------------------------------------------
    // restate_awakeables
    // -----------------------------------------------------------------------

    /// Insert a new Restate awakeable.
    pub async fn save_restate_awakeable(
        &self,
        awakeable_id: &str,
        execution_id: &str,
        awakeable_type: &str,
        type_data: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"INSERT INTO restate_awakeables
               (awakeable_id, execution_id, awakeable_type, type_data)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT (awakeable_id) DO NOTHING"#,
            &[
                &awakeable_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &execution_id,
                &awakeable_type,
                &type_data,
            ],
        )
        .await
        .map_err(|e| {
            error!("save_restate_awakeable failed: {}", e);
            format!("PG save_restate_awakeable: {}", e)
        })?;

        info!(
            "Saved Restate awakeable: id={}, execution_id={}, type={}",
            awakeable_id, execution_id, awakeable_type
        );
        Ok(())
    }

    /// Resolve (complete) a pending awakeable.
    pub async fn resolve_restate_awakeable(&self, awakeable_id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows_modified = conn
            .execute(
                r#"UPDATE restate_awakeables
                   SET status = 'resolved', resolved_at = NOW()
                   WHERE awakeable_id = $1 AND status = 'pending'"#,
                &[&awakeable_id as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| {
                error!("resolve_restate_awakeable failed: {}", e);
                format!("PG resolve_restate_awakeable: {}", e)
            })?;

        if rows_modified == 0 {
            return Err(format!(
                "No pending awakeable found for awakeable_id={}",
                awakeable_id
            ));
        }

        info!("Resolved Restate awakeable: id={}", awakeable_id);
        Ok(())
    }

    /// Find a pending awakeable by type and type_data.
    ///
    /// Used to locate the awakeable associated with a specific breakpoint snapshot
    /// (where `awakeable_type = "breakpoint"` and `type_data = snapshot_id`).
    pub async fn find_pending_awakeable_by_type_data(
        &self,
        execution_id: &str,
        awakeable_type: &str,
        type_data: &str,
    ) -> Result<Option<RestateAwakeable>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT awakeable_id,
                          execution_id,
                          awakeable_type,
                          type_data,
                          status,
                          created_at::TEXT,
                          resolved_at::TEXT
                   FROM restate_awakeables
                   WHERE execution_id = $1
                     AND awakeable_type = $2
                     AND type_data = $3
                     AND status = 'pending'
                   ORDER BY created_at DESC
                   LIMIT 1"#,
                &[
                    &execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &awakeable_type,
                    &type_data,
                ],
            )
            .await
            .map_err(|e| format!("PG find_pending_awakeable_by_type_data: {}", e))?;

        Ok(row.map(|r| RestateAwakeable {
            awakeable_id: r.get(0),
            execution_id: r.get(1),
            awakeable_type: r.get(2),
            type_data: r.get(3),
            status: r.get(4),
            created_at: r.get(5),
            resolved_at: r.get(6),
        }))
    }

    /// Get all pending awakeables for a given execution.
    pub async fn get_pending_awakeables(
        &self,
        execution_id: &str,
    ) -> Result<Vec<RestateAwakeable>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT awakeable_id,
                          execution_id,
                          awakeable_type,
                          type_data,
                          status,
                          created_at::TEXT,
                          resolved_at::TEXT
                   FROM restate_awakeables
                   WHERE execution_id = $1 AND status = 'pending'
                   ORDER BY created_at ASC"#,
                &[&execution_id as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| format!("PG get_pending_awakeables: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| RestateAwakeable {
                awakeable_id: r.get(0),
                execution_id: r.get(1),
                awakeable_type: r.get(2),
                type_data: r.get(3),
                status: r.get(4),
                created_at: r.get(5),
                resolved_at: r.get(6),
            })
            .collect())
    }
}
