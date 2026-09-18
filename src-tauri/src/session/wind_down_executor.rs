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
//!   loop DEFINITION is untouched, so the supervisor brings the agent back by
//!   itself once the drain lifts; nothing here re-registers it. The spawn it
//!   makes is a `DeathRespawn`, not a `FirstSpawn` — `looping_agent::policy`
//!   picks `FirstSpawn` only while `ever_spawned` is false, which no tab this
//!   module could have closed satisfies — and it is backoff-gated.
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
//! ## A bare shell left by a refused close (review S-1, related lead)
//!
//! `CloseRefused` and `CloseOutcomeUnknown` are reached only AFTER `claude` is
//! gone, so they leave a pane holding a bare shell, still registered with the
//! `TerminalManager`. Two consumers read that pane as ALIVE, and they are worth
//! knowing about because neither is this module's to fix:
//!
//! * **Stewards** — fixed, but NOT here alone, and the first attempt to fix it
//!   here alone did nothing. Recording `stopped_by_drain` on "`claude` left"
//!   rather than "the tab closed" is necessary and was not sufficient: the
//!   undrain restart still hit `find_running_steward`, which keyed on the
//!   SHELL's `is_alive`, still answered `running: true` for the emptied pane,
//!   and returned a 409 carrying no `DeferClass` code — so the kind read as the
//!   benign "already running" family, was settled out of the set, and the
//!   steward stayed down with a log line claiming no restart was needed. The
//!   real fix is `mcp::steward::steward_pane_is_running`, which asks whether a
//!   `claude` lives in the pane. Both halves are required: without the
//!   recording the kind is never owed, without the liveness change the restart
//!   is refused.
//! * **Looping agents** — `looping_agent_supervisor::resolve_live_session`
//!   derives `Liveness` from whether the terminal id is still REGISTERED with
//!   the manager, never from whether a `claude` lives in it, so `tab_alive` is
//!   true for that bare shell. `looping_agent::policy::decide` therefore skips
//!   its `DeathRespawn` arm and takes the live-tab arm: on the next idle tick
//!   past grace it emits `Nudge`, which types a journal prompt into a shell
//!   that will never answer, and increments `cycles_since_relaunch` doing it.
//!   A `Relaunch` (context-low, or the K-cycle budget) self-heals, because it
//!   closes the tab before respawning — so the agent recovers at the next
//!   relaunch cadence rather than staying wedged forever. **Not fixed here:**
//!   the honest fix is in that supervisor's own liveness resolution, which is
//!   outside this plan, and the failure is bounded and observable (the
//!   lifecycle record carries `close_refused` / `close_unknown`). Recorded as a
//!   follow-up rather than patched from a neighbouring module.
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
use crate::session::tracking_health::LiveClaudeProcess;
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
    /// The FIRST call establishes the reference and is trustworthy, for two
    /// separate reasons — one per clock the window is built from:
    ///
    /// * the grid-idle window is carried on a per-`TerminalSession`
    ///   `GridIdleTracker`, and this process created every `TerminalSession` it
    ///   can see, so that window cannot predate the process;
    /// * coord's `finished_at` DOES routinely predate the process, arriving
    ///   over the wire — but it only ever `.max()`es `since_ms` in
    ///   `wind_down::eligibility`, so it can move a session towards `NotYet`
    ///   and never towards `Eligible`. A stale one cannot authorise a close.
    ///
    /// Both of those need the guard to live for the PROCESS ([`clock_guard`]),
    /// not for one run of the loop: a per-run guard would drop a live
    /// quarantine on every supervised respawn and then read the next tick as a
    /// trustworthy first call.
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

    /// Forget a kind. Returns `true` when it was there to forget.
    pub fn remove(&mut self, kind: &str) -> bool {
        self.kinds.remove(kind)
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
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
    // S-3: the clock is compared on EVERY tick, not only on drained ones. The
    // tolerance is an ABSOLUTE 5 ms-scale bound, not a rate, so a reference left
    // behind by the last drained tick — days old on a runner that is rarely
    // drained — turns ordinary NTP slew, or an hour of laptop suspend (Linux
    // `Instant` is CLOCK_MONOTONIC and does not advance across it), into a
    // "jump" and quarantines wind-down for a full grace period at exactly the
    // moment a drain wants the runner to reach idle. Comparing every tick keeps
    // the interval at ~`TICK`, where 5 s of disagreement really is a step.
    // The verdict is only CONSULTED in the wind-down arm.
    let grace = wind_down::grace_from_env();
    let clock = check_clock(grace);
    // Logged HERE, at DETECTION, not where the verdict is consulted. S-3's
    // whole premise is that suspend and NTP slew are ordinary, so the jump is
    // usually detected on a `Nothing` or `Undrain` tick — and a log buried in
    // the wind-down arm would arm a full grace period of quarantine with
    // nothing at info level saying why. An operator would then watch a drained
    // runner refuse to wind down and have no thread to pull.
    if let ClockVerdict::Jumped { skew_ms } = clock {
        warn!(
            skew_ms,
            quarantine_s = grace.as_secs(),
            drain = crate::coord_drain_state::current().label(),
            "wind_down_executor: the wall clock stepped against the monotonic clock — \
             every idle window in play is untrustworthy, so wind-down is quarantined \
             for a full grace period"
        );
    }

    match decide_tick(&drain, state.was_drained, Utc::now()) {
        TickAction::Nothing => {}
        TickAction::Undrain => {
            // `undrain` reports what it could not restart, so a kind the drain
            // re-armed under is still owed and is retried on a later undrain.
            state.was_drained = undrain(&app).await;
        }
        TickAction::WindDown => {
            state.was_drained = true;
            match clock {
                ClockVerdict::Trustworthy => wind_down_once(&app, grace).await,
                // Hazard 1. Fail closed: no close is attempted on an idle
                // window measured across a clock step. Already logged at
                // detection, above.
                ClockVerdict::Jumped { .. } => {}
                ClockVerdict::Quarantined => debug!(
                    "wind_down_executor: still within the post-clock-jump quarantine — no closes"
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The two pure halves of a pass (extracted so they can be tested directly —
// the invariants they carry are the ones this module, not Phase 1, owns)
// ---------------------------------------------------------------------------

/// PURE: which `(claude_session_id, terminal_id)` pairs a pass may close.
///
/// Three filters, each load-bearing:
/// * the verdict must be exactly `Eligible` — `NotYet`, `Ineligible` and
///   `Unknown` are all refusals, and a process with NO verdict at all (a nested
///   subagent) is not a candidate either;
/// * the process must resolve to a pane, because `graceful_exit` targets a pane;
/// * ONE candidate per pane, since two top-level `claude` processes attributed
///   to the same terminal would spend two of the tick's budget slots on one
///   close and the second would fail on a pane that is already gone.
pub fn select_candidates(
    processes: &[LiveClaudeProcess],
    observed: &wind_down_observer::ObservedInputs,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for proc in processes {
        // A nested subagent is never a candidate. The observer already leaves
        // its verdict `None`, so this is belt and braces — but the function
        // ADVERTISES the property, and a doc that only a collaborator enforces
        // is the defect class this module keeps finding.
        if proc.nested_under_claude {
            continue;
        }
        if !proc
            .wind_down
            .as_ref()
            .is_some_and(wind_down::WindDownView::is_eligible)
        {
            continue;
        }
        let Some(session_id) = proc.session_id.clone() else {
            continue;
        };
        let Some(terminal_id) = observed.terminal_for(&session_id).map(str::to_string) else {
            continue;
        };
        if out.iter().any(|(_, tid)| tid == &terminal_id) {
            continue;
        }
        out.push((session_id, terminal_id));
    }
    out
}

/// PURE: the word an outcome is recorded on the lifecycle record as, or `None`
/// when the executor cannot honestly claim anything happened to the session.
pub fn outcome_word(outcome: &GracefulExitOutcome) -> Option<&'static str> {
    match outcome {
        GracefulExitOutcome::Exited { .. } => Some(WIND_DOWN_CLOSED),
        GracefulExitOutcome::ExitStuck { .. } => Some(WIND_DOWN_EXIT_STUCK),
        GracefulExitOutcome::CloseRefused { .. } => Some(WIND_DOWN_CLOSE_REFUSED),
        GracefulExitOutcome::CloseOutcomeUnknown { .. } => Some(WIND_DOWN_CLOSE_UNKNOWN),
        // `Refused` and `WriteFailed` are NOT non-events: `/exit` may already
        // have been typed into the pane, and where the refusal's Ctrl-U
        // recovery fired it cleared the whole input line. An unattended closer
        // that touched an operator's pane and recorded nothing would be the
        // worst of both. Recorded.
        GracefulExitOutcome::Refused { .. } | GracefulExitOutcome::WriteFailed { .. } => {
            Some(WIND_DOWN_NOT_ATTEMPTED)
        }
        // These two return before anything is typed, so nothing happened to the
        // session and nothing is claimed.
        GracefulExitOutcome::NoLiveClaude | GracefulExitOutcome::ProbeUnavailable { .. } => None,
    }
}

/// PURE: must this candidate's verdict be re-established before it is closed?
///
/// Only the FIRST candidate is exempt: its verdict comes from the pass that
/// just ran, milliseconds old, with nothing having elapsed since. Every later
/// one can be minutes stale (see the call site).
pub fn recheck_is_owed(index: usize) -> bool {
    index > 0
}

/// PURE: may this candidate be closed, given what the re-check found?
///
/// `view` is `None` both when no re-check was owed and when one ran and found
/// the session gone — which the caller distinguishes, and which this does not
/// need to: it is FAIL-CLOSED for every index that owes a re-check. Only an
/// index that owes none, or a re-check that came back `Eligible`, admits.
///
/// Extracted and tested as a function because the mutations that matter are
/// invisible otherwise: deleting the re-check, or weakening its condition to
/// an index no batch reaches, is green across the whole suite and walks past
/// the kill-path tripwire, which forbids literals and pins nothing positive.
pub fn recheck_admits(index: usize, view: Option<&wind_down::WindDownView>) -> bool {
    if !recheck_is_owed(index) {
        return true;
    }
    view.is_some_and(wind_down::WindDownView::is_eligible)
}

/// PURE: did `claude` leave the pane? See the `stopped_by_drain` comment in
/// [`wind_down_once`] for why this, and not "the tab closed", is the predicate
/// a steward restart is owed on.
pub fn claude_left(outcome: &GracefulExitOutcome) -> bool {
    matches!(
        outcome,
        GracefulExitOutcome::Exited { .. }
            | GracefulExitOutcome::CloseRefused { .. }
            | GracefulExitOutcome::CloseOutcomeUnknown { .. }
    )
}

/// PURE: did this exit attempt POSITIVELY OBSERVE a `claude` in the pane?
///
/// B4-1's evidence. The steward registry needs to know a pane was once
/// occupied, and until now the only thing that could tell it was a process
/// probe driven by an HTTP request — which an unattended runner never
/// receives, so the fact went unrecorded on exactly the boxes this plan is
/// about.
///
/// This reads the evidence the exit already gathered rather than re-deriving
/// it: six of the eight outcomes carry `claude_pids`, and a NON-EMPTY list is
/// the pids `drive` actually saw. `NoLiveClaude` (looked, found none) and
/// `ProbeUnavailable` (could not look) carry none, and both correctly answer
/// false — the second because "could not look" is never evidence.
///
/// An exhaustive `match` rather than `matches!` deliberately: a new outcome
/// variant must be classified here by the compiler, not silently default to
/// "no claude was ever here", which is the answer that loses a steward.
pub fn claude_was_observed(outcome: &GracefulExitOutcome) -> bool {
    match outcome {
        GracefulExitOutcome::Exited { claude_pids, .. }
        | GracefulExitOutcome::ExitStuck { claude_pids, .. }
        | GracefulExitOutcome::Refused { claude_pids, .. }
        | GracefulExitOutcome::WriteFailed { claude_pids, .. }
        | GracefulExitOutcome::CloseRefused { claude_pids, .. }
        | GracefulExitOutcome::CloseOutcomeUnknown { claude_pids, .. } => !claude_pids.is_empty(),
        GracefulExitOutcome::NoLiveClaude | GracefulExitOutcome::ProbeUnavailable { .. } => false,
    }
}

/// Add `kind` to the persisted stopped-by-drain set. Off the runtime worker
/// (N-2): the store is small but `std::fs` is blocking, and this runs inside
/// the executor's async tick.
async fn record_stopped_by_drain(kind: String) {
    // The `JoinError` is REPORTED, not discarded. This module's own doctrine is
    // that under-recording a stopped steward is permanent, so a panic in the
    // task that does the recording is the loudest thing that can happen here —
    // `let _ =` would convert it into silence.
    let named = kind.clone();
    if let Err(e) = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        let path = stopped_by_drain_path();
        let mut set = StoppedByDrain::load(&path);
        if set.insert(&kind) {
            set.save(&path);
            info!(steward = %kind, "wind_down_executor: recorded stopped_by_drain");
        }
    })
    .await
    {
        warn!(steward = %named, error = %e, "wind_down_executor: recording stopped_by_drain DIED — this steward may not be restarted on undrain");
    }
}

/// Read the persisted set off the runtime worker (N-2).
async fn load_stopped_by_drain() -> StoppedByDrain {
    match qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(|| {
        StoppedByDrain::load(&stopped_by_drain_path())
    })
    .await
    {
        Ok(set) => set,
        Err(e) => {
            // An empty set is the same answer an absent file gives, so the
            // undrain simply restarts nothing — but SAY so, because here it
            // means "could not look", not "nothing was stopped".
            warn!(error = %e, "wind_down_executor: reading the stopped-by-drain set DIED — treating it as empty for this tick");
            StoppedByDrain::default()
        }
    }
}

/// Persist the set off the runtime worker (N-2).
async fn save_stopped_by_drain(set: StoppedByDrain) {
    if let Err(e) = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        set.save(&stopped_by_drain_path());
    })
    .await
    {
        warn!(error = %e, "wind_down_executor: persisting the stopped-by-drain set DIED — a kind may be restarted twice, or not at all, after a restart");
    }
}

/// One wind-down pass: observe, then close what is eligible.
async fn wind_down_once(app: &tauri::AppHandle, grace: Duration) {
    use tauri::Manager;

    let fresh = wind_down_observer::fresh_pass(app, grace).await;
    let Some(pass) = fresh.pass.as_ref() else {
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

    let candidates = select_candidates(&pass.report.terminal_hosted, &fresh.observed);

    if candidates.is_empty() {
        return;
    }
    info!(
        eligible = candidates.len(),
        budget = MAX_CLOSES_PER_TICK,
        "wind_down_executor: the device is drained — closing eligible sessions"
    );

    let effects = LiveCloseEffects {
        app,
        grace,
        manager,
        store,
        fresh: &fresh,
    };
    close_batch(candidates, &effects).await;
}

/// What one candidate's turn came to. Returned rather than only logged so a
/// batch is assertable — see [`close_batch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateOutcome {
    /// A graceful exit RAN. Whether it closed the pane is
    /// [`GracefulExitOutcome`]'s to say — `ExitStuck` and `CloseRefused` are
    /// both ordinary — so this deliberately does not claim one. It used to be
    /// spelled `Closed`, which was a lie on both of those and on the arm
    /// below.
    Attempted,
    /// `graceful_exit` could not even start (the pane was gone from the
    /// manager, say). Nothing was typed.
    ExitNotStarted,
    /// Re-observation no longer admitted it (B1). Nothing was typed at it.
    NoLongerEligible,
    /// The drain lifted before its turn. This candidate and every later one
    /// were abandoned, so this is always the LAST element.
    DrainLifted,
}

/// Everything [`close_batch`] does to the world, behind a trait.
///
/// The two per-candidate preconditions are **sequencing**, not computation, and
/// sequencing is exactly what the pure helpers around it cannot pin: with
/// `recheck_is_owed` and `recheck_admits` tested in isolation, deleting the
/// block that CALLS them stayed green across the whole suite. Injecting the
/// three effects is what makes "index 0 is not re-checked, index 1 is, and a
/// `NotYet` answer skips the close" an assertion instead of a comment.
#[async_trait::async_trait]
pub(crate) trait CloseEffects {
    /// (a) Is the device still drained?
    fn still_drained(&self) -> bool;
    /// (b) Re-observe ONE candidate. `None` when the session or its pane is
    /// gone, or could not be observed — both fail closed.
    async fn recheck(&self, session_id: &str, terminal_id: &str)
        -> Option<wind_down::WindDownView>;
    /// Ask the pane to leave. `Err` when the exit could not START at all.
    ///
    /// Deliberately NOT named `close`: the kill-path tripwire forbids the bare
    /// token `.close(` anywhere in this module's executable lines, receiver-
    /// agnostic, and a trait method spelled that way would have put one there
    /// — which is the tripwire working, not a false positive. This module
    /// never closes a pane; it asks for a graceful exit and the exit closes it.
    async fn wind_down_one(
        &self,
        session_id: &str,
        terminal_id: &str,
    ) -> Result<GracefulExitOutcome, String>;
    /// What kind of session this pane holds, and — for a steward — which kind
    /// of steward. Reads, not effects, but they come from the pass and the
    /// steward registry, so a test double supplies them.
    fn session_kind(&self, terminal_id: &str) -> SessionKind;
    fn steward_kind(&self, terminal_id: &str) -> Option<String>;
    /// Stamp the outcome on the session's lifecycle record.
    fn record_outcome(&self, session_id: &str, outcome: &GracefulExitOutcome);
    /// Latch [`crate::mcp::steward::StewardMeta::claude_seen`] — B4-1's
    /// backend-owned writer.
    fn record_claude_seen(&self, terminal_id: &str);
    /// D6: this steward kind is owed a restart on undrain.
    async fn record_stopped_by_drain(&self, kind: &str);
}

/// Close up to [`MAX_CLOSES_PER_TICK`] candidates, re-establishing BOTH
/// preconditions before each one.
///
/// A batch is up to `MAX_CLOSES_PER_TICK` closes, each of which can wait a full
/// [`EXIT_DEADLINE`], so the last one can start minutes after the pass that
/// authorised it. Both things that authorise a close can have changed in those
/// minutes, and both are re-read:
///
/// * **The DRAIN.** "Only while `Drained`" would otherwise be false for every
///   close after the first.
/// * **The ELIGIBILITY of THIS session.** The scenario: four sessions are
///   eligible, D is last; while A, B and C are being closed the operator
///   returns to D, types a prompt, `claude` works for 90 s, answers, and D is
///   back at an empty prompt. D's grid generation moved and its grace clock
///   restarted, so a re-observed verdict is `NotYet` for another full grace
///   period — but the frozen one still says `Eligible`, `exit_prompt_ready`
///   passes (the pane genuinely IS at an empty prompt), and the session is
///   closed out from under them. `drive`'s own preamble catches a pane that is
///   BUSY at close time; it cannot catch one that was busy twenty seconds ago
///   and is momentarily quiet, which is precisely the state a grace period
///   exists to distinguish from idleness. Without this, two of this module's
///   four advertised invariants — "a `working` sideband resets the grace clock"
///   and "an unfinished idle terminal session is never closed" — would hold
///   only of a value read before the batch began.
///
///   Skipped for the FIRST candidate alone, whose verdict is the pass that just
///   ran, milliseconds old, with nothing having elapsed since. Every later one
///   pays a re-check.
pub(crate) async fn close_batch<E: CloseEffects + Sync>(
    candidates: Vec<(String, String)>,
    effects: &E,
) -> Vec<CandidateOutcome> {
    let mut outcomes = Vec::new();
    for (index, (session_id, terminal_id)) in
        candidates.into_iter().take(MAX_CLOSES_PER_TICK).enumerate()
    {
        if !effects.still_drained() {
            info!("wind_down_executor: the drain lifted mid-batch — stopping this pass");
            outcomes.push(CandidateOutcome::DrainLifted);
            return outcomes;
        }
        let rechecked = if recheck_is_owed(index) {
            effects.recheck(&session_id, &terminal_id).await
        } else {
            None
        };
        if !recheck_admits(index, rechecked.as_ref()) {
            info!(
                session_id = %session_id,
                terminal_id = %terminal_id,
                verdict = rechecked.as_ref().map_or("gone", |v| v.eligibility),
                reason = rechecked.as_ref().and_then(|v| v.reason).unwrap_or("-"),
                "wind_down_executor: no longer eligible when its turn came — not closed"
            );
            outcomes.push(CandidateOutcome::NoLongerEligible);
            continue;
        }
        let outcome = match effects.wind_down_one(&session_id, &terminal_id).await {
            Ok(outcome) => outcome,
            Err(e) => {
                warn!(session_id = %session_id, terminal_id = %terminal_id, error = %e, "wind_down_executor: graceful exit could not start");
                outcomes.push(CandidateOutcome::ExitNotStarted);
                continue;
            }
        };
        effects.record_outcome(&session_id, &outcome);

        // B4-1: POSITIVE PROOF that a `claude` lived in this pane, taken at the
        // exact moment the husk is created. Six of the eight outcomes carry the
        // pids they saw, and `claude_was_observed` reads THAT rather than
        // re-deriving it — so the latch the steward registry needs is written
        // by this executor, which runs on a timer the runner owns, and not by
        // an HTTP probe that an unattended box never receives.
        if claude_was_observed(&outcome) {
            effects.record_claude_seen(&terminal_id);
        }

        // D6: a steward the drain STOPPED is owed a restart on undrain. The
        // predicate is "`claude` left", NOT "the tab closed" — `drive` reaches
        // its close callback only after `GONE_PROBES_REQUIRED` consecutive gone
        // probes, so `CloseRefused` and `CloseOutcomeUnknown` both mean the
        // steward's `claude` is already gone and only the bare shell survives.
        // Recording just `Exited` left those as zombies: the pane reported
        // `running: true`, the kind was not in the set, undrain did not restart
        // it, a later start was refused 409, and the next tick could not retry
        // because a pane with no live `claude` never appears in
        // `terminal_hosted` again. Over-recording is cheap — the set is
        // idempotent and `restart_after_drain` answers a benign 409 with
        // `NotNeeded` — and under-recording is permanent.
        if effects.session_kind(&terminal_id) == SessionKind::Steward && claude_left(&outcome) {
            if let Some(kind) = effects.steward_kind(&terminal_id) {
                effects.record_stopped_by_drain(&kind).await;
            }
        }
        outcomes.push(CandidateOutcome::Attempted);
    }
    outcomes
}

/// The shipped [`CloseEffects`] — the real drain state, the real observer, and
/// `TerminalManager::graceful_exit`.
struct LiveCloseEffects<'a> {
    app: &'a tauri::AppHandle,
    grace: Duration,
    manager: Arc<TerminalManager>,
    store: Option<Arc<SessionLifecycleStore>>,
    fresh: &'a wind_down_observer::FreshPass,
}

#[async_trait::async_trait]
impl CloseEffects for LiveCloseEffects<'_> {
    fn still_drained(&self) -> bool {
        decide_tick(&crate::coord_drain_state::current(), false, Utc::now()) == TickAction::WindDown
    }

    async fn recheck(
        &self,
        session_id: &str,
        terminal_id: &str,
    ) -> Option<wind_down::WindDownView> {
        wind_down_observer::recheck(self.app, self.grace, session_id, terminal_id).await
    }

    async fn wind_down_one(
        &self,
        session_id: &str,
        terminal_id: &str,
    ) -> Result<GracefulExitOutcome, String> {
        let kind = self.fresh.observed.kind_for(terminal_id);
        let observation = self.fresh.observed.observation_for(terminal_id);
        let origin = self
            .fresh
            .pass
            .as_ref()
            .and_then(|pass| {
                pass.open_records
                    .iter()
                    .find(|r| r.claude_session_id == session_id)
            })
            .and_then(|r| r.origin.clone());
        info!(
            session_id = %session_id,
            terminal_id = %terminal_id,
            ?kind,
            origin = origin.as_deref().unwrap_or("-"),
            steward_kind = self.steward_kind(terminal_id).as_deref().unwrap_or("-"),
            sideband = ?observation.sideband,
            grid = ?observation.grid,
            judged_at_ms = self.fresh.observed.now_ms,
            grace_s = self.grace.as_secs(),
            "wind_down_executor: graceful /exit — eligible past grace"
        );

        // The ONE way this module reaches a pane. Never `TerminalManager::close`.
        let outcome = self
            .manager
            .graceful_exit(terminal_id, EXIT_DEADLINE)
            .await?;
        if matches!(outcome, GracefulExitOutcome::Exited { .. }) {
            info!(session_id = %session_id, terminal_id = %terminal_id, ?kind, "wind_down_executor: closed");
        } else {
            warn!(session_id = %session_id, terminal_id = %terminal_id, ?outcome, "wind_down_executor: not closed");
        }
        Ok(outcome)
    }

    fn session_kind(&self, terminal_id: &str) -> SessionKind {
        self.fresh.observed.kind_for(terminal_id)
    }

    fn steward_kind(&self, terminal_id: &str) -> Option<String> {
        crate::mcp::steward::steward_kind_for_terminal(terminal_id)
    }

    fn record_outcome(&self, session_id: &str, outcome: &GracefulExitOutcome) {
        match (outcome_word(outcome), &self.store) {
            (Some(word), Some(store)) => {
                store.set_wind_down_outcome(session_id, word, Utc::now().timestamp_millis())
            }
            _ => {
                debug!(session_id = %session_id, ?outcome, "wind_down_executor: nothing recorded for this outcome")
            }
        }
    }

    fn record_claude_seen(&self, terminal_id: &str) {
        crate::mcp::steward::record_claude_seen(terminal_id);
    }

    async fn record_stopped_by_drain(&self, kind: &str) {
        record_stopped_by_drain(kind.to_string()).await;
    }
}

/// The undrain half: restart exactly the stewards the drain stopped, and remove
/// each kind from the persisted set only once ITS OWN restart has reached a
/// terminal outcome.
///
/// Returns whether anything is STILL OWED, which the caller folds back into
/// `was_drained` so a kind left behind is retried on a later undrain rather
/// than silently dropped.
///
/// ## Why per-kind, and not "drain the set, then restart"
///
/// Draining it up front loses stewards two ways, and both are ordinary:
///
/// * the drain RE-ARMS between the undrain decision and a restart. The restart
///   is deferred by the gate (a 409 carrying a `DeferClass` code), the kind is
///   already out of the set, and nothing puts it back — the wind-down tick
///   cannot, because there is no tab left to close. The steward is gone until a
///   human notices. Here the kind stays in the set.
/// * the process DIES between the save and the restarts, losing every kind at
///   once. Here at most the one in flight is at risk.
///
/// A hard error (not a deferral) still removes the kind: that is a steward an
/// operator has to look at, and retrying it every 30 s forever — which is what
/// `was_drained` staying true would do — buries the message it needs to send.
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
///   `None` the moment the gate allows, and a tab this module closed is no
///   longer registered with the `TerminalManager`, so `resolve_live_session`
///   reports `Liveness::Dead` and `policy::decide` takes its self-heal arm. The
///   spawn is a `DeathRespawn` (`ever_spawned` is true for any tab wind-down
///   could have closed) and is backoff-gated, not cadence-gated.
async fn undrain(app: &tauri::AppHandle) -> bool {
    let mut set = load_stopped_by_drain().await;
    if set.is_empty() {
        info!("wind_down_executor: the drain lifted — nothing was stopped by it");
        return false;
    }
    let kinds: Vec<String> = set.kinds().map(str::to_string).collect();
    info!(
        ?kinds,
        "wind_down_executor: the drain lifted — restarting the stewards it stopped"
    );
    for kind in kinds {
        // Restarted on the roster's DEFAULTS: the mode and interval a stopped
        // steward was launched with are not persisted (the metadata store is
        // in-memory and the close removed the row), so reconstructing them
        // would be invention. An operator who launched a non-default mode over
        // the API relaunches it the same way.
        let outcome = crate::mcp::steward::restart_after_drain(app.clone(), &kind).await;
        let settled = match &outcome {
            Ok(crate::mcp::steward::RestartOutcome::Started) => {
                info!(steward = %kind, "wind_down_executor: restarted");
                true
            }
            Ok(crate::mcp::steward::RestartOutcome::NotNeeded(why)) => {
                info!(steward = %kind, "wind_down_executor: no restart needed — {why}");
                true
            }
            // The one arm that is NOT settled: the drain re-armed, so this kind
            // is still owed and stays in the set for a later undrain.
            Ok(crate::mcp::steward::RestartOutcome::Deferred(why)) => {
                info!(steward = %kind, "wind_down_executor: restart deferred by the drain, still owed — {why}");
                false
            }
            Err(e) => {
                warn!(steward = %kind, error = %e, "wind_down_executor: could not restart the steward the drain stopped — it needs an operator");
                true
            }
        };
        if settled && set.remove(&kind) {
            // Persisted per kind, so a crash costs at most the one in flight.
            save_stopped_by_drain(set.clone()).await;
        }
    }
    let still_owed = !set.is_empty();
    if still_owed {
        info!(
            owed = ?set.kinds().collect::<Vec<_>>(),
            "wind_down_executor: stewards still owed a restart — retried on a later undrain"
        );
    }
    still_owed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
            // RECEIVER-AGNOSTIC. Naming `manager.close(` alone was the hole:
            // renaming the binding to `mgr` walked straight past it. This
            // module legitimately calls `.close(` on nothing at all, so the
            // bare method name is the right thing to forbid.
            ".close(",
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
        // ...and the strip itself must not have eaten the module: a filter bug
        // that returned nothing would pass every forbidden-literal check.
        assert!(
            body.len() > 2_000,
            "the comment strip left {} bytes — the scan is vacuous",
            body.len()
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

    /// S-2: a kind leaves the set ONE AT A TIME, after its own restart settled.
    /// The all-or-nothing `drain_all` this replaced lost every kind to a crash
    /// between the save and the restarts, and lost a deferred one permanently.
    #[test]
    fn a_kind_leaves_the_set_one_at_a_time() {
        let mut set = StoppedByDrain::default();
        set.insert("dev-ops");
        set.insert("merge-train");
        assert!(set.remove("dev-ops"));
        assert!(!set.remove("dev-ops"), "removing twice is a no-op");
        assert_eq!(set.kinds().collect::<Vec<_>>(), vec!["merge-train"]);
        assert!(!set.is_empty(), "the undeferred kind is still owed");
        assert!(set.remove("merge-train"));
        assert!(set.is_empty());
    }

    // ── S-4: the two pure halves of a pass, tested directly ─────────────
    //
    // Invariants 2 and 3 below are Phase-1 properties re-asserted at the seam:
    // no mutation confined to THIS module can make either fail. These are the
    // ones this module owns, and each of them fails on a mutation that would
    // otherwise close a session it must not.

    fn candidate_proc(
        pid: u32,
        session_id: Option<&str>,
        verdict: Option<&str>,
        nested: bool,
    ) -> LiveClaudeProcess {
        LiveClaudeProcess {
            pid,
            parent_pid: None,
            image: Some("claude".to_string()),
            age_s: Some(60),
            cwd: None,
            has_live_children: Some(false),
            nested_under_claude: nested,
            session_id: session_id.map(str::to_string),
            session_status: Some("finished".to_string()),
            blocks_restart: false,
            wind_down: verdict.map(|eligibility| wind_down::WindDownView {
                eligibility: match eligibility {
                    "eligible" => "eligible",
                    "not_yet" => "not_yet",
                    "ineligible" => "ineligible",
                    _ => "unknown",
                },
                since: None,
                until: None,
                reason: None,
                kind: SessionKind::Terminal,
            }),
        }
    }

    fn observed_with(pairs: &[(&str, &str)]) -> wind_down_observer::ObservedInputs {
        wind_down_observer::ObservedInputs {
            terminal_by_session: pairs
                .iter()
                .map(|(s, t)| (s.to_string(), t.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    /// Only an `Eligible` verdict is a candidate — the mutation that deletes
    /// the filter, and the one that weakens it to "has a verdict at all", both
    /// close sessions the grace period is still protecting.
    #[test]
    fn only_an_eligible_verdict_is_a_candidate() {
        let observed = observed_with(&[
            ("s-ok", "t1"),
            ("s-not-yet", "t2"),
            ("s-inel", "t3"),
            ("s-unk", "t4"),
        ]);
        let processes = vec![
            candidate_proc(1, Some("s-ok"), Some("eligible"), false),
            candidate_proc(2, Some("s-not-yet"), Some("not_yet"), false),
            candidate_proc(3, Some("s-inel"), Some("ineligible"), false),
            candidate_proc(4, Some("s-unk"), Some("unknown"), false),
        ];
        assert_eq!(
            select_candidates(&processes, &observed),
            vec![("s-ok".to_string(), "t1".to_string())]
        );
    }

    /// S-A: the per-close re-check, as a property rather than a code shape.
    /// Deleting the re-check or weakening it to an index no batch reaches is
    /// what this catches — both are green across every other test.
    #[test]
    fn only_the_first_candidate_skips_the_recheck_and_every_other_fails_closed() {
        let eligible = wind_down::WindDownView {
            eligibility: "eligible",
            since: Some(1),
            until: None,
            reason: None,
            kind: SessionKind::Terminal,
        };
        let not_yet = wind_down::WindDownView {
            eligibility: "not_yet",
            since: Some(1),
            until: Some(2),
            reason: None,
            kind: SessionKind::Terminal,
        };

        // Index 0 owes nothing: its verdict is the pass that just ran.
        assert!(!recheck_is_owed(0));
        assert!(recheck_admits(0, None));

        // Every later index owes one, and NOTHING but a fresh `Eligible`
        // admits — not a stale verdict, not a missing one.
        for index in 1..8 {
            assert!(recheck_is_owed(index), "index {index} must owe a re-check");
            assert!(
                !recheck_admits(index, None),
                "index {index}: a session the re-check could not find must not be closed"
            );
            assert!(
                !recheck_admits(index, Some(&not_yet)),
                "index {index}: a session whose grace clock restarted must not be closed"
            );
            assert!(recheck_admits(index, Some(&eligible)), "index {index}");
        }
    }

    /// The mutation the reviewer named: weakening the condition to an index no
    /// batch reaches must not silently restore per-tick behaviour.
    #[test]
    fn every_index_a_batch_can_reach_owes_a_recheck_except_the_first() {
        let owed: Vec<bool> = (0..MAX_CLOSES_PER_TICK).map(recheck_is_owed).collect();
        // Built from the constant, not written out: a hardcoded vector fails on
        // any change to `MAX_CLOSES_PER_TICK`, including a correct one, while
        // pinning nothing this does not.
        let expected: Vec<bool> = std::iter::once(false)
            .chain(std::iter::repeat(true))
            .take(MAX_CLOSES_PER_TICK)
            .collect();
        assert_eq!(
            owed, expected,
            "with MAX_CLOSES_PER_TICK = {MAX_CLOSES_PER_TICK}, exactly the first \
             candidate may skip the re-check"
        );
    }

    // ── The close batch, at its call site ──────────────────────────────────
    //
    // S3-2: the pure helpers `recheck_is_owed` / `recheck_admits` stay green if
    // the block that CALLS them is deleted outright. S4-2: so did the D6
    // steward recording, while the seam sat above it. These drive
    // [`close_batch`] with a recording double whose seam is BELOW
    // `graceful_exit`, so the wiring — the order, the skips, and the two
    // records — is what is asserted.

    /// A [`CloseEffects`] that records every call and answers from a script.
    struct RecordingEffects {
        /// `still_drained` answers by index of call: `drained[n]`, then the
        /// last value forever.
        drained: Vec<bool>,
        /// What `recheck` answers, by terminal id. Absent = `None` (gone).
        verdicts: HashMap<String, wind_down::WindDownView>,
        /// What `wind_down_one` answers, by terminal id. Absent = `Exited`
        /// with one pid — the ordinary case.
        outcomes: HashMap<String, Result<GracefulExitOutcome, String>>,
        /// Which terminals hold a steward, and of which kind.
        stewards: HashMap<String, String>,
        calls: std::sync::Mutex<Vec<String>>,
        drained_calls: std::sync::atomic::AtomicUsize,
    }

    impl RecordingEffects {
        fn new() -> Self {
            Self {
                drained: vec![true],
                verdicts: HashMap::new(),
                outcomes: HashMap::new(),
                stewards: HashMap::new(),
                calls: std::sync::Mutex::new(Vec::new()),
                drained_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn eligible(mut self, terminal_id: &str) -> Self {
            self.verdicts
                .insert(terminal_id.to_string(), view("eligible"));
            self
        }
        fn not_yet(mut self, terminal_id: &str) -> Self {
            self.verdicts
                .insert(terminal_id.to_string(), view("not_yet"));
            self
        }
        fn drained(mut self, script: &[bool]) -> Self {
            self.drained = script.to_vec();
            self
        }
        fn steward(mut self, terminal_id: &str, kind: &str) -> Self {
            self.stewards
                .insert(terminal_id.to_string(), kind.to_string());
            self
        }
        fn outcome(mut self, terminal_id: &str, outcome: GracefulExitOutcome) -> Self {
            self.outcomes.insert(terminal_id.to_string(), Ok(outcome));
            self
        }
        fn exit_fails(mut self, terminal_id: &str) -> Self {
            self.outcomes
                .insert(terminal_id.to_string(), Err("no such pane".to_string()));
            self
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn log(&self, call: String) {
            self.calls.lock().unwrap().push(call);
        }
    }

    fn view(eligibility: &'static str) -> wind_down::WindDownView {
        wind_down::WindDownView {
            eligibility,
            since: Some(1),
            until: None,
            reason: None,
            kind: SessionKind::Terminal,
        }
    }

    fn exited() -> GracefulExitOutcome {
        GracefulExitOutcome::Exited {
            waited_ms: 10,
            claude_pids: vec![42],
        }
    }

    #[async_trait::async_trait]
    impl CloseEffects for RecordingEffects {
        fn still_drained(&self) -> bool {
            let n = self
                .drained_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.drained.get(n).unwrap_or(self.drained.last().unwrap())
        }
        async fn recheck(
            &self,
            _session_id: &str,
            terminal_id: &str,
        ) -> Option<wind_down::WindDownView> {
            self.log(format!("recheck:{terminal_id}"));
            self.verdicts.get(terminal_id).cloned()
        }
        async fn wind_down_one(
            &self,
            _session_id: &str,
            terminal_id: &str,
        ) -> Result<GracefulExitOutcome, String> {
            self.log(format!("exit:{terminal_id}"));
            match self.outcomes.get(terminal_id) {
                Some(Ok(outcome)) => Ok(outcome.clone()),
                Some(Err(e)) => Err(e.clone()),
                None => Ok(exited()),
            }
        }
        fn session_kind(&self, terminal_id: &str) -> SessionKind {
            if self.stewards.contains_key(terminal_id) {
                SessionKind::Steward
            } else {
                SessionKind::Terminal
            }
        }
        fn steward_kind(&self, terminal_id: &str) -> Option<String> {
            self.stewards.get(terminal_id).cloned()
        }
        fn record_outcome(&self, session_id: &str, _outcome: &GracefulExitOutcome) {
            self.log(format!("record_outcome:{session_id}"));
        }
        fn record_claude_seen(&self, terminal_id: &str) {
            self.log(format!("claude_seen:{terminal_id}"));
        }
        async fn record_stopped_by_drain(&self, kind: &str) {
            self.log(format!("stopped_by_drain:{kind}"));
        }
    }

    fn batch(ids: &[&str]) -> Vec<(String, String)> {
        ids.iter()
            .map(|t| (format!("s-{t}"), (*t).to_string()))
            .collect()
    }

    /// B1 AT THE CALL SITE. The first candidate is closed on the pass's own
    /// verdict; every later one is re-observed FIRST, and the close happens
    /// only if that answer admits it.
    ///
    /// Deleting the re-check block from [`close_batch`] fails here on the call
    /// ORDER, which no pure-helper test can see.
    #[tokio::test]
    async fn the_first_candidate_is_closed_unrechecked_and_the_rest_are_rechecked_first() {
        let effects = RecordingEffects::new().eligible("t2").eligible("t3");
        let outcomes = close_batch(batch(&["t1", "t2", "t3"]), &effects).await;

        let order: Vec<String> = effects
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("recheck:") || c.starts_with("exit:"))
            .collect();
        assert_eq!(
            order,
            vec![
                // No `recheck:t1` — index 0 is exempt, and only index 0.
                "exit:t1",
                "recheck:t2",
                "exit:t2",
                "recheck:t3",
                "exit:t3",
            ],
            "every candidate after the first must be re-observed BEFORE it is closed"
        );
        assert_eq!(outcomes, vec![CandidateOutcome::Attempted; 3]);
    }

    /// The scenario B1 exists for: the operator came back to D while A was
    /// being closed. A re-check that answers `NotYet` must skip the close and
    /// keep going — not close it, and not abandon the batch.
    #[tokio::test]
    async fn a_candidate_whose_grace_clock_restarted_is_skipped_not_closed() {
        let effects = RecordingEffects::new().not_yet("t2").eligible("t3");
        let outcomes = close_batch(batch(&["t1", "t2", "t3"]), &effects).await;

        assert!(
            !effects.calls().contains(&"exit:t2".to_string()),
            "t2 was re-observed and must NOT have been closed"
        );
        assert_eq!(
            outcomes,
            vec![
                CandidateOutcome::Attempted,
                CandidateOutcome::NoLongerEligible,
                CandidateOutcome::Attempted
            ]
        );
    }

    /// FAIL CLOSED. A re-check that cannot find the session answers `None`, and
    /// `None` must never be read as "nothing changed".
    #[tokio::test]
    async fn a_candidate_the_recheck_cannot_find_is_not_closed() {
        let effects = RecordingEffects::new();
        let outcomes = close_batch(batch(&["t1", "t2"]), &effects).await;

        assert!(!effects.calls().contains(&"exit:t2".to_string()));
        assert_eq!(
            outcomes,
            vec![
                CandidateOutcome::Attempted,
                CandidateOutcome::NoLongerEligible
            ]
        );
    }

    /// "Only while `Drained`" is a per-CLOSE property, not a per-tick one: a
    /// batch can run for minutes. The drain lifting mid-batch abandons every
    /// remaining candidate.
    #[tokio::test]
    async fn the_drain_lifting_mid_batch_stops_the_pass() {
        let effects = RecordingEffects::new()
            .drained(&[true, false])
            .eligible("t2")
            .eligible("t3");
        let outcomes = close_batch(batch(&["t1", "t2", "t3"]), &effects).await;

        assert!(
            !effects.calls().iter().any(|c| c.ends_with(":t2")),
            "t2 must not even be re-observed once the drain has lifted"
        );
        assert_eq!(
            outcomes,
            vec![CandidateOutcome::Attempted, CandidateOutcome::DrainLifted]
        );
    }

    /// S5 (thundering herd): the per-tick budget is applied by the batch, so a
    /// census with fifty eligible sessions closes four and leaves the rest to
    /// the next tick.
    #[tokio::test]
    async fn the_batch_never_exceeds_the_per_tick_budget() {
        let ids: Vec<String> = (0..12).map(|i| format!("t{i}")).collect();
        let mut effects = RecordingEffects::new();
        for id in &ids {
            effects = effects.eligible(id);
        }
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let outcomes = close_batch(batch(&refs), &effects).await;

        assert_eq!(outcomes.len(), MAX_CLOSES_PER_TICK);
        assert_eq!(
            effects
                .calls()
                .iter()
                .filter(|c| c.starts_with("exit:"))
                .count(),
            MAX_CLOSES_PER_TICK
        );
    }

    /// S4-2: D6, at the call site. A steward whose `claude` left is recorded as
    /// owed a restart — and a plain terminal session is NOT.
    ///
    /// The seam used to sit above this, so deleting the whole
    /// `if kind == Steward && claude_left(..)` block left every batch test
    /// green. It does not now.
    #[tokio::test]
    async fn a_steward_whose_claude_left_is_recorded_as_owed_a_restart() {
        let effects = RecordingEffects::new()
            .steward("t1", "merge-train")
            .eligible("t2");
        close_batch(batch(&["t1", "t2"]), &effects).await;

        let calls = effects.calls();
        assert!(
            calls.contains(&"stopped_by_drain:merge-train".to_string()),
            "the steward kind must be owed a restart on undrain: {calls:?}"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.starts_with("stopped_by_drain:"))
                .count(),
            1,
            "t2 is an ordinary terminal session and owes nothing: {calls:?}"
        );
    }

    /// D6's predicate is "`claude` left", not "the tab closed". A refused close
    /// is reached only AFTER `claude` is gone, so it owes the restart too —
    /// recording only `Exited` is what left those stewards down.
    #[tokio::test]
    async fn a_steward_whose_close_was_refused_still_owes_a_restart() {
        let effects = RecordingEffects::new().steward("t1", "dev-ops").outcome(
            "t1",
            GracefulExitOutcome::CloseRefused {
                waited_ms: 10,
                claude_pids: vec![7],
                reason: "pane not provably clear".to_string(),
            },
        );
        close_batch(batch(&["t1"]), &effects).await;

        assert!(effects
            .calls()
            .contains(&"stopped_by_drain:dev-ops".to_string()));
    }

    /// B4-1 AT THE CALL SITE. The latch the steward registry depends on is
    /// written by THIS executor, from evidence the exit already gathered — not
    /// by an HTTP probe an unattended runner never receives.
    ///
    /// This is the test that fails if the `record_claude_seen` call is deleted,
    /// and the scenario it stands for is the whole plan's: a drained runner
    /// nobody is watching.
    #[tokio::test]
    async fn an_exit_that_saw_a_claude_latches_it_on_the_steward_registry() {
        let effects = RecordingEffects::new().steward("t1", "merge-train");
        close_batch(batch(&["t1"]), &effects).await;

        assert!(
            effects.calls().contains(&"claude_seen:t1".to_string()),
            "a graceful exit carrying claude_pids is proof the pane was occupied"
        );
    }

    /// ...and an exit that saw NOTHING must not latch it. `NoLiveClaude` is
    /// "looked, found none"; latching on it would mark a pane occupied that
    /// never was, which is how a pane that is merely starting gets reaped.
    #[tokio::test]
    async fn an_exit_that_saw_no_claude_latches_nothing() {
        let effects = RecordingEffects::new()
            .steward("t1", "merge-train")
            .outcome("t1", GracefulExitOutcome::NoLiveClaude);
        close_batch(batch(&["t1"]), &effects).await;

        let calls = effects.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("claude_seen:")),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("stopped_by_drain:")),
            "and nothing left, so nothing is owed: {calls:?}"
        );
    }

    /// S4-3: an exit that could not START is not an attempt at anything. The
    /// enum used to call this `Closed`.
    #[tokio::test]
    async fn an_exit_that_could_not_start_is_not_reported_as_attempted() {
        let effects = RecordingEffects::new().exit_fails("t1").eligible("t2");
        let outcomes = close_batch(batch(&["t1", "t2"]), &effects).await;

        assert_eq!(
            outcomes,
            vec![
                CandidateOutcome::ExitNotStarted,
                CandidateOutcome::Attempted
            ]
        );
        let calls = effects.calls();
        assert!(
            !calls.contains(&"record_outcome:s-t1".to_string()),
            "nothing happened to that session, so nothing is stamped on it: {calls:?}"
        );
        assert!(
            !calls.contains(&"claude_seen:t1".to_string()),
            "an exit that never started gathered no evidence to latch: {calls:?}"
        );
        // Scoped to t1 deliberately: t2's exit DID run and DID see a claude, so
        // it latches. An `any(starts_with("claude_seen:"))` here would be
        // asserting that the batch stops after a failed exit, which is a
        // different (and wrong) claim.
        assert!(calls.contains(&"claude_seen:t2".to_string()));
    }

    /// `claude_was_observed` reads the pids the exit actually saw, so a variant
    /// that carries an EMPTY list is not evidence either.
    #[test]
    fn only_an_outcome_carrying_pids_is_evidence_a_claude_was_there() {
        assert!(claude_was_observed(&exited()));
        assert!(!claude_was_observed(&GracefulExitOutcome::NoLiveClaude));
        assert!(
            !claude_was_observed(&GracefulExitOutcome::ProbeUnavailable {
                detail: "unreadable".to_string()
            }),
            "could not look is never evidence"
        );
        assert!(
            !claude_was_observed(&GracefulExitOutcome::Exited {
                waited_ms: 1,
                claude_pids: vec![],
            }),
            "an empty pid list proves nothing, whatever the variant"
        );
        // `ExitStuck` means claude is STILL there — the strongest evidence of
        // all, and one `claude_left` deliberately excludes.
        let stuck = GracefulExitOutcome::ExitStuck {
            waited_ms: 1,
            claude_pids: vec![9],
            last_probe_unreadable: false,
        };
        assert!(claude_was_observed(&stuck));
        assert!(!claude_left(&stuck));
    }

    /// N-4: the nested check is the function's own, not a collaborator's.
    #[test]
    fn a_nested_subagent_is_not_a_candidate_even_with_an_eligible_verdict() {
        let observed = observed_with(&[("s-nested", "t1")]);
        // An eligible verdict AND nested — the observer would not produce this,
        // which is exactly why the function must refuse it itself.
        let processes = vec![candidate_proc(1, Some("s-nested"), Some("eligible"), true)];
        assert!(select_candidates(&processes, &observed).is_empty());
    }

    /// A process with NO verdict is not a candidate either — that is how a
    /// nested subagent (and anything the observer could not judge) is excluded.
    /// `.is_some()` in place of `.is_eligible()` fails here.
    #[test]
    fn a_process_with_no_verdict_is_never_a_candidate() {
        let observed = observed_with(&[("s-nested", "t1")]);
        let processes = vec![candidate_proc(1, Some("s-nested"), None, true)];
        assert!(select_candidates(&processes, &observed).is_empty());
    }

    /// An eligible process the pass cannot resolve to a pane is skipped rather
    /// than closed against a guessed terminal.
    #[test]
    fn an_eligible_process_with_no_pane_is_skipped() {
        let processes = vec![
            candidate_proc(1, Some("s-unmapped"), Some("eligible"), false),
            candidate_proc(2, None, Some("eligible"), false),
        ];
        assert!(select_candidates(&processes, &observed_with(&[])).is_empty());
    }

    /// ONE candidate per PANE: two top-level `claude` processes attributed to
    /// the same terminal must not spend two of the tick's budget slots on one
    /// close, the second of which lands on a pane that is already gone.
    #[test]
    fn two_processes_on_one_pane_yield_one_candidate() {
        let observed = observed_with(&[("s-a", "t1"), ("s-b", "t1"), ("s-c", "t2")]);
        let processes = vec![
            candidate_proc(1, Some("s-a"), Some("eligible"), false),
            candidate_proc(2, Some("s-b"), Some("eligible"), false),
            candidate_proc(3, Some("s-c"), Some("eligible"), false),
        ];
        let got = select_candidates(&processes, &observed);
        assert_eq!(
            got,
            vec![
                ("s-a".to_string(), "t1".to_string()),
                ("s-c".to_string(), "t2".to_string())
            ],
            "the first process on a pane wins and the second is dropped"
        );
    }

    /// Every outcome maps to the word that is TRUE of it, and the two that
    /// happen before anything is typed claim nothing at all.
    #[test]
    fn every_outcome_maps_to_the_word_that_is_true_of_it() {
        use crate::session::session_lifecycle_store::{
            WIND_DOWN_CLOSED, WIND_DOWN_CLOSE_REFUSED, WIND_DOWN_CLOSE_UNKNOWN,
            WIND_DOWN_EXIT_STUCK, WIND_DOWN_NOT_ATTEMPTED,
        };
        let pids = vec![42];
        let cases: Vec<(GracefulExitOutcome, Option<&str>, bool)> = vec![
            (
                GracefulExitOutcome::Exited {
                    waited_ms: 1,
                    claude_pids: pids.clone(),
                },
                Some(WIND_DOWN_CLOSED),
                true,
            ),
            (
                GracefulExitOutcome::ExitStuck {
                    waited_ms: 1,
                    claude_pids: pids.clone(),
                    last_probe_unreadable: false,
                },
                Some(WIND_DOWN_EXIT_STUCK),
                // `claude` is STILL RUNNING: nothing was stopped, so nothing is
                // owed a restart.
                false,
            ),
            (
                GracefulExitOutcome::CloseRefused {
                    waited_ms: 1,
                    claude_pids: pids.clone(),
                    reason: "r".into(),
                },
                Some(WIND_DOWN_CLOSE_REFUSED),
                true,
            ),
            (
                GracefulExitOutcome::CloseOutcomeUnknown {
                    waited_ms: 1,
                    claude_pids: pids.clone(),
                    detail: "d".into(),
                },
                Some(WIND_DOWN_CLOSE_UNKNOWN),
                true,
            ),
            (
                GracefulExitOutcome::Refused {
                    reason: "r".into(),
                    claude_pids: pids.clone(),
                },
                Some(WIND_DOWN_NOT_ATTEMPTED),
                false,
            ),
            (
                GracefulExitOutcome::WriteFailed {
                    error: "e".into(),
                    claude_pids: pids.clone(),
                },
                Some(WIND_DOWN_NOT_ATTEMPTED),
                false,
            ),
            (GracefulExitOutcome::NoLiveClaude, None, false),
            (
                GracefulExitOutcome::ProbeUnavailable { detail: "d".into() },
                None,
                false,
            ),
        ];
        for (outcome, word, left) in cases {
            assert_eq!(outcome_word(&outcome), word, "word for {outcome:?}");
            assert_eq!(claude_left(&outcome), left, "claude_left for {outcome:?}");
        }
    }

    /// S-1: the predicate a steward restart is owed on is "`claude` left", not
    /// "the tab closed". `CloseRefused` and `CloseOutcomeUnknown` are reached
    /// only AFTER the gone probes, so the steward really is stopped even though
    /// its bare shell survives.
    #[test]
    fn a_refused_close_still_counts_as_the_steward_having_stopped() {
        assert!(claude_left(&GracefulExitOutcome::CloseRefused {
            waited_ms: 1,
            claude_pids: vec![42],
            reason: "a claude reappeared".into(),
        }));
        assert!(claude_left(&GracefulExitOutcome::CloseOutcomeUnknown {
            waited_ms: 1,
            claude_pids: vec![42],
            detail: "the close task died".into(),
        }));
        assert!(
            !claude_left(&GracefulExitOutcome::ExitStuck {
                waited_ms: 1,
                claude_pids: vec![42],
                last_probe_unreadable: false,
            }),
            "an exit-stuck steward is still running and is owed nothing"
        );
    }

    /// The lifecycle write this module makes. Absent record = no-op; present
    /// record takes the word and the instant; a later write supersedes.
    #[test]
    fn the_wind_down_outcome_is_recorded_on_the_record_it_names() {
        use crate::session::session_lifecycle_store::SessionLifecycleStore;

        let dir = tempfile::tempdir().unwrap();
        let store =
            SessionLifecycleStore::open(&dir.path().join("terminal-sessions.json")).unwrap();
        // An absent record is a no-op, not an error and not a new row.
        store.set_wind_down_outcome("missing", WIND_DOWN_CLOSED, 10);
        assert!(store.get("missing").is_none());

        store.record_open(
            crate::session::session_lifecycle_store::TerminalSessionRecord {
                claude_session_id: "s1".to_string(),
                config_dir: None,
                working_dir: None,
                page_id: "default".to_string(),
                zone_index: 0,
                title: None,
                terminal_id: "t1".to_string(),
                opened_at: 1,
                last_seen_at: 2,
                state: "open".to_string(),
                closed_at: None,
                close_reason: None,
                provider: "claude".to_string(),
                origin: None,
                restore_pending_at: None,
                confirmed_at: None,
                handle: None,
                account_label: None,
                account_wrapper: None,
                session_name: None,
                name_source: None,
                tenant_id: None,
                task_run_id: None,
                bypass_permissions: None,
                restored_from_boot_at: None,
                restore_tier: None,
                finished_at: None,
                wind_down_outcome: None,
                wind_down_at: None,
                finish_reason: None,
                finish_synced: false,
            },
        );
        assert!(store.get("s1").unwrap().wind_down_outcome.is_none());

        store.set_wind_down_outcome("s1", WIND_DOWN_EXIT_STUCK, 111);
        let after = store.get("s1").unwrap();
        assert_eq!(
            after.wind_down_outcome.as_deref(),
            Some(WIND_DOWN_EXIT_STUCK)
        );
        assert_eq!(after.wind_down_at, Some(111));
        // Orthogonal to `state`: an exit-stuck session is still open.
        assert_eq!(after.state, "open");

        store.set_wind_down_outcome("s1", WIND_DOWN_CLOSED, 222);
        let later = store.get("s1").unwrap();
        assert_eq!(later.wind_down_outcome.as_deref(), Some(WIND_DOWN_CLOSED));
        assert_eq!(later.wind_down_at, Some(222));
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
