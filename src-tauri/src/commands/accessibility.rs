//! Accessibility commands
//!
//! This module handles accessibility-related operations:
//! - Getting and saving accessibility settings (Chrome path, CDP port)
//! - Launching Chrome with remote debugging enabled
//! - Native accessibility tree capture, querying, and interaction via AccessibilityManager

use crate::settings::{self, AccessibilitySettings};
use qontinui_runner_lib::accessibility::model::UnifiedRole;
use qontinui_runner_lib::accessibility::traits::ConnectionTarget;
use qontinui_runner_lib::accessibility::AccessibilityManager;

use tokio::sync::Mutex as TokioMutex;
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
            "use_rust_accessibility": accessibility_settings.use_rust_accessibility,
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
/// * `use_rust_accessibility` - Whether to use Rust-native accessibility APIs
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if settings cannot be saved
#[tauri::command]
pub fn save_accessibility_settings(
    chrome_path: Option<String>,
    cdp_port: u16,
    use_rust_accessibility: bool,
) -> Result<CommandResponse, String> {
    info!(
        "Saving accessibility settings: chrome_path={:?}, cdp_port={}, use_rust_accessibility={}",
        chrome_path, cdp_port, use_rust_accessibility
    );

    let accessibility_settings = AccessibilitySettings {
        chrome_path,
        cdp_port,
        use_rust_accessibility,
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

// ============================================================================
// Native Accessibility Manager Commands
// ============================================================================

/// Connect the accessibility manager to a target.
///
/// # Arguments
/// * `target` - Connection target: "desktop", a window title, "cdp://host:port", or "pid:1234"
/// * `backend` - Backend hint (currently unused; the manager auto-selects)
///
/// # Returns
/// * `Ok(serde_json::Value)` - Connection result with backend info
/// * `Err(String)` - Error message
#[tauri::command]
pub async fn a11y_connect(
    target: String,
    backend: String,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    info!("a11y_connect: target={}, backend={}", target, backend);

    let connection_target = parse_connection_target(&target, &backend)?;

    let mut mgr = state.lock().await;
    mgr.connect(connection_target, 30_000)
        .await
        .map_err(|e| format!("Failed to connect: {}", e))?;

    Ok(serde_json::json!({
        "connected": true,
        "backend": mgr.backend_name(),
    }))
}

/// Capture the accessibility tree.
///
/// # Arguments
/// * `include_hidden` - Whether to include hidden elements
/// * `max_depth` - Optional maximum depth for the tree traversal
///
/// # Returns
/// * `Ok(serde_json::Value)` - The captured snapshot serialized as JSON
/// * `Err(String)` - Error message
#[tauri::command]
pub async fn a11y_capture(
    include_hidden: bool,
    max_depth: Option<u32>,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    info!(
        "a11y_capture: include_hidden={}, max_depth={:?}",
        include_hidden, max_depth
    );

    let mut mgr = state.lock().await;
    let snapshot = mgr
        .capture(max_depth, include_hidden)
        .await
        .map_err(|e| format!("Capture failed: {}", e))?;

    serde_json::to_value(&snapshot).map_err(|e| format!("Serialization failed: {}", e))
}

/// Query the cached accessibility tree by role, label, and interactivity.
///
/// # Arguments
/// * `role` - Optional role filter (e.g., "button", "textbox")
/// * `label` - Optional exact label match
/// * `label_contains` - Optional substring label match
/// * `interactive_only` - If true, only return interactive elements
///
/// # Returns
/// * `Ok(serde_json::Value)` - Array of matching nodes
/// * `Err(String)` - Error message
#[tauri::command]
pub async fn a11y_query(
    role: Option<String>,
    label: Option<String>,
    label_contains: Option<String>,
    interactive_only: bool,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    let mgr = state.lock().await;

    let snapshot = mgr
        .snapshot()
        .await
        .ok_or_else(|| "No accessibility tree captured yet".to_string())?;

    let mut builder = mgr.query();

    if let Some(ref role_str) = role {
        let parsed_role: UnifiedRole =
            serde_json::from_value(serde_json::Value::String(role_str.clone()))
                .map_err(|_| format!("Unknown role: {}", role_str))?;
        builder = builder.by_role(parsed_role);
    }

    if let Some(ref lbl) = label {
        builder = builder.by_label(lbl.as_str());
    }

    if let Some(ref substr) = label_contains {
        builder = builder
            .by_label_contains(substr.as_str())
            .case_insensitive();
    }

    if interactive_only {
        builder = builder.interactive();
    }

    let results = builder.find_all(&snapshot.root);

    let nodes: Vec<serde_json::Value> = results
        .iter()
        .map(|n| {
            serde_json::json!({
                "ref_id": n.ref_id,
                "role": n.role.as_str(),
                "name": n.name,
                "value": n.value,
                "is_interactive": n.is_interactive,
                "bounds": n.bounds,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "count": nodes.len(),
        "nodes": nodes,
    }))
}

/// Click an element by its ref ID.
///
/// # Arguments
/// * `ref_id` - The reference ID (e.g., "@e3")
///
/// # Returns
/// * `Ok(serde_json::Value)` - Interaction result
/// * `Err(String)` - Error message
#[tauri::command]
pub async fn a11y_click(
    ref_id: String,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    info!("a11y_click: ref_id={}", ref_id);
    let mgr = state.lock().await;
    let result = mgr
        .click(&ref_id)
        .await
        .map_err(|e| format!("Click failed: {}", e))?;

    serde_json::to_value(&result).map_err(|e| format!("Serialization failed: {}", e))
}

/// Type text into an element by its ref ID.
///
/// # Arguments
/// * `ref_id` - The reference ID (e.g., "@e2")
/// * `text` - Text to type
/// * `clear_first` - If true, clear existing content before typing
///
/// # Returns
/// * `Ok(serde_json::Value)` - Interaction result
/// * `Err(String)` - Error message
#[tauri::command]
pub async fn a11y_type_text(
    ref_id: String,
    text: String,
    clear_first: bool,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    info!(
        "a11y_type_text: ref_id={}, clear_first={}",
        ref_id, clear_first
    );
    let mgr = state.lock().await;
    let result = mgr
        .type_text(&ref_id, &text, clear_first)
        .await
        .map_err(|e| format!("Type text failed: {}", e))?;

    serde_json::to_value(&result).map_err(|e| format!("Serialization failed: {}", e))
}

/// Focus an element by its ref ID.
///
/// # Arguments
/// * `ref_id` - The reference ID (e.g., "@e1")
///
/// # Returns
/// * `Ok(serde_json::Value)` - Interaction result
/// * `Err(String)` - Error message
#[tauri::command]
pub async fn a11y_focus(
    ref_id: String,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    info!("a11y_focus: ref_id={}", ref_id);
    let mgr = state.lock().await;
    let result = mgr
        .focus(&ref_id)
        .await
        .map_err(|e| format!("Focus failed: {}", e))?;

    serde_json::to_value(&result).map_err(|e| format!("Serialization failed: {}", e))
}

/// Generate an AI-friendly text representation of the current accessibility tree.
///
/// # Arguments
/// * `max_elements` - Maximum number of elements to include (default 100)
/// * `interactive_only` - If true, only include interactive elements
///
/// # Returns
/// * `Ok(String)` - AI-friendly text representation
/// * `Err(String)` - Error message
#[tauri::command]
pub async fn a11y_ai_context(
    max_elements: Option<usize>,
    interactive_only: bool,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<String, String> {
    let mgr = state.lock().await;
    let max = max_elements.unwrap_or(100);
    Ok(mgr.to_ai_context(max, interactive_only).await)
}

/// Disconnect from the current accessibility source.
///
/// # Returns
/// * `Ok(serde_json::Value)` - Disconnection confirmation
/// * `Err(String)` - Error message
#[tauri::command]
pub async fn a11y_disconnect(
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    info!("a11y_disconnect");
    let mut mgr = state.lock().await;
    mgr.disconnect()
        .await
        .map_err(|e| format!("Disconnect failed: {}", e))?;

    Ok(serde_json::json!({
        "connected": false,
    }))
}

// ============================================================================
// Helpers
// ============================================================================

/// Parse a user-facing target string into a `ConnectionTarget`.
///
/// Accepted formats:
/// - `"desktop"` -> `ConnectionTarget::Desktop`
/// - `"pid:1234"` -> `ConnectionTarget::ProcessId(1234)`
/// - anything else -> `ConnectionTarget::WindowTitle(target)`
fn parse_connection_target(target: &str, _backend: &str) -> Result<ConnectionTarget, String> {
    let lower = target.to_lowercase();

    if lower == "desktop" {
        return Ok(ConnectionTarget::Desktop);
    }

    if let Some(pid_str) = lower.strip_prefix("pid:") {
        let pid: u32 = pid_str
            .trim()
            .parse()
            .map_err(|_| format!("Invalid PID: {}", pid_str))?;
        return Ok(ConnectionTarget::ProcessId(pid));
    }

    // Default: treat as window title
    Ok(ConnectionTarget::WindowTitle(target.to_string()))
}
