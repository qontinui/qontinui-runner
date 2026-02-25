//! Step Handlers Module
//!
//! This module defines the `StepHandler` trait and registry for executing
//! different step types. Each step type (workflow, playwright, shell_command, etc.)
//! has its own handler implementation.
//!
//! ## Architecture
//!
//! The handler pattern provides polymorphic dispatch for step execution:
//!
//! ```text
//! StepExecutor.execute_single_step()
//!     └── HandlerRegistry.get_handler(step_type)
//!             └── handler.execute(step, context)
//! ```
//!
//! ## Available Handlers (4 active)
//!
//! | Category | Handlers |
//! |----------|----------|
//! | **Command** | command (dispatches to shell_command, check, check_group, test) |
//! | **UI Bridge** | ui_bridge |
//! | **AI** | prompt |
//! | **Artifact** | save_workflow_artifact |
//!
//! ## Adding a New Step Type
//!
//! 1. Create a new file in `handlers/` (e.g., `my_step.rs`)
//! 2. Implement `StepHandler` for your handler struct
//! 3. Add the module declaration and re-export in this file
//! 4. Register the handler in `HandlerRegistry::with_standard_handlers()`
//!
//! ## Example Handler
//!
//! ```ignore
//! pub struct MyStepHandler;
//!
//! #[async_trait]
//! impl StepHandler for MyStepHandler {
//!     fn step_type(&self) -> &'static str {
//!         "my_step"
//!     }
//!
//!     fn display_name(&self) -> &'static str {
//!         "My Step"
//!     }
//!
//!     async fn execute(
//!         &self,
//!         step: &ExecutionStepConfig,
//!         context: &HandlerContext,
//!     ) -> StepHandlerResult {
//!         // implementation
//!     }
//! }
//! ```

use crate::action_service::UnifiedActionService;
use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::orchestrator::context_propagation::{RuntimeContext, SharedVariableStore};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

use super::events::TreeEventEmitter;
use super::executor::ExecutionStepConfig;

// Step handler implementations
// Internal modules used by CommandHandler (not registered directly)
pub(super) mod check;
pub(super) mod check_group;
pub(crate) mod shell_command;
pub(super) mod test;
// Active handlers
mod command;
mod save_workflow_artifact;
pub(crate) mod ui_bridge;

// Re-export handlers for registration
use command::CommandHandler;
use save_workflow_artifact::SaveWorkflowArtifactHandler;
use ui_bridge::UiBridgeHandler;

/// Result of executing a step handler.
///
/// This is the raw result from a handler before it's wrapped into
/// a full `StepExecutionResult` with timing and metadata.
#[derive(Debug, Clone)]
pub struct StepHandlerResult {
    /// Whether the step succeeded
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
    /// Path to screenshot if captured
    pub screenshot_path: Option<String>,
    /// Additional output data (step-type specific)
    pub output_data: Option<serde_json::Value>,
}

impl StepHandlerResult {
    /// Create a successful result.
    pub fn success() -> Self {
        Self {
            success: true,
            error: None,
            screenshot_path: None,
            output_data: None,
        }
    }

    /// Create a successful result with a screenshot path.
    pub fn success_with_screenshot(path: String) -> Self {
        Self {
            success: true,
            error: None,
            screenshot_path: Some(path),
            output_data: None,
        }
    }

    /// Create a successful result with output data.
    pub fn success_with_data(data: serde_json::Value) -> Self {
        Self {
            success: true,
            error: None,
            screenshot_path: None,
            output_data: Some(data),
        }
    }

    /// Create a failed result.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(error.into()),
            screenshot_path: None,
            output_data: None,
        }
    }

    /// Create a failed result with output data (e.g., partial results).
    pub fn failure_with_data(error: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            success: false,
            error: Some(error.into()),
            screenshot_path: None,
            output_data: Some(data),
        }
    }

    /// Add a screenshot path to this result.
    pub fn with_screenshot(mut self, path: String) -> Self {
        self.screenshot_path = Some(path);
        self
    }

    /// Add output data to this result.
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.output_data = Some(data);
        self
    }
}

/// Context passed to step handlers during execution.
///
/// This provides access to shared services and state needed by handlers.
pub struct HandlerContext {
    /// Application state (database, display processor, etc.)
    pub app_state: Arc<AppState>,
    /// Configuration storage for loading saved configs
    pub config_storage: Arc<TokioMutex<ConfigStorage>>,
    /// Unified action service for GUI automation
    pub action_service: UnifiedActionService,
    /// Tree event emitter for action logging
    pub event_emitter: TreeEventEmitter,
    /// Runtime context for variable expansion
    pub runtime_context: RuntimeContext,
    /// Shared variable store for API request chaining
    pub shared_variables: SharedVariableStore,
    /// Optional task run ID for database logging
    pub task_run_id: Option<String>,
}

impl HandlerContext {
    /// Create a new handler context.
    #[allow(clippy::unwrap_or_default)]
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<TokioMutex<ConfigStorage>>,
        app_handle: Option<tauri::AppHandle>,
        task_run_id: Option<String>,
    ) -> Self {
        let action_service = UnifiedActionService::new(app_state.clone(), config_storage.clone());
        let event_emitter = TreeEventEmitter::new(app_state.clone(), app_handle);
        let runtime_context = task_run_id
            .as_ref()
            .map(|id| RuntimeContext::with_task_run_id(id))
            .unwrap_or_else(RuntimeContext::new);

        Self {
            app_state,
            config_storage,
            action_service,
            event_emitter,
            runtime_context,
            shared_variables: SharedVariableStore::new(),
            task_run_id,
        }
    }

    /// Create a handler context with shared state from an existing executor.
    ///
    /// This is used when integrating handlers with the existing StepExecutor,
    /// allowing them to share runtime context and variable stores.
    ///
    /// Note: A new UnifiedActionService instance is created because it's not Clone-able,
    /// but this is fine since it only holds Arc references internally.
    pub fn with_shared_state(
        app_state: Arc<AppState>,
        config_storage: Arc<TokioMutex<ConfigStorage>>,
        app_handle: Option<tauri::AppHandle>,
        runtime_context: RuntimeContext,
        shared_variables: SharedVariableStore,
        task_run_id: Option<String>,
    ) -> Self {
        let action_service = UnifiedActionService::new(app_state.clone(), config_storage.clone());
        let event_emitter = TreeEventEmitter::new(app_state.clone(), app_handle);

        Self {
            app_state,
            config_storage,
            action_service,
            event_emitter,
            runtime_context,
            shared_variables,
            task_run_id,
        }
    }

    /// Set a variable in the runtime context.
    pub fn set_variable(&mut self, name: &str, value: serde_json::Value) {
        self.runtime_context.set_variable(name, value);
    }

    /// Get the runtime context.
    pub fn runtime_context(&self) -> &RuntimeContext {
        &self.runtime_context
    }

    /// Get a mutable reference to the runtime context.
    pub fn runtime_context_mut(&mut self) -> &mut RuntimeContext {
        &mut self.runtime_context
    }
}

/// Trait for step type handlers.
///
/// Each step type (workflow, playwright, shell_command, etc.) implements
/// this trait to provide its execution logic.
#[async_trait]
pub trait StepHandler: Send + Sync {
    /// Get the step type this handler handles (e.g., "workflow", "playwright").
    fn step_type(&self) -> &'static str;

    /// Execute the step and return the result.
    ///
    /// The handler should:
    /// 1. Validate the step configuration
    /// 2. Emit action_started event via context.event_emitter
    /// 3. Perform the actual execution
    /// 4. Emit action_completed or action_failed event
    /// 5. Return the result
    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult;

    /// Check if this handler can handle the given step.
    ///
    /// Default implementation checks if step_type matches.
    fn can_handle(&self, step: &ExecutionStepConfig) -> bool {
        step.step_type == self.step_type()
    }

    /// Get the display name for this step type (for logging).
    fn display_name(&self) -> &'static str {
        self.step_type()
    }
}

/// Registry of step handlers.
///
/// This maintains a mapping from step type strings to handler implementations.
pub struct HandlerRegistry {
    handlers: HashMap<&'static str, Arc<dyn StepHandler>>,
}

impl HandlerRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a handler for a step type.
    pub fn register<H: StepHandler + 'static>(&mut self, handler: H) {
        let step_type = handler.step_type();
        self.handlers.insert(step_type, Arc::new(handler));
    }

    /// Get a handler for a step type.
    pub fn get(&self, step_type: &str) -> Option<Arc<dyn StepHandler>> {
        self.handlers.get(step_type).cloned()
    }

    /// Check if a handler exists for a step type.
    pub fn has_handler(&self, step_type: &str) -> bool {
        self.handlers.contains_key(step_type)
    }

    /// Get all registered step types.
    pub fn step_types(&self) -> Vec<&'static str> {
        self.handlers.keys().copied().collect()
    }

    /// Get the number of registered handlers.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

impl Default for HandlerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HandlerRegistry {
    /// Create a registry pre-populated with all standard handlers.
    ///
    /// This is the recommended way to create a registry for production use.
    /// 4 active handlers: command (incl. test dispatch), ui_bridge, prompt, save_workflow_artifact
    pub fn with_standard_handlers() -> Self {
        let mut registry = Self::new();

        registry.register(CommandHandler);
        registry.register(PromptStepHandler);
        registry.register(SaveWorkflowArtifactHandler);
        registry.register(UiBridgeHandler);

        registry
    }
}

// ============================================================================
// Built-in Handlers (to be moved to separate files)
// ============================================================================

/// Handler for unknown/unsupported step types.
///
/// This is used as a fallback when no handler is registered for a step type.
pub struct UnknownStepHandler;

#[async_trait]
impl StepHandler for UnknownStepHandler {
    fn step_type(&self) -> &'static str {
        "_unknown"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        _context: &HandlerContext,
    ) -> StepHandlerResult {
        StepHandlerResult::failure(format!("Unknown step type: {}", step.step_type))
    }

    fn can_handle(&self, _step: &ExecutionStepConfig) -> bool {
        // This handler is only used as explicit fallback
        false
    }
}

/// Handler for prompt steps.
///
/// - **Non-verification phases**: Pass-through (returns success), content is consumed by AI elsewhere.
/// - **Verification phase**: Runs AI evaluation with pass/fail output (boolean prompt wrapper).
pub struct PromptStepHandler;

#[async_trait]
impl StepHandler for PromptStepHandler {
    fn step_type(&self) -> &'static str {
        "prompt"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Non-verification prompt steps remain pass-through
        if step.phase.as_deref() != Some("verification") {
            return StepHandlerResult::success();
        }

        let prompt_content = step.prompt_content.clone().unwrap_or_default();
        if prompt_content.is_empty() {
            return StepHandlerResult::success();
        }

        tracing::info!(
            "Evaluating verification prompt: {}",
            if prompt_content.len() > 80 {
                format!("{}...", &prompt_content[..80])
            } else {
                prompt_content.clone()
            }
        );

        // Gather UI context if SDK is available (best-effort)
        let ui_context = try_gather_ui_context().await;

        // Build evaluation prompt
        let eval_prompt = format_evaluation_prompt(&prompt_content, &ui_context);

        // Run AI evaluation via spawn_blocking (run_prompt_with_routing is sync)
        let task_context = crate::ai_router::TaskContext::from_prompt(&eval_prompt);
        let prompt_clone = eval_prompt.clone();
        let doctor_handle = context.app_state.doctor_handle.lock().await.clone();
        let ai_result = tokio::task::spawn_blocking(move || {
            crate::ai_provider::run_prompt_with_routing(
                &prompt_clone,
                &task_context,
                doctor_handle.as_ref(),
            )
        })
        .await;

        let ai_response = match ai_result {
            Ok(resp) => resp,
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "Prompt evaluation task panicked: {}",
                    e
                ));
            }
        };

        if !ai_response.success {
            return StepHandlerResult::failure(format!(
                "AI evaluation failed: {}",
                ai_response.error.unwrap_or_else(|| "unknown".to_string())
            ));
        }

        // Parse boolean response
        match parse_pass_fail(&ai_response.output) {
            Some(true) => {
                let reasoning = extract_reasoning(&ai_response.output);
                StepHandlerResult::success_with_data(serde_json::json!({
                    "evaluation": "pass",
                    "reasoning": reasoning,
                }))
            }
            Some(false) => {
                let reasoning = extract_reasoning(&ai_response.output);
                StepHandlerResult::failure_with_data(
                    format!("Verification failed: {}", reasoning),
                    serde_json::json!({
                        "evaluation": "fail",
                        "reasoning": reasoning,
                    }),
                )
            }
            None => {
                // Retry with explicit boolean instruction
                tracing::debug!("First response ambiguous, retrying with explicit instruction");
                let retry_prompt = format!(
                    "Your previous response did not contain a clear PASS or FAIL.\n\n\
                     Previous response: {}\n\n\
                     Respond with EXACTLY: PASS: <reason> or FAIL: <reason>",
                    ai_response.output
                );
                let retry_task_context = crate::ai_router::TaskContext::from_prompt(&retry_prompt);
                let retry_prompt_clone = retry_prompt.clone();
                let doctor_handle2 = context.app_state.doctor_handle.lock().await.clone();
                let retry_result = tokio::task::spawn_blocking(move || {
                    crate::ai_provider::run_prompt_with_routing(
                        &retry_prompt_clone,
                        &retry_task_context,
                        doctor_handle2.as_ref(),
                    )
                })
                .await;

                match retry_result {
                    Ok(resp) if resp.success => match parse_pass_fail(&resp.output) {
                        Some(true) => StepHandlerResult::success_with_data(serde_json::json!({
                            "evaluation": "pass",
                            "reasoning": extract_reasoning(&resp.output),
                            "required_retry": true,
                        })),
                        _ => StepHandlerResult::failure_with_data(
                            format!("Verification failed: {}", extract_reasoning(&resp.output)),
                            serde_json::json!({
                                "evaluation": "fail",
                                "reasoning": extract_reasoning(&resp.output),
                                "required_retry": true,
                            }),
                        ),
                    },
                    _ => StepHandlerResult::failure(
                        "Evaluation did not produce a clear pass/fail verdict",
                    ),
                }
            }
        }
    }

    fn display_name(&self) -> &'static str {
        "Prompt"
    }
}

// ============================================================================
// Boolean prompt wrapper helpers
// ============================================================================

/// Parse AI response for PASS or FAIL verdict.
///
/// Looks for PASS or FAIL at the start of a line or after common delimiters.
/// Returns `Some(true)` for PASS, `Some(false)` for FAIL, `None` for ambiguous.
fn parse_pass_fail(response: &str) -> Option<bool> {
    let trimmed = response.trim();

    // Check each line for PASS/FAIL markers
    for line in trimmed.lines() {
        let line = line.trim();
        let upper = line.to_uppercase();

        // Match "PASS:" or "PASS " or just "PASS" at start of line
        if upper.starts_with("PASS:") || upper.starts_with("PASS ") || upper == "PASS" {
            return Some(true);
        }
        if upper.starts_with("FAIL:") || upper.starts_with("FAIL ") || upper == "FAIL" {
            return Some(false);
        }

        // Match "**PASS**" or "**FAIL**" (markdown bold)
        if upper.starts_with("**PASS**") || upper.starts_with("**PASS**:") {
            return Some(true);
        }
        if upper.starts_with("**FAIL**") || upper.starts_with("**FAIL**:") {
            return Some(false);
        }

        // Match "Result: PASS" or "Verdict: PASS" patterns
        if let Some(after_colon) = line.split(':').next_back() {
            let after = after_colon.trim().to_uppercase();
            if after.starts_with("PASS") {
                return Some(true);
            }
            if after.starts_with("FAIL") {
                return Some(false);
            }
        }
    }

    None
}

/// Extract reasoning from AI response (the text after PASS:/FAIL:).
fn extract_reasoning(response: &str) -> String {
    for line in response.lines() {
        let line = line.trim();
        let upper = line.to_uppercase();

        if upper.starts_with("PASS:") {
            return line[5..].trim().to_string();
        }
        if upper.starts_with("FAIL:") {
            return line[5..].trim().to_string();
        }
        if upper.starts_with("**PASS**:") {
            return line[9..].trim().to_string();
        }
        if upper.starts_with("**FAIL**:") {
            return line[9..].trim().to_string();
        }
    }

    // Fallback: return first non-empty line
    response
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("No reasoning provided")
        .to_string()
}

/// Build the evaluation prompt for AI verification.
fn format_evaluation_prompt(criteria: &str, ui_context: &Option<String>) -> String {
    let mut prompt =
        String::from("You are evaluating a verification check during a workflow execution.\n\n");

    if let Some(ctx) = ui_context {
        prompt.push_str("## Current Page State\n\n");
        prompt.push_str(ctx);
        prompt.push_str("\n\n");
    }

    prompt.push_str("## Verification Criteria\n\n");
    prompt.push_str(criteria);
    prompt.push_str(
        "\n\n## Instructions\n\
         Based on the current state, evaluate whether the criteria above are satisfied.\n\
         Respond with EXACTLY one of:\n\
         \n\
         PASS: <brief explanation of why the check passes>\n\
         FAIL: <brief explanation of what is missing or wrong>\n\
         \n\
         You MUST respond with PASS or FAIL. No other format is acceptable.\n",
    );

    prompt
}

/// Try to gather UI context from the UI Bridge SDK (best-effort).
///
/// Makes a quick HTTP request to the SDK elements endpoint.
/// Returns None if the request fails or times out.
async fn try_gather_ui_context() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;

    let response = client
        .get("http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true")
        .send()
        .await
        .ok()?;

    if response.status().is_success() {
        let body = response.text().await.ok()?;
        if body.len() > 50 {
            // Only use if there's meaningful content
            Some(body)
        } else {
            None
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_handler_result_success() {
        let result = StepHandlerResult::success();
        assert!(result.success);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_step_handler_result_failure() {
        let result = StepHandlerResult::failure("Something went wrong");
        assert!(!result.success);
        assert_eq!(result.error, Some("Something went wrong".to_string()));
    }

    #[test]
    fn test_step_handler_result_with_screenshot() {
        let result = StepHandlerResult::success().with_screenshot("/tmp/shot.png".to_string());
        assert!(result.success);
        assert_eq!(result.screenshot_path, Some("/tmp/shot.png".to_string()));
    }

    #[test]
    fn test_handler_registry() {
        let mut registry = HandlerRegistry::new();
        assert!(registry.is_empty());

        registry.register(PromptStepHandler);
        assert_eq!(registry.len(), 1);
        assert!(registry.has_handler("prompt"));
        assert!(!registry.has_handler("workflow"));

        let handler = registry.get("prompt");
        assert!(handler.is_some());
        assert_eq!(handler.unwrap().step_type(), "prompt");
    }

    // === Boolean prompt wrapper tests ===

    #[test]
    fn test_parse_pass_fail_pass_with_colon() {
        assert_eq!(parse_pass_fail("PASS: Everything looks good"), Some(true));
    }

    #[test]
    fn test_parse_pass_fail_fail_with_colon() {
        assert_eq!(
            parse_pass_fail("FAIL: Button is missing from the page"),
            Some(false)
        );
    }

    #[test]
    fn test_parse_pass_fail_lowercase() {
        assert_eq!(parse_pass_fail("pass: All checks satisfied"), Some(true));
        assert_eq!(parse_pass_fail("fail: Missing element"), Some(false));
    }

    #[test]
    fn test_parse_pass_fail_with_preceding_text() {
        let response = "After analyzing the page state:\nPASS: The button exists and is visible";
        assert_eq!(parse_pass_fail(response), Some(true));
    }

    #[test]
    fn test_parse_pass_fail_markdown_bold() {
        assert_eq!(
            parse_pass_fail("**PASS**: The feature is implemented"),
            Some(true)
        );
        assert_eq!(
            parse_pass_fail("**FAIL**: The feature is missing"),
            Some(false)
        );
    }

    #[test]
    fn test_parse_pass_fail_verdict_prefix() {
        assert_eq!(parse_pass_fail("Result: PASS"), Some(true));
        assert_eq!(parse_pass_fail("Verdict: FAIL"), Some(false));
    }

    #[test]
    fn test_parse_pass_fail_ambiguous() {
        assert_eq!(
            parse_pass_fail("The implementation looks mostly correct but has some issues"),
            None
        );
    }

    #[test]
    fn test_parse_pass_fail_bare() {
        assert_eq!(parse_pass_fail("PASS"), Some(true));
        assert_eq!(parse_pass_fail("FAIL"), Some(false));
    }

    #[test]
    fn test_extract_reasoning_pass() {
        let reasoning = extract_reasoning("PASS: The button renders correctly and is clickable");
        assert_eq!(reasoning, "The button renders correctly and is clickable");
    }

    #[test]
    fn test_extract_reasoning_fail() {
        let reasoning = extract_reasoning("FAIL: Expected 3 items but found 0");
        assert_eq!(reasoning, "Expected 3 items but found 0");
    }

    #[test]
    fn test_extract_reasoning_fallback() {
        let reasoning = extract_reasoning("Some analysis without a clear verdict");
        assert_eq!(reasoning, "Some analysis without a clear verdict");
    }

    #[test]
    fn test_format_evaluation_prompt_without_context() {
        let prompt = format_evaluation_prompt("Check if login form exists", &None);
        assert!(prompt.contains("Verification Criteria"));
        assert!(prompt.contains("Check if login form exists"));
        assert!(prompt.contains("PASS"));
        assert!(prompt.contains("FAIL"));
        assert!(!prompt.contains("Current Page State"));
    }

    #[test]
    fn test_format_evaluation_prompt_with_context() {
        let ui_ctx = Some("[{\"id\":\"btn-login\",\"text\":\"Login\"}]".to_string());
        let prompt = format_evaluation_prompt("Check if login button exists", &ui_ctx);
        assert!(prompt.contains("Current Page State"));
        assert!(prompt.contains("btn-login"));
        assert!(prompt.contains("Check if login button exists"));
    }
}
