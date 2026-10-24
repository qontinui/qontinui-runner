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

/// Set once the production thread has been started, so a second
/// `create_router` (tests, or a re-created router) never starts a second one.
static STARTED: OnceLock<()> = OnceLock::new();

/// Never set in production: the ping thread lives for the process.
static NEVER_STOP: AtomicBool = AtomicBool::new(false);

/// Debug-build verification hook: with this env var set at spawn, the ping
/// thread is not started, so a temp runner can show that the native-UI verdict
/// reads `ping_not_emitted` (not wedged) when no ping is sent. Compiled only
/// under `debug_assertions`; a release build has no such switch.
pub const DEBUG_STOP_ENV: &str = "QONTINUI_DEBUG_STOP_UI_PING";

#[cfg(debug_assertions)]
fn debug_ping_suppressed() -> bool {
    std::env::var_os(DEBUG_STOP_ENV).is_some_and(|v| !v.is_empty())
}

#[cfg(not(debug_assertions))]
fn debug_ping_suppressed() -> bool {
    false
}

/// Wall-clock ms of the ping thread's last tick, `0` when it never ran.
pub fn ping_thread_tick_ms() -> u64 {
    PING_THREAD_TICK_MS.load(Ordering::Relaxed)
}

fn now_ms_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Start the process's one ping thread. Idempotent: a second call is a no-op
/// that returns `Ok(false)`. `Ok(true)` means this call started it.
///
/// A thread that cannot be spawned is logged at `error!` and returned as
/// `Err`: with no ping, the native-UI verdict reads `ping_not_emitted` (UNKNOWN)
/// rather than firing, which is the honest consequence.
pub fn start_ui_ping_thread<R: tauri::Runtime>(
    handle: tauri::AppHandle<R>,
) -> std::io::Result<bool> {
    if STARTED.set(()).is_err() {
        return Ok(false);
    }
    if debug_ping_suppressed() {
        tracing::warn!(
            "ui_ping: {DEBUG_STOP_ENV} is set — the ui-bridge-ping thread is NOT started \
             (debug-build verification hook; the native verdict must read ping_not_emitted)"
        );
        return Ok(false);
    }
    let spawned = std::thread::Builder::new()
        .name(UI_PING_THREAD_NAME.to_string())
        .spawn(move || {
            use tauri::Emitter;
            run_ping_loop(
                || {
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
                &NEVER_STOP,
                &PING_THREAD_TICK_MS,
            );
        });
    match spawned {
        Ok(_) => Ok(true),
        Err(e) => {
            tracing::error!(
                error = %e,
                "ui_ping: could not start the ui-bridge-ping thread — the native-UI \
                 verdict will read ping_not_emitted (UNKNOWN), not wedged"
            );
            Err(e)
        }
    }
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
        tick_stamp.store(now_ms_epoch(), Ordering::Relaxed);
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
    use std::sync::{Arc, Barrier, Mutex};

    /// Park EVERY worker of `rt` in a synchronous sleep, and return only once
    /// all of them are provably parked: each blocking task waits on a barrier
    /// sized `workers + 1`, which can only trip when `workers` distinct
    /// threads are inside it at once.
    fn park_every_worker(rt: &tokio::runtime::Runtime, workers: usize, for_: Duration) {
        let barrier = Arc::new(Barrier::new(workers + 1));
        for _ in 0..workers {
            let b = barrier.clone();
            rt.spawn(async move {
                b.wait();
                std::thread::sleep(for_);
            });
        }
        barrier.wait();
    }

    fn runtime(workers: usize, name: &str) -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .thread_name(name)
            .enable_all()
            .build()
            .expect("test runtime")
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
    // parked") is asserted in ~2 s instead of ~12 s. Ratio of window to
    // cadence (≥ 12 ticks) and the gap bound (cadence + 50 %) match the plan's
    // 10 s / 3 s / 3.5 s shape with margin for a loaded CI box.
    const TEST_CADENCE: Duration = Duration::from_millis(150);
    const TEST_WINDOW: Duration = Duration::from_millis(1_950);

    #[test]
    fn the_ping_keeps_its_cadence_while_every_runtime_worker_is_blocked() {
        // An `app-rt`-shaped runtime (16 workers) and an `mcp-api-rt`-shaped
        // one (4), both fully parked for longer than the measurement window.
        let app_rt = runtime(16, "app-rt");
        let api_rt = runtime(4, "mcp-api-rt");
        park_every_worker(&app_rt, 16, TEST_WINDOW * 3);
        park_every_worker(&api_rt, 4, TEST_WINDOW * 3);

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

        let emits = emits.lock().unwrap().clone();
        assert!(
            emits.len() >= 10,
            "only {} emits in {TEST_WINDOW:?} at a {TEST_CADENCE:?} cadence while every \
             runtime worker was parked",
            emits.len()
        );
        let gap = max_gap(&emits);
        assert!(
            gap <= TEST_CADENCE + TEST_CADENCE / 2,
            "max inter-emit gap {gap:?} exceeds 1.5x the {TEST_CADENCE:?} cadence"
        );
        assert!(tick.load(Ordering::Relaxed) > 0, "the tick stamp never moved");

        app_rt.shutdown_background();
        api_rt.shutdown_background();
    }

    /// The mutation the plan names, kept in the suite as a negative control:
    /// the SAME emit cadence driven as a tokio task on the parked runtime (the
    /// deleted `create_router` shape) does NOT keep its cadence. If this ever
    /// passes the cadence assertion, the test above has stopped measuring
    /// anything.
    #[test]
    fn a_ping_task_on_a_parked_runtime_misses_its_cadence() {
        let app_rt = runtime(2, "app-rt");
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
        park_every_worker(&app_rt, 2, TEST_WINDOW * 3);
        std::thread::sleep(TEST_WINDOW);
        let during = emits.lock().unwrap().len() - before;
        app_rt.shutdown_background();
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
        // After the slow emit the next tick lands on the next cadence
        // boundary, never immediately (no burst of the skipped ticks).
        let after_slow = emits[2].duration_since(emits[1]);
        assert!(
            after_slow >= TEST_CADENCE * 7 / 2,
            "the slow emit was not waited out: {after_slow:?}"
        );
        let next = emits[3].duration_since(emits[2]);
        assert!(
            next >= TEST_CADENCE / 2,
            "skipped ticks were burst: the tick after the late one came {next:?} later"
        );
    }

    /// The debug hook must not ship: the env read exists only in the
    /// `debug_assertions` arm, and the release arm is a constant `false`.
    #[test]
    fn the_stop_hook_is_compiled_only_into_debug_builds() {
        let src = include_str!("ui_ping.rs").replace("\r\n", "\n");
        let prod = src.split("\n#[cfg(test)]\nmod tests").next().unwrap();
        let debug_arm = prod
            .split("#[cfg(debug_assertions)]\nfn debug_ping_suppressed")
            .nth(1)
            .expect("a debug_assertions-gated debug_ping_suppressed");
        assert!(debug_arm.contains("DEBUG_STOP_ENV"));
        let release_arm = prod
            .split("#[cfg(not(debug_assertions))]\nfn debug_ping_suppressed() -> bool {")
            .nth(1)
            .expect("a release arm");
        assert!(
            release_arm.trim_start().starts_with("false"),
            "the release arm must be a constant false, not an env read"
        );
        assert_eq!(prod.matches("var_os(DEBUG_STOP_ENV)").count(), 1);
    }

    /// Production wiring: `ui_ping.rs` is the ONLY non-test source that names
    /// the `ui-bridge-ping` event in a string literal, so no second emitter
    /// (in particular the deleted `create_router` task) can come back. Parsed
    /// with `syn`, so comments and doc text that mention the event do not
    /// count — only literals in code do. Test modules are skipped.
    #[test]
    fn ui_ping_is_the_only_emitter_of_the_ping_event() {
        use syn::visit::{self, Visit};

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
                // Literals inside macro arguments (`json!`, `format!`, …).
                let lits = m.tokens.clone().into_iter().filter_map(|tt| match tt {
                    proc_macro2::TokenTree::Literal(l) => syn::parse_str::<syn::LitStr>(&l.to_string()).ok(),
                    _ => None,
                });
                for l in lits {
                    if l.value() == "ui-bridge-ping" {
                        self.found.push(0);
                    }
                }
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
        assert!(walked > 200, "walked only {walked} .rs files — the scan saw nothing");
        assert!(
            in_ui_ping >= 1,
            "ui_ping.rs no longer names the event — the scan is not finding the real emitter"
        );
        assert!(
            offenders.is_empty(),
            "`ui-bridge-ping` is named as a string literal outside ui_ping.rs: {offenders:?}. \
             The ping must have exactly one emitter, the OS thread in ui_ping.rs."
        );
    }
}
