//! AI Data commands for the AI Data Viewer.
//!
//! These commands expose the data that is available to AI via MCP,
//! allowing the frontend to display it in the same format.

use crate::commands::AppState;
use crate::database::TaskRun;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;
use tracing::warn;

/// Response wrapper for AI data commands.
#[derive(Debug, Serialize)]
pub struct AiDataResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T> AiDataResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
        }
    }
}

/// Get task runs for the AI Data Viewer.
#[tauri::command]
pub async fn get_task_runs_for_viewer(
    state: State<'_, Arc<AppState>>,
    limit: Option<u32>,
) -> Result<AiDataResponse<Vec<TaskRun>>, String> {
    let limit = limit.unwrap_or(20);

    match state.checkpoint_db.get_recent_task_runs(limit) {
        Ok(runs) => Ok(AiDataResponse::ok(runs)),
        Err(e) => Ok(AiDataResponse::err(e)),
    }
}

/// Get a specific task run with full output.
#[tauri::command]
pub async fn get_task_run_for_viewer(
    state: State<'_, Arc<AppState>>,
    task_id: String,
) -> Result<AiDataResponse<TaskRun>, String> {
    match state.checkpoint_db.get_task_run(&task_id) {
        Ok(Some(run)) => Ok(AiDataResponse::ok(run)),
        Ok(None) => Ok(AiDataResponse::err(format!(
            "Task run not found: {}",
            task_id
        ))),
        Err(e) => Ok(AiDataResponse::err(e)),
    }
}

/// JSONL log entry (generic).
#[derive(Debug, Serialize, Deserialize)]
pub struct JsonlLogEntry {
    /// The raw JSON value
    #[serde(flatten)]
    pub data: serde_json::Value,
}

/// Result of reading JSONL logs.
#[derive(Debug, Serialize)]
pub struct JsonlLogsResult {
    pub log_type: String,
    pub entries: Vec<serde_json::Value>,
    pub count: usize,
    pub file_path: String,
    pub file_exists: bool,
}

/// Get the .dev-logs directory path.
fn get_dev_logs_dir() -> PathBuf {
    // The .dev-logs directory is in the parent directory
    PathBuf::from(r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs")
}

/// Read JSONL log file and return entries.
fn read_jsonl_file(path: &PathBuf, limit: usize) -> Result<Vec<serde_json::Value>, String> {
    if !path.exists() {
        return Ok(vec![]);
    }

    let content = fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

    let mut entries: Vec<serde_json::Value> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    // Return most recent entries (last N)
    if entries.len() > limit {
        entries = entries.split_off(entries.len() - limit);
    }

    Ok(entries)
}

/// Read JSONL logs from .dev-logs directory.
#[tauri::command]
pub async fn read_jsonl_logs_for_viewer(
    log_type: String,
    limit: Option<u32>,
) -> Result<AiDataResponse<JsonlLogsResult>, String> {
    let limit = limit.unwrap_or(100) as usize;
    let dev_logs_dir = get_dev_logs_dir();

    let filename = match log_type.as_str() {
        "general" => "runner-general.jsonl",
        "actions" => "runner-actions.jsonl",
        "image-recognition" => "runner-image-recognition.jsonl",
        "playwright" => "runner-playwright.jsonl",
        "ai-output" => "ai-output.jsonl",
        _ => {
            return Ok(AiDataResponse::err(format!(
                "Unknown log type: {}. Valid types: general, actions, image-recognition, playwright, ai-output",
                log_type
            )));
        }
    };

    let file_path = dev_logs_dir.join(filename);
    let file_exists = file_path.exists();
    let file_path_str = file_path.to_string_lossy().to_string();

    match read_jsonl_file(&file_path, limit) {
        Ok(entries) => {
            let count = entries.len();
            Ok(AiDataResponse::ok(JsonlLogsResult {
                log_type,
                entries,
                count,
                file_path: file_path_str,
                file_exists,
            }))
        }
        Err(e) => {
            warn!("Failed to read JSONL logs: {}", e);
            Ok(AiDataResponse::err(e))
        }
    }
}

/// Summary of all available JSONL logs.
#[derive(Debug, Serialize)]
pub struct JsonlLogsSummary {
    pub general: JsonlLogFileInfo,
    pub actions: JsonlLogFileInfo,
    pub image_recognition: JsonlLogFileInfo,
    pub playwright: JsonlLogFileInfo,
    pub ai_output: JsonlLogFileInfo,
}

#[derive(Debug, Serialize)]
pub struct JsonlLogFileInfo {
    pub file_path: String,
    pub file_exists: bool,
    pub entry_count: usize,
}

/// Get summary of all JSONL log files.
#[tauri::command]
pub async fn get_jsonl_logs_summary() -> Result<AiDataResponse<JsonlLogsSummary>, String> {
    let dev_logs_dir = get_dev_logs_dir();

    fn get_file_info(dir: &PathBuf, filename: &str) -> JsonlLogFileInfo {
        let path = dir.join(filename);
        let exists = path.exists();
        let count = if exists {
            fs::read_to_string(&path)
                .map(|c| c.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(0)
        } else {
            0
        };
        JsonlLogFileInfo {
            file_path: path.to_string_lossy().to_string(),
            file_exists: exists,
            entry_count: count,
        }
    }

    Ok(AiDataResponse::ok(JsonlLogsSummary {
        general: get_file_info(&dev_logs_dir, "runner-general.jsonl"),
        actions: get_file_info(&dev_logs_dir, "runner-actions.jsonl"),
        image_recognition: get_file_info(&dev_logs_dir, "runner-image-recognition.jsonl"),
        playwright: get_file_info(&dev_logs_dir, "runner-playwright.jsonl"),
        ai_output: get_file_info(&dev_logs_dir, "ai-output.jsonl"),
    }))
}
