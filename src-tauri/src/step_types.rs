//! Step Types Module
//!
//! Central definition of all step types with associated behavior.
//! This module provides a single source of truth for step type semantics.

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Enumeration of all step types supported by the runner.
///
/// Step types are categorized into groups:
/// - **GUI Automation**: workflow, state, action, screenshot, gui_workflow
/// - **Verification**: playwright, test, check, check_group
/// - **Command**: shell_command, api_request, mcp_call
/// - **AI**: prompt, ai_session
/// - **Web Automation**: awas_discover, awas_execute, awas_check_support, awas_list_actions, awas_extract_elements
/// - **Utility**: macro
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum StepType {
    // ========================================================================
    // GUI Automation Steps
    // ========================================================================
    /// Execute a named workflow (sequence of state transitions)
    #[default]
    Workflow,
    /// Navigate to a specific state
    State,
    /// Execute a single action (click, type, etc.)
    Action,
    /// Capture a screenshot
    Screenshot,
    /// GUI workflow (deprecated, alias for Workflow)
    GuiWorkflow,

    // ========================================================================
    // Verification Steps
    // ========================================================================
    /// Execute a Playwright test script
    Playwright,
    /// Execute a verification test by ID
    Test,
    /// Execute a code quality check
    Check,
    /// Execute a group of checks
    CheckGroup,
    /// Watch logs for errors (runtime error detection)
    LogWatch,

    // ========================================================================
    // Command Steps
    // ========================================================================
    /// Execute a shell command
    ShellCommand,
    /// Execute an HTTP API request
    ApiRequest,
    /// Call an MCP tool
    McpCall,

    // ========================================================================
    // AI Steps
    // ========================================================================
    /// AI prompt step (passed to Claude for execution)
    Prompt,
    /// AI session (agentic execution with Claude)
    AiSession,

    // ========================================================================
    // Web Automation (AWAS) Steps
    // ========================================================================
    /// Discover available actions on a webpage
    AwasDiscover,
    /// Execute an AWAS action
    AwasExecute,
    /// Check if AWAS is supported
    AwasCheckSupport,
    /// List available AWAS actions
    AwasListActions,
    /// Extract elements from HTML
    AwasExtractElements,

    // ========================================================================
    // Utility Steps
    // ========================================================================
    /// Execute a saved macro
    Macro,
}

impl StepType {
    /// Returns true if this step type requires AI execution.
    ///
    /// AI steps are not executed directly by the step executor - instead,
    /// they are passed to Claude for execution.
    pub fn requires_ai(&self) -> bool {
        matches!(self, StepType::Prompt | StepType::AiSession)
    }

    /// Returns true if this step type is a verification step.
    ///
    /// Verification steps are used to check the state of the application
    /// and determine if the workflow should continue or fail.
    pub fn is_verification_type(&self) -> bool {
        matches!(
            self,
            StepType::Playwright
                | StepType::Test
                | StepType::Check
                | StepType::CheckGroup
                | StepType::LogWatch
                | StepType::Screenshot
        )
    }

    /// Returns true if this step type is a GUI automation step.
    pub fn is_gui_type(&self) -> bool {
        matches!(
            self,
            StepType::Workflow
                | StepType::State
                | StepType::Action
                | StepType::Screenshot
                | StepType::GuiWorkflow
        )
    }

    /// Returns true if this step type is a command step (shell, API, MCP).
    pub fn is_command_type(&self) -> bool {
        matches!(
            self,
            StepType::ShellCommand | StepType::ApiRequest | StepType::McpCall
        )
    }

    /// Returns true if this step type is an AWAS (web automation) step.
    pub fn is_awas_type(&self) -> bool {
        matches!(
            self,
            StepType::AwasDiscover
                | StepType::AwasExecute
                | StepType::AwasCheckSupport
                | StepType::AwasListActions
                | StepType::AwasExtractElements
        )
    }

    /// Returns the default timeout in seconds for this step type.
    /// Returns None to indicate no timeout (run until completion).
    /// Users can override this by explicitly setting a timeout.
    pub fn default_timeout_seconds(&self) -> Option<u64> {
        // All steps default to no timeout - they run until completion.
        // Users can explicitly set a timeout if needed.
        None
    }

    /// Returns the default expected duration in milliseconds for this step type.
    ///
    /// This is used for progress estimation in the Timeline widget.
    /// Values are based on typical execution times:
    /// - GUI actions are fast (5 seconds)
    /// - Shell commands are medium (30 seconds)
    /// - Playwright tests are longer (60 seconds)
    /// - AI sessions can be very long (5 minutes)
    pub fn default_expected_duration_ms(&self) -> Option<u64> {
        match self {
            // GUI Automation - generally fast
            StepType::Action => Some(5_000),         // 5 seconds
            StepType::Screenshot => Some(2_000),     // 2 seconds
            StepType::State => Some(10_000),         // 10 seconds
            StepType::Workflow | StepType::GuiWorkflow => Some(120_000), // 2 minutes

            // Verification - varies by test complexity
            StepType::Playwright => Some(60_000),    // 60 seconds
            StepType::Test => Some(30_000),          // 30 seconds
            StepType::Check => Some(10_000),         // 10 seconds
            StepType::CheckGroup => Some(60_000),    // 60 seconds
            StepType::LogWatch => Some(5_000),       // 5 seconds (quick log scan)

            // Command - varies widely, use conservative defaults
            StepType::ShellCommand => Some(30_000),  // 30 seconds
            StepType::ApiRequest => Some(10_000),    // 10 seconds
            StepType::McpCall => Some(30_000),       // 30 seconds

            // AI - typically long-running
            StepType::Prompt | StepType::AiSession => Some(300_000), // 5 minutes

            // AWAS - web automation
            StepType::AwasDiscover => Some(30_000),  // 30 seconds
            StepType::AwasExecute => Some(15_000),   // 15 seconds
            StepType::AwasCheckSupport => Some(5_000), // 5 seconds
            StepType::AwasListActions => Some(10_000), // 10 seconds
            StepType::AwasExtractElements => Some(10_000), // 10 seconds

            // Utility
            StepType::Macro => Some(60_000),         // 60 seconds
        }
    }

    /// Parse a step type from a string, with compatibility for legacy names.
    ///
    /// This function handles various naming conventions used in the codebase:
    /// - snake_case: "shell_command", "api_request"
    /// - camelCase: "shellCommand", "apiRequest"
    /// - kebab-case: "shell-command", "api-request"
    /// - Aliases: "shell" -> ShellCommand, "command" -> ShellCommand
    ///
    /// Returns None for unknown step types with a warning log.
    pub fn from_str_compat(s: &str) -> Option<Self> {
        // Normalize to lowercase and replace hyphens with underscores
        let normalized = s.to_lowercase().replace('-', "_");

        // Handle camelCase by inserting underscores before uppercase letters
        // This is a simple approach that works for most cases
        let snake_case = normalized.chars().fold(String::new(), |mut acc, c| {
            if c.is_uppercase() && !acc.is_empty() {
                acc.push('_');
            }
            acc.push(c.to_ascii_lowercase());
            acc
        });

        match snake_case.as_str() {
            // GUI Automation
            "workflow" => Some(StepType::Workflow),
            "state" => Some(StepType::State),
            "action" => Some(StepType::Action),
            "screenshot" => Some(StepType::Screenshot),
            "gui_workflow" | "guiworkflow" => Some(StepType::GuiWorkflow),

            // Verification
            "playwright" => Some(StepType::Playwright),
            "test" => Some(StepType::Test),
            "check" => Some(StepType::Check),
            "check_group" | "checkgroup" => Some(StepType::CheckGroup),
            "log_watch" | "logwatch" => Some(StepType::LogWatch),

            // Command
            "shell_command" | "shellcommand" | "shell" | "command" => Some(StepType::ShellCommand),
            "api_request" | "apirequest" | "api" | "http" => Some(StepType::ApiRequest),
            "mcp_call" | "mcpcall" | "mcp" => Some(StepType::McpCall),

            // AI
            "prompt" | "ai_prompt" | "aiprompt" => Some(StepType::Prompt),
            "ai_session" | "aisession" | "agentic" => Some(StepType::AiSession),

            // AWAS
            "awas_discover" | "awasdiscover" => Some(StepType::AwasDiscover),
            "awas_execute" | "awasexecute" => Some(StepType::AwasExecute),
            "awas_check_support" | "awaschesupport" => Some(StepType::AwasCheckSupport),
            "awas_list_actions" | "awaslistactions" => Some(StepType::AwasListActions),
            "awas_extract_elements" | "awasextractelements" => Some(StepType::AwasExtractElements),

            // Utility
            "macro" => Some(StepType::Macro),

            _ => {
                warn!(
                    "Unknown step type '{}' (normalized: '{}') - defaulting to automation",
                    s, snake_case
                );
                None
            }
        }
    }

    /// Returns the canonical string representation of this step type.
    pub fn as_str(&self) -> &'static str {
        match self {
            StepType::Workflow => "workflow",
            StepType::State => "state",
            StepType::Action => "action",
            StepType::Screenshot => "screenshot",
            StepType::GuiWorkflow => "gui_workflow",
            StepType::Playwright => "playwright",
            StepType::Test => "test",
            StepType::Check => "check",
            StepType::CheckGroup => "check_group",
            StepType::LogWatch => "log_watch",
            StepType::ShellCommand => "shell_command",
            StepType::ApiRequest => "api_request",
            StepType::McpCall => "mcp_call",
            StepType::Prompt => "prompt",
            StepType::AiSession => "ai_session",
            StepType::AwasDiscover => "awas_discover",
            StepType::AwasExecute => "awas_execute",
            StepType::AwasCheckSupport => "awas_check_support",
            StepType::AwasListActions => "awas_list_actions",
            StepType::AwasExtractElements => "awas_extract_elements",
            StepType::Macro => "macro",
        }
    }
}

impl std::fmt::Display for StepType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_str_compat() {
        // snake_case
        assert_eq!(
            StepType::from_str_compat("shell_command"),
            Some(StepType::ShellCommand)
        );
        assert_eq!(
            StepType::from_str_compat("api_request"),
            Some(StepType::ApiRequest)
        );

        // Aliases
        assert_eq!(
            StepType::from_str_compat("shell"),
            Some(StepType::ShellCommand)
        );
        assert_eq!(
            StepType::from_str_compat("command"),
            Some(StepType::ShellCommand)
        );

        // GUI types
        assert_eq!(
            StepType::from_str_compat("workflow"),
            Some(StepType::Workflow)
        );
        assert_eq!(
            StepType::from_str_compat("playwright"),
            Some(StepType::Playwright)
        );

        // AI types
        assert_eq!(StepType::from_str_compat("prompt"), Some(StepType::Prompt));
        assert_eq!(
            StepType::from_str_compat("ai_session"),
            Some(StepType::AiSession)
        );

        // Unknown
        assert_eq!(StepType::from_str_compat("unknown_type"), None);
    }

    #[test]
    fn test_requires_ai() {
        assert!(StepType::Prompt.requires_ai());
        assert!(StepType::AiSession.requires_ai());
        assert!(!StepType::Workflow.requires_ai());
        assert!(!StepType::ShellCommand.requires_ai());
    }

    #[test]
    fn test_is_verification_type() {
        assert!(StepType::Playwright.is_verification_type());
        assert!(StepType::Test.is_verification_type());
        assert!(StepType::Check.is_verification_type());
        assert!(!StepType::Workflow.is_verification_type());
        assert!(!StepType::Prompt.is_verification_type());
    }

    #[test]
    fn test_as_str_roundtrip() {
        let step_types = [
            StepType::Workflow,
            StepType::ShellCommand,
            StepType::Prompt,
            StepType::Playwright,
        ];

        for step_type in step_types {
            let s = step_type.as_str();
            let parsed = StepType::from_str_compat(s);
            assert_eq!(parsed, Some(step_type));
        }
    }

    #[test]
    fn test_default_expected_duration_ms() {
        // GUI actions should be fast
        assert_eq!(StepType::Action.default_expected_duration_ms(), Some(5_000));
        assert_eq!(StepType::Screenshot.default_expected_duration_ms(), Some(2_000));

        // Shell commands medium
        assert_eq!(StepType::ShellCommand.default_expected_duration_ms(), Some(30_000));

        // Playwright tests longer
        assert_eq!(StepType::Playwright.default_expected_duration_ms(), Some(60_000));

        // AI sessions longest
        assert_eq!(StepType::AiSession.default_expected_duration_ms(), Some(300_000));
        assert_eq!(StepType::Prompt.default_expected_duration_ms(), Some(300_000));

        // Workflows
        assert_eq!(StepType::Workflow.default_expected_duration_ms(), Some(120_000));

        // All step types should return Some value (no None cases currently)
        let all_types = [
            StepType::Workflow,
            StepType::State,
            StepType::Action,
            StepType::Screenshot,
            StepType::GuiWorkflow,
            StepType::Playwright,
            StepType::Test,
            StepType::Check,
            StepType::CheckGroup,
            StepType::ShellCommand,
            StepType::ApiRequest,
            StepType::McpCall,
            StepType::Prompt,
            StepType::AiSession,
            StepType::AwasDiscover,
            StepType::AwasExecute,
            StepType::AwasCheckSupport,
            StepType::AwasListActions,
            StepType::AwasExtractElements,
            StepType::Macro,
        ];

        for step_type in all_types {
            assert!(
                step_type.default_expected_duration_ms().is_some(),
                "{:?} should have a default expected duration",
                step_type
            );
        }
    }
}
