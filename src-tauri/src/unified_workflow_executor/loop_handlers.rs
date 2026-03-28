//! Handler methods for the verification-agentic loop state machine.
//!
//! Each handler corresponds to one `LoopState` variant and contains the logic
//! for one phase of the verification-agentic loop. The dispatch method
//! `run_loop_state_machine` drives the state machine by matching on the current
//! state and calling the appropriate handler, which returns the next state.
//!
//! This is the only implementation of the Traditional workflow loop — the
//! monolithic `run_verification_agentic_loop` was deleted after parity was
//! verified by the Group E scout integration tests.

use std::time::Instant;

use tauri::Emitter;
use tracing::{debug, error, info, warn};

use crate::database::CreateTaskRunEventInput;
use crate::event_system::EventBroadcaster;
use crate::orchestrator::integration::StageTransition;
use crate::orchestrator::knowledge::{parse_findings_from_output, AgentType};
use crate::step_executor::{ExecutionStepConfig, StepExecutionResult, VerificationPhaseResult};
use crate::step_registry::StepEventLogger;
use crate::str_utils::truncate_str;

use super::compensation;
use super::convergence::{ConvergenceAction, ConvergencePattern, ConvergenceReport};
use super::loop_state_machine::{CompletionReason, LoopContext, LoopState};
use super::states::UnifiedWorkflowState;
use super::types::{
    get_parent_task_id, AgenticOutcome, IterationResult, LoopConfig, LoopResult, RollbackPolicy,
};

use super::loop_controller::LoopController;

impl LoopController {
    // =========================================================================
    // Dispatch Method
    // =========================================================================

    /// Run the verification-agentic loop as an explicit state machine.
    ///
    /// This replaces `run_verification_agentic_loop` with the same signature
    /// and behavior, but delegates each phase to a small handler that returns
    /// the next `LoopState`.
    pub(crate) async fn run_loop_state_machine(
        &self,
        config: &mut LoopConfig,
        verification_steps: &[ExecutionStepConfig],
        has_agentic_steps: bool,
        agentic_steps: &[ExecutionStepConfig],
        all_step_results: &mut Vec<StepExecutionResult>,
        transitions: &mut Vec<StageTransition>,
        current_stage: &mut String,
        logger: &StepEventLogger,
        initial_dynamic_steps: Vec<ExecutionStepConfig>,
    ) -> LoopResult {
        // Enforce a floor of 1 on max_iterations to prevent zero-iteration failures
        config.max_iterations = config.max_iterations.max(1);

        // Initialize loop context with all cross-iteration state
        let mut ctx = LoopContext::new(config, self.checkpoint_db.clone(), initial_dynamic_steps);

        if !ctx.dynamic_steps.is_empty() {
            info!(
                "Starting verification-agentic loop with {} pre-injected dynamic step(s)",
                ctx.dynamic_steps.len()
            );
        }

        // Capture initial HEAD before the loop starts — used as source_commit for
        // the Clean rollback policy (revert to pre-workflow state).
        ctx.initial_source_commit = if let Some(ref p) = config.project_path {
            compensation::get_head_commit_async(std::path::Path::new(p)).await
        } else {
            None
        };

        // State machine dispatch loop
        let mut state = LoopState::CheckPreconditions;

        let loop_result = loop {
            state = match state {
                LoopState::CheckPreconditions => {
                    self.handle_check_preconditions(&mut ctx, config).await
                }

                LoopState::CheckEnvironment => {
                    self.handle_check_environment(&mut ctx, config).await
                }

                LoopState::RunVerification => {
                    self.handle_run_verification(
                        &mut ctx,
                        config,
                        verification_steps,
                        all_step_results,
                        transitions,
                        current_stage,
                        logger,
                    )
                    .await
                }

                LoopState::EvaluateVerification {
                    verification_result,
                } => {
                    self.handle_evaluate_verification(&mut ctx, config, verification_result)
                        .await
                }

                LoopState::BuildFailureContext {
                    verification_result,
                    convergence_report,
                } => {
                    self.handle_build_failure_context(
                        &mut ctx,
                        config,
                        verification_result,
                        convergence_report,
                        transitions,
                        current_stage,
                    )
                    .await
                }

                LoopState::RunAgentic {
                    failure_context,
                    verification_result,
                } => {
                    self.handle_run_agentic(
                        &mut ctx,
                        config,
                        &failure_context,
                        has_agentic_steps,
                        agentic_steps,
                        verification_steps,
                        &verification_result,
                        logger,
                    )
                    .await
                }

                LoopState::PostAgentic {
                    outcome,
                    injected_steps,
                    failure_context,
                } => {
                    self.handle_post_agentic(
                        &mut ctx,
                        config,
                        outcome,
                        injected_steps,
                        &failure_context,
                        has_agentic_steps,
                    )
                    .await
                }

                LoopState::CheckPostAgenticSignals { outcome } => {
                    self.handle_check_post_agentic_signals(&mut ctx, config, &outcome)
                        .await
                }

                LoopState::ApprovalGate { outcome } => {
                    self.handle_approval_gate(&mut ctx, config, &outcome).await
                }

                LoopState::AdvanceIteration => {
                    self.handle_advance_iteration(&mut ctx, config).await
                }

                LoopState::Complete { reason } => {
                    break reason.into_loop_result(&ctx);
                }
            };
        };

        // =====================================================================
        // Post-loop cleanup (lines 4026-4118 from original)
        // =====================================================================

        // Conductor-inspired compensation: execute rollback if the workflow failed
        // and a rollback policy is configured. Only runs for non-success exits
        // (critical failure, max iterations, unfixable errors).
        if !loop_result.verification_passed
            && !loop_result.was_stopped
            && !matches!(config.rollback_policy, RollbackPolicy::None)
        {
            if let Some(wd) = config.project_path.as_deref().map(std::path::Path::new) {
                let source_commit = ctx.initial_source_commit.as_deref();
                match ctx
                    .compensation_manager
                    .execute_rollback(
                        &config.execution_id,
                        &config.rollback_policy,
                        wd,
                        source_commit,
                        &loop_result.iteration_results,
                    )
                    .await
                {
                    Ok(Some(commit)) => {
                        info!(
                            "COMPENSATION: Rolled back to {} (policy: {:?})",
                            &commit[..commit.len().min(8)],
                            config.rollback_policy
                        );
                        let _ = self.app_state.pg_db.append_task_output_ex(
                            &config.execution_id,
                            &format!(
                                "\n=== COMPENSATION ROLLBACK ===\nRolled back to commit {} (policy: {:?})\n",
                                &commit[..commit.len().min(8)],
                                config.rollback_policy
                            ),
                            false,
                            false,
                        ).await;
                    }
                    Ok(None) => {
                        debug!("COMPENSATION: No rollback performed (policy returned None)");
                    }
                    Err(e) => {
                        warn!("COMPENSATION: Rollback failed: {}", e);
                    }
                }
            }
        }

        // Auto-detect recurring findings → known issues
        let auto_detected_ids: Vec<String> = {
            let db_c = self.checkpoint_db.clone();
            let eid_c = config.execution_id.clone();
            tokio::task::spawn_blocking(move || {
                db_c.with_conn(|conn| {
                    match crate::known_issues::auto_detect::check_and_promote_recurring_findings(
                        conn,
                        &eid_c,
                    ) {
                        Ok(new_ids) => {
                            if !new_ids.is_empty() {
                                info!(
                                    "Auto-detected {} new known issue(s) from recurring findings",
                                    new_ids.len()
                                );
                            }
                            Ok(new_ids)
                        }
                        Err(e) => {
                            warn!("Failed to check for recurring findings: {}", e);
                            Ok(vec![])
                        }
                    }
                })
            }).await.unwrap_or_else(|_| Ok(vec![])).unwrap_or_default()
        };

        // Notify frontend about auto-detected known issues
        if !auto_detected_ids.is_empty() {
            let _ = self.app_handle.emit(
                "known-issues-auto-detected",
                serde_json::json!({
                    "count": auto_detected_ids.len(),
                    "issue_ids": auto_detected_ids,
                }),
            );
        }

        // Backfill downstream_success on all traces emitted during this loop.
        // This correlates each iteration's agent performance with the final outcome.
        if let Err(e) = crate::database::pipeline_traces::backfill_downstream_success(
            &self.checkpoint_db,
            &config.execution_id,
            loop_result.verification_passed,
        ) {
            debug!("Failed to backfill downstream_success on traces: {}", e);
        }

        loop_result
    }

    // =========================================================================
    // Handler: CheckPreconditions
    // =========================================================================

    /// Check stop/pause, max iterations, session budget, reset task status, log iteration start.
    ///
    /// Original lines: 2167-2302
    async fn handle_check_preconditions(
        &self,
        ctx: &mut LoopContext,
        config: &mut LoopConfig,
    ) -> LoopState {
        ctx.iteration += 1;

        // Update routing context for conditional model routing
        config.set_routing_context(ctx.iteration, ctx.verification_failures);

        info!(
            "--- LOOP ITERATION {} of {} ---{}",
            ctx.iteration,
            config.max_iterations,
            if config.starting_iteration > 0 {
                " (resumed)"
            } else {
                ""
            }
        );

        self.record_activity(
            &config.execution_id,
            &format!("loop_iteration_{}_start", ctx.iteration),
        );

        // Check if the task has been stopped externally (e.g., user clicked Stop button)
        if self.is_task_stopped(&config.execution_id) {
            warn!("Task was stopped externally - exiting loop");
            // Decrement because this iteration didn't run
            ctx.iteration -= 1;
            return LoopState::Complete {
                reason: CompletionReason::Stopped,
            };
        }

        // Wait if paused (user clicked Pause in the dashboard)
        self.wait_while_paused(&config.execution_id).await;

        // Re-check stop after unpause (user may have stopped while paused)
        if self.is_task_stopped(&config.execution_id) {
            warn!("Task was stopped while paused - exiting loop");
            ctx.iteration -= 1;
            return LoopState::Complete {
                reason: CompletionReason::Stopped,
            };
        }

        // Check global max_sessions budget (across all stages).
        // sessions_count is incremented in the database on each agentic session,
        // so this enforces the overall session budget even in multi-stage workflows.
        if let Some(max) = config.max_sessions {
            let task_run_result = self.app_state.pg_db.get_task_run(&config.execution_id).await;
            if let Ok(Some(task_run)) = task_run_result {
                if task_run.sessions_count >= max {
                    warn!(
                        "Global max_sessions ({}) reached (sessions_count={}) - exiting loop",
                        max, task_run.sessions_count
                    );
                    ctx.iteration -= 1;
                    return LoopState::Complete {
                        reason: CompletionReason::MaxSessionsReached,
                    };
                }
            }
        }

        // Check escalation policy time limit (wall-clock).
        // This prevents workflows from running indefinitely when a time budget is set.
        if config.escalation_policy.is_time_exceeded(ctx.loop_start_time.elapsed().as_secs()) {
            let elapsed = ctx.loop_start_time.elapsed().as_secs();
            warn!(
                "ESCALATION-TIMEOUT: Wall-clock time limit exceeded ({}s elapsed, limit={}s) - stopping loop",
                elapsed,
                config.escalation_policy.time_limit_secs.unwrap_or(0),
            );
            ctx.iteration -= 1;
            return LoopState::Complete {
                reason: CompletionReason::PhaseTimeout {
                    phase: "escalation_time_limit".to_string(),
                    elapsed_ms: elapsed * 1000,
                },
            };
        }

        // CRITICAL: Reset task status to "running" at the start of each iteration
        // This ensures that any external modifications (e.g., AI calling APIs) don't
        // prematurely mark the task as complete or failed. The loop controller is
        // the ONLY authority on task completion in unified workflows.
        let status_result = self.app_state.pg_db.update_task_run_status(&config.execution_id, "running").await;
        if let Err(e) = status_result {
            warn!(
                "Failed to reset task status to running for iteration {}: {}",
                ctx.iteration, e
            );
        } else {
            // Broadcast task-run-update to both Tauri + WebSocket
            let broadcaster = EventBroadcaster::new(self.app_handle.clone());
            broadcaster.task_run_update(&config.execution_id, "running", Some(ctx.iteration), None);
        }

        // Check phase_timeout_ms: if configured, verify the loop hasn't exceeded its budget
        if let Some(timeout_ms) = config.phase_timeout_ms {
            if let Some(ref start) = ctx.agentic_phase_start {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                if elapsed_ms > timeout_ms {
                    warn!(
                        "Phase timeout exceeded: {}ms > {}ms budget - exiting loop",
                        elapsed_ms, timeout_ms
                    );
                    ctx.iteration -= 1;
                    return LoopState::Complete {
                        reason: CompletionReason::PhaseTimeout {
                            phase: "loop".to_string(),
                            elapsed_ms,
                        },
                    };
                }
            }
        }

        // Check if we've exceeded max iterations BEFORE running verification
        if ctx.iteration > config.max_iterations {
            warn!(
                "Max iterations ({}) exceeded - exiting loop",
                config.max_iterations
            );
            ctx.iteration -= 1; // Don't count this iteration
            return LoopState::Complete {
                reason: CompletionReason::MaxIterationsReached,
            };
        }

        // Log iteration start to database
        // Use append_task_output_ex with check_completion_marker=false
        // because in unified workflows, VERIFICATION is the authority on completion,
        // not the [TASK_COMPLETE] marker from the AI
        let _ = self.app_state.pg_db.append_task_output_ex(
            &config.execution_id,
            &format!(
                "\n\n=== Verification-Agentic Loop: Iteration {} ===\n",
                ctx.iteration
            ),
            false,
            false,
        ).await;

        LoopState::CheckEnvironment
    }

    // =========================================================================
    // Handler: CheckEnvironment
    // =========================================================================

    /// Auto-connect SDK (iteration 1), check environment readiness.
    ///
    /// Original lines: 2304-2363
    async fn handle_check_environment(
        &self,
        ctx: &mut LoopContext,
        config: &mut LoopConfig,
    ) -> LoopState {
        // AUTO-CONNECT SDK FOR UI-FOCUSED WORKFLOWS (first iteration only)
        if ctx.iteration == 1 {
            super::phases::try_auto_connect_sdk_for_ui_workflow(&config.workflow_name).await;
        }

        // ENVIRONMENT READINESS CHECK (before verification)
        // Check that the runtime environment (runner API, SDK connection, app health)
        // is ready before running verification assertions. If issues are found, attempt
        // automated recovery (reconnect SDK, refresh page). This prevents wasting
        // agentic iterations on environment problems vs actual code issues.
        {
            let env_result =
                super::phases::check_environment_readiness(ctx.iteration, &config.workflow_name)
                    .await;

            if !env_result.ready {
                // Log environment issue to task output
                let _ = self.app_state.pg_db.append_task_output_ex(
                    &config.execution_id,
                    &format!(
                        "\n--- Environment Check (Iteration {}): NOT READY ---\n{}\n",
                        ctx.iteration, env_result.summary
                    ),
                    false,
                    false,
                ).await;

                // Merge env failure context with any existing health regression from the
                // previous iteration (don't clobber — both are valuable diagnostic context).
                if let Some(env_ctx) = env_result.env_failure_context {
                    ctx.pending_health_regression =
                        Some(match ctx.pending_health_regression.take() {
                            Some(existing) => format!("{}\n\n{}", env_ctx, existing),
                            None => env_ctx,
                        });
                }

                warn!(
                    "ENV-DOCTOR: Environment not ready for iteration {} — \
                     verification will likely fail due to environment, not code issues",
                    ctx.iteration
                );
            } else {
                if env_result.recovery_attempted {
                    let _ = self.app_state.pg_db.append_task_output_ex(
                        &config.execution_id,
                        &format!(
                            "\n--- Environment Check (Iteration {}): RECOVERED ---\n{}\n",
                            ctx.iteration, env_result.summary
                        ),
                        false,
                        false,
                    ).await;
                }
                debug!("ENV-DOCTOR: {}", env_result.summary);
            }
        }

        LoopState::RunVerification
    }

    // =========================================================================
    // Handler: RunVerification
    // =========================================================================

    /// Merge static+dynamic steps, filter consistently passing, execute verification, save trace.
    ///
    /// Original lines: 2365-2555
    async fn handle_run_verification(
        &self,
        ctx: &mut LoopContext,
        config: &mut LoopConfig,
        verification_steps: &[ExecutionStepConfig],
        all_step_results: &mut Vec<StepExecutionResult>,
        transitions: &mut Vec<StageTransition>,
        current_stage: &mut String,
        logger: &StepEventLogger,
    ) -> LoopState {
        info!("Running verification phase (iteration {})", ctx.iteration);

        // Persist workflow state: VerificationRunning
        self.persist_workflow_state(
            &config.execution_id,
            &UnifiedWorkflowState::verification_running(ctx.iteration),
        );

        self.record_activity(
            &config.execution_id,
            &format!("verification_start_iter_{}", ctx.iteration),
        );

        self.record_stage_transition(
            &config.execution_id,
            transitions,
            current_stage,
            "verification",
            ctx.iteration,
        );

        // Build effective verification steps: static steps + any dynamically injected steps
        let all_verification_steps = ctx.effective_verification_steps(verification_steps);
        if !ctx.dynamic_steps.is_empty() {
            info!(
                "Verification using {} static + {} dynamic = {} total steps",
                verification_steps.len(),
                ctx.dynamic_steps.len(),
                all_verification_steps.len()
            );
        }

        // Improvement C: Intelligent test selection — skip consistently passing steps
        let mut skipped_consistent_pass: Vec<String> = Vec::new();
        let effective_verification_steps: Vec<ExecutionStepConfig> = if ctx.iteration > 3 {
            let mut filtered = Vec::new();
            for step in &all_verification_steps {
                let step_name = step.name.as_deref().unwrap_or(&step.step_type);
                let history = config.verification_history.get(step_name);
                let should_skip = if let Some(results) = history {
                    // Skip if last 3 consecutive iterations all passed
                    let len = results.len();
                    len >= 3 && results[len - 1] && results[len - 2] && results[len - 3]
                } else {
                    false
                };

                if let (true, Some(results)) = (should_skip, history) {
                    let consecutive_passes =
                        results.iter().rev().take_while(|&&passed| passed).count();
                    info!(
                        "VERIFICATION-SKIP: step '{}' skipped (passed {} consecutive times)",
                        step_name, consecutive_passes
                    );
                    skipped_consistent_pass.push(step_name.to_string());
                } else {
                    filtered.push(step.clone());
                }
            }
            filtered
        } else {
            all_verification_steps.clone()
        };

        if !skipped_consistent_pass.is_empty() {
            info!(
                "VERIFICATION-SELECTION: Running {}/{} steps ({} skipped - consistently passing)",
                effective_verification_steps.len(),
                all_verification_steps.len(),
                skipped_consistent_pass.len()
            );
        }

        let (mut verification_result, step_results) = self
            .verification_executor
            .run_verification(
                &effective_verification_steps,
                &config.execution_id,
                ctx.iteration,
                &config.workflow_name,
                logger,
                config.stage_index,
            )
            .await;

        // Update skipped_steps count to include consistently-passing skips
        verification_result.skipped_steps += skipped_consistent_pass.len();

        // Update verification history with this iteration's results
        for step_result in &verification_result.step_results {
            let entry = config
                .verification_history
                .entry(step_result.step_name.clone())
                .or_default();
            entry.push(step_result.success);
        }
        // Record skipped-as-passed entries too (they're still passing)
        for name in &skipped_consistent_pass {
            let entry = config.verification_history.entry(name.clone()).or_default();
            entry.push(true);
        }

        // Track regression issue step results
        {
            let step_results_clone: Vec<_> = verification_result.step_results.iter()
                .filter_map(|sr| {
                    sr.step_id.as_ref()
                        .and_then(|sid| sid.strip_prefix("regression-"))
                        .map(|issue_id| (issue_id.to_string(), sr.success))
                })
                .collect();
            for (issue_id, success) in &step_results_clone {
                if let Err(e) = self.app_state.pg_db.increment_known_issue_checked(issue_id).await {
                    tracing::debug!("Failed to increment checked for {}: {}", issue_id, e);
                }
                if !*success {
                    if let Err(e) = self.app_state.pg_db.increment_known_issue_detected(issue_id).await {
                        tracing::debug!("Failed to increment detected for {}: {}", issue_id, e);
                    }
                } else {
                    if let Err(e) = self.app_state.pg_db.decay_known_issue_confidence(issue_id).await {
                        tracing::debug!("Failed to decay confidence for {}: {}", issue_id, e);
                    }
                }
            }
        }

        // Add step results to overall results
        all_step_results.extend(step_results);

        // Emit pipeline agent trace for the verification phase
        {
            let trace = crate::autoresearch::agentic_verification::PipelineAgentTrace {
                agent_type: "verification".to_string(),
                agent_id: format!("verifier_iter{}", ctx.iteration),
                run_id: config.execution_id.clone(),
                input_snapshot: serde_json::json!({
                    "iteration": ctx.iteration,
                    "total_steps": verification_result.total_steps,
                }),
                output_snapshot: serde_json::json!({
                    "all_passed": verification_result.all_passed,
                    "passed_steps": verification_result.passed_steps,
                    "failed_steps": verification_result.failed_steps,
                    "critical_failure": verification_result.critical_failure,
                }),
                config: Default::default(),
                duration_ms: 0, // verification timing not separately tracked
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                downstream_success: None, // backfilled after loop completes
                output_quality_score: None,
                parent_span_id: None,
                span_type: "verification".to_string(),
                guardrail_results: vec![],
                handoff_received: None,
            };
            if let Err(e) = crate::database::pipeline_traces::save_pipeline_agent_trace(
                &self.checkpoint_db,
                &config.execution_id,
                &trace,
            ) {
                debug!("Failed to persist verification trace: {}", e);
            }
        }

        LoopState::EvaluateVerification {
            verification_result,
        }
    }

    // =========================================================================
    // Handler: EvaluateVerification
    // =========================================================================

    /// Convergence analysis, resource limits, handle verification_passed / critical_failure,
    /// record knowledge.
    ///
    /// Original lines: 2557-2852
    async fn handle_evaluate_verification(
        &self,
        ctx: &mut LoopContext,
        config: &mut LoopConfig,
        verification_result: VerificationPhaseResult,
    ) -> LoopState {
        // Log verification results to output_log so the summary AI has context
        {
            let status = if verification_result.all_passed {
                "PASSED"
            } else if verification_result.critical_failure {
                "CRITICAL FAILURE"
            } else {
                "FAILED"
            };
            let summary_line = format!(
                "\n--- Verification (Iteration {}): {} ({} passed, {} failed, {} total) ---\n",
                ctx.iteration,
                status,
                verification_result.passed_steps,
                verification_result.failed_steps,
                verification_result.total_steps,
            );

            // Include brief details of failed steps
            let mut details = String::new();
            for sr in &verification_result.step_results {
                if !sr.success {
                    let err = sr.error.as_deref().unwrap_or("unknown error");
                    // Truncate long error messages
                    let truncated = truncate_str(err, 200);
                    details.push_str(&format!("  - {} [FAILED]: {}\n", sr.step_name, truncated));
                }
            }

            let _ = self.app_state.pg_db.append_task_output_ex(
                &config.execution_id,
                &format!("{}{}", summary_line, details),
                false,
                false,
            ).await;
        }

        // Store verification result in database for Recap page
        // Use parent task ID for workflow sequences (same remapping as step checkpoints)
        if let Ok(result_json) = serde_json::to_value(&verification_result) {
            let parent_id = get_parent_task_id(&config.execution_id);
            let _ = self.app_state.pg_db.store_verification_phase_result(
                &parent_id,
                ctx.iteration,
                &result_json,
            ).await;

            // Sync to web backend (best-effort, non-blocking)
            let parent_id_clone = parent_id.clone();
            let result_json_clone = result_json.clone();
            let iteration = ctx.iteration;
            tokio::spawn(async move {
                let sync_service = crate::commands::task_sync::AITaskSyncService::new();
                if let Err(e) = sync_service
                    .sync_verification_result(&parent_id_clone, iteration, &result_json_clone)
                    .await
                {
                    warn!("Failed to sync verification result to backend: {}", e);
                }
            });
        }

        // Emit canvas panel for verification completion
        self.canvas_manager
            .lock()
            .await
            .on_verification_complete(
                ctx.iteration,
                &verification_result,
                &config.verification_history,
            )
            .await;

        // Persist workflow state: VerificationComplete
        self.persist_workflow_state(
            &config.execution_id,
            &UnifiedWorkflowState::verification_complete(
                ctx.iteration,
                verification_result.all_passed,
            ),
        );

        // Run convergence detector and emit metrics for the frontend dashboard
        let convergence_report = {
            let failed_count = verification_result.failed_steps as u32;
            let passed_count = verification_result.passed_steps as u32;
            let skipped_count = verification_result.skipped_steps as u32;

            // Compute current failed step names
            let current_failed_names: std::collections::HashSet<String> = verification_result
                .step_results
                .iter()
                .filter(|sr| !sr.success)
                .map(|sr| sr.step_name.clone())
                .collect();

            // Run the convergence detector
            let report = ctx.convergence_detector.analyze(
                ctx.iteration,
                &config.verification_history,
                current_failed_names.clone(),
                failed_count,
                passed_count,
            );

            // Derive new/repeated from the detector's snapshot history for
            // backward-compatible metrics emission.
            let new_failures = current_failed_names.len() as u32;
            let repeated_failures = 0u32;
            let is_stalled = !report.is_healthy;

            let broadcaster = EventBroadcaster::new(self.app_handle.clone());
            broadcaster.iteration_metrics(
                &config.execution_id,
                ctx.iteration,
                failed_count,
                passed_count,
                skipped_count,
                new_failures,
                repeated_failures,
                is_stalled,
            );

            // Log convergence status
            if report.has_concerns() {
                let pattern_names: Vec<&str> = report
                    .patterns
                    .iter()
                    .map(|p| match p {
                        ConvergencePattern::Stuck { .. } => "stuck",
                        ConvergencePattern::Diverging { .. } => "diverging",
                        ConvergencePattern::Oscillating { .. } => "oscillating",
                        ConvergencePattern::Plateau { .. } => "plateau",
                        ConvergencePattern::Converging => "converging",
                    })
                    .collect();
                warn!(
                    "CONVERGENCE: iteration={}, failed={}, passed={}, patterns={:?}, actions={}",
                    ctx.iteration,
                    failed_count,
                    passed_count,
                    pattern_names,
                    report.actions.len(),
                );
            } else {
                info!(
                    "CONVERGENCE: iteration={}, failed={}, passed={}, healthy=true",
                    ctx.iteration, failed_count, passed_count,
                );
            }

            // Increment cumulative verification failures for routing context
            if !verification_result.all_passed {
                ctx.verification_failures += 1;
            }

            // If escalation was recommended, bump verification_failures to trigger
            // routing rules that use the verification_failures threshold
            if report.should_escalate_model() {
                info!(
                    "CONVERGENCE-ESCALATE: Bumping verification_failures for routing (was {})",
                    ctx.verification_failures
                );
                // Ensure it's at least 3 so routing rules like
                // "verification_failures >= 3 → use opus" fire
                if ctx.verification_failures < 3 {
                    ctx.verification_failures = 3;
                }
            }

            // Check resource limits and merge any actions into the report.
            // Resource actions use the same ConvergenceAction type so the loop
            // controller handles them uniformly with convergence pattern actions.
            let resource_actions = ctx.resource_tracker.check_limits(ctx.iteration);
            if !resource_actions.is_empty() {
                // Merge resource actions into the report
                let mut merged = report;
                merged.is_healthy = false;
                for action in &resource_actions {
                    if let ConvergenceAction::InjectContext { context, .. } = action {
                        if merged.context_injection.is_empty() {
                            merged
                                .context_injection
                                .push_str("\n## Resource Constraints\n\n");
                        }
                        merged.context_injection.push_str(context);
                        merged.context_injection.push('\n');
                    }
                }
                merged.actions.extend(resource_actions);
                merged
            } else {
                report
            }
        };

        // Store current iteration's verification counts for PostAgentic
        ctx.current_passed_checks = verification_result.passed_steps;
        ctx.current_failed_checks = verification_result.failed_steps;

        // Check verification outcome
        if verification_result.all_passed {
            info!("*** VERIFICATION PASSED on iteration {} ***", ctx.iteration);

            // Resolve all unresolved knowledge entries — the issues they describe
            // have been addressed since verification now passes.
            {
                let parent_id = get_parent_task_id(&config.execution_id);
                match self.knowledge_base.get_all_knowledge(&parent_id) {
                    Ok(entries) => {
                        let unresolved: Vec<_> =
                            entries.iter().filter(|k| !k.is_resolved).collect();
                        if !unresolved.is_empty() {
                            info!(
                                "Resolving {} unresolved knowledge entries after verification pass",
                                unresolved.len()
                            );
                            for entry in &unresolved {
                                if let Err(e) = self.knowledge_base.resolve_finding(
                                    &entry.id,
                                    Some("Resolved: verification passed"),
                                ) {
                                    warn!("Failed to resolve knowledge entry {}: {}", entry.id, e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to fetch knowledge entries for resolution: {}", e);
                    }
                }
            }

            let iter_result = IterationResult {
                iteration: ctx.iteration,
                verification_passed: true,
                critical_failure: false,
                passed_checks: verification_result.passed_steps,
                failed_checks: verification_result.failed_steps,
                failure_context: String::new(),
                agentic_phase_ran: false,
                agentic_phase_success: None,
                blame_json: None,
            };
            ctx.iteration_results.push(iter_result);

            return LoopState::Complete {
                reason: CompletionReason::VerificationPassed,
            };
        }

        // Check for critical failure
        if verification_result.critical_failure {
            error!(
                "*** CRITICAL FAILURE on iteration {} - stopping loop ***",
                ctx.iteration
            );

            let iter_result = IterationResult {
                iteration: ctx.iteration,
                verification_passed: false,
                critical_failure: true,
                passed_checks: verification_result.passed_steps,
                failed_checks: verification_result.failed_steps,
                failure_context: verification_result.build_failure_context(),
                agentic_phase_ran: false,
                agentic_phase_success: None,
                blame_json: None,
            };
            ctx.iteration_results.push(iter_result);

            return LoopState::Complete {
                reason: CompletionReason::CriticalFailure {
                    error: verification_result.build_failure_context(),
                },
            };
        }

        // Verification failed (non-critical) — continue to build failure context
        LoopState::BuildFailureContext {
            verification_result,
            convergence_report,
        }
    }

    // =========================================================================
    // Handler: BuildFailureContext
    // =========================================================================

    /// Build failure context string from: verification result, regression, health regression,
    /// constraints, convergence hints, diffs, build errors.
    ///
    /// Original lines: 2854-3095
    async fn handle_build_failure_context(
        &self,
        ctx: &mut LoopContext,
        config: &mut LoopConfig,
        verification_result: VerificationPhaseResult,
        convergence_report: ConvergenceReport,
        transitions: &mut Vec<StageTransition>,
        current_stage: &mut String,
    ) -> LoopState {
        // Verification failed (non-critical) - run agentic phase
        // LOOP CONTINUES: This is the expected path when verification fails
        info!(
            "LOOP-CONTINUE: Verification FAILED (passed={}, failed={}) on iteration {} - will run agentic phase then loop back",
            verification_result.passed_steps, verification_result.failed_steps, ctx.iteration
        );
        info!(
            "LOOP-DEBUG: all_passed={}, critical_failure={}, iteration={}/{} - loop will continue",
            verification_result.all_passed,
            verification_result.critical_failure,
            ctx.iteration,
            config.max_iterations
        );

        let failure_context = verification_result.build_failure_context();

        // Inject convergence analysis into the failure context.
        // The detector's context_injection contains formatted warnings about
        // stuck, diverging, oscillating, or plateau patterns — giving the AI
        // actionable feedback to change its approach.
        let failure_context = if !convergence_report.context_injection.is_empty() {
            format!(
                "{}\n{}",
                convergence_report.context_injection, failure_context
            )
        } else {
            failure_context
        };

        // Record verification feedback as knowledge for cross-iteration context
        {
            let failed_criteria: Vec<String> = verification_result
                .step_results
                .iter()
                .filter(|sr| !sr.success)
                .map(|sr| sr.step_name.clone())
                .collect();

            let parent_id = get_parent_task_id(&config.execution_id);
            if let Err(e) = self.knowledge_base.record_verification_feedback(
                &parent_id,
                ctx.iteration,
                &failure_context,
                &failed_criteria,
            ) {
                warn!(
                    "Failed to record verification feedback as knowledge (iteration {}): {}",
                    ctx.iteration, e
                );
            } else {
                debug!(
                    "Recorded verification feedback knowledge: {} failed criteria (iteration {})",
                    failed_criteria.len(),
                    ctx.iteration
                );
            }
        }

        // Detect regressions from previous iteration (iteration 2+)
        let failure_context = if ctx.iteration > 1 {
            match super::health_monitor::detect_regression(
                &self.checkpoint_db,
                &get_parent_task_id(&config.execution_id),
                ctx.iteration,
                &verification_result,
                &self.app_state.pg_db,
            ) {
                Some(warning) => format!("{}\n\n{}", warning, failure_context),
                None => failure_context,
            }
        } else {
            failure_context
        };

        // Inject environment/health context BEFORE verification failures so the AI
        // sees the environmental framing first. This includes:
        // - Environment doctor results (SDK disconnected, app crashed, etc.)
        // - Health regression warnings from previous iteration's agentic phase
        // By placing this first, the AI knows to fix environment issues before
        // attempting code changes when verification failures are due to env problems.
        let failure_context = if let Some(warning) = ctx.pending_health_regression.take() {
            format!("{}\n\n{}", warning, failure_context)
        } else {
            failure_context
        };

        // Inject constraint violations from the previous agentic phase.
        // These are evaluated after the AI applies changes and stored for the
        // next iteration, so the AI gets feedback about constraint issues
        // alongside verification failures.
        let failure_context = if let Some(constraint_ctx) = ctx.pending_constraint_context.take() {
            format!("{}\n{}", failure_context, constraint_ctx)
        } else {
            failure_context
        };

        // Inject proactive constraints prompt on the first agentic iteration only.
        // This tells the AI about active constraints upfront so it can avoid
        // violations rather than learning about them reactively.
        let failure_context = if !ctx.constraints_prompt_injected {
            if let Some(ref prompt) = ctx.proactive_constraints_prompt {
                ctx.constraints_prompt_injected = true;
                info!(
                    "CONSTRAINT-ENGINE: Injecting proactive constraints prompt into iteration {}",
                    ctx.iteration,
                );
                if failure_context.is_empty() {
                    prompt.clone()
                } else {
                    format!("{}\n\n{}", failure_context, prompt)
                }
            } else {
                failure_context
            }
        } else {
            failure_context
        };

        // Enrich failure context with structured build errors from managed processes.
        // Parses stderr from dev servers to extract actionable errors (file, line, message)
        // instead of dumping raw stderr output.
        let failure_context = {
            let mut enriched = failure_context;
            let mgr_lock = self.app_state.process_capture_manager.lock().await;
            if let Some(ref mgr) = *mgr_lock {
                let statuses = mgr.get_all_status().await;
                let active_processes: Vec<&crate::process_capture::types::ProcessStatus> = statuses
                    .iter()
                    .filter(|s| s.state != crate::process_capture::types::ProcessState::Stopped)
                    .collect();

                let mut analyses = Vec::new();
                for status in &active_processes {
                    if let Ok(lines) = mgr.get_output(&status.id, 100).await {
                        let analysis = crate::process_capture::build_errors::analyze_process_output(
                            &status.name,
                            &lines,
                            status,
                        );
                        analyses.push(analysis);
                    }
                }

                if let Some(build_section) =
                    crate::process_capture::build_errors::format_build_analysis(&analyses)
                {
                    enriched.push_str("\n\n");
                    enriched.push_str(&build_section);
                }
            }
            enriched
        };

        // Persist workflow state: AgenticRunning
        self.persist_workflow_state(
            &config.execution_id,
            &UnifiedWorkflowState::agentic_running(ctx.iteration),
        );

        self.record_stage_transition(
            &config.execution_id,
            transitions,
            current_stage,
            "agentic",
            ctx.iteration,
        );

        // Apply convergence detector actions: force reflection mode if recommended.
        // This temporarily enables reflection_mode for this iteration even if the
        // workflow didn't configure it, giving the AI a structured investigation
        // protocol when it's clearly stuck.
        ctx.reflection_was_forced =
            if convergence_report.should_force_reflection() && !config.reflection_mode {
                info!(
                    "CONVERGENCE-ACTION: Forcing reflection mode for iteration {} (was disabled)",
                    ctx.iteration
                );
                config.reflection_mode = true;
                true
            } else {
                false
            };

        // Inject reflection investigation hint into the failure context if present.
        // This gives the AI specific guidance about what to investigate.
        let failure_context = if let Some(hint) = convergence_report.reflection_hint() {
            format!("{}\n\n{}", failure_context, hint)
        } else {
            failure_context
        };

        // Inject unfixable suggestion into the failure context if recommended.
        // This doesn't force unfixable — it asks the AI to seriously consider it.
        let failure_context = {
            let mut ctx_str = failure_context;
            for action in &convergence_report.actions {
                if let ConvergenceAction::SuggestUnfixable { reason } = action {
                    ctx_str.push_str(&format!(
                        "\n\n### Consider Declaring Unfixable\n\n\
                        {}\n\n\
                        If after thorough investigation you determine these errors truly cannot \
                        be fixed through code changes, output `[UNFIXABLE_ERRORS]` with an \
                        explanation. Only do this if you've exhausted all approaches.\n",
                        reason
                    ));
                }
            }
            ctx_str
        };

        // Run blame attribution engine to attribute failures to specific iterations.
        // This gives the AI targeted, actionable blame context instead of generic failure info.
        let (failure_context, blame_json) = if !ctx.accumulated_diffs.is_empty() {
            let blame_report = super::blame::analyze_blame(
                &verification_result,
                &ctx.accumulated_diffs,
                ctx.iteration,
            );
            let blame_section = super::blame::format_blame_context(&blame_report);
            let blame_json = if !blame_report.attributions.is_empty()
                || !blame_report.oscillating_files.is_empty()
                || !blame_report.revert_patterns.is_empty()
            {
                serde_json::to_string(&blame_report).ok()
            } else {
                None
            };
            if !blame_section.is_empty() {
                (format!("{}\n\n{}", failure_context, blame_section), blame_json)
            } else {
                (failure_context, blame_json)
            }
        } else {
            (failure_context, None)
        };

        // Store blame JSON for later recording in IterationResult
        ctx.blame_json = blame_json.clone();

        // Log structured blame attributions and emit events for observability
        if let Some(ref json) = blame_json {
            if let Ok(report) = serde_json::from_str::<super::blame::BlameReport>(json) {
                for attr in &report.attributions {
                    let files: Vec<&str> = attr.implicated_changes.iter().map(|c| c.file_path.as_str()).collect();
                    warn!(
                        "BLAME iteration={} step={} confidence={:.1} files={} reason={}",
                        attr.blamed_iteration,
                        attr.failed_step,
                        attr.confidence,
                        files.join(","),
                        attr.explanation,
                    );
                }
                for osc in &report.oscillating_files {
                    warn!(
                        "BLAME-OSCILLATION file={} consecutive={} iterations={:?}",
                        osc.file_path, osc.consecutive_blames, osc.blamed_iterations,
                    );
                }
                for rp in &report.revert_patterns {
                    warn!(
                        "BLAME-REVERT file={} iterations={:?}",
                        rp.file_path, rp.iterations,
                    );
                }

                // Emit blame attribution event for frontend dashboard
                let broadcaster = EventBroadcaster::new(self.app_handle.clone());
                broadcaster.blame_attribution(
                    &config.execution_id,
                    ctx.iteration,
                    report.attributions.len() as u32,
                    report.oscillating_files.len() as u32,
                    report.revert_patterns.len() as u32,
                    json,
                );

                // Store blame event in database for API access
                let blame_event = CreateTaskRunEventInput {
                    task_run_id: config.execution_id.clone(),
                    event_type: "blame_attribution".to_string(),
                    event_subtype: None,
                    message: format!(
                        "Blamed {} failure(s) on iteration {} (oscillating: {}, reverts: {})",
                        report.attributions.len(),
                        ctx.iteration,
                        report.oscillating_files.len(),
                        report.revert_patterns.len(),
                    ),
                    data: Some(json.clone()),
                    workflow_name: None,
                    state_name: None,
                    action_id: None,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    duration_ms: None,
                };
                {
                    let pg = self.app_state.pg_db.clone();
                    let event_clone = blame_event.clone();
                    tokio::spawn(async move {
                        if let Err(e) = pg.create_task_run_event(&event_clone).await {
                            tracing::warn!("PG blame event write failed: {}", e);
                        }
                    });
                }

                // Record blame as causal events for cross-run learning.
                {
                    let db_c = self.checkpoint_db.clone();
                    let eid_c = config.execution_id.clone();
                    let wf_c = config.workflow_name.clone();
                    let report_c = report.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        if let Ok(conn) = db_c.connection() {
                            super::blame::record_blame_as_causal_events(
                                &conn,
                                &eid_c,
                                &wf_c,
                                &report_c,
                            );
                        }
                    }).await;
                }
            }
        }

        // Check escalation policy: inject "rethink" meta-prompt if stuck too long
        let failure_context = if ctx.verification_failures >= 3 {
            if config.escalation_policy.should_warn(ctx.verification_failures) {
                warn!(
                    "ESCALATION-WARN: {} iterations without progress (iteration {})",
                    ctx.verification_failures, ctx.iteration,
                );
            }
            if config.escalation_policy.should_rethink(ctx.verification_failures) {
                warn!(
                    "ESCALATION-RETHINK: Injecting rethink meta-prompt after {} fruitless iterations",
                    ctx.verification_failures,
                );
                let rethink = super::blame::EscalationPolicy::rethink_prompt(ctx.verification_failures);
                format!("{}\n{}", failure_context, rethink)
            } else {
                failure_context
            }
        } else {
            failure_context
        };

        // Inject cross-iteration diff context so the AI knows what previous
        // iterations changed. Uses the structured diffs captured by compensation module.
        let failure_context = if !ctx.accumulated_diffs.is_empty() {
            let diff_section =
                compensation::format_iteration_diffs_context(&ctx.accumulated_diffs, 8000);
            if !diff_section.is_empty() {
                format!("{}\n\n{}", failure_context, diff_section)
            } else {
                failure_context
            }
        } else {
            failure_context
        };

        LoopState::RunAgentic {
            failure_context,
            verification_result,
        }
    }

    // =========================================================================
    // Handler: RunAgentic
    // =========================================================================

    /// Capture pre-agentic state, run multi-agent or standard agentic session, restore reflection mode.
    ///
    /// Original lines: 3097-3159
    async fn handle_run_agentic(
        &self,
        ctx: &mut LoopContext,
        config: &mut LoopConfig,
        failure_context: &str,
        has_agentic_steps: bool,
        agentic_steps: &[ExecutionStepConfig],
        verification_steps: &[ExecutionStepConfig],
        verification_result: &VerificationPhaseResult,
        logger: &StepEventLogger,
    ) -> LoopState {
        // Capture HEAD commit before agentic phase for structured diff tracking.
        let working_dir = config.project_path.as_deref().map(std::path::Path::new);
        ctx.commit_before = if let Some(wd) = working_dir {
            compensation::get_head_commit_async(wd).await
        } else {
            None
        };

        // Capture error baseline before agentic phase for regression detection.
        // After the AI makes changes, we compare to identify newly introduced errors.
        ctx.pre_agentic_health = super::health_monitor::fetch_pre_agentic_health_baseline().await;

        ctx.agentic_phase_start = Some(Instant::now());

        // Build verification steps for multi-agent (needs all_verification_steps)
        let all_verification_steps = ctx.effective_verification_steps(verification_steps);

        // Multi-agent mode: triage failures and spawn specialized fix agents
        // instead of one monolithic AI session.
        let (agentic_outcome, new_injected_steps) =
            if config.multi_agent_mode && verification_result.failed_steps > 0 {
                info!(
                    "MULTI-AGENT: Engaging multi-agent fixer (iteration {}, {} failed steps)",
                    ctx.iteration, verification_result.failed_steps
                );

                let ma_result = self
                    .run_multi_agent_fix(
                        config,
                        ctx.iteration,
                        failure_context,
                        verification_result,
                        &all_verification_steps,
                        logger,
                    )
                    .await;

                match ma_result {
                    Some((outcome, steps)) => (outcome, steps),
                    None => {
                        // Multi-agent triage/fix failed, fall back to standard session
                        warn!("MULTI-AGENT: Falling back to standard agentic session");
                        self.agentic_executor
                            .run_agentic(
                                config,
                                ctx.iteration,
                                failure_context,
                                has_agentic_steps,
                                agentic_steps,
                                logger,
                            )
                            .await
                    }
                }
            } else {
                self.agentic_executor
                    .run_agentic(
                        config,
                        ctx.iteration,
                        failure_context,
                        has_agentic_steps,
                        agentic_steps,
                        logger,
                    )
                    .await
            };

        // Restore reflection_mode if we forced it for this iteration only
        if ctx.reflection_was_forced {
            config.reflection_mode = false;
            debug!("CONVERGENCE-ACTION: Restored reflection_mode to false after forced iteration");
        }

        LoopState::PostAgentic {
            outcome: agentic_outcome,
            injected_steps: new_injected_steps,
            failure_context: failure_context.to_string(),
        }
    }

    // =========================================================================
    // Handler: PostAgentic
    // =========================================================================

    /// Process constraints, health regression, tokens, findings, knowledge, canvas, iteration result.
    ///
    /// Original lines: 3161-3571
    async fn handle_post_agentic(
        &self,
        ctx: &mut LoopContext,
        config: &mut LoopConfig,
        agentic_outcome: AgenticOutcome,
        new_injected_steps: Vec<ExecutionStepConfig>,
        failure_context: &str,
        has_agentic_steps: bool,
    ) -> LoopState {
        let agentic_duration_ms = ctx
            .agentic_phase_start
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0);

        // Feed the resource tracker with this iteration's data.
        // Extract files_modified from the parsed agentic output (if available).
        let iteration_files: Vec<String> = agentic_outcome
            .parsed()
            .map(|p| p.files_modified.iter().map(|f| f.path.clone()).collect())
            .unwrap_or_default();
        ctx.resource_tracker
            .record_iteration(agentic_duration_ms, &iteration_files);

        // Run constraint engine against modified files.
        // Violations are stored and injected into the NEXT iteration's failure
        // context (alongside verification results). This catches issues like
        // secrets, debug statements, or scope violations early.
        {
            if !iteration_files.is_empty() {
                let constraint_results = ctx.constraint_engine.evaluate(&iteration_files);
                let all_passed = constraint_results.iter().all(|r| r.passed);
                let has_blocking = constraint_results.iter().any(|r| {
                    !r.passed && r.severity == crate::constraint_engine::ConstraintSeverity::Block
                });
                let summary = if all_passed {
                    format!("all {} constraints passed", constraint_results.len())
                } else {
                    crate::constraint_engine::ConstraintEngine::summarize_results(
                        &constraint_results,
                    )
                };

                // Persist constraint results to database for post-run review
                let parent_id = get_parent_task_id(&config.execution_id);
                let constraint_store_result = self.app_state.pg_db.store_constraint_results(
                    &parent_id,
                    ctx.iteration,
                    &constraint_results,
                ).await;
                if let Err(e) = constraint_store_result {
                    warn!(
                        "Failed to store constraint results: {} - continuing anyway",
                        e
                    );
                }

                // Emit constraint results to the frontend
                let broadcaster = EventBroadcaster::new(self.app_handle.clone());
                let serialized_results = serde_json::to_value(&constraint_results)
                    .unwrap_or_else(|_| serde_json::json!([]));
                broadcaster.constraint_results(
                    &config.execution_id,
                    ctx.iteration,
                    &summary,
                    has_blocking,
                    serialized_results,
                );

                if !all_passed {
                    info!(
                        "CONSTRAINT-ENGINE: iteration {} — {}",
                        ctx.iteration, summary
                    );
                    let actions = crate::constraint_engine::ConstraintEngine::results_to_actions(
                        &constraint_results,
                    );
                    // Build context injection from constraint actions
                    let mut constraint_ctx = String::new();
                    for action in &actions {
                        if let ConvergenceAction::InjectContext { context, .. } = action {
                            constraint_ctx.push_str(context);
                            constraint_ctx.push('\n');
                        }
                    }
                    if !constraint_ctx.is_empty() {
                        ctx.pending_constraint_context =
                            Some(format!("\n## Constraint Violations\n\n{}", constraint_ctx));
                    }
                } else {
                    debug!(
                        "CONSTRAINT-ENGINE: iteration {} — all constraints passed",
                        ctx.iteration
                    );
                }
            }
        }

        // Compare post-agentic health with baseline to detect regressions.
        // Store for the NEXT iteration's failure context (since this iteration's
        // failure context was already built from verification results).
        if agentic_outcome.is_success() {
            ctx.pending_health_regression =
                super::health_monitor::detect_health_regression(&ctx.pre_agentic_health).await;
            if ctx.pending_health_regression.is_some() {
                warn!(
                    "HEALTH-REGRESSION: Detected after agentic phase (iteration {})",
                    ctx.iteration
                );
            }
        }

        // Accumulate any newly injected steps for future verification iterations
        let new_injected_count = new_injected_steps.len();
        let had_ui_bridge_actions = new_injected_steps.iter().any(|s| {
            s.step_type == "ui_bridge"
                && matches!(
                    s.ui_bridge_action.as_deref(),
                    Some("action_plan") | Some("execute")
                )
        });
        if !new_injected_steps.is_empty() {
            info!(
                "Injected {} dynamic verification step(s) from agentic phase (iteration {})",
                new_injected_count, ctx.iteration
            );
            ctx.dynamic_steps.extend(new_injected_steps);
        }

        // Goal verification: after successful agentic iterations that included
        // UI Bridge actions, inject a snapshot-based verification step so the
        // next verification phase can confirm the UI state matches the goal.
        // This catches false-positive completions without requiring workflow
        // authors to write explicit assertions.
        if had_ui_bridge_actions && agentic_outcome.is_success() {
            let goal_desc = agentic_outcome
                .parsed()
                .map(|p| p.summary.clone())
                .unwrap_or_else(|| "Verify UI state after agentic actions".to_string());

            // Remove any previous goal-verification-snapshot to avoid accumulation
            ctx.dynamic_steps
                .retain(|s| s.name.as_deref() != Some("goal-verification-snapshot"));

            let verify_step = ExecutionStepConfig {
                step_type: "ui_bridge".to_string(),
                name: Some("goal-verification-snapshot".to_string()),
                ui_bridge_action: Some("snapshot".to_string()),
                ui_bridge_instruction: Some(truncate_str(&goal_desc, 200).to_string()),
                // Inherit URL from the first ui_bridge step in dynamic_steps
                ui_bridge_url: ctx
                    .dynamic_steps
                    .iter()
                    .find(|s| s.step_type == "ui_bridge")
                    .and_then(|s| s.ui_bridge_url.clone()),
                ..Default::default()
            };
            info!(
                "GOAL-VERIFY: Injecting post-agentic snapshot step (iteration {})",
                ctx.iteration
            );
            ctx.dynamic_steps.push(verify_step);
        }

        // Persist workflow state: AgenticComplete
        self.persist_workflow_state(
            &config.execution_id,
            &UnifiedWorkflowState::agentic_complete(ctx.iteration),
        );

        // Compute token usage for the agentic phase trace
        let (mut trace_tokens_in, mut trace_tokens_out) =
            super::multi_agent_pipeline_loop::query_iteration_tokens(
                &self.checkpoint_db,
                &self.app_state.pg_db,
                &config.execution_id,
                ctx.iteration,
            );
        if trace_tokens_in == 0 && trace_tokens_out == 0 {
            let (ot_in, ot_out) = agentic_outcome.token_usage();
            trace_tokens_in = ot_in.unwrap_or(0);
            trace_tokens_out = ot_out.unwrap_or(0);
        }
        let trace_model = config
            .resolve_model_for_phase("agentic")
            .unwrap_or_else(|| "claude-cli".to_string());
        let trace_cost =
            crate::ai_pricing::calculate_cost_usd(trace_tokens_in, trace_tokens_out, &trace_model);

        // Emit pipeline agent trace for the agentic phase (populates
        // pipeline_agent_traces for meta-optimizer analysis — works for
        // traditional architecture, not just MultiAgentPipeline).
        {
            let trace = crate::autoresearch::agentic_verification::PipelineAgentTrace {
                agent_type: "agentic_fixer".to_string(),
                agent_id: format!("fixer_iter{}", ctx.iteration),
                run_id: config.execution_id.clone(),
                input_snapshot: serde_json::json!({
                    "iteration": ctx.iteration,
                    "failure_context_len": failure_context.len(),
                    "has_agentic_steps": has_agentic_steps,
                }),
                output_snapshot: serde_json::json!({
                    "success": agentic_outcome.is_success(),
                    "files_modified": &iteration_files,
                }),
                config: Default::default(),
                duration_ms: agentic_duration_ms,
                tokens_in: trace_tokens_in as u32,
                tokens_out: trace_tokens_out as u32,
                cost_usd: trace_cost,
                downstream_success: None, // backfilled after loop completes
                output_quality_score: None,
                parent_span_id: None,
                span_type: "agent".to_string(),
                guardrail_results: vec![],
                handoff_received: None,
            };
            if let Err(e) = crate::database::pipeline_traces::save_pipeline_agent_trace(
                &self.checkpoint_db,
                &config.execution_id,
                &trace,
            ) {
                debug!("Failed to persist agentic trace: {}", e);
            }
        }

        // Log agentic output to database
        // CRITICAL: Use append_task_output_ex with check_completion_marker=false
        // The AI may output [TASK_COMPLETE] but that does NOT mean verification passed!
        // In unified workflows, only verification passing can mark the task complete.
        // ALWAYS log and increment session count, even for Error outcomes, to prevent
        // the task from getting stuck with sessions_count=0 and status="running".
        {
            let output_text = match agentic_outcome.output() {
                Some(text) => {
                    format!(
                        "\n--- AI Output (Iteration {}) ---\n{}\n",
                        ctx.iteration, text
                    )
                }
                None => format!(
                    "\n--- AI Output (Iteration {}) ---\n(no output - skipped)\n",
                    ctx.iteration
                ),
            };
            let _ = self.app_state.pg_db.append_task_output_ex(
                &config.execution_id,
                &output_text,
                true,
                false,
            ).await;
        }

        // Sync session to web backend (best-effort, non-blocking).
        // The frontend-driven sync only fires for UI-initiated tasks; workflow-
        // executor sessions must sync themselves to keep the backend up to date.
        {
            let exec_id = config.execution_id.clone();
            let session_num = ctx.iteration;
            let duration_secs = (agentic_duration_ms / 1000) as i64;
            let output_summary = agentic_outcome
                .output()
                .map(|o| truncate_str(o, 5000).to_string());
            tokio::spawn(async move {
                let sync_service = crate::commands::task_sync::AITaskSyncService::new();
                // Start
                if let Err(e) = sync_service
                    .sync_session_started(&exec_id, session_num)
                    .await
                {
                    debug!("Failed to sync session start to backend: {}", e);
                }
                // End
                if let Err(e) = sync_service
                    .sync_session_ended(
                        &exec_id,
                        session_num,
                        duration_secs,
                        output_summary.as_deref(),
                    )
                    .await
                {
                    debug!("Failed to sync session end to backend: {}", e);
                }
            });
        }

        // Record findings from AI output as knowledge entries
        let parsed_findings = if let Some(output) = agentic_outcome.output() {
            let findings = parse_findings_from_output(output);
            if !findings.is_empty() {
                let parent_id = get_parent_task_id(&config.execution_id);
                info!(
                    "Parsed {} finding(s) from agentic output (iteration {})",
                    findings.len(),
                    ctx.iteration
                );
                for finding in &findings {
                    if let Err(e) =
                        self.knowledge_base
                            .record_finding(&parent_id, finding, ctx.iteration)
                    {
                        warn!("Failed to record finding as knowledge: {}", e);
                    }
                }
            }

            // Record agentic outcome as an observation
            let parent_id = get_parent_task_id(&config.execution_id);
            let observation = match &agentic_outcome {
                AgenticOutcome::Success { .. } => {
                    format!(
                        "Iteration {}: Agentic phase completed successfully ({} chars of output)",
                        ctx.iteration,
                        output.len()
                    )
                }
                AgenticOutcome::Failed { error, .. } => {
                    format!(
                        "Iteration {}: Agentic phase failed: {}",
                        ctx.iteration, error
                    )
                }
                AgenticOutcome::Error { error } => {
                    format!(
                        "Iteration {}: Agentic phase error: {}",
                        ctx.iteration, error
                    )
                }
                AgenticOutcome::Skipped => String::new(),
            };
            if !observation.is_empty() {
                if let Err(e) = self.knowledge_base.record_observation(
                    &parent_id,
                    AgentType::Worker,
                    ctx.iteration,
                    &observation,
                    &[],
                ) {
                    warn!("Failed to record agentic observation: {}", e);
                }
            }

            findings
        } else {
            Vec::new()
        };

        // Parse AI-discovered constraint proposals from the agentic output.
        // Proposals are applied to the constraint engine for subsequent iterations
        // and logged as TOML for the user to add to constraints.toml.
        if let Some(output) = agentic_outcome.output() {
            let proposals = crate::constraint_engine::proposals::parse_proposals(output);
            if !proposals.is_empty() {
                let applied = ctx.constraint_engine.apply_proposals(&proposals);
                info!(
                    "CONSTRAINT-PROPOSAL: Applied {}/{} proposal(s) from iteration {}",
                    applied,
                    proposals.len(),
                    ctx.iteration,
                );
                // Log the TOML snippet for user reference
                let toml_snippet =
                    crate::constraint_engine::proposals::proposals_to_toml(&proposals);
                if !toml_snippet.is_empty() {
                    info!(
                        "CONSTRAINT-PROPOSAL: Suggested constraints.toml additions:\n{}",
                        toml_snippet,
                    );
                }
            }
        }

        // Emit canvas panel for agentic phase completion
        self.canvas_manager
            .lock()
            .await
            .on_agentic_complete(
                ctx.iteration,
                &agentic_outcome,
                &parsed_findings,
                new_injected_count,
                agentic_duration_ms,
            )
            .await;

        let iter_result = IterationResult {
            iteration: ctx.iteration,
            verification_passed: false,
            critical_failure: false,
            passed_checks: ctx.current_passed_checks,
            failed_checks: ctx.current_failed_checks,
            failure_context: failure_context.to_string(),
            agentic_phase_ran: !matches!(agentic_outcome, AgenticOutcome::Skipped),
            agentic_phase_success: Some(agentic_outcome.is_success()),
            blame_json: ctx.blame_json.take(),
        };
        ctx.iteration_results.push(iter_result);

        // Clear failure_context from previous iterations to prevent unbounded
        // memory growth. The context has already been persisted to the knowledge
        // base (via record_verification_feedback) and used for the agentic prompt.
        // Downstream consumers only use passed_checks/failed_checks/verification_passed.
        let results_len = ctx.iteration_results.len();
        if results_len > 1 {
            for old in &mut ctx.iteration_results[..results_len - 1] {
                if !old.failure_context.is_empty() {
                    old.failure_context = String::new();
                }
            }
        }

        // Log agentic outcome for debugging (including parsed confidence)
        info!(
            "AGENTIC-OUTCOME: iteration={}, outcome={}, confidence={}",
            ctx.iteration,
            match &agentic_outcome {
                AgenticOutcome::Success { .. } => "Success",
                AgenticOutcome::Failed { .. } => "Failed",
                AgenticOutcome::Error { error } => {
                    error!("AGENTIC-OUTCOME: Error details: {}", error);
                    "Error"
                }
                AgenticOutcome::Skipped => "Skipped",
            },
            agentic_outcome
                .parsed()
                .and_then(|p| p.confidence)
                .map(|c| format!("{:.0}%", c * 100.0))
                .unwrap_or_else(|| "N/A".to_string()),
        );

        // Log and store findings from the parsed output, if any
        if let Some(parsed) = agentic_outcome.parsed() {
            if !parsed.findings.is_empty() {
                info!(
                    "AGENTIC-FINDINGS: iteration={}, count={}, titles=[{}]",
                    ctx.iteration,
                    parsed.findings.len(),
                    parsed
                        .findings
                        .iter()
                        .map(|f| format!("{}({})", f.title, f.severity))
                        .collect::<Vec<_>>()
                        .join(", "),
                );

                // Store findings as a task_run_event for later retrieval
                let findings_data = serde_json::json!({
                    "phase": "agentic",
                    "iteration": ctx.iteration,
                    "findings": parsed.findings,
                });
                let findings_event = CreateTaskRunEventInput {
                    task_run_id: config.execution_id.clone(),
                    event_type: "agentic_findings".to_string(),
                    event_subtype: None,
                    message: format!(
                        "Agentic phase reported {} finding(s) on iteration {}",
                        parsed.findings.len(),
                        ctx.iteration,
                    ),
                    data: Some(serde_json::to_string(&findings_data).unwrap_or_default()),
                    workflow_name: None,
                    state_name: None,
                    action_id: None,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    duration_ms: None,
                };
                if let Err(e) = self.app_state.pg_db.create_task_run_event(&findings_event).await {
                    warn!("Failed to store agentic findings event (PG): {}", e);
                }
            }
        }

        LoopState::CheckPostAgenticSignals {
            outcome: agentic_outcome,
        }
    }

    // =========================================================================
    // Handler: CheckPostAgenticSignals
    // =========================================================================

    /// Check unfixable errors, stop signals, pause.
    ///
    /// Original lines: 3574-3654
    async fn handle_check_post_agentic_signals(
        &self,
        ctx: &mut LoopContext,
        config: &mut LoopConfig,
        outcome: &AgenticOutcome,
    ) -> LoopState {
        // Check if the AI signaled unfixable errors.
        // Prefer the structured `parsed` output, fall back to raw marker check.
        let is_unfixable = if let Some(parsed) = outcome.parsed() {
            parsed.unfixable
        } else if let Some(output) = outcome.output() {
            output.contains("[UNFIXABLE_ERRORS]") || output.contains("[UNFIXABLE_ERROR]")
        } else {
            false
        };
        if is_unfixable {
            let reason = outcome
                .parsed()
                .and_then(|p| p.unfixable_reason.as_deref())
                .unwrap_or("(no reason provided)");
            warn!(
                "AI signaled unfixable errors on iteration {} - exiting loop gracefully. Reason: {}",
                ctx.iteration, reason
            );

            // Log the unfixable signal to the task output
            let unfixable_msg = format!(
                "\n=== AI SIGNALED UNFIXABLE ERRORS ===\nThe AI has determined that some errors cannot be fixed automatically.\nReason: {}\nProceeding to completion phase.\n",
                reason
            );
            let _ = self.app_state.pg_db.append_task_output_ex(
                &config.execution_id,
                &unfixable_msg,
                false,
                false,
            ).await;

            return LoopState::Complete {
                reason: CompletionReason::UnfixableErrors,
            };
        }

        // Check if the task was stopped during the agentic phase
        if self.is_task_stopped(&config.execution_id) {
            warn!("Task was stopped during agentic phase - exiting loop");
            return LoopState::Complete {
                reason: CompletionReason::Stopped,
            };
        }

        // Wait if paused after agentic phase
        self.wait_while_paused(&config.execution_id).await;

        // Re-check stop after unpause
        if self.is_task_stopped(&config.execution_id) {
            warn!("Task was stopped while paused (post-agentic) - exiting loop");
            return LoopState::Complete {
                reason: CompletionReason::Stopped,
            };
        }

        LoopState::ApprovalGate {
            outcome: outcome.clone(),
        }
    }

    // =========================================================================
    // Handler: ApprovalGate
    // =========================================================================

    /// If approval enabled: build diff context, request approval, poll for response, handle abort/reject.
    ///
    /// Original lines: 3656-3897
    async fn handle_approval_gate(
        &self,
        ctx: &mut LoopContext,
        config: &mut LoopConfig,
        outcome: &AgenticOutcome,
    ) -> LoopState {
        // APPROVAL GATE (optional human-in-the-loop pause)
        // If the workflow has approval_gate enabled, or if the AI output
        // contains the [APPROVAL_GATE] marker, pause for human review.
        let needs_approval = config.approval_gate
            || outcome
                .output()
                .map(|o| o.contains("[APPROVAL_GATE]"))
                .unwrap_or(false);

        if needs_approval && outcome.is_success() {
            info!(
                "Approval gate triggered on iteration {} - pausing for human review",
                ctx.iteration
            );

            // Collect context for the reviewer
            let diff_stat_for_approval = match crate::process_helpers::tokio_no_window("git")
                .args(["diff", "--stat"])
                .output()
                .await
            {
                Ok(o) if o.status.success() => {
                    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                }
                _ => None,
            };

            let diff_for_approval = match crate::process_helpers::tokio_no_window("git")
                .args(["diff"])
                .output()
                .await
            {
                Ok(o) if o.status.success() => {
                    let raw = String::from_utf8_lossy(&o.stdout).to_string();
                    if raw.is_empty() {
                        None
                    } else if raw.len() > 8000 {
                        let t = truncate_str(&raw, 8000);
                        Some(format!(
                            "{}...\n[truncated, {} more chars]",
                            t,
                            raw.len() - t.len()
                        ))
                    } else {
                        Some(raw)
                    }
                }
                _ => None,
            };

            // Build the approval request
            let approval_id = format!("approval-{}-iter-{}", config.execution_id, ctx.iteration);
            let summary = outcome
                .parsed()
                .map(|p| p.summary.clone())
                .unwrap_or_else(|| {
                    format!("Agentic phase completed (iteration {})", ctx.iteration)
                });
            let files_modified = outcome
                .parsed()
                .map(|p| {
                    p.files_modified
                        .iter()
                        .map(|f| f.path.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            let context = super::approval::ApprovalContext {
                summary: summary.clone(),
                files_modified: files_modified.clone(),
                git_diff_stat: diff_stat_for_approval.clone(),
                git_diff: diff_for_approval.clone(),
            };

            let request = super::approval::ApprovalRequest {
                id: approval_id.clone(),
                execution_id: config.execution_id.clone(),
                iteration: ctx.iteration,
                prompt: format!(
                    "The AI has completed iteration {}. Review the changes and approve to continue.",
                    ctx.iteration
                ),
                context: context.clone(),
                options: vec![
                    "Approve".to_string(),
                    "Reject".to_string(),
                    "Abort Workflow".to_string(),
                ],
                created_at: chrono::Utc::now().to_rfc3339(),
            };

            // Record to database for audit trail
            let context_json = serde_json::to_string(&context).unwrap_or_else(|_| "{}".to_string());
            let _ = self.app_state.pg_db.insert_approval_gate(
                &approval_id,
                &config.execution_id,
                ctx.iteration as i32,
                &request.prompt,
                &context_json,
            ).await;

            // Persist workflow state: ApprovalPending
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::approval_pending(
                    ctx.iteration,
                    config.stage_index,
                    approval_id.clone(),
                    request.prompt.clone(),
                ),
            );

            // Register with the in-memory approval registry
            let registry = super::approval::get_approval_registry();
            let receiver = registry.register(request).await;

            // Emit event to notify the frontend
            let broadcaster = EventBroadcaster::new(self.app_handle.clone());
            broadcaster.approval_required(
                &config.execution_id,
                &approval_id,
                ctx.iteration,
                &format!("Review changes from iteration {}", ctx.iteration),
            );

            // Log the pause
            let _ = self.app_state.pg_db.append_task_output_ex(
                &config.execution_id,
                &format!(
                    "\n=== APPROVAL GATE (Iteration {}) ===\nWaiting for human review...\n",
                    ctx.iteration
                ),
                false,
                false,
            ).await;

            // Wait for the human response (or stop signal)
            let approval_response = tokio::select! {
                resp = receiver => {
                    match resp {
                        Ok(r) => r,
                        Err(_) => {
                            warn!("Approval receiver dropped - treating as abort");
                            super::approval::ApprovalResponse {
                                approved: false,
                                action: "abort".to_string(),
                                comment: Some("Approval channel closed unexpectedly".to_string()),
                            }
                        }
                    }
                }
                _ = async {
                    // Poll for stop signal while waiting for approval
                    loop {
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        if self.is_task_stopped(&config.execution_id) {
                            return;
                        }
                    }
                } => {
                    // Task was stopped while waiting for approval
                    warn!("Task stopped while waiting for approval - aborting");
                    // Cancel the pending approval
                    registry.cancel_all_for_execution(&config.execution_id).await;
                    super::approval::ApprovalResponse {
                        approved: false,
                        action: "abort".to_string(),
                        comment: Some("Task was stopped".to_string()),
                    }
                }
            };

            // Record the response to the database
            let status = match approval_response.action.as_str() {
                "approve" => "approved",
                "reject" => "rejected",
                "abort" => "aborted",
                other => other,
            };
            let _ = self.app_state.pg_db.resolve_approval_gate(
                &approval_id,
                &approval_response.action,
                approval_response.comment.as_deref(),
                status,
            ).await;

            // Emit resolved event
            broadcaster.approval_resolved(
                &config.execution_id,
                &approval_id,
                approval_response.approved,
                &approval_response.action,
            );

            // Log the decision
            let _ = self.app_state.pg_db.append_task_output_ex(
                &config.execution_id,
                &format!(
                    "Approval decision: {} (comment: {})\n",
                    approval_response.action,
                    approval_response.comment.as_deref().unwrap_or("none")
                ),
                false,
                false,
            ).await;

            // Handle the response
            if approval_response.action == "abort" {
                warn!(
                    "Workflow aborted via approval gate on iteration {}",
                    ctx.iteration
                );
                return LoopState::Complete {
                    reason: CompletionReason::ApprovalAborted,
                };
            }
            if !approval_response.approved {
                info!(
                    "Changes rejected on iteration {} - AI will retry",
                    ctx.iteration
                );
                // Continue to verification, which will likely fail,
                // prompting the AI to try a different approach
            }

            // Restore workflow state to agentic_complete so normal flow continues
            self.persist_workflow_state(
                &config.execution_id,
                &UnifiedWorkflowState::agentic_complete(ctx.iteration),
            );
        }

        LoopState::AdvanceIteration
    }

    // =========================================================================
    // Handler: AdvanceIteration
    // =========================================================================

    /// Record compensation commit, capture iteration diff, record git observation.
    ///
    /// Original lines: 3899-4023
    async fn handle_advance_iteration(
        &self,
        ctx: &mut LoopContext,
        config: &mut LoopConfig,
    ) -> LoopState {
        // Capture git diff after agentic phase for cross-iteration context.
        // This helps the AI understand what it changed in the previous iteration.
        // Also captures structured IterationDiff and records commit checkpoint
        // for the compensation module (Conductor-inspired durable execution).
        {
            let parent_id = get_parent_task_id(&config.execution_id);
            let working_dir = config.project_path.as_deref().map(std::path::Path::new);

            // Capture HEAD after agentic phase for commit checkpoint
            let commit_after = if let Some(wd) = working_dir {
                compensation::get_head_commit_async(wd).await
            } else {
                None
            };

            // Record commit checkpoint for this iteration (enables rollback)
            if let Some(ref hash) = commit_after {
                if let Err(e) = ctx.compensation_manager.record_iteration_commit(
                    &config.execution_id,
                    ctx.iteration,
                    hash,
                ) {
                    warn!(
                        "COMPENSATION: Failed to record iteration {} commit: {}",
                        ctx.iteration, e
                    );
                } else {
                    debug!(
                        "COMPENSATION: Recorded commit {} for iteration {}",
                        &hash[..hash.len().min(8)],
                        ctx.iteration
                    );
                }
            }

            // Capture structured iteration diff (uses commit_before from pre-agentic capture)
            if let Some(wd) = working_dir {
                if let Some(diff) = compensation::capture_iteration_diff(
                    wd,
                    ctx.iteration,
                    ctx.commit_before.as_deref(),
                    commit_after.as_deref(),
                )
                .await
                {
                    // Persist to database
                    if let Err(e) = self
                        .checkpoint_db
                        .append_iteration_diff(&config.execution_id, &diff)
                    {
                        warn!(
                            "COMPENSATION: Failed to persist iteration diff {}: {}",
                            ctx.iteration, e
                        );
                    }

                    // Keep in memory for cross-iteration context injection
                    ctx.accumulated_diffs.push(diff);
                }
            }

            // Legacy: also record as knowledge base observation for backward compat
            match crate::process_helpers::tokio_no_window("git")
                .args(["diff", "--stat"])
                .output()
                .await
            {
                Ok(output) if output.status.success() => {
                    let diff_stat = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !diff_stat.is_empty() {
                        let full_diff = match crate::process_helpers::tokio_no_window("git")
                            .args(["diff"])
                            .output()
                            .await
                        {
                            Ok(d) if d.status.success() => {
                                let raw = String::from_utf8_lossy(&d.stdout).to_string();
                                if raw.len() > 8000 {
                                    let t = truncate_str(&raw, 8000);
                                    format!(
                                        "{}...\n[truncated, {} more chars]",
                                        t,
                                        raw.len() - t.len()
                                    )
                                } else {
                                    raw
                                }
                            }
                            _ => String::new(),
                        };

                        let observation = format!(
                            "Git changes after iteration {}:\n{}\n\n{}",
                            ctx.iteration, diff_stat, full_diff
                        );
                        if let Err(e) = self.knowledge_base.record_observation(
                            &parent_id,
                            AgentType::Worker,
                            ctx.iteration,
                            &observation,
                            &[],
                        ) {
                            warn!(
                                "Failed to record git diff observation (iteration {}): {}",
                                ctx.iteration, e
                            );
                        } else {
                            debug!(
                                "Recorded git diff as knowledge observation (iteration {})",
                                ctx.iteration
                            );
                        }
                    }
                }
                _ => {
                    debug!("Git diff capture skipped (git not available or not in repo)");
                }
            }
        }

        info!(
            "LOOP-CONTINUE: Iteration {} complete - looping back to verification (next iteration: {})",
            ctx.iteration, ctx.iteration + 1
        );

        // Loop back to the top — next iteration starts with precondition checks
        LoopState::CheckPreconditions
    }
}
