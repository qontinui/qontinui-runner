//! Checkpoint persistence and management.
//!
//! Handles saving and loading session checkpoints to disk,
//! enabling session resumption after runner restart.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

// ============================================================================
// Checkpoint
// ============================================================================

/// Unified checkpoint format for all sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Session ID this checkpoint belongs to
    pub session_id: String,

    /// Current phase number (1-indexed for display, 0 = not started)
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
    pub fn new(session_id: &str, total_phases: u32) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            session_id: session_id.to_string(),
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
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_checkpoint_completion_detection() {
        let mut checkpoint = Checkpoint::new("test", 5);

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
        let checkpoint = Checkpoint::new("test-session", 10);

        // Save
        checkpoint.save(dir.path()).unwrap();

        // Load
        let loaded = Checkpoint::load(dir.path(), "test-session").unwrap();
        assert_eq!(loaded.session_id, "test-session");
        assert_eq!(loaded.total_phases, 10);
    }
}
