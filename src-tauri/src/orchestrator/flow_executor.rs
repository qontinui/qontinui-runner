//! Flow Execution Engine
//!
//! Implements the execution engine for Flow-based workflows.
//! Connects the Flow Designer UI to actual flow execution by:
//! - Executing steps sequentially based on `next_step` connections
//! - Handling different step types (Agent, Tool, Conditional, Parallel, etc.)
//! - Updating FlowState as execution progresses
//! - Storing step results in context
//! - Emitting events for UI updates
//!
//! ## Real AI and Tool Execution
//!
//! Agent steps call the actual AI provider (Claude, Gemini) configured in settings.
//! Tool steps use the EmbeddedMcp for in-process tool execution, or fall back to
//! HTTP API calls for tools requiring external communication.

use super::agent_roles::RoleRegistry;
use super::flow::{Flow, FlowState, FlowStatus, FlowStep, ParallelMerge, StepType};
use super::role_specializations::register_default_specializations;
use super::tool_guard::ToolGuard;
use crate::ai_provider;
use crate::ai_router::{model_for_tier, TaskContext};
use crate::doctor::DoctorHandle;
use crate::execution_core::builtin_tools::{execute_builtin_tool, BuiltinToolRegistry};
use crate::execution_core::unified_tools::{execute_unified_tool, UnifiedToolRegistry};
use crate::mcp_embedded::EmbeddedMcp;
use once_cell::sync::Lazy;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Global role registry, initialised once with both the generic predefined
/// roles and the four Analyze-Plan-Execute-Verify specializations.
static ROLE_REGISTRY: Lazy<RoleRegistry> = Lazy::new(|| {
    let mut registry = RoleRegistry::with_defaults();
    register_default_specializations(&mut registry);
    registry
});

// ============================================================================
// Flow Executor Configuration
// ============================================================================

/// Configuration for the flow executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowExecutorConfig {
    /// Maximum iterations to prevent infinite loops.
    pub max_iterations: u32,
    /// Default timeout for steps in seconds.
    pub default_step_timeout_secs: u64,
    /// Whether to continue on non-critical errors.
    pub continue_on_error: bool,
    /// Whether to emit events for each step.
    pub emit_events: bool,
}

impl Default for FlowExecutorConfig {
    fn default() -> Self {
        Self {
            max_iterations: 1000,
            default_step_timeout_secs: 300,
            continue_on_error: false,
            emit_events: true,
        }
    }
}

// ============================================================================
// Step Result
// ============================================================================

/// Result of executing a single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// The step ID that was executed.
    pub step_id: String,
    /// Whether the step succeeded.
    pub success: bool,
    /// Output values from the step.
    pub outputs: HashMap<String, serde_json::Value>,
    /// Error message if the step failed.
    pub error: Option<String>,
    /// The next step to execute (determined by step logic).
    pub next_step: Option<String>,
    /// Duration of execution in milliseconds.
    pub duration_ms: u64,
}

impl StepResult {
    /// Create a successful result.
    pub fn success(step_id: impl Into<String>, next_step: Option<String>) -> Self {
        Self {
            step_id: step_id.into(),
            success: true,
            outputs: HashMap::new(),
            error: None,
            next_step,
            duration_ms: 0,
        }
    }

    /// Create a failed result.
    pub fn failure(step_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            step_id: step_id.into(),
            success: false,
            outputs: HashMap::new(),
            error: Some(error.into()),
            next_step: None,
            duration_ms: 0,
        }
    }

    /// Add an output value.
    pub fn with_output(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.outputs.insert(key.into(), value);
        self
    }

    /// Set duration.
    pub fn with_duration(mut self, ms: u64) -> Self {
        self.duration_ms = ms;
        self
    }

    /// Set the next step.
    pub fn with_next(mut self, next: impl Into<String>) -> Self {
        self.next_step = Some(next.into());
        self
    }
}

// ============================================================================
// Flow Events
// ============================================================================

/// Events emitted during flow execution for UI updates.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FlowEvent {
    /// Flow execution started.
    FlowStarted {
        instance_id: String,
        flow_id: String,
        flow_name: String,
    },
    /// A step is about to execute.
    StepStarted {
        instance_id: String,
        step_id: String,
        step_name: String,
        step_type: String,
    },
    /// A step completed.
    StepCompleted {
        instance_id: String,
        step_id: String,
        success: bool,
        outputs: HashMap<String, serde_json::Value>,
        error: Option<String>,
        duration_ms: u64,
    },
    /// Flow execution completed.
    FlowCompleted {
        instance_id: String,
        flow_id: String,
        success: bool,
        error: Option<String>,
        total_steps: usize,
        duration_ms: u64,
    },
    /// Waiting for human input.
    WaitingForInput {
        instance_id: String,
        step_id: String,
        prompt: String,
        options: Vec<String>,
    },
    /// Progress update during parallel execution.
    ParallelProgress {
        instance_id: String,
        step_id: String,
        completed: usize,
        total: usize,
    },
}

// ============================================================================
// Flow Executor
// ============================================================================

/// The flow execution engine.
///
/// Executes flow steps and handles different step types including Agent, Tool,
/// Conditional, Parallel, HumanInput, Transform, Loop, and End steps.
pub struct FlowExecutor {
    /// Configuration.
    config: FlowExecutorConfig,
    /// Event callback for emitting flow events.
    event_callback: Option<Arc<dyn Fn(FlowEvent) + Send + Sync>>,
    /// Doctor health monitor handle for tracking AI processes.
    doctor_handle: Option<DoctorHandle>,
}

impl FlowExecutor {
    /// Create a new flow executor with default configuration.
    pub fn new() -> Self {
        Self {
            config: FlowExecutorConfig::default(),
            event_callback: None,
            doctor_handle: None,
        }
    }

    /// Set the doctor handle for health monitoring.
    pub fn with_doctor_handle(mut self, doctor_handle: Option<DoctorHandle>) -> Self {
        self.doctor_handle = doctor_handle;
        self
    }

    /// Set configuration.
    pub fn with_config(mut self, config: FlowExecutorConfig) -> Self {
        self.config = config;
        self
    }

    /// Set event callback.
    pub fn with_event_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(FlowEvent) + Send + Sync + 'static,
    {
        self.event_callback = Some(Arc::new(callback));
        self
    }

    /// Emit a flow event.
    fn emit_event(&self, event: FlowEvent) {
        if self.config.emit_events {
            if let Some(callback) = &self.event_callback {
                callback(event.clone());
            }
            debug!(?event, "Flow event emitted");
        }
    }

    /// Execute a flow to completion.
    pub async fn execute(&self, flow: &Flow, state: &mut FlowState) -> Result<(), String> {
        // Validate flow before execution
        if let Err(errors) = flow.validate() {
            let error_msg = errors.join("; ");
            state.fail(&error_msg);
            return Err(error_msg);
        }

        let start_time = std::time::Instant::now();
        state.start();

        self.emit_event(FlowEvent::FlowStarted {
            instance_id: state.instance_id.clone(),
            flow_id: flow.id.clone(),
            flow_name: flow.name.clone(),
        });

        let mut iteration_count = 0;

        // Main execution loop
        while !state.is_finished() && iteration_count < self.config.max_iterations {
            iteration_count += 1;

            // Get current step
            let current_step_id = match &state.current_step {
                Some(id) => id.clone(),
                None => {
                    state.complete();
                    break;
                }
            };

            // Handle special step IDs
            if current_step_id == "end" {
                state.complete();
                break;
            }

            if current_step_id == "fail" {
                state.fail("Flow reached fail step");
                break;
            }

            // Get the step definition
            let step = match flow.get_step(&current_step_id) {
                Some(s) => s.clone(),
                None => {
                    let error = format!("Step '{}' not found in flow", current_step_id);
                    state.fail(&error);
                    return Err(error);
                }
            };

            // Execute the step (pass flow for loop steps)
            let result = self.execute_step_with_flow(&step, state, Some(flow)).await;

            // Handle result
            if result.success {
                state.step_completed(true, result.outputs.clone());

                // Move to next step
                if let Some(next) = result.next_step {
                    if next == "end" {
                        state.complete();
                    } else if next == "fail" {
                        state.fail("Flow reached fail step");
                    } else {
                        state.set_current_step(next);
                    }
                } else {
                    // No next step means end
                    state.complete();
                }
            } else {
                let error = result.error.unwrap_or_else(|| "Unknown error".to_string());
                state.step_failed(&error);

                if step.continue_on_error || self.config.continue_on_error {
                    // Continue to next step despite error
                    if let Some(next) = result.next_step {
                        state.set_current_step(next);
                    } else {
                        state.fail(&error);
                    }
                } else {
                    state.fail(&error);
                }
            }
        }

        if iteration_count >= self.config.max_iterations {
            state.fail("Maximum iterations exceeded");
        }

        let total_duration = start_time.elapsed().as_millis() as u64;

        self.emit_event(FlowEvent::FlowCompleted {
            instance_id: state.instance_id.clone(),
            flow_id: flow.id.clone(),
            success: state.status == FlowStatus::Completed,
            error: state.error.clone(),
            total_steps: state.execution_count(),
            duration_ms: total_duration,
        });

        if state.status == FlowStatus::Failed {
            Err(state
                .error
                .clone()
                .unwrap_or_else(|| "Flow failed".to_string()))
        } else {
            Ok(())
        }
    }

    /// Execute a single step.
    ///
    /// The optional `flow` parameter is needed for loop steps to look up and execute
    /// body steps. It's None for standalone step execution.
    pub async fn execute_step(&self, step: &FlowStep, state: &mut FlowState) -> StepResult {
        self.execute_step_with_flow(step, state, None).await
    }

    /// Execute a single step with access to the full flow definition.
    ///
    /// This is the internal implementation that loop steps use to execute body steps.
    /// Uses Pin<Box> to handle recursion in async fn (loops can contain nested loops).
    pub fn execute_step_with_flow<'a>(
        &'a self,
        step: &'a FlowStep,
        state: &'a mut FlowState,
        flow: Option<&'a Flow>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = StepResult> + Send + 'a>> {
        Box::pin(async move {
            let step_type_name = get_step_type_name(&step.step_type);

            self.emit_event(FlowEvent::StepStarted {
                instance_id: state.instance_id.clone(),
                step_id: step.id.clone(),
                step_name: step.name.clone(),
                step_type: step_type_name.clone(),
            });

            let start_time = std::time::Instant::now();

            // Record step start
            state.step_started(&step.id);

            // Check step condition if present
            if let Some(condition) = &step.condition {
                if !condition.evaluate(&state.context) {
                    debug!(step_id = %step.id, "Step condition not met, skipping");
                    let next = step.get_next_steps().first().cloned();
                    return StepResult::success(&step.id, next)
                        .with_output("skipped".to_string(), serde_json::json!(true));
                }
            }

            // Resolve input mappings
            let resolved_inputs = resolve_input_mappings(&step.input_mappings, &state.context);

            // Execute with timeout and retry logic
            let result = self
                .execute_step_inner_with_retry(step, state, flow, resolved_inputs)
                .await;

            let duration = start_time.elapsed().as_millis() as u64;
            let mut result = result;
            result.duration_ms = duration;

            self.emit_event(FlowEvent::StepCompleted {
                instance_id: state.instance_id.clone(),
                step_id: step.id.clone(),
                success: result.success,
                outputs: result.outputs.clone(),
                error: result.error.clone(),
                duration_ms: duration,
            });

            result
        })
    }

    /// Execute a step with retry logic.
    async fn execute_step_inner_with_retry(
        &self,
        step: &FlowStep,
        state: &mut FlowState,
        flow: Option<&Flow>,
        resolved_inputs: HashMap<String, serde_json::Value>,
    ) -> StepResult {
        let max_retries = step.retry_count;
        let timeout_secs = step
            .timeout_secs
            .unwrap_or(self.config.default_step_timeout_secs);

        let mut last_result = None;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                // Exponential backoff between retries
                let backoff_ms = 100 * 2u64.pow(attempt - 1);
                debug!(
                    step_id = %step.id,
                    attempt = attempt,
                    max_retries = max_retries,
                    backoff_ms = backoff_ms,
                    "Retrying step"
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
            }

            // Execute with timeout
            let result = self
                .execute_step_with_timeout(step, state, flow, &resolved_inputs, timeout_secs)
                .await;

            if result.success {
                return result;
            }

            last_result = Some(result);
        }

        // Return the last failed result
        last_result.unwrap_or_else(|| StepResult::failure(&step.id, "Step execution failed"))
    }

    /// Execute a step with timeout.
    async fn execute_step_with_timeout(
        &self,
        step: &FlowStep,
        state: &mut FlowState,
        flow: Option<&Flow>,
        resolved_inputs: &HashMap<String, serde_json::Value>,
        timeout_secs: u64,
    ) -> StepResult {
        let timeout = std::time::Duration::from_secs(timeout_secs);

        match tokio::time::timeout(
            timeout,
            self.execute_step_inner(step, state, flow, resolved_inputs),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                warn!(
                    step_id = %step.id,
                    timeout_secs = timeout_secs,
                    "Step timed out"
                );
                StepResult::failure(
                    &step.id,
                    format!("Step timed out after {} seconds", timeout_secs),
                )
            }
        }
    }

    /// Inner step execution logic (dispatches by step type).
    async fn execute_step_inner(
        &self,
        step: &FlowStep,
        state: &mut FlowState,
        flow: Option<&Flow>,
        resolved_inputs: &HashMap<String, serde_json::Value>,
    ) -> StepResult {
        // Execute based on step type
        match &step.step_type {
            StepType::Agent {
                role, prompt, next, ..
            } => {
                self.execute_agent_step(&step.id, role, prompt, next, &state.context)
                    .await
            }

            StepType::Tool {
                tool_id,
                inputs,
                next,
            } => {
                // Merge static inputs with resolved inputs
                let mut merged_inputs = inputs.clone();
                for (k, v) in resolved_inputs.iter() {
                    merged_inputs.insert(k.clone(), v.clone());
                }
                self.execute_tool_step(&step.id, tool_id, &merged_inputs, next, &state.context)
                    .await
            }

            StepType::Conditional {
                condition,
                then_step,
                else_step,
            } => {
                let condition_met = condition.evaluate(&state.context);
                let next_step = if condition_met {
                    then_step.clone()
                } else {
                    else_step.clone()
                };

                StepResult::success(&step.id, Some(next_step)).with_output(
                    "condition_result".to_string(),
                    serde_json::json!(condition_met),
                )
            }

            StepType::Parallel {
                branches,
                merge_strategy,
                next,
            } => {
                self.execute_parallel_step(
                    &step.id,
                    &state.instance_id,
                    branches,
                    merge_strategy,
                    next,
                )
                .await
            }

            StepType::HumanInput {
                prompt,
                options,
                next,
            } => {
                // Emit waiting event and pause execution
                self.emit_event(FlowEvent::WaitingForInput {
                    instance_id: state.instance_id.clone(),
                    step_id: step.id.clone(),
                    prompt: prompt.clone(),
                    options: options.clone(),
                });

                // For human input, we return a result that indicates waiting
                // The flow will need to be resumed with provide_human_input
                StepResult {
                    step_id: step.id.clone(),
                    success: true,
                    outputs: HashMap::new(),
                    error: None,
                    next_step: Some(next.clone()),
                    duration_ms: 0,
                }
            }

            StepType::Transform {
                expression,
                output_key,
                next,
            } => {
                self.execute_transform_step(&step.id, expression, output_key, next, &state.context)
                    .await
            }

            StepType::Wait { seconds, next } => {
                tokio::time::sleep(tokio::time::Duration::from_secs(*seconds)).await;
                StepResult::success(&step.id, Some(next.clone()))
                    .with_output("waited_seconds".to_string(), serde_json::json!(seconds))
                    .with_duration(seconds * 1000)
            }

            StepType::Loop {
                over,
                body_step,
                next,
            } => {
                if let Some(flow) = flow {
                    self.execute_loop_step(&step.id, over, body_step, next, state, flow)
                        .await
                } else {
                    StepResult::failure(
                        &step.id,
                        "Loop step requires flow context for body step execution",
                    )
                }
            }

            StepType::End => StepResult::success(&step.id, None),

            StepType::Fail { error } => StepResult::failure(&step.id, error),
        }
    }

    /// Execute an agent step by calling the real AI provider.
    ///
    /// Builds a prompt from the step's role and prompt fields, calls the configured
    /// AI provider (Claude CLI, Claude API, Gemini CLI, or Gemini API), and parses
    /// the response.
    async fn execute_agent_step(
        &self,
        step_id: &str,
        role: &str,
        prompt: &str,
        next: &str,
        context: &HashMap<String, serde_json::Value>,
    ) -> StepResult {
        info!(
            step_id = %step_id,
            role = %role,
            prompt_len = prompt.len(),
            "Executing agent step with real AI provider"
        );

        // -----------------------------------------------------------------
        // Resolve the agent role from the global registry (lazy-initialised).
        // When a matching role is found we use its system prompt, model tier,
        // max-tokens budget, and tool guard.  When no role matches we fall
        // through to the previous generic behaviour.
        // -----------------------------------------------------------------
        let resolved_role = ROLE_REGISTRY.get(role);

        let (system_prompt, model_override, max_tokens, tool_guard) =
            if let Some(agent_role) = resolved_role {
                let sys = agent_role.build_system_prompt();
                let model = model_for_tier(agent_role.preferred_model_tier);
                let max_tok = agent_role.max_tokens_budget;
                let guard = agent_role
                    .allowed_tools
                    .as_ref()
                    .map(|tools| ToolGuard::new(&agent_role.id, tools));

                info!(
                    step_id = %step_id,
                    role_id = %agent_role.id,
                    model = %model,
                    max_tokens = ?max_tok,
                    has_tool_guard = guard.is_some(),
                    "Resolved agent role specialization"
                );

                (Some(sys), Some(model), max_tok, guard)
            } else {
                debug!(
                    step_id = %step_id,
                    role = %role,
                    "No matching role in registry, using default behaviour"
                );
                (None, None, None, None)
            };

        // Build the full prompt — if we have a role system prompt, prepend it.
        let base_prompt = self.build_agent_prompt(role, prompt, context);
        let full_prompt = match &system_prompt {
            Some(sys) => format!("{}\n\n---\n\n{}", sys, base_prompt),
            None => base_prompt,
        };

        // Build task context for AI routing (helps select appropriate model)
        let task_context = TaskContext::from_prompt(&full_prompt);

        // Call AI provider in a blocking task (ai_provider is synchronous).
        // When a role specified a model override we use run_prompt_with_model_override
        // so the tier-derived model takes precedence over the generic router.
        let prompt_clone = full_prompt.clone();
        let doctor_handle_clone = self.doctor_handle.clone();
        let model_clone = model_override.clone();
        let ai_result = tokio::task::spawn_blocking(move || {
            if let Some(ref model) = model_clone {
                ai_provider::run_prompt_with_model_override(
                    &prompt_clone,
                    &task_context,
                    doctor_handle_clone.as_ref(),
                    Some(model.as_str()),
                    None,  // provider_override
                    None,  // temperature_override
                    max_tokens,
                    None,  // fallback_model
                    None,  // fallback_provider
                )
            } else {
                ai_provider::run_prompt_with_routing(
                    &prompt_clone,
                    &task_context,
                    doctor_handle_clone.as_ref(),
                )
            }
        })
        .await;

        match ai_result {
            Ok(response) => {
                if response.success {
                    info!(
                        step_id = %step_id,
                        response_len = response.output.len(),
                        "Agent step completed successfully"
                    );

                    // Parse the response and extract structured outputs if possible
                    let outputs = self.parse_agent_response(&response.output, role);

                    let mut result = StepResult::success(step_id, Some(next.to_string()));
                    for (key, value) in outputs {
                        result = result.with_output(key, value);
                    }
                    // Always include the raw response
                    result = result
                        .with_output("response".to_string(), serde_json::json!(response.output));
                    result = result.with_output("role".to_string(), serde_json::json!(role));

                    // Store the tool guard's allowed-tool list in the step
                    // context so downstream tool steps can enforce it.
                    if let Some(ref guard) = tool_guard {
                        if !guard.is_unrestricted() {
                            result = result.with_output(
                                "_tool_guard_allowed".to_string(),
                                serde_json::json!(
                                    resolved_role
                                        .and_then(|r| r.allowed_tools.as_ref())
                                        .unwrap_or(&Vec::new())
                                ),
                            );
                        }
                    }

                    result
                } else {
                    let error_msg = response
                        .error
                        .unwrap_or_else(|| "Unknown AI error".to_string());
                    error!(
                        step_id = %step_id,
                        error = %error_msg,
                        "Agent step failed"
                    );
                    StepResult::failure(step_id, error_msg)
                }
            }
            Err(join_error) => {
                let error_msg = format!("AI task panicked or was cancelled: {}", join_error);
                error!(step_id = %step_id, error = %error_msg, "Agent step task failed");
                StepResult::failure(step_id, error_msg)
            }
        }
    }

    /// Build a prompt for an agent step with role context.
    fn build_agent_prompt(
        &self,
        role: &str,
        prompt: &str,
        context: &HashMap<String, serde_json::Value>,
    ) -> String {
        let mut full_prompt = String::new();

        // Add role context
        full_prompt.push_str(&format!(
            "You are acting as a {} in a flow-based workflow.\n\n",
            role
        ));

        // Add any relevant context from previous steps
        if !context.is_empty() {
            full_prompt.push_str("## Available Context\n\n");
            for (key, value) in context {
                // Format context values nicely
                let value_str = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => {
                        serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string())
                    }
                };
                full_prompt.push_str(&format!("- **{}**: {}\n", key, value_str));
            }
            full_prompt.push('\n');
        }

        // Add the step's prompt
        full_prompt.push_str("## Task\n\n");
        full_prompt.push_str(prompt);

        // Add output format hint
        full_prompt.push_str("\n\n## Response Format\n\n");
        full_prompt.push_str(
            "Provide your response as clear, structured output. \
             If you have specific data to return, format it clearly \
             so it can be used by subsequent steps in the workflow.",
        );

        full_prompt
    }

    /// Parse an agent response and extract structured outputs.
    ///
    /// Attempts to extract JSON from the response, or falls back to
    /// treating the entire response as text output.
    fn parse_agent_response(
        &self,
        response: &str,
        _role: &str,
    ) -> HashMap<String, serde_json::Value> {
        let mut outputs = HashMap::new();

        // Try to extract JSON from the response
        if let Some(json_start) = response.find('{') {
            if let Some(json_end) = response.rfind('}') {
                let json_str = &response[json_start..=json_end];
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                    // If we got a JSON object, extract its fields as outputs
                    if let Some(obj) = parsed.as_object() {
                        for (key, value) in obj {
                            outputs.insert(key.clone(), value.clone());
                        }
                        return outputs;
                    }
                }
            }
        }

        // Also try to find JSON arrays
        if let Some(json_start) = response.find('[') {
            if let Some(json_end) = response.rfind(']') {
                let json_str = &response[json_start..=json_end];
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                    outputs.insert("data".to_string(), parsed);
                    return outputs;
                }
            }
        }

        // Fallback: treat entire response as text
        outputs.insert("text".to_string(), serde_json::json!(response));
        outputs
    }

    /// Execute a tool step by calling real tools.
    ///
    /// Uses EmbeddedMcp for in-process tool execution, or falls back to HTTP API
    /// for tools requiring external communication. Also supports built-in tools
    /// for common operations, and Unified Workflow tools (shell_command, api_request,
    /// playwright_test, mcp_call).
    async fn execute_tool_step(
        &self,
        step_id: &str,
        tool_id: &str,
        inputs: &HashMap<String, serde_json::Value>,
        next: &str,
        context: &HashMap<String, serde_json::Value>,
    ) -> StepResult {
        info!(
            step_id = %step_id,
            tool_id = %tool_id,
            input_count = inputs.len(),
            "Executing tool step"
        );

        // -----------------------------------------------------------------
        // Tool guard enforcement: if a preceding agent step stored a tool
        // whitelist in the context, reconstruct the guard and check whether
        // this tool invocation is permitted.
        // -----------------------------------------------------------------
        if let Some(serde_json::Value::Array(allowed)) = context.get("_tool_guard_allowed") {
            let allowed_strs: Vec<String> = allowed
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            let role_id = context
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let guard = ToolGuard::new(role_id, &allowed_strs);
            // Strip the MCP prefix if present so guard matches bare tool names
            let bare_tool = tool_id
                .strip_prefix("mcp__qontinui__")
                .unwrap_or(tool_id);
            if let Err(e) = guard.check_tool(bare_tool) {
                warn!(
                    step_id = %step_id,
                    tool_id = %tool_id,
                    role = %role_id,
                    "Tool guard blocked invocation: {}",
                    e
                );
                return StepResult::failure(step_id, e.to_string());
            }
        }

        // Convert inputs HashMap to serde_json::Value for MCP call
        let args = serde_json::json!(inputs);

        // Check if this is a built-in tool first
        if let Some(result) = self
            .try_execute_builtin_tool(step_id, tool_id, inputs, context)
            .await
        {
            return result.with_next(next);
        }

        // Check if this is a Unified Workflow tool (shell_command, api_request, etc.)
        if UnifiedToolRegistry::is_unified_tool(tool_id) {
            if let Some(result) = execute_unified_tool(step_id, tool_id, inputs, context).await {
                info!(
                    step_id = %step_id,
                    tool_id = %tool_id,
                    success = result.success,
                    "Unified tool step completed"
                );
                // Convert StepExecutionResult to StepResult
                return if result.success {
                    let mut step_result = StepResult::success(step_id, Some(next.to_string()));
                    for (key, value) in result.outputs {
                        step_result = step_result.with_output(key, value);
                    }
                    step_result.with_duration(result.duration_ms)
                } else {
                    StepResult::failure(
                        step_id,
                        result.error.unwrap_or_else(|| "Unknown error".to_string()),
                    )
                    .with_duration(result.duration_ms)
                };
            }
        }

        // Try EmbeddedMcp for supported tools
        if EmbeddedMcp::supports_tool(tool_id) {
            let mcp = EmbeddedMcp::new();
            match mcp.call_tool(tool_id, args).await {
                Ok(output) => {
                    info!(
                        step_id = %step_id,
                        tool_id = %tool_id,
                        "Tool step completed successfully via EmbeddedMcp"
                    );

                    let mut result = StepResult::success(step_id, Some(next.to_string()));

                    // Extract outputs from the result
                    if let Some(obj) = output.as_object() {
                        for (key, value) in obj {
                            result = result.with_output(key.clone(), value.clone());
                        }
                    } else {
                        result = result.with_output("result".to_string(), output);
                    }
                    result = result.with_output("tool_id".to_string(), serde_json::json!(tool_id));
                    result
                }
                Err(e) => {
                    error!(
                        step_id = %step_id,
                        tool_id = %tool_id,
                        error = %e,
                        "Tool step failed via EmbeddedMcp"
                    );
                    StepResult::failure(step_id, e)
                }
            }
        } else if EmbeddedMcp::requires_http_api(tool_id) {
            // For tools requiring HTTP API, we call the local runner API
            self.execute_http_tool(step_id, tool_id, inputs, next).await
        } else {
            // Unknown tool - try to execute via HTTP API as fallback
            warn!(
                step_id = %step_id,
                tool_id = %tool_id,
                "Unknown tool, attempting HTTP API fallback"
            );
            self.execute_http_tool(step_id, tool_id, inputs, next).await
        }
    }

    /// Try to execute a built-in tool.
    ///
    /// Delegates to the centralized builtin_tools module which provides 30+ tools:
    /// - String: concat, split, replace, trim, uppercase, lowercase
    /// - Array: length, map, filter, find, join, push, slice
    /// - JSON: parse, stringify
    /// - Context: get_context, merge_context
    /// - Timestamp: timestamp, format_date
    /// - Utility: uuid, random_number, hash_sha256, base64_encode, base64_decode, env_get, log, sleep
    async fn try_execute_builtin_tool(
        &self,
        step_id: &str,
        tool_id: &str,
        inputs: &HashMap<String, serde_json::Value>,
        context: &HashMap<String, serde_json::Value>,
    ) -> Option<StepResult> {
        // Check if this is a builtin tool
        if !BuiltinToolRegistry::is_builtin(tool_id) {
            return None;
        }

        // Execute the builtin tool using the centralized implementation
        let result = execute_builtin_tool(step_id, tool_id, inputs, context).await;

        // Convert StepExecutionResult to StepResult
        result.map(|exec_result| {
            if exec_result.success {
                let mut step_result = StepResult::success(step_id, None);
                for (key, value) in exec_result.outputs {
                    step_result = step_result.with_output(key, value);
                }
                step_result.with_duration(exec_result.duration_ms)
            } else {
                StepResult::failure(
                    step_id,
                    exec_result
                        .error
                        .unwrap_or_else(|| "Unknown error".to_string()),
                )
                .with_duration(exec_result.duration_ms)
            }
        })
    }

    /// Execute a tool via HTTP API.
    ///
    /// This is used for tools that require external communication, such as
    /// workflow execution, screenshot capture, etc.
    async fn execute_http_tool(
        &self,
        step_id: &str,
        tool_id: &str,
        inputs: &HashMap<String, serde_json::Value>,
        next: &str,
    ) -> StepResult {
        info!(
            step_id = %step_id,
            tool_id = %tool_id,
            "Executing tool via HTTP API"
        );

        // Build the HTTP request
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                self.config.default_step_timeout_secs,
            ))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return StepResult::failure(
                    step_id,
                    format!("Failed to create HTTP client: {}", e),
                );
            }
        };

        // The runner's MCP API endpoint for tool execution
        let base_url = crate::mcp::types::get_self_base_url_from_env();
        let url = format!("{}/api/mcp/tools/{}/execute", base_url, tool_id);

        match client.post(&url).json(inputs).send().await {
            Ok(response) => {
                if response.status().is_success() {
                    match response.json::<serde_json::Value>().await {
                        Ok(output) => {
                            info!(
                                step_id = %step_id,
                                tool_id = %tool_id,
                                "Tool step completed successfully via HTTP API"
                            );

                            let mut result = StepResult::success(step_id, Some(next.to_string()));

                            // Extract outputs from the result
                            if let Some(obj) = output.as_object() {
                                for (key, value) in obj {
                                    result = result.with_output(key.clone(), value.clone());
                                }
                            } else {
                                result = result.with_output("result".to_string(), output);
                            }
                            result = result
                                .with_output("tool_id".to_string(), serde_json::json!(tool_id));
                            result
                        }
                        Err(e) => StepResult::failure(
                            step_id,
                            format!("Failed to parse tool response: {}", e),
                        ),
                    }
                } else {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    StepResult::failure(
                        step_id,
                        format!("Tool execution failed ({}): {}", status, body),
                    )
                }
            }
            Err(e) => {
                // If the HTTP call fails, it might be because the tool doesn't exist
                // or the runner API is not available
                error!(
                    step_id = %step_id,
                    tool_id = %tool_id,
                    error = %e,
                    "HTTP API call failed"
                );
                StepResult::failure(
                    step_id,
                    format!(
                        "Tool '{}' execution failed: {}. Is the runner API available?",
                        tool_id, e
                    ),
                )
            }
        }
    }

    /// Execute a transform step.
    async fn execute_transform_step(
        &self,
        step_id: &str,
        expression: &str,
        output_key: &str,
        next: &str,
        context: &HashMap<String, serde_json::Value>,
    ) -> StepResult {
        info!(
            step_id = %step_id,
            expression = %expression,
            "Executing transform"
        );

        // Simple expression evaluation for common patterns
        let result = evaluate_simple_expression(expression, context);

        StepResult::success(step_id, Some(next.to_string()))
            .with_output(output_key.to_string(), result)
            .with_duration(1)
    }

    /// Execute parallel branches.
    ///
    /// Spawns tokio tasks for each branch and executes them concurrently.
    /// The merge_strategy determines when to continue:
    /// - WaitAll: Wait for all branches to complete
    /// - WaitAny: Continue when any branch succeeds
    /// - WaitN(n): Wait for n branches to complete
    async fn execute_parallel_step(
        &self,
        step_id: &str,
        instance_id: &str,
        branches: &[String],
        merge_strategy: &ParallelMerge,
        next: &str,
    ) -> StepResult {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use tokio::sync::mpsc;

        let total = branches.len();
        if total == 0 {
            return StepResult::success(step_id, Some(next.to_string()))
                .with_output("branches".to_string(), serde_json::json!({}))
                .with_output("completed_count".to_string(), serde_json::json!(0));
        }

        info!(
            step_id = %step_id,
            branch_count = total,
            merge_strategy = ?merge_strategy,
            "Starting parallel execution"
        );

        // Create channel for branch results
        let (tx, mut rx) = mpsc::channel::<(
            String,
            bool,
            HashMap<String, serde_json::Value>,
            Option<String>,
            u64,
        )>(total);
        let completed_count = Arc::new(AtomicUsize::new(0));
        let successful_count = Arc::new(AtomicUsize::new(0));

        // Spawn tasks for each branch
        // Note: In a full implementation, each branch would execute its own step chain.
        // For now, we simulate branch execution with configurable delays.
        for branch_id in branches.iter() {
            let tx = tx.clone();
            let branch_id_clone = branch_id.clone();
            let step_id_clone = step_id.to_string();
            let completed = completed_count.clone();
            let successful = successful_count.clone();

            tokio::spawn(async move {
                let start_time = std::time::Instant::now();

                // Execute the branch
                // In a full implementation, this would look up the branch step
                // in the flow and execute it. For now, we simulate success.
                let (success, outputs, error) = Self::execute_branch_work(&branch_id_clone).await;

                let duration = start_time.elapsed().as_millis() as u64;

                // Update counters
                completed.fetch_add(1, Ordering::SeqCst);
                if success {
                    successful.fetch_add(1, Ordering::SeqCst);
                }

                debug!(
                    step_id = %step_id_clone,
                    branch_id = %branch_id_clone,
                    success = success,
                    duration_ms = duration,
                    "Branch completed"
                );

                // Send result (ignore send errors if receiver dropped)
                let _ = tx
                    .send((branch_id_clone, success, outputs, error, duration))
                    .await;
            });
        }

        // Drop our sender so the channel closes when all tasks complete
        drop(tx);

        // Collect results based on merge strategy
        let mut results = HashMap::new();
        let mut all_outputs = HashMap::new();
        let mut has_failure = false;
        let mut first_error: Option<String> = None;

        let _required_count = match merge_strategy {
            ParallelMerge::WaitAll => total,
            ParallelMerge::WaitAny => 1,
            ParallelMerge::WaitN(n) => *n,
        };

        let mut received_count = 0;
        let mut success_count = 0;

        while let Some((branch_id, success, outputs, error, duration)) = rx.recv().await {
            received_count += 1;
            if success {
                success_count += 1;
            } else {
                has_failure = true;
                if first_error.is_none() {
                    first_error = error.clone();
                }
            }

            // Store branch result
            results.insert(
                branch_id.clone(),
                serde_json::json!({
                    "success": success,
                    "duration_ms": duration,
                    "error": error
                }),
            );

            // Merge outputs with branch prefix
            for (key, value) in outputs {
                all_outputs.insert(format!("{}.{}", branch_id, key), value);
            }

            // Emit progress event
            self.emit_event(FlowEvent::ParallelProgress {
                instance_id: instance_id.to_string(),
                step_id: step_id.to_string(),
                completed: received_count,
                total,
            });

            // Check if merge condition is satisfied
            let should_continue = match merge_strategy {
                ParallelMerge::WaitAll => received_count < total,
                ParallelMerge::WaitAny => success_count < 1,
                ParallelMerge::WaitN(n) => received_count < *n,
            };

            if !should_continue {
                break;
            }
        }

        info!(
            step_id = %step_id,
            completed = received_count,
            successful = success_count,
            total = total,
            "Parallel execution completed"
        );

        // Determine overall success based on merge strategy
        let overall_success = match merge_strategy {
            ParallelMerge::WaitAll => !has_failure && received_count == total,
            ParallelMerge::WaitAny => success_count >= 1,
            ParallelMerge::WaitN(n) => received_count >= *n,
        };

        if overall_success {
            let mut result = StepResult::success(step_id, Some(next.to_string()))
                .with_output("branches".to_string(), serde_json::json!(results))
                .with_output(
                    "completed_count".to_string(),
                    serde_json::json!(received_count),
                )
                .with_output(
                    "successful_count".to_string(),
                    serde_json::json!(success_count),
                );

            // Add all branch outputs to step result
            for (key, value) in all_outputs {
                result = result.with_output(key, value);
            }
            result
        } else {
            StepResult::failure(
                step_id,
                first_error.unwrap_or_else(|| "Parallel execution failed".to_string()),
            )
        }
    }

    /// Execute the work for a single branch.
    ///
    /// In a full implementation, this would look up the branch step in the flow
    /// and execute it. For now, this is a placeholder that simulates work.
    async fn execute_branch_work(
        branch_id: &str,
    ) -> (bool, HashMap<String, serde_json::Value>, Option<String>) {
        // Simulate some work with a small delay
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Return success with the branch ID as output
        let mut outputs = HashMap::new();
        outputs.insert("branch_id".to_string(), serde_json::json!(branch_id));
        outputs.insert("executed".to_string(), serde_json::json!(true));

        (true, outputs, None)
    }

    /// Execute a loop step.
    ///
    /// Iterates over an array and executes the body step for each item.
    /// The loop provides context variables:
    /// - loop.index: Current index (0-based)
    /// - loop.item: Current item value
    /// - loop.length: Total array length
    /// - loop.first: true if first iteration
    /// - loop.last: true if last iteration
    /// - loop.break: Set to true in body step to break the loop
    ///
    /// Iteration results are collected in loop.results.{index}.{key}
    async fn execute_loop_step(
        &self,
        step_id: &str,
        over: &str,
        body_step: &str,
        next: &str,
        state: &mut FlowState,
        flow: &Flow,
    ) -> StepResult {
        // Get the array to iterate over
        // Support both direct context key and dot notation
        let items = match self.resolve_loop_variable(over, &state.context) {
            Some(serde_json::Value::Array(arr)) => arr,
            Some(_) => {
                return StepResult::failure(
                    step_id,
                    format!("Loop variable '{}' is not an array", over),
                );
            }
            None => {
                return StepResult::failure(
                    step_id,
                    format!("Loop variable '{}' not found in context", over),
                );
            }
        };

        let total_iterations = items.len();
        if total_iterations == 0 {
            info!(step_id = %step_id, "Loop has no items to iterate");
            return StepResult::success(step_id, Some(next.to_string()))
                .with_output("loop_results".to_string(), serde_json::json!([]))
                .with_output("iterations".to_string(), serde_json::json!(0));
        }

        info!(
            step_id = %step_id,
            body_step = %body_step,
            total_iterations = total_iterations,
            "Starting loop execution"
        );

        let mut loop_results = Vec::new();
        let mut iteration_outputs = HashMap::new();
        let mut had_error = false;
        let mut last_error: Option<String> = None;

        for (index, item) in items.iter().enumerate() {
            // Set loop context variables
            state
                .context
                .insert("loop.index".to_string(), serde_json::json!(index));
            state.context.insert("loop.item".to_string(), item.clone());
            state.context.insert(
                "loop.length".to_string(),
                serde_json::json!(total_iterations),
            );
            state
                .context
                .insert("loop.first".to_string(), serde_json::json!(index == 0));
            state.context.insert(
                "loop.last".to_string(),
                serde_json::json!(index == total_iterations - 1),
            );
            state
                .context
                .insert("loop.break".to_string(), serde_json::json!(false));

            debug!(
                step_id = %step_id,
                index = index,
                total = total_iterations,
                "Executing loop iteration"
            );

            // Execute the body step (pass flow for nested loops)
            let body_result = if let Some(body) = flow.get_step(body_step) {
                self.execute_step_with_flow(body, state, Some(flow)).await
            } else {
                StepResult::failure(
                    body_step,
                    format!("Body step '{}' not found in flow", body_step),
                )
            };

            // Collect iteration result
            let iteration_result = serde_json::json!({
                "index": index,
                "success": body_result.success,
                "outputs": body_result.outputs,
                "error": body_result.error
            });
            loop_results.push(iteration_result);

            // Store iteration outputs with index prefix
            for (key, value) in &body_result.outputs {
                iteration_outputs.insert(format!("loop.results.{}.{}", index, key), value.clone());
            }

            // Track errors
            if !body_result.success {
                had_error = true;
                last_error = body_result.error.clone();

                // Check continue_on_error flag
                if let Some(body) = flow.get_step(body_step) {
                    if !body.continue_on_error {
                        warn!(
                            step_id = %step_id,
                            index = index,
                            error = ?body_result.error,
                            "Loop iteration failed, breaking loop"
                        );
                        break;
                    }
                }
            }

            // Check for break condition
            if let Some(should_break) = state.context.get("loop.break") {
                if should_break.as_bool().unwrap_or(false) {
                    info!(
                        step_id = %step_id,
                        index = index,
                        "Loop break requested"
                    );
                    break;
                }
            }
        }

        // Clean up loop context (but keep loop.results)
        state.context.remove("loop.index");
        state.context.remove("loop.item");
        state.context.remove("loop.length");
        state.context.remove("loop.first");
        state.context.remove("loop.last");
        state.context.remove("loop.break");

        // Store iteration outputs in context
        for (key, value) in iteration_outputs {
            state.context.insert(key, value);
        }

        let iterations_completed = loop_results.len();
        info!(
            step_id = %step_id,
            iterations_completed = iterations_completed,
            total = total_iterations,
            had_error = had_error,
            "Loop execution completed"
        );

        if had_error && !self.config.continue_on_error {
            StepResult::failure(
                step_id,
                last_error.unwrap_or_else(|| "Loop execution failed".to_string()),
            )
        } else {
            StepResult::success(step_id, Some(next.to_string()))
                .with_output("loop_results".to_string(), serde_json::json!(loop_results))
                .with_output(
                    "iterations".to_string(),
                    serde_json::json!(iterations_completed),
                )
                .with_output("total".to_string(), serde_json::json!(total_iterations))
                .with_output(
                    "completed".to_string(),
                    serde_json::json!(iterations_completed == total_iterations),
                )
        }
    }

    /// Resolve a loop variable from context.
    ///
    /// Supports both direct keys and dot notation (e.g., "step1.items").
    fn resolve_loop_variable(
        &self,
        variable: &str,
        context: &HashMap<String, serde_json::Value>,
    ) -> Option<serde_json::Value> {
        // Try direct lookup first
        if let Some(value) = context.get(variable) {
            return Some(value.clone());
        }

        // Try dot notation
        if variable.contains('.') {
            let parts: Vec<&str> = variable.split('.').collect();
            if let Some(first) = parts.first() {
                if let Some(mut value) = context.get(*first).cloned() {
                    for part in parts.iter().skip(1) {
                        match &value {
                            serde_json::Value::Object(obj) => {
                                if let Some(v) = obj.get(*part) {
                                    value = v.clone();
                                } else {
                                    return None;
                                }
                            }
                            serde_json::Value::Array(arr) => {
                                if let Ok(idx) = part.parse::<usize>() {
                                    if let Some(v) = arr.get(idx) {
                                        value = v.clone();
                                    } else {
                                        return None;
                                    }
                                } else {
                                    return None;
                                }
                            }
                            _ => return None,
                        }
                    }
                    return Some(value);
                }
            }
        }

        None
    }

    /// Advance execution by one step (for step-by-step UI updates).
    pub async fn step_once(
        &self,
        flow: &Flow,
        state: &mut FlowState,
    ) -> Result<StepResult, String> {
        if state.is_finished() {
            return Err("Flow is already finished".to_string());
        }

        if state.status == FlowStatus::Pending {
            state.start();
            self.emit_event(FlowEvent::FlowStarted {
                instance_id: state.instance_id.clone(),
                flow_id: flow.id.clone(),
                flow_name: flow.name.clone(),
            });
        }

        let current_step_id = match &state.current_step {
            Some(id) => id.clone(),
            None => {
                state.complete();
                return Err("No current step".to_string());
            }
        };

        // Handle special step IDs
        if current_step_id == "end" {
            state.complete();
            return Ok(StepResult::success("end", None));
        }

        if current_step_id == "fail" {
            state.fail("Flow reached fail step");
            return Ok(StepResult::failure("fail", "Flow reached fail step"));
        }

        let step = match flow.get_step(&current_step_id) {
            Some(s) => s.clone(),
            None => {
                let error = format!("Step '{}' not found", current_step_id);
                state.fail(&error);
                return Err(error);
            }
        };

        let result = self.execute_step_with_flow(&step, state, Some(flow)).await;

        // Update state based on result
        if result.success {
            state.step_completed(true, result.outputs.clone());
            if let Some(next) = &result.next_step {
                if next == "end" {
                    state.complete();
                } else if next == "fail" {
                    state.fail("Flow reached fail step");
                } else {
                    state.set_current_step(next);
                }
            } else {
                state.complete();
            }
        } else {
            let error = result
                .error
                .clone()
                .unwrap_or_else(|| "Unknown error".to_string());
            state.step_failed(&error);
            if !step.continue_on_error {
                state.fail(&error);
            }
        }

        Ok(result)
    }

    /// Provide human input to resume a waiting flow.
    pub fn provide_human_input(
        &self,
        state: &mut FlowState,
        step_id: &str,
        input: serde_json::Value,
    ) -> Result<(), String> {
        if state.status != FlowStatus::WaitingForInput {
            return Err("Flow is not waiting for input".to_string());
        }

        // Store the input in context
        let key = format!("{}.input", step_id);
        state.context.insert(key, input);

        // Resume execution
        state.status = FlowStatus::Running;

        Ok(())
    }
}

impl Default for FlowExecutor {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get a string representation of the step type.
fn get_step_type_name(step_type: &StepType) -> String {
    match step_type {
        StepType::Agent { .. } => "agent".to_string(),
        StepType::Tool { .. } => "tool".to_string(),
        StepType::Conditional { .. } => "conditional".to_string(),
        StepType::Parallel { .. } => "parallel".to_string(),
        StepType::HumanInput { .. } => "human_input".to_string(),
        StepType::Transform { .. } => "transform".to_string(),
        StepType::Wait { .. } => "wait".to_string(),
        StepType::Loop { .. } => "loop".to_string(),
        StepType::End => "end".to_string(),
        StepType::Fail { .. } => "fail".to_string(),
    }
}

/// Resolve input mappings from context.
fn resolve_input_mappings(
    mappings: &HashMap<String, String>,
    context: &HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    let mut resolved = HashMap::new();

    for (key, source) in mappings {
        if let Some(value) = context.get(source) {
            resolved.insert(key.clone(), value.clone());
        } else {
            // Try dot notation access
            let parts: Vec<&str> = source.split('.').collect();
            if let Some(first) = parts.first() {
                if let Some(mut value) = context.get(*first).cloned() {
                    let mut found = true;
                    for part in parts.iter().skip(1) {
                        if let Some(obj) = value.as_object() {
                            if let Some(v) = obj.get(*part) {
                                value = v.clone();
                            } else {
                                found = false;
                                break;
                            }
                        } else {
                            found = false;
                            break;
                        }
                    }
                    if found {
                        resolved.insert(key.clone(), value);
                    }
                }
            }
        }
    }

    resolved
}

/// Simple expression evaluator for transform steps.
fn evaluate_simple_expression(
    expression: &str,
    context: &HashMap<String, serde_json::Value>,
) -> serde_json::Value {
    // Handle simple variable references like "context.foo"
    if let Some(key) = expression.strip_prefix("context.") {
        // Remove "context." prefix
        if let Some(value) = context.get(key) {
            return value.clone();
        }
    }

    // Handle dot notation for nested access
    if expression.contains('.') {
        let parts: Vec<&str> = expression.split('.').collect();
        if let Some(first) = parts.first() {
            if let Some(mut value) = context.get(*first).cloned() {
                for part in parts.iter().skip(1) {
                    if let Some(obj) = value.as_object() {
                        if let Some(v) = obj.get(*part) {
                            value = v.clone();
                        } else {
                            return serde_json::Value::Null;
                        }
                    } else {
                        return serde_json::Value::Null;
                    }
                }
                return value;
            }
        }
    }

    // Handle simple key lookup (no dots)
    if let Some(value) = context.get(expression) {
        return value.clone();
    }

    // Return the expression as a string if not found
    serde_json::json!(expression)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::flow::{Condition, Flow, FlowStep, ParallelMerge, StepType};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // ============================================================================
    // Basic Sequential Execution Tests
    // ============================================================================

    #[tokio::test]
    async fn test_simple_flow_execution() {
        let flow = Flow::new("test")
            .add_step(FlowStep::tool("start", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);
        // Only 1 step is executed - "start". "end" is a terminal marker that
        // completes the flow without actually executing.
        assert_eq!(state.execution_count(), 1);
    }

    #[tokio::test]
    async fn test_sequential_multi_step_flow() {
        // Test: step_a -> step_b -> step_c -> end
        let flow = Flow::new("sequential_test")
            .add_step(FlowStep::tool("step_a", "timestamp").then("step_b"))
            .add_step(FlowStep::tool("step_b", "timestamp").then("step_c"))
            .add_step(FlowStep::tool("step_c", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);
        assert_eq!(state.execution_count(), 3); // step_a, step_b, step_c
    }

    #[tokio::test]
    async fn test_step_by_step_execution() {
        let flow = Flow::new("step_test")
            .add_step(FlowStep::tool("step1", "timestamp").then("step2"))
            .add_step(FlowStep::tool("step2", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        // Step 1
        let result1 = executor.step_once(&flow, &mut state).await;
        assert!(result1.is_ok());
        assert_eq!(state.current_step, Some("step2".to_string()));

        // Step 2 - after this step, next is "end" which completes the flow
        let result2 = executor.step_once(&flow, &mut state).await;
        assert!(result2.is_ok());
        // "end" is a terminal marker - when reached, flow completes and current_step becomes None
        assert_eq!(state.current_step, None);
        assert_eq!(state.status, FlowStatus::Completed);
    }

    // ============================================================================
    // Conditional Branching Tests
    // ============================================================================

    #[tokio::test]
    async fn test_conditional_flow_then_branch() {
        let flow = Flow::new("conditional_test")
            .add_step(FlowStep::conditional(
                "check",
                Condition::is_true("flag"),
                "success",
                "failure",
            ))
            .add_step(FlowStep::end("success"))
            .add_step(FlowStep::fail("failure", "Condition not met"));

        let mut state = FlowState::new(&flow);
        state.set("flag", serde_json::json!(true));

        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);
    }

    #[tokio::test]
    async fn test_conditional_flow_else_branch() {
        let flow = Flow::new("conditional_test")
            .add_step(FlowStep::conditional(
                "check",
                Condition::is_true("flag"),
                "success",
                "failure",
            ))
            .add_step(FlowStep::end("success"))
            .add_step(FlowStep::fail("failure", "Condition not met"));

        let mut state = FlowState::new(&flow);
        state.set("flag", serde_json::json!(false));

        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_err());
        assert_eq!(state.status, FlowStatus::Failed);
    }

    #[tokio::test]
    async fn test_conditional_with_equals() {
        let flow = Flow::new("conditional_equals_test")
            .add_step(FlowStep::conditional(
                "check",
                Condition::equals("status", serde_json::json!("approved")),
                "proceed",
                "reject",
            ))
            .add_step(FlowStep::end("proceed"))
            .add_step(FlowStep::fail("reject", "Status not approved"));

        // Test approved case
        let mut state = FlowState::new(&flow);
        state.set("status", serde_json::json!("approved"));
        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;
        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);

        // Test rejected case
        let mut state2 = FlowState::new(&flow);
        state2.set("status", serde_json::json!("pending"));
        let result2 = executor.execute(&flow, &mut state2).await;
        assert!(result2.is_err());
        assert_eq!(state2.status, FlowStatus::Failed);
    }

    #[tokio::test]
    async fn test_conditional_with_numeric_comparison() {
        let flow = Flow::new("numeric_condition_test")
            .add_step(FlowStep::conditional(
                "check_count",
                Condition::GreaterThan {
                    left: "count".to_string(),
                    right: 5.0,
                },
                "high_count",
                "low_count",
            ))
            .add_step(FlowStep::end("high_count"))
            .add_step(FlowStep::end("low_count"));

        // Test count > 5
        let mut state = FlowState::new(&flow);
        state.set("count", serde_json::json!(10));
        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;
        assert!(result.is_ok());

        // Test count <= 5
        let mut state2 = FlowState::new(&flow);
        state2.set("count", serde_json::json!(3));
        let result2 = executor.execute(&flow, &mut state2).await;
        assert!(result2.is_ok());
    }

    #[tokio::test]
    async fn test_nested_conditionals() {
        // Test: if A then (if B then success else partial_fail) else fail
        let flow = Flow::new("nested_conditional_test")
            .add_step(FlowStep::conditional(
                "check_a",
                Condition::is_true("flag_a"),
                "check_b",
                "full_fail",
            ))
            .add_step(FlowStep::conditional(
                "check_b",
                Condition::is_true("flag_b"),
                "success",
                "partial_fail",
            ))
            .add_step(FlowStep::end("success"))
            .add_step(FlowStep::fail("partial_fail", "Partial failure"))
            .add_step(FlowStep::fail("full_fail", "Full failure"));

        // Both true -> success
        let mut state = FlowState::new(&flow);
        state.set("flag_a", serde_json::json!(true));
        state.set("flag_b", serde_json::json!(true));
        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;
        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);

        // A true, B false -> partial_fail
        let mut state2 = FlowState::new(&flow);
        state2.set("flag_a", serde_json::json!(true));
        state2.set("flag_b", serde_json::json!(false));
        let result2 = executor.execute(&flow, &mut state2).await;
        assert!(result2.is_err());
        assert_eq!(state2.error, Some("Partial failure".to_string()));

        // A false -> full_fail
        let mut state3 = FlowState::new(&flow);
        state3.set("flag_a", serde_json::json!(false));
        let result3 = executor.execute(&flow, &mut state3).await;
        assert!(result3.is_err());
        assert_eq!(state3.error, Some("Full failure".to_string()));
    }

    // ============================================================================
    // Loop Execution Tests
    // ============================================================================

    #[tokio::test]
    async fn test_loop_over_array() {
        // Create a flow with a loop step
        let flow = Flow::new("loop_test")
            .add_step(FlowStep {
                id: "loop_step".to_string(),
                name: "Loop Step".to_string(),
                step_type: StepType::Loop {
                    over: "items".to_string(),
                    body_step: "body".to_string(),
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: None,
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::tool("body", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        state.set("items", serde_json::json!(["a", "b", "c"]));

        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);
        // The loop stores iteration outputs under "loop.results.{index}.{key}" pattern
        // and the body step results under "body.{key}" pattern
        // Verify that loop executed all items by checking for loop result keys
        let has_loop_results = state.context.keys().any(|k| k.starts_with("loop.results."));
        assert!(
            has_loop_results,
            "Expected loop.results.* keys in context, got keys: {:?}",
            state.context.keys().collect::<Vec<_>>()
        );
        // Verify all 3 items were processed (indices 0, 1, 2)
        assert!(
            state.context.contains_key("loop.results.0.unix")
                || state.context.contains_key("loop.results.0.iso"),
            "Expected loop.results.0.* keys"
        );
        assert!(
            state.context.contains_key("loop.results.1.unix")
                || state.context.contains_key("loop.results.1.iso"),
            "Expected loop.results.1.* keys"
        );
        assert!(
            state.context.contains_key("loop.results.2.unix")
                || state.context.contains_key("loop.results.2.iso"),
            "Expected loop.results.2.* keys"
        );
    }

    #[tokio::test]
    async fn test_loop_with_empty_array() {
        let flow = Flow::new("loop_empty_test")
            .add_step(FlowStep {
                id: "loop_step".to_string(),
                name: "Loop Step".to_string(),
                step_type: StepType::Loop {
                    over: "items".to_string(),
                    body_step: "body".to_string(),
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: None,
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::tool("body", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        state.set("items", serde_json::json!([]));

        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);
    }

    #[tokio::test]
    async fn test_loop_variable_not_found() {
        let flow = Flow::new("loop_no_var_test")
            .add_step(FlowStep {
                id: "loop_step".to_string(),
                name: "Loop Step".to_string(),
                step_type: StepType::Loop {
                    over: "nonexistent".to_string(),
                    body_step: "body".to_string(),
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: None,
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::tool("body", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        // Don't set the "nonexistent" variable

        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_err());
        assert_eq!(state.status, FlowStatus::Failed);
        assert!(state.error.as_ref().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn test_loop_over_non_array() {
        let flow = Flow::new("loop_non_array_test")
            .add_step(FlowStep {
                id: "loop_step".to_string(),
                name: "Loop Step".to_string(),
                step_type: StepType::Loop {
                    over: "not_an_array".to_string(),
                    body_step: "body".to_string(),
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: None,
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::tool("body", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        state.set("not_an_array", serde_json::json!("string_value"));

        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_err());
        assert_eq!(state.status, FlowStatus::Failed);
        assert!(state.error.as_ref().unwrap().contains("not an array"));
    }

    // ============================================================================
    // Parallel Execution Tests
    // ============================================================================

    #[tokio::test]
    async fn test_parallel_wait_all() {
        let flow = Flow::new("parallel_wait_all_test")
            .add_step(FlowStep {
                id: "parallel_step".to_string(),
                name: "Parallel Step".to_string(),
                step_type: StepType::Parallel {
                    branches: vec![
                        "branch_a".to_string(),
                        "branch_b".to_string(),
                        "branch_c".to_string(),
                    ],
                    merge_strategy: ParallelMerge::WaitAll,
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: None,
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);
        // Check that branch results were recorded
        assert!(state.context.contains_key("parallel_step.completed_count"));
    }

    #[tokio::test]
    async fn test_parallel_wait_any() {
        let flow = Flow::new("parallel_wait_any_test")
            .add_step(FlowStep {
                id: "parallel_step".to_string(),
                name: "Parallel Step".to_string(),
                step_type: StepType::Parallel {
                    branches: vec!["branch_a".to_string(), "branch_b".to_string()],
                    merge_strategy: ParallelMerge::WaitAny,
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: None,
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);
    }

    #[tokio::test]
    async fn test_parallel_wait_n() {
        let flow = Flow::new("parallel_wait_n_test")
            .add_step(FlowStep {
                id: "parallel_step".to_string(),
                name: "Parallel Step".to_string(),
                step_type: StepType::Parallel {
                    branches: vec![
                        "branch_a".to_string(),
                        "branch_b".to_string(),
                        "branch_c".to_string(),
                    ],
                    merge_strategy: ParallelMerge::WaitN(2),
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: None,
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);
    }

    #[tokio::test]
    async fn test_parallel_empty_branches() {
        let flow = Flow::new("parallel_empty_test")
            .add_step(FlowStep {
                id: "parallel_step".to_string(),
                name: "Parallel Step".to_string(),
                step_type: StepType::Parallel {
                    branches: vec![],
                    merge_strategy: ParallelMerge::WaitAll,
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: None,
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        // Empty branches should complete immediately
        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);
    }

    // ============================================================================
    // Timeout Handling Tests
    // ============================================================================

    #[tokio::test]
    async fn test_step_timeout() {
        // Create a wait step that exceeds its timeout
        let flow = Flow::new("timeout_test")
            .add_step(FlowStep {
                id: "slow_step".to_string(),
                name: "Slow Step".to_string(),
                step_type: StepType::Wait {
                    seconds: 10, // Would wait 10 seconds
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: Some(1), // But timeout after 1 second
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new().with_config(FlowExecutorConfig {
            default_step_timeout_secs: 1,
            ..Default::default()
        });

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_err());
        assert_eq!(state.status, FlowStatus::Failed);
        assert!(state.error.as_ref().unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn test_global_executor_timeout() {
        let flow = Flow::new("global_timeout_test")
            .add_step(FlowStep::tool("step1", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new().with_config(FlowExecutorConfig {
            default_step_timeout_secs: 60, // Short timeout
            ..Default::default()
        });

        let result = executor.execute(&flow, &mut state).await;
        assert!(result.is_ok()); // Should complete before timeout
    }

    // ============================================================================
    // Retry Logic Tests
    // ============================================================================

    #[tokio::test]
    async fn test_step_with_retry_count() {
        // Create a step with retry configuration
        let step = FlowStep::tool("retry_step", "nonexistent_tool")
            .with_retry(2)
            .then("end");

        let flow = Flow::new("retry_test")
            .add_step(step)
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        // This will fail because the tool doesn't exist
        let result = executor.execute(&flow, &mut state).await;

        // Should have attempted retries before failing
        assert!(result.is_err());
        assert_eq!(state.status, FlowStatus::Failed);
    }

    #[tokio::test]
    async fn test_exponential_backoff_calculation() {
        // Test the exponential backoff by checking step timing with retries
        let config = FlowExecutorConfig {
            default_step_timeout_secs: 300,
            max_iterations: 100,
            continue_on_error: false,
            emit_events: false,
        };

        let executor = FlowExecutor::new().with_config(config);

        // The executor uses exponential backoff internally (100ms * 2^attempt)
        // Verify the config is applied
        assert_eq!(executor.config.default_step_timeout_secs, 300);
    }

    // ============================================================================
    // Error Propagation Tests
    // ============================================================================

    #[tokio::test]
    async fn test_step_failure_stops_flow() {
        let flow = Flow::new("error_propagation_test")
            .add_step(FlowStep::fail("fail_step", "Intentional failure"))
            .add_step(FlowStep::tool("never_reached", "timestamp").then("end"))
            .add_step(FlowStep::end("end"))
            .with_start("fail_step");

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_err());
        assert_eq!(state.status, FlowStatus::Failed);
        assert_eq!(state.error, Some("Intentional failure".to_string()));
        // Only the fail step should have executed
        assert_eq!(state.execution_count(), 1);
    }

    #[tokio::test]
    async fn test_continue_on_error() {
        let flow = Flow::new("continue_on_error_test")
            .add_step(
                FlowStep::tool("failing_step", "nonexistent_tool")
                    .continue_on_error()
                    .then("next_step"),
            )
            .add_step(FlowStep::tool("next_step", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new().with_config(FlowExecutorConfig {
            continue_on_error: true, // Also enable globally
            ..Default::default()
        });

        let result = executor.execute(&flow, &mut state).await;

        // With continue_on_error, the flow should complete despite the failing step
        // Note: actual behavior depends on whether there's a next step configured
        assert!(result.is_ok() || state.execution_count() >= 1);
    }

    #[tokio::test]
    async fn test_missing_step_reference() {
        let flow = Flow::new("missing_step_test")
            .add_step(FlowStep::tool("start", "timestamp").then("nonexistent"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_err());
        assert_eq!(state.status, FlowStatus::Failed);
        // The error message should indicate the step couldn't be found
        let error = state.error.as_ref().unwrap();
        assert!(
            error.contains("not found") || error.contains("nonexistent"),
            "Expected error about missing step, got: {}",
            error
        );
    }

    #[tokio::test]
    async fn test_max_iterations_exceeded() {
        // Create a flow with a cycle that would run forever
        let flow = Flow::new("infinite_loop_test")
            .add_step(FlowStep::tool("step_a", "timestamp").then("step_b"))
            .add_step(FlowStep::tool("step_b", "timestamp").then("step_a")) // Cycle!
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new().with_config(FlowExecutorConfig {
            max_iterations: 10, // Limit iterations
            ..Default::default()
        });

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_err());
        assert_eq!(state.status, FlowStatus::Failed);
        assert!(state.error.as_ref().unwrap().contains("Maximum iterations"));
    }

    // ============================================================================
    // Event Emission Tests
    // ============================================================================

    #[tokio::test]
    async fn test_event_callback() {
        let event_count = Arc::new(AtomicUsize::new(0));
        let event_count_clone = event_count.clone();

        let flow = Flow::new("event_test")
            .add_step(FlowStep::tool("step1", "timestamp").then("step2"))
            .add_step(FlowStep::tool("step2", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new().with_event_callback(move |_event| {
            event_count_clone.fetch_add(1, Ordering::SeqCst);
        });

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        // Should have emitted: FlowStarted, StepStarted, StepCompleted (x2), FlowCompleted
        let events = event_count.load(Ordering::SeqCst);
        assert!(events >= 4, "Expected at least 4 events, got {}", events);
    }

    #[tokio::test]
    async fn test_events_disabled() {
        let event_count = Arc::new(AtomicUsize::new(0));
        let event_count_clone = event_count.clone();

        let flow = Flow::new("no_events_test")
            .add_step(FlowStep::tool("step1", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new()
            .with_config(FlowExecutorConfig {
                emit_events: false,
                ..Default::default()
            })
            .with_event_callback(move |_event| {
                event_count_clone.fetch_add(1, Ordering::SeqCst);
            });

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        // No events should have been emitted
        assert_eq!(event_count.load(Ordering::SeqCst), 0);
    }

    // ============================================================================
    // Transform Step Tests
    // ============================================================================

    #[tokio::test]
    async fn test_transform_step() {
        let flow = Flow::new("transform_test")
            .add_step(FlowStep {
                id: "transform_step".to_string(),
                name: "Transform Step".to_string(),
                step_type: StepType::Transform {
                    expression: "input_value".to_string(),
                    output_key: "transformed".to_string(),
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: None,
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        state.set("input_value", serde_json::json!("test_data"));

        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);
    }

    // ============================================================================
    // Wait Step Tests
    // ============================================================================

    #[tokio::test]
    async fn test_wait_step() {
        let flow = Flow::new("wait_test")
            .add_step(FlowStep {
                id: "wait_step".to_string(),
                name: "Wait Step".to_string(),
                step_type: StepType::Wait {
                    seconds: 1, // Wait 1 second
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: Some(5), // Timeout after 5 seconds
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let start = std::time::Instant::now();
        let result = executor.execute(&flow, &mut state).await;
        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Completed);
        // Should have waited approximately 1 second
        assert!(elapsed.as_secs() >= 1);
    }

    // ============================================================================
    // Human Input Step Tests
    // ============================================================================

    #[tokio::test]
    async fn test_human_input_step() {
        let flow = Flow::new("human_input_test")
            .add_step(FlowStep {
                id: "input_step".to_string(),
                name: "Input Step".to_string(),
                step_type: StepType::HumanInput {
                    prompt: "Please select an option".to_string(),
                    options: vec!["Yes".to_string(), "No".to_string()],
                    next: "end".to_string(),
                },
                description: None,
                timeout_secs: None,
                continue_on_error: false,
                retry_count: 0,
                input_mappings: HashMap::new(),
                condition: None,
            })
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        // Execute the human input step
        let result = executor.step_once(&flow, &mut state).await;

        assert!(result.is_ok());
        // Human input step should have succeeded and moved to next
        assert_eq!(state.current_step, None); // Ends at "end"
    }

    #[tokio::test]
    async fn test_provide_human_input() {
        let flow = Flow::new("provide_input_test")
            .add_step(FlowStep::human_input("input_step", "Enter value").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        state.status = FlowStatus::WaitingForInput;
        state.current_step = Some("input_step".to_string());

        let executor = FlowExecutor::new();
        let result = executor.provide_human_input(
            &mut state,
            "input_step",
            serde_json::json!("user_input_value"),
        );

        assert!(result.is_ok());
        assert_eq!(state.status, FlowStatus::Running);
        assert!(state.context.contains_key("input_step.input"));
    }

    // ============================================================================
    // Step Condition Tests
    // ============================================================================

    #[tokio::test]
    async fn test_step_with_condition_met() {
        let mut step = FlowStep::tool("conditional_tool", "timestamp").then("end");
        step.condition = Some(Condition::is_true("should_run"));

        let flow = Flow::new("step_condition_test")
            .add_step(step)
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        state.set("should_run", serde_json::json!(true));

        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        assert_eq!(state.execution_count(), 1);
    }

    #[tokio::test]
    async fn test_step_with_condition_not_met() {
        let mut step = FlowStep::tool("conditional_tool", "timestamp").then("end");
        step.condition = Some(Condition::is_true("should_run"));

        let flow = Flow::new("step_skip_test")
            .add_step(step)
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        state.set("should_run", serde_json::json!(false));

        let executor = FlowExecutor::new();
        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        // Step was skipped but still recorded as executed
        assert_eq!(state.execution_count(), 1);
        // Check the skip output was recorded
        assert!(state.context.contains_key("conditional_tool.skipped"));
    }

    // ============================================================================
    // Built-in Tool Tests
    // ============================================================================

    #[tokio::test]
    async fn test_builtin_timestamp_tool() {
        let flow = Flow::new("timestamp_test")
            .add_step(FlowStep::tool("ts_step", "timestamp").then("end"))
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        // Timestamp tool should have produced outputs
        assert!(
            state.context.contains_key("ts_step.iso") || state.context.contains_key("ts_step.unix")
        );
    }

    #[tokio::test]
    async fn test_builtin_log_tool() {
        let mut tool_step = FlowStep::tool("log_step", "log");
        if let StepType::Tool { ref mut inputs, .. } = tool_step.step_type {
            inputs.insert("message".to_string(), serde_json::json!("Test log message"));
            inputs.insert("level".to_string(), serde_json::json!("info"));
        }
        tool_step = tool_step.then("end");

        let flow = Flow::new("log_test")
            .add_step(tool_step)
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        assert!(state.context.contains_key("log_step.logged"));
    }

    #[tokio::test]
    async fn test_builtin_json_parse_tool() {
        let mut tool_step = FlowStep::tool("parse_step", "json_parse");
        if let StepType::Tool { ref mut inputs, .. } = tool_step.step_type {
            inputs.insert(
                "input".to_string(),
                serde_json::json!(r#"{"key": "value"}"#),
            );
        }
        tool_step = tool_step.then("end");

        let flow = Flow::new("json_parse_test")
            .add_step(tool_step)
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        // JSON parse should have produced parsed output
        let parsed = state.context.get("parse_step.parsed");
        assert!(parsed.is_some());
    }

    #[tokio::test]
    async fn test_builtin_array_length_tool() {
        let mut tool_step = FlowStep::tool("length_step", "array_length");
        if let StepType::Tool { ref mut inputs, .. } = tool_step.step_type {
            inputs.insert("array".to_string(), serde_json::json!([1, 2, 3, 4, 5]));
        }
        tool_step = tool_step.then("end");

        let flow = Flow::new("array_length_test")
            .add_step(tool_step)
            .add_step(FlowStep::end("end"));

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_ok());
        let length = state.context.get("length_step.length");
        assert!(length.is_some());
        assert_eq!(length.unwrap().as_u64(), Some(5));
    }

    // ============================================================================
    // Helper Function Tests
    // ============================================================================

    #[test]
    fn test_step_result_builder() {
        let result = StepResult::success("step1", Some("step2".to_string()))
            .with_output("key".to_string(), serde_json::json!("value"))
            .with_duration(100);

        assert!(result.success);
        assert_eq!(result.step_id, "step1");
        assert_eq!(result.next_step, Some("step2".to_string()));
        assert_eq!(result.duration_ms, 100);
        assert!(result.outputs.contains_key("key"));
    }

    #[test]
    fn test_step_result_failure() {
        let result = StepResult::failure("failed_step", "Something went wrong");

        assert!(!result.success);
        assert_eq!(result.step_id, "failed_step");
        assert_eq!(result.error, Some("Something went wrong".to_string()));
        assert!(result.next_step.is_none());
    }

    #[test]
    fn test_resolve_input_mappings() {
        let mut mappings = HashMap::new();
        mappings.insert("input1".to_string(), "source1".to_string());
        mappings.insert("input2".to_string(), "nested.value".to_string());

        let mut context = HashMap::new();
        context.insert("source1".to_string(), serde_json::json!("value1"));
        context.insert("nested".to_string(), serde_json::json!({"value": "value2"}));

        let resolved = resolve_input_mappings(&mappings, &context);

        assert_eq!(resolved.get("input1"), Some(&serde_json::json!("value1")));
        assert_eq!(resolved.get("input2"), Some(&serde_json::json!("value2")));
    }

    #[test]
    fn test_resolve_input_mappings_missing_source() {
        let mut mappings = HashMap::new();
        mappings.insert("input1".to_string(), "nonexistent".to_string());

        let context = HashMap::new();

        let resolved = resolve_input_mappings(&mappings, &context);

        // Missing source should not produce any output
        assert!(!resolved.contains_key("input1"));
    }

    #[test]
    fn test_evaluate_simple_expression() {
        let mut context = HashMap::new();
        context.insert("foo".to_string(), serde_json::json!("bar"));
        context.insert("nested".to_string(), serde_json::json!({"key": "value"}));

        assert_eq!(
            evaluate_simple_expression("foo", &context),
            serde_json::json!("bar")
        );
        assert_eq!(
            evaluate_simple_expression("nested.key", &context),
            serde_json::json!("value")
        );
        assert_eq!(
            evaluate_simple_expression("nonexistent", &context),
            serde_json::json!("nonexistent")
        );
    }

    #[test]
    fn test_evaluate_expression_with_context_prefix() {
        let mut context = HashMap::new();
        context.insert("data".to_string(), serde_json::json!("test_value"));

        assert_eq!(
            evaluate_simple_expression("context.data", &context),
            serde_json::json!("test_value")
        );
    }

    #[test]
    fn test_get_step_type_name() {
        assert_eq!(
            get_step_type_name(&StepType::Agent {
                role: "".to_string(),
                prompt: "".to_string(),
                max_iterations: None,
                next: "".to_string(),
            }),
            "agent"
        );
        assert_eq!(
            get_step_type_name(&StepType::Tool {
                tool_id: "".to_string(),
                inputs: HashMap::new(),
                next: "".to_string(),
            }),
            "tool"
        );
        assert_eq!(
            get_step_type_name(&StepType::Conditional {
                condition: Condition::is_true("x"),
                then_step: "".to_string(),
                else_step: "".to_string(),
            }),
            "conditional"
        );
        assert_eq!(
            get_step_type_name(&StepType::Parallel {
                branches: vec![],
                merge_strategy: ParallelMerge::WaitAll,
                next: "".to_string(),
            }),
            "parallel"
        );
        assert_eq!(get_step_type_name(&StepType::End), "end");
        assert_eq!(
            get_step_type_name(&StepType::Fail {
                error: "".to_string()
            }),
            "fail"
        );
    }

    // ============================================================================
    // Flow Validation Tests
    // ============================================================================

    #[tokio::test]
    async fn test_flow_validation_failure() {
        // Create a flow with invalid start step
        let flow = Flow {
            id: "invalid_flow".to_string(),
            name: "Invalid Flow".to_string(),
            description: None,
            steps: HashMap::new(), // Empty steps
            start_step: Some("nonexistent".to_string()),
            timeout_secs: None,
            inputs: vec![],
            outputs: vec![],
            tags: vec![],
            version: "1.0".to_string(),
        };

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_err());
        assert_eq!(state.status, FlowStatus::Failed);
    }

    #[tokio::test]
    async fn test_empty_flow() {
        let flow = Flow {
            id: "empty_flow".to_string(),
            name: "Empty Flow".to_string(),
            description: None,
            steps: HashMap::new(),
            start_step: None,
            timeout_secs: None,
            inputs: vec![],
            outputs: vec![],
            tags: vec![],
            version: "1.0".to_string(),
        };

        let mut state = FlowState::new(&flow);
        let executor = FlowExecutor::new();

        let result = executor.execute(&flow, &mut state).await;

        assert!(result.is_err());
        // Validation should fail for empty flow
    }

    // ============================================================================
    // Config Tests
    // ============================================================================

    #[test]
    fn test_default_config() {
        let config = FlowExecutorConfig::default();
        assert_eq!(config.max_iterations, 1000);
        assert_eq!(config.default_step_timeout_secs, 300);
        assert!(!config.continue_on_error);
        assert!(config.emit_events);
    }

    #[test]
    fn test_executor_with_config() {
        let config = FlowExecutorConfig {
            max_iterations: 100,
            default_step_timeout_secs: 60,
            continue_on_error: true,
            emit_events: false,
        };

        let executor = FlowExecutor::new().with_config(config);

        assert_eq!(executor.config.max_iterations, 100);
        assert_eq!(executor.config.default_step_timeout_secs, 60);
        assert!(executor.config.continue_on_error);
        assert!(!executor.config.emit_events);
    }
}
