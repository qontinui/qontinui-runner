//! SessionManager - tracks active Claude sessions by task_run_id.

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
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

    // (helper `worktree_path_matches` lives at module scope below so it is
    // unit-testable without constructing a `ClaudeSession`.)

    /// Resolve the `task_run_id` of the active `ClaudeSession` whose isolated
    /// worktree checkout is `workdir` (session-fabric Phase 0 self-identification).
    ///
    /// The coord-mcp loopback proxy knows only the session WORKDIR its
    /// per-session nonce was provisioned into; this bridges that back to the
    /// `task_run_id` the `AiCoordRegistrar` keys the coord `agent_session_id`
    /// on. Matches on the promoted worktree path — the terminal chokepoint
    /// (`acquire_for_terminal`) provisions the nonce into exactly that dir.
    ///
    /// Sessions NOT promoted into a worktree carry no stored cwd to match, so
    /// they resolve `None`: a documented Phase-0 residual (the proxy then omits
    /// the caller-session header and coord keeps its fuzzy fallback). O(N) over
    /// active sessions (bounded by the operator's live terminal count).
    pub fn task_run_id_for_workdir(&self, workdir: &str) -> Option<String> {
        let target_canon = std::fs::canonicalize(workdir).ok();
        // Snapshot (task_run_id, worktree path) pairs under the lock, then do
        // the filesystem `canonicalize` comparisons AFTER releasing it. This
        // runs on every proxied coord_* call (session-fabric Phase 0 caller
        // self-identification), and holding `sessions` across O(live sessions)
        // `canonicalize` syscalls would serialize all session management behind
        // proxy traffic. The snapshot is cheap (clone of a String + PathBuf per
        // worktree-carrying session); the syscalls happen lock-free.
        let candidates: Vec<(String, std::path::PathBuf)> = {
            let guard = self.sessions.lock().ok()?;
            guard
                .iter()
                .filter_map(|(task_run_id, session)| {
                    session
                        .worktree()
                        .map(|wt| (task_run_id.clone(), wt.path.clone()))
                })
                .collect()
        };
        for (task_run_id, path) in candidates {
            if worktree_path_matches(&path, workdir, target_canon.as_deref()) {
                return Some(task_run_id);
            }
        }
        None
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
    /// Includes ClaudeSessions and inline PID registrations.
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

    /// Snapshot every registered `ClaudeSession` as a tuple of
    /// `(task_run_id, holder_name, last_activity_tracker)`.
    ///
    /// `holder_name` matches the friendly display name the file-lock
    /// dispatcher emits as `holder_name` on `file-lock-*` events (see
    /// `claude_session/dispatcher.rs:398-404` and
    /// `ClaudeSession::holder_name`).
    ///
    /// `last_activity_tracker` is a shared `Arc<AtomicU64>` holding the
    /// most-recent stdout-line epoch SECONDS (written at
    /// `claude_session/session.rs:420`). Callers convert to ms at the
    /// boundary.
    ///
    /// Inline-PID registrations are intentionally excluded: they don't
    /// expose a `last_activity` channel.
    /// Returns an empty Vec when no `ClaudeSession`s are registered or
    /// when the inner Mutex is poisoned.
    pub fn snapshot(&self) -> Vec<(String, String, Arc<AtomicU64>)> {
        self.sessions
            .lock()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(id, s)| {
                        (
                            id.clone(),
                            s.holder_name().to_string(),
                            s.last_activity_tracker(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// List all active session task_run_ids — every ClaudeSession whose
    /// state is non-Closed. Broad-by-default: includes `Initializing`
    /// sessions. Callers that need `Ready` sessions only MUST filter the
    /// returned ids via [`Self::get_state`].
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

    /// Snapshot every active `ClaudeSession` as `(task_run_id, session)`.
    ///
    /// Used by the graceful-drain sequence (`crate::drain`) which needs the
    /// live `Arc<ClaudeSession>` (not just the id) to flush in-flight turns,
    /// read each session's `worktree()`, and force-flush unpersisted output.
    /// Excludes inline PIDs — they don't run a resumable Claude
    /// conversation with an `output_log` replay, so there's nothing to drain.
    pub fn active_claude_sessions(&self) -> Vec<(String, Arc<ClaudeSession>)> {
        self.sessions
            .lock()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .filter(|(_, s)| s.state().is_active())
                    .map(|(k, s)| (k.clone(), s.clone()))
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

/// Does a session's worktree `path` identify `workdir`? A fast literal-string
/// compare first (the common case — the proxy provisions the nonce into the
/// exact stored path), then a canonical-path fallback for symlink/8.3/case
/// differences. `target_canon` is `canonicalize(workdir)` computed once by the
/// caller so it is not repeated per session. Pure and lock-free by design so
/// `task_run_id_for_workdir` can run it outside the `sessions` lock and so it
/// is unit-testable without a live `ClaudeSession`.
fn worktree_path_matches(
    path: &std::path::Path,
    workdir: &str,
    target_canon: Option<&std::path::Path>,
) -> bool {
    if path.to_string_lossy() == workdir {
        return true;
    }
    match target_canon {
        Some(tc) => std::fs::canonicalize(path).ok().as_deref() == Some(tc),
        None => false,
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

    #[test]
    fn worktree_path_matches_on_exact_string_without_touching_fs() {
        // The common path: the stored worktree path is byte-identical to the
        // workdir the proxy provisioned. Must match on the string alone, so a
        // `None` target_canon (canonicalize failed / not computed) is
        // irrelevant — no filesystem access is required for a hit.
        let p = std::path::Path::new("D:/qontinui-root/agent-worktrees/abc/qontinui-runner");
        assert!(worktree_path_matches(
            p,
            "D:/qontinui-root/agent-worktrees/abc/qontinui-runner",
            None
        ));
    }

    #[test]
    fn worktree_path_matches_rejects_a_different_path() {
        // Distinct paths, and no canonical target to fall back on ⇒ no match.
        // This is the worktree-less / wrong-session case that must resolve to
        // None so coord keeps its fuzzy fallback rather than misattributing.
        let p = std::path::Path::new("D:/qontinui-root/agent-worktrees/abc/qontinui-runner");
        assert!(!worktree_path_matches(
            p,
            "D:/qontinui-root/agent-worktrees/XYZ/qontinui-runner",
            None
        ));
    }

    #[test]
    fn worktree_path_matches_canonical_fallback_on_a_real_dir() {
        // When the strings differ, an equal *canonical* form still matches.
        // Use the temp dir (a real path) reached two ways: itself, and itself
        // + "/." — different strings, identical canonicalization.
        let base = std::env::temp_dir();
        let dotted = base.join(".");
        let target_canon = std::fs::canonicalize(&base).ok();
        assert!(
            target_canon.is_some(),
            "temp dir must canonicalize for this test to be meaningful"
        );
        // `dotted` as the stored session path, `base` as the query workdir:
        // strings differ (trailing "/."), canonical forms match.
        assert!(worktree_path_matches(
            &dotted,
            &base.to_string_lossy(),
            target_canon.as_deref()
        ));
    }
}
