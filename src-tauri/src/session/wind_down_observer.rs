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
//! bounded by [`CUSTODY_PROBE_TIMEOUT`]. The session's transcript is read
//! through the same seam ([`CustodyProbe::transcript_scan`], bounded by
//! [`crate::session::transcript_trees`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::agent_worktree::custody::{self, coord::CoordOwnership};
use crate::mcp::session_work_status::StatusFetch;
use crate::session::session_lifecycle_store::TerminalSessionRecord;
use crate::session::tracking_health::{LiveClaudeProcess, SessionWorkStatus, TrackingHealthPass};
use crate::session::transcript_trees::{self, TranscriptScan};
use crate::terminal::agent_status_sideband::ObservedAgentState;
use crate::terminal::TerminalManager;
use qontinui_runner_lib::wind_down::{
    self, CustodyInputs, CustodyMember, EligibilityInputs, GridIdle, MemberState, SessionKind,
    Sideband, SidebandState, TranscriptRead, WindDownView, WorkStatus,
};

/// The cause recorded when the terminal-session plane cannot be counted
/// because `tracking_health::compute` saw an UNKNOWN process table. Shared
/// with the restart-readiness tests so the two cannot drift.
pub(crate) const TERMINAL_PLANE_TABLE_UNKNOWN: &str = "the terminal-session plane could not be determined: the process table is unreadable (the fresh process-table read failed or served only the stale last-good table, ProcessSnapshot::is_unknown), so live `claude` processes cannot be enumerated";

/// The census's undecided population, logged by its one reader: every
/// `/restart-readiness` request and every wind-down pass come through
/// [`fresh_pass`], so this is the only place it is observed, and one line
/// names both consequences (see [`undecided_unknown`]).
static CENSUS_UNDECIDED_LOG: crate::session::tracking_health::UndecidedLog =
    crate::session::tracking_health::UndecidedLog::new(
        "live-claude census",
        "a restart is refused (safe_to_restart: false) AND the wind-down executor closes \
         nothing (neither its drain arm nor finished_close) while it lasts",
    );

/// The `unknowns` entry for a pass whose census holds processes Claude Code's
/// session registry could not settle
/// ([`crate::session::tracking_health::TrackingHealthReport::undecided`]): a
/// possible npm-installed `claude` is never read as absent, so the pass is not
/// safe to act on. `None` when every process was decided.
pub(crate) fn undecided_unknown(
    report: &crate::session::tracking_health::TrackingHealthReport,
) -> Option<String> {
    let n = report.undecided.len();
    (n > 0).then(|| {
        let pids: Vec<String> = report.undecided.iter().map(|u| u.pid.to_string()).collect();
        format!(
            "{n} process{} (pid {}) may be a live npm-installed `claude` that Claude Code's \
             session registry could not settle ({}), so the live-`claude` census is UNDECIDED",
            if n == 1 { "" } else { "es" },
            pids.join(", "),
            report.undecided[0].reason,
        )
    })
}

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
    /// Whether the looping-agent registry answered this pass. When it did
    /// not, `looping_terminal_ids` is empty and every looping agent's pane
    /// resolves to [`SessionKind::Terminal`] — the strictest kind for the drain
    /// arm, but the ONE kind the `finished_close` arm may close, so that arm
    /// considers nothing on a pass where this is `false` (unknown never closes).
    pub looping_registry_resolved: bool,
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
    let looping = looping_terminal_ids(app);
    let looping_registry_resolved = looping.is_some();
    let looping_terminal_ids = looping.unwrap_or_default();
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
        looping_registry_resolved,
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
    /// reason is then in `unknowns`. A pass that IS here can still be
    /// incomplete — a census with undecided processes ([`undecided_unknown`]) —
    /// and then `unknowns` names why: a caller that ACTS on the pass must act
    /// on nothing while `unknowns` is non-empty.
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
            &status_fetch.since_by_session_id,
        )
        .await
        {
            Some(pass) => {
                // UNDECIDED reads as not-safe: the pass is kept (its counts
                // are reported), but it is incomplete, and a caller that acts
                // on a pass acts on none with an `unknowns` entry.
                let _ = CENSUS_UNDECIDED_LOG.observe(&pass.report.undecided);
                unknowns.extend(undecided_unknown(&pass.report));
                Some(pass)
            }
            None => {
                unknowns.push(TERMINAL_PLANE_TABLE_UNKNOWN.to_string());
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
/// this pass can see at all — which is not eligibility either — or that the
/// looping-agent registry did not answer, so the verdict's `kind` cannot be
/// trusted (both arms fail closed on `None`).
pub async fn recheck(
    app: &tauri::AppHandle,
    grace: Duration,
    claude_session_id: &str,
    terminal_id: &str,
) -> Option<WindDownView> {
    let fresh = fresh_pass(app, grace).await;
    // An incomplete pass (see [`FreshPass::pass`]) re-derives no verdict.
    if !fresh.observed.looping_registry_resolved || !fresh.unknowns.is_empty() {
        return None;
    }
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
    // coord's finish stamp — carried on the view so the close guard's revival
    // arm judges against the stamp of the SAME fresh pass as its eligibility.
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
    let mut view = WindDownView::from_verdict(&wind_down::eligibility(&inputs, now_ms), kind);
    view.finished_at_ms = finished_at_ms;
    Some(view)
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

/// Terminal ids the looping-agent registry currently attributes to an agent,
/// or `None` when the registry is not reachable this pass (see
/// [`ObservedInputs::looping_registry_resolved`] for what each arm does then).
pub fn looping_terminal_ids(app: &tauri::AppHandle) -> Option<HashSet<String>> {
    use tauri::Manager;

    app.try_state::<Arc<qontinui_runner_lib::looping_agent::registry::LoopingAgentRegistry>>()
        .map(|registry| {
            registry
                .list()
                .into_iter()
                .filter_map(|rec| rec.runtime.terminal_id)
                .collect()
        })
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

/// Read coord's attributed-worktree index for exactly `claude_session_ids` —
/// `custody::coord::fetch_live_ownership`, coord's keyed read (one snapshot
/// per chunk), which is `Err` only when it could not be read at all and
/// otherwise records every id it did not answer (including every id of a
/// chunk that failed) as UNKNOWN.
pub async fn fetch_ownership(claude_session_ids: &[String]) -> OwnershipRead {
    custody::coord::fetch_live_ownership(claude_session_ids).await
}

/// Per-request deadline for coord's obligations read. Coord bounds its own
/// inline verdicts at 25 s (`VERDICT_DEADLINE` in `overlord_proof.rs`) and
/// serves the answer after that with the stragglers marked `unknown`, so the
/// client waits past it; shorter would turn a slow-but-answering coord into a
/// transport failure and lose the typed `unknown` items it would have served.
pub const OBLIGATIONS_REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

/// `{base}/coord/overlord/session-obligations/{id}` — the HTTP twin of the
/// `coord_session_obligations` MCP tool (qontinui-coord#2913), each over one
/// shared core in coord, so this is the same derivation `/next-move` and
/// `session-closeable.sh` read.
pub fn obligations_url(base: &str, claude_session_id: &str) -> String {
    format!(
        "{}/coord/overlord/session-obligations/{}",
        base.trim_end_matches('/'),
        urlencoding::encode(claude_session_id.trim())
    )
}

/// GET `req` with `timeout` and decode a 2xx body as JSON. Every failure is
/// `Err` with its cause chain — the caller feeds it to
/// [`wind_down::obligations_gate`], where `Err` is UNKNOWN and never "nothing
/// owed". Split from [`fetch_obligations`] so a test can drive it over a
/// loopback socket without a device bearer.
pub async fn read_obligations(
    req: reqwest::RequestBuilder,
    url: &str,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let what = |e: &(dyn std::error::Error + 'static)| {
        format!("{url}: {}", crate::util::error_chain::error_chain(e))
    };
    let resp = req
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| format!("GET {}", what(&e)))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(200).collect();
        return Err(format!("coord returned {status} for GET {url}: {excerpt}"));
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("decode GET {}", what(&e)))
}

/// Read coord's obligations for one session — the REMOTE half of `closeable`
/// (plan `2026-10-18-a-finished-session-is-safe-to-close-when-nothing-lives-only-in-it-and-coord-owns-whether-its-watchers-work`,
/// Phase 6). `Err` when no coord base is configured, the shared client could
/// not be built, or the read failed in any way; the gate reads every `Err` as
/// UNKNOWN. Like [`fetch_ownership`] it presents the device's default
/// credential, and relies on the answer's `session_proven_in_tenant` to refuse
/// a session that credential cannot place.
pub async fn fetch_obligations(claude_session_id: &str) -> Result<serde_json::Value, String> {
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        return Err(
            "no coord base is configured, so the session's obligations cannot be read".to_string(),
        );
    };
    let Some(client) = crate::coord_http::coord_client() else {
        return Err(
            "the shared coord HTTP client could not be built, so the session's obligations \
             cannot be read"
                .to_string(),
        );
    };
    let url = obligations_url(&base, claude_session_id);
    read_obligations(
        crate::coord_http::coord_get(client, &url),
        &url,
        OBLIGATIONS_REQUEST_TIMEOUT,
    )
    .await
}

/// The first worktree in coord's obligations answer (`worktrees.items[]`)
/// that the custody gate did NOT cover — the rule that keeps the remote half
/// of `closeable` from vouching for a tree the local half never looked at
/// (#2981 review concern 3, coord finding `9f265810`).
///
/// coord's obligations read and the custody gate's keyed ownership read both
/// derive from `coord.agent_worktrees`, but through different keys and at
/// different moments, so they can disagree: a row allocated between the two
/// reads, or one keyed to the session one way and not the other. Each item's
/// `worktree_path` must match a path in `covered` (the gather's
/// [`wind_down::CustodyInputs::covered`]) by [`custody::worktree_path_matches`]
/// — exact after normalization, or the covered absolute path ending in coord's
/// relative ledger path. An item with no `worktree_path` cannot be matched and
/// is returned as `<absent>`. A body with no `worktrees` block, or one whose
/// `items` is not a list, returns `None`: the obligations gate judges that
/// block's coverage itself.
///
/// `Some` makes a `watched` obligations verdict UNKNOWN
/// (`ObligationsUnknown::WorktreeNotProbed`), never the reverse: it only ever
/// withholds a close.
pub fn first_uncovered_coord_worktree(
    doc: &serde_json::Value,
    covered: &[String],
) -> Option<String> {
    let items = doc.get("worktrees")?.get("items")?.as_array()?;
    items.iter().find_map(|item| {
        match item
            .get("worktree_path")
            .and_then(serde_json::Value::as_str)
        {
            Some(path) if !path.trim().is_empty() => (!covered
                .iter()
                .any(|c| custody::worktree_path_matches(c, path)))
            .then(|| path.to_string()),
            _ => Some("<absent>".to_string()),
        }
    })
}

/// Per-request deadline for coord's pending-message read (the close guard's
/// "no undelivered message" input) — the delivery poller's own bound.
pub const PENDING_MESSAGES_TIMEOUT: Duration = Duration::from_secs(10);

/// coord's undelivered, unacked, unexpired messages for this device — `GET
/// /coord/session-messages/pending`, the door the delivery poller pulls — as
/// `(message_id, to_session)` pairs. `?live=` names the session when its id is
/// a UUID, which ranks its oldest row onto the page (coord's round-robin puts
/// a `live` recipient's first row ahead of every other recipient's). `Err`
/// when there is no coord base, no shared client, no device credential (coord
/// refuses an anonymous read), the read fails, or the body is not the shape
/// coord serves: the close guard reads every `Err` as UNKNOWN.
pub async fn fetch_pending_messages(
    claude_session_id: &str,
) -> Result<Vec<(String, String)>, String> {
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        return Err("no coord base is configured, so pending messages cannot be read".to_string());
    };
    let Some(client) = crate::coord_http::coord_client() else {
        return Err("the shared coord HTTP client could not be built".to_string());
    };
    if !crate::coord_http::have_device_token() {
        return Err(
            "no device credential: coord's pending-message read refuses an anonymous caller"
                .to_string(),
        );
    }
    let mut url = format!(
        "{}/coord/session-messages/pending",
        base.trim_end_matches('/')
    );
    if let Ok(uuid) = claude_session_id.trim().parse::<uuid::Uuid>() {
        url.push_str(&format!("?live={uuid}"));
    }
    let body = read_obligations(
        crate::coord_http::coord_get(client, &url),
        &url,
        PENDING_MESSAGES_TIMEOUT,
    )
    .await?;
    pending_message_rows(&body)
}

/// The `(message_id, to_session)` rows of a pending-message body. A body with
/// no `messages` list, or a row missing either id, is `Err`: a row that cannot
/// be attributed must not vanish. The error names such a row by its
/// `message_id` alone and never quotes it: a row carries the message body, and
/// this text reaches the close-guard log line and `/restart-readiness`.
pub fn pending_message_rows(body: &serde_json::Value) -> Result<Vec<(String, String)>, String> {
    let rows = body
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "the pending-message body carries no `messages` list".to_string())?;
    rows.iter()
        .map(|row| {
            let field = |key: &str| {
                row.get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| {
                        let id = row
                            .get("message_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("<no message_id>");
                        format!("pending message {id} has no `{key}`")
                    })
            };
            Ok((field("message_id")?, field("to_session")?))
        })
        .collect()
}

/// What the runner knows about one candidate before any probe runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCustodyContext {
    /// The lifecycle record's `working_dir`, else the process cwd (which is
    /// `None` on Windows — `tracking_health` reads it from `/proc`).
    pub session_dir: Option<PathBuf>,
    /// The parked isolated-edit context's materialized worktrees; `Err` when
    /// the context's slot could not be read (a poisoned lock), which must not
    /// read as "no isolated worktrees".
    pub isolated_worktrees: Result<Vec<PathBuf>, String>,
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
        // A pane that is gone parks no context at all.
        isolated_worktrees: pane.map_or(Ok(Vec::new()), |s| s.isolated_edit_worktree_paths()),
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

/// The `coord.agent_worktrees.status` values whose work is OVER: the
/// worktree's branch merged, or the allocation was abandoned.
pub const RETIRED_LEDGER_STATUSES: [&str; 2] = ["merged", "abandoned"];

/// One worktree coord attributes to the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributedWorktree {
    pub path: PathBuf,
    /// The ledger says its work is over ([`RETIRED_LEDGER_STATUSES`]). Such a
    /// row whose path is already gone was reaped, and has nothing left to
    /// lose; a LIVE row whose path is gone is still an unknown.
    pub retired: bool,
}

/// PURE: the worktrees coord attributes to the session, as absolute paths.
///
/// `Err` — which the gate reads as `Unknown` — when the index could not be
/// read, when coord's keyed read did not answer `claude_session_id` (it was
/// `unknown`, `invalid`, cut by `inputTruncated`, or had no entry of its
/// own), or when a ledger path is relative and no workspace root resolves to
/// anchor it.
///
/// The Claude session resolves ONLY through its own keyed entry
/// (`claudeCodeSessionId → sessionId`). There is deliberately no fallback to
/// the pane's own coord id: a pane id that differs from the row coord files
/// the Claude session's worktrees under would look up a row holding none of
/// them and read `[]` — a false Clean.
pub fn attributed_worktrees(
    ownership: &OwnershipRead,
    claude_session_id: &str,
    workspace_root: Option<&Path>,
) -> Result<Vec<AttributedWorktree>, String> {
    let index = ownership.as_ref().map_err(String::clone)?;
    if let Some(why) = index.unanswered_reason(claude_session_id) {
        return Err(format!(
            "coord's keyed worktree read did not answer Claude session {claude_session_id} \
             ({why}), so its worktree allocations cannot be read"
        ));
    }
    let Some(session_id) = index.coord_session_for_claude(claude_session_id) else {
        return Err(format!(
            "coord's keyed worktree read carries no entry for Claude session \
             {claude_session_id}, so its worktree allocations cannot be read"
        ));
    };
    index
        .worktrees_for_session(session_id)
        .into_iter()
        .map(|row| {
            let raw = PathBuf::from(&row.worktree_path);
            let path = if raw.is_absolute() {
                raw
            } else {
                workspace_root.map(|root| root.join(&raw)).ok_or_else(|| {
                    format!(
                        "coord's ledger path {} is relative and the workspace root does not resolve",
                        row.worktree_path
                    )
                })?
            };
            let retired = row
                .ledger_status
                .as_deref()
                .is_some_and(|s| RETIRED_LEDGER_STATUSES.contains(&s));
            Ok(AttributedWorktree { path, retired })
        })
        .collect()
}

/// The questions a custody gather asks of the filesystem and git, behind a
/// seam so the gather is testable without either.
#[async_trait::async_trait]
pub trait CustodyProbe: Send + Sync {
    /// Does `path` exist at all? `Err` when that could not be determined —
    /// only a positive "not found" is `Ok(false)`.
    async fn exists(&self, path: &Path) -> Result<bool, String>;
    /// The root of the worktree containing `dir`. `Ok(None)` when `dir` is in
    /// no repository at all; `Err` when `dir` cannot be read.
    async fn worktree_root(&self, dir: &Path) -> Result<Option<PathBuf>, String>;
    /// Probe one worktree root (D2c).
    async fn member_state(&self, root: &Path) -> MemberState;
    /// `wip_state` of the custody SLOT this session wrote in `root`, if any.
    async fn slot_wip_state(&self, root: &Path, claude_session_id: &str) -> Option<String>;
    /// The directories the session's transcript names, and whether it proves
    /// the session called no write-capable tool
    /// ([`crate::session::transcript_trees`]).
    async fn transcript_scan(&self, claude_session_id: &str) -> TranscriptScan;
    /// What a REAPED allocation tree (one [`allocation_tree`] names that no
    /// longer exists) left behind: its working tree is gone, but its branch
    /// survives the reap in the primary checkout and may hold commits on no
    /// remote. `Probed { dirty: false, ahead }` counts those commits;
    /// `ProbeFailed` when the branch or its primary cannot be found or read.
    async fn reaped_allocation_state(&self, tree: &Path) -> MemberState;
}

/// The worktree holding a directory the transcript names, found through its
/// nearest EXISTING ancestor. A transcript can name a directory that has
/// since been removed. `Ok(None)` when that ancestor is in no repository.
/// `Err` when an existence check could not be answered, or no ancestor
/// exists.
async fn transcript_dir_root<P: CustodyProbe + ?Sized>(
    probe: &P,
    dir: &Path,
) -> Result<Option<PathBuf>, String> {
    for ancestor in dir.ancestors().filter(|a| !a.as_os_str().is_empty()) {
        if probe.exists(ancestor).await? {
            return probe.worktree_root(ancestor).await;
        }
    }
    Err(format!("no ancestor of {} exists", dir.display()))
}

/// The directories, under the workspace root, that allocations are
/// materialised in: `allocate-worktree.sh`'s and the runner's own.
const ALLOCATION_ROOT_DIRNAMES: [&str; 2] = [
    crate::agent_worktree::canonical_paths::SCRIPT_WORKTREE_ROOT_DIRNAME,
    crate::agent_worktree::canonical_paths::WORKTREE_ROOT_DIRNAME,
];

/// PURE: the allocation tree a directory sits under, by path alone —
/// `<ws>/<root>/<agent_id>/<repo>`, or `<ws>/<root>/<agent_id>` when the
/// directory names no `<repo>` component, where `<root>` is one of
/// [`ALLOCATION_ROOT_DIRNAMES`]. `None` outside that layout, and for a
/// dot-named `<agent_id>` (the shared `.siblings` store, never an allocation).
pub fn allocation_tree(dir: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let parts: Vec<Component> = dir.components().collect();
    let at = parts.iter().position(|c| {
        matches!(c, Component::Normal(n)
            if ALLOCATION_ROOT_DIRNAMES
                .iter()
                .any(|root| n.to_string_lossy().eq_ignore_ascii_case(root)))
    })?;
    match parts.get(at + 1)? {
        Component::Normal(id) if !id.to_string_lossy().starts_with('.') => {}
        _ => return None,
    }
    let end = match parts.get(at + 2) {
        Some(Component::Normal(_)) => at + 3,
        _ => at + 2,
    };
    Some(parts[..end].iter().collect())
}

/// PURE: where a reaped allocation tree `<ws>/<root>/<agent_id>/<repo>` left
/// its branch — the primary checkout `<ws>/<repo>` — and the first 12 hex
/// digits of `<agent_id>`, which its branch name carries
/// (`agent/<…>-<id12>/<slug>`). `Err` when the tree names no `<repo>` or the
/// id has fewer than 12 hex digits.
pub fn reaped_allocation_origin(tree: &Path) -> Result<(PathBuf, String), String> {
    let unnamed = || format!("{} names no <agent_id>/<repo> allocation", tree.display());
    let repo = tree.file_name().ok_or_else(unnamed)?;
    let id_dir = tree.parent().ok_or_else(unnamed)?;
    let id = id_dir.file_name().ok_or_else(unnamed)?.to_string_lossy();
    let root = id_dir.parent().ok_or_else(unnamed)?;
    let is_root = root.file_name().is_some_and(|n| {
        ALLOCATION_ROOT_DIRNAMES
            .iter()
            .any(|r| n.to_string_lossy().eq_ignore_ascii_case(r))
    });
    let workspace = root.parent().filter(|_| is_root).ok_or_else(unnamed)?;
    let id12: String = id.chars().filter(|c| *c != '-').take(12).collect();
    if id12.len() < 12 || !id12.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "allocation id {id} does not carry the 12 hex digits its branch is named by"
        ));
    }
    Ok((workspace.join(repo), id12.to_ascii_lowercase()))
}

/// PURE: is `refname` a branch of the allocation whose id starts `id12` —
/// `refs/heads/agent/<…>-<id12>/<slug>`?
pub fn allocation_branch_matches(refname: &str, id12: &str) -> bool {
    refname
        .strip_prefix("refs/heads/agent/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(owner, slug)| {
            !slug.is_empty() && owner.to_ascii_lowercase().ends_with(&format!("-{id12}"))
        })
}

/// Gather [`CustodyInputs`] for one candidate (D2b).
///
/// The set is the repo holding the session's directory, every attributed
/// worktree, every isolated-edit worktree and every worktree holding a
/// directory the session's transcript names, each resolved to its worktree
/// root and probed ONCE however many sources named it. A directory in no
/// repository contributes no member, so a session at the workspace root is
/// judged by the worktrees the other sources name. When none names one,
/// the transcript's no-write proof decides (step 5 of
/// [`wind_down::custody_gate`]), and a transcript that could not be read
/// whole leaves any set UNKNOWN (step 6). An attributed worktree whose ledger
/// row is retired (merged or abandoned) and whose path no longer exists was
/// reaped and is skipped. A transcript directory under an allocation tree
/// that is gone ([`allocation_tree`]) is covered and judged by the branch the
/// reaped tree left ([`CustodyProbe::reaped_allocation_state`]). Every other
/// missing path is a failed probe.
///
/// When the verdict is already decided — no directory, or an unreadable
/// attribution (the ownership read, or the isolated-edit context), both
/// `Unknown` — nothing is probed: the probes could not change the answer and
/// they are the expensive part.
pub async fn gather_custody<P: CustodyProbe + ?Sized>(
    probe: &P,
    context: &SessionCustodyContext,
    attributed: Result<Vec<AttributedWorktree>, String>,
    claude_session_id: &str,
) -> CustodyInputs {
    let session_dir = context
        .session_dir
        .as_ref()
        .map(|d| d.display().to_string());
    let attribution = attributed.and_then(|coord| {
        context
            .isolated_worktrees
            .clone()
            .map(|isolated| (coord, isolated))
            .map_err(|e| format!("the isolated-edit context could not be read: {e}"))
    });
    let ownership = attribution.as_ref().map(|_| ()).map_err(String::clone);
    let (Some(dir), Ok((attributed, isolated))) = (context.session_dir.as_ref(), attribution)
    else {
        return CustodyInputs {
            session_dir,
            ownership,
            members: Vec::new(),
            covered: Vec::new(),
            transcript: TranscriptRead::unread("not read: the verdict was already decided"),
        };
    };

    let mut members: Vec<CustodyMember> = Vec::new();
    // Every path this gather looked at — what the obligations cross-check
    // holds coord's `worktrees` block against. An attributed path is covered
    // whatever its probe found (a failed probe is already this gate's own
    // Unknown, and a reaped one was positively seen gone).
    let mut covered: Vec<String> = attributed
        .iter()
        .map(|w| w.path.display().to_string())
        .chain(isolated.iter().map(|p| p.display().to_string()))
        .collect();
    let mut named: Vec<(&Path, bool)> = vec![(dir.as_path(), true)];
    for worktree in &attributed {
        if worktree.retired {
            match probe.exists(&worktree.path).await {
                // Reaped: its work is over and nothing is left to lose.
                Ok(false) => continue,
                Ok(true) => {}
                Err(e) => {
                    members.push(CustodyMember {
                        path: worktree.path.display().to_string(),
                        state: MemberState::ProbeFailed(e),
                        slot_wip_state: None,
                    });
                    continue;
                }
            }
        }
        named.push((worktree.path.as_path(), false));
    }
    named.extend(isolated.iter().map(|p| (p.as_path(), false)));

    let mut roots: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
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
    // The transcript's directories (plan
    // `2026-11-20-triage-sessions-custody-reads-the-sessions-recorded-trees-not-its-cwd`,
    // D1, D6, D10 and D11). One in no repository, such as a scratch dir or the
    // workspace root itself, names no tree and contributes nothing. One in a
    // primary checkout is that checkout, a member (D10). One under an
    // allocation tree that is gone was reaped (D11): it is never walked up into
    // the repository that encloses the allocation root. The tree is covered and
    // judged by the branch it left behind, which survives the reap, so commits
    // on no remote are not lost to a close.
    let transcript = probe.transcript_scan(claude_session_id).await;
    let mut allocations: HashMap<PathBuf, bool> = HashMap::new();
    for dir in &transcript.dirs {
        if let Some(tree) = allocation_tree(dir) {
            let live = match allocations.get(&tree) {
                Some(live) => Ok(*live),
                None => probe.exists(&tree).await,
            };
            match live {
                Ok(false) => {
                    if allocations.insert(tree.clone(), false).is_none() {
                        tracing::debug!(
                            session_id = %claude_session_id,
                            tree = %tree.display(),
                            "custody: the transcript names a reaped allocation tree; judging its branch"
                        );
                        covered.push(tree.display().to_string());
                        members.push(CustodyMember {
                            path: tree.display().to_string(),
                            state: probe.reaped_allocation_state(&tree).await,
                            slot_wip_state: None,
                        });
                    }
                    continue;
                }
                Ok(true) => {
                    allocations.insert(tree, true);
                }
                Err(e) => {
                    members.push(CustodyMember {
                        path: dir.display().to_string(),
                        state: MemberState::ProbeFailed(e),
                        slot_wip_state: None,
                    });
                    continue;
                }
            }
        }
        match transcript_dir_root(probe, dir).await {
            Ok(Some(root)) => {
                if seen.insert(custody::norm(&root.to_string_lossy())) {
                    roots.push(root);
                }
            }
            Ok(None) => {}
            Err(e) => members.push(CustodyMember {
                path: dir.display().to_string(),
                state: MemberState::ProbeFailed(e),
                slot_wip_state: None,
            }),
        }
    }
    covered.extend(roots.iter().map(|r| r.display().to_string()));
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
        covered,
        transcript: transcript.read,
    }
}

/// The shipped [`CustodyProbe`]: a filesystem walk for the root, git for the
/// state, the custody slot store for the cross-check, and the session's
/// transcript for the directories it names.
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
    /// Run `git --no-optional-locks -C <root> <args>` under [`Self::timeout`].
    /// `Err` only when the process could not be run or did not finish in time;
    /// a non-zero exit is returned for the caller to read, because for
    /// `symbolic-ref -q` and `show-ref -q` it is an answer rather than a
    /// failure.
    ///
    /// The probe runs in OTHER sessions' worktrees as readily as in this one's,
    /// so it must leave no trace there: `--no-optional-locks` (and
    /// `GIT_OPTIONAL_LOCKS=0`) stops `git status` taking `index.lock` to
    /// refresh the index, which would make a peer's own `git` fail on a lock it
    /// never took.
    async fn git(&self, root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
        let mut cmd = crate::process_helpers::tokio_no_window("git");
        cmd.arg("--no-optional-locks")
            .arg("-C")
            .arg(root)
            .args(args)
            // An inherited GIT_DIR / GIT_WORK_TREE would point every probe at
            // some other repository than the one named by `-C`.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("GIT_OPTIONAL_LOCKS", "0")
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
    /// * **With an upstream that resolves** — `rev-list --count
    ///   <upstream>..HEAD`.
    /// * **With none, a detached `HEAD`, or an upstream whose remote-tracking
    ///   ref no longer resolves (pruned after its branch was deleted)** —
    ///   `rev-list --count HEAD --not --remotes`: the commits on NO
    ///   remote-tracking ref. `0` means every commit at `HEAD` is already on
    ///   some remote, so nothing is unpushed; that is the shape of a freshly
    ///   allocated worktree that made no commits (it sits on `origin/main`'s
    ///   commit with no upstream), the most common finished session there is.
    ///
    /// Which arm applies is decided from exit codes and ref names, never from
    /// git's (localisable) error text. Every call is local — `rev-list` reads
    /// remote-tracking refs as this clone last saw them and fetches nothing,
    /// and [`Self::git`] sets `GIT_NO_LAZY_FETCH` so a partial clone cannot
    /// fetch a missing object behind the probe's back. `Err` only when a git
    /// call failed or timed out.
    async fn unpushed_commits(&self, root: &Path) -> Result<u64, String> {
        let range = self
            .resolved_upstream(root)
            .await?
            .map(|upstream| format!("{upstream}..HEAD"));
        let args: Vec<&str> = match range.as_deref() {
            Some(range) => vec!["rev-list", "--count", range],
            None => vec!["rev-list", "--count", "HEAD", "--not", "--remotes"],
        };
        let count = self.git_ok(root, &args).await?;
        count
            .parse::<u64>()
            .map_err(|e| format!("git {} printed {count:?}: {e}", args.join(" ")))
    }

    /// The full ref name of `HEAD`'s upstream when one is configured, is a
    /// REMOTE-tracking ref, and still resolves; `None` for a detached `HEAD`
    /// (`symbolic-ref -q` exits 1), a branch with no upstream (`for-each-ref`
    /// prints nothing), an upstream that is a LOCAL branch (`--track main`:
    /// `main..HEAD == 0` proves nothing was pushed), and an upstream whose ref
    /// is gone (`show-ref --verify -q` exits 1).
    async fn resolved_upstream(&self, root: &Path) -> Result<Option<String>, String> {
        let head = self.git(root, &["symbolic-ref", "-q", "HEAD"]).await?;
        match head.status.code() {
            Some(0) => {}
            Some(1) => return Ok(None),
            _ => return Err(failed_git(&["symbolic-ref", "-q", "HEAD"], &head)),
        }
        let head_ref = String::from_utf8_lossy(&head.stdout).trim().to_string();
        let upstream = self
            .git_ok(root, &["for-each-ref", "--format=%(upstream)", &head_ref])
            .await?;
        if !upstream.starts_with("refs/remotes/") {
            return Ok(None);
        }
        let exists = self
            .git(root, &["show-ref", "--verify", "-q", &upstream])
            .await?;
        match exists.status.code() {
            Some(0) => Ok(Some(upstream)),
            Some(1) => Ok(None),
            _ => Err(failed_git(
                &["show-ref", "--verify", "-q", &upstream],
                &exists,
            )),
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
    async fn exists(&self, path: &Path) -> Result<bool, String> {
        let path = path.to_path_buf();
        // Only `NotFound` is "missing". Permission denied, a stale mount or a
        // walk that died says nothing about whether the worktree was reaped.
        qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
            match std::fs::symlink_metadata(&path) {
                Ok(_) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(format!("{}: {e}", path.display())),
            }
        })
        .await
        .map_err(|e| format!("existence check died: {e}"))?
    }

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

    async fn transcript_scan(&self, claude_session_id: &str) -> TranscriptScan {
        let session_id = claude_session_id.to_string();
        qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
            transcript_trees::scan_session(&session_id)
        })
        .await
        .unwrap_or_else(|e| TranscriptScan::unread(format!("the transcript scan died: {e}")))
    }

    async fn reaped_allocation_state(&self, tree: &Path) -> MemberState {
        match self.reaped_branch_commits(tree).await {
            Ok(ahead) => MemberState::Probed {
                dirty: false,
                ahead,
            },
            Err(e) => MemberState::ProbeFailed(e),
        }
    }
}

impl GitCustodyProbe {
    /// The commits on no remote-tracking ref across every branch a reaped
    /// allocation tree left in its primary checkout
    /// ([`reaped_allocation_origin`]). `Err` when that primary is not a
    /// primary checkout, no branch carries the allocation's id, or a git call
    /// fails. A missing branch is not "nothing to lose": it may be the wrong
    /// primary, so it is never read as zero.
    async fn reaped_branch_commits(&self, tree: &Path) -> Result<u64, String> {
        let (primary, id12) = reaped_allocation_origin(tree)?;
        match std::fs::metadata(primary.join(".git")) {
            Ok(meta) if meta.is_dir() => {}
            _ => {
                return Err(format!(
                    "{} was reaped and {} is not the primary checkout its branch would be in",
                    tree.display(),
                    primary.display()
                ))
            }
        }
        let refs = self
            .git_ok(
                &primary,
                &["for-each-ref", "--format=%(refname)", "refs/heads/agent/"],
            )
            .await?;
        let branches: Vec<&str> = refs
            .lines()
            .filter(|r| allocation_branch_matches(r, &id12))
            .collect();
        if branches.is_empty() {
            return Err(format!(
                "{} was reaped and {} holds no branch refs/heads/agent/*-{id12}/*, so what it \
                 left cannot be read",
                tree.display(),
                primary.display()
            ));
        }
        let mut args = vec!["rev-list", "--count"];
        args.extend(branches.iter().copied());
        args.extend(["--not", "--remotes"]);
        let count = self.git_ok(&primary, &args).await?;
        count
            .parse::<u64>()
            .map_err(|e| format!("git {} printed {count:?}: {e}", args.join(" ")))
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
            session_status_since: None,
            blocks_restart: status != Some("finished"),
            wind_down: None,
            stray_wait_shells: 0,
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

    use qontinui_runner_lib::wind_down::{
        custody_gate, CustodyDirty, CustodyUnknown, CustodyVerdict,
    };

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
        /// paths that do not exist at all.
        missing: HashSet<PathBuf>,
        /// paths whose existence cannot be determined.
        exists_unreadable: HashSet<PathBuf>,
        /// What the transcript scan answers. Absent = read whole, naming no
        /// directory and no write-capable call: the tests that exercise the
        /// other sources need a transcript that adds nothing.
        transcript: Option<TranscriptScan>,
        /// reaped allocation tree → what its branch left. Absent = unreadable.
        reaped: HashMap<PathBuf, MemberState>,
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
        /// Script the transcript scan: the directories it names, the reason
        /// it was not read whole (if any), and the write-capable call it holds
        /// (if any).
        fn transcript(
            mut self,
            dirs: &[&str],
            incomplete: Option<&str>,
            write_call: Option<&str>,
        ) -> Self {
            self.transcript = Some(TranscriptScan {
                dirs: dirs.iter().map(PathBuf::from).collect(),
                read: TranscriptRead {
                    incomplete: incomplete.map(str::to_string),
                    write_call: write_call.map(str::to_string),
                },
            });
            self
        }
    }

    #[async_trait::async_trait]
    impl CustodyProbe for FakeProbe {
        async fn exists(&self, path: &Path) -> Result<bool, String> {
            if self.exists_unreadable.contains(path) {
                return Err(format!("{}: permission denied", path.display()));
            }
            Ok(!self.missing.contains(path))
        }
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
        async fn reaped_allocation_state(&self, tree: &Path) -> MemberState {
            self.calls
                .lock()
                .unwrap()
                .push(format!("reaped:{}", tree.display()));
            self.reaped.get(tree).cloned().unwrap_or_else(|| {
                MemberState::ProbeFailed(format!("{}: no branch found", tree.display()))
            })
        }
        async fn transcript_scan(&self, _claude_session_id: &str) -> TranscriptScan {
            self.transcript.clone().unwrap_or(TranscriptScan {
                dirs: Vec::new(),
                read: TranscriptRead {
                    incomplete: None,
                    write_call: None,
                },
            })
        }
    }

    fn context(dir: Option<&str>, isolated: &[&str]) -> SessionCustodyContext {
        SessionCustodyContext {
            session_dir: dir.map(PathBuf::from),
            isolated_worktrees: Ok(isolated.iter().map(PathBuf::from).collect()),
        }
    }

    /// A live attributed worktree.
    fn live(path: &str) -> AttributedWorktree {
        AttributedWorktree {
            path: PathBuf::from(path),
            retired: false,
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
        let proven = FakeProbe::default().transcript(&["/ws"], None, None);
        let none = gather_custody(&proven, &context(Some("/ws"), &[]), Ok(vec![]), "s").await;
        assert!(none.members.is_empty());
        assert!(
            custody_gate(&none).is_clean(),
            "nothing attributed and a proven no-write transcript → clean"
        );

        let one = gather_custody(
            &probe,
            &context(Some("/ws"), &[]),
            Ok(vec![live("/ws/agent-worktrees/a/repo")]),
            "s",
        )
        .await;
        assert_eq!(one.members.len(), 1);
        assert_eq!(custody_gate(&one).as_str(), "dirty");
    }

    /// D6: a worktree the transcript names is judged like any other member —
    /// here the tree a workspace-root session edited by absolute path, whose
    /// edited subdirectory is gone, so it resolves through its nearest
    /// existing ancestor.
    #[tokio::test]
    async fn a_transcript_edit_target_in_a_dirty_worktree_is_a_member() {
        let mut probe = FakeProbe::default()
            .root(
                "/ws/agent-worktrees/b/repo/src",
                "/ws/agent-worktrees/b/repo",
            )
            .state(
                "/ws/agent-worktrees/b/repo",
                MemberState::Probed {
                    dirty: true,
                    ahead: 0,
                },
            )
            .transcript(
                &["/ws", "/ws/agent-worktrees/b/repo/src/gone"],
                None,
                Some("/p/s.jsonl line 3 calls \"Edit\""),
            );
        probe
            .missing
            .insert(PathBuf::from("/ws/agent-worktrees/b/repo/src/gone"));
        let inputs = gather_custody(&probe, &context(Some("/ws"), &[]), Ok(vec![]), "s").await;
        assert_eq!(
            inputs
                .members
                .iter()
                .map(|m| m.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/ws/agent-worktrees/b/repo"]
        );
        assert_eq!(inputs.covered, vec!["/ws/agent-worktrees/b/repo"]);
        assert_eq!(
            custody_gate(&inputs),
            CustodyVerdict::Dirty(CustodyDirty::Uncommitted {
                path: "/ws/agent-worktrees/b/repo".to_string()
            })
        );
    }

    /// D2/D6: a workspace-root session whose every source names no tree —
    /// its transcript names only the root and a scratch dir, both in no
    /// repository — is `Clean` only on the transcript's no-write proof.
    #[tokio::test]
    async fn a_workspace_root_session_that_names_no_tree_is_clean_only_on_proof() {
        let ctx = context(Some("/ws"), &[]);
        let dirs = ["/ws", "/home/u/.qontinui/scratch/s"];
        let absent = FakeProbe::default().transcript(
            &[],
            Some("no transcript for session s under any of 3 account root(s)"),
            None,
        );
        let unread = gather_custody(&absent, &ctx, Ok(vec![]), "s").await;
        assert_eq!(
            custody_gate(&unread).reason(),
            Some("no_tree_unproven"),
            "no transcript"
        );

        let bash =
            FakeProbe::default().transcript(&dirs, None, Some("/p/s.jsonl line 9 calls \"Bash\""));
        let bashed = gather_custody(&bash, &ctx, Ok(vec![]), "s").await;
        assert!(bashed.members.is_empty() && bashed.covered.is_empty());
        assert_eq!(custody_gate(&bashed).reason(), Some("no_tree_unproven"));

        let proven = FakeProbe::default().transcript(&dirs, None, None);
        let clean = gather_custody(&proven, &ctx, Ok(vec![]), "s").await;
        assert_eq!(custody_gate(&clean), CustodyVerdict::Clean);
    }

    /// D10: a primary checkout the transcript names as a `cwd` (a read-only
    /// `cd` leaves one) is that checkout, a member, and keeps the session in
    /// custody.
    #[tokio::test]
    async fn a_primary_checkout_among_the_transcript_cwds_is_a_shared_checkout() {
        let probe = FakeProbe::default()
            .root("/ws/agent-worktrees/a/repo", "/ws/agent-worktrees/a/repo")
            .root("/ws/qontinui-runner", "/ws/qontinui-runner")
            .state("/ws/qontinui-runner", MemberState::PrimaryCheckout)
            .transcript(&["/ws", "/ws/qontinui-runner"], None, None);
        let inputs = gather_custody(
            &probe,
            &context(Some("/ws"), &[]),
            Ok(vec![live("/ws/agent-worktrees/a/repo")]),
            "s",
        )
        .await;
        assert_eq!(
            custody_gate(&inputs),
            CustodyVerdict::Dirty(CustodyDirty::SharedCheckout {
                path: "/ws/qontinui-runner".to_string()
            })
        );
    }

    const REAPED_TREE: &str = "/ws/agent-worktrees/gone/repo";

    /// A workspace-root-repo box (`/ws` is a primary) whose session's
    /// transcript names two directories under the reaped allocation tree
    /// [`REAPED_TREE`], and what that tree's branch reads (`None` =
    /// unreadable).
    fn reaped_allocation_probe(branch: Option<MemberState>) -> FakeProbe {
        let mut probe = FakeProbe::default()
            .root("/ws", "/ws")
            .root("/ws/agent-worktrees", "/ws")
            .state("/ws", MemberState::PrimaryCheckout)
            .transcript(
                &[
                    "/ws/agent-worktrees/gone/repo/src",
                    "/ws/agent-worktrees/gone/repo/docs",
                ],
                None,
                Some("/p/s.jsonl line 3 calls \"Edit\""),
            );
        for p in [
            "/ws/agent-worktrees/gone",
            REAPED_TREE,
            "/ws/agent-worktrees/gone/repo/src",
            "/ws/agent-worktrees/gone/repo/docs",
        ] {
            probe.missing.insert(PathBuf::from(p));
        }
        if let Some(state) = branch {
            probe.reaped.insert(PathBuf::from(REAPED_TREE), state);
        }
        probe
    }

    /// D11: a transcript directory under an allocation that is gone is that
    /// reaped tree, covered and judged ONCE — never walked up into the
    /// repository that encloses `agent-worktrees/`, which here is a primary
    /// and would read `shared_checkout`.
    #[tokio::test]
    async fn a_transcript_path_under_a_reaped_allocation_is_never_its_enclosing_repo() {
        let landed = reaped_allocation_probe(Some(MemberState::Probed {
            dirty: false,
            ahead: 0,
        }));
        let ctx = context(Some("/home/u/scratch"), &[]);
        let inputs = gather_custody(&landed, &ctx, Ok(vec![]), "s").await;
        assert_eq!(inputs.covered, vec![REAPED_TREE]);
        assert_eq!(
            inputs
                .members
                .iter()
                .map(|m| m.path.as_str())
                .collect::<Vec<_>>(),
            vec![REAPED_TREE]
        );
        assert_eq!(custody_gate(&inputs), CustodyVerdict::Clean);
        let calls = landed.calls();
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("root:/ws/agent-worktrees")),
            "{calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("reaped:")).count(),
            1,
            "{calls:?}"
        );
    }

    /// D11 refined: the reaped tree's branch survives the reap. Commits on no
    /// remote read unpushed; a branch that cannot be read is unknown.
    #[tokio::test]
    async fn a_reaped_allocation_is_judged_by_the_branch_it_left() {
        let ctx = context(Some("/home/u/scratch"), &[]);
        let stranded = reaped_allocation_probe(Some(MemberState::Probed {
            dirty: false,
            ahead: 2,
        }));
        let inputs = gather_custody(&stranded, &ctx, Ok(vec![]), "s").await;
        assert_eq!(
            custody_gate(&inputs),
            CustodyVerdict::Dirty(CustodyDirty::Unpushed {
                path: REAPED_TREE.to_string(),
                ahead: 2
            })
        );

        let unreadable = reaped_allocation_probe(None);
        let inputs = gather_custody(&unreadable, &ctx, Ok(vec![]), "s").await;
        assert_eq!(custody_gate(&inputs).reason(), Some("probe_failed"));
    }

    #[test]
    fn the_allocation_tree_is_found_by_path_alone() {
        let tree = |p: &str| allocation_tree(Path::new(p)).map(|t| t.display().to_string());
        assert_eq!(
            tree("/ws/agent-worktrees/01a1/qontinui-runner/src-tauri/src").as_deref(),
            Some("/ws/agent-worktrees/01a1/qontinui-runner")
        );
        assert_eq!(
            tree("/ws/agent-worktrees/01a1").as_deref(),
            Some("/ws/agent-worktrees/01a1")
        );
        assert_eq!(
            tree("/ws/qontinui-worktrees/01a1/qontinui-web/backend").as_deref(),
            Some("/ws/qontinui-worktrees/01a1/qontinui-web")
        );
        assert_eq!(tree("/ws/agent-worktrees/.siblings/schemas@abc/rust"), None);
        assert_eq!(tree("/ws/agent-worktrees"), None);
        assert_eq!(tree("/ws/qontinui-runner/src"), None);
    }

    #[test]
    fn a_reaped_tree_names_its_primary_and_its_allocation_id() {
        let tree =
            Path::new("/ws/agent-worktrees/01a1fd70-7e45-7001-ab8b-bf23c7372864/qontinui-runner");
        assert_eq!(
            reaped_allocation_origin(tree),
            Ok((
                PathBuf::from("/ws/qontinui-runner"),
                "01a1fd707e45".to_string()
            ))
        );
        assert!(reaped_allocation_origin(Path::new("/ws/agent-worktrees/01a1")).is_err());
        assert!(reaped_allocation_origin(Path::new("/ws/agent-worktrees/xyz/repo")).is_err());
        assert!(reaped_allocation_origin(Path::new("/ws/other/01a1fd70-7e45-7001/repo")).is_err());
    }

    #[test]
    fn only_a_branch_carrying_the_allocation_id_is_its_branch() {
        let id12 = "01a1fd707e45";
        assert!(allocation_branch_matches(
            "refs/heads/agent/eb2155ed4152-01a1fd707e45/2026-11-20-triage",
            id12
        ));
        assert!(!allocation_branch_matches(
            "refs/heads/agent/eb2155ed4152-01a1fd707e46/2026-11-20-triage",
            id12
        ));
        assert!(!allocation_branch_matches(
            "refs/heads/agent/eb2155ed4152-01a1fd707e45",
            id12
        ));
        assert!(!allocation_branch_matches(
            "refs/heads/feat/x-01a1fd707e45/y",
            id12
        ));
    }

    /// Re-plan 2026-11-21: a transcript that could not be read whole leaves
    /// a session UNKNOWN even when every tree it did name is clean.
    #[tokio::test]
    async fn an_unreadable_transcript_is_unknown_beside_a_clean_worktree() {
        let probe = FakeProbe::default()
            .root("/ws/wt/a", "/ws/wt/a")
            .transcript(
                &["/ws/wt/a"],
                Some("/p/s.jsonl line 40 is not a record: EOF while parsing"),
                None,
            );
        let inputs = gather_custody(&probe, &context(Some("/ws/wt/a"), &[]), Ok(vec![]), "s").await;
        assert_eq!(
            custody_gate(&inputs).reason(),
            Some("transcript_unreadable")
        );
    }

    /// A transcript directory whose existence cannot be checked is a failed
    /// probe, never "in no repository".
    #[tokio::test]
    async fn an_unanswerable_transcript_directory_is_a_failed_probe() {
        let mut probe = FakeProbe::default().transcript(&["/mnt/stale/wt/src"], None, None);
        probe
            .exists_unreadable
            .insert(PathBuf::from("/mnt/stale/wt/src"));
        let inputs = gather_custody(&probe, &context(Some("/ws"), &[]), Ok(vec![]), "s").await;
        assert_eq!(custody_gate(&inputs).reason(), Some("probe_failed"));
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
            Ok(vec![live("/ws/wt/a")]),
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
            Ok(vec![live("/ws/gone")]),
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

    /// Review item 7b: a RETIRED ledger row (merged or abandoned) whose path
    /// is gone was reaped and is skipped; a LIVE row whose path is gone, and a
    /// retired row whose path still exists, are both still judged.
    #[tokio::test]
    async fn a_reaped_retired_worktree_is_skipped_and_a_missing_live_one_is_unknown() {
        let mut probe = FakeProbe::default().root("/ws/kept", "/ws/kept");
        probe.missing.insert(PathBuf::from("/ws/reaped"));
        probe.missing.insert(PathBuf::from("/ws/lost"));
        probe.unreadable.insert(PathBuf::from("/ws/lost"));
        let retired = |p: &str| AttributedWorktree {
            path: PathBuf::from(p),
            retired: true,
        };

        let reaped = gather_custody(
            &probe,
            &context(Some("/ws"), &[]),
            Ok(vec![retired("/ws/reaped"), retired("/ws/kept")]),
            "s",
        )
        .await;
        assert_eq!(
            reaped
                .members
                .iter()
                .map(|m| m.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/ws/kept"],
            "the reaped row contributes nothing; the retired row still on disk is probed"
        );
        assert!(custody_gate(&reaped).is_clean());

        // Round-2 suggestion 5: a retired row whose existence cannot be read
        // is not "reaped" — it is a failed probe.
        probe.exists_unreadable.insert(PathBuf::from("/ws/denied"));
        let denied = gather_custody(
            &probe,
            &context(Some("/ws"), &[]),
            Ok(vec![retired("/ws/denied")]),
            "s",
        )
        .await;
        assert_eq!(custody_gate(&denied).reason(), Some("probe_failed"));

        let lost = gather_custody(
            &probe,
            &context(Some("/ws"), &[]),
            Ok(vec![live("/ws/lost")]),
            "s",
        )
        .await;
        assert_eq!(custody_gate(&lost).reason(), Some("probe_failed"));
    }

    /// Review item 8: an isolated-edit context that cannot be READ is not "no
    /// isolated worktrees".
    #[tokio::test]
    async fn an_unreadable_isolated_edit_context_is_unknown() {
        let probe = FakeProbe::default().root("/ws/wt/a", "/ws/wt/a");
        let mut ctx = context(Some("/ws/wt/a"), &[]);
        ctx.isolated_worktrees = Err("isolated-edit context lock poisoned".to_string());
        let inputs = gather_custody(&probe, &ctx, Ok(vec![]), "s").await;
        assert_eq!(custody_gate(&inputs).reason(), Some("ownership_unreadable"));
        assert!(probe.calls().is_empty(), "{:?}", probe.calls());
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

    /// coord's keyed answer for `asked`: `sessions` is the raw `sessions[]`
    /// JSON array, every asked id is echoed, and `unknown` lists the ids coord
    /// resolved to no session row.
    fn keyed_ownership(asked: &[&str], sessions: &str, unknown: &[&str]) -> OwnershipRead {
        let echo: Vec<String> = asked.iter().map(|s| format!("\"{s}\"")).collect();
        let unknown: Vec<String> = unknown
            .iter()
            .map(|s| format!(r#"{{"claudeCodeSessionId":"{s}","worktrees":[]}}"#))
            .collect();
        let json = format!(
            r#"{{"sessionBridgeColumnPresent":true,"inputTruncated":false,"invalid":[],
                "claudeCodeSessionIds":[{}],"unknown":[{}],"sessions":{sessions}}}"#,
            echo.join(","),
            unknown.join(",")
        );
        custody::coord::ownership_from_keyed(vec![(
            asked.iter().map(|s| s.to_string()).collect(),
            Ok(serde_json::from_str(&json).unwrap()),
        )])
    }

    #[test]
    fn attributed_worktrees_are_anchored_on_the_workspace_root() {
        let own = keyed_ownership(
            &["claude-x", "claude-empty"],
            r#"[{"sessionId":"c-1","claudeCodeSessionId":"claude-x","ownerSessionState":"active",
                "worktrees":[
                {"worktreePath":"agent-worktrees/a/repo","repo":"repo","ledgerStatus":"active"},
                {"worktreePath":"/abs/wt","repo":"repo2","ledgerStatus":"merged"},
                {"worktreePath":"/abs/old","repo":"repo3","ledgerStatus":"abandoned"},
                {"worktreePath":"/abs/new","repo":"repo4"}]},
               {"sessionId":"c-other","claudeCodeSessionId":"claude-empty",
                "ownerSessionState":"closed","worktrees":[]}]"#,
            &[],
        );
        assert_eq!(
            attributed_worktrees(&own, "claude-x", Some(Path::new("/ws"))).unwrap(),
            vec![
                live("/ws/agent-worktrees/a/repo"),
                AttributedWorktree {
                    path: PathBuf::from("/abs/wt"),
                    retired: true
                },
                AttributedWorktree {
                    path: PathBuf::from("/abs/old"),
                    retired: true
                },
                // No ledger status is not "retired".
                live("/abs/new"),
            ]
        );
        let err = attributed_worktrees(&own, "claude-x", None).unwrap_err();
        assert!(
            err.contains("is relative") && err.contains("workspace root"),
            "a relative ledger path with no root to anchor it is not guessed: {err}"
        );
        assert_eq!(
            attributed_worktrees(&own, "CLAUDE-X", Some(Path::new("/ws")))
                .unwrap()
                .len(),
            4,
            "the Claude session id matches case-insensitively"
        );
        assert_eq!(
            attributed_worktrees(&own, "claude-empty", None).unwrap(),
            Vec::<AttributedWorktree>::new(),
            "an entry of its own with `worktrees: []` is positively empty, whatever its state"
        );
    }

    /// The pane-id fallback is gone: coord's keyed read answers the Claude
    /// session `unknown` while the pane's own (differing) coord id names a
    /// row holding nothing. Under the old fallback that read `[]` — a false
    /// Clean. Now it is Unknown, end to end.
    #[tokio::test]
    async fn a_differing_pane_coord_id_never_reads_clean() {
        let own = keyed_ownership(
            &["claude-x", "claude-y"],
            r#"[{"sessionId":"c-pane","claudeCodeSessionId":"claude-y","worktrees":[]}]"#,
            &["claude-x"],
        );
        let err = attributed_worktrees(&own, "claude-x", Some(Path::new("/ws"))).unwrap_err();
        assert!(err.contains("`unknown`"), "{err}");
        let inputs = gather_custody(
            &FakeProbe::default().root("/ws/wt/a", "/ws/wt/a"),
            &context(Some("/ws/wt/a"), &[]),
            attributed_worktrees(&own, "claude-x", Some(Path::new("/ws"))),
            "s",
        )
        .await;
        assert_eq!(custody_gate(&inputs).reason(), Some("ownership_unreadable"));
    }

    /// The positive mirror: the Claude session's OWN entry (`c-real`, holding
    /// a dirty worktree) answers it, never another entry that holds nothing
    /// (`c-pane`, the id a pane might carry). Custody reads the dirt.
    #[tokio::test]
    async fn a_session_resolves_through_its_own_entry_not_an_empty_one() {
        let own = keyed_ownership(
            &["claude-x", "claude-y"],
            r#"[{"sessionId":"c-real","claudeCodeSessionId":"claude-x",
                 "worktrees":[{"worktreePath":"/ws/wt/dirty","repo":"r"}]},
                {"sessionId":"c-pane","claudeCodeSessionId":"claude-y","worktrees":[]}]"#,
            &[],
        );
        let attributed = attributed_worktrees(&own, "claude-x", Some(Path::new("/ws")));
        assert_eq!(attributed, Ok(vec![live("/ws/wt/dirty")]));
        let probe = FakeProbe::default()
            .root("/ws/wt/a", "/ws/wt/a")
            .root("/ws/wt/dirty", "/ws/wt/dirty")
            .state(
                "/ws/wt/dirty",
                MemberState::Probed {
                    dirty: true,
                    ahead: 0,
                },
            );
        let inputs = gather_custody(&probe, &context(Some("/ws/wt/a"), &[]), attributed, "s").await;
        assert_eq!(custody_gate(&inputs).as_str(), "dirty");
    }

    #[test]
    fn attributed_worktrees_fail_closed_without_a_read_or_an_entry() {
        let own = keyed_ownership(&[], "[]", &[]);
        let err = attributed_worktrees(&own, "claude-x", Some(Path::new("/ws"))).unwrap_err();
        assert!(err.contains("no entry"), "{err}");
        assert!(
            attributed_worktrees(&Ok(CoordOwnership::default()), "claude-x", None).is_err(),
            "an index that answered nothing answers nothing"
        );
        let failed: OwnershipRead = Err("coord returned 503".to_string());
        assert_eq!(
            attributed_worktrees(&failed, "claude-x", Some(Path::new("/ws"))),
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

    /// D11 through the SHIPPED probe and a real git: a reaped allocation's
    /// branch with nothing off the remotes reads clean, one commit on no
    /// remote reads unpushed, and no branch carrying the id is a failed probe.
    #[tokio::test]
    async fn the_git_probe_judges_a_reaped_allocation_by_its_surviving_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let origin = tmp.path().join("origin.git");
        let primary = ws.join("repo");
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
        let branch = "agent/eb2155ed4152-01a1fd707e45/2026-11-20-x";
        git(&primary, &["branch", branch, "origin/main"]);
        let tree = ws
            .join("agent-worktrees")
            .join("01a1fd70-7e45-7001-ab8b-bf23c7372864")
            .join("repo");

        let probe = GitCustodyProbe::default();
        assert_eq!(
            probe.reaped_allocation_state(&tree).await,
            MemberState::Probed {
                dirty: false,
                ahead: 0
            }
        );

        git(&primary, &["checkout", "-q", branch]);
        std::fs::write(primary.join("NEW"), "y").unwrap();
        git(&primary, &["add", "NEW"]);
        git(&primary, &["commit", "-q", "-m", "local only"]);
        git(&primary, &["checkout", "-q", "main"]);
        assert_eq!(
            probe.reaped_allocation_state(&tree).await,
            MemberState::Probed {
                dirty: false,
                ahead: 1
            }
        );

        let other = ws
            .join("agent-worktrees")
            .join("99999999-0000-4000-8000-000000000000")
            .join("repo");
        assert!(matches!(
            probe.reaped_allocation_state(&other).await,
            MemberState::ProbeFailed(_)
        ));
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

        // Round-2 WARNING 3: an upstream that is a LOCAL branch proves nothing
        // was pushed. `--track main` with a local-only commit: `main..HEAD`
        // would read 0 — the probe must count commits on no remote ref.
        git(&linked, &["switch", "-q", "--detach", "origin/main"]);
        git(
            &linked,
            &["branch", "-q", "-f", "local-base", "origin/main"],
        );
        git(
            &linked,
            &[
                "switch",
                "-q",
                "-c",
                "tracks-local",
                "--track",
                "local-base",
            ],
        );
        std::fs::write(linked.join("tracked-local.txt"), "mine").unwrap();
        git(&linked, &["add", "tracked-local.txt"]);
        git(
            &linked,
            &["commit", "-q", "-m", "local only, local upstream"],
        );
        git(&linked, &["branch", "-q", "-f", "local-base", "HEAD"]);
        assert_eq!(
            probe.member_state(&linked).await,
            one_unpushed,
            "`local-base..HEAD` is 0, but the commit is on no remote"
        );

        // Review item 7a: an upstream whose remote-tracking ref was pruned (its
        // branch deleted on the remote) falls back to "on any remote ref".
        git(&linked, &["switch", "-q", "-c", "pruned", "origin/main"]);
        git(&linked, &["push", "-q", "-u", "origin", "pruned"]);
        git(&linked, &["push", "-q", "origin", "--delete", "pruned"]);
        git(&linked, &["fetch", "-q", "--prune", "origin"]);
        assert_eq!(
            probe.member_state(&linked).await,
            clean,
            "pruned upstream, HEAD on origin/main's commit"
        );
        std::fs::write(linked.join("pruned.txt"), "mine").unwrap();
        git(&linked, &["add", "pruned.txt"]);
        git(&linked, &["commit", "-q", "-m", "after the prune"]);
        assert_eq!(probe.member_state(&linked).await, one_unpushed);

        // A peer holding `index.lock` in the tree does not fail the probe: it
        // runs `--no-optional-locks`, so it never needs that lock.
        let git_dir = std::fs::read_to_string(linked.join(".git")).unwrap();
        let admin = PathBuf::from(git_dir.trim().trim_start_matches("gitdir:").trim());
        std::fs::write(admin.join("index.lock"), "").unwrap();
        assert_eq!(probe.member_state(&linked).await, one_unpushed);
        std::fs::remove_file(admin.join("index.lock")).unwrap();

        // The primary clone's `.git` is a directory.
        assert_eq!(
            probe.member_state(&primary).await,
            MemberState::PrimaryCheckout
        );

        // And outside any repository there is no root at all.
        let outside = tempfile::tempdir().unwrap();
        assert_eq!(probe.worktree_root(outside.path()).await.unwrap(), None);
    }

    #[test]
    fn pending_message_rows_keep_every_row_and_refuse_a_malformed_one() {
        let body = serde_json::json!({"messages": [
            {"message_id": "m1", "to_session": "s1", "kind": "ci_rework", "priority": "blocking"},
            {"message_id": "m2", "to_session": "0192aaaa-0000-7000-8000-000000000001"},
        ]});
        assert_eq!(
            pending_message_rows(&body).unwrap(),
            vec![
                ("m1".to_string(), "s1".to_string()),
                (
                    "m2".to_string(),
                    "0192aaaa-0000-7000-8000-000000000001".to_string()
                ),
            ]
        );
        assert!(pending_message_rows(&serde_json::json!({"messages": []}))
            .unwrap()
            .is_empty());
        assert!(pending_message_rows(&serde_json::json!({"error": "x"})).is_err());
        assert!(
            pending_message_rows(&serde_json::json!({"messages": [{"message_id": "m"}]})).is_err(),
            "a row with no recipient cannot be ruled out as this session's"
        );
        // The refusal names the row by id and never quotes it.
        let err = pending_message_rows(&serde_json::json!({"messages": [
            {"message_id": "m9", "body": "private words", "to_session": null},
        ]}))
        .unwrap_err();
        assert!(err.contains("m9") && err.contains("to_session"), "{err}");
        assert!(!err.contains("private words"), "{err}");
    }

    // ── Obligations read (finished_close, remote half of `closeable`) ────

    #[test]
    fn the_obligations_url_is_the_http_twin_with_the_id_encoded() {
        assert_eq!(
            obligations_url(
                "https://coord.example/",
                "0a1b2c3d-0000-4000-8000-000000000001"
            ),
            "https://coord.example/coord/overlord/session-obligations/0a1b2c3d-0000-4000-8000-000000000001"
        );
        assert_eq!(
            obligations_url("http://h", " a/b "),
            "http://h/coord/overlord/session-obligations/a%2Fb"
        );
    }

    fn plain_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    /// Serve one canned HTTP response on a loopback port; return its URL.
    async fn serve_once(response: String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://127.0.0.1:{}/coord/overlord/session-obligations/s",
            listener.local_addr().unwrap().port()
        );
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
        url
    }

    fn http_ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// The whole remote half over a real socket: the transport and the gate,
    /// for an answer that is clean, one that is unwatched, and the three ways a
    /// read fails (timeout, non-2xx, undecodable body) — none of which may
    /// read as "watched".
    #[tokio::test]
    async fn the_read_and_the_gate_together_never_read_a_failed_read_as_watched() {
        let watched = r#"{"all_tracked_sources_read":true,"session_proven_in_tenant":true,
            "prs":{"coverage":"read","items":[]},
            "gates":{"coverage":"read","items":[{"item":{"gate_id":"g"},"verdict":"watched"}]}}"#;
        let url = serve_once(http_ok(watched)).await;
        let read = read_obligations(plain_client().get(&url), &url, Duration::from_secs(5)).await;
        assert_eq!(
            wind_down::obligations_gate(&read),
            wind_down::ObligationsVerdict::Watched { judged: 1 }
        );

        let owed = watched.replace("\"watched\"}]", "\"not_watched\"}]");
        let url = serve_once(http_ok(&owed)).await;
        let read = read_obligations(plain_client().get(&url), &url, Duration::from_secs(5)).await;
        assert_eq!(wind_down::obligations_gate(&read).as_str(), "unwatched");

        let url = serve_once(
            "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 4\r\nconnection: close\r\n\r\nbusy"
                .to_string(),
        )
        .await;
        let read = read_obligations(plain_client().get(&url), &url, Duration::from_secs(5)).await;
        let err = read.as_ref().unwrap_err();
        assert!(err.contains("503") && err.contains("busy"), "{err}");
        let verdict = wind_down::obligations_gate(&read);
        assert_eq!(verdict.reason(), Some("obligations_unreadable"));
        assert!(!verdict.is_watched());

        let url = serve_once(http_ok("not-json")).await;
        let read = read_obligations(plain_client().get(&url), &url, Duration::from_secs(5)).await;
        assert!(
            read.as_ref().unwrap_err().starts_with("decode GET"),
            "{read:?}"
        );
        assert!(!wind_down::obligations_gate(&read).is_watched());

        // Bound and never answered: the request times out.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/x",
            listener.local_addr().unwrap().port()
        );
        let read =
            read_obligations(plain_client().get(&url), &url, Duration::from_millis(200)).await;
        let err = read.as_ref().unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        let verdict = wind_down::obligations_gate(&read);
        assert_eq!(verdict.as_str(), "unknown");
        assert!(!verdict.is_watched());
        drop(listener);
    }
}
