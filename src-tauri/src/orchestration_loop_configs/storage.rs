//! SQLite CRUD operations for orchestration loop configs.

use uuid::Uuid;

use super::types::*;

/// List all saved configs, favorites first then by updated_at desc.
pub fn list_configs() -> Result<Vec<OlConfig>, String> {
    Err("SQLite removed".to_string())
}

/// Get a single config by ID.
pub fn get_config(id: &str) -> Result<Option<OlConfig>, String> {
    Err("SQLite removed".to_string())
}

/// Insert a new config. Returns the created config.
pub fn insert_config(req: &CreateOlConfigRequest) -> Result<OlConfig, String> {
    Err("SQLite removed".to_string())
}

/// Update an existing config. Returns the updated config.
pub fn update_config(
    id: &str,
    req: &UpdateOlConfigRequest,
) -> Result<OlConfig, String> {
    Err("SQLite removed".to_string())
}

/// Delete a config by ID. Returns true if deleted.
pub fn delete_config(id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}
