//! Health Monitoring Module
//!
//! Provides periodic health monitoring to detect gradual resource exhaustion
//! that could lead to application crashes. Logs memory usage, thread count,
//! and other system metrics.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
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

/// Flag to control the health monitor
static MONITOR_RUNNING: AtomicBool = AtomicBool::new(false);

/// Set while the backend is believed wedged, so other in-process surfaces can
/// read it without waiting on the monitor thread.
static BACKEND_WEDGED: AtomicBool = AtomicBool::new(false);

/// Whether the monitor currently believes the HTTP surface is wedged.
///
/// Read by the UI/status surfaces; never blocks.
pub fn backend_wedged() -> bool {
    BACKEND_WEDGED.load(Ordering::SeqCst)
}

/// Blocking `/livez` probe.
///
/// Deliberately a plain blocking HTTP call on this OS thread, NOT a
/// `block_on` against the app runtime: routing the probe through the runtime
/// under test would make it hang exactly when it needs to report, and would
/// consume a worker from the pool it is trying to measure.
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

    /// Fold in an observation and perform the escalation side effects.
    fn observe(&mut self, alive: bool) {
        match self.step(alive) {
            WedgeAction::None => {}
            WedgeAction::Escalate {
                consecutive_failures,
            }
            | WedgeAction::ReEscalate {
                consecutive_failures,
            } => {
                BACKEND_WEDGED.store(true, Ordering::SeqCst);
                let secs = consecutive_failures as u64 * SELF_PROBE_INTERVAL_SECS;
                error!(
                    consecutive_failures,
                    unresponsive_for_secs = secs,
                    "BACKEND WEDGED: /livez has not answered for {secs}s. The HTTP surface is \
                     not serving — every agent session on this box is affected, and coord-mcp \
                     writes issued through this runner are being LOST. Not restarting: a \
                     process restart destroys in-flight sessions and is an explicit non-goal.",
                );
                write_wedge_breadcrumb(secs);
            }
            WedgeAction::Recovered { was_failing_for } => {
                BACKEND_WEDGED.store(false, Ordering::SeqCst);
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
         livez_probe_every: {}s)",
        HEALTH_CHECK_INTERVAL_SECS,
        MEMORY_WARNING_THRESHOLD_MB,
        THREAD_WARNING_THRESHOLD,
        SELF_PROBE_INTERVAL_SECS
    );

    // Spawn the background task.
    //
    // A DEDICATED OS THREAD, not a tokio task — and that is load-bearing, not
    // incidental. This thread is the only thing that keeps observing when the
    // async runtime it is watching is starved, which is exactly the condition
    // it exists to detect. Measured on 2026-08-08: during a 25-minute wedge
    // that silenced the entire HTTP surface, this loop kept its 60s cadence
    // throughout. Never move it onto the runtime it monitors.
    std::thread::spawn(|| {
        let mut wedge = WedgeDetector::default();
        // Probe several times per metrics tick: 60s is a fine cadence for a
        // leak trend and far too coarse for an outage clock.
        let probes_per_metrics_tick =
            (HEALTH_CHECK_INTERVAL_SECS / SELF_PROBE_INTERVAL_SECS).max(1);

        while MONITOR_RUNNING.load(Ordering::SeqCst) {
            let metrics = collect_metrics();
            log_metrics(&metrics);

            for _ in 0..probes_per_metrics_tick {
                if !MONITOR_RUNNING.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_secs(SELF_PROBE_INTERVAL_SECS));
                wedge.observe(probe_livez_blocking());
            }
        }

        info!("Health monitor stopped");
    });
}

/// Stop the health monitoring background task
pub fn stop_health_monitor() {
    info!("Stopping health monitor");
    MONITOR_RUNNING.store(false, Ordering::SeqCst);
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
}
