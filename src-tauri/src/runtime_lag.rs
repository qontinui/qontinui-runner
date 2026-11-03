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
//! runtime is **starved**. `starved` is derived at READ time from the stamp's
//! age, so it never depends on the watch thread being alive. The watch thread
//! keeps the episode bookkeeping — how many episodes, the current and longest
//! one, when the last one began, the cumulative starved time — and stamps its
//! own heartbeat each pass; when that heartbeat is older than
//! [`WATCHER_STALE_AFTER_MS`] (or was never stamped) every episode-derived
//! field reads `null` (UNKNOWN).
//!
//! Stamps are MONOTONIC: milliseconds since a process-start `Instant`, plus 1
//! so that 0 still means "never". They become wall-clock only where a
//! timestamp is published (`lastEpisodeStartedAt`).
//!
//! A runtime that has never ticked is UNKNOWN (`null` on the wire), never 0.
//!
//! ## Attribution
//!
//! On the first sample of an episode the watch thread `try_send`s ONE request
//! to a dedicated capture thread started at boot by [`install_attribution`]
//! (it never waits on it), which writes ONE bounded `wedge_diagnostics` record
//! (`event: "runtime_starved"`), whose
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
use std::time::{Duration, Instant};

/// How often each registered runtime's tick task stamps its atomic.
pub const RUNTIME_LAG_TICK: Duration = Duration::from_millis(500);
/// How often the watch thread reads the stamps.
pub const RUNTIME_LAG_WATCH: Duration = Duration::from_millis(250);
/// A tick stamp older than this means the runtime is starved. Ten missed
/// ticks: far outside a GC-like pause or one slow poll, well inside the 30 s
/// windows the UI verdicts use.
pub const RUNTIME_STARVED_AFTER_MS: u64 = 5_000;
/// A watch-thread heartbeat older than this (eight missed passes) means the
/// episode bookkeeping is not being kept: every episode-derived field reads
/// `null` (UNKNOWN). `starved` is unaffected — it is derived from tick age.
pub const WATCHER_STALE_AFTER_MS: u64 = 2_000;
/// At most one attribution capture per this interval, process-wide.
pub const CAPTURE_MIN_INTERVAL: Duration = Duration::from_secs(60);
/// The watch thread's name.
pub const WATCH_THREAD_NAME: &str = "runtime-lag-watch";
/// The attribution capture thread's name.
pub const CAPTURE_THREAD_NAME: &str = "runtime-lag-capture";
/// How many capture requests may queue; the watcher drops any beyond it.
const CAPTURE_QUEUE_CAPACITY: usize = 4;
/// The runtime whose starvation `derived_status` reports.
pub const APP_RUNTIME_NAME: &str = "app-rt";

fn process_start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Monotonic ms since the process-start `Instant`, PLUS 1 — so a stamp of 0
/// still means "never". Immune to wall-clock steps.
fn now_mono_ms() -> u64 {
    (process_start().elapsed().as_millis() as u64).saturating_add(1)
}

fn now_ms_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// A monotonic stamp as wall-clock epoch ms: `now_wall - (now_mono - stamp)`.
fn mono_to_wall_ms(stamp: u64, now_mono: u64, now_wall: u64) -> u64 {
    now_wall.saturating_sub(now_mono.saturating_sub(stamp))
}

/// One registered runtime's live state. Atomics only, so the watch thread and
/// any reader can touch it without a lock that a starved runtime could hold.
/// Every `*_ms` stamp is monotonic ([`now_mono_ms`]); 0 = never.
#[derive(Debug)]
pub struct RuntimeProbe {
    name: &'static str,
    workers: usize,
    /// Monotonic ms of the last tick; 0 = never ticked (UNKNOWN).
    last_tick_ms: AtomicU64,
    /// The WATCHER's episode state (is an episode open in its bookkeeping).
    /// Never read as "starved now" — that is derived from tick age.
    in_episode: AtomicBool,
    episodes: AtomicU64,
    /// Monotonic ms the current episode began (the last tick before it); 0 =
    /// no episode in progress.
    episode_start_ms: AtomicU64,
    longest_episode_ms: AtomicU64,
    /// Monotonic ms the most recent episode began (kept after it ends); 0 =
    /// no episode since boot. The relay heartbeat's post-hoc field.
    last_episode_start_ms: AtomicU64,
    /// Cumulative ms of every ENDED episode (the watcher adds each one's
    /// duration on `Ended`). Readers add the open episode's elapsed time at
    /// read time ([`starved_total_at`]). `ui_error` snapshots it beside each
    /// event pong so only UN-starved time ages a pong.
    starved_total_ms: AtomicU64,
    /// Monotonic ms of the last attribution capture this runtime requested;
    /// 0 = none. Per runtime, so one flapping runtime cannot use up another's
    /// capture.
    last_capture_ms: AtomicU64,
}

impl RuntimeProbe {
    fn new(name: &'static str, workers: usize) -> Self {
        Self {
            name,
            workers,
            last_tick_ms: AtomicU64::new(0),
            in_episode: AtomicBool::new(false),
            episodes: AtomicU64::new(0),
            episode_start_ms: AtomicU64::new(0),
            longest_episode_ms: AtomicU64::new(0),
            last_episode_start_ms: AtomicU64::new(0),
            starved_total_ms: AtomicU64::new(0),
            last_capture_ms: AtomicU64::new(0),
        }
    }

    /// Stamp a tick (the tick task's whole job).
    pub fn tick(&self) {
        self.last_tick_ms.store(now_mono_ms(), Ordering::Relaxed);
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
    let was = probe.in_episode.load(Ordering::Relaxed);
    if age > starved_after_ms {
        let start = if was {
            probe.episode_start_ms.load(Ordering::Relaxed)
        } else {
            probe.episode_start_ms.store(last, Ordering::Relaxed);
            probe.last_episode_start_ms.store(last, Ordering::Relaxed);
            probe.episodes.fetch_add(1, Ordering::Relaxed);
            // Release: a reader that sees `in_episode` also sees the start.
            probe.in_episode.store(true, Ordering::Release);
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
        let start = probe.episode_start_ms.load(Ordering::Relaxed);
        // The episode ended at the tick that just landed.
        let dur = last.saturating_sub(start);
        probe.longest_episode_ms.fetch_max(dur, Ordering::Relaxed);
        // Add to the closed total BEFORE closing the episode, so a concurrent
        // `starved_total_at` can only double-count it (which leans toward
        // firing the wedge rung), never miss it (which would over-mask).
        probe.starved_total_ms.fetch_add(dur, Ordering::Release);
        probe.in_episode.store(false, Ordering::Release);
        probe.episode_start_ms.store(0, Ordering::Release);
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

// ─────────────────────────────────────────────────────────────────────────────
// Attribution capture
// ─────────────────────────────────────────────────────────────────────────────

/// One capture request: which runtime starved, and how old its stamp was.
struct CaptureRequest {
    /// The runtime whose episode triggered it. The capture itself is
    /// process-wide (every thread's name × wait), so the record does not
    /// carry it today; kept so the message names its cause.
    #[allow(dead_code)]
    runtime: &'static str,
    age_ms: u64,
}

/// The capture thread's inbox. Set once by [`install_attribution`]; unset
/// means no attribution was installed and the watcher sends nothing.
fn capture_tx() -> &'static OnceLock<std::sync::mpsc::SyncSender<CaptureRequest>> {
    static TX: OnceLock<std::sync::mpsc::SyncSender<CaptureRequest>> = OnceLock::new();
    &TX
}

/// The watcher's side of an episode start: rate-limit on the probe's own
/// slot (check-and-set), then `try_send`. Never blocks — a full queue or a
/// dead capture thread drops the request. Does nothing when
/// [`install_attribution`] was never called.
fn request_capture(probe: &RuntimeProbe, age_ms: u64, now_mono: u64) {
    let Some(tx) = capture_tx().get() else {
        return;
    };
    let last = probe.last_capture_ms.load(Ordering::Relaxed);
    if last != 0 && now_mono.saturating_sub(last) < CAPTURE_MIN_INTERVAL.as_millis() as u64 {
        return;
    }
    if probe
        .last_capture_ms
        .compare_exchange(last, now_mono, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let _ = tx.try_send(CaptureRequest {
        runtime: probe.name,
        age_ms,
    });
}

/// Install the attribution capture that runs on every episode start. Called
/// once at boot with the diagnostics directory; a second call is ignored.
///
/// Starts ONE dedicated capture thread ([`CAPTURE_THREAD_NAME`]) here, at
/// boot, and builds the `WedgeDiagnostics` rig here too, for the reason
/// `health_monitor::WedgeDetector::with_diagnostics` gives: a capture thread
/// must exist before the incident, not be spawned during one. The watch
/// thread only `try_send`s to it on a bounded queue and never waits, so a
/// slow per-thread walk can never stall the watcher.
pub fn install_attribution(dir: std::path::PathBuf) {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<CaptureRequest>(CAPTURE_QUEUE_CAPACITY);
    let mut diag = crate::wedge_diagnostics::WedgeDiagnostics::new(dir);
    let spawned = std::thread::Builder::new()
        .name(CAPTURE_THREAD_NAME.to_string())
        .spawn(move || {
            while let Ok(req) = rx.recv() {
                let workers = worker_tids();
                diag.capture_and_append_with_tally(
                    "runtime_starved",
                    Duration::from_millis(req.age_ms),
                    &workers,
                );
            }
        });
    match spawned {
        Ok(_) => {
            let _ = capture_tx().set(tx);
        }
        Err(e) => {
            // Off the hot path: this runs once, at boot.
            tracing::warn!(
                error = %e,
                "runtime_lag: could not start the attribution capture thread — \
                 no runtime_starved record will be written"
            );
        }
    }
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
/// blocking-pool threads that share the name. Threads leave the set when they
/// exit (`.on_thread_stop(runtime_lag::forget_worker_thread)`).
///
/// APPROXIMATE: `tokio::task::block_in_place` hands the calling worker's core
/// to a blocking-pool thread, which then runs as a worker (and is noted on its
/// first park) while the original thread — still in the set — runs the
/// blocking closure. So the `[worker]` split of the tally can over-count.
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

/// The `on_thread_stop` callback: drop the exiting thread's OS id from the
/// worker set, so a reused id is never mistaken for a worker. Fires for
/// blocking-pool threads too, where it removes nothing.
pub fn forget_worker_thread() {
    if let Some(tid) = current_os_tid() {
        worker_tid_set()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&tid);
    }
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

// ─────────────────────────────────────────────────────────────────────────────
// The watch thread
// ─────────────────────────────────────────────────────────────────────────────

/// Monotonic ms of the watch thread's last pass; 0 = it never ran.
static WATCH_HEARTBEAT_MS: AtomicU64 = AtomicU64::new(0);

/// Is the episode bookkeeping live at `now` (monotonic)? False when the watch
/// thread never ran or its heartbeat is older than [`WATCHER_STALE_AFTER_MS`].
fn watcher_alive(now: u64) -> bool {
    let hb = WATCH_HEARTBEAT_MS.load(Ordering::Relaxed);
    hb != 0 && now.saturating_sub(hb) <= WATCHER_STALE_AFTER_MS
}

/// One pass of the watch thread at monotonic `now`: stamp `heartbeat`, step
/// every probe, and on an episode start hand the capture thread a request
/// (never waiting on it). Production passes [`WATCH_HEARTBEAT_MS`].
fn watch_once(probes: &[Arc<RuntimeProbe>], now: u64, heartbeat: &AtomicU64) {
    heartbeat.store(now, Ordering::Relaxed);
    for p in probes {
        if step(p, now, RUNTIME_STARVED_AFTER_MS) == Transition::Started {
            let age = now.saturating_sub(p.last_tick_ms.load(Ordering::Relaxed));
            request_capture(p, age, now);
        }
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
            watch_once(&probes(), now_mono_ms(), &WATCH_HEARTBEAT_MS);
        });
    if let Err(e) = spawned {
        // Off the hot path: this runs once, at registration.
        tracing::warn!(
            error = %e,
            "runtime_lag: could not start the watch thread — runtimes[].starved is still \
             derived from tick age, but the episode fields (episodes, currentEpisodeMs, \
             longestEpisodeMs, lastEpisodeStartedAt) read null (UNKNOWN) and no \
             runtime_starved attribution is captured"
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
    /// Derived from `last_tick_age_ms` at read time. `None` = never ticked
    /// (UNKNOWN).
    pub starved: Option<bool>,
    /// `None` = the watch thread is not keeping the bookkeeping (UNKNOWN).
    pub episodes: Option<u64>,
    /// Age of the current episode; `None` when none is in progress, or when
    /// `episodes` is `None` (UNKNOWN).
    pub current_episode_ms: Option<u64>,
    /// `None` = UNKNOWN, as `episodes`.
    pub longest_episode_ms: Option<u64>,
}

/// Every registered runtime, read now. Empty only when nothing registered.
pub fn runtime_lag_report() -> Vec<RuntimeLagRow> {
    let now = now_mono_ms();
    let alive = watcher_alive(now);
    probes().iter().map(|p| row(p, now, alive)).collect()
}

/// `starved` at monotonic `now`: known && age > [`RUNTIME_STARVED_AFTER_MS`].
fn starved_at(p: &RuntimeProbe, now: u64) -> Option<bool> {
    let last = p.last_tick_ms.load(Ordering::Relaxed);
    (last > 0).then(|| now.saturating_sub(last) > RUNTIME_STARVED_AFTER_MS)
}

fn row(p: &RuntimeProbe, now: u64, watcher_alive: bool) -> RuntimeLagRow {
    let last = p.last_tick_ms.load(Ordering::Relaxed);
    let known = last > 0;
    let start = p.episode_start_ms.load(Ordering::Relaxed);
    RuntimeLagRow {
        name: p.name,
        workers: p.workers,
        last_tick_age_ms: known.then(|| now.saturating_sub(last)),
        starved: starved_at(p, now),
        episodes: watcher_alive.then(|| p.episodes.load(Ordering::Relaxed)),
        current_episode_ms: (watcher_alive && start > 0).then(|| now.saturating_sub(start)),
        longest_episode_ms: watcher_alive.then(|| p.longest_episode_ms.load(Ordering::Relaxed)),
    }
}

/// The APP runtime's starvation history, for the RELAY heartbeat.
///
/// That heartbeat is sent from `app-rt` itself (`heartbeat.rs`), so it can
/// never carry `starved: true` while `app-rt` is starved — it is not running
/// then. What it CAN carry, once `app-rt` is scheduled again, is what happened:
/// how many episodes, the longest, and when the last one began. The live
/// `starved` value rides the fleet heartbeat on `fleet-hb-rt` instead
/// ([`app_runtime_starved`]). Every field is `None` (UNKNOWN) while the watch
/// thread is not keeping the bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRuntimeEpisodes {
    /// `None` = UNKNOWN (watch thread not live).
    pub episodes: Option<u64>,
    /// `None` = UNKNOWN (watch thread not live).
    pub longest_episode_ms: Option<u64>,
    /// RFC3339; `None` = no episode since boot, or UNKNOWN when `episodes` is
    /// `None`.
    pub last_episode_started_at: Option<String>,
}

/// `None` = `app-rt` not registered (UNKNOWN), never an all-zero summary.
pub fn app_runtime_episodes() -> Option<AppRuntimeEpisodes> {
    let probe = probes().into_iter().find(|p| p.name == APP_RUNTIME_NAME)?;
    let now_mono = now_mono_ms();
    if !watcher_alive(now_mono) {
        return Some(AppRuntimeEpisodes {
            episodes: None,
            longest_episode_ms: None,
            last_episode_started_at: None,
        });
    }
    let last = probe.last_episode_start_ms.load(Ordering::Relaxed);
    let now_wall = now_ms_epoch();
    Some(AppRuntimeEpisodes {
        episodes: Some(probe.episodes.load(Ordering::Relaxed)),
        longest_episode_ms: Some(probe.longest_episode_ms.load(Ordering::Relaxed)),
        last_episode_started_at: (last > 0)
            .then(|| {
                let wall = mono_to_wall_ms(last, now_mono, now_wall);
                chrono::DateTime::<chrono::Utc>::from_timestamp_millis(wall as i64)
            })
            .flatten()
            .map(|t| t.to_rfc3339()),
    })
}

/// Is the APP runtime starved right now? Derived from the tick stamp's age at
/// read time. `None` = not registered or never ticked (UNKNOWN). The
/// `derived_status` input.
pub fn app_runtime_starved() -> Option<bool> {
    probes()
        .iter()
        .find(|p| p.name == APP_RUNTIME_NAME)
        .and_then(|p| starved_at(p, now_mono_ms()))
}

/// Cumulative starved ms of `p` at monotonic `now`: every ended episode plus
/// the open one's elapsed time.
fn starved_total_at(p: &RuntimeProbe, now: u64) -> u64 {
    // Open episode first, closed total LAST (pairs with `step`'s Ended
    // order): a read torn across an episode's end double-counts it rather
    // than missing it.
    let in_episode = p.in_episode.load(Ordering::Acquire);
    let start = p.episode_start_ms.load(Ordering::Acquire);
    let open = if in_episode && start > 0 {
        now.saturating_sub(start)
    } else {
        0
    };
    let closed = p.starved_total_ms.load(Ordering::Acquire);
    closed.saturating_add(open)
}

/// Cumulative ms `app-rt` has spent starved since boot (monotonic, open
/// episode included). `None` = UNKNOWN: not registered, or the watch thread
/// is not keeping the bookkeeping. `ui_error` snapshots it beside each event
/// pong, so the difference measures the starved share of a pong's age.
pub fn app_runtime_starved_total_ms() -> Option<u64> {
    let probe = probes().into_iter().find(|p| p.name == APP_RUNTIME_NAME)?;
    let now = now_mono_ms();
    watcher_alive(now).then(|| starved_total_at(&probe, now))
}

/// Verification hook: `QONTINUI_DEBUG_BLOCK_APP_WORKERS_SECS=<n>` at spawn
/// parks EVERY worker of the given runtime for `n` seconds, once,
/// [`DEBUG_BLOCK_DELAY`] after boot — so a temp runner can show
/// `runtimes[app-rt].starved: true`, a ping that keeps its cadence anyway and
/// a native verdict that does not call the UI wedged. Compiled into builds
/// with the `debug-hooks` feature only (temp/test runners); the
/// release-checked primary keeps `debug_assertions` on, so `debug_assertions`
/// is not a safe gate. Every other build has the no-op twin.
pub const DEBUG_BLOCK_APP_WORKERS_ENV: &str = "QONTINUI_DEBUG_BLOCK_APP_WORKERS_SECS";
/// How long after boot the debug park starts (lets `/health` bind first).
pub const DEBUG_BLOCK_DELAY: Duration = Duration::from_secs(20);

/// The hook's requested park, in seconds. `None` = no park. Only the
/// `debug-hooks` arm reads the environment.
#[cfg(feature = "debug-hooks")]
fn debug_block_secs() -> Option<u64> {
    std::env::var_os(DEBUG_BLOCK_APP_WORKERS_ENV)
        .and_then(|v| v.to_str().and_then(|s| s.trim().parse::<u64>().ok()))
        .filter(|s| *s > 0)
}

#[cfg(not(feature = "debug-hooks"))]
fn debug_block_secs() -> Option<u64> {
    None
}

#[cfg(feature = "debug-hooks")]
pub fn debug_block_workers_from_env(handle: &tokio::runtime::Handle, workers: usize) {
    let Some(secs) = debug_block_secs() else {
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
            // A barrier of `workers` parties: no task starts sleeping until
            // every one holds a distinct worker, so EVERY worker is parked.
            let barrier = Arc::new(std::sync::Barrier::new(workers.max(1)));
            for _ in 0..workers.max(1) {
                let b = barrier.clone();
                handle.spawn(async move {
                    b.wait();
                    std::thread::sleep(Duration::from_secs(secs));
                });
            }
        });
}

/// The no-op twin: without the `debug-hooks` feature there is no switch.
#[cfg(not(feature = "debug-hooks"))]
pub fn debug_block_workers_from_env(_handle: &tokio::runtime::Handle, _workers: usize) {
    // `debug_block_secs` is the constant `None` here; reading it keeps the
    // twin and the inert-hook test on one definition of "no switch".
    let _ = debug_block_secs();
}

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
        let r = row(&p, 1_000_000, true);
        assert_eq!(r.last_tick_age_ms, None);
        assert_eq!(r.starved, None, "never ticked is UNKNOWN, not `false`");
    }

    #[test]
    fn an_episode_starts_holds_and_ends_with_its_duration() {
        // Stamps are monotonic ms (+1); the values are arbitrary offsets.
        let p = probe_at(10_000);
        assert_eq!(
            step(&p, 14_000, 5_000),
            Transition::None,
            "4 s old: not starved"
        );
        assert_eq!(step(&p, 15_001, 5_000), Transition::Started);
        assert_eq!(row(&p, 15_001, true).starved, Some(true));
        assert_eq!(
            step(&p, 20_000, 5_000),
            Transition::None,
            "still the same episode"
        );
        assert_eq!(p.episodes.load(Ordering::Relaxed), 1);
        assert_eq!(row(&p, 20_000, true).current_episode_ms, Some(10_000));
        // The runtime ticks again at 21 s: the episode lasted 11 s.
        p.last_tick_ms.store(21_000, Ordering::Relaxed);
        assert_eq!(step(&p, 21_100, 5_000), Transition::Ended(11_000));
        let r = row(&p, 21_100, true);
        assert_eq!(r.starved, Some(false));
        assert_eq!(r.current_episode_ms, None);
        assert_eq!(r.longest_episode_ms, Some(11_000));
        assert_eq!(
            p.last_episode_start_ms.load(Ordering::Relaxed),
            10_000,
            "the post-hoc start survives the episode's end"
        );
    }

    /// `starved` is read off the stamp's age, so a runtime whose watcher never
    /// stepped it (or died) still reads starved — the watcher only owns the
    /// episode bookkeeping, which then reads UNKNOWN.
    #[test]
    fn starved_is_derived_at_read_time_and_a_dead_watcher_makes_episodes_unknown() {
        let p = probe_at(10_000);
        // No `step` ever ran on this probe.
        let r = row(&p, 15_001, true);
        assert_eq!(r.starved, Some(true), "read-time, not a watcher flag");
        assert_eq!(r.episodes, Some(0));
        let dead = row(&p, 15_001, false);
        assert_eq!(dead.starved, Some(true));
        assert_eq!(dead.episodes, None, "watcher not live: UNKNOWN, not 0");
        assert_eq!(dead.current_episode_ms, None);
        assert_eq!(dead.longest_episode_ms, None);
    }

    #[test]
    fn a_monotonic_stamp_converts_to_wall_clock_by_its_age() {
        assert_eq!(
            mono_to_wall_ms(4_000, 10_000, 1_700_000_000_000),
            1_699_999_994_000
        );
        assert_eq!(
            mono_to_wall_ms(10_000, 10_000, 1_700_000_000_000),
            1_700_000_000_000
        );
        assert!(now_mono_ms() >= 1, "0 is reserved for never");
    }

    /// A test runtime built as a LOCAL, so the disallowed `Runtime` type is
    /// never named (clippy.toml `disallowed-types`; `expect_ratchet.rs`).
    macro_rules! runtime {
        ($workers:expr) => {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads($workers)
                .thread_name("lag-test-rt")
                .enable_all()
                .build()
                .unwrap()
        };
    }

    fn park_every_worker(rt: &tokio::runtime::Handle, workers: usize, for_: Duration) {
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

    /// Watch `probe` for `window`; the instant a starvation episode was first
    /// observed, or `None`. The INSTANT matters: a watcher that only runs
    /// after the park ends can still read the stale stamp once and report the
    /// episode post hoc, so the caller must check it was seen DURING the park.
    fn observed_starvation(
        probe: &RuntimeProbe,
        threshold_ms: u64,
        window: Duration,
    ) -> Option<Instant> {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            if step(probe, now_mono_ms(), threshold_ms) == Transition::Started {
                return Some(Instant::now());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    /// The probe's whole claim: a runtime whose every worker is parked in
    /// synchronous work is reported starved by a watcher on an OS thread.
    #[test]
    fn a_fully_parked_runtime_is_reported_starved_by_the_os_thread_watcher() {
        let rt = runtime!(2);
        let probe = Arc::new(RuntimeProbe::new("lag-test-rt", 2));
        spawn_tick_task(rt.handle(), probe.clone(), Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            probe.last_tick_ms.load(Ordering::Relaxed) > 0,
            "the tick task never ran"
        );
        let park = Duration::from_millis(1_500);
        // Taken BEFORE the park: the workers start sleeping only once the
        // barrier releases, so the park ends no earlier than `park_floor`.
        let park_floor = Instant::now() + park;
        park_every_worker(rt.handle(), 2, park);
        let p = probe.clone();
        let seen_at =
            std::thread::spawn(move || observed_starvation(&p, 400, Duration::from_millis(1_200)))
                .join()
                .unwrap();
        let seen_at = seen_at.expect("an OS-thread watcher must see a fully parked runtime starve");
        // A correct watcher sees the episode ~threshold + one tick (~450 ms)
        // into the park; demanding it 500 ms before the end leaves no room for
        // a watcher that only ran once the runtime resumed.
        assert!(
            seen_at + Duration::from_millis(500) < park_floor,
            "the starvation must be seen WHILE the runtime is parked, not read post hoc \
             off a stale stamp after it resumes"
        );
        rt.shutdown_background();
    }

    /// The mutation the plan names, kept as a negative control: the SAME
    /// watcher as a tokio task ON the runtime it measures never runs while
    /// that runtime is parked, so it can never report the starvation while
    /// it is happening.
    #[test]
    fn a_watcher_on_the_measured_runtime_cannot_see_its_starvation() {
        let rt = runtime!(2);
        let probe = Arc::new(RuntimeProbe::new("lag-test-rt", 2));
        spawn_tick_task(rt.handle(), probe.clone(), Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(200));
        let park = Duration::from_millis(1_500);
        // Taken BEFORE the park: the workers start sleeping only once the
        // barrier releases, so the park cannot end before `park_floor`.
        let park_floor = Instant::now() + park;
        park_every_worker(rt.handle(), 2, park);
        // When the on-runtime watcher ran, and whether it saw an episode start.
        let ran: Arc<Mutex<Option<(Instant, bool)>>> = Arc::new(Mutex::new(None));
        let (p, r) = (probe.clone(), ran.clone());
        rt.spawn(async move {
            let saw = step(&p, now_mono_ms(), 400) == Transition::Started;
            *r.lock().unwrap() = Some((Instant::now(), saw));
        });
        // It DOES run once the park ends (bounded wait).
        let deadline = Instant::now() + Duration::from_secs(5);
        let (ran_at, _saw_started) = loop {
            if let Some(v) = *ran.lock().unwrap() {
                break v;
            }
            assert!(
                Instant::now() < deadline,
                "the on-runtime watcher never ran after the park ended"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(
            ran_at >= park_floor,
            "a watcher on the parked runtime ran {:?} before the park could have ended — \
             the fixture no longer parks it",
            park_floor.saturating_duration_since(ran_at)
        );
        // What it read when it finally ran is not asserted either way: it is
        // queued on the injection queue, while the tick needs the timer
        // driver, so which runs first after the park is the scheduler's
        // choice — it may still read the stale stamp and report `Started`.
        // Either way that report came at or after `park_floor`: post hoc,
        // never WHILE the runtime was parked, which is the property.
        rt.shutdown_background();
    }

    /// End to end through the production path: `register_runtime` (its tick
    /// task and the real watch thread running `watch_once`) on a 2-worker
    /// runtime, every worker parked, and `runtime_lag_report()` read from
    /// this OS thread shows the row starved — with the watcher's episode open
    /// — WHILE the runtime is parked.
    #[test]
    fn a_registered_runtime_reads_starved_on_the_report_while_parked() {
        const NAME: &str = "lag-test-registered-rt";
        let rt = runtime!(2);
        register_runtime(NAME, rt.handle(), 2);
        let find = || {
            runtime_lag_report()
                .into_iter()
                .find(|r| r.name == NAME)
                .expect("the registered row is reported")
        };
        let ticked_by = Instant::now() + Duration::from_secs(5);
        while find().last_tick_age_ms.is_none() {
            assert!(Instant::now() < ticked_by, "the tick task never ran");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(find().starved, Some(false));
        // Long enough to exceed the production threshold (5 s) plus one
        // production tick (500 ms), with a second of margin before the
        // 500 ms-before-the-end deadline.
        let park = Duration::from_millis(RUNTIME_STARVED_AFTER_MS + 2_000);
        // Taken BEFORE the park, so the deadline is a lower bound on the
        // park's end: every sample before it is a sample while parked.
        let park_floor = Instant::now() + park;
        park_every_worker(rt.handle(), 2, park);
        let deadline = park_floor - Duration::from_millis(500);
        let mut seen = None;
        while Instant::now() < deadline {
            let r = find();
            if r.starved == Some(true) && r.current_episode_ms.is_some() {
                seen = Some(r);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let r = seen.expect(
            "runtime_lag_report() must show the parked runtime starved, with the watch \
             thread's episode open, before the park ends",
        );
        assert!(r.episodes.is_some_and(|n| n >= 1), "{r:?}");
        rt.shutdown_background();
    }

    /// `watch_once` steps every probe and stamps the watcher heartbeat; with
    /// no attribution installed it requests nothing.
    #[test]
    fn watch_once_steps_every_probe_and_stamps_the_heartbeat() {
        let a = Arc::new(probe_at(10_000));
        let b = Arc::new(probe_at(14_000));
        let heartbeat = AtomicU64::new(0);
        watch_once(&[a.clone(), b.clone()], 15_001, &heartbeat);
        assert_eq!(a.episodes.load(Ordering::Relaxed), 1);
        assert_eq!(b.episodes.load(Ordering::Relaxed), 0);
        assert_eq!(heartbeat.load(Ordering::Relaxed), 15_001);
    }

    /// The cumulative starved time: every ended episode, plus the open one's
    /// elapsed time at read time.
    #[test]
    fn starved_total_sums_ended_episodes_and_the_open_one() {
        let p = probe_at(10_000);
        assert_eq!(starved_total_at(&p, 12_000), 0);
        assert_eq!(step(&p, 15_001, 5_000), Transition::Started);
        assert_eq!(starved_total_at(&p, 17_000), 7_000, "open: 17 s - 10 s");
        p.last_tick_ms.store(21_000, Ordering::Relaxed);
        assert_eq!(step(&p, 21_100, 5_000), Transition::Ended(11_000));
        assert_eq!(starved_total_at(&p, 30_000), 11_000);
        // A second episode, 60 s later.
        p.last_tick_ms.store(81_000, Ordering::Relaxed);
        assert_eq!(step(&p, 86_001, 5_000), Transition::Started);
        assert_eq!(starved_total_at(&p, 90_000), 11_000 + 9_000);
        p.last_tick_ms.store(95_000, Ordering::Relaxed);
        assert_eq!(step(&p, 95_100, 5_000), Transition::Ended(14_000));
        assert_eq!(starved_total_at(&p, 200_000), 25_000);
    }

    /// The debug hook that parks `app-rt` must not ship: without the
    /// `debug-hooks` feature it reads no switch at all.
    /// (`debug_assertions` is not the gate — the release-checked primary keeps
    /// it on.)
    #[cfg(not(feature = "debug-hooks"))]
    #[test]
    fn the_worker_park_hook_is_inert_without_the_debug_hooks_feature() {
        assert_eq!(debug_block_secs(), None);
    }
}
