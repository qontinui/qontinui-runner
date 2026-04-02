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
    execute_prompt_response_mode, extract_and_preread_failure_files,
    get_active_sdk_app_name, preread_previously_edited_files, record_phase_token_usage,
    record_phase_token_usage_with_cache, record_phase_token_usage_with_target,
    REFLECTION_MODE_PREAMBLE,
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
            app_handle,
            reflection_fix_ctx: None,
            step_injection_ctx: None,
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
        todo!("SQLite removed")
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
        todo!("SQLite removed")
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
