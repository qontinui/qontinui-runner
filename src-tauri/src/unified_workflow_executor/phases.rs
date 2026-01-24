//! Phase executors for the unified workflow.
//!
//! Each phase has a dedicated executor that handles a single responsibility:
//! - SetupExecutor: Runs one-time setup steps
//! - VerificationExecutor: Runs verification/test steps and reports results
//! - AgenticExecutor: Runs the AI with failure context
//! - CompletionExecutor: Runs completion steps (only if verification passed)

use std::sync::Arc;
use tracing::{error, info, warn};

use crate::config_storage::ConfigStorage;
use crate::database::CheckpointDb;
use crate::step_event_builder::StepEventBuilder;
use crate::step_executor::{
    ExecutionStepConfig, StepExecutionResult, StepExecutor, VerificationPhaseResult,
};
use crate::step_metadata::{StepDetails, StepMetadata};
use crate::step_types::StepType;
use crate::AppState;

use super::types::{AgenticOutcome, LoopConfig};

// =============================================================================
// Helper Functions
// =============================================================================

/// Strip [TASK_COMPLETE] and similar completion marker instructions from prompts.
///
/// In unified workflows, verification determines completion, not the AI.
/// This removes any instructions telling the AI to output completion markers.
fn strip_completion_marker_instructions(prompt: &str) -> String {
    // Common patterns that instruct AI to output completion markers
    let patterns_to_remove = [
        // Full sentences with marker instructions
        "When you complete the task, include a summary line starting with [TASK_COMPLETE] followed by a brief summary.",
        "When complete, print [TASK_COMPLETE].",
        "When the goal is VERIFIED achieved, print [TASK_COMPLETE].",
        "Continue the task. When complete, print [TASK_COMPLETE].",
        "Continue the task. When the goal is VERIFIED achieved, print [TASK_COMPLETE].",
        "Continue the task from where you left off. When complete, print [TASK_COMPLETE].",
        // Shorter variations
        "print [TASK_COMPLETE]",
        "output [TASK_COMPLETE]",
        "[TASK_COMPLETE]",
    ];

    let mut result = prompt.to_string();
    for pattern in patterns_to_remove {
        result = result.replace(pattern, "");
    }

    // Clean up any resulting double newlines
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result.trim().to_string()
}

// =============================================================================
// Setup Phase Executor
// =============================================================================

/// Executes the setup phase (runs once at the start).
///
/// Handles both automation steps (shell commands, workflows) and prompt steps (AI tasks).
pub struct SetupExecutor {
    executor: StepExecutor,
    app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    checkpoint_db: Arc<CheckpointDb>,
}

impl SetupExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        Self {
            executor: StepExecutor::with_app_handle(
                app_state.clone(),
                config_storage,
                app_handle.clone(),
            ),
            app_handle,
            pid_tracker,
            checkpoint_db: app_state.checkpoint_db.clone(),
        }
    }

    /// Run setup steps. Returns true if successful.
    ///
    /// Executes automation steps first (shell commands, etc.), then prompt steps (AI tasks).
    pub async fn execute(
        &self,
        automation_steps: &[ExecutionStepConfig],
        prompt_steps: &[ExecutionStepConfig],
        execution_id: &str,
        workflow_name: &str,
        timeout_seconds: u64,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Run automation setup steps first
        if !automation_steps.is_empty() {
            info!(
                "SETUP-PHASE: Running {} automation steps",
                automation_steps.len()
            );

            let (result, _has_gui) = self
                .executor
                .execute_setup_phase(automation_steps, execution_id, &[])
                .await;

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

            // Get the combined step name for display
            let step_name = prompt_steps
                .iter()
                .filter_map(|s| s.name.as_ref())
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(" + ");
            let step_name = if step_name.is_empty() {
                "Setup AI Task".to_string()
            } else {
                step_name
            };

            let setup_prompt: String = prompt_steps
                .iter()
                .filter_map(|s| s.prompt_content.as_ref())
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");

            if !setup_prompt.is_empty() {
                let success = self
                    .run_setup_ai(
                        &setup_prompt,
                        execution_id,
                        workflow_name,
                        timeout_seconds,
                        &step_name,
                    )
                    .await;
                overall_success = overall_success && success;

                if !success {
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

    async fn run_setup_ai(
        &self,
        prompt: &str,
        execution_id: &str,
        workflow_name: &str,
        timeout_seconds: u64,
        step_name: &str,
    ) -> bool {
        let session_id = format!("{}-setup", execution_id);
        let start_time = std::time::Instant::now();

        let workspace_root = crate::mcp_api::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        let pid_tracker = self.pid_tracker.clone();
        let retry_config = crate::settings::get_ai_settings().retry;
        let app_handle = self.app_handle.clone();

        let session_ctx = Some(crate::mcp_api::AiOutputSessionContext {
            session_id: Some(session_id.clone()),
            session_name: Some(format!("{} - Setup", workflow_name)),
            phase: Some("setup".to_string()),
        });

        let finding_ctx = Some(crate::mcp_api::FindingContext {
            task_run_id: execution_id.to_string(),
            session_num: 0, // Setup is session 0
        });

        info!("SETUP-PHASE: Running setup AI (session: {})", session_id);

        // Create metadata and builder for consistent events
        let metadata = StepMetadata::setup(StepType::Prompt, step_name, 0);
        let details = StepDetails::ai_session(session_id.clone());
        let builder = StepEventBuilder::new(execution_id, metadata)
            .with_details(details)
            .with_workflow_name(workflow_name);

        // Log start event
        let start_event = builder.build_start();
        if let Err(e) = self.checkpoint_db.create_task_run_event(&start_event) {
            warn!("Failed to log setup AI start event: {}", e);
        }

        // Strip completion marker instructions - verification determines completion
        let prompt_for_claude = strip_completion_marker_instructions(prompt);
        let workspace_for_claude = workspace_root;
        let sid_for_claude = session_id.clone();

        let result = tokio::task::spawn_blocking(move || {
            crate::mcp_api::run_claude_session_with_retry(
                &workspace_for_claude,
                &prompt_for_claude,
                &sid_for_claude,
                &app_handle,
                timeout_seconds,
                session_ctx,
                finding_ctx,
                Some(pid_tracker),
                Some(&retry_config),
            )
        })
        .await;

        let duration_ms = start_time.elapsed().as_millis() as i64;

        // Rebuild builder with updated details for completion event
        let metadata = StepMetadata::setup(StepType::Prompt, step_name, 0);

        match result {
            Ok(Ok((success, output, _))) => {
                info!(
                    "SETUP-PHASE: AI completed (success={}, output={} chars, duration={}ms)",
                    success,
                    output.len(),
                    duration_ms
                );

                let details =
                    StepDetails::ai_session_complete(session_id.clone(), output.len(), duration_ms);
                let builder = StepEventBuilder::new(execution_id, metadata)
                    .with_details(details)
                    .with_workflow_name(workflow_name);

                let event = if success {
                    builder.build_complete(duration_ms)
                } else {
                    builder.build_error(duration_ms, Some("AI reported failure"))
                };

                if let Err(e) = self.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to log setup AI complete event: {}", e);
                }

                success
            }
            Ok(Err(e)) => {
                error!("SETUP-PHASE: AI failed: {}", e);

                let details = StepDetails::ai_session(session_id.clone()).with_error(e.to_string());
                let builder = StepEventBuilder::new(execution_id, metadata)
                    .with_details(details)
                    .with_workflow_name(workflow_name);

                let event = builder.build_error(duration_ms, Some(&e));

                if let Err(log_err) = self.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to log setup AI error event: {}", log_err);
                }

                false
            }
            Err(e) => {
                error!("SETUP-PHASE: Task join error: {}", e);

                let details = StepDetails::ai_session(session_id.clone()).with_error(e.to_string());
                let builder = StepEventBuilder::new(execution_id, metadata)
                    .with_details(details)
                    .with_workflow_name(workflow_name);

                let event =
                    builder.build_error(duration_ms, Some(&format!("Task join error: {}", e)));

                if let Err(log_err) = self.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to log setup AI error event: {}", log_err);
                }

                false
            }
        }
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
        Self {
            executor: StepExecutor::with_app_handle(app_state.clone(), config_storage, app_handle),
            checkpoint_db: app_state.checkpoint_db.clone(),
        }
    }

    /// Run verification steps.
    ///
    /// Returns (verification_result, step_results)
    pub async fn execute(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
        workflow_name: &str,
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
                },
                Vec::new(),
            );
        }

        info!(
            "VERIFICATION-PHASE: Running {} steps (iteration {})",
            steps.len(),
            iteration
        );

        // Log START events for each step before execution
        for (idx, step) in steps.iter().enumerate() {
            let step_type =
                StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Playwright);
            let step_name = step.name.as_deref().unwrap_or(&step.step_type);
            let metadata = StepMetadata::verification(step_type, step_name, idx, iteration);
            let builder =
                StepEventBuilder::new(execution_id, metadata).with_workflow_name(workflow_name);

            let start_event = builder.build_start();
            if let Err(e) = self.checkpoint_db.create_task_run_event(&start_event) {
                warn!("Failed to log verification step start event: {}", e);
            }
        }

        let result = self
            .executor
            .execute_verification_steps(steps, execution_id, iteration)
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

        // Log step_execution completion events for each verification step for Timeline widget
        for step_result in &result.step_results {
            let step_type =
                StepType::from_str_compat(&step_result.step_type).unwrap_or(StepType::Playwright);
            let metadata = StepMetadata::verification(
                step_type,
                &step_result.step_name,
                step_result.step_index,
                iteration,
            );

            let details = if step_result.success {
                StepDetails::default().with_duration(step_result.duration_ms as i64)
            } else {
                StepDetails::default()
                    .with_duration(step_result.duration_ms as i64)
                    .with_error(step_result.error.clone().unwrap_or_default())
            };

            let builder = StepEventBuilder::new(execution_id, metadata)
                .with_details(details)
                .with_workflow_name(workflow_name);

            let event = if step_result.success {
                builder.build_complete(step_result.duration_ms as i64)
            } else {
                builder.build_error(step_result.duration_ms as i64, step_result.error.as_deref())
            };

            if let Err(e) = self.checkpoint_db.create_task_run_event(&event) {
                warn!("Failed to log verification step event: {}", e);
            }
        }

        let step_results = result.step_results.clone();
        (result, step_results)
    }
}

// =============================================================================
// Agentic Phase Executor
// =============================================================================

/// Executes the AI agentic phase with failure context.
pub struct AgenticExecutor {
    app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    checkpoint_db: Arc<CheckpointDb>,
}

impl AgenticExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        Self {
            app_handle,
            pid_tracker,
            checkpoint_db: app_state.checkpoint_db.clone(),
        }
    }

    /// Run the AI with the given prompt and failure context.
    ///
    /// This calls Claude directly (no session system, no orchestrator).
    pub async fn execute(
        &self,
        config: &LoopConfig,
        iteration: u32,
        failure_context: &str,
        has_agentic_steps: bool,
    ) -> AgenticOutcome {
        if !has_agentic_steps {
            info!(
                "AGENTIC-PHASE: No agentic steps defined, skipping (iteration {})",
                iteration
            );
            return AgenticOutcome::Skipped;
        }

        // Build enhanced prompt with failure context
        // Strip any [TASK_COMPLETE] instructions from the base prompt since
        // in unified workflows, VERIFICATION determines completion, not the AI.
        let clean_base_prompt = strip_completion_marker_instructions(&config.base_prompt);

        let enhanced_prompt = if failure_context.is_empty() {
            warn!(
                "AGENTIC-PHASE: No failure context provided for iteration {} - AI won't know what to fix!",
                iteration
            );
            clean_base_prompt
        } else {
            info!(
                "AGENTIC-PHASE: Building prompt with {} chars of failure context (iteration {})",
                failure_context.len(),
                iteration
            );
            format!(
                "{}\n\n---\n\nThe following verification checks FAILED. Please fix these issues:\n\n{}\n\nFix the issues above and ensure all checks pass.",
                clean_base_prompt, failure_context
            )
        };

        let session_id = format!("{}-agentic-{}", config.execution_id, iteration);
        let step_name = format!("Fix issues (iteration {})", iteration);
        let start_time = std::time::Instant::now();

        let workspace_root = crate::mcp_api::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        let pid_tracker = self.pid_tracker.clone();
        let retry_config = crate::settings::get_ai_settings().retry;
        let app_handle = self.app_handle.clone();

        // Create context for output grouping
        let session_ctx = Some(crate::mcp_api::AiOutputSessionContext {
            session_id: Some(session_id.clone()),
            session_name: Some(format!(
                "{} - Iteration {}",
                config.workflow_name, iteration
            )),
            phase: Some("agentic".to_string()),
        });

        // Create finding context
        let finding_ctx = Some(crate::mcp_api::FindingContext {
            task_run_id: config.execution_id.clone(),
            session_num: iteration,
        });

        info!(
            "AGENTIC-PHASE: Running Claude directly for iteration {} (session: {})",
            iteration, session_id
        );

        // Create metadata and builder for consistent events
        // Use iteration as step_index for agentic AI steps
        let metadata = StepMetadata::agentic(
            StepType::AiSession,
            &step_name,
            iteration as usize,
            iteration,
        );
        let details = StepDetails::ai_session(session_id.clone());
        let builder = StepEventBuilder::new(&config.execution_id, metadata.clone())
            .with_details(details)
            .with_workflow_name(&config.workflow_name);

        // Log start event
        let start_event = builder.build_start();
        if let Err(e) = self.checkpoint_db.create_task_run_event(&start_event) {
            warn!("Failed to log agentic AI start event: {}", e);
        }

        // Run Claude directly - bypasses session system and orchestrator
        let prompt_for_claude = enhanced_prompt;
        let workspace_for_claude = workspace_root;
        let sid_for_claude = session_id.clone();
        let timeout = config.timeout_seconds;

        let result = tokio::task::spawn_blocking(move || {
            crate::mcp_api::run_claude_session_with_retry(
                &workspace_for_claude,
                &prompt_for_claude,
                &sid_for_claude,
                &app_handle,
                timeout,
                session_ctx,
                finding_ctx,
                Some(pid_tracker),
                Some(&retry_config),
            )
        })
        .await;

        let duration_ms = start_time.elapsed().as_millis() as i64;

        match result {
            Ok(Ok((success, output, _retry_state))) => {
                info!(
                    "AGENTIC-PHASE: Claude completed (success={}, output={} chars, duration={}ms) for iteration {}",
                    success,
                    output.len(),
                    duration_ms,
                    iteration
                );

                let details =
                    StepDetails::ai_session_complete(session_id.clone(), output.len(), duration_ms);
                let builder = StepEventBuilder::new(&config.execution_id, metadata)
                    .with_details(details)
                    .with_workflow_name(&config.workflow_name);

                let event = if success {
                    builder.build_complete(duration_ms)
                } else {
                    builder.build_error(duration_ms, Some("AI reported failure"))
                };

                if let Err(e) = self.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to log agentic AI complete event: {}", e);
                }

                if success {
                    AgenticOutcome::Success { output }
                } else {
                    AgenticOutcome::Failed {
                        output: output.clone(),
                        error: "AI reported failure".to_string(),
                    }
                }
            }
            Ok(Err(e)) => {
                error!(
                    "AGENTIC-PHASE: Claude failed with error for iteration {}: {}",
                    iteration, e
                );

                let details = StepDetails::ai_session(session_id.clone()).with_error(e.to_string());
                let builder = StepEventBuilder::new(&config.execution_id, metadata)
                    .with_details(details)
                    .with_workflow_name(&config.workflow_name);

                let event = builder.build_error(duration_ms, Some(&e));

                if let Err(log_err) = self.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to log agentic AI error event: {}", log_err);
                }

                AgenticOutcome::Error { error: e }
            }
            Err(e) => {
                error!(
                    "AGENTIC-PHASE: Task join error for iteration {}: {}",
                    iteration, e
                );

                let details = StepDetails::ai_session(session_id.clone()).with_error(e.to_string());
                let builder = StepEventBuilder::new(&config.execution_id, metadata)
                    .with_details(details)
                    .with_workflow_name(&config.workflow_name);

                let event =
                    builder.build_error(duration_ms, Some(&format!("Task join error: {}", e)));

                if let Err(log_err) = self.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to log agentic AI error event: {}", log_err);
                }

                AgenticOutcome::Error {
                    error: format!("Task join error: {}", e),
                }
            }
        }
    }
}

// =============================================================================
// Completion Phase Executor
// =============================================================================

/// Executes the completion phase (runs once after verification passes).
pub struct CompletionExecutor {
    executor: StepExecutor,
    app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    checkpoint_db: Arc<CheckpointDb>,
}

impl CompletionExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        Self {
            executor: StepExecutor::with_app_handle(
                app_state.clone(),
                config_storage,
                app_handle.clone(),
            ),
            app_handle,
            pid_tracker,
            checkpoint_db: app_state.checkpoint_db.clone(),
        }
    }

    /// Run completion steps.
    ///
    /// This should ONLY be called when verification has passed.
    pub async fn execute(
        &self,
        automation_steps: &[ExecutionStepConfig],
        prompt_steps: &[ExecutionStepConfig],
        execution_id: &str,
        workflow_name: &str,
        timeout_seconds: u64,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Run automation completion steps
        if !automation_steps.is_empty() {
            info!(
                "COMPLETION-PHASE: Running {} automation steps",
                automation_steps.len()
            );

            let result = self
                .executor
                .execute_completion_phase(automation_steps, execution_id, &[])
                .await;

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

            // Get the combined step name for display
            let step_name = prompt_steps
                .iter()
                .filter_map(|s| s.name.as_ref())
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(" + ");
            let step_name = if step_name.is_empty() {
                "Completion AI Task".to_string()
            } else {
                step_name
            };

            let completion_prompt: String = prompt_steps
                .iter()
                .filter_map(|s| s.prompt_content.as_ref())
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");

            if !completion_prompt.is_empty() {
                let success = self
                    .run_completion_ai(
                        &completion_prompt,
                        execution_id,
                        workflow_name,
                        timeout_seconds,
                        &step_name,
                    )
                    .await;
                overall_success = overall_success && success;
            }
        }

        (overall_success, all_results)
    }

    async fn run_completion_ai(
        &self,
        prompt: &str,
        execution_id: &str,
        workflow_name: &str,
        timeout_seconds: u64,
        step_name: &str,
    ) -> bool {
        let session_id = format!("{}-completion", execution_id);
        let start_time = std::time::Instant::now();

        let workspace_root = crate::mcp_api::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        let pid_tracker = self.pid_tracker.clone();
        let retry_config = crate::settings::get_ai_settings().retry;
        let app_handle = self.app_handle.clone();

        let session_ctx = Some(crate::mcp_api::AiOutputSessionContext {
            session_id: Some(session_id.clone()),
            session_name: Some(format!("{} - Completion", workflow_name)),
            phase: Some("completion".to_string()),
        });

        let finding_ctx = Some(crate::mcp_api::FindingContext {
            task_run_id: execution_id.to_string(),
            session_num: 999,
        });

        info!(
            "COMPLETION-PHASE: Running completion AI (session: {})",
            session_id
        );

        // Create metadata and builder for consistent events
        let metadata = StepMetadata::completion(StepType::Prompt, step_name, 0);
        let details = StepDetails::ai_session(session_id.clone());
        let builder = StepEventBuilder::new(execution_id, metadata.clone())
            .with_details(details)
            .with_workflow_name(workflow_name);

        // Log start event (previously missing!)
        let start_event = builder.build_start();
        if let Err(e) = self.checkpoint_db.create_task_run_event(&start_event) {
            warn!("Failed to log completion AI start event: {}", e);
        }

        // Strip completion marker instructions for consistency, even though
        // the completion phase runs after verification passed
        let prompt_for_claude = strip_completion_marker_instructions(prompt);
        let workspace_for_claude = workspace_root;
        let sid_for_claude = session_id.clone();

        let result = tokio::task::spawn_blocking(move || {
            crate::mcp_api::run_claude_session_with_retry(
                &workspace_for_claude,
                &prompt_for_claude,
                &sid_for_claude,
                &app_handle,
                timeout_seconds,
                session_ctx,
                finding_ctx,
                Some(pid_tracker),
                Some(&retry_config),
            )
        })
        .await;

        let duration_ms = start_time.elapsed().as_millis() as i64;

        match result {
            Ok(Ok((success, output, _))) => {
                info!(
                    "COMPLETION-PHASE: AI completed (success={}, output={} chars, duration={}ms)",
                    success,
                    output.len(),
                    duration_ms
                );

                let details =
                    StepDetails::ai_session_complete(session_id.clone(), output.len(), duration_ms);
                let builder = StepEventBuilder::new(execution_id, metadata)
                    .with_details(details)
                    .with_workflow_name(workflow_name);

                let event = if success {
                    builder.build_complete(duration_ms)
                } else {
                    builder.build_error(duration_ms, Some("AI reported failure"))
                };

                if let Err(e) = self.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to log completion AI complete event: {}", e);
                }

                // Save the AI output as the task summary
                // The completion AI output should be the summary shown in the recap page
                if success && !output.is_empty() {
                    if let Err(e) = self.checkpoint_db.update_task_summary(
                        execution_id,
                        &output,
                        true, // goal_achieved = true since verification passed
                        None, // remaining_work = None since task is complete
                    ) {
                        warn!("Failed to save completion AI output as summary: {}", e);
                    } else {
                        info!(
                            "Saved completion AI output ({} chars) as task summary",
                            output.len()
                        );
                    }
                }

                success
            }
            Ok(Err(e)) => {
                error!("COMPLETION-PHASE: AI failed: {}", e);

                let details = StepDetails::ai_session(session_id.clone()).with_error(e.to_string());
                let builder = StepEventBuilder::new(execution_id, metadata)
                    .with_details(details)
                    .with_workflow_name(workflow_name);

                let event = builder.build_error(duration_ms, Some(&e));

                if let Err(log_err) = self.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to log completion AI error event: {}", log_err);
                }

                false
            }
            Err(e) => {
                error!("COMPLETION-PHASE: Task join error: {}", e);

                let details = StepDetails::ai_session(session_id.clone()).with_error(e.to_string());
                let builder = StepEventBuilder::new(execution_id, metadata)
                    .with_details(details)
                    .with_workflow_name(workflow_name);

                let event =
                    builder.build_error(duration_ms, Some(&format!("Task join error: {}", e)));

                if let Err(log_err) = self.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to log completion AI error event: {}", log_err);
                }

                false
            }
        }
    }
}
