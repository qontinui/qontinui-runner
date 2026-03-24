//! Generation pipeline instrumentation.
//!
//! Provides functions to emit pipeline events, create workflow versions,
//! and track step provenance during workflow generation. These functions
//! are called from hook points in generator.rs.

use crate::database::graph_ops;
use rusqlite::Connection;
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
    let payload_str = payload.map(|v| serde_json::to_string(v).unwrap_or_default());
    match graph_ops::insert_pipeline_event(
        conn,
        task_run_id,
        workflow_id,
        event_type,
        Some(phase),
        iteration,
        payload_str.as_deref(),
        duration_ms.map(|d| d as i64),
        token_count.map(|t| t as i64),
        errors_before,
        errors_after,
    ) {
        Ok(id) => info!("Pipeline event recorded: {} phase={} id={}", event_type, phase, id),
        Err(e) => warn!("Failed to record pipeline event: {}", e),
    }
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
    // 1. Get the latest existing version for this workflow
    let latest = graph_ops::get_latest_workflow_version(conn, workflow_id)
        .ok()
        .flatten();

    // 2. Compute version number (latest + 1 or 1)
    let version_number = latest
        .as_ref()
        .map(|v| v.version_number + 1)
        .unwrap_or(1);
    let parent_version_id = latest.as_ref().map(|v| v.id.as_str());

    // 3. Compute diff summary if there's a parent version
    let diff_summary = if let Some(ref parent) = latest {
        compute_diff_summary(
            &parent.workflow_json,
            &serde_json::to_string(workflow_json).unwrap_or_default(),
        )
    } else {
        Some("Initial version".to_string())
    };

    // 4. Serialize workflow JSON
    let workflow_json_str = serde_json::to_string(workflow_json).unwrap_or_default();

    // 5. Insert
    match graph_ops::insert_workflow_version(
        conn,
        workflow_id,
        version_number,
        parent_version_id,
        generation_task_run_id,
        &workflow_json_str,
        diff_summary.as_deref(),
        None, // diff_json computed on demand
        trigger,
    ) {
        Ok(id) => {
            info!(
                "Created workflow version {} (v{}) for {}",
                id, version_number, workflow_id
            );
            Some(id)
        }
        Err(e) => {
            warn!("Failed to create workflow version: {}", e);
            None
        }
    }
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
    let mut count = 0u32;
    let phases = [
        "setup_steps",
        "verification_steps",
        "agentic_steps",
        "completion_steps",
    ];
    let phase_names = ["setup", "verification", "agentic", "completion"];

    for (phase_key, phase_name) in phases.iter().zip(phase_names.iter()) {
        if let Some(steps) = workflow_json.get(phase_key).and_then(|v| v.as_array()) {
            for (i, step) in steps.iter().enumerate() {
                let step_name = step
                    .get("name")
                    .or_else(|| step.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unnamed");

                let step_json_str = serde_json::to_string(step).unwrap_or_default();

                if let Err(e) = graph_ops::insert_step_provenance(
                    conn,
                    workflow_id,
                    version_id,
                    step_name,
                    i as i32,
                    phase_name,
                    agent,
                    iteration,
                    None, // original_step_json — only set for fixer modifications
                    Some(&step_json_str),
                ) {
                    warn!("Failed to record step provenance for {}: {}", step_name, e);
                } else {
                    count += 1;
                }
            }
        }
    }

    if count > 0 {
        info!(
            "Recorded {} step provenance entries for workflow {} agent={}",
            count, workflow_id, agent
        );
    }
    count
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
    let mut count = 0u32;
    let phases = [
        "setup_steps",
        "verification_steps",
        "agentic_steps",
        "completion_steps",
    ];
    let phase_names = ["setup", "verification", "agentic", "completion"];

    for (phase_key, phase_name) in phases.iter().zip(phase_names.iter()) {
        let old_steps = old_workflow.get(phase_key).and_then(|v| v.as_array());
        let new_steps = new_workflow.get(phase_key).and_then(|v| v.as_array());

        if let (Some(old), Some(new)) = (old_steps, new_steps) {
            for (i, new_step) in new.iter().enumerate() {
                let old_step = old.get(i);
                let changed = old_step.map(|o| o != new_step).unwrap_or(true);

                if changed {
                    let step_name = new_step
                        .get("name")
                        .or_else(|| new_step.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unnamed");

                    let old_json =
                        old_step.map(|s| serde_json::to_string(s).unwrap_or_default());
                    let new_json = serde_json::to_string(new_step).unwrap_or_default();

                    if let Err(e) = graph_ops::insert_step_provenance(
                        conn,
                        workflow_id,
                        version_id,
                        step_name,
                        i as i32,
                        phase_name,
                        "fixer",
                        Some(iteration),
                        old_json.as_deref(),
                        Some(&new_json),
                    ) {
                        warn!(
                            "Failed to record fixer provenance for step {}: {}",
                            step_name, e
                        );
                    } else {
                        count += 1;
                    }
                }
            }
        }
    }

    if count > 0 {
        info!(
            "Fixer iteration {} changed {} steps in workflow {}",
            iteration, count, workflow_id
        );
    }
    count
}

/// Record rule influence entries for all rules that were loaded during generation.
pub fn record_rule_influences(
    conn: &Connection,
    rules: &[(String, String, String)], // Vec of (rule_id, agent, section)
    task_run_id: &str,
    workflow_id: Option<&str>,
    phase: &str,
) {
    for (rule_id, _agent, _section) in rules {
        if let Err(e) = graph_ops::insert_rule_influence(
            conn,
            rule_id,
            task_run_id,
            workflow_id,
            "loaded",
            None,
            Some(phase),
        ) {
            warn!("Failed to record rule influence for {}: {}", rule_id, e);
        }
    }
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
