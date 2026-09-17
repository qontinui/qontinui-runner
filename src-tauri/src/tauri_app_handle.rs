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

/// Set once the Tauri runtime a VISIBLE continuation needs is fully in place:
/// the `AppHandle` above AND the managed `SessionRegistry` a terminal session
/// registers into. The handle alone is not enough — `.setup()` stores it
/// before it manages the registry, and a dispatch that lands between the two
/// fails with `SessionRegistry state not managed`.
///
/// Plan
/// `2026-09-17-a-gate-continuation-delivered-before-the-runner-finishes-booting-is-consumed-as-spawn-failed-and-never-retried`:
/// the continuation runtime starts before `.setup()`, so a boot delivers the
/// pending backlog while this is still unset. `agent_runtime` defers such a
/// dispatch UNCLAIMED instead of claiming it and recording `spawn_failed`.
static RUNTIME_READY: OnceLock<()> = OnceLock::new();

/// Mark the visible-terminal runtime ready. Idempotent. Called from
/// `main.rs::setup()` immediately after `SessionRegistry` is managed.
pub fn mark_runtime_ready() {
    let _ = RUNTIME_READY.set(());
}

/// Whether [`mark_runtime_ready`] has run in this process.
pub fn runtime_ready() -> bool {
    RUNTIME_READY.get().is_some()
}
