//! Resume Manager for Unified Workflow
//!
//! This module provides checkpoint-based resume capability for unified workflows.
//! When a workflow is interrupted (crash, user stop, etc.), the ResumeManager can
//! determine the exact point to resume from using step-level checkpoints.

use std::sync::Arc;
use tracing::{debug, info, warn};

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
        /// Stage index for multi-stage workflows (None = single-stage).
        stage_index: Option<u32>,
    },

    /// Resume from verification phase at a specific iteration and step.
    VerificationPhase {
        /// Iteration number (1-indexed).
        iteration: u32,
        /// Step index to resume from within the iteration.
        from_step: usize,
        /// Stage index for multi-stage workflows (None = single-stage).
        stage_index: Option<u32>,
    },

    /// Resume from agentic phase at a specific iteration.
    AgenticPhase {
        /// Iteration number (1-indexed).
        iteration: u32,
        /// Stage index for multi-stage workflows (None = single-stage).
        stage_index: Option<u32>,
    },

    /// Resume from an approval gate (re-present the approval dialog).
    ApprovalPhase {
        /// Iteration number (1-indexed).
        iteration: u32,
        /// Stage index for multi-stage workflows (None = single-stage).
        stage_index: Option<u32>,
        /// The approval request ID to re-present.
        approval_id: String,
    },

    /// Resume from completion phase at a specific step.
    CompletionPhase {
        /// Step index to resume from.
        from_step: usize,
    },

    /// Resume from a specific stage in a multi-stage workflow.
    /// All stages before `from_stage` are skipped.
    StageStart {
        /// Stage index to resume from (0-indexed).
        from_stage: u32,
    },
}

impl ResumePoint {
    /// Get a human-readable description of the resume point.
    pub fn description(&self) -> String {
        match self {
            ResumePoint::FromStart => "from start".to_string(),
            ResumePoint::SetupPhase {
                from_step,
                stage_index,
            } => {
                if let Some(si) = stage_index {
                    format!("from setup phase, step {}, stage {}", from_step, si)
                } else {
                    format!("from setup phase, step {}", from_step)
                }
            }
            ResumePoint::VerificationPhase {
                iteration,
                from_step,
                stage_index,
            } => {
                if let Some(si) = stage_index {
                    format!(
                        "from verification phase, iteration {}, step {}, stage {}",
                        iteration, from_step, si
                    )
                } else {
                    format!(
                        "from verification phase, iteration {}, step {}",
                        iteration, from_step
                    )
                }
            }
            ResumePoint::AgenticPhase {
                iteration,
                stage_index,
            } => {
                if let Some(si) = stage_index {
                    format!("from agentic phase, iteration {}, stage {}", iteration, si)
                } else {
                    format!("from agentic phase, iteration {}", iteration)
                }
            }
            ResumePoint::ApprovalPhase {
                iteration,
                stage_index,
                approval_id,
            } => {
                if let Some(si) = stage_index {
                    format!(
                        "from approval gate '{}', iteration {}, stage {}",
                        approval_id, iteration, si
                    )
                } else {
                    format!(
                        "from approval gate '{}', iteration {}",
                        approval_id, iteration
                    )
                }
            }
            ResumePoint::CompletionPhase { from_step } => {
                format!("from completion phase, step {}", from_step)
            }
            ResumePoint::StageStart { from_stage } => {
                format!("from stage {} start", from_stage)
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
    checkpoint_mgr: CheckpointManager,
    pg_db: std::sync::Arc<crate::database::pg::PgDb>,
}

impl std::fmt::Debug for ResumeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResumeManager")
            .field("checkpoint_mgr", &self.checkpoint_mgr)
            .finish()
    }
}

impl ResumeManager {
    /// Create a new ResumeManager.
    pub fn new(pg_db: std::sync::Arc<crate::database::pg::PgDb>) -> Self {
        let checkpoint_mgr = CheckpointManager::new("unified");
        Self {
            checkpoint_mgr,
            pg_db,
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
        // Workflow execution state removed — all persistence now via PgDb.
        // Workflow execution state removed — all persistence now via PgDb.
        // Always use legacy heuristic.
        debug!(
            execution_id = %execution_id,
            "No explicit workflow state found, using legacy heuristic"
        );
        self.legacy_resume_point(execution_id)
    }

    /// Determine resume point from explicit workflow state.
    fn resume_point_from_state(
        &self,
        state_name: &str,
        _phase: Option<&str>,
        iteration: Option<u32>,
        execution_id: &str,
        state_data: Option<&str>,
    ) -> Result<ResumePoint, String> {
        // Extract stage_index from state_data JSON if present.
        // state_data is the full serialized UnifiedWorkflowState, e.g.:
        // {"type":"verification_running","iteration":2,"stage_index":1}
        let stage_index = state_data.and_then(|data| {
            serde_json::from_str::<serde_json::Value>(data)
                .ok()
                .and_then(|v| v.get("stage_index")?.as_u64().map(|n| n as u32))
        });

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
                    stage_index,
                })
            }

            "setup_complete" => {
                // Setup done, start verification from iteration 1
                Ok(ResumePoint::VerificationPhase {
                    iteration: 1,
                    from_step: 0,
                    stage_index,
                })
            }

            "verification_running" => {
                let iter = iteration.unwrap_or(1);
                let completed_count =
                    self.count_completed_steps(execution_id, "verification", Some(iter))?;
                Ok(ResumePoint::VerificationPhase {
                    iteration: iter,
                    from_step: completed_count,
                    stage_index,
                })
            }

            "verification_complete" => {
                let iter = iteration.unwrap_or(1);
                Ok(ResumePoint::AgenticPhase {
                    iteration: iter,
                    stage_index,
                })
            }

            "agentic_running" => {
                let iter = iteration.unwrap_or(1);
                Ok(ResumePoint::AgenticPhase {
                    iteration: iter,
                    stage_index,
                })
            }

            "agentic_complete" => {
                // Agentic done, start next verification iteration
                let iter = iteration.unwrap_or(1);
                Ok(ResumePoint::VerificationPhase {
                    iteration: iter + 1,
                    from_step: 0,
                    stage_index,
                })
            }

            "completion_running" => {
                let completed_count =
                    self.count_completed_steps(execution_id, "completion", None)?;
                Ok(ResumePoint::CompletionPhase {
                    from_step: completed_count,
                })
            }

            "approval_pending" => {
                // Was waiting for human approval — re-present the approval gate.
                let iter = iteration.unwrap_or(1);
                let approval_id = state_data
                    .and_then(|data| {
                        serde_json::from_str::<serde_json::Value>(data)
                            .ok()
                            .and_then(|v| v.get("approval_id")?.as_str().map(String::from))
                    })
                    .unwrap_or_default();
                Ok(ResumePoint::ApprovalPhase {
                    iteration: iter,
                    stage_index,
                    approval_id,
                })
            }

            "stage_complete" => {
                // A stage completed — resume from the NEXT stage.
                // stage_index is the index of the completed stage, so resume from stage_index + 1.
                let completed_stage = stage_index.unwrap_or(0);
                Ok(ResumePoint::StageStart {
                    from_stage: completed_stage + 1,
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
        let checkpoints =
            self.checkpoint_mgr
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
        // PG-primary: try PG first, fall back to SQLite
        let task_run = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let pg = self.pg_db.clone();
            let id = execution_id.to_string();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handle.block_on(async move { pg.get_task_run(&id).await })
            }))
            .unwrap_or_else(|_| Err("block_on panicked".to_string()))?
        } else {
            Err("no tokio runtime".to_string())?
        };

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
                        stage_index: None,
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
        assert_eq!(ResumePoint::FromStart.description(), "from start");
        assert_eq!(
            ResumePoint::SetupPhase {
                from_step: 2,
                stage_index: None
            }
            .description(),
            "from setup phase, step 2"
        );
        assert_eq!(
            ResumePoint::VerificationPhase {
                iteration: 3,
                from_step: 1,
                stage_index: None,
            }
            .description(),
            "from verification phase, iteration 3, step 1"
        );
        assert_eq!(
            ResumePoint::VerificationPhase {
                iteration: 2,
                from_step: 0,
                stage_index: Some(1),
            }
            .description(),
            "from verification phase, iteration 2, step 0, stage 1"
        );
        assert_eq!(
            ResumePoint::AgenticPhase {
                iteration: 2,
                stage_index: None
            }
            .description(),
            "from agentic phase, iteration 2"
        );
        assert_eq!(
            ResumePoint::CompletionPhase { from_step: 0 }.description(),
            "from completion phase, step 0"
        );
        assert_eq!(
            ResumePoint::StageStart { from_stage: 2 }.description(),
            "from stage 2 start"
        );
    }

    #[test]
    fn test_is_fresh_start() {
        assert!(ResumePoint::FromStart.is_fresh_start());
        assert!(!ResumePoint::SetupPhase {
            from_step: 0,
            stage_index: None
        }
        .is_fresh_start());
        assert!(!ResumePoint::VerificationPhase {
            iteration: 1,
            from_step: 0,
            stage_index: None,
        }
        .is_fresh_start());
    }
}
