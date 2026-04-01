//! Periodic background detection of potentially stale task runs.
//!
//! Task runs can get stuck as "running" in the database when:
//! - The runner crashes and task status never gets updated
//! - A Claude CLI session exits unexpectedly (OOM, killed, etc.)
//! - The startup resume mechanism fails silently
//!
//! This module runs a periodic sweep that detects orphaned tasks (tasks marked
//! as running in the DB but with no active Claude CLI process) and notifies
//! the user after a grace period. It does NOT automatically stop most tasks,
//! since false positives (e.g., inline sessions without SessionManager
//! registration) would destroy legitimate in-progress work.
//!
//! **Exception**: Task runs with `sessions_count == 0` that have been orphaned
//! for `ZERO_SESSION_AUTO_STOP` are automatically marked as failed. These tasks
//! never started any AI work, so there is nothing to lose.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::AppHandle;
use tauri::Emitter;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::claude_session::manager::SessionManager;
use crate::database::CheckpointDb;
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

    /// Check whether we've already notified for this task.
    fn was_notified(&self, task_id: &str) -> bool {
        self.notified.contains(task_id)
    }

    /// Mark a task as notified.
    fn mark_notified(&mut self, task_id: &str) {
        self.notified.insert(task_id.to_string());
    }

    /// Remove entries for tasks no longer in the running set.
    fn retain_known(&mut self, running_ids: &[String]) {
        self.first_seen.retain(|id, _| running_ids.contains(id));
        self.notified.retain(|id| running_ids.contains(id));
    }
}

/// Run a single sweep cycle: detect potentially stale task runs and notify.
async fn sweep_once(
    db: &Arc<CheckpointDb>,
    session_manager: &Arc<SessionManager>,
    app_handle: &AppHandle,
    orphan_tracker: &mut OrphanTracker,
) {
    // 1. Query DB for all running tasks (both workflows and chat sessions)
    let db_clone = db.clone();
    let running_tasks = tokio::task::spawn_blocking(move || {
        let mut tasks: Vec<(String, String, u32)> = Vec::new();

        let port_filter = db_clone.get_runner_port();
        match db_clone.get_running_task_runs(port_filter) {
            Ok(runs) => {
                for run in runs {
                    tasks.push((run.id, run.task_name, run.sessions_count));
                }
            }
            Err(e) => warn!("Stale task sweep: failed to query running task runs: {}", e),
        }

        match db_clone.get_running_ai_sessions(port_filter) {
            Ok(sessions) => {
                for session in sessions {
                    tasks.push((session.id, session.task_name, session.sessions_count));
                }
            }
            Err(e) => warn!(
                "Stale task sweep: failed to query running AI sessions: {}",
                e
            ),
        }

        tasks
    })
    .await
    .unwrap_or_default();

    if running_tasks.is_empty() {
        orphan_tracker.first_seen.clear();
        orphan_tracker.notified.clear();
        return;
    }

    // 2. Get all sessions from the SessionManager (includes both interactive and inline PIDs)
    let sessions = session_manager.list_all_with_state();
    let session_map: HashMap<String, (crate::claude_session::state::SessionState, u32)> = sessions
        .into_iter()
        .map(|(id, state, pid)| (id, (state, pid)))
        .collect();

    // 3. Check each running task
    let running_ids: Vec<String> = running_tasks.iter().map(|(id, _, _)| id.clone()).collect();

    // Collect tasks to auto-stop (avoid borrowing db while iterating)
    let mut auto_stop_tasks: Vec<(String, String)> = Vec::new();

    for (task_id, task_name, sessions_count) in &running_tasks {
        let has_live_session = session_map
            .get(task_id)
            .is_some_and(|(state, pid)| state.is_active() && is_process_alive(*pid));

        if has_live_session {
            // Session is alive — clear any orphan tracking
            orphan_tracker.clear(task_id);
            continue;
        }

        // No live session — track as potentially stale
        let orphan_duration = orphan_tracker.observe(task_id);

        // Auto-stop tasks that never started any AI sessions.
        // These are orphaned task_run records that were created but never executed.
        if *sessions_count == 0
            && orphan_duration >= ZERO_SESSION_AUTO_STOP
            && !orphan_tracker.was_notified(task_id)
        {
            warn!(
                "Stale task sweep: task '{}' (id={}) has 0 sessions after {:.0}s — auto-stopping",
                task_name,
                task_id,
                orphan_duration.as_secs_f64()
            );
            auto_stop_tasks.push((task_id.clone(), task_name.clone()));
            orphan_tracker.mark_notified(task_id);
            continue;
        }

        if orphan_duration >= ORPHAN_GRACE && !orphan_tracker.was_notified(task_id) {
            // Orphaned long enough and not yet notified — warn the user
            warn!(
                "Stale task sweep: task '{}' (id={}) has had no active session for {:.0}s — notifying user",
                task_name,
                task_id,
                orphan_duration.as_secs_f64()
            );

            // Emit notification event for toast (advisory only — does NOT stop the task)
            let payload = StaleTaskDetectedPayload {
                task_run_id: task_id.clone(),
                task_name: task_name.clone(),
                message: "No active session detected — this task may be stale. You can stop it manually if needed.".to_string(),
            };
            if let Err(e) = app_handle.emit("stale-task-detected", &payload) {
                warn!("Stale task sweep: failed to emit event: {}", e);
            }

            orphan_tracker.mark_notified(task_id);
        }
    }

    // 4. Auto-stop zero-session orphans
    if !auto_stop_tasks.is_empty() {
        let db_clone = db.clone();
        let tasks_to_stop = auto_stop_tasks.clone();
        tokio::task::spawn_blocking(move || {
            for (task_id, task_name) in &tasks_to_stop {
                if let Err(e) = db_clone.fail_task_run(
                    task_id,
                    "Auto-stopped: task had no AI sessions after 5 minutes",
                ) {
                    warn!(
                        "Stale task sweep: failed to auto-stop task '{}' (id={}): {}",
                        task_name, task_id, e
                    );
                } else {
                    info!(
                        "Stale task sweep: auto-stopped orphaned task '{}' (id={})",
                        task_name, task_id
                    );
                }
            }
        })
        .await
        .ok();
    }

    // 5. Clean up closed sessions from the SessionManager
    session_manager.cleanup_closed();

    // 6. Prune orphan tracker for tasks no longer running
    orphan_tracker.retain_known(&running_ids);
}

/// Start the stale task sweep background task.
///
/// Waits for `STARTUP_GRACE` before the first sweep, then runs
/// `sweep_once()` every `SWEEP_INTERVAL`.
pub fn start_zombie_sweep(
    session_manager: Arc<SessionManager>,
    app_handle: AppHandle,
) {
    // Zombie sweep: SQLite-dependent. Skipping until PG migration.
    let _ = (session_manager, app_handle);
    info!("Stale task sweep: disabled (SQLite CheckpointDb removed, PG support pending)");
    return;
    #[allow(unreachable_code, clippy::diverging_sub_expression)]
    let db: Arc<CheckpointDb> = unreachable!();
    let orphan_tracker = Arc::new(Mutex::new(OrphanTracker::new()));

    tokio::spawn(async move {
        // Wait for startup resume to complete
        tokio::time::sleep(STARTUP_GRACE).await;
        info!(
            "Stale task sweep: started (interval={}s, orphan_grace={}s)",
            SWEEP_INTERVAL.as_secs(),
            ORPHAN_GRACE.as_secs()
        );

        loop {
            {
                let mut tracker = orphan_tracker.lock().await;
                sweep_once(&db, &session_manager, &app_handle, &mut tracker).await;
            }
            tokio::time::sleep(SWEEP_INTERVAL).await;
        }
    });
}
