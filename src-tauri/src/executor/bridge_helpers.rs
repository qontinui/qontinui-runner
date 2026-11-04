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
//! That map lock is held only to look a bridge up; the closures below run
//! under the bridge's own lock (`bridge_manager::BridgeHandle`).
//!
//! Everything here BLOCKS. Never call it from the main thread; a Tauri command
//! that needs it is `async` and goes through [`run_bridge_blocking`].

#![allow(dead_code)]

use super::BridgeManager;
use super::PythonBridge;
use crate::commands::compartments::BridgeCompartment;
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

    // Clone the per-bridge handle and release the map lock BEFORE running the
    // closure, so a long call here never blocks map-level reads.
    let handle = manager
        .bridge_handle(&bridge_id)
        .ok_or_else(|| NO_BRIDGE_ERROR.to_string())?;

    handle.with(|bridge| {
        // Run the async closure on a scoped thread so the caller remains
        // free of any tokio context.
        std::thread::scope(|s| {
            s.spawn(|| tauri::async_runtime::block_on(f(bridge)))
                .join()
                .expect("Bridge async operation thread panicked")
        })
    })
}

/// Run a blocking bridge operation off the calling thread.
///
/// Tauri 2 runs a SYNC `#[tauri::command]` on the main (UI) thread, and every
/// bridge helper here can wait: on the per-bridge lock while another caller's
/// `send_command_and_wait` is in flight (up to its timeout), or on the Python
/// round-trip itself. A command that touches the bridge is therefore `async`
/// and runs its body through this, which keeps the main thread pumping (plan
/// `2026-11-04-runner-wedge-detector-reads-a-boot-as-a-wedge-and-never-bound-is-silent`
/// Phase 3: the post-boot ~45 s UI-thread freeze).
pub async fn run_bridge_blocking<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(f)
        .await
        .map_err(|e| format!("Bridge task failed: {}", e))
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

// =============================================================================
// Compartment-scoped variants
// =============================================================================
//
// Each of the `*_compartment` helpers below is a thin delegator to the
// corresponding `&Arc<AppState>` helper above, keyed off `BridgeCompartment.0`.
// Their purpose is to let handlers migrated to `State<'_, BridgeCompartment>`
// call into the bridge-helpers layer without reaching back through the full
// AppState god-object — the Workstream-C goal of restricting each module to
// only the fields in its domain. No behaviour change vs the pre-compartment
// helpers.

/// Compartment-scoped equivalent of [`get_bridge_manager`].
pub fn get_bridge_manager_compartment(
    compartment: &BridgeCompartment,
) -> Result<Arc<BridgeManager>, String> {
    get_bridge_manager(&compartment.0)
}

/// Compartment-scoped equivalent of [`with_default_bridge`].
pub fn with_default_bridge_compartment<F, R>(
    compartment: &BridgeCompartment,
    f: F,
) -> Result<R, String>
where
    F: FnOnce(&mut PythonBridge) -> R + Send,
    R: Send,
{
    with_default_bridge(&compartment.0, f)
}

/// Compartment-scoped equivalent of [`with_default_bridge_async`].
pub fn with_default_bridge_async_compartment<F, Fut, R>(
    compartment: &BridgeCompartment,
    f: F,
) -> Result<R, String>
where
    F: FnOnce(&mut PythonBridge) -> Fut + Send,
    Fut: std::future::Future<Output = R> + Send,
    R: Send,
{
    with_default_bridge_async(&compartment.0, f)
}

/// Compartment-scoped equivalent of [`is_default_bridge_running`].
pub fn is_default_bridge_running_compartment(compartment: &BridgeCompartment) -> bool {
    is_default_bridge_running(&compartment.0)
}

/// Compartment-scoped equivalent of [`get_or_create_default_bridge`].
pub fn get_or_create_default_bridge_compartment(
    compartment: &BridgeCompartment,
) -> Result<String, String> {
    get_or_create_default_bridge(&compartment.0)
}

/// Compartment-scoped equivalent of [`start_default_bridge`].
pub fn start_default_bridge_compartment(compartment: &BridgeCompartment) -> Result<bool, String> {
    start_default_bridge(&compartment.0)
}

/// Compartment-scoped equivalent of [`stop_default_bridge`].
pub fn stop_default_bridge_compartment(compartment: &BridgeCompartment) -> Result<(), String> {
    stop_default_bridge(&compartment.0)
}

/// Compartment-scoped equivalent of [`get_default_bridge_state`].
pub fn get_default_bridge_state_compartment(
    compartment: &BridgeCompartment,
) -> Option<super::ExecutorState> {
    get_default_bridge_state(&compartment.0)
}

/// Compartment-scoped equivalent of [`with_bridge`].
pub fn with_bridge_compartment<F, R>(
    compartment: &BridgeCompartment,
    bridge_id: &str,
    f: F,
) -> Result<R, String>
where
    F: FnOnce(&mut PythonBridge) -> R + Send,
    R: Send,
{
    with_bridge(&compartment.0, bridge_id, f)
}

/// Compartment-scoped equivalent of [`require_running_bridge`].
pub fn require_running_bridge_compartment(compartment: &BridgeCompartment) -> Result<(), String> {
    require_running_bridge(&compartment.0)
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// Calls that wait on a bridge (its per-bridge lock, or a Python round-trip).
    const BRIDGE_CALLS: &[&str] = &[
        "with_default_bridge",
        "with_bridge",
        "is_default_bridge_running",
        "require_running_bridge",
        "get_or_create_default_bridge",
        "start_default_bridge",
        "stop_default_bridge",
        "get_default_bridge_state",
        "get_bridge_manager",
    ];

    fn visit(dir: &Path, out: &mut Vec<(std::path::PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push((path.clone(), std::fs::read_to_string(&path).unwrap()));
            }
        }
    }

    /// Tauri 2 runs a SYNC `#[tauri::command]` on the main thread, so a sync
    /// command that waits on the bridge freezes the window for as long as the
    /// bridge is busy: the ~45 s post-boot freeze of plan
    /// `2026-11-04-runner-wedge-detector-reads-a-boot-as-a-wedge-and-never-bound-is-silent`
    /// Phase 3 was `start_python_executor`. Such a command must be `async` and
    /// run its body through `run_bridge_blocking`. This pins it source-wide.
    /// (It sees direct calls in the command body only, not calls through a
    /// helper the command invokes.)
    #[test]
    fn no_sync_tauri_command_waits_on_the_bridge() {
        let mut files = Vec::new();
        visit(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );

        let mut commands_seen = 0;
        let mut offenders = Vec::new();
        for (path, text) in &files {
            let lines: Vec<&str> = text.lines().collect();
            for i in 0..lines.len() {
                if !lines[i].trim_start().starts_with("#[tauri::command") {
                    continue;
                }
                let mut j = i + 1;
                while j < lines.len() && {
                    let t = lines[j].trim_start();
                    t.starts_with("#[") || t.starts_with("//")
                } {
                    j += 1;
                }
                if j >= lines.len() {
                    continue;
                }
                commands_seen += 1;
                if lines[j].contains("async fn") {
                    continue;
                }
                // The body runs to the first column-0 closing brace.
                let mut body = String::new();
                for line in &lines[j..] {
                    body.push_str(line);
                    body.push('\n');
                    if line.starts_with('}') {
                        break;
                    }
                }
                if BRIDGE_CALLS.iter().any(|c| body.contains(c)) {
                    offenders.push(format!("{}:{}", path.display(), j + 1));
                }
            }
        }

        // Positive control: the scan must actually be finding commands.
        assert!(
            commands_seen > 50,
            "scan found only {} tauri commands; the walker is broken",
            commands_seen
        );
        assert!(
            offenders.is_empty(),
            "sync #[tauri::command]s that wait on the Python bridge (run on the \
             main thread; make them async + run_bridge_blocking): {:#?}",
            offenders
        );
    }
}
