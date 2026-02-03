//! Runtime Environment Module
//!
//! Provides centralized access to runtime environment data for populating
//! ExecutionContext and related structures. This module acts as the single
//! source of truth for environment-related fields.

#![allow(dead_code)]

use once_cell::sync::Lazy;
use std::sync::Arc;
use std::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::AppState;

// ============================================================================
// Static Runtime State
// ============================================================================

/// Cached runner ID (persistent across restarts)
static RUNNER_ID: Lazy<RwLock<Option<String>>> = Lazy::new(|| RwLock::new(None));

/// Current project context (set when a config is loaded)
static PROJECT_CONTEXT: Lazy<RwLock<ProjectContext>> =
    Lazy::new(|| RwLock::new(ProjectContext::default()));

// ============================================================================
// Types
// ============================================================================

/// Project-level context from loaded configuration.
#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    /// Project ID from config metadata
    pub project_id: Option<String>,
    /// Workspace/organization ID
    pub workspace_id: Option<String>,
    /// Project name
    pub name: Option<String>,
    /// User ID who triggered the current task (from web backend)
    pub triggered_by: Option<String>,
}

/// Complete runtime environment snapshot.
#[derive(Debug, Clone)]
pub struct RuntimeEnv {
    /// Persistent runner/device ID
    pub runner_id: Option<String>,
    /// Environment name (dev, staging, prod)
    pub environment: String,
    /// Project context from loaded config
    pub project: ProjectContext,
}

// ============================================================================
// Environment Detection
// ============================================================================

/// Detect the current environment based on build configuration and env vars.
///
/// Priority:
/// 1. QONTINUI_ENV environment variable
/// 2. Build configuration (debug_assertions)
pub fn detect_environment() -> String {
    // Check for explicit environment variable
    if let Ok(env) = std::env::var("QONTINUI_ENV") {
        return env;
    }

    // Fall back to build configuration
    if cfg!(debug_assertions) {
        "development".to_string()
    } else {
        "production".to_string()
    }
}

// ============================================================================
// Runner ID
// ============================================================================

/// Get the runner ID, lazily initializing from AuthManager if needed.
///
/// The runner ID is a persistent UUID that identifies this runner instance.
/// It's stored in secure storage and persists across application restarts.
pub fn get_runner_id() -> Option<String> {
    // Try to read from cache first
    if let Ok(guard) = RUNNER_ID.read() {
        if let Some(ref id) = *guard {
            return Some(id.clone());
        }
    }

    // Not cached, try to get from AuthManager
    let auth_manager = crate::auth::AuthManager::new();
    match auth_manager.get_device_id() {
        Ok(id) => {
            // Cache for future calls
            if let Ok(mut guard) = RUNNER_ID.write() {
                *guard = Some(id.clone());
            }
            Some(id)
        }
        Err(e) => {
            warn!("Failed to get runner ID: {}", e);
            None
        }
    }
}

/// Generate a new trace ID for distributed tracing.
///
/// Format: UUID v4 without hyphens (32 hex chars)
pub fn generate_trace_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// Generate a new span ID for distributed tracing.
///
/// Format: 16 hex characters (64 bits)
pub fn generate_span_id() -> String {
    let uuid = Uuid::new_v4();
    let bytes = uuid.as_bytes();
    // Take first 8 bytes and format as hex
    format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]
    )
}

// ============================================================================
// Project Context
// ============================================================================

/// Set the current project context (called when loading a config).
pub fn set_project_context(ctx: ProjectContext) {
    if let Ok(mut guard) = PROJECT_CONTEXT.write() {
        debug!(
            "Setting project context: project_id={:?}, workspace_id={:?}",
            ctx.project_id, ctx.workspace_id
        );
        *guard = ctx;
    }
}

/// Get the current project context.
pub fn get_project_context() -> ProjectContext {
    PROJECT_CONTEXT
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Clear the project context (called when unloading a config).
pub fn clear_project_context() {
    if let Ok(mut guard) = PROJECT_CONTEXT.write() {
        *guard = ProjectContext::default();
    }
}

// ============================================================================
// Complete Environment Snapshot
// ============================================================================

/// Get a complete snapshot of the current runtime environment.
///
/// This is useful for populating ExecutionContext with all available data.
pub fn get_runtime_env() -> RuntimeEnv {
    RuntimeEnv {
        runner_id: get_runner_id(),
        environment: detect_environment(),
        project: get_project_context(),
    }
}

// ============================================================================
// ExecutionContext Integration
// ============================================================================

/// Extension trait for populating ExecutionContext with runtime environment data.
pub trait ExecutionContextExt {
    /// Populate the context with available runtime environment data.
    fn with_runtime_env(self) -> Self;

    /// Add distributed tracing IDs.
    fn with_new_trace(self) -> Self;
}

impl ExecutionContextExt for crate::execution_context::ExecutionContext {
    fn with_runtime_env(mut self) -> Self {
        let env = get_runtime_env();

        // Set runner ID if available
        if let Some(runner_id) = env.runner_id {
            self.runner_id = Some(runner_id);
        }

        // Set environment
        self.environment = Some(env.environment);

        // Set project context if available
        if let Some(project_id) = env.project.project_id {
            self.project_id = Some(project_id);
        }
        if let Some(workspace_id) = env.project.workspace_id {
            self.workspace_id = Some(workspace_id);
        }
        if let Some(triggered_by) = env.project.triggered_by {
            self.triggered_by = Some(triggered_by);
        }

        self
    }

    fn with_new_trace(mut self) -> Self {
        self.trace_id = Some(generate_trace_id());
        self.span_id = Some(generate_span_id());
        self
    }
}

impl ExecutionContextExt for crate::execution_context::AiSessionContext {
    fn with_runtime_env(mut self) -> Self {
        self.context = self.context.with_runtime_env();
        self
    }

    fn with_new_trace(mut self) -> Self {
        self.context = self.context.with_new_trace();
        self
    }
}

/// Extension trait for AiSessionContext to add AI-specific runtime data.
pub trait AiSessionContextExt {
    /// Populate AI-specific fields from current AI settings.
    fn with_ai_settings(self) -> Self;
}

impl AiSessionContextExt for crate::execution_context::AiSessionContext {
    fn with_ai_settings(mut self) -> Self {
        let settings = crate::settings::get_ai_settings();

        // Set model_id based on the current provider
        let model_id = match settings.provider {
            crate::settings::AiProvider::ClaudeCli => {
                // Claude CLI uses the default model or a routed model
                // For CLI, we don't know the exact model until runtime
                Some("claude-cli".to_string())
            }
            crate::settings::AiProvider::ClaudeApi => Some(settings.claude_api.model.clone()),
            crate::settings::AiProvider::GeminiCli => Some(settings.gemini_cli.model.clone()),
            crate::settings::AiProvider::GeminiApi => Some(settings.gemini_api.model.clone()),
        };

        if let Some(model) = model_id {
            self.model_id = Some(model);
        }

        self
    }
}

/// Extension trait for calculating cost from tokens in AiSessionContext.
pub trait AiSessionContextCostExt {
    /// Calculate and set cost_cents based on input_tokens, output_tokens, and model_id.
    ///
    /// This method should be called after token counts have been populated.
    /// If any required fields (input_tokens, output_tokens, model_id) are None,
    /// or if the model is not recognized, cost_cents will not be set.
    fn with_cost_from_tokens(self) -> Self;
}

impl AiSessionContextCostExt for crate::execution_context::AiSessionContext {
    fn with_cost_from_tokens(mut self) -> Self {
        if let (Some(input), Some(output), Some(model)) =
            (self.input_tokens, self.output_tokens, &self.model_id)
        {
            self.cost_cents = crate::ai_pricing::calculate_cost_cents(input, output, model);
        }
        self
    }
}

// ============================================================================
// Token Usage Tracking
// ============================================================================

/// Extension trait for updating AiSessionContext with token usage from AiResponse.
pub trait TokenUsageExt {
    /// Update token usage fields from an AiResponse.
    ///
    /// This should be called after receiving an AI response to populate
    /// the `tokens_used`, `input_tokens`, and `output_tokens` fields.
    ///
    /// Note: Token counts are only available from API providers (Claude API, Gemini API).
    /// CLI providers (Claude CLI, Gemini CLI) do not expose token counts.
    fn with_token_usage(self, response: &crate::ai_provider::AiResponse) -> Self;

    /// Accumulate token usage from an AiResponse (adds to existing counts).
    ///
    /// Useful when a session has multiple AI calls and you want to track
    /// the total token usage across all calls.
    fn accumulate_token_usage(self, response: &crate::ai_provider::AiResponse) -> Self;
}

impl TokenUsageExt for crate::execution_context::AiSessionContext {
    fn with_token_usage(mut self, response: &crate::ai_provider::AiResponse) -> Self {
        if let Some(input) = response.input_tokens {
            self.input_tokens = Some(input);
        }
        if let Some(output) = response.output_tokens {
            self.output_tokens = Some(output);
        }
        if let Some(total) = response.total_tokens() {
            self.tokens_used = Some(total);
        }

        debug!(
            "Updated AiSessionContext with token usage: input={:?}, output={:?}, total={:?}",
            self.input_tokens, self.output_tokens, self.tokens_used
        );

        self
    }

    fn accumulate_token_usage(mut self, response: &crate::ai_provider::AiResponse) -> Self {
        // Accumulate input tokens
        if let Some(new_input) = response.input_tokens {
            self.input_tokens = Some(self.input_tokens.unwrap_or(0) + new_input);
        }

        // Accumulate output tokens
        if let Some(new_output) = response.output_tokens {
            self.output_tokens = Some(self.output_tokens.unwrap_or(0) + new_output);
        }

        // Update total
        self.tokens_used = match (self.input_tokens, self.output_tokens) {
            (Some(input), Some(output)) => Some(input + output),
            (Some(input), None) => Some(input),
            (None, Some(output)) => Some(output),
            (None, None) => None,
        };

        debug!(
            "Accumulated token usage: input={:?}, output={:?}, total={:?}",
            self.input_tokens, self.output_tokens, self.tokens_used
        );

        self
    }
}

/// Update token usage on a mutable AiSessionContext reference.
///
/// Convenience function for cases where you need to update an existing context
/// without consuming and returning it.
pub fn update_token_usage(
    ctx: &mut crate::execution_context::AiSessionContext,
    response: &crate::ai_provider::AiResponse,
) {
    if let Some(input) = response.input_tokens {
        ctx.input_tokens = Some(input);
    }
    if let Some(output) = response.output_tokens {
        ctx.output_tokens = Some(output);
    }
    if let Some(total) = response.total_tokens() {
        ctx.tokens_used = Some(total);
    }

    debug!(
        "Updated AiSessionContext with token usage: input={:?}, output={:?}, total={:?}",
        ctx.input_tokens, ctx.output_tokens, ctx.tokens_used
    );
}

/// Accumulate token usage on a mutable AiSessionContext reference.
///
/// Adds to existing counts instead of replacing them.
pub fn accumulate_token_usage(
    ctx: &mut crate::execution_context::AiSessionContext,
    response: &crate::ai_provider::AiResponse,
) {
    if let Some(new_input) = response.input_tokens {
        ctx.input_tokens = Some(ctx.input_tokens.unwrap_or(0) + new_input);
    }
    if let Some(new_output) = response.output_tokens {
        ctx.output_tokens = Some(ctx.output_tokens.unwrap_or(0) + new_output);
    }

    ctx.tokens_used = match (ctx.input_tokens, ctx.output_tokens) {
        (Some(input), Some(output)) => Some(input + output),
        (Some(input), None) => Some(input),
        (None, Some(output)) => Some(output),
        (None, None) => None,
    };

    debug!(
        "Accumulated token usage: input={:?}, output={:?}, total={:?}",
        ctx.input_tokens, ctx.output_tokens, ctx.tokens_used
    );
}

// ============================================================================
// MCP Tools Discovery
// ============================================================================

/// Get available MCP tools from all connected servers.
///
/// This queries the MCP client manager for all connected servers and collects
/// their available tools. Tool names are prefixed with the server name for clarity.
///
/// Returns a list of tool names in the format "server_name:tool_name".
pub async fn get_available_mcp_tools(app_state: &Arc<AppState>) -> Vec<String> {
    let mut tools = Vec::new();

    // Get the MCP client manager
    let mcp_manager = app_state.mcp_client_manager.lock().await;

    // Get status of all servers (includes tool info for connected servers)
    let statuses = mcp_manager.get_all_status().await;

    for status in statuses {
        if status.connected {
            if let Some(server_tools) = status.tools {
                // Get the server config to get the friendly name
                let server_name = match mcp_manager.get_server(&status.server_id).await {
                    Ok(Some(config)) => config.name,
                    _ => status.server_id.clone(),
                };

                for tool in server_tools {
                    // Format: "server_name:tool_name" for clarity
                    tools.push(format!("{}:{}", server_name, tool.name));
                }
            }
        }
    }

    if !tools.is_empty() {
        info!(
            "Found {} available MCP tools from connected servers",
            tools.len()
        );
        debug!("Available MCP tools: {:?}", tools);
    }

    tools
}

/// Extension trait for setting available tools on AiSessionContext.
pub trait AiSessionContextToolsExt {
    /// Set the available MCP tools for this session.
    fn with_available_tools(self, tools: Vec<String>) -> Self;
}

impl AiSessionContextToolsExt for crate::execution_context::AiSessionContext {
    fn with_available_tools(mut self, tools: Vec<String>) -> Self {
        if !tools.is_empty() {
            self.available_tools = Some(tools);
        }
        self
    }
}

// ============================================================================
// Turn Counting
// ============================================================================

/// Count conversation turns from output log by counting [SESSION_START:N] markers.
///
/// Each `[SESSION_START:N]` marker in the output log represents one AI session turn.
/// This function counts how many such markers exist in the output.
///
/// # Example
/// ```
/// let output = "[SESSION_START:1]\nSome output\n[SESSION_START:2]\nMore output";
/// assert_eq!(count_turns_in_output(output), 2);
/// ```
pub fn count_turns_in_output(output: &str) -> u32 {
    output.matches("[SESSION_START:").count() as u32
}

/// Get the maximum session number from output log.
///
/// Parses the output log to find the highest `[SESSION_START:N]` marker number.
/// Returns 0 if no markers are found.
///
/// This is useful for continuation scenarios where we need to know the last
/// session number to continue from.
pub fn get_max_session_number(output: &str) -> u32 {
    let mut max_session = 0u32;

    for marker_start in output.match_indices("[SESSION_START:") {
        let rest = &output[marker_start.0 + 15..]; // Skip "[SESSION_START:"
        if let Some(end_bracket) = rest.find(']') {
            if let Ok(num) = rest[..end_bracket].parse::<u32>() {
                max_session = max_session.max(num);
            }
        }
    }

    max_session
}

/// Extension trait for setting turn count on AiSessionContext from output log.
pub trait AiSessionContextTurnExt {
    /// Set the turn count based on output log analysis.
    ///
    /// For new sessions, the turn count will be 1.
    /// For continuations, this should be called with the existing output log
    /// to get the correct accumulated turn count.
    fn with_turn_count_from_output(self, output: &str) -> Self;

    /// Set continuation state with turn count from previous output.
    ///
    /// This combines marking the session as a continuation and setting the
    /// turn count based on the previous session's output log.
    fn as_continuation_with_turns(
        self,
        previous_session_id: impl Into<String>,
        previous_output: &str,
    ) -> Self;
}

impl AiSessionContextTurnExt for crate::execution_context::AiSessionContext {
    fn with_turn_count_from_output(self, output: &str) -> Self {
        let turn_count = count_turns_in_output(output);
        // If there are no markers, this is a new session with turn 1
        // If there are markers, use that count
        let effective_count = if turn_count == 0 { 1 } else { turn_count };
        self.with_turn_count(effective_count)
    }

    fn as_continuation_with_turns(
        self,
        previous_session_id: impl Into<String>,
        previous_output: &str,
    ) -> Self {
        let previous_turns = count_turns_in_output(previous_output);
        // The new turn will be previous + 1
        let turn_count = previous_turns + 1;
        self.as_continuation_of(previous_session_id)
            .with_turn_count(turn_count)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_environment() {
        let env = detect_environment();
        // In test builds, should be "development"
        assert!(env == "development" || env == "production");
    }

    #[test]
    fn test_generate_trace_id() {
        let trace_id = generate_trace_id();
        assert_eq!(trace_id.len(), 32);
        assert!(trace_id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_span_id() {
        let span_id = generate_span_id();
        assert_eq!(span_id.len(), 16);
        assert!(span_id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_project_context() {
        // Set context
        set_project_context(ProjectContext {
            project_id: Some("proj-123".to_string()),
            workspace_id: Some("ws-456".to_string()),
            name: Some("Test Project".to_string()),
            triggered_by: Some("user-789".to_string()),
        });

        // Get context
        let ctx = get_project_context();
        assert_eq!(ctx.project_id, Some("proj-123".to_string()));
        assert_eq!(ctx.workspace_id, Some("ws-456".to_string()));
        assert_eq!(ctx.triggered_by, Some("user-789".to_string()));

        // Clear context
        clear_project_context();
        let ctx = get_project_context();
        assert_eq!(ctx.project_id, None);
    }

    #[test]
    fn test_ai_session_context_cost_from_tokens() {
        use super::AiSessionContextCostExt;
        use crate::execution_context::AiSessionContext;

        // Test with valid model and tokens
        let ctx = AiSessionContext::agentic("task-123", "Test Workflow", 1)
            .with_model("claude-3-5-sonnet")
            .with_tokens(100_000, 50_000)
            .with_cost_from_tokens();

        // Input: 100K / 1M * $3 = $0.30 = 30 cents
        // Output: 50K / 1M * $15 = $0.75 = 75 cents
        // Total: 105 cents
        assert_eq!(ctx.cost_cents, Some(105));
    }

    #[test]
    fn test_ai_session_context_cost_unknown_model() {
        use super::AiSessionContextCostExt;
        use crate::execution_context::AiSessionContext;

        // Test with unknown model - cost should not be set
        let ctx = AiSessionContext::agentic("task-123", "Test Workflow", 1)
            .with_model("unknown-model-xyz")
            .with_tokens(100_000, 50_000)
            .with_cost_from_tokens();

        assert_eq!(ctx.cost_cents, None);
    }

    #[test]
    fn test_ai_session_context_cost_no_tokens() {
        use super::AiSessionContextCostExt;
        use crate::execution_context::AiSessionContext;

        // Test without tokens - cost should not be set
        let ctx = AiSessionContext::agentic("task-123", "Test Workflow", 1)
            .with_model("claude-3-5-sonnet")
            .with_cost_from_tokens();

        assert_eq!(ctx.cost_cents, None);
    }

    #[test]
    fn test_count_turns_in_output_empty() {
        assert_eq!(count_turns_in_output(""), 0);
        assert_eq!(count_turns_in_output("Some random output without markers"), 0);
    }

    #[test]
    fn test_count_turns_in_output_single() {
        let output = "[SESSION_START:1]\nSome AI output here";
        assert_eq!(count_turns_in_output(output), 1);
    }

    #[test]
    fn test_count_turns_in_output_multiple() {
        let output = r#"[SESSION_START:1]
First session output
[SESSION_START:2]
Second session output
[SESSION_START:3]
Third session output"#;
        assert_eq!(count_turns_in_output(output), 3);
    }

    #[test]
    fn test_get_max_session_number_empty() {
        assert_eq!(get_max_session_number(""), 0);
        assert_eq!(get_max_session_number("No markers here"), 0);
    }

    #[test]
    fn test_get_max_session_number_single() {
        let output = "[SESSION_START:5]\nSome output";
        assert_eq!(get_max_session_number(output), 5);
    }

    #[test]
    fn test_get_max_session_number_multiple() {
        let output = r#"[SESSION_START:1]
First
[SESSION_START:3]
Third (skipped 2)
[SESSION_START:2]
Out of order"#;
        assert_eq!(get_max_session_number(output), 3);
    }

    #[test]
    fn test_ai_session_context_turn_count_from_output() {
        use super::AiSessionContextTurnExt;
        use crate::execution_context::AiSessionContext;

        // Empty output should result in turn_count = 1 (first turn)
        let ctx = AiSessionContext::agentic("task-123", "Test Workflow", 1)
            .with_turn_count_from_output("");
        assert_eq!(ctx.turn_count, Some(1));

        // Output with 3 markers should result in turn_count = 3
        let output = "[SESSION_START:1]\n[SESSION_START:2]\n[SESSION_START:3]\n";
        let ctx = AiSessionContext::agentic("task-123", "Test Workflow", 3)
            .with_turn_count_from_output(output);
        assert_eq!(ctx.turn_count, Some(3));
    }

    #[test]
    fn test_ai_session_context_continuation_with_turns() {
        use super::AiSessionContextTurnExt;
        use crate::execution_context::AiSessionContext;

        // Previous session had 3 turns, continuation should have turn 4
        let previous_output = "[SESSION_START:1]\n[SESSION_START:2]\n[SESSION_START:3]\n";
        let ctx = AiSessionContext::agentic("task-123", "Test Workflow", 4)
            .as_continuation_with_turns("previous-session-id", previous_output);

        assert_eq!(ctx.turn_count, Some(4));
        assert!(ctx.is_continuation);
        assert_eq!(ctx.continued_from, Some("previous-session-id".to_string()));
    }

    #[test]
    fn test_token_usage_ext_with_token_usage() {
        use super::TokenUsageExt;
        use crate::ai_provider::AiResponse;
        use crate::execution_context::AiSessionContext;

        // Test with token response from API provider
        let response = AiResponse::success_with_tokens("Hello!".to_string(), 100, 50);
        let ctx =
            AiSessionContext::agentic("task-123", "Test Workflow", 1).with_token_usage(&response);

        assert_eq!(ctx.input_tokens, Some(100));
        assert_eq!(ctx.output_tokens, Some(50));
        assert_eq!(ctx.tokens_used, Some(150));
    }

    #[test]
    fn test_token_usage_ext_with_no_tokens() {
        use super::TokenUsageExt;
        use crate::ai_provider::AiResponse;
        use crate::execution_context::AiSessionContext;

        // Test with CLI response (no tokens)
        let response = AiResponse::success("Hello!".to_string());
        let ctx =
            AiSessionContext::agentic("task-123", "Test Workflow", 1).with_token_usage(&response);

        assert_eq!(ctx.input_tokens, None);
        assert_eq!(ctx.output_tokens, None);
        assert_eq!(ctx.tokens_used, None);
    }

    #[test]
    fn test_token_usage_ext_accumulate() {
        use super::TokenUsageExt;
        use crate::ai_provider::AiResponse;
        use crate::execution_context::AiSessionContext;

        // Start with first response
        let response1 = AiResponse::success_with_tokens("First".to_string(), 100, 50);
        let ctx =
            AiSessionContext::agentic("task-123", "Test Workflow", 1).with_token_usage(&response1);

        assert_eq!(ctx.input_tokens, Some(100));
        assert_eq!(ctx.output_tokens, Some(50));
        assert_eq!(ctx.tokens_used, Some(150));

        // Accumulate second response
        let response2 = AiResponse::success_with_tokens("Second".to_string(), 200, 100);
        let ctx = ctx.accumulate_token_usage(&response2);

        assert_eq!(ctx.input_tokens, Some(300));
        assert_eq!(ctx.output_tokens, Some(150));
        assert_eq!(ctx.tokens_used, Some(450));
    }

    #[test]
    fn test_update_token_usage_function() {
        use crate::ai_provider::AiResponse;
        use crate::execution_context::AiSessionContext;

        let response = AiResponse::success_with_tokens("Hello!".to_string(), 100, 50);
        let mut ctx = AiSessionContext::agentic("task-123", "Test Workflow", 1);

        // Use the function instead of trait method
        super::update_token_usage(&mut ctx, &response);

        assert_eq!(ctx.input_tokens, Some(100));
        assert_eq!(ctx.output_tokens, Some(50));
        assert_eq!(ctx.tokens_used, Some(150));
    }

    #[test]
    fn test_accumulate_token_usage_function() {
        use crate::ai_provider::AiResponse;
        use crate::execution_context::AiSessionContext;

        let mut ctx = AiSessionContext::agentic("task-123", "Test Workflow", 1);

        // First response
        let response1 = AiResponse::success_with_tokens("First".to_string(), 100, 50);
        super::update_token_usage(&mut ctx, &response1);

        // Accumulate second response
        let response2 = AiResponse::success_with_tokens("Second".to_string(), 200, 100);
        super::accumulate_token_usage(&mut ctx, &response2);

        assert_eq!(ctx.input_tokens, Some(300));
        assert_eq!(ctx.output_tokens, Some(150));
        assert_eq!(ctx.tokens_used, Some(450));
    }
}
