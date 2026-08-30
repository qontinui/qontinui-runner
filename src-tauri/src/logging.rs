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

// `Layer::with_filter` is only reached from the `debug-tokio-console` arms
// below; importing it unconditionally would be an unused import in the default
// build, which this crate denies.
#[cfg(feature = "debug-tokio-console")]
use tracing_subscriber::Layer as _;

/// Dev-only `tokio-console` wiring. **Compiled only under the
/// `debug-tokio-console` Cargo feature, which is absent from `default`.**
///
/// # What it is for
///
/// Phase 5 of `2026-08-30-runner-blocking-pool-exhaustion-and-wedge-diagnostics`.
/// The blocking-pool / wedge bug class is the one where the process is alive
/// and answering nothing: tokio's blocking pool is pinned at its ceiling, or a
/// worker thread is parked inside a `WriteFile` that will never return, and the
/// symptom is only ever "requests time out".
///
/// The native OS-level census added by Phase 4 (thread count, handle count,
/// child processes) proves the process is *in* that state, but it structurally
/// **cannot** say *which async task* is stalled or *for how long* — the OS has
/// no concept of a tokio task. `tokio-console` reads the runtime's own task
/// graph: per-task poll durations, what each task is currently awaiting,
/// task-level contention, and the busy/idle split that separates "blocked" from
/// "starved". That is real diagnostic capability nothing else here provides,
/// and the point of having it is to reproduce and diagnose the *next* instance
/// of this bug class live on a dev box.
///
/// # Why it is off by default, and cannot ship
///
/// `console-subscriber` only works if the entire dependency graph — tokio
/// included — was compiled with `--cfg tokio_unstable`. That is a build-wide
/// rustc flag, not a Cargo feature, so it opts the whole binary into tokio's
/// explicitly unstable API surface, whose compatibility guarantees are
/// deliberately weaker than tokio's semver promise. A shipped desktop binary
/// should not be built that way, and `tokio-console` is a developer's debugger,
/// not an end-user capability.
///
/// Two independent gates enforce that:
///
/// 1. `debug-tokio-console` is not in `default`, so a normal build never even
///    resolves `console-subscriber`; and
/// 2. `--cfg tokio_unstable` is set **nowhere in this repository** — not in
///    `.cargo/config.toml`, not in `build.rs`. `build.rs`'s
///    `guard_tokio_console_cfg` fails the build with a one-line explanation if
///    the feature is on without the flag, so the mistake is a legible error
///    rather than a runtime `assert!` inside `ConsoleLayer::build`.
///
/// # Running it
///
/// ```text
/// cargo install --locked tokio-console          # once
///
/// # bash / WSL
/// scripts/dev-tokio-console.sh run
/// # PowerShell
/// scripts/dev-tokio-console.ps1 -Action run
///
/// # or by hand, from src-tauri/
/// RUSTFLAGS="--cfg tokio_unstable" cargo run --features debug-tokio-console
///
/// tokio-console http://127.0.0.1:6669           # in a second terminal
/// ```
///
/// `TOKIO_CONSOLE_BIND` overrides the listen address (`ip:port`); the runner
/// logs one INFO line at startup naming the address it actually bound. Changing
/// `RUSTFLAGS` invalidates the whole build cache, so the first such build is a
/// full rebuild of the dependency graph — that is expected, not a fault.
///
/// Full write-up: `src-tauri/docs/tokio-console.md`.
#[cfg(feature = "debug-tokio-console")]
pub mod tokio_console {
    use tracing_subscriber::filter::{filter_fn, FilterFn};
    use tracing_subscriber::registry::LookupSpan;
    use tracing_subscriber::{EnvFilter, Layer};

    /// `console-subscriber`'s own default listen address, restated so the
    /// startup log line can name an address even when `TOKIO_CONSOLE_BIND` is
    /// unset. Keep in sync with `console_subscriber::Builder::with_default_env`.
    pub const DEFAULT_BIND: &str = "127.0.0.1:6669";

    /// The address `tokio-console` should be pointed at, as configured by the
    /// standard `TOKIO_CONSOLE_BIND` environment variable that
    /// `Builder::with_default_env` reads.
    pub fn bind_addr() -> String {
        std::env::var("TOKIO_CONSOLE_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string())
    }

    /// Widen the process-wide [`EnvFilter`] so tokio's runtime instrumentation
    /// callsites are enabled.
    ///
    /// This is load-bearing and easy to miss. `init_logging` installs
    /// `EnvFilter` as a *global* filter layer, and a global filter short-circuits
    /// `Subscriber::enabled` for **every** layer, per-layer filters included. The
    /// default directive set (`qontinui_runner=info,tauri=info`) matches neither
    /// `tokio` nor `runtime`, so without this the console layer would be
    /// installed, bind its port, and then show an empty task list forever —
    /// tokio would never even construct the spans.
    pub fn widen_env_filter(filter: EnvFilter) -> EnvFilter {
        filter
            .add_directive(
                "tokio=trace"
                    .parse()
                    .expect("`tokio=trace` is a valid static directive"),
            )
            .add_directive(
                "runtime=trace"
                    .parse()
                    .expect("`runtime=trace` is a valid static directive"),
            )
    }

    /// True for the callsites `console-subscriber` consumes — tokio's runtime
    /// instrumentation.
    ///
    /// Deliberately the same predicate `console_subscriber::Builder::spawn`
    /// attaches to its own layer, so that [`not_runtime_instrumentation`] is its
    /// exact complement.
    fn is_runtime_instrumentation(meta: &tracing::Metadata<'_>) -> bool {
        if meta.is_event() {
            return meta.target().starts_with("runtime") || meta.target().starts_with("tokio");
        }
        meta.name().starts_with("runtime.") || meta.target().starts_with("tokio")
    }

    /// Per-layer filter that keeps the runtime firehose out of the *human* logs.
    ///
    /// [`widen_env_filter`] has to enable `tokio`/`runtime` at TRACE globally,
    /// and tokio emits one of those per poll, per task. Attached to the file,
    /// console and JSONL-span layers this restores exactly the volume a default
    /// build produces: those callsites reach the console layer and nothing else,
    /// so enabling this feature does not drown the log file that every other
    /// diagnostic in this crate reads.
    pub fn not_runtime_instrumentation() -> FilterFn<fn(&tracing::Metadata<'_>) -> bool> {
        fn keep(meta: &tracing::Metadata<'_>) -> bool {
            !is_runtime_instrumentation(meta)
        }
        filter_fn(keep as fn(&tracing::Metadata<'_>) -> bool)
    }

    /// Build the `tokio-console` layer and spawn its gRPC server.
    ///
    /// `Builder::spawn` puts the server on its own dedicated thread with its own
    /// current-thread runtime, so this is safe to call before the application's
    /// runtime exists — which matters, because `init_logging` runs early in
    /// `main`. The returned layer carries `console-subscriber`'s own per-layer
    /// filter, so it never widens what the other layers record.
    pub fn layer<S>() -> impl Layer<S>
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        console_subscriber::ConsoleLayer::builder()
            .with_default_env()
            .spawn()
    }
}

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

    /// Path of the fixed `latest_crash.txt` alias this thread's panic hook
    /// wrote most recently.
    ///
    /// `crash_dumps::is_crash_dump_filename` deliberately ignores this file, so
    /// it never fooled the startup scan — but it IS the copy humans and agents
    /// open by name, and it was the one artifact `retract_last_crash_dump` left
    /// behind. A caught panic therefore kept advertising a crash indefinitely.
    static LAST_LATEST_CRASH_ALIAS: std::cell::RefCell<Option<PathBuf>> =
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
    // Downgrade the fixed `latest_crash.txt` alias alongside the timestamped
    // dump. Renamed rather than deleted so the forensics survive — under a name
    // that says the process lived.
    LAST_LATEST_CRASH_ALIAS.with(|cell| {
        if let Some(alias) = cell.borrow_mut().take() {
            let caught_alias = alias.with_file_name("latest_caught.txt");
            if let Err(e) = std::fs::rename(&alias, &caught_alias) {
                safe_eprintln(&format!(
                    "Failed to downgrade caught-panic latest_crash alias {alias:?}: {e}"
                ));
            }
        }
    });

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
        // Write the runner's tracing sink into the SAME dev-logs tree as every
        // other dev log (backend/frontend/supervisor). `get_dev_logs_dir()`
        // honors the `paths.dev_logs_dir` settings override (so on a dev box
        // this resolves to e.g. `D:/qontinui-root/.dev-logs/`, where CLAUDE.md
        // and the operator look) and already applies `instance::scope_path`,
        // so secondary runners still get their own `instance-<id>/` subdir and
        // never interleave into one daily-rolling file. On an installed runner
        // with no override it falls back to `<app_data>/qontinui-runner/dev-logs`.
        //
        // Previously this was a hardcoded `<app_data>/qontinui-runner/logs`
        // that ignored the override, stranding the sink in a directory tree
        // divorced from every other log — the root cause of the 2026-07-21
        // "runner tracing is unlogged" misdiagnosis (the tracing was written,
        // just nowhere anyone looked). See
        // plans/2026-07-21-runner-tracing-observability-tailable-log-sink.md.
        let log_dir = crate::paths::get_dev_logs_dir();

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
    /// Non-blocking file appender guard — keeps the background writer thread
    /// alive. If this drops, the worker shuts down and every subsequent log
    /// line is silently discarded (empty `qontinui-runner.log`). Must outlive
    /// the process, so it rides in the returned struct alongside `_otel_guard`.
    /// `None` when file logging is disabled.
    pub _file_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
    /// Non-blocking **console** appender guard. Same contract as
    /// `_file_guard`: dropping it stops the writer thread and silences the
    /// console. `None` when console logging is disabled.
    ///
    /// The console needs the same treatment as the file for a reason that cost
    /// three iterations to find --- see [`console_writer`].
    pub _console_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// Wrap a console sink so a stalled stdout can never park the caller.
///
/// ## Why this exists
///
/// `fmt::layer().with_writer(std::io::stdout)` performs a **blocking**
/// `WriteFile` on whatever thread emitted the event --- and most events are
/// emitted from tokio worker threads. When stdout's consumer stops draining
/// (a full pipe, a console host that has stopped servicing writes, a redirect
/// into a stalled reader), that write blocks *forever*. Every task that logs
/// then consumes a worker permanently, and the runtime dies once the last
/// worker is taken --- eight tasks on an eight-core box.
///
/// The signature this leaves in the log file is distinctive, because the file
/// layer runs **before** the console layer in the layer stack: every wedged
/// task's last log line IS present in the file, and nothing after it. Two
/// reproductions (2026-08-24) stalled after 124,667 and 124,445 cumulative log
/// bytes --- 0.18% apart --- while their wall-clock uptimes differed by 16.9%.
/// A byte-quantized onset is the fingerprint of a fixed-size downstream buffer
/// filling up, not of a timer or a race.
///
/// `non_blocking` moves the write onto a dedicated OS thread behind a bounded
/// lossy queue. A jammed stdout then costs console lines --- never the runtime,
/// and never the file log, which stays complete.
///
/// `lossy(true)` is stated explicitly rather than inherited from
/// `NonBlockingBuilder::default()`. It is the default today, but it is the
/// single property the whole fix rests on: with `lossy(false)` a full queue
/// makes the writer *wait*, which is the shipped defect wearing a different
/// hat. A dependency bump must not be able to re-arm this silently.
///
/// Generic over the sink and the limit purely so the mechanism tests can hand
/// it a writer that deliberately blocks and a queue small enough to fill;
/// production always passes [`std::io::stdout`] and
/// [`CONSOLE_BUFFERED_LINES_LIMIT`].
fn console_writer<W>(
    sink: W,
    buffered_lines_limit: usize,
) -> (non_blocking::NonBlocking, non_blocking::WorkerGuard)
where
    W: std::io::Write + Send + 'static,
{
    non_blocking::NonBlockingBuilder::default()
        .lossy(true)
        .buffered_lines_limit(buffered_lines_limit)
        .finish(sink)
}

/// How many console lines may queue behind a jammed stdout before the wrapper
/// starts dropping them.
///
/// This restates `tracing_appender`'s own default. It is restated rather than
/// inherited because the two costs it balances pull in OPPOSITE directions,
/// and a future tuner needs to see both before touching it:
///
/// * **Larger** --- more resident memory behind a permanent jam. Each queued
///   line owns its formatted `Vec<u8>`; at ~250 B/line this ceiling is tens of
///   megabytes. The bounded array is also allocated eagerly at startup.
/// * **Smaller** --- the queue reaches capacity *sooner*, which makes
///   `WorkerGuard::drop`'s shutdown `send_timeout` more likely to expire. When
///   it does, tracing-appender reports the failure with a `println!` --- onto
///   the jammed stdout. Shrinking this buys memory with the reachability of a
///   hang inside someone else's `Drop`.
const CONSOLE_BUFFERED_LINES_LIMIT: usize = 128_000;

/// The `RUST_LOG` string used when the environment does not supply one.
///
/// # Why `tao` and `wry` are named explicitly
///
/// `EnvFilter` has no global default directive here — every directive in this
/// string is target-scoped — and an `EnvFilter` built that way **disables every
/// target that matches no directive**. So `qontinui_runner=…,tauri=info` did
/// not merely turn the windowing crates down, it turned them OFF: a 17 MB
/// instance log contained 0 `tao`, 0 `wry` and 0 `webview2` lines.
///
/// That is not cosmetic. `tao` reaches this subscriber through the `log` →
/// `tracing` bridge that `SubscriberInitExt::init` installs (`tao` logs via
/// `#[macro_use] extern crate log`), and the one diagnostic our vendored
/// `tao` patch emits for the `ERROR_INVALID_WINDOW_HANDLE` `PostMessage`
/// storm — `platform_impl/windows/event_loop.rs`'s "target window {:#x} is
/// destroyed", deliberately bounded to one line per window — is a
/// `log::warn!` on the `tao::…` target. Without a `tao` directive that warning
/// is dropped inside the filter, which makes the patch **unfalsifiable**: the
/// fix cannot be distinguished from the race never firing.
///
/// `warn` rather than `info` is the level because these crates are chatty at
/// `info` and below on Windows, and the events worth having are exactly the
/// ones that report a broken window handle or a failed webview operation.
///
/// Keeping the level for our own crate a parameter (rather than hard-coding
/// `info`) preserves the existing behaviour where the persisted log level
/// steers `qontinui_runner`'s verbosity and nothing else.
fn default_env_filter(level: Level) -> String {
    format!("qontinui_runner={level},tauri=info,tao=warn,wry=warn")
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
        std::env::var("RUST_LOG").unwrap_or_else(|_| default_env_filter(config.level)),
    );

    // Dev-only (`debug-tokio-console`): `EnvFilter` is installed below as a
    // GLOBAL filter, which gates every layer including per-layer-filtered ones,
    // so the console layer sees nothing unless `tokio`/`runtime` are enabled
    // here. See `tokio_console::widen_env_filter`.
    #[cfg(feature = "debug-tokio-console")]
    let env_filter = tokio_console::widen_env_filter(env_filter);

    // Create span layer config
    let span_config = SpanLayerConfig {
        dev_logs_dir: crate::paths::get_dev_logs_dir(),
        enable_jsonl: config.enable_span_jsonl,
    };

    // Create the JSONL span layer
    let jsonl_layer = JsonlSpanLayer::new(span_config);

    // Store log_dir for logging before it's moved
    let log_dir_path = config.log_dir.clone();

    // Dev-only (`debug-tokio-console`): don't export tokio's per-poll runtime
    // instrumentation to the OTLP collector — those spans belong to the console
    // layer, not to the application trace.
    #[cfg(feature = "debug-tokio-console")]
    let otel_layer = otel_layer.with_filter(tokio_console::not_runtime_instrumentation());

    // Build the subscriber with all layers.
    // OTel layer is added first (directly on Registry) so its type matches.
    let registry = Registry::default().with(otel_layer).with(env_filter);

    // Dev-only (`debug-tokio-console`): layer the tokio-console collector on
    // ADDITION to — never in place of — everything above. It carries
    // `console-subscriber`'s own per-layer filter, so it consumes the
    // `tokio`/`runtime` callsites and nothing else. Added here, before the
    // branch below, so all four `.init()` arms inherit it from one place.
    #[cfg(feature = "debug-tokio-console")]
    let registry = registry.with(tokio_console::layer());

    // Hoisted so the WorkerGuard survives past this function and rides home in
    // `LoggingInitResult`; dropping it here would kill the writer thread.
    let mut file_guard = None;
    let mut console_guard = None;

    if config.log_to_file {
        // Daily rotation with a bounded retention window. Without
        // `max_log_files` the sink grew unbounded — the primary's log dir had
        // accreted 126 MB across 117 daily files (~35 MB/day) by 2026-07-24.
        // 14 keeps a fortnight of history, which comfortably covers any
        // interactive-flow post-mortem, and caps disk at ~2 weeks of volume.
        let file_appender = rolling::Builder::new()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix("qontinui-runner.log")
            .max_log_files(14)
            .build(&config.log_dir)
            .map_err(|e| anyhow::anyhow!("failed to build rolling log appender: {e}"))?;
        let (non_blocking_file, guard) = non_blocking(file_appender);
        file_guard = Some(guard);

        let file_layer = fmt::layer()
            .with_writer(non_blocking_file)
            .with_ansi(false)
            .with_span_events(FmtSpan::CLOSE)
            .with_timer(fmt::time::ChronoLocal::new(
                "%Y-%m-%d %H:%M:%S%.3f".to_string(),
            ));

        // Dev-only (`debug-tokio-console`): restore the default build's log
        // volume by dropping the runtime firehose the widened `EnvFilter` had to
        // let through. The log file stays readable for every other diagnostic.
        #[cfg(feature = "debug-tokio-console")]
        let file_layer = file_layer.with_filter(tokio_console::not_runtime_instrumentation());

        // Dev-only (`debug-tokio-console`): tokio opens a `runtime.spawn` span
        // per task and a `runtime.resource` span per primitive. Without this the
        // span JSONL — a diagnostic in its own right — would be pure runtime
        // noise. Shadowed per branch because `Filtered` bakes the subscriber
        // type in, and each arm below builds a different one.
        #[cfg(feature = "debug-tokio-console")]
        let jsonl_layer = jsonl_layer.with_filter(tokio_console::not_runtime_instrumentation());

        let subscriber = registry.with(file_layer).with(jsonl_layer);

        if config.log_to_console {
            let (console_sink, guard) =
                console_writer(std::io::stdout(), CONSOLE_BUFFERED_LINES_LIMIT);
            console_guard = Some(guard);
            let console_layer = fmt::layer()
                .with_writer(console_sink)
                .with_span_events(FmtSpan::CLOSE);

            // Dev-only (`debug-tokio-console`): as for the file layer above.
            #[cfg(feature = "debug-tokio-console")]
            let console_layer =
                console_layer.with_filter(tokio_console::not_runtime_instrumentation());

            subscriber.with(console_layer).init();
        } else {
            subscriber.init();
        }
    } else if config.log_to_console {
        let (console_sink, guard) = console_writer(std::io::stdout(), CONSOLE_BUFFERED_LINES_LIMIT);
        console_guard = Some(guard);
        let console_layer = fmt::layer()
            .with_writer(console_sink)
            .with_span_events(FmtSpan::CLOSE);

        // Dev-only (`debug-tokio-console`): as for the file layer above.
        #[cfg(feature = "debug-tokio-console")]
        let console_layer = console_layer.with_filter(tokio_console::not_runtime_instrumentation());

        // Dev-only (`debug-tokio-console`): tokio opens a `runtime.spawn` span
        // per task and a `runtime.resource` span per primitive. Without this the
        // span JSONL — a diagnostic in its own right — would be pure runtime
        // noise. Shadowed per branch because `Filtered` bakes the subscriber
        // type in, and each arm below builds a different one.
        #[cfg(feature = "debug-tokio-console")]
        let jsonl_layer = jsonl_layer.with_filter(tokio_console::not_runtime_instrumentation());

        registry.with(console_layer).with(jsonl_layer).init();
    } else {
        // Dev-only (`debug-tokio-console`): tokio opens a `runtime.spawn` span
        // per task and a `runtime.resource` span per primitive. Without this the
        // span JSONL — a diagnostic in its own right — would be pure runtime
        // noise. Shadowed per branch because `Filtered` bakes the subscriber
        // type in, and each arm below builds a different one.
        #[cfg(feature = "debug-tokio-console")]
        let jsonl_layer = jsonl_layer.with_filter(tokio_console::not_runtime_instrumentation());

        registry.with(jsonl_layer).init();
    }

    tracing::info!("Logging initialized at level: {:?}", config.level);
    tracing::info!("Log directory: {:?}", log_dir_path);
    tracing::info!("JSONL span layer enabled: {}", config.enable_span_jsonl);
    tracing::info!("Application started at {}", Local::now());

    // Dev-only (`debug-tokio-console`): tell the developer where to point the
    // client. One line, emitted once, only in a build that carries the feature.
    #[cfg(feature = "debug-tokio-console")]
    tracing::info!(
        "tokio-console instrumentation ACTIVE — attach with `tokio-console http://{}` \
         (override the address with TOKIO_CONSOLE_BIND). This build carries \
         --cfg tokio_unstable and MUST NOT be shipped.",
        tokio_console::bind_addr()
    );

    Ok(LoggingInitResult {
        _otel_guard: otel_guard,
        _file_guard: file_guard,
        _console_guard: console_guard,
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

    // Also write to a fixed "latest_crash.txt" for easy access. Remember it so
    // `retract_last_crash_dump` can downgrade this copy too when the panic
    // turns out to have been caught — `latest_crash.txt` is the file a human or
    // agent actually opens, and leaving a caught middleware assertion there
    // reports a process death that never happened (live evidence
    // 2026-08-05T15:28:10: the `crash_*.txt` was correctly retracted to
    // `caught_*.txt`, but the byte-identical `latest_crash.txt` was not, and
    // that stale copy is what a later investigation read as a crash).
    let latest_crash = crash_dir.join("latest_crash.txt");
    if let Ok(mut file) = std::fs::File::create(&latest_crash) {
        let _ = file.write_all(crash_content.as_bytes());
        LAST_LATEST_CRASH_ALIAS.with(|cell| *cell.borrow_mut() = Some(latest_crash.clone()));
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

#[cfg(test)]
mod console_jam_tests {
    //! The mechanism test for the 2026-08-24 P0 runtime stall.
    //!
    //! A blocking console sink used the way `fmt::layer().with_writer(..)`
    //! uses it will park whichever thread emits the event. Because most events
    //! are emitted from tokio worker threads, a stalled stdout consumed one
    //! worker per logging task until the runtime had none left.
    //!
    //! Every assertion here is measured **from outside the runtime**, over a
    //! `std::sync::mpsc` channel. That is deliberate: a check that awaits
    //! inside the runtime it is testing cannot fail when the runtime is
    //! parked --- it hangs instead, which is how two earlier attempts lost
    //! their evidence.

    use super::console_writer;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    /// A sink that accepts `capacity` bytes and then blocks forever --- a
    /// stdout whose consumer has stopped draining. `release` exists only so
    /// the test can let the writer thread go at teardown instead of leaking a
    /// permanently parked thread into the rest of the suite.
    struct JammedSink {
        remaining: Arc<AtomicUsize>,
        release: Arc<AtomicBool>,
        parked: Arc<AtomicUsize>,
    }

    impl Write for JammedSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = buf.len();
            if self.remaining.load(Ordering::SeqCst) >= n {
                self.remaining.fetch_sub(n, Ordering::SeqCst);
                return Ok(n);
            }
            self.parked.fetch_add(1, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Writing through the production console wrapper must never park a tokio
    /// worker, however hard the sink is jammed.
    ///
    /// Neuter check --- this exact edit was run, and it goes RED in 10s:
    /// swap `.lossy(true)` for `.lossy(false)` in [`console_writer`] and drop
    /// the limit to 1. Same signature, same types, but a full queue now makes
    /// the caller wait, which is the shipped defect. The probe below is then
    /// never scheduled and `recv_timeout` REPORTS the parked runtime instead
    /// of hanging on it --- the distinction that cost two earlier attempts
    /// their evidence.
    #[test]
    fn a_jammed_console_sink_cannot_park_the_runtime() {
        const WORKERS: usize = 2;

        let release = Arc::new(AtomicBool::new(false));
        let parked = Arc::new(AtomicUsize::new(0));
        let sink = JammedSink {
            // Enough for a couple of lines, then the jam.
            remaining: Arc::new(AtomicUsize::new(64)),
            release: Arc::clone(&release),
            parked: Arc::clone(&parked),
        };

        let (writer, guard) = console_writer(sink, super::CONSOLE_BUFFERED_LINES_LIMIT);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .enable_all()
            .build()
            .expect("runtime");

        // Far more logging tasks than there are workers. Under the defect each
        // one takes a worker and never gives it back.
        for i in 0..(WORKERS * 8) {
            let mut w = writer.clone();
            rt.spawn(async move {
                let _ = w.write_all(format!("jammed console line {i}\n").as_bytes());
            });
        }

        // The probe. It runs on the same runtime, so it can only answer if a
        // worker is still free --- and it answers over a std channel, so the
        // ASSERTION is measured off the runtime entirely.
        let (tx, rx) = mpsc::channel::<()>();
        rt.spawn(async move {
            let _ = tx.send(());
        });

        let verdict = rx.recv_timeout(Duration::from_secs(5));

        // Teardown BEFORE asserting: a failing assert must not leave parked
        // writer threads behind for the rest of the suite.
        release.store(true, Ordering::SeqCst);
        drop(guard);
        rt.shutdown_timeout(Duration::from_secs(5));

        assert!(
            verdict.is_ok(),
            "the tokio runtime was parked by a jammed console sink: no worker could run a \
             trivial task within 5s. {} write(s) were blocked in the sink. This is the \
             2026-08-24 P0 --- console writes must go through a non-blocking appender.",
            parked.load(Ordering::SeqCst)
        );
    }

    /// `init_logging` must never hand a console layer a raw stdout writer.
    ///
    /// Without this, the guard is only on the helper: revert the two
    /// `.with_writer(console_sink)` call sites back to
    /// `.with_writer(std::io::stdout)`, leave `console_writer` defined and
    /// unused, and both runtime tests above still pass green. The regression
    /// this file exists to prevent is a *wiring* choice, and `.init()` is
    /// global-once per process, so the wiring cannot be exercised twice in a
    /// test binary. A source check is the honest guard available.
    #[test]
    fn init_logging_never_gives_a_console_layer_a_raw_stdout_writer() {
        // Split so the needle never appears verbatim on a code line --- this
        // check would otherwise match itself.
        let needle = concat!("with_writer(std::io::", "stdout");
        let src = include_str!("logging.rs");
        let offenders: Vec<(usize, &str)> = src
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim_start().starts_with("//"))
            .filter(|(_, l)| l.contains(needle))
            .map(|(i, l)| (i + 1, l.trim()))
            .collect();

        assert!(
            offenders.is_empty(),
            "a console layer is writing straight to stdout again --- that write blocks the              emitting tokio worker when stdout's consumer stops draining, which is the              2026-08-24 P0. Route it through `console_writer`. Offending line(s): {offenders:?}"
        );
    }

    /// The wrapper must be lossy rather than back-pressuring: a jammed sink is
    /// allowed to cost console lines, and nothing else.
    ///
    /// The queue limit here is deliberately TINY. With the production limit of
    /// 128,000 this test would be vacuous --- ten thousand writes would never
    /// reach capacity, back-pressure would never engage, and the test would
    /// pass just as happily against a `lossy(false)` wrapper. A limit of 8
    /// guarantees the queue is full within the first few writes, so every
    /// write after that exercises the property the test claims to check.
    #[test]
    fn a_jammed_console_sink_never_blocks_the_calling_thread() {
        const TINY_QUEUE: usize = 8;

        let release = Arc::new(AtomicBool::new(false));
        let sink = JammedSink {
            remaining: Arc::new(AtomicUsize::new(0)),
            release: Arc::clone(&release),
            parked: Arc::new(AtomicUsize::new(0)),
        };
        let (writer, guard) = console_writer(sink, TINY_QUEUE);

        let (tx, rx) = mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            let mut w = writer;
            for i in 0..10_000 {
                let _ = w.write_all(format!("line {i}\n").as_bytes());
            }
            let _ = tx.send(());
        });

        let verdict = rx.recv_timeout(Duration::from_secs(10));

        release.store(true, Ordering::SeqCst);
        drop(guard);
        let _ = handle.join();

        assert!(
            verdict.is_ok(),
            "a jammed console sink blocked its caller: the wrapper must drop lines under \
             back-pressure, never wait on the sink"
        );
    }
}

/// Behavioural tests for [`default_env_filter`].
///
/// Separate from the main `tests` module on purpose: these install *scoped*
/// subscribers, and keeping them in their own module keeps the
/// `tracing_subscriber::Layer` import out of the rest of the file's namespace.
#[cfg(test)]
mod default_filter_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use tracing_subscriber::Layer;

    /// The target `tao` logs the vendored `event_loop.rs` patch's
    /// `ERROR_INVALID_WINDOW_HANDLE` warning on. `tao` logs through the `log`
    /// crate (`#[macro_use] extern crate log` in its `lib.rs`), and the `log`
    /// → `tracing` bridge preserves the module path as the event target, so
    /// this is the exact string `EnvFilter` matches against at runtime.
    const TAO_EVENT_LOOP_TARGET: &str = "tao::platform_impl::platform::event_loop";

    /// Counts every event that survives the filter above it.
    #[derive(Clone)]
    struct CountingLayer(Arc<AtomicUsize>);

    impl<S: tracing::Subscriber> Layer<S> for CountingLayer {
        fn on_event(
            &self,
            _event: &tracing::Event<'_>,
            _cx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counter() -> (Arc<AtomicUsize>, CountingLayer) {
        let n = Arc::new(AtomicUsize::new(0));
        (Arc::clone(&n), CountingLayer(Arc::clone(&n)))
    }

    /// The regression this fix exists for, with its own negative control.
    ///
    /// The two `warn!` invocations are deliberately written out twice rather
    /// than factored into a helper: each `warn!` is a distinct *callsite*, and
    /// a shared helper would evaluate ONE callsite under both filters, letting
    /// `tracing`'s per-callsite interest cache decide the second verdict from
    /// the first. Two sites, two filters, no cache to share.
    ///
    /// The pre-fix arm is not decoration. Without it this test passes just as
    /// happily against a filter that enables everything, and would not have
    /// caught the bug it is named for: the shipped default emitted 0 `tao`
    /// lines into a 17 MB log because `EnvFilter` disables every target that
    /// matches no directive.
    #[test]
    fn the_default_filter_passes_taos_destroyed_window_warning_and_the_pre_fix_one_dropped_it() {
        // Shipped default.
        let (shipped, layer) = counter();
        let subscriber = Registry::default()
            .with(EnvFilter::new(default_env_filter(Level::INFO)))
            .with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                target: TAO_EVENT_LOOP_TARGET,
                "tao: target window 0x1234 is destroyed; dropping queued closures for it."
            );
        });

        // NEGATIVE CONTROL: the exact filter string that shipped before this
        // fix, spelled as a literal so it cannot drift with the helper.
        let (pre_fix, layer) = counter();
        let subscriber = Registry::default()
            .with(EnvFilter::new("qontinui_runner=INFO,tauri=info"))
            .with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                target: TAO_EVENT_LOOP_TARGET,
                "tao: target window 0x1234 is destroyed; dropping queued closures for it."
            );
        });

        assert_eq!(
            pre_fix.load(Ordering::SeqCst),
            0,
            "negative control failed: the pre-fix filter was supposed to DROP the tao warning. \
             If this counts 1, the test proves nothing about the fix."
        );
        assert_eq!(
            shipped.load(Ordering::SeqCst),
            1,
            "the default filter dropped tao's destroyed-window warning — the vendored \
             event_loop.rs patch is unverifiable again"
        );
    }

    /// Pins the literal directives, so removing one is a test failure rather
    /// than a silent loss of diagnostics. Asserted against a hard-coded string
    /// rather than against `default_env_filter`'s own output, which would pin
    /// nothing.
    #[test]
    fn the_default_filter_string_is_exactly_the_four_expected_directives() {
        assert_eq!(
            default_env_filter(Level::INFO),
            "qontinui_runner=INFO,tauri=info,tao=warn,wry=warn"
        );
        assert_eq!(
            default_env_filter(Level::DEBUG),
            "qontinui_runner=DEBUG,tauri=info,tao=warn,wry=warn",
            "the configured level must steer qontinui_runner ONLY; tao/wry stay at warn"
        );
    }

    /// `wry` reaches the subscriber the same way `tao` does, and was equally
    /// invisible (0 `wry` lines in the same 17 MB log). WebView2 creation
    /// failures — the `HRESULT(0x8007139F)` class that pop-out windows hit —
    /// surface here.
    #[test]
    fn the_default_filter_passes_wry_warnings() {
        let (seen, layer) = counter();
        let subscriber = Registry::default()
            .with(EnvFilter::new(default_env_filter(Level::INFO)))
            .with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "wry::webview", "webview creation failed");
        });
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "wry warnings are filtered out"
        );
    }

    /// The level choice, stated as a test: `tao` at `info` and below is chatty
    /// enough to drown the log, so only `warn` and above may pass.
    #[test]
    fn the_default_filter_still_suppresses_tao_info_chatter() {
        let (seen, layer) = counter();
        let subscriber = Registry::default()
            .with(EnvFilter::new(default_env_filter(Level::DEBUG)))
            .with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: TAO_EVENT_LOOP_TARGET, "routine windowing chatter");
            tracing::debug!(target: TAO_EVENT_LOOP_TARGET, "routine windowing chatter");
        });
        assert_eq!(
            seen.load(Ordering::SeqCst),
            0,
            "tao=warn must not widen to info/debug even when qontinui_runner is at debug"
        );
    }
}
