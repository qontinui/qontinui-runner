//! Transcript-tail populator for PTY-launched AI tabs (Phase 1.5).
//!
//! Watches Claude CLI's per-session JSONL transcripts on disk
//! (`<config_dir>/projects/<encoded-project>/<session_id>.jsonl`), parses
//! `Edit` / `Write` / `MultiEdit` `tool_use` blocks out of them, and calls
//! `pg.record_file_touched(session_id, file_path, None)` so that the
//! `project.session_touched_files` table populates for PTY tabs the same way
//! it already does for SDK chat sessions via `auto_register_file`.
//!
//! Why this exists: the dominant terminal-AI launch path
//! (`onLaunchAiSession` at `TerminalPage.tsx:389-424`) types `claude` into a
//! PTY shell. The Claude CLI runs as a child of the PTY, NOT of
//! `ClaudeSession::spawn`, so its tool-use stream is never read by the runner
//! and `auto_register_file` never fires for those tabs. Without this watcher,
//! the §1 traffic light has no rows to read for the dominant UX path.
//!
//! Reuses existing infrastructure:
//!   - `terminal::transcript::find_claude_config_dirs()` for config dirs.
//!   - `terminal::transcript::list_sessions()` for startup discovery.
//!   - `terminal::transcript::is_workflow_session_marker()` for the
//!     workflow-vs-interactive filter.
//!   - `terminal::transcript::parse_line_for_touched_files()` for parsing.
//!
//! Mirrors the `notify::RecommendedWatcher` + tokio-channel pattern from
//! `trigger_system/watchers/file_watcher.rs` and `wrappers/registry.rs`.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::OnceCell;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use tokio::sync::{mpsc, Mutex as TokioMutex, Notify};
use tracing::{debug, info, warn};

use super::transcript::{
    find_claude_config_dirs, is_workflow_session_marker, list_sessions, parse_line_for_agent_log,
    parse_line_for_touched_files, AgentLogObs,
};
use crate::claude_session::coord_register::{AgentLogEmitter, AiCoordRegistrar};
use crate::session::session_transcript_tailer::SessionTranscriptTailer;
use crate::settings::TranscriptWatcherSettings;

/// Ceiling for "recent enough to schedule a tail task on startup". Older
/// JSONLs are skipped — they belong to closed sessions whose tabs the user
/// can no longer act on.
const STARTUP_RECENCY: Duration = Duration::from_secs(24 * 60 * 60);

/// Per-tail-task fallback poll interval — the BASE of the backoff ladder in
/// [`next_fallback_tick`]. Defends a missed `Modify` event from `notify` by
/// ensuring the loop runs at least this often even without a wake, while the
/// file is actively being written to. A tail that has been idle longer backs
/// off (5 s after 30 s idle, 30 s after 5 min), and every tick carries ±20 %
/// jitter so a cohort of tails woken together de-phases instead of locking
/// into the 1 Hz spawn cascade that kept tokio's blocking pool from ever
/// retiring an idle thread (plan
/// `2026-09-21-runner-blocking-pool-ratchets-to-peak-because-transcript-tails-rotate-every-idle-thread`,
/// Evidence 4 and 7).
const TAIL_FALLBACK_TICK: Duration = Duration::from_secs(1);

/// Idle-for beyond which the fallback tick steps from 1 s to 5 s.
const TAIL_BACKOFF_STEP_1: Duration = Duration::from_secs(30);
/// Fallback tick between [`TAIL_BACKOFF_STEP_1`] and [`TAIL_BACKOFF_STEP_2`].
const TAIL_FALLBACK_TICK_MID: Duration = Duration::from_secs(5);
/// Idle-for beyond which the fallback tick steps from 5 s to 30 s.
const TAIL_BACKOFF_STEP_2: Duration = Duration::from_secs(5 * 60);
/// Fallback tick beyond [`TAIL_BACKOFF_STEP_2`].
const TAIL_FALLBACK_TICK_SLOW: Duration = Duration::from_secs(30);
/// Fraction of the base tick the jitter may add or subtract.
const TAIL_TICK_JITTER_FRACTION: f64 = 0.2;

/// Consecutive `tokio::fs::metadata` failures after which a tail parks
/// instead of retrying forever. A deleted transcript whose `Remove` event
/// never arrives (Windows delivers it late or not at all for a file another
/// process still holds open) used to keep its tail polling for the rest of
/// the process's life.
const METADATA_ERROR_PARK_STREAK: u32 = 3;

/// How often the orchestrator sweeps PARKED entries older than the retention
/// window out of the registry.
const PARKED_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Width of the window the cohort-wake detector counts wakes in.
const COHORT_WAKE_WINDOW: Duration = Duration::from_millis(250);

/// More than this many tails woken inside one [`COHORT_WAKE_WINDOW`] is a
/// "cohort wake" — the shape that minted the +51 / +125 pool-thread steps
/// nothing logged (plan Evidence 5). Recorded once per window at `info!`.
pub(crate) const COHORT_WAKE_THRESHOLD: usize = 25;

/// Where a freshly scheduled tail puts its byte cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TailStart {
    /// Offset 0 — a file we saw being created, so nothing has been read yet.
    Start,
    /// The file's current length — a file that predates this tail, whose
    /// earlier content belongs to a session we cannot attribute reads to.
    Eof,
    /// An exact byte offset — the cursor a PARKED tail left behind, so the
    /// append that revived it is read rather than skipped or re-read.
    At(u64),
}

/// Why a tail task returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TailExit {
    /// The registry cancelled it (its transcript was removed).
    Cancelled,
    /// The in-task re-check found the workflow-session marker.
    WorkflowTornDown,
    /// Neither a wake nor byte growth for the configured idle timeout, or the
    /// file's metadata was unreadable [`METADATA_ERROR_PARK_STREAK`] times in
    /// a row. The task is gone; `cursor` is what the registry keeps so the
    /// next `Modify` revives the tail from exactly here.
    Idle { cursor: u64 },
}

/// One step of a [`TailReader`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TailStep {
    /// Fully-terminated lines appended past the cursor, in file order, each
    /// still carrying its `\n`. Never empty. `truncated` says the file shrank
    /// since the previous step and the cursor restarted at 0 before this read
    /// — the consumer resets any per-file prefix state.
    Appended { bytes: String, truncated: bool },
    /// The reader is finished; nothing more will be delivered.
    Exit(TailExit),
}

/// Spawn-time tail configuration, derived once from
/// [`TranscriptWatcherSettings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TailConfig {
    /// Zero = never park.
    pub(crate) idle_timeout: Duration,
    /// How long a PARKED entry is kept before the sweep drops it.
    pub(crate) parked_retention: Duration,
}

impl From<&TranscriptWatcherSettings> for TailConfig {
    fn from(s: &TranscriptWatcherSettings) -> Self {
        Self {
            idle_timeout: Duration::from_secs(s.tail_idle_timeout_secs),
            parked_retention: Duration::from_secs(s.tail_parked_retention_secs),
        }
    }
}

/// Next fallback tick for a tail that has been idle for `idle_for`, with
/// `jitter` in `[-1, 1]` (values outside are clamped) spreading the result
/// across `[0.8, 1.2] × base`. Pure, so the ladder is unit-tested; the loop
/// draws `jitter` from `rand`.
///
/// Base: [`TAIL_FALLBACK_TICK`] (1 s) while `idle_for < 30 s`,
/// [`TAIL_FALLBACK_TICK_MID`] (5 s) until 5 min, [`TAIL_FALLBACK_TICK_SLOW`]
/// (30 s) beyond. With ~550 mostly-idle tails this takes the blocking-pool
/// spawn stream from ~450/s to well under 30/s — below the N/10 rotation
/// rate at which tokio's keep-alive starts retiring idle threads again.
pub(crate) fn next_fallback_tick(idle_for: Duration, jitter: f64) -> Duration {
    let base = if idle_for < TAIL_BACKOFF_STEP_1 {
        TAIL_FALLBACK_TICK
    } else if idle_for < TAIL_BACKOFF_STEP_2 {
        TAIL_FALLBACK_TICK_MID
    } else {
        TAIL_FALLBACK_TICK_SLOW
    };
    let jitter = if jitter.is_finite() {
        jitter.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    base.mul_f64(1.0 + TAIL_TICK_JITTER_FRACTION * jitter)
}

/// A uniform draw in `[-1, 1)` for [`next_fallback_tick`].
fn random_jitter() -> f64 {
    rand::random::<f64>() * 2.0 - 1.0
}

// ── Process-wide gauge (served on `/health` as `transcriptWatcher`) ──────────
//
// The registry sits behind a tokio mutex, so the counts a sync, lock-free
// `health_snapshot` needs are kept in atomics that every registry mutation
// updates. There is exactly one registry per process (the `WATCHER`
// `OnceCell` rejects a second `start_transcript_watcher`), so the atomics
// describe it.

/// Live tail tasks in the registry.
static TAILS_LIVE: AtomicUsize = AtomicUsize::new(0);
/// Parked entries in the registry.
static TAILS_PARKED: AtomicUsize = AtomicUsize::new(0);
/// Tail tasks spawned since boot (revives included).
static TAILS_STARTED_SINCE_BOOT: AtomicU64 = AtomicU64::new(0);
/// Tail tasks that returned since boot, for any reason.
static TAILS_ENDED_SINCE_BOOT: AtomicU64 = AtomicU64::new(0);
/// Unix milliseconds of the last cohort wake; 0 = none since boot.
static LAST_COHORT_WAKE_UNIX_MS: AtomicU64 = AtomicU64::new(0);
/// Wake count of the last cohort wake's window.
static LAST_COHORT_WAKE_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Monotonic id for tail tasks, so a task's exit only ever touches the
/// registry entry it was spawned for.
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

/// The watcher's gauge, for `/health`'s `transcriptWatcher` key. Sync and
/// lock-free — reads atomics only.
///
/// `lastCohortWake` is `null` until the first cohort wake since boot; that is
/// "none observed", never "unknown" — the detector runs on every wake.
pub fn health_snapshot() -> serde_json::Value {
    let last_cohort_wake = match LAST_COHORT_WAKE_UNIX_MS.load(Ordering::Relaxed) {
        0 => serde_json::Value::Null,
        ms => {
            let at = chrono::DateTime::<chrono::Utc>::from(
                SystemTime::UNIX_EPOCH + Duration::from_millis(ms),
            )
            .to_rfc3339();
            json!({
                "at": at,
                "count": LAST_COHORT_WAKE_COUNT.load(Ordering::Relaxed),
            })
        }
    };
    json!({
        "tailsLive": TAILS_LIVE.load(Ordering::Relaxed),
        "tailsParked": TAILS_PARKED.load(Ordering::Relaxed),
        "tailsStartedSinceBoot": TAILS_STARTED_SINCE_BOOT.load(Ordering::Relaxed),
        "tailsEndedSinceBoot": TAILS_ENDED_SINCE_BOOT.load(Ordering::Relaxed),
        "lastCohortWake": last_cohort_wake,
    })
}

/// A cohort wake the detector reported for one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CohortWake {
    /// Wakes counted in the window so far (grows as the window continues).
    pub(crate) count: usize,
    /// The first three session ids woken in the window.
    pub(crate) first_ids: Vec<String>,
    /// True on the observation that crossed the threshold — the one to log.
    pub(crate) newly_crossed: bool,
}

/// Counts tail wakes (a `Modify` delivered to a live tail, or a parked
/// revive) in fixed [`COHORT_WAKE_WINDOW`]s and reports when a window holds
/// more than [`COHORT_WAKE_THRESHOLD`] of them. Pure over an injected clock.
#[derive(Debug)]
pub(crate) struct CohortWakeDetector {
    window_start: Instant,
    count: usize,
    first_ids: Vec<String>,
    threshold: usize,
}

impl CohortWakeDetector {
    pub(crate) fn new(threshold: usize, now: Instant) -> Self {
        Self {
            window_start: now,
            count: 0,
            first_ids: Vec::with_capacity(3),
            threshold,
        }
    }

    /// Record one wake. Returns `Some` while the current window is over the
    /// threshold; `newly_crossed` is true exactly once per window.
    pub(crate) fn observe(&mut self, session_id: &str, now: Instant) -> Option<CohortWake> {
        if now.saturating_duration_since(self.window_start) > COHORT_WAKE_WINDOW {
            self.window_start = now;
            self.count = 0;
            self.first_ids.clear();
        }
        self.count += 1;
        if self.first_ids.len() < 3 {
            self.first_ids.push(session_id.to_string());
        }
        if self.count > self.threshold {
            Some(CohortWake {
                count: self.count,
                first_ids: self.first_ids.clone(),
                newly_crossed: self.count == self.threshold + 1,
            })
        } else {
            None
        }
    }
}

/// Record a cohort wake in the gauge and, on the crossing observation, log
/// it — the line Evidence 5 was missing.
fn record_cohort_wake(wake: &CohortWake) {
    LAST_COHORT_WAKE_COUNT.store(wake.count, Ordering::Relaxed);
    if wake.newly_crossed {
        let ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
            .max(1);
        LAST_COHORT_WAKE_UNIX_MS.store(ms, Ordering::Relaxed);
        info!(
            count = wake.count,
            window_ms = COHORT_WAKE_WINDOW.as_millis() as u64,
            first_sessions = %wake.first_ids.join(", "),
            "transcript_watcher: cohort wake — more than {} tails woken inside one window",
            COHORT_WAKE_THRESHOLD
        );
    }
}

/// Hold the watcher for the app's lifetime. `notify::RecommendedWatcher` is
/// `Drop`-stop-the-watcher, so we keep it alive in a `OnceCell`. The watcher
/// is opaque to the rest of the program; its events flow through an
/// `mpsc::Sender` instead. The std::sync::Mutex is never actually locked
/// after `set()` — it's just a `Send + Sync` wrapper for the
/// not-necessarily-Sync watcher handle.
static WATCHER: OnceCell<std::sync::Mutex<Vec<RecommendedWatcher>>> = OnceCell::new();

/// Errors a tail task can return. Non-fatal — the supervisor logs and
/// proceeds.
#[derive(Debug, thiserror::Error)]
enum TailError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Handle to a running tail task. Wakes the task on `notify`-fired events and
/// supports clean cancellation. `id` is the task's own [`NEXT_TASK_ID`]
/// draw: a task's exit path only parks or removes the entry if it still
/// carries this id, so a `Remove` + re-`Create` that raced the exit is never
/// clobbered by a stale writer.
struct TailHandle {
    wake: Arc<Notify>,
    cancel: Arc<Notify>,
    id: u64,
}

/// One registry entry per transcript session id.
enum TailEntry {
    /// A task is running and can be woken or cancelled.
    Live(TailHandle),
    /// The task exited on idle. No task exists; the cursor is what the next
    /// `Modify` revives from. Dropped by `Remove` or by the retention sweep.
    Parked {
        path: PathBuf,
        cursor: u64,
        parked_at: Instant,
    },
}

/// The per-session registry. Every mutation goes through a method here so
/// the [`TAILS_LIVE`] / [`TAILS_PARKED`] atomics `health_snapshot` reads
/// stay equal to the map's composition.
#[derive(Default)]
struct TailRegistry {
    entries: HashMap<String, TailEntry>,
}

type SharedRegistry = Arc<TokioMutex<TailRegistry>>;

fn account_inserted(entry: &TailEntry) {
    match entry {
        TailEntry::Live(_) => TAILS_LIVE.fetch_add(1, Ordering::Relaxed),
        TailEntry::Parked { .. } => TAILS_PARKED.fetch_add(1, Ordering::Relaxed),
    };
}

fn account_removed(entry: &TailEntry) {
    match entry {
        TailEntry::Live(_) => TAILS_LIVE.fetch_sub(1, Ordering::Relaxed),
        TailEntry::Parked { .. } => TAILS_PARKED.fetch_sub(1, Ordering::Relaxed),
    };
}

impl TailRegistry {
    fn get(&self, session_id: &str) -> Option<&TailEntry> {
        self.entries.get(session_id)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    /// Insert a live handle, displacing whatever was there (a parked entry
    /// being revived). Callers check for an existing `Live` first.
    fn insert_live(&mut self, session_id: String, handle: TailHandle) {
        let entry = TailEntry::Live(handle);
        account_inserted(&entry);
        if let Some(old) = self.entries.insert(session_id, entry) {
            account_removed(&old);
        }
    }

    /// Remove the entry outright (the `Remove` arm). Returns it so a live
    /// task can be cancelled.
    fn remove(&mut self, session_id: &str) -> Option<TailEntry> {
        let old = self.entries.remove(session_id)?;
        account_removed(&old);
        Some(old)
    }

    /// The exit path of task `task_id`: replace ITS live entry with a parked
    /// one carrying `cursor`. A no-op (returns false) when the entry is gone
    /// or belongs to a newer task.
    fn park_if_current(
        &mut self,
        session_id: &str,
        task_id: u64,
        path: PathBuf,
        cursor: u64,
        now: Instant,
    ) -> bool {
        if !self.is_current_live(session_id, task_id) {
            return false;
        }
        let parked = TailEntry::Parked {
            path,
            cursor,
            parked_at: now,
        };
        account_inserted(&parked);
        if let Some(old) = self.entries.insert(session_id.to_string(), parked) {
            account_removed(&old);
        }
        true
    }

    /// The exit path of task `task_id`: remove ITS live entry. A no-op
    /// (returns false) when the entry is gone or belongs to a newer task.
    fn remove_if_current(&mut self, session_id: &str, task_id: u64) -> bool {
        if !self.is_current_live(session_id, task_id) {
            return false;
        }
        self.remove(session_id).is_some()
    }

    fn is_current_live(&self, session_id: &str, task_id: u64) -> bool {
        matches!(
            self.entries.get(session_id),
            Some(TailEntry::Live(h)) if h.id == task_id
        )
    }

    /// Drop every parked entry parked longer than `retention` ago. Returns
    /// how many were dropped.
    fn sweep_parked(&mut self, retention: Duration, now: Instant) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, entry| match entry {
            TailEntry::Live(_) => true,
            TailEntry::Parked { parked_at, .. } => {
                let keep = now.saturating_duration_since(*parked_at) < retention;
                if !keep {
                    account_removed(entry);
                }
                keep
            }
        });
        before - self.entries.len()
    }
}

/// Public entry point. Spawns the watcher + tail tasks asynchronously and
/// returns immediately. The watcher lives until process exit; do not call
/// twice for the same process.
///
/// `workspace_paths` is the list of project paths the runner currently tracks
/// (typically a single workspace root). Each path is matched against every
/// discovered config dir's `projects/<encoded>/` folder.
///
/// `tailer` is the session-repository transcript tailer (plan
/// `2026-08-26-claude-code-session-repository-in-qontinui-web` Phase 2). It is
/// a SECOND consumer of the same byte-cursor read this watcher already
/// performs — deliberately not a second watcher, so there is one cursor, one
/// rotation/truncation handler and one workflow-session filter rather than two
/// that can disagree. `None` disables it entirely.
///
/// `settings` is read ONCE by the caller (`crate::settings::get_transcript_watcher_settings`)
/// — the idle-park and parked-retention knobs are spawn-time.
pub fn start_transcript_watcher(
    app_handle: tauri::AppHandle,
    pg: Arc<crate::database::pg::PgDb>,
    workspace_paths: Vec<String>,
    registrar: Option<Arc<AiCoordRegistrar>>,
    tailer: Option<Arc<SessionTranscriptTailer>>,
    settings: TranscriptWatcherSettings,
) -> Result<(), String> {
    if WATCHER.get().is_some() {
        return Err("transcript watcher already started".to_string());
    }

    let cfg = TailConfig::from(&settings);
    info!(
        tail_idle_timeout_secs = settings.tail_idle_timeout_secs,
        tail_parked_retention_secs = settings.tail_parked_retention_secs,
        "transcript_watcher: starting with {} workspace path(s)",
        workspace_paths.len()
    );
    // Phase 9 (plan 2026-09-18-notifications-are-agent-actions-and-alerts-are-
    // agent-work): record the notifier gate's value once per process.
    super::commit_report::log_action_notify_gate();

    // Spawn the orchestrator on the tokio runtime. It owns the channel, the
    // watcher, and the per-session task map.
    tauri::async_runtime::spawn(async move {
        if let Err(e) =
            run_orchestrator(app_handle, pg, workspace_paths, registrar, tailer, cfg).await
        {
            warn!("transcript_watcher: orchestrator exited with error: {}", e);
        }
    });

    Ok(())
}

/// Long-lived task: builds the `notify` watcher per config dir, schedules
/// startup tail tasks, and dispatches filesystem events to per-session
/// handles.
async fn run_orchestrator(
    app_handle: tauri::AppHandle,
    pg: Arc<crate::database::pg::PgDb>,
    workspace_paths: Vec<String>,
    registrar: Option<Arc<AiCoordRegistrar>>,
    tailer: Option<Arc<SessionTranscriptTailer>>,
    cfg: TailConfig,
) -> Result<(), String> {
    let config_dirs = find_claude_config_dirs();
    if config_dirs.is_empty() {
        info!("transcript_watcher: no Claude config dirs found; nothing to watch");
        return Ok(());
    }

    // session_id -> live handle or parked cursor
    let tasks: SharedRegistry = Arc::new(TokioMutex::new(TailRegistry::default()));

    // ── 0. Parked-retention sweep ─────────────────────────────────────────
    //
    // A parked entry older than the retention window is dropped, so the
    // registry is bounded by files touched within that window rather than
    // by files created since boot. Its own task so the dispatch loop below
    // stays a plain `recv`.
    {
        let tasks = tasks.clone();
        let retention = cfg.parked_retention;
        tauri::async_runtime::spawn(async move {
            let mut ticks = tokio::time::interval(PARKED_SWEEP_INTERVAL);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                let dropped = tasks.lock().await.sweep_parked(retention, Instant::now());
                if dropped > 0 {
                    debug!(
                        dropped,
                        retention_secs = retention.as_secs(),
                        "transcript_watcher: swept parked tails past retention"
                    );
                }
            }
        });
    }

    // The event channel is created BEFORE discovery: every tail task holds
    // a sender so an exit that parks a cursor with bytes already past it can
    // feed itself back through the dispatcher as a `Modify` (see the exit
    // closure in `schedule_tail`). One `mpsc` channel for all config dirs;
    // each `notify` callback runs in a sync context so it uses `try_send`.
    let (tx, mut rx) = mpsc::channel::<Event>(128);
    let revive_tx = tx.clone();

    // ── 1. Discovery on startup ───────────────────────────────────────────
    //
    // For every (config_dir × workspace_path) pair, list current sessions and
    // schedule a tail starting from EOF for any whose mtime is within the
    // last STARTUP_RECENCY.
    let now = SystemTime::now();
    let mut transcripts_on_disk = 0usize;
    let mut recent_transcripts = 0usize;
    for config_dir in &config_dirs {
        for project in &workspace_paths {
            let sessions = match list_sessions(config_dir, project) {
                Ok(s) => s,
                Err(e) => {
                    debug!(
                        "transcript_watcher: list_sessions({:?}, {}) failed: {}",
                        config_dir, project, e
                    );
                    continue;
                }
            };
            for s in sessions {
                let jsonl = config_dir
                    .join("projects")
                    .join(encode_for_lookup(project))
                    .join(format!("{}.jsonl", s.session_id));
                if !jsonl.exists() {
                    continue;
                }
                transcripts_on_disk += 1;
                let mtime_ok = std::fs::metadata(&jsonl)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| now.duration_since(t).unwrap_or_default() <= STARTUP_RECENCY)
                    .unwrap_or(false);
                if !mtime_ok {
                    continue;
                }
                recent_transcripts += 1;
                schedule_tail(
                    &tasks,
                    s.session_id.clone(),
                    jsonl,
                    pg.clone(),
                    app_handle.clone(),
                    registrar.clone(),
                    tailer.clone(),
                    TailStart::Eof,
                    cfg,
                    revive_tx.clone(),
                )
                .await;
            }
        }
    }

    // ── 1a. Transcript-absence symptom check ──────────────────────────────
    //
    // This watcher is the ONLY populator of `project.session_touched_files` for
    // PTY-launched AI tabs, so "no transcripts" means coord's cross-session
    // file-conflict traffic light is blind for the dominant session type —
    // with no second writer to cover it. Nothing used to notice: a 2026-07-28
    // report of "13 live sessions, zero transcripts" ran for a week on a
    // misdiagnosed cause and was ultimately traced to a probe that searched
    // `$HOME/.claude` while the fleet sets `CLAUDE_CONFIG_DIR` per account —
    // i.e. it would have reported zero no matter what.
    //
    // So this logs the config dirs it ACTUALLY searched. That is the whole
    // point: it makes "we looked in the wrong place" distinguishable from
    // "persistence is off", which is exactly the distinction the original
    // investigation could not make.
    let searched: Vec<String> = config_dirs
        .iter()
        .map(|d| d.to_string_lossy().to_string())
        .collect();
    if workspace_paths.is_empty() {
        // Nothing was searched, so "zero found" carries no information —
        // emitting the warning here would be the same looked-in-the-wrong-place
        // false signal it exists to prevent.
        info!(
            config_dirs = %searched.join(", "),
            "transcript_watcher: no workspace paths tracked yet; transcript-absence check skipped"
        );
    } else if transcripts_on_disk == 0 {
        warn!(
            config_dirs = %searched.join(", "),
            workspace_paths = %workspace_paths.join(", "),
            "transcript_watcher: ZERO Claude transcripts found for any watched workspace path. \
             project.session_touched_files will not populate for PTY tabs, so cross-session file \
             conflict detection is blind for them. Verify the config dirs listed here are the \
             ones the CLI actually writes to (CLAUDE_CONFIG_DIR is set per account) BEFORE \
             concluding that transcript persistence is off."
        );
    } else {
        info!(
            transcripts_on_disk,
            recent_transcripts,
            config_dirs = %searched.join(", "),
            "transcript_watcher: startup discovery complete"
        );
    }

    // ── 2. Live watching ──────────────────────────────────────────────────

    let mut watchers: Vec<RecommendedWatcher> = Vec::new();
    for config_dir in &config_dirs {
        let projects_root = config_dir.join("projects");
        if !projects_root.exists() {
            continue;
        }
        let tx_for_handler = tx.clone();
        let watcher_result =
            notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
                if let Ok(ev) = res {
                    let _ = tx_for_handler.try_send(ev);
                }
            });
        let mut watcher = match watcher_result {
            Ok(w) => w,
            Err(e) => {
                warn!(
                    "transcript_watcher: failed to create watcher for {:?}: {}",
                    projects_root, e
                );
                continue;
            }
        };
        if let Err(e) = watcher.watch(&projects_root, RecursiveMode::Recursive) {
            warn!(
                "transcript_watcher: failed to watch {:?}: {}",
                projects_root, e
            );
            continue;
        }
        info!("transcript_watcher: watching {:?}", projects_root);
        watchers.push(watcher);
    }

    // Stash watchers so they live until process exit. Re-entry into
    // `start_transcript_watcher` is rejected by the `OnceCell` guard above.
    let _ = WATCHER.set(std::sync::Mutex::new(watchers));

    // ── 3. Event dispatch loop ────────────────────────────────────────────
    let mut cohort = CohortWakeDetector::new(COHORT_WAKE_THRESHOLD, Instant::now());
    while let Some(event) = rx.recv().await {
        for path in &event.paths {
            // Only attend to *.jsonl files inside a `projects/` subtree.
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let session_id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            match event.kind {
                EventKind::Create(_) => {
                    // Create: read first 5 lines, run workflow filter; if
                    // interactive, schedule a tail starting at offset 0.
                    if path_looks_like_workflow_session(path) {
                        debug!(
                            "transcript_watcher: skipping workflow session {} ({})",
                            session_id,
                            path.display()
                        );
                        continue;
                    }
                    schedule_tail(
                        &tasks,
                        session_id.clone(),
                        path.clone(),
                        pg.clone(),
                        app_handle.clone(),
                        registrar.clone(),
                        tailer.clone(),
                        TailStart::Start,
                        cfg,
                        revive_tx.clone(),
                    )
                    .await;
                }
                EventKind::Modify(_) => {
                    // Wake a live task; revive a parked one from its cursor;
                    // if neither, treat as a create (the runner may have
                    // started after the file).
                    let action = {
                        let map = tasks.lock().await;
                        match map.get(&session_id) {
                            Some(TailEntry::Live(h)) => {
                                h.wake.notify_one();
                                ModifyAction::Woken
                            }
                            Some(TailEntry::Parked { cursor, .. }) => {
                                ModifyAction::Revive { cursor: *cursor }
                            }
                            None => ModifyAction::Fresh,
                        }
                    };
                    match action {
                        ModifyAction::Woken => {
                            if let Some(w) = cohort.observe(&session_id, Instant::now()) {
                                record_cohort_wake(&w);
                            }
                        }
                        ModifyAction::Revive { cursor } => {
                            // No `path_looks_like_workflow_session` sniff: the
                            // tail passed the in-task re-check before it
                            // parked. The cursor is carried OUT from under the
                            // lock: if the retention sweep or a `Remove` drops
                            // the parked entry between this decision and
                            // `schedule_tail` re-taking the lock, the `None`
                            // arm still starts at the parked cursor rather
                            // than at EOF, so the append that woke us is not
                            // skipped.
                            if let Some(w) = cohort.observe(&session_id, Instant::now()) {
                                record_cohort_wake(&w);
                            }
                            schedule_tail(
                                &tasks,
                                session_id.clone(),
                                path.clone(),
                                pg.clone(),
                                app_handle.clone(),
                                registrar.clone(),
                                tailer.clone(),
                                TailStart::At(cursor),
                                cfg,
                                revive_tx.clone(),
                            )
                            .await;
                        }
                        ModifyAction::Fresh => {
                            if path_looks_like_workflow_session(path) {
                                continue;
                            }
                            schedule_tail(
                                &tasks,
                                session_id.clone(),
                                path.clone(),
                                pg.clone(),
                                app_handle.clone(),
                                registrar.clone(),
                                tailer.clone(),
                                TailStart::Eof,
                                cfg,
                                revive_tx.clone(),
                            )
                            .await;
                        }
                    }
                }
                EventKind::Remove(_) => {
                    let mut map = tasks.lock().await;
                    match map.remove(&session_id) {
                        Some(TailEntry::Live(h)) => {
                            h.cancel.notify_one();
                            debug!(
                                "transcript_watcher: cancelled tail for removed session {}",
                                session_id
                            );
                        }
                        Some(TailEntry::Parked { .. }) => {
                            debug!(
                                "transcript_watcher: dropped parked tail for removed session {}",
                                session_id
                            );
                        }
                        None => {}
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// What the dispatch loop decided under the registry lock for a `Modify`
/// event, acted on after the lock is released.
enum ModifyAction {
    /// A live tail was notified.
    Woken,
    /// A parked entry exists; reschedule from its cursor (carried here so a
    /// sweep racing the reschedule cannot downgrade the start to EOF).
    Revive { cursor: u64 },
    /// No entry; treat as a create.
    Fresh,
}

/// Cheap sniff for `is_workflow_session` without re-loading via
/// `list_sessions`. Reads up to the first 5 lines of the file. Returns true
/// if it's a workflow session OR if the file can't be read yet (which is
/// common immediately after `Create` because the writer hasn't flushed) —
/// callers fall back to spawning the task and the in-task re-check (step 7
/// in the loop) catches it after the first wake.
fn path_looks_like_workflow_session(path: &Path) -> bool {
    use std::io::{BufRead, BufReader};
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        // File not yet readable → treat as not-workflow; the in-task re-check
        // will tear down if the marker shows up later.
        Err(_) => return false,
    };
    let reader = BufReader::new(f);
    let mut buf = String::with_capacity(2048);
    for (i, line) in reader.lines().enumerate() {
        if i >= 5 {
            break;
        }
        if let Ok(l) = line {
            buf.push_str(&l);
            buf.push('\n');
        }
    }
    is_workflow_session_marker(&buf)
}

/// Spawn the per-session tail task. A `Live` entry is left alone (already
/// tailed). A `Parked` entry is replaced by a fresh `Live` one whose tail
/// starts at the parked cursor — `requested` is ignored for it, because the
/// cursor is the only start that neither re-reads nor skips the append that
/// revived the tail.
#[allow(clippy::too_many_arguments)]
async fn schedule_tail(
    tasks: &SharedRegistry,
    session_id: String,
    path: PathBuf,
    pg: Arc<crate::database::pg::PgDb>,
    app_handle: tauri::AppHandle,
    registrar: Option<Arc<AiCoordRegistrar>>,
    tailer: Option<Arc<SessionTranscriptTailer>>,
    requested: TailStart,
    cfg: TailConfig,
    revive_tx: mpsc::Sender<Event>,
) {
    let mut map = tasks.lock().await;
    let start = match map.get(&session_id) {
        Some(TailEntry::Live(_)) => return, // Already tailed.
        Some(TailEntry::Parked {
            cursor,
            path: parked_path,
            ..
        }) => {
            if parked_path != &path {
                // Same session id, different file (a moved transcript): the
                // parked cursor is meaningless for the new path, so start
                // from its end as a never-seen file would.
                debug!(
                    "transcript_watcher: parked tail for {} was at {} but the revive names {}; starting at EOF",
                    session_id,
                    parked_path.display(),
                    path.display()
                );
                TailStart::Eof
            } else {
                TailStart::At(*cursor)
            }
        }
        None => requested,
    };
    let wake = Arc::new(Notify::new());
    let cancel = Arc::new(Notify::new());
    let task_id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
    map.insert_live(
        session_id.clone(),
        TailHandle {
            wake: wake.clone(),
            cancel: cancel.clone(),
            id: task_id,
        },
    );
    drop(map);

    let tasks_for_exit = tasks.clone();
    let path_for_park = path.clone();

    tauri::async_runtime::spawn(async move {
        TAILS_STARTED_SINCE_BOOT.fetch_add(1, Ordering::Relaxed);
        // Armed until the exit below settles the entry: a panic inside
        // `tail_session` unwinds past the settle and would otherwise leave a
        // `Live` handle whose `wake` nobody polls — every later `Modify`
        // notified into the void and the file never tailed again until a
        // `Remove`.
        let mut settle_guard = SettleGuard {
            tasks: tasks_for_exit.clone(),
            session_id: session_id.clone(),
            task_id,
            armed: true,
        };
        let result = tail_session(
            session_id.clone(),
            path,
            pg,
            app_handle,
            registrar,
            tailer,
            wake,
            cancel,
            start,
            cfg,
        )
        .await;
        TAILS_ENDED_SINCE_BOOT.fetch_add(1, Ordering::Relaxed);
        settle_guard.armed = false;
        // Settle the registry entry THIS task owns. An idle exit parks the
        // cursor in place of the handle so the next `Modify` revives from it;
        // every other exit removes the entry so a reschedule works. Both are
        // gated on the entry still being this task's — a `Remove` that raced
        // the exit already dropped it, and a re-`Create` after that is a
        // newer task's entry.
        let mut map = tasks_for_exit.lock().await;
        match result {
            Ok(TailExit::Idle { cursor }) => {
                let parked = map.park_if_current(
                    &session_id,
                    task_id,
                    path_for_park.clone(),
                    cursor,
                    Instant::now(),
                );
                // A `Modify` that landed between the idle check and this
                // park notified a `wake` nobody will poll again. The write
                // always precedes its event, and the dispatcher's
                // `notify_one` runs under this same lock, so a length read
                // HERE sees every append that lost permit stood for. If the
                // file is not exactly at the parked cursor, do not leave it
                // parked: feed a synthetic `Modify` back through the
                // dispatcher, whose `Revive` arm restarts the tail at this
                // cursor. (A recursive `schedule_tail` from inside its own
                // spawned task is an infinitely-sized future; the channel is
                // the seam that already exists.)
                if parked {
                    let on_disk = std::fs::metadata(&path_for_park).map(|m| m.len()).ok();
                    if on_disk.is_some_and(|len| len != cursor) {
                        debug!(
                            "transcript_watcher: {} parked at {} but the file is {:?} bytes — reviving",
                            session_id, cursor, on_disk
                        );
                        let ev = Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
                            .add_path(path_for_park);
                        if revive_tx.try_send(ev).is_err() {
                            warn!(
                                "transcript_watcher: revive of {} was not queued (dispatcher channel full or closed); the next Modify will pick it up from offset {}",
                                session_id, cursor
                            );
                        }
                    }
                }
            }
            Ok(TailExit::Cancelled) | Ok(TailExit::WorkflowTornDown) => {
                map.remove_if_current(&session_id, task_id);
            }
            Err(e) => {
                warn!(
                    "transcript_watcher: tail task for {} exited with error: {}",
                    session_id, e
                );
                map.remove_if_current(&session_id, task_id);
            }
        }
    });
}

/// Removes a tail's registry entry if its task unwinds past the normal
/// settle in the exit closure of `schedule_tail`. Disarmed on every non-panic
/// exit; `Drop` cannot await, so it hands the removal to a fresh task.
struct SettleGuard {
    tasks: SharedRegistry,
    session_id: String,
    task_id: u64,
    armed: bool,
}

impl Drop for SettleGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let tasks = self.tasks.clone();
        let session_id = std::mem::take(&mut self.session_id);
        let task_id = self.task_id;
        warn!(
            "transcript_watcher: tail task for {} unwound without settling; dropping its registry entry",
            session_id
        );
        tauri::async_runtime::spawn(async move {
            tasks.lock().await.remove_if_current(&session_id, task_id);
        });
    }
}

/// The byte-cursor half of a tail: the wake/tick/cancel wait, the metadata
/// poll with its error streak, the truncation reset, the read of appended
/// complete lines, and the idle detection. Factored out of [`tail_session`]
/// so the idle exit and the cursor-preserving revive are pinned on temp
/// files without a `PgDb` or an `AppHandle`; `tail_session` owns everything
/// that consumes the lines.
struct TailReader {
    session_id: String,
    path: PathBuf,
    file: tokio::fs::File,
    cursor: u64,
    wake: Arc<Notify>,
    cancel: Arc<Notify>,
    /// Zero = never park.
    idle_timeout: Duration,
    /// Reset on every wake and on every byte GROWTH — `len` rising past
    /// `last_seen_len`, or complete lines actually consumed. NOT on "bytes past
    /// the cursor": a file whose tail is an unterminated line has bytes past
    /// the cursor on every poll forever, and resetting on that would keep such
    /// a file (a session killed mid-write — exactly the dead file this park
    /// exists for) at the 1 s base tick and never parked.
    last_activity: Instant,
    /// The largest length the last poll saw; growth is measured against it.
    last_seen_len: u64,
    metadata_err_streak: u32,
    /// The file shrank since the last delivered step; carried until the next
    /// `Appended` so the consumer can reset its per-file prefix state.
    truncated_pending: bool,
}

impl TailReader {
    async fn open(
        session_id: String,
        path: PathBuf,
        wake: Arc<Notify>,
        cancel: Arc<Notify>,
        start: TailStart,
        idle_timeout: Duration,
    ) -> Result<Self, TailError> {
        let file = tokio::fs::File::open(&path).await?;
        let cursor = match start {
            TailStart::Start => 0,
            TailStart::Eof => file.metadata().await.map(|m| m.len()).unwrap_or(0),
            // A cursor past the current length (the file was rewritten while
            // parked) is caught by the truncation check on the first step.
            TailStart::At(cursor) => cursor,
        };
        Ok(Self {
            session_id,
            path,
            file,
            last_seen_len: cursor,
            cursor,
            wake,
            cancel,
            idle_timeout,
            last_activity: Instant::now(),
            metadata_err_streak: 0,
            truncated_pending: false,
        })
    }

    fn cursor(&self) -> u64 {
        self.cursor
    }

    /// `Some(Idle)` once `idle_timeout` (non-zero) has elapsed since the last
    /// activity.
    fn idle_exit(&self, now: Instant) -> Option<TailExit> {
        if self.idle_timeout.is_zero() {
            return None;
        }
        if now.saturating_duration_since(self.last_activity) >= self.idle_timeout {
            Some(TailExit::Idle {
                cursor: self.cursor,
            })
        } else {
            None
        }
    }

    /// How long to sleep before the next fallback poll: the backoff ladder,
    /// capped so the idle timeout is observed on time rather than up to one
    /// slow tick late.
    fn next_sleep(&self, now: Instant) -> Duration {
        let idle_for = now.saturating_duration_since(self.last_activity);
        let tick = next_fallback_tick(idle_for, random_jitter());
        if self.idle_timeout.is_zero() {
            tick
        } else {
            tick.min(self.idle_timeout.saturating_sub(idle_for))
        }
    }

    /// Wait for the next wake / tick / cancel and deliver what it produced.
    /// Loops internally over ticks that find nothing appended.
    async fn next_step(&mut self) -> Result<TailStep, TailError> {
        loop {
            // ── 2. Idle check, then wait for wake or fallback tick or cancel ──
            let now = Instant::now();
            if let Some(exit) = self.idle_exit(now) {
                debug!(
                    "transcript_watcher: tail idle for {}s, parking {} at offset {}",
                    self.idle_timeout.as_secs(),
                    self.session_id,
                    self.cursor
                );
                return Ok(TailStep::Exit(exit));
            }
            let sleep_for = self.next_sleep(now);
            tokio::select! {
                _ = self.cancel.notified() => {
                    debug!("transcript_watcher: tail cancelled for {}", self.session_id);
                    return Ok(TailStep::Exit(TailExit::Cancelled));
                }
                _ = self.wake.notified() => {
                    self.last_activity = Instant::now();
                }
                _ = tokio::time::sleep(sleep_for) => {}
            }

            // ── 3. Detect rotation/truncation ─────────────────────────────
            let len = match tokio::fs::metadata(&self.path).await {
                Ok(m) => {
                    self.metadata_err_streak = 0;
                    m.len()
                }
                Err(e) => {
                    self.metadata_err_streak += 1;
                    if self.metadata_err_streak >= METADATA_ERROR_PARK_STREAK {
                        debug!(
                            "transcript_watcher: metadata for {} failed {} times in a row ({}), parking at offset {}",
                            self.session_id, self.metadata_err_streak, e, self.cursor
                        );
                        return Ok(TailStep::Exit(TailExit::Idle {
                            cursor: self.cursor,
                        }));
                    }
                    continue; // Transient — retry on next tick.
                }
            };
            if len < self.cursor {
                debug!(
                    "transcript_watcher: detected truncation for {} (was {} bytes, now {})",
                    self.session_id, self.cursor, len
                );
                self.cursor = 0;
                self.last_seen_len = 0;
                // Re-open to reset any internal seek state.
                self.file = tokio::fs::File::open(&self.path).await?;
                self.truncated_pending = true;
            }
            if len > self.last_seen_len {
                // Real growth. A poll that finds the same unterminated
                // fragment it found last time is NOT activity.
                self.last_activity = Instant::now();
                self.last_seen_len = len;
            }
            if len == self.cursor {
                continue;
            }

            // ── 4. Read appended bytes line-by-line ───────────────────────
            if let Err(e) = self.file.seek(std::io::SeekFrom::Start(self.cursor)).await {
                warn!(
                    "transcript_watcher: seek failed for {}: {}",
                    self.session_id, e
                );
                // Re-open and retry next tick.
                self.file = tokio::fs::File::open(&self.path).await?;
                continue;
            }
            let mut reader = BufReader::new(&mut self.file);
            let mut bytes = String::new();
            let mut bytes_read: u64 = 0;
            let mut line = String::new();
            loop {
                line.clear();
                let n = match reader.read_line(&mut line).await {
                    Ok(n) => n,
                    Err(e) => {
                        warn!(
                            "transcript_watcher: read_line failed for {}: {}",
                            self.session_id, e
                        );
                        break;
                    }
                };
                if n == 0 {
                    break; // EOF
                }
                // Only fully-terminated lines count — partial trailing reads
                // are NOT added to `bytes_read`, so we re-read them on the
                // next pass once the writer flushes the trailing newline.
                if !line.ends_with('\n') {
                    break;
                }
                bytes_read += n as u64;
                bytes.push_str(&line);
            }
            self.cursor += bytes_read;
            if bytes_read > 0 {
                self.last_activity = Instant::now();
            }
            if bytes.is_empty() {
                continue; // Only a partial line so far.
            }
            let truncated = std::mem::take(&mut self.truncated_pending);
            return Ok(TailStep::Appended { bytes, truncated });
        }
    }
}

/// Per-session tail loop. Reads new bytes on each wake, parses lines into
/// `TouchedFile`s, persists via `pg.record_file_touched`, and triggers a
/// debounced commit-state emit on landed rows. Returns how it ended — the
/// caller parks the cursor on [`TailExit::Idle`].
#[allow(clippy::too_many_arguments)]
async fn tail_session(
    session_id: String,
    path: PathBuf,
    pg: Arc<crate::database::pg::PgDb>,
    app_handle: tauri::AppHandle,
    registrar: Option<Arc<AiCoordRegistrar>>,
    tailer: Option<Arc<SessionTranscriptTailer>>,
    wake: Arc<Notify>,
    cancel: Arc<Notify>,
    start: TailStart,
    cfg: TailConfig,
) -> Result<TailExit, TailError> {
    // ── 1. Open file, establish initial cursor ────────────────────────────
    let mut reader = TailReader::open(
        session_id.clone(),
        path.clone(),
        wake,
        cancel,
        start,
        cfg.idle_timeout,
    )
    .await?;

    // Buffered prefix used for the workflow-session re-check on first wake.
    let mut workflow_check_buf = String::new();
    let mut did_workflow_recheck = false;

    // ── Interactive-agent log emitter (Phase 2) ───────────────────────────────
    //
    // Stream this PTY CLI session's assistant text + tool_use activity to
    // coord's `agent_logs` ingest so it appears on `/admin/coord/agents`. The
    // emitter is keyed on the transcript's own session UUID (the `.jsonl`
    // stem); coord's Phase-1a fallback resolves the tenant from the `device_id`
    // the emitter stamps, so there is NO separate `coord.sessions` registration
    // step here. `start()` is a strict no-op (returns `None`, spawns nothing)
    // when the gate `QONTINUI_AGENT_LOGS_FROM_SESSIONS` is set OFF (it defaults
    // ON), the `device_id` is unreadable, or `session_id` isn't a UUID. Held
    // for the tail's lifetime; the handle owns no thread — its lines sit in
    // the shared emitter service's per-agent queue, which is flushed on the
    // next tick and removed once empty, so dropping the handle at function
    // exit leaves nothing behind.
    //
    // We emit NEITHER a `session_started` NOR a `session_closed` milestone here:
    // a single real CLI session produces MANY `tail_session` invocations (file
    // rotation/truncation re-opens the cursor, the discovery loop reschedules a
    // torn-down tail on the next `Modify`, a runner restart re-discovers every
    // live transcript), so neither a tail start nor a tail teardown is a
    // reliable session-lifecycle signal — emitting `started()` per tail floods
    // `coord.agent_logs` with duplicate `session_started` rows (~one per tail
    // re-spawn). The per-line `assistant` / `tool_use` emits below ARE the
    // activity signal, and they are inherently de-duplicated by the byte cursor
    // (only lines past `cursor` emit), so re-invocation never double-counts
    // content. The once-per-session `session_started` milestone belongs to the
    // UI-interactive spawn path (`session.rs`), which fires exactly once per
    // genuine `ClaudeSession::spawn`.
    let agent_log_emitter = uuid::Uuid::parse_str(&session_id)
        .ok()
        .and_then(AgentLogEmitter::start);

    // ── Sensitive-action notifier (plan
    // `2026-09-18-notifications-are-agent-actions-and-alerts-are-agent-work`,
    // Phase 9) ─────────────────────────────────────────────────────────────
    //
    // The pending map that pairs a Bash `tool_use` (force-push, ref delete,
    // release, publish) with its `tool_result`, which lands in a LATER `user`
    // record. Owned by this tail — one per transcript, lock-free, bounded
    // (256 entries, 10-minute expiry) — and dropped with it. The gate is read
    // once per tail rather than per line.
    let notify_actions = registrar.is_some() && super::commit_report::action_notify_enabled();
    let mut action_tracker = super::commit_report::SensitiveActionTracker::new();
    let action_lane = super::commit_report::agent_notification_lane(&session_id);

    debug!(
        "transcript_watcher: tail started for {} at offset {} ({})",
        session_id,
        reader.cursor(),
        path.display()
    );

    loop {
        // ── 2–4. Wait, poll, read appended complete lines ─────────────────
        let (bytes, truncated) = match reader.next_step().await? {
            TailStep::Exit(exit) => return Ok(exit),
            TailStep::Appended { bytes, truncated } => (bytes, truncated),
        };
        if truncated {
            workflow_check_buf.clear();
            did_workflow_recheck = false;
        }
        let mut rows_landed = 0usize;
        // Session-repository Phase 2: the tailer gets the whole batch this
        // wake consumed (`bytes`) — ONE outbox append, one durable offset
        // reservation per wake instead of one per line.
        for line in bytes.split_inclusive('\n') {
            // Buffer the first 5 lines for the workflow re-check.
            if !did_workflow_recheck && workflow_check_buf.lines().count() < 5 {
                workflow_check_buf.push_str(line);
            }

            match parse_line_for_touched_files(&session_id, line) {
                Ok(touched) => {
                    for tf in touched {
                        match pg
                            .record_file_touched(&session_id, &tf.file_path, None)
                            .await
                        {
                            Ok(()) => rows_landed += 1,
                            Err(e) => warn!(
                                "transcript_watcher: record_file_touched({}, {}) failed: {}",
                                session_id, tf.file_path, e
                            ),
                        }
                    }
                }
                Err(e) => {
                    debug!(
                        "transcript_watcher: malformed JSON line in {}: {}",
                        session_id, e
                    );
                }
            }

            // ── Commit ↔ session lineage push-report (Population path 2) ──
            //
            // Detect `git push` Bash tool_use blocks on this line and, for
            // each, resolve repo/branch/SHAs and enqueue a coord outbox
            // report. Best-effort; skipped entirely when no registrar
            // (outbox) is wired.
            //
            // This used to be one `spawn_blocking` per observation — an
            // UNBOUNDED fan-out onto the shared blocking pool, bounded only
            // by the transcript's line rate. `dispatch_push_observation`
            // replaces it with a bounded queue drained by one private worker
            // thread (see `commit_report`'s "Bounded fan-out" section): the
            // dispatch is non-blocking, the pool is never touched, and a
            // pathological git can cost at most that one thread.
            if let Some(reg) = registrar.as_ref() {
                let pushes = super::commit_report::parse_line_for_pushes(line);
                for obs in pushes {
                    super::commit_report::dispatch_push_observation(obs, reg.clone());
                }

                // Sensitive agent actions (Phase 9): a `tool_use` line parks
                // its classification; the `tool_result` line that proves it
                // succeeded releases it here. The tracker is pure — repo
                // lookup (git) and the outbox write happen on the same
                // bounded worker the push report uses.
                if notify_actions {
                    for detected in action_tracker.observe_line(line, Instant::now()) {
                        // Refusals are WARNed inside, as lost notifications.
                        super::commit_report::dispatch_detected_tool_use(
                            detected,
                            action_lane,
                            reg.clone(),
                        );
                    }
                }
            }

            // ── Interactive-agent log emit (Phase 2) ──────────────────────
            //
            // Stream assistant text + tool_use observations from this
            // transcript line to coord. Pushing is a non-blocking channel
            // send (safe from async); the gate/device-id check already
            // happened in `start()`, so this is a strict no-op when off.
            if let Some(em) = agent_log_emitter.as_ref() {
                for obs in parse_line_for_agent_log(line) {
                    match obs {
                        AgentLogObs::Assistant { text } => {
                            em.emit("info", "assistant", Some(json!({ "text": text })));
                        }
                        AgentLogObs::ToolUse { tool, input } => {
                            let mut payload = json!({ "tool": tool });
                            if let Some(input) = input {
                                payload["input"] = json!(input);
                            }
                            em.emit("info", "tool_use", Some(payload));
                        }
                    }
                }
            }
        }

        // ── 5. Trigger commit-state emit if rows landed ───────────────────
        if rows_landed > 0 {
            crate::mcp::ai_session::emit_commit_state_for_session(
                app_handle.clone(),
                session_id.clone(),
            );
        }

        // ── 6. Workflow-session re-check on first complete wake ───────────
        //
        // Creation-time discovery may race the writer: the file existed at
        // `Create` but the `queue-operation` marker hadn't landed yet.
        // After the first read pass we have the buffered prefix; if the
        // marker is there, this is a workflow session — log and tear down.
        if !did_workflow_recheck && !workflow_check_buf.is_empty() {
            did_workflow_recheck = true;
            if is_workflow_session_marker(&workflow_check_buf) {
                info!(
                    "transcript_watcher: tearing down tail for workflow session {} (detected post-creation)",
                    session_id
                );
                return Ok(TailExit::WorkflowTornDown);
            }
        }

        // ── 7. Session-repository transcript tail (Phase 2) ───────────────
        //
        // Deliberately AFTER the workflow re-check above: a workflow session
        // discovered post-creation returns without ever handing bytes to the
        // tailer, so the runner's own workflow transcripts keep exactly one
        // emitter (the executor's `TranscriptEmitter::emit` call sites) rather
        // than gaining a second, differently-chunked one here.
        //
        // The session id is the JSONL stem — the pane's
        // `claude_code_session_id`, which is exactly the key the registrar's
        // R4 index resolves for the `terminal_claude` plane. Best-effort and
        // synchronous: the tailer's whole write path is a bounded local append
        // that swallows its own errors, so it cannot fail or stall this loop.
        if let Some(t) = tailer.as_ref() {
            t.on_appended(&session_id, &bytes);
        }
    }
}

/// Re-encode a project path for direct lookup. Mirrors
/// `terminal::transcript::encode_project_path` (which is private). Kept tiny
/// + intentional duplication so the watcher doesn't have to re-export
/// internal helpers.
fn encode_for_lookup(project_path: &str) -> String {
    let normalized = project_path
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();
    normalized
        .replace(":/", "--")
        .replace([':', '/', '\\', '_'], "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_for_lookup_matches_transcript_encoder() {
        // Spot-check that this watcher's encoder produces the same path
        // shape as the canonical encoder in `transcript::encode_project_path`.
        // We compare against a known-good output rather than calling the
        // private function.
        assert_eq!(
            encode_for_lookup("C:/Users/jspin/Documents/qontinui_parent"),
            "C--Users-jspin-Documents-qontinui-parent"
        );
        assert_eq!(
            encode_for_lookup("C:\\Users\\jspin\\Documents\\qontinui_parent"),
            "C--Users-jspin-Documents-qontinui-parent"
        );
    }

    #[test]
    fn test_path_looks_like_workflow_session_missing_file() {
        // Missing files must not blow up; treat as not-workflow and let the
        // in-task re-check decide later.
        let nope = std::path::Path::new("C:/this/path/does/not/exist.jsonl");
        assert!(!path_looks_like_workflow_session(nope));
    }

    #[test]
    fn test_path_looks_like_workflow_session_detects_marker() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-tw-test-{}.jsonl", std::process::id()));
        std::fs::write(
            &tmp,
            r#"{"type":"system","subtype":"queue-operation","timestamp":"2026-05-09T00:00:00Z"}
{"type":"user","uuid":"u1","timestamp":"2026-05-09T00:00:01Z","message":{"role":"user","content":"hi"}}
"#,
        )
        .unwrap();
        assert!(path_looks_like_workflow_session(&tmp));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_path_looks_like_workflow_session_interactive_passes() {
        let tmp = std::env::temp_dir().join(format!(
            "qontinui-tw-test-interactive-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &tmp,
            r#"{"type":"user","uuid":"u1","timestamp":"2026-05-09T00:00:01Z","message":{"role":"user","content":"hi"}}
"#,
        )
        .unwrap();
        assert!(!path_looks_like_workflow_session(&tmp));
        let _ = std::fs::remove_file(&tmp);
    }

    // ── Phase 2: the fallback-tick ladder (pure) ──────────────────────────

    #[test]
    fn next_fallback_tick_is_monotone_across_the_bands() {
        let fresh = next_fallback_tick(Duration::ZERO, 0.0);
        let mid = next_fallback_tick(Duration::from_secs(60), 0.0);
        let slow = next_fallback_tick(Duration::from_secs(3600), 0.0);
        assert_eq!(fresh, Duration::from_secs(1));
        assert_eq!(mid, Duration::from_secs(5));
        assert_eq!(slow, Duration::from_secs(30));
        assert!(fresh < mid && mid < slow);
    }

    #[test]
    fn next_fallback_tick_band_edges() {
        // Just under 30 s idle is still the 1 s base; exactly 30 s steps up.
        assert_eq!(
            next_fallback_tick(Duration::from_secs(30) - Duration::from_millis(1), 0.0),
            Duration::from_secs(1)
        );
        assert_eq!(
            next_fallback_tick(Duration::from_secs(30), 0.0),
            Duration::from_secs(5)
        );
        // Just under 5 min is still 5 s; exactly 5 min steps up.
        assert_eq!(
            next_fallback_tick(Duration::from_secs(300) - Duration::from_millis(1), 0.0),
            Duration::from_secs(5)
        );
        assert_eq!(
            next_fallback_tick(Duration::from_secs(300), 0.0),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn next_fallback_tick_is_bounded_to_the_jitter_band() {
        for (idle_for, base) in [
            (Duration::ZERO, 1.0),
            (Duration::from_secs(60), 5.0),
            (Duration::from_secs(3600), 30.0),
        ] {
            let lo = next_fallback_tick(idle_for, -1.0).as_secs_f64();
            let hi = next_fallback_tick(idle_for, 1.0).as_secs_f64();
            assert!((lo - 0.8 * base).abs() < 1e-9, "lo={lo} base={base}");
            assert!((hi - 1.2 * base).abs() < 1e-9, "hi={hi} base={base}");
            // Every jitter in [-1, 1] lands inside [0.8, 1.2] × base.
            for step in 0..=20 {
                let j = -1.0 + 0.1 * step as f64;
                let t = next_fallback_tick(idle_for, j).as_secs_f64();
                assert!(
                    t >= 0.8 * base - 1e-9 && t <= 1.2 * base + 1e-9,
                    "j={j} t={t}"
                );
            }
        }
    }

    #[test]
    fn next_fallback_tick_clamps_out_of_range_and_non_finite_jitter() {
        let base = Duration::from_secs(1);
        assert_eq!(next_fallback_tick(Duration::ZERO, 7.0), base.mul_f64(1.2));
        assert_eq!(next_fallback_tick(Duration::ZERO, -7.0), base.mul_f64(0.8));
        assert_eq!(next_fallback_tick(Duration::ZERO, f64::NAN), base);
        assert_eq!(next_fallback_tick(Duration::ZERO, f64::INFINITY), base);
    }

    #[test]
    fn random_jitter_is_within_the_unit_band() {
        for _ in 0..1000 {
            let j = random_jitter();
            assert!((-1.0..1.0).contains(&j), "j={j}");
        }
    }

    // ── The cohort-wake detector (pure over an injected clock) ────────────

    #[test]
    fn cohort_detector_reports_once_per_window_with_first_three_ids() {
        let t0 = Instant::now();
        let mut d = CohortWakeDetector::new(3, t0);
        assert_eq!(d.observe("a", t0), None);
        assert_eq!(d.observe("b", t0), None);
        assert_eq!(d.observe("c", t0), None);
        let crossed = d
            .observe("d", t0)
            .expect("fourth wake crosses a threshold of 3");
        assert_eq!(crossed.count, 4);
        assert!(crossed.newly_crossed);
        assert_eq!(crossed.first_ids, vec!["a", "b", "c"]);
        let grown = d.observe("e", t0 + Duration::from_millis(100)).unwrap();
        assert_eq!(grown.count, 5);
        assert!(
            !grown.newly_crossed,
            "the crossing is reported exactly once per window"
        );
        assert_eq!(grown.first_ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn cohort_detector_resets_after_the_window_elapses() {
        let t0 = Instant::now();
        let mut d = CohortWakeDetector::new(2, t0);
        d.observe("a", t0);
        d.observe("b", t0);
        assert!(d.observe("c", t0 + Duration::from_millis(200)).is_some());
        // Past the 250 ms window: a fresh count, no report.
        let later = t0 + Duration::from_millis(600);
        assert_eq!(d.observe("z", later), None);
        assert_eq!(d.observe("y", later), None);
        let again = d.observe("x", later).unwrap();
        assert!(again.newly_crossed);
        assert_eq!(again.first_ids, vec!["z", "y", "x"]);
    }

    #[test]
    fn cohort_detector_never_fires_below_the_shipped_threshold() {
        let t0 = Instant::now();
        let mut d = CohortWakeDetector::new(COHORT_WAKE_THRESHOLD, t0);
        for i in 0..COHORT_WAKE_THRESHOLD {
            assert_eq!(d.observe(&format!("s{i}"), t0), None);
        }
        assert!(d.observe("one-more", t0).unwrap().newly_crossed);
    }

    // ── The registry: Live ↔ Parked transitions, gated on the task id ─────

    fn handle(id: u64) -> TailHandle {
        TailHandle {
            wake: Arc::new(Notify::new()),
            cancel: Arc::new(Notify::new()),
            id,
        }
    }

    fn is_parked_at(entry: Option<&TailEntry>, want: u64) -> bool {
        matches!(entry, Some(TailEntry::Parked { cursor, .. }) if *cursor == want)
    }

    #[test]
    fn registry_parks_only_the_current_task_and_removes_only_the_current_task() {
        let now = Instant::now();
        let mut r = TailRegistry::default();
        r.insert_live("s".into(), handle(1));
        assert!(matches!(r.get("s"), Some(TailEntry::Live(h)) if h.id == 1));

        // A stale task (id 0) cannot park over the live entry.
        assert!(!r.park_if_current("s", 0, PathBuf::from("x"), 10, now));
        assert!(matches!(r.get("s"), Some(TailEntry::Live(_))));

        // The current task parks its cursor in place.
        assert!(r.park_if_current("s", 1, PathBuf::from("x"), 42, now));
        assert!(is_parked_at(r.get("s"), 42));

        // Once parked, neither park nor remove by task id touches it — only
        // `remove` (the `Remove` arm) or the sweep do.
        assert!(!r.park_if_current("s", 1, PathBuf::from("x"), 99, now));
        assert!(!r.remove_if_current("s", 1));
        assert!(is_parked_at(r.get("s"), 42));

        // A revive replaces Parked with a new Live; the old task's exit is
        // now a no-op against the new entry.
        r.insert_live("s".into(), handle(2));
        assert!(!r.remove_if_current("s", 1));
        assert!(matches!(r.get("s"), Some(TailEntry::Live(h)) if h.id == 2));
        assert!(r.remove_if_current("s", 2));
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn registry_remove_drops_parked_and_returns_live_for_cancel() {
        let now = Instant::now();
        let mut r = TailRegistry::default();
        r.insert_live("live".into(), handle(1));
        r.insert_live("parked".into(), handle(2));
        assert!(r.park_if_current("parked", 2, PathBuf::from("p"), 7, now));
        assert!(matches!(
            r.remove("parked"),
            Some(TailEntry::Parked { cursor: 7, .. })
        ));
        assert!(matches!(r.remove("live"), Some(TailEntry::Live(h)) if h.id == 1));
        assert!(r.remove("gone").is_none());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn registry_sweep_drops_only_parked_entries_past_retention() {
        let t0 = Instant::now();
        let retention = Duration::from_secs(100);
        let mut r = TailRegistry::default();
        r.insert_live("live".into(), handle(1));
        r.insert_live("old".into(), handle(2));
        r.insert_live("fresh".into(), handle(3));
        assert!(r.park_if_current("old", 2, PathBuf::from("o"), 1, t0));
        assert!(r.park_if_current(
            "fresh",
            3,
            PathBuf::from("f"),
            2,
            t0 + Duration::from_secs(50)
        ));

        let dropped = r.sweep_parked(retention, t0 + Duration::from_secs(120));
        assert_eq!(dropped, 1, "only the entry parked ≥ retention ago goes");
        assert!(r.get("old").is_none());
        assert!(is_parked_at(r.get("fresh"), 2));
        assert!(matches!(r.get("live"), Some(TailEntry::Live(_))));

        // A zero-length retention is not a special case: it drops every
        // parked entry on the next sweep.
        assert_eq!(
            r.sweep_parked(Duration::ZERO, t0 + Duration::from_secs(121)),
            1
        );
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn health_snapshot_serves_every_gauge_key() {
        let v = health_snapshot();
        for key in [
            "tailsLive",
            "tailsParked",
            "tailsStartedSinceBoot",
            "tailsEndedSinceBoot",
        ] {
            assert!(v[key].is_u64(), "{key} must be a counter, got {}", v[key]);
        }
        // Either no cohort wake yet (null) or a well-formed record — never an
        // empty object, never absent.
        let lcw = &v["lastCohortWake"];
        assert!(
            lcw.is_null() || (lcw["at"].is_string() && lcw["count"].is_u64()),
            "lastCohortWake = {lcw}"
        );
    }

    // ── Phase 1 regression: idle exit + cursor-preserving revive ──────────
    //
    // The plan's acceptance test, pinned on the `TailReader` seam (the
    // wait/poll/read/idle half of `tail_session`, which is everything that
    // touches the cursor — `tail_session` only consumes the lines it yields).
    // BOTH halves in one test on purpose: an idle exit that lost the cursor
    // and a revive that re-read from 0 are each the 2026-09-04 shape, a fix
    // that closes one door and opens another.

    const USER_LINE: &str = r#"{"type":"user","uuid":"u1","timestamp":"2026-09-21T00:00:00Z","message":{"role":"user","content":"hi"}}
"#;
    const ASSISTANT_LINE: &str = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-09-21T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}
"#;

    async fn open_reader(path: &Path, start: TailStart, idle_timeout: Duration) -> TailReader {
        TailReader::open(
            path.file_stem().unwrap().to_string_lossy().to_string(),
            path.to_path_buf(),
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
            start,
            idle_timeout,
        )
        .await
        .expect("open")
    }

    #[tokio::test]
    async fn idle_tails_park_with_their_cursor_and_a_revive_reads_exactly_the_append() {
        let dir = tempfile::tempdir().unwrap();
        let idle_timeout = Duration::from_secs(1);

        // N = 5 tails on files that already hold one line, started at EOF —
        // the shape of startup discovery.
        let mut paths = Vec::new();
        for i in 0..5 {
            let p = dir.path().join(format!("session-{i}.jsonl"));
            std::fs::write(&p, USER_LINE).unwrap();
            paths.push(p);
        }
        let initial_len = USER_LINE.len() as u64;

        let mut joins = Vec::new();
        for p in &paths {
            let mut reader = open_reader(p, TailStart::Eof, idle_timeout).await;
            joins.push(tokio::spawn(async move { reader.next_step().await }));
        }
        let all = tokio::time::timeout(Duration::from_secs(3), async {
            let mut out = Vec::new();
            for j in joins {
                out.push(j.await.unwrap().unwrap());
            }
            out
        })
        .await
        .expect("every tail must park within 3 s of a 1 s idle timeout");
        assert_eq!(all.len(), 5);
        for step in &all {
            assert_eq!(
                *step,
                TailStep::Exit(TailExit::Idle {
                    cursor: initial_len
                }),
                "an idle exit carries the cursor at EOF of the initial content"
            );
        }

        // Append ONE well-formed line to one file, then revive from the
        // parked cursor.
        let target = &paths[2];
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(target)
                .unwrap();
            f.write_all(ASSISTANT_LINE.as_bytes()).unwrap();
            f.flush().unwrap();
        }
        let mut revived = open_reader(target, TailStart::At(initial_len), Duration::ZERO).await;
        let step = tokio::time::timeout(Duration::from_secs(3), revived.next_step())
            .await
            .expect("the revived tail must deliver within 3 s")
            .unwrap();
        assert_eq!(
            step,
            TailStep::Appended {
                bytes: ASSISTANT_LINE.to_string(),
                truncated: false,
            },
            "exactly the appended line: not re-read from 0, not skipped past"
        );
        assert_eq!(
            revived.cursor(),
            initial_len + ASSISTANT_LINE.len() as u64,
            "the cursor advanced by exactly the append"
        );
    }

    #[tokio::test]
    async fn zero_idle_timeout_never_parks() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("never.jsonl");
        std::fs::write(&p, USER_LINE).unwrap();
        let mut reader = open_reader(&p, TailStart::Eof, Duration::ZERO).await;
        // Two full base ticks (+ jitter) with nothing appended: still waiting.
        let r = tokio::time::timeout(Duration::from_millis(2500), reader.next_step()).await;
        assert!(
            r.is_err(),
            "with idle_timeout = 0 the reader must still be waiting"
        );
    }

    #[tokio::test]
    async fn a_wake_delivers_the_append_before_the_fallback_tick_and_resets_idle() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("woken.jsonl");
        std::fs::write(&p, USER_LINE).unwrap();
        let wake = Arc::new(Notify::new());
        let mut reader = TailReader::open(
            "woken".into(),
            p.clone(),
            wake.clone(),
            Arc::new(Notify::new()),
            TailStart::Eof,
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
            f.write_all(ASSISTANT_LINE.as_bytes()).unwrap();
        }
        wake.notify_one();
        let started = Instant::now();
        let step = reader.next_step().await.unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(700),
            "a wake must not wait for the 0.8–1.2 s fallback tick (took {:?})",
            started.elapsed()
        );
        assert_eq!(
            step,
            TailStep::Appended {
                bytes: ASSISTANT_LINE.to_string(),
                truncated: false
            }
        );
        assert!(
            reader.idle_exit(Instant::now()).is_none(),
            "growth resets the idle clock"
        );
    }

    #[tokio::test]
    async fn cancel_exits_immediately_with_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cancelled.jsonl");
        std::fs::write(&p, USER_LINE).unwrap();
        let cancel = Arc::new(Notify::new());
        let mut reader = TailReader::open(
            "cancelled".into(),
            p,
            Arc::new(Notify::new()),
            cancel.clone(),
            TailStart::Eof,
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        cancel.notify_one();
        let step = tokio::time::timeout(Duration::from_millis(500), reader.next_step())
            .await
            .expect("cancel is immediate")
            .unwrap();
        assert_eq!(step, TailStep::Exit(TailExit::Cancelled));
    }

    #[tokio::test]
    async fn start_reads_existing_content_and_a_partial_line_is_held_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("start.jsonl");
        let partial = r#"{"type":"assistant","uuid":"a2""#;
        std::fs::write(&p, format!("{USER_LINE}{partial}")).unwrap();
        let mut reader = open_reader(&p, TailStart::Start, Duration::ZERO).await;
        let step = tokio::time::timeout(Duration::from_secs(3), reader.next_step())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            step,
            TailStep::Appended {
                bytes: USER_LINE.to_string(),
                truncated: false
            },
            "only the fully-terminated line is delivered"
        );
        assert_eq!(
            reader.cursor(),
            USER_LINE.len() as u64,
            "the cursor stops before the partial line so it is re-read once terminated"
        );
    }

    /// A transcript whose last line is unterminated — a session killed
    /// mid-write, exactly the dead file the park exists for — has bytes past
    /// the cursor on EVERY poll. Growth is measured against the last length
    /// seen, not against the cursor, so that file still idles and parks with
    /// its cursor before the fragment. (Finding 2 of the 2026-09-21 review:
    /// the first cut reset the idle clock on `len != cursor`, and such a file
    /// polled at the base tick forever.)
    #[tokio::test]
    async fn a_file_ending_in_a_partial_line_still_idles_and_parks_before_the_fragment() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("partial-tail.jsonl");
        let partial = r#"{"type":"assistant","uuid":"a3""#;
        std::fs::write(&p, format!("{USER_LINE}{partial}")).unwrap();
        let mut reader = open_reader(&p, TailStart::Start, Duration::from_secs(1)).await;
        // The complete line is delivered once...
        let step = tokio::time::timeout(Duration::from_secs(3), reader.next_step())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(step, TailStep::Appended { .. }));
        // ...and then, with only the fragment left, the reader parks within
        // the idle budget instead of re-reading the fragment every tick.
        let step = tokio::time::timeout(Duration::from_secs(3), reader.next_step())
            .await
            .expect("the reader must idle out, not poll the fragment forever")
            .unwrap();
        assert_eq!(
            step,
            TailStep::Exit(TailExit::Idle {
                cursor: USER_LINE.len() as u64
            }),
            "parked at the cursor BEFORE the unterminated fragment"
        );
    }

    #[tokio::test]
    async fn a_parked_cursor_past_a_rewritten_file_resets_to_zero_and_flags_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rewritten.jsonl");
        std::fs::write(&p, USER_LINE).unwrap();
        // Parked at 500 (a longer earlier life), file since rewritten shorter.
        let mut reader = open_reader(&p, TailStart::At(500), Duration::ZERO).await;
        let step = tokio::time::timeout(Duration::from_secs(3), reader.next_step())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            step,
            TailStep::Appended {
                bytes: USER_LINE.to_string(),
                truncated: true
            }
        );
        assert_eq!(reader.cursor(), USER_LINE.len() as u64);
    }

    #[tokio::test]
    async fn three_consecutive_metadata_failures_park_instead_of_polling_forever() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("deleted.jsonl");
        std::fs::write(&p, USER_LINE).unwrap();
        // idle_timeout = 0 so the ONLY way out is the metadata streak.
        let mut reader = open_reader(&p, TailStart::Eof, Duration::ZERO).await;
        std::fs::remove_file(&p).unwrap();
        // Three ticks at ≤ 1.2 s each.
        let step = tokio::time::timeout(Duration::from_secs(5), reader.next_step())
            .await
            .expect("three failed polls must park the tail within 5 s")
            .unwrap();
        assert_eq!(
            step,
            TailStep::Exit(TailExit::Idle {
                cursor: USER_LINE.len() as u64
            })
        );
    }

    #[test]
    fn tail_config_from_settings_maps_seconds_and_keeps_zero() {
        let cfg = TailConfig::from(&TranscriptWatcherSettings {
            tail_idle_timeout_secs: 0,
            tail_parked_retention_secs: 30,
        });
        assert_eq!(cfg.idle_timeout, Duration::ZERO);
        assert_eq!(cfg.parked_retention, Duration::from_secs(30));
        let default = TailConfig::from(&TranscriptWatcherSettings::default());
        assert_eq!(default.idle_timeout, Duration::from_secs(600));
        assert_eq!(default.parked_retention, Duration::from_secs(86_400));
    }
}
