use std::sync::Arc;
use tracing::{info, instrument, warn};

use crate::config_storage::ConfigStorage;
use crate::executor::{prompt_builder, timeout_helper, ExecutionOutcome, IntoOutcome};
use crate::step_executor::{ExecutionStepConfig, StepExecutionResult, StepExecutor};
use crate::step_metadata::{StepDetails, StepMetadata};
use crate::step_registry::{StepEventKind, StepEventLogger};
use crate::step_types::StepType;
use crate::unified_ai_session::{AiSessionConfig, UnifiedAiSessionExecutor};
use crate::workflow_state::{CheckpointManager, StepCheckpoint};
use crate::AppState;

use super::super::phase_configs::{CompletionConfig, CompletionResult};
use super::super::phase_helpers::{
    build_llm_metrics, execute_prompt_response_mode, record_phase_token_usage,
};

// =============================================================================
// Completion Phase Executor
// =============================================================================

/// Executes the completion phase (runs once after verification passes).
/// AI session execution is delegated to the UnifiedAiSessionExecutor.
pub struct CompletionExecutor {
    pub(crate) app_state: Arc<AppState>,
    executor: StepExecutor,
    ai_executor: UnifiedAiSessionExecutor,
}

impl CompletionExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        Self {
            app_state: app_state.clone(),
            executor: StepExecutor::with_app_handle(
                app_state.clone(),
                config_storage,
                app_handle.clone(),
            ),
            ai_executor: UnifiedAiSessionExecutor::new(app_state, app_handle, pid_tracker),
        }
    }

    /// Enable interactive sessions via the session manager.
    pub fn set_session_manager(&mut self, sm: Arc<crate::claude_session::SessionManager>) {
        self.ai_executor.session_manager = Some(sm);
    }

    /// Set the middleware chain on the inner AI session executor.
    pub fn set_middleware_chain(
        &mut self,
        chain: crate::ai_provider::middleware::AiMiddlewareChain,
    ) {
        self.ai_executor.middleware_chain = Some(chain);
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
    /// After completion automation steps run, this contains output variables
    /// (e.g., `evaluation_results`) that need substitution into prompt step content.
    pub fn shared_variables(
        &self,
    ) -> &crate::orchestrator::context_propagation::SharedVariableStore {
        self.executor.shared_variables()
    }

    /// Run completion steps.
    ///
    /// This should ONLY be called when verification has passed.
    ///
    /// # Arguments
    /// * `iterations_run` - Number of verification-agentic iterations that were executed.
    ///   Used to calculate the correct turn number for the completion phase.
    /// * `logger` - Required logger for consistent step event logging.
    ///
    /// Step checkpointing is integrated for resume capability.
    #[instrument(
        name = "qontinui.workflow.phase.completion",
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
        iterations_run: u32,
        logger: &StepEventLogger,
        stage_index: Option<u32>,
        model_override: Option<String>,
        provider_override: Option<String>,
        completion_prompts_first: bool,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Filter out dev_mode_only automation steps when not in dev mode
        let automation_steps: Vec<ExecutionStepConfig> = automation_steps
            .iter()
            .filter(|step| {
                if step.dev_mode_only.unwrap_or(false) && !cfg!(debug_assertions) {
                    info!(
                        "COMPLETION-PHASE: Skipping dev-mode-only automation step: {:?}",
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
        let checkpoint_mgr = CheckpointManager::new("unified");

        // When completion_prompts_first is set, run prompts before automation.
        // This is used by meta-workflows where the AI hardener must run before
        // save_workflow_artifact persists the final result.
        if completion_prompts_first {
            info!("COMPLETION-PHASE: Running prompts-first order (completion_prompts_first=true)");

            // Run prompts first
            let (prompt_ok, prompt_results) = self
                .run_completion_prompts(
                    prompt_steps,
                    execution_id,
                    workflow_name,
                    iterations_run,
                    logger,
                    stage_index,
                    model_override.clone(),
                    provider_override.clone(),
                    &checkpoint_mgr,
                    0, // prompts run first, so prior_step_count is 0
                )
                .await;
            overall_success = overall_success && prompt_ok;
            let prompt_count = prompt_results.len();
            all_results.extend(prompt_results);

            // Then run automation
            let (auto_ok, auto_results) = self
                .run_completion_automation(
                    automation_steps,
                    execution_id,
                    logger,
                    stage_index,
                    &checkpoint_mgr,
                    prompt_count, // automation runs second
                )
                .await;
            overall_success = overall_success && auto_ok;
            all_results.extend(auto_results);

            return (overall_success, all_results);
        }

        // Default order: automation first, then prompts
        let (auto_ok, auto_results) = self
            .run_completion_automation(
                automation_steps,
                execution_id,
                logger,
                stage_index,
                &checkpoint_mgr,
                0, // automation runs first
            )
            .await;
        overall_success = overall_success && auto_ok;
        let auto_count = auto_results.len();
        all_results.extend(auto_results);

        let (prompt_ok, prompt_results) = self
            .run_completion_prompts(
                prompt_steps,
                execution_id,
                workflow_name,
                iterations_run,
                logger,
                stage_index,
                model_override,
                provider_override,
                &checkpoint_mgr,
                auto_count, // prompts run second
            )
            .await;
        overall_success = overall_success && prompt_ok;
        all_results.extend(prompt_results);

        (overall_success, all_results)
    }

    /// Run completion automation steps with checkpointing.
    ///
    /// This is extracted from `run_completion` so both the default order
    /// (automation-first) and the prompts-first order can share the same code.
    ///
    /// `step_index_offset` is used to offset checkpoint step indices when
    /// another phase has already run (e.g., prompts ran first).
    async fn run_completion_automation(
        &self,
        automation_steps: &[ExecutionStepConfig],
        execution_id: &str,
        _logger: &StepEventLogger,
        stage_index: Option<u32>,
        checkpoint_mgr: &CheckpointManager,
        step_index_offset: usize,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        if !automation_steps.is_empty() {
            info!(
                "COMPLETION-PHASE: Running {} automation steps",
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
                    "completion",
                    Some(0),
                    step_index_offset + idx,
                    step_type.as_str(),
                )
                .with_step_name(step_name)
                .with_stage_index(stage_index);
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
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Command);
                let step_name = step.name.as_deref().unwrap_or(&step.step_type);

                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "completion",
                    Some(0),
                    step_index_offset + idx,
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

        (overall_success, all_results)
    }

    /// Run completion prompt steps (both response-mode and session-mode) with checkpointing.
    ///
    /// This is extracted from `run_completion` so both the default order
    /// (automation-first) and the prompts-first order can share the same code.
    ///
    /// `step_index_offset` is the base offset for checkpoint step indices
    /// (e.g., the number of automation steps that ran before prompts).
    #[allow(clippy::too_many_arguments)]
    async fn run_completion_prompts(
        &self,
        prompt_steps: &[ExecutionStepConfig],
        execution_id: &str,
        workflow_name: &str,
        iterations_run: u32,
        logger: &StepEventLogger,
        stage_index: Option<u32>,
        model_override: Option<String>,
        provider_override: Option<String>,
        checkpoint_mgr: &CheckpointManager,
        step_index_offset: usize,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Expand runtime variables in prompt step content before execution.
        // Variables set by automation steps (e.g., evaluation_results) need to be
        // substituted into {{variable_name}} patterns in prompt content.
        let prompt_steps: Vec<ExecutionStepConfig> = {
            let shared_vars = self.executor.shared_variables().get_all();
            if shared_vars.is_empty() {
                prompt_steps.to_vec()
            } else {
                prompt_steps
                    .iter()
                    .map(|step| {
                        let mut step = step.clone();
                        if let Some(ref mut content) = step.prompt_content {
                            for (name, value) in &shared_vars {
                                let pattern = format!("{{{{{}}}}}", name);
                                if content.contains(&pattern) {
                                    *content = content.replace(&pattern, value);
                                }
                            }
                        }
                        step
                    })
                    .collect()
            }
        };
        let prompt_steps = prompt_steps.as_slice();

        if !prompt_steps.is_empty() {
            info!(
                "COMPLETION-PHASE: Running {} prompt steps (AI summary)",
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
                        "COMPLETION-PHASE: Executing response-mode prompt step: {}",
                        step_name
                    );

                    // Checkpoint the response-mode prompt step as "running"
                    let step_idx = step_index_offset + response_step_count;
                    let mut resp_checkpoint = StepCheckpoint::new(
                        execution_id,
                        "unified",
                        "completion",
                        Some(0),
                        step_idx,
                        "prompt",
                    )
                    .with_step_name(step_name)
                    .with_stage_index(stage_index);
                    resp_checkpoint.mark_started();
                    if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                        warn!(
                            "Failed to save completion response-mode step checkpoint: {}",
                            e
                        );
                    }

                    // Log start event for Active Dashboard visibility
                    let metadata = StepMetadata::completion(
                        execution_id,
                        StepType::Prompt,
                        step_name,
                        step_idx,
                    );
                    if let Err(e) = logger.log_start(
                        StepEventKind::CompletionAiStart,
                        metadata,
                        StepDetails::default(),
                    ) {
                        warn!("Failed to log completion AI step start event: {}", e);
                    }

                    let doctor_handle = self.app_state.doctor_handle.lock().await.clone();
                    let start = std::time::Instant::now();
                    // Step-level overrides take precedence over phase-level
                    let step_model = step.model.clone().or_else(|| model_override.clone());
                    let step_provider = step.provider.clone().or_else(|| provider_override.clone());
                    match execute_prompt_response_mode(
                        step,
                        &self.app_state.pg_db,
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
                        Ok(resp) => {
                            let duration_ms = start.elapsed().as_millis() as u64;
                            record_phase_token_usage(
                                &self.app_state.pg_db,
                                execution_id,
                                "completion",
                                stage_index,
                                None,
                                step_model.as_deref(),
                                step_provider.as_deref(),
                                resp.input_tokens,
                                resp.output_tokens,
                                Some(duration_ms),
                            );
                            let llm_metrics = build_llm_metrics(
                                step_model.as_deref(),
                                step_provider.as_deref(),
                                resp.input_tokens,
                                resp.output_tokens,
                            );
                            let output = resp.output;
                            info!(
                                "COMPLETION-PHASE: Response-mode step '{}' completed successfully ({} bytes)",
                                step_name,
                                output.len()
                            );
                            // Persist AI output to chunks for the /output endpoint
                            if !output.is_empty() {
                                let formatted = format!(
                                    "\n--- AI Completion Output ({}) ---\n{}\n",
                                    step_name, output
                                );
                                if let Err(e) = self
                                    .app_state
                                    .pg_db
                                    .append_task_output_ex(execution_id, &formatted, false, false)
                                    .await
                                {
                                    warn!("PG append_task_output_ex failed: {}", e);
                                }
                                if let Err(e) = self
                                    .app_state
                                    .pg_db
                                    .append_task_output_ex(execution_id, &formatted, false, false)
                                    .await
                                {
                                    warn!("Failed to persist completion response-mode AI output to chunks: {}", e);
                                }
                            }
                            // Save completion checkpoint
                            let mut resp_checkpoint = StepCheckpoint::new(
                                execution_id,
                                "unified",
                                "completion",
                                Some(0),
                                step_idx,
                                "prompt",
                            )
                            .with_step_name(step_name)
                            .with_stage_index(stage_index);
                            resp_checkpoint.mark_success(Some(output.clone()), duration_ms as i64);
                            if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                                warn!("Failed to save completion response-mode step completion checkpoint: {}", e);
                            }

                            // Log complete event for Active Dashboard visibility
                            let metadata = StepMetadata::completion(
                                execution_id,
                                StepType::Prompt,
                                step_name,
                                step_idx,
                            );
                            if let Err(e) = logger.log_complete(
                                StepEventKind::CompletionAiComplete,
                                metadata,
                                StepDetails::default(),
                                duration_ms as i64,
                            ) {
                                warn!("Failed to log completion AI step complete event: {}", e);
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
                                output_data: Some(serde_json::json!({
                                    "output": output,
                                    "llm_metrics": llm_metrics,
                                })),
                                required: None,
                                resolved_inputs: None,
                                extracted_values: None,
                                failure_category: None,
                                interrupted: None,
                            });
                        }
                        Err(e) => {
                            let duration_ms = start.elapsed().as_millis() as u64;
                            warn!(
                                "COMPLETION-PHASE: Response-mode step '{}' failed: {}",
                                step_name, e
                            );
                            // Save failure checkpoint
                            let mut resp_checkpoint = StepCheckpoint::new(
                                execution_id,
                                "unified",
                                "completion",
                                Some(0),
                                step_idx,
                                "prompt",
                            )
                            .with_step_name(step_name)
                            .with_stage_index(stage_index);
                            resp_checkpoint.mark_failed(&e, duration_ms as i64);
                            if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                                warn!("Failed to save completion response-mode step failure checkpoint: {}", e);
                            }

                            // Log error event for Active Dashboard visibility
                            let metadata = StepMetadata::completion(
                                execution_id,
                                StepType::Prompt,
                                step_name,
                                step_idx,
                            );
                            if let Err(log_err) = logger.log_error(
                                StepEventKind::CompletionAiError,
                                metadata,
                                StepDetails::default(),
                                duration_ms as i64,
                                Some(&e),
                            ) {
                                warn!("Failed to log completion AI step error event: {}", log_err);
                            }

                            response_step_count += 1;
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
                                required: None,
                                resolved_inputs: None,
                                extracted_values: None,
                                failure_category: None,
                                interrupted: None,
                            });
                            // Completion failures are non-fatal - don't return early
                            overall_success = false;
                        }
                    }
                } else {
                    session_prompt_steps.push(step.clone());
                }
            }

            // Run remaining session-mode prompt steps via consolidated AI session
            if !session_prompt_steps.is_empty() {
                // Checkpoint the AI step as a single step (after any response-mode steps)
                let ai_step_idx = step_index_offset + response_step_count;
                let step_name = prompt_builder::consolidate_step_names_with_default(
                    &session_prompt_steps,
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
                .with_step_name(&step_name)
                .with_stage_index(stage_index);
                ai_checkpoint.mark_started();
                if let Err(e) = checkpoint_mgr.save_step(&ai_checkpoint) {
                    warn!("Failed to save completion AI step checkpoint: {}", e);
                }

                // Log start event for Active Dashboard visibility
                {
                    let metadata = StepMetadata::completion(
                        execution_id,
                        StepType::Prompt,
                        &step_name,
                        ai_step_idx,
                    );
                    if let Err(e) = logger.log_start(
                        StepEventKind::CompletionAiStart,
                        metadata,
                        StepDetails::default(),
                    ) {
                        warn!("Failed to log completion AI session start event: {}", e);
                    }
                }

                // Use structured prompts for granular sub-step tracking
                let (mut completion_prompt, sub_step_metadata) =
                    prompt_builder::consolidate_prompts_structured(
                        &session_prompt_steps,
                        "completion",
                    );

                // Inject prior phase output context so the completion AI knows what happened
                if !completion_prompt.is_empty() {
                    let prior_context =
                        self.build_prior_phase_context(execution_id, iterations_run);
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
                    .with_checkpoint_id(&ai_checkpoint.id)
                    .with_sub_step_metadata(sub_step_metadata)
                    .with_model_override(model_override.clone());

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
                    .with_step_name(&step_name)
                    .with_stage_index(stage_index);

                    if result.success {
                        ai_completion_checkpoint
                            .mark_success(Some(result.output.clone()), duration_ms);
                    } else {
                        ai_completion_checkpoint.mark_failed("AI session failed", duration_ms);
                    }

                    if let Err(e) = checkpoint_mgr.save_step(&ai_completion_checkpoint) {
                        warn!(
                            "Failed to save completion AI step completion checkpoint: {}",
                            e
                        );
                    }

                    // Log complete/error event for Active Dashboard visibility
                    {
                        let metadata = StepMetadata::completion(
                            execution_id,
                            StepType::Prompt,
                            &step_name,
                            ai_step_idx,
                        );
                        if result.success {
                            if let Err(e) = logger.log_complete(
                                StepEventKind::CompletionAiComplete,
                                metadata,
                                StepDetails::default(),
                                duration_ms,
                            ) {
                                warn!("Failed to log completion AI session complete event: {}", e);
                            }
                        } else if let Err(e) = logger.log_error(
                            StepEventKind::CompletionAiError,
                            metadata,
                            StepDetails::default(),
                            duration_ms,
                            Some("AI session failed"),
                        ) {
                            warn!("Failed to log completion AI session error event: {}", e);
                        }
                    }

                    // Don't save completion AI output as summary here --
                    // the async summary generator (summary_generator.rs) produces a proper
                    // aggregated summary across ALL workflow phases after completion.

                    overall_success = overall_success && result.success;
                }
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
                config.iterations_run,
                logger,
                None,
                config.model_override.clone(),
                config.provider_override.clone(),
                config.completion_prompts_first,
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
        // Verification checkpoint reading removed — all persistence now via PgDb.
        sections.push(
            "### Verification Test Results\n\nNo verification checkpoints recorded.\n".to_string(),
        );

        // Fetch and include accumulated output_log (from agentic phases)
        // PG-primary via block_on since build_prior_phase_context is sync
        let output_result: Result<String, String> =
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let pg = self.app_state.pg_db.clone();
                let id = execution_id.to_string();
                let pg_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle.block_on(async move { pg.get_task_output(&id).await })
                }));
                match pg_res {
                    Ok(r) => r,
                    Err(_) => Err("block_on panicked".to_string()),
                }
            } else {
                Err("No tokio runtime available".to_string())
            };
        match output_result {
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

        // Fetch and include findings (PG-primary, SQLite fallback)
        let findings_result = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let pg = self.app_state.pg_db.clone();
            let id = execution_id.to_string();
            let pg_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handle.block_on(async move { pg.get_findings_for_task(&id).await })
            }));
            match pg_res {
                Ok(r) => r,
                Err(_) => Err("block_on panicked".to_string()),
            }
        } else {
            Err("No tokio runtime available".to_string())
        };
        match findings_result {
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

        // Include unresolved errors so the completion AI can report on them.
        // This runs BEFORE the loop_controller marks completion, so workflow-scoped
        // errors are still visible here.
        // PG-primary via block_on since build_prior_phase_context is sync
        let errors_result: Result<Vec<serde_json::Value>, String> =
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let pg = self.app_state.pg_db.clone();
                let pg_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle.block_on(async move { pg.get_unresolved_errors(None, 20).await })
                }));
                match pg_res {
                    Ok(r) => r,
                    Err(_) => Err("block_on panicked".to_string()),
                }
            } else {
                Err("No tokio runtime available".to_string())
            };
        match errors_result {
            Ok(errors) if !errors.is_empty() => {
                let mut workflow_errors = Vec::new();
                let mut pre_existing_errors = Vec::new();

                for e in &errors {
                    let is_workflow_scoped = e["task_run_id"]
                        .as_str()
                        .is_some_and(|id| id == execution_id);
                    if is_workflow_scoped {
                        workflow_errors.push(e);
                    } else {
                        pre_existing_errors.push(e);
                    }
                }

                let mut lines = vec!["### Unresolved Errors (Error Monitor)\n".to_string()];

                if !workflow_errors.is_empty() {
                    lines.push(format!(
                        "**Errors from this workflow run ({}):**",
                        workflow_errors.len()
                    ));
                    for e in &workflow_errors {
                        let severity = e["severity"].as_str().unwrap_or("error");
                        let message = e["message"].as_str().unwrap_or("");
                        lines.push(format!(
                            "- [{}] {}",
                            severity,
                            message.chars().take(200).collect::<String>()
                        ));
                    }
                    lines.push(String::new());
                }

                if !pre_existing_errors.is_empty() {
                    lines.push(format!(
                        "**Pre-existing errors ({}):**",
                        pre_existing_errors.len()
                    ));
                    for e in pre_existing_errors.iter().take(10) {
                        let severity = e["severity"].as_str().unwrap_or("error");
                        let message = e["message"].as_str().unwrap_or("");
                        lines.push(format!(
                            "- [{}] {}",
                            severity,
                            message.chars().take(200).collect::<String>()
                        ));
                    }
                    if pre_existing_errors.len() > 10 {
                        lines.push(format!("... and {} more", pre_existing_errors.len() - 10));
                    }
                    lines.push(String::new());
                }

                lines.push(
                    "Include any relevant errors in your completion summary. \
                     Workflow-scoped errors will be auto-resolved if the workflow succeeded."
                        .to_string(),
                );

                sections.push(lines.join("\n"));
            }
            Ok(_) => {} // No unresolved errors
            Err(e) => {
                warn!(
                    "Failed to read unresolved errors for completion context: {}",
                    e
                );
            }
        }

        sections.join("\n")
    }
}
