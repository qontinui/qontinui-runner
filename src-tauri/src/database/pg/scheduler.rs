//! PostgreSQL scheduler_settings operations (raw SQL).
//!
//! Mirrors the SQLite scheduler settings from database/scheduler.rs.

use super::PgDb;
use crate::scheduler::SchedulerSettings;

impl PgDb {
    // ========================================================================
    // Scheduler Settings (singleton table, id=1)
    // ========================================================================

    /// Get global scheduler settings. Returns defaults if no row exists.
    pub async fn get_scheduler_settings(&self) -> Result<SchedulerSettings, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT enabled, max_concurrent, default_auto_fix_on_failure, timezone FROM scheduler_settings WHERE id = 1",
                &[],
            )
            .await
            .map_err(|e| format!("PG get_scheduler_settings: {}", e))?;

        Ok(row
            .map(|r| SchedulerSettings {
                enabled: r.get(0),
                max_concurrent: r.get::<_, i32>(1) as u32,
                default_auto_fix_on_failure: r.get(2),
                timezone: r.get(3),
            })
            .unwrap_or_default())
    }

    /// Upsert global scheduler settings (single-row table, id=1).
    pub async fn update_scheduler_settings(&self, settings: &SchedulerSettings) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let max_concurrent = settings.max_concurrent as i32;

        conn.execute(
            r#"
            INSERT INTO scheduler_settings (id, enabled, max_concurrent, default_auto_fix_on_failure, timezone)
            VALUES (1, $1, $2, $3, $4)
            ON CONFLICT(id) DO UPDATE SET
                enabled = EXCLUDED.enabled,
                max_concurrent = EXCLUDED.max_concurrent,
                default_auto_fix_on_failure = EXCLUDED.default_auto_fix_on_failure,
                timezone = EXCLUDED.timezone
            "#,
            &[
                &settings.enabled as &(dyn tokio_postgres::types::ToSql + Sync),
                &max_concurrent as &(dyn tokio_postgres::types::ToSql + Sync),
                &settings.default_auto_fix_on_failure as &(dyn tokio_postgres::types::ToSql + Sync),
                &settings.timezone as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_scheduler_settings: {}", e))?;

        Ok(())
    }
}
