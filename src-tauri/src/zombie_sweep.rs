//! Periodic background detection of potentially stale task runs.
//!
//! Task runs can get stuck as "running" in the database when:
//! - The runner crashes and task status never gets updated
//! - A Claude CLI session exits unexpectedly (OOM, killed, etc.)
//! - The startup resume mechanism fails silently
//! - A scheduler-triggered workflow loses its spawning context
//!
//! This module runs a periodic sweep that detects orphaned tasks (tasks marked
//! as running in the DB but with no active Claude CLI process or in-memory
//! session handle) and notifies the user after a grace period.
//!
//! # Auto-fail cases
//!
//! A task is automatically marked `failed` when any of these hold and the task
//! has no live session:
//!
//! - `sessions_count == 0` and orphaned for `ZERO_SESSION_AUTO_STOP` (5 min) —
//!   the task never started AI work, so there is nothing to lose.
//! - Running for longer than `MAX_RUNTIME_AUTO_STOP` (24 h) — any legitimate
//!   workflow should have completed, been manually stopped, or emitted recent
//!   activity before then. This is the safety net that catches long-forgotten
//!   zombies (e.g. a spec-chat row left running for a month).
//!
//! Otherwise (session_count > 0, runtime < 24h, no live session) we emit a
//! one-shot `stale-task-detected` event and let the user decide — active
//! workflows with unregistered sessions are rare but possible, and quietly
//! failing them would destroy in-flight work.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Serialize;
use tauri::AppHandle;
use tauri::Emitter;
use tracing::{error, info, warn};

use crate::claude_session::manager::SessionManager;
use crate::database::pg::PgDb;
use crate::doctor::strategies::is_process_alive;

/// How often to run the sweep.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Delay before the first sweep, allowing startup resume to complete.
const STARTUP_GRACE: Duration = Duration::from_secs(45);

/// How long a task must be orphaned before we notify the user.
const ORPHAN_GRACE: Duration = Duration::from_secs(90);

/// How long a task with 0 sessions must be orphaned before auto-stop.
/// These tasks never started AI work, so it's safe to clean them up.
const ZERO_SESSION_AUTO_STOP: Duration = Duration::from_secs(300);

/// Hard cap on how long a task may remain `running` in the DB before we
/// auto-fail it regardless of session count. Any legitimate workflow should
/// finish, be stopped, or update within this window.
const MAX_RUNTIME_AUTO_STOP: Duration = Duration::from_secs(60 * 60 * 24);

/// Payload emitted to the frontend when a potentially stale task is detected.
#[derive(Debug, Clone, Serialize)]
struct StaleTaskDetectedPayload {
    task_run_id: String,
    task_name: String,
    message: String,
}

/// Tracks when each task was first observed as orphaned.
///
/// A task must remain orphaned for `ORPHAN_GRACE` before notification,
/// preventing false positives during brief session transitions.
struct OrphanTracker {
    first_seen: HashMap<String, Instant>,
    /// Tasks we've already notified about — avoids spamming the same toast every sweep.
    /// Cleared when a task regains a live session or leaves the running set.
    notified: HashSet<String>,
}

impl OrphanTracker {
    fn new() -> Self {
        Self {
            first_seen: HashMap::new(),
            notified: HashSet::new(),
        }
    }

    /// Record that a task was observed as orphaned.
    /// Returns the duration since it was first observed.
    fn observe(&mut self, task_id: &str) -> Duration {
        let now = Instant::now();
        let first = self.first_seen.entry(task_id.to_string()).or_insert(now);
        now.duration_since(*first)
    }

    /// Clear tracking for a task (it has a live session again).
    fn clear(&mut self, task_id: &str) {
        self.first_seen.remove(task_id);
        self.notified.remove(task_id);
    }

    fn was_notified(&self, task_id: &str) -> bool {
        self.notified.contains(task_id)
    }

    fn mark_notified(&mut self, task_id: &str) {
        self.notified.insert(task_id.to_string());
    }

    /// Remove entries for tasks no longer in the running set.
    fn retain_known(&mut self, running_ids: &HashSet<String>) {
        self.first_seen.retain(|id, _| running_ids.contains(id));
        self.notified.retain(|id| running_ids.contains(id));
    }
}

/// Start the stale task sweep background task.
///
/// The sweep:
/// 1. Claims any NULL-`runner_port` running rows for this runner once, so
///    scheduler-launched orphans (which never had a runner_port assigned)
///    are covered by the normal sweep logic on subsequent ticks.
/// 2. Every `SWEEP_INTERVAL` queries all running task_runs and cross-checks
///    them against the in-memory `SessionManager`. Any `running` row with no
///    live session or alive PID is considered orphaned and either notified
///    (soft) or auto-failed (hard) depending on the rules above.
pub fn start_zombie_sweep(
    pg_db: Arc<PgDb>,
    session_manager: Arc<SessionManager>,
    app_handle: AppHandle,
    my_port: u16,
) {
    tokio::spawn(async move {
        // Defer the first sweep so startup_resume has time to re-register
        // any sessions it reclaims.
        tokio::time::sleep(STARTUP_GRACE).await;

        // Step 1: claim NULL-port orphans left over from previous crashes /
        // scheduler launches. Future sweeps then see them under this port.
        match pg_db.claim_orphaned_task_runs(my_port).await {
            Ok(claimed) if !claimed.is_empty() => {
                info!(
                    "Zombie sweep: claimed {} NULL-port running task_run(s) for runner port {}",
                    claimed.len(),
                    my_port
                );
            }
            Ok(_) => {}
            Err(e) => {
                warn!("Zombie sweep: claim_orphaned_task_runs failed: {}", e);
            }
        }

        let mut tracker = OrphanTracker::new();
        let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
        // Skip the immediate tick — we already waited STARTUP_GRACE.
        ticker.tick().await;

        loop {
            ticker.tick().await;
            if let Err(e) = run_sweep(&pg_db, &session_manager, &app_handle, &mut tracker).await {
                warn!("Zombie sweep iteration failed: {}", e);
            }
        }
    });
}

async fn run_sweep(
    pg_db: &PgDb,
    session_manager: &SessionManager,
    app_handle: &AppHandle,
    tracker: &mut OrphanTracker,
) -> Result<(), String> {
    // Pull everything currently marked running (including NULL-port rows so
    // we still catch orphans even if the claim step failed transiently).
    let running = pg_db.get_running_task_runs(None).await?;

    // Build a set of live session IDs: ones we see in SessionManager AND the
    // backing PID (if any) is still alive. A dead PID behind a registered
    // session handle still counts as orphaned.
    let live_ids: HashSet<String> = session_manager
        .list_all_with_state()
        .into_iter()
        .filter_map(|(task_id, _state, pid)| {
            // pid == 0 means "no pid recorded" — treat as alive to avoid
            // failing interactive sessions that haven't forked a subprocess.
            if pid == 0 || is_process_alive(pid) {
                Some(task_id)
            } else {
                None
            }
        })
        .collect();

    let running_ids: HashSet<String> = running.iter().map(|t| t.id.clone()).collect();
    tracker.retain_known(&running_ids);

    let now_utc = Utc::now();

    for task in &running {
        if live_ids.contains(&task.id) {
            tracker.clear(&task.id);
            continue;
        }

        let orphan_age = tracker.observe(&task.id);
        let db_age = parse_rfc3339_age(&task.created_at, now_utc);

        // Auto-fail: hard runtime cap. Protects against zombies that never
        // re-register a session (e.g. scheduler workflows whose runner died
        // long ago, spec-chats from previous boots).
        if db_age >= MAX_RUNTIME_AUTO_STOP {
            auto_fail(
                pg_db,
                task,
                &format!(
                    "Auto-failed by zombie sweep: running for {} without completion \
                     and no live session",
                    humanize(db_age)
                ),
            )
            .await;
            continue;
        }

        // Auto-fail: never started AI work and has been orphaned long enough
        // that a brief session-registration gap is ruled out.
        if task.sessions_count == 0 && orphan_age >= ZERO_SESSION_AUTO_STOP {
            auto_fail(
                pg_db,
                task,
                "Auto-failed by zombie sweep: no sessions ever started and no \
                 live session after grace period",
            )
            .await;
            continue;
        }

        // Soft notify: let the user decide whether to stop or resume.
        if orphan_age >= ORPHAN_GRACE && !tracker.was_notified(&task.id) {
            let payload = StaleTaskDetectedPayload {
                task_run_id: task.id.clone(),
                task_name: task.task_name.clone(),
                message: format!(
                    "Task has been orphaned for {} with no active session. \
                     It may have crashed — stop it from the runs list if it's \
                     no longer needed.",
                    humanize(orphan_age)
                ),
            };
            if let Err(e) = app_handle.emit("stale-task-detected", &payload) {
                warn!(
                    "Failed to emit stale-task-detected for {}: {}",
                    task.id, e
                );
            } else {
                tracker.mark_notified(&task.id);
                info!(
                    "Zombie sweep: notified user about stale task '{}' ({})",
                    task.task_name, task.id
                );
            }
        }
    }

    Ok(())
}

async fn auto_fail(pg_db: &PgDb, task: &crate::database::types::TaskRun, reason: &str) {
    match pg_db.fail_task_run(&task.id, reason).await {
        Ok(()) => info!(
            "Zombie sweep: auto-failed stale task '{}' ({}): {}",
            task.task_name, task.id, reason
        ),
        Err(e) => error!(
            "Zombie sweep: failed to auto-fail stale task {}: {}",
            task.id, e
        ),
    }
}

/// Parse an RFC3339 timestamp and return how long ago it was.
/// Returns `Duration::ZERO` if the input is malformed or in the future.
fn parse_rfc3339_age(created_at: &str, now: chrono::DateTime<chrono::Utc>) -> Duration {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .ok()
        .map(|dt| {
            let delta = now - dt.with_timezone(&chrono::Utc);
            if delta.num_seconds() > 0 {
                Duration::from_secs(delta.num_seconds() as u64)
            } else {
                Duration::ZERO
            }
        })
        .unwrap_or(Duration::ZERO)
}

/// Human-readable duration: "17m", "3h 22m", "2d 4h".
fn humanize(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        return format!("{}s", secs);
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{}m", mins);
    }
    let hours = mins / 60;
    let rem_mins = mins % 60;
    if hours < 24 {
        return if rem_mins == 0 {
            format!("{}h", hours)
        } else {
            format!("{}h {}m", hours, rem_mins)
        };
    }
    let days = hours / 24;
    let rem_hours = hours % 24;
    if rem_hours == 0 {
        format!("{}d", days)
    } else {
        format!("{}d {}h", days, rem_hours)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_formats_durations() {
        assert_eq!(humanize(Duration::from_secs(5)), "5s");
        assert_eq!(humanize(Duration::from_secs(59)), "59s");
        assert_eq!(humanize(Duration::from_secs(60)), "1m");
        assert_eq!(humanize(Duration::from_secs(90)), "1m");
        assert_eq!(humanize(Duration::from_secs(3599)), "59m");
        assert_eq!(humanize(Duration::from_secs(3600)), "1h");
        assert_eq!(humanize(Duration::from_secs(3660)), "1h 1m");
        assert_eq!(humanize(Duration::from_secs(86400)), "1d");
        assert_eq!(humanize(Duration::from_secs(90000)), "1d 1h");
    }

    #[test]
    fn parse_rfc3339_age_handles_past_present_future() {
        let now = chrono::Utc::now();
        let past = (now - chrono::Duration::minutes(10)).to_rfc3339();
        let future = (now + chrono::Duration::minutes(10)).to_rfc3339();
        assert!(parse_rfc3339_age(&past, now).as_secs() >= 600);
        assert_eq!(parse_rfc3339_age(&future, now), Duration::ZERO);
        assert_eq!(parse_rfc3339_age("not a date", now), Duration::ZERO);
    }

    #[test]
    fn orphan_tracker_retain_known_drops_stale_entries() {
        let mut tracker = OrphanTracker::new();
        tracker.observe("alive");
        tracker.observe("dead");
        tracker.mark_notified("alive");
        tracker.mark_notified("dead");

        let mut keep = HashSet::new();
        keep.insert("alive".to_string());
        tracker.retain_known(&keep);

        assert!(tracker.first_seen.contains_key("alive"));
        assert!(!tracker.first_seen.contains_key("dead"));
        assert!(tracker.was_notified("alive"));
        assert!(!tracker.was_notified("dead"));
    }
}
