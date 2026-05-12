//! TerminalManager — registry of active terminal sessions.
//!
//! Follows the same HashMap-based manager pattern as `claude_session::SessionManager`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter};
use tracing::{error, info};

use super::interceptor::OutputInterceptor;
use super::session::TerminalSession;
use super::types::{TerminalId, TerminalInfo};

/// Manages all active terminal sessions.
pub struct TerminalManager {
    sessions: Mutex<HashMap<TerminalId, Arc<TerminalSession>>>,
    interceptor: Arc<OutputInterceptor>,
}

impl TerminalManager {
    /// Create a new terminal manager with an empty interceptor pipeline.
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            interceptor: Arc::new(OutputInterceptor::new()),
        }
    }

    /// Create a new terminal session, returning its info.
    pub fn create(
        &self,
        title: Option<String>,
        working_dir: Option<String>,
        page_id: Option<String>,
        cols: Option<u16>,
        rows: Option<u16>,
        app_handle: AppHandle,
    ) -> Result<TerminalInfo, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let title = title.unwrap_or_else(|| format!("Terminal {}", self.count() + 1));
        let working_dir = working_dir
            .filter(|d| !d.is_empty())
            .or_else(|| {
                // Default to workspace root (parent of qontinui-runner)
                crate::mcp::shared::current_project_path()
            })
            .unwrap_or_default();
        let page_id = page_id.unwrap_or_else(|| "default".to_string());
        let cols = cols.unwrap_or(120);
        let rows = rows.unwrap_or(30);

        let emitter = app_handle.clone();
        let session = TerminalSession::spawn(
            id.clone(),
            title,
            working_dir,
            page_id,
            cols,
            rows,
            app_handle,
            self.interceptor.clone(),
        )?;

        let info = session.info();
        let session = Arc::new(session);

        let mut sessions = self
            .sessions
            .lock()
            .map_err(|e| format!("Sessions lock poisoned: {}", e))?;
        sessions.insert(id, session);

        // Notify frontend so externally-created terminals get a UI tab
        if let Err(e) = emitter.emit("terminal-created", &info) {
            error!("Failed to emit terminal-created: {}", e);
        }

        Ok(info)
    }

    /// Get a terminal session by ID.
    pub fn get(&self, id: &str) -> Option<Arc<TerminalSession>> {
        self.sessions.lock().ok().and_then(|s| s.get(id).cloned())
    }

    /// Update the title of a terminal session and emit a
    /// `terminal-title-changed` Tauri event so other webview windows
    /// (and the backend relay's WS subscribers) stay in sync.
    ///
    /// Phase 2 of the bi-directional title sync (plan
    /// `2026-05-11-runner-dispatch-and-terminal-ux-fixes-plan.md`): the
    /// frontend's xterm.js `onTitleChange` callback now invokes
    /// `terminal_set_title` so backend `/terminals` titles match what the
    /// user sees in the UI. Without this, `TerminalSession.title` was
    /// frozen at spawn time and drifted from the OSC 0 title the child
    /// emits at runtime.
    pub fn set_title(&self, id: &str, title: String, app_handle: &AppHandle) -> Result<(), String> {
        let session = self
            .get(id)
            .ok_or_else(|| format!("Terminal session not found: {}", id))?;
        session.set_title(title.clone());
        let payload = serde_json::json!({ "id": id, "title": title });
        if let Err(e) = app_handle.emit("terminal-title-changed", &payload) {
            error!("Failed to emit terminal-title-changed: {}", e);
        }
        // Mirror to the backend WS relay so remote mobile viewers stay
        // consistent (same pattern as the reader thread's terminal-output
        // mirror in session.rs).
        crate::event_system::broadcast_ws_notification(
            app_handle,
            "terminal-title-changed",
            &payload,
        );
        Ok(())
    }

    /// Remove and close a terminal session.
    pub fn close(&self, id: &str) -> Result<(), String> {
        let session = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|e| format!("Sessions lock poisoned: {}", e))?;
            sessions.remove(id)
        };

        if let Some(session) = session {
            session.close();
            Ok(())
        } else {
            Err(format!("Terminal session not found: {}", id))
        }
    }

    /// List all terminal sessions with their info, sorted by creation time.
    pub fn list(&self) -> Vec<TerminalInfo> {
        let sessions = match self.sessions.lock() {
            Ok(s) => s,
            Err(e) => {
                error!("Sessions lock poisoned: {}", e);
                return Vec::new();
            }
        };

        let mut infos: Vec<TerminalInfo> = sessions.values().map(|s| s.info()).collect();
        infos.sort_by_key(|info| info.created_at);
        infos
    }

    /// Get the number of active sessions.
    pub fn count(&self) -> usize {
        self.sessions.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Return the `(cols, rows)` of the largest currently-registered
    /// terminal — "largest" measured by `cols * rows` cell count. Falls
    /// back to `(120, 30)` (the historical `create()` default) when no
    /// sessions exist.
    ///
    /// Phase 4 of the 2026-05-11 dispatch-fix plan: worker tabs spawned
    /// into a zone where another tab is currently visible mount under
    /// `display: none`. xterm.js's fit-addon then can't measure the
    /// container, so the backend PTY stays at the 120×30 default until
    /// the user activates the tab. Pre-sizing the new PTY to the
    /// dominant zone dims means Coordinator dispatch lands on a PTY
    /// matching the eventual visible size (Claude doesn't have to wrap
    /// twice). Fallback dims match what `create()` would have used on
    /// `None, None`, so behaviour is identical when there's no signal to
    /// crib from.
    pub fn dominant_zone_dims(&self) -> (u16, u16) {
        let sessions = match self.sessions.lock() {
            Ok(s) => s,
            Err(e) => {
                error!("Sessions lock poisoned in dominant_zone_dims: {}", e);
                return (120, 30);
            }
        };
        let mut best: Option<(u16, u16, u32)> = None;
        for sess in sessions.values() {
            let info = sess.info();
            let area = (info.cols as u32).saturating_mul(info.rows as u32);
            if area == 0 {
                continue;
            }
            if best.map(|(_, _, a)| area > a).unwrap_or(true) {
                best = Some((info.cols, info.rows, area));
            }
        }
        best.map(|(c, r, _)| (c, r)).unwrap_or((120, 30))
    }

    /// Snapshot of `(session_id, Arc<TerminalSession>)` pairs for
    /// callers that need to iterate every active session — e.g. the
    /// cross-session grid search endpoint. Sorted by creation time
    /// for stable output ordering.
    pub fn sessions_snapshot(&self) -> Vec<(String, Arc<TerminalSession>)> {
        let sessions = match self.sessions.lock() {
            Ok(s) => s,
            Err(e) => {
                error!("Sessions lock poisoned: {}", e);
                return Vec::new();
            }
        };
        let mut pairs: Vec<(String, Arc<TerminalSession>)> = sessions
            .iter()
            .map(|(id, sess)| (id.clone(), sess.clone()))
            .collect();
        pairs.sort_by_key(|(_, sess)| sess.info().created_at);
        pairs
    }

    /// Close all terminal sessions. Called on app shutdown.
    pub fn close_all(&self) {
        let sessions: Vec<Arc<TerminalSession>> = {
            match self.sessions.lock() {
                Ok(mut s) => s.drain().map(|(_, v)| v).collect(),
                Err(e) => {
                    error!("Sessions lock poisoned during close_all: {}", e);
                    return;
                }
            }
        };

        if sessions.is_empty() {
            return;
        }

        info!("Closing {} terminal session(s)", sessions.len());
        for session in &sessions {
            session.close();
        }
        info!("All terminal sessions closed");
    }

    /// Get a reference to the output interceptor (for adding hooks).
    #[allow(dead_code)]
    pub fn interceptor(&self) -> &Arc<OutputInterceptor> {
        &self.interceptor
    }
}
