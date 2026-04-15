//! Bridge Helper Functions
//!
//! Provides synchronous wrappers around BridgeManager for use in Tauri commands
//! and other sync contexts.
//!
//! Each helper that needs async work spawns a scoped OS thread and calls
//! `tauri::async_runtime::block_on` from within it. This keeps the callers
//! free of any tokio context while reusing Tauri's process-wide runtime.
//!
//! The BridgeManager uses std::sync::RwLock for its bridges map, so we can
//! access bridges directly from any thread without needing async operations.

#![allow(dead_code)]

use super::BridgeManager;
use super::PythonBridge;
use crate::commands::AppState;
use std::sync::Arc;

/// Error returned when no bridge is available.
pub const NO_BRIDGE_ERROR: &str = "No Python bridge available. Start the executor first.";
pub const BRIDGE_MANAGER_NOT_INIT: &str = "Bridge manager not initialized";

/// Helper to get the bridge manager from AppState.
///
/// Uses std::thread::scope to acquire the tokio Mutex on the bridge_manager
/// field from a thread without tokio context.
pub fn get_bridge_manager(state: &Arc<AppState>) -> Result<Arc<BridgeManager>, String> {
    std::thread::scope(|s| {
        s.spawn(|| {
            tauri::async_runtime::block_on(async {
                let manager_guard = state.bridge_manager.lock().await;
                match &*manager_guard {
                    Some(manager) => Ok(manager.clone()),
                    None => Err(BRIDGE_MANAGER_NOT_INIT.to_string()),
                }
            })
        })
        .join()
        .expect("Bridge operation thread panicked")
    })
}

/// Execute a closure with the default bridge (synchronous wrapper).
///
/// This is the main helper for code that needs to call PythonBridge methods
/// from a sync context. It:
/// 1. Gets the BridgeManager (via a temporary tokio runtime on a scoped thread)
/// 2. Calls with_default_bridge directly (no tokio context needed - uses std::sync::RwLock)
///
/// The closure `f` runs WITHOUT any tokio runtime context, which is essential
/// because PythonBridge methods use self.runtime.block_on() internally.
pub fn with_default_bridge<F, R>(state: &Arc<AppState>, f: F) -> Result<R, String>
where
    F: FnOnce(&mut PythonBridge) -> R + Send,
    R: Send,
{
    let manager = get_bridge_manager(state)?;
    manager.with_default_bridge(f)
}

/// Execute an async closure with the default bridge (synchronous wrapper).
///
/// Use this when the closure itself needs to do async operations.
pub fn with_default_bridge_async<F, Fut, R>(state: &Arc<AppState>, f: F) -> Result<R, String>
where
    F: FnOnce(&mut PythonBridge) -> Fut + Send,
    Fut: std::future::Future<Output = R> + Send,
    R: Send,
{
    let manager = get_bridge_manager(state)?;

    let bridge_id = manager
        .default_bridge_id_sync()
        .ok_or_else(|| NO_BRIDGE_ERROR.to_string())?;

    let bid = super::gui_lock::BridgeId::from_string(bridge_id);
    let mut bridges = manager.bridges.write().unwrap();

    if let Some(managed) = bridges.get_mut(&bid) {
        // Run the async closure on a scoped thread so the caller remains
        // free of any tokio context.
        let bridge = &mut managed.bridge;
        std::thread::scope(|s| {
            s.spawn(|| {
                // SAFETY: The scoped thread borrows `bridge` which is valid
                // for the duration of the scope.
                Ok(tauri::async_runtime::block_on(f(bridge)))
            })
            .join()
            .expect("Bridge async operation thread panicked")
        })
    } else {
        Err(NO_BRIDGE_ERROR.to_string())
    }
}

/// Check if the default bridge is running (synchronous wrapper).
pub fn is_default_bridge_running(state: &Arc<AppState>) -> bool {
    match get_bridge_manager(state) {
        Ok(manager) => manager.is_default_bridge_running(),
        Err(_) => false,
    }
}

/// Get or create the default bridge (synchronous wrapper).
///
/// Returns the bridge ID of the default bridge, creating one if necessary.
/// Note: This still needs async because create_bridge calls bridge.start().await.
pub fn get_or_create_default_bridge(state: &Arc<AppState>) -> Result<String, String> {
    std::thread::scope(|s| {
        s.spawn(|| {
            tauri::async_runtime::block_on(async {
                let manager_guard = state.bridge_manager.lock().await;
                match &*manager_guard {
                    Some(manager) => manager.get_or_create_default_bridge().await,
                    None => Err(BRIDGE_MANAGER_NOT_INIT.to_string()),
                }
            })
        })
        .join()
        .expect("Bridge operation thread panicked")
    })
}

/// Start the default bridge if not running (synchronous wrapper).
/// Note: Needs async because bridge.start() is async.
pub fn start_default_bridge(state: &Arc<AppState>) -> Result<bool, String> {
    std::thread::scope(|s| {
        s.spawn(|| {
            tauri::async_runtime::block_on(async {
                let manager_guard = state.bridge_manager.lock().await;
                match &*manager_guard {
                    Some(manager) => manager.start_default_bridge().await,
                    None => Err(BRIDGE_MANAGER_NOT_INIT.to_string()),
                }
            })
        })
        .join()
        .expect("Bridge operation thread panicked")
    })
}

/// Stop the default bridge (synchronous wrapper).
/// Note: Needs async because bridge.stop() is async.
pub fn stop_default_bridge(state: &Arc<AppState>) -> Result<(), String> {
    std::thread::scope(|s| {
        s.spawn(|| {
            tauri::async_runtime::block_on(async {
                let manager_guard = state.bridge_manager.lock().await;
                match &*manager_guard {
                    Some(manager) => manager.stop_default_bridge().await,
                    None => Err(BRIDGE_MANAGER_NOT_INIT.to_string()),
                }
            })
        })
        .join()
        .expect("Bridge operation thread panicked")
    })
}

/// Get the default bridge state (synchronous wrapper).
pub fn get_default_bridge_state(state: &Arc<AppState>) -> Option<super::ExecutorState> {
    match get_bridge_manager(state) {
        Ok(manager) => manager.get_default_bridge_state(),
        Err(_) => None,
    }
}

/// Execute a closure with a specific bridge by ID (synchronous wrapper).
pub fn with_bridge<F, R>(state: &Arc<AppState>, bridge_id: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&mut PythonBridge) -> R + Send,
    R: Send,
{
    let manager = get_bridge_manager(state)?;
    manager.with_bridge(bridge_id, f)
}

/// Check if the default bridge exists and is running, returning a user-friendly error if not.
pub fn require_running_bridge(state: &Arc<AppState>) -> Result<(), String> {
    if !is_default_bridge_running(state) {
        return Err("Python executor not running".to_string());
    }
    Ok(())
}

/// Migrate helper: wraps the old pattern of getting a mutable bridge reference.
///
/// Old pattern:
/// ```ignore
/// let mut bridge_lock = crate::safe_lock::safe_lock_or_recover(&state.python_bridge, "python_bridge");
/// if let Some(ref mut bridge) = *bridge_lock {
///     // use bridge
/// }
/// ```
///
/// New pattern:
/// ```ignore
/// with_default_bridge(&state, |bridge| {
///     // use bridge
/// })?;
/// ```
#[doc(hidden)]
pub fn _migration_example() {}
