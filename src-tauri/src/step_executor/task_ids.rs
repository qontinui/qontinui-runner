//! Task-run id helpers shared by step execution, step events and checkpoints.

/// Extract the parent task ID from a composed run child ID.
///
/// For composed runs, individual workflows have IDs like:
/// `composed-run-{timestamp}-workflow-{n}`
///
/// But only the parent `composed-run-{timestamp}` exists in task_runs.
/// This function extracts the parent ID for use in database operations that
/// reference task_runs (step checkpoints, task_run_events, etc.).
///
/// For non-composed IDs (e.g., `unified-workflow-{id}-{timestamp}`), returns
/// the original ID unchanged.
///
/// # Examples
///
/// ```ignore
/// use qontinui_runner::step_executor::get_parent_task_id;
///
/// // Composed run child → parent
/// assert_eq!(
///     get_parent_task_id("composed-run-1234567890-workflow-1"),
///     "composed-run-1234567890"
/// );
///
/// // Non-composed → unchanged
/// assert_eq!(
///     get_parent_task_id("unified-workflow-abc-1234567890"),
///     "unified-workflow-abc-1234567890"
/// );
/// ```
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
pub fn get_parent_task_id(execution_id: &str) -> String {
    // Check if this is a composed run child: composed-run-{timestamp}-workflow-{n}
    if let Some(pos) = execution_id.rfind("-workflow-") {
        let potential_parent = &execution_id[..pos];
        // Verify it looks like a composed run parent (starts with composed-run-)
        if potential_parent.starts_with("composed-run-") {
            return potential_parent.to_string();
        }
    }
    execution_id.to_string()
}

/// Check if an execution ID is a composed run child.
///
/// Returns true for IDs like `composed-run-{timestamp}-workflow-{n}`.
pub fn is_sequence_child(execution_id: &str) -> bool {
    get_parent_task_id(execution_id) != execution_id
}

/// Extract workflow ID from task_run.id (format: unified-workflow-{uuid}-{timestamp})
pub fn extract_workflow_id_from_task_id(task_id: &str) -> Option<String> {
    // Format: unified-workflow-{uuid part 1}-{uuid part 2}-{uuid part 3}-{uuid part 4}-{uuid part 5}-{timestamp}
    // Example: unified-workflow-12345678-1234-5678-1234-567812345678-20250101120000
    if !task_id.starts_with("unified-workflow-") {
        return None;
    }

    let parts: Vec<&str> = task_id.split('-').collect();
    // "unified" = 0, "workflow" = 1, then 5 UUID parts, then timestamp
    if parts.len() >= 7 {
        // UUID is parts 2-6 (indices 2,3,4,5,6)
        let uuid = format!(
            "{}-{}-{}-{}-{}",
            parts[2], parts[3], parts[4], parts[5], parts[6]
        );
        Some(uuid)
    } else {
        None
    }
}
