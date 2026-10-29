//! Runtime scheduling lag: is each tokio runtime actually being scheduled?
//!
//! ## Why this exists
//!
//! On spaceship (2026-10-18/19) every `CreateProcessW` waited 0.7–4.2 s in a
//! kernel push-lock convoy. `tokio::process::Command::spawn` creates the
//! process synchronously on the calling thread (tokio 1.50.0
//! `process/mod.rs`, `self.std.spawn()`), so the runner's periodic git
//! spawners parked all 16 `app-rt` workers for minutes. Nothing in the runner
//! named that condition: `/livez` probes the API runtime only, and the visible
//! symptom was a false native-UI wedge (plan
//! `2026-10-19-runner-ui-wedge-signal-fires-when-the-app-runtime-is-starved-by-process-spawns`).
//!
//! ## The probe
//!
//! [`register_runtime`] spawns ONE tick task on a runtime; it stamps an atomic
//! every [`RUNTIME_LAG_TICK`]. A dedicated OS thread ([`WATCH_THREAD_NAME`],
//! not tokio, and no `tracing` on its hot path — the same discipline as
//! `wedge_diagnostics`' capture thread) reads every stamp every
//! [`RUNTIME_LAG_WATCH`]. A stamp older than [`RUNTIME_STARVED_AFTER_MS`] means
//! the runtime's workers have not run its scheduler for that long: the
//! runtime is **starved**. The watch thread keeps, per runtime, the stamp's
//! age, whether it is starved now, how many episodes it has had and the
//! current and longest episode.
//!
//! A runtime that has never ticked is UNKNOWN (`null` on the wire), never 0.
//!
//! ## Attribution
//!
//! On the first sample of an episode the watch thread writes ONE bounded
//! `wedge_diagnostics` record (`event: "runtime_starved"`), whose
//! `threads_by_name_and_wait` tally joins each thread's name to its kernel
//! wait reason — "`app-rt`: 16 in `WrPushLock`" names a convoy without WPR.
//! tokio gives the scheduler workers and the blocking pool of one runtime the
//! same thread name, so that tally is per NAME, not per worker; the blocking
//! share of a name is the runner's own lane count
//! (`tracked_blocking_bodies.by_thread`, in the same record). Captures are
//! rate-limited ([`CAPTURE_MIN_INTERVAL`]) so a flapping runtime cannot fill
//! the sink.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// How often each registered runtime's tick task stamps its atomic.
pub const RUNTIME_LAG_TICK: Duration = Duration::from_millis(500);
/// How often the watch thread reads the stamps.
pub const RUNTIME_LAG_WATCH: Duration = Duration::from_millis(250);
/// A tick stamp older than this means the runtime is starved. Ten missed
/// ticks: far outside a GC-like pause or one slow poll, well inside the 30 s
/// windows the UI verdicts use.
pub const RUNTIME_STARVED_AFTER_MS: u64 = 5_000;
/// At most one attribution capture per this interval, process-wide.
pub const CAPTURE_MIN_INTERVAL: Duration = Duration::from_secs(60);
/// The watch thread's name.
pub const WATCH_THREAD_NAME: &str = "runtime-lag-watch";
/// The runtime whose starvation `derived_status` reports.
pub const APP_RUNTIME_NAME: &str = "app-rt";

fn now_ms_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// One registered runtime's live state. Atomics only, so the watch thread and
/// any reader can touch it without a lock that a starved runtime could hold.
#[derive(Debug)]
pub struct RuntimeProbe {
    name: &'static str,
    workers: usize,
    /// Wall-clock ms of the last tick; 0 = never ticked (UNKNOWN).
    last_tick_ms: AtomicU64,
    starved: AtomicBool,
    episodes: AtomicU64,
    /// Wall-clock ms the current episode began (the last tick before it); 0 =
    /// no episode in progress.
    episode_start_ms: AtomicU64,
    longest_episode_ms: AtomicU64,
    /// Wall-clock ms the most recent episode began (kept after it ends); 0 =
    /// no episode since boot. The relay heartbeat's post-hoc field.
    last_episode_start_ms: AtomicU64,
}

impl RuntimeProbe {
    fn new(name: &'static str, workers: usize) -> Self {
        Self {
            name,
            workers,
            last_tick_ms: AtomicU64::new(0),
            starved: AtomicBool::new(false),
            episodes: AtomicU64::new(0),
            episode_start_ms: AtomicU64::new(0),
            longest_episode_ms: AtomicU64::new(0),
            last_episode_start_ms: AtomicU64::new(0),
        }
    }

    /// Stamp a tick (the tick task's whole job).
    pub fn tick(&self) {
        self.last_tick_ms.store(now_ms_epoch(), Ordering::Relaxed);
    }
}

/// What one watch step decided for one runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// No change of state.
    None,
    /// An episode began on this sample.
    Started,
    /// The episode that was in progress ended; its duration in ms.
    Ended(u64),
}

/// The pure state machine the watch thread runs per runtime per sample.
pub fn step(probe: &RuntimeProbe, now_ms: u64, starved_after_ms: u64) -> Transition {
    let last = probe.last_tick_ms.load(Ordering::Relaxed);
    if last == 0 {
        // Never ticked: UNKNOWN, never starved.
        return Transition::None;
    }
    let age = now_ms.saturating_sub(last);
    let was = probe.starved.load(Ordering::Relaxed);
    if age > starved_after_ms {
        let start = if was {
            probe.episode_start_ms.load(Ordering::Relaxed)
        } else {
            probe.episode_start_ms.store(last, Ordering::Relaxed);
            probe.last_episode_start_ms.store(last, Ordering::Relaxed);
            probe.episodes.fetch_add(1, Ordering::Relaxed);
            probe.starved.store(true, Ordering::Relaxed);
            last
        };
        probe
            .longest_episode_ms
            .fetch_max(now_ms.saturating_sub(start), Ordering::Relaxed);
        if was {
            Transition::None
        } else {
            Transition::Started
        }
    } else if was {
        let start = probe.episode_start_ms.swap(0, Ordering::Relaxed);
        probe.starved.store(false, Ordering::Relaxed);
        // The episode ended at the tick that just landed.
        let dur = last.saturating_sub(start);
        probe.longest_episode_ms.fetch_max(dur, Ordering::Relaxed);
        Transition::Ended(dur)
    } else {
        Transition::None
    }
}

fn registry() -> &'static Mutex<Vec<Arc<RuntimeProbe>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Arc<RuntimeProbe>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn probes() -> Vec<Arc<RuntimeProbe>> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Register a runtime: spawn its tick task on `handle` and make sure the
/// watch thread is running. Registering a name twice replaces nothing — the
/// second registration is ignored and its probe returned — so a router that
/// is rebuilt does not double-count.
pub fn register_runtime(
    name: &'static str,
    handle: &tokio::runtime::Handle,
    workers: usize,
) -> Arc<RuntimeProbe> {
    let probe = {
        let mut reg = registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = reg.iter().find(|p| p.name == name) {
            return existing.clone();
        }
        let p = Arc::new(RuntimeProbe::new(name, workers));
        reg.push(p.clone());
        p
    };
    spawn_tick_task(handle, probe.clone(), RUNTIME_LAG_TICK);
    start_watch_thread();
    probe
}

/// The tick task: ON the runtime it measures, by design — a stamp that stops
/// moving is the measurement.
fn spawn_tick_task(handle: &tokio::runtime::Handle, probe: Arc<RuntimeProbe>, period: Duration) {
    handle.spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            probe.tick();
        }
    });
}

/// What the watch thread does on an episode start. Production writes one
/// rate-limited attribution record; tests observe the call.
type OnStarved = Box<dyn Fn(&RuntimeProbe, u64) + Send + Sync>;

fn on_starved() -> &'static OnceLock<OnStarved> {
    static HOOK: OnceLock<OnStarved> = OnceLock::new();
    &HOOK
}

/// Install the attribution capture that runs on every episode start. Called
/// once at boot with the diagnostics directory; a second call is ignored.
///
/// The `WedgeDiagnostics` rig is built HERE, at boot, for the reason
/// `health_monitor::WedgeDetector::with_diagnostics` gives: its capture worker
/// thread must exist before the incident, not be spawned during one.
pub fn install_attribution(dir: std::path::PathBuf) {
    let last_capture: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let diag = Mutex::new(crate::wedge_diagnostics::WedgeDiagnostics::new(dir));
    let _ = on_starved().set(Box::new(move |_probe: &RuntimeProbe, age_ms: u64| {
        {
            let mut last = last_capture
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if last.is_some_and(|t| t.elapsed() < CAPTURE_MIN_INTERVAL) {
                return;
            }
            *last = Some(std::time::Instant::now());
        }
        let workers = worker_tids();
        diag.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .capture_and_append_with_tally(
                "runtime_starved",
                Duration::from_millis(age_ms),
                &workers,
            );
    }));
}

// ─────────────────────────────────────────────────────────────────────────────
// Worker identification
// ─────────────────────────────────────────────────────────────────────────────

/// OS thread ids of every tokio SCHEDULER worker of a registered runtime.
///
/// tokio 1.50.0 fires `on_thread_park` only from scheduler workers
/// (`runtime/scheduler/multi_thread/worker.rs`, `current_thread/mod.rs`), never
/// from the blocking pool, so a runtime built with
/// `.on_thread_park(runtime_lag::note_worker_thread)` names its workers here —
/// which is what separates "16 `app-rt` workers in `WrPushLock`" from the
/// blocking-pool threads that share the name.
fn worker_tid_set() -> &'static Mutex<std::collections::HashSet<u64>> {
    static SET: OnceLock<Mutex<std::collections::HashSet<u64>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// The `on_thread_park` callback. Records the calling thread's OS id ONCE per
/// thread (a thread-local flag), so the per-park cost after the first is one
/// TLS read — `on_thread_park` runs every time a worker goes idle.
pub fn note_worker_thread() {
    thread_local! {
        static NOTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    NOTED.with(|n| {
        if n.get() {
            return;
        }
        n.set(true);
        if let Some(tid) = current_os_tid() {
            worker_tid_set()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(tid);
        }
    });
}

/// A snapshot of every worker id noted so far.
pub fn worker_tids() -> std::collections::HashSet<u64> {
    worker_tid_set()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The calling thread's OS thread id — the id `wedge_diagnostics`' census
/// reports (`SYSTEM_THREAD_INFORMATION.ClientId.UniqueThread` on Windows, the
/// `/proc/self/task/<tid>` entry on Linux). `None` where neither applies.
fn current_os_tid() -> Option<u64> {
    #[cfg(target_os = "windows")]
    {
        // SAFETY: no arguments, no failure mode.
        Some(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() } as u64)
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: gettid has no preconditions and cannot fail (the same call
        // `alloc_breadcrumb` makes).
        Some(unsafe { libc::syscall(libc::SYS_gettid) } as u64)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        None
    }
}

fn start_watch_thread() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name(WATCH_THREAD_NAME.to_string())
        .spawn(|| loop {
            std::thread::sleep(RUNTIME_LAG_WATCH);
            let now = now_ms_epoch();
            for p in probes() {
                if step(&p, now, RUNTIME_STARVED_AFTER_MS) == Transition::Started {
                    if let Some(hook) = on_starved().get() {
                        let age = now.saturating_sub(p.last_tick_ms.load(Ordering::Relaxed));
                        hook(&p, age);
                    }
                }
            }
        });
    if let Err(e) = spawned {
        // Off the hot path: this runs once, at registration.
        tracing::warn!(
            error = %e,
            "runtime_lag: could not start the watch thread — runtimes[].starved stays UNKNOWN"
        );
    }
}

/// One runtime's row on `/health.runtimes[]` and the heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeLagRow {
    pub name: &'static str,
    pub workers: usize,
    /// `None` = never ticked (UNKNOWN).
    pub last_tick_age_ms: Option<u64>,
    /// `None` = never ticked (UNKNOWN).
    pub starved: Option<bool>,
    pub episodes: u64,
    /// Age of the current episode; `None` when none is in progress.
    pub current_episode_ms: Option<u64>,
    pub longest_episode_ms: u64,
}

/// Every registered runtime, read now. Empty only when nothing registered.
pub fn runtime_lag_report() -> Vec<RuntimeLagRow> {
    let now = now_ms_epoch();
    probes().iter().map(|p| row(p, now)).collect()
}

fn row(p: &RuntimeProbe, now: u64) -> RuntimeLagRow {
    let last = p.last_tick_ms.load(Ordering::Relaxed);
    let known = last > 0;
    let start = p.episode_start_ms.load(Ordering::Relaxed);
    RuntimeLagRow {
        name: p.name,
        workers: p.workers,
        last_tick_age_ms: known.then(|| now.saturating_sub(last)),
        starved: known.then(|| p.starved.load(Ordering::Relaxed)),
        episodes: p.episodes.load(Ordering::Relaxed),
        current_episode_ms: (start > 0).then(|| now.saturating_sub(start)),
        longest_episode_ms: p.longest_episode_ms.load(Ordering::Relaxed),
    }
}

/// The APP runtime's starvation history, for the RELAY heartbeat.
///
/// That heartbeat is sent from `app-rt` itself (`heartbeat.rs`), so it can
/// never carry `starved: true` while `app-rt` is starved — it is not running
/// then. What it CAN carry, once `app-rt` is scheduled again, is what happened:
/// how many episodes, the longest, and when the last one began. The live
/// `starved` value rides the fleet heartbeat on `fleet-hb-rt` instead
/// ([`app_runtime_starved`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRuntimeEpisodes {
    pub episodes: u64,
    pub longest_episode_ms: u64,
    /// RFC3339; `None` = no episode since boot.
    pub last_episode_started_at: Option<String>,
}

/// `None` = `app-rt` not registered (UNKNOWN), never an all-zero summary.
pub fn app_runtime_episodes() -> Option<AppRuntimeEpisodes> {
    let probe = probes().into_iter().find(|p| p.name == APP_RUNTIME_NAME)?;
    let last = probe.last_episode_start_ms.load(Ordering::Relaxed);
    Some(AppRuntimeEpisodes {
        episodes: probe.episodes.load(Ordering::Relaxed),
        longest_episode_ms: probe.longest_episode_ms.load(Ordering::Relaxed),
        last_episode_started_at: (last > 0)
            .then(|| chrono::DateTime::<chrono::Utc>::from_timestamp_millis(last as i64))
            .flatten()
            .map(|t| t.to_rfc3339()),
    })
}

/// Is the APP runtime starved right now? `None` = not registered or never
/// ticked (UNKNOWN). The `derived_status` input.
pub fn app_runtime_starved() -> Option<bool> {
    probes()
        .iter()
        .find(|p| p.name == APP_RUNTIME_NAME)
        .and_then(|p| row(p, now_ms_epoch()).starved)
}

/// Debug-build verification hook: `QONTINUI_DEBUG_BLOCK_APP_WORKERS_SECS=<n>`
/// at spawn parks EVERY worker of the given runtime for `n` seconds, once,
/// [`DEBUG_BLOCK_DELAY`] after boot — so a temp runner can show
/// `runtimes[app-rt].starved: true`, a ping that keeps its cadence anyway and
/// a native verdict that does not call the UI wedged. Compiled only under
/// `debug_assertions`; a release build has no such switch, and nothing sets it
/// on a primary.
pub const DEBUG_BLOCK_APP_WORKERS_ENV: &str = "QONTINUI_DEBUG_BLOCK_APP_WORKERS_SECS";
/// How long after boot the debug park starts (lets `/health` bind first).
pub const DEBUG_BLOCK_DELAY: Duration = Duration::from_secs(20);

#[cfg(debug_assertions)]
pub fn debug_block_workers_from_env(handle: &tokio::runtime::Handle, workers: usize) {
    let Some(secs) = std::env::var_os(DEBUG_BLOCK_APP_WORKERS_ENV)
        .and_then(|v| v.to_str().and_then(|s| s.trim().parse::<u64>().ok()))
        .filter(|s| *s > 0)
    else {
        return;
    };
    let handle = handle.clone();
    let _ = std::thread::Builder::new()
        .name("debug-block-app-workers".into())
        .spawn(move || {
            std::thread::sleep(DEBUG_BLOCK_DELAY);
            tracing::warn!(
                secs,
                workers,
                "runtime_lag: {DEBUG_BLOCK_APP_WORKERS_ENV} — parking every app-rt worker"
            );
            for _ in 0..workers {
                handle.spawn(async move { std::thread::sleep(Duration::from_secs(secs)) });
            }
        });
}

#[cfg(not(debug_assertions))]
pub fn debug_block_workers_from_env(_handle: &tokio::runtime::Handle, _workers: usize) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    fn probe_at(last_tick_ms: u64) -> RuntimeProbe {
        let p = RuntimeProbe::new("test-rt", 2);
        p.last_tick_ms.store(last_tick_ms, Ordering::Relaxed);
        p
    }

    #[test]
    fn a_runtime_that_never_ticked_is_unknown_never_starved() {
        let p = probe_at(0);
        assert_eq!(step(&p, 1_000_000, 5_000), Transition::None);
        let r = row(&p, 1_000_000);
        assert_eq!(r.last_tick_age_ms, None);
        assert_eq!(r.starved, None, "never ticked is UNKNOWN, not `false`");
    }

    #[test]
    fn an_episode_starts_holds_and_ends_with_its_duration() {
        let p = probe_at(10_000);
        assert_eq!(
            step(&p, 14_000, 5_000),
            Transition::None,
            "4 s old: not starved"
        );
        assert_eq!(step(&p, 15_001, 5_000), Transition::Started);
        assert_eq!(row(&p, 15_001).starved, Some(true));
        assert_eq!(
            step(&p, 20_000, 5_000),
            Transition::None,
            "still the same episode"
        );
        assert_eq!(p.episodes.load(Ordering::Relaxed), 1);
        assert_eq!(row(&p, 20_000).current_episode_ms, Some(10_000));
        // The runtime ticks again at 21 s: the episode lasted 11 s.
        p.last_tick_ms.store(21_000, Ordering::Relaxed);
        assert_eq!(step(&p, 21_100, 5_000), Transition::Ended(11_000));
        let r = row(&p, 21_100);
        assert_eq!(r.starved, Some(false));
        assert_eq!(r.current_episode_ms, None);
        assert_eq!(r.longest_episode_ms, 11_000);
        assert_eq!(
            p.last_episode_start_ms.load(Ordering::Relaxed),
            10_000,
            "the post-hoc start survives the episode's end"
        );
    }

    fn runtime(workers: usize) -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .thread_name("lag-test-rt")
            .enable_all()
            .build()
            .unwrap()
    }

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

    /// Watch `probe` from `watcher` for `window`; true once a starvation
    /// episode was observed.
    fn observed_starvation(probe: &RuntimeProbe, threshold_ms: u64, window: Duration) -> bool {
        let deadline = std::time::Instant::now() + window;
        while std::time::Instant::now() < deadline {
            if step(probe, now_ms_epoch(), threshold_ms) == Transition::Started {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The probe's whole claim: a runtime whose every worker is parked in
    /// synchronous work is reported starved by a watcher on an OS thread.
    #[test]
    fn a_fully_parked_runtime_is_reported_starved_by_the_os_thread_watcher() {
        let rt = runtime(2);
        let probe = Arc::new(RuntimeProbe::new("lag-test-rt", 2));
        spawn_tick_task(rt.handle(), probe.clone(), Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            probe.last_tick_ms.load(Ordering::Relaxed) > 0,
            "the tick task never ran"
        );
        park_every_worker(&rt, 2, Duration::from_millis(1_500));
        let p = probe.clone();
        let seen =
            std::thread::spawn(move || observed_starvation(&p, 400, Duration::from_millis(1_200)))
                .join()
                .unwrap();
        assert!(
            seen,
            "an OS-thread watcher must see a fully parked runtime starve"
        );
        rt.shutdown_background();
    }

    /// The mutation the plan names, kept as a negative control: the SAME
    /// watcher as a tokio task ON the runtime it measures never runs while
    /// that runtime is parked, so it can never report the starvation.
    #[test]
    fn a_watcher_on_the_measured_runtime_cannot_see_its_starvation() {
        let rt = runtime(2);
        let probe = Arc::new(RuntimeProbe::new("lag-test-rt", 2));
        spawn_tick_task(rt.handle(), probe.clone(), Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(200));
        let saw = Arc::new(AtomicBool::new(false));
        park_every_worker(&rt, 2, Duration::from_millis(1_500));
        let (p, s) = (probe.clone(), saw.clone());
        rt.spawn(async move {
            // Runs only once the park ends — by then the stamp is fresh again.
            if step(&p, now_ms_epoch(), 400) == Transition::Started {
                s.store(true, Ordering::Relaxed);
            }
        });
        std::thread::sleep(Duration::from_millis(1_200));
        assert!(
            !saw.load(Ordering::Relaxed),
            "a watcher on the parked runtime ran during the park — the fixture no longer parks it"
        );
        rt.shutdown_background();
    }

    /// The debug hook that parks `app-rt` for a temp-runner arming check must
    /// not ship: its env read exists only under `debug_assertions`.
    #[test]
    fn the_worker_park_hook_is_compiled_only_into_debug_builds() {
        let src = include_str!("runtime_lag.rs").replace("\r\n", "\n");
        let prod = src.split("\n#[cfg(test)]\nmod tests").next().unwrap();
        assert_eq!(
            prod.matches("var_os(DEBUG_BLOCK_APP_WORKERS_ENV)").count(),
            1
        );
        let gated = prod
            .split("#[cfg(debug_assertions)]\npub fn debug_block_workers_from_env")
            .nth(1)
            .expect("the hook is #[cfg(debug_assertions)]");
        assert!(gated.contains("var_os(DEBUG_BLOCK_APP_WORKERS_ENV)"));
    }
}
