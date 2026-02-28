//! Periodic background detection and cleanup of zombie task runs.
//!
//! Task runs can get stuck as "running" in the database when:
//! - The runner crashes and task status never gets updated
//! - A Claude CLI session exits unexpectedly (OOM, killed, etc.)
//! - The startup resume mechanism fails silently
//!
//! This module runs a periodic sweep that detects orphaned tasks (tasks marked
//! as running in the DB but with no active Claude CLI process) and cleans them up
//! after a grace period.

use std::collections::HashMap;
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
use crate::event_system::EventBroadcaster;

/// How often to run the sweep.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Delay before the first sweep, allowing startup resume to complete.
const STARTUP_GRACE: Duration = Duration::from_secs(45);

/// How long a task must be orphaned before it gets cleaned up.
const ORPHAN_GRACE: Duration = Duration::from_secs(90);

/// Payload emitted to the frontend when a zombie task is cleaned up.
#[derive(Debug, Clone, Serialize)]
struct ZombieTaskCleanedPayload {
    task_run_id: String,
    task_name: String,
    message: String,
}

/// Tracks when each task was first observed as orphaned.
///
/// A task must remain orphaned for `ORPHAN_GRACE` before cleanup,
/// preventing false positives during brief session transitions.
struct OrphanTracker {
    first_seen: HashMap<String, Instant>,
}

impl OrphanTracker {
    fn new() -> Self {
        Self {
            first_seen: HashMap::new(),
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
    }

    /// Remove entries for tasks no longer in the running set.
    fn retain_known(&mut self, running_ids: &[String]) {
        self.first_seen.retain(|id, _| running_ids.contains(id));
    }
}

/// Run a single sweep cycle: detect and clean up zombie task runs.
async fn sweep_once(
    db: &Arc<CheckpointDb>,
    session_manager: &Arc<SessionManager>,
    app_handle: &AppHandle,
    orphan_tracker: &mut OrphanTracker,
) {
    // 1. Query DB for all running tasks (both workflows and chat sessions)
    let db_clone = db.clone();
    let running_tasks = tokio::task::spawn_blocking(move || {
        let mut tasks = Vec::new();

        match db_clone.get_running_task_runs() {
            Ok(runs) => {
                for run in runs {
                    tasks.push((run.id, run.task_name));
                }
            }
            Err(e) => warn!("Zombie sweep: failed to query running task runs: {}", e),
        }

        match db_clone.get_running_chat_sessions() {
            Ok(chats) => {
                for chat in chats {
                    tasks.push((chat.id, chat.task_name));
                }
            }
            Err(e) => warn!("Zombie sweep: failed to query running chat sessions: {}", e),
        }

        tasks
    })
    .await
    .unwrap_or_default();

    if running_tasks.is_empty() {
        orphan_tracker.first_seen.clear();
        return;
    }

    // 2. Get all sessions from the SessionManager
    let sessions = session_manager.list_all_with_state();
    let session_map: HashMap<String, (crate::claude_session::state::SessionState, u32)> = sessions
        .into_iter()
        .map(|(id, state, pid)| (id, (state, pid)))
        .collect();

    // 3. Check each running task
    let mut cleaned_ids = Vec::new();
    let running_ids: Vec<String> = running_tasks.iter().map(|(id, _)| id.clone()).collect();

    for (task_id, task_name) in &running_tasks {
        let has_live_session = session_map
            .get(task_id)
            .is_some_and(|(state, pid)| state.is_active() && is_process_alive(*pid));

        if has_live_session {
            // Session is alive — clear any orphan tracking
            orphan_tracker.clear(task_id);
            continue;
        }

        // No live session — track as orphaned
        let orphan_duration = orphan_tracker.observe(task_id);

        if orphan_duration >= ORPHAN_GRACE {
            // Orphaned long enough — clean it up
            warn!(
                "Zombie sweep: cleaning up orphaned task '{}' (id={}, orphaned for {:.0}s)",
                task_name,
                task_id,
                orphan_duration.as_secs_f64()
            );

            // Mark as stopped in DB
            let db_for_stop = db.clone();
            let stop_id = task_id.clone();
            let stop_result =
                tokio::task::spawn_blocking(move || db_for_stop.stop_task_run(&stop_id)).await;

            match stop_result {
                Ok(Ok(())) => {
                    info!("Zombie sweep: task {} marked as stopped", task_id);
                }
                Ok(Err(e)) => {
                    warn!("Zombie sweep: failed to stop task {}: {}", task_id, e);
                    continue;
                }
                Err(e) => {
                    warn!(
                        "Zombie sweep: spawn_blocking failed for task {}: {}",
                        task_id, e
                    );
                    continue;
                }
            }

            // Remove stale session entry if present
            session_manager.remove(task_id);

            // Notify frontend via EventBroadcaster (updates task list polling)
            let broadcaster = EventBroadcaster::new(app_handle.clone());
            broadcaster.task_run_update(task_id, "stopped", None, None);

            // Emit dedicated event for toast notification
            let payload = ZombieTaskCleanedPayload {
                task_run_id: task_id.clone(),
                task_name: task_name.clone(),
                message: "No active session found — task was stopped automatically".to_string(),
            };
            if let Err(e) = app_handle.emit("zombie-task-cleaned", &payload) {
                warn!("Zombie sweep: failed to emit event: {}", e);
            }

            cleaned_ids.push(task_id.clone());
        }
    }

    // 4. Clean up closed sessions from the SessionManager
    session_manager.cleanup_closed();

    // 5. Prune orphan tracker for tasks no longer running
    // (also removes entries we just cleaned up, since they're now 'stopped')
    let still_running: Vec<String> = running_ids
        .into_iter()
        .filter(|id| !cleaned_ids.contains(id))
        .collect();
    orphan_tracker.retain_known(&still_running);
}

/// Start the zombie sweep background task.
///
/// Waits for `STARTUP_GRACE` before the first sweep, then runs
/// `sweep_once()` every `SWEEP_INTERVAL`.
pub fn start_zombie_sweep(
    db: Arc<CheckpointDb>,
    session_manager: Arc<SessionManager>,
    app_handle: AppHandle,
) {
    let orphan_tracker = Arc::new(Mutex::new(OrphanTracker::new()));

    tokio::spawn(async move {
        // Wait for startup resume to complete
        tokio::time::sleep(STARTUP_GRACE).await;
        info!(
            "Zombie sweep: started (interval={}s, orphan_grace={}s)",
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
