//! Setup phase executor for unified workflows.
//!
//! Executes the setup phase (runs once at the start). Handles both automation
//! steps (shell commands, workflows) and prompt steps (AI tasks). AI session
//! execution is delegated to the UnifiedAiSessionExecutor.

use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, instrument, warn};

use crate::config_storage::ConfigStorage;
use crate::database::CheckpointDb;
use crate::executor::{prompt_builder, timeout_helper, ExecutionOutcome, IntoOutcome};
use crate::step_executor::{ExecutionStepConfig, StepExecutionResult, StepExecutor};
use crate::step_metadata::{StepDetails, StepMetadata};
use crate::step_registry::{StepEventKind, StepEventLogger};
use crate::step_types::StepType;
use crate::unified_ai_session::{AiSessionConfig, UnifiedAiSessionExecutor};
use crate::workflow_state::{CheckpointManager, StepCheckpoint};
use crate::AppState;

use super::super::phase_configs::{SetupConfig, SetupResult};
use super::super::phase_helpers::{execute_prompt_response_mode, record_phase_token_usage};

/// Executes the setup phase (runs once at the start).
///
/// Handles both automation steps (shell commands, workflows) and prompt steps (AI tasks).
/// AI session execution is delegated to the UnifiedAiSessionExecutor.
pub struct SetupExecutor {
    app_state: Arc<AppState>,
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
            app_state: app_state.clone(),
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

    /// Set the task run ID on the inner step executor for database logging.
    pub fn set_task_run_id(&mut self, task_run_id: String) {
        self.executor.set_task_run_id(task_run_id);
    }

    /// Set the path scope policy on the inner step executor.
    pub fn set_path_scope_policy(&mut self, policy: crate::paths::PathScopePolicy) {
        self.executor.set_path_scope_policy(policy);
    }

    /// Get the shared variable store from the inner step executor.
    ///
    /// After setup phase completes, this contains all variables set by API steps
    /// (e.g., `source_findings`, `source_knowledge`). These need to be substituted
    /// into the agentic prompt before the agentic phase runs.
    pub fn shared_variables(
        &self,
    ) -> &crate::orchestrator::context_propagation::SharedVariableStore {
        self.executor.shared_variables()
    }

    /// Run setup steps. Returns true if successful.
    ///
    /// Executes automation steps first (shell commands, etc.), then prompt steps (AI tasks).
    /// The logger is required for consistent step event logging.
    ///
    /// Step checkpointing is integrated for resume capability.
    #[instrument(
        name = "qontinui.workflow.phase.setup",
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
        logger: &StepEventLogger,
        stage_index: Option<u32>,
        model_override: Option<String>,
        provider_override: Option<String>,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Filter out dev_mode_only steps when not in dev mode
        let automation_steps: Vec<ExecutionStepConfig> = automation_steps
            .iter()
            .filter(|step| {
                if step.dev_mode_only.unwrap_or(false) && !cfg!(debug_assertions) {
                    info!(
                        "SETUP-PHASE: Skipping dev-mode-only automation step: {:?}",
                        step.name
                    );
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();
        let automation_steps = automation_steps.as_slice();

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
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Command);
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
                .with_step_name(step_name)
                .with_stage_index(stage_index);
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
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Command);
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
                .with_step_name(step_name)
                .with_stage_index(stage_index);

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

            // Separate response-mode steps from session-mode steps
            let mut session_prompt_steps = Vec::new();
            let mut response_step_count = 0usize;
            for step in prompt_steps {
                // Skip dev_mode_only steps when not in dev mode
                if step.dev_mode_only.unwrap_or(false) && !cfg!(debug_assertions) {
                    info!("Skipping dev-mode-only step: {:?}", step.name);
                    continue;
                }

                if step.prompt_mode.as_deref() == Some("response") {
                    let step_name = step.name.as_deref().unwrap_or("Response Prompt");
                    info!(
                        "SETUP-PHASE: Executing response-mode prompt step: {}",
                        step_name
                    );

                    // Checkpoint the response-mode prompt step as "running"
                    let step_idx = automation_steps.len() + response_step_count;
                    let mut resp_checkpoint = StepCheckpoint::new(
                        execution_id,
                        "unified",
                        "setup",
                        Some(0),
                        step_idx,
                        "prompt",
                    )
                    .with_step_name(step_name)
                    .with_stage_index(stage_index);
                    resp_checkpoint.mark_started();
                    if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                        warn!("Failed to save setup response-mode step checkpoint: {}", e);
                    }

                    // Log start event for Active Dashboard visibility
                    let metadata =
                        StepMetadata::setup(execution_id, StepType::Prompt, step_name, step_idx);
                    if let Err(e) = logger.log_start(
                        StepEventKind::SetupAiStart,
                        metadata,
                        StepDetails::default(),
                    ) {
                        warn!("Failed to log setup AI step start event: {}", e);
                    }

                    // Step-level overrides take precedence over phase-level
                    let step_model = step.model.clone().or_else(|| model_override.clone());
                    let step_provider = step.provider.clone().or_else(|| provider_override.clone());

                    // Retry loop for interruption resilience.
                    // Uses the step's retry_count if configured, otherwise falls back to default.
                    let mut retry_count = 0u32;
                    let max_retries = step.retry_count.unwrap_or(2);
                    let retry_delay_ms = step.retry_delay_ms.unwrap_or(10_000);
                    let overall_start = std::time::Instant::now();
                    let resp_result = loop {
                        let doctor_handle = self.app_state.doctor_handle.lock().await.clone();
                        let start = std::time::Instant::now();
                        match execute_prompt_response_mode(
                            step,
                            &self.checkpoint_db,
                            Some(execution_id),
                            doctor_handle,
                            step_model.clone(),
                            step_provider.clone(),
                            None,
                            None,
                            None,
                            None,
                        )
                        .await
                        {
                            Ok(resp) => break Ok((resp, start)),
                            Err(e) => {
                                let duration_ms = start.elapsed().as_millis() as u64;
                                if duration_ms < 5000 && retry_count < max_retries {
                                    retry_count += 1;
                                    let delay_secs = retry_delay_ms as f64 / 1000.0;
                                    warn!(
                                        "SETUP-PHASE: Step '{}' appears interrupted ({}ms < 5s), retry {}/{} after {}s delay",
                                        step_name, duration_ms, retry_count, max_retries, delay_secs
                                    );
                                    tokio::time::sleep(std::time::Duration::from_millis(
                                        retry_delay_ms,
                                    ))
                                    .await;
                                    continue;
                                }
                                break Err(e);
                            }
                        }
                    };

                    match resp_result {
                        Ok((resp, start)) => {
                            let duration_ms = start.elapsed().as_millis() as u64;
                            record_phase_token_usage(
                                &self.checkpoint_db,
                                execution_id,
                                "setup",
                                stage_index,
                                Some(0),
                                step_model.as_deref(),
                                step_provider.as_deref(),
                                resp.input_tokens,
                                resp.output_tokens,
                                Some(duration_ms),
                            );
                            let output = resp.output;
                            info!(
                                "SETUP-PHASE: Response-mode step '{}' completed successfully ({} bytes)",
                                step_name,
                                output.len()
                            );
                            // Persist AI output to chunks for the /output endpoint
                            if !output.is_empty() {
                                let formatted = format!(
                                    "\n--- AI Setup Output ({}) ---\n{}\n",
                                    step_name, output
                                );
                                if let Err(e) = self.checkpoint_db.append_task_output_ex(
                                    execution_id,
                                    &formatted,
                                    false,
                                    false,
                                ) {
                                    warn!("Failed to persist setup response-mode AI output to chunks: {}", e);
                                }
                            }
                            // Save completion checkpoint
                            let mut resp_checkpoint = StepCheckpoint::new(
                                execution_id,
                                "unified",
                                "setup",
                                Some(0),
                                step_idx,
                                "prompt",
                            )
                            .with_step_name(step_name)
                            .with_stage_index(stage_index);
                            resp_checkpoint.mark_success(Some(output.clone()), duration_ms as i64);
                            if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                                warn!("Failed to save setup response-mode step completion checkpoint: {}", e);
                            }

                            // Log complete event for Active Dashboard visibility
                            let metadata = StepMetadata::setup(
                                execution_id,
                                StepType::Prompt,
                                step_name,
                                step_idx,
                            );
                            if let Err(e) = logger.log_complete(
                                StepEventKind::SetupAiComplete,
                                metadata,
                                StepDetails::default(),
                                duration_ms as i64,
                            ) {
                                warn!("Failed to log setup AI step complete event: {}", e);
                            }
                            response_step_count += 1;
                            all_results.push(StepExecutionResult {
                                step_index: all_results.len(),
                                step_type: "prompt".to_string(),
                                step_name: step_name.to_string(),
                                step_id: step.id.clone(),
                                success: true,
                                error: None,
                                screenshot_path: None,
                                started_at: None,
                                ended_at: None,
                                duration_ms,
                                config: crate::step_executor::StepExecutionConfig::default(),
                                verification_details: None,
                                output_data: Some(serde_json::json!({ "output": output })),
                                required: step.required,
                                resolved_inputs: None,
                                extracted_values: None,
                                failure_category: None,
                                interrupted: None,
                            });
                        }
                        Err(e) => {
                            let duration_ms = overall_start.elapsed().as_millis() as u64;
                            response_step_count += 1; // Increment to avoid step_index collisions with subsequent steps
                            warn!(
                                "SETUP-PHASE: Response-mode step '{}' failed: {}",
                                step_name, e
                            );
                            // Save failure checkpoint
                            let mut resp_checkpoint = StepCheckpoint::new(
                                execution_id,
                                "unified",
                                "setup",
                                Some(0),
                                step_idx,
                                "prompt",
                            )
                            .with_step_name(step_name)
                            .with_stage_index(stage_index);
                            resp_checkpoint.mark_failed(&e, duration_ms as i64);
                            if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                                warn!("Failed to save setup response-mode step failure checkpoint: {}", e);
                            }

                            // Log error event for Active Dashboard visibility
                            let metadata = StepMetadata::setup(
                                execution_id,
                                StepType::Prompt,
                                step_name,
                                step_idx,
                            );
                            if let Err(log_err) = logger.log_error(
                                StepEventKind::SetupAiError,
                                metadata,
                                StepDetails::default(),
                                duration_ms as i64,
                                Some(&e),
                            ) {
                                warn!("Failed to log setup AI step error event: {}", log_err);
                            }

                            let is_required = step.required.unwrap_or(true);
                            all_results.push(StepExecutionResult {
                                step_index: all_results.len(),
                                step_type: "prompt".to_string(),
                                step_name: step_name.to_string(),
                                step_id: step.id.clone(),
                                success: false,
                                error: Some(e),
                                screenshot_path: None,
                                started_at: None,
                                ended_at: None,
                                duration_ms,
                                config: crate::step_executor::StepExecutionConfig::default(),
                                verification_details: None,
                                output_data: None,
                                required: step.required,
                                resolved_inputs: None,
                                extracted_values: None,
                                failure_category: None,
                                interrupted: Some(true),
                            });
                            if is_required {
                                return (false, all_results);
                            } else {
                                warn!(
                                    "SETUP-PHASE: Non-required response-mode step '{}' failed, continuing",
                                    step_name
                                );
                                // Non-required step failure doesn't affect overall_success
                            }
                        }
                    }
                } else {
                    session_prompt_steps.push(step.clone());
                }
            }

            // Run remaining session-mode prompt steps via consolidated AI session
            if !session_prompt_steps.is_empty() {
                // Checkpoint the AI step as a single step (after any response-mode steps)
                let ai_step_idx = automation_steps.len() + response_step_count;
                let step_name = prompt_builder::consolidate_step_names_with_default(
                    &session_prompt_steps,
                    "Setup AI Task",
                );

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
                .with_step_name(&step_name)
                .with_stage_index(stage_index);
                ai_checkpoint.mark_started();
                if let Err(e) = checkpoint_mgr.save_step(&ai_checkpoint) {
                    warn!("Failed to save setup AI step checkpoint: {}", e);
                }

                // Log start event for Active Dashboard visibility
                {
                    let metadata = StepMetadata::setup(
                        execution_id,
                        StepType::Prompt,
                        &step_name,
                        ai_step_idx,
                    );
                    if let Err(e) = logger.log_start(
                        StepEventKind::SetupAiStart,
                        metadata,
                        StepDetails::default(),
                    ) {
                        warn!("Failed to log setup AI session start event: {}", e);
                    }
                }

                // Use structured prompts for granular sub-step tracking
                let (setup_prompt, sub_step_metadata) =
                    prompt_builder::consolidate_prompts_structured(&session_prompt_steps, "setup");

                if !setup_prompt.is_empty() {
                    // Use the unified AI session executor with sub-step metadata
                    let config = AiSessionConfig::setup(execution_id, workflow_name, &step_name)
                        .with_checkpoint_id(&ai_checkpoint.id)
                        .with_sub_step_metadata(sub_step_metadata)
                        .with_model_override(model_override.clone());

                    let (result, duration_ms) = timeout_helper::timed_result_async(
                        self.ai_executor.execute(&config, &setup_prompt, logger),
                    )
                    .await;
                    let duration_ms = duration_ms as i64;
                    // Only fail overall setup if at least one session-mode step is required.
                    // Non-required steps failing should not block the setup phase.
                    let any_required = session_prompt_steps
                        .iter()
                        .any(|s| s.required.unwrap_or(true));
                    if any_required {
                        overall_success = overall_success && result.success;
                    }
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
                    .with_step_name(&step_name)
                    .with_stage_index(stage_index);

                    if result.success {
                        ai_checkpoint.mark_success(Some(result.output.clone()), duration_ms);
                    } else {
                        ai_checkpoint.mark_failed("AI session failed", duration_ms);
                    }

                    if let Err(e) = checkpoint_mgr.save_step(&ai_checkpoint) {
                        warn!("Failed to save setup AI step completion checkpoint: {}", e);
                    }

                    // Log complete/error event for Active Dashboard visibility
                    {
                        let metadata = StepMetadata::setup(
                            execution_id,
                            StepType::Prompt,
                            &step_name,
                            ai_step_idx,
                        );
                        if result.success {
                            if let Err(e) = logger.log_complete(
                                StepEventKind::SetupAiComplete,
                                metadata,
                                StepDetails::default(),
                                duration_ms,
                            ) {
                                warn!("Failed to log setup AI session complete event: {}", e);
                            }
                        } else if let Err(e) = logger.log_error(
                            StepEventKind::SetupAiError,
                            metadata,
                            StepDetails::default(),
                            duration_ms,
                            Some("AI session failed"),
                        ) {
                            warn!("Failed to log setup AI session error event: {}", e);
                        }
                    }

                    if !result.success {
                        warn!("SETUP-PHASE: AI prompt steps failed");
                    }
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
                logger,
                None,
                config.model_override.clone(),
                config.provider_override.clone(),
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
