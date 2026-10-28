//! The unattended spawn gate — paces every unattended PTY spawn and HOLDS it
//! while the machine reads loaded, immediately before the PTY opens.
//!
//! Plan `2026-10-28-runner-spawn-bursts-overwhelm-the-box-beyond-the-continuation-cap`,
//! Phase 2 (D1).
//!
//! ## Why it exists
//!
//! On 2026-10-27 a burst of gate continuations opened 19 PTYs in 17 s on one
//! box. The seam's own admission ([`crate::resource_guard::admit_spawn`])
//! refuses only at CRITICAL and otherwise proceeds with a warning, and it runs
//! AFTER the coord consume-claim — so refusing there would burn claimed work.
//! The continuation load guard defers at WARN, but it runs before the claim,
//! minutes before the PTY actually opens. Nothing sat at the moment that
//! mattered.
//!
//! This gate sits there. Every task-owned unattended terminal site awaits
//! [`admit`] immediately before its PTY create, and it:
//!
//! * **PACES** — one unattended PTY spawn per [`UNATTENDED_SPAWN_SPACING`],
//!   through one process-global slot cursor. The thread reading the seam
//!   consults is memoized for 250 ms, so spawns released 3 s apart each read a
//!   number that already includes the predecessor's reader/waiter threads;
//!   19 spawns on one reading cannot happen.
//! * **HOLDS** while [`crate::resource_guard::probe_for_spawn`] reads WARN or
//!   CRITICAL, re-probing every [`UNATTENDED_RECHECK_INTERVAL`], up to
//!   [`UNATTENDED_HOLD_DEADLINE`]. A hold keeps the claimed work instead of
//!   burning it. The thresholds are the ones the resource guard already folds —
//!   no second set.
//! * **RELEASES at the deadline** (`why=hold_deadline`). A standing WARN must
//!   not silently stop all unattended work, so the hold is bounded; after it,
//!   the seam's unchanged ladder applies (WARN proceeds with its log, CRITICAL
//!   refuses). The gate has no override channel: it never makes the seam admit
//!   what the seam would refuse.
//!
//! Operator origins (`!SpawnOrigin::is_autonomous()`) pass at once and do not
//! advance the pace cursor. `SpawnOrigin::Unknown` is autonomous: fail closed.
//!
//! ## Request-scoped doors refuse instead of holding
//!
//! An HTTP or relay create is a request with a bounded reply lane; holding it
//! is a timed-out request. Those doors call [`check_request`], which runs
//! [`decide`] ONCE and answers any hold as a typed refusal — `machine_loaded`
//! plus `retry_after_ms` (the next pace slot or the re-probe interval) — the
//! way their drain arm answers a drain.
//!
//! ## What it does not own
//!
//! The headless `spawn_claude_child` seam (open PR #2207 builds its own hold
//! loop there); whichever lands second collapses both onto [`decide`], so there
//! is one hold state machine. The continuation count (`ContinuationRegistry`
//! and its admission) belongs to plan `2026-10-28-gate-continuation-cap-is-a-real-permit`.
//!
//! ## Log grammar
//!
//! ```text
//! resource_guard: holding unattended <what> origin=<origin> work=<key> lane=<lane> observed=<n> limit=<n> why=<warn|critical|pace>
//! resource_guard: releasing unattended <what> origin=<origin> work=<key> held_ms=<n> why=<clear|hold_deadline|pace>
//! resource_guard: refusing unattended <what> origin=<origin> work=<key> lane=<lane> observed=<n> limit=<n> why=<warn|critical|pace> retry_after_ms=<n>
//! ```
//!
//! A pace hold has no lane reading, so it prints `lane=- observed=- limit=-`.
//!
//! ## Enforcement
//!
//! `runner_spawn_sites.txt` marks every autonomous row `pty|headless|n/a`;
//! `runner_spawn_sites::every_autonomous_terminal_site_passes_the_unattended_spawn_gate`
//! requires every `autonomous … pty` row's own fn body to name this module.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::coord_drain_state::{next_release_slot, SpawnOrigin};
use crate::resource_guard::SpawnGate;

/// Minimum spacing between two unattended PTY spawns. 12x the thread
/// reading's 250 ms memo, so each spawn reads a number that includes its
/// predecessor; 46 spawns pace over ~2.3 minutes instead of 17-60 s.
pub const UNATTENDED_SPAWN_SPACING: Duration = Duration::from_secs(3);

/// How often a pressure hold re-reads the probe.
pub const UNATTENDED_RECHECK_INTERVAL: Duration = Duration::from_secs(5);

/// The longest a pressure hold lasts before the spawn is released to the
/// seam's own ladder.
pub const UNATTENDED_HOLD_DEADLINE: Duration = Duration::from_secs(600);

/// Overrides [`UNATTENDED_SPAWN_SPACING`], in milliseconds. `0` disables pacing.
pub const SPACING_ENV: &str = "QONTINUI_UNATTENDED_SPAWN_SPACING_MS";

/// Overrides [`UNATTENDED_HOLD_DEADLINE`], in seconds. `0` disables the
/// pressure hold (every tripped reading releases at once as `hold_deadline`).
pub const DEADLINE_ENV: &str = "QONTINUI_UNATTENDED_HOLD_DEADLINE_SECS";

/// The machine-readable refusal code a request-scoped door answers with.
pub const MACHINE_LOADED_CODE: &str = "machine_loaded";

/// The gate's tunables. Read from the environment on every [`admit`] /
/// [`check_request`] so an override needs no restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateConfig {
    pub spacing: Duration,
    pub recheck: Duration,
    pub deadline: Duration,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            spacing: UNATTENDED_SPAWN_SPACING,
            recheck: UNATTENDED_RECHECK_INTERVAL,
            deadline: UNATTENDED_HOLD_DEADLINE,
        }
    }
}

impl GateConfig {
    /// The live configuration: defaults, overridden by [`SPACING_ENV`] and
    /// [`DEADLINE_ENV`].
    pub fn from_env() -> Self {
        Self::from_values(
            std::env::var(SPACING_ENV).ok().as_deref(),
            std::env::var(DEADLINE_ENV).ok().as_deref(),
        )
    }

    /// PURE: the configuration the two raw override values produce. An
    /// unparseable value keeps the default and says so, rather than silently
    /// becoming `0` (which would switch the gate off).
    pub fn from_values(spacing_ms: Option<&str>, deadline_secs: Option<&str>) -> Self {
        let mut cfg = Self::default();
        if let Some(raw) = spacing_ms {
            match raw.trim().parse::<u64>() {
                Ok(ms) => cfg.spacing = Duration::from_millis(ms),
                Err(_) => warn!(
                    "unattended_spawn_gate: {SPACING_ENV}={raw:?} is not a whole number of \
                     milliseconds; keeping {} ms",
                    cfg.spacing.as_millis()
                ),
            }
        }
        if let Some(raw) = deadline_secs {
            match raw.trim().parse::<u64>() {
                Ok(secs) => cfg.deadline = Duration::from_secs(secs),
                Err(_) => warn!(
                    "unattended_spawn_gate: {DEADLINE_ENV}={raw:?} is not a whole number of \
                     seconds; keeping {} s",
                    cfg.deadline.as_secs()
                ),
            }
        }
        cfg
    }
}

/// One lane reading that tripped, as the log and the refusal quote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tripped {
    pub lane: String,
    pub observed: u64,
    pub limit: u64,
}

/// The probe's verdict, reduced to what the gate decides on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pressure {
    Clear,
    Warn(Tripped),
    Critical(Tripped),
}

impl Pressure {
    fn tripped(&self) -> Option<(HoldWhy, &Tripped)> {
        match self {
            Pressure::Clear => None,
            Pressure::Warn(t) => Some((HoldWhy::Warn, t)),
            Pressure::Critical(t) => Some((HoldWhy::Critical, t)),
        }
    }
}

impl From<&SpawnGate> for Pressure {
    fn from(gate: &SpawnGate) -> Self {
        let tripped = |o: &crate::resource_guard::GateObservation| Tripped {
            lane: o.lane.clone(),
            observed: o.observed,
            limit: o.limit,
        };
        match gate {
            SpawnGate::Proceed => Pressure::Clear,
            SpawnGate::Warn(o) => Pressure::Warn(tripped(o)),
            SpawnGate::Critical(o) => Pressure::Critical(tripped(o)),
        }
    }
}

/// Why a spawn is being held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldWhy {
    Warn,
    Critical,
    /// Another unattended spawn holds the current pace slot.
    Pace,
}

impl HoldWhy {
    pub fn as_wire(self) -> &'static str {
        match self {
            HoldWhy::Warn => "warn",
            HoldWhy::Critical => "critical",
            HoldWhy::Pace => "pace",
        }
    }
}

/// Why a spawn was released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseWhy {
    /// A pressure hold ended because the probe read clear.
    Clear,
    /// A pressure hold reached the deadline; the seam's own ladder decides.
    HoldDeadline,
    /// No pressure hold: released on its pace slot (at once when it was free).
    Pace,
}

impl ReleaseWhy {
    pub fn as_wire(self) -> &'static str {
        match self {
            ReleaseWhy::Clear => "clear",
            ReleaseWhy::HoldDeadline => "hold_deadline",
            ReleaseWhy::Pace => "pace",
        }
    }
}

/// One step of the gate's state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStep {
    /// Open the PTY now. The caller claims the pace slot at `now`.
    Release(ReleaseWhy),
    /// Not yet: decide again at the instant given.
    Hold(HoldWhy, Instant),
}

/// PURE: the earliest instant the next unattended spawn may open, given the
/// instant the previous one was released.
///
/// [`next_release_slot`] answers "`now`, unless a queued instant is still
/// ahead, in which case that one"; the queued instant here is the moment the
/// previous release's slot frees up (`last + spacing`), so the result is
/// `max(now, last + spacing)` — a minimum spacing between consecutive
/// releases however far apart they arrive.
fn pace_slot(now: Instant, last_release: Option<Instant>, spacing: Duration) -> Instant {
    next_release_slot(
        now,
        last_release.map(|last| last + spacing),
        Duration::ZERO,
        Duration::ZERO,
    )
}

/// PURE: the gate's decision.
///
/// * `now` — the instant of this decision.
/// * `last_slot` — the instant the previous unattended spawn was released
///   (the pace cursor), or `None` before the first.
/// * `verdict` — the probe's reading, taken just now.
/// * `held_since` — when THIS spawn's pressure hold began, or `None` if it has
///   not been pressure-held.
///
/// Pressure first: a tripped reading holds until `held_since + deadline`,
/// re-probing every `recheck` (never past the deadline, so the release lands
/// on it exactly). Then pace: a spawn that may go still waits for its slot.
pub fn decide(
    now: Instant,
    last_slot: Option<Instant>,
    verdict: &Pressure,
    held_since: Option<Instant>,
    cfg: &GateConfig,
) -> GateStep {
    let mut deadline_reached = false;
    if let Some((why, _)) = verdict.tripped() {
        let deadline_at = held_since.unwrap_or(now) + cfg.deadline;
        if now < deadline_at {
            return GateStep::Hold(why, (now + cfg.recheck).min(deadline_at));
        }
        deadline_reached = true;
    }
    let slot = pace_slot(now, last_slot, cfg.spacing);
    if slot > now {
        return GateStep::Hold(HoldWhy::Pace, slot);
    }
    GateStep::Release(if deadline_reached {
        ReleaseWhy::HoldDeadline
    } else if held_since.is_some() {
        ReleaseWhy::Clear
    } else {
        ReleaseWhy::Pace
    })
}

/// The process-global pace cursor: the instant the last unattended spawn was
/// released. Claimed under the lock in the same critical section as the
/// decision, so two concurrent spawns can never both take one slot.
pub(crate) struct PaceCursor(Mutex<Option<Instant>>);

impl PaceCursor {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// Decide against the cursor and, on a release, claim the slot at `now`.
    fn step(
        &self,
        now: Instant,
        decide_with: impl FnOnce(Option<Instant>) -> GateStep,
    ) -> GateStep {
        let mut last = match self.0.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        let step = decide_with(*last);
        if matches!(step, GateStep::Release(_)) {
            *last = Some(now);
        }
        step
    }
}

fn global_cursor() -> &'static PaceCursor {
    static CURSOR: PaceCursor = PaceCursor::new();
    &CURSOR
}

/// What [`admit`] reports back. Callers may ignore it; the gate has already
/// logged and counted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    /// `None` for an operator origin, which the gate does not touch.
    pub why: Option<ReleaseWhy>,
    pub held_ms: u64,
    /// The reading the release was decided on. A `hold_deadline` release
    /// under `Critical` is still refused by the seam.
    pub pressure: Pressure,
}

impl Admission {
    fn operator() -> Self {
        Self {
            why: None,
            held_ms: 0,
            pressure: Pressure::Clear,
        }
    }
}

// --- counters, for `/health` `spawnAdmission.unattendedGate` ---------------

static HELD_NOW: AtomicU64 = AtomicU64::new(0);
static RELEASED_IMMEDIATE: AtomicU64 = AtomicU64::new(0);
static RELEASED_CLEAR: AtomicU64 = AtomicU64::new(0);
static RELEASED_HOLD_DEADLINE: AtomicU64 = AtomicU64::new(0);
static RELEASED_PACE: AtomicU64 = AtomicU64::new(0);
static REFUSED_TOTAL: AtomicU64 = AtomicU64::new(0);
static LAST_HELD_SINCE_MS: AtomicU64 = AtomicU64::new(0);

/// Decrements [`HELD_NOW`] however the hold ends — release, or the caller's
/// future being dropped mid-hold.
struct HeldGuard;

impl HeldGuard {
    fn enter() -> Self {
        HELD_NOW.fetch_add(1, Ordering::Relaxed);
        LAST_HELD_SINCE_MS.store(
            u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0),
            Ordering::Relaxed,
        );
        Self
    }
}

impl Drop for HeldGuard {
    fn drop(&mut self) {
        HELD_NOW.fetch_sub(1, Ordering::Relaxed);
    }
}

fn count_release(why: ReleaseWhy, held: bool) {
    let counter = match (held, why) {
        (false, _) => &RELEASED_IMMEDIATE,
        (true, ReleaseWhy::Clear) => &RELEASED_CLEAR,
        (true, ReleaseWhy::HoldDeadline) => &RELEASED_HOLD_DEADLINE,
        (true, ReleaseWhy::Pace) => &RELEASED_PACE,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

/// The `/health` `spawnAdmission.unattendedGate` block.
pub fn health_json() -> serde_json::Value {
    let cfg = GateConfig::from_env();
    let since = LAST_HELD_SINCE_MS.load(Ordering::Relaxed);
    serde_json::json!({
        "spacingMs": u64::try_from(cfg.spacing.as_millis()).unwrap_or(u64::MAX),
        "recheckMs": u64::try_from(cfg.recheck.as_millis()).unwrap_or(u64::MAX),
        "deadlineSecs": cfg.deadline.as_secs(),
        "spacingEnv": SPACING_ENV,
        "deadlineEnv": DEADLINE_ENV,
        "heldNow": HELD_NOW.load(Ordering::Relaxed),
        "lastHoldStartedAtMs": if since == 0 { serde_json::Value::Null } else { since.into() },
        "releasedTotal": {
            "immediate": RELEASED_IMMEDIATE.load(Ordering::Relaxed),
            "clear": RELEASED_CLEAR.load(Ordering::Relaxed),
            "hold_deadline": RELEASED_HOLD_DEADLINE.load(Ordering::Relaxed),
            "pace": RELEASED_PACE.load(Ordering::Relaxed),
        },
        "refusedTotal": REFUSED_TOTAL.load(Ordering::Relaxed),
    })
}

/// The live probe the gate decides on — the resource guard's two-lane verdict,
/// with its folded thresholds.
fn live_pressure() -> Pressure {
    Pressure::from(&crate::resource_guard::probe_for_spawn())
}

/// `lane=<lane> observed=<n> limit=<n>` for a tripped reading, dashes for pace.
fn reading_fields(pressure: &Pressure, why: HoldWhy) -> (String, String, String) {
    match (why, pressure.tripped()) {
        (HoldWhy::Pace, _) | (_, None) => ("-".into(), "-".into(), "-".into()),
        (_, Some((_, t))) => (t.lane.clone(), t.observed.to_string(), t.limit.to_string()),
    }
}

/// Wait until an unattended PTY spawn of `origin` may open, then return.
///
/// Task-owned sites only — a continuation, a condition check, a looping agent,
/// a task-started steward, a resume, a respawn, a handoff. Awaited immediately
/// before the PTY create. Returns at once for an operator origin. Never
/// refuses: a hold ends on a clear reading or at the deadline, and the seam's
/// own ladder then decides.
pub async fn admit(origin: SpawnOrigin, what: &str, work_key: &str) -> Admission {
    admit_with(
        origin,
        what,
        work_key,
        &GateConfig::from_env(),
        global_cursor(),
        live_pressure,
    )
    .await
}

/// [`admit`] over an injected configuration, cursor and probe — the seam the
/// tests drive under paused time.
pub(crate) async fn admit_with(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
    cfg: &GateConfig,
    cursor: &PaceCursor,
    mut probe: impl FnMut() -> Pressure,
) -> Admission {
    if !origin.is_autonomous() {
        return Admission::operator();
    }
    let started = Instant::now();
    let mut held_since: Option<Instant> = None;
    let mut announced: Option<HoldWhy> = None;
    let mut guard: Option<HeldGuard> = None;
    loop {
        let pressure = probe();
        let now = Instant::now();
        let step = cursor.step(now, |last| decide(now, last, &pressure, held_since, cfg));
        match step {
            GateStep::Release(why) => {
                let held_ms =
                    u64::try_from(now.duration_since(started).as_millis()).unwrap_or(u64::MAX);
                count_release(why, guard.is_some());
                if guard.is_some() {
                    info!(
                        "resource_guard: releasing unattended {what} origin={origin} \
                         work={work_key} held_ms={held_ms} why={}",
                        why.as_wire()
                    );
                } else {
                    debug!(
                        "resource_guard: admitting unattended {what} origin={origin} \
                         work={work_key} at once"
                    );
                }
                drop(guard);
                return Admission {
                    why: Some(why),
                    held_ms,
                    pressure,
                };
            }
            GateStep::Hold(why, at) => {
                if guard.is_none() {
                    guard = Some(HeldGuard::enter());
                }
                if why != HoldWhy::Pace && held_since.is_none() {
                    held_since = Some(now);
                }
                if announced != Some(why) {
                    let (lane, observed, limit) = reading_fields(&pressure, why);
                    info!(
                        "resource_guard: holding unattended {what} origin={origin} \
                         work={work_key} lane={lane} observed={observed} limit={limit} why={}",
                        why.as_wire()
                    );
                    announced = Some(why);
                }
                tokio::time::sleep_until(at).await;
            }
        }
    }
}

/// A request-scoped door's refusal: answer it at once, never hold the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub why: HoldWhy,
    pub retry_after_ms: u64,
    pub message: String,
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        MACHINE_LOADED_CODE
    }

    /// The HTTP 409 body: the reason, code `machine_loaded`, and
    /// `{retry_after_ms, why}` in `hint`.
    pub fn http_body(&self) -> crate::mcp::types::ApiResponse<()> {
        let mut body = crate::mcp::types::api_error(self.message.clone());
        body.code = Some(MACHINE_LOADED_CODE.to_string());
        body.hint = Some(serde_json::json!({
            "retry_after_ms": self.retry_after_ms,
            "why": self.why.as_wire(),
        }));
        body
    }

    /// The relay's error frame, shaped like its drain refusal frame.
    pub fn relay_frame(&self, request_id: Option<&serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "type": "error",
            "code": MACHINE_LOADED_CODE,
            "message": self.message,
            "retry_after_ms": self.retry_after_ms,
            "request_id": request_id,
        })
    }
}

/// For a request-scoped door: decide ONCE; a release claims the pace slot and
/// returns `Ok`, any hold is a [`Refusal`]. Never awaits.
pub fn check_request(origin: SpawnOrigin, what: &str, work_key: &str) -> Result<(), Refusal> {
    check_request_with(
        origin,
        what,
        work_key,
        &GateConfig::from_env(),
        global_cursor(),
        live_pressure,
    )
}

pub(crate) fn check_request_with(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
    cfg: &GateConfig,
    cursor: &PaceCursor,
    probe: impl FnOnce() -> Pressure,
) -> Result<(), Refusal> {
    if !origin.is_autonomous() {
        return Ok(());
    }
    let pressure = probe();
    let now = Instant::now();
    match cursor.step(now, |last| decide(now, last, &pressure, None, cfg)) {
        GateStep::Release(why) => {
            count_release(why, false);
            Ok(())
        }
        GateStep::Hold(why, at) => {
            REFUSED_TOTAL.fetch_add(1, Ordering::Relaxed);
            let retry_after_ms =
                u64::try_from(at.duration_since(now).as_millis()).unwrap_or(u64::MAX);
            let (lane, observed, limit) = reading_fields(&pressure, why);
            warn!(
                "resource_guard: refusing unattended {what} origin={origin} work={work_key} \
                 lane={lane} observed={observed} limit={limit} why={} \
                 retry_after_ms={retry_after_ms}",
                why.as_wire()
            );
            let detail = match pressure.tripped() {
                Some((_, t)) if why != HoldWhy::Pace => format!(
                    "the {} lane reads {} against its {} {} limit",
                    t.lane,
                    t.observed,
                    t.limit,
                    why.as_wire()
                ),
                _ => "another unattended spawn holds the current pace slot".to_string(),
            };
            Err(Refusal {
                why,
                retry_after_ms,
                message: format!(
                    "{MACHINE_LOADED_CODE}: not starting a new {what} now — {detail}. Retry in \
                     {retry_after_ms} ms."
                ),
            })
        }
    }
}

/// How a site that serves both a task and a request waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnWait {
    /// A task owns the spawn: [`admit`].
    Hold,
    /// A request is waiting on the answer: [`check_request`].
    Refuse,
}

/// [`admit`] or [`check_request`], by `wait`. For a door shared by a task and a
/// request (the steward start path).
pub async fn admit_or_refuse(
    wait: SpawnWait,
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
) -> Result<(), Refusal> {
    match wait {
        SpawnWait::Hold => {
            admit(origin, what, work_key).await;
            Ok(())
        }
        SpawnWait::Refuse => check_request(origin, what, work_key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warn_reading() -> Pressure {
        Pressure::Warn(Tripped {
            lane: "threads".into(),
            observed: 293,
            limit: 256,
        })
    }

    fn critical_reading() -> Pressure {
        Pressure::Critical(Tripped {
            lane: "threads".into(),
            observed: 412,
            limit: 400,
        })
    }

    fn cfg() -> GateConfig {
        GateConfig {
            spacing: Duration::from_millis(3_000),
            recheck: Duration::from_millis(5_000),
            deadline: Duration::from_secs(600),
        }
    }

    fn ms(d: Duration) -> u64 {
        u64::try_from(d.as_millis()).expect("fits")
    }

    /// The 19 arrival offsets (ms) of the densest 2026-10-27 burst — 19 starts
    /// in 17 s (20:30:24.9-20:30:41.3), spread as the minute counts measured.
    const BURST_19_IN_17S_MS: [u64; 19] = [
        0, 900, 1_800, 2_700, 3_600, 4_500, 5_400, 6_300, 7_200, 8_100, 9_000, 9_900, 10_800,
        11_700, 12_600, 13_500, 14_400, 15_300, 16_400,
    ];

    /// (a) The 2026-10-27 burst fixture: 19 admits requested within 17 s under
    /// `Proceed` release on a >= 3 s grid; the total span is >= 54 s.
    #[tokio::test(start_paused = true)]
    async fn a_the_19_in_17s_burst_releases_on_a_3s_grid() {
        let cursor = std::sync::Arc::new(PaceCursor::new());
        let cfg = cfg();
        let t0 = Instant::now();
        let mut handles = Vec::new();
        for offset in BURST_19_IN_17S_MS {
            let cursor = cursor.clone();
            handles.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(offset)).await;
                admit_with(
                    SpawnOrigin::GateContinuation,
                    "gate-continuation terminal",
                    "fixture",
                    &cfg,
                    &cursor,
                    || Pressure::Clear,
                )
                .await;
                Instant::now()
            }));
        }
        let mut released = Vec::new();
        for h in handles {
            released.push(h.await.expect("join"));
        }
        released.sort();
        assert_eq!(released.len(), 19);
        for pair in released.windows(2) {
            assert!(
                ms(pair[1] - pair[0]) >= 3_000,
                "two releases {} ms apart",
                ms(pair[1] - pair[0])
            );
        }
        assert_eq!(ms(released[0] - t0), 0, "the first goes at once");
        assert_eq!(ms(released[18] - released[0]), 54_000);
    }

    /// (b) WARN holds, then a clear reading releases `why=clear`.
    #[test]
    fn b_warn_holds_then_clear_releases_as_clear() {
        let t0 = Instant::now();
        let c = cfg();
        assert_eq!(
            decide(t0, None, &warn_reading(), None, &c),
            GateStep::Hold(HoldWhy::Warn, t0 + Duration::from_millis(5_000))
        );
        let t1 = t0 + Duration::from_millis(5_000);
        assert_eq!(
            decide(t1, None, &Pressure::Clear, Some(t0), &c),
            GateStep::Release(ReleaseWhy::Clear)
        );
    }

    /// (b), through `admit_with`: two WARN readings, then clear.
    #[tokio::test(start_paused = true)]
    async fn b_admit_holds_under_warn_and_releases_when_clear() {
        let cursor = PaceCursor::new();
        let mut readings = vec![Pressure::Clear, warn_reading(), warn_reading()];
        let t0 = Instant::now();
        let got = admit_with(
            SpawnOrigin::GateContinuation,
            "gate-continuation terminal",
            "anchor",
            &cfg(),
            &cursor,
            || readings.pop().expect("reading"),
        )
        .await;
        assert_eq!(got.why, Some(ReleaseWhy::Clear));
        assert_eq!(got.held_ms, 10_000);
        assert_eq!(ms(Instant::now() - t0), 10_000);
    }

    /// Drive `decide` under a standing reading from `t0` and return the release
    /// instant and why.
    fn run_until_release(reading: &Pressure, c: &GateConfig) -> (Instant, Instant, ReleaseWhy) {
        let t0 = Instant::now();
        let mut now = t0;
        let mut held_since = None;
        for _ in 0..1_000 {
            match decide(now, None, reading, held_since, c) {
                GateStep::Release(why) => return (t0, now, why),
                GateStep::Hold(why, at) => {
                    if why != HoldWhy::Pace && held_since.is_none() {
                        held_since = Some(now);
                    }
                    assert!(at > now, "a hold must move time forward");
                    now = at;
                }
            }
        }
        panic!("never released");
    }

    /// (c) A standing WARN releases at the deadline `why=hold_deadline`, never
    /// before it and never later than deadline + one recheck.
    #[test]
    fn c_a_standing_warn_releases_at_the_deadline() {
        let c = cfg();
        let (t0, at, why) = run_until_release(&warn_reading(), &c);
        assert_eq!(why, ReleaseWhy::HoldDeadline);
        let held = ms(at - t0);
        assert!(held >= 600_000, "released early at {held} ms");
        assert!(held <= 605_000, "released late at {held} ms");
        assert_eq!(held, 600_000);
        // One instant before the deadline it still holds.
        assert_eq!(
            decide(
                t0 + Duration::from_millis(599_999),
                None,
                &warn_reading(),
                Some(t0),
                &c
            ),
            GateStep::Hold(HoldWhy::Warn, t0 + Duration::from_secs(600))
        );
    }

    /// (d) CRITICAL holds the same way, and the deadline release carries the
    /// CRITICAL reading, which the seam (`admit_spawn`, `resource_override:
    /// false` at every gated site) still refuses — the gate has no override.
    #[tokio::test(start_paused = true)]
    async fn d_critical_holds_the_same_way_and_releases_still_critical() {
        let t0 = Instant::now();
        let c = cfg();
        assert_eq!(
            decide(t0, None, &critical_reading(), None, &c),
            GateStep::Hold(HoldWhy::Critical, t0 + Duration::from_millis(5_000))
        );
        let (s0, at, why) = run_until_release(&critical_reading(), &c);
        assert_eq!((why, ms(at - s0)), (ReleaseWhy::HoldDeadline, 600_000));

        let cursor = PaceCursor::new();
        let got = admit_with(
            SpawnOrigin::CoordDispatch,
            "condition-check terminal",
            "run",
            &c,
            &cursor,
            critical_reading,
        )
        .await;
        assert_eq!(got.why, Some(ReleaseWhy::HoldDeadline));
        assert_eq!(got.held_ms, 600_000);
        assert_eq!(got.pressure, critical_reading());
    }

    /// (e) Operator origins never hold; `Unknown` does.
    #[tokio::test(start_paused = true)]
    async fn e_operator_origins_never_hold_and_unknown_does() {
        let c = cfg();
        for origin in [SpawnOrigin::OperatorTerminal, SpawnOrigin::OperatorChat] {
            let cursor = PaceCursor::new();
            let t0 = Instant::now();
            let got = admit_with(origin, "terminal", "k", &c, &cursor, critical_reading).await;
            assert_eq!(got.why, None, "{origin}");
            assert_eq!(ms(Instant::now() - t0), 0, "{origin}");
            assert_eq!(
                check_request_with(origin, "terminal", "k", &c, &cursor, critical_reading),
                Ok(())
            );
        }
        let cursor = PaceCursor::new();
        let t0 = Instant::now();
        let got = admit_with(
            SpawnOrigin::Unknown,
            "terminal",
            "k",
            &c,
            &cursor,
            warn_reading,
        )
        .await;
        assert_eq!(got.why, Some(ReleaseWhy::HoldDeadline));
        assert_eq!(ms(Instant::now() - t0), 600_000);
        assert!(check_request_with(
            SpawnOrigin::Unknown,
            "terminal",
            "k",
            &c,
            &cursor,
            warn_reading
        )
        .is_err());
        // Every autonomous origin is gated; exactly the two operator ones are not.
        let ungated: Vec<_> = SpawnOrigin::ALL
            .into_iter()
            .filter(|o| {
                check_request_with(*o, "t", "k", &c, &PaceCursor::new(), warn_reading).is_ok()
            })
            .collect();
        assert_eq!(
            ungated,
            vec![SpawnOrigin::OperatorTerminal, SpawnOrigin::OperatorChat]
        );
    }

    /// (f) The same work key admitted twice is paced twice.
    #[tokio::test(start_paused = true)]
    async fn f_the_same_work_key_twice_is_paced_twice() {
        let cursor = PaceCursor::new();
        let c = cfg();
        let t0 = Instant::now();
        for _ in 0..2 {
            admit_with(
                SpawnOrigin::LoopingAgent,
                "looping-agent session",
                "looping_agent:same",
                &c,
                &cursor,
                || Pressure::Clear,
            )
            .await;
        }
        assert_eq!(ms(Instant::now() - t0), 3_000);
        let third = admit_with(
            SpawnOrigin::LoopingAgent,
            "looping-agent session",
            "looping_agent:same",
            &c,
            &cursor,
            || Pressure::Clear,
        )
        .await;
        assert_eq!(third.why, Some(ReleaseWhy::Pace));
        assert_eq!(ms(Instant::now() - t0), 6_000);
    }

    /// (g) A request-scoped site under WARN returns `machine_loaded` with no
    /// await at all (so none longer than one pace slot), and a pace conflict
    /// refuses with the slot's remaining time.
    #[tokio::test(start_paused = true)]
    async fn g_a_request_scoped_site_refuses_with_machine_loaded() {
        let cursor = PaceCursor::new();
        let c = cfg();
        let t0 = Instant::now();
        let refusal = check_request_with(
            SpawnOrigin::Unknown,
            "terminal session",
            "http_terminal:x",
            &c,
            &cursor,
            warn_reading,
        )
        .expect_err("WARN refuses");
        assert_eq!(ms(Instant::now() - t0), 0);
        assert_eq!(refusal.code(), "machine_loaded");
        assert_eq!(refusal.why, HoldWhy::Warn);
        assert_eq!(refusal.retry_after_ms, 5_000);
        assert_eq!(
            refusal.message,
            "machine_loaded: not starting a new terminal session now — the threads lane reads \
             293 against its 256 warn limit. Retry in 5000 ms."
        );
        let body = serde_json::to_value(refusal.http_body()).expect("serialize");
        assert_eq!(body["code"], "machine_loaded");
        assert_eq!(body["hint"]["retry_after_ms"], 5_000);
        assert_eq!(body["success"], false);
        let frame = refusal.relay_frame(Some(&serde_json::json!("req-1")));
        assert_eq!(frame["code"], "machine_loaded");
        assert_eq!(frame["retry_after_ms"], 5_000);
        assert_eq!(frame["request_id"], "req-1");

        // A clear reading admits and claims the slot; one second later the
        // next request is refused for pace with the 2 s that remain.
        assert_eq!(
            check_request_with(SpawnOrigin::Unknown, "t", "k", &c, &cursor, || {
                Pressure::Clear
            }),
            Ok(())
        );
        tokio::time::advance(Duration::from_millis(1_000)).await;
        let pace = check_request_with(SpawnOrigin::Unknown, "t", "k", &c, &cursor, || {
            Pressure::Clear
        })
        .expect_err("pace refuses");
        assert_eq!((pace.why, pace.retry_after_ms), (HoldWhy::Pace, 2_000));
    }

    #[test]
    fn env_overrides_parse_and_a_bad_value_keeps_the_default() {
        assert_eq!(GateConfig::from_values(None, None), cfg());
        let set = GateConfig::from_values(Some("1500"), Some("90"));
        assert_eq!(set.spacing, Duration::from_millis(1_500));
        assert_eq!(set.deadline, Duration::from_secs(90));
        let bad = GateConfig::from_values(Some("fast"), Some("-1"));
        assert_eq!(bad, cfg());
        // `0` is the kill switch: no pacing, and a tripped reading releases at once.
        let off = GateConfig::from_values(Some("0"), Some("0"));
        let t0 = Instant::now();
        assert_eq!(
            decide(t0, Some(t0), &warn_reading(), None, &off),
            GateStep::Release(ReleaseWhy::HoldDeadline)
        );
    }

    #[test]
    fn health_block_names_its_values() {
        let v = health_json();
        assert_eq!(v["spacingEnv"], "QONTINUI_UNATTENDED_SPAWN_SPACING_MS");
        assert_eq!(v["deadlineEnv"], "QONTINUI_UNATTENDED_HOLD_DEADLINE_SECS");
        for key in ["immediate", "clear", "hold_deadline", "pace"] {
            assert!(v["releasedTotal"][key].is_u64(), "{key}: {v}");
        }
        assert!(v["heldNow"].is_u64());
    }
}
