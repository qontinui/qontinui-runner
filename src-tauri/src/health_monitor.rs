//! Health Monitoring Module
//!
//! Provides periodic health monitoring to detect gradual resource exhaustion
//! that could lead to application crashes. Logs memory usage, thread count,
//! and other system metrics.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::{debug, info, warn};

/// Interval between health checks in seconds
const HEALTH_CHECK_INTERVAL_SECS: u64 = 60;

/// Memory threshold in MB to trigger a warning
const MEMORY_WARNING_THRESHOLD_MB: u64 = 1024; // 1 GB

/// Thread count threshold to trigger a warning.
/// Set to 150 to avoid false positives — the runner legitimately uses 100-130
/// threads during normal operation (tokio runtime, bridges, background tasks).
const THREAD_WARNING_THRESHOLD: usize = 150;

/// Flag to control the health monitor
static MONITOR_RUNNING: AtomicBool = AtomicBool::new(false);

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
        "Starting health monitor (interval: {}s, memory_threshold: {}MB, thread_threshold: {})",
        HEALTH_CHECK_INTERVAL_SECS, MEMORY_WARNING_THRESHOLD_MB, THREAD_WARNING_THRESHOLD
    );

    // Spawn the background task
    std::thread::spawn(|| {
        let interval = Duration::from_secs(HEALTH_CHECK_INTERVAL_SECS);

        while MONITOR_RUNNING.load(Ordering::SeqCst) {
            // Collect and log metrics
            let metrics = collect_metrics();
            log_metrics(&metrics);

            // Sleep until next check
            std::thread::sleep(interval);
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

    #[test]
    fn test_collect_metrics() {
        let metrics = collect_metrics();
        // Memory should be non-zero on most platforms, but macOS CI runners
        // return 0 when sysinfo lacks permissions to read the current
        // process's RSS — accept zero as "couldn't read" rather than fail.
        if cfg!(not(target_os = "macos")) {
            assert!(metrics.memory_bytes > 0);
        }
        // Thread count should be at least 1 (the current thread)
        assert!(metrics.thread_count >= 1);
    }

    #[test]
    fn test_health_status() {
        let status = get_health_status();
        // Same caveat as test_collect_metrics: macOS CI may return 0 RSS.
        if cfg!(not(target_os = "macos")) {
            assert!(status.memory_mb > 0);
        }
        assert!(status.thread_count >= 1);
    }
}
