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

/// True once a **deliberate** shutdown has been requested — the main window's
/// `CloseRequested` fired, or some other path explicitly asked the app to quit.
///
/// This is the runner's only record of *quit intent*, which is what separates a
/// real exit from an exit request manufactured by window teardown.
/// `webview_recovery::decide_exit_veto` reads it to tell the two apart; see the
/// veto rationale there. Note the ordering contract every quit path must honour:
/// call [`mark_app_quitting`] **before** `AppHandle::exit`, never after.
pub fn is_app_quitting() -> bool {
    APP_QUITTING.load(Ordering::SeqCst)
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
///
/// ⚠ **Blocking. Never call this directly from a tokio worker** — the
/// post-build [`crate::webview_recovery::verify_window_has_a_webview`] probe is
/// an unbounded event-loop round-trip off the main thread. `restore_pop_out_windows`
/// runs on the main thread (inline dispatch); [`open_terminal_window`] hands it
/// to `spawn_blocking`.
fn build_pop_out_webview(
    app: &tauri::AppHandle,
    label: &str,
    bound_page: Option<&str>,
) -> Result<tauri::WebviewWindow, String> {
    // Same embedded bundle; the boot hint makes it render Terminal-only and
    // adopt `term-N` as its window identity (via getCurrentWindow().label).
    // A page-bound pop-out also carries `&page=<id>` so the frontend pins
    // itself to that one terminal page (ignoring the shared active-page) and
    // renders only that page's grid.
    // Page ids are always `"default"` or a v4 UUID (see `useTerminalPages`
    // `addPage`), both URL-safe — no percent-encoding needed.
    let url = match bound_page {
        Some(page) => tauri::WebviewUrl::App(
            format!("index.html?view=terminal&window={}&page={}", label, page).into(),
        ),
        None => tauri::WebviewUrl::App(format!("index.html?view=terminal&window={}", label).into()),
    };

    let mut builder = tauri::WebviewWindowBuilder::new(app, label, url)
        .title(format!("Qontinui Terminal — {}", label))
        .inner_size(1000.0, 720.0)
        .min_inner_size(640.0, 400.0)
        .resizable(true)
        .decorations(true)
        .on_web_resource_request(crate::asset_headers::stamp_no_store_on_index);

    // Same WebView2 environment as this runner's main window — its isolated
    // user-data folder and its anti-throttling browser args. Without this
    // Tauri forces `%LOCALAPPDATA%\com.qontinui.runner` (the PRIMARY's profile
    // root) on the pop-out, and on a secondary runner WebView2 then fails with
    // `HRESULT(0x8007139F)`, leaving a window with no webview at all. See
    // `webview_recovery::apply_main_window_env_options`.
    builder = crate::webview_recovery::apply_main_window_env_options(builder);

    // Mirror the main window: inject the API port so the pop-out's frontend
    // addresses the runner's HTTP API (location.port is empty on tauri.localhost).
    let api_port = crate::mcp::types::get_mcp_api_port();
    builder = builder.initialization_script(format!("window.__QONTINUI_PORT__ = {};", api_port));

    let window = builder
        .build()
        .map_err(|e| format!("Failed to build pop-out window {}: {}", label, e))?;

    // `build()` returning `Ok` is NOT evidence of a webview — see
    // `webview_recovery::verify_window_has_a_webview`. Both callers treat a
    // negative here as a build failure: the interactive path persists no
    // record, the boot path skips and continues.
    crate::webview_recovery::verify_window_has_a_webview(&window, label)?;

    // A webview that is created and later DIES is a different failure from one
    // that was never created, and the probe above — a point-in-time check —
    // structurally cannot see it. Subscribe for the rest of this window's life.
    // Non-main role on purpose: a pop-out's death must never drive the MAIN
    // window's recovery ladder.
    crate::webview_recovery::attach_non_main_process_failed_handler(&window);

    Ok(window)
}

/// Open a new pop-out terminal window. Allocates the next `term-N` label,
/// records it, builds the `WebviewWindow` (loading the same bundle with a
/// `?view=terminal&window=term-N` boot hint), positions it via the reused
/// placement pipeline when a `placement` is supplied, and emits `window-opened`.
/// Returns the new window record.
///
/// **Nothing is persisted until the window's webview is proven.** Before Phase 3
/// of `2026-08-10-popout-webview2-creation-failure` this called
/// `WindowAssignments::create_window` *before* the build, so a failure left a
/// persisted `term-N` record behind — and a PAGE-BOUND record survives
/// `prune_empty_pop_outs` by design, so it was rebuilt (and failed again) at
/// every subsequent boot, forever. The label is now *reserved* across the build
/// (which keeps allocation atomic against a concurrent open) and the record is
/// committed only after [`build_pop_out_webview`] — build **and** its post-build
/// webview probe — succeeds.
///
/// # Why the build runs on a blocking thread
///
/// This is an `async fn` command, so its body runs on a tokio **worker**, and
/// [`build_pop_out_webview`]'s post-build probe is a `Message::Window` getter
/// with an unbounded `rx.recv()` (see
/// [`crate::webview_recovery::verify_window_has_a_webview`]). On a cold WebView2
/// profile the event loop is busy inside
/// `CreateCoreWebView2EnvironmentWithOptions` for **seconds**, and a wedged
/// event loop never answers at all — either would occupy a worker for the whole
/// wait. `spawn_blocking` puts that on a thread whose job is to block. A
/// timeout was deliberately not used instead: a slow-but-healthy cold-profile
/// build would trip it and a healthy pop-out would be reported as a failure.
#[tauri::command]
pub async fn open_terminal_window(
    app: tauri::AppHandle,
    assignments: tauri::State<'_, Arc<WindowAssignments>>,
    placement: Option<SpawnPlacement>,
    bound_page: Option<String>,
) -> Result<WindowRecord, String> {
    // Skip any label `tauri` still holds a window for. Records and the
    // windowing registry can disagree (a pop-out closed without being
    // destroyed leaves a live webview and no record), and it is the registry —
    // not the record — that `build()` collides with.
    let label = {
        let app = app.clone();
        assignments.reserve_popout_label_where(move |l| app.get_webview_window(l).is_some())
    };

    let built = {
        let app = app.clone();
        let label_for_build = label.clone();
        let bound_page = bound_page.clone();
        tauri::async_runtime::spawn_blocking(move || {
            build_pop_out_webview(&app, &label_for_build, bound_page.as_deref())
        })
        .await
        .map_err(|e| format!("pop-out build task for {} panicked: {}", label, e))?
    };

    let window = match built {
        Ok(w) => w,
        Err(e) => {
            // No record was written, so there is nothing to prune and nothing
            // survives to be retried at the next boot — which is the whole
            // point. The label stays RESERVED (burned) rather than being handed
            // back: `build()` returning `Ok` already put a webview-less window
            // into Tauri's own registry, which nothing ever removes, so
            // rebuilding this label would fail with
            // `Error::WebviewLabelAlreadyExists` forever. See
            // `WindowAssignments::reserved_labels`.
            //
            // Loud, and loud in the two places that can act on it: this
            // `error!` for the operator reading the log, and the `Err` return
            // for the frontend that asked for the window. Deliberately NOT
            // `ui_error` — see the "no backend writer" section of
            // `crate::ui_error`: that field is a process-lifetime latch meaning
            // "the MAIN window's React tree crashed", and writing a pop-out
            // failure into it would take this otherwise-healthy runner out of
            // the fleet's dispatch pool permanently.
            tracing::error!(
                window = %label,
                error = %e,
                "open_terminal_window: pop-out has no webview — no window record created"
            );
            return Err(e);
        }
    };

    // The webview is real: commit the reserved label as a persisted record.
    let record =
        assignments.create_reserved_window(label.clone(), None, None, bound_page.clone(), now_ms());

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
    // every PER-ID `term-N` record is genuinely empty (no live tab can claim
    // it). Restoring those just produces empty, un-closable windows whose
    // monotonic `term-N` counter keeps climbing (the observed orphan-
    // accumulation loop, `term-19`). So FIRST clear the stale owner map, THEN
    // prune the now-empty pop-out records, so we neither restore them nor
    // resurrect them next boot.
    //
    // PAGE-BOUND pop-outs are the exception and are deliberately PRESERVED by
    // `prune_empty_pop_outs` (they claim terminals by stable `page_id`, not the
    // cleared owner map). They fall through to the restore loop below and are
    // rebuilt with their `&page=` boot hint; the page's sessions re-attach there
    // via the pinned page's normal restore path.
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
        let window = match build_pop_out_webview(app, &label, record.bound_page.as_deref()) {
            Ok(w) => w,
            Err(e) => {
                // Boot must NOT abort on one bad window — skip and continue, as
                // this arm always did. What changed in Phase 3 is that there is
                // finally an `Err` to catch: the post-build webview probe in
                // `build_pop_out_webview` turns the silent
                // `failed to create webview` class into one. `error!` rather
                // than the old `warn!`, because this is now a diagnosed failure
                // rather than a generic builder complaint — and it is the only
                // sink this path has: a restore failure is deliberately not
                // written to `ui_error` (see the "no backend writer" section of
                // `crate::ui_error`), which would latch the whole runner
                // `errored` for the rest of the process's life over one
                // pop-out.
                tracing::error!(window = %label, error = %e, "restore_pop_out_windows: pop-out has no webview — skipping");
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
/// forces the teardown synchronously. `destroy()` fires NO events (tauri
/// 2.11.1: "does not emit any events"), so the central `CloseRequested` handler
/// does NOT run on this path — which is why the records are pruned explicitly
/// below rather than left to it. Returns the labels closed/pruned. Never
/// touches `"main"`.
///
/// Note this sweep deliberately skips PAGE-BOUND pop-outs, so it is not a
/// backstop for their records; [`close_terminal_window`] owns that teardown.
pub fn sweep_empty_pop_out_windows(
    app: &tauri::AppHandle,
    assignments: &Arc<WindowAssignments>,
) -> Vec<String> {
    let mut swept: Vec<String> = Vec::new();

    // Pass 0 — DEAD RECORDS. A pop-out record whose OS window no longer exists
    // can host nothing, and while it survives it actively strands state: a
    // page-bound record keeps the frontend's `pageId → windowLabel` mirror
    // hiding that page in every live window, which is how the grid ended up
    // rendering zero zones permanently (see [`close_terminal_window`]). This
    // sweep is operator-initiated (`close_empty_terminal_windows`), never
    // automatic, so it cannot race boot-restore recreating those windows — and
    // it is the recovery affordance for a runner already carrying a leaked
    // record.
    //
    // Runs BEFORE the emptiness pass, and deliberately ignores the
    // `bound_page` carve-out below: that carve-out protects a LIVE page-bound
    // window from being swept for looking empty, which cannot apply to a window
    // that is gone.
    for record in assignments.pop_out_records() {
        let label = &record.label;
        if app.get_webview_window(label).is_some() {
            continue;
        }
        let close = assignments.close_window(label);
        if close.removed || !close.reassigned.is_empty() {
            tracing::info!(
                window = %label,
                bound_page = ?record.bound_page,
                reassigned = close.reassigned.len(),
                "sweep_empty_pop_out_windows: pruned record for a window that no longer exists",
            );
            swept.push(label.clone());
        }
    }

    // Pass 1 — LIVE but empty pop-outs.
    for record in assignments.pop_out_records() {
        let label = &record.label;
        if assignments.has_assigned_sessions(label) || record.bound_page.is_some() {
            // Still hosts a tab, OR is a page-bound window (claims terminals by
            // page_id, not the session_owner map) — leave it.
            continue;
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
/// and prunes its record, AND prunes the record of any pop-out whose OS window
/// is already gone (including page-bound ones). The latter is the recovery
/// affordance for a leaked record: while one survives, the page it was bound to
/// stays hidden in every live window and the zone grid renders zero zones.
/// Returns the labels swept (for an optional toast).
#[tauri::command]
pub async fn close_empty_terminal_windows(
    app: tauri::AppHandle,
    assignments: tauri::State<'_, Arc<WindowAssignments>>,
) -> Result<Vec<String>, String> {
    Ok(sweep_empty_pop_out_windows(&app, assignments.inner()))
}

/// Whether `owner` (a non-main window) should be torn down now that one of its
/// sessions has exited cleanly. Pure — no `AppHandle`, so it is unit-testable.
fn should_close_owner_window(assignments: &WindowAssignments, owner: &str) -> bool {
    !assignments.has_assigned_sessions(owner) && !assignments.is_page_bound(owner)
}

/// P1 (close-on-clean-exit) — invoked from the terminal session's PTY-exit
/// waiter the instant a session's child exits. On a CLEAN exit (`Some(0)`)
/// only, if the session's owning window is a `term-N` pop-out that now hosts NO
/// other live session, close that window (the predictable terminal-emulator
/// close-on-exit behaviour) and prune its record so boot-restore won't
/// resurrect it. A non-zero / unknown exit NEVER auto-closes (honesty: a failed
/// session stays visible). Multi-tab pop-outs are never closed by one tab's exit
/// (the emptiness check sees the sibling tabs).
///
/// Two window kinds are NEVER auto-closed: `"main"`, and PAGE-BOUND pop-outs.
/// A page-bound window claims its terminals by `page_id`, not through the
/// `session_owner` map, so an empty `session_owner` map does NOT mean an empty
/// window — it hosts a whole page that outlives any single session, and even its
/// last tab exiting must leave the window (and the operator's grid layout)
/// standing. Only the OS title-bar close dismisses one. See
/// [`should_close_owner_window`], and the same carve-out in
/// [`sweep_empty_pop_out_windows`] / `WindowAssignments::prune_empty_pop_outs`.
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
    if !should_close_owner_window(assignments, &owner) {
        // Another live tab remains in the pop-out, OR it is a page-bound window
        // (its tabs are claimed by page_id, not the session_owner map, so an
        // empty owner map does NOT mean an empty window). Leave it open.
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

/// Close a pop-out terminal window.
///
/// We use `destroy()` rather than `close()` because `close()` only REQUESTS a
/// close that WebView2 may never finish, leaving the pop-out visible on screen
/// (observed live: `EnumWindows` still reported it `[vis]` after a successful
/// `close()`).
///
/// `destroy()` forces the teardown but — per its own tauri docs, "Similar to
/// `close` but **does not emit any events** and force close the window instead"
/// (tauri 2.11.1) — it does NOT fire `CloseRequested`. So the central
/// `on_window_event` handler never runs on this path, and this function must
/// invoke [`handle_window_close`] itself. It previously did not, and the
/// consequences were severe and PERSISTENT (measured live on the runner
/// 2026-08-20):
///
///  - the window's [`WindowRecord`] — including its `bound_page` — survived in
///    `WindowAssignments` forever, and is PERSISTED, so it outlived a runner
///    restart too;
///  - the frontend derives its `pageId → windowLabel` mirror from exactly that
///    registry, so the main window went on hiding a page bound to a window that
///    no longer exists. With every page hidden, `useTerminalPages` minted a
///    fresh empty one, and every terminal created afterwards landed on a page
///    the main window refused to render: **the zone grid rendered zero zones,
///    permanently, and neither a refresh, a navigation nor a tab switch
///    recovered** (the P1 in the 2026-08-20 manual-test loop);
///  - sessions the window owned were never reassigned to `"main"`, so their
///    tabs were orphaned in every live window.
///
/// The sweep in [`sweep_empty_pop_out_windows`] does not cover this: it
/// deliberately SKIPS page-bound pop-outs, which is exactly the case that
/// strands a page.
///
/// [`handle_window_close`] is idempotent (a second call finds no record and no
/// sessions to reassign), so the OS-close path — which still goes through
/// `CloseRequested` — is unaffected, and a record left behind by an EARLIER
/// destroy is healed by calling this on a window that is already gone.
///
/// Closing `"main"` is rejected.
#[tauri::command]
pub async fn close_terminal_window(app: tauri::AppHandle, label: String) -> Result<(), String> {
    if label == MAIN_WINDOW_LABEL {
        return Err("refusing to close the main window via close_terminal_window".to_string());
    }
    if let Some(win) = app.get_webview_window(&label) {
        win.destroy()
            .map_err(|e| format!("Failed to destroy window {}: {}", label, e))?;
    }
    // Registry teardown + `window-closed` / `session-assignment-changed`
    // events. Runs whether or not a live window was found: the `None` arm is
    // "already gone", which is precisely the state a previous `destroy()` left
    // a stale record in.
    handle_window_close(&app, &label);
    Ok(())
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

    // Force the window down. The record is gone as of `close_window` above, so
    // anything less than destruction leaves the two views of this pop-out
    // permanently disagreeing.
    //
    // Not belt-and-braces — on the X-button path it is the ONLY thing that
    // closes the window. A pop-out's React tree registers `onCloseRequested`
    // (`useTerminalInitialization.ts`), and `tauri` turns the mere existence of
    // such a JS listener into an automatic `api.prevent_close()`
    // (`tauri-2.11.1/src/manager/window.rs`, `WindowEvent::CloseRequested`).
    // `tauri-runtime-wry`'s `on_close_requested` then reads that prevent signal
    // with a `try_recv` and skips `on_window_close`, so the tao `Window` is
    // never dropped: the HWND, the webview and tauri's registry entry for this
    // label all survive a close the user experienced as final. The next
    // `open_terminal_window` re-derived the same `term-N` (records are what
    // `next_popout_label` counts, and this one's record had just been removed)
    // and its `build()` failed with `Error::WebviewLabelAlreadyExists`.
    //
    // `destroy()` bypasses `CloseRequested` entirely — it posts
    // `WindowMessage::Destroy` to the event loop, which drops the tao `Window`
    // and produces a real `Destroyed`, on which `tauri`'s
    // `AppManager::on_window_close` finally releases the label.
    //
    // Idempotent, which is what lets it live here rather than only in the
    // event handler: `close_terminal_window` destroys first and then calls
    // this, and a second `Destroy` for an already-removed window is a no-op
    // inside the runtime. Deliberately AFTER the app-quitting early return
    // above — during teardown the process is going away and the records are
    // being preserved for the next boot's restore.
    if let Some(win) = app.get_webview_window(label) {
        if let Err(e) = win.destroy() {
            tracing::warn!(
                window = %label,
                error = %e,
                "handle_window_close: destroy failed — the label stays held by a live webview; \
                 the next pop-out open will skip past it"
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webview_recovery::{webview_env_options, MainWindowSpec, MAIN_WINDOW_BROWSER_ARGS};
    use crate::window_placement::WindowPlacement;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// A `MainWindowSpec` differing only in the field under test.
    ///
    /// `MAIN_WINDOW_SPEC` is a write-once `OnceLock` with no setter, so the
    /// tests build the spec directly (all fields are `pub`) and assert on the
    /// pure selection helper — mirroring `instance.rs`'s
    /// `primary_keeps_the_unscoped_path`: assert on the pure core, never
    /// through the process env, because sibling harness threads mutate it.
    fn spec_with(data_dir: Option<PathBuf>) -> MainWindowSpec {
        MainWindowSpec {
            data_dir,
            initial_size: (1600.0, 1000.0),
            decorations: true,
            placement: WindowPlacement::Maximized,
            is_secondary: false,
        }
    }

    /// The whole selection, in one table.
    ///
    /// `webview_env_options` is a small function and these are its only
    /// interesting inputs, so three near-identical `assert_eq!(opts.data_dir,
    /// Some(dir))` tests were three spellings of "the field is copied". What
    /// is worth pinning is that the copy happens for **every** shape of
    /// `data_dir` — including the primary's, which is the one that CHANGES
    /// behaviour — and that the browser args ride along in all of them (D2:
    /// same folder with differing `additionalBrowserArguments` is the one
    /// configuration WebView2 is documented inconsistently on across runtime
    /// versions).
    ///
    /// Rows, and what each is about:
    ///
    /// * **Secondary** — the defect this plan is about. A `term-N` pop-out set
    ///   no `data_directory`, so Tauri forced it onto
    ///   `%LOCALAPPDATA%\com.qontinui.runner` — the **primary's** profile root —
    ///   and WebView2 failed with `HRESULT(0x8007139F)`.
    /// * **Primary** — stated honestly: this makes the primary's pop-out
    ///   consistent with the primary's own main window, it does NOT leave it
    ///   where it is today (today it lands on `…\com.qontinui.runner`, profile
    ///   `…\EBWebView`). A deliberate behaviour change, covered in the plan's
    ///   Risks, effective only on the primary's next operator-driven restart.
    /// * **No data dir** — off-Windows `instance::webview2_data_dir` returns
    ///   `None`, so nothing may be applied and the propagation is a no-op there.
    #[test]
    fn every_non_main_webview_inherits_its_own_main_windows_environment() {
        let cases: [(&str, Option<PathBuf>); 3] = [
            (
                "secondary — its own isolated folder, never the primary's root",
                Some(PathBuf::from(
                    r"C:\Users\x\AppData\Local\com.qontinui.runner\EBWebView-test-19fed73e1a7-6",
                )),
            ),
            (
                "primary — its own main window's folder",
                Some(PathBuf::from(
                    r"C:\Users\x\AppData\Local\com.qontinui.runner\EBWebView",
                )),
            ),
            ("off-Windows / default profile — nothing to apply", None),
        ];

        for (why, data_dir) in cases {
            let opts = webview_env_options(Some(&spec_with(data_dir.clone())));
            assert_eq!(
                opts.data_dir, data_dir,
                "{why}: a non-main webview must land on exactly the folder its own main \
                 window uses, not on Tauri's forced %LOCALAPPDATA%\\<identifier> default"
            );
            assert_eq!(
                opts.browser_args,
                Some(MAIN_WINDOW_BROWSER_ARGS),
                "{why}: the shared constant must be reused verbatim — a hand-rolled second \
                 string silently drops wry's msWebOOUI/msPdfOOUI/msSmartScreenProtection \
                 defaults, which setting additional_browser_args REPLACES"
            );
        }
    }

    /// Server mode / no main window ever built: `MAIN_WINDOW_SPEC` is an unset
    /// `OnceLock`, so `main_window_spec()` is `None`. Apply nothing, and above
    /// all do not panic — the pop-out path is unreachable in server mode, but
    /// the fix must survive being reached.
    #[test]
    fn no_main_window_yields_no_environment_options() {
        let opts = webview_env_options(None);

        assert_eq!(opts.data_dir, None);
        assert_eq!(
            opts.browser_args, None,
            "with no recorded main window there is no option source to mirror"
        );
    }

    fn store() -> (tempfile::TempDir, WindowAssignments) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("window-assignments.json");
        let wa = WindowAssignments::open(&path).unwrap();
        (dir, wa)
    }

    /// Reserve + commit in one step, for the close/prune tests that only need a
    /// pop-out record to exist. `WindowAssignments::create_window` used to spell
    /// this in production; Phase 3 left it with no production callers (a record
    /// must not be persisted until its window's webview is proven), so it was
    /// deleted rather than kept as a public API nothing ships.
    fn create_window(
        wa: &WindowAssignments,
        title: Option<String>,
        geometry: Option<WindowGeometry>,
        bound_page: Option<String>,
        now_ms: i64,
    ) -> WindowRecord {
        let label = wa.reserve_popout_label();
        wa.create_reserved_window(label, title, geometry, bound_page, now_ms)
    }

    #[test]
    fn page_bound_popout_is_never_closed_even_when_its_last_session_exits() {
        // Regression: a page-bound pop-out claims its tabs by `page_id`, not
        // through `session_owner`, so an empty owner map does NOT mean an empty
        // window. Before the guard this returned `true` and destroyed the
        // operator's window (and grid layout) on a clean `exit`.
        let (_d, wa) = store();
        wa.ensure_main(1);
        let bound = create_window(&wa, None, None, Some("page-A".to_string()), 10);
        wa.assign_session("sess-A", &bound.label);
        // The exit waiter reassigns the dead session to main immediately before
        // consulting the guard, leaving the owner map empty.
        wa.assign_session("sess-A", MAIN_WINDOW_LABEL);
        assert!(!wa.has_assigned_sessions(&bound.label));

        assert!(
            !should_close_owner_window(&wa, &bound.label),
            "a page-bound pop-out stays open even with no assigned sessions"
        );
    }

    #[test]
    fn plain_popout_still_closes_when_its_last_session_exits() {
        // The fix must not disable close-on-clean-exit for ordinary pop-outs —
        // that is the behaviour the helper exists for.
        let (_d, wa) = store();
        wa.ensure_main(1);
        let w = create_window(&wa, None, None, None, 10);
        wa.assign_session("sess-A", &w.label);
        wa.assign_session("sess-A", MAIN_WINDOW_LABEL); // the exit reassign

        assert!(
            should_close_owner_window(&wa, &w.label),
            "an emptied per-id pop-out is still torn down on clean exit"
        );
    }

    // ── Phase 3: a failed pop-out build must leave nothing behind ─────────

    /// Why the orphan mattered, and why "it gets pruned anyway" is false.
    /// A PAGE-BOUND record is deliberately preserved by the boot orphan sweep
    /// (`clear_session_owners` + `prune_empty_pop_outs`), so under the pre-fix
    /// ordering a webview-less page-bound pop-out was rebuilt — and failed
    /// again — at **every** subsequent boot, forever.
    #[test]
    fn the_pre_fix_ordering_left_a_page_bound_orphan_that_boot_retried_forever() {
        let (_d, wa) = store();
        wa.ensure_main(1);

        // The ordering Phase 3 removed, composed explicitly because the
        // one-step `create_window` that used to spell it no longer exists in
        // production (F7): the record exists BEFORE the build.
        let orphan_label = wa.reserve_popout_label();
        let orphan =
            wa.create_reserved_window(orphan_label, None, None, Some("page-A".to_string()), 10);
        // …the build then fails and nothing removes the record.

        // Replay the boot orphan sweep exactly as `restore_pop_out_windows` does.
        wa.clear_session_owners();
        let pruned = wa.prune_empty_pop_outs();

        assert!(
            !pruned.contains(&orphan.label),
            "page-bound records survive the sweep by design — that is the carve-out"
        );
        assert!(
            wa.pop_out_records().iter().any(|r| r.label == orphan.label),
            "so the pre-fix ordering handed boot-restore a webview-less window to retry forever"
        );

        // And the post-fix ordering never produces this state at all — see
        // `a_label_burned_by_a_failed_build_is_not_reused_by_the_retry`, which
        // asserts the same emptiness against a sequence that actually contains
        // a failed attempt.
    }

    /// The reservation keeps label allocation atomic even though the record is
    /// now committed after the build. Two opens in flight at once must not
    /// derive the same `term-N` — which a plain read-only "peek" would.
    #[test]
    fn two_in_flight_opens_never_reserve_the_same_label() {
        let (_d, wa) = store();
        wa.ensure_main(1);

        let a = wa.reserve_popout_label();
        let b = wa.reserve_popout_label();

        assert_eq!((a.as_str(), b.as_str()), ("term-1", "term-2"));
    }

    /// A committed reservation stops occupying the counter, so a normal
    /// open/open/open sequence still yields `term-1`, `term-2`, `term-3` —
    /// the reservation set is not a leak.
    #[test]
    fn committing_a_reservation_advances_the_counter_by_exactly_one() {
        let (_d, wa) = store();
        wa.ensure_main(1);

        let first = wa.reserve_popout_label();
        let record = wa.create_reserved_window(first.clone(), None, None, None, 10);
        assert_eq!(record.label, first);
        assert_eq!(record.label, "term-1");

        let second = wa.reserve_popout_label();
        assert_eq!(second, "term-2");
        wa.create_reserved_window(second, None, None, None, 20);

        assert_eq!(wa.reserve_popout_label(), "term-3");
    }

    /// A label burned by a failed build is **never** handed back. `build()`
    /// returning `Ok` already put a webview-less window into Tauri's own
    /// registry (`tauri` 2.11.1 `src/manager/webview.rs`, `attach_webview`),
    /// which nothing removes because no wry-side window exists to raise a
    /// `Destroyed` event — so rebuilding that label would fail with
    /// `Error::WebviewLabelAlreadyExists` (`src/manager/webview.rs:437`) for
    /// the rest of the process's life. The retry must get a fresh label.
    #[test]
    fn a_label_burned_by_a_failed_build_is_not_reused_by_the_retry() {
        let (_d, wa) = store();
        wa.ensure_main(1);

        let failed = wa.reserve_popout_label(); // build fails; never committed
        let retry = wa.reserve_popout_label();

        assert_ne!(
            failed, retry,
            "reusing the burned label would hit WebviewLabelAlreadyExists forever"
        );
        assert_eq!(retry, "term-2");
        assert!(
            wa.pop_out_records().is_empty(),
            "and still nothing is persisted for either attempt"
        );
    }

    /// The error must name the cause, not the symptom. The getter failing
    /// reads like a stalled window; what actually happened is that no webview
    /// was ever created, and wry's own `failed to create webview` log line is
    /// the proof.
    #[test]
    fn the_no_webview_error_names_the_real_cause() {
        let msg = crate::webview_recovery::no_webview_error("term-3", "failed to receive message");

        assert!(msg.contains("term-3"), "names the window: {msg}");
        assert!(msg.contains("no webview"), "names the cause: {msg}");
        assert!(
            msg.contains("failed to create webview"),
            "points at the log line that IS the failure: {msg}"
        );
    }

    /// **A pop-out failure must NOT make the runner report `errored`.**
    ///
    /// The inverse of what the first cut of this code asserted, and the
    /// correction is the point. That version routed a webview-less pop-out into
    /// `ui_error`, which is a **process-lifetime latch** (its only clearer is
    /// the React boundary's own `componentDidUpdate` transition, unreachable for
    /// an error the boundary never raised) that every consumer reads as "the
    /// MAIN window's React tree crashed": `/health` `derived_status: errored`,
    /// `frontendReady: false`, qontinui-web's dispatcher 503ing
    /// `runner_unhealthy`, the runner gone from every picker. One failed pop-out
    /// would have taken an otherwise-healthy runner out of the fleet's dispatch
    /// pool for good — breaking the very UI-Bridge readiness this plan exists to
    /// restore. Full reasoning: the "no backend writer" section of
    /// `crate::ui_error`.
    ///
    /// The flag is **derived from the state**, never passed as a literal — the
    /// test it replaces hard-coded both `has_ui_error` arguments, so it held
    /// whether or not the code under test did anything at all.
    #[tokio::test]
    async fn a_failed_pop_out_leaves_the_runner_reporting_healthy() {
        let state = crate::ui_error::UiErrorState::new();

        let status = |has_ui_error: bool| {
            crate::ui_error::compute_derived_status(&crate::ui_error::HealthInputs {
                has_ui_error,
                // `native_ui_wedged` is left at its `None` default: this
                // pop-out test does not probe the native message loop.
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            })
        };

        assert_eq!(status(state.get().await.is_some()), "healthy", "control");

        // NOT an end-to-end pop-out exercise — no window is built here and no
        // line of `build_pop_out_webview` runs; that needs a Tauri app harness
        // this module deliberately does not have. What is covered is the two
        // halves that decide the fleet-visible outcome: the message a hollow
        // build produces (`no_webview_error` — the exact string
        // `build_pop_out_webview` returns and logs) is loud enough to diagnose
        // from, and `UiErrorState` is untouched, so `derived_status` stays
        // `healthy`. That the builder actually returns THIS message on a hollow
        // build is held by `webview_recovery::verify_window_has_a_webview` and
        // by the source-level probe guard, not by this test.
        let err = crate::webview_recovery::no_webview_error("term-1", "failed to receive message");
        assert!(err.contains("failed to create webview"), "loud: {err}");

        assert!(
            state.get().await.is_none(),
            "no backend path may write ui_error — `ui_error_has_exactly_one_writer` \
             guards this at the source level"
        );
        assert_eq!(
            status(state.get().await.is_some()),
            "healthy",
            "one dead pop-out must not remove an otherwise-healthy runner from the fleet"
        );

        // And the latch really is one-way, which is why it must stay the React
        // boundary's: once set, only `clear_ui_error` (a #[tauri::command] the
        // boundary invokes) empties it.
        state
            .report("the React tree crashed".into(), None, None, None)
            .await;
        assert_eq!(status(state.get().await.is_some()), "errored");
    }

    #[test]
    fn popout_with_a_surviving_sibling_session_stays_open() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let w = create_window(&wa, None, None, None, 10);
        wa.assign_session("sess-A", &w.label);
        wa.assign_session("sess-B", &w.label);
        wa.assign_session("sess-A", MAIN_WINDOW_LABEL); // sess-A exited

        assert!(
            !should_close_owner_window(&wa, &w.label),
            "sess-B still renders here, so one tab's exit must not close it"
        );
    }
}
