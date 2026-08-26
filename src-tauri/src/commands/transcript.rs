//! Tauri commands for Claude Code transcript import and workflow generation.
//!
//! Provides commands to list, read, and extract Claude Code session transcripts,
//! plus a standalone workflow generation command that doesn't require an existing task_run_id.

use crate::commands::compartments::{ExecutionCompartment, HealthCompartment, StorageCompartment};
use crate::commands::CommandResponse;
use crate::terminal::transcript;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

/// Collect workspace project paths for scanning (root + immediate child directories).
fn collect_workspace_project_paths() -> Vec<String> {
    let workspace_root = crate::mcp::shared::get_workspace_paths_internal()
        .map(|(root, _, _)| root.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut paths = vec![workspace_root.clone()];
    if !workspace_root.is_empty() {
        if let Ok(entries) = std::fs::read_dir(&workspace_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if !name.starts_with('.') && name != "node_modules" {
                        paths.push(path.to_string_lossy().to_string());
                    }
                }
            }
        }
    }
    paths
}

/// Collect all transcript sessions across all project paths and config dirs, deduplicated.
fn collect_all_sessions(
    project_paths: &[String],
    config_dirs: &[std::path::PathBuf],
) -> Vec<transcript::TranscriptSession> {
    let mut all_sessions = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for project in project_paths {
        for dir in config_dirs {
            match transcript::list_sessions(dir, project) {
                Ok(sessions) => {
                    for session in sessions {
                        if seen_ids.insert(session.session_id.clone()) {
                            all_sessions.push(session);
                        }
                    }
                }
                Err(e) => warn!("Failed to list sessions in {:?}: {}", dir, e),
            }
        }
    }

    all_sessions.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    all_sessions
}

// ── Scan cache (last-known-good + off-runtime refresh) ───────────────────────
//
// **Why this exists.** Every terminal tab is its own `WebviewWindow`
// (`commands::terminal_windows`), so each one loads a full app instance with
// its own `useTranscriptSessions` hook and its own 30s poll. N tabs therefore
// meant N *independent* full-workspace scans, and because the tabs mount
// together their timers stay in phase — they fire as one burst.
//
// Measured on the fleet box 2026-08-07 with 6 tabs open (Terminal, tio, gmail,
// hotmail, paktis, qontinui):
//
// ```text
// 10:16:01.383 … .394   6 scans start  (11ms spread)
// 10:16:01.808 … .816   all 6 finish   (~433ms burst)
// ```
//
// Each scan crosses 624 workspace child directories with 6 Claude config dirs
// — ~3,744 `list_sessions` calls — so the burst was ~22,000 filesystem probes
// for six byte-identical answers.
//
// ## Iteration 14's fix, and why it was INERT (rewritten 2026-08-24)
//
// Iteration 14 removed the `tokio::sync::Mutex` held across
// `spawn_blocking(...).await` — correctly — and bounded both remaining waits
// with `tokio::time::timeout`. Iteration 15 reproduced the same wedge **3/3**
// on that build, and neither the 20s leader give-up WARN nor the follower
// give-up WARN appeared in any of the three logs.
//
// The reason is structural: `tokio::time::timeout` is driven by the runtime's
// time driver, and the time driver only advances when a worker runs the
// scheduler loop. Once every worker is blocked in synchronous code the wheel
// stops turning and the timeout is disabled by *exactly* the condition it
// exists to guard. See `crate::off_runtime` for the full argument.
//
// Ironically the module's own test already had the right pattern — it measured
// the runtime's liveness with a std channel *from outside the runtime* — while
// the production path stayed on the tokio timer.
//
// ## The shape now
//
// Two changes, and the second is the one that keeps `/health` answering:
//
//   1. **The bound is a `std::sync::mpsc::Receiver::recv_timeout` on a
//      dedicated OS thread.** The scan dispatcher hands the job to a private
//      worker thread and waits on it with `recv_timeout(SCAN_WAIT_TIMEOUT)`.
//      The OS schedules that thread whether or not tokio is healthy, so the
//      give-up decision — the WARN, the in-flight release, the degraded
//      broadcast — is made off the runtime and lands even if no tokio worker
//      ever runs again. A second, independent bound (`off_runtime::deadline`)
//      guards the async caller's wait for the same reason.
//
//   2. **The periodic scan is off the request path.** `transcript_list_sessions`
//      serves the **last-known-good** entry at any age and, if it is stale,
//      *kicks* a background refresh through a bounded queue without waiting
//      for it. Only a caller with nothing at all to serve (cold start, or a
//      brand-new cache key) waits — and only when the scan thread is free.
//      A stale answer is strictly better than a parked runtime.
//
// The scan worker is a **private OS thread**, not `spawn_blocking`: the
// blocking pool is shared with `tokio::fs` and every other blocking call in
// the process, so a pathological filesystem walk must not be able to consume
// it. This is the same pattern `terminal::commit_report::PushDispatcher` uses
// for the git fan-out, which iteration 14 got right.

/// How long a completed scan counts as *fresh*. Below this the entry is served
/// with no refresh at all. Above it the entry is still served — it is the
/// last-known-good — but a background refresh is kicked.
const SCAN_CACHE_TTL: Duration = Duration::from_secs(5);

struct ScanCache {
    computed_at: Instant,
    /// Inputs the entry was computed for. A caller asking about different
    /// paths (e.g. `all_projects: false`) must not be served this entry.
    project_paths: Vec<String>,
    config_dirs: Vec<std::path::PathBuf>,
    sessions: Vec<transcript::TranscriptSession>,
}

/// Identity of one scan: the exact inputs it answers for. Single flight is
/// per-key — a caller with different inputs must never be served, or wait for,
/// another key's scan.
type ScanKey = (Vec<String>, Vec<std::path::PathBuf>);

/// Broadcast payload — shared so N followers clone once, not N times.
type ScanResult = Arc<Vec<transcript::TranscriptSession>>;

/// Everything the cache coordinates, behind ONE synchronous mutex.
#[derive(Default)]
struct ScanState {
    cache: Option<ScanCache>,
    /// Scans currently running or queued, by key. Present means someone is
    /// already doing this exact work and will broadcast the result.
    in_flight: HashMap<ScanKey, broadcast::Sender<ScanResult>>,
}

/// Process-wide scan state.
///
/// `std::sync::Mutex`, NOT `tokio::sync::Mutex`: every critical section here is
/// synchronous, and the `!Send` guard makes "held across an await" a compile
/// error rather than a latent wedge (see the module note above).
fn scan_state() -> &'static StdMutex<ScanState> {
    static STATE: OnceLock<StdMutex<ScanState>> = OnceLock::new();
    STATE.get_or_init(|| StdMutex::new(ScanState::default()))
}

/// Upper bound on how long a scan may run before it is abandoned, and on how
/// long any caller will wait for one. Two orders of magnitude above the
/// measured 433ms burst, so it only ever fires on a genuine stall.
///
/// **Enforced off the tokio runtime** — `recv_timeout` on the dispatcher
/// thread, and `off_runtime::deadline` for the async caller. Never
/// `tokio::time::timeout`: that is the inert shape iteration 15 reproduced 3/3.
const SCAN_WAIT_TIMEOUT: Duration = Duration::from_secs(20);

/// Upper bound on the per-mount digest batch (`transcript_session_digests`).
/// Higher than the scan bound because it reads up to 100 file tails, and it is
/// still an *upper* bound, not a latency target.
const DIGEST_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Refresh requests that may sit queued. One is enough: a second pending
/// refresh answers the same question as the first, and a deeper queue only
/// delays the WARN that says the scan is stuck.
const SCAN_QUEUE_CAPACITY: usize = 1;

/// Count of scans actually dispatched (cache misses + stale refreshes). Lets a
/// test prove the collapse happened rather than inferring it from timing.
static SCANS_PERFORMED: AtomicU64 = AtomicU64::new(0);

/// Count of scans abandoned by the `recv_timeout` bound. Non-zero means the
/// filesystem walk stopped completing — the wedge precursor, now visible.
static SCANS_GAVE_UP: AtomicU64 = AtomicU64::new(0);

/// Count of refresh requests refused because the queue was full or the
/// dispatcher is gone. The caller was served last-known-good instead.
static SCANS_DROPPED: AtomicU64 = AtomicU64::new(0);

/// True while the scan worker is executing a scan. Read (never waited on) by
/// cold callers so they degrade immediately instead of queueing behind an
/// unrelated key's scan.
static SCAN_BUSY: AtomicBool = AtomicBool::new(false);

/// Number of real filesystem scans dispatched since process start.
pub fn scans_performed() -> u64 {
    SCANS_PERFORMED.load(Ordering::Relaxed)
}

/// Number of scans abandoned by the off-runtime bound since process start.
pub fn scans_gave_up() -> u64 {
    SCANS_GAVE_UP.load(Ordering::Relaxed)
}

/// Number of refresh requests dropped because the queue was full.
pub fn scans_dropped() -> u64 {
    SCANS_DROPPED.load(Ordering::Relaxed)
}

/// True when a cache entry is FRESH enough to serve with no refresh at all.
/// Pure — unit-tested without a clock or a filesystem.
fn entry_is_servable(
    age: Duration,
    entry_paths: &[String],
    entry_dirs: &[std::path::PathBuf],
    want_paths: &[String],
    want_dirs: &[std::path::PathBuf],
) -> bool {
    age < SCAN_CACHE_TTL && entry_paths == want_paths && entry_dirs == want_dirs
}

// ── The off-runtime scan dispatcher ──────────────────────────────────────────

type ScanFn = Box<dyn FnOnce() -> Vec<transcript::TranscriptSession> + Send + 'static>;

struct ScanRequest {
    key: ScanKey,
    scan: ScanFn,
}

/// Publish the outcome of one scan: release the in-flight slot, update the
/// cache on success, and hand every waiter something to return.
///
/// Called from the **dispatcher thread**, never from an async task. That is
/// the point: the degradation lands even when the runtime cannot run anything.
fn finish_scan(key: &ScanKey, sessions: Option<Vec<transcript::TranscriptSession>>) {
    let (waiters, payload) = {
        let mut st = scan_state().lock().unwrap_or_else(|e| e.into_inner());
        let waiters = st.in_flight.remove(key);
        let payload: ScanResult = match sessions {
            Some(s) => {
                st.cache = Some(ScanCache {
                    computed_at: Instant::now(),
                    project_paths: key.0.clone(),
                    config_dirs: key.1.clone(),
                    sessions: s.clone(),
                });
                Arc::new(s)
            }
            None => {
                // Degraded. Hand waiters the last-known-good for THIS key if
                // one exists, else empty. The entry keeps its original
                // timestamp — a failed refresh must never look like a fresh
                // answer.
                let lkg = st
                    .cache
                    .as_ref()
                    .filter(|e| e.project_paths == key.0 && e.config_dirs == key.1)
                    .map(|e| e.sessions.clone())
                    .unwrap_or_default();
                Arc::new(lkg)
            }
        };
        (waiters, payload)
    };
    // A send error just means "no subscribers".
    if let Some(tx) = waiters {
        let _ = tx.send(payload);
    }
}

/// The dispatcher loop. Blocking by design; runs on its own OS thread.
///
/// `reqs` — refresh requests from the command path (bounded, never blocks the
/// caller). `job_tx`/`res_rx` — the private worker thread that actually walks
/// the filesystem.
fn scan_dispatch_loop(
    reqs: std::sync::mpsc::Receiver<ScanRequest>,
    job_tx: std::sync::mpsc::Sender<ScanFn>,
    res_rx: std::sync::mpsc::Receiver<Vec<transcript::TranscriptSession>>,
) {
    scan_dispatch_loop_with_bound(reqs, job_tx, res_rx, SCAN_WAIT_TIMEOUT)
}

/// The dispatcher loop with the bound injected, so a test can exercise the
/// REAL loop without waiting out the production 20s.
fn scan_dispatch_loop_with_bound(
    reqs: std::sync::mpsc::Receiver<ScanRequest>,
    job_tx: std::sync::mpsc::Sender<ScanFn>,
    res_rx: std::sync::mpsc::Receiver<Vec<transcript::TranscriptSession>>,
    bound: Duration,
) {
    use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
    // True while the worker still owes a result we already gave up on.
    let mut abandoned = false;

    while let Ok(req) = reqs.recv() {
        if abandoned {
            // Reclaim the worker if the stuck scan finally finished. Its
            // result answers a stale key, so it is discarded rather than
            // cached under this request's key.
            match res_rx.try_recv() {
                Ok(_late) => abandoned = false,
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    finish_scan(&req.key, None);
                    break;
                }
            }
        }

        if abandoned {
            SCANS_DROPPED.fetch_add(1, Ordering::Relaxed);
            // Release FIRST, log second: `warn!` goes through the tracing
            // subscriber, which is a shared-fate path in exactly the condition
            // being reported.
            finish_scan(&req.key, None);
            warn!(
                "transcript_list_sessions: the scan thread is still stuck on an earlier scan — \
                 skipping this refresh and serving last-known-good"
            );
            continue;
        }

        SCAN_BUSY.store(true, Ordering::SeqCst);
        if job_tx.send(req.scan).is_err() {
            SCAN_BUSY.store(false, Ordering::SeqCst);
            finish_scan(&req.key, None);
            break;
        }

        // ── THE BOUND ──
        // A std `recv_timeout` on a non-runtime thread. It fires whether or
        // not a tokio worker is available, which `tokio::time::timeout` cannot
        // promise — that is the entire fix.
        let outcome = res_rx.recv_timeout(bound);
        SCAN_BUSY.store(false, Ordering::SeqCst);

        match outcome {
            Ok(sessions) => finish_scan(&req.key, Some(sessions)),
            Err(RecvTimeoutError::Timeout) => {
                abandoned = true;
                SCANS_GAVE_UP.fetch_add(1, Ordering::Relaxed);
                finish_scan(&req.key, None);
                warn!(
                    "transcript_list_sessions: the filesystem scan has not returned within {}s — \
                     giving up on this refresh and serving last-known-good. This bound is a std \
                     recv_timeout on a dedicated OS thread, so it fires even when every tokio \
                     worker is parked.",
                    bound.as_secs()
                );
            }
            Err(RecvTimeoutError::Disconnected) => {
                finish_scan(&req.key, None);
                break;
            }
        }
    }
}

/// Bounded, single-worker dispatcher for the blocking transcript scan.
struct ScanDispatcher {
    tx: std::sync::mpsc::SyncSender<ScanRequest>,
}

impl ScanDispatcher {
    fn new(capacity: usize) -> Self {
        let (tx, reqs) = std::sync::mpsc::sync_channel::<ScanRequest>(capacity);
        let (job_tx, job_rx) = std::sync::mpsc::channel::<ScanFn>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<Vec<transcript::TranscriptSession>>();

        // The worker: walks the filesystem, nothing else. Private to this
        // module so a pathological walk cannot consume the shared blocking
        // pool.
        if let Err(e) = std::thread::Builder::new()
            .name("transcript-scan".to_string())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let out = job();
                    if res_tx.send(out).is_err() {
                        break;
                    }
                }
            })
        {
            warn!("transcript_list_sessions: could not start the scan worker thread ({e})");
        }

        // The bound-holder. Separate from the worker so it can still act while
        // the worker is stuck.
        if let Err(e) = std::thread::Builder::new()
            .name("transcript-scan-bound".to_string())
            .spawn(move || scan_dispatch_loop(reqs, job_tx, res_rx))
        {
            warn!("transcript_list_sessions: could not start the scan dispatcher thread ({e})");
        }

        Self { tx }
    }

    /// Enqueue a refresh. NEVER blocks: a full queue (or a dead dispatcher)
    /// refuses the job and returns `false`, and the caller serves
    /// last-known-good.
    fn try_dispatch(&self, key: ScanKey, scan: ScanFn) -> bool {
        match self.tx.try_send(ScanRequest { key, scan }) {
            Ok(()) => true,
            Err(_) => {
                SCANS_DROPPED.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
}

fn scan_dispatcher() -> &'static ScanDispatcher {
    static DISPATCHER: OnceLock<ScanDispatcher> = OnceLock::new();
    DISPATCHER.get_or_init(|| ScanDispatcher::new(SCAN_QUEUE_CAPACITY))
}

/// What this caller's turn at the cache decided.
enum ScanTurn {
    /// An entry answered immediately — fresh, or stale-but-usable while
    /// someone else refreshes.
    Serve(Vec<transcript::TranscriptSession>),
    /// A stale entry answered immediately AND we own the background refresh.
    /// **This is the steady-state path: it never waits.**
    ServeAndRefresh(Vec<transcript::TranscriptSession>),
    /// Nothing to serve and we own the refresh. `wait` is false when the scan
    /// thread is already busy — then we degrade now rather than queue behind
    /// an unrelated key.
    Refresh {
        rx: broadcast::Receiver<ScanResult>,
        wait: bool,
    },
    /// Nothing to serve and someone else owns this key's scan.
    Follow(broadcast::Receiver<ScanResult>),
}

/// Await a scan's broadcast under an **off-runtime** bound.
///
/// The dispatcher already guarantees a terminal event within
/// `SCAN_WAIT_TIMEOUT`; this second bound is independent insurance for the
/// case where the dispatcher itself is lost. Deliberately NOT
/// `tokio::time::timeout` — see `crate::off_runtime`.
async fn await_scan(mut rx: broadcast::Receiver<ScanResult>) -> Vec<transcript::TranscriptSession> {
    let bound = crate::off_runtime::deadline(SCAN_WAIT_TIMEOUT);
    tokio::select! {
        r = rx.recv() => match r {
            Ok(sessions) => (*sessions).clone(),
            Err(e) => {
                // The owner went away without publishing. Its slot is already
                // cleared, so the next poll elects a new one.
                warn!("transcript_list_sessions: scan owner vanished ({e}) — reporting empty");
                Vec::new()
            }
        },
        _ = bound => {
            warn!(
                "transcript_list_sessions: waited {}s for a first scan and gave up — reporting \
                 empty rather than holding the command surface open. This bound is measured on a \
                 dedicated OS thread, so it fires even when every tokio worker is parked.",
                SCAN_WAIT_TIMEOUT.as_secs()
            );
            Vec::new()
        }
    }
}

/// [`collect_all_sessions`] behind the last-known-good cache, with the
/// blocking filesystem work moved onto a private OS thread.
async fn collect_all_sessions_cached(
    project_paths: &[String],
    config_dirs: &[std::path::PathBuf],
) -> Vec<transcript::TranscriptSession> {
    let paths = project_paths.to_vec();
    let dirs = config_dirs.to_vec();
    collect_all_sessions_cached_with(project_paths, config_dirs, move || {
        collect_all_sessions(&paths, &dirs)
    })
    .await
}

/// The cache/single-flight machinery with the scan itself injected.
///
/// Split out purely so the concurrency contract is testable without touching a
/// real Claude config tree: the tests supply a scan that blocks on a channel
/// and then assert that callers still make progress.
async fn collect_all_sessions_cached_with<F>(
    project_paths: &[String],
    config_dirs: &[std::path::PathBuf],
    scan: F,
) -> Vec<transcript::TranscriptSession>
where
    F: FnOnce() -> Vec<transcript::TranscriptSession> + Send + 'static,
{
    let key: ScanKey = (project_paths.to_vec(), config_dirs.to_vec());

    // Synchronous critical section. No `.await` may appear inside it; the
    // `!Send` std guard enforces that at compile time.
    let turn = {
        let mut st = scan_state().lock().unwrap_or_else(|e| e.into_inner());
        let entry = st
            .cache
            .as_ref()
            .filter(|e| e.project_paths == *project_paths && e.config_dirs == *config_dirs);
        let fresh = entry
            .map(|e| {
                entry_is_servable(
                    e.computed_at.elapsed(),
                    &e.project_paths,
                    &e.config_dirs,
                    project_paths,
                    config_dirs,
                )
            })
            .unwrap_or(false);
        let last_known_good = entry.map(|e| e.sessions.clone());
        // Subscribe eagerly rather than holding a borrow of `st.in_flight`
        // into the arm that inserts into it: a `Receiver` is owned, so the
        // borrow ends here. (Borrowing across the insert is NLL problem case
        // #3 and does not compile.)
        let already_running = st.in_flight.get(&key).map(|tx| tx.subscribe());

        match (fresh, last_known_good) {
            (true, Some(sessions)) => ScanTurn::Serve(sessions),
            (_, lkg) => {
                if let Some(rx) = already_running {
                    match lkg {
                        // Someone is already refreshing. Serve the stale entry
                        // NOW — never wait on a live scan when an answer
                        // exists.
                        Some(sessions) => ScanTurn::Serve(sessions),
                        None => ScanTurn::Follow(rx),
                    }
                } else {
                    // Capacity 1: exactly one send per flight.
                    let (tx, rx) = broadcast::channel(1);
                    st.in_flight.insert(key.clone(), tx);
                    SCANS_PERFORMED.fetch_add(1, Ordering::Relaxed);
                    match lkg {
                        Some(sessions) => ScanTurn::ServeAndRefresh(sessions),
                        None => ScanTurn::Refresh {
                            rx,
                            wait: !SCAN_BUSY.load(Ordering::SeqCst),
                        },
                    }
                }
            }
        }
    };

    match turn {
        ScanTurn::Serve(sessions) => {
            debug!(
                "transcript_list_sessions: served {} sessions from the scan cache",
                sessions.len()
            );
            sessions
        }

        ScanTurn::ServeAndRefresh(sessions) => {
            // Item 2: the periodic scan leaves the request path here. We hand
            // the work to the private scan thread and return the
            // last-known-good immediately — no await, and nothing on this
            // request that a stuck filesystem can hold.
            if !scan_dispatcher().try_dispatch(key.clone(), Box::new(scan)) {
                finish_scan(&key, None);
                warn!(
                    "transcript_list_sessions: the refresh queue is full — serving \
                     last-known-good ({} sessions) without refreshing",
                    sessions.len()
                );
            }
            sessions
        }

        ScanTurn::Refresh { rx, wait } => {
            if !scan_dispatcher().try_dispatch(key.clone(), Box::new(scan)) {
                finish_scan(&key, None);
                warn!(
                    "transcript_list_sessions: the refresh queue is full and nothing is cached \
                     for these inputs — reporting empty"
                );
                return Vec::new();
            }
            if !wait {
                // Cold, but the scan thread is occupied by another key. Do not
                // queue behind it — an empty answer now beats an open request.
                debug!(
                    "transcript_list_sessions: scan thread busy — refresh queued, reporting empty \
                     for this poll"
                );
                return Vec::new();
            }
            await_scan(rx).await
        }

        ScanTurn::Follow(rx) => await_scan(rx).await,
    }
}

/// List Claude Code transcript sessions.
///
/// When `all_projects` is true, scans the workspace root **and** all immediate
/// child directories (the individual repos in the monorepo) so sessions started
/// from subdirectories are visible too.
#[tauri::command]
pub async fn transcript_list_sessions(
    project_path: Option<String>,
    all_projects: Option<bool>,
) -> Result<CommandResponse, String> {
    let project_paths: Vec<String> = if all_projects.unwrap_or(true) {
        collect_workspace_project_paths()
    } else {
        let project = project_path.unwrap_or_else(|| {
            crate::mcp::shared::get_workspace_paths_internal()
                .map(|(root, _, _)| root.to_string_lossy().to_string())
                .unwrap_or_default()
        });
        if project.is_empty() {
            return Ok(CommandResponse {
                success: false,
                message: Some("No project path available".to_string()),
                data: None,
            });
        }
        vec![project]
    };

    let config_dirs = transcript::find_claude_config_dirs();
    info!(
        "transcript_list_sessions: scanning {} project paths across {} config dirs",
        project_paths.len(),
        config_dirs.len()
    );

    // Note: don't early-return on empty config_dirs — Phase 5.1 fakes still
    // need to flow through even when the host has no Claude installations.
    let all_sessions = if config_dirs.is_empty() {
        Vec::new()
    } else {
        collect_all_sessions_cached(&project_paths, &config_dirs).await
    };

    // Phase 5.1 of the UI Bridge discoverability/effectiveness plan:
    // append any fakes injected via the debug-only
    // `/ui-bridge/test/inject-session` route so SessionCard can render
    // without a live PTY. The merge is a no-op (and the module + accessor
    // don't exist) on production release builds without `test-fixtures`.
    #[cfg(any(debug_assertions, feature = "test-fixtures"))]
    let all_sessions = crate::mcp::test_fixtures::merge_with_injected(
        all_sessions,
        crate::mcp::test_fixtures::injected_test_sessions(),
    );

    info!(
        "transcript_list_sessions: found {} total sessions",
        all_sessions.len()
    );

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} sessions", all_sessions.len())),
        data: Some(serde_json::to_value(&all_sessions).unwrap_or_default()),
    })
}

/// Read all messages from a specific Claude Code transcript session.
#[tauri::command]
pub async fn transcript_read_session(
    session_id: String,
    config_dir: Option<String>,
    project_path: Option<String>,
) -> Result<CommandResponse, String> {
    let project = project_path.unwrap_or_else(|| {
        crate::mcp::shared::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    // If config_dir provided, use it directly; otherwise scan all config dirs
    let config_dirs = if let Some(dir) = config_dir {
        vec![std::path::PathBuf::from(dir)]
    } else {
        transcript::find_claude_config_dirs()
    };

    for dir in &config_dirs {
        match transcript::read_session(dir, &project, &session_id) {
            Ok(messages) => {
                return Ok(CommandResponse {
                    success: true,
                    message: Some(format!("Read {} messages", messages.len())),
                    data: Some(serde_json::to_value(&messages).unwrap_or_default()),
                });
            }
            Err(_) => continue, // Try next config dir
        }
    }

    Ok(CommandResponse {
        success: false,
        message: Some(format!(
            "Session '{}' not found in any config directory",
            session_id
        )),
        data: None,
    })
}

/// Read ONLY the operator's own prompts from a session transcript.
///
/// Backs the per-zone prompts panel, which polls while open. Two properties
/// that `transcript_read_session` does not have and that a poll needs:
/// machine-authored `user` records (`isMeta` slash-command expansions,
/// `isCompactSummary` continuations, `isSidechain` subagent turns) are dropped
/// server-side where the flags still exist, and `sinceMtimeMs` skips the parse
/// entirely while the file is untouched. See `transcript::read_user_prompts`
/// for the measurements behind both.
#[tauri::command]
pub async fn transcript_read_user_prompts(
    session_id: String,
    config_dir: Option<String>,
    project_path: Option<String>,
    since_mtime_ms: Option<u64>,
) -> Result<CommandResponse, String> {
    let project = project_path.unwrap_or_else(|| {
        crate::mcp::shared::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    let config_dirs = if let Some(dir) = config_dir {
        vec![std::path::PathBuf::from(dir)]
    } else {
        transcript::find_claude_config_dirs()
    };

    // Reading and parsing a transcript is blocking file I/O — on the largest
    // real transcript here, 10.4 MB. Keep it off the async worker.
    let result = tokio::task::spawn_blocking(move || {
        for dir in &config_dirs {
            if let Ok(r) =
                transcript::read_user_prompts(dir, &project, &session_id, since_mtime_ms)
            {
                return Some(r);
            }
        }
        None
    })
    .await
    .map_err(|e| format!("prompt read task failed: {e}"))?;

    match result {
        Some(r) => Ok(CommandResponse {
            success: true,
            message: Some(if r.unchanged {
                "Transcript unchanged".to_string()
            } else {
                format!("Read {} prompts", r.prompts.len())
            }),
            data: Some(serde_json::to_value(&r).unwrap_or_default()),
        }),
        None => Ok(CommandResponse {
            success: false,
            message: Some("Session transcript not found in any config directory".to_string()),
            data: None,
        }),
    }
}

/// Get the most recent Claude Code session for the current project.
///
/// `config_dir` filters to a specific Claude config directory (e.g.
/// `C:\claude\.claude-hotmail`). When omitted, all known config dirs are
/// scanned and the first hit wins. The filter is required for callers
/// like `useTabSessionIdCapture` that launched a tab into a specific
/// account — without it, a more-recently-touched JSONL in a *different*
/// account for the same `project_path` would shadow the session we
/// actually want.
///
/// `since_ms` (epoch milliseconds, matches `Date.now()` on the JS side as
/// camelCase `sinceMs`) filters out sessions whose `last_modified` is at
/// or before the supplied threshold. This lets the frontend lift its
/// freshness check into the backend so the `.claude.json` shortcut and
/// mtime fallback are filtered consistently — see Phase 1 of
/// `plans/traffic-light-session-id-followups.md`.
#[tauri::command]
pub async fn transcript_get_latest(
    project_path: Option<String>,
    config_dir: Option<String>,
    since_ms: Option<i64>,
) -> Result<CommandResponse, String> {
    let project = project_path.unwrap_or_else(|| {
        crate::mcp::shared::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    // If config_dir was supplied, scope to it; otherwise scan all known dirs.
    let config_dirs: Vec<std::path::PathBuf> = if let Some(dir) = config_dir {
        vec![std::path::PathBuf::from(dir)]
    } else {
        transcript::find_claude_config_dirs()
    };

    // Convert the epoch-millis threshold to a UTC timestamp for the backend
    // helper. `from_timestamp_millis` returns `None` for out-of-range
    // values; in that case we behave as if no filter was supplied.
    let since = since_ms.and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis);

    // Try each config dir, return first match
    for dir in &config_dirs {
        if let Some(session) = transcript::get_latest_session_id(dir, &project, since) {
            return Ok(CommandResponse {
                success: true,
                message: Some(format!("Latest session: {}", session.session_id)),
                data: Some(serde_json::to_value(&session).unwrap_or_default()),
            });
        }
    }

    Ok(CommandResponse {
        success: true,
        message: Some("No sessions found".to_string()),
        data: None,
    })
}

/// Compute lightweight digests for recent sessions (frozen detection + work summary hints).
///
/// Takes the most recent N sessions (default 50) and returns a digest for each,
/// reading only the tail of each JSONL file for efficiency.
#[tauri::command]
pub async fn transcript_session_digests(
    max_sessions: Option<usize>,
) -> Result<CommandResponse, String> {
    let project_paths = collect_workspace_project_paths();
    let config_dirs = transcript::find_claude_config_dirs();
    if config_dirs.is_empty() {
        return Ok(CommandResponse {
            success: true,
            message: Some("No config directories found".to_string()),
            data: Some(serde_json::json!([])),
        });
    }

    // Shares the single-flight scan cache with `transcript_list_sessions`:
    // this runs once per tab ON MOUNT, so six tabs previously meant six more
    // full scans stacked on top of the poll bursts during startup — the
    // busiest moment the runner has.
    let mut all_sessions = collect_all_sessions_cached(&project_paths, &config_dirs).await;
    let limit = max_sessions.unwrap_or(50).min(100);
    all_sessions.truncate(limit);

    // Digest computation reads the TAIL OF EVERY session file (up to `limit`),
    // so it is blocking I/O and must not run on an async worker.
    //
    // Bounded for the same reason the scan is (Item 1's audit): this runs once
    // per tab ON MOUNT and touches up to 100 files, so it is a real
    // blocking-pool consumer on the same request-path family. An exhausted pool
    // means the `spawn_blocking` future never resolves, and the bound has to be
    // one the sick runtime cannot disable — `off_runtime::deadline`, never
    // `tokio::time::timeout`.
    let digest_task =
        tokio::task::spawn_blocking(move || transcript::session_digests_batch(&all_sessions));
    let digest_bound = crate::off_runtime::deadline(DIGEST_WAIT_TIMEOUT);
    let digests = tokio::select! {
        joined = digest_task => match joined {
            Ok(d) => d,
            Err(e) => {
                warn!("transcript_session_digests: digest task failed: {e}");
                Vec::new()
            }
        },
        _ = digest_bound => {
            warn!(
                "transcript_session_digests: digests did not compute within {}s (blocking pool \
                 exhausted?) — returning none rather than holding the command surface open. This \
                 bound is measured on a dedicated OS thread.",
                DIGEST_WAIT_TIMEOUT.as_secs()
            );
            Vec::new()
        }
    };

    info!(
        "transcript_session_digests: computed {} digests",
        digests.len()
    );

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Computed {} session digests", digests.len())),
        data: Some(serde_json::to_value(&digests).unwrap_or_default()),
    })
}

/// Detect Claude Code processes running outside this Runner instance.
///
/// Returns a list of PIDs and optional working directories for Claude processes
/// that are NOT managed by this Runner's PTY or session system.
#[tauri::command]
pub async fn transcript_find_external_processes(
    execution: tauri::State<'_, ExecutionCompartment>,
) -> Result<CommandResponse, String> {
    // Collect PIDs managed by this runner
    let managed_pids = execution
        .ai_pid_tracker()
        .lock()
        .map_err(|e| format!("Failed to lock ai_pid_tracker: {e}"))?
        .clone();

    let external = transcript::find_external_claude_processes(&managed_pids);

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Found {} external Claude processes",
            external.len()
        )),
        data: Some(serde_json::to_value(&external).unwrap_or_default()),
    })
}

/// Generate a workflow from arbitrary text context (no task_run_id required).
///
/// This is the standalone generation entrypoint for terminal text selections
/// and transcript imports. Calls the same pipeline as `generate_workflow_from_session`
/// but doesn't need an existing AI session.
#[tauri::command]
pub async fn generate_workflow_standalone(
    storage: tauri::State<'_, StorageCompartment>,
    health: tauri::State<'_, HealthCompartment>,
    description: String,
    inline_context: String,
    include_ui_bridge: Option<bool>,
) -> Result<CommandResponse, String> {
    info!(
        "generate_workflow_standalone: desc_len={}, context_len={}",
        description.len(),
        inline_context.len()
    );

    if inline_context.trim().is_empty() {
        return Ok(CommandResponse {
            success: false,
            message: Some("No context provided for workflow generation".to_string()),
            data: None,
        });
    }

    // Detect brief-mode: the Specs-page AI path prefixes its inline_context
    // with "Spec Generation Brief (JSON)". Those calls want the brief fed to
    // the Builder verbatim — the transcript-style spec-aware rewrap
    // (`build_spec_aware_context` is a "generate NEW spec files from a plan"
    // envelope) would compete with the brief's "consume EXISTING spec into
    // snapshot_assert steps" instructions and the Builder produces an empty
    // workflow because the two voices contradict. Brief mode also skips the
    // specification-synthesis agent for the same reason.
    let is_brief_mode = inline_context.contains("Spec Generation Brief (JSON)");

    let enriched_context = if is_brief_mode {
        // Pass through unchanged — the brief carries its own instructions and
        // meta_workflow.rs::build_builder_prompt will append the matching
        // "Spec Generation Brief Recognition" rules section.
        inline_context.clone()
    } else {
        // `fetch_existing_specs` is a pre-compartment helper that takes
        // `&AppState`. Use the explicit `app_state()` escape hatch on
        // health to keep the legacy passthrough greppable.
        let existing_specs =
            crate::commands::ai_session::fetch_existing_specs(health.app_state()).await;
        if existing_specs != "No existing specs found" {
            crate::commands::ai_session::build_spec_aware_context(&inline_context, &existing_specs)
        } else {
            format!(
                "The following is a Claude Code conversation transcript or selected text. \
                 Use this context to generate an appropriate workflow:\n\n{}",
                inline_context
            )
        }
    };

    // Build generation request
    let (request_category, request_tags, generate_spec_flag) = if is_brief_mode {
        (
            Some("spec-generated".to_string()),
            Some(vec!["spec".to_string(), "auto-generated".to_string()]),
            Some(false),
        )
    } else {
        (None, None, Some(true))
    };

    let request = crate::workflow_generation::GenerateWorkflowRequest {
        // Stream C: transcript-replay workflows target the runner app by default.
        app_id: crate::spec_api::storage::RUNNER_APP_ID.to_string(),
        description,
        inline_context: Some(enriched_context),
        category: request_category,
        tags: request_tags,
        max_iterations: None,
        provider: None,
        model: None,
        skip_ai_summary: None,
        log_source_selection: None,
        prompt_template: None,
        auto_include_contexts: Some(true),
        context_ids: None,
        max_fix_iterations: Some(3),
        discovery_mode: None,
        include_ui_bridge_instructions: include_ui_bridge,
        reflection_mode: None,
        investigate_codebase: Some(true),
        include_design_guidance: None,
        auto_run: None,
        model_overrides: None,
        generate_specification: generate_spec_flag,
        verification_depth: None,
        discover_ui_bridge_specs: None,
        simple_mode: None,
        pipeline_depth: None,
        tool_tags: None,
        exploration_settings: None,
        target_runner_port: None,
    };

    // Get doctor handle for health monitoring
    let doctor_handle = health.doctor_handle().lock().await.clone();
    let pg_db = storage.pg_db().clone();
    let pg_clone = pg_db.clone();
    // Clone the AppState Arc so the builder's brief-mode port resolution
    // can use the actually-bound runner port (matters for temp runners on
    // 9877+). Without this the synchronous path falls back to
    // get_mcp_api_port() which reads $QONTINUI_PORT / defaults to 9876.
    // `generate_workflow` is a pre-compartment helper; escape-hatch via
    // health.app_state() keeps the legacy passthrough greppable.
    let app_state_for_gen = health.app_state().clone();

    // catch_unwind safety net: converts any latent panic inside the generation
    // pipeline (e.g. a future PG deserialization drift) into a clean structured
    // error response instead of a bare JoinError::Panic bubbling to the frontend.
    let gen_result = tokio::task::spawn_blocking(move || {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        let panic_result = catch_unwind(AssertUnwindSafe(|| {
            crate::workflow_generation::generate_workflow(
                request,
                doctor_handle.as_ref(),
                Some(&pg_clone),
                None,
                Some(&*app_state_for_gen),
            )
        }));
        match panic_result {
            Ok(pair) => pair,
            Err(payload) => {
                let reason = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "<non-string panic payload>".to_string());
                tracing::error!("generate_workflow panicked: {}", reason);
                let response = crate::workflow_generation::GenerateWorkflowResponse {
                    workflow: None,
                    validation_errors: Vec::new(),
                    success: false,
                    error: Some(format!("Workflow generation panicked: {}", reason)),
                    model_used: None,
                    verification_iterations: Vec::new(),
                    hardening_summary: None,
                    discovery_calls: Vec::new(),
                    acceptance_criteria: None,
                    quality_report: None,
                    confidence_score: None,
                    workflow_evaluation: None,
                    exploration_stats: None,
                };
                // Empty artifact placeholder so the save path can skip cleanly.
                let artifact =
                    crate::workflow_generation::pipeline_artifacts::PipelineArtifactBuilder::new(
                        "", None,
                    )
                    .build(0);
                (response, artifact)
            }
        }
    })
    .await;

    // Save pipeline artifact to PG (async, outside spawn_blocking)
    if let Ok((_, ref artifact)) = gen_result {
        if let Err(e) = pg_db.save_generation_artifact(artifact).await {
            tracing::warn!("Failed to save pipeline artifact to PG: {}", e);
        }
    }
    let gen_result = gen_result.map(|(response, _)| response);

    match gen_result {
        Ok(mut response) => {
            // Save workflow to database if generation succeeded
            if response.success {
                if let Some(ref workflow) = response.workflow {
                    let create_req = crate::unified_workflows::CreateUnifiedWorkflowRequest {
                        name: workflow.name.clone(),
                        description: workflow.description.clone(),
                        category: workflow.category.clone(),
                        tags: workflow.tags.clone(),
                        setup_steps: workflow.setup_steps.clone(),
                        verification_steps: workflow.verification_steps.clone(),
                        agentic_steps: workflow.agentic_steps.clone(),
                        completion_steps: workflow.completion_steps.clone(),
                        max_iterations: workflow.max_iterations,
                        timeout_seconds: workflow.timeout_seconds,
                        provider: workflow.provider.clone(),
                        model: workflow.model.clone(),
                        skip_ai_summary: false,
                        log_source_selection: None,
                        context_ids: None,
                        disabled_context_ids: None,
                        auto_include_contexts: Some(workflow.auto_include_contexts),
                        prompt_template: workflow.prompt_template.clone(),
                        log_watch_enabled: Some(workflow.log_watch_enabled),
                        health_check_enabled: Some(workflow.health_check_enabled),
                        health_check_urls: if workflow.health_check_urls.is_empty() {
                            None
                        } else {
                            Some(workflow.health_check_urls.clone())
                        },
                        preflight_check_enabled: Some(workflow.preflight_check_enabled),
                        enable_sweep: Some(workflow.enable_sweep),
                        max_sweep_iterations: Some(workflow.max_sweep_iterations),
                        targeted_error_ids: None,
                        generated_by_task_run_id: None,
                        stages: if workflow.stages.is_empty() {
                            None
                        } else {
                            Some(workflow.stages.clone())
                        },
                        stop_on_failure: Some(workflow.stop_on_failure),
                        constraint_overrides: Some(workflow.constraint_overrides.clone()),
                        approval_gate: Some(workflow.approval_gate),
                        reflection_mode: Some(workflow.reflection_mode),
                        completion_prompts_first: Some(workflow.completion_prompts_first),
                        model_overrides: Some(workflow.model_overrides.clone()),
                        dependency_graph: workflow.dependency_graph.clone(),
                        cost_annotations: workflow.cost_annotations.clone(),
                        quality_report: workflow.quality_report.clone(),
                        acceptance_criteria: workflow.acceptance_criteria.clone(),
                        ai_reviewed: Some(workflow.ai_reviewed),
                        workflow_architecture: workflow.workflow_architecture.clone(),
                        enforce_token_budget: Some(workflow.enforce_token_budget),
                        strict_cwd: workflow.strict_cwd,
                        tool_tags: workflow.tool_tags.clone(),
                        flow_control_json: None,
                        phase_timeouts_json: None,
                        rollback_policy: workflow.rollback_policy.clone(),
                        htn_enabled: workflow.htn_enabled,
                        htn_ui_bridge_url: workflow.htn_ui_bridge_url.clone(),
                        htn_state_machine_path: workflow.htn_state_machine_path.clone(),
                    };

                    match storage.pg_db().create_unified_workflow(&create_req).await {
                        Ok(saved) => {
                            info!(
                                "Saved standalone generated workflow '{}' (id={})",
                                saved.name, saved.id
                            );
                            // Overwrite the pre-save generator UUID with the persisted
                            // DB id so callers can navigate to the actual row without
                            // re-POSTing and creating a duplicate.
                            if let Some(ref mut wf) = response.workflow {
                                wf.id = saved.id.clone();
                            }
                        }
                        Err(e) => {
                            warn!("Failed to save standalone generated workflow: {}", e);
                        }
                    }
                }
            }

            Ok(CommandResponse {
                success: response.success,
                message: response.error.clone().or_else(|| {
                    response
                        .workflow
                        .as_ref()
                        .map(|w| format!("Generated workflow: {}", w.name))
                }),
                data: Some(serde_json::to_value(&response).unwrap_or_default()),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Generation task failed: {}", e)),
            data: None,
        }),
    }
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_transcript")
        .invoke_handler(tauri::generate_handler![
            transcript_list_sessions,
            transcript_read_session,
            transcript_read_user_prompts,
            transcript_get_latest,
            transcript_session_digests,
            transcript_find_external_processes,
            generate_workflow_standalone,
        ])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(names: &[&str]) -> Vec<std::path::PathBuf> {
        names.iter().map(std::path::PathBuf::from).collect()
    }

    /// The scan cache, the counters AND the process-wide scan dispatcher are
    /// global, and the harness runs tests in parallel threads — so any two
    /// cache tests running at once corrupt each other's counters and evict
    /// each other's entry. Every test that touches the cache takes this first.
    static CACHE_TESTS_ARE_SERIAL: StdMutex<()> = StdMutex::new(());

    fn cache_test_guard() -> std::sync::MutexGuard<'static, ()> {
        CACHE_TESTS_ARE_SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Drop any entry left by a previous test so this one starts from a real
    /// miss, and wait for the shared scan thread to go idle so `SCAN_BUSY`
    /// reflects THIS test rather than the previous one's trailing job.
    fn reset_scan_cache() {
        // The dispatcher is process-global and its queue holds one job, so
        // "not busy right now" can be the gap *between* two jobs. Require the
        // idle observation to hold for a beat before trusting it.
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut idle_since: Option<Instant> = None;
        while Instant::now() < deadline {
            if SCAN_BUSY.load(Ordering::SeqCst) {
                idle_since = None;
            } else {
                let since = *idle_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(150) {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut st = scan_state().lock().unwrap_or_else(|e| e.into_inner());
        st.cache = None;
        st.in_flight.clear();
    }

    fn fake_session(id: &str) -> transcript::TranscriptSession {
        transcript::TranscriptSession {
            session_id: id.to_string(),
            project_path: String::new(),
            config_dir: String::new(),
            message_count: 1,
            last_modified: String::new(),
            started_at: None,
            first_message_preview: None,
            has_plans: false,
            display_name: String::new(),
            injected_live_status: None,
            injected_tab: None,
            resume_name: None,
        }
    }

    #[test]
    fn fresh_entry_with_matching_inputs_is_servable() {
        let p = vec!["a".to_string()];
        let d = dirs(&["cfg"]);
        assert!(entry_is_servable(Duration::from_millis(1), &p, &d, &p, &d));
    }

    #[test]
    fn expired_entry_is_not_servable() {
        let p = vec!["a".to_string()];
        let d = dirs(&["cfg"]);
        assert!(!entry_is_servable(
            SCAN_CACHE_TTL + Duration::from_millis(1),
            &p,
            &d,
            &p,
            &d
        ));
    }

    /// The cache key must include BOTH inputs. Serving an `all_projects:false`
    /// caller the workspace-wide entry (or vice-versa) would silently return
    /// the wrong session set.
    #[test]
    fn differing_inputs_are_not_servable() {
        let d = dirs(&["cfg"]);
        assert!(
            !entry_is_servable(
                Duration::ZERO,
                &["a".to_string()],
                &d,
                &["b".to_string()],
                &d
            ),
            "different project paths must not share an entry"
        );
        assert!(
            !entry_is_servable(
                Duration::ZERO,
                &["a".to_string()],
                &dirs(&["cfg1"]),
                &["a".to_string()],
                &dirs(&["cfg2"])
            ),
            "different config dirs must not share an entry"
        );
    }

    // ── Item 1: the bound must fire OFF the tokio runtime ────────────────
    //
    // Iteration 14's bound was `tokio::time::timeout`, which needs the
    // runtime's time driver. Iteration 15 reproduced the wedge 3/3 on that
    // build with neither give-up WARN in any log. These tests pin the
    // replacement.

    /// **The load-bearing test for Item 1.** A scan that never returns must be
    /// abandoned by the bound, the in-flight slot released and the give-up
    /// counter incremented — with **no tokio runtime in the process at all**.
    ///
    /// That is the strongest available statement of "this bound does not
    /// depend on the runtime it protects": there is no runtime to depend on.
    /// Everything here is measured on the test thread with std channels, so on
    /// the regressed shape the test FAILS rather than hangs.
    ///
    /// Neuter check: put `res_rx.recv()` (or a `tokio::time::timeout`) back in
    /// place of `res_rx.recv_timeout(..)` in `scan_dispatch_loop` and this
    /// test fails on the `slot never released` assertion.
    #[test]
    fn a_stuck_scan_is_abandoned_by_the_bound_with_no_runtime_at_all() {
        let _serial = cache_test_guard();

        // A private dispatcher so the test can use a short bound without
        // touching the 20s production constant or the shared worker.
        let (req_tx, reqs) = std::sync::mpsc::sync_channel::<ScanRequest>(1);
        let (job_tx, job_rx) = std::sync::mpsc::channel::<ScanFn>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<Vec<transcript::TranscriptSession>>();
        std::thread::spawn(move || {
            while let Ok(job) = job_rx.recv() {
                let out = job();
                if res_tx.send(out).is_err() {
                    break;
                }
            }
        });
        // Same loop body as production, with the bound shortened. The BOUND
        // ITSELF is the production code path.
        std::thread::spawn(move || {
            scan_dispatch_loop_with_bound(reqs, job_tx, res_rx, Duration::from_millis(400))
        });

        let key: ScanKey = (Vec::new(), dirs(&["no-runtime-stuck-key"]));
        let (tx, mut rx) = broadcast::channel::<ScanResult>(1);
        {
            let mut st = scan_state().lock().unwrap_or_else(|e| e.into_inner());
            st.cache = None;
            st.in_flight.insert(key.clone(), tx);
        }
        let gave_up_before = scans_gave_up();

        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        req_tx
            .try_send(ScanRequest {
                key: key.clone(),
                scan: Box::new(move || {
                    let _ = entered_tx.send(());
                    let _ = release_rx.recv(); // a walk that never returns
                    Vec::new()
                }),
            })
            .expect("queued");

        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the scan never started");

        // Give the bound time to fire, then observe — all from the test
        // thread, with no runtime anywhere.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut released = false;
        while Instant::now() < deadline {
            let held = {
                let st = scan_state().lock().unwrap_or_else(|e| e.into_inner());
                st.in_flight.contains_key(&key)
            };
            if !held {
                released = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            released,
            "the in-flight slot was never released — the bound did not fire, which is exactly \
             the inert shape iteration 15 reproduced 3/3"
        );
        assert!(
            scans_gave_up() > gave_up_before,
            "the give-up counter did not move — the scan was not recorded as abandoned"
        );
        // The waiter must have been handed something, not left parked.
        assert!(
            rx.try_recv().is_ok(),
            "no degraded result was broadcast to the waiters"
        );

        let _ = release_tx.send(());
    }

    /// The give-up must reach the LOG, not just the counter.
    ///
    /// Iteration 15's single most useful missing piece of evidence was this
    /// WARN: it never appeared in any of the four reproductions, because the
    /// `tokio::time::timeout` that would have emitted it could not fire. The
    /// line is captured here through a real `tracing` subscriber so "the WARN
    /// appears on a stuck scan" is a tested fact rather than a code reading.
    ///
    /// Neuter check: delete the `warn!` in the timeout arm of
    /// `scan_dispatch_loop_with_bound` and this test fails.
    #[test]
    fn a_stuck_scan_logs_the_give_up_warning() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        /// A `MakeWriter` that appends everything into a shared buffer.
        #[derive(Clone, Default)]
        struct Captured(Arc<Mutex<Vec<u8>>>);
        impl Write for Captured {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
            type Writer = Captured;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let _serial = cache_test_guard();
        let sink = Captured::default();
        let buf = sink.0.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink)
            .with_max_level(tracing::Level::WARN)
            .finish();

        // Scoped, not global: the harness runs other tests in parallel and a
        // global subscriber can only be set once per process.
        tracing::subscriber::with_default(subscriber, || {
            let (req_tx, reqs) = std::sync::mpsc::sync_channel::<ScanRequest>(1);
            let (job_tx, job_rx) = std::sync::mpsc::channel::<ScanFn>();
            let (res_tx, res_rx) = std::sync::mpsc::channel::<Vec<transcript::TranscriptSession>>();
            std::thread::spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let out = job();
                    if res_tx.send(out).is_err() {
                        break;
                    }
                }
            });
            // The dispatcher thread needs the subscriber too — `with_default`
            // is per-thread, so hand it an explicit dispatch clone.
            let dispatch = tracing::dispatcher::get_default(|d| d.clone());
            std::thread::spawn(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    scan_dispatch_loop_with_bound(reqs, job_tx, res_rx, Duration::from_millis(300))
                })
            });

            let key: ScanKey = (Vec::new(), dirs(&["give-up-warn-key"]));
            let (tx, _rx) = broadcast::channel::<ScanResult>(1);
            {
                let mut st = scan_state().lock().unwrap_or_else(|e| e.into_inner());
                st.cache = None;
                st.in_flight.insert(key.clone(), tx);
            }

            let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            req_tx
                .try_send(ScanRequest {
                    key,
                    scan: Box::new(move || {
                        let _ = entered_tx.send(());
                        let _ = release_rx.recv();
                        Vec::new()
                    }),
                })
                .expect("queued");
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the scan never started");

            // Measured on the test thread with a std clock — never a tokio
            // timer, so a regression fails instead of hanging.
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut logged = String::new();
            while Instant::now() < deadline {
                logged =
                    String::from_utf8_lossy(&buf.lock().unwrap_or_else(|e| e.into_inner()).clone())
                        .to_string();
                if logged.contains("has not returned within") {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let _ = release_tx.send(());

            assert!(
                logged.contains("has not returned within"),
                "the give-up WARN never reached the log on a stuck scan — this is the exact \
                 line that was missing from all four wedge reproductions. Captured output was: \
                 {logged:?}"
            );
            assert!(
                logged.contains("serving last-known-good"),
                "the give-up WARN did not say what it did instead; captured: {logged:?}"
            );
        });
        reset_scan_cache();
    }

    // ── Item 2: the periodic scan must be OFF the request path ───────────

    /// **The load-bearing test for Item 2.** With a last-known-good entry in
    /// the cache, a caller must be served *immediately* even while the scan
    /// thread is stuck on a walk that never returns.
    ///
    /// Neuter check: make the stale path `await_scan(rx).await` instead of
    /// returning `sessions`, and this test fails on the 2s bound.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stuck_scan_does_not_delay_a_caller_that_has_last_known_good() {
        let _serial = cache_test_guard();
        reset_scan_cache();
        let paths: Vec<String> = Vec::new();
        let key = dirs(&["lkg-key"]);

        // Seed a STALE last-known-good entry.
        {
            let mut st = scan_state().lock().unwrap_or_else(|e| e.into_inner());
            st.cache = Some(ScanCache {
                computed_at: Instant::now()
                    .checked_sub(SCAN_CACHE_TTL + Duration::from_secs(1))
                    .unwrap_or_else(Instant::now),
                project_paths: paths.clone(),
                config_dirs: key.clone(),
                sessions: vec![fake_session("cached-answer")],
            });
        }

        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();

        // This caller owns the refresh and must NOT wait for it.
        let started = Instant::now();
        let served = collect_all_sessions_cached_with(&paths, &key, move || {
            let _ = entered_tx.send(());
            let _ = release_rx.recv(); // the walk that wedged the runner
            vec![fake_session("fresh")]
        })
        .await;

        assert_eq!(
            served.len(),
            1,
            "the caller was not served the last-known-good entry"
        );
        assert_eq!(served[0].session_id, "cached-answer");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the caller waited {:?} on a live scan — the periodic scan is back on the request \
             path",
            started.elapsed()
        );

        // Confirm the refresh really was dispatched (not silently skipped).
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the background refresh never ran");
        let _ = release_tx.send(());
        // Let the dispatcher settle so the next test starts idle.
        reset_scan_cache();
    }

    /// A blocked scan must not block *unrelated* callers.
    ///
    /// Under the pre-iteration-14 implementation the leader held a
    /// `tokio::sync::Mutex` across `spawn_blocking(...).await`, so a
    /// different-key caller parked on that guard for as long as the scan was
    /// stuck. Measured with a std channel from OUTSIDE the runtime, so a
    /// regression fails instead of hanging.
    #[test]
    fn a_stuck_scan_does_not_block_an_unrelated_caller() {
        let _serial = cache_test_guard();
        reset_scan_cache();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("runtime");

        let paths: Vec<String> = Vec::new();
        let stuck_key = dirs(&["stuck-scan-key"]);
        let other_key = dirs(&["unrelated-scan-key"]);

        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();

        let p = paths.clone();
        let d = stuck_key.clone();
        let leader = rt.spawn(async move {
            collect_all_sessions_cached_with(&p, &d, move || {
                let _ = entered_tx.send(());
                let _ = release_rx.recv(); // park exactly like a wedged walk
                Vec::new()
            })
            .await
        });

        // Do not race the leader: wait until its scan is genuinely in flight.
        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("leader scan entered");

        // The whole point: a caller for a DIFFERENT key must complete now.
        // Measured on THIS thread, outside the runtime.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();
        let p2 = paths.clone();
        let d2 = other_key.clone();
        rt.spawn(async move {
            let out = collect_all_sessions_cached_with(&p2, &d2, Vec::new).await;
            let _ = done_tx.send(out.len());
        });
        let unrelated = done_rx.recv_timeout(Duration::from_secs(5));
        assert!(
            unrelated.is_ok(),
            "an unrelated caller was blocked by an in-flight scan"
        );

        let _ = release_tx.send(());
        let _ = rt.block_on(leader);
        reset_scan_cache();
    }

    /// Single flight must survive the restructure: two concurrent cold callers
    /// on the SAME key run exactly one scan, and the follower gets the leader's
    /// result rather than an empty list.
    #[test]
    fn concurrent_same_key_callers_still_run_exactly_one_scan() {
        let _serial = cache_test_guard();
        reset_scan_cache();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("runtime");

        let paths: Vec<String> = Vec::new();
        let key = dirs(&["single-flight-key"]);
        let before = scans_performed();

        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();

        let p = paths.clone();
        let d = key.clone();
        let leader = rt.spawn(async move {
            collect_all_sessions_cached_with(&p, &d, move || {
                let _ = entered_tx.send(());
                let _ = release_rx.recv();
                vec![fake_session("leader-scan")]
            })
            .await
        });

        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("leader scan entered");

        // This one must NOT start a second scan — it must follow.
        let p2 = paths.clone();
        let d2 = key.clone();
        let follower = rt.spawn(async move {
            collect_all_sessions_cached_with(&p2, &d2, || {
                panic!("a follower must never run its own scan")
            })
            .await
        });

        let _ = release_tx.send(());
        let lead = rt.block_on(leader).expect("leader joined");
        let follow = rt.block_on(follower).expect("follower joined");

        assert_eq!(
            scans_performed() - before,
            1,
            "two concurrent cold callers on one key must collapse to a single scan"
        );
        assert_eq!(lead.len(), 1);
        assert_eq!(
            follow.len(),
            1,
            "the follower must be handed the leader's result, not an empty list"
        );
        assert_eq!(follow[0].session_id, "leader-scan");
        reset_scan_cache();
    }

    /// The regression this module exists for: six terminal tabs polling in one
    /// burst must produce ONE scan, not six.
    ///
    /// Single test rather than three, because the cache and the scan counter
    /// are process-global — separate `#[tokio::test]`s would race each other.
    /// Empty `project_paths` makes the underlying scan a no-op, so this
    /// measures the caching decision and nothing else.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_callers_collapse_to_one_scan() {
        let _serial = cache_test_guard();
        reset_scan_cache();
        let paths: Vec<String> = Vec::new();
        let key_a = dirs(&["scan-cache-test-a"]);
        let before = scans_performed();

        let mut handles = Vec::new();
        for _ in 0..6 {
            let p = paths.clone();
            let d = key_a.clone();
            handles.push(tokio::spawn(async move {
                collect_all_sessions_cached(&p, &d).await
            }));
        }
        for h in handles {
            h.await.expect("scan task joined");
        }
        assert_eq!(
            scans_performed() - before,
            1,
            "six concurrent callers must collapse to a single scan"
        );

        // A further poll inside the TTL is served from the entry.
        let _ = collect_all_sessions_cached(&paths, &key_a).await;
        assert_eq!(
            scans_performed() - before,
            1,
            "a repeat within the TTL must not rescan"
        );

        // Different inputs get their own scan — the key is honoured.
        let _ = collect_all_sessions_cached(&paths, &dirs(&["scan-cache-test-b"])).await;
        assert_eq!(
            scans_performed() - before,
            2,
            "a different cache key must force its own scan"
        );
        reset_scan_cache();
    }

    /// No `tokio::time` may return to the scan path. The whole defect was a
    /// bound that needed the runtime it was guarding; a grep-level invariant
    /// is the cheapest way to stop it coming back.
    #[test]
    fn the_scan_path_uses_no_tokio_timer() {
        let src = include_str!("transcript.rs");
        // Split on the attribute alone: line-ending independent, and this file
        // contains exactly one `#[cfg(test)]`. Match on the CALL shape
        // (trailing paren) so the module docs, which have to name the defect
        // in order to explain it, do not trip their own guard.
        let production = src.split("#[cfg(test)]").next().expect("module body");
        assert!(
            !production.contains("tokio::time::timeout("),
            "tokio::time::timeout is back on the scan path — it cannot fire when the runtime it \
             guards is parked (iteration 15 reproduced that 3/3)"
        );
        assert!(
            !production.contains("tokio::time::sleep("),
            "tokio::time::sleep is back on the scan path — same defect class"
        );
    }
}
