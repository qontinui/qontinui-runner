//! A circuit-breaker state machine shared by every breaker in the runner.
//!
//! Plan `2026-10-19-runner-pg-unreachable-costs-every-caller-a-pool-timeout`
//! D8: one implementation, configured per use, instead of one private state
//! machine per subsystem. Two configurations exist today:
//!
//! * **PostgreSQL** (`crate::pg_circuit`): one connect-class failure trips the
//!   breaker, a single background prober is the only actor that touches the
//!   network while it is not Closed, and the retry delay backs off
//!   1 s, 2 s, 4 s, 8 s, then holds at 15 s ([`ProbeMode::Background`]).
//! * **AI providers** (`ai_provider::circuit_breaker`, bin): four consecutive
//!   failures trip it, it stays Open for a fixed 60 s, and then the next caller
//!   is admitted as the probe; two consecutive successes close it
//!   ([`ProbeMode::CallerPermit`]).
//!
//! # States
//!
//! * **Closed** — callers pass. The check is ONE atomic load
//!   ([`Circuit::is_closed`]); the lock is only taken to record an outcome.
//! * **Open** — callers are rejected with no I/O. Carries `since` (the
//!   ORIGINAL trip, kept across failed trials), `cause`, `last_error` and
//!   `retry_at`.
//! * **HalfOpen** — a trial is in flight. In background mode callers are still
//!   rejected; in caller-permit mode up to `half_open_max_probes` callers are
//!   admitted as probes.
//!
//! The lock is a `std::sync::Mutex` and is never held across an `.await`.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// The three breaker states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitState {
    fn as_u8(self) -> u8 {
        match self {
            CircuitState::Closed => 0,
            CircuitState::Open => 1,
            CircuitState::HalfOpen => 2,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => CircuitState::Closed,
            1 => CircuitState::Open,
            _ => CircuitState::HalfOpen,
        }
    }

    /// Lower-case wire spelling (`closed` / `open` / `half_open`).
    pub fn as_str(self) -> &'static str {
        match self {
            CircuitState::Closed => "closed",
            CircuitState::Open => "open",
            CircuitState::HalfOpen => "half_open",
        }
    }
}

/// Who decides when a trial runs while the breaker is not Closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeMode {
    /// A background task owns trials ([`Circuit::begin_trial`]); callers are
    /// always rejected while not Closed, so no caller ever pays a probe's
    /// latency.
    Background,
    /// Once `retry_at` passes, [`Circuit::allow`] admits callers as probes, up
    /// to `half_open_max_probes` at a time.
    CallerPermit,
}

/// Retry-delay schedule: `initial * factor^n`, capped at `cap`, where `n` is
/// the number of failed trials since the breaker opened. `factor == 1` is a
/// fixed open timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub factor: u32,
    pub cap: Duration,
}

impl Backoff {
    /// The delay before trial number `failed_trials` (0 for the first trial).
    pub fn delay(&self, failed_trials: u32) -> Duration {
        let mut d = self.initial;
        for _ in 0..failed_trials {
            if d >= self.cap {
                break;
            }
            d = d.saturating_mul(self.factor.max(1));
        }
        d.min(self.cap)
    }
}

/// Breaker configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CircuitConfig {
    /// Consecutive trip-class failures (while Closed) that open the breaker.
    pub failure_threshold: u32,
    /// Consecutive successes (while HalfOpen) that close it.
    pub success_threshold: u32,
    pub backoff: Backoff,
    /// Callers admitted at once while HalfOpen (caller-permit mode only).
    pub half_open_max_probes: u32,
    pub probe_mode: ProbeMode,
}

/// What a non-tripping failure was, so it can be counted under its own name
/// rather than vanishing (plan D2: a wait timeout must never read as
/// unreachability, and must stay visible).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonTripKind {
    /// No free slot within the wait timeout: the resource is busy, or every
    /// slot is held by in-flight work. Never evidence that it is down.
    WaitTimeout,
    /// The backend ANSWERED with an error: it is reachable.
    BackendError,
    /// Evidence that cannot distinguish "down" from anything else.
    Ambiguous,
}

/// The outcome of one guarded operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Success,
    /// Unambiguous evidence the resource is unreachable.
    TripFailure(String),
    /// A failure that must never open the breaker.
    NonTripFailure(NonTripKind),
}

/// The admission decision for one caller.
#[derive(Debug, Clone, PartialEq)]
pub enum Admission {
    Pass,
    /// Admitted as a HalfOpen probe (caller-permit mode). The caller must
    /// [`Circuit::record`] its outcome, which releases the probe slot.
    ProbePermit,
    Reject(CircuitSnapshot),
}

/// A point-in-time copy of everything the breaker knows, for error bodies and
/// `/health`.
#[derive(Debug, Clone, PartialEq)]
pub struct CircuitSnapshot {
    pub state: CircuitState,
    /// When the CURRENT outage began (the original trip). `None` while Closed.
    pub since: Option<SystemTime>,
    pub cause: Option<String>,
    pub last_error: Option<String>,
    /// How long until the next trial may run. `None` while Closed.
    pub retry_after: Option<Duration>,
    pub failed_trials: u32,
    /// Times the breaker has opened over its lifetime.
    pub trips: u64,
    pub wait_timeouts: u64,
    pub wait_timeout_last_at: Option<SystemTime>,
    pub backend_errors: u64,
    pub ambiguous_errors: u64,
}

/// A time source, injectable so tests drive transitions with literals.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn wall_now(&self) -> SystemTime;
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// A clock that only moves when told to. Test support, public so the bin
/// crate's tests can use it too.
#[derive(Debug)]
pub struct ManualClock {
    base: Instant,
    wall_base: SystemTime,
    offset: Mutex<Duration>,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self {
            base: Instant::now(),
            wall_base: SystemTime::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }
}

impl ManualClock {
    pub fn advance(&self, by: Duration) {
        let mut o = self.offset.lock().unwrap_or_else(|p| p.into_inner());
        *o += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + *self.offset.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn wall_now(&self) -> SystemTime {
        self.wall_base + *self.offset.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[derive(Debug)]
struct Inner {
    state: CircuitState,
    consecutive_failures: u32,
    consecutive_successes: u32,
    active_probes: u32,
    since: Option<Instant>,
    since_wall: Option<SystemTime>,
    cause: Option<String>,
    last_error: Option<String>,
    retry_at: Option<Instant>,
    failed_trials: u32,
    trial_in_flight: bool,
    trips: u64,
    wait_timeouts: u64,
    wait_timeout_last_at: Option<SystemTime>,
    backend_errors: u64,
    ambiguous_errors: u64,
}

/// A trial slot from [`Circuit::begin_trial`]. Report its result with
/// [`Circuit::finish_trial`]; dropping it unreported releases the slot and
/// leaves the breaker as it was (a cancelled trial is not evidence).
#[must_use = "report the trial with Circuit::finish_trial"]
pub struct TrialPermit {
    circuit: Arc<Circuit>,
    reported: bool,
}

impl Drop for TrialPermit {
    fn drop(&mut self) {
        if !self.reported {
            let mut g = self.circuit.lock();
            g.trial_in_flight = false;
        }
    }
}

/// The breaker.
pub struct Circuit {
    config: CircuitConfig,
    clock: Arc<dyn Clock>,
    state: AtomicU8,
    inner: Mutex<Inner>,
    opened: tokio::sync::Notify,
    closed: tokio::sync::Notify,
}

impl std::fmt::Debug for Circuit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Circuit")
            .field("config", &self.config)
            .field("state", &self.state())
            .finish()
    }
}

impl Circuit {
    pub fn new(config: CircuitConfig) -> Self {
        Self::with_clock(config, Arc::new(SystemClock))
    }

    pub fn with_clock(config: CircuitConfig, clock: Arc<dyn Clock>) -> Self {
        Self {
            config,
            clock,
            state: AtomicU8::new(CircuitState::Closed.as_u8()),
            inner: Mutex::new(Inner {
                state: CircuitState::Closed,
                consecutive_failures: 0,
                consecutive_successes: 0,
                active_probes: 0,
                since: None,
                since_wall: None,
                cause: None,
                last_error: None,
                retry_at: None,
                failed_trials: 0,
                trial_in_flight: false,
                trips: 0,
                wait_timeouts: 0,
                wait_timeout_last_at: None,
                backend_errors: 0,
                ambiguous_errors: 0,
            }),
            opened: tokio::sync::Notify::new(),
            closed: tokio::sync::Notify::new(),
        }
    }

    pub fn config(&self) -> &CircuitConfig {
        &self.config
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The fast path: one atomic load.
    #[inline]
    pub fn is_closed(&self) -> bool {
        self.state.load(Ordering::Acquire) == CircuitState::Closed.as_u8()
    }

    pub fn state(&self) -> CircuitState {
        CircuitState::from_u8(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, g: &mut Inner, s: CircuitState) {
        g.state = s;
        self.state.store(s.as_u8(), Ordering::Release);
    }

    /// Resolves once the breaker is not Closed: immediately when it already
    /// is, otherwise at the next trip. The waiter is registered BEFORE the
    /// state is checked, so a trip racing this call is never missed.
    pub async fn wait_opened(&self) {
        let n = self.opened.notified();
        tokio::pin!(n);
        n.as_mut().enable();
        if !self.is_closed() {
            return;
        }
        n.await;
    }

    /// Resolves once the breaker is Closed (immediately when it already is).
    pub async fn wait_closed(&self) {
        let n = self.closed.notified();
        tokio::pin!(n);
        n.as_mut().enable();
        if self.is_closed() {
            return;
        }
        n.await;
    }

    /// Admit or reject one caller.
    pub fn allow(&self) -> Admission {
        if self.is_closed() {
            return Admission::Pass;
        }
        let mut g = self.lock();
        let now = self.clock.now();
        match g.state {
            CircuitState::Closed => Admission::Pass,
            CircuitState::Open => {
                if self.config.probe_mode == ProbeMode::CallerPermit
                    && g.retry_at.is_some_and(|t| now >= t)
                {
                    self.set_state(&mut g, CircuitState::HalfOpen);
                    g.consecutive_successes = 0;
                    g.active_probes = 1;
                    Admission::ProbePermit
                } else {
                    Admission::Reject(self.snapshot_locked(&g, now))
                }
            }
            CircuitState::HalfOpen => {
                if self.config.probe_mode == ProbeMode::CallerPermit
                    && g.active_probes < self.config.half_open_max_probes
                {
                    g.active_probes += 1;
                    Admission::ProbePermit
                } else {
                    Admission::Reject(self.snapshot_locked(&g, now))
                }
            }
        }
    }

    /// Record the outcome of an operation.
    ///
    /// * Closed: a success resets the failure run; `failure_threshold`
    ///   consecutive trip failures open the breaker.
    /// * HalfOpen: releases a probe slot; `success_threshold` consecutive
    ///   successes close it; a trip failure re-opens it with the next backoff.
    /// * Open: outcomes are ignored (they come from calls admitted before the
    ///   trip), except that non-trip failures are still counted.
    pub fn record(&self, outcome: Outcome) {
        let mut g = self.lock();
        let now = self.clock.now();
        if let Outcome::NonTripFailure(kind) = &outcome {
            self.count_non_trip(&mut g, *kind);
        }
        match (g.state, outcome) {
            (CircuitState::Closed, Outcome::Success) => g.consecutive_failures = 0,
            (CircuitState::Closed, Outcome::TripFailure(err)) => {
                g.consecutive_failures += 1;
                if g.consecutive_failures >= self.config.failure_threshold.max(1) {
                    self.open_locked(&mut g, now, "runtime", err);
                } else {
                    g.last_error = Some(err);
                }
            }
            (CircuitState::HalfOpen, Outcome::Success) => {
                g.active_probes = g.active_probes.saturating_sub(1);
                g.consecutive_successes += 1;
                if g.consecutive_successes >= self.config.success_threshold.max(1) {
                    self.close_locked(&mut g);
                }
            }
            (CircuitState::HalfOpen, Outcome::TripFailure(err)) => {
                g.active_probes = g.active_probes.saturating_sub(1);
                self.reopen_locked(&mut g, now, err);
            }
            (CircuitState::HalfOpen, Outcome::NonTripFailure(_)) => {
                g.active_probes = g.active_probes.saturating_sub(1);
            }
            _ => {}
        }
    }

    fn count_non_trip(&self, g: &mut Inner, kind: NonTripKind) {
        match kind {
            NonTripKind::WaitTimeout => {
                g.wait_timeouts += 1;
                g.wait_timeout_last_at = Some(self.clock.wall_now());
            }
            NonTripKind::BackendError => g.backend_errors += 1,
            NonTripKind::Ambiguous => g.ambiguous_errors += 1,
        }
    }

    /// Open the breaker now, whatever its state, naming the cause (e.g.
    /// `boot` for a runner that started without its database). A no-op when
    /// already Open or HalfOpen, except that the newest error is recorded.
    pub fn force_open(&self, cause: &str, last_error: impl Into<String>) {
        let mut g = self.lock();
        let now = self.clock.now();
        if g.state == CircuitState::Closed {
            self.open_locked(&mut g, now, cause, last_error.into());
        } else {
            g.last_error = Some(last_error.into());
        }
    }

    /// Close the breaker unconditionally (manual reset).
    pub fn reset(&self) {
        let mut g = self.lock();
        self.close_locked(&mut g);
    }

    fn open_locked(&self, g: &mut Inner, now: Instant, cause: &str, err: String) {
        self.set_state(g, CircuitState::Open);
        g.since = Some(now);
        g.since_wall = Some(self.clock.wall_now());
        g.cause = Some(cause.to_string());
        g.last_error = Some(err);
        g.failed_trials = 0;
        g.retry_at = Some(now + self.config.backoff.delay(0));
        g.consecutive_successes = 0;
        g.active_probes = 0;
        g.trips += 1;
        self.opened.notify_waiters();
    }

    fn reopen_locked(&self, g: &mut Inner, now: Instant, err: String) {
        self.set_state(g, CircuitState::Open);
        g.failed_trials = g.failed_trials.saturating_add(1);
        g.last_error = Some(err);
        g.retry_at = Some(now + self.config.backoff.delay(g.failed_trials));
        g.consecutive_successes = 0;
        g.active_probes = 0;
        g.trial_in_flight = false;
        // `since` deliberately kept: the outage began at the ORIGINAL trip.
    }

    fn close_locked(&self, g: &mut Inner) {
        let was_closed = g.state == CircuitState::Closed;
        self.set_state(g, CircuitState::Closed);
        g.consecutive_failures = 0;
        g.consecutive_successes = 0;
        g.active_probes = 0;
        g.since = None;
        g.since_wall = None;
        g.cause = None;
        g.retry_at = None;
        g.failed_trials = 0;
        g.trial_in_flight = false;
        if !was_closed {
            self.closed.notify_waiters();
        }
    }

    /// How long until the next trial is due; zero when it is due now, `None`
    /// while Closed.
    pub fn retry_after(&self) -> Option<Duration> {
        let g = self.lock();
        if g.state == CircuitState::Closed {
            return None;
        }
        let now = self.clock.now();
        Some(
            g.retry_at
                .map(|t| t.saturating_duration_since(now))
                .unwrap_or(Duration::ZERO),
        )
    }

    /// Single-flight trial slot (background mode). Returns `None` when the
    /// breaker is Closed or a trial is already in flight. Does NOT wait for
    /// `retry_at`: the caller owns the schedule ([`Circuit::retry_after`]).
    pub fn begin_trial(self: &Arc<Self>) -> Option<TrialPermit> {
        let mut g = self.lock();
        if g.state == CircuitState::Closed || g.trial_in_flight {
            return None;
        }
        g.trial_in_flight = true;
        self.set_state(&mut g, CircuitState::HalfOpen);
        g.consecutive_successes = 0;
        Some(TrialPermit {
            circuit: Arc::clone(self),
            reported: false,
        })
    }

    /// Report a trial. `Ok` counts as one success (closing at
    /// `success_threshold`); `Err` re-opens with the next backoff and keeps
    /// the original `since`.
    pub fn finish_trial(&self, mut permit: TrialPermit, result: Result<(), String>) {
        permit.reported = true;
        let mut g = self.lock();
        let now = self.clock.now();
        g.trial_in_flight = false;
        if g.state != CircuitState::HalfOpen {
            return;
        }
        match result {
            Ok(()) => {
                g.consecutive_successes += 1;
                if g.consecutive_successes >= self.config.success_threshold.max(1) {
                    self.close_locked(&mut g);
                }
            }
            Err(e) => self.reopen_locked(&mut g, now, e),
        }
    }

    pub fn snapshot(&self) -> CircuitSnapshot {
        let g = self.lock();
        self.snapshot_locked(&g, self.clock.now())
    }

    fn snapshot_locked(&self, g: &Inner, now: Instant) -> CircuitSnapshot {
        let open = g.state != CircuitState::Closed;
        CircuitSnapshot {
            state: g.state,
            since: if open { g.since_wall } else { None },
            cause: if open { g.cause.clone() } else { None },
            last_error: g.last_error.clone(),
            retry_after: if open {
                Some(
                    g.retry_at
                        .map(|t| t.saturating_duration_since(now))
                        .unwrap_or(Duration::ZERO),
                )
            } else {
                None
            },
            failed_trials: g.failed_trials,
            trips: g.trips,
            wait_timeouts: g.wait_timeouts,
            wait_timeout_last_at: g.wait_timeout_last_at,
            backend_errors: g.backend_errors,
            ambiguous_errors: g.ambiguous_errors,
        }
    }

    /// `since` as an `Instant`, for callers measuring outage length.
    pub fn since_instant(&self) -> Option<Instant> {
        self.lock().since
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg_config() -> CircuitConfig {
        CircuitConfig {
            failure_threshold: 1,
            success_threshold: 1,
            backoff: Backoff {
                initial: Duration::from_secs(1),
                factor: 2,
                cap: Duration::from_secs(15),
            },
            half_open_max_probes: 1,
            probe_mode: ProbeMode::Background,
        }
    }

    fn ai_config() -> CircuitConfig {
        CircuitConfig {
            failure_threshold: 4,
            success_threshold: 2,
            backoff: Backoff {
                initial: Duration::from_secs(60),
                factor: 1,
                cap: Duration::from_secs(60),
            },
            half_open_max_probes: 1,
            probe_mode: ProbeMode::CallerPermit,
        }
    }

    fn manual(config: CircuitConfig) -> (Arc<Circuit>, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::default());
        (Arc::new(Circuit::with_clock(config, clock.clone())), clock)
    }

    #[test]
    fn first_trip_failure_opens_the_breaker() {
        let (c, _) = manual(pg_config());
        assert_eq!(c.allow(), Admission::Pass);
        c.record(Outcome::TripFailure("connection refused".into()));
        assert_eq!(c.state(), CircuitState::Open);
        assert!(!c.is_closed());
        let s = c.snapshot();
        assert_eq!(s.cause.as_deref(), Some("runtime"));
        assert_eq!(s.last_error.as_deref(), Some("connection refused"));
        assert_eq!(s.trips, 1);
        assert!(s.since.is_some());
        assert!(matches!(c.allow(), Admission::Reject(_)));
    }

    #[test]
    fn non_trip_failures_never_open_it_even_a_thousand_times() {
        let (c, _) = manual(pg_config());
        for _ in 0..1000 {
            c.record(Outcome::NonTripFailure(NonTripKind::WaitTimeout));
            c.record(Outcome::NonTripFailure(NonTripKind::BackendError));
            c.record(Outcome::NonTripFailure(NonTripKind::Ambiguous));
        }
        assert_eq!(c.state(), CircuitState::Closed);
        let s = c.snapshot();
        assert_eq!(s.wait_timeouts, 1000);
        assert_eq!(s.backend_errors, 1000);
        assert_eq!(s.ambiguous_errors, 1000);
        assert!(s.wait_timeout_last_at.is_some());
        assert_eq!(s.trips, 0);
    }

    #[test]
    fn backoff_runs_1_2_4_8_15_15_seconds() {
        let b = pg_config().backoff;
        let got: Vec<u64> = (0..6).map(|n| b.delay(n).as_secs()).collect();
        assert_eq!(got, vec![1, 2, 4, 8, 15, 15]);
    }

    #[test]
    fn failed_trials_follow_the_backoff_and_keep_the_original_since() {
        let (c, clock) = manual(pg_config());
        c.record(Outcome::TripFailure("refused".into()));
        let since = c.snapshot().since;
        assert_eq!(c.retry_after(), Some(Duration::from_secs(1)));
        for expected in [2u64, 4, 8, 15, 15] {
            clock.advance(Duration::from_secs(3));
            let p = c.begin_trial().expect("a trial slot");
            c.finish_trial(p, Err("still refused".into()));
            assert_eq!(c.state(), CircuitState::Open);
            assert_eq!(c.retry_after(), Some(Duration::from_secs(expected)));
            assert_eq!(
                c.snapshot().since,
                since,
                "since must keep the original trip"
            );
        }
        assert_eq!(c.snapshot().failed_trials, 5);
        assert_eq!(c.snapshot().trips, 1, "a failed trial is not a new trip");
    }

    #[test]
    fn two_concurrent_trials_yield_exactly_one_permit() {
        let (c, _) = manual(pg_config());
        c.record(Outcome::TripFailure("refused".into()));
        let first = c.begin_trial();
        let second = c.begin_trial();
        assert!(first.is_some());
        assert!(second.is_none());
        drop(first);
        assert!(c.begin_trial().is_some(), "a dropped trial frees the slot");
    }

    #[test]
    fn a_successful_trial_closes_and_resets_the_backoff() {
        let (c, _) = manual(pg_config());
        c.record(Outcome::TripFailure("refused".into()));
        let p = c.begin_trial().unwrap();
        c.finish_trial(p, Err("refused".into()));
        let p = c.begin_trial().unwrap();
        c.finish_trial(p, Ok(()));
        assert_eq!(c.state(), CircuitState::Closed);
        let s = c.snapshot();
        assert_eq!(s.failed_trials, 0);
        assert_eq!(s.since, None);
        assert_eq!(s.retry_after, None);
        // Re-trip starts the schedule again at 1 s, with a new since.
        c.record(Outcome::TripFailure("refused again".into()));
        assert_eq!(c.retry_after(), Some(Duration::from_secs(1)));
        assert_eq!(c.snapshot().trips, 2);
    }

    #[test]
    fn background_mode_rejects_callers_while_half_open() {
        let (c, _) = manual(pg_config());
        c.record(Outcome::TripFailure("refused".into()));
        let _p = c.begin_trial().unwrap();
        assert_eq!(c.state(), CircuitState::HalfOpen);
        assert!(matches!(c.allow(), Admission::Reject(_)));
    }

    #[tokio::test]
    async fn opened_wakes_every_waiter_on_trip() {
        let c = Arc::new(Circuit::new(pg_config()));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let c2 = c.clone();
            handles.push(tokio::spawn(async move { c2.wait_opened().await }));
        }
        // Let every waiter park first, so the wake (not the fast return) is
        // what this exercises.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(handles.iter().all(|h| !h.is_finished()));
        c.record(Outcome::TripFailure("refused".into()));
        for h in handles {
            tokio::time::timeout(Duration::from_secs(1), h)
                .await
                .expect("every waiter is woken")
                .unwrap();
        }
    }

    #[test]
    fn force_open_names_its_cause() {
        let (c, _) = manual(pg_config());
        c.force_open("boot", "no database at boot");
        let s = c.snapshot();
        assert_eq!(s.state, CircuitState::Open);
        assert_eq!(s.cause.as_deref(), Some("boot"));
        assert_eq!(s.last_error.as_deref(), Some("no database at boot"));
    }

    /// The AI-provider configuration reproduces `ai_provider::circuit_breaker`'s
    /// documented transitions: 4 failures open it, it rejects for a fixed 60 s,
    /// then admits ONE caller as the probe, and 2 successes close it.
    #[test]
    fn ai_provider_configuration_reproduces_its_transitions() {
        let (c, clock) = manual(ai_config());
        for _ in 0..3 {
            c.record(Outcome::TripFailure("500".into()));
        }
        assert_eq!(c.state(), CircuitState::Closed, "3 < threshold 4");
        c.record(Outcome::Success);
        for _ in 0..3 {
            c.record(Outcome::TripFailure("500".into()));
        }
        assert_eq!(c.state(), CircuitState::Closed, "a success resets the run");
        c.record(Outcome::TripFailure("500".into()));
        assert_eq!(c.state(), CircuitState::Open);
        clock.advance(Duration::from_secs(59));
        assert!(matches!(c.allow(), Admission::Reject(_)));
        clock.advance(Duration::from_secs(1));
        assert_eq!(c.allow(), Admission::ProbePermit);
        assert_eq!(c.state(), CircuitState::HalfOpen);
        assert!(matches!(c.allow(), Admission::Reject(_)), "max 1 probe");
        c.record(Outcome::Success);
        assert_eq!(c.state(), CircuitState::HalfOpen, "1 of 2 successes");
        assert_eq!(c.allow(), Admission::ProbePermit);
        c.record(Outcome::Success);
        assert_eq!(c.state(), CircuitState::Closed);
        // A HalfOpen failure re-opens for another fixed 60 s.
        for _ in 0..4 {
            c.record(Outcome::TripFailure("500".into()));
        }
        clock.advance(Duration::from_secs(60));
        assert_eq!(c.allow(), Admission::ProbePermit);
        c.record(Outcome::TripFailure("500".into()));
        assert_eq!(c.state(), CircuitState::Open);
        assert_eq!(c.retry_after(), Some(Duration::from_secs(60)));
        // Outcomes recorded while Open are ignored.
        c.record(Outcome::Success);
        assert_eq!(c.state(), CircuitState::Open);
    }
}
