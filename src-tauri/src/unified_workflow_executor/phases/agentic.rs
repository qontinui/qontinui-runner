//! Agentic phase executor.
//!
//! Runs the AI with failure context from verification to fix issues.
//! AI session execution is delegated to the `UnifiedAiSessionExecutor`.

use std::sync::Arc;
use tracing::{info, instrument, warn};

use crate::database::CreateTaskRunEventInput;
use crate::executor::timeout_helper;
use crate::step_executor::ExecutionStepConfig;
use crate::step_registry::StepEventLogger;
use crate::unified_ai_session::{AiSessionConfig, UnifiedAiSessionExecutor};
use crate::workflow_state::{CheckpointManager, StepCheckpoint};
use crate::AppState;

use super::super::output_parser;
use super::super::types::{get_parent_task_id, AgenticOutcome, LoopConfig};
use super::{
    build_compressed_iteration_history, build_execution_timing_context, build_llm_metrics,
    execute_prompt_response_mode, extract_and_preread_failure_files, get_active_sdk_app_name,
    preread_previously_edited_files, record_phase_token_usage, record_phase_token_usage_with_cache,
    record_phase_token_usage_with_target, REFLECTION_MODE_PREAMBLE,
};

// =============================================================================
// Agentic Phase Executor
// =============================================================================

/// Executes the AI agentic phase with failure context.
/// AI session execution is delegated to the UnifiedAiSessionExecutor.
pub struct AgenticExecutor {
    pub(crate) app_state: Arc<AppState>,
    app_handle: tauri::AppHandle,
    ai_executor: UnifiedAiSessionExecutor,
    reflection_fix_ctx: Option<crate::mcp::shared::ReflectionFixContext>,
    step_injection_ctx: Option<crate::step_injection::types::StepInjectionContext>,
    cost_trackers: std::sync::Mutex<Option<Arc<crate::cost_management::RunCostTrackers>>>,
    broadcaster: crate::event_system::SharedEventBroadcaster,
}

impl AgenticExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        Self {
            app_state: app_state.clone(),
            ai_executor: UnifiedAiSessionExecutor::new(app_state, app_handle.clone(), pid_tracker),
            broadcaster: crate::event_system::shared_broadcaster(app_handle.clone()),
            app_handle,
            reflection_fix_ctx: None,
            step_injection_ctx: None,
            cost_trackers: std::sync::Mutex::new(None),
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

    /// Set the reflection fix context for parsing [REFLECTION_FIX:...] markers.
    pub fn set_reflection_fix_ctx(&mut self, ctx: crate::mcp::shared::ReflectionFixContext) {
        self.reflection_fix_ctx = Some(ctx);
    }

    /// Set the step injection context for parsing [INJECT_STEP]...[/INJECT_STEP] markers.
    pub fn set_step_injection_ctx(
        &mut self,
        ctx: crate::step_injection::types::StepInjectionContext,
    ) {
        self.step_injection_ctx = Some(ctx);
    }

    /// Set cost trackers for budget tracking and anomaly detection.
    /// Uses interior mutability so it can be called after Arc wrapping.
    pub fn set_cost_trackers(&self, trackers: Arc<crate::cost_management::RunCostTrackers>) {
        *self.cost_trackers.lock().unwrap() = Some(trackers);
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
        name = "qontinui.workflow.phase.agentic",
        skip(self, config, failure_context, agentic_steps, logger),
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
        agentic_steps: &[ExecutionStepConfig],
        logger: &StepEventLogger,
    ) -> (AgenticOutcome, Vec<ExecutionStepConfig>) {
        if !has_agentic_steps && config.base_prompt.is_empty() {
            info!(
                "AGENTIC-PHASE: No agentic steps and no base prompt, skipping (iteration {})",
                iteration
            );
            return (AgenticOutcome::Skipped, Vec::new());
        }

        // Filter out dev_mode_only steps when not in dev mode
        let agentic_steps: Vec<ExecutionStepConfig> = agentic_steps
            .iter()
            .filter(|step| {
                if step.dev_mode_only.unwrap_or(false) && !cfg!(debug_assertions) {
                    info!(
                        "AGENTIC-PHASE: Skipping dev-mode-only step: {:?}",
                        step.name
                    );
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();
        let agentic_steps = agentic_steps.as_slice();

        if agentic_steps.is_empty() && config.base_prompt.is_empty() {
            info!(
                "AGENTIC-PHASE: No remaining agentic steps and no base prompt, skipping (iteration {})",
                iteration
            );
            return (AgenticOutcome::Skipped, Vec::new());
        }

        // Check if any agentic step uses response mode (only relevant when steps exist)
        let has_response_mode = !agentic_steps.is_empty()
            && agentic_steps
                .iter()
                .any(|s| s.prompt_mode.as_deref() == Some("response"));

        // If response mode, handle with simple prompt->response instead of full session
        if has_response_mode {
            info!(
                "AGENTIC-PHASE: Using response mode for iteration {}",
                iteration
            );

            // Emit start event for Active Dashboard
            let parent_id = get_parent_task_id(&config.execution_id);
            let resp_action_id = format!("agentic-response-{}-0", parent_id);
            let resp_start_event = CreateTaskRunEventInput {
                task_run_id: parent_id.clone(),
                event_type: "step_execution".to_string(),
                event_subtype: Some("start".to_string()),
                message: format!(
                    "Starting agentic response-mode prompt (iteration {})",
                    iteration
                ),
                data: Some(
                    serde_json::to_string(&serde_json::json!({
                        "step_index": 0,
                        "step_type": "prompt",
                        "step_name": "Agentic Response Prompt",
                        "phase": "agentic",
                        "iteration": iteration,
                    }))
                    .unwrap_or_default(),
                ),
                workflow_name: None,
                state_name: None,
                action_id: Some(resp_action_id.clone()),
                timestamp: chrono::Utc::now().to_rfc3339(),
                duration_ms: None,
            };
            // PG-primary: fire-and-forget async write to PostgreSQL
            {
                let pg = self.app_state.pg_db.clone();
                let event_clone = resp_start_event.clone();
                tokio::spawn(async move {
                    if let Err(e) = pg.create_task_run_event(&event_clone).await {
                        tracing::warn!("PG event write failed: {}", e);
                    }
                });
            }
            if let Err(e) = self
                .app_state
                .pg_db
                .create_task_run_event(&resp_start_event)
                .await
            {
                warn!("Failed to emit agentic response-mode start event: {}", e);
            }
            let resp_mode_start = std::time::Instant::now();

            // Build a temporary step with failure context appended to the prompt
            for step in agentic_steps {
                if step.prompt_mode.as_deref() != Some("response") {
                    continue;
                }

                let step_name = step.name.as_deref().unwrap_or("Agentic Response Prompt");

                // Checkpoint the response-mode agentic step as "running"
                let checkpoint_mgr = CheckpointManager::new("unified");
                let mut resp_checkpoint = StepCheckpoint::new(
                    &config.execution_id,
                    "unified",
                    "agentic",
                    Some(iteration),
                    0,
                    "prompt",
                )
                .with_step_name(step_name)
                .with_stage_index(config.stage_index);
                resp_checkpoint.mark_started();
                if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                    warn!(
                        "Failed to save agentic response-mode step checkpoint: {}",
                        e
                    );
                }

                // Create a modified step with failure context appended to the prompt
                let mut modified_step = step.clone();
                let base_prompt = modified_step.prompt_content.clone().unwrap_or_default();
                let enhanced = if failure_context.is_empty() {
                    base_prompt
                } else {
                    format!(
                        "{}\n\n---\n\nThe following verification checks FAILED. Please fix these issues:\n\n{}\n\nFix the issues above and ensure all checks pass.",
                        base_prompt, failure_context
                    )
                };
                modified_step.prompt_content = Some(enhanced);

                let doctor_handle = self.app_state.doctor_handle.lock().await.clone();
                let start = std::time::Instant::now();
                // Step-level overrides take precedence over phase-level
                let step_model = modified_step
                    .model
                    .clone()
                    .or_else(|| config.resolve_model_for_phase("agentic"));
                let step_provider = modified_step
                    .provider
                    .clone()
                    .or_else(|| config.resolve_provider_for_phase("agentic"));
                match execute_prompt_response_mode(
                    &modified_step,
                    &self.app_state.pg_db,
                    Some(&config.execution_id),
                    doctor_handle,
                    step_model.clone(),
                    step_provider.clone(),
                    config.resolve_temperature_for_phase("agentic"),
                    config.resolve_max_tokens_for_phase("agentic"),
                    config.resolve_fallback_model_for_phase("agentic"),
                    config.resolve_fallback_provider_for_phase("agentic"),
                )
                .await
                {
                    Ok(resp) => {
                        let duration_ms = start.elapsed().as_millis() as u64;
                        {
                            let target_app = get_active_sdk_app_name(&self.app_state);
                            record_phase_token_usage_with_cache(
                                &self.app_state.pg_db,
                                &config.execution_id,
                                "agentic",
                                config.stage_index,
                                Some(iteration),
                                step_model.as_deref(),
                                step_provider.as_deref(),
                                resp.input_tokens,
                                resp.output_tokens,
                                Some(duration_ms),
                                resp.cache_creation_tokens,
                                resp.cache_read_tokens,
                                target_app.as_deref(),
                                None,
                            );
                            // Emit realtime cost update event
                            {
                                let cost_usd = if let (Some(input), Some(output)) =
                                    (resp.input_tokens, resp.output_tokens)
                                {
                                    let cache_create = resp.cache_creation_tokens.unwrap_or(0);
                                    let cache_read = resp.cache_read_tokens.unwrap_or(0);
                                    crate::ai_pricing::calculate_cost_usd_with_cache(
                                        input,
                                        output,
                                        cache_create,
                                        cache_read,
                                        step_model.as_deref().unwrap_or("claude-sonnet-4-20250514"),
                                    )
                                } else {
                                    0.0
                                };

                                let cumulative = self
                                    .cost_trackers
                                    .lock()
                                    .unwrap()
                                    .as_ref()
                                    .map(|t| t.budget.snapshot().total_cost_usd)
                                    .unwrap_or(0.0);

                                self.broadcaster.cost_update(
                                    &config.execution_id,
                                    "agentic",
                                    Some(iteration),
                                    resp.input_tokens.unwrap_or(0),
                                    resp.output_tokens.unwrap_or(0),
                                    resp.cache_creation_tokens.unwrap_or(0),
                                    resp.cache_read_tokens.unwrap_or(0),
                                    cost_usd,
                                    cumulative,
                                );

                                // Record cost in budget tracker and check safety limits
                                let trackers = self.cost_trackers.lock().unwrap().clone();
                                if let Some(ref trackers) = trackers {
                                    let total_tokens = resp.input_tokens.unwrap_or(0)
                                        + resp.output_tokens.unwrap_or(0);
                                    let budget_result =
                                        trackers.budget.record("agentic", total_tokens, cost_usd);

                                    match budget_result {
                                        crate::cost_management::budget::BudgetResult::Warning { remaining_fraction, message } => {
                                            warn!("Budget warning ({}% remaining): {}", (remaining_fraction * 100.0) as u32, message);
                                            self.broadcaster.budget_warning(
                                                &config.execution_id,
                                                remaining_fraction,
                                                trackers.budget.snapshot().total_cost_usd,
                                                trackers.budget.snapshot().max_cost_usd,
                                                &message,
                                            );
                                        }
                                        crate::cost_management::budget::BudgetResult::Exceeded { ref phase, overage_usd } => {
                                            let reason = format!("Budget exceeded in phase {}: ${:.4} over limit", phase, overage_usd);
                                            tracing::error!("{}", reason);
                                            return (AgenticOutcome::BudgetExceeded { reason }, Vec::new());
                                        }
                                        crate::cost_management::budget::BudgetResult::Ok { .. } => {}
                                    }

                                    // Check circuit breaker
                                    let cb_result =
                                        trackers.circuit_breaker.check_single_call(cost_usd);
                                    if let crate::cost_management::circuit_breaker::CircuitBreakerResult::Tripped(reason) = &cb_result {
                                        tracing::error!("Cost circuit breaker tripped: {}", reason);
                                        return (AgenticOutcome::BudgetExceeded { reason: reason.clone() }, Vec::new());
                                    }

                                    // Check cache health
                                    let cache_create = resp.cache_creation_tokens.unwrap_or(0);
                                    let cache_read = resp.cache_read_tokens.unwrap_or(0);
                                    let cache_cb = trackers
                                        .circuit_breaker
                                        .record_cache_metrics(cache_create, cache_read);
                                    if let crate::cost_management::circuit_breaker::CircuitBreakerResult::Tripped(reason) = &cache_cb {
                                        tracing::error!("Cache circuit breaker tripped: {}", reason);
                                        return (AgenticOutcome::BudgetExceeded { reason: reason.clone() }, Vec::new());
                                    }

                                    // Anomaly detection
                                    if let Ok(mut detector) = trackers.anomaly_detector.lock() {
                                        if let Some(anomaly) = detector.check(cost_usd) {
                                            warn!(
                                                "Cost anomaly detected: ${:.4} (z-score: {:.2})",
                                                anomaly.cost_usd, anomaly.z_score
                                            );
                                            self.broadcaster.cost_anomaly(
                                                &config.execution_id,
                                                anomaly.cost_usd,
                                                anomaly.mean,
                                                anomaly.std_dev,
                                                anomaly.z_score,
                                            );
                                        }
                                        detector.update(cost_usd);
                                    }
                                }
                            }
                        }
                        let resp_llm_metrics = build_llm_metrics(
                            step_model.as_deref(),
                            step_provider.as_deref(),
                            resp.input_tokens,
                            resp.output_tokens,
                        );
                        let output = resp.output;
                        info!(
                            "AGENTIC-PHASE: Response-mode step '{}' completed ({} bytes, {}ms)",
                            step_name,
                            output.len(),
                            duration_ms
                        );
                        // Save completion checkpoint
                        let mut resp_checkpoint = StepCheckpoint::new(
                            &config.execution_id,
                            "unified",
                            "agentic",
                            Some(iteration),
                            0,
                            "prompt",
                        )
                        .with_step_name(step_name)
                        .with_stage_index(config.stage_index);
                        resp_checkpoint.mark_success(Some(output.clone()), duration_ms as i64);
                        if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                            warn!("Failed to save agentic response-mode step completion checkpoint: {}", e);
                        }
                        // Emit completion event for Active Dashboard
                        let resp_duration = resp_mode_start.elapsed().as_millis() as i64;
                        let complete_event = CreateTaskRunEventInput {
                            task_run_id: parent_id.clone(),
                            event_type: "step_execution".to_string(),
                            event_subtype: Some("complete".to_string()),
                            message: format!(
                                "Agentic response-mode completed (iteration {}, {}ms)",
                                iteration, resp_duration
                            ),
                            data: Some(
                                serde_json::to_string(&serde_json::json!({
                                    "step_index": 0,
                                    "step_type": "prompt",
                                    "step_name": "Agentic Response Prompt",
                                    "phase": "agentic",
                                    "iteration": iteration,
                                    "success": true,
                                    "llm_metrics": resp_llm_metrics,
                                }))
                                .unwrap_or_default(),
                            ),
                            workflow_name: None,
                            state_name: None,
                            action_id: Some(resp_action_id.clone()),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            duration_ms: Some(resp_duration),
                        };
                        // PG-primary: fire-and-forget async write to PostgreSQL
                        {
                            let pg = self.app_state.pg_db.clone();
                            let event_clone = complete_event.clone();
                            tokio::spawn(async move {
                                if let Err(e) = pg.create_task_run_event(&event_clone).await {
                                    tracing::warn!("PG event write failed: {}", e);
                                }
                            });
                        }
                        if let Err(e) = self
                            .app_state
                            .pg_db
                            .create_task_run_event(&complete_event)
                            .await
                        {
                            warn!(
                                "Failed to emit agentic response-mode completion event: {}",
                                e
                            );
                        }
                        // Response-mode steps produce raw text output without structured
                        // agentic markers, so there is no AgenticPhaseOutput to parse.
                        // The loop controller handles `parsed: None` gracefully by falling
                        // back to raw marker checks for unfixable errors, etc.
                        return (
                            AgenticOutcome::Success {
                                output,
                                parsed: None,
                                input_tokens: resp.input_tokens,
                                output_tokens: resp.output_tokens,
                            },
                            Vec::new(),
                        );
                    }
                    Err(e) => {
                        let duration_ms = start.elapsed().as_millis() as u64;
                        warn!(
                            "AGENTIC-PHASE: Response-mode step '{}' failed ({}ms): {}",
                            step_name, duration_ms, e
                        );
                        // Save failure checkpoint
                        let mut resp_checkpoint = StepCheckpoint::new(
                            &config.execution_id,
                            "unified",
                            "agentic",
                            Some(iteration),
                            0,
                            "prompt",
                        )
                        .with_step_name(step_name)
                        .with_stage_index(config.stage_index);
                        resp_checkpoint.mark_failed(&e, duration_ms as i64);
                        if let Err(e2) = checkpoint_mgr.save_step(&resp_checkpoint) {
                            warn!(
                                "Failed to save agentic response-mode step failure checkpoint: {}",
                                e2
                            );
                        }
                        // Emit error event for Active Dashboard
                        let resp_duration = resp_mode_start.elapsed().as_millis() as i64;
                        let error_event = CreateTaskRunEventInput {
                            task_run_id: parent_id.clone(),
                            event_type: "step_execution".to_string(),
                            event_subtype: Some("error".to_string()),
                            message: format!(
                                "Agentic response-mode failed (iteration {}): {}",
                                iteration, e
                            ),
                            data: Some(
                                serde_json::to_string(&serde_json::json!({
                                    "step_index": 0,
                                    "step_type": "prompt",
                                    "step_name": "Agentic Response Prompt",
                                    "phase": "agentic",
                                    "iteration": iteration,
                                    "success": false,
                                }))
                                .unwrap_or_default(),
                            ),
                            workflow_name: None,
                            state_name: None,
                            action_id: Some(resp_action_id.clone()),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            duration_ms: Some(resp_duration),
                        };
                        // PG-primary: fire-and-forget async write to PostgreSQL
                        {
                            let pg = self.app_state.pg_db.clone();
                            let event_clone = error_event.clone();
                            tokio::spawn(async move {
                                if let Err(e) = pg.create_task_run_event(&event_clone).await {
                                    tracing::warn!("PG event write failed: {}", e);
                                }
                            });
                        }
                        if let Err(e2) = self
                            .app_state
                            .pg_db
                            .create_task_run_event(&error_event)
                            .await
                        {
                            warn!("Failed to emit agentic response-mode error event: {}", e2);
                        }
                        return (AgenticOutcome::Error { error: e }, Vec::new());
                    }
                }
            }

            // If we get here, no response-mode steps were found (shouldn't happen)
            return (AgenticOutcome::Skipped, Vec::new());
        }

        // Create checkpoint manager for step-level checkpointing
        let checkpoint_mgr = CheckpointManager::new("unified");

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
        .with_step_name("AI Fixing Issues")
        .with_stage_index(config.stage_index);
        checkpoint.mark_started();
        if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
            warn!("Failed to save agentic step checkpoint: {}", e);
        }

        // Emit step event so the Active Dashboard timeline shows the agentic phase
        let parent_id = get_parent_task_id(&config.execution_id);
        let action_id = format!("agentic-ai_session-{}-0", parent_id);
        let start_event = CreateTaskRunEventInput {
            task_run_id: parent_id.clone(),
            event_type: "step_execution".to_string(),
            event_subtype: Some("start".to_string()),
            message: format!("Starting agentic AI session (iteration {})", iteration),
            data: Some(
                serde_json::to_string(&serde_json::json!({
                    "step_index": 0,
                    "step_type": "ai_session",
                    "step_name": "AI Fixing Issues",
                    "phase": "agentic",
                    "iteration": iteration,
                }))
                .unwrap_or_default(),
            ),
            workflow_name: None,
            state_name: None,
            action_id: Some(action_id.clone()),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms: None,
        };
        // PG-primary: fire-and-forget async write to PostgreSQL
        {
            let pg = self.app_state.pg_db.clone();
            let event_clone = start_event.clone();
            tokio::spawn(async move {
                if let Err(e) = pg.create_task_run_event(&event_clone).await {
                    tracing::warn!("PG event write failed: {}", e);
                }
            });
        }
        if let Err(e) = self
            .app_state
            .pg_db
            .create_task_run_event(&start_event)
            .await
        {
            warn!("Failed to emit agentic start event: {}", e);
        }

        let agentic_start = std::time::Instant::now();

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
            let base = if config.reflection_mode {
                format!(
                    "{}\n\n---\n\n{}\n\nThe following verification checks FAILED:\n\n{}\n\nAfter your investigation, implement fixes that address root causes.",
                    config.base_prompt, REFLECTION_MODE_PREAMBLE, failure_context
                )
            } else {
                format!(
                    "{}\n\n---\n\nThe following verification checks FAILED. Please fix these issues:\n\n{}\n\nFix the issues above and ensure all checks pass.",
                    config.base_prompt, failure_context
                )
            };
            // Append progress context if available
            if let Some(ref progress) = progress_context {
                format!("{}\n\n{}", base, progress)
            } else {
                base
            }
        };

        // Check file registry for conflicts and warn about files under active development
        let enhanced_prompt = {
            let conflicts = self
                .app_state
                .file_registry_manager
                .check_conflicts(&config.execution_id)
                .await;
            if conflicts.is_empty() {
                enhanced_prompt
            } else {
                let mut warning = String::from("\n\n## Active File Conflicts Warning\n\n");
                warning.push_str(
                    "The following files are currently being worked on by other active sessions. \
                     Avoid modifying these files to prevent merge conflicts:\n\n",
                );
                for conflict in &conflicts {
                    let holders: Vec<String> = conflict
                        .other_holders
                        .iter()
                        .map(|h| format!("'{}'", h.holder_name))
                        .collect();
                    warning.push_str(&format!(
                        "- **{}** (active in: {})\n",
                        conflict.file_path,
                        holders.join(", ")
                    ));
                }
                info!(
                    "AGENTIC-PHASE: Injected {} file conflict warning(s) for {}",
                    conflicts.len(),
                    config.execution_id
                );
                format!("{}{}", enhanced_prompt, warning)
            }
        };

        // Append unified memory context (cross-run learnings, knowledge graph, PG memories).
        // Uses MemorySystem::build_context_unified which queries PG + SQLite + graph and
        // falls back gracefully when the unified query returns nothing.
        let enhanced_prompt = {
            let mem = crate::orchestrator::memory::MemorySystem::new();
            let mem_ctx = mem
                .build_context_unified(20, &config.base_prompt, &self.app_state.pg_db)
                .await;
            if mem_ctx.trim().is_empty() {
                enhanced_prompt
            } else {
                info!(
                    "AGENTIC-PHASE: Injecting unified memory context ({} chars)",
                    mem_ctx.len()
                );
                format!("{}\n\n{}", enhanced_prompt, mem_ctx)
            }
        };

        // Append pre-computed working representation (entity profiles, patterns, findings,
        // fixes, skills). Uses an in-memory cache so rebuilds only happen on first access
        // or after consolidation invalidates entries.
        let enhanced_prompt = {
            let wr_cache = &self.app_state.working_representation_cache;
            match wr_cache
                .get_or_build(
                    &config.execution_id,
                    Some(&config.workflow_id),
                    Some(&config.workflow_name),
                    &self.app_state.pg_db,
                )
                .await
            {
                Ok(wr) => {
                    let wr_ctx =
                        crate::memory::working_representation::format_working_representation(&wr);
                    if wr_ctx.is_empty() {
                        enhanced_prompt
                    } else {
                        info!(
                            "AGENTIC-PHASE: Injecting working representation ({} items, {} chars)",
                            wr.total_items,
                            wr_ctx.len()
                        );
                        format!("{}\n\n{}", enhanced_prompt, wr_ctx)
                    }
                }
                Err(e) => {
                    warn!(
                        "AGENTIC-PHASE: Failed to build working representation: {}",
                        e
                    );
                    enhanced_prompt
                }
            }
        };

        // Append execution timing context if available (from iteration 2+ or cross-stage)
        let enhanced_prompt = if iteration > 1 || config.stage_index.is_some_and(|idx| idx > 0) {
            match build_execution_timing_context(&config.execution_id) {
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

        // Pre-build managed process status summary for iteration context
        let process_status_summary = {
            // ProcessStateExt provides `.as_str()` since ProcessState moved to
            // qontinui_types::process_management (no more Display impl).
            use crate::process_capture::types::ProcessStateExt;
            let mgr_lock = self.app_state.process_capture_manager.lock().await;
            if let Some(ref mgr) = *mgr_lock {
                let statuses = mgr.get_all_status().await;
                if !statuses.is_empty() {
                    let mut lines = vec!["## Managed Process Status".to_string()];
                    lines.push("The following dev processes are being managed:".to_string());
                    for s in &statuses {
                        let health = s
                            .port_healthy
                            .map(|h| {
                                if h {
                                    "port healthy"
                                } else {
                                    "port not responding"
                                }
                            })
                            .unwrap_or("no health check");
                        let uptime = s
                            .uptime_secs
                            .map(|u| format!("uptime {}s", u))
                            .unwrap_or_default();
                        lines.push(format!(
                            "- {} [{}]: {} ({}, {} errors)",
                            s.name,
                            s.category,
                            s.state.as_str(),
                            if uptime.is_empty() {
                                health.to_string()
                            } else {
                                format!("{}, {}", uptime, health)
                            },
                            s.error_count
                        ));
                    }
                    Some(lines.join("\n"))
                } else {
                    None
                }
            } else {
                None
            }
        };

        // Pre-build error monitor summary for iteration context.
        // Only inject when the workflow is specifically targeting errors (error-fix workflows),
        // otherwise it distracts the AI from its actual task.
        let error_monitor_summary = if !config.targeted_error_ids.is_empty() {
            match self.app_state.pg_db.get_unresolved_errors(None, 20).await {
                Ok(errors) if !errors.is_empty() => {
                    let mut lines = vec!["## Recent Errors (Error Monitor)".to_string()];
                    lines.push(format!(
                        "The error monitor has detected {} unresolved error(s):",
                        errors.len()
                    ));
                    for e in errors.iter().take(15) {
                        let occurrence_count = e
                            .get("occurrence_count")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(1);
                        let count_str = if occurrence_count > 1 {
                            format!(" ({} occurrences)", occurrence_count)
                        } else {
                            String::new()
                        };
                        let log_source_name = e
                            .get("log_source_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let message = e.get("message").and_then(|v| v.as_str()).unwrap_or("");
                        lines.push(format!(
                            "- [{}] {}{}",
                            log_source_name,
                            message.chars().take(200).collect::<String>(),
                            count_str
                        ));
                    }
                    if errors.len() > 15 {
                        lines.push(format!("... and {} more", errors.len() - 15));
                    }
                    Some(lines.join("\n"))
                }
                _ => None,
            }
        } else {
            None
        };

        // For iteration 2+, add context from previous iterations (findings + verification results)
        // Also for stage > 0 at iteration 1, inject cross-stage context so the AI
        // has visibility into prior stages' findings and knowledge.
        let needs_iteration_context =
            iteration > 1 || config.stage_index.is_some_and(|idx| idx > 0);
        let enhanced_prompt = if needs_iteration_context {
            match build_compressed_iteration_history(
                &config.execution_id,
                iteration,
                process_status_summary.as_deref(),
                error_monitor_summary.as_deref(),
                Some(&config.workflow_name),
                config.max_context_tokens,
                config.cross_workflow_learning,
                config.project_path.as_deref(),
                &self.app_state.pg_db,
            )
            .await
            {
                Some(ctx) => {
                    let label = if iteration == 1 {
                        format!(
                            "cross-stage context for stage {}",
                            config.stage_index.unwrap_or(0)
                        )
                    } else {
                        format!("iteration context for iteration {}", iteration)
                    };
                    info!("AGENTIC-PHASE: Appending {} ({} chars)", label, ctx.len(),);
                    format!("{}\n\n{}", enhanced_prompt, ctx)
                }
                None => enhanced_prompt,
            }
        } else {
            enhanced_prompt
        };

        // Inject canary prompt overrides for non-pipeline architectures.
        // The multi-agent pipeline injects its own prompt overrides via active_prompt_variants
        // in the pipeline loop, so we skip injection here for pipeline runs to avoid duplication.
        let is_pipeline = matches!(
            config.workflow_architecture,
            Some(crate::agentic_verification::WorkflowArchitecture::MultiAgentPipeline)
        );
        let enhanced_prompt = if !is_pipeline {
            if let Some((_, ref rec_id)) = config.active_canary {
                match crate::meta_optimizer::canary::get_canary_prompt_overrides(
                    &self.app_state.pg_db,
                    rec_id,
                ) {
                    Ok(overrides) => {
                        if let Some(override_prompt) = overrides.get("implementer") {
                            info!(
                                "AGENTIC-PHASE: Canary injecting implementer prompt override ({} chars)",
                                override_prompt.len()
                            );
                            format!(
                                "{}\n\n## Optimized Agent Instructions\n\n{}",
                                enhanced_prompt, override_prompt
                            )
                        } else {
                            enhanced_prompt
                        }
                    }
                    Err(_) => enhanced_prompt,
                }
            } else {
                enhanced_prompt
            }
        } else {
            enhanced_prompt
        };

        // Append safety and focus instructions
        let enhanced_prompt = format!(
            "{}\n\n## Important Constraints\n\n\
            - **STAY FOCUSED**: ONLY work on fixing the failed verification checks listed above. Do NOT investigate, diagnose, or fix unrelated errors, warnings, or issues you find in log files or elsewhere.\n\
            - Do NOT modify the runner's database directly. Configuration changes must go through the runner UI or API.\n\
            - Do NOT modify workflow JSON files in the parent directory. Fix the application code instead.\n\
            - Focus exclusively on the source code that the verification checks are testing. When all checks pass, your work is done.",
            enhanced_prompt
        );

        // === Enrichment #1: Pre-read files referenced in failure context ===
        // Extract file paths from verification failure output and pre-read their contents
        // so the AI has them immediately without needing tool calls.
        let enhanced_prompt = {
            let preread = extract_and_preread_failure_files(
                failure_context,
                config.project_path.as_deref(),
                15,
                300,
                60_000,
            );
            if !preread.is_empty() {
                format!(
                    "{}\n\n## Pre-loaded Source Files\n\nThese files were referenced in the verification failures above. Read them here instead of using tool calls.\n\n{}",
                    enhanced_prompt, preread
                )
            } else {
                enhanced_prompt
            }
        };

        // === Enrichment #2: Pre-read previously edited files ===
        // On iteration 2+, read the current state of files edited in prior iterations
        // so the AI can see the cumulative changes without tool calls.
        let enhanced_prompt = if iteration > 1 {
            let preread = preread_previously_edited_files(
                &config.execution_id,
                iteration,
                config.project_path.as_deref(),
                10,
                300,
                40_000,
            );
            if !preread.is_empty() {
                format!(
                    "{}\n\n## Previously Edited Files (Current State)\n\nThese files were modified in prior iterations. Their current contents are shown below.\n\n{}",
                    enhanced_prompt, preread
                )
            } else {
                enhanced_prompt
            }
        } else {
            enhanced_prompt
        };

        // Record activity heartbeat before AI session spawn
        {
            let persist_id = get_parent_task_id(&config.execution_id);
            let now = chrono::Utc::now().to_rfc3339();
            let ctx_json = serde_json::json!({
                "last_activity": format!("agentic_session_spawn_iter_{}", iteration),
                "last_activity_at": now,
            });
            if let Ok(json) = serde_json::to_string(&ctx_json) {
                // Runtime context persistence removed — all persistence now via PgDb.
                let _ = &persist_id; // suppress unused warning
                let _ = &json;
            }
        }

        // Use the unified AI session executor with timing
        // Step-level model override takes precedence over phase-level
        let agentic_model = agentic_steps
            .first()
            .and_then(|s| s.model.clone())
            .or_else(|| config.resolve_model_for_phase("agentic"));
        let mut ai_config =
            AiSessionConfig::agentic(&config.execution_id, &config.workflow_name, iteration)
                .with_checkpoint_id(&checkpoint.id)
                .with_model_override(agentic_model);

        // CLI session context for restart survival.
        // Check if there's an interrupted session we can resume via `--resume`.
        // If so, reuse its CLI session ID; otherwise generate a fresh one.
        let parent_task_id = get_parent_task_id(&config.execution_id);
        let (cli_session_id, is_resume) = match self
            .app_state
            .pg_db
            .get_workflow_ai_session(&parent_task_id, iteration as i32, "agentic")
            .await
        {
            Ok(Some((prev_cli_id, prev_status))) if prev_status == "interrupted" => {
                info!(
                    "AGENTIC-PHASE: Found interrupted CLI session {} for iteration {} — will resume",
                    prev_cli_id, iteration
                );
                (prev_cli_id, true)
            }
            Err(e) => {
                warn!(
                    "AGENTIC-PHASE: Failed to check for interrupted AI session: {} — starting fresh",
                    e
                );
                (uuid::Uuid::new_v4().to_string(), false)
            }
            _ => (uuid::Uuid::new_v4().to_string(), false),
        };

        ai_config.cli_session_ctx = Some(crate::claude_session::runner::CliSessionContext {
            cli_session_id: cli_session_id.clone(),
            is_resume,
        });

        // Record the AI session in the database for restart recovery
        if let Err(e) = self
            .app_state
            .pg_db
            .create_workflow_ai_session(
                &parent_task_id,
                iteration as i32,
                "agentic",
                config.stage_index.map(|i| i as i32),
                &cli_session_id,
            )
            .await
        {
            warn!("PG create_workflow_ai_session failed: {}", e);
        }
        if let Err(e) = self
            .app_state
            .pg_db
            .create_workflow_ai_session(
                &parent_task_id,
                iteration as i32,
                "agentic",
                config.stage_index.map(|i| i as i32),
                &cli_session_id,
            )
            .await
        {
            warn!("Failed to create workflow AI session record: {}", e);
        }

        // Attach DB flush context for periodic output persistence
        ai_config.db_flush_ctx = Some(crate::claude_session::runner::DbFlushContext {
            task_run_id: parent_task_id.clone(),
            iteration: iteration as i32,
        });

        // Attach reflection fix context if this is a reflection workflow
        if let Some(ref ctx) = self.reflection_fix_ctx {
            ai_config = ai_config.with_reflection_fix_ctx(ctx.clone());
        }

        // Attach step injection context if set
        if let Some(ref ctx) = self.step_injection_ctx {
            ai_config = ai_config.with_step_injection_ctx(ctx.clone());
        }

        // When resuming an interrupted CLI session, send a brief continuation message
        // instead of the full prompt. The CLI already has the full conversation history.
        let resume_prompt = if is_resume {
            let resume_msg = format!(
                "The runner was restarted while you were working on iteration {}. \
                 Your previous Claude Code session has been resumed — you have full context \
                 of everything you did before the interruption. \
                 Continue where you left off. Complete the remaining work for this iteration.",
                iteration
            );
            info!(
                "AGENTIC-PHASE: Using resume prompt ({} chars) instead of full prompt ({} chars)",
                resume_msg.len(),
                enhanced_prompt.len()
            );
            Some(resume_msg)
        } else {
            None
        };
        let final_prompt = resume_prompt.as_deref().unwrap_or(&enhanced_prompt);

        let (mut result, duration) = timeout_helper::timed_result_async(self.ai_executor.execute(
            &ai_config,
            final_prompt,
            logger,
        ))
        .await;
        let mut duration_ms = duration as i64;

        // Fallback: if --resume failed, retry with a fresh session.
        // This handles cases where the CLI session was not persisted, expired, or corrupted.
        // We check for failure regardless of output content, since the CLI may emit
        // error text (e.g., "Error: session not found") as non-empty output.
        if is_resume && !result.success {
            warn!(
                "AGENTIC-PHASE: CLI session resume failed (error: {}, output_len: {}). Falling back to fresh session.",
                result.error,
                result.output.len()
            );
            // Create a fresh CLI session
            let fresh_cli_id = uuid::Uuid::new_v4().to_string();
            ai_config.cli_session_ctx = Some(crate::claude_session::runner::CliSessionContext {
                cli_session_id: fresh_cli_id.clone(),
                is_resume: false,
            });
            // Update the DB record with the new session ID
            if let Err(e) = self
                .app_state
                .pg_db
                .create_workflow_ai_session(
                    &parent_task_id,
                    iteration as i32,
                    "agentic",
                    config.stage_index.map(|i| i as i32),
                    &fresh_cli_id,
                )
                .await
            {
                warn!("PG create fallback workflow AI session failed: {}", e);
            }
            if let Err(e) = self
                .app_state
                .pg_db
                .create_workflow_ai_session(
                    &parent_task_id,
                    iteration as i32,
                    "agentic",
                    config.stage_index.map(|i| i as i32),
                    &fresh_cli_id,
                )
                .await
            {
                warn!(
                    "Failed to create fallback workflow AI session record: {}",
                    e
                );
            }
            // Retry with the full enhanced prompt
            let (retry_result, retry_duration) = timeout_helper::timed_result_async(
                self.ai_executor
                    .execute(&ai_config, &enhanced_prompt, logger),
            )
            .await;
            result = retry_result;
            duration_ms = retry_duration as i64;
        }

        // Checkpoint completion
        let mut completion_checkpoint = StepCheckpoint::new(
            &config.execution_id,
            "unified",
            "agentic",
            Some(iteration),
            0,
            "ai_session",
        )
        .with_step_name("AI Fixing Issues")
        .with_stage_index(config.stage_index);

        let injected_steps = result.injected_steps;

        // Parse structured output from the AI response
        let parsed_output = if !result.output.is_empty() {
            Some(output_parser::parse_agentic_output(&result.output))
        } else {
            None
        };

        let outcome = if result.success {
            completion_checkpoint.mark_success(Some(result.output.clone()), duration_ms);
            AgenticOutcome::Success {
                output: result.output,
                parsed: parsed_output,
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
            }
        } else if result.output.is_empty() {
            let error_msg = if result.error.is_empty() {
                "AI session failed (no output, no error details)".to_string()
            } else {
                format!("AI session failed: {}", result.error)
            };
            completion_checkpoint.mark_failed(&error_msg, duration_ms);
            AgenticOutcome::Error { error: error_msg }
        } else {
            let error_msg = if result.error.is_empty() {
                "AI reported failure".to_string()
            } else {
                format!("AI reported failure: {}", result.error)
            };
            completion_checkpoint.mark_failed(&error_msg, duration_ms);
            AgenticOutcome::Failed {
                output: result.output,
                error: error_msg,
                parsed: parsed_output,
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
            }
        };

        if let Err(e) = checkpoint_mgr.save_step(&completion_checkpoint) {
            warn!("Failed to save agentic step completion checkpoint: {}", e);
        }

        // Record token usage for the main AI session and build LLM metrics
        let (session_input_tokens, session_output_tokens) = outcome.token_usage();
        let session_model = config.resolve_model_for_phase("agentic");
        let session_provider = config.resolve_provider_for_phase("agentic");
        {
            let target_app = get_active_sdk_app_name(&self.app_state);
            record_phase_token_usage_with_target(
                &self.app_state.pg_db,
                &config.execution_id,
                "agentic",
                config.stage_index,
                Some(iteration),
                session_model.as_deref(),
                session_provider.as_deref(),
                session_input_tokens,
                session_output_tokens,
                Some(duration_ms.max(0) as u64),
                target_app.as_deref(),
                None,
            );

            // Emit realtime cost update for the main AI session
            {
                let cost_usd = if let (Some(input), Some(output)) =
                    (session_input_tokens, session_output_tokens)
                {
                    crate::ai_pricing::calculate_cost_usd_with_cache(
                        input,
                        output,
                        0,
                        0,
                        session_model
                            .as_deref()
                            .unwrap_or("claude-sonnet-4-20250514"),
                    )
                } else {
                    0.0
                };

                let cumulative = self
                    .cost_trackers
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|t| t.budget.snapshot().total_cost_usd)
                    .unwrap_or(0.0);

                self.broadcaster.cost_update(
                    &config.execution_id,
                    "agentic",
                    Some(iteration),
                    session_input_tokens.unwrap_or(0),
                    session_output_tokens.unwrap_or(0),
                    0,
                    0,
                    cost_usd,
                    cumulative,
                );

                // Record cost in budget tracker and check safety limits
                let trackers = self.cost_trackers.lock().unwrap().clone();
                if let Some(ref trackers) = trackers {
                    let total_tokens =
                        session_input_tokens.unwrap_or(0) + session_output_tokens.unwrap_or(0);
                    let budget_result = trackers.budget.record("agentic", total_tokens, cost_usd);

                    match budget_result {
                        crate::cost_management::budget::BudgetResult::Warning {
                            remaining_fraction,
                            message,
                        } => {
                            warn!(
                                "Budget warning ({}% remaining): {}",
                                (remaining_fraction * 100.0) as u32,
                                message
                            );
                            self.broadcaster.budget_warning(
                                &config.execution_id,
                                remaining_fraction,
                                trackers.budget.snapshot().total_cost_usd,
                                trackers.budget.snapshot().max_cost_usd,
                                &message,
                            );
                        }
                        crate::cost_management::budget::BudgetResult::Exceeded {
                            ref phase,
                            overage_usd,
                        } => {
                            let reason = format!(
                                "Budget exceeded in phase {}: ${:.4} over limit",
                                phase, overage_usd
                            );
                            tracing::error!("{}", reason);
                            return (AgenticOutcome::BudgetExceeded { reason }, Vec::new());
                        }
                        crate::cost_management::budget::BudgetResult::Ok { .. } => {}
                    }

                    // Check circuit breaker
                    let cb_result = trackers.circuit_breaker.check_single_call(cost_usd);
                    if let crate::cost_management::circuit_breaker::CircuitBreakerResult::Tripped(
                        reason,
                    ) = &cb_result
                    {
                        tracing::error!("Cost circuit breaker tripped: {}", reason);
                        return (
                            AgenticOutcome::BudgetExceeded {
                                reason: reason.clone(),
                            },
                            Vec::new(),
                        );
                    }

                    // Anomaly detection
                    if let Ok(mut detector) = trackers.anomaly_detector.lock() {
                        if let Some(anomaly) = detector.check(cost_usd) {
                            warn!(
                                "Cost anomaly detected: ${:.4} (z-score: {:.2})",
                                anomaly.cost_usd, anomaly.z_score
                            );
                            self.broadcaster.cost_anomaly(
                                &config.execution_id,
                                anomaly.cost_usd,
                                anomaly.mean,
                                anomaly.std_dev,
                                anomaly.z_score,
                            );
                        }
                        detector.update(cost_usd);
                    }
                }
            }
        }
        let session_llm_metrics = build_llm_metrics(
            session_model.as_deref(),
            session_provider.as_deref(),
            session_input_tokens,
            session_output_tokens,
        );

        // Mark the workflow AI session as completed/failed and clean up partial output
        {
            let session_status = match &outcome {
                AgenticOutcome::Success { .. } => "completed",
                AgenticOutcome::Failed { .. } => "failed",
                AgenticOutcome::Error { .. } => "failed",
                AgenticOutcome::Skipped => "completed",
                AgenticOutcome::BudgetExceeded { .. } => "failed",
            };
            let output_len = outcome.output().map(|o| o.len() as i64).unwrap_or(0);
            if let Err(e) = self
                .app_state
                .pg_db
                .complete_workflow_ai_session(
                    &parent_task_id,
                    iteration as i32,
                    "agentic",
                    config.stage_index.map(|i| i as i32),
                    session_status,
                    output_len,
                )
                .await
            {
                warn!("PG complete_workflow_ai_session failed: {}", e);
            }
            if let Err(e) = self
                .app_state
                .pg_db
                .complete_workflow_ai_session(
                    &parent_task_id,
                    iteration as i32,
                    "agentic",
                    config.stage_index.map(|i| i as i32),
                    session_status,
                    output_len,
                )
                .await
            {
                warn!("Failed to complete workflow AI session: {}", e);
            }
            // Partial AI output deletion removed — all persistence now via PgDb.
        }

        // Emit completion event so the Active Dashboard timeline shows agentic phase result
        let agentic_duration_ms = agentic_start.elapsed().as_millis() as i64;
        let (event_subtype, event_message) = match &outcome {
            AgenticOutcome::Success { .. } => (
                "complete",
                format!(
                    "Agentic AI session completed successfully (iteration {}, {}ms)",
                    iteration, agentic_duration_ms
                ),
            ),
            AgenticOutcome::Failed { error, .. } => (
                "error",
                format!(
                    "Agentic AI session failed (iteration {}): {}",
                    iteration, error
                ),
            ),
            AgenticOutcome::Error { error } => (
                "error",
                format!(
                    "Agentic AI session error (iteration {}): {}",
                    iteration, error
                ),
            ),
            AgenticOutcome::Skipped => ("complete", "Agentic phase skipped".to_string()),
            AgenticOutcome::BudgetExceeded { reason } => (
                "error",
                format!(
                    "Agentic AI session budget exceeded (iteration {}): {}",
                    iteration, reason
                ),
            ),
        };
        let completion_event = CreateTaskRunEventInput {
            task_run_id: parent_id.clone(),
            event_type: "step_execution".to_string(),
            event_subtype: Some(event_subtype.to_string()),
            message: event_message,
            data: Some(
                serde_json::to_string(&serde_json::json!({
                    "step_index": 0,
                    "step_type": "ai_session",
                    "step_name": "AI Fixing Issues",
                    "phase": "agentic",
                    "iteration": iteration,
                    "success": matches!(&outcome, AgenticOutcome::Success { .. }),
                    "llm_metrics": session_llm_metrics,
                }))
                .unwrap_or_default(),
            ),
            workflow_name: None,
            state_name: None,
            action_id: Some(action_id),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms: Some(agentic_duration_ms),
        };
        // PG-primary: fire-and-forget async write to PostgreSQL
        {
            let pg = self.app_state.pg_db.clone();
            let event_clone = completion_event.clone();
            tokio::spawn(async move {
                if let Err(e) = pg.create_task_run_event(&event_clone).await {
                    tracing::warn!("PG event write failed: {}", e);
                }
            });
        }
        if let Err(e) = self
            .app_state
            .pg_db
            .create_task_run_event(&completion_event)
            .await
        {
            warn!("Failed to emit agentic completion event: {}", e);
        }

        if !injected_steps.is_empty() {
            info!(
                "AGENTIC-PHASE: Collected {} injected verification step(s) from AI output",
                injected_steps.len()
            );
        }

        (outcome, injected_steps)
    }

    /// Run a focused AI session with a custom prompt.
    ///
    /// Unlike `run_agentic()`, this doesn't build the prompt from config.base_prompt.
    /// It runs the provided prompt directly. Used by the multi-agent fixer to spawn
    /// specialized fix agents with narrow, targeted prompts.
    ///
    /// Returns (success, output, duration_ms).
    pub async fn run_focused_session(
        &self,
        execution_id: &str,
        workflow_name: &str,
        iteration: u32,
        agent_label: &str,
        prompt: &str,
        model_override: Option<String>,
        logger: &StepEventLogger,
    ) -> (bool, String, u64) {
        let start = std::time::Instant::now();
        let parent_task_id = get_parent_task_id(execution_id);

        info!(
            "MULTI-AGENT: Running focused session '{}' (iteration {})",
            agent_label, iteration
        );

        let mut ai_config = crate::unified_ai_session::AiSessionConfig::agentic(
            execution_id,
            workflow_name,
            iteration,
        )
        .with_model_override(model_override);

        // Create a fresh CLI session for each focused agent
        let cli_session_id = uuid::Uuid::new_v4().to_string();
        ai_config.cli_session_ctx = Some(crate::claude_session::runner::CliSessionContext {
            cli_session_id,
            is_resume: false,
        });
        ai_config.db_flush_ctx = Some(crate::claude_session::runner::DbFlushContext {
            task_run_id: parent_task_id.clone(),
            iteration: iteration as i32,
        });

        let result = self.ai_executor.execute(&ai_config, prompt, logger).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        info!(
            "MULTI-AGENT: Focused session '{}' completed in {}ms (success={})",
            agent_label, duration_ms, result.success
        );

        (result.success, result.output, duration_ms)
    }

    /// Run a triage prompt in response mode (fast, no session state).
    ///
    /// Used by the multi-agent fixer to classify verification failures
    /// before spawning specialized fix agents.
    pub async fn run_triage_prompt(
        &self,
        prompt: &str,
        model_override: Option<String>,
    ) -> Result<String, String> {
        let step = ExecutionStepConfig {
            step_type: "prompt".to_string(),
            name: Some("Multi-agent triage".to_string()),
            prompt_content: Some(prompt.to_string()),
            prompt_mode: Some("response".to_string()),
            model: model_override.clone(),
            ..Default::default()
        };

        let result = execute_prompt_response_mode(
            &step,
            &self.app_state.pg_db,
            None,
            None,
            model_override,
            None,
            None,
            None,
            None,
            None,
        )
        .await?;

        Ok(result.output)
    }

    /// Get progress marker context from previous checkpoints.
    ///
    /// This queries for the most recent checkpoint from a previous agentic session
    /// and retrieves its latest progress marker. This information helps the AI
    /// understand where to resume long operations.
    ///
    /// Returns a formatted string like:
    /// "Last progress: file_progress 50/100. Continue from where you left off."
    fn get_progress_marker_context(&self, _execution_id: &str, _iteration: u32) -> Option<String> {
        // Progress marker context removed — all persistence now via PgDb.
        None
    }
}
