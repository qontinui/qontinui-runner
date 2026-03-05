//! Configuration types for phase executors.
//!
//! These types define the input configuration for each phase executor
//! when used through the Executor trait interface.

use serde::{Deserialize, Serialize};

use crate::step_executor::ExecutionStepConfig;

// =============================================================================
// Setup Phase Config
// =============================================================================

/// Configuration for the setup phase executor.
#[derive(Debug, Clone)]
pub struct SetupConfig {
    /// Automation steps to execute (shell commands, etc.)
    pub automation_steps: Vec<ExecutionStepConfig>,
    /// Prompt steps for AI tasks
    pub prompt_steps: Vec<ExecutionStepConfig>,
    /// Unique execution ID
    pub execution_id: String,
    /// Workflow name
    pub workflow_name: String,
    /// Model override for setup phase AI calls
    pub model_override: Option<String>,
    /// Provider override for setup phase AI calls
    pub provider_override: Option<String>,
}

impl SetupConfig {
    /// Create a new setup configuration.
    pub fn new(execution_id: impl Into<String>, workflow_name: impl Into<String>) -> Self {
        Self {
            automation_steps: Vec::new(),
            prompt_steps: Vec::new(),
            execution_id: execution_id.into(),
            workflow_name: workflow_name.into(),
            model_override: None,
            provider_override: None,
        }
    }

    /// Add automation steps.
    #[must_use]
    pub fn with_automation_steps(mut self, steps: Vec<ExecutionStepConfig>) -> Self {
        self.automation_steps = steps;
        self
    }

    /// Add prompt steps.
    #[must_use]
    pub fn with_prompt_steps(mut self, steps: Vec<ExecutionStepConfig>) -> Self {
        self.prompt_steps = steps;
        self
    }
}

/// Result of the setup phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupResult {
    /// Whether setup completed successfully
    pub success: bool,
    /// Step execution results
    pub step_results: Vec<crate::step_executor::StepExecutionResult>,
}

// =============================================================================
// Verification Phase Config
// =============================================================================

/// Configuration for the verification phase executor.
#[derive(Debug, Clone)]
pub struct VerificationConfig {
    /// Steps to execute for verification
    pub steps: Vec<ExecutionStepConfig>,
    /// Unique execution ID
    pub execution_id: String,
    /// Current iteration number
    pub iteration: u32,
    /// Workflow name
    pub workflow_name: String,
}

impl VerificationConfig {
    /// Create a new verification configuration.
    pub fn new(
        execution_id: impl Into<String>,
        workflow_name: impl Into<String>,
        iteration: u32,
    ) -> Self {
        Self {
            steps: Vec::new(),
            execution_id: execution_id.into(),
            iteration,
            workflow_name: workflow_name.into(),
        }
    }

    /// Add steps.
    #[must_use]
    pub fn with_steps(mut self, steps: Vec<ExecutionStepConfig>) -> Self {
        self.steps = steps;
        self
    }
}

/// Result of the verification phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    /// The underlying verification phase result
    pub phase_result: crate::step_executor::VerificationPhaseResult,
    /// Step execution results
    pub step_results: Vec<crate::step_executor::StepExecutionResult>,
}

// =============================================================================
// Agentic Phase Config
// =============================================================================

/// Configuration for the agentic phase executor.
#[derive(Debug, Clone)]
pub struct AgenticConfig {
    /// Maximum iterations
    pub max_iterations: u32,
    /// Base prompt for the AI
    pub base_prompt: String,
    /// Workflow name
    pub workflow_name: String,
    /// Workflow ID
    pub workflow_id: String,
    /// Execution ID
    pub execution_id: String,
    /// Current iteration
    pub iteration: u32,
    /// Failure context from verification
    pub failure_context: String,
    /// Whether agentic steps are defined
    pub has_agentic_steps: bool,
}

impl AgenticConfig {
    /// Create a new agentic configuration.
    pub fn new(
        execution_id: impl Into<String>,
        workflow_name: impl Into<String>,
        iteration: u32,
    ) -> Self {
        Self {
            max_iterations: 5,
            base_prompt: String::new(),
            workflow_name: workflow_name.into(),
            workflow_id: String::new(),
            execution_id: execution_id.into(),
            iteration,
            failure_context: String::new(),
            has_agentic_steps: true,
        }
    }

    /// Set the base prompt.
    #[must_use]
    pub fn with_base_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.base_prompt = prompt.into();
        self
    }

    /// Set the failure context.
    #[must_use]
    pub fn with_failure_context(mut self, context: impl Into<String>) -> Self {
        self.failure_context = context.into();
        self
    }

    /// Set whether agentic steps are defined.
    #[must_use]
    pub fn with_agentic_steps(mut self, has_steps: bool) -> Self {
        self.has_agentic_steps = has_steps;
        self
    }
}

// =============================================================================
// Completion Phase Config
// =============================================================================

/// Configuration for the completion phase executor.
#[derive(Debug, Clone)]
pub struct CompletionConfig {
    /// Automation steps to execute
    pub automation_steps: Vec<ExecutionStepConfig>,
    /// Prompt steps for AI summary
    pub prompt_steps: Vec<ExecutionStepConfig>,
    /// Unique execution ID
    pub execution_id: String,
    /// Workflow name
    pub workflow_name: String,
    /// Number of verification-agentic iterations that ran
    pub iterations_run: u32,
    /// Model override for completion phase AI calls
    pub model_override: Option<String>,
    /// Provider override for completion phase AI calls
    pub provider_override: Option<String>,
    /// When true, run prompt steps before automation steps.
    pub completion_prompts_first: bool,
}

impl CompletionConfig {
    /// Create a new completion configuration.
    pub fn new(
        execution_id: impl Into<String>,
        workflow_name: impl Into<String>,
        iterations_run: u32,
    ) -> Self {
        Self {
            automation_steps: Vec::new(),
            prompt_steps: Vec::new(),
            execution_id: execution_id.into(),
            workflow_name: workflow_name.into(),
            iterations_run,
            model_override: None,
            provider_override: None,
            completion_prompts_first: false,
        }
    }

    /// Add automation steps.
    #[must_use]
    pub fn with_automation_steps(mut self, steps: Vec<ExecutionStepConfig>) -> Self {
        self.automation_steps = steps;
        self
    }

    /// Add prompt steps.
    #[must_use]
    pub fn with_prompt_steps(mut self, steps: Vec<ExecutionStepConfig>) -> Self {
        self.prompt_steps = steps;
        self
    }
}

/// Result of the completion phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResult {
    /// Whether completion was successful
    pub success: bool,
    /// Step execution results
    pub step_results: Vec<crate::step_executor::StepExecutionResult>,
}
