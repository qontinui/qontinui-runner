#![allow(dead_code)]

use crate::settings::{self, AccountSelectionMode};
use std::sync::Mutex;
use tracing::info;

/// Cached resolved config dir for least-usage account selection.
/// Set by `set_resolved_config_dir` at startup or when usage is checked.
static RESOLVED_CONFIG_DIR: Mutex<Option<String>> = Mutex::new(None);

/// Set the resolved config directory (called after usage check).
pub fn set_resolved_config_dir(dir: Option<String>) {
    if let Ok(mut cached) = RESOLVED_CONFIG_DIR.lock() {
        info!("Setting resolved config dir: {:?}", dir);
        *cached = dir;
    }
}

/// Get the effective config directory, considering account selection mode.
pub fn get_effective_config_dir(cli_settings: &settings::ClaudeCliSettings) -> Option<String> {
    match cli_settings.account_selection_mode {
        AccountSelectionMode::LeastUsage => {
            if let Ok(cached) = RESOLVED_CONFIG_DIR.lock() {
                if cached.is_some() {
                    return cached.clone();
                }
            }
            // Fallback to manual config_dir if no resolved dir
            cli_settings.config_dir.clone()
        }
        AccountSelectionMode::Manual => cli_settings.config_dir.clone(),
    }
}
