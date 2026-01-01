//! Playwright type definitions
//!
//! Contains enums and constants used across the playwright module.

use serde::{Deserialize, Serialize};

/// File name for storing scripts
pub const SCRIPTS_FILE: &str = "playwright-scripts.json";

/// Sync status for cloud backup
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    #[default]
    LocalOnly,
    Synced,
    LocalModified,
    CloudModified,
    Conflict,
}

/// Display mode for Playwright test execution
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    /// Run without browser UI (fastest, for CI)
    #[default]
    Headless,
    /// Launch a new browser window with UI visible
    Headed,
    /// Connect to an existing Chrome browser via CDP (Chrome DevTools Protocol)
    ConnectExisting,
}

/// Default timeout in seconds for script execution
pub fn default_timeout_seconds() -> u32 {
    60
}

/// Default browser for Playwright tests
pub fn default_browser() -> String {
    "chromium".to_string()
}

/// Default version for the library format
pub fn default_version() -> String {
    "1.0.0".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_status_default() {
        let status = SyncStatus::default();
        assert_eq!(status, SyncStatus::LocalOnly);
    }

    #[test]
    fn test_display_mode_default() {
        let mode = DisplayMode::default();
        assert_eq!(mode, DisplayMode::Headless);
    }

    #[test]
    fn test_default_timeout() {
        assert_eq!(default_timeout_seconds(), 60);
    }

    #[test]
    fn test_default_browser() {
        assert_eq!(default_browser(), "chromium");
    }
}
