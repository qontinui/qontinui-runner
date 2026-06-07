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
    // P2 (orphan sweep): a persisted pop-out's PTYs never survive the process
    // restart — terminal ids are a fresh uuid per launch and are never
    // recreated — so EVERY persisted `session_owner` entry is stale on boot and
    // every `term-N` record is genuinely empty (no live tab can claim it).
    // Restoring them just produces empty, un-closable windows whose monotonic
    // `term-N` counter keeps climbing (the observed orphan-accumulation loop,
    // `term-19`). So FIRST clear the stale owner map, THEN prune the now-empty
    // pop-out records, so we neither restore them nor resurrect them next boot.
    let cleared = assignments.clear_session_owners();
    let pruned = assignments.prune_empty_pop_outs();
    if cleared > 0 || !pruned.is_empty() {
        tracing::info!(
            cleared_owners = cleared,
            pruned = pruned.len(),
            labels = ?pruned,
            "restore_pop_out_windows: cleared stale owners + pruned empty pop-out records (boot orphan sweep)"
        );
    }

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

/// P2 (orphan sweep) — close every OPEN pop-out (`term-N`) window that owns no
/// assigned session, and prune any session-less pop-out records. Shared by the
/// boot path's intent and the operator-initiated "close empty terminal windows"
/// affordance ([`close_empty_terminal_windows`]) so both behave identically.
///
/// A pop-out is "empty" when [`WindowAssignments::has_assigned_sessions`] is
/// false for its label — no tab renders there. Programmatic teardown routes
/// through `WebviewWindow::destroy()` (NOT `close()`): on Windows/WebView2 a
/// `close()` only requests a close that the event loop may never finish
/// destroying, leaving the OS window VISIBLE (observed live — `EnumWindows`
/// still reports the window `[vis]` after `close()` returned `ok`). `destroy()`
/// forces the teardown synchronously. The central `CloseRequested` handler still
/// fires (so the usual reassign-to-main + `window-closed` path runs — a no-op
/// reassign when empty), then we prune the now-dead records. Returns the labels
/// closed/pruned. Never touches `"main"`.
pub fn sweep_empty_pop_out_windows(
    app: &tauri::AppHandle,
    assignments: &Arc<WindowAssignments>,
) -> Vec<String> {
    let mut swept: Vec<String> = Vec::new();
    for record in assignments.pop_out_records() {
        let label = &record.label;
        if assignments.has_assigned_sessions(label) {
            continue; // still hosts a tab — leave it
        }
        // Destroy (not close) the live OS window if one is open (best-effort):
        // close() leaves WebView2 pop-outs visible; destroy() forces teardown.
        // The record is pruned below regardless, so a record with no live
        // window is still cleaned up.
        if let Some(win) = app.get_webview_window(label) {
            if let Err(e) = win.destroy() {
                tracing::warn!(window = %label, error = %e, "sweep_empty_pop_out_windows: destroy failed");
            }
        }
        swept.push(label.clone());
    }
    let pruned = assignments.prune_empty_pop_outs();
    if !pruned.is_empty() {
        tracing::info!(count = pruned.len(), labels = ?pruned, "sweep_empty_pop_out_windows: pruned empty pop-out records");
    }
    // Union (a record may be pruned without a live window, or closed without
    // having been in pop_out_records by the time prune ran) — de-dup by set.
    let mut set: std::collections::BTreeSet<String> = swept.into_iter().collect();
    set.extend(pruned);
    set.into_iter().collect()
}

/// Operator-initiated "close empty terminal windows" affordance (P2). Reuses
/// [`sweep_empty_pop_out_windows`] — closes every pop-out that owns no live tab
/// and prunes its record. Returns the labels swept (for an optional toast).
#[tauri::command]
pub async fn close_empty_terminal_windows(
    app: tauri::AppHandle,
    assignments: tauri::State<'_, Arc<WindowAssignments>>,
) -> Result<Vec<String>, String> {
    Ok(sweep_empty_pop_out_windows(&app, assignments.inner()))
}

/// P1 (close-on-clean-exit) — invoked from the terminal session's PTY-exit
/// waiter the instant a session's child exits. On a CLEAN exit (`Some(0)`)
/// only, if the session's owning window is a `term-N` pop-out that now hosts NO
/// other live session, close that window (the predictable terminal-emulator
/// close-on-exit behaviour) and prune its record so boot-restore won't
/// resurrect it. A non-zero / unknown exit NEVER auto-closes (honesty: a failed
/// session stays visible). `"main"` is never auto-closed. Multi-tab pop-outs are
/// never closed by one tab's exit (the emptiness check sees the sibling tabs).
///
/// `terminal_id` is the session id used as the `window_assignments` owner key
/// (the same id the frontend passes to `assign_session_to_window`).
///
/// Best-effort + cheap: a non-clean exit returns immediately; an owner of
/// `"main"` (the common case — most sessions are docked) returns immediately.
pub fn auto_close_owner_window_if_empty(
    app: &tauri::AppHandle,
    assignments: &Arc<WindowAssignments>,
    terminal_id: &str,
    exit_code: Option<i32>,
) {
    // Honesty: only a clean exit auto-closes. Non-zero / None stays open.
    if exit_code != Some(0) {
        return;
    }
    let owner = assignments.owner_of(terminal_id);
    if owner == MAIN_WINDOW_LABEL {
        return; // docked session, or main — never auto-close
    }
    // Drop this session's ownership first (its PTY just died), then test the
    // window for remaining tabs. `assign_session(.., "main")` removes the
    // explicit entry; emit the reassignment so any window views stay in sync.
    if let Some(from) = assignments.assign_session(terminal_id, MAIN_WINDOW_LABEL) {
        let payload = SessionAssignmentChanged {
            session_id: terminal_id.to_string(),
            from: Some(from),
            to: MAIN_WINDOW_LABEL.to_string(),
        };
        if let Err(e) = app.emit("session-assignment-changed", &payload) {
            tracing::warn!(error = %e, "auto_close_owner_window_if_empty: failed to emit reassignment");
        }
    }
    if assignments.has_assigned_sessions(&owner) {
        // Another live tab remains in the pop-out — leave it open.
        return;
    }
    // The owner pop-out is now empty: destroy the OS window and prune the
    // record so the boot-restore loop won't resurrect it. We destroy() rather
    // than close() because close() only REQUESTS a close that WebView2 may never
    // finish, leaving the pop-out visible on screen (observed live: `EnumWindows`
    // still reported it `[vis]` after a successful close()). destroy() forces the
    // teardown; the central `CloseRequested` handler still fires (and emits
    // `window-closed`) before the window is gone.
    if let Some(win) = app.get_webview_window(&owner) {
        if let Err(e) = win.destroy() {
            tracing::warn!(window = %owner, error = %e, "auto_close_owner_window_if_empty: destroy failed");
        }
    }
    let pruned = assignments.prune_empty_pop_outs();
    tracing::info!(
        window = %owner,
        terminal_id = %terminal_id,
        pruned = ?pruned,
        "auto-closed empty pop-out on clean session exit"
    );
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
/// button takes the same path); here we force the teardown. We use `destroy()`
/// rather than `close()` because `close()` only requests a close that WebView2
/// may never finish, leaving the pop-out visible on screen (observed live:
/// `EnumWindows` still reported it `[vis]` after a successful `close()`).
/// `destroy()` forces the teardown while still firing `CloseRequested` (so the
/// central handler emits `window-closed`). Closing `"main"` is rejected.
#[tauri::command]
pub async fn close_terminal_window(app: tauri::AppHandle, label: String) -> Result<(), String> {
    if label == MAIN_WINDOW_LABEL {
        return Err("refusing to close the main window via close_terminal_window".to_string());
    }
    match app.get_webview_window(&label) {
        Some(win) => win
            .destroy()
            .map_err(|e| format!("Failed to destroy window {}: {}", label, e)),
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
