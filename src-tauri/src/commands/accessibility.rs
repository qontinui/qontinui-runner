//! Accessibility commands
//!
//! This module handles accessibility-related operations:
//! - Getting and saving accessibility settings (Chrome path, CDP port)
//! - Launching Chrome with remote debugging enabled
//! - Native accessibility tree capture, querying, and interaction via AccessibilityManager
//!
//! # Typed errors (Workstream D)
//!
//! Handlers keep `-> Result<T, String>` at the Tauri boundary and build
//! errors via [`AppError`] internally, converting with
//! `.map_err(String::from)`. See `commands/mod.rs` for the migration guide.

use crate::error::AppError;
use crate::event_system::AppEvent;
use crate::settings::{self, AccessibilitySettings};
use qontinui_runner_lib::accessibility::model::UnifiedRole;
use qontinui_runner_lib::accessibility::traits::ConnectionTarget;
use qontinui_runner_lib::accessibility::AccessibilityManager;

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

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

/// Internal implementation of [`save_accessibility_settings`] returning [`AppError`].
fn save_accessibility_settings_impl(
    chrome_path: Option<String>,
    cdp_port: u16,
    use_rust_accessibility: bool,
) -> Result<CommandResponse, AppError> {
    info!(
        "Saving accessibility settings: chrome_path={:?}, cdp_port={}, use_rust_accessibility={}",
        chrome_path, cdp_port, use_rust_accessibility
    );

    let accessibility_settings = AccessibilitySettings {
        chrome_path,
        cdp_port,
        use_rust_accessibility,
    };

    settings::save_accessibility_settings(accessibility_settings).map_err(AppError::ConfigError)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Accessibility settings saved".to_string()),
        data: None,
    })
}

/// Save accessibility settings.
#[tauri::command]
pub fn save_accessibility_settings(
    chrome_path: Option<String>,
    cdp_port: u16,
    use_rust_accessibility: bool,
) -> Result<CommandResponse, String> {
    save_accessibility_settings_impl(chrome_path, cdp_port, use_rust_accessibility)
        .map_err(String::from)
}

/// Internal implementation of [`launch_chrome_debug`] returning [`AppError`].
fn launch_chrome_debug_impl(
    port: Option<u16>,
    user_data_dir: Option<String>,
) -> Result<CommandResponse, AppError> {
    let accessibility_settings = settings::get_accessibility_settings();

    // Determine Chrome path
    let chrome_path = accessibility_settings
        .chrome_path
        .or_else(find_chrome_executable)
        .ok_or_else(|| {
            AppError::ConfigError(
                "Chrome not found. Please set the Chrome path in Settings > Accessibility."
                    .to_string(),
            )
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

    // Launch Chrome. `?` lifts `std::io::Error` into `AppError::IoError` via
    // the blanket `From<std::io::Error>` impl.
    let child = if cfg!(target_os = "windows") {
        crate::process_helpers::no_window(&chrome_path)
            .args(&args)
            .spawn()?
    } else {
        crate::process_helpers::no_window(&chrome_path)
            .args(&args)
            .spawn()?
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

/// Launch Chrome/Chromium with remote debugging enabled.
#[tauri::command]
pub fn launch_chrome_debug(
    port: Option<u16>,
    user_data_dir: Option<String>,
) -> Result<CommandResponse, String> {
    launch_chrome_debug_impl(port, user_data_dir).map_err(String::from)
}

/// Check if Chrome is available (either configured or auto-detected).
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

/// Internal implementation of [`a11y_connect`] returning [`AppError`].
async fn a11y_connect_impl<R: Runtime>(
    target: String,
    backend: String,
    app: AppHandle<R>,
    state: &TokioMutex<AccessibilityManager>,
) -> Result<serde_json::Value, AppError> {
    info!("a11y_connect: target={}, backend={}", target, backend);

    let connection_target = parse_connection_target(&target, &backend)?;

    let mut mgr = state.lock().await;
    mgr.connect(connection_target, 30_000)
        .await
        .map_err(|e| AppError::CommunicationError(format!("Failed to connect: {}", e)))?;

    let backend_name = mgr.backend_name().to_string();
    drop(mgr);

    // Emit tauri event so UI Bridge observers (e.g., Accessibility Explorer) can react.
    let event = AppEvent::accessibility_connected(&backend_name, Some(target.clone()));
    if let Err(e) = app.emit(event.event_name(), &event) {
        warn!("Event emission failed for {}: {}", event.event_name(), e);
    }

    Ok(serde_json::json!({
        "connected": true,
        "backend": backend_name,
    }))
}

/// Connect the accessibility manager to a target.
#[tauri::command]
pub async fn a11y_connect<R: Runtime>(
    target: String,
    backend: String,
    app: AppHandle<R>,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    a11y_connect_impl(target, backend, app, &state)
        .await
        .map_err(String::from)
}

/// Internal implementation of [`a11y_capture`] returning [`AppError`].
async fn a11y_capture_impl<R: Runtime>(
    include_hidden: bool,
    max_depth: Option<u32>,
    app: AppHandle<R>,
    state: &TokioMutex<AccessibilityManager>,
) -> Result<serde_json::Value, AppError> {
    info!(
        "a11y_capture: include_hidden={}, max_depth={:?}",
        include_hidden, max_depth
    );

    let start = std::time::Instant::now();

    let mut mgr = state.lock().await;
    let snapshot = mgr
        .capture(max_depth, include_hidden)
        .await
        .map_err(|e| AppError::CommunicationError(format!("Capture failed: {}", e)))?;

    let duration_ms = start.elapsed().as_millis() as u64;
    let node_count = snapshot.total_nodes;
    let backend_name = mgr.backend_name().to_string();
    drop(mgr);

    // Emit tauri event so UI Bridge observers can surface capture completion.
    let event = AppEvent::accessibility_capture_complete(&backend_name, node_count, duration_ms);
    if let Err(e) = app.emit(event.event_name(), &event) {
        warn!("Event emission failed for {}: {}", event.event_name(), e);
    }

    let value = serde_json::to_value(&snapshot)?;
    Ok(value)
}

/// Capture the accessibility tree.
#[tauri::command]
pub async fn a11y_capture<R: Runtime>(
    include_hidden: bool,
    max_depth: Option<u32>,
    app: AppHandle<R>,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    a11y_capture_impl(include_hidden, max_depth, app, &state)
        .await
        .map_err(String::from)
}

/// Internal implementation of [`a11y_query`] returning [`AppError`].
async fn a11y_query_impl(
    role: Option<String>,
    label: Option<String>,
    label_contains: Option<String>,
    interactive_only: bool,
    state: &TokioMutex<AccessibilityManager>,
) -> Result<serde_json::Value, AppError> {
    let mgr = state.lock().await;

    let snapshot = mgr.snapshot().await.ok_or_else(|| {
        AppError::StateError("No accessibility tree captured yet".to_string())
    })?;

    let mut builder = mgr.query();

    if let Some(ref role_str) = role {
        let parsed_role: UnifiedRole =
            serde_json::from_value(serde_json::Value::String(role_str.clone()))
                .map_err(|_| AppError::ValidationError(format!("Unknown role: {}", role_str)))?;
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

/// Query the cached accessibility tree by role, label, and interactivity.
#[tauri::command]
pub async fn a11y_query(
    role: Option<String>,
    label: Option<String>,
    label_contains: Option<String>,
    interactive_only: bool,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    a11y_query_impl(role, label, label_contains, interactive_only, &state)
        .await
        .map_err(String::from)
}

/// Internal implementation of [`a11y_click`] returning [`AppError`].
async fn a11y_click_impl(
    ref_id: String,
    state: &TokioMutex<AccessibilityManager>,
) -> Result<serde_json::Value, AppError> {
    info!("a11y_click: ref_id={}", ref_id);
    let mgr = state.lock().await;
    let result = mgr
        .click(&ref_id)
        .await
        .map_err(|e| AppError::CommunicationError(format!("Click failed: {}", e)))?;

    let value = serde_json::to_value(&result)?;
    Ok(value)
}

/// Click an element by its ref ID.
#[tauri::command]
pub async fn a11y_click(
    ref_id: String,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    a11y_click_impl(ref_id, &state).await.map_err(String::from)
}

/// Internal implementation of [`a11y_type_text`] returning [`AppError`].
async fn a11y_type_text_impl(
    ref_id: String,
    text: String,
    clear_first: bool,
    state: &TokioMutex<AccessibilityManager>,
) -> Result<serde_json::Value, AppError> {
    info!(
        "a11y_type_text: ref_id={}, clear_first={}",
        ref_id, clear_first
    );
    let mgr = state.lock().await;
    let result = mgr
        .type_text(&ref_id, &text, clear_first)
        .await
        .map_err(|e| AppError::CommunicationError(format!("Type text failed: {}", e)))?;

    let value = serde_json::to_value(&result)?;
    Ok(value)
}

/// Type text into an element by its ref ID.
#[tauri::command]
pub async fn a11y_type_text(
    ref_id: String,
    text: String,
    clear_first: bool,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    a11y_type_text_impl(ref_id, text, clear_first, &state)
        .await
        .map_err(String::from)
}

/// Internal implementation of [`a11y_focus`] returning [`AppError`].
async fn a11y_focus_impl(
    ref_id: String,
    state: &TokioMutex<AccessibilityManager>,
) -> Result<serde_json::Value, AppError> {
    info!("a11y_focus: ref_id={}", ref_id);
    let mgr = state.lock().await;
    let result = mgr
        .focus(&ref_id)
        .await
        .map_err(|e| AppError::CommunicationError(format!("Focus failed: {}", e)))?;

    let value = serde_json::to_value(&result)?;
    Ok(value)
}

/// Focus an element by its ref ID.
#[tauri::command]
pub async fn a11y_focus(
    ref_id: String,
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    a11y_focus_impl(ref_id, &state).await.map_err(String::from)
}

/// Generate an AI-friendly text representation of the current accessibility tree.
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

/// Internal implementation of [`a11y_disconnect`] returning [`AppError`].
async fn a11y_disconnect_impl(
    state: &TokioMutex<AccessibilityManager>,
) -> Result<serde_json::Value, AppError> {
    info!("a11y_disconnect");
    let mut mgr = state.lock().await;
    mgr.disconnect()
        .await
        .map_err(|e| AppError::CommunicationError(format!("Disconnect failed: {}", e)))?;

    Ok(serde_json::json!({
        "connected": false,
    }))
}

/// Disconnect from the current accessibility source.
#[tauri::command]
pub async fn a11y_disconnect(
    state: tauri::State<'_, TokioMutex<AccessibilityManager>>,
) -> Result<serde_json::Value, String> {
    a11y_disconnect_impl(&state).await.map_err(String::from)
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
fn parse_connection_target(target: &str, _backend: &str) -> Result<ConnectionTarget, AppError> {
    let lower = target.to_lowercase();

    if lower == "desktop" {
        return Ok(ConnectionTarget::Desktop);
    }

    if let Some(pid_str) = lower.strip_prefix("pid:") {
        let pid: u32 = pid_str
            .trim()
            .parse()
            .map_err(|_| AppError::ValidationError(format!("Invalid PID: {}", pid_str)))?;
        return Ok(ConnectionTarget::ProcessId(pid));
    }

    // Default: treat as window title
    Ok(ConnectionTarget::WindowTitle(target.to_string()))
}

/// Tauri plugin exposing all accessibility commands.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_accessibility")
        .invoke_handler(tauri::generate_handler![
            get_accessibility_settings,
            save_accessibility_settings,
            launch_chrome_debug,
            check_chrome_available,
            a11y_connect,
            a11y_capture,
            a11y_query,
            a11y_click,
            a11y_type_text,
            a11y_focus,
            a11y_ai_context,
            a11y_disconnect,
        ])
        .build()
}
