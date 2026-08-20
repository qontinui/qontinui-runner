//! Replay from arbitrary checkpoint.
//!
//! Allows users to select a past iteration and replay the workflow from that point,
//! using saved step checkpoints and git commit state.

use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

use crate::database::pg::PgDb;

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
    pg_db: Arc<PgDb>,
    compensation: CompensationManager,
}

impl ReplayManager {
    pub fn new(pg_db: Arc<PgDb>) -> Self {
        let compensation = CompensationManager::new(pg_db.clone());
        Self {
            pg_db,
            compensation,
        }
    }

    /// List all available replay points for a given execution.
    ///
    /// Returns one entry per iteration that has recorded checkpoints, along with
    /// the commit hash (if available) and verification results.
    pub fn list_replay_points(&self, execution_id: &str) -> Result<Vec<ReplayPoint>, String> {
        let commits = self.compensation.get_iteration_commits(execution_id)?;

        // Get verification results from PG via block_in_place
        let pg = self.pg_db.clone();
        let eid = execution_id.to_string();
        let verification_results = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tokio::task::block_in_place(|| {
                    handle.block_on(async move { pg.get_workflow_verification_results(&eid).await })
                })
            }));
            match result {
                Ok(inner) => inner?,
                Err(_) => return Err("block_in_place panicked in list_replay_points".to_string()),
            }
        } else {
            return Err("No tokio runtime available for list_replay_points".to_string());
        };

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
            let before_commit = commits.iter().find(|c| c.iteration == target_iteration - 1);

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

        // 2. Read the stage the target iteration belongs to BEFORE deleting
        //    its checkpoints — after step 3 there is nothing left to read it
        //    from. Phase 1 made `stage_index` load-bearing: `loop_controller`
        //    derives `start_from_stage` from the resume point, so returning
        //    `None` here would restart a multi-stage workflow at stage 0.
        let stage_index = self
            .stage_index_for_iteration(execution_id, target_iteration)
            .await;

        // 3. Clear checkpoints from target iteration onward (PG).
        //    This is what FORCES re-execution: Phase 2 replays a step only when
        //    a completed checkpoint exists for it, so deleting the rows makes
        //    every step from `target_iteration` on look un-run. A deliberate,
        //    user-requested replay therefore always beats the resume skip.
        self.pg_db
            .clear_checkpoints_from_iteration(execution_id, target_iteration)
            .await?;

        // 4. Clear iteration commits from target onward (PG)
        self.pg_db
            .clear_iteration_commits_from(execution_id, target_iteration)
            .await?;

        // 5. Clear iteration diffs from target onward (PG)
        self.pg_db
            .clear_iteration_diffs_from(execution_id, target_iteration)
            .await?;

        // 6. Return appropriate ResumePoint
        let resume_point = replay_resume_point(target, stage_index);

        info!(
            "REPLAY: Prepared resume point {:?} for execution {}",
            resume_point, execution_id
        );

        Ok(resume_point)
    }

    /// Which stage does `iteration` belong to?
    ///
    /// Read from the checkpoints the replay is about to delete. Degrades to
    /// `None` (stage 0) on any read failure or on an iteration with no
    /// checkpoints at all — the same "start at the first stage" convention
    /// `resume::normalize_stage` uses.
    async fn stage_index_for_iteration(&self, execution_id: &str, iteration: u32) -> Option<u32> {
        match self
            .pg_db
            .get_all_workflow_step_checkpoints(execution_id)
            .await
        {
            Ok(checkpoints) => stage_index_of_iteration(&checkpoints, iteration),
            Err(e) => {
                warn!(
                    "REPLAY: could not read checkpoints to recover the stage index: {} - replaying from stage 0",
                    e
                );
                None
            }
        }
    }
}

/// The stage a given iteration ran in, from its checkpoints.
///
/// Stage 0 is reported as `None`: the checkpoint write path COALESCEs a missing
/// stage to 0, so 0 and "no stage" are the same thing and both mean "start at
/// the first stage" (identical to `resume::normalize_stage`).
pub fn stage_index_of_iteration(
    checkpoints: &[crate::workflow_state::StepCheckpoint],
    iteration: u32,
) -> Option<u32> {
    checkpoints
        .iter()
        .filter(|cp| cp.iteration == Some(iteration))
        .filter_map(|cp| cp.stage_index)
        .max()
        .filter(|s| *s > 0)
}

/// Map a replay target onto the resume point the loop controller re-enters at.
///
/// Pure, so the stage-preservation contract is testable without a database.
pub fn replay_resume_point(target: &ReplayTarget, stage_index: Option<u32>) -> ResumePoint {
    match target {
        ReplayTarget::FromIteration { iteration }
        | ReplayTarget::VerificationOnly { iteration } => ResumePoint::VerificationPhase {
            iteration: *iteration,
            from_step: 0,
            stage_index,
        },
        ReplayTarget::AgenticOnly { iteration } => ResumePoint::AgenticPhase {
            iteration: *iteration,
            stage_index,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_state::{StepCheckpoint, StepCheckpointStatus};

    fn cp(iteration: u32, stage_index: Option<u32>, step_index: usize) -> StepCheckpoint {
        let mut c = StepCheckpoint::new(
            "exec-1",
            "unified",
            "verification",
            Some(iteration),
            step_index,
            "playwright",
        );
        c.stage_index = stage_index;
        c.status = StepCheckpointStatus::Success;
        c.result_json = Some("{}".to_string());
        c
    }

    /// A user-requested replay must land back in the stage the iteration
    /// actually ran in. Phase 1 made `stage_index` drive `start_from_stage`,
    /// so a hardcoded `None` restarts a multi-stage workflow at stage 0.
    #[test]
    fn replay_preserves_the_stage_of_the_target_iteration() {
        let rows = vec![cp(1, Some(0), 0), cp(4, Some(2), 0), cp(4, Some(2), 1)];
        assert_eq!(stage_index_of_iteration(&rows, 4), Some(2));

        let point = replay_resume_point(&ReplayTarget::FromIteration { iteration: 4 }, Some(2));
        match point {
            ResumePoint::VerificationPhase {
                iteration,
                from_step,
                stage_index,
            } => {
                assert_eq!(iteration, 4);
                assert_eq!(from_step, 0);
                assert_eq!(stage_index, Some(2));
            }
            other => panic!("expected a verification resume point, got {:?}", other),
        }

        match replay_resume_point(&ReplayTarget::AgenticOnly { iteration: 4 }, Some(2)) {
            ResumePoint::AgenticPhase {
                iteration,
                stage_index,
            } => {
                assert_eq!(iteration, 4);
                assert_eq!(stage_index, Some(2));
            }
            other => panic!("expected an agentic resume point, got {:?}", other),
        }
    }

    #[test]
    fn stage_zero_and_absent_stages_are_both_reported_as_no_stage() {
        assert_eq!(stage_index_of_iteration(&[cp(2, Some(0), 0)], 2), None);
        assert_eq!(stage_index_of_iteration(&[cp(2, None, 0)], 2), None);
        assert_eq!(stage_index_of_iteration(&[], 2), None);
        // An iteration with no rows of its own must not borrow another stage.
        assert_eq!(stage_index_of_iteration(&[cp(9, Some(3), 0)], 2), None);
    }

    /// A deliberate replay is NOT defeated by the Phase-2 skip: it deletes the
    /// checkpoints from the target iteration onward, and the replay lookup only
    /// fires on a checkpoint that still exists.
    #[test]
    fn cleared_checkpoints_force_re_execution() {
        let before = vec![cp(3, Some(1), 0), cp(4, Some(1), 0), cp(5, Some(1), 0)];

        // `clear_checkpoints_from_iteration(.., 4)` deletes `iteration >= 4`.
        let after: Vec<StepCheckpoint> = before
            .iter()
            .filter(|c| c.iteration.unwrap_or(0) < 4)
            .cloned()
            .collect();

        assert_eq!(after.len(), 1, "only iteration 3 survives a replay from 4");
        for iteration in [4u32, 5] {
            assert!(
                !after.iter().any(|c| c.iteration == Some(iteration)),
                "iteration {} must have nothing left to replay",
                iteration
            );
            assert!(
                crate::workflow_state::select_replayable(
                    &after
                        .iter()
                        .filter(|c| c.iteration == Some(iteration))
                        .cloned()
                        .collect::<Vec<_>>(),
                    Some(1),
                    0,
                )
                .is_none(),
                "the replay lookup must miss for iteration {}",
                iteration
            );
        }

        // Iterations BEFORE the target keep their journal, so setup work and
        // earlier iterations are not re-run or re-billed.
        assert!(crate::workflow_state::select_replayable(&after, Some(1), 0).is_some());
    }
}
