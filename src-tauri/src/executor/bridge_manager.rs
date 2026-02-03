//! Bridge Manager Module
//!
//! Manages multiple PythonBridge instances for concurrent workflow execution.
//! Supports both GUI mode (exclusive, with GUI lock) and headless mode (parallel).

use super::gui_lock::{BridgeId, GuiLock, GuiLockInfo};
use super::python_bridge::PythonBridge;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

/// Mode of operation for a bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BridgeMode {
    /// Full GUI mode with screen capture and input control.
    /// Requires exclusive GUI lock.
    Gui,
    /// Headless mode without GUI interaction.
    /// Can run in parallel with other headless bridges.
    Headless,
}

impl Default for BridgeMode {
    fn default() -> Self {
        Self::Gui
    }
}

/// Information about a managed bridge.
#[derive(Debug, Clone, Serialize)]
pub struct BridgeInfo {
    /// Unique bridge identifier.
    pub bridge_id: String,
    /// Associated task run ID, if any.
    pub run_id: Option<String>,
    /// Operating mode.
    pub mode: BridgeMode,
    /// Monitor indices this bridge can use (GUI mode only).
    pub monitor_indices: Vec<i32>,
    /// Current executor state.
    pub state: String,
    /// Whether this bridge holds the GUI lock.
    pub has_gui_lock: bool,
    /// When the bridge was created.
    pub created_at: u64,
}

/// A managed bridge instance with metadata.
pub struct ManagedBridge {
    /// The actual Python bridge.
    pub bridge: PythonBridge,
    /// Bridge operating mode.
    pub mode: BridgeMode,
    /// Associated task run ID.
    pub run_id: Option<String>,
    /// Monitor indices for GUI mode.
    pub monitor_indices: Vec<i32>,
    /// Creation timestamp.
    pub created_at: u64,
    /// Channel for headless events (only used in headless mode).
    pub headless_events: Option<broadcast::Sender<serde_json::Value>>,
}

/// Result of creating a new bridge.
#[derive(Debug, Serialize)]
pub struct CreateBridgeResult {
    /// The new bridge ID.
    pub bridge_id: String,
    /// Whether the bridge was started successfully.
    pub started: bool,
    /// Error message if start failed.
    pub error: Option<String>,
}

/// Bridge manager for handling multiple concurrent Python bridges.
///
/// Key responsibilities:
/// - Create and destroy bridge instances
/// - Map run IDs to bridge IDs
/// - Manage GUI lock for exclusive access
/// - Route events for headless bridges
pub struct BridgeManager {
    /// All managed bridges by ID.
    /// Uses std::sync::RwLock so bridge_helpers can access without a tokio runtime.
    /// This is critical: PythonBridge methods use their own tokio runtime internally
    /// (self.runtime.block_on), so the bridges map MUST NOT require a tokio context
    /// to lock - otherwise we'd get nested runtime panics.
    pub(crate) bridges: RwLock<HashMap<BridgeId, ManagedBridge>>,
    /// Map from run ID to bridge ID for quick lookup.
    run_id_map: RwLock<HashMap<String, BridgeId>>,
    /// GUI lock for exclusive GUI access.
    gui_lock: Arc<GuiLock>,
    /// Tauri app handle for creating new bridges.
    app_handle: tauri::AppHandle,
    /// When true, all bridges must be created in headless mode.
    /// GUI mode requests will be rejected. Used for server deployments.
    headless_only: RwLock<bool>,
}

impl BridgeManager {
    /// Create a new bridge manager.
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self {
            bridges: RwLock::new(HashMap::new()),
            run_id_map: RwLock::new(HashMap::new()),
            gui_lock: Arc::new(GuiLock::new()),
            app_handle,
            headless_only: RwLock::new(false),
        }
    }

    /// Set headless-only mode.
    ///
    /// When enabled, all bridges must be created in headless mode.
    /// GUI mode requests will be rejected with an error.
    /// This is intended for server deployments where GUI is not available.
    pub fn set_headless_only(&self, enabled: bool) {
        let mut headless_only = self.headless_only.write().unwrap();
        *headless_only = enabled;
        info!("Headless-only mode set to: {}", enabled);
    }

    /// Check if headless-only mode is enabled.
    pub fn is_headless_only(&self) -> bool {
        let headless_only = self.headless_only.read().unwrap();
        *headless_only
    }

    /// Create a new bridge with the specified mode.
    ///
    /// For GUI mode, attempts to acquire the GUI lock. If the lock is already
    /// held, creation will fail unless `force_gui_lock` is true.
    ///
    /// For headless mode, creates a dedicated event channel for the bridge.
    ///
    /// If headless-only mode is enabled, GUI mode requests will be rejected.
    pub async fn create_bridge(
        &self,
        mode: BridgeMode,
        run_id: Option<String>,
        monitor_indices: Vec<i32>,
        force_gui_lock: bool,
    ) -> Result<CreateBridgeResult, String> {
        // Check headless-only mode
        let headless_only = self.is_headless_only();
        let effective_mode = if headless_only && mode == BridgeMode::Gui {
            return Err(
                "Cannot create GUI bridge: headless-only mode is enabled. \
                Server deployments only support headless bridges for parallel workflow execution."
                    .to_string(),
            );
        } else {
            mode
        };

        let bridge_id = BridgeId::new();
        info!(
            "Creating new bridge: id={}, mode={:?}, run_id={:?}, headless_only={}",
            bridge_id, effective_mode, run_id, headless_only
        );

        // For GUI mode, acquire the lock
        if effective_mode == BridgeMode::Gui {
            if !self.gui_lock.try_acquire(&bridge_id).await {
                if force_gui_lock {
                    warn!("Force-releasing GUI lock for new bridge");
                    self.gui_lock.force_release().await;
                    if !self.gui_lock.try_acquire(&bridge_id).await {
                        return Err("Failed to acquire GUI lock even after force release".to_string());
                    }
                } else {
                    return Err("GUI lock is held by another bridge. Use force_gui_lock=true to override or use headless mode.".to_string());
                }
            }
        }

        // Create the Python bridge based on mode
        let mut bridge = if effective_mode == BridgeMode::Headless {
            info!("Creating headless Python bridge");
            PythonBridge::new_headless(self.app_handle.clone())
        } else {
            PythonBridge::new(self.app_handle.clone())
        };

        // Create headless event channel if needed and set it on the bridge
        // This must be done BEFORE start() so the bridge uses the headless reader
        let headless_events = if effective_mode == BridgeMode::Headless {
            let (tx, _) = broadcast::channel(256);
            bridge.set_headless_events(tx.clone());
            Some(tx)
        } else {
            None
        };

        // Try to start the bridge (async)
        let (started, error) = match bridge.start().await {
            Ok(()) => {
                info!("Bridge {} started successfully", bridge_id);
                (true, None)
            }
            Err(e) => {
                error!("Failed to start bridge {}: {}", bridge_id, e);
                // Release GUI lock if we acquired it
                if effective_mode == BridgeMode::Gui {
                    self.gui_lock.release(&bridge_id).await;
                }
                (false, Some(e))
            }
        };

        // Create managed bridge
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let managed = ManagedBridge {
            bridge,
            mode: effective_mode,
            run_id: run_id.clone(),
            monitor_indices,
            created_at: now,
            headless_events,
        };

        // Store the bridge
        {
            let mut bridges = self.bridges.write().unwrap();
            bridges.insert(bridge_id.clone(), managed);
        }

        // Map run ID to bridge ID
        if let Some(ref rid) = run_id {
            let mut run_map = self.run_id_map.write().unwrap();
            run_map.insert(rid.clone(), bridge_id.clone());
        }

        Ok(CreateBridgeResult {
            bridge_id: bridge_id.0,
            started,
            error,
        })
    }

    /// Remove a bridge by ID.
    ///
    /// Stops the bridge if running and releases any held GUI lock.
    pub async fn remove_bridge(&self, bridge_id: &str) -> Result<(), String> {
        let bid = BridgeId::from_string(bridge_id.to_string());
        info!("Removing bridge: {}", bridge_id);

        // Remove from bridges map
        let managed = {
            let mut bridges = self.bridges.write().unwrap();
            bridges.remove(&bid)
        };

        if let Some(mut managed) = managed {
            // Stop the bridge (async)
            if let Err(e) = managed.bridge.stop().await {
                warn!("Error stopping bridge {}: {}", bridge_id, e);
            }

            // Remove run ID mapping
            if let Some(ref run_id) = managed.run_id {
                let mut run_map = self.run_id_map.write().unwrap();
                run_map.remove(run_id);
            }

            // Release GUI lock if held
            if managed.mode == BridgeMode::Gui {
                self.gui_lock.release(&bid).await;
            }

            info!("Bridge {} removed successfully", bridge_id);
            Ok(())
        } else {
            Err(format!("Bridge not found: {}", bridge_id))
        }
    }

    /// Get a bridge by ID for direct access.
    ///
    /// Returns a reference that allows calling methods on the bridge.
    /// The caller must not hold this reference for long periods.
    pub fn with_bridge<F, R>(&self, bridge_id: &str, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut PythonBridge) -> R,
    {
        let bid = BridgeId::from_string(bridge_id.to_string());
        let mut bridges = self.bridges.write().unwrap();

        if let Some(managed) = bridges.get_mut(&bid) {
            Ok(f(&mut managed.bridge))
        } else {
            Err(format!("Bridge not found: {}", bridge_id))
        }
    }

    /// Get a bridge by run ID.
    pub fn with_bridge_by_run_id<F, R>(&self, run_id: &str, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut PythonBridge) -> R,
    {
        // Look up bridge ID from run ID
        let bridge_id = {
            let run_map = self.run_id_map.read().unwrap();
            run_map.get(run_id).cloned()
        };

        if let Some(bid) = bridge_id {
            let mut bridges = self.bridges.write().unwrap();
            if let Some(managed) = bridges.get_mut(&bid) {
                Ok(f(&mut managed.bridge))
            } else {
                Err(format!("Bridge not found for run ID: {}", run_id))
            }
        } else {
            Err(format!("No bridge mapped to run ID: {}", run_id))
        }
    }

    /// List all active bridges.
    pub async fn list_bridges(&self) -> Vec<BridgeInfo> {
        // Collect bridge data and lifecycle handles under the sync lock, then drop the guard
        // before doing async operations. We collect lifecycle handles separately because
        // get_state_async() must be called outside the sync lock to avoid nested runtime panics.
        let bridge_data: Vec<(
            BridgeId,
            Option<String>,
            BridgeMode,
            Vec<i32>,
            std::sync::Arc<tokio::sync::RwLock<super::lifecycle::ExecutorLifecycle>>,
            u64,
        )> = {
            let bridges = self.bridges.read().unwrap();
            bridges
                .iter()
                .map(|(bid, managed)| {
                    (
                        bid.clone(),
                        managed.run_id.clone(),
                        managed.mode,
                        managed.monitor_indices.clone(),
                        managed.bridge.get_lifecycle(),
                        managed.created_at,
                    )
                })
                .collect()
        };
        // Guard is dropped here

        let mut result = Vec::with_capacity(bridge_data.len());
        for (bid, run_id, mode, monitor_indices, lifecycle, created_at) in bridge_data {
            // Now we can safely call async methods outside the sync lock
            let state = lifecycle.read().await.get_state().await;
            let has_gui_lock = self.gui_lock.is_held_by(&bid).await;
            result.push(BridgeInfo {
                bridge_id: bid.0,
                run_id,
                mode,
                monitor_indices,
                state: state.name().to_string(),
                has_gui_lock,
                created_at,
            });
        }

        result
    }

    /// Get info for a specific bridge.
    pub async fn get_bridge_info(&self, bridge_id: &str) -> Option<BridgeInfo> {
        let bid = BridgeId::from_string(bridge_id.to_string());

        // Collect bridge data and lifecycle handle under the sync lock, then drop the guard
        // before calling async methods to avoid nested runtime panics
        let bridge_data = {
            let bridges = self.bridges.read().unwrap();
            bridges.get(&bid).map(|managed| {
                (
                    managed.run_id.clone(),
                    managed.mode,
                    managed.monitor_indices.clone(),
                    managed.bridge.get_lifecycle(),
                    managed.created_at,
                )
            })
        };
        // Guard is dropped here

        if let Some((run_id, mode, monitor_indices, lifecycle, created_at)) = bridge_data {
            // Now we can safely call async methods outside the sync lock
            let state = lifecycle.read().await.get_state().await;
            let has_gui_lock = self.gui_lock.is_held_by(&bid).await;
            Some(BridgeInfo {
                bridge_id: bid.0,
                run_id,
                mode,
                monitor_indices,
                state: state.name().to_string(),
                has_gui_lock,
                created_at,
            })
        } else {
            None
        }
    }

    /// Get the current GUI lock holder.
    pub async fn get_gui_lock_holder(&self) -> Option<String> {
        self.gui_lock.holder().await.map(|h| h.0)
    }

    /// Get GUI lock info.
    pub async fn get_gui_lock_info(&self) -> GuiLockInfo {
        self.gui_lock.info().await
    }

    /// Transfer the GUI lock from one bridge to another.
    ///
    /// This atomically releases the lock from the source bridge and acquires it
    /// on the target bridge. If the source bridge doesn't hold the lock, the
    /// transfer fails.
    ///
    /// # Arguments
    /// * `from` - Bridge ID that currently holds the lock
    /// * `to` - Bridge ID to transfer the lock to
    ///
    /// # Returns
    /// * `Ok(())` - Transfer successful
    /// * `Err(String)` - Transfer failed (source doesn't hold lock, etc.)
    pub async fn transfer_gui_lock(&self, from: &str, to: &str) -> Result<(), String> {
        let from_bid = BridgeId::from_string(from.to_string());
        let to_bid = BridgeId::from_string(to.to_string());

        // Verify source bridge exists and holds the lock
        if !self.gui_lock.is_held_by(&from_bid).await {
            return Err(format!(
                "Bridge '{}' does not hold the GUI lock",
                from
            ));
        }

        // Verify target bridge exists
        {
            let bridges = self.bridges.read().unwrap();
            if !bridges.contains_key(&to_bid) {
                return Err(format!("Target bridge not found: {}", to));
            }

            // Verify target bridge is in GUI mode
            if let Some(managed) = bridges.get(&to_bid) {
                if managed.mode != BridgeMode::Gui {
                    return Err(format!(
                        "Target bridge '{}' is in headless mode and cannot hold GUI lock",
                        to
                    ));
                }
            }
        }

        // Release from source
        if !self.gui_lock.release(&from_bid).await {
            return Err(format!(
                "Failed to release GUI lock from bridge '{}'",
                from
            ));
        }

        // Acquire on target
        if !self.gui_lock.try_acquire(&to_bid).await {
            // Try to restore the lock to the source on failure
            warn!(
                "Failed to acquire GUI lock on target '{}', attempting to restore to source '{}'",
                to, from
            );
            if !self.gui_lock.try_acquire(&from_bid).await {
                error!(
                    "CRITICAL: Failed to restore GUI lock to source '{}' after failed transfer",
                    from
                );
            }
            return Err(format!(
                "Failed to acquire GUI lock on bridge '{}'",
                to
            ));
        }

        info!("GUI lock transferred from '{}' to '{}'", from, to);
        Ok(())
    }

    /// Associate a run ID with an existing bridge.
    pub fn set_run_id(&self, bridge_id: &str, run_id: String) -> Result<(), String> {
        let bid = BridgeId::from_string(bridge_id.to_string());

        // Update the managed bridge
        {
            let mut bridges = self.bridges.write().unwrap();
            if let Some(managed) = bridges.get_mut(&bid) {
                // Remove old run ID mapping if any
                if let Some(ref old_run_id) = managed.run_id {
                    let mut run_map = self.run_id_map.write().unwrap();
                    run_map.remove(old_run_id);
                }
                managed.run_id = Some(run_id.clone());
            } else {
                return Err(format!("Bridge not found: {}", bridge_id));
            }
        }

        // Add new run ID mapping
        {
            let mut run_map = self.run_id_map.write().unwrap();
            run_map.insert(run_id, bid);
        }

        Ok(())
    }

    /// Get the headless event sender for a bridge.
    ///
    /// Returns None if the bridge doesn't exist or is in GUI mode.
    pub fn get_headless_events(
        &self,
        bridge_id: &str,
    ) -> Option<broadcast::Sender<serde_json::Value>> {
        let bid = BridgeId::from_string(bridge_id.to_string());
        let bridges = self.bridges.read().unwrap();

        bridges
            .get(&bid)
            .and_then(|m| m.headless_events.clone())
    }

    /// Subscribe to headless events for a bridge.
    ///
    /// Returns a receiver that will receive all events from the headless bridge.
    pub fn subscribe_headless_events(
        &self,
        bridge_id: &str,
    ) -> Option<broadcast::Receiver<serde_json::Value>> {
        self.get_headless_events(bridge_id)
            .map(|tx| tx.subscribe())
    }

    /// Get the number of active bridges.
    pub fn bridge_count(&self) -> usize {
        let bridges = self.bridges.read().unwrap();
        bridges.len()
    }

    /// Check if a bridge exists.
    pub fn has_bridge(&self, bridge_id: &str) -> bool {
        let bid = BridgeId::from_string(bridge_id.to_string());
        let bridges = self.bridges.read().unwrap();
        bridges.contains_key(&bid)
    }

    /// Remove all bridges.
    ///
    /// Useful for cleanup on shutdown.
    pub async fn remove_all(&self) {
        info!("Removing all bridges");

        let bridge_ids: Vec<BridgeId> = {
            let bridges = self.bridges.read().unwrap();
            bridges.keys().cloned().collect()
        };

        for bid in bridge_ids {
            if let Err(e) = self.remove_bridge(&bid.0).await {
                warn!("Error removing bridge {}: {}", bid, e);
            }
        }

        // Force release GUI lock just in case
        self.gui_lock.force_release().await;
    }

    // =========================================================================
    // Default Bridge Methods (for backward compatibility with single-bridge code)
    // =========================================================================

    /// Get the default bridge ID.
    ///
    /// The default bridge is:
    /// 1. The bridge holding the GUI lock (preferred)
    /// 2. The first GUI-mode bridge
    /// 3. The first bridge of any mode
    /// 4. None if no bridges exist
    pub async fn default_bridge_id(&self) -> Option<String> {
        // First, check if there's a GUI lock holder
        if let Some(holder) = self.gui_lock.holder().await {
            return Some(holder.0);
        }

        // Otherwise, find the first GUI bridge or any bridge
        let bridges = self.bridges.read().unwrap();

        // Prefer GUI mode bridges
        for (bid, managed) in bridges.iter() {
            if managed.mode == BridgeMode::Gui {
                return Some(bid.0.clone());
            }
        }

        // Fall back to any bridge
        bridges.keys().next().map(|bid| bid.0.clone())
    }

    /// Get or create the default bridge.
    ///
    /// If no bridge exists, creates one. Returns the bridge ID.
    /// This is the main entry point for backward-compatible code that
    /// expects a single bridge.
    ///
    /// In headless-only mode, creates a headless bridge instead of GUI bridge.
    pub async fn get_or_create_default_bridge(&self) -> Result<String, String> {
        // Check if we already have a default bridge
        if let Some(bid) = self.default_bridge_id().await {
            return Ok(bid);
        }

        // No bridge exists, create a new bridge
        // Use headless mode if headless-only is enabled
        let headless_only = self.is_headless_only();
        let mode = if headless_only {
            info!("Creating default headless bridge (headless-only mode enabled)");
            BridgeMode::Headless
        } else {
            info!("Creating default GUI bridge");
            BridgeMode::Gui
        };

        let result = self.create_bridge(mode, None, vec![0], false).await?;

        if result.started {
            Ok(result.bridge_id)
        } else {
            Err(result.error.unwrap_or_else(|| "Failed to start default bridge".to_string()))
        }
    }

    /// Get the default bridge ID (synchronous version).
    ///
    /// Skips the async GUI lock check and uses only the bridges map.
    /// Prefers GUI-mode bridges, falls back to any bridge.
    pub fn default_bridge_id_sync(&self) -> Option<String> {
        let bridges = self.bridges.read().unwrap();

        // Prefer GUI mode bridges
        for (bid, managed) in bridges.iter() {
            if managed.mode == BridgeMode::Gui {
                return Some(bid.0.clone());
            }
        }

        // Fall back to any bridge
        bridges.keys().next().map(|bid| bid.0.clone())
    }

    /// Execute a closure with the default bridge.
    ///
    /// If no bridge exists, returns an error. Use `get_or_create_default_bridge`
    /// first if you need to ensure a bridge exists.
    pub fn with_default_bridge<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut PythonBridge) -> R,
    {
        let bridge_id = self.default_bridge_id_sync()
            .ok_or_else(|| "No default bridge available".to_string())?;

        self.with_bridge(&bridge_id, f)
    }

    /// Check if the default bridge exists and is running (sync version).
    ///
    /// WARNING: This method calls `bridge.is_running()` which uses block_on internally.
    /// MUST NOT be called from within a tokio async context. Use `is_default_bridge_running_async()`
    /// instead when in an async context.
    pub fn is_default_bridge_running(&self) -> bool {
        if let Some(bridge_id) = self.default_bridge_id_sync() {
            let bid = BridgeId::from_string(bridge_id);
            let bridges = self.bridges.read().unwrap();
            if let Some(managed) = bridges.get(&bid) {
                return managed.bridge.is_running();
            }
        }
        false
    }

    /// Check if the default bridge exists and is running (async version).
    ///
    /// Use this version when calling from within an async context (e.g., Tauri async commands,
    /// axum handlers, async functions). This avoids the nested runtime panic that would occur
    /// with `is_default_bridge_running()`.
    pub async fn is_default_bridge_running_async(&self) -> bool {
        // Get the lifecycle Arc without holding the lock across await
        let lifecycle = {
            let bridge_id = match self.default_bridge_id_sync() {
                Some(id) => id,
                None => return false,
            };
            let bid = BridgeId::from_string(bridge_id);
            let bridges = self.bridges.read().unwrap();
            match bridges.get(&bid) {
                Some(managed) => managed.bridge.get_lifecycle(),
                None => return false,
            }
        };

        // Now we can safely await without holding the lock
        let state = lifecycle.read().await.get_state().await;
        state.can_accept_commands()
    }

    /// Start the default bridge if it exists but is not running.
    ///
    /// Returns Ok(true) if started, Ok(false) if already running or no bridge,
    /// Err if start failed.
    pub async fn start_default_bridge(&self) -> Result<bool, String> {
        let bridge_id = match self.default_bridge_id_sync() {
            Some(id) => id,
            None => return Ok(false),
        };

        let bid = BridgeId::from_string(bridge_id.clone());

        // Check if already running - get lifecycle without holding lock across await
        let is_running = {
            let lifecycle = {
                let bridges = self.bridges.read().unwrap();
                match bridges.get(&bid) {
                    Some(managed) => managed.bridge.get_lifecycle(),
                    None => return Ok(false),
                }
            };
            let state = lifecycle.read().await.get_state().await;
            state.can_accept_commands()
        };

        if is_running {
            return Ok(false);
        }

        // Now start the bridge - acquire write lock, start, release
        // We need to use spawn_blocking to avoid holding the guard across await
        let bridges_ref = &self.bridges;
        let mut bridges = bridges_ref.write().unwrap();
        if let Some(managed) = bridges.get_mut(&bid) {
            managed.bridge.start().await.map_err(|e| format!("Failed to start bridge: {}", e))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Stop the default bridge.
    pub async fn stop_default_bridge(&self) -> Result<(), String> {
        let bridge_id = match self.default_bridge_id_sync() {
            Some(id) => id,
            None => return Err("No default bridge available".to_string()),
        };

        let bid = BridgeId::from_string(bridge_id);
        let mut bridges = self.bridges.write().unwrap();

        if let Some(managed) = bridges.get_mut(&bid) {
            managed.bridge.stop().await
        } else {
            Err("Default bridge not found".to_string())
        }
    }

    /// Get the state of the default bridge (sync version).
    ///
    /// WARNING: This method uses block_on internally and MUST NOT be called from
    /// within a tokio async context. Use `get_default_bridge_state_async()` instead.
    pub fn get_default_bridge_state(&self) -> Option<crate::executor::ExecutorState> {
        let bridge_id = self.default_bridge_id_sync()?;
        let bid = BridgeId::from_string(bridge_id);
        let bridges = self.bridges.read().unwrap();
        bridges.get(&bid).map(|m| m.bridge.get_state())
    }

    /// Get the state of the default bridge (async version).
    ///
    /// Use this when calling from within an async context (e.g., Tauri commands, axum handlers).
    pub async fn get_default_bridge_state_async(&self) -> Option<crate::executor::ExecutorState> {
        let bridge_id = self.default_bridge_id_sync()?;
        let bid = BridgeId::from_string(bridge_id);

        // Get lifecycle handle under sync lock, then drop it before async call
        let lifecycle = {
            let bridges = self.bridges.read().unwrap();
            bridges.get(&bid).map(|m| m.bridge.get_lifecycle())
        };

        if let Some(lifecycle) = lifecycle {
            Some(lifecycle.read().await.get_state().await)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests would require a mock Tauri app handle,
    // which is complex to set up. In practice, integration tests
    // would be more appropriate for the BridgeManager.
}
