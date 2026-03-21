//! Replay from arbitrary checkpoint.
//!
//! Allows users to select a past iteration and replay the workflow from that point,
//! using saved step checkpoints and git commit state.

use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

use crate::database::CheckpointDb;

use super::compensation::CompensationManager;
use super::resume::ResumePoint;
use super::types::ReplayPoint;

/// Target for replaying a workflow execution.
#[derive(Debug, Clone)]
pub enum ReplayTarget {
    /// Replay the entire workflow from iteration N (re-runs verification + agentic).
    FromIteration { iteration: u32 },
    /// Replay just the verification phase of iteration N.
    VerificationOnly { iteration: u32 },
    /// Replay from the agentic phase of iteration N.
    AgenticOnly { iteration: u32 },
}

/// Manages replay operations for completed or failed workflow executions.
pub struct ReplayManager {
    checkpoint_db: Arc<CheckpointDb>,
    compensation: CompensationManager,
}

impl ReplayManager {
    pub fn new(checkpoint_db: Arc<CheckpointDb>) -> Self {
        let compensation = CompensationManager::new(checkpoint_db.clone());
        Self {
            checkpoint_db,
            compensation,
        }
    }

    /// List all available replay points for a given execution.
    ///
    /// Returns one entry per iteration that has recorded checkpoints, along with
    /// the commit hash (if available) and verification results.
    pub fn list_replay_points(
        &self,
        execution_id: &str,
    ) -> Result<Vec<ReplayPoint>, String> {
        let commits = self.compensation.get_iteration_commits(execution_id)?;

        // Get verification results from step checkpoints
        let verification_results = self
            .checkpoint_db
            .get_workflow_verification_results(execution_id)?;

        let mut points = Vec::new();

        // Build replay points from verification results (one per iteration)
        for vr in &verification_results {
            let commit_hash = commits
                .iter()
                .find(|c| c.iteration == vr.iteration as u32)
                .map(|c| c.commit_hash.clone());

            points.push(ReplayPoint {
                iteration: vr.iteration as u32,
                commit_hash,
                passed_checks: vr.passed_steps as usize,
                failed_checks: vr.failed_steps as usize,
                timestamp: vr.created_at.clone(),
            });
        }

        // If no verification results, fall back to commit data
        if points.is_empty() {
            for commit in &commits {
                points.push(ReplayPoint {
                    iteration: commit.iteration,
                    commit_hash: Some(commit.commit_hash.clone()),
                    passed_checks: 0,
                    failed_checks: 0,
                    timestamp: commit.timestamp.clone(),
                });
            }
        }

        points.sort_by_key(|p| p.iteration);
        Ok(points)
    }

    /// Prepare a replay by resetting git state and clearing downstream checkpoints.
    ///
    /// Returns a `ResumePoint` that can be used to re-enter the loop controller.
    pub async fn prepare_replay(
        &self,
        execution_id: &str,
        target: &ReplayTarget,
        working_dir: &Path,
    ) -> Result<ResumePoint, String> {
        let target_iteration = match target {
            ReplayTarget::FromIteration { iteration } => *iteration,
            ReplayTarget::VerificationOnly { iteration } => *iteration,
            ReplayTarget::AgenticOnly { iteration } => *iteration,
        };

        info!(
            "REPLAY: Preparing replay of execution {} from iteration {} ({:?})",
            execution_id, target_iteration, target
        );

        // 1. Reset git to the commit BEFORE the target iteration
        let commits = self.compensation.get_iteration_commits(execution_id)?;

        if target_iteration > 1 {
            // Find commit from the iteration before the target
            let before_commit = commits
                .iter()
                .find(|c| c.iteration == target_iteration - 1);

            if let Some(commit) = before_commit {
                info!(
                    "REPLAY: Resetting git to iteration {} commit {}",
                    target_iteration - 1,
                    &commit.commit_hash[..commit.commit_hash.len().min(8)]
                );
                self.compensation
                    .rollback_to_iteration(execution_id, target_iteration - 1, working_dir)
                    .await?;
            } else {
                warn!(
                    "REPLAY: No commit found for iteration {}, proceeding without git reset",
                    target_iteration - 1
                );
            }
        }

        // 2. Clear checkpoints from target iteration onward
        self.checkpoint_db
            .clear_checkpoints_from_iteration(execution_id, target_iteration)?;

        // 3. Clear iteration commits from target onward
        self.checkpoint_db
            .clear_iteration_commits_from(execution_id, target_iteration)?;

        // 4. Clear iteration diffs from target onward
        self.checkpoint_db
            .clear_iteration_diffs_from(execution_id, target_iteration)?;

        // 5. Return appropriate ResumePoint
        let resume_point = match target {
            ReplayTarget::FromIteration { iteration } => ResumePoint::VerificationPhase {
                iteration: *iteration,
                from_step: 0,
                stage_index: None,
            },
            ReplayTarget::VerificationOnly { iteration } => ResumePoint::VerificationPhase {
                iteration: *iteration,
                from_step: 0,
                stage_index: None,
            },
            ReplayTarget::AgenticOnly { iteration } => ResumePoint::AgenticPhase {
                iteration: *iteration,
                stage_index: None,
            },
        };

        info!(
            "REPLAY: Prepared resume point {:?} for execution {}",
            resume_point, execution_id
        );

        Ok(resume_point)
    }
}
