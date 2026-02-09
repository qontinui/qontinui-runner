//! Phase executors for the unified workflow.
//!
//! Each phase has a dedicated executor that handles a single responsibility:
//! - SetupExecutor: Runs one-time setup steps
//! - VerificationExecutor: Runs verification/test steps and reports results
//! - AgenticExecutor: Runs the AI with failure context
//! - CompletionExecutor: Runs completion steps (only if verification passed)
//!
//! All step event logging is done through the StepEventLogger facade, which
//! ensures consistent event format and prevents duplicate logging.
//!
//! AI session execution is delegated to the UnifiedAiSessionExecutor, which
//! consolidates the common logic for context building, prompt transformation,
//! and session management.
//!
//! ## Executor Trait
//!
//! Each phase executor implements the `Executor` trait from `crate::executor::traits`,
//! providing a uniform interface for execution with typed configuration and results.
//! This enables:
//! - Consistent error handling through `ExecutorError`
//! - Typed configuration via `SetupConfig`, `VerificationConfig`, etc.
//! - Factory construction via `FromContext`

#![allow(dead_code)]

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, info, instrument, warn};

use crate::config_storage::ConfigStorage;
use crate::database::CheckpointDb;
use crate::executor::{
    prompt_builder, timeout_helper, ExecutionOutcome, Executor, ExecutorContext, ExecutorError,
    FromContext, IntoOutcome,
};
use crate::step_executor::{
    handlers::spec::fetch_external_elements, ExecutionStepConfig, StepExecutionResult,
    StepExecutor, VerificationPhaseResult,
};
use crate::step_metadata::{StepDetails, StepMetadata};
use crate::step_registry::{StepEventKind, StepEventLogger};
use crate::step_types::StepType;
use crate::unified_ai_session::{AiSessionConfig, UnifiedAiSessionExecutor};
use crate::workflow_state::{CheckpointManager, StepCheckpoint};
use crate::AppState;

use super::phase_configs::{
    AgenticConfig, CompletionConfig, CompletionResult, SetupConfig, SetupResult,
    VerificationConfig, VerificationResult,
};
use super::types::{AgenticOutcome, LoopConfig};

// =============================================================================
// Execution Timing Context
// =============================================================================

/// Build a timing context string from execution spans for the current execution.
///
/// Returns None if no spans exist or the query fails.
fn build_execution_timing_context(
    checkpoint_db: &CheckpointDb,
    execution_id: &str,
) -> Option<String> {
    let spans = checkpoint_db
        .get_execution_spans(Some(execution_id), None, None, Some(100))
        .ok()?;

    if spans.is_empty() {
        return None;
    }

    let mut sections = Vec::new();

    // Phase timings
    let phase_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.name.starts_with("workflow.phase."))
        .collect();
    if !phase_spans.is_empty() {
        let mut phase_lines = vec!["**Phase Timings:**".to_string()];
        for span in &phase_spans {
            let phase_name = span
                .name
                .strip_prefix("workflow.phase.")
                .unwrap_or(&span.name);
            let duration = span
                .duration_ms
                .map(format_duration_ms)
                .unwrap_or_else(|| "in progress".to_string());
            let status = if !span.success { " (failed)" } else { "" };
            phase_lines.push(format!("- {}: {}{}", phase_name, duration, status));
        }
        sections.push(phase_lines.join("\n"));
    }

    // AI session stats
    let ai_spans: Vec<_> = spans.iter().filter(|s| s.name == "ai.session").collect();
    if !ai_spans.is_empty() {
        let total_ms: i64 = ai_spans.iter().filter_map(|s| s.duration_ms).sum();
        let count = ai_spans.len();
        let avg_ms = if count > 0 {
            total_ms / count as i64
        } else {
            0
        };
        let failed = ai_spans.iter().filter(|s| !s.success).count();

        let mut ai_lines = vec!["**AI Sessions:**".to_string()];
        ai_lines.push(format!(
            "- Total: {} sessions, {} total",
            count,
            format_duration_ms(total_ms)
        ));
        ai_lines.push(format!(
            "- Average: {} per session",
            format_duration_ms(avg_ms)
        ));
        if failed > 0 {
            ai_lines.push(format!("- Failed: {} sessions", failed));
        }
        sections.push(ai_lines.join("\n"));
    }

    // Slow operations (>5s)
    let slow_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.duration_ms.unwrap_or(0) > 5000)
        .collect();
    if !slow_spans.is_empty() {
        let mut slow_lines = vec!["**Slow Operations (>5s):**".to_string()];
        for span in &slow_spans {
            let duration = span.duration_ms.map(format_duration_ms).unwrap_or_default();
            let error_suffix = if let Some(ref err) = span.error {
                format!(" - FAILED: {}", err)
            } else if !span.success {
                " - FAILED".to_string()
            } else {
                String::new()
            };
            slow_lines.push(format!("- {}: {}{}", span.name, duration, error_suffix));
        }
        sections.push(slow_lines.join("\n"));
    }

    if sections.is_empty() {
        return None;
    }

    Some(format!(
        "---\n\n### Execution Timing\n\n{}",
        sections.join("\n\n")
    ))
}

/// Format milliseconds into a human-readable duration string.
fn format_duration_ms(ms: i64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let minutes = ms / 60_000;
        let seconds = (ms % 60_000) / 1000;
        format!("{}m {}s", minutes, seconds)
    }
}

// =============================================================================
// Iteration Context Builder
// =============================================================================

/// Build context from previous iterations for the AI to reference.
///
/// Includes findings from the findings database and previous verification
/// results so the AI can understand what was tried and what happened.
fn build_unified_iteration_context(
    checkpoint_db: &CheckpointDb,
    execution_id: &str,
    current_iteration: u32,
) -> Option<String> {
    let mut sections = Vec::new();

    // 1. Collect findings from the findings database
    if let Ok(findings) = checkpoint_db.get_findings_for_task(execution_id) {
        if !findings.is_empty() {
            let mut findings_lines = vec!["### Findings from Previous Iterations".to_string()];
            for finding in &findings {
                let status_str = finding.status.as_str();
                let category_str = finding.category.as_str();
                findings_lines.push(format!(
                    "- [{}:{}] {}",
                    category_str, status_str, finding.title
                ));
                if !finding.description.is_empty() {
                    // Truncate long descriptions
                    let desc = if finding.description.len() > 200 {
                        format!("{}...", &finding.description[..200])
                    } else {
                        finding.description.clone()
                    };
                    findings_lines.push(format!("  {}", desc));
                }
            }
            sections.push(findings_lines.join("\n"));
        }
    }

    // 2. Collect previous verification results
    let mut verification_lines = vec!["### Previous Verification Results".to_string()];
    let mut has_prev_results = false;
    for iter in 1..current_iteration {
        if let Ok(Some(result)) = checkpoint_db.get_verification_phase_result(execution_id, iter) {
            has_prev_results = true;
            let passed = result
                .get("passed_steps")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total = result
                .get("total_steps")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let failed = result
                .get("failed_steps")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let all_passed = result
                .get("all_passed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let status = if all_passed {
                "ALL PASSED".to_string()
            } else {
                format!("{}/{} passed, {} failed", passed, total, failed)
            };
            verification_lines.push(format!("- Iteration {}: {}", iter, status));

            // List failed step names from previous iterations
            if let Some(step_results) = result.get("step_results").and_then(|v| v.as_array()) {
                let failed_names: Vec<_> = step_results
                    .iter()
                    .filter(|s| s.get("success").and_then(|v| v.as_bool()) == Some(false))
                    .filter_map(|s| s.get("step_name").and_then(|v| v.as_str()))
                    .collect();
                if !failed_names.is_empty() {
                    verification_lines.push(format!("  Failed: {}", failed_names.join(", ")));
                }
            }
        }
    }
    if has_prev_results {
        sections.push(verification_lines.join("\n"));
    }

    if sections.is_empty() {
        return None;
    }

    Some(format!(
        "---\n\n## Previous Iteration Context\n\n{}",
        sections.join("\n\n")
    ))
}

// =============================================================================
// Setup Phase Executor
// =============================================================================

/// Executes the setup phase (runs once at the start).
///
/// Handles both automation steps (shell commands, workflows) and prompt steps (AI tasks).
/// AI session execution is delegated to the UnifiedAiSessionExecutor.
pub struct SetupExecutor {
    executor: StepExecutor,
    ai_executor: UnifiedAiSessionExecutor,
    checkpoint_db: Arc<CheckpointDb>,
}

impl SetupExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<TokioMutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        let checkpoint_db = app_state.checkpoint_db.clone();
        Self {
            executor: StepExecutor::with_app_handle(
                app_state.clone(),
                config_storage,
                app_handle.clone(),
            ),
            ai_executor: UnifiedAiSessionExecutor::new(app_state, app_handle, pid_tracker),
            checkpoint_db,
        }
    }

    /// Enable interactive sessions via the session manager.
    pub fn set_session_manager(&mut self, sm: Arc<crate::claude_session::SessionManager>) {
        self.ai_executor.session_manager = Some(sm);
    }

    /// Run setup steps. Returns true if successful.
    ///
    /// Executes automation steps first (shell commands, etc.), then prompt steps (AI tasks).
    /// The logger is required for consistent step event logging.
    ///
    /// `timeout_seconds` is optional - None means no timeout (default, recommended).
    ///
    /// Step checkpointing is integrated for resume capability.
    #[instrument(
        name = "workflow.phase.setup",
        skip(self, automation_steps, prompt_steps, logger),
        fields(
            execution_id = %execution_id,
            workflow_name = %workflow_name,
            automation_step_count = automation_steps.len(),
            prompt_step_count = prompt_steps.len()
        )
    )]
    pub async fn run_setup(
        &self,
        automation_steps: &[ExecutionStepConfig],
        prompt_steps: &[ExecutionStepConfig],
        execution_id: &str,
        workflow_name: &str,
        timeout_seconds: Option<u64>,
        logger: &StepEventLogger,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Create checkpoint manager for step-level checkpointing
        let checkpoint_mgr = CheckpointManager::new(self.checkpoint_db.clone(), "unified");

        // Run automation setup steps first
        if !automation_steps.is_empty() {
            info!(
                "SETUP-PHASE: Running {} automation steps",
                automation_steps.len()
            );

            // Checkpoint each automation step
            for (idx, step) in automation_steps.iter().enumerate() {
                let step_type =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::ShellCommand);
                let step_name = step.name.as_deref().unwrap_or(&step.step_type);

                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "setup",
                    Some(0),
                    idx,
                    step_type.as_str(),
                )
                .with_step_name(step_name);
                checkpoint.mark_started();
                if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                    warn!("Failed to save setup step checkpoint: {}", e);
                }
            }

            let (result, _has_gui) = self
                .executor
                .execute_setup_phase(automation_steps, execution_id, &[])
                .await;

            // Checkpoint completion for each step
            for (idx, step_result) in result.steps.iter().enumerate() {
                let step = &automation_steps[idx];
                let step_type =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::ShellCommand);
                let step_name = step.name.as_deref().unwrap_or(&step.step_type);

                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "setup",
                    Some(0),
                    idx,
                    step_type.as_str(),
                )
                .with_step_name(step_name);

                let duration_ms = step_result.duration_ms as i64;
                if step_result.success {
                    checkpoint.mark_success(serde_json::to_string(step_result).ok(), duration_ms);
                } else {
                    checkpoint.mark_failed(
                        step_result.error.as_deref().unwrap_or("Unknown error"),
                        duration_ms,
                    );
                }

                if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                    warn!("Failed to save setup step completion checkpoint: {}", e);
                }
            }

            overall_success = overall_success && result.success;
            all_results.extend(result.steps);

            if !result.success {
                warn!("SETUP-PHASE: Automation steps failed");
                return (false, all_results);
            }
        }

        // Run prompt setup steps (AI tasks)
        if !prompt_steps.is_empty() {
            info!(
                "SETUP-PHASE: Running {} prompt steps (AI tasks)",
                prompt_steps.len()
            );

            // Checkpoint the AI step as a single step
            let ai_step_idx = automation_steps.len();
            let step_name =
                prompt_builder::consolidate_step_names_with_default(prompt_steps, "Setup AI Task");

            // Use Some(0) instead of None for iteration to ensure SQLite's
            // UNIQUE constraint works correctly (NULL != NULL in SQLite)
            let mut ai_checkpoint = StepCheckpoint::new(
                execution_id,
                "unified",
                "setup",
                Some(0),
                ai_step_idx,
                "ai_session",
            )
            .with_step_name(&step_name);
            ai_checkpoint.mark_started();
            if let Err(e) = checkpoint_mgr.save_step(&ai_checkpoint) {
                warn!("Failed to save setup AI step checkpoint: {}", e);
            }

            // Use structured prompts for granular sub-step tracking
            let (setup_prompt, sub_step_metadata) =
                prompt_builder::consolidate_prompts_structured(prompt_steps, "setup");

            if !setup_prompt.is_empty() {
                // Use the unified AI session executor with sub-step metadata
                let config = AiSessionConfig::setup(execution_id, workflow_name, &step_name)
                    .with_timeout(timeout_seconds)
                    .with_sub_step_metadata(sub_step_metadata);

                let (result, duration_ms) = timeout_helper::timed_result_async(
                    self.ai_executor.execute(&config, &setup_prompt, logger),
                )
                .await;
                let duration_ms = duration_ms as i64;
                overall_success = overall_success && result.success;
                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut ai_checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "setup",
                    Some(0),
                    ai_step_idx,
                    "ai_session",
                )
                .with_step_name(&step_name);

                if result.success {
                    ai_checkpoint.mark_success(Some(result.output.clone()), duration_ms);
                } else {
                    ai_checkpoint.mark_failed("AI session failed", duration_ms);
                }

                if let Err(e) = checkpoint_mgr.save_step(&ai_checkpoint) {
                    warn!("Failed to save setup AI step completion checkpoint: {}", e);
                }

                if !result.success {
                    warn!("SETUP-PHASE: AI prompt steps failed");
                }
            }
        }

        if automation_steps.is_empty() && prompt_steps.is_empty() {
            info!("SETUP-PHASE: No setup steps to execute");
        } else {
            info!("SETUP-PHASE: Completed with success={}", overall_success);
        }

        (overall_success, all_results)
    }

    /// Run setup and return a unified ExecutionOutcome.
    ///
    /// This uses the IntoOutcome trait to convert the SetupResult into a
    /// standardized ExecutionOutcome, which is useful for consistent result handling.
    ///
    /// # Arguments
    /// * `config` - The setup configuration
    /// * `logger` - Logger for step events
    ///
    /// # Returns
    /// An `ExecutionOutcome` summarizing the setup phase execution.
    pub async fn run_setup_to_outcome(
        &self,
        config: &SetupConfig,
        logger: &StepEventLogger,
    ) -> ExecutionOutcome {
        let start = std::time::Instant::now();

        let (success, step_results) = self
            .run_setup(
                &config.automation_steps,
                &config.prompt_steps,
                &config.execution_id,
                &config.workflow_name,
                config.timeout_seconds,
                logger,
            )
            .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        // Use the IntoOutcome trait for consistent conversion
        let result = SetupResult {
            success,
            step_results,
        };
        result.into_outcome(duration_ms)
    }
}

// =============================================================================
// Verification Phase Executor
// =============================================================================

/// Executes verification steps and determines if they all pass.
pub struct VerificationExecutor {
    executor: StepExecutor,
    checkpoint_db: Arc<CheckpointDb>,
}

impl VerificationExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        let checkpoint_db = app_state.checkpoint_db.clone();
        Self {
            executor: StepExecutor::with_app_handle(app_state.clone(), config_storage, app_handle),
            checkpoint_db,
        }
    }

    /// Run verification steps.
    ///
    /// Returns (verification_result, step_results)
    /// The logger is required for consistent step event logging.
    ///
    /// Step checkpointing is now integrated to enable resume after crashes:
    /// - Each step is checkpointed before (running) and after (success/failed) execution
    /// - On resume, completed steps can be skipped based on checkpoint data
    #[instrument(
        name = "workflow.phase.verification",
        skip(self, steps, logger),
        fields(
            execution_id = %execution_id,
            iteration = iteration,
            workflow_name = %workflow_name,
            step_count = steps.len()
        )
    )]
    pub async fn run_verification(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
        workflow_name: &str,
        logger: &StepEventLogger,
    ) -> (VerificationPhaseResult, Vec<StepExecutionResult>) {
        if steps.is_empty() {
            info!(
                "VERIFICATION-PHASE: No verification steps defined (iteration {})",
                iteration
            );
            // No verification steps = verification passes
            return (
                VerificationPhaseResult {
                    iteration,
                    all_passed: true,
                    critical_failure: false,
                    total_steps: 0,
                    passed_steps: 0,
                    failed_steps: 0,
                    skipped_steps: 0,
                    total_duration_ms: 0,
                    step_results: Vec::new(),
                    gate_results: Vec::new(),
                    gate_based_evaluation: false,
                },
                Vec::new(),
            );
        }

        info!(
            "VERIFICATION-PHASE: Running {} steps (iteration {})",
            steps.len(),
            iteration
        );

        // Create checkpoint manager for step-level checkpointing
        let checkpoint_mgr = CheckpointManager::new(self.checkpoint_db.clone(), "unified");

        // Pre-fetch external elements if any spec step needs them
        // This avoids per-step timeouts and provides early failure detection
        let needs_external_elements = steps.iter().any(|step| {
            step.step_type == "spec" && step.spec_element_source.as_deref() == Some("external")
        });

        let prefetched_elements = if needs_external_elements {
            info!("VERIFICATION-PHASE: Pre-fetching external elements for spec verification");
            match fetch_external_elements().await {
                Ok(elements) => {
                    let count = elements.as_array().map(|a| a.len()).unwrap_or(0);
                    info!(
                        "VERIFICATION-PHASE: Pre-fetched {} external elements from Chrome extension",
                        count
                    );
                    Some(elements)
                }
                Err(e) => {
                    // External elements are required but couldn't be fetched - fail early
                    // This provides clear feedback about Chrome extension connectivity
                    warn!(
                        "VERIFICATION-PHASE: UI Bridge spec verification failed - Chrome extension not connected (not a Playwright issue): {}",
                        e
                    );

                    // Return a failed verification result with connectivity error
                    let error_msg = format!(
                        "UI Bridge spec verification failed: Chrome extension not connected. \
                         This is NOT a Playwright test - it's a UI Bridge spec verification \
                         that requires the Qontinui browser extension to be installed and \
                         connected to an active browser tab. Error: {}",
                        e
                    );
                    let now = chrono::Utc::now().to_rfc3339();
                    let connectivity_result = StepExecutionResult {
                        step_index: 0,
                        step_type: "ui_bridge_connectivity".to_string(),
                        step_name: "UI Bridge Extension Connectivity Check".to_string(),
                        success: false,
                        error: Some(error_msg.clone()),
                        screenshot_path: None,
                        started_at: Some(now.clone()),
                        ended_at: Some(now),
                        duration_ms: 0,
                        config: crate::step_executor::StepExecutionConfig::default(),
                        verification_details: None,
                        output_data: None,
                    };

                    return (
                        VerificationPhaseResult {
                            iteration,
                            all_passed: false,
                            critical_failure: true, // Mark as critical since external elements are required
                            total_steps: steps.len(),
                            passed_steps: 0,
                            failed_steps: 1,
                            skipped_steps: steps.len().saturating_sub(1),
                            total_duration_ms: 0,
                            step_results: vec![connectivity_result.clone()],
                            gate_results: Vec::new(),
                            gate_based_evaluation: false,
                        },
                        vec![connectivity_result],
                    );
                }
            }
        } else {
            None
        };

        // Clone steps and inject pre-fetched elements if available
        let steps_to_execute: Vec<ExecutionStepConfig> = if let Some(ref elements) =
            prefetched_elements
        {
            steps
                .iter()
                .map(|step| {
                    let mut step = step.clone();
                    if step.step_type == "spec"
                        && step.spec_element_source.as_deref() == Some("external")
                    {
                        debug!(
                            "VERIFICATION-PHASE: Injecting pre-fetched elements into spec step '{}'",
                            step.name.as_deref().unwrap_or("unnamed")
                        );
                        step.spec_prefetched_elements = Some(elements.clone());
                    }
                    step
                })
                .collect()
        } else {
            steps.to_vec()
        };
        let steps = &steps_to_execute;

        // Log START events and save checkpoints for each step before execution
        for (idx, step) in steps.iter().enumerate() {
            let step_type =
                StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Playwright);
            let step_name = step.name.as_deref().unwrap_or(&step.step_type);
            let metadata =
                StepMetadata::verification(execution_id, step_type, step_name, idx, iteration);
            let details = StepDetails::default();

            if let Err(e) =
                logger.log_start(StepEventKind::VerificationStepStart, metadata, details)
            {
                warn!("Failed to log verification step start event: {}", e);
            }

            // Save step checkpoint as "running"
            let mut checkpoint = StepCheckpoint::new(
                execution_id,
                "unified",
                "verification",
                Some(iteration),
                idx,
                step_type.as_str(),
            )
            .with_step_name(step_name);
            checkpoint.mark_started();
            if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                warn!("Failed to save verification step checkpoint: {}", e);
            }
        }

        // Use the new method that emits completion events as each step finishes
        // This allows the UI to show real-time progress instead of waiting for all steps
        let result = self
            .executor
            .execute_verification_steps_with_events(
                steps,
                execution_id,
                iteration,
                Some(workflow_name),
            )
            .await;

        info!(
            "VERIFICATION-PHASE: Iteration {} result: all_passed={}, critical_failure={}, passed={}/{}, failed={}",
            iteration,
            result.all_passed,
            result.critical_failure,
            result.passed_steps,
            result.total_steps,
            result.failed_steps
        );

        // Save completion checkpoints for each step
        for (idx, step_result) in result.step_results.iter().enumerate() {
            let step = &steps[idx];
            let step_type =
                StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Playwright);
            let step_name = step.name.as_deref().unwrap_or(&step.step_type);

            let mut checkpoint = StepCheckpoint::new(
                execution_id,
                "unified",
                "verification",
                Some(iteration),
                idx,
                step_type.as_str(),
            )
            .with_step_name(step_name);

            let duration_ms = step_result.duration_ms as i64;
            if step_result.success {
                checkpoint.mark_success(serde_json::to_string(step_result).ok(), duration_ms);
            } else {
                checkpoint.mark_failed(
                    step_result.error.as_deref().unwrap_or("Unknown error"),
                    duration_ms,
                );
            }

            if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                warn!(
                    "Failed to save verification step completion checkpoint: {}",
                    e
                );
            }
        }

        let step_results = result.step_results.clone();
        (result, step_results)
    }

    /// Run verification and return a unified ExecutionOutcome.
    ///
    /// This uses the IntoOutcome trait to convert the VerificationResult into a
    /// standardized ExecutionOutcome, which is useful for consistent result handling.
    ///
    /// # Arguments
    /// * `config` - The verification configuration
    /// * `logger` - Logger for step events
    ///
    /// # Returns
    /// An `ExecutionOutcome` summarizing the verification phase execution.
    pub async fn run_verification_to_outcome(
        &self,
        config: &VerificationConfig,
        logger: &StepEventLogger,
    ) -> ExecutionOutcome {
        let start = std::time::Instant::now();

        let (phase_result, step_results) = self
            .run_verification(
                &config.steps,
                &config.execution_id,
                config.iteration,
                &config.workflow_name,
                logger,
            )
            .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        // Use the IntoOutcome trait for consistent conversion
        let result = VerificationResult {
            phase_result,
            step_results,
        };
        result.into_outcome(duration_ms)
    }
}

// =============================================================================
// Agentic Phase Executor
// =============================================================================

/// Executes the AI agentic phase with failure context.
/// AI session execution is delegated to the UnifiedAiSessionExecutor.
pub struct AgenticExecutor {
    ai_executor: UnifiedAiSessionExecutor,
    checkpoint_db: Arc<CheckpointDb>,
}

impl AgenticExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        let checkpoint_db = app_state.checkpoint_db.clone();
        Self {
            ai_executor: UnifiedAiSessionExecutor::new(app_state, app_handle, pid_tracker),
            checkpoint_db,
        }
    }

    /// Enable interactive sessions via the session manager.
    pub fn set_session_manager(&mut self, sm: Arc<crate::claude_session::SessionManager>) {
        self.ai_executor.session_manager = Some(sm);
    }

    /// Run the AI with the given prompt and failure context.
    ///
    /// This calls Claude directly (no session system, no orchestrator).
    /// The logger is required for consistent step event logging.
    ///
    /// Step checkpointing is integrated for resume capability.
    /// Progress markers from previous sessions are included in the context
    /// to help the AI understand where to resume long operations.
    #[instrument(
        name = "workflow.phase.agentic",
        skip(self, config, failure_context, logger),
        fields(
            execution_id = %config.execution_id,
            iteration = iteration,
            workflow_name = %config.workflow_name,
            has_steps = has_agentic_steps
        )
    )]
    pub async fn run_agentic(
        &self,
        config: &LoopConfig,
        iteration: u32,
        failure_context: &str,
        has_agentic_steps: bool,
        logger: &StepEventLogger,
    ) -> AgenticOutcome {
        if !has_agentic_steps {
            info!(
                "AGENTIC-PHASE: No agentic steps defined, skipping (iteration {})",
                iteration
            );
            return AgenticOutcome::Skipped;
        }

        // Create checkpoint manager for step-level checkpointing
        let checkpoint_mgr = CheckpointManager::new(self.checkpoint_db.clone(), "unified");

        // Try to get the latest progress marker from previous checkpoints
        // This helps the AI understand where to resume if a previous session was interrupted
        let progress_context = self.get_progress_marker_context(&config.execution_id, iteration);

        // Checkpoint the agentic phase as a single step
        let mut checkpoint = StepCheckpoint::new(
            &config.execution_id,
            "unified",
            "agentic",
            Some(iteration),
            0, // Agentic is a single-step phase
            "ai_session",
        )
        .with_step_name("AI Fixing Issues");
        checkpoint.mark_started();
        if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
            warn!("Failed to save agentic step checkpoint: {}", e);
        }

        // Build enhanced prompt with failure context and progress marker
        // Note: The UnifiedAiSessionExecutor will handle:
        // - Adding autonomous context (configured in AiSessionConfig::agentic)
        // - Stripping completion markers
        // - Appending finding instructions
        let enhanced_prompt = if failure_context.is_empty() {
            warn!(
                "AGENTIC-PHASE: No failure context provided for iteration {} - AI won't know what to fix!",
                iteration
            );
            // Still include progress context if available
            if let Some(ref progress) = progress_context {
                format!("{}\n\n{}", config.base_prompt, progress)
            } else {
                config.base_prompt.clone()
            }
        } else {
            info!(
                "AGENTIC-PHASE: Building prompt with {} chars of failure context (iteration {})",
                failure_context.len(),
                iteration
            );
            let base = format!(
                "{}\n\n---\n\nThe following verification checks FAILED. Please fix these issues:\n\n{}\n\nFix the issues above and ensure all checks pass.",
                config.base_prompt, failure_context
            );
            // Append progress context if available
            if let Some(ref progress) = progress_context {
                format!("{}\n\n{}", base, progress)
            } else {
                base
            }
        };

        // Append execution timing context if available (from iteration 2+)
        let enhanced_prompt = if iteration > 1 {
            match build_execution_timing_context(&self.checkpoint_db, &config.execution_id) {
                Some(timing) => {
                    info!(
                        "AGENTIC-PHASE: Appending execution timing context ({} chars)",
                        timing.len()
                    );
                    format!("{}\n\n{}", enhanced_prompt, timing)
                }
                None => enhanced_prompt,
            }
        } else {
            enhanced_prompt
        };

        // For iteration 2+, add context from previous iterations (findings + verification results)
        let enhanced_prompt = if iteration > 1 {
            match build_unified_iteration_context(
                &self.checkpoint_db,
                &config.execution_id,
                iteration,
            ) {
                Some(ctx) => {
                    info!(
                        "AGENTIC-PHASE: Appending iteration context ({} chars) for iteration {}",
                        ctx.len(),
                        iteration
                    );
                    format!("{}\n\n{}", enhanced_prompt, ctx)
                }
                None => enhanced_prompt,
            }
        } else {
            enhanced_prompt
        };

        // Append safety instructions to prevent AI from modifying runner internals
        let enhanced_prompt = format!(
            "{}\n\n## Important Constraints\n\n\
            - Do NOT modify the runner's SQLite database directly. Configuration changes must go through the runner UI or API.\n\
            - Do NOT modify workflow JSON files in the parent directory. Fix the application code instead.\n\
            - Focus on fixing the source code that the verification tests are checking.",
            enhanced_prompt
        );

        // Use the unified AI session executor with timing
        let ai_config =
            AiSessionConfig::agentic(&config.execution_id, &config.workflow_name, iteration)
                .with_timeout(config.timeout_seconds);

        let (result, duration_ms) = timeout_helper::timed_result_async(self.ai_executor.execute(
            &ai_config,
            &enhanced_prompt,
            logger,
        ))
        .await;
        let duration_ms = duration_ms as i64;

        // Checkpoint completion
        let mut completion_checkpoint = StepCheckpoint::new(
            &config.execution_id,
            "unified",
            "agentic",
            Some(iteration),
            0,
            "ai_session",
        )
        .with_step_name("AI Fixing Issues");

        let outcome = if result.success {
            completion_checkpoint.mark_success(Some(result.output.clone()), duration_ms);
            AgenticOutcome::Success {
                output: result.output,
            }
        } else if result.output.is_empty() {
            completion_checkpoint.mark_failed("AI session failed", duration_ms);
            AgenticOutcome::Error {
                error: "AI session failed".to_string(),
            }
        } else {
            completion_checkpoint.mark_failed("AI reported failure", duration_ms);
            AgenticOutcome::Failed {
                output: result.output,
                error: "AI reported failure".to_string(),
            }
        };

        if let Err(e) = checkpoint_mgr.save_step(&completion_checkpoint) {
            warn!("Failed to save agentic step completion checkpoint: {}", e);
        }

        outcome
    }

    /// Get progress marker context from previous checkpoints.
    ///
    /// This queries for the most recent checkpoint from a previous agentic session
    /// and retrieves its latest progress marker. This information helps the AI
    /// understand where to resume long operations.
    ///
    /// Returns a formatted string like:
    /// "Last progress: file_progress 50/100. Continue from where you left off."
    fn get_progress_marker_context(&self, execution_id: &str, iteration: u32) -> Option<String> {
        // Get checkpoints for the agentic phase at this iteration
        let checkpoints = self
            .checkpoint_db
            .get_workflow_step_checkpoints(execution_id, "agentic", Some(iteration))
            .ok()?;

        // Find the most recent checkpoint (by step_index descending, or just take the last one)
        // There should typically only be one checkpoint per iteration, but we want the latest
        let latest_checkpoint = checkpoints.into_iter().last()?;

        // Query for the latest progress marker using the checkpoint's id
        let progress_marker = self
            .checkpoint_db
            .get_latest_step_progress_marker(&latest_checkpoint.id)
            .ok()
            .flatten()?;

        // Format the progress context message
        let progress_string = progress_marker.progress_string();
        let marker_type = &progress_marker.marker_type;

        let mut message = format!(
            "---\n\n**Resume Context:** Last progress: {} {}.",
            marker_type, progress_string
        );

        // Add description if available
        if let Some(description) = &progress_marker.description {
            message.push_str(&format!(" ({})", description));
        }

        message.push_str(" Continue from where you left off.");

        info!(
            "AGENTIC-PHASE: Including progress marker context: {} {}/{}",
            marker_type,
            progress_marker.current_value,
            progress_marker
                .total_value
                .map_or("?".to_string(), |v| v.to_string())
        );

        Some(message)
    }
}

// =============================================================================
// Completion Phase Executor
// =============================================================================

/// Executes the completion phase (runs once after verification passes).
/// AI session execution is delegated to the UnifiedAiSessionExecutor.
pub struct CompletionExecutor {
    executor: StepExecutor,
    ai_executor: UnifiedAiSessionExecutor,
    checkpoint_db: Arc<CheckpointDb>,
}

impl CompletionExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        let checkpoint_db = app_state.checkpoint_db.clone();
        Self {
            executor: StepExecutor::with_app_handle(
                app_state.clone(),
                config_storage,
                app_handle.clone(),
            ),
            ai_executor: UnifiedAiSessionExecutor::new(app_state, app_handle, pid_tracker),
            checkpoint_db,
        }
    }

    /// Enable interactive sessions via the session manager.
    pub fn set_session_manager(&mut self, sm: Arc<crate::claude_session::SessionManager>) {
        self.ai_executor.session_manager = Some(sm);
    }

    /// Run completion steps.
    ///
    /// This should ONLY be called when verification has passed.
    ///
    /// # Arguments
    /// * `iterations_run` - Number of verification-agentic iterations that were executed.
    ///   Used to calculate the correct turn number for the completion phase.
    /// * `timeout_seconds` - Optional inactivity timeout. None means no timeout (default).
    /// * `logger` - Required logger for consistent step event logging.
    ///
    /// Step checkpointing is integrated for resume capability.
    #[instrument(
        name = "workflow.phase.completion",
        skip(self, automation_steps, prompt_steps, logger),
        fields(
            execution_id = %execution_id,
            workflow_name = %workflow_name,
            automation_step_count = automation_steps.len(),
            prompt_step_count = prompt_steps.len(),
            iterations_run = iterations_run
        )
    )]
    pub async fn run_completion(
        &self,
        automation_steps: &[ExecutionStepConfig],
        prompt_steps: &[ExecutionStepConfig],
        execution_id: &str,
        workflow_name: &str,
        timeout_seconds: Option<u64>,
        iterations_run: u32,
        logger: &StepEventLogger,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Create checkpoint manager for step-level checkpointing
        let checkpoint_mgr = CheckpointManager::new(self.checkpoint_db.clone(), "unified");

        // Run automation completion steps
        if !automation_steps.is_empty() {
            info!(
                "COMPLETION-PHASE: Running {} automation steps",
                automation_steps.len()
            );

            // Checkpoint each automation step
            for (idx, step) in automation_steps.iter().enumerate() {
                let step_type =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::ShellCommand);
                let step_name = step.name.as_deref().unwrap_or(&step.step_type);

                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "completion",
                    Some(0),
                    idx,
                    step_type.as_str(),
                )
                .with_step_name(step_name);
                checkpoint.mark_started();
                if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                    warn!("Failed to save completion step checkpoint: {}", e);
                }
            }

            let result = self
                .executor
                .execute_completion_phase(automation_steps, execution_id, &[])
                .await;

            // Checkpoint completion for each step
            for (idx, step_result) in result.steps.iter().enumerate() {
                let step = &automation_steps[idx];
                let step_type =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::ShellCommand);
                let step_name = step.name.as_deref().unwrap_or(&step.step_type);

                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "completion",
                    Some(0),
                    idx,
                    step_type.as_str(),
                )
                .with_step_name(step_name);

                let duration_ms = step_result.duration_ms as i64;
                if step_result.success {
                    checkpoint.mark_success(serde_json::to_string(step_result).ok(), duration_ms);
                } else {
                    checkpoint.mark_failed(
                        step_result.error.as_deref().unwrap_or("Unknown error"),
                        duration_ms,
                    );
                }

                if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                    warn!(
                        "Failed to save completion step completion checkpoint: {}",
                        e
                    );
                }
            }

            overall_success = overall_success && result.success;
            all_results.extend(result.steps);

            if !result.success {
                warn!("COMPLETION-PHASE: Automation steps failed");
            }
        }

        // Run prompt completion steps (AI summary)
        if !prompt_steps.is_empty() {
            info!(
                "COMPLETION-PHASE: Running {} prompt steps (AI summary)",
                prompt_steps.len()
            );

            // Checkpoint the AI step as a single step
            let ai_step_idx = automation_steps.len();
            let step_name = prompt_builder::consolidate_step_names_with_default(
                prompt_steps,
                "Completion AI Task",
            );

            // Use Some(0) instead of None for iteration to ensure SQLite's
            // UNIQUE constraint works correctly (NULL != NULL in SQLite)
            let mut ai_checkpoint = StepCheckpoint::new(
                execution_id,
                "unified",
                "completion",
                Some(0),
                ai_step_idx,
                "ai_session",
            )
            .with_step_name(&step_name);
            ai_checkpoint.mark_started();
            if let Err(e) = checkpoint_mgr.save_step(&ai_checkpoint) {
                warn!("Failed to save completion AI step checkpoint: {}", e);
            }

            // Use structured prompts for granular sub-step tracking
            let (mut completion_prompt, sub_step_metadata) =
                prompt_builder::consolidate_prompts_structured(prompt_steps, "completion");

            // Inject prior phase output context so the completion AI knows what happened
            if !completion_prompt.is_empty() {
                let prior_context = self.build_prior_phase_context(execution_id, iterations_run);
                if !prior_context.is_empty() {
                    completion_prompt =
                        format!("{}\n\n---\n\n{}", prior_context, completion_prompt);
                }
            }

            if !completion_prompt.is_empty() {
                // Use the unified AI session executor with sub-step metadata
                let config = AiSessionConfig::completion(
                    execution_id,
                    workflow_name,
                    &step_name,
                    iterations_run,
                )
                .with_timeout(timeout_seconds)
                .with_sub_step_metadata(sub_step_metadata);

                let (result, duration_ms) = timeout_helper::timed_result_async(
                    self.ai_executor
                        .execute(&config, &completion_prompt, logger),
                )
                .await;
                let duration_ms = duration_ms as i64;
                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut ai_completion_checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "completion",
                    Some(0),
                    ai_step_idx,
                    "ai_session",
                )
                .with_step_name(&step_name);

                if result.success {
                    ai_completion_checkpoint.mark_success(Some(result.output.clone()), duration_ms);
                } else {
                    ai_completion_checkpoint.mark_failed("AI session failed", duration_ms);
                }

                if let Err(e) = checkpoint_mgr.save_step(&ai_completion_checkpoint) {
                    warn!(
                        "Failed to save completion AI step completion checkpoint: {}",
                        e
                    );
                }

                // Save the AI output as the task summary (only on success)
                // The completion AI output should be the summary shown in the recap page
                if result.success && !result.output.is_empty() {
                    if let Err(e) = self.checkpoint_db.update_task_summary(
                        execution_id,
                        &result.output,
                        true, // goal_achieved = true since verification passed
                        None, // remaining_work = None since task is complete
                    ) {
                        warn!("Failed to save completion AI output as summary: {}", e);
                    } else {
                        info!(
                            "Saved completion AI output ({} chars) as task summary",
                            result.output.len()
                        );
                    }
                }

                overall_success = overall_success && result.success;
            }
        }

        (overall_success, all_results)
    }

    /// Run completion and return a unified ExecutionOutcome.
    ///
    /// This uses the IntoOutcome trait to convert the CompletionResult into a
    /// standardized ExecutionOutcome, which is useful for consistent result handling.
    ///
    /// # Arguments
    /// * `config` - The completion configuration
    /// * `logger` - Logger for step events
    ///
    /// # Returns
    /// An `ExecutionOutcome` summarizing the completion phase execution.
    pub async fn run_completion_to_outcome(
        &self,
        config: &CompletionConfig,
        logger: &StepEventLogger,
    ) -> ExecutionOutcome {
        let start = std::time::Instant::now();

        let (success, step_results) = self
            .run_completion(
                &config.automation_steps,
                &config.prompt_steps,
                &config.execution_id,
                &config.workflow_name,
                config.timeout_seconds,
                config.iterations_run,
                logger,
            )
            .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        // Use the IntoOutcome trait for consistent conversion
        let result = CompletionResult {
            success,
            step_results,
        };
        result.into_outcome(duration_ms)
    }

    /// Build context from prior phases (setup, verification, agentic) to give the
    /// completion AI knowledge of what happened during the workflow execution.
    ///
    /// This reads the accumulated output_log, verification results, and findings
    /// from the database and formats them as context that gets prepended to the
    /// completion prompt.
    fn build_prior_phase_context(&self, execution_id: &str, iterations_run: u32) -> String {
        let mut sections = Vec::new();

        sections.push("## Prior Workflow Execution Context\n".to_string());
        sections.push(format!(
            "This workflow ran {} verification-agentic iteration(s) before reaching the completion phase.\n\
             Below is the accumulated output from all prior phases.\n",
            iterations_run
        ));

        // Fetch and include verification test results from step checkpoints
        // This is especially important when verification passes on the first try
        // (no agentic phase runs, so output_log would be empty)
        match self
            .checkpoint_db
            .get_workflow_step_checkpoints(execution_id, "verification", None)
        {
            Ok(checkpoints) if !checkpoints.is_empty() => {
                let mut verification_lines = vec!["### Verification Test Results\n".to_string()];
                let mut passed_count = 0;
                let mut failed_count = 0;

                for checkpoint in &checkpoints {
                    use crate::workflow_state::StepCheckpointStatus;
                    let status_emoji = match checkpoint.status {
                        StepCheckpointStatus::Success => {
                            passed_count += 1;
                            "✓"
                        }
                        StepCheckpointStatus::Failed => {
                            failed_count += 1;
                            "✗"
                        }
                        _ => "○",
                    };
                    let duration = checkpoint
                        .duration_ms
                        .map(|ms| format!(" ({}ms)", ms))
                        .unwrap_or_default();

                    verification_lines.push(format!(
                        "- {} **{}**{}",
                        status_emoji,
                        checkpoint
                            .step_name
                            .as_deref()
                            .unwrap_or(&checkpoint.step_type),
                        duration
                    ));

                    // Include error details for failed checks
                    if checkpoint.status == StepCheckpointStatus::Failed {
                        if let Some(ref error) = checkpoint.error {
                            verification_lines.push(format!("  - Error: {}", error));
                        }
                    }
                }

                verification_lines.push(format!(
                    "\n**Summary:** {} passed, {} failed\n",
                    passed_count, failed_count
                ));
                sections.push(verification_lines.join("\n"));
            }
            Ok(_) => {
                sections.push(
                    "### Verification Test Results\n\nNo verification checkpoints recorded.\n"
                        .to_string(),
                );
            }
            Err(e) => {
                warn!(
                    "Failed to read verification checkpoints for completion context: {}",
                    e
                );
            }
        }

        // Fetch and include accumulated output_log (from agentic phases)
        match self.checkpoint_db.get_full_task_output(execution_id) {
            Ok(output) if !output.is_empty() => {
                let cleaned = crate::summary_generator::strip_output_markers(&output);
                // Truncate to last 50k chars to avoid overwhelming the AI
                let max_chars = 50_000;
                let truncated = if cleaned.len() > max_chars {
                    let start = cleaned.len() - max_chars;
                    format!("...[earlier output truncated]...\n{}", &cleaned[start..])
                } else {
                    cleaned
                };
                sections.push(format!(
                    "### AI Session Output ({} chars)\n\n{}\n",
                    truncated.len(),
                    truncated
                ));
            }
            Ok(_) => {
                // Don't add "no output" message if we already have verification results
                // This is expected when verification passes on the first try
            }
            Err(e) => {
                warn!("Failed to read prior output for completion context: {}", e);
            }
        }

        // Fetch and include findings
        match self.checkpoint_db.get_findings_for_task(execution_id) {
            Ok(findings) if !findings.is_empty() => {
                let findings_section =
                    crate::summary_generator::format_findings_for_summary(&findings);
                sections.push(findings_section);
            }
            Ok(_) => {} // No findings, skip section
            Err(e) => {
                warn!("Failed to read findings for completion context: {}", e);
            }
        }

        sections.join("\n")
    }
}

// =============================================================================
// FromContext Implementations
// =============================================================================

impl FromContext for SetupExecutor {
    fn from_context(context: ExecutorContext) -> Result<Self, ExecutorError> {
        let config_storage = context
            .config_storage()
            .cloned()
            .ok_or(ExecutorError::missing("config_storage"))?;
        let pid_tracker = context
            .pid_tracker()
            .cloned()
            .ok_or(ExecutorError::missing("pid_tracker"))?;

        Ok(Self::new(
            context.app_state,
            config_storage,
            context.app_handle,
            pid_tracker,
        ))
    }
}

impl FromContext for VerificationExecutor {
    fn from_context(context: ExecutorContext) -> Result<Self, ExecutorError> {
        let config_storage = context
            .config_storage()
            .cloned()
            .ok_or(ExecutorError::missing("config_storage"))?;

        Ok(Self::new(
            context.app_state,
            config_storage,
            context.app_handle,
        ))
    }
}

impl FromContext for AgenticExecutor {
    fn from_context(context: ExecutorContext) -> Result<Self, ExecutorError> {
        let pid_tracker = context
            .pid_tracker()
            .cloned()
            .ok_or(ExecutorError::missing("pid_tracker"))?;

        Ok(Self::new(
            context.app_state,
            context.app_handle,
            pid_tracker,
        ))
    }
}

impl FromContext for CompletionExecutor {
    fn from_context(context: ExecutorContext) -> Result<Self, ExecutorError> {
        let config_storage = context
            .config_storage()
            .cloned()
            .ok_or(ExecutorError::missing("config_storage"))?;
        let pid_tracker = context
            .pid_tracker()
            .cloned()
            .ok_or(ExecutorError::missing("pid_tracker"))?;

        Ok(Self::new(
            context.app_state,
            config_storage,
            context.app_handle,
            pid_tracker,
        ))
    }
}

// =============================================================================
// Executor Trait Implementations
// =============================================================================

/// Wrapper to hold a logger for async execution.
/// Since SetupConfig can't own the logger (it's borrowed), we need a separate
/// struct that contains everything needed for execution.
pub struct SetupExecutionRequest<'a> {
    pub config: SetupConfig,
    pub logger: &'a StepEventLogger,
}

#[async_trait]
impl Executor for SetupExecutor {
    type Config = SetupConfig;
    type Output = SetupResult;

    async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError> {
        // Create a logger for this execution
        // Note: This is a simplified version that doesn't use the StepEventLogger
        // because the trait interface doesn't allow passing borrowed references.
        // For full logging support, use the direct execute() method with a logger.
        let (success, step_results) = self
            .run_setup(
                &config.automation_steps,
                &config.prompt_steps,
                &config.execution_id,
                &config.workflow_name,
                config.timeout_seconds,
                &StepEventLogger::noop(),
            )
            .await;

        Ok(SetupResult {
            success,
            step_results,
        })
    }

    fn name(&self) -> &'static str {
        "setup"
    }
}

#[async_trait]
impl Executor for VerificationExecutor {
    type Config = VerificationConfig;
    type Output = VerificationResult;

    async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError> {
        let (phase_result, step_results) = self
            .run_verification(
                &config.steps,
                &config.execution_id,
                config.iteration,
                &config.workflow_name,
                &StepEventLogger::noop(),
            )
            .await;

        Ok(VerificationResult {
            phase_result,
            step_results,
        })
    }

    fn name(&self) -> &'static str {
        "verification"
    }
}

#[async_trait]
impl Executor for AgenticExecutor {
    type Config = AgenticConfig;
    type Output = AgenticOutcome;

    async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError> {
        // Build a LoopConfig from AgenticConfig
        let loop_config = LoopConfig {
            max_iterations: config.max_iterations,
            timeout_seconds: config.timeout_seconds,
            base_prompt: config.base_prompt,
            workflow_name: config.workflow_name,
            workflow_id: config.workflow_id,
            execution_id: config.execution_id.clone(),
            targeted_error_ids: Vec::new(),
            starting_iteration: 0,
            run_agentic_first: false,
        };

        let outcome = self
            .run_agentic(
                &loop_config,
                config.iteration,
                &config.failure_context,
                config.has_agentic_steps,
                &StepEventLogger::noop(),
            )
            .await;

        Ok(outcome)
    }

    fn name(&self) -> &'static str {
        "agentic"
    }
}

#[async_trait]
impl Executor for CompletionExecutor {
    type Config = CompletionConfig;
    type Output = CompletionResult;

    async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError> {
        let (success, step_results) = self
            .run_completion(
                &config.automation_steps,
                &config.prompt_steps,
                &config.execution_id,
                &config.workflow_name,
                config.timeout_seconds,
                config.iterations_run,
                &StepEventLogger::noop(),
            )
            .await;

        Ok(CompletionResult {
            success,
            step_results,
        })
    }

    fn name(&self) -> &'static str {
        "completion"
    }
}
