//! Debug Lifecycle Tracking
//!
//! This module provides comprehensive debugging for tracking the runner's lifecycle,
//! including heartbeat files, thread monitoring, and crash detection.

use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Global flag to track if heartbeat is running
static HEARTBEAT_RUNNING: AtomicBool = AtomicBool::new(false);

/// Counter for heartbeat iterations
static HEARTBEAT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Get the debug log directory
fn get_debug_dir() -> PathBuf {
    PathBuf::from(r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs")
}

/// Write a debug message to the lifecycle log
pub fn log_lifecycle(category: &str, message: &str) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let log_line = format!("[{}] [{}] {}\n", timestamp, category, message);

    // Write to file
    let log_path = get_debug_dir().join("runner-lifecycle.log");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
        let _ = file.write_all(log_line.as_bytes());
    }

    // Also write to the safe_eprintln for console output
    crate::safe_eprintln!("[LIFECYCLE] [{}] {}", category, message);
}

/// Initialize lifecycle debugging - call this at app startup
pub fn init_lifecycle_debug() {
    let debug_dir = get_debug_dir();
    let _ = fs::create_dir_all(&debug_dir);

    // Clear the lifecycle log for this session
    let log_path = debug_dir.join("runner-lifecycle.log");
    let _ = fs::write(&log_path, "");

    log_lifecycle(
        "STARTUP",
        &format!("Runner starting - PID: {}", std::process::id()),
    );
    log_lifecycle(
        "STARTUP",
        &format!("Version: {}", env!("CARGO_PKG_VERSION")),
    );
    log_lifecycle(
        "STARTUP",
        &format!("Debug build: {}", cfg!(debug_assertions)),
    );

    // Write startup timestamp to a separate file for easy checking
    let startup_path = debug_dir.join("runner-startup.txt");
    let startup_info = format!(
        "Started: {}\nPID: {}\nVersion: {}\n",
        Local::now().to_rfc3339(),
        std::process::id(),
        env!("CARGO_PKG_VERSION")
    );
    let _ = fs::write(&startup_path, &startup_info);

    // Start heartbeat thread
    start_heartbeat();
}

/// Start the heartbeat thread that writes periodic updates
fn start_heartbeat() {
    if HEARTBEAT_RUNNING.swap(true, Ordering::SeqCst) {
        // Already running
        return;
    }

    thread::spawn(move || {
        log_lifecycle("HEARTBEAT", "Heartbeat thread started");
        let heartbeat_path = get_debug_dir().join("runner-heartbeat.txt");
        let start_time = Instant::now();

        loop {
            let count = HEARTBEAT_COUNT.fetch_add(1, Ordering::SeqCst);
            let elapsed = start_time.elapsed();

            // Write heartbeat file
            let heartbeat_info = format!(
                "Heartbeat: {}\nTimestamp: {}\nUptime: {:.1}s\nPID: {}\nCount: {}\n",
                Local::now().to_rfc3339(),
                Local::now().format("%H:%M:%S%.3f"),
                elapsed.as_secs_f64(),
                std::process::id(),
                count
            );
            let _ = fs::write(&heartbeat_path, &heartbeat_info);

            // Log every 30 seconds to lifecycle log
            if count % 6 == 0 {
                log_lifecycle(
                    "HEARTBEAT",
                    &format!(
                        "Still alive - uptime: {:.0}s, count: {}",
                        elapsed.as_secs_f64(),
                        count
                    ),
                );
            }

            thread::sleep(Duration::from_secs(5));

            // Check if we should stop
            if !HEARTBEAT_RUNNING.load(Ordering::SeqCst) {
                log_lifecycle("HEARTBEAT", "Heartbeat thread stopping");
                break;
            }
        }
    });
}

/// Stop the heartbeat thread
pub fn stop_heartbeat() {
    HEARTBEAT_RUNNING.store(false, Ordering::SeqCst);
    log_lifecycle("SHUTDOWN", "Heartbeat stop requested");
}

/// Log when a thread starts
pub fn log_thread_start(thread_name: &str) {
    log_lifecycle("THREAD", &format!("Started: {}", thread_name));
}

/// Log when a thread ends
pub fn log_thread_end(thread_name: &str, reason: &str) {
    log_lifecycle("THREAD", &format!("Ended: {} - {}", thread_name, reason));
}

/// Log an AI session event
pub fn log_ai_session(event: &str, details: &str) {
    log_lifecycle("AI_SESSION", &format!("{}: {}", event, details));
}

/// Log a Claude CLI event
pub fn log_claude_cli(event: &str, details: &str) {
    log_lifecycle("CLAUDE_CLI", &format!("{}: {}", event, details));
}

/// Log when the runner is about to exit
pub fn log_exit(reason: &str, exit_code: i32) {
    log_lifecycle("EXIT", &format!("Reason: {}, Code: {}", reason, exit_code));

    // Write exit info to a separate file
    let exit_path = get_debug_dir().join("runner-exit.txt");
    let exit_info = format!(
        "Exited: {}\nReason: {}\nExit Code: {}\nPID: {}\nHeartbeat Count: {}\n",
        Local::now().to_rfc3339(),
        reason,
        exit_code,
        std::process::id(),
        HEARTBEAT_COUNT.load(Ordering::SeqCst)
    );
    let _ = fs::write(&exit_path, &exit_info);

    stop_heartbeat();
}

/// Enhanced panic handler that logs detailed information
pub fn log_panic_details(info: &std::panic::PanicHookInfo) {
    let location = if let Some(loc) = info.location() {
        format!("{}:{}:{}", loc.file(), loc.line(), loc.column())
    } else {
        "unknown".to_string()
    };

    let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "Unknown panic payload".to_string()
    };

    let thread_name = thread::current().name().unwrap_or("unnamed").to_string();

    log_lifecycle("PANIC", &format!("Thread: {}", thread_name));
    log_lifecycle("PANIC", &format!("Location: {}", location));
    log_lifecycle("PANIC", &format!("Message: {}", message));

    // Write panic details to a separate file
    let panic_path = get_debug_dir().join("runner-last-panic.txt");
    let panic_info = format!(
        "Panic at: {}\nThread: {}\nLocation: {}\nMessage: {}\nPID: {}\nHeartbeat Count: {}\n\nBacktrace:\n{:?}\n",
        Local::now().to_rfc3339(),
        thread_name,
        location,
        message,
        std::process::id(),
        HEARTBEAT_COUNT.load(Ordering::SeqCst),
        std::backtrace::Backtrace::capture()
    );
    let _ = fs::write(&panic_path, &panic_info);
}

/// Wrapper for tracking async task lifecycle
#[allow(dead_code)]
pub struct TaskTracker {
    name: String,
    start_time: Instant,
}

#[allow(dead_code)]
impl TaskTracker {
    pub fn new(name: &str) -> Self {
        log_lifecycle("TASK", &format!("Started: {}", name));
        Self {
            name: name.to_string(),
            start_time: Instant::now(),
        }
    }

    pub fn checkpoint(&self, message: &str) {
        log_lifecycle(
            "TASK",
            &format!(
                "{}: {} (elapsed: {:.1}s)",
                self.name,
                message,
                self.start_time.elapsed().as_secs_f64()
            ),
        );
    }
}

impl Drop for TaskTracker {
    fn drop(&mut self) {
        log_lifecycle(
            "TASK",
            &format!(
                "Ended: {} (duration: {:.1}s)",
                self.name,
                self.start_time.elapsed().as_secs_f64()
            ),
        );
    }
}
