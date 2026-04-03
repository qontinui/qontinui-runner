//! Storage operations for error monitoring.
//!
//! Provides database access for log sources and error events.

use super::types::*;
use std::collections::HashMap;

/// Storage operations for log sources
pub struct LogSourceStorage;

impl LogSourceStorage {
    /// Insert a new log source configuration
    pub fn insert(config: &LogSourceConfig) -> Result<i64, String> {
        Err("SQLite removed".to_string())
    }

    /// Update an existing log source configuration
    pub fn update(id: i64, config: &LogSourceConfig) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// Delete a log source configuration
    pub fn delete(id: i64) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// Get a log source by ID
    pub fn get_by_id(id: i64) -> Result<Option<LogSourceConfig>, String> {
        Err("SQLite removed".to_string())
    }

    /// Get all log sources
    pub fn get_all() -> Result<Vec<LogSourceConfig>, String> {
        Err("SQLite removed".to_string())
    }

    /// List log sources with optional enabled filter
    pub fn list(enabled_only: bool) -> Result<Vec<LogSourceConfig>, String> {
        Err("SQLite removed".to_string())
    }

    /// Delete a log source by name
    pub fn delete_by_name(name: &str) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// Get enabled log sources
    pub fn get_enabled() -> Result<Vec<LogSourceConfig>, String> {
        Err("SQLite removed".to_string())
    }
}

/// Storage operations for error events
pub struct ErrorEventStorage;

impl ErrorEventStorage {
    /// Insert a new error event and return the stored event
    pub fn insert(
        event: &ErrorEvent,
        task_run_id: Option<&str>,
        workflow_name: Option<&str>,
    ) -> Result<StoredErrorEvent, String> {
        Err("SQLite removed".to_string())
    }

    /// Insert a new error event or increment occurrence count if duplicate
    pub fn insert_or_increment(
        event: &ErrorEvent,
        log_source_id: Option<i64>,
        task_run_id: Option<&str>,
        workflow_step_id: Option<&str>,
    ) -> Result<i64, String> {
        Err("SQLite removed".to_string())
    }

    /// Get an error event by ID
    pub fn get_by_id(id: i64) -> Result<Option<StoredErrorEvent>, String> {
        Err("SQLite removed".to_string())
    }

    /// Query error events with filters
    pub fn query(query: &ErrorQuery) -> Result<Vec<StoredErrorEvent>, String> {
        Err("SQLite removed".to_string())
    }

    /// Get unresolved errors (for debug agent context)
    pub fn get_unresolved(
        task_run_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredErrorEvent>, String> {
        Err("SQLite removed".to_string())
    }

    /// Update error status
    pub fn update_status(
        id: i64,
        status: ErrorStatus,
        resolution_notes: Option<&str>,
    ) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// Link an error event to a finding
    pub fn link_to_finding(error_id: i64, finding_id: i64) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// Mark an error as resolved by a task run
    pub fn mark_resolved_by_task(
        error_id: i64,
        task_run_id: &str,
        resolution_notes: Option<&str>,
    ) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// Auto-resolve any unresolved SPEC verification error events.
    ///
    /// SPEC events (action_failed with "SPEC: " prefix) are verification test results,
    /// not application errors. The JSONL preprocessor now filters them out, but older
    /// events may still exist in the database from before the filter was added.
    /// This cleans them up on startup.
    pub fn auto_resolve_spec_events() -> Result<usize, String> {
        Err("SQLite removed".to_string())
    }

    /// Bulk-resolve all unresolved errors scoped to a specific task run.
    ///
    /// This is called when a workflow completes successfully to clean up errors
    /// that were captured during its execution. Errors with status 'promoted'
    /// (linked to findings) are excluded — they're resolved through that system.
    ///
    /// Returns the count of resolved errors.
    pub fn resolve_errors_by_task_run(
        task_run_id: &str,
        resolved_by_task_run_id: &str,
    ) -> Result<usize, String> {
        Err("SQLite removed".to_string())
    }

    /// Get error summary statistics
    pub fn get_summary(task_run_id: Option<&str>) -> Result<ErrorSummary, String> {
        Err("SQLite removed".to_string())
    }

    /// Get count breakdown by a column
    fn get_count_by_column(
        column: &str,
        task_run_id: Option<&str>,
    ) -> Result<HashMap<String, u32>, String> {
        Err("SQLite removed".to_string())
    }

    /// Search errors by message content (FTS)
    pub fn search(query: &str, limit: usize) -> Result<Vec<StoredErrorEvent>, String> {
        Err("SQLite removed".to_string())
    }

    // row_to_stored_event removed (SQLite dead code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal ErrorEvent for testing.
    fn make_error_event(source: &str, message: &str) -> ErrorEvent {
        ErrorEvent {
            log_source_name: source.to_string(),
            severity: ErrorSeverity::Error,
            error_type: Some("TestError".to_string()),
            error_code: None,
            message: message.to_string(),
            stack_trace: None,
            location: None,
            context_lines: None,
            raw_entry: message.to_string(),
            log_timestamp: None,
            trace_id: None,
        }
    }

    // Test helpers removed (SQLite dead code)

    // ---------------------------------------------------------------
    // insert_or_increment: recurrence detection tests
    // ---------------------------------------------------------------

    #[test]
    fn test_new_error_with_no_history_is_new() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_recurring_after_resolved_predecessor() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_ignored_predecessor_does_not_trigger_recurring() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_duplicate_unresolved_increments_count() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_wont_fix_predecessor_triggers_recurring() {
        // SQLite removed - no-op
    }
}
