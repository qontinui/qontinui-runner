//! Process-global `tauri::AppHandle` slot.
//!
//! Tauri commands receive `AppHandle` as a parameter, but background
//! tokio tasks (e.g. claim heartbeats from `agent_claims`) don't have
//! direct access. Set once during `tauri::Builder::setup()` and read
//! by any background task that needs to emit events to the webview.
//!
//! Plan reference: `D:/qontinui-root/plans/2026-05-18-agent-spawn-coordination.md`
//! Phase 3 — required for `agent_claims` heartbeat-task → webview
//! event emission.

use std::sync::OnceLock;

use tauri::AppHandle;

static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// Store the global `AppHandle`. Idempotent — first set wins.
/// Called from `main.rs::setup()`.
pub fn set(handle: AppHandle) {
    let _ = APP_HANDLE.set(handle);
}

/// Get the global `AppHandle` if it's been set. Returns `None` if
/// the runner is still starting up or running in a context with no
/// Tauri runtime (e.g., unit tests). Callers should silently drop
/// events when this returns `None`.
pub fn current() -> Option<AppHandle> {
    APP_HANDLE.get().cloned()
}
