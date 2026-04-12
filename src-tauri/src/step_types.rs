//! Step Types Module
//!
//! Central definition of all step types with associated behavior.
//! This module provides a single source of truth for step type semantics.

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Explicit mode for command steps, indicating which sub-handler to use.
///
/// When set, the `CommandHandler` uses this field to dispatch directly instead
/// of inferring the mode from which optional fields are populated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandMode {
    /// Plain shell command execution
    Shell,
    /// Code quality check (check_type determines the checker)
    Check,
    /// Execute all checks in a saved check group
    CheckGroup,
    /// Run a test (test_type/test_id determines the runner)
    Test,
}

impl CommandMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Check => "check",
            Self::CheckGroup => "check_group",
            Self::Test => "test",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "shell" => Some(Self::Shell),
            "check" => Some(Self::Check),
            "check_group" => Some(Self::CheckGroup),
            "test" => Some(Self::Test),
            _ => None,
        }
    }
}

impl std::fmt::Display for CommandMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Enumeration of all step types supported by the runner.
///
/// Step types are categorized into groups:
/// - **GUI Automation**: workflow, state, action, screenshot, gui_workflow
/// - **Verification**: playwright, ui_bridge
/// - **Command**: command (unified: shell command, check, check group, or test)
/// - **AI**: prompt, ai_session
/// - **Web Automation**: awas_discover, awas_execute, awas_check_support, awas_list_actions, awas_extract_elements
/// - **Accessibility**: native_accessibility
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
    /// Watch logs for errors (runtime error detection)
    LogWatch,
    /// Gate step: aggregates required step results (pass/fail)
    Gate,

    // ========================================================================
    // Command Steps
    // ========================================================================
    /// Unified command step (shell command, check, or check group based on config)
    Command,
    /// Execute a UI Bridge action (navigate, execute, assert, snapshot)
    UiBridge,
    /// Run a UI Bridge design audit (contrast, accessibility, visibility checks)
    UiBridgeDesignAudit,

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
    // Artifact Steps
    // ========================================================================
    /// Save a generated workflow JSON to the library
    SaveWorkflowArtifact,

    // ========================================================================
    // Utility Steps
    // ========================================================================
    /// Execute a saved macro
    Macro,

    // ========================================================================
    // Accessibility Steps
    // ========================================================================
    /// Run a native accessibility audit using OS-level accessibility APIs.
    NativeAccessibility,

    // ========================================================================
    // Watcher Steps (screenpipe-inspired reactive agents)
    // ========================================================================
    /// Scheduled watcher: queries the activity timeline, reasons with AI,
    /// and triggers a conditional action.
    Watcher,

    // ========================================================================
    // Code Execution Steps
    // ========================================================================
    /// Execute inline Python code or a Python file in an optional sandbox.
    CodeExecution,

    // ========================================================================
    // Visual Assertion Steps
    // ========================================================================
    /// Visual assertion via UI Bridge auto module: text assertions (DOM/OCR),
    /// screenshot comparison, and element highlighting.
    UiBridgeVisualAssertion,
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
            StepType::Command
                | StepType::Playwright
                | StepType::LogWatch
                | StepType::Gate
                | StepType::Screenshot
                | StepType::UiBridge
                | StepType::UiBridgeDesignAudit
                | StepType::UiBridgeVisualAssertion
                | StepType::NativeAccessibility
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

    /// Returns true if this step type is a command step.
    pub fn is_command_type(&self) -> bool {
        matches!(self, StepType::Command)
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
            StepType::Action => Some(5_000),     // 5 seconds
            StepType::Screenshot => Some(2_000), // 2 seconds
            StepType::State => Some(10_000),     // 10 seconds
            StepType::Workflow | StepType::GuiWorkflow => Some(120_000), // 2 minutes

            // Verification - varies by test complexity
            StepType::Playwright => Some(60_000), // 60 seconds
            StepType::LogWatch => Some(5_000),    // 5 seconds (quick log scan)
            StepType::Gate => Some(100),          // Near-instant (aggregation only)

            // Command - varies widely, use conservative defaults
            StepType::Command => Some(30_000),  // 30 seconds
            StepType::UiBridge => Some(15_000), // 15 seconds
            StepType::UiBridgeDesignAudit => Some(10_000), // 10 seconds

            // AI - typically long-running
            StepType::Prompt | StepType::AiSession => Some(300_000), // 5 minutes

            // AWAS - web automation
            StepType::AwasDiscover => Some(30_000), // 30 seconds
            StepType::AwasExecute => Some(15_000),  // 15 seconds
            StepType::AwasCheckSupport => Some(5_000), // 5 seconds
            StepType::AwasListActions => Some(10_000), // 10 seconds
            StepType::AwasExtractElements => Some(10_000), // 10 seconds

            // Accessibility
            StepType::NativeAccessibility => Some(15_000), // 15 seconds

            // Artifact
            StepType::SaveWorkflowArtifact => Some(5_000), // 5 seconds (DB write)

            // Utility
            StepType::Macro => Some(60_000), // 60 seconds

            // Watcher
            StepType::Watcher => Some(120_000), // 120 seconds (includes AI reasoning)

            // Code Execution
            StepType::CodeExecution => Some(10_000), // 10 seconds

            // Visual Assertion
            StepType::UiBridgeVisualAssertion => Some(5_000), // 5 seconds
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
            "log_watch" | "logwatch" => Some(StepType::LogWatch),
            "gate" => Some(StepType::Gate),

            // Command (includes legacy: shell_command, check, check_group, api_request, mcp_call, test)
            "command" | "test" | "shell_command" | "shellcommand" | "shell" | "check"
            | "check_group" | "checkgroup" | "api_request" | "apirequest" | "api" | "http"
            | "mcp_call" | "mcpcall" | "mcp" => Some(StepType::Command),
            "ui_bridge" | "uibridge" => Some(StepType::UiBridge),
            "ui_bridge_design_audit" | "uibridgedesignaudit" => Some(StepType::UiBridgeDesignAudit),

            // AI
            "prompt" | "ai_prompt" | "aiprompt" => Some(StepType::Prompt),
            "ai_session" | "aisession" | "agentic" => Some(StepType::AiSession),

            // AWAS
            "awas_discover" | "awasdiscover" => Some(StepType::AwasDiscover),
            "awas_execute" | "awasexecute" => Some(StepType::AwasExecute),
            "awas_check_support" | "awaschesupport" => Some(StepType::AwasCheckSupport),
            "awas_list_actions" | "awaslistactions" => Some(StepType::AwasListActions),
            "awas_extract_elements" | "awasextractelements" => Some(StepType::AwasExtractElements),

            // Accessibility
            "native_accessibility" | "nativeaccessibility" => Some(StepType::NativeAccessibility),

            // Artifact
            "save_workflow_artifact" | "saveworkflowartifact" => {
                Some(StepType::SaveWorkflowArtifact)
            }

            // Utility
            "macro" => Some(StepType::Macro),

            // Watcher
            "watcher" => Some(StepType::Watcher),

            // Code Execution
            "code_execution" | "codeexecution" | "code" => Some(StepType::CodeExecution),

            // Visual Assertion
            "ui_bridge_visual_assertion" | "uibridgevisualassertion" | "visual_assertion"
            | "visualassertion" => Some(StepType::UiBridgeVisualAssertion),

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
            StepType::LogWatch => "log_watch",
            StepType::Gate => "gate",
            StepType::Command => "command",
            StepType::UiBridge => "ui_bridge",
            StepType::UiBridgeDesignAudit => "ui_bridge_design_audit",
            StepType::Prompt => "prompt",
            StepType::AiSession => "ai_session",
            StepType::AwasDiscover => "awas_discover",
            StepType::AwasExecute => "awas_execute",
            StepType::AwasCheckSupport => "awas_check_support",
            StepType::AwasListActions => "awas_list_actions",
            StepType::AwasExtractElements => "awas_extract_elements",
            StepType::NativeAccessibility => "native_accessibility",
            StepType::SaveWorkflowArtifact => "save_workflow_artifact",
            StepType::Macro => "macro",
            StepType::Watcher => "watcher",
            StepType::CodeExecution => "code_execution",
            StepType::UiBridgeVisualAssertion => "ui_bridge_visual_assertion",
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
        // Current types
        assert_eq!(
            StepType::from_str_compat("command"),
            Some(StepType::Command)
        );
        assert_eq!(StepType::from_str_compat("prompt"), Some(StepType::Prompt));
        assert_eq!(StepType::from_str_compat("test"), Some(StepType::Command));
        assert_eq!(
            StepType::from_str_compat("ui_bridge"),
            Some(StepType::UiBridge)
        );

        // Legacy names map to Command
        assert_eq!(
            StepType::from_str_compat("shell_command"),
            Some(StepType::Command)
        );
        assert_eq!(StepType::from_str_compat("shell"), Some(StepType::Command));
        assert_eq!(
            StepType::from_str_compat("api_request"),
            Some(StepType::Command)
        );
        assert_eq!(StepType::from_str_compat("check"), Some(StepType::Command));
        assert_eq!(
            StepType::from_str_compat("check_group"),
            Some(StepType::Command)
        );
        assert_eq!(
            StepType::from_str_compat("mcp_call"),
            Some(StepType::Command)
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
        assert!(!StepType::Command.requires_ai());
    }

    #[test]
    fn test_is_verification_type() {
        assert!(StepType::Playwright.is_verification_type());
        assert!(StepType::Command.is_verification_type());
        assert!(!StepType::Workflow.is_verification_type());
        assert!(!StepType::Prompt.is_verification_type());
    }

    #[test]
    fn test_as_str_roundtrip() {
        let step_types = [
            StepType::Workflow,
            StepType::Command,
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
        assert_eq!(
            StepType::Screenshot.default_expected_duration_ms(),
            Some(2_000)
        );

        // Command steps medium
        assert_eq!(
            StepType::Command.default_expected_duration_ms(),
            Some(30_000)
        );

        // Playwright tests longer
        assert_eq!(
            StepType::Playwright.default_expected_duration_ms(),
            Some(60_000)
        );

        // AI sessions longest
        assert_eq!(
            StepType::AiSession.default_expected_duration_ms(),
            Some(300_000)
        );
        assert_eq!(
            StepType::Prompt.default_expected_duration_ms(),
            Some(300_000)
        );

        // Workflows
        assert_eq!(
            StepType::Workflow.default_expected_duration_ms(),
            Some(120_000)
        );

        // All step types should return Some value
        let all_types = [
            StepType::Workflow,
            StepType::State,
            StepType::Action,
            StepType::Screenshot,
            StepType::GuiWorkflow,
            StepType::Playwright,
            StepType::LogWatch,
            StepType::Gate,
            StepType::Command,
            StepType::UiBridge,
            StepType::Prompt,
            StepType::AiSession,
            StepType::AwasDiscover,
            StepType::AwasExecute,
            StepType::AwasCheckSupport,
            StepType::AwasListActions,
            StepType::AwasExtractElements,
            StepType::NativeAccessibility,
            StepType::CodeExecution,
            StepType::Macro,
            StepType::Watcher,
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
