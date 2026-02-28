//! Accessibility commands
//!
//! This module handles accessibility-related operations:
//! - Getting and saving accessibility settings (Chrome path, CDP port)
//! - Launching Chrome with remote debugging enabled

use crate::settings::{self, AccessibilitySettings};

use tracing::info;

use super::CommandResponse;

// ============================================================================
// Platform-specific Chrome paths
// ============================================================================

/// Get common Chrome installation paths for the current platform
fn get_default_chrome_paths() -> Vec<&'static str> {
    if cfg!(target_os = "windows") {
        vec![
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Users\%USERNAME%\AppData\Local\Google\Chrome\Application\chrome.exe",
            // Edge (Chromium-based)
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        ]
    } else if cfg!(target_os = "macos") {
        vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ]
    } else {
        // Linux
        vec![
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ]
    }
}

/// Find the first existing Chrome/Chromium executable
fn find_chrome_executable() -> Option<String> {
    for path in get_default_chrome_paths() {
        // Handle Windows %USERNAME% variable
        let expanded_path = if cfg!(target_os = "windows") && path.contains("%USERNAME%") {
            if let Ok(username) = std::env::var("USERNAME") {
                path.replace("%USERNAME%", &username)
            } else {
                path.to_string()
            }
        } else {
            path.to_string()
        };

        if std::path::Path::new(&expanded_path).exists() {
            return Some(expanded_path);
        }
    }
    None
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get the current accessibility settings.
///
/// Returns the accessibility settings from the persistent settings file.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with accessibility settings data
/// * `Err(String)` - Error message if settings cannot be loaded
#[tauri::command]
pub fn get_accessibility_settings() -> Result<CommandResponse, String> {
    info!("Getting accessibility settings");

    let accessibility_settings = settings::get_accessibility_settings();

    // Also include auto-detected Chrome path if none is set
    let auto_detected_path = if accessibility_settings.chrome_path.is_none() {
        find_chrome_executable()
    } else {
        None
    };

    Ok(CommandResponse {
        success: true,
        message: Some("Accessibility settings retrieved".to_string()),
        data: Some(serde_json::json!({
            "chrome_path": accessibility_settings.chrome_path,
            "cdp_port": accessibility_settings.cdp_port,
            "auto_detected_path": auto_detected_path,
        })),
    })
}

/// Save accessibility settings.
///
/// Updates the accessibility settings in the persistent settings file.
///
/// # Arguments
/// * `chrome_path` - Optional path to Chrome/Chromium executable
/// * `cdp_port` - CDP port for remote debugging
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if settings cannot be saved
#[tauri::command]
pub fn save_accessibility_settings(
    chrome_path: Option<String>,
    cdp_port: u16,
) -> Result<CommandResponse, String> {
    info!(
        "Saving accessibility settings: chrome_path={:?}, cdp_port={}",
        chrome_path, cdp_port
    );

    let accessibility_settings = AccessibilitySettings {
        chrome_path,
        cdp_port,
    };

    settings::save_accessibility_settings(accessibility_settings)
        .map_err(|e| format!("Failed to save accessibility settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Accessibility settings saved".to_string()),
        data: None,
    })
}

/// Launch Chrome/Chromium with remote debugging enabled.
///
/// Uses the configured Chrome path or auto-detects the installation.
/// Launches with `--remote-debugging-port` flag for CDP access.
///
/// # Arguments
/// * `port` - Optional CDP port (defaults to settings or 9222)
/// * `user_data_dir` - Optional separate user data directory for debugging session
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with process info
/// * `Err(String)` - Error message if Chrome cannot be launched
#[tauri::command]
pub fn launch_chrome_debug(
    port: Option<u16>,
    user_data_dir: Option<String>,
) -> Result<CommandResponse, String> {
    let accessibility_settings = settings::get_accessibility_settings();

    // Determine Chrome path
    let chrome_path = accessibility_settings
        .chrome_path
        .or_else(find_chrome_executable)
        .ok_or_else(|| {
            "Chrome not found. Please set the Chrome path in Settings > Accessibility.".to_string()
        })?;

    // Determine port
    let cdp_port = port.unwrap_or(accessibility_settings.cdp_port);

    info!(
        "Launching Chrome with remote debugging: path={}, port={}",
        chrome_path, cdp_port
    );

    // Build command arguments
    let mut args = vec![format!("--remote-debugging-port={}", cdp_port)];

    // Add user data dir if specified (allows running alongside regular Chrome)
    if let Some(ref data_dir) = user_data_dir {
        args.push(format!("--user-data-dir={}", data_dir));
    } else {
        // Use a separate profile directory by default to avoid conflicts
        let temp_dir = std::env::temp_dir().join("qontinui-chrome-debug");
        args.push(format!("--user-data-dir={}", temp_dir.display()));
    }

    // Launch Chrome
    let child = if cfg!(target_os = "windows") {
        crate::process_helpers::no_window(&chrome_path)
            .args(&args)
            .spawn()
            .map_err(|e| format!("Failed to launch Chrome: {}", e))?
    } else {
        crate::process_helpers::no_window(&chrome_path)
            .args(&args)
            .spawn()
            .map_err(|e| format!("Failed to launch Chrome: {}", e))?
    };

    let pid = child.id();
    info!("Chrome launched with PID: {}", pid);

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Chrome launched with remote debugging on port {}",
            cdp_port
        )),
        data: Some(serde_json::json!({
            "pid": pid,
            "port": cdp_port,
            "chrome_path": chrome_path,
        })),
    })
}

/// Check if Chrome is available (either configured or auto-detected).
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with availability info
#[tauri::command]
pub fn check_chrome_available() -> Result<CommandResponse, String> {
    let accessibility_settings = settings::get_accessibility_settings();

    let configured_path = accessibility_settings.chrome_path.clone();
    let auto_detected = find_chrome_executable();

    let is_available = configured_path.is_some() || auto_detected.is_some();
    let effective_path = configured_path.or(auto_detected);

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "available": is_available,
            "path": effective_path,
            "cdp_port": accessibility_settings.cdp_port,
        })),
    })
}
