//! Startup-panic telemetry.
//!
//! Installs a `std::panic::set_hook` at the very top of `main()` — before
//! anything else — so panics that fire during early init (DB connect, Tauri
//! builder, axum router construction, etc.) are captured to a file the
//! supervisor can read after the process exits.
//!
//! # Why this module exists
//!
//! Before this file existed, a panic during early `main()` — e.g. an
//! axum-0.8 router built with the old `:app_id` path syntax — would kill
//! the process ~6 seconds after spawn with no log trail. The supervisor
//! saw only "process exited 1." The panic payload went nowhere because
//! `logging::setup_panic_handler` runs *inside* `run_app()` after the
//! tracing subscriber is initialised — panics before that point are
//! printed to stderr (which the supervisor's stderr reader may or may not
//! have captured yet) and then lost.
//!
//! This module plugs the gap: install a minimal, self-contained hook
//! that writes a single file — `<log-dir>/runner-panic.log` — and then
//! chains to whatever hook was previously installed (which is the default
//! stderr printer during this early window). Later, `logging::setup_panic_handler`
//! overwrites the hook with the richer crash-dump version; by then early
//! init has succeeded and the simpler hook has done its job. The late hook
//! (`logging::log_panic`) calls back into [`write_panic_log`] so post-init
//! panics keep producing the supervisor-parseable `runner-panic.log` too —
//! before 2026-07-03 they didn't, and a crashed primary showed
//! `recent_panic: null` on the supervisor's `/runners`.
//!
//! # File semantics
//!
//! The log file is a *last crash* record, not a history: each startup
//! truncates it on install. If the runner boots successfully, the file
//! stays there until the next panic overwrites it. The supervisor uses
//! mtime-within-60s of exit to decide whether a given exit corresponds
//! to the panic on disk.
//!
//! # Log dir resolution
//!
//! 1. `QONTINUI_RUNNER_LOG_DIR` env var (supervisor sets this when spawning).
//! 2. `<LOCALAPPDATA>/qontinui-runner/dev-logs` (Windows) or the platform
//!    equivalent via `dirs::data_local_dir()`.
//!
//! We deliberately do *not* consult `crate::paths::get_dev_logs_dir()`
//! here because that helper reads settings — which may itself panic
//! during early init — and because it caches inside a `OnceLock` that
//! we'd prefer not to poison with an early-panic call.

use chrono::Local;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// File name inside the log dir. One file per startup — overwritten.
pub const PANIC_LOG_FILENAME: &str = "runner-panic.log";

/// Re-entrancy guard so a panic *inside* the panic hook doesn't recurse.
static PANIC_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Resolve the directory where the panic log should be written.
///
/// Resolution order matches the module docs: env var first, then default.
pub fn resolve_log_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("QONTINUI_RUNNER_LOG_DIR") {
        if !custom.trim().is_empty() {
            return PathBuf::from(custom);
        }
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qontinui-runner")
        .join("dev-logs")
}

/// Full path to the panic log file.
pub fn panic_log_path() -> PathBuf {
    resolve_log_dir().join(PANIC_LOG_FILENAME)
}

/// Build the panic-log body from its parts. Exposed for tests.
pub fn format_panic_log(
    payload: &str,
    location: &str,
    thread_name: &str,
    backtrace: &str,
) -> String {
    let runner_id = std::env::var("QONTINUI_RUNNER_ID")
        .or_else(|_| std::env::var("QONTINUI_INSTANCE_NAME"))
        .unwrap_or_else(|_| "primary".to_string());
    let pid = std::process::id();
    let version = env!("CARGO_PKG_VERSION");
    let ts = Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    format!(
        "[{ts}] PANIC runner-id={runner_id} pid={pid} version={version}\n\
         location: {location}\n\
         thread: {thread_name}\n\
         payload:\n\
         {payload}\n\
         \n\
         backtrace:\n\
         {backtrace}\n"
    )
}

/// Write the formatted panic body to `<log-dir>/runner-panic.log`, creating
/// the directory if needed. Overwrites any existing file (the log is a
/// last-crash record, not a history).
///
/// Exposed as a free function so tests can exercise the write path without
/// actually panicking.
pub fn write_panic_log(payload: &str, location: &str, thread_name: &str, backtrace: &str) {
    let body = format_panic_log(payload, location, thread_name, backtrace);
    let dir = resolve_log_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        // Best-effort stderr — we may be mid-panic, stderr may be broken.
        let _ = writeln!(
            std::io::stderr(),
            "[startup_panic] failed to create log dir {:?}: {}",
            dir,
            e
        );
        return;
    }
    let path = dir.join(PANIC_LOG_FILENAME);
    match std::fs::File::create(&path) {
        Ok(mut f) => {
            let _ = f.write_all(body.as_bytes());
            let _ = f.flush();
            let _ = writeln!(std::io::stderr(), "[startup_panic] wrote {:?}", path);
        }
        Err(e) => {
            let _ = writeln!(
                std::io::stderr(),
                "[startup_panic] failed to write {:?}: {}",
                path,
                e
            );
        }
    }
}

/// Install the process-wide panic hook. MUST be called before any other
/// setup in `main()` so panics during early init — DB connect, Tauri
/// builder, axum router construction — are captured.
///
/// The previous hook (usually the default stderr printer at this point)
/// is saved and invoked after we write the log, so developers still see
/// the panic on their terminal.
pub fn install_startup_panic_hook() {
    // Ensure backtraces are captured; no-op if already set by main().
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Re-entrancy guard — a panic inside this hook must not recurse.
        if PANIC_IN_PROGRESS.swap(true, Ordering::SeqCst) {
            let _ = writeln!(
                std::io::stderr(),
                "[startup_panic] recursive panic; skipping file write"
            );
            previous(info);
            return;
        }

        // Wrap the entire write in catch_unwind so a broken log dir
        // can't turn a single panic into a double panic + abort.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let location = info
                .location()
                .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
                .unwrap_or_else(|| "unknown".to_string());

            let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = info.payload().downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic payload>".to_string()
            };

            let thread_name = std::thread::current()
                .name()
                .unwrap_or("<unnamed>")
                .to_string();

            let backtrace = format!("{}", std::backtrace::Backtrace::force_capture());

            write_panic_log(&payload, &location, &thread_name, &backtrace);
        }));

        PANIC_IN_PROGRESS.store(false, Ordering::SeqCst);

        // Chain — default handler prints to stderr during dev, which we
        // want to preserve for interactive debugging.
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests in this module manipulate process-wide env vars
    /// (`QONTINUI_RUNNER_LOG_DIR`, `QONTINUI_RUNNER_ID`), so they must be
    /// serialised against each other. `cargo test` runs tests in a
    /// multi-threaded pool by default; without this guard a set-var in one
    /// test races with a remove-var in another and the file-creation
    /// tests intermittently write to the wrong directory.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Scope-guard that clears the env vars this module touches on drop,
    /// so a test-panic can't leak state into sibling tests.
    struct EnvGuard;
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var("QONTINUI_RUNNER_LOG_DIR");
            std::env::remove_var("QONTINUI_RUNNER_ID");
            std::env::remove_var("QONTINUI_INSTANCE_NAME");
        }
    }

    #[test]
    fn format_panic_log_includes_all_fields() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard;
        std::env::set_var("QONTINUI_RUNNER_ID", "test-runner-42");
        let body = format_panic_log(
            "boom",
            "src/foo.rs:10:5",
            "test-thread",
            "0: frame_a\n1: frame_b",
        );
        assert!(body.contains("PANIC"), "missing PANIC marker: {}", body);
        assert!(
            body.contains("runner-id=test-runner-42"),
            "missing runner-id: {}",
            body
        );
        assert!(
            body.contains("location: src/foo.rs:10:5"),
            "missing location: {}",
            body
        );
        assert!(body.contains("thread: test-thread"));
        assert!(body.contains("payload:\nboom"));
        assert!(body.contains("backtrace:\n0: frame_a"));
        assert!(
            body.contains(&format!("version={}", env!("CARGO_PKG_VERSION"))),
            "missing version: {}",
            body
        );
    }

    #[test]
    fn resolve_log_dir_honors_env_var() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard;
        let tmp = std::env::temp_dir().join("qontinui-startup-panic-test-env");
        std::env::set_var("QONTINUI_RUNNER_LOG_DIR", &tmp);
        assert_eq!(resolve_log_dir(), tmp);
    }

    #[test]
    fn resolve_log_dir_ignores_empty_env_var() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard;
        std::env::set_var("QONTINUI_RUNNER_LOG_DIR", "   ");
        let dir = resolve_log_dir();
        assert!(
            !dir.as_os_str().is_empty(),
            "whitespace env should fall through to default"
        );
    }

    #[test]
    fn write_panic_log_creates_file_with_expected_content() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard;
        let tmp = std::env::temp_dir().join("qontinui-startup-panic-test-write");
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("QONTINUI_RUNNER_LOG_DIR", &tmp);
        std::env::set_var("QONTINUI_RUNNER_ID", "unit-test");

        write_panic_log(
            "expected panic message",
            "src/bar.rs:33:1",
            "main",
            "0: unit_test_frame",
        );

        let path = tmp.join(PANIC_LOG_FILENAME);
        assert!(path.exists(), "panic log should exist at {:?}", path);
        let contents = std::fs::read_to_string(&path).expect("read panic log");
        assert!(contents.contains("expected panic message"));
        assert!(contents.contains("src/bar.rs:33:1"));
        assert!(contents.contains("runner-id=unit-test"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn write_panic_log_overwrites_existing_file() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard;
        let tmp = std::env::temp_dir().join("qontinui-startup-panic-test-overwrite");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("QONTINUI_RUNNER_LOG_DIR", &tmp);

        let path = tmp.join(PANIC_LOG_FILENAME);
        std::fs::write(&path, "STALE CONTENT FROM PRIOR BOOT").unwrap();

        write_panic_log("fresh panic", "src/baz.rs:1:1", "main", "backtrace");

        let contents = std::fs::read_to_string(&path).expect("read");
        assert!(!contents.contains("STALE CONTENT"), "should overwrite");
        assert!(contents.contains("fresh panic"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
