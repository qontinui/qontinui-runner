//! GUI automation lock management.
//!
//! Ensures only one GUI automation session can run at a time,
//! preventing conflicts when multiple sessions try to control the GUI.

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

// ============================================================================
// GUI Lock
// ============================================================================

/// Manages exclusive access to GUI automation.
///
/// Only one session can perform GUI automation at a time to prevent
/// conflicts (e.g., mouse movements interfering with each other).
pub struct GuiLock {
    /// Session ID that currently holds the lock
    holder: Arc<RwLock<Option<String>>>,
}

impl GuiLock {
    /// Create a new GUI lock
    pub fn new() -> Self {
        Self {
            holder: Arc::new(RwLock::new(None)),
        }
    }

    /// Acquire GUI automation lock for a session.
    ///
    /// Returns an error if another session already holds the lock.
    /// If the same session already holds the lock, this is a no-op.
    pub async fn acquire(&self, session_id: &str) -> Result<(), String> {
        let mut lock = self.holder.write().await;
        if let Some(holder) = &*lock {
            if holder != session_id {
                return Err(format!(
                    "GUI automation lock held by session '{}'. Only one GUI session can run at a time.",
                    holder
                ));
            }
            return Ok(());
        }
        *lock = Some(session_id.to_string());
        info!("GUI lock acquired by session '{}'", session_id);
        Ok(())
    }

    /// Release GUI automation lock.
    ///
    /// Only releases if the specified session currently holds the lock.
    pub async fn release(&self, session_id: &str) {
        let mut lock = self.holder.write().await;
        if let Some(holder) = &*lock {
            if holder == session_id {
                *lock = None;
                info!("GUI lock released by session '{}'", session_id);
            }
        }
    }

    /// Get current lock holder session ID, if any.
    #[allow(dead_code)]
    pub async fn holder(&self) -> Option<String> {
        self.holder.read().await.clone()
    }

    /// Check if GUI is available for a session.
    ///
    /// Returns true if no session holds the lock, or if the specified
    /// session already holds the lock.
    #[allow(dead_code)]
    pub async fn is_available_for(&self, session_id: &str) -> bool {
        let lock = self.holder.read().await;
        match &*lock {
            None => true,
            Some(holder) => holder == session_id,
        }
    }
}

impl Default for GuiLock {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_gui_lock_basic() {
        let lock = GuiLock::new();

        // Acquire lock
        lock.acquire("session-1").await.unwrap();
        assert_eq!(lock.holder().await, Some("session-1".to_string()));

        // Second acquisition by different session should fail
        let result = lock.acquire("session-2").await;
        assert!(result.is_err());

        // Same session can re-acquire
        lock.acquire("session-1").await.unwrap();

        // Release and re-acquire by different session
        lock.release("session-1").await;
        assert_eq!(lock.holder().await, None);

        lock.acquire("session-2").await.unwrap();
        assert_eq!(lock.holder().await, Some("session-2".to_string()));
    }

    #[tokio::test]
    async fn test_gui_lock_availability() {
        let lock = GuiLock::new();

        // Available for any session when unlocked
        assert!(lock.is_available_for("session-1").await);
        assert!(lock.is_available_for("session-2").await);

        // Acquire by session-1
        lock.acquire("session-1").await.unwrap();

        // Only available for session-1
        assert!(lock.is_available_for("session-1").await);
        assert!(!lock.is_available_for("session-2").await);
    }
}
