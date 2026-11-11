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
use crate::step_executor::{ExecutionStepConfig, StepExecutor};
use crate::AppState;

use super::compensation::{CompensationAction, CompensationType};
use super::types::DurableStepResult;

/// Re-export the shared PhaseResult so Restate consumers keep using
/// `super::durable_executor::PhaseResult`.
pub use crate::unified_workflow_executor::types::PhaseResult;

/// Parse a durable batch's steps, or the `failure_context` the batch fails
/// with. An unparseable `ui_bridge` step fails the batch naming the step
/// (`StepsJsonError::UnparseableStep`) — it is never dropped.
fn parse_batch_steps(steps_json: &str) -> Result<Vec<ExecutionStepConfig>, String> {
    crate::unified_workflow_executor::step_conversion::parse_steps_json(steps_json)
        .map_err(|e| format!("Step deserialization failed: {e}"))
}

/// The result of a batch with no step to execute.
///
/// A setup or completion phase with nothing to do has succeeded: no step
/// failed. A VERIFICATION phase with nothing to run checked nothing, so it is
/// not `all_passed`. The phase still ran (`success`), and the durable loop ends
/// it `NoVerificationSteps` (`durable_verification_verdict`).
fn empty_batch_result(phase_name: &str, iteration: Option<u32>) -> PhaseResult {
    PhaseResult {
        phase: phase_name.to_string(),
        iteration,
        stage_index: None,
        success: true,
        all_passed: phase_name != "verification",
        step_results: vec![],
        failure_context: None,
        duration_ms: 0,
        variables_set: Some(vec![]),
        commit_hash: None,
    }
}

/// How many of a verification batch's steps count toward the verdict, by
/// `ExecutionStepConfig::counts_toward_verdict`: required (`required` absent
/// counts as required) and checking something (not a `gate`, not a prompt with
/// no content). It is the one rule the unified loop's verification phase stamps
/// on its step results. This batch folds its own results and stamps nothing,
/// so it applies the rule to the configs.
fn required_verification_steps(steps: &[ExecutionStepConfig]) -> usize {
    steps.iter().filter(|s| s.counts_toward_verdict()).count()
}

/// Whether a durable workflow's verification batch checks nothing: no step
/// counts toward the verdict (an empty list included). That is a property of
/// the workflow's input, so the durable loop reads it once, before the loop and
/// outside any journalled side effect, and replay sees the same answer.
///
/// A batch that does not parse is NOT "nothing to check". It fails as a
/// batch, naming the step (`parse_batch_steps`), and takes the failure path.
pub(crate) fn verification_batch_checks_nothing(steps_json: &str) -> bool {
    matches!(
        parse_batch_steps(steps_json),
        Ok(ref steps) if required_verification_steps(steps) == 0
    )
}

/// What the durable loop does with one verification batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableVerificationVerdict {
    /// Every required step passed: run completion.
    Passed,
    /// No required step: nothing was checked. End the run, not passed, with no
    /// agentic iteration, because no fix can add a check.
    NoVerificationSteps,
    /// A required step failed: run the agentic phase and verify again.
    Failed,
}

/// The durable loop's verdict on one verification batch. "Nothing checked" is
/// tested first. A batch that reports `all_passed` over no required step was a
/// pass from nothing.
pub(crate) fn durable_verification_verdict(
    checks_nothing: bool,
    all_passed: bool,
) -> DurableVerificationVerdict {
    if checks_nothing {
        DurableVerificationVerdict::NoVerificationSteps
    } else if all_passed {
        DurableVerificationVerdict::Passed
    } else {
        DurableVerificationVerdict::Failed
    }
}

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
    let steps: Vec<ExecutionStepConfig> = match parse_batch_steps(steps_json) {
        Ok(s) => s,
        Err(failure_context) => {
            error!(
                "Failed to deserialize {} steps for {}: {}",
                phase_name, execution_id, failure_context
            );
            return PhaseResult {
                phase: phase_name.to_string(),
                iteration,
                stage_index: None,
                success: false,
                all_passed: false,
                step_results: vec![],
                failure_context: Some(failure_context),
                duration_ms: start.elapsed().as_millis() as u64,
                variables_set: Some(vec![]),
                commit_hash: None,
            };
        }
    };

    if steps.is_empty() {
        debug!("No {} steps to execute for {}", phase_name, execution_id);
        return empty_batch_result(phase_name, iteration);
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

    // A verification batch with no required step checked nothing, so it is
    // not `all_passed` however its informational steps went.
    if phase_name == "verification" && required_verification_steps(&steps) == 0 {
        all_passed = false;
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

/// Get the current git HEAD commit hash for compensation tracking.
/// Uses the current working directory unless `repo_path` is specified.
async fn get_current_commit_hash_in(repo_path: Option<&str>) -> Option<String> {
    let mut cmd = crate::process_helpers::tokio_no_window("git");
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
    let output = crate::process_helpers::tokio_no_window("git")
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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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

#[cfg(test)]
mod tests {
    use super::parse_batch_steps;

    #[test]
    fn unparseable_ui_bridge_step_fails_the_batch_naming_it() {
        let json = r#"[{"type":"ui_bridge","id":"s1","name":"open runs","action":"navigate",
                        "url":"http://localhost:3001","timeoutMs":"soon"}]"#;
        let failure = parse_batch_steps(json).unwrap_err();
        assert!(
            failure.starts_with("Step deserialization failed: ui_bridge step 'open runs' (id s1)"),
            "{failure}"
        );
        assert!(
            failure.contains("invalid type: string \"soon\""),
            "{failure}"
        );
        let ok =
            parse_batch_steps(r#"[{"type":"ui_bridge","action":"navigate","url":"http://x"}]"#)
                .unwrap();
        assert_eq!(ok[0].ui_bridge_url.as_deref(), Some("http://x"));
    }
}

#[cfg(test)]
mod nothing_checked_tests {
    use super::{
        durable_verification_verdict, empty_batch_result, verification_batch_checks_nothing,
        DurableVerificationVerdict,
    };

    /// An empty VERIFICATION batch is not `all_passed`. Before plan
    /// 2026-11-06-a-product-workflow-verification-with-nothing-to-check-never-reports-passed
    /// it was, and the durable run stored `complete`.
    #[test]
    fn an_empty_verification_batch_is_not_all_passed() {
        let result = empty_batch_result("verification", Some(1));
        assert!(!result.all_passed);
        assert!(result.success, "the phase ran; it checked nothing");
        assert!(result.step_results.is_empty());
    }

    /// The durable path's own entry: `execute_steps_batch` handed an empty
    /// VERIFICATION batch returns before it builds a step executor or touches a
    /// database, and that batch is not all_passed. An empty setup batch is the
    /// control. The `AppState` is the in-memory one (no database, disk or
    /// network).
    #[tokio::test]
    async fn the_batch_executor_does_not_pass_an_empty_verification_batch() {
        let app_state = crate::AppState::new_for_test();
        let config_storage = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::config_storage::ConfigStorage::new_degraded(),
        ));
        let verification = super::execute_steps_batch(
            &app_state,
            &config_storage,
            "[]",
            "verification",
            Some(1),
            "exec-durable",
            None,
        )
        .await;
        assert!(!verification.all_passed);
        assert!(verification.success, "the phase ran; it checked nothing");
        assert!(verification.step_results.is_empty());
        let setup = super::execute_steps_batch(
            &app_state,
            &config_storage,
            "[]",
            "setup",
            None,
            "exec-durable",
            None,
        )
        .await;
        assert!(setup.all_passed);
    }

    /// A non-empty VERIFICATION batch with no required step is not all_passed
    /// either. A gate runs (through `execute_single_step`, which touches no
    /// database) and succeeds, and it checks nothing.
    #[tokio::test]
    async fn the_batch_executor_does_not_pass_a_verification_batch_that_checks_nothing() {
        let app_state = crate::AppState::new_for_test();
        let config_storage = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::config_storage::ConfigStorage::new_degraded(),
        ));
        let gate_only = super::execute_steps_batch(
            &app_state,
            &config_storage,
            r#"[{"type":"gate","name":"all"}]"#,
            "verification",
            Some(1),
            "exec-durable-gate",
            None,
        )
        .await;
        assert_eq!(gate_only.step_results.len(), 1);
        assert!(
            gate_only.step_results[0].success,
            "the gate itself ran and succeeded"
        );
        assert!(!gate_only.all_passed);
        assert!(gate_only.success, "the phase ran; it checked nothing");
    }

    /// The control: a setup or completion phase with nothing to do succeeded.
    #[test]
    fn an_empty_setup_or_completion_batch_still_succeeds() {
        assert!(empty_batch_result("setup", None).all_passed);
        assert!(empty_batch_result("completion", None).all_passed);
    }

    #[test]
    fn a_batch_with_no_required_step_checks_nothing() {
        assert!(verification_batch_checks_nothing("[]"));
        assert!(verification_batch_checks_nothing(
            r#"[{"type":"command","name":"lint","command":"true","required":false}]"#
        ));
        assert!(verification_batch_checks_nothing(
            r#"[{"type":"command","name":"lint","command":"true","required":false},
                {"type":"command","name":"fmt","command":"true","required":false}]"#
        ));
    }

    /// One required step (absent `required` counts as required) is a check.
    #[test]
    fn a_batch_with_a_required_step_checks_something() {
        assert!(!verification_batch_checks_nothing(
            r#"[{"type":"command","name":"tests","command":"true"}]"#
        ));
        assert!(!verification_batch_checks_nothing(
            r#"[{"type":"command","name":"lint","command":"true","required":false},
                {"type":"command","name":"tests","command":"true","required":true}]"#
        ));
    }

    /// The shared rule (`counts_toward_verdict`) applies to the configs: a
    /// `gate` step and a prompt with no content check nothing, and a prompt
    /// with content checks something.
    #[test]
    fn gates_and_contentless_prompts_check_nothing() {
        assert!(verification_batch_checks_nothing(
            r#"[{"type":"gate","name":"all"}]"#
        ));
        assert!(verification_batch_checks_nothing(
            r#"[{"type":"prompt","name":"review","content":""},{"type":"gate","name":"all"}]"#
        ));
        assert!(!verification_batch_checks_nothing(
            r#"[{"type":"prompt","name":"review","content":"Does the page render?"}]"#
        ));
    }

    /// A batch that does not parse fails as a batch; it is not "nothing to check".
    #[test]
    fn an_unparseable_batch_is_not_nothing_to_check() {
        assert!(!verification_batch_checks_nothing("not json"));
    }

    #[test]
    fn the_durable_verdict_tests_nothing_checked_first() {
        assert_eq!(
            durable_verification_verdict(true, true),
            DurableVerificationVerdict::NoVerificationSteps
        );
        assert_eq!(
            durable_verification_verdict(true, false),
            DurableVerificationVerdict::NoVerificationSteps
        );
        assert_eq!(
            durable_verification_verdict(false, true),
            DurableVerificationVerdict::Passed
        );
        assert_eq!(
            durable_verification_verdict(false, false),
            DurableVerificationVerdict::Failed
        );
    }
}
