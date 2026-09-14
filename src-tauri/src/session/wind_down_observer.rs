//! Wind-down observation — the ONE place that turns live runner state into
//! [`qontinui_runner_lib::wind_down::eligibility`] inputs and attaches the
//! verdict to a census pass (plan `2026-09-13-drained-runner-never-reaches-idle`).
//!
//! Two callers, one body:
//!
//! - `GET /restart-readiness` (Phase 1) calls [`observe_and_apply`] on its
//!   fresh pass and reports the result read-only;
//! - the Phase 4 wind-down tick calls the same function on its own pass, and is
//!   also what keeps each pane's grid-idle window advancing between readiness
//!   requests (a window only extends on an observation — see
//!   `wind_down::GridIdleTracker`).
//!
//! **Synchronous and sleep-free.** Each pane is observed with ONE grid snapshot
//! bracketed by two reads of its grid generation (`TerminalSession::
//! observe_grid_idle`); a generation that moved during the read counts as busy,
//! and the tracker extends a window only across observations with an unchanged
//! generation. That continuity check replaces a quiescence debounce, so
//! `/restart-readiness` — polled on every Stop turn by `wip-custody-record.sh`
//! under a 2 s client timeout — pays microseconds per pane, not a sleep.
//!
//! Nothing here closes anything. The pure rules live in the lib crate; this
//! module only gathers inputs:
//!
//! - **work axis** — the per-process `session_status` the pass already carries.
//!   The caller must have run `tracking_health::compute` with a FRESHLY fetched
//!   status map; the 600 s background census passes an empty one, which would
//!   make every terminal session `Unknown`;
//! - **finished_at** — when coord's row became `finished`, from the same fetch
//!   (`StatusFetch::finished_at_by_session_id`); absent on a coord that does not
//!   serve it, in which case the declaration does not bound the idle window;
//! - **sideband** — the pane's runner-local last OSC 9999 state;
//! - **grid** — the pane's tracked idle observation;
//! - **children** — `has_live_children` from the pass's own snapshot;
//! - **kind** — steward registry, then looping-agent registry, else terminal.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use crate::session::session_lifecycle_store::TerminalSessionRecord;
use crate::session::tracking_health::{LiveClaudeProcess, SessionWorkStatus, TrackingHealthPass};
use crate::terminal::agent_status_sideband::ObservedAgentState;
use crate::terminal::TerminalManager;
use qontinui_runner_lib::wind_down::{
    self, EligibilityInputs, GridIdle, SessionKind, Sideband, SidebandState, WindDownView,
    WorkStatus,
};

/// Observe every top-level terminal-hosted process in `pass` and attach its
/// wind-down verdict (`LiveClaudeProcess::wind_down`). Nested subagents get
/// `None`. The verdict's clock is read AFTER the observations, so an
/// observation's `since` is never later than the `now` it is judged at.
pub fn observe_and_apply(
    app: &tauri::AppHandle,
    pass: &mut TrackingHealthPass,
    finished_at_by_session: &HashMap<String, i64>,
    grace: Duration,
) {
    use tauri::Manager;

    let terminal_by_session = terminal_ids_by_session(&pass.open_records);
    let wanted: HashSet<String> = pass
        .report
        .terminal_hosted
        .iter()
        .filter(|proc| !proc.nested_under_claude)
        .filter_map(|proc| proc.session_id.as_ref())
        .filter_map(|sid| terminal_by_session.get(sid).cloned())
        .collect();
    let manager = app
        .try_state::<Arc<TerminalManager>>()
        .map(|s| s.inner().clone());
    let observations = observe_terminals(manager.as_deref(), wanted);
    let now_ms = chrono::Utc::now().timestamp_millis();
    apply_wind_down(
        &mut pass.report.terminal_hosted,
        &terminal_by_session,
        &observations,
        finished_at_by_session,
        &crate::mcp::steward::steward_terminal_ids(),
        &looping_terminal_ids(app),
        grace,
        now_ms,
    );
}

/// What one terminal pane showed wind-down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalObservation {
    pub sideband: Sideband,
    pub grid: GridIdle,
}

impl TerminalObservation {
    /// No live pane to look at: every pane-derived input is unknown.
    pub const UNOBSERVABLE: Self = Self {
        sideband: Sideband::Unreadable,
        grid: GridIdle::Unknown,
    };
}

/// The coord work axis as eligibility reads it — from the per-process status
/// the caller's FRESH work-status read resolved (never the background census's
/// empty map). Absent, unset, unrecognised and ambiguous are all `Unknown`.
pub fn work_status_for(process: &LiveClaudeProcess) -> WorkStatus {
    match process
        .session_status
        .as_deref()
        .map(SessionWorkStatus::parse)
    {
        Some(SessionWorkStatus::Finished) => WorkStatus::Finished,
        Some(SessionWorkStatus::Unrecognised(_)) | None => WorkStatus::Unknown,
        Some(_) => WorkStatus::NotFinished,
    }
}

/// A pane's last-state slot as eligibility reads it.
pub fn sideband_from(read: Result<Option<ObservedAgentState>, String>) -> Sideband {
    match read {
        Ok(None) => Sideband::NeverReported,
        Ok(Some(observed)) => Sideband::Reported {
            state: if observed.is_working() {
                SidebandState::Working
            } else {
                SidebandState::NotWorking
            },
            set_at_ms: observed.set_at_ms,
        },
        Err(_) => Sideband::Unreadable,
    }
}

/// Steward first, then looping agent, else an ordinary terminal session — the
/// strictest kind, so an unresolvable registry never relaxes the `finished`
/// requirement.
pub fn session_kind_for(
    terminal_id: &str,
    steward_terminal_ids: &HashSet<String>,
    looping_terminal_ids: &HashSet<String>,
) -> SessionKind {
    if steward_terminal_ids.contains(terminal_id) {
        SessionKind::Steward
    } else if looping_terminal_ids.contains(terminal_id) {
        SessionKind::Looping
    } else {
        SessionKind::Terminal
    }
}

/// `claude_session_id` → `terminal_id`, from the pass's open records.
pub fn terminal_ids_by_session(open_records: &[TerminalSessionRecord]) -> HashMap<String, String> {
    open_records
        .iter()
        .map(|r| (r.claude_session_id.clone(), r.terminal_id.clone()))
        .collect()
}

/// The wind-down view for one process, or `None` for a nested subagent (a
/// graceful exit targets the pane's top-level `claude`; a nested one leaves
/// with it).
#[allow(clippy::too_many_arguments)]
pub fn wind_down_for_process(
    process: &LiveClaudeProcess,
    terminal_by_session: &HashMap<String, String>,
    observations: &HashMap<String, TerminalObservation>,
    finished_at_by_session: &HashMap<String, i64>,
    steward_terminal_ids: &HashSet<String>,
    looping_terminal_ids: &HashSet<String>,
    grace: Duration,
    now_ms: i64,
) -> Option<WindDownView> {
    if process.nested_under_claude {
        return None;
    }
    let terminal_id = process
        .session_id
        .as_ref()
        .and_then(|sid| terminal_by_session.get(sid));
    let kind = terminal_id.map_or(SessionKind::Terminal, |tid| {
        session_kind_for(tid, steward_terminal_ids, looping_terminal_ids)
    });
    let observation = terminal_id
        .and_then(|tid| observations.get(tid))
        .copied()
        .unwrap_or(TerminalObservation::UNOBSERVABLE);
    let work_status = work_status_for(process);
    let finished_at_ms = match work_status {
        WorkStatus::Finished => process
            .session_id
            .as_ref()
            .and_then(|sid| finished_at_by_session.get(sid))
            .copied(),
        _ => None,
    };
    let inputs = EligibilityInputs {
        kind,
        work_status,
        sideband: observation.sideband,
        grid: observation.grid,
        // The pass read this from a snapshot it successfully took.
        has_live_children: Some(process.has_live_children),
        finished_at_ms,
        grace,
    };
    Some(WindDownView::from_verdict(
        &wind_down::eligibility(&inputs, now_ms),
        kind,
    ))
}

/// Attach [`wind_down_for_process`] to every process in `processes`.
#[allow(clippy::too_many_arguments)]
pub fn apply_wind_down(
    processes: &mut [LiveClaudeProcess],
    terminal_by_session: &HashMap<String, String>,
    observations: &HashMap<String, TerminalObservation>,
    finished_at_by_session: &HashMap<String, i64>,
    steward_terminal_ids: &HashSet<String>,
    looping_terminal_ids: &HashSet<String>,
    grace: Duration,
    now_ms: i64,
) {
    for process in processes.iter_mut() {
        process.wind_down = wind_down_for_process(
            process,
            terminal_by_session,
            observations,
            finished_at_by_session,
            steward_terminal_ids,
            looping_terminal_ids,
            grace,
            now_ms,
        );
    }
}

/// Observe every named pane: its last sideband state and one tracked grid-idle
/// snapshot. A pane that no longer exists, or no `TerminalManager` at all, is
/// [`TerminalObservation::UNOBSERVABLE`]. No sleep, no await.
pub fn observe_terminals(
    manager: Option<&TerminalManager>,
    terminal_ids: HashSet<String>,
) -> HashMap<String, TerminalObservation> {
    terminal_ids
        .into_iter()
        .map(|terminal_id| {
            let observation = match manager.and_then(|m| m.get(&terminal_id)) {
                Some(session) => TerminalObservation {
                    sideband: sideband_from(session.last_agent_status()),
                    grid: session.observe_grid_idle(),
                },
                None => TerminalObservation::UNOBSERVABLE,
            };
            (terminal_id, observation)
        })
        .collect()
}

/// Terminal ids the looping-agent registry currently attributes to an agent.
/// An unresolvable registry is an empty set (see [`session_kind_for`]).
pub fn looping_terminal_ids(app: &tauri::AppHandle) -> HashSet<String> {
    use tauri::Manager;

    app.try_state::<Arc<qontinui_runner_lib::looping_agent::registry::LoopingAgentRegistry>>()
        .map(|registry| {
            registry
                .list()
                .into_iter()
                .filter_map(|rec| rec.runtime.terminal_id)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(session_id: Option<&str>, status: Option<&str>) -> LiveClaudeProcess {
        LiveClaudeProcess {
            pid: 1,
            parent_pid: None,
            image: Some("claude".to_string()),
            age_s: Some(60),
            cwd: None,
            has_live_children: false,
            nested_under_claude: false,
            session_id: session_id.map(str::to_string),
            session_status: status.map(str::to_string),
            blocks_restart: status != Some("finished"),
            wind_down: None,
        }
    }

    #[test]
    fn wind_down_work_status_mapping() {
        let p = |s: Option<&str>| process(Some("s"), s);
        assert_eq!(work_status_for(&p(Some("finished"))), WorkStatus::Finished);
        assert_eq!(work_status_for(&p(Some("done"))), WorkStatus::Finished);
        for word in ["working", "blocked", "stalled", "waiting_human"] {
            assert_eq!(work_status_for(&p(Some(word))), WorkStatus::NotFinished);
        }
        assert_eq!(work_status_for(&p(Some("vibing"))), WorkStatus::Unknown);
        assert_eq!(work_status_for(&p(None)), WorkStatus::Unknown);
    }

    #[test]
    fn wind_down_sideband_mapping() {
        assert_eq!(sideband_from(Ok(None)), Sideband::NeverReported);
        assert_eq!(sideband_from(Err("poisoned".into())), Sideband::Unreadable);
        assert_eq!(
            sideband_from(Ok(Some(ObservedAgentState {
                state: "working".into(),
                set_at_ms: 5
            }))),
            Sideband::Reported {
                state: SidebandState::Working,
                set_at_ms: 5
            }
        );
        assert_eq!(
            sideband_from(Ok(Some(ObservedAgentState {
                state: "finished".into(),
                set_at_ms: 6
            }))),
            Sideband::Reported {
                state: SidebandState::NotWorking,
                set_at_ms: 6
            }
        );
    }

    #[test]
    fn wind_down_kind_resolution_prefers_steward_then_looping() {
        let stewards: HashSet<String> = ["t-steward".to_string()].into();
        let loops: HashSet<String> = ["t-loop".to_string(), "t-steward".to_string()].into();
        assert_eq!(
            session_kind_for("t-steward", &stewards, &loops),
            SessionKind::Steward
        );
        assert_eq!(
            session_kind_for("t-loop", &stewards, &loops),
            SessionKind::Looping
        );
        assert_eq!(
            session_kind_for("t-other", &stewards, &loops),
            SessionKind::Terminal
        );
    }

    #[test]
    fn finished_at_bounds_the_window_only_for_a_finished_row() {
        let grace = Duration::from_secs(600);
        let terminal_by_session: HashMap<String, String> =
            [("s".to_string(), "t".to_string())].into();
        let observations: HashMap<String, TerminalObservation> = [(
            "t".to_string(),
            TerminalObservation {
                sideband: Sideband::NeverReported,
                grid: GridIdle::Idle { since_ms: 1_000 },
            },
        )]
        .into();
        let finished_at: HashMap<String, i64> = [("s".to_string(), 500_000)].into();
        let view = |status: &str, now_ms: i64| {
            wind_down_for_process(
                &process(Some("s"), Some(status)),
                &terminal_by_session,
                &observations,
                &finished_at,
                &HashSet::new(),
                &HashSet::new(),
                grace,
                now_ms,
            )
            .unwrap()
        };
        let not_yet = view("finished", 1_000 + 600_000);
        assert_eq!(
            (not_yet.eligibility, not_yet.since),
            ("not_yet", Some(500_000))
        );
        assert_eq!(view("finished", 500_000 + 600_000).eligibility, "eligible");
        // The same map entry is not consulted for a row that does not read
        // finished (and that row is ineligible anyway).
        assert_eq!(view("working", i64::MAX).reason, Some("not_finished"));
    }

    #[test]
    fn observing_without_a_terminal_manager_is_unobservable() {
        let got = observe_terminals(None, ["t1".to_string()].into());
        assert_eq!(got.get("t1"), Some(&TerminalObservation::UNOBSERVABLE));
    }
}
