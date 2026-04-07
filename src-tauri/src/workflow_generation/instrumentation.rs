//! Generation pipeline instrumentation.
//!
//! Provides functions to emit pipeline events, create workflow versions,
//! and track step provenance during workflow generation. These functions
//! are called from hook points in generator.rs.

use crate::database::graph_ops;
use serde_json::Value;
use tracing::{info, warn};

/// Record a generation pipeline event with timing and validation data.
/// Called at the end of each pipeline phase (discovery, builder, fixer, hardener, etc.).
/// Silently logs warnings on failure — pipeline events are telemetry, not critical path.
pub fn emit_pipeline_event(
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
///
/// Returns a formatted string to include in the builder prompt.
pub fn build_graph_context(workflow_name: Option<&str>) -> String {
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

