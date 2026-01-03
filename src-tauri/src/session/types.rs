//! Session types and data structures.
//!
//! Contains the core types used throughout the session management system:
//! - `SessionStatus` - Current state of a session
//! - `SessionConfig` - Configuration for starting a session
//! - `Session` - An active or completed session
//! - `SessionEvent` - Log entry for session events

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use super::checkpoints::Checkpoint;

// ============================================================================
// Session Status
// ============================================================================

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
// Session Configuration
// ============================================================================

/// Configuration for starting a new session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
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

    /// AI provider override (e.g., "claude_cli", "gemini_api")
    #[serde(default)]
    pub provider: Option<String>,

    /// AI model override (e.g., "claude-sonnet-4-20250514", "gemini-2.0-flash")
    #[serde(default)]
    pub model: Option<String>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            continuation_prompt: None,
            total_phases: 0, // 0 = unlimited
            uses_gui: false,
            timeout_seconds: 1800,        // 30 minutes
            stall_threshold_seconds: 300, // 5 minutes
            name: "Unnamed Session".to_string(),
            description: String::new(),
            custom_config: serde_json::json!({}),
            provider: None,
            model: None,
        }
    }
}

// ============================================================================
// Session Event
// ============================================================================

/// An event in the session log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub timestamp: u64,
    pub event_type: String,
    pub message: String,
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

impl Session {
    /// Create a new session
    pub fn new(id: &str, config: SessionConfig) -> Self {
        let checkpoint = Checkpoint::new(id, config.total_phases);

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
