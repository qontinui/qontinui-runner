//! TerminalManager — registry of active terminal sessions.
//!
//! Follows the same HashMap-based manager pattern as `claude_session::SessionManager`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::AppHandle;
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
                crate::mcp::shared::get_workspace_paths_internal()
                    .ok()
                    .map(|(root, _, _)| root.to_string_lossy().to_string())
            })
            .unwrap_or_default();
        let cols = cols.unwrap_or(120);
        let rows = rows.unwrap_or(30);

        let session = TerminalSession::spawn(
            id.clone(),
            title,
            working_dir,
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

    /// List all terminal sessions with their info.
    pub fn list(&self) -> Vec<TerminalInfo> {
        let sessions = match self.sessions.lock() {
            Ok(s) => s,
            Err(e) => {
                error!("Sessions lock poisoned: {}", e);
                return Vec::new();
            }
        };

        sessions.values().map(|s| s.info()).collect()
    }

    /// Get the number of active sessions.
    pub fn count(&self) -> usize {
        self.sessions.lock().map(|s| s.len()).unwrap_or(0)
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
