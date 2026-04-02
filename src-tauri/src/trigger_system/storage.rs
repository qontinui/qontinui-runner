//! Storage for trigger definitions and history (stubs, pending PG port).

use std::sync::Arc;
use tracing::info;


use super::types::{TriggerCondition, TriggerConfig, TriggerHistoryEntry, WorkflowTrigger};

// ============================================================================
// Trigger CRUD
// ============================================================================

/// Get all triggers.
pub fn get_all_triggers() -> Result<Vec<WorkflowTrigger>, String> {
    Err("SQLite removed".to_string())
}

/// Get a single trigger by ID.
pub fn get_trigger(id: &str) -> Result<Option<WorkflowTrigger>, String> {
    Err("SQLite removed".to_string())
}

/// Get all enabled triggers.
pub fn get_enabled_triggers() -> Result<Vec<WorkflowTrigger>, String> {
    Err("SQLite removed".to_string())
}

/// Create a new trigger.
pub fn create_trigger(trigger: &WorkflowTrigger) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Update an existing trigger.
pub fn update_trigger(trigger: &WorkflowTrigger) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Delete a trigger.
pub fn delete_trigger(id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Set trigger enabled status.
pub fn set_trigger_enabled(id: &str, enabled: bool) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Record that a trigger was fired.
pub fn record_trigger_fired(
    id: &str,
    execution_id: Option<&str>,
) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

// ============================================================================
// Trigger History
// ============================================================================

/// Record a trigger history entry.
pub fn record_history(entry: &TriggerHistoryEntry) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Get trigger history entries.
pub fn get_trigger_history(
    trigger_id: &str,
    limit: u32,
) -> Result<Vec<TriggerHistoryEntry>, String> {
    Err("SQLite removed".to_string())
}

/// Get trigger history entries with optional filtering.
pub fn get_trigger_history_filtered(
    trigger_id: &str,
    limit: u32,
    action_filter: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<Vec<TriggerHistoryEntry>, String> {
    Err("SQLite removed".to_string())
}

/// Get trigger counts by type.
pub fn get_trigger_stats() -> Result<(u64, u64), String> {
    Err("SQLite removed".to_string())
}

// row_to_trigger removed (SQLite dead code)
