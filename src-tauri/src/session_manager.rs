//! Unified Session Manager
//!
//! Manages all AI sessions (Prompt Library workflows, AI Builder, one-shot)
//! with a unified checkpoint format and shared execution infrastructure.
//!
//! Key design decisions:
//! - All sessions use checkpoint-based continuation (no independent process spawning)
//! - Sessions run as child processes of the runner
//! - Multiple non-GUI sessions can run concurrently
//! - Only one GUI automation session can run at a time (gui_lock)
//! - Checkpoint persistence enables resume after runner restart

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{error, info};

// ============================================================================
// Session Types
// ============================================================================

/// Type of AI session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionType {
    /// Multi-phase workflow from Prompt Library
    PromptWorkflow,
    /// Iterative AI Builder session
    AiBuilder,
    /// Simple single-run session (e.g., /trigger-ai-analysis)
    OneShot,
}

/// Status of a session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Session is starting up
    Starting,
    /// Session is actively running
    Running,
    /// Session completed successfully
    Completed,
    /// Session failed with error
    Failed,
    /// Session was stopped by user
    Stopped,
    /// Session is waiting for continuation (between phases/iterations)
    WaitingForContinuation,
    /// Session is stalled (no progress for threshold duration)
    Stalled,
}

// ============================================================================
// Unified Checkpoint Format
// ============================================================================

/// Unified checkpoint format for all session types.
///
/// This combines the features of both Prompt Library checkpoints and
/// AI Builder state files into a single, consistent format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Session ID this checkpoint belongs to
    pub session_id: String,

    /// Type of session
    pub session_type: SessionType,

    /// Current phase number (1-indexed for display, 0 = not started)
    /// For AI Builder, this maps to iteration number
    pub current_phase: u32,

    /// Total number of phases (0 = unknown/unlimited)
    /// For AI Builder, this is max_iterations
    pub total_phases: u32,

    /// Whether the session has completed successfully
    pub completed: bool,

    /// Human-readable status string
    pub status: String,

    /// When the session started
    pub started_at: String,

    /// Last activity timestamp
    pub last_activity: String,

    /// Number of Claude sessions spawned
    pub sessions_spawned: u32,

    /// Whether restart is permitted after runner restart
    pub restart_permitted: bool,

    /// Error message if status is failed
    pub error_message: Option<String>,

    /// Session-specific custom data (workflow results, errors found, etc.)
    #[serde(default)]
    pub custom_data: serde_json::Value,

    /// Activity log entries
    #[serde(default)]
    pub activity_log: Vec<String>,
}

impl Checkpoint {
    /// Create a new checkpoint for a session
    pub fn new(session_id: &str, session_type: SessionType, total_phases: u32) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            session_id: session_id.to_string(),
            session_type,
            current_phase: 0,
            total_phases,
            completed: false,
            status: "starting".to_string(),
            started_at: now.clone(),
            last_activity: now,
            sessions_spawned: 0,
            restart_permitted: false,
            error_message: None,
            custom_data: serde_json::json!({}),
            activity_log: vec![],
        }
    }

    /// Check if the session is complete
    pub fn is_complete(&self) -> bool {
        if self.completed {
            return true;
        }

        // Check status strings
        let status_upper = self.status.to_uppercase();
        if matches!(
            status_upper.as_str(),
            "COMPLETE" | "COMPLETED" | "DONE" | "FINISHED"
        ) {
            return true;
        }

        // Check if current_phase >= total_phases (if total_phases is known)
        if self.total_phases > 0 && self.current_phase >= self.total_phases {
            return true;
        }

        false
    }

    /// Update last activity timestamp
    pub fn touch(&mut self) {
        self.last_activity = chrono::Utc::now().to_rfc3339();
    }

    /// Add a log entry
    pub fn log(&mut self, message: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        self.activity_log.push(format!("[{}] {}", now, message));
        self.touch();
    }

    /// Mark session as completed
    pub fn mark_completed(&mut self) {
        self.completed = true;
        self.status = "completed".to_string();
        self.log("Session completed successfully");
    }

    /// Mark session as failed
    pub fn mark_failed(&mut self, error: &str) {
        self.completed = false;
        self.status = "failed".to_string();
        self.error_message = Some(error.to_string());
        self.log(&format!("Session failed: {}", error));
    }

    /// Advance to next phase
    #[allow(dead_code)]
    pub fn advance_phase(&mut self) {
        self.current_phase += 1;
        self.sessions_spawned += 1;
        self.log(&format!("Advanced to phase {}", self.current_phase));
    }

    /// Get checkpoint file path
    pub fn file_path(dev_logs_dir: &Path, session_id: &str) -> std::path::PathBuf {
        dev_logs_dir.join(format!("session-{}-checkpoint.json", session_id))
    }

    /// Save checkpoint to file
    pub fn save(&self, dev_logs_dir: &Path) -> Result<(), String> {
        let path = Self::file_path(dev_logs_dir, &self.session_id);
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize checkpoint: {}", e))?;
        fs::write(&path, json).map_err(|e| format!("Failed to write checkpoint: {}", e))?;
        Ok(())
    }

    /// Load checkpoint from file
    #[allow(dead_code)]
    pub fn load(dev_logs_dir: &Path, session_id: &str) -> Result<Self, String> {
        let path = Self::file_path(dev_logs_dir, session_id);
        if !path.exists() {
            return Err("Checkpoint file does not exist".to_string());
        }
        let content =
            fs::read_to_string(&path).map_err(|e| format!("Failed to read checkpoint: {}", e))?;
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse checkpoint: {}", e))
    }

    /// Delete checkpoint file
    #[allow(dead_code)]
    pub fn delete(dev_logs_dir: &Path, session_id: &str) -> Result<(), String> {
        let path = Self::file_path(dev_logs_dir, session_id);
        if path.exists() {
            fs::remove_file(&path).map_err(|e| format!("Failed to delete checkpoint: {}", e))?;
        }
        Ok(())
    }
}

// ============================================================================
// Session Configuration
// ============================================================================

/// Configuration for starting a new session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Session type
    pub session_type: SessionType,

    /// Initial prompt content
    pub prompt: String,

    /// Prompt for continuation sessions (if different from initial)
    pub continuation_prompt: Option<String>,

    /// Total phases/iterations (0 = unlimited)
    pub total_phases: u32,

    /// Whether this session uses GUI automation
    pub uses_gui: bool,

    /// Timeout in seconds per session
    pub timeout_seconds: u64,

    /// Stall threshold in seconds (for detecting stuck sessions)
    pub stall_threshold_seconds: u64,

    /// Session name (for display)
    pub name: String,

    /// Session description
    pub description: String,

    /// Custom configuration data
    #[serde(default)]
    pub custom_config: serde_json::Value,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            session_type: SessionType::OneShot,
            prompt: String::new(),
            continuation_prompt: None,
            total_phases: 1,
            uses_gui: false,
            timeout_seconds: 1800,        // 30 minutes
            stall_threshold_seconds: 300, // 5 minutes
            name: "Unnamed Session".to_string(),
            description: String::new(),
            custom_config: serde_json::json!({}),
        }
    }
}

// ============================================================================
// Session
// ============================================================================

/// An active or completed AI session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session ID
    pub id: String,

    /// Session configuration
    pub config: SessionConfig,

    /// Current status
    pub status: SessionStatus,

    /// Checkpoint data
    pub checkpoint: Checkpoint,

    /// ID of currently active Claude subprocess (if any)
    pub active_subprocess_id: Option<String>,

    /// Event log for this session
    pub event_log: Vec<SessionEvent>,
}

/// An event in the session log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub timestamp: u64,
    pub event_type: String,
    pub message: String,
}

impl Session {
    /// Create a new session
    pub fn new(id: &str, config: SessionConfig) -> Self {
        let checkpoint = Checkpoint::new(id, config.session_type.clone(), config.total_phases);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            id: id.to_string(),
            config,
            status: SessionStatus::Starting,
            checkpoint,
            active_subprocess_id: None,
            event_log: vec![SessionEvent {
                timestamp: now,
                event_type: "created".to_string(),
                message: "Session created".to_string(),
            }],
        }
    }

    /// Log an event
    pub fn log_event(&mut self, event_type: &str, message: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.event_log.push(SessionEvent {
            timestamp: now,
            event_type: event_type.to_string(),
            message: message.to_string(),
        });
        self.checkpoint.touch();
    }

    /// Set session status with logging
    pub fn set_status(&mut self, status: SessionStatus, message: &str) {
        self.status = status.clone();
        let status_str = format!("{:?}", status).to_lowercase();
        self.log_event(&status_str, message);
    }

    /// Check if session is complete
    #[allow(dead_code)]
    pub fn is_complete(&self) -> bool {
        matches!(
            self.status,
            SessionStatus::Completed | SessionStatus::Failed | SessionStatus::Stopped
        ) || self.checkpoint.is_complete()
    }

    /// Check if session is running
    pub fn is_running(&self) -> bool {
        matches!(
            self.status,
            SessionStatus::Running
                | SessionStatus::Starting
                | SessionStatus::WaitingForContinuation
        )
    }
}

// ============================================================================
// Session Manager
// ============================================================================

/// Manages all AI sessions with unified checkpoint-based execution
pub struct SessionManager {
    /// Active sessions, keyed by session ID
    sessions: Arc<RwLock<HashMap<String, Session>>>,

    /// Session ID that currently holds the GUI automation lock
    gui_lock_holder: Arc<RwLock<Option<String>>>,

    /// Path to .dev-logs directory for checkpoints
    dev_logs_path: std::path::PathBuf,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new(dev_logs_path: std::path::PathBuf) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            gui_lock_holder: Arc::new(RwLock::new(None)),
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

        info!(
            "Started session {} ({:?})",
            session_id, session.config.session_type
        );
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

    /// Get sessions by type
    #[allow(dead_code)]
    pub async fn list_sessions_by_type(&self, session_type: SessionType) -> Vec<Session> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .filter(|s| s.config.session_type == session_type)
            .cloned()
            .collect()
    }

    // ========================================================================
    // GUI Lock
    // ========================================================================

    /// Acquire GUI automation lock
    pub async fn acquire_gui_lock(&self, session_id: &str) -> Result<(), String> {
        let mut lock = self.gui_lock_holder.write().await;
        if let Some(holder) = &*lock {
            if holder != session_id {
                return Err(format!(
                    "GUI automation lock held by session '{}'. Only one GUI session can run at a time.",
                    holder
                ));
            }
            return Ok(());
        }
        *lock = Some(session_id.to_string());
        info!("GUI lock acquired by session '{}'", session_id);
        Ok(())
    }

    /// Release GUI automation lock
    pub async fn release_gui_lock(&self, session_id: &str) {
        let mut lock = self.gui_lock_holder.write().await;
        if let Some(holder) = &*lock {
            if holder == session_id {
                *lock = None;
                info!("GUI lock released by session '{}'", session_id);
            }
        }
    }

    /// Get current GUI lock holder
    #[allow(dead_code)]
    pub async fn gui_lock_holder(&self) -> Option<String> {
        self.gui_lock_holder.read().await.clone()
    }

    /// Check if GUI is available for a session
    #[allow(dead_code)]
    pub async fn can_use_gui(&self, session_id: &str) -> bool {
        let lock = self.gui_lock_holder.read().await;
        match &*lock {
            None => true,
            Some(holder) => holder == session_id,
        }
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
            "gui_lock_holder": *self.gui_lock_holder.read().await,
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

    #[test]
    fn test_checkpoint_completion_detection() {
        let mut checkpoint = Checkpoint::new("test", SessionType::OneShot, 5);

        // Not complete initially
        assert!(!checkpoint.is_complete());

        // Complete via boolean
        checkpoint.completed = true;
        assert!(checkpoint.is_complete());

        // Complete via status string
        checkpoint.completed = false;
        checkpoint.status = "COMPLETED".to_string();
        assert!(checkpoint.is_complete());

        // Complete via phase count
        checkpoint.status = "running".to_string();
        checkpoint.current_phase = 5;
        assert!(checkpoint.is_complete());
    }

    #[test]
    fn test_checkpoint_save_load() {
        let dir = tempdir().unwrap();
        let checkpoint = Checkpoint::new("test-session", SessionType::AiBuilder, 10);

        // Save
        checkpoint.save(dir.path()).unwrap();

        // Load
        let loaded = Checkpoint::load(dir.path(), "test-session").unwrap();
        assert_eq!(loaded.session_id, "test-session");
        assert_eq!(loaded.total_phases, 10);
    }

    #[tokio::test]
    async fn test_session_manager_lifecycle() {
        let dir = tempdir().unwrap();
        let manager = SessionManager::new(dir.path().to_path_buf());

        let config = SessionConfig {
            session_type: SessionType::OneShot,
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
    async fn test_gui_lock() {
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
