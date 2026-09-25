//! Durable ledger types for the Approach-D Conductor/Engine (Phase 1).
//!
//! A `/orchestrate` invocation produces exactly one [`Run`] row plus a growing
//! DAG of [`Subtask`] rows in the runner-owned `orchestration` schema. A later
//! (Phase 3) reconciler is stateless over these two tables, so every conductor
//! transition must be durable here before it is acted on.
//!
//! The two structs mirror `orchestration.runs` / `orchestration.subtasks`
//! column-for-column (see `atlas/schema.hcl` and the self-heal in
//! `database/pg/mod.rs::verify_and_provision`). CRUD lives in
//! `database/pg/orchestration.rs`.
//!
//! `Subtask::artifact` reuses the canonical [`CompletionReport`] verbatim from
//! `database/pg/completion_reports.rs` — it is NOT redefined here, so the
//! `artifact` JSONB column round-trips byte-identically to a worker-written
//! completion report.

use crate::database::pg::completion_reports::CompletionReport;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle state of a single [`Subtask`], stored as TEXT in
/// `orchestration.subtasks.state`.
///
/// Serializes `snake_case` to match the text-column convention used elsewhere
/// in this codebase (e.g. `CompletionReport` field naming). Use
/// [`SubtaskState::as_str`] / [`SubtaskState::from_str_value`] for the
/// DB round-trip rather than relying on serde for the column value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtaskState {
    /// Row persisted, not yet picked up by a worker.
    Submitted,
    /// A worker (AI session) is actively executing this subtask.
    Working,
    /// RESERVED A2A-projection state with NO v1 producer.
    ///
    /// The runner FSM has no awaiting-input edge today, so the v1 reconciler
    /// NEVER writes this variant. It exists only so the enum is forward-
    /// compatible with the A2A task-state projection (`input-required`) and so
    /// a future awaiting-input edge has a name to land on. Do not add code that
    /// emits `InputRequired` until that producer exists.
    InputRequired,
    /// Worker finished successfully; `artifact` holds the `CompletionReport`.
    Completed,
    /// Worker failed terminally.
    Failed,
    /// Run was canceled (or this subtask was superseded) before completion.
    Canceled,
}

impl SubtaskState {
    /// Canonical TEXT representation for the `state` column. Matches the serde
    /// `snake_case` rename so a value written via `as_str` deserializes
    /// identically through serde and vice-versa.
    pub fn as_str(self) -> &'static str {
        match self {
            SubtaskState::Submitted => "submitted",
            SubtaskState::Working => "working",
            SubtaskState::InputRequired => "input_required",
            SubtaskState::Completed => "completed",
            SubtaskState::Failed => "failed",
            SubtaskState::Canceled => "canceled",
        }
    }

    /// Parse the TEXT representation back into the enum. Returns `None` on
    /// unknown values (forward-compat — a future state value loaded by an older
    /// runner shouldn't crash the read path).
    pub fn from_str_value(s: &str) -> Option<Self> {
        Some(match s {
            "submitted" => SubtaskState::Submitted,
            "working" => SubtaskState::Working,
            "input_required" => SubtaskState::InputRequired,
            "completed" => SubtaskState::Completed,
            "failed" => SubtaskState::Failed,
            "canceled" => SubtaskState::Canceled,
            _ => return None,
        })
    }
}

/// One row of `orchestration.runs` — a single `/orchestrate` invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub run_id: Uuid,
    pub goal: String,
    /// Named recipe driving phase composition, if any.
    pub recipe: Option<String>,
    /// Ordered phase names for this run (e.g. `["plan", "implement", "test"]`).
    /// (`plan` is the phase that runs the DESIGN step which emits the org-chart;
    /// `design` is the step name, not a phase name — contract §3.)
    pub phases: Vec<String>,
    /// Lifecycle status of the run as the conductor last wrote it:
    /// `running` | `complete` | `failed` | `stalled` | `stopped`. Every
    /// terminal exit of the reconciler writes this column — a run whose
    /// reconciler is gone reads its true outcome from here, not from the
    /// in-memory loop phase (which dies with the process).
    pub status: String,
    /// Why the run left `running`: the fatal error (a DAG cycle, a DESIGN
    /// failure), the stall pattern, or the stop request. `None` while the run
    /// is `running` and for a `complete` run. Surfaced as `error` on the run
    /// status payload when the reconciler is no longer registered.
    pub status_reason: Option<String>,
    /// The runner instance that started this run (`QONTINUI_INSTANCE_NAME`,
    /// or `primary` when unset). `None` for a row that predates the column.
    /// The boot sweep relaunches only `running` rows this instance owns.
    pub owner_instance: Option<String>,
    /// The run's `OrchestrationRunConfig` as serialized at create, so a
    /// relaunch after a restart runs at the knobs the operator started it
    /// with. `None` for a row that predates the column.
    pub config: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One row of `orchestration.subtasks` — a node in the run's subtask DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtask {
    /// Stable id within the run (the DAG node key). Unique per `run_id`.
    pub task_id: String,
    pub run_id: Uuid,
    /// Ordering hint within the run / phase.
    pub idx: i32,
    pub title: String,
    pub brief: String,
    pub phase: String,
    pub repo: Option<String>,
    /// `task_id`s within the same run that must complete before this one.
    pub depends_on: Vec<String>,
    pub expected_output: String,
    /// True when this subtask is itself an elaborator that emits child
    /// subtasks (DESIGN nodes, progressive elaboration).
    pub emits_subtasks: bool,
    pub state: SubtaskState,
    /// The AI-session / task run executing this subtask, once dispatched.
    pub task_run_id: Option<Uuid>,
    /// Worker-written completion report. `None` until the subtask completes.
    /// Round-trips byte-identically to the canonical `CompletionReport`.
    pub artifact: Option<CompletionReport>,
    /// The elaborating parent `task_id` that spliced this row in (null for
    /// DESIGN-origin rows). The idempotent splice key for Phase 4.
    pub produced_by: Option<String>,
    /// Phase 6: the coord gate id that gates this subtask's dispatch, once
    /// registered. `None` for a subtask with no observable external pre-condition
    /// (the Phase-3 dispatch-normally path). A `Submitted` subtask with `gate_id`
    /// set and `gate_status` not yet `cleared` is **blocked-on-gate** — the
    /// reconciler treats it as not-dispatchable. This column is the DURABLE record
    /// a restart re-attaches to (it resumes polling, never re-registers).
    pub gate_id: Option<String>,
    /// Phase 6: the last-polled coord gate verdict (`open` | `cleared` |
    /// `failed`), or one of the two runner-side typed block tokens when the
    /// runner has no coord ANSWER about this row —
    /// [`GATE_STATUS_COORD_UNREACHABLE`](super::coord_gate::GATE_STATUS_COORD_UNREACHABLE)
    /// (`coord_unreachable`: it could not ask — no device credential, a dead
    /// transport, a 401/408/429/5xx) and
    /// [`GATE_STATUS_COORD_ERROR`](super::coord_gate::GATE_STATUS_COORD_ERROR)
    /// (`coord_error`: coord answered and refused the call, or sent a verdict
    /// this build does not understand). `None` until the gate is first polled.
    /// `cleared` ⇒ unblock + dispatch; `failed` ⇒ the subtask is failed (coord's
    /// `withdrawn` / `misconfigured` verdicts land here too — a gate that will
    /// never clear); either coord-block token ⇒ the call is retried every tick
    /// and the row COUNTS toward the run's stall fingerprint (it is not
    /// legitimately waiting on anything coord said). The full token set is
    /// `open | cleared | failed | coord_unreachable | coord_error`; the column
    /// is plain nullable `text` with no CHECK, so this doc and the matching
    /// comment in `atlas/schema.hcl` are the only enumeration there is.
    pub gate_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// How many times the boot sweep may put one row back to `submitted` after its
/// worker died with the runner process. The reset that would exceed this fails
/// the row instead.
pub const MAX_RESTART_RESETS: i32 = 2;

/// A `working` row as the boot sweep reads it
/// (`PgDb::list_working_rows_for_sweep`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingRow {
    pub task_id: String,
    pub task_run_id: Option<Uuid>,
    /// A `CompletionReport` already landed in `artifact`.
    pub has_artifact: bool,
    /// Resets this row has already had (`subtasks.restart_resets`).
    pub restart_resets: i32,
}

/// What the boot sweep does with a `working` row whose worker is not live in
/// this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LostWorkerDisposition {
    /// The report landed; only the idle signal was lost. The row is left
    /// `working` and NOT written: the relaunched reconciler completes a `Gone`
    /// worker with an artifact through its normal path
    /// (`conductor::compute_tick`), which still reads a drift-verify row's
    /// verdict and harvests an elaborator before completing it. Writing
    /// `completed` here would skip both.
    Reported,
    /// No report → back to `submitted`, `task_run_id` cleared, counted.
    Resubmit,
    /// No report and the reset budget is spent → `failed`.
    Fail,
}

impl LostWorkerDisposition {
    /// The reason written to the log for a [`Self::Fail`], stated once.
    pub const FAIL_REASON: &'static str = "worker lost across 2 restarts";

    /// Decide one lost row. A landed report always wins, whatever the count:
    /// the work is done, and re-running it would duplicate it.
    pub fn decide(row: &WorkingRow) -> Self {
        if row.has_artifact {
            Self::Reported
        } else if row.restart_resets >= MAX_RESTART_RESETS {
            Self::Fail
        } else {
            Self::Resubmit
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn working(has_artifact: bool, restart_resets: i32) -> WorkingRow {
        WorkingRow {
            task_id: "t".to_string(),
            task_run_id: Some(Uuid::nil()),
            has_artifact,
            restart_resets,
        }
    }

    #[test]
    fn a_lost_worker_with_a_report_is_left_to_the_reconciler_whatever_its_count() {
        for n in [0, 1, 2, 5] {
            assert_eq!(
                LostWorkerDisposition::decide(&working(true, n)),
                LostWorkerDisposition::Reported
            );
        }
    }

    #[test]
    fn a_lost_worker_without_a_report_is_resubmitted_twice_then_failed() {
        assert_eq!(
            LostWorkerDisposition::decide(&working(false, 0)),
            LostWorkerDisposition::Resubmit
        );
        assert_eq!(
            LostWorkerDisposition::decide(&working(false, 1)),
            LostWorkerDisposition::Resubmit
        );
        // The third loss would be reset number 3 — over the budget of 2.
        assert_eq!(
            LostWorkerDisposition::decide(&working(false, MAX_RESTART_RESETS)),
            LostWorkerDisposition::Fail
        );
    }

    #[test]
    fn subtask_state_round_trips_via_text() {
        for st in [
            SubtaskState::Submitted,
            SubtaskState::Working,
            SubtaskState::InputRequired,
            SubtaskState::Completed,
            SubtaskState::Failed,
            SubtaskState::Canceled,
        ] {
            let s = st.as_str();
            assert_eq!(SubtaskState::from_str_value(s), Some(st));
            // serde snake_case must agree with as_str so the column value is
            // interchangeable between the two paths.
            let json = serde_json::to_string(&st).unwrap();
            assert_eq!(json, format!("\"{}\"", s));
        }
        assert_eq!(SubtaskState::from_str_value("bogus_state"), None);
    }
}
