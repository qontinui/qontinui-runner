//! Step-Level Checkpointing
//!
//! Provides fine-grained checkpointing at the step level for resume capability.
//! When a workflow is interrupted mid-execution, this allows it to resume
//! from the exact step where it stopped.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::debug;

use crate::database::pg::PgDb;
use crate::unified_workflow_executor::get_parent_task_id;

/// Status of a step checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepCheckpointStatus {
    /// Step is pending execution.
    Pending,
    /// Step is currently running.
    Running,
    /// Step completed successfully.
    Success,
    /// Step failed.
    Failed,
    /// Step was skipped.
    Skipped,
}

impl std::fmt::Display for StepCheckpointStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepCheckpointStatus::Pending => write!(f, "pending"),
            StepCheckpointStatus::Running => write!(f, "running"),
            StepCheckpointStatus::Success => write!(f, "success"),
            StepCheckpointStatus::Failed => write!(f, "failed"),
            StepCheckpointStatus::Skipped => write!(f, "skipped"),
        }
    }
}

impl StepCheckpointStatus {
    /// Does this status mean the step's journaled outcome may be REUSED on
    /// resume instead of executing the step again?
    ///
    /// This is the single definition of "Phase 2 replays it". The narration
    /// reader (`unified_workflow_executor::health_monitor::
    /// build_resume_agentic_context`) uses [`Self::is_outstanding`], which is
    /// the disjoint complement over the terminal statuses — so a step can never
    /// be both replayed as done AND narrated to the model as still to do.
    /// `checkpoint_reader_partition_is_total_and_disjoint` pins that property.
    ///
    /// Written as an exhaustive `match` rather than `matches!` on purpose: a
    /// new `StepCheckpointStatus` variant must be a COMPILE ERROR here, not a
    /// silent `false`. Under `matches!` a new variant would fall out of both
    /// this predicate and [`Self::is_outstanding`] and therefore out of
    /// [`Self::is_terminal`] — neither replayed nor narrated to the model, and
    /// no test would catch it, because the partition test enumerates the
    /// variants it already knows about.
    pub fn is_replayable(&self) -> bool {
        match self {
            StepCheckpointStatus::Success | StepCheckpointStatus::Skipped => true,
            StepCheckpointStatus::Pending
            | StepCheckpointStatus::Running
            | StepCheckpointStatus::Failed => false,
        }
    }

    /// Did this step reach a terminal state that still represents WORK TO DO?
    ///
    /// Only `Failed`. `Pending`/`Running` are not terminal: they are the debris
    /// of the crash itself (the step was started and never finished), so they
    /// are neither replayed nor reported as a verification outcome.
    ///
    /// Exhaustive for the same reason as [`Self::is_replayable`] — see there.
    pub fn is_outstanding(&self) -> bool {
        match self {
            StepCheckpointStatus::Failed => true,
            StepCheckpointStatus::Pending
            | StepCheckpointStatus::Running
            | StepCheckpointStatus::Success
            | StepCheckpointStatus::Skipped => false,
        }
    }

    /// Did the step finish, one way or another?
    ///
    /// `Pending`/`Running` rows survive a crash and would otherwise be counted
    /// as verification results that never actually produced one.
    pub fn is_terminal(&self) -> bool {
        self.is_replayable() || self.is_outstanding()
    }
}

impl std::str::FromStr for StepCheckpointStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(StepCheckpointStatus::Pending),
            "running" => Ok(StepCheckpointStatus::Running),
            "success" => Ok(StepCheckpointStatus::Success),
            "failed" => Ok(StepCheckpointStatus::Failed),
            "skipped" => Ok(StepCheckpointStatus::Skipped),
            _ => Err(format!("Unknown step checkpoint status: {}", s)),
        }
    }
}

/// A checkpoint for a single step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepCheckpoint {
    /// Unique ID for this checkpoint.
    pub id: String,
    /// Execution ID (task_run_id) this checkpoint belongs to.
    pub execution_id: String,
    /// Workflow type.
    pub workflow_type: String,
    /// Phase of the workflow (e.g., "setup", "verification", "agentic", "completion").
    pub phase: String,
    /// Iteration number (for phases that repeat).
    pub iteration: Option<u32>,
    /// Step index within the phase.
    pub step_index: usize,
    /// Stage index for multi-stage phased workflows (0-indexed).
    pub stage_index: Option<u32>,
    /// Step type (e.g., "playwright", "automation", "ai").
    pub step_type: String,
    /// Step name for display.
    pub step_name: Option<String>,
    /// Current status.
    pub status: StepCheckpointStatus,
    /// Result data as JSON (if completed).
    pub result_json: Option<String>,
    /// Step configuration as JSON (single source of truth for step config).
    /// This eliminates the need to look up config in task_runs.execution_steps_json.
    pub step_config_json: Option<String>,
    /// When the step started.
    pub started_at: Option<String>,
    /// When the step completed.
    pub completed_at: Option<String>,
    /// Duration in milliseconds.
    pub duration_ms: Option<i64>,
    /// Error message if failed.
    pub error: Option<String>,
}

impl StepCheckpoint {
    /// Create a new step checkpoint in pending state.
    pub fn new(
        execution_id: impl Into<String>,
        workflow_type: impl Into<String>,
        phase: impl Into<String>,
        iteration: Option<u32>,
        step_index: usize,
        step_type: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            execution_id: execution_id.into(),
            workflow_type: workflow_type.into(),
            phase: phase.into(),
            iteration,
            step_index,
            stage_index: None,
            step_type: step_type.into(),
            step_name: None,
            status: StepCheckpointStatus::Pending,
            result_json: None,
            step_config_json: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            error: None,
        }
    }

    /// Set the step name.
    pub fn with_step_name(mut self, name: impl Into<String>) -> Self {
        self.step_name = Some(name.into());
        self
    }

    /// Set the stage index for multi-stage workflows.
    pub fn with_stage_index(mut self, stage_index: Option<u32>) -> Self {
        self.stage_index = stage_index;
        self
    }

    /// Set the step configuration JSON.
    /// This stores the full step configuration as the single source of truth.
    pub fn with_step_config<T: serde::Serialize>(mut self, config: &T) -> Self {
        self.step_config_json = serde_json::to_string(config).ok();
        self
    }

    /// Set the step configuration JSON from a raw string.
    pub fn with_step_config_json(mut self, config_json: impl Into<String>) -> Self {
        self.step_config_json = Some(config_json.into());
        self
    }

    /// Mark the step as started.
    pub fn mark_started(&mut self) {
        self.status = StepCheckpointStatus::Running;
        self.started_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Mark the step as completed successfully.
    pub fn mark_success(&mut self, result_json: Option<String>, duration_ms: i64) {
        self.status = StepCheckpointStatus::Success;
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
        self.duration_ms = Some(duration_ms);
        self.result_json = result_json;
    }

    /// Mark the step as failed.
    pub fn mark_failed(&mut self, error: impl Into<String>, duration_ms: i64) {
        self.status = StepCheckpointStatus::Failed;
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
        self.duration_ms = Some(duration_ms);
        self.error = Some(error.into());
    }

    /// Mark the step as skipped.
    pub fn mark_skipped(&mut self) {
        self.status = StepCheckpointStatus::Skipped;
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }
}

/// Manages step-level checkpoints for workflow execution.
///
/// The CheckpointManager provides:
/// - Saving checkpoints as steps execute
/// - Querying completed steps for resume
/// - Clearing checkpoints for re-execution
#[derive(Clone)]
pub struct CheckpointManager {
    db: Arc<PgDb>,
    workflow_type: String,
}

impl std::fmt::Debug for CheckpointManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointManager")
            .field("workflow_type", &self.workflow_type)
            .field("db", &"<PgDb>")
            .finish()
    }
}

impl CheckpointManager {
    /// Create a new checkpoint manager using PgDb::global().
    pub fn new(workflow_type: impl Into<String>) -> Self {
        Self {
            db: PgDb::global(),
            workflow_type: workflow_type.into(),
        }
    }

    /// Create a new checkpoint manager with an explicit PgDb.
    pub fn with_db(db: Arc<PgDb>, workflow_type: impl Into<String>) -> Self {
        Self {
            db,
            workflow_type: workflow_type.into(),
        }
    }

    /// Save a step checkpoint.
    ///
    /// This is called when a step starts (status=running) and when it completes
    /// (status=success/failed/skipped).
    ///
    /// Note: For composed run children (e.g., composed-run-X-workflow-N),
    /// the execution_id is automatically remapped to the parent task ID because
    /// only parent IDs exist in task_runs (required by foreign key constraint).
    pub fn save_step(&self, checkpoint: &StepCheckpoint) -> Result<(), String> {
        // For composed run children, remap to parent ID to satisfy FK constraint
        let parent_id = get_parent_task_id(&checkpoint.execution_id);

        debug!(
            execution_id = %checkpoint.execution_id,
            parent_id = %parent_id,
            phase = %checkpoint.phase,
            step_index = %checkpoint.step_index,
            status = %checkpoint.status,
            "Saving step checkpoint"
        );

        // Create a modified checkpoint with the parent ID if needed
        if parent_id != checkpoint.execution_id {
            let mut modified = checkpoint.clone();
            modified.execution_id = parent_id;
            self.db.save_workflow_step_checkpoint_sync(&modified)
        } else {
            self.db.save_workflow_step_checkpoint_sync(checkpoint)
        }
    }

    /// Get all completed steps for a given execution, phase, and iteration.
    ///
    /// This is used during resume to determine which steps have already been executed.
    pub fn get_completed_steps(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
    ) -> Result<Vec<StepCheckpoint>, String> {
        self.db
            .get_workflow_step_checkpoints_sync(execution_id, phase, iteration)
    }

    /// Clear all checkpoints for a specific iteration.
    ///
    /// This is used when retrying a failed iteration.
    pub fn clear_iteration_checkpoints(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: u32,
    ) -> Result<(), String> {
        debug!(
            execution_id = %execution_id,
            phase = %phase,
            iteration = %iteration,
            "Clearing iteration checkpoints"
        );

        self.db
            .delete_workflow_step_checkpoints_sync(execution_id, Some(phase), Some(iteration))
    }

    /// Clear all checkpoints for an execution.
    ///
    /// This is used when starting a fresh execution.
    pub fn clear_all_checkpoints(&self, execution_id: &str) -> Result<(), String> {
        debug!(
            execution_id = %execution_id,
            "Clearing all checkpoints"
        );

        self.db
            .delete_workflow_step_checkpoints_sync(execution_id, None, None)
    }

    /// The journaled checkpoint for one step slice, if that step is already
    /// done and its outcome may be reused instead of re-executing it.
    ///
    /// This replaces the former `is_step_completed` passthrough (and the
    /// unused `get_last_completed_step_index`): a `bool` cannot carry the
    /// `result_json` that the resume path must REUSE, and the last-completed
    /// index silently assumed the completed set was contiguous, which a
    /// partially-failed phase violates.
    ///
    /// Two things it does that the old predicate did not:
    ///
    /// 1. **Stage-aware.** `(phase, iteration, step_index)` is NOT unique — the
    ///    checkpoint table is keyed on `stage_index` too, and every stage of a
    ///    multi-stage workflow writes its setup steps as
    ///    `iteration = Some(0), step_index = 0..`. A stage-blind lookup would
    ///    replay stage 0's result as stage 1's.
    /// 2. **Refuses composed-run children.** `save_step` remaps a child's
    ///    `execution_id` to `get_parent_task_id(..)`, so every child of one
    ///    composed run writes into a SINGLE shared keyspace and siblings
    ///    overwrite each other. The read path does NOT remap, so a child reads
    ///    back nothing today regardless; refusing explicitly makes that
    ///    outcome independent of the asymmetry, because a read made symmetric
    ///    with the write would hand a child its SIBLING's result — which is
    ///    not evidence about this workflow at all.
    pub fn completed_step(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
        stage_index: Option<u32>,
        step_index: usize,
    ) -> Result<Option<StepCheckpoint>, String> {
        if get_parent_task_id(execution_id) != execution_id {
            debug!(
                execution_id = %execution_id,
                phase = %phase,
                step_index = %step_index,
                "Composed-run child: checkpoints are keyed under the shared parent id, not replaying"
            );
            return Ok(None);
        }

        let checkpoints = self.get_completed_steps(execution_id, phase, iteration)?;
        Ok(select_replayable(&checkpoints, stage_index, step_index).cloned())
    }
}

/// Pure half of [`CheckpointManager::completed_step`]: pick the replayable
/// checkpoint for one `(stage_index, step_index)` slice.
///
/// A missing stage index and stage 0 are the same thing: the write path
/// `COALESCE`s a missing stage to 0 (`database/pg/workflow_state.rs`), so a row
/// read back always carries `Some(0)` where the caller passed `None`.
pub fn select_replayable(
    checkpoints: &[StepCheckpoint],
    stage_index: Option<u32>,
    step_index: usize,
) -> Option<&StepCheckpoint> {
    checkpoints.iter().find(|cp| {
        cp.step_index == step_index && in_stage(cp, stage_index) && cp.status.is_replayable()
    })
}

/// Does this checkpoint belong to the stage the caller is asking about?
///
/// The single definition of the missing-stage rule that
/// [`select_replayable`] and every stage-scoped count share: the write path
/// `COALESCE`s a missing stage to 0, so a row read back always carries
/// `Some(0)` where the caller passed `None`, and the two must compare equal.
/// Spelling this once keeps a second stage-aware reader from drifting into
/// the stage-BLIND shape `select_replayable` exists to avoid — `(phase,
/// iteration, step_index)` is not unique across stages.
pub fn in_stage(cp: &StepCheckpoint, stage_index: Option<u32>) -> bool {
    cp.stage_index.unwrap_or(0) == stage_index.unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The write path `COALESCE`s a missing stage to 0, so a caller passing
    /// `None` and a row carrying `Some(0)` are the SAME stage. Every
    /// stage-scoped reader depends on that equality holding in one place.
    #[test]
    fn a_missing_stage_and_stage_zero_are_the_same_stage() {
        let mut cp = StepCheckpoint::new("exec", "unified", "verification", Some(1), 0, "cmd");

        cp.stage_index = None;
        assert!(in_stage(&cp, None));
        assert!(in_stage(&cp, Some(0)));
        assert!(!in_stage(&cp, Some(1)));

        cp.stage_index = Some(0);
        assert!(in_stage(&cp, None));
        assert!(in_stage(&cp, Some(0)));

        cp.stage_index = Some(2);
        assert!(in_stage(&cp, Some(2)));
        assert!(!in_stage(&cp, None));
        assert!(!in_stage(&cp, Some(0)));
    }

    #[test]
    fn test_step_checkpoint_lifecycle() {
        let mut checkpoint = StepCheckpoint::new(
            "exec-1",
            "unified",
            "verification",
            Some(1),
            0,
            "playwright",
        )
        .with_step_name("Test Login");

        assert_eq!(checkpoint.status, StepCheckpointStatus::Pending);

        checkpoint.mark_started();
        assert_eq!(checkpoint.status, StepCheckpointStatus::Running);
        assert!(checkpoint.started_at.is_some());

        checkpoint.mark_success(Some(r#"{"passed": true}"#.to_string()), 1500);
        assert_eq!(checkpoint.status, StepCheckpointStatus::Success);
        assert!(checkpoint.completed_at.is_some());
        assert_eq!(checkpoint.duration_ms, Some(1500));
    }

    #[test]
    fn test_step_checkpoint_failure() {
        let mut checkpoint = StepCheckpoint::new(
            "exec-2",
            "unified",
            "verification",
            Some(1),
            0,
            "playwright",
        );

        checkpoint.mark_started();
        checkpoint.mark_failed("Assertion failed: expected 'Submit' button", 2500);

        assert_eq!(checkpoint.status, StepCheckpointStatus::Failed);
        assert!(checkpoint.error.is_some());
        assert_eq!(checkpoint.duration_ms, Some(2500));
    }

    fn cp(
        step_index: usize,
        stage_index: Option<u32>,
        status: StepCheckpointStatus,
    ) -> StepCheckpoint {
        let mut c =
            StepCheckpoint::new("exec-1", "unified", "setup", Some(0), step_index, "command")
                .with_stage_index(stage_index);
        c.status = status;
        c.result_json = Some(format!("{{\"step\":{}}}", step_index));
        c
    }

    /// The two journal readers partition the status set: every status is
    /// replayed by Phase 2 XOR narrated as outstanding XOR not terminal at all.
    #[test]
    fn checkpoint_reader_partition_is_total_and_disjoint() {
        let all = [
            StepCheckpointStatus::Pending,
            StepCheckpointStatus::Running,
            StepCheckpointStatus::Success,
            StepCheckpointStatus::Failed,
            StepCheckpointStatus::Skipped,
        ];
        for st in all {
            assert!(
                !(st.is_replayable() && st.is_outstanding()),
                "{:?} is both replayed and narrated as work to do",
                st
            );
            assert_eq!(
                st.is_terminal(),
                st.is_replayable() || st.is_outstanding(),
                "{:?} terminality disagrees with the partition",
                st
            );
        }
        assert!(StepCheckpointStatus::Success.is_replayable());
        assert!(StepCheckpointStatus::Skipped.is_replayable());
        assert!(StepCheckpointStatus::Failed.is_outstanding());
        // Crash debris is neither replayed nor counted as a result.
        assert!(!StepCheckpointStatus::Running.is_terminal());
        assert!(!StepCheckpointStatus::Pending.is_terminal());
    }

    #[test]
    fn select_replayable_only_returns_completed_steps() {
        let rows = vec![
            cp(0, Some(0), StepCheckpointStatus::Success),
            cp(1, Some(0), StepCheckpointStatus::Failed),
            cp(2, Some(0), StepCheckpointStatus::Running),
            cp(3, Some(0), StepCheckpointStatus::Skipped),
        ];
        assert_eq!(select_replayable(&rows, None, 0).unwrap().step_index, 0);
        assert!(
            select_replayable(&rows, None, 1).is_none(),
            "failed step must re-execute"
        );
        assert!(
            select_replayable(&rows, None, 2).is_none(),
            "crash debris must re-execute"
        );
        assert_eq!(select_replayable(&rows, None, 3).unwrap().step_index, 3);
        assert!(select_replayable(&rows, None, 9).is_none());
    }

    /// `(phase, iteration, step_index)` repeats across the stages of a
    /// multi-stage workflow; only `stage_index` separates them.
    #[test]
    fn select_replayable_does_not_cross_stages() {
        let rows = vec![
            cp(0, Some(0), StepCheckpointStatus::Success),
            cp(0, Some(1), StepCheckpointStatus::Failed),
        ];
        assert_eq!(
            select_replayable(&rows, Some(0), 0).unwrap().status,
            StepCheckpointStatus::Success
        );
        assert!(
            select_replayable(&rows, Some(1), 0).is_none(),
            "stage 1 must not replay stage 0's result"
        );
        assert!(
            select_replayable(&rows, Some(2), 0).is_none(),
            "an unwritten stage has nothing to replay"
        );
    }

    /// A missing stage index and stage 0 are the same slice (the write path
    /// COALESCEs a missing stage to 0).
    #[test]
    fn select_replayable_treats_missing_stage_as_stage_zero() {
        let rows = vec![cp(0, None, StepCheckpointStatus::Success)];
        assert!(select_replayable(&rows, Some(0), 0).is_some());
        assert!(select_replayable(&rows, None, 0).is_some());
        assert!(select_replayable(&rows, Some(1), 0).is_none());
    }

    #[test]
    fn test_status_parsing() {
        assert_eq!(
            "pending".parse::<StepCheckpointStatus>().unwrap(),
            StepCheckpointStatus::Pending
        );
        assert_eq!(
            "running".parse::<StepCheckpointStatus>().unwrap(),
            StepCheckpointStatus::Running
        );
        assert_eq!(
            "success".parse::<StepCheckpointStatus>().unwrap(),
            StepCheckpointStatus::Success
        );
        assert_eq!(
            "failed".parse::<StepCheckpointStatus>().unwrap(),
            StepCheckpointStatus::Failed
        );
        assert_eq!(
            "skipped".parse::<StepCheckpointStatus>().unwrap(),
            StepCheckpointStatus::Skipped
        );
    }
}
