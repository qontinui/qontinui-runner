//! Screenshot listing commands
//!
//! This module provides commands for listing screenshots from the dev-logs directories:
//! - `.dev-logs/screenshots/` - Annotated screenshots from image recognition
//! - `.dev-logs/playwright-screenshots/` - Screenshots from Playwright test failures

use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use tracing::info;

/// Information about a single screenshot file
#[derive(Debug, Serialize)]
pub struct ScreenshotInfo {
    /// Filename (e.g., "screenshot_001.png")
    pub filename: String,
    /// Full absolute path to the file
    pub path: String,
    /// File size in bytes
    pub size_bytes: u64,
    /// Last modified timestamp (RFC3339 format)
    pub modified: Option<String>,
}

/// Result containing screenshots from both directories
#[derive(Debug, Serialize)]
pub struct ScreenshotsResult {
    /// Screenshots from .dev-logs/screenshots/ (annotated)
    pub annotated: Vec<ScreenshotInfo>,
    /// Screenshots from .dev-logs/playwright-screenshots/
    pub playwright: Vec<ScreenshotInfo>,
}

/// Response wrapper for the list_screenshots command
#[derive(Debug, Serialize)]
pub struct ScreenshotsResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<ScreenshotsResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ScreenshotsResponse {
    fn ok(data: ScreenshotsResult) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    fn err(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
        }
    }
}

/// Get the .dev-logs directory path.
fn get_dev_logs_dir() -> PathBuf {
    crate::paths::get_dev_logs_dir()
}

/// List all image files in a directory.
///
/// Returns screenshot info for all PNG, JPG, and JPEG files, sorted by
/// modification time (newest first).
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
                    let ext_lower = ext.to_string_lossy().to_lowercase();
                    if ext_lower == "png" || ext_lower == "jpg" || ext_lower == "jpeg" {
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
                                use chrono::{DateTime, Utc};
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

/// List all screenshots from both dev-logs screenshot directories.
///
/// Returns screenshots from:
/// - `.dev-logs/screenshots/` - Annotated screenshots from image recognition
/// - `.dev-logs/playwright-screenshots/` - Screenshots from Playwright test failures
///
/// Each screenshot includes: filename, path, size_bytes, and modified timestamp.
/// Results are sorted by modification time, newest first.
#[tauri::command]
pub async fn list_screenshots() -> ScreenshotsResponse {
    info!("Listing screenshots from dev-logs directories");

    let dev_logs_dir = get_dev_logs_dir();

    let annotated_dir = dev_logs_dir.join("screenshots");
    let playwright_dir = dev_logs_dir.join("playwright-screenshots");

    let annotated = list_screenshots_in_dir(&annotated_dir);
    let playwright = list_screenshots_in_dir(&playwright_dir);

    info!(
        "Found {} annotated screenshots and {} playwright screenshots",
        annotated.len(),
        playwright.len()
    );

    ScreenshotsResponse::ok(ScreenshotsResult {
        annotated,
        playwright,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screenshots_response_ok() {
        let result = ScreenshotsResult {
            annotated: vec![],
            playwright: vec![],
        };
        let response = ScreenshotsResponse::ok(result);
        assert!(response.success);
        assert!(response.data.is_some());
        assert!(response.error.is_none());
    }

    #[test]
    fn test_screenshots_response_err() {
        let response = ScreenshotsResponse::err("Test error".to_string());
        assert!(!response.success);
        assert!(response.data.is_none());
        assert_eq!(response.error, Some("Test error".to_string()));
    }
}
