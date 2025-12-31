//! Logging commands for AI output persistence
//!
//! Provides commands for persisting AI output logs to disk for use by
//! the QA feedback loop in `/analyze-automation`.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use tracing::{error, info, warn};

use super::CommandResponse;

/// AI output entry structure (matches TypeScript AiOutputEntry)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AiOutputEntry {
    pub id: String,
    pub timestamp: i64,
    pub line: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// Session/workflow ID for grouping loops by workflow
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Human-readable session/workflow name (the task title)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Path to screenshot file (relative to .dev-logs/)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    /// Screenshot width in pixels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_width: Option<i32>,
    /// Screenshot height in pixels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_height: Option<i32>,
}

/// Get the path to the AI output log file
fn get_ai_output_log_path() -> PathBuf {
    // Use the same .dev-logs directory as other logs
    let base_dir = PathBuf::from(r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs");
    base_dir.join("ai-output.jsonl")
}

/// Append an AI output entry to the log file
#[tauri::command]
pub fn append_ai_output_log(entry: AiOutputEntry) -> CommandResponse {
    let log_path = get_ai_output_log_path();

    // Ensure directory exists
    if let Some(parent) = log_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            error!("Failed to create log directory: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to create log directory: {}", e)),
                data: None,
            };
        }
    }

    // Serialize to JSON
    let json_line = match serde_json::to_string(&entry) {
        Ok(json) => json,
        Err(e) => {
            error!("Failed to serialize AI output entry: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to serialize: {}", e)),
                data: None,
            };
        }
    };

    // Append to file
    let mut file = match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(f) => f,
        Err(e) => {
            error!("Failed to open AI output log file: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to open log file: {}", e)),
                data: None,
            };
        }
    };

    if let Err(e) = writeln!(file, "{}", json_line) {
        error!("Failed to write to AI output log: {}", e);
        return CommandResponse {
            success: false,
            message: Some(format!("Failed to write: {}", e)),
            data: None,
        };
    }

    CommandResponse {
        success: true,
        message: None,
        data: None,
    }
}

/// Clear the AI output log file (called when runner starts manually)
#[tauri::command]
pub fn clear_ai_output_log() -> CommandResponse {
    let log_path = get_ai_output_log_path();

    if log_path.exists() {
        if let Err(e) = fs::remove_file(&log_path) {
            error!("Failed to clear AI output log: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to clear log: {}", e)),
                data: None,
            };
        }
        info!("Cleared AI output log");
    }

    CommandResponse {
        success: true,
        message: Some("AI output log cleared".to_string()),
        data: None,
    }
}

/// Get the path to the AI output log file
#[tauri::command]
pub fn get_ai_output_log_path_cmd() -> CommandResponse {
    let log_path = get_ai_output_log_path();
    CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "path": log_path.to_string_lossy()
        })),
    }
}

/// Load all AI output entries from the log file
/// Used to restore conversation history on app startup
#[tauri::command]
pub fn load_ai_output_log() -> CommandResponse {
    let log_path = get_ai_output_log_path();

    if !log_path.exists() {
        info!("No AI output log file exists, returning empty history");
        return CommandResponse {
            success: true,
            message: Some("No history file exists".to_string()),
            data: Some(serde_json::json!({
                "entries": []
            })),
        };
    }

    let file = match fs::File::open(&log_path) {
        Ok(f) => f,
        Err(e) => {
            error!("Failed to open AI output log file: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to open log file: {}", e)),
                data: None,
            };
        }
    };

    let reader = BufReader::new(file);
    let mut entries: Vec<AiOutputEntry> = Vec::new();
    let mut line_number = 0;

    for line_result in reader.lines() {
        line_number += 1;
        match line_result {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<AiOutputEntry>(&line) {
                    Ok(entry) => entries.push(entry),
                    Err(e) => {
                        warn!(
                            "Failed to parse AI output entry at line {}: {}",
                            line_number, e
                        );
                        // Continue parsing other lines
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to read line {} from AI output log: {}",
                    line_number, e
                );
            }
        }
    }

    info!("Loaded {} AI output entries from log file", entries.len());

    CommandResponse {
        success: true,
        message: Some(format!("Loaded {} entries", entries.len())),
        data: Some(serde_json::json!({
            "entries": entries
        })),
    }
}

/// Get the base directory for dev logs
fn get_dev_logs_dir() -> PathBuf {
    PathBuf::from(r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs")
}

/// List all session checkpoint files
/// Returns session IDs that have checkpoint files
#[tauri::command]
pub fn list_session_checkpoints() -> CommandResponse {
    let dev_logs_dir = get_dev_logs_dir();

    if !dev_logs_dir.exists() {
        return CommandResponse {
            success: true,
            message: Some("No dev-logs directory".to_string()),
            data: Some(serde_json::json!({
                "session_ids": []
            })),
        };
    }

    let mut session_ids: Vec<String> = Vec::new();

    match fs::read_dir(&dev_logs_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                // Match pattern: session-{id}-checkpoint.json
                if name.starts_with("session-") && name.ends_with("-checkpoint.json") {
                    // Extract session ID
                    let id = name
                        .strip_prefix("session-")
                        .and_then(|s| s.strip_suffix("-checkpoint.json"))
                        .map(|s| s.to_string());
                    if let Some(id) = id {
                        session_ids.push(id);
                    }
                }
            }
        }
        Err(e) => {
            error!("Failed to read dev-logs directory: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to read directory: {}", e)),
                data: None,
            };
        }
    }

    info!("Found {} session checkpoint files", session_ids.len());

    CommandResponse {
        success: true,
        message: Some(format!("Found {} checkpoints", session_ids.len())),
        data: Some(serde_json::json!({
            "session_ids": session_ids
        })),
    }
}

/// Delete session checkpoint files by session IDs
/// Also cleans up session manager state file if all sessions are deleted
#[tauri::command]
pub fn delete_session_checkpoints(session_ids: Vec<String>) -> CommandResponse {
    let dev_logs_dir = get_dev_logs_dir();

    if !dev_logs_dir.exists() {
        return CommandResponse {
            success: true,
            message: Some("No dev-logs directory".to_string()),
            data: Some(serde_json::json!({
                "deleted_count": 0
            })),
        };
    }

    let mut deleted_count = 0;
    let mut errors: Vec<String> = Vec::new();

    for session_id in &session_ids {
        let checkpoint_path = dev_logs_dir.join(format!("session-{}-checkpoint.json", session_id));

        if checkpoint_path.exists() {
            match fs::remove_file(&checkpoint_path) {
                Ok(_) => {
                    info!("Deleted checkpoint for session {}", session_id);
                    deleted_count += 1;
                }
                Err(e) => {
                    let err_msg = format!("Failed to delete checkpoint {}: {}", session_id, e);
                    error!("{}", err_msg);
                    errors.push(err_msg);
                }
            }
        }
    }

    // Also clean up session-manager-state.json if it exists and all sessions deleted
    let state_file_path = dev_logs_dir.join("session-manager-state.json");
    if state_file_path.exists() {
        // Try to remove it - it will be recreated if there are active sessions
        if let Err(e) = fs::remove_file(&state_file_path) {
            warn!("Failed to remove session manager state file: {}", e);
        } else {
            info!("Cleaned up session manager state file");
        }
    }

    if errors.is_empty() {
        CommandResponse {
            success: true,
            message: Some(format!("Deleted {} checkpoint(s)", deleted_count)),
            data: Some(serde_json::json!({
                "deleted_count": deleted_count
            })),
        }
    } else {
        CommandResponse {
            success: false,
            message: Some(format!(
                "Deleted {} checkpoint(s) with {} error(s): {}",
                deleted_count,
                errors.len(),
                errors.join("; ")
            )),
            data: Some(serde_json::json!({
                "deleted_count": deleted_count,
                "errors": errors
            })),
        }
    }
}

/// Delete all session checkpoints and AI output log
/// Complete cleanup of run history
#[tauri::command]
pub fn clear_all_run_history() -> CommandResponse {
    let dev_logs_dir = get_dev_logs_dir();

    let mut deleted_checkpoints = 0;
    let mut errors: Vec<String> = Vec::new();

    // Delete all session checkpoint files
    if dev_logs_dir.exists() {
        match fs::read_dir(&dev_logs_dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let file_name = entry.file_name();
                    let name = file_name.to_string_lossy();
                    // Match checkpoint files and state files
                    if (name.starts_with("session-") && name.ends_with("-checkpoint.json"))
                        || name == "session-manager-state.json"
                    {
                        if let Err(e) = fs::remove_file(entry.path()) {
                            errors.push(format!("Failed to delete {}: {}", name, e));
                        } else {
                            deleted_checkpoints += 1;
                        }
                    }
                }
            }
            Err(e) => {
                errors.push(format!("Failed to read dev-logs directory: {}", e));
            }
        }
    }

    // Clear AI output log
    let log_path = get_ai_output_log_path();
    let log_deleted = if log_path.exists() {
        match fs::remove_file(&log_path) {
            Ok(_) => true,
            Err(e) => {
                errors.push(format!("Failed to delete AI output log: {}", e));
                false
            }
        }
    } else {
        false
    };

    info!(
        "Cleared run history: {} checkpoints, log deleted: {}",
        deleted_checkpoints, log_deleted
    );

    if errors.is_empty() {
        CommandResponse {
            success: true,
            message: Some(format!(
                "Cleared {} checkpoint(s) and AI output log",
                deleted_checkpoints
            )),
            data: Some(serde_json::json!({
                "deleted_checkpoints": deleted_checkpoints,
                "log_deleted": log_deleted
            })),
        }
    } else {
        CommandResponse {
            success: false,
            message: Some(format!(
                "Partial cleanup: {} checkpoint(s), errors: {}",
                deleted_checkpoints,
                errors.join("; ")
            )),
            data: Some(serde_json::json!({
                "deleted_checkpoints": deleted_checkpoints,
                "log_deleted": log_deleted,
                "errors": errors
            })),
        }
    }
}
