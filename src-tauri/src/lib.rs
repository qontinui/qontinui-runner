use base64::{engine::general_purpose::STANDARD, Engine};
use std::fs;
use std::sync::OnceLock;
use tauri::Manager;

pub mod accessibility;
pub mod schema_export;

// ============================================================================
// Main window label abstraction
// ============================================================================

/// The main window label. On the primary instance this is `"main"` (from
/// the declarative tauri.conf.json). On secondary instances (test runners
/// spawned by the supervisor), it may be `"main-isolated"` because we
/// recreate the window with an isolated WebView2 profile via
/// `data_directory()`.
///
/// Set once at startup by `main.rs` via [`set_main_window_label`] and read
/// by all callsites via [`get_main_window_label`].
static MAIN_WINDOW_LABEL: OnceLock<String> = OnceLock::new();

/// Set the main window label. Call exactly once from `main.rs` during setup.
pub fn set_main_window_label(label: &str) {
    let _ = MAIN_WINDOW_LABEL.set(label.to_string());
}

/// Get the main window label. Returns `"main"` if never explicitly set.
pub fn get_main_window_label() -> &'static str {
    MAIN_WINDOW_LABEL.get().map(|s| s.as_str()).unwrap_or("main")
}

/// Convenience: get the main WebviewWindow from an AppHandle.
/// Uses the label from [`get_main_window_label`].
pub fn get_main_window(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window(get_main_window_label())
}

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// Read an image file and return it as a base64 data URL
#[tauri::command]
fn read_image_as_base64(path: String) -> Result<String, String> {
    // Read the file
    let data = fs::read(&path).map_err(|e| format!("Failed to read file: {}", e))?;

    // Determine MIME type from extension
    let mime_type = if path.to_lowercase().ends_with(".png") {
        "image/png"
    } else if path.to_lowercase().ends_with(".jpg") || path.to_lowercase().ends_with(".jpeg") {
        "image/jpeg"
    } else if path.to_lowercase().ends_with(".gif") {
        "image/gif"
    } else if path.to_lowercase().ends_with(".webp") {
        "image/webp"
    } else {
        "application/octet-stream"
    };

    // Encode to base64 and return as data URL
    let base64_data = STANDARD.encode(&data);
    Ok(format!("data:{};base64,{}", mime_type, base64_data))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window(crate::get_main_window_label()) {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet, read_image_as_base64])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
