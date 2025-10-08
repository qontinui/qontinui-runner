use chrono::Local;
use std::path::PathBuf;
use tracing::Level;
use tracing_appender::{non_blocking, rolling};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter, Registry,
};

pub struct LoggingConfig {
    pub level: Level,
    pub log_to_file: bool,
    pub log_to_console: bool,
    pub log_dir: PathBuf,
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
        }
    }
}

pub fn init_logging(config: LoggingConfig) -> anyhow::Result<()> {
    std::fs::create_dir_all(&config.log_dir)?;

    let env_filter = EnvFilter::new(
        std::env::var("RUST_LOG")
            .unwrap_or_else(|_| format!("qontinui_runner={},tauri=info", config.level)),
    );

    let registry = Registry::default().with(env_filter);

    // Store log_dir for logging before it's moved
    let log_dir_path = config.log_dir.clone();

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

        let subscriber = registry.with(file_layer);

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

        registry.with(console_layer).init();
    }

    tracing::info!("Logging initialized at level: {:?}", config.level);
    tracing::info!("Log directory: {:?}", log_dir_path);
    tracing::info!("Application started at {}", Local::now());

    Ok(())
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

pub fn log_panic(info: &std::panic::PanicHookInfo) {
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

    tracing::error!(
        "PANIC at {}: {}\nBacktrace:\n{:?}",
        location,
        message,
        std::backtrace::Backtrace::capture()
    );
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
