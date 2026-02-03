//! Unified Workflow State Definitions
//!
//! This module defines the explicit state machine for Unified Workflows.
//! These states replace the implicit state inference from task_runs status fields.

use serde::{Deserialize, Serialize};

use crate::workflow_state::WorkflowState;

/// State of a Unified Workflow execution.
///
/// The unified workflow follows this flow:
/// 1. Created -> SetupRunning -> SetupComplete
/// 2. VerificationRunning -> VerificationComplete (repeat with AgenticRunning as needed)
/// 3. CompletionRunning -> CompletionComplete (only if verification passed)
///
/// At any point, the workflow can transition to Failed or Stopped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UnifiedWorkflowState {
    /// Workflow created but not yet started.
    Created,

    /// Setup phase is running.
    SetupRunning,

    /// Setup phase completed successfully.
    SetupComplete,

    /// Verification phase is running.
    VerificationRunning {
        /// Current iteration (1-indexed).
        iteration: u32,
    },

    /// Verification phase completed for this iteration.
    VerificationComplete {
        /// Current iteration (1-indexed).
        iteration: u32,
        /// Whether all verification checks passed.
        passed: bool,
    },

    /// Agentic phase is running (AI fixing issues).
    AgenticRunning {
        /// Current iteration (1-indexed).
        iteration: u32,
    },

    /// Agentic phase completed for this iteration.
    AgenticComplete {
        /// Current iteration (1-indexed).
        iteration: u32,
    },

    /// Completion phase is running.
    CompletionRunning,

    /// Completion phase finished (workflow done).
    CompletionComplete,

    /// Workflow failed with an error.
    Failed {
        /// Reason for failure.
        reason: String,
        /// Phase where failure occurred.
        phase: Option<String>,
        /// Iteration where failure occurred (if applicable).
        iteration: Option<u32>,
    },

    /// Workflow was stopped by user request.
    Stopped {
        /// Phase where it was stopped.
        phase: Option<String>,
        /// Iteration where it was stopped (if applicable).
        iteration: Option<u32>,
    },
}

impl WorkflowState for UnifiedWorkflowState {
    fn name(&self) -> &'static str {
        match self {
            UnifiedWorkflowState::Created => "created",
            UnifiedWorkflowState::SetupRunning => "setup_running",
            UnifiedWorkflowState::SetupComplete => "setup_complete",
            UnifiedWorkflowState::VerificationRunning { .. } => "verification_running",
            UnifiedWorkflowState::VerificationComplete { .. } => "verification_complete",
            UnifiedWorkflowState::AgenticRunning { .. } => "agentic_running",
            UnifiedWorkflowState::AgenticComplete { .. } => "agentic_complete",
            UnifiedWorkflowState::CompletionRunning => "completion_running",
            UnifiedWorkflowState::CompletionComplete => "completion_complete",
            UnifiedWorkflowState::Failed { .. } => "failed",
            UnifiedWorkflowState::Stopped { .. } => "stopped",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self,
            UnifiedWorkflowState::CompletionComplete
                | UnifiedWorkflowState::Failed { .. }
                | UnifiedWorkflowState::Stopped { .. }
        )
    }

    fn is_resumable(&self) -> bool {
        // We can resume from any "running" state
        matches!(
            self,
            UnifiedWorkflowState::SetupRunning
                | UnifiedWorkflowState::VerificationRunning { .. }
                | UnifiedWorkflowState::AgenticRunning { .. }
                | UnifiedWorkflowState::CompletionRunning
        )
    }

    fn phase(&self) -> Option<&'static str> {
        match self {
            UnifiedWorkflowState::Created => None,
            UnifiedWorkflowState::SetupRunning | UnifiedWorkflowState::SetupComplete => {
                Some("setup")
            }
            UnifiedWorkflowState::VerificationRunning { .. }
            | UnifiedWorkflowState::VerificationComplete { .. } => Some("verification"),
            UnifiedWorkflowState::AgenticRunning { .. }
            | UnifiedWorkflowState::AgenticComplete { .. } => Some("agentic"),
            UnifiedWorkflowState::CompletionRunning | UnifiedWorkflowState::CompletionComplete => {
                Some("completion")
            }
            // For failed/stopped states, we can't return &'static str since phase is owned String
            // Just return the known phases or None
            UnifiedWorkflowState::Failed { phase, .. } => match phase.as_deref() {
                Some("setup") => Some("setup"),
                Some("verification") => Some("verification"),
                Some("agentic") => Some("agentic"),
                Some("completion") => Some("completion"),
                _ => None,
            },
            UnifiedWorkflowState::Stopped { phase, .. } => match phase.as_deref() {
                Some("setup") => Some("setup"),
                Some("verification") => Some("verification"),
                Some("agentic") => Some("agentic"),
                Some("completion") => Some("completion"),
                _ => None,
            },
        }
    }

    fn iteration(&self) -> Option<u32> {
        match self {
            UnifiedWorkflowState::VerificationRunning { iteration }
            | UnifiedWorkflowState::VerificationComplete { iteration, .. }
            | UnifiedWorkflowState::AgenticRunning { iteration }
            | UnifiedWorkflowState::AgenticComplete { iteration } => Some(*iteration),
            UnifiedWorkflowState::Failed { iteration, .. }
            | UnifiedWorkflowState::Stopped { iteration, .. } => *iteration,
            _ => None,
        }
    }
}

impl UnifiedWorkflowState {
    /// Create a new Created state.
    pub fn created() -> Self {
        UnifiedWorkflowState::Created
    }

    /// Create a SetupRunning state.
    pub fn setup_running() -> Self {
        UnifiedWorkflowState::SetupRunning
    }

    /// Create a SetupComplete state.
    pub fn setup_complete() -> Self {
        UnifiedWorkflowState::SetupComplete
    }

    /// Create a VerificationRunning state.
    pub fn verification_running(iteration: u32) -> Self {
        UnifiedWorkflowState::VerificationRunning { iteration }
    }

    /// Create a VerificationComplete state.
    pub fn verification_complete(iteration: u32, passed: bool) -> Self {
        UnifiedWorkflowState::VerificationComplete { iteration, passed }
    }

    /// Create an AgenticRunning state.
    pub fn agentic_running(iteration: u32) -> Self {
        UnifiedWorkflowState::AgenticRunning { iteration }
    }

    /// Create an AgenticComplete state.
    pub fn agentic_complete(iteration: u32) -> Self {
        UnifiedWorkflowState::AgenticComplete { iteration }
    }

    /// Create a CompletionRunning state.
    pub fn completion_running() -> Self {
        UnifiedWorkflowState::CompletionRunning
    }

    /// Create a CompletionComplete state.
    pub fn completion_complete() -> Self {
        UnifiedWorkflowState::CompletionComplete
    }

    /// Create a Failed state.
    pub fn failed(reason: impl Into<String>) -> Self {
        UnifiedWorkflowState::Failed {
            reason: reason.into(),
            phase: None,
            iteration: None,
        }
    }

    /// Create a Failed state with phase info.
    pub fn failed_in_phase(
        reason: impl Into<String>,
        phase: impl Into<String>,
        iteration: Option<u32>,
    ) -> Self {
        UnifiedWorkflowState::Failed {
            reason: reason.into(),
            phase: Some(phase.into()),
            iteration,
        }
    }

    /// Create a Stopped state.
    pub fn stopped() -> Self {
        UnifiedWorkflowState::Stopped {
            phase: None,
            iteration: None,
        }
    }

    /// Create a Stopped state with phase info.
    pub fn stopped_in_phase(phase: impl Into<String>, iteration: Option<u32>) -> Self {
        UnifiedWorkflowState::Stopped {
            phase: Some(phase.into()),
            iteration,
        }
    }

    /// Check if the workflow is in a "complete" state (passed verification and ran completion).
    pub fn is_success(&self) -> bool {
        matches!(self, UnifiedWorkflowState::CompletionComplete)
    }

    /// Check if verification passed in the last VerificationComplete state.
    pub fn verification_passed(&self) -> Option<bool> {
        match self {
            UnifiedWorkflowState::VerificationComplete { passed, .. } => Some(*passed),
            _ => None,
        }
    }

    /// Get the error reason if in Failed state.
    pub fn error_reason(&self) -> Option<&str> {
        match self {
            UnifiedWorkflowState::Failed { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

impl std::fmt::Display for UnifiedWorkflowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnifiedWorkflowState::Created => write!(f, "Created"),
            UnifiedWorkflowState::SetupRunning => write!(f, "Setup Running"),
            UnifiedWorkflowState::SetupComplete => write!(f, "Setup Complete"),
            UnifiedWorkflowState::VerificationRunning { iteration } => {
                write!(f, "Verification Running (iteration {})", iteration)
            }
            UnifiedWorkflowState::VerificationComplete { iteration, passed } => {
                write!(
                    f,
                    "Verification Complete (iteration {}, {})",
                    iteration,
                    if *passed { "PASSED" } else { "FAILED" }
                )
            }
            UnifiedWorkflowState::AgenticRunning { iteration } => {
                write!(f, "Agentic Running (iteration {})", iteration)
            }
            UnifiedWorkflowState::AgenticComplete { iteration } => {
                write!(f, "Agentic Complete (iteration {})", iteration)
            }
            UnifiedWorkflowState::CompletionRunning => write!(f, "Completion Running"),
            UnifiedWorkflowState::CompletionComplete => write!(f, "Completion Complete"),
            UnifiedWorkflowState::Failed { reason, phase, .. } => {
                if let Some(p) = phase {
                    write!(f, "Failed in {}: {}", p, reason)
                } else {
                    write!(f, "Failed: {}", reason)
                }
            }
            UnifiedWorkflowState::Stopped { phase, .. } => {
                if let Some(p) = phase {
                    write!(f, "Stopped in {}", p)
                } else {
                    write!(f, "Stopped")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_names() {
        assert_eq!(UnifiedWorkflowState::Created.name(), "created");
        assert_eq!(UnifiedWorkflowState::SetupRunning.name(), "setup_running");
        assert_eq!(
            UnifiedWorkflowState::verification_running(1).name(),
            "verification_running"
        );
        assert_eq!(UnifiedWorkflowState::failed("test").name(), "failed");
    }

    #[test]
    fn test_terminal_states() {
        assert!(!UnifiedWorkflowState::Created.is_terminal());
        assert!(!UnifiedWorkflowState::SetupRunning.is_terminal());
        assert!(!UnifiedWorkflowState::verification_running(1).is_terminal());
        assert!(UnifiedWorkflowState::CompletionComplete.is_terminal());
        assert!(UnifiedWorkflowState::failed("error").is_terminal());
        assert!(UnifiedWorkflowState::stopped().is_terminal());
    }

    #[test]
    fn test_resumable_states() {
        assert!(!UnifiedWorkflowState::Created.is_resumable());
        assert!(UnifiedWorkflowState::SetupRunning.is_resumable());
        assert!(UnifiedWorkflowState::verification_running(1).is_resumable());
        assert!(UnifiedWorkflowState::agentic_running(1).is_resumable());
        assert!(UnifiedWorkflowState::CompletionRunning.is_resumable());
        assert!(!UnifiedWorkflowState::CompletionComplete.is_resumable());
        assert!(!UnifiedWorkflowState::failed("error").is_resumable());
    }

    #[test]
    fn test_phase_and_iteration() {
        assert_eq!(UnifiedWorkflowState::Created.phase(), None);
        assert_eq!(UnifiedWorkflowState::SetupRunning.phase(), Some("setup"));
        assert_eq!(
            UnifiedWorkflowState::verification_running(2).phase(),
            Some("verification")
        );
        assert_eq!(
            UnifiedWorkflowState::verification_running(2).iteration(),
            Some(2)
        );
        assert_eq!(
            UnifiedWorkflowState::agentic_running(3).iteration(),
            Some(3)
        );
    }

    #[test]
    fn test_serialization() {
        let state = UnifiedWorkflowState::verification_running(2);
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("verification_running"));
        assert!(json.contains("\"iteration\":2"));

        let parsed: UnifiedWorkflowState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, state);
    }
}
