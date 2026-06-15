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
    pub status: String,
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
    /// Phase 6: the last-polled coord gate verdict (`open` | `cleared` | `failed`).
    /// `None` until the gate is first polled. `cleared` ⇒ unblock + dispatch;
    /// `failed` ⇒ the subtask is failed.
    pub gate_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
