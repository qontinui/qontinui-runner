//! Plan Workflow State Definitions
//!
//! Defines the state machine for plan execution.
//! Plans follow a linear flow: Created -> PhaseRunning -> PhaseComplete -> ... -> Completed

use serde::{Deserialize, Serialize};

use crate::workflow_state::WorkflowState;

/// State of a plan execution.
///
/// The plan follows this flow:
/// 1. Created -> PhaseRunning(0) -> PhaseComplete(0) -> PhaseRunning(1) -> ...
/// 2. After all phases: optionally SweepRunning -> SweepComplete
/// 3. Completed
///
/// At any point, the execution can transition to Failed or Stopped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanWorkflowState {
    /// Plan created but not yet started.
    Created,

    /// A phase is currently running.
    PhaseRunning {
        /// Zero-based phase index.
        phase_index: usize,
        /// Human-readable phase name.
        phase_name: String,
    },

    /// A phase has completed.
    PhaseComplete {
        /// Zero-based phase index.
        phase_index: usize,
        /// Human-readable phase name.
        phase_name: String,
        /// Whether this phase succeeded.
        success: bool,
    },

    /// The next-steps sweep is running.
    SweepRunning {
        /// Zero-based sweep iteration index.
        iteration: usize,
    },

    /// The next-steps sweep has completed.
    SweepComplete {
        /// Zero-based sweep iteration index.
        iteration: usize,
        /// Whether the sweep succeeded.
        success: bool,
    },

    /// Plan execution completed successfully.
    Completed,

    /// Plan execution failed.
    Failed {
        /// Reason for failure.
        reason: String,
        /// Phase index where failure occurred (if applicable).
        phase_index: Option<usize>,
        /// Phase name where failure occurred (if applicable).
        phase_name: Option<String>,
    },

    /// Plan execution was stopped by user.
    Stopped {
        /// Phase index where it was stopped (if applicable).
        phase_index: Option<usize>,
        /// Phase name where it was stopped (if applicable).
        phase_name: Option<String>,
    },
}

impl WorkflowState for PlanWorkflowState {
    fn name(&self) -> &'static str {
        match self {
            PlanWorkflowState::Created => "created",
            PlanWorkflowState::PhaseRunning { .. } => "phase_running",
            PlanWorkflowState::PhaseComplete { .. } => "phase_complete",
            PlanWorkflowState::SweepRunning { .. } => "sweep_running",
            PlanWorkflowState::SweepComplete { .. } => "sweep_complete",
            PlanWorkflowState::Completed => "completed",
            PlanWorkflowState::Failed { .. } => "failed",
            PlanWorkflowState::Stopped { .. } => "stopped",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self,
            PlanWorkflowState::Completed
                | PlanWorkflowState::Failed { .. }
                | PlanWorkflowState::Stopped { .. }
        )
    }

    fn is_resumable(&self) -> bool {
        matches!(
            self,
            PlanWorkflowState::PhaseRunning { .. } | PlanWorkflowState::SweepRunning { .. }
        )
    }

    fn phase(&self) -> Option<&'static str> {
        match self {
            PlanWorkflowState::Created => None,
            PlanWorkflowState::PhaseRunning { .. } | PlanWorkflowState::PhaseComplete { .. } => {
                Some("agentic")
            }
            PlanWorkflowState::SweepRunning { .. } | PlanWorkflowState::SweepComplete { .. } => {
                Some("completion")
            }
            PlanWorkflowState::Completed => Some("completion"),
            PlanWorkflowState::Failed { .. } | PlanWorkflowState::Stopped { .. } => None,
        }
    }

    fn iteration(&self) -> Option<u32> {
        match self {
            PlanWorkflowState::PhaseRunning { phase_index, .. }
            | PlanWorkflowState::PhaseComplete { phase_index, .. } => Some(*phase_index as u32 + 1),
            PlanWorkflowState::SweepRunning { iteration, .. }
            | PlanWorkflowState::SweepComplete { iteration, .. } => Some(*iteration as u32 + 1),
            PlanWorkflowState::Failed { phase_index, .. }
            | PlanWorkflowState::Stopped { phase_index, .. } => phase_index.map(|i| i as u32 + 1),
            _ => None,
        }
    }
}

impl PlanWorkflowState {
    pub fn phase_running(phase_index: usize, phase_name: impl Into<String>) -> Self {
        PlanWorkflowState::PhaseRunning {
            phase_index,
            phase_name: phase_name.into(),
        }
    }

    pub fn phase_complete(
        phase_index: usize,
        phase_name: impl Into<String>,
        success: bool,
    ) -> Self {
        PlanWorkflowState::PhaseComplete {
            phase_index,
            phase_name: phase_name.into(),
            success,
        }
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        PlanWorkflowState::Failed {
            reason: reason.into(),
            phase_index: None,
            phase_name: None,
        }
    }

    pub fn failed_in_phase(
        reason: impl Into<String>,
        phase_index: usize,
        phase_name: impl Into<String>,
    ) -> Self {
        PlanWorkflowState::Failed {
            reason: reason.into(),
            phase_index: Some(phase_index),
            phase_name: Some(phase_name.into()),
        }
    }

    pub fn stopped() -> Self {
        PlanWorkflowState::Stopped {
            phase_index: None,
            phase_name: None,
        }
    }

    pub fn stopped_in_phase(phase_index: usize, phase_name: impl Into<String>) -> Self {
        PlanWorkflowState::Stopped {
            phase_index: Some(phase_index),
            phase_name: Some(phase_name.into()),
        }
    }
}

impl std::fmt::Display for PlanWorkflowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanWorkflowState::Created => write!(f, "Created"),
            PlanWorkflowState::PhaseRunning {
                phase_index,
                phase_name,
            } => {
                write!(f, "Phase {} Running: {}", phase_index + 1, phase_name)
            }
            PlanWorkflowState::PhaseComplete {
                phase_index,
                phase_name,
                success,
            } => {
                write!(
                    f,
                    "Phase {} Complete: {} ({})",
                    phase_index + 1,
                    phase_name,
                    if *success { "SUCCESS" } else { "FAILED" }
                )
            }
            PlanWorkflowState::SweepRunning { iteration } => {
                write!(f, "Next Steps Sweep {} Running", iteration + 1)
            }
            PlanWorkflowState::SweepComplete { iteration, success } => {
                write!(
                    f,
                    "Next Steps Sweep {} Complete ({})",
                    iteration + 1,
                    if *success { "SUCCESS" } else { "FAILED" }
                )
            }
            PlanWorkflowState::Completed => write!(f, "Completed"),
            PlanWorkflowState::Failed {
                reason, phase_name, ..
            } => {
                if let Some(name) = phase_name {
                    write!(f, "Failed in {}: {}", name, reason)
                } else {
                    write!(f, "Failed: {}", reason)
                }
            }
            PlanWorkflowState::Stopped { phase_name, .. } => {
                if let Some(name) = phase_name {
                    write!(f, "Stopped in {}", name)
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
        assert_eq!(PlanWorkflowState::Created.name(), "created");
        assert_eq!(
            PlanWorkflowState::phase_running(0, "Phase 1").name(),
            "phase_running"
        );
        assert_eq!(
            PlanWorkflowState::phase_complete(0, "Phase 1", true).name(),
            "phase_complete"
        );
        assert_eq!(
            PlanWorkflowState::SweepRunning { iteration: 0 }.name(),
            "sweep_running"
        );
        assert_eq!(PlanWorkflowState::Completed.name(), "completed");
        assert_eq!(PlanWorkflowState::failed("error").name(), "failed");
    }

    #[test]
    fn test_terminal_states() {
        assert!(!PlanWorkflowState::Created.is_terminal());
        assert!(!PlanWorkflowState::phase_running(0, "Phase 1").is_terminal());
        assert!(!PlanWorkflowState::SweepRunning { iteration: 0 }.is_terminal());
        assert!(PlanWorkflowState::Completed.is_terminal());
        assert!(PlanWorkflowState::failed("error").is_terminal());
        assert!(PlanWorkflowState::stopped().is_terminal());
    }

    #[test]
    fn test_resumable_states() {
        assert!(!PlanWorkflowState::Created.is_resumable());
        assert!(PlanWorkflowState::phase_running(0, "Phase 1").is_resumable());
        assert!(PlanWorkflowState::SweepRunning { iteration: 0 }.is_resumable());
        assert!(!PlanWorkflowState::Completed.is_resumable());
        assert!(!PlanWorkflowState::failed("error").is_resumable());
    }

    #[test]
    fn test_iteration() {
        assert_eq!(PlanWorkflowState::Created.iteration(), None);
        assert_eq!(
            PlanWorkflowState::phase_running(0, "Phase 1").iteration(),
            Some(1)
        );
        assert_eq!(
            PlanWorkflowState::phase_running(2, "Phase 3").iteration(),
            Some(3)
        );
        assert_eq!(
            PlanWorkflowState::SweepRunning { iteration: 0 }.iteration(),
            Some(1)
        );
        assert_eq!(
            PlanWorkflowState::SweepComplete {
                iteration: 2,
                success: true
            }
            .iteration(),
            Some(3)
        );
    }

    #[test]
    fn test_serialization() {
        let state = PlanWorkflowState::phase_running(1, "Phase 2");
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("phase_running"));
        assert!(json.contains("\"phase_index\":1"));

        let parsed: PlanWorkflowState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, state);
    }
}
