//! Pressure STOP events — every time the resource guard refuses or defers work
//! because of the low-memory (free-commit) lane or the thread-pressure lane,
//! one durable event goes to coord carrying what the box was carrying at that
//! instant (plan `2026-11-24-pressure-stops-record-box-stats-in-coord`,
//! Phase 3).
//!
//! ## What "stop" means here
//!
//! An admission REFUSAL or DEFERRAL before a child exists. No site in this
//! runner kills an already-running process for a memory or thread verdict, so
//! there is nothing else to record; a future one calls [`record_observation`]
//! like every other site. Kernel/cgroup OOM kills are coord's `oom_kill`
//! computer events, not this stream. A WARN-level spawn that PROCEEDS is not an
//! event (nothing was stopped), and background-tick shedding is a throttle,
//! not a stop.
//!
//! The hook sites, one call each, after the verdict and before the refusal
//! returns:
//!
//! | site | `stop_kind` | `what_class` |
//! |---|---|---|
//! | `resource_guard::admit_spawn` (critical, not overridden) | `refused_spawn` | from the caller's noun ([`WhatClass::from_spawn_noun`]) |
//! | `resource_guard::precheck_spawn` (critical) | `refused_spawn` | from the caller's noun |
//! | `agent_runtime::admit_launch` (any tripped lane) | `deferred_launch` | `coord_launch` |
//! | `agent_runtime::run_gate_continuation_inner` `LoadPressure` arm | `deferred_continuation` | `gate_continuation` |
//! | `ci_node::admission::admit` headroom `Defer` (not the at-cap one) | `deferred_ci_job` | `ci_job` |
//!
//! ## The three properties the admission path relies on
//!
//! - **It never blocks or panics the guard.** [`record_observation`] takes one
//!   short in-process lock to decide coalescing, reads cached or cheap gauges
//!   (each behind `catch_unwind`), and pushes onto an in-memory queue. Delivery
//!   happens elsewhere ([`flush`], on the fleet republisher tick).
//! - **A storm is one event per minute per kind.** The looping-agent supervisor
//!   retries every tick with no backoff, so the emitter coalesces per
//!   `(stop_kind, lane, what_class)`: the first stop posts, further ones inside
//!   [`COALESCE_WINDOW`] only count, and that count rides the NEXT posted event
//!   of the same key as `suppressed_count` — or, when the window expires with
//!   no later stop, a TRAILING event the next flush queues for it (stats
//!   UNKNOWN: a coalesced stop is counted, never measured).
//! - **Loss is counted, never silent.** The queue is bounded at [`QUEUE_CAP`]
//!   and drops the OLDEST; the drop count (plus anything coord said it did not
//!   store) rides the next pushed event as `dropped_before`.
//!
//! ## UNKNOWN is null, never 0
//!
//! Every box-stat count is `Option` on the wire and serializes as `null` when
//! the runner could not measure it, with the reason in `box_stats.unknown`. The
//! live-claude census is a background reading taken every
//! `tracking_health::CHECK_INTERVAL`; one older than [`CENSUS_MAX_AGE`] is
//! nulled (its age is still recorded) so a stale census never reads as current.
//!
//! ## What leaves the machine
//!
//! Fixed tokens only: `what_class` is from a closed allowlist, `reason` is the
//! numeric stamp grammar the continuation stamps already use. No command
//! lines, paths, branch names or prompts. The per-machine switch
//! `Settings.report_pressure_stops` (default on) turns filling the queue off.

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::resource_guard::{GateObservation, LaneMetric};

// ===========================================================================
// Constants — each one has a literal test
// ===========================================================================

/// Inside this window a repeat stop of the same `(stop_kind, lane,
/// what_class)` only increments the next event's `suppressed_count`.
pub(crate) const COALESCE_WINDOW: Duration = Duration::from_secs(60);

/// The in-memory queue bound. Drop-oldest past it.
pub(crate) const QUEUE_CAP: usize = 64;

/// Events per POST — coord's `MAX_BATCH`.
pub(crate) const MAX_BATCH: usize = 20;

/// Per-POST timeout.
pub(crate) const POST_TIMEOUT: Duration = Duration::from_secs(8);

/// The oldest live-claude census reading still reported as a count.
///
/// The census runs every `tracking_health::CHECK_INTERVAL` (600 s), so a bound
/// BELOW that interval (the plan's first sketch said 5 minutes) would null the
/// count on roughly half of all stops on a perfectly healthy runner. One
/// interval plus two minutes of slack for a slow pass: a reading older than
/// that means the census loop missed a beat, and the count is then UNKNOWN.
pub(crate) const CENSUS_MAX_AGE: Duration =
    Duration::from_secs(crate::session::tracking_health::CHECK_INTERVAL.as_secs() + 120);

/// Backoff after coord says the route or the table is not there yet (a coord
/// or web deploy behind this runner): quiet, and long enough not to hammer.
const ABSENT_BACKOFF: Duration = Duration::from_secs(300);

/// Backoff after a 401/403: the device credential is not usable right now.
const AUTH_BACKOFF: Duration = Duration::from_secs(300);

/// Base backoff after a 5xx or a transport failure; jittered up to double.
const SERVER_BACKOFF: Duration = Duration::from_secs(30);

/// Coord's per-object JSON cap (`MAX_JSON_BYTES`). An object over it would be
/// rejected whole, so the runner replaces it with a typed UNKNOWN first.
pub(crate) const MAX_JSON_BYTES: usize = 8 * 1024;

// ===========================================================================
// Vocabulary — the same closed sets as coord's CHECK constraints
// ===========================================================================

/// What the guard did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StopKind {
    RefusedSpawn,
    DeferredLaunch,
    DeferredContinuation,
    DeferredCiJob,
}

impl StopKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            StopKind::RefusedSpawn => "refused_spawn",
            StopKind::DeferredLaunch => "deferred_launch",
            StopKind::DeferredContinuation => "deferred_continuation",
            StopKind::DeferredCiJob => "deferred_ci_job",
        }
    }
}

/// The metric class that tripped (coord's `lane` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StopLane {
    MemoryFreeCommit,
    ThreadPressure,
}

impl StopLane {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            StopLane::MemoryFreeCommit => "memory_free_commit",
            StopLane::ThreadPressure => "thread_pressure",
        }
    }

    pub(crate) fn from_metric(metric: LaneMetric) -> Self {
        match metric {
            LaneMetric::FreeCommitBytes => StopLane::MemoryFreeCommit,
            LaneMetric::ThreadCount => StopLane::ThreadPressure,
        }
    }
}

/// The class of work that was stopped. A fixed token, never a description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    pub(crate) fn as_str(self) -> &'static str {
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

    /// The class for the noun phrase every `admit_spawn` / `precheck_spawn`
    /// caller already passes (it lands verbatim in the refusal text, so it is
    /// the caller's own statement of what it is starting). An unmapped noun —
    /// `"scheduled remote agent"` today — is `other`, never guessed.
    pub(crate) fn from_spawn_noun(what: &str) -> Self {
        match what {
            "terminal session" => WhatClass::TerminalSpawn,
            "runner instance" => WhatClass::RunnerInstance,
            "looping-agent session" => WhatClass::LoopingAgent,
            _ => WhatClass::Other,
        }
    }
}

/// `warn` or `critical` — coord's `severity` CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    Warn,
    Critical,
}

impl Severity {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Severity::Warn => "warn",
            Severity::Critical => "critical",
        }
    }

    /// The word `SpawnGate::tripped` hands out. Anything else is `None`.
    pub(crate) fn from_word(word: &str) -> Option<Self> {
        match word {
            "warn" => Some(Severity::Warn),
            "critical" => Some(Severity::Critical),
            _ => None,
        }
    }
}

/// The reading behind one stop: which metric, which pool, the number and the
/// limit it crossed.
///
/// `observed` / `limit` are `None` when the tripping quantity is not the
/// lane's own metric (the CI swap term: swap USED over a ceiling share is not
/// "free commit under a floor", so it rides `box_stats` instead of reusing a
/// column whose direction it would invert).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StopReading {
    pub(crate) lane: StopLane,
    /// `host`, `wsl` or `threads` — coord's `guard_lane` (NOT NULL there).
    pub(crate) guard_lane: &'static str,
    pub(crate) observed: Option<u64>,
    pub(crate) limit: Option<u64>,
}

/// `GateObservation.lane` as one of coord's three `guard_lane` tokens. The
/// guard only ever writes these three; anything else is refused rather than
/// sent as a guess (coord would 422 it anyway).
fn guard_lane_token(lane: &str) -> Option<&'static str> {
    match lane {
        "host" => Some("host"),
        "wsl" => Some("wsl"),
        "threads" => Some("threads"),
        _ => None,
    }
}

impl StopReading {
    pub(crate) fn from_observation(observation: &GateObservation) -> Option<Self> {
        Some(StopReading {
            lane: StopLane::from_metric(observation.metric),
            guard_lane: guard_lane_token(&observation.lane)?,
            observed: Some(observation.observed),
            limit: Some(observation.limit),
        })
    }
}

// ===========================================================================
// The wire event — coord's `StopEventIn`
// ===========================================================================

/// One event, exactly as coord's `StopEventIn` reads it. Every count is
/// `Option` and serializes as `null` when unmeasured — never `0`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct PressureStopEvent {
    pub(crate) client_event_id: String,
    pub(crate) observed_at: String,
    pub(crate) stop_kind: &'static str,
    pub(crate) lane: &'static str,
    pub(crate) guard_lane: &'static str,
    pub(crate) severity: &'static str,
    pub(crate) what_class: &'static str,
    pub(crate) observed: Option<i64>,
    pub(crate) limit_value: Option<i64>,
    pub(crate) reason: Option<String>,
    pub(crate) session_count_terminal: Option<i64>,
    pub(crate) session_count_live_claude: Option<i64>,
    pub(crate) active_ci_runners: Option<i64>,
    pub(crate) ci_jobs_queued: Option<i64>,
    pub(crate) calibration: Value,
    pub(crate) fleet_calibration: Value,
    pub(crate) suppressed_count: u32,
    pub(crate) dropped_before: u32,
    pub(crate) box_stats: Value,
}

/// What the box was carrying at the stop. Built by [`collect_box_stats`] in
/// production and by hand in tests.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoxStats {
    pub(crate) session_count_terminal: Option<i64>,
    pub(crate) session_count_live_claude: Option<i64>,
    pub(crate) active_ci_runners: Option<i64>,
    pub(crate) ci_jobs_queued: Option<i64>,
    /// A JSON object.
    pub(crate) calibration: Value,
    /// A JSON object — the typed UNKNOWN until the fleet-calibration seam is
    /// filled ([`fleet_calibration_snapshot`]).
    pub(crate) fleet_calibration: Value,
    /// A JSON object: the remaining readings plus `unknown: {field: reason}`.
    pub(crate) box_stats: Value,
}

/// u64 → i64 for the wire; a reading past `i64::MAX` is not a real reading and
/// goes as UNKNOWN rather than wrapping negative.
fn wire_i64(v: u64) -> Option<i64> {
    i64::try_from(v).ok()
}

/// An object over [`MAX_JSON_BYTES`] replaced by a typed UNKNOWN that says so,
/// so one fat snapshot cannot make coord reject the whole event. PURE.
pub(crate) fn cap_json(field: &str, v: Value) -> Value {
    let len = v.to_string().len();
    if len > MAX_JSON_BYTES {
        json!({
            "state": "unknown",
            "reason": format!("{field} snapshot was {len} bytes, over the {MAX_JSON_BYTES}-byte wire cap"),
        })
    } else {
        v
    }
}

/// Build one event. PURE over its inputs (`observed_at`, `client_event_id` and
/// the stats are parameters).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_event(
    client_event_id: String,
    observed_at: chrono::DateTime<chrono::Utc>,
    kind: StopKind,
    severity: Severity,
    what: WhatClass,
    reading: &StopReading,
    reason: Option<String>,
    stats: BoxStats,
    suppressed_count: u32,
) -> PressureStopEvent {
    PressureStopEvent {
        client_event_id,
        observed_at: observed_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        stop_kind: kind.as_str(),
        lane: reading.lane.as_str(),
        guard_lane: reading.guard_lane,
        severity: severity.as_str(),
        what_class: what.as_str(),
        observed: reading.observed.and_then(wire_i64),
        limit_value: reading.limit.and_then(wire_i64),
        reason,
        session_count_terminal: stats.session_count_terminal,
        session_count_live_claude: stats.session_count_live_claude,
        active_ci_runners: stats.active_ci_runners,
        ci_jobs_queued: stats.ci_jobs_queued,
        calibration: cap_json("calibration", stats.calibration),
        fleet_calibration: cap_json("fleet_calibration", stats.fleet_calibration),
        suppressed_count,
        // Set by `Emitter::push`, which is the only place that knows.
        dropped_before: 0,
        box_stats: cap_json("box_stats", stats.box_stats),
    }
}

// ===========================================================================
// The emitter — coalescing and the bounded queue (no I/O)
// ===========================================================================

type CoalesceKey = (StopKind, StopLane, WhatClass);

#[derive(Debug, Clone)]
struct Window {
    opened: Instant,
    suppressed: u32,
    /// The latest suppressed stop and when it happened — the template for the
    /// TRAILING event that reports `suppressed` when the window expires with
    /// no later stop to carry it ([`Emitter::take_trailing`]).
    last: Option<(Stop, chrono::DateTime<chrono::Utc>)>,
}

impl Window {
    fn open(now: Instant) -> Self {
        Window {
            opened: now,
            suppressed: 0,
            last: None,
        }
    }
}

/// The coalescing verdict for one stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admit {
    /// Build and queue an event carrying this many earlier, suppressed stops.
    Post { suppressed: u32 },
    /// Inside the window — counted, not posted.
    Suppressed,
}

/// The process's pending events and coalescing state. No I/O and no clock of
/// its own: every method takes `now`, so the window and the bound are
/// testable with literal instants.
#[derive(Debug)]
pub(crate) struct Emitter {
    queue: VecDeque<PressureStopEvent>,
    cap: usize,
    /// Events lost since the last push: queue overflow, plus what coord said
    /// it did not store. Carried by the next pushed event.
    pending_dropped: u32,
    windows: HashMap<CoalesceKey, Window>,
    /// No POST before this instant (route absent, auth, 5xx backoff).
    next_attempt: Option<Instant>,
    /// Failure classes already warned about, so a stuck coord logs once.
    warned: HashSet<&'static str>,
}

impl Emitter {
    pub(crate) fn new(cap: usize) -> Self {
        Emitter {
            queue: VecDeque::new(),
            cap: cap.max(1),
            pending_dropped: 0,
            windows: HashMap::new(),
            next_attempt: None,
            warned: HashSet::new(),
        }
    }

    /// Coalesce one stop at `now` (`at` is the same instant on the wall clock,
    /// kept for a trailing event).
    pub(crate) fn admit(
        &mut self,
        stop: &Stop,
        now: Instant,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Admit {
        let key = stop.key();
        match self.windows.get_mut(&key) {
            Some(w) if now.saturating_duration_since(w.opened) < COALESCE_WINDOW => {
                w.suppressed = w.suppressed.saturating_add(1);
                w.last = Some((stop.clone(), at));
                Admit::Suppressed
            }
            Some(w) => {
                let suppressed = w.suppressed;
                *w = Window::open(now);
                Admit::Post { suppressed }
            }
            None => {
                self.windows.insert(key, Window::open(now));
                Admit::Post { suppressed: 0 }
            }
        }
    }

    /// Give back a count [`Self::admit`] handed to an event that was never
    /// queued (its stats read failed), so it rides the next one instead.
    pub(crate) fn restore_suppressed(&mut self, stop: &Stop, suppressed: u32) {
        if suppressed == 0 {
            return;
        }
        if let Some(w) = self.windows.get_mut(&stop.key()) {
            w.suppressed = w.suppressed.saturating_add(suppressed);
        }
    }

    /// Close every window older than [`COALESCE_WINDOW`] at `now`. One that
    /// still holds suppressed stops returns its latest one with the count of
    /// the OTHERS (`suppressed - 1`: the template is itself one of them), so a
    /// storm that ends is reported rather than silently dropped.
    pub(crate) fn take_trailing(
        &mut self,
        now: Instant,
    ) -> Vec<(Stop, chrono::DateTime<chrono::Utc>, u32)> {
        let mut out = Vec::new();
        self.windows.retain(|_, w| {
            if now.saturating_duration_since(w.opened) < COALESCE_WINDOW {
                return true;
            }
            if let (Some((stop, at)), true) = (w.last.take(), w.suppressed > 0) {
                out.push((stop, at, w.suppressed - 1));
            }
            false
        });
        out
    }

    /// Forget everything queued and pending: reporting was switched off.
    pub(crate) fn clear(&mut self) {
        self.queue.clear();
        self.windows.clear();
        self.pending_dropped = 0;
    }

    /// One event dropped off the front: it, and whatever loss IT was carrying.
    fn drop_front(&mut self) {
        if let Some(dropped) = self.queue.pop_front() {
            self.pending_dropped = self
                .pending_dropped
                .saturating_add(1)
                .saturating_add(dropped.dropped_before);
        }
    }

    /// Queue an event, dropping the oldest past the cap. The event carries
    /// every loss since the last push.
    pub(crate) fn push(&mut self, mut event: PressureStopEvent) {
        while self.queue.len() >= self.cap {
            self.drop_front();
        }
        event.dropped_before = event.dropped_before.saturating_add(self.pending_dropped);
        self.pending_dropped = 0;
        self.queue.push_back(event);
    }

    /// Take up to `max` of the oldest events for one POST.
    pub(crate) fn take_batch(&mut self, max: usize) -> Vec<PressureStopEvent> {
        let n = max.min(self.queue.len());
        self.queue.drain(..n).collect()
    }

    /// Put a batch that was not delivered back at the FRONT, in order; past
    /// the cap the oldest go and are counted.
    pub(crate) fn requeue(&mut self, batch: Vec<PressureStopEvent>) {
        for event in batch.into_iter().rev() {
            self.queue.push_front(event);
        }
        while self.queue.len() > self.cap {
            self.drop_front();
        }
    }

    /// Count `n` events coord did not store against the next push.
    pub(crate) fn note_lost(&mut self, n: u32) {
        self.pending_dropped = self.pending_dropped.saturating_add(n);
    }

    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub(crate) fn pending_dropped(&self) -> u32 {
        self.pending_dropped
    }

    /// `true` when a backoff forbids a POST at `now`.
    pub(crate) fn backing_off(&self, now: Instant) -> bool {
        self.next_attempt.is_some_and(|at| now < at)
    }

    /// Apply one delivery outcome for `batch`, sent at `now`. Returns the lines
    /// worth logging (empty on a clean store).
    pub(crate) fn apply(
        &mut self,
        delivery: Delivery,
        batch: Vec<PressureStopEvent>,
        now: Instant,
    ) -> Vec<(LogLevel, String)> {
        let mut lines = Vec::new();
        match delivery {
            Delivery::Stored {
                stored,
                duplicate,
                lost,
                problems,
            } => {
                self.next_attempt = None;
                // Delivery works again: the NEXT outage of any class is news.
                self.warned.clear();
                if lost > 0 {
                    self.note_lost(u32::try_from(lost).unwrap_or(u32::MAX));
                    lines.push((
                        LogLevel::Warn,
                        format!(
                            "coord stored {stored} and matched {duplicate} duplicate(s) of {} sent \
                             pressure-stop event(s); {lost} NOT stored, counted in the next \
                             event's dropped_before: {}",
                            batch.len(),
                            problems.join("; ")
                        ),
                    ));
                } else {
                    lines.push((
                        LogLevel::Debug,
                        format!(
                            "coord stored {stored}, duplicate {duplicate} pressure-stop event(s)"
                        ),
                    ));
                }
            }
            Delivery::Unknown { detail } => {
                self.next_attempt = None;
                if self.warned.insert("ack_unknown") {
                    lines.push((
                        LogLevel::Info,
                        format!(
                            "coord's 2xx body is not the documented outcome; whether {} \
                             pressure-stop event(s) were stored is UNKNOWN: {detail}",
                            batch.len()
                        ),
                    ));
                }
            }
            Delivery::Requeue {
                class,
                backoff,
                detail,
            } => {
                self.next_attempt = backoff.map(|b| now + b);
                self.requeue(batch);
                if self.warned.insert(class) {
                    let level = match class {
                        // A coord or web deploy behind this runner: expected,
                        // and the events wait in the bounded queue.
                        "route_absent" | "table_absent" | "unavailable" => LogLevel::Info,
                        _ => LogLevel::Warn,
                    };
                    lines.push((
                        level,
                        format!("pressure-stop events kept queued ({class}): {detail}"),
                    ));
                }
            }
            Delivery::Drop { class, detail } => {
                self.next_attempt = None;
                self.note_lost(u32::try_from(batch.len()).unwrap_or(u32::MAX));
                if self.warned.insert(class) {
                    lines.push((
                        LogLevel::Warn,
                        format!(
                            "coord refused {} pressure-stop event(s) ({class}) — dropped and \
                             counted in the next event's dropped_before: {detail}",
                            batch.len()
                        ),
                    ));
                }
            }
        }
        lines
    }
}

/// The level a delivery line is logged at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogLevel {
    Debug,
    Info,
    Warn,
}

// ===========================================================================
// Response triage (PURE)
// ===========================================================================

/// What one POST established.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Delivery {
    /// Coord's documented outcome. `lost` = sent − stored − duplicate.
    Stored {
        stored: u64,
        duplicate: u64,
        lost: u64,
        problems: Vec<String>,
    },
    /// A 2xx whose body is not the documented outcome.
    Unknown { detail: String },
    /// Not delivered; keep the events and try later.
    Requeue {
        class: &'static str,
        backoff: Option<Duration>,
        detail: String,
    },
    /// Refused on its merits; retrying the same bytes cannot help.
    Drop { class: &'static str, detail: String },
}

/// A jittered server backoff in `[SERVER_BACKOFF, 2 × SERVER_BACKOFF]`, from
/// the clock's sub-second noise (no RNG dependency for one number).
fn jittered_server_backoff() -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    SERVER_BACKOFF
        + Duration::from_millis(u64::from(nanos) % (SERVER_BACKOFF.as_millis() as u64 + 1))
}

/// Triage a response to a POST of `sent` events. `body` is `None` when it
/// could not be read. PURE except for the jitter on the 5xx arm.
pub(crate) fn triage(status: u16, body: Option<&str>, sent: usize) -> Delivery {
    match status {
        200..=299 => triage_ok(body, sent),
        404 | 405 => Delivery::Requeue {
            class: "route_absent",
            backoff: Some(ABSENT_BACKOFF),
            detail: format!(
                "HTTP {status} — coord does not serve POST /coord/devices/:id/resource-stop yet"
            ),
        },
        503 => Delivery::Requeue {
            class: "unavailable",
            backoff: None,
            detail: "HTTP 503".to_string(),
        },
        401 | 403 => Delivery::Requeue {
            class: "auth",
            backoff: Some(AUTH_BACKOFF),
            detail: format!("HTTP {status} — the device credential was not accepted"),
        },
        422 => Delivery::Drop {
            class: "unprocessable",
            detail: format!("HTTP 422: {}", body.unwrap_or("<unreadable body>")),
        },
        500..=599 => Delivery::Requeue {
            class: "server_error",
            backoff: Some(jittered_server_backoff()),
            detail: format!("HTTP {status}"),
        },
        _ => Delivery::Drop {
            class: "client_error",
            detail: format!("HTTP {status}: {}", body.unwrap_or("<unreadable body>")),
        },
    }
}

fn triage_ok(body: Option<&str>, sent: usize) -> Delivery {
    let Some(v) = body
        .and_then(|b| serde_json::from_str::<Value>(b).ok())
        .filter(Value::is_object)
    else {
        return Delivery::Unknown {
            detail: "body is not a JSON object".to_string(),
        };
    };
    // The route exists but the web migration has not created the table: the
    // events are storable later, so they wait (bounded) rather than count lost.
    if v.get("table_present").and_then(Value::as_bool) == Some(false) {
        return Delivery::Requeue {
            class: "table_absent",
            backoff: Some(ABSENT_BACKOFF),
            detail: "coord.resource_pressure_stop_events is not present yet".to_string(),
        };
    }
    let (Some(stored), Some(duplicate)) = (
        v.get("stored").and_then(Value::as_u64),
        v.get("duplicate").and_then(Value::as_u64),
    ) else {
        return Delivery::Unknown {
            detail: "no stored/duplicate counts".to_string(),
        };
    };
    let lost = (sent as u64).saturating_sub(stored.saturating_add(duplicate));
    let mut problems = Vec::new();
    if let Some(e) = v.get("persist_error").and_then(Value::as_str) {
        problems.push(format!("persist_error: {e}"));
    }
    if let Some(rejected) = v.get("rejected").and_then(Value::as_array) {
        for r in rejected {
            if let Some(reason) = r.get("reason").and_then(Value::as_str) {
                problems.push(format!("rejected: {reason}"));
            }
        }
    }
    Delivery::Stored {
        stored,
        duplicate,
        lost,
        problems,
    }
}

// ===========================================================================
// Box stats — what the machine was carrying (cheap, cached, failure-tolerant)
// ===========================================================================

/// The live-claude count from a census `(checked_at_ms, total)` read at
/// `now_ms`: the count when the reading is no older than `max_age`, else
/// `None` with a reason. The age goes in `box_stats` either way. PURE.
pub(crate) fn live_claude_from_census(
    census: Option<(i64, usize)>,
    now_ms: i64,
    max_age: Duration,
) -> (Option<i64>, Option<i64>, Option<String>) {
    let Some((checked_at_ms, total)) = census else {
        return (
            None,
            None,
            Some("no session-tracking census has completed yet".to_string()),
        );
    };
    let age_ms = now_ms.saturating_sub(checked_at_ms);
    let max_ms = i64::try_from(max_age.as_millis()).unwrap_or(i64::MAX);
    if age_ms > max_ms {
        return (
            None,
            Some(age_ms),
            Some(format!(
                "the session-tracking census is {age_ms} ms old, past the {max_ms} ms bound"
            )),
        );
    }
    (i64::try_from(total).ok(), Some(age_ms), None)
}

/// The machine-calibration state of plan
/// `2026-10-05-fleet-calibration-reversible-machine-tuning`. The ONE seam that
/// plan fills; until its runner collector exists this is a typed UNKNOWN —
/// never `{}` and never a zero.
pub(crate) fn fleet_calibration_snapshot() -> Value {
    json!({"state": "unknown", "reason": "fleet-calibration state source not shipped"})
}

/// One gauge read behind `catch_unwind`: a read that panics (a poisoned lock
/// whose owner `unwrap`s) is UNKNOWN, never a panic in the admission path.
fn guarded<T>(read: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(read)).ok()
}

fn floors_json(floors: &crate::settings::SessionGuardSettings) -> Value {
    json!({
        "enabled": floors.enabled,
        "warn_free_commit_bytes": floors.warn_free_commit_bytes,
        "critical_free_commit_bytes": floors.critical_free_commit_bytes,
    })
}

/// What the runner itself can read about its calibration: the effective thread
/// ceilings with each term's source, the measured baselines, the memory floors
/// (local, fleet dial and effective, so the source of each is visible).
fn calibration_snapshot() -> Value {
    let thread_ceilings = guarded(crate::resource_guard::thread_ceilings_health_json)
        .unwrap_or_else(|| json!({"state": "unknown", "reason": "thread-ceiling read failed"}));
    let local = guarded(crate::settings::get_session_guard_settings);
    let effective = |lane: &str| -> Value {
        local
            .as_ref()
            .and_then(|l| guarded(|| crate::resource_guard::effective_session_floors(l, lane)))
            .map(|f| floors_json(&f))
            .unwrap_or(Value::Null)
    };
    let dial = guarded(crate::mcp::fleet_policy_poller::dial_snapshot);
    json!({
        "thread_ceilings": thread_ceilings,
        "at_rest_thread_baseline": guarded(crate::resource_guard::at_rest_thread_baseline).flatten(),
        "measured_per_session_threads": guarded(crate::resource_guard::measured_per_session_threads).flatten(),
        "memory_floors": {
            "local": local.as_ref().map(floors_json),
            "effective": {"host": effective("host"), "wsl": effective("wsl")},
        },
        "fleet_dial": dial.map(|d| json!({
            "poll_interval_ms": u64::try_from(d.poll_interval_ms).ok(),
            "host_warn_free_bytes": d.host_warn_free_bytes,
            "host_critical_free_bytes": d.host_critical_free_bytes,
            "wsl_warn_free_bytes": d.wsl_warn_free_bytes,
            "wsl_critical_free_bytes": d.wsl_critical_free_bytes,
            "threads_warn_count": d.threads_warn_count,
            "threads_critical_count": d.threads_critical_count,
        })),
    })
}

/// Read the box at the stop. Every reading is cached or cheap (the guard's
/// memoized thread count, the census the background loop keeps, one
/// `GlobalMemoryStatusEx`), and every one that fails is `null` with a reason.
pub(crate) fn collect_box_stats() -> BoxStats {
    let mut unknown = serde_json::Map::new();

    let session_count_terminal =
        guarded(crate::fleet::resource_sample::live_terminal_session_count)
            .flatten()
            .map(i64::from);
    if session_count_terminal.is_none() {
        unknown.insert(
            "session_count_terminal".into(),
            json!("no readable terminal manager (no app handle yet, or a poisoned registry)"),
        );
    }

    let census = guarded(crate::session::tracking_health::latest)
        .flatten()
        .map(|r| (r.checked_at_ms, r.live_claude_total));
    let (session_count_live_claude, census_age_ms, census_reason) = live_claude_from_census(
        census,
        chrono::Utc::now().timestamp_millis(),
        CENSUS_MAX_AGE,
    );
    if let Some(reason) = census_reason {
        unknown.insert("session_count_live_claude".into(), json!(reason));
    }

    let occupancy = guarded(crate::ci_node::admission::occupancy);
    let (active_ci_runners, ci_jobs_queued) = match occupancy {
        Some((running, queued)) => (i64::try_from(running).ok(), i64::try_from(queued).ok()),
        None => {
            unknown.insert(
                "active_ci_runners".into(),
                json!("the CI admission state could not be read"),
            );
            unknown.insert(
                "ci_jobs_queued".into(),
                json!("the CI admission state could not be read"),
            );
            (None, None)
        }
    };

    let thread_count = guarded(crate::health_monitor::thread_count_reading_memoized).flatten();
    if thread_count.is_none() {
        unknown.insert(
            "thread_count".into(),
            json!("the runner's thread count could not be read"),
        );
    }
    let free_commit_bytes =
        guarded(crate::fleet::resource_sample::available_commit_bytes).flatten();
    if free_commit_bytes.is_none() {
        unknown.insert(
            "free_commit_bytes".into(),
            json!("free commit could not be read on this platform or this tick"),
        );
    }

    BoxStats {
        session_count_terminal,
        session_count_live_claude,
        active_ci_runners,
        ci_jobs_queued,
        calibration: calibration_snapshot(),
        fleet_calibration: fleet_calibration_snapshot(),
        box_stats: json!({
            "thread_count": thread_count,
            "free_commit_bytes": free_commit_bytes,
            "live_claude_census_age_ms": census_age_ms,
            "live_claude_census_max_age_ms": u64::try_from(CENSUS_MAX_AGE.as_millis()).ok(),
            "unknown": Value::Object(unknown),
        }),
    }
}

// ===========================================================================
// The process-global sink — what the hook sites call
// ===========================================================================

/// `Settings.report_pressure_stops`, cached. Starts ON (the shipped default)
/// and is read from settings when the republisher starts, on every flush tick
/// and on save; an unreadable settings file keeps the last known value. A stop
/// queued while the cache still said ON on a box whose setting is OFF never
/// leaves the machine: [`flush`] re-reads the switch BEFORE posting and
/// discards the queue when it is off.
static ENABLED: AtomicBool = AtomicBool::new(true);

fn emitter() -> &'static Mutex<Emitter> {
    static EMITTER: OnceLock<Mutex<Emitter>> = OnceLock::new();
    EMITTER.get_or_init(|| Mutex::new(Emitter::new(QUEUE_CAP)))
}

fn lock_emitter() -> std::sync::MutexGuard<'static, Emitter> {
    emitter()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Set the cached switch (the settings writers call this after a save).
/// Switching OFF also discards everything queued: off means nothing leaves.
pub(crate) fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if !enabled {
        lock_emitter().clear();
    }
}

pub(crate) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Re-read the switch; a non-authoritative (unreadable) settings load leaves
/// the cached value as it was. Called when the republisher starts, so the
/// cache reflects the setting from the loop's first moment.
pub(crate) fn refresh_enabled_from_settings() {
    if let Some(v) = crate::settings::get_report_pressure_stops() {
        set_enabled(v);
    }
}

/// Record one stop. The hook sites call this, after the verdict and before the
/// refusal returns. Never blocks on I/O, never panics, never changes the
/// verdict: when the switch is off it does nothing at all.
pub(crate) fn record(stop: Stop) {
    record_into(emitter(), enabled(), stop, collect_box_stats);
}

/// One stop, as a hook site describes it.
#[derive(Debug, Clone)]
pub(crate) struct Stop {
    pub(crate) kind: StopKind,
    pub(crate) severity: Severity,
    pub(crate) what: WhatClass,
    pub(crate) reading: StopReading,
    pub(crate) reason: Option<String>,
    /// Site-specific facts merged into `box_stats` (a JSON object, or `Null`
    /// for none): what a CI term measured, that a reading is a projection.
    pub(crate) extra: Value,
}

impl Stop {
    fn key(&self) -> CoalesceKey {
        (self.kind, self.reading.lane, self.what)
    }
}

/// `extra`'s keys merged into the `box_stats` object (extra wins on a clash;
/// a non-object `extra` adds nothing). PURE.
pub(crate) fn merge_extra(mut box_stats: Value, extra: &Value) -> Value {
    if let (Some(target), Some(add)) = (box_stats.as_object_mut(), extra.as_object()) {
        for (k, v) in add {
            target.insert(k.clone(), v.clone());
        }
    }
    box_stats
}

/// The stats a TRAILING event carries: a coalesced stop is counted, never
/// measured, so every count is UNKNOWN and says why.
pub(crate) fn trailing_stats() -> BoxStats {
    let why = "not read: this event reports stops coalesced inside the window, whose box \
               stats are never collected";
    BoxStats {
        session_count_terminal: None,
        session_count_live_claude: None,
        active_ci_runners: None,
        ci_jobs_queued: None,
        calibration: json!({"state": "unknown", "reason": why}),
        fleet_calibration: fleet_calibration_snapshot(),
        box_stats: json!({
            "trailing": true,
            "unknown": {
                "session_count_terminal": why,
                "session_count_live_claude": why,
                "active_ci_runners": why,
                "ci_jobs_queued": why,
            },
        }),
    }
}

/// Queue a trailing event for every expired window that still holds
/// suppressed stops ([`Emitter::take_trailing`]). Returns how many it queued.
pub(crate) fn queue_trailing(em: &mut Emitter, now: Instant) -> usize {
    let trailing = em.take_trailing(now);
    let n = trailing.len();
    for (stop, at, others) in trailing {
        let stats = BoxStats {
            box_stats: merge_extra(trailing_stats().box_stats, &stop.extra),
            ..trailing_stats()
        };
        em.push(build_event(
            uuid::Uuid::new_v4().to_string(),
            at,
            stop.kind,
            stop.severity,
            stop.what,
            &stop.reading,
            stop.reason,
            stats,
            others,
        ));
    }
    n
}

/// [`record`] over an explicit emitter, switch and stats reader, so the
/// off-switch, the coalescing and a failing reader are testable without the
/// process globals. Returns whether an event was queued. Never panics: a
/// reader (or anything else here) that panics queues nothing and returns
/// `false`.
pub(crate) fn record_into(
    sink: &Mutex<Emitter>,
    enabled: bool,
    stop: Stop,
    collect: impl FnOnce() -> BoxStats,
) -> bool {
    if !enabled {
        return false;
    }
    let lock = || sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let kind = stop.kind;
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let at = chrono::Utc::now();
        let admit = lock().admit(&stop, Instant::now(), at);
        let Admit::Post { suppressed } = admit else {
            return false;
        };
        // Stats are read OUTSIDE the emitter lock: they take other locks. A
        // read that panics gives the carried count back to the window.
        let Ok(stats) = catch_unwind(AssertUnwindSafe(collect)) else {
            lock().restore_suppressed(&stop, suppressed);
            return false;
        };
        let stats = BoxStats {
            box_stats: merge_extra(stats.box_stats, &stop.extra),
            ..stats
        };
        let event = build_event(
            uuid::Uuid::new_v4().to_string(),
            at,
            stop.kind,
            stop.severity,
            stop.what,
            &stop.reading,
            stop.reason,
            stats,
            suppressed,
        );
        lock().push(event);
        true
    }));
    outcome.unwrap_or_else(|_| {
        warn!(
            "pressure_stop: recording a {} stop panicked — the stop itself is unaffected",
            kind.as_str()
        );
        false
    })
}

/// [`record`] for a guard verdict: the severity word and observation
/// `SpawnGate::tripped` hands out, with the reason in the continuation stamp
/// grammar (`thread_pressure:<sev>:<obs>_over_<lim>` /
/// `commit_pressure:<sev>:<obs>_under_<lim>`), authored in one place.
///
/// `extra` is merged into `box_stats` (`Value::Null` for nothing) — e.g. that
/// `observed` is a PROJECTION charged with admitted, not-yet-running spawns.
pub(crate) fn record_observation(
    kind: StopKind,
    severity: &str,
    what: WhatClass,
    observation: &GateObservation,
    extra: Value,
) {
    let (Some(sev), Some(reading)) = (
        Severity::from_word(severity),
        StopReading::from_observation(observation),
    ) else {
        debug!(
            "pressure_stop: not recording a stop with severity {severity:?} on lane {:?}",
            observation.lane
        );
        return;
    };
    let reason = crate::agent_runtime::load_pressure_stamp_reason(severity, observation);
    record(Stop {
        kind,
        severity: sev,
        what,
        reading,
        reason: Some(reason),
        extra,
    });
}

/// The CI headroom term that deferred a dispatch, as a stop.
///
/// - **commit**: the host pool's free commit under the defer floor — the
///   memory lane's own direction, so it fills `observed` / `limit_value`.
/// - **swap**: swap USED over a share of the ceiling. Memory class, but the
///   opposite direction to "free under a floor", so the two numbers ride
///   `box_stats` and the columns stay `null` rather than read backwards.
/// - **saturation**: tasks (threads or PIDs, named in `box_stats`) over a
///   share of the ceiling that bounds them — the thread class, `threads` pool.
pub(crate) fn ci_headroom_stop(term: crate::ci_node::admission::HeadroomTerm) -> Stop {
    use crate::ci_node::admission::HeadroomTerm;
    let (reading, reason, extra) = match term {
        HeadroomTerm::Swap {
            used_bytes,
            limit_bytes,
        } => (
            StopReading {
                lane: StopLane::MemoryFreeCommit,
                guard_lane: "host",
                observed: None,
                limit: None,
            },
            format!("ci_headroom:swap:{used_bytes}_over_{limit_bytes}"),
            json!({
                "ci_headroom_term": "swap",
                "swap_used_bytes": used_bytes,
                "swap_defer_bytes": limit_bytes,
            }),
        ),
        HeadroomTerm::Saturation {
            used,
            limit,
            counter,
        } => (
            StopReading {
                lane: StopLane::ThreadPressure,
                guard_lane: "threads",
                observed: Some(used),
                limit: Some(limit),
            },
            format!("ci_headroom:saturation:{used}_over_{limit}"),
            json!({"ci_headroom_term": "saturation", "saturation_counter": counter}),
        ),
        HeadroomTerm::Commit {
            free_bytes,
            floor_bytes,
        } => (
            StopReading {
                lane: StopLane::MemoryFreeCommit,
                guard_lane: "host",
                observed: Some(free_bytes),
                limit: Some(floor_bytes),
            },
            format!("ci_headroom:commit:{free_bytes}_under_{floor_bytes}"),
            json!({"ci_headroom_term": "commit"}),
        ),
    };
    Stop {
        kind: StopKind::DeferredCiJob,
        severity: Severity::Warn,
        what: WhatClass::CiJob,
        reading,
        reason: Some(reason),
        extra,
    }
}

/// Record a CI dispatch deferred on headroom (never the at-cap defer).
pub(crate) fn record_ci_headroom(term: crate::ci_node::admission::HeadroomTerm) {
    record(ci_headroom_stop(term));
}

// ===========================================================================
// Delivery — one batch per fleet republisher tick
// ===========================================================================

/// `POST {base}/coord/devices/{device_id}/resource-stop`.
pub(crate) fn stop_url(base: &str, device_id: &str) -> String {
    format!(
        "{}/coord/devices/{device_id}/resource-stop",
        base.trim_end_matches('/')
    )
}

/// POST one batch and triage the answer.
pub(crate) async fn send_batch(
    client: &reqwest::Client,
    url: &str,
    batch: &[PressureStopEvent],
) -> Delivery {
    let body = json!({ "events": batch });
    // The route is device-keyed: coord takes the tenant from the device
    // principal and binds the path id to it, never from the body.
    let request = crate::auth::attach_device_auth_for(
        client.post(url).timeout(POST_TIMEOUT).json(&body),
        crate::auth::TenantScope::Device,
    );
    match request.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let text = resp.text().await.ok();
            triage(status, text.as_deref(), batch.len())
        }
        Err(e) => Delivery::Requeue {
            class: "transport",
            backoff: Some(jittered_server_backoff()),
            detail: format!("POST {url} failed: {e}"),
        },
    }
}

fn log_lines(lines: Vec<(LogLevel, String)>) {
    for (level, line) in lines {
        match level {
            LogLevel::Debug => debug!("pressure_stop: {line}"),
            LogLevel::Info => info!("pressure_stop: {line}"),
            LogLevel::Warn => warn!("pressure_stop: {line}"),
        }
    }
}

/// Deliver one batch of queued stops. Called from the fleet republisher loop
/// beside `resource_sample::publish_once`, so it rides that loop's
/// primary-only gate and cadence. Best-effort: no device, no coord base or no
/// client leaves the queue as it is.
pub(crate) async fn flush() {
    refresh_enabled_from_settings();
    if !enabled() {
        // Off means nothing leaves: drop anything queued before the switch
        // was read (set_enabled(false) already did, this is belt and braces).
        lock_emitter().clear();
        return;
    }
    queue_trailing(&mut lock_emitter(), Instant::now());
    let Some(device) = crate::fleet::load_device_file() else {
        return;
    };
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        return;
    };
    let Some(client) = crate::coord_http::coord_client() else {
        return;
    };
    let batch = {
        let mut em = lock_emitter();
        if em.backing_off(Instant::now()) {
            return;
        }
        em.take_batch(MAX_BATCH)
    };
    if batch.is_empty() {
        return;
    }
    let url = stop_url(&base, &device.device_id);
    let delivery = send_batch(client, &url, &batch).await;
    let lines = lock_emitter().apply(delivery, batch, Instant::now());
    log_lines(lines);
}

#[cfg(test)]
mod tests;
