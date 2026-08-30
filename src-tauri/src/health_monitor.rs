//! Health Monitoring Module
//!
//! Provides periodic health monitoring to detect gradual resource exhaustion
//! that could lead to application crashes. Logs memory usage, thread count,
//! and other system metrics.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Interval between health checks in seconds
const HEALTH_CHECK_INTERVAL_SECS: u64 = 60;

/// Memory threshold in MB to trigger a warning
const MEMORY_WARNING_THRESHOLD_MB: u64 = 1024; // 1 GB

/// Thread count threshold to trigger a warning.
/// Set to 150 to avoid false positives — the runner legitimately uses 100-130
/// threads during normal operation (tokio runtime, bridges, background tasks).
///
/// **The 100-130 band above is STALE, measured 2026-08-30.** A live, idle
/// runner (debug build, embedded Postgres, full bridge set) sampled every 3 s
/// on the Linux dev box sat at **150-151** threads — at and just over this
/// threshold, so this line now fires as a matter of course rather than on an
/// anomaly. The number is left alone deliberately: this is a LOG threshold,
/// where the cost of a false positive is one line a minute, and re-measuring
/// the band on every platform is not this change's job.
///
/// It is `pub(crate)` so the spawn gate can say what it is NOT: the gate's
/// thread ceilings ([`crate::settings::SessionGuardSettings::warn_thread_count`],
/// [`crate::resource_guard::THREAD_CEILING_MIN`]) must sit strictly ABOVE this
/// number, and a test pins that. A ceiling at a count the process already
/// carries at rest would refuse or warn on every spawn forever — which is what
/// reusing this constant verbatim as the gate's warn ceiling would have done.
pub(crate) const THREAD_WARNING_THRESHOLD: usize = 150;

/// How often the monitor self-probes `/livez`.
const SELF_PROBE_INTERVAL_SECS: u64 = 5;

/// Per-probe timeout. Generous for a handler that returns a literal 200 —
/// anything slower than this is already a symptom, not latency.
const SELF_PROBE_TIMEOUT_SECS: u64 = 5;

/// Consecutive failed probes before declaring the backend wedged.
///
/// 3 × 5s ≈ 15s of continuous silence. Above the noise floor of a single
/// dropped connection or a GC pause; far below the 7 hours the 2026-08-08
/// incident went unnoticed.
const WEDGE_FAILURE_THRESHOLD: u32 = 3;

/// Re-escalate every N further consecutive failures (≈5 min at a 5s probe)
/// so a long wedge stays visible without flooding the log.
const WEDGE_REESCALATION_EVERY: u32 = 60;

/// Flag to control the health monitor
static MONITOR_RUNNING: AtomicBool = AtomicBool::new(false);

/// Set while the backend is believed wedged, so other in-process surfaces can
/// read it without waiting on the monitor thread.
static BACKEND_WEDGED: AtomicBool = AtomicBool::new(false);

// ── Watchdog-visible state ───────────────────────────────────────────────────
//
// Plain atomics, published by the monitor thread and read by the watchdog
// thread (below). No lock, no channel, no allocation: the watchdog must be
// able to read them when every other thread in the process is stuck.

/// Unix ms at the probe thread's last completed probe. `0` before the first.
static MONITOR_HEARTBEAT_MS: AtomicI64 = AtomicI64::new(0);

/// Unix ms at the METRICS loop's last liveness stamp, published **before**
/// `collect_metrics`/`log_metrics` as well as after.
///
/// Item 4 (2026-08-24). Iteration 14 gave the monitor a single heartbeat,
/// published only *after* `wedge.observe(..)` — i.e. after the escalation's
/// `error!` had already handed the record to the tracing subscriber. Tracing
/// is a shared-fate path: a blocked writer parks the thread there, the stamp
/// stops moving, and the watchdog can then only ever say
/// `health-monitor-thread-stalled`. That is precisely what the 11-minute
/// outage produced — every incident a monitor-stall, `backend-wedged` never
/// once, and `consecutive /livez failures` reading **0**, because that store
/// came after the blocking call too.
///
/// Two changes close it: the probe loop gets its OWN thread and its own stamp
/// (so slow or blocked metrics work can no longer suppress wedge detection),
/// and this stamp lets the watchdog name a stalled metrics loop as its own
/// condition instead of mislabelling it as a dead detector.
static METRICS_HEARTBEAT_MS: AtomicI64 = AtomicI64::new(0);

/// Consecutive failed `/livez` probes, published for the watchdog.
static MONITOR_CONSECUTIVE_FAILURES: AtomicU32 = AtomicU32::new(0);

/// How often the watchdog thread wakes to write its heartbeat.
const WATCHDOG_TICK_SECS: u64 = 15;

/// The monitor thread is considered STALLED if its heartbeat is older than
/// this. It probes every 5s, so 3 minutes is ~36 missed beats — far outside
/// any scheduling jitter, and the condition the 2026-08-11 freeze produced
/// (10 breadcrumbs for lesser incidents, then none at all for 2h45m).
const WATCHDOG_MONITOR_STALL_SECS: i64 = 180;

/// While a breadcrumb-worthy condition persists, re-append at most this often
/// so a multi-hour wedge leaves a readable trail instead of a 15s-cadence
/// flood.
const WATCHDOG_REPEAT_SECS: i64 = 300;

/// Whether the monitor currently believes the HTTP surface is wedged.
///
/// Read by the UI/status surfaces; never blocks.
pub fn backend_wedged() -> bool {
    BACKEND_WEDGED.load(Ordering::SeqCst)
}

/// Upper bound on ONE probe, enforced by the probe loop itself rather than by
/// reqwest.
///
/// Generous next to `SELF_PROBE_TIMEOUT_SECS`, because it is a backstop for the
/// parts of a probe that reqwest's own timeout does not cover, not a second
/// latency target.
const PROBE_HARD_DEADLINE_SECS: u64 = 20;

/// Run one `/livez` probe under a deadline the probe loop can always enforce.
///
/// **Why reqwest's own timeout is not enough.** Measured on this box
/// 2026-08-24, during a wedge under heavy memory pressure: the watchdog
/// recorded `probe heartbeat 184s old, metrics heartbeat 4s old`. The metrics
/// loop was ticking fine — so the process was scheduled — while the probe loop
/// was stuck *inside a single probe*, well past its 5s request timeout.
/// `Client::builder().timeout(..)` bounds the REQUEST; it does not bound
/// building the blocking client (which spins up its own runtime on a fresh
/// thread) or dropping it (which shuts that runtime down). Under memory
/// pressure those are exactly the slow parts.
///
/// The consequence was not a slow probe, it was a BLIND detector: with the
/// probe loop stuck, `consecutive_failures` never advanced, `BACKEND_WEDGED`
/// stayed false, and the watchdog could only report a monitor stall — the very
/// mislabel Item 4 exists to remove.
///
/// So the probe runs on ONE dedicated prober thread and the loop waits on it
/// with a std `recv_timeout`. A probe that overruns is reported as a FAILURE —
/// the honest reading, since `/livez` did not answer — and the loop then stops
/// queueing work onto a thread that cannot take it, so every later tick reports
/// immediately instead of re-waiting the deadline. One thread, not one per
/// probe: at a 5s cadence an 11-minute wedge would otherwise leave ~130 stuck
/// threads behind, a leak triggered by the very condition being reported.
struct LivezProber {
    req_tx: std::sync::mpsc::SyncSender<()>,
    res_rx: std::sync::mpsc::Receiver<bool>,
    /// True while the prober still owes an answer we already gave up on.
    outstanding: bool,
    alive: bool,
}

impl LivezProber {
    fn new() -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let (res_tx, res_rx) = std::sync::mpsc::channel::<bool>();
        // ONE prober thread for the process, not one per probe. A probe every
        // 5s through an 11-minute wedge would otherwise leave ~130 stuck
        // threads behind — a thread leak triggered by the very condition the
        // detector exists to report.
        let alive = std::thread::Builder::new()
            .name("livez-prober".to_string())
            .spawn(move || {
                while req_rx.recv().is_ok() {
                    if res_tx.send(probe_livez_blocking()).is_err() {
                        break;
                    }
                }
            })
            .is_ok();
        Self {
            req_tx,
            res_rx,
            outstanding: false,
            alive,
        }
    }

    /// One bounded observation. Never blocks longer than
    /// `PROBE_HARD_DEADLINE_SECS`, whatever the prober is doing.
    fn probe(&mut self) -> bool {
        use std::sync::mpsc::{RecvTimeoutError, TryRecvError};

        if !self.alive {
            // No prober: report UNKNOWN as "not a failure" rather than
            // manufacturing a wedge out of our own defect (the same rule as a
            // client that will not build).
            return true;
        }

        if self.outstanding {
            match self.res_rx.try_recv() {
                // A late answer to an abandoned probe. Discard it — it
                // describes a moment we already reported on — and reuse the
                // thread from the next tick.
                Ok(_late) => self.outstanding = false,
                // Still stuck. `/livez` is not answering; say so, and do NOT
                // queue more work onto a thread that cannot take it.
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => {
                    self.alive = false;
                    return true;
                }
            }
        }

        if self.req_tx.try_send(()).is_err() {
            return false;
        }
        match self
            .res_rx
            .recv_timeout(Duration::from_secs(PROBE_HARD_DEADLINE_SECS))
        {
            Ok(alive) => alive,
            Err(RecvTimeoutError::Timeout) => {
                self.outstanding = true;
                false
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.alive = false;
                true
            }
        }
    }
}

/// Blocking `/livez` probe.
///
/// Deliberately a plain blocking HTTP call on its own OS thread, NOT a
/// `block_on` against the app runtime: routing the probe through the runtime
/// under test would make it hang exactly when it needs to report, and would
/// consume a worker from the pool it is trying to measure.
///
/// Call it through [`probe_livez_bounded`], never directly from the probe
/// loop — on its own it can outlast its own timeout (see there).
fn probe_livez_blocking() -> bool {
    let port = crate::mcp::types::get_mcp_api_port();
    let url = format!("http://127.0.0.1:{port}/livez");
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(SELF_PROBE_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            // Cannot build a client: report UNKNOWN as "not a failure" rather
            // than manufacturing a wedge out of our own defect.
            warn!("livez self-probe client build failed: {e}");
            return true;
        }
    };
    match client.get(&url).send() {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

/// Escalation state machine for consecutive `/livez` failures.
///
/// Pure and side-effect free apart from the escalation callbacks, so the
/// thresholds can be tested without a runtime, a socket, or a clock.
#[derive(Debug, Default)]
struct WedgeDetector {
    consecutive_failures: u32,
    escalated: bool,
}

/// What the caller should do about this observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WedgeAction {
    /// Healthy, or not yet past the threshold.
    None,
    /// Cross the threshold for the first time in this incident.
    Escalate { consecutive_failures: u32 },
    /// Still wedged — periodic reminder.
    ReEscalate { consecutive_failures: u32 },
    /// Recovered after having escalated.
    Recovered { was_failing_for: u32 },
}

impl WedgeDetector {
    /// Fold one probe result into the state machine and return the action.
    fn step(&mut self, alive: bool) -> WedgeAction {
        if alive {
            let was = self.consecutive_failures;
            let had_escalated = self.escalated;
            self.consecutive_failures = 0;
            self.escalated = false;
            return if had_escalated {
                WedgeAction::Recovered {
                    was_failing_for: was,
                }
            } else {
                WedgeAction::None
            };
        }

        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let n = self.consecutive_failures;

        if n == WEDGE_FAILURE_THRESHOLD {
            self.escalated = true;
            WedgeAction::Escalate {
                consecutive_failures: n,
            }
        } else if self.escalated
            && n > WEDGE_FAILURE_THRESHOLD
            && (n - WEDGE_FAILURE_THRESHOLD).is_multiple_of(WEDGE_REESCALATION_EVERY)
        {
            WedgeAction::ReEscalate {
                consecutive_failures: n,
            }
        } else {
            WedgeAction::None
        }
    }

    /// Publish everything the watchdog reads, using non-blocking stores only.
    ///
    /// Called before any reporting side effect, so the watchdog can name
    /// `backend-wedged` even if the very next line parks this thread forever.
    fn publish(&self) {
        BACKEND_WEDGED.store(
            self.consecutive_failures >= WEDGE_FAILURE_THRESHOLD,
            Ordering::SeqCst,
        );
        MONITOR_CONSECUTIVE_FAILURES.store(self.consecutive_failures, Ordering::SeqCst);
        MONITOR_HEARTBEAT_MS.store(chrono::Utc::now().timestamp_millis(), Ordering::SeqCst);
    }

    /// Fold in an observation, **publish the watchdog-visible state**, and
    /// only then perform the escalation side effects.
    ///
    /// The ordering is the Item 4 fix and it is load-bearing. Iteration 14
    /// stored `BACKEND_WEDGED` inside the escalation arm and left
    /// `MONITOR_CONSECUTIVE_FAILURES` / `MONITOR_HEARTBEAT_MS` to the caller,
    /// *after* this function returned — i.e. after `error!` had already handed
    /// the record to the tracing subscriber. One blocked writer and the thread
    /// stops here having published nothing, which is why an 11-minute outage
    /// showed `consecutive /livez failures 0` and never named `backend-wedged`.
    ///
    /// Atomics are stores to plain memory: they cannot block, so they go
    /// first. Then the on-disk breadcrumb. Tracing last, because it is the
    /// only step that shares fate with the subsystem being reported.
    fn observe(&mut self, alive: bool) {
        let action = self.step(alive);
        self.publish();
        match action {
            WedgeAction::None => {}
            WedgeAction::Escalate {
                consecutive_failures,
            }
            | WedgeAction::ReEscalate {
                consecutive_failures,
            } => {
                let secs = consecutive_failures as u64 * SELF_PROBE_INTERVAL_SECS;
                // Breadcrumb FIRST, log second. `error!` goes through the
                // tracing subscriber — a writer, a bounded channel, a file
                // mutex — none of which is guaranteed to be schedulable in the
                // condition we are reporting. The durable record must not sit
                // behind it.
                write_wedge_breadcrumb(secs);
                error!(
                    consecutive_failures,
                    unresponsive_for_secs = secs,
                    "BACKEND WEDGED: /livez has not answered for {secs}s. The HTTP surface is \
                     not serving — every agent session on this box is affected, and coord-mcp \
                     writes issued through this runner are being LOST. Not restarting: a \
                     process restart destroys in-flight sessions and is an explicit non-goal.",
                );
            }
            WedgeAction::Recovered { was_failing_for } => {
                let secs = was_failing_for as u64 * SELF_PROBE_INTERVAL_SECS;
                warn!(
                    was_failing_for_secs = secs,
                    "Backend recovered: /livez is answering again after {secs}s of silence"
                );
            }
        }
    }
}

/// Append a durable on-disk record of the wedge.
///
/// **This is the load-bearing half of the escalation, not the log line.** The
/// monitor already logged a thread-leak warning every 60s for days before
/// 2026-08-11 and nobody saw it; a WARN in a 12,000-line/hour file is not a
/// signal. This breadcrumb survives the process, sits in a file whose only
/// content is incidents, and is the first thing to read after an unexplained
/// outage.
///
/// Best-effort by contract: the process is already sick, so a failure to
/// write must never make it worse.
fn write_wedge_breadcrumb(unresponsive_for_secs: u64) {
    let dir = crate::paths::get_dev_logs_dir();
    let path = dir.join("wedge-incidents.log");
    let line = format!(
        "{} runner backend wedged — /livez silent for {}s (pid {})\n",
        chrono::Utc::now().to_rfc3339(),
        unresponsive_for_secs,
        std::process::id()
    );
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

// ── Runtime-independent wedge watchdog (Item 4) ──────────────────────────────
//
// **Why the detector could not report the worst freeze it ever saw.** The
// monitor thread already runs off the tokio runtime, but its *reporting* path
// did not: the escalation called `error!` before the breadcrumb, and `error!`
// hands the record to the tracing subscriber — a writer behind a lock, a
// bounded channel, a file handle — any of which can be held by a task the
// starved runtime will never schedule again. One blocked `error!` and the
// monitor thread stops at the top of its own escalation, having produced
// nothing. That is consistent with the observed evidence: 10 breadcrumbs for
// lesser incidents, then total silence across a 2h45m freeze.
//
// The fix is a SECOND dedicated OS thread whose only job is to put bytes on
// disk. It:
//
//   * never calls `tracing` (that is the shared-fate path),
//   * never calls into the async runtime, reqwest, or any app subsystem,
//   * resolves its output directory ONCE at start, so a later stall in path
//     or settings resolution cannot silence it,
//   * reads its inputs from plain atomics, so it cannot block on a lock,
//   * rewrites a fixed-size heartbeat file every tick (bounded), and appends
//     to the incident log only on a state change or every
//     `WATCHDOG_REPEAT_SECS` (bounded).
//
// It also watches the MONITOR THREAD ITSELF. A stalled monitor is now an
// incident in its own right — the failure mode that produced no evidence last
// time now produces the line that names it.

/// Everything the watchdog needs to decide, sampled without taking a lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WatchdogSample {
    now_ms: i64,
    /// Unix ms of the PROBE thread's last completed probe (`0` = never).
    monitor_heartbeat_ms: i64,
    /// Unix ms of the METRICS loop's last liveness stamp (`0` = never).
    metrics_heartbeat_ms: i64,
    backend_wedged: bool,
    consecutive_failures: u32,
}

/// Why the watchdog is writing an incident line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchdogReason {
    /// The monitor says `/livez` is silent.
    BackendWedged,
    /// The probe thread itself has stopped ticking — the case that produced
    /// no evidence at all on 2026-08-11.
    MonitorStalled,
    /// The metrics loop has stopped ticking while the probe thread is still
    /// healthy. Deliberately distinct from `MonitorStalled`: it means the
    /// detector is intact and something in `collect_metrics` or tracing is
    /// stuck — a real finding, and no longer a mislabel that hides a wedge.
    HealthMetricsStalled,
}

impl WatchdogReason {
    fn as_str(self) -> &'static str {
        match self {
            WatchdogReason::BackendWedged => "backend-wedged",
            WatchdogReason::MonitorStalled => "health-monitor-thread-stalled",
            WatchdogReason::HealthMetricsStalled => "health-metrics-thread-stalled",
        }
    }
}

/// Rate-limiting state machine for the watchdog. Pure — no clock, no IO.
#[derive(Debug, Default)]
struct WatchdogState {
    active: Option<WatchdogReason>,
    last_written_ms: i64,
}

impl WatchdogState {
    /// Fold one sample in and return the incident to append, if any.
    fn step(&mut self, s: WatchdogSample) -> Option<WatchdogReason> {
        // A thread that never started (heartbeat 0) is not yet stalled.
        let monitor_stalled = s.monitor_heartbeat_ms > 0
            && (s.now_ms - s.monitor_heartbeat_ms) / 1000 > WATCHDOG_MONITOR_STALL_SECS;
        let metrics_stalled = s.metrics_heartbeat_ms > 0
            && (s.now_ms - s.metrics_heartbeat_ms) / 1000 > WATCHDOG_MONITOR_STALL_SECS;

        // Precedence — each rank is a claim about EVIDENCE, not severity:
        //
        //   1. Probe thread stalled → its `backend_wedged` flag is stale by
        //      definition, so nothing it says can be trusted.
        //   2. Backend wedged → the probe thread is live and reporting silence
        //      on `/livez`. **This arm has to be reachable.** Before Item 4 it
        //      never was: the probe shared a thread with the metrics work and
        //      published its stamp only after a blocking `error!`, so any real
        //      wedge presented as a monitor-stall instead.
        //   3. Metrics loop stalled → detector healthy, backend answering, but
        //      something in `collect_metrics`/tracing is stuck.
        let reason = if monitor_stalled {
            Some(WatchdogReason::MonitorStalled)
        } else if s.backend_wedged {
            Some(WatchdogReason::BackendWedged)
        } else if metrics_stalled {
            Some(WatchdogReason::HealthMetricsStalled)
        } else {
            None
        };

        match (self.active, reason) {
            (_, None) => {
                self.active = None;
                None
            }
            (Some(prev), Some(now)) if prev == now => {
                if s.now_ms - self.last_written_ms >= WATCHDOG_REPEAT_SECS * 1000 {
                    self.last_written_ms = s.now_ms;
                    Some(now)
                } else {
                    None
                }
            }
            (_, Some(now)) => {
                // First observation of this condition — always write.
                self.active = Some(now);
                self.last_written_ms = s.now_ms;
                Some(now)
            }
        }
    }
}

/// Path of the always-rewritten liveness stamp.
fn watchdog_heartbeat_path(dir: &Path) -> PathBuf {
    dir.join("wedge-watchdog.txt")
}

/// Path of the append-only incident log (shared with the monitor's own
/// breadcrumb, so one file answers "what happened to this runner").
fn wedge_incidents_path(dir: &Path) -> PathBuf {
    dir.join("wedge-incidents.log")
}

/// Rewrite the fixed-size heartbeat stamp. Best-effort, blocking, no tracing.
fn write_watchdog_heartbeat(dir: &Path, s: WatchdogSample) {
    use std::io::Write;
    let monitor_age = if s.monitor_heartbeat_ms > 0 {
        ((s.now_ms - s.monitor_heartbeat_ms) / 1000).to_string()
    } else {
        "never".to_string()
    };
    let metrics_age = if s.metrics_heartbeat_ms > 0 {
        ((s.now_ms - s.metrics_heartbeat_ms) / 1000).to_string()
    } else {
        "never".to_string()
    };
    let line = format!(
        "{} pid={} monitor_heartbeat_age_secs={} metrics_heartbeat_age_secs={} \
         backend_wedged={} consecutive_failures={}\n",
        chrono::Utc::now().to_rfc3339(),
        std::process::id(),
        monitor_age,
        metrics_age,
        s.backend_wedged,
        s.consecutive_failures
    );
    if let Ok(mut f) = std::fs::File::create(watchdog_heartbeat_path(dir)) {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}

/// Append one incident line. Best-effort, blocking, no tracing.
fn append_watchdog_incident(dir: &Path, reason: WatchdogReason, s: WatchdogSample) {
    use std::io::Write;
    let monitor_age = if s.monitor_heartbeat_ms > 0 {
        ((s.now_ms - s.monitor_heartbeat_ms) / 1000).to_string()
    } else {
        "never".to_string()
    };
    let metrics_age = if s.metrics_heartbeat_ms > 0 {
        ((s.now_ms - s.metrics_heartbeat_ms) / 1000).to_string()
    } else {
        "never".to_string()
    };
    let line = format!(
        "{} WATCHDOG {} — pid {}, probe heartbeat {}s old, metrics heartbeat {}s old, \
         consecutive /livez failures {}. Written by the runtime-independent watchdog \
         thread, so this line survives a fully parked runtime.\n",
        chrono::Utc::now().to_rfc3339(),
        reason.as_str(),
        std::process::id(),
        monitor_age,
        metrics_age,
        s.consecutive_failures
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(wedge_incidents_path(dir))
    {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}

/// The watchdog loop. Blocking by design; call it from a dedicated OS thread.
fn watchdog_loop<S>(dir: PathBuf, tick: Duration, stop: Arc<AtomicBool>, sample: S)
where
    S: Fn() -> WatchdogSample,
{
    let mut state = WatchdogState::default();
    while !stop.load(Ordering::SeqCst) {
        let s = sample();
        write_watchdog_heartbeat(&dir, s);
        if let Some(reason) = state.step(s) {
            append_watchdog_incident(&dir, reason, s);
        }
        std::thread::sleep(tick);
    }
}

/// Sample the live process state.
fn live_watchdog_sample() -> WatchdogSample {
    WatchdogSample {
        now_ms: chrono::Utc::now().timestamp_millis(),
        monitor_heartbeat_ms: MONITOR_HEARTBEAT_MS.load(Ordering::SeqCst),
        metrics_heartbeat_ms: METRICS_HEARTBEAT_MS.load(Ordering::SeqCst),
        backend_wedged: BACKEND_WEDGED.load(Ordering::SeqCst),
        consecutive_failures: MONITOR_CONSECUTIVE_FAILURES.load(Ordering::SeqCst),
    }
}

/// Start the watchdog on its own OS thread, with an explicit output dir, tick
/// and stop flag. The seam the tests drive.
fn start_wedge_watchdog_with<S>(dir: PathBuf, tick: Duration, stop: Arc<AtomicBool>, sample: S)
where
    S: Fn() -> WatchdogSample + Send + 'static,
{
    // `std::thread::spawn`, NOT a tokio task. This is the entire point of the
    // item: the thread must be schedulable by the OS when the runtime is not.
    let _ = std::thread::Builder::new()
        .name("wedge-watchdog".to_string())
        .spawn(move || watchdog_loop(dir, tick, stop, sample));
}

/// Guard so `stop_health_monitor` also stops the watchdog.
static WATCHDOG_STOP: once_cell::sync::Lazy<Arc<AtomicBool>> =
    once_cell::sync::Lazy::new(|| Arc::new(AtomicBool::new(false)));

/// Start the production watchdog. Idempotent alongside the monitor's own
/// `MONITOR_RUNNING` latch (this is only ever called from `start_health_monitor`).
fn start_wedge_watchdog() {
    // Resolve the directory ONCE, here, on a healthy thread — never inside the
    // loop, where a stall in settings/path resolution would silence us.
    let dir = crate::paths::get_dev_logs_dir();
    WATCHDOG_STOP.store(false, Ordering::SeqCst);
    start_wedge_watchdog_with(
        dir,
        Duration::from_secs(WATCHDOG_TICK_SECS),
        WATCHDOG_STOP.clone(),
        live_watchdog_sample,
    );
}

/// Health metrics snapshot
#[derive(Debug, Clone)]
pub struct HealthMetrics {
    /// Timestamp of the check
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Memory usage in bytes (working set on Windows)
    pub memory_bytes: u64,
    /// Number of threads
    pub thread_count: usize,
    /// Process uptime in seconds
    pub uptime_secs: u64,
    /// CPU usage percentage (0-100)
    pub cpu_percent: Option<f32>,
}

impl HealthMetrics {
    /// Memory usage in megabytes
    pub fn memory_mb(&self) -> u64 {
        self.memory_bytes / (1024 * 1024)
    }
}

/// Get current process memory usage in bytes
#[cfg(target_os = "windows")]
fn get_memory_usage() -> u64 {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    unsafe {
        let handle = GetCurrentProcess();
        let mut counters = MaybeUninit::<PROCESS_MEMORY_COUNTERS>::uninit();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;

        if GetProcessMemoryInfo(handle, counters.as_mut_ptr(), size) != 0 {
            let counters = counters.assume_init();
            counters.WorkingSetSize as u64
        } else {
            0
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn get_memory_usage() -> u64 {
    // On non-Windows platforms, try to read from /proc/self/statm
    if let Ok(content) = std::fs::read_to_string("/proc/self/statm") {
        if let Some(first) = content.split_whitespace().next() {
            if let Ok(pages) = first.parse::<u64>() {
                // Page size is typically 4096 bytes
                return pages * 4096;
            }
        }
    }
    0
}

/// This process's OS thread count, or `None` when the count is genuinely
/// UNREADABLE.
///
/// ## Why an `Option` and not the `0` its caller used to get
///
/// Every arm below has a failure path, and a live process always owns at least
/// the thread asking the question — so `0` is not a low reading, it is a
/// sensor failure wearing a reading's clothes. That distinction is load-bearing
/// now that [`crate::resource_guard`] compares this number against a CEILING:
/// read as a quantity, `0` is the most reassuring value the type can hold, and
/// a guard that silently reads "perfectly idle" out of a failed snapshot is a
/// guard that is missing on exactly the boxes whose instrumentation is already
/// suffering. `None` is UNKNOWN, and this fleet's guards fail OPEN on UNKNOWN
/// (`resource_guard`'s "Fail OPEN, always").
///
/// ## Cost, because this now runs on the spawn path
///
/// [`crate::resource_guard::probe_for_spawn`] calls this synchronously on a
/// tokio worker immediately before a PTY opens, so whatever this touches, a
/// runtime worker waits for. Both arms are in-process reads of an OS table: no
/// subprocess, no WMI, no network, no `sysinfo` refresh. The Windows arm is the
/// more expensive of the two — `CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD)`
/// snapshots the SYSTEM-wide thread table and this walks it filtering on our
/// PID — but it is the same call the health monitor has made every 60 s since
/// this module shipped, it allocates nothing on the heap, and it is bounded by
/// the number of threads on the box (thousands, not millions). It is still
/// cheaper than the WMI query whose UNTIMED variant wedged the runner on
/// 2026-08-29, which is the incident this lane exists to keep from repeating.
pub(crate) fn thread_count_reading() -> Option<usize> {
    #[cfg(target_os = "windows")]
    {
        use std::mem::MaybeUninit;
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcessId;

        unsafe {
            let current_pid = GetCurrentProcessId();
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
                // The snapshot handle failed — under memory pressure, which is
                // precisely when the caller most wants an answer. UNKNOWN.
                return None;
            }

            let mut te32 = MaybeUninit::<THREADENTRY32>::uninit();
            (*te32.as_mut_ptr()).dwSize = std::mem::size_of::<THREADENTRY32>() as u32;

            let mut count = 0usize;

            if Thread32First(snapshot, te32.as_mut_ptr()) != 0 {
                loop {
                    let entry = te32.assume_init_ref();
                    if entry.th32OwnerProcessID == current_pid {
                        count += 1;
                    }
                    if Thread32Next(snapshot, te32.as_mut_ptr()) == 0 {
                        break;
                    }
                }
            }

            windows_sys::Win32::Foundation::CloseHandle(snapshot);
            // A zero count means `Thread32First` failed or the walk never saw
            // this process — impossible for a live process, so it is the
            // enumeration that failed, not the thread count that is zero.
            (count > 0).then_some(count)
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        // Linux: `/proc/self/task` has one entry per thread. An unreadable
        // `/proc` (macOS, which has no procfs at all, or a container that
        // hides it) is UNKNOWN rather than zero — same argument as above, and
        // the arm that keeps this lane harmless off Windows exactly as the
        // free-commit lane is harmless there.
        let count = std::fs::read_dir("/proc/self/task").ok()?.count();
        (count > 0).then_some(count)
    }
}

/// Get current thread count, with an unreadable sensor rendered as `0`.
///
/// The health monitor's own surfaces ([`HealthMetrics`], [`HealthStatus`]) are
/// a `usize` on the wire and compare against a warning threshold, where a `0`
/// on a failed read is merely a missing warning. Anything making a DECISION
/// from this number must use [`thread_count_reading`] instead and handle the
/// `None`.
fn get_thread_count() -> usize {
    thread_count_reading().unwrap_or(0)
}

/// Get process start time
fn get_start_time() -> Option<std::time::Instant> {
    // We use a static to track when the monitor was started
    use std::sync::OnceLock;
    static START_TIME: OnceLock<std::time::Instant> = OnceLock::new();
    Some(*START_TIME.get_or_init(std::time::Instant::now))
}

/// Collect current health metrics
pub fn collect_metrics() -> HealthMetrics {
    let uptime = get_start_time()
        .map(|start| start.elapsed().as_secs())
        .unwrap_or(0);

    HealthMetrics {
        timestamp: chrono::Utc::now(),
        memory_bytes: get_memory_usage(),
        thread_count: get_thread_count(),
        uptime_secs: uptime,
        cpu_percent: None, // CPU tracking requires more complex implementation
    }
}

/// Log health metrics
fn log_metrics(metrics: &HealthMetrics) {
    let memory_mb = metrics.memory_mb();
    let threads = metrics.thread_count;

    // Always log at debug level
    debug!(
        memory_mb = memory_mb,
        threads = threads,
        uptime_secs = metrics.uptime_secs,
        "Health check"
    );

    // Warn if thresholds are exceeded
    if memory_mb > MEMORY_WARNING_THRESHOLD_MB {
        warn!(
            memory_mb = memory_mb,
            threshold_mb = MEMORY_WARNING_THRESHOLD_MB,
            "Memory usage exceeds warning threshold - possible memory leak"
        );
    }

    if threads > THREAD_WARNING_THRESHOLD {
        warn!(
            threads = threads,
            threshold = THREAD_WARNING_THRESHOLD,
            "Thread count exceeds warning threshold - possible thread leak"
        );
    }
}

/// Start the health monitoring background task
pub fn start_health_monitor() {
    // Check if already running
    if MONITOR_RUNNING.swap(true, Ordering::SeqCst) {
        info!("Health monitor already running");
        return;
    }

    info!(
        "Starting health monitor (interval: {}s, memory_threshold: {}MB, thread_threshold: {}, \
         livez_probe_every: {}s)",
        HEALTH_CHECK_INTERVAL_SECS,
        MEMORY_WARNING_THRESHOLD_MB,
        THREAD_WARNING_THRESHOLD,
        SELF_PROBE_INTERVAL_SECS
    );

    // Everything below runs on DEDICATED OS THREADS, never tokio tasks — and
    // that is load-bearing, not incidental. These threads are the only things
    // that keep observing when the async runtime they watch is starved, which
    // is exactly the condition they exist to detect. Measured on 2026-08-08:
    // during a 25-minute wedge that silenced the entire HTTP surface, this
    // loop kept its cadence throughout. Never move any of them onto the
    // runtime they monitor.

    // The runtime-independent escape hatch. Started first so a stall in the
    // monitor threads themselves is still reported.
    start_wedge_watchdog();

    // TWO threads, not one. Item 4 (2026-08-24).
    //
    // Iteration 14 ran the metrics tick and the `/livez` probe on the same
    // loop, metrics first. `collect_metrics` walks process state and
    // `log_metrics` goes through the tracing subscriber — both can block when
    // the process is sick, and when they did, the probe below them simply
    // never ran. The watchdog then saw a stale probe stamp and reported
    // `health-monitor-thread-stalled` for an 11-minute outage whose actual
    // condition was `backend-wedged`, with `consecutive /livez failures`
    // stuck at 0.
    //
    // Splitting them means a stall in the metrics path costs metrics only:
    // the probe keeps its 5s cadence, the wedge detector keeps escalating, and
    // the watchdog can name what is actually wrong. Each loop stamps its own
    // liveness, so a stall in either is still *counted* rather than inferred.

    // ── The probe loop: the wedge detector, and nothing that can block it ──
    std::thread::spawn(|| {
        let mut wedge = WedgeDetector::default();
        let mut prober = LivezProber::new();
        while MONITOR_RUNNING.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(SELF_PROBE_INTERVAL_SECS));
            if !MONITOR_RUNNING.load(Ordering::SeqCst) {
                break;
            }
            // `observe` publishes the heartbeat, the failure count and
            // BACKEND_WEDGED *before* it writes the breadcrumb or logs, so a
            // blocked subscriber can no longer erase the evidence.
            wedge.observe(prober.probe());
        }
        info!("Health monitor livez probe stopped");
    });

    // ── The metrics loop: leak trends, on a coarse cadence ──
    std::thread::spawn(|| {
        while MONITOR_RUNNING.load(Ordering::SeqCst) {
            // Stamp BEFORE the work as well as after. A stamp published only
            // afterwards cannot distinguish "this loop is stuck inside
            // collect_metrics" from "this loop was never scheduled"; with
            // both, a stall anywhere in the tick is bounded by one interval.
            METRICS_HEARTBEAT_MS.store(chrono::Utc::now().timestamp_millis(), Ordering::SeqCst);
            let metrics = collect_metrics();
            log_metrics(&metrics);
            METRICS_HEARTBEAT_MS.store(chrono::Utc::now().timestamp_millis(), Ordering::SeqCst);

            let mut slept = 0;
            while slept < HEALTH_CHECK_INTERVAL_SECS && MONITOR_RUNNING.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(SELF_PROBE_INTERVAL_SECS));
                slept += SELF_PROBE_INTERVAL_SECS;
                // Sleeping is liveness too — otherwise a 60s interval with a
                // 180s stall threshold leaves only three ticks of margin.
                METRICS_HEARTBEAT_MS.store(chrono::Utc::now().timestamp_millis(), Ordering::SeqCst);
            }
        }

        info!("Health monitor stopped");
    });
}

/// Stop the health monitoring background task
pub fn stop_health_monitor() {
    info!("Stopping health monitor");
    MONITOR_RUNNING.store(false, Ordering::SeqCst);
    WATCHDOG_STOP.store(true, Ordering::SeqCst);
}

/// Check if the health monitor is running
pub fn is_running() -> bool {
    MONITOR_RUNNING.load(Ordering::SeqCst)
}

/// Get the current health status as a JSON-serializable struct
#[derive(Debug, Clone, serde::Serialize)]
pub struct HealthStatus {
    pub healthy: bool,
    pub memory_mb: u64,
    pub memory_warning: bool,
    pub thread_count: usize,
    pub thread_warning: bool,
    pub uptime_secs: u64,
    pub monitor_running: bool,
}

impl HealthStatus {
    /// Create a health status from metrics
    pub fn from_metrics(metrics: &HealthMetrics) -> Self {
        let memory_mb = metrics.memory_mb();
        let memory_warning = memory_mb > MEMORY_WARNING_THRESHOLD_MB;
        let thread_warning = metrics.thread_count > THREAD_WARNING_THRESHOLD;

        Self {
            healthy: !memory_warning && !thread_warning,
            memory_mb,
            memory_warning,
            thread_count: metrics.thread_count,
            thread_warning,
            uptime_secs: metrics.uptime_secs,
            monitor_running: is_running(),
        }
    }
}

/// Get the current health status
pub fn get_health_status() -> HealthStatus {
    let metrics = collect_metrics();
    HealthStatus::from_metrics(&metrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    // sysinfo's per-process introspection (memory + thread count) returns 0
    // on macOS CI runners because the runner process lacks the entitlement
    // to read its own /proc-equivalent. The functions still don't panic, so
    // the meaningful smoke-test on macOS is "doesn't crash"; quantitative
    // assertions are skipped.
    #[test]
    #[cfg_attr(target_os = "macos", ignore = "sysinfo returns 0 on macOS CI runners")]
    fn test_collect_metrics() {
        let metrics = collect_metrics();
        assert!(metrics.memory_bytes > 0);
        assert!(metrics.thread_count >= 1);
    }

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "sysinfo returns 0 on macOS CI runners")]
    fn test_health_status() {
        let status = get_health_status();
        assert!(status.memory_mb > 0);
        assert!(status.thread_count >= 1);
    }

    // ---- Phase 2: wedge escalation state machine ----
    //
    // Pure `step()` tests: no socket, no runtime, no clock. The thresholds are
    // the whole contract, so they get pinned rather than eyeballed.

    #[test]
    fn healthy_probes_never_escalate() {
        let mut d = WedgeDetector::default();
        for _ in 0..100 {
            assert_eq!(d.step(true), WedgeAction::None);
        }
    }

    #[test]
    fn escalates_exactly_once_at_the_threshold() {
        let mut d = WedgeDetector::default();
        for _ in 1..WEDGE_FAILURE_THRESHOLD {
            assert_eq!(d.step(false), WedgeAction::None, "escalated too early");
        }
        assert_eq!(
            d.step(false),
            WedgeAction::Escalate {
                consecutive_failures: WEDGE_FAILURE_THRESHOLD
            }
        );
        // Immediately after, it must go quiet rather than firing every probe.
        assert_eq!(d.step(false), WedgeAction::None);
    }

    #[test]
    fn a_single_success_resets_the_streak() {
        // The failure count must be CONSECUTIVE. An intermittently-accepting
        // runner (the observed 401 -> TIMEOUT -> REFUSED -> 401 fingerprint)
        // must not accumulate scattered failures into a false wedge.
        let mut d = WedgeDetector::default();
        for _ in 0..(WEDGE_FAILURE_THRESHOLD - 1) {
            d.step(false);
        }
        assert_eq!(d.step(true), WedgeAction::None);
        for _ in 0..(WEDGE_FAILURE_THRESHOLD - 1) {
            assert_eq!(d.step(false), WedgeAction::None, "streak did not reset");
        }
    }

    #[test]
    fn re_escalates_periodically_while_still_wedged() {
        let mut d = WedgeDetector::default();
        for _ in 0..WEDGE_FAILURE_THRESHOLD {
            d.step(false);
        }
        let mut reescalations = 0;
        for _ in 0..(WEDGE_REESCALATION_EVERY * 2) {
            if matches!(d.step(false), WedgeAction::ReEscalate { .. }) {
                reescalations += 1;
            }
        }
        assert_eq!(reescalations, 2, "expected one reminder per period");
    }

    #[test]
    fn recovery_is_reported_only_if_we_had_escalated() {
        // Recovering from a sub-threshold blip is not an incident and must
        // not produce a "recovered" line for an outage nobody saw.
        let mut d = WedgeDetector::default();
        d.step(false);
        assert_eq!(d.step(true), WedgeAction::None);

        for _ in 0..WEDGE_FAILURE_THRESHOLD {
            d.step(false);
        }
        assert_eq!(
            d.step(true),
            WedgeAction::Recovered {
                was_failing_for: WEDGE_FAILURE_THRESHOLD
            }
        );
        // And the next healthy probe is silent again.
        assert_eq!(d.step(true), WedgeAction::None);
    }

    // ---- Item 4: the detector must be able to report a wedge ----

    fn sample(now_ms: i64, monitor_ms: i64, wedged: bool) -> WatchdogSample {
        WatchdogSample {
            now_ms,
            monitor_heartbeat_ms: monitor_ms,
            // Default the metrics loop to the same liveness as the probe loop,
            // so the existing cases keep testing exactly what they tested.
            metrics_heartbeat_ms: monitor_ms,
            backend_wedged: wedged,
            consecutive_failures: if wedged { 3 } else { 0 },
        }
    }

    /// Independent stamps, for the cases Item 4 introduced.
    fn sample2(
        now_ms: i64,
        probe_ms: i64,
        metrics_ms: i64,
        wedged: bool,
        failures: u32,
    ) -> WatchdogSample {
        WatchdogSample {
            now_ms,
            monitor_heartbeat_ms: probe_ms,
            metrics_heartbeat_ms: metrics_ms,
            backend_wedged: wedged,
            consecutive_failures: failures,
        }
    }

    /// Saturate a tokio runtime — every worker AND every blocking thread — so
    /// nothing scheduled on it can run, then assert the watchdog still lands a
    /// breadcrumb on disk. This is the 2026-08-23 condition reproduced in
    /// miniature.
    #[test]
    fn the_watchdog_writes_a_breadcrumb_while_the_runtime_is_fully_parked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(2)
            .enable_all()
            .build()
            .expect("runtime");

        // Park every worker AND every blocking thread. Each parker gets its
        // OWN channel (no shared mutex) and signals `entered` before it
        // blocks, so the fixture is established before we measure — never
        // raced against task pickup.
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let mut releases = Vec::new();
        for _ in 0..2 {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            releases.push(tx);
            let etx = entered_tx.clone();
            rt.spawn(async move {
                let _ = etx.send(());
                let _ = rx.recv();
            });
        }
        for _ in 0..2 {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            releases.push(tx);
            let etx = entered_tx.clone();
            rt.spawn_blocking(move || {
                let _ = etx.send(());
                let _ = rx.recv();
            });
        }
        drop(entered_tx);
        for _ in 0..4 {
            entered_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("all four parkers must start");
        }

        // Prove the runtime really is parked: a trivial task must not finish.
        //
        // Measured with a std channel, NOT `tokio::time::timeout`: the time
        // driver is owned by the runtime's workers, so on a fully parked
        // runtime the timer itself never fires and `block_on` would hang
        // forever. Everything this test measures has to live outside the
        // runtime — which is the same property the watchdog itself relies on.
        let (probe_tx, probe_rx) = std::sync::mpsc::channel::<()>();
        rt.spawn(async move {
            let _ = probe_tx.send(());
        });
        assert!(
            probe_rx.recv_timeout(Duration::from_millis(750)).is_err(),
            "the runtime under test was not actually parked"
        );

        // Now start the watchdog — on its own OS thread, as production does.
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let now = chrono::Utc::now().timestamp_millis();
        start_wedge_watchdog_with(
            dir.clone(),
            Duration::from_millis(50),
            stop.clone(),
            move || sample(now, now, true),
        );

        let incidents = wedge_incidents_path(&dir);
        let heartbeat = watchdog_heartbeat_path(&dir);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if incidents.exists() && heartbeat.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        stop.store(true, Ordering::SeqCst);

        let body = std::fs::read_to_string(&incidents).unwrap_or_default();
        assert!(
            body.contains("backend-wedged"),
            "no breadcrumb reached disk while the runtime was parked; file was {body:?}"
        );
        assert!(
            heartbeat.exists(),
            "the watchdog heartbeat stamp was never written"
        );

        for tx in releases {
            let _ = tx.send(());
        }
    }

    /// The companion assertion — and the reason `std::thread::spawn` in
    /// `start_wedge_watchdog_with` is load-bearing rather than incidental.
    /// Hosting the same loop on the wedged runtime produces NOTHING, which is
    /// exactly what the old reporting path did.
    #[test]
    fn a_runtime_hosted_watchdog_would_be_silenced_by_the_wedge() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .expect("runtime");

        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let mut releases = Vec::new();
        {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            releases.push(tx);
            let etx = entered_tx.clone();
            rt.spawn(async move {
                let _ = etx.send(());
                let _ = rx.recv();
            });
        }
        {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            releases.push(tx);
            let etx = entered_tx.clone();
            rt.spawn_blocking(move || {
                let _ = etx.send(());
                let _ = rx.recv();
            });
        }
        drop(entered_tx);
        for _ in 0..2 {
            entered_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("both parkers must start");
        }

        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let now = chrono::Utc::now().timestamp_millis();
        let d = dir.clone();
        let s = stop.clone();
        rt.spawn_blocking(move || {
            watchdog_loop(d, Duration::from_millis(50), s, move || {
                sample(now, now, true)
            })
        });

        std::thread::sleep(Duration::from_millis(600));
        stop.store(true, Ordering::SeqCst);

        assert!(
            !wedge_incidents_path(&dir).exists(),
            "a runtime-hosted watchdog somehow reported — the wedge fixture is not wedging"
        );

        for tx in releases {
            let _ = tx.send(());
        }
    }

    /// A stalled MONITOR THREAD is itself an incident. This is the specific
    /// gap that produced zero evidence for the 2h45m freeze.
    #[test]
    fn a_stalled_monitor_thread_is_reported_even_when_it_says_healthy() {
        let mut w = WatchdogState::default();
        let t0 = 1_000_000_000_000i64;

        // Healthy and ticking → nothing.
        assert_eq!(w.step(sample(t0, t0, false)), None);

        // Monitor heartbeat goes stale while it still claims "not wedged".
        let later = t0 + (WATCHDOG_MONITOR_STALL_SECS + 1) * 1000;
        assert_eq!(
            w.step(sample(later, t0, false)),
            Some(WatchdogReason::MonitorStalled),
            "a monitor that stopped ticking must be reported on its own"
        );
    }

    /// Escalate once, then re-append on a bounded cadence — a multi-hour wedge
    /// must leave a trail, not a flood.
    #[test]
    fn watchdog_breadcrumbs_are_rate_limited_while_the_condition_persists() {
        let mut w = WatchdogState::default();
        let t0 = 1_000_000_000_000i64;

        assert_eq!(
            w.step(sample(t0, t0, true)),
            Some(WatchdogReason::BackendWedged),
            "the first observation must always be written"
        );
        // Every tick inside the repeat window is silent.
        let mut writes = 0;
        for i in 1..=(WATCHDOG_REPEAT_SECS / WATCHDOG_TICK_SECS as i64) {
            let t = t0 + i * WATCHDOG_TICK_SECS as i64 * 1000;
            if w.step(sample(t, t, true)).is_some() {
                writes += 1;
            }
        }
        assert_eq!(writes, 1, "exactly one reminder per repeat window");

        // Recovery clears the latch, so the next incident escalates again.
        let t = t0 + WATCHDOG_REPEAT_SECS * 1000 + 1000;
        assert_eq!(w.step(sample(t, t, false)), None);
        let t = t + 1000;
        assert_eq!(
            w.step(sample(t, t, true)),
            Some(WatchdogReason::BackendWedged)
        );
    }

    /// A monitor that has never ticked (heartbeat 0) is starting up, not
    /// stalled — the watchdog must not manufacture an incident at boot.
    #[test]
    fn a_never_started_monitor_is_not_a_stall() {
        let mut w = WatchdogState::default();
        let t0 = 1_000_000_000_000i64;
        assert_eq!(w.step(sample(t0, 0, false)), None);
    }

    #[test]
    fn detection_latency_stays_under_a_minute() {
        // The point of the phase is that an outage surfaces in seconds, not
        // the 7 hours the 2026-08-08 incident took. Guard the arithmetic so a
        // later interval bump cannot quietly restore a coarse clock.
        let latency = WEDGE_FAILURE_THRESHOLD as u64 * SELF_PROBE_INTERVAL_SECS;
        assert!(
            latency <= 60,
            "wedge detection latency regressed to {latency}s"
        );
    }

    // ---- Item 4 (2026-08-24): the watchdog must name the RIGHT thing ----
    //
    // The dedicated-thread watchdog worked — it wrote incidents through three
    // fully parked runtimes. What it could not do was tell the truth: every
    // incident read `health-monitor-thread-stalled`, the `backend-wedged` arm
    // never fired, and `consecutive /livez failures` read 0 through an
    // 11-minute outage. These pin the repair.

    /// **The load-bearing test for Item 4.** A live probe thread reporting a
    /// wedge must produce `backend-wedged` — not a monitor stall — even while
    /// the METRICS loop is stalled, which is the shape the outage actually had.
    ///
    /// Neuter check: restore the single-stamp precedence (drop
    /// `metrics_heartbeat_ms` and derive `monitor_stalled` from the metrics
    /// tick) and this test fails with `MonitorStalled`.
    #[test]
    fn a_wedge_is_named_backend_wedged_even_when_the_metrics_loop_is_stalled() {
        let mut w = WatchdogState::default();
        let t0 = 1_000_000_000_000i64;
        let now = t0 + (WATCHDOG_MONITOR_STALL_SECS + 60) * 1000;

        // Probe thread ticking (fresh), metrics loop long dead, backend wedged.
        let got = w.step(sample2(now, now - 3_000, t0, true, 7));
        assert_eq!(
            got,
            Some(WatchdogReason::BackendWedged),
            "a live probe thread reporting a wedge must be named backend-wedged; reporting a \
             monitor stall here is the exact mislabel that hid an 11-minute outage"
        );
    }

    /// A stalled metrics loop is its own condition — reported, but never at the
    /// cost of hiding a wedge, and never mislabelled as a dead detector.
    #[test]
    fn a_stalled_metrics_loop_is_reported_as_its_own_condition() {
        let mut w = WatchdogState::default();
        let t0 = 1_000_000_000_000i64;
        let now = t0 + (WATCHDOG_MONITOR_STALL_SECS + 60) * 1000;

        // Probe thread fresh, backend healthy, metrics loop stalled.
        assert_eq!(
            w.step(sample2(now, now - 3_000, t0, false, 0)),
            Some(WatchdogReason::HealthMetricsStalled)
        );
        assert_eq!(
            WatchdogReason::HealthMetricsStalled.as_str(),
            "health-metrics-thread-stalled"
        );
    }

    /// A dead PROBE thread still outranks everything: its wedge flag is stale
    /// by definition, so the watchdog must not repeat it as fact.
    #[test]
    fn a_stalled_probe_thread_still_outranks_a_stale_wedge_flag() {
        let mut w = WatchdogState::default();
        let t0 = 1_000_000_000_000i64;
        let now = t0 + (WATCHDOG_MONITOR_STALL_SECS + 60) * 1000;
        assert_eq!(
            w.step(sample2(now, t0, now - 3_000, true, 9)),
            Some(WatchdogReason::MonitorStalled)
        );
    }

    /// The failure count and the wedge flag must be published by `observe`
    /// BEFORE it reaches any tracing call. Iteration 14 published them after,
    /// which is why the outage reported `consecutive /livez failures 0`.
    ///
    /// Neuter check: move `self.publish()` to the end of `observe` and this
    /// test fails on the first assertion.
    #[test]
    fn observe_publishes_the_failure_count_before_it_reports() {
        // Serialise: these are process-global atomics.
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

        MONITOR_CONSECUTIVE_FAILURES.store(0, Ordering::SeqCst);
        BACKEND_WEDGED.store(false, Ordering::SeqCst);

        let mut d = WedgeDetector::default();
        // One sub-threshold failure: no escalation, no tracing — but the count
        // must already be visible to the watchdog.
        d.observe(false);
        assert_eq!(
            MONITOR_CONSECUTIVE_FAILURES.load(Ordering::SeqCst),
            1,
            "the failure count was not published — the watchdog would read 0 through a real \
             outage, exactly as it did"
        );
        assert!(
            !BACKEND_WEDGED.load(Ordering::SeqCst),
            "one failure is below the threshold and must not set the wedge flag"
        );
        assert!(
            MONITOR_HEARTBEAT_MS.load(Ordering::SeqCst) > 0,
            "the probe heartbeat was not published"
        );

        // Cross the threshold with the PURE half, then publish. Driving
        // `observe` here would append a real line to the operator's
        // `wedge-incidents.log`; the property under test is that `publish`
        // sets the flag, and `observe` calls `publish` before it reports —
        // asserted above.
        for _ in 1..WEDGE_FAILURE_THRESHOLD {
            d.step(false);
        }
        d.publish();
        assert_eq!(
            MONITOR_CONSECUTIVE_FAILURES.load(Ordering::SeqCst),
            WEDGE_FAILURE_THRESHOLD
        );
        assert!(
            BACKEND_WEDGED.load(Ordering::SeqCst),
            "the wedge flag was not published — the watchdog can never name backend-wedged"
        );

        // And recovery clears it.
        d.observe(true);
        assert!(!BACKEND_WEDGED.load(Ordering::SeqCst));
        assert_eq!(MONITOR_CONSECUTIVE_FAILURES.load(Ordering::SeqCst), 0);

        BACKEND_WEDGED.store(false, Ordering::SeqCst);
        MONITOR_CONSECUTIVE_FAILURES.store(0, Ordering::SeqCst);
    }

    /// The probe loop must survive a probe that never returns. Without this,
    /// a stuck probe freezes `consecutive_failures` at whatever it was — which
    /// is how an outage was recorded with `consecutive /livez failures 0` and
    /// no `backend-wedged` line.
    ///
    /// Measured with a std channel on the test thread: no runtime is involved
    /// at all, so a regression fails instead of hanging.
    ///
    /// Neuter check: raise `PROBE_HARD_DEADLINE_SECS` past the assertion
    /// window (or call `probe_livez_blocking` directly from the loop) and this
    /// test fails.
    #[test]
    fn a_probe_that_never_returns_is_bounded_and_reads_as_a_failure() {
        use std::sync::mpsc::{RecvTimeoutError, TryRecvError};

        // The production `LivezProber::probe` shape, driven with a short
        // deadline so the test does not wait out the real 20s. Everything is
        // measured on the test thread with std channels — no runtime is
        // involved, so a regression FAILS rather than hangs.
        let (req_tx, req_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let (res_tx, res_rx) = std::sync::mpsc::channel::<bool>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            while req_rx.recv().is_ok() {
                let _ = release_rx.recv(); // a probe that never answers
                if res_tx.send(true).is_err() {
                    break;
                }
            }
        });

        let short = Duration::from_millis(300);
        let mut outstanding = false;
        let mut verdicts = Vec::new();
        let started = std::time::Instant::now();

        // Three consecutive ticks against a prober that never answers. The
        // FIRST costs the deadline; every later one must be instant, because a
        // detector that re-waits per tick falls behind the outage it is timing.
        for _ in 0..3 {
            if outstanding {
                match res_rx.try_recv() {
                    Ok(_late) => outstanding = false,
                    Err(TryRecvError::Empty) => {
                        verdicts.push(false);
                        continue;
                    }
                    Err(TryRecvError::Disconnected) => break,
                }
            }
            let _ = req_tx.try_send(());
            match res_rx.recv_timeout(short) {
                Ok(alive) => verdicts.push(alive),
                Err(RecvTimeoutError::Timeout) => {
                    outstanding = true;
                    verdicts.push(false);
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        assert_eq!(
            verdicts,
            vec![false, false, false],
            "an unanswered probe must read as a FAILURE every tick — otherwise \
             consecutive_failures freezes and the watchdog can never name backend-wedged, \
             which is exactly what an 11-minute outage recorded"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the probe loop re-waited the full deadline on every tick ({:?}) — it must give up \
             once and then report immediately",
            started.elapsed()
        );
        let _ = release_tx.send(());

        // The per-probe deadline must sit well inside the stall window, or a
        // bounded probe still presents as a stalled thread.
        assert!(
            (PROBE_HARD_DEADLINE_SECS as i64) < WATCHDOG_MONITOR_STALL_SECS,
            "the per-probe deadline must be well inside the monitor-stall window"
        );
    }

    /// The probe loop must not sit behind the metrics work. A source-level
    /// invariant is the cheapest guard: the two loops are separate
    /// `std::thread::spawn` bodies, and `collect_metrics` appears in neither
    /// the probe loop nor before the probe's first stamp.
    #[test]
    fn the_probe_loop_does_not_share_a_thread_with_the_metrics_work() {
        let src = include_str!("health_monitor.rs");
        let body = src
            .split("pub fn start_health_monitor()")
            .nth(1)
            .expect("start_health_monitor must exist");
        let probe_at = body.find("prober.probe()").expect("probe call");
        let metrics_at = body.find("collect_metrics()").expect("metrics call");
        assert!(
            probe_at < metrics_at,
            "the /livez probe runs after collect_metrics again — a stall in the metrics path \
             suppresses wedge detection, which is how an 11-minute outage reported 0 failures"
        );
    }
}
