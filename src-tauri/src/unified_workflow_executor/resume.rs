//! Resume Manager for Unified Workflow
//!
//! This module provides checkpoint-based resume capability for unified workflows.
//! When a workflow is interrupted (crash, user stop, etc.), the ResumeManager can
//! determine the exact point to resume from using step-level checkpoints.

use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::database::CheckpointDb;
use crate::workflow_state::{CheckpointManager, StepCheckpointStatus};

/// Resume point in a unified workflow.
///
/// Indicates where execution should resume after an interruption.
#[derive(Debug, Clone)]
pub enum ResumePoint {
    /// Start from the beginning (no checkpoints or fresh execution).
    FromStart,

    /// Resume from setup phase at a specific step.
    SetupPhase {
        /// Step index to resume from (0-indexed).
        from_step: usize,
    },

    /// Resume from verification phase at a specific iteration and step.
    VerificationPhase {
        /// Iteration number (1-indexed).
        iteration: u32,
        /// Step index to resume from within the iteration.
        from_step: usize,
    },

    /// Resume from agentic phase at a specific iteration.
    AgenticPhase {
        /// Iteration number (1-indexed).
        iteration: u32,
    },

    /// Resume from completion phase at a specific step.
    CompletionPhase {
        /// Step index to resume from.
        from_step: usize,
    },
}

impl ResumePoint {
    /// Get a human-readable description of the resume point.
    pub fn description(&self) -> String {
        match self {
            ResumePoint::FromStart => "from start".to_string(),
            ResumePoint::SetupPhase { from_step } => {
                format!("from setup phase, step {}", from_step)
            }
            ResumePoint::VerificationPhase { iteration, from_step } => {
                format!(
                    "from verification phase, iteration {}, step {}",
                    iteration, from_step
                )
            }
            ResumePoint::AgenticPhase { iteration } => {
                format!("from agentic phase, iteration {}", iteration)
            }
            ResumePoint::CompletionPhase { from_step } => {
                format!("from completion phase, step {}", from_step)
            }
        }
    }

    /// Check if this is a fresh start (no resume needed).
    pub fn is_fresh_start(&self) -> bool {
        matches!(self, ResumePoint::FromStart)
    }
}

/// Manages resume logic for unified workflows.
///
/// The ResumeManager uses checkpoint data to determine where to resume
/// a workflow that was interrupted. It provides a clear, deterministic
/// way to continue execution without re-running completed steps.
#[derive(Clone)]
pub struct ResumeManager {
    checkpoint_db: Arc<CheckpointDb>,
    checkpoint_mgr: CheckpointManager,
}

impl std::fmt::Debug for ResumeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResumeManager")
            .field("checkpoint_db", &"<CheckpointDb>")
            .field("checkpoint_mgr", &self.checkpoint_mgr)
            .finish()
    }
}

impl ResumeManager {
    /// Create a new ResumeManager.
    pub fn new(checkpoint_db: Arc<CheckpointDb>) -> Self {
        let checkpoint_mgr = CheckpointManager::new(checkpoint_db.clone(), "unified");
        Self {
            checkpoint_db,
            checkpoint_mgr,
        }
    }

    /// Determine the resume point for a given execution.
    ///
    /// This analyzes the workflow state and step checkpoints to find
    /// the exact point where execution should resume.
    pub fn determine_resume_point(&self, execution_id: &str) -> Result<ResumePoint, String> {
        info!(
            execution_id = %execution_id,
            "Determining resume point for workflow"
        );

        // 1. Try to load explicit workflow state
        let state = self
            .checkpoint_db
            .get_workflow_execution_state(execution_id)?;

        match state {
            Some(s) => {
                debug!(
                    execution_id = %execution_id,
                    state_name = %s.state_name,
                    phase = ?s.phase,
                    iteration = ?s.iteration,
                    "Found saved workflow state"
                );

                // Determine resume point from explicit state + step checkpoints
                self.resume_point_from_state(&s.state_name, s.phase.as_deref(), s.iteration, execution_id)
            }
            None => {
                // No explicit state saved - try legacy heuristic
                debug!(
                    execution_id = %execution_id,
                    "No explicit workflow state found, using legacy heuristic"
                );
                self.legacy_resume_point(execution_id)
            }
        }
    }

    /// Determine resume point from explicit workflow state.
    fn resume_point_from_state(
        &self,
        state_name: &str,
        phase: Option<&str>,
        iteration: Option<u32>,
        execution_id: &str,
    ) -> Result<ResumePoint, String> {
        match state_name {
            "created" => {
                // Not started yet
                Ok(ResumePoint::FromStart)
            }

            "setup_running" => {
                // Was in setup phase
                let completed_count = self.count_completed_steps(execution_id, "setup", None)?;
                Ok(ResumePoint::SetupPhase {
                    from_step: completed_count,
                })
            }

            "setup_complete" => {
                // Setup done, start verification from iteration 1
                Ok(ResumePoint::VerificationPhase {
                    iteration: 1,
                    from_step: 0,
                })
            }

            "verification_running" => {
                let iter = iteration.unwrap_or(1);
                let completed_count =
                    self.count_completed_steps(execution_id, "verification", Some(iter))?;
                Ok(ResumePoint::VerificationPhase {
                    iteration: iter,
                    from_step: completed_count,
                })
            }

            "verification_complete" => {
                // Check if verification passed - if so, we might need to go to completion
                // For now, assume we need to continue to agentic or completion based on pass status
                // The loop controller will handle this logic
                let iter = iteration.unwrap_or(1);
                Ok(ResumePoint::AgenticPhase { iteration: iter })
            }

            "agentic_running" => {
                let iter = iteration.unwrap_or(1);
                Ok(ResumePoint::AgenticPhase { iteration: iter })
            }

            "agentic_complete" => {
                // Agentic done, start next verification iteration
                let iter = iteration.unwrap_or(1);
                Ok(ResumePoint::VerificationPhase {
                    iteration: iter + 1,
                    from_step: 0,
                })
            }

            "completion_running" => {
                let completed_count = self.count_completed_steps(execution_id, "completion", None)?;
                Ok(ResumePoint::CompletionPhase {
                    from_step: completed_count,
                })
            }

            "completion_complete" | "failed" | "stopped" => {
                // Terminal states - shouldn't resume
                warn!(
                    execution_id = %execution_id,
                    state_name = %state_name,
                    "Workflow is in terminal state, cannot resume"
                );
                Ok(ResumePoint::FromStart)
            }

            _ => {
                warn!(
                    execution_id = %execution_id,
                    state_name = %state_name,
                    "Unknown workflow state, starting from beginning"
                );
                Ok(ResumePoint::FromStart)
            }
        }
    }

    /// Count completed steps for a given phase and iteration.
    fn count_completed_steps(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
    ) -> Result<usize, String> {
        let checkpoints = self
            .checkpoint_mgr
            .get_completed_steps(execution_id, phase, iteration)?;

        let completed = checkpoints
            .iter()
            .filter(|cp| {
                matches!(
                    cp.status,
                    StepCheckpointStatus::Success | StepCheckpointStatus::Skipped
                )
            })
            .count();

        debug!(
            execution_id = %execution_id,
            phase = %phase,
            iteration = ?iteration,
            completed_steps = %completed,
            "Counted completed steps"
        );

        Ok(completed)
    }

    /// Legacy resume point detection using session_count heuristic.
    ///
    /// This is used as a fallback when no explicit workflow state is saved.
    /// It's less accurate but provides backward compatibility.
    fn legacy_resume_point(&self, execution_id: &str) -> Result<ResumePoint, String> {
        // Check if there's a task run with sessions_count > 0
        let task_run = self.checkpoint_db.get_task_run(execution_id)?;

        match task_run {
            Some(task) => {
                if task.sessions_count > 0 {
                    // Some sessions have run - try to figure out where we are
                    // This is imprecise, so we default to starting verification
                    // from iteration 1, step 0
                    info!(
                        execution_id = %execution_id,
                        sessions_count = %task.sessions_count,
                        "Using legacy heuristic: resuming from verification phase"
                    );
                    Ok(ResumePoint::VerificationPhase {
                        iteration: 1,
                        from_step: 0,
                    })
                } else {
                    // No sessions run yet
                    Ok(ResumePoint::FromStart)
                }
            }
            None => {
                // No task run found
                Ok(ResumePoint::FromStart)
            }
        }
    }

    /// Check if a specific step has been completed.
    pub fn is_step_completed(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
        step_index: usize,
    ) -> Result<bool, String> {
        self.checkpoint_mgr
            .is_step_completed(execution_id, phase, iteration, step_index)
    }

    /// Clear all checkpoints for an execution (for fresh restart).
    pub fn clear_all_checkpoints(&self, execution_id: &str) -> Result<(), String> {
        info!(
            execution_id = %execution_id,
            "Clearing all checkpoints for fresh restart"
        );
        self.checkpoint_mgr.clear_all_checkpoints(execution_id)
    }

    /// Clear checkpoints for a specific iteration (for retry).
    pub fn clear_iteration_checkpoints(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: u32,
    ) -> Result<(), String> {
        info!(
            execution_id = %execution_id,
            phase = %phase,
            iteration = %iteration,
            "Clearing checkpoints for iteration retry"
        );
        self.checkpoint_mgr
            .clear_iteration_checkpoints(execution_id, phase, iteration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resume_point_description() {
        assert_eq!(
            ResumePoint::FromStart.description(),
            "from start"
        );
        assert_eq!(
            ResumePoint::SetupPhase { from_step: 2 }.description(),
            "from setup phase, step 2"
        );
        assert_eq!(
            ResumePoint::VerificationPhase {
                iteration: 3,
                from_step: 1
            }
            .description(),
            "from verification phase, iteration 3, step 1"
        );
        assert_eq!(
            ResumePoint::AgenticPhase { iteration: 2 }.description(),
            "from agentic phase, iteration 2"
        );
        assert_eq!(
            ResumePoint::CompletionPhase { from_step: 0 }.description(),
            "from completion phase, step 0"
        );
    }

    #[test]
    fn test_is_fresh_start() {
        assert!(ResumePoint::FromStart.is_fresh_start());
        assert!(!ResumePoint::SetupPhase { from_step: 0 }.is_fresh_start());
        assert!(!ResumePoint::VerificationPhase {
            iteration: 1,
            from_step: 0
        }
        .is_fresh_start());
    }
}
