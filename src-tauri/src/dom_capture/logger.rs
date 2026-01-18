//! DOM Capture logging and storage.
//!
//! Stores DOM captures as:
//! - HTML files in `.dev-logs/dom-captures/`
//! - Metadata in `.dev-logs/runner-dom-captures.jsonl`

use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tracing::{debug, error, info, warn};

use super::types::{
    DomCapture, DomCaptureFileEntry, DomCaptureSource, DomCaptureTrigger, DomCaptureType,
};

/// Directory for DOM capture HTML files
fn get_dom_captures_dir() -> PathBuf {
    crate::paths::get_dev_logs_dir().join("dom-captures")
}

/// Path to the DOM captures JSONL log file
fn get_dom_captures_log_path() -> PathBuf {
    crate::paths::get_dev_logs_dir().join("runner-dom-captures.jsonl")
}

/// Directory for screenshots
fn get_screenshots_dir() -> PathBuf {
    crate::paths::get_screenshots_dir()
}

/// Directory for Playwright screenshots
fn get_playwright_screenshots_dir() -> PathBuf {
    crate::paths::get_dev_logs_dir().join("playwright-screenshots")
}

/// Find the most recent screenshot taken within the given duration
/// Returns the path to the screenshot if found
fn find_recent_screenshot(max_age: Duration) -> Option<String> {
    let now = SystemTime::now();

    // Check both screenshot directories
    let dirs = [get_screenshots_dir(), get_playwright_screenshots_dir()];

    let mut newest_screenshot: Option<(PathBuf, SystemTime)> = None;

    for dir in dirs {
        if !dir.exists() {
            continue;
        }

        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();

                // Only consider PNG files
                if path.extension().map_or(false, |ext| ext == "png") {
                    if let Ok(metadata) = path.metadata() {
                        if let Ok(modified) = metadata.modified() {
                            // Check if within max_age
                            if let Ok(age) = now.duration_since(modified) {
                                if age <= max_age {
                                    // Keep track of the newest one
                                    if newest_screenshot.is_none()
                                        || modified > newest_screenshot.as_ref().unwrap().1
                                    {
                                        newest_screenshot = Some((path, modified));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    newest_screenshot.map(|(path, _)| path.to_string_lossy().to_string())
}

/// Ensure required directories exist
fn ensure_dirs() -> std::io::Result<()> {
    fs::create_dir_all(crate::paths::get_dev_logs_dir())?;
    fs::create_dir_all(get_dom_captures_dir())?;
    Ok(())
}

/// Format current timestamp as ISO 8601
fn format_timestamp_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Append a serializable entry to the JSONL log file
fn append_to_jsonl<T: Serialize>(entry: &T) {
    let path = get_dom_captures_log_path();

    let json_line = match serde_json::to_string(entry) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to serialize DOM capture entry: {}", e);
            return;
        }
    };

    let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            error!("Failed to open DOM captures log file: {}", e);
            return;
        }
    };

    if let Err(e) = writeln!(file, "{}", json_line) {
        error!("Failed to write to DOM captures log file: {}", e);
    }
}

/// DOM capture logger
pub struct DomCaptureLogger;

impl DomCaptureLogger {
    /// Log a DOM capture, saving HTML to file and metadata to JSONL
    ///
    /// Returns the created DomCapture on success
    pub fn log_capture(
        url: &str,
        page_title: &str,
        html_content: &str,
        selector: Option<&str>,
        source: DomCaptureSource,
        trigger: DomCaptureTrigger,
        task_run_id: Option<&str>,
        linked_screenshot: Option<&str>,
    ) -> Result<DomCapture, String> {
        if let Err(e) = ensure_dirs() {
            return Err(format!("Failed to ensure log directories: {}", e));
        }

        let id = format!("dom-{}", uuid::Uuid::new_v4());
        let timestamp = format_timestamp_now();
        let html_size_bytes = html_content.len();

        // Determine capture type
        let capture_type = if selector.is_some() {
            DomCaptureType::Selector
        } else {
            DomCaptureType::Full
        };

        // Save HTML to file
        let html_filename = format!("{}.html", id);
        let html_file_path = get_dom_captures_dir().join(&html_filename);

        let mut html_file = File::create(&html_file_path)
            .map_err(|e| format!("Failed to create HTML file: {}", e))?;

        html_file
            .write_all(html_content.as_bytes())
            .map_err(|e| format!("Failed to write HTML file: {}", e))?;

        debug!(
            "Saved DOM capture: {} ({} bytes)",
            html_file_path.display(),
            html_size_bytes
        );

        // Create the capture record
        let capture = DomCapture {
            id: id.clone(),
            timestamp,
            url: url.to_string(),
            page_title: page_title.to_string(),
            capture_type,
            selector: selector.map(String::from),
            html_size_bytes,
            html_file_path: html_file_path.to_string_lossy().to_string(),
            linked_screenshot: linked_screenshot.map(String::from),
            source,
            task_run_id: task_run_id.map(String::from),
            trigger,
        };

        // Log to JSONL
        let file_entry: DomCaptureFileEntry = capture.clone().into();
        append_to_jsonl(&file_entry);

        debug!(
            "Logged DOM capture: {} from {} ({})",
            id,
            capture.source,
            capture.capture_type
        );

        Ok(capture)
    }

    /// List all DOM captures from the JSONL log
    pub fn list_captures() -> Vec<DomCapture> {
        let path = get_dom_captures_log_path();

        if !path.exists() {
            return Vec::new();
        }

        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                warn!("Failed to open DOM captures log: {}", e);
                return Vec::new();
            }
        };

        let reader = BufReader::new(file);
        let mut captures = Vec::new();

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };

            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<DomCaptureFileEntry>(&line) {
                Ok(entry) => match DomCapture::try_from(entry) {
                    Ok(capture) => captures.push(capture),
                    Err(e) => warn!("Failed to convert DOM capture entry: {}", e),
                },
                Err(e) => warn!("Failed to parse DOM capture entry: {}", e),
            }
        }

        // Sort by timestamp descending (most recent first)
        captures.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        captures
    }

    /// Get a specific DOM capture by ID
    pub fn get_capture(id: &str) -> Option<DomCapture> {
        Self::list_captures().into_iter().find(|c| c.id == id)
    }

    /// Get the HTML content of a DOM capture
    pub fn get_capture_html(id: &str) -> Result<String, String> {
        let capture = Self::get_capture(id).ok_or_else(|| format!("Capture not found: {}", id))?;

        let path = PathBuf::from(&capture.html_file_path);
        if !path.exists() {
            return Err(format!("HTML file not found: {}", capture.html_file_path));
        }

        fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read HTML file: {}", e))
    }

    /// Clear all DOM capture files and logs (called on startup)
    pub fn clear_captures() {
        if let Err(e) = ensure_dirs() {
            error!("Failed to ensure log directories: {}", e);
            return;
        }

        // Clear JSONL log
        let log_path = get_dom_captures_log_path();
        if log_path.exists() {
            if let Err(e) = fs::remove_file(&log_path) {
                warn!("Failed to clear DOM captures log: {}", e);
            }
        }

        // Clear HTML files directory
        let captures_dir = get_dom_captures_dir();
        if captures_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&captures_dir) {
                warn!("Failed to clear DOM captures directory: {}", e);
            }
            if let Err(e) = fs::create_dir_all(&captures_dir) {
                warn!("Failed to recreate DOM captures directory: {}", e);
            }
        }

        debug!("Cleared DOM capture files");
    }

    /// Filter captures by task run ID
    pub fn list_captures_for_task(task_run_id: &str) -> Vec<DomCapture> {
        Self::list_captures()
            .into_iter()
            .filter(|c| c.task_run_id.as_deref() == Some(task_run_id))
            .collect()
    }

    /// Log a DOM capture with automatic screenshot linking
    ///
    /// Automatically finds the most recent screenshot (within 30 seconds) and links it.
    /// If linked_screenshot is already provided, uses that instead.
    pub fn log_capture_with_auto_link(
        url: &str,
        page_title: &str,
        html_content: &str,
        selector: Option<&str>,
        source: DomCaptureSource,
        trigger: DomCaptureTrigger,
        task_run_id: Option<&str>,
        linked_screenshot: Option<&str>,
    ) -> Result<DomCapture, String> {
        // If no screenshot provided, try to find a recent one
        let screenshot_to_link = if linked_screenshot.is_some() {
            linked_screenshot.map(String::from)
        } else {
            // Look for screenshots taken in the last 30 seconds
            let recent = find_recent_screenshot(Duration::from_secs(30));
            if let Some(ref path) = recent {
                info!("Auto-linked DOM capture to recent screenshot: {}", path);
            }
            recent
        };

        Self::log_capture(
            url,
            page_title,
            html_content,
            selector,
            source,
            trigger,
            task_run_id,
            screenshot_to_link.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_timestamp() {
        let ts = format_timestamp_now();
        // Should be ISO 8601 format
        assert!(ts.contains("T"));
        assert!(ts.contains("+") || ts.contains("Z"));
    }

    #[test]
    fn test_capture_type_display() {
        assert_eq!(DomCaptureType::Full.to_string(), "full");
        assert_eq!(DomCaptureType::Selector.to_string(), "selector");
    }

    #[test]
    fn test_source_display() {
        assert_eq!(DomCaptureSource::Playwright.to_string(), "playwright");
        assert_eq!(DomCaptureSource::Extension.to_string(), "extension");
    }

    #[test]
    fn test_trigger_display() {
        assert_eq!(DomCaptureTrigger::OnDemand.to_string(), "on_demand");
        assert_eq!(DomCaptureTrigger::Auto.to_string(), "auto");
        assert_eq!(DomCaptureTrigger::Error.to_string(), "error");
    }
}
