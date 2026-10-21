//! OS health: kernel objects, kernel pool and OS process-creation latency,
//! measured on EVERY runner instance.
//!
//! Plan `2026-10-19-fleet-is-blind-to-kernel-object-leaks-and-os-process-creation-latency`,
//! Phase 4. On 2026-10-15 spaceship booted into a `win32kfull.sys`
//! (10.0.26100.9711) that leaks a primary-token reference on every console
//! attach. Over 3.75 days it leaked 4.2M Token objects and ~40 GB of kernel
//! pool, and every process creation slowed from tens of ms to seconds — while
//! CPU, memory, `/health` and coord's headroom alert all read healthy. Nothing
//! in the fleet measured any of the three things that were wrong. This module
//! measures them:
//!
//! | Reading | Windows | Linux |
//! |---|---|---|
//! | kernel paged / nonpaged bytes | `GetPerformanceInfo` × page size | `SReclaimable` / `SUnreclaim + KernelStack + PageTables` |
//! | handle count | `GetPerformanceInfo` | `/proc/sys/fs/file-nr` |
//! | Token / Process objects | `NtQueryObject(ObjectTypesInformation)` | NOT APPLICABLE |
//! | live process count | `GetPerformanceInfo` | `/proc/[0-9]*` |
//! | boot instant | `now - GetTickCount64` | `/proc/stat` `btime` |
//! | top 8 pool tags | `SystemPoolTagInformation`, every 5th tick | NOT APPLICABLE |
//! | spawn latency | `cmd.exe /c exit` every 60 s | `/bin/true` every 60 s |
//!
//! ## One `os-health` thread, on every instance (D1)
//!
//! The 30 s resource sampler cannot carry this: it is driven from
//! `fleet::spawn_budget_republisher`, which returns early on a secondary
//! instance, and `resource_sample::publish_once` returns before collecting
//! when there is no `machine.json`, coord base or device JWT. A temp runner,
//! or an end user's unpaired runner, would never measure anything. So the
//! collector is its own `std::thread`, started from `run_app` before any of
//! those gates exist, and stores an [`OsHealthSnapshot`] in a process-global
//! cell. Three readers take it from there without ever triggering a read:
//! `/health` `osHealth` ([`health_json`]), the resource sample's host lane
//! ([`sample_fields`], which only the PRIMARY publishes — the secondary gate
//! still governs publishing), and Phase 5's local verdict and banner
//! ([`current`]).
//!
//! **Never on a tokio runtime.** The probe is a blocking `CreateProcess`, and a
//! blocking `CreateProcess` on the 4-worker `mcp-api-rt` runtime is exactly
//! what starved the runner's ping task during the incident. The thread here
//! keeps measuring while every runtime in the process is starved, which is the
//! condition it exists to see.
//!
//! ## UNKNOWN is never a default; NOT APPLICABLE is not UNKNOWN (D6)
//!
//! Every reading is an `Option`: `None` means never read, an APPLICABLE read
//! failed, or the reading does not exist on this OS — never 0. `kernel_source`
//! names the instruments that ANSWERED (`windows:getperformanceinfo+
//! objecttypes+pooltags`, `linux:proc-meminfo+file-nr+proc-stat+proc-pids`),
//! and [`OsPlatform`] says which readings this OS has at all, which is how a
//! reader tells "does not apply" from "failed". A failed read is logged on the
//! transition into failure (WARN) and back (INFO), not once per tick.

mod parse;
mod render;
mod spawn_probe;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

pub(crate) use parse::PoolTag;
use parse::{instrument, GaugeReading};
pub(crate) use render::{health_json, sample_fields, OsHealthSampleFields};
use spawn_probe::{ProbeResult, ProbeRun, ProbeWindow, SpawnProbe};
pub(crate) use spawn_probe::{SpawnProbeStats, CENSORED_MS};

/// The collector thread's name. `/health` `threadCensus` shows it, and a test
/// pins that the probe body runs on it.
pub(crate) const THREAD_NAME: &str = "os-health";

/// Gauge cadence (plan D1): the same 30 s as the resource sampler, so every
/// published sample carries a reading no older than one period.
pub(crate) const GAUGE_PERIOD: Duration = Duration::from_secs(30);
/// The spawn probe runs every 2nd gauge tick: once a minute (plan D3).
pub(crate) const PROBE_EVERY_TICKS: u64 = 2;
/// Pool tags are re-read every 5th gauge tick: about 2.5 minutes (plan D2).
pub(crate) const POOL_TAGS_EVERY_TICKS: u64 = 5;

/// Which collector family this build runs, and therefore which readings exist
/// at all on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OsPlatform {
    Windows,
    Linux,
    /// Any other OS (macOS): no collector and no probe. Every reading is NOT
    /// APPLICABLE, never UNKNOWN.
    Unsupported,
}

impl OsPlatform {
    /// The platform this binary was built for.
    pub(crate) const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Unsupported
        }
    }

    /// The `kernel_source` prefix.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Unsupported => "unsupported",
        }
    }

    /// Paged / nonpaged / handles / process count / boot instant.
    pub(crate) const fn has_kernel_gauges(self) -> bool {
        matches!(self, Self::Windows | Self::Linux)
    }

    /// Token and Process object counts — the NT object manager. Windows only.
    pub(crate) const fn has_kernel_objects(self) -> bool {
        matches!(self, Self::Windows)
    }

    /// The pool-tag table. Windows only.
    pub(crate) const fn has_pool_tags(self) -> bool {
        matches!(self, Self::Windows)
    }

    /// The D3 spawn probe.
    pub(crate) const fn has_spawn_probe(self) -> bool {
        matches!(self, Self::Windows | Self::Linux)
    }

    /// The `/health` reading names (camelCase) this platform has NO instrument
    /// for. A `null` among them is NOT APPLICABLE; a `null` elsewhere is
    /// UNKNOWN.
    pub(crate) fn not_applicable(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.has_kernel_gauges() {
            out.extend([
                "kernelPagedBytes",
                "kernelNonpagedBytes",
                "handleCount",
                "processCount",
                "bootedAt",
            ]);
        }
        if !self.has_kernel_objects() {
            out.extend(["tokenObjects", "processObjects"]);
        }
        if !self.has_pool_tags() {
            out.extend(["poolTopTags", "poolTagsAt"]);
        }
        if !self.has_spawn_probe() {
            out.push("spawnProbe");
        }
        out
    }
}

/// One reading of the machine's kernel and process-creation health.
///
/// Every reading is `Option` and `None` is never a stand-in for 0 — see the
/// module docs (D6). `platform` is what tells a reader whether a `None` is
/// UNKNOWN (an applicable read failed or has not happened) or NOT APPLICABLE.
///
/// Integers are `i64` here, the width of coord's `bigint` columns; the `integer`
/// columns are narrowed (clamped) only on the way into a resource sample.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OsHealthSnapshot {
    pub(crate) platform: OsPlatform,
    pub(crate) kernel_paged_bytes: Option<i64>,
    pub(crate) kernel_nonpaged_bytes: Option<i64>,
    pub(crate) kernel_handle_count: Option<i64>,
    /// Windows `Token` `TotalNumberOfObjects`. NOT APPLICABLE on Linux.
    pub(crate) kernel_token_objects: Option<i64>,
    /// Windows `Process` `TotalNumberOfObjects` (live + zombie). NOT
    /// APPLICABLE on Linux. Minus `os_process_count` it is the zombie count.
    pub(crate) kernel_process_objects: Option<i64>,
    pub(crate) os_process_count: Option<i64>,
    /// Stable for the life of the boot (see `parse::stable_boot_time`), so a
    /// consumer can group samples by it. Whole seconds.
    pub(crate) os_booted_at: Option<DateTime<Utc>>,
    /// The top 8 pool tags by bytes, as of `kernel_pool_tags_at`. Re-read
    /// every 5th tick and carried on every snapshot in between; `None` means
    /// never read or the last re-read failed, never "carried from before a
    /// failure". NOT APPLICABLE on Linux.
    pub(crate) kernel_pool_top_tags: Option<Vec<PoolTag>>,
    pub(crate) kernel_pool_tags_at: Option<DateTime<Utc>>,
    /// The spawn probe over its rolling window; `None` before the first probe.
    pub(crate) spawn_probe: Option<SpawnProbeStats>,
    /// The instruments that answered, prefixed by platform; `None` when none
    /// did.
    pub(crate) kernel_source: Option<String>,
    /// When this snapshot's gauges were read.
    pub(crate) read_at: DateTime<Utc>,
}

impl OsHealthSnapshot {
    /// Paged + nonpaged kernel pool — S8's quantity. `None` unless both
    /// halves are known: half a pool is not a pool.
    pub(crate) fn kernel_pool_bytes(&self) -> Option<i64> {
        Some(
            self.kernel_paged_bytes?
                .saturating_add(self.kernel_nonpaged_bytes?),
        )
    }
}

/// The process-global snapshot cell. A writer holds the lock only to swap an
/// `Arc`; a reader only to clone one. Nobody ever reads the machine while
/// holding it, so no reader can be made to wait on a kernel query.
pub(crate) struct SnapshotCell {
    inner: Mutex<Option<Arc<OsHealthSnapshot>>>,
}

impl SnapshotCell {
    pub(crate) const fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// The latest snapshot, or `None` before the first tick.
    pub(crate) fn load(&self) -> Option<Arc<OsHealthSnapshot>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn store(&self, snap: OsHealthSnapshot) {
        self.set(Some(Arc::new(snap)));
    }

    /// Replace the cell's content, returning the previous one.
    fn set(&self, snap: Option<Arc<OsHealthSnapshot>>) -> Option<Arc<OsHealthSnapshot>> {
        std::mem::replace(
            &mut *self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            snap,
        )
    }
}

static CELL: SnapshotCell = SnapshotCell::new();

/// How old a snapshot may be and still be served as CURRENT. A tick is 30 s
/// and a probe tick can hold the thread for the 30 s probe bound on top, so
/// the gap between two stores is at most about a minute; 120 s is two missed
/// ticks — a stopped, panicking or wedged collector, not a slow one. Past it,
/// the resource sample drops the kernel readings (a stale reading must never
/// be republished as a fresh one) and `/health` `osHealth` says `stale`.
pub(crate) const STALE_AFTER_SECS: i64 = 120;

/// Whether `snap`, read at its `read_at`, is too old to serve as current at
/// `now`.
pub(crate) fn is_stale(snap: &OsHealthSnapshot, now: DateTime<Utc>) -> bool {
    (now - snap.read_at).num_seconds() > STALE_AFTER_SECS
}

/// Serialises the tests that write the process-global cell (and, in Phase 5,
/// the controls and slope history), so they cannot read one another's
/// injections.
///
/// A CHILD of `env_lock` in the test-lock hierarchy, as
/// `env_test_lock_hierarchy_guard` requires of every test lock:
/// `hierarchy_lock` takes the (per-thread reentrant) env lock first, so a
/// holder — the host-lane arming test holds it across `collect_host_lane()` —
/// can take the env lock again in either order without an AB/BA deadlock.
#[cfg(test)]
pub(crate) fn test_cell_lock() -> crate::test_env::TestLockGuard {
    static LOCK: Mutex<()> = Mutex::new(());
    crate::test_env::hierarchy_lock(&LOCK)
}

/// The test seam into the PROCESS-GLOBAL cell — the one `/health` and the
/// resource sample's host lane read. Puts `snap` there (`None` empties it)
/// until the returned guard drops, which restores what was there before
/// ("clear for tests"). Holds [`test_cell_lock`] for its lifetime. Compiled
/// for tests only; nothing in a shipped build can write the cell but the
/// `os-health` thread.
#[cfg(test)]
pub(crate) fn store_for_tests(snap: Option<OsHealthSnapshot>) -> StoredForTests {
    let lock = test_cell_lock();
    let previous = CELL.set(snap.map(Arc::new));
    StoredForTests {
        previous,
        _lock: lock,
    }
}

/// Restores the cell on drop. See [`store_for_tests`].
#[cfg(test)]
pub(crate) struct StoredForTests {
    previous: Option<Arc<OsHealthSnapshot>>,
    _lock: crate::test_env::TestLockGuard,
}

#[cfg(test)]
impl Drop for StoredForTests {
    fn drop(&mut self) {
        CELL.set(self.previous.take());
    }
}

/// The latest snapshot the `os-health` thread stored, or `None` before its
/// first tick (UNKNOWN — never healthy). Never blocks on a fresh read.
pub(crate) fn current() -> Option<Arc<OsHealthSnapshot>> {
    CELL.load()
}

/// One OS's gauge reads. The production sources are `windows::WindowsCollector`
/// and `linux::LinuxCollector`; tests inject their own.
pub(crate) trait GaugeSource {
    /// This tick's gauges.
    fn gauges(&mut self, log: &mut FailureLog) -> GaugeReading;
    /// The top pool tags, or `None` (failed, or NOT APPLICABLE).
    fn pool_tags(&mut self, _log: &mut FailureLog) -> Option<Vec<PoolTag>> {
        None
    }
}

/// The source for an OS with no collector.
struct NoCollector;

impl GaugeSource for NoCollector {
    fn gauges(&mut self, _log: &mut FailureLog) -> GaugeReading {
        GaugeReading::default()
    }
}

#[cfg(windows)]
fn production_source() -> Box<dyn GaugeSource + Send> {
    Box::new(windows::WindowsCollector::default())
}

#[cfg(target_os = "linux")]
fn production_source() -> Box<dyn GaugeSource + Send> {
    Box::new(linux::LinuxCollector)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn production_source() -> Box<dyn GaugeSource + Send> {
    Box::new(NoCollector)
}

/// Rate-limits instrument failures to their TRANSITIONS: WARN when an
/// instrument starts failing, INFO when it answers again, DEBUG while it keeps
/// failing. A 30 s loop against a broken instrument would otherwise bury the
/// log in 120 identical WARNs an hour (D6's `warn_once`-style limiter).
///
/// **Bounded for a FLAPPING instrument too.** Transitions alone do not bound
/// an instrument that fails every other tick — that is a WARN a minute,
/// forever. So each instrument gets [`FLAP_WARN_BUDGET`] failure WARNs for the
/// life of the process; the last of them says so, and every transition after
/// it (failing and recovering alike) is DEBUG.
#[derive(Debug, Default)]
pub(crate) struct FailureLog {
    failing: BTreeMap<&'static str, bool>,
    warned: BTreeMap<&'static str, u32>,
}

/// Failure WARNs one instrument may emit before its transitions go to DEBUG.
pub(crate) const FLAP_WARN_BUDGET: u32 = 3;

/// The level one recorded result is logged at; `None` logs nothing above
/// DEBUG. Split out of [`FailureLog::record`] so the limiter is testable
/// without a tracing subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogLevel {
    /// The instrument started failing, inside its WARN budget.
    Warn,
    /// The instrument started failing and this is the LAST WARN it gets.
    WarnLast,
    /// The instrument answers again, after a failure that was WARNed.
    Info,
    /// Anything else: still failing, or a transition past the budget.
    Debug,
}

impl FailureLog {
    /// Classify one result for `instrument` and update the state.
    pub(crate) fn level_for(&mut self, instrument: &'static str, ok: bool) -> LogLevel {
        let was_failing = self.failing.insert(instrument, !ok) == Some(true);
        let warned = self.warned.get(instrument).copied().unwrap_or(0);
        match (ok, was_failing) {
            // A recovery from a failure that was WARNed (every failure
            // transition inside the budget is), unless that WARN was the last.
            (true, true) if warned < FLAP_WARN_BUDGET => LogLevel::Info,
            (false, false) if warned < FLAP_WARN_BUDGET => {
                self.warned.insert(instrument, warned + 1);
                if warned + 1 == FLAP_WARN_BUDGET {
                    LogLevel::WarnLast
                } else {
                    LogLevel::Warn
                }
            }
            _ => LogLevel::Debug,
        }
    }

    /// Pass an instrument's result through, logging a state change.
    pub(crate) fn record<T>(
        &mut self,
        instrument: &'static str,
        r: Result<T, String>,
    ) -> Option<T> {
        let level = self.level_for(instrument, r.is_ok());
        match r {
            Ok(v) => {
                match level {
                    LogLevel::Info => info!(instrument, "os-health: {instrument} answers again"),
                    _ => debug!(instrument, "os-health: {instrument} answers"),
                }
                Some(v)
            }
            Err(reason) => {
                match level {
                    LogLevel::Warn => warn!(
                        instrument,
                        "os-health: {instrument} failed: {reason} — its readings are \
                         published as UNKNOWN (NULL), never 0, until it answers again"
                    ),
                    LogLevel::WarnLast => warn!(
                        instrument,
                        "os-health: {instrument} failed: {reason} — its readings are \
                         published as UNKNOWN (NULL), never 0, until it answers again. \
                         This is its {FLAP_WARN_BUDGET}th failure WARN: it is flapping, and \
                         its further transitions are logged at DEBUG"
                    ),
                    _ => debug!(instrument, "os-health: {instrument} failing: {reason}"),
                }
                None
            }
        }
    }
}

/// The cadences, the sources and the probe a collector runs with.
pub(crate) struct CollectorConfig {
    pub(crate) platform: OsPlatform,
    pub(crate) period: Duration,
    pub(crate) probe_every_ticks: u64,
    pub(crate) pool_tags_every_ticks: u64,
    pub(crate) source: Box<dyn GaugeSource + Send>,
    pub(crate) probe: Option<SpawnProbe>,
}

impl CollectorConfig {
    /// The production collector for this OS.
    pub(crate) fn production() -> Self {
        let platform = OsPlatform::current();
        Self {
            platform,
            period: GAUGE_PERIOD,
            probe_every_ticks: PROBE_EVERY_TICKS,
            pool_tags_every_ticks: POOL_TAGS_EVERY_TICKS,
            source: production_source(),
            probe: platform.has_spawn_probe().then(SpawnProbe::production),
        }
    }
}

/// The cached pool-tag reading and when it was taken.
#[derive(Debug, Clone, Default)]
struct PoolTagsState {
    tags: Option<Vec<PoolTag>>,
    at: Option<DateTime<Utc>>,
}

/// The collector's state between ticks.
pub(crate) struct Collector {
    cfg: CollectorConfig,
    log: FailureLog,
    window: ProbeWindow,
    pool: PoolTagsState,
    gauges: GaugeReading,
    read_at: Option<DateTime<Utc>>,
    tick: u64,
}

impl Collector {
    pub(crate) fn new(cfg: CollectorConfig) -> Self {
        Self {
            cfg,
            log: FailureLog::default(),
            window: ProbeWindow::default(),
            pool: PoolTagsState::default(),
            gauges: GaugeReading::default(),
            read_at: None,
            tick: 0,
        }
    }

    /// One tick: read the gauges (and, on a pool tick, the pool tags) and
    /// publish; then, on a probe tick, run ONE probe and publish again with its
    /// result. Synchronous on the calling thread — which is the `os-health`
    /// thread in production, so exactly one probe is ever in flight.
    pub(crate) fn step(&mut self, publish: &dyn Fn(OsHealthSnapshot)) {
        self.gauges = self.cfg.source.gauges(&mut self.log);
        let read_at = Utc::now();
        self.read_at = Some(read_at);
        if self
            .tick
            .is_multiple_of(self.cfg.pool_tags_every_ticks.max(1))
        {
            let tags = self.cfg.source.pool_tags(&mut self.log);
            // A failed re-read publishes NULL — not the last good table — so
            // NULL means only "never read" or "read failed" (plan D2, vet R8).
            self.pool = PoolTagsState {
                at: tags.as_ref().map(|_| read_at),
                tags,
            };
        }
        publish(self.snapshot());

        if self.tick.is_multiple_of(self.cfg.probe_every_ticks.max(1)) {
            if let Some(probe) = &self.cfg.probe {
                let run = probe.run_once();
                log_probe(&mut self.log, &run);
                // An `Unavailable` result empties the window (instrument
                // failure — see `ProbeWindow::push`).
                self.window.push(run.result);
                publish(self.snapshot());
            }
        }
        self.tick = self.tick.wrapping_add(1);
    }

    /// The snapshot of the current state.
    fn snapshot(&self) -> OsHealthSnapshot {
        let g = &self.gauges;
        let mut instruments = g.instruments.clone();
        if self.pool.tags.is_some() {
            instruments.push(instrument::POOL_TAGS);
        }
        let spawn_probe = self.window.stats();
        // The probe ANSWERED when its window holds a result — a timeout or a
        // create error is an answer about the OS. A missing probe program
        // empties the window, so it is not named.
        if spawn_probe.is_some() {
            instruments.push(instrument::SPAWN_PROBE);
        }
        OsHealthSnapshot {
            platform: self.cfg.platform,
            kernel_paged_bytes: g.kernel_paged_bytes,
            kernel_nonpaged_bytes: g.kernel_nonpaged_bytes,
            kernel_handle_count: g.kernel_handle_count,
            kernel_token_objects: g.kernel_token_objects,
            kernel_process_objects: g.kernel_process_objects,
            os_process_count: g.os_process_count,
            os_booted_at: g.os_booted_at,
            kernel_pool_top_tags: self.pool.tags.clone(),
            kernel_pool_tags_at: self.pool.at,
            spawn_probe,
            kernel_source: kernel_source(self.cfg.platform, &instruments),
            read_at: self.read_at.unwrap_or_else(Utc::now),
        }
    }
}

/// `"<platform>:<a>+<b>+…"`, or `None` when nothing answered.
fn kernel_source(platform: OsPlatform, instruments: &[&str]) -> Option<String> {
    (!instruments.is_empty() && platform != OsPlatform::Unsupported)
        .then(|| format!("{}:{}", platform.as_str(), instruments.join("+")))
}

/// A probe that did not complete is a measurement, but one an operator should
/// hear about — on the transition, like any other instrument.
fn log_probe(log: &mut FailureLog, run: &ProbeRun) {
    let outcome = match run.result {
        ProbeResult::Completed { .. } => Ok(()),
        ProbeResult::TimedOut => Err(format!(
            "the trivial child overran {:?} and its tree was killed (pid {:?}, reaped {:?}) — \
             recorded as a censored {CENSORED_MS} ms sample",
            run.elapsed, run.killed_pid, run.reaped
        )),
        ProbeResult::SpawnError { code } => Err(format!(
            "the trivial child could not be created (os error {code:?}) — counted in errors, \
             not as a latency sample"
        )),
        ProbeResult::ExitedNonZero { code } => Err(format!(
            "the trivial child was created but exited failing (exit status {code:?}, \
             0xC0000142 is STATUS_DLL_INIT_FAILED) — counted in errors, not as a latency sample"
        )),
        ProbeResult::Unavailable => Err(
            "the probe program was not found — an INSTRUMENT failure, not a measurement of \
             the OS: the probe window is emptied and spawn latency reads UNKNOWN"
                .to_string(),
        ),
    };
    log.record(instrument::SPAWN_PROBE, outcome);
}

/// Run `collector` on a new `os-health` thread until `stop` is set, ticking
/// every `cfg.period`. Production never sets `stop`; tests do.
pub(crate) fn spawn_collector_thread(
    collector: Collector,
    publish: impl Fn(OsHealthSnapshot) + Send + 'static,
    stop: Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(THREAD_NAME.to_string())
        .spawn(move || run_loop(collector, &publish, &stop))
}

fn run_loop(mut collector: Collector, publish: &dyn Fn(OsHealthSnapshot), stop: &AtomicBool) {
    let period = collector.cfg.period;
    let mut panics: u32 = 0;
    while !stop.load(Ordering::Relaxed) {
        let tick_started = Instant::now();
        // A panic in one tick (a parser bug on a build we have never seen, a
        // poisoned invariant) must not end the thread: the cell would then
        // hold its last snapshot forever. Catch it, say so, and tick again —
        // the stale gate (`STALE_AFTER_SECS`) covers the gap meanwhile.
        let stepped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            collector.step(publish);
        }));
        if let Err(payload) = stepped {
            panics = panics.saturating_add(1);
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_string());
            if panics <= FLAP_WARN_BUDGET {
                warn!("os-health: a collector tick panicked ({what}); the thread carries on (panic #{panics})");
            } else {
                debug!("os-health: a collector tick panicked ({what}) (panic #{panics})");
            }
        }
        // A tick that overran (a 30 s probe timeout) starts the next at once.
        let next = tick_started + period;
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let now = Instant::now();
            if now >= next {
                break;
            }
            std::thread::park_timeout(next - now);
        }
    }
}

/// Start the `os-health` thread. Called once from `run_app` on EVERY instance
/// — primary, secondary/temp, with or without a coord credential — and never
/// gated on `fleet::machine_state_publish_allowed`. A second call is a no-op.
pub(crate) fn start() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let platform = OsPlatform::current();
    if platform == OsPlatform::Unsupported {
        info!(
            "os-health: no kernel-gauge collector or spawn probe for this OS; every \
             osHealth reading is NOT APPLICABLE"
        );
    }
    let collector = Collector::new(CollectorConfig::production());
    match spawn_collector_thread(
        collector,
        |snap| CELL.store(snap),
        Arc::new(AtomicBool::new(false)),
    ) {
        Ok(_) => info!(
            "os-health: collector thread started (gauges every {}s, spawn probe every {}s, \
             pool tags every {}s)",
            GAUGE_PERIOD.as_secs(),
            GAUGE_PERIOD.as_secs() * PROBE_EVERY_TICKS,
            GAUGE_PERIOD.as_secs() * POOL_TAGS_EVERY_TICKS,
        ),
        Err(e) => warn!(
            "os-health: could not start the collector thread ({e}); /health osHealth stays \
             UNKNOWN and no resource sample carries kernel readings — the runner still boots"
        ),
    }
}

/// The incident snapshot — degraded spaceship's readings from the captured
/// buffers (2026-10-19T08:20:52Z) and the diagnostic's spawn latency. For
/// tests here and Phase 5's verdict tests.
#[cfg(test)]
pub(crate) fn incident_snapshot_for_tests() -> OsHealthSnapshot {
    let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
    OsHealthSnapshot {
        platform: OsPlatform::Windows,
        kernel_paged_bytes: Some(24_282_075_136),
        kernel_nonpaged_bytes: Some(15_075_627_008),
        kernel_handle_count: Some(406_585),
        kernel_token_objects: Some(4_365_962),
        kernel_process_objects: Some(2_930),
        os_process_count: Some(1_442),
        os_booted_at: Some(at("2026-10-15T06:12:00Z")),
        kernel_pool_top_tags: Some(vec![
            PoolTag {
                tag: "File".into(),
                bytes: 9_693_669_888,
                outstanding_allocs: 24_238_128,
            },
            PoolTag {
                tag: "Toke".into(),
                bytes: 9_499_968_256,
                outstanding_allocs: 4_365_962,
            },
            PoolTag {
                tag: "D2d ".into(),
                bytes: 759_218_944,
                outstanding_allocs: 23_725_583,
            },
        ]),
        kernel_pool_tags_at: Some(at("2026-10-19T08:20:52Z")),
        spawn_probe: Some(SpawnProbeStats {
            p50_ms: Some(2_400),
            max_ms: Some(30_000),
            count: 10,
            timeouts: 1,
            errors: 1,
            error_code: Some(1450),
        }),
        kernel_source: Some("windows:getperformanceinfo+objecttypes+pooltags+spawn-probe".into()),
        read_at: at("2026-10-19T08:20:52Z"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A gauge source whose readings, and pool-tag answers, are scripted.
    struct Scripted {
        gauges: GaugeReading,
        pool: Vec<Option<Vec<PoolTag>>>,
        pool_calls: Arc<Mutex<u32>>,
        threads: Arc<Mutex<Vec<String>>>,
    }

    impl GaugeSource for Scripted {
        fn gauges(&mut self, _log: &mut FailureLog) -> GaugeReading {
            self.threads.lock().unwrap().push(
                std::thread::current()
                    .name()
                    .unwrap_or("<unnamed>")
                    .to_string(),
            );
            self.gauges.clone()
        }
        fn pool_tags(&mut self, _log: &mut FailureLog) -> Option<Vec<PoolTag>> {
            let mut n = self.pool_calls.lock().unwrap();
            let i = *n as usize;
            *n += 1;
            self.pool.get(i).cloned().flatten()
        }
    }

    fn windows_like_gauges() -> GaugeReading {
        GaugeReading {
            kernel_paged_bytes: Some(1_750_000_000),
            kernel_nonpaged_bytes: Some(1_200_000_000),
            kernel_handle_count: Some(120_000),
            kernel_token_objects: Some(5_580),
            kernel_process_objects: Some(400),
            os_process_count: Some(380),
            os_booted_at: DateTime::from_timestamp(1_760_000_000, 0),
            instruments: vec!["getperformanceinfo", "objecttypes"],
        }
    }

    fn tag(name: &str, bytes: i64) -> PoolTag {
        PoolTag {
            tag: name.into(),
            bytes,
            outstanding_allocs: 1,
        }
    }

    fn scripted_config(
        platform: OsPlatform,
        gauges: GaugeReading,
        pool: Vec<Option<Vec<PoolTag>>>,
        probe: Option<SpawnProbe>,
    ) -> (CollectorConfig, Arc<Mutex<u32>>, Arc<Mutex<Vec<String>>>) {
        let pool_calls = Arc::new(Mutex::new(0));
        let threads = Arc::new(Mutex::new(Vec::new()));
        let cfg = CollectorConfig {
            platform,
            period: Duration::from_millis(10),
            probe_every_ticks: PROBE_EVERY_TICKS,
            pool_tags_every_ticks: POOL_TAGS_EVERY_TICKS,
            source: Box::new(Scripted {
                gauges,
                pool,
                pool_calls: Arc::clone(&pool_calls),
                threads: Arc::clone(&threads),
            }),
            probe,
        };
        (cfg, pool_calls, threads)
    }

    /// Wait (bounded) until the cell holds a snapshot satisfying `pred`.
    fn wait_for(
        cell: &SnapshotCell,
        within: Duration,
        pred: impl Fn(&OsHealthSnapshot) -> bool,
    ) -> Option<Arc<OsHealthSnapshot>> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if let Some(s) = cell.load() {
                if pred(&s) {
                    return Some(s);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    fn stop_and_join(stop: &AtomicBool, handle: std::thread::JoinHandle<()>) {
        stop.store(true, Ordering::Relaxed);
        handle.thread().unpark();
        handle.join().expect("the os-health thread must not panic");
    }

    #[test]
    fn the_production_cadences_are_the_plans() {
        assert_eq!(GAUGE_PERIOD, Duration::from_secs(30));
        assert_eq!(
            GAUGE_PERIOD * PROBE_EVERY_TICKS as u32,
            Duration::from_secs(60)
        );
        assert_eq!(
            GAUGE_PERIOD * POOL_TAGS_EVERY_TICKS as u32,
            Duration::from_secs(150)
        );
        let cfg = CollectorConfig::production();
        assert_eq!(cfg.platform, OsPlatform::current());
        assert_eq!(cfg.probe.is_some(), cfg.platform.has_spawn_probe());
    }

    #[test]
    fn the_probe_body_and_the_gauge_reads_run_on_the_os_health_thread() {
        let probe_threads = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&probe_threads);
        // The PRODUCTION command, observed from inside the probe body: the
        // factory runs in `run_once`, on whatever thread runs the probe.
        let probe = SpawnProbe::with(
            move || {
                seen.lock().unwrap().push(
                    std::thread::current()
                        .name()
                        .unwrap_or("<unnamed>")
                        .to_string(),
                );
                spawn_probe::probe_command()
            },
            spawn_probe::PROBE_TIMEOUT,
        );
        let (cfg, _, gauge_threads) = scripted_config(
            OsPlatform::current(),
            windows_like_gauges(),
            vec![],
            Some(probe),
        );
        let cell = Arc::new(SnapshotCell::new());
        let sink = Arc::clone(&cell);
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn_collector_thread(
            Collector::new(cfg),
            move |s| sink.store(s),
            Arc::clone(&stop),
        )
        .unwrap();
        let got = wait_for(&cell, Duration::from_secs(60), |s| {
            s.spawn_probe.is_some_and(|p| p.count >= 1)
        });
        stop_and_join(&stop, handle);
        assert!(got.is_some(), "no probe result arrived");
        let probes = probe_threads.lock().unwrap().clone();
        assert!(!probes.is_empty());
        assert!(probes.iter().all(|n| n == THREAD_NAME), "{probes:?}");
        let gauges = gauge_threads.lock().unwrap().clone();
        assert!(gauges.iter().all(|n| n == THREAD_NAME), "{gauges:?}");
    }

    /// The collector must not need ANY tokio runtime. Two workers, both
    /// parked on a blocking wait, starve a test-local runtime completely —
    /// the shape `mcp-api-rt` was in during the incident — and the real
    /// production probe still delivers.
    ///
    /// What this does NOT prove: it starves a TEST-LOCAL runtime, not
    /// `mcp-api-rt` itself (that runtime exists only inside
    /// `serve_on_dedicated_runtime`, so a unit test cannot block it), and it
    /// says nothing about OS-level starvation — a box whose process creation
    /// takes seconds slows this thread's probe exactly as much as any other
    /// thread's, which is the measurement. It proves the collector's path
    /// contains no tokio dependency that a starved runtime could stall.
    #[test]
    fn probe_results_arrive_while_every_tokio_worker_is_blocked() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (blocked_tx, blocked_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        for _ in 0..2 {
            let blocked_tx = blocked_tx.clone();
            let release_rx = Arc::clone(&release_rx);
            rt.spawn(async move {
                blocked_tx.send(()).unwrap();
                // Deliberately blocking a worker: this IS the starvation.
                let _ = release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(120));
            });
        }
        // Only one task can hold the receiver lock; the other blocks on the
        // lock itself — both workers are parked either way. Wait until both
        // tasks have started, i.e. both workers are taken.
        for _ in 0..2 {
            blocked_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("both workers must pick up a blocking task");
        }
        // Proof the runtime is starved: a canary task cannot run.
        let (canary_tx, canary_rx) = mpsc::channel::<()>();
        rt.spawn(async move {
            let _ = canary_tx.send(());
        });
        assert!(
            canary_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the test runtime is not starved, so this test would prove nothing"
        );

        let (cfg, _, _) = scripted_config(
            OsPlatform::current(),
            windows_like_gauges(),
            vec![],
            Some(SpawnProbe::production()),
        );
        let cell = Arc::new(SnapshotCell::new());
        let sink = Arc::clone(&cell);
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn_collector_thread(
            Collector::new(cfg),
            move |s| sink.store(s),
            Arc::clone(&stop),
        )
        .unwrap();
        let got = wait_for(&cell, Duration::from_secs(60), |s| {
            s.spawn_probe.is_some_and(|p| p.count >= 2)
        });
        stop_and_join(&stop, handle);
        // Release the workers before asserting, so a failure does not leave
        // them parked.
        drop(release_tx);
        rt.shutdown_background();

        let snap = got.expect("probe results must arrive while the runtime is starved");
        let p = snap.spawn_probe.unwrap();
        assert_eq!((p.timeouts, p.errors), (0, 0), "{p:?}");
        assert!(p.p50_ms.is_some_and(|ms| ms < CENSORED_MS), "{p:?}");
    }

    #[test]
    fn a_probe_timeout_does_not_wedge_the_thread() {
        let probe = SpawnProbe::with(spawn_probe::sleeper, Duration::from_millis(200));
        let (mut cfg, _, _) = scripted_config(
            OsPlatform::current(),
            windows_like_gauges(),
            vec![],
            Some(probe),
        );
        cfg.probe_every_ticks = 1;
        let cell = Arc::new(SnapshotCell::new());
        let sink = Arc::clone(&cell);
        let stop = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let handle = spawn_collector_thread(
            Collector::new(cfg),
            move |s| sink.store(s),
            Arc::clone(&stop),
        )
        .unwrap();
        // Three consecutive timeouts: the thread got back after each.
        let got = wait_for(&cell, Duration::from_secs(60), |s| {
            s.spawn_probe.is_some_and(|p| p.timeouts >= 3)
        });
        stop_and_join(&stop, handle);
        let snap = got.expect("the thread wedged on a timed-out probe");
        let p = snap.spawn_probe.unwrap();
        assert_eq!(p.p50_ms, Some(30_000));
        assert_eq!(p.max_ms, Some(30_000));
        assert_eq!(p.successes(), 0);
        // Three 60 s children were each killed at their 200 ms bound.
        assert!(
            started.elapsed() < Duration::from_secs(45),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn pool_tags_are_read_every_fifth_tick_and_carried_with_their_read_time() {
        let first = vec![tag("Toke", 9_000), tag("File", 8_000)];
        let (cfg, pool_calls, _) = scripted_config(
            OsPlatform::Windows,
            windows_like_gauges(),
            vec![Some(first.clone()), None, Some(vec![tag("File", 1)])],
            None,
        );
        let mut c = Collector::new(cfg);
        let published: Arc<Mutex<Vec<OsHealthSnapshot>>> = Arc::default();
        let sink = Arc::clone(&published);
        let publish = move |s: OsHealthSnapshot| sink.lock().unwrap().push(s);

        c.step(&publish); // tick 0: pool read #1 answers
        let t0 = published.lock().unwrap().last().unwrap().clone();
        assert_eq!(t0.kernel_pool_top_tags, Some(first.clone()));
        assert_eq!(t0.kernel_pool_tags_at, Some(t0.read_at));
        assert_eq!(
            t0.kernel_source.as_deref(),
            Some("windows:getperformanceinfo+objecttypes+pooltags")
        );

        for _ in 1..5 {
            c.step(&publish); // ticks 1..=4: no read, the cached table rides along
        }
        assert_eq!(*pool_calls.lock().unwrap(), 1);
        let t4 = published.lock().unwrap().last().unwrap().clone();
        assert_eq!(t4.kernel_pool_top_tags, Some(first));
        assert_eq!(t4.kernel_pool_tags_at, t0.kernel_pool_tags_at);
        assert_ne!(t4.read_at, t0.read_at);

        c.step(&publish); // tick 5: pool read #2 FAILS
        assert_eq!(*pool_calls.lock().unwrap(), 2);
        let t5 = published.lock().unwrap().last().unwrap().clone();
        assert_eq!(
            t5.kernel_pool_top_tags, None,
            "a failed re-read is NULL, not the old table"
        );
        assert_eq!(t5.kernel_pool_tags_at, None);
        assert_eq!(
            t5.kernel_source.as_deref(),
            Some("windows:getperformanceinfo+objecttypes")
        );

        for _ in 6..=10 {
            c.step(&publish);
        }
        assert_eq!(*pool_calls.lock().unwrap(), 3, "ticks 0, 5 and 10");
        assert_eq!(
            published
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .kernel_pool_top_tags,
            Some(vec![tag("File", 1)])
        );
    }

    #[test]
    fn a_failed_read_publishes_none_and_no_instrument() {
        let (cfg, _, _) =
            scripted_config(OsPlatform::Windows, GaugeReading::default(), vec![], None);
        let mut c = Collector::new(cfg);
        let published: Arc<Mutex<Vec<OsHealthSnapshot>>> = Arc::default();
        let sink = Arc::clone(&published);
        c.step(&move |s| sink.lock().unwrap().push(s));
        let s = published.lock().unwrap().last().unwrap().clone();
        assert_eq!(s.kernel_paged_bytes, None);
        assert_eq!(s.kernel_nonpaged_bytes, None);
        assert_eq!(s.kernel_handle_count, None);
        assert_eq!(s.kernel_token_objects, None);
        assert_eq!(s.kernel_process_objects, None);
        assert_eq!(s.os_process_count, None);
        assert_eq!(s.os_booted_at, None);
        assert_eq!(s.kernel_pool_top_tags, None);
        assert_eq!(s.kernel_pool_bytes(), None);
        assert_eq!(s.spawn_probe, None);
        assert_eq!(
            s.kernel_source, None,
            "nothing answered, so nothing is named"
        );
        // UNKNOWN on Windows, where every one of these applies.
        assert!(s.platform.not_applicable().is_empty());
    }

    #[test]
    fn linux_publishes_not_applicable_axes_as_none_and_says_so() {
        let gauges = parse::linux_gauges(
            Some("SReclaimable: 600000 kB\nSUnreclaim: 300000 kB\nKernelStack: 20000 kB\nPageTables: 40000 kB\n"),
            Some("4128 0 9223372036854775807"),
            Some("btime 1760000000\n"),
            Some(312),
        );
        // A Linux source never answers pool tags (the trait default).
        let (cfg, _, _) = scripted_config(OsPlatform::Linux, gauges, vec![], None);
        let mut c = Collector::new(cfg);
        let published: Arc<Mutex<Vec<OsHealthSnapshot>>> = Arc::default();
        let sink = Arc::clone(&published);
        c.step(&move |s| sink.lock().unwrap().push(s));
        let s = published.lock().unwrap().last().unwrap().clone();
        assert_eq!(s.kernel_token_objects, None);
        assert_eq!(s.kernel_process_objects, None);
        assert_eq!(s.kernel_pool_top_tags, None);
        assert_eq!(
            s.kernel_source.as_deref(),
            Some("linux:proc-meminfo+file-nr+proc-stat+proc-pids"),
            "the source names what answered and nothing that does not exist here"
        );
        assert_eq!(s.kernel_pool_bytes(), Some((600_000 + 360_000) * 1024));
        assert_eq!(
            OsPlatform::Linux.not_applicable(),
            vec![
                "tokenObjects",
                "processObjects",
                "poolTopTags",
                "poolTagsAt"
            ]
        );
        assert!(!OsPlatform::Linux.has_kernel_objects());
        assert!(OsPlatform::Linux.has_spawn_probe());
    }

    #[test]
    fn the_probe_runs_every_second_tick_and_errors_are_counted_with_their_code() {
        // A file that is not an executable: a real create failure, not
        // `NotFound`.
        let bad = spawn_probe::not_an_executable();
        let probe = SpawnProbe::with(
            move || crate::process_helpers::no_window(&bad),
            Duration::from_secs(5),
        );
        let (cfg, _, _) = scripted_config(
            OsPlatform::Windows,
            windows_like_gauges(),
            vec![],
            Some(probe),
        );
        let mut c = Collector::new(cfg);
        let published: Arc<Mutex<Vec<OsHealthSnapshot>>> = Arc::default();
        let sink = Arc::clone(&published);
        let publish = move |s: OsHealthSnapshot| sink.lock().unwrap().push(s);
        for _ in 0..6 {
            c.step(&publish);
        }
        let last = published.lock().unwrap().last().unwrap().clone();
        let p = last.spawn_probe.unwrap();
        // Ticks 0, 2 and 4.
        assert_eq!(p.count, 3);
        assert_eq!(p.errors, 3);
        assert_eq!(p.error_code, Some(spawn_probe::NOT_AN_EXE_SPAWN_ERROR));
        assert_eq!(
            (p.p50_ms, p.max_ms),
            (None, None),
            "an error is not a sample"
        );
        assert_eq!(p.successes(), 0);
        // A create error is an ANSWER about the OS: the probe is named.
        assert_eq!(
            last.kernel_source.as_deref(),
            Some("windows:getperformanceinfo+objecttypes+spawn-probe")
        );
    }

    /// `kernel_source` names `spawn-probe` exactly when the probe window holds
    /// a result: not before the first probe, and not once a missing probe
    /// program (an instrument failure) has emptied it.
    #[test]
    fn kernel_source_names_the_spawn_probe_only_while_it_answers() {
        let missing = std::env::temp_dir().join("os-health-probe-missing-77c1.exe");
        let present = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&present);
        let probe = SpawnProbe::with(
            move || {
                if flag.load(Ordering::Relaxed) {
                    spawn_probe::probe_command()
                } else {
                    crate::process_helpers::no_window(&missing)
                }
            },
            Duration::from_secs(20),
        );
        let (mut cfg, _, _) = scripted_config(
            OsPlatform::Windows,
            windows_like_gauges(),
            vec![],
            Some(probe),
        );
        cfg.probe_every_ticks = 1;
        let mut c = Collector::new(cfg);
        let published: Arc<Mutex<Vec<OsHealthSnapshot>>> = Arc::default();
        let sink = Arc::clone(&published);
        let publish = move |s: OsHealthSnapshot| sink.lock().unwrap().push(s);

        c.step(&publish);
        {
            let got = published.lock().unwrap();
            // The gauge publish, before the probe ran.
            assert_eq!(
                got[0].kernel_source.as_deref(),
                Some("windows:getperformanceinfo+objecttypes")
            );
            // The publish after the probe.
            assert_eq!(
                got[1].kernel_source.as_deref(),
                Some("windows:getperformanceinfo+objecttypes+spawn-probe")
            );
            assert!(got[1].spawn_probe.is_some_and(|p| p.successes() == 1));
        }
        present.store(false, Ordering::Relaxed);
        c.step(&publish);
        let last = published.lock().unwrap().last().unwrap().clone();
        assert_eq!(
            last.spawn_probe, None,
            "a missing probe program is UNKNOWN, not a failure"
        );
        assert_eq!(
            last.kernel_source.as_deref(),
            Some("windows:getperformanceinfo+objecttypes")
        );
    }

    /// A tick that panics does not end the `os-health` thread: the next tick
    /// still publishes.
    #[test]
    fn a_panicking_tick_does_not_end_the_thread() {
        struct PanicsFirst {
            calls: u32,
        }
        impl GaugeSource for PanicsFirst {
            fn gauges(&mut self, _log: &mut FailureLog) -> GaugeReading {
                self.calls += 1;
                assert!(self.calls > 1, "os-health test: the first tick panics");
                windows_like_gauges()
            }
        }
        let cfg = CollectorConfig {
            platform: OsPlatform::Windows,
            period: Duration::from_millis(10),
            probe_every_ticks: PROBE_EVERY_TICKS,
            pool_tags_every_ticks: POOL_TAGS_EVERY_TICKS,
            source: Box::new(PanicsFirst { calls: 0 }),
            probe: None,
        };
        let cell = Arc::new(SnapshotCell::new());
        let sink = Arc::clone(&cell);
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn_collector_thread(
            Collector::new(cfg),
            move |s| sink.store(s),
            Arc::clone(&stop),
        )
        .unwrap();
        let got = wait_for(&cell, Duration::from_secs(30), |s| {
            s.kernel_token_objects.is_some()
        });
        stop_and_join(&stop, handle);
        assert!(got.is_some(), "the thread died with its first tick");
    }

    #[test]
    fn staleness_is_two_missed_ticks() {
        assert_eq!(STALE_AFTER_SECS, 120);
        let s = incident_snapshot_for_tests();
        assert!(!is_stale(&s, s.read_at + chrono::Duration::seconds(120)));
        assert!(is_stale(&s, s.read_at + chrono::Duration::seconds(121)));
    }

    #[test]
    fn the_cell_is_unknown_until_stored() {
        let cell = SnapshotCell::new();
        assert!(cell.load().is_none());
        cell.store(incident_snapshot_for_tests());
        assert_eq!(cell.load().unwrap().kernel_token_objects, Some(4_365_962));
        assert_eq!(
            incident_snapshot_for_tests().kernel_pool_bytes(),
            Some(24_282_075_136 + 15_075_627_008)
        );
    }

    #[test]
    fn failures_are_logged_on_transitions_and_pass_values_through() {
        let mut log = FailureLog::default();
        assert_eq!(log.record("x", Ok::<_, String>(5)), Some(5));
        assert_eq!(log.record::<u8>("x", Err("boom".into())), None);
        assert_eq!(log.failing.get("x"), Some(&true));
        assert_eq!(log.record::<u8>("x", Err("boom".into())), None);
        assert_eq!(log.record("x", Ok::<_, String>(6)), Some(6));
        assert_eq!(log.failing.get("x"), Some(&false));

        // The levels: WARN on the transition into failure, DEBUG while it
        // keeps failing, INFO on the recovery.
        let mut log = FailureLog::default();
        assert_eq!(log.level_for("y", true), LogLevel::Debug);
        assert_eq!(log.level_for("y", false), LogLevel::Warn);
        assert_eq!(log.level_for("y", false), LogLevel::Debug);
        assert_eq!(log.level_for("y", true), LogLevel::Info);
        assert_eq!(log.level_for("y", true), LogLevel::Debug);
    }

    /// An instrument that fails every other tick gets FLAP_WARN_BUDGET
    /// WARNs for the life of the process, the last one flagged, and nothing
    /// above DEBUG after it — not a WARN a minute forever.
    #[test]
    fn a_flapping_instrument_is_warned_about_a_bounded_number_of_times() {
        let mut log = FailureLog::default();
        let mut levels = Vec::new();
        for _ in 0..50 {
            levels.push(log.level_for("flappy", false));
            levels.push(log.level_for("flappy", true));
        }
        let warns = levels
            .iter()
            .filter(|l| matches!(l, LogLevel::Warn | LogLevel::WarnLast))
            .count();
        assert_eq!(warns, FLAP_WARN_BUDGET as usize, "{levels:?}");
        assert_eq!(
            levels.iter().filter(|l| **l == LogLevel::WarnLast).count(),
            1,
            "{levels:?}"
        );
        // Recoveries are INFO only for the WARNed failures before the last.
        assert_eq!(
            levels.iter().filter(|l| **l == LogLevel::Info).count(),
            FLAP_WARN_BUDGET as usize - 1,
            "{levels:?}"
        );
        let last_loud = levels.iter().rposition(|l| *l != LogLevel::Debug).unwrap();
        assert!(last_loud < 2 * FLAP_WARN_BUDGET as usize, "{levels:?}");
        // The budget is per instrument.
        assert_eq!(log.level_for("other", false), LogLevel::Warn);
    }
}
