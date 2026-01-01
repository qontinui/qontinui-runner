//! Session lifecycle management.
//!
//! Handles the complete lifecycle of AI sessions:
//! - Starting new sessions
//! - Tracking active sessions
//! - Completing/failing/stopping sessions
//! - Persisting state for recovery after restart

use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

use super::locking::GuiLock;
use super::types::{Session, SessionConfig, SessionStatus};

// ============================================================================
// Session Manager
// ============================================================================

/// Manages all AI sessions with unified checkpoint-based execution.
///
/// Key responsibilities:
/// - Start, stop, complete, and fail sessions
/// - Track active sessions in memory
/// - Coordinate GUI lock for exclusive automation access
/// - Persist and restore state for crash recovery
pub struct SessionManager {
    /// Active sessions, keyed by session ID
    sessions: Arc<RwLock<HashMap<String, Session>>>,

    /// GUI automation lock
    gui_lock: GuiLock,

    /// Path to .dev-logs directory for checkpoints
    dev_logs_path: std::path::PathBuf,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new(dev_logs_path: std::path::PathBuf) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            gui_lock: GuiLock::new(),
            dev_logs_path,
        }
    }

    // ========================================================================
    // Session Lifecycle
    // ========================================================================

    /// Start a new session
    pub async fn start_session(&self, config: SessionConfig) -> Result<Session, String> {
        let session_id = uuid::Uuid::new_v4().to_string();

        // Check GUI lock if needed
        if config.uses_gui {
            self.acquire_gui_lock(&session_id).await?;
        }

        let session = Session::new(&session_id, config);

        // Save initial checkpoint
        session.checkpoint.save(&self.dev_logs_path)?;

        // Add to active sessions
        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(session_id.clone(), session.clone());
        }

        info!("Started session {}", session_id);
        Ok(session)
    }

    /// Get a session by ID
    pub async fn get_session(&self, session_id: &str) -> Option<Session> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned()
    }

    /// Update a session
    pub async fn update_session(&self, session: Session) -> Result<(), String> {
        // Save checkpoint
        session.checkpoint.save(&self.dev_logs_path)?;

        // Update in memory
        let mut sessions = self.sessions.write().await;
        sessions.insert(session.id.clone(), session);
        Ok(())
    }

    /// Stop a session
    pub async fn stop_session(&self, session_id: &str, reason: &str) -> Option<Session> {
        let mut sessions = self.sessions.write().await;
        if let Some(mut session) = sessions.get_mut(session_id).cloned() {
            session.set_status(SessionStatus::Stopped, reason);
            session.checkpoint.status = "stopped".to_string();
            session.checkpoint.log(reason);

            // Save final checkpoint
            let _ = session.checkpoint.save(&self.dev_logs_path);

            // Release GUI lock if held
            drop(sessions);
            self.release_gui_lock(session_id).await;

            info!("Stopped session {}: {}", session_id, reason);
            return Some(session);
        }
        None
    }

    /// Mark a session as completed
    #[allow(dead_code)]
    pub async fn complete_session(&self, session_id: &str) -> Option<Session> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.set_status(SessionStatus::Completed, "Session completed successfully");
            session.checkpoint.mark_completed();
            let _ = session.checkpoint.save(&self.dev_logs_path);

            drop(sessions);
            self.release_gui_lock(session_id).await;

            info!("Completed session {}", session_id);
            return self.get_session(session_id).await;
        }
        None
    }

    /// Mark a session as failed
    #[allow(dead_code)]
    pub async fn fail_session(&self, session_id: &str, error: &str) -> Option<Session> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.set_status(SessionStatus::Failed, error);
            session.checkpoint.mark_failed(error);
            let _ = session.checkpoint.save(&self.dev_logs_path);

            drop(sessions);
            self.release_gui_lock(session_id).await;

            error!("Failed session {}: {}", session_id, error);
            return self.get_session(session_id).await;
        }
        None
    }

    /// Remove a session from tracking
    pub async fn remove_session(&self, session_id: &str) -> Option<Session> {
        let mut sessions = self.sessions.write().await;
        let session = sessions.remove(session_id);

        if session.is_some() {
            drop(sessions);
            self.release_gui_lock(session_id).await;
        }

        session
    }

    /// Get all active sessions
    pub async fn list_sessions(&self) -> Vec<Session> {
        let sessions = self.sessions.read().await;
        sessions.values().cloned().collect()
    }

    /// Get all running sessions
    #[allow(dead_code)]
    pub async fn list_running_sessions(&self) -> Vec<Session> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .filter(|s| s.is_running())
            .cloned()
            .collect()
    }

    // ========================================================================
    // GUI Lock (delegated to GuiLock)
    // ========================================================================

    /// Acquire GUI automation lock
    pub async fn acquire_gui_lock(&self, session_id: &str) -> Result<(), String> {
        self.gui_lock.acquire(session_id).await
    }

    /// Release GUI automation lock
    pub async fn release_gui_lock(&self, session_id: &str) {
        self.gui_lock.release(session_id).await
    }

    /// Get current GUI lock holder
    #[allow(dead_code)]
    pub async fn gui_lock_holder(&self) -> Option<String> {
        self.gui_lock.holder().await
    }

    /// Check if GUI is available for a session
    #[allow(dead_code)]
    pub async fn can_use_gui(&self, session_id: &str) -> bool {
        self.gui_lock.is_available_for(session_id).await
    }

    // ========================================================================
    // Persistence
    // ========================================================================

    /// Get path to persisted state file
    fn state_file_path(&self) -> std::path::PathBuf {
        self.dev_logs_path.join("session-manager-state.json")
    }

    /// Persist current state for recovery after restart
    pub async fn persist_state(&self) -> Result<(), String> {
        let sessions = self.sessions.read().await;
        let running_sessions: Vec<&Session> =
            sessions.values().filter(|s| s.is_running()).collect();

        if running_sessions.is_empty() {
            // Remove state file if no running sessions
            let path = self.state_file_path();
            if path.exists() {
                let _ = fs::remove_file(&path);
            }
            return Ok(());
        }

        let state = serde_json::json!({
            "sessions": running_sessions,
            "gui_lock_holder": self.gui_lock.holder().await,
            "persisted_at": chrono::Utc::now().to_rfc3339(),
        });

        let json = serde_json::to_string_pretty(&state)
            .map_err(|e| format!("Failed to serialize state: {}", e))?;
        fs::write(self.state_file_path(), json)
            .map_err(|e| format!("Failed to write state file: {}", e))?;

        info!("Persisted {} running session(s)", running_sessions.len());
        Ok(())
    }

    /// Restore state after restart
    pub async fn restore_state(&self) -> Result<Vec<Session>, String> {
        let path = self.state_file_path();
        if !path.exists() {
            return Ok(vec![]);
        }

        let content =
            fs::read_to_string(&path).map_err(|e| format!("Failed to read state file: {}", e))?;

        let state: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse state file: {}", e))?;

        let sessions: Vec<Session> = serde_json::from_value(
            state
                .get("sessions")
                .cloned()
                .unwrap_or(serde_json::json!([])),
        )
        .unwrap_or_default();

        // Filter to only sessions that have restart_permitted
        let resumable: Vec<Session> = sessions
            .into_iter()
            .filter(|s| s.checkpoint.restart_permitted && s.is_running())
            .collect();

        if resumable.is_empty() {
            let _ = fs::remove_file(&path);
            return Ok(vec![]);
        }

        // Restore sessions
        let mut sessions_map = self.sessions.write().await;
        for session in &resumable {
            sessions_map.insert(session.id.clone(), session.clone());
        }

        info!("Restored {} session(s) for resumption", resumable.len());
        Ok(resumable)
    }

    /// Clear persisted state
    #[allow(dead_code)]
    pub fn clear_persisted_state(&self) -> Result<(), String> {
        let path = self.state_file_path();
        if path.exists() {
            fs::remove_file(&path).map_err(|e| format!("Failed to remove state file: {}", e))?;
        }
        Ok(())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_session_manager_lifecycle() {
        let dir = tempdir().unwrap();
        let manager = SessionManager::new(dir.path().to_path_buf());

        let config = SessionConfig {
            prompt: "Test prompt".to_string(),
            name: "Test Session".to_string(),
            ..Default::default()
        };

        // Start session
        let session = manager.start_session(config).await.unwrap();
        assert!(session.is_running());

        // Get session
        let retrieved = manager.get_session(&session.id).await.unwrap();
        assert_eq!(retrieved.id, session.id);

        // Complete session
        manager.complete_session(&session.id).await;
        let completed = manager.get_session(&session.id).await.unwrap();
        assert!(completed.is_complete());
    }

    #[tokio::test]
    async fn test_gui_lock_via_manager() {
        let dir = tempdir().unwrap();
        let manager = SessionManager::new(dir.path().to_path_buf());

        // Acquire lock
        manager.acquire_gui_lock("session-1").await.unwrap();

        // Second acquisition should fail
        let result = manager.acquire_gui_lock("session-2").await;
        assert!(result.is_err());

        // Same session can re-acquire
        manager.acquire_gui_lock("session-1").await.unwrap();

        // Release and re-acquire by different session
        manager.release_gui_lock("session-1").await;
        manager.acquire_gui_lock("session-2").await.unwrap();
    }
}
