use base64::{engine::general_purpose::STANDARD, Engine};
use std::fs;
use std::sync::OnceLock;
use tauri::Manager;

pub mod accessibility;
// Pure install-interception core (classify + gate + wire types), shared by the
// `qontinui-runner` bin (via `install_effects_producer::intercept`) AND the
// standalone `qontinui-shim` Windows `.exe` shadow stub. Lifted into the lib
// crate because a second bin cannot import from the runner bin's module tree.
// Pure logic only — no async, no coord/keyring deps. See the module doc.
pub mod intercept_core;
pub mod observable_bridge;
pub mod profiles;
pub mod relay_envelopes;
pub mod schema_export;
pub mod tauri_event_payloads;

// Exposed for the `qontinui_profile device pair` CLI (and any other binary
// that needs the encrypted token store outside the Tauri runtime). Both
// modules are Tauri-free.
pub mod auth;
pub mod secure_storage;

// Machine-side dev-environment capture agent (feat/devenv-environments). Runs
// on a developer's machine, captures that machine's real dev-environment
// configuration (SECRET-FREE), and POSTs it to the qontinui-web backend so the
// server computes drift vs a canonical machine. Auth is a per-machine API key
// (`X-Machine-Key: mk_<token>`), NOT a user JWT. Lives in the lib crate so both
// the `qontinui_profile env` CLI and the Tauri runner GUI share one code path.
pub mod env_agent;

// Device-pairing flow (headless + browser-mediated). Lifted out of
// `bin/qontinui_profile.rs` so both the CLI and the Tauri runner GUI
// share one code path. See `pair.rs` for the canonical wire shapes.
pub mod pair;

// Cognito Hosted-UI sign-in (RFC 8252 PKCE). Phase 5 of the
// unified-Cognito-identity plan. Tauri-free (loopback + system browser); the
// `cognito_sign_in` Tauri command in `commands::auth` drives it.
pub mod cognito;

// `coord doctor` self-check (plan 2026-06-13 Phase 4). Lifted into the lib so
// BOTH the standalone `coord_doctor` bin and the in-app Tauri command
// (`crate::coord_doctor` in the runner binary) share one driver + formatter +
// the 7-check wiring. Reuses the lib's `auth`/`pair`/`secure_storage`/`profiles`
// modules (which compile into the runner binary too), so the bin and the
// command produce an identical report.
pub mod coord_doctor;

// Harness markdown -> work-unit adapter (plan
// `2026-06-18-harness-markdown-to-workunit-adapter`, P2 of the plan-decoupling
// program). Phase 1 = the pure parser that turns operator plan markdown into a
// structured work-unit (slug + opaque status + phase sub-units + dependency
// edges), richer than coord's old slug+status projection. Later phases add the
// push client + trigger as siblings under this module.
pub mod plan_workunit_adapter;

// ============================================================================
// Main window label abstraction
// ============================================================================

/// The main window label. Always `"main"` — the window is created
/// programmatically in `.setup()` for both primary and secondary
/// instances. Secondary instances get an isolated WebView2 profile via
/// `data_directory()` but share the same label.
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
    MAIN_WINDOW_LABEL
        .get()
        .map(|s| s.as_str())
        .unwrap_or("main")
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
