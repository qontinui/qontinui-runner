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

use super::flow::{Flow, FlowState, FlowStatus, FlowStep, ParallelMerge, StepType};
use crate::ai_provider;
use crate::ai_router::TaskContext;
use crate::mcp_embedded::EmbeddedMcp;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl FlowExecutor {
    /// Create a new flow executor with default configuration.
    pub fn new() -> Self {
        Self {
            config: FlowExecutorConfig::default(),
            event_callback: None,
        }
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

            // Execute the step
            let result = self.execute_step(&step, state).await;

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
            Err(state.error.clone().unwrap_or_else(|| "Flow failed".to_string()))
        } else {
            Ok(())
        }
    }

    /// Execute a single step.
    pub async fn execute_step(&self, step: &FlowStep, state: &mut FlowState) -> StepResult {
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

        // Execute based on step type
        let result = match &step.step_type {
            StepType::Agent {
                role,
                prompt,
                next,
                ..
            } => {
                self.execute_agent_step(&step.id, role, prompt, next, &state.context).await
            }

            StepType::Tool {
                tool_id,
                inputs,
                next,
            } => {
                // Merge static inputs with resolved inputs
                let mut merged_inputs = inputs.clone();
                for (k, v) in resolved_inputs {
                    merged_inputs.insert(k, v);
                }
                self.execute_tool_step(&step.id, tool_id, &merged_inputs, next, &state.context).await
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

                StepResult::success(&step.id, Some(next_step))
                    .with_output("condition_result".to_string(), serde_json::json!(condition_met))
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
                self.execute_transform_step(&step.id, expression, output_key, next, &state.context).await
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
                self.execute_loop_step(&step.id, over, body_step, next, state).await
            }

            StepType::End => StepResult::success(&step.id, None),

            StepType::Fail { error } => StepResult::failure(&step.id, error),
        };

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

        // Build the full prompt with role context
        let full_prompt = self.build_agent_prompt(role, prompt, context);

        // Build task context for AI routing (helps select appropriate model)
        let task_context = TaskContext::from_prompt(&full_prompt);

        // Get the timeout for this step
        let timeout_secs = self.config.default_step_timeout_secs;

        // Call AI provider in a blocking task (ai_provider is synchronous)
        let prompt_clone = full_prompt.clone();
        let ai_result = tokio::task::spawn_blocking(move || {
            ai_provider::run_prompt_with_routing(&prompt_clone, &task_context, timeout_secs)
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
                    result = result.with_output(
                        "response".to_string(),
                        serde_json::json!(response.output),
                    );
                    result = result.with_output("role".to_string(), serde_json::json!(role));
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
                    other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
                };
                full_prompt.push_str(&format!("- **{}**: {}\n", key, value_str));
            }
            full_prompt.push_str("\n");
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
    /// for common operations.
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

        // Convert inputs HashMap to serde_json::Value for MCP call
        let args = serde_json::json!(inputs);

        // Check if this is a built-in tool first
        if let Some(result) = self.try_execute_builtin_tool(step_id, tool_id, inputs, context).await {
            return result.with_next(next);
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
    /// Built-in tools are simple operations that don't require external services.
    async fn try_execute_builtin_tool(
        &self,
        step_id: &str,
        tool_id: &str,
        inputs: &HashMap<String, serde_json::Value>,
        context: &HashMap<String, serde_json::Value>,
    ) -> Option<StepResult> {
        match tool_id {
            // JSON manipulation tools
            "json_parse" => {
                let input = inputs.get("input")?.as_str()?;
                match serde_json::from_str::<serde_json::Value>(input) {
                    Ok(parsed) => Some(
                        StepResult::success(step_id, None)
                            .with_output("parsed".to_string(), parsed),
                    ),
                    Err(e) => Some(StepResult::failure(step_id, format!("JSON parse error: {}", e))),
                }
            }
            "json_stringify" => {
                let input = inputs.get("input")?;
                let pretty = inputs
                    .get("pretty")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let output = if pretty {
                    serde_json::to_string_pretty(input)
                } else {
                    serde_json::to_string(input)
                };
                match output {
                    Ok(s) => Some(
                        StepResult::success(step_id, None)
                            .with_output("output".to_string(), serde_json::json!(s)),
                    ),
                    Err(e) => Some(StepResult::failure(
                        step_id,
                        format!("JSON stringify error: {}", e),
                    )),
                }
            }

            // Context tools
            "get_context" => {
                let key = inputs.get("key")?.as_str()?;
                let value = context.get(key).cloned().unwrap_or(serde_json::Value::Null);
                Some(
                    StepResult::success(step_id, None)
                        .with_output("value".to_string(), value),
                )
            }
            "merge_context" => {
                let mut merged = serde_json::Map::new();
                for (key, value) in context {
                    merged.insert(key.clone(), value.clone());
                }
                Some(
                    StepResult::success(step_id, None)
                        .with_output("context".to_string(), serde_json::Value::Object(merged)),
                )
            }

            // String tools
            "string_concat" => {
                let parts = inputs.get("parts")?.as_array()?;
                let separator = inputs
                    .get("separator")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let result: Vec<String> = parts
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                Some(
                    StepResult::success(step_id, None)
                        .with_output("output".to_string(), serde_json::json!(result.join(separator))),
                )
            }
            "string_split" => {
                let input = inputs.get("input")?.as_str()?;
                let separator = inputs.get("separator")?.as_str()?;
                let parts: Vec<&str> = input.split(separator).collect();
                Some(
                    StepResult::success(step_id, None)
                        .with_output("parts".to_string(), serde_json::json!(parts)),
                )
            }

            // Array tools
            "array_length" => {
                let array = inputs.get("array")?.as_array()?;
                Some(
                    StepResult::success(step_id, None)
                        .with_output("length".to_string(), serde_json::json!(array.len())),
                )
            }
            "array_map" => {
                // Simple map that extracts a field from each object
                let array = inputs.get("array")?.as_array()?;
                let field = inputs.get("field")?.as_str()?;
                let mapped: Vec<serde_json::Value> = array
                    .iter()
                    .filter_map(|item| item.get(field).cloned())
                    .collect();
                Some(
                    StepResult::success(step_id, None)
                        .with_output("result".to_string(), serde_json::json!(mapped)),
                )
            }
            "array_filter" => {
                // Simple filter that checks if a field equals a value
                let array = inputs.get("array")?.as_array()?;
                let field = inputs.get("field")?.as_str()?;
                let value = inputs.get("value")?;
                let filtered: Vec<serde_json::Value> = array
                    .iter()
                    .filter(|item| item.get(field) == Some(value))
                    .cloned()
                    .collect();
                Some(
                    StepResult::success(step_id, None)
                        .with_output("result".to_string(), serde_json::json!(filtered)),
                )
            }

            // Timestamp tools
            "timestamp" => {
                let now = chrono::Utc::now();
                Some(
                    StepResult::success(step_id, None)
                        .with_output("iso".to_string(), serde_json::json!(now.to_rfc3339()))
                        .with_output("unix".to_string(), serde_json::json!(now.timestamp()))
                        .with_output("unix_millis".to_string(), serde_json::json!(now.timestamp_millis())),
                )
            }

            // Log tool (useful for debugging flows)
            "log" => {
                let message = inputs
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no message)");
                let level = inputs
                    .get("level")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info");

                match level {
                    "debug" => debug!(tool = "log", "{}", message),
                    "info" => info!(tool = "log", "{}", message),
                    "warn" => warn!(tool = "log", "{}", message),
                    "error" => error!(tool = "log", "{}", message),
                    _ => info!(tool = "log", "{}", message),
                }

                Some(
                    StepResult::success(step_id, None)
                        .with_output("logged".to_string(), serde_json::json!(true)),
                )
            }

            // Sleep/delay tool
            "sleep" => {
                let ms = inputs
                    .get("ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1000);
                tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
                Some(
                    StepResult::success(step_id, None)
                        .with_output("slept_ms".to_string(), serde_json::json!(ms)),
                )
            }

            _ => None, // Not a built-in tool
        }
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
            .timeout(std::time::Duration::from_secs(self.config.default_step_timeout_secs))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return StepResult::failure(step_id, format!("Failed to create HTTP client: {}", e));
            }
        };

        // The runner's MCP API endpoint for tool execution
        let url = format!("http://localhost:9876/api/mcp/tools/{}/execute", tool_id);

        match client
            .post(&url)
            .json(inputs)
            .send()
            .await
        {
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
                            result = result.with_output("tool_id".to_string(), serde_json::json!(tool_id));
                            result
                        }
                        Err(e) => {
                            StepResult::failure(
                                step_id,
                                format!("Failed to parse tool response: {}", e),
                            )
                        }
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
                    format!("Tool '{}' execution failed: {}. Is the runner API available?", tool_id, e),
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
    async fn execute_parallel_step(
        &self,
        step_id: &str,
        instance_id: &str,
        branches: &[String],
        merge_strategy: &ParallelMerge,
        next: &str,
    ) -> StepResult {
        let total = branches.len();

        // For now, simulate parallel execution
        // In a real implementation, this would spawn actual parallel tasks
        let mut completed = 0;
        let mut outputs = HashMap::new();

        for branch_id in branches {
            // Simulate branch execution
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            completed += 1;

            self.emit_event(FlowEvent::ParallelProgress {
                instance_id: instance_id.to_string(),
                step_id: step_id.to_string(),
                completed,
                total,
            });

            outputs.insert(
                branch_id.clone(),
                serde_json::json!({
                    "success": true,
                    "simulated": true
                }),
            );

            // Check merge strategy
            match merge_strategy {
                ParallelMerge::WaitAny => {
                    // Exit after first completion
                    break;
                }
                ParallelMerge::WaitN(n) if completed >= *n => {
                    break;
                }
                _ => {}
            }
        }

        StepResult::success(step_id, Some(next.to_string()))
            .with_output("branches".to_string(), serde_json::json!(outputs))
            .with_output("completed_count".to_string(), serde_json::json!(completed))
    }

    /// Execute a loop step.
    async fn execute_loop_step(
        &self,
        step_id: &str,
        over: &str,
        body_step: &str,
        next: &str,
        state: &mut FlowState,
    ) -> StepResult {
        // Get the array to iterate over
        let items = match state.context.get(over) {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => {
                return StepResult::failure(step_id, format!("Loop variable '{}' is not an array", over));
            }
        };

        let mut loop_results = Vec::new();

        for (index, item) in items.iter().enumerate() {
            // Set loop context
            state.context.insert("loop.index".to_string(), serde_json::json!(index));
            state.context.insert("loop.item".to_string(), item.clone());
            state.context.insert("loop.length".to_string(), serde_json::json!(items.len()));

            // Execute body step (this is simplified - in reality we'd need to
            // look up the step and execute it)
            loop_results.push(serde_json::json!({
                "index": index,
                "body_step": body_step,
                "item": item
            }));
        }

        // Clean up loop context
        state.context.remove("loop.index");
        state.context.remove("loop.item");
        state.context.remove("loop.length");

        StepResult::success(step_id, Some(next.to_string()))
            .with_output("loop_results".to_string(), serde_json::json!(loop_results))
            .with_output("iterations".to_string(), serde_json::json!(items.len()))
    }

    /// Advance execution by one step (for step-by-step UI updates).
    pub async fn step_once(&self, flow: &Flow, state: &mut FlowState) -> Result<StepResult, String> {
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

        let result = self.execute_step(&step, state).await;

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
            let error = result.error.clone().unwrap_or_else(|| "Unknown error".to_string());
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
    if expression.starts_with("context.") {
        let key = &expression[8..]; // Remove "context." prefix
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
    use crate::orchestrator::flow::{Condition, Flow, FlowStep};

    #[tokio::test]
    async fn test_simple_flow_execution() {
        let flow = Flow::new("test")
            .add_step(FlowStep::agent("start", "tester", "Test prompt").then("end"))
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
    async fn test_conditional_flow() {
        let flow = Flow::new("conditional_test")
            .add_step(
                FlowStep::conditional(
                    "check",
                    Condition::is_true("flag"),
                    "success",
                    "failure",
                ),
            )
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
            .add_step(
                FlowStep::conditional(
                    "check",
                    Condition::is_true("flag"),
                    "success",
                    "failure",
                ),
            )
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
    async fn test_step_by_step_execution() {
        let flow = Flow::new("step_test")
            .add_step(FlowStep::agent("step1", "role", "prompt").then("step2"))
            .add_step(FlowStep::agent("step2", "role", "prompt").then("end"))
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
    fn test_resolve_input_mappings() {
        let mut mappings = HashMap::new();
        mappings.insert("input1".to_string(), "source1".to_string());
        mappings.insert("input2".to_string(), "nested.value".to_string());

        let mut context = HashMap::new();
        context.insert("source1".to_string(), serde_json::json!("value1"));
        context.insert(
            "nested".to_string(),
            serde_json::json!({"value": "value2"}),
        );

        let resolved = resolve_input_mappings(&mappings, &context);

        assert_eq!(resolved.get("input1"), Some(&serde_json::json!("value1")));
        assert_eq!(resolved.get("input2"), Some(&serde_json::json!("value2")));
    }

    #[test]
    fn test_evaluate_simple_expression() {
        let mut context = HashMap::new();
        context.insert("foo".to_string(), serde_json::json!("bar"));
        context.insert(
            "nested".to_string(),
            serde_json::json!({"key": "value"}),
        );

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
}
