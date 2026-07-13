use chrono::Local;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::Level;
use tracing_appender::{non_blocking, rolling};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter, Registry,
};

use crate::tracing_layers::{JsonlSpanLayer, SpanLayerConfig};

/// Flag to track if we're already handling a crash (prevent recursive crashes)
static CRASH_HANDLING: AtomicBool = AtomicBool::new(false);

/// Re-entrancy guard for the `runner-panic.log` write inside `log_panic`.
/// A panic raised while writing the panic log re-enters the hook; without
/// this latch that recursion would be unbounded (each nested panic runs the
/// hook synchronously before unwinding).
static PANIC_LOG_WRITING: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Path of the `crash_*.txt` this thread's panic hook wrote most recently.
    ///
    /// The panic HOOK runs before unwinding, so it cannot know whether the
    /// panic is about to be caught. An HTTP handler panic IS caught (by
    /// `CatchPanicLayer`, which turns it into a 500 JSON envelope and lets the
    /// process live) — but the dump is on disk by then, and the next startup's
    /// [`crate::crash_dumps`] scan adopts any fresh `crash_*.txt` as a
    /// `recent_crash`, badging a runner that never actually died as `errored`.
    ///
    /// `catch_unwind` resumes on the SAME thread that panicked, so the catch
    /// handler can hand this path to [`retract_last_crash_dump`]. A thread-local
    /// keeps concurrent panics on other worker threads from retracting each
    /// other's dumps.
    static LAST_CRASH_DUMP: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Downgrade the crash dump this thread's panic hook just wrote from
/// `crash_*.txt` to `caught_*.txt`, because the panic was caught and the
/// process is still alive.
///
/// The forensics survive under the new name (and the `runner-panic.log` entry
/// is left alone); only the `crash_` prefix that `crash_dumps`'
/// `is_crash_dump_filename` keys on is dropped, so the startup scan no longer
/// mistakes a handled 500 for a process death.
///
/// Returns `true` if a dump was actually renamed.
pub fn retract_last_crash_dump(reason: &str) -> bool {
    LAST_CRASH_DUMP.with(|cell| {
        let Some(path) = cell.borrow_mut().take() else {
            return false;
        };
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        let Some(rest) = name.strip_prefix("crash_") else {
            return false;
        };
        let caught = path.with_file_name(format!("caught_{rest}"));
        match std::fs::rename(&path, &caught) {
            Ok(()) => {
                safe_eprintln(&format!(
                    "Panic was caught ({reason}) — crash dump downgraded to {caught:?}"
                ));
                true
            }
            Err(e) => {
                safe_eprintln(&format!(
                    "Failed to downgrade caught-panic crash dump {path:?}: {e}"
                ));
                false
            }
        }
    })
}

/// Safely write to stderr, ignoring errors if the pipe is closed.
/// This prevents panics when stderr is unavailable
/// (e.g., when the parent terminal/process has closed the pipe - Windows error 232).
///
/// # Usage
/// ```
/// use crate::logging::safe_eprintln;
/// safe_eprintln("Error message");
/// safe_eprintln(&format!("Value: {}", some_value));
/// ```
pub fn safe_eprintln(msg: &str) {
    use std::io::Write;
    // Use write_all instead of eprintln! to avoid panicking on broken pipe
    let _ = std::io::stderr().write_all(msg.as_bytes());
    let _ = std::io::stderr().write_all(b"\n");
    let _ = std::io::stderr().flush();
}

/// Macro for safe stderr printing with format args, similar to eprintln!
/// This is the preferred way to write debug/error output in the runner.
#[macro_export]
macro_rules! safe_eprintln {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let msg = format!($($arg)*);
        let _ = std::io::stderr().write_all(msg.as_bytes());
        let _ = std::io::stderr().write_all(b"\n");
        let _ = std::io::stderr().flush();
    }};
}

pub struct LoggingConfig {
    pub level: Level,
    pub log_to_file: bool,
    pub log_to_console: bool,
    pub log_dir: PathBuf,
    /// Enable JSONL span output for real-time debugging
    pub enable_span_jsonl: bool,
    /// OpenTelemetry configuration, read from persisted settings at startup.
    /// The tracing subscriber is set once, so OTel changes require a restart.
    pub otel: crate::otel::OtelConfig,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        // Scoped per-runner for secondary instances so concurrent runners
        // don't interleave log lines in the same daily-rolling file.
        let base = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("qontinui-runner");
        let log_dir = crate::instance::scope_path(&base).join("logs");

        Self {
            level: Level::INFO,
            log_to_file: true,
            log_to_console: cfg!(debug_assertions),
            log_dir,
            enable_span_jsonl: true,
            otel: crate::otel::OtelConfig::default(),
        }
    }
}

/// Result of logging initialization.
pub struct LoggingInitResult {
    /// OTel guard — keeps the TracerProvider alive; drops on shutdown.
    pub _otel_guard: crate::otel::OtelGuard,
}

pub fn init_logging(config: LoggingConfig) -> anyhow::Result<LoggingInitResult> {
    std::fs::create_dir_all(&config.log_dir)?;

    // Clear the spans JSONL file on startup
    crate::tracing_layers::clear_spans_jsonl();

    // Initialise OpenTelemetry from persisted settings.
    // The guard must outlive the subscriber so the provider flushes on shutdown.
    // The optional layer is `None` when OTel is disabled — tracing-subscriber
    // treats `Option<Layer>` as a no-op in that case.
    let (otel_guard, otel_layer) = crate::otel::init_otel(&config.otel);

    let env_filter = EnvFilter::new(
        std::env::var("RUST_LOG")
            .unwrap_or_else(|_| format!("qontinui_runner={},tauri=info", config.level)),
    );

    // Create span layer config
    let span_config = SpanLayerConfig {
        dev_logs_dir: crate::paths::get_dev_logs_dir(),
        enable_jsonl: config.enable_span_jsonl,
    };

    // Create the JSONL span layer
    let jsonl_layer = JsonlSpanLayer::new(span_config);

    // Store log_dir for logging before it's moved
    let log_dir_path = config.log_dir.clone();

    // Build the subscriber with all layers.
    // OTel layer is added first (directly on Registry) so its type matches.
    let registry = Registry::default().with(otel_layer).with(env_filter);

    if config.log_to_file {
        let file_appender = rolling::daily(config.log_dir, "qontinui-runner.log");
        let (non_blocking_file, _guard) = non_blocking(file_appender);

        let file_layer = fmt::layer()
            .with_writer(non_blocking_file)
            .with_ansi(false)
            .with_span_events(FmtSpan::CLOSE)
            .with_timer(fmt::time::ChronoLocal::new(
                "%Y-%m-%d %H:%M:%S%.3f".to_string(),
            ));

        let subscriber = registry.with(file_layer).with(jsonl_layer);

        if config.log_to_console {
            let console_layer = fmt::layer()
                .with_writer(std::io::stdout)
                .with_span_events(FmtSpan::CLOSE);

            subscriber.with(console_layer).init();
        } else {
            subscriber.init();
        }
    } else if config.log_to_console {
        let console_layer = fmt::layer()
            .with_writer(std::io::stdout)
            .with_span_events(FmtSpan::CLOSE);

        registry.with(console_layer).with(jsonl_layer).init();
    } else {
        registry.with(jsonl_layer).init();
    }

    tracing::info!("Logging initialized at level: {:?}", config.level);
    tracing::info!("Log directory: {:?}", log_dir_path);
    tracing::info!("JSONL span layer enabled: {}", config.enable_span_jsonl);
    tracing::info!("Application started at {}", Local::now());

    Ok(LoggingInitResult {
        _otel_guard: otel_guard,
    })
}

#[macro_export]
macro_rules! log_error {
    ($result:expr, $context:expr) => {
        $result.map_err(|e| {
            tracing::error!("Error in {}: {:?}", $context, e);
            e
        })
    };
}

#[macro_export]
macro_rules! log_warn {
    ($message:expr) => {
        tracing::warn!($message)
    };
    ($message:expr, $($arg:tt)*) => {
        tracing::warn!($message, $($arg)*)
    };
}

#[macro_export]
macro_rules! log_info {
    ($message:expr) => {
        tracing::info!($message)
    };
    ($message:expr, $($arg:tt)*) => {
        tracing::info!($message, $($arg)*)
    };
}

#[macro_export]
macro_rules! log_debug {
    ($message:expr) => {
        tracing::debug!($message)
    };
    ($message:expr, $($arg:tt)*) => {
        tracing::debug!($message, $($arg)*)
    };
}

/// Walk up from `exe_path` looking for an ancestor directory that already
/// contains a `.dev-logs` child, and return that child.
///
/// This replaces a fixed five-`.parent()` walk written for the
/// pre-workspace `src-tauri\target\debug\` exe layout. The workspace root
/// moved `target/` one level up (`qontinui-runner\target\debug\`), so the
/// fixed count overshot to the drive root — the 2026-07-03 primary crash
/// dumps landed at `D:\.dev-logs` instead of `D:\qontinui-root\.dev-logs`.
/// A marker-based search survives any future layout move (slot pools,
/// deeper target dirs, worktrees) as long as a `.dev-logs` dir exists
/// somewhere above the exe.
fn find_dev_logs_ancestor(exe_path: &std::path::Path) -> Option<PathBuf> {
    // `ancestors()` yields the path itself first — skip it (it's the exe
    // file, not a directory).
    exe_path.ancestors().skip(1).find_map(|dir| {
        let candidate = dir.join(".dev-logs");
        if candidate.is_dir() {
            Some(candidate)
        } else {
            None
        }
    })
}

/// Get the crash dump directory.
///
/// Scoped per-runner for secondary instances so the `latest_crash.txt` file
/// from one runner doesn't overwrite another's.
pub fn get_crash_dump_dir() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|exe_path| find_dev_logs_ancestor(&exe_path))
        .unwrap_or_else(|| {
            // Fallback to local app data
            dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("qontinui-runner")
                .join("crash-dumps")
        });

    crate::instance::scope_path(&base)
}

/// Write a crash dump file with detailed information
pub fn write_crash_dump(location: &str, message: &str, backtrace: &str) {
    // Prevent recursive crash handling
    if CRASH_HANDLING.swap(true, Ordering::SeqCst) {
        safe_eprintln("RECURSIVE CRASH DETECTED - skipping crash dump");
        return;
    }

    let crash_dir = get_crash_dump_dir();
    let _ = std::fs::create_dir_all(&crash_dir);

    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let crash_file = crash_dir.join(format!("crash_{}.txt", timestamp));

    let crash_content = format!(
        "=== QONTINUI RUNNER CRASH DUMP ===\n\
        Timestamp: {}\n\
        Version: {}\n\
        \n\
        === PANIC LOCATION ===\n\
        {}\n\
        \n\
        === PANIC MESSAGE ===\n\
        {}\n\
        \n\
        === ENVIRONMENT ===\n\
        OS: {}\n\
        Arch: {}\n\
        Current Dir: {:?}\n\
        Exe Path: {:?}\n\
        \n\
        === THREAD INFO ===\n\
        Thread: {:?}\n\
        \n\
        === BACKTRACE ===\n\
        {}\n\
        \n\
        === END CRASH DUMP ===\n",
        Local::now().to_rfc3339(),
        env!("CARGO_PKG_VERSION"),
        location,
        message,
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::current_dir().ok(),
        std::env::current_exe().ok(),
        std::thread::current().name().unwrap_or("unnamed"),
        backtrace,
    );

    // Write to file
    match std::fs::File::create(&crash_file) {
        Ok(mut file) => {
            let _ = file.write_all(crash_content.as_bytes());
            safe_eprintln(&format!("Crash dump written to: {:?}", crash_file));
            // Remember it so a catch handler on THIS thread can downgrade it if
            // the panic turns out to be caught. See `retract_last_crash_dump`.
            LAST_CRASH_DUMP.with(|cell| *cell.borrow_mut() = Some(crash_file.clone()));
        }
        Err(e) => {
            safe_eprintln(&format!("Failed to write crash dump: {}", e));
        }
    }

    // Also write to a fixed "latest_crash.txt" for easy access
    let latest_crash = crash_dir.join("latest_crash.txt");
    if let Ok(mut file) = std::fs::File::create(&latest_crash) {
        let _ = file.write_all(crash_content.as_bytes());
    }

    // Print to stderr as well (using safe write to avoid panic on closed pipe)
    safe_eprintln(&format!("\n{}", crash_content));

    CRASH_HANDLING.store(false, Ordering::SeqCst);
}

pub fn log_panic(info: &std::panic::PanicHookInfo) {
    // Wrap the entire panic logging in catch_unwind to prevent double-panics
    // This is critical for stability - a panic in the panic handler is catastrophic
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let location = if let Some(location) = info.location() {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        } else {
            "unknown location".to_string()
        };

        let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic payload".to_string()
        };

        let backtrace = format!("{:?}", std::backtrace::Backtrace::capture());

        // ALSO write the supervisor-visible `runner-panic.log`. This hook
        // REPLACES the early `startup_panic` hook at init, and the
        // supervisor's postmortem path (`check_and_record_panic_log` /
        // `parse_panic_file`) reads ONLY that file — before this call,
        // any post-init crash left `recent_panic: null` on `GET /runners`
        // (observed live 3x on 2026-07-03). `startup_panic::write_panic_log`
        // is deliberately self-contained: dir resolution via
        // `QONTINUI_RUNNER_LOG_DIR` (set by the supervisor) with a
        // data_local_dir fallback, and no settings reads that could panic
        // or poison a OnceLock mid-panic.
        if !PANIC_LOG_WRITING.swap(true, Ordering::SeqCst) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let thread_name = std::thread::current()
                    .name()
                    .unwrap_or("<unnamed>")
                    .to_string();
                crate::startup_panic::write_panic_log(
                    &message,
                    &location,
                    &thread_name,
                    &backtrace,
                );
            }));
            PANIC_LOG_WRITING.store(false, Ordering::SeqCst);
        }

        // Write crash dump to file
        write_crash_dump(&location, &message, &backtrace);

        // Also log to lifecycle debug
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::debug_lifecycle::log_panic_details(info);
        }));

        // Try to log via tracing (may fail if tracing is not initialized or broken)
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tracing::error!(
                "PANIC at {}: {}\nBacktrace:\n{}",
                location,
                message,
                backtrace
            );
        }));
    }));
}

pub fn setup_panic_handler() {
    std::panic::set_hook(Box::new(|info| {
        log_panic(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Create `dir` and every missing parent, panicking on failure (tests only).
    fn mk(dir: &Path) {
        std::fs::create_dir_all(dir).expect("create test dir");
    }

    /// All three deployed exe layouts must resolve to `<root>/.dev-logs`
    /// when it exists. The old fixed five-`.parent()` walk only handled the
    /// pre-workspace `src-tauri\target\debug\` layout and overshot to the
    /// drive root for the others (live evidence 2026-07-03: dumps at
    /// `D:\.dev-logs` instead of `D:\qontinui-root\.dev-logs`).
    #[test]
    fn find_dev_logs_ancestor_resolves_all_known_exe_layouts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let dev_logs = root.join(".dev-logs");
        mk(&dev_logs);

        let layouts = [
            // Workspace-root target (current deployed layout).
            root.join("qontinui-runner")
                .join("target")
                .join("debug")
                .join("qontinui-runner-primary.exe"),
            // Pre-workspace layout (the one the old fixed walk assumed).
            root.join("qontinui-runner")
                .join("src-tauri")
                .join("target")
                .join("debug")
                .join("qontinui-runner.exe"),
            // Supervisor build-pool slot layout.
            root.join("qontinui-runner")
                .join("target-pool")
                .join("slot-0")
                .join("debug")
                .join("qontinui-runner.exe"),
        ];

        for exe in &layouts {
            mk(exe.parent().unwrap());
            assert_eq!(
                find_dev_logs_ancestor(exe).as_deref(),
                Some(dev_logs.as_path()),
                "layout {:?} must resolve to the fixture .dev-logs",
                exe
            );
        }
    }

    /// The NEAREST ancestor with a `.dev-logs` child wins — a repo-local
    /// `.dev-logs` must shadow one further up the tree.
    #[test]
    fn find_dev_logs_ancestor_prefers_nearest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        mk(&root.join(".dev-logs"));
        let near = root.join("qontinui-runner").join(".dev-logs");
        mk(&near);

        let exe = root
            .join("qontinui-runner")
            .join("target")
            .join("debug")
            .join("qontinui-runner.exe");
        mk(exe.parent().unwrap());

        assert_eq!(
            find_dev_logs_ancestor(&exe).as_deref(),
            Some(near.as_path())
        );
    }

    /// A `.dev-logs` FILE (not dir) must not be picked up.
    #[test]
    fn find_dev_logs_ancestor_ignores_plain_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join(".dev-logs"), b"not a dir").unwrap();

        let exe = root
            .join("qontinui-runner")
            .join("target")
            .join("debug")
            .join("qontinui-runner.exe");
        mk(exe.parent().unwrap());

        // Either nothing is found, or (on a dev box that genuinely has a
        // `.dev-logs` dir above the temp dir) the hit must lie OUTSIDE the
        // fixture — never the plain file inside it.
        if let Some(found) = find_dev_logs_ancestor(&exe) {
            assert!(
                !found.starts_with(root),
                "must not resolve to the .dev-logs FILE inside the fixture: {:?}",
                found
            );
        }
    }

    /// A caught panic must leave NO `crash_*.txt` behind: the hook writes one
    /// before unwinding, and the catch handler downgrades it. The forensics
    /// survive under `caught_*`, which the startup scan ignores.
    #[test]
    fn retract_last_crash_dump_downgrades_crash_to_caught() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dump = tmp.path().join("crash_20260713_101500.txt");
        std::fs::write(&dump, "=== QONTINUI RUNNER CRASH DUMP ===").expect("write dump");
        LAST_CRASH_DUMP.with(|cell| *cell.borrow_mut() = Some(dump.clone()));

        assert!(retract_last_crash_dump("unit test"));

        let caught = tmp.path().join("caught_20260713_101500.txt");
        assert!(!dump.exists(), "the crash_ dump must be gone");
        assert!(caught.exists(), "it must survive as caught_");
        assert_eq!(
            std::fs::read_to_string(&caught).expect("read caught"),
            "=== QONTINUI RUNNER CRASH DUMP ===",
            "forensics must be preserved verbatim"
        );
    }

    /// Retraction consumes the record, so a second call cannot reach back and
    /// downgrade an unrelated later dump. Also covers the "hook never wrote
    /// one" path (e.g. the recursive-crash guard tripped).
    #[test]
    fn retract_last_crash_dump_is_noop_when_nothing_was_written() {
        LAST_CRASH_DUMP.with(|cell| *cell.borrow_mut() = None);
        assert!(!retract_last_crash_dump("unit test"));
    }

    /// A panic that killed the process leaves its dump alone — only the catch
    /// handler retracts, and it never runs on that path. Guards against a
    /// future refactor pointing the thread-local at a real crash.
    #[test]
    fn retract_last_crash_dump_ignores_a_non_crash_filename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let other = tmp.path().join("latest_crash.txt");
        std::fs::write(&other, "x").expect("write");
        LAST_CRASH_DUMP.with(|cell| *cell.borrow_mut() = Some(other.clone()));

        assert!(!retract_last_crash_dump("unit test"));
        assert!(other.exists(), "a non-`crash_` file must not be renamed");
    }
}
