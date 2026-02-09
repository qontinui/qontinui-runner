//! Plan Executor
//!
//! Core execution logic for structured plans. Each phase runs as a separate AI session
//! with full context from prior phases via output_log accumulation.

use std::sync::Arc;
use tracing::{error, info, warn};

use crate::database::CheckpointDb;
use crate::step_registry::StepEventLogger;
use crate::summary_generator::generate_task_summary_async;
use crate::unified_ai_session::{AiSessionConfig, AiSessionResult, UnifiedAiSessionExecutor};
use crate::unified_workflow_executor::get_parent_task_id;
use crate::workflow_state::StateMachine;
use crate::AppState;

use super::states::PlanWorkflowState;
use super::types::{PlanConfig, PlanPhaseResult, PlanResult};

/// Executor for structured implementation plans.
///
/// Each phase is executed as a separate AI session. Output from each phase is
/// appended to the task's output_log so subsequent phases have full context.
pub struct PlanExecutor {
    ai_executor: UnifiedAiSessionExecutor,
    checkpoint_db: Arc<CheckpointDb>,
    #[allow(dead_code)]
    app_handle: tauri::AppHandle,
}

impl PlanExecutor {
    /// Create a new plan executor.
    pub fn new(
        app_state: Arc<AppState>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        Self {
            ai_executor: UnifiedAiSessionExecutor::new(
                app_state.clone(),
                app_handle.clone(),
                pid_tracker,
            ),
            checkpoint_db: app_state.checkpoint_db.clone(),
            app_handle,
        }
    }

    /// Set the session manager for interactive mode.
    pub fn with_session_manager(mut self, sm: Arc<crate::claude_session::SessionManager>) -> Self {
        self.ai_executor = self.ai_executor.with_session_manager(sm);
        self
    }

    /// Run the full plan execution.
    pub async fn run(&self, config: PlanConfig) -> PlanResult {
        let start = std::time::Instant::now();
        let total_phases = config.phases.len();

        let logger = StepEventLogger::new(
            self.checkpoint_db.clone(),
            &config.execution_id,
            &config.plan_name,
        );

        let mut state_machine = StateMachine::new(
            self.checkpoint_db.clone(),
            &config.execution_id,
            "plan",
            PlanWorkflowState::Created,
        );

        if let Err(e) = state_machine.persist() {
            warn!("Failed to persist initial plan state: {}", e);
        }

        let mut phase_results: Vec<PlanPhaseResult> = Vec::new();
        let all_succeeded = true;
        let mut failed_at_phase: Option<usize> = None;

        // Execute each phase
        for (phase_index, phase) in config.phases.iter().enumerate() {
            // Check if task was stopped
            if self.is_task_stopped(&config.execution_id) {
                warn!(
                    "Plan was stopped before phase {}: {}",
                    phase_index + 1,
                    phase.name
                );

                let _ = state_machine.transition_to(PlanWorkflowState::stopped_in_phase(
                    phase_index,
                    &phase.name,
                ));
                let _ = state_machine.persist();

                return PlanResult {
                    success: false,
                    total_phases,
                    completed_phases: phase_results.len(),
                    failed_at_phase: None,
                    sweep_ran: false,
                    sweep_iterations: 0,
                    phase_results,
                    sweep_results: Vec::new(),
                    total_duration_ms: start.elapsed().as_millis() as u64,
                };
            }

            // Transition to PhaseRunning
            let _ = state_machine
                .transition_to(PlanWorkflowState::phase_running(phase_index, &phase.name));
            let _ = state_machine.persist();

            info!(
                "PLAN-EXECUTOR: Starting phase {}/{}: {}",
                phase_index + 1,
                total_phases,
                phase.name
            );

            // Build the enhanced prompt with plan context
            let enhanced_prompt = self.build_phase_prompt(
                &config.plan_name,
                &config.plan_overview,
                &config.phases,
                &phase_results,
                phase_index,
            );

            // Create AI session config for this phase
            let ai_config = AiSessionConfig::agentic(
                &config.execution_id,
                &config.plan_name,
                (phase_index + 1) as u32,
            )
            .with_timeout(config.timeout_seconds);

            // Execute the AI session
            let result: AiSessionResult = self
                .ai_executor
                .execute(&ai_config, &enhanced_prompt, &logger)
                .await;

            // Append output to output_log with session marker
            let phase_header = format!(
                "\n\n=== Plan Phase {}/{}: {} ===\n\n",
                phase_index + 1,
                total_phases,
                phase.name
            );
            let full_output = format!("{}{}", phase_header, result.output);

            if let Err(e) = self.checkpoint_db.append_task_output_ex(
                &config.execution_id,
                &full_output,
                true,  // increment_session
                false, // check_completion_marker
            ) {
                error!(
                    "Failed to append output for phase {}: {}",
                    phase_index + 1,
                    e
                );
            }

            let phase_result = PlanPhaseResult {
                phase_index,
                phase_name: phase.name.clone(),
                success: result.success,
                output: result.output,
                duration_ms: result.duration_ms,
            };

            // Transition to PhaseComplete
            let _ = state_machine.transition_to(PlanWorkflowState::phase_complete(
                phase_index,
                &phase.name,
                result.success,
            ));
            let _ = state_machine.persist();

            info!(
                "PLAN-EXECUTOR: Phase {}/{} complete: {} (success={})",
                phase_index + 1,
                total_phases,
                phase.name,
                result.success
            );

            if !result.success {
                failed_at_phase = Some(phase_index);
                phase_results.push(phase_result);

                // Fail early
                let _ = state_machine.transition_to(PlanWorkflowState::failed_in_phase(
                    format!("Phase '{}' failed", phase.name),
                    phase_index,
                    &phase.name,
                ));
                let _ = state_machine.persist();

                // Generate summary for partial completion
                let db = self.checkpoint_db.clone();
                let exec_id = config.execution_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = generate_task_summary_async(db, exec_id).await {
                        warn!("Failed to generate summary for failed plan: {}", e);
                    }
                });

                return PlanResult {
                    success: false,
                    total_phases,
                    completed_phases: phase_results.len(),
                    failed_at_phase,
                    sweep_ran: false,
                    sweep_iterations: 0,
                    phase_results,
                    sweep_results: Vec::new(),
                    total_duration_ms: start.elapsed().as_millis() as u64,
                };
            }

            phase_results.push(phase_result);
        }

        // Next steps sweep loop
        let mut sweep_ran = false;
        let mut sweep_iterations: u32 = 0;
        let mut sweep_results: Vec<PlanPhaseResult> = Vec::new();

        if config.next_steps_sweep && all_succeeded && !self.is_task_stopped(&config.execution_id) {
            sweep_ran = true;
            let max_iterations = config.max_next_steps_iterations.max(1);

            for iteration in 0..max_iterations {
                if self.is_task_stopped(&config.execution_id) {
                    break;
                }

                let _ = state_machine.transition_to(PlanWorkflowState::SweepRunning {
                    iteration: iteration as usize,
                });
                let _ = state_machine.persist();

                info!(
                    "PLAN-EXECUTOR: Next steps sweep iteration {}/{}",
                    iteration + 1,
                    max_iterations
                );

                let sweep_prompt = self.build_sweep_prompt(
                    &config.plan_name,
                    &config.plan_overview,
                    &phase_results,
                    &sweep_results,
                    iteration,
                    max_iterations,
                );

                let ai_config = AiSessionConfig::completion(
                    &config.execution_id,
                    &config.plan_name,
                    format!("Next Steps Sweep {}", iteration + 1),
                    (total_phases + iteration as usize) as u32,
                )
                .with_timeout(config.timeout_seconds);

                let result = self
                    .ai_executor
                    .execute(&ai_config, &sweep_prompt, &logger)
                    .await;

                // Append sweep output
                let sweep_header = format!(
                    "\n\n=== Next Steps Sweep (Iteration {}/{}) ===\n\n",
                    iteration + 1,
                    max_iterations
                );
                let full_output = format!("{}{}", sweep_header, result.output);

                if let Err(e) = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &full_output,
                    true,
                    false,
                ) {
                    error!("Failed to append sweep output: {}", e);
                }

                let no_more_steps = result.output.contains("[NO_MORE_STEPS]");

                sweep_results.push(PlanPhaseResult {
                    phase_index: total_phases + iteration as usize,
                    phase_name: format!("Next Steps Sweep {}", iteration + 1),
                    success: result.success,
                    output: result.output,
                    duration_ms: result.duration_ms,
                });
                sweep_iterations = iteration + 1;

                let _ = state_machine.transition_to(PlanWorkflowState::SweepComplete {
                    iteration: iteration as usize,
                    success: result.success,
                });
                let _ = state_machine.persist();

                if no_more_steps {
                    info!("PLAN-EXECUTOR: AI signaled [NO_MORE_STEPS], ending sweep loop");
                    break;
                }

                if !result.success {
                    warn!(
                        "PLAN-EXECUTOR: Sweep iteration {} failed, ending loop",
                        iteration + 1
                    );
                    break;
                }
            }
        }

        // Mark task complete
        let _ = state_machine.transition_to(PlanWorkflowState::Completed);
        let _ = state_machine.persist();

        if let Err(e) = self.checkpoint_db.complete_task_run(&config.execution_id) {
            error!("Failed to complete task run: {}", e);
        }

        // Generate summary
        let db = self.checkpoint_db.clone();
        let exec_id = config.execution_id.clone();
        tokio::spawn(async move {
            if let Err(e) = generate_task_summary_async(db, exec_id).await {
                warn!("Failed to generate plan summary: {}", e);
            }
        });

        PlanResult {
            success: all_succeeded,
            total_phases,
            completed_phases: phase_results.len(),
            failed_at_phase,
            sweep_ran,
            sweep_iterations,
            phase_results,
            sweep_results,
            total_duration_ms: start.elapsed().as_millis() as u64,
        }
    }

    /// Build an enhanced prompt for a specific phase with full plan context.
    fn build_phase_prompt(
        &self,
        plan_name: &str,
        plan_overview: &str,
        all_phases: &[super::types::PlanPhase],
        completed_results: &[PlanPhaseResult],
        current_index: usize,
    ) -> String {
        let total = all_phases.len();

        let mut prompt = format!(
            "## Implementation Plan: {}\n\n{}\n\n## Progress: Phase {}/{}\n\n",
            plan_name,
            plan_overview,
            current_index + 1,
            total,
        );

        // Completed phases
        if !completed_results.is_empty() {
            prompt.push_str("### Completed Phases:\n");
            for result in completed_results {
                let status = if result.success { "SUCCESS" } else { "FAILED" };
                prompt.push_str(&format!(
                    "- Phase {}: {} [{}]\n",
                    result.phase_index + 1,
                    result.phase_name,
                    status
                ));
            }
            prompt.push('\n');
        }

        // Upcoming phases (after current)
        if current_index + 1 < total {
            prompt.push_str("### Upcoming Phases (DO NOT implement yet):\n");
            for (i, phase) in all_phases.iter().enumerate().skip(current_index + 1) {
                prompt.push_str(&format!("- Phase {}: {}\n", i + 1, phase.name));
            }
            prompt.push('\n');
        }

        // Current phase
        prompt.push_str(&format!(
            "## Current Phase: {}\n\n{}\n",
            all_phases[current_index].name, all_phases[current_index].prompt
        ));

        prompt
    }

    /// Build the prompt for the next-steps sweep.
    fn build_sweep_prompt(
        &self,
        plan_name: &str,
        plan_overview: &str,
        phase_results: &[PlanPhaseResult],
        prior_sweep_results: &[PlanPhaseResult],
        iteration: u32,
        max_iterations: u32,
    ) -> String {
        let mut prompt = format!(
            "## Next Steps Sweep for: {} (Iteration {}/{})\n\n{}\n\n### All planned phases completed:\n",
            plan_name,
            iteration + 1,
            max_iterations,
            plan_overview,
        );

        for result in phase_results {
            let status = if result.success { "SUCCESS" } else { "FAILED" };
            prompt.push_str(&format!(
                "- Phase {}: {} [{}]\n",
                result.phase_index + 1,
                result.phase_name,
                status
            ));
        }

        // Include prior sweep iteration summaries
        if !prior_sweep_results.is_empty() {
            prompt.push_str("\n### Prior sweep iterations:\n");
            for (i, result) in prior_sweep_results.iter().enumerate() {
                let status = if result.success { "SUCCESS" } else { "FAILED" };
                // Include a brief excerpt of what was addressed
                let excerpt = if result.output.len() > 300 {
                    format!("{}...", &result.output[..300])
                } else {
                    result.output.clone()
                };
                prompt.push_str(&format!(
                    "- Sweep {} [{}]: {}\n",
                    i + 1,
                    status,
                    excerpt.lines().next().unwrap_or("(no output)")
                ));
            }
        }

        prompt.push_str(
            r#"
## Your Task

Review ALL work done across all phases and any prior sweep iterations. Look for:

1. **Overlooked items** from the plan that weren't fully addressed
2. **Incomplete implementations** - partially done work, stub functions, placeholder code
3. **Edge cases** that weren't handled
4. **Integration gaps** - phases that don't connect properly
5. **TODO/FIXME markers** left in the code

**IMPORTANT:** Do not just list items. Actually implement any fixes or missing pieces you find.

**STOP SIGNAL:** If ALL work is complete and there are no remaining next steps to implement, output `[NO_MORE_STEPS]` at the end of your response. Only output this marker when you are confident everything is done.
If there IS remaining work, implement it now. Do NOT output `[NO_MORE_STEPS]` if you made changes or found issues.
"#,
        );

        prompt
    }

    /// Check if the task has been stopped externally.
    fn is_task_stopped(&self, execution_id: &str) -> bool {
        let task_id_to_check = get_parent_task_id(execution_id);

        match self.checkpoint_db.get_task_run(&task_id_to_check) {
            Ok(Some(task)) => {
                if task.status == "stopped" {
                    info!("Plan task {} has been stopped externally", task_id_to_check);
                    true
                } else {
                    false
                }
            }
            Ok(None) => false,
            Err(e) => {
                warn!("Failed to check task status: {}", e);
                false
            }
        }
    }
}
