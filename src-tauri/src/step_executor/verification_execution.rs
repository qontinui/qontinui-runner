//! Verification step execution methods for StepExecutor.
//!
//! Extracted from `executor.rs` — contains the verification phase loop
//! that runs all verification steps and produces a `VerificationPhaseResult`.

use tracing::{info, warn};

use crate::step_event_builder::StepEventBuilder;
use crate::step_metadata::{StepDetails, StepMetadata};
use crate::step_types::StepType;
use crate::test_executor::TestStatus;
use crate::unified_workflow_executor::get_parent_task_id;
use crate::workflow_state::{CheckpointManager, StepCheckpoint};

use super::executor::StepExecutor;
use super::executor_types::*;
use super::handlers::StepHandler;
use super::verification_context::{categorize_failure, extract_text_from_output_data};

impl StepExecutor {
    /// Execute all verification steps and return a VerificationPhaseResult
    ///
    /// This is the main entry point for the verification phase in the
    /// verification-agentic loop. It:
    /// 1. Executes each verification step in order
    /// 2. Captures detailed results for each step
    /// 3. Stops on critical step failure
    /// 4. Returns a summary that can be used to build AI context
    #[tracing::instrument(
        name = "workflow.verification.execute",
        skip(self, steps),
        fields(
            step_count = %steps.len(),
            execution_id = %execution_id,
            iteration = %iteration
        )
    )]
    pub async fn execute_verification_steps(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
    ) -> VerificationPhaseResult {
        self.execute_verification_steps_with_events(steps, execution_id, iteration, None)
            .await
    }

    /// Run verification phase steps with optional event emission.
    ///
    /// This version emits completion events as each step finishes, allowing
    /// the UI to show real-time progress instead of waiting until all steps complete.
    #[tracing::instrument(
        name = "workflow.verification.with_events",
        skip(self, steps),
        fields(
            step_count = %steps.len(),
            execution_id = %execution_id,
            iteration = %iteration,
            workflow_name = ?workflow_name
        )
    )]
    pub async fn execute_verification_steps_with_events(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
        workflow_name: Option<&str>,
    ) -> VerificationPhaseResult {
        use std::time::Instant;

        // For workflow sequence children, use parent ID for event logging (FK constraint)
        let event_execution_id = get_parent_task_id(execution_id);
        let start = Instant::now();
        let mut step_results = Vec::new();
        let mut passed_steps = 0;
        let mut failed_steps = 0;
        let mut skipped_steps = 0;
        let critical_failure = false;

        // Filter to only verification phase steps
        let verification_steps: Vec<_> = steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("verification"))
            .collect();

        info!(
            "Executing {} verification steps for iteration {}",
            verification_steps.len(),
            iteration
        );

        // Track whether a navigation step has been seen, so we can auto-inject
        // retries for subsequent SDK steps (WebSocket reconnection takes ~15s).
        let mut after_navigation = false;

        for (index, step) in verification_steps.iter().enumerate() {
            // Skip remaining steps if we had a critical failure
            if critical_failure {
                let skipped_at = chrono::Utc::now().to_rfc3339();
                let result = StepExecutionResult {
                    step_index: index,
                    step_name: step
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("Step {}", index + 1)),
                    step_type: step.step_type.clone(),
                    step_id: step.id.clone(),
                    success: false,
                    error: Some("Skipped due to critical failure".to_string()),
                    screenshot_path: None,
                    started_at: Some(skipped_at.clone()),
                    ended_at: Some(skipped_at),
                    duration_ms: 0,
                    config: StepExecutionConfig {
                        timeout_seconds: None,
                        check_type: None,
                        command: None,
                        test_id: step.test_id.clone(),
                        test_type: step.test_type.clone(),
                        working_directory: None,
                        ui_bridge_action: None,
                    },
                    verification_details: None,
                    output_data: None,
                    required: step.required,
                    resolved_inputs: None,
                    extracted_values: None,
                    failure_category: None,
                    interrupted: None,
                };
                step_results.push(result);
                skipped_steps += 1;
                continue;
            }

            let step_start = Instant::now();
            let step_started_at = chrono::Utc::now().to_rfc3339();
            let step_name = step
                .name
                .clone()
                .unwrap_or_else(|| format!("Step {}", index + 1));

            // Runtime command sanitization: replace jq with python (jq unavailable on Windows MSYS)
            // This is a safety net in case the hardener didn't process the workflow at generation time.
            let step = if step.step_type == "command" || step.step_type == "shell" {
                if let Some(ref cmd) = step.shell_command {
                    if cmd.contains("| jq ") {
                        let sanitized = super::handlers::shell_command::ShellCommandHandler::replace_jq_with_python_static(cmd);
                        if sanitized != *cmd {
                            info!("Verification executor: jq→python replacement applied for step '{}'", step_name);
                            let mut patched = (*step).clone();
                            patched.shell_command = Some(sanitized);
                            std::borrow::Cow::Owned(patched)
                        } else {
                            std::borrow::Cow::Borrowed(*step)
                        }
                    } else {
                        std::borrow::Cow::Borrowed(*step)
                    }
                } else {
                    std::borrow::Cow::Borrowed(*step)
                }
            } else {
                std::borrow::Cow::Borrowed(*step)
            };
            let step = step.as_ref();

            // Track navigation steps for auto-retry injection
            let cmd_str = step.shell_command.as_deref().unwrap_or("");
            if cmd_str.contains("sdk/page/navigate") {
                after_navigation = true;
            }

            // Determine retry configuration: explicit from step config, or auto-inject
            // for SDK steps that follow a navigation step (WebSocket reconnection delay).
            let (max_retries, retry_delay) = if step.retry_count.is_some() {
                // Explicit retry config takes precedence
                (
                    step.retry_count.unwrap_or(0),
                    step.retry_delay_ms.unwrap_or(2000),
                )
            } else if after_navigation
                && cmd_str.contains("ui-bridge/sdk/")
                && !cmd_str.contains("sdk/page/navigate")
            {
                // Auto-inject retries for SDK verification steps after page navigation.
                // After navigation, the WebSocket connection needs time to reconnect (~15s).
                info!(
                    "Auto-injecting retries for SDK step '{}' after navigation",
                    step_name
                );
                (3_u32, 3000_u64)
            } else {
                (0, 2000)
            };

            // Stop auto-retry injection after hitting a non-SDK step
            if after_navigation
                && !cmd_str.is_empty()
                && !cmd_str.contains("ui-bridge/sdk/")
                && !cmd_str.contains("sdk/page/navigate")
            {
                after_navigation = false;
            }

            // Execute with retry loop
            let (mut success, mut error, mut verification_details, step_output_data) = {
                let mut last_result = (false, Some("not executed".to_string()), None, None);
                for attempt in 0..=max_retries {
                    if attempt > 0 {
                        info!(
                            "Retrying verification step '{}' (attempt {}/{}, delay {}ms)",
                            step_name,
                            attempt + 1,
                            max_retries + 1,
                            retry_delay
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(retry_delay)).await;
                    }

                    last_result = match step.step_type.as_str() {
                        "test" => {
                            if let Some(ref test_id) = step.test_id {
                                match self.execute_verification_test_with_details(test_id).await {
                                    Ok(test_result) => {
                                        let passed = test_result.status == TestStatus::Passed;
                                        let details = VerificationStepDetails {
                                            step_id: step
                                                .name
                                                .clone()
                                                .unwrap_or_else(|| format!("step-{}", index)),
                                            phase: "verification".to_string(),
                                            stdout: Some(test_result.output.clone()),
                                            stderr: None,
                                            assertions_passed: Some(test_result.assertions_passed),
                                            assertions_total: Some(
                                                test_result.assertions_passed
                                                    + test_result.assertions_failed,
                                            ),
                                            console_output: test_result
                                                .structured_output
                                                .as_ref()
                                                .and_then(|v| v.get("console_output"))
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string()),
                                            page_snapshot: test_result
                                                .structured_output
                                                .as_ref()
                                                .and_then(|v| v.get("page_snapshot"))
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string()),
                                            exit_code: test_result.exit_code,
                                            check_results: None,
                                            console_errors: None,
                                        };
                                        (
                                            passed,
                                            if passed {
                                                None
                                            } else {
                                                test_result.error.clone()
                                            },
                                            Some(details),
                                            None,
                                        )
                                    }
                                    Err(e) => (
                                        false,
                                        Some(format!("Test execution error: {}", e)),
                                        Some(VerificationStepDetails {
                                            step_id: step
                                                .name
                                                .clone()
                                                .unwrap_or_else(|| format!("step-{}", index)),
                                            phase: "verification".to_string(),
                                            stderr: Some(e),
                                            ..Default::default()
                                        }),
                                        None,
                                    ),
                                }
                            } else {
                                // No test_id — delegate to handler system which supports
                                // repository tests, inline commands (check_command/shell_command),
                                // and auto-detection fallbacks.
                                let (success, handler_error, _screenshot, handler_output_data) =
                                    self.execute_single_step(step).await;
                                let details = VerificationStepDetails {
                                    step_id: step
                                        .name
                                        .clone()
                                        .unwrap_or_else(|| format!("step-{}", index)),
                                    phase: "verification".to_string(),
                                    ..Default::default()
                                };
                                (success, handler_error, Some(details), handler_output_data)
                            }
                        }
                        "check" => {
                            // Execute check step (shell command for checks like lint, typecheck, etc.)
                            // Output is extracted by post-match normalization from handler_output_data.
                            let (success, error, _screenshot, handler_output_data) =
                                self.execute_single_step(step).await;
                            let details = VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                // stdout filled by post-match normalization from handler_output_data
                                ..Default::default()
                            };
                            (success, error, Some(details), handler_output_data)
                        }
                        "shell" => {
                            // Execute shell command step
                            // Timeouts are disabled by default
                            let timeout = step.timeout_seconds;
                            let (success, error, output) =
                                self.execute_shell_command_step(step, timeout).await;
                            let details = VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                stdout: output, // Capture output for AI context
                                ..Default::default()
                            };
                            (success, error, Some(details), None)
                        }
                        "check_group" => {
                            // Execute check group - runs all checks in the group
                            // Timeouts are disabled by default
                            let timeout = step.timeout_seconds;
                            let (success, error, summary, check_results) =
                                self.execute_check_group_step(step, timeout).await;
                            let details = VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                // Capture the detailed summary with all check results for AI context
                                stdout: summary,
                                // Include structured check results for UI display
                                check_results,
                                ..Default::default()
                            };
                            (success, error, Some(details), None)
                        }
                        "log_watch" => {
                            // Execute log watch step (scans dev logs for errors)
                            let (success, error, output) = self.execute_log_watch_step(step).await;
                            let details = VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                stdout: output,
                                ..Default::default()
                            };
                            (success, error, Some(details), None)
                        }
                        "gate" => {
                            // Gate step is a semantic aggregation marker. Actual pass/fail
                            // aggregation is handled by the verification phase result logic.
                            // The gate step itself always succeeds at execution time.
                            info!(
                        "Gate step '{}' executed (aggregation handled by verification executor)",
                        step.name.as_deref().unwrap_or("unnamed")
                    );
                            (true, None, None, None)
                        }
                        "prompt" => {
                            // AI Review verification step — dispatch via handler with iteration context
                            let mut handler_ctx = self.create_handler_context();
                            handler_ctx.iteration = Some(iteration);
                            let handler = super::handlers::PromptStepHandler;
                            let result = handler.execute(step, &handler_ctx).await;
                            let details = VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                stdout: result
                                    .output_data
                                    .as_ref()
                                    .and_then(|d| d.get("reasoning"))
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string()),
                                ..Default::default()
                            };
                            (
                                result.success,
                                result.error,
                                Some(details),
                                result.output_data,
                            )
                        }
                        _ => {
                            // Generic handler for all other step types in verification.
                            // Output is captured by the post-match normalization block below.
                            let (success, error, screenshot, handler_output_data) =
                                self.execute_single_step(step).await;
                            let details = screenshot.map(|s| VerificationStepDetails {
                                step_id: step
                                    .name
                                    .clone()
                                    .unwrap_or_else(|| format!("step-{}", index)),
                                phase: "verification".to_string(),
                                stdout: Some(s),
                                ..Default::default()
                            });
                            (success, error, details, handler_output_data)
                        }
                    };

                    // If step succeeded or no retries left, break out
                    if last_result.0 || attempt >= max_retries {
                        break;
                    }
                }
                last_result
            };

            // === Post-match normalization ===
            // Ensure every verification step has VerificationStepDetails with stdout
            // populated. This makes output available to the agentic phase regardless
            // of step type. Handlers put their output in different places (stdout,
            // output_data, check_results), so we normalize here.
            if verification_details.is_none() {
                // No verification_details at all — extract text from output_data
                let extracted = extract_text_from_output_data(&step_output_data);
                if extracted.is_some() || !success {
                    verification_details = Some(VerificationStepDetails {
                        step_id: step
                            .name
                            .clone()
                            .unwrap_or_else(|| format!("step-{}", index)),
                        phase: "verification".to_string(),
                        stdout: extracted,
                        ..Default::default()
                    });
                }
            } else if let Some(ref mut details) = verification_details {
                // verification_details exists but stdout is None — fill from output_data
                if details.stdout.is_none() {
                    details.stdout = extract_text_from_output_data(&step_output_data);
                }
            }

            // Extract consoleErrors from output_data and attach to verification_details
            if let Some(ref mut details) = verification_details {
                if details.console_errors.is_none() {
                    if let Some(ref output) = step_output_data {
                        // consoleErrors may be at top level (action response) or nested in spec_result
                        let errors = output
                            .get("consoleErrors")
                            .or_else(|| {
                                output
                                    .get("spec_result")
                                    .and_then(|sr| sr.get("consoleErrors"))
                            })
                            .and_then(|v| v.as_array())
                            .cloned();
                        if errors.as_ref().is_some_and(|e| !e.is_empty()) {
                            details.console_errors = errors;
                        }
                    }
                }
            }

            // Auto-fail: if fail_on_console_errors is set and console errors were captured,
            // flip a passing step to failed
            if success && step.fail_on_console_errors {
                if let Some(ref details) = verification_details {
                    if details
                        .console_errors
                        .as_ref()
                        .is_some_and(|e| !e.is_empty())
                    {
                        let count = details.console_errors.as_ref().map_or(0, |e| e.len());
                        warn!(
                            "Verification step '{}' passed but has {} console error(s) — failing due to fail_on_console_errors",
                            step_name, count
                        );
                        success = false;
                        error = Some(format!(
                            "Step passed but {} console error(s) detected (fail_on_console_errors=true)",
                            count
                        ));
                    }
                }
            }

            let duration_ms = step_start.elapsed().as_millis() as u64;

            if success {
                passed_steps += 1;
                info!(
                    "Verification step '{}' passed in {}ms",
                    step_name, duration_ms
                );
            } else {
                failed_steps += 1;
                warn!(
                    "Verification step '{}' failed: {:?}",
                    step_name,
                    error.as_deref().unwrap_or("unknown error")
                );

                // Note: critical_failure is set by connectivity or infrastructure failures
            }

            let step_ended_at = chrono::Utc::now().to_rfc3339();

            // Auto-detect failure category from step output
            let failure_category = if !success {
                let output_text = verification_details
                    .as_ref()
                    .and_then(|d| d.stdout.as_deref())
                    .unwrap_or("")
                    .to_string()
                    + verification_details
                        .as_ref()
                        .and_then(|d| d.stderr.as_deref())
                        .unwrap_or("")
                    + error.as_deref().unwrap_or("");
                Some(categorize_failure(&output_text).to_string())
            } else {
                None
            };

            let result = StepExecutionResult {
                step_index: index,
                step_name,
                step_type: step.step_type.clone(),
                step_id: step.id.clone(),
                success,
                error,
                screenshot_path: None,
                started_at: Some(step_started_at),
                ended_at: Some(step_ended_at),
                duration_ms,
                config: StepExecutionConfig {
                    timeout_seconds: step.timeout_seconds,
                    check_type: step.check_type.clone(),
                    command: step
                        .check_command
                        .clone()
                        .or_else(|| step.shell_command.clone()),
                    test_id: step.test_id.clone(),
                    test_type: step.test_type.clone(),
                    working_directory: step
                        .check_working_directory
                        .clone()
                        .or_else(|| step.shell_command_working_directory.clone()),
                    ui_bridge_action: step.ui_bridge_action.clone(),
                },
                verification_details,
                output_data: step_output_data,
                required: step.required,
                resolved_inputs: None,
                extracted_values: None,
                failure_category,
                interrupted: None,
            };

            // Emit completion event for this step (real-time UI update)
            // This allows the frontend to show progress as each step finishes
            if workflow_name.is_some() {
                let step_type_enum =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Command);
                let metadata = StepMetadata::verification(
                    &event_execution_id, // Use parent ID for FK constraint
                    step_type_enum,
                    &result.step_name,
                    index,
                    iteration,
                );

                let details = if result.success {
                    StepDetails::default().with_duration(duration_ms as i64)
                } else {
                    StepDetails::default()
                        .with_duration(duration_ms as i64)
                        .with_error(result.error.clone().unwrap_or_default())
                };

                let builder = StepEventBuilder::new(&event_execution_id, metadata) // Use parent ID
                    .with_details(details)
                    .with_workflow_name(workflow_name.unwrap_or_default());

                let event = if result.success {
                    builder.build_complete(duration_ms as i64)
                } else {
                    builder.build_error(duration_ms as i64, result.error.as_deref())
                };

                // PG-primary: fire-and-forget async write to PostgreSQL
                {
                    let pg = self.app_state.pg_db.clone();
                    let event_clone = event.clone();
                    tokio::spawn(async move {
                        if let Err(e) = pg.create_task_run_event(&event_clone).await {
                            tracing::warn!("PG event write failed: {}", e);
                        }
                    });
                }
                if let Err(e) = self.app_state.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to emit verification step completion event: {}", e);
                }
            }

            // Update step checkpoint to reflect completion progressively
            // (enables real-time UI updates via /task-runs/{id}/full-state)
            {
                let checkpoint_mgr =
                    CheckpointManager::new(self.app_state.checkpoint_db.clone(), "unified");
                let cp_step_type =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Command);
                let mut checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "verification",
                    Some(iteration),
                    index,
                    cp_step_type.as_str(),
                )
                .with_step_name(&result.step_name)
                .with_stage_index(None);

                let result_json_str = serde_json::to_string(&result).ok();
                if result.success {
                    checkpoint.mark_success(result_json_str, duration_ms as i64);
                } else {
                    checkpoint.mark_failed(
                        result.error.as_deref().unwrap_or("Unknown error"),
                        duration_ms as i64,
                    );
                    // Also store result_json for failed steps so resume can access details
                    checkpoint.result_json = result_json_str;
                }

                if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                    warn!("Failed to update verification step checkpoint: {}", e);
                }

                // Broadcast step-progress to WebSocket clients so the web dashboard refetches
                if let Some(ref app_handle) = self.app_handle {
                    let status = if result.success { "success" } else { "failed" };
                    crate::event_system::broadcast_ws_notification(
                        app_handle,
                        "step-progress",
                        &serde_json::json!({
                            "task_run_id": execution_id,
                            "step_index": index,
                            "step_name": result.step_name,
                            "status": status,
                        }),
                    );
                }
            }

            step_results.push(result);
        }

        let total_duration_ms = start.elapsed().as_millis() as u64;

        // Determine all_passed: all required steps must succeed.
        // Steps with required=false are informational only — their failure
        // doesn't trigger the agentic loop.
        let all_passed = step_results.iter().all(|r| {
            let is_required = r.required.unwrap_or(true); // default: required
            r.success || !is_required
        });

        let result = VerificationPhaseResult {
            iteration,
            all_passed,
            total_steps: verification_steps.len(),
            passed_steps,
            failed_steps,
            skipped_steps,
            total_duration_ms,
            step_results,
            critical_failure,
            console_errors: None, // Populated by phases.rs after verification completes
            app_health: None,     // Populated by phases.rs after verification completes
            browser_events: None, // Populated by phases.rs after verification completes
            network_failures: None, // Populated by phases.rs after verification completes
        };

        info!("{}", result.summary());
        result
    }
}
