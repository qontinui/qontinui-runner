//! Generation pipeline instrumentation.
//!
//! Provides functions to emit pipeline events, create workflow versions,
//! and track step provenance during workflow generation. These functions
//! are called from hook points in generator.rs.

use crate::database::Connection;
use crate::database::graph_ops;
use serde_json::Value;
use tracing::{info, warn};

/// Record a generation pipeline event with timing and validation data.
/// Called at the end of each pipeline phase (discovery, builder, fixer, hardener, etc.).
/// Silently logs warnings on failure — pipeline events are telemetry, not critical path.
pub fn emit_pipeline_event(
    conn: &Connection,
    task_run_id: &str,
    workflow_id: Option<&str>,
    event_type: &str,
    phase: &str,
    iteration: Option<i32>,
    payload: Option<&Value>,
    duration_ms: Option<u64>,
    token_count: Option<u64>,
    errors_before: Option<i32>,
    errors_after: Option<i32>,
) {
    // SQLite removed - no-op
}

/// Create a new workflow version snapshot, computing diff from the previous version.
/// Returns the version ID on success, or None if versioning fails (non-fatal).
pub fn create_workflow_version(
    conn: &Connection,
    workflow_id: &str,
    generation_task_run_id: Option<&str>,
    workflow_json: &Value,
    trigger: &str,
) -> Option<String> {
    None
}

/// Record provenance for all steps in a workflow's phase arrays.
/// Iterates over setup_steps, verification_steps, agentic_steps, completion_steps
/// and creates a step_provenance entry for each.
/// Returns count of entries created.
pub fn record_all_step_provenance(
    conn: &Connection,
    workflow_id: &str,
    version_id: Option<&str>,
    workflow_json: &Value,
    agent: &str,
    iteration: Option<i32>,
) -> u32 {
    0
}

/// After a fixer iteration, diff old and new steps and record provenance for changed steps.
/// Compares steps by index within each phase. If a step changed, records with original_step_json.
pub fn record_fixer_provenance(
    conn: &Connection,
    workflow_id: &str,
    version_id: Option<&str>,
    old_workflow: &Value,
    new_workflow: &Value,
    iteration: i32,
) -> u32 {
    0
}

/// Record rule influence entries for all rules that were loaded during generation.
pub fn record_rule_influences(
    conn: &Connection,
    rules: &[(String, String, String)], // Vec of (rule_id, agent, section)
    task_run_id: &str,
    workflow_id: Option<&str>,
    phase: &str,
) {
    // SQLite removed - no-op
}

/// Build graph-informed context for the workflow generator.
/// Queries the knowledge graph for:
/// 1. Active cross-run patterns for this workflow (warnings about known issues)
/// 2. Ineffective rules to exclude from the prompt
/// 3. Phase stats showing which phases are bottlenecks
/// Returns a formatted string to include in the builder prompt.
pub fn build_graph_context(conn: &Connection, workflow_name: Option<&str>) -> String {
    String::new()
}

/// Compute a human-readable diff summary between two workflow JSON strings.
fn compute_diff_summary(old_json: &str, new_json: &str) -> Option<String> {
    let old: Value = serde_json::from_str(old_json).ok()?;
    let new: Value = serde_json::from_str(new_json).ok()?;

    let mut changes = Vec::new();

    let phases = [
        "setup_steps",
        "verification_steps",
        "agentic_steps",
        "completion_steps",
    ];
    for phase in &phases {
        let old_count = old
            .get(phase)
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let new_count = new
            .get(phase)
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if old_count != new_count {
            changes.push(format!("{}: {} -> {} steps", phase, old_count, new_count));
        }
    }

    // Check top-level field changes
    for key in ["max_iterations", "reflection_mode", "workflow_architecture"] {
        if old.get(key) != new.get(key) {
            changes.push(format!("{} changed", key));
        }
    }

    if changes.is_empty() {
        Some("No structural changes".to_string())
    } else {
        Some(changes.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
use crate::database::Connection;

    /// Create an in-memory SQLite database with all tables needed by instrumentation.
    fn setup_test_db() -> Connection {
        todo!("SQLite removed")
    }

    #[test]
    fn test_emit_pipeline_event() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_create_workflow_version_first() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_create_workflow_version_increments() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_record_all_step_provenance() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_record_fixer_provenance_only_changed() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_compute_diff_summary() {
        let old_json = serde_json::to_string(&json!({
            "setup_steps": [{"name": "s1"}],
            "verification_steps": [{"name": "v1"}, {"name": "v2"}],
            "agentic_steps": [],
            "completion_steps": [],
            "max_iterations": 10
        }))
        .unwrap();

        let new_json = serde_json::to_string(&json!({
            "setup_steps": [{"name": "s1"}, {"name": "s2"}],
            "verification_steps": [{"name": "v1"}, {"name": "v2"}],
            "agentic_steps": [],
            "completion_steps": [],
            "max_iterations": 15
        }))
        .unwrap();

        let summary = compute_diff_summary(&old_json, &new_json);
        assert!(summary.is_some());

        let s = summary.unwrap();
        // Should detect setup_steps count change (1 -> 2)
        assert!(
            s.contains("setup_steps: 1 -> 2 steps"),
            "Expected setup_steps change, got: {}",
            s
        );
        // Should detect max_iterations change
        assert!(
            s.contains("max_iterations changed"),
            "Expected max_iterations changed, got: {}",
            s
        );
        // verification_steps did NOT change count
        assert!(
            !s.contains("verification_steps"),
            "Should not mention verification_steps, got: {}",
            s
        );
    }
}
