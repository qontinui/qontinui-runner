//! SQLite CRUD operations for orchestration loop configs.

use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use super::types::*;

/// List all saved configs, favorites first then by updated_at desc.
pub fn list_configs(conn: &Connection) -> Result<Vec<OlConfig>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, description, is_favorite, config_json, created_at, updated_at
             FROM orchestration_loop_configs
             ORDER BY is_favorite DESC, updated_at DESC",
        )
        .map_err(|e| format!("Failed to prepare list query: {}", e))?;

    let rows = stmt
        .query_map([], |row| {
            let config_str: String = row.get(4)?;
            Ok(OlConfig {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                is_favorite: row.get(3)?,
                config_json: serde_json::from_str(&config_str).unwrap_or(serde_json::Value::Null),
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })
        .map_err(|e| format!("Failed to query configs: {}", e))?;

    let mut configs = Vec::new();
    for row in rows {
        configs.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
    }
    Ok(configs)
}

/// Get a single config by ID.
pub fn get_config(conn: &Connection, id: &str) -> Result<Option<OlConfig>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, description, is_favorite, config_json, created_at, updated_at
             FROM orchestration_loop_configs WHERE id = ?1",
        )
        .map_err(|e| format!("Failed to prepare get query: {}", e))?;

    let result = stmt
        .query_row(params![id], |row| {
            let config_str: String = row.get(4)?;
            Ok(OlConfig {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                is_favorite: row.get(3)?,
                config_json: serde_json::from_str(&config_str).unwrap_or(serde_json::Value::Null),
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })
        .optional()
        .map_err(|e| format!("Failed to get config: {}", e))?;

    Ok(result)
}

/// Insert a new config. Returns the created config.
pub fn insert_config(conn: &Connection, req: &CreateOlConfigRequest) -> Result<OlConfig, String> {
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let config_str =
        serde_json::to_string(&req.config_json).map_err(|e| format!("Failed to serialize config: {}", e))?;

    conn.execute(
        "INSERT INTO orchestration_loop_configs (id, name, description, is_favorite, config_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)",
        params![id, req.name, req.description, config_str, now, now],
    )
    .map_err(|e| format!("Failed to insert config: {}", e))?;

    get_config(conn, &id)?.ok_or_else(|| "Config not found after insert".to_string())
}

/// Update an existing config. Returns the updated config.
pub fn update_config(
    conn: &Connection,
    id: &str,
    req: &UpdateOlConfigRequest,
) -> Result<OlConfig, String> {
    let now = chrono::Utc::now().to_rfc3339();

    // Build dynamic SET clause
    let mut sets = vec!["updated_at = ?1".to_string()];
    let mut param_idx = 2u32;

    if req.name.is_some() {
        sets.push(format!("name = ?{}", param_idx));
        param_idx += 1;
    }
    if req.description.is_some() {
        sets.push(format!("description = ?{}", param_idx));
        param_idx += 1;
    }
    if req.is_favorite.is_some() {
        sets.push(format!("is_favorite = ?{}", param_idx));
        param_idx += 1;
    }
    if req.config_json.is_some() {
        sets.push(format!("config_json = ?{}", param_idx));
        param_idx += 1;
    }

    let sql = format!(
        "UPDATE orchestration_loop_configs SET {} WHERE id = ?{}",
        sets.join(", "),
        param_idx
    );

    // Build params dynamically
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(now));
    if let Some(ref name) = req.name {
        param_values.push(Box::new(name.clone()));
    }
    if let Some(ref desc) = req.description {
        param_values.push(Box::new(desc.clone()));
    }
    if let Some(fav) = req.is_favorite {
        param_values.push(Box::new(fav));
    }
    if let Some(ref config) = req.config_json {
        let s = serde_json::to_string(config).map_err(|e| format!("Failed to serialize: {}", e))?;
        param_values.push(Box::new(s));
    }
    param_values.push(Box::new(id.to_string()));

    let params_ref: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
    conn.execute(&sql, params_ref.as_slice())
        .map_err(|e| format!("Failed to update config: {}", e))?;

    get_config(conn, id)?.ok_or_else(|| format!("Config '{}' not found", id))
}

/// Delete a config by ID. Returns true if deleted.
pub fn delete_config(conn: &Connection, id: &str) -> Result<bool, String> {
    let count = conn
        .execute(
            "DELETE FROM orchestration_loop_configs WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to delete config: {}", e))?;
    Ok(count > 0)
}
