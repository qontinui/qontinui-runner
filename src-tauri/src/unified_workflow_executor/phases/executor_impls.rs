//! FromContext and Executor trait implementations for all four phase executors.
//!
//! These impls wire the executors into the generic execution framework.

use async_trait::async_trait;

use crate::step_registry::StepEventLogger;

use super::super::phase_configs::{
    AgenticConfig, CompletionConfig, CompletionResult, SetupConfig, SetupResult,
    VerificationConfig, VerificationResult,
};
use super::super::types::{AgenticOutcome, LoopConfig};
use super::agentic::AgenticExecutor;
use super::completion::CompletionExecutor;
use super::setup::SetupExecutor;
use super::verification::VerificationExecutor;

// Executor framework traits
use crate::executor::context::ExecutorContext;
use crate::executor::traits::{Executor, ExecutorError, FromContext};

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
        let (success, step_results) = self
            .run_setup(
                &config.automation_steps,
                &config.prompt_steps,
                &config.execution_id,
                &config.workflow_name,
                &StepEventLogger::noop(self.app_state.pg_db.clone()),
                None,
                config.model_override.clone(),
                config.provider_override.clone(),
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
                &StepEventLogger::noop(self.app_state.pg_db.clone()),
                None,
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
            base_prompt: config.base_prompt,
            workflow_name: config.workflow_name,
            workflow_id: config.workflow_id,
            execution_id: config.execution_id.clone(),
            targeted_error_ids: Vec::new(),
            starting_iteration: 0,
            run_agentic_first: false,
            artifact_dir: None,
            is_dev_mode: false,
            enable_sweep: false,
            max_sweep_iterations: 5,
            stages: Vec::new(),
            stop_on_failure: false,
            constraint_overrides: std::collections::HashMap::new(),
            reflection_mode: false,
            provider_override: None,
            model_override: None,
            model_overrides: std::collections::HashMap::new(),
            stage_index: None,
            max_sessions: None,
            auto_run_generated: false,
            approval_gate: false,
            max_context_tokens: 100_000,
            enforce_token_budget: false,
            cross_workflow_learning: true,
            verification_history: std::collections::HashMap::new(),
            routing_context: Default::default(),
            project_path: crate::mcp::shared::current_project_path(),
            acceptance_criteria: None,
            multi_agent_mode: false,
            strict_cwd: false,
            tool_tags: Vec::new(),
            use_worktree: false,
            worktree_path: None,
            worktree_branch: None,
            workflow_architecture: None,
            agentic_verification_config: None,
            multi_agent_pipeline_config: None,
            rollback_policy: crate::unified_workflow_executor::RollbackPolicy::None,
            escalation_policy: crate::unified_workflow_executor::blame::EscalationPolicy::default(),
            iteration_diffs: Vec::new(),
            active_canary: None,
            is_canary_run: false,
            phase_timeout_ms: None,
        };

        let (outcome, _injected_steps) = self
            .run_agentic(
                &loop_config,
                config.iteration,
                &config.failure_context,
                config.has_agentic_steps,
                &[], // No step configs available via trait interface
                &StepEventLogger::noop(self.app_state.pg_db.clone()),
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
                config.iterations_run,
                &StepEventLogger::noop(self.app_state.pg_db.clone()),
                None,
                config.model_override.clone(),
                config.provider_override.clone(),
                config.completion_prompts_first,
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
