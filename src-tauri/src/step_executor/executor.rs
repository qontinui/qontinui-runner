//! Step Executor Module
//!
//! Provides unified execution of automation steps (workflows, actions, states,
//! screenshots, Playwright tests, AWAS). This is the core execution layer used by:
//! - Run page (single workflow execution)
//! - AI Builder (multi-step execution before AI session)
//! - MCP API (direct step execution)
//!
//! The design principle: multi-step execution is the foundation, and running
//! a single workflow is just a special case (one step of type "workflow").
//!
//! ## Architecture
//!
//! Step execution uses a polymorphic handler dispatch system:
//!
//! ```text
//! StepExecutor.execute_single_step()
//!     └── HandlerRegistry.get_handler(step_type)
//!             └── handler.execute(step, context)
//! ```
//!
//! All step types are implemented as separate handlers in the `handlers/` module.
//! The `HandlerRegistry` maps step type strings to handler implementations.
//!
//! ## Step Categories (24 handlers)
//!
//! - **GUI** (7): workflow, workflow_ref, state, action, gui_action, screenshot, macro
//! - **Shell/Script** (4): shell_command, shell, script, playwright
//! - **Verification** (4): log_watch, check, check_group, test
//! - **API/MCP** (2): api_request, mcp_call
//! - **AWAS** (5): awas_discover, awas_execute, awas_check_support, awas_list_actions, awas_extract_elements
//! - **Other** (1): prompt

#![allow(dead_code)]

use jsonpath_rust::JsonPathFinder;
use regex::Regex;

use crate::action_service::UnifiedActionService;
use crate::api_request::{ApiRequestConfig, ApiRequestSession, HttpMethod, VariableExtraction};
use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::database::CreateTaskRunEventInput;
use crate::display::RawEvent;
use crate::executor::file_logger::FileLogger;
use crate::iteration_bundle::{
    parse_action_events, parse_image_recognition_events, ActionEvent, ImageRecognitionEvent,
    RelevantLogSources,
};
use crate::mcp_client::CreateMcpCallInput;
use crate::orchestrator::context_propagation::{
    ExpressionEvaluator, RuntimeContext, SharedVariableStore,
};
use crate::unified_workflow_executor::get_parent_task_id;

// Handler system imports
use super::handlers::{HandlerContext, HandlerRegistry};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

/// Workflow phase for step execution.
///
/// Steps can explicitly declare which phase they belong to, eliminating
/// the need for heuristic-based phase detection from step names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepPhase {
    /// Setup phase - runs once at the start
    Setup,
    /// Verification phase - runs tests/checks each iteration
    Verification,
    /// Agentic phase - AI execution
    Agentic,
    /// Completion phase - runs once at the end
    Completion,
}

impl StepPhase {
    /// Convert to string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            StepPhase::Setup => "setup",
            StepPhase::Verification => "verification",
            StepPhase::Agentic => "agentic",
            StepPhase::Completion => "completion",
        }
    }

    /// Parse from string, returning None for invalid values.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "setup" => Some(StepPhase::Setup),
            "verification" => Some(StepPhase::Verification),
            "agentic" => Some(StepPhase::Agentic),
            "completion" => Some(StepPhase::Completion),
            _ => None,
        }
    }
}

impl std::fmt::Display for StepPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Configuration for a single execution step
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ExecutionStepConfig {
    /// Step type: "workflow", "state", "action", "screenshot", "playwright", "prompt"
    #[serde(rename = "type")]
    pub step_type: String,

    /// Step name (workflow name, state name, or description)
    #[serde(default)]
    pub name: Option<String>,

    /// For action steps: "click", "double_click", "right_click"
    #[serde(rename = "actionType")]
    pub action_type: Option<String>,

    /// Target image ID for action steps
    #[serde(rename = "targetImageId")]
    pub target_image_id: Option<String>,

    /// Target image name for display
    #[serde(rename = "targetImageName")]
    pub target_image_name: Option<String>,

    /// Monitor index (0 = primary)
    #[serde(rename = "monitorIndex", default)]
    pub monitor_index: Option<i32>,

    /// Whether to take screenshot after this step
    #[serde(rename = "takeScreenshot", default)]
    pub take_screenshot: bool,

    /// Delay before screenshot in seconds
    #[serde(rename = "screenshotDelay", default)]
    pub screenshot_delay: u32,

    /// Monitor for screenshot ("all" or index)
    #[serde(rename = "screenshotMonitor", default)]
    pub screenshot_monitor: Option<serde_json::Value>,

    /// Playwright script ID
    #[serde(rename = "playwrightScriptId")]
    pub playwright_script_id: Option<String>,

    /// Playwright script content (for combined/inline scripts)
    #[serde(rename = "playwrightScriptContent")]
    pub playwright_script_content: Option<String>,

    /// Playwright target URL (for combined scripts)
    #[serde(rename = "playwrightTargetUrl")]
    pub playwright_target_url: Option<String>,

    /// Prompt content (for prompt steps - not executed, passed to AI)
    /// Frontend uses "content" field, so we accept both "promptContent" and "content"
    #[serde(rename = "promptContent", alias = "content")]
    pub prompt_content: Option<String>,

    /// Timeout for this step in seconds (default: 300 for workflows, 30 for actions)
    #[serde(rename = "timeoutSeconds", default)]
    pub timeout_seconds: Option<u64>,

    /// Initial state IDs for workflow steps (overrides default initial states)
    #[serde(rename = "initialStateIds", default)]
    pub initial_state_ids: Option<Vec<String>>,

    /// Whether this step is a setup step (brings app to target state) vs verification step.
    /// Setup steps typically run on the first iteration only.
    /// Default: true for GUI automation steps (workflow, state, action, gui_workflow)
    /// Default: false for verification steps (playwright, test, screenshot)
    #[serde(rename = "isSetup", default)]
    pub is_setup: Option<bool>,

    /// Workflow phase: "setup", "verification", "agentic", or "completion"
    #[serde(default)]
    pub phase: Option<String>,

    /// Whether to run this step on subsequent iterations (after the first).
    /// Default: true (all steps run on each iteration for fresh data)
    /// Users can toggle off individual steps if they only need to run once (e.g., one-time setup)
    #[serde(rename = "runOnSubsequentIterations", default)]
    pub run_on_subsequent_iterations: Option<bool>,

    /// Optional sub-step identifier for granular progress tracking.
    /// When multiple prompts are consolidated, each sub-step has a unique ID
    /// that allows tracking completion at a granular level.
    #[serde(rename = "subStepId", alias = "sub_step_id")]
    pub sub_step_id: Option<String>,

    /// Test ID for verification test steps
    #[serde(rename = "testId", alias = "test_id")]
    pub test_id: Option<String>,

    /// Test type for verification test steps
    #[serde(alias = "testType", alias = "test_type")]
    pub test_type: Option<String>,

    /// Whether test failure should fail the workflow
    #[serde(
        alias = "testIsCritical",
        alias = "test_is_critical",
        alias = "is_critical",
        alias = "is_blocking",
        default
    )]
    pub test_is_critical: Option<bool>,

    // ========================================================================
    // AWAS Step Fields
    // ========================================================================
    /// AWAS: URL for AWAS operations (discover, execute, check_support)
    #[serde(rename = "awasUrl")]
    pub awas_url: Option<String>,

    /// AWAS: Action ID to execute (for awas_execute steps)
    #[serde(rename = "awasActionId")]
    pub awas_action_id: Option<String>,

    /// AWAS: Parameters for action execution (for awas_execute steps)
    #[serde(rename = "awasParams")]
    pub awas_params: Option<serde_json::Value>,

    /// AWAS: HTML content for element extraction (for awas_extract_elements steps)
    #[serde(rename = "awasHtml")]
    pub awas_html: Option<String>,

    /// AWAS: Base URL for resolving relative URLs (for awas_extract_elements steps)
    #[serde(rename = "awasBaseUrl")]
    pub awas_base_url: Option<String>,

    // ========================================================================
    // MCP Call Step Fields
    // ========================================================================
    /// MCP: Server ID for the MCP server to call
    #[serde(rename = "mcpServerId")]
    pub mcp_server_id: Option<String>,

    /// MCP: Server name for display purposes
    #[serde(rename = "mcpServerName")]
    pub mcp_server_name: Option<String>,

    /// MCP: Tool name to call on the MCP server
    #[serde(rename = "mcpToolName")]
    pub mcp_tool_name: Option<String>,

    /// MCP: Arguments to pass to the tool (JSON object)
    #[serde(rename = "mcpArguments")]
    pub mcp_arguments: Option<serde_json::Value>,

    /// MCP: Whether to fail the workflow if the MCP call fails
    #[serde(rename = "mcpFailOnError", default)]
    pub mcp_fail_on_error: Option<bool>,

    // ========================================================================
    // Shell Command Step Fields
    // ========================================================================
    /// Shell Command: The command to execute
    #[serde(alias = "shellCommand", alias = "command")]
    pub shell_command: Option<String>,

    /// Shell Command: Reference to a saved shell command ID
    #[serde(alias = "shellCommandId", alias = "shell_command_id")]
    pub shell_command_id: Option<String>,

    /// Shell Command: Working directory for command execution
    #[serde(alias = "shellCommandWorkingDirectory", alias = "working_directory")]
    pub shell_command_working_directory: Option<String>,

    /// Shell Command: Whether to fail the workflow if command returns non-zero
    #[serde(alias = "shellCommandFailOnError", alias = "fail_on_error", default)]
    pub shell_command_fail_on_error: Option<bool>,

    // ========================================================================
    // API Request Step Fields
    // ========================================================================
    /// API Request: HTTP method (GET, POST, PUT, PATCH, DELETE)
    #[serde(alias = "apiMethod", alias = "method")]
    pub api_method: Option<String>,

    /// API Request: URL to request
    #[serde(alias = "apiUrl", alias = "url")]
    pub api_url: Option<String>,

    /// API Request: Request headers as JSON object
    #[serde(alias = "apiHeaders", alias = "headers")]
    pub api_headers: Option<serde_json::Value>,

    /// API Request: Request body
    #[serde(alias = "apiBody", alias = "body")]
    pub api_body: Option<String>,

    /// API Request: Content type
    #[serde(alias = "apiContentType", alias = "content_type")]
    pub api_content_type: Option<String>,

    /// API Request: Variable name to store response body (enables request chaining)
    /// If specified, the response body will be stored in RuntimeContext with this name.
    /// Subsequent steps can reference it using `{{variable_name}}` syntax.
    #[serde(alias = "apiOutputVariable", alias = "output_variable")]
    pub api_output_variable: Option<String>,

    /// API Request: Variable extractions from response using JSON paths.
    /// Each extraction specifies a variable name and JSON path to extract from the response.
    #[serde(alias = "apiExtractions", alias = "extractions")]
    pub api_extractions: Option<Vec<VariableExtraction>>,

    /// API Request: Timeout in milliseconds (optional, no timeout if not specified)
    #[serde(alias = "apiTimeoutMs", alias = "timeout_ms")]
    pub api_timeout_ms: Option<u64>,

    // ========================================================================
    // Check Step Fields
    // ========================================================================
    /// Check: Type of check (lint, format, typecheck, analyze, security, custom_command)
    #[serde(alias = "checkType", alias = "check_type")]
    pub check_type: Option<String>,

    /// Check: Command to run
    /// Note: The "command" alias conflicts with shell_command, so JSON "command" will go there.
    /// The execute_check_step function handles this by checking both fields.
    #[serde(alias = "checkCommand")]
    pub check_command: Option<String>,

    /// Check: Working directory
    /// Also used for repository test steps
    #[serde(alias = "checkWorkingDirectory")]
    pub check_working_directory: Option<String>,

    /// Check: Whether to run auto-fix
    #[serde(alias = "checkAutoFix", alias = "auto_fix", default)]
    pub check_auto_fix: Option<bool>,

    /// Check: URL to check for http_status check type
    #[serde(alias = "checkUrl")]
    pub check_url: Option<String>,

    /// Check: Expected HTTP status code (default: 200)
    #[serde(alias = "expectedStatus")]
    pub expected_status: Option<u16>,

    // ========================================================================
    // Macro Step Fields
    // ========================================================================
    /// Macro: ID of the saved macro to execute
    #[serde(alias = "macroId", alias = "macro_id")]
    pub macro_id: Option<String>,

    // ========================================================================
    // Check Group Step Fields
    // ========================================================================
    /// Check Group: ID of the check group to execute
    #[serde(alias = "checkGroupId", alias = "check_group_id")]
    pub check_group_id: Option<String>,

    // ========================================================================
    // Log Watch Step Fields
    // ========================================================================
    /// Log Watch: Log sources to watch (e.g., ["backend.log", "frontend.log"])
    /// If not specified, defaults to ["backend.log", "frontend.log"]
    #[serde(rename = "logSources", alias = "log_sources")]
    pub log_sources: Option<Vec<String>>,

    /// Log Watch: Time window in seconds to scan (default: 60)
    #[serde(rename = "timeWindowSeconds", alias = "time_window_seconds")]
    pub time_window_seconds: Option<u64>,

    /// Log Watch: Custom error patterns to match (in addition to defaults)
    #[serde(rename = "errorPatterns", alias = "error_patterns")]
    pub error_patterns: Option<Vec<String>>,
}

impl ExecutionStepConfig {
    /// Get the typed phase if set, parsing from string if needed.
    pub fn get_phase(&self) -> Option<StepPhase> {
        self.phase.as_ref().and_then(|p| StepPhase::from_str_opt(p))
    }

    /// Set the phase explicitly.
    pub fn with_phase(mut self, phase: StepPhase) -> Self {
        self.phase = Some(phase.as_str().to_string());
        self
    }

    /// Set the phase on a mutable reference.
    pub fn set_phase(&mut self, phase: StepPhase) {
        self.phase = Some(phase.as_str().to_string());
    }

    /// Create a workflow step (convenience constructor)
    pub fn workflow(name: &str) -> Self {
        Self {
            step_type: "workflow".to_string(),
            name: Some(name.to_string()),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: false,
            screenshot_delay: 0,
            screenshot_monitor: None,
            playwright_script_id: None,
            playwright_script_content: None,
            playwright_target_url: None,
            prompt_content: None,
            timeout_seconds: None, // No timeout by default
            initial_state_ids: None,
            is_setup: Some(true), // Workflow is setup by default
            phase: None,
            run_on_subsequent_iterations: Some(true), // Default: run on all iterations for fresh data
            test_id: None,
            test_type: None,
            test_is_critical: None,
            sub_step_id: None,
            // AWAS fields
            awas_url: None,
            awas_action_id: None,
            awas_params: None,
            awas_html: None,
            awas_base_url: None,
            // MCP fields
            mcp_server_id: None,
            mcp_server_name: None,
            mcp_tool_name: None,
            mcp_arguments: None,
            mcp_fail_on_error: None,
            // Shell command fields
            shell_command: None,
            shell_command_id: None,
            shell_command_working_directory: None,
            shell_command_fail_on_error: None,
            // API request fields
            api_method: None,
            api_url: None,
            api_headers: None,
            api_body: None,
            api_content_type: None,
            api_output_variable: None,
            api_extractions: None,
            api_timeout_ms: None,
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            check_url: None,
            expected_status: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
            // Log watch fields
            log_sources: None,
            time_window_seconds: None,
            error_patterns: None,
        }
    }

    /// Create a workflow step with screenshot
    pub fn workflow_with_screenshot(name: &str, delay: u32) -> Self {
        Self {
            step_type: "workflow".to_string(),
            name: Some(name.to_string()),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: true,
            screenshot_delay: delay,
            screenshot_monitor: None,
            playwright_script_id: None,
            playwright_script_content: None,
            playwright_target_url: None,
            prompt_content: None,
            timeout_seconds: None, // No timeout by default
            initial_state_ids: None,
            is_setup: Some(true), // Workflow is setup by default
            phase: None,
            run_on_subsequent_iterations: Some(true), // Default: run on all iterations for fresh data
            test_id: None,
            test_type: None,
            test_is_critical: None,
            sub_step_id: None,
            // AWAS fields
            awas_url: None,
            awas_action_id: None,
            awas_params: None,
            awas_html: None,
            awas_base_url: None,
            // MCP fields
            mcp_server_id: None,
            mcp_server_name: None,
            mcp_tool_name: None,
            mcp_arguments: None,
            mcp_fail_on_error: None,
            // Shell command fields
            shell_command: None,
            shell_command_id: None,
            shell_command_working_directory: None,
            shell_command_fail_on_error: None,
            // API request fields
            api_method: None,
            api_url: None,
            api_headers: None,
            api_body: None,
            api_content_type: None,
            api_output_variable: None,
            api_extractions: None,
            api_timeout_ms: None,
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            check_url: None,
            expected_status: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
            // Log watch fields
            log_sources: None,
            time_window_seconds: None,
            error_patterns: None,
        }
    }

    /// Create a screenshot step
    pub fn screenshot(monitor: Option<i32>, delay: u32) -> Self {
        Self {
            step_type: "screenshot".to_string(),
            name: Some("Capture Screenshot".to_string()),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: monitor,
            take_screenshot: true,
            screenshot_delay: delay,
            screenshot_monitor: monitor.map(|m| serde_json::Value::Number(m.into())),
            playwright_script_id: None,
            playwright_script_content: None,
            playwright_target_url: None,
            prompt_content: None,
            timeout_seconds: None, // No timeout by default
            initial_state_ids: None,
            is_setup: Some(false), // Screenshot is verification, not setup
            phase: None,
            run_on_subsequent_iterations: Some(true), // Verification runs on all iterations
            test_id: None,
            test_type: None,
            test_is_critical: None,
            sub_step_id: None,
            // AWAS fields
            awas_url: None,
            awas_action_id: None,
            awas_params: None,
            awas_html: None,
            awas_base_url: None,
            // MCP fields
            mcp_server_id: None,
            mcp_server_name: None,
            mcp_tool_name: None,
            mcp_arguments: None,
            mcp_fail_on_error: None,
            // Shell command fields
            shell_command: None,
            shell_command_id: None,
            shell_command_working_directory: None,
            shell_command_fail_on_error: None,
            // API request fields
            api_method: None,
            api_url: None,
            api_headers: None,
            api_body: None,
            api_content_type: None,
            api_output_variable: None,
            api_extractions: None,
            api_timeout_ms: None,
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            check_url: None,
            expected_status: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
            // Log watch fields
            log_sources: None,
            time_window_seconds: None,
            error_patterns: None,
        }
    }

    // ========================================================================
    // AWAS Step Constructors
    // ========================================================================

    /// Create an AWAS discover step
    pub fn awas_discover(url: &str) -> Self {
        Self {
            step_type: "awas_discover".to_string(),
            name: Some(format!("AWAS Discover: {}", url)),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: false,
            screenshot_delay: 0,
            screenshot_monitor: None,
            playwright_script_id: None,
            playwright_script_content: None,
            playwright_target_url: None,
            prompt_content: None,
            timeout_seconds: None, // No timeout by default
            initial_state_ids: None,
            is_setup: Some(true), // AWAS discover is typically setup
            phase: None,
            run_on_subsequent_iterations: Some(false), // Usually only discover once
            test_id: None,
            test_type: None,
            test_is_critical: None,
            sub_step_id: None,
            // AWAS fields
            awas_url: Some(url.to_string()),
            awas_action_id: None,
            awas_params: None,
            awas_html: None,
            awas_base_url: None,
            // MCP fields
            mcp_server_id: None,
            mcp_server_name: None,
            mcp_tool_name: None,
            mcp_arguments: None,
            mcp_fail_on_error: None,
            // Shell command fields
            shell_command: None,
            shell_command_id: None,
            shell_command_working_directory: None,
            shell_command_fail_on_error: None,
            // API request fields
            api_method: None,
            api_url: None,
            api_headers: None,
            api_body: None,
            api_content_type: None,
            api_output_variable: None,
            api_extractions: None,
            api_timeout_ms: None,
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            check_url: None,
            expected_status: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
            // Log watch fields
            log_sources: None,
            time_window_seconds: None,
            error_patterns: None,
        }
    }

    /// Create an AWAS execute step
    pub fn awas_execute(url: &str, action_id: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            step_type: "awas_execute".to_string(),
            name: Some(format!("AWAS Execute: {}", action_id)),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: false,
            screenshot_delay: 0,
            screenshot_monitor: None,
            playwright_script_id: None,
            playwright_script_content: None,
            playwright_target_url: None,
            prompt_content: None,
            timeout_seconds: None, // No timeout by default
            initial_state_ids: None,
            is_setup: Some(false), // AWAS execute is typically an action step
            phase: None,
            run_on_subsequent_iterations: Some(true),
            test_id: None,
            test_type: None,
            test_is_critical: None,
            sub_step_id: None,
            // AWAS fields
            awas_url: Some(url.to_string()),
            awas_action_id: Some(action_id.to_string()),
            awas_params: params,
            awas_html: None,
            awas_base_url: None,
            // MCP fields
            mcp_server_id: None,
            mcp_server_name: None,
            mcp_tool_name: None,
            mcp_arguments: None,
            mcp_fail_on_error: None,
            // Shell command fields
            shell_command: None,
            shell_command_id: None,
            shell_command_working_directory: None,
            shell_command_fail_on_error: None,
            // API request fields
            api_method: None,
            api_url: None,
            api_headers: None,
            api_body: None,
            api_content_type: None,
            api_output_variable: None,
            api_extractions: None,
            api_timeout_ms: None,
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            check_url: None,
            expected_status: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
            // Log watch fields
            log_sources: None,
            time_window_seconds: None,
            error_patterns: None,
        }
    }

    /// Create an AWAS check support step
    pub fn awas_check_support(url: &str) -> Self {
        Self {
            step_type: "awas_check_support".to_string(),
            name: Some(format!("AWAS Check Support: {}", url)),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: false,
            screenshot_delay: 0,
            screenshot_monitor: None,
            playwright_script_id: None,
            playwright_script_content: None,
            playwright_target_url: None,
            prompt_content: None,
            timeout_seconds: None, // No timeout by default
            initial_state_ids: None,
            is_setup: Some(true), // Check support is typically setup
            phase: None,
            run_on_subsequent_iterations: Some(false),
            test_id: None,
            test_type: None,
            test_is_critical: None,
            sub_step_id: None,
            // AWAS fields
            awas_url: Some(url.to_string()),
            awas_action_id: None,
            awas_params: None,
            awas_html: None,
            awas_base_url: None,
            // MCP fields
            mcp_server_id: None,
            mcp_server_name: None,
            mcp_tool_name: None,
            mcp_arguments: None,
            mcp_fail_on_error: None,
            // Shell command fields
            shell_command: None,
            shell_command_id: None,
            shell_command_working_directory: None,
            shell_command_fail_on_error: None,
            // API request fields
            api_method: None,
            api_url: None,
            api_headers: None,
            api_body: None,
            api_content_type: None,
            api_output_variable: None,
            api_extractions: None,
            api_timeout_ms: None,
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            check_url: None,
            expected_status: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
            // Log watch fields
            log_sources: None,
            time_window_seconds: None,
            error_patterns: None,
        }
    }

    /// Create an AWAS list actions step
    pub fn awas_list_actions() -> Self {
        Self {
            step_type: "awas_list_actions".to_string(),
            name: Some("AWAS List Actions".to_string()),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: false,
            screenshot_delay: 0,
            screenshot_monitor: None,
            playwright_script_id: None,
            playwright_script_content: None,
            playwright_target_url: None,
            prompt_content: None,
            timeout_seconds: None, // No timeout by default
            initial_state_ids: None,
            is_setup: Some(false),
            phase: None,
            run_on_subsequent_iterations: Some(true),
            test_id: None,
            test_type: None,
            test_is_critical: None,
            sub_step_id: None,
            // AWAS fields
            awas_url: None,
            awas_action_id: None,
            awas_params: None,
            awas_html: None,
            awas_base_url: None,
            // MCP fields
            mcp_server_id: None,
            mcp_server_name: None,
            mcp_tool_name: None,
            mcp_arguments: None,
            mcp_fail_on_error: None,
            // Shell command fields
            shell_command: None,
            shell_command_id: None,
            shell_command_working_directory: None,
            shell_command_fail_on_error: None,
            // API request fields
            api_method: None,
            api_url: None,
            api_headers: None,
            api_body: None,
            api_content_type: None,
            api_output_variable: None,
            api_extractions: None,
            api_timeout_ms: None,
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            check_url: None,
            expected_status: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
            // Log watch fields
            log_sources: None,
            time_window_seconds: None,
            error_patterns: None,
        }
    }

    /// Create an AWAS extract elements step
    pub fn awas_extract_elements(html: &str, base_url: Option<&str>) -> Self {
        Self {
            step_type: "awas_extract_elements".to_string(),
            name: Some("AWAS Extract Elements".to_string()),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: false,
            screenshot_delay: 0,
            screenshot_monitor: None,
            playwright_script_id: None,
            playwright_script_content: None,
            playwright_target_url: None,
            prompt_content: None,
            timeout_seconds: None, // No timeout by default
            initial_state_ids: None,
            is_setup: Some(false),
            phase: None,
            run_on_subsequent_iterations: Some(true),
            test_id: None,
            test_type: None,
            test_is_critical: None,
            sub_step_id: None,
            // AWAS fields
            awas_url: None,
            awas_action_id: None,
            awas_params: None,
            awas_html: Some(html.to_string()),
            awas_base_url: base_url.map(|s| s.to_string()),
            // MCP fields
            mcp_server_id: None,
            mcp_server_name: None,
            mcp_tool_name: None,
            mcp_arguments: None,
            mcp_fail_on_error: None,
            // Shell command fields
            shell_command: None,
            shell_command_id: None,
            shell_command_working_directory: None,
            shell_command_fail_on_error: None,
            // API request fields
            api_method: None,
            api_url: None,
            api_headers: None,
            api_body: None,
            api_content_type: None,
            api_output_variable: None,
            api_extractions: None,
            api_timeout_ms: None,
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            check_url: None,
            expected_status: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
            // Log watch fields
            log_sources: None,
            time_window_seconds: None,
            error_patterns: None,
        }
    }

    // ========================================================================
    // MCP Call Step Constructor
    // ========================================================================

    /// Create an MCP call step
    pub fn mcp_call(
        server_id: &str,
        tool_name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Self {
        Self {
            step_type: "mcp_call".to_string(),
            name: Some(format!("MCP Call: {}", tool_name)),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: false,
            screenshot_delay: 0,
            screenshot_monitor: None,
            playwright_script_id: None,
            playwright_script_content: None,
            playwright_target_url: None,
            prompt_content: None,
            timeout_seconds: None, // No timeout by default
            initial_state_ids: None,
            is_setup: Some(false),
            phase: None,
            run_on_subsequent_iterations: Some(true),
            test_id: None,
            test_type: None,
            test_is_critical: None,
            sub_step_id: None,
            // AWAS fields
            awas_url: None,
            awas_action_id: None,
            awas_params: None,
            awas_html: None,
            awas_base_url: None,
            // MCP fields
            mcp_server_id: Some(server_id.to_string()),
            mcp_server_name: None,
            mcp_tool_name: Some(tool_name.to_string()),
            mcp_arguments: arguments,
            mcp_fail_on_error: Some(true),
            // Shell command fields
            shell_command: None,
            shell_command_id: None,
            shell_command_working_directory: None,
            shell_command_fail_on_error: None,
            // API request fields
            api_method: None,
            api_url: None,
            api_headers: None,
            api_body: None,
            api_content_type: None,
            api_output_variable: None,
            api_extractions: None,
            api_timeout_ms: None,
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            check_url: None,
            expected_status: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
            // Log watch fields
            log_sources: None,
            time_window_seconds: None,
            error_patterns: None,
        }
    }

    /// Create a macro step (runs a saved macro by ID)
    pub fn macro_step(macro_id: &str, name: Option<&str>) -> Self {
        Self {
            step_type: "macro".to_string(),
            name: name.map(|n| n.to_string()),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: false,
            screenshot_delay: 0,
            screenshot_monitor: None,
            playwright_script_id: None,
            playwright_script_content: None,
            playwright_target_url: None,
            prompt_content: None,
            timeout_seconds: None, // No timeout by default
            initial_state_ids: None,
            is_setup: Some(true), // Macro is setup by default
            phase: None,
            run_on_subsequent_iterations: Some(true),
            test_id: None,
            test_type: None,
            test_is_critical: None,
            sub_step_id: None,
            // AWAS fields
            awas_url: None,
            awas_action_id: None,
            awas_params: None,
            awas_html: None,
            awas_base_url: None,
            // MCP fields
            mcp_server_id: None,
            mcp_server_name: None,
            mcp_tool_name: None,
            mcp_arguments: None,
            mcp_fail_on_error: None,
            // Shell command fields
            shell_command: None,
            shell_command_id: None,
            shell_command_working_directory: None,
            shell_command_fail_on_error: None,
            // API request fields
            api_method: None,
            api_url: None,
            api_headers: None,
            api_body: None,
            api_content_type: None,
            api_output_variable: None,
            api_extractions: None,
            api_timeout_ms: None,
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            check_url: None,
            expected_status: None,
            // Macro fields
            macro_id: Some(macro_id.to_string()),
            // Check group fields
            check_group_id: None,
            // Log watch fields
            log_sources: None,
            time_window_seconds: None,
            error_patterns: None,
        }
    }

    /// Create a log_watch step for automatic error detection.
    ///
    /// This step scans development log files for errors within a time window.
    /// Used by the automatic log watch feature when `log_watch_enabled` is true.
    pub fn log_watch(name: &str, log_sources: Vec<String>, time_window_seconds: u64) -> Self {
        Self {
            step_type: "log_watch".to_string(),
            name: Some(name.to_string()),
            phase: Some("verification".to_string()),
            log_sources: Some(log_sources),
            time_window_seconds: Some(time_window_seconds),
            // Log watch is informative by default - doesn't block the workflow
            test_is_critical: Some(false),
            run_on_subsequent_iterations: Some(true),
            ..Default::default()
        }
    }

    /// Create the default log_watch step used when `log_watch_enabled` is true.
    ///
    /// This creates a step that:
    /// - Monitors log sources from global settings (Settings > Log Sources)
    /// - Scans the last 60 seconds for errors
    /// - Is non-critical (won't fail the workflow, just reports errors)
    pub fn default_log_watch() -> Self {
        Self::log_watch(
            "Check for runtime errors",
            get_default_log_source_names(),
            60,
        )
    }

    /// Create a health check step for verifying server availability.
    ///
    /// This step makes an HTTP request to a URL and checks for the expected status code.
    /// Used by the automatic health check feature when `health_check_enabled` is true.
    pub fn health_check(
        name: &str,
        url: &str,
        expected_status: u16,
        timeout_seconds: u64,
        is_critical: bool,
    ) -> Self {
        Self {
            step_type: "check".to_string(),
            name: Some(name.to_string()),
            phase: Some("verification".to_string()),
            check_type: Some("http_status".to_string()),
            check_url: Some(url.to_string()),
            expected_status: Some(expected_status),
            timeout_seconds: Some(timeout_seconds),
            test_is_critical: Some(is_critical),
            run_on_subsequent_iterations: Some(true),
            ..Default::default()
        }
    }

    /// Check if this step should run based on the current iteration number
    /// Returns true if the step should be executed, false if it should be skipped
    pub fn should_run_on_iteration(&self, iteration: u32) -> bool {
        // First iteration always runs all steps
        if iteration <= 1 {
            return true;
        }

        // For subsequent iterations, check if the step is configured to run
        // Default: all steps run on each iteration for fresh data
        // Users can explicitly set run_on_subsequent_iterations: false to skip on subsequent iterations
        self.run_on_subsequent_iterations.unwrap_or(true)
    }
}

/// Result of executing a single step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    /// Step index (0-based)
    pub step_index: usize,
    /// Step type that was executed
    pub step_type: String,
    /// Step name for display
    pub step_name: String,
    /// Whether the step succeeded
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
    /// Path to screenshot if captured
    pub screenshot_path: Option<String>,
    /// When this step started (ISO 8601 timestamp)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When this step ended (ISO 8601 timestamp)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Step configuration (for AI visibility)
    pub config: StepExecutionConfig,
    /// Verification-specific fields (for test/check steps in verification phase)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_details: Option<VerificationStepDetails>,
}

/// Verification-specific details for test and check steps
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VerificationStepDetails {
    /// Step ID from the workflow
    pub step_id: String,
    /// Phase this step belongs to
    pub phase: String,
    /// Whether this is a critical step (failure stops execution)
    pub is_critical: bool,
    /// Whether this is a blocking step (failure prevents agentic phase from continuing)
    pub is_blocking: bool,
    /// Standard output from the step
    pub stdout: Option<String>,
    /// Standard error from the step
    pub stderr: Option<String>,
    /// For test steps: number of assertions passed
    pub assertions_passed: Option<u32>,
    /// For test steps: total number of assertions
    pub assertions_total: Option<u32>,
    /// For test steps: console output from browser/runtime
    pub console_output: Option<String>,
    /// For Playwright tests: page snapshot (YAML accessibility tree)
    pub page_snapshot: Option<String>,
    /// Exit code from command execution
    pub exit_code: Option<i32>,
    /// For check_group steps: individual check results with details
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_results: Option<Vec<IndividualCheckResult>>,
}

/// Individual check result within a check group
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndividualCheckResult {
    /// Check name
    pub name: String,
    /// Status: "passed", "failed", "skipped"
    pub status: String,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Number of issues found
    pub issues_found: u32,
    /// Number of issues fixed (if auto-fix is enabled)
    pub issues_fixed: u32,
    /// Number of files checked
    pub files_checked: u32,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Raw output from the check tool
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Individual issues found (limited to avoid huge payloads)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<CheckIssueDetail>,
}

/// Details of an individual issue found by a check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckIssueDetail {
    /// File path where the issue was found
    pub file: String,
    /// Line number (1-based)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Column number (1-based)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// Rule code (e.g., "E501", "no-unused-vars")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Issue message
    pub message: String,
    /// Severity level: "error", "warning", "info"
    pub severity: String,
    /// Whether this issue is fixable
    #[serde(default)]
    pub fixable: bool,
}

/// Step configuration captured for AI visibility
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepExecutionConfig {
    /// For action steps: "click", "double_click", "right_click"
    pub action_type: Option<String>,
    /// Target image ID for action steps
    pub target_image_id: Option<String>,
    /// Target image name for display
    pub target_image_name: Option<String>,
    /// Monitor index (0 = primary)
    pub monitor_index: Option<i32>,
    /// Delay before screenshot in seconds
    pub screenshot_delay: Option<u32>,
    /// Timeout for this step in seconds
    pub timeout_seconds: Option<u64>,
    /// Playwright script ID
    pub playwright_script_id: Option<String>,
    /// Initial state IDs for workflow steps
    pub initial_state_ids: Option<Vec<String>>,
    /// For check steps: "lint", "format", "typecheck", "analyze", "security", "custom_command"
    pub check_type: Option<String>,
    /// Shell command or check command
    pub command: Option<String>,
    /// Test ID for verification test steps
    pub test_id: Option<String>,
    /// Test type for test steps: "repository", "playwright", etc.
    pub test_type: Option<String>,
    /// Working directory for shell commands and checks
    pub working_directory: Option<String>,
}

/// Result of executing all steps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Whether all steps completed successfully
    pub success: bool,
    /// Total number of steps
    pub total_steps: usize,
    /// Number of successful steps
    pub successful_steps: usize,
    /// Number of failed steps
    pub failed_steps: usize,
    /// Total execution time in milliseconds
    pub total_duration_ms: u64,
    /// Individual step results
    pub steps: Vec<StepExecutionResult>,
    /// Logs captured during execution (from .dev-logs/)
    #[serde(default)]
    pub captured_logs: Option<CapturedLogs>,
    /// Runner logs captured during execution (GUI automation events)
    #[serde(default)]
    pub captured_runner_logs: Option<CapturedRunnerLogs>,
    /// Whether verification passed (for unified workflows)
    #[serde(default)]
    pub verification_passed: Option<bool>,
    /// Loop/iteration details (for unified workflows)
    #[serde(default)]
    pub loop_result: Option<crate::unified_workflow_executor::LoopResult>,
    /// Task summary (AI-generated)
    #[serde(default)]
    pub task_summary: Option<String>,
}

/// Result of running all verification_steps in a unified workflow
///
/// This is returned by execute_verification_steps and used to:
/// 1. Determine if the agentic phase should run (any failures)
/// 2. Build context for the AI about what failed
/// 3. Store results in the database for the Recap page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationPhaseResult {
    /// Iteration number (1-indexed)
    pub iteration: u32,
    /// Whether all verification steps passed
    pub all_passed: bool,
    /// Total number of verification steps
    pub total_steps: usize,
    /// Number of steps that passed
    pub passed_steps: usize,
    /// Number of steps that failed
    pub failed_steps: usize,
    /// Number of steps that were skipped (due to critical step failure)
    pub skipped_steps: usize,
    /// Total execution time in milliseconds
    pub total_duration_ms: u64,
    /// Individual step results
    pub step_results: Vec<StepExecutionResult>,
    /// Whether a critical step failed (should stop execution)
    pub critical_failure: bool,
}

impl VerificationPhaseResult {
    /// Build a failure context string for the agentic phase
    ///
    /// This summarizes what failed so the AI knows what to work on.
    pub fn build_failure_context(&self) -> String {
        if self.all_passed {
            return String::new();
        }

        let mut context = String::new();
        context.push_str("## Verification Results\n\n");
        context.push_str(&format!(
            "**Status:** {} of {} verification steps passed\n\n",
            self.passed_steps, self.total_steps
        ));

        // List failed steps with details
        context.push_str("### Failed Steps\n\n");
        for result in &self.step_results {
            if !result.success {
                context.push_str(&format!(
                    "#### {} ({})\n",
                    result.step_name, result.step_type
                ));

                if let Some(error) = &result.error {
                    context.push_str(&format!("**Error:** {}\n", error));
                }

                if let Some(details) = &result.verification_details {
                    if let Some(stdout) = &details.stdout {
                        if !stdout.is_empty() {
                            // Truncate long output
                            let truncated = if stdout.len() > 2000 {
                                format!(
                                    "{}...\n[truncated, {} more chars]",
                                    &stdout[..2000],
                                    stdout.len() - 2000
                                )
                            } else {
                                stdout.clone()
                            };
                            context.push_str(&format!("**Output:**\n```\n{}\n```\n", truncated));
                        }
                    }
                    if let Some(stderr) = &details.stderr {
                        if !stderr.is_empty() {
                            let truncated = if stderr.len() > 1000 {
                                format!("{}...\n[truncated]", &stderr[..1000])
                            } else {
                                stderr.clone()
                            };
                            context.push_str(&format!("**Stderr:**\n```\n{}\n```\n", truncated));
                        }
                    }
                    if let Some(passed) = details.assertions_passed {
                        if let Some(total) = details.assertions_total {
                            context.push_str(&format!(
                                "**Assertions:** {}/{} passed\n",
                                passed, total
                            ));
                        }
                    }
                }
                context.push('\n');
            }
        }

        // List passed steps briefly
        let passed: Vec<_> = self.step_results.iter().filter(|r| r.success).collect();
        if !passed.is_empty() {
            context.push_str("### Passed Steps\n\n");
            for result in passed {
                context.push_str(&format!(
                    "- ✓ {} ({}ms)\n",
                    result.step_name, result.duration_ms
                ));
            }
        }

        context
    }

    /// Build a brief summary for logging
    pub fn summary(&self) -> String {
        if self.all_passed {
            format!(
                "Verification PASSED: {}/{} steps in {}ms",
                self.passed_steps, self.total_steps, self.total_duration_ms
            )
        } else {
            format!(
                "Verification FAILED: {}/{} steps passed, {} failed in {}ms{}",
                self.passed_steps,
                self.total_steps,
                self.failed_steps,
                self.total_duration_ms,
                if self.critical_failure {
                    " (CRITICAL)"
                } else {
                    ""
                }
            )
        }
    }
}

/// A log source configuration (passed from frontend)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSourceConfig {
    /// Unique identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Absolute path to the log file
    pub path: String,
    /// Whether this source is enabled
    pub enabled: bool,
}

/// Logs captured from application log files during automation
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapturedLogs {
    /// Log entries per source (keyed by source name)
    pub sources: HashMap<String, String>,
}

/// Runner logs captured during automation (GUI automation + Playwright)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapturedRunnerLogs {
    /// Action/workflow execution events (from runner-actions.jsonl)
    pub actions: Vec<ActionEvent>,
    /// Image recognition events (from runner-image-recognition.jsonl)
    pub image_recognition: Vec<ImageRecognitionEvent>,
}

// ============================================================================
// Log Watch Types
// ============================================================================

/// An error detected in a log file during log_watch step execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogError {
    /// Source log file name (e.g., "backend.log")
    pub source: String,
    /// Line number in the log file (1-indexed)
    pub line_number: usize,
    /// Timestamp extracted from the log line (if available)
    pub timestamp: Option<String>,
    /// The error message/line
    pub message: String,
    /// Context lines before the error (typically 2-3 lines)
    pub context_before: Vec<String>,
    /// Context lines after the error (typically 2-3 lines)
    pub context_after: Vec<String>,
    /// Type of error: "error", "exception", "traceback", "warning", "fatal", "panic"
    pub error_type: String,
}

/// Default error patterns used for log_watch if none specified
pub(crate) const DEFAULT_ERROR_PATTERNS: &[&str] = &[
    "ERROR",
    "Error:",
    "error:",
    "Exception",
    "exception",
    "Traceback",
    "traceback",
    "TypeError",
    "SyntaxError",
    "ReferenceError",
    "ValueError",
    "KeyError",
    "AttributeError",
    "ImportError",
    "RuntimeError",
    "FATAL",
    "fatal",
    "panic",
    "PANIC",
    "FAILED",
    "Failed:",
];

/// Get default log source filenames from global settings.
/// Falls back to ["backend.log", "frontend.log"] if no sources are configured.
pub(crate) fn get_default_log_source_names() -> Vec<String> {
    let settings = crate::settings::get_global_log_source_settings();
    let names: Vec<String> = settings
        .sources
        .iter()
        .filter(|s| s.enabled)
        .map(|s| {
            // If path is absolute, extract the filename; otherwise use as-is
            let path = std::path::Path::new(&s.path);
            if path.is_absolute() {
                path.file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| s.path.clone())
            } else {
                s.path.clone()
            }
        })
        .collect();
    if names.is_empty() {
        vec!["backend.log".to_string(), "frontend.log".to_string()]
    } else {
        names
    }
}

/// Default time window in seconds
pub(crate) const DEFAULT_TIME_WINDOW_SECONDS: u64 = 60;

/// Number of context lines before/after an error
pub(crate) const CONTEXT_LINES: usize = 3;

impl ExecutionResult {
    /// Generate a markdown summary of the execution results
    pub fn to_markdown_summary(&self) -> String {
        if self.steps.is_empty() {
            return String::new();
        }

        let mut summary = String::new();
        summary.push_str("\n## Pre-Execution Results\n\n");
        summary.push_str("The following steps were executed deterministically by the runner:\n\n");

        for result in &self.steps {
            summary.push_str(&format!(
                "{}. **{}** ({}): {} in {}ms\n",
                result.step_index + 1,
                result.step_name,
                result.step_type,
                if result.success {
                    "Success".to_string()
                } else {
                    format!(
                        "Failed - {}",
                        result.error.as_deref().unwrap_or("unknown error")
                    )
                },
                result.duration_ms
            ));

            if let Some(ref path) = result.screenshot_path {
                summary.push_str(&format!("   Screenshot: `{}`\n", path));
            }
        }

        summary.push_str(&format!(
            "\n**Summary:** {} of {} steps completed successfully.\n",
            self.successful_steps, self.total_steps
        ));

        if self.failed_steps > 0 {
            summary.push_str("\n**Note:** Some steps failed. Please analyze the errors above.\n");
        }

        // Include captured logs if any
        if let Some(ref logs) = self.captured_logs {
            if !logs.sources.is_empty() {
                summary.push_str("\n## Application Logs (Captured During Automation)\n\n");

                for (name, content) in &logs.sources {
                    if !content.trim().is_empty() {
                        summary.push_str(&format!("### {} Logs\n\n```\n", name));
                        // Limit to last 100 lines to avoid overwhelming the AI
                        let lines: Vec<&str> = content.lines().collect();
                        let start = if lines.len() > 100 {
                            lines.len() - 100
                        } else {
                            0
                        };
                        for line in &lines[start..] {
                            summary.push_str(line);
                            summary.push('\n');
                        }
                        summary.push_str("```\n\n");
                    }
                }
            }
        }

        summary
    }
}

/// Step Executor - executes automation steps using UnifiedActionService
pub struct StepExecutor {
    action_service: UnifiedActionService,
    app_state: Arc<AppState>,
    /// Configuration storage for loading saved configs
    config_storage: Arc<TokioMutex<ConfigStorage>>,
    /// Optional app handle for emitting events to the Tauri frontend
    app_handle: Option<tauri::AppHandle>,
    /// Optional task run ID for database logging (AWAS steps, etc.)
    task_run_id: Option<String>,
    /// Runtime context for variable expansion in commands
    runtime_context: RuntimeContext,
    /// Shared variable store for API request chaining (thread-safe, clone-friendly)
    shared_variables: SharedVariableStore,
    /// Registry of step handlers for polymorphic dispatch
    handler_registry: HandlerRegistry,
}

impl StepExecutor {
    /// Create a new StepExecutor
    pub fn new(app_state: Arc<AppState>, config_storage: Arc<TokioMutex<ConfigStorage>>) -> Self {
        Self {
            action_service: UnifiedActionService::new(app_state.clone(), config_storage.clone()),
            app_state,
            config_storage,
            app_handle: None,
            task_run_id: None,
            runtime_context: RuntimeContext::new(),
            shared_variables: SharedVariableStore::new(),
            handler_registry: HandlerRegistry::with_standard_handlers(),
        }
    }

    /// Create a new StepExecutor with an app handle for frontend event emission
    pub fn with_app_handle(
        app_state: Arc<AppState>,
        config_storage: Arc<TokioMutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            action_service: UnifiedActionService::new(app_state.clone(), config_storage.clone()),
            app_state,
            config_storage,
            app_handle: Some(app_handle),
            task_run_id: None,
            runtime_context: RuntimeContext::new(),
            shared_variables: SharedVariableStore::new(),
            handler_registry: HandlerRegistry::with_standard_handlers(),
        }
    }

    /// Set the task run ID for database logging
    ///
    /// When set, AWAS step results will be saved to the database.
    pub fn with_task_run_id(mut self, task_run_id: String) -> Self {
        self.runtime_context = RuntimeContext::with_task_run_id(&task_run_id);
        self.task_run_id = Some(task_run_id);
        self
    }

    /// Set a variable in the runtime context for variable expansion in commands.
    ///
    /// Variables can be referenced in shell commands using `{{variable_name}}` syntax.
    pub fn set_context_variable(&mut self, name: &str, value: serde_json::Value) {
        self.runtime_context.set_variable(name, value);
    }

    /// Get the runtime context (for advanced use cases).
    pub fn runtime_context(&self) -> &RuntimeContext {
        &self.runtime_context
    }

    /// Get a mutable reference to the runtime context (for advanced use cases).
    pub fn runtime_context_mut(&mut self) -> &mut RuntimeContext {
        &mut self.runtime_context
    }

    /// Get the shared variable store.
    pub fn shared_variables(&self) -> &SharedVariableStore {
        &self.shared_variables
    }

    /// Create a HandlerContext for executing steps via the handler system.
    ///
    /// This shares the executor's state (runtime_context, shared_variables)
    /// with the handlers to maintain consistency during step execution.
    fn create_handler_context(&self) -> HandlerContext {
        HandlerContext::with_shared_state(
            self.app_state.clone(),
            self.config_storage.clone(),
            self.app_handle.clone(),
            self.runtime_context.clone(),
            self.shared_variables.clone(),
            self.task_run_id.clone(),
        )
    }

    /// Expand shared variables in a string.
    ///
    /// Replaces `{{variable_name}}` patterns with values from the shared variable store.
    /// This is used for API request chaining where response data from one request
    /// can be referenced in subsequent requests.
    fn expand_with_shared_vars(&self, text: &str) -> String {
        use once_cell::sync::Lazy;
        static VAR_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"\{\{([^}]+)\}\}").unwrap());

        let mut result = text.to_string();
        for cap in VAR_PATTERN.captures_iter(text) {
            let var_name = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            if let Some(value) = self.shared_variables.get(var_name) {
                result = result.replace(&cap[0], &value);
            }
        }
        result
    }

    /// Log a step execution event to the database
    ///
    /// This logs step start, complete, and error events to the task_run_events table.
    ///
    /// Note: For workflow sequence children (e.g., workflow-sequence-X-workflow-N),
    /// the task_run_id is automatically remapped to the parent task ID because
    /// only parent IDs exist in task_runs (required by foreign key constraint).
    fn log_step_event(
        &self,
        task_run_id: &str,
        step: &ExecutionStepConfig,
        step_index: usize,
        event_subtype: &str,
        message: &str,
        duration_ms: Option<i64>,
        error: Option<&str>,
        exit_code: Option<i32>,
        stdout: Option<&str>,
        stderr: Option<&str>,
    ) {
        // For workflow sequence children, remap to parent ID to satisfy FK constraint
        let parent_id = get_parent_task_id(task_run_id);
        let step_name = step.name.clone().unwrap_or_else(|| step.step_type.clone());

        // Generate action_id for consistent event aggregation
        // Format matches StepEventBuilder: {phase}-{step_type}-{task_run_id}-{step_index}
        // This ensures start/complete events for the same step are merged in the Timeline
        let phase = step.phase.as_deref().unwrap_or("setup");
        let action_id = format!("{}-{}-{}-{}", phase, step.step_type, parent_id, step_index);

        // Build data JSON with step details (include original task_run_id for context)
        let data = json!({
            "step_index": step_index,
            "step_type": step.step_type,
            "step_name": step_name,
            "phase": step.phase,
            "original_task_run_id": task_run_id,  // Keep original ID for debugging
            "command": step.shell_command.as_ref().or(step.check_command.as_ref()),
            "working_directory": step.shell_command_working_directory.as_ref().or(step.check_working_directory.as_ref()),
            "exit_code": exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "error": error,
            "playwright_script_id": step.playwright_script_id,
            "target_image_name": step.target_image_name,
            "action_type": step.action_type,
        });

        let event_input = CreateTaskRunEventInput {
            task_run_id: parent_id, // Use parent ID for FK constraint
            event_type: "step_execution".to_string(),
            event_subtype: Some(event_subtype.to_string()),
            message: message.to_string(),
            data: Some(serde_json::to_string(&data).unwrap_or_default()),
            workflow_name: None,
            state_name: None,
            action_id: Some(action_id),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms,
        };

        if let Err(e) = self
            .app_state
            .checkpoint_db
            .create_task_run_event(&event_input)
        {
            warn!("Failed to log step event: {}", e);
        }
    }

    /// Emit a tree event to the Tauri frontend (if app_handle is available)
    fn emit_tree_event(
        &self,
        event_type: &str,
        node: &serde_json::Value,
        timestamp: f64,
        sequence: u32,
    ) {
        use tauri::Emitter;
        if let Some(ref app_handle) = self.app_handle {
            let tree_event = json!({
                "type": "tree_event",
                "event_type": event_type,
                "node": node,
                "path": [],
                "timestamp": timestamp,
                "sequence": sequence,
            });
            if let Err(e) = app_handle.emit("executor-event", &tree_event) {
                warn!("Failed to emit tree event to frontend: {}", e);
            }
        }
    }

    /// Record a screenshot capture event to the RunRecordingHandler.
    ///
    /// This ensures screenshots captured directly by the step executor
    /// (not through Python) are still recorded in the automation logs.
    async fn record_screenshot_event(
        &self,
        screenshot_type: &str,
        file_path: &str,
        monitor: Option<i32>,
        delay_seconds: Option<u32>,
        success: bool,
        associated_action: Option<String>,
        error: Option<String>,
    ) {
        let monitor_str = monitor.map(|m| m.to_string());
        self.app_state
            .run_recording_handler
            .on_screenshot_captured(
                screenshot_type,
                file_path,
                monitor_str,
                delay_seconds,
                success,
                associated_action,
                error,
            )
            .await;
    }

    /// Execute a list of steps and return results
    ///
    /// This is the core execution function used by all consumers.
    /// Steps are executed in order, and execution continues even if a step fails
    /// (so the caller can see all results and decide how to proceed).
    pub async fn execute_steps(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
    ) -> ExecutionResult {
        self.execute_steps_with_log_sources(steps, execution_id, &[])
            .await
    }

    /// Execute steps for a specific iteration
    ///
    /// For iterations > 1, filters out setup steps that aren't marked to run on
    /// subsequent iterations. This is the iteration-aware version of execute_steps.
    ///
    /// For Playwright steps, all Playwright steps are combined (since Playwright
    /// closes the browser after each run). Setup Playwright scripts are run first,
    /// followed by verification Playwright scripts.
    pub async fn execute_steps_for_iteration(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
        iteration: u32,
    ) -> ExecutionResult {
        // Preprocess steps for iteration:
        // 1. Filter out setup steps that shouldn't run on subsequent iterations
        // 2. Combine Playwright steps for efficiency (setup + verification)
        let processed_steps = Self::preprocess_steps_for_iteration(steps, iteration);

        if processed_steps.len() != steps.len() {
            info!(
                "Iteration {}: Preprocessed {} steps to {} (filtered/combined)",
                iteration,
                steps.len(),
                processed_steps.len(),
            );
        }

        self.execute_steps_with_log_sources(&processed_steps, execution_id, log_sources)
            .await
    }

    /// Preprocess steps for a specific iteration
    ///
    /// This handles:
    /// 1. Filtering out setup steps that shouldn't run on subsequent iterations
    /// 2. For Playwright steps: combining multiple scripts into a single script
    ///    (setup scripts first, then verification scripts) since Playwright closes
    ///    the browser after each run
    fn preprocess_steps_for_iteration(
        steps: &[ExecutionStepConfig],
        iteration: u32,
    ) -> Vec<ExecutionStepConfig> {
        // Separate Playwright steps from other steps
        let (playwright_steps, other_steps): (Vec<_>, Vec<_>) =
            steps.iter().partition(|s| s.step_type == "playwright");

        // For first iteration, still combine Playwright scripts for efficiency
        // For subsequent iterations, also filter non-Playwright steps

        // Filter non-Playwright steps based on iteration
        let filtered_other_steps: Vec<ExecutionStepConfig> = if iteration <= 1 {
            other_steps.into_iter().cloned().collect()
        } else {
            other_steps
                .into_iter()
                .filter(|step| {
                    let should_run = step.should_run_on_iteration(iteration);
                    if !should_run {
                        info!(
                            "Iteration {}: Skipping setup step '{}' (type: {})",
                            iteration,
                            step.name.as_deref().unwrap_or("unnamed"),
                            step.step_type
                        );
                    }
                    should_run
                })
                .cloned()
                .collect()
        };

        // Combine Playwright steps if there are multiple
        let combined_playwright = Self::combine_playwright_steps(&playwright_steps);

        // Reconstruct steps in original order, but with combined Playwright step
        // placed at the position of the first Playwright step
        let mut result = Vec::new();
        let mut playwright_inserted = false;

        for step in steps {
            if step.step_type == "playwright" {
                if !playwright_inserted {
                    // Insert the combined Playwright step at the first Playwright position
                    if let Some(ref combined) = combined_playwright {
                        result.push(combined.clone());
                    }
                    playwright_inserted = true;
                }
                // Skip individual Playwright steps (they're now combined)
            } else {
                // Include non-Playwright steps that passed the filter
                if filtered_other_steps
                    .iter()
                    .any(|s| s.name == step.name && s.step_type == step.step_type)
                {
                    result.push(step.clone());
                }
            }
        }

        result
    }

    /// Combine multiple Playwright steps into a single step with combined script content
    ///
    /// This ensures all Playwright scripts run in the same browser session.
    /// Setup scripts are placed first, followed by verification scripts.
    fn combine_playwright_steps(steps: &[&ExecutionStepConfig]) -> Option<ExecutionStepConfig> {
        if steps.is_empty() {
            return None;
        }

        // If only one step, return it as-is
        if steps.len() == 1 {
            return Some(steps[0].clone());
        }

        // Separate setup and verification Playwright steps
        let (setup_steps, verification_steps): (
            Vec<&&ExecutionStepConfig>,
            Vec<&&ExecutionStepConfig>,
        ) = steps.iter().partition(|s| s.is_setup.unwrap_or(false));

        info!(
            "Combining {} Playwright scripts ({} setup, {} verification) into single script",
            steps.len(),
            setup_steps.len(),
            verification_steps.len()
        );

        // Collect script IDs for the combined step name
        let script_names: Vec<String> = steps.iter().filter_map(|s| s.name.clone()).collect();

        // Collect script contents (setup first, then verification)
        let mut combined_content_parts: Vec<String> = Vec::new();
        let mut target_url: Option<String> = None;
        let mut script_ids: Vec<String> = Vec::<String>::new();

        // Add setup scripts first
        for step in &setup_steps {
            if let Some(ref content) = step.playwright_script_content {
                combined_content_parts.push(format!(
                    "// === Setup: {} ===\n{}",
                    step.name.as_deref().unwrap_or("unnamed"),
                    content
                ));
            }
            if let Some(id) = &step.playwright_script_id {
                script_ids.push(id.to_string());
            }
            if target_url.is_none() {
                target_url = step.playwright_target_url.clone();
            }
        }

        // Add verification scripts
        for step in &verification_steps {
            if let Some(ref content) = step.playwright_script_content {
                combined_content_parts.push(format!(
                    "// === Verification: {} ===\n{}",
                    step.name.as_deref().unwrap_or("unnamed"),
                    content
                ));
            }
            if let Some(id) = &step.playwright_script_id {
                script_ids.push(id.to_string());
            }
            if target_url.is_none() {
                target_url = step.playwright_target_url.clone();
            }
        }

        // Create combined step
        Some(ExecutionStepConfig {
            step_type: "playwright".to_string(),
            name: Some(format!("Combined: {}", script_names.join(" + "))),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: steps.iter().any(|s| s.take_screenshot),
            screenshot_delay: steps.iter().map(|s| s.screenshot_delay).max().unwrap_or(0),
            screenshot_monitor: None,
            // Use the first script ID for fallback if no content is available
            playwright_script_id: script_ids.first().cloned(),
            // Combined script content
            playwright_script_content: if combined_content_parts.is_empty() {
                None
            } else {
                Some(combined_content_parts.join("\n\n"))
            },
            playwright_target_url: target_url,
            prompt_content: None,
            timeout_seconds: Some(
                steps
                    .iter()
                    .filter_map(|s| s.timeout_seconds)
                    .sum::<u64>()
                    .max(60),
            ),
            initial_state_ids: None,
            is_setup: Some(false), // Combined step is treated as verification
            phase: None,
            run_on_subsequent_iterations: Some(true), // Always runs
            test_id: None,
            test_type: None,
            test_is_critical: None,
            sub_step_id: None,
            // AWAS fields
            awas_url: None,
            awas_action_id: None,
            awas_params: None,
            awas_html: None,
            awas_base_url: None,
            // MCP fields
            mcp_server_id: None,
            mcp_server_name: None,
            mcp_tool_name: None,
            mcp_arguments: None,
            mcp_fail_on_error: None,
            // Shell command fields
            shell_command: None,
            shell_command_id: None,
            shell_command_working_directory: None,
            shell_command_fail_on_error: None,
            // API request fields
            api_method: None,
            api_url: None,
            api_headers: None,
            api_body: None,
            api_content_type: None,
            api_output_variable: None,
            api_extractions: None,
            api_timeout_ms: None,
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            check_url: None,
            expected_status: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
            // Log watch fields
            log_sources: None,
            time_window_seconds: None,
            error_patterns: None,
        })
    }

    /// Execute steps with log source configuration for log capture
    #[tracing::instrument(
        name = "workflow.steps.execute",
        skip(self, steps, log_sources),
        fields(
            step_count = %steps.len(),
            execution_id = %execution_id,
            log_source_count = %log_sources.len()
        )
    )]
    pub async fn execute_steps_with_log_sources(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
    ) -> ExecutionResult {
        let mut results = Vec::new();
        let total_start = std::time::Instant::now();

        if steps.is_empty() {
            return ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: results,
                captured_logs: None,
                captured_runner_logs: None,
                verification_passed: None,
                loop_result: None,
                task_summary: None,
            };
        }

        // Determine which logs are relevant based on step types
        let relevant_logs = RelevantLogSources::from_steps(steps);
        relevant_logs.log_relevance();

        // Record log file positions before execution (only for enabled sources)
        let log_positions = Self::capture_log_positions(log_sources);

        // Record runner log positions (only if GUI automation is relevant)
        let runner_log_positions = if relevant_logs.gui_automation {
            Self::capture_runner_log_positions()
        } else {
            HashMap::new()
        };

        info!(
            "Executing {} steps for execution {}",
            steps.len(),
            execution_id
        );

        // Get the task run ID for event logging (prefer self.task_run_id, fall back to execution_id)
        let log_task_run_id = self
            .task_run_id
            .clone()
            .unwrap_or_else(|| execution_id.to_string());

        for (index, step) in steps.iter().enumerate() {
            let step_name = step.name.clone().unwrap_or_else(|| step.step_type.clone());
            let start_time = std::time::Instant::now();
            let started_at = chrono::Utc::now().to_rfc3339();

            info!(
                "Executing step {}/{}: {} ({})",
                index + 1,
                steps.len(),
                step_name,
                step.step_type
            );

            // Log step start event
            self.log_step_event(
                &log_task_run_id,
                step,
                index,
                "start",
                &format!(
                    "Starting step {}/{}: {} ({})",
                    index + 1,
                    steps.len(),
                    step_name,
                    step.step_type
                ),
                None,
                None,
                None,
                None,
                None,
            );

            let (success, error, screenshot_path) = self.execute_single_step(step).await;

            // Take post-step screenshot if requested (and step succeeded)
            let final_screenshot =
                if step.take_screenshot && success && step.step_type != "screenshot" {
                    self.capture_post_step_screenshot(step)
                        .await
                        .or(screenshot_path)
                } else {
                    screenshot_path
                };

            let duration_ms = start_time.elapsed().as_millis() as u64;

            if success {
                info!(
                    "Step {}/{} completed successfully in {}ms",
                    index + 1,
                    steps.len(),
                    duration_ms
                );
                // Log step completion event
                self.log_step_event(
                    &log_task_run_id,
                    step,
                    index,
                    "complete",
                    &format!(
                        "Step {}/{} completed successfully in {}ms",
                        index + 1,
                        steps.len(),
                        duration_ms
                    ),
                    Some(duration_ms as i64),
                    None,
                    None,
                    None,
                    None,
                );
            } else {
                warn!("Step {}/{} failed: {:?}", index + 1, steps.len(), error);
                // Log step error event
                self.log_step_event(
                    &log_task_run_id,
                    step,
                    index,
                    "error",
                    &format!("Step {}/{} failed: {:?}", index + 1, steps.len(), error),
                    Some(duration_ms as i64),
                    error.as_deref(),
                    None,
                    None,
                    None,
                );
            }

            let ended_at = chrono::Utc::now().to_rfc3339();

            results.push(StepExecutionResult {
                step_index: index,
                step_type: step.step_type.clone(),
                step_name,
                success,
                error,
                screenshot_path: final_screenshot,
                started_at: Some(started_at),
                ended_at: Some(ended_at),
                duration_ms,
                config: StepExecutionConfig {
                    action_type: step.action_type.clone(),
                    target_image_id: step.target_image_id.clone(),
                    target_image_name: step.target_image_name.clone(),
                    monitor_index: step.monitor_index,
                    screenshot_delay: if step.screenshot_delay > 0 {
                        Some(step.screenshot_delay)
                    } else {
                        None
                    },
                    timeout_seconds: step.timeout_seconds,
                    playwright_script_id: step.playwright_script_id.clone(),
                    initial_state_ids: step.initial_state_ids.clone(),
                    check_type: step.check_type.clone(),
                    command: step
                        .check_command
                        .clone()
                        .or_else(|| step.shell_command.clone()),
                    test_id: step.test_id.clone(),
                    test_type: step.test_type.clone(),
                    working_directory: step
                        .check_working_directory
                        .clone()
                        .or_else(|| step.shell_command_working_directory.clone()),
                },
                verification_details: None,
            });
        }

        let successful_steps = results.iter().filter(|r| r.success).count();
        let failed_steps = results.len() - successful_steps;

        info!(
            "Completed {} steps: {} succeeded, {} failed",
            results.len(),
            successful_steps,
            failed_steps
        );

        // Capture logs that were written during execution
        let captured_logs = Self::capture_logs_since(log_sources, log_positions);

        // Capture runner logs (only if GUI automation was relevant)
        let captured_runner_logs = if relevant_logs.gui_automation {
            Self::capture_runner_logs_since(runner_log_positions)
        } else {
            None
        };

        ExecutionResult {
            success: failed_steps == 0,
            total_steps: results.len(),
            successful_steps,
            failed_steps,
            total_duration_ms: total_start.elapsed().as_millis() as u64,
            steps: results,
            captured_logs,
            captured_runner_logs,
            verification_passed: None,
            loop_result: None,
            task_summary: None,
        }
    }

    // ========================================================================
    // Phase-Based Execution Methods
    // ========================================================================

    /// Filter steps by phase
    pub fn filter_steps_by_phase(
        steps: &[ExecutionStepConfig],
        phase: &str,
    ) -> Vec<ExecutionStepConfig> {
        steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some(phase))
            .cloned()
            .collect()
    }

    /// Check if any steps exist in the given phase
    pub fn has_steps_in_phase(steps: &[ExecutionStepConfig], phase: &str) -> bool {
        steps.iter().any(|s| s.phase.as_deref() == Some(phase))
    }

    /// Count steps in each phase
    pub fn count_steps_by_phase(
        steps: &[ExecutionStepConfig],
    ) -> std::collections::HashMap<String, usize> {
        let mut counts = std::collections::HashMap::new();
        for step in steps {
            if let Some(ref phase) = step.phase {
                *counts.entry(phase.clone()).or_insert(0) += 1;
            } else {
                // Steps without explicit phase are considered "unknown"
                *counts.entry("unknown".to_string()).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Execute only setup phase steps.
    ///
    /// This runs setup steps (shell commands, workflows, etc.) that prepare the
    /// environment before the verification loop begins. Setup steps run ONCE
    /// at the start of the workflow.
    ///
    /// Returns the execution result and whether setup completed successfully.
    pub async fn execute_setup_phase(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
    ) -> (ExecutionResult, bool) {
        let setup_steps = Self::filter_steps_by_phase(steps, "setup");

        if setup_steps.is_empty() {
            info!("No setup steps to execute, setup phase complete by default");
            return (
                ExecutionResult {
                    success: true,
                    total_steps: 0,
                    successful_steps: 0,
                    failed_steps: 0,
                    total_duration_ms: 0,
                    steps: vec![],
                    captured_logs: None,
                    captured_runner_logs: None,
                    verification_passed: None,
                    loop_result: None,
                    task_summary: None,
                },
                true, // Setup phase complete
            );
        }

        info!(
            "Executing {} setup phase steps for {}",
            setup_steps.len(),
            execution_id
        );

        let result = self
            .execute_steps_with_log_sources(&setup_steps, execution_id, log_sources)
            .await;

        let setup_complete = result.success;

        info!(
            "Setup phase {}: {} of {} steps succeeded",
            if setup_complete { "complete" } else { "failed" },
            result.successful_steps,
            result.total_steps
        );

        (result, setup_complete)
    }

    /// Execute only completion phase steps.
    ///
    /// This runs completion steps (cleanup, reports, notifications) that run
    /// ONCE after the verification loop exits (success or max iterations).
    ///
    /// Returns the execution result.
    pub async fn execute_completion_phase(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
    ) -> ExecutionResult {
        let completion_steps = Self::filter_steps_by_phase(steps, "completion");

        if completion_steps.is_empty() {
            info!("No completion steps to execute");
            return ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: vec![],
                captured_logs: None,
                captured_runner_logs: None,
                verification_passed: None,
                loop_result: None,
                task_summary: None,
            };
        }

        info!(
            "Executing {} completion phase steps for {}",
            completion_steps.len(),
            execution_id
        );

        let result = self
            .execute_steps_with_log_sources(&completion_steps, execution_id, log_sources)
            .await;

        info!(
            "Completion phase done: {} of {} steps succeeded",
            result.successful_steps, result.total_steps
        );

        result
    }

    /// Execute only verification/agentic phase steps (for iterations).
    ///
    /// This runs verification and agentic steps that may run on each iteration.
    /// On iteration > 1, setup steps are filtered out (unless marked to run on
    /// subsequent iterations).
    ///
    /// Completion steps are always excluded from this method.
    pub async fn execute_verification_phase(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
        iteration: u32,
    ) -> ExecutionResult {
        // Filter out setup and completion steps, keep only verification/agentic
        let mut verification_steps: Vec<ExecutionStepConfig> = steps
            .iter()
            .filter(|s| {
                let phase = s.phase.as_deref().unwrap_or("unknown");
                // Include verification and agentic phase steps
                phase == "verification" || phase == "agentic"
            })
            .cloned()
            .collect();

        // For iteration > 1, also filter based on run_on_subsequent_iterations
        if iteration > 1 {
            verification_steps.retain(|step| step.should_run_on_iteration(iteration));
        }

        if verification_steps.is_empty() {
            info!(
                "No verification/agentic steps to execute for iteration {}",
                iteration
            );
            return ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: vec![],
                captured_logs: None,
                captured_runner_logs: None,
                verification_passed: None,
                loop_result: None,
                task_summary: None,
            };
        }

        info!(
            "Executing {} verification/agentic phase steps for iteration {}",
            verification_steps.len(),
            iteration
        );

        self.execute_steps_with_log_sources(&verification_steps, execution_id, log_sources)
            .await
    }

    /// Get current file positions for configured log sources
    fn capture_log_positions(
        log_sources: &[LogSourceConfig],
    ) -> std::collections::HashMap<String, u64> {
        use std::io::{Seek, SeekFrom};

        let mut positions = std::collections::HashMap::new();

        for source in log_sources {
            if !source.enabled {
                continue;
            }

            let path = std::path::Path::new(&source.path);
            if let Ok(mut file) = std::fs::File::open(path) {
                if let Ok(pos) = file.seek(SeekFrom::End(0)) {
                    positions.insert(source.id.clone(), pos);
                }
            }
        }

        positions
    }

    /// Read log content that was written since the given positions
    fn capture_logs_since(
        log_sources: &[LogSourceConfig],
        positions: std::collections::HashMap<String, u64>,
    ) -> Option<CapturedLogs> {
        use std::io::{Read, Seek, SeekFrom};

        let mut sources = std::collections::HashMap::new();

        for source in log_sources {
            if !source.enabled {
                continue;
            }

            let start_pos = positions.get(&source.id).copied().unwrap_or(0);
            let path = std::path::Path::new(&source.path);

            if let Ok(mut file) = std::fs::File::open(path) {
                if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                    let mut content = String::new();
                    if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                        sources.insert(source.name.clone(), content);
                    }
                }
            }
        }

        if sources.is_empty() {
            None
        } else {
            Some(CapturedLogs { sources })
        }
    }

    /// Get the .dev-logs directory path
    fn get_dev_logs_dir() -> PathBuf {
        crate::paths::get_dev_logs_dir()
    }

    /// Get current file positions for runner log files (actions + image recognition)
    fn capture_runner_log_positions() -> HashMap<String, u64> {
        use std::io::{Seek, SeekFrom};

        let mut positions = HashMap::new();
        let dev_logs = Self::get_dev_logs_dir();

        // Track positions for runner-actions.jsonl and runner-image-recognition.jsonl
        for filename in &["runner-actions.jsonl", "runner-image-recognition.jsonl"] {
            let path = dev_logs.join(filename);
            if let Ok(mut file) = std::fs::File::open(&path) {
                if let Ok(pos) = file.seek(SeekFrom::End(0)) {
                    positions.insert(filename.to_string(), pos);
                    info!(
                        "Captured runner log position for {}: {} bytes",
                        filename, pos
                    );
                }
            }
        }

        positions
    }

    /// Read runner logs that were written since the given positions
    fn capture_runner_logs_since(positions: HashMap<String, u64>) -> Option<CapturedRunnerLogs> {
        use std::io::{Read, Seek, SeekFrom};

        let dev_logs = Self::get_dev_logs_dir();
        let mut actions = Vec::new();
        let mut image_recognition = Vec::new();

        // Read runner-actions.jsonl
        let actions_path = dev_logs.join("runner-actions.jsonl");
        let start_pos = positions.get("runner-actions.jsonl").copied().unwrap_or(0);
        if let Ok(mut file) = std::fs::File::open(&actions_path) {
            if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                let mut content = String::new();
                if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                    actions = parse_action_events(&content);
                    info!("Captured {} action events from runner log", actions.len());
                }
            }
        }

        // Read runner-image-recognition.jsonl
        let ir_path = dev_logs.join("runner-image-recognition.jsonl");
        let start_pos = positions
            .get("runner-image-recognition.jsonl")
            .copied()
            .unwrap_or(0);
        if let Ok(mut file) = std::fs::File::open(&ir_path) {
            if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                let mut content = String::new();
                if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                    image_recognition = parse_image_recognition_events(&content);
                    info!(
                        "Captured {} image recognition events from runner log",
                        image_recognition.len()
                    );
                }
            }
        }

        if actions.is_empty() && image_recognition.is_empty() {
            None
        } else {
            Some(CapturedRunnerLogs {
                actions,
                image_recognition,
            })
        }
    }

    /// Execute a single step and return (success, error, screenshot_path)
    async fn execute_single_step(
        &self,
        step: &ExecutionStepConfig,
    ) -> (bool, Option<String>, Option<String>) {
        // Try to use the handler registry for polymorphic dispatch.
        // This is the new modular approach - handlers are self-contained and testable.
        // If no handler is registered, fall back to the legacy match statement below.
        if let Some(handler) = self.handler_registry.get(&step.step_type) {
            let context = self.create_handler_context();
            let result = handler.execute(step, &context).await;
            return (result.success, result.error, result.screenshot_path);
        }

        // Legacy match statement for step types not yet migrated to handlers.
        // As handlers are implemented, the match arms below will be removed.

        // Timeouts are disabled by default - only apply if explicitly specified
        let timeout = step.timeout_seconds;

        match step.step_type.as_str() {
            // NOTE: These step types now have handlers registered:
            // - "workflow", "state", "action", "screenshot", "shell_command", "prompt"
            // The handler registry dispatch above will handle them.
            // The match arms remain as fallback but should never be reached.
            "workflow" => {
                if let Some(ref workflow_name) = step.name {
                    match self
                        .action_service
                        .run_workflow(
                            workflow_name,
                            None,
                            step.monitor_index,
                            timeout,
                            step.initial_state_ids.as_deref(),
                        )
                        .await
                    {
                        Ok(result) => (result.success, result.error, None),
                        Err(e) => (false, Some(format!("Workflow error: {}", e)), None),
                    }
                } else {
                    (false, Some("No workflow name specified".to_string()), None)
                }
            }
            "state" => {
                if let Some(ref state_name) = step.name {
                    match self
                        .action_service
                        .go_to_state(state_name, None, step.monitor_index, timeout)
                        .await
                    {
                        Ok(result) => {
                            if result.success {
                                info!(
                                    "GO_TO_STATE '{}': Success. Check Python logs for details \
                                    (transition may have been skipped if state was already active)",
                                    state_name
                                );
                            }
                            (result.success, result.error, None)
                        }
                        Err(e) => (false, Some(format!("State navigation error: {}", e)), None),
                    }
                } else {
                    (false, Some("No state name specified".to_string()), None)
                }
            }
            "action" => {
                if let (Some(ref action_type), Some(ref image_id)) =
                    (&step.action_type, &step.target_image_id)
                {
                    match self
                        .action_service
                        .execute_action(action_type, image_id, None, step.monitor_index)
                        .await
                    {
                        Ok(result) => (
                            result.success,
                            result.message.filter(|_| !result.success),
                            None,
                        ),
                        Err(e) => (false, Some(format!("Action error: {}", e)), None),
                    }
                } else {
                    (
                        false,
                        Some("No action type or image ID specified".to_string()),
                        None,
                    )
                }
            }
            "screenshot" => {
                let monitor = match &step.screenshot_monitor {
                    Some(serde_json::Value::Number(n)) => n.as_i64().map(|v| v as i32),
                    Some(serde_json::Value::String(s)) if s == "all" => None,
                    _ => step.monitor_index,
                };
                let delay = if step.screenshot_delay > 0 {
                    Some(step.screenshot_delay as f64)
                } else {
                    None
                };

                // Get sequence number for tree events
                use std::sync::atomic::{AtomicU32, Ordering};
                static SCREENSHOT_SEQUENCE: AtomicU32 = AtomicU32::new(1);
                let sequence = SCREENSHOT_SEQUENCE.fetch_add(1, Ordering::SeqCst);
                let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                let action_id = format!("screenshot-{}", sequence);

                // Build action node for tree events
                // Must include: id, node_type, name, timestamp, status for ActionLogProfile
                let action_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": "SCREENSHOT",
                    "timestamp": timestamp,
                    "status": "pending",
                    "metadata": {
                        "monitor": monitor.map(|m| m.to_string()).unwrap_or_else(|| "all".to_string()),
                        "delay_seconds": delay.unwrap_or(0.0),
                    }
                });

                // Emit action_started tree event to file log
                FileLogger::log_tree_event(
                    "action_started",
                    &action_node,
                    &[],
                    timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: "action_started".to_string(),
                        timestamp,
                        data: json!({ "node": action_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event("action_started", &action_node, timestamp, sequence);

                let result = self.action_service.capture_screenshot(monitor, delay).await;
                let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;

                match result {
                    // Use absolute_path instead of screenshot_path to avoid relative path resolution issues
                    Ok(res) => {
                        // Record screenshot event for automation logs
                        let file_path = res.absolute_path.clone().unwrap_or_default();

                        // Emit action_completed tree event
                        // Must include: id, node_type, name, timestamp, status for ActionLogProfile
                        let completed_node = json!({
                            "id": &action_id,
                            "node_type": "action",
                            "name": "SCREENSHOT",
                            "timestamp": end_timestamp,
                            "status": if res.success { "success" } else { "failed" },
                            "duration": end_timestamp - timestamp,
                            "metadata": {
                                "monitor": monitor.map(|m| m.to_string()).unwrap_or_else(|| "all".to_string()),
                                "delay_seconds": delay.unwrap_or(0.0),
                                "filename": &file_path,
                            }
                        });
                        let event_type = if res.success {
                            "action_completed"
                        } else {
                            "action_failed"
                        };
                        FileLogger::log_tree_event(
                            event_type,
                            &completed_node,
                            &[],
                            end_timestamp,
                            sequence,
                        );

                        // Also add to DisplayProcessor for Session/Actions page
                        {
                            let raw_event = RawEvent {
                                id: uuid::Uuid::new_v4().to_string(),
                                event_type: event_type.to_string(),
                                timestamp: end_timestamp,
                                data: json!({ "node": completed_node.clone() }),
                                sequence: sequence as u64,
                            };
                            let mut processor = self.app_state.display_processor.lock().await;
                            processor.event_log_mut().add_event(raw_event);
                        }

                        // Emit to Tauri frontend for action log refresh
                        self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

                        self.record_screenshot_event(
                            "standalone",
                            &file_path,
                            monitor,
                            if step.screenshot_delay > 0 {
                                Some(step.screenshot_delay)
                            } else {
                                None
                            },
                            res.success,
                            None,
                            res.error.clone(),
                        )
                        .await;
                        (res.success, res.error, res.absolute_path)
                    }
                    Err(e) => {
                        let error_msg = format!("Screenshot error: {}", e);

                        // Emit action_failed tree event
                        // Must include: id, node_type, name, timestamp, status for ActionLogProfile
                        let failed_node = json!({
                            "id": &action_id,
                            "node_type": "action",
                            "name": "SCREENSHOT",
                            "timestamp": end_timestamp,
                            "status": "failed",
                            "duration": end_timestamp - timestamp,
                            "error": &error_msg,
                            "metadata": {
                                "monitor": monitor.map(|m| m.to_string()).unwrap_or_else(|| "all".to_string()),
                                "delay_seconds": delay.unwrap_or(0.0),
                            }
                        });
                        FileLogger::log_tree_event(
                            "action_failed",
                            &failed_node,
                            &[],
                            end_timestamp,
                            sequence,
                        );

                        // Also add to DisplayProcessor for Session/Actions page
                        {
                            let raw_event = RawEvent {
                                id: uuid::Uuid::new_v4().to_string(),
                                event_type: "action_failed".to_string(),
                                timestamp: end_timestamp,
                                data: json!({ "node": failed_node.clone() }),
                                sequence: sequence as u64,
                            };
                            let mut processor = self.app_state.display_processor.lock().await;
                            processor.event_log_mut().add_event(raw_event);
                        }

                        // Emit to Tauri frontend for action log refresh
                        self.emit_tree_event(
                            "action_failed",
                            &failed_node,
                            end_timestamp,
                            sequence,
                        );

                        // Record failed screenshot event
                        self.record_screenshot_event(
                            "standalone",
                            "",
                            monitor,
                            if step.screenshot_delay > 0 {
                                Some(step.screenshot_delay)
                            } else {
                                None
                            },
                            false,
                            None,
                            Some(error_msg.clone()),
                        )
                        .await;
                        (false, Some(error_msg), None)
                    }
                }
            }
            "playwright" => {
                // If we have inline script content (from combined scripts), run it directly
                if let Some(ref script_content) = step.playwright_script_content {
                    self.run_playwright_inline(
                        script_content,
                        step.playwright_target_url.as_deref(),
                        step.name.as_deref().unwrap_or("combined_script"),
                    )
                    .await
                } else if let Some(ref script_id) = step.playwright_script_id {
                    // Otherwise, run by script ID
                    self.run_playwright_script(script_id).await
                } else {
                    (
                        false,
                        Some("No Playwright script ID or content specified".to_string()),
                        None,
                    )
                }
            }
            "test" => {
                // Execute verification test with tree event emission
                use std::sync::atomic::{AtomicU32, Ordering};
                static TEST_SEQUENCE: AtomicU32 = AtomicU32::new(1);
                let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::SeqCst);
                let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                let action_id = format!("test-{}", sequence);
                let step_name = step
                    .name
                    .clone()
                    .unwrap_or_else(|| "Verification Test".to_string());
                let test_id_display = step
                    .test_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                let is_critical = step.test_is_critical.unwrap_or(false);

                // Build action node for tree events
                let action_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("TEST: {}", step_name),
                    "timestamp": timestamp,
                    "status": "pending",
                    "metadata": {
                        "test_id": &test_id_display,
                        "is_critical": is_critical,
                    }
                });

                // Emit action_started tree event to file log
                FileLogger::log_tree_event(
                    "action_started",
                    &action_node,
                    &[],
                    timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: "action_started".to_string(),
                        timestamp,
                        data: json!({ "node": action_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event("action_started", &action_node, timestamp, sequence);

                // Execute the test
                let result = if let Some(ref test_id) = step.test_id {
                    // Execute stored verification test by ID
                    match self.execute_verification_test(test_id, is_critical).await {
                        Ok((success, error)) => (success, error, None),
                        Err(e) => (false, Some(format!("Test execution error: {}", e)), None),
                    }
                } else if step.test_type.as_deref() == Some("repository") {
                    // Repository test: run a command in the working directory
                    let command = step
                        .check_command
                        .clone()
                        .or_else(|| step.shell_command.clone())
                        .unwrap_or_else(|| "pytest".to_string());
                    let working_dir = step
                        .check_working_directory
                        .clone()
                        .or_else(|| step.shell_command_working_directory.clone())
                        .unwrap_or_else(|| {
                            std::env::current_dir()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|_| ".".to_string())
                        });

                    info!("Executing repository test: {} in {}", command, working_dir);

                    // Create a temporary step config for the shell command execution
                    let temp_step = ExecutionStepConfig {
                        shell_command: Some(command.clone()),
                        shell_command_working_directory: Some(working_dir),
                        ..Default::default()
                    };
                    // Timeouts are disabled by default
                    let timeout = step.timeout_seconds;
                    self.execute_shell_command_step(&temp_step, timeout).await
                } else {
                    (
                        false,
                        Some("No test ID specified and test_type is not 'repository'".to_string()),
                        None,
                    )
                };

                let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                let duration = end_timestamp - timestamp;
                let (success, ref error_opt, _) = result;

                // Build completed/failed node
                let completed_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("TEST: {}", step_name),
                    "timestamp": end_timestamp,
                    "status": if success { "success" } else { "failed" },
                    "duration": duration,
                    "error": error_opt.clone(),
                    "metadata": {
                        "test_id": &test_id_display,
                        "is_critical": is_critical,
                    }
                });

                let event_type = if success {
                    "action_completed"
                } else {
                    "action_failed"
                };

                // Emit tree event to file log
                FileLogger::log_tree_event(
                    event_type,
                    &completed_node,
                    &[],
                    end_timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: event_type.to_string(),
                        timestamp: end_timestamp,
                        data: json!({ "node": completed_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

                result
            }
            "prompt" => {
                // Prompt steps are text for the AI, not executed here - emit tree events for UI visibility
                use std::sync::atomic::{AtomicU32, Ordering};
                static PROMPT_SEQUENCE: AtomicU32 = AtomicU32::new(1);
                let sequence = PROMPT_SEQUENCE.fetch_add(1, Ordering::SeqCst);
                let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                let action_id = format!("prompt-{}", sequence);
                let step_name = step.name.clone().unwrap_or_else(|| "AI Prompt".to_string());
                let prompt_text = step.prompt_content.clone().unwrap_or_else(String::new);
                let prompt_preview = if prompt_text.len() > 100 {
                    format!("{}...", &prompt_text[..100])
                } else {
                    prompt_text.clone()
                };

                // Build action node for tree events
                let action_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("PROMPT: {}", step_name),
                    "timestamp": timestamp,
                    "status": "pending",
                    "metadata": {
                        "prompt_preview": prompt_preview,
                        "type": "ai_prompt",
                    }
                });

                // Emit action_started tree event to file log
                FileLogger::log_tree_event(
                    "action_started",
                    &action_node,
                    &[],
                    timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: "action_started".to_string(),
                        timestamp,
                        data: json!({ "node": action_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event("action_started", &action_node, timestamp, sequence);

                // Prompt steps complete immediately (text is passed to AI, not executed here)
                let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;

                // Build completed node
                let completed_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("PROMPT: {}", step_name),
                    "timestamp": end_timestamp,
                    "status": "success",
                    "duration": end_timestamp - timestamp,
                    "metadata": {
                        "prompt_preview": prompt_preview,
                        "type": "ai_prompt",
                        "note": "Prompt text passed to AI for processing",
                    }
                });

                // Emit action_completed tree event to file log
                FileLogger::log_tree_event(
                    "action_completed",
                    &completed_node,
                    &[],
                    end_timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: "action_completed".to_string(),
                        timestamp: end_timestamp,
                        data: json!({ "node": completed_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event("action_completed", &completed_node, end_timestamp, sequence);

                (true, None, None)
            }
            // ================================================================
            // MCP Call Step Type
            // ================================================================
            "mcp_call" => {
                if let (Some(ref server_id), Some(ref tool_name)) =
                    (&step.mcp_server_id, &step.mcp_tool_name)
                {
                    self.execute_mcp_call(
                        server_id,
                        tool_name,
                        step.mcp_arguments.clone().unwrap_or(serde_json::json!({})),
                        timeout,
                        step.name.as_deref(),
                        step.mcp_fail_on_error.unwrap_or(true),
                    )
                    .await
                } else {
                    (
                        false,
                        Some("Server ID and tool name required for MCP call".to_string()),
                        None,
                    )
                }
            }
            // ================================================================
            // Shell Command Step Type
            // ================================================================
            "shell_command" => self.execute_shell_command_step(step, timeout).await,
            // ================================================================
            // Script Step Type (Playwright inline code)
            // ================================================================
            "script" => {
                // Script steps contain inline Playwright code - treat like playwright
                if let Some(ref script_content) = step.playwright_script_content {
                    self.run_playwright_inline(
                        script_content,
                        step.playwright_target_url.as_deref(),
                        step.name.as_deref().unwrap_or("script"),
                    )
                    .await
                } else if let Some(ref script_id) = step.playwright_script_id {
                    self.run_playwright_script(script_id).await
                } else {
                    (
                        false,
                        Some("No script code or script ID specified".to_string()),
                        None,
                    )
                }
            }
            // ================================================================
            // Workflow Reference Step Type (by ID)
            // ================================================================
            "workflow_ref" => {
                // workflow_ref runs another workflow by ID
                // For now, we look up the workflow name and delegate to the workflow handler
                if let Some(ref workflow_name) = step.name {
                    match self
                        .action_service
                        .run_workflow(
                            workflow_name,
                            None,
                            step.monitor_index,
                            timeout,
                            step.initial_state_ids.as_deref(),
                        )
                        .await
                    {
                        Ok(result) => (result.success, result.error, None),
                        Err(e) => (false, Some(format!("Workflow ref error: {}", e)), None),
                    }
                } else {
                    (
                        false,
                        Some("No workflow name specified for workflow_ref".to_string()),
                        None,
                    )
                }
            }
            // ================================================================
            // GUI Action Step Type (vision-based)
            // ================================================================
            "gui_action" => {
                // gui_action uses vision-based automation
                // Map to action handler with appropriate field extraction
                if let Some(ref action_type) = step.action_type {
                    if let Some(ref image_id) = step.target_image_id {
                        match self
                            .action_service
                            .execute_action(action_type, image_id, None, step.monitor_index)
                            .await
                        {
                            Ok(result) => (
                                result.success,
                                result.message.filter(|_| !result.success),
                                None,
                            ),
                            Err(e) => (false, Some(format!("GUI action error: {}", e)), None),
                        }
                    } else {
                        // For type/hotkey actions that don't need a target image
                        match action_type.as_str() {
                            "type" | "hotkey" | "scroll" => {
                                // These would need special handling via Python bridge
                                (
                                    false,
                                    Some(format!(
                                        "GUI action '{}' not yet implemented in step executor",
                                        action_type
                                    )),
                                    None,
                                )
                            }
                            _ => (
                                false,
                                Some("No target image ID specified for GUI action".to_string()),
                                None,
                            ),
                        }
                    }
                } else {
                    (
                        false,
                        Some("No action type specified for GUI action".to_string()),
                        None,
                    )
                }
            }
            // ================================================================
            // API Request Step Type
            // ================================================================
            "api_request" => self.execute_api_request_step(step, timeout).await,
            // ================================================================
            // Check Step Type (code quality checks)
            // ================================================================
            "check" => self.execute_check_step(step, timeout).await,
            // ================================================================
            // Check Group Step Type (run all checks in a group)
            // ================================================================
            "check_group" => {
                let (success, error, summary, _check_results) =
                    self.execute_check_group_step(step, timeout).await;
                (success, error, summary)
            }
            // ================================================================
            // Macro Step Type (runs a saved macro by ID)
            // ================================================================
            "macro" => {
                if let Some(ref macro_id) = step.macro_id {
                    self.execute_macro_step(macro_id, step.monitor_index).await
                } else {
                    (
                        false,
                        Some("No macro ID specified for macro step".to_string()),
                        None,
                    )
                }
            }
            // ================================================================
            // Log Watch Step Type (deterministic error detection from logs)
            // ================================================================
            "log_watch" => self.execute_log_watch_step(step).await,
            // ================================================================
            // Shell Step Type (execute shell command)
            // ================================================================
            "shell" => {
                // Timeouts are disabled by default
                let timeout = step.timeout_seconds;
                let (success, error, output) = self.execute_shell_command_step(step, timeout).await;
                // Return output as the third element for potential logging
                (success, error, output)
            }
            _ => {
                warn!("Unknown step type: {}", step.step_type);
                (
                    false,
                    Some(format!("Unknown step type: {}", step.step_type)),
                    None,
                )
            }
        }
    }

    /// Capture a post-step screenshot
    async fn capture_post_step_screenshot(&self, step: &ExecutionStepConfig) -> Option<String> {
        // Apply configured screenshot delay (no default delay)
        if step.screenshot_delay > 0 {
            info!(
                "Waiting {}s before screenshot capture",
                step.screenshot_delay
            );
            tokio::time::sleep(std::time::Duration::from_secs(step.screenshot_delay as u64)).await;
        }

        let monitor = match &step.screenshot_monitor {
            Some(serde_json::Value::Number(n)) => n.as_i64().map(|v| v as i32),
            Some(serde_json::Value::String(s)) if s == "all" => None,
            _ => step.monitor_index,
        };

        // Build associated action description
        let associated_action = match step.step_type.as_str() {
            "workflow" => step.name.clone().map(|n| format!("workflow:{}", n)),
            "action" => step.action_type.clone().map(|t| format!("action:{}", t)),
            "state" => step.name.clone().map(|n| format!("state:{}", n)),
            _ => Some(format!("step:{}", step.step_type)),
        };

        match self.action_service.capture_screenshot(monitor, None).await {
            Ok(result) => {
                let file_path = result.absolute_path.clone().unwrap_or_default();
                // Record post-action screenshot event
                self.record_screenshot_event(
                    "post_action",
                    &file_path,
                    monitor,
                    if step.screenshot_delay > 0 {
                        Some(step.screenshot_delay)
                    } else {
                        None
                    },
                    result.success,
                    associated_action,
                    result.error,
                )
                .await;
                result.absolute_path // Use absolute path for step screenshots
            }
            Err(e) => {
                warn!("Failed to capture post-step screenshot: {}", e);
                // Record failed post-action screenshot event
                self.record_screenshot_event(
                    "post_action",
                    "",
                    monitor,
                    if step.screenshot_delay > 0 {
                        Some(step.screenshot_delay)
                    } else {
                        None
                    },
                    false,
                    associated_action,
                    Some(format!("Screenshot error: {}", e)),
                )
                .await;
                None
            }
        }
    }

    /// Run a Playwright test script via HTTP API
    #[tracing::instrument(
        name = "playwright.test.script",
        skip(self),
        fields(
            test_name = %script_id
        )
    )]
    async fn run_playwright_script(
        &self,
        script_id: &str,
    ) -> (bool, Option<String>, Option<String>) {
        let client = reqwest::Client::new();
        let url = format!("http://localhost:9876/playwright/scripts/{}/run", script_id);

        match client
            .post(&url)
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
        {
            Ok(response) => {
                if let Ok(json) = response.json::<serde_json::Value>().await {
                    let success = json
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let error = if !success {
                        json.get("error")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    };
                    (success, error, None)
                } else {
                    (
                        false,
                        Some("Failed to parse Playwright response".to_string()),
                        None,
                    )
                }
            }
            Err(e) => (
                false,
                Some(format!("Playwright request error: {}", e)),
                None,
            ),
        }
    }

    /// Run inline Playwright script content (for combined scripts)
    ///
    /// This runs script content directly without needing a script ID.
    /// Used for combined setup+verification scripts.
    #[tracing::instrument(
        name = "playwright.test.inline",
        skip(self, content),
        fields(
            test_name = %script_name,
            content_length = %content.len(),
            target_url = ?target_url
        )
    )]
    async fn run_playwright_inline(
        &self,
        content: &str,
        target_url: Option<&str>,
        script_name: &str,
    ) -> (bool, Option<String>, Option<String>) {
        info!(
            "Running inline Playwright script: {} ({} chars)",
            script_name,
            content.len()
        );

        // Run the inline script using the playwright executor
        match crate::playwright::run_script_inline(content, target_url, script_name) {
            Ok(result) => {
                let error = if !result.passed {
                    result.error.clone()
                } else {
                    None
                };
                (result.passed, error, None)
            }
            Err(e) => (false, Some(format!("Inline Playwright error: {}", e)), None),
        }
    }

    /// Execute a verification test by ID and return simplified (success, error) tuple
    ///
    /// This is the legacy interface used by execute_single_step.
    async fn execute_verification_test(
        &self,
        test_id: &str,
        is_critical: bool,
    ) -> Result<(bool, Option<String>), String> {
        use crate::test_executor::TestStatus;

        let result = self.execute_verification_test_with_details(test_id).await?;

        // Log the result
        if result.status == TestStatus::Passed {
            info!(
                "Test '{}' passed in {}ms ({}/{} assertions)",
                result.test_name,
                result.duration_ms,
                result.assertions_passed,
                result.assertions_passed + result.assertions_failed
            );
            Ok((true, None))
        } else {
            let error_msg = format!(
                "Test '{}' {}: {} ({}/{} assertions passed)",
                result.test_name,
                match result.status {
                    TestStatus::Failed => "failed",
                    TestStatus::Error => "errored",
                    TestStatus::Timeout => "timed out",
                    _ => "did not pass",
                },
                result.error.as_deref().unwrap_or("Unknown error"),
                result.assertions_passed,
                result.assertions_passed + result.assertions_failed
            );

            warn!("{}", error_msg);

            // If critical, report as step failure; otherwise, log but succeed
            if is_critical {
                Ok((false, Some(error_msg)))
            } else {
                info!("Non-critical test failure - step continues");
                Ok((true, Some(format!("(Non-critical) {}", error_msg))))
            }
        }
    }

    /// Execute a verification test by ID and return the full TestExecutionResult
    ///
    /// This provides rich details for verification phase context building.
    async fn execute_verification_test_with_details(
        &self,
        test_id: &str,
    ) -> Result<crate::test_executor::TestExecutionResult, String> {
        use crate::database::TestType as DbTestType;
        use crate::test_executor::{self, TestCategory, TestDefinition, TestType, VisionConfig};

        info!("Executing verification test with details: {}", test_id);

        // Get the test from database
        let verification_test = self
            .app_state
            .checkpoint_db
            .get_verification_test(test_id)?
            .ok_or_else(|| format!("Verification test not found: {}", test_id))?;

        // Convert database TestType to test_executor TestType
        let test_type = match verification_test.test_type {
            DbTestType::PlaywrightCdp => TestType::PlaywrightCdp,
            DbTestType::QontinuiVision => TestType::QontinuiVision,
            DbTestType::PythonScript => TestType::PythonScript,
            DbTestType::RepositoryTest => TestType::RepositoryTest,
        };

        // Parse vision config if present
        let vision_config: Option<VisionConfig> = verification_test
            .vision_config
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Parse repo test config if present
        let repo_test_config = verification_test
            .repo_test_config
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Convert to TestDefinition
        let test_def = TestDefinition {
            id: verification_test.id.clone(),
            name: verification_test.name.clone(),
            test_type,
            category: TestCategory::Custom, // Default to Custom
            playwright_code: verification_test.playwright_code.clone(),
            vision_config,
            python_code: verification_test.python_code.clone(),
            repo_test_config,
            timeout_seconds: verification_test.timeout_seconds.unwrap_or(60),
            is_critical: verification_test.is_critical,
            config: verification_test.config.clone(),
        };

        // Execute the test (synchronous)
        let result = test_executor::execute_test(&test_def);

        Ok(result)
    }

    /// Execute all verification steps and return a VerificationPhaseResult
    ///
    /// This is the main entry point for the verification phase in the
    /// verification-agentic loop. It:
    /// 1. Executes each verification step in order
    /// 2. Captures detailed results for each step
    /// 3. Stops on critical step failure
    /// 4. Returns a summary that can be used to build AI context
    #[tracing::instrument(
        name = "workflow.verification.execute",
        skip(self, steps),
        fields(
            step_count = %steps.len(),
            execution_id = %execution_id,
            iteration = %iteration
        )
    )]
    pub async fn execute_verification_steps(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
    ) -> VerificationPhaseResult {
        self.execute_verification_steps_with_events(steps, execution_id, iteration, None)
            .await
    }

    /// Run verification phase steps with optional event emission.
    ///
    /// This version emits completion events as each step finishes, allowing
    /// the UI to show real-time progress instead of waiting until all steps complete.
    #[tracing::instrument(
        name = "workflow.verification.with_events",
        skip(self, steps),
        fields(
            step_count = %steps.len(),
            execution_id = %execution_id,
            iteration = %iteration,
            workflow_name = ?workflow_name
        )
    )]
    pub async fn execute_verification_steps_with_events(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
        workflow_name: Option<&str>,
    ) -> VerificationPhaseResult {
        use crate::step_event_builder::StepEventBuilder;
        use crate::step_metadata::{StepDetails, StepMetadata};
        use crate::step_types::StepType;
        use crate::test_executor::TestStatus;
        use std::time::Instant;

        // For workflow sequence children, use parent ID for event logging (FK constraint)
        let event_execution_id = get_parent_task_id(execution_id);
        let start = Instant::now();
        let mut step_results = Vec::new();
        let mut passed_steps = 0;
        let mut failed_steps = 0;
        let mut skipped_steps = 0;
        let mut critical_failure = false;

        // Filter to only verification phase steps
        let verification_steps: Vec<_> = steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("verification"))
            .collect();

        info!(
            "Executing {} verification steps for iteration {}",
            verification_steps.len(),
            iteration
        );

        for (index, step) in verification_steps.iter().enumerate() {
            // Skip remaining steps if we had a critical failure
            if critical_failure {
                let skipped_at = chrono::Utc::now().to_rfc3339();
                let result = StepExecutionResult {
                    step_index: index,
                    step_name: step
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("Step {}", index + 1)),
                    step_type: step.step_type.clone(),
                    success: false,
                    error: Some("Skipped due to critical failure".to_string()),
                    screenshot_path: None,
                    started_at: Some(skipped_at.clone()),
                    ended_at: Some(skipped_at),
                    duration_ms: 0,
                    config: StepExecutionConfig {
                        action_type: None,
                        target_image_id: None,
                        target_image_name: None,
                        monitor_index: None,
                        screenshot_delay: None,
                        timeout_seconds: None,
                        playwright_script_id: None,
                        initial_state_ids: None,
                        check_type: None,
                        command: None,
                        test_id: step.test_id.clone(),
                        test_type: step.test_type.clone(),
                        working_directory: None,
                    },
                    verification_details: None,
                };
                step_results.push(result);
                skipped_steps += 1;
                continue;
            }

            let step_start = Instant::now();
            let step_started_at = chrono::Utc::now().to_rfc3339();
            let step_name = step
                .name
                .clone()
                .unwrap_or_else(|| format!("Step {}", index + 1));
            let is_critical = step.test_is_critical.unwrap_or(false);

            // Execute based on step type
            let (success, error, verification_details) = match step.step_type.as_str() {
                "test" => {
                    if let Some(ref test_id) = step.test_id {
                        match self.execute_verification_test_with_details(test_id).await {
                            Ok(test_result) => {
                                let passed = test_result.status == TestStatus::Passed;
                                let details = VerificationStepDetails {
                                    step_id: step
                                        .name
                                        .clone()
                                        .unwrap_or_else(|| format!("step-{}", index)),
                                    phase: "verification".to_string(),
                                    is_critical,
                                    is_blocking: is_critical,
                                    stdout: Some(test_result.output.clone()),
                                    stderr: None,
                                    assertions_passed: Some(test_result.assertions_passed),
                                    assertions_total: Some(
                                        test_result.assertions_passed
                                            + test_result.assertions_failed,
                                    ),
                                    console_output: test_result
                                        .structured_output
                                        .as_ref()
                                        .and_then(|v| v.get("console_output"))
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string()),
                                    page_snapshot: test_result
                                        .structured_output
                                        .as_ref()
                                        .and_then(|v| v.get("page_snapshot"))
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string()),
                                    exit_code: test_result.exit_code,
                                    check_results: None,
                                };
                                (
                                    passed,
                                    if passed {
                                        None
                                    } else {
                                        test_result.error.clone()
                                    },
                                    Some(details),
                                )
                            }
                            Err(e) => (
                                false,
                                Some(format!("Test execution error: {}", e)),
                                Some(VerificationStepDetails {
                                    step_id: step
                                        .name
                                        .clone()
                                        .unwrap_or_else(|| format!("step-{}", index)),
                                    phase: "verification".to_string(),
                                    is_critical,
                                    is_blocking: is_critical,
                                    stderr: Some(e),
                                    ..Default::default()
                                }),
                            ),
                        }
                    } else {
                        (false, Some("No test_id specified".to_string()), None)
                    }
                }
                "check" => {
                    // Execute check step (shell command for checks like lint, typecheck, etc.)
                    let (success, error, output) = self.execute_single_step(step).await;
                    let details = VerificationStepDetails {
                        step_id: step
                            .name
                            .clone()
                            .unwrap_or_else(|| format!("step-{}", index)),
                        phase: "verification".to_string(),
                        is_critical,
                        is_blocking: is_critical,
                        stdout: output, // Capture output for AI context
                        ..Default::default()
                    };
                    (success, error, Some(details))
                }
                "shell" => {
                    // Execute shell command step
                    // Timeouts are disabled by default
                    let timeout = step.timeout_seconds;
                    let (success, error, output) =
                        self.execute_shell_command_step(step, timeout).await;
                    let details = VerificationStepDetails {
                        step_id: step
                            .name
                            .clone()
                            .unwrap_or_else(|| format!("step-{}", index)),
                        phase: "verification".to_string(),
                        is_critical,
                        is_blocking: is_critical,
                        stdout: output, // Capture output for AI context
                        ..Default::default()
                    };
                    (success, error, Some(details))
                }
                "check_group" => {
                    // Execute check group - runs all checks in the group
                    // Timeouts are disabled by default
                    let timeout = step.timeout_seconds;
                    let (success, error, summary, check_results) =
                        self.execute_check_group_step(step, timeout).await;
                    let details = VerificationStepDetails {
                        step_id: step
                            .name
                            .clone()
                            .unwrap_or_else(|| format!("step-{}", index)),
                        phase: "verification".to_string(),
                        is_critical,
                        is_blocking: is_critical,
                        // Capture the detailed summary with all check results for AI context
                        stdout: summary,
                        // Include structured check results for UI display
                        check_results,
                        ..Default::default()
                    };
                    (success, error, Some(details))
                }
                _ => {
                    // For other step types, use the generic executor
                    let (success, error, output) = self.execute_single_step(step).await;
                    // Still capture output even for unknown step types
                    let details = if output.is_some() {
                        Some(VerificationStepDetails {
                            step_id: step
                                .name
                                .clone()
                                .unwrap_or_else(|| format!("step-{}", index)),
                            phase: "verification".to_string(),
                            is_critical,
                            is_blocking: is_critical,
                            stdout: output,
                            ..Default::default()
                        })
                    } else {
                        None
                    };
                    (success, error, details)
                }
            };

            let duration_ms = step_start.elapsed().as_millis() as u64;

            if success {
                passed_steps += 1;
                info!(
                    "Verification step '{}' passed in {}ms",
                    step_name, duration_ms
                );
            } else {
                failed_steps += 1;
                warn!(
                    "Verification step '{}' failed: {:?}",
                    step_name,
                    error.as_deref().unwrap_or("unknown error")
                );

                // Check for critical failure
                if is_critical {
                    critical_failure = true;
                    warn!("Critical verification step failed - stopping verification phase");
                }
            }

            let step_ended_at = chrono::Utc::now().to_rfc3339();

            let result = StepExecutionResult {
                step_index: index,
                step_name,
                step_type: step.step_type.clone(),
                success,
                error,
                screenshot_path: None,
                started_at: Some(step_started_at),
                ended_at: Some(step_ended_at),
                duration_ms,
                config: StepExecutionConfig {
                    action_type: step.action_type.clone(),
                    target_image_id: step.target_image_id.clone(),
                    target_image_name: step.target_image_name.clone(),
                    monitor_index: step.monitor_index,
                    screenshot_delay: if step.screenshot_delay > 0 {
                        Some(step.screenshot_delay)
                    } else {
                        None
                    },
                    timeout_seconds: step.timeout_seconds,
                    playwright_script_id: step.playwright_script_id.clone(),
                    initial_state_ids: step.initial_state_ids.clone(),
                    check_type: step.check_type.clone(),
                    command: step
                        .check_command
                        .clone()
                        .or_else(|| step.shell_command.clone()),
                    test_id: step.test_id.clone(),
                    test_type: step.test_type.clone(),
                    working_directory: step
                        .check_working_directory
                        .clone()
                        .or_else(|| step.shell_command_working_directory.clone()),
                },
                verification_details,
            };

            // Emit completion event for this step (real-time UI update)
            // This allows the frontend to show progress as each step finishes
            if workflow_name.is_some() {
                let step_type_enum =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::CheckGroup);
                let metadata = StepMetadata::verification(
                    &event_execution_id, // Use parent ID for FK constraint
                    step_type_enum,
                    &result.step_name,
                    index,
                    iteration,
                );

                let details = if result.success {
                    StepDetails::default().with_duration(duration_ms as i64)
                } else {
                    StepDetails::default()
                        .with_duration(duration_ms as i64)
                        .with_error(result.error.clone().unwrap_or_default())
                };

                let builder = StepEventBuilder::new(&event_execution_id, metadata) // Use parent ID
                    .with_details(details)
                    .with_workflow_name(workflow_name.unwrap_or_default());

                let event = if result.success {
                    builder.build_complete(duration_ms as i64)
                } else {
                    builder.build_error(duration_ms as i64, result.error.as_deref())
                };

                if let Err(e) = self.app_state.checkpoint_db.create_task_run_event(&event) {
                    warn!("Failed to emit verification step completion event: {}", e);
                }
            }

            step_results.push(result);
        }

        let total_duration_ms = start.elapsed().as_millis() as u64;
        let all_passed = failed_steps == 0 && skipped_steps == 0;

        let result = VerificationPhaseResult {
            iteration,
            all_passed,
            total_steps: verification_steps.len(),
            passed_steps,
            failed_steps,
            skipped_steps,
            total_duration_ms,
            step_results,
            critical_failure,
        };

        info!("{}", result.summary());
        result
    }

    // =========================================================================
    // MCP Call Step Execution
    // =========================================================================

    /// Execute an MCP call step - calls a tool on an MCP server
    async fn execute_mcp_call(
        &self,
        server_id: &str,
        tool_name: &str,
        arguments: serde_json::Value,
        _timeout_secs: Option<u64>,
        step_name: Option<&str>,
        fail_on_error: bool,
    ) -> (bool, Option<String>, Option<String>) {
        info!("MCP Call: {}.{}", server_id, tool_name);

        let start_time = std::time::Instant::now();

        // Get the MCP client manager from app state
        let mcp_manager = self.app_state.mcp_client_manager.lock().await;

        // Temporarily set timeout on the server config (not persisted)
        // The actual timeout is handled by the MCP client implementation
        let result = mcp_manager
            .call_tool(server_id, tool_name, arguments.clone())
            .await;
        let duration_ms = start_time.elapsed().as_millis() as i64;

        match result {
            Ok(call_result) => {
                // Save result to database
                self.save_mcp_call_result(
                    server_id,
                    tool_name,
                    &arguments,
                    call_result.content.as_ref(),
                    &call_result.response_type,
                    call_result.success,
                    call_result.error.as_deref(),
                    duration_ms,
                    step_name,
                );

                if call_result.success {
                    info!(
                        "MCP Call '{}.{}' succeeded in {}ms",
                        server_id, tool_name, duration_ms
                    );
                    // Return the content as a JSON string in the screenshot_path field
                    // (repurposing this field for MCP result data)
                    let result_data = call_result
                        .content
                        .map(|c| serde_json::to_string(&c).unwrap_or_default());
                    (true, None, result_data)
                } else {
                    let error = call_result
                        .error
                        .unwrap_or_else(|| "MCP call failed".to_string());
                    if fail_on_error {
                        (false, Some(error), None)
                    } else {
                        // Return success but include the error message
                        info!(
                            "MCP Call '{}.{}' failed but fail_on_error=false, continuing",
                            server_id, tool_name
                        );
                        (true, Some(format!("(ignored) {}", error)), None)
                    }
                }
            }
            Err(e) => {
                let error_msg = format!("MCP call error: {}", e);

                // Save error result to database
                self.save_mcp_call_result(
                    server_id,
                    tool_name,
                    &arguments,
                    None,
                    "error",
                    false,
                    Some(&error_msg),
                    duration_ms,
                    step_name,
                );

                if fail_on_error {
                    (false, Some(error_msg), None)
                } else {
                    info!(
                        "MCP Call '{}.{}' errored but fail_on_error=false, continuing",
                        server_id, tool_name
                    );
                    (true, Some(format!("(ignored) {}", error_msg)), None)
                }
            }
        }
    }

    /// Save an MCP call result to the database (if task_run_id is set)
    fn save_mcp_call_result(
        &self,
        server_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
        response: Option<&serde_json::Value>,
        response_type: &str,
        success: bool,
        error: Option<&str>,
        duration_ms: i64,
        step_name: Option<&str>,
    ) {
        // Only save if we have a task_run_id
        let Some(ref task_run_id) = self.task_run_id else {
            return;
        };

        let input = CreateMcpCallInput {
            task_run_id: task_run_id.clone(),
            step_id: uuid::Uuid::new_v4().to_string(), // Generate a step ID if not provided
            step_name: step_name.map(|s| s.to_string()),
            server_id: server_id.to_string(),
            server_name: None, // Could be resolved from MCP client manager if needed
            tool_name: tool_name.to_string(),
            arguments: Some(serde_json::to_string(arguments).unwrap_or_default()),
            resolved_arguments: None, // Same as arguments in this case
            response: response.map(|r| serde_json::to_string(r).unwrap_or_default()),
            response_type: response_type.to_string(),
            duration_ms,
            extractions: None, // No variable extractions for now
            assertions: None,  // No assertions for now
            success,
            error_message: error.map(|e| e.to_string()),
        };

        match self
            .app_state
            .checkpoint_db
            .create_task_run_mcp_call(&input)
        {
            Ok(id) => {
                info!(
                    "Saved MCP call result to database: {} (tool: {})",
                    id, tool_name
                );
            }
            Err(e) => {
                warn!("Failed to save MCP call result to database: {}", e);
            }
        }
    }

    // =========================================================================
    // Shell Command Step Execution
    // =========================================================================

    /// Execute a shell command step
    ///
    /// Supports variable expansion using `{{variable_name}}` syntax in the command.
    /// Variables are resolved from the runtime context.
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_shell_command_step(
        &self,
        step: &ExecutionStepConfig,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<String>, Option<String>) {
        use std::process::Stdio;
        use tokio::process::Command;
        use tokio::time::{timeout, Duration};

        // Get the command template - either directly or from shell_command_id (not implemented yet)
        let template_command = match &step.shell_command {
            Some(cmd) => cmd.clone(),
            None => {
                // If no direct command, check for shell_command_id
                if let Some(_id) = &step.shell_command_id {
                    // TODO: Look up saved shell command by ID from database
                    return (
                        false,
                        Some(
                            "Shell command lookup by ID not yet implemented in step executor"
                                .to_string(),
                        ),
                        None,
                    );
                }
                return (false, Some("No shell command specified".to_string()), None);
            }
        };

        // Expand variables in the command using runtime context
        let evaluator = ExpressionEvaluator::new();
        let has_variables = evaluator.has_expressions(&template_command);
        let command = evaluator.evaluate(&template_command, &self.runtime_context);

        // Track which variables were resolved (for UI display)
        let resolved_variables: Option<HashMap<String, String>> = if has_variables {
            let expressions = evaluator.find_expressions(&template_command);
            let mut vars = HashMap::new();
            for expr in expressions {
                // Try to resolve the expression to get the value
                let resolved =
                    evaluator.evaluate(&format!("{{{{{}}}}}", expr), &self.runtime_context);
                // Only include if it was actually resolved (doesn't still contain braces)
                if !resolved.contains("{{") {
                    vars.insert(expr, resolved);
                }
            }
            if vars.is_empty() {
                None
            } else {
                Some(vars)
            }
        } else {
            None
        };

        // Log variable expansion if applicable
        if has_variables {
            info!(
                "Shell command variables expanded: template='{}' -> resolved='{}'",
                template_command, command
            );
            if let Some(ref vars) = resolved_variables {
                info!("Resolved variables: {:?}", vars);
            }
        }

        let step_name = step.name.as_deref().unwrap_or("Shell Command");
        let working_directory = step.shell_command_working_directory.clone();
        let fail_on_error = step.shell_command_fail_on_error.unwrap_or(true);

        // Detect if command uses PowerShell syntax
        let is_powershell = command.contains("Get-")
            || command.contains("Set-")
            || command.contains("New-")
            || command.contains("Remove-")
            || command.contains("Invoke-")
            || command.contains("ForEach-Object")
            || command.contains("Where-Object")
            || command.contains("Select-Object")
            || command.contains("$_")
            || command.contains("$env:")
            || command.contains("-ErrorAction")
            || command.contains("| %")
            || command.contains("| ?");

        let shell_type = if cfg!(target_os = "windows") && is_powershell {
            "powershell"
        } else if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        };

        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());
        info!(
            "Executing shell command '{}': {} (shell: {}, timeout: {}, working_dir: {:?})",
            step_name, command, shell_type, timeout_str, working_directory
        );

        // Generate sequence number and timestamp for tree events
        use std::sync::atomic::{AtomicU32, Ordering};
        static SHELL_COMMAND_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let sequence = SHELL_COMMAND_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let action_id = format!("shell-command-{}", sequence);

        // Truncate command for display (first 50 chars)
        let command_display = if command.len() > 50 {
            format!("{}...", &command[..50])
        } else {
            command.clone()
        };

        // Build action node for tree events
        let action_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("SHELL: {}", step_name),
            "timestamp": timestamp,
            "status": "pending",
            "metadata": {
                "command": &command_display,
                "shell_type": shell_type,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "timeout_seconds": timeout_secs,
            }
        });

        // Emit action_started tree event to file log
        FileLogger::log_tree_event("action_started", &action_node, &[], timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "action_started".to_string(),
                timestamp,
                data: json!({ "node": action_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event("action_started", &action_node, timestamp, sequence);

        // NOTE: Database event logging is handled by the unified workflow executor
        // (execute_steps_with_log_sources -> log_step_event) to avoid duplicates.
        // Tree events above are still emitted for the Session/Actions page.

        // Build the command - use PowerShell for PowerShell syntax on Windows
        let mut cmd = if cfg!(target_os = "windows") {
            if is_powershell {
                let mut c = Command::new("powershell");
                c.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
                c
            } else {
                let mut c = Command::new("cmd");
                c.args(["/C", &command]);
                c
            }
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &command]);
            c
        };

        // Set working directory if specified
        if let Some(ref wd) = working_directory {
            cmd.current_dir(wd);
        }

        // Capture stdout and stderr
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Execute with optional timeout
        let start = std::time::Instant::now();

        // Process the result - execute with or without timeout depending on setting
        let (success, exit_code, stdout, stderr) = if let Some(timeout_secs_val) = timeout_secs {
            // Execute with timeout
            let timeout_duration = Duration::from_secs(timeout_secs_val);
            let output_result = timeout(timeout_duration, cmd.output()).await;

            match output_result {
                Ok(Ok(output)) => {
                    let exit_code = output.status.code();
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let success = output.status.success();
                    (success, exit_code, stdout, stderr)
                }
                Ok(Err(e)) => {
                    warn!("Failed to execute shell command '{}': {}", step_name, e);
                    (
                        false,
                        None,
                        String::new(),
                        format!("Failed to execute command: {}", e),
                    )
                }
                Err(_) => {
                    warn!(
                        "Shell command '{}' timed out after {}s",
                        step_name, timeout_secs_val
                    );
                    (
                        false,
                        None,
                        String::new(),
                        format!("Command timed out after {} seconds", timeout_secs_val),
                    )
                }
            }
        } else {
            // No timeout - execute without timeout wrapper
            match cmd.output().await {
                Ok(output) => {
                    let exit_code = output.status.code();
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let success = output.status.success();
                    (success, exit_code, stdout, stderr)
                }
                Err(e) => {
                    warn!("Failed to execute shell command '{}': {}", step_name, e);
                    (
                        false,
                        None,
                        String::new(),
                        format!("Failed to execute command: {}", e),
                    )
                }
            }
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        info!(
            "Shell command '{}' completed: success={}, exit_code={:?}, duration={}ms",
            step_name, success, exit_code, duration_ms
        );

        // Log output if present
        if !stdout.is_empty() {
            info!("Shell command stdout:\n{}", stdout.trim());
        }
        if !stderr.is_empty() {
            if success {
                info!("Shell command stderr:\n{}", stderr.trim());
            } else {
                warn!("Shell command stderr:\n{}", stderr.trim());
            }
        }

        // Determine overall success based on fail_on_error setting
        let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let duration = end_timestamp - timestamp;

        // Truncate stdout/stderr for display
        let stdout_display = if stdout.len() > 200 {
            format!("{}...", &stdout[..200])
        } else {
            stdout.clone()
        };
        let stderr_display = if stderr.len() > 200 {
            format!("{}...", &stderr[..200])
        } else {
            stderr.clone()
        };

        let (final_success, error_msg, output_data) = if success {
            // Return stdout in the screenshot_path field (repurposed for output data)
            let output_data = if stdout.is_empty() {
                None
            } else {
                Some(stdout.clone())
            };
            (true, None, output_data)
        } else if fail_on_error {
            let error_msg = if !stderr.is_empty() {
                format!(
                    "Command failed (exit code {:?}): {}",
                    exit_code,
                    stderr.trim()
                )
            } else {
                format!("Command failed with exit code {:?}", exit_code)
            };
            (false, Some(error_msg), None)
        } else {
            // Return success but include the error message
            info!(
                "Shell command '{}' failed but fail_on_error=false, continuing",
                step_name
            );
            let error_msg = if !stderr.is_empty() {
                format!("(ignored) Command failed: {}", stderr.trim())
            } else {
                format!("(ignored) Command failed with exit code {:?}", exit_code)
            };
            (true, Some(error_msg), Some(stdout.clone()))
        };

        // Build completed action node
        let completed_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("SHELL: {}", step_name),
            "timestamp": end_timestamp,
            "status": if final_success { "success" } else { "failed" },
            "duration": duration,
            "metadata": {
                "command": &command_display,
                "shell_type": shell_type,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "exit_code": exit_code,
                "stdout": &stdout_display,
                "stderr": &stderr_display,
                "duration_ms": duration_ms,
            }
        });

        let event_type = if final_success {
            "action_completed"
        } else {
            "action_failed"
        };

        // Emit completion tree event to file log
        FileLogger::log_tree_event(event_type, &completed_node, &[], end_timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.to_string(),
                timestamp: end_timestamp,
                data: json!({ "node": completed_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

        // NOTE: Database event logging is handled by the unified workflow executor
        // (execute_steps_with_log_sources -> log_step_event) to avoid duplicates.
        // Tree events above are still emitted for the Session/Actions page.

        (final_success, error_msg, output_data)
    }

    // =========================================================================
    // API Request Step Execution
    // =========================================================================

    /// Parse an output variable specification to extract variable name and optional JSON path.
    ///
    /// Supports two formats:
    /// - `var_name` - stores entire response in `var_name`
    /// - `var_name.path.to.field` - stores value at JSON path `$.path.to.field` in `var_name`
    ///
    /// Returns (variable_name, optional_json_path) where json_path is in JSONPath syntax (e.g., `$.path.to.field`)
    fn parse_output_variable_spec(spec: &str) -> (String, Option<String>) {
        if let Some(dot_pos) = spec.find('.') {
            let var_name = spec[..dot_pos].to_string();
            let path_part = &spec[dot_pos + 1..];
            // Convert dot notation to JSONPath: "data.token" -> "$.data.token"
            let json_path = format!("$.{}", path_part);
            (var_name, Some(json_path))
        } else {
            (spec.to_string(), None)
        }
    }

    /// Extract a value from a JSON string using a JSONPath expression.
    ///
    /// Returns the extracted value as a string, or None if:
    /// - The JSON is invalid
    /// - The path doesn't match anything (returns Null)
    /// - The path syntax is invalid
    fn extract_json_path_value(json_body: &str, json_path: &str) -> Option<String> {
        let finder = match JsonPathFinder::from_str(json_body, json_path) {
            Ok(f) => f,
            Err(e) => {
                warn!("Invalid JSON path '{}': {}", json_path, e);
                return None;
            }
        };

        let results = finder.find_slice();
        if results.is_empty() {
            warn!("JSON path '{}' matched no values", json_path);
            return None;
        }

        // Get first result and convert to string
        // Note: jsonpath_rust returns Null for missing fields, which we treat as "not found"
        let data = results[0].clone().to_data();

        // Treat Null as "not found" - the field doesn't exist in the JSON
        if data.is_null() {
            warn!(
                "JSON path '{}' resolved to null (field not found)",
                json_path
            );
            return None;
        }

        let value = match data {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => unreachable!(), // Already handled above
            other => other.to_string(),                // Arrays/objects become JSON strings
        };

        Some(value)
    }

    /// Execute an API request step using ApiRequestSession for proper variable chaining.
    ///
    /// Variables from the SharedVariableStore can be referenced in the URL, headers, and body
    /// using `{{variable_name}}` syntax. This enables API request chaining where response
    /// data from one request can be used in subsequent requests.
    ///
    /// The function uses ApiRequestSession which provides:
    /// - Proper variable resolution via VariableResolver
    /// - JSON path extractions for extracting specific fields from responses
    /// - Automatic variable storage for chained requests
    async fn execute_api_request_step(
        &self,
        step: &ExecutionStepConfig,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<String>, Option<String>) {
        // Validate required fields
        let method_str = match &step.api_method {
            Some(m) => m.to_uppercase(),
            None => {
                return (
                    false,
                    Some("No HTTP method specified for API request".to_string()),
                    None,
                );
            }
        };

        let url = match &step.api_url {
            Some(u) => u.clone(),
            None => {
                return (
                    false,
                    Some("No URL specified for API request".to_string()),
                    None,
                );
            }
        };

        // Parse HTTP method
        let method = match method_str.as_str() {
            "GET" => HttpMethod::Get,
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "PATCH" => HttpMethod::Patch,
            "DELETE" => HttpMethod::Delete,
            _ => {
                return (
                    false,
                    Some(format!("Unsupported HTTP method: {}", method_str)),
                    None,
                );
            }
        };

        let step_name = step.name.as_deref().unwrap_or("API Request");

        // First, expand variables in URL using RuntimeContext (for step outputs, etc.)
        let evaluator = ExpressionEvaluator::new();
        let url_with_context = evaluator.evaluate(&url, &self.runtime_context);

        // Build headers HashMap from serde_json::Value
        let headers: Option<HashMap<String, String>> = step.api_headers.as_ref().and_then(|h| {
            h.as_object().map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| {
                        v.as_str().map(|s| {
                            // Expand variables in header values
                            let expanded = evaluator.evaluate(s, &self.runtime_context);
                            (k.clone(), expanded)
                        })
                    })
                    .collect()
            })
        });

        // Expand variables in body
        let body = step
            .api_body
            .as_ref()
            .map(|b| evaluator.evaluate(b, &self.runtime_context));

        // Build extractions list - combine api_extractions with legacy api_output_variable
        let mut extractions = step.api_extractions.clone().unwrap_or_default();

        // Support legacy api_output_variable format: "var_name.path.to.field" extracts $.path.to.field
        if let Some(ref var_spec) = step.api_output_variable {
            let (var_name, json_path) = Self::parse_output_variable_spec(var_spec);
            let json_path_str = json_path.unwrap_or_else(|| "$".to_string());
            extractions.push(VariableExtraction {
                variable_name: var_name,
                json_path: json_path_str,
                default_value: None,
            });
        }

        // Determine timeout: use step-specific, then function parameter, then default
        let timeout_ms = step
            .api_timeout_ms
            .or_else(|| timeout_secs.map(|s| s * 1000))
            .unwrap_or(30_000);

        // Build ApiRequestConfig
        let config = ApiRequestConfig {
            step_id: None,
            step_name: Some(step_name.to_string()),
            method,
            url: url_with_context.clone(),
            resolved_url: None,
            headers,
            body,
            content_type: step.api_content_type.clone(),
            timeout_ms: Some(timeout_ms),
            follow_redirects: Some(true),
            credential_id: None,
            extractions: if extractions.is_empty() {
                None
            } else {
                Some(extractions)
            },
            assertions: None,
        };

        info!(
            "Executing API request '{}': {} {} (timeout: {}ms)",
            step_name, method_str, url_with_context, timeout_ms
        );

        // Create session from shared variable store for proper variable chaining
        let mut session = ApiRequestSession::from_shared_store(&self.shared_variables);

        // Execute the request
        match session.execute(&config, None).await {
            Ok(result) => {
                // Sync extracted variables back to the shared store
                session.sync_to_shared_store(&self.shared_variables);

                // Log extraction results
                for ext in &result.extractions {
                    if ext.success {
                        if let Some(ref value) = ext.extracted_value {
                            let preview = if value.len() > 50 {
                                format!("{}...", &value[..50])
                            } else {
                                value.clone()
                            };
                            info!(
                                "Extracted '{}' using JSON path '{}' -> '{}' ({} chars)",
                                ext.variable_name,
                                ext.json_path,
                                preview,
                                value.len()
                            );
                        }
                    } else if let Some(ref error) = ext.error {
                        warn!(
                            "Extraction failed for '{}' ({}): {}",
                            ext.variable_name, ext.json_path, error
                        );
                    }
                }

                info!(
                    "API request '{}' completed: status={}, duration={}ms, extractions={}",
                    step_name,
                    result.status_code,
                    result.response_time_ms,
                    result.extractions.len()
                );

                if result.success {
                    (true, None, result.response_body)
                } else {
                    let error_msg = result.error.unwrap_or_else(|| {
                        format!(
                            "HTTP {}: {}",
                            result.status_code,
                            result
                                .response_body
                                .as_ref()
                                .map(|b| b.chars().take(500).collect::<String>())
                                .unwrap_or_default()
                        )
                    });
                    (false, Some(error_msg), result.response_body)
                }
            }
            Err(e) => (false, Some(format!("API request failed: {}", e)), None),
        }
    }

    // =========================================================================
    // Check Step Execution
    // =========================================================================

    /// Execute a code quality check step
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_check_step(
        &self,
        step: &ExecutionStepConfig,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<String>, Option<String>) {
        use std::process::Stdio;
        use tokio::process::Command;
        use tokio::time::{timeout, Duration};

        // Debug logging to trace check_type values
        info!(
            "execute_check_step: step_name={:?}, check_type={:?}, check_command={:?}, working_dir={:?}",
            step.name, step.check_type, step.check_command, step.check_working_directory
        );

        let check_type = step.check_type.as_deref().unwrap_or("custom_command");
        let step_name = step.name.as_deref().unwrap_or("Check");
        // Note: Due to serde alias conflict, "working_directory" goes to shell_command_working_directory
        // So we check both fields for backwards compatibility
        let working_directory = step
            .check_working_directory
            .clone()
            .or_else(|| step.shell_command_working_directory.clone());

        // Handle http_status check type separately (doesn't need language detection)
        if check_type == "http_status" {
            return self
                .execute_http_status_check(step, step_name, timeout_secs)
                .await;
        }

        // Detect project type from working directory to auto-select appropriate tools
        let detected_language = {
            let work_dir = working_directory.as_deref().unwrap_or(".");
            let path = std::path::Path::new(work_dir);

            if path.join("Cargo.toml").exists() {
                "rust"
            } else if path.join("pyproject.toml").exists()
                || path.join("setup.py").exists()
                || path.join("requirements.txt").exists()
            {
                "python"
            } else if path.join("go.mod").exists() {
                "go"
            } else if path.join("tsconfig.json").exists() {
                "typescript"
            } else if path.join("package.json").exists() {
                "javascript"
            } else if path.join("CMakeLists.txt").exists()
                || path.join("Makefile").exists()
                || path.join("configure.ac").exists()
            {
                "c_cpp"
            } else if path.join("build.gradle").exists()
                || path.join("build.gradle.kts").exists()
                || path.join("pom.xml").exists()
            {
                "java"
            } else if path.join("mix.exs").exists() {
                "elixir"
            } else if path.join("Gemfile").exists() {
                "ruby"
            } else if path.join("composer.json").exists() {
                "php"
            } else if path.join("Package.swift").exists() {
                "swift"
            } else if path.join("*.csproj").exists() || path.join("*.sln").exists() {
                // Note: glob patterns don't work with exists(), but we'll check for common .NET files
                "dotnet"
            } else {
                "unknown"
            }
        };

        // Additional check for .NET projects (need to actually scan directory)
        let detected_language = if detected_language == "unknown" {
            let work_dir = working_directory.as_deref().unwrap_or(".");
            let path = std::path::Path::new(work_dir);
            if let Ok(entries) = std::fs::read_dir(path) {
                let has_dotnet = entries.filter_map(|e| e.ok()).any(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    name.ends_with(".csproj") || name.ends_with(".sln") || name.ends_with(".fsproj")
                });
                if has_dotnet {
                    "dotnet"
                } else {
                    "unknown"
                }
            } else {
                "unknown"
            }
        } else {
            detected_language
        };

        info!(
            "Check step '{}': detected language = {}",
            step_name, detected_language
        );

        // Get the command to run - auto-detect based on language if not specified
        // Note: Due to serde alias conflict, "command" in JSON goes to shell_command, not check_command
        // So we check both fields for backwards compatibility with frontend using "command" field
        let explicit_command = step
            .check_command
            .as_ref()
            .filter(|s| !s.is_empty())
            .or_else(|| step.shell_command.as_ref().filter(|s| !s.is_empty()));

        let command = match explicit_command {
            Some(cmd) => Some(cmd.clone()),
            None => {
                // Auto-select commands based on detected language and check type
                match (check_type, detected_language) {
                    // Python checks
                    ("lint", "python") => Some("ruff check .".to_string()),
                    ("format", "python") => Some("black --check .".to_string()),
                    ("typecheck", "python") => Some("mypy .".to_string()),
                    ("analyze", "python") => Some("ruff check . --statistics".to_string()),
                    ("security", "python") => Some("pip-audit".to_string()),

                    // Rust checks
                    ("lint", "rust") => Some("cargo clippy -- -D warnings".to_string()),
                    ("format", "rust") => Some("cargo fmt --check".to_string()),
                    ("typecheck", "rust") => Some("cargo check".to_string()),
                    ("analyze", "rust") => Some("cargo clippy --all-targets --all-features".to_string()),
                    ("security", "rust") => Some("cargo audit".to_string()),

                    // Go checks
                    ("lint", "go") => Some("golangci-lint run".to_string()),
                    ("format", "go") => Some("gofmt -l .".to_string()),
                    ("typecheck", "go") => Some("go vet ./...".to_string()),
                    ("analyze", "go") => Some("go vet ./... && staticcheck ./...".to_string()),
                    ("security", "go") => Some("gosec ./...".to_string()),

                    // TypeScript checks
                    ("lint", "typescript") => Some("npx eslint . --ext .ts,.tsx".to_string()),
                    ("format", "typescript") => Some("npx prettier --check .".to_string()),
                    ("typecheck", "typescript") => Some("npx tsc --noEmit".to_string()),
                    ("analyze", "typescript") => Some("npx eslint . --ext .ts,.tsx --format json".to_string()),
                    ("security", "typescript") => Some("npm audit".to_string()),

                    // JavaScript checks
                    ("lint", "javascript") => Some("npx eslint .".to_string()),
                    ("format", "javascript") => Some("npx prettier --check .".to_string()),
                    ("typecheck", "javascript") => None, // No typecheck for plain JS
                    ("analyze", "javascript") => Some("npx eslint . --format json".to_string()),
                    ("security", "javascript") => Some("npm audit".to_string()),

                    // C/C++ checks (using common tools)
                    ("lint", "c_cpp") => Some("cppcheck --enable=all .".to_string()),
                    ("format", "c_cpp") => Some("clang-format --dry-run -Werror **/*.cpp **/*.c **/*.h".to_string()),
                    ("typecheck", "c_cpp") => Some("make -n".to_string()), // Dry-run make
                    ("analyze", "c_cpp") => Some("cppcheck --enable=all --xml .".to_string()),
                    ("security", "c_cpp") => Some("flawfinder .".to_string()),

                    // Java checks
                    ("lint", "java") => Some("./gradlew checkstyleMain || mvn checkstyle:check".to_string()),
                    ("format", "java") => Some("./gradlew spotlessCheck || mvn spotless:check".to_string()),
                    ("typecheck", "java") => Some("./gradlew compileJava || mvn compile".to_string()),
                    ("analyze", "java") => Some("./gradlew pmd || mvn pmd:check".to_string()),
                    ("security", "java") => Some("./gradlew dependencyCheckAnalyze || mvn org.owasp:dependency-check-maven:check".to_string()),

                    // Ruby checks
                    ("lint", "ruby") => Some("bundle exec rubocop".to_string()),
                    ("format", "ruby") => Some("bundle exec rubocop --format offenses".to_string()),
                    ("typecheck", "ruby") => Some("bundle exec srb tc".to_string()), // Sorbet
                    ("analyze", "ruby") => Some("bundle exec rubocop --format json".to_string()),
                    ("security", "ruby") => Some("bundle exec bundler-audit check".to_string()),

                    // PHP checks
                    ("lint", "php") => Some("./vendor/bin/phpcs".to_string()),
                    ("format", "php") => Some("./vendor/bin/php-cs-fixer fix --dry-run --diff".to_string()),
                    ("typecheck", "php") => Some("./vendor/bin/phpstan analyse".to_string()),
                    ("analyze", "php") => Some("./vendor/bin/phpmd . text cleancode,codesize,controversial".to_string()),
                    ("security", "php") => Some("composer audit".to_string()),

                    // Elixir checks
                    ("lint", "elixir") => Some("mix credo".to_string()),
                    ("format", "elixir") => Some("mix format --check-formatted".to_string()),
                    ("typecheck", "elixir") => Some("mix dialyzer".to_string()),
                    ("analyze", "elixir") => Some("mix credo --format json".to_string()),
                    ("security", "elixir") => Some("mix deps.audit".to_string()),

                    // Swift checks
                    ("lint", "swift") => Some("swiftlint".to_string()),
                    ("format", "swift") => Some("swiftformat --lint .".to_string()),
                    ("typecheck", "swift") => Some("swift build".to_string()),
                    ("analyze", "swift") => Some("swiftlint --reporter json".to_string()),
                    ("security", "swift") => None, // No standard security tool

                    // .NET checks
                    ("lint", "dotnet") => Some("dotnet format --verify-no-changes".to_string()),
                    ("format", "dotnet") => Some("dotnet format --verify-no-changes".to_string()),
                    ("typecheck", "dotnet") => Some("dotnet build --no-restore".to_string()),
                    ("analyze", "dotnet") => Some("dotnet build /p:TreatWarningsAsErrors=true".to_string()),
                    ("security", "dotnet") => Some("dotnet list package --vulnerable".to_string()),

                    // Unknown language - skip gracefully
                    (check_type_val, "unknown") => {
                        warn!(
                            "Check step '{}': No language detected, skipping {} check. \
                            Specify a command explicitly or ensure project has recognizable marker files.",
                            step_name, check_type_val
                        );
                        None
                    }

                    // Catch-all for unrecognized check types on known languages
                    _ => {
                        warn!(
                            "Check step '{}': Unsupported check type '{}' for language '{}', skipping.",
                            step_name, check_type, detected_language
                        );
                        None
                    }
                }
            }
        };

        // Handle the case where no command could be determined (skip gracefully)
        let command = match command {
            Some(cmd) => cmd,
            None => {
                info!(
                    "Check step '{}' skipped: no applicable check for type '{}' and language '{}'",
                    step_name, check_type, detected_language
                );
                // Return success with a warning message
                return (
                    true,
                    Some(format!(
                        "Skipped: No {} check available for {} projects. Specify a command explicitly if needed.",
                        check_type, detected_language
                    )),
                    None,
                );
            }
        };
        let auto_fix = step.check_auto_fix.unwrap_or(false);

        // Modify command for auto-fix if enabled (language-aware)
        let final_command = if auto_fix {
            match (check_type, detected_language) {
                // Python auto-fix
                ("lint", "python") => command.replace("ruff check", "ruff check --fix"),
                ("format", "python") => command.replace("--check", ""),

                // Rust auto-fix
                ("lint", "rust") => command.replace("cargo clippy", "cargo clippy --fix"),
                ("format", "rust") => command.replace("--check", ""),

                // Go auto-fix
                ("lint", "go") => command.replace("golangci-lint run", "golangci-lint run --fix"),
                ("format", "go") => command.replace("gofmt -l", "gofmt -w"),

                // TypeScript/JavaScript auto-fix
                ("lint", "typescript") | ("lint", "javascript") => {
                    if command.contains("eslint") {
                        format!("{} --fix", command)
                    } else {
                        command.replace("lint", "lint:fix")
                    }
                }
                ("format", "typescript") | ("format", "javascript") => {
                    if command.contains("prettier") {
                        command.replace("--check", "--write")
                    } else {
                        command
                            .replace("format:check", "format")
                            .replace("--check", "")
                    }
                }

                // C/C++ auto-fix
                ("format", "c_cpp") => command.replace("--dry-run -Werror", "-i"),

                // Ruby auto-fix
                ("lint", "ruby") | ("format", "ruby") => format!("{} --autocorrect", command),

                // PHP auto-fix
                ("lint", "php") => command.replace("phpcs", "phpcbf"),
                ("format", "php") => command.replace("--dry-run --diff", ""),

                // Elixir auto-fix
                ("format", "elixir") => command.replace("--check-formatted", ""),

                // Swift auto-fix
                ("lint", "swift") => format!("{} --fix", command),
                ("format", "swift") => command.replace("--lint", ""),

                // .NET auto-fix
                ("lint", "dotnet") | ("format", "dotnet") => {
                    command.replace("--verify-no-changes", "")
                }

                // For languages without auto-fix, just return the command as-is
                _ => command,
            }
        } else {
            command
        };

        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());
        info!(
            "Executing check '{}' ({}): {} (timeout: {}, working_dir: {:?})",
            step_name, check_type, final_command, timeout_str, working_directory
        );

        // Generate sequence number and timestamp for tree events
        use std::sync::atomic::{AtomicU32, Ordering};
        static CHECK_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let sequence = CHECK_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let action_id = format!("check-{}", sequence);

        // Truncate command for display
        let command_display = if final_command.len() > 50 {
            format!("{}...", &final_command[..50])
        } else {
            final_command.clone()
        };

        // Build action node for tree events
        let action_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("CHECK: {}", step_name),
            "timestamp": timestamp,
            "status": "pending",
            "metadata": {
                "check_type": check_type,
                "command": &command_display,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "auto_fix": auto_fix,
                "timeout_seconds": timeout_secs,
            }
        });

        // Emit action_started tree event to file log
        FileLogger::log_tree_event("action_started", &action_node, &[], timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "action_started".to_string(),
                timestamp,
                data: json!({ "node": action_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event("action_started", &action_node, timestamp, sequence);

        // Build the command
        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", &final_command]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &final_command]);
            c
        };

        // Set working directory if specified
        if let Some(ref wd) = working_directory {
            cmd.current_dir(wd);
        }

        // Capture stdout and stderr
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Execute with optional timeout
        let start = std::time::Instant::now();

        // Helper to process command output
        let process_output = |output: std::process::Output,
                              duration_ms: u64|
         -> (bool, Option<String>, Option<String>) {
            let exit_code = output.status.code();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let success = output.status.success();

            info!(
                "Check '{}' completed: success={}, exit_code={:?}, duration={}ms",
                step_name, success, exit_code, duration_ms
            );

            if success {
                let output_data = if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                };
                (true, None, output_data)
            } else {
                // IMPORTANT: Capture BOTH stdout and stderr for failed checks
                // so the AI can see the full error context for fixing
                let mut combined_output = String::new();
                if !stdout.is_empty() {
                    combined_output.push_str("=== STDOUT ===\n");
                    combined_output.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !combined_output.is_empty() {
                        combined_output.push_str("\n\n");
                    }
                    combined_output.push_str("=== STDERR ===\n");
                    combined_output.push_str(&stderr);
                }
                let error_summary = if !stderr.is_empty() {
                    stderr.lines().take(5).collect::<Vec<_>>().join("\n")
                } else {
                    stdout.lines().take(5).collect::<Vec<_>>().join("\n")
                };
                (
                    false,
                    Some(format!(
                        "Check failed (exit code {:?}): {}",
                        exit_code,
                        error_summary.trim()
                    )),
                    Some(combined_output), // Return full output for AI context
                )
            }
        };

        // Process the result - execute with or without timeout depending on setting
        let (final_success, error_msg, output_data) = if let Some(timeout_secs_val) = timeout_secs {
            // Execute with timeout
            let timeout_duration = Duration::from_secs(timeout_secs_val);
            let output_result = timeout(timeout_duration, cmd.output()).await;
            let duration_ms = start.elapsed().as_millis() as u64;

            match output_result {
                Ok(Ok(output)) => process_output(output, duration_ms),
                Ok(Err(e)) => {
                    warn!("Failed to execute check '{}': {}", step_name, e);
                    (false, Some(format!("Failed to execute check: {}", e)), None)
                }
                Err(_) => {
                    warn!(
                        "Check '{}' timed out after {}s",
                        step_name, timeout_secs_val
                    );
                    (
                        false,
                        Some(format!(
                            "Check timed out after {} seconds",
                            timeout_secs_val
                        )),
                        None,
                    )
                }
            }
        } else {
            // No timeout - execute without timeout wrapper
            let duration_ms = start.elapsed().as_millis() as u64;
            match cmd.output().await {
                Ok(output) => process_output(output, duration_ms),
                Err(e) => {
                    warn!("Failed to execute check '{}': {}", step_name, e);
                    (false, Some(format!("Failed to execute check: {}", e)), None)
                }
            }
        };

        // Emit completion event
        let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let duration = end_timestamp - timestamp;
        let total_duration_ms = start.elapsed().as_millis() as u64;

        let completed_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("CHECK: {}", step_name),
            "timestamp": end_timestamp,
            "status": if final_success { "success" } else { "failed" },
            "duration": duration,
            "metadata": {
                "check_type": check_type,
                "command": &command_display,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "duration_ms": total_duration_ms,
                "error": error_msg.as_deref().unwrap_or(""),
            }
        });

        let event_type = if final_success {
            "action_completed"
        } else {
            "action_failed"
        };

        // Emit completion tree event to file log
        FileLogger::log_tree_event(event_type, &completed_node, &[], end_timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.to_string(),
                timestamp: end_timestamp,
                data: json!({ "node": completed_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

        (final_success, error_msg, output_data)
    }

    // =========================================================================
    // HTTP Status Check Execution
    // =========================================================================

    /// Execute an HTTP status check
    ///
    /// Makes an HTTP GET request to the specified URL and verifies the status code
    /// matches the expected value. Useful for health checks before running tests.
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_http_status_check(
        &self,
        step: &ExecutionStepConfig,
        step_name: &str,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<String>, Option<String>) {
        use std::time::Duration;

        // Get the URL to check
        let url = match &step.check_url {
            Some(u) => u.clone(),
            None => {
                return (
                    false,
                    Some("check_url is required for http_status check".to_string()),
                    None,
                );
            }
        };

        let expected_status = step.expected_status.unwrap_or(200);
        // Cap at 5 minutes if specified, otherwise use a large default for the HTTP client
        let timeout = timeout_secs
            .map(|t| Duration::from_secs(t.min(300)))
            .unwrap_or(Duration::from_secs(300)); // 5 min default for HTTP checks
        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());

        info!(
            "Executing HTTP status check '{}': url={}, expected_status={}, timeout={}",
            step_name, url, expected_status, timeout_str
        );

        // Generate sequence number and timestamp for tree events
        use std::sync::atomic::{AtomicU32, Ordering};
        static HTTP_CHECK_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let sequence = HTTP_CHECK_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let action_id = format!("http-check-{}", sequence);

        // Build action node for tree events
        let action_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("HTTP CHECK: {}", step_name),
            "timestamp": timestamp,
            "status": "pending",
            "metadata": {
                "check_type": "http_status",
                "url": &url,
                "expected_status": expected_status,
                "timeout_seconds": timeout.as_secs(),
            }
        });

        // Emit action_started tree event to file log
        FileLogger::log_tree_event("action_started", &action_node, &[], timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "action_started".to_string(),
                timestamp,
                data: json!({ "node": action_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event("action_started", &action_node, timestamp, sequence);

        // Make the HTTP request
        let start = std::time::Instant::now();
        let client = match reqwest::Client::builder().timeout(timeout).build() {
            Ok(c) => c,
            Err(e) => {
                let error_msg = format!("Failed to create HTTP client: {}", e);
                warn!("{}", error_msg);
                return (false, Some(error_msg), None);
            }
        };

        let result = client.get(&url).send().await;
        let duration_ms = start.elapsed().as_millis() as u64;

        // Process the result
        let (final_success, error_msg, output_data) = match result {
            Ok(response) => {
                let actual_status = response.status().as_u16();
                info!(
                    "HTTP check '{}' completed: actual_status={}, expected={}, duration={}ms",
                    step_name, actual_status, expected_status, duration_ms
                );

                if actual_status == expected_status {
                    (
                        true,
                        None,
                        Some(
                            json!({
                                "status": actual_status,
                                "url": url,
                                "duration_ms": duration_ms
                            })
                            .to_string(),
                        ),
                    )
                } else {
                    (
                        false,
                        Some(format!(
                            "Expected status {} but got {} from {}",
                            expected_status, actual_status, url
                        )),
                        Some(
                            json!({
                                "status": actual_status,
                                "expected": expected_status,
                                "url": url,
                                "duration_ms": duration_ms
                            })
                            .to_string(),
                        ),
                    )
                }
            }
            Err(e) => {
                // Categorize error for better AI understanding
                let error_msg = if e.is_connect() {
                    format!(
                        "Server not running at {} - Connection refused. Make sure the service is started.",
                        url
                    )
                } else if e.is_timeout() {
                    format!(
                        "Server at {} not responding - Request timed out after {}s. The service may be overloaded or not running.",
                        url, timeout.as_secs()
                    )
                } else if e.is_request() {
                    format!("Invalid request to {}: {}", url, e)
                } else {
                    format!("Failed to reach {}: {}", url, e)
                };

                warn!("HTTP check '{}' failed: {}", step_name, error_msg);
                (
                    false,
                    Some(error_msg.clone()),
                    Some(
                        json!({
                            "error": error_msg,
                            "url": url,
                            "duration_ms": duration_ms
                        })
                        .to_string(),
                    ),
                )
            }
        };

        // Emit completion event
        let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let duration = end_timestamp - timestamp;

        let completed_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("HTTP CHECK: {}", step_name),
            "timestamp": end_timestamp,
            "status": if final_success { "success" } else { "failed" },
            "duration": duration,
            "metadata": {
                "check_type": "http_status",
                "url": &url,
                "expected_status": expected_status,
                "duration_ms": duration_ms,
                "error": error_msg.as_deref().unwrap_or(""),
            }
        });

        let event_type = if final_success {
            "action_completed"
        } else {
            "action_failed"
        };

        // Emit completion tree event to file log
        FileLogger::log_tree_event(event_type, &completed_node, &[], end_timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.to_string(),
                timestamp: end_timestamp,
                data: json!({ "node": completed_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

        (final_success, error_msg, output_data)
    }

    // =========================================================================
    // Check Group Step Execution
    // =========================================================================

    /// Execute all checks in a check group
    /// Returns: (success, error_message, summary_text, individual_check_results)
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_check_group_step(
        &self,
        step: &ExecutionStepConfig,
        _timeout_secs: Option<u64>,
    ) -> (
        bool,
        Option<String>,
        Option<String>,
        Option<Vec<IndividualCheckResult>>,
    ) {
        let step_name = step.name.as_deref().unwrap_or("Check Group");
        let group_id = match &step.check_group_id {
            Some(id) => id.clone(),
            None => {
                return (
                    false,
                    Some("No check group ID specified for check_group step".to_string()),
                    None,
                    None,
                );
            }
        };

        info!(
            "execute_check_group_step: step_name={:?}, group_id={:?}",
            step_name, group_id
        );

        // Get the checkpoint_db from app_state
        let db = &self.app_state.checkpoint_db;

        // Get the group
        let group = match db.get_check_group(&group_id) {
            Ok(Some(g)) => g,
            Ok(None) => {
                return (
                    false,
                    Some(format!("Check group not found: {}", group_id)),
                    None,
                    None,
                );
            }
            Err(e) => {
                return (
                    false,
                    Some(format!("Failed to get check group: {}", e)),
                    None,
                    None,
                );
            }
        };

        if !group.enabled {
            info!("Check group '{}' is disabled, skipping", group.name);
            return (
                true,
                None,
                Some(format!("Check group '{}' is disabled", group.name)),
                None,
            );
        }

        // Get checks in the group
        let checks = match db.get_checks_in_group(&group_id) {
            Ok(c) => c,
            Err(e) => {
                return (
                    false,
                    Some(format!("Failed to get checks in group: {}", e)),
                    None,
                    None,
                );
            }
        };

        if checks.is_empty() {
            return (
                true,
                None,
                Some(format!("No checks in group '{}'", group.name)),
                None,
            );
        }

        info!(
            "Executing check group '{}' with {} checks (stop_on_failure: {})",
            group.name,
            checks.len(),
            group.stop_on_failure
        );

        // Execute each check
        use crate::check_executor::{execute_check, CheckDefinition, CheckTool, CheckType};
        use std::time::Instant;

        let start_time = Instant::now();
        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;
        let mut results_output = Vec::new();
        let mut check_results: Vec<IndividualCheckResult> = Vec::new();

        for check in &checks {
            if !check.enabled {
                results_output.push(format!("  [SKIPPED] {} (disabled)", check.name));
                check_results.push(IndividualCheckResult {
                    name: check.name.clone(),
                    status: "skipped".to_string(),
                    duration_ms: 0,
                    issues_found: 0,
                    issues_fixed: 0,
                    files_checked: 0,
                    error_message: Some("Check is disabled".to_string()),
                    output: None,
                    issues: Vec::new(),
                });
                skipped += 1;
                continue;
            }

            let check_def = CheckDefinition {
                id: check.id.clone(),
                name: check.name.clone(),
                check_type: serde_json::from_str(&format!("\"{}\"", check.check_type))
                    .unwrap_or(CheckType::Lint),
                tool: serde_json::from_str(&format!("\"{}\"", check.tool))
                    .unwrap_or(CheckTool::Custom),
                command: check.command.clone(),
                working_directory: check.working_directory.clone(),
                config_path: check.config_path.clone(),
                auto_fix: check.auto_fix,
                fail_on_warning: check.fail_on_warning,
                timeout_seconds: check.timeout_seconds,
                is_critical: check.is_critical,
            };

            let result = execute_check(&check_def);
            let is_success = result.is_success();

            // Extract issues from structured output (limit to 50 to avoid huge payloads)
            let issues: Vec<CheckIssueDetail> = result
                .structured_output
                .as_ref()
                .map(|so| {
                    so.issues
                        .iter()
                        .take(50) // Limit to 50 issues per check
                        .map(|issue| CheckIssueDetail {
                            file: issue.file.clone(),
                            line: issue.line,
                            column: issue.column,
                            code: issue.code.clone(),
                            message: issue.message.clone(),
                            severity: format!("{:?}", issue.severity).to_lowercase(),
                            fixable: issue.fixable,
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Build individual check result
            let check_result = IndividualCheckResult {
                name: check.name.clone(),
                status: if is_success { "passed" } else { "failed" }.to_string(),
                duration_ms: result.duration_ms,
                issues_found: result.issues_found,
                issues_fixed: result.issues_fixed,
                files_checked: result.files_checked,
                error_message: result.error.clone(),
                output: if result.output.len() > 2000 {
                    Some(format!("{}... (truncated)", &result.output[..2000]))
                } else if !result.output.is_empty() {
                    Some(result.output.clone())
                } else {
                    None
                },
                issues,
            };
            check_results.push(check_result);

            if is_success {
                passed += 1;
                results_output.push(format!(
                    "  [PASSED] {} ({}ms, {} issues found, {} fixed)",
                    check.name, result.duration_ms, result.issues_found, result.issues_fixed
                ));
            } else {
                failed += 1;
                results_output.push(format!(
                    "  [FAILED] {} ({}ms): {}",
                    check.name,
                    result.duration_ms,
                    result.error.as_deref().unwrap_or(&result.output)
                ));

                if group.stop_on_failure {
                    results_output.push("  Stopping due to stop_on_failure setting".to_string());
                    break;
                }
            }
        }

        let duration_ms = start_time.elapsed().as_millis();
        let total = passed + failed;
        let success = failed == 0;

        let summary = format!(
            "Check group '{}': {}/{} passed ({}ms total)\n{}",
            group.name,
            passed,
            total,
            duration_ms,
            results_output.join("\n")
        );

        info!(
            "Check group '{}' completed: {}/{} passed, {} skipped ({}ms)",
            group.name, passed, total, skipped, duration_ms
        );

        if success {
            (true, None, Some(summary), Some(check_results))
        } else {
            (
                false,
                Some(format!(
                    "Check group '{}' failed: {}/{} passed",
                    group.name, passed, total
                )),
                Some(summary),
                Some(check_results),
            )
        }
    }

    // =========================================================================
    // Macro Step Execution
    // =========================================================================

    /// Execute a saved macro by ID
    async fn execute_macro_step(
        &self,
        macro_id: &str,
        monitor_index: Option<i32>,
    ) -> (bool, Option<String>, Option<String>) {
        use crate::macros;

        // Get the macro
        let macro_item = match macros::get_macro(macro_id) {
            Some(m) => m,
            None => {
                return (false, Some(format!("Macro not found: {}", macro_id)), None);
            }
        };

        info!("Executing macro: {} ({})", macro_item.name, macro_id);

        // Increment run count
        if let Err(e) = macros::increment_run_count(macro_id) {
            warn!(
                "Failed to increment run count for macro {}: {}",
                macro_id, e
            );
        }

        let mut all_success = true;
        let mut errors: Vec<String> = Vec::new();

        // Execute each step
        for (idx, step) in macro_item.steps.iter().enumerate() {
            let step_monitor = step.monitor_index.or(monitor_index);

            let result = match step.action_type.as_str() {
                "click" | "double_click" | "right_click" => {
                    if let Some(ref image_ids) = step.target_image_ids {
                        if let Some(first_image_id) = image_ids.first() {
                            self.action_service
                                .execute_action(
                                    &step.action_type,
                                    first_image_id,
                                    None,
                                    step_monitor,
                                )
                                .await
                                .map(|r| r.success)
                                .map_err(|e| format!("{:?}", e))
                        } else {
                            Err("No target image specified".to_string())
                        }
                    } else {
                        Err("No target image IDs specified".to_string())
                    }
                }
                "type" => {
                    if let Some(ref text) = step.text_input {
                        let config = json!({"text": text});
                        self.action_service
                            .execute_action("TYPE", "", Some(&config), step_monitor)
                            .await
                            .map(|r| r.success)
                            .map_err(|e| format!("{:?}", e))
                    } else {
                        Err("No text specified for type action".to_string())
                    }
                }
                "hotkey" => {
                    if let Some(ref hotkey) = step.hotkey {
                        let config = json!({"hotkey": hotkey});
                        self.action_service
                            .execute_action("HOTKEY", "", Some(&config), step_monitor)
                            .await
                            .map(|r| r.success)
                            .map_err(|e| format!("{:?}", e))
                    } else {
                        Err("No hotkey specified".to_string())
                    }
                }
                "go_to_state" => {
                    if let Some(ref state_ids) = step.target_state_ids {
                        if let Some(first_state_id) = state_ids.first() {
                            // Timeouts are disabled by default
                            let timeout = step.timeout_seconds;
                            self.action_service
                                .go_to_state(first_state_id, None, step_monitor, timeout)
                                .await
                                .map(|r| r.success)
                                .map_err(|e| format!("{:?}", e))
                        } else {
                            Err("No target state specified".to_string())
                        }
                    } else {
                        Err("No target state IDs specified".to_string())
                    }
                }
                _ => Err(format!("Unknown action type: {}", step.action_type)),
            };

            match result {
                Ok(success) => {
                    if !success {
                        all_success = false;
                        errors.push(format!("Step {} '{}' failed", idx + 1, step.name));
                    }
                }
                Err(e) => {
                    all_success = false;
                    errors.push(format!("Step {} '{}': {}", idx + 1, step.name, e));
                }
            }

            // Apply pause_after_ms if specified
            if let Some(pause_ms) = step.pause_after_ms {
                tokio::time::sleep(tokio::time::Duration::from_millis(pause_ms as u64)).await;
            }
        }

        let error_msg = if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        };

        (all_success, error_msg, None)
    }

    // =========================================================================
    // Log Watch Step Execution
    // =========================================================================

    /// Execute a log_watch step - scans log files for errors
    ///
    /// This step provides deterministic error detection from log files.
    /// It scans configured log sources within a time window and returns
    /// failure if any errors are found, along with a formatted report.
    async fn execute_log_watch_step(
        &self,
        step: &ExecutionStepConfig,
    ) -> (bool, Option<String>, Option<String>) {
        let step_name = step.name.as_deref().unwrap_or("Log Watch");
        let time_window = step
            .time_window_seconds
            .unwrap_or(DEFAULT_TIME_WINDOW_SECONDS);

        // Get log sources - use configured or global settings defaults
        let log_sources: Vec<String> = step
            .log_sources
            .clone()
            .unwrap_or_else(get_default_log_source_names);

        // Get custom patterns (will be combined with defaults)
        let custom_patterns = step.error_patterns.as_deref();

        info!(
            "Executing log_watch step '{}': sources={:?}, time_window={}s",
            step_name, log_sources, time_window
        );

        // Collect errors from all log sources
        let errors = collect_recent_log_errors(&log_sources, time_window, custom_patterns).await;

        if errors.is_empty() {
            info!("Log watch '{}': No errors detected in logs", step_name);
            (true, None, Some("No errors detected in logs".to_string()))
        } else {
            let error_count = errors.len();
            let formatted_report = format_log_errors_for_ai(&errors);

            warn!(
                "Log watch '{}': Found {} error(s) in logs",
                step_name, error_count
            );

            // Return failure with the formatted report as the output (third parameter)
            // This allows the AI to see the full error details
            (
                false,
                Some(format!("Found {} error(s) in logs", error_count)),
                Some(formatted_report),
            )
        }
    }
}

// ============================================================================
// Log Watch Helper Functions (outside impl block for reusability)
// ============================================================================

/// Collect recent errors from log files
///
/// Reads the tail of each log file, parses timestamps, and extracts
/// error lines within the specified time window.
pub(crate) async fn collect_recent_log_errors(
    log_sources: &[String],
    time_window_seconds: u64,
    custom_patterns: Option<&[String]>,
) -> Vec<LogError> {
    use chrono::Utc;
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let dev_logs_dir = crate::paths::get_dev_logs_dir();
    let cutoff_time = Utc::now() - chrono::Duration::seconds(time_window_seconds as i64);
    let mut all_errors = Vec::new();

    // Build pattern list: defaults + custom
    let mut patterns: Vec<&str> = DEFAULT_ERROR_PATTERNS.to_vec();
    if let Some(custom) = custom_patterns {
        for p in custom {
            patterns.push(p.as_str());
        }
    }

    for source_name in log_sources {
        // If the source name is an absolute path, use it directly;
        // otherwise join with dev_logs_dir (backward compat for workflow configs)
        let source_path = std::path::Path::new(source_name);
        let log_path = if source_path.is_absolute() {
            source_path.to_path_buf()
        } else {
            dev_logs_dir.join(source_name)
        };

        if !log_path.exists() {
            // Log file doesn't exist - this is OK, just skip it
            info!("Log file not found, skipping: {:?}", log_path);
            continue;
        }

        // Read the file
        let file = match File::open(&log_path) {
            Ok(f) => f,
            Err(e) => {
                warn!("Failed to open log file {:?}: {}", log_path, e);
                continue;
            }
        };

        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();
        let total_lines = lines.len();

        // Process lines, looking for errors
        for (line_idx, line) in lines.iter().enumerate() {
            // Check if this line matches any error pattern
            let error_type = find_error_type(line, &patterns);
            if error_type.is_none() {
                continue;
            }
            let error_type = error_type.unwrap();

            // Try to parse timestamp from the line
            let timestamp = extract_timestamp(line);

            // If we have a timestamp, check if it's within the time window
            if let Some(ref ts) = timestamp {
                if let Some(parsed) = parse_log_timestamp(ts) {
                    if parsed < cutoff_time {
                        // This error is older than our time window, skip it
                        continue;
                    }
                }
            }

            // Collect context lines
            let context_before: Vec<String> = lines
                [line_idx.saturating_sub(CONTEXT_LINES)..line_idx]
                .iter()
                .cloned()
                .collect();

            let context_after: Vec<String> = lines
                [(line_idx + 1).min(total_lines)..(line_idx + 1 + CONTEXT_LINES).min(total_lines)]
                .iter()
                .cloned()
                .collect();

            all_errors.push(LogError {
                source: source_name.clone(),
                line_number: line_idx + 1, // 1-indexed
                timestamp,
                message: line.clone(),
                context_before,
                context_after,
                error_type,
            });
        }
    }

    // Limit to avoid overwhelming output (keep most recent 50 errors)
    if all_errors.len() > 50 {
        all_errors = all_errors.into_iter().rev().take(50).rev().collect();
    }

    all_errors
}

/// Find what type of error a line represents, if any
pub(crate) fn find_error_type(line: &str, patterns: &[&str]) -> Option<String> {
    let line_lower = line.to_lowercase();

    // Check each pattern
    for pattern in patterns {
        let pattern_lower = pattern.to_lowercase();
        if line_lower.contains(&pattern_lower) {
            // Categorize the error type
            if pattern_lower.contains("traceback") {
                return Some("traceback".to_string());
            } else if pattern_lower.contains("exception") {
                return Some("exception".to_string());
            } else if pattern_lower.contains("panic") {
                return Some("panic".to_string());
            } else if pattern_lower.contains("fatal") {
                return Some("fatal".to_string());
            } else if pattern_lower.contains("error") {
                return Some("error".to_string());
            } else if pattern_lower.contains("failed") {
                return Some("failed".to_string());
            } else {
                return Some("error".to_string());
            }
        }
    }

    None
}

/// Extract timestamp from a log line (handles multiple formats)
pub(crate) fn extract_timestamp(line: &str) -> Option<String> {
    use once_cell::sync::Lazy;
    use regex::Regex;

    // Common timestamp patterns
    // Pattern 1: 2026-01-26 10:30:45 or 2026-01-26T10:30:45
    // Pattern 2: [2026-01-26T10:30:45Z] or [2026-01-26 10:30:45]
    // Pattern 3: ISO 8601 with milliseconds

    // Try to match ISO 8601 format
    static ISO_TIMESTAMP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)")
            .unwrap()
    });
    static BRACKETED_TIMESTAMP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"\[(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)\]")
            .unwrap()
    });

    // Try bracketed format first
    if let Some(caps) = BRACKETED_TIMESTAMP.captures(line) {
        return caps.get(1).map(|m| m.as_str().to_string());
    }

    // Try ISO format
    if let Some(caps) = ISO_TIMESTAMP.captures(line) {
        return caps.get(1).map(|m| m.as_str().to_string());
    }

    None
}

/// Parse a timestamp string into a DateTime
pub(crate) fn parse_log_timestamp(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

    // Try RFC3339 first
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        return Some(dt.with_timezone(&Utc));
    }

    // Try common formats
    let formats = [
        "%Y-%m-%dT%H:%M:%S%.fZ",
        "%Y-%m-%dT%H:%M:%SZ",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ];

    for fmt in formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(ts, fmt) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }

    None
}

/// Format log errors into a markdown report for AI consumption
pub(crate) fn format_log_errors_for_ai(errors: &[LogError]) -> String {
    let mut report = String::new();

    report.push_str("## Log Errors Detected\n\n");
    report.push_str(&format!("**Total errors found:** {}\n\n", errors.len()));

    // Group errors by source
    let mut by_source: std::collections::HashMap<String, Vec<&LogError>> =
        std::collections::HashMap::new();
    for error in errors {
        by_source
            .entry(error.source.clone())
            .or_default()
            .push(error);
    }

    for (source, source_errors) in by_source {
        report.push_str(&format!(
            "### {} ({} errors)\n\n",
            source,
            source_errors.len()
        ));

        for error in source_errors {
            report.push_str(&format!(
                "#### Line {} ({})\n",
                error.line_number, error.error_type
            ));

            if let Some(ref ts) = error.timestamp {
                report.push_str(&format!("**Timestamp:** {}\n", ts));
            }

            report.push_str("\n**Context:**\n```\n");

            // Context before
            for line in &error.context_before {
                report.push_str(&format!("  {}\n", line));
            }

            // Error line (highlighted)
            report.push_str(&format!("> {}\n", error.message));

            // Context after
            for line in &error.context_after {
                report.push_str(&format!("  {}\n", line));
            }

            report.push_str("```\n\n");
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_step_creation() {
        let step = ExecutionStepConfig::workflow("TestWorkflow");
        assert_eq!(step.step_type, "workflow");
        assert_eq!(step.name, Some("TestWorkflow".to_string()));
        assert!(!step.take_screenshot);
    }

    #[test]
    fn test_workflow_with_screenshot_creation() {
        let step = ExecutionStepConfig::workflow_with_screenshot("TestWorkflow", 2);
        assert_eq!(step.step_type, "workflow");
        assert_eq!(step.name, Some("TestWorkflow".to_string()));
        assert!(step.take_screenshot);
        assert_eq!(step.screenshot_delay, 2);
    }

    #[test]
    fn test_execution_result_empty_summary() {
        let result = ExecutionResult {
            success: true,
            total_steps: 0,
            successful_steps: 0,
            failed_steps: 0,
            total_duration_ms: 0,
            steps: vec![],
            captured_logs: None,
            captured_runner_logs: None,
            verification_passed: None,
            loop_result: None,
            task_summary: None,
        };
        assert_eq!(result.to_markdown_summary(), "");
    }

    #[test]
    fn test_execution_result_summary() {
        let result = ExecutionResult {
            success: true,
            total_steps: 2,
            successful_steps: 2,
            failed_steps: 0,
            total_duration_ms: 1500,
            steps: vec![
                StepExecutionResult {
                    step_index: 0,
                    step_type: "workflow".to_string(),
                    step_name: "Login".to_string(),
                    success: true,
                    error: None,
                    screenshot_path: Some("screenshot1.png".to_string()),
                    started_at: None,
                    ended_at: None,
                    duration_ms: 1000,
                    config: StepExecutionConfig::default(),
                    verification_details: None,
                },
                StepExecutionResult {
                    step_index: 1,
                    step_type: "screenshot".to_string(),
                    step_name: "Capture".to_string(),
                    success: true,
                    error: None,
                    screenshot_path: Some("screenshot2.png".to_string()),
                    started_at: None,
                    ended_at: None,
                    duration_ms: 500,
                    config: StepExecutionConfig::default(),
                    verification_details: None,
                },
            ],
            captured_logs: None,
            captured_runner_logs: None,
            verification_passed: None,
            loop_result: None,
            task_summary: None,
        };
        let summary = result.to_markdown_summary();
        assert!(summary.contains("Pre-Execution Results"));
        assert!(summary.contains("Login"));
        assert!(summary.contains("2 of 2 steps completed successfully"));
    }

    #[test]
    fn test_parse_output_variable_spec_simple() {
        let (var_name, json_path) = StepExecutor::parse_output_variable_spec("auth_response");
        assert_eq!(var_name, "auth_response");
        assert!(json_path.is_none());
    }

    #[test]
    fn test_parse_output_variable_spec_with_path() {
        let (var_name, json_path) = StepExecutor::parse_output_variable_spec("auth_response.token");
        assert_eq!(var_name, "auth_response");
        assert_eq!(json_path, Some("$.token".to_string()));
    }

    #[test]
    fn test_parse_output_variable_spec_nested_path() {
        let (var_name, json_path) =
            StepExecutor::parse_output_variable_spec("response.data.user.id");
        assert_eq!(var_name, "response");
        assert_eq!(json_path, Some("$.data.user.id".to_string()));
    }

    #[test]
    fn test_extract_json_path_value_string() {
        let json = r#"{"token": "abc123", "expires_in": 3600}"#;
        let result = StepExecutor::extract_json_path_value(json, "$.token");
        assert_eq!(result, Some("abc123".to_string()));
    }

    #[test]
    fn test_extract_json_path_value_number() {
        let json = r#"{"token": "abc123", "expires_in": 3600}"#;
        let result = StepExecutor::extract_json_path_value(json, "$.expires_in");
        assert_eq!(result, Some("3600".to_string()));
    }

    #[test]
    fn test_extract_json_path_value_nested() {
        let json = r#"{"data": {"user": {"id": 42, "name": "John"}}}"#;
        let result = StepExecutor::extract_json_path_value(json, "$.data.user.id");
        assert_eq!(result, Some("42".to_string()));
    }

    #[test]
    fn test_extract_json_path_value_nested_string() {
        let json = r#"{"data": {"user": {"id": 42, "name": "John"}}}"#;
        let result = StepExecutor::extract_json_path_value(json, "$.data.user.name");
        assert_eq!(result, Some("John".to_string()));
    }

    #[test]
    fn test_extract_json_path_value_boolean() {
        let json = r#"{"success": true, "enabled": false}"#;
        let result = StepExecutor::extract_json_path_value(json, "$.success");
        assert_eq!(result, Some("true".to_string()));
    }

    #[test]
    fn test_extract_json_path_value_not_found() {
        let json = r#"{"token": "abc123"}"#;
        let result = StepExecutor::extract_json_path_value(json, "$.missing_field");
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_json_path_value_invalid_json() {
        let json = "not valid json";
        let result = StepExecutor::extract_json_path_value(json, "$.token");
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_json_path_value_array_element() {
        let json = r#"{"items": [{"id": 1}, {"id": 2}, {"id": 3}]}"#;
        let result = StepExecutor::extract_json_path_value(json, "$.items[0].id");
        assert_eq!(result, Some("1".to_string()));
    }

    #[test]
    fn test_extract_json_path_value_explicit_null() {
        // When a field explicitly contains null, we treat it as "not found"
        // since there's no meaningful value to extract
        let json = r#"{"value": null}"#;
        let result = StepExecutor::extract_json_path_value(json, "$.value");
        assert!(result.is_none());
    }
}
