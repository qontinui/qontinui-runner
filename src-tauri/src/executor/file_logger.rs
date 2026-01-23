//! File-based logging for runner events
//!
//! Writes runner events to .dev-logs/ for access by Claude Code.
//! Includes saving annotated screenshots from image recognition.

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, error, warn};

use crate::playwright::WorkflowStatus;

/// Counter for generating unique screenshot filenames
static SCREENSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Base directory for all dev logs
fn get_dev_logs_dir() -> PathBuf {
    crate::paths::get_dev_logs_dir()
}

/// Directory for screenshots
fn get_screenshots_dir() -> PathBuf {
    get_dev_logs_dir().join("screenshots")
}

/// Directory for Playwright screenshots
fn get_playwright_screenshots_dir() -> PathBuf {
    get_dev_logs_dir().join("playwright-screenshots")
}

/// Directory for API response images
fn get_api_images_dir() -> PathBuf {
    get_dev_logs_dir().join("api-images")
}

/// Ensure directories exist
fn ensure_dirs() -> std::io::Result<()> {
    fs::create_dir_all(get_dev_logs_dir())?;
    fs::create_dir_all(get_screenshots_dir())?;
    fs::create_dir_all(get_playwright_screenshots_dir())?;
    fs::create_dir_all(get_api_images_dir())?;
    Ok(())
}

/// General log entry (matches frontend LogEntry)
#[derive(Debug, Serialize, Deserialize)]
pub struct GeneralLogEntry {
    pub id: String,
    pub timestamp: String,
    pub level: String,
    pub message: String,
}

/// Image recognition log entry for file storage
/// Similar to frontend but with file paths instead of base64
#[derive(Debug, Serialize, Deserialize)]
pub struct ImageRecognitionFileEntry {
    pub id: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_timestamp: Option<String>,
    pub node: String,
    pub template: String,
    pub confidence: f64,
    pub found: bool,
    pub threshold: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent_off: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_match_location: Option<String>,
    /// Path to the annotated screenshot file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotated_screenshot_path: Option<String>,
    /// Path to the matched region screenshot
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_region_path: Option<String>,
    /// Path to the template image
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_index: Option<i32>,
    /// Debug data with top matches
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Location {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Tree/Action log entry
#[derive(Debug, Serialize, Deserialize)]
pub struct ActionLogEntry {
    pub id: String,
    pub timestamp: f64,
    pub sequence: u32,
    pub event_type: String,
    pub node: serde_json::Value,
    pub path: Vec<serde_json::Value>,
}

/// Playwright test execution log entry
#[derive(Debug, Serialize, Deserialize)]
pub struct PlaywrightLogEntry {
    pub id: String,
    pub timestamp: String,
    pub script_id: String,
    pub script_name: String,
    pub passed: bool,
    pub tests_passed: u32,
    pub tests_failed: u32,
    pub tests_skipped: u32,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    /// Paths to screenshots copied to .dev-logs
    pub screenshot_paths: Vec<String>,
    /// Console output lines from the test
    pub console_output: Vec<String>,
    /// Individual test spec results
    pub specs: Vec<PlaywrightTestSpec>,
    /// Page snapshot (YAML showing elements on page at failure)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_snapshot: Option<String>,
    /// Workflow verification status (if this is workflow automation)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_status: Option<WorkflowStatus>,
}

/// Individual test spec result for logging
#[derive(Debug, Serialize, Deserialize)]
pub struct PlaywrightTestSpec {
    pub title: String,
    pub status: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// API request log entry for file storage
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiRequestFileEntry {
    pub id: String,
    pub timestamp: String,
    pub step_id: String,
    pub step_name: String,

    // Request details
    pub method: String,
    pub url: String,
    pub resolved_url: String,
    pub headers: std::collections::HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,

    // Response details
    pub status_code: u16,
    pub status_text: String,
    pub response_headers: std::collections::HashMap<String, String>,
    pub response_time_ms: u64,

    // Response body handling
    pub response_body_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_file_path: Option<String>,
    pub response_size_bytes: usize,

    // Variable extractions performed
    pub extractions: Vec<ApiExtractionResult>,

    // Assertion results
    pub assertions: Vec<ApiAssertionResult>,

    // Overall result
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Extraction result for API request logging
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiExtractionResult {
    pub variable_name: String,
    pub json_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_value: Option<String>,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Assertion result for API request logging
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiAssertionResult {
    pub assertion_type: String,
    pub expected: String,
    pub actual: String,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Parameters for logging an API request execution
pub struct ApiRequestLogParams<'a> {
    pub step_id: &'a str,
    pub step_name: &'a str,
    pub method: &'a str,
    pub url: &'a str,
    pub resolved_url: &'a str,
    pub headers: &'a std::collections::HashMap<String, String>,
    pub body: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub status_code: u16,
    pub status_text: &'a str,
    pub response_headers: &'a std::collections::HashMap<String, String>,
    pub response_time_ms: u64,
    pub response_body_type: &'a str,
    pub response_body: Option<&'a str>,
    pub response_file_path: Option<&'a str>,
    pub response_size_bytes: usize,
    pub extractions: &'a [(String, String, Option<String>, bool, Option<String>)],
    pub assertions: &'a [(String, String, String, bool, Option<String>)],
    pub success: bool,
    pub error: Option<&'a str>,
}

/// UI Bridge event log entry
#[derive(Debug, Serialize, Deserialize)]
pub struct UIBridgeLogEntry {
    pub id: String,
    pub timestamp: String,
    pub sequence: u32,
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Parameters for logging a UI Bridge event
pub struct UIBridgeLogParams<'a> {
    pub event_type: &'a str,
    pub element_id: Option<&'a str>,
    pub state_id: Option<&'a str>,
    pub transition_id: Option<&'a str>,
    pub action: Option<&'a str>,
    pub params: Option<&'a serde_json::Value>,
    pub result: Option<&'a serde_json::Value>,
    pub duration_ms: Option<f64>,
    pub success: bool,
    pub error_message: Option<&'a str>,
    pub metadata: Option<&'a serde_json::Value>,
}

/// Parameters for logging a Playwright test execution
pub struct PlaywrightLogParams<'a> {
    pub script_id: &'a str,
    pub script_name: &'a str,
    pub target_url: Option<&'a str>,
    pub display_mode: Option<&'a str>,
    pub browser: Option<&'a str>,
    pub passed: bool,
    pub tests_passed: u32,
    pub tests_failed: u32,
    pub tests_skipped: u32,
    pub duration_ms: u64,
    pub error: Option<&'a str>,
    pub original_screenshots: &'a [String],
    pub console_output: &'a [String],
    /// (title, status, duration_ms, error)
    pub specs: &'a [(String, String, u64, Option<String>)],
    pub page_snapshot: Option<&'a str>,
    pub workflow_status: Option<WorkflowStatus>,
}

/// File logger for persisting runner events to disk
pub struct FileLogger;

impl FileLogger {
    /// Log a general event
    pub fn log_general_event(event: &str, data: &serde_json::Value, timestamp: f64) {
        if let Err(e) = ensure_dirs() {
            error!("Failed to ensure log directories: {}", e);
            return;
        }

        let entry = GeneralLogEntry {
            id: format!("gen-{}", uuid::Uuid::new_v4()),
            timestamp: format_timestamp(timestamp),
            level: "info".to_string(),
            message: format!("{}: {}", event, data),
        };

        Self::append_to_jsonl("runner-general.jsonl", &entry);
    }

    /// Log an error event
    pub fn log_error(message: &str, details: Option<&str>) {
        if let Err(e) = ensure_dirs() {
            error!("Failed to ensure log directories: {}", e);
            return;
        }

        let entry = GeneralLogEntry {
            id: format!("err-{}", uuid::Uuid::new_v4()),
            timestamp: format_timestamp_now(),
            level: "error".to_string(),
            message: if let Some(d) = details {
                format!("{}: {}", message, d)
            } else {
                message.to_string()
            },
        };

        Self::append_to_jsonl("runner-general.jsonl", &entry);
    }

    /// Log a tree/action event
    pub fn log_tree_event(
        event_type: &str,
        node: &serde_json::Value,
        path: &[serde_json::Value],
        timestamp: f64,
        sequence: u32,
    ) {
        if let Err(e) = ensure_dirs() {
            error!("Failed to ensure log directories: {}", e);
            return;
        }

        let entry = ActionLogEntry {
            id: format!("act-{}", uuid::Uuid::new_v4()),
            timestamp,
            sequence,
            event_type: event_type.to_string(),
            node: node.clone(),
            path: path.to_vec(),
        };

        Self::append_to_jsonl("runner-actions.jsonl", &entry);
    }

    /// Log an image recognition event, saving images to files
    pub fn log_image_recognition(data: &serde_json::Value) {
        if let Err(e) = ensure_dirs() {
            error!("Failed to ensure log directories: {}", e);
            return;
        }

        let counter = SCREENSHOT_COUNTER.fetch_add(1, Ordering::SeqCst);
        let timestamp_ms = chrono::Utc::now().timestamp_millis();
        let base_name = format!("img-{}-{}", timestamp_ms, counter);

        // Save annotated screenshot if present
        let annotated_path = data
            .get("visual_debug_image")
            .and_then(|v| v.as_str())
            .and_then(|base64_data| {
                Self::save_base64_image(base64_data, &format!("{}-annotated.png", base_name))
            });

        // Save matched region if present
        let matched_region_path = data
            .get("matched_region_image")
            .and_then(|v| v.as_str())
            .and_then(|base64_data| {
                Self::save_base64_image(base64_data, &format!("{}-matched-region.png", base_name))
            });

        // Parse location
        let location = data.get("location").and_then(|loc| {
            Some(Location {
                x: loc.get("x")?.as_i64()? as i32,
                y: loc.get("y")?.as_i64()? as i32,
                width: loc.get("width")?.as_i64()? as i32,
                height: loc.get("height")?.as_i64()? as i32,
            })
        });

        // Extract node from hierarchy.workflow_name, hierarchy.parent_id, or fallback
        let node = data
            .get("hierarchy")
            .and_then(|h| {
                h.get("workflow_name")
                    .and_then(|v| v.as_str())
                    .or_else(|| h.get("parent_id").and_then(|v| v.as_str()))
            })
            .or_else(|| data.get("node").and_then(|v| v.as_str()))
            .unwrap_or("unknown")
            .to_string();

        // Extract template from template_name, image_path, or fallback
        let template = data
            .get("template_name")
            .and_then(|v| v.as_str())
            .or_else(|| data.get("image_path").and_then(|v| v.as_str()))
            .or_else(|| data.get("template").and_then(|v| v.as_str()))
            .unwrap_or("unknown")
            .to_string();

        let entry = ImageRecognitionFileEntry {
            id: format!("img-{}", uuid::Uuid::new_v4()),
            timestamp: format_timestamp_now(),
            screenshot_timestamp: data
                .get("screenshot_timestamp")
                .and_then(|v| v.as_str())
                .map(String::from),
            node,
            template,
            confidence: data
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            found: data.get("found").and_then(|v| v.as_bool()).unwrap_or(false),
            threshold: data
                .get("threshold")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            location,
            gap: data.get("gap").and_then(|v| v.as_f64()),
            percent_off: data.get("percent_off").and_then(|v| v.as_f64()),
            best_match_location: data
                .get("best_match_location")
                .and_then(|v| v.as_str())
                .map(String::from),
            annotated_screenshot_path: annotated_path,
            matched_region_path,
            template_path: data
                .get("template_path")
                .and_then(|v| v.as_str())
                .map(String::from),
            monitor_index: data
                .get("monitor_index")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
            debug: data.get("debug").cloned(),
        };

        Self::append_to_jsonl("runner-image-recognition.jsonl", &entry);
    }

    /// Log a Playwright test execution, copying screenshots to .dev-logs
    pub fn log_playwright_execution(params: &PlaywrightLogParams<'_>) {
        if let Err(e) = ensure_dirs() {
            error!("Failed to ensure log directories: {}", e);
            return;
        }

        // Copy screenshots to .dev-logs/playwright-screenshots
        let timestamp_ms = chrono::Utc::now().timestamp_millis();
        let mut copied_screenshots = Vec::new();

        for (idx, original_path) in params.original_screenshots.iter().enumerate() {
            let source = std::path::Path::new(original_path);
            if source.exists() {
                let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("png");
                let dest_name = format!("pw-{}-{}-{}.{}", timestamp_ms, params.script_id, idx, ext);
                let dest_path = get_playwright_screenshots_dir().join(&dest_name);

                match fs::copy(source, &dest_path) {
                    Ok(_) => {
                        debug!("Copied Playwright screenshot: {}", dest_path.display());
                        copied_screenshots.push(dest_path.to_string_lossy().to_string());
                    }
                    Err(e) => {
                        warn!("Failed to copy screenshot {}: {}", original_path, e);
                    }
                }
            }
        }

        let spec_entries: Vec<PlaywrightTestSpec> = params
            .specs
            .iter()
            .map(|(title, status, dur, err)| PlaywrightTestSpec {
                title: title.clone(),
                status: status.clone(),
                duration_ms: *dur,
                error: err.clone(),
            })
            .collect();

        let entry = PlaywrightLogEntry {
            id: format!("pw-{}", uuid::Uuid::new_v4()),
            timestamp: format_timestamp_now(),
            script_id: params.script_id.to_string(),
            script_name: params.script_name.to_string(),
            passed: params.passed,
            tests_passed: params.tests_passed,
            tests_failed: params.tests_failed,
            tests_skipped: params.tests_skipped,
            duration_ms: params.duration_ms,
            error: params.error.map(String::from),
            target_url: params.target_url.map(String::from),
            display_mode: params.display_mode.map(String::from),
            browser: params.browser.map(String::from),
            screenshot_paths: copied_screenshots,
            console_output: params.console_output.to_vec(),
            specs: spec_entries,
            page_snapshot: params.page_snapshot.map(String::from),
            workflow_status: params.workflow_status.clone(),
        };

        Self::append_to_jsonl("runner-playwright.jsonl", &entry);
        debug!(
            "Logged Playwright execution: {} (passed: {})",
            params.script_name, params.passed
        );
    }

    /// Log a UI Bridge event
    pub fn log_ui_bridge_event(params: &UIBridgeLogParams<'_>, sequence: u32) {
        if let Err(e) = ensure_dirs() {
            error!("Failed to ensure log directories: {}", e);
            return;
        }

        let entry = UIBridgeLogEntry {
            id: format!("uib-{}", uuid::Uuid::new_v4()),
            timestamp: format_timestamp_now(),
            sequence,
            event_type: params.event_type.to_string(),
            element_id: params.element_id.map(String::from),
            state_id: params.state_id.map(String::from),
            transition_id: params.transition_id.map(String::from),
            action: params.action.map(String::from),
            params: params.params.cloned(),
            result: params.result.cloned(),
            duration_ms: params.duration_ms,
            success: params.success,
            error_message: params.error_message.map(String::from),
            metadata: params.metadata.cloned(),
        };

        Self::append_to_jsonl("runner-ui-bridge.jsonl", &entry);
        debug!(
            "Logged UI Bridge event: {} (element: {:?}, state: {:?})",
            params.event_type, params.element_id, params.state_id
        );
    }

    /// Log an API request execution
    pub fn log_api_request(params: &ApiRequestLogParams<'_>) {
        if let Err(e) = ensure_dirs() {
            error!("Failed to ensure log directories: {}", e);
            return;
        }

        let extraction_entries: Vec<ApiExtractionResult> = params
            .extractions
            .iter()
            .map(
                |(var_name, json_path, value, success, err)| ApiExtractionResult {
                    variable_name: var_name.clone(),
                    json_path: json_path.clone(),
                    extracted_value: value.clone(),
                    success: *success,
                    error: err.clone(),
                },
            )
            .collect();

        let assertion_entries: Vec<ApiAssertionResult> = params
            .assertions
            .iter()
            .map(
                |(assertion_type, expected, actual, passed, err)| ApiAssertionResult {
                    assertion_type: assertion_type.clone(),
                    expected: expected.clone(),
                    actual: actual.clone(),
                    passed: *passed,
                    error: err.clone(),
                },
            )
            .collect();

        let entry = ApiRequestFileEntry {
            id: format!("api-{}", uuid::Uuid::new_v4()),
            timestamp: format_timestamp_now(),
            step_id: params.step_id.to_string(),
            step_name: params.step_name.to_string(),
            method: params.method.to_string(),
            url: params.url.to_string(),
            resolved_url: params.resolved_url.to_string(),
            headers: params.headers.clone(),
            body: params.body.map(String::from),
            content_type: params.content_type.map(String::from),
            status_code: params.status_code,
            status_text: params.status_text.to_string(),
            response_headers: params.response_headers.clone(),
            response_time_ms: params.response_time_ms,
            response_body_type: params.response_body_type.to_string(),
            response_body: params.response_body.map(String::from),
            response_file_path: params.response_file_path.map(String::from),
            response_size_bytes: params.response_size_bytes,
            extractions: extraction_entries,
            assertions: assertion_entries,
            success: params.success,
            error: params.error.map(String::from),
        };

        Self::append_to_jsonl("runner-api-requests.jsonl", &entry);
        debug!(
            "Logged API request: {} {} -> {} ({} ms)",
            params.method, params.url, params.status_code, params.response_time_ms
        );
    }

    /// Save a base64-encoded image to a file
    fn save_base64_image(base64_data: &str, filename: &str) -> Option<String> {
        // Handle data URL prefix if present
        let data = if base64_data.starts_with("data:") {
            base64_data.split(',').nth(1).unwrap_or(base64_data)
        } else {
            base64_data
        };

        let bytes = match base64::engine::general_purpose::STANDARD.decode(data) {
            Ok(b) => b,
            Err(e) => {
                warn!("Failed to decode base64 image: {}", e);
                return None;
            }
        };

        let path = get_screenshots_dir().join(filename);
        match File::create(&path) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(&bytes) {
                    warn!("Failed to write image file: {}", e);
                    return None;
                }
                debug!("Saved screenshot: {}", path.display());
                Some(path.to_string_lossy().to_string())
            }
            Err(e) => {
                warn!("Failed to create image file: {}", e);
                None
            }
        }
    }

    /// Append a serializable entry to a JSONL file
    fn append_to_jsonl<T: Serialize>(filename: &str, entry: &T) {
        let path = get_dev_logs_dir().join(filename);

        let json_line = match serde_json::to_string(entry) {
            Ok(j) => j,
            Err(e) => {
                error!("Failed to serialize log entry: {}", e);
                return;
            }
        };

        let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to open log file {}: {}", path.display(), e);
                return;
            }
        };

        if let Err(e) = writeln!(file, "{}", json_line) {
            error!("Failed to write to log file: {}", e);
        }
    }

    /// Clear all runner log files (called on startup)
    pub fn clear_logs() {
        if let Err(e) = ensure_dirs() {
            error!("Failed to ensure log directories: {}", e);
            return;
        }

        let files = [
            "runner-general.jsonl",
            "runner-image-recognition.jsonl",
            "runner-actions.jsonl",
            "runner-playwright.jsonl",
            "runner-api-requests.jsonl",
            "runner-ui-bridge.jsonl",
        ];

        for filename in files {
            let path = get_dev_logs_dir().join(filename);
            if path.exists() {
                if let Err(e) = fs::remove_file(&path) {
                    warn!("Failed to clear log file {}: {}", filename, e);
                }
            }
        }

        // Clear screenshots directory
        let screenshots_dir = get_screenshots_dir();
        if screenshots_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&screenshots_dir) {
                warn!("Failed to clear screenshots directory: {}", e);
            }
            if let Err(e) = fs::create_dir_all(&screenshots_dir) {
                warn!("Failed to recreate screenshots directory: {}", e);
            }
        }

        // Clear playwright screenshots directory
        let pw_screenshots_dir = get_playwright_screenshots_dir();
        if pw_screenshots_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&pw_screenshots_dir) {
                warn!("Failed to clear playwright screenshots directory: {}", e);
            }
            if let Err(e) = fs::create_dir_all(&pw_screenshots_dir) {
                warn!("Failed to recreate playwright screenshots directory: {}", e);
            }
        }

        // Clear API images directory
        let api_images_dir = get_api_images_dir();
        if api_images_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&api_images_dir) {
                warn!("Failed to clear api-images directory: {}", e);
            }
            if let Err(e) = fs::create_dir_all(&api_images_dir) {
                warn!("Failed to recreate api-images directory: {}", e);
            }
        }

        debug!("Cleared runner log files");
    }
}

/// Copy a GUI automation config file to .dev-logs for Claude Code access.
///
/// NOTE: This is for model-based GUI automation configs (images, states, etc.),
/// NOT unified workflow configs. The file is saved as "last-loaded-gui-config.*"
/// to avoid confusion with unified workflow configurations.
pub fn copy_config_file(source_path: &str) {
    if let Err(e) = ensure_dirs() {
        error!("Failed to ensure log directories: {}", e);
        return;
    }

    let source = std::path::Path::new(source_path);
    if !source.exists() {
        warn!("Config file not found: {}", source_path);
        return;
    }

    // Determine the destination filename based on extension
    // Using "gui-config" to distinguish from unified workflow configs
    let extension = source
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("json");

    let dest_filename = format!("last-loaded-gui-config.{}", extension);
    let dest_path = get_dev_logs_dir().join(&dest_filename);

    match fs::copy(source, &dest_path) {
        Ok(_) => {
            debug!("Copied GUI config to: {}", dest_path.display());

            // Also write a metadata file with source path and timestamp
            let metadata = serde_json::json!({
                "source_path": source_path,
                "copied_at": chrono::Utc::now().to_rfc3339(),
                "filename": dest_filename,
                "config_type": "gui_automation",
                "description": "Model-based GUI automation config (images, states, transitions). This is NOT a unified workflow."
            });

            let meta_path = get_dev_logs_dir().join("last-loaded-gui-config.meta.json");
            if let Ok(json) = serde_json::to_string_pretty(&metadata) {
                if let Err(e) = fs::write(&meta_path, json) {
                    warn!("Failed to write config metadata: {}", e);
                }
            }
        }
        Err(e) => {
            error!("Failed to copy config file: {}", e);
        }
    }
}

/// Save a unified workflow config to .dev-logs for Claude Code access.
///
/// This is for unified workflow configurations (setup_steps, verification_steps,
/// agentic_steps, completion_steps). Saved as "last-unified-workflow.json".
pub fn save_unified_workflow_config(workflow: &serde_json::Value, workflow_name: &str) {
    if let Err(e) = ensure_dirs() {
        error!("Failed to ensure log directories: {}", e);
        return;
    }

    let dest_path = get_dev_logs_dir().join("last-unified-workflow.json");

    match serde_json::to_string_pretty(workflow) {
        Ok(json) => {
            if let Err(e) = fs::write(&dest_path, &json) {
                error!("Failed to write unified workflow config: {}", e);
                return;
            }
            debug!("Saved unified workflow config to: {}", dest_path.display());

            // Also write a metadata file
            let metadata = serde_json::json!({
                "workflow_name": workflow_name,
                "saved_at": chrono::Utc::now().to_rfc3339(),
                "filename": "last-unified-workflow.json",
                "config_type": "unified_workflow",
                "description": "Unified workflow with phases: setup_steps, verification_steps, agentic_steps, completion_steps"
            });

            let meta_path = get_dev_logs_dir().join("last-unified-workflow.meta.json");
            if let Ok(meta_json) = serde_json::to_string_pretty(&metadata) {
                if let Err(e) = fs::write(&meta_path, meta_json) {
                    warn!("Failed to write unified workflow metadata: {}", e);
                }
            }
        }
        Err(e) => {
            error!("Failed to serialize unified workflow config: {}", e);
        }
    }
}

/// Format a Unix timestamp (seconds) to ISO 8601 string
fn format_timestamp(timestamp: f64) -> String {
    use chrono::{TimeZone, Utc};
    let secs = timestamp as i64;
    let nanos = ((timestamp - secs as f64) * 1_000_000_000.0) as u32;
    Utc.timestamp_opt(secs, nanos)
        .single()
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| format!("{}", timestamp))
}

/// Format current time as ISO 8601 string
fn format_timestamp_now() -> String {
    chrono::Utc::now().to_rfc3339()
}
