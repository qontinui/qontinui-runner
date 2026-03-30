//! AI Data commands for the AI Data Viewer.
//!
//! These commands expose the data that is available to AI via MCP,
//! allowing the frontend to display it in the same format.

use crate::commands::AppState;
use crate::database::TaskRun;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
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

    let result = state.pg_db.get_recent_task_runs(limit, None).await;
    match result {
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
    let result = state.pg_db.get_task_run(&task_id).await;
    match result {
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
    pub task_run_id: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

/// Get the .dev-logs directory path.
fn get_dev_logs_dir() -> PathBuf {
    crate::paths::get_dev_logs_dir()
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
                task_run_id: None,
                start_time: None,
                end_time: None,
            }))
        }
        Err(e) => {
            warn!("Failed to read JSONL logs: {}", e);
            Ok(AiDataResponse::err(e))
        }
    }
}

/// Parse timestamp from a JSONL entry.
/// Supports various timestamp formats found in log entries.
fn parse_jsonl_timestamp(entry: &serde_json::Value) -> Option<DateTime<Utc>> {
    let timestamp_value = entry
        .get("timestamp")
        .or_else(|| entry.get("time"))
        .or_else(|| entry.get("ts"))?;

    // Try numeric timestamp (epoch milliseconds)
    if let Some(ms) = timestamp_value.as_i64() {
        return DateTime::from_timestamp_millis(ms).map(|dt| dt.with_timezone(&Utc));
    }

    // Try numeric as u64
    if let Some(ms) = timestamp_value.as_u64() {
        return DateTime::from_timestamp_millis(ms as i64).map(|dt| dt.with_timezone(&Utc));
    }

    // Try numeric as f64 (some systems use float timestamps)
    if let Some(ms) = timestamp_value.as_f64() {
        return DateTime::from_timestamp_millis(ms as i64).map(|dt| dt.with_timezone(&Utc));
    }

    // Try string formats
    let timestamp_str = timestamp_value.as_str()?;

    // Try RFC3339 format first (e.g., "2026-01-07T10:30:00Z")
    if let Ok(dt) = DateTime::parse_from_rfc3339(timestamp_str) {
        return Some(dt.with_timezone(&Utc));
    }

    // Try ISO format without timezone (e.g., "2026-01-07T10:30:00")
    if let Ok(dt) = NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%dT%H:%M:%S") {
        return Some(DateTime::from_naive_utc_and_offset(dt, Utc));
    }

    // Try simple format (e.g., "2026-01-07 10:30:00")
    if let Ok(dt) = NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S") {
        return Some(DateTime::from_naive_utc_and_offset(dt, Utc));
    }

    // Try milliseconds format
    if let Ok(dt) = NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%dT%H:%M:%S%.3f") {
        return Some(DateTime::from_naive_utc_and_offset(dt, Utc));
    }

    None
}

/// Read JSONL log file and return entries filtered by time range.
fn read_jsonl_file_in_time_range(
    path: &PathBuf,
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
) -> Result<Vec<serde_json::Value>, String> {
    if !path.exists() {
        return Ok(vec![]);
    }

    let content = fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;
    let end = end_time.unwrap_or_else(Utc::now);

    let entries: Vec<serde_json::Value> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|entry: &serde_json::Value| {
            if let Some(ts) = parse_jsonl_timestamp(entry) {
                ts >= start_time && ts <= end
            } else {
                // Include entries without parseable timestamps
                true
            }
        })
        .collect();

    Ok(entries)
}

/// Read JSONL logs filtered by task run time range.
#[tauri::command]
pub async fn read_jsonl_logs_for_task_run(
    state: State<'_, Arc<AppState>>,
    log_type: String,
    task_run_id: String,
) -> Result<AiDataResponse<JsonlLogsResult>, String> {
    // Get the task run to get time range
    let task_run_result = state.pg_db.get_task_run(&task_run_id).await;
    let task_run = match task_run_result {
        Ok(Some(run)) => run,
        Ok(None) => {
            return Ok(AiDataResponse::err(format!(
                "Task run not found: {}",
                task_run_id
            )));
        }
        Err(e) => return Ok(AiDataResponse::err(e)),
    };

    // Parse task run times
    let start_time = DateTime::parse_from_rfc3339(&task_run.created_at)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| format!("Failed to parse start time: {}", e))?;

    let end_time = task_run
        .completed_at
        .as_ref()
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&Utc));

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

    match read_jsonl_file_in_time_range(&file_path, start_time, end_time) {
        Ok(entries) => {
            let count = entries.len();
            Ok(AiDataResponse::ok(JsonlLogsResult {
                log_type,
                entries,
                count,
                file_path: file_path_str,
                file_exists,
                task_run_id: Some(task_run_id),
                start_time: Some(task_run.created_at),
                end_time: task_run.completed_at,
            }))
        }
        Err(e) => {
            warn!("Failed to read JSONL logs: {}", e);
            Ok(AiDataResponse::err(e))
        }
    }
}

// =============================================================================
// Consolidated AI Output (grouped by source with readable format)
// =============================================================================

/// A chunk of consolidated AI output from a single source.
#[derive(Debug, Serialize)]
pub struct AiOutputChunk {
    /// Source of the output (e.g., "claude", "prompt")
    pub source: String,
    /// Start time of this chunk (formatted as HH:MM:SS)
    pub start_time: String,
    /// End time of this chunk (formatted as HH:MM:SS), None if single entry
    pub end_time: Option<String>,
    /// Combined content from all entries in this chunk
    pub content: String,
    /// Number of raw entries that were consolidated
    pub entry_count: usize,
}

/// Result of reading consolidated AI output.
#[derive(Debug, Serialize)]
pub struct ConsolidatedAiOutputResult {
    pub chunks: Vec<AiOutputChunk>,
    pub total_entries: usize,
    pub task_run_id: String,
    pub start_time: String,
    pub end_time: Option<String>,
}

/// Read and consolidate AI output logs for a task run.
/// Groups consecutive entries by source into readable chunks.
#[tauri::command]
pub async fn get_consolidated_ai_output(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<AiDataResponse<ConsolidatedAiOutputResult>, String> {
    // Get the task run to get time range
    let task_run_result = state.pg_db.get_task_run(&task_run_id).await;
    let task_run = match task_run_result {
        Ok(Some(run)) => run,
        Ok(None) => {
            return Ok(AiDataResponse::err(format!(
                "Task run not found: {}",
                task_run_id
            )));
        }
        Err(e) => return Ok(AiDataResponse::err(e)),
    };

    // Parse task run times
    let start_time = DateTime::parse_from_rfc3339(&task_run.created_at)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| format!("Failed to parse start time: {}", e))?;

    let end_time = task_run
        .completed_at
        .as_ref()
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let dev_logs_dir = get_dev_logs_dir();
    let file_path = dev_logs_dir.join("ai-output.jsonl");

    if !file_path.exists() {
        return Ok(AiDataResponse::ok(ConsolidatedAiOutputResult {
            chunks: vec![],
            total_entries: 0,
            task_run_id,
            start_time: task_run.created_at.clone(),
            end_time: task_run.completed_at.clone(),
        }));
    }

    // Read and filter entries
    let content =
        fs::read_to_string(&file_path).map_err(|e| format!("Failed to read file: {}", e))?;

    let end = end_time.unwrap_or_else(Utc::now);

    // Parse entries with timestamp and source
    struct ParsedEntry {
        timestamp: DateTime<Utc>,
        source: String,
        line: String,
    }

    let mut entries: Vec<ParsedEntry> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|entry| {
            let ts = parse_jsonl_timestamp(&entry)?;
            if ts < start_time || ts > end {
                return None;
            }
            let source = entry.get("source")?.as_str()?.to_string();
            let line = entry.get("line")?.as_str()?.to_string();
            Some(ParsedEntry {
                timestamp: ts,
                source,
                line,
            })
        })
        .collect();

    let total_entries = entries.len();

    // Sort by timestamp
    entries.sort_by_key(|e| e.timestamp);

    // Consolidate consecutive entries by source
    let mut chunks: Vec<AiOutputChunk> = Vec::new();
    let mut current_source: Option<String> = None;
    let mut current_lines: Vec<String> = Vec::new();
    let mut chunk_start: Option<DateTime<Utc>> = None;
    let mut chunk_end: Option<DateTime<Utc>> = None;
    let mut chunk_count: usize = 0;

    for entry in entries {
        if current_source.as_ref() == Some(&entry.source) {
            // Same source, add to current chunk
            current_lines.push(entry.line);
            chunk_end = Some(entry.timestamp);
            chunk_count += 1;
        } else {
            // Different source, finalize current chunk and start new one
            if let Some(source) = current_source.take() {
                if !current_lines.is_empty() {
                    chunks.push(AiOutputChunk {
                        source,
                        start_time: chunk_start
                            .map(|t| t.format("%H:%M:%S").to_string())
                            .unwrap_or_default(),
                        end_time: if chunk_count > 1 {
                            chunk_end.map(|t| t.format("%H:%M:%S").to_string())
                        } else {
                            None
                        },
                        content: current_lines.join("\n"),
                        entry_count: chunk_count,
                    });
                }
            }
            // Start new chunk
            current_source = Some(entry.source);
            current_lines = vec![entry.line];
            chunk_start = Some(entry.timestamp);
            chunk_end = Some(entry.timestamp);
            chunk_count = 1;
        }
    }

    // Don't forget the last chunk
    if let Some(source) = current_source {
        if !current_lines.is_empty() {
            chunks.push(AiOutputChunk {
                source,
                start_time: chunk_start
                    .map(|t| t.format("%H:%M:%S").to_string())
                    .unwrap_or_default(),
                end_time: if chunk_count > 1 {
                    chunk_end.map(|t| t.format("%H:%M:%S").to_string())
                } else {
                    None
                },
                content: current_lines.join("\n"),
                entry_count: chunk_count,
            });
        }
    }

    Ok(AiDataResponse::ok(ConsolidatedAiOutputResult {
        chunks,
        total_entries,
        task_run_id,
        start_time: task_run.created_at,
        end_time: task_run.completed_at,
    }))
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

    fn get_file_info(dir: &Path, filename: &str) -> JsonlLogFileInfo {
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

/// Reopen a finished task run to add more iterations.
/// This allows continuing a task that didn't achieve its goal.
#[tauri::command]
pub async fn reopen_task_run(
    state: State<'_, Arc<AppState>>,
    task_id: String,
    additional_sessions: u32,
) -> Result<AiDataResponse<TaskRun>, String> {
    // Validate additional_sessions
    if additional_sessions == 0 {
        return Ok(AiDataResponse::err(
            "additional_sessions must be greater than 0".to_string(),
        ));
    }
    if additional_sessions > 100 {
        return Ok(AiDataResponse::err(
            "additional_sessions cannot exceed 100".to_string(),
        ));
    }

    match state
        .checkpoint_db
        .reopen_task_run(&task_id, additional_sessions)
    {
        Ok(run) => Ok(AiDataResponse::ok(run)),
        Err(e) => Ok(AiDataResponse::err(e)),
    }
}

// =============================================================================
// Text Log Files (plain text, not JSONL) - Filtered by Task Run
// =============================================================================

use chrono::{DateTime, NaiveDateTime, Utc};

/// Result of reading text logs for a task run.
#[derive(Debug, Serialize)]
pub struct TextLogsResult {
    pub log_type: String,
    pub content: String,
    pub line_count: usize,
    pub file_path: String,
    pub file_exists: bool,
    pub task_run_id: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

/// Info about a single text log file for a task run.
#[derive(Debug, Serialize)]
pub struct TextLogFileInfo {
    pub log_type: String,
    pub file_path: String,
    pub file_exists: bool,
    pub line_count: usize,
}

/// Summary of all text log files for a task run.
#[derive(Debug, Serialize)]
pub struct TextLogsSummary {
    pub task_run_id: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub logs: Vec<TextLogFileInfo>,
}

/// Parse timestamp from a log line.
/// Supports format: "2026-01-06 18:22:53" at the start of the line
fn parse_log_timestamp(line: &str) -> Option<DateTime<Utc>> {
    // Format: "2026-01-06 18:22:53 [info ..."
    if line.len() < 19 {
        return None;
    }
    let timestamp_str = &line[..19];
    NaiveDateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc))
}

/// Read log lines filtered by time range.
fn read_logs_in_time_range(
    path: &PathBuf,
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
) -> Result<(String, usize), String> {
    if !path.exists() {
        return Ok((String::new(), 0));
    }

    let content = fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;
    let end = end_time.unwrap_or_else(Utc::now);

    let filtered_lines: Vec<&str> = content
        .lines()
        .filter(|line| {
            if let Some(ts) = parse_log_timestamp(line) {
                ts >= start_time && ts <= end
            } else {
                false // Skip lines without parseable timestamps
            }
        })
        .collect();

    let line_count = filtered_lines.len();
    Ok((filtered_lines.join("\n"), line_count))
}

/// Count log lines in time range (for summary).
fn count_logs_in_time_range(
    path: &PathBuf,
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
) -> usize {
    if !path.exists() {
        return 0;
    }

    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return 0,
    };

    let end = end_time.unwrap_or_else(Utc::now);

    content
        .lines()
        .filter(|line| {
            if let Some(ts) = parse_log_timestamp(line) {
                ts >= start_time && ts <= end
            } else {
                false
            }
        })
        .count()
}

/// Get the filename for a log type.
fn get_log_filename(log_type: &str) -> Option<&'static str> {
    match log_type {
        "backend" => Some("backend.log"),
        "backend-err" => Some("backend.err.log"),
        _ => None,
    }
}

/// Read text logs for a specific task run.
#[tauri::command]
pub async fn read_text_logs_for_viewer(
    state: State<'_, Arc<AppState>>,
    log_type: String,
    task_run_id: String,
) -> Result<AiDataResponse<TextLogsResult>, String> {
    // Get the task run to get time range
    let task_run_result = state.pg_db.get_task_run(&task_run_id).await;
    let task_run = match task_run_result {
        Ok(Some(run)) => run,
        Ok(None) => {
            return Ok(AiDataResponse::err(format!(
                "Task run not found: {}",
                task_run_id
            )));
        }
        Err(e) => return Ok(AiDataResponse::err(e)),
    };

    // Parse task run times
    let start_time = DateTime::parse_from_rfc3339(&task_run.created_at)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| format!("Failed to parse start time: {}", e))?;

    let end_time = task_run
        .completed_at
        .as_ref()
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let dev_logs_dir = get_dev_logs_dir();

    let filename = match get_log_filename(&log_type) {
        Some(f) => f,
        None => {
            return Ok(AiDataResponse::err(format!(
                "Unknown log type: {}. Valid types: backend, backend-err",
                log_type
            )));
        }
    };

    let file_path = dev_logs_dir.join(filename);
    let file_exists = file_path.exists();
    let file_path_str = file_path.to_string_lossy().to_string();

    match read_logs_in_time_range(&file_path, start_time, end_time) {
        Ok((content, line_count)) => Ok(AiDataResponse::ok(TextLogsResult {
            log_type,
            content,
            line_count,
            file_path: file_path_str,
            file_exists,
            task_run_id: Some(task_run_id),
            start_time: Some(task_run.created_at),
            end_time: task_run.completed_at,
        })),
        Err(e) => {
            warn!("Failed to read text logs: {}", e);
            Ok(AiDataResponse::err(e))
        }
    }
}

/// Get summary of all text log files for a task run.
#[tauri::command]
pub async fn get_text_logs_summary(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<AiDataResponse<TextLogsSummary>, String> {
    // Get the task run to get time range
    let task_run_result = state.pg_db.get_task_run(&task_run_id).await;
    let task_run = match task_run_result {
        Ok(Some(run)) => run,
        Ok(None) => {
            return Ok(AiDataResponse::err(format!(
                "Task run not found: {}",
                task_run_id
            )));
        }
        Err(e) => return Ok(AiDataResponse::err(e)),
    };

    // Parse task run times
    let start_time = DateTime::parse_from_rfc3339(&task_run.created_at)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| format!("Failed to parse start time: {}", e))?;

    let end_time = task_run
        .completed_at
        .as_ref()
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let dev_logs_dir = get_dev_logs_dir();

    let log_types = ["backend", "backend-err"];
    let mut logs = Vec::new();

    for log_type in log_types {
        if let Some(filename) = get_log_filename(log_type) {
            let path = dev_logs_dir.join(filename);
            let exists = path.exists();
            let count = count_logs_in_time_range(&path, start_time, end_time);

            logs.push(TextLogFileInfo {
                log_type: log_type.to_string(),
                file_path: path.to_string_lossy().to_string(),
                file_exists: exists,
                line_count: count,
            });
        }
    }

    Ok(AiDataResponse::ok(TextLogsSummary {
        task_run_id,
        start_time: task_run.created_at,
        end_time: task_run.completed_at,
        logs,
    }))
}

// =============================================================================
// Screenshots
// =============================================================================

/// Screenshot file info
#[derive(Debug, Serialize)]
pub struct ScreenshotInfo {
    pub filename: String,
    pub path: String,
    pub size_bytes: u64,
    pub modified: Option<String>,
}

/// Screenshots result
#[derive(Debug, Serialize)]
pub struct ScreenshotsResult {
    pub annotated: Vec<ScreenshotInfo>,
    pub playwright: Vec<ScreenshotInfo>,
}

fn list_screenshots_in_dir(dir: &PathBuf) -> Vec<ScreenshotInfo> {
    if !dir.exists() {
        return vec![];
    }

    let mut screenshots = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "png" || ext == "jpg" || ext == "jpeg" {
                        let metadata = fs::metadata(&path).ok();
                        screenshots.push(ScreenshotInfo {
                            filename: path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string(),
                            path: path.to_string_lossy().to_string(),
                            size_bytes: metadata.as_ref().map(|m| m.len()).unwrap_or(0),
                            modified: metadata.and_then(|m| m.modified().ok()).map(|t| {
                                let datetime: DateTime<Utc> = t.into();
                                datetime.to_rfc3339()
                            }),
                        });
                    }
                }
            }
        }
    }
    // Sort by modified time, newest first
    screenshots.sort_by(|a, b| b.modified.cmp(&a.modified));
    screenshots
}

/// Get list of screenshots
#[tauri::command]
pub async fn get_screenshots_for_viewer() -> Result<AiDataResponse<ScreenshotsResult>, String> {
    let dev_logs_dir = get_dev_logs_dir();

    let annotated = list_screenshots_in_dir(&dev_logs_dir.join("screenshots"));
    let playwright = list_screenshots_in_dir(&dev_logs_dir.join("playwright-screenshots"));

    Ok(AiDataResponse::ok(ScreenshotsResult {
        annotated,
        playwright,
    }))
}

// =============================================================================
// Loaded Config
// =============================================================================

/// Loaded config info
#[derive(Debug, Serialize)]
pub struct LoadedConfigInfo {
    pub config_content: Option<String>,
    pub config_path: Option<String>,
    pub config_format: Option<String>,
    pub meta: Option<serde_json::Value>,
}

/// Get the currently loaded config
///
/// This returns config information in priority order:
/// 1. Unified workflow config (last-unified-workflow.json) - phase-based workflow
/// 2. GUI automation config (last-loaded-gui-config.json) - model-based GUI automation
/// 3. Legacy config (last-loaded-config.json) - backward compatibility
#[tauri::command]
pub async fn get_loaded_config_for_viewer() -> Result<AiDataResponse<LoadedConfigInfo>, String> {
    let dev_logs_dir = get_dev_logs_dir();

    // Priority 1: Unified workflow config (recommended for AI agents)
    let unified_workflow_path = dev_logs_dir.join("last-unified-workflow.json");
    let unified_workflow_meta = dev_logs_dir.join("last-unified-workflow.meta.json");

    // Priority 2: GUI automation config (model-based GUI automation)
    let gui_json_path = dev_logs_dir.join("last-loaded-gui-config.json");
    let gui_yaml_path = dev_logs_dir.join("last-loaded-gui-config.yaml");
    let gui_meta_path = dev_logs_dir.join("last-loaded-gui-config.meta.json");

    // Priority 3: Legacy config (backward compatibility)
    let legacy_json_path = dev_logs_dir.join("last-loaded-config.json");
    let legacy_yaml_path = dev_logs_dir.join("last-loaded-config.yaml");
    let legacy_meta_path = dev_logs_dir.join("last-loaded-config.meta.json");

    // Try unified workflow first
    if unified_workflow_path.exists() {
        let config_content = fs::read_to_string(&unified_workflow_path).ok();
        let meta = if unified_workflow_meta.exists() {
            fs::read_to_string(&unified_workflow_meta)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        } else {
            None
        };
        return Ok(AiDataResponse::ok(LoadedConfigInfo {
            config_content,
            config_path: Some(unified_workflow_path.to_string_lossy().to_string()),
            config_format: Some("json".to_string()),
            meta,
        }));
    }

    // Try GUI config next
    let (config_content, config_path, config_format) = if gui_json_path.exists() {
        (
            fs::read_to_string(&gui_json_path).ok(),
            Some(gui_json_path.to_string_lossy().to_string()),
            Some("json".to_string()),
        )
    } else if gui_yaml_path.exists() {
        (
            fs::read_to_string(&gui_yaml_path).ok(),
            Some(gui_yaml_path.to_string_lossy().to_string()),
            Some("yaml".to_string()),
        )
    } else if legacy_json_path.exists() {
        // Fall back to legacy path
        (
            fs::read_to_string(&legacy_json_path).ok(),
            Some(legacy_json_path.to_string_lossy().to_string()),
            Some("json".to_string()),
        )
    } else if legacy_yaml_path.exists() {
        (
            fs::read_to_string(&legacy_yaml_path).ok(),
            Some(legacy_yaml_path.to_string_lossy().to_string()),
            Some("yaml".to_string()),
        )
    } else {
        (None, None, None)
    };

    // Get metadata from appropriate path
    let meta = if gui_meta_path.exists() {
        fs::read_to_string(&gui_meta_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    } else if legacy_meta_path.exists() {
        fs::read_to_string(&legacy_meta_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    } else {
        None
    };

    Ok(AiDataResponse::ok(LoadedConfigInfo {
        config_content,
        config_path,
        config_format,
        meta,
    }))
}

// =============================================================================
// AI Prompts (the actual prompts sent to Claude)
// =============================================================================

/// AI prompt info
#[derive(Debug, Serialize)]
pub struct AiPromptInfo {
    pub prompt_file: String,
    pub content: String,
    pub iteration: Option<u32>,
}

/// AI prompts result for a task run
#[derive(Debug, Serialize)]
pub struct AiPromptsResult {
    pub task_run_id: String,
    pub prompts: Vec<AiPromptInfo>,
}

/// Get AI prompts for a task run
#[tauri::command]
pub async fn get_ai_prompts_for_viewer(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<AiDataResponse<AiPromptsResult>, String> {
    // Get the task run to find associated prompt files
    let task_run_result = state.pg_db.get_task_run(&task_run_id).await;
    let task_run = match task_run_result {
        Ok(Some(run)) => run,
        Ok(None) => {
            return Ok(AiDataResponse::err(format!(
                "Task run not found: {}",
                task_run_id
            )));
        }
        Err(e) => return Ok(AiDataResponse::err(e)),
    };

    let dev_logs_dir = get_dev_logs_dir();
    let mut prompts = Vec::new();

    // Look for prompt files that match this task run
    // Pattern: ai-developer-{id}-prompt.txt or ai-developer-{id}-iter{N}-prompt.txt
    if let Ok(entries) = fs::read_dir(&dev_logs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(filename) = path.file_name() {
                    let filename_str = filename.to_string_lossy();

                    // Check if this prompt file belongs to this task run
                    // Task run IDs are like "mjklrc88zspgp" and files are like "ai-developer-mjklrc88zspgp-prompt.txt"
                    if filename_str.contains(&task_run_id) && filename_str.ends_with("-prompt.txt")
                    {
                        if let Ok(content) = fs::read_to_string(&path) {
                            // Determine iteration number
                            let iteration = if filename_str.contains("-iter") {
                                filename_str
                                    .split("-iter")
                                    .nth(1)
                                    .and_then(|s| s.split('-').next())
                                    .and_then(|s| s.parse().ok())
                            } else {
                                Some(1)
                            };

                            prompts.push(AiPromptInfo {
                                prompt_file: filename_str.to_string(),
                                content,
                                iteration,
                            });
                        }
                    }
                }
            }
        }
    }

    // Also check if task run has an embedded prompt
    if prompts.is_empty() {
        if let Some(prompt) = &task_run.prompt {
            if !prompt.is_empty() {
                prompts.push(AiPromptInfo {
                    prompt_file: "embedded".to_string(),
                    content: prompt.clone(),
                    iteration: Some(1),
                });
            }
        }
    }

    // Sort by iteration
    prompts.sort_by_key(|p| p.iteration);

    Ok(AiDataResponse::ok(AiPromptsResult {
        task_run_id,
        prompts,
    }))
}

// =============================================================================
// Contexts (available context snippets)
// =============================================================================

use crate::context;

/// Context info
#[derive(Debug, Serialize)]
pub struct ContextInfo {
    pub id: String,
    pub name: String,
    pub context_type: String, // "builtin", "project", "user"
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub content: String,
    pub enabled: bool,
    pub auto_include: Option<serde_json::Value>,
}

/// Contexts result
#[derive(Debug, Serialize)]
pub struct ContextsResult {
    pub contexts: Vec<ContextInfo>,
}

/// Get available contexts
#[tauri::command]
pub async fn get_contexts_for_viewer() -> Result<AiDataResponse<ContextsResult>, String> {
    // Load user contexts
    let library = context::load_user_context_library();
    let mut contexts = Vec::new();

    // Add user contexts
    for ctx in &library.contexts {
        let metadata = library.metadata.iter().find(|m| m.context_id == ctx.id);
        contexts.push(ContextInfo {
            id: ctx.id.clone(),
            name: ctx.name.clone(),
            context_type: "user".to_string(),
            category: ctx.category.clone(),
            tags: ctx.tags.clone(),
            content: ctx.content.clone(),
            enabled: metadata.map(|m| m.enabled).unwrap_or(true),
            auto_include: ctx.auto_include.as_ref().map(|ai| {
                serde_json::json!({
                    "task_mentions": ai.task_mentions,
                    "action_types": ai.action_types,
                    "error_patterns": ai.error_patterns,
                    "file_patterns": ai.file_patterns,
                })
            }),
        });
    }

    // Add builtin contexts
    for ctx in context::get_builtin_contexts() {
        contexts.push(ContextInfo {
            id: ctx.id.clone(),
            name: ctx.name.clone(),
            context_type: "builtin".to_string(),
            category: ctx.category.clone(),
            tags: ctx.tags.clone(),
            content: ctx.content.clone(),
            enabled: true,
            auto_include: ctx.auto_include.as_ref().map(|ai| {
                serde_json::json!({
                    "task_mentions": ai.task_mentions,
                    "action_types": ai.action_types,
                    "error_patterns": ai.error_patterns,
                    "file_patterns": ai.file_patterns,
                })
            }),
        });
    }

    Ok(AiDataResponse::ok(ContextsResult { contexts }))
}

// =============================================================================
// SQLite Event Queries (migrated from JSONL)
// =============================================================================

use crate::database::{
    TaskRunApiRequest, TaskRunAwasStep, TaskRunEvent, TaskRunPlaywrightResult, TaskRunScreenshot,
};

/// Result of querying task run events from SQLite.
#[derive(Debug, Serialize)]
pub struct TaskRunEventsResult {
    pub task_run_id: String,
    pub events: Vec<TaskRunEvent>,
    pub count: usize,
    pub event_type_filter: Option<String>,
}

/// Get task run events from PG database.
/// This replaces JSONL file reading for historical analysis.
#[tauri::command]
pub async fn get_task_run_events_from_db(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
    event_type: Option<String>,
    limit: Option<u32>,
) -> Result<AiDataResponse<TaskRunEventsResult>, String> {
    let limit = limit.unwrap_or(1000);

    match state
        .pg_db
        .get_task_run_events(&task_run_id, event_type.as_deref(), Some(limit))
        .await
    {
        Ok(events) => {
            let count = events.len();
            Ok(AiDataResponse::ok(TaskRunEventsResult {
                task_run_id,
                events,
                count,
                event_type_filter: event_type,
            }))
        }
        Err(e) => Ok(AiDataResponse::err(e)),
    }
}

/// Result of querying task run screenshots from SQLite.
#[derive(Debug, Serialize)]
pub struct TaskRunScreenshotsResult {
    pub task_run_id: String,
    pub screenshots: Vec<TaskRunScreenshot>,
    pub count: usize,
}

/// Get task run screenshots from PG database.
#[tauri::command]
pub async fn get_task_run_screenshots_from_db(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
    screenshot_type: Option<String>,
) -> Result<AiDataResponse<TaskRunScreenshotsResult>, String> {
    match state
        .pg_db
        .get_task_run_screenshots(&task_run_id, screenshot_type.as_deref())
        .await
    {
        Ok(screenshots) => {
            let count = screenshots.len();
            Ok(AiDataResponse::ok(TaskRunScreenshotsResult {
                task_run_id,
                screenshots,
                count,
            }))
        }
        Err(e) => Ok(AiDataResponse::err(e)),
    }
}

/// Result of querying Playwright results from SQLite.
#[derive(Debug, Serialize)]
pub struct TaskRunPlaywrightResultsResult {
    pub task_run_id: String,
    pub results: Vec<TaskRunPlaywrightResult>,
    pub count: usize,
    pub passed: usize,
    pub failed: usize,
}

/// Get Playwright test results from PG database.
#[tauri::command]
pub async fn get_task_run_playwright_results_from_db(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<AiDataResponse<TaskRunPlaywrightResultsResult>, String> {
    match state
        .pg_db
        .get_task_run_playwright_results(&task_run_id)
        .await
    {
        Ok(results) => {
            let count = results.len();
            let passed = results.iter().filter(|r| r.status == "passed").count();
            let failed = results.iter().filter(|r| r.status == "failed").count();
            Ok(AiDataResponse::ok(TaskRunPlaywrightResultsResult {
                task_run_id,
                results,
                count,
                passed,
                failed,
            }))
        }
        Err(e) => Ok(AiDataResponse::err(e)),
    }
}

/// Combined summary of all migrated log data for a task run.
#[derive(Debug, Serialize)]
pub struct TaskRunMigratedLogsSummary {
    pub task_run_id: String,
    pub events_count: usize,
    pub screenshots_count: usize,
    pub playwright_results_count: usize,
    pub events_by_type: std::collections::HashMap<String, usize>,
}

/// Get summary of migrated logs for a task run.
#[tauri::command]
pub async fn get_task_run_migrated_logs_summary(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<AiDataResponse<TaskRunMigratedLogsSummary>, String> {
    // Get events
    let events = state
        .pg_db
        .get_task_run_events(&task_run_id, None, None)
        .await
        .unwrap_or_default();

    // Count events by type
    let mut events_by_type = std::collections::HashMap::new();
    for event in &events {
        *events_by_type.entry(event.event_type.clone()).or_insert(0) += 1;
    }

    // Get screenshots
    let screenshots = state
        .pg_db
        .get_task_run_screenshots(&task_run_id, None)
        .await
        .unwrap_or_default();

    // Get Playwright results
    let playwright_results = state
        .pg_db
        .get_task_run_playwright_results(&task_run_id)
        .await
        .unwrap_or_default();

    Ok(AiDataResponse::ok(TaskRunMigratedLogsSummary {
        task_run_id,
        events_count: events.len(),
        screenshots_count: screenshots.len(),
        playwright_results_count: playwright_results.len(),
        events_by_type,
    }))
}

/// Result of querying API requests from SQLite.
#[derive(Debug, Serialize)]
pub struct TaskRunApiRequestsResult {
    pub task_run_id: String,
    pub requests: Vec<TaskRunApiRequest>,
    pub count: usize,
    pub success_count: usize,
    pub failed_count: usize,
}

/// Get API requests for a task run from PG database.
#[tauri::command]
pub async fn get_task_run_api_requests_from_db(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
    success_filter: Option<bool>,
) -> Result<AiDataResponse<TaskRunApiRequestsResult>, String> {
    match state
        .pg_db
        .get_task_run_api_requests(&task_run_id)
        .await
    {
        Ok(all_requests) => {
            // Apply success_filter client-side (PG method returns all)
            let requests: Vec<_> = match success_filter {
                Some(filter) => all_requests.into_iter().filter(|r| r.success == filter).collect(),
                None => all_requests,
            };
            let count = requests.len();
            let success_count = requests.iter().filter(|r| r.success).count();
            let failed_count = requests.iter().filter(|r| !r.success).count();
            Ok(AiDataResponse::ok(TaskRunApiRequestsResult {
                task_run_id,
                requests,
                count,
                success_count,
                failed_count,
            }))
        }
        Err(e) => Ok(AiDataResponse::err(e)),
    }
}

/// Result of querying AWAS steps from SQLite.
#[derive(Debug, Serialize)]
pub struct TaskRunAwasStepsResult {
    pub task_run_id: String,
    pub steps: Vec<TaskRunAwasStep>,
    pub count: usize,
    pub success_count: usize,
    pub failed_count: usize,
}

/// Get AWAS steps for a task run from database.
#[tauri::command]
pub async fn get_task_run_awas_steps_from_db(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
    step_type: Option<String>,
) -> Result<AiDataResponse<TaskRunAwasStepsResult>, String> {
    let steps_result = state.pg_db.get_task_run_awas_steps(&task_run_id).await;
    match steps_result {
        Ok(steps) => {
            // Filter by step_type if provided
            let filtered_steps: Vec<TaskRunAwasStep> = if let Some(ref filter_type) = step_type {
                steps
                    .into_iter()
                    .filter(|s| s.step_type == *filter_type)
                    .collect()
            } else {
                steps
            };
            let count = filtered_steps.len();
            let success_count = filtered_steps.iter().filter(|s| s.success).count();
            let failed_count = filtered_steps.iter().filter(|s| !s.success).count();
            Ok(AiDataResponse::ok(TaskRunAwasStepsResult {
                task_run_id,
                steps: filtered_steps,
                count,
                success_count,
                failed_count,
            }))
        }
        Err(e) => Ok(AiDataResponse::err(e)),
    }
}

// =============================================================================
// Verification Phase Results (Unified Workflow Test Results)
// =============================================================================

/// Result of querying verification phase results from SQLite.
#[derive(Debug, Serialize)]
pub struct TaskRunVerificationResultsDbResult {
    pub task_run_id: String,
    /// Results from each verification iteration
    pub results: Vec<serde_json::Value>,
    pub count: usize,
    /// Count of iterations where all tests passed
    pub passed_iterations: usize,
    /// Count of iterations where some tests failed
    pub failed_iterations: usize,
}

/// Get verification phase results for a task run from SQLite database.
///
/// Returns all verification results across all iterations, including individual
/// step results (tests, checks, etc.) from the unified workflow's verification phase.
#[tauri::command]
pub async fn get_task_run_verification_results_from_db(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<AiDataResponse<TaskRunVerificationResultsDbResult>, String> {
    match state
        .checkpoint_db
        .get_all_verification_phase_results(&task_run_id)
    {
        Ok(results) => {
            let count = results.len();
            let passed_iterations = results
                .iter()
                .filter(|r| {
                    r.get("all_passed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .count();
            let failed_iterations = count - passed_iterations;

            Ok(AiDataResponse::ok(TaskRunVerificationResultsDbResult {
                task_run_id,
                results,
                count,
                passed_iterations,
                failed_iterations,
            }))
        }
        Err(e) => Ok(AiDataResponse::err(e)),
    }
}

/// Response type for runtime context query.
#[derive(Debug, Serialize)]
pub struct RuntimeContextResult {
    pub task_run_id: String,
    pub variables: Vec<RuntimeContextVariable>,
}

/// A single context variable for display.
#[derive(Debug, Serialize)]
pub struct RuntimeContextVariable {
    pub name: String,
    pub value: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_step: Option<String>,
}

/// Get runtime context variables for a task run.
///
/// Reads the `runtime_context_json` column from `task_runs` and transforms
/// the stored variables into a flat list for the Context Tab.
#[tauri::command]
pub async fn get_task_run_context(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<AiDataResponse<RuntimeContextResult>, String> {
    let context_json = state
        .checkpoint_db
        .get_task_run_runtime_context(&task_run_id)?;

    let mut variables = Vec::new();

    if let Some(json_str) = context_json {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json_str) {
            // Extract variables from the persisted context
            if let Some(vars) = parsed.get("variables").and_then(|v| v.as_object()) {
                for (name, entry) in vars {
                    let (value, source) = if let Some(obj) = entry.as_object() {
                        let val = obj
                            .get("value")
                            .map(|v| match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default();
                        let src = obj
                            .get("source")
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        (val, src)
                    } else {
                        // Legacy format: plain value
                        let val = match entry {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        (val, "unknown".to_string())
                    };

                    variables.push(RuntimeContextVariable {
                        name: name.clone(),
                        value,
                        source,
                        source_step: None,
                    });
                }
            }

            // Extract replay lineage info as a system variable if present
            if let Some(lineage) = parsed.get("replay_lineage") {
                variables.push(RuntimeContextVariable {
                    name: "replay_lineage".to_string(),
                    value: serde_json::to_string(lineage).unwrap_or_default(),
                    source: "system".to_string(),
                    source_step: None,
                });
            }
            if let Some(checkpoint_id) = parsed.get("source_checkpoint_id") {
                variables.push(RuntimeContextVariable {
                    name: "source_checkpoint_id".to_string(),
                    value: checkpoint_id.as_str().unwrap_or_default().to_string(),
                    source: "system".to_string(),
                    source_step: None,
                });
            }
        }
    }

    // Sort variables: system first, then step, then by name
    variables.sort_by(|a, b| {
        let source_order = |s: &str| match s {
            "system" => 0,
            "user" => 1,
            "step" => 2,
            _ => 3,
        };
        source_order(&a.source)
            .cmp(&source_order(&b.source))
            .then(a.name.cmp(&b.name))
    });

    Ok(AiDataResponse::ok(RuntimeContextResult {
        task_run_id,
        variables,
    }))
}
