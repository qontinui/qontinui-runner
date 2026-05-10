//! SessionManager - tracks active Claude sessions by task_run_id.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

use super::session::ClaudeSession;
use super::state::SessionState;
use super::worker_session::WorkerSession;

/// Manages active Claude CLI sessions, keyed by task_run_id.
pub struct SessionManager {
    sessions: Mutex<HashMap<String, Arc<ClaudeSession>>>,
    /// PIDs for inline (non-interactive) Claude sessions, keyed by task_run_id.
    /// These sessions don't have a full ClaudeSession object but still need to be
    /// visible to the stale task sweep so it can check process liveness.
    inline_pids: Mutex<HashMap<String, u32>>,
    /// Pty-backed worker sessions, keyed by task_run_id. Sibling map to
    /// `sessions` (Option B′ from productivity-coordinator-workers-phase6).
    /// Workers do NOT share `ClaudeSession`'s strict state machine — they
    /// carry their own state on `WorkerSession`. They appear in
    /// `list_active`, `get_state`, `cleanup_closed`, and `close_all_sessions`
    /// alongside ClaudeSessions; the keyspace (UUIDv4 task_run_ids) is
    /// disjoint, so callers don't need to disambiguate.
    worker_sessions: Mutex<HashMap<String, Arc<WorkerSession>>>,
    /// Pending context to prepend to the next user message for a given task_run_id.
    /// Used for system notes that should be delivered with the next user message
    /// rather than sent as standalone messages (which would trigger unwanted response turns).
    pending_context: Mutex<HashMap<String, Vec<String>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            inline_pids: Mutex::new(HashMap::new()),
            worker_sessions: Mutex::new(HashMap::new()),
            pending_context: Mutex::new(HashMap::new()),
        }
    }

    /// Register a session. Returns Err if a session already exists for this key.
    pub fn register(&self, task_run_id: &str, session: Arc<ClaudeSession>) -> Result<(), String> {
        let mut guard = self
            .sessions
            .lock()
            .map_err(|e| format!("SessionManager lock poisoned: {}", e))?;

        if guard.contains_key(task_run_id) {
            return Err(format!(
                "Session already registered for task_run_id: {}",
                task_run_id
            ));
        }

        info!("SessionManager: registered session for {}", task_run_id);
        guard.insert(task_run_id.to_string(), session);
        Ok(())
    }

    /// Get a session by task_run_id.
    pub fn get(&self, task_run_id: &str) -> Option<Arc<ClaudeSession>> {
        self.sessions
            .lock()
            .ok()
            .and_then(|guard| guard.get(task_run_id).cloned())
    }

    /// Remove and return a session.
    pub fn remove(&self, task_run_id: &str) -> Option<Arc<ClaudeSession>> {
        let removed = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut guard| guard.remove(task_run_id));

        if removed.is_some() {
            info!("SessionManager: removed session for {}", task_run_id);
        } else {
            debug!(
                "SessionManager: no session found for {} (already removed?)",
                task_run_id
            );
        }

        removed
    }

    /// Get the current state of a session. Looks first in the
    /// ClaudeSession map, then falls back to the worker map (Phase 6).
    pub fn get_state(&self, task_run_id: &str) -> Option<SessionState> {
        if let Some(s) = self.get(task_run_id) {
            return Some(s.state());
        }
        self.get_worker(task_run_id).map(|w| w.state())
    }

    /// Register a pty-backed worker session. Returns Err on duplicate key
    /// — mirrors `register` for ClaudeSessions.
    pub fn register_worker(&self, session: Arc<WorkerSession>) -> Result<(), String> {
        let task_run_id = session.task_run_id().to_string();
        let mut guard = self
            .worker_sessions
            .lock()
            .map_err(|e| format!("SessionManager worker lock poisoned: {}", e))?;
        if guard.contains_key(&task_run_id) {
            return Err(format!(
                "Worker already registered for task_run_id: {}",
                task_run_id
            ));
        }
        info!("SessionManager: registered worker for {}", task_run_id);
        guard.insert(task_run_id, session);
        Ok(())
    }

    /// Get a registered worker by task_run_id.
    pub fn get_worker(&self, task_run_id: &str) -> Option<Arc<WorkerSession>> {
        self.worker_sessions
            .lock()
            .ok()
            .and_then(|guard| guard.get(task_run_id).cloned())
    }

    /// Remove and return a worker registration. Does NOT close the
    /// underlying pty (TerminalManager owns those lifetimes).
    pub fn remove_worker(&self, task_run_id: &str) -> Option<Arc<WorkerSession>> {
        let removed = self
            .worker_sessions
            .lock()
            .ok()
            .and_then(|mut guard| guard.remove(task_run_id));
        if removed.is_some() {
            info!("SessionManager: removed worker for {}", task_run_id);
        }
        removed
    }

    /// Register an inline (non-interactive) session's PID for stale task sweep visibility.
    /// Unlike `register`, this doesn't require a full ClaudeSession — just the PID.
    pub fn register_inline_pid(&self, task_run_id: &str, pid: u32) {
        if let Ok(mut guard) = self.inline_pids.lock() {
            guard.insert(task_run_id.to_string(), pid);
            info!(
                "SessionManager: registered inline PID {} for {}",
                pid, task_run_id
            );
        }
    }

    /// Remove an inline session's PID (called when the inline session completes).
    pub fn remove_inline_pid(&self, task_run_id: &str) {
        if let Ok(mut guard) = self.inline_pids.lock() {
            if guard.remove(task_run_id).is_some() {
                info!("SessionManager: removed inline PID for {}", task_run_id);
            }
        }
    }

    /// List all sessions with their task_run_id, current state, and PID.
    /// Includes ClaudeSessions, inline PID registrations, and pty Workers.
    /// Used by the stale task sweep to check session liveness.
    pub fn list_all_with_state(&self) -> Vec<(String, SessionState, u32)> {
        let mut results: Vec<(String, SessionState, u32)> = self
            .sessions
            .lock()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(k, s)| (k.clone(), s.state(), s.pid()))
                    .collect()
            })
            .unwrap_or_default();

        // Include inline PIDs with a synthetic Processing state
        if let Ok(guard) = self.inline_pids.lock() {
            for (id, pid) in guard.iter() {
                // Only add if not already covered by an interactive session
                if !results.iter().any(|(k, _, _)| k == id) {
                    results.push((id.clone(), SessionState::Processing, *pid));
                }
            }
        }

        // Include workers. Their pid is best-effort — `0` means unknown
        // (pty was already removed from TerminalManager); the sweep
        // tolerates 0 by treating it as "process liveness unknown".
        if let Ok(guard) = self.worker_sessions.lock() {
            for (id, w) in guard.iter() {
                if !results.iter().any(|(k, _, _)| k == id) {
                    results.push((id.clone(), w.state(), w.pid()));
                }
            }
        }

        results
    }

    /// List every registered worker, ordered by creation time (oldest first).
    /// Returns `(task_run_id, worker)` tuples so callers can correlate the
    /// registry key with the worker handle in one pass. Closed-but-not-yet-
    /// removed workers are included (cleanup is owned by `cleanup_closed`).
    /// Used by the `list_workers` Tauri command and `GET /workers` HTTP route.
    pub fn list_workers(&self) -> Vec<(String, Arc<WorkerSession>)> {
        self.worker_sessions
            .lock()
            .ok()
            .map(|guard| {
                let mut v: Vec<_> = guard.iter().map(|(k, w)| (k.clone(), w.clone())).collect();
                v.sort_by_key(|(_, w)| w.created_at_unix());
                v
            })
            .unwrap_or_default()
    }

    /// List all active session task_run_ids — union of ClaudeSessions and
    /// Workers whose state is non-Closed. Phase 6: workers participate in
    /// `Observation.live_sessions` so Rule B can assign them tasks.
    pub fn list_active(&self) -> Vec<String> {
        let mut active: Vec<String> = self
            .sessions
            .lock()
            .map(|guard| {
                guard
                    .iter()
                    .filter(|(_, s)| s.state().is_active())
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();

        if let Ok(guard) = self.worker_sessions.lock() {
            for (id, w) in guard.iter() {
                if w.state().is_active() && !active.contains(id) {
                    active.push(id.clone());
                }
            }
        }

        active
    }

    /// Clean up any closed sessions and workers.
    pub fn cleanup_closed(&self) {
        if let Ok(mut guard) = self.sessions.lock() {
            let before = guard.len();
            guard.retain(|_, s| s.state().is_active());
            let removed = before - guard.len();
            if removed > 0 {
                info!("SessionManager: cleaned up {} closed sessions", removed);
            }
        }
        if let Ok(mut guard) = self.worker_sessions.lock() {
            let before = guard.len();
            guard.retain(|_, w| w.state().is_active());
            let removed = before - guard.len();
            if removed > 0 {
                info!("SessionManager: cleaned up {} closed workers", removed);
            }
        }
    }

    /// Add pending context that will be prepended to the next user message.
    /// This is used for system notes that Claude should see but that should NOT
    /// trigger a standalone response turn.
    pub fn push_pending_context(&self, task_run_id: &str, context: String) {
        if let Ok(mut guard) = self.pending_context.lock() {
            let len = context.len();
            guard
                .entry(task_run_id.to_string())
                .or_default()
                .push(context);
            info!(
                "SessionManager: queued pending context for {} ({} chars)",
                task_run_id, len
            );
        }
    }

    /// Drain all pending context for a task_run_id, returning it as a single
    /// string to prepend to the next user message. Returns None if no pending context.
    pub fn drain_pending_context(&self, task_run_id: &str) -> Option<String> {
        if let Ok(mut guard) = self.pending_context.lock() {
            if let Some(contexts) = guard.remove(task_run_id) {
                if contexts.is_empty() {
                    return None;
                }
                let combined = contexts.join("\n\n");
                info!(
                    "SessionManager: drained {} pending context(s) for {} ({} chars)",
                    contexts.len(),
                    task_run_id,
                    combined.len()
                );
                return Some(combined);
            }
        }
        None
    }

    /// Close and remove all active sessions (used during app shutdown or stop-all).
    pub fn close_all_sessions(&self) {
        if let Ok(mut guard) = self.sessions.lock() {
            let count = guard.len();
            if count > 0 {
                info!("SessionManager: closing all {} active sessions", count);
                for (task_run_id, session) in guard.drain() {
                    info!(
                        "SessionManager: closing session for {} (state: {})",
                        task_run_id,
                        session.state()
                    );
                    let _ = session.close();
                }
            }
        }
        // Also clear inline PID registrations
        if let Ok(mut guard) = self.inline_pids.lock() {
            let count = guard.len();
            if count > 0 {
                info!(
                    "SessionManager: clearing {} inline PID registrations",
                    count
                );
                guard.clear();
            }
        }
        // Drop worker registrations. The pty TerminalSessions are owned
        // by TerminalManager (its own shutdown path closes them); we just
        // forget the worker handles here.
        if let Ok(mut guard) = self.worker_sessions.lock() {
            let count = guard.len();
            if count > 0 {
                info!("SessionManager: clearing {} worker registrations", count);
                for (_, w) in guard.iter() {
                    w.close();
                }
                guard.clear();
            }
        }
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.sessions.lock().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("SessionManager")
            .field("session_count", &count)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalManager;
    use std::thread;
    use std::time::Duration;

    fn make_worker(task_run_id: &str, terminal_id: &str, title: &str) -> Arc<WorkerSession> {
        Arc::new(WorkerSession::new(
            task_run_id.to_string(),
            terminal_id.to_string(),
            title.to_string(),
            Arc::new(TerminalManager::new()),
        ))
    }

    #[test]
    fn list_workers_empty_returns_empty_vec() {
        let mgr = SessionManager::new();
        assert!(mgr.list_workers().is_empty());
    }

    #[test]
    fn list_workers_orders_by_created_at_unix() {
        let mgr = SessionManager::new();
        // Build the older worker first, then sleep so the second one's
        // `created_at_unix` is strictly greater. WorkerSession::new() uses
        // `SystemTime::now()` at second precision, so a 1.1s gap is enough
        // to guarantee distinct seconds even on coarse-clock platforms.
        let older = make_worker("task-older", "term-A", "Worker A");
        mgr.register_worker(older.clone()).expect("register older");
        thread::sleep(Duration::from_millis(1100));
        let newer = make_worker("task-newer", "term-B", "Worker B");
        mgr.register_worker(newer.clone()).expect("register newer");

        let listed = mgr.list_workers();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].0, "task-older");
        assert_eq!(listed[1].0, "task-newer");
        assert!(listed[0].1.created_at_unix() <= listed[1].1.created_at_unix());
    }

    #[test]
    fn list_workers_includes_closed_until_cleanup() {
        let mgr = SessionManager::new();
        let w = make_worker("task-closed", "term-X", "Worker X");
        mgr.register_worker(w.clone()).expect("register");
        // Closing the worker flips its state to Closed but does NOT remove
        // it from the registry — `list_workers` must still surface it so
        // observability callers can render the closed state. Cleanup is the
        // separate `cleanup_closed` path.
        w.close();
        let listed = mgr.list_workers();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, "task-closed");
        assert_eq!(listed[0].1.state(), SessionState::Closed);
    }
}
