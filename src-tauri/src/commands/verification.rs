//! Verification commands for AI self-healing workflow
//!
//! Provides commands for persisting pending verification state across
//! runner restarts, enabling the AI to verify fixes after restart.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info, warn};

use super::CommandResponse;

/// Pending verification state structure
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PendingVerification {
    /// Unique identifier for this verification
    pub id: String,
    /// Timestamp when the verification was created (ms since epoch)
    pub created_at: i64,
    /// Reason for the verification
    pub reason: String, // "fix_applied" or "restart_requested"
    /// Summary of what was being discussed
    pub conversation_summary: String,
    /// Issue IDs that were fixed
    pub issues_fixed: Vec<String>,
    /// Files that were modified
    pub files_modified: Vec<String>,
    /// Prompt to run for verification
    pub verification_prompt: String,
    /// Current status
    pub status: String, // "pending", "in_progress", "completed", "failed"
}

/// Get the path to the pending verification file
fn get_pending_verification_path() -> PathBuf {
    crate::paths::get_dev_logs_dir().join("pending-verification.json")
}

/// Verification expiry time in milliseconds (24 hours)
const VERIFICATION_EXPIRY_MS: i64 = 24 * 60 * 60 * 1000;

/// Save a pending verification to disk
#[tauri::command]
pub fn save_pending_verification(verification: PendingVerification) -> CommandResponse {
    let file_path = get_pending_verification_path();

    // Ensure directory exists
    if let Some(parent) = file_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            error!("Failed to create verification directory: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to create directory: {}", e)),
                data: None,
            };
        }
    }

    // Serialize to JSON
    let json = match serde_json::to_string_pretty(&verification) {
        Ok(json) => json,
        Err(e) => {
            error!("Failed to serialize pending verification: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to serialize: {}", e)),
                data: None,
            };
        }
    };

    // Write to file
    if let Err(e) = fs::write(&file_path, json) {
        error!("Failed to write pending verification: {}", e);
        return CommandResponse {
            success: false,
            message: Some(format!("Failed to write: {}", e)),
            data: None,
        };
    }

    info!(
        "Saved pending verification: {} (reason: {})",
        verification.id, verification.reason
    );

    CommandResponse {
        success: true,
        message: Some("Pending verification saved".to_string()),
        data: None,
    }
}

/// Load the pending verification from disk
/// Returns null if no pending verification exists or if it has expired
#[tauri::command]
pub fn load_pending_verification() -> CommandResponse {
    let file_path = get_pending_verification_path();

    if !file_path.exists() {
        info!("No pending verification file exists");
        return CommandResponse {
            success: true,
            message: Some("No pending verification".to_string()),
            data: Some(serde_json::json!({
                "verification": null
            })),
        };
    }

    let contents = match fs::read_to_string(&file_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to read pending verification: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to read: {}", e)),
                data: None,
            };
        }
    };

    let verification: PendingVerification = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse pending verification, clearing: {}", e);
            let _ = fs::remove_file(&file_path);
            return CommandResponse {
                success: true,
                message: Some("Invalid verification file cleared".to_string()),
                data: Some(serde_json::json!({
                    "verification": null
                })),
            };
        }
    };

    // Check if expired (24 hours)
    let now = chrono::Utc::now().timestamp_millis();
    if now - verification.created_at > VERIFICATION_EXPIRY_MS {
        warn!(
            "Pending verification expired (created {} ms ago), clearing",
            now - verification.created_at
        );
        let _ = fs::remove_file(&file_path);
        return CommandResponse {
            success: true,
            message: Some("Expired verification cleared".to_string()),
            data: Some(serde_json::json!({
                "verification": null
            })),
        };
    }

    info!(
        "Loaded pending verification: {} (status: {})",
        verification.id, verification.status
    );

    CommandResponse {
        success: true,
        message: Some("Loaded pending verification".to_string()),
        data: Some(serde_json::json!({
            "verification": verification
        })),
    }
}

/// Clear the pending verification file
#[tauri::command]
pub fn clear_pending_verification() -> CommandResponse {
    let file_path = get_pending_verification_path();

    if file_path.exists() {
        if let Err(e) = fs::remove_file(&file_path) {
            error!("Failed to clear pending verification: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to clear: {}", e)),
                data: None,
            };
        }
        info!("Cleared pending verification");
    }

    CommandResponse {
        success: true,
        message: Some("Pending verification cleared".to_string()),
        data: None,
    }
}

/// Update the status of the pending verification
#[tauri::command]
pub fn update_verification_status(status: String) -> CommandResponse {
    let file_path = get_pending_verification_path();

    if !file_path.exists() {
        return CommandResponse {
            success: false,
            message: Some("No pending verification to update".to_string()),
            data: None,
        };
    }

    let contents = match fs::read_to_string(&file_path) {
        Ok(c) => c,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to read: {}", e)),
                data: None,
            };
        }
    };

    let mut verification: PendingVerification = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to parse: {}", e)),
                data: None,
            };
        }
    };

    verification.status = status.clone();

    let json = match serde_json::to_string_pretty(&verification) {
        Ok(j) => j,
        Err(e) => {
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to serialize: {}", e)),
                data: None,
            };
        }
    };

    if let Err(e) = fs::write(&file_path, json) {
        return CommandResponse {
            success: false,
            message: Some(format!("Failed to write: {}", e)),
            data: None,
        };
    }

    info!("Updated verification status to: {}", status);

    CommandResponse {
        success: true,
        message: Some(format!("Status updated to: {}", status)),
        data: None,
    }
}
