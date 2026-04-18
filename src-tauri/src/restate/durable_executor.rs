//! Durable workflow execution engine.
//!
//! The `DurableLoopController` is the Restate-backed equivalent of `LoopController`.
//! It orchestrates the same 4-phase workflow (Setup → Verification → Agentic → Completion)
//! but wraps each step in `ctx.run()` for journal-based replay and exactly-once semantics.
//!
//! On crash and restart, Restate replays the journal: completed steps return their
//! cached results instantly, and execution resumes at the first un-journaled step.

use std::sync::Arc;

use tracing::{debug, error, info, warn};

use crate::config_storage::ConfigStorage;
use crate::database::pg::PgDb;
use crate::step_executor::{ExecutionStepConfig, StepExecutor};
use crate::AppState;

use super::compensation::{CompensationAction, CompensationType};
use super::types::{DurableStepResult, WorkflowInput, WorkflowOutput};

/// Re-export the shared PhaseResult so Restate consumers keep using
/// `super::durable_executor::PhaseResult`.
pub use crate::unified_workflow_executor::types::PhaseResult;

/// Executes a batch of steps, returning a serializable `PhaseResult`.
///
/// This function is designed to be called inside a Restate `ctx.run()` side effect.
/// It creates a fresh `StepExecutor`, runs all steps sequentially, and captures
/// the results in a serializable form that Restate can journal.
///
/// On replay, Restate returns the cached `PhaseResult` without re-executing.
pub async fn execute_steps_batch(
    app_state: &Arc<AppState>,
    config_storage: &Arc<tokio::sync::Mutex<ConfigStorage>>,
    steps_json: &str,
    phase_name: &str,
    iteration: Option<u32>,
    execution_id: &str,
    base_prompt: Option<&str>,
) -> PhaseResult {
    let start = std::time::Instant::now();
    let mut step_results = Vec::new();
    let mut all_passed = true;
    let mut failure_context = None;
    let mut variables_set = Vec::new();

    // Deserialize steps
    let steps: Vec<ExecutionStepConfig> = match serde_json::from_str(steps_json) {
        Ok(s) => s,
        Err(e) => {
            error!(
                "Failed to deserialize {} steps for {}: {}",
                phase_name, execution_id, e
            );
            return PhaseResult {
                phase: phase_name.to_string(),
                iteration,
                stage_index: None,
                success: false,
                all_passed: false,
                step_results: vec![],
                failure_context: Some(format!("Step deserialization failed: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
                variables_set: Some(vec![]),
                commit_hash: None,
            };
        }
    };

    if steps.is_empty() {
        debug!("No {} steps to execute for {}", phase_name, execution_id);
        return PhaseResult {
            phase: phase_name.to_string(),
            iteration,
            stage_index: None,
            success: true,
            all_passed: true,
            step_results: vec![],
            failure_context: None,
            duration_ms: 0,
            variables_set: Some(vec![]),
            commit_hash: None,
        };
    }

    // Create a step executor
    let mut executor = StepExecutor::new(app_state.clone(), config_storage.clone());
    executor.set_task_run_id(execution_id.to_string());

    info!(
        "Executing {} {} steps for {} (iteration {:?})",
        steps.len(),
        phase_name,
        execution_id,
        iteration
    );

    for (i, step) in steps.iter().enumerate() {
        let step_start = std::time::Instant::now();
        let step_name = step
            .name
            .clone()
            .or_else(|| step.check_type.clone())
            .unwrap_or_else(|| format!("{}-step-{}", phase_name, i));

        debug!(
            "Executing step {}/{}: {} (type: {:?})",
            i + 1,
            steps.len(),
            step_name,
            step.step_type
        );

        // Execute the step
        let (success, error_msg, _screenshot, output_data) =
            executor.execute_single_step(step).await;

        let step_duration = step_start.elapsed().as_millis() as u64;

        // Capture variables set by this step
        let mut step_vars = Vec::new();
        if let Some(ref data) = output_data {
            if let Some(obj) = data.as_object() {
                for (k, v) in obj {
                    if let Some(s) = v.as_str() {
                        step_vars.push((k.clone(), s.to_string()));
                        variables_set.push((k.clone(), s.to_string()));
                    }
                }
            }
        }

        let result = DurableStepResult {
            success,
            step_index: i,
            step_type: step.step_type.clone(),
            step_name: Some(step_name.clone()),
            error: error_msg.clone(),
            output_data,
            duration_ms: step_duration,
            variables_set: Some(step_vars),
        };

        if !success {
            all_passed = false;
            if failure_context.is_none() {
                failure_context = Some(format!(
                    "Step '{}' failed: {}",
                    step_name,
                    error_msg.as_deref().unwrap_or("unknown error")
                ));
            }
        }

        step_results.push(result);

        // For verification phases, continue even if a step fails
        // For other phases, stop on first failure
        if !success && phase_name != "verification" {
            warn!("Stopping {} phase at step {} due to failure", phase_name, i);
            break;
        }
    }

    // Get current git commit hash for compensation tracking
    let commit_hash = get_current_commit_hash().await;

    let duration_ms = start.elapsed().as_millis() as u64;
    info!(
        "{} phase complete for {}: all_passed={}, {}/{} steps, {}ms",
        phase_name,
        execution_id,
        all_passed,
        step_results.iter().filter(|r| r.success).count(),
        step_results.len(),
        duration_ms
    );

    PhaseResult {
        phase: phase_name.to_string(),
        iteration,
        stage_index: None,
        success: all_passed || phase_name == "verification",
        all_passed,
        step_results,
        failure_context,
        duration_ms,
        variables_set: Some(variables_set),
        commit_hash,
    }
}

/// Legacy monolithic workflow executor (retained for fallback/testing).
///
/// Prefer the phase-level journaled execution in `service.rs::run()` which
/// wraps each phase in a separate `ctx.run()` for crash recovery.
#[allow(dead_code)]
async fn execute_workflow_monolithic(
    app_state: &Arc<AppState>,
    config_storage: &Arc<tokio::sync::Mutex<ConfigStorage>>,
    input: &WorkflowInput,
    execution_id: &str,
    pg_db: &Arc<PgDb>,
) -> WorkflowOutput {
    let start = std::time::Instant::now();
    let mut iterations_run = 0u32;
    let mut verification_passed = false;
    let mut critical_failure = false;
    let mut was_stopped = false;
    let mut files_modified = Vec::new();
    let mut error_msg = None;

    // Deserialize loop config
    let loop_config: serde_json::Value = match serde_json::from_str(&input.loop_config_json) {
        Ok(c) => c,
        Err(e) => {
            return WorkflowOutput {
                success: false,
                verification_passed: false,
                iterations_run: 0,
                critical_failure: true,
                was_stopped: false,
                duration_ms: 0,
                files_modified: vec![],
                error: Some(format!("Failed to deserialize loop config: {}", e)),
            };
        }
    };

    let max_iterations = loop_config
        .get("max_iterations")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as u32;

    let workflow_name = loop_config
        .get("workflow_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    info!(
        "Starting durable workflow execution: {} (id={}, max_iterations={})",
        workflow_name, execution_id, max_iterations
    );

    // Update task run status
    if let Err(e) = pg_db.update_task_run_status(execution_id, "running").await {
        warn!("Failed to update task run status: {}", e);
    }

    // =========================================================================
    // PHASE 1: SETUP
    // =========================================================================
    info!("Phase 1/4: Setup — execution_id={}", execution_id);

    // Setup automation steps
    let setup_result = execute_steps_batch(
        app_state,
        config_storage,
        &input.setup_automation_steps_json,
        "setup",
        None,
        execution_id,
        None,
    )
    .await;

    if !setup_result.success {
        error!("Setup phase failed: {:?}", setup_result.failure_context);
        return WorkflowOutput {
            success: false,
            verification_passed: false,
            iterations_run: 0,
            critical_failure: true,
            was_stopped: false,
            duration_ms: start.elapsed().as_millis() as u64,
            files_modified: vec![],
            error: setup_result.failure_context,
        };
    }

    // Setup prompt steps (AI prompts that run after automation setup)
    let setup_prompt_result = execute_steps_batch(
        app_state,
        config_storage,
        &input.setup_prompt_steps_json,
        "setup_prompt",
        None,
        execution_id,
        None,
    )
    .await;

    if !setup_prompt_result.success {
        error!(
            "Setup prompt phase failed: {:?}",
            setup_prompt_result.failure_context
        );
        return WorkflowOutput {
            success: false,
            verification_passed: false,
            iterations_run: 0,
            critical_failure: true,
            was_stopped: false,
            duration_ms: start.elapsed().as_millis() as u64,
            files_modified: vec![],
            error: setup_prompt_result.failure_context,
        };
    }

    // =========================================================================
    // PHASE 2-3: VERIFICATION-AGENTIC LOOP
    // =========================================================================
    let mut iteration = 1u32;

    while iteration <= max_iterations {
        // Check for stop request
        if is_stop_requested(app_state, execution_id).await {
            info!("Stop requested at iteration {}", iteration);
            was_stopped = true;
            break;
        }

        // --- Verification ---
        info!(
            "Phase 2/4: Verification — iteration {}/{}, execution_id={}",
            iteration, max_iterations, execution_id
        );

        let v_result = execute_steps_batch(
            app_state,
            config_storage,
            &input.verification_steps_json,
            "verification",
            Some(iteration),
            execution_id,
            None,
        )
        .await;

        iterations_run = iteration;

        if v_result.all_passed {
            info!(
                "Verification PASSED at iteration {} — proceeding to completion",
                iteration
            );
            verification_passed = true;
            break;
        }

        // Build failure context for the agentic phase
        let failure_context = v_result
            .failure_context
            .unwrap_or_else(|| "Verification failed".to_string());

        // --- Agentic ---
        if iteration < max_iterations {
            info!(
                "Phase 3/4: Agentic — iteration {}/{}, execution_id={}",
                iteration, max_iterations, execution_id
            );

            let a_result = execute_steps_batch(
                app_state,
                config_storage,
                &input.agentic_steps_json,
                "agentic",
                Some(iteration),
                execution_id,
                Some(&failure_context),
            )
            .await;

            if !a_result.success {
                warn!(
                    "Agentic phase failed at iteration {}: {:?}",
                    iteration, a_result.failure_context
                );
                // Continue to next iteration — the agentic phase failing
                // doesn't mean the problem can't be fixed differently
            }
        }

        iteration += 1;
    }

    // =========================================================================
    // PHASE 4: COMPLETION
    // =========================================================================
    if verification_passed {
        info!("Phase 4/4: Completion — execution_id={}", execution_id);

        // Completion automation steps
        let c_result = execute_steps_batch(
            app_state,
            config_storage,
            &input.completion_automation_steps_json,
            "completion",
            None,
            execution_id,
            None,
        )
        .await;

        if !c_result.success {
            warn!(
                "Completion automation phase had failures: {:?}",
                c_result.failure_context
            );
        }

        // Completion prompt steps (AI prompts after automation completion)
        let cp_result = execute_steps_batch(
            app_state,
            config_storage,
            &input.completion_prompt_steps_json,
            "completion_prompt",
            None,
            execution_id,
            None,
        )
        .await;

        if !cp_result.success {
            warn!(
                "Completion prompt phase had failures: {:?}",
                cp_result.failure_context
            );
        }
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    let success = verification_passed && !critical_failure && !was_stopped;

    // Update task run status
    let final_status = if success {
        "complete"
    } else if was_stopped {
        "stopped"
    } else {
        "failed"
    };

    if let Err(e) = pg_db
        .update_task_run_status(execution_id, final_status)
        .await
    {
        warn!("Failed to update final task run status: {}", e);
    }

    info!(
        "Durable workflow {} complete: success={}, verification_passed={}, iterations={}, {}ms",
        workflow_name, success, verification_passed, iterations_run, duration_ms
    );

    WorkflowOutput {
        success,
        verification_passed,
        iterations_run,
        critical_failure,
        was_stopped,
        duration_ms,
        files_modified,
        error: error_msg,
    }
}

/// Check if a stop has been requested for the current workflow.
///
/// The stop mechanism uses the existing execution control flow:
/// the frontend calls `stop_execution` which signals the executor.
/// For Restate workflows, we check if the execution status was updated to "stopped".
async fn is_stop_requested(app_state: &AppState, execution_id: &str) -> bool {
    // Query PG for the current task run status. The frontend sets
    // the status to 'stopped' or 'cancelling' via the stop_execution command.
    match app_state.pg_db.get_task_run(execution_id).await {
        Ok(Some(task_run)) => {
            let status = task_run.status.as_str();
            status == "stopped" || status == "cancelling"
        }
        Ok(None) => {
            warn!("is_stop_requested: task run {} not found", execution_id);
            false
        }
        Err(e) => {
            warn!(
                "is_stop_requested: failed to query task run {}: {}",
                execution_id, e
            );
            false
        }
    }
}

/// Get the current git HEAD commit hash for compensation tracking.
/// Uses the current working directory unless `repo_path` is specified.
async fn get_current_commit_hash_in(repo_path: Option<&str>) -> Option<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["rev-parse", "HEAD"]);
    if let Some(path) = repo_path {
        cmd.current_dir(path);
    }
    match cmd.output().await {
        Ok(output) if output.status.success() => {
            let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if hash.is_empty() {
                None
            } else {
                Some(hash)
            }
        }
        _ => None,
    }
}

/// Get the current git HEAD commit hash using the process CWD.
async fn get_current_commit_hash() -> Option<String> {
    get_current_commit_hash_in(None).await
}

/// Get the list of files modified since the last commit (unstaged + staged).
/// Used to populate `WorkflowOutput.files_modified`.
pub async fn get_modified_files() -> Vec<String> {
    let output = tokio::process::Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect(),
        _ => vec![],
    }
}

/// Build a compensation action for a git reset to a specific commit.
pub fn git_reset_compensation(
    execution_id: &str,
    phase: &str,
    iteration: Option<u32>,
    commit_hash: &str,
    repo_path: &str,
) -> CompensationAction {
    CompensationAction {
        id: format!(
            "comp-{}-{}-{}-git-reset",
            execution_id,
            phase,
            iteration.unwrap_or(0)
        ),
        phase: phase.to_string(),
        iteration,
        action_type: CompensationType::GitReset {
            commit_hash: commit_hash.to_string(),
            repo_path: repo_path.to_string(),
        },
        recorded_at: chrono::Utc::now().to_rfc3339(),
        description: format!(
            "Reset {} to commit {} after {} phase failure",
            repo_path,
            &commit_hash[..8.min(commit_hash.len())],
            phase
        ),
    }
}

/// Build a compensation action for worktree removal.
pub fn worktree_remove_compensation(
    execution_id: &str,
    worktree_path: &str,
    branch_name: Option<&str>,
) -> CompensationAction {
    CompensationAction {
        id: format!("comp-{}-worktree-remove", execution_id),
        phase: "setup".to_string(),
        iteration: None,
        action_type: CompensationType::WorktreeRemove {
            worktree_path: worktree_path.to_string(),
            branch_name: branch_name.map(|s| s.to_string()),
        },
        recorded_at: chrono::Utc::now().to_rfc3339(),
        description: format!("Remove worktree at {}", worktree_path),
    }
}
