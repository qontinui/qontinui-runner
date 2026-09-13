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
//! Nothing here closes anything. The pure rules live in the lib crate; this
//! module only gathers inputs:
//!
//! - **work axis** — the per-process `session_status` the pass already carries.
//!   The caller must have run `tracking_health::compute` with a FRESHLY fetched
//!   status map; the 600 s background census passes an empty one, which would
//!   make every terminal session `Unknown`;
//! - **sideband** — the pane's runner-local last OSC 9999 state;
//! - **grid** — a debounced idle observation through the pane's tracker;
//! - **children** — `has_live_children` from the pass's own snapshot;
//! - **kind** — steward registry, then looping-agent registry, else terminal.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::session::session_lifecycle_store::TerminalSessionRecord;
use crate::session::tracking_health::{LiveClaudeProcess, SessionWorkStatus, TrackingHealthPass};
use crate::terminal::agent_status_sideband::ObservedAgentState;
use qontinui_runner_lib::wind_down::{
    self, EligibilityInputs, GridIdle, SessionKind, Sideband, SidebandState, WindDownView,
    WorkStatus,
};

/// Observe every top-level terminal-hosted process in `pass` and attach its
/// wind-down verdict (`LiveClaudeProcess::wind_down`). Nested subagents get
/// `None`. Panes are observed concurrently, so this costs about one
/// [`WIND_DOWN_IDLE_DEBOUNCE`] regardless of how many there are.
pub async fn observe_and_apply(
    app: &tauri::AppHandle,
    pass: &mut TrackingHealthPass,
    grace: std::time::Duration,
    now_ms: i64,
) {
    let terminal_by_session = terminal_ids_by_session(&pass.open_records);
    let wanted: HashSet<String> = pass
        .report
        .terminal_hosted
        .iter()
        .filter(|proc| !proc.nested_under_claude)
        .filter_map(|proc| proc.session_id.as_ref())
        .filter_map(|sid| terminal_by_session.get(sid).cloned())
        .collect();
    let observations = observe_terminals(app, wanted, now_ms).await;
    apply_wind_down(
        &mut pass.report.terminal_hosted,
        &terminal_by_session,
        &observations,
        &crate::mcp::steward::steward_terminal_ids(),
        &looping_terminal_ids(app),
        grace,
        now_ms,
    );
}

/// Idle-gate debounce for the wind-down grid observation. The session message
/// poller's value: long enough to catch a mid-output frame, short enough that
/// observing every terminal concurrently adds well under a second to this
/// endpoint.
pub const WIND_DOWN_IDLE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(600);

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
/// this request's FRESH work-status read resolved (never the background
/// census's empty map). Absent, unset, unrecognised and ambiguous are all
/// `Unknown`.
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
pub fn wind_down_for_process(
    process: &LiveClaudeProcess,
    terminal_by_session: &HashMap<String, String>,
    observations: &HashMap<String, TerminalObservation>,
    steward_terminal_ids: &HashSet<String>,
    looping_terminal_ids: &HashSet<String>,
    grace: std::time::Duration,
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
    let inputs = EligibilityInputs {
        kind,
        work_status: work_status_for(process),
        sideband: observation.sideband,
        grid: observation.grid,
        // The pass read this from a snapshot it successfully took.
        has_live_children: Some(process.has_live_children),
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
    steward_terminal_ids: &HashSet<String>,
    looping_terminal_ids: &HashSet<String>,
    grace: std::time::Duration,
    now_ms: i64,
) {
    for process in processes.iter_mut() {
        process.wind_down = wind_down_for_process(
            process,
            terminal_by_session,
            observations,
            steward_terminal_ids,
            looping_terminal_ids,
            grace,
            now_ms,
        );
    }
}

/// Observe every named pane concurrently (each observation waits one
/// [`WIND_DOWN_IDLE_DEBOUNCE`]). A pane that no longer exists, or a
/// `TerminalManager` that does not resolve, is [`TerminalObservation::UNOBSERVABLE`].
pub async fn observe_terminals(
    app: &tauri::AppHandle,
    terminal_ids: HashSet<String>,
    observed_at_ms: i64,
) -> HashMap<String, TerminalObservation> {
    use tauri::Manager;

    let Some(manager) = app
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|s| s.inner().clone())
    else {
        return HashMap::new();
    };
    let observations = terminal_ids.into_iter().map(|terminal_id| {
        let session = manager.get(&terminal_id);
        async move {
            let observation = match session {
                Some(session) => TerminalObservation {
                    sideband: sideband_from(session.last_agent_status()),
                    grid: session
                        .observe_grid_idle(WIND_DOWN_IDLE_DEBOUNCE, observed_at_ms)
                        .await,
                },
                None => TerminalObservation::UNOBSERVABLE,
            };
            (terminal_id, observation)
        }
    });
    futures::future::join_all(observations)
        .await
        .into_iter()
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
}
