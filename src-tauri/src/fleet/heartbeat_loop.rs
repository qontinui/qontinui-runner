//! The periodic coord heartbeat loop — the timer, its timeout, and the log
//! lines that prove it is (or is not) running.
//!
//! Plan `2026-10-29-a-drained-device-is-alerted-as-no-runner-online`, Phase 3
//! (runner half). A coord gate-continuation alert claimed "no runner online"
//! for a device; reading the runner log could not settle it, because the
//! loop's success path logs only at `debug!`. A loop that ticked fine and a
//! loop that was hung on an await looked identical at the default level. This
//! module makes three things observable at default level:
//!
//! * **A hung tick.** Each tick's [`super::heartbeat_to_coord`] is bounded by
//!   [`tick_timeout`] (3 × interval). Expiry is a failed tick with cause
//!   `tick_timeout`: a `WARN` carrying the failure streak, and a miss folded
//!   into the drain state exactly as a transport error is.
//! * **A stalled loop.** A tick that STARTS more than one interval later than
//!   it could have — later than both its scheduled deadline and the end of the
//!   previous tick — means the timer or the runtime was starved (the 2026-06-03
//!   mode), and opens with one `WARN`. A slow or timed-out tick is NOT a stall:
//!   under `MissedTickBehavior::Skip` the next `tick()` returns the missed
//!   deadline immediately, so the lateness our own tick caused is excluded.
//! * **Proof of sending.** Every [`SUMMARY_WINDOW`] one `info!` line beginning
//!   `fleet::heartbeat: summary` reports ticks ok/failed, the widest
//!   start-to-start gap and the last coord status, then the window counters
//!   reset. Windows are anchored to the loop start, so they do not drift.
//!
//! The timing logic lives in [`HeartbeatLoopStats`] (pure, clock readings
//! passed in, tested with synthetic instants) and the loop itself in
//! [`run_heartbeat_loop`], which takes the tick's work and its sinks as
//! closures so tests drive the real loop on a paused tokio clock.
//!
//! The success path is otherwise unchanged: same interval, same request, same
//! handling of coord's response — `heartbeat_to_coord` itself stays in
//! `fleet.rs`, where the capability-assembly source pins look for it.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;
use tracing::{info, warn};

use crate::coord_drain_state::{DrainObservation, HeartbeatOutcome};

/// Default heartbeat cadence (seconds) when `COORD_HEARTBEAT_INTERVAL_SECS`
/// is unset or unparseable.
const DEFAULT_INTERVAL_SECS: u64 = 30;

/// A tick that has not finished within this many intervals is abandoned and
/// counted as failed (`tick_timeout`).
const TICK_TIMEOUT_INTERVALS: u32 = 3;

/// Cadence of the `fleet::heartbeat: summary` line.
pub(crate) const SUMMARY_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Heartbeat interval from `COORD_HEARTBEAT_INTERVAL_SECS` (default 30s,
/// floored at 1s).
fn heartbeat_interval() -> Duration {
    let secs: u64 = std::env::var("COORD_HEARTBEAT_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .max(1);
    Duration::from_secs(secs)
}

/// The bound on one tick's `heartbeat_to_coord` await: 3 × `interval`.
///
/// It bounds `heartbeat_to_coord` only: the drain-state fold after a success
/// (`note_heartbeat`) runs outside it, bounded by its own `ME_DRAIN_TIMEOUT`.
/// And it can only fire at an await point — a synchronous section on this
/// current_thread runtime cannot be preempted, which is why the blocking
/// probes run on the blocking pool.
pub(crate) fn tick_timeout(interval: Duration) -> Duration {
    interval * TICK_TIMEOUT_INTERVALS
}

/// Why a tick failed. `as_str` is the cause token written to the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureCause {
    /// `heartbeat_to_coord` returned `Err` (transport error, non-2xx, …).
    Error,
    /// `heartbeat_to_coord` did not complete within [`tick_timeout`].
    TickTimeout,
}

impl FailureCause {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            FailureCause::Error => "error",
            FailureCause::TickTimeout => "tick_timeout",
        }
    }
}

/// A failed tick: its cause and the message to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TickFailure {
    pub cause: FailureCause,
    pub message: String,
}

/// Run one tick's work under `limit`. An `Err` from the work is
/// [`FailureCause::Error`] carrying its message verbatim (so the existing WARN
/// text is unchanged); expiry is [`FailureCause::TickTimeout`].
pub(crate) async fn run_tick<T, F>(work: F, limit: Duration) -> Result<T, TickFailure>
where
    F: Future<Output = Result<T, String>>,
{
    match tokio::time::timeout(limit, work).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(message)) => Err(TickFailure {
            cause: FailureCause::Error,
            message,
        }),
        Err(_elapsed) => Err(TickFailure {
            cause: FailureCause::TickTimeout,
            message: format!(
                "tick_timeout: heartbeat_to_coord did not complete within {}s",
                limit.as_secs()
            ),
        }),
    }
}

/// The tick started more than one interval later than it could have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StallWarning {
    pub late: Duration,
    pub interval: Duration,
}

impl StallWarning {
    pub(crate) fn line(&self) -> String {
        format!(
            "fleet::heartbeat: loop stalled: tick started {:.1}s late (interval {}s)",
            self.late.as_secs_f64(),
            self.interval.as_secs()
        )
    }
}

/// One summary window's counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Summary {
    pub window: Duration,
    pub ticks_ok: u64,
    pub ticks_failed: u64,
    /// Widest tick-start-to-tick-start gap in the window.
    pub max_gap: Duration,
    pub last_status: String,
}

impl Summary {
    pub(crate) fn line(&self) -> String {
        format!(
            "fleet::heartbeat: summary window={}s ticks_ok={} ticks_failed={} \
             max_gap_secs={:.1} last_status={}",
            self.window.as_secs(),
            self.ticks_ok,
            self.ticks_failed,
            self.max_gap.as_secs_f64(),
            self.last_status
        )
    }
}

/// Pure loop bookkeeping: tick lateness and gaps, the failure streak and the
/// summary window. Every method takes the clock reading from the caller.
#[derive(Debug)]
pub(crate) struct HeartbeatLoopStats {
    interval: Duration,
    last_tick_start: Option<Instant>,
    last_tick_end: Option<Instant>,
    window_start: Instant,
    ticks_ok: u64,
    ticks_failed: u64,
    max_gap: Duration,
    /// The latest tick's status. Not a window counter: it survives a summary.
    last_status: Option<String>,
    consecutive_failures: u32,
}

impl HeartbeatLoopStats {
    pub(crate) fn new(interval: Duration, now: Instant) -> Self {
        Self {
            interval,
            last_tick_start: None,
            last_tick_end: None,
            window_start: now,
            ticks_ok: 0,
            ticks_failed: 0,
            max_gap: Duration::ZERO,
            last_status: None,
            consecutive_failures: 0,
        }
    }

    /// Record a tick start at `now` for the tick scheduled at `deadline` (what
    /// `Interval::tick` returned). Returns a warning when the tick started more
    /// than one interval after the later of `deadline` and the previous tick's
    /// end — i.e. lateness our own previous tick does not explain.
    pub(crate) fn on_tick_start(
        &mut self,
        now: Instant,
        deadline: Instant,
    ) -> Option<StallWarning> {
        if let Some(previous) = self.last_tick_start.replace(now) {
            self.max_gap = self.max_gap.max(now.saturating_duration_since(previous));
        }
        let could_start = self.last_tick_end.map_or(deadline, |end| end.max(deadline));
        let late = now.saturating_duration_since(could_start);
        (late > self.interval).then_some(StallWarning {
            late,
            interval: self.interval,
        })
    }

    /// Record the end of a tick (after its sinks ran).
    pub(crate) fn on_tick_end(&mut self, now: Instant) {
        self.last_tick_end = Some(now);
    }

    /// Record a successful tick with coord's status. Returns the length of the
    /// failure streak this tick ended, if any.
    pub(crate) fn record_ok(&mut self, status: String) -> Option<u32> {
        self.ticks_ok += 1;
        self.last_status = Some(status);
        let ended = std::mem::take(&mut self.consecutive_failures);
        (ended > 0).then_some(ended)
    }

    /// Record a failed tick. Returns the running failure streak.
    pub(crate) fn record_failed(&mut self, cause: FailureCause) -> u32 {
        self.ticks_failed += 1;
        self.last_status = Some(format!("failed:{}", cause.as_str()));
        self.consecutive_failures += 1;
        self.consecutive_failures
    }

    /// Once [`SUMMARY_WINDOW`] has elapsed since the window opened, return the
    /// window's summary and zero the counters. The next window opens at the
    /// last whole multiple of [`SUMMARY_WINDOW`] past the old one — anchored to
    /// the loop start, so a summary emitted at 630s still leaves the next due
    /// at 1200s, and a long suspend yields one summary, not a burst.
    pub(crate) fn maybe_summary(&mut self, now: Instant) -> Option<Summary> {
        let window = now.saturating_duration_since(self.window_start);
        if window < SUMMARY_WINDOW {
            return None;
        }
        let whole = u32::try_from(window.as_nanos() / SUMMARY_WINDOW.as_nanos()).unwrap_or(1);
        let summary = Summary {
            window,
            ticks_ok: self.ticks_ok,
            ticks_failed: self.ticks_failed,
            max_gap: self.max_gap,
            last_status: self.last_status.clone().unwrap_or_else(|| "none".into()),
        };
        self.window_start += SUMMARY_WINDOW * whole;
        self.ticks_ok = 0;
        self.ticks_failed = 0;
        self.max_gap = Duration::ZERO;
        Some(summary)
    }
}

/// The status token a successful tick reports: what coord's register response
/// said about this device's drain, or why no register was sent.
pub(crate) fn outcome_status(outcome: &HeartbeatOutcome) -> String {
    match outcome {
        HeartbeatOutcome::Registered { drain } => match drain {
            DrainObservation::Clear => "registered/clear".into(),
            DrainObservation::Drained { .. } => "registered/DRAINED".into(),
            DrainObservation::Unreadable => "registered/drain_unreadable".into(),
            DrainObservation::Absent { .. } => "registered/drain_absent".into(),
            DrainObservation::NotEnrolled { .. } => "registered/not_enrolled".into(),
        },
        HeartbeatOutcome::NotSent { .. } => "not_sent".into(),
        HeartbeatOutcome::NotEnrolled { .. } => "not_enrolled".into(),
    }
}

/// What the loop reports. Production writes each as one log line
/// ([`log_event`]); tests collect them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoopEvent {
    Stall(StallWarning),
    Failed { failure: TickFailure, streak: u32 },
    Recovered { after: u32 },
    Summary(Summary),
}

/// Production sink for [`LoopEvent`]s.
fn log_event(event: &LoopEvent) {
    match event {
        LoopEvent::Stall(stall) => warn!("{}", stall.line()),
        LoopEvent::Failed { failure, streak } => warn!(
            "fleet::heartbeat: {} (consecutive_failures={streak})",
            failure.message
        ),
        LoopEvent::Recovered { after } => {
            info!("fleet::heartbeat: recovered after {after} failed tick(s)")
        }
        LoopEvent::Summary(summary) => info!("{}", summary.line()),
    }
}

/// The heartbeat loop. Never returns.
///
/// `work` produces one tick's heartbeat (production: `heartbeat_to_coord`),
/// run under [`tick_timeout`]. `on_tick` marks the loop alive for the worker
/// supervisor; `on_failure` and `on_success` fold each tick's result into the
/// drain state; `emit` receives the observability events.
pub(crate) async fn run_heartbeat_loop<W, WF, OnTick, OnFail, OnOk, OkF, Emit>(
    interval: Duration,
    mut work: W,
    mut on_tick: OnTick,
    mut on_failure: OnFail,
    mut on_success: OnOk,
    mut emit: Emit,
) where
    W: FnMut() -> WF,
    WF: Future<Output = Result<HeartbeatOutcome, String>>,
    OnTick: FnMut(),
    OnFail: FnMut(&TickFailure),
    OnOk: FnMut(HeartbeatOutcome) -> OkF,
    OkF: Future<Output = ()>,
    Emit: FnMut(&LoopEvent),
{
    let limit = tick_timeout(interval);
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stats = HeartbeatLoopStats::new(interval, Instant::now());
    loop {
        let deadline = tick.tick().await;
        on_tick();
        if let Some(stall) = stats.on_tick_start(Instant::now(), deadline) {
            emit(&LoopEvent::Stall(stall));
        }
        match run_tick(work(), limit).await {
            Err(failure) => {
                let streak = stats.record_failed(failure.cause);
                emit(&LoopEvent::Failed {
                    failure: failure.clone(),
                    streak,
                });
                on_failure(&failure);
            }
            Ok(outcome) => {
                if let Some(after) = stats.record_ok(outcome_status(&outcome)) {
                    emit(&LoopEvent::Recovered { after });
                }
                on_success(outcome).await;
            }
        }
        let end = Instant::now();
        stats.on_tick_end(end);
        if let Some(summary) = stats.maybe_summary(end) {
            emit(&LoopEvent::Summary(summary));
        }
    }
}

/// Spawn the periodic heartbeat task on the ambient tokio runtime.
///
/// Interval is read from `COORD_HEARTBEAT_INTERVAL_SECS` (default 30s,
/// floored at 1s). `MissedTickBehavior::Skip`: if a tick is missed (e.g.
/// system suspend), skip catch-up and resume on the next aligned tick.
/// Heartbeat failures `warn!` and retry on the next interval; the loop never
/// panics.
pub fn spawn_heartbeat() {
    let interval = heartbeat_interval();

    info!(
        "fleet::heartbeat: starting periodic heartbeat task, interval={}s tick_timeout={}s",
        interval.as_secs(),
        tick_timeout(interval).as_secs()
    );

    // Supervised (plan 2026-09-03-…-supervisor Phase 4): a panic in a tick
    // used to kill this task silently and let coord's 120s liveness TTL
    // expire; the supervisor rebuilds the loop after a bounded backoff and
    // `/health` `supervised_workers` shows the restart. Per-run state lives
    // inside the future, so a rebuilt loop re-enters cleanly from the top.
    crate::worker_supervisor::spawn_supervised_with_heartbeat(
        "fleet.heartbeat",
        move |hb| async move {
            run_heartbeat_loop(
                interval,
                super::heartbeat_to_coord,
                move || hb.tick(),
                // Three of these in a row make the drain state Unknown.
                |failure: &TickFailure| {
                    crate::coord_drain_state::note_heartbeat_failure(&failure.message)
                },
                crate::coord_drain_state::note_heartbeat,
                log_event,
            )
            .await;
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const INTERVAL: Duration = Duration::from_secs(30);

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn clear() -> HeartbeatOutcome {
        HeartbeatOutcome::Registered {
            drain: DrainObservation::Clear,
        }
    }

    /// Paused tokio clock: a tick whose work blocks past the bound is a failed
    /// tick with cause `tick_timeout`, abandoned exactly at 3 × interval.
    #[tokio::test(start_paused = true)]
    async fn a_tick_blocked_past_the_timeout_fails_with_tick_timeout() {
        let limit = tick_timeout(INTERVAL);
        assert_eq!(limit, secs(90));
        let started = Instant::now();
        let result = run_tick(
            async {
                tokio::time::sleep(secs(10_000)).await;
                Ok::<HeartbeatOutcome, String>(clear())
            },
            limit,
        )
        .await;
        let failure = result.expect_err("a blocked tick must fail");
        assert_eq!(failure.cause, FailureCause::TickTimeout);
        assert_eq!(failure.cause.as_str(), "tick_timeout");
        assert_eq!(
            failure.message,
            "tick_timeout: heartbeat_to_coord did not complete within 90s"
        );
        assert_eq!(Instant::now() - started, secs(90));
    }

    /// Paused tokio clock: work that finishes inside the bound passes through
    /// unchanged — `Ok` as `Ok`, and `Err` as cause `error` with the message
    /// verbatim.
    #[tokio::test(start_paused = true)]
    async fn a_tick_inside_the_timeout_passes_through() {
        let limit = tick_timeout(INTERVAL);
        let ok = run_tick(
            async {
                tokio::time::sleep(secs(89)).await;
                Ok::<u8, String>(7)
            },
            limit,
        )
        .await;
        assert_eq!(ok, Ok(7));
        let err = run_tick(
            async { Err::<u8, String>("coord returned 503".into()) },
            limit,
        )
        .await;
        assert_eq!(
            err,
            Err(TickFailure {
                cause: FailureCause::Error,
                message: "coord returned 503".into(),
            })
        );
    }

    /// Synthetic instants: a slow tick (61s or 89s) and a timed-out tick (90s)
    /// do NOT make the next tick a stall, even though `Skip` hands that tick
    /// its long-missed deadline. Only lateness past the previous tick's end
    /// counts.
    #[test]
    fn a_slow_or_timed_out_tick_is_not_a_stall() {
        for tick_secs in [61, 89, 90] {
            let t0 = Instant::now();
            let mut stats = HeartbeatLoopStats::new(INTERVAL, t0);
            assert_eq!(stats.on_tick_start(t0, t0), None);
            stats.on_tick_end(t0 + secs(tick_secs));
            // Skip: the next tick returns at once with its missed deadline.
            let next = stats.on_tick_start(t0 + secs(tick_secs), t0 + INTERVAL);
            assert_eq!(next, None, "a {tick_secs}s tick is not a stall");
        }
    }

    /// Synthetic instants: a starved timer — the previous tick ended promptly,
    /// but this one started late against its own deadline — warns past one
    /// interval and not before. The line prints one decimal, so a 30.4s
    /// lateness does not read as "30s late" while exceeding 30s.
    #[test]
    fn a_starved_timer_is_a_stall() {
        let t0 = Instant::now();
        let mut stats = HeartbeatLoopStats::new(INTERVAL, t0);
        stats.on_tick_start(t0, t0);
        stats.on_tick_end(t0 + secs(1));

        let deadline = t0 + INTERVAL;
        let mut quiet = HeartbeatLoopStats::new(INTERVAL, t0);
        quiet.on_tick_start(t0, t0);
        quiet.on_tick_end(t0 + secs(1));
        assert_eq!(quiet.on_tick_start(deadline + secs(29), deadline), None);

        let warning = stats
            .on_tick_start(deadline + secs(31), deadline)
            .expect("31s late at a 30s interval");
        assert_eq!(warning.late, secs(31));
        assert_eq!(
            warning.line(),
            "fleet::heartbeat: loop stalled: tick started 31.0s late (interval 30s)"
        );

        let mut fractional = HeartbeatLoopStats::new(INTERVAL, t0);
        fractional.on_tick_start(t0, t0);
        fractional.on_tick_end(t0 + secs(1));
        let w = fractional
            .on_tick_start(deadline + Duration::from_millis(30_400), deadline)
            .expect("30.4s exceeds 30s");
        assert_eq!(
            w.line(),
            "fleet::heartbeat: loop stalled: tick started 30.4s late (interval 30s)"
        );
    }

    /// Synthetic instants: no summary before 10 minutes, one at 10 minutes,
    /// counters reset after it, the last status carries over, and windows stay
    /// anchored (a summary emitted at 630s leaves the next due at 1200s).
    #[test]
    fn the_summary_is_emitted_every_ten_minutes_and_resets_its_counters() {
        let t0 = Instant::now();
        let mut stats = HeartbeatLoopStats::new(INTERVAL, t0);
        stats.on_tick_start(t0, t0);
        stats.record_ok("registered/clear".into());
        stats.on_tick_start(t0 + secs(30), t0 + secs(30));
        stats.record_failed(FailureCause::TickTimeout);
        stats.on_tick_start(t0 + secs(120), t0 + secs(60));
        stats.record_ok("registered/DRAINED".into());

        assert_eq!(stats.maybe_summary(t0 + secs(599)), None);

        let summary = stats
            .maybe_summary(t0 + secs(630))
            .expect("past 10 minutes");
        assert_eq!(
            summary,
            Summary {
                window: secs(630),
                ticks_ok: 2,
                ticks_failed: 1,
                max_gap: secs(90),
                last_status: "registered/DRAINED".into(),
            }
        );
        assert_eq!(
            summary.line(),
            "fleet::heartbeat: summary window=630s ticks_ok=2 ticks_failed=1 \
             max_gap_secs=90.0 last_status=registered/DRAINED"
        );
        assert_eq!(stats.maybe_summary(t0 + secs(630)), None, "one per window");
        assert_eq!(
            stats.maybe_summary(t0 + secs(1199)),
            None,
            "the next window is anchored at 600s, not at the 630s emission"
        );

        let next = stats
            .maybe_summary(t0 + secs(1200))
            .expect("second window elapsed");
        assert_eq!(next.window, secs(600));
        assert_eq!(next.ticks_ok, 0);
        assert_eq!(next.ticks_failed, 0);
        assert_eq!(next.max_gap, Duration::ZERO);
        assert_eq!(next.last_status, "registered/DRAINED");
    }

    #[test]
    fn outcome_status_names_the_drain_state() {
        let drained = HeartbeatOutcome::Registered {
            drain: DrainObservation::Drained {
                until: None,
                reason: None,
            },
        };
        assert_eq!(outcome_status(&drained), "registered/DRAINED");
        assert_eq!(outcome_status(&clear()), "registered/clear");
        assert_eq!(
            outcome_status(&HeartbeatOutcome::NotSent { why: "x" }),
            "not_sent"
        );
    }

    /// Recording sinks for driving [`run_heartbeat_loop`].
    #[derive(Default)]
    struct Recorded {
        events: Vec<LoopEvent>,
        failures: Vec<TickFailure>,
        successes: usize,
        ticks: usize,
    }

    /// Paused tokio clock, the real loop: work that hangs on the second tick
    /// and succeeds otherwise. Over 610s that is exactly one `tick_timeout`
    /// failure, handed to the failure sink once; a recovery; NO stall (the
    /// tick after the timeout starts on its missed deadline); and one summary
    /// at 600s whose counters cover every tick.
    #[tokio::test(start_paused = true)]
    async fn the_loop_times_out_a_hung_tick_and_summarises_the_window() {
        let rec = Rc::new(RefCell::new(Recorded::default()));
        let calls = Rc::new(RefCell::new(0usize));
        let (r1, r2, r3, r4) = (rec.clone(), rec.clone(), rec.clone(), rec.clone());
        let loop_fut = run_heartbeat_loop(
            INTERVAL,
            move || {
                let n = {
                    let mut c = calls.borrow_mut();
                    *c += 1;
                    *c
                };
                async move {
                    if n == 2 {
                        std::future::pending::<()>().await;
                    }
                    Ok(clear())
                }
            },
            move || r1.borrow_mut().ticks += 1,
            move |f: &TickFailure| r2.borrow_mut().failures.push(f.clone()),
            move |_o: HeartbeatOutcome| {
                r3.borrow_mut().successes += 1;
                async {}
            },
            move |e: &LoopEvent| r4.borrow_mut().events.push(e.clone()),
        );
        let started = Instant::now();
        assert!(tokio::time::timeout(secs(610), loop_fut).await.is_err());
        assert_eq!(Instant::now() - started, secs(610));

        let rec = rec.borrow();
        // Ticks: 0 ok, 30 hangs until 120, 120 (missed deadline 60) ok, then
        // 150, 180, …, 600 ok — 18 successes, 19 ticks.
        assert_eq!(rec.ticks, 19);
        assert_eq!(rec.successes, 18);
        assert_eq!(rec.failures.len(), 1, "failure sink called once");
        assert_eq!(rec.failures[0].cause, FailureCause::TickTimeout);

        let timeout = TickFailure {
            cause: FailureCause::TickTimeout,
            message: "tick_timeout: heartbeat_to_coord did not complete within 90s".into(),
        };
        assert_eq!(
            rec.events,
            vec![
                LoopEvent::Failed {
                    failure: timeout,
                    streak: 1
                },
                LoopEvent::Recovered { after: 1 },
                LoopEvent::Summary(Summary {
                    window: secs(600),
                    ticks_ok: 18,
                    ticks_failed: 1,
                    max_gap: secs(90),
                    last_status: "registered/clear".into(),
                }),
            ]
        );
    }

    /// Paused tokio clock, the real loop: the clock advances 70s while the
    /// loop is not polled — a starved runtime. The next tick starts 41s past
    /// its deadline (60s) and emits exactly one stall.
    #[tokio::test(start_paused = true)]
    async fn the_loop_warns_when_its_runtime_is_starved() {
        let events = Rc::new(RefCell::new(Vec::<LoopEvent>::new()));
        let sink = events.clone();
        let loop_fut = run_heartbeat_loop(
            INTERVAL,
            || async { Ok(clear()) },
            || {},
            |_f: &TickFailure| {},
            |_o: HeartbeatOutcome| async {},
            move |e: &LoopEvent| sink.borrow_mut().push(e.clone()),
        );
        tokio::pin!(loop_fut);
        // Ticks at 0 and 30, then the loop parks on the 60s deadline.
        assert!(tokio::time::timeout(secs(31), &mut loop_fut).await.is_err());
        assert!(events.borrow().is_empty());
        // Starve it: time passes with nobody polling the loop.
        tokio::time::advance(secs(70)).await;
        assert!(tokio::time::timeout(secs(1), &mut loop_fut).await.is_err());
        assert_eq!(
            *events.borrow(),
            vec![LoopEvent::Stall(StallWarning {
                late: secs(41),
                interval: INTERVAL,
            })]
        );
        assert_eq!(
            StallWarning {
                late: secs(41),
                interval: INTERVAL
            }
            .line(),
            "fleet::heartbeat: loop stalled: tick started 41.0s late (interval 30s)"
        );
    }
}
