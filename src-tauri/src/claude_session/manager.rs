//! SessionManager - tracks active Claude sessions by task_run_id.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

use super::session::ClaudeSession;
use super::state::SessionState;

/// Manages active Claude CLI sessions, keyed by task_run_id.
pub struct SessionManager {
    sessions: Mutex<HashMap<String, Arc<ClaudeSession>>>,
    /// PIDs for inline (non-interactive) Claude sessions, keyed by task_run_id.
    /// These sessions don't have a full ClaudeSession object but still need to be
    /// visible to the stale task sweep so it can check process liveness.
    inline_pids: Mutex<HashMap<String, u32>>,
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

    /// Get the current state of a session.
    pub fn get_state(&self, task_run_id: &str) -> Option<SessionState> {
        self.get(task_run_id).map(|s| s.state())
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
    /// Includes both interactive sessions and inline PID registrations.
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

        results
    }

    /// List all active session task_run_ids.
    pub fn list_active(&self) -> Vec<String> {
        self.sessions
            .lock()
            .map(|guard| {
                guard
                    .iter()
                    .filter(|(_, s)| s.state().is_active())
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Clean up any closed sessions.
    pub fn cleanup_closed(&self) {
        if let Ok(mut guard) = self.sessions.lock() {
            let before = guard.len();
            guard.retain(|_, s| s.state().is_active());
            let removed = before - guard.len();
            if removed > 0 {
                info!("SessionManager: cleaned up {} closed sessions", removed);
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
