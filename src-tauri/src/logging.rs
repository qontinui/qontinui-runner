use chrono::Local;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::Level;
use tracing_appender::{non_blocking, rolling};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter, Registry,
};

use crate::tracing_layers::{JsonlSpanLayer, SpanLayerConfig, SqliteSpanLayer};

/// Flag to track if we're already handling a crash (prevent recursive crashes)
static CRASH_HANDLING: AtomicBool = AtomicBool::new(false);

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
    /// Enable SQLite span output for AI analysis
    pub enable_span_sqlite: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        let log_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("qontinui-runner")
            .join("logs");

        Self {
            level: Level::INFO,
            log_to_file: true,
            log_to_console: cfg!(debug_assertions),
            log_dir,
            enable_span_jsonl: true,
            enable_span_sqlite: true,
        }
    }
}

/// Result of logging initialization, containing layers that need post-init configuration
pub struct LoggingInitResult {
    /// SQLite span layer (needs database pool to be set after DB init)
    pub sqlite_span_layer: Option<Arc<SqliteSpanLayer>>,
}

pub fn init_logging(config: LoggingConfig) -> anyhow::Result<LoggingInitResult> {
    std::fs::create_dir_all(&config.log_dir)?;

    // Clear the spans JSONL file on startup
    crate::tracing_layers::clear_spans_jsonl();

    let env_filter = EnvFilter::new(
        std::env::var("RUST_LOG")
            .unwrap_or_else(|_| format!("qontinui_runner={},tauri=info", config.level)),
    );

    // Create span layer config
    let span_config = SpanLayerConfig {
        dev_logs_dir: crate::paths::get_dev_logs_dir(),
        enable_jsonl: config.enable_span_jsonl,
        enable_sqlite: config.enable_span_sqlite,
        ..Default::default()
    };

    // Create the span layers
    let jsonl_layer = JsonlSpanLayer::new(span_config.clone());
    let sqlite_layer = Arc::new(SqliteSpanLayer::new(span_config));
    let sqlite_layer_for_registry = Arc::clone(&sqlite_layer);

    // Store log_dir for logging before it's moved
    let log_dir_path = config.log_dir.clone();

    // Build the subscriber with all layers
    // Note: We need to use a boxed approach to handle the conditional layers
    let registry = Registry::default().with(env_filter);

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

        // Add span layers
        let subscriber = registry
            .with(file_layer)
            .with(jsonl_layer)
            .with(SqliteSpanLayerWrapper(sqlite_layer_for_registry));

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

        registry
            .with(console_layer)
            .with(jsonl_layer)
            .with(SqliteSpanLayerWrapper(sqlite_layer_for_registry))
            .init();
    } else {
        // No file or console, but still add span layers
        registry
            .with(jsonl_layer)
            .with(SqliteSpanLayerWrapper(sqlite_layer_for_registry))
            .init();
    }

    tracing::info!("Logging initialized at level: {:?}", config.level);
    tracing::info!("Log directory: {:?}", log_dir_path);
    tracing::info!(
        "Span layers enabled: JSONL={}, SQLite={}",
        config.enable_span_jsonl,
        config.enable_span_sqlite
    );
    tracing::info!("Application started at {}", Local::now());

    Ok(LoggingInitResult {
        sqlite_span_layer: Some(sqlite_layer),
    })
}

/// Wrapper to allow Arc<SqliteSpanLayer> to implement Layer
struct SqliteSpanLayerWrapper(Arc<SqliteSpanLayer>);

impl<S> tracing_subscriber::Layer<S> for SqliteSpanLayerWrapper
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.0.on_new_span(attrs, id, ctx);
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.0.on_record(id, values, ctx);
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        self.0.on_event(event, ctx);
    }

    fn on_close(&self, id: tracing::span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        self.0.on_close(id, ctx);
    }
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

/// Get the crash dump directory
pub fn get_crash_dump_dir() -> PathBuf {
    // Try to use the .dev-logs directory in the parent directory
    if let Ok(exe_path) = std::env::current_exe() {
        // Navigate from exe to qontinui_parent_directory/.dev-logs
        if let Some(parent_dir) = exe_path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
        {
            let dev_logs = parent_dir.join(".dev-logs");
            if dev_logs.exists() || std::fs::create_dir_all(&dev_logs).is_ok() {
                return dev_logs;
            }
        }
    }

    // Fallback to local app data
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qontinui-runner")
        .join("crash-dumps")
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

        #[cfg(not(debug_assertions))]
        {
            let guard = sentry::init((
                std::env::var("SENTRY_DSN").unwrap_or_default(),
                sentry::ClientOptions {
                    release: sentry::release_name!(),
                    environment: Some("production".into()),
                    ..Default::default()
                },
            ));
            std::mem::forget(guard);
        }
    }));
}
