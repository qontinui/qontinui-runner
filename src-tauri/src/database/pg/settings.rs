//! PostgreSQL settings and config storage operations via Clorinde-generated queries.
//!
//! PG-primary with SQLite fallback for key-value settings and config storage.
//! JSON file migration helpers remain SQLite-only (one-time startup operation).

use super::PgDb;
use crate::database::types::ConfigStorageEntry;

/// Convert Clorinde empty-string (from NULL) to Option::None.
fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

impl PgDb {
    // ========================================================================
    // Settings (key-value store)
    // ========================================================================

    /// Get a setting value by key.
    pub async fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let value_str = qontinui_db::queries::settings::get_setting()
            .bind(&conn, &key)
            .opt()
            .await
            .map_err(|e| format!("PG get_setting: {}", e))?;

        match value_str {
            Some(v) => {
                let parsed = serde_json::from_str(&v)
                    .map_err(|e| format!("Failed to parse setting value: {}", e))?;
                Ok(Some(parsed))
            }
            None => Ok(None),
        }
    }

    /// Set a setting value (upsert).
    pub async fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let value_str = value.to_string();
        qontinui_db::queries::settings::set_setting()
            .bind(&conn, &key, &value_str.as_str())
            .one()
            .await
            .map_err(|e| format!("PG set_setting: {}", e))?;
        Ok(())
    }

    /// Delete a setting by key. Returns true if a row was deleted.
    pub async fn delete_setting(&self, key: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let deleted = qontinui_db::queries::settings::delete_setting()
            .bind(&conn, &key)
            .opt()
            .await
            .map_err(|e| format!("PG delete_setting: {}", e))?;
        Ok(deleted.is_some())
    }

    /// Get all settings as a JSON object.
    pub async fn get_all_settings(&self) -> Result<serde_json::Value, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::settings::get_all_settings()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG get_all_settings: {}", e))?;

        let mut settings = serde_json::Map::new();
        for row in rows {
            if let Ok(value) = serde_json::from_str(&row.value) {
                settings.insert(row.key, value);
            }
        }
        Ok(serde_json::Value::Object(settings))
    }

    // ========================================================================
    // Config Storage
    // ========================================================================

    /// Save a config with a specific ID (upsert).
    pub async fn save_config_with_id(
        &self,
        id: &str,
        config: serde_json::Value,
        name: &str,
        source_type: &str,
        source_path: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let config_json = config.to_string();
        let source_path_opt = source_path.map(|s| s.to_string());
        qontinui_db::queries::settings::save_config()
            .bind(
                &conn,
                &id,
                &name,
                &config_json.as_str(),
                &source_type,
                &source_path_opt,
            )
            .one()
            .await
            .map_err(|e| format!("PG save_config: {}", e))?;
        Ok(())
    }

    /// Get a config by ID.
    pub async fn get_config(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let json_str = qontinui_db::queries::settings::get_config()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_config: {}", e))?;

        match json_str {
            Some(s) => {
                let config = serde_json::from_str(&s)
                    .map_err(|e| format!("Failed to parse config JSON: {}", e))?;
                Ok(Some(config))
            }
            None => Ok(None),
        }
    }

    /// List all stored configs.
    pub async fn list_configs(&self) -> Result<Vec<ConfigStorageEntry>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::settings::list_configs()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG list_configs: {}", e))?;

        Ok(rows.into_iter().map(|r| ConfigStorageEntry {
            id: r.id,
            name: r.name,
            source_type: r.source_type,
            source_path: non_empty(r.source_path),
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }).collect())
    }

    /// Delete a config by ID. Returns true if a row was deleted.
    pub async fn delete_config(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let deleted = qontinui_db::queries::settings::delete_config()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG delete_config: {}", e))?;
        Ok(deleted.is_some())
    }
}
