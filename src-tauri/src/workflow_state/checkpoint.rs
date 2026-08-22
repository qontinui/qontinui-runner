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
    pub fn is_replayable(&self) -> bool {
        matches!(
            self,
            StepCheckpointStatus::Success | StepCheckpointStatus::Skipped
        )
    }

    /// Did this step reach a terminal state that still represents WORK TO DO?
    ///
    /// Only `Failed`. `Pending`/`Running` are not terminal: they are the debris
    /// of the crash itself (the step was started and never finished), so they
    /// are neither replayed nor reported as a verification outcome.
    pub fn is_outstanding(&self) -> bool {
        matches!(self, StepCheckpointStatus::Failed)
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
///
/// `PartialEq` is derived so [`ReplayLookup`] — which wraps a checkpoint — can
/// be compared in tests; the replay logic itself never compares whole
/// checkpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Content hash of the inputs that determine this step's output — prompt
    /// text, model, provider, the step's own definition, and its declared
    /// upstream input values. See [`crate::workflow_state::fingerprint`].
    ///
    /// **NON-KEY validation column** (alembic `wf_resume_fingerprint_01`): it is
    /// deliberately NOT part of `workflow_step_checkpoints_uniq`. The row is
    /// located by the existing positional key and the fingerprint is then
    /// COMPARED.
    ///
    /// `None` means the row carries no fingerprint — every row written before
    /// this shipped, and every row written while the runner is talking to a
    /// database whose migration has not deployed yet. `None` is a cache MISS,
    /// **never a wildcard**: see [`select_replayable`].
    pub step_fingerprint: Option<String>,
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
            step_fingerprint: None,
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

    /// Set the content fingerprint of the inputs that determine this step's
    /// output.
    ///
    /// The value MUST come from the same
    /// [`crate::workflow_state::fingerprint`] adapter the replay lookup for
    /// this step calls. If the producer and the consumer compute it
    /// differently the row can never match, and replay is silently disabled
    /// for that step rather than loudly broken.
    pub fn with_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.step_fingerprint = Some(fingerprint.into());
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
    /// 3. **Fingerprint-gated.** The positional key says WHERE a step ran, not
    ///    WHAT it ran. `expected_fingerprint` is the content hash of the step
    ///    about to execute; a row whose `step_fingerprint` differs — or that
    ///    carries none at all — is a MISS. See [`select_replayable`].
    pub fn completed_step(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
        stage_index: Option<u32>,
        step_index: usize,
        expected_fingerprint: &str,
    ) -> Result<ReplayLookup<StepCheckpoint>, String> {
        if get_parent_task_id(execution_id) != execution_id {
            debug!(
                execution_id = %execution_id,
                phase = %phase,
                step_index = %step_index,
                "Composed-run child: checkpoints are keyed under the shared parent id, not replaying"
            );
            return Ok(ReplayLookup::NoRow);
        }

        let checkpoints = self.get_completed_steps(execution_id, phase, iteration)?;
        Ok(
            select_replayable(&checkpoints, stage_index, step_index, expected_fingerprint)
                .into_owned(),
        )
    }
}

/// The outcome of a replay lookup.
///
/// A plain `Option` cannot express the distinction this plan turns on: "no
/// journal row" and "a journal row for work that has since CHANGED" both mean
/// re-execute, but only the second means an edit was honoured, and conflating
/// them in the logs is how a silently-disabled replay path goes unnoticed.
///
/// Generic over the checkpoint holder so the pure selector can borrow
/// (`ReplayLookup<&StepCheckpoint>`) while [`CheckpointManager::completed_step`]
/// returns owned (`ReplayLookup<StepCheckpoint>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayLookup<C> {
    /// A replayable row exists AND its fingerprint matches: reuse its result.
    Hit(C),
    /// No replayable row for this slice — the step has not run (or did not
    /// finish) here.
    NoRow,
    /// A replayable row exists for this slice but describes DIFFERENT work.
    /// `stored` is what the row carried (`None` when the row predates the
    /// fingerprint column, or was written against a schema without it).
    FingerprintMismatch { stored: Option<String> },
}

impl<C> ReplayLookup<C> {
    /// The checkpoint to replay, if this is a hit.
    pub fn hit(self) -> Option<C> {
        match self {
            ReplayLookup::Hit(cp) => Some(cp),
            _ => None,
        }
    }

    pub fn is_hit(&self) -> bool {
        matches!(self, ReplayLookup::Hit(_))
    }
}

impl ReplayLookup<&StepCheckpoint> {
    /// Clone the borrowed checkpoint out, for callers that need to own it.
    pub fn into_owned(self) -> ReplayLookup<StepCheckpoint> {
        match self {
            ReplayLookup::Hit(cp) => ReplayLookup::Hit(cp.clone()),
            ReplayLookup::NoRow => ReplayLookup::NoRow,
            ReplayLookup::FingerprintMismatch { stored } => {
                ReplayLookup::FingerprintMismatch { stored }
            }
        }
    }
}

/// Pure half of [`CheckpointManager::completed_step`]: pick the replayable
/// checkpoint for one `(stage_index, step_index)` slice, then check that it
/// describes the SAME work the caller is about to do.
///
/// A missing stage index and stage 0 are the same thing: the write path
/// `COALESCE`s a missing stage to 0 (`database/pg/workflow_state.rs`), so a row
/// read back always carries `Some(0)` where the caller passed `None`.
///
/// # The NULL contract
///
/// The comparison is plain equality on a supplied value. A stored fingerprint
/// that is `None`, empty, or different is a MISS. It is deliberately NOT
/// written as "no stored fingerprint means it matches whatever we ask for":
/// that is the natural SQL instinct (`fingerprint IS NULL OR fingerprint = $1`)
/// and it would serve exactly the stale cached outputs the column exists to
/// prevent. An empty `expected_fingerprint` is likewise a miss, so a caller
/// that could not compute one re-executes rather than replaying blind.
pub fn select_replayable<'a>(
    checkpoints: &'a [StepCheckpoint],
    stage_index: Option<u32>,
    step_index: usize,
    expected_fingerprint: &str,
) -> ReplayLookup<&'a StepCheckpoint> {
    let row = checkpoints.iter().find(|cp| {
        cp.step_index == step_index
            && cp.stage_index.unwrap_or(0) == stage_index.unwrap_or(0)
            && cp.status.is_replayable()
    });

    match row {
        None => ReplayLookup::NoRow,
        Some(cp) => {
            let stored = cp.step_fingerprint.as_deref().filter(|f| !f.is_empty());
            if !expected_fingerprint.is_empty() && stored == Some(expected_fingerprint) {
                ReplayLookup::Hit(cp)
            } else {
                ReplayLookup::FingerprintMismatch {
                    stored: stored.map(str::to_string),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The fingerprint every fixture row carries unless a test says otherwise.
    const FP: &str = "sf1:aaaa";

    fn cp(
        step_index: usize,
        stage_index: Option<u32>,
        status: StepCheckpointStatus,
    ) -> StepCheckpoint {
        cp_fp(step_index, stage_index, status, Some(FP))
    }

    fn cp_fp(
        step_index: usize,
        stage_index: Option<u32>,
        status: StepCheckpointStatus,
        fingerprint: Option<&str>,
    ) -> StepCheckpoint {
        let mut c =
            StepCheckpoint::new("exec-1", "unified", "setup", Some(0), step_index, "command")
                .with_stage_index(stage_index);
        c.status = status;
        c.result_json = Some(format!("{{\"step\":{}}}", step_index));
        c.step_fingerprint = fingerprint.map(str::to_string);
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
        assert_eq!(
            select_replayable(&rows, None, 0, FP)
                .hit()
                .unwrap()
                .step_index,
            0
        );
        assert_eq!(
            select_replayable(&rows, None, 1, FP),
            ReplayLookup::NoRow,
            "failed step must re-execute"
        );
        assert_eq!(
            select_replayable(&rows, None, 2, FP),
            ReplayLookup::NoRow,
            "crash debris must re-execute"
        );
        assert_eq!(
            select_replayable(&rows, None, 3, FP)
                .hit()
                .unwrap()
                .step_index,
            3
        );
        assert_eq!(select_replayable(&rows, None, 9, FP), ReplayLookup::NoRow);
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
            select_replayable(&rows, Some(0), 0, FP)
                .hit()
                .unwrap()
                .status,
            StepCheckpointStatus::Success
        );
        assert_eq!(
            select_replayable(&rows, Some(1), 0, FP),
            ReplayLookup::NoRow,
            "stage 1 must not replay stage 0's result"
        );
        assert_eq!(
            select_replayable(&rows, Some(2), 0, FP),
            ReplayLookup::NoRow,
            "an unwritten stage has nothing to replay"
        );
    }

    /// A missing stage index and stage 0 are the same slice (the write path
    /// COALESCEs a missing stage to 0).
    #[test]
    fn select_replayable_treats_missing_stage_as_stage_zero() {
        let rows = vec![cp(0, None, StepCheckpointStatus::Success)];
        assert!(select_replayable(&rows, Some(0), 0, FP).is_hit());
        assert!(select_replayable(&rows, None, 0, FP).is_hit());
        assert_eq!(
            select_replayable(&rows, Some(1), 0, FP),
            ReplayLookup::NoRow
        );
    }

    // -- Fingerprint gate ---------------------------------------------------

    /// Identical inputs replay.
    #[test]
    fn matching_fingerprint_is_a_hit() {
        let rows = vec![cp(0, Some(0), StepCheckpointStatus::Success)];
        assert!(select_replayable(&rows, Some(0), 0, FP).is_hit());
    }

    /// The defect this closes: the row is in the right slice with the right
    /// status, but describes work that has since been edited.
    #[test]
    fn changed_fingerprint_is_a_miss_not_a_hit() {
        let rows = vec![cp(0, Some(0), StepCheckpointStatus::Success)];
        assert_eq!(
            select_replayable(&rows, Some(0), 0, "sf1:bbbb"),
            ReplayLookup::FingerprintMismatch {
                stored: Some(FP.to_string())
            },
            "an edited step must re-execute, and must be distinguishable from 'no row'"
        );
    }

    /// **NULL is a MISS, never a wildcard.** A row written before the
    /// fingerprint column existed carries none; the SQL instinct
    /// `fingerprint IS NULL OR fingerprint = $1` would replay it, which is
    /// exactly the stale hit the column exists to prevent.
    #[test]
    fn null_stored_fingerprint_is_a_miss_never_a_wildcard() {
        let rows = vec![cp_fp(0, Some(0), StepCheckpointStatus::Success, None)];
        assert_eq!(
            select_replayable(&rows, Some(0), 0, FP),
            ReplayLookup::FingerprintMismatch { stored: None }
        );
        assert!(!select_replayable(&rows, Some(0), 0, FP).is_hit());
        // ...and it does not match an empty expectation either, which would be
        // the same wildcard by another spelling.
        assert!(!select_replayable(&rows, Some(0), 0, "").is_hit());
    }

    /// An empty stored fingerprint is treated exactly like a NULL one -- a
    /// `TEXT` column can hold the empty string, and it is no more evidence
    /// than `NULL`.
    #[test]
    fn empty_stored_fingerprint_is_a_miss() {
        let rows = vec![cp_fp(0, Some(0), StepCheckpointStatus::Success, Some(""))];
        assert_eq!(
            select_replayable(&rows, Some(0), 0, FP),
            ReplayLookup::FingerprintMismatch { stored: None }
        );
    }

    /// A caller that could not compute a fingerprint re-executes rather than
    /// replaying blind.
    #[test]
    fn empty_expected_fingerprint_never_hits() {
        let rows = vec![cp(0, Some(0), StepCheckpointStatus::Success)];
        assert!(!select_replayable(&rows, Some(0), 0, "").is_hit());
    }

    /// Editing step 2 must not stop steps 0 and 1 replaying -- the fingerprint
    /// is per-step, so an edit's re-billing stays bounded.
    #[test]
    fn editing_a_later_step_does_not_invalidate_earlier_checkpoints() {
        let rows = vec![
            cp_fp(0, Some(0), StepCheckpointStatus::Success, Some("sf1:s0")),
            cp_fp(1, Some(0), StepCheckpointStatus::Success, Some("sf1:s1")),
            cp_fp(2, Some(0), StepCheckpointStatus::Success, Some("sf1:s2")),
        ];
        // Step 2 was edited; steps 0 and 1 were not.
        assert!(select_replayable(&rows, Some(0), 0, "sf1:s0").is_hit());
        assert!(select_replayable(&rows, Some(0), 1, "sf1:s1").is_hit());
        assert_eq!(
            select_replayable(&rows, Some(0), 2, "sf1:s2-edited"),
            ReplayLookup::FingerprintMismatch {
                stored: Some("sf1:s2".to_string())
            }
        );
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
