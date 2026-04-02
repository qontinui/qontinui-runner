//! SQLite CRUD operations for state machine configs, states, and transitions.

use super::types::*;

// =============================================================================
// Config CRUD
// =============================================================================

pub fn list_configs() -> Result<Vec<SmConfig>, String> {
    Err("SQLite removed".to_string())
}

pub fn get_config(id: &str) -> Result<Option<SmConfig>, String> {
    Err("SQLite removed".to_string())
}

pub fn insert_config(req: &CreateSmConfigRequest) -> Result<SmConfig, String> {
    Err("SQLite removed".to_string())
}

pub fn update_config(
    id: &str,
    req: &UpdateSmConfigRequest,
) -> Result<SmConfig, String> {
    Err("SQLite removed".to_string())
}

pub fn delete_config(id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Config Full (with states + transitions)
// =============================================================================

pub fn get_config_full(id: &str) -> Result<Option<SmConfigFull>, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// State CRUD
// =============================================================================

// row_to_state removed (SQLite dead code)

pub fn list_states(config_id: &str) -> Result<Vec<SmState>, String> {
    Err("SQLite removed".to_string())
}

pub fn get_state(id: &str) -> Result<Option<SmState>, String> {
    Err("SQLite removed".to_string())
}

pub fn insert_state(
    config_id: &str,
    req: &CreateSmStateRequest,
) -> Result<SmState, String> {
    Err("SQLite removed".to_string())
}

pub fn update_state(
    id: &str,
    req: &UpdateSmStateRequest,
) -> Result<SmState, String> {
    Err("SQLite removed".to_string())
}

pub fn delete_state(id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Transition CRUD
// =============================================================================

// row_to_transition removed (SQLite dead code)

pub fn list_transitions(config_id: &str) -> Result<Vec<SmTransition>, String> {
    Err("SQLite removed".to_string())
}

pub fn get_transition(id: &str) -> Result<Option<SmTransition>, String> {
    Err("SQLite removed".to_string())
}

pub fn insert_transition(
    config_id: &str,
    req: &CreateSmTransitionRequest,
) -> Result<SmTransition, String> {
    Err("SQLite removed".to_string())
}

pub fn update_transition(
    id: &str,
    req: &UpdateSmTransitionRequest,
) -> Result<SmTransition, String> {
    Err("SQLite removed".to_string())
}

pub fn delete_transition(id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

// =============================================================================
// Import
// =============================================================================

/// Import a state machine config from the export format.
///
/// The export format has `config`, `states`, and `transitions` as nested objects.
/// This creates a new config with all its states and transitions.
pub fn import_config(req: &SmImportRequest) -> Result<SmConfigFull, String> {
    Err("SQLite removed".to_string())
}
