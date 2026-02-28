//! Mobile Development Feedback Module
//!
//! Provides Tauri commands for mobile development feedback integration.
//! Enables autonomous AI development with visual verification of mobile apps
//! by capturing device screenshots, logcat output, and Metro bundler state.
//!
//! # Features
//! - Device screenshot capture via ADB
//! - Logcat monitoring and filtering
//! - Metro bundler state tracking
//! - Mobile state persistence in SQLite
//! - Integration with task runs for AI context

use crate::commands::{AppState, CommandResponse};
use crate::database::{CreateMobileLogInput, CreateMobileStateInput};
use crate::settings;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tauri::State;

use tracing::{debug, info, warn};

// ============================================================================
// Types
// ============================================================================

/// Information about a connected Android device
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileDevice {
    pub device_id: String,
    pub device_type: String, // "emulator" or "physical"
    pub model: Option<String>,
    pub state: String, // "device", "offline", "unauthorized"
}

/// Result of capturing a mobile screenshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileScreenshotResult {
    pub success: bool,
    pub screenshot_path: Option<String>,
    pub device_id: String,
    pub error: Option<String>,
}

/// Result of capturing logcat
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogcatResult {
    pub success: bool,
    pub log_path: Option<String>,
    pub lines_captured: usize,
    pub error: Option<String>,
}

/// Input for creating a mobile state capture
#[derive(Debug, Deserialize)]
pub struct CreateMobileStateRequest {
    pub task_run_id: String,
    pub device_id: Option<String>,
    pub device_type: Option<String>,
    pub device_model: Option<String>,
    pub app_package: Option<String>,
    pub app_activity: Option<String>,
    pub app_state: Option<String>,
    pub metro_connected: Option<bool>,
    pub bundle_status: Option<String>,
    pub last_reload_type: Option<String>,
    pub screenshot_path: Option<String>,
    pub logcat_path: Option<String>,
    pub has_errors: Option<bool>,
    pub error_summary: Option<String>,
}

/// Input for creating a mobile log entry
#[derive(Debug, Deserialize)]
pub struct CreateMobileLogRequest {
    pub task_run_id: String,
    pub mobile_state_id: Option<i64>,
    pub log_source: String,
    pub log_level: Option<String>,
    pub log_tag: Option<String>,
    pub message: String,
    pub raw_line: Option<String>,
    pub data: Option<String>,
    pub error_type: Option<String>,
    pub error_code: Option<String>,
    pub stack_trace: Option<String>,
    pub file_path: Option<String>,
    pub line_number: Option<i32>,
    pub column_number: Option<i32>,
    pub device_timestamp: Option<String>,
}

// ============================================================================
// ADB Utilities
// ============================================================================

/// Find the ADB executable path.
/// First checks user settings, then environment variables, then common paths.
fn find_adb_path() -> Option<PathBuf> {
    // First, check user-configured ADB path from settings
    let mobile_settings = settings::get_mobile_settings();
    if let Some(custom_path) = mobile_settings.adb_path {
        let path = PathBuf::from(&custom_path);
        if path.exists() {
            debug!("Using custom ADB path from settings: {:?}", path);
            return Some(path);
        } else {
            warn!(
                "Custom ADB path from settings does not exist: {:?}",
                custom_path
            );
        }
    }

    // Check ANDROID_HOME environment variable
    if let Ok(android_home) = std::env::var("ANDROID_HOME") {
        let adb_path = PathBuf::from(&android_home)
            .join("platform-tools")
            .join(if cfg!(windows) { "adb.exe" } else { "adb" });
        if adb_path.exists() {
            return Some(adb_path);
        }
    }

    // Check ANDROID_SDK_ROOT
    if let Ok(sdk_root) = std::env::var("ANDROID_SDK_ROOT") {
        let adb_path = PathBuf::from(&sdk_root)
            .join("platform-tools")
            .join(if cfg!(windows) { "adb.exe" } else { "adb" });
        if adb_path.exists() {
            return Some(adb_path);
        }
    }

    // Check common locations
    let common_paths = if cfg!(windows) {
        vec![
            PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
                .join("Android")
                .join("Sdk")
                .join("platform-tools")
                .join("adb.exe"),
            PathBuf::from(r"C:\Android\sdk\platform-tools\adb.exe"),
            PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
                .join("AppData")
                .join("Local")
                .join("Android")
                .join("Sdk")
                .join("platform-tools")
                .join("adb.exe"),
        ]
    } else {
        vec![
            PathBuf::from("/usr/local/bin/adb"),
            PathBuf::from("/usr/bin/adb"),
            dirs::home_dir()
                .unwrap_or_default()
                .join("Android")
                .join("Sdk")
                .join("platform-tools")
                .join("adb"),
            PathBuf::from("/opt/android-sdk/platform-tools/adb"),
        ]
    };

    for path in common_paths {
        if path.exists() {
            return Some(path);
        }
    }

    // Try PATH
    if cfg!(windows) {
        Some(PathBuf::from("adb.exe"))
    } else {
        Some(PathBuf::from("adb"))
    }
}

/// Run an ADB command and return the output
async fn run_adb_command(args: &[&str]) -> Result<String, String> {
    let adb_path = find_adb_path().ok_or("ADB not found. Please install Android SDK.")?;

    debug!("Running ADB command: {:?} {:?}", adb_path, args);

    let output = crate::process_helpers::tokio_no_window(&adb_path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to run ADB: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("ADB command failed: {}", stderr))
    }
}

// ============================================================================
// Device Management Commands
// ============================================================================

/// List all connected Android devices and emulators.
///
/// Returns a list of connected devices with their IDs, types, and states.
#[tauri::command]
pub async fn list_mobile_devices(
    _state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Listing connected mobile devices");

    let output = run_adb_command(&["devices", "-l"]).await?;

    let mut devices: Vec<MobileDevice> = Vec::new();

    for line in output.lines().skip(1) {
        // Skip header
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let device_id = parts[0].to_string();
        let state = parts[1].to_string();

        // Determine device type
        let device_type = if device_id.starts_with("emulator-") {
            "emulator"
        } else {
            "physical"
        }
        .to_string();

        // Try to extract model from the line
        let model = parts
            .iter()
            .find(|p| p.starts_with("model:"))
            .map(|p| p.trim_start_matches("model:").to_string());

        devices.push(MobileDevice {
            device_id,
            device_type,
            model,
            state,
        });
    }

    info!("Found {} connected devices", devices.len());

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} devices", devices.len())),
        data: Some(
            serde_json::to_value(&devices)
                .map_err(|e| format!("Failed to serialize devices: {}", e))?,
        ),
    })
}

/// Get the first connected device ID, or None if no devices are connected.
async fn get_default_device() -> Option<String> {
    // First, check if a default device is configured in settings
    let mobile_settings = settings::get_mobile_settings();
    if let Some(default_device) = mobile_settings.default_device_id {
        // Verify the device is actually connected
        let output = run_adb_command(&["devices"]).await.ok()?;
        for line in output.lines().skip(1) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[0] == default_device && parts[1] == "device" {
                debug!("Using default device from settings: {}", default_device);
                return Some(default_device);
            }
        }
        warn!(
            "Default device from settings ({}) is not connected, falling back to first device",
            default_device
        );
    }

    // Fall back to first connected device
    let output = run_adb_command(&["devices"]).await.ok()?;

    for line in output.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[1] == "device" {
            return Some(parts[0].to_string());
        }
    }

    None
}

// ============================================================================
// Screenshot Commands
// ============================================================================

/// Capture a screenshot from a connected Android device.
///
/// # Arguments
/// * `task_run_id` - Optional task run ID to associate the screenshot with
/// * `device_id` - Optional device ID (uses first connected device if not specified)
/// * `output_dir` - Optional output directory (defaults to .dev-logs/mobile/screenshots)
#[tauri::command]
pub async fn capture_mobile_screenshot(
    task_run_id: Option<String>,
    device_id: Option<String>,
    output_dir: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Capturing mobile screenshot (task_run_id: {:?}, device_id: {:?})",
        task_run_id, device_id
    );

    // Get settings
    let mobile_settings = settings::get_mobile_settings();

    // Get device ID
    let device = match device_id {
        Some(id) => id,
        None => get_default_device()
            .await
            .ok_or("No connected devices found")?,
    };

    // Determine output path (parameter > settings > default)
    let output_path = match output_dir {
        Some(dir) => PathBuf::from(dir),
        None => match mobile_settings.output_dir {
            Some(ref dir) => PathBuf::from(dir).join("screenshots"),
            None => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                cwd.join(".dev-logs").join("mobile").join("screenshots")
            }
        },
    };

    // Create output directory
    std::fs::create_dir_all(&output_path)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    // Generate filename
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let filename = format!("screenshot_{}.png", timestamp);
    let local_path = output_path.join(&filename);

    // Capture screenshot on device
    let device_path = "/sdcard/screenshot_temp.png";

    // Run screencap
    let screencap_args = if device == get_default_device().await.unwrap_or_default() {
        vec!["shell", "screencap", "-p", device_path]
    } else {
        vec!["-s", &device, "shell", "screencap", "-p", device_path]
    };

    run_adb_command(&screencap_args).await?;

    // Pull screenshot to local
    let pull_args = if device == get_default_device().await.unwrap_or_default() {
        vec!["pull", device_path, local_path.to_str().unwrap()]
    } else {
        vec![
            "-s",
            &device,
            "pull",
            device_path,
            local_path.to_str().unwrap(),
        ]
    };

    run_adb_command(&pull_args).await?;

    // Clean up device file
    let rm_args = if device == get_default_device().await.unwrap_or_default() {
        vec!["shell", "rm", device_path]
    } else {
        vec!["-s", &device, "shell", "rm", device_path]
    };

    let _ = run_adb_command(&rm_args).await; // Ignore cleanup errors

    let screenshot_path = local_path.to_string_lossy().to_string();
    info!("Screenshot captured: {}", screenshot_path);

    // If task_run_id provided, store in database
    if let Some(ref task_id) = task_run_id {
        let input = CreateMobileStateInput {
            task_run_id: task_id.clone(),
            device_id: Some(device.clone()),
            device_type: Some(
                if device.starts_with("emulator-") {
                    "emulator"
                } else {
                    "physical"
                }
                .to_string(),
            ),
            device_model: None,
            app_package: None,
            app_activity: None,
            app_state: None,
            metro_connected: false,
            bundle_status: None,
            last_reload_type: None,
            last_reload_time: None,
            screenshot_path: Some(screenshot_path.clone()),
            logcat_path: None,
            has_errors: false,
            error_summary: None,
        };

        if let Err(e) = state.checkpoint_db.create_mobile_state(&input) {
            warn!("Failed to save mobile state to database: {}", e);
        }
    }

    let result = MobileScreenshotResult {
        success: true,
        screenshot_path: Some(screenshot_path),
        device_id: device,
        error: None,
    };

    Ok(CommandResponse {
        success: true,
        message: Some("Screenshot captured successfully".to_string()),
        data: Some(
            serde_json::to_value(&result)
                .map_err(|e| format!("Failed to serialize result: {}", e))?,
        ),
    })
}

// ============================================================================
// Logcat Commands
// ============================================================================

/// Capture logcat output from a connected Android device.
///
/// # Arguments
/// * `task_run_id` - Optional task run ID to associate logs with
/// * `device_id` - Optional device ID
/// * `lines` - Number of lines to capture (default: 500)
/// * `filter_app` - If true, filter to React Native logs only
/// * `output_dir` - Optional output directory
#[tauri::command]
pub async fn capture_mobile_logcat(
    task_run_id: Option<String>,
    device_id: Option<String>,
    lines: Option<u32>,
    filter_app: Option<bool>,
    output_dir: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Capturing mobile logcat (task_run_id: {:?}, device_id: {:?}, lines: {:?})",
        task_run_id, device_id, lines
    );

    // Get settings for defaults
    let mobile_settings = settings::get_mobile_settings();

    // Get device ID
    let device = match device_id {
        Some(id) => id,
        None => get_default_device()
            .await
            .ok_or("No connected devices found")?,
    };

    // Use settings for defaults, falling back to hardcoded values
    let line_count = lines.unwrap_or(mobile_settings.logcat_lines);
    let _filter = filter_app.unwrap_or(mobile_settings.filter_react_native);

    // Build and run logcat command
    let logcat_args_str = line_count.to_string();
    let logcat_args: Vec<&str> = vec!["-s", &device, "logcat", "-d", "-t", &logcat_args_str];

    let output = run_adb_command(&logcat_args).await?;

    // Determine output path (parameter > settings > default)
    let output_path = match output_dir {
        Some(dir) => PathBuf::from(dir),
        None => match mobile_settings.output_dir {
            Some(ref dir) => PathBuf::from(dir).join("logcat"),
            None => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                cwd.join(".dev-logs").join("mobile").join("logcat")
            }
        },
    };

    // Create output directory
    std::fs::create_dir_all(&output_path)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    // Generate filename
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let filename = format!("logcat_{}.txt", timestamp);
    let local_path = output_path.join(&filename);

    // Write logcat to file
    std::fs::write(&local_path, &output)
        .map_err(|e| format!("Failed to write logcat file: {}", e))?;

    let log_path = local_path.to_string_lossy().to_string();
    let lines_captured = output.lines().count();

    info!("Logcat captured: {} ({} lines)", log_path, lines_captured);

    // Parse and store error logs if task_run_id provided
    if let Some(ref task_id) = task_run_id {
        for line in output.lines() {
            // Parse Android logcat format: DATE TIME PID TID LEVEL TAG: MESSAGE
            if let Some((level, tag, message)) = parse_logcat_line(line) {
                if level == "E" || level == "F" || level == "W" {
                    let input = CreateMobileLogInput {
                        task_run_id: task_id.clone(),
                        mobile_state_id: None,
                        log_source: "logcat".to_string(),
                        log_level: Some(level),
                        log_tag: Some(tag),
                        message,
                        raw_line: Some(line.to_string()),
                        data: None,
                        error_type: None,
                        error_code: None,
                        stack_trace: None,
                        file_path: None,
                        line_number: None,
                        column_number: None,
                        device_timestamp: None,
                    };

                    if let Err(e) = state.checkpoint_db.create_mobile_log(&input) {
                        debug!("Failed to save mobile log: {}", e);
                    }
                }
            }
        }
    }

    let result = LogcatResult {
        success: true,
        log_path: Some(log_path),
        lines_captured,
        error: None,
    };

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Captured {} logcat lines", lines_captured)),
        data: Some(
            serde_json::to_value(&result)
                .map_err(|e| format!("Failed to serialize result: {}", e))?,
        ),
    })
}

/// Parse a logcat line into (level, tag, message)
fn parse_logcat_line(line: &str) -> Option<(String, String, String)> {
    // Format: DATE TIME PID TID LEVEL TAG: MESSAGE
    // Example: 01-18 12:34:56.789  1234  5678 E ReactNativeJS: Error message
    let parts: Vec<&str> = line.splitn(6, char::is_whitespace).collect();

    if parts.len() < 6 {
        return None;
    }

    // Find the level (single character: V, D, I, W, E, F)
    let level_idx = parts.iter().position(|p| {
        p.len() == 1 && matches!(p.chars().next(), Some('V' | 'D' | 'I' | 'W' | 'E' | 'F'))
    })?;

    let level = parts[level_idx].to_string();

    // Everything after the level is TAG: MESSAGE
    let rest = parts[level_idx + 1..].join(" ");
    let (tag, message) = if let Some(colon_pos) = rest.find(':') {
        (
            rest[..colon_pos].trim().to_string(),
            rest[colon_pos + 1..].trim().to_string(),
        )
    } else {
        ("unknown".to_string(), rest)
    };

    Some((level, tag, message))
}

// ============================================================================
// Mobile State CRUD Commands
// ============================================================================

/// Get mobile state captures for a task run.
///
/// # Arguments
/// * `task_run_id` - Task run ID to query
/// * `limit` - Maximum number of results (default: 100)
#[tauri::command]
pub async fn get_mobile_states(
    task_run_id: String,
    limit: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Getting mobile states for task run: {}", task_run_id);

    let states = state
        .checkpoint_db
        .get_mobile_states(&task_run_id, limit)
        .map_err(|e| format!("Failed to get mobile states: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} mobile states", states.len())),
        data: Some(
            serde_json::to_value(&states)
                .map_err(|e| format!("Failed to serialize states: {}", e))?,
        ),
    })
}

/// Get the latest mobile state for a task run.
///
/// # Arguments
/// * `task_run_id` - Task run ID to query
#[tauri::command]
pub async fn get_latest_mobile_state(
    task_run_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Getting latest mobile state for task run: {}", task_run_id);

    let mobile_state = state
        .checkpoint_db
        .get_latest_mobile_state(&task_run_id)
        .map_err(|e| format!("Failed to get mobile state: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: match &mobile_state {
            Some(_) => Some("Mobile state found".to_string()),
            None => Some("No mobile state found".to_string()),
        },
        data: Some(
            serde_json::to_value(&mobile_state)
                .map_err(|e| format!("Failed to serialize state: {}", e))?,
        ),
    })
}

/// Create a new mobile state capture.
///
/// # Arguments
/// * `input` - Mobile state data
#[tauri::command]
pub async fn create_mobile_state(
    input: CreateMobileStateRequest,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Creating mobile state for task run: {}", input.task_run_id);

    let db_input = CreateMobileStateInput {
        task_run_id: input.task_run_id,
        device_id: input.device_id,
        device_type: input.device_type,
        device_model: input.device_model,
        app_package: input.app_package,
        app_activity: input.app_activity,
        app_state: input.app_state,
        metro_connected: input.metro_connected.unwrap_or(false),
        bundle_status: input.bundle_status,
        last_reload_type: input.last_reload_type,
        last_reload_time: None,
        screenshot_path: input.screenshot_path,
        logcat_path: input.logcat_path,
        has_errors: input.has_errors.unwrap_or(false),
        error_summary: input.error_summary,
    };

    let mobile_state = state
        .checkpoint_db
        .create_mobile_state(&db_input)
        .map_err(|e| format!("Failed to create mobile state: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Mobile state created".to_string()),
        data: Some(
            serde_json::to_value(&mobile_state)
                .map_err(|e| format!("Failed to serialize state: {}", e))?,
        ),
    })
}

// ============================================================================
// Mobile Log Commands
// ============================================================================

/// Get mobile logs for a task run.
///
/// # Arguments
/// * `task_run_id` - Task run ID to query
/// * `log_source` - Optional filter by log source (e.g., "logcat", "metro")
/// * `errors_only` - If true, only return error logs
/// * `limit` - Maximum number of results (default: 500)
#[tauri::command]
pub async fn get_mobile_logs(
    task_run_id: String,
    log_source: Option<String>,
    errors_only: Option<bool>,
    limit: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Getting mobile logs for task run: {} (source: {:?}, errors_only: {:?})",
        task_run_id, log_source, errors_only
    );

    let logs = state
        .checkpoint_db
        .get_mobile_logs(
            &task_run_id,
            log_source.as_deref(),
            errors_only.unwrap_or(false),
            limit,
        )
        .map_err(|e| format!("Failed to get mobile logs: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} mobile logs", logs.len())),
        data: Some(
            serde_json::to_value(&logs).map_err(|e| format!("Failed to serialize logs: {}", e))?,
        ),
    })
}

/// Get mobile error logs for a task run.
///
/// # Arguments
/// * `task_run_id` - Task run ID to query
/// * `limit` - Maximum number of results (default: 100)
#[tauri::command]
pub async fn get_mobile_errors(
    task_run_id: String,
    limit: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Getting mobile errors for task run: {}", task_run_id);

    let errors = state
        .checkpoint_db
        .get_mobile_errors(&task_run_id, limit)
        .map_err(|e| format!("Failed to get mobile errors: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} mobile errors", errors.len())),
        data: Some(
            serde_json::to_value(&errors)
                .map_err(|e| format!("Failed to serialize errors: {}", e))?,
        ),
    })
}

/// Create a new mobile log entry.
///
/// # Arguments
/// * `input` - Mobile log data
#[tauri::command]
pub async fn create_mobile_log(
    input: CreateMobileLogRequest,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Creating mobile log for task run: {} (source: {})",
        input.task_run_id, input.log_source
    );

    let db_input = CreateMobileLogInput {
        task_run_id: input.task_run_id,
        mobile_state_id: input.mobile_state_id,
        log_source: input.log_source,
        log_level: input.log_level,
        log_tag: input.log_tag,
        message: input.message,
        raw_line: input.raw_line,
        data: input.data,
        error_type: input.error_type,
        error_code: input.error_code,
        stack_trace: input.stack_trace,
        file_path: input.file_path,
        line_number: input.line_number,
        column_number: input.column_number,
        device_timestamp: input.device_timestamp,
    };

    let log = state
        .checkpoint_db
        .create_mobile_log(&db_input)
        .map_err(|e| format!("Failed to create mobile log: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Mobile log created".to_string()),
        data: Some(
            serde_json::to_value(&log).map_err(|e| format!("Failed to serialize log: {}", e))?,
        ),
    })
}

// ============================================================================
// Combined Capture Command
// ============================================================================

/// Capture both screenshot and logcat from a mobile device.
///
/// This is a convenience command that captures both in one call,
/// storing the results in the database if a task_run_id is provided.
///
/// # Arguments
/// * `task_run_id` - Task run ID to associate captures with
/// * `device_id` - Optional device ID
/// * `logcat_lines` - Number of logcat lines to capture (default: 500)
#[tauri::command]
pub async fn capture_mobile_feedback(
    task_run_id: String,
    device_id: Option<String>,
    logcat_lines: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Capturing mobile feedback for task run: {} (device: {:?})",
        task_run_id, device_id
    );

    // Get device ID
    let device = match device_id {
        Some(id) => id,
        None => get_default_device()
            .await
            .ok_or("No connected devices found")?,
    };

    // Capture screenshot
    let screenshot_result = capture_mobile_screenshot(
        Some(task_run_id.clone()),
        Some(device.clone()),
        None,
        state.clone(),
    )
    .await;

    // Capture logcat
    let logcat_result = capture_mobile_logcat(
        Some(task_run_id.clone()),
        Some(device.clone()),
        logcat_lines,
        Some(true), // Filter to app logs
        None,
        state.clone(),
    )
    .await;

    // Combine results
    let screenshot_path = screenshot_result.as_ref().ok().and_then(|r| {
        r.data
            .as_ref()
            .and_then(|d| d.get("screenshot_path"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    let logcat_path = logcat_result.as_ref().ok().and_then(|r| {
        r.data
            .as_ref()
            .and_then(|d| d.get("log_path"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    let has_errors = screenshot_result.is_err() || logcat_result.is_err();

    // Create a mobile state with both captures
    let input = CreateMobileStateInput {
        task_run_id: task_run_id.clone(),
        device_id: Some(device.clone()),
        device_type: Some(
            if device.starts_with("emulator-") {
                "emulator"
            } else {
                "physical"
            }
            .to_string(),
        ),
        device_model: None,
        app_package: None,
        app_activity: None,
        app_state: Some("captured".to_string()),
        metro_connected: false,
        bundle_status: None,
        last_reload_type: None,
        last_reload_time: None,
        screenshot_path: screenshot_path.clone(),
        logcat_path: logcat_path.clone(),
        has_errors,
        error_summary: if has_errors {
            Some("Capture errors occurred".to_string())
        } else {
            None
        },
    };

    let mobile_state = state
        .checkpoint_db
        .create_mobile_state(&input)
        .map_err(|e| format!("Failed to create mobile state: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Mobile feedback captured".to_string()),
        data: Some(serde_json::json!({
            "mobile_state_id": mobile_state.id,
            "screenshot_path": screenshot_path,
            "logcat_path": logcat_path,
            "device_id": device,
            "has_errors": has_errors,
        })),
    })
}

/// Delete all mobile data for a task run.
///
/// # Arguments
/// * `task_run_id` - Task run ID to delete data for
#[tauri::command]
pub async fn delete_mobile_data(
    task_run_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Deleting mobile data for task run: {}", task_run_id);

    let (states_deleted, logs_deleted) = state
        .checkpoint_db
        .delete_mobile_data_for_task(&task_run_id)
        .map_err(|e| format!("Failed to delete mobile data: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Deleted {} mobile states and {} logs",
            states_deleted, logs_deleted
        )),
        data: Some(serde_json::json!({
            "states_deleted": states_deleted,
            "logs_deleted": logs_deleted,
        })),
    })
}
