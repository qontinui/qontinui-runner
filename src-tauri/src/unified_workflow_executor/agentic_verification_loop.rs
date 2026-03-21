//! Agentic Verification Loop — alternative workflow architecture.
//!
//! Runs a Verification Agent → Worker Agent loop where verification is performed
//! by an AI agent rather than deterministic steps.

use tracing::{info, warn};

use crate::step_executor::ExecutionStepConfig;
use crate::step_registry::StepEventLogger;

use super::health_monitor::fetch_verifier_ui_context;
use super::loop_controller::LoopController;
use super::types::{AgenticOutcome, LoopConfig, LoopResult};

impl LoopController {
    // =========================================================================
    // Agentic Verification Loop — alternative architecture
    // =========================================================================

    /// Run the agentic verification loop: Verification Agent → Worker Agent → repeat.
    ///
    /// Unlike the traditional loop that uses pre-defined deterministic verification steps,
    /// this architecture uses a verification *agent* that reasons about whether the goal
    /// has been achieved, and a worker agent that takes actions based on the verifier's
    /// feedback.
    ///
    /// Returns a standard LoopResult for compatibility with the existing result pipeline.
    pub(crate) async fn run_agentic_verification_loop(
        &self,
        config: &mut LoopConfig,
        has_agentic_steps: bool,
        agentic_steps: &[ExecutionStepConfig],
        _all_step_results: &mut Vec<crate::step_executor::StepExecutionResult>,
        logger: &StepEventLogger,
    ) -> LoopResult {
        use crate::autoresearch::agentic_verification::*;

        let av_config = config
            .agentic_verification_config
            .clone()
            .unwrap_or_default();

        let goal = if av_config.goal.is_empty() {
            config.base_prompt.clone()
        } else {
            av_config.goal.clone()
        };

        let max_iterations = if av_config.max_iterations > 0 {
            av_config.max_iterations
        } else {
            config.max_iterations
        };

        info!(
            "AGENTIC-VERIFICATION: Starting loop (max_iterations={}, confidence_threshold={}, consecutive_passes_required={})",
            max_iterations, av_config.confidence_threshold, av_config.required_consecutive_passes
        );

        let mut iteration_results: Vec<AgenticVerificationIterationResult> = Vec::new();
        let mut consecutive_passes: u32 = 0;
        let mut iteration: u32 = 0;

        // Save original model/provider overrides so we can restore after each iteration
        let original_model_override = config.model_override.clone();
        let original_provider_override = config.provider_override.clone();

        loop {
            iteration += 1;

            // Check stop signal
            if self.is_task_stopped(&config.execution_id) {
                info!(
                    "AGENTIC-VERIFICATION: Stopped by user at iteration {}",
                    iteration
                );
                let av_result = AgenticVerificationResult {
                    iterations_run: iteration,
                    goal_achieved: false,
                    unreachable: false,
                    was_stopped: true,
                    max_iterations_reached: false,
                    iteration_results,
                    final_verdict: None,
                };
                self.canvas_manager
                    .lock()
                    .await
                    .on_agentic_verification_exit(&av_result)
                    .await;
                return av_result.to_loop_result();
            }

            // Check max iterations
            if iteration > max_iterations {
                info!(
                    "AGENTIC-VERIFICATION: Max iterations ({}) reached",
                    max_iterations
                );
                let av_result = AgenticVerificationResult {
                    iterations_run: iteration - 1,
                    goal_achieved: false,
                    unreachable: false,
                    was_stopped: false,
                    max_iterations_reached: true,
                    iteration_results: iteration_results.clone(),
                    final_verdict: iteration_results.last().map(|r| r.verdict.clone()),
                };
                self.canvas_manager
                    .lock()
                    .await
                    .on_agentic_verification_exit(&av_result)
                    .await;
                return av_result.to_loop_result();
            }

            // Update routing context
            config.set_routing_context(iteration, 0);

            // Record activity
            self.record_activity(
                &config.execution_id,
                &format!("agentic_verification_iteration_{}", iteration),
            );

            // ── STEP 1: Verification Agent ──────────────────────────────
            // The verification agent assesses the current state against the goal.
            // On the first iteration (if verify_first is false), we skip directly
            // to the worker agent.

            let should_verify = iteration > 1 || av_config.verify_first;

            let verdict = if should_verify {
                info!(
                    "AGENTIC-VERIFICATION: Running verification agent (iteration {})",
                    iteration
                );

                let verifier_start = std::time::Instant::now();

                let verifier_system = AgenticVerificationPrompts::verifier_system_prompt(
                    &goal,
                    av_config.verifier.system_preamble.as_deref(),
                );

                // Build context for the verifier: include previous iteration results
                let mut verifier_context = verifier_system;
                if let Some(last_result) = iteration_results.last() {
                    verifier_context.push_str(&format!(
                        "\n\n## Previous Iteration ({}) Summary\nWorker action: {}\n",
                        last_result.iteration,
                        last_result.worker_summary.as_deref().unwrap_or("(none)"),
                    ));
                }

                // Fetch live UI Bridge data for the verifier
                let ui_context = fetch_verifier_ui_context(
                    av_config.verifier.use_screenshots,
                    av_config.verifier.include_console_errors,
                    av_config.verifier.include_app_health,
                )
                .await;
                if !ui_context.is_empty() {
                    verifier_context.push_str(&ui_context);
                }

                // Apply verifier model/provider overrides
                config.model_override = av_config
                    .verifier
                    .model
                    .clone()
                    .or_else(|| original_model_override.clone());
                config.provider_override = av_config
                    .verifier
                    .provider
                    .clone()
                    .or_else(|| original_provider_override.clone());

                // Run verifier through the agentic executor with a verification prompt
                let verifier_prompt = ExecutionStepConfig {
                    step_type: "prompt".to_string(),
                    name: Some(format!("Verification Agent (iteration {})", iteration)),
                    prompt_content: Some(verifier_context),
                    ..ExecutionStepConfig::default()
                };

                let (outcome, _injected) = self
                    .agentic_executor
                    .run_agentic(
                        config,
                        iteration,
                        "", // No failure context — the verifier generates its own
                        true,
                        &[verifier_prompt],
                        logger,
                    )
                    .await;

                let verifier_duration = verifier_start.elapsed().as_millis() as u64;

                // Parse the verdict from the AI output
                let verdict = match &outcome {
                    AgenticOutcome::Success { output, .. }
                    | AgenticOutcome::Failed { output, .. } => {
                        parse_verification_verdict(output)
                            .unwrap_or_else(|| {
                                info!("AGENTIC-VERIFICATION: Failed to parse structured verdict, using heuristic");
                                heuristic_verdict(output)
                            })
                    }
                    AgenticOutcome::Error { error } => {
                        warn!("AGENTIC-VERIFICATION: Verifier error: {}", error);
                        VerificationVerdict {
                            status: VerificationStatus::Fail,
                            confidence: 0.0,
                            observations: format!("Verification agent error: {}", error),
                            next_priority: Some("Retry verification".to_string()),
                            issues: vec![VerificationIssue {
                                description: error.clone(),
                                severity: "critical".to_string(),
                                suggestion: None,
                            }],
                            unreachable: false,
                            unreachable_reason: None,
                        }
                    }
                    AgenticOutcome::Skipped => {
                        VerificationVerdict {
                            status: VerificationStatus::Fail,
                            confidence: 0.0,
                            observations: "Verification agent was skipped".to_string(),
                            next_priority: Some("Configure verification agent".to_string()),
                            issues: vec![],
                            unreachable: false,
                            unreachable_reason: None,
                        }
                    }
                };

                info!(
                    "AGENTIC-VERIFICATION: Verdict: status={}, confidence={:.0}%, issues={}  ({}ms)",
                    verdict.status,
                    verdict.confidence * 100.0,
                    verdict.issues.len(),
                    verifier_duration,
                );

                // Emit verdict to task output
                if let Err(e) = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &format!(
                        "\n--- Verification Agent (iteration {}) ---\nStatus: {}\nConfidence: {:.0}%\nObservations: {}\n",
                        iteration,
                        verdict.status,
                        verdict.confidence * 100.0,
                        verdict.observations,
                    ),
                    false,
                    false,
                ) {
                    warn!("Failed to append verifier output: {}", e);
                }

                Some((verdict, verifier_duration))
            } else {
                None
            };

            // ── STEP 2: Check if goal achieved ──────────────────────────
            if let Some((ref v, _)) = verdict {
                if v.status == VerificationStatus::Pass
                    && v.confidence >= av_config.confidence_threshold
                {
                    consecutive_passes += 1;
                    if consecutive_passes >= av_config.required_consecutive_passes {
                        info!(
                            "AGENTIC-VERIFICATION: Goal achieved after {} iterations ({} consecutive passes)",
                            iteration, consecutive_passes
                        );
                        iteration_results.push(AgenticVerificationIterationResult {
                            iteration,
                            verdict: v.clone(),
                            worker_ran: false,
                            worker_summary: None,
                            verifier_duration_ms: verdict.as_ref().map(|(_, d)| *d).unwrap_or(0),
                            worker_duration_ms: 0,
                        });
                        // Canvas: emit iteration panel and exit summary
                        {
                            let verifier_ms = verdict.as_ref().map(|(_, d)| *d).unwrap_or(0);
                            self.canvas_manager
                                .lock()
                                .await
                                .on_agentic_verification_iteration(
                                    iteration,
                                    v,
                                    None,
                                    verifier_ms,
                                    0,
                                )
                                .await;
                        }
                        let av_result = AgenticVerificationResult {
                            iterations_run: iteration,
                            goal_achieved: true,
                            unreachable: false,
                            was_stopped: false,
                            max_iterations_reached: false,
                            iteration_results,
                            final_verdict: Some(v.clone()),
                        };
                        self.canvas_manager
                            .lock()
                            .await
                            .on_agentic_verification_exit(&av_result)
                            .await;
                        return av_result.to_loop_result();
                    }
                } else {
                    consecutive_passes = 0;
                }

                // Check unreachable
                if v.status == VerificationStatus::Unreachable || v.unreachable {
                    info!(
                        "AGENTIC-VERIFICATION: Goal deemed unreachable: {}",
                        v.unreachable_reason
                            .as_deref()
                            .unwrap_or("(no reason given)")
                    );
                    // Canvas: emit iteration panel for unreachable verdict
                    {
                        let verifier_ms = verdict.as_ref().map(|(_, d)| *d).unwrap_or(0);
                        self.canvas_manager
                            .lock()
                            .await
                            .on_agentic_verification_iteration(iteration, v, None, verifier_ms, 0)
                            .await;
                    }
                    iteration_results.push(AgenticVerificationIterationResult {
                        iteration,
                        verdict: v.clone(),
                        worker_ran: false,
                        worker_summary: None,
                        verifier_duration_ms: verdict.as_ref().map(|(_, d)| *d).unwrap_or(0),
                        worker_duration_ms: 0,
                    });
                    let av_result = AgenticVerificationResult {
                        iterations_run: iteration,
                        goal_achieved: false,
                        unreachable: true,
                        was_stopped: false,
                        max_iterations_reached: false,
                        iteration_results,
                        final_verdict: Some(v.clone()),
                    };
                    self.canvas_manager
                        .lock()
                        .await
                        .on_agentic_verification_exit(&av_result)
                        .await;
                    return av_result.to_loop_result();
                }
            }

            // ── STEP 3: Worker Agent ────────────────────────────────────
            // The worker agent receives the verifier's feedback and takes action.

            info!(
                "AGENTIC-VERIFICATION: Running worker agent (iteration {})",
                iteration
            );

            let worker_start = std::time::Instant::now();

            // Apply worker model/provider overrides
            config.model_override = av_config
                .worker
                .model
                .clone()
                .or_else(|| original_model_override.clone());
            config.provider_override = av_config
                .worker
                .provider
                .clone()
                .or_else(|| original_provider_override.clone());

            // Build worker context from the verification verdict
            let worker_failure_context = if let Some((ref v, _)) = verdict {
                AgenticVerificationPrompts::worker_context_from_verdict(
                    &goal,
                    v,
                    iteration,
                    max_iterations,
                )
            } else {
                // First iteration without verify_first — just provide the goal
                format!(
                    "## Goal\n{}\n\nThis is the first iteration. Assess the current state and take action toward the goal.\n",
                    goal
                )
            };

            let (worker_outcome, _injected_steps) = self
                .agentic_executor
                .run_agentic(
                    config,
                    iteration,
                    &worker_failure_context,
                    has_agentic_steps,
                    agentic_steps,
                    logger,
                )
                .await;

            let worker_duration = worker_start.elapsed().as_millis() as u64;

            let worker_summary = match &worker_outcome {
                AgenticOutcome::Success { output, .. } => {
                    // Extract first 500 chars as summary
                    let summary = if output.len() > 500 {
                        format!("{}...", &output[..500])
                    } else {
                        output.clone()
                    };
                    Some(summary)
                }
                AgenticOutcome::Failed { output, error, .. } => Some(format!(
                    "Failed: {} (output: {}...)",
                    error,
                    &output[..output.len().min(200)]
                )),
                AgenticOutcome::Error { error } => Some(format!("Error: {}", error)),
                AgenticOutcome::Skipped => None,
            };

            info!(
                "AGENTIC-VERIFICATION: Worker completed in {}ms",
                worker_duration
            );

            // ── Record iteration result ─────────────────────────────────
            let iter_result = AgenticVerificationIterationResult {
                iteration,
                verdict: verdict
                    .as_ref()
                    .map(|(v, _)| v.clone())
                    .unwrap_or(VerificationVerdict {
                        status: VerificationStatus::Partial,
                        confidence: 0.0,
                        observations: "No verification performed (first iteration)".to_string(),
                        next_priority: None,
                        issues: vec![],
                        unreachable: false,
                        unreachable_reason: None,
                    }),
                worker_ran: true,
                worker_summary,
                verifier_duration_ms: verdict.as_ref().map(|(_, d)| *d).unwrap_or(0),
                worker_duration_ms: worker_duration,
            };
            iteration_results.push(iter_result);

            // Canvas: emit iteration panel and update tracker
            {
                let last = iteration_results.last().expect("just pushed");
                let mut cm = self.canvas_manager.lock().await;
                cm.on_agentic_verification_iteration(
                    iteration,
                    &last.verdict,
                    last.worker_summary.as_deref(),
                    last.verifier_duration_ms,
                    last.worker_duration_ms,
                )
                .await;
                cm.on_agentic_verification_tracker(&iteration_results).await;
            }

            // Restore original model/provider overrides for next iteration
            config.model_override = original_model_override.clone();
            config.provider_override = original_provider_override.clone();

            // Increment session count in DB
            if let Err(e) = self.checkpoint_db.append_task_output_ex(
                &config.execution_id,
                "",
                true,  // increment_session
                false, // check_completion_marker
            ) {
                warn!("Failed to increment session count: {}", e);
            }
        }
    }
}
