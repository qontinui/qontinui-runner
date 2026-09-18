//! Wind-down EXECUTOR — the tick that actually closes a drained runner's
//! finished, idle sessions (plan `2026-09-13-drained-runner-never-reaches-idle`,
//! Phase 4; design decisions D4, D5, D6).
//!
//! Phase 1 built the two halves and wired neither: the pure verdict
//! ([`qontinui_runner_lib::wind_down::eligibility`]) and the closing primitive
//! (`TerminalManager::graceful_exit`). This module is what joins them, and it
//! is the ONLY place in the runner that closes a session because of a drain.
//!
//! ## What one tick does
//!
//! Every [`TICK`], and **only while [`CoordDrainState::Drained`]**:
//!
//! * **Terminal-hosted sessions** — `Eligible` past grace → `graceful_exit`.
//!   The outcome (`closed` / `exit_stuck` / `close_refused`) is recorded on the
//!   lifecycle record, and each close is logged with the session id, the
//!   record's origin and the eligibility inputs the verdict was reached on.
//! * **Looping agents** — the idle-past-grace tab → `graceful_exit` (D6). The
//!   loop DEFINITION is untouched, so undrain brings the agent back as a
//!   `FirstSpawn` on the supervisor's next tick; nothing here re-registers it.
//! * **Stewards** — idle-past-grace → `graceful_exit`, and the kind goes into
//!   the persisted `stopped_by_drain` set ([`StoppedByDrain`]).
//!
//! On the transition back to a state that ALLOWS autonomous spawns, the tick
//! restarts exactly the `stopped_by_drain` kinds and clears the set. The
//! deferred boot restore and resume need nothing from this module: Phase 3 left
//! them parked on `coord_drain_state::wait_until_allowed`, which the same state
//! change wakes, and the looping supervisor resumes `FirstSpawn` by itself once
//! its drain rewrite stops firing.
//!
//! **`Unknown` does NEITHER.** It is not a drain (so nothing is wound down) and
//! it is not an undrain (so nothing is restarted). A state that could not be
//! read is not evidence for either action — served policy
//! `verification-and-evidence` `unknown-must-not-render-as-a-default`.
//!
//! ## What this module must never do
//!
//! It reaches a pane through `TerminalManager::graceful_exit` and through
//! nothing else. `TerminalManager::close` — the kill path every other closer
//! uses — is not called from here, so no `io.kill` can land on a live `claude`
//! (D5). [`tests::the_executor_never_reaches_a_kill_path`] pins that by
//! scanning this file's own source.
//!
//! ## Hazard 1: the grace window is WALL-CLOCK
//!
//! Every clock the verdict folds is wall-clock: the sideband's `set_at_ms`, the
//! grid tracker's `observed_at_ms` (`chrono::Utc::now()` at
//! `terminal/session.rs`), coord's `finished_at`, and `eligibility`'s own
//! `now_ms`. A forward step of ≥ grace — an NTP correction, a laptop resume —
//! therefore promotes a pane to `Eligible` without it having been idle that
//! long; a backward step freezes the tracker. Phase 1 could tolerate this
//! because it only reported.
//!
//! **Chosen: DETECT AND REJECT, not a monotonic interval.** A monotonic grace
//! interval is not available here, because two of the three clocks the window
//! is built from are not ours to convert — coord's `finished_at` arrives as a
//! wall-clock instant over the wire, and a pane's idle window is carried across
//! observations as wall-clock millis. Re-basing them on `Instant` would mean
//! re-deriving instants we never measured. So the executor measures the SKEW
//! instead ([`ClockJumpGuard`]): each tick it compares the monotonic elapsed
//! time against the wall-clock delta, and a disagreement past
//! [`CLOCK_SKEW_TOLERANCE`] quarantines wind-down for a full grace period —
//! long enough for every idle window in play to have been re-established under
//! a trustworthy clock. It fails CLOSED in both directions: a jump forwards or
//! backwards stops closes, never starts them.
//!
//! ## Hazard 2: the relaunch window at the close
//!
//! Closed one level down, in `TerminalManager::graceful_exit`, which re-proves
//! the pane clear immediately before removing it and refuses otherwise — see
//! `terminal::graceful_exit::CloseTabResult`. A refusal arrives here as
//! [`WIND_DOWN_CLOSE_REFUSED`] and is recorded like any other outcome.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use crate::coord_drain_state::CoordDrainState;
use crate::session::session_lifecycle_store::{
    SessionLifecycleStore, WIND_DOWN_CLOSED, WIND_DOWN_CLOSE_REFUSED, WIND_DOWN_CLOSE_UNKNOWN,
    WIND_DOWN_EXIT_STUCK, WIND_DOWN_NOT_ATTEMPTED,
};
use crate::session::wind_down_observer;
use crate::terminal::graceful_exit::GracefulExitOutcome;
use crate::terminal::TerminalManager;
use qontinui_runner_lib::wind_down::{self, SessionKind};

/// How often the executor looks (plan Phase 4).
pub const TICK: Duration = Duration::from_secs(30);

/// Boot-settle delay before the first tick, matching the looping supervisor's.
/// A runner that has just started has no trustworthy idle windows anyway.
pub const BOOT_SETTLE_DELAY: Duration = Duration::from_secs(60);

/// Deadline handed to each `graceful_exit` — the primitive's own default.
pub const EXIT_DEADLINE: Duration = crate::terminal::graceful_exit::DEFAULT_DEADLINE;

/// Closes attempted per tick. Each one can wait up to [`EXIT_DEADLINE`], so an
/// unbounded batch would make one tick outlast many; the rest come back on the
/// next tick, which is the whole point of a tick.
pub const MAX_CLOSES_PER_TICK: usize = 4;

/// Wall-clock/monotonic disagreement past which this tick's idle windows are
/// not trusted (hazard 1). Generous enough for timer granularity and a late
/// tick — lateness moves BOTH clocks — and far below the 10 min grace.
pub const CLOCK_SKEW_TOLERANCE: Duration = Duration::from_secs(5);

/// Environment kill switch: exactly `0` disables the executor on this machine.
/// Absent, `1`, or anything else leaves it on.
pub const ENABLE_ENV: &str = "QONTINUI_WIND_DOWN_EXECUTOR";

/// Is the executor armed on this machine? See [`ENABLE_ENV`].
pub fn enabled() -> bool {
    std::env::var(ENABLE_ENV).ok().as_deref() != Some("0")
}

// ---------------------------------------------------------------------------
// The pure core
// ---------------------------------------------------------------------------

/// What one tick should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickAction {
    /// Drained: look for eligible sessions and close them.
    WindDown,
    /// Autonomous spawns are allowed again after a drain: restart what the
    /// drain stopped.
    Undrain,
    /// Do nothing at all — including while `Unknown`.
    Nothing,
}

/// PURE: decide one tick from the drain state and whether this runner still
/// OWES an undrain — a drain it has seen (or, across a restart, one whose
/// stopped-by-drain set it inherited) that has not yet been undone.
///
/// A `Drained` whose `until` has already passed is over, and is read as
/// allowing — the same rule `coord_drain_state::gate_for_at` applies, so the
/// executor and the spawn gate can never disagree about an expired drain.
///
/// `NotEnrolled` sits with `Clear`: both ALLOW autonomous spawns, so if this
/// runner was wound down and is now un-drained by either route, what the drain
/// stopped is owed a restart. `Unknown` is neither, and does nothing.
pub fn decide_tick(state: &CoordDrainState, owes_undrain: bool, now: DateTime<Utc>) -> TickAction {
    let allows = match state {
        CoordDrainState::Clear | CoordDrainState::NotEnrolled { .. } => true,
        CoordDrainState::Drained { until: Some(u), .. } => *u <= now,
        CoordDrainState::Drained { .. } => false,
        // Not a drain and not an undrain. Never an action.
        CoordDrainState::Unknown { .. } => return TickAction::Nothing,
    };
    match (allows, owes_undrain) {
        (false, _) => TickAction::WindDown,
        (true, true) => TickAction::Undrain,
        (true, false) => TickAction::Nothing,
    }
}

/// Wall-clock jump detector (hazard 1). Monotonic time is the reference;
/// wall-clock time is the thing under suspicion.
#[derive(Debug, Default)]
pub struct ClockJumpGuard {
    last: Option<(Instant, i64)>,
    quarantine_until: Option<Instant>,
}

/// What [`ClockJumpGuard::check`] concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockVerdict {
    /// The clocks agree and no quarantine is running.
    Trustworthy,
    /// A jump was just detected; `skew_ms` is wall-clock minus monotonic.
    Jumped { skew_ms: i64 },
    /// An earlier jump's quarantine has not expired yet.
    Quarantined,
}

impl ClockVerdict {
    pub fn trustworthy(self) -> bool {
        matches!(self, ClockVerdict::Trustworthy)
    }
}

impl ClockJumpGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one tick's clocks and say whether this tick's idle windows may be
    /// acted on. `quarantine` should be the grace period: after a jump, every
    /// window in play has to be re-established before any of them means
    /// anything. `tolerance` is the disagreement below which the two clocks
    /// count as agreeing — a parameter so the boundary is testable at values
    /// other than the shipped [`CLOCK_SKEW_TOLERANCE`].
    ///
    /// The FIRST call establishes the reference and is trustworthy. That is
    /// sound only because the guard lives for the PROCESS
    /// ([`clock_guard`]), not for one run of the loop: the windows a tick reads
    /// are carried on long-lived per-`TerminalSession` trackers and on coord's
    /// wall-clock `finished_at`, so they can be much older than the tick — but
    /// they cannot be older than this process, which created every
    /// `TerminalSession` it can see. A per-run guard would have made this false
    /// and, worse, would have dropped a live quarantine on every supervised
    /// respawn.
    pub fn check(
        &mut self,
        mono: Instant,
        wall_ms: i64,
        quarantine: Duration,
        tolerance: Duration,
    ) -> ClockVerdict {
        let previous = self.last.replace((mono, wall_ms));
        if let Some((last_mono, last_wall)) = previous {
            let mono_delta_ms =
                i64::try_from(mono.saturating_duration_since(last_mono).as_millis())
                    .unwrap_or(i64::MAX);
            let skew_ms = (wall_ms - last_wall).saturating_sub(mono_delta_ms);
            if skew_ms.unsigned_abs() > tolerance.as_millis() as u64 {
                self.quarantine_until = Some(mono + quarantine);
                return ClockVerdict::Jumped { skew_ms };
            }
        }
        match self.quarantine_until {
            Some(until) if mono < until => ClockVerdict::Quarantined,
            Some(_) => {
                self.quarantine_until = None;
                ClockVerdict::Trustworthy
            }
            None => ClockVerdict::Trustworthy,
        }
    }
}

/// The ONE clock guard, for the life of the process.
///
/// It must outlive the loop. `spawn_supervised_forever` rebuilds the loop
/// future on a panic, so a guard held in the loop's own state would come back
/// `Default` — `quarantine_until: None` — and the very next tick would read as
/// a trustworthy first call and close on exactly the windows the quarantine
/// existed to protect.
fn clock_guard() -> &'static std::sync::Mutex<ClockJumpGuard> {
    static GUARD: std::sync::OnceLock<std::sync::Mutex<ClockJumpGuard>> =
        std::sync::OnceLock::new();
    GUARD.get_or_init(|| std::sync::Mutex::new(ClockJumpGuard::new()))
}

/// [`ClockJumpGuard::check`] over the process-lifetime guard. Synchronous; the
/// lock is never held across an await.
fn check_clock(quarantine: Duration) -> ClockVerdict {
    let mut guard = match clock_guard().lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    guard.check(
        Instant::now(),
        Utc::now().timestamp_millis(),
        quarantine,
        CLOCK_SKEW_TOLERANCE,
    )
}

// ---------------------------------------------------------------------------
// The persisted stopped-by-drain set
// ---------------------------------------------------------------------------

/// Steward kinds this runner stopped BECAUSE of the drain, persisted so a
/// runner restarted mid-drain still knows what it owes on undrain (D6).
///
/// A plain sorted set of kind strings in one small JSON file beside the
/// lifecycle store, instance-scoped exactly like it. A read failure is an EMPTY
/// set, never an error: the worst case is a steward an operator restarts by
/// hand, whereas refusing to run on an unreadable file would wedge the whole
/// undrain path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoppedByDrain {
    kinds: BTreeSet<String>,
}

/// Where [`StoppedByDrain`] lives.
pub fn stopped_by_drain_path() -> std::path::PathBuf {
    crate::instance::scope_path(&qontinui_runner_lib::ambient::runner_dir_or_cwd())
        .join("wind-down-stopped-stewards.json")
}

impl StoppedByDrain {
    /// Load from `path`. An absent, unreadable or unparseable file is an empty
    /// set (see the type docs).
    pub fn load(path: &std::path::Path) -> Self {
        let kinds = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<BTreeSet<String>>(&raw).ok())
            .unwrap_or_default();
        Self { kinds }
    }

    /// Persist to `path`, atomically. A write failure is logged, never fatal.
    pub fn save(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(body) = serde_json::to_vec_pretty(&self.kinds) else {
            return;
        };
        if let Err(e) = crate::fs_atomic::atomic_write(path, &body) {
            warn!(path = %path.display(), error = %e, "wind_down_executor: could not persist the stopped-by-drain set");
        }
    }

    /// Record a kind. Returns `true` when it was not already recorded.
    pub fn insert(&mut self, kind: &str) -> bool {
        self.kinds.insert(kind.to_string())
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Take everything recorded, leaving the set empty.
    pub fn drain_all(&mut self) -> Vec<String> {
        std::mem::take(&mut self.kinds).into_iter().collect()
    }

    pub fn kinds(&self) -> impl Iterator<Item = &str> {
        self.kinds.iter().map(String::as_str)
    }
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// Start the wind-down executor. Runs under the shared task supervisor, so a
/// panicking tick respawns the loop instead of killing the feature until the
/// next runner restart.
pub fn start() {
    if !enabled() {
        info!("wind_down_executor: disabled by {ENABLE_ENV}=0 — a drained runner will not wind down on this machine");
        return;
    }
    tauri::async_runtime::spawn(async move {
        crate::mcp::task_supervisor::spawn_supervised_forever(
            "wind-down-executor",
            Duration::from_secs(5),
            Duration::from_secs(60),
            Duration::from_secs(120),
            run_loop,
        );
    });
    info!("wind_down_executor: started");
}

/// The loop body: boot-settle delay, then tick forever.
async fn run_loop() {
    tokio::time::sleep(BOOT_SETTLE_DELAY).await;
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut state = ExecutorState::default();
    loop {
        ticker.tick().await;
        tick_once(&mut state).await;
    }
}

/// What survives between ticks. The clock guard is deliberately NOT here — see
/// [`clock_guard`].
#[derive(Debug, Default)]
struct ExecutorState {
    /// Whether a drain has been seen that has not yet been undone. Deliberately
    /// NOT cleared by an `Unknown`: a drain that goes unreadable and then clear
    /// is still an undrain, and the stewards are still owed.
    was_drained: bool,
    /// Whether the durable stopped-by-drain set has been consulted yet. It
    /// answers a once-per-process question ("did a previous process leave an
    /// undrain owed?"), so reading the file on every 30 s tick would be a
    /// blocking filesystem call on a runtime worker for an answer that cannot
    /// change after the first look.
    inherited_checked: bool,
}

async fn tick_once(state: &mut ExecutorState) {
    let Some(app) = crate::tauri_app_handle::current() else {
        return; // headless / unit-test context
    };
    let drain = crate::coord_drain_state::current();
    // `was_drained` is in-memory, so a runner RESTARTED mid-drain would forget
    // what it owes and the stewards the drain stopped would never come back.
    // The persisted set is the durable half of the same fact — consulted ONCE
    // per process, because it can only tell us about an obligation inherited
    // from a previous one.
    if !state.inherited_checked {
        state.inherited_checked = true;
        if !StoppedByDrain::load(&stopped_by_drain_path()).is_empty() {
            info!(
                "wind_down_executor: a previous process left stewards stopped by a drain — \
                 an undrain is owed"
            );
            state.was_drained = true;
        }
    }
    match decide_tick(&drain, state.was_drained, Utc::now()) {
        TickAction::Nothing => {}
        TickAction::Undrain => {
            state.was_drained = false;
            undrain(&app).await;
        }
        TickAction::WindDown => {
            state.was_drained = true;
            let grace = wind_down::grace_from_env();
            match check_clock(grace) {
                ClockVerdict::Trustworthy => wind_down_once(&app, grace).await,
                // Hazard 1. Fail closed: no close is attempted on an idle
                // window measured across a clock step.
                ClockVerdict::Jumped { skew_ms } => warn!(
                    skew_ms,
                    quarantine_s = grace.as_secs(),
                    "wind_down_executor: the wall clock stepped against the monotonic clock — \
                     every idle window in play is untrustworthy, so wind-down is quarantined \
                     for a full grace period"
                ),
                ClockVerdict::Quarantined => debug!(
                    "wind_down_executor: still within the post-clock-jump quarantine — no closes"
                ),
            }
        }
    }
}

/// One wind-down pass: observe, then close what is eligible.
async fn wind_down_once(app: &tauri::AppHandle, grace: Duration) {
    use tauri::Manager;

    let fresh = wind_down_observer::fresh_pass(app, grace).await;
    let Some(pass) = fresh.pass else {
        debug!(
            unknowns = ?fresh.unknowns,
            "wind_down_executor: no census this tick — nothing is eligible, nothing closed"
        );
        return;
    };
    let Some(manager) = app
        .try_state::<Arc<TerminalManager>>()
        .map(|s| s.inner().clone())
    else {
        return;
    };
    let store = app
        .try_state::<Arc<SessionLifecycleStore>>()
        .map(|s| s.inner().clone());

    // The record per session, for the `origin` the close is logged with.
    let record_origin = |session_id: &str| -> Option<String> {
        pass.open_records
            .iter()
            .find(|r| r.claude_session_id == session_id)
            .and_then(|r| r.origin.clone())
    };

    let candidates: Vec<(String, String)> = pass
        .report
        .terminal_hosted
        .iter()
        .filter(|proc| {
            proc.wind_down
                .as_ref()
                .is_some_and(wind_down::WindDownView::is_eligible)
        })
        .filter_map(|proc| {
            let session_id = proc.session_id.clone()?;
            let terminal_id = fresh.observed.terminal_for(&session_id)?.to_string();
            Some((session_id, terminal_id))
        })
        // ONE candidate per PANE. `graceful_exit` targets a pane, not a pid, so
        // two top-level `claude` processes attributed to the same terminal
        // would spend two of the tick's budget slots on one close and the
        // second would fail on a pane that is already gone.
        .fold(Vec::new(), |mut acc: Vec<(String, String)>, entry| {
            if !acc.iter().any(|(_, tid)| tid == &entry.1) {
                acc.push(entry);
            }
            acc
        });

    if candidates.is_empty() {
        return;
    }
    info!(
        eligible = candidates.len(),
        budget = MAX_CLOSES_PER_TICK,
        "wind_down_executor: the device is drained — closing eligible sessions"
    );

    for (session_id, terminal_id) in candidates.into_iter().take(MAX_CLOSES_PER_TICK) {
        // Re-read the drain BEFORE each close, not once per tick. Each
        // `graceful_exit` can wait a full `EXIT_DEADLINE`, so a batch can run
        // for minutes — long enough for coord to lift the drain underneath it,
        // and "only while Drained" would then be false for every close after
        // the first. `was_drained` is not touched here: the tick's own
        // bookkeeping owns it, and the undrain runs on the next tick.
        if decide_tick(&crate::coord_drain_state::current(), false, Utc::now())
            != TickAction::WindDown
        {
            info!("wind_down_executor: the drain lifted mid-batch — stopping this pass");
            return;
        }
        let kind = fresh.observed.kind_for(&terminal_id);
        let observation = fresh.observed.observation_for(&terminal_id);
        let steward_kind = crate::mcp::steward::steward_kind_for_terminal(&terminal_id);
        info!(
            session_id = %session_id,
            terminal_id = %terminal_id,
            ?kind,
            origin = record_origin(&session_id).as_deref().unwrap_or("-"),
            steward_kind = steward_kind.as_deref().unwrap_or("-"),
            sideband = ?observation.sideband,
            grid = ?observation.grid,
            judged_at_ms = fresh.observed.now_ms,
            grace_s = grace.as_secs(),
            "wind_down_executor: graceful /exit — eligible past grace"
        );

        // The ONE way this module reaches a pane. Never `TerminalManager::close`.
        let outcome = match manager.graceful_exit(&terminal_id, EXIT_DEADLINE).await {
            Ok(outcome) => outcome,
            Err(e) => {
                warn!(session_id = %session_id, terminal_id = %terminal_id, error = %e, "wind_down_executor: graceful exit could not start");
                continue;
            }
        };
        let recorded = match &outcome {
            GracefulExitOutcome::Exited { .. } => Some(WIND_DOWN_CLOSED),
            GracefulExitOutcome::ExitStuck { .. } => Some(WIND_DOWN_EXIT_STUCK),
            GracefulExitOutcome::CloseRefused { .. } => Some(WIND_DOWN_CLOSE_REFUSED),
            GracefulExitOutcome::CloseOutcomeUnknown { .. } => Some(WIND_DOWN_CLOSE_UNKNOWN),
            // `Refused` and `WriteFailed` are NOT non-events: `/exit` may
            // already have been typed into the pane, and where the refusal's
            // Ctrl-U recovery fired it cleared the whole input line. An
            // unattended closer that touched an operator's pane and recorded
            // nothing would be the worst of both. Recorded.
            GracefulExitOutcome::Refused { .. } | GracefulExitOutcome::WriteFailed { .. } => {
                Some(WIND_DOWN_NOT_ATTEMPTED)
            }
            // `NoLiveClaude` and `ProbeUnavailable` return before anything is
            // typed, so nothing happened to the session and nothing is claimed.
            GracefulExitOutcome::NoLiveClaude | GracefulExitOutcome::ProbeUnavailable { .. } => {
                None
            }
        };
        match (recorded, &store) {
            (Some(word), Some(store)) => {
                store.set_wind_down_outcome(&session_id, word, Utc::now().timestamp_millis())
            }
            _ => {
                debug!(session_id = %session_id, ?outcome, "wind_down_executor: nothing recorded for this outcome")
            }
        }
        if matches!(outcome, GracefulExitOutcome::Exited { .. }) {
            info!(session_id = %session_id, terminal_id = %terminal_id, ?kind, "wind_down_executor: closed");
            // D6: a steward that the drain stopped is owed a restart on
            // undrain, and only a CLOSED one is actually stopped.
            if kind == SessionKind::Steward {
                if let Some(kind) = steward_kind {
                    let path = stopped_by_drain_path();
                    let mut set = StoppedByDrain::load(&path);
                    if set.insert(&kind) {
                        set.save(&path);
                        info!(steward = %kind, "wind_down_executor: recorded stopped_by_drain");
                    }
                }
            }
        } else {
            warn!(session_id = %session_id, terminal_id = %terminal_id, ?outcome, "wind_down_executor: not closed");
        }
    }
}

/// The undrain half: restart exactly the stewards the drain stopped, then clear
/// the set.
///
/// The other two undrain obligations need no code here, and saying so is the
/// point of this comment rather than an omission:
///
/// * **deferred restore and resume** are parked on
///   `coord_drain_state::wait_until_allowed` (boot AI-session resume, workflow
///   startup resume) or re-run from the frontend's `coord-drain-state-changed`
///   subscription (terminal tab restore). All three are woken by the very state
///   change that produced this `Undrain`, so re-driving them here would double
///   them.
/// * **the looping supervisor** resumes by itself: its drain rewrite
///   (`looping_agent_supervisor::drain_rewrite`) stops turning `Spawn` into
///   `None` the moment the gate allows, and a closed tab makes the next tick's
///   decision a `FirstSpawn`.
async fn undrain(app: &tauri::AppHandle) {
    let path = stopped_by_drain_path();
    let mut set = StoppedByDrain::load(&path);
    if set.is_empty() {
        info!("wind_down_executor: the drain lifted — nothing was stopped by it");
        return;
    }
    let kinds = set.drain_all();
    info!(
        ?kinds,
        "wind_down_executor: the drain lifted — restarting the stewards it stopped"
    );
    // Cleared BEFORE the restarts, and persisted first: a restart that fails is
    // reported and left to the operator, whereas a set that survived a failure
    // would retry the same steward on every tick forever.
    set.save(&path);
    for kind in kinds {
        match crate::mcp::steward::restart_after_drain(app.clone(), &kind).await {
            Ok(crate::mcp::steward::RestartOutcome::Started) => {
                info!(steward = %kind, "wind_down_executor: restarted")
            }
            Ok(crate::mcp::steward::RestartOutcome::NotNeeded(why)) => {
                info!(steward = %kind, "wind_down_executor: no restart needed — {why}")
            }
            Err(e) => {
                warn!(steward = %kind, error = %e, "wind_down_executor: could not restart the steward the drain stopped — it needs an operator")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drained() -> CoordDrainState {
        CoordDrainState::Drained {
            until: None,
            reason: Some("rebuild".to_string()),
        }
    }

    fn unknown() -> CoordDrainState {
        CoordDrainState::Unknown {
            since: DateTime::<Utc>::UNIX_EPOCH,
            cause: "three missed heartbeats".to_string(),
        }
    }

    // ── Invariant 1: no kill path from this module ───────────────────────

    /// The module reaches a pane through `TerminalManager::graceful_exit` and
    /// nothing else. A future edit that reached for the kill path — directly,
    /// or through the ordinary `close` that calls it — fails here.
    ///
    /// A TRIPWIRE, not a proof. It is literal text matching over this file's
    /// own executable lines, so a kill spelled some other way (a receiver bound
    /// to a new name, a helper in another module) walks past it. What makes the
    /// invariant structural is the code above — this module holds exactly one
    /// call that reaches a pane — and this test is what makes a careless edit
    /// to that fact noisy.
    #[test]
    fn the_executor_never_reaches_a_kill_path() {
        let src = include_str!("wind_down_executor.rs");
        // Strip this test module, and then every comment line: BOTH the module
        // docs and this test NAME the forbidden calls, so a scan over raw text
        // can only ever fail. Only executable lines are evidence.
        let body: String = src
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half")
            .lines()
            .filter(|line| {
                let t = line.trim_start();
                !(t.starts_with("//") || t.starts_with("/*") || t.starts_with('*'))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let body = body.as_str();
        for forbidden in [
            ".kill(",
            "io.kill",
            "close_with_deadline",
            "TerminalManager::close",
            "manager.close(",
            ".close(&terminal_id)",
            "taskkill",
        ] {
            assert!(
                !body.contains(forbidden),
                "wind_down_executor reaches `{forbidden}` — D5 forbids this module from killing \
                 a live claude; the only close it may use is TerminalManager::graceful_exit"
            );
        }
        assert!(
            body.contains("graceful_exit(") && body.contains("EXIT_DEADLINE"),
            "the one permitted close call is gone from the module's EXECUTABLE \
             lines — this test is now vacuous"
        );
    }

    // ── Invariant 4: `Unknown` performs no action ────────────────────────

    #[test]
    fn unknown_performs_no_action_in_either_direction() {
        let now = Utc::now();
        // Not a wind-down...
        assert_eq!(
            decide_tick(&unknown(), false, now),
            TickAction::Nothing,
            "an unreadable drain state must not wind anything down"
        );
        // ...and not an undrain either, even with a drain outstanding.
        assert_eq!(
            decide_tick(&unknown(), true, now),
            TickAction::Nothing,
            "an unreadable drain state must not restart anything"
        );
    }

    #[test]
    fn a_drain_that_goes_unknown_and_then_clear_is_still_an_undrain() {
        let now = Utc::now();
        let mut was_drained = false;
        for (state, expected) in [
            (drained(), TickAction::WindDown),
            (unknown(), TickAction::Nothing),
            (CoordDrainState::Clear, TickAction::Undrain),
            (CoordDrainState::Clear, TickAction::Nothing),
        ] {
            let action = decide_tick(&state, was_drained, now);
            assert_eq!(
                action, expected,
                "{state:?} with owes_undrain={was_drained}"
            );
            match action {
                TickAction::WindDown => was_drained = true,
                TickAction::Undrain => was_drained = false,
                TickAction::Nothing => {}
            }
        }
    }

    #[test]
    fn an_expired_drain_reads_as_allowing_exactly_as_the_spawn_gate_does() {
        let now = Utc::now();
        let expired = CoordDrainState::Drained {
            until: Some(now - chrono::Duration::seconds(1)),
            reason: None,
        };
        let live = CoordDrainState::Drained {
            until: Some(now + chrono::Duration::seconds(60)),
            reason: None,
        };
        assert_eq!(decide_tick(&expired, true, now), TickAction::Undrain);
        assert_eq!(decide_tick(&expired, false, now), TickAction::Nothing);
        assert_eq!(decide_tick(&live, false, now), TickAction::WindDown);
        // ...and the spawn gate agrees about the same two states.
        assert!(crate::coord_drain_state::gate_for_at(
            &expired,
            crate::coord_drain_state::SpawnOrigin::LoopingAgent,
            now
        )
        .allows());
        assert!(!crate::coord_drain_state::gate_for_at(
            &live,
            crate::coord_drain_state::SpawnOrigin::LoopingAgent,
            now
        )
        .allows());
    }

    #[test]
    fn not_enrolled_is_an_undrain_not_a_wind_down() {
        let now = Utc::now();
        let not_enrolled = CoordDrainState::NotEnrolled {
            why: "no machine.json",
        };
        assert_eq!(decide_tick(&not_enrolled, true, now), TickAction::Undrain);
        assert_eq!(decide_tick(&not_enrolled, false, now), TickAction::Nothing);
    }

    // ── Hazard 1: the wall-clock guard ───────────────────────────────────

    const TOL: Duration = CLOCK_SKEW_TOLERANCE;

    #[test]
    fn a_forward_wall_clock_step_quarantines_wind_down_for_a_grace_period() {
        let grace = Duration::from_secs(600);
        let mut guard = ClockJumpGuard::new();
        let t0 = Instant::now();
        // First tick establishes the reference.
        assert_eq!(
            guard.check(t0, 1_000_000, grace, TOL),
            ClockVerdict::Trustworthy
        );
        // A normal tick: both clocks advanced 30 s.
        assert_eq!(
            guard.check(t0 + TICK, 1_030_000, grace, TOL),
            ClockVerdict::Trustworthy
        );
        // An hour of wall clock across one 30 s tick — an NTP step or a resume.
        let jumped = guard.check(t0 + TICK * 2, 1_030_000 + 30_000 + 3_600_000, grace, TOL);
        assert_eq!(jumped, ClockVerdict::Jumped { skew_ms: 3_600_000 });
        // ...and the next ordinary tick is still refused.
        assert_eq!(
            guard.check(t0 + TICK * 3, 1_030_000 + 60_000 + 3_600_000, grace, TOL),
            ClockVerdict::Quarantined
        );
        // Past the quarantine it recovers.
        assert_eq!(
            guard.check(
                t0 + TICK * 2 + grace + TICK,
                1_030_000 + 30_000 + 3_600_000 + grace.as_millis() as i64 + 30_000,
                grace,
                TOL
            ),
            ClockVerdict::Trustworthy
        );
    }

    #[test]
    fn a_backward_wall_clock_step_is_refused_too() {
        let grace = Duration::from_secs(600);
        let mut guard = ClockJumpGuard::new();
        let t0 = Instant::now();
        guard.check(t0, 1_000_000, grace, TOL);
        assert_eq!(
            guard.check(t0 + TICK, 1_000_000 + 30_000 - 600_000, grace, TOL),
            ClockVerdict::Jumped { skew_ms: -600_000 }
        );
    }

    #[test]
    fn a_late_tick_is_not_a_clock_jump() {
        // Lateness moves BOTH clocks, so the skew stays ~0 however late the
        // tick is. Without this the executor would quarantine itself on any
        // loaded box.
        let grace = Duration::from_secs(600);
        let mut guard = ClockJumpGuard::new();
        let t0 = Instant::now();
        guard.check(t0, 1_000_000, grace, TOL);
        assert_eq!(
            guard.check(
                t0 + Duration::from_secs(900),
                1_000_000 + 900_000 + 200,
                grace,
                TOL
            ),
            ClockVerdict::Trustworthy
        );
    }

    /// The tolerance boundary is exclusive on `>`, so a skew EQUAL to it still
    /// passes and one millisecond more does not. Exercised at a value other
    /// than the shipped constant, which is only possible because the parameter
    /// exists.
    #[test]
    fn the_skew_tolerance_boundary_is_exact() {
        let grace = Duration::from_secs(600);
        let tol = Duration::from_millis(1_000);
        let t0 = Instant::now();

        let mut at = ClockJumpGuard::new();
        at.check(t0, 0, grace, tol);
        assert_eq!(
            at.check(t0 + TICK, 30_000 + 1_000, grace, tol),
            ClockVerdict::Trustworthy
        );

        let mut over = ClockJumpGuard::new();
        over.check(t0, 0, grace, tol);
        assert_eq!(
            over.check(t0 + TICK, 30_000 + 1_001, grace, tol),
            ClockVerdict::Jumped { skew_ms: 1_001 }
        );
    }

    /// A live quarantine must survive a supervised respawn, which is exactly
    /// what holding the guard in the loop's own state would have lost.
    #[test]
    fn the_clock_guard_is_process_lifetime_not_per_run() {
        // Two `check_clock` calls through the shared guard establish a
        // reference the second call can be judged against — an accessor over a
        // per-run value could not do that at all.
        let grace = Duration::from_secs(600);
        assert!(check_clock(grace).trustworthy());
        assert!(
            clock_guard().lock().unwrap().last.is_some(),
            "the shared guard did not retain its reference"
        );
        // The loop's own state must not carry one.
        let state = ExecutorState::default();
        assert!(!state.was_drained);
        assert!(!state.inherited_checked);
    }

    // ── The persisted stopped-by-drain set ───────────────────────────────

    #[test]
    fn the_stopped_by_drain_set_round_trips_and_an_unreadable_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wind-down-stopped-stewards.json");
        assert!(StoppedByDrain::load(&path).is_empty(), "absent file");

        let mut set = StoppedByDrain::load(&path);
        assert!(set.insert("merge-train"));
        assert!(!set.insert("merge-train"), "idempotent per kind");
        assert!(set.insert("cleanup"));
        set.save(&path);

        let reloaded = StoppedByDrain::load(&path);
        assert_eq!(
            reloaded.kinds().collect::<Vec<_>>(),
            vec!["cleanup", "merge-train"]
        );

        std::fs::write(&path, b"not json").unwrap();
        assert!(
            StoppedByDrain::load(&path).is_empty(),
            "an unparseable file is an empty set, never an error"
        );
    }

    /// A runner restarted mid-drain has `was_drained: false` in memory, but the
    /// persisted set still names what it owes — and that is what
    /// [`tick_once`] folds into `owes_undrain`, so the stewards come back.
    #[test]
    fn a_non_empty_persisted_set_is_itself_an_owed_undrain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wind-down-stopped-stewards.json");
        let mut set = StoppedByDrain::default();
        set.insert("merge-train");
        set.save(&path);

        // What a freshly started process sees: nothing in memory...
        let in_memory = false;
        // ...and the durable half.
        let owes = in_memory || !StoppedByDrain::load(&path).is_empty();
        assert!(owes);
        assert_eq!(
            decide_tick(&CoordDrainState::Clear, owes, Utc::now()),
            TickAction::Undrain
        );
    }

    #[test]
    fn draining_the_set_empties_it() {
        let mut set = StoppedByDrain::default();
        set.insert("dev-ops");
        assert_eq!(set.drain_all(), vec!["dev-ops".to_string()]);
        assert!(set.is_empty());
    }

    // ── Invariants 2 and 3, over the pure verdict this executor acts on ──
    //
    // The executor closes exactly what `wind_down::eligibility` calls
    // `Eligible`, so these pin the property AT THE SEAM the executor reads:
    // the verdict a candidate must carry before `graceful_exit` is called.

    use qontinui_runner_lib::wind_down::{
        eligibility, Eligibility, EligibilityInputs, GridIdle, IneligibleReason, Sideband,
        SidebandState, WorkStatus,
    };

    const GRACE: Duration = Duration::from_secs(600);
    const GRACE_MS: i64 = 600_000;

    fn closable(inputs: &EligibilityInputs, now_ms: i64) -> bool {
        matches!(eligibility(inputs, now_ms), Eligibility::Eligible { .. })
    }

    /// Invariant 2: a `working` sideband report restarts the grace clock, so a
    /// session that reported `working` cannot be closed until a full grace
    /// period has passed SINCE that report — not since it went idle.
    #[test]
    fn a_working_sideband_state_resets_the_grace_clock() {
        let idle_since = 1_000;
        let base = EligibilityInputs {
            kind: SessionKind::Terminal,
            work_status: WorkStatus::Finished,
            sideband: Sideband::NeverReported,
            grid: GridIdle::Idle {
                since_ms: idle_since,
            },
            has_live_children: Some(false),
            finished_at_ms: None,
            grace: GRACE,
        };
        // With no sideband report, grace runs from the idle window.
        assert!(closable(&base, idle_since + GRACE_MS));

        // A `working` report is an outright refusal while it stands...
        let working = EligibilityInputs {
            sideband: Sideband::Reported {
                state: SidebandState::Working,
                set_at_ms: idle_since + 1,
            },
            ..base
        };
        assert_eq!(
            eligibility(&working, i64::MAX),
            Eligibility::Ineligible {
                reason: IneligibleReason::SidebandWorking
            }
        );

        // ...and once it stops, the clock runs from the LAST report, not from
        // the old idle window: the moment that used to be eligible is not.
        let after_working = EligibilityInputs {
            sideband: Sideband::Reported {
                state: SidebandState::NotWorking,
                set_at_ms: idle_since + 300_000,
            },
            ..base
        };
        assert!(
            !closable(&after_working, idle_since + GRACE_MS),
            "the grace clock did not restart at the sideband report"
        );
        assert!(closable(&after_working, idle_since + 300_000 + GRACE_MS));
    }

    /// Invariant 3: an idle terminal session that is not `finished` is never
    /// closed — however long it has been idle, and whatever else holds.
    #[test]
    fn an_unfinished_idle_terminal_session_is_never_closed() {
        for work_status in [WorkStatus::NotFinished, WorkStatus::Unknown] {
            let inputs = EligibilityInputs {
                kind: SessionKind::Terminal,
                work_status,
                sideband: Sideband::NeverReported,
                grid: GridIdle::Idle { since_ms: 0 },
                has_live_children: Some(false),
                finished_at_ms: None,
                grace: GRACE,
            };
            assert!(
                !closable(&inputs, i64::MAX),
                "a terminal session with work_status={work_status:?} became closable"
            );
        }
        // And the exemption really is only for the two kinds D6 names.
        for kind in [SessionKind::Looping, SessionKind::Steward] {
            let inputs = EligibilityInputs {
                kind,
                work_status: WorkStatus::Unknown,
                sideband: Sideband::NeverReported,
                grid: GridIdle::Idle { since_ms: 0 },
                has_live_children: Some(false),
                finished_at_ms: None,
                grace: GRACE,
            };
            assert!(
                closable(&inputs, GRACE_MS),
                "{kind:?} never finishes by design and must still wind down"
            );
        }
    }
}
