//! Step Handlers Module
//!
//! This module defines the `StepHandler` trait and registry for executing
//! different step types. Each step type (workflow, playwright, shell_command, etc.)
//! has its own handler implementation.
//!
//! ## Architecture
//!
//! The handler pattern replaces the monolithic match statement in `execute_single_step`
//! with a polymorphic dispatch mechanism:
//!
//! ```text
//! StepExecutor.execute_single_step()
//!     └── HandlerRegistry.get_handler(step_type)
//!             └── handler.execute(step, context)
//! ```
//!
//! ## Adding a New Step Type
//!
//! 1. Create a new file in `handlers/` (e.g., `my_step.rs`)
//! 2. Implement `StepHandler` for your handler struct
//! 3. Register the handler in `HandlerRegistry::new()`
//!
//! ## Example Handler
//!
//! ```ignore
//! pub struct MyStepHandler {
//!     // dependencies
//! }
//!
//! #[async_trait]
//! impl StepHandler for MyStepHandler {
//!     fn step_type(&self) -> &'static str {
//!         "my_step"
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

use super::executor::ExecutionStepConfig;
use super::events::TreeEventEmitter;

// Step handler implementations
mod action;
mod api_request;
mod check;
mod check_group;
mod gui_action;
mod log_watch;
mod macro_step;
mod mcp_call;
mod playwright;
mod screenshot;
mod script;
mod shell;
mod shell_command;
mod state;
mod test;
mod workflow;
mod workflow_ref;

// AWAS handlers
mod awas_check_support;
mod awas_common;
mod awas_discover;
mod awas_execute;
mod awas_extract_elements;
mod awas_list_actions;

// Re-export handlers for registration
pub use action::ActionHandler;
pub use api_request::ApiRequestHandler;
pub use check::CheckHandler;
pub use check_group::CheckGroupHandler;
pub use gui_action::GuiActionHandler;
pub use log_watch::LogWatchHandler;
pub use macro_step::MacroHandler;
pub use mcp_call::McpCallHandler;
pub use playwright::PlaywrightHandler;
pub use screenshot::ScreenshotHandler;
pub use script::ScriptHandler;
pub use shell::ShellHandler;
pub use shell_command::ShellCommandHandler;
pub use state::StateHandler;
pub use test::TestHandler;
pub use workflow::WorkflowHandler;
pub use workflow_ref::WorkflowRefHandler;

// AWAS handler re-exports
pub use awas_check_support::AwasCheckSupportHandler;
pub use awas_discover::AwasDiscoverHandler;
pub use awas_execute::AwasExecuteHandler;
pub use awas_extract_elements::AwasExtractElementsHandler;
pub use awas_list_actions::AwasListActionsHandler;

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
    pub fn with_standard_handlers() -> Self {
        let mut registry = Self::new();

        // GUI Automation handlers
        registry.register(WorkflowHandler);
        registry.register(WorkflowRefHandler);
        registry.register(StateHandler);
        registry.register(ActionHandler);
        registry.register(GuiActionHandler);
        registry.register(ScreenshotHandler);
        registry.register(MacroHandler);

        // Shell/Script handlers
        registry.register(ShellCommandHandler);
        registry.register(ShellHandler); // Alias for shell_command
        registry.register(ScriptHandler);
        registry.register(PlaywrightHandler);

        // Verification handlers
        registry.register(LogWatchHandler);
        registry.register(CheckHandler);
        registry.register(CheckGroupHandler);

        // API handlers
        registry.register(ApiRequestHandler);

        // MCP handlers
        registry.register(McpCallHandler);

        // Test handlers
        registry.register(TestHandler);

        // Simple pass-through handlers
        registry.register(PromptStepHandler);

        // AWAS handlers
        registry.register(AwasDiscoverHandler);
        registry.register(AwasExecuteHandler);
        registry.register(AwasCheckSupportHandler);
        registry.register(AwasListActionsHandler);
        registry.register(AwasExtractElementsHandler);

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

/// Handler for prompt steps (pass-through, no execution).
///
/// Prompt steps are not executed directly - their content is passed to AI.
/// This handler just returns success.
pub struct PromptStepHandler;

#[async_trait]
impl StepHandler for PromptStepHandler {
    fn step_type(&self) -> &'static str {
        "prompt"
    }

    async fn execute(
        &self,
        _step: &ExecutionStepConfig,
        _context: &HandlerContext,
    ) -> StepHandlerResult {
        // Prompt steps are pass-through - content goes to AI, not executed here
        StepHandlerResult::success()
    }

    fn display_name(&self) -> &'static str {
        "Prompt"
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
}
