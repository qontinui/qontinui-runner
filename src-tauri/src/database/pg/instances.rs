//! PostgreSQL coord.runner_instances operations for multi-instance coordination.

use super::PgDb;
use serde::Serialize;

/// A runner instance row from the database.
#[derive(Debug, Clone, Serialize)]
pub struct DbRunnerInstance {
    pub id: String,
    pub name: String,
    pub port: i32,
    pub hostname: String,
    pub is_primary: bool,
    pub pid: Option<i32>,
    pub status: String,
    pub last_heartbeat: String,
    pub started_at: String,
    pub running_tasks: i32,
}

impl PgDb {
    /// Upsert a runner instance (insert or update on port conflict).
    pub async fn upsert_runner_instance(
        &self,
        id: &str,
        name: &str,
        port: u16,
        hostname: &str,
        is_primary: bool,
        pid: Option<u32>,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let port_i32 = port as i32;
        let pid_i32 = pid.map(|p| p as i32);

        // Remove any stale entry on the same port (from a previous runner with a different id)
        // before upserting. This avoids a unique-constraint violation on the port column.
        conn.execute(
            "DELETE FROM coord.runner_instances WHERE port = $1 AND id != $2",
            &[
                &port_i32 as &(dyn tokio_postgres::types::ToSql + Sync),
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG upsert_runner_instance (port cleanup): {}", e))?;

        conn.execute(
            r#"
            INSERT INTO coord.runner_instances (id, name, port, hostname, is_primary, pid, status, last_heartbeat, started_at, running_tasks)
            VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW(), 0)
            ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                port = EXCLUDED.port,
                hostname = EXCLUDED.hostname,
                is_primary = EXCLUDED.is_primary,
                pid = EXCLUDED.pid,
                status = EXCLUDED.status,
                last_heartbeat = NOW()
            "#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &name as &(dyn tokio_postgres::types::ToSql + Sync),
                &port_i32 as &(dyn tokio_postgres::types::ToSql + Sync),
                &hostname as &(dyn tokio_postgres::types::ToSql + Sync),
                &is_primary as &(dyn tokio_postgres::types::ToSql + Sync),
                &pid_i32 as &(dyn tokio_postgres::types::ToSql + Sync),
                &status as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG upsert_runner_instance: {}", e))?;

        Ok(())
    }

    /// Get all runner instances from the database.
    pub async fn get_all_runner_instances(&self) -> Result<Vec<DbRunnerInstance>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, port, hostname, is_primary, pid, status,
                       last_heartbeat::TEXT, started_at::TEXT, running_tasks
                FROM coord.runner_instances
                ORDER BY is_primary DESC, port ASC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_all_runner_instances: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| DbRunnerInstance {
                id: row.get(0),
                name: row.get(1),
                port: row.get(2),
                hostname: row.get(3),
                is_primary: row.get(4),
                pid: row.get(5),
                status: row.get(6),
                last_heartbeat: row.get(7),
                started_at: row.get(8),
                running_tasks: row.get(9),
            })
            .collect())
    }

    /// Remove a runner instance from the database.
    pub async fn remove_runner_instance(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let count = conn
            .execute("DELETE FROM coord.runner_instances WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG remove_runner_instance: {}", e))?;

        Ok(count > 0)
    }

    /// Update heartbeat timestamp and running task count for an instance.
    pub async fn update_runner_instance_heartbeat(
        &self,
        id: &str,
        running_tasks: Option<u32>,
        status: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let tasks = running_tasks.map(|t| t as i32);

        let count = conn
            .execute(
                r#"
                UPDATE coord.runner_instances
                SET last_heartbeat = NOW(),
                    status = $2,
                    running_tasks = COALESCE($3, running_tasks)
                WHERE id = $1
                "#,
                &[
                    &id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &status as &(dyn tokio_postgres::types::ToSql + Sync),
                    &tasks as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| format!("PG update_runner_instance_heartbeat: {}", e))?;

        Ok(count > 0)
    }

    /// Mark instances as unhealthy if their last heartbeat is older than the threshold.
    /// Returns the number of instances marked.
    pub async fn mark_stale_runner_instances(
        &self,
        stale_threshold_secs: i64,
    ) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let count = conn
            .execute(
                r#"
                UPDATE coord.runner_instances
                SET status = 'unhealthy'
                WHERE status = 'healthy'
                  AND is_primary = FALSE
                  AND last_heartbeat < NOW() - ($1 * INTERVAL '1 second')
                "#,
                &[&stale_threshold_secs],
            )
            .await
            .map_err(|e| format!("PG mark_stale_runner_instances: {}", e))?;

        Ok(count)
    }

    /// Remove instances that have been stopped or unhealthy beyond a timeout.
    /// Returns the number of instances cleaned up.
    pub async fn cleanup_dead_runner_instances(
        &self,
        dead_threshold_secs: i64,
    ) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let count = conn
            .execute(
                r#"
                DELETE FROM coord.runner_instances
                WHERE status IN ('stopped', 'unhealthy')
                  AND last_heartbeat < NOW() - ($1 * INTERVAL '1 second')
                "#,
                &[&dead_threshold_secs],
            )
            .await
            .map_err(|e| format!("PG cleanup_dead_runner_instances: {}", e))?;

        Ok(count)
    }
}
