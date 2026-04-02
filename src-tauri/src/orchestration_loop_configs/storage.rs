//! SQLite CRUD operations for orchestration loop configs.

use uuid::Uuid;

use super::types::*;
use crate::database::Connection;

/// List all saved configs, favorites first then by updated_at desc.
pub fn list_configs(conn: &Connection) -> Result<Vec<OlConfig>, String> {
    Err("SQLite removed".to_string())
}

/// Get a single config by ID.
pub fn get_config(conn: &Connection, id: &str) -> Result<Option<OlConfig>, String> {
    Err("SQLite removed".to_string())
}

/// Insert a new config. Returns the created config.
pub fn insert_config(conn: &Connection, req: &CreateOlConfigRequest) -> Result<OlConfig, String> {
    Err("SQLite removed".to_string())
}

/// Update an existing config. Returns the updated config.
pub fn update_config(
    conn: &Connection,
    id: &str,
    req: &UpdateOlConfigRequest,
) -> Result<OlConfig, String> {
    Err("SQLite removed".to_string())
}

/// Delete a config by ID. Returns true if deleted.
pub fn delete_config(conn: &Connection, id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}
