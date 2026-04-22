//! Timeout Diagnostics Module
//!
//! Provides diagnostic information collection when tests timeout or hang.
//! This helps AI agents debug issues with browser extensions and processes.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Extension endpoint for diagnostic checks (uses default port; runtime code should use dynamic port)
fn extension_endpoint() -> String {
    // NOTE: no AppState in scope here — this is a diagnostic helper with no
    // caller-supplied context. Uses the env-var lookup as the intentional
    // AppState-less fallback.
    let base = crate::mcp::types::get_self_base_url_from_env();
    format!("{}/extension/command", base)
}

/// Diagnostic information collected when a test times out
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticInfo {
    /// Whether the extension endpoint is reachable
    pub extension_reachable: Option<bool>,
    /// Error message if extension check failed
    pub extension_error: Option<String>,
    /// Whether Chrome/browser process is running
    pub browser_process_running: Option<bool>,
    /// Last successful operation (if tracked)
    pub last_successful_operation: Option<String>,
    /// Timestamp when diagnostics were collected
    pub collected_at: String,
}

impl Default for DiagnosticInfo {
    fn default() -> Self {
        Self {
            extension_reachable: None,
            extension_error: None,
            browser_process_running: None,
            last_successful_operation: None,
            collected_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

impl DiagnosticInfo {
    /// Format diagnostic info as a human-readable string for AI context
    pub fn format_for_ai(&self) -> String {
        let mut lines = vec!["=== Timeout Diagnostics ===".to_string()];

        // Extension status
        match (self.extension_reachable, &self.extension_error) {
            (Some(true), _) => {
                lines.push("Extension: REACHABLE (endpoint responded)".to_string());
            }
            (Some(false), Some(err)) => {
                lines.push(format!("Extension: NOT REACHABLE - {}", err));
            }
            (Some(false), None) => {
                lines.push("Extension: NOT REACHABLE (unknown error)".to_string());
            }
            (None, Some(err)) => {
                lines.push(format!("Extension: CHECK FAILED - {}", err));
            }
            (None, None) => {
                lines.push("Extension: NOT CHECKED".to_string());
            }
        }

        // Browser process status
        match self.browser_process_running {
            Some(true) => {
                lines.push("Browser Process: RUNNING (chrome.exe found)".to_string());
            }
            Some(false) => {
                lines.push("Browser Process: NOT RUNNING (chrome.exe not found)".to_string());
            }
            None => {
                lines.push("Browser Process: NOT CHECKED".to_string());
            }
        }

        // Last successful operation
        if let Some(ref op) = self.last_successful_operation {
            lines.push(format!("Last Successful Operation: {}", op));
        }

        lines.push(format!("Collected At: {}", self.collected_at));
        lines.push("=== End Diagnostics ===".to_string());

        lines.join("\n")
    }
}

/// Collect diagnostic information for timeout debugging
///
/// This function performs non-blocking checks to gather diagnostic info:
/// - Tries to ping the extension endpoint with a simple listTabs command
/// - Checks if chrome.exe process is running (on Windows)
///
/// All checks are designed to be fast and never cause additional failures.
pub fn collect_diagnostics() -> DiagnosticInfo {
    let mut info = DiagnosticInfo::default();

    // Check extension reachability
    let extension_result = check_extension_reachability();
    match extension_result {
        Ok(reachable) => {
            info.extension_reachable = Some(reachable);
            if !reachable {
                info.extension_error = Some("Extension did not respond".to_string());
            }
        }
        Err(e) => {
            info.extension_reachable = Some(false);
            info.extension_error = Some(e);
        }
    }

    // Check browser process (Windows only)
    info.browser_process_running = Some(check_browser_process_running());

    debug!("Collected timeout diagnostics: {:?}", info);

    info
}

/// Check if the extension endpoint is reachable by sending a simple command
fn check_extension_reachability() -> Result<bool, String> {
    // Use blocking client with a short timeout for quick check
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to create HTTP client: {}", e)),
    };

    // Try to send a simple listTabs command to check connectivity
    let request_body = serde_json::json!({
        "action": "listTabs",
        "params": {},
        "timeout_secs": 3
    });

    match client.post(extension_endpoint()).json(&request_body).send() {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                // Try to parse the response to see if it's a valid response
                match response.json::<serde_json::Value>() {
                    Ok(json) => {
                        // Check if response indicates success or at least a valid response
                        let success = json
                            .get("success")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let has_error = json.get("error").is_some();

                        // Even if success is false, getting a response means endpoint is reachable
                        // (the extension might just not have any tabs, or websocket isn't connected)
                        if success || has_error {
                            return Ok(true);
                        }

                        // If we got valid JSON back, endpoint is working
                        Ok(true)
                    }
                    Err(_) => {
                        // Got a response but couldn't parse it - endpoint exists but may be misconfigured
                        Ok(true)
                    }
                }
            } else {
                // Got an HTTP error response
                Err(format!(
                    "Extension endpoint returned HTTP {}: {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("Unknown")
                ))
            }
        }
        Err(e) => {
            // Categorize the error for better diagnostics
            if e.is_timeout() {
                Err("Connection timed out (extension may be hanging)".to_string())
            } else if e.is_connect() {
                Err("Connection refused (extension server may not be running)".to_string())
            } else if e.is_request() {
                Err(format!("Request error: {}", e))
            } else {
                Err(format!("Network error: {}", e))
            }
        }
    }
}

/// Check if a Chrome/browser process is running
#[cfg(target_os = "windows")]
fn check_browser_process_running() -> bool {
    // Use tasklist to check for chrome.exe - fast and reliable
    match crate::process_helpers::no_window("tasklist")
        .args(["/FI", "IMAGENAME eq chrome.exe", "/NH", "/FO", "CSV"])
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // If chrome.exe is running, output will contain "chrome.exe"
            // If not running, output will be "INFO: No tasks are running..."
            stdout.contains("chrome.exe")
        }
        Err(e) => {
            warn!("Failed to check browser process: {}", e);
            false
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn check_browser_process_running() -> bool {
    // On Unix, use pgrep
    match crate::process_helpers::no_window("pgrep")
        .args(["-x", "chrome"])
        .output()
    {
        Ok(output) => output.status.success(),
        Err(e) => {
            warn!("Failed to check browser process: {}", e);
            // Try alternative with ps
            match crate::process_helpers::no_window("ps")
                .args(["aux"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains("chrome"))
            {
                Ok(found) => found,
                Err(_) => false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostic_info_default() {
        let info = DiagnosticInfo::default();
        assert!(info.extension_reachable.is_none());
        assert!(info.browser_process_running.is_none());
        assert!(!info.collected_at.is_empty());
    }

    #[test]
    fn test_format_for_ai_all_fields() {
        let info = DiagnosticInfo {
            extension_reachable: Some(false),
            extension_error: Some("Connection refused".to_string()),
            browser_process_running: Some(true),
            last_successful_operation: Some("screenshot capture".to_string()),
            collected_at: "2024-01-15T10:30:00Z".to_string(),
        };

        let formatted = info.format_for_ai();
        assert!(formatted.contains("Extension: NOT REACHABLE"));
        assert!(formatted.contains("Connection refused"));
        assert!(formatted.contains("Browser Process: RUNNING"));
        assert!(formatted.contains("Last Successful Operation: screenshot capture"));
    }

    #[test]
    fn test_format_for_ai_minimal() {
        let info = DiagnosticInfo::default();
        let formatted = info.format_for_ai();
        assert!(formatted.contains("=== Timeout Diagnostics ==="));
        assert!(formatted.contains("=== End Diagnostics ==="));
    }

    #[test]
    fn test_collect_diagnostics_runs_without_error() {
        // This test just ensures collect_diagnostics doesn't panic
        // The actual values depend on the system state
        let info = collect_diagnostics();
        assert!(!info.collected_at.is_empty());
        // These should be set even if checks fail
        assert!(info.extension_reachable.is_some() || info.extension_error.is_some());
        assert!(info.browser_process_running.is_some());
    }
}
