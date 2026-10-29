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
//! * **A stalled loop.** The gap between consecutive tick starts is measured;
//!   above 2 × interval the tick opens with one `WARN` naming the gap.
//! * **Proof of sending.** Every [`SUMMARY_WINDOW`] one `info!` line beginning
//!   `fleet::heartbeat: summary` reports ticks ok/failed, the widest gap and
//!   the last coord status, then the window counters reset.
//!
//! The timing logic lives in [`HeartbeatLoopStats`] (pure, clock passed in)
//! and [`run_tick`] (the timeout), so tests drive them with a paused tokio
//! clock instead of waiting real minutes.
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

/// A gap between tick starts above this many intervals is a stall.
const STALL_INTERVALS: u32 = 2;

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

/// The tick started more than 2 × interval after the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StallWarning {
    pub gap: Duration,
    pub interval: Duration,
}

impl StallWarning {
    pub(crate) fn line(&self) -> String {
        format!(
            "fleet::heartbeat: loop stalled {}s (expected {}s)",
            self.gap.as_secs(),
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
    pub max_gap_secs: u64,
    pub last_status: String,
}

impl Summary {
    pub(crate) fn line(&self) -> String {
        format!(
            "fleet::heartbeat: summary window={}s ticks_ok={} ticks_failed={} \
             max_gap_secs={} last_status={}",
            self.window.as_secs(),
            self.ticks_ok,
            self.ticks_failed,
            self.max_gap_secs,
            self.last_status
        )
    }
}

/// Pure loop bookkeeping: tick gaps, the failure streak and the summary
/// window. Every method takes the clock reading from the caller.
#[derive(Debug)]
pub(crate) struct HeartbeatLoopStats {
    interval: Duration,
    last_tick_start: Option<Instant>,
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
            window_start: now,
            ticks_ok: 0,
            ticks_failed: 0,
            max_gap: Duration::ZERO,
            last_status: None,
            consecutive_failures: 0,
        }
    }

    /// Record a tick start. Returns a warning when the gap since the previous
    /// tick start exceeds 2 × interval (never on the first tick).
    pub(crate) fn on_tick_start(&mut self, now: Instant) -> Option<StallWarning> {
        let previous = self.last_tick_start.replace(now)?;
        let gap = now.saturating_duration_since(previous);
        self.max_gap = self.max_gap.max(gap);
        (gap > self.interval * STALL_INTERVALS).then_some(StallWarning {
            gap,
            interval: self.interval,
        })
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
    /// window's summary and open a new window with zeroed counters.
    pub(crate) fn maybe_summary(&mut self, now: Instant) -> Option<Summary> {
        let window = now.saturating_duration_since(self.window_start);
        if window < SUMMARY_WINDOW {
            return None;
        }
        let summary = Summary {
            window,
            ticks_ok: self.ticks_ok,
            ticks_failed: self.ticks_failed,
            max_gap_secs: self.max_gap.as_secs(),
            last_status: self.last_status.clone().unwrap_or_else(|| "none".into()),
        };
        self.window_start = now;
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

/// Spawn the periodic heartbeat task on the ambient tokio runtime.
///
/// Interval is read from `COORD_HEARTBEAT_INTERVAL_SECS` (default 30s,
/// floored at 1s). `MissedTickBehavior::Skip` mirrors the watcher in
/// `qontinui-coord/src/health_watcher.rs:99-100` — if a tick is missed
/// (e.g. system suspend), skip catch-up and resume on the next aligned
/// tick. Heartbeat failures `warn!` and retry on the next interval;
/// the loop never panics.
pub fn spawn_heartbeat() {
    let interval = heartbeat_interval();
    let limit = tick_timeout(interval);

    info!(
        "fleet::heartbeat: starting periodic heartbeat task, interval={}s tick_timeout={}s",
        interval.as_secs(),
        limit.as_secs()
    );

    // Supervised (plan 2026-09-03-…-supervisor Phase 4): a panic in a tick
    // used to kill this task silently and let coord's 120s liveness TTL
    // expire; the supervisor rebuilds the loop after a bounded backoff and
    // `/health` `supervised_workers` shows the restart. Per-run state lives
    // inside the future, so a rebuilt loop re-enters cleanly from the top.
    crate::worker_supervisor::spawn_supervised_with_heartbeat(
        "fleet.heartbeat",
        move |hb| async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Each WARN carries the running failure streak (so log timestamps
            // reveal whether ticks are actually firing), recovery logs once at
            // info, and the periodic summary proves ticks were SENT — the
            // success path is otherwise debug-quiet, which hid the 2026-06-03
            // outage's intermittency from the default log level.
            let mut stats = HeartbeatLoopStats::new(interval, Instant::now());
            loop {
                tick.tick().await;
                hb.tick();
                if let Some(stall) = stats.on_tick_start(Instant::now()) {
                    warn!("{}", stall.line());
                }
                match run_tick(super::heartbeat_to_coord(), limit).await {
                    Err(failure) => {
                        let streak = stats.record_failed(failure.cause);
                        warn!(
                            "fleet::heartbeat: {} (consecutive_failures={streak})",
                            failure.message
                        );
                        // Three of these in a row make the drain state Unknown.
                        crate::coord_drain_state::note_heartbeat_failure(&failure.message);
                    }
                    Ok(outcome) => {
                        if let Some(ended) = stats.record_ok(outcome_status(&outcome)) {
                            info!("fleet::heartbeat: recovered after {ended} failed tick(s)");
                        }
                        crate::coord_drain_state::note_heartbeat(outcome).await;
                    }
                }
                if let Some(summary) = stats.maybe_summary(Instant::now()) {
                    info!("{}", summary.line());
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_secs(30);

    /// A tick whose work blocks past the bound is a failed tick with cause
    /// `tick_timeout`, abandoned exactly at 3 × interval.
    #[tokio::test(start_paused = true)]
    async fn a_tick_blocked_past_the_timeout_fails_with_tick_timeout() {
        let limit = tick_timeout(INTERVAL);
        assert_eq!(limit, Duration::from_secs(90));
        let started = Instant::now();
        let result = run_tick(
            async {
                tokio::time::sleep(Duration::from_secs(10_000)).await;
                Ok::<HeartbeatOutcome, String>(HeartbeatOutcome::NotSent { why: "never" })
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
        assert_eq!(Instant::now() - started, Duration::from_secs(90));

        // It counts toward the failure streak and the window like any failure.
        let mut stats = HeartbeatLoopStats::new(INTERVAL, started);
        assert_eq!(stats.record_failed(failure.cause), 1);
        assert_eq!(stats.record_failed(FailureCause::Error), 2);
        assert_eq!(stats.record_ok("registered/clear".into()), Some(2));
        let s = stats
            .maybe_summary(started + SUMMARY_WINDOW)
            .expect("window elapsed");
        assert_eq!(s.ticks_failed, 2);
        assert_eq!(s.ticks_ok, 1);
    }

    /// Work that finishes inside the bound passes through unchanged: `Ok` as
    /// `Ok`, and `Err` as cause `error` with the message verbatim.
    #[tokio::test(start_paused = true)]
    async fn a_tick_inside_the_timeout_passes_through() {
        let limit = tick_timeout(INTERVAL);
        let ok = run_tick(
            async {
                tokio::time::sleep(Duration::from_secs(89)).await;
                Ok::<u8, String>(7)
            },
            limit,
        )
        .await;
        assert_eq!(ok, Ok(7));
        let err = run_tick(async { Err::<u8, String>("coord returned 503".into()) }, limit).await;
        assert_eq!(
            err,
            Err(TickFailure {
                cause: FailureCause::Error,
                message: "coord returned 503".into(),
            })
        );
    }

    /// A stall warning fires at 2 × interval + 1s and not at 2 × interval − 1s.
    #[test]
    fn a_stall_is_warned_only_past_twice_the_interval() {
        let t0 = Instant::now();
        let mut stats = HeartbeatLoopStats::new(INTERVAL, t0);
        assert_eq!(stats.on_tick_start(t0), None, "the first tick has no gap");

        let t1 = t0 + Duration::from_secs(59);
        assert_eq!(stats.on_tick_start(t1), None);

        let t2 = t1 + Duration::from_secs(61);
        let warning = stats.on_tick_start(t2).expect("61s gap at a 30s interval");
        assert_eq!(warning.gap, Duration::from_secs(61));
        assert_eq!(
            warning.line(),
            "fleet::heartbeat: loop stalled 61s (expected 30s)"
        );

        let summary = stats
            .maybe_summary(t0 + SUMMARY_WINDOW)
            .expect("window elapsed");
        assert_eq!(summary.max_gap_secs, 61);
    }

    /// No summary before 10 minutes, exactly one at 10 minutes, and the window
    /// counters reset after it while the last status carries over.
    #[test]
    fn the_summary_is_emitted_every_ten_minutes_and_resets_its_counters() {
        let t0 = Instant::now();
        let mut stats = HeartbeatLoopStats::new(INTERVAL, t0);
        stats.on_tick_start(t0);
        stats.record_ok("registered/clear".into());
        stats.on_tick_start(t0 + Duration::from_secs(30));
        stats.record_failed(FailureCause::TickTimeout);
        stats.on_tick_start(t0 + Duration::from_secs(60));
        stats.record_ok("registered/DRAINED".into());

        assert_eq!(stats.maybe_summary(t0 + Duration::from_secs(599)), None);

        let at = t0 + SUMMARY_WINDOW;
        let summary = stats.maybe_summary(at).expect("10 minutes elapsed");
        assert_eq!(
            summary,
            Summary {
                window: Duration::from_secs(600),
                ticks_ok: 2,
                ticks_failed: 1,
                max_gap_secs: 30,
                last_status: "registered/DRAINED".into(),
            }
        );
        assert_eq!(
            summary.line(),
            "fleet::heartbeat: summary window=600s ticks_ok=2 ticks_failed=1 \
             max_gap_secs=30 last_status=registered/DRAINED"
        );
        assert_eq!(stats.maybe_summary(at), None, "one summary per window");
        assert_eq!(
            stats.maybe_summary(at + Duration::from_secs(599)),
            None,
            "the next window opens at the summary"
        );

        let next = stats
            .maybe_summary(at + SUMMARY_WINDOW)
            .expect("second window elapsed");
        assert_eq!(next.ticks_ok, 0);
        assert_eq!(next.ticks_failed, 0);
        assert_eq!(next.max_gap_secs, 0);
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
        let clear = HeartbeatOutcome::Registered {
            drain: DrainObservation::Clear,
        };
        assert_eq!(outcome_status(&clear), "registered/clear");
        assert_eq!(
            outcome_status(&HeartbeatOutcome::NotSent { why: "x" }),
            "not_sent"
        );
    }
}
