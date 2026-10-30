//! The runner's ONE process-table provider: an in-process read, shared.
//!
//! Plan `2026-10-28-runner-spawn-bursts-overwhelm-the-box-beyond-the-continuation-cap`,
//! Phase 5 (D4). Every process-table read in the runner used to fork
//! `powershell.exe` (plus a hidden `conhost.exe`) and ask WMI: the whole-table
//! snapshot from eight periodic callers on independent timers, the per-pid
//! command-line query, `Get-NetTCPConnection` for a port owner, and the
//! frontend's 30 s external-claude scan. In the two hours before the
//! 2026-10-27 hard restart that was 179 snapshot timeouts and 524 scan
//! timeouts, each one a process creation on a box where creating a process
//! had become the bottleneck. This module replaces the common read with:
//!
//! - **Windows**: one `sysinfo::System::refresh_processes_specifics` walk
//!   (`NtQuerySystemInformation` — no child process, no WMI), the native
//!   enumeration `wedge_diagnostics::collect_children` already chose over a
//!   second hand-rolled Toolhelp walk, plus each row's PRECISE creation time
//!   (a `PROCESS_QUERY_LIMITED_INFORMATION` handle and #2386's
//!   [`qontinui_runner_win32::process_tree::creation_time`], 100 ns FILETIME).
//!   Identity follows #2386's rule
//!   ([`qontinui_runner_win32::process_tree::admits_descendant`]); see
//!   [`build_snapshot`]. Command lines come from `sysinfo`'s `cmd()` for
//!   exactly the pids asked; a port owner from the listener-class read of the
//!   `GetExtendedTcpTable` binding the peer-user guard already carries.
//! - **Unix**: the existing `/proc` walk in [`super::process_tree`] (and
//!   `/proc/<pid>/cmdline` for command lines).
//!
//! **Shared.** A single-flight memo ([`MEMO_TTL`]): concurrent callers join the
//! walk already running instead of each starting one, and a caller within the
//! TTL reuses its result. A caller whose decision needs an observation taken
//! AFTER it asked — absence evidence before a tab close or a reap, a resume's
//! fork guard, the orphan reclaim's kill — uses [`snapshot_fresh`], which never
//! serves the memo and joins only a walk that began after the call. A caller
//! never waits on another caller's walk longer than [`JOIN_WAIT_MAX`]; past it
//! the answer is the last good snapshot flagged `stale`.
//!
//! **The external probe is the fallback, behind one circuit.** On Windows the
//! PowerShell/WMI queries run only when the in-process read fails (Unix has no
//! external snapshot fallback: `/proc` is the read). At most ONE fallback
//! attempt is in flight at a time (a single half-open probe); others skip it.
//! A failed attempt opens the circuit for [`CIRCUIT_BASE`] counted from when
//! the attempt ENDED, doubling per further failure up to [`CIRCUIT_MAX`]; a
//! success closes it and resets the backoff. A snapshot that no path could take
//! is the last good one flagged `stale: true` — or, with none, an empty one,
//! also `stale` — never a fabricated table presented as fresh. A kill or an
//! absence decision must not be made from it ([`ProcessSnapshot::is_unknown`]).
//!
//! **Kill switch (Windows only).** [`ENV_IN_PROCESS`]`=0` disables the
//! in-process read on Windows — the snapshot, the command-line read and the
//! port-owner read — so each takes the circuit-protected PowerShell/WMI path
//! again. The port KILL (`health::kill_port_process`) has no such path: its
//! listener read ([`port_listener_in_process`]) is in-process only, because
//! the kill path creates no process, so with the switch set every port kill
//! refuses (and says so). It is ignored elsewhere: off Windows the in-process
//! read is the only read there is. Read once per process.
//!
//! `/health` serves the counters as `processTable` ([`health_json`]).

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

use super::process_tree::ProcessSnapshot;
pub(crate) use qontinui_runner_lib::peer_user_guard::decode::ListenerOwner;

/// How long one walk's result serves later callers. Two seconds: long enough
/// that a burst of callers costs one walk, short against every periodic
/// caller's own interval. The pollers that look for a process's absence on a
/// short cadence (the graceful-exit probe: 1 s on Windows, 500 ms elsewhere)
/// read [`snapshot_fresh`] instead.
pub(crate) const MEMO_TTL: Duration = Duration::from_secs(2);

/// The fallback is skipped this long after its first failure, counted from
/// when the failed attempt ended.
pub(crate) const CIRCUIT_BASE: Duration = Duration::from_secs(30);

/// The longest the fallback is ever skipped; each further failure doubles the
/// skip up to this.
pub(crate) const CIRCUIT_MAX: Duration = Duration::from_secs(600);

/// Headroom [`JOIN_WAIT_MAX`] keeps over the fallback snapshot's own budget.
const JOIN_WAIT_MARGIN: Duration = Duration::from_secs(4);

/// The longest a caller waits for ANOTHER caller's walk (and that walk's
/// fallback) to settle before answering stale: the WMI snapshot's budget
/// ([`super::process_tree::WMI_SNAPSHOT_TIMEOUT`]) plus [`JOIN_WAIT_MARGIN`],
/// defined from it so the two cannot drift apart. It never fires on a healthy
/// read; it bounds a synchronous caller that reached here from an async
/// context.
pub(crate) const JOIN_WAIT_MAX: Duration =
    super::process_tree::WMI_SNAPSHOT_TIMEOUT.saturating_add(JOIN_WAIT_MARGIN);

/// `0` disables the in-process read on Windows (see the module docs): reads
/// take the circuit-protected external fallback, and port kills refuse (no
/// spawn on the kill path).
pub(crate) const ENV_IN_PROCESS: &str = "QONTINUI_PROCESS_TABLE_IN_PROCESS";

/// The provider's tunables. Production uses [`Config::default`]; tests pass
/// their own.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Config {
    pub(crate) memo_ttl: Duration,
    pub(crate) circuit_base: Duration,
    pub(crate) circuit_max: Duration,
    pub(crate) join_wait_max: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            memo_ttl: MEMO_TTL,
            circuit_base: CIRCUIT_BASE,
            circuit_max: CIRCUIT_MAX,
            join_wait_max: JOIN_WAIT_MAX,
        }
    }
}

/// The provider's notion of "now" — `Instant::now` in production, a
/// controllable clock in tests.
pub(crate) type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The fallback's circuit.
#[derive(Debug)]
struct Circuit {
    /// Until when the fallback is skipped; `None` = closed.
    open_until: Option<Instant>,
    /// The same instant as wall-clock time, for `/health`.
    open_until_utc: Option<chrono::DateTime<chrono::Utc>>,
    /// The skip the NEXT failure opens the circuit for.
    next_backoff: Duration,
    /// A fallback attempt is in flight (the single half-open probe).
    probing: bool,
}

/// The single-flight state.
#[derive(Debug, Default)]
struct State {
    /// `Some(start)` while a walk is running, `start` being the instant its
    /// leader asked.
    in_flight: Option<Instant>,
    /// Bumped each time a walk completes, so a waiter can tell its walk ended.
    generation: u64,
    /// What the last completed walk delivered (fresh, or stale).
    delivered: Option<ProcessSnapshot>,
    /// The last FRESH snapshot and the instant its walk began.
    last_good: Option<(Instant, ProcessSnapshot)>,
}

/// One process-table provider: memo, single flight, fallback circuit and the
/// `/health` counters. The runner holds one ([`provider`]); tests build their
/// own with injected walks, fallbacks and clocks.
pub(crate) struct Provider {
    cfg: Config,
    clock: Clock,
    state: Mutex<State>,
    walked: Condvar,
    circuit: Mutex<Circuit>,
    reads: AtomicU64,
    memo_hits: AtomicU64,
    fallbacks: AtomicU64,
    /// In-process reads that failed in a row (any kind); a success resets it.
    failures_in_a_row: AtomicU64,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider")
            .field("cfg", &self.cfg)
            .field("reads", &self.reads)
            .field("memo_hits", &self.memo_hits)
            .field("fallbacks", &self.fallbacks)
            .finish_non_exhaustive()
    }
}

/// Lock a mutex, recovering it if a panicking holder poisoned it: every
/// critical section here leaves the state consistent before it can panic.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether data observed at `at` may answer a caller that asked at `now` and
/// accepts data up to `max_age` old. With `max_age` zero only an observation
/// that began at or after the call qualifies.
fn serves(at: Instant, now: Instant, max_age: Duration) -> bool {
    at >= now || now.duration_since(at) < max_age
}

/// The last good snapshot flagged stale, or an empty stale one.
fn stale_of(st: &State) -> ProcessSnapshot {
    st.last_good
        .as_ref()
        .map(|(_, good)| ProcessSnapshot {
            stale: true,
            ..good.clone()
        })
        .unwrap_or_default()
}

/// The duty a caller holds when its own walk failed: settle the flight — with
/// the fallback's snapshot, or with none. Dropping it unsettled (a panic, a
/// cancelled future) settles it with none, so no waiter is ever stranded.
pub(crate) struct Flight<'a> {
    provider: &'a Provider,
    started: Instant,
    settled: bool,
}

impl Flight<'_> {
    /// Complete the walk: `fresh` becomes the memo and every waiter's answer;
    /// `None` answers the last good snapshot flagged stale (or an empty stale
    /// one).
    pub(crate) fn settle(mut self, fresh: Option<ProcessSnapshot>) -> ProcessSnapshot {
        self.settled = true;
        self.provider.complete(self.started, fresh)
    }
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.provider.complete(self.started, None);
        }
    }
}

/// The one fallback attempt allowed in flight. End it with [`Probe::end`];
/// dropping it un-ended (a panic, a cancelled future) frees the slot without
/// touching the circuit.
pub(crate) struct Probe<'a> {
    provider: &'a Provider,
    ended: bool,
}

impl Probe<'_> {
    /// Record the attempt's outcome (see [`Provider::end_fallback`]).
    pub(crate) fn end(mut self, ok: bool, what: &str) {
        self.ended = true;
        self.provider.end_fallback(ok, what);
    }
}

impl Drop for Probe<'_> {
    fn drop(&mut self) {
        if !self.ended {
            lock(&self.provider.circuit).probing = false;
        }
    }
}

/// What a fallback attempt did.
#[derive(Debug)]
pub(crate) enum FallbackRun<T> {
    /// The fallback ran and produced this.
    Ran(T),
    /// The circuit is open, or another attempt is in flight: the fallback was
    /// not run.
    Skipped {
        /// When an open circuit closes again (RFC 3339, UTC); `None` when it is
        /// closed (the attempt was skipped because another is in flight).
        reopens: Option<String>,
    },
}

impl Provider {
    pub(crate) fn new(cfg: Config) -> Self {
        Self::with_clock(cfg, Arc::new(Instant::now))
    }

    pub(crate) fn with_clock(cfg: Config, clock: Clock) -> Self {
        Provider {
            cfg,
            clock,
            state: Mutex::new(State::default()),
            walked: Condvar::new(),
            circuit: Mutex::new(Circuit {
                open_until: None,
                open_until_utc: None,
                next_backoff: cfg.circuit_base,
                probing: false,
            }),
            reads: AtomicU64::new(0),
            memo_hits: AtomicU64::new(0),
            fallbacks: AtomicU64::new(0),
            failures_in_a_row: AtomicU64::new(0),
        }
    }

    fn now(&self) -> Instant {
        (self.clock)()
    }

    /// Log a failed in-process read: WARN on the first of a run and every
    /// hundredth after it, DEBUG otherwise — a platform where the read can
    /// never work must not warn on every call.
    fn note_read_failure(&self, what: &str, why: &str) {
        if in_process_disabled() {
            // Disabled on purpose and announced once (`in_process_disabled`);
            // not a failure worth a line per read.
            tracing::debug!("process_table: in-process {what} read skipped: {why}");
            return;
        }
        let n = self.failures_in_a_row.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 || n % 100 == 0 {
            tracing::warn!("process_table: in-process {what} read failed ({n} in a row): {why}");
        } else {
            tracing::debug!("process_table: in-process {what} read failed ({n} in a row): {why}");
        }
    }

    /// Serve a snapshot no older than `max_age`: from the memo, by joining the
    /// walk already running, or by walking.
    ///
    /// `Ok` is the answer. `Err` is a [`Flight`]: this caller led a walk that
    /// failed, and owes the flight its settlement (after a fallback, or with
    /// none) — every concurrent caller is waiting on it.
    pub(crate) fn lead_or_join(
        &self,
        max_age: Duration,
        walk: impl FnOnce() -> Result<ProcessSnapshot, String>,
    ) -> Result<ProcessSnapshot, Flight<'_>> {
        let now = self.now();
        self.reads.fetch_add(1, Ordering::Relaxed);
        let mut st = lock(&self.state);
        loop {
            if let Some((at, snap)) = &st.last_good {
                if serves(*at, now, max_age) {
                    self.memo_hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(snap.clone());
                }
            }
            // SINGLE FLIGHT: a walk is already running, so wait for it rather
            // than start a second one beside it — but never longer than
            // `join_wait_max` (real time).
            let Some(started) = st.in_flight else { break };
            let generation = st.generation;
            let give_up = Instant::now() + self.cfg.join_wait_max;
            while st.in_flight.is_some() && st.generation == generation {
                let left = give_up.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    tracing::debug!(
                        "process_table: a walk in flight did not settle within {}s; answering stale",
                        self.cfg.join_wait_max.as_secs()
                    );
                    return Ok(stale_of(&st));
                }
                st = self
                    .walked
                    .wait_timeout(st, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            if serves(started, now, max_age) {
                if let Some(delivered) = &st.delivered {
                    self.memo_hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(delivered.clone());
                }
            }
            // That walk began before this caller may accept; look again.
        }
        st.in_flight = Some(now);
        drop(st);
        // Built BEFORE the walk, so a panicking walk still settles the flight.
        let flight = Flight {
            provider: self,
            started: now,
            settled: false,
        };
        match walk() {
            Ok(snap) => Ok(flight.settle(Some(snap))),
            Err(why) => {
                self.note_read_failure("snapshot", &why);
                Err(flight)
            }
        }
    }

    fn complete(&self, started: Instant, fresh: Option<ProcessSnapshot>) -> ProcessSnapshot {
        let mut st = lock(&self.state);
        let out = match fresh {
            Some(mut snap) => {
                snap.stale = false;
                self.failures_in_a_row.store(0, Ordering::Relaxed);
                st.last_good = Some((started, snap.clone()));
                snap
            }
            None => stale_of(&st),
        };
        st.delivered = Some(out.clone());
        st.generation = st.generation.wrapping_add(1);
        st.in_flight = None;
        drop(st);
        self.walked.notify_all();
        out
    }

    /// Take the one fallback slot: `None` when the circuit is open or another
    /// attempt is in flight. A taken slot is counted as an attempt.
    pub(crate) fn begin_fallback(&self) -> Option<Probe<'_>> {
        let now = self.now();
        let mut c = lock(&self.circuit);
        if c.probing || c.open_until.is_some_and(|until| now < until) {
            return None;
        }
        c.probing = true;
        self.fallbacks.fetch_add(1, Ordering::Relaxed);
        Some(Probe {
            provider: self,
            ended: false,
        })
    }

    /// Record a fallback attempt's outcome at the moment it ENDED: a failure
    /// opens the circuit for the current backoff from now, and doubles the
    /// backoff (to [`Config::circuit_max`]); a success closes it and resets the
    /// backoff. Frees the in-flight slot either way.
    fn end_fallback(&self, ok: bool, what: &str) {
        let ended = self.now();
        let mut c = lock(&self.circuit);
        c.probing = false;
        if ok {
            if c.open_until.is_some() || c.next_backoff != self.cfg.circuit_base {
                tracing::info!(
                    "process_table: the external-probe fallback answered ({what}); circuit closed"
                );
            }
            c.open_until = None;
            c.open_until_utc = None;
            c.next_backoff = self.cfg.circuit_base;
        } else {
            let backoff = c.next_backoff;
            c.open_until = Some(ended + backoff);
            c.open_until_utc = chrono::Duration::from_std(backoff)
                .ok()
                .map(|d| chrono::Utc::now() + d);
            c.next_backoff = backoff.saturating_mul(2).min(self.cfg.circuit_max);
            tracing::warn!(
                "process_table: the external-probe fallback failed ({what}); skipping it for {}s",
                backoff.as_secs()
            );
        }
    }

    /// Run `fallback` in the one fallback slot, recording whether `ok` judged
    /// its result a success.
    pub(crate) fn run_fallback<T>(
        &self,
        what: &str,
        fallback: impl FnOnce() -> T,
        ok: impl FnOnce(&T) -> bool,
    ) -> FallbackRun<T> {
        let Some(probe) = self.begin_fallback() else {
            return FallbackRun::Skipped {
                reopens: self.circuit_open_until(),
            };
        };
        let out = fallback();
        probe.end(ok(&out), what);
        FallbackRun::Ran(out)
    }

    /// When the open circuit closes (RFC 3339, UTC); `None` when it is closed.
    fn circuit_open_until(&self) -> Option<String> {
        let now = self.now();
        let c = lock(&self.circuit);
        c.open_until
            .filter(|until| now < *until)
            .and(c.open_until_utc)
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    }

    /// Single-flight snapshot from a synchronous context: memo, join or walk,
    /// then `fallback` (in the fallback slot) when the walk failed. An empty
    /// table from the fallback is a failed fallback.
    pub(crate) fn snapshot_with(
        &self,
        max_age: Duration,
        walk: impl FnOnce() -> Result<ProcessSnapshot, String>,
        fallback: impl FnOnce() -> Option<ProcessSnapshot>,
    ) -> ProcessSnapshot {
        match self.lead_or_join(max_age, walk) {
            Ok(snap) => snap,
            Err(flight) => {
                let fresh = match self.run_fallback(
                    "snapshot",
                    || fallback().filter(|s| !s.parent_map.is_empty()),
                    Option::is_some,
                ) {
                    FallbackRun::Ran(fresh) => fresh,
                    FallbackRun::Skipped { .. } => None,
                };
                flight.settle(fresh)
            }
        }
    }

    /// The `/health` `processTable` object.
    pub(crate) fn health_json(&self) -> serde_json::Value {
        serde_json::json!({
            "reads": self.reads.load(Ordering::Relaxed),
            "memoHits": self.memo_hits.load(Ordering::Relaxed),
            "powershellFallbacks": self.fallbacks.load(Ordering::Relaxed),
            "circuitOpenUntil": self.circuit_open_until(),
        })
    }
}

/// The runner's provider.
pub(crate) fn provider() -> &'static Provider {
    static PROVIDER: OnceLock<Provider> = OnceLock::new();
    PROVIDER.get_or_init(|| Provider::new(Config::default()))
}

/// `/health` `processTable {reads, memoHits, powershellFallbacks,
/// circuitOpenUntil}`: snapshot reads served, how many of them the memo or a
/// walk in flight answered, external-probe fallback ATTEMPTS (all kinds:
/// snapshot, command lines, port owner, the external-claude scan), and when an
/// open fallback circuit closes (`null` = closed). In-process only.
pub(crate) fn health_json() -> serde_json::Value {
    provider().health_json()
}

/// The Windows-only kill switch (see the module docs). Off Windows the
/// in-process read is the only read, so it is never disabled.
fn in_process_disabled() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| {
        let disabled =
            cfg!(windows) && std::env::var(ENV_IN_PROCESS).is_ok_and(|v| v.trim() == "0");
        if disabled {
            tracing::warn!(
                "process_table: {ENV_IN_PROCESS}=0 -- the in-process read is disabled; every \
                 snapshot, command-line and port-owner read takes the external-probe fallback"
            );
        }
        disabled
    })
}

/// One in-process walk, stamped and validated: a table that lists no process,
/// or does not list this process, is a failed read — the runner is always
/// live.
fn walk_in_process() -> Result<ProcessSnapshot, String> {
    if in_process_disabled() {
        return Err(format!("{ENV_IN_PROCESS}=0 disables the in-process read"));
    }
    let began_ms = chrono::Utc::now().timestamp_millis();
    let mut snap = std::panic::catch_unwind(platform::walk)
        .map_err(|_| "the in-process walk panicked".to_string())??;
    snap.observed_at_ms = began_ms;
    validate_walk(snap, std::process::id())
}

fn validate_walk(mut snap: ProcessSnapshot, self_pid: u32) -> Result<ProcessSnapshot, String> {
    if snap.creation_times.is_empty() {
        return Err("the in-process walk listed no processes".to_string());
    }
    if !snap.creation_times.contains_key(&self_pid) {
        return Err(format!(
            "the in-process walk did not list this process (pid {self_pid})"
        ));
    }
    snap.stale = false;
    Ok(snap)
}

/// A process-table snapshot, at most [`MEMO_TTL`] old (see the module docs).
pub(crate) async fn snapshot() -> ProcessSnapshot {
    read_with(
        provider(),
        MEMO_TTL,
        walk_in_process,
        platform::fallback_snapshot,
    )
    .await
}

/// A process-table snapshot whose walk began after this call — for a caller
/// acting on ABSENCE (closing a tab, reaping a pane, deciding nobody holds a
/// session, reaping an orphan), where a memoized observation could predate
/// the very process it is looking for.
pub(crate) async fn snapshot_fresh() -> ProcessSnapshot {
    read_with(
        provider(),
        Duration::ZERO,
        walk_in_process,
        platform::fallback_snapshot,
    )
    .await
}

/// The async read every snapshot goes through: `walk` on the blocking pool
/// (memo, single flight), then — when this caller led a walk that failed —
/// `fallback` in the fallback slot, its result settling the flight. Generic
/// over the walk and the fallback so tests drive this exact path.
pub(crate) async fn read_with<W, F, Fut>(
    p: &'static Provider,
    max_age: Duration,
    walk: W,
    fallback: F,
) -> ProcessSnapshot
where
    W: FnOnce() -> Result<ProcessSnapshot, String> + Send + 'static,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Option<ProcessSnapshot>> + Send + 'static,
{
    let led = spawn_blocking_tracked(move || p.lead_or_join(max_age, walk)).await;
    let flight = match led {
        Ok(Ok(snap)) => return snap,
        Ok(Err(flight)) => flight,
        Err(e) => {
            // The flight (if any) was settled by its drop during the unwind.
            tracing::warn!("process_table: the in-process read did not join: {e}");
            return ProcessSnapshot::default();
        }
    };
    let fresh = match p.begin_fallback() {
        Some(probe) => {
            let attempt = fallback();
            in_slot(probe, "snapshot", async move {
                attempt.await.filter(|s| !s.parent_map.is_empty())
            })
            .await
        }
        None => None,
    };
    flight.settle(fresh)
}

/// Run one fallback `attempt` on a DETACHED task that holds `probe` (the
/// fallback slot) until the attempt completes, recording `Some` as success.
/// A caller cancelled mid-attempt therefore does not free the slot while the
/// external probe it started (a PowerShell child, up to its own budget) is
/// still running: the next attempt waits for that one to end.
async fn in_slot<T, Fut>(probe: Probe<'static>, what: &'static str, attempt: Fut) -> Option<T>
where
    T: Send + 'static,
    Fut: Future<Output = Option<T>> + Send + 'static,
{
    let task = tokio::spawn(async move {
        let out = attempt.await;
        probe.end(out.is_some(), what);
        out
    });
    // A panicked attempt dropped its probe un-ended, which freed the slot.
    task.await.unwrap_or(None)
}

/// A snapshot from a SYNCHRONOUS caller (memo, join or walk; a join waits at
/// most [`JOIN_WAIT_MAX`]). The PowerShell snapshot fallback is async and is
/// not run from here: a failed walk answers stale, and the caller applies its
/// own fallback.
pub(crate) fn snapshot_blocking() -> ProcessSnapshot {
    match provider().lead_or_join(MEMO_TTL, walk_in_process) {
        Ok(snap) => snap,
        Err(flight) => flight.settle(None),
    }
}

/// Run a caller-owned fallback (`f`) in the provider's fallback slot from a
/// synchronous context.
pub(crate) fn run_fallback_blocking<T>(
    what: &str,
    f: impl FnOnce() -> T,
    ok: impl FnOnce(&T) -> bool,
) -> FallbackRun<T> {
    provider().run_fallback(what, f, ok)
}

/// Join argv into one command line, re-quoting an argument that contains
/// whitespace so the line reads like the one the process was started with.
fn join_argv<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> String {
    argv.iter()
        .map(|a| {
            let a = a.as_ref().to_string_lossy();
            if a.chars().any(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Command lines for exactly `pids`, read in-process, from a synchronous
/// context. `Err` when the read itself failed (Windows: it did not even list
/// this process, or the kill switch is set); a pid that is dead or unreadable
/// is simply absent.
pub(crate) fn command_lines_blocking(pids: &[u32]) -> Result<HashMap<u32, String>, String> {
    if pids.is_empty() {
        return Ok(HashMap::new());
    }
    if in_process_disabled() {
        return Err(format!("{ENV_IN_PROCESS}=0 disables the in-process read"));
    }
    std::panic::catch_unwind(|| platform::command_lines(pids))
        .map_err(|_| "the in-process command-line read panicked".to_string())?
}

/// Command lines for exactly `pids`: in-process, with the external probe
/// (Windows: the WMI query) as the fallback when the in-process read fails.
///
/// `None` when the read failed AND no fallback answered (it failed, its
/// circuit is open, or another attempt holds the slot): UNKNOWN, never "none
/// of these pids has a command line". `Some` maps every pid whose command
/// line was read; a dead or unreadable pid is simply absent from it.
pub(crate) async fn command_lines_for(pids: &[u32]) -> Option<HashMap<u32, String>> {
    command_lines_with(
        provider(),
        pids,
        |asked| command_lines_blocking(&asked),
        platform::fallback_command_lines,
    )
    .await
}

/// The async command-line read: `read` on the blocking pool, then `fallback`
/// in the fallback slot when `read` failed. Generic so tests drive this exact
/// path.
pub(crate) async fn command_lines_with<R, F, Fut>(
    p: &'static Provider,
    pids: &[u32],
    read: R,
    fallback: F,
) -> Option<HashMap<u32, String>>
where
    R: FnOnce(Vec<u32>) -> Result<HashMap<u32, String>, String> + Send + 'static,
    F: FnOnce(Vec<u32>) -> Fut,
    Fut: Future<Output = Option<HashMap<u32, String>>> + Send + 'static,
{
    if pids.is_empty() {
        return Some(HashMap::new());
    }
    let asked = pids.to_vec();
    let for_read = asked.clone();
    match spawn_blocking_tracked(move || read(for_read)).await {
        Ok(Ok(lines)) => return Some(lines),
        Ok(Err(why)) => p.note_read_failure("command-line", &why),
        Err(e) => p.note_read_failure("command-line", &format!("did not join: {e}")),
    }
    let probe = p.begin_fallback()?;
    in_slot(probe, "command lines", fallback(asked)).await
}

/// The pid owning the LISTENING socket on local TCP `port` (Windows: the
/// listener-class `GetExtendedTcpTable` read), with the external probe as the
/// fallback when the table cannot be read. `None` = nothing listens there, or
/// unknown. Unix callers keep their own `lsof` read
/// ([`super::process_tree::port_owner_pid`]) and do not reach this.
pub(crate) fn port_owner(port: u16) -> Option<u32> {
    let p = provider();
    if !in_process_disabled() {
        match platform::listening_port_owner(port) {
            Ok(owner) => return owner,
            Err(why) => p.note_read_failure("port-owner", &format!("port {port}: {why}")),
        }
    }
    match p.run_fallback(
        "port owner",
        || platform::fallback_port_owner(port),
        Option::is_some,
    ) {
        FallbackRun::Ran(owner) => owner.flatten(),
        FallbackRun::Skipped { .. } => None,
    }
}

/// The LISTENING socket on local TCP `port` with its owner AND the socket's
/// own creation time (Windows: the owner-MODULE listener class of the same
/// `GetExtendedTcpTable` binding), read in-process ALONE: there is no
/// external fallback. `Err` when the table cannot be read, or the
/// [`ENV_IN_PROCESS`] kill switch disables the in-process read: UNKNOWN, as
/// distinct from `Ok(None)`, nothing listens. The port kill
/// (`health::kill_port_process`) reads this and refuses on `Err`: the kill
/// path creates no process, and a kill is not decided from a spawned probe's
/// answer. Off Windows there is no such read (`Err`).
pub(crate) fn port_listener_in_process(port: u16) -> Result<Option<ListenerOwner>, String> {
    if in_process_disabled() {
        return Err(format!("{ENV_IN_PROCESS}=0 disables the in-process read"));
    }
    platform::listening_port_listener(port).map_err(|why| {
        let why = format!("port {port}: {why}");
        provider().note_read_failure("port-listener", &why);
        why
    })
}

/// Whether `name` is a node runtime image (the external-claude scan's filter).
fn is_node_image(name: &str) -> bool {
    if cfg!(windows) {
        name.eq_ignore_ascii_case("node.exe")
    } else {
        name.contains("node")
    }
}

/// Whether a command line names claude (Windows: case-insensitive, as the
/// PowerShell `-match 'claude'` it replaces was).
///
/// SEQUENCING: open qontinui-runner PR #1990 widens the subprocess
/// enumeration's match from `claude` to every CLI profile's program
/// (`cli_profile::all_programs`). This is now the predicate the scan actually
/// applies — whichever of #1990 and this change lands second must widen THIS
/// function the same way, or the in-process scan keeps matching claude alone.
fn mentions_claude(cmdline: &str) -> bool {
    if cfg!(windows) {
        cmdline.to_ascii_lowercase().contains("claude")
    } else {
        cmdline.contains("claude")
    }
}

/// `(pid, command line)` of every `node` process whose command line names
/// `claude`, sorted by pid — the set the external-session scan reports — from
/// the shared snapshot plus an in-process read of the candidates' command
/// lines. `Err` when either read could not be made in-process; the caller's
/// own enumeration is then the fallback.
pub(crate) fn node_claude_processes() -> Result<Vec<(u32, String)>, String> {
    node_claude_processes_in(&snapshot_blocking(), command_lines_blocking)
}

fn node_claude_processes_in(
    snap: &ProcessSnapshot,
    command_lines: impl FnOnce(&[u32]) -> Result<HashMap<u32, String>, String>,
) -> Result<Vec<(u32, String)>, String> {
    if snap.is_unknown() {
        return Err("the process table is stale: the in-process read failed".to_string());
    }
    let candidates: Vec<u32> = snap
        .names
        .iter()
        .filter(|(_, name)| is_node_image(name))
        .map(|(pid, _)| *pid)
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let mut found: Vec<(u32, String)> = command_lines(&candidates)?
        .into_iter()
        .filter(|(_, line)| mentions_claude(line))
        .collect();
    found.sort_by_key(|(pid, _)| *pid);
    Ok(found)
}

// ============================================================================
// Creation-time identity (#2386's rule), platform-independent
// ============================================================================

/// Seconds between the FILETIME epoch (1601-01-01) and the Unix epoch.
const FILETIME_UNIX_EPOCH_SECS: u64 = 11_644_473_600;
/// FILETIME ticks (100 ns) per second.
const FILETIME_TICKS_PER_SEC: u64 = 10_000_000;

/// Unix seconds as the FILETIME at the start of that second.
pub(super) fn unix_secs_to_filetime(secs: u64) -> u64 {
    secs.saturating_add(FILETIME_UNIX_EPOCH_SECS)
        .saturating_mul(FILETIME_TICKS_PER_SEC)
}

/// A FILETIME as Unix seconds, truncated. Also the unit `health`'s tree kill
/// compares a held handle's creation time in against a recorded one.
pub(super) fn filetime_to_unix_secs(ft: u64) -> i64 {
    i64::try_from(ft / FILETIME_TICKS_PER_SEC).unwrap_or(i64::MAX) - FILETIME_UNIX_EPOCH_SECS as i64
}

/// One process as the Windows walk read it.
#[derive(Debug, Clone)]
struct Row {
    pid: u32,
    ppid: u32,
    /// The process's precise creation time (FILETIME, 100 ns), read through a
    /// query handle after the enumeration; `None` when it could not be opened
    /// (protected, another user's, already exited).
    created_ft: Option<u64>,
    name: String,
    exe: Option<String>,
}

/// Build the snapshot from one walk, applying #2386's identity rule
/// ([`qontinui_runner_win32::process_tree::admits_descendant`]) to each row's
/// PRECISE creation time against `walk_began` (a FILETIME taken before the
/// enumeration):
///
/// - a creation time later than `walk_began` belongs to a process that took
///   the pid AFTER the enumeration listed it (the creation times are read
///   after the one-shot enumeration), so the row's creation time is withheld:
///   `creation_times` holds `0` (unknown) and `creation_filetimes` no entry;
/// - a child created before the process that now holds its recorded parent
///   pid is not that process's child (its real parent died and the pid was
///   reused): it is listed under `0`, as an orphan, never under the stranger.
///
/// Both guards compare 100 ns FILETIMEs. The residual window is the kernel's
/// creation-time granularity: a process created after `walk_began` but stamped
/// within the same clock tick passes the first guard.
///
/// A row whose creation time could not be read (`created_ft: None`) carries
/// creation `0` (unknown) and no `creation_filetimes` entry; neither guard can
/// judge it, so its recorded parent link is kept, exactly as the WMI path
/// always kept it. Every consumer of creation times already reads `0` as
/// unknown — a PID-reuse check against it fails closed (`orphan_state`), and a
/// liveness check does not treat it as reuse (`claude_present_in_inclusive_subtree`).
fn build_snapshot(rows: &[Row], walk_began: u64) -> ProcessSnapshot {
    use qontinui_runner_win32::process_tree::admits_descendant;

    let created: HashMap<u32, u64> = rows
        .iter()
        .filter_map(|r| r.created_ft.map(|ft| (r.pid, ft)))
        .filter(|(_, ft)| *ft > 0 && *ft <= walk_began)
        .collect();
    let mut snap = ProcessSnapshot::default();
    for r in rows {
        let ppid = match (created.get(&r.pid), created.get(&r.ppid)) {
            (Some(&child), Some(&parent))
                if r.ppid != 0
                    && r.ppid != r.pid
                    && !admits_descendant(child, parent, walk_began) =>
            {
                0
            }
            _ => r.ppid,
        };
        snap.parent_map.entry(ppid).or_default().push(r.pid);
        match created.get(&r.pid) {
            Some(&ft) => {
                snap.creation_times.insert(r.pid, filetime_to_unix_secs(ft));
                snap.creation_filetimes.insert(r.pid, ft);
            }
            None => {
                snap.creation_times.insert(r.pid, 0);
            }
        }
        snap.names.insert(r.pid, r.name.clone());
        if let Some(exe) = r.exe.as_ref().filter(|e| !e.trim().is_empty()) {
            snap.exe_paths.insert(r.pid, exe.clone());
        }
    }
    snap
}

// ============================================================================
// Platform halves. Every function exists on both sides.
// ============================================================================

#[cfg(windows)]
mod platform {
    use super::{build_snapshot, join_argv, Row};
    use crate::process_capture::process_tree::{self, ProcessSnapshot};
    use std::collections::HashMap;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    /// One `NtQuerySystemInformation` enumeration (via `sysinfo`), with exe
    /// paths (the qontinui-shim exclusion needs them), then each row's precise
    /// creation time.
    pub(super) fn walk() -> Result<ProcessSnapshot, String> {
        let walk_began = qontinui_runner_win32::holder_spawn::filetime_now();
        let mut sys = System::new();
        let listed = sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new().with_exe(UpdateKind::Always),
        );
        if listed == 0 {
            return Err("NtQuerySystemInformation listed no processes".to_string());
        }
        let rows: Vec<Row> = sys
            .processes()
            .iter()
            .map(|(pid, p)| Row {
                pid: pid.as_u32(),
                ppid: p.parent().map_or(0, |pp| pp.as_u32()),
                created_ft: precise_creation_filetime(pid.as_u32()),
                name: p.name().to_string_lossy().into_owned(),
                exe: p.exe().map(|e| e.to_string_lossy().into_owned()),
            })
            .collect();
        Ok(build_snapshot(&rows, walk_began))
    }

    /// `pid`'s creation time as a FILETIME, through a
    /// `PROCESS_QUERY_LIMITED_INFORMATION` handle (the least access that reads
    /// it — no terminate right is asked for a read) and #2386's
    /// `creation_time`. `None` when the process cannot be opened.
    pub(super) fn precise_creation_filetime(pid: u32) -> Option<u64> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        if pid == 0 {
            return None;
        }
        // SAFETY: OpenProcess takes no pointers; a non-null handle is passed to
        // `creation_time` (which requires query access, granted here) and
        // closed exactly once.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let created = qontinui_runner_win32::process_tree::creation_time(handle);
            CloseHandle(handle);
            created
        }
    }

    /// `cmd()` for exactly `pids`. This process is asked for too: a read that
    /// does not list it did not read the table at all.
    pub(super) fn command_lines(pids: &[u32]) -> Result<HashMap<u32, String>, String> {
        let me = Pid::from_u32(std::process::id());
        let mut asked: Vec<Pid> = pids.iter().map(|p| Pid::from_u32(*p)).collect();
        asked.push(me);
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&asked),
            true,
            ProcessRefreshKind::new().with_cmd(UpdateKind::Always),
        );
        if sys.process(me).is_none() {
            return Err("the in-process command-line read did not list this process".to_string());
        }
        Ok(pids
            .iter()
            .filter_map(|pid| {
                let line = join_argv(sys.process(Pid::from_u32(*pid))?.cmd());
                (!line.is_empty()).then_some((*pid, line))
            })
            .collect())
    }

    /// The listener-table read of the peer-user guard's binding.
    pub(super) fn listening_port_owner(port: u16) -> Result<Option<u32>, String> {
        qontinui_runner_lib::peer_user_guard::windows::listening_port_owner(port)
            .map_err(|e| e.as_str().to_string())
    }

    /// The owner-MODULE listener read of the same binding (owner + the
    /// socket's creation time).
    pub(super) fn listening_port_listener(
        port: u16,
    ) -> Result<Option<super::ListenerOwner>, String> {
        qontinui_runner_lib::peer_user_guard::windows::listening_port_owner_bind(port)
            .map_err(|e| e.as_str().to_string())
    }

    /// The PowerShell/WMI snapshot. An empty table is a failed fallback.
    pub(super) async fn fallback_snapshot() -> Option<ProcessSnapshot> {
        let began_ms = chrono::Utc::now().timestamp_millis();
        let mut snap = process_tree::powershell_snapshot().await;
        snap.observed_at_ms = began_ms;
        (!snap.parent_map.is_empty()).then_some(snap)
    }

    /// The WMI command-line query.
    pub(super) async fn fallback_command_lines(pids: Vec<u32>) -> Option<HashMap<u32, String>> {
        process_tree::command_lines_for_pids_powershell(&pids).await
    }

    /// The `Get-NetTCPConnection` query (outer `None` = it degraded).
    pub(super) fn fallback_port_owner(port: u16) -> Option<Option<u32>> {
        process_tree::port_owner_pid_powershell(port)
    }
}

#[cfg(not(windows))]
mod platform {
    use crate::process_capture::process_tree::{self, ProcessSnapshot};
    use std::collections::HashMap;

    /// The existing `/proc` walk.
    pub(super) fn walk() -> Result<ProcessSnapshot, String> {
        Ok(process_tree::snapshot_process_table_sync())
    }

    /// The `/proc` walk already carries second-granular creation times; there
    /// is no finer per-pid read here.
    pub(super) fn precise_creation_filetime(_pid: u32) -> Option<u64> {
        None
    }

    /// `/proc/<pid>/cmdline` for exactly `pids`.
    pub(super) fn command_lines(pids: &[u32]) -> Result<HashMap<u32, String>, String> {
        Ok(process_tree::proc_command_lines(pids))
    }

    /// No in-process listener table here (Unix callers use `lsof`).
    pub(super) fn listening_port_owner(_port: u16) -> Result<Option<u32>, String> {
        Err("no in-process listener-table read on this platform".to_string())
    }

    /// No in-process listener table here.
    pub(super) fn listening_port_listener(
        _port: u16,
    ) -> Result<Option<super::ListenerOwner>, String> {
        Err("no in-process listener-table read on this platform".to_string())
    }

    /// No external snapshot fallback: `/proc` is the read.
    pub(super) async fn fallback_snapshot() -> Option<ProcessSnapshot> {
        None
    }

    /// No external command-line fallback: `/proc` is the read.
    pub(super) async fn fallback_command_lines(_pids: Vec<u32>) -> Option<HashMap<u32, String>> {
        None
    }

    /// No external port-owner fallback through this provider.
    pub(super) fn fallback_port_owner(_port: u16) -> Option<Option<u32>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Barrier;

    fn cfg() -> Config {
        Config::default()
    }

    /// A clock the test moves by hand.
    #[derive(Clone)]
    struct FakeClock {
        base: Instant,
        offset_ms: Arc<AtomicU64>,
    }

    impl FakeClock {
        fn new() -> Self {
            FakeClock {
                base: Instant::now(),
                offset_ms: Arc::new(AtomicU64::new(0)),
            }
        }
        fn clock(&self) -> Clock {
            let (base, offset) = (self.base, Arc::clone(&self.offset_ms));
            Arc::new(move || base + Duration::from_millis(offset.load(Ordering::SeqCst)))
        }
        fn advance(&self, d: Duration) {
            self.offset_ms
                .fetch_add(d.as_millis() as u64, Ordering::SeqCst);
        }
    }

    /// A non-empty table that lists `pid`.
    fn table(pid: u32) -> ProcessSnapshot {
        let mut s = ProcessSnapshot::default();
        s.parent_map.insert(1, vec![pid]);
        s.creation_times.insert(pid, 1_000);
        s.names.insert(pid, "runner.exe".to_string());
        s.stale = false;
        s
    }

    /// N concurrent callers, all arriving while one walk runs, cost ONE walk;
    /// every caller gets that walk's (fresh) table.
    #[test]
    fn process_table_concurrent_callers_cause_one_walk() {
        const CALLERS: usize = 8;
        let provider = Arc::new(Provider::new(cfg()));
        let walks = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(CALLERS));
        let handles: Vec<_> = (0..CALLERS)
            .map(|_| {
                let (provider, walks, barrier) = (
                    Arc::clone(&provider),
                    Arc::clone(&walks),
                    Arc::clone(&barrier),
                );
                std::thread::spawn(move || {
                    barrier.wait();
                    provider.snapshot_with(
                        MEMO_TTL,
                        || {
                            walks.fetch_add(1, Ordering::SeqCst);
                            // Long enough that every caller arrives mid-walk.
                            std::thread::sleep(Duration::from_millis(400));
                            Ok(table(42))
                        },
                        || panic!("the fallback must not run when the walk succeeds"),
                    )
                })
            })
            .collect();
        for h in handles {
            let snap = h.join().expect("caller panicked");
            assert!(!snap.stale);
            assert_eq!(snap.names.get(&42).map(String::as_str), Some("runner.exe"));
        }
        assert_eq!(
            walks.load(Ordering::SeqCst),
            1,
            "concurrent callers must share one walk"
        );
        let health = provider.health_json();
        assert_eq!(health["reads"], 8);
        assert_eq!(health["memoHits"], 7);
    }

    /// A FRESH caller that arrives while a walk which began BEFORE it is in
    /// flight waits for that walk and then walks again — it never takes an
    /// observation older than its own call. A memo caller arriving at the same
    /// moment joins the first walk.
    #[test]
    fn process_table_a_fresh_caller_never_takes_a_walk_that_began_before_it() {
        let clock = FakeClock::new();
        let provider = Arc::new(Provider::with_clock(cfg(), clock.clock()));
        let walks = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();

        let leader = {
            let (provider, walks) = (Arc::clone(&provider), Arc::clone(&walks));
            std::thread::spawn(move || {
                provider.snapshot_with(
                    MEMO_TTL,
                    || {
                        walks.fetch_add(1, Ordering::SeqCst);
                        started_tx.send(()).unwrap();
                        // Hold the walk open while the others queue up.
                        std::thread::sleep(Duration::from_millis(400));
                        Ok(table(1))
                    },
                    || None,
                )
            })
        };
        started_rx.recv().unwrap();
        // The others ask strictly after the leader's walk began.
        clock.advance(Duration::from_millis(1));
        let joiner = {
            let (provider, walks) = (Arc::clone(&provider), Arc::clone(&walks));
            std::thread::spawn(move || {
                provider.snapshot_with(
                    MEMO_TTL,
                    || {
                        walks.fetch_add(1, Ordering::SeqCst);
                        Ok(table(2))
                    },
                    || None,
                )
            })
        };
        let fresh = {
            let (provider, walks) = (Arc::clone(&provider), Arc::clone(&walks));
            std::thread::spawn(move || {
                provider.snapshot_with(
                    Duration::ZERO,
                    || {
                        walks.fetch_add(1, Ordering::SeqCst);
                        Ok(table(3))
                    },
                    || None,
                )
            })
        };
        assert!(leader.join().unwrap().names.contains_key(&1));
        let joined = joiner.join().unwrap();
        let fresh = fresh.join().unwrap();
        assert!(
            fresh.names.contains_key(&3),
            "the fresh caller walked itself: {:?}",
            fresh.names
        );
        assert!(
            joined.names.contains_key(&1) || joined.names.contains_key(&3),
            "the memo caller joined a walk: {:?}",
            joined.names
        );
        assert_eq!(walks.load(Ordering::SeqCst), 2, "leader + the fresh caller");
    }

    /// Within the TTL the memo answers; past it, a new walk runs; a fresh read
    /// never takes the memo.
    #[test]
    fn process_table_memo_serves_within_the_ttl_then_walks_again() {
        let clock = FakeClock::new();
        let p = Provider::with_clock(cfg(), clock.clock());
        let walks = AtomicUsize::new(0);
        let walk = || {
            walks.fetch_add(1, Ordering::SeqCst);
            Ok(table(7))
        };
        p.snapshot_with(MEMO_TTL, walk, || None);
        clock.advance(Duration::from_millis(1_999));
        p.snapshot_with(MEMO_TTL, walk, || None);
        assert_eq!(walks.load(Ordering::SeqCst), 1, "inside the 2 s TTL");
        clock.advance(Duration::from_millis(1));
        p.snapshot_with(MEMO_TTL, walk, || None);
        assert_eq!(
            walks.load(Ordering::SeqCst),
            2,
            "at the TTL a new walk runs"
        );
        clock.advance(Duration::from_millis(1));
        p.snapshot_with(Duration::ZERO, walk, || None);
        assert_eq!(walks.load(Ordering::SeqCst), 3, "a fresh read walks");
    }

    /// The circuit: a failed fallback opens it for 30 s counted from when the
    /// attempt ENDED (a 30 s attempt is not immediately retried), doubles to
    /// 60 s, and a success resets it.
    #[test]
    fn process_table_circuit_counts_from_the_end_of_a_failed_attempt_doubles_and_resets() {
        let clock = FakeClock::new();
        let p = Provider::with_clock(cfg(), clock.clock());
        let attempts = AtomicUsize::new(0);
        let failing_walk = || Err("scripted in-process failure".to_string());

        // 1. t=0: the fallback runs for 30 s (the scan budget) and fails.
        let s = p.snapshot_with(Duration::ZERO, failing_walk, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            clock.advance(Duration::from_secs(30));
            None
        });
        assert!(
            s.stale && s.is_unknown(),
            "nothing good yet: an empty stale table"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(p.health_json()["circuitOpenUntil"].is_string());

        // 2. t=59 s: 29 s after the attempt ended — still skipped.
        clock.advance(Duration::from_secs(29));
        let s = p.snapshot_with(Duration::ZERO, failing_walk, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            Some(table(9))
        });
        assert!(s.stale);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "the skip is counted from the end of the failed attempt"
        );

        // 3. t=60 s: runs again; fails at once → open for 60 s.
        clock.advance(Duration::from_secs(1));
        p.snapshot_with(Duration::ZERO, failing_walk, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            None
        });
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        clock.advance(Duration::from_secs(59));
        p.snapshot_with(Duration::ZERO, failing_walk, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            None
        });
        assert_eq!(attempts.load(Ordering::SeqCst), 2, "doubled to 60 s");

        // 4. t=120 s: runs and succeeds — fresh table, circuit closed.
        clock.advance(Duration::from_secs(1));
        let s = p.snapshot_with(Duration::ZERO, failing_walk, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            Some(table(9))
        });
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(!s.stale, "a fallback that answered is fresh");
        assert!(p.health_json()["circuitOpenUntil"].is_null());

        // 5. Reset: the next failure opens it for 30 s again, not 120 s.
        clock.advance(Duration::from_secs(10));
        p.snapshot_with(Duration::ZERO, failing_walk, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            None
        });
        clock.advance(Duration::from_secs(30));
        p.snapshot_with(Duration::ZERO, failing_walk, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            None
        });
        assert_eq!(attempts.load(Ordering::SeqCst), 5, "backoff reset to 30 s");
        assert_eq!(p.health_json()["powershellFallbacks"], 5);
    }

    /// ONE fallback attempt in flight at a time: while a probe is out, every
    /// other attempt is skipped (and so cannot fail and double the backoff);
    /// ending it, or dropping it un-ended, frees the slot.
    #[test]
    fn process_table_only_one_fallback_attempt_is_in_flight() {
        let p = Provider::new(cfg());
        let first = p.begin_fallback().expect("closed circuit, free slot");
        assert!(
            p.begin_fallback().is_none(),
            "a second attempt while the first is in flight is skipped"
        );
        assert!(matches!(
            p.run_fallback("test", || panic!("must not run"), |_: &()| true),
            FallbackRun::Skipped { reopens: None }
        ));
        first.end(true, "test");
        let second = p.begin_fallback().expect("the slot is free again");
        drop(second);
        assert!(
            p.begin_fallback().is_some(),
            "a dropped probe frees the slot too"
        );
        assert_eq!(p.health_json()["powershellFallbacks"], 3);
    }

    /// A caller waiting on another caller's walk gives up after
    /// `join_wait_max` and answers stale, instead of waiting for a leader that
    /// may need the very thread it is blocking.
    #[test]
    fn process_table_a_join_is_bounded() {
        let p = Arc::new(Provider::new(Config {
            join_wait_max: Duration::from_millis(100),
            ..cfg()
        }));
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let leader = {
            let p = Arc::clone(&p);
            std::thread::spawn(move || {
                p.snapshot_with(
                    MEMO_TTL,
                    || {
                        started_tx.send(()).unwrap();
                        std::thread::sleep(Duration::from_millis(1_500));
                        Ok(table(1))
                    },
                    || None,
                )
            })
        };
        started_rx.recv().unwrap();
        let t = Instant::now();
        let joined = p.snapshot_with(MEMO_TTL, || panic!("a joiner must not walk"), || None);
        assert!(joined.stale, "a timed-out join answers stale");
        assert!(
            t.elapsed() < Duration::from_millis(1_000),
            "waited {:?}",
            t.elapsed()
        );
        assert!(!leader.join().unwrap().stale);
    }

    /// The production ASYNC snapshot path (`read_with`, used by `snapshot` and
    /// `snapshot_fresh`), with an injected walk and fallback: a failed walk
    /// takes the fallback, a failed fallback opens the circuit, and the next
    /// read inside the skip does not run it.
    #[tokio::test]
    async fn process_table_async_read_takes_the_fallback_and_honours_the_circuit() {
        let clock = FakeClock::new();
        let p: &'static Provider = Box::leak(Box::new(Provider::with_clock(cfg(), clock.clock())));
        let attempts = Arc::new(AtomicUsize::new(0));

        let a = Arc::clone(&attempts);
        let s = read_with(
            p,
            Duration::ZERO,
            || Err("scripted".to_string()),
            || async move {
                a.fetch_add(1, Ordering::SeqCst);
                Some(table(5))
            },
        )
        .await;
        assert!(
            !s.stale && s.names.contains_key(&5),
            "the fallback answered"
        );

        clock.advance(Duration::from_secs(1));
        let a = Arc::clone(&attempts);
        let s = read_with(
            p,
            Duration::ZERO,
            || Err("scripted".to_string()),
            || async move {
                a.fetch_add(1, Ordering::SeqCst);
                None::<ProcessSnapshot>
            },
        )
        .await;
        assert!(
            s.stale && s.names.contains_key(&5),
            "last good, flagged stale"
        );

        clock.advance(Duration::from_secs(29));
        let a = Arc::clone(&attempts);
        let s = read_with(
            p,
            Duration::ZERO,
            || Err("scripted".to_string()),
            || async move {
                a.fetch_add(1, Ordering::SeqCst);
                Some(table(6))
            },
        )
        .await;
        assert!(s.stale, "inside the skip: no fallback, stale");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// Wait (bounded) until the fallback slot is free again. Taking it to look
    /// frees it at once (the probe is dropped un-ended).
    async fn slot_frees_within(p: &'static Provider, limit: Duration) -> bool {
        let give_up = Instant::now() + limit;
        while Instant::now() < give_up {
            if p.begin_fallback().is_some() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        false
    }

    /// `in_slot`: a `read_with` caller dropped MID-FALLBACK does not free the
    /// fallback slot while its attempt is still running — `begin_fallback()`
    /// stays `None` until the attempt completes, then answers `Some`.
    #[tokio::test]
    async fn process_table_a_cancelled_read_holds_the_slot_until_its_attempt_ends() {
        let p: &'static Provider = Box::leak(Box::new(Provider::new(cfg())));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let caller = tokio::spawn(read_with(
            p,
            Duration::ZERO,
            || Err("scripted".to_string()),
            move || async move {
                let _ = started_tx.send(());
                // Stands in for a PowerShell child still running.
                let _ = release_rx.await;
                Some(table(1))
            },
        ));
        started_rx.await.expect("the attempt started");
        caller.abort();
        assert!(caller.await.is_err(), "the caller was cancelled");

        assert!(
            p.begin_fallback().is_none(),
            "the slot is held while the cancelled caller's attempt still runs"
        );
        release_tx.send(()).expect("release the attempt");
        assert!(
            slot_frees_within(p, Duration::from_secs(5)).await,
            "the slot frees once the attempt ends"
        );
    }

    /// `in_slot`: an attempt that PANICS frees the slot (its probe is dropped
    /// un-ended), leaves the circuit closed, and the read answers stale.
    #[tokio::test]
    async fn process_table_a_panicking_attempt_frees_the_slot() {
        async fn panicking_attempt() -> Option<ProcessSnapshot> {
            panic!("scripted attempt panic")
        }
        let p: &'static Provider = Box::leak(Box::new(Provider::new(cfg())));
        let s = read_with(
            p,
            Duration::ZERO,
            || Err("scripted".to_string()),
            panicking_attempt,
        )
        .await;
        assert!(s.stale, "a panicked attempt answered nothing");
        assert!(
            p.begin_fallback().is_some(),
            "the panicked attempt freed the slot"
        );
        assert!(
            p.health_json()["circuitOpenUntil"].is_null(),
            "a panic is not recorded as a failed attempt"
        );
    }

    /// The production ASYNC command-line path (`command_lines_with`, used by
    /// `command_lines_for`), with an injected read and fallback.
    #[tokio::test]
    async fn process_table_async_command_lines_take_the_fallback_and_honour_the_circuit() {
        let clock = FakeClock::new();
        let p: &'static Provider = Box::leak(Box::new(Provider::with_clock(cfg(), clock.clock())));
        let attempts = Arc::new(AtomicUsize::new(0));

        type Lines = HashMap<u32, String>;
        let unreached = |_: Vec<u32>| -> std::future::Ready<Option<Lines>> {
            panic!("the fallback must not run here")
        };

        let ok = command_lines_with(
            p,
            &[7],
            |pids| Ok(HashMap::from([(pids[0], "claude --resume x".to_string())])),
            unreached,
        )
        .await;
        assert_eq!(
            ok.as_ref().and_then(|m| m.get(&7)).map(String::as_str),
            Some("claude --resume x")
        );

        let a = Arc::clone(&attempts);
        let failed = command_lines_with(
            p,
            &[7],
            |_| Err("scripted".to_string()),
            |_| async move {
                a.fetch_add(1, Ordering::SeqCst);
                None::<Lines>
            },
        )
        .await;
        assert_eq!(
            failed, None,
            "a failed read with a failed fallback is UNKNOWN"
        );

        let a = Arc::clone(&attempts);
        let skipped = command_lines_with(
            p,
            &[7],
            |_| Err("scripted".to_string()),
            |_| async move {
                a.fetch_add(1, Ordering::SeqCst);
                Some(Lines::new())
            },
        )
        .await;
        assert_eq!(
            skipped, None,
            "a read the open circuit kept from its fallback is UNKNOWN"
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "the open circuit skipped the second fallback"
        );
        let no_read = |_: Vec<u32>| -> Result<Lines, String> { panic!("no read for no pids") };
        assert_eq!(
            command_lines_with(p, &[], no_read, unreached).await,
            Some(HashMap::new()),
            "no pids asked: an empty answer, read without any I/O"
        );
    }

    /// A read that fails after a good one answers the good one, flagged stale.
    #[test]
    fn process_table_a_failed_read_after_a_good_one_is_the_last_good_flagged_stale() {
        let clock = FakeClock::new();
        let p = Provider::with_clock(cfg(), clock.clock());
        p.snapshot_with(MEMO_TTL, || Ok(table(5)), || None);
        clock.advance(Duration::from_secs(10));
        let s = p.snapshot_with(MEMO_TTL, || Err("scripted".to_string()), || None);
        assert!(s.stale, "the last good table, labelled");
        assert!(
            s.is_unknown(),
            "stale is unknown even though it is not empty"
        );
        assert!(s.names.contains_key(&5));
    }

    /// A failed FIRST read: `Default` itself is stale, so the answer is an
    /// empty table flagged stale — and the orphan reclaim does not act on it.
    #[test]
    fn process_table_a_failed_first_read_is_stale_and_orphan_reclaim_does_not_act() {
        assert!(ProcessSnapshot::default().stale, "Default is UNKNOWN");
        let p = Provider::new(cfg());
        let s = p.snapshot_with(Duration::ZERO, || Err("scripted".to_string()), || None);
        assert!(s.stale);
        let mut state = crate::process_capture::orphan_state::OrphanState::new();
        state.trees.insert(
            "backend".to_string(),
            vec![crate::process_capture::process_tree::PidWithSpawnedAt {
                pid: 77,
                spawned_at_unix: 1_000,
                spawned_at_ft: None,
            }],
        );
        assert!(crate::process_capture::orphan_state::reclaim_targets(&state, &s).is_err());
    }

    /// A walk that panics settles its flight: a waiter is released, and the
    /// next caller walks again.
    #[test]
    fn process_table_a_panicking_walk_strands_no_caller() {
        let p = Provider::new(cfg());
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            p.snapshot_with(MEMO_TTL, || panic!("scripted"), || None)
        }));
        assert!(r.is_err());
        let s = p.snapshot_with(MEMO_TTL, || Ok(table(3)), || None);
        assert!(!s.stale);
    }

    #[test]
    fn process_table_validate_refuses_an_empty_table_and_one_without_this_process() {
        assert!(validate_walk(ProcessSnapshot::default(), 42).is_err());
        assert!(validate_walk(table(41), 42).is_err());
        let ok = validate_walk(
            ProcessSnapshot {
                stale: true,
                ..table(42)
            },
            42,
        )
        .expect("lists this process");
        assert!(!ok.stale, "a validated walk is fresh");
    }

    /// #2386's identity rule over the walk's rows, at FILETIME precision.
    #[test]
    fn process_table_build_withholds_reused_pids_and_unlinks_impossible_parents() {
        let ft = |secs: u64, ticks: u64| unix_secs_to_filetime(secs) + ticks;
        let began = ft(2_000, 5);
        let row = |pid, ppid, created_ft, name: &str, exe: Option<&str>| Row {
            pid,
            ppid,
            created_ft,
            name: name.to_string(),
            exe: exe.map(str::to_string),
        };
        let rows = [
            row(10, 0, Some(ft(1_000, 500)), "parent.exe", None),
            // A genuine child, created 100 ns after its parent.
            row(
                11,
                10,
                Some(ft(1_000, 501)),
                "child.exe",
                Some("C:\\x\\child.exe"),
            ),
            // Same SECOND as its listed parent but 100 ns OLDER: 10's pid was
            // reused — a second-granular guard would have linked it.
            row(12, 10, Some(ft(1_000, 499)), "orphan.exe", Some("  ")),
            // Created 100 ns after the walk began (same second): the pid was
            // reused mid-walk.
            row(13, 10, Some(ft(2_000, 6)), "late.exe", None),
            // Unopenable: no creation time — its link is kept.
            row(14, 10, None, "protected.exe", None),
        ];
        let s = build_snapshot(&rows, began);
        let mut under_10 = s.parent_map.get(&10).cloned().unwrap_or_default();
        under_10.sort_unstable();
        assert_eq!(under_10, vec![11, 13, 14], "12 is not 10's child");
        let mut under_0 = s.parent_map.get(&0).cloned().unwrap_or_default();
        under_0.sort_unstable();
        assert_eq!(under_0, vec![10, 12]);
        assert_eq!(s.creation_times.get(&11), Some(&1_000));
        assert_eq!(s.creation_filetimes.get(&11), Some(&ft(1_000, 501)));
        assert_eq!(
            s.creation_times.get(&13),
            Some(&0),
            "reused mid-walk: withheld"
        );
        assert!(!s.creation_filetimes.contains_key(&13));
        assert_eq!(s.creation_times.get(&14), Some(&0));
        assert!(!s.creation_filetimes.contains_key(&14));
        assert_eq!(
            s.exe_paths.get(&11).map(String::as_str),
            Some("C:\\x\\child.exe")
        );
        assert!(!s.exe_paths.contains_key(&12), "a blank exe path is absent");
        assert_eq!(
            filetime_to_unix_secs(unix_secs_to_filetime(1_715_911_000)),
            1_715_911_000
        );
    }

    #[test]
    fn process_table_join_argv_requotes_arguments_with_spaces() {
        assert_eq!(
            join_argv(&["C:\\Program Files\\node.exe", "cli.js", "--session-id", "u"]),
            "\"C:\\Program Files\\node.exe\" cli.js --session-id u"
        );
        assert_eq!(join_argv::<&str>(&[]), "");
    }

    /// The external scan's filter: node images whose command line names
    /// claude; a stale table is an error, never an empty answer.
    #[test]
    fn process_table_node_claude_filter_and_stale_refusal() {
        let mut snap = table(1);
        let node = if cfg!(windows) { "node.exe" } else { "node" };
        snap.names.insert(20, node.to_string());
        snap.names.insert(21, node.to_string());
        snap.names.insert(22, "claude.exe".to_string());
        let found = node_claude_processes_in(&snap, |pids| {
            let mut asked = pids.to_vec();
            asked.sort_unstable();
            assert_eq!(asked, vec![20, 21], "only node images are read");
            Ok(HashMap::from([
                (20, "node C:/x/claude-code/cli.js".to_string()),
                (21, "node server.js".to_string()),
            ]))
        })
        .expect("readable");
        assert_eq!(
            found,
            vec![(20, "node C:/x/claude-code/cli.js".to_string())]
        );

        let stale = ProcessSnapshot {
            stale: true,
            ..snap
        };
        assert!(node_claude_processes_in(&stale, |_| panic!("must not read")).is_err());
    }

    #[test]
    fn process_table_listener_decoder_reads_only_listening_rows_on_the_port() {
        use qontinui_runner_lib::peer_user_guard::decode::{listener_owner_pid, WinRow};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let at = |port| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let row = |port, state, pid| WinRow {
            local: at(port),
            remote: at(0),
            state,
            pid,
        };
        let rows = vec![
            row(8000, 5, 900), // ESTABLISHED on the port: not the owner
            row(8000, 2, 0),   // LISTEN with no pid: skipped
            row(8001, 2, 901), // another port
            row(8000, 2, 902), // the listener
        ];
        assert_eq!(listener_owner_pid(rows.clone(), 8000), Some(902));
        assert_eq!(listener_owner_pid(rows, 8002), None);
    }

    /// The real owner-MODULE read, through a socket this test process
    /// listens on: the owner is this process, and the socket's creation time
    /// lies between this process's own creation and now — the ordering the
    /// port kill's owner proof (`created <= bind_ft`) relies on.
    #[cfg(windows)]
    #[test]
    fn process_table_listener_read_names_this_process_and_a_socket_time_after_its_creation() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a listener");
        let port = listener.local_addr().expect("listener address").port();
        let me = std::process::id();
        let created = platform::precise_creation_filetime(me).expect("own creation time");
        let found = port_listener_in_process(port)
            .expect("a readable listener table")
            .expect("this test's own listener");
        let now = qontinui_runner_win32::holder_spawn::filetime_now();
        assert_eq!(found.pid, me);
        assert!(
            created <= found.bind_ft && found.bind_ft <= now,
            "creation {created} <= socket {} <= now {now}",
            found.bind_ft
        );
    }

    /// The owner-MODULE decoder selects the same row and keeps the socket's
    /// creation time; a negative timestamp reads as `0` (not reported), which
    /// the port kill refuses.
    #[test]
    fn process_table_module_listener_decoder_keeps_the_socket_creation_time() {
        use qontinui_runner_lib::peer_user_guard::decode::{listener_owner, ListenerOwner, WinRow};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let at = |port| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let row = |port, state, pid, created: i64| {
            (
                WinRow {
                    local: at(port),
                    remote: at(0),
                    state,
                    pid,
                },
                created,
            )
        };
        let rows = vec![
            row(8000, 5, 900, 11), // ESTABLISHED on the port: not the owner
            row(8000, 2, 0, 12),   // LISTEN with no pid: skipped
            row(8000, 2, 902, 134_378_302_137_147_198),
        ];
        assert_eq!(
            listener_owner(rows.clone(), 8000),
            Some(ListenerOwner {
                pid: 902,
                bind_ft: 134_378_302_137_147_198
            })
        );
        assert_eq!(listener_owner(rows, 8001), None);
        assert_eq!(
            listener_owner(vec![row(8000, 2, 903, -1)], 8000),
            Some(ListenerOwner {
                pid: 903,
                bind_ft: 0
            })
        );
    }

    /// The real walk lists this test process with its true parent and, on
    /// Windows, its precise creation FILETIME exactly as `GetProcessTimes`
    /// reports it through #2386's `OwnedProcess::creation_time`.
    #[test]
    fn process_table_live_walk_lists_this_process_with_its_true_parent() {
        let me = std::process::id();
        let snap = validate_walk(platform::walk().expect("walk"), me).expect("valid walk");
        let listed_parent = snap
            .parent_map
            .iter()
            .find(|(_, kids)| kids.contains(&me))
            .map(|(parent, _)| *parent)
            .expect("this process is listed under a parent");

        #[cfg(windows)]
        {
            let toolhelp_parent = qontinui_runner_win32::process_tree::snapshot()
                .expect("Toolhelp snapshot")
                .into_iter()
                .find(|(pid, _)| *pid == me)
                .map(|(_, ppid)| ppid)
                .expect("Toolhelp lists this process");
            assert_eq!(listed_parent, toolhelp_parent, "the true ppid");

            let kernel = qontinui_runner_win32::process_tree::OwnedProcess::open(me)
                .expect("open self")
                .creation_time()
                .expect("GetProcessTimes");
            assert_eq!(
                snap.creation_filetimes.get(&me),
                Some(&kernel),
                "the precise creation FILETIME"
            );
            let listed = *snap.creation_times.get(&me).expect("creation time");
            assert!(
                (filetime_to_unix_secs(kernel) - listed).abs() <= 5,
                "walk {listed} vs GetProcessTimes {kernel}"
            );
        }
        #[cfg(unix)]
        {
            assert_eq!(
                listed_parent,
                std::os::unix::process::parent_id(),
                "the true ppid"
            );
        }
    }

    /// `parse_session_id_from_cmdline` reads the same id from the in-process
    /// command line of a spawned child as from the argv it was given.
    #[test]
    fn process_table_session_id_parses_from_an_in_process_command_line() {
        use std::process::Stdio;
        let id = "1a2b3c4d-5e6f-7a8b-9c0d-1e2f3a4b5c6d";
        #[cfg(windows)]
        let mut cmd = {
            // `cmd /d /k` with stdin held open waits for input; the trailing
            // `rem` makes the session-id flags inert text it never runs.
            let mut c = crate::process_helpers::no_window("cmd.exe");
            c.args(["/d", "/k", "rem", "--session-id", id]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = crate::process_helpers::no_window("sh");
            c.args(["-c", "read _line", "sh", "--session-id", id]);
            c
        };
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");
        let pid = child.id();
        // The process may not have published its command line the instant
        // it is created; give it a bounded moment.
        let mut line = None;
        for _ in 0..50 {
            line = command_lines_blocking(&[pid]).expect("read").remove(&pid);
            if line.as_deref().is_some_and(|l| l.contains(id)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        let line = line.expect("the child's command line was read");
        assert_eq!(
            crate::process_capture::process_tree::parse_session_id_from_cmdline(&line),
            Some(id.to_string()),
            "from {line:?}"
        );
    }
}
