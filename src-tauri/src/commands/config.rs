//! Configuration management commands
//!
//! This module handles all configuration-related operations including:
//! - Loading and validating YAML configuration files
//! - Managing the current configuration state
//! - Persisting configuration paths and workflow IDs
//! - Auto-load settings

use crate::config::{ConfigLoader, QontinuiConfig};
use crate::error::AppError;
use crate::executor::file_logger;
use crate::settings;
use std::sync::Arc;
use tauri::State;
use tracing::{error, info, warn};

use super::{AppState, CommandResponse};

/// Load a configuration file from the specified path.
///
/// This command:
/// 1. Loads and validates the YAML configuration
/// 2. Stores it in the app state
/// 3. Persists the path for auto-load functionality
/// 4. Sends the configuration to the Python executor if running
/// 5. Applies current debug settings to the executor
///
/// # Arguments
/// * `path` - Absolute path to the YAML configuration file
/// * `state` - Application state containing config and Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with configuration summary
/// * `Err(String)` - Error message if loading fails
#[tauri::command]
pub async fn load_configuration(
    path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Loading configuration from: {}", path);

    // Load the configuration file
    let config = ConfigLoader::load_from_file(&path)
        .map_err(|e| {
            error!("Failed to load configuration from {}: {}", path, e);
            AppError::ConfigError(format!("Failed to load configuration: {}", e))
        })
        .map_err(|e| e.to_string())?;

    let summary = config.summary();

    // Create data object with configuration info (including metadata for projectId)
    let config_data = serde_json::json!({
        "metadata": config.metadata.clone(),
        "workflows": config.workflows.clone(),
        "states": config.states.clone(),
        "transitions": config.transitions.clone(),
        "images": config.images.clone(),
        "categories": config.categories.clone()
    });

    // Extract config_id and project_id for run recording
    // Use config name as config_id (or path as fallback)
    let config_id = if !config.metadata.name.is_empty() {
        config.metadata.name.clone()
    } else {
        path.clone()
    };
    let project_id = config.metadata.project_id.clone();

    // Set config context on run recording handler
    {
        let handler = state.run_recording_handler.clone();
        let config_id_clone = config_id.clone();
        let project_id_clone = project_id.clone();
        tauri::async_runtime::spawn(async move {
            handler.set_config(config_id_clone, project_id_clone).await;
        });
    }

    // Store the configuration
    *crate::safe_lock::safe_lock_or_recover(&state.current_config, "current_config") = Some(config);
    info!(
        "Configuration loaded successfully: {} (config_id: {}, project_id: {:?})",
        summary, config_id, project_id
    );

    // Save the path as the last loaded config
    if let Err(e) = settings::save_last_config_path(&path) {
        warn!("Failed to save last config path: {}", e);
    }

    // Copy config to .dev-logs for Claude Code access
    file_logger::copy_config_file(&path);

    // If Python bridge is running, send the configuration and debug settings
    // NOTE: We use spawn_blocking because PythonBridge methods use block_on internally
    let manager_guard = state.bridge_manager.lock().await;
    if let Some(ref manager) = *manager_guard {
        // Use async check to avoid nested runtime panic
        if manager.is_default_bridge_running_async().await {
            let manager_clone = manager.clone();
            let path_clone = path.clone();
            let debug_settings = settings::get_debug_settings();

            tokio::task::spawn_blocking(move || {
                // First send debug settings to ensure they're applied before config execution
                let _ = manager_clone.with_default_bridge(|bridge| {
                    if let Err(e) = bridge.set_debug_settings(
                        debug_settings.enable_image_debug,
                        debug_settings.top_matches_count,
                    ) {
                        warn!("Failed to send debug settings before config load: {}", e);
                    } else {
                        info!(
                            "Debug settings sent before config load: enable={}, top_matches={}",
                            debug_settings.enable_image_debug, debug_settings.top_matches_count
                        );
                    }
                }); // Ignore errors for debug settings

                manager_clone.with_default_bridge(|bridge| bridge.load_configuration(&path_clone))
            })
            .await
            .map_err(|e| format!("Task join error: {}", e))?
            .map_err(|e| format!("Failed to access bridge: {}", e))?
            .map_err(|e| {
                error!("Failed to send configuration to Python: {}", e);
                format!("Failed to send configuration to Python: {}", e)
            })?;
            info!("Configuration sent to Python executor");
        }
    }

    Ok(CommandResponse {
        success: true,
        message: Some(summary),
        data: Some(config_data),
    })
}

/// Get the currently loaded configuration.
///
/// # Arguments
/// * `state` - Application state containing the current configuration
///
/// # Returns
/// * `Ok(QontinuiConfig)` - The current configuration
/// * `Err(String)` - Error if no configuration is loaded
#[tauri::command]
pub fn get_current_configuration(state: State<Arc<AppState>>) -> Result<QontinuiConfig, String> {
    state
        .current_config
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "No configuration loaded".to_string())
}

/// Get the path of the last loaded configuration file.
///
/// Returns the path if it exists and the file is still present on disk.
/// Also returns the last workflow_id and monitor_indices if saved.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with path, workflow_id, and monitor_indices, or message if not found
/// * `Err(String)` - Error message if settings cannot be read
#[tauri::command]
pub fn get_last_config_path() -> Result<CommandResponse, String> {
    info!("Getting last config path");

    if let Some(path) = settings::get_last_config_path() {
        // Check if the file still exists
        if std::path::Path::new(&path).exists() {
            let workflow_id = settings::get_last_workflow_id();
            let monitor_indices = settings::get_last_monitor_indices();
            // Also provide legacy single monitor_index for backward compatibility
            let monitor_index = monitor_indices.as_ref().and_then(|v| v.first().copied());
            Ok(CommandResponse {
                success: true,
                message: Some(format!("Last config found: {}", path)),
                data: Some(serde_json::json!({
                    "path": path,
                    "workflow_id": workflow_id,
                    "monitor_indices": monitor_indices,
                    "monitor_index": monitor_index
                })),
            })
        } else {
            info!(
                "Last config path exists in settings but file not found: {}",
                path
            );
            Ok(CommandResponse {
                success: false,
                message: Some("Last config file not found".to_string()),
                data: None,
            })
        }
    } else {
        Ok(CommandResponse {
            success: false,
            message: Some("No last config path saved".to_string()),
            data: None,
        })
    }
}

/// Save the last used workflow ID to persistent settings.
///
/// # Arguments
/// * `workflow_id` - The workflow ID to save
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if settings cannot be saved
#[tauri::command]
pub fn save_last_workflow_id(workflow_id: String) -> Result<CommandResponse, String> {
    info!("Saving last workflow ID: {}", workflow_id);

    settings::save_last_workflow_id(&workflow_id)
        .map_err(|e| format!("Failed to save last workflow ID: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Last workflow ID saved".to_string()),
        data: None,
    })
}

/// Save the last used monitor index to persistent settings.
///
/// # Arguments
/// * `monitor_index` - The monitor index to save
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if settings cannot be saved
#[tauri::command]
pub fn save_last_monitor_index(monitor_index: i32) -> Result<CommandResponse, String> {
    info!("Saving last monitor index: {}", monitor_index);

    settings::save_last_monitor_index(monitor_index)
        .map_err(|e| format!("Failed to save last monitor index: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Last monitor index saved".to_string()),
        data: None,
    })
}

/// Save the last used monitor indices to persistent settings (multi-monitor support).
///
/// # Arguments
/// * `monitor_indices` - Array of monitor indices to save
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if settings cannot be saved
#[tauri::command]
pub fn save_last_monitor_indices(monitor_indices: Vec<i32>) -> Result<CommandResponse, String> {
    info!("Saving last monitor indices: {:?}", monitor_indices);

    settings::save_last_monitor_indices(monitor_indices)
        .map_err(|e| format!("Failed to save last monitor indices: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Last monitor indices saved".to_string()),
        data: None,
    })
}

/// Get the auto-load last config setting.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with enabled status
/// * `Err(String)` - Error message if settings cannot be read
#[tauri::command]
pub fn get_auto_load_last_config() -> Result<CommandResponse, String> {
    let enabled = settings::get_auto_load_last_config();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "enabled": enabled
        })),
    })
}

/// Save the auto-load last config setting.
///
/// # Arguments
/// * `enabled` - Whether to auto-load the last config on startup
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if settings cannot be saved
#[tauri::command]
pub fn save_auto_load_last_config(enabled: bool) -> Result<CommandResponse, String> {
    info!("Saving auto-load last config setting: {}", enabled);

    settings::save_auto_load_last_config(enabled)
        .map_err(|e| format!("Failed to save auto-load setting: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Auto-load last config {}",
            if enabled { "enabled" } else { "disabled" }
        )),
        data: None,
    })
}

/// Get the include summary step by default setting.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with enabled status
/// * `Err(String)` - Error message if settings cannot be read
#[tauri::command]
pub fn get_include_summary_step_by_default() -> Result<CommandResponse, String> {
    let enabled = settings::get_include_summary_step_by_default();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "enabled": enabled
        })),
    })
}

/// Save the include summary step by default setting.
///
/// # Arguments
/// * `enabled` - Whether to include AI Summary step in new workflows by default
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if settings cannot be saved
#[tauri::command]
pub fn save_include_summary_step_by_default(enabled: bool) -> Result<CommandResponse, String> {
    info!(
        "Saving include summary step by default setting: {}",
        enabled
    );

    settings::save_include_summary_step_by_default(enabled)
        .map_err(|e| format!("Failed to save include summary step setting: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Include summary step by default {}",
            if enabled { "enabled" } else { "disabled" }
        )),
        data: None,
    })
}

/// Get workspace paths for portable operation.
///
/// Returns paths that are dynamically determined based on the runner's location,
/// allowing the app to work on any user's machine without hardcoded paths.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with paths:
///   - `workspace_root`: Parent directory of the runner (the main project folder)
///   - `dev_logs_path`: Path to .dev-logs directory
///   - `scripts_path`: Path to qontinui-claude-config/scripts
///   - `spawn_script`: Full path to spawn-independent-claude.py
#[tauri::command]
pub fn get_workspace_paths() -> Result<CommandResponse, String> {
    // Get the current executable's directory
    let exe_path =
        std::env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))?;

    // The executable is in qontinui-runner/src-tauri/target/debug or release
    // We need to go up to find the qontinui-runner directory, then up again for workspace
    let mut current = exe_path.as_path();

    // Navigate up to find qontinui-runner directory (contains src-tauri)
    let runner_dir = loop {
        if let Some(parent) = current.parent() {
            if parent.join("src-tauri").exists()
                || parent.file_name().is_some_and(|n| n == "qontinui-runner")
            {
                break parent.to_path_buf();
            }
            current = parent;
        } else {
            // Fallback: try to find from current working directory
            let cwd = std::env::current_dir()
                .map_err(|e| format!("Failed to get current directory: {}", e))?;
            break cwd;
        }
    };

    // Workspace root is parent of qontinui-runner
    let workspace_root = runner_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| runner_dir.clone());

    let dev_logs_path = workspace_root.join(".dev-logs");
    let scripts_path = workspace_root
        .join("qontinui-claude-config")
        .join("scripts");
    let spawn_script = scripts_path.join("spawn-independent-claude.py");

    // Convert paths to strings with forward slashes for cross-platform compatibility in prompts
    // But use double backslashes for PowerShell commands on Windows
    let workspace_str = workspace_root.to_string_lossy().to_string();
    let dev_logs_str = dev_logs_path.to_string_lossy().to_string();
    let scripts_str = scripts_path.to_string_lossy().to_string();
    let spawn_script_str = spawn_script.to_string_lossy().to_string();

    // Also provide escaped versions for embedding in PowerShell commands
    let workspace_escaped = workspace_str.replace('\\', "\\\\");
    let dev_logs_escaped = dev_logs_str.replace('\\', "\\\\");
    let spawn_script_escaped = spawn_script_str.replace('\\', "\\\\");

    info!("Workspace paths resolved: root={}", workspace_str);

    Ok(CommandResponse {
        success: true,
        message: Some("Workspace paths resolved".to_string()),
        data: Some(serde_json::json!({
            "workspace_root": workspace_str,
            "dev_logs_path": dev_logs_str,
            "scripts_path": scripts_str,
            "spawn_script": spawn_script_str,
            // Escaped versions for PowerShell
            "workspace_root_escaped": workspace_escaped,
            "dev_logs_path_escaped": dev_logs_escaped,
            "spawn_script_escaped": spawn_script_escaped
        })),
    })
}

/// Get the configured Claude Code config directories.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with dirs array
#[tauri::command]
pub fn get_claude_config_dirs() -> Result<CommandResponse, String> {
    let dirs = settings::get_claude_config_dirs();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "dirs": dirs })),
    })
}

/// Save the configured Claude Code config directories.
///
/// Validates that each path has a `projects/` subdirectory before saving.
///
/// # Arguments
/// * `dirs` - List of directory paths to save
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if settings cannot be saved
#[tauri::command]
pub fn save_claude_config_dirs(dirs: Vec<String>) -> Result<CommandResponse, String> {
    info!("Saving {} Claude config dirs", dirs.len());

    // Validate each path has a projects/ subdirectory
    let valid_dirs: Vec<String> = dirs
        .into_iter()
        .filter(|d| std::path::Path::new(d).join("projects").exists())
        .collect();

    settings::save_claude_config_dirs(valid_dirs.clone())
        .map_err(|e| format!("Failed to save Claude config dirs: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Saved {} Claude config dirs", valid_dirs.len())),
        data: Some(serde_json::json!({ "dirs": valid_dirs })),
    })
}
