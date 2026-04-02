//! SQLite CRUD operations for state machine configs, states, and transitions.

use super::types::*;
use crate::database::Connection;

// =============================================================================
// Config CRUD
// =============================================================================

pub fn list_configs(conn: &Connection) -> Result<Vec<SmConfig>, String> {
    Err("SQLite removed".to_string())
}

pub fn get_config(conn: &Connection, id: &str) -> Result<Option<SmConfig>, String> {
    Err("SQLite removed".to_string())
}

pub fn insert_config(conn: &Connection, req: &CreateSmConfigRequest) -> Result<SmConfig, String> {
    Err("SQLite removed".to_string())
}

pub fn update_config(
    conn: &Connection,
    id: &str,
    req: &UpdateSmConfigRequest,
) -> Result<SmConfig, String> {
    Err("SQLite removed".to_string())
}

pub fn delete_config(conn: &Connection, id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Config Full (with states + transitions)
// =============================================================================

pub fn get_config_full(conn: &Connection, id: &str) -> Result<Option<SmConfigFull>, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// State CRUD
// =============================================================================

// row_to_state removed (SQLite dead code)

pub fn list_states(conn: &Connection, config_id: &str) -> Result<Vec<SmState>, String> {
    Err("SQLite removed".to_string())
}

pub fn get_state(conn: &Connection, id: &str) -> Result<Option<SmState>, String> {
    Err("SQLite removed".to_string())
}

pub fn insert_state(
    conn: &Connection,
    config_id: &str,
    req: &CreateSmStateRequest,
) -> Result<SmState, String> {
    Err("SQLite removed".to_string())
}

pub fn update_state(
    conn: &Connection,
    id: &str,
    req: &UpdateSmStateRequest,
) -> Result<SmState, String> {
    Err("SQLite removed".to_string())
}

pub fn delete_state(conn: &Connection, id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Transition CRUD
// =============================================================================

// row_to_transition removed (SQLite dead code)

pub fn list_transitions(conn: &Connection, config_id: &str) -> Result<Vec<SmTransition>, String> {
    Err("SQLite removed".to_string())
}

pub fn get_transition(conn: &Connection, id: &str) -> Result<Option<SmTransition>, String> {
    Err("SQLite removed".to_string())
}

pub fn insert_transition(
    conn: &Connection,
    config_id: &str,
    req: &CreateSmTransitionRequest,
) -> Result<SmTransition, String> {
    Err("SQLite removed".to_string())
}

pub fn update_transition(
    conn: &Connection,
    id: &str,
    req: &UpdateSmTransitionRequest,
) -> Result<SmTransition, String> {
    Err("SQLite removed".to_string())
}

pub fn delete_transition(conn: &Connection, id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Import
// =============================================================================

/// Import a state machine config from the export format.
///
/// The export format has `config`, `states`, and `transitions` as nested objects.
/// This creates a new config with all its states and transitions.
pub fn import_config(conn: &Connection, req: &SmImportRequest) -> Result<SmConfigFull, String> {
    Err("SQLite removed".to_string())
}
