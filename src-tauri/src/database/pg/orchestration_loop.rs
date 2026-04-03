//! PostgreSQL orchestration_loop_configs CRUD operations (raw SQL).
//!
//! Mirrors the SQLite orchestration_loop_configs/storage.rs operations.

use super::PgDb;
use crate::orchestration_loop_configs::types::*;
use chrono::Utc;

impl PgDb {
    // ========================================================================
    // Orchestration Loop Configs
    // ========================================================================

    /// List all saved configs, favorites first then by updated_at desc.
    pub async fn list_ol_configs(&self) -> Result<Vec<OlConfig>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, is_favorite, config_json, created_at, updated_at
                FROM orchestration_loop_configs
                ORDER BY is_favorite DESC, updated_at DESC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG list_ol_configs: {}", e))?;

        Ok(rows.iter().map(|r| Self::ol_row_to_config(r)).collect())
    }

    /// Get a single config by ID.
    pub async fn get_ol_config(&self, id: &str) -> Result<Option<OlConfig>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, name, description, is_favorite, config_json, created_at, updated_at
                FROM orchestration_loop_configs
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_ol_config: {}", e))?;

        Ok(row.map(|r| Self::ol_row_to_config(&r)))
    }

    /// Insert a new orchestration loop config. Returns the created config.
    pub async fn insert_ol_config(&self, req: &CreateOlConfigRequest) -> Result<OlConfig, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let config_str = serde_json::to_string(&req.config_json)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO orchestration_loop_configs (id, name, description, is_favorite, config_json, created_at, updated_at)
            VALUES ($1, $2, $3, false, $4, $5, $6)
            "#,
            &[&id, &req.name, &req.description, &config_str, &now, &now],
        )
        .await
        .map_err(|e| format!("PG insert_ol_config: {}", e))?;

        self.get_ol_config(&id)
            .await?
            .ok_or_else(|| "Config not found after insert".to_string())
    }

    /// Update an existing orchestration loop config.
    pub async fn update_ol_config(
        &self,
        id: &str,
        req: &UpdateOlConfigRequest,
    ) -> Result<OlConfig, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();

        // Build dynamic SET clause
        let mut sets = vec!["updated_at = $1".to_string()];
        let mut param_values: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        param_values.push(Box::new(now));
        let mut param_idx = 2u32;

        if let Some(ref name) = req.name {
            sets.push(format!("name = ${}", param_idx));
            param_values.push(Box::new(name.clone()));
            param_idx += 1;
        }
        if let Some(ref desc) = req.description {
            sets.push(format!("description = ${}", param_idx));
            param_values.push(Box::new(desc.clone()));
            param_idx += 1;
        }
        if let Some(fav) = req.is_favorite {
            sets.push(format!("is_favorite = ${}", param_idx));
            param_values.push(Box::new(fav));
            param_idx += 1;
        }
        if let Some(ref config) = req.config_json {
            let s =
                serde_json::to_string(config).map_err(|e| format!("Failed to serialize: {}", e))?;
            sets.push(format!("config_json = ${}", param_idx));
            param_values.push(Box::new(s));
            param_idx += 1;
        }

        let id_owned = id.to_string();
        sets.push(String::new()); // placeholder — we use WHERE instead
                                  // Remove empty placeholder
        sets.pop();

        let sql = format!(
            "UPDATE orchestration_loop_configs SET {} WHERE id = ${}",
            sets.join(", "),
            param_idx
        );
        param_values.push(Box::new(id_owned));

        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = param_values
            .iter()
            .map(|v| v.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        conn.execute(&sql, &params)
            .await
            .map_err(|e| format!("PG update_ol_config: {}", e))?;

        self.get_ol_config(id)
            .await?
            .ok_or_else(|| format!("Config '{}' not found", id))
    }

    /// Delete a config by ID.
    pub async fn delete_ol_config(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                "DELETE FROM orchestration_loop_configs WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG delete_ol_config: {}", e))?;

        Ok(affected > 0)
    }

    // -- helpers --

    fn ol_row_to_config(row: &tokio_postgres::Row) -> OlConfig {
        let config_str: String = row.get(4);
        OlConfig {
            id: row.get(0),
            name: row.get(1),
            description: row.get(2),
            is_favorite: row.get(3),
            config_json: serde_json::from_str(&config_str).unwrap_or(serde_json::Value::Null),
            created_at: row.get(5),
            updated_at: row.get(6),
        }
    }
}
