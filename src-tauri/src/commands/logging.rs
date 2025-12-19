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
#[derive(Debug, Serialize, Deserialize)]
pub struct AiOutputEntry {
    pub id: String,
    pub timestamp: i64,
    pub line: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
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
