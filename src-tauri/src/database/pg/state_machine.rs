//! PostgreSQL state_machine_configs CRUD operations (raw SQL).
//!
//! Mirrors the SQLite state_machine_configs/storage.rs operations.

use super::PgDb;
use crate::state_machine_configs::types::*;
use chrono::Utc;

impl PgDb {
    // ========================================================================
    // State Machine Configs
    // ========================================================================

    /// List all state machine configs, ordered by updated_at DESC.
    pub async fn list_sm_configs(&self) -> Result<Vec<SmConfig>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, render_count, element_count, include_html_ids,
                       created_at::TEXT, updated_at::TEXT
                FROM state_machine_configs
                ORDER BY updated_at DESC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG list_sm_configs: {}", e))?;

        Ok(rows.iter().map(|r| Self::sm_row_to_config(r)).collect())
    }

    /// Get a single state machine config by ID.
    pub async fn get_sm_config(&self, id: &str) -> Result<Option<SmConfig>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, name, description, render_count, element_count, include_html_ids,
                       created_at::TEXT, updated_at::TEXT
                FROM state_machine_configs
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_sm_config: {}", e))?;

        Ok(row.map(|r| Self::sm_row_to_config(&r)))
    }

    /// Insert a new state machine config. Returns the created config.
    pub async fn insert_sm_config(&self, req: &CreateSmConfigRequest) -> Result<SmConfig, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO state_machine_configs (id, name, description, render_count, element_count, include_html_ids, created_at, updated_at)
            VALUES ($1, $2, $3, 0, 0, FALSE, $4::TIMESTAMPTZ, $4::TIMESTAMPTZ)
            "#,
            &[&id, &req.name, &req.description, &now],
        )
        .await
        .map_err(|e| format!("PG insert_sm_config: {}", e))?;

        self.get_sm_config(&id)
            .await?
            .ok_or_else(|| "Config not found after insert".to_string())
    }

    /// Update an existing state machine config.
    pub async fn update_sm_config(
        &self,
        id: &str,
        req: &UpdateSmConfigRequest,
    ) -> Result<SmConfig, String> {
        let existing = self
            .get_sm_config(id)
            .await?
            .ok_or_else(|| format!("Config not found: {}", id))?;

        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();

        let name = req.name.as_deref().unwrap_or(&existing.name).to_string();
        let description = req.description.as_deref().or(existing.description.as_deref()).map(|s| s.to_string());
        let render_count = req.render_count.unwrap_or(existing.render_count) as i32;
        let element_count = req.element_count.unwrap_or(existing.element_count) as i32;
        let include_html_ids = req.include_html_ids.unwrap_or(existing.include_html_ids);

        conn.execute(
            r#"
            UPDATE state_machine_configs
            SET name = $1, description = $2, render_count = $3, element_count = $4,
                include_html_ids = $5, updated_at = $6::TIMESTAMPTZ
            WHERE id = $7
            "#,
            &[
                &name as &(dyn tokio_postgres::types::ToSql + Sync),
                &description as &(dyn tokio_postgres::types::ToSql + Sync),
                &render_count as &(dyn tokio_postgres::types::ToSql + Sync),
                &element_count as &(dyn tokio_postgres::types::ToSql + Sync),
                &include_html_ids as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_sm_config: {}", e))?;

        self.get_sm_config(id)
            .await?
            .ok_or_else(|| "Config not found after update".to_string())
    }

    /// Delete a state machine config by ID (cascades to states and transitions).
    pub async fn delete_sm_config(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute("DELETE FROM state_machine_configs WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_sm_config: {}", e))?;

        Ok(affected > 0)
    }

    // -- helpers --

    fn sm_row_to_config(row: &tokio_postgres::Row) -> SmConfig {
        SmConfig {
            id: row.get(0),
            name: row.get(1),
            description: row.get(2),
            render_count: row.get::<_, i32>(3) as i64,
            element_count: row.get::<_, i32>(4) as i64,
            include_html_ids: row.get(5),
            created_at: row.get(6),
            updated_at: row.get(7),
        }
    }
}
