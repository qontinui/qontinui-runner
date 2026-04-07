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

/// Start the stale task sweep background task.
///
/// Currently disabled — the sweep depended on SQLite-backed task run state and
/// has not yet been ported to PostgreSQL.
pub fn start_zombie_sweep(session_manager: Arc<SessionManager>, app_handle: AppHandle) {
    let _ = (session_manager, app_handle);
    info!("Stale task sweep: disabled (pending PG port)");
}
