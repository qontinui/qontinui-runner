//! SQLite storage for trigger definitions and history.

use std::sync::Arc;
use tracing::info;


use super::types::{TriggerCondition, TriggerConfig, TriggerHistoryEntry, WorkflowTrigger};
use crate::database::CheckpointDb;

// ============================================================================
// Trigger CRUD
// ============================================================================

/// Get all triggers.
pub fn get_all_triggers(db: &Arc<CheckpointDb>) -> Result<Vec<WorkflowTrigger>, String> {
    Err("SQLite removed".to_string())
}

/// Get a single trigger by ID.
pub fn get_trigger(db: &Arc<CheckpointDb>, id: &str) -> Result<Option<WorkflowTrigger>, String> {
    Err("SQLite removed".to_string())
}

/// Get all enabled triggers.
pub fn get_enabled_triggers(db: &Arc<CheckpointDb>) -> Result<Vec<WorkflowTrigger>, String> {
    Err("SQLite removed".to_string())
}

/// Create a new trigger.
pub fn create_trigger(db: &Arc<CheckpointDb>, trigger: &WorkflowTrigger) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Update an existing trigger.
pub fn update_trigger(db: &Arc<CheckpointDb>, trigger: &WorkflowTrigger) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Delete a trigger.
pub fn delete_trigger(db: &Arc<CheckpointDb>, id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Set trigger enabled status.
pub fn set_trigger_enabled(db: &Arc<CheckpointDb>, id: &str, enabled: bool) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Record that a trigger was fired.
pub fn record_trigger_fired(
    db: &Arc<CheckpointDb>,
    id: &str,
    execution_id: Option<&str>,
) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

// ============================================================================
// Trigger History
// ============================================================================

/// Record a trigger history entry.
pub fn record_history(db: &Arc<CheckpointDb>, entry: &TriggerHistoryEntry) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Get trigger history entries.
pub fn get_trigger_history(
    db: &Arc<CheckpointDb>,
    trigger_id: &str,
    limit: u32,
) -> Result<Vec<TriggerHistoryEntry>, String> {
    Err("SQLite removed".to_string())
}

/// Get trigger history entries with optional filtering.
pub fn get_trigger_history_filtered(
    db: &Arc<CheckpointDb>,
    trigger_id: &str,
    limit: u32,
    action_filter: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<Vec<TriggerHistoryEntry>, String> {
    Err("SQLite removed".to_string())
}

/// Get trigger counts by type.
pub fn get_trigger_stats(db: &Arc<CheckpointDb>) -> Result<(u64, u64), String> {
    Err("SQLite removed".to_string())
}

// row_to_trigger removed (SQLite dead code)
