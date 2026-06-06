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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::settings::SpawnPlacement;
use crate::window_assignments::{
    WindowAssignments, WindowAssignmentsState, WindowGeometry, WindowRecord, MAIN_WINDOW_LABEL,
};

/// Unix-millis now. (Plain `std::time` — this is Rust, not a workflow script.)
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Set once the app is shutting down (main window closed, or `ExitRequested`).
/// While true, a pop-out window's `CloseRequested` is the app tearing it down —
/// NOT the user dismissing it — so [`handle_window_close`] must PRESERVE the
/// window record (Phase 2 restores it next boot) instead of removing it. A
/// user closing a pop-out while the app runs (flag false) still removes it.
static APP_QUITTING: AtomicBool = AtomicBool::new(false);

/// Mark the process as shutting down. Idempotent. Called from the `main.rs`
/// main-window close branch and `RunEvent::ExitRequested`.
pub fn mark_app_quitting() {
    APP_QUITTING.store(true, Ordering::SeqCst);
}

#[derive(Serialize, Clone)]
struct SessionAssignmentChanged {
    session_id: String,
    from: Option<String>,
    to: String,
}

/// Build (but do not position or show) the pop-out `WebviewWindow` for `label`.
/// Shared by the interactive open path ([`open_terminal_window`]) and the
/// boot-restore path ([`restore_pop_out_windows`]) so both produce a byte-
/// identical webview (same bundle, boot hint, port injection, headers). The
/// caller is responsible for positioning (placement vs saved geometry),
/// `show()`/`set_focus()`, and any events.
fn build_pop_out_webview(
    app: &tauri::AppHandle,
    label: &str,
) -> Result<tauri::WebviewWindow, String> {
    // Same embedded bundle; the boot hint makes it render Terminal-only and
    // adopt `term-N` as its window identity (via getCurrentWindow().label).
    let url = tauri::WebviewUrl::App(format!("index.html?view=terminal&window={}", label).into());

    let mut builder = tauri::WebviewWindowBuilder::new(app, label, url)
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

    builder
        .build()
        .map_err(|e| format!("Failed to build pop-out window {}: {}", label, e))
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

    let window = build_pop_out_webview(&app, &label)?;

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

/// Recreate persisted pop-out windows on boot (Phase 2 of the pop-out plan).
/// For every `term-N` record in the registry, build its `WebviewWindow` and
/// restore its saved geometry (position/size, or maximized). Returns the count
/// restored.
///
/// **Best-effort and non-fatal by contract** — invoked from the `main.rs` setup
/// closure AFTER the main window is up, so it must never block startup: a build
/// failure for one window is logged and skipped, and an already-open label
/// (idempotent re-entry) is left alone. It does NOT emit `window-opened`; each
/// restored window's frontend hydrates via `get_window_assignments` and replays
/// its terminals through the existing reconnect path when it loads.
pub fn restore_pop_out_windows(
    app: &tauri::AppHandle,
    assignments: &Arc<WindowAssignments>,
) -> usize {
    let mut restored = 0usize;
    for record in assignments.pop_out_records() {
        let label = record.label.clone();
        if app.get_webview_window(&label).is_some() {
            continue; // already open — don't double-build
        }
        let window = match build_pop_out_webview(app, &label) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(window = %label, error = %e, "restore_pop_out_windows: build failed — skipping");
                continue;
            }
        };
        if let Some(g) = record.geometry.as_ref() {
            let _ = window.set_position(tauri::PhysicalPosition::new(g.x, g.y));
            if g.maximized {
                let _ = window.maximize();
            } else {
                let _ = window.set_size(tauri::PhysicalSize::new(g.w, g.h));
            }
        }
        let _ = window.show();
        restored += 1;
        tracing::info!(window = %label, has_geometry = record.geometry.is_some(), "Restored pop-out terminal window");
    }
    if restored > 0 {
        tracing::info!(restored, "Restored pop-out terminal windows on boot");
    }
    restored
}

/// Snapshot every open pop-out window's current geometry into the registry
/// (persist-on-change). Called periodically and at shutdown so a restart —
/// including the operator's rebuild-and-kill path, which never runs the clean
/// quit handler — restores windows at their last position/size. Best-effort:
/// windows we can't read are skipped; `update_geometry` no-ops when unchanged.
pub fn capture_open_geometry(app: &tauri::AppHandle, assignments: &Arc<WindowAssignments>) {
    for record in assignments.pop_out_records() {
        let label = &record.label;
        let Some(window) = app.get_webview_window(label) else {
            continue;
        };
        let maximized = window.is_maximized().unwrap_or(false);
        // Outer position (top-left incl. decorations) + inner (content) size —
        // mirrors what the placement path writes, so restore round-trips.
        if let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) {
            let geom = WindowGeometry {
                x: pos.x,
                y: pos.y,
                w: size.width,
                h: size.height,
                maximized,
            };
            assignments.update_geometry(label, geom);
        }
    }
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
    // App shutting down: this close is teardown, not a user dismissing the
    // window. PRESERVE the record so Phase-2 restore recreates it next boot;
    // skip the reassignment/events (the process is going away). Still return
    // `true` so the caller skips the main-window app-quit cleanup.
    if APP_QUITTING.load(Ordering::SeqCst) {
        return true;
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
