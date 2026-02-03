//! Step Event Kind Definitions
//!
//! This module defines all possible step events in the system.
//! If an event kind is not defined here, it cannot be logged.
//!
//! This ensures:
//! 1. Compile-time safety - no typos in event names
//! 2. Exhaustiveness - all events are documented in one place
//! 3. Self-documenting code - reading this file shows all possible events

use crate::unified_workflow_executor::WorkflowPhase;

/// All possible step events in the unified workflow system.
///
/// Each phase has three event types: Start, Complete, Error.
/// The enum is organized by phase for clarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepEventKind {
    // =========================================================================
    // Setup Phase Events
    // =========================================================================
    /// Setup automation step started (shell command, workflow, etc.)
    SetupAutomationStart,
    /// Setup automation step completed successfully
    SetupAutomationComplete,
    /// Setup automation step failed
    SetupAutomationError,

    /// Setup AI/prompt step started
    SetupAiStart,
    /// Setup AI/prompt step completed successfully
    SetupAiComplete,
    /// Setup AI/prompt step failed
    SetupAiError,

    // =========================================================================
    // Verification Phase Events
    // =========================================================================
    /// Verification step started (playwright, test, check)
    VerificationStepStart,
    /// Verification step completed successfully
    VerificationStepComplete,
    /// Verification step failed
    VerificationStepError,

    // =========================================================================
    // Agentic Phase Events
    // =========================================================================
    /// Agentic AI session started
    AgenticAiStart,
    /// Agentic AI session completed successfully
    AgenticAiComplete,
    /// Agentic AI session failed
    AgenticAiError,

    // =========================================================================
    // Completion Phase Events
    // =========================================================================
    /// Completion automation step started
    CompletionAutomationStart,
    /// Completion automation step completed successfully
    CompletionAutomationComplete,
    /// Completion automation step failed
    CompletionAutomationError,

    /// Completion AI/prompt step started
    CompletionAiStart,
    /// Completion AI/prompt step completed successfully
    CompletionAiComplete,
    /// Completion AI/prompt step failed
    CompletionAiError,
}

impl StepEventKind {
    /// Get the workflow phase for this event kind.
    pub fn phase(&self) -> WorkflowPhase {
        match self {
            Self::SetupAutomationStart
            | Self::SetupAutomationComplete
            | Self::SetupAutomationError
            | Self::SetupAiStart
            | Self::SetupAiComplete
            | Self::SetupAiError => WorkflowPhase::Setup,

            Self::VerificationStepStart
            | Self::VerificationStepComplete
            | Self::VerificationStepError => WorkflowPhase::Verification,

            Self::AgenticAiStart
            | Self::AgenticAiComplete
            | Self::AgenticAiError => WorkflowPhase::Agentic,

            Self::CompletionAutomationStart
            | Self::CompletionAutomationComplete
            | Self::CompletionAutomationError
            | Self::CompletionAiStart
            | Self::CompletionAiComplete
            | Self::CompletionAiError => WorkflowPhase::Completion,
        }
    }

    /// Check if this is a start event.
    pub fn is_start_event(&self) -> bool {
        matches!(
            self,
            Self::SetupAutomationStart
                | Self::SetupAiStart
                | Self::VerificationStepStart
                | Self::AgenticAiStart
                | Self::CompletionAutomationStart
                | Self::CompletionAiStart
        )
    }

    /// Check if this is a complete event.
    pub fn is_complete_event(&self) -> bool {
        matches!(
            self,
            Self::SetupAutomationComplete
                | Self::SetupAiComplete
                | Self::VerificationStepComplete
                | Self::AgenticAiComplete
                | Self::CompletionAutomationComplete
                | Self::CompletionAiComplete
        )
    }

    /// Check if this is an error event.
    pub fn is_error_event(&self) -> bool {
        matches!(
            self,
            Self::SetupAutomationError
                | Self::SetupAiError
                | Self::VerificationStepError
                | Self::AgenticAiError
                | Self::CompletionAutomationError
                | Self::CompletionAiError
        )
    }

    /// Check if this is an end event (complete or error).
    pub fn is_end_event(&self) -> bool {
        self.is_complete_event() || self.is_error_event()
    }

    /// Check if this is an AI event (prompt or ai_session).
    pub fn is_ai_event(&self) -> bool {
        matches!(
            self,
            Self::SetupAiStart
                | Self::SetupAiComplete
                | Self::SetupAiError
                | Self::AgenticAiStart
                | Self::AgenticAiComplete
                | Self::AgenticAiError
                | Self::CompletionAiStart
                | Self::CompletionAiComplete
                | Self::CompletionAiError
        )
    }

    /// Check if this is an automation event (non-AI).
    pub fn is_automation_event(&self) -> bool {
        matches!(
            self,
            Self::SetupAutomationStart
                | Self::SetupAutomationComplete
                | Self::SetupAutomationError
                | Self::VerificationStepStart
                | Self::VerificationStepComplete
                | Self::VerificationStepError
                | Self::CompletionAutomationStart
                | Self::CompletionAutomationComplete
                | Self::CompletionAutomationError
        )
    }

    /// Get a human-readable description of this event kind.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SetupAutomationStart => "setup_automation_start",
            Self::SetupAutomationComplete => "setup_automation_complete",
            Self::SetupAutomationError => "setup_automation_error",
            Self::SetupAiStart => "setup_ai_start",
            Self::SetupAiComplete => "setup_ai_complete",
            Self::SetupAiError => "setup_ai_error",
            Self::VerificationStepStart => "verification_step_start",
            Self::VerificationStepComplete => "verification_step_complete",
            Self::VerificationStepError => "verification_step_error",
            Self::AgenticAiStart => "agentic_ai_start",
            Self::AgenticAiComplete => "agentic_ai_complete",
            Self::AgenticAiError => "agentic_ai_error",
            Self::CompletionAutomationStart => "completion_automation_start",
            Self::CompletionAutomationComplete => "completion_automation_complete",
            Self::CompletionAutomationError => "completion_automation_error",
            Self::CompletionAiStart => "completion_ai_start",
            Self::CompletionAiComplete => "completion_ai_complete",
            Self::CompletionAiError => "completion_ai_error",
        }
    }
}

impl std::fmt::Display for StepEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_phases() {
        assert_eq!(
            StepEventKind::SetupAiStart.phase(),
            WorkflowPhase::Setup
        );
        assert_eq!(
            StepEventKind::VerificationStepComplete.phase(),
            WorkflowPhase::Verification
        );
        assert_eq!(
            StepEventKind::AgenticAiError.phase(),
            WorkflowPhase::Agentic
        );
        assert_eq!(
            StepEventKind::CompletionAiComplete.phase(),
            WorkflowPhase::Completion
        );
    }

    #[test]
    fn test_event_categorization() {
        // Start events
        assert!(StepEventKind::SetupAiStart.is_start_event());
        assert!(!StepEventKind::SetupAiStart.is_end_event());

        // Complete events
        assert!(StepEventKind::SetupAiComplete.is_complete_event());
        assert!(StepEventKind::SetupAiComplete.is_end_event());
        assert!(!StepEventKind::SetupAiComplete.is_error_event());

        // Error events
        assert!(StepEventKind::SetupAiError.is_error_event());
        assert!(StepEventKind::SetupAiError.is_end_event());
        assert!(!StepEventKind::SetupAiError.is_complete_event());
    }

    #[test]
    fn test_ai_vs_automation() {
        assert!(StepEventKind::SetupAiStart.is_ai_event());
        assert!(!StepEventKind::SetupAiStart.is_automation_event());

        assert!(StepEventKind::SetupAutomationStart.is_automation_event());
        assert!(!StepEventKind::SetupAutomationStart.is_ai_event());

        // Verification is considered automation
        assert!(StepEventKind::VerificationStepStart.is_automation_event());
    }
}
