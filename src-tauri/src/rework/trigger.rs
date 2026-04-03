//! Targeted rework trigger for rejected deferred questions.
//!
//! When a user rejects a deferred question via the review endpoint,
//! this module rolls back to the git checkpoint and spawns a new
//! task run that incorporates the user's feedback.

use std::sync::Arc;
use tracing::{info, warn};

/// Trigger a rework run for a rejected deferred question.
///
/// This function:
/// 1. Looks up the original workflow from the task run
/// 2. Performs git rollback to the deferred question's checkpoint
/// 3. Creates a new task_run with the user's rejection feedback injected
/// 4. Returns the rework task run ID for the caller to spawn
pub async fn trigger_rework(
    app_state: Arc<crate::AppState>,
    _app_handle: tauri::AppHandle,
    source_task_run_id: &str,
    deferred_question: &serde_json::Value,
    reviewer_comment: Option<&str>,
) -> Result<String, String> {
    let rework_id = uuid::Uuid::new_v4().to_string();
    let question_id = deferred_question["id"].as_str().unwrap_or("unknown");
    let iteration = deferred_question["iteration"].as_i64().unwrap_or(0) as u32;
    let git_checkpoint = deferred_question["git_checkpoint"].as_str();
    let original_question = deferred_question["question"].as_str().unwrap_or("");
    let auto_decision = deferred_question["auto_decision_type"]
        .as_str()
        .unwrap_or("proceeded");

    let rework_name = format!("Rework: rejected decision at iter {}", iteration);

    // 1. Get the original task run to find workflow info
    let source_run = app_state
        .pg_db
        .get_task_run(source_task_run_id)
        .await
        .map_err(|e| format!("Failed to get source task run: {}", e))?
        .ok_or_else(|| format!("Source task run {} not found", source_task_run_id))?;

    let workflow_id = source_run
        .workflow_id
        .as_deref()
        .ok_or("Source task run has no workflow_id")?;
    let workflow_name = source_run.workflow_name.as_deref().unwrap_or("Unknown");
    let project_path = crate::mcp::shared::current_project_path();

    // 2. Git rollback if checkpoint available
    if let (Some(checkpoint), Some(ref proj_path)) = (git_checkpoint, &project_path) {
        if !checkpoint.is_empty() {
            info!("Rolling back to git checkpoint {} for rework", checkpoint);
            let output = crate::process_helpers::tokio_no_window("git")
                .args(["reset", "--hard", checkpoint])
                .current_dir(proj_path)
                .output()
                .await
                .map_err(|e| format!("Git rollback failed: {}", e))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("Git rollback warning: {}", stderr);
            }
        }
    }

    // 3. Build the rework prompt
    let feedback = reviewer_comment.unwrap_or("No specific feedback provided");
    let base_prompt = format!(
        "## Rework Task\n\n\
         A previous automated decision was rejected by the user during workflow '{}'.\n\n\
         ### Rejected Decision\n\
         **Iteration:** {}\n\
         **Question:** {}\n\
         **Auto-decision:** {}\n\n\
         ### User Feedback\n\
         {}\n\n\
         ### Instructions\n\
         1. The codebase has been rolled back to the state BEFORE the rejected decision.\n\
         2. Take a DIFFERENT approach than what was previously attempted.\n\
         3. Consider the user's feedback carefully — it explains why the previous approach was wrong.\n\
         4. Implement the fix and verify it works.\n\
         5. Do NOT repeat the same approach that was rejected.\n",
        workflow_name, iteration, original_question, auto_decision, feedback
    );

    // 4. Create the task run record
    let input = crate::database::CreateTaskRunInput::new(&rework_id, &rework_name)
        .with_task_type("rework")
        .with_workflow_type("unified")
        .with_parent_task_run_id(source_task_run_id)
        .with_workflow_id(workflow_id)
        .with_workflow_name(&rework_name)
        .with_prompt(&base_prompt)
        .with_max_sessions(5)
        .with_auto_continue(true);

    app_state
        .pg_db
        .create_task_run(&input)
        .await
        .map_err(|e| format!("Failed to create rework task run: {}", e))?;

    info!(
        "Spawning rework run {} for rejected question {} from {}",
        rework_id, question_id, source_task_run_id
    );

    Ok(rework_id)
}
