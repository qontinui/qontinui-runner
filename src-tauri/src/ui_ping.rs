//! The 3 s `ui-bridge-ping` emitter, on its own OS thread.
//!
//! ## Why this is not a tokio task
//!
//! The ping is the half of the native-UI liveness signal that *asks*: Rust
//! emits `ui-bridge-ping` as a Tauri event, the frontend answers with a
//! `ui-bridge-pong` event, and an event-provenance pong proves the native
//! message loop delivered something (`ui_error::LAST_EVENT_PONG_MS`). Until
//! plan `2026-10-19-runner-ui-wedge-signal-fires-when-the-app-runtime-is-starved-by-process-spawns`
//! the asking side was a `tokio::spawn` inside `mcp_api::create_router`, which
//! runs before the API runtime exists, so it landed on the APP runtime
//! (`app-rt`). When `app-rt`'s workers were all parked in synchronous
//! `CreateProcessW` (0.7–4.2 s each on spaceship, 2026-10-18/19) the task was
//! not scheduled, no ping was sent for minutes, the event pong went stale, and
//! `/health` called the native UI loop wedged while it was pumping.
//!
//! An OS thread is scheduled by the OS whatever tokio is doing — the same
//! reason `off_runtime::deadline` and `health_monitor`'s watchdog are threads.
//!
//! ## Why the emit is safe from a plain thread
//!
//! Verified against the pinned Tauri (2.11.1, `tauri-runtime-wry` 2.11.2):
//! `Emitter::emit` serialises the payload, then `Webview::emit_js` →
//! `eval_script`. With the `tracing` feature OFF (it is: `cargo tree -e
//! features -i tauri-runtime-wry` shows only `x11`, `dbus` and
//! `common-controls-v6`), `eval_script` is `send_user_message`, which off the
//! main thread is `EventLoopProxy::send_event` — a non-blocking post. It needs
//! no tokio runtime and does not wait for the event loop, so a wedged native
//! loop cannot block this thread: the post succeeds, the pong never comes back,
//! and the verdict fires for the right reason.
//!
//! That rests on the `tracing` feature staying OFF. With it on, `eval_script`
//! sends and then waits on a reply channel, so a wedged native loop would
//! block this thread: emits would stop succeeding, `lastPingEmitOk` would age,
//! and the native verdict would degrade to `ping_not_emitted` (UNKNOWN) rather
//! than fire. A degraded detector, never a false one -- but a feature change
//! that turns it on should re-read this paragraph.
//!
//! ## What the thread records
//!
//! Every emit's `Result` goes to `ui_error::record_ping_emit_ok` /
//! `record_ping_emit_failure`, exactly as the deleted task did. Every tick
//! also stamps [`ping_thread_tick_ms`], so a ping thread that died is visible
//! as an aging stamp rather than as a silent absence.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The ping cadence. `ui_error::UI_EVENT_DEAD_AFTER_MS` (90 s) is "thirty
/// consecutive pings sent and unanswered" at this rate.
pub const UI_PING_CADENCE: Duration = Duration::from_secs(3);

/// The thread's name — what a thread census (`wedge_diagnostics`) reports.
pub const UI_PING_THREAD_NAME: &str = "ui-bridge-ping";

/// Wall-clock ms of the ping thread's last tick. 0 = the thread never ran.
static PING_THREAD_TICK_MS: AtomicU64 = AtomicU64::new(0);

/// Set by the first [`start_ui_ping_thread`] call, so a second call (setup
/// re-entered, or a test) never starts a second thread.
static STARTED: OnceLock<()> = OnceLock::new();

/// Set only by the debug-build stop hook below; in production nothing sets it
/// and the ping thread lives for the process.
static STOP: AtomicBool = AtomicBool::new(false);

/// Debug-build verification hook, read once at spawn. Compiled only under
/// `debug_assertions`; a release build has no such switch.
///
/// * `QONTINUI_DEBUG_STOP_UI_PING=<N>` (a positive integer): the thread runs
///   normally for N seconds, then STOPS. Pongs have already landed by then, so
///   once the event pong ages past `UI_EVENT_DEAD_AFTER_MS` (90 s) with the
///   renderer still alive, the native verdict reads `ping_not_emitted` with
///   `wedged: null` -- on a box whose Win32 probe cannot be asked. Where the
///   probe answers (a Windows runner with its health monitor running) the
///   measured round trip outranks it and the verdict reads `pumping`; there
///   the reading to assert is `eventsUndelivered: false` and `wedged != true`.
/// * any other non-empty value: the thread is never started. No pong ever
///   lands, the event pong stays 0, and the verdict reads `unknown` — or
///   `pumping` where the Win32 probe answers, the same caveat as above.
pub const DEBUG_STOP_ENV: &str = "QONTINUI_DEBUG_STOP_UI_PING";

/// What the debug stop hook asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugStop {
    /// No hook: run for the life of the process.
    Off,
    /// Never start the thread.
    NeverStart,
    /// Run normally, then stop after this long.
    After(Duration),
}

/// Parse the hook's value. Pure, so every arm is testable in any build.
fn parse_debug_stop(value: Option<&str>) -> DebugStop {
    match value.map(str::trim) {
        None | Some("") => DebugStop::Off,
        Some(v) => match v.parse::<u64>() {
            Ok(secs) if secs > 0 => DebugStop::After(Duration::from_secs(secs)),
            _ => DebugStop::NeverStart,
        },
    }
}

#[cfg(debug_assertions)]
fn debug_stop() -> DebugStop {
    parse_debug_stop(std::env::var(DEBUG_STOP_ENV).ok().as_deref())
}

#[cfg(not(debug_assertions))]
fn debug_stop() -> DebugStop {
    DebugStop::Off
}

/// Wall-clock ms of the ping thread's last tick, `0` when it never ran.
pub fn ping_thread_tick_ms() -> u64 {
    PING_THREAD_TICK_MS.load(Ordering::Relaxed)
}

/// What [`start_ui_ping_thread`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingStart {
    /// This call started the thread.
    Started,
    /// A previous call already started (or tried to start) it.
    AlreadyStarted,
    /// The debug-build stop hook asked for no thread at all.
    SuppressedByDebugHook,
}

/// Start the process's one ping thread. Idempotent: only the first call does
/// anything.
///
/// A thread that cannot be spawned is returned as `Err` for the caller to log:
/// with no ping, the native-UI verdict reads `ping_not_emitted` (UNKNOWN)
/// rather than firing, which is the honest consequence. A failed spawn is not
/// retried by a later call.
pub fn start_ui_ping_thread<R: tauri::Runtime>(
    handle: tauri::AppHandle<R>,
) -> std::io::Result<PingStart> {
    if STARTED.set(()).is_err() {
        return Ok(PingStart::AlreadyStarted);
    }
    let stop_after = match debug_stop() {
        DebugStop::Off => None,
        DebugStop::NeverStart => {
            tracing::warn!(
                env = DEBUG_STOP_ENV,
                "ui_ping: the ui-bridge-ping thread is NOT started (debug-build verification hook)"
            );
            return Ok(PingStart::SuppressedByDebugHook);
        }
        DebugStop::After(d) => {
            tracing::warn!(
                env = DEBUG_STOP_ENV,
                stop_after_secs = d.as_secs(),
                "ui_ping: the ui-bridge-ping thread will STOP after this many seconds \
                 (debug-build verification hook)"
            );
            Some(std::time::Instant::now() + d)
        }
    };
    std::thread::Builder::new()
        .name(UI_PING_THREAD_NAME.to_string())
        .spawn(move || {
            use tauri::Emitter;
            run_ping_loop(
                || {
                    if stop_after.is_some_and(|at| std::time::Instant::now() >= at) {
                        // Debug hook only: the loop sees STOP before its next tick.
                        STOP.store(true, Ordering::Relaxed);
                        return;
                    }
                    // The `Result` is CHECKED, not discarded. This is the
                    // unfiltered broadcast (`emit`, not `emit_to`), so on
                    // Windows a single destroyed pop-out makes it fail with
                    // `ERROR_INVALID_WINDOW_HANDLE (0x80070578)` — 119,012
                    // times on the operator box on 2026-09-01 while `let _ =`
                    // swallowed them. Recording both arms is what lets
                    // `ui_error` tell "the UI did not answer" from "we never
                    // asked".
                    match handle.emit(
                        "ui-bridge-ping",
                        serde_json::json!({ "timestamp": chrono::Utc::now().timestamp_millis() }),
                    ) {
                        Ok(()) => crate::ui_error::record_ping_emit_ok(),
                        Err(e) => {
                            crate::ui_error::record_ping_emit_failure();
                            // `debug!`: a storm is 20/minute and the COUNTER on
                            // `/health` is the surface meant to carry it.
                            tracing::debug!(
                                error = %e,
                                "ui-bridge-ping emit failed — the frontend was not asked for a pong"
                            );
                        }
                    }
                },
                UI_PING_CADENCE,
                &STOP,
                &PING_THREAD_TICK_MS,
            );
        })
        .map(|_| PingStart::Started)
}

/// The ping loop, free of Tauri so it can be tested on its own thread.
///
/// Fixed-rate at `cadence`: tick `n` is due at `start + n * cadence`. A tick
/// that is late by more than one cadence (the thread was descheduled, or the
/// emit itself took long) SKIPS the missed ticks rather than bursting them —
/// a burst would publish a fresh `lastPingEmitOk` that says nothing about the
/// interval it skipped.
///
/// Returns when `stop` is set (checked once per tick; production never sets
/// it). `tick_stamp` receives the wall-clock ms of every tick.
pub fn run_ping_loop(
    mut emit: impl FnMut(),
    cadence: Duration,
    stop: &AtomicBool,
    tick_stamp: &AtomicU64,
) {
    let cadence = cadence.max(Duration::from_millis(1));
    let mut next = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        tick_stamp.store(crate::ui_error::now_ms_epoch_pub(), Ordering::Relaxed);
        emit();
        next += cadence;
        let now = Instant::now();
        if next <= now {
            // Late: skip every tick that is already in the past.
            let behind = now.duration_since(next);
            let missed = (behind.as_nanos() / cadence.as_nanos()) as u32 + 1;
            next += cadence * missed;
        }
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    /// How long a fixture may take to park every worker before the test fails
    /// instead of hanging.
    const PARK_DEADLINE: Duration = Duration::from_secs(10);

    /// Park EVERY worker of the runtime behind `rt` in a synchronous sleep, and
    /// return only once all of them are provably parked: each task reports in
    /// over a channel and then sleeps, and `workers` distinct reports can only
    /// arrive when `workers` distinct threads are inside a task at once.
    /// Panics (rather than hangs) when they do not all report in time.
    fn park_every_worker(rt: &tokio::runtime::Handle, workers: usize, for_: Duration) {
        let (tx, rx) = mpsc::channel::<()>();
        for _ in 0..workers {
            let tx = tx.clone();
            rt.spawn(async move {
                let _ = tx.send(());
                std::thread::sleep(for_);
            });
        }
        for n in 0..workers {
            rx.recv_timeout(PARK_DEADLINE).unwrap_or_else(|_| {
                panic!("only {n} of {workers} workers parked within {PARK_DEADLINE:?}")
            });
        }
    }

    /// Run `f` with the handle of a fresh multi-thread runtime of `workers`
    /// threads named `name`, then shut the runtime down without waiting for its
    /// parked workers. The runtime's type is never named (clippy
    /// `disallowed-types`: an owned runtime must not be held where it could be
    /// dropped from async context); it lives only in this frame.
    fn with_runtime<T>(
        workers: usize,
        name: &str,
        f: impl FnOnce(&tokio::runtime::Handle) -> T,
    ) -> T {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .thread_name(name)
            .enable_all()
            .build()
            .expect("test runtime");
        let out = f(rt.handle());
        rt.shutdown_background();
        out
    }

    /// Emit instants recorded by a counting emitter.
    type Emits = Arc<Mutex<Vec<Instant>>>;

    fn max_gap(emits: &[Instant]) -> Duration {
        emits
            .windows(2)
            .map(|w| w[1].duration_since(w[0]))
            .max()
            .unwrap_or(Duration::ZERO)
    }

    // The production cadence is 3 s; these tests run the same loop at 150 ms
    // so the property ("keeps its cadence while every runtime worker is
    // parked") is asserted in ~2 s instead of ~12 s.
    const TEST_CADENCE: Duration = Duration::from_millis(150);
    const TEST_WINDOW: Duration = Duration::from_millis(1_950);

    #[test]
    fn the_ping_keeps_its_cadence_while_every_runtime_worker_is_blocked() {
        // An `app-rt`-shaped runtime (16 workers) and an `mcp-api-rt`-shaped
        // one (4), both fully parked for longer than the measurement window.
        let emits = with_runtime(16, "app-rt", |app_rt| {
            with_runtime(4, "mcp-api-rt", |api_rt| {
                park_every_worker(app_rt, 16, TEST_WINDOW * 3);
                park_every_worker(api_rt, 4, TEST_WINDOW * 3);

                let emits: Emits = Arc::default();
                let stop = Arc::new(AtomicBool::new(false));
                let tick = Arc::new(AtomicU64::new(0));
                let (e, s, t) = (emits.clone(), stop.clone(), tick.clone());
                let thread = std::thread::Builder::new()
                    .name(UI_PING_THREAD_NAME.into())
                    .spawn(move || {
                        run_ping_loop(
                            || e.lock().unwrap().push(Instant::now()),
                            TEST_CADENCE,
                            &s,
                            &t,
                        )
                    })
                    .unwrap();
                std::thread::sleep(TEST_WINDOW);
                stop.store(true, Ordering::Relaxed);
                thread.join().unwrap();
                assert!(
                    tick.load(Ordering::Relaxed) > 0,
                    "the tick stamp never moved"
                );
                let emits = emits.lock().unwrap().clone();
                emits
            })
        });
        assert!(
            emits.len() >= 10,
            "only {} emits in {TEST_WINDOW:?} at a {TEST_CADENCE:?} cadence while every \
             runtime worker was parked",
            emits.len()
        );
        // A loose bound on purpose: one deschedule on a saturated CI box must
        // not fail it, and the negative control below already separates the
        // two cases by count alone (a starved runtime emits almost nothing).
        let gap = max_gap(&emits);
        assert!(
            gap <= TEST_WINDOW / 4,
            "max inter-emit gap {gap:?} exceeds a quarter of the {TEST_WINDOW:?} window"
        );
    }

    /// The mutation the plan names, kept in the suite as a negative control:
    /// the SAME emit cadence driven as a tokio task on the parked runtime (the
    /// deleted `create_router` shape) does NOT keep its cadence. If this ever
    /// passes the cadence assertion, the test above has stopped measuring
    /// anything.
    #[test]
    fn a_ping_task_on_a_parked_runtime_misses_its_cadence() {
        let during = with_runtime(2, "app-rt", |app_rt| {
            let emits: Emits = Arc::default();
            let e = emits.clone();
            // Started BEFORE the park, like the real task (it was spawned at boot).
            app_rt.spawn(async move {
                let mut interval = tokio::time::interval(TEST_CADENCE);
                loop {
                    interval.tick().await;
                    e.lock().unwrap().push(Instant::now());
                }
            });
            std::thread::sleep(TEST_CADENCE * 2);
            let before = emits.lock().unwrap().len();
            park_every_worker(app_rt, 2, TEST_WINDOW * 3);
            std::thread::sleep(TEST_WINDOW);
            let n = emits.lock().unwrap().len();
            n - before
        });
        assert!(
            during < 10,
            "a tokio ping task on a fully parked runtime still emitted {during} times — \
             the parking fixture no longer starves the runtime, so the cadence test \
             above proves nothing"
        );
    }

    #[test]
    fn a_late_tick_skips_missed_ticks_instead_of_bursting() {
        let emits: Emits = Arc::default();
        let stop = AtomicBool::new(false);
        let tick = AtomicU64::new(0);
        let mut n = 0u32;
        let e = emits.clone();
        run_ping_loop(
            || {
                n += 1;
                e.lock().unwrap().push(Instant::now());
                if n == 2 {
                    // One emit that takes 3.5 cadences.
                    std::thread::sleep(TEST_CADENCE * 7 / 2);
                }
                if n == 4 {
                    stop.store(true, Ordering::Relaxed);
                }
            },
            TEST_CADENCE,
            &stop,
            &tick,
        );
        let emits = emits.lock().unwrap().clone();
        assert_eq!(emits.len(), 4);
        // Emit 1 is at 0, emit 2 at 1 cadence and takes 3.5, so ticks 2..=4
        // are missed: emit 3 lands on the 5-cadence boundary and emit 4 on the
        // 6. A burst would put both at ~4.5. One-sided checks against the
        // FIRST emit, so a descheduled thread can only make them pass more
        // easily, never fail them.
        let slack = TEST_CADENCE / 4;
        let third = emits[2].duration_since(emits[0]);
        assert!(
            third + slack >= TEST_CADENCE * 5,
            "skipped ticks were burst: emit 3 came {third:?} after emit 1"
        );
        let fourth = emits[3].duration_since(emits[0]);
        assert!(
            fourth + slack >= TEST_CADENCE * 6,
            "skipped ticks were burst: emit 4 came {fourth:?} after emit 1"
        );
    }

    #[test]
    fn the_debug_stop_hook_parses_its_three_modes() {
        assert_eq!(parse_debug_stop(None), DebugStop::Off);
        assert_eq!(parse_debug_stop(Some("")), DebugStop::Off);
        assert_eq!(parse_debug_stop(Some("  ")), DebugStop::Off);
        assert_eq!(
            parse_debug_stop(Some("45")),
            DebugStop::After(Duration::from_secs(45))
        );
        assert_eq!(
            parse_debug_stop(Some(" 7 ")),
            DebugStop::After(Duration::from_secs(7))
        );
        for never in ["0", "1s", "yes", "-3"] {
            assert_eq!(
                parse_debug_stop(Some(never)),
                DebugStop::NeverStart,
                "{never:?}"
            );
        }
    }

    /// The debug hook must not ship: the env read exists only in the
    /// `debug_assertions` arm, and the release arm is the constant `Off`.
    #[test]
    fn the_stop_hook_is_compiled_only_into_debug_builds() {
        let src = include_str!("ui_ping.rs").replace("\r\n", "\n");
        let prod = src.split("\n#[cfg(test)]\nmod tests").next().unwrap();
        let debug_arm = prod
            .split("#[cfg(debug_assertions)]\nfn debug_stop() -> DebugStop {")
            .nth(1)
            .expect("a debug_assertions-gated debug_stop");
        assert!(debug_arm
            .trim_start()
            .starts_with("parse_debug_stop(std::env::var(DEBUG_STOP_ENV)"));
        let release_arm = prod
            .split("#[cfg(not(debug_assertions))]\nfn debug_stop() -> DebugStop {")
            .nth(1)
            .expect("a release arm");
        assert!(
            release_arm.trim_start().starts_with("DebugStop::Off"),
            "the release arm must be the constant Off, not an env read"
        );
        assert_eq!(prod.matches("std::env::var(DEBUG_STOP_ENV)").count(), 1);
    }

    /// Production wiring, the other half: `main.rs` setup STARTS the thread,
    /// exactly once. Without that call no ping is ever sent, the native
    /// verdict reads `ping_not_emitted` forever and the 2026-08-19 detector is
    /// silently off — and every other test here stays green.
    #[test]
    fn main_setup_starts_the_ping_thread_exactly_once() {
        let main = include_str!("main.rs").replace("\r\n", "\n");
        let calls = main
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .filter(|l| l.contains("ui_ping::start_ui_ping_thread("))
            .count();
        assert_eq!(
            calls, 1,
            "main.rs must call ui_ping::start_ui_ping_thread exactly once"
        );
    }

    /// Production wiring: `ui_ping.rs` is the ONLY non-test source that names
    /// the `ui-bridge-ping` event in a string literal, so no second emitter
    /// (in particular the deleted `create_router` task) can come back. Parsed
    /// with `syn`, so comments and doc text that mention the event do not
    /// count — only literals in code do, including literals nested inside
    /// macro arguments. Test modules are skipped.
    #[test]
    fn ui_ping_is_the_only_emitter_of_the_ping_event() {
        use syn::visit::{self, Visit};

        fn literals_in(tokens: proc_macro2::TokenStream, found: &mut Vec<usize>) {
            for tt in tokens {
                match tt {
                    proc_macro2::TokenTree::Literal(l) => {
                        if syn::parse_str::<syn::LitStr>(&l.to_string())
                            .is_ok_and(|s| s.value() == "ui-bridge-ping")
                        {
                            found.push(l.span().start().line);
                        }
                    }
                    proc_macro2::TokenTree::Group(g) => literals_in(g.stream(), found),
                    _ => {}
                }
            }
        }

        struct Lits {
            found: Vec<usize>,
        }
        impl<'ast> Visit<'ast> for Lits {
            fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
                let is_test = m.attrs.iter().any(|a| {
                    a.path().is_ident("cfg")
                        && a.parse_args::<syn::Meta>()
                            .map(|meta| meta.path().is_ident("test"))
                            .unwrap_or(false)
                });
                if !is_test {
                    visit::visit_item_mod(self, m);
                }
            }
            fn visit_lit_str(&mut self, l: &'ast syn::LitStr) {
                if l.value() == "ui-bridge-ping" {
                    self.found.push(l.span().start().line);
                }
            }
            fn visit_macro(&mut self, m: &'ast syn::Macro) {
                // Literals inside macro arguments (`json!`, `format!`, …), at
                // any nesting depth.
                literals_in(m.tokens.clone(), &mut self.found);
            }
        }

        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut walked = 0usize;
        let mut offenders = Vec::new();
        let mut in_ui_ping = 0usize;
        for entry in walkdir::WalkDir::new(&root).sort_by_file_name() {
            let entry = entry.expect("walk src");
            let path = entry.path();
            if !entry.file_type().is_file() || !path.extension().is_some_and(|x| x == "rs") {
                continue;
            }
            walked += 1;
            let src = std::fs::read_to_string(path).expect("read source");
            if !src.contains("ui-bridge-ping") {
                continue;
            }
            let rel = path.strip_prefix(&root).unwrap().display().to_string();
            let file = syn::parse_file(&src).unwrap_or_else(|e| panic!("parsing src/{rel}: {e}"));
            let mut v = Lits { found: Vec::new() };
            v.visit_file(&file);
            if v.found.is_empty() {
                continue;
            }
            if rel.replace('\\', "/") == "ui_ping.rs" {
                in_ui_ping += v.found.len();
            } else {
                offenders.push(format!("src/{rel} (lines {:?})", v.found));
            }
        }
        assert!(
            walked > 200,
            "walked only {walked} .rs files — the scan saw nothing"
        );
        // Two in ui_ping.rs: the thread name and the emit itself.
        assert!(
            in_ui_ping >= 2,
            "ui_ping.rs names the event {in_ui_ping} time(s) — the scan is not finding the real emitter"
        );
        assert!(
            offenders.is_empty(),
            "`ui-bridge-ping` is named as a string literal outside ui_ping.rs: {offenders:?}. \
             The ping must have exactly one emitter, the OS thread in ui_ping.rs."
        );
    }
}
