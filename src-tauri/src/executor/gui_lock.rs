//! GUI Lock Module
//!
//! Provides exclusive access control for GUI automation operations.
//! Only one bridge can hold the GUI lock at a time, ensuring that
//! GUI interactions don't conflict between concurrent workflows.

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Unique identifier for a bridge instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BridgeId(pub String);

impl BridgeId {
    /// Create a new bridge ID.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Create a bridge ID from an existing string.
    pub fn from_string(id: String) -> Self {
        Self(id)
    }

    /// Get the underlying string ID.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for BridgeId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for BridgeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// GUI lock state - tracks which bridge (if any) holds exclusive GUI access.
#[derive(Debug, Clone, Default)]
struct GuiLockState {
    /// The bridge ID that currently holds the lock, if any.
    holder: Option<BridgeId>,
    /// Timestamp when the lock was acquired.
    acquired_at: Option<u64>,
}

/// GUI lock controller for exclusive GUI access.
///
/// The GUI lock ensures that only one bridge can perform GUI operations
/// (screen capture, mouse/keyboard input) at a time. Bridges running in
/// headless mode don't need the GUI lock.
#[derive(Debug, Clone)]
pub struct GuiLock {
    state: Arc<RwLock<GuiLockState>>,
}

impl GuiLock {
    /// Create a new GUI lock.
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(GuiLockState::default())),
        }
    }

    /// Try to acquire the GUI lock for the specified bridge.
    ///
    /// Returns `true` if the lock was successfully acquired, `false` if
    /// another bridge already holds the lock.
    pub async fn try_acquire(&self, bridge_id: &BridgeId) -> bool {
        let mut state = self.state.write().await;

        // Check if lock is already held by someone else
        if let Some(ref holder) = state.holder {
            if holder != bridge_id {
                warn!(
                    "GUI lock acquisition failed: already held by {}",
                    holder.as_str()
                );
                return false;
            }
            // Already held by this bridge - idempotent success
            debug!("GUI lock already held by {}", bridge_id.as_str());
            return true;
        }

        // Acquire the lock
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        state.holder = Some(bridge_id.clone());
        state.acquired_at = Some(now);

        info!("GUI lock acquired by {}", bridge_id.as_str());
        true
    }

    /// Release the GUI lock held by the specified bridge.
    ///
    /// Returns `true` if the lock was successfully released, `false` if
    /// the bridge didn't hold the lock.
    pub async fn release(&self, bridge_id: &BridgeId) -> bool {
        let mut state = self.state.write().await;

        // Check if this bridge holds the lock
        if let Some(ref holder) = state.holder {
            if holder != bridge_id {
                warn!(
                    "GUI lock release failed: held by {} but {} tried to release",
                    holder.as_str(),
                    bridge_id.as_str()
                );
                return false;
            }
        } else {
            // No one holds the lock
            debug!("GUI lock release: no lock to release");
            return true;
        }

        // Release the lock
        state.holder = None;
        state.acquired_at = None;

        info!("GUI lock released by {}", bridge_id.as_str());
        true
    }

    /// Force-release the GUI lock regardless of who holds it.
    ///
    /// Use with caution - this should only be used for cleanup/recovery.
    pub async fn force_release(&self) {
        let mut state = self.state.write().await;

        if let Some(ref holder) = state.holder {
            warn!("GUI lock force-released from {}", holder.as_str());
        }

        state.holder = None;
        state.acquired_at = None;
    }

    /// Get the current lock holder, if any.
    pub async fn holder(&self) -> Option<BridgeId> {
        let state = self.state.read().await;
        state.holder.clone()
    }

    /// Check if a specific bridge holds the lock.
    pub async fn is_held_by(&self, bridge_id: &BridgeId) -> bool {
        let state = self.state.read().await;
        state.holder.as_ref() == Some(bridge_id)
    }

    /// Check if the lock is currently held by anyone.
    pub async fn is_locked(&self) -> bool {
        let state = self.state.read().await;
        state.holder.is_some()
    }

    /// Get information about the current lock state.
    pub async fn info(&self) -> GuiLockInfo {
        let state = self.state.read().await;
        GuiLockInfo {
            holder_id: state.holder.as_ref().map(|h| h.0.clone()),
            acquired_at: state.acquired_at,
        }
    }
}

impl Default for GuiLock {
    fn default() -> Self {
        Self::new()
    }
}

/// Information about the current GUI lock state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GuiLockInfo {
    /// The ID of the bridge holding the lock, if any.
    pub holder_id: Option<String>,
    /// Timestamp when the lock was acquired.
    pub acquired_at: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_acquire_and_release() {
        let lock = GuiLock::new();
        let bridge_id = BridgeId::new();

        // Should be able to acquire
        assert!(lock.try_acquire(&bridge_id).await);
        assert!(lock.is_held_by(&bridge_id).await);

        // Should be able to release
        assert!(lock.release(&bridge_id).await);
        assert!(!lock.is_locked().await);
    }

    #[tokio::test]
    async fn test_idempotent_acquire() {
        let lock = GuiLock::new();
        let bridge_id = BridgeId::new();

        // Acquiring twice should succeed (idempotent)
        assert!(lock.try_acquire(&bridge_id).await);
        assert!(lock.try_acquire(&bridge_id).await);
        assert!(lock.is_held_by(&bridge_id).await);
    }

    #[tokio::test]
    async fn test_exclusive_access() {
        let lock = GuiLock::new();
        let bridge1 = BridgeId::new();
        let bridge2 = BridgeId::new();

        // First bridge acquires
        assert!(lock.try_acquire(&bridge1).await);

        // Second bridge should fail to acquire
        assert!(!lock.try_acquire(&bridge2).await);

        // Release and second bridge should succeed
        assert!(lock.release(&bridge1).await);
        assert!(lock.try_acquire(&bridge2).await);
    }

    #[tokio::test]
    async fn test_force_release() {
        let lock = GuiLock::new();
        let bridge1 = BridgeId::new();
        let bridge2 = BridgeId::new();

        // First bridge acquires
        assert!(lock.try_acquire(&bridge1).await);

        // Force release
        lock.force_release().await;

        // Second bridge should now succeed
        assert!(lock.try_acquire(&bridge2).await);
    }
}
