//! Panic supervisor for the runner's long-lived background loops.
//!
//! Plan `2026-09-03-coord-row-get-panic-class-closed-by-lint-and-supervisor`
//! Phase 4 (dossier `row-get-panic-kills-spawned-loop`) — the runner-side port
//! of coord's `worker_supervisor` (Phase 2, qontinui-coord PR #1889).
//!
//! # Why
//!
//! The runner's fleet heartbeat, budget re-publisher, tree publisher,
//! auto-fresh engine, auto-response fetch/scan loops, visibility sweeper and
//! plan-adapter reconcile loop are all `tokio::spawn`ed (or
//! `tauri::async_runtime::spawn`ed) `loop { tick.await; work().await }` tasks.
//! A panic inside one — a `tokio_postgres::Row::get` on a NULL, an `expect` on
//! a bad index, an `unwrap` on a poisoned lock — aborts the TASK, not the
//! process: nothing restarts it, nothing logs a second time, no health surface
//! reports it, and a dead loop is observably identical to a healthy idle one.
//! For the heartbeat that means coord's 120 s liveness TTL expires and the
//! device reads as offline until someone restarts a runner that fleet policy
//! says never to restart.
//!
//! [`spawn_supervised`] changes that. The worker future is built by a
//! **factory** (`Fn() -> Future`) so it can be rebuilt from scratch after a
//! panic; the supervisor task joins it, and on `JoinError::is_panic` logs the
//! payload at `error!`, records the panic in a process-local registry, sleeps a
//! bounded backoff (1 s doubling to a 60 s cap, reset after a run that lived
//! ≥ 5 min) and re-enters the factory. A future that RETURNS is recorded
//! `exited` and NOT restarted — a runner loop returns only on purpose.
//!
//! # Registry (design decision D2)
//!
//! The registry is a `OnceLock<Mutex<HashMap>>`: process-local, always
//! writable, and **nothing on the recording path awaits or touches PG or the
//! network**. `GET :9876/health` renders it as `supervised_workers` (rows +
//! counts) through [`health_block`], so the surface cannot share the failure
//! mode it observes.
//!
//! # Threading model
//!
//! The runner starts these loops from several runtimes on purpose — the
//! heartbeat and the budget re-publisher on a dedicated OS thread whose runtime
//! must never be starved, the publishers on a second one, and the terminal
//! loops on Tauri's global runtime. The supervisor does not change that:
//! [`spawn_supervised`] spawns on the CURRENT tokio runtime (the one the entry
//! point was called on), and [`spawn_supervised_on_tauri`] spawns the
//! supervisor on `tauri::async_runtime` exactly where the loop used to live.
//! The rebuilt worker future is `tokio::spawn`ed from inside the supervisor, so
//! it always lands on the same runtime as its supervisor.
//!
//! # Restart safety (design decision D3)
//!
//! Every supervised loop keeps its per-iteration state INSIDE the loop body
//! (tick intervals, failure streaks, the visibility sweeper's window set, the
//! plan adapter's edge-detection maps), and every "do not run on this
//! instance" gate sits BEFORE the spawn — a secondary instance returns before
//! anything is registered. So re-entering the factory is equivalent to the
//! process restart these loops already survive, never a double-run. The
//! migration classified every entry point started from `main.rs`'s two
//! fleet-thread blocks; the classification is pinned by
//! `every_phase4_entry_point_is_classified` below.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::task::JoinHandle;

/// First sleep after a panic.
pub const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// The backoff never exceeds this — a worker that panics on every entry
/// restarts at this cadence forever: loud (an `error!` per attempt, a climbing
/// counter on `/health`) rather than silent, and the cap bounds the cost.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A run that lived at least this long before panicking resets the backoff to
/// [`INITIAL_BACKOFF`]: it was a working loop that hit a bad row, not a loop
/// that cannot start.
pub const STABLE_RUN: Duration = Duration::from_secs(5 * 60);

/// Where a supervised worker is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    /// The worker future is live.
    Running,
    /// The worker panicked and the supervisor is sleeping its backoff before
    /// rebuilding the future.
    Restarting,
    /// The worker future returned `()`. Deliberate — never restarted.
    Exited,
    /// The worker task was cancelled (runtime shutdown or an explicit abort).
    /// Never restarted.
    Cancelled,
}

impl WorkerState {
    /// Stable lowercase token, the same one serde emits.
    pub fn as_str(self) -> &'static str {
        match self {
            WorkerState::Running => "running",
            WorkerState::Restarting => "restarting",
            WorkerState::Exited => "exited",
            WorkerState::Cancelled => "cancelled",
        }
    }
}

/// The registry's per-worker record. Private: readers get [`WorkerStatusRow`]
/// through [`snapshot`], which also derives the age fields at read time.
#[derive(Debug, Clone)]
struct WorkerStatus {
    state: WorkerState,
    /// Times the factory was re-invoked after a panic.
    restarts_total: u64,
    /// Times the worker future panicked. Leads `restarts_total` by exactly one
    /// while the supervisor sleeps its backoff.
    panics_total: u64,
    last_panic_at: Option<DateTime<Utc>>,
    last_panic_message: Option<String>,
    /// When the CURRENT (or last) future was started.
    started_at: Option<DateTime<Utc>>,
    exited_at: Option<DateTime<Utc>>,
    /// Last [`Heartbeat::tick`] — opt-in; `None` for a worker that never ticks.
    last_tick_at: Option<DateTime<Utc>>,
}

impl WorkerStatus {
    fn fresh() -> Self {
        Self {
            state: WorkerState::Running,
            restarts_total: 0,
            panics_total: 0,
            last_panic_at: None,
            last_panic_message: None,
            started_at: None,
            exited_at: None,
            last_tick_at: None,
        }
    }
}

/// One row of the registry as surfaced on `/health`. Timestamps are RFC 3339
/// strings (this crate's `chrono` carries no `serde` feature).
#[derive(Debug, Clone, Serialize)]
pub struct WorkerStatusRow {
    pub name: &'static str,
    pub state: WorkerState,
    pub restarts_total: u64,
    pub panics_total: u64,
    pub last_panic_at: Option<String>,
    pub last_panic_message: Option<String>,
    pub started_at: Option<String>,
    pub exited_at: Option<String>,
    pub last_tick_at: Option<String>,
    /// Seconds since `last_tick_at`, computed at snapshot time. `None` when the
    /// worker has never ticked (the heartbeat is opt-in).
    pub last_tick_age_secs: Option<f64>,
}

/// Counts over the whole registry, the summary half of the `/health` block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct WorkerCounts {
    pub running: usize,
    pub exited: usize,
    pub cancelled: usize,
    pub restarting: usize,
    /// Workers with `panics_total > 0`, whatever their current state.
    pub panicked_ever: usize,
}

static REGISTRY: OnceLock<Mutex<HashMap<&'static str, WorkerStatus>>> = OnceLock::new();

/// Run `f` against the registry under the lock. The guard never crosses an
/// `.await` (this function is synchronous, and so is every caller), and a
/// poisoned lock is recovered rather than propagated — a panic on some OTHER
/// thread while it held this lock must not take the observability surface down
/// with it.
fn with_registry<R>(f: impl FnOnce(&mut HashMap<&'static str, WorkerStatus>) -> R) -> R {
    let lock = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

fn rfc3339(t: Option<DateTime<Utc>>) -> Option<String> {
    t.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// Every registered worker, sorted by name so the surface is stable.
pub fn snapshot() -> Vec<WorkerStatusRow> {
    let now = Utc::now();
    let mut rows: Vec<WorkerStatusRow> = with_registry(|reg| {
        reg.iter()
            .map(|(name, s)| WorkerStatusRow {
                name,
                state: s.state,
                restarts_total: s.restarts_total,
                panics_total: s.panics_total,
                last_panic_at: rfc3339(s.last_panic_at),
                last_panic_message: s.last_panic_message.clone(),
                started_at: rfc3339(s.started_at),
                exited_at: rfc3339(s.exited_at),
                last_tick_at: rfc3339(s.last_tick_at),
                last_tick_age_secs: s
                    .last_tick_at
                    .map(|t| (now - t).to_std().map(|d| d.as_secs_f64()).unwrap_or(0.0)),
            })
            .collect()
    });
    rows.sort_by(|a, b| a.name.cmp(b.name));
    rows
}

/// The state / panic tallies over [`snapshot`].
pub fn counts_of(rows: &[WorkerStatusRow]) -> WorkerCounts {
    let mut c = WorkerCounts::default();
    for r in rows {
        match r.state {
            WorkerState::Running => c.running += 1,
            WorkerState::Restarting => c.restarting += 1,
            WorkerState::Exited => c.exited += 1,
            WorkerState::Cancelled => c.cancelled += 1,
        }
        if r.panics_total > 0 {
            c.panicked_ever += 1;
        }
    }
    c
}

/// The `supervised_workers` block on `GET :9876/health`: the rows plus their
/// counts. Pure over the in-process registry — no PG, no network, no await.
pub fn health_block() -> serde_json::Value {
    let rows = snapshot();
    let counts = counts_of(&rows);
    serde_json::json!({
        "counts": counts,
        "workers": rows,
        "backoff": {
            "initial_secs": INITIAL_BACKOFF.as_secs(),
            "max_secs": MAX_BACKOFF.as_secs(),
            "stable_run_secs": STABLE_RUN.as_secs(),
        },
        "note": "restarts_total > 0 means a worker loop PANICKED and was rebuilt \
                 by worker_supervisor; read last_panic_message. exited means the \
                 loop returned on purpose and was not restarted. A loop that is \
                 not listed here was never started on this instance (its \
                 instance/config gate returned before the spawn).",
    })
}

/// Opt-in liveness handle for a supervised worker's loop body: call
/// [`Heartbeat::tick`] once per iteration and `last_tick_at` /
/// `last_tick_age_secs` become meaningful on `/health`. Cheap (one mutex
/// take, no await), `Copy`, and a no-op for a name the supervisor does not
/// know — the registry stays "exactly the supervised workers".
#[derive(Debug, Clone, Copy)]
pub struct Heartbeat(pub &'static str);

impl Heartbeat {
    /// Record "the loop body reached this point" for the named worker.
    pub fn tick(&self) {
        let now = Utc::now();
        with_registry(|reg| {
            if let Some(s) = reg.get_mut(self.0) {
                s.last_tick_at = Some(now);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Backoff — pure, so the doubling and the cap are unit-testable without time.
// ---------------------------------------------------------------------------

/// The backoff to sleep after a panic, given the previous backoff and how
/// long the run that just panicked lived: a run that reached [`STABLE_RUN`]
/// starts over at [`INITIAL_BACKOFF`]; otherwise the previous value stands.
pub fn backoff_after_run(previous: Duration, lived: Duration) -> Duration {
    if lived >= STABLE_RUN {
        INITIAL_BACKOFF
    } else {
        previous
    }
}

/// The backoff after one more consecutive panic: doubled, capped at
/// [`MAX_BACKOFF`].
pub fn next_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_BACKOFF)
}

/// Render a panic payload the way `std`'s default hook does: the `&str` or
/// `String` a `panic!` carries, else a fixed marker for a non-string payload.
pub fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

// ---------------------------------------------------------------------------
// Registry transitions — each one synchronous, none awaits.
// ---------------------------------------------------------------------------

fn record_started(name: &'static str, restart: bool) {
    let now = Utc::now();
    with_registry(|reg| {
        let s = reg.entry(name).or_insert_with(WorkerStatus::fresh);
        s.state = WorkerState::Running;
        s.started_at = Some(now);
        s.exited_at = None;
        if restart {
            s.restarts_total += 1;
        }
    });
}

fn record_panic(name: &'static str, message: String) {
    let now = Utc::now();
    with_registry(|reg| {
        let s = reg.entry(name).or_insert_with(WorkerStatus::fresh);
        s.state = WorkerState::Restarting;
        s.panics_total += 1;
        s.last_panic_at = Some(now);
        s.last_panic_message = Some(message);
    });
}

fn record_terminal(name: &'static str, state: WorkerState) {
    let now = Utc::now();
    with_registry(|reg| {
        let s = reg.entry(name).or_insert_with(WorkerStatus::fresh);
        s.state = state;
        s.exited_at = Some(now);
    });
}

/// Aborts the wrapped task when dropped, so cancelling the SUPERVISOR (its
/// `JoinHandle::abort`, or runtime teardown) cancels the worker future it is
/// currently joining instead of detaching it as an unsupervised orphan.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Run `factory()` as a supervised task on the CURRENT tokio runtime,
/// restarting it after a panic.
///
/// `name` is the registry key — unique per process, `"<module>.<role>"`. The
/// returned handle is the SUPERVISOR's; aborting it aborts the current worker
/// future too.
///
/// Semantics, in the order the supervisor sees them:
/// - the future returns `()` → recorded `exited`, supervisor stops (a loop
///   returns only on purpose);
/// - the future panics → `error!` with the payload, `panics_total += 1`,
///   `last_panic_*` set, sleep the backoff, `restarts_total += 1`, rebuild via
///   `factory()`, loop;
/// - the future is cancelled → recorded `cancelled`, supervisor stops.
pub fn spawn_supervised<F, Fut>(name: &'static str, factory: F) -> JoinHandle<()>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(supervise(name, factory))
}

/// [`spawn_supervised`] for a body that wants to report liveness: the factory
/// receives the worker's [`Heartbeat`].
pub fn spawn_supervised_with_heartbeat<F, Fut>(name: &'static str, factory: F) -> JoinHandle<()>
where
    F: Fn(Heartbeat) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    spawn_supervised(name, move || factory(Heartbeat(name)))
}

/// [`spawn_supervised`] on Tauri's global async runtime — for the loops that
/// used to be `tauri::async_runtime::spawn`ed (the terminal scanners and
/// sweepers), so the migration keeps them on the runtime they always ran on.
/// The worker future is rebuilt on that same runtime.
pub fn spawn_supervised_on_tauri<F, Fut>(
    name: &'static str,
    factory: F,
) -> tauri::async_runtime::JoinHandle<()>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tauri::async_runtime::spawn(supervise(name, factory))
}

/// [`spawn_supervised_on_tauri`] with a [`Heartbeat`] handed to the factory.
pub fn spawn_supervised_on_tauri_with_heartbeat<F, Fut>(
    name: &'static str,
    factory: F,
) -> tauri::async_runtime::JoinHandle<()>
where
    F: Fn(Heartbeat) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    spawn_supervised_on_tauri(name, move || factory(Heartbeat(name)))
}

/// The supervisor loop itself. Public so a caller with its own runtime (a
/// dedicated `block_on` thread) can drive it directly; the `spawn_*` doors
/// above are the ordinary entry.
pub async fn supervise<F, Fut>(name: &'static str, factory: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut backoff = INITIAL_BACKOFF;
    let mut restart = false;
    loop {
        record_started(name, restart);
        let started = tokio::time::Instant::now();
        // Spawned (not awaited inline) so the panic is caught at the task
        // boundary as a `JoinError` — no `catch_unwind` / `UnwindSafe`
        // plumbing on the body. `tokio::spawn` here lands on whichever
        // runtime is polling the supervisor, so the worker stays beside it.
        let mut worker = AbortOnDrop(tokio::spawn(factory()));
        // `&mut JoinHandle` is itself a `Future` (`JoinHandle: Unpin`), so the
        // guard keeps ownership — and its abort-on-drop — for the whole join.
        let joined = (&mut worker.0).await;
        match joined {
            Ok(()) => {
                tracing::info!(
                    worker = name,
                    "supervised worker exited on its own — not restarting"
                );
                record_terminal(name, WorkerState::Exited);
                return;
            }
            Err(e) if e.is_panic() => {
                let payload = e.into_panic();
                let message = panic_payload_message(payload.as_ref());
                let lived = started.elapsed();
                backoff = backoff_after_run(backoff, lived);
                tracing::error!(
                    worker = name,
                    panic = %message,
                    lived_secs = lived.as_secs(),
                    backoff_secs = backoff.as_secs(),
                    "supervised worker PANICKED — restarting after backoff \
                     (dossier row-get-panic-kills-spawned-loop)"
                );
                record_panic(name, message);
                tokio::time::sleep(backoff).await;
                backoff = next_backoff(backoff);
                restart = true;
            }
            Err(_cancelled) => {
                tracing::warn!(
                    worker = name,
                    "supervised worker task was cancelled — not restarting"
                );
                record_terminal(name, WorkerState::Cancelled);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn row(name: &str) -> Option<WorkerStatusRow> {
        snapshot().into_iter().find(|r| r.name == name)
    }

    /// Poll the registry until `pred` holds. Under a paused clock the sleeps
    /// auto-advance, so this costs no wall time; the cap (20 000 s of paused
    /// time — the full ladder is 183 s, the reset test ~310 s) makes a wrong
    /// expectation fail instead of hang.
    async fn wait_for(name: &str, pred: impl Fn(&WorkerStatusRow) -> bool) -> WorkerStatusRow {
        for _ in 0..2_000_000 {
            if let Some(r) = row(name) {
                if pred(&r) {
                    return r;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "registry never reached the expected state for {name}: {:?}",
            row(name)
        );
    }

    // (a) a factory whose first future panics and whose second runs to a
    // signal is restarted exactly once; the registry carries the payload.
    #[tokio::test(start_paused = true)]
    async fn a_panicking_worker_is_restarted_once_and_the_payload_is_recorded() {
        const NAME: &str = "test.restart_once";
        let calls = Arc::new(AtomicUsize::new(0));
        let hold = Arc::new(tokio::sync::Notify::new());
        let supervisor = {
            let calls = calls.clone();
            let hold = hold.clone();
            spawn_supervised(NAME, move || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                let hold = hold.clone();
                async move {
                    if n == 0 {
                        panic!("boom: NULL into a non-Option column");
                    }
                    hold.notified().await;
                }
            })
        };

        let r = wait_for(NAME, |r| {
            r.restarts_total == 1 && r.state == WorkerState::Running
        })
        .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "factory must be invoked exactly twice"
        );
        assert_eq!(r.panics_total, 1);
        assert_eq!(
            r.last_panic_message.as_deref(),
            Some("boom: NULL into a non-Option column"),
            "the panic payload text must reach the registry"
        );
        assert!(r.last_panic_at.is_some());
        assert!(r.started_at.is_some());
        assert!(r.exited_at.is_none(), "a running worker has no exited_at");

        // Nothing else happens while the second run is alive.
        tokio::time::sleep(MAX_BACKOFF * 3).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a live worker is never rebuilt"
        );
        assert_eq!(row(NAME).unwrap().restarts_total, 1);

        supervisor.abort();
    }

    // (b) a normally-returning future is recorded Exited and NOT restarted.
    #[tokio::test(start_paused = true)]
    async fn a_worker_that_returns_is_recorded_exited_and_never_rebuilt() {
        const NAME: &str = "test.exits_on_purpose";
        let calls = Arc::new(AtomicUsize::new(0));
        let supervisor = {
            let calls = calls.clone();
            spawn_supervised(NAME, move || {
                calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    // "disabled via flag" — the loop returns on purpose.
                }
            })
        };
        // The supervisor itself returns once the worker exits.
        supervisor
            .await
            .expect("supervisor task must complete cleanly");

        let r = row(NAME).expect("registered");
        assert_eq!(r.state, WorkerState::Exited);
        assert!(r.exited_at.is_some());
        assert_eq!(r.restarts_total, 0);
        assert_eq!(r.panics_total, 0);

        // Well past the initial backoff: still exactly one factory call.
        tokio::time::sleep(INITIAL_BACKOFF * 10).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "an exited worker is not restarted"
        );
        assert_eq!(row(NAME).unwrap().state, WorkerState::Exited);
    }

    // (c) backoff doubles and caps — the pure half.
    #[test]
    fn backoff_doubles_to_the_cap_and_resets_after_a_stable_run() {
        let mut b = INITIAL_BACKOFF;
        let mut seen = Vec::new();
        for _ in 0..8 {
            seen.push(b.as_secs());
            b = next_backoff(b);
        }
        assert_eq!(seen, vec![1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(next_backoff(MAX_BACKOFF), MAX_BACKOFF, "the cap is sticky");

        // A run that lived ≥ 5 min resets; a shorter one keeps the ladder.
        assert_eq!(
            backoff_after_run(Duration::from_secs(32), STABLE_RUN),
            INITIAL_BACKOFF
        );
        assert_eq!(
            backoff_after_run(Duration::from_secs(32), STABLE_RUN + Duration::from_secs(1)),
            INITIAL_BACKOFF
        );
        assert_eq!(
            backoff_after_run(Duration::from_secs(32), STABLE_RUN - Duration::from_secs(1)),
            Duration::from_secs(32)
        );
    }

    // (c) backoff doubles and caps — measured on the paused clock through the
    // real supervisor loop: a worker that panics on every entry is rebuilt at
    // 1, 2, 4, 8, 16, 32, 60, 60 s gaps.
    #[tokio::test(start_paused = true)]
    async fn a_worker_that_always_panics_is_rebuilt_on_the_doubling_ladder() {
        const NAME: &str = "test.always_panics";
        let starts: Arc<Mutex<Vec<tokio::time::Instant>>> = Arc::new(Mutex::new(Vec::new()));
        let supervisor = {
            let starts = starts.clone();
            spawn_supervised(NAME, move || {
                starts.lock().unwrap().push(tokio::time::Instant::now());
                async move {
                    panic!("boom every time");
                }
            })
        };
        wait_for(NAME, |r| r.restarts_total >= 8).await;
        supervisor.abort();

        let starts = starts.lock().unwrap().clone();
        let gaps: Vec<u64> = starts
            .windows(2)
            .map(|w| (w[1] - w[0]).as_secs())
            .take(8)
            .collect();
        assert_eq!(
            gaps,
            vec![1, 2, 4, 8, 16, 32, 60, 60],
            "restart gaps: {gaps:?}"
        );
        let r = row(NAME).unwrap();
        assert!(r.panics_total >= 8, "{r:?}");
        assert_eq!(r.last_panic_message.as_deref(), Some("boom every time"));
    }

    // The reset arm, measured: after two immediate panics the ladder is at 4 s;
    // a run that then lives STABLE_RUN before panicking is rebuilt after 1 s.
    #[tokio::test(start_paused = true)]
    async fn a_stable_run_resets_the_ladder_to_one_second() {
        const NAME: &str = "test.stable_run_resets";
        let starts: Arc<Mutex<Vec<tokio::time::Instant>>> = Arc::new(Mutex::new(Vec::new()));
        let hold = Arc::new(tokio::sync::Notify::new());
        let supervisor = {
            let starts = starts.clone();
            let hold = hold.clone();
            spawn_supervised(NAME, move || {
                let n = {
                    let mut s = starts.lock().unwrap();
                    s.push(tokio::time::Instant::now());
                    s.len()
                };
                let hold = hold.clone();
                async move {
                    match n {
                        1 | 2 => panic!("immediate"),
                        3 => {
                            tokio::time::sleep(STABLE_RUN).await;
                            panic!("late");
                        }
                        _ => hold.notified().await,
                    }
                }
            })
        };
        wait_for(NAME, |r| {
            r.restarts_total == 3 && r.state == WorkerState::Running
        })
        .await;
        supervisor.abort();

        let starts = starts.lock().unwrap().clone();
        let gaps: Vec<u64> = starts.windows(2).map(|w| (w[1] - w[0]).as_secs()).collect();
        // run1 → 1 s → run2 → 2 s → run3 (lives 300 s) → 1 s (reset) → run4
        assert_eq!(gaps, vec![1, 2, STABLE_RUN.as_secs() + 1], "gaps: {gaps:?}");
    }

    // The `/health` block: rows + the five counts, the backoff constants, and
    // a registered worker shows up in it with every row field present.
    #[tokio::test(start_paused = true)]
    async fn health_block_carries_rows_and_counts() {
        const NAME: &str = "test.health_block_row";
        let hold = Arc::new(tokio::sync::Notify::new());
        let supervisor = {
            let hold = hold.clone();
            spawn_supervised(NAME, move || {
                let hold = hold.clone();
                async move { hold.notified().await }
            })
        };
        wait_for(NAME, |r| r.state == WorkerState::Running).await;

        let v = health_block();
        for key in [
            "running",
            "exited",
            "cancelled",
            "restarting",
            "panicked_ever",
        ] {
            assert!(v["counts"][key].is_u64(), "counts.{key} missing: {v}");
        }
        assert!(v["counts"]["running"].as_u64().unwrap() >= 1);
        assert_eq!(v["backoff"]["initial_secs"], 1);
        assert_eq!(v["backoff"]["max_secs"], 60);
        assert_eq!(v["backoff"]["stable_run_secs"], 300);
        let rows = v["workers"].as_array().expect("workers is an array");
        let mine = rows
            .iter()
            .find(|r| r["name"] == NAME)
            .expect("the registered worker is a row");
        assert_eq!(mine["state"], "running");
        assert_eq!(mine["restarts_total"], 0);
        assert!(mine["last_tick_at"].is_null(), "no heartbeat was ticked");
        assert!(
            mine["started_at"].as_str().is_some(),
            "started_at is an RFC 3339 string: {mine}"
        );
        for key in [
            "name",
            "state",
            "restarts_total",
            "panics_total",
            "last_panic_at",
            "last_panic_message",
            "started_at",
            "exited_at",
            "last_tick_at",
            "last_tick_age_secs",
        ] {
            assert!(mine.get(key).is_some(), "row field {key} missing: {mine}");
        }
        supervisor.abort();
    }

    // The heartbeat is opt-in and never creates a row for an unknown name.
    #[tokio::test(start_paused = true)]
    async fn heartbeat_ticks_a_registered_worker_and_ignores_an_unknown_name() {
        const NAME: &str = "test.heartbeat";
        let hold = Arc::new(tokio::sync::Notify::new());
        let supervisor = {
            let hold = hold.clone();
            spawn_supervised_with_heartbeat(NAME, move |hb| {
                let hold = hold.clone();
                async move {
                    hb.tick();
                    hold.notified().await
                }
            })
        };
        let r = wait_for(NAME, |r| r.last_tick_at.is_some()).await;
        assert!(r.last_tick_age_secs.is_some());

        Heartbeat("test.never_registered").tick();
        assert!(
            row("test.never_registered").is_none(),
            "tick must not register"
        );
        supervisor.abort();
    }

    // Aborting the supervisor aborts the worker it is joining — no orphan.
    #[tokio::test(start_paused = true)]
    async fn aborting_the_supervisor_aborts_the_worker() {
        const NAME: &str = "test.abort_propagates";
        let dropped = Arc::new(AtomicUsize::new(0));
        struct OnDrop(Arc<AtomicUsize>);
        impl Drop for OnDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let supervisor = {
            let dropped = dropped.clone();
            spawn_supervised(NAME, move || {
                let guard = OnDrop(dropped.clone());
                async move {
                    let _guard = guard;
                    std::future::pending::<()>().await
                }
            })
        };
        wait_for(NAME, |r| r.state == WorkerState::Running).await;
        supervisor.abort();
        for _ in 0..1_000 {
            if dropped.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            1,
            "the worker future must be dropped"
        );
    }

    // The Tauri-runtime door: the supervisor runs on `tauri::async_runtime`
    // (a separate, real-time runtime, so this test uses wall time — the
    // ladder's first rung is 1 s) and restarts a panicking worker there too.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_tauri_door_supervises_on_tauris_runtime() {
        const NAME: &str = "test.tauri_door";
        let calls = Arc::new(AtomicUsize::new(0));
        let hold = Arc::new(tokio::sync::Notify::new());
        let handle = {
            let calls = calls.clone();
            let hold = hold.clone();
            spawn_supervised_on_tauri_with_heartbeat(NAME, move |hb| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                let hold = hold.clone();
                async move {
                    if n == 0 {
                        panic!("boom on tauri");
                    }
                    hb.tick();
                    hold.notified().await;
                }
            })
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut seen = None;
        while std::time::Instant::now() < deadline {
            if let Some(r) = row(NAME) {
                if r.restarts_total == 1 && r.last_tick_at.is_some() {
                    seen = Some(r);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let r =
            seen.unwrap_or_else(|| panic!("tauri-door worker never restarted: {:?}", row(NAME)));
        assert_eq!(r.state, WorkerState::Running);
        assert_eq!(r.last_panic_message.as_deref(), Some("boom on tauri"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        handle.abort();
    }

    // The payload renderer covers the three payload shapes.
    #[test]
    fn panic_payload_message_renders_str_string_and_other() {
        let s: Box<dyn std::any::Any + Send> = Box::new("static str");
        assert_eq!(panic_payload_message(s.as_ref()), "static str");
        let s: Box<dyn std::any::Any + Send> = Box::new(String::from("owned"));
        assert_eq!(panic_payload_message(s.as_ref()), "owned");
        let s: Box<dyn std::any::Any + Send> = Box::new(42u8);
        assert_eq!(
            panic_payload_message(s.as_ref()),
            "<non-string panic payload>"
        );
    }

    /// Slice a source file from the line that starts with `signature` to the
    /// first `}` at column 0 after it — the whole entry-point fn — with comment
    /// lines dropped so a mention in prose cannot satisfy or trip a needle.
    fn fn_region(src_root: &std::path::Path, file: &str, signature: &str) -> String {
        let path = src_root.join(file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let lines: Vec<&str> = text.lines().collect();
        let start = lines
            .iter()
            .position(|l| l.starts_with(signature))
            .unwrap_or_else(|| panic!("{file}: no line starts with `{signature}`"));
        let end = lines[start..]
            .iter()
            .position(|l| *l == "}")
            .map(|i| start + i)
            .unwrap_or_else(|| panic!("{file}: `{signature}` never closes at column 0"));
        lines[start..=end]
            .iter()
            .filter(|l| !l.trim_start().starts_with("//"))
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    }

    // Source scan (plan Phase 4 step 2 / Phase 2 step 4(f)): every long-lived
    // loop started from `main.rs`'s two fleet-thread blocks is routed through
    // the supervisor and holds no bare spawn; the deliberately unsupervised
    // calls in the same blocks are pinned with their reasons; and main.rs
    // actually starts every one of them, so the scan is not vacuous.
    #[test]
    fn every_phase4_entry_point_is_classified() {
        // (file, entry-point signature prefix, registry name). Each region
        // must contain `spawn_supervised` and no bare `tokio::spawn(` /
        // `tauri::async_runtime::spawn(`.
        const SUPERVISED: &[(&str, &str, &str)] = &[
            ("fleet.rs", "pub fn spawn_heartbeat()", "fleet.heartbeat"),
            (
                "fleet.rs",
                "pub fn spawn_budget_republisher(",
                "fleet.budget_republisher",
            ),
            (
                "fleet.rs",
                "pub fn spawn_tree_publisher()",
                "fleet.tree_publisher",
            ),
            (
                "fleet.rs",
                "pub fn spawn_auto_fresh_engine()",
                "fleet.auto_fresh_engine",
            ),
            (
                "terminal/auto_response_fleet.rs",
                "pub fn spawn_fetch_loop()",
                "terminal.auto_response_fleet.fetch_loop",
            ),
            (
                "terminal/auto_response.rs",
                "pub fn spawn_grid_scan_loop()",
                "terminal.auto_response.grid_scan",
            ),
            (
                "terminal/usage_limit.rs",
                "pub fn spawn_grid_scan_loop()",
                "terminal.usage_limit.grid_scan",
            ),
            (
                "terminal/visibility.rs",
                "pub fn spawn_sweeper()",
                "terminal.visibility.sweeper",
            ),
            (
                "plan_workunit_adapter/trigger.rs",
                "pub fn spawn_if_configured(",
                "plan_workunit_adapter.reconcile_loop",
            ),
        ];
        // Calls interleaved with the SUPERVISED set above (main.rs's two
        // fleet-thread blocks, `spawn_heartbeat` through
        // `spawn_auto_fresh_engine`, plus the plan adapter) that the
        // supervisor deliberately does NOT wrap: (main.rs call needle,
        // reason). None of them is a spawned task — a restart-on-panic
        // wrapper would buy nothing.
        const EXCLUDED: &[(&str, &str)] = &[
            (
                "fleet::publish_on_startup(fleet::MachineRole::Agent).await",
                "one-shot boot publish, awaited inline on the publishers runtime — \
                 not a spawned task; `spawn_budget_republisher` re-asserts it periodically",
            ),
            (
                "terminal::auto_response_fleet::reload_from_cache_at_boot()",
                "synchronous one-shot cache load at boot — no task to supervise",
            ),
            (
                "qontinui_runner_lib::env_agent::publish_pg_pool(",
                "synchronous one-shot publication of the PG pool handle — no task",
            ),
        ];
        // The REST of the `fleet-publishers` block (main.rs :1350-:1525, past
        // the plan's Phase 4 set) also starts long-lived tasks. They are NOT
        // supervised by this PR and are listed here so the classification is
        // complete rather than silently partial: each needs its own D3
        // re-entry read (several own arming flags, on-disk cursors or
        // subscriber connections whose restart semantics are not obvious from
        // the entry point), which is follow-up work, not Phase 4's. Pinned so
        // that adding or removing one forces a conscious re-classification.
        const DEFERRED: &[(&str, &str)] = &[
            (
                "qontinui_runner_lib::env_agent::spawn_env_capture();",
                "dev-environment capture task — enrollment gate lives inside the loop; \
                 restart safety not audited in Phase 4",
            ),
            (
                "qontinui_runner_lib::env_agent::directive::spawn_enroll_directive_subscriber();",
                "coord WS subscriber — a rebuild reconnects the socket; needs its own \
                 reconnect/backoff read before it is wrapped",
            ),
            (
                "session_attribution::spawn_session_attribution();",
                "forward-only transcript reader — holds per-session file cursors; a \
                 rebuild's re-seed semantics are not audited",
            ),
            (
                "agent_worktree::census::spawn_census();",
                "worktree census poller — out of Phase 4's declared set",
            ),
            (
                "agent_worktree::reclaim::spawn_reclaim();",
                "reclaim executor — DESTRUCTIVE actions behind arming flags; a restart \
                 must be reasoned about before it is automatic",
            ),
            (
                "credential_helper::spawn_startup_sweep();",
                "startup sweep — out of Phase 4's declared set",
            ),
            (
                "agent_worktree::maintenance_executor::spawn_maintenance();",
                "maintenance executor — mutating; same reasoning as reclaim",
            ),
            (
                "agent_worktree::orphan_target_reaper::spawn_orphan_reaper();",
                "orphan reaper — mutating; same reasoning as reclaim",
            ),
            (
                "agent_worktree::census::spawn_volume_publisher();",
                "volume publisher — out of Phase 4's declared set",
            ),
            (
                "agent_worktree::disk_survey::spawn_disk_surveyor();",
                "disk surveyor — out of Phase 4's declared set",
            ),
            (
                "agent_worktree::fs_backstop::spawn_backstop();",
                "filesystem backstop — out of Phase 4's declared set",
            ),
            (
                "crate::mcp::probe_executor::spawn_probe_executor();",
                "probe executor — out of Phase 4's declared set",
            ),
            (
                "agent_runtime::spawn_runtime();",
                "agent runtime — owns spawned agent sessions; a restart's effect on \
                 in-flight sessions is not audited",
            ),
            (
                "ci_node::spawn_ci_node_runtime();",
                "CI node runtime — owns in-flight builds; same reasoning as agent_runtime",
            ),
        ];

        let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let main_src = std::fs::read_to_string(src_root.join("main.rs")).expect("main.rs");

        let mut violations: Vec<String> = Vec::new();
        for (file, signature, name) in SUPERVISED {
            let region = fn_region(&src_root, file, signature);
            let bare =
                region.contains("tokio::spawn(") || region.contains("tauri::async_runtime::spawn(");
            let wrapped = region.contains("spawn_supervised");
            if bare || !wrapped {
                violations.push(format!(
                    "{file} `{signature}`: must go through worker_supervisor::spawn_supervised* \
                     and hold no bare spawn — bare={bare} wrapped={wrapped}"
                ));
            }
            if !region.contains(&format!("\"{name}\"")) {
                violations.push(format!(
                    "{file} `{signature}`: registry name \"{name}\" not found in the entry point"
                ));
            }
            // Non-vacuity: main.rs starts this entry point.
            let fn_name = signature
                .trim_start_matches("pub fn ")
                .split('(')
                .next()
                .unwrap();
            if !main_src.contains(&format!("::{fn_name}(")) {
                violations.push(format!(
                    "main.rs no longer calls `{fn_name}(` — re-classify {file}"
                ));
            }
        }
        for (needle, reason) in EXCLUDED {
            if !main_src.contains(needle) {
                violations.push(format!(
                    "main.rs no longer contains the excluded call `{needle}` ({reason}) — \
                     update EXCLUDED"
                ));
            }
        }
        for (needle, reason) in DEFERRED {
            if !main_src.contains(needle) {
                violations.push(format!(
                    "main.rs no longer contains the deferred entry point `{needle}` ({reason}) \
                     — update DEFERRED (and supervise it if it is still started elsewhere)"
                ));
            }
        }
        assert!(
            violations.is_empty(),
            "Phase 4 classification drifted:\n{}",
            violations.join("\n")
        );
        assert!(
            SUPERVISED.len() >= 6,
            "the supervised set must cover the plan's six named loops at least"
        );
        // Registry names are unique — two loops on one key would fold their
        // panics into one row.
        let mut names: Vec<&str> = SUPERVISED.iter().map(|(_, _, n)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SUPERVISED.len(), "duplicate registry name");
    }
}
