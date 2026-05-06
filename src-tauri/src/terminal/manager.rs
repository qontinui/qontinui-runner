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
