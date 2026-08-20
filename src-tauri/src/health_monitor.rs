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
const THREAD_WARNING_THRESHOLD: usize = 150;

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

/// Per-sample timeout for the native message-loop probe, in milliseconds.
///
/// 3s deliberately matches `mcp::ui_bridge::window_probe`'s
/// `WINDOW_GETTER_TIMEOUT` — the bound this tree already decided a healthy
/// event loop must beat — so the two liveness surfaces cannot disagree about
/// what "slow" means. It must stay strictly below the sample interval, or the
/// probe would stretch the cadence of the loop it rides on; a unit test pins
/// that ordering.
pub const UI_THREAD_PROBE_TIMEOUT_MS: u32 = 3_000;

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

/// Set while the *native message loop* is believed hung, so other in-process
/// surfaces can read it without waiting on the monitor thread.
static UI_THREAD_WEDGED: AtomicBool = AtomicBool::new(false);

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

/// Whether the monitor currently believes the native UI thread has stopped
/// pumping messages.
///
/// A plain atomic read: this is the one fact a wedged runner must be able to
/// report from any thread, including one that cannot touch the event loop.
pub fn ui_thread_wedged() -> bool {
    UI_THREAD_WEDGED.load(Ordering::SeqCst)
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

// ───────────────── native message-loop liveness (Phase 4) ─────────────────
//
// Plan: `2026-08-19-runner-blocked-ui-thread-cannot-be-closed.md`, Phase 4.
//
// # This probe is the ONLY detector for the failure it watches
//
// That is not rhetoric, it is the vetted finding, and it is the reason this
// rung exists at all rather than being folded into an existing one:
//
// * `/livez` and `/health` keep answering `200` while the native loop is
//   wedged — measured 2026-08-19 on PID 148320, `Responding: False` with
//   `/health` still returning `200`. So [`probe_livez_blocking`], the
//   2026-08-07 detector, is blind here **by construction**.
// * The WebView2 `ProcessFailed` push event (`webview_recovery`, 2026-08-01)
//   cannot fire: **no process died**.
// * The shipped `ui_stale` / `UI_DEAD_AFTER_MS` backstop is blind too. The
//   frontend runs an *unconditional* 3s HTTP pong
//   (`src/hooks/useUIBridgeEventHandler.ts` → `POST /ui-bridge/pong` →
//   `mcp/ui_bridge/capabilities.rs`), and WebView2 services `fetch` in the
//   browser/network process, not on the host's UI thread. So
//   `ui_bridge_last_pong` stays fresh throughout the hang and
//   `derived_status` stays `healthy`. Giving that atom provenance is a
//   separate, later change — do not assume it has landed.
//
// There is therefore no 90s floor under this on Windows: without this probe
// the condition is not detected late, it is **never detected**.

/// One observation of the native message loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiThreadSample {
    /// The loop answered a `WM_NULL` round-trip inside the timeout.
    pub pumping: bool,
    /// `IsHungAppWindow`'s independent verdict, sampled in the same tick as a
    /// free cross-check. Its threshold is OS-internal and untunable, which is
    /// why it corroborates rather than decides. `None` where the OS cannot be
    /// asked at all.
    pub os_reports_hung: Option<bool>,
    /// `GetLastError` after a failed round-trip. Used only to tell a genuine
    /// hang from *our own* defect — see [`ui_thread_pumping`].
    pub last_error: u32,
}

/// Warn once, not every 5s, when the probe cannot ask the question.
///
/// `cfg(windows)` because its only reader is [`ui_thread_pumping`]'s Windows
/// arm; without the gate it is `dead_code` on every other platform.
#[cfg(windows)]
static UI_THREAD_PROBE_DEFECT_LOGGED: AtomicBool = AtomicBool::new(false);

/// Blocking native message-loop probe.
///
/// `SendMessageTimeoutW(hwnd, WM_NULL, …, SMTO_ABORTIFHUNG, timeout_ms, …)`.
/// `WM_NULL` is defined to be ignored by every window procedure, so the round
/// trip observes the loop without perturbing the application: it succeeds iff
/// the thread that owns `hwnd` reached `GetMessage`/`DispatchMessage` within
/// the timeout.
///
/// Deliberately a *blocking* call on the health monitor's own OS thread —
/// bounded by `timeout_ms` and by `SMTO_ABORTIFHUNG`, which returns
/// immediately when Windows has already flagged the window hung. Routing it
/// through the async runtime (as `window_probe` does, correctly, on the async
/// side) would make the detector share fate with the process it is measuring.
#[cfg(windows)]
fn probe_ui_thread_blocking(hwnd: isize, timeout_ms: u32) -> UiThreadSample {
    use windows_sys::Win32::Foundation::{GetLastError, HWND};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IsHungAppWindow, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_NULL,
    };

    let hwnd = hwnd as HWND;
    let mut round_trip_result: usize = 0;

    // SAFETY: `hwnd` is the main window's handle, cached on the UI thread at
    // startup by `ui_thread_probe` (never fetched via the unbounded
    // `window.hwnd()` getter, which would park this thread during exactly the
    // hang it is detecting). `round_trip_result` is a live stack local for the
    // duration of the call. Both functions are read-only Win32 queries.
    unsafe {
        let lresult = SendMessageTimeoutW(
            hwnd,
            WM_NULL,
            0,
            0,
            SMTO_ABORTIFHUNG,
            timeout_ms,
            &mut round_trip_result as *mut usize,
        );
        let last_error = if lresult == 0 { GetLastError() } else { 0 };
        UiThreadSample {
            pumping: lresult != 0,
            os_reports_hung: Some(IsHungAppWindow(hwnd) != 0),
            last_error,
        }
    }
}

/// Sample the native message loop, or report UNKNOWN.
///
/// `None` means "could not ask" — no cached HWND yet (startup, server mode,
/// or a headless build) or a handle the OS rejects. UNKNOWN is **never** folded
/// into the wedge detector: manufacturing a hang out of our own defect is the
/// same failure `probe_livez_blocking` guards against when its client build
/// fails.
///
/// Only `ERROR_INVALID_WINDOW_HANDLE` is read as our defect, and only when
/// `IsHungAppWindow` disagrees. Everything else — including a `GetLastError`
/// of `0`, which is what an `SMTO_ABORTIFHUNG` abort can leave behind — counts
/// as a hang. The asymmetry is deliberate: under-reporting this failure means
/// nobody ever learns of it, and over-reporting is already fenced off by the
/// 3-consecutive-sample threshold.
#[cfg(windows)]
pub fn ui_thread_pumping() -> Option<UiThreadSample> {
    use windows_sys::Win32::Foundation::ERROR_INVALID_WINDOW_HANDLE;

    let hwnd = crate::ui_thread_probe::main_hwnd()?;
    let sample = probe_ui_thread_blocking(hwnd, UI_THREAD_PROBE_TIMEOUT_MS);

    if !sample.pumping
        && sample.last_error == ERROR_INVALID_WINDOW_HANDLE
        && !sample.os_reports_hung.unwrap_or(false)
    {
        if !UI_THREAD_PROBE_DEFECT_LOGGED.swap(true, Ordering::SeqCst) {
            warn!(
                hwnd,
                last_error = sample.last_error,
                "UI-thread probe: the cached main-window handle is no longer valid (a window \
                 recreate, most likely). Reporting UNKNOWN rather than a hang."
            );
        }
        return None;
    }

    Some(sample)
}

/// Non-Windows stub — **a deliberate no-op, not an oversight.**
///
/// There is no portable equivalent of `SendMessageTimeoutW`: the probe asks a
/// Win32 message loop a Win32 question. This matches the precedent already set
/// by the 2026-08-01 `ProcessFailed` detector in `webview_recovery`, which is
/// `#[cfg(windows)]` with a stub for the same reason.
///
/// **What other platforms fall back to, stated so nobody has to guess:** the
/// already-shipped, already-cross-platform heartbeat-staleness arm —
/// `ui_error::ui_dead_now` at `UI_DEAD_AFTER_MS` (90s), consumed by
/// `heartbeat.rs`, which calls `webview_recovery::trigger_ui_recovery` with
/// `RecoveryReason::HeartbeatStale`. On macOS/Linux that arm really does cover
/// this failure, because the webview's JS runs on the same blocked main
/// thread, so a blocked loop stops the HTTP pong as well. The Windows-specific
/// hole this probe fills is WebView2's out-of-process `fetch`, which keeps
/// ponging while the host thread is dead. So off Windows the detection is
/// **90s-latency, not absent**.
#[cfg(not(windows))]
pub fn ui_thread_pumping() -> Option<UiThreadSample> {
    None
}

/// Which liveness rung an observation belongs to.
///
/// The two rungs share **one** state machine on purpose. The noise-floor
/// argument is identical for both — [`WEDGE_FAILURE_THRESHOLD`] consecutive
/// samples, so a single dropped connection or a single long repaint cannot
/// escalate — and a second hand-rolled copy of it would be free to drift.
/// Only the side effects at the escalation edge differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum WedgeKind {
    /// `/livez` stopped answering: the async HTTP surface is starved
    /// (2026-08-07 incident). The Rust backend cannot serve.
    #[default]
    Backend,
    /// The native message loop stopped pumping (2026-08-19 incident). The
    /// window does not repaint and the X button does nothing, while `/livez`
    /// and `/health` keep answering `200`.
    UiThread,
}

impl WedgeKind {
    /// Stable machine-readable token written into `wedge-incidents.log`.
    ///
    /// **Do not reword these.** The breadcrumb file is the thing someone greps
    /// after an unexplained outage, precisely because the logs that would have
    /// answered the 2026-08-19 incident were destroyed within a day —
    /// `runner-lifecycle.log` is truncated at every runner startup, so a
    /// restart erases the record of the wedge that provoked it.
    fn breadcrumb_reason(self) -> &'static str {
        match self {
            Self::Backend => "backend_wedged",
            Self::UiThread => "ui_thread_wedged",
        }
    }
}

/// Escalation state machine for consecutive probe failures on one rung.
///
/// Pure and side-effect free apart from the escalation callbacks, so the
/// thresholds can be tested without a runtime, a socket, or a clock.
#[derive(Debug, Default)]
struct WedgeDetector {
    kind: WedgeKind,
    consecutive_failures: u32,
    escalated: bool,
    /// When the CURRENT failure streak started, for the reported duration.
    ///
    /// Set by [`Self::observe`], never by the pure [`Self::step`] — so every
    /// threshold test stays clock-free while the number a human reads is
    /// measured rather than inferred. See [`reported_unresponsive_secs`].
    first_failure_at: Option<std::time::Instant>,
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
    /// The probe went UNKNOWN while this rung was latched, so the latch is
    /// released for want of evidence — **not** because recovery was observed.
    ///
    /// Without this the latch is a one-way door: `observe()` is only reached
    /// with a real sample, so a rung that escalates and then loses its probe
    /// (a destroyed window, a handle the OS rejects) can never see the
    /// `Recovered` arm again. `derived_status` would stay pinned `errored` and
    /// `close-request` would answer 503 for the rest of the process's life,
    /// including long after the loop came back.
    LatchReleasedUnknown { was_failing_for: u32 },
}

impl WedgeDetector {
    /// A detector for one rung. `Default` is the backend rung, which is what
    /// the pre-existing call sites and tests mean.
    fn new(kind: WedgeKind) -> Self {
        Self {
            kind,
            ..Self::default()
        }
    }

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
        let wedged = self.consecutive_failures >= WEDGE_FAILURE_THRESHOLD;
        match self.kind {
            WedgeKind::Backend => {
                BACKEND_WEDGED.store(wedged, Ordering::SeqCst);
                // Only the backend rung publishes the watchdog's view of the
                // probe loop. Both rungs ride ONE thread, so a second stamp
                // per tick would not add liveness — it would only make a
                // stalled backend detector look alive.
                MONITOR_CONSECUTIVE_FAILURES.store(self.consecutive_failures, Ordering::SeqCst);
                MONITOR_HEARTBEAT_MS.store(chrono::Utc::now().timestamp_millis(), Ordering::SeqCst);
            }
            WedgeKind::UiThread => UI_THREAD_WEDGED.store(wedged, Ordering::SeqCst),
        }
    }

    /// Fold in an UNKNOWN observation — "the probe could not ask".
    ///
    /// UNKNOWN is NOT a failure (it must never accumulate toward an
    /// escalation) and it is NOT a success (nothing was observed, so it must
    /// not be reported as recovery). What it must do is release a latch it can
    /// no longer justify: see [`WedgeAction::LatchReleasedUnknown`].
    fn step_unknown(&mut self) -> WedgeAction {
        let was = self.consecutive_failures;
        self.consecutive_failures = 0;
        if !self.escalated {
            return WedgeAction::None;
        }
        self.escalated = false;
        WedgeAction::LatchReleasedUnknown {
            was_failing_for: was,
        }
    }

    /// Fold in an observation and perform the escalation side effects.
    fn observe(&mut self, alive: bool) {
        // Stamp the START of the streak, before `step` mutates the counter.
        // The reported duration is then measured wall-clock, not
        // `failures × interval` — see [`reported_unresponsive_secs`].
        if !alive && self.consecutive_failures == 0 {
            self.first_failure_at = Some(std::time::Instant::now());
        }
        let streak_started = self.first_failure_at;
        let action = self.step(alive);
        if alive {
            self.first_failure_at = None;
        }
        self.dispatch(action, streak_started);
    }

    /// Fold in an UNKNOWN observation and perform its side effects.
    fn observe_unknown(&mut self) {
        let streak_started = self.first_failure_at;
        let action = self.step_unknown();
        self.first_failure_at = None;
        self.dispatch(action, streak_started);
    }

    /// The side effects for one [`WedgeAction`].
    ///
    /// **Publishes the watchdog-visible state first**, for every action and
    /// both entry points. Atomics are stores to plain memory: they cannot
    /// block, so they go first. Then the on-disk breadcrumb. Tracing last,
    /// because it is the only step that shares fate with the subsystem being
    /// reported — an 11-minute outage showed `consecutive /livez failures 0`
    /// and never named `backend-wedged` precisely because the publish sat
    /// behind an `error!` whose subscriber was parked.
    fn dispatch(&mut self, action: WedgeAction, streak_started: Option<std::time::Instant>) {
        self.publish();
        match action {
            WedgeAction::None => {}
            WedgeAction::Escalate {
                consecutive_failures,
            }
            | WedgeAction::ReEscalate {
                consecutive_failures,
            } => {
                let secs = reported_unresponsive_secs(streak_started, consecutive_failures);
                // Breadcrumb FIRST, log second. `error!` goes through the
                // tracing subscriber — a writer, a bounded channel, a file
                // mutex — none of which is guaranteed to be schedulable in the
                // condition we are reporting. The durable record must not sit
                // behind it.
                write_wedge_breadcrumb(self.kind, secs);
                match self.kind {
                    WedgeKind::Backend => {
                        error!(
                            consecutive_failures,
                            unresponsive_for_secs = secs,
                            "BACKEND WEDGED: /livez has not answered for {secs}s. The HTTP \
                             surface is not serving — every agent session on this box is \
                             affected, and coord-mcp writes issued through this runner are \
                             being LOST. Not restarting: a process restart destroys in-flight \
                             sessions and is an explicit non-goal.",
                        );
                    }
                    WedgeKind::UiThread => {
                        error!(
                            consecutive_failures,
                            unresponsive_for_secs = secs,
                            "UI THREAD WEDGED: the native message loop has not answered a \
                             WM_NULL round-trip for {secs}s. The window will not repaint and \
                             the X button does nothing, while /livez and /health keep \
                             answering 200 — this probe is the ONLY detector for that state. \
                             Not restarting: a process restart destroys in-flight sessions \
                             and is an explicit non-goal.",
                        );
                    }
                }

                if self.kind == WedgeKind::UiThread {
                    // Third rung into the SHIPPED recovery surface, not a
                    // fourth parallel mechanism: `webview_recovery` owns the
                    // one user-visible escalation. It deliberately attempts no
                    // recovery ACTION here — every rung of that ladder
                    // dispatches through the loop that is wedged.
                    if let Some(app) = crate::tauri_app_handle::current() {
                        crate::webview_recovery::report_native_ui_thread_hang(&app, secs);
                    }
                    // No `AppHandle` means no Tauri runtime (still starting, or
                    // a unit-test context). The breadcrumb above is already on
                    // disk, so the incident is recorded either way.
                }
            }
            WedgeAction::Recovered { was_failing_for } => {
                let secs = reported_unresponsive_secs(streak_started, was_failing_for);
                match self.kind {
                    WedgeKind::Backend => {
                        BACKEND_WEDGED.store(false, Ordering::SeqCst);
                        warn!(
                            was_failing_for_secs = secs,
                            "Backend recovered: /livez is answering again after {secs}s of \
                             silence"
                        );
                    }
                    WedgeKind::UiThread => {
                        UI_THREAD_WEDGED.store(false, Ordering::SeqCst);
                        crate::webview_recovery::clear_native_ui_thread_hang();
                        warn!(
                            was_failing_for_secs = secs,
                            "UI thread recovered: the native message loop is pumping again \
                             after {secs}s"
                        );
                    }
                }
            }
            WedgeAction::LatchReleasedUnknown { was_failing_for } => {
                let secs = reported_unresponsive_secs(streak_started, was_failing_for);
                match self.kind {
                    WedgeKind::Backend => BACKEND_WEDGED.store(false, Ordering::SeqCst),
                    WedgeKind::UiThread => {
                        UI_THREAD_WEDGED.store(false, Ordering::SeqCst);
                        crate::webview_recovery::clear_native_ui_thread_hang();
                    }
                }
                // Deliberately NOT phrased as a recovery, and deliberately no
                // breadcrumb: nothing was observed. The latch is released
                // because the evidence for it expired, which is a different
                // fact from "the loop is pumping again".
                warn!(
                    was_failing_for_secs = secs,
                    kind = ?self.kind,
                    "Wedge latch released after {secs}s of failures: the probe can no longer \
                     be asked (UNKNOWN). NOT an observed recovery — the surfaces that read \
                     the latch now report UNKNOWN instead of a wedge nobody is still watching."
                );
            }
        }
    }
}

/// How long to report a rung as having been unresponsive.
///
/// Prefers the MEASURED wall-clock span since the first failure of the streak.
/// The old arithmetic — `consecutive_failures × SELF_PROBE_INTERVAL_SECS` —
/// silently under-reports, because the real inter-sample gap is the sleep PLUS
/// whatever the probes cost: on the UI-thread rung a hung loop can add up to
/// `UI_THREAD_PROBE_TIMEOUT_MS` per sample, so the runtime gate measured
/// ~17.2s for a wedge this formula called 15s (worst case 24s). That figure is
/// written into `wedge-incidents.log` and into a user-visible notification, so
/// it is read by a human trying to reconstruct an outage.
///
/// The estimate stays as the fallback for the one case with no stamp (a
/// detector driven directly, e.g. by a test), where it is explicitly a LOWER
/// BOUND.
fn reported_unresponsive_secs(
    streak_started: Option<std::time::Instant>,
    consecutive_failures: u32,
) -> u64 {
    match streak_started {
        Some(t) => t.elapsed().as_secs(),
        None => consecutive_failures as u64 * SELF_PROBE_INTERVAL_SECS,
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
fn write_wedge_breadcrumb(kind: WedgeKind, unresponsive_for_secs: u64) {
    let dir = crate::paths::get_dev_logs_dir();
    let path = dir.join("wedge-incidents.log");
    let detail = match kind {
        WedgeKind::Backend => {
            format!("runner backend wedged — /livez silent for {unresponsive_for_secs}s")
        }
        WedgeKind::UiThread => format!(
            "native UI thread wedged — no WM_NULL round-trip for {unresponsive_for_secs}s \
             (the HTTP surface, including /livez, was still answering)"
        ),
    };
    let line = format!(
        "{} {} {} (pid {})\n",
        chrono::Utc::now().to_rfc3339(),
        kind.breadcrumb_reason(),
        detail,
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

/// Get current thread count
fn get_thread_count() -> usize {
    // This is an approximation - we count threads by looking at the process
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
                return 0;
            }

            let mut te32 = MaybeUninit::<THREADENTRY32>::uninit();
            (*te32.as_mut_ptr()).dwSize = std::mem::size_of::<THREADENTRY32>() as u32;

            let mut count = 0;

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
            count
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        // On Linux, count entries in /proc/self/task
        if let Ok(entries) = std::fs::read_dir("/proc/self/task") {
            entries.count()
        } else {
            0
        }
    }
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
         livez_probe_every: {}s, ui_thread_probe_timeout: {}ms)",
        HEALTH_CHECK_INTERVAL_SECS,
        MEMORY_WARNING_THRESHOLD_MB,
        THREAD_WARNING_THRESHOLD,
        SELF_PROBE_INTERVAL_SECS,
        UI_THREAD_PROBE_TIMEOUT_MS
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
        let mut wedge = WedgeDetector::new(WedgeKind::Backend);
        // The SECOND rung rides the SAME thread and the SAME cadence on
        // purpose. A dedicated monitor thread for the UI probe would double
        // the thing whose survival is the entire point, and this loop is
        // already proved to keep its cadence through a wedge.
        let mut ui_wedge = WedgeDetector::new(WedgeKind::UiThread);
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

            // UNKNOWN (no cached HWND yet, non-Windows, a rejected
            // handle) never accumulates toward an escalation — but it is
            // not simply skipped either. `observe_unknown` releases an
            // already-latched wedge, because a latch is only honest while
            // something is still observing it: the raw skip this replaces
            // made the latch a ONE-WAY door, pinning `derived_status` at
            // `errored` and `close-request` at 503 for the rest of the
            // process's life if the probe went permanently UNKNOWN after
            // escalating.
            match ui_thread_pumping() {
                Some(sample) => {
                    if !sample.pumping {
                        debug!(
                            os_reports_hung = ?sample.os_reports_hung,
                            last_error = sample.last_error,
                            "UI-thread probe: no WM_NULL round-trip within \
                             {UI_THREAD_PROBE_TIMEOUT_MS}ms"
                        );
                    }
                    ui_wedge.observe(sample.pumping);
                }
                None => ui_wedge.observe_unknown(),
            }
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

/// Stop the health monitoring background task.
///
/// **Clears the wedge latches too, and that is not tidiness.** The atoms are
/// read by surfaces that outlive the monitor: `ui_error::native_ui_probe_verdict`
/// (→ every `derived_status` sink) and `mcp/ui_bridge/page.rs`'s
/// `event_loop_verdict` (→ the `close-request` 503). Leaving `UI_THREAD_WEDGED`
/// set with no thread left to ever clear it means those surfaces keep asserting
/// a wedge that nobody is observing any more — permanently, since only the
/// monitor's `Recovered` arm clears it. A stopped monitor knows nothing; the
/// honest state is UNKNOWN, which is what a cleared atom plus the
/// `is_running()` guard produces.
pub fn stop_health_monitor() {
    info!("Stopping health monitor");
    MONITOR_RUNNING.store(false, Ordering::SeqCst);
    WATCHDOG_STOP.store(true, Ordering::SeqCst);
    BACKEND_WEDGED.store(false, Ordering::SeqCst);
    UI_THREAD_WEDGED.store(false, Ordering::SeqCst);
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

    // ---- Phase 4: the native message-loop rung ----
    //
    // Same state machine, second rung. These pin the two things that make the
    // rung trustworthy: it shares the backend rung's noise floor exactly, and
    // it cannot be confused with it in the durable record.

    #[test]
    fn ui_thread_rung_escalates_on_the_same_noise_floor() {
        // A modal dialog or a large repaint blocks the loop for a moment. The
        // 3-consecutive-sample threshold is what keeps that from being an
        // incident, and it must be the SAME threshold the backend rung uses —
        // one argument, one constant, one place to change it.
        let mut d = WedgeDetector::new(WedgeKind::UiThread);
        for _ in 1..WEDGE_FAILURE_THRESHOLD {
            assert_eq!(d.step(false), WedgeAction::None, "escalated too early");
        }
        assert_eq!(
            d.step(false),
            WedgeAction::Escalate {
                consecutive_failures: WEDGE_FAILURE_THRESHOLD
            }
        );
        assert_eq!(d.step(false), WedgeAction::None);
    }

    #[test]
    fn one_pumping_sample_clears_the_ui_thread_streak() {
        // A UI thread that answers between two slow samples is busy, not hung.
        let mut d = WedgeDetector::new(WedgeKind::UiThread);
        for _ in 0..(WEDGE_FAILURE_THRESHOLD - 1) {
            d.step(false);
        }
        assert_eq!(d.step(true), WedgeAction::None);
        for _ in 0..(WEDGE_FAILURE_THRESHOLD - 1) {
            assert_eq!(d.step(false), WedgeAction::None, "streak did not reset");
        }
    }

    #[test]
    fn ui_thread_rung_reports_recovery_only_after_escalating() {
        let mut d = WedgeDetector::new(WedgeKind::UiThread);
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
    }

    #[test]
    fn the_two_rungs_do_not_share_a_streak() {
        // One wedged surface must never escalate the other. They are separate
        // detectors over one type, and the 2026-08-19 incident is exactly the
        // case where one is failing while the other is perfectly healthy.
        let mut backend = WedgeDetector::new(WedgeKind::Backend);
        let mut ui = WedgeDetector::new(WedgeKind::UiThread);
        for _ in 0..(WEDGE_FAILURE_THRESHOLD * 3) {
            assert_eq!(backend.step(true), WedgeAction::None);
        }
        for _ in 1..WEDGE_FAILURE_THRESHOLD {
            assert_eq!(ui.step(false), WedgeAction::None);
        }
        assert!(matches!(ui.step(false), WedgeAction::Escalate { .. }));
        assert_eq!(backend.step(true), WedgeAction::None);
    }

    #[test]
    fn breadcrumb_reasons_are_distinct_and_stable() {
        // `wedge-incidents.log` is the record that survives the restart which
        // truncates `runner-lifecycle.log`. These tokens are its grep keys.
        assert_eq!(WedgeKind::Backend.breadcrumb_reason(), "backend_wedged");
        assert_eq!(WedgeKind::UiThread.breadcrumb_reason(), "ui_thread_wedged");
        assert_ne!(
            WedgeKind::Backend.breadcrumb_reason(),
            WedgeKind::UiThread.breadcrumb_reason()
        );
    }

    #[test]
    fn the_default_rung_is_the_backend_rung() {
        // `WedgeDetector::default()` predates the second rung; it must keep
        // meaning what its existing call sites and tests mean.
        assert_eq!(WedgeDetector::default().kind, WedgeKind::Backend);
    }

    #[test]
    fn the_probes_cannot_dominate_the_sample_cadence() {
        // The predecessor of this test asserted only `3000 < 5000` while its
        // comment claimed the probe "does not stretch the cadence of the loop
        // it rides on". Those are different statements, and the second is the
        // one that matters: the loop's real inter-sample gap is the sleep PLUS
        // both probes, so a timeout under the interval bounds nothing on its
        // own. Assert the property the comment claims.
        assert!(
            (UI_THREAD_PROBE_TIMEOUT_MS as u64) < SELF_PROBE_INTERVAL_SECS * 1_000,
            "ui-thread probe timeout {UI_THREAD_PROBE_TIMEOUT_MS}ms is not under the \
             {SELF_PROBE_INTERVAL_SECS}s sample interval"
        );

        // Worst case per iteration: sleep + a /livez probe that burns its full
        // timeout + a UI probe that burns its full timeout.
        let worst_case_gap_ms = SELF_PROBE_INTERVAL_SECS * 1_000
            + SELF_PROBE_TIMEOUT_SECS * 1_000
            + UI_THREAD_PROBE_TIMEOUT_MS as u64;
        assert!(
            worst_case_gap_ms <= 3 * SELF_PROBE_INTERVAL_SECS * 1_000,
            "the worst-case inter-sample gap is {worst_case_gap_ms}ms — over 3x the nominal \
             {SELF_PROBE_INTERVAL_SECS}s cadence, so the clock behind every reported \
             duration has drifted from the constant that names it"
        );
    }

    #[test]
    fn a_reported_duration_prefers_the_measured_elapsed_over_the_estimate() {
        // FINDING 8. `consecutive_failures x SELF_PROBE_INTERVAL_SECS` is a
        // LOWER BOUND, not the duration: the gap between samples is the sleep
        // plus up to `UI_THREAD_PROBE_TIMEOUT_MS`, which is how the runtime
        // gate measured ~17.2s for a wedge the formula called 15s. The number
        // reaches `wedge-incidents.log` and a user-visible notification, so it
        // must be the measured span whenever one exists.
        let estimate = WEDGE_FAILURE_THRESHOLD as u64 * SELF_PROBE_INTERVAL_SECS;
        assert_eq!(
            reported_unresponsive_secs(None, WEDGE_FAILURE_THRESHOLD),
            estimate,
            "with no stamp the estimate is all there is"
        );

        // A streak that demonstrably started longer ago than the estimate must
        // report the longer, true figure.
        let long_ago = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(estimate + 7))
            .expect("Instant arithmetic");
        let measured = reported_unresponsive_secs(Some(long_ago), WEDGE_FAILURE_THRESHOLD);
        assert!(
            measured >= estimate + 7,
            "reported {measured}s must be the measured span, not the {estimate}s estimate"
        );
    }

    #[test]
    fn an_unknown_probe_releases_a_latched_wedge_but_is_not_a_recovery() {
        // FINDING 6(a). `observe()` runs only for a REAL sample, so before
        // `step_unknown` a rung that escalated and then lost its probe could
        // never reach the `Recovered` arm again: `derived_status` stayed
        // pinned `errored` and `close-request` answered 503 for the rest of
        // the process's life, even after the loop came back.
        let mut d = WedgeDetector::new(WedgeKind::UiThread);
        for _ in 0..WEDGE_FAILURE_THRESHOLD {
            d.step(false);
        }
        assert!(d.escalated, "precondition: the rung is latched");

        let action = d.step_unknown();
        assert_eq!(
            action,
            WedgeAction::LatchReleasedUnknown {
                was_failing_for: WEDGE_FAILURE_THRESHOLD
            },
            "an UNKNOWN sample must release the latch for want of evidence"
        );
        assert!(!d.escalated, "the latch must be gone");
        assert_eq!(d.consecutive_failures, 0);

        // …and it is DISTINCT from a recovery, which is a positive
        // observation. Collapsing the two would report an outage as having
        // ended when nothing was seen at all.
        assert_ne!(
            action,
            WedgeAction::Recovered {
                was_failing_for: WEDGE_FAILURE_THRESHOLD
            }
        );
    }

    #[test]
    fn unknown_never_escalates_and_never_manufactures_a_release() {
        // The other half of the contract: UNKNOWN must not accumulate toward
        // an escalation (that is how a non-Windows build, UNKNOWN forever,
        // would otherwise declare a permanent hang), and on a rung that never
        // latched it must be silent.
        let mut d = WedgeDetector::new(WedgeKind::UiThread);
        for _ in 0..(WEDGE_FAILURE_THRESHOLD * 10) {
            assert_eq!(d.step_unknown(), WedgeAction::None);
        }
        assert!(!d.escalated);

        // Sub-threshold failures followed by UNKNOWN: the streak is dropped,
        // silently, because nothing was ever escalated.
        for _ in 0..(WEDGE_FAILURE_THRESHOLD - 1) {
            d.step(false);
        }
        assert_eq!(d.step_unknown(), WedgeAction::None);
        assert_eq!(d.consecutive_failures, 0);
    }

    #[test]
    fn stopping_the_monitor_clears_the_wedge_latches() {
        // FINDING 6(b). `stop_health_monitor` cleared `MONITOR_RUNNING` and
        // left the atoms set, so `event_loop_verdict()` answered
        // `Wedged("wedge_detector_latched")` off a DEAD monitor — refusing
        // every close forever — while `/health`, which additionally checks
        // `is_running()`, correctly reported UNKNOWN. Two readers, two
        // answers, one atom.
        let was_running = is_running();
        BACKEND_WEDGED.store(true, Ordering::SeqCst);
        UI_THREAD_WEDGED.store(true, Ordering::SeqCst);
        assert!(backend_wedged() && ui_thread_wedged(), "precondition");

        stop_health_monitor();

        assert!(
            !ui_thread_wedged(),
            "a stopped monitor must not keep asserting a UI-thread wedge nobody is watching"
        );
        assert!(!backend_wedged(), "same for the backend rung");
        MONITOR_RUNNING.store(was_running, Ordering::SeqCst);
    }

    #[test]
    fn ui_thread_detection_latency_stays_in_the_tens_of_seconds() {
        // Worst case is threshold x (sleep + probe timeout): the probe runs
        // after the sleep, and a hung loop costs the full timeout unless
        // SMTO_ABORTIFHUNG short-circuits it (which is the common case, and
        // faster). Guard the arithmetic so a later constant bump cannot
        // quietly restore a coarse clock — there is no other detector for
        // this failure to fall back on.
        let worst_case_secs = WEDGE_FAILURE_THRESHOLD as u64
            * (SELF_PROBE_INTERVAL_SECS + (UI_THREAD_PROBE_TIMEOUT_MS as u64).div_ceil(1_000));
        assert!(
            worst_case_secs <= 30,
            "ui-thread wedge detection latency regressed to {worst_case_secs}s"
        );
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
