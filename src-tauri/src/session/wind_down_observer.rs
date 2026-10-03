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
//! - **children** — `has_live_children` from the pass's own snapshot, passed
//!   through as the `Option` it already is: `None` (the snapshot never
//!   enumerated that pid) must reach `eligibility` as `ChildrenUnknown` rather
//!   than being flattened into a confident "no children";
//! - **kind** — steward registry, then looping-agent registry, else terminal.
//!
//! ## Custody inputs (the `finished_close` arm only)
//!
//! The executor's `finished_close` arm additionally needs
//! [`wind_down::custody_gate`]'s inputs (plan
//! `2026-10-03-finished-runner-sessions-close-their-window-without-a-drain`,
//! D2b/D2c), and those cost git subprocesses and a coord read — so they are
//! gathered here, by [`gather_custody`], ONLY for candidates `eligibility`
//! already rated `Eligible`, never by `/restart-readiness`. The probes sit
//! behind [`CustodyProbe`] so the gather is testable without shelling out;
//! [`GitCustodyProbe`] is the shipped one, and every git call it makes is
//! bounded by [`CUSTODY_PROBE_TIMEOUT`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::agent_worktree::custody::{self, coord::CoordOwnership};
use crate::mcp::session_work_status::StatusFetch;
use crate::session::session_lifecycle_store::TerminalSessionRecord;
use crate::session::tracking_health::{LiveClaudeProcess, SessionWorkStatus, TrackingHealthPass};
use crate::terminal::agent_status_sideband::ObservedAgentState;
use crate::terminal::TerminalManager;
use qontinui_runner_lib::wind_down::{
    self, CustodyInputs, CustodyMember, EligibilityInputs, GridIdle, MemberState, SessionKind,
    Sideband, SidebandState, WindDownView, WorkStatus,
};

/// What one [`observe_and_apply`] pass looked at, kept so a caller that ACTS on
/// a verdict (the Phase 4 wind-down executor) can log the inputs the verdict
/// was reached on, and resolve a process's pane and kind. Readiness ignores it.
#[derive(Debug, Clone, Default)]
pub struct ObservedInputs {
    /// `claude_session_id` → `terminal_id`.
    pub terminal_by_session: HashMap<String, String>,
    /// `terminal_id` → what its pane showed.
    pub by_terminal: HashMap<String, TerminalObservation>,
    pub steward_terminal_ids: HashSet<String>,
    pub looping_terminal_ids: HashSet<String>,
    /// The instant every verdict in the pass was judged at.
    pub now_ms: i64,
}

impl ObservedInputs {
    /// The pane hosting `claude_session_id`, when the pass resolved one.
    pub fn terminal_for(&self, claude_session_id: &str) -> Option<&str> {
        self.terminal_by_session
            .get(claude_session_id)
            .map(String::as_str)
    }

    /// What that pane showed, or [`TerminalObservation::UNOBSERVABLE`].
    pub fn observation_for(&self, terminal_id: &str) -> TerminalObservation {
        self.by_terminal
            .get(terminal_id)
            .copied()
            .unwrap_or(TerminalObservation::UNOBSERVABLE)
    }

    /// The session kind the pass resolved for that pane.
    pub fn kind_for(&self, terminal_id: &str) -> SessionKind {
        session_kind_for(
            terminal_id,
            &self.steward_terminal_ids,
            &self.looping_terminal_ids,
        )
    }
}

/// Observe every top-level terminal-hosted process in `pass` and attach its
/// wind-down verdict (`LiveClaudeProcess::wind_down`). Nested subagents get
/// `None`. The verdict's clock is read AFTER the observations, so an
/// observation's `since` is never later than the `now` it is judged at.
pub fn observe_and_apply(
    app: &tauri::AppHandle,
    pass: &mut TrackingHealthPass,
    finished_at_by_session: &HashMap<String, i64>,
    grace: Duration,
) -> ObservedInputs {
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
    // Registry reads FIRST, pane observations second, and the clock last — so
    // `now_ms` is still read after the observations it judges (see this
    // function's doc comment). Hoisting these two above the observations, as
    // this code briefly did, pushed `now_ms` later than the observations by the
    // cost of two registry reads, which can only make an idle window look
    // longer than it was — the one direction that must never drift.
    let steward_terminal_ids = crate::mcp::steward::steward_terminal_ids();
    let looping_terminal_ids = looping_terminal_ids(app);
    let observations = observe_terminals(manager.as_deref(), wanted);
    let now_ms = chrono::Utc::now().timestamp_millis();
    apply_wind_down(
        &mut pass.report.terminal_hosted,
        &terminal_by_session,
        &observations,
        finished_at_by_session,
        &steward_terminal_ids,
        &looping_terminal_ids,
        grace,
        now_ms,
    );
    ObservedInputs {
        terminal_by_session,
        by_terminal: observations,
        steward_terminal_ids,
        looping_terminal_ids,
        now_ms,
    }
}

/// One freshly computed, freshly observed census pass — everything both
/// wind-down callers need, gathered once.
///
/// Built by [`fresh_pass`], which is the ONE place the sequence
/// "read the open ids → fetch the coord work axis in bulk → `tracking_health::
/// compute` with that FRESH map → [`observe_and_apply`]" lives. `GET
/// /restart-readiness` and the Phase 4 wind-down tick both go through it, so
/// the two can never drift into two differently-built censuses (the "second
/// census" the plan forbids).
pub struct FreshPass {
    /// `None` when the terminal plane could not be determined at all; the
    /// reason is then in `unknowns`.
    pub pass: Option<TrackingHealthPass>,
    pub status_fetch: StatusFetch,
    pub observed: ObservedInputs,
    /// Human-readable reasons the pass is incomplete, in the order they were
    /// discovered. `/restart-readiness` renders these verbatim.
    pub unknowns: Vec<String>,
}

/// Compute + observe one fresh pass. See [`FreshPass`].
pub async fn fresh_pass(app: &tauri::AppHandle, grace: Duration) -> FreshPass {
    use tauri::Manager;

    let mut unknowns: Vec<String> = Vec::new();

    // `compute` reads `store.open_records()` itself, so it cannot be handed a
    // status map unless the ids are known first. A record that APPEARS between
    // this read and `compute`'s own gets no status and therefore blocks —
    // fail-closed by construction.
    let open_ids: Option<Vec<String>> = app
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .map(|store| {
            store
                .open_records()
                .into_iter()
                .map(|r| r.claude_session_id)
                .collect()
        });
    let status_fetch: StatusFetch = match &open_ids {
        Some(ids) => crate::mcp::session_work_status::fetch(ids).await,
        // An unresolvable store is NOT "there was nothing to ask about" — it is
        // an axis that could not be consulted at all.
        None => StatusFetch::store_unavailable(),
    };

    let mut pass = 'terminal: {
        let Some(tm) = app.try_state::<Arc<TerminalManager>>() else {
            unknowns.push(
                "the terminal-session plane could not be determined: TerminalManager did not resolve"
                    .to_string(),
            );
            break 'terminal None;
        };
        let Some(store) =
            app.try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        else {
            unknowns.push(
                "the terminal-session plane could not be determined: SessionLifecycleStore did not resolve"
                    .to_string(),
            );
            break 'terminal None;
        };
        let Some(sm) = app.try_state::<Arc<crate::claude_session::SessionManager>>() else {
            unknowns.push(
                "the terminal-session plane could not be determined: SessionManager did not resolve, so the exempt AI plane cannot be subtracted"
                    .to_string(),
            );
            break 'terminal None;
        };
        // NEVER `now()` here — that reference feeds the PID-reuse guard and
        // substituting it falsely flips live idle sessions to tracked-dead.
        let Some(boot_ms) = crate::session::tracking_health::primary_boot_unix_millis() else {
            unknowns.push(
                "the terminal-session plane could not be determined: the PID-reuse guard's primary-boot reference is not initialized yet (the runner is still starting)"
                    .to_string(),
            );
            break 'terminal None;
        };

        match crate::session::tracking_health::compute(
            tm.inner(),
            store.inner(),
            sm.inner(),
            boot_ms,
            &status_fetch.by_session_id,
        )
        .await
        {
            Some(pass) => Some(pass),
            None => {
                unknowns.push(
                    "the terminal-session plane could not be determined: the process table is unreadable (snapshot_process_table_public returned an empty parent_map), so live `claude` processes cannot be enumerated"
                        .to_string(),
                );
                None
            }
        }
    };

    let observed = match pass.as_mut() {
        Some(p) => observe_and_apply(app, p, &status_fetch.finished_at_by_session_id, grace),
        None => ObservedInputs::default(),
    };

    FreshPass {
        pass,
        status_fetch,
        observed,
        unknowns,
    }
}

/// Re-derive ONE session's wind-down verdict from a brand-new pass.
///
/// The Phase 4 executor closes up to four panes per tick and each close can
/// wait a full `EXIT_DEADLINE`, so the verdict that authorised the LAST close
/// in a batch can be minutes old by the time `/exit` is typed into its pane —
/// and in those minutes an operator can have returned to that session, worked
/// in it, and left it momentarily quiet again. `exit_prompt_ready` cannot tell
/// that state from idleness; only a re-observed grace window can. This is the
/// door for that re-check.
///
/// It re-runs [`fresh_pass`] rather than re-observing the pane alone,
/// deliberately: the verdict folds FIVE inputs and a pane observation refreshes
/// only two of them. A cheaper partial re-check would leave `has_live_children`
/// and coord's work axis frozen at the tick's start, which is the very
/// staleness this exists to remove.
///
/// **What it costs, in full.** One process-table census, plus one bulk
/// work-status read — and that read is a coord HTTP call whose credential
/// resolution can block on the platform keychain. At `MAX_CLOSES_PER_TICK` of
/// four, with the first candidate exempt, that is up to three per tick, paid
/// only while the device is drained. It buys the difference between closing a
/// session on a verdict and closing one on a verdict that was true four
/// minutes ago.
///
/// `None` means the session is no longer a top-level terminal-hosted `claude`
/// this pass can see at all — which is not eligibility either.
pub async fn recheck(
    app: &tauri::AppHandle,
    grace: Duration,
    claude_session_id: &str,
    terminal_id: &str,
) -> Option<WindDownView> {
    let fresh = fresh_pass(app, grace).await;
    // The pane must still host this session: a terminal id that has been
    // rebound to another session since the tick's first pass is not the pane
    // whose verdict we are re-checking.
    if fresh.observed.terminal_for(claude_session_id) != Some(terminal_id) {
        return None;
    }
    fresh
        .pass?
        .report
        .terminal_hosted
        .iter()
        .find(|proc| {
            !proc.nested_under_claude && proc.session_id.as_deref() == Some(claude_session_id)
        })
        .and_then(|proc| proc.wind_down.clone())
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
        // Passed through, NOT re-wrapped in `Some`. A snapshot that never
        // enumerated this pid carries `None` here, and that has to survive as
        // far as `eligibility`, which turns it into `ChildrenUnknown`. Wrapping
        // it — which this line used to do — made an uncomputable input render
        // as a confident "no children" and left `ChildrenUnknown` unreachable.
        has_live_children: process.has_live_children,
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

// ---------------------------------------------------------------------------
// Custody inputs for the `finished_close` arm (D2b, D2c)
// ---------------------------------------------------------------------------

/// Bound on EACH git subprocess a custody probe runs. A probe that does not
/// answer inside it is a failed probe, and a failed probe is `Unknown`.
pub const CUSTODY_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// coord's attributed-worktree index, or why it could not be read. Fetched
/// once per arm pass and shared by every candidate in it.
pub type OwnershipRead = Result<CoordOwnership, String>;

/// Read coord's attributed-worktree index (`GET /coord/sessions/worktrees`,
/// through the existing `custody::coord::fetch_ownership`, whose client is
/// bounded at 15 s).
///
/// A runner with no coord base configured answers `Err` here, not an empty
/// index: "nothing is attributed" would be a claim about allocations this
/// runner had no way to read.
pub async fn fetch_ownership() -> OwnershipRead {
    match custody::coord::fetch_ownership().await {
        Ok(Some(ownership)) => Ok(ownership),
        Ok(None) => Err(
            "no coord base is configured, so the session's worktree allocations cannot be read"
                .to_string(),
        ),
        Err(e) => Err(e),
    }
}

/// What the runner knows about one candidate before any probe runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionCustodyContext {
    /// The lifecycle record's `working_dir`, else the process cwd (which is
    /// `None` on Windows — `tracking_health` reads it from `/proc`).
    pub session_dir: Option<PathBuf>,
    /// The `coord.sessions.id` the pane registered under — the key
    /// `/coord/sessions/worktrees` lists allocations by.
    pub coord_session_id: Option<String>,
    /// The parked isolated-edit context's materialized worktrees.
    pub isolated_worktrees: Vec<PathBuf>,
}

/// Collect [`SessionCustodyContext`] for one candidate from the pass that
/// rated it and the live pane.
pub fn custody_context(
    app: &tauri::AppHandle,
    pass: &TrackingHealthPass,
    claude_session_id: &str,
    terminal_id: &str,
) -> SessionCustodyContext {
    use tauri::Manager;

    let record_dir = pass
        .open_records
        .iter()
        .find(|r| r.claude_session_id == claude_session_id)
        .and_then(|r| r.working_dir.clone());
    let process_cwd = pass
        .report
        .terminal_hosted
        .iter()
        .find(|p| !p.nested_under_claude && p.session_id.as_deref() == Some(claude_session_id))
        .and_then(|p| p.cwd.clone());
    let session_dir = session_dir_from(record_dir.as_deref(), process_cwd.as_deref());
    let pane = app
        .try_state::<Arc<TerminalManager>>()
        .and_then(|m| m.get(terminal_id));
    SessionCustodyContext {
        session_dir,
        coord_session_id: pane
            .as_ref()
            .and_then(|s| s.coord_session_id())
            .map(|id| id.to_string()),
        isolated_worktrees: pane
            .map(|s| s.isolated_edit_worktree_paths())
            .unwrap_or_default(),
    }
}

/// PURE: the record's `working_dir`, else the process cwd. A blank value is
/// not a directory.
pub fn session_dir_from(record_dir: Option<&str>, process_cwd: Option<&str>) -> Option<PathBuf> {
    [record_dir, process_cwd]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|d| !d.is_empty())
        .map(PathBuf::from)
}

/// PURE: the worktrees coord attributes to the session, as absolute paths.
///
/// `Err` — which the gate reads as `Unknown` — when the index could not be
/// read, when the pane carries no coord session id (its allocations cannot be
/// looked up at all), or when a ledger path is relative and no workspace root
/// resolves to anchor it.
pub fn attributed_worktrees(
    ownership: &OwnershipRead,
    coord_session_id: Option<&str>,
    workspace_root: Option<&Path>,
) -> Result<Vec<PathBuf>, String> {
    let index = ownership.as_ref().map_err(String::clone)?;
    let Some(session_id) = coord_session_id else {
        return Err(
            "the pane carries no coord session id, so its worktree allocations cannot be looked up"
                .to_string(),
        );
    };
    index
        .worktree_paths_for_session(session_id)
        .into_iter()
        .map(|raw| {
            let path = PathBuf::from(&raw);
            if path.is_absolute() {
                Ok(path)
            } else {
                workspace_root.map(|root| root.join(&path)).ok_or_else(|| {
                    format!(
                        "coord's ledger path {raw} is relative and the workspace root does not resolve"
                    )
                })
            }
        })
        .collect()
}

/// The three questions a custody gather asks of the filesystem and git,
/// behind a seam so the gather is testable without either.
#[async_trait::async_trait]
pub trait CustodyProbe: Send + Sync {
    /// The root of the worktree containing `dir`. `Ok(None)` when `dir` is in
    /// no repository at all; `Err` when `dir` cannot be read.
    async fn worktree_root(&self, dir: &Path) -> Result<Option<PathBuf>, String>;
    /// Probe one worktree root (D2c).
    async fn member_state(&self, root: &Path) -> MemberState;
    /// `wip_state` of the custody SLOT this session wrote in `root`, if any.
    async fn slot_wip_state(&self, root: &Path, claude_session_id: &str) -> Option<String>;
}

/// Gather [`CustodyInputs`] for one candidate (D2b).
///
/// The set is the repo holding the session's directory, every attributed
/// worktree and every isolated-edit worktree, each resolved to its worktree
/// root and probed ONCE however many sources named it. A directory in no
/// repository contributes no member, so a session at the workspace root is
/// judged by its attributed worktrees alone.
///
/// When the verdict is already decided — no directory, or an unreadable
/// ownership read, both `Unknown` — nothing is probed: the probes could not
/// change the answer and they are the expensive part.
pub async fn gather_custody<P: CustodyProbe + ?Sized>(
    probe: &P,
    context: &SessionCustodyContext,
    attributed: Result<Vec<PathBuf>, String>,
    claude_session_id: &str,
) -> CustodyInputs {
    let session_dir = context
        .session_dir
        .as_ref()
        .map(|d| d.display().to_string());
    let ownership = attributed.as_ref().map(|_| ()).map_err(String::clone);
    let (Some(dir), Ok(attributed)) = (context.session_dir.as_ref(), attributed) else {
        return CustodyInputs {
            session_dir,
            ownership,
            members: Vec::new(),
        };
    };

    let mut members: Vec<CustodyMember> = Vec::new();
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let named = std::iter::once((dir, true))
        .chain(attributed.iter().map(|p| (p, false)))
        .chain(context.isolated_worktrees.iter().map(|p| (p, false)));
    for (path, is_session_dir) in named {
        match probe.worktree_root(path).await {
            Ok(Some(root)) => {
                if seen.insert(custody::norm(&root.to_string_lossy())) {
                    roots.push(root);
                }
            }
            // The session's own directory may legitimately sit outside every
            // repo (the workspace root). A worktree coord or the isolated-edit
            // context NAMED is supposed to be one, so not finding a repo there
            // is a failed probe.
            Ok(None) if is_session_dir => {}
            Ok(None) => members.push(CustodyMember {
                path: path.display().to_string(),
                state: MemberState::ProbeFailed("not inside a git worktree".to_string()),
                slot_wip_state: None,
            }),
            Err(e) => members.push(CustodyMember {
                path: path.display().to_string(),
                state: MemberState::ProbeFailed(e),
                slot_wip_state: None,
            }),
        }
    }
    for root in roots {
        let state = probe.member_state(&root).await;
        let slot_wip_state = probe.slot_wip_state(&root, claude_session_id).await;
        members.push(CustodyMember {
            path: root.display().to_string(),
            state,
            slot_wip_state,
        });
    }
    CustodyInputs {
        session_dir,
        ownership,
        members,
    }
}

/// The shipped [`CustodyProbe`]: a filesystem walk for the root, git for the
/// state, and the custody slot store for the cross-check.
#[derive(Debug, Clone, Copy)]
pub struct GitCustodyProbe {
    /// Bound on each git subprocess.
    pub timeout: Duration,
}

impl Default for GitCustodyProbe {
    fn default() -> Self {
        Self {
            timeout: CUSTODY_PROBE_TIMEOUT,
        }
    }
}

impl GitCustodyProbe {
    /// Run `git -C <root> <args>` under [`Self::timeout`]. `Err` only when the
    /// process could not be run or did not finish in time; a non-zero exit is
    /// returned for the caller to read, because for `config --get` it is an
    /// answer rather than a failure.
    async fn git(&self, root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
        let mut cmd = crate::process_helpers::tokio_no_window("git");
        cmd.arg("-C")
            .arg(root)
            .args(args)
            // An inherited GIT_DIR / GIT_WORK_TREE would point every probe at
            // some other repository than the one named by `-C`.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            // A probe must never touch the network: in a partial clone a
            // missing object would otherwise be lazily fetched.
            .env("GIT_NO_LAZY_FETCH", "1")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(self.timeout, cmd.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(format!("git {} could not run: {e}", args.join(" "))),
            Err(_) => Err(format!(
                "git {} did not finish within {} s",
                args.join(" "),
                self.timeout.as_secs()
            )),
        }
    }

    /// Run a git probe that must exit zero, returning its trimmed stdout.
    async fn git_ok(&self, root: &Path, args: &[&str]) -> Result<String, String> {
        let output = self.git(root, args).await?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(failed_git(args, &output))
        }
    }

    /// Commits reachable from `HEAD` that are not pushed.
    ///
    /// * **With an upstream** — `rev-list --count @{upstream}..HEAD`.
    /// * **With none, or a detached `HEAD`** — `rev-list --count HEAD --not
    ///   --remotes`: the commits on NO remote-tracking ref. `0` means every
    ///   commit at `HEAD` is already on some remote, so nothing is unpushed;
    ///   that is the shape of a freshly allocated worktree that made no
    ///   commits (it sits on `origin/main`'s commit with no upstream), the
    ///   most common finished session there is.
    ///
    /// Which arm applies is decided from exit codes, never from git's
    /// (localisable) error text. Every call is local — `rev-list` reads
    /// remote-tracking refs as this clone last saw them and fetches nothing,
    /// and [`Self::git`] sets `GIT_NO_LAZY_FETCH` so a partial clone cannot
    /// fetch a missing object behind the probe's back. `Err` only when a git
    /// call failed or timed out.
    async fn unpushed_commits(&self, root: &Path) -> Result<u64, String> {
        let args: &[&str] = if self.has_upstream(root).await? {
            &["rev-list", "--count", "@{upstream}..HEAD"]
        } else {
            &["rev-list", "--count", "HEAD", "--not", "--remotes"]
        };
        let count = self.git_ok(root, args).await?;
        count
            .parse::<u64>()
            .map_err(|e| format!("git {} printed {count:?}: {e}", args.join(" ")))
    }

    /// Does `HEAD` name a branch with a configured upstream? `false` for a
    /// detached `HEAD` (`symbolic-ref -q` exits 1) and for a branch with no
    /// `branch.<name>.merge` (`config --get` exits 1).
    async fn has_upstream(&self, root: &Path) -> Result<bool, String> {
        let head = self.git(root, &["symbolic-ref", "-q", "HEAD"]).await?;
        match head.status.code() {
            Some(0) => {}
            Some(1) => return Ok(false),
            _ => return Err(failed_git(&["symbolic-ref", "-q", "HEAD"], &head)),
        }
        let head_ref = String::from_utf8_lossy(&head.stdout).trim().to_string();
        let branch = head_ref.strip_prefix("refs/heads/").unwrap_or(&head_ref);
        let key = format!("branch.{branch}.merge");
        let merge = self.git(root, &["config", "--get", &key]).await?;
        match merge.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(failed_git(&["config", "--get", &key], &merge)),
        }
    }
}

/// One line describing a git probe that exited non-zero.
fn failed_git(args: &[&str], output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let excerpt: String = stderr.trim().chars().take(200).collect();
    format!("git {} exited {}: {excerpt}", args.join(" "), output.status)
}

#[async_trait::async_trait]
impl CustodyProbe for GitCustodyProbe {
    async fn worktree_root(&self, dir: &Path) -> Result<Option<PathBuf>, String> {
        let dir = dir.to_path_buf();
        qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
            if !dir.is_dir() {
                return Err(format!("{} is not a readable directory", dir.display()));
            }
            // The nearest ancestor holding a `.git` entry — a directory for a
            // primary checkout, a file for a linked worktree — is what git
            // itself would resolve, without a subprocess per candidate.
            Ok(dir
                .ancestors()
                .find(|a| std::fs::symlink_metadata(a.join(".git")).is_ok())
                .map(Path::to_path_buf))
        })
        .await
        .map_err(|e| format!("worktree-root walk died: {e}"))?
    }

    async fn member_state(&self, root: &Path) -> MemberState {
        match std::fs::metadata(root.join(".git")) {
            Ok(meta) if meta.is_dir() => return MemberState::PrimaryCheckout,
            Ok(_) => {}
            Err(e) => return MemberState::ProbeFailed(format!("{}/.git: {e}", root.display())),
        }
        let status = match self
            .git_ok(root, &["status", "--porcelain", "--untracked-files=normal"])
            .await
        {
            Ok(out) => out,
            Err(e) => return MemberState::ProbeFailed(e),
        };
        match self.unpushed_commits(root).await {
            Ok(ahead) => MemberState::Probed {
                dirty: !status.is_empty(),
                ahead,
            },
            Err(e) => MemberState::ProbeFailed(e),
        }
    }

    async fn slot_wip_state(&self, root: &Path, claude_session_id: &str) -> Option<String> {
        let root = root.to_path_buf();
        let session_id = claude_session_id.to_string();
        // A read that dies answers `None`, the same as an absent slot. That is
        // safe only because the slot is a cross-check and never the source:
        // the git probes above already judged the tree on their own.
        qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
            custody::read_custody_slots(&root)
                .into_iter()
                .find(|slot| slot.session_id.as_deref() == Some(session_id.as_str()))
                .and_then(|slot| slot.wip_state)
        })
        .await
        .ok()
        .flatten()
    }
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
            has_live_children: Some(false),
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

    // ── Custody inputs (finished_close, D2b/D2c) ─────────────────────────

    use qontinui_runner_lib::wind_down::{custody_gate, CustodyUnknown, CustodyVerdict};

    /// A [`CustodyProbe`] answering from a script and recording every call.
    #[derive(Default)]
    struct FakeProbe {
        /// dir → its worktree root. Absent = in no repo.
        roots: HashMap<PathBuf, PathBuf>,
        /// dirs whose read fails.
        unreadable: HashSet<PathBuf>,
        /// root → its state. Absent = clean and pushed.
        states: HashMap<PathBuf, MemberState>,
        /// root → this session's slot wip_state.
        slots: HashMap<PathBuf, String>,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl FakeProbe {
        fn root(mut self, dir: &str, root: &str) -> Self {
            self.roots.insert(PathBuf::from(dir), PathBuf::from(root));
            self
        }
        fn state(mut self, root: &str, state: MemberState) -> Self {
            self.states.insert(PathBuf::from(root), state);
            self
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl CustodyProbe for FakeProbe {
        async fn worktree_root(&self, dir: &Path) -> Result<Option<PathBuf>, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("root:{}", dir.display()));
            if self.unreadable.contains(dir) {
                return Err(format!("{} is not a readable directory", dir.display()));
            }
            Ok(self.roots.get(dir).cloned())
        }
        async fn member_state(&self, root: &Path) -> MemberState {
            self.calls
                .lock()
                .unwrap()
                .push(format!("probe:{}", root.display()));
            self.states
                .get(root)
                .cloned()
                .unwrap_or(MemberState::Probed {
                    dirty: false,
                    ahead: 0,
                })
        }
        async fn slot_wip_state(&self, root: &Path, _claude_session_id: &str) -> Option<String> {
            self.slots.get(root).cloned()
        }
    }

    fn context(dir: Option<&str>, isolated: &[&str]) -> SessionCustodyContext {
        SessionCustodyContext {
            session_dir: dir.map(PathBuf::from),
            coord_session_id: Some("c-1".to_string()),
            isolated_worktrees: isolated.iter().map(PathBuf::from).collect(),
        }
    }

    #[tokio::test]
    async fn a_session_in_a_clean_worktree_gathers_one_clean_member() {
        let probe = FakeProbe::default().root("/ws/wt/repo/src", "/ws/wt/repo");
        let inputs = gather_custody(
            &probe,
            &context(Some("/ws/wt/repo/src"), &[]),
            Ok(vec![]),
            "s",
        )
        .await;
        assert_eq!(inputs.members.len(), 1);
        assert_eq!(inputs.members[0].path, "/ws/wt/repo");
        assert_eq!(custody_gate(&inputs), CustodyVerdict::Clean);
    }

    /// D2b: a workspace-root session has no repo of its own and is judged by
    /// its attributed worktrees alone.
    #[tokio::test]
    async fn a_workspace_root_session_is_judged_by_its_attributed_worktrees() {
        let probe = FakeProbe::default()
            .root("/ws/agent-worktrees/a/repo", "/ws/agent-worktrees/a/repo")
            .state(
                "/ws/agent-worktrees/a/repo",
                MemberState::Probed {
                    dirty: true,
                    ahead: 0,
                },
            );
        let none = gather_custody(&probe, &context(Some("/ws"), &[]), Ok(vec![]), "s").await;
        assert!(none.members.is_empty());
        assert!(custody_gate(&none).is_clean(), "nothing attributed → clean");

        let one = gather_custody(
            &probe,
            &context(Some("/ws"), &[]),
            Ok(vec![PathBuf::from("/ws/agent-worktrees/a/repo")]),
            "s",
        )
        .await;
        assert_eq!(one.members.len(), 1);
        assert_eq!(custody_gate(&one).as_str(), "dirty");
    }

    /// A multi-worktree session with one dirty sibling is not clean, and a
    /// worktree named by two sources is probed ONCE.
    #[tokio::test]
    async fn every_source_contributes_and_a_shared_root_is_probed_once() {
        let probe = FakeProbe::default()
            .root("/ws/wt/a", "/ws/wt/a")
            .root("/ws/wt/b", "/ws/wt/b")
            .state(
                "/ws/wt/b",
                MemberState::Probed {
                    dirty: false,
                    ahead: 3,
                },
            );
        let inputs = gather_custody(
            &probe,
            &context(Some("/ws/wt/a"), &["/ws/wt/a", "/ws/wt/b"]),
            Ok(vec![PathBuf::from("/ws/wt/a")]),
            "s",
        )
        .await;
        assert_eq!(
            probe
                .calls()
                .iter()
                .filter(|c| c.starts_with("probe:"))
                .count(),
            2,
            "{:?}",
            probe.calls()
        );
        assert_eq!(custody_gate(&inputs).reason(), Some("unpushed"));
    }

    /// The verdict is already `Unknown` without a directory or an ownership
    /// read, so nothing is probed.
    #[tokio::test]
    async fn an_already_unknown_verdict_probes_nothing() {
        let probe = FakeProbe::default().root("/ws/wt/a", "/ws/wt/a");
        let no_dir = gather_custody(&probe, &context(None, &["/ws/wt/a"]), Ok(vec![]), "s").await;
        assert_eq!(
            custody_gate(&no_dir),
            CustodyVerdict::Unknown(CustodyUnknown::NoSessionDir)
        );
        let no_ownership = gather_custody(
            &probe,
            &context(Some("/ws/wt/a"), &[]),
            Err("coord returned 503".to_string()),
            "s",
        )
        .await;
        assert_eq!(
            custody_gate(&no_ownership).reason(),
            Some("ownership_unreadable")
        );
        assert!(probe.calls().is_empty(), "{:?}", probe.calls());
    }

    /// An attributed worktree that is not a repo, or a directory that cannot
    /// be read, is a failed probe — never silently dropped.
    #[tokio::test]
    async fn a_named_worktree_that_is_not_a_repo_is_a_failed_probe() {
        let mut probe = FakeProbe::default();
        probe.unreadable.insert(PathBuf::from("/ws/gone"));
        let inputs = gather_custody(
            &probe,
            &context(Some("/ws"), &["/ws/not-a-repo"]),
            Ok(vec![PathBuf::from("/ws/gone")]),
            "s",
        )
        .await;
        assert_eq!(inputs.members.len(), 2);
        assert!(inputs
            .members
            .iter()
            .all(|m| matches!(m.state, MemberState::ProbeFailed(_))));
        assert_eq!(custody_gate(&inputs).reason(), Some("probe_failed"));
    }

    #[tokio::test]
    async fn the_sessions_own_slot_is_the_cross_check() {
        let mut probe = FakeProbe::default().root("/ws/wt/a", "/ws/wt/a");
        probe
            .slots
            .insert(PathBuf::from("/ws/wt/a"), "deferred".to_string());
        let inputs = gather_custody(&probe, &context(Some("/ws/wt/a"), &[]), Ok(vec![]), "s").await;
        assert_eq!(custody_gate(&inputs).reason(), Some("wip_slot"));
    }

    #[test]
    fn the_record_directory_wins_and_a_blank_one_falls_through() {
        assert_eq!(
            session_dir_from(Some("/rec"), Some("/cwd")),
            Some(PathBuf::from("/rec"))
        );
        assert_eq!(
            session_dir_from(Some("  "), Some("/cwd")),
            Some(PathBuf::from("/cwd"))
        );
        assert_eq!(session_dir_from(None, None), None);
    }

    fn ownership(json: &str) -> OwnershipRead {
        Ok(CoordOwnership::from_response(
            serde_json::from_str(json).unwrap(),
        ))
    }

    #[test]
    fn attributed_worktrees_are_anchored_on_the_workspace_root() {
        let own = ownership(
            r#"{"sessions":[{"sessionId":"c-1","ownerSessionState":"active","worktrees":[
                {"worktreePath":"agent-worktrees/a/repo","repo":"repo"},
                {"worktreePath":"/abs/wt","repo":"repo2"}]}]}"#,
        );
        assert_eq!(
            attributed_worktrees(&own, Some("c-1"), Some(Path::new("/ws"))).unwrap(),
            vec![
                PathBuf::from("/ws/agent-worktrees/a/repo"),
                PathBuf::from("/abs/wt")
            ]
        );
        assert!(
            attributed_worktrees(&own, Some("c-1"), None).is_err(),
            "a relative ledger path with no root to anchor it is not guessed"
        );
        assert_eq!(
            attributed_worktrees(&own, Some("c-other"), None).unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn attributed_worktrees_fail_closed_without_a_read_or_a_coord_id() {
        let own = ownership(r#"{"sessions":[]}"#);
        assert!(attributed_worktrees(&own, None, Some(Path::new("/ws"))).is_err());
        let failed: OwnershipRead = Err("coord returned 503".to_string());
        assert_eq!(
            attributed_worktrees(&failed, Some("c-1"), Some(Path::new("/ws"))),
            Err("coord returned 503".to_string())
        );
    }

    // ── The real git probe, against a temp repository ─────────────────────

    /// Run git in `dir` with an identity and no hooks, panicking on failure.
    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=wind-down-test",
                "-c",
                "user.email=wind-down-test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The D2c cases — clean, an untracked file, an unpushed commit, and a
    /// branch with no upstream both with and without a local-only commit —
    /// plus a detached HEAD and the primary-checkout case, through the
    /// SHIPPED probe and a real git.
    #[tokio::test]
    async fn the_git_probe_reads_clean_untracked_unpushed_and_no_upstream() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let primary = tmp.path().join("primary");
        let linked = tmp.path().join("linked");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "--bare", "-q"]);
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                primary.to_str().unwrap(),
            ],
        );
        std::fs::write(primary.join("README"), "x").unwrap();
        git(&primary, &["add", "README"]);
        git(&primary, &["commit", "-q", "-m", "init"]);
        git(&primary, &["push", "-q", "-u", "origin", "HEAD:main"]);
        git(&primary, &["fetch", "-q", "origin"]);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "--track",
                "-b",
                "feat",
                linked.to_str().unwrap(),
                "origin/main",
            ],
        );

        let probe = GitCustodyProbe::default();
        let clean = MemberState::Probed {
            dirty: false,
            ahead: 0,
        };

        // The root walk resolves a subdirectory to its worktree root.
        std::fs::create_dir_all(linked.join("sub")).unwrap();
        assert_eq!(
            probe.worktree_root(&linked.join("sub")).await.unwrap(),
            Some(linked.clone())
        );

        // Clean and pushed.
        assert_eq!(probe.member_state(&linked).await, clean);

        // An untracked file is dirty — `--untracked-files=normal`, unlike the
        // custody hook's `-uno`.
        std::fs::write(linked.join("scratch.txt"), "wip").unwrap();
        assert_eq!(
            probe.member_state(&linked).await,
            MemberState::Probed {
                dirty: true,
                ahead: 0
            }
        );

        // An unpushed commit.
        git(&linked, &["add", "scratch.txt"]);
        git(&linked, &["commit", "-q", "-m", "local only"]);
        assert_eq!(
            probe.member_state(&linked).await,
            MemberState::Probed {
                dirty: false,
                ahead: 1
            }
        );

        // Pushed, it is clean again.
        git(&linked, &["push", "-q", "origin", "HEAD:main"]);
        git(&linked, &["fetch", "-q", "origin"]);
        assert_eq!(probe.member_state(&linked).await, clean);

        // No upstream, HEAD on a remote-tracking ref's commit — the freshly
        // allocated worktree that made no commits: nothing is unpushed.
        git(&linked, &["switch", "-q", "-c", "no-upstream"]);
        assert_eq!(probe.member_state(&linked).await, clean);

        // No upstream, one local-only commit: on no remote ref, so unpushed.
        std::fs::write(linked.join("local.txt"), "mine").unwrap();
        git(&linked, &["add", "local.txt"]);
        git(&linked, &["commit", "-q", "-m", "on no remote"]);
        let one_unpushed = MemberState::Probed {
            dirty: false,
            ahead: 1,
        };
        assert_eq!(probe.member_state(&linked).await, one_unpushed);

        // A detached HEAD has no upstream either and is judged the same way.
        git(&linked, &["switch", "-q", "--detach"]);
        assert_eq!(probe.member_state(&linked).await, one_unpushed);
        git(&linked, &["switch", "-q", "--detach", "origin/main"]);
        assert_eq!(probe.member_state(&linked).await, clean);

        // The primary clone's `.git` is a directory.
        assert_eq!(
            probe.member_state(&primary).await,
            MemberState::PrimaryCheckout
        );

        // And outside any repository there is no root at all.
        let outside = tempfile::tempdir().unwrap();
        assert_eq!(probe.worktree_root(outside.path()).await.unwrap(), None);
    }
}
