//! Tauri commands for pop-out terminal windows (Phase 1 of
//! `plans/2026-06-03-runner-popout-terminal-windows.md`).
//!
//! A single runner process hosts multiple OS windows — `"main"` plus
//! `"term-N"` pop-outs — each rendering its own subset of terminal tabs.
//! These commands open/close pop-out windows and move sessions between
//! windows, backed by the persisted [`crate::window_assignments`] state.
//!
//! Distinct from `commands::window_manager` (which enumerates/activates
//! OS desktop windows for automation) — this is about the runner's OWN
//! webview windows.
//!
//! ## Events (broadcast; each window's frontend filters by its own label)
//! - `window-opened` `{ record: WindowRecord }`
//! - `window-closed` `{ label: String }`
//! - `session-assignment-changed` `{ session_id, from: Option<String>, to: String }`
//!
//! `window-closed` and the close-driven `session-assignment-changed` events are
//! emitted from the central `on_window_event` `CloseRequested` handler in
//! `main.rs` (so they fire for BOTH the OS close button AND a programmatic
//! [`close_terminal_window`]); see [`handle_window_close`].

use std::sync::Arc;

use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::settings::SpawnPlacement;
use crate::window_assignments::{
    WindowAssignments, WindowAssignmentsState, WindowRecord, MAIN_WINDOW_LABEL,
};

/// Unix-millis now. (Plain `std::time` — this is Rust, not a workflow script.)
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Serialize, Clone)]
struct SessionAssignmentChanged {
    session_id: String,
    from: Option<String>,
    to: String,
}

/// Open a new pop-out terminal window. Allocates the next `term-N` label,
/// records it, builds the `WebviewWindow` (loading the same bundle with a
/// `?view=terminal&window=term-N` boot hint), positions it via the reused
/// placement pipeline when a `placement` is supplied, and emits `window-opened`.
/// Returns the new window record.
#[tauri::command]
pub async fn open_terminal_window(
    app: tauri::AppHandle,
    assignments: tauri::State<'_, Arc<WindowAssignments>>,
    placement: Option<SpawnPlacement>,
) -> Result<WindowRecord, String> {
    let record = assignments.create_window(None, None, now_ms());
    let label = record.label.clone();

    // Same embedded bundle; the boot hint makes it render Terminal-only and
    // adopt `term-N` as its window identity (via getCurrentWindow().label).
    let url = tauri::WebviewUrl::App(format!("index.html?view=terminal&window={}", label).into());

    let mut builder = tauri::WebviewWindowBuilder::new(&app, label.as_str(), url)
        .title(format!("Qontinui Terminal — {}", label))
        .inner_size(1000.0, 720.0)
        .min_inner_size(640.0, 400.0)
        .resizable(true)
        .decorations(true)
        .on_web_resource_request(crate::asset_headers::stamp_no_store_on_index);

    // Mirror the main window: inject the API port so the pop-out's frontend
    // addresses the runner's HTTP API (location.port is empty on tauri.localhost).
    let api_port = crate::mcp::types::get_mcp_api_port();
    builder = builder.initialization_script(format!("window.__QONTINUI_PORT__ = {};", api_port));

    let window = builder
        .build()
        .map_err(|e| format!("Failed to build pop-out window {}: {}", label, e))?;

    // Apply placement (physical global coords) when provided; otherwise let the
    // OS place it (cascaded near the focused window).
    if let Some(p) = placement {
        match crate::spawn_placement::resolve_to_global_physical(&app, &p) {
            Ok(rp) => {
                let _ = window.set_position(tauri::PhysicalPosition::new(rp.global_x, rp.global_y));
                let _ = window.set_size(tauri::PhysicalSize::new(rp.width, rp.height));
            }
            Err(e) => {
                tracing::warn!(
                    window = %label,
                    error = %e,
                    "open_terminal_window: placement resolve failed — using OS default position"
                );
            }
        }
    }

    let _ = window.show();
    let _ = window.set_focus();

    if let Err(e) = app.emit("window-opened", &record) {
        tracing::warn!(error = %e, "open_terminal_window: failed to emit window-opened");
    }

    tracing::info!(window = %label, "Opened pop-out terminal window");
    Ok(record)
}

/// Close a pop-out terminal window. The actual reassignment + events happen in
/// the central `on_window_event` `CloseRequested` handler (so the OS close
/// button takes the same path); here we just request the close. Closing
/// `"main"` is rejected.
#[tauri::command]
pub async fn close_terminal_window(app: tauri::AppHandle, label: String) -> Result<(), String> {
    if label == MAIN_WINDOW_LABEL {
        return Err("refusing to close the main window via close_terminal_window".to_string());
    }
    match app.get_webview_window(&label) {
        Some(win) => win
            .close()
            .map_err(|e| format!("Failed to close window {}: {}", label, e)),
        None => Ok(()), // already gone — idempotent
    }
}

/// Move a session (terminalId) to a window ("move tab to window N", or back to
/// "main"). Persists the new owner and emits `session-assignment-changed`.
/// A no-op assignment (already owned by `window_label`) emits nothing.
#[tauri::command]
pub async fn assign_session_to_window(
    app: tauri::AppHandle,
    assignments: tauri::State<'_, Arc<WindowAssignments>>,
    session_id: String,
    window_label: String,
) -> Result<(), String> {
    if let Some(from) = assignments.assign_session(&session_id, &window_label) {
        let payload = SessionAssignmentChanged {
            session_id,
            from: Some(from),
            to: window_label,
        };
        if let Err(e) = app.emit("session-assignment-changed", &payload) {
            tracing::warn!(error = %e, "assign_session_to_window: failed to emit event");
        }
    }
    Ok(())
}

/// Full snapshot of window assignments, to hydrate a window on load.
#[tauri::command]
pub async fn get_window_assignments(
    assignments: tauri::State<'_, Arc<WindowAssignments>>,
) -> Result<WindowAssignmentsState, String> {
    Ok(assignments.snapshot())
}

/// All windows this runner process owns (label + kind + title). Distinct from
/// the OS-window enumerator in `commands::window_manager`.
#[tauri::command]
pub async fn list_runner_windows(
    assignments: tauri::State<'_, Arc<WindowAssignments>>,
) -> Result<Vec<WindowRecord>, String> {
    Ok(assignments.window_records())
}

/// Bring a runner window to the foreground.
#[tauri::command]
pub async fn focus_runner_window(app: tauri::AppHandle, label: String) -> Result<(), String> {
    match app.get_webview_window(&label) {
        Some(win) => {
            let _ = win.unminimize();
            win.set_focus()
                .map_err(|e| format!("Failed to focus window {}: {}", label, e))
        }
        None => Err(format!("window not found: {}", label)),
    }
}

/// Central handling of a pop-out window closing — called from `main.rs`'s
/// `on_window_event` `CloseRequested` for any non-`"main"` window. Reassigns
/// the closing window's sessions to `"main"` (never orphans a PTY), emits one
/// `session-assignment-changed` per moved session, then `window-closed`.
///
/// Returns `true` if `label` was a known pop-out (so the caller can skip the
/// main-window app-quit cleanup).
pub fn handle_window_close(app: &tauri::AppHandle, label: &str) -> bool {
    if label == MAIN_WINDOW_LABEL {
        return false;
    }
    let assignments = match app.try_state::<Arc<WindowAssignments>>() {
        Some(s) => s,
        None => return false,
    };
    let close = assignments.close_window(label);

    for (session_id, from) in &close.reassigned {
        let payload = SessionAssignmentChanged {
            session_id: session_id.clone(),
            from: Some(from.clone()),
            to: MAIN_WINDOW_LABEL.to_string(),
        };
        if let Err(e) = app.emit("session-assignment-changed", &payload) {
            tracing::warn!(error = %e, "handle_window_close: failed to emit reassignment");
        }
    }

    if let Err(e) = app.emit("window-closed", &serde_json::json!({ "label": label })) {
        tracing::warn!(error = %e, "handle_window_close: failed to emit window-closed");
    }

    // Treat any non-main window as a pop-out for the purpose of skipping the
    // app-quit cleanup, even if its record was already removed (e.g. closed
    // programmatically then the OS event arrives) — `removed` only tells us
    // whether THIS call removed it.
    tracing::info!(
        window = %label,
        reassigned = close.reassigned.len(),
        "Closed pop-out terminal window"
    );
    true
}
