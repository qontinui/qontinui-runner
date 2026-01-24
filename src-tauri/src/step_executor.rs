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
//! ## Step Categories
//!
//! - **GUI Automation**: workflow, state, action
//! - **Web Automation**: playwright
//! - **AWAS Automation**: awas_discover, awas_execute, awas_check_support, awas_list_actions, awas_extract_elements

use crate::action_service::UnifiedActionService;
use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::database::{CreateTaskRunAwasStepInput, CreateTaskRunEventInput};
use crate::display::RawEvent;
use crate::executor::file_logger::FileLogger;
use crate::iteration_bundle::{
    parse_action_events, parse_image_recognition_events, ActionEvent, ImageRecognitionEvent,
    RelevantLogSources,
};
use crate::mcp_client::CreateMcpCallInput;
use crate::orchestrator::context_propagation::{ExpressionEvaluator, RuntimeContext};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

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
    #[serde(rename = "promptContent")]
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
    #[serde(alias = "checkWorkingDirectory", alias = "working_directory")]
    pub check_working_directory: Option<String>,

    /// Check: Whether to run auto-fix
    #[serde(alias = "checkAutoFix", alias = "auto_fix", default)]
    pub check_auto_fix: Option<bool>,

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
}

impl ExecutionStepConfig {
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
            timeout_seconds: Some(300),
            initial_state_ids: None,
            is_setup: Some(true), // Workflow is setup by default
            phase: None,
            run_on_subsequent_iterations: Some(true), // Default: run on all iterations for fresh data
            test_id: None,
            test_type: None,
            test_is_critical: None,
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
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
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
            timeout_seconds: Some(300),
            initial_state_ids: None,
            is_setup: Some(true), // Workflow is setup by default
            phase: None,
            run_on_subsequent_iterations: Some(true), // Default: run on all iterations for fresh data
            test_id: None,
            test_type: None,
            test_is_critical: None,
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
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
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
            timeout_seconds: Some(30),
            initial_state_ids: None,
            is_setup: Some(false), // Screenshot is verification, not setup
            phase: None,
            run_on_subsequent_iterations: Some(true), // Verification runs on all iterations
            test_id: None,
            test_type: None,
            test_is_critical: None,
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
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
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
            timeout_seconds: Some(30),
            initial_state_ids: None,
            is_setup: Some(true), // AWAS discover is typically setup
            phase: None,
            run_on_subsequent_iterations: Some(false), // Usually only discover once
            test_id: None,
            test_type: None,
            test_is_critical: None,
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
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
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
            timeout_seconds: Some(30),
            initial_state_ids: None,
            is_setup: Some(false), // AWAS execute is typically an action step
            phase: None,
            run_on_subsequent_iterations: Some(true),
            test_id: None,
            test_type: None,
            test_is_critical: None,
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
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
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
            timeout_seconds: Some(30),
            initial_state_ids: None,
            is_setup: Some(true), // Check support is typically setup
            phase: None,
            run_on_subsequent_iterations: Some(false),
            test_id: None,
            test_type: None,
            test_is_critical: None,
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
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
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
            timeout_seconds: Some(30),
            initial_state_ids: None,
            is_setup: Some(false),
            phase: None,
            run_on_subsequent_iterations: Some(true),
            test_id: None,
            test_type: None,
            test_is_critical: None,
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
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
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
            timeout_seconds: Some(30),
            initial_state_ids: None,
            is_setup: Some(false),
            phase: None,
            run_on_subsequent_iterations: Some(true),
            test_id: None,
            test_type: None,
            test_is_critical: None,
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
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
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
            timeout_seconds: Some(30),
            initial_state_ids: None,
            is_setup: Some(false),
            phase: None,
            run_on_subsequent_iterations: Some(true),
            test_id: None,
            test_type: None,
            test_is_critical: None,
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
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
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
            timeout_seconds: Some(300),
            initial_state_ids: None,
            is_setup: Some(true), // Macro is setup by default
            phase: None,
            run_on_subsequent_iterations: Some(true),
            test_id: None,
            test_type: None,
            test_is_critical: None,
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
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            // Macro fields
            macro_id: Some(macro_id.to_string()),
            // Check group fields
            check_group_id: None,
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
    /// Optional app handle for emitting events to the Tauri frontend
    app_handle: Option<tauri::AppHandle>,
    /// Optional task run ID for database logging (AWAS steps, etc.)
    task_run_id: Option<String>,
    /// Runtime context for variable expansion in commands
    runtime_context: RuntimeContext,
}

impl StepExecutor {
    /// Create a new StepExecutor
    pub fn new(app_state: Arc<AppState>, config_storage: Arc<TokioMutex<ConfigStorage>>) -> Self {
        Self {
            action_service: UnifiedActionService::new(app_state.clone(), config_storage),
            app_state,
            app_handle: None,
            task_run_id: None,
            runtime_context: RuntimeContext::new(),
        }
    }

    /// Create a new StepExecutor with an app handle for frontend event emission
    pub fn with_app_handle(
        app_state: Arc<AppState>,
        config_storage: Arc<TokioMutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            action_service: UnifiedActionService::new(app_state.clone(), config_storage),
            app_state,
            app_handle: Some(app_handle),
            task_run_id: None,
            runtime_context: RuntimeContext::new(),
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

    /// Log a step execution event to the database
    ///
    /// This logs step start, complete, and error events to the task_run_events table.
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
        let step_name = step.name.clone().unwrap_or_else(|| step.step_type.clone());

        // Build data JSON with step details
        let data = json!({
            "step_index": step_index,
            "step_type": step.step_type,
            "step_name": step_name,
            "phase": step.phase,
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
            task_run_id: task_run_id.to_string(),
            event_type: "step_execution".to_string(),
            event_subtype: Some(event_subtype.to_string()),
            message: message.to_string(),
            data: Some(serde_json::to_string(&data).unwrap_or_default()),
            workflow_name: None,
            state_name: None,
            action_id: None,
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

    /// Save an AWAS step result to the database (if task_run_id is set)
    ///
    /// This method is called after each AWAS step execution to persist
    /// the results for later analysis and debugging.
    fn save_awas_step_result(
        &self,
        step_type: &str,
        url: Option<&str>,
        action_id: Option<&str>,
        parameters: Option<&serde_json::Value>,
        response: &AwasCommandResponse,
        duration_ms: i64,
        step_name: Option<&str>,
    ) {
        // Only save if we have a task_run_id
        let Some(ref task_run_id) = self.task_run_id else {
            return;
        };

        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let input = CreateTaskRunAwasStepInput {
            task_run_id: task_run_id.clone(),
            step_id: None, // Step ID from workflow if available
            step_name: step_name.map(|s| s.to_string()),
            step_type: step_type.to_string(),
            url: url.map(|s| s.to_string()),
            action_id: action_id.map(|s| s.to_string()),
            parameters: parameters.map(|p| serde_json::to_string(p).unwrap_or_default()),
            response_data: response
                .data
                .as_ref()
                .map(|d| serde_json::to_string(d).unwrap_or_default()),
            success: response.success,
            error_message: response.error.clone(),
            duration_ms: Some(duration_ms),
            timestamp,
        };

        match self
            .app_state
            .checkpoint_db
            .create_task_run_awas_step(&input)
        {
            Ok(id) => {
                info!(
                    "Saved AWAS step result to database: {} (type: {})",
                    id, step_type
                );
            }
            Err(e) => {
                warn!("Failed to save AWAS step result to database: {}", e);
            }
        }
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
            // Check fields
            check_type: None,
            check_command: None,
            check_working_directory: None,
            check_auto_fix: None,
            // Macro fields
            macro_id: None,
            // Check group fields
            check_group_id: None,
        })
    }

    /// Execute steps with log source configuration for log capture
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

            results.push(StepExecutionResult {
                step_index: index,
                step_type: step.step_type.clone(),
                step_name,
                success,
                error,
                screenshot_path: final_screenshot,
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
        let timeout = step
            .timeout_seconds
            .unwrap_or(match step.step_type.as_str() {
                "workflow" => 300,
                "state" => 300,
                "check" => 120, // 2 minutes for code checks (Rust/TypeScript can be slow)
                "test" => 300,  // 5 minutes for tests
                _ => 60,        // 1 minute default for other steps
            });

        match step.step_type.as_str() {
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
                    let timeout = step.timeout_seconds.unwrap_or(300);
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
                let prompt_text = step.prompt_content.clone().unwrap_or_else(|| String::new());
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
            // AWAS Step Types
            // ================================================================
            "awas_discover" => {
                if let Some(ref url) = step.awas_url {
                    self.execute_awas_discover(url, timeout).await
                } else {
                    (
                        false,
                        Some("No URL specified for AWAS discover".to_string()),
                        None,
                    )
                }
            }
            "awas_execute" => {
                if let (Some(ref url), Some(ref action_id)) = (&step.awas_url, &step.awas_action_id)
                {
                    self.execute_awas_action(url, action_id, step.awas_params.clone(), timeout)
                        .await
                } else {
                    (
                        false,
                        Some("URL and action_id required for AWAS execute".to_string()),
                        None,
                    )
                }
            }
            "awas_check_support" => {
                if let Some(ref url) = step.awas_url {
                    self.execute_awas_check_support(url, timeout).await
                } else {
                    (
                        false,
                        Some("No URL specified for AWAS check support".to_string()),
                        None,
                    )
                }
            }
            "awas_list_actions" => self.execute_awas_list_actions(timeout).await,
            "awas_extract_elements" => {
                if let Some(ref html) = step.awas_html {
                    self.execute_awas_extract_elements(html, step.awas_base_url.as_deref(), timeout)
                        .await
                } else {
                    (
                        false,
                        Some("No HTML specified for AWAS extract elements".to_string()),
                        None,
                    )
                }
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
            "check_group" => self.execute_check_group_step(step, timeout).await,
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
            timeout_seconds: verification_test.timeout_seconds,
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
    pub async fn execute_verification_steps(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
    ) -> VerificationPhaseResult {
        use crate::test_executor::TestStatus;
        use std::time::Instant;

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
                let result = StepExecutionResult {
                    step_index: index,
                    step_name: step
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("Step {}", index + 1)),
                    step_type: step.step_type.clone(),
                    success: false,
                    error: Some("Skipped due to critical failure".to_string()),
                    duration_ms: 0,
                    screenshot_path: None,
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
                    let timeout = step.timeout_seconds.unwrap_or(120);
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
                    let timeout = step.timeout_seconds.unwrap_or(300);
                    let (success, error, summary) =
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

            let result = StepExecutionResult {
                step_index: index,
                step_name,
                step_type: step.step_type.clone(),
                success,
                error,
                duration_ms,
                screenshot_path: None,
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

    // ========================================================================
    // AWAS Step Execution Methods
    // ========================================================================

    /// Execute AWAS discover step - discovers AWAS manifest from a URL
    async fn execute_awas_discover(
        &self,
        url: &str,
        timeout_secs: u64,
    ) -> (bool, Option<String>, Option<String>) {
        info!("AWAS Discover: {}", url);

        let start_time = std::time::Instant::now();
        let params = json!({
            "url": url,
        });

        match self
            .execute_awas_command("awas_discover", Some(params.clone()), timeout_secs)
            .await
        {
            Ok(response) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;

                // Save result to database
                self.save_awas_step_result(
                    "awas_discover",
                    Some(url),
                    None,
                    Some(&params),
                    &response,
                    duration_ms,
                    Some(&format!("AWAS Discover: {}", url)),
                );

                if response.success {
                    info!(
                        "AWAS Discover completed successfully for {} in {}ms",
                        url, duration_ms
                    );
                    (true, None, None)
                } else {
                    let error = response
                        .error
                        .unwrap_or_else(|| "AWAS discover failed".to_string());
                    (false, Some(error), None)
                }
            }
            Err(e) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;
                let error_msg = format!("AWAS discover error: {}", e);

                // Save error result to database
                let error_response = AwasCommandResponse {
                    success: false,
                    data: None,
                    error: Some(error_msg.clone()),
                };
                self.save_awas_step_result(
                    "awas_discover",
                    Some(url),
                    None,
                    Some(&params),
                    &error_response,
                    duration_ms,
                    Some(&format!("AWAS Discover: {}", url)),
                );

                (false, Some(error_msg), None)
            }
        }
    }

    /// Execute AWAS action step - executes an AWAS action on the target application
    async fn execute_awas_action(
        &self,
        url: &str,
        action_id: &str,
        params: Option<serde_json::Value>,
        timeout_secs: u64,
    ) -> (bool, Option<String>, Option<String>) {
        info!("AWAS Execute: {} on {}", action_id, url);

        let start_time = std::time::Instant::now();
        let command_params = json!({
            "url": url,
            "action_id": action_id,
            "params": params,
        });

        match self
            .execute_awas_command("awas_execute", Some(command_params.clone()), timeout_secs)
            .await
        {
            Ok(response) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;

                // Save result to database
                self.save_awas_step_result(
                    "awas_execute",
                    Some(url),
                    Some(action_id),
                    Some(&command_params),
                    &response,
                    duration_ms,
                    Some(&format!("AWAS Execute: {}", action_id)),
                );

                if response.success {
                    info!(
                        "AWAS Execute '{}' completed successfully in {}ms",
                        action_id, duration_ms
                    );
                    (true, None, None)
                } else {
                    let error = response
                        .error
                        .unwrap_or_else(|| "AWAS execute failed".to_string());
                    (false, Some(error), None)
                }
            }
            Err(e) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;
                let error_msg = format!("AWAS execute error: {}", e);

                // Save error result to database
                let error_response = AwasCommandResponse {
                    success: false,
                    data: None,
                    error: Some(error_msg.clone()),
                };
                self.save_awas_step_result(
                    "awas_execute",
                    Some(url),
                    Some(action_id),
                    Some(&command_params),
                    &error_response,
                    duration_ms,
                    Some(&format!("AWAS Execute: {}", action_id)),
                );

                (false, Some(error_msg), None)
            }
        }
    }

    /// Execute AWAS check support step - checks if a URL supports AWAS
    async fn execute_awas_check_support(
        &self,
        url: &str,
        timeout_secs: u64,
    ) -> (bool, Option<String>, Option<String>) {
        info!("AWAS Check Support: {}", url);

        let start_time = std::time::Instant::now();
        let params = json!({
            "url": url,
        });

        match self
            .execute_awas_command("awas_check_support", Some(params.clone()), timeout_secs)
            .await
        {
            Ok(response) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;

                // Save result to database
                self.save_awas_step_result(
                    "awas_check_support",
                    Some(url),
                    None,
                    Some(&params),
                    &response,
                    duration_ms,
                    Some(&format!("AWAS Check Support: {}", url)),
                );

                if response.success {
                    // Extract support status from response data
                    let supported = response
                        .data
                        .as_ref()
                        .and_then(|d| d.get("supported"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    if supported {
                        info!(
                            "AWAS is supported at {} (checked in {}ms)",
                            url, duration_ms
                        );
                    } else {
                        info!(
                            "AWAS is not supported at {} (checked in {}ms)",
                            url, duration_ms
                        );
                    }
                    (true, None, None)
                } else {
                    let error = response
                        .error
                        .unwrap_or_else(|| "AWAS check support failed".to_string());
                    (false, Some(error), None)
                }
            }
            Err(e) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;
                let error_msg = format!("AWAS check support error: {}", e);

                // Save error result to database
                let error_response = AwasCommandResponse {
                    success: false,
                    data: None,
                    error: Some(error_msg.clone()),
                };
                self.save_awas_step_result(
                    "awas_check_support",
                    Some(url),
                    None,
                    Some(&params),
                    &error_response,
                    duration_ms,
                    Some(&format!("AWAS Check Support: {}", url)),
                );

                (false, Some(error_msg), None)
            }
        }
    }

    /// Execute AWAS list actions step - lists available AWAS actions
    async fn execute_awas_list_actions(
        &self,
        timeout_secs: u64,
    ) -> (bool, Option<String>, Option<String>) {
        info!("AWAS List Actions");

        let start_time = std::time::Instant::now();

        match self
            .execute_awas_command("awas_list_actions", None, timeout_secs)
            .await
        {
            Ok(response) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;

                // Save result to database
                self.save_awas_step_result(
                    "awas_list_actions",
                    None,
                    None,
                    None,
                    &response,
                    duration_ms,
                    Some("AWAS List Actions"),
                );

                if response.success {
                    let action_count = response
                        .data
                        .as_ref()
                        .and_then(|d| d.get("actions"))
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    info!(
                        "AWAS List Actions: found {} actions in {}ms",
                        action_count, duration_ms
                    );
                    (true, None, None)
                } else {
                    let error = response
                        .error
                        .unwrap_or_else(|| "AWAS list actions failed".to_string());
                    (false, Some(error), None)
                }
            }
            Err(e) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;
                let error_msg = format!("AWAS list actions error: {}", e);

                // Save error result to database
                let error_response = AwasCommandResponse {
                    success: false,
                    data: None,
                    error: Some(error_msg.clone()),
                };
                self.save_awas_step_result(
                    "awas_list_actions",
                    None,
                    None,
                    None,
                    &error_response,
                    duration_ms,
                    Some("AWAS List Actions"),
                );

                (false, Some(error_msg), None)
            }
        }
    }

    /// Execute AWAS extract elements step - extracts AWAS elements from HTML
    async fn execute_awas_extract_elements(
        &self,
        html: &str,
        base_url: Option<&str>,
        timeout_secs: u64,
    ) -> (bool, Option<String>, Option<String>) {
        info!("AWAS Extract Elements (HTML length: {} bytes)", html.len());

        let start_time = std::time::Instant::now();
        let params = json!({
            "html": html,
            "base_url": base_url,
        });

        // For the database, we don't want to store the full HTML in parameters
        // (could be very large), so we just store metadata
        let params_for_db = json!({
            "html_length": html.len(),
            "base_url": base_url,
        });

        match self
            .execute_awas_command("awas_extract_elements", Some(params), timeout_secs)
            .await
        {
            Ok(response) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;

                // Save result to database
                self.save_awas_step_result(
                    "awas_extract_elements",
                    base_url,
                    None,
                    Some(&params_for_db),
                    &response,
                    duration_ms,
                    Some("AWAS Extract Elements"),
                );

                if response.success {
                    info!(
                        "AWAS Extract Elements completed successfully in {}ms",
                        duration_ms
                    );
                    (true, None, None)
                } else {
                    let error = response
                        .error
                        .unwrap_or_else(|| "AWAS extract elements failed".to_string());
                    (false, Some(error), None)
                }
            }
            Err(e) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;
                let error_msg = format!("AWAS extract elements error: {}", e);

                // Save error result to database
                let error_response = AwasCommandResponse {
                    success: false,
                    data: None,
                    error: Some(error_msg.clone()),
                };
                self.save_awas_step_result(
                    "awas_extract_elements",
                    base_url,
                    None,
                    Some(&params_for_db),
                    &error_response,
                    duration_ms,
                    Some("AWAS Extract Elements"),
                );

                (false, Some(error_msg), None)
            }
        }
    }

    /// Execute an AWAS command via the Python bridge
    async fn execute_awas_command(
        &self,
        command: &str,
        params: Option<serde_json::Value>,
        timeout_secs: u64,
    ) -> Result<AwasCommandResponse, String> {
        let app_state = self.app_state.clone();
        let command = command.to_string();
        let timeout_duration = std::time::Duration::from_secs(timeout_secs);

        tokio::task::spawn_blocking(move || {
            let mut bridge_lock = app_state
                .python_bridge
                .lock()
                .map_err(|e| format!("Failed to acquire python_bridge lock: {}", e))?;

            if let Some(ref mut bridge) = *bridge_lock {
                if !bridge.is_running() {
                    return Err("Python executor not running".to_string());
                }

                let result = bridge.send_command_and_wait(&command, params, timeout_duration)?;

                Ok(AwasCommandResponse {
                    success: result.success,
                    data: result.data,
                    error: result.error,
                })
            } else {
                Err("Python executor not initialized".to_string())
            }
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
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
        _timeout_secs: u64,
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
    async fn execute_shell_command_step(
        &self,
        step: &ExecutionStepConfig,
        timeout_secs: u64,
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

        // Store template info for later use in event logging
        let template_info = if has_variables {
            Some((template_command.clone(), resolved_variables.clone()))
        } else {
            None
        };

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

        info!(
            "Executing shell command '{}': {} (shell: {}, timeout: {}s, working_dir: {:?})",
            step_name, command, shell_type, timeout_secs, working_directory
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

        // Log start event to database (so shell commands show as "running" in the Active page)
        if let Some(ref task_run_id) = self.task_run_id {
            let start_data = json!({
                "step_type": "shell_command",
                "step_name": step_name,
                "phase": step.phase,
                "command": &command_display,
                "working_directory": working_directory.as_deref(),
                "shell_type": shell_type,
                "start_time": (timestamp * 1000.0) as i64,
            });

            let start_event_input = CreateTaskRunEventInput {
                task_run_id: task_run_id.clone(),
                event_type: "shell_command".to_string(),
                event_subtype: Some("start".to_string()),
                message: format!("Shell command '{}' started", step_name),
                data: Some(serde_json::to_string(&start_data).unwrap_or_default()),
                workflow_name: None,
                state_name: None,
                action_id: Some(action_id.clone()),
                timestamp: chrono::Utc::now().to_rfc3339(),
                duration_ms: None,
            };

            if let Err(e) = self
                .app_state
                .checkpoint_db
                .create_task_run_event(&start_event_input)
            {
                warn!("Failed to log shell command start event: {}", e);
            }
        }

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

        // Execute with timeout
        let start = std::time::Instant::now();
        let timeout_duration = Duration::from_secs(timeout_secs);

        let output_result = timeout(timeout_duration, cmd.output()).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        // Process the result
        let (success, exit_code, stdout, stderr) = match output_result {
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
                    step_name, timeout_secs
                );
                (
                    false,
                    None,
                    String::new(),
                    format!("Command timed out after {} seconds", timeout_secs),
                )
            }
        };

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

        // Log detailed shell command event to database (if task_run_id is set)
        if let Some(ref task_run_id) = self.task_run_id {
            // Truncate stdout/stderr for database storage (limit to 10KB each)
            let stdout_db = if stdout.len() > 10240 {
                format!("{}... (truncated)", &stdout[..10240])
            } else {
                stdout.clone()
            };
            let stderr_db = if stderr.len() > 10240 {
                format!("{}... (truncated)", &stderr[..10240])
            } else {
                stderr.clone()
            };

            // Build shell_data with optional template info
            let mut shell_data = json!({
                "step_type": "shell_command",
                "step_name": step_name,
                "phase": step.phase,
                "command": &command,
                "working_directory": working_directory.as_deref(),
                "shell_type": shell_type,
                "exit_code": exit_code,
                "stdout": stdout_db,
                "stderr": stderr_db,
                "duration_ms": duration_ms,
                "end_time": (end_timestamp * 1000.0) as i64,
                "success": final_success,
            });

            // Add template info if variables were used
            if let Some((ref template_cmd, ref vars)) = template_info {
                if let Some(obj) = shell_data.as_object_mut() {
                    obj.insert("template_command".to_string(), json!(template_cmd));
                    if let Some(resolved_vars) = vars {
                        obj.insert("resolved_variables".to_string(), json!(resolved_vars));
                    }
                }
            }

            let event_input = CreateTaskRunEventInput {
                task_run_id: task_run_id.clone(),
                event_type: "shell_command".to_string(),
                event_subtype: Some(if final_success { "complete" } else { "error" }.to_string()),
                message: format!(
                    "Shell command '{}' {} (exit code: {:?}, {}ms)",
                    step_name,
                    if final_success { "completed" } else { "failed" },
                    exit_code,
                    duration_ms
                ),
                data: Some(serde_json::to_string(&shell_data).unwrap_or_default()),
                workflow_name: None,
                state_name: None,
                action_id: Some(action_id.clone()),
                timestamp: chrono::Utc::now().to_rfc3339(),
                duration_ms: Some(duration_ms as i64),
            };

            if let Err(e) = self
                .app_state
                .checkpoint_db
                .create_task_run_event(&event_input)
            {
                warn!("Failed to log shell command event: {}", e);
            }
        }

        (final_success, error_msg, output_data)
    }

    // =========================================================================
    // API Request Step Execution
    // =========================================================================

    /// Execute an API request step
    async fn execute_api_request_step(
        &self,
        step: &ExecutionStepConfig,
        timeout_secs: u64,
    ) -> (bool, Option<String>, Option<String>) {
        let method = match &step.api_method {
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

        let step_name = step.name.as_deref().unwrap_or("API Request");
        info!("Executing API request '{}': {} {}", step_name, method, url);

        // Build the HTTP client with timeout
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return (
                    false,
                    Some(format!("Failed to create HTTP client: {}", e)),
                    None,
                );
            }
        };

        // Build the request
        let mut request = match method.as_str() {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "PATCH" => client.patch(&url),
            "DELETE" => client.delete(&url),
            _ => {
                return (
                    false,
                    Some(format!("Unsupported HTTP method: {}", method)),
                    None,
                );
            }
        };

        // Add headers
        if let Some(headers) = &step.api_headers {
            if let Some(obj) = headers.as_object() {
                for (key, value) in obj {
                    if let Some(v) = value.as_str() {
                        request = request.header(key.as_str(), v);
                    }
                }
            }
        }

        // Add content type
        if let Some(content_type) = &step.api_content_type {
            request = request.header("Content-Type", content_type.as_str());
        }

        // Add body
        if let Some(body) = &step.api_body {
            request = request.body(body.clone());
        }

        // Execute the request
        let start = std::time::Instant::now();
        match request.send().await {
            Ok(response) => {
                let duration_ms = start.elapsed().as_millis();
                let status = response.status();
                let status_code = status.as_u16();

                match response.text().await {
                    Ok(body) => {
                        info!(
                            "API request '{}' completed: status={}, duration={}ms",
                            step_name, status_code, duration_ms
                        );

                        if status.is_success() {
                            // Return response body in the output field
                            (true, None, Some(body))
                        } else {
                            (
                                false,
                                Some(format!(
                                    "HTTP {}: {}",
                                    status_code,
                                    body.chars().take(500).collect::<String>()
                                )),
                                Some(body),
                            )
                        }
                    }
                    Err(e) => (
                        false,
                        Some(format!("Failed to read response body: {}", e)),
                        None,
                    ),
                }
            }
            Err(e) => (false, Some(format!("API request failed: {}", e)), None),
        }
    }

    // =========================================================================
    // Check Step Execution
    // =========================================================================

    /// Execute a code quality check step
    async fn execute_check_step(
        &self,
        step: &ExecutionStepConfig,
        timeout_secs: u64,
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

        info!(
            "Executing check '{}' ({}): {} (timeout: {}s, working_dir: {:?})",
            step_name, check_type, final_command, timeout_secs, working_directory
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

        // Execute with timeout
        let start = std::time::Instant::now();
        let timeout_duration = Duration::from_secs(timeout_secs);

        let output_result = timeout(timeout_duration, cmd.output()).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        // Process the result
        let (final_success, error_msg, output_data) = match output_result {
            Ok(Ok(output)) => {
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
                        Some(format!("Check failed (exit code {:?}): {}", exit_code, error_summary.trim())),
                        Some(combined_output), // Return full output for AI context
                    )
                }
            }
            Ok(Err(e)) => {
                warn!("Failed to execute check '{}': {}", step_name, e);
                (false, Some(format!("Failed to execute check: {}", e)), None)
            }
            Err(_) => {
                warn!("Check '{}' timed out after {}s", step_name, timeout_secs);
                (
                    false,
                    Some(format!("Check timed out after {} seconds", timeout_secs)),
                    None,
                )
            }
        };

        // Emit completion event
        let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let duration = end_timestamp - timestamp;

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
    async fn execute_check_group_step(
        &self,
        step: &ExecutionStepConfig,
        timeout_secs: u64,
    ) -> (bool, Option<String>, Option<String>) {
        let step_name = step.name.as_deref().unwrap_or("Check Group");
        let group_id = match &step.check_group_id {
            Some(id) => id.clone(),
            None => {
                return (
                    false,
                    Some("No check group ID specified for check_group step".to_string()),
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
                );
            }
            Err(e) => {
                return (
                    false,
                    Some(format!("Failed to get check group: {}", e)),
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
                );
            }
        };

        if checks.is_empty() {
            return (
                true,
                None,
                Some(format!("No checks in group '{}'", group.name)),
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
        let mut results_output = Vec::new();

        for check in &checks {
            if !check.enabled {
                results_output.push(format!("  [SKIPPED] {} (disabled)", check.name));
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
            "Check group '{}' completed: {}/{} passed ({}ms)",
            group.name, passed, total, duration_ms
        );

        if success {
            (true, None, Some(summary))
        } else {
            (
                false,
                Some(format!(
                    "Check group '{}' failed: {}/{} passed",
                    group.name, passed, total
                )),
                Some(summary),
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
                            let timeout = step.timeout_seconds.unwrap_or(60);
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
}

/// Response from AWAS command execution
#[derive(Debug)]
struct AwasCommandResponse {
    success: bool,
    data: Option<serde_json::Value>,
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_step_creation() {
        let step = ExecutionStepConfig::workflow("TestWorkflow");
        assert_eq!(step.step_type, "workflow");
        assert_eq!(step.name, Some("TestWorkflow".to_string()));
        assert_eq!(step.take_screenshot, false);
    }

    #[test]
    fn test_workflow_with_screenshot_creation() {
        let step = ExecutionStepConfig::workflow_with_screenshot("TestWorkflow", 2);
        assert_eq!(step.step_type, "workflow");
        assert_eq!(step.name, Some("TestWorkflow".to_string()));
        assert_eq!(step.take_screenshot, true);
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
                    duration_ms: 500,
                    config: StepExecutionConfig::default(),
                    verification_details: None,
                },
            ],
            captured_logs: None,
            captured_runner_logs: None,
        };
        let summary = result.to_markdown_summary();
        assert!(summary.contains("Pre-Execution Results"));
        assert!(summary.contains("Login"));
        assert!(summary.contains("2 of 2 steps completed successfully"));
    }
}
