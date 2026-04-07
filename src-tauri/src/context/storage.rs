//! File I/O and persistence for the user context library.

#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use tracing::{error, info};

use super::types::UserContextLibrary;

const CONTEXTS_FILE: &str = "contexts.json";

/// Get the contexts directory path in the app data directory.
///
/// Per-runner for secondary instances — the instance subdirectory is
/// inserted between `com.qontinui.runner` and `contexts`.
fn get_contexts_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir()
        .ok_or("Failed to get config directory")?
        .join("com.qontinui.runner");
    let app_data_dir = crate::instance::scope_path(&base).join("contexts");

    // Create directory if it doesn't exist
    if !app_data_dir.exists() {
        fs::create_dir_all(&app_data_dir)
            .map_err(|e| format!("Failed to create contexts directory: {}", e))?;
    }

    Ok(app_data_dir)
}

/// Get the contexts file path
fn get_contexts_path() -> Result<PathBuf, String> {
    get_contexts_dir().map(|dir| dir.join(CONTEXTS_FILE))
}

/// Load the user context library from disk
pub fn load_user_context_library() -> UserContextLibrary {
    match get_contexts_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str(&contents) {
                        Ok(library) => {
                            info!("Loaded user context library from {:?}", path);
                            library
                        }
                        Err(e) => {
                            error!("Failed to parse contexts file: {}", e);
                            UserContextLibrary::new()
                        }
                    },
                    Err(e) => {
                        error!("Failed to read contexts file: {}", e);
                        UserContextLibrary::new()
                    }
                }
            } else {
                info!("No contexts file found, using empty library");
                UserContextLibrary::new()
            }
        }
        Err(e) => {
            error!("Failed to get contexts path: {}", e);
            UserContextLibrary::new()
        }
    }
}

/// Save the user context library to disk
pub fn save_user_context_library(library: &UserContextLibrary) -> Result<(), String> {
    let path = get_contexts_path()?;

    let contents = serde_json::to_string_pretty(library)
        .map_err(|e| format!("Failed to serialize contexts: {}", e))?;

    fs::write(&path, contents).map_err(|e| format!("Failed to write contexts file: {}", e))?;

    info!("Saved user context library to {:?}", path);
    Ok(())
}
