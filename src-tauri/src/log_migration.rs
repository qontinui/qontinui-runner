//! Log Migration Module
//!
//! Provides batch migration of JSONL log files to SQLite database.
//! This enables historical queries on events while keeping JSONL for real-time streaming.

use crate::database::{
    CheckpointDb, CreateTaskRunApiRequestInput, CreateTaskRunEventInput,
    CreateTaskRunPlaywrightResultInput, CreateTaskRunScreenshotInput,
};
use crate::iteration_bundle::{ActionEvent, ImageRecognitionEvent, PlaywrightLogs};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use tracing::{debug, info};

/// Result of a log migration operation.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LogMigrationResult {
    pub general_events: usize,
    pub action_events: usize,
    pub image_recognition_events: usize,
    pub screenshots: usize,
    pub playwright_results: usize,
    pub api_requests: usize,
    pub errors: Vec<String>,
}

/// General event from runner-general.jsonl
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeneralEvent {
    timestamp: String,
    level: Option<String>,
    message: String,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

/// AI output event from ai-output.jsonl
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiOutputEvent {
    timestamp: String,
    event_type: String,
    content: Option<String>,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

/// API request event from runner-api-requests.jsonl
/// Matches ApiRequestFileEntry from file_logger.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApiRequestEvent {
    id: String,
    timestamp: String,
    step_id: String,
    step_name: String,
    method: String,
    url: String,
    resolved_url: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    body: Option<String>,
    content_type: Option<String>,
    status_code: u16,
    status_text: String,
    #[serde(default)]
    response_headers: HashMap<String, String>,
    response_time_ms: u64,
    response_body_type: String,
    response_body: Option<String>,
    response_file_path: Option<String>,
    response_size_bytes: usize,
    #[serde(default)]
    extractions: Vec<serde_json::Value>,
    #[serde(default)]
    assertions: Vec<serde_json::Value>,
    success: bool,
    error: Option<String>,
}

/// Migrate JSONL logs to SQLite for a specific task run.
///
/// This reads the current JSONL files in .dev-logs/ and inserts their contents
/// into the SQLite database linked to the specified task_run_id.
///
/// # Arguments
/// * `db` - Database handle
/// * `task_run_id` - The task run to link events to
/// * `dev_logs_dir` - Path to .dev-logs/ directory
/// * `workflow_name` - Optional workflow name for context
pub fn migrate_logs_to_sqlite(
    db: &CheckpointDb,
    task_run_id: &str,
    dev_logs_dir: &Path,
    workflow_name: Option<&str>,
) -> Result<LogMigrationResult, String> {
    let mut result = LogMigrationResult::default();

    // Migrate general events
    let general_path = dev_logs_dir.join("runner-general.jsonl");
    if general_path.exists() {
        match migrate_general_events(db, task_run_id, &general_path, workflow_name) {
            Ok(count) => result.general_events = count,
            Err(e) => result.errors.push(format!("General events: {}", e)),
        }
    }

    // Migrate action events
    let actions_path = dev_logs_dir.join("runner-actions.jsonl");
    if actions_path.exists() {
        match migrate_action_events(db, task_run_id, &actions_path, workflow_name) {
            Ok(count) => result.action_events = count,
            Err(e) => result.errors.push(format!("Action events: {}", e)),
        }
    }

    // Migrate image recognition events and screenshots
    let image_path = dev_logs_dir.join("runner-image-recognition.jsonl");
    if image_path.exists() {
        match migrate_image_recognition_events(db, task_run_id, &image_path, workflow_name) {
            Ok((events, screenshots)) => {
                result.image_recognition_events = events;
                result.screenshots = screenshots;
            }
            Err(e) => result.errors.push(format!("Image recognition: {}", e)),
        }
    }

    // Migrate Playwright results
    let playwright_path = dev_logs_dir.join("runner-playwright.jsonl");
    if playwright_path.exists() {
        match migrate_playwright_results(db, task_run_id, &playwright_path) {
            Ok(count) => result.playwright_results = count,
            Err(e) => result.errors.push(format!("Playwright results: {}", e)),
        }
    }

    // Migrate AI output events
    let ai_output_path = dev_logs_dir.join("ai-output.jsonl");
    if ai_output_path.exists() {
        match migrate_ai_output_events(db, task_run_id, &ai_output_path, workflow_name) {
            Ok(count) => {
                // Add to general events count since they go into the same table
                result.general_events += count;
            }
            Err(e) => result.errors.push(format!("AI output events: {}", e)),
        }
    }

    // Migrate API request events
    let api_requests_path = dev_logs_dir.join("runner-api-requests.jsonl");
    if api_requests_path.exists() {
        match migrate_api_request_events(db, task_run_id, &api_requests_path) {
            Ok(count) => result.api_requests = count,
            Err(e) => result.errors.push(format!("API requests: {}", e)),
        }
    }

    info!(
        "Log migration complete for task {}: {} general, {} actions, {} image recognition, {} screenshots, {} playwright, {} api_requests",
        task_run_id,
        result.general_events,
        result.action_events,
        result.image_recognition_events,
        result.screenshots,
        result.playwright_results,
        result.api_requests
    );

    // Clear migrated JSONL files to prevent unbounded growth.
    // These files are write-ahead buffers — once migrated to DB they are redundant.
    let migrated_files = [
        "runner-general.jsonl",
        "runner-actions.jsonl",
        "runner-image-recognition.jsonl",
        "runner-playwright.jsonl",
        "runner-api-requests.jsonl",
        "ai-output.jsonl",
    ];
    for filename in migrated_files {
        let path = dev_logs_dir.join(filename);
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                debug!("Failed to clear migrated log file {}: {}", filename, e);
            } else {
                debug!("Cleared migrated log file: {}", filename);
            }
        }
    }

    Ok(result)
}

/// Migrate general events from runner-general.jsonl
fn migrate_general_events(
    db: &CheckpointDb,
    task_run_id: &str,
    path: &Path,
    workflow_name: Option<&str>,
) -> Result<usize, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<GeneralEvent>(&line) {
            Ok(event) => {
                let subtype = event.level.as_deref();
                let data = if event.extra.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&event.extra).unwrap_or_default())
                };

                events.push(CreateTaskRunEventInput {
                    task_run_id: task_run_id.to_string(),
                    event_type: "general".to_string(),
                    event_subtype: subtype.map(|s| s.to_string()),
                    message: event.message,
                    data,
                    workflow_name: workflow_name.map(|s| s.to_string()),
                    state_name: None,
                    action_id: None,
                    timestamp: event.timestamp,
                    duration_ms: None,
                });
            }
            Err(e) => {
                debug!("Failed to parse general event: {} - {}", e, line);
            }
        }
    }

    let count = events.len();
    db.batch_create_task_run_events(&events)?;
    Ok(count)
}

/// Migrate action events from runner-actions.jsonl
fn migrate_action_events(
    db: &CheckpointDb,
    task_run_id: &str,
    path: &Path,
    workflow_name: Option<&str>,
) -> Result<usize, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<ActionEvent>(&line) {
            Ok(event) => {
                let data = if event.extra.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&event.extra).unwrap_or_default())
                };

                let message = event
                    .step_name
                    .clone()
                    .unwrap_or_else(|| event.event_type.clone());

                events.push(CreateTaskRunEventInput {
                    task_run_id: task_run_id.to_string(),
                    event_type: "action".to_string(),
                    event_subtype: Some(event.event_type),
                    message,
                    data,
                    workflow_name: workflow_name.map(|s| s.to_string()),
                    state_name: None,
                    action_id: event.step_name,
                    timestamp: event.timestamp,
                    duration_ms: event.duration_ms.map(|d| d as i64),
                });
            }
            Err(e) => {
                debug!("Failed to parse action event: {} - {}", e, line);
            }
        }
    }

    let count = events.len();
    db.batch_create_task_run_events(&events)?;
    Ok(count)
}

/// Migrate image recognition events from runner-image-recognition.jsonl
fn migrate_image_recognition_events(
    db: &CheckpointDb,
    task_run_id: &str,
    path: &Path,
    workflow_name: Option<&str>,
) -> Result<(usize, usize), String> {
    let file = File::open(path).map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
    let reader = BufReader::new(file);
    let mut event_count = 0;
    let mut screenshot_count = 0;

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<ImageRecognitionEvent>(&line) {
            Ok(event) => {
                // Create the event
                let subtype = if event.matched { "match" } else { "no_match" };
                let location_json = event
                    .location
                    .as_ref()
                    .map(|loc| serde_json::to_string(loc).unwrap_or_default());

                let message = format!(
                    "Template '{}': {} (confidence: {:.2}%, threshold: {:.2}%)",
                    event.template_name,
                    if event.matched { "matched" } else { "no match" },
                    event.confidence * 100.0,
                    event.threshold * 100.0
                );

                let data = serde_json::json!({
                    "template_name": event.template_name,
                    "matched": event.matched,
                    "confidence": event.confidence,
                    "threshold": event.threshold,
                    "location": event.location,
                });

                let event_input = CreateTaskRunEventInput {
                    task_run_id: task_run_id.to_string(),
                    event_type: "image_recognition".to_string(),
                    event_subtype: Some(subtype.to_string()),
                    message,
                    data: Some(serde_json::to_string(&data).unwrap_or_default()),
                    workflow_name: workflow_name.map(|s| s.to_string()),
                    state_name: None,
                    action_id: None,
                    timestamp: event.timestamp.clone(),
                    duration_ms: None,
                };

                let event_id = db.create_task_run_event(&event_input)?;
                event_count += 1;

                // Create screenshot record if path exists
                if let Some(screenshot_path) = &event.screenshot_path {
                    let screenshot_input = CreateTaskRunScreenshotInput {
                        task_run_id: task_run_id.to_string(),
                        event_id: Some(event_id),
                        file_path: screenshot_path.clone(),
                        screenshot_type: "annotated".to_string(),
                        template_name: Some(event.template_name.clone()),
                        confidence: Some(event.confidence),
                        match_location: location_json,
                        width: None,
                        height: None,
                        file_size_bytes: None,
                    };

                    db.create_task_run_screenshot(&screenshot_input)?;
                    screenshot_count += 1;
                }
            }
            Err(e) => {
                debug!("Failed to parse image recognition event: {} - {}", e, line);
            }
        }
    }

    Ok((event_count, screenshot_count))
}

/// Migrate Playwright results from runner-playwright.jsonl
fn migrate_playwright_results(
    db: &CheckpointDb,
    task_run_id: &str,
    path: &Path,
) -> Result<usize, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
    let reader = BufReader::new(file);
    let mut count = 0;

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<PlaywrightLogs>(&line) {
            Ok(logs) => {
                // Create a result for each spec
                for spec in &logs.specs {
                    let input = CreateTaskRunPlaywrightResultInput {
                        task_run_id: task_run_id.to_string(),
                        test_name: spec.name.clone(),
                        spec_file: None,
                        status: if spec.passed {
                            "passed".to_string()
                        } else {
                            "failed".to_string()
                        },
                        duration_ms: Some(spec.duration_ms as i64),
                        stdout: None,
                        stderr: None,
                        console_output: logs.console_output.clone(),
                        page_snapshot: None,
                        error_message: spec.error.clone(),
                        failure_screenshot_path: logs.failure_screenshots.first().cloned(),
                        assertions_passed: if spec.passed { 1 } else { 0 },
                        assertions_failed: if spec.passed { 0 } else { 1 },
                    };

                    db.create_task_run_playwright_result(&input)?;
                    count += 1;
                }
            }
            Err(e) => {
                debug!("Failed to parse Playwright logs: {} - {}", e, line);
            }
        }
    }

    Ok(count)
}

/// Migrate AI output events from ai-output.jsonl
fn migrate_ai_output_events(
    db: &CheckpointDb,
    task_run_id: &str,
    path: &Path,
    workflow_name: Option<&str>,
) -> Result<usize, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<AiOutputEvent>(&line) {
            Ok(event) => {
                let data = if event.extra.is_empty() {
                    event.content.clone()
                } else {
                    Some(serde_json::to_string(&event.extra).unwrap_or_default())
                };

                let message = event
                    .content
                    .clone()
                    .unwrap_or_else(|| event.event_type.clone());

                events.push(CreateTaskRunEventInput {
                    task_run_id: task_run_id.to_string(),
                    event_type: "ai_output".to_string(),
                    event_subtype: Some(event.event_type),
                    message,
                    data,
                    workflow_name: workflow_name.map(|s| s.to_string()),
                    state_name: None,
                    action_id: None,
                    timestamp: event.timestamp,
                    duration_ms: None,
                });
            }
            Err(e) => {
                debug!("Failed to parse AI output event: {} - {}", e, line);
            }
        }
    }

    let count = events.len();
    db.batch_create_task_run_events(&events)?;
    Ok(count)
}

/// Migrate API request events from runner-api-requests.jsonl
fn migrate_api_request_events(
    db: &CheckpointDb,
    task_run_id: &str,
    path: &Path,
) -> Result<usize, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
    let reader = BufReader::new(file);
    let mut requests = Vec::new();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<ApiRequestEvent>(&line) {
            Ok(event) => {
                // Serialize headers to JSON string
                let request_headers = if event.headers.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&event.headers).unwrap_or_default())
                };

                let response_headers = if event.response_headers.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&event.response_headers).unwrap_or_default())
                };

                // Serialize extractions and assertions to JSON strings
                let extractions = if event.extractions.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&event.extractions).unwrap_or_default())
                };

                let assertions = if event.assertions.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&event.assertions).unwrap_or_default())
                };

                requests.push(CreateTaskRunApiRequestInput {
                    task_run_id: task_run_id.to_string(),
                    step_id: event.step_id,
                    step_name: Some(event.step_name),
                    method: event.method,
                    url: event.url,
                    resolved_url: event.resolved_url,
                    request_headers,
                    request_body: event.body,
                    status_code: event.status_code as i32,
                    status_text: Some(event.status_text),
                    response_headers,
                    response_time_ms: event.response_time_ms as i64,
                    response_body_type: event.response_body_type,
                    response_body: event.response_body,
                    response_size_bytes: Some(event.response_size_bytes as i64),
                    extractions,
                    assertions,
                    success: event.success,
                    error_message: event.error,
                    timestamp: event.timestamp,
                });
            }
            Err(e) => {
                debug!("Failed to parse API request event: {} - {}", e, line);
            }
        }
    }

    let count = requests.len();
    db.batch_create_task_run_api_requests(&requests)?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_general_event() {
        let json = r#"{"timestamp":"2024-01-15T10:00:00Z","level":"info","message":"Test message","extra_field":"value"}"#;
        let event: GeneralEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.message, "Test message");
        assert_eq!(event.level, Some("info".to_string()));
    }

    #[test]
    fn test_parse_action_event() {
        let json = r#"{"timestamp":"2024-01-15T10:00:00Z","event_type":"start","step_name":"click_button","success":true,"duration_ms":100}"#;
        let event: ActionEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "start");
        assert_eq!(event.step_name, Some("click_button".to_string()));
    }
}
