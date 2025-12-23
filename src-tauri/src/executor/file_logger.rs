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

/// Counter for generating unique screenshot filenames
static SCREENSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Base directory for all dev logs
fn get_dev_logs_dir() -> PathBuf {
    PathBuf::from(r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs")
}

/// Directory for screenshots
fn get_screenshots_dir() -> PathBuf {
    get_dev_logs_dir().join("screenshots")
}

/// Directory for Playwright screenshots
fn get_playwright_screenshots_dir() -> PathBuf {
    get_dev_logs_dir().join("playwright-screenshots")
}

/// Ensure directories exist
fn ensure_dirs() -> std::io::Result<()> {
    fs::create_dir_all(get_dev_logs_dir())?;
    fs::create_dir_all(get_screenshots_dir())?;
    fs::create_dir_all(get_playwright_screenshots_dir())?;
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

        let entry = ImageRecognitionFileEntry {
            id: format!("img-{}", uuid::Uuid::new_v4()),
            timestamp: format_timestamp_now(),
            screenshot_timestamp: data
                .get("screenshot_timestamp")
                .and_then(|v| v.as_str())
                .map(String::from),
            node: data
                .get("node")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            template: data
                .get("template")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
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
    pub fn log_playwright_execution(
        script_id: &str,
        script_name: &str,
        target_url: Option<&str>,
        display_mode: Option<&str>,
        browser: Option<&str>,
        passed: bool,
        tests_passed: u32,
        tests_failed: u32,
        tests_skipped: u32,
        duration_ms: u64,
        error: Option<&str>,
        original_screenshots: &[String],
        console_output: &[String],
        specs: &[(String, String, u64, Option<String>)], // (title, status, duration_ms, error)
        page_snapshot: Option<&str>,
    ) {
        if let Err(e) = ensure_dirs() {
            error!("Failed to ensure log directories: {}", e);
            return;
        }

        // Copy screenshots to .dev-logs/playwright-screenshots
        let timestamp_ms = chrono::Utc::now().timestamp_millis();
        let mut copied_screenshots = Vec::new();

        for (idx, original_path) in original_screenshots.iter().enumerate() {
            let source = std::path::Path::new(original_path);
            if source.exists() {
                let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("png");
                let dest_name = format!("pw-{}-{}-{}.{}", timestamp_ms, script_id, idx, ext);
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

        let spec_entries: Vec<PlaywrightTestSpec> = specs
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
            script_id: script_id.to_string(),
            script_name: script_name.to_string(),
            passed,
            tests_passed,
            tests_failed,
            tests_skipped,
            duration_ms,
            error: error.map(String::from),
            target_url: target_url.map(String::from),
            display_mode: display_mode.map(String::from),
            browser: browser.map(String::from),
            screenshot_paths: copied_screenshots,
            console_output: console_output.to_vec(),
            specs: spec_entries,
            page_snapshot: page_snapshot.map(String::from),
        };

        Self::append_to_jsonl("runner-playwright.jsonl", &entry);
        debug!(
            "Logged Playwright execution: {} (passed: {})",
            script_name, passed
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

        debug!("Cleared runner log files");
    }
}

/// Copy a config file to .dev-logs for Claude Code access
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
    let extension = source
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("json");

    let dest_filename = format!("last-loaded-config.{}", extension);
    let dest_path = get_dev_logs_dir().join(&dest_filename);

    match fs::copy(source, &dest_path) {
        Ok(_) => {
            debug!("Copied config to: {}", dest_path.display());

            // Also write a metadata file with source path and timestamp
            let metadata = serde_json::json!({
                "source_path": source_path,
                "copied_at": chrono::Utc::now().to_rfc3339(),
                "filename": dest_filename
            });

            let meta_path = get_dev_logs_dir().join("last-loaded-config.meta.json");
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
