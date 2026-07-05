//! Session-tracking health check + untracked-backend-spawn counter (plan
//! `2026-07-03-runner-session-tracking-drift-and-guardrails`, Phase 3 items
//! 1–2).
//!
//! ## Why
//!
//! Three prior "spawned but never durably recorded" fixes each patched one
//! call site; nothing detects the NEXT gap. The 2026-07-03 live audit found
//! 9 of 11 live Claude sessions untracked/mis-tracked — discovered only by a
//! manual multi-hour process-tree cross-reference. This module performs that
//! exact cross-reference automatically on a slow interval:
//!
//! - **live-but-untracked** — a live `claude` process in the runner's own
//!   process subtree that no open [`SessionLifecycleStore`] record accounts
//!   for (a restart would silently drop it), and
//! - **tracked-open-but-dead** — an open record whose terminal is gone or
//!   whose subtree no longer contains a live `claude` (a stale row the
//!   liveness poll should have flipped).
//!
//! ## Legitimate headless planes are EXEMPT
//!
//! Two claude planes legitimately run outside the terminal lifecycle store
//! and must not trip the WARN (false-positive alert fatigue is the exact
//! failure mode this plan fights):
//!
//! 1. **WS `LaunchPayload` agent spawns** (`agent_runtime::run_agent_subprocess`
//!    → `spawn_claude_child`, plus the headless gate-continuation arm) —
//!    direct tokio children with no PTY and no `capture_hint` BY DESIGN.
//!    Their child PIDs are registered here via
//!    [`register_headless_claude_pid`] for the child's lifetime.
//! 2. **Headless AI-session / task-run plane** (`ClaudeSession::spawn`,
//!    inline PIDs, pty workers) — restore rides the task-run DB plane via a
//!    pinned `--session-id <task_run_id>`, not the terminal store. Their PIDs
//!    come from `SessionManager::list_all_with_state()` at check time.
//!
//! Both exemptions are by tracked child PID (each PID's inclusive subtree is
//! subtracted), never by name/cmdline heuristics.
//!
//! The decision core ([`evaluate`]) is a pure function over a
//! [`ProcessSnapshot`] + the open records, so it is unit-testable with a
//! synthetic snapshot and a tempdir store — no real processes needed. Process
//! enumeration REUSES `process_capture::process_tree` (one snapshot per tick,
//! same as the liveness poll); no new enumerator.
//!
//! The latest result is held in a module-global (same idiom as the
//! `auth.rs` data-plane counters) and surfaced on `/health` as the
//! `sessionTracking` object via [`health_json`].

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tracing::{debug, warn};

use crate::process_capture::process_tree::{
    claude_pids_in_inclusive_subtree, claude_present_in_inclusive_subtree, ProcessSnapshot,
};
use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};
use crate::terminal::TerminalManager;

/// How often the periodic cross-reference runs.
const CHECK_INTERVAL: Duration = Duration::from_secs(600);
/// Boot delay before the first check — lets boot-restore re-open its records
/// and the restored PTYs register, so the first tick doesn't report the
/// restore window itself as drift.
const INITIAL_DELAY: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Untracked-backend-spawn counter (Phase 3 item 1)
// ---------------------------------------------------------------------------

/// `session_lifecycle_untracked_backend_spawn_total` — total backend-flavored
/// `create_terminal_session_backend` calls that arrived with
/// `capture_hint: None` (i.e. spawns that will NOT be durably recorded and
/// would be silently lost on restart). Same static-`AtomicU64` idiom as
/// `auth.rs::DATA_PLANE_TOTAL`; surfaced on `/health` via [`health_json`].
static UNTRACKED_BACKEND_SPAWN_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Increment the untracked-backend-spawn counter, returning the new total.
/// The caller owns the accompanying `warn!` (it has the spawn context).
pub fn note_untracked_backend_spawn() -> u64 {
    UNTRACKED_BACKEND_SPAWN_TOTAL.fetch_add(1, Ordering::Relaxed) + 1
}

/// Current value of `session_lifecycle_untracked_backend_spawn_total`.
pub fn untracked_backend_spawn_total() -> u64 {
    UNTRACKED_BACKEND_SPAWN_TOTAL.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Headless-plane exemption registry (agent-runtime children)
// ---------------------------------------------------------------------------

/// Live PIDs of headless `claude` children the agent runtime owns directly
/// (WS `LaunchPayload` spawns + the headless gate-continuation arm). These
/// legitimately have no lifecycle record — the spawn loop registers each
/// child for its lifetime so the health check never flags a normal agent run.
static HEADLESS_CLAUDE_PIDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

fn headless_pids_cell() -> &'static Mutex<HashSet<u32>> {
    HEADLESS_CLAUDE_PIDS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Mark a headless agent-runtime `claude` child as exempt from the
/// live-but-untracked diff. Pair with [`unregister_headless_claude_pid`] when
/// the child exits (a leaked entry is self-limiting — the PID stops matching
/// a live claude once the process dies).
pub fn register_headless_claude_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    if let Ok(mut g) = headless_pids_cell().lock() {
        g.insert(pid);
    }
}

/// Remove a headless child from the exemption set (its process exited).
pub fn unregister_headless_claude_pid(pid: u32) {
    if let Ok(mut g) = headless_pids_cell().lock() {
        g.remove(&pid);
    }
}

/// Snapshot of the currently-registered headless agent-runtime child PIDs.
pub fn headless_claude_pids() -> HashSet<u32> {
    headless_pids_cell()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Health-check report
// ---------------------------------------------------------------------------

/// A live `claude` process in the runner's subtree that no open lifecycle
/// record accounts for — a restart would silently drop this session.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LiveUntrackedProcess {
    pub pid: u32,
    /// Image name from the snapshot (e.g. `claude.exe`), when resolved.
    pub image: Option<String>,
}

/// An open lifecycle record whose terminal is gone or whose subtree contains
/// no live `claude` — a stale row masquerading as a restorable session.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrackedDeadRecord {
    pub claude_session_id: String,
    pub terminal_id: String,
    pub title: Option<String>,
}

/// Result of one cross-reference pass.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackingHealthReport {
    /// Unix millis when this pass ran.
    pub checked_at_ms: i64,
    /// Total live `claude` processes found in the runner's inclusive subtree.
    pub live_claude_total: usize,
    /// Total open lifecycle records at check time.
    pub tracked_open_total: usize,
    pub live_untracked: Vec<LiveUntrackedProcess>,
    pub tracked_dead: Vec<TrackedDeadRecord>,
}

impl TrackingHealthReport {
    pub fn is_clean(&self) -> bool {
        self.live_untracked.is_empty() && self.tracked_dead.is_empty()
    }
}

/// Latest completed report — `/health` reads this; the periodic task is the
/// sole writer. `None` until the first pass completes.
static LATEST: OnceLock<Mutex<Option<TrackingHealthReport>>> = OnceLock::new();

fn latest_cell() -> &'static Mutex<Option<TrackingHealthReport>> {
    LATEST.get_or_init(|| Mutex::new(None))
}

/// Clone of the most recent report, if any pass has completed.
pub fn latest() -> Option<TrackingHealthReport> {
    latest_cell().lock().ok().and_then(|g| g.clone())
}

fn store_latest(report: TrackingHealthReport) {
    if let Ok(mut g) = latest_cell().lock() {
        *g = Some(report);
    }
}

/// The `/health` `sessionTracking` object: last run timestamp + drift counts
/// (fields null before the first pass), plus the untracked-backend-spawn
/// counter (always live — it counts spawns, not checks).
pub fn health_json() -> serde_json::Value {
    let counter = untracked_backend_spawn_total();
    match latest() {
        Some(r) => serde_json::json!({
            "lastCheckAt": r.checked_at_ms,
            "liveClaudeTotal": r.live_claude_total,
            "trackedOpenTotal": r.tracked_open_total,
            "liveUntracked": r.live_untracked.len(),
            "trackedDead": r.tracked_dead.len(),
            "liveUntrackedDetail": r.live_untracked,
            "trackedDeadDetail": r.tracked_dead,
            "untrackedBackendSpawnsTotal": counter,
        }),
        None => serde_json::json!({
            "lastCheckAt": serde_json::Value::Null,
            "liveUntracked": serde_json::Value::Null,
            "trackedDead": serde_json::Value::Null,
            "untrackedBackendSpawnsTotal": counter,
        }),
    }
}

// ---------------------------------------------------------------------------
// Pure decision core
// ---------------------------------------------------------------------------

/// Cross-reference one process snapshot against the open lifecycle records.
///
/// Pure over its inputs (testable with a synthetic snapshot — no real
/// processes):
///
/// - `runner_pid` roots the "live claude" universe: every claude-image PID in
///   its inclusive subtree.
/// - `terminal_pids` maps each LIVE terminal id to its PTY child PID (from
///   `TerminalManager::list()`); a record whose terminal id is absent has no
///   live PTY.
/// - A record is **accounted** when its terminal's subtree has a live claude
///   (per `claude_present_in_inclusive_subtree`, including its PID-reuse
///   guard against `opened_at`); those claude PIDs are subtracted from the
///   live set. Everything left over is **live-but-untracked**; every open
///   record that is not accounted is **tracked-open-but-dead**.
/// - `exempt_root_pids` are the legitimate headless planes (agent-runtime
///   children + AI-session/task-run children — see module docs): each root's
///   inclusive-subtree claude PIDs are pre-accounted so a normal agent run
///   never reports drift.
pub fn evaluate(
    snapshot: &ProcessSnapshot,
    runner_pid: u32,
    open_records: &[TerminalSessionRecord],
    terminal_pids: &HashMap<String, u32>,
    exempt_root_pids: &HashSet<u32>,
    now_ms: i64,
) -> TrackingHealthReport {
    let live_claude: Vec<u32> = claude_pids_in_inclusive_subtree(runner_pid, snapshot);

    let mut accounted: HashSet<u32> = HashSet::new();
    for &root in exempt_root_pids {
        accounted.extend(claude_pids_in_inclusive_subtree(root, snapshot));
    }
    let mut tracked_dead: Vec<TrackedDeadRecord> = Vec::new();

    for rec in open_records {
        let alive = match terminal_pids.get(&rec.terminal_id) {
            Some(&pid) => {
                let present = claude_present_in_inclusive_subtree(pid, snapshot, rec.opened_at);
                if present {
                    accounted.extend(claude_pids_in_inclusive_subtree(pid, snapshot));
                }
                present
            }
            // No live terminal hosts this record at all.
            None => false,
        };
        if !alive {
            tracked_dead.push(TrackedDeadRecord {
                claude_session_id: rec.claude_session_id.clone(),
                terminal_id: rec.terminal_id.clone(),
                title: rec.title.clone(),
            });
        }
    }

    let live_untracked: Vec<LiveUntrackedProcess> = live_claude
        .iter()
        .filter(|pid| !accounted.contains(pid))
        .map(|&pid| LiveUntrackedProcess {
            pid,
            image: snapshot.names.get(&pid).cloned(),
        })
        .collect();

    TrackingHealthReport {
        checked_at_ms: now_ms,
        live_claude_total: live_claude.len(),
        tracked_open_total: open_records.len(),
        live_untracked,
        tracked_dead,
    }
}

// ---------------------------------------------------------------------------
// Periodic task
// ---------------------------------------------------------------------------

/// One live pass: snapshot the process table, cross-reference, log + publish.
/// Skips (keeping the previous result) when the snapshot helper failed — an
/// empty parent map means "couldn't see the process table", not "no drift"
/// (same posture as the liveness poll's `tick_snapshot_ok`).
async fn run_once(
    terminal_manager: &Arc<TerminalManager>,
    store: &Arc<SessionLifecycleStore>,
    session_manager: &Arc<crate::claude_session::SessionManager>,
) {
    let snap = crate::process_capture::process_tree::snapshot_process_table_public().await;
    if snap.parent_map.is_empty() {
        debug!("session tracking health: process snapshot unavailable — skipping this pass");
        return;
    }

    let open = store.open_records();
    let terminal_pids: HashMap<String, u32> = terminal_manager
        .list()
        .into_iter()
        .filter_map(|i| i.pid.filter(|&p| p > 0).map(|p| (i.id, p)))
        .collect();

    // Exempt planes (module docs): agent-runtime headless children (registry)
    // + the AI-session/task-run plane (ClaudeSessions, inline PIDs, workers).
    let mut exempt: HashSet<u32> = headless_claude_pids();
    exempt.extend(
        session_manager
            .list_all_with_state()
            .into_iter()
            .map(|(_, _, pid)| pid)
            .filter(|&p| p > 0),
    );

    let report = evaluate(
        &snap,
        std::process::id(),
        &open,
        &terminal_pids,
        &exempt,
        chrono::Utc::now().timestamp_millis(),
    );

    if report.is_clean() {
        debug!(
            live_claude = report.live_claude_total,
            tracked_open = report.tracked_open_total,
            "session tracking health: clean"
        );
    } else {
        let untracked_pids: Vec<u32> = report.live_untracked.iter().map(|p| p.pid).collect();
        let dead_sessions: Vec<&str> = report
            .tracked_dead
            .iter()
            .map(|d| d.claude_session_id.as_str())
            .collect();
        warn!(
            live_untracked = report.live_untracked.len(),
            tracked_dead = report.tracked_dead.len(),
            live_claude_total = report.live_claude_total,
            tracked_open_total = report.tracked_open_total,
            untracked_pids = ?untracked_pids,
            dead_sessions = ?dead_sessions,
            "session tracking health: DRIFT — live claude sessions not durably tracked \
             (would be lost on restart) and/or stale open records"
        );
    }

    store_latest(report);
}

/// Detached periodic health check — spawned once at startup alongside the
/// liveness poll (main.rs setup). Runs forever; every pass is fail-open.
pub async fn run_periodic(
    terminal_manager: Arc<TerminalManager>,
    store: Arc<SessionLifecycleStore>,
    session_manager: Arc<crate::claude_session::SessionManager>,
) {
    tokio::time::sleep(INITIAL_DELAY).await;
    loop {
        run_once(&terminal_manager, &store, &session_manager).await;
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn snap_with(
        parent_map: &[(u32, &[u32])],
        creation: &[(u32, i64)],
        names: &[(u32, &str)],
    ) -> ProcessSnapshot {
        let mut s = ProcessSnapshot::default();
        for (p, kids) in parent_map {
            s.parent_map.insert(*p, kids.to_vec());
        }
        for (pid, t) in creation {
            s.creation_times.insert(*pid, *t);
        }
        for (pid, n) in names {
            s.names.insert(*pid, n.to_string());
        }
        s
    }

    fn record(claude_session_id: &str, terminal_id: &str, opened_at: i64) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: claude_session_id.to_string(),
            config_dir: None,
            working_dir: Some("D:/work".to_string()),
            page_id: "default".to_string(),
            zone_index: 0,
            title: Some(format!("title-{claude_session_id}")),
            terminal_id: terminal_id.to_string(),
            opened_at,
            last_seen_at: opened_at,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: "claude".to_string(),
            origin: None,
            restore_pending_at: None,
            confirmed_at: None,
        }
    }

    /// Reusable tempdir-based store harness: open records flow through a real
    /// `SessionLifecycleStore` so the test exercises the same read path
    /// (`open_records`) the live task uses.
    fn store_with(
        records: &[TerminalSessionRecord],
    ) -> (Arc<SessionLifecycleStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        for rec in records {
            store.record_open(rec.clone());
        }
        (store, dir)
    }

    /// Fixture: runner (pid 1) → shell 5 → claude 10 (tracked, "t-5"), and a
    /// stray claude 20 directly under the runner with no record. Creation
    /// times sit at `now` so the PID-reuse guard is a no-op.
    #[test]
    fn detects_live_untracked_claude() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        let snap = snap_with(
            &[(1, &[5, 20]), (5, &[10])],
            &[(5, now_s), (10, now_s), (20, now_s)],
            &[
                (5, "powershell.exe"),
                (10, "claude.exe"),
                (20, "claude.exe"),
            ],
        );
        let (store, _dir) = store_with(&[record("sess-a", "t-5", now_ms)]);
        let terminal_pids: HashMap<String, u32> = [("t-5".to_string(), 5)].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &store.open_records(),
            &terminal_pids,
            &HashSet::new(),
            now_ms,
        );

        assert_eq!(report.live_claude_total, 2);
        assert_eq!(report.tracked_open_total, 1);
        assert!(report.tracked_dead.is_empty());
        assert_eq!(
            report.live_untracked,
            vec![LiveUntrackedProcess {
                pid: 20,
                image: Some("claude.exe".to_string()),
            }]
        );
        assert!(!report.is_clean());
    }

    /// An open record whose terminal id maps to no live PTY, and one whose
    /// PTY subtree has no claude, are both tracked-open-but-dead.
    #[test]
    fn detects_tracked_open_but_dead() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → bare shell 5 (no claude child anywhere).
        let snap = snap_with(&[(1, &[5])], &[(5, now_s)], &[(5, "powershell.exe")]);
        let (store, _dir) = store_with(&[
            record("sess-gone-terminal", "t-missing", now_ms),
            record("sess-no-claude", "t-5", now_ms),
        ]);
        let terminal_pids: HashMap<String, u32> = [("t-5".to_string(), 5)].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &store.open_records(),
            &terminal_pids,
            &HashSet::new(),
            now_ms,
        );

        assert!(report.live_untracked.is_empty());
        let mut dead: Vec<&str> = report
            .tracked_dead
            .iter()
            .map(|d| d.claude_session_id.as_str())
            .collect();
        dead.sort();
        assert_eq!(dead, vec!["sess-gone-terminal", "sess-no-claude"]);
        assert!(!report.is_clean());
    }

    /// Fully-consistent state: every live claude is accounted for by an open
    /// record, and every open record is alive → clean report. Covers both
    /// session shapes (claude-as-child under a shell, tracked PID *is*
    /// claude).
    #[test]
    fn clean_when_fully_tracked() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        let snap = snap_with(
            &[(1, &[5, 30]), (5, &[10])],
            &[(5, now_s), (10, now_s), (30, now_s)],
            &[(5, "pwsh.exe"), (10, "claude.exe"), (30, "claude.exe")],
        );
        let (store, _dir) = store_with(&[
            record("sess-shell", "t-5", now_ms),
            // Agent shape: the tracked PID itself is claude.
            record("sess-agent", "t-30", now_ms),
        ]);
        let terminal_pids: HashMap<String, u32> =
            [("t-5".to_string(), 5u32), ("t-30".to_string(), 30u32)]
                .into_iter()
                .collect();

        let report = evaluate(
            &snap,
            1,
            &store.open_records(),
            &terminal_pids,
            &HashSet::new(),
            now_ms,
        );

        assert!(report.is_clean(), "unexpected drift: {report:?}");
        assert_eq!(report.live_claude_total, 2);
        assert_eq!(report.tracked_open_total, 2);
    }

    /// Headless-plane exemption: a claude with no lifecycle record whose PID
    /// (or subtree root) is exempt — an agent-runtime child or an
    /// AI-session/task-run child — must NOT be reported as live-untracked. A
    /// second genuinely-untracked claude in the same pass still is.
    #[test]
    fn exempt_headless_planes_do_not_report_drift() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → agent claude 40 (exempt, with a nested claude 41 in its
        // subtree), and a stray claude 50 that is NOT exempt.
        let snap = snap_with(
            &[(1, &[40, 50]), (40, &[41])],
            &[(40, now_s), (41, now_s), (50, now_s)],
            &[(40, "claude.exe"), (41, "claude.exe"), (50, "claude.exe")],
        );
        let (store, _dir) = store_with(&[]);
        let exempt: HashSet<u32> = [40].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &store.open_records(),
            &HashMap::new(),
            &exempt,
            now_ms,
        );

        // 40 and its subtree member 41 are exempt; only 50 remains.
        let untracked: Vec<u32> = report.live_untracked.iter().map(|p| p.pid).collect();
        assert_eq!(untracked, vec![50]);
        assert!(report.tracked_dead.is_empty());
    }

    #[test]
    fn headless_pid_registry_round_trip() {
        register_headless_claude_pid(0); // ignored — 0 means "unknown"
        register_headless_claude_pid(9999);
        assert!(headless_claude_pids().contains(&9999));
        assert!(!headless_claude_pids().contains(&0));
        unregister_headless_claude_pid(9999);
        assert!(!headless_claude_pids().contains(&9999));
    }

    #[test]
    fn untracked_backend_spawn_counter_increments() {
        let before = untracked_backend_spawn_total();
        let after = note_untracked_backend_spawn();
        assert!(after > before);
        assert!(untracked_backend_spawn_total() >= after);
    }

    /// `health_json` always carries the counter; report fields flip from null
    /// to concrete after a pass is stored.
    #[test]
    fn health_json_shape() {
        let v = health_json();
        assert!(v.get("untrackedBackendSpawnsTotal").is_some());

        store_latest(TrackingHealthReport {
            checked_at_ms: 123,
            live_claude_total: 1,
            tracked_open_total: 1,
            live_untracked: vec![],
            tracked_dead: vec![],
        });
        let v = health_json();
        assert_eq!(v["lastCheckAt"], 123);
        assert_eq!(v["liveUntracked"], 0);
        assert_eq!(v["trackedDead"], 0);
    }
}
