//! Pressure-stop events: when the resource guard refuses or defers work for
//! memory or thread pressure, post one durable event to coord carrying what the
//! box was carrying at that instant (plan
//! `2026-11-24-pressure-stops-record-box-stats-in-coord`, Phase 3).
//!
//! ## What "stop" means here
//!
//! No site in this runner kills an already-running process or session because of
//! a memory or thread verdict. Every enforcement is a refusal or deferral BEFORE
//! a child exists: a terminal PTY spawn, a runner-instance launch, a coord
//! launch, a gate continuation, a CI job admission. Those are the hook sites
//! ([`StopKind`]). A later change that adds a true kill calls the same
//! [`record_gate`]. Background-tick shedding is NOT an event: it is a throttle
//! that fires constantly by design and stops nothing. Kernel and cgroup OOM
//! kills are not this module's either — coord already records those as
//! `oom_kill` computer events.
//!
//! ## The admission path is never blocked
//!
//! [`record_gate`] / [`record_reading`] run on the refusal path, so they only
//! read cheap cached figures, push to a bounded in-memory queue and return.
//! Nothing here can fail the refusal: a panic while reading the box is caught
//! and becomes an all-UNKNOWN stats block, a poisoned lock is recovered, and
//! the POST happens later on the republisher loop ([`flush`]).
//!
//! ## Honesty rules
//!
//! * A stat that could not be read is `null` with a reason in `box_stats`,
//!   never `0` (served policy `verification-and-evidence`
//!   `unknown-must-not-render-as-a-default`).
//! * The live-claude census is a cached pass: it is nulled when older than
//!   [`LIVE_CLAUDE_MAX_AGE_MS`], and its age is recorded either way, so a stale
//!   census never renders as current.
//! * Loss is visible: a queue overflow, a coord-side persist failure or a
//!   rejected event raises `dropped_before` on the next event coord receives.
//! * No payload identifies work: `what_class` is a fixed token, never a command
//!   line, path, branch or prompt ([`WhatClass`]).
//!
//! The per-machine switch is `session_guard.report_pressure_stops` (default
//! true); off means nothing is queued. An unreadable `settings.json` keeps the
//! last value that was read ([`resolve_reporting`]).

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::resource_guard::{GateObservation, LaneMetric};

/// Events of one `(stop_kind, lane, what_class)` inside this window after the
/// first coalesce into `suppressed_count` on the next posted event. The
/// looping-agent supervisor retries on every tick with no backoff, so without
/// this a refusal storm posts one event per tick.
pub(crate) const COALESCE_WINDOW: Duration = Duration::from_secs(60);

/// Bound on the in-memory queue. Drop-oldest; the drop is counted.
pub(crate) const QUEUE_CAP: usize = 64;

/// coord's per-request event bound.
pub(crate) const BATCH_MAX: usize = 20;

/// A live-claude census older than this is UNKNOWN, not "the count".
pub(crate) const LIVE_CLAUDE_MAX_AGE_MS: i64 = 5 * 60 * 1000;

const POST_TIMEOUT: Duration = Duration::from_secs(8);

/// Base wait after coord answered 404/405/503: the route or its schema is not
/// live on that coord yet.
const ROUTE_ABSENT_BACKOFF: Duration = Duration::from_secs(300);

/// Base wait after 401/403: the credential was refused.
const CREDENTIAL_REFUSED_BACKOFF: Duration = Duration::from_secs(300);

/// Why the work did not start. Wire names are coord's `stop_kind` CHECK set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StopKind {
    /// A spawn was refused outright (terminal, runner instance, looping agent).
    RefusedSpawn,
    /// A coord launch was deferred and will be re-offered.
    DeferredLaunch,
    /// A gate continuation was deferred and stays pending on coord.
    DeferredContinuation,
    /// A CI dispatch was queued instead of started because of headroom.
    DeferredCiJob,
}

impl StopKind {
    pub(crate) fn wire_name(self) -> &'static str {
        match self {
            StopKind::RefusedSpawn => "refused_spawn",
            StopKind::DeferredLaunch => "deferred_launch",
            StopKind::DeferredContinuation => "deferred_continuation",
            StopKind::DeferredCiJob => "deferred_ci_job",
        }
    }
}

/// The metric class of the lane that spoke. Wire names are coord's `lane` CHECK
/// set; the pool that refused (`host` / `wsl` / `threads`) rides separately as
/// `guard_lane`, because WSL and host are different memory pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PressureLane {
    MemoryFreeCommit,
    ThreadPressure,
}

impl PressureLane {
    pub(crate) fn wire_name(self) -> &'static str {
        match self {
            PressureLane::MemoryFreeCommit => "memory_free_commit",
            PressureLane::ThreadPressure => "thread_pressure",
        }
    }

    fn of_metric(metric: LaneMetric) -> Self {
        match metric {
            LaneMetric::FreeCommitBytes => PressureLane::MemoryFreeCommit,
            LaneMetric::ThreadCount => PressureLane::ThreadPressure,
        }
    }
}

/// What kind of work was stopped — a fixed allowlist. Never a free-text label:
/// the caller's `what` can carry anything, and this telemetry leaves the
/// machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum WhatClass {
    TerminalSpawn,
    RunnerInstance,
    CoordLaunch,
    GateContinuation,
    CiJob,
    LoopingAgent,
    Other,
}

impl WhatClass {
    pub(crate) fn wire_name(self) -> &'static str {
        match self {
            WhatClass::TerminalSpawn => "terminal_spawn",
            WhatClass::RunnerInstance => "runner_instance",
            WhatClass::CoordLaunch => "coord_launch",
            WhatClass::GateContinuation => "gate_continuation",
            WhatClass::CiJob => "ci_job",
            WhatClass::LoopingAgent => "looping_agent",
            WhatClass::Other => "other",
        }
    }

    /// The class of an admission seam's `what` noun phrase
    /// (`resource_guard::admit_spawn` / `precheck_spawn`). An unrecognised
    /// phrase is [`WhatClass::Other`] and its text goes nowhere.
    pub(crate) fn for_spawn_what(what: &str) -> Self {
        match what {
            "terminal session" => WhatClass::TerminalSpawn,
            "runner instance" => WhatClass::RunnerInstance,
            "looping-agent session" => WhatClass::LoopingAgent,
            _ => WhatClass::Other,
        }
    }
}

/// The reading a stop was judged on: which lane, which pool, and the two
/// numbers. Built from a [`GateObservation`] for the spawn gate; built by hand
/// for the CI headroom arms, whose swap and saturation readings are not gate
/// observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StopReading {
    pub(crate) lane: PressureLane,
    /// `host`, `wsl` or `threads` — the pool that refused.
    pub(crate) guard_lane: String,
    pub(crate) observed: u64,
    pub(crate) limit: u64,
}

impl StopReading {
    pub(crate) fn from_observation(observation: &GateObservation) -> Self {
        Self {
            lane: PressureLane::of_metric(observation.metric),
            guard_lane: observation.lane.clone(),
            observed: observation.observed,
            limit: observation.limit,
        }
    }
}

/// What stopped, why and on which reading — everything [`record_with`] needs
/// beyond the injected clock, reader and switch.
pub(crate) struct StopRequest<'a> {
    pub(crate) kind: StopKind,
    /// `"warn"` or `"critical"`.
    pub(crate) severity: &'static str,
    pub(crate) what_class: WhatClass,
    pub(crate) reading: &'a StopReading,
    pub(crate) reason: String,
}

/// One event, in coord's column names.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct PressureStopEvent {
    pub(crate) client_event_id: String,
    pub(crate) observed_at: String,
    pub(crate) stop_kind: &'static str,
    pub(crate) lane: &'static str,
    pub(crate) guard_lane: String,
    pub(crate) severity: &'static str,
    pub(crate) what_class: &'static str,
    pub(crate) observed: i64,
    pub(crate) limit_value: i64,
    pub(crate) reason: String,
    pub(crate) session_count_terminal: Option<i32>,
    pub(crate) session_count_live_claude: Option<i32>,
    pub(crate) active_ci_runners: Option<i32>,
    pub(crate) ci_jobs_queued: Option<i32>,
    pub(crate) calibration: Value,
    pub(crate) fleet_calibration: Value,
    pub(crate) suppressed_count: u32,
    pub(crate) dropped_before: u32,
    pub(crate) box_stats: Value,
}

/// What the box was carrying when it said no. `None` / `null` is UNKNOWN.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoxReading {
    pub(crate) session_count_terminal: Option<i32>,
    pub(crate) session_count_live_claude: Option<i32>,
    pub(crate) active_ci_runners: Option<i32>,
    pub(crate) ci_jobs_queued: Option<i32>,
    pub(crate) calibration: Value,
    pub(crate) box_stats: Value,
}

impl BoxReading {
    /// A reading that could not be taken at all: every count UNKNOWN, with the
    /// reason where a reader will look for it.
    fn unreadable(reason: &str) -> Self {
        Self {
            session_count_terminal: None,
            session_count_live_claude: None,
            active_ci_runners: None,
            ci_jobs_queued: None,
            calibration: json!({ "unknown_reason": reason }),
            box_stats: json!({ "unknown_reason": reason }),
        }
    }
}

/// The machine-calibration state of the fleet-calibration plan
/// (`2026-10-05-fleet-calibration-reversible-machine-tuning`). That plan's
/// runner collector does not exist yet, so this is the typed UNKNOWN — never an
/// empty object, which would read as "calibrated, nothing applied". It is the
/// ONE seam that plan fills.
pub(crate) fn fleet_calibration_snapshot() -> Value {
    json!({
        "state": "unknown",
        "reason": "fleet-calibration state source not shipped",
    })
}

// ---------------------------------------------------------------------------
// Emitter: coalescing, bounded queue, back-off. Pure over an injected clock.
// ---------------------------------------------------------------------------

type CoalesceKey = (StopKind, PressureLane, WhatClass);

/// Queue and bookkeeping behind the process-global emitter.
pub(crate) struct Emitter {
    queue: VecDeque<PressureStopEvent>,
    /// First-post time and suppressed count per key.
    coalesce: BTreeMap<CoalesceKey, (Instant, u32)>,
    /// Events lost since the last event that carried the count.
    dropped: u32,
    /// No POST before this instant (coord said "not now").
    not_before: Option<Instant>,
    /// Consecutive 5xx answers, for the back-off ladder.
    server_errors: u32,
}

impl Emitter {
    pub(crate) const fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            coalesce: BTreeMap::new(),
            dropped: 0,
            not_before: None,
            server_errors: 0,
        }
    }

    /// `Some(suppressed)` when this key may post now (carrying the count of
    /// events suppressed since its last post), `None` when it falls inside the
    /// window and was counted instead.
    fn admit(&mut self, key: CoalesceKey, now: Instant) -> Option<u32> {
        match self.coalesce.get_mut(&key) {
            Some((posted_at, suppressed))
                if now.saturating_duration_since(*posted_at) < COALESCE_WINDOW =>
            {
                *suppressed = suppressed.saturating_add(1);
                None
            }
            Some(entry) => {
                let carried = entry.1;
                *entry = (now, 0);
                Some(carried)
            }
            None => {
                self.coalesce.insert(key, (now, 0));
                Some(0)
            }
        }
    }

    /// Queue an event, dropping the oldest when full. The loss count rides on
    /// the event pushed after the loss.
    fn push(&mut self, mut event: PressureStopEvent) {
        if self.queue.len() >= QUEUE_CAP {
            self.queue.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        event.dropped_before = std::mem::take(&mut self.dropped);
        self.queue.push_back(event);
    }

    /// Up to [`BATCH_MAX`] events, oldest first — or none while backing off.
    fn take_batch(&mut self, now: Instant) -> Vec<PressureStopEvent> {
        if self.not_before.is_some_and(|until| now < until) {
            return Vec::new();
        }
        let n = self.queue.len().min(BATCH_MAX);
        self.queue.drain(..n).collect()
    }

    /// Put an undelivered batch back at the front, order preserved. Anything
    /// beyond the cap is the oldest and is counted as lost.
    fn requeue(&mut self, batch: Vec<PressureStopEvent>) {
        for event in batch.into_iter().rev() {
            self.queue.push_front(event);
        }
        while self.queue.len() > QUEUE_CAP {
            self.queue.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    fn lose(&mut self, count: usize) {
        self.dropped = self
            .dropped
            .saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
    }

    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.queue.len()
    }
}

static EMITTER: Mutex<Emitter> = Mutex::new(Emitter::new());

fn lock(emitter: &Mutex<Emitter>) -> std::sync::MutexGuard<'_, Emitter> {
    emitter.lock().unwrap_or_else(|e| e.into_inner())
}

/// Build an event and queue it, unless the switch is off or the key coalesced.
/// Injected clock, reader and switch: the production wrappers pass the live
/// ones.
pub(crate) fn record_with(
    emitter: &Mutex<Emitter>,
    enabled: bool,
    now: Instant,
    read_box: &dyn Fn() -> BoxReading,
    request: StopRequest<'_>,
) {
    if !enabled {
        return;
    }
    let StopRequest {
        kind,
        severity,
        what_class,
        reading,
        reason,
    } = request;
    let key = (kind, reading.lane, what_class);
    let Some(suppressed_count) = lock(emitter).admit(key, now) else {
        return;
    };
    // Outside the emitter lock, and shielded: the reader touches the terminal
    // registry, the CI state and the settings file, and none of that may be
    // able to turn a refusal into a panic.
    let stats = std::panic::catch_unwind(std::panic::AssertUnwindSafe(read_box))
        .unwrap_or_else(|_| BoxReading::unreadable("box stats collection panicked"));
    let event = PressureStopEvent {
        client_event_id: uuid::Uuid::new_v4().to_string(),
        observed_at: chrono::Utc::now().to_rfc3339(),
        stop_kind: kind.wire_name(),
        lane: reading.lane.wire_name(),
        guard_lane: reading.guard_lane.clone(),
        severity,
        what_class: what_class.wire_name(),
        observed: i64::try_from(reading.observed).unwrap_or(i64::MAX),
        limit_value: i64::try_from(reading.limit).unwrap_or(i64::MAX),
        reason,
        session_count_terminal: stats.session_count_terminal,
        session_count_live_claude: stats.session_count_live_claude,
        active_ci_runners: stats.active_ci_runners,
        ci_jobs_queued: stats.ci_jobs_queued,
        calibration: stats.calibration,
        fleet_calibration: fleet_calibration_snapshot(),
        suppressed_count,
        dropped_before: 0,
        box_stats: stats.box_stats,
    };
    lock(emitter).push(event);
}

/// Record a stop judged by the spawn gate's own observation. `severity` is the
/// word [`crate::resource_guard::SpawnGate::tripped`] returned. The `reason`
/// is the same stamp grammar the continuation and launch deferrals already
/// write (`thread_pressure:<sev>:<obs>_over_<lim>` /
/// `commit_pressure:<sev>:<obs>_under_<lim>`).
pub(crate) fn record_gate(
    kind: StopKind,
    severity: &'static str,
    what_class: WhatClass,
    observation: &GateObservation,
) {
    record_reading(
        kind,
        severity,
        what_class,
        &StopReading::from_observation(observation),
        crate::agent_runtime::load_pressure_stamp_reason(severity, observation),
    );
}

/// Record a stop whose reading is not a [`GateObservation`] (the CI headroom
/// arms).
pub(crate) fn record_reading(
    kind: StopKind,
    severity: &'static str,
    what_class: WhatClass,
    reading: &StopReading,
    reason: String,
) {
    record_with(
        &EMITTER,
        reporting_enabled(),
        Instant::now(),
        &read_box_live,
        StopRequest {
            kind,
            severity,
            what_class,
            reading,
            reason,
        },
    );
}

// ---------------------------------------------------------------------------
// The switch
// ---------------------------------------------------------------------------

/// The last `report_pressure_stops` value read from a readable `settings.json`.
static LAST_KNOWN_REPORTING: AtomicBool = AtomicBool::new(true);

/// PURE over the cell: an authoritative read sets and returns the value; an
/// unreadable one keeps the last known value rather than reading the default
/// placeholder as the operator's choice.
pub(crate) fn resolve_reporting(authoritative: bool, value: bool, last_known: &AtomicBool) -> bool {
    if authoritative {
        last_known.store(value, Ordering::Relaxed);
        value
    } else {
        last_known.load(Ordering::Relaxed)
    }
}

fn reporting_enabled() -> bool {
    let loaded = crate::settings::load_settings_full();
    resolve_reporting(
        loaded.is_authoritative(),
        loaded.settings.session_guard.report_pressure_stops,
        &LAST_KNOWN_REPORTING,
    )
}

// ---------------------------------------------------------------------------
// Reading the box
// ---------------------------------------------------------------------------

fn narrow(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// The live-claude count from the cached tracking-health pass, and the pass's
/// age. PURE. `report` is `(live_claude_total, checked_at_ms)`; a pass older
/// than `max_age_ms`, or stamped in the future, is UNKNOWN with its age still
/// recorded — a stale census must never render as current.
pub(crate) fn live_claude_reading(
    report: Option<(usize, i64)>,
    now_ms: i64,
    max_age_ms: i64,
) -> (Option<i32>, Value) {
    let Some((total, checked_at_ms)) = report else {
        return (
            None,
            json!({ "unknown_reason": "no tracking-health pass has completed yet" }),
        );
    };
    let age_ms = now_ms.saturating_sub(checked_at_ms);
    if age_ms < 0 {
        return (
            None,
            json!({
                "age_ms": age_ms,
                "unknown_reason": "census is stamped in the future",
            }),
        );
    }
    if age_ms > max_age_ms {
        return (
            None,
            json!({
                "age_ms": age_ms,
                "unknown_reason": format!("census older than {max_age_ms} ms"),
            }),
        );
    }
    (Some(narrow(total)), json!({ "age_ms": age_ms }))
}

/// Which term decided an effective warn floor. PURE. A fleet floor that is
/// above the local one and equals the effective one is `fleet`; the local one
/// is `local`; anything else is the hardcoded default.
pub(crate) fn floor_source(local_warn: u64, fleet_warn: Option<u64>, effective_warn: u64) -> &'static str {
    match fleet_warn {
        Some(f) if f == effective_warn && f > local_warn => "fleet",
        _ if effective_warn == local_warn => "local",
        _ => "default",
    }
}

fn floors_json(
    local: &crate::settings::SessionGuardSettings,
    lane: &str,
    fleet_warn: Option<u64>,
    fleet_critical: Option<u64>,
) -> Value {
    let effective = crate::resource_guard::effective_session_floors(local, lane);
    json!({
        "local": {
            "warn": local.warn_free_commit_bytes,
            "critical": local.critical_free_commit_bytes,
        },
        "fleet": { "warn": fleet_warn, "critical": fleet_critical },
        "effective": {
            "warn": effective.warn_free_commit_bytes,
            "critical": effective.critical_free_commit_bytes,
        },
        "source": floor_source(
            local.warn_free_commit_bytes,
            fleet_warn,
            effective.warn_free_commit_bytes,
        ),
    })
}

/// What the runner itself can read of its calibration now: the effective thread
/// ceilings with the term behind each, the at-rest baseline and measured
/// per-session cost (`null` = UNKNOWN), the effective memory floors with their
/// source, and the fleet dial. Cheap and bounded: every figure is a cached or
/// memoized reading the guard already holds.
fn calibration_json() -> Value {
    use crate::fleet::resource_sample::Lane;

    let local = crate::settings::get_session_guard_settings();
    let dial = crate::mcp::fleet_policy_poller::dial_snapshot();
    json!({
        "thread_ceilings": crate::resource_guard::thread_ceilings_health_json(),
        "at_rest_thread_baseline": crate::resource_guard::at_rest_thread_baseline(),
        "measured_per_session_threads": crate::resource_guard::measured_per_session_threads(),
        "memory_floors": {
            Lane::Host.as_str(): floors_json(
                &local,
                Lane::Host.as_str(),
                dial.host_warn_free_bytes,
                dial.host_critical_free_bytes,
            ),
            Lane::Wsl.as_str(): floors_json(
                &local,
                Lane::Wsl.as_str(),
                dial.wsl_warn_free_bytes,
                dial.wsl_critical_free_bytes,
            ),
        },
        "fleet_dial": {
            "poll_interval_ms": u64::try_from(dial.poll_interval_ms).unwrap_or(u64::MAX),
            "host_warn_free_bytes": dial.host_warn_free_bytes,
            "host_critical_free_bytes": dial.host_critical_free_bytes,
            "wsl_warn_free_bytes": dial.wsl_warn_free_bytes,
            "wsl_critical_free_bytes": dial.wsl_critical_free_bytes,
            "threads_warn_count": dial.threads_warn_count,
            "threads_critical_count": dial.threads_critical_count,
        },
    })
}

/// The live reader behind [`record_reading`].
fn read_box_live() -> BoxReading {
    let mut unknown = serde_json::Map::new();

    let terminal = crate::fleet::resource_sample::live_terminal_session_count();
    if terminal.is_none() {
        unknown.insert(
            "session_count_terminal".into(),
            json!("terminal manager unavailable (no app handle, no manager, or poisoned registry)"),
        );
    }

    let report = crate::session::tracking_health::latest().map(|r| (r.live_claude_total, r.checked_at_ms));
    let (live_claude, live_claude_meta) = live_claude_reading(
        report,
        chrono::Utc::now().timestamp_millis(),
        LIVE_CLAUDE_MAX_AGE_MS,
    );
    if live_claude.is_none() {
        unknown.insert(
            "session_count_live_claude".into(),
            live_claude_meta
                .get("unknown_reason")
                .cloned()
                .unwrap_or(Value::Null),
        );
    }

    let (running, queued) = crate::ci_node::admission::occupancy();

    // The guard's own memoized readings, never a fresh thread census: a stop is
    // recorded while the box is under pressure, and a census walk is exactly
    // the cost to avoid then.
    let lanes = crate::resource_guard::lane_verdicts();
    let (free_commit_bytes, thread_count) = match &lanes {
        Some(l) => (l.free_commit_bytes, l.threads.graded),
        None => (None, None),
    };
    if free_commit_bytes.is_none() {
        unknown.insert(
            "free_commit_bytes".into(),
            json!("no commit reading (session guard disabled or sensor unreadable)"),
        );
    }
    if thread_count.is_none() {
        unknown.insert(
            "thread_count".into(),
            json!("no thread reading (session guard disabled or thread table unreadable)"),
        );
    }

    BoxReading {
        session_count_terminal: terminal,
        session_count_live_claude: live_claude,
        active_ci_runners: Some(narrow(running)),
        ci_jobs_queued: Some(narrow(queued)),
        calibration: calibration_json(),
        box_stats: json!({
            "thread_count": thread_count,
            "free_commit_bytes": free_commit_bytes,
            "live_claude_census": live_claude_meta,
            "unknown": Value::Object(unknown),
        }),
    }
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ResourceStopReq<'a> {
    device_id: &'a str,
    events: &'a [PressureStopEvent],
}

/// What a response status means for the batch that was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Triage {
    /// 2xx: coord answered; the body says how many were stored.
    Delivered,
    /// Keep the batch queued and wait at least this long (before jitter).
    KeepQueued(Duration),
    /// The batch will never be accepted as sent: drop it.
    Drop,
}

/// PURE: how to treat a response status. `server_errors` is the count of
/// consecutive 5xx answers before this one.
pub(crate) fn triage(status: u16, server_errors: u32) -> Triage {
    match status {
        200..=299 => Triage::Delivered,
        404 | 405 | 503 => Triage::KeepQueued(ROUTE_ABSENT_BACKOFF),
        401 | 403 => Triage::KeepQueued(CREDENTIAL_REFUSED_BACKOFF),
        500..=599 => {
            Triage::KeepQueued(crate::fleet::computer::server_error_backoff(server_errors))
        }
        // 422 and every other 4xx: the request itself is refused.
        _ => Triage::Drop,
    }
}

/// What a 2xx body established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ack {
    /// coord accounted for every event sent (stored or already held).
    Stored,
    /// coord did not store `lost` of the events, with its reason if it gave one.
    Lost { lost: usize, why: String },
    /// The body is not coord's documented outcome: whether anything was stored
    /// is UNKNOWN, and that is not read as success.
    Unknown,
}

/// PURE: classify a 2xx body for `sent` events. coord's reply names `stored`,
/// `duplicate`, `rejected` (a count or a list), an optional `reason` (a
/// persist failure answers 200 with `stored: 0` and a reason) and, when the
/// table is absent, `table_present: false`. A 200 never silently means stored.
pub(crate) fn parse_ack(body: Option<&str>, sent: usize) -> Ack {
    let Some(v) = body.and_then(|b| serde_json::from_str::<Value>(b).ok()) else {
        return Ack::Unknown;
    };
    let Some(stored) = v.get("stored").and_then(Value::as_u64) else {
        return Ack::Unknown;
    };
    let duplicate = v.get("duplicate").and_then(Value::as_u64).unwrap_or(0);
    let accounted = usize::try_from(stored.saturating_add(duplicate)).unwrap_or(usize::MAX);
    let lost = sent.saturating_sub(accounted);
    let table_absent = v.get("table_present").and_then(Value::as_bool) == Some(false);
    if lost == 0 && !table_absent {
        return Ack::Stored;
    }
    let why = match v.get("reason").and_then(Value::as_str) {
        Some(r) => r.to_string(),
        None if table_absent => "coord's resource_pressure_stop_events table is absent".to_string(),
        None => format!("coord stored {stored} and held {duplicate} of {sent} sent"),
    };
    Ack::Lost {
        lost: lost.max(usize::from(table_absent)),
        why,
    }
}

static WARNED_REFUSED_BATCH: AtomicBool = AtomicBool::new(false);
static LOGGED_ROUTE_ABSENT: AtomicBool = AtomicBool::new(false);

/// POST one batch and apply the triage. Never returns an error: a failure keeps
/// or drops the batch and says so.
pub(crate) async fn deliver(
    emitter: &Mutex<Emitter>,
    client: &reqwest::Client,
    url: &str,
    device_id: &str,
    batch: Vec<PressureStopEvent>,
) {
    let body = ResourceStopReq {
        device_id,
        events: &batch,
    };
    // coord-tenant-scope(device): ResourceStopReq{device_id, events}; the route is device-keyed (`/coord/devices/:id/resource-stop`) and coord takes the tenant from the authenticated principal, never from the body.
    let sent = crate::auth::attach_device_auth(client.post(url))
        .timeout(POST_TIMEOUT)
        .json(&body)
        .send()
        .await;
    let now = Instant::now();
    let resp = match sent {
        Ok(r) => r,
        Err(e) => {
            let wait = crate::fleet::resource_sample::jittered_sleep(ROUTE_ABSENT_BACKOFF.as_secs());
            warn!(
                "pressure_stop: POST {url} failed: {e}; keeping {} event(s) queued, retrying in {}s",
                batch.len(),
                wait.as_secs()
            );
            let mut g = lock(emitter);
            g.not_before = Some(now + wait);
            g.requeue(batch);
            return;
        }
    };
    let status = resp.status();
    let text = resp.text().await.ok();
    let n = batch.len();
    let server_errors = lock(emitter).server_errors;
    match triage(status.as_u16(), server_errors) {
        Triage::Delivered => {
            {
                let mut g = lock(emitter);
                g.server_errors = 0;
                g.not_before = None;
            }
            match parse_ack(text.as_deref(), n) {
                Ack::Stored => debug!("pressure_stop: coord accounted for {n} event(s)"),
                Ack::Unknown => info!(
                    "pressure_stop: coord's 2xx body is not the documented outcome; whether \
                     {n} event(s) were stored is UNKNOWN"
                ),
                Ack::Lost { lost, why } => {
                    warn!(
                        "pressure_stop: coord did NOT store {lost} of {n} event(s) ({why}) — \
                         counted in dropped_before on the next event"
                    );
                    lock(emitter).lose(lost);
                }
            }
        }
        Triage::KeepQueued(base) => {
            let wait = crate::fleet::resource_sample::jittered_sleep(base.as_secs());
            let is_5xx = status.is_server_error() && status.as_u16() != 503;
            {
                let mut g = lock(emitter);
                g.not_before = Some(now + wait);
                g.server_errors = if is_5xx { g.server_errors.saturating_add(1) } else { 0 };
                g.requeue(batch);
            }
            if matches!(status.as_u16(), 404 | 405 | 503) {
                if !LOGGED_ROUTE_ABSENT.swap(true, Ordering::Relaxed) {
                    info!(
                        "pressure_stop: coord answered HTTP {status} for {url} — the route or its \
                         schema is not live on this coord yet; events stay queued"
                    );
                }
            } else {
                warn!(
                    "pressure_stop: POST {url} -> HTTP {status}; {n} event(s) stay queued, \
                     retrying in {}s",
                    wait.as_secs()
                );
            }
        }
        Triage::Drop => {
            lock(emitter).lose(n);
            if !WARNED_REFUSED_BATCH.swap(true, Ordering::Relaxed) {
                warn!(
                    "pressure_stop: coord refused {n} event(s) with HTTP {status} ({}); dropped, \
                     counted in dropped_before",
                    text.as_deref().unwrap_or("no body").chars().take(200).collect::<String>()
                );
            }
        }
    }
}

/// Post what is queued. Called from the republisher loop (`fleet.rs`), which is
/// already gated on owning shared machine state. Best-effort and quiet when
/// there is nothing to send, no paired device or no coord base.
pub(crate) async fn flush() {
    {
        let g = lock(&EMITTER);
        if g.queue.is_empty() {
            return;
        }
    }
    let Some(device) = crate::fleet::load_device_file() else {
        debug!("pressure_stop: no ~/.qontinui/machine.json — events stay queued");
        return;
    };
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        debug!("pressure_stop: no coord base configured — events stay queued");
        return;
    };
    let Some(client) = crate::coord_http::coord_client() else {
        debug!("pressure_stop: no shared coord client — events stay queued");
        return;
    };
    let batch = lock(&EMITTER).take_batch(Instant::now());
    if batch.is_empty() {
        return;
    }
    let url = format!(
        "{}/coord/devices/{}/resource-stop",
        base.trim_end_matches('/'),
        device.device_id
    );
    deliver(&EMITTER, client, &url, &device.device_id, batch).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU16;
    use std::sync::Arc;

    fn memory_reading(observed: u64, limit: u64) -> StopReading {
        StopReading {
            lane: PressureLane::MemoryFreeCommit,
            guard_lane: "host".to_string(),
            observed,
            limit,
        }
    }

    fn known_box() -> BoxReading {
        BoxReading {
            session_count_terminal: Some(7),
            session_count_live_claude: Some(5),
            active_ci_runners: Some(2),
            ci_jobs_queued: Some(1),
            calibration: json!({"thread_ceilings": {"warn": 300}}),
            box_stats: json!({"thread_count": 411}),
        }
    }

    fn record(
        emitter: &Mutex<Emitter>,
        now: Instant,
        what: WhatClass,
        reading: &StopReading,
    ) {
        record_with(
            emitter,
            true,
            now,
            &known_box,
            StopRequest {
                kind: StopKind::RefusedSpawn,
                severity: "critical",
                what_class: what,
                reading,
                reason: "commit_pressure:critical:1000_under_2000".to_string(),
            },
        );
    }

    #[test]
    fn an_event_is_built_from_the_observation_with_literal_values() {
        let emitter = Mutex::new(Emitter::new());
        let obs = GateObservation {
            lane: "host".to_string(),
            metric: LaneMetric::FreeCommitBytes,
            observed: 1_073_741_824,
            limit: 1_610_612_736,
        };
        record_with(
            &emitter,
            true,
            Instant::now(),
            &known_box,
            StopRequest {
                kind: StopKind::RefusedSpawn,
                severity: "critical",
                what_class: WhatClass::for_spawn_what("terminal session"),
                reading: &StopReading::from_observation(&obs),
                reason: crate::agent_runtime::load_pressure_stamp_reason("critical", &obs),
            },
        );
        let ev = lock(&emitter).queue.pop_front().expect("one event queued");
        assert_eq!(ev.stop_kind, "refused_spawn");
        assert_eq!(ev.lane, "memory_free_commit");
        assert_eq!(ev.guard_lane, "host");
        assert_eq!(ev.severity, "critical");
        assert_eq!(ev.what_class, "terminal_spawn");
        assert_eq!(ev.observed, 1_073_741_824);
        assert_eq!(ev.limit_value, 1_610_612_736);
        assert_eq!(ev.reason, "commit_pressure:critical:1073741824_under_1610612736");
        assert_eq!(ev.session_count_terminal, Some(7));
        assert_eq!(ev.session_count_live_claude, Some(5));
        assert_eq!(ev.active_ci_runners, Some(2));
        assert_eq!(ev.ci_jobs_queued, Some(1));
        assert_eq!(ev.suppressed_count, 0);
        assert_eq!(ev.dropped_before, 0);
        assert_eq!(
            ev.fleet_calibration,
            json!({"state":"unknown","reason":"fleet-calibration state source not shipped"})
        );
        assert!(uuid::Uuid::parse_str(&ev.client_event_id).is_ok(), "{}", ev.client_event_id);
    }

    #[test]
    fn a_thread_observation_maps_to_the_thread_lane_and_pool() {
        let obs = GateObservation {
            lane: "threads".to_string(),
            metric: LaneMetric::ThreadCount,
            observed: 412,
            limit: 400,
        };
        let r = StopReading::from_observation(&obs);
        assert_eq!(r.lane.wire_name(), "thread_pressure");
        assert_eq!(r.guard_lane, "threads");
        assert_eq!((r.observed, r.limit), (412, 400));
    }

    #[test]
    fn what_class_is_an_allowlist_and_free_text_never_survives() {
        assert_eq!(WhatClass::for_spawn_what("terminal session").wire_name(), "terminal_spawn");
        assert_eq!(WhatClass::for_spawn_what("runner instance").wire_name(), "runner_instance");
        assert_eq!(WhatClass::for_spawn_what("looping-agent session").wire_name(), "looping_agent");
        assert_eq!(
            WhatClass::for_spawn_what("scheduled remote agent for D:/secret/branch").wire_name(),
            "other"
        );
    }

    #[test]
    fn two_refusals_inside_the_window_post_one_event_then_carry_the_count() {
        let emitter = Mutex::new(Emitter::new());
        let t0 = Instant::now();
        let r = memory_reading(1000, 2000);
        record(&emitter, t0, WhatClass::TerminalSpawn, &r);
        record(&emitter, t0 + Duration::from_secs(10), WhatClass::TerminalSpawn, &r);
        record(&emitter, t0 + Duration::from_secs(59), WhatClass::TerminalSpawn, &r);
        assert_eq!(lock(&emitter).queued(), 1, "only the first posts inside 60 s");
        record(&emitter, t0 + Duration::from_secs(61), WhatClass::TerminalSpawn, &r);
        let g = lock(&emitter);
        assert_eq!(g.queued(), 2);
        assert_eq!(g.queue[0].suppressed_count, 0);
        assert_eq!(g.queue[1].suppressed_count, 2, "the two suppressed ones ride the next event");
    }

    #[test]
    fn a_different_what_class_or_lane_does_not_coalesce() {
        let emitter = Mutex::new(Emitter::new());
        let t0 = Instant::now();
        record(&emitter, t0, WhatClass::TerminalSpawn, &memory_reading(1, 2));
        record(&emitter, t0, WhatClass::RunnerInstance, &memory_reading(1, 2));
        let threads = StopReading {
            lane: PressureLane::ThreadPressure,
            guard_lane: "threads".to_string(),
            observed: 500,
            limit: 400,
        };
        record(&emitter, t0, WhatClass::TerminalSpawn, &threads);
        assert_eq!(lock(&emitter).queued(), 3);
    }

    #[test]
    fn overflow_drops_the_oldest_and_the_next_event_reports_the_loss() {
        let emitter = Mutex::new(Emitter::new());
        let t0 = Instant::now();
        // A distinct key per event keeps coalescing out of the way: vary the
        // time past the window for the single key instead.
        for i in 0..(QUEUE_CAP + 3) {
            record(
                &emitter,
                t0 + COALESCE_WINDOW * (i as u32 + 1),
                WhatClass::TerminalSpawn,
                &memory_reading(i as u64, 9999),
            );
        }
        let g = lock(&emitter);
        assert_eq!(g.queued(), QUEUE_CAP);
        assert_eq!(g.queue.front().expect("oldest kept").observed, 3, "events 0..3 were dropped");
        let reported: u32 = g.queue.iter().map(|e| e.dropped_before).sum();
        assert_eq!(reported, 3, "each drop is reported exactly once");
    }

    #[test]
    fn unknown_counts_stay_null_and_never_become_zero() {
        let emitter = Mutex::new(Emitter::new());
        record_with(
            &emitter,
            true,
            Instant::now(),
            &|| BoxReading {
                session_count_terminal: None,
                session_count_live_claude: None,
                active_ci_runners: None,
                ci_jobs_queued: None,
                calibration: json!({}),
                box_stats: json!({"unknown": {"session_count_terminal": "no manager"}}),
            },
            StopRequest {
                kind: StopKind::DeferredLaunch,
                severity: "warn",
                what_class: WhatClass::CoordLaunch,
                reading: &memory_reading(5, 6),
                reason: "r".to_string(),
            },
        );
        let ev = lock(&emitter).queue.pop_front().expect("queued");
        let wire = serde_json::to_value(&ev).expect("serializes");
        for field in [
            "session_count_terminal",
            "session_count_live_claude",
            "active_ci_runners",
            "ci_jobs_queued",
        ] {
            assert_eq!(wire[field], Value::Null, "{field} must be null, not 0");
        }
    }

    #[test]
    fn a_panicking_reader_neither_unwinds_nor_loses_the_event() {
        let emitter = Mutex::new(Emitter::new());
        record_with(
            &emitter,
            true,
            Instant::now(),
            &|| panic!("registry exploded"),
            StopRequest {
                kind: StopKind::RefusedSpawn,
                severity: "critical",
                what_class: WhatClass::TerminalSpawn,
                reading: &memory_reading(1, 2),
                reason: "r".to_string(),
            },
        );
        let ev = lock(&emitter).queue.pop_front().expect("event still queued");
        assert_eq!(ev.session_count_terminal, None);
        assert_eq!(ev.box_stats["unknown_reason"], "box stats collection panicked");
    }

    #[test]
    fn the_switch_off_queues_nothing() {
        let emitter = Mutex::new(Emitter::new());
        record_with(
            &emitter,
            false,
            Instant::now(),
            &known_box,
            StopRequest {
                kind: StopKind::RefusedSpawn,
                severity: "critical",
                what_class: WhatClass::TerminalSpawn,
                reading: &memory_reading(1, 2),
                reason: "r".to_string(),
            },
        );
        assert_eq!(lock(&emitter).queued(), 0);
    }

    #[test]
    fn an_unreadable_settings_file_keeps_the_last_known_switch() {
        let cell = AtomicBool::new(true);
        assert!(!resolve_reporting(true, false, &cell), "authoritative off is honoured");
        assert!(
            !resolve_reporting(false, true, &cell),
            "an unreadable file's default placeholder must not turn reporting back on"
        );
        assert!(resolve_reporting(true, true, &cell));
        assert!(resolve_reporting(false, false, &cell));
    }

    #[test]
    fn the_live_claude_census_is_unknown_when_stale_and_age_is_always_recorded() {
        let (n, meta) = live_claude_reading(Some((9, 1_000_000)), 1_060_000, LIVE_CLAUDE_MAX_AGE_MS);
        assert_eq!(n, Some(9));
        assert_eq!(meta, json!({"age_ms": 60_000}));

        let (n, meta) = live_claude_reading(Some((9, 1_000_000)), 1_000_000 + 300_001, LIVE_CLAUDE_MAX_AGE_MS);
        assert_eq!(n, None, "older than 5 minutes is UNKNOWN, not nine");
        assert_eq!(meta["age_ms"], 300_001);
        assert_eq!(meta["unknown_reason"], "census older than 300000 ms");

        let (n, _) = live_claude_reading(Some((0, 1_000_000)), 1_000_000, LIVE_CLAUDE_MAX_AGE_MS);
        assert_eq!(n, Some(0), "a fresh zero is a measurement");

        let (n, meta) = live_claude_reading(None, 5, LIVE_CLAUDE_MAX_AGE_MS);
        assert_eq!(n, None);
        assert_eq!(meta["unknown_reason"], "no tracking-health pass has completed yet");

        let (n, meta) = live_claude_reading(Some((3, 2_000)), 1_000, LIVE_CLAUDE_MAX_AGE_MS);
        assert_eq!(n, None);
        assert_eq!(meta["unknown_reason"], "census is stamped in the future");
    }

    #[test]
    fn the_floor_source_names_the_deciding_term() {
        assert_eq!(floor_source(3, Some(5), 5), "fleet");
        assert_eq!(floor_source(3, Some(2), 3), "local");
        assert_eq!(floor_source(3, None, 3), "local");
        assert_eq!(floor_source(3, None, 4), "default");
    }

    #[test]
    fn fleet_calibration_is_the_typed_unknown_object() {
        let v = fleet_calibration_snapshot();
        assert_eq!(v["state"], "unknown");
        assert_eq!(v["reason"], "fleet-calibration state source not shipped");
        assert_ne!(v, json!({}), "UNKNOWN must never render as an empty object");
    }

    #[test]
    fn triage_maps_every_status_class() {
        assert_eq!(triage(200, 0), Triage::Delivered);
        assert_eq!(triage(204, 0), Triage::Delivered);
        for s in [404, 405, 503] {
            assert_eq!(triage(s, 0), Triage::KeepQueued(ROUTE_ABSENT_BACKOFF), "{s}");
        }
        assert_eq!(triage(401, 0), Triage::KeepQueued(CREDENTIAL_REFUSED_BACKOFF));
        assert_eq!(triage(403, 0), Triage::KeepQueued(CREDENTIAL_REFUSED_BACKOFF));
        assert_eq!(triage(500, 0), Triage::KeepQueued(Duration::from_secs(30)));
        assert_eq!(triage(502, 2), Triage::KeepQueued(Duration::from_secs(120)));
        assert_eq!(triage(422, 0), Triage::Drop);
        assert_eq!(triage(400, 0), Triage::Drop);
    }

    #[test]
    fn a_2xx_is_not_stored_until_the_body_says_so() {
        assert_eq!(parse_ack(Some(r#"{"stored":2,"duplicate":0}"#), 2), Ack::Stored);
        assert_eq!(parse_ack(Some(r#"{"stored":1,"duplicate":1}"#), 2), Ack::Stored);
        assert_eq!(
            parse_ack(Some(r#"{"stored":0,"reason":"persist failed: db down"}"#), 3),
            Ack::Lost { lost: 3, why: "persist failed: db down".to_string() }
        );
        assert_eq!(
            parse_ack(Some(r#"{"stored":1,"duplicate":0,"rejected":[{"i":1}]}"#), 2),
            Ack::Lost { lost: 1, why: "coord stored 1 and held 0 of 2 sent".to_string() }
        );
        assert_eq!(
            parse_ack(Some(r#"{"stored":0,"table_present":false}"#), 1),
            Ack::Lost {
                lost: 1,
                why: "coord's resource_pressure_stop_events table is absent".to_string()
            }
        );
        for unknown in [None, Some(""), Some("not json"), Some("{}"), Some(r#"{"ok":true}"#)] {
            assert_eq!(parse_ack(unknown, 1), Ack::Unknown, "{unknown:?}");
        }
    }

    // --- delivery against a fake coord -------------------------------------

    struct FakeCoord {
        url: String,
        posts: Arc<Mutex<Vec<Value>>>,
        status: Arc<AtomicU16>,
        body: Arc<Mutex<String>>,
    }

    async fn fake_coord() -> FakeCoord {
        use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
        #[derive(Clone)]
        struct S {
            posts: Arc<Mutex<Vec<Value>>>,
            status: Arc<AtomicU16>,
            body: Arc<Mutex<String>>,
        }
        let s = S {
            posts: Arc::default(),
            status: Arc::new(AtomicU16::new(200)),
            body: Arc::new(Mutex::new(String::new())),
        };
        let app = Router::new()
            .route(
                "/coord/devices/{id}/resource-stop",
                post(|State(s): State<S>, Json(b): Json<Value>| async move {
                    s.posts.lock().unwrap().push(b);
                    (
                        StatusCode::from_u16(s.status.load(Ordering::Relaxed)).unwrap(),
                        s.body.lock().unwrap().clone(),
                    )
                }),
            )
            .with_state(s.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        FakeCoord {
            url: format!("http://{addr}/coord/devices/dev-1/resource-stop"),
            posts: s.posts,
            status: s.status,
            body: s.body,
        }
    }

    fn seeded(n: usize) -> Mutex<Emitter> {
        let emitter = Mutex::new(Emitter::new());
        let t0 = Instant::now();
        for i in 0..n {
            record(
                &emitter,
                t0 + COALESCE_WINDOW * (i as u32 + 1),
                WhatClass::TerminalSpawn,
                &memory_reading(100 + i as u64, 2000),
            );
        }
        emitter
    }

    async fn deliver_all(emitter: &Mutex<Emitter>, fake: &FakeCoord) {
        let batch = lock(emitter).take_batch(Instant::now());
        deliver(emitter, &reqwest::Client::new(), &fake.url, "dev-1", batch).await;
    }

    #[tokio::test]
    async fn a_delivered_batch_posts_the_documented_body_and_leaves_the_queue_empty() {
        let fake = fake_coord().await;
        *fake.body.lock().unwrap() = r#"{"stored":2,"duplicate":0}"#.to_string();
        let emitter = seeded(2);
        deliver_all(&emitter, &fake).await;
        let posts = fake.posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0]["device_id"], "dev-1");
        let events = posts[0]["events"].as_array().expect("events array");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["stop_kind"], "refused_spawn");
        assert_eq!(events[0]["lane"], "memory_free_commit");
        assert_eq!(events[0]["observed"], 100);
        assert_eq!(events[0]["session_count_live_claude"], 5);
        assert_eq!(events[0]["fleet_calibration"]["state"], "unknown");
        assert!(posts[0].get("tenant_id").is_none(), "tenant comes from the principal");
        assert_eq!(lock(&emitter).queued(), 0);
    }

    #[tokio::test]
    async fn a_batch_is_bounded_to_what_coord_accepts() {
        let fake = fake_coord().await;
        *fake.body.lock().unwrap() = format!(r#"{{"stored":{BATCH_MAX},"duplicate":0}}"#);
        let emitter = seeded(BATCH_MAX + 5);
        deliver_all(&emitter, &fake).await;
        assert_eq!(fake.posts.lock().unwrap()[0]["events"].as_array().unwrap().len(), BATCH_MAX);
        assert_eq!(lock(&emitter).queued(), 5);
    }

    #[tokio::test]
    async fn a_route_that_is_not_live_keeps_events_queued_and_backs_off() {
        for status in [404u16, 405, 503, 401, 403, 500] {
            let fake = fake_coord().await;
            fake.status.store(status, Ordering::Relaxed);
            let emitter = seeded(3);
            deliver_all(&emitter, &fake).await;
            let mut g = lock(&emitter);
            assert_eq!(g.queued(), 3, "{status}: events must stay queued");
            assert!(g.not_before.is_some(), "{status}: must back off");
            assert!(g.take_batch(Instant::now()).is_empty(), "{status}: no POST inside the back-off");
            assert_eq!(g.dropped, 0, "{status}: keeping is not losing");
        }
    }

    #[tokio::test]
    async fn requeue_preserves_order() {
        let fake = fake_coord().await;
        fake.status.store(503, Ordering::Relaxed);
        let emitter = seeded(3);
        deliver_all(&emitter, &fake).await;
        let g = lock(&emitter);
        let observed: Vec<i64> = g.queue.iter().map(|e| e.observed).collect();
        assert_eq!(observed, vec![100, 101, 102]);
    }

    #[tokio::test]
    async fn a_422_drops_the_batch_and_counts_the_loss() {
        let fake = fake_coord().await;
        fake.status.store(422, Ordering::Relaxed);
        let emitter = seeded(2);
        deliver_all(&emitter, &fake).await;
        let g = lock(&emitter);
        assert_eq!(g.queued(), 0);
        assert_eq!(g.dropped, 2);
        drop(g);
        record(&emitter, Instant::now() + Duration::from_secs(10_000), WhatClass::TerminalSpawn, &memory_reading(1, 2));
        assert_eq!(lock(&emitter).queue[0].dropped_before, 2, "the loss is visible on the next event");
    }

    #[tokio::test]
    async fn a_200_that_stored_nothing_is_counted_as_lost() {
        let fake = fake_coord().await;
        *fake.body.lock().unwrap() = r#"{"stored":0,"reason":"persist failed"}"#.to_string();
        let emitter = seeded(2);
        deliver_all(&emitter, &fake).await;
        let g = lock(&emitter);
        assert_eq!(g.queued(), 0);
        assert_eq!(g.dropped, 2);
    }

    #[tokio::test]
    async fn an_unreachable_coord_keeps_the_batch() {
        let emitter = seeded(2);
        let batch = lock(&emitter).take_batch(Instant::now());
        // Port 1 is never listening.
        deliver(&emitter, &reqwest::Client::new(), "http://127.0.0.1:1/x", "dev-1", batch).await;
        let g = lock(&emitter);
        assert_eq!(g.queued(), 2);
        assert!(g.not_before.is_some());
    }

    // --- wiring: each hook site records, and the refusal is unchanged -------

    fn src(rel: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(rel);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// The text of `rel` before its first `#[cfg(test)]`, so a test that quotes
    /// a call cannot satisfy the scan.
    fn prod_src(rel: &str) -> String {
        let s = src(rel);
        match s.find("#[cfg(test)]") {
            Some(i) => s[..i].to_string(),
            None => s,
        }
    }

    #[test]
    fn every_hook_site_records_a_pressure_stop() {
        let guard = prod_src("resource_guard.rs");
        assert_eq!(
            guard.matches("pressure_stop::record_gate(").count(),
            2,
            "admit_spawn and precheck_spawn each record their critical refusal"
        );
        assert_eq!(guard.matches("pressure_stop::StopKind::RefusedSpawn").count(), 2);

        let rt = src("agent_runtime.rs");
        assert!(rt.contains("pressure_stop::StopKind::DeferredLaunch"), "admit_launch");
        assert!(rt.contains("pressure_stop::StopKind::DeferredContinuation"), "continuation guard");

        let ci = src("ci_node/admission.rs");
        assert!(ci.contains("pressure_stop::StopKind::DeferredCiJob"), "CI headroom defer");

        let fleet = src("fleet.rs");
        assert!(
            fleet.contains("crate::pressure_stop::flush().await"),
            "the republisher loop must drive the flush"
        );
    }

    #[test]
    fn the_refusal_text_and_wire_prefix_are_unchanged_by_the_hook() {
        let refusal = crate::resource_guard::critical_refusal_fixture("terminal session");
        assert!(
            refusal.starts_with(crate::resource_guard::CRITICAL_REFUSAL_PREFIX),
            "{refusal}"
        );
        assert!(refusal.contains("Not starting a new terminal session"), "{refusal}");
        let guard = prod_src("resource_guard.rs");
        let refusing = guard
            .find("refusing to spawn past the session critical limit")
            .expect("the refusal log line");
        let tail = &guard[refusing..];
        let hook = tail.find("pressure_stop::record_gate(").expect("hook after the log");
        let ret = tail.find("Err(critical_refusal(").expect("the refusal return");
        assert!(hook < ret, "the hook runs after the verdict and before the refusal returns");
    }
}
