//! WorkerSession - pty-backed Claude worker tracked by SessionManager.
//!
//! Unlike [`crate::claude_session::ClaudeSession`] (which drives the
//! stream-json automation protocol with a strict
//! `Created→Initializing→Ready→Processing` state machine), a
//! `WorkerSession` is a thin handle around a `TerminalManager` pty
//! session — the user can watch the raw xterm output, and the
//! coordinator dispatches `assign-task` briefs to it via
//! `TerminalSession::write` (raw stdin keystrokes).
//!
//! Workers carry their own `Arc<AtomicU8>` for state (see
//! [`SessionManager`](super::SessionManager) for the sibling-map
//! registration shape — Option B′ from the Phase 6 worker plan).
//! Workers track only `Ready ⇄ Processing → Closed`; they never enter
//! the `Created`/`Initializing`/`Interrupting`/`Promoting` states.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::state::SessionState;
use crate::terminal::TerminalManager;

// Compact u8 encoding for the three states a worker can hold. Round-trips
// only the states the worker lifecycle uses; other variants map back to
// `Closed` on read so a corrupt write fails closed rather than open.
const STATE_READY: u8 = 2;
const STATE_PROCESSING: u8 = 3;
const STATE_CLOSED: u8 = 7;

fn state_to_u8(state: SessionState) -> u8 {
    match state {
        SessionState::Ready => STATE_READY,
        SessionState::Processing => STATE_PROCESSING,
        SessionState::Closed => STATE_CLOSED,
        // Workers never logically enter the other states; any caller that
        // tries to set one falls back to Closed. The set_state path also
        // logs a warning so the regression is visible.
        _ => STATE_CLOSED,
    }
}

fn state_from_u8(value: u8) -> SessionState {
    match value {
        STATE_READY => SessionState::Ready,
        STATE_PROCESSING => SessionState::Processing,
        _ => SessionState::Closed,
    }
}

/// A pty-backed Claude worker session. See module docs for context.
pub struct WorkerSession {
    task_run_id: String,
    terminal_id: String,
    title: String,
    state: Arc<AtomicU8>,
    terminal_manager: Arc<TerminalManager>,
    created_at_unix: u64,
}

impl WorkerSession {
    /// Build a new worker. State starts as `Ready` — workers are immediately
    /// available to Rule B once registered.
    pub fn new(
        task_run_id: String,
        terminal_id: String,
        title: String,
        terminal_manager: Arc<TerminalManager>,
    ) -> Self {
        let created_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            task_run_id,
            terminal_id,
            title,
            state: Arc::new(AtomicU8::new(STATE_READY)),
            terminal_manager,
            created_at_unix,
        }
    }

    /// The `task_run_id` this worker is registered under. Acts as the key
    /// in `SessionManager::worker_sessions`.
    pub fn task_run_id(&self) -> &str {
        &self.task_run_id
    }

    /// The pty session id (key into `TerminalManager`).
    pub fn terminal_id(&self) -> &str {
        &self.terminal_id
    }

    /// User-facing title — typically `"Worker N"` from `next_worker_title`.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Unix timestamp (seconds) when this worker was registered.
    pub fn created_at_unix(&self) -> u64 {
        self.created_at_unix
    }

    /// Returns the worker's logical state. Workers are NEVER `Closed`
    /// unless we explicitly call [`Self::close`] — the pty might outlive
    /// the worker registration if the user closed the tab. We track
    /// `Ready ⇄ Processing`.
    pub fn state(&self) -> SessionState {
        state_from_u8(self.state.load(Ordering::Acquire))
    }

    /// Set state. Used by `observe.rs` Phase 2c wiring (Processing→Ready
    /// when the assigned task transitions to `done`/`cancelled`) and by
    /// [`Self::send_user_message`] (Ready→Processing on dispatch).
    pub fn set_state(&self, new_state: SessionState) {
        let encoded = state_to_u8(new_state);
        if encoded == STATE_CLOSED && !matches!(new_state, SessionState::Closed) {
            tracing::warn!(
                "WorkerSession::set_state: ignoring unsupported state {} for worker {}",
                new_state,
                self.task_run_id
            );
            return;
        }
        self.state.store(encoded, Ordering::Release);
    }

    /// Mark the worker `Closed`. Idempotent.
    pub fn close(&self) {
        self.state.store(STATE_CLOSED, Ordering::Release);
    }

    /// Best-effort PID for the pty's child process, used by stale-task
    /// sweep liveness checks. Returns `0` when the pty session has been
    /// removed from `TerminalManager` (the manager is the source of truth
    /// for child processes; sweep tolerates `0` as "unknown").
    pub fn pid(&self) -> u32 {
        self.terminal_manager
            .get(&self.terminal_id)
            .and_then(|s| s.info().pid)
            .unwrap_or(0)
    }

    /// Mirrors `ClaudeSession::send_user_message` — returns `Ok(true)` on
    /// successful immediate write. Workers do not queue (pty stdin is
    /// always writable). Delegates the entire submit framing to
    /// [`TerminalSession::submit_prompt`], which wraps the brief in ANSI
    /// bracketed-paste markers and follows it with a bare `\r` outside
    /// the paste window so the Claude CLI prompt actually submits.
    /// Flips state `Ready → Processing` on success. See Phase 6 §6
    /// remediation plan, Issue 1 for why CR-LF was insufficient.
    pub fn send_user_message(&self, message: &str) -> Result<bool, String> {
        let session = self
            .terminal_manager
            .get(&self.terminal_id)
            .ok_or_else(|| {
                format!(
                    "Worker pty {} not found in TerminalManager",
                    self.terminal_id
                )
            })?;

        session.submit_prompt(message)?;

        self.state.store(STATE_PROCESSING, Ordering::Release);
        Ok(true)
    }
}

impl std::fmt::Debug for WorkerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerSession")
            .field("task_run_id", &self.task_run_id)
            .field("terminal_id", &self.terminal_id)
            .field("title", &self.title)
            .field("state", &self.state())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trip() {
        assert_eq!(
            state_from_u8(state_to_u8(SessionState::Ready)),
            SessionState::Ready
        );
        assert_eq!(
            state_from_u8(state_to_u8(SessionState::Processing)),
            SessionState::Processing
        );
        assert_eq!(
            state_from_u8(state_to_u8(SessionState::Closed)),
            SessionState::Closed
        );
        // Unsupported state encodes to Closed.
        assert_eq!(
            state_from_u8(state_to_u8(SessionState::Initializing)),
            SessionState::Closed
        );
    }

    #[test]
    fn send_user_message_payload_matches_submit_prompt_framing() {
        // The actual byte sequence written by `submit_prompt` is exercised
        // in `terminal::session::tests`. This test guards the contract:
        // for input "hello", the bytes that hit the pty must be exactly
        // `\x1b[200~hello\x1b[201~\r` — paste begin, body, paste end,
        // bare CR (NOT CR-LF; LF would be eaten by the paste block).
        let payload = crate::terminal::session::build_submit_payload("hello");
        assert_eq!(payload, b"\x1b[200~hello\x1b[201~\r");
        // Regression guard: the Phase 6 §6 bug was that send_user_message
        // sent CR-LF; Claude Code's bracketed-paste handler ate the LF
        // and never submitted. The submit byte must be a bare CR.
        assert!(
            !payload.contains(&b'\n'),
            "submit framing must not contain LF bytes"
        );
    }

    #[test]
    fn unsupported_set_state_is_ignored() {
        let tm = Arc::new(TerminalManager::new());
        let w = WorkerSession::new(
            "task-1".to_string(),
            "term-1".to_string(),
            "Worker 1".to_string(),
            tm,
        );
        assert_eq!(w.state(), SessionState::Ready);
        // Initializing isn't representable for a worker; set_state should
        // refuse and leave the previous state in place.
        w.set_state(SessionState::Initializing);
        assert_eq!(w.state(), SessionState::Ready);
        // Processing and Closed are accepted.
        w.set_state(SessionState::Processing);
        assert_eq!(w.state(), SessionState::Processing);
        w.set_state(SessionState::Ready);
        assert_eq!(w.state(), SessionState::Ready);
        w.close();
        assert_eq!(w.state(), SessionState::Closed);
    }
}
