//! Resume Manager for Unified Workflow
//!
//! This module provides checkpoint-based resume capability for unified workflows.
//! When a workflow is interrupted (crash, user stop, etc.), the ResumeManager can
//! determine the exact point to resume from using step-level checkpoints.

use std::sync::Arc;
use tracing::{debug, info, warn};

use super::types::is_sequence_child;
use crate::workflow_state::{
    CheckpointManager, StepCheckpoint, StepCheckpointStatus, WorkflowExecutionStateRecord,
};

/// Workflow type every unified workflow persists its state row under
/// (`task_state.rs::persist_workflow_state`).
const UNIFIED_WORKFLOW_TYPE: &str = "unified";

/// Does this checkpoint represent work that must NOT be re-executed on resume?
fn is_completed(cp: &StepCheckpoint) -> bool {
    matches!(
        cp.status,
        StepCheckpointStatus::Success | StepCheckpointStatus::Skipped
    )
}

/// The newest completed checkpoint.
///
/// Ordered by `completed_at` (every `Success`/`Skipped` row stamps it), with
/// execution order — stage, then iteration, then step — as the tie-break.
fn newest_completed(checkpoints: &[StepCheckpoint]) -> Option<&StepCheckpoint> {
    checkpoints
        .iter()
        .filter(|cp| is_completed(cp))
        .max_by(|a, b| {
            a.completed_at
                .as_deref()
                .unwrap_or("")
                .cmp(b.completed_at.as_deref().unwrap_or(""))
                .then_with(|| a.stage_index.unwrap_or(0).cmp(&b.stage_index.unwrap_or(0)))
                .then_with(|| a.iteration.unwrap_or(0).cmp(&b.iteration.unwrap_or(0)))
                .then_with(|| a.step_index.cmp(&b.step_index))
        })
}

/// Count completed steps for one (phase, iteration, stage) slice of an execution.
///
/// A missing stage index and stage 0 are the same thing here: the checkpoint
/// write path COALESCEs a missing stage to 0 (`database/pg/workflow_state.rs`),
/// and `loop_controller`'s `start_from_stage` treats `None` and `Some(0)`
/// identically.
fn count_completed_in(
    checkpoints: &[StepCheckpoint],
    phase: &str,
    iteration: Option<u32>,
    stage_index: Option<u32>,
) -> usize {
    checkpoints
        .iter()
        .filter(|cp| {
            cp.phase == phase
                && cp.iteration == iteration
                && cp.stage_index.unwrap_or(0) == stage_index.unwrap_or(0)
                && is_completed(cp)
        })
        .count()
}

/// Normalize a checkpoint stage index into the `ResumePoint` convention.
///
/// Stage 0 is indistinguishable from "no stage" on the write path (see
/// `count_completed_in`), and both mean "start at the first stage", so 0 is
/// reported as `None` rather than asserting stage knowledge we do not have.
fn normalize_stage(stage_index: Option<u32>) -> Option<u32> {
    stage_index.filter(|s| *s > 0)
}

/// Recover the stage index from checkpoints when the state row does not carry one.
fn stage_index_from_checkpoints(checkpoints: &[StepCheckpoint]) -> Option<u32> {
    newest_completed(checkpoints).and_then(|cp| normalize_stage(cp.stage_index))
}

/// Extract `stage_index` from a serialized `UnifiedWorkflowState`.
///
/// `state_data` is the full serialized state, e.g.
/// `{"type":"verification_running","iteration":2,"stage_index":1}`.
fn stage_index_from_state_data(state_data: Option<&str>) -> Option<u32> {
    state_data.and_then(|data| {
        serde_json::from_str::<serde_json::Value>(data)
            .ok()
            .and_then(|v| v.get("stage_index")?.as_u64().map(|n| n as u32))
    })
}

/// Extract `approval_id` from a serialized `UnifiedWorkflowState::ApprovalPending`.
fn approval_id_from_state_data(state_data: Option<&str>) -> String {
    state_data
        .and_then(|data| {
            serde_json::from_str::<serde_json::Value>(data)
                .ok()
                .and_then(|v| v.get("approval_id")?.as_str().map(String::from))
        })
        .unwrap_or_default()
}

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

/// What [`ResumeManager::clear_checkpoints_for_fresh_run`] decided to do with
/// the step journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearVerdict {
    /// Genuinely a fresh run with nothing journaled — safe to clear.
    Clear,
    /// Not a fresh start, so there is nothing to clear.
    KeptNotAFreshStart,
    /// A composed-run child (`composed-run-<ts>-workflow-<n>`).
    ///
    /// The checkpoint paths are ASYMMETRIC about that id shape: `save_step`
    /// remaps a child to `get_parent_task_id(..)`, while the read and delete
    /// paths pass the id through verbatim (`database/pg/workflow_state.rs`).
    /// So TODAY a child's wipe matches zero rows and destroys nothing - this
    /// arm is an invariant, not a live fix, and it also stops the run logging
    /// a wipe it did not perform. It earns its place because the obvious
    /// repair of that asymmetry (remap on delete too, so a child can clean up
    /// what it wrote) would turn every child's "fresh start" into a wipe of
    /// the PARENT's journal and every sibling's. Deciding on the ID SHAPE,
    /// before the journal is ever read, means that repair cannot become a
    /// data-loss bug.
    KeptComposedRunChild,
    /// `FromStart` was reached with work already journaled. After Phase 1,
    /// `FromStart` no longer means "nothing ran": it is what a MISSING state
    /// row resolves to, which is exactly what a crash mid-setup leaves behind
    /// (checkpoints written, state row never persisted). Wiping there is the
    /// re-execute-and-re-bill defect this plan exists to close.
    KeptJournalNotEmpty,
    /// The journal could not be READ (pool timeout, query error, no runtime,
    /// panic). An absent answer is UNKNOWN, never zero — and the only
    /// irreversible action on this path is the delete, so an unknown read must
    /// never authorise it. This is why the delete path takes a FALLIBLE
    /// accessor while the resume-point path keeps the soft-degrade one: for the
    /// resume point, "empty" is the conservative direction (re-run work); for
    /// the delete, it is exactly inverted (destroy the journal that would have
    /// let the run skip work it already paid for).
    KeptJournalUnknown,
}

impl ClearVerdict {
    /// Human-readable reason, for the log line at the call site.
    pub fn reason(&self) -> &'static str {
        match self {
            ClearVerdict::Clear => "fresh run with an empty journal",
            ClearVerdict::KeptNotAFreshStart => "resuming, not a fresh start",
            ClearVerdict::KeptComposedRunChild => {
                "composed-run child: the journal belongs to the parent and its siblings"
            }
            ClearVerdict::KeptJournalNotEmpty => {
                "FromStart with journalled work: a crash before the state row was written"
            }
            ClearVerdict::KeptJournalUnknown => {
                "the step journal could not be read: unknown is not empty, refusing to delete"
            }
        }
    }

    /// Did this verdict delete anything?
    pub fn cleared(&self) -> bool {
        matches!(self, ClearVerdict::Clear)
    }
}

/// Pure guard for the fresh-run checkpoint wipe.
///
/// `checkpoints` is invoked lazily and only when the cheap structural checks
/// have not already answered, so a resume never pays for a read it will ignore.
///
/// It is FALLIBLE on purpose. This guard is the last thing standing between a
/// transient database hiccup and an irreversible `clear_all_checkpoints`, and
/// an infallible `Fn() -> Vec<_>` structurally cannot say "I could not find
/// out" — every failure would arrive here indistinguishable from an empty
/// journal, and empty is the one answer that authorises the delete.
pub fn fresh_run_clear_verdict(
    execution_id: &str,
    resume_point: &ResumePoint,
    checkpoints: &dyn Fn() -> Result<Vec<StepCheckpoint>, String>,
) -> ClearVerdict {
    if !resume_point.is_fresh_start() {
        return ClearVerdict::KeptNotAFreshStart;
    }

    // A composed-run child ALWAYS resolves to `FromStart` (it never reads the
    // shared state row, and `completed_step` refuses its checkpoints), so this
    // arm is on the hot path, not a corner case.
    if is_sequence_child(execution_id) {
        return ClearVerdict::KeptComposedRunChild;
    }

    match checkpoints() {
        Ok(cps) if cps.is_empty() => ClearVerdict::Clear,
        Ok(_) => ClearVerdict::KeptJournalNotEmpty,
        Err(_) => ClearVerdict::KeptJournalUnknown,
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

        // Composed-run children (`composed-run-<ts>-workflow-<n>`) do NOT own a
        // workflow_execution_state row: `task_state.rs::persist_workflow_state`
        // persists every child's state under `get_parent_task_id(execution_id)`,
        // so all children of one composed run SHARE a single row and the last
        // writer wins. Reading it would resume the WRONG workflow, so children
        // never read it — not here, and not in `decide_resume_point`, which
        // re-checks so that a future refactor of this guard cannot silently
        // reintroduce the cross-workflow read.
        let state = if is_sequence_child(execution_id) {
            None
        } else {
            self.load_state_record(execution_id)
        };

        Self::decide_resume_point(
            execution_id,
            state.as_ref(),
            &|| self.load_checkpoints(execution_id),
            &|| self.legacy_resume_point(execution_id),
        )
    }

    /// Pure resume decision: pick the source, then derive the point.
    ///
    /// Split out from `determine_resume_point` so the decision is testable
    /// without a live PostgreSQL: `checkpoints` and `legacy` are the only I/O,
    /// and both are supplied by the caller (and invoked lazily).
    fn decide_resume_point(
        execution_id: &str,
        state: Option<&WorkflowExecutionStateRecord>,
        checkpoints: &dyn Fn() -> Vec<StepCheckpoint>,
        legacy: &dyn Fn() -> Result<ResumePoint, String>,
    ) -> Result<ResumePoint, String> {
        // See `determine_resume_point`: a child's state row belongs to its
        // siblings just as much as to it, so it is never admissible evidence
        // for THIS child and we fall back to checkpoints.
        //
        // Note what that buys today, precisely: `CheckpointManager::save_step`
        // remaps a child's checkpoints onto the PARENT id (the `task_runs` FK
        // requires it) and `StepCheckpoint` carries no column saying which
        // child wrote a row. So a read by the child's id returns EMPTY and a
        // child resolves to `FromStart` — safe (it can never resume as a
        // sibling), and identical to the pre-existing behaviour, but NOT
        // precise: composed-run children still re-execute and re-bill.
        //
        // This branch is therefore the correct shape waiting on the write
        // path, not a live fix. The moment checkpoints gain a child-keyed
        // identity, it starts returning precise child resume points with no
        // change here.
        if is_sequence_child(execution_id) {
            debug!(
                execution_id = %execution_id,
                "Composed-run child: deriving resume point from checkpoints only"
            );
            return Ok(Self::resume_point_from_checkpoints(&checkpoints()));
        }

        match state {
            Some(record) if record.workflow_type != UNIFIED_WORKFLOW_TYPE => {
                warn!(
                    execution_id = %execution_id,
                    workflow_type = %record.workflow_type,
                    "Workflow type mismatch on state row, using legacy heuristic"
                );
                legacy()
            }
            Some(record) => Self::resume_point_from_state(execution_id, record, checkpoints),
            None => {
                debug!(
                    execution_id = %execution_id,
                    "No explicit workflow state found, using legacy heuristic"
                );
                legacy()
            }
        }
    }

    /// Derive a resume point purely from step checkpoints.
    ///
    /// Used for composed-run children, whose workflow state row is shared with
    /// their siblings. Checkpoints carry phase, iteration and stage on every
    /// row, which is everything the resume point needs.
    fn resume_point_from_checkpoints(checkpoints: &[StepCheckpoint]) -> ResumePoint {
        let newest = match newest_completed(checkpoints) {
            Some(cp) => cp,
            None => return ResumePoint::FromStart,
        };

        let stage_index = normalize_stage(newest.stage_index);
        let iteration = newest.iteration.unwrap_or(1);
        let from_step = count_completed_in(
            checkpoints,
            &newest.phase,
            newest.iteration,
            newest.stage_index,
        );

        match newest.phase.as_str() {
            "setup" => ResumePoint::SetupPhase {
                from_step,
                stage_index,
            },
            "verification" => ResumePoint::VerificationPhase {
                iteration,
                from_step,
                stage_index,
            },
            "agentic" => ResumePoint::AgenticPhase {
                iteration,
                stage_index,
            },
            "completion" => ResumePoint::CompletionPhase { from_step },
            other => {
                warn!(
                    phase = %other,
                    "Unknown checkpoint phase, starting from beginning"
                );
                ResumePoint::FromStart
            }
        }
    }

    /// Determine resume point from explicit workflow state.
    fn resume_point_from_state(
        execution_id: &str,
        record: &WorkflowExecutionStateRecord,
        checkpoints: &dyn Fn() -> Vec<StepCheckpoint>,
    ) -> Result<ResumePoint, String> {
        let state_name = record.state_name.as_str();
        let iteration = record.iteration;
        let state_data = record.state_data.as_deref();

        // Checkpoints are read at most once, and only when a branch needs them.
        let mut loaded: Option<Vec<StepCheckpoint>> = None;

        // stage_index comes from state_data when the persisted variant carries
        // one; otherwise from the newest completed checkpoint, whose
        // stage_index column is populated on EVERY row. Without that fallback
        // `loop_controller`'s `start_from_stage` can never resolve non-zero.
        let stage_index = match stage_index_from_state_data(state_data) {
            Some(si) => Some(si),
            None => {
                let cps = loaded.get_or_insert_with(|| checkpoints());
                stage_index_from_checkpoints(cps)
            }
        };

        let mut completed_steps = |phase: &str, iter: Option<u32>| -> usize {
            let cps = loaded.get_or_insert_with(|| checkpoints());
            let count = count_completed_in(cps, phase, iter, stage_index);
            debug!(
                execution_id = %execution_id,
                phase = %phase,
                iteration = ?iter,
                stage_index = ?stage_index,
                completed_steps = %count,
                "Counted completed steps"
            );
            count
        };

        match state_name {
            "created" => {
                // Not started yet
                Ok(ResumePoint::FromStart)
            }

            "setup_running" => {
                // Was in setup phase.
                //
                // `Some(0)`, NOT `None`: every setup write site stamps
                // `iteration = Some(0)` on purpose (the upsert's conflict
                // target is a unique index, and `NULL != NULL` would defeat
                // it), and the read path maps the column with
                // `iter_raw.map(|i| i as u32)`, so a stored 0 reads back as
                // `Some(0)`. `count_completed_in` compares `cp.iteration ==
                // iteration` exactly, so passing `None` matches ZERO real rows
                // and the count is structurally always 0.
                //
                // Nothing consumes `from_step` for setup yet
                // (`loop_controller` reads only `stage_index`/`iteration`), so
                // this value is inert today — which is exactly why it has to be
                // right now, before something wires it up on top of a count
                // that can never be non-zero.
                let completed_count = completed_steps("setup", Some(0));
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
                let completed_count = completed_steps("verification", Some(iter));
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
                // `Some(0)` for the same reason as `setup_running` above: the
                // completion phase also writes `iteration = Some(0)` at every
                // site, so `None` here would match no row that a writer ever
                // produces. Also inert today, and fixed for the same reason.
                let completed_count = completed_steps("completion", Some(0));
                Ok(ResumePoint::CompletionPhase {
                    from_step: completed_count,
                })
            }

            "approval_pending" => {
                // Was waiting for human approval — re-present the approval gate.
                let iter = iteration.unwrap_or(1);
                let approval_id = approval_id_from_state_data(state_data);
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

    /// Read the persisted workflow execution state row (sync).
    ///
    /// Replicates `StateMachine::restore`'s `block_in_place` + `Handle::try_current`
    /// + `catch_unwind` shape (`workflow_state/mod.rs`) rather than calling it:
    /// `restore` hard-errors on a missing or undeserializable `state_data` and on a
    /// workflow-type mismatch, and any of those would demote a perfectly resumable
    /// row to the legacy heuristic — which restarts the run at verification
    /// iteration 1, step 0, re-executing and re-billing everything after it.
    /// Here every failure degrades softly to `None` (→ legacy), and the type check
    /// is applied in `decide_resume_point` as a warning instead.
    ///
    /// (Until `legacy_resume_point` was given the same `block_in_place` wrapper,
    /// that demotion was even blunter: the heuristic could not run at all from a
    /// worker thread, so every demotion landed on `FromStart`.)
    fn load_state_record(&self, execution_id: &str) -> Option<WorkflowExecutionStateRecord> {
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => {
                warn!(
                    execution_id = %execution_id,
                    "No tokio runtime available to read workflow execution state"
                );
                return None;
            }
        };

        let pg = self.pg_db.clone();
        let eid = execution_id.to_string();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::task::block_in_place(|| {
                handle.block_on(async move { pg.get_workflow_execution_state(&eid).await })
            })
        }));

        match result {
            Ok(Ok(record)) => record,
            Ok(Err(e)) => {
                warn!(
                    execution_id = %execution_id,
                    error = %e,
                    "Failed to read workflow execution state, falling back to legacy heuristic"
                );
                None
            }
            Err(_) => {
                warn!(
                    execution_id = %execution_id,
                    "block_in_place panicked reading workflow execution state"
                );
                None
            }
        }
    }

    /// Read every step checkpoint for an execution (sync), REPORTING failure.
    ///
    /// Same `block_in_place` shape as `load_state_record`. Every failure mode
    /// (no runtime, pool error, query error, panic) comes back as `Err`, so a
    /// caller that must distinguish "the journal is empty" from "I could not
    /// read the journal" can. [`Self::clear_checkpoints_for_fresh_run`] is that
    /// caller, because its action is an irreversible delete.
    fn load_checkpoints_fallible(&self, execution_id: &str) -> Result<Vec<StepCheckpoint>, String> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "no tokio runtime available to read step checkpoints".to_string())?;

        let pg = self.pg_db.clone();
        let eid = execution_id.to_string();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::task::block_in_place(|| {
                handle.block_on(async move { pg.get_all_workflow_step_checkpoints(&eid).await })
            })
        }))
        .unwrap_or_else(|_| Err("block_in_place panicked reading step checkpoints".to_string()))
    }

    /// Read every step checkpoint for an execution (sync), degrading softly.
    ///
    /// A read failure yields an empty slice, which resolves to "nothing
    /// completed" — the conservative direction FOR THE RESUME POINT (re-run
    /// work) rather than skipping work that never ran. Do NOT reuse this on any
    /// path whose action is a delete: there, empty is the destructive answer.
    /// See [`Self::load_checkpoints_fallible`].
    fn load_checkpoints(&self, execution_id: &str) -> Vec<StepCheckpoint> {
        match self.load_checkpoints_fallible(execution_id) {
            Ok(checkpoints) => checkpoints,
            Err(e) => {
                warn!(
                    execution_id = %execution_id,
                    error = %e,
                    "Failed to read step checkpoints, treating as none completed"
                );
                Vec::new()
            }
        }
    }

    /// Legacy resume point detection using the `sessions_count` heuristic.
    ///
    /// This is used as a fallback when no explicit workflow state is saved.
    /// It's less accurate but provides backward compatibility.
    fn legacy_resume_point(&self, execution_id: &str) -> Result<ResumePoint, String> {
        Self::legacy_point_from_read(execution_id, &|| self.load_sessions_count(execution_id))
    }

    /// Read `task_runs.sessions_count` for an execution (sync).
    ///
    /// Same `block_in_place` shape as [`Self::load_state_record`] and
    /// [`Self::load_checkpoints`], and for the same reason: a bare
    /// `Handle::block_on` PANICS when it is called from inside a runtime, and
    /// this whole path runs on a worker thread under
    /// `LoopController::run_inner`. Before that wrapper existed, the panic was
    /// caught and turned into an `Err`, so the heuristic below never ran at all
    /// in production.
    fn load_sessions_count(&self, execution_id: &str) -> Result<Option<u32>, String> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "no tokio runtime available to read the task run".to_string())?;

        let pg = self.pg_db.clone();
        let id = execution_id.to_string();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::task::block_in_place(|| {
                handle.block_on(async move { pg.get_task_run(&id).await })
            })
        }))
        .unwrap_or_else(|_| Err("block_in_place panicked reading the task run".to_string()))
        .map(|task_run| task_run.map(|task| task.sessions_count))
    }

    /// Pure decision half of the legacy heuristic: turn a `sessions_count` read
    /// into a resume point.
    ///
    /// NEVER propagates the read failure. A failed heuristic read means "we do
    /// not know where this run got to", and `FromStart` is precisely what the
    /// caller (`LoopController::run_inner`) already substitutes for an `Err`.
    /// Returning it here instead of `?`-ing removes a hard error from what is
    /// already a fallback path, and keeps the failure visible as a warning
    /// rather than as a resume-point error the caller silently rewrites.
    fn legacy_point_from_read(
        execution_id: &str,
        read: &dyn Fn() -> Result<Option<u32>, String>,
    ) -> Result<ResumePoint, String> {
        match read() {
            Ok(Some(sessions_count)) => {
                info!(
                    execution_id = %execution_id,
                    sessions_count = %sessions_count,
                    "No workflow state row: using legacy sessions_count heuristic"
                );
                Ok(Self::legacy_point_from_sessions_count(sessions_count))
            }
            Ok(None) => {
                // No task run found.
                Ok(ResumePoint::FromStart)
            }
            Err(e) => {
                warn!(
                    execution_id = %execution_id,
                    error = %e,
                    "Legacy sessions_count read failed, starting from the beginning"
                );
                Ok(ResumePoint::FromStart)
            }
        }
    }

    /// Pure half of the legacy heuristic (testable without a database).
    ///
    /// Imprecise by construction: any run that has started at least one session
    /// restarts at verification iteration 1, step 0.
    fn legacy_point_from_sessions_count(sessions_count: u32) -> ResumePoint {
        if sessions_count > 0 {
            ResumePoint::VerificationPhase {
                iteration: 1,
                from_step: 0,
                stage_index: None,
            }
        } else {
            ResumePoint::FromStart
        }
    }

    /// Clear all checkpoints for an execution (for fresh restart).
    ///
    /// UNGUARDED — this is the raw delete. Callers on the resume path must go
    /// through [`Self::clear_checkpoints_for_fresh_run`], which refuses the
    /// two cases where a wipe destroys a journal that is still load-bearing.
    pub fn clear_all_checkpoints(&self, execution_id: &str) -> Result<(), String> {
        info!(
            execution_id = %execution_id,
            "Clearing all checkpoints for fresh restart"
        );
        self.checkpoint_mgr.clear_all_checkpoints(execution_id)
    }

    /// Clear the step journal, but only when this really is a fresh run.
    ///
    /// Returns the verdict that was applied, so the caller can log it.
    /// See [`fresh_run_clear_verdict`] for the rules and why each exists.
    pub fn clear_checkpoints_for_fresh_run(
        &self,
        execution_id: &str,
        resume_point: &ResumePoint,
    ) -> ClearVerdict {
        // The FALLIBLE reader: a failed read must not look like an empty
        // journal on the one path that deletes it.
        let verdict = fresh_run_clear_verdict(execution_id, resume_point, &|| {
            self.load_checkpoints_fallible(execution_id)
        });

        if matches!(verdict, ClearVerdict::KeptJournalUnknown) {
            warn!(
                execution_id = %execution_id,
                "Could not read the step journal; refusing to clear it on an unknown read"
            );
        }

        if matches!(verdict, ClearVerdict::Clear) {
            if let Err(e) = self.clear_all_checkpoints(execution_id) {
                warn!(
                    execution_id = %execution_id,
                    error = %e,
                    "Failed to clear old checkpoints - continuing anyway"
                );
            }
        } else {
            info!(
                execution_id = %execution_id,
                verdict = %verdict.reason(),
                "Keeping the existing step journal"
            );
        }

        verdict
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
    use crate::unified_workflow_executor::get_parent_task_id;

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

    // =====================================================================
    // Resume DECISION tests
    //
    // `decide_resume_point` is the whole decision: source selection plus the
    // state → resume-point table. Its only I/O — the checkpoint read and the
    // legacy heuristic — is injected, so these run without a database.
    // =====================================================================

    const EXEC: &str = "unified-workflow-abc-1755000000";
    const CHILD: &str = "composed-run-1755000000-workflow-2";

    /// A sentinel the legacy heuristic can never produce naturally, so a test
    /// can prove the legacy arm was the one that ran.
    fn legacy_sentinel() -> ResumePoint {
        ResumePoint::AgenticPhase {
            iteration: 999,
            stage_index: None,
        }
    }

    fn legacy_ok() -> Result<ResumePoint, String> {
        Ok(legacy_sentinel())
    }

    fn no_checkpoints() -> Vec<StepCheckpoint> {
        Vec::new()
    }

    fn legacy_must_not_run() -> Result<ResumePoint, String> {
        panic!("legacy heuristic must not run for this input");
    }

    fn state_row(
        state_name: &str,
        iteration: Option<u32>,
        state_data: Option<&str>,
    ) -> WorkflowExecutionStateRecord {
        WorkflowExecutionStateRecord {
            execution_id: EXEC.to_string(),
            workflow_type: UNIFIED_WORKFLOW_TYPE.to_string(),
            state_name: state_name.to_string(),
            state_data: state_data.map(String::from),
            phase: None,
            iteration,
            updated_at: "2026-08-20T00:00:00+00:00".to_string(),
        }
    }

    fn checkpoint(
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
        step_index: usize,
        stage_index: Option<u32>,
        status: StepCheckpointStatus,
        completed_at: &str,
    ) -> StepCheckpoint {
        let mut cp = StepCheckpoint::new(
            execution_id,
            UNIFIED_WORKFLOW_TYPE,
            phase,
            iteration,
            step_index,
            "automation",
        );
        cp.stage_index = stage_index;
        cp.status = status;
        cp.completed_at = Some(completed_at.to_string());
        cp
    }

    /// Previously unreachable variant #1: SetupPhase.
    ///
    /// The fixture stamps `iteration = Some(0)` because that is what every
    /// setup write site actually writes (and what the read path hands back for
    /// a stored 0). With the `None` these rows used to carry, the fixture
    /// described a row shape no writer produces, and the assertion below passed
    /// against a count that is structurally 0 for real data.
    #[test]
    fn test_setup_running_resumes_after_completed_setup_steps() {
        let cps = vec![
            checkpoint(
                EXEC,
                "setup",
                Some(0),
                0,
                Some(0),
                StepCheckpointStatus::Success,
                "2026-08-20T00:00:01+00:00",
            ),
            checkpoint(
                EXEC,
                "setup",
                Some(0),
                1,
                Some(0),
                StepCheckpointStatus::Skipped,
                "2026-08-20T00:00:02+00:00",
            ),
            checkpoint(
                EXEC,
                "setup",
                Some(0),
                2,
                Some(0),
                StepCheckpointStatus::Failed,
                "2026-08-20T00:00:03+00:00",
            ),
        ];
        let point = ResumeManager::decide_resume_point(
            EXEC,
            Some(&state_row("setup_running", None, None)),
            &|| cps.clone(),
            &legacy_must_not_run,
        )
        .unwrap();

        match point {
            ResumePoint::SetupPhase {
                from_step,
                stage_index,
            } => {
                assert_eq!(from_step, 2, "failed step must be re-run");
                assert_eq!(stage_index, None);
            }
            other => panic!("expected SetupPhase, got {:?}", other),
        }
    }

    /// Previously unreachable variant #2: CompletionPhase.
    ///
    /// `Some(0)` for the same reason as the setup fixture above.
    #[test]
    fn test_completion_running_resumes_after_completed_completion_steps() {
        let cps = vec![checkpoint(
            EXEC,
            "completion",
            Some(0),
            0,
            Some(0),
            StepCheckpointStatus::Success,
            "2026-08-20T00:00:01+00:00",
        )];
        let point = ResumeManager::decide_resume_point(
            EXEC,
            Some(&state_row("completion_running", None, None)),
            &|| cps.clone(),
            &legacy_must_not_run,
        )
        .unwrap();

        match point {
            ResumePoint::CompletionPhase { from_step } => assert_eq!(from_step, 1),
            other => panic!("expected CompletionPhase, got {:?}", other),
        }
    }

    /// Previously unreachable variant #3: StageStart.
    #[test]
    fn test_stage_complete_resumes_from_next_stage() {
        let point = ResumeManager::decide_resume_point(
            EXEC,
            Some(&state_row(
                "stage_complete",
                None,
                Some(r#"{"type":"stage_complete","stage_index":1}"#),
            )),
            &no_checkpoints,
            &legacy_must_not_run,
        )
        .unwrap();

        match point {
            ResumePoint::StageStart { from_stage } => assert_eq!(from_stage, 2),
            other => panic!("expected StageStart, got {:?}", other),
        }
    }

    /// `ApprovalPhase` had no test at all; the variant IS reachable —
    /// `loop_handlers.rs` persists `UnifiedWorkflowState::approval_pending`.
    #[test]
    fn test_approval_pending_represents_the_approval_gate() {
        let point = ResumeManager::decide_resume_point(
            EXEC,
            Some(&state_row(
                "approval_pending",
                Some(4),
                Some(
                    r#"{"type":"approval_pending","iteration":4,"stage_index":2,"approval_id":"appr-77","prompt":"ok?"}"#,
                ),
            )),
            &no_checkpoints,
            &legacy_must_not_run,
        )
        .unwrap();

        match point {
            ResumePoint::ApprovalPhase {
                iteration,
                stage_index,
                approval_id,
            } => {
                assert_eq!(iteration, 4);
                assert_eq!(stage_index, Some(2));
                assert_eq!(approval_id, "appr-77");
            }
            other => panic!("expected ApprovalPhase, got {:?}", other),
        }
    }

    /// The defect this phase fixes: iteration 7 must not restart at iteration 1.
    #[test]
    fn test_verification_running_resumes_at_its_own_iteration_and_step() {
        let cps = vec![
            checkpoint(
                EXEC,
                "verification",
                Some(7),
                0,
                Some(0),
                StepCheckpointStatus::Success,
                "2026-08-20T00:00:01+00:00",
            ),
            checkpoint(
                EXEC,
                "verification",
                Some(7),
                1,
                Some(0),
                StepCheckpointStatus::Success,
                "2026-08-20T00:00:02+00:00",
            ),
            // A different iteration must not inflate the step count.
            checkpoint(
                EXEC,
                "verification",
                Some(6),
                3,
                Some(0),
                StepCheckpointStatus::Success,
                "2026-08-20T00:00:00+00:00",
            ),
        ];
        let point = ResumeManager::decide_resume_point(
            EXEC,
            Some(&state_row(
                "verification_running",
                Some(7),
                Some(r#"{"type":"verification_running","iteration":7}"#),
            )),
            &|| cps.clone(),
            &legacy_must_not_run,
        )
        .unwrap();

        match point {
            ResumePoint::VerificationPhase {
                iteration,
                from_step,
                ..
            } => {
                assert_eq!(iteration, 7);
                assert_eq!(from_step, 2);
            }
            other => panic!("expected VerificationPhase, got {:?}", other),
        }
    }

    #[test]
    fn test_stage_index_survives_from_state_data() {
        let point = ResumeManager::decide_resume_point(
            EXEC,
            Some(&state_row(
                "verification_running",
                Some(2),
                Some(r#"{"type":"verification_running","iteration":2,"stage_index":3}"#),
            )),
            &no_checkpoints,
            &legacy_must_not_run,
        )
        .unwrap();

        match point {
            ResumePoint::VerificationPhase {
                iteration,
                from_step,
                stage_index,
            } => {
                assert_eq!(iteration, 2);
                assert_eq!(from_step, 0);
                assert_eq!(stage_index, Some(3));
            }
            other => panic!("expected VerificationPhase, got {:?}", other),
        }
    }

    #[test]
    fn test_stage_index_recovered_from_checkpoints_when_state_data_lacks_it() {
        let cps = vec![
            checkpoint(
                EXEC,
                "verification",
                Some(2),
                0,
                Some(3),
                StepCheckpointStatus::Success,
                "2026-08-20T00:00:01+00:00",
            ),
            checkpoint(
                EXEC,
                "verification",
                Some(2),
                1,
                Some(3),
                StepCheckpointStatus::Success,
                "2026-08-20T00:00:02+00:00",
            ),
            // Earlier stage: must not be counted into stage 3's step total.
            checkpoint(
                EXEC,
                "verification",
                Some(2),
                0,
                Some(2),
                StepCheckpointStatus::Success,
                "2026-08-20T00:00:00+00:00",
            ),
        ];
        let point = ResumeManager::decide_resume_point(
            EXEC,
            Some(&state_row(
                "verification_running",
                Some(2),
                Some(r#"{"type":"verification_running","iteration":2}"#),
            )),
            &|| cps.clone(),
            &legacy_must_not_run,
        )
        .unwrap();

        match point {
            ResumePoint::VerificationPhase {
                stage_index,
                from_step,
                ..
            } => {
                assert_eq!(
                    stage_index,
                    Some(3),
                    "stage must be recovered from the newest completed checkpoint"
                );
                assert_eq!(from_step, 2);
            }
            other => panic!("expected VerificationPhase, got {:?}", other),
        }
    }

    /// Stage 0 is written as 0 whether or not the workflow is staged
    /// (`COALESCE($7, 0)`), so it must not be reported as a known stage.
    #[test]
    fn test_stage_zero_checkpoint_does_not_assert_a_stage() {
        let cps = vec![checkpoint(
            EXEC,
            "verification",
            Some(1),
            0,
            Some(0),
            StepCheckpointStatus::Success,
            "2026-08-20T00:00:01+00:00",
        )];
        assert_eq!(stage_index_from_checkpoints(&cps), None);
    }

    /// Composed-run children SHARE one state row (keyed on the parent id), so
    /// the row must be ignored entirely — even when one is handed in.
    #[test]
    fn test_composed_run_child_ignores_shared_parent_state_row() {
        assert!(is_sequence_child(CHILD), "test fixture must be a child id");

        let misleading = state_row(
            "agentic_running",
            Some(9),
            Some(r#"{"type":"agentic_running","iteration":9,"stage_index":5}"#),
        );
        let cps = vec![
            checkpoint(
                CHILD,
                "verification",
                Some(7),
                0,
                Some(0),
                StepCheckpointStatus::Success,
                "2026-08-20T00:00:01+00:00",
            ),
            checkpoint(
                CHILD,
                "verification",
                Some(7),
                1,
                Some(0),
                StepCheckpointStatus::Success,
                "2026-08-20T00:00:02+00:00",
            ),
        ];

        let point = ResumeManager::decide_resume_point(
            CHILD,
            Some(&misleading),
            &|| cps.clone(),
            &legacy_must_not_run,
        )
        .unwrap();

        match point {
            ResumePoint::VerificationPhase {
                iteration,
                from_step,
                stage_index,
            } => {
                assert_eq!(iteration, 7, "must follow the child's OWN checkpoints");
                assert_eq!(from_step, 2);
                assert_eq!(stage_index, None, "stage 5 came from a sibling's state row");
            }
            other => panic!("expected VerificationPhase, got {:?}", other),
        }
    }

    #[test]
    fn test_composed_run_child_without_checkpoints_starts_from_start() {
        let point = ResumeManager::decide_resume_point(
            CHILD,
            Some(&state_row("completion_running", None, None)),
            &no_checkpoints,
            &legacy_must_not_run,
        )
        .unwrap();
        assert!(point.is_fresh_start(), "got {:?}", point);
    }

    #[test]
    fn test_no_state_row_falls_back_to_legacy() {
        let point =
            ResumeManager::decide_resume_point(EXEC, None, &no_checkpoints, &legacy_ok).unwrap();
        assert_eq!(point.description(), legacy_sentinel().description());
    }

    #[test]
    fn test_workflow_type_mismatch_falls_back_to_legacy() {
        let mut record = state_row("verification_running", Some(5), None);
        record.workflow_type = "orchestrator".to_string();
        let point =
            ResumeManager::decide_resume_point(EXEC, Some(&record), &no_checkpoints, &legacy_ok)
                .unwrap();
        assert_eq!(point.description(), legacy_sentinel().description());
    }

    #[test]
    fn test_legacy_heuristic_decision() {
        assert!(ResumeManager::legacy_point_from_sessions_count(0).is_fresh_start());
        match ResumeManager::legacy_point_from_sessions_count(7) {
            ResumePoint::VerificationPhase {
                iteration,
                from_step,
                stage_index,
            } => {
                assert_eq!((iteration, from_step, stage_index), (1, 0, None));
            }
            other => panic!("expected VerificationPhase, got {:?}", other),
        }
    }

    /// The `sessions_count` heuristic is REACHABLE.
    ///
    /// `legacy_resume_point` used to call a bare `Handle::block_on`, which
    /// panics when it runs inside a runtime — and `determine_resume_point` is
    /// only ever called from `LoopController::run_inner`, an async fn on a
    /// worker thread. The `catch_unwind` turned that panic into `Err`, and the
    /// `?` propagated it, so `legacy_point_from_sessions_count` never ran in
    /// production and every legacy fallback landed on `FromStart`.
    ///
    /// This drives the whole chain — no state row → legacy arm →
    /// `legacy_point_from_read` → `legacy_point_from_sessions_count` — rather
    /// than calling the pure half directly, so the arm's reachability is what
    /// is pinned, not just its arithmetic.
    #[test]
    fn test_legacy_sessions_count_heuristic_is_reachable_through_the_decision() {
        let point = ResumeManager::decide_resume_point(EXEC, None, &no_checkpoints, &|| {
            ResumeManager::legacy_point_from_read(EXEC, &|| Ok(Some(3)))
        })
        .unwrap();

        match point {
            ResumePoint::VerificationPhase {
                iteration,
                from_step,
                stage_index,
            } => assert_eq!((iteration, from_step, stage_index), (1, 0, None)),
            other => panic!("expected VerificationPhase, got {:?}", other),
        }
    }

    /// A failed heuristic read is "we do not know", not an error to propagate:
    /// `FromStart` is what the caller substitutes for an `Err` anyway.
    #[test]
    fn test_legacy_read_failure_degrades_to_from_start_not_err() {
        let point =
            ResumeManager::legacy_point_from_read(EXEC, &|| Err("pool timeout".to_string()))
                .expect("a failed heuristic read must not be a hard error");
        assert!(point.is_fresh_start(), "got {:?}", point);

        // No task run row at all is also a fresh start, and also not an error.
        let point = ResumeManager::legacy_point_from_read(EXEC, &|| Ok(None)).unwrap();
        assert!(point.is_fresh_start(), "got {:?}", point);
    }

    #[test]
    fn test_agentic_complete_advances_to_next_iteration() {
        let point = ResumeManager::decide_resume_point(
            EXEC,
            Some(&state_row(
                "agentic_complete",
                Some(3),
                Some(r#"{"type":"agentic_complete","iteration":3}"#),
            )),
            &no_checkpoints,
            &legacy_must_not_run,
        )
        .unwrap();

        match point {
            ResumePoint::VerificationPhase {
                iteration,
                from_step,
                ..
            } => {
                assert_eq!(iteration, 4);
                assert_eq!(from_step, 0);
            }
            other => panic!("expected VerificationPhase, got {:?}", other),
        }
    }

    #[test]
    fn test_terminal_and_unknown_states_do_not_resume() {
        for state_name in ["completion_complete", "failed", "stopped", "who_knows"] {
            let point = ResumeManager::decide_resume_point(
                EXEC,
                Some(&state_row(state_name, None, None)),
                &no_checkpoints,
                &legacy_must_not_run,
            )
            .unwrap();
            assert!(point.is_fresh_start(), "{} → {:?}", state_name, point);
        }
    }

    #[test]
    fn test_newest_completed_checkpoint_wins() {
        let cps = vec![
            checkpoint(
                EXEC,
                "verification",
                Some(1),
                9,
                Some(0),
                StepCheckpointStatus::Success,
                "2026-08-20T00:00:01+00:00",
            ),
            checkpoint(
                EXEC,
                "agentic",
                Some(1),
                0,
                Some(0),
                StepCheckpointStatus::Success,
                "2026-08-20T00:00:05+00:00",
            ),
            // Running, not completed — never the newest.
            checkpoint(
                EXEC,
                "completion",
                None,
                0,
                Some(0),
                StepCheckpointStatus::Running,
                "2026-08-20T00:00:09+00:00",
            ),
        ];
        let newest = newest_completed(&cps).expect("a completed checkpoint exists");
        assert_eq!(newest.phase, "agentic");
    }

    // =====================================================================
    // Fresh-run journal wipe guard
    //
    // `loop_controller` wipes the step journal whenever the resume point is
    // `FromStart`. After Phase 1, `FromStart` is ALSO what a missing state row
    // resolves to — so the wipe now has two ways to destroy a journal that is
    // still needed. These pin both.
    // =====================================================================

    fn setup_checkpoint(execution_id: &str) -> Vec<StepCheckpoint> {
        vec![checkpoint(
            execution_id,
            "setup",
            Some(0),
            0,
            Some(0),
            StepCheckpointStatus::Success,
            "2026-08-20T00:00:01+00:00",
        )]
    }

    fn checkpoints_must_not_be_read() -> Result<Vec<StepCheckpoint>, String> {
        panic!("the journal must not be read once a structural check has answered");
    }

    /// The journal read SUCCEEDED and found nothing.
    fn journal_empty() -> Result<Vec<StepCheckpoint>, String> {
        Ok(Vec::new())
    }

    /// The journal read FAILED — a pool timeout is the realistic instance on
    /// this box, where `/health` has been sampled from 296ms to 10120ms under
    /// load. Neither `no_checkpoints` nor `checkpoints_must_not_be_read` can
    /// express this case: one is a successful empty read, the other never runs.
    fn journal_read_failed() -> Result<Vec<StepCheckpoint>, String> {
        Err("PG pool error: timed out waiting for a connection".to_string())
    }

    #[test]
    fn test_fresh_run_with_empty_journal_clears() {
        assert_eq!(
            fresh_run_clear_verdict(EXEC, &ResumePoint::FromStart, &journal_empty),
            ClearVerdict::Clear
        );
    }

    /// A failed READ must never authorise the delete. Before the accessor was
    /// made fallible, this exact input arrived as `Vec::new()` — identical to a
    /// genuinely empty journal — and the run wiped its own checkpoints, then
    /// re-executed and re-billed everything while logging "Fresh run - cleared
    /// old step checkpoints".
    #[test]
    fn test_unreadable_journal_is_never_cleared() {
        let verdict = fresh_run_clear_verdict(EXEC, &ResumePoint::FromStart, &journal_read_failed);
        assert_eq!(verdict, ClearVerdict::KeptJournalUnknown);
        assert!(!verdict.cleared(), "an unknown read must not delete");
    }

    #[test]
    fn test_resuming_never_clears_and_never_reads_the_journal() {
        for point in [
            ResumePoint::SetupPhase {
                from_step: 2,
                stage_index: None,
            },
            ResumePoint::VerificationPhase {
                iteration: 3,
                from_step: 1,
                stage_index: Some(1),
            },
            ResumePoint::AgenticPhase {
                iteration: 3,
                stage_index: None,
            },
            ResumePoint::CompletionPhase { from_step: 0 },
            ResumePoint::StageStart { from_stage: 2 },
        ] {
            assert_eq!(
                fresh_run_clear_verdict(EXEC, &point, &checkpoints_must_not_be_read),
                ClearVerdict::KeptNotAFreshStart,
                "{:?}",
                point
            );
        }
    }

    /// The mid-setup crash: checkpoints written, state row never persisted, so
    /// Phase 1 resolves `FromStart`. Wiping here is exactly the
    /// re-execute-and-re-bill defect.
    #[test]
    fn test_from_start_with_journalled_work_is_not_wiped() {
        let cps = setup_checkpoint(EXEC);
        assert_eq!(
            fresh_run_clear_verdict(EXEC, &ResumePoint::FromStart, &|| Ok(cps.clone())),
            ClearVerdict::KeptJournalNotEmpty
        );
    }

    /// A composed-run child always resolves to `FromStart`: it never reads the
    /// shared state row, and its own id has no checkpoint rows to read either.
    /// Writes remap to `get_parent_task_id(..)` but reads and deletes do NOT,
    /// so an unguarded wipe is a no-op today. What this pins is that the
    /// verdict is reached from the ID SHAPE ALONE, before the journal is
    /// consulted — so making the delete path symmetric with the write path
    /// (the natural repair) cannot turn a child's fresh start into a wipe of
    /// the parent's journal and every sibling's.
    #[test]
    fn test_composed_run_child_from_start_never_wipes_the_parent_journal() {
        let parent = get_parent_task_id(CHILD);
        assert_ne!(parent, CHILD, "CHILD must actually be a composed-run child");

        // Sibling `workflow-1` already journalled work under the parent id.
        let parent_journal = setup_checkpoint(&parent);
        assert!(!parent_journal.is_empty());

        // The child's own id has no rows at all, so an unguarded "is the
        // journal empty?" test would answer YES and clear.
        assert_eq!(
            fresh_run_clear_verdict(
                CHILD,
                &ResumePoint::FromStart,
                &checkpoints_must_not_be_read
            ),
            ClearVerdict::KeptComposedRunChild
        );
    }

    #[test]
    fn test_clear_verdict_reports_whether_it_deleted() {
        assert!(ClearVerdict::Clear.cleared());
        assert!(!ClearVerdict::KeptNotAFreshStart.cleared());
        assert!(!ClearVerdict::KeptComposedRunChild.cleared());
        assert!(!ClearVerdict::KeptJournalNotEmpty.cleared());
        assert!(!ClearVerdict::KeptJournalUnknown.cleared());
        for v in [
            ClearVerdict::Clear,
            ClearVerdict::KeptNotAFreshStart,
            ClearVerdict::KeptComposedRunChild,
            ClearVerdict::KeptJournalNotEmpty,
            ClearVerdict::KeptJournalUnknown,
        ] {
            assert!(!v.reason().is_empty());
        }
    }
}
